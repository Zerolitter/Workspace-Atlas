//! Production semantic reconcile orchestrator.
//!
//! Chains project-scope discovery → provider probe/spawn → bounded SCIP
//! decode/map → persistence → relationship resolution → evidence
//! merge/conflict → activation policy inside the same candidate-generation
//! transaction used by `discovery::reconcile`. Semantic work extends the
//! existing atomic generation lifecycle rather than creating a parallel index.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Transaction};

use crate::config::Config;
use crate::error::Result;
use crate::migrations::iso8601_now;
use crate::project_scope::{self, MarkerRule, ProjectScope};
use crate::provider_contract::{NetworkIsolationState, ProviderOutcome, PROTOCOL_VERSION};
use crate::provider_persistence::{self, ExecutionTelemetry, InvalidationReason};
use crate::providers;
use crate::scip_decoder::{decode_index, DecodeLimits};
use crate::scip_mapping::{self, MappedDocument};
use crate::workspace::WorkspaceRecord;

pub const DECODER_VERSION: &str = "scip-decoder-1.0.0";
pub const SCIP_MAPPING_POLICY_VERSION: &str = "1.0.0";
pub const RESOLVER_POLICY_VERSION: &str = "semantic-resolution-1.0.0";
pub const PROJECTION_POLICY_VERSION: &str = "preferred-evidence-1.0.0";
pub const NORMALIZED_SCHEMA_VERSION: &str = "1.1.0";

fn mapping_policy_version_for(provider_name: &str) -> String {
    if provider_name == "scip-typescript" {
        return format!("scip-typescript-mapping-{SCIP_MAPPING_POLICY_VERSION}");
    }
    format!("{provider_name}-scip-mapping-{SCIP_MAPPING_POLICY_VERSION}")
}

/// One (provider, project scope) pairing this reconcile pass must consider.
pub struct PlannedSemanticExecution {
    pub provider_name: String,
    pub provider_version: String,
    pub command: String,
    pub arguments: Vec<String>,
    pub probe_arguments: Vec<String>,
    pub languages: Vec<String>,
    pub project_markers: Vec<String>,
    pub required: bool,
    pub enabled: bool,
    pub priority: i64,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub configuration: serde_json::Value,
    pub scope: ProjectScope,
}

/// Discover every (semantic provider, project scope) pairing configured for
/// this workspace. Pure filesystem discovery — no provider is spawned here.
pub fn discover_semantic_plans(root: &Path, config: &Config) -> Vec<PlannedSemanticExecution> {
    let mut plans = Vec::new();
    for entry in &config.providers {
        if entry.kind == "builtin" {
            continue;
        }
        if entry.scope != "project" {
            continue; // V1.1 pilot only implements project-scoped semantic execution
        }
        let Some(command) = entry.command.clone() else {
            continue;
        };
        let Some(provider_version) = entry.resolved_version() else {
            continue;
        };

        let markers: Vec<MarkerRule> = if entry.project_markers.is_empty() {
            project_scope::default_project_markers()
                .into_iter()
                .filter(|m| {
                    m.language_family
                        .map(|lf| entry.languages.iter().any(|l| l == lf))
                        .unwrap_or(false)
                })
                .collect()
        } else {
            let defaults = project_scope::default_project_markers();
            entry
                .project_markers
                .iter()
                .filter_map(|m| defaults.iter().find(|d| d.file_name == m).cloned())
                .collect()
        };
        let marker_names: Vec<String> = markers.iter().map(|m| m.file_name.to_string()).collect();

        let scopes = match project_scope::discover_project_scopes(root, &markers) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for scope in scopes {
            plans.push(PlannedSemanticExecution {
                provider_name: entry.name.clone(),
                provider_version: provider_version.clone(),
                command: command.clone(),
                arguments: entry.arguments.clone(),
                probe_arguments: entry.probe_arguments.clone(),
                languages: entry.languages.clone(),
                project_markers: marker_names.clone(),
                required: entry.required,
                enabled: entry.enabled,
                priority: entry.priority,
                timeout_ms: entry.timeout_ms.unwrap_or(120_000),
                max_output_bytes: entry.max_output_bytes.unwrap_or(536_870_912),
                configuration: entry.configuration.clone(),
                scope,
            });
        }
    }
    plans
}

#[derive(Debug, Clone, Default)]
pub struct SemanticReconcileOutcome {
    pub required_failure: bool,
    pub failure_reason: Option<String>,
    pub degraded: bool,
    pub scopes_considered: usize,
    pub executions_run: usize,
    pub executions_reused: usize,
    pub documents_processed: usize,
    pub symbols_persisted: usize,
    pub relationships_persisted: usize,
    pub resolutions_persisted: usize,
    pub conflicts_persisted: usize,
    pub corroborations_persisted: usize,
}

