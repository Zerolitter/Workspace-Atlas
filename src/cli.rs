//! CLI layer for the `atlas` binary.
//!
//! Command handlers are shared with the MCP adapter so both transports expose
//! the same application behavior.
use clap::{Parser, Subcommand};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::catalogue;
use crate::config::Config;
use crate::error::{AtlasError, Result};
use crate::generation;
use crate::ids::workspace_id;
use crate::migrations;
use crate::paths::path_as_utf8;
use crate::workspace;

/// Default catalogue location for a workspace ID (per ADR-015).
fn default_catalogue_path(canonical_root: &Path, display_name: &str) -> Result<PathBuf> {
    let id = workspace_id(
        path_as_utf8(canonical_root, "canonical workspace root")?,
        display_name,
    );
    crate::hashing::default_catalogue_dir(&id)
}

#[derive(Parser, Debug)]
#[command(name = "atlas", version, about = "Workspace Atlas CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum GovernorCommand {
    Capabilities {
        workspace_root: PathBuf,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    Run {
        workspace_root: PathBuf,
        #[arg(long)]
        request: String,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long, requires = "max_materialized_bytes")]
        materialize_source: bool,
        #[arg(long, requires = "materialize_source")]
        max_materialized_bytes: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskCommand {
    Start {
        workspace_root: PathBuf,
        #[arg(long)]
        request: String,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    Show {
        workspace_root: PathBuf,
        session_id: String,
        #[arg(long, default_value_t = crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    Complete {
        workspace_root: PathBuf,
        session_id: String,
        #[arg(long)]
        request: String,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    Abandon {
        workspace_root: PathBuf,
        session_id: String,
        #[arg(long)]
        reason_code: CliAbandonReason,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CliAbandonReason {
    UserRequested,
    InactivityTimeout,
}

#[derive(Subcommand, Debug)]
pub enum CompiledContextCommand {
    Show {
        workspace_root: PathBuf,
        #[arg(
            long,
            required_unless_present = "session_id",
            conflicts_with = "session_id"
        )]
        context_id: Option<String>,
        #[arg(
            long,
            required_unless_present = "context_id",
            conflicts_with = "context_id"
        )]
        session_id: Option<String>,
        #[arg(long, default_value_t = crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ContextYieldCommand {
    Show {
        workspace_root: PathBuf,
        session_id: String,
        #[arg(long, default_value_t = crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum RetentionCommand {
    Status {
        workspace_root: PathBuf,
        #[arg(long, default_value_t = crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    Compact {
        workspace_root: PathBuf,
        #[arg(long, required_unless_present = "confirm_manifest", conflicts_with_all = ["confirm_manifest", "confirm_privacy_deletion"])]
        dry_run: bool,
        #[arg(
            long,
            requires = "confirm_privacy_deletion",
            conflicts_with = "dry_run"
        )]
        confirm_manifest: Option<String>,
        #[arg(long, requires = "confirm_manifest", conflicts_with = "dry_run")]
        confirm_privacy_deletion: bool,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, conflicts_with = "confirm_manifest")]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Register a workspace and create the catalogue.
    Init {
        /// Canonical path to the workspace root (must exist; will be the
        /// root for INV-014 confinement).
        workspace_root: PathBuf,
        /// Display name (default = directory name).
        #[arg(long)]
        display_name: Option<String>,
        /// Path to TOML config (default: built-in defaults).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Catalogue path override (default: per-platform app data dir).
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Output human-readable instead of JSON.
        #[arg(long)]
        human: bool,
    },
    /// Report workspace status, active generation, integrity.
    Status {
        /// Canonical path to the workspace root.
        workspace_root: PathBuf,
        /// Catalogue path override.
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Output human-readable instead of JSON.
        #[arg(long)]
        human: bool,
    },
    /// Atomically reconcile file and provider changes into a new active generation.
    Reconcile {
        /// Canonical path to the workspace root.
        workspace_root: PathBuf,
        /// Path to TOML config. Required when reconcile behavior differs
        /// from built-in defaults (for example, external SCIP providers).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Catalogue path override.
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Output human-readable instead of JSON.
        #[arg(long)]
        human: bool,
    },
    /// Report database health.
    Doctor {
        /// Canonical path to the workspace root.
        workspace_root: PathBuf,
        /// Catalogue path override.
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Output human-readable instead of JSON.
        #[arg(long)]
        human: bool,
    },
    /// Search current symbols by display-name substring.
    Find {
        workspace_root: PathBuf,
        query: String,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// Attribute this query to an existing task session.
        #[arg(long)]
        task_session_id: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Inspect a path: identity, symbols, relationships, diagnostics. With
    /// `--symbol <canonical_symbol_key>` instead of a path, resolves the
    /// symbol to its owning file first (QUERY-003) -- same output shape.
    Inspect {
        workspace_root: PathBuf,
        /// Path to inspect. Omit when using --symbol instead.
        path: Option<String>,
        /// Inspect by canonical symbol key instead of path.
        #[arg(long, conflicts_with = "path")]
        symbol: Option<String>,
        /// Attribute this query to an existing task session.
        #[arg(long)]
        task_session_id: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Bounded relationship traversal from a seed path.
    Trace {
        workspace_root: PathBuf,
        path: String,
        #[arg(long, default_value = "outbound")]
        direction: String,
        #[arg(long, default_value_t = 3)]
        max_depth: i64,
        #[arg(long, default_value_t = 200)]
        max_records: i64,
        /// Attribute this traversal to an existing task session.
        #[arg(long)]
        task_session_id: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Bounded change frontier for one or more seed paths.
    Impact {
        workspace_root: PathBuf,
        paths: Vec<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Exact source with hash verification (blocks on stale content).
    Source {
        workspace_root: PathBuf,
        path: String,
        /// Attribute this completed source result to an existing task session.
        #[arg(long)]
        task_session_id: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Build a bounded Context Packet for a task (Map/Change/Audit).
    Context {
        workspace_root: PathBuf,
        task: String,
        #[arg(long, default_value = "map")]
        mode: String,
        #[arg(long)]
        path: Vec<String>,
        #[arg(long)]
        symbol: Vec<String>,
        #[arg(long, default_value_t = 40)]
        max_records: i64,
        #[arg(long, default_value_t = 32768)]
        max_source_bytes: i64,
        #[arg(long, default_value_t = 8000)]
        max_token_estimate: i64,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Lifecycle history: renames, deletions, tombstones -- never combined
    /// with current-state queries.
    History {
        workspace_root: PathBuf,
        /// Restrict to events touching this path (matches old or new path).
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// V1.1: provider capability/status (configured/enabled/available/last execution).
    Providers {
        workspace_root: PathBuf,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// V1.3: compile deterministic, task-kind-specific, generation-bound
    /// Context IR (creating and context-compiling a local task session as a
    /// side effect). No embeddings or learned ranking.
    ContextIr {
        workspace_root: PathBuf,
        task: String,
        /// Declared task kind (explore|bug_fix|behavior_change|api_change|
        /// refactor|configuration_change|test_change|review|audit|unknown).
        /// Omit to classify deterministically from `task`'s keywords.
        #[arg(long)]
        task_kind: Option<String>,
        #[arg(long)]
        path: Vec<String>,
        #[arg(long)]
        symbol: Vec<String>,
        #[arg(long, default_value_t = 40)]
        max_records: i64,
        #[arg(long, default_value_t = 32768)]
        max_source_bytes: i64,
        #[arg(long, default_value_t = 8000)]
        max_estimated_tokens: i64,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// V1.2: build (or rebuild) the Serving Plane for the active generation.
    ServingBuild {
        workspace_root: PathBuf,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// V1.2: diff two committed generations into a Generation Delta.
    GenerationDelta {
        workspace_root: PathBuf,
        from: String,
        to: String,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// V1.4: explain bounded changes and live-verified unchanged contracts.
    Temporal {
        workspace_root: PathBuf,
        /// Retained committed ancestor; defaults to the active generation's parent.
        #[arg(long)]
        from: Option<String>,
        /// Maximum detail records returned in each changes/validity section.
        #[arg(long, default_value_t = 50)]
        max_records: i64,
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long)]
        human: bool,
    },
    /// Discover the frozen Context Governor contract or execute it.
    Governor {
        #[command(subcommand)]
        command: GovernorCommand,
    },
    /// Operate the explicit legacy task lifecycle.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Read persisted legacy compiled context.
    CompiledContext {
        #[command(subcommand)]
        command: CompiledContextCommand,
    },
    /// Read bounded Context Yield evidence.
    ContextYield {
        #[command(subcommand)]
        command: ContextYieldCommand,
    },
    /// Read bounded Serving Plane readiness.
    ServingStatus {
        workspace_root: PathBuf,
        #[arg(long, default_value_t = crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
    /// Inspect or apply privacy retention.
    Retention {
        #[command(subcommand)]
        command: RetentionCommand,
    },
    /// Preview truthful catalogue unregister or invoke its fail-closed apply boundary.
    Unregister {
        workspace_root: PathBuf,
        #[arg(long, required_unless_present = "confirm_manifest", conflicts_with_all = ["confirm_manifest", "irreversible"])]
        dry_run: bool,
        #[arg(long, requires = "irreversible", conflicts_with = "dry_run")]
        confirm_manifest: Option<String>,
        #[arg(long, requires = "confirm_manifest", conflicts_with = "dry_run")]
        irreversible: bool,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, conflicts_with = "confirm_manifest")]
        cursor: Option<String>,
        #[arg(long)]
        catalogue: Option<PathBuf>,
    },
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error(transparent)]
    Application(#[from] crate::context_application::CatalogueContextApplicationError),
}

fn error_detail(error: &CliError) -> String {
    const MAX_ADDITIVE_ERROR_DETAIL_BYTES: usize = 1024;
    let detail = error.to_string();
    if matches!(error, CliError::Atlas(_)) || detail.len() <= MAX_ADDITIVE_ERROR_DETAIL_BYTES {
        return detail;
    }
    let mut end = MAX_ADDITIVE_ERROR_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_string()
}

pub fn run(cli: Cli) {
    match dispatch(cli) {
        Ok(()) => {}
        Err(error) => {
            let payload = serde_json::json!({
                "ok": false,
                "error": error_detail(&error),
                "kind": cli_error_kind(&error),
            });
            eprintln!("{}", serde_json::to_string_pretty(&payload).unwrap());
            std::process::exit(1);
        }
    }
}

pub fn error_kind(error: &AtlasError) -> &'static str {
    match error {
        AtlasError::Sqlite(_) => "sqlite",
        AtlasError::Migration(_) => "migration",
        AtlasError::WorkspaceAlreadyRegistered { .. } => "workspace_already_registered",
        AtlasError::WorkspaceNotFound { .. } => "workspace_not_found",
        AtlasError::PathEscape { .. } => "path_escape",
        AtlasError::WriterLeaseHeld { .. } => "writer_lease_held",
        AtlasError::WriterLeaseExpired { .. } => "writer_lease_expired",
        AtlasError::GenerationStateInvalid { .. } => "generation_state_invalid",
        AtlasError::ActiveGenerationMismatch { .. } => "active_generation_mismatch",
        AtlasError::InvalidConfig(_) => "invalid_config",
        AtlasError::Io(_) => "io",
        AtlasError::Serde(_) => "serde",
        AtlasError::Toml(_) => "toml",
        AtlasError::Other(_) => "other",
    }
}

fn route_error_kind(error: &crate::context_route::ContextRouteError) -> &'static str {
    use crate::context_route::ContextRouteError;
    match error {
        ContextRouteError::UnsupportedVersion { .. } => "unsupported_version",
        ContextRouteError::NoCommonVersion { .. } => "no_common_version",
        ContextRouteError::FloorAboveCeiling => "floor_above_ceiling",
        ContextRouteError::IntentFloorConflict => "intent_floor_conflict",
        ContextRouteError::IntentCeilingConflict => "intent_ceiling_conflict",
        ContextRouteError::DeepBudgetRequired => "deep_budget_required",
        ContextRouteError::InvalidDeepBudget => "invalid_deep_budget",
        ContextRouteError::InvalidDeepBudgetDigest => "invalid_deep_budget_digest",
        ContextRouteError::InvalidIdentity => "invalid_identity",
        ContextRouteError::InvalidLightOperations => "invalid_light_operations",
        ContextRouteError::UnknownProfile => "unknown_profile",
        ContextRouteError::InvalidExecution => "invalid_execution",
    }
}

fn lifecycle_error_kind(error: &crate::task_session::LegacyLifecycleError) -> &'static str {
    match error {
        crate::task_session::LegacyLifecycleError::Atlas(error) => error_kind(error),
        crate::task_session::LegacyLifecycleError::Sqlite(_) => "sqlite",
        crate::task_session::LegacyLifecycleError::DurableContractUnavailable => {
            "durable_contract_unavailable"
        }
        crate::task_session::LegacyLifecycleError::CursorInvalid => "cursor_invalid",
        crate::task_session::LegacyLifecycleError::CursorStale => "cursor_stale",
    }
}

fn boundary_error_kind(
    error: &crate::context_application::ApplicationBoundaryError,
) -> &'static str {
    use crate::context_application::ApplicationBoundaryError;
    match error {
        ApplicationBoundaryError::LiteralConfirmation { .. } => "confirmation_required",
        ApplicationBoundaryError::ManifestMismatch { .. } => "manifest_mismatch",
        ApplicationBoundaryError::UnregisterUnavailable { .. } => "unregister_unavailable",
    }
}

fn application_error_kind(
    error: &crate::context_application::CatalogueContextApplicationError,
) -> &'static str {
    use crate::context_application::{ApplicationCursorError, CatalogueContextApplicationError};
    match error {
        CatalogueContextApplicationError::Route(error) => route_error_kind(error),
        CatalogueContextApplicationError::Atlas(error) => error_kind(error),
        CatalogueContextApplicationError::Cursor(error) => match error {
            ApplicationCursorError::Invalid => "cursor_invalid",
            ApplicationCursorError::Stale => "cursor_stale",
            ApplicationCursorError::Atlas(error) => error_kind(error),
            ApplicationCursorError::Sqlite(_) => "sqlite",
        },
        CatalogueContextApplicationError::Lifecycle(error) => lifecycle_error_kind(error),
        CatalogueContextApplicationError::Sqlite(_) => "sqlite",
        CatalogueContextApplicationError::Serde(_) => "serde",
        CatalogueContextApplicationError::Boundary(error) => boundary_error_kind(error),
    }
}

fn cli_error_kind(error: &CliError) -> &'static str {
    match error {
        CliError::Atlas(error) => error_kind(error),
        CliError::Application(error) => application_error_kind(error),
    }
}

fn dispatch(cli: Cli) -> std::result::Result<(), CliError> {
    match cli.command {
        Command::Governor { command } => Ok(governor_cmd(command)?),
        Command::Task { command } => Ok(task_cmd(command)?),
        Command::CompiledContext { command } => Ok(compiled_context_cmd(command)?),
        Command::ContextYield { command } => Ok(context_yield_cmd(command)?),
        Command::ServingStatus {
            workspace_root,
            limit,
            cursor,
            catalogue,
        } => Ok(serving_status_cmd(
            workspace_root,
            limit,
            cursor,
            catalogue,
        )?),
        Command::Retention { command } => Ok(retention_cmd(command)?),
        Command::Unregister {
            workspace_root,
            dry_run,
            confirm_manifest,
            irreversible,
            limit,
            cursor,
            catalogue,
        } => Ok(unregister_cmd(
            workspace_root,
            dry_run,
            confirm_manifest,
            irreversible,
            limit,
            cursor,
            catalogue,
        )?),
        command => dispatch_atlas(Cli { command }).map_err(Into::into),
    }
}

fn dispatch_atlas(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init {
            workspace_root,
            display_name,
            config,
            catalogue,
            human,
        } => init_cmd(workspace_root, display_name, config, catalogue, human),
        Command::Status {
            workspace_root,
            catalogue,
            human,
        } => status_cmd(workspace_root, catalogue, human),
        Command::Reconcile {
            workspace_root,
            config,
            catalogue,
            human,
        } => reconcile_cmd(workspace_root, config, catalogue, human),
        Command::Doctor {
            workspace_root,
            catalogue,
            human,
        } => doctor_cmd(workspace_root, catalogue, human),
        Command::Find {
            workspace_root,
            query,
            limit,
            task_session_id,
            catalogue,
            human,
        } => find_cmd(
            workspace_root,
            query,
            limit,
            task_session_id,
            catalogue,
            human,
        ),
        Command::Inspect {
            workspace_root,
            path,
            symbol,
            task_session_id,
            catalogue,
            human,
        } => inspect_cmd(
            workspace_root,
            path,
            symbol,
            task_session_id,
            catalogue,
            human,
        ),
        Command::Trace {
            workspace_root,
            path,
            direction,
            max_depth,
            max_records,
            task_session_id,
            catalogue,
            human,
        } => trace_cmd(
            workspace_root,
            path,
            direction,
            max_depth,
            max_records,
            task_session_id,
            catalogue,
            human,
        ),
        Command::Impact {
            workspace_root,
            paths,
            catalogue,
            human,
        } => impact_cmd(workspace_root, paths, catalogue, human),
        Command::Source {
            workspace_root,
            path,
            task_session_id,
            catalogue,
            human,
        } => source_cmd(workspace_root, path, task_session_id, catalogue, human),
        Command::Context {
            workspace_root,
            task,
            mode,
            path,
            symbol,
            max_records,
            max_source_bytes,
            max_token_estimate,
            catalogue,
            human,
        } => context_cmd(
            workspace_root,
            task,
            mode,
            path,
            symbol,
            max_records,
            max_source_bytes,
            max_token_estimate,
            catalogue,
            human,
        ),
        Command::History {
            workspace_root,
            path,
            limit,
            catalogue,
            human,
        } => history_cmd(workspace_root, path, limit, catalogue, human),
        Command::Providers {
            workspace_root,
            catalogue,
            human,
        } => providers_cmd(workspace_root, catalogue, human),
        Command::ContextIr {
            workspace_root,
            task,
            task_kind,
            path,
            symbol,
            max_records,
            max_source_bytes,
            max_estimated_tokens,
            catalogue,
            human,
        } => context_ir_cmd(
            workspace_root,
            task,
            task_kind,
            path,
            symbol,
            max_records,
            max_source_bytes,
            max_estimated_tokens,
            catalogue,
            human,
        ),
        Command::ServingBuild {
            workspace_root,
            catalogue,
            human,
        } => serving_build_cmd(workspace_root, catalogue, human),
        Command::GenerationDelta {
            workspace_root,
            from,
            to,
            catalogue,
            human,
        } => generation_delta_cmd(workspace_root, from, to, catalogue, human),
        Command::Temporal {
            workspace_root,
            from,
            max_records,
            catalogue,
            human,
        } => temporal_cmd(workspace_root, from, max_records, catalogue, human),
        Command::Governor { .. }
        | Command::Task { .. }
        | Command::CompiledContext { .. }
        | Command::ContextYield { .. }
        | Command::ServingStatus { .. }
        | Command::Retention { .. }
        | Command::Unregister { .. } => {
            unreachable!("typed public commands are handled before Atlas-only dispatch")
        }
    }
}

