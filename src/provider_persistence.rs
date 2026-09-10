//! Persistence + reuse/invalidation orchestration for V1.1 provider
//! executions (S5 — `PERSIST-001`..`007`, `INV-001`..`009`).
//!
//! Writes to the tables added by migration
//! `migrations/0002_semantic_provider_foundation.sql`: `provider_descriptor`,
//! `workspace_provider`, `project_scope`, `provider_execution`,
//! `provider_execution_file`, `provider_execution_run`,
//! `provider_runtime_diagnostic`, and `provider_invalidation_event`. The
//! migration is additive; this module never rewrites foundation tables.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;
use crate::migrations::iso8601_now;
use crate::provider_contract::{NetworkIsolationState, ProviderDescriptor, ProviderOutcome};

// ---------------------------------------------------------------------------
// Descriptor + workspace policy (PERSIST-001)
// ---------------------------------------------------------------------------

/// Register a provider descriptor. Idempotent: the row's primary key is the
/// descriptor's own `provider_key` (name + version + fingerprint prefix), so
/// re-registering an unchanged descriptor is a no-op and a changed
/// descriptor (any field, since that changes the fingerprint) always gets a
/// fresh row rather than silently overwriting history.
pub fn register_provider_descriptor(
    conn: &Connection,
    descriptor: &ProviderDescriptor,
) -> Result<String> {
    let provider_key = descriptor.provider_key();
    let capabilities_json = serde_json::to_string(&descriptor.capabilities)?;
    let limitations_json = serde_json::to_string(&descriptor.limitations)?;
    let executable_identity = descriptor.executable.as_ref().map(|e| e.command.clone());
    conn.execute(
        "INSERT OR IGNORE INTO provider_descriptor (
            provider_key, provider_name, provider_version, provider_tier,
            execution_kind, execution_scope, protocol_version, output_format,
            executable_identity, executable_hash, deterministic,
            capabilities_json, limitations_json, descriptor_hash, registered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            provider_key,
            descriptor.name,
            descriptor.version,
            descriptor.tier.as_str(),
            execution_kind_str(descriptor.execution_kind),
            execution_scope_str(descriptor.execution_scope),
            descriptor.protocol_version,
            output_format_str(descriptor.output_format),
            executable_identity,
            Option::<String>::None, // caller-supplied executable_hash lives on the descriptor's fingerprint input, not duplicated here
            descriptor.deterministic as i64,
            capabilities_json,
            limitations_json,
            descriptor.fingerprint,
            iso8601_now(),
        ],
    )?;
    Ok(provider_key)
}

fn execution_kind_str(k: crate::provider_contract::ExecutionKind) -> &'static str {
    use crate::provider_contract::ExecutionKind::*;
    match k {
        Builtin => "builtin",
        ExternalProcess => "external_process",
    }
}
fn execution_scope_str(s: crate::provider_contract::ExecutionScopeKind) -> &'static str {
    use crate::provider_contract::ExecutionScopeKind::*;
    match s {
        File => "file",
        Project => "project",
        Package => "package",
        Workspace => "workspace",
    }
}
fn output_format_str(o: crate::provider_contract::OutputFormat) -> &'static str {
    use crate::provider_contract::OutputFormat::*;
    match o {
        NormalizedJson => "normalized_json",
        Scip => "scip",
        Builtin => "builtin",
    }
}
fn network_isolation_state_str(n: NetworkIsolationState) -> &'static str {
    use NetworkIsolationState::*;
    match n {
        Enforced => "enforced",
        BestEffort => "best_effort",
        NotAvailable => "not_available",
        NotApplicable => "not_applicable",
    }
}
fn outcome_str(o: ProviderOutcome) -> &'static str {
    use ProviderOutcome::*;
    match o {
        Planned => "planned",
        Running => "running",
        Complete => "complete",
        Partial => "partial",
        Unsupported => "unsupported",
        Unavailable => "unavailable",
        Failed => "failed",
        Cancelled => "cancelled",
        TimedOut => "timed_out",
        OutputRejected => "output_rejected",
        DecodeFailed => "decode_failed",
        MappingFailed => "mapping_failed",
        Stale => "stale",
    }
}

