//! Semantic-provider JSON contract types.
//!
//! These versioned `serde` definitions are the authoritative public wire
//! contracts. They reject unknown fields, and the examples under
//! `tests/contract_examples/` verify their serialized shape. Matching
//! persistence constraints live in
//! `migrations/0002_semantic_provider_foundation.sql`.
//!
//! These types form the shared boundary between core, which owns execution
//! identity, scope, persistence, and activation, and provider adapters, which
//! never receive a database handle. Nothing in this module writes to the
//! catalogue.

use serde::{Deserialize, Serialize};

use crate::providers::ProviderTier;

/// Protocol version pinned by every provider-runtime JSON document's
/// `schema_version` or `protocol_version` field.
pub const PROTOCOL_VERSION: &str = "1.0.0";

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Builtin,
    ExternalProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionScopeKind {
    File,
    Project,
    Package,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    NormalizedJson,
    Scip,
    Builtin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultOutputFormat {
    Scip,
    NormalizedJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Symbols,
    Definitions,
    References,
    Imports,
    Exports,
    Implementations,
    TypeDefinitions,
    CallSites,
    Effects,
    Coverage,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationDefaultScope {
    None,
    File,
    Project,
    Package,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUpgradeInvalidation {
    None,
    AffectedScopes,
    AllProviderOutput,
    ResolutionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkIsolationState {
    Enforced,
    BestEffort,
    NotAvailable,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
    Fatal,
}

/// Provider outcome states. One enum spans the transient database lifecycle
/// (`planned`, `running`) and the terminal states carried by
/// `ProviderExecutionResult`. `planned` and `running` are never constructed
/// into a finished result, so the code and migration share one canonical
/// outcome vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOutcome {
    Planned,
    Running,
    Complete,
    Partial,
    Unsupported,
    Unavailable,
    Failed,
    Cancelled,
    TimedOut,
    OutputRejected,
    DecodeFailed,
    MappingFailed,
    Stale,
}

impl ProviderOutcome {
    /// Terminal outcomes are the ones a `ProviderExecutionResult` document
    /// may carry; `planned`/`running` are DB-only in-flight states.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Planned | Self::Running)
    }
}

/// Availability status returned by a provider probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Available,
    Unavailable,
    Unsupported,
    Misconfigured,
    BlockedByPolicy,
}

/// Document-level status inside a `ProviderExecutionResult`
/// (`provider_execution_file.document_status` DB domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Included,
    Reused,
    Partial,
    Skipped,
    Failed,
    Unmapped,
    OutsideWorkspace,
}

// ---------------------------------------------------------------------------
// Provider descriptor (CFG-001)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidationPolicy {
    pub default_scope: InvalidationDefaultScope,
    pub provider_upgrade: ProviderUpgradeInvalidation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableSpec {
    pub command: String,
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,
}

/// Full provider descriptor. Its fields also supply the persisted
/// `provider_descriptor` identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDescriptor {
    pub protocol_version: String,
    pub name: String,
    pub version: String,
    pub tier: ProviderTier,
    pub execution_kind: ExecutionKind,
    pub execution_scope: ExecutionScopeKind,
    pub output_format: OutputFormat,
    pub deterministic: bool,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub artifact_classes: Vec<String>,
    #[serde(default)]
    pub project_markers: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub limitations: Vec<String>,
    pub invalidation: InvalidationPolicy,
    pub executable: Option<ExecutableSpec>,
    /// `sha256(canonical_json(...))`, populated by `ProviderDescriptor::seal`.
    /// Empty until sealed (never emitted this way — `seal` is the only public
    /// constructor besides `Deserialize`, which requires the field present).
    pub fingerprint: String,
}

/// Inputs to the descriptor fingerprint:
/// `sha256(canonical JSON of {descriptor, executable_hash, configuration,
/// protocol_version, output_format, decoder_version, mapping_policy_version})`.
/// Canonical JSON is UTF-8 with sorted keys and no insignificant whitespace.
#[derive(Serialize)]
struct FingerprintInput<'a> {
    descriptor: &'a ProviderDescriptorIdentity<'a>,
    executable_hash: Option<&'a str>,
    configuration: &'a serde_json::Value,
    protocol_version: &'a str,
    output_format: OutputFormat,
    decoder_version: &'a str,
    mapping_policy_version: &'a str,
}