const MAX_REQUEST_DOCUMENT_BYTES: u64 = 1_048_576;

fn read_request_document<T: serde::de::DeserializeOwned>(source: &str) -> Result<T> {
    let mut bytes = Vec::new();
    if source == "-" {
        std::io::stdin()
            .lock()
            .take(MAX_REQUEST_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
    } else {
        let metadata = std::fs::metadata(source)?;
        if metadata.len() > MAX_REQUEST_DOCUMENT_BYTES {
            return Err(AtlasError::InvalidConfig(
                "request document exceeds 1048576 bytes".into(),
            ));
        }
        bytes.reserve_exact(metadata.len() as usize);
        std::fs::File::open(source)?
            .take(MAX_REQUEST_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > MAX_REQUEST_DOCUMENT_BYTES {
        return Err(AtlasError::InvalidConfig(
            "request document exceeds 1048576 bytes".into(),
        ));
    }
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn validate_page_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > crate::context_route::MAX_APPLICATION_PAGE_LIMIT {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        )));
    }
    Ok(())
}

fn governor_cmd(
    command: GovernorCommand,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    match command {
        GovernorCommand::Capabilities {
            workspace_root,
            catalogue,
        } => {
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            required_workspace(resolved.workspace)?;
            print_output(
                false,
                &crate::context_route::discover_context_capabilities(),
            )
            .map_err(Into::into)
        }
        GovernorCommand::Run {
            workspace_root,
            request,
            catalogue,
            materialize_source,
            max_materialized_bytes,
        } => {
            let request: crate::context_application::GovernorRunRequest =
                read_request_document(&request)?;
            if request.semantic.materialize_source
                || request.semantic.max_materialized_bytes.is_some()
            {
                return Err(AtlasError::InvalidConfig(
                    "governor request document cannot grant CLI materialization authority".into(),
                )
                .into());
            }
            let materialization_authorization = match (materialize_source, max_materialized_bytes) {
                (false, None) => None,
                (true, Some(max_bytes)) => Some(
                    crate::context_application::CliSourceMaterializationAuthorization::new(
                        max_bytes,
                    )?,
                ),
                _ => {
                    return Err(AtlasError::InvalidConfig(
                        "CLI materialization requires both authorization flags".into(),
                    )
                    .into());
                }
            };
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let mut connection = resolved.connection;
            let output = crate::context_application::run_catalogue_governor_from_cli(
                request,
                &mut connection,
                &workspace,
                materialization_authorization,
            )?;
            print_output(false, &output).map_err(Into::into)
        }
    }
}

fn task_cmd(
    command: TaskCommand,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    match command {
        TaskCommand::Start {
            workspace_root,
            request,
            catalogue,
        } => {
            let request: crate::task_session::LegacyTaskStartRequest =
                read_request_document(&request)?;
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let mut connection = resolved.connection;
            let output = crate::task_session::start_legacy_task_session(
                &mut connection,
                &workspace,
                &request,
            )?;
            print_output(false, &output).map_err(Into::into)
        }
        TaskCommand::Show {
            workspace_root,
            session_id,
            limit,
            cursor,
            catalogue,
        } => {
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let output = crate::task_session::show_legacy_task_session(
                &resolved.connection,
                &workspace,
                &crate::task_session::LegacyTaskShowRequest {
                    context_ir_version: crate::task_session::LEGACY_LIFECYCLE_CONTRACT_VERSION
                        .to_string(),
                    task_session_id: session_id,
                    limit: Some(limit),
                    cursor,
                },
            )?;
            print_output(false, &output).map_err(Into::into)
        }
        TaskCommand::Complete {
            workspace_root,
            session_id,
            request,
            catalogue,
        } => {
            let request: crate::task_session::LegacyTaskCompleteRequest =
                read_request_document(&request)?;
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let mut connection = resolved.connection;
            let output = crate::task_session::complete_legacy_task_session(
                &mut connection,
                &workspace,
                &session_id,
                &request,
            )?;
            print_output(false, &output).map_err(Into::into)
        }
        TaskCommand::Abandon {
            workspace_root,
            session_id,
            reason_code,
            catalogue,
        } => {
            let reason = match reason_code {
                CliAbandonReason::UserRequested => {
                    crate::task_session::LegacyTaskAbandonReason::UserRequested
                }
                CliAbandonReason::InactivityTimeout => {
                    crate::task_session::LegacyTaskAbandonReason::InactivityTimeout
                }
            };
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let mut connection = resolved.connection;
            let output = crate::task_session::abandon_legacy_task_session(
                &mut connection,
                &workspace,
                &session_id,
                &crate::task_session::LegacyTaskAbandonRequest {
                    context_ir_version: crate::task_session::LEGACY_LIFECYCLE_CONTRACT_VERSION
                        .to_string(),
                    reason,
                },
            )?;
            print_output(false, &output).map_err(Into::into)
        }
    }
}

fn compiled_context_cmd(
    command: CompiledContextCommand,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    match command {
        CompiledContextCommand::Show {
            workspace_root,
            context_id,
            session_id,
            limit,
            cursor,
            catalogue,
        } => {
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let output = crate::context_application::show_compiled_context(
                &resolved.connection,
                &workspace,
                &crate::context_application::CompiledContextShowRequest {
                    context_id,
                    task_session_id: session_id,
                    limit: Some(limit),
                    cursor,
                },
            )?;
            print_output(false, &output).map_err(Into::into)
        }
    }
}

fn context_yield_cmd(
    command: ContextYieldCommand,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    match command {
        ContextYieldCommand::Show {
            workspace_root,
            session_id,
            limit,
            cursor,
            catalogue,
        } => {
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let output = crate::context_application::show_context_yield(
                &resolved.connection,
                &workspace,
                &crate::task_session::LegacyTaskShowRequest {
                    context_ir_version: crate::task_session::LEGACY_LIFECYCLE_CONTRACT_VERSION
                        .to_string(),
                    task_session_id: session_id,
                    limit: Some(limit),
                    cursor,
                },
            )?;
            print_output(false, &output).map_err(Into::into)
        }
    }
}