/// Per-workspace provider policy row (`workspace_provider`). Upserted on
/// every config load — this is how `Config::providers`/`provider_runtime`
/// entries become the DB-side activation policy the S5 orchestrator reads.
#[allow(clippy::too_many_arguments)]
pub fn set_workspace_provider_policy(
    conn: &Connection,
    workspace_id: &str,
    provider_key: &str,
    enabled: bool,
    required: bool,
    priority: i64,
    configuration_json: &str,
    configuration_hash: &str,
    timeout_ms: u64,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    max_output_bytes: u64,
    network_isolation_policy: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO workspace_provider (
            workspace_id, provider_key, enabled, required, priority,
            configuration_json, configuration_hash, timeout_ms,
            max_stdout_bytes, max_stderr_bytes, max_output_bytes,
            network_isolation_policy, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(workspace_id, provider_key) DO UPDATE SET
            enabled = excluded.enabled,
            required = excluded.required,
            priority = excluded.priority,
            configuration_json = excluded.configuration_json,
            configuration_hash = excluded.configuration_hash,
            timeout_ms = excluded.timeout_ms,
            max_stdout_bytes = excluded.max_stdout_bytes,
            max_stderr_bytes = excluded.max_stderr_bytes,
            max_output_bytes = excluded.max_output_bytes,
            network_isolation_policy = excluded.network_isolation_policy,
            updated_at = excluded.updated_at",
        params![
            workspace_id,
            provider_key,
            enabled as i64,
            required as i64,
            priority,
            configuration_json,
            configuration_hash,
            timeout_ms as i64,
            max_stdout_bytes as i64,
            max_stderr_bytes as i64,
            max_output_bytes as i64,
            network_isolation_policy,
            iso8601_now(),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Project scopes (PERSIST-002)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn record_project_scope(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    scope: &crate::project_scope::ProjectScope,
) -> Result<String> {
    let input_manifest_hash = crate::project_scope::project_input_manifest_hash(scope);
    let canonical_root = scope.canonical_root.to_string_lossy().into_owned();
    let project_scope_id = deterministic_id(
        "psc",
        &[
            workspace_id,
            generation_id,
            scope.scope_kind.as_str(),
            &canonical_root,
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO project_scope (
            project_scope_id, workspace_id, generation_id, scope_kind, canonical_root,
            primary_manifest_path, primary_manifest_hash, language_family,
            owning_policy_version, input_manifest_hash, status, details_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', '{}', ?11)",
        params![
            project_scope_id,
            workspace_id,
            generation_id,
            scope.scope_kind.as_str(),
            canonical_root,
            scope.primary_manifest_path.to_string_lossy().into_owned(),
            scope.primary_manifest_hash,
            scope.language_family,
            scope.owning_policy_version,
            input_manifest_hash,
            iso8601_now(),
        ],
    )?;
    Ok(project_scope_id)
}

fn deterministic_id(prefix: &str, parts: &[&str]) -> String {
    use blake3::Hasher;
    let mut h = Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(&[0x1f]);
    }
    let hex = h.finalize().to_hex();
    format!("{prefix}_{}", &hex.as_str()[..32])
}

// ---------------------------------------------------------------------------
// Coverage (PERSIST-006)
// ---------------------------------------------------------------------------

/// Map a terminal `ProviderOutcome` to `coverage_record.status`'s bounded
/// `CHECK` vocabulary (`complete`/`partial`/`unsupported`/`excluded`/
/// `failed`/`stale`). `Unavailable` and `Unsupported` mean the provider
/// could not even attempt this scope (`excluded`, not `failed` — a failed
/// attempt implies the provider *tried* and did not complete).
pub fn coverage_status_for_outcome(outcome: ProviderOutcome) -> &'static str {
    use ProviderOutcome::*;
    match outcome {
        Complete => "complete",
        Partial => "partial",
        Unsupported | Unavailable => "excluded",
        Stale => "stale",
        Failed | Cancelled | TimedOut | OutputRejected | DecodeFailed | MappingFailed => "failed",
        Planned | Running => "partial", // defensive: never persisted at a non-terminal outcome in practice
    }
}

/// Persist one typed, aggregate coverage record for a `(generation, scope)`
/// pair (`coverage_record`, migration `0001`). `scope_kind = 'provider'`
/// records one project-scoped semantic provider execution's coverage;
/// callers may also record `'capability'`-scoped rows for finer-grained
/// claims (e.g. "resolution" coverage) without inventing a new table.
#[allow(clippy::too_many_arguments)]
pub fn record_coverage(
    conn: &Connection,
    generation_id: &str,
    file_id: Option<&str>,
    scope_kind: &str,
    scope_key: &str,
    status: &str,
    capabilities_json: &str,
    limitations_json: &str,
    details_json: &str,
) -> Result<String> {
    let id = deterministic_id("cov", &[generation_id, scope_kind, scope_key]);
    conn.execute(
        "INSERT OR IGNORE INTO coverage_record (
            coverage_id, generation_id, file_id, scope_kind, scope_key, status,
            capabilities_json, limitations_json, details_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            generation_id,
            file_id,
            scope_kind,
            scope_key,
            status,
            capabilities_json,
            limitations_json,
            details_json
        ],
    )?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Reuse decision (INV-001, INV-002..007)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusableExecution {
    pub provider_execution_id: String,
    pub status: String,
}

/// INV-001: reuse is permitted only on an *exact* match of every identity
/// component baked into `input_fingerprint`
/// (`project_scope::execution_input_fingerprint`) plus the schema/mapping
/// policy versions recorded alongside it. Any one of those changing
/// (provider version, executable hash, config, protocol, normalized schema,
/// mapping policy — INV-002..INV-005) changes the fingerprint and therefore
/// naturally produces a cache miss here; the caller is then expected to
/// `record_invalidation_event` and run a fresh execution.
#[allow(clippy::too_many_arguments)]
pub fn find_reusable_execution(
    conn: &Connection,
    workspace_id: &str,
    provider_key: &str,
    scope_kind: &str,
    scope_key: &str,
    input_fingerprint: &str,
    normalized_schema_version: &str,
    mapping_policy_version: &str,
) -> Result<Option<ReusableExecution>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT provider_execution_id, status FROM provider_execution
             WHERE workspace_id = ?1 AND provider_key = ?2 AND scope_kind = ?3 AND scope_key = ?4
               AND input_fingerprint = ?5 AND normalized_schema_version = ?6 AND mapping_policy_version = ?7
               AND status IN ('complete', 'partial')
             ORDER BY created_at DESC LIMIT 1",
            params![workspace_id, provider_key, scope_kind, scope_key, input_fingerprint, normalized_schema_version, mapping_policy_version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(
        row.map(|(provider_execution_id, status)| ReusableExecution {
            provider_execution_id,
            status,
        }),
    )
}

/// Record why a prior execution is being superseded rather than reused.
/// Dependency changes are explicit, auditable invalidation events rather than
/// silent runtime substitutions.
#[allow(clippy::too_many_arguments)]
pub fn record_invalidation_event(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    provider_key: &str,
    scope_kind: &str,
    scope_key: &str,
    prior_execution_id: Option<&str>,
    reason_code: InvalidationReason,
    old_fingerprint: Option<&str>,
    new_fingerprint: &str,
) -> Result<String> {
    let id = deterministic_id(
        "piv",
        &[
            workspace_id,
            generation_id,
            provider_key,
            scope_key,
            new_fingerprint,
            reason_code.as_str(),
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO provider_invalidation_event (
            provider_invalidation_event_id, workspace_id, generation_id, provider_key,
            scope_kind, scope_key, prior_execution_id, reason_code, old_fingerprint,
            new_fingerprint, details_json, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '{}', ?11)",
        params![
            id,
            workspace_id,
            generation_id,
            provider_key,
            scope_kind,
            scope_key,
            prior_execution_id,
            reason_code.as_str(),
            old_fingerprint,
            new_fingerprint,
            iso8601_now(),
        ],
    )?;
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    SourceChanged,
    ProjectInputsChanged,
    ProviderVersionChanged,
    ExecutableChanged,
    ProviderConfigChanged,
    ProtocolChanged,
    NormalizedSchemaChanged,
    MappingPolicyChanged,
    ResolutionPolicyChanged,
    ManualRebuild,
    PriorOutputMissing,
    PriorOutputIncompatible,
}

impl InvalidationReason {
    pub fn as_str(&self) -> &'static str {
        use InvalidationReason::*;
        match self {
            SourceChanged => "source_changed",
            ProjectInputsChanged => "project_inputs_changed",
            ProviderVersionChanged => "provider_version_changed",
            ExecutableChanged => "executable_changed",
            ProviderConfigChanged => "provider_config_changed",
            ProtocolChanged => "protocol_changed",
            NormalizedSchemaChanged => "normalized_schema_changed",
            MappingPolicyChanged => "mapping_policy_changed",
            ResolutionPolicyChanged => "resolution_policy_changed",
            ManualRebuild => "manual_rebuild",
            PriorOutputMissing => "prior_output_missing",
            PriorOutputIncompatible => "prior_output_incompatible",
        }
    }
}

// ---------------------------------------------------------------------------
// Execution persistence (PERSIST-003, PERSIST-004, PERSIST-006, OBS-001)
// ---------------------------------------------------------------------------

pub struct ExecutionTelemetry {
    pub status: ProviderOutcome,
    pub network_isolation_state: NetworkIsolationState,
    pub started_at: String,
    pub finished_at: String,
    pub exit_code: Option<i64>,
    pub stdout_hash: Option<String>,
    pub stderr_hash: Option<String>,
    pub stdout_bytes: i64,
    pub stderr_bytes: i64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub output_hash: Option<String>,
    pub output_bytes: Option<i64>,
}

/// Persist one provider execution row. Enforces the same identity the DB's
/// own `UNIQUE` constraint enforces — `INSERT OR IGNORE` means a duplicate
/// (byte-identical fingerprint/schema/mapping tuple) is silently a no-op,
/// and the caller should have already checked `find_reusable_execution`
/// first if reuse (not a fresh execution) was intended.
#[allow(clippy::too_many_arguments)]
pub fn record_provider_execution(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    provider_key: &str,
    project_scope_id: Option<&str>,
    scope_kind: &str,
    scope_key: &str,
    input_fingerprint: &str,
    provider_set_hash: &str,
    normalized_schema_version: &str,
    mapping_policy_version: &str,
    telemetry: &ExecutionTelemetry,
) -> Result<String> {
    let provider_execution_id = deterministic_id(
        "pex",
        &[
            workspace_id,
            provider_key,
            scope_kind,
            scope_key,
            input_fingerprint,
            normalized_schema_version,
            mapping_policy_version,
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO provider_execution (
            provider_execution_id, workspace_id, generation_id, provider_key, project_scope_id,
            scope_kind, scope_key, input_fingerprint, provider_set_hash, normalized_schema_version,
            mapping_policy_version, status, network_isolation_state, started_at, finished_at,
            exit_code, stdout_hash, stderr_hash, stdout_bytes, stderr_bytes, stdout_truncated,
            stderr_truncated, output_hash, output_bytes, diagnostic_summary_json, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,'[]',?25)",
        params![
            provider_execution_id,
            workspace_id,
            generation_id,
            provider_key,
            project_scope_id,
            scope_kind,
            scope_key,
            input_fingerprint,
            provider_set_hash,
            normalized_schema_version,
            mapping_policy_version,
            outcome_str(telemetry.status),
            network_isolation_state_str(telemetry.network_isolation_state),
            telemetry.started_at,
            telemetry.finished_at,
            telemetry.exit_code,
            telemetry.stdout_hash,
            telemetry.stderr_hash,
            telemetry.stdout_bytes,
            telemetry.stderr_bytes,
            telemetry.stdout_truncated as i64,
            telemetry.stderr_truncated as i64,
            telemetry.output_hash,
            telemetry.output_bytes,
            iso8601_now(),
        ],
    )?;
    Ok(provider_execution_id)
}

/// Link one document's outcome to an execution (`provider_execution_file`,
/// PERSIST-004). `reused_extractor_run_id` is set when this document's
/// normalized digest matched a prior run (S5 "reuse unchanged normalized
/// document batches" — INV-006) rather than being freshly persisted.
#[allow(clippy::too_many_arguments)]
pub fn record_execution_document(
    conn: &Connection,
    provider_execution_id: &str,
    provider_document_path: &str,
    canonical_path: Option<&str>,
    document_status: &str,
    raw_document_hash: Option<&str>,
    normalized_batch_hash: Option<&str>,
    reused_extractor_run_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO provider_execution_file (
            provider_execution_id, file_id, revision_id, provider_document_path, canonical_path,
            document_status, raw_document_hash, normalized_batch_hash, reused_extractor_run_id, details_json
         ) VALUES (?1, NULL, NULL, ?2, ?3, ?4, ?5, ?6, ?7, '{}')",
        params![
            provider_execution_id,
            provider_document_path,
            canonical_path,
            document_status,
            raw_document_hash,
            normalized_batch_hash,
            reused_extractor_run_id,
        ],
    )?;
    Ok(())
}

/// INV-006: a document's per-execution digest
/// (`scip_mapping::document_normalized_digest`) is what actually gates
/// whether Atlas persists a fresh extractor run for it — not merely whether
/// the *provider execution* as a whole reused its fingerprint. Two
/// project-scoped executions can share nothing at the execution level yet
/// still have 999 of 1000 documents digest-identical; those 999 must reuse.
pub fn decide_document_reuse(
    previous_normalized_digest: Option<&str>,
    current_normalized_digest: &str,
) -> bool {
    previous_normalized_digest == Some(current_normalized_digest)
}

/// Look up the most recent normalized digest persisted for `canonical_path`
/// under this provider, across *any* prior execution for the workspace —
/// the comparison baseline for `decide_document_reuse`.
pub fn previous_document_digest(
    conn: &Connection,
    workspace_id: &str,
    provider_key: &str,
    canonical_path: &str,
) -> Result<Option<String>> {
    let digest: Option<String> = conn
        .query_row(
            "SELECT pef.normalized_batch_hash
             FROM provider_execution_file pef
             JOIN provider_execution pe ON pe.provider_execution_id = pef.provider_execution_id
             WHERE pe.workspace_id = ?1 AND pe.provider_key = ?2 AND pef.canonical_path = ?3
               AND pef.normalized_batch_hash IS NOT NULL
             ORDER BY pe.created_at DESC LIMIT 1",
            params![workspace_id, provider_key, canonical_path],
            |r| r.get(0),
        )
        .optional()?;
    Ok(digest)
}

/// Link an execution to a persisted `extractor_run` (V1's per-revision fact
/// table) — PERSIST-004 "link execution to files and extractor runs".
pub fn link_execution_to_extractor_run(
    conn: &Connection,
    provider_execution_id: &str,
    extractor_run_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO provider_execution_run (provider_execution_id, extractor_run_id) VALUES (?1, ?2)",
        params![provider_execution_id, extractor_run_id],
    )?;
    Ok(())
}

/// Find or create the `extractor_run` row for one (revision, semantic
/// provider) pair, matching the table's own
/// `UNIQUE(revision_id, provider_name, provider_version, configuration_hash,
/// normalized_schema_version)` identity — the same reuse key V1's
/// structural providers already use (`discovery::persist_provider_batch`).
#[allow(clippy::too_many_arguments)]
pub fn ensure_semantic_extractor_run(
    conn: &Connection,
    revision_id: &str,
    provider_name: &str,
    provider_version: &str,
    configuration_hash: &str,
    normalized_schema_version: &str,
    status: &str, // 'complete' | 'partial'
    bytes_total: i64,
) -> Result<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT extractor_run_id FROM extractor_run
             WHERE revision_id = ?1 AND provider_name = ?2 AND provider_version = ?3
               AND configuration_hash = ?4 AND normalized_schema_version = ?5",
            params![
                revision_id,
                provider_name,
                provider_version,
                configuration_hash,
                normalized_schema_version
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = deterministic_id(
        "run",
        &[
            revision_id,
            provider_name,
            provider_version,
            configuration_hash,
            normalized_schema_version,
        ],
    );
    let now = iso8601_now();
    conn.execute(
        "INSERT INTO extractor_run (
            extractor_run_id, revision_id, provider_name, provider_version, provider_tier,
            configuration_hash, normalized_schema_version, deterministic, status,
            started_at, finished_at, bytes_total, bytes_processed, capabilities_json, limitations_json
         ) VALUES (?1, ?2, ?3, ?4, 'semantic_index', ?5, ?6, 1, ?7, ?8, ?8, ?9, ?9, '[]', '[]')",
        params![id, revision_id, provider_name, provider_version, configuration_hash, normalized_schema_version, status, now, bytes_total],
    )?;
    Ok(id)
}

/// Convert a SCIP position to a UTF-8 byte offset using the enclosing
/// document's declared position encoding. Unspecified encodings fail closed.
fn scip_position_to_byte_offset(
    text: &str,
    line: i64,
    character: i64,
    encoding: crate::scip_decoder::PositionEncoding,
) -> Option<usize> {
    if line < 0 || character < 0 {
        return None;
    }
    let mut offset = 0usize;
    for (idx, line_with_ending) in text.split_inclusive('\n').enumerate() {
        if idx as i64 == line {
            let line_text = line_with_ending.trim_end_matches(['\n', '\r']);
            return byte_offset_for_units(line_text, character as usize, encoding)
                .map(|column| offset + column);
        }
        offset += line_with_ending.len();
    }
    if line as usize == text.lines().count() && text.ends_with('\n') && character == 0 {
        return Some(text.len());
    }
    None
}

fn byte_offset_for_units(
    text: &str,
    target: usize,
    encoding: crate::scip_decoder::PositionEncoding,
) -> Option<usize> {
    use crate::scip_decoder::PositionEncoding;
    if encoding == PositionEncoding::Utf8 {
        return (target <= text.len() && text.is_char_boundary(target)).then_some(target);
    }
    let mut units = 0usize;
    for (byte, ch) in text.char_indices() {
        if units == target {
            return Some(byte);
        }
        units += match encoding {
            PositionEncoding::Utf16 => ch.len_utf16(),
            PositionEncoding::Utf32 => 1,
            PositionEncoding::Unspecified
            | PositionEncoding::Unknown(_)
            | PositionEncoding::Utf8 => return None,
        };
        if units > target {
            return None;
        }
    }
    (units == target).then_some(text.len())
}

/// Persist one `MappedSymbol` into V1's `symbol_fact` table
/// (`evidence_method = 'semantic'`), converting its SCIP line/column span to
/// a byte offset via `document_text` (the current live file content for
/// that revision — required because `symbol_fact.start_byte`/`end_byte` are
/// `NOT NULL`, unlike `relationship_fact`'s nullable span).
pub fn record_semantic_symbol_fact(
    conn: &Connection,
    extractor_run_id: &str,
    revision_id: &str,
    document_text: &str,
    position_encoding: crate::scip_decoder::PositionEncoding,
    sym: &crate::scip_mapping::MappedSymbol,
) -> Result<Option<String>> {
    let start_byte = scip_position_to_byte_offset(
        document_text,
        sym.start_line,
        sym.start_column,
        position_encoding,
    );
    let end_byte = scip_position_to_byte_offset(
        document_text,
        sym.end_line,
        sym.end_column,
        position_encoding,
    );
    let (start_byte, end_byte) = match (start_byte, end_byte) {
        (Some(s), Some(e)) if e >= s => (s as i64, e as i64),
        _ => return Ok(None), // span outside the current text; skip rather than fabricate
    };
    let display_name = display_name_from_provider_symbol(&sym.provider_symbol_id);
    let id = deterministic_id(
        "sym",
        &[
            extractor_run_id,
            &sym.canonical_symbol_key,
            &start_byte.to_string(),
            &end_byte.to_string(),
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO symbol_fact (
            symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
            provider_symbol_id, symbol_kind, display_name, qualified_name, signature,
            visibility, start_byte, end_byte, start_line, start_column, end_line, end_column,
            documentation, attributes_json, evidence_method, confidence, evidence_reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'semantic_symbol', ?6, ?6, NULL, NULL, ?7, ?8, ?9, ?10, ?11, ?12, NULL, '{}', 'semantic', ?13, 'scip_definition_occurrence')",
        params![
            id,
            extractor_run_id,
            revision_id,
            sym.canonical_symbol_key,
            sym.provider_symbol_id,
            display_name,
            start_byte,
            end_byte,
            sym.start_line + 1, // symbol_fact.start_line CHECK(>= 1); SCIP is 0-based
            sym.start_column + 1,
            sym.end_line + 1,
            sym.end_column + 1,
            sym.confidence,
        ],
    )?;
    Ok(Some(id))
}

/// Best-effort display name from a raw SCIP symbol string — the last
/// descriptor segment, matching `resolution::extract_display_name`'s
/// heuristic. Used only for human-readable display, never for resolution.
fn display_name_from_provider_symbol(scip_symbol: &str) -> String {
    let trimmed = scip_symbol.trim_end_matches(['.', '#', '(', ')', ':', '!']);
    trimmed
        .rsplit(['/', '#', '.', ' '])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(scip_symbol)
        .to_string()
}

/// Persist one `MappedRelationship` (semantic, SCIP-derived evidence) into
/// V1's `relationship_fact` table — PERSIST-005: this is the concrete glue
/// connecting the SCIP mapper's output to the same fact table every other
/// provider writes into, tagged `evidence_method = 'semantic'`.
///
/// SCIP's own relationship vocabulary (`IMPORTS`/`REFERENCES`/`CALLS`/
/// `IMPLEMENTS`) maps directly onto `relationship_fact`'s `CHECK`ed
/// vocabulary; `TYPE_DEFINITION`/`DEFINED_BY` (which the V1 schema has no
/// dedicated slot for) are stored as `'other'` with the original SCIP kind
/// preserved in `attributes_json` so no information is silently dropped.
pub fn record_semantic_relationship_fact(
    conn: &Connection,
    extractor_run_id: &str,
    revision_id: &str,
    rel: &crate::scip_mapping::MappedRelationship,
) -> Result<String> {
    let (db_relationship_type, attributes_json): (&str, String) = match rel.relationship_type {
        "IMPORTS" => ("imports", "{}".to_string()),
        "REFERENCES" => ("references", "{}".to_string()),
        "CALLS" => ("calls", "{}".to_string()),
        "IMPLEMENTS" => ("implements", "{}".to_string()),
        other => (
            "other",
            serde_json::json!({"scip_relationship_kind": other}).to_string(),
        ),
    };
    let (source_ref_kind, source_ref_value): (&str, String) = if matches!(
        rel.relationship_type,
        "IMPLEMENTS" | "TYPE_DEFINITION" | "DEFINED_BY"
    ) {
        ("symbol", rel.provider_symbol_id.clone())
    } else {
        ("path", rel.source_canonical_document_path.clone())
    };
    let target_ref_kind = match rel.target_ref_kind {
        crate::scip_mapping::RefKind::Symbol => "symbol",
        crate::scip_mapping::RefKind::ExternalSymbol => "external",
    };

    let id = deterministic_id(
        "rel",
        &[
            extractor_run_id,
            db_relationship_type,
            &source_ref_value,
            &rel.target_ref_value,
            &rel.source_start_line.to_string(),
            &rel.source_start_column.to_string(),
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO relationship_fact (
            relationship_fact_id, extractor_run_id, revision_id, relationship_type,
            source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
            start_byte, end_byte, attributes_json, evidence_method, confidence, evidence_reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, 'semantic', ?10, ?11)",
        params![
            id,
            extractor_run_id,
            revision_id,
            db_relationship_type,
            source_ref_kind,
            source_ref_value,
            target_ref_kind,
            rel.target_ref_value,
            attributes_json,
            rel.confidence,
            rel.reason_code,
        ],
    )?;
    Ok(id)
}

pub fn record_runtime_diagnostic(
    conn: &Connection,
    provider_execution_id: &str,
    severity: &str,
    code: &str,
    message: &str,
    provider_document_path: Option<&str>,
) -> Result<()> {
    let id = deterministic_id("prd", &[provider_execution_id, code, message]);
    conn.execute(
        "INSERT OR IGNORE INTO provider_runtime_diagnostic (
            provider_runtime_diagnostic_id, provider_execution_id, severity, code, message,
            provider_document_path, details_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', ?7)",
        params![
            id,
            provider_execution_id,
            severity,
            code,
            message,
            provider_document_path,
            iso8601_now()
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Required-vs-optional activation policy (INV-008, INV-009, RUN-014)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationDecision {
    /// Proceed with candidate activation. `degraded` is `true` when an
    /// optional provider failed and coverage is therefore incomplete —
    /// still a valid candidate, just one that must report degraded coverage
    /// rather than silently claiming completeness.
    Proceed { degraded: bool },
    /// A required provider failed or is unavailable: the candidate MUST NOT
    /// activate. The caller is expected to call
    /// `generation::fail_candidate`, which — by construction — never touches
    /// `workspace.active_generation_id` (INV-005), leaving the previous
    /// generation authoritative.
    Block { reason: String },
}

/// INV-008/INV-009: decide whether one provider's outcome blocks candidate
/// activation. Terminal-failure-shaped outcomes
/// (`Failed`/`Unavailable`/`TimedOut`/`Cancelled`/`OutputRejected`/
/// `DecodeFailed`/`MappingFailed`) on a `required` provider block; the same
/// outcomes on an optional provider degrade coverage instead.
/// `Complete`/`Partial` never block regardless of `required`.
pub fn decide_activation(required: bool, outcome: ProviderOutcome) -> ActivationDecision {
    let is_failure = matches!(
        outcome,
        ProviderOutcome::Failed
            | ProviderOutcome::Unavailable
            | ProviderOutcome::TimedOut
            | ProviderOutcome::Cancelled
            | ProviderOutcome::OutputRejected
            | ProviderOutcome::DecodeFailed
            | ProviderOutcome::MappingFailed
            | ProviderOutcome::Unsupported
    );
    if !is_failure {
        return ActivationDecision::Proceed {
            degraded: matches!(outcome, ProviderOutcome::Partial),
        };
    }
    if required {
        ActivationDecision::Block {
            reason: format!(
                "required provider outcome {:?} blocks candidate activation",
                outcome
            ),
        }
    } else {
        ActivationDecision::Proceed { degraded: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::generation::{begin_candidate, TriggerKind};
    use crate::providers::builtin_provider_descriptors;
    use crate::workspace::register_workspace;

    fn setup() -> (rusqlite::Connection, String, String) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let cfg =
            Config::parse("schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"test\"\n")
                .unwrap();
        let root = std::path::Path::new("/ws");
        let ws = register_workspace(
            &conn,
            root,
            &cfg,
            std::path::Path::new("/ws/.atlas/db.sqlite"),
            "policy-1",
        )
        .unwrap();
        let gen = begin_candidate(
            &conn,
            &ws.workspace_id,
            TriggerKind::Baseline,
            "psh_test",
            "1.1.0",
        )
        .unwrap();
        (conn, ws.workspace_id, gen.generation_id)
    }

    #[test]
    fn descriptor_registration_is_idempotent() {
        let (conn, _, _) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key_a = register_provider_descriptor(&conn, &d).unwrap();
        let key_b = register_provider_descriptor(&conn, &d).unwrap();
        assert_eq!(key_a, key_b);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_descriptor WHERE provider_key = ?1",
                params![key_a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn workspace_provider_policy_upserts() {
        let (conn, ws, _) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key = register_provider_descriptor(&conn, &d).unwrap();
        set_workspace_provider_policy(
            &conn,
            &ws,
            &key,
            true,
            true,
            950,
            "{}",
            &"0".repeat(64),
            1000,
            100,
            100,
            1000,
            "not_required",
        )
        .unwrap();
        set_workspace_provider_policy(
            &conn,
            &ws,
            &key,
            false,
            true,
            950,
            "{}",
            &"0".repeat(64),
            1000,
            100,
            100,
            1000,
            "not_required",
        )
        .unwrap();
        let enabled: i64 = conn
            .query_row(
                "SELECT enabled FROM workspace_provider WHERE workspace_id=?1 AND provider_key=?2",
                params![ws, key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(enabled, 0);
    }

    #[test]
    fn execution_reuse_requires_exact_fingerprint_match() {
        let (conn, ws, gen) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key = register_provider_descriptor(&conn, &d).unwrap();
        let telemetry = ExecutionTelemetry {
            status: ProviderOutcome::Complete,
            network_isolation_state: NetworkIsolationState::NotApplicable,
            started_at: iso8601_now(),
            finished_at: iso8601_now(),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_hash: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            output_hash: None,
            output_bytes: None,
        };
        let fp = "a".repeat(64);
        record_provider_execution(
            &conn,
            &ws,
            &gen,
            &key,
            None,
            "project",
            "project:tsconfig.json",
            &fp,
            &"b".repeat(64),
            "1.1.0",
            "mapping-1.0.0",
            &telemetry,
        )
        .unwrap();

        let reused = find_reusable_execution(
            &conn,
            &ws,
            &key,
            "project",
            "project:tsconfig.json",
            &fp,
            "1.1.0",
            "mapping-1.0.0",
        )
        .unwrap();
        assert!(reused.is_some(), "identical fingerprint must be reusable");

        let different_fp = "c".repeat(64);
        let not_reused = find_reusable_execution(
            &conn,
            &ws,
            &key,
            "project",
            "project:tsconfig.json",
            &different_fp,
            "1.1.0",
            "mapping-1.0.0",
        )
        .unwrap();
        assert!(
            not_reused.is_none(),
            "a different input fingerprint must not be reused"
        );
    }

    #[test]
    fn invalidation_event_is_recorded_with_reason() {
        let (conn, ws, gen) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key = register_provider_descriptor(&conn, &d).unwrap();
        let id = record_invalidation_event(
            &conn,
            &ws,
            &gen,
            &key,
            "project",
            "project:tsconfig.json",
            None,
            InvalidationReason::ProviderVersionChanged,
            Some(&"a".repeat(64)),
            &"b".repeat(64),
        )
        .unwrap();
        let reason: String = conn.query_row("SELECT reason_code FROM provider_invalidation_event WHERE provider_invalidation_event_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(reason, "provider_version_changed");
    }

    #[test]
    fn required_provider_failure_blocks_activation() {
        let decision = decide_activation(true, ProviderOutcome::Failed);
        assert!(matches!(decision, ActivationDecision::Block { .. }));
    }

    #[test]
    fn optional_provider_failure_degrades_but_proceeds() {
        let decision = decide_activation(false, ProviderOutcome::Failed);
        assert_eq!(decision, ActivationDecision::Proceed { degraded: true });
    }

    #[test]
    fn complete_outcome_never_blocks_even_when_required() {
        let decision = decide_activation(true, ProviderOutcome::Complete);
        assert_eq!(decision, ActivationDecision::Proceed { degraded: false });
    }

    #[test]
    fn partial_outcome_proceeds_but_marks_degraded() {
        let decision = decide_activation(true, ProviderOutcome::Partial);
        assert_eq!(decision, ActivationDecision::Proceed { degraded: true });
    }

    #[test]
    fn execution_document_and_run_links_persist() {
        let (conn, ws, gen) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key = register_provider_descriptor(&conn, &d).unwrap();
        let telemetry = ExecutionTelemetry {
            status: ProviderOutcome::Complete,
            network_isolation_state: NetworkIsolationState::NotApplicable,
            started_at: iso8601_now(),
            finished_at: iso8601_now(),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_hash: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            output_hash: None,
            output_bytes: None,
        };
        let pex = record_provider_execution(
            &conn,
            &ws,
            &gen,
            &key,
            None,
            "project",
            "project:tsconfig.json",
            &"a".repeat(64),
            &"b".repeat(64),
            "1.1.0",
            "mapping-1.0.0",
            &telemetry,
        )
        .unwrap();
        record_execution_document(
            &conn,
            &pex,
            "src/a.ts",
            Some("src/a.ts"),
            "included",
            Some(&"d".repeat(64)),
            Some(&"e".repeat(64)),
            None,
        )
        .unwrap();
        let doc_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_execution_file WHERE provider_execution_id=?1",
                params![pex],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(doc_count, 1);

        record_runtime_diagnostic(
            &conn,
            &pex,
            "warning",
            "dynamic_dispatch_unresolved",
            "registry lookup",
            Some("src/a.ts"),
        )
        .unwrap();
        let diag_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_runtime_diagnostic WHERE provider_execution_id=?1",
                params![pex],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(diag_count, 1);
    }

    #[test]
    fn project_scope_persists_and_matches_input_manifest_hash() {
        let (conn, ws, gen) = setup();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        let scopes = crate::project_scope::discover_project_scopes(
            dir.path(),
            &crate::project_scope::default_typescript_markers(),
        )
        .unwrap();
        let scope_id = record_project_scope(&conn, &ws, &gen, &scopes[0]).unwrap();
        let stored_hash: String = conn
            .query_row(
                "SELECT input_manifest_hash FROM project_scope WHERE project_scope_id=?1",
                params![scope_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_hash,
            crate::project_scope::project_input_manifest_hash(&scopes[0])
        );
    }

    #[test]
    fn document_reuse_decision_matches_on_identical_digest_only() {
        assert!(decide_document_reuse(Some("d1"), "d1"));
        assert!(!decide_document_reuse(Some("d1"), "d2"));
        assert!(!decide_document_reuse(None, "d1"));
    }

    #[test]
    fn previous_document_digest_reflects_the_most_recent_execution() {
        let (conn, ws, gen) = setup();
        let d = builtin_provider_descriptors().remove(0);
        let key = register_provider_descriptor(&conn, &d).unwrap();
        let telemetry = ExecutionTelemetry {
            status: ProviderOutcome::Complete,
            network_isolation_state: NetworkIsolationState::NotApplicable,
            started_at: iso8601_now(),
            finished_at: iso8601_now(),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_hash: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            output_hash: None,
            output_bytes: None,
        };

        assert_eq!(
            previous_document_digest(&conn, &ws, &key, "src/a.ts").unwrap(),
            None
        );

        let pex1 = record_provider_execution(
            &conn,
            &ws,
            &gen,
            &key,
            None,
            "project",
            "scope1",
            &"a".repeat(64),
            &"b".repeat(64),
            "1.1.0",
            "mapping-1.0.0",
            &telemetry,
        )
        .unwrap();
        record_execution_document(
            &conn,
            &pex1,
            "src/a.ts",
            Some("src/a.ts"),
            "included",
            Some(&"1".repeat(64)),
            Some(&"c".repeat(64)),
            None,
        )
        .unwrap();

        record_execution_document(
            &conn,
            &pex1,
            "src/b.ts",
            Some("src/b.ts"),
            "included",
            Some(&"1".repeat(64)),
            Some(&"d".repeat(64)),
            None,
        )
        .unwrap();
        let found = previous_document_digest(&conn, &ws, &key, "src/b.ts").unwrap();
        assert_eq!(found.as_deref(), Some("d".repeat(64).as_str()));

        let reuse = decide_document_reuse(found.as_deref(), &"d".repeat(64));
        assert!(
            reuse,
            "identical digest on the next execution must be reusable"
        );
    }

    /// SCOPE-006 / ADR-024: a project-scoped provider run is keyed by
    /// `(provider_key, scope_kind, scope_key, input_fingerprint, ...)`, not
    /// by file — so N changed files under the same project scope must
    /// persist exactly one `provider_execution` row, with N
    /// `provider_execution_file` rows hanging off it.
    #[test]
    fn many_documents_in_one_project_scope_persist_exactly_one_execution() {
        let (conn, ws, gen) = setup();
        let d = crate::providers::scip_typescript_descriptor(None, &serde_json::json!({}));
        let key = register_provider_descriptor(&conn, &d).unwrap();
        let telemetry = ExecutionTelemetry {
            status: ProviderOutcome::Complete,
            network_isolation_state: NetworkIsolationState::NotApplicable,
            started_at: iso8601_now(),
            finished_at: iso8601_now(),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_hash: None,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            output_hash: Some("f".repeat(64)),
            output_bytes: Some(23_867),
        };
        let scope_key = "project:tsconfig.json";
        let fp = "a".repeat(64);
        let pex = record_provider_execution(
            &conn,
            &ws,
            &gen,
            &key,
            None,
            "project",
            scope_key,
            &fp,
            &"b".repeat(64),
            "1.1.0",
            "scip-typescript-mapping-1.0.0",
            &telemetry,
        )
        .unwrap();

        for path in ["src/a.ts", "src/b.ts", "src/c.ts", "src/d.ts", "src/e.ts"] {
            record_execution_document(
                &conn,
                &pex,
                path,
                Some(path),
                "included",
                Some(&"1".repeat(64)),
                Some(&"2".repeat(64)),
                None,
            )
            .unwrap();
        }

        let execution_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_execution WHERE workspace_id=?1 AND provider_key=?2 AND scope_key=?3",
                params![ws, key, scope_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            execution_count, 1,
            "5 changed files in one project scope must be exactly 1 execution, never 1-per-file"
        );

        let document_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM provider_execution_file WHERE provider_execution_id=?1",
                params![pex],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(document_count, 5);
    }

    /// RUN-014: a required provider's failure outcome, run through
    /// `decide_activation`, must actually result in the workspace's active
    /// generation staying on the last good committed generation — not just
    /// a `Block` value in memory.
    #[test]
    fn required_provider_failure_leaves_active_generation_unchanged_end_to_end() {
        let (conn, ws, gen1) = setup();
        crate::generation::activate_candidate(&conn, &ws, &gen1, Some("tree_hash_1")).unwrap();

        let gen2 = crate::generation::begin_candidate(
            &conn,
            &ws,
            crate::generation::TriggerKind::Reconcile,
            "psh_2",
            "1.1.0",
        )
        .unwrap();

        let decision = decide_activation(true, ProviderOutcome::Failed);
        match decision {
            ActivationDecision::Block { reason } => {
                crate::generation::fail_candidate(
                    &conn,
                    &gen2.generation_id,
                    "required_provider_failed",
                    &reason,
                )
                .unwrap();
            }
            other => panic!("expected Block for a required-provider failure, got {other:?}"),
        }

        let active: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active.as_deref(),
            Some(gen1.as_str()),
            "active generation must remain gen1 after gen2's required-provider failure"
        );

        let gen2_state: String = conn
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id = ?1",
                params![gen2.generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gen2_state, "failed");
    }

    #[test]
    fn ensure_semantic_extractor_run_is_idempotent_by_identity() {
        let (conn, _ws, gen) = setup();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("a.ts"), "export {}\n").unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws2 = crate::workspace::register_workspace(
            &conn,
            dir.path(),
            &cfg,
            std::path::Path::new("/ws2/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let report = crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        let _ = gen;
        let revision_id: String = conn
            .query_row("SELECT revision_id FROM file_revision LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = ensure_semantic_extractor_run(
            &conn,
            &revision_id,
            "scip-typescript",
            "0.4.0",
            &"c".repeat(64),
            "1.1.0",
            "complete",
            100,
        )
        .unwrap();
        let b = ensure_semantic_extractor_run(
            &conn,
            &revision_id,
            "scip-typescript",
            "0.4.0",
            &"c".repeat(64),
            "1.1.0",
            "complete",
            100,
        )
        .unwrap();
        assert_eq!(a, b);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM extractor_run WHERE extractor_run_id=?1",
                params![a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(!report.candidate_generation_id.is_empty());
    }

    #[test]
    fn scip_position_conversion_respects_utf8_utf16_and_utf32_units() {
        use crate::scip_decoder::PositionEncoding;
        let text = "é😀foo\n";
        assert_eq!(
            scip_position_to_byte_offset(text, 0, 6, PositionEncoding::Utf8),
            Some(6)
        );
        assert_eq!(
            scip_position_to_byte_offset(text, 0, 3, PositionEncoding::Utf16),
            Some(6)
        );
        assert_eq!(
            scip_position_to_byte_offset(text, 0, 2, PositionEncoding::Utf32),
            Some(6)
        );
        assert_eq!(
            scip_position_to_byte_offset(text, 0, 99, PositionEncoding::Utf8),
            None
        );
    }

    #[test]
    fn semantic_symbol_fact_persists_with_correct_byte_offsets() {
        let (conn, _ws, _gen) = setup();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("a.ts"), "export function foo() {}\n").unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws2 = crate::workspace::register_workspace(
            &conn,
            dir.path(),
            &cfg,
            std::path::Path::new("/ws3/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        let revision_id: String = conn
            .query_row("SELECT revision_id FROM file_revision LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();

        let run_id = ensure_semantic_extractor_run(
            &conn,
            &revision_id,
            "scip-typescript",
            "0.4.0",
            &"c".repeat(64),
            "1.1.0",
            "complete",
            100,
        )
        .unwrap();
        let text = "export function foo() {}\n";
        let sym = crate::scip_mapping::MappedSymbol {
            canonical_symbol_key: "scip:pkg foo().".to_string(),
            provider_symbol_id: "pkg foo().".to_string(),
            canonical_document_path: "a.ts".to_string(),
            is_local: false,
            start_line: 0,
            start_column: 16, // "export function " is 16 chars
            end_line: 0,
            end_column: 19, // "foo" is 3 chars
            confidence: 0.95,
        };
        let id = record_semantic_symbol_fact(
            &conn,
            &run_id,
            &revision_id,
            text,
            crate::scip_decoder::PositionEncoding::Utf16,
            &sym,
        )
        .unwrap()
        .unwrap();
        let (start_byte, end_byte, display_name): (i64, i64, String) = conn
            .query_row("SELECT start_byte, end_byte, display_name FROM symbol_fact WHERE symbol_fact_id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(&text[start_byte as usize..end_byte as usize], "foo");
        assert_eq!(display_name, "foo");
    }

    #[test]
    fn semantic_symbol_fact_skips_span_outside_current_text() {
        let (conn, _ws, _gen) = setup();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("a.ts"), "x\n").unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws2 = crate::workspace::register_workspace(
            &conn,
            dir.path(),
            &cfg,
            std::path::Path::new("/ws4/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        crate::discovery::reconcile(&ws2, &conn, &cfg).unwrap();
        let revision_id: String = conn
            .query_row("SELECT revision_id FROM file_revision LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let run_id = ensure_semantic_extractor_run(
            &conn,
            &revision_id,
            "scip-typescript",
            "0.4.0",
            &"c".repeat(64),
            "1.1.0",
            "complete",
            100,
        )
        .unwrap();
        let sym = crate::scip_mapping::MappedSymbol {
            canonical_symbol_key: "scip:pkg gone().".to_string(),
            provider_symbol_id: "pkg gone().".to_string(),
            canonical_document_path: "a.ts".to_string(),
            is_local: false,
            start_line: 50,
            start_column: 0,
            end_line: 50,
            end_column: 4,
            confidence: 0.95,
        };
        let result = record_semantic_symbol_fact(
            &conn,
            &run_id,
            &revision_id,
            "x\n",
            crate::scip_decoder::PositionEncoding::Utf16,
            &sym,
        )
        .unwrap();
        assert!(
            result.is_none(),
            "a span past the end of the current text must be skipped, not fabricated"
        );
    }
}