/// The descriptor's identity fields, excluding `fingerprint` itself (which
/// would otherwise make the hash self-referential).
#[derive(Serialize)]
struct ProviderDescriptorIdentity<'a> {
    protocol_version: &'a str,
    name: &'a str,
    version: &'a str,
    tier: ProviderTier,
    execution_kind: ExecutionKind,
    execution_scope: ExecutionScopeKind,
    output_format: OutputFormat,
    deterministic: bool,
    languages: &'a [String],
    artifact_classes: &'a [String],
    project_markers: &'a [String],
    capabilities: &'a [Capability],
    limitations: &'a [String],
    invalidation: &'a InvalidationPolicy,
    executable: &'a Option<ExecutableSpec>,
}

/// Canonical JSON: `serde_json::Value`'s default `Map` implementation is
/// `BTreeMap`-backed (the crate's `preserve_order` feature is not enabled in
/// `Cargo.toml`), so converting through `Value` sorts every object's keys.
/// `to_vec` then produces compact (no insignificant whitespace), UTF-8 bytes.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    let as_value = serde_json::to_value(value).expect("contract types always serialize");
    serde_json::to_vec(&as_value).expect("serde_json::Value always serializes")
}

impl ProviderDescriptor {
    /// Build a descriptor and stamp its `fingerprint`. `configuration` is the
    /// canonical (already-validated) provider configuration this descriptor
    /// registration corresponds to; pass `serde_json::json!({})` for a
    /// registration with no instance configuration.
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        protocol_version: String,
        name: String,
        version: String,
        tier: ProviderTier,
        execution_kind: ExecutionKind,
        execution_scope: ExecutionScopeKind,
        output_format: OutputFormat,
        deterministic: bool,
        languages: Vec<String>,
        artifact_classes: Vec<String>,
        project_markers: Vec<String>,
        capabilities: Vec<Capability>,
        limitations: Vec<String>,
        invalidation: InvalidationPolicy,
        executable: Option<ExecutableSpec>,
        executable_hash: Option<&str>,
        configuration: &serde_json::Value,
        decoder_version: &str,
        mapping_policy_version: &str,
    ) -> Self {
        let identity = ProviderDescriptorIdentity {
            protocol_version: &protocol_version,
            name: &name,
            version: &version,
            tier,
            execution_kind,
            execution_scope,
            output_format,
            deterministic,
            languages: &languages,
            artifact_classes: &artifact_classes,
            project_markers: &project_markers,
            capabilities: &capabilities,
            limitations: &limitations,
            invalidation: &invalidation,
            executable: &executable,
        };
        let input = FingerprintInput {
            descriptor: &identity,
            executable_hash,
            configuration,
            protocol_version: &protocol_version,
            output_format,
            decoder_version,
            mapping_policy_version,
        };
        let fingerprint = crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&input));

        Self {
            protocol_version,
            name,
            version,
            tier,
            execution_kind,
            execution_scope,
            output_format,
            deterministic,
            languages,
            artifact_classes,
            project_markers,
            capabilities,
            limitations,
            invalidation,
            executable,
            fingerprint,
        }
    }

    /// `provider_key` used as the `provider_descriptor.provider_key` primary
    /// key: stable across re-registration as long as name/version/fingerprint
    /// are unchanged (mirrors the table's
    /// `UNIQUE(provider_name, provider_version, descriptor_hash)`).
    pub fn provider_key(&self) -> String {
        format!("{}@{}#{}", self.name, self.version, &self.fingerprint[..16])
    }
}

// ---------------------------------------------------------------------------
// Support decision (CFG-002)
// ---------------------------------------------------------------------------