fn serving_status_cmd(
    workspace_root: PathBuf,
    limit: usize,
    cursor: Option<String>,
    catalogue: Option<PathBuf>,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
    let workspace = required_workspace(resolved.workspace)?;
    let output = crate::serving::serving_status_page(
        &resolved.connection,
        &workspace,
        limit,
        cursor.as_deref(),
    )?;
    print_output(false, &output).map_err(Into::into)
}
fn retention_cmd(
    command: RetentionCommand,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    match command {
        RetentionCommand::Status {
            workspace_root,
            limit,
            cursor,
            catalogue,
        } => {
            validate_page_limit(limit)?;
            let cursor = cursor.as_deref();
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let output = crate::task_session::application_retention_status(
                &resolved.connection,
                &workspace,
                chrono::Utc::now(),
                limit,
                cursor,
            )?;
            print_output(false, &output).map_err(Into::into)
        }
        RetentionCommand::Compact {
            workspace_root,
            dry_run,
            confirm_manifest,
            confirm_privacy_deletion: _,
            limit,
            cursor,
            catalogue,
        } => {
            let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
            let workspace = required_workspace(resolved.workspace)?;
            let as_of = chrono::Utc::now();
            if dry_run {
                let limit = limit.unwrap_or(crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT);
                validate_page_limit(limit)?;
                let cursor = cursor.as_deref();
                let output = crate::task_session::application_privacy_compaction_preview(
                    &resolved.connection,
                    &workspace,
                    as_of,
                    limit,
                    cursor,
                )?;
                print_output(false, &output).map_err(Into::into)
            } else {
                if limit.is_some() || cursor.is_some() {
                    return Err(AtlasError::InvalidConfig(
                        "compact apply does not accept limit or cursor".into(),
                    )
                    .into());
                }
                let mut connection = resolved.connection;
                let output = crate::task_session::application_apply_privacy_compaction(
                    &mut connection,
                    &workspace.workspace_id,
                    as_of,
                    confirm_manifest.as_deref().unwrap_or_default(),
                    crate::task_session::PRIVACY_DELETION_CONFIRMATION,
                    crate::task_session::PrivacyCompactionFault::None,
                )?;
                print_output(false, &output).map_err(Into::into)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn unregister_cmd(
    workspace_root: PathBuf,
    dry_run: bool,
    confirm_manifest: Option<String>,
    irreversible: bool,
    limit: Option<usize>,
    cursor: Option<String>,
    catalogue: Option<PathBuf>,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    let _ = irreversible;
    let resolved = resolve_catalogue(&workspace_root, catalogue.as_deref())?;
    let workspace = required_workspace(resolved.workspace)?;
    let target = crate::catalogue::UnregisterTarget::new(
        resolved.canonical_root,
        resolved.path,
        default_catalogue_dir_parent()?,
        std::env::temp_dir().join(".atlas-provider-tmp"),
    )?;
    if dry_run {
        let limit = limit.unwrap_or(crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT);
        validate_page_limit(limit)?;
        let cursor = cursor.as_deref();
        let output = crate::catalogue::application_unregister_preview(
            &resolved.connection,
            &workspace,
            &target,
            limit,
            cursor,
        )?;
        print_output(false, &output).map_err(Into::into)
    } else {
        if limit.is_some() || cursor.is_some() {
            return Err(AtlasError::InvalidConfig(
                "unregister apply does not accept limit or cursor".into(),
            )
            .into());
        }
        crate::catalogue::application_apply_unregister(
            resolved.connection,
            &workspace,
            target,
            confirm_manifest.as_deref().unwrap_or_default(),
            crate::catalogue::IRREVERSIBLE_CONFIRMATION,
            crate::catalogue::UnregisterFault::None,
        )?;
        unreachable!("successful unregister is unavailable under the current atomicity boundary")
    }
}

#[derive(Serialize)]
struct InitOutput {
    ok: bool,
    workspace_id: String,
    display_name: String,
    canonical_root: String,
    catalogue_path: String,
    schema_version: String,
    migration_version: i64,
    config_hash: String,
    active_generation_id: Option<String>,
    created_at: String,
}

fn init_cmd(
    workspace_root: PathBuf,
    display_name: Option<String>,
    config_path: Option<PathBuf>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let canonical_root = canonical_workspace_root(&workspace_root)?;
    let cfg = load_config(
        config_path.as_deref(),
        display_name.as_deref(),
        &canonical_root,
    )?;
    let cat_path = match catalogue_override.as_ref() {
        Some(path) => path.clone(),
        None => default_catalogue_path(&canonical_root, &cfg.workspace.display_name)?,
    };
    let default_workspace_id = workspace_id(
        path_as_utf8(&canonical_root, "canonical workspace root")?,
        &cfg.workspace.display_name,
    );
    if catalogue_override.is_none() {
        prepare_default_catalogue_registration(&canonical_root, &default_workspace_id, &cat_path)?;
    }

    let initialization = (|| {
        if let Some(parent) = cat_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = catalogue::init_catalogue(&cat_path, &cfg)?;
        let ws = workspace::register_workspace(
            &conn,
            &canonical_root,
            &cfg,
            &cat_path,
            migrations::CURRENT_SCHEMA_VERSION,
        )?;
        workspace::persist_registered_config(&cat_path, &ws, &cfg)?;
        Ok((conn, ws))
    })();
    let (conn, ws) = initialization.map_err(|error: AtlasError| {
        if catalogue_override.is_some() {
            error
        } else {
            AtlasError::Other(format!(
                "default catalogue route for workspace {} was reserved at {}, but initialization did not complete: {error}. Retry `atlas init` with the same workspace root and display name to resume safely",
                canonical_root.display(),
                cat_path.display()
            ))
        }
    })?;

    let mv = migrations::applied_version(&conn)?.unwrap_or(0);

    let out = InitOutput {
        ok: true,
        workspace_id: ws.workspace_id.clone(),
        display_name: ws.display_name.clone(),
        canonical_root: ws.canonical_root.clone(),
        catalogue_path: ws.catalogue_path.clone(),
        schema_version: migrations::CURRENT_SCHEMA_VERSION.to_string(),
        migration_version: mv,
        config_hash: ws.configuration_hash.clone(),
        active_generation_id: ws.active_generation_id.clone(),
        created_at: ws.created_at.clone(),
    };

    print_output(human, &out)
}

fn load_config(
    path: Option<&Path>,
    display_name: Option<&str>,
    workspace_root: &Path,
) -> Result<Config> {
    let mut cfg = match path {
        Some(p) => Config::load(p)?,
        None => minimal_config(),
    };
    if let Some(name) = display_name {
        cfg.workspace.display_name = name.to_string();
    } else if path.is_none() {
        cfg.workspace.display_name = workspace_default_display_name(workspace_root);
    }
    cfg.validate()?;
    Ok(cfg)
}

#[derive(Serialize)]
pub struct StatusOutput {
    pub ok: bool,
    pub workspace_id: String,
    pub canonical_root: String,
    pub config_hash: String,
    pub registered_config_hash: String,
    pub effective_config_hash: Option<String>,
    pub schema_version: String,
    pub migration_version: i64,
    pub active_generation_id: Option<String>,
    pub active_generation_sequence: Option<i64>,
    pub active_generation_state: Option<String>,
    pub committed_generation_count: i64,
    pub failed_generation_count: i64,
    pub abandoned_generation_count: i64,
    pub candidate_generation_count: i64,
    pub integrity_ok: bool,
    pub writer_lease_holder: Option<String>,
    pub writer_lease_expires_at: Option<String>,
}

/// Build the canonical `atlas status` result without printing -- reused by
/// both the CLI and the MCP adapter so the two surfaces can never drift
/// (ADR-017: "no indexing or ranking logic may live in atlas-mcp").
pub fn build_status_output(
    workspace_root: &Path,
    catalogue_override: Option<&Path>,
) -> Result<StatusOutput> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    status_output_for(&resolved.connection, &ws, &resolved.path)
}

/// Build a status result for an already-resolved catalogue connection +
/// workspace record. Shared by `build_status_output` (root-addressed, CLI
/// and MCP tool) and `status_by_workspace_id` (id-addressed, MCP resource).
pub fn status_output_for(
    conn: &rusqlite::Connection,
    ws: &workspace::WorkspaceRecord,
    catalogue_path: &Path,
) -> Result<StatusOutput> {
    let active: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT g.generation_id, g.sequence_no, g.state
             FROM workspace w
             JOIN index_generation g ON g.generation_id = w.active_generation_id
             WHERE w.workspace_id = ?1",
            rusqlite::params![ws.workspace_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    let mv = migrations::applied_version(conn)?.unwrap_or(0);
    let counts = generation_counts(conn, &ws.workspace_id)?;
    let integrity_ok = migrations::integrity_check(conn).is_ok();
    let lease: Option<(String, String)> = conn
        .query_row(
            "SELECT holder_id, expires_at FROM writer_lease WHERE workspace_id = ?1",
            rusqlite::params![ws.workspace_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    Ok(StatusOutput {
        ok: true,
        workspace_id: ws.workspace_id.clone(),
        canonical_root: ws.canonical_root.clone(),
        config_hash: ws.configuration_hash.clone(),
        registered_config_hash: ws.configuration_hash.clone(),
        effective_config_hash: workspace::inspect_registered_config(catalogue_path, ws)
            .ok()
            .flatten()
            .map(|config| config.configuration_hash()),
        schema_version: migrations::CURRENT_SCHEMA_VERSION.to_string(),
        migration_version: mv,
        active_generation_id: active.as_ref().map(|(id, _, _)| id.clone()),
        active_generation_sequence: active.as_ref().map(|(_, s, _)| *s),
        active_generation_state: active.as_ref().map(|(_, _, s)| s.clone()),
        committed_generation_count: counts.committed,
        failed_generation_count: counts.failed,
        abandoned_generation_count: counts.abandoned,
        candidate_generation_count: counts.candidate,
        integrity_ok,
        writer_lease_holder: lease.as_ref().map(|(h, _)| h.clone()),
        writer_lease_expires_at: lease.as_ref().map(|(_, e)| e.clone()),
    })
}

/// Resolve a workspace purely by `workspace_id` (no `workspace_root`
/// needed) via ADR-015's deterministic catalogue path -- used by the MCP
/// `atlas://catalogues/<workspace_id>/status` resource.
pub fn status_by_workspace_id(workspace_id: &str) -> Result<StatusOutput> {
    let cat_path = crate::hashing::default_catalogue_dir(workspace_id)?;
    let conn = catalogue::open_connection(&cat_path, &minimal_config())?;
    let ws = workspace::load_workspace(&conn, workspace_id)?.ok_or_else(|| {
        AtlasError::WorkspaceNotFound {
            workspace_id: workspace_id.to_string(),
        }
    })?;
    status_output_for(&conn, &ws, &cat_path)
}

fn status_cmd(
    workspace_root: PathBuf,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_status_output(&workspace_root, catalogue_override.as_deref())?;
    print_output(human, &out)
}

#[derive(Default)]
struct GenerationCounts {
    candidate: i64,
    committed: i64,
    failed: i64,
    abandoned: i64,
}

fn generation_counts(conn: &rusqlite::Connection, ws_id: &str) -> Result<GenerationCounts> {
    let mut out = GenerationCounts::default();
    let mut stmt = conn.prepare(
        "SELECT state, COUNT(*) FROM index_generation WHERE workspace_id = ?1 GROUP BY state",
    )?;
    let rows = stmt.query_map(rusqlite::params![ws_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (state, count) = row?;
        match state.as_str() {
            "candidate" => out.candidate = count,
            "committed" => out.committed = count,
            "failed" => out.failed = count,
            "abandoned" => out.abandoned = count,
            _ => {}
        }
    }
    Ok(out)
}

#[derive(Serialize)]
struct ReconcileOutput {
    ok: bool,
    workspace_id: String,
    previous_active_generation_id: Option<String>,
    candidate_generation_id: String,
    activation: String, // "committed" or "failed"
    failure_code: Option<String>,
    failure_message: Option<String>,
    source_tree_hash: String,
    eligible_file_count: i64,
    indexed_file_count: i64,
    excluded_file_count: i64,
    unsupported_file_count: i64,
    failed_file_count: i64,
    secret_excluded_file_count: i64,
    parsed_file_count: i64,
    renamed_file_count: i64,
    deleted_file_count: i64,
}

fn reconcile_cmd(
    workspace_root: PathBuf,
    config_path: Option<PathBuf>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let mut resolved = resolve_catalogue(&workspace_root, catalogue_override.as_deref())?;
    let ws = required_workspace(resolved.workspace.clone())?;
    let cfg = match config_path {
        Some(path) => {
            let config = load_config(Some(&path), None, &resolved.canonical_root)?;
            workspace::persist_registered_config(&resolved.path, &ws, &config)?;
            config
        }
        None => workspace::load_registered_config(&resolved.path, &ws)?,
    };
    let report = with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
        crate::discovery::reconcile(&ws, connection, &cfg)
    })?;

    let out = ReconcileOutput {
        ok: report.activation == "committed",
        workspace_id: report.workspace_id,
        previous_active_generation_id: report.previous_active_generation_id,
        candidate_generation_id: report.candidate_generation_id,
        activation: report.activation,
        failure_code: report.failure_code,
        failure_message: report.failure_message,
        source_tree_hash: report.source_tree_hash,
        eligible_file_count: report.eligible_file_count,
        indexed_file_count: report.indexed_file_count,
        excluded_file_count: report.excluded_file_count - report.secret_excluded_file_count,
        unsupported_file_count: report.unsupported_file_count,
        failed_file_count: report.failed_file_count,
        secret_excluded_file_count: report.secret_excluded_file_count,
        parsed_file_count: report.parsed_file_count,
        renamed_file_count: report.renamed_file_count,
        deleted_file_count: report.deleted_file_count,
    };

    print_output(human, &out)
}

#[derive(Serialize)]
pub struct DoctorOutput {
    pub ok: bool,
    pub workspace_id: Option<String>,
    pub catalogue_path: String,
    pub schema_version: String,
    pub migration_version: i64,
    pub integrity_ok: bool,
    pub registered_config_hash: Option<String>,
    pub effective_config_hash: Option<String>,
    pub foreign_keys_on: bool,
    pub writer_lease_present: bool,
    pub workspace_registered: bool,
    pub active_generation_present: bool,
    /// Candidate-state generations found still open at doctor time -- a
    /// process that began `reconcile` and crashed/was killed before
    /// activation leaves exactly this behind (INV-005: the active pointer
    /// itself is never affected, so reads stay correct even mid-crash).
    pub stuck_candidate_ids: Vec<String>,
    /// Stuck candidates this run marked `abandoned` (crash recovery).
    pub recovered_candidate_ids: Vec<String>,
    /// OBS-002: migration names whose stored `migration_integrity`
    /// checksum disagrees with the embedded SQL (MIG-001).
    pub migration_checksum_mismatches: Vec<String>,
    /// OBS-002: `provider_execution` rows still `status = 'running'` with
    /// no `finished_at` -- a provider process that never reached a
    /// terminal outcome (crashed mid-execution before the reconcile
    /// transaction committed a terminal status).
    pub stale_provider_executions: i64,
    /// OBS-002: open (`status IN ('unresolved','ambiguous')`)
    /// `relationship_resolution` rows for the active generation.
    pub unresolved_relationship_count: i64,
    /// OBS-002: open/preferred-with-conflict `evidence_conflict` rows for
    /// the active generation.
    pub open_conflict_count: i64,
    /// G9: `serving_generation` rows stuck in `state = 'building'` with no
    /// `build_finished_at` -- a Serving Plane build that crashed mid-write
    /// (the whole build runs in one transaction, so this can only be a
    /// row from a build that has not yet reached `COMMIT`, never a
    /// genuinely partial projection).
    pub stale_serving_generations: i64,
    pub issues: Vec<String>,
    pub recommendations: Vec<String>,
}

/// Build the canonical `atlas doctor` result without printing. Detects and
/// recovers abandoned candidate generations after an interrupted reconcile:
/// a candidate left in state `'candidate'` by a process that never reached
/// `activate_candidate`/`fail_candidate` is marked `abandoned` so future
/// `reconcile` calls begin a fresh candidate from the correct (still
/// untouched) active generation rather than tripping over a half-written
/// row with a stale sequence number.
pub fn build_doctor_output(
    workspace_root: &Path,
    catalogue_override: Option<&Path>,
) -> Result<DoctorOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let conn = &mut resolved.connection;
    let integrity_ok = migrations::integrity_check(conn).is_ok();
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap_or(0);
    let mv = migrations::applied_version(conn)?.unwrap_or(0);

    let ws = resolved.workspace.clone();
    let ws_registered = ws.is_some();
    let registered_config_hash = ws
        .as_ref()
        .map(|workspace| workspace.configuration_hash.clone());
    let (effective_config_hash, configuration_issue) = match ws.as_ref() {
        Some(workspace) => match workspace::inspect_registered_config(&resolved.path, workspace) {
            Ok(Some(config)) => (Some(config.configuration_hash()), None),
            Ok(None) => (None, Some("configuration_required")),
            Err(_) => (None, Some("configuration_mismatch")),
        },
        None => (None, None),
    };
    let configuration_matches =
        registered_config_hash.is_some() && registered_config_hash == effective_config_hash;
    let (has_lease, has_active, stuck_candidate_ids) = if let Some(ws) = ws.as_ref() {
        let lease: Option<String> = conn
            .query_row(
                "SELECT holder_id FROM writer_lease WHERE workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        let active: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let mut stmt = conn.prepare(
            "SELECT generation_id FROM index_generation WHERE workspace_id = ?1 AND state = 'candidate' ORDER BY sequence_no",
        )?;
        let stuck: Vec<String> = stmt
            .query_map(rusqlite::params![ws.workspace_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        (lease.is_some(), active.is_some(), stuck)
    } else {
        (false, false, Vec::new())
    };

    let recovered_candidate_ids = match ws.as_ref() {
        Some(workspace) if !stuck_candidate_ids.is_empty() => {
            with_cli_writer_lease(conn, &workspace.workspace_id, |connection| {
                for generation_id in &stuck_candidate_ids {
                    generation::abandon_candidate(connection, generation_id)?;
                }
                Ok(stuck_candidate_ids.clone())
            })?
        }
        _ => Vec::new(),
    };

    // OBS-002: migration/provider/resolution health, independent of the
    // structural checks above.
    let migration_checksum_mismatches =
        migrations::verify_migration_checksums(conn).unwrap_or_default();
    let stale_provider_executions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM provider_execution WHERE status = 'running' AND finished_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let (unresolved_relationship_count, open_conflict_count): (i64, i64) = if let Some(ws) =
        ws.as_ref()
    {
        let active_gen: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        if let Some(gen_id) = active_gen {
            let unresolved: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM relationship_resolution WHERE generation_id = ?1 AND status IN ('unresolved', 'ambiguous')",
                    rusqlite::params![gen_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let conflicts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM evidence_conflict WHERE generation_id = ?1 AND status IN ('open', 'preferred_with_conflict')",
                    rusqlite::params![gen_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            (unresolved, conflicts)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let mut issues = Vec::new();
    let mut recommendations = Vec::new();
    if !ws_registered {
        issues.push("workspace_not_registered".into());
        recommendations.push("run `atlas init` to register this workspace".into());
    }
    if let Some(issue) = configuration_issue {
        issues.push(issue.into());
        recommendations.push(if issue == "configuration_required" {
            "rerun `atlas reconcile --config <original-config>` once to register an application-owned configuration copy".into()
        } else {
            "restore the registered configuration copy whose hash matches the catalogue workspace record".into()
        });
    }
    if !has_active {
        issues.push("no_active_generation".into());
        recommendations.push("run `atlas reconcile` to produce generation 1".into());
    }
    if !integrity_ok {
        issues.push("integrity_check_failed".into());
        recommendations.push("restore the catalogue from the most recent backup".into());
    }
    if !recovered_candidate_ids.is_empty() {
        issues.push("abandoned_candidate_recovered".into());
        recommendations.push(format!(
            "{} stuck candidate generation(s) from a prior crash/kill were marked abandoned; the active generation was never affected (INV-005) -- run `atlas reconcile` normally",
            recovered_candidate_ids.len()
        ));
    }
    if !migration_checksum_mismatches.is_empty() {
        issues.push("migration_checksum_mismatch".into());
        recommendations.push(format!(
            "{} migration(s) have a checksum mismatch ({}); the catalogue's applied SQL diverges from the embedded migration -- restore from backup, do not continue writing",
            migration_checksum_mismatches.len(),
            migration_checksum_mismatches.join(", ")
        ));
    }
    if stale_provider_executions > 0 {
        issues.push("stale_provider_executions".into());
        recommendations.push(format!(
            "{stale_provider_executions} provider execution(s) are stuck in status 'running' with no finished_at -- likely a crashed provider process; the next `atlas reconcile` will invalidate and rerun them"
        ));
    }
    let stale_serving_generations: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM serving_generation WHERE state = 'building' AND build_finished_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if stale_serving_generations > 0 {
        issues.push("stale_serving_generations".into());
        recommendations.push(format!(
            "{stale_serving_generations} Serving Plane build(s) are stuck in state 'building' -- likely an interrupted `atlas serving-build`; delete and rebuild"
        ));
    }

    let ok = ws_registered
        && has_active
        && integrity_ok
        && configuration_matches
        && migration_checksum_mismatches.is_empty();

    Ok(DoctorOutput {
        ok,
        workspace_id: ws.map(|w| w.workspace_id),
        catalogue_path: resolved.path.to_string_lossy().into_owned(),
        schema_version: migrations::CURRENT_SCHEMA_VERSION.to_string(),
        migration_version: mv,
        integrity_ok,
        registered_config_hash,
        effective_config_hash,
        foreign_keys_on: fk == 1,
        writer_lease_present: has_lease,
        workspace_registered: ws_registered,
        active_generation_present: has_active,
        stuck_candidate_ids,
        recovered_candidate_ids,
        migration_checksum_mismatches,
        stale_provider_executions,
        unresolved_relationship_count,
        open_conflict_count,
        stale_serving_generations,
        issues,
        recommendations,
    })
}

fn doctor_cmd(
    workspace_root: PathBuf,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_doctor_output(&workspace_root, catalogue_override.as_deref())?;
    print_output(human, &out)
}

fn required_workspace(
    workspace: Option<workspace::WorkspaceRecord>,
) -> Result<workspace::WorkspaceRecord> {
    workspace.ok_or_else(|| AtlasError::WorkspaceNotFound {
        workspace_id: "<by root>".into(),
    })
}

pub fn build_find_output(
    workspace_root: &Path,
    query: &str,
    limit: i64,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::FindOutput> {
    build_find_output_with_task_session(workspace_root, query, limit, None, catalogue_override)
}

pub fn build_find_output_with_task_session(
    workspace_root: &Path,
    query: &str,
    limit: i64,
    task_session_id: Option<&str>,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::FindOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    match task_session_id {
        Some(id) => {
            with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
                crate::query::find_attributed(
                    connection,
                    &ws,
                    query,
                    limit,
                    crate::query::QueryAttribution::new(id),
                )
            })
        }
        None => crate::query::find(&resolved.connection, &ws, query, limit),
    }
}

fn find_cmd(
    workspace_root: PathBuf,
    query: String,
    limit: i64,
    task_session_id: Option<String>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_find_output_with_task_session(
        &workspace_root,
        &query,
        limit,
        task_session_id.as_deref(),
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

pub fn build_providers_output(
    workspace_root: &Path,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::ProviderStatusOutput> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    crate::query::provider_status(&resolved.connection, &ws)
}

fn providers_cmd(
    workspace_root: PathBuf,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_providers_output(&workspace_root, catalogue_override.as_deref())?;
    print_output(human, &out)
}

pub fn build_inspect_output(
    workspace_root: &Path,
    path: &str,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::InspectOutput> {
    build_inspect_output_with_task_session(workspace_root, path, None, catalogue_override)
}

pub fn build_inspect_output_with_task_session(
    workspace_root: &Path,
    path: &str,
    task_session_id: Option<&str>,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::InspectOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    match task_session_id {
        Some(id) => {
            with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
                crate::query::inspect_attributed(
                    connection,
                    &ws,
                    path,
                    crate::query::QueryAttribution::new(id),
                )
            })
        }
        None => crate::query::inspect(&resolved.connection, &ws, path),
    }
}

fn inspect_cmd(
    workspace_root: PathBuf,
    path: Option<String>,
    symbol: Option<String>,
    task_session_id: Option<String>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = match (path, symbol) {
        (_, Some(symbol)) => build_inspect_symbol_output_with_task_session(
            &workspace_root,
            &symbol,
            task_session_id.as_deref(),
            catalogue_override.as_deref(),
        )?,
        (Some(path), None) => build_inspect_output_with_task_session(
            &workspace_root,
            &path,
            task_session_id.as_deref(),
            catalogue_override.as_deref(),
        )?,
        (None, None) => {
            return Err(AtlasError::InvalidConfig(
                "inspect requires either a path or --symbol <canonical_symbol_key>".to_string(),
            ));
        }
    };
    print_output(human, &out)
}

/// QUERY-003: inspect by canonical symbol key instead of path -- same
/// `InspectOutput` shape, shared by the CLI's `--symbol` flag and the MCP
/// `atlas_inspect` tool's `symbol` argument (ADR-017 parity).
pub fn build_inspect_symbol_output(
    workspace_root: &Path,
    canonical_symbol_key: &str,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::InspectOutput> {
    build_inspect_symbol_output_with_task_session(
        workspace_root,
        canonical_symbol_key,
        None,
        catalogue_override,
    )
}

pub fn build_inspect_symbol_output_with_task_session(
    workspace_root: &Path,
    canonical_symbol_key: &str,
    task_session_id: Option<&str>,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::InspectOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    match task_session_id {
        Some(id) => {
            with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
                crate::query::inspect_symbol_attributed(
                    connection,
                    &ws,
                    canonical_symbol_key,
                    crate::query::QueryAttribution::new(id),
                )
            })
        }
        None => crate::query::inspect_symbol(&resolved.connection, &ws, canonical_symbol_key),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_trace_output(
    workspace_root: &Path,
    path: &str,
    direction: &str,
    max_depth: i64,
    max_records: i64,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::TraceOutput> {
    build_trace_output_with_task_session(
        workspace_root,
        path,
        direction,
        max_depth,
        max_records,
        None,
        catalogue_override,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_trace_output_with_task_session(
    workspace_root: &Path,
    path: &str,
    direction: &str,
    max_depth: i64,
    max_records: i64,
    task_session_id: Option<&str>,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::TraceOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    match task_session_id {
        Some(id) => {
            with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
                crate::query::trace_attributed(
                    connection,
                    &ws,
                    path,
                    direction,
                    max_depth,
                    max_records,
                    crate::query::QueryAttribution::new(id),
                )
            })
        }
        None => crate::query::trace(
            &resolved.connection,
            &ws,
            path,
            direction,
            max_depth,
            max_records,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn trace_cmd(
    workspace_root: PathBuf,
    path: String,
    direction: String,
    max_depth: i64,
    max_records: i64,
    task_session_id: Option<String>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_trace_output_with_task_session(
        &workspace_root,
        &path,
        &direction,
        max_depth,
        max_records,
        task_session_id.as_deref(),
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

pub fn build_impact_output(
    workspace_root: &Path,
    paths: &[String],
    catalogue_override: Option<&Path>,
) -> Result<crate::query::ImpactOutput> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    crate::query::impact(&resolved.connection, &ws, paths)
}

fn impact_cmd(
    workspace_root: PathBuf,
    paths: Vec<String>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_impact_output(&workspace_root, &paths, catalogue_override.as_deref())?;
    print_output(human, &out)
}

pub fn build_source_output(
    workspace_root: &Path,
    path: &str,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::SourceOutput> {
    build_source_output_with_task_session(workspace_root, path, None, catalogue_override)
}

/// Canonical source builder shared by the CLI and MCP adapter. `None`
/// deliberately delegates to the unchanged legacy query path, which does not
/// write telemetry.
pub fn build_source_output_with_task_session(
    workspace_root: &Path,
    path: &str,
    task_session_id: Option<&str>,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::SourceOutput> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    match task_session_id {
        Some(task_session_id) => {
            with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
                crate::query::source_for_task_session(connection, &ws, path, task_session_id)
            })
        }
        None => crate::query::source(&resolved.connection, &ws, path),
    }
}

fn source_cmd(
    workspace_root: PathBuf,
    path: String,
    task_session_id: Option<String>,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_source_output_with_task_session(
        &workspace_root,
        &path,
        task_session_id.as_deref(),
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

#[allow(clippy::too_many_arguments)]
pub fn build_context_output(
    workspace_root: &Path,
    task: String,
    mode: &str,
    paths: Vec<String>,
    symbols: Vec<String>,
    max_records: i64,
    max_source_bytes: i64,
    max_token_estimate: i64,
    catalogue_override: Option<&Path>,
) -> Result<crate::broker::ContextPacket> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    let broker_mode = crate::broker::BrokerMode::parse(mode).ok_or_else(|| {
        AtlasError::InvalidConfig(format!("unknown mode {mode:?}; expected map|change|audit"))
    })?;
    let req = crate::broker::TaskRequest {
        task,
        mode: broker_mode,
        known_paths: paths,
        known_symbols: symbols,
        max_records,
        max_source_bytes,
        max_token_estimate,
    };
    crate::broker::build_packet(&resolved.connection, &ws, &req)
}

#[allow(clippy::too_many_arguments)]
fn context_cmd(
    workspace_root: PathBuf,
    task: String,
    mode: String,
    paths: Vec<String>,
    symbols: Vec<String>,
    max_records: i64,
    max_source_bytes: i64,
    max_token_estimate: i64,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let packet = build_context_output(
        &workspace_root,
        task,
        &mode,
        paths,
        symbols,
        max_records,
        max_source_bytes,
        max_token_estimate,
        catalogue_override.as_deref(),
    )?;
    print_output(human, &packet)
}

pub fn build_history_output(
    workspace_root: &Path,
    path: Option<&str>,
    limit: i64,
    catalogue_override: Option<&Path>,
) -> Result<crate::query::HistoryOutput> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    crate::query::history(&resolved.connection, &ws, path, limit)
}

fn history_cmd(
    workspace_root: PathBuf,
    path: Option<String>,
    limit: i64,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_history_output(
        &workspace_root,
        path.as_deref(),
        limit,
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

fn parse_task_kind(s: &str) -> Result<crate::context_ir::TaskKind> {
    use crate::context_ir::TaskKind::*;
    Ok(match s {
        "explore" => Explore,
        "bug_fix" => BugFix,
        "behavior_change" => BehaviorChange,
        "api_change" => ApiChange,
        "refactor" => Refactor,
        "configuration_change" => ConfigurationChange,
        "test_change" => TestChange,
        "review" => Review,
        "audit" => Audit,
        "unknown" => Unknown,
        other => return Err(AtlasError::InvalidConfig(format!(
            "unknown task_kind {other:?}; expected explore|bug_fix|behavior_change|api_change|refactor|configuration_change|test_change|review|audit|unknown"
        ))),
    })
}

#[derive(Serialize)]
pub struct ContextIrCliOutput {
    pub ok: bool,
    pub task_session_id: String,
    pub task_kind: crate::context_ir::TaskKind,
    pub task_kind_source: crate::context_ir::TaskKindSource,
    pub kind_rule_id: Option<String>,
    pub context_ir: crate::context_ir::ContextIr,
}

/// V1.3: compile task-kind-specific Context IR. Creates and
/// context-compiles a local task session as a side effect when the workspace
/// has an active generation (skipped, with a synthetic session id, when it
/// does not -- `compile_context_ir` reports that case as `Blocked`, not a
/// fabricated result). Shared by the CLI `context-ir` command and the MCP
/// `atlas_context_ir` tool (ADR-017 parity).
#[allow(clippy::too_many_arguments)]
pub fn build_context_ir_output(
    workspace_root: &Path,
    task: String,
    declared_task_kind: Option<String>,
    paths: Vec<String>,
    symbols: Vec<String>,
    max_records: i64,
    max_source_bytes: i64,
    max_estimated_tokens: i64,
    catalogue_override: Option<&Path>,
) -> Result<ContextIrCliOutput> {
    for (name, value) in [
        ("max_records", max_records),
        ("max_source_bytes", max_source_bytes),
        ("max_estimated_tokens", max_estimated_tokens),
    ] {
        if value <= 0 {
            return Err(AtlasError::InvalidConfig(format!(
                "{name} must be > 0, got {value}"
            )));
        }
    }
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let mut conn = resolved.connection;
    let ws = required_workspace(resolved.workspace)?;

    let declared = declared_task_kind
        .as_deref()
        .map(parse_task_kind)
        .transpose()?;
    let (task_kind, kind_source, kind_rule_id) =
        crate::task_compiler::classify_task(declared, &task);

    let task_hash = crate::hashing::content_hash_of_bytes(task.as_bytes());
    let normalized_goal_hash = crate::task_compiler::normalized_task_hash(&task);

    let active_generation_id: Option<String> = conn
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            rusqlite::params![ws.workspace_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    with_optional_cli_writer_lease(
        &mut conn,
        &ws.workspace_id,
        active_generation_id.is_some(),
        |connection| {
            let task_session_id = match &active_generation_id {
                Some(gen_id) => {
                    let session = crate::task_session::create_classified_task_session(
                        connection,
                        &ws,
                        gen_id,
                        &task_hash,
                        &normalized_goal_hash,
                        crate::context_ir::RawTaskRetention::None,
                        None,
                        task_kind,
                        kind_source,
                        kind_rule_id.as_deref(),
                        crate::context_ir::CONTEXT_SCHEMA_VERSION,
                    )?;
                    // Idempotent: an identical task, classification, and policy reuse
                    // the same deterministic session id. Transition only a fresh
                    // session, never one already advanced by an equivalent request.
                    if session.state == crate::context_ir::TaskSessionState::Created {
                        crate::task_session::transition_task_session(
                            connection,
                            &session.task_session_id,
                            crate::context_ir::TaskSessionState::ContextCompiled,
                        )?;
                    }
                    session.task_session_id
                }
                None => crate::resolution::deterministic_id(
                    "task",
                    &[
                        &ws.workspace_id,
                        "no_generation",
                        &task_hash,
                        crate::task_compiler::task_kind_identity(task_kind),
                        crate::task_compiler::task_kind_source_identity(kind_source),
                        kind_rule_id.as_deref().unwrap_or(""),
                        crate::task_compiler::PLANNER_POLICY_VERSION,
                    ],
                ),
            };

            let request = crate::task_compiler::CompileRequest {
                known_paths: paths,
                known_symbols: symbols,
                max_records,
                max_source_bytes,
                max_estimated_tokens,
                ..Default::default()
            };
            let request_hash = request.deterministic_hash();
            let context_ir = crate::task_compiler::compile_context_ir(
                connection,
                &ws,
                &task_session_id,
                &task_hash,
                &normalized_goal_hash,
                task_kind,
                kind_source,
                kind_rule_id.clone(),
                &request,
            )?;
            if active_generation_id.is_some() {
                crate::task_session::record_compiled_context_use(
                    connection,
                    &task_session_id,
                    &request_hash,
                    &context_ir,
                )?;
            }

            Ok(ContextIrCliOutput {
                ok: true,
                task_session_id,
                task_kind,
                task_kind_source: kind_source,
                kind_rule_id,
                context_ir,
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn context_ir_cmd(
    workspace_root: PathBuf,
    task: String,
    task_kind: Option<String>,
    paths: Vec<String>,
    symbols: Vec<String>,
    max_records: i64,
    max_source_bytes: i64,
    max_estimated_tokens: i64,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_context_ir_output(
        &workspace_root,
        task,
        task_kind,
        paths,
        symbols,
        max_records,
        max_source_bytes,
        max_estimated_tokens,
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

/// V1.2: build (or rebuild) the Serving Plane for the active generation.
/// Shared by the CLI `serving-build` command and the MCP `atlas_serving_build` tool.
pub fn build_serving_build_output(
    workspace_root: &Path,
    catalogue_override: Option<&Path>,
) -> Result<crate::serving::ServingBuildReport> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
        crate::serving::build_serving_generation(connection, &ws)
    })
}

/// Shared application-layer readiness query reserved for the H6B-approved
/// CLI/MCP surface. T16 exposes no new public command grammar.
pub fn build_serving_status_output(
    workspace_root: &Path,
    catalogue_override: Option<&Path>,
) -> Result<crate::serving::ServingStatusReport> {
    let resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace)?;
    crate::serving::serving_status(&resolved.connection, &ws)
}

fn serving_build_cmd(
    workspace_root: PathBuf,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_serving_build_output(&workspace_root, catalogue_override.as_deref())?;
    print_output(human, &out)
}

/// V1.2: diff two committed generations. Shared by the CLI
/// `generation-delta` command and the MCP `atlas_generation_delta` tool.
pub fn build_generation_delta_output(
    workspace_root: &Path,
    from: &str,
    to: &str,
    catalogue_override: Option<&Path>,
) -> Result<crate::context_ir::GenerationDelta> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
        let delta = crate::generation_delta::compute_generation_delta(connection, &ws, from, to)?;
        crate::generation_delta::persist_generation_delta(connection, &delta)?;
        Ok(delta)
    })
}

fn generation_delta_cmd(
    workspace_root: PathBuf,
    from: String,
    to: String,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out =
        build_generation_delta_output(&workspace_root, &from, &to, catalogue_override.as_deref())?;
    print_output(human, &out)
}

/// V1.4: explain changes to the active generation and live-verify unchanged
/// contracts. Shared by the CLI `temporal` command and MCP `atlas_temporal`.
pub fn build_temporal_output(
    workspace_root: &Path,
    from: Option<&str>,
    max_records: i64,
    catalogue_override: Option<&Path>,
) -> Result<crate::temporal::TemporalReport> {
    let mut resolved = resolve_catalogue(workspace_root, catalogue_override)?;
    let ws = required_workspace(resolved.workspace.clone())?;
    with_cli_writer_lease(&mut resolved.connection, &ws.workspace_id, |connection| {
        crate::temporal::explain_temporal_state(connection, &ws, from, max_records)
    })
}

fn temporal_cmd(
    workspace_root: PathBuf,
    from: Option<String>,
    max_records: i64,
    catalogue_override: Option<PathBuf>,
    human: bool,
) -> Result<()> {
    let out = build_temporal_output(
        &workspace_root,
        from.as_deref(),
        max_records,
        catalogue_override.as_deref(),
    )?;
    print_output(human, &out)
}

const CATALOGUE_LOCATOR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogueLocator {
    schema_version: u32,
    canonical_root: String,
    workspace_id: String,
    catalogue_path: String,
}

impl CatalogueLocator {
    fn new(canonical_root: &Path, workspace_id: &str, catalogue_path: &Path) -> Result<Self> {
        Ok(Self {
            schema_version: CATALOGUE_LOCATOR_SCHEMA_VERSION,
            canonical_root: path_as_utf8(canonical_root, "canonical workspace root")?.to_owned(),
            workspace_id: workspace_id.to_string(),
            catalogue_path: path_as_utf8(catalogue_path, "catalogue path")?.to_owned(),
        })
    }

    fn has_same_route(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.workspace_id == other.workspace_id
            && workspace::paths_have_same_identity(
                Path::new(&self.canonical_root),
                Path::new(&other.canonical_root),
            )
            && workspace::paths_have_same_identity(
                Path::new(&self.catalogue_path),
                Path::new(&other.catalogue_path),
            )
    }
}

struct ResolvedCatalogue {
    canonical_root: PathBuf,
    path: PathBuf,
    connection: rusqlite::Connection,
    workspace: Option<workspace::WorkspaceRecord>,
}

fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    let canonical_root = std::fs::canonicalize(workspace_root).map_err(|error| {
        let detail = if error.kind() == std::io::ErrorKind::NotFound {
            "does not exist".to_string()
        } else {
            error.to_string()
        };
        AtlasError::Other(format!(
            "workspace root {} {detail}",
            workspace_root.display()
        ))
    })?;
    if !canonical_root.is_dir() {
        return Err(AtlasError::Other(format!(
            "workspace root {} is not a directory",
            workspace_root.display()
        )));
    }
    path_as_utf8(&canonical_root, "canonical workspace root")?;
    Ok(canonical_root)
}

fn workspace_default_display_name(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned()
}

fn default_catalogue_dir_parent() -> Result<PathBuf> {
    crate::hashing::default_catalogue_directory()
}

fn catalogue_locator_path(canonical_root: &Path) -> Result<PathBuf> {
    Ok(default_catalogue_dir_parent()?
        .join("locators")
        .join(format!(
            "{}.json",
            workspace::blake3_root_fingerprint(canonical_root)?
        )))
}

fn read_catalogue_locator(canonical_root: &Path) -> Result<Option<CatalogueLocator>> {
    let path = catalogue_locator_path(canonical_root)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let physical_path = validate_locator_file_path(&path)?;
    let bytes = std::fs::read(&physical_path)?;
    let locator: CatalogueLocator = serde_json::from_slice(&bytes).map_err(|error| {
        AtlasError::Other(format!(
            "catalogue locator {} is corrupt: {error}",
            path.display()
        ))
    })?;
    if locator.schema_version != CATALOGUE_LOCATOR_SCHEMA_VERSION {
        return Err(AtlasError::Other(format!(
            "catalogue locator {} has unsupported schema version {}",
            path.display(),
            locator.schema_version
        )));
    }
    let expected_root = Path::new(path_as_utf8(canonical_root, "canonical workspace root")?);
    if !workspace::paths_have_same_identity(Path::new(&locator.canonical_root), expected_root) {
        return Err(AtlasError::Other(format!(
            "catalogue locator {} canonical root mismatch: expected {}, found {}",
            path.display(),
            expected_root.display(),
            locator.canonical_root
        )));
    }
    Ok(Some(locator))
}

fn write_catalogue_locator(locator: &CatalogueLocator) -> Result<()> {
    let canonical_root = Path::new(&locator.canonical_root);
    if let Some(existing) = read_catalogue_locator(canonical_root)? {
        return if existing.has_same_route(locator) {
            Ok(())
        } else {
            Err(AtlasError::Other(format!(
                "catalogue locator for {} already points to workspace {}",
                locator.canonical_root, existing.workspace_id
            )))
        };
    }

    let path = catalogue_locator_path(canonical_root)?;
    let parent = path
        .parent()
        .expect("catalogue locator path always has a parent");
    std::fs::create_dir_all(parent)?;
    validate_locator_directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, locator)?;
    temporary.write_all(b"\n")?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => {
            #[cfg(unix)]
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_catalogue_locator(canonical_root)?.ok_or_else(|| {
                AtlasError::Other(format!(
                    "catalogue locator {} appeared during atomic creation but cannot be read",
                    path.display()
                ))
            })?;
            if existing.has_same_route(locator) {
                Ok(())
            } else {
                Err(AtlasError::Other(format!(
                    "catalogue locator for {} concurrently changed to workspace {}",
                    locator.canonical_root, existing.workspace_id
                )))
            }
        }
        Err(error) => Err(AtlasError::Other(format!(
            "cannot atomically create catalogue locator {}: {}",
            path.display(),
            error.error
        ))),
    }
}

fn validate_workspace_identity(ws: &workspace::WorkspaceRecord) -> Result<()> {
    let expected = workspace_id(&ws.canonical_root, &ws.display_name);
    if ws.workspace_id != expected {
        return Err(AtlasError::Other(format!(
            "catalogue workspace identity mismatch: stored {}, derived {}",
            ws.workspace_id, expected
        )));
    }
    Ok(())
}

/// Legacy fallback discovery snapshots only bounded candidates. The 256 MiB
/// ceiling is more than 3.5 times the largest known catalogue (~75.5 MB);
/// larger catalogues remain available through authoritative `--catalogue`.
const LEGACY_DISCOVERY_MAX_BYTES: u64 = 268_435_456;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LegacyFileState {
    identity: (u64, u64),
    len: u64,
    modified: std::time::SystemTime,
}

struct LegacySidecarState {
    state: Option<LegacyFileState>,
    _file: Option<std::fs::File>,
}

struct LegacyCatalogueInspection {
    connection: rusqlite::Connection,
    _snapshot: Vec<u8>,
    main_file: std::fs::File,
    main_state: LegacyFileState,
    sidecars: [(PathBuf, LegacySidecarState); 3],
}

fn legacy_file_identity(file: &std::fs::File) -> Result<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[repr(C)]
        struct ByHandleFileInformation {
            attributes: u32,
            creation_time: FileTime,
            last_access_time: FileTime,
            last_write_time: FileTime,
            volume_serial_number: u32,
            file_size_high: u32,
            file_size_low: u32,
            number_of_links: u32,
            file_index_high: u32,
            file_index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                file: *mut std::ffi::c_void,
                information: *mut ByHandleFileInformation,
            ) -> i32;
        }

        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: the live file handle and exact Windows output structure remain
        // valid for the duration of this non-owning system call.
        let succeeded = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
        };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: success guarantees that Windows initialized the whole structure.
        let information = unsafe { information.assume_init() };
        Ok((
            u64::from(information.volume_serial_number),
            (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low),
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
}

fn legacy_file_state(file: &std::fs::File) -> Result<LegacyFileState> {
    let metadata = file.metadata()?;
    Ok(LegacyFileState {
        identity: legacy_file_identity(file)?,
        len: metadata.len(),
        modified: metadata.modified()?,
    })
}

fn legacy_path_state(path: &Path) -> Result<LegacyFileState> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(AtlasError::Other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AtlasError::Other(format!(
                "{} is a reparse point",
                path.display()
            )));
        }
    }
    let file = std::fs::File::open(path)?;
    legacy_file_state(&file)
}

fn lock_legacy_catalogue(file: &std::fs::File, path: &Path) -> Result<()> {
    file.try_lock_shared().map_err(|error| {
        AtlasError::Other(format!(
            "legacy catalogue {} is locked or changing; refusing inspection: {error}",
            path.display()
        ))
    })
}
fn probe_legacy_sqlite_lock(path: &Path) -> Result<()> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?;
    Ok(())
}

fn validate_legacy_header(header: &[u8], file_len: u64, path: &Path) -> Result<bool> {
    if header.len() < 16 {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has a short SQLite header ({} bytes; need 100)",
            path.display(),
            header.len()
        )));
    }
    if &header[..16] != b"SQLite format 3\0" {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has invalid SQLite header magic",
            path.display()
        )));
    }
    if header.len() < 100 {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has a short SQLite header ({} bytes; need 100)",
            path.display(),
            header.len()
        )));
    }
    let page_size_field = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if page_size_field == 1 {
        65_536_u64
    } else {
        u64::from(page_size_field)
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has invalid SQLite page size {page_size}",
            path.display()
        )));
    }
    let uses_wal = match (header[18], header[19]) {
        (1, 1) => false,
        (2, 2) => true,
        versions => {
            return Err(AtlasError::Other(format!(
                "legacy catalogue {} has unsupported or mismatched SQLite format versions {}/{}",
                path.display(),
                versions.0,
                versions.1
            )));
        }
    };
    let usable_size = page_size.saturating_sub(u64::from(header[20]));
    if usable_size < 480 || header[21..24] != [64, 32, 32] {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has invalid SQLite page-reserve or payload header fields",
            path.display()
        )));
    }
    let schema_format = u32::from_be_bytes(header[44..48].try_into().unwrap());
    let encoding = u32::from_be_bytes(header[56..60].try_into().unwrap());
    if !(1..=4).contains(&schema_format)
        || !(1..=3).contains(&encoding)
        || header[72..92].iter().any(|byte| *byte != 0)
    {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} has unsupported SQLite schema, encoding, or reserved header fields",
            path.display()
        )));
    }
    if file_len < page_size || !file_len.is_multiple_of(page_size) {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} size {file_len} is not a complete SQLite page image",
            path.display()
        )));
    }
    Ok(uses_wal)
}