/// Run every planned semantic execution for this candidate generation and
/// persist its evidence, then run relationship resolution + evidence
/// merge/conflict over the generation's complete semantic fact set.
///
/// Must be called from inside the same transaction `discovery::reconcile`
/// uses to persist structural facts, *after* `file_identity`/`file_revision`
/// rows for present files already exist (so SCIP documents can be linked to
/// a live revision).
pub fn run_semantic_reconcile(
    tx: &Transaction<'_>,
    ws: &WorkspaceRecord,
    generation_id: &str,
    root: &Path,
    config: &Config,
) -> Result<SemanticReconcileOutcome> {
    let plans = discover_semantic_plans(root, config);
    let mut outcome = SemanticReconcileOutcome {
        scopes_considered: plans.len(),
        ..Default::default()
    };

    for plan in &plans {
        if !plan.enabled {
            continue;
        }
        run_one_plan(tx, ws, generation_id, root, config, plan, &mut outcome)?;
    }

    if !outcome.required_failure {
        run_resolution_pass(tx, ws, generation_id, &mut outcome)?;
    }

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn run_one_plan(
    tx: &Transaction<'_>,
    ws: &WorkspaceRecord,
    generation_id: &str,
    root: &Path,
    config: &Config,
    plan: &PlannedSemanticExecution,
    outcome: &mut SemanticReconcileOutcome,
) -> Result<()> {
    let scope_key = format!(
        "project:{}",
        plan.scope
            .primary_manifest_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );

    // --- Probe -------------------------------------------------------
    let env = crate::provider_runtime::build_environment(
        &config.provider_runtime.allowed_environment,
        config.provider_runtime.inherit_environment,
    );
    let probe = crate::provider_runtime::probe_provider(
        &plan.provider_name,
        &plan.command,
        &plan.probe_arguments,
        root,
        env.clone(),
        std::time::Duration::from_millis(plan.timeout_ms.min(15_000)),
    );
    let executable_hash = probe.executable_hash.clone();

    let mapping_policy_version = mapping_policy_version_for(&plan.provider_name);
    let descriptor = providers::external_scip_descriptor(
        &plan.provider_name,
        &plan.provider_version,
        plan.languages.clone(),
        plan.project_markers.clone(),
        &plan.command,
        plan.arguments.clone(),
        executable_hash.as_deref(),
        &plan.configuration,
        &mapping_policy_version,
    );
    let provider_key = provider_persistence::register_provider_descriptor(tx, &descriptor)?;
    let configuration_hash = crate::hashing::content_hash_of_bytes(
        &crate::provider_contract::canonical_json_bytes(&plan.configuration),
    );
    // CHECK(length(provider_set_hash) = 64) on `provider_execution`: this
    // single-provider-execution's own identity contribution, computed the
    // same deterministic way as `index_generation.provider_set_hash`
    // (`provider_contract::provider_set_hash`) rather than a placeholder.
    let provider_set_hash = crate::provider_contract::provider_set_hash(&[
        crate::provider_contract::ProviderSetMember {
            provider_key: &provider_key,
            descriptor_fingerprint: &descriptor.fingerprint,
            configuration_hash: &configuration_hash,
            priority: plan.priority,
        },
    ]);
    provider_persistence::set_workspace_provider_policy(
        tx,
        &ws.workspace_id,
        &provider_key,
        plan.enabled,
        plan.required,
        plan.priority,
        &plan.configuration.to_string(),
        &configuration_hash,
        plan.timeout_ms,
        config.provider_runtime.max_stdout_bytes,
        config.provider_runtime.max_stderr_bytes,
        plan.max_output_bytes,
        network_isolation_policy_str(&config.provider_runtime.network_isolation_policy),
    )?;

    let project_scope_id = provider_persistence::record_project_scope(
        tx,
        &ws.workspace_id,
        generation_id,
        &plan.scope,
    )?;

    if probe.status != crate::provider_contract::ProbeStatus::Available {
        provider_persistence::record_coverage(
            tx,
            generation_id,
            None,
            "provider",
            &provider_key,
            provider_persistence::coverage_status_for_outcome(ProviderOutcome::Unavailable),
            "[]",
            "[]",
            "{}",
        )?;
        record_outcome_failure(
            outcome,
            plan.required,
            ProviderOutcome::Unavailable,
            format!(
                "{} unavailable: {:?}",
                plan.provider_name, probe.diagnostics
            ),
        );
        return Ok(());
    }

    // --- Fingerprint + reuse check ------------------------------------
    let relevant_files = list_relevant_files(&plan.scope.canonical_root, &plan.languages)?;
    let input_fingerprint = project_scope::execution_input_fingerprint(
        &ws.workspace_id,
        &plan.scope,
        &relevant_files,
        &descriptor.fingerprint,
        &configuration_hash,
        PROTOCOL_VERSION,
        NORMALIZED_SCHEMA_VERSION,
        &mapping_policy_version,
    );

    if let Some(reused) = provider_persistence::find_reusable_execution(
        tx,
        &ws.workspace_id,
        &provider_key,
        "project",
        &scope_key,
        &input_fingerprint,
        NORMALIZED_SCHEMA_VERSION,
        &mapping_policy_version,
    )? {
        outcome.executions_reused += 1;
        provider_persistence::record_coverage(
            tx,
            generation_id,
            None,
            "provider",
            &provider_key,
            &reused.status,
            "[]",
            "[]",
            "{}",
        )?;
        return Ok(());
    }

    // A miss means something in the fingerprint changed; record why when we
    // can identify a specific prior execution for this (provider, scope).
    let prior_execution_id: Option<String> = tx
        .query_row(
            "SELECT provider_execution_id FROM provider_execution
             WHERE workspace_id = ?1 AND provider_key = ?2 AND scope_kind = 'project' AND scope_key = ?3
             ORDER BY created_at DESC LIMIT 1",
            params![ws.workspace_id, provider_key, scope_key],
            |r| r.get(0),
        )
        .optional_ok();
    if prior_execution_id.is_some() {
        provider_persistence::record_invalidation_event(
            tx,
            &ws.workspace_id,
            generation_id,
            &provider_key,
            "project",
            &scope_key,
            prior_execution_id.as_deref(),
            InvalidationReason::SourceChanged,
            None,
            &input_fingerprint,
        )?;
    }

    // --- Spawn ---------------------------------------------------------
    let execution_id = format!("pex_{}", &input_fingerprint[..24]);
    let temp_root = std::env::temp_dir().join(".atlas-provider-tmp");
    let output_dir = crate::provider_runtime::make_execution_output_dir(&temp_root, &execution_id)?;
    let output_file = output_dir.join("index.scip");

    let arguments = plan
        .arguments
        .iter()
        .map(|argument| argument.replace("{output_file}", &output_file.to_string_lossy()))
        .collect();
    let started_at = iso8601_now();
    let spawn_plan = crate::provider_runtime::SpawnPlan {
        command: plan.command.clone(),
        arguments,
        cwd: plan.scope.canonical_root.clone(),
        environment: env,
        timeout: std::time::Duration::from_millis(plan.timeout_ms),
        graceful_cancel: std::time::Duration::from_millis(
            config.provider_runtime.graceful_cancel_ms,
        ),
        max_stdout_bytes: config.provider_runtime.max_stdout_bytes as usize,
        max_stderr_bytes: config.provider_runtime.max_stderr_bytes as usize,
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let spawn_outcome = match crate::provider_runtime::spawn_and_wait(&spawn_plan, &cancel) {
        Ok(o) => o,
        Err(e) => {
            // SEC-002: a genuine internal spawn-machinery error (not a typed
            // ProviderOutcome) must not leak the raw output directory either.
            cleanup_temp_dir(&output_dir);
            return Err(e);
        }
    };
    let finished_at = iso8601_now();

    let network_isolation_state =
        network_isolation_state_for(&config.provider_runtime.network_isolation_policy);

    let (status, exit_code, stdout_bytes, stderr_bytes, stdout_truncated, stderr_truncated): (
        ProviderOutcome,
        Option<i64>,
        i64,
        i64,
        bool,
        bool,
    ) = match &spawn_outcome {
        crate::provider_runtime::SpawnOutcome::Exited {
            exit_code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => {
            let ok = *exit_code == Some(0);
            (
                if ok {
                    ProviderOutcome::Complete
                } else {
                    ProviderOutcome::Failed
                },
                exit_code.map(|c| c as i64),
                stdout.len() as i64,
                stderr.len() as i64,
                *stdout_truncated,
                *stderr_truncated,
            )
        }
        crate::provider_runtime::SpawnOutcome::TimedOut {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => (
            ProviderOutcome::TimedOut,
            None,
            stdout.len() as i64,
            stderr.len() as i64,
            *stdout_truncated,
            *stderr_truncated,
        ),
        crate::provider_runtime::SpawnOutcome::Cancelled {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => (
            ProviderOutcome::Cancelled,
            None,
            stdout.len() as i64,
            stderr.len() as i64,
            *stdout_truncated,
            *stderr_truncated,
        ),
        crate::provider_runtime::SpawnOutcome::Unavailable { .. } => {
            (ProviderOutcome::Unavailable, None, 0, 0, false, false)
        }
    };

    if !matches!(status, ProviderOutcome::Complete) {
        let telemetry = ExecutionTelemetry {
            status,
            network_isolation_state,
            started_at,
            finished_at,
            exit_code,
            stdout_hash: None,
            stderr_hash: None,
            stdout_bytes,
            stderr_bytes,
            stdout_truncated,
            stderr_truncated,
            output_hash: None,
            output_bytes: None,
        };
        let pex_id = provider_persistence::record_provider_execution(
            tx,
            &ws.workspace_id,
            generation_id,
            &provider_key,
            Some(&project_scope_id),
            "project",
            &scope_key,
            &input_fingerprint,
            &provider_set_hash,
            NORMALIZED_SCHEMA_VERSION,
            &mapping_policy_version,
            &telemetry,
        )?;
        provider_persistence::record_runtime_diagnostic(
            tx,
            &pex_id,
            "error",
            "provider_execution_failed",
            &format!("{status:?}"),
            None,
        )?;
        provider_persistence::record_coverage(
            tx,
            generation_id,
            None,
            "provider",
            &provider_key,
            provider_persistence::coverage_status_for_outcome(status),
            "[]",
            "[]",
            "{}",
        )?;
        cleanup_temp_dir(&output_dir);
        record_outcome_failure(
            outcome,
            plan.required,
            status,
            format!("{} execution {status:?}", plan.provider_name),
        );
        return Ok(());
    }

    // --- Validate + decode + map ---------------------------------------
    let artifact = match crate::provider_runtime::validate_output_artifact(
        &output_dir,
        Path::new("index.scip"),
        plan.max_output_bytes,
    ) {
        Ok(a) => a,
        Err(e) => {
            let telemetry = ExecutionTelemetry {
                status: ProviderOutcome::OutputRejected,
                network_isolation_state,
                started_at,
                finished_at,
                exit_code,
                stdout_hash: None,
                stderr_hash: None,
                stdout_bytes,
                stderr_bytes,
                stdout_truncated,
                stderr_truncated,
                output_hash: None,
                output_bytes: None,
            };
            let pex_id = provider_persistence::record_provider_execution(
                tx,
                &ws.workspace_id,
                generation_id,
                &provider_key,
                Some(&project_scope_id),
                "project",
                &scope_key,
                &input_fingerprint,
                &provider_set_hash,
                NORMALIZED_SCHEMA_VERSION,
                &mapping_policy_version,
                &telemetry,
            )?;
            provider_persistence::record_runtime_diagnostic(
                tx,
                &pex_id,
                "error",
                "output_rejected",
                &format!("{e}"),
                None,
            )?;
            provider_persistence::record_coverage(
                tx,
                generation_id,
                None,
                "provider",
                &provider_key,
                provider_persistence::coverage_status_for_outcome(ProviderOutcome::OutputRejected),
                "[]",
                "[]",
                "{}",
            )?;
            cleanup_temp_dir(&output_dir);
            record_outcome_failure(
                outcome,
                plan.required,
                ProviderOutcome::OutputRejected,
                format!("{e}"),
            );
            return Ok(());
        }
    };
    let raw_bytes = match std::fs::read(&artifact.path) {
        Ok(b) => b,
        Err(e) => {
            cleanup_temp_dir(&output_dir);
            return Err(e.into());
        }
    };
    let decoded = decode_index(&raw_bytes, &DecodeLimits::default());
    let index = match decoded {
        Ok(i) => i,
        Err(e) => {
            let telemetry = ExecutionTelemetry {
                status: ProviderOutcome::DecodeFailed,
                network_isolation_state,
                started_at,
                finished_at,
                exit_code,
                stdout_hash: None,
                stderr_hash: None,
                stdout_bytes,
                stderr_bytes,
                stdout_truncated,
                stderr_truncated,
                output_hash: Some(artifact.sha256.clone()),
                output_bytes: Some(artifact.bytes as i64),
            };
            let pex_id = provider_persistence::record_provider_execution(
                tx,
                &ws.workspace_id,
                generation_id,
                &provider_key,
                Some(&project_scope_id),
                "project",
                &scope_key,
                &input_fingerprint,
                &provider_set_hash,
                NORMALIZED_SCHEMA_VERSION,
                &mapping_policy_version,
                &telemetry,
            )?;
            provider_persistence::record_runtime_diagnostic(
                tx,
                &pex_id,
                "fatal",
                "decode_failed",
                &format!("{e}"),
                None,
            )?;
            provider_persistence::record_coverage(
                tx,
                generation_id,
                None,
                "provider",
                &provider_key,
                provider_persistence::coverage_status_for_outcome(ProviderOutcome::DecodeFailed),
                "[]",
                "[]",
                "{}",
            )?;
            cleanup_temp_dir(&output_dir);
            record_outcome_failure(
                outcome,
                plan.required,
                ProviderOutcome::DecodeFailed,
                format!("{e}"),
            );
            return Ok(());
        }
    };

    let defined = scip_mapping::locally_defined_symbol_set(&index);
    let mut mapped_docs: Vec<MappedDocument> = Vec::new();
    let mut any_partial = false;
    for doc in &index.documents {
        let mapped = scip_mapping::map_document(root, &plan.scope.canonical_root, doc, &defined);
        any_partial |= mapped.partial;
        mapped_docs.push(mapped);
    }

    let final_status = if any_partial {
        ProviderOutcome::Partial
    } else {
        ProviderOutcome::Complete
    };
    let telemetry = ExecutionTelemetry {
        status: final_status,
        network_isolation_state,
        started_at,
        finished_at,
        exit_code,
        stdout_hash: None,
        stderr_hash: None,
        stdout_bytes,
        stderr_bytes,
        stdout_truncated,
        stderr_truncated,
        output_hash: Some(artifact.sha256.clone()),
        output_bytes: Some(artifact.bytes as i64),
    };
    let pex_id = provider_persistence::record_provider_execution(
        tx,
        &ws.workspace_id,
        generation_id,
        &provider_key,
        Some(&project_scope_id),
        "project",
        &scope_key,
        &input_fingerprint,
        &provider_set_hash,
        NORMALIZED_SCHEMA_VERSION,
        &mapping_policy_version,
        &telemetry,
    )?;
    provider_persistence::record_coverage(
        tx,
        generation_id,
        None,
        "provider",
        &provider_key,
        provider_persistence::coverage_status_for_outcome(final_status),
        "[]",
        "[]",
        "{}",
    )?;
    outcome.executions_run += 1;
    if matches!(final_status, ProviderOutcome::Partial) {
        outcome.degraded = true;
    }

    let mut this_execution_symbols: HashSet<String> = HashSet::new();

    for mapped in &mapped_docs {
        outcome.documents_processed += 1;
        let document_status = if mapped.partial {
            "partial"
        } else {
            "included"
        };

        // Look up this document's live file_identity/file_revision under
        // the CURRENT candidate generation (SCIP-004): only documents that
        // are Present files this generation get facts persisted.
        let revision: Option<(String, String)> = tx
            .query_row(
                "SELECT gf.file_id, gf.revision_id FROM generation_file gf
                 WHERE gf.generation_id = ?1 AND gf.canonical_path = ?2 AND gf.presence_state = 'present'",
                params![generation_id, mapped.canonical_path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional_ok();

        provider_persistence::record_execution_document(
            tx,
            &pex_id,
            &mapped.canonical_path,
            Some(&mapped.canonical_path),
            if revision.is_some() {
                document_status
            } else {
                "unmapped"
            },
            None,
            Some(&scip_mapping::document_normalized_digest(mapped)),
            None,
        )?;

        let Some((_file_id, revision_id)) = revision else {
            continue;
        };
        if mapped.partial {
            continue;
        }

        let document_text =
            std::fs::read_to_string(root.join(&mapped.canonical_path)).unwrap_or_default();
        let run_id = provider_persistence::ensure_semantic_extractor_run(
            tx,
            &revision_id,
            &plan.provider_name,
            &descriptor.version,
            &configuration_hash,
            NORMALIZED_SCHEMA_VERSION,
            "complete",
            document_text.len() as i64,
        )?;
        // scip-typescript@0.4.0 predates mandatory Document.position_encoding
        // metadata but its contract is UTF-16 code-unit columns. Keep that
        // compatibility explicit; every other provider must declare its
        // encoding in the SCIP document or its spans fail closed.
        let position_encoding = match mapped.position_encoding {
            crate::scip_decoder::PositionEncoding::Unspecified
                if plan.provider_name == "scip-typescript" =>
            {
                crate::scip_decoder::PositionEncoding::Utf16
            }
            declared => declared,
        };

        provider_persistence::link_execution_to_extractor_run(tx, &pex_id, &run_id)?;

        for sym in &mapped.symbols {
            if provider_persistence::record_semantic_symbol_fact(
                tx,
                &run_id,
                &revision_id,
                &document_text,
                position_encoding,
                sym,
            )?
            .is_some()
            {
                outcome.symbols_persisted += 1;
                this_execution_symbols.insert(sym.canonical_symbol_key.clone());
            }
        }
        for rel in &mapped.relationships {
            provider_persistence::record_semantic_relationship_fact(
                tx,
                &run_id,
                &revision_id,
                rel,
            )?;
            outcome.relationships_persisted += 1;
        }
    }

    cleanup_temp_dir(&output_dir);
    Ok(())
}

fn record_outcome_failure(
    outcome: &mut SemanticReconcileOutcome,
    required: bool,
    provider_outcome: ProviderOutcome,
    reason: String,
) {
    match provider_persistence::decide_activation(required, provider_outcome) {
        provider_persistence::ActivationDecision::Block {
            reason: block_reason,
        } => {
            outcome.required_failure = true;
            outcome.failure_reason = Some(
                outcome
                    .failure_reason
                    .clone()
                    .map(|prev| format!("{prev}; {block_reason}"))
                    .unwrap_or(block_reason),
            );
        }
        provider_persistence::ActivationDecision::Proceed { degraded } => {
            outcome.degraded |= degraded;
            let _ = reason;
        }
    }
}

fn cleanup_temp_dir(dir: &Path) {
    // SEC-002: raw provider output is temporary by default. Best-effort —
    // an OS-level cleanup failure is not itself a reconcile failure, but it
    // must never be silently unattempted.
    let _ = std::fs::remove_dir_all(dir);
}

fn list_relevant_files(scope_root: &Path, languages: &[String]) -> Result<Vec<(String, String)>> {
    let extensions: Vec<&str> = languages
        .iter()
        .flat_map(|l| match l.as_str() {
            "typescript" => vec!["ts", "tsx"],
            "javascript" => vec!["js", "jsx"],
            "rust" => vec!["rs"],
            other => vec![other],
        })
        .collect();
    let extensions: Vec<&str> = if extensions.is_empty() {
        vec!["ts", "tsx", "js", "jsx"]
    } else {
        extensions
    };

    let mut out = Vec::new();
    let mut stack = vec![scope_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if matches!(
                    name.as_ref(),
                    "node_modules"
                        | "vendor"
                        | "target"
                        | "dist"
                        | "build"
                        | ".git"
                        | ".workspace_atlas"
                ) {
                    continue;
                }
                stack.push(path);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    if let Ok(rel) = path.strip_prefix(scope_root) {
                        if let Ok(hash) = crate::hashing::content_hash_of_file(&path) {
                            out.push((rel.to_string_lossy().replace('\\', "/"), hash));
                        }
                    }
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn network_isolation_policy_str(policy: &crate::config::NetworkIsolationPolicy) -> &'static str {
    use crate::config::NetworkIsolationPolicy::*;
    match policy {
        NotRequired => "not_required",
        RequireEnforced => "require_enforced",
        BestEffortAllowed => "best_effort_allowed",
    }
}

fn network_isolation_state_for(
    policy: &crate::config::NetworkIsolationPolicy,
) -> NetworkIsolationState {
    use crate::config::NetworkIsolationPolicy::*;
    match policy {
        RequireEnforced => NetworkIsolationState::NotAvailable, // cross-platform Atlas core cannot honestly claim `enforced`
        _ => NetworkIsolationState::NotAvailable,
    }
}

// ---------------------------------------------------------------------------
// Resolution + evidence merge pass (INV-007, RES-*, MERGE-*)
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
fn run_resolution_pass(
    tx: &Transaction<'_>,
    ws: &WorkspaceRecord,
    generation_id: &str,
    outcome: &mut SemanticReconcileOutcome,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT rf.relationship_fact_id, rf.source_ref_kind, rf.source_ref_value,
                rf.target_ref_kind, rf.target_ref_value, rf.confidence, rf.evidence_reason,
                rf.relationship_type, pe.provider_key, wp.priority, pd.deterministic
         FROM relationship_fact rf
         JOIN generation_file gf ON gf.revision_id = rf.revision_id AND gf.generation_id = ?1
         JOIN provider_execution pe ON pe.provider_execution_id = (
             SELECT pe2.provider_execution_id
             FROM provider_execution_run per2
             JOIN provider_execution pe2 ON pe2.provider_execution_id = per2.provider_execution_id
             WHERE per2.extractor_run_id = rf.extractor_run_id AND pe2.workspace_id = ?2
             ORDER BY pe2.created_at DESC, pe2.provider_execution_id DESC
             LIMIT 1
         )
         JOIN provider_descriptor pd ON pd.provider_key = pe.provider_key
         JOIN workspace_provider wp ON wp.workspace_id = ?2 AND wp.provider_key = pe.provider_key
         WHERE rf.evidence_method = 'semantic'",
    )?;
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        f64,
        String,
        String,
        String,
        i64,
        bool,
    )> = stmt
        .query_map(params![generation_id, ws.workspace_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get::<_, i64>(10)? != 0,
            ))
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(stmt);

    if rows.is_empty() {
        return Ok(());
    }

    // Symbol universe reachable in this generation.
    let mut stmt = tx.prepare(
        "SELECT DISTINCT sf.canonical_symbol_key, gf.canonical_path
         FROM symbol_fact sf
         JOIN generation_file gf ON gf.revision_id = sf.revision_id AND gf.generation_id = ?1",
    )?;
    let symbol_rows: Vec<(String, String)> = stmt
        .query_map(params![generation_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(stmt);

    let globally_defined: HashMap<String, String> = symbol_rows.iter().cloned().collect();
    let mut by_display_name: HashMap<String, Vec<String>> = HashMap::new();
    for (key, _) in &symbol_rows {
        let name = key
            .rsplit(['/', '#', '.', ' '])
            .next()
            .unwrap_or(key)
            .to_string();
        by_display_name.entry(name).or_default().push(key.clone());
    }
    let known_files: HashSet<String> = symbol_rows.iter().map(|(_, p)| p.clone()).collect();
    let this_execution: HashSet<String> = HashSet::new(); // conservative: cross-execution symbols only, see canonical-key match below

    let universe = crate::resolution::SymbolUniverse {
        globally_defined: &globally_defined,
        this_execution: &this_execution,
        by_display_name: &by_display_name,
        known_files: &known_files,
    };

    // Cluster raw facts by (relationship_type, source, ) for corroboration/conflict.
    let mut clusters: HashMap<String, Vec<crate::resolution::EvidenceFact>> = HashMap::new();

    for (
        fact_id,
        source_ref_kind,
        source_ref_value,
        target_ref_kind,
        target_ref_value,
        confidence,
        reason_code,
        relationship_type,
        provider_key,
        provider_priority,
        deterministic,
    ) in &rows
    {
        let target_kind = if target_ref_kind == "external" {
            scip_mapping::RefKind::ExternalSymbol
        } else {
            scip_mapping::RefKind::Symbol
        };
        let source_document_path = if source_ref_kind == "path" {
            source_ref_value.clone()
        } else {
            String::new()
        };
        // `relationship_fact.target_ref_value` is the raw SCIP symbol
        // string (SCIP-005 "preserve exact provider symbol IDs"), but
        // `SymbolUniverse::globally_defined`/`by_display_name` are keyed by
        // `scip_mapping::canonical_symbol_key` (which prefixes global
        // symbols with `scip:` and scopes local symbols to their defining
        // document). Apply the identical transform here or every exact-key
        // match in `resolve_relationship` silently misses. A local target
        // can only ever be referenced from its own defining document, so
        // the *source*'s document path is the correct (and only available)
        // scoping path for a local target too.
        let canonical_target_value = if matches!(target_kind, scip_mapping::RefKind::Symbol) {
            scip_mapping::canonical_symbol_key(target_ref_value, &source_document_path)
        } else {
            target_ref_value.clone()
        };
        let synthetic = crate::scip_mapping::MappedRelationship {
            relationship_type: intern_relationship_type(relationship_type),
            source_canonical_document_path: source_document_path,
            source_start_line: 0,
            source_start_column: 0,
            source_end_line: 0,
            source_end_column: 0,
            target_ref_kind: target_kind,
            target_ref_value: canonical_target_value,
            provider_symbol_id: if source_ref_kind == "symbol" {
                source_ref_value.clone()
            } else {
                target_ref_value.clone()
            },
            confidence: *confidence,
            reason_code: intern_reason_code(reason_code),
        };

        let resolved = crate::resolution::resolve_relationship(&synthetic, &universe);
        crate::resolution::record_relationship_resolution(
            tx,
            &ws.workspace_id,
            generation_id,
            fact_id,
            None,
            RESOLVER_POLICY_VERSION,
            &resolved,
        )?;
        outcome.resolutions_persisted += 1;

        let resolved_target = match &resolved.resolved {
            Some(crate::resolution::ResolvedRef::Symbol(v)) => Some(v.clone()),
            Some(crate::resolution::ResolvedRef::File(v)) => Some(v.clone()),
            Some(crate::resolution::ResolvedRef::ExternalSymbol(v)) => Some(v.clone()),
            None => None,
        };
        let cluster_key = format!(
            "{}\u{1f}{}\u{1f}{}",
            relationship_type,
            source_ref_value,
            target_ref_value.split_whitespace().next().unwrap_or("")
        );
        clusters
            .entry(cluster_key)
            .or_default()
            .push(crate::resolution::EvidenceFact {
                fact_id: fact_id.clone(),
                provider_key: provider_key.clone(),
                evidence_tier_rank: crate::providers::ProviderTier::SemanticIndex.default_rank(),
                provider_priority: Some(*provider_priority),
                deterministic: *deterministic,
                confidence: *confidence,
                resolved_target,
            });
    }

    for (subject_key, facts) in &clusters {
        if let Some(cluster_outcome) = crate::resolution::project_cluster(facts) {
            crate::resolution::persist_cluster_outcome(
                tx,
                &ws.workspace_id,
                generation_id,
                "relationship",
                subject_key,
                &cluster_outcome,
                PROJECTION_POLICY_VERSION,
            )?;
            match cluster_outcome {
                crate::resolution::ClusterOutcome::Corroborated { .. } if facts.len() > 1 => {
                    outcome.corroborations_persisted += 1
                }
                crate::resolution::ClusterOutcome::Conflict { .. } => {
                    outcome.conflicts_persisted += 1
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Map a `relationship_fact.relationship_type` DB value (a bounded `CHECK`
/// vocabulary this module itself writes — see
/// `provider_persistence::record_semantic_relationship_fact`) back to the
/// matching `&'static str` constant `scip_mapping::MappedRelationship`
/// expects. An unrecognized value (should not occur; defensive) maps to the
/// generic `"REFERENCES"` reconstruction rather than fabricating a leaked
/// allocation.
fn intern_relationship_type(db_value: &str) -> &'static str {
    match db_value {
        "imports" => "IMPORTS",
        "calls" => "CALLS",
        "implements" => "IMPLEMENTS",
        _ => "REFERENCES",
    }
}

/// Same bounded-vocabulary interning for `relationship_fact.evidence_reason`
/// (the reason codes `scip_mapping`/`resolution` themselves emit).
fn intern_reason_code(db_value: &str) -> &'static str {
    match db_value {
        "composed_span_match" => crate::resolution::reason::COMPOSED_SPAN_MATCH,
        "scip_import_role" => "scip_import_role",
        "scip_symbol_relationship" => "scip_symbol_relationship",
        _ => "scip_occurrence",
    }
}

trait OptionalOk<T> {
    fn optional_ok(self) -> Option<T>;
}
impl<T> OptionalOk<T> for rusqlite::Result<T> {
    fn optional_ok(self) -> Option<T> {
        self.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::workspace::register_workspace;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn semantic_config(display_name: &str, required: bool) -> Config {
        let toml = format!(
            r#"
schema_version = "1.1.0"
[workspace]
display_name = "{display_name}"
[[providers]]
name = "scip-typescript"
kind = "external_scip"
tier = "semantic_index"
scope = "project"
enabled = true
required = {required}
priority = 800
languages = ["typescript", "javascript"]
project_markers = ["tsconfig.json"]
command = "scip-typescript"
arguments = ["index", "--output", "{{output_file}}"]
probe_arguments = ["--version"]
timeout_ms = 60000
max_output_bytes = 536870912
"#
        );
        Config::parse(&toml).unwrap()
    }

    fn structural_only_config(display_name: &str) -> Config {
        Config::parse(&format!(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"{display_name}\"\n"
        ))
        .unwrap()
    }

    fn rust_semantic_config(display_name: &str, required: bool) -> Config {
        let toml = format!(
            r#"
schema_version = "1.1.0"
[workspace]
display_name = "{display_name}"
[[providers]]
name = "rust-analyzer"
version = "1.94.1"
kind = "external_scip"
tier = "semantic_index"
scope = "project"
enabled = true
required = {required}
priority = 731
languages = ["rust"]
project_markers = ["Cargo.toml"]
command = "rust-analyzer"
arguments = ["scip", ".", "--output", "{{output_file}}"]
probe_arguments = ["--version"]
"#
        );
        Config::parse(&toml).unwrap()
    }

    #[test]
    fn rust_plan_uses_cargo_scope_rs_inputs_and_provider_specific_mapping_identity() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        );
        write(&dir.path().join("src/lib.rs"), "pub fn rust_symbol() {}\n");
        write(
            &dir.path().join("src/ignored.ts"),
            "export function tsSymbol() {}\n",
        );
        let cfg = rust_semantic_config("rust-plan", false);
        let plans = discover_semantic_plans(dir.path(), &cfg);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].provider_name, "rust-analyzer");
        assert_eq!(plans[0].provider_version, "1.94.1");
        assert_eq!(plans[0].scope.language_family.as_deref(), Some("rust"));
        assert_eq!(
            mapping_policy_version_for(&plans[0].provider_name),
            "rust-analyzer-scip-mapping-1.0.0"
        );
        let files = list_relevant_files(dir.path(), &plans[0].languages).unwrap();
        assert_eq!(
            files.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
            vec!["src/lib.rs"]
        );
    }

    #[test]
    fn real_rust_analyzer_reconcile_preserves_identity_priority_and_unicode_ranges() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("Cargo.toml"),
            "[package]\nname = \"rust_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let source = "pub const CAFÉ: &str = \"café\";\npub fn target_λ() -> &'static str { CAFÉ }\npub fn caller_é() -> &'static str { target_λ() }\n";
        write(&dir.path().join("src/lib.rs"), source);
        let cfg = rust_semantic_config("rust-e2e", true);
        let ws = register_workspace(
            &conn,
            dir.path(),
            &cfg,
            &dir.path().join(".atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let report = crate::discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(report.activation, "committed", "{report:?}");
        assert!(report.semantic_executions_run > 0, "{report:?}");
        assert!(report.semantic_symbols_persisted > 0, "{report:?}");
        assert!(report.semantic_relationships_persisted > 0, "{report:?}");
        assert!(report.semantic_resolutions_persisted > 0, "{report:?}");

        let provenance: (String, String, String, i64) = conn
            .query_row(
                "SELECT pd.provider_name, pd.provider_version, pe.mapping_policy_version, wp.priority
                 FROM provider_execution pe
                 JOIN provider_descriptor pd ON pd.provider_key = pe.provider_key
                 JOIN workspace_provider wp ON wp.workspace_id = pe.workspace_id AND wp.provider_key = pe.provider_key
                 WHERE pe.generation_id = ?1",
                params![report.candidate_generation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            provenance,
            (
                "rust-analyzer".to_string(),
                "1.94.1".to_string(),
                "rust-analyzer-scip-mapping-1.0.0".to_string(),
                731
            )
        );

        let (start_byte, end_byte): (i64, i64) = conn
            .query_row(
                "SELECT sf.start_byte, sf.end_byte
                 FROM symbol_fact sf
                 JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
                 WHERE er.provider_name = 'rust-analyzer' AND sf.provider_symbol_id LIKE '%target_λ%' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(&source[start_byte as usize..end_byte as usize], "target_λ");

        let wrong_identity_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_descriptor WHERE provider_name = 'scip-typescript'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_identity_count, 0);

        let ws2 = crate::workspace::load_workspace_by_root(&conn, dir.path())
            .unwrap()
            .unwrap();
        let replay = crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        assert_eq!(replay.activation, "committed", "{replay:?}");
        assert!(replay.semantic_executions_reused > 0, "{replay:?}");
        let reused_coverage: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM coverage_record
                 WHERE generation_id = ?1 AND scope_kind = 'provider' AND scope_key LIKE 'rust-analyzer@%' AND status = 'complete'",
                params![replay.candidate_generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reused_coverage, 1);
        let replay_resolutions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relationship_resolution WHERE generation_id = ?1",
                params![replay.candidate_generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(replay_resolutions > 0);
    }

    /// Phase A end-to-end proof: baseline generation → semantic reconcile →
    /// relationship resolution → active generation, using the REAL
    /// `scip-typescript@0.4.0` binary (not mocked), then a cross-file
    /// target-universe change proving resolution reruns (INV-007) and the
    /// changed result is visible in the newly-activated generation.
    #[test]
    fn production_semantic_reconcile_resolves_cross_file_reference_end_to_end() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("tsconfig.json"),
            "{\"compilerOptions\":{\"target\":\"es2020\",\"module\":\"commonjs\"}}",
        );
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): string {\n  return \"hi\";\n}\n",
        );

        let cfg = semantic_config("e2e-cross-file", false);
        let ws = register_workspace(
            &conn,
            dir.path(),
            &cfg,
            &dir.path().join(".atlas/db.sqlite"),
            "p1",
        )
        .unwrap();

        // Generation 1: only `a.ts` exists, defining `helper` with no
        // cross-file reference yet.
        let report1 = crate::discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(
            report1.activation, "committed",
            "gen1 must activate: {:?}",
            report1.failure_message
        );
        assert!(
            report1.semantic_executions_run >= 1,
            "the real scip-typescript execution must actually run: {report1:?}"
        );
        assert!(
            report1.semantic_symbols_persisted > 0,
            "helper() must be persisted as a semantic symbol"
        );

        let helper_key: Option<String> = conn
            .query_row(
                "SELECT canonical_symbol_key FROM symbol_fact WHERE evidence_method='semantic' AND canonical_symbol_key LIKE '%helper%' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional_ok();
        let helper_key = helper_key.expect("helper() must be indexed as a semantic symbol in gen1");

        // No relationship yet should resolve to this exact symbol as a
        // cross-file reference (only b.ts, added next, will produce one).
        let resolved_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relationship_resolution WHERE resolved_ref_value = ?1 AND status='resolved_symbol'",
                params![helper_key],
                |r| r.get(0),
            )
            .unwrap();

        // Generation 2: add `b.ts`, which imports and calls `helper` from
        // `a.ts` -- the target universe now includes a real cross-file
        // reference to an existing symbol.
        write(&dir.path().join("src/b.ts"), "import { helper } from \"./a\";\nexport function use(): string {\n  return helper();\n}\n");
        let ws2 = crate::workspace::load_workspace_by_root(&conn, dir.path())
            .unwrap()
            .unwrap();
        let report2 = crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        assert_eq!(
            report2.activation, "committed",
            "gen2 must activate: {:?}",
            report2.failure_message
        );
        assert!(
            report2.semantic_resolutions_persisted > 0,
            "gen2 must run relationship resolution: {report2:?}"
        );

        let resolved_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relationship_resolution rr
                 JOIN workspace w ON w.workspace_id = rr.workspace_id AND w.active_generation_id = rr.generation_id
                 WHERE rr.resolved_ref_value = ?1 AND rr.status = 'resolved_symbol'",
                params![helper_key],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            resolved_after > resolved_before,
            "adding a real cross-file reference to an existing symbol must produce a NEW resolved_symbol resolution in the newly-active generation (before={resolved_before}, after={resolved_after})"
        );

        let active_generation: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id=?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active_generation.as_deref(),
            Some(report2.candidate_generation_id.as_str())
        );
    }

    /// Required-provider failure must fail the candidate generation and
    /// leave the previous active generation untouched (RUN-014, INV-009,
    /// RISK S-003) -- proven with a REAL failing spawn (nonexistent
    /// executable), not a mocked outcome.
    #[test]
    fn required_semantic_provider_failure_leaves_previous_generation_active() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): string { return \"hi\"; }\n",
        );

        // Gen1: structural-only baseline (no semantic provider configured),
        // establishing an active generation to protect.
        let baseline_cfg = structural_only_config("e2e-required-failure");
        let ws = register_workspace(
            &conn,
            dir.path(),
            &baseline_cfg,
            &dir.path().join(".atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let report1 = crate::discovery::reconcile(&ws, &conn, &baseline_cfg).unwrap();
        assert_eq!(report1.activation, "committed");
        let gen1_id = report1.candidate_generation_id.clone();

        // Gen2: a required semantic provider pointed at a nonexistent
        // executable -- must fail the candidate outright.
        let mut broken_toml = String::from(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"e2e-required-failure\"\n\
             [[providers]]\nname = \"scip-typescript\"\nkind = \"external_scip\"\ntier = \"semantic_index\"\n\
             scope = \"project\"\nenabled = true\nrequired = true\npriority = 800\n\
             languages = [\"typescript\"]\nproject_markers = [\"tsconfig.json\"]\n\
             command = \"this-executable-definitely-does-not-exist-98765\"\n\
             arguments = [\"index\", \"--output\", \"{output_file}\"]\n\
             probe_arguments = [\"--version\"]\ntimeout_ms = 5000\nmax_output_bytes = 1048576\n",
        );
        let broken_cfg = Config::parse(&broken_toml).unwrap();
        broken_toml.clear();

        let ws_reloaded = crate::workspace::load_workspace_by_root(&conn, dir.path())
            .unwrap()
            .unwrap();
        let report2 = crate::discovery::reconcile(&ws_reloaded, &conn, &broken_cfg).unwrap();
        assert_eq!(
            report2.activation, "failed",
            "a required provider pointed at a nonexistent executable must fail the candidate"
        );
        assert_eq!(
            report2.failure_code.as_deref(),
            Some("E_SEMANTIC_REQUIRED_PROVIDER")
        );

        let active_generation: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id=?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active_generation.as_deref(),
            Some(gen1_id.as_str()),
            "the previous committed generation must remain active"
        );

        let gen2_state: String = conn
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id=?1",
                params![report2.candidate_generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gen2_state, "failed");
    }

    /// Optional-provider failure must degrade coverage but still activate
    /// the candidate (ADR-032).
    #[test]
    fn optional_semantic_provider_failure_still_activates_with_degraded_coverage() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): string { return \"hi\"; }\n",
        );

        let toml = "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"e2e-optional-failure\"\n\
             [[providers]]\nname = \"scip-typescript\"\nkind = \"external_scip\"\ntier = \"semantic_index\"\n\
             scope = \"project\"\nenabled = true\nrequired = false\npriority = 800\n\
             languages = [\"typescript\"]\nproject_markers = [\"tsconfig.json\"]\n\
             command = \"this-executable-definitely-does-not-exist-98765\"\n\
             arguments = [\"index\", \"--output\", \"{output_file}\"]\n\
             probe_arguments = [\"--version\"]\ntimeout_ms = 5000\nmax_output_bytes = 1048576\n";
        let cfg = Config::parse(toml).unwrap();
        let ws = register_workspace(
            &conn,
            dir.path(),
            &cfg,
            &dir.path().join(".atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let report = crate::discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(
            report.activation, "committed",
            "an optional provider's failure must not block activation"
        );
        assert!(
            report.semantic_degraded,
            "coverage must be reported as degraded when the optional provider failed"
        );
        let unavailable_coverage: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM coverage_record
                 WHERE generation_id = ?1 AND scope_kind = 'provider' AND status = 'excluded'",
                params![report.candidate_generation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            unavailable_coverage, 1,
            "an unavailable optional provider must retain explicit excluded coverage"
        );
    }

    #[test]
    fn unchanged_project_reuses_the_prior_semantic_execution() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): string { return \"hi\"; }\n",
        );
        let cfg = semantic_config("e2e-reuse", false);
        let ws = register_workspace(
            &conn,
            dir.path(),
            &cfg,
            &dir.path().join(".atlas/db.sqlite"),
            "p1",
        )
        .unwrap();

        let report1 = crate::discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(report1.activation, "committed");
        assert!(report1.semantic_executions_run >= 1);

        // Touch an unrelated file so the structural reconcile has *some*
        // change to process, but the semantic project's inputs are
        // byte-identical -- the semantic execution must be reused, not rerun.
        write(&dir.path().join("README.md"), "unrelated change\n");
        let ws2 = crate::workspace::load_workspace_by_root(&conn, dir.path())
            .unwrap()
            .unwrap();
        let report2 = crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        assert_eq!(report2.activation, "committed");
        assert_eq!(
            report2.semantic_executions_run, 0,
            "an unchanged project input must reuse the prior execution, not rerun"
        );
        assert!(report2.semantic_executions_reused >= 1);
    }

    /// SEC-002: raw provider output must be deleted by default. This
    /// exercises the exact `cleanup_temp_dir` helper `run_one_plan` calls on
    /// every terminal outcome (spawn-failure, output-rejected, decode-failed,
    /// and success paths all call it -- see `run_one_plan`; the two internal
    /// `?`-propagation sites for `spawn_and_wait`/`std::fs::read` explicitly
    /// clean up before re-raising too, so no terminal outcome can leak the
    /// directory).
    #[test]
    fn cleanup_temp_dir_removes_directory_and_its_contents() {
        let temp_root = tempfile::tempdir().unwrap();
        let output_dir = crate::provider_runtime::make_execution_output_dir(
            temp_root.path(),
            "pex_test_cleanup",
        )
        .unwrap();
        std::fs::write(output_dir.join("index.scip"), b"raw scip protobuf bytes").unwrap();
        assert!(
            output_dir.exists(),
            "fixture setup must have created the directory"
        );
        cleanup_temp_dir(&output_dir);
        assert!(
            !output_dir.exists(),
            "SEC-002: raw provider output directory must be deleted by default"
        );
    }
}