/// Whether a provider applies to a given file/project scope. Not part of the
/// JSON wire contracts (those start from a probe result); this is the local
/// decision Atlas makes before even attempting a probe/execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SupportDecision {
    Supported,
    UnsupportedLanguage { language: Option<String> },
    UnsupportedScope { requested: ExecutionScopeKind },
    NoProjectMarker,
    DisabledByConfiguration,
}

impl SupportDecision {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Decide whether `descriptor` supports a candidate artifact. `language` is
/// `None` for non-language artifacts (e.g. document/config providers).
pub fn decide_support(
    descriptor: &ProviderDescriptor,
    enabled: bool,
    language: Option<&str>,
    requested_scope: ExecutionScopeKind,
) -> SupportDecision {
    if !enabled {
        return SupportDecision::DisabledByConfiguration;
    }
    if descriptor.execution_scope != requested_scope {
        return SupportDecision::UnsupportedScope {
            requested: requested_scope,
        };
    }
    if !descriptor.languages.is_empty() {
        match language {
            Some(lang) if descriptor.languages.iter().any(|l| l == lang) => {}
            _ => {
                return SupportDecision::UnsupportedLanguage {
                    language: language.map(str::to_string),
                }
            }
        }
    }
    SupportDecision::Supported
}

// ---------------------------------------------------------------------------
// Probe result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProbeResult {
    pub schema_version: String,
    pub provider_name: String,
    pub status: ProbeStatus,
    #[serde(default)]
    pub observed_version: Option<String>,
    #[serde(default)]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub executable_hash: Option<String>,
    pub supported_output_formats: Vec<String>,
    pub supported_arguments: Vec<String>,
    pub network_isolation_state: NetworkIsolationState,
    pub observed_at: String,
    pub diagnostics: Vec<ProbeDiagnostic>,
}