fn legacy_sidecar_state(path: &Path, allow_empty: bool) -> Result<LegacySidecarState> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LegacySidecarState {
            state: None,
            _file: None,
        }),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(AtlasError::Other(format!(
                    "legacy catalogue sidecar {} is non-regular; refusing inspection",
                    path.display()
                )));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(AtlasError::Other(format!(
                        "legacy catalogue sidecar {} is a reparse point; refusing inspection",
                        path.display()
                    )));
                }
            }
            if allow_empty && metadata.len() != 0 {
                return Err(AtlasError::Other(format!(
                    "legacy catalogue has a non-empty WAL {}; pass --catalogue explicitly after resolving active writes",
                    path.display()
                )));
            }
            if !allow_empty {
                return Err(AtlasError::Other(format!(
                    "legacy catalogue has active sidecar {}; pass --catalogue explicitly after resolving active writes",
                    path.display()
                )));
            }
            let file = std::fs::File::open(path)?;
            let state = legacy_file_state(&file)?;
            if state.len != 0 || legacy_path_state(path)? != state {
                return Err(AtlasError::Other(format!(
                    "legacy catalogue WAL {} changed during inspection",
                    path.display()
                )));
            }
            Ok(LegacySidecarState {
                state: Some(state),
                _file: Some(file),
            })
        }
    }
}

#[cfg(debug_assertions)]
fn legacy_scan_test_pause(stage: &str) -> Result<()> {
    if std::env::var_os("ATLAS_TEST_LEGACY_SCAN_PAUSE_STAGE").as_deref()
        != Some(std::ffi::OsStr::new(stage))
    {
        return Ok(());
    }
    let ready = std::env::var_os("ATLAS_TEST_LEGACY_SCAN_READY")
        .ok_or_else(|| AtlasError::Other("test scan hook is missing ready path".into()))?;
    let proceed = std::env::var_os("ATLAS_TEST_LEGACY_SCAN_CONTINUE")
        .ok_or_else(|| AtlasError::Other("test scan hook is missing continue path".into()))?;
    std::fs::write(&ready, b"ready")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !Path::new(&proceed).exists() {
        if std::time::Instant::now() >= deadline {
            return Err(AtlasError::Other(format!(
                "test scan hook timed out at {stage}"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(not(debug_assertions))]
fn legacy_scan_test_pause(_stage: &str) -> Result<()> {
    Ok(())
}

fn open_validated_legacy_file(path: &Path) -> Result<(PathBuf, std::fs::File, LegacyFileState)> {
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AtlasError::Other(format!(
                "catalogue target {} is a reparse point",
                path.display()
            )));
        }
    }
    let canonical_parent = std::fs::canonicalize(default_catalogue_dir_parent()?)?;
    let canonical_file = std::fs::canonicalize(path)?;
    validate_resolved_file(
        path,
        &canonical_parent,
        &canonical_file,
        metadata.file_type().is_file(),
        "catalogue target",
    )?;
    let file = std::fs::File::open(&canonical_file)?;
    lock_legacy_catalogue(&file, &canonical_file)?;
    let state = legacy_file_state(&file)?;
    if legacy_path_state(&canonical_file)? != state {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} changed during physical validation",
            path.display()
        )));
    }
    Ok((canonical_file, file, state))
}