// ---------------------------------------------------------------------------
// Process request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRef {
    pub name: String,
    pub version: String,
    pub fingerprint: String,
    pub configuration_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestScope {
    pub kind: ExecutionScopeKind,
    pub key: String,
    pub canonical_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_manifest: Option<String>,
    pub input_manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestOutput {
    pub format: ResultOutputFormat,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBudget {
    pub timeout_ms: u64,
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProcessRequest {
    pub schema_version: String,
    pub execution_id: String,
    pub workspace_id: String,
    pub candidate_generation_id: String,
    pub provider: ProviderRef,
    pub scope: RequestScope,
    pub input_fingerprint: String,
    pub output: RequestOutput,
    pub budget: RequestBudget,
}

// ---------------------------------------------------------------------------
// Execution result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultScope {
    pub kind: ExecutionScopeKind,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultOutput {
    pub format: ResultOutputFormat,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultTiming {
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultStreams {
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultDocument {
    pub provider_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_path: Option<String>,
    pub status: DocumentStatus,
    #[serde(default)]
    pub raw_document_hash: Option<String>,
    #[serde(default)]
    pub normalized_batch_hash: Option<String>,
    pub diagnostic_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub provider_document_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExecutionResult {
    pub schema_version: String,
    pub execution_id: String,
    pub workspace_id: String,
    pub candidate_generation_id: String,
    pub provider: ProviderRef,
    pub scope: ResultScope,
    pub input_fingerprint: String,
    pub status: ProviderOutcome,
    pub network_isolation_state: NetworkIsolationState,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub output: Option<ResultOutput>,
    pub timing: ResultTiming,
    pub streams: ResultStreams,
    pub documents: Vec<ResultDocument>,
    pub diagnostics: Vec<ResultDiagnostic>,
}

// ---------------------------------------------------------------------------
// Deterministic provider-set hash (CFG-007)
// ---------------------------------------------------------------------------

/// One workspace's enabled-provider identity contribution to
/// `index_generation.provider_set_hash`.
pub struct ProviderSetMember<'a> {
    pub provider_key: &'a str,
    pub descriptor_fingerprint: &'a str,
    pub configuration_hash: &'a str,
    pub priority: i64,
}

/// Deterministic, order-independent hash over the enabled provider set for a
/// workspace generation. Sorted by `provider_key` before hashing so callers
/// may pass members in any order (SCOPE/CFG requirement: "order-independence
/// test").
pub fn provider_set_hash(members: &[ProviderSetMember<'_>]) -> String {
    #[derive(Serialize)]
    struct Row<'a> {
        provider_key: &'a str,
        descriptor_fingerprint: &'a str,
        configuration_hash: &'a str,
        priority: i64,
    }
    let mut rows: Vec<Row> = members
        .iter()
        .map(|m| Row {
            provider_key: m.provider_key,
            descriptor_fingerprint: m.descriptor_fingerprint,
            configuration_hash: m.configuration_hash,
            priority: m.priority,
        })
        .collect();
    rows.sort_by(|a, b| a.provider_key.cmp(b.provider_key));
    crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&rows))
}

// ---------------------------------------------------------------------------
// Capability / status reporting (CFG-008)
// ---------------------------------------------------------------------------

/// One provider's status as reported by `atlas providers` and its MCP mirror.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderCapabilityStatus {
    pub provider_key: String,
    pub name: String,
    pub version: String,
    pub configured: bool,
    pub enabled: bool,
    pub required: bool,
    pub available: bool,
    pub descriptor_fingerprint: String,
    pub tier: ProviderTier,
    pub execution_scope: ExecutionScopeKind,
    pub languages: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub limitations: Vec<String>,
    pub last_execution_status: Option<ProviderOutcome>,
    pub last_compatible_execution_at: Option<String>,
    pub network_isolation_state: Option<NetworkIsolationState>,
    pub invalidation_reason: Option<String>,
}

/// Build a `ProviderCapabilityStatus` from the descriptor plus the
/// workspace's configuration/probe/execution-history knowledge. Pure
/// projection — no I/O, no database handle (providers/adapters never get
/// one; this mirrors that boundary for the reporting path too).
pub fn capability_status(
    descriptor: &ProviderDescriptor,
    configured: bool,
    enabled: bool,
    required: bool,
    last_probe: Option<&ProviderProbeResult>,
    last_execution: Option<&ProviderExecutionResult>,
    invalidation_reason: Option<String>,
) -> ProviderCapabilityStatus {
    let available = last_probe
        .map(|p| p.status == ProbeStatus::Available)
        .unwrap_or(descriptor.execution_kind == ExecutionKind::Builtin);
    let network_isolation_state = last_probe
        .map(|p| p.network_isolation_state)
        .or_else(|| last_execution.map(|e| e.network_isolation_state));
    let last_execution_status = last_execution.map(|e| e.status);
    let last_compatible_execution_at = last_execution
        .filter(|e| {
            e.status.is_terminal()
                && matches!(
                    e.status,
                    ProviderOutcome::Complete | ProviderOutcome::Partial
                )
        })
        .map(|e| e.timing.finished_at.clone());

    ProviderCapabilityStatus {
        provider_key: descriptor.provider_key(),
        name: descriptor.name.clone(),
        version: descriptor.version.clone(),
        configured,
        enabled,
        required,
        available,
        descriptor_fingerprint: descriptor.fingerprint.clone(),
        tier: descriptor.tier,
        execution_scope: descriptor.execution_scope,
        languages: descriptor.languages.clone(),
        capabilities: descriptor.capabilities.clone(),
        limitations: descriptor.limitations.clone(),
        last_execution_status,
        last_compatible_execution_at,
        network_isolation_state,
        invalidation_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_descriptor() -> ProviderDescriptor {
        ProviderDescriptor::seal(
            PROTOCOL_VERSION.to_string(),
            "scip-typescript".to_string(),
            "0.4.0".to_string(),
            ProviderTier::SemanticIndex,
            ExecutionKind::ExternalProcess,
            ExecutionScopeKind::Project,
            OutputFormat::Scip,
            true,
            vec!["typescript".to_string(), "javascript".to_string()],
            vec!["source".to_string(), "test".to_string()],
            vec![
                "tsconfig.json".to_string(),
                "jsconfig.json".to_string(),
                "package.json".to_string(),
            ],
            vec![
                Capability::Symbols,
                Capability::Definitions,
                Capability::References,
                Capability::Imports,
                Capability::Implementations,
                Capability::TypeDefinitions,
                Capability::Coverage,
                Capability::Diagnostics,
            ],
            vec!["dynamic dispatch is not exhaustively resolved".to_string()],
            InvalidationPolicy {
                default_scope: InvalidationDefaultScope::Project,
                provider_upgrade: ProviderUpgradeInvalidation::AllProviderOutput,
            },
            Some(ExecutableSpec {
                command: "scip-typescript".to_string(),
                arguments: vec![
                    "index".to_string(),
                    "--output".to_string(),
                    "{output_file}".to_string(),
                ],
                expected_output: Some("index.scip".to_string()),
            }),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            &serde_json::json!({}),
            "scip-decoder-1.0.0",
            "scip-typescript-mapping-1.0.0",
        )
    }

    #[test]
    fn descriptor_fingerprint_is_deterministic_and_64_hex() {
        let a = sample_descriptor();
        let b = sample_descriptor();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 64);
        assert!(a.fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn descriptor_fingerprint_changes_with_version() {
        let a = sample_descriptor();
        let mut b_builder = sample_descriptor();
        b_builder.version = "0.4.1".to_string();
        // Re-seal with the changed version so the fingerprint reflects it
        // (mutating `.version` alone does not recompute `.fingerprint`).
        let b = ProviderDescriptor::seal(
            b_builder.protocol_version,
            b_builder.name,
            b_builder.version,
            b_builder.tier,
            b_builder.execution_kind,
            b_builder.execution_scope,
            b_builder.output_format,
            b_builder.deterministic,
            b_builder.languages,
            b_builder.artifact_classes,
            b_builder.project_markers,
            b_builder.capabilities,
            b_builder.limitations,
            b_builder.invalidation,
            b_builder.executable,
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            &serde_json::json!({}),
            "scip-decoder-1.0.0",
            "scip-typescript-mapping-1.0.0",
        );
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn descriptor_fingerprint_changes_with_executable_hash() {
        let d = sample_descriptor();
        let zero_hash = "0".repeat(64);
        let same_hash =
            crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&FingerprintInput {
                descriptor: &ProviderDescriptorIdentity {
                    protocol_version: &d.protocol_version,
                    name: &d.name,
                    version: &d.version,
                    tier: d.tier,
                    execution_kind: d.execution_kind,
                    execution_scope: d.execution_scope,
                    output_format: d.output_format,
                    deterministic: d.deterministic,
                    languages: &d.languages,
                    artifact_classes: &d.artifact_classes,
                    project_markers: &d.project_markers,
                    capabilities: &d.capabilities,
                    limitations: &d.limitations,
                    invalidation: &d.invalidation,
                    executable: &d.executable,
                },
                executable_hash: Some(zero_hash.as_str()),
                configuration: &serde_json::json!({}),
                protocol_version: &d.protocol_version,
                output_format: d.output_format,
                decoder_version: "scip-decoder-1.0.0",
                mapping_policy_version: "scip-typescript-mapping-1.0.0",
            }));
        assert_ne!(
            same_hash, d.fingerprint,
            "changing executable_hash must change the fingerprint"
        );
    }

    #[test]
    fn descriptor_round_trips_contract_example() {
        let example = include_str!("../tests/contract_examples/provider-descriptor.example.json");
        let parsed: ProviderDescriptor = serde_json::from_str(example)
            .unwrap_or_else(|e| panic!("descriptor contract example must deserialize: {e}"));
        assert_eq!(parsed.name, "scip-typescript");
        assert_eq!(parsed.tier, ProviderTier::SemanticIndex);
        assert_eq!(parsed.execution_kind, ExecutionKind::ExternalProcess);
        let reserialized = serde_json::to_value(&parsed).unwrap();
        let original: serde_json::Value = serde_json::from_str(example).unwrap();
        assert_eq!(reserialized, original, "round trip must be lossless");
    }

    #[test]
    fn probe_result_round_trips_contract_example() {
        let example = include_str!("../tests/contract_examples/provider-probe-result.example.json");
        let parsed: ProviderProbeResult = serde_json::from_str(example)
            .unwrap_or_else(|e| panic!("probe contract example must deserialize: {e}"));
        assert_eq!(parsed.status, ProbeStatus::Available);
        assert_eq!(
            parsed.network_isolation_state,
            NetworkIsolationState::NotAvailable
        );
    }

    #[test]
    fn process_request_round_trips_contract_example() {
        let example =
            include_str!("../tests/contract_examples/provider-process-request.example.json");
        let parsed: ProviderProcessRequest = serde_json::from_str(example)
            .unwrap_or_else(|e| panic!("process-request contract example must deserialize: {e}"));
        assert_eq!(parsed.scope.kind, ExecutionScopeKind::Project);
        assert_eq!(parsed.output.format, ResultOutputFormat::Scip);
        assert_eq!(parsed.budget.timeout_ms, 120000);
    }

    #[test]
    fn execution_result_round_trips_contract_example() {
        let example =
            include_str!("../tests/contract_examples/provider-execution-result.example.json");
        let parsed: ProviderExecutionResult = serde_json::from_str(example)
            .unwrap_or_else(|e| panic!("execution-result contract example must deserialize: {e}"));
        assert_eq!(parsed.status, ProviderOutcome::Complete);
        assert_eq!(parsed.documents.len(), 2);
        assert_eq!(parsed.documents[1].status, DocumentStatus::Partial);
        assert!(parsed.status.is_terminal());
    }

    #[test]
    fn unknown_field_is_rejected_fail_closed() {
        let bad = r#"{
            "schema_version": "1.0.0",
            "provider_name": "x",
            "status": "available",
            "observed_at": "2026-01-01T00:00:00Z",
            "network_isolation_state": "not_available",
            "diagnostics": [],
            "supported_output_formats": [],
            "supported_arguments": [],
            "totally_unexpected_field": true
        }"#;
        let err = serde_json::from_str::<ProviderProbeResult>(bad);
        assert!(err.is_err(), "unknown fields must fail closed (CFG-009)");
    }

    #[test]
    fn support_decision_rejects_unsupported_language() {
        let d = sample_descriptor();
        let decision = decide_support(&d, true, Some("python"), ExecutionScopeKind::Project);
        assert!(!decision.is_supported());
        let decision = decide_support(&d, true, Some("typescript"), ExecutionScopeKind::Project);
        assert!(decision.is_supported());
    }

    #[test]
    fn support_decision_respects_enabled_flag() {
        let d = sample_descriptor();
        let decision = decide_support(&d, false, Some("typescript"), ExecutionScopeKind::Project);
        assert_eq!(decision, SupportDecision::DisabledByConfiguration);
    }

    #[test]
    fn support_decision_rejects_scope_mismatch() {
        let d = sample_descriptor();
        let decision = decide_support(&d, true, Some("typescript"), ExecutionScopeKind::File);
        assert!(!decision.is_supported());
    }

    #[test]
    fn provider_set_hash_is_order_independent() {
        let a = ProviderSetMember {
            provider_key: "file_metadata@1.0.0",
            descriptor_fingerprint:
                "1111111111111111111111111111111111111111111111111111111111111111",
            configuration_hash: "2222222222222222222222222222222222222222222222222222222222222222",
            priority: 950,
        };
        let b = ProviderSetMember {
            provider_key: "scip-typescript@0.4.0",
            descriptor_fingerprint:
                "3333333333333333333333333333333333333333333333333333333333333333",
            configuration_hash: "4444444444444444444444444444444444444444444444444444444444444444",
            priority: 800,
        };
        let forward = provider_set_hash(&[
            ProviderSetMember {
                provider_key: a.provider_key,
                descriptor_fingerprint: a.descriptor_fingerprint,
                configuration_hash: a.configuration_hash,
                priority: a.priority,
            },
            ProviderSetMember {
                provider_key: b.provider_key,
                descriptor_fingerprint: b.descriptor_fingerprint,
                configuration_hash: b.configuration_hash,
                priority: b.priority,
            },
        ]);
        let reversed = provider_set_hash(&[
            ProviderSetMember {
                provider_key: b.provider_key,
                descriptor_fingerprint: b.descriptor_fingerprint,
                configuration_hash: b.configuration_hash,
                priority: b.priority,
            },
            ProviderSetMember {
                provider_key: a.provider_key,
                descriptor_fingerprint: a.descriptor_fingerprint,
                configuration_hash: a.configuration_hash,
                priority: a.priority,
            },
        ]);
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 64);
    }

    #[test]
    fn provider_set_hash_changes_when_membership_changes() {
        let a = provider_set_hash(&[ProviderSetMember {
            provider_key: "file_metadata@1.0.0",
            descriptor_fingerprint:
                "1111111111111111111111111111111111111111111111111111111111111111",
            configuration_hash: "2222222222222222222222222222222222222222222222222222222222222222",
            priority: 950,
        }]);
        let b = provider_set_hash(&[
            ProviderSetMember {
                provider_key: "file_metadata@1.0.0",
                descriptor_fingerprint:
                    "1111111111111111111111111111111111111111111111111111111111111111",
                configuration_hash:
                    "2222222222222222222222222222222222222222222222222222222222222222",
                priority: 950,
            },
            ProviderSetMember {
                provider_key: "scip-typescript@0.4.0",
                descriptor_fingerprint:
                    "3333333333333333333333333333333333333333333333333333333333333333",
                configuration_hash:
                    "4444444444444444444444444444444444444444444444444444444444444444",
                priority: 800,
            },
        ]);
        assert_ne!(a, b);
    }

    #[test]
    fn capability_status_reflects_probe_and_last_execution() {
        let d = sample_descriptor();
        let probe: ProviderProbeResult = serde_json::from_str(include_str!(
            "../tests/contract_examples/provider-probe-result.example.json"
        ))
        .unwrap();
        let execution: ProviderExecutionResult = serde_json::from_str(include_str!(
            "../tests/contract_examples/provider-execution-result.example.json"
        ))
        .unwrap();

        let status = capability_status(&d, true, true, false, Some(&probe), Some(&execution), None);
        assert!(status.configured);
        assert!(status.enabled);
        assert!(
            status.available,
            "probe status 'available' must mark the provider available"
        );
        assert_eq!(
            status.last_execution_status,
            Some(ProviderOutcome::Complete)
        );
        assert_eq!(
            status.last_compatible_execution_at.as_deref(),
            Some("2026-08-29T08:00:02Z")
        );
        assert_eq!(
            status.network_isolation_state,
            Some(NetworkIsolationState::NotAvailable)
        );
        assert_eq!(status.descriptor_fingerprint, d.fingerprint);
    }

    #[test]
    fn capability_status_unconfigured_provider_is_unavailable_without_probe() {
        let d = sample_descriptor();
        let status = capability_status(
            &d,
            false,
            false,
            false,
            None,
            None,
            Some("no probe attempted".to_string()),
        );
        assert!(!status.configured);
        assert!(!status.enabled);
        assert!(
            !status.available,
            "external provider with no probe evidence must not claim availability"
        );
        assert_eq!(
            status.invalidation_reason.as_deref(),
            Some("no probe attempted")
        );
    }

    #[test]
    fn capability_status_builtin_without_probe_is_available() {
        let d = ProviderDescriptor::seal(
            PROTOCOL_VERSION.to_string(),
            "file_metadata".to_string(),
            "1.0.0".to_string(),
            ProviderTier::DocumentConfig,
            ExecutionKind::Builtin,
            ExecutionScopeKind::File,
            OutputFormat::Builtin,
            true,
            vec![],
            vec![],
            vec![],
            vec![Capability::Symbols],
            vec![],
            InvalidationPolicy {
                default_scope: InvalidationDefaultScope::File,
                provider_upgrade: ProviderUpgradeInvalidation::AffectedScopes,
            },
            None,
            None,
            &serde_json::json!({}),
            "builtin-1.0.0",
            "builtin-1.0.0",
        );
        let status = capability_status(&d, true, true, true, None, None, None);
        assert!(
            status.available,
            "a builtin provider needs no external probe to be available"
        );
    }
}