fn open_legacy_catalogue_read_only(
    path: &Path,
    mut main_file: std::fs::File,
    main_state: LegacyFileState,
) -> Result<LegacyCatalogueInspection> {
    legacy_scan_test_pause("after_main_open")?;

    let mut snapshot = Vec::new();
    snapshot.try_reserve_exact(100).map_err(|error| {
        AtlasError::Other(format!(
            "legacy catalogue {} header allocation failed: {error}",
            path.display()
        ))
    })?;
    (&mut main_file).take(100).read_to_end(&mut snapshot)?;
    let uses_wal = validate_legacy_header(&snapshot, main_state.len, path)?;
    if !uses_wal {
        probe_legacy_sqlite_lock(path)?;
    }
    if main_state.len > LEGACY_DISCOVERY_MAX_BYTES {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} is {} bytes, exceeding the legacy discovery ceiling of {} bytes",
            path.display(),
            main_state.len,
            LEGACY_DISCOVERY_MAX_BYTES
        )));
    }
    let snapshot_capacity = usize::try_from(main_state.len).map_err(|_| {
        AtlasError::Other(format!(
            "legacy catalogue {} is too large to inspect safely",
            path.display()
        ))
    })?;
    let remaining_len = snapshot_capacity - snapshot.len();
    snapshot.try_reserve_exact(remaining_len).map_err(|error| {
        AtlasError::Other(format!(
            "legacy catalogue {} cannot allocate its bounded {}-byte snapshot: {error}",
            path.display(),
            main_state.len
        ))
    })?;
    let header_len = snapshot.len();
    snapshot.resize(snapshot_capacity, 0);
    main_file
        .read_exact(&mut snapshot[header_len..])
        .map_err(|error| {
            AtlasError::Other(format!(
                "legacy catalogue {} ended before its recorded {}-byte length could be read: {error}",
                path.display(),
                main_state.len
            ))
        })?;
    let mut growth_probe = [0_u8; 1];
    if main_file.read(&mut growth_probe)? != 0 {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} grew beyond its recorded {}-byte length during inspection",
            path.display(),
            main_state.len
        )));
    }
    if legacy_file_state(&main_file)? != main_state || legacy_path_state(path)? != main_state {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} changed during inspection",
            path.display()
        )));
    }

    let sidecars = ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut sidecar_path = path.as_os_str().to_os_string();
        sidecar_path.push(suffix);
        let sidecar_path = PathBuf::from(sidecar_path);
        let state = legacy_sidecar_state(&sidecar_path, suffix == "-wal")?;
        Ok::<_, AtlasError>((sidecar_path, state))
    });
    let [wal, shm, journal] = sidecars;
    let sidecars = [wal?, shm?, journal?];
    legacy_scan_test_pause("after_quiescence")?;

    // sqlite3_deserialize cannot consume a WAL-mode page image. A quiescent
    // main database has no WAL content to apply, so normalize only the private
    // snapshot's format bytes before opening it read-only in memory.
    if uses_wal {
        snapshot[18] = 1;
        snapshot[19] = 1;
    }
    let connection = rusqlite::Connection::open_in_memory()?;
    let snapshot_len = i64::try_from(snapshot.len()).map_err(|_| {
        AtlasError::Other(format!(
            "legacy catalogue {} is too large to deserialize safely",
            path.display()
        ))
    })?;
    // SAFETY: snapshot owns stable initialized bytes until after connection is
    // dropped, the schema name is NUL-terminated, and READONLY forbids resizing.
    let deserialize_result = unsafe {
        rusqlite::ffi::sqlite3_deserialize(
            connection.handle(),
            c"main".as_ptr(),
            snapshot.as_mut_ptr(),
            snapshot_len,
            snapshot_len,
            rusqlite::ffi::SQLITE_DESERIALIZE_READONLY,
        )
    };
    if deserialize_result != rusqlite::ffi::SQLITE_OK {
        return Err(AtlasError::Other(format!(
            "legacy catalogue {} cannot be deserialized safely (SQLite code {deserialize_result})",
            path.display()
        )));
    }
    connection.busy_timeout(Duration::from_millis(250))?;
    Ok(LegacyCatalogueInspection {
        connection,
        _snapshot: snapshot,
        main_file,
        main_state,
        sidecars,
    })
}

impl LegacyCatalogueInspection {
    fn verify_unchanged(&self, path: &Path) -> Result<()> {
        if legacy_file_state(&self.main_file)? != self.main_state
            || legacy_path_state(path)? != self.main_state
        {
            return Err(AtlasError::Other(format!(
                "legacy catalogue {} changed during inspection",
                path.display()
            )));
        }
        for (sidecar_path, expected) in &self.sidecars {
            let allow_empty = sidecar_path.as_os_str().to_string_lossy().ends_with("-wal");
            let actual = legacy_sidecar_state(sidecar_path, allow_empty)?;
            if actual.state != expected.state {
                return Err(AtlasError::Other(format!(
                    "legacy catalogue sidecar state changed during inspection: {}",
                    sidecar_path.display()
                )));
            }
        }
        Ok(())
    }
}

fn validate_resolved_file(
    expected_path: &Path,
    canonical_parent: &Path,
    canonical_file: &Path,
    is_regular_file: bool,
    role: &str,
) -> Result<()> {
    let file_name = expected_path.file_name().ok_or_else(|| {
        AtlasError::Other(format!(
            "{role} {} has no file name",
            expected_path.display()
        ))
    })?;
    let expected_physical_path = canonical_parent.join(file_name);
    if !is_regular_file || canonical_file != expected_physical_path {
        return Err(AtlasError::Other(format!(
            "{role} {} physically escapes the canonical Atlas catalogue directory; resolved to {} instead of {}",
            expected_path.display(),
            canonical_file.display(),
            expected_physical_path.display()
        )));
    }
    Ok(())
}

fn validate_physical_file(path: &Path, allowed_parent: &Path, role: &str) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        AtlasError::Other(format!(
            "{role} {} does not exist or cannot be inspected: {error}",
            path.display()
        ))
    })?;
    let canonical_parent = std::fs::canonicalize(allowed_parent).map_err(|error| {
        AtlasError::Other(format!(
            "canonical Atlas catalogue directory {} cannot be resolved: {error}",
            allowed_parent.display()
        ))
    })?;
    let canonical_file = std::fs::canonicalize(path).map_err(|error| {
        AtlasError::Other(format!(
            "{role} {} cannot be resolved: {error}",
            path.display()
        ))
    })?;
    validate_resolved_file(
        path,
        &canonical_parent,
        &canonical_file,
        metadata.file_type().is_file(),
        role,
    )?;
    Ok(canonical_file)
}

fn validate_catalogue_file_path(path: &Path) -> Result<PathBuf> {
    validate_physical_file(path, &default_catalogue_dir_parent()?, "catalogue target")
}

fn validate_locator_directory(directory: &Path) -> Result<PathBuf> {
    let catalogue_directory = default_catalogue_dir_parent()?;
    let canonical_catalogue_directory =
        std::fs::canonicalize(&catalogue_directory).map_err(|error| {
            AtlasError::Other(format!(
                "canonical Atlas catalogue directory {} cannot be resolved: {error}",
                catalogue_directory.display()
            ))
        })?;
    let canonical_locator_directory = std::fs::canonicalize(directory).map_err(|error| {
        AtlasError::Other(format!(
            "catalogue locator directory {} cannot be resolved: {error}",
            directory.display()
        ))
    })?;
    let expected = canonical_catalogue_directory.join("locators");
    if canonical_locator_directory != expected {
        return Err(AtlasError::Other(format!(
            "catalogue locator directory {} physically escapes the canonical Atlas catalogue directory; resolved to {} instead of {}",
            directory.display(),
            canonical_locator_directory.display(),
            expected.display()
        )));
    }
    Ok(canonical_locator_directory)
}

fn validate_locator_file_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .expect("catalogue locator path always has a parent");
    let canonical_parent = validate_locator_directory(parent)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        AtlasError::Other(format!(
            "catalogue locator {} does not exist or cannot be inspected: {error}",
            path.display()
        ))
    })?;
    let canonical_file = std::fs::canonicalize(path).map_err(|error| {
        AtlasError::Other(format!(
            "catalogue locator {} cannot be resolved: {error}",
            path.display()
        ))
    })?;
    validate_resolved_file(
        path,
        &canonical_parent,
        &canonical_file,
        metadata.file_type().is_file(),
        "catalogue locator",
    )?;
    Ok(canonical_file)
}

fn legacy_catalogue_matches(canonical_root: &Path) -> Result<Vec<CatalogueLocator>> {
    let directory = default_catalogue_dir_parent()?;
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();

    let mut matches = Vec::new();
    for path in paths {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("ws_") || !file_name.ends_with(".sqlite") {
            continue;
        }
        let recovery =
            "Re-run `atlas init <absolute-root>` or pass `--catalogue <path>` explicitly for recovery";
        let (physical_path, main_file, main_state) =
            open_validated_legacy_file(&path).map_err(|error| {
                AtlasError::Other(format!(
                    "cannot inspect legacy catalogue {}: {error}. {recovery}",
                    path.display()
                ))
            })?;
        let inspection = open_legacy_catalogue_read_only(&physical_path, main_file, main_state)
            .map_err(|error| {
                AtlasError::Other(format!(
                    "cannot inspect legacy catalogue {}: {error}. {recovery}",
                    path.display()
                ))
            })?;
        let ws = workspace::load_workspace_by_root(&inspection.connection, canonical_root)
            .map_err(|error| {
                AtlasError::Other(format!(
                    "cannot inspect legacy catalogue {}: {error}. {recovery}",
                    path.display()
                ))
            })?;
        inspection
            .verify_unchanged(&physical_path)
            .map_err(|error| {
                AtlasError::Other(format!(
                    "cannot inspect legacy catalogue {}: {error}. {recovery}",
                    path.display()
                ))
            })?;
        let Some(ws) = ws else {
            continue;
        };
        validate_workspace_identity(&ws)?;
        let expected_path = crate::hashing::default_catalogue_dir(&ws.workspace_id)?;
        if !workspace::paths_have_same_identity(&path, &expected_path) {
            return Err(AtlasError::Other(format!(
                "legacy catalogue {} for workspace {} is outside its ADR-015 path {}",
                path.display(),
                ws.workspace_id,
                expected_path.display()
            )));
        }
        matches.push(CatalogueLocator::new(
            canonical_root,
            &ws.workspace_id,
            &path,
        )?);
    }
    Ok(matches)
}

fn migrate_legacy_catalogue(canonical_root: &Path) -> Result<CatalogueLocator> {
    let matches = legacy_catalogue_matches(canonical_root)?;
    match matches.as_slice() {
        [] => Err(AtlasError::Other(format!(
            "no default catalogue registered for workspace root {}",
            canonical_root.display()
        ))),
        [locator] => {
            write_catalogue_locator(locator)?;
            Ok(locator.clone())
        }
        _ => Err(AtlasError::Other(format!(
            "multiple legacy catalogues match workspace root {}; pass --catalogue explicitly",
            canonical_root.display()
        ))),
    }
}

fn prepare_default_catalogue_registration(
    canonical_root: &Path,
    workspace_id: &str,
    catalogue_path: &Path,
) -> Result<()> {
    let expected = CatalogueLocator::new(canonical_root, workspace_id, catalogue_path)?;
    if let Some(existing) = read_catalogue_locator(canonical_root)? {
        return if existing.has_same_route(&expected) {
            Ok(())
        } else {
            Err(AtlasError::Other(format!(
                "workspace root {} is already routed to workspace {}",
                canonical_root.display(),
                existing.workspace_id
            )))
        };
    }

    let matches = legacy_catalogue_matches(canonical_root)?;
    match matches.as_slice() {
        [] => write_catalogue_locator(&expected),
        [existing] if existing.has_same_route(&expected) => write_catalogue_locator(existing),
        [existing] => Err(AtlasError::Other(format!(
            "workspace root {} is already registered as workspace {}; pass its original display name or use --catalogue explicitly",
            canonical_root.display(),
            existing.workspace_id
        ))),
        _ => Err(AtlasError::Other(format!(
            "multiple legacy catalogues match workspace root {}; pass --catalogue explicitly",
            canonical_root.display()
        ))),
    }
}

fn validate_locator_target(locator: &CatalogueLocator) -> Result<PathBuf> {
    let path = PathBuf::from(&locator.catalogue_path);
    let expected_path = crate::hashing::default_catalogue_dir(&locator.workspace_id)?;
    if !workspace::paths_have_same_identity(&path, &expected_path) {
        return Err(AtlasError::Other(format!(
            "catalogue locator target {} escapes or mismatches the Atlas catalogue directory; expected {}",
            path.display(),
            expected_path.display()
        )));
    }
    validate_catalogue_file_path(&path)
}

fn resolve_catalogue(
    workspace_root: &Path,
    override_path: Option<&Path>,
) -> Result<ResolvedCatalogue> {
    let canonical_root = canonical_workspace_root(workspace_root)?;
    let (path, connection_path) = if let Some(path) = override_path {
        if !path.is_file() {
            return Err(AtlasError::Other(format!(
                "catalogue override {} does not exist or is not a file",
                path.display()
            )));
        }
        (path.to_path_buf(), path.to_path_buf())
    } else {
        let locator = match read_catalogue_locator(&canonical_root)? {
            Some(locator) => locator,
            None => migrate_legacy_catalogue(&canonical_root)?,
        };
        let path = PathBuf::from(&locator.catalogue_path);
        let connection_path = validate_locator_target(&locator)?;
        (path, connection_path)
    };

    let connection = catalogue::open_connection(&connection_path, &minimal_config())?;
    let workspace = workspace::load_workspace_by_root(&connection, &canonical_root)?;
    if override_path.is_none() {
        let locator = read_catalogue_locator(&canonical_root)?.ok_or_else(|| {
            AtlasError::Other(format!(
                "catalogue locator for {} disappeared during resolution",
                canonical_root.display()
            ))
        })?;
        let ws = workspace.as_ref().ok_or_else(|| {
            AtlasError::Other(format!(
                "catalogue locator for {} points to a catalogue without that workspace",
                canonical_root.display()
            ))
        })?;
        validate_workspace_identity(ws)?;
        if ws.workspace_id != locator.workspace_id {
            return Err(AtlasError::Other(format!(
                "catalogue locator workspace mismatch: expected {}, found {}",
                locator.workspace_id, ws.workspace_id
            )));
        }
    }
    Ok(ResolvedCatalogue {
        canonical_root,
        path,
        connection,
        workspace,
    })
}

fn minimal_config() -> Config {
    let toml_text = r#"
        schema_version = "1.0.0"
        [workspace]
        display_name = "_internal"
        "#;
    Config::parse(toml_text).expect("static toml is valid")
}

fn print_output(human: bool, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    if human {
        println!("{}", humanize(&json));
    } else {
        println!("{json}");
    }
    Ok(())
}

fn humanize(json: &str) -> String {
    // Human mode currently emits the canonical JSON unchanged.
    json.to_string()
}

/// Acquire the writer lease for the duration of a CLI subcommand (R-015:
/// "concurrent writers corrupt generation lineage" -- process lock or DB
/// lease). Returns the holder id so the caller can release it when done;
/// an unreleased lease still recovers via its TTL (`writer_lease_recovers_after_stale_heartbeat`),
/// so a crashed one-shot CLI process never wedges the workspace.
pub fn acquire_lease_for_cli(conn: &rusqlite::Connection, workspace_id: &str) -> Result<String> {
    let holder = catalogue::default_holder_id();
    catalogue::acquire_writer_lease(
        conn,
        workspace_id,
        &holder,
        Duration::from_secs(60),
        Duration::from_secs(30),
    )?;
    Ok(holder)
}

fn with_cli_writer_lease<T>(
    connection: &mut rusqlite::Connection,
    workspace_id: &str,
    operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    let holder = acquire_lease_for_cli(connection, workspace_id)?;
    let operation_result = operation(connection);
    let release_result = catalogue::release_writer_lease(connection, workspace_id, &holder);
    match (operation_result, release_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn with_optional_cli_writer_lease<T>(
    connection: &mut rusqlite::Connection,
    workspace_id: &str,
    required: bool,
    operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    if required {
        with_cli_writer_lease(connection, workspace_id, operation)
    } else {
        operation(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::context_ir::{ContextUseEventType, ObservationSource};
    use crate::discovery;
    use crate::generation::TriggerKind;

    #[test]
    fn additive_boundary_errors_map_to_exact_cli_kinds() {
        use crate::context_application::{
            ApplicationBoundaryError, CatalogueContextApplicationError,
        };

        for (boundary, expected) in [
            (
                ApplicationBoundaryError::LiteralConfirmation {
                    operation: "privacy deletion",
                    required: crate::task_session::PRIVACY_DELETION_CONFIRMATION,
                },
                "confirmation_required",
            ),
            (
                ApplicationBoundaryError::ManifestMismatch {
                    operation: "privacy compaction",
                    expected: "expected".into(),
                    found: "found".into(),
                },
                "manifest_mismatch",
            ),
            (
                ApplicationBoundaryError::UnregisterUnavailable {
                    reason: "atomic removal unavailable".into(),
                },
                "unregister_unavailable",
            ),
        ] {
            let error = CliError::Application(CatalogueContextApplicationError::Boundary(boundary));
            assert_eq!(cli_error_kind(&error), expected);
        }
    }

    #[test]
    fn resolved_catalogue_path_validation_rejects_injected_escape_and_symlink() {
        let expected = Path::new("/configured/catalogues/ws_test.sqlite");
        let canonical_parent = Path::new("/physical/catalogues");
        let expected_canonical = canonical_parent.join("ws_test.sqlite");
        validate_resolved_file(
            expected,
            canonical_parent,
            &expected_canonical,
            true,
            "catalogue target",
        )
        .unwrap();

        let escape = validate_resolved_file(
            expected,
            canonical_parent,
            Path::new("/outside/ws_test.sqlite"),
            true,
            "catalogue target",
        )
        .unwrap_err();
        assert!(escape.to_string().contains("physically escapes"));

        let symlink = validate_resolved_file(
            expected,
            canonical_parent,
            &expected_canonical,
            false,
            "catalogue target",
        )
        .unwrap_err();
        assert!(symlink.to_string().contains("physically escapes"));
    }

    /// Crash/restart drill: a process that calls `begin_candidate` and then
    /// dies (crash, kill -9, power loss) before `activate_candidate` or
    /// `fail_candidate` leaves a stray `'candidate'`-state generation row
    /// behind. Proves (1) the active pointer is never touched by the crash
    /// (INV-005 -- reads stay correct throughout), (2) `atlas doctor`
    /// detects and recovers the stuck candidate by marking it `abandoned`,
    /// and (3) a subsequent normal `reconcile` proceeds correctly from the
    /// still-valid prior active generation.
    #[test]
    fn crash_leaves_active_pointer_intact_and_doctor_recovers() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();

        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        workspace::persist_registered_config(&db_path, &ws, &cfg).unwrap();
        let baseline = discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(baseline.activation, "committed");

        let active_before: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active_before.as_deref(),
            Some(baseline.candidate_generation_id.as_str())
        );

        // Simulate a crash: begin a candidate (as `reconcile` does at the
        // top of its cycle) and never activate or fail it -- the process
        // "dies" right here.
        let stuck = generation::begin_candidate(
            &conn,
            &ws.workspace_id,
            TriggerKind::Reconcile,
            "p_reconcile_v1",
            migrations::CURRENT_SCHEMA_VERSION,
        )
        .unwrap();

        // The active pointer must be completely unaffected by the crash.
        let active_during: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active_during, active_before);
        let stuck_state: String = conn
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id = ?1",
                rusqlite::params![stuck.generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stuck_state, "candidate");

        // "Restart": run doctor against the same catalogue -- it must find
        // and recover the stuck candidate.
        let doctor_out = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert_eq!(
            doctor_out.stuck_candidate_ids,
            vec![stuck.generation_id.clone()]
        );
        assert_eq!(
            doctor_out.recovered_candidate_ids,
            vec![stuck.generation_id.clone()]
        );
        assert!(doctor_out
            .issues
            .contains(&"abandoned_candidate_recovered".to_string()));
        // Recovery never flips ok=false on its own -- the workspace is
        // still registered, has an active generation, and integrity holds.
        assert!(doctor_out.ok);

        let recovered_state: String = conn
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id = ?1",
                rusqlite::params![stuck.generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recovered_state, "abandoned");

        // Running doctor again finds nothing left to recover.
        let doctor_again = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert!(doctor_again.stuck_candidate_ids.is_empty());
        assert!(doctor_again.recovered_candidate_ids.is_empty());

        // A normal reconcile after recovery proceeds correctly from the
        // untouched prior active generation, skipping past the abandoned
        // candidate's sequence number (INV-005 evidence: the crash never
        // corrupted state; `reconcile` just picks up where the last
        // *committed* generation left off).
        let after = discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(after.activation, "committed");
        let (active_after, active_after_seq): (String, i64) = conn
            .query_row(
                "SELECT g.generation_id, g.sequence_no
                 FROM workspace w JOIN index_generation g ON g.generation_id = w.active_generation_id
                 WHERE w.workspace_id = ?1",
                rusqlite::params![ws.workspace_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_ne!(
            Some(active_after.clone()),
            active_before,
            "active generation must have advanced past the crash"
        );
        assert_eq!(active_after_seq, 3, "sequence must skip past the abandoned candidate's slot (1=baseline, 2=abandoned, 3=this reconcile)");
    }

    #[test]
    fn doctor_flags_stale_provider_executions_still_running() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        let report = discovery::reconcile(&ws, &conn, &cfg).unwrap();

        // Simulate a provider execution left `running` by a crashed process
        // (never reached a terminal status).
        let descriptor = crate::providers::builtin_provider_descriptors().remove(0);
        let provider_key =
            crate::provider_persistence::register_provider_descriptor(&conn, &descriptor).unwrap();
        conn.execute(
            "INSERT INTO provider_execution (
                provider_execution_id, workspace_id, generation_id, provider_key, project_scope_id,
                scope_kind, scope_key, input_fingerprint, provider_set_hash, normalized_schema_version,
                mapping_policy_version, status, network_isolation_state, started_at, finished_at,
                exit_code, stdout_hash, stderr_hash, stdout_bytes, stderr_bytes, stdout_truncated,
                stderr_truncated, output_hash, output_bytes, diagnostic_summary_json, created_at
             ) VALUES ('pex_stale', ?1, ?2, ?3, NULL, 'file', 'stale-scope', ?4, ?4, '1.1.0', '1.0.0',
                       'running', 'not_available', NULL, NULL, NULL, NULL, NULL, 0, 0, 0, 0, NULL, NULL, '[]', ?5)",
            rusqlite::params![ws.workspace_id, report.candidate_generation_id, provider_key, "a".repeat(64), migrations::iso8601_now()],
        ).unwrap();

        let out = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert_eq!(out.stale_provider_executions, 1);
        assert!(out
            .issues
            .contains(&"stale_provider_executions".to_string()));
    }

    #[test]
    fn doctor_reports_unresolved_and_conflict_health_for_active_generation() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        workspace::persist_registered_config(&db_path, &ws, &cfg).unwrap();
        let report = discovery::reconcile(&ws, &conn, &cfg).unwrap();

        crate::resolution::record_evidence_conflict(
            &conn,
            &ws.workspace_id,
            &report.candidate_generation_id,
            "relationship",
            "subject_1",
            "incompatible_target",
            &["f1".to_string(), "f2".to_string()],
            Some("f1"),
            "preferred-evidence-1.0.0",
            "disagreement",
        )
        .unwrap();

        let out = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert_eq!(out.open_conflict_count, 1);
        assert_eq!(out.unresolved_relationship_count, 0);
        // A resolvable, self-healing conflict is not itself a hard doctor
        // failure -- it is reported, not treated as `ok = false`.
        assert!(out.ok);
    }

    #[test]
    fn doctor_detects_a_tampered_migration_checksum() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        conn.execute(
            "UPDATE migration_integrity SET sha256 = ?1 WHERE version = 1",
            rusqlite::params!["0".repeat(64)],
        )
        .unwrap();

        let out = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert!(!out.migration_checksum_mismatches.is_empty());
        assert!(out
            .issues
            .contains(&"migration_checksum_mismatch".to_string()));
        assert!(
            !out.ok,
            "a tampered migration checksum must flip doctor ok=false"
        );
    }

    #[test]
    fn inspect_cmd_by_symbol_matches_inspect_cmd_by_path() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let by_path = build_inspect_output(ws_dir.path(), "src/a.ts", Some(&db_path)).unwrap();
        let key = by_path.symbols[0].canonical_symbol_key.clone();
        let by_symbol = build_inspect_symbol_output(ws_dir.path(), &key, Some(&db_path)).unwrap();

        assert_eq!(
            serde_json::to_value(&by_path).unwrap(),
            serde_json::to_value(&by_symbol).unwrap(),
            "CLI --symbol and positional path inspect must produce identical output (QUERY-003)"
        );
    }

    #[test]
    fn context_ir_cmd_auto_classifies_and_creates_a_task_session() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let out = build_context_ir_output(
            ws_dir.path(),
            "fix a bug in alpha".to_string(),
            None,
            vec![],
            vec!["alpha".to_string()],
            10,
            32768,
            8000,
            Some(&db_path),
        )
        .unwrap();
        assert!(out.ok);
        assert_eq!(out.task_kind, crate::context_ir::TaskKind::BugFix);
        assert_eq!(
            out.task_kind_source,
            crate::context_ir::TaskKindSource::DeterministicRule
        );
        assert!(out
            .context_ir
            .working_set
            .iter()
            .any(|i| i.entity_id.contains("alpha")));

        let state: String = conn
            .query_row(
                "SELECT state FROM task_session WHERE task_session_id = ?1",
                rusqlite::params![out.task_session_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "context_compiled",
            "compiling a Context IR must leave the task session in context_compiled state"
        );

        let events = crate::task_session::task_session_events(&conn, &out.task_session_id).unwrap();
        let supplied: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == ContextUseEventType::ContextSupplied)
            .collect();
        assert_eq!(supplied.len(), out.context_ir.working_set.len());
        for item in &out.context_ir.working_set {
            let event = supplied
                .iter()
                .find(|event| event.item_id.as_deref() == Some(&item.item_id))
                .unwrap();
            assert_eq!(
                event.context_id.as_deref(),
                Some(out.context_ir.context_id.as_str())
            );
            assert_eq!(event.entity_id.as_deref(), Some(item.entity_id.as_str()));
            assert_eq!(event.observation_source, ObservationSource::AtlasObserved);
            assert_eq!(
                event.bytes,
                Some(item.cost.metadata_bytes + item.cost.source_bytes)
            );
            assert_eq!(
                event.event_id,
                crate::task_session::event_id_for(
                    &out.task_session_id,
                    ContextUseEventType::ContextSupplied,
                    &format!("{}:1:{}", out.context_ir.context_hash, item.item_id),
                ),
            );
            let details = serde_json::to_string(&event.details).unwrap();
            assert!(details.len() <= 4096);
            assert!(!details.contains("fix a bug in alpha"));
            assert!(!details.contains("export function alpha"));
        }
        let counts =
            crate::task_session::event_counts_by_type_and_source(&conn, &out.task_session_id)
                .unwrap();
        assert_eq!(
            counts.get(&("context_supplied".to_string(), "atlas_observed".to_string())),
            Some(&(out.context_ir.working_set.len() as i64)),
        );
    }

    fn context_ir_fixture(
        source: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), source).unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();
        drop(conn);
        (db_dir, ws_dir, db_path)
    }

    #[test]
    fn repeated_context_ir_delivery_records_distinct_deterministic_supplies_without_recompile_claims(
    ) {
        let (_db, ws, db_path) = context_ir_fixture("export function alpha() { return 1; }\n");
        let run = || {
            build_context_ir_output(
                ws.path(),
                "inspect alpha".to_string(),
                None,
                vec![],
                vec!["alpha".to_string()],
                10,
                32768,
                8000,
                Some(&db_path),
            )
            .unwrap()
        };
        let first = run();
        let second = run();
        assert_eq!(
            serde_json::to_vec(&first.context_ir).unwrap(),
            serde_json::to_vec(&second.context_ir).unwrap(),
        );

        let conn = catalogue::open_connection(&db_path, &minimal_config()).unwrap();
        let events =
            crate::task_session::task_session_events(&conn, &first.task_session_id).unwrap();
        let repeated_query =
            crate::task_session::task_session_events(&conn, &first.task_session_id).unwrap();
        assert_eq!(
            serde_json::to_vec(&events).unwrap(),
            serde_json::to_vec(&repeated_query).unwrap(),
        );
        let supplied: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == ContextUseEventType::ContextSupplied)
            .collect();
        assert_eq!(supplied.len(), first.context_ir.working_set.len() * 2);
        assert!(events
            .iter()
            .all(|event| event.event_type != ContextUseEventType::ContextRecompiled));
        let ordinals: std::collections::BTreeSet<i64> = supplied
            .iter()
            .map(|event| event.details["delivery_ordinal"].as_i64().unwrap())
            .collect();
        assert_eq!(ordinals, std::collections::BTreeSet::from([1, 2]));
        for event in supplied {
            let ordinal = event.details["delivery_ordinal"].as_i64().unwrap();
            assert_eq!(
                event.event_id,
                crate::task_session::event_id_for(
                    &first.task_session_id,
                    ContextUseEventType::ContextSupplied,
                    &format!(
                        "{}:{ordinal}:{}",
                        first.context_ir.context_hash,
                        event.item_id.as_deref().unwrap(),
                    ),
                ),
            );
        }
    }

    #[test]
    fn partial_context_ir_telemetry_marks_only_selected_items_as_supplied() {
        let (_db, ws, db_path) = context_ir_fixture(
            "export function alpha() { return 1; }\nexport function beta() { return 2; }\nexport function gamma() { return 3; }\n",
        );
        let out = build_context_ir_output(
            ws.path(),
            "inspect module".to_string(),
            None,
            vec!["src/a.ts".to_string()],
            vec![],
            1,
            32768,
            8000,
            Some(&db_path),
        )
        .unwrap();
        assert!(out.context_ir.omissions.truncated);
        assert_eq!(out.context_ir.working_set.len(), 1);

        let conn = catalogue::open_connection(&db_path, &minimal_config()).unwrap();
        let events = crate::task_session::task_session_events(&conn, &out.task_session_id).unwrap();
        let supplied: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == ContextUseEventType::ContextSupplied)
            .collect();
        assert_eq!(supplied.len(), 1);
        assert_eq!(
            supplied[0].item_id.as_deref(),
            Some(out.context_ir.working_set[0].item_id.as_str())
        );
        assert_eq!(
            supplied[0].entity_id.as_deref(),
            Some(out.context_ir.working_set[0].entity_id.as_str())
        );
        assert!(events.iter().all(|event| {
            event.item_id.as_ref().is_none_or(|item_id| {
                out.context_ir
                    .working_set
                    .iter()
                    .any(|item| &item.item_id == item_id)
            })
        }));
    }

    #[test]
    fn mixed_context_ir_telemetry_records_only_the_evidence_actually_supplied() {
        let (_db, ws, db_path) = context_ir_fixture("export function alpha() { return 1; }\n");
        let out = build_context_ir_output(
            ws.path(),
            "inspect mixed evidence".to_string(),
            None,
            vec![],
            vec!["alpha".to_string(), "missing::symbol".to_string()],
            10,
            32768,
            8000,
            Some(&db_path),
        )
        .unwrap();
        assert_eq!(
            out.context_ir.status.working_set_status,
            crate::context_ir::WorkingSetStatus::Partial
        );
        let conn = catalogue::open_connection(&db_path, &minimal_config()).unwrap();
        let supplied: Vec<_> =
            crate::task_session::task_session_events(&conn, &out.task_session_id)
                .unwrap()
                .into_iter()
                .filter(|event| event.event_type == ContextUseEventType::ContextSupplied)
                .collect();
        assert_eq!(supplied.len(), out.context_ir.working_set.len());
        assert!(supplied.iter().all(|event| event
            .entity_id
            .as_deref()
            .is_some_and(|id| id.contains("alpha"))));
    }

    #[test]
    fn blocked_context_ir_without_generation_persists_no_supplied_events() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::write(ws_dir.path().join("a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        drop(conn);

        let out = build_context_ir_output(
            ws_dir.path(),
            "inspect unavailable context".to_string(),
            None,
            vec![],
            vec![],
            10,
            32768,
            8000,
            Some(&db_path),
        )
        .unwrap();
        assert_eq!(
            out.context_ir.status.working_set_status,
            crate::context_ir::WorkingSetStatus::Blocked
        );
        let conn = catalogue::open_connection(&db_path, &minimal_config()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM context_use_event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn context_ir_cmd_respects_declared_task_kind() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let out = build_context_ir_output(
            ws_dir.path(),
            "look at alpha".to_string(),
            Some("audit".to_string()),
            vec![],
            vec!["alpha".to_string()],
            10,
            32768,
            8000,
            Some(&db_path),
        )
        .unwrap();
        assert_eq!(out.task_kind, crate::context_ir::TaskKind::Audit);
        assert_eq!(
            out.task_kind_source,
            crate::context_ir::TaskKindSource::Declared
        );
        assert!(out.kind_rule_id.is_none());
    }

    #[test]
    fn serving_build_cmd_produces_a_ready_projection() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let out = build_serving_build_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert!(out.ok);
        assert_eq!(out.state, "ready");
        assert!(out.card_count >= 1);
    }

    #[test]
    fn generation_delta_cmd_computes_and_persists() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        let gen1 = discovery::reconcile(&ws, &conn, &cfg)
            .unwrap()
            .candidate_generation_id;

        std::fs::write(
            ws_dir.path().join("src/b.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        let gen2 = discovery::reconcile(&ws, &conn, &cfg)
            .unwrap()
            .candidate_generation_id;

        let delta =
            build_generation_delta_output(ws_dir.path(), &gen1, &gen2, Some(&db_path)).unwrap();
        assert!(delta
            .files
            .changes
            .iter()
            .any(|c| c.entity_id == "src/b.ts"));

        let persisted: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_delta WHERE delta_id = ?1",
                rusqlite::params![delta.delta_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            persisted, 1,
            "generation-delta CLI command must persist the computed delta"
        );
    }

    #[test]
    fn doctor_detects_stale_serving_generations() {
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let mut conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws =
            workspace::register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        let report = discovery::reconcile(&ws, &conn, &cfg).unwrap();

        // Simulate a Serving Plane build left `building` by a crashed process.
        with_cli_writer_lease(&mut conn, &ws.workspace_id, |connection| {
            connection.execute(
                "INSERT INTO serving_generation (
                    serving_generation_id, workspace_id, generation_id, serving_schema_version,
                    projection_policy_version, state, source_fact_fingerprint, card_count, edge_count,
                    coverage_rollup_count, build_started_at
                ) VALUES ('serve_stale', ?1, ?2, '1.0.0', 'projection-v1.0.0', 'building', 'fp', 0, 0, 0, ?3)",
                rusqlite::params![
                    ws.workspace_id,
                    report.candidate_generation_id,
                    migrations::iso8601_now()
                ],
            )?;
            Ok(())
        })
        .unwrap();

        let out = build_doctor_output(ws_dir.path(), Some(&db_path)).unwrap();
        assert_eq!(out.stale_serving_generations, 1);
        assert!(out
            .issues
            .contains(&"stale_serving_generations".to_string()));
    }
}
