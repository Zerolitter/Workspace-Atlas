//! Context Intelligence JSON contract types.
//!
//! These versioned `serde` definitions are the authoritative public wire
//! contracts. They reject unknown fields, and the examples under
//! `tests/fixtures/context_ir/` verify their serialized shape. The matching
//! persistence constraints live in
//! `migrations/0003_context_intelligence_foundation.sql`.
//!
//! These are pure document types: persistence and compilation remain in their
//! owning modules, and a migrated catalogue remains usable with zero rows in
//! the Context Intelligence tables.

use serde::{Deserialize, Serialize};

use crate::error::{AtlasError, Result};
use crate::provider_contract::canonical_json_bytes;

/// Contract schema version pinned by every Context Intelligence JSON
/// document's `schema_version` field.
pub const CONTEXT_SCHEMA_VERSION: &str = "1.0.0";
/// Internal deep-only Context IR successor. This is not the schema used by
/// the existing explicit CLI or MCP operations.
pub const CONTEXT_SCHEMA_V2_VERSION: &str = "2.0.0";

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Explore,
    BugFix,
    BehaviorChange,
    ApiChange,
    Refactor,
    ConfigurationChange,
    TestChange,
    Review,
    Audit,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindSource {
    Declared,
    DeterministicRule,
    Unknown,
}

/// Privacy default (G5 requirement, `SEC-001`-style bar): raw task text is
/// never retained unless the operator explicitly opts in per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawTaskRetention {
    None,
    HashOnly,
    LocalOptIn,
}

impl Default for RawTaskRetention {
    /// Privacy default: never retain raw task text unless explicitly opted in.
    /// Migration `migrations/0003_context_intelligence_foundation.sql` enforces the same
    /// `task_session.raw_task_retention` constraint.
    fn default() -> Self {
        RawTaskRetention::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSessionState {
    Created,
    ContextCompiled,
    Active,
    Reconciled,
    Completed,
    Abandoned,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkingSetStatus {
    Complete,
    Partial,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    File,
    Symbol,
    Config,
    Test,
    Document,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ItemRole {
    PrimaryImplementation,
    DirectDependency,
    DirectDependent,
    TypeContract,
    TestContract,
    ConfigurationInput,
    Effect,
    HistoricalConstraint,
    Uncertainty,
    ValidationTarget,
    Documentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SelectionReason {
    ExplicitSeed,
    ExactSymbolMatch,
    ExactPathMatch,
    DirectCaller,
    DirectCallee,
    ImportNeighbor,
    ImplementationRelation,
    TestRelation,
    ConfigRelation,
    EffectRelation,
    ConflictRelevance,
    UnresolvedRelevance,
    GenerationDeltaRelevance,
    TaskRecipeRequirement,
}

/// Broader evidence-quality vocabulary than `ResolutionStatus`; one closed
/// set covers both symbol and relationship evidence from `verified` through
/// `excluded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceQuality {
    Verified,
    Supported,
    Inferred,
    Resolved,
    Ambiguous,
    Unresolved,
    External,
    Stale,
    Conflicting,
    Partial,
    Unsupported,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceVerificationStatus {
    NotRequested,
    Verified,
    HashMismatch,
    ReadFailed,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrength {
    None,
    Bounded,
    ExhaustiveWithinScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationKind {
    Test,
    Build,
    Lint,
    SourceVerify,
    ManualReview,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectPhase {
    Direct,
    Transitive,
    Declared,
    Observed,
}

/// Change kinds emitted as explicit `DeltaChange` rows. Database-only
/// `unchanged` and `uncertain` bulk-scan states are represented by
/// `GenerationDelta.unchanged_verified_contracts` and `.uncertainty` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Renamed,
    Retargeted,
    StateChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaEvidenceState {
    Verified,
    Supported,
    Inferred,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextUseEventType {
    ContextSupplied,
    ExactSourceRequested,
    EntityQueried,
    RelationshipTraversed,
    ContextRecompiled,
    ArtifactChanged,
    ValidationSelected,
    ReasoningUseReported,
    ModificationTargetReported,
    TestConsideredReported,
    OutcomeRecorded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    AtlasObserved,
    AgentReported,
    OperatorReported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Complete,
    Partial,
    Unsupported,
    Failed,
}

impl CoverageState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CoverageState::Complete => "complete",
            CoverageState::Partial => "partial",
            CoverageState::Unsupported => "unsupported",
            CoverageState::Failed => "failed",
        }
    }
}

// ---------------------------------------------------------------------------
// TaskSession
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSession {
    pub schema_version: String,
    pub task_session_id: String,
    pub workspace_id: String,
    pub start_generation_id: String,
    #[serde(default)]
    pub end_generation_id: Option<String>,
    pub task_hash: String,
    pub normalized_goal_hash: String,
    pub raw_task_retention: RawTaskRetention,
    #[serde(default)]
    pub raw_task: Option<String>,
    pub task_kind: TaskKind,
    pub state: TaskSessionState,
    pub created_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    pub planner_policy_version: String,
    pub context_ir_version: String,
    #[serde(default)]
    pub accepted: Option<bool>,
    #[serde(default)]
    pub tests_passed: Option<bool>,
    #[serde(default)]
    pub outcome_code: Option<String>,
    #[serde(default)]
    pub start_tree_hash: Option<String>,
    #[serde(default)]
    pub end_tree_hash: Option<String>,
}

impl TaskSession {
    /// Enforces the privacy invariant mirrored from
    /// `task_session.CHECK(raw_task_retention = 'local_opt_in' OR raw_task
    /// IS NULL)`: raw task text may only be present when the operator
    /// explicitly opted in for this session. Also rejects a malformed
    /// `task_hash`/`normalized_goal_hash` (must be 64 lowercase hex chars,
    /// matching every other content hash in the catalogue).
    pub fn validate(&self) -> Result<()> {
        if self.raw_task_retention != RawTaskRetention::LocalOptIn && self.raw_task.is_some() {
            return Err(AtlasError::InvalidConfig(
                "raw_task must be null unless raw_task_retention = local_opt_in".to_string(),
            ));
        }
        for (field, value) in [
            ("task_hash", &self.task_hash),
            ("normalized_goal_hash", &self.normalized_goal_hash),
        ] {
            if !is_hex64(value) {
                return Err(AtlasError::InvalidConfig(format!(
                    "{field} must be 64 lowercase hex characters, got {value:?}"
                )));
            }
        }
        Ok(())
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// SymbolCard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolRange {
    pub start_byte: i64,
    pub end_byte: i64,
    pub start_line: i64,
    pub end_line: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferredEvidenceSummary {
    pub fact_id: String,
    pub provider_tier: String,
    pub confidence: f64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolCounts {
    pub callers: i64,
    pub callees: i64,
    pub references: i64,
    pub tests: i64,
    pub configs: i64,
    pub effects: i64,
    pub conflicts: i64,
    pub unresolved: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolCost {
    pub source_bytes: i64,
    pub estimated_tokens: i64,
    pub metadata_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolCard {
    pub schema_version: String,
    pub serving_generation_id: String,
    pub workspace_id: String,
    pub generation_id: String,
    pub canonical_symbol_key: String,
    pub path: String,
    pub range: SymbolRange,
    pub kind: String,
    pub language: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    pub preferred_evidence: PreferredEvidenceSummary,
    pub counts: SymbolCounts,
    pub coverage_state: CoverageState,
    pub cost: SymbolCost,
    /// `sha256(canonical_json(everything above))`, populated by `seal`.
    pub card_hash: String,
}

#[derive(Serialize)]
struct SymbolCardIdentity<'a> {
    schema_version: &'a str,
    serving_generation_id: &'a str,
    workspace_id: &'a str,
    generation_id: &'a str,
    canonical_symbol_key: &'a str,
    path: &'a str,
    range: &'a SymbolRange,
    kind: &'a str,
    language: &'a Option<String>,
    signature: &'a Option<String>,
    preferred_evidence: &'a PreferredEvidenceSummary,
    counts: &'a SymbolCounts,
    coverage_state: CoverageState,
    cost: &'a SymbolCost,
}

impl SymbolCard {
    /// Build a card and stamp its deterministic `card_hash`. Two calls with
    /// identical fields (down to field order — irrelevant, canonical JSON
    /// sorts keys) always produce the same hash (G4's "byte-identical
    /// unchanged claims" requirement).
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        serving_generation_id: String,
        workspace_id: String,
        generation_id: String,
        canonical_symbol_key: String,
        path: String,
        range: SymbolRange,
        kind: String,
        language: Option<String>,
        signature: Option<String>,
        preferred_evidence: PreferredEvidenceSummary,
        counts: SymbolCounts,
        coverage_state: CoverageState,
        cost: SymbolCost,
    ) -> Self {
        let identity = SymbolCardIdentity {
            schema_version: CONTEXT_SCHEMA_VERSION,
            serving_generation_id: &serving_generation_id,
            workspace_id: &workspace_id,
            generation_id: &generation_id,
            canonical_symbol_key: &canonical_symbol_key,
            path: &path,
            range: &range,
            kind: &kind,
            language: &language,
            signature: &signature,
            preferred_evidence: &preferred_evidence,
            counts: &counts,
            coverage_state,
            cost: &cost,
        };
        let card_hash = crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&identity));
        SymbolCard {
            schema_version: CONTEXT_SCHEMA_VERSION.to_string(),
            serving_generation_id,
            workspace_id,
            generation_id,
            canonical_symbol_key,
            path,
            range,
            kind,
            language,
            signature,
            preferred_evidence,
            counts,
            coverage_state,
            cost,
            card_hash,
        }
    }
}

// ---------------------------------------------------------------------------
// ContextUseEvent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUseEvent {
    pub schema_version: String,
    pub event_id: String,
    pub task_session_id: String,
    pub context_id: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub entity_id: Option<String>,
    pub event_type: ContextUseEventType,
    pub observation_source: ObservationSource,
    pub occurred_at: String,
    #[serde(default)]
    pub bytes: Option<i64>,
    #[serde(default = "empty_object")]
    pub details: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

// ---------------------------------------------------------------------------
// GenerationDelta
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaChange {
    pub entity_id: String,
    pub change_kind: ChangeKind,
    pub evidence_state: DeltaEvidenceState,
    #[serde(default)]
    pub before_id: Option<String>,
    #[serde(default)]
    pub after_id: Option<String>,
    #[serde(default)]
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaChangeSet {
    pub changes: Vec<DeltaChange>,
    pub omitted_count: i64,
}

impl DeltaChangeSet {
    pub fn empty() -> Self {
        DeltaChangeSet {
            changes: Vec::new(),
            omitted_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnchangedContract {
    pub entity_id: String,
    pub claim_scope: String,
    /// At least one supporting reason — an "unchanged" claim with no
    /// evidence is never valid (mirrors JSON Schema `minItems: 1`).
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationDelta {
    pub schema_version: String,
    pub delta_id: String,
    pub workspace_id: String,
    pub from_generation_id: String,
    pub to_generation_id: String,
    pub delta_policy_version: String,
    pub files: DeltaChangeSet,
    pub symbols: DeltaChangeSet,
    pub relationships: DeltaChangeSet,
    pub effects: DeltaChangeSet,
    pub coverage: DeltaChangeSet,
    pub conflicts: DeltaChangeSet,
    #[serde(default)]
    pub unchanged_verified_contracts: Vec<UnchangedContract>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
    pub delta_hash: String,
}

#[derive(Serialize)]
struct GenerationDeltaIdentity<'a> {
    schema_version: &'a str,
    delta_id: &'a str,
    workspace_id: &'a str,
    from_generation_id: &'a str,
    to_generation_id: &'a str,
    delta_policy_version: &'a str,
    files: &'a DeltaChangeSet,
    symbols: &'a DeltaChangeSet,
    relationships: &'a DeltaChangeSet,
    effects: &'a DeltaChangeSet,
    coverage: &'a DeltaChangeSet,
    conflicts: &'a DeltaChangeSet,
    unchanged_verified_contracts: &'a [UnchangedContract],
    uncertainty: &'a [String],
}

impl GenerationDelta {
    /// `from_generation_id` and `to_generation_id` must differ (mirrors
    /// `generation_delta.CHECK(from_generation_id <> to_generation_id)`) —
    /// enforced here so an invalid delta is never even constructed, not
    /// merely rejected later at the DB layer.
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        delta_id: String,
        workspace_id: String,
        from_generation_id: String,
        to_generation_id: String,
        delta_policy_version: String,
        files: DeltaChangeSet,
        symbols: DeltaChangeSet,
        relationships: DeltaChangeSet,
        effects: DeltaChangeSet,
        coverage: DeltaChangeSet,
        conflicts: DeltaChangeSet,
        unchanged_verified_contracts: Vec<UnchangedContract>,
        uncertainty: Vec<String>,
    ) -> Result<Self> {
        if from_generation_id == to_generation_id {
            return Err(AtlasError::InvalidConfig(
                "GenerationDelta.from_generation_id must differ from to_generation_id".to_string(),
            ));
        }
        let identity = GenerationDeltaIdentity {
            schema_version: CONTEXT_SCHEMA_VERSION,
            delta_id: &delta_id,
            workspace_id: &workspace_id,
            from_generation_id: &from_generation_id,
            to_generation_id: &to_generation_id,
            delta_policy_version: &delta_policy_version,
            files: &files,
            symbols: &symbols,
            relationships: &relationships,
            effects: &effects,
            coverage: &coverage,
            conflicts: &conflicts,
            unchanged_verified_contracts: &unchanged_verified_contracts,
            uncertainty: &uncertainty,
        };
        let delta_hash = crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&identity));
        Ok(GenerationDelta {
            schema_version: CONTEXT_SCHEMA_VERSION.to_string(),
            delta_id,
            workspace_id,
            from_generation_id,
            to_generation_id,
            delta_policy_version,
            files,
            symbols,
            relationships,
            effects,
            coverage,
            conflicts,
            unchanged_verified_contracts,
            uncertainty,
            delta_hash,
        })
    }
}

// ---------------------------------------------------------------------------
// ContextIr
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrWorkspace {
    pub workspace_id: String,
    pub generation_id: String,
    pub generation_sequence: i64,
    #[serde(default)]
    pub source_tree_hash: Option<String>,
    pub configuration_hash: String,
    pub provider_set_hash: String,
    #[serde(default)]
    pub serving_generation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTask {
    pub task_session_id: String,
    pub task_hash: String,
    pub normalized_goal_hash: String,
    pub task_kind: TaskKind,
    pub kind_source: TaskKindSource,
    #[serde(default)]
    pub kind_rule_id: Option<String>,
    #[serde(default)]
    pub seed_paths: Vec<String>,
    #[serde(default)]
    pub seed_symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrPolicy {
    pub planner_policy_version: String,
    pub projection_policy_version: String,
    pub budget_profile: String,
    /// JSON Schema `const: true` — Context IR compilation is never
    /// non-deterministic (G6's "no learned ranking" bar); the field exists
    /// so a consumer can distinguish this from a future non-deterministic
    /// mode without guessing.
    pub deterministic: bool,
}

impl IrPolicy {
    pub fn new(
        planner_policy_version: String,
        projection_policy_version: String,
        budget_profile: String,
    ) -> Self {
        IrPolicy {
            planner_policy_version,
            projection_policy_version,
            budget_profile,
            deterministic: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrStatus {
    pub working_set_status: WorkingSetStatus,
    #[serde(default)]
    pub reasons: Vec<String>,
    pub serving_fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrEvidence {
    pub state: EvidenceQuality,
    pub confidence: f64,
    #[serde(default)]
    pub provider_fingerprint: Option<String>,
    #[serde(default)]
    pub source_revision_hash: Option<String>,
    pub generation_id: String,
    #[serde(default)]
    pub preferred: Option<bool>,
    #[serde(default)]
    pub alternative_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrItemCost {
    pub metadata_bytes: i64,
    pub source_bytes: i64,
    pub estimated_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrSourceRef {
    pub path: String,
    pub start_byte: i64,
    pub end_byte: i64,
    pub indexed_hash: String,
    #[serde(default)]
    pub observed_hash: Option<String>,
    pub verification_status: SourceVerificationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrWorkingSetItem {
    pub item_id: String,
    pub entity_kind: EntityKind,
    pub entity_id: String,
    pub role: ItemRole,
    pub selection_reason: SelectionReason,
    #[serde(default)]
    pub origin_id: Option<String>,
    pub distance: i64,
    pub rank: i64,
    pub evidence: IrEvidence,
    pub cost: IrItemCost,
    #[serde(default)]
    pub source: Option<IrSourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrRelationship {
    pub relationship_id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub relationship_type: String,
    /// Reuses `resolution::ResolutionStatus` — the same closed vocabulary
    /// (`resolved_symbol`/`resolved_file`/`external`/`ambiguous`/
    /// `unresolved`/`invalid`/`stale`) as `relationship_resolution.status`,
    /// so a Context IR relationship's resolution state can never diverge
    /// from the resolver's own domain (one canonical vocabulary, not two).
    pub resolution_state: crate::resolution::ResolutionStatus,
    pub evidence: IrEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrEffect {
    pub subject_entity_id: String,
    pub effect_type: String,
    pub phase: EffectPhase,
    pub evidence: IrEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrNotice {
    pub code: String,
    pub severity: NoticeSeverity,
    pub message: String,
    #[serde(default)]
    pub entity_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrCoverage {
    pub eligible: i64,
    pub complete: i64,
    pub partial: i64,
    pub unsupported: i64,
    pub excluded: i64,
    pub failed: i64,
    pub claim_strength: ClaimStrength,
    #[serde(default)]
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrOmissions {
    pub candidates_considered: i64,
    pub selected: i64,
    pub omitted: i64,
    pub truncated: bool,
    #[serde(default)]
    pub by_reason: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrValidation {
    pub kind: ValidationKind,
    pub target: String,
    pub reason: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrCost {
    pub max_records: i64,
    pub selected_records: i64,
    pub max_source_bytes: i64,
    pub selected_source_bytes: i64,
    pub max_estimated_tokens: i64,
    pub selected_estimated_tokens: i64,
    pub soft_latency_ms: i64,
    pub hard_latency_ms: i64,
    pub elapsed_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextIr {
    pub schema_version: String,
    pub context_id: String,
    pub workspace: IrWorkspace,
    pub task: IrTask,
    pub policy: IrPolicy,
    pub status: IrStatus,
    pub working_set: Vec<IrWorkingSetItem>,
    pub relationships: Vec<IrRelationship>,
    pub effects: Vec<IrEffect>,
    pub uncertainty: Vec<IrNotice>,
    pub coverage: IrCoverage,
    pub omissions: IrOmissions,
    pub validation_plan: Vec<IrValidation>,
    pub cost: IrCost,
    /// `sha256(canonical_json(everything above))`, populated by `seal`.
    pub context_hash: String,
}

#[derive(Serialize)]
struct ContextIrIdentity<'a> {
    schema_version: &'a str,
    context_id: &'a str,
    workspace: &'a IrWorkspace,
    task: &'a IrTask,
    policy: &'a IrPolicy,
    status: &'a IrStatus,
    working_set: &'a [IrWorkingSetItem],
    relationships: &'a [IrRelationship],
    effects: &'a [IrEffect],
    uncertainty: &'a [IrNotice],
    coverage: &'a IrCoverage,
    omissions: &'a IrOmissions,
    validation_plan: &'a [IrValidation],
    cost: &'a IrCost,
}

impl ContextIr {
    /// Build a Context IR document and stamp its deterministic
    /// `context_hash`. Two compilations against the same generation with
    /// the same request produce a byte-identical hash — the empirical
    /// backbone of G8's Context Yield harness ("repeatable frozen-generation
    /// experiments with identical outcomes").
    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        context_id: String,
        workspace: IrWorkspace,
        task: IrTask,
        policy: IrPolicy,
        status: IrStatus,
        working_set: Vec<IrWorkingSetItem>,
        relationships: Vec<IrRelationship>,
        effects: Vec<IrEffect>,
        uncertainty: Vec<IrNotice>,
        coverage: IrCoverage,
        omissions: IrOmissions,
        validation_plan: Vec<IrValidation>,
        cost: IrCost,
    ) -> Self {
        let identity = ContextIrIdentity {
            schema_version: CONTEXT_SCHEMA_VERSION,
            context_id: &context_id,
            workspace: &workspace,
            task: &task,
            policy: &policy,
            status: &status,
            working_set: &working_set,
            relationships: &relationships,
            effects: &effects,
            uncertainty: &uncertainty,
            coverage: &coverage,
            omissions: &omissions,
            validation_plan: &validation_plan,
            cost: &cost,
        };
        let context_hash = crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&identity));
        ContextIr {
            schema_version: CONTEXT_SCHEMA_VERSION.to_string(),
            context_id,
            workspace,
            task,
            policy,
            status,
            working_set,
            relationships,
            effects,
            uncertainty,
            coverage,
            omissions,
            validation_plan,
            cost,
            context_hash,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal ATLAS_DEEP Context IR 2.0.0
// ---------------------------------------------------------------------------

/// Closed semantic outcome for the V2 required-role policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrRequirementStateV2 {
    Satisfied,
    Missing,
    Unresolved,
    Stale,
    Unavailable,
    Unsupported,
    Conflicting,
    BudgetOmitted,
}

/// Closed, countable reasons shared by V2 sufficiency, uncertainty, coverage,
/// and omission ledgers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrOmissionReasonV2 {
    MissingRequiredRole,
    UnresolvedEvidence,
    StaleEvidence,
    UnavailableEvidence,
    UnsupportedEvidence,
    ConflictingEvidence,
    AmbiguousSeed,
    FtsCandidateLimit,
    RecordBudget,
    SourceByteBudget,
    EstimatedTokenBudget,
    RelationshipDepthBudget,
    WorkUnitBudget,
    UncertaintyReserve,
    TemporalEvidenceUnavailable,
    ValidationTargetUnavailable,
    SourceVerificationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrPathConfinementV2 {
    Confined,
    OutsideCanonicalRoot,
    SymlinkEscape,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrSourceVerificationV2 {
    Verified,
    Stale,
    Unavailable,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrTemporalStateV2 {
    NotRequired,
    VerifiedAncestor,
    Unavailable,
    InvalidBaseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrValidationReasonV2 {
    TaskRecipe,
    AffectedTest,
    AffectedConfiguration,
    AffectedBuildContract,
    RequiredRoleClosure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrLeaseDependencyKindV2 {
    SourceDigest,
    Relationship,
    Effect,
    ProviderFingerprint,
    Coverage,
    Conflict,
    PlannerPolicy,
    ProjectionPolicy,
    Estimator,
}

impl IrLeaseDependencyKindV2 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SourceDigest => "source_digest",
            Self::Relationship => "relationship",
            Self::Effect => "effect",
            Self::ProviderFingerprint => "provider_fingerprint",
            Self::Coverage => "coverage",
            Self::Conflict => "conflict",
            Self::PlannerPolicy => "planner_policy",
            Self::ProjectionPolicy => "projection_policy",
            Self::Estimator => "estimator",
        }
    }
}

fn deserialize_required_option<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrWorkspaceV2 {
    pub workspace_id: String,
    pub generation_id: String,
    pub generation_sequence: i64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub source_tree_hash: Option<String>,
    pub configuration_hash: String,
    pub provider_set_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTaskV2 {
    pub task_session_id: String,
    pub task_hash: String,
    pub normalized_goal_hash: String,
    pub task_kind: TaskKind,
    pub kind_source: TaskKindSource,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub kind_rule_id: Option<String>,
    pub seed_paths: Vec<String>,
    pub seed_symbols: Vec<String>,
}

/// Every V2 semantic limit is explicit and independently represented. T24
/// supplies and enforces these values; T11 freezes their wire positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrSemanticBudgetV2 {
    pub max_records: u64,
    pub max_source_bytes: u64,
    pub max_estimated_tokens: u64,
    pub max_relationship_depth: u32,
    pub max_work_units: u64,
    pub uncertainty_reserve_percent: u32,
    pub budget_digest: String,
}

impl IrSemanticBudgetV2 {
    pub(crate) fn from_explicit_budget(
        supplied: &crate::context_route::DeepSemanticBudget,
    ) -> Result<Self> {
        supplied.validate().map_err(|error| {
            AtlasError::InvalidConfig(format!("invalid explicit V2 semantic budget: {error}"))
        })?;
        let mut budget = Self {
            max_records: supplied.max_records,
            max_source_bytes: supplied.max_source_bytes,
            max_estimated_tokens: supplied.max_estimated_tokens,
            max_relationship_depth: supplied.max_depth,
            max_work_units: supplied.max_work_units,
            uncertainty_reserve_percent: u32::from(supplied.uncertainty_reserve_percent),
            budget_digest: String::new(),
        };
        budget.budget_digest = budget.expected_digest();
        Ok(budget)
    }

    fn expected_digest(&self) -> String {
        let mut encoder = IrCanonicalEncoder::new("workspace-atlas/context-ir-v2-semantic-budget");
        encoder.u64(self.max_records);
        encoder.u64(self.max_source_bytes);
        encoder.u64(self.max_estimated_tokens);
        encoder.u64(u64::from(self.max_relationship_depth));
        encoder.u64(self.max_work_units);
        encoder.u64(u64::from(self.uncertainty_reserve_percent));
        encoder.finish()
    }
}

pub(crate) fn uncertainty_reserve_records(
    max_records: u64,
    uncertainty_reserve_percent: u32,
) -> Result<u64> {
    let reserved = u128::from(max_records)
        .checked_mul(u128::from(uncertainty_reserve_percent))
        .map(|value| value.div_ceil(100))
        .ok_or_else(|| {
            AtlasError::InvalidConfig("deep Context IR uncertainty reserve overflow".into())
        })?;
    u64::try_from(reserved).map_err(|_| {
        AtlasError::InvalidConfig("deep Context IR uncertainty reserve exceeds record bound".into())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrPolicyV2 {
    pub planner_policy_version: String,
    pub projection_policy_version: String,
    pub estimator_version: String,
    pub deterministic: bool,
    pub semantic_budget: IrSemanticBudgetV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrRoleSufficiencyV2 {
    pub role: ItemRole,
    pub required: bool,
    pub state: IrRequirementStateV2,
    pub evidence_item_ids: Vec<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub reason: Option<IrOmissionReasonV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrStatusV2 {
    pub working_set_status: WorkingSetStatus,
    pub role_sufficiency: Vec<IrRoleSufficiencyV2>,
    pub exact_source: IrRequirementStateV2,
    pub validation: IrRequirementStateV2,
    pub reasons: Vec<IrOmissionReasonV2>,
}

/// Verified source authority is a digest/range reference only. Source bytes,
/// authorization, and response-lifetime materialization remain in the
/// separately versioned execution envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrSourceRefV2 {
    pub canonical_path: String,
    pub whole_file_sha256: String,
    pub start_byte: u64,
    pub end_byte: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub observed_sha256: Option<String>,
    pub verification_status: IrSourceVerificationV2,
    pub canonical_root_confinement: IrPathConfinementV2,
    pub revalidated_before_seal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrItemCostV2 {
    pub metadata_bytes: u64,
    pub source_bytes: u64,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrWorkingSetItemV2 {
    pub item_id: String,
    pub entity_kind: EntityKind,
    pub entity_id: String,
    pub role: ItemRole,
    pub selection_reason: SelectionReason,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub origin_id: Option<String>,
    pub distance: u32,
    pub rank: u64,
    pub evidence: IrEvidence,
    pub cost: IrItemCostV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub source: Option<IrSourceRefV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTemporalConstraintsV2 {
    pub current_generation_id: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub baseline_generation_id: Option<String>,
    pub state: IrTemporalStateV2,
    pub evidence_item_ids: Vec<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub omission_reason: Option<IrOmissionReasonV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrNoticeV2 {
    pub code: IrOmissionReasonV2,
    pub severity: NoticeSeverity,
    pub entity_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrCoverageV2 {
    pub eligible: u64,
    pub complete: u64,
    pub partial: u64,
    pub unsupported: u64,
    pub excluded: u64,
    pub failed: u64,
    pub claim_strength: ClaimStrength,
    pub deficits: Vec<IrOmissionReasonV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrOmissionsV2 {
    pub candidates_considered: u64,
    pub selected: u64,
    pub omitted: u64,
    pub truncated: bool,
    pub by_reason: std::collections::BTreeMap<IrOmissionReasonV2, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrValidationV2 {
    pub kind: ValidationKind,
    pub target_entity_id: String,
    pub reason: IrValidationReasonV2,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrLeaseDependencyV2 {
    pub kind: IrLeaseDependencyKindV2,
    pub key: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrEvidenceLeaseV2 {
    pub generation_id: String,
    pub dependencies: Vec<IrLeaseDependencyV2>,
}

/// Deterministic semantic work only. Wall time, deadlines, retries, attempts,
/// cache state, and cancellation are deliberately unrepresentable here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrCostV2 {
    pub selected_records: u64,
    pub selected_source_bytes: u64,
    pub selected_estimated_tokens: u64,
    pub relationship_depth_reached: u32,
    pub work_units_consumed: u64,
    pub uncertainty_records_reserved: u64,
    pub uncertainty_records_selected: u64,
}

/// In-memory/application-only semantic result for ATLAS_DEEP. No persistence
/// API accepts this type before the H7 durability gate.
#[derive(Debug, Clone)]
pub struct DeepContextIrV2 {
    pub schema_version: String,
    pub context_id: String,
    pub workspace: IrWorkspaceV2,
    pub task: IrTaskV2,
    pub policy: IrPolicyV2,
    pub status: IrStatusV2,
    pub working_set: Vec<IrWorkingSetItemV2>,
    pub relationships: Vec<IrRelationship>,
    pub effects: Vec<IrEffect>,
    pub temporal_constraints: IrTemporalConstraintsV2,
    pub uncertainty: Vec<IrNoticeV2>,
    pub coverage: IrCoverageV2,
    pub omissions: IrOmissionsV2,
    pub validation_plan: Vec<IrValidationV2>,
    pub evidence_lease: IrEvidenceLeaseV2,
    pub cost: IrCostV2,
    pub context_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeepContextIrV2Wire {
    schema_version: String,
    context_id: String,
    workspace: IrWorkspaceV2,
    task: IrTaskV2,
    policy: IrPolicyV2,
    status: IrStatusV2,
    working_set: Vec<IrWorkingSetItemV2>,
    relationships: Vec<IrRelationship>,
    effects: Vec<IrEffect>,
    temporal_constraints: IrTemporalConstraintsV2,
    uncertainty: Vec<IrNoticeV2>,
    coverage: IrCoverageV2,
    omissions: IrOmissionsV2,
    validation_plan: Vec<IrValidationV2>,
    evidence_lease: IrEvidenceLeaseV2,
    cost: IrCostV2,
    context_hash: String,
}

#[derive(Serialize)]
struct DeepContextIrV2WireRef<'a> {
    schema_version: &'a str,
    context_id: &'a str,
    workspace: &'a IrWorkspaceV2,
    task: &'a IrTaskV2,
    policy: &'a IrPolicyV2,
    status: &'a IrStatusV2,
    working_set: &'a [IrWorkingSetItemV2],
    relationships: &'a [IrRelationship],
    effects: &'a [IrEffect],
    temporal_constraints: &'a IrTemporalConstraintsV2,
    uncertainty: &'a [IrNoticeV2],
    coverage: &'a IrCoverageV2,
    omissions: &'a IrOmissionsV2,
    validation_plan: &'a [IrValidationV2],
    evidence_lease: &'a IrEvidenceLeaseV2,
    cost: &'a IrCostV2,
    context_hash: &'a str,
}

impl From<DeepContextIrV2Wire> for DeepContextIrV2 {
    fn from(wire: DeepContextIrV2Wire) -> Self {
        Self {
            schema_version: wire.schema_version,
            context_id: wire.context_id,
            workspace: wire.workspace,
            task: wire.task,
            policy: wire.policy,
            status: wire.status,
            working_set: wire.working_set,
            relationships: wire.relationships,
            effects: wire.effects,
            temporal_constraints: wire.temporal_constraints,
            uncertainty: wire.uncertainty,
            coverage: wire.coverage,
            omissions: wire.omissions,
            validation_plan: wire.validation_plan,
            evidence_lease: wire.evidence_lease,
            cost: wire.cost,
            context_hash: wire.context_hash,
        }
    }
}

impl Serialize for DeepContextIrV2 {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        DeepContextIrV2WireRef {
            schema_version: &self.schema_version,
            context_id: &self.context_id,
            workspace: &self.workspace,
            task: &self.task,
            policy: &self.policy,
            status: &self.status,
            working_set: &self.working_set,
            relationships: &self.relationships,
            effects: &self.effects,
            temporal_constraints: &self.temporal_constraints,
            uncertainty: &self.uncertainty,
            coverage: &self.coverage,
            omissions: &self.omissions,
            validation_plan: &self.validation_plan,
            evidence_lease: &self.evidence_lease,
            cost: &self.cost,
            context_hash: &self.context_hash,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeepContextIrV2 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let document = Self::from(DeepContextIrV2Wire::deserialize(deserializer)?);
        document.validate().map_err(serde::de::Error::custom)?;
        Ok(document)
    }
}

#[derive(Debug, Clone)]
pub enum ContextIrDocument {
    LegacyV1(ContextIr),
    DeepV2(DeepContextIrV2),
}

#[derive(Serialize)]
struct IrTaskV2Identity<'a> {
    task_hash: &'a str,
    normalized_goal_hash: &'a str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<&'a str>,
    seed_paths: &'a [String],
    seed_symbols: &'a [String],
}

#[derive(Serialize)]
struct DeepContextIrV2Identity<'a> {
    schema_version: &'a str,
    workspace: &'a IrWorkspaceV2,
    task: IrTaskV2Identity<'a>,
    policy: &'a IrPolicyV2,
    status: &'a IrStatusV2,
    working_set: &'a [IrWorkingSetItemV2],
    relationships: &'a [IrRelationship],
    effects: &'a [IrEffect],
    temporal_constraints: &'a IrTemporalConstraintsV2,
    uncertainty: &'a [IrNoticeV2],
    coverage: &'a IrCoverageV2,
    omissions: &'a IrOmissionsV2,
    validation_plan: &'a [IrValidationV2],
    evidence_lease: &'a IrEvidenceLeaseV2,
    cost: &'a IrCostV2,
}

struct IrCanonicalEncoder {
    hasher: sha2::Sha256,
}

impl IrCanonicalEncoder {
    fn new(domain: &str) -> Self {
        use sha2::Digest;
        let mut value = Self {
            hasher: sha2::Sha256::new(),
        };
        value.bytes(domain.as_bytes());
        value
    }

    fn bytes(&mut self, value: &[u8]) {
        use sha2::Digest;
        self.hasher.update((value.len() as u64).to_be_bytes());
        self.hasher.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    fn finish(self) -> String {
        use sha2::Digest;
        hex::encode(self.hasher.finalize())
    }
}

impl DeepContextIrV2 {
    fn identity(&self) -> DeepContextIrV2Identity<'_> {
        DeepContextIrV2Identity {
            schema_version: &self.schema_version,
            workspace: &self.workspace,
            task: IrTaskV2Identity {
                task_hash: &self.task.task_hash,
                normalized_goal_hash: &self.task.normalized_goal_hash,
                task_kind: self.task.task_kind,
                kind_source: self.task.kind_source,
                kind_rule_id: self.task.kind_rule_id.as_deref(),
                seed_paths: &self.task.seed_paths,
                seed_symbols: &self.task.seed_symbols,
            },
            policy: &self.policy,
            status: &self.status,
            working_set: &self.working_set,
            relationships: &self.relationships,
            effects: &self.effects,
            temporal_constraints: &self.temporal_constraints,
            uncertainty: &self.uncertainty,
            coverage: &self.coverage,
            omissions: &self.omissions,
            validation_plan: &self.validation_plan,
            evidence_lease: &self.evidence_lease,
            cost: &self.cost,
        }
    }

    fn expected_hash(&self) -> String {
        let mut encoder = IrCanonicalEncoder::new("workspace-atlas/context-ir-v2");
        // Contract and planner versions are intentionally encoded first.
        encoder.text(&self.schema_version);
        encoder.text(&self.policy.planner_policy_version);
        encoder.bytes(&canonical_json_bytes(&self.identity()));
        encoder.finish()
    }

    /// Stamp the fixed budget and Context IR canonical identities.
    pub fn seal(mut self) -> Result<Self> {
        self.policy.semantic_budget.budget_digest = self.policy.semantic_budget.expected_digest();
        self.context_hash = self.expected_hash();
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != CONTEXT_SCHEMA_V2_VERSION {
            return Err(AtlasError::InvalidConfig(format!(
                "unsupported deep Context IR version {}; supported: {}",
                self.schema_version, CONTEXT_SCHEMA_V2_VERSION
            )));
        }
        if self.policy.planner_policy_version != crate::task_compiler::PLANNER_POLICY_V2_VERSION
            || self.policy.projection_policy_version
                != crate::context_route::DEEP_PROJECTION_VERSION
            || self.policy.estimator_version != crate::context_route::DEEP_ESTIMATOR_VERSION
            || !self.policy.deterministic
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR 2.0.0 requires its fixed planner, projection, and estimator"
                    .into(),
            ));
        }
        if self.context_id.is_empty()
            || self.workspace.workspace_id.is_empty()
            || self.workspace.generation_id.is_empty()
            || self.task.task_session_id.is_empty()
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR contains an empty semantic identity".into(),
            ));
        }
        let budget = &self.policy.semantic_budget;
        if budget.max_records == 0
            || budget.max_records > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS
            || budget.max_source_bytes == 0
            || budget.max_source_bytes > crate::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES
            || budget.max_estimated_tokens == 0
            || budget.max_estimated_tokens
                > crate::context_metrics::MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS
            || budget.max_relationship_depth == 0
            || budget.max_relationship_depth > crate::context_route::MAX_DEEP_DEPTH
            || budget.max_work_units == 0
            || budget.max_work_units > crate::context_metrics::MAX_CONTEXT_EXECUTION_WORK_UNITS
            || budget.uncertainty_reserve_percent == 0
            || budget.uncertainty_reserve_percent > 100
            || budget.budget_digest != budget.expected_digest()
        {
            return Err(AtlasError::InvalidConfig(
                "invalid deep Context IR semantic budget identity".into(),
            ));
        }
        let expected_uncertainty_reserve =
            uncertainty_reserve_records(budget.max_records, budget.uncertainty_reserve_percent)?;
        for digest in [
            Some(self.workspace.configuration_hash.as_str()),
            Some(self.workspace.provider_set_hash.as_str()),
            self.workspace.source_tree_hash.as_deref(),
            Some(self.task.task_hash.as_str()),
            Some(self.task.normalized_goal_hash.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            if !is_hex64(digest) {
                return Err(AtlasError::InvalidConfig(
                    "deep Context IR contains an invalid workspace digest".into(),
                ));
            }
        }
        if self.temporal_constraints.current_generation_id != self.workspace.generation_id
            || self.evidence_lease.generation_id != self.workspace.generation_id
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR generation-bound sections disagree".into(),
            ));
        }
        let valid_evidence = |evidence: &IrEvidence| {
            evidence.generation_id == self.workspace.generation_id
                && evidence.confidence.is_finite()
                && (0.0..=1.0).contains(&evidence.confidence)
                && evidence
                    .provider_fingerprint
                    .as_deref()
                    .is_none_or(is_hex64)
                && evidence
                    .source_revision_hash
                    .as_deref()
                    .is_none_or(is_hex64)
        };
        let seeds_are_canonical = self
            .task
            .seed_paths
            .windows(2)
            .all(|pair| pair[0] < pair[1])
            && self
                .task
                .seed_symbols
                .windows(2)
                .all(|pair| pair[0] < pair[1]);
        if !seeds_are_canonical {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR seeds are not sorted and deduplicated".into(),
            ));
        }
        for (index, item) in self.working_set.iter().enumerate() {
            if item.item_id.is_empty()
                || item.entity_id.is_empty()
                || item.rank != index as u64
                || item.distance > budget.max_relationship_depth
                || self.working_set[..index]
                    .iter()
                    .any(|prior| prior.item_id == item.item_id)
                || !valid_evidence(&item.evidence)
            {
                return Err(AtlasError::InvalidConfig(
                    "deep Context IR selected evidence identity is invalid".into(),
                ));
            }
            if let Some(source) = &item.source {
                if source.canonical_path.is_empty()
                    || source.start_byte >= source.end_byte
                    || source.end_byte - source.start_byte != item.cost.source_bytes
                    || !is_hex64(&source.whole_file_sha256)
                    || source
                        .observed_sha256
                        .as_deref()
                        .is_some_and(|digest| !is_hex64(digest))
                {
                    return Err(AtlasError::InvalidConfig(
                        "deep Context IR contains an invalid source reference".into(),
                    ));
                }
                let live_proof_valid = match source.verification_status {
                    IrSourceVerificationV2::Verified => {
                        source.observed_sha256.as_deref() == Some(source.whole_file_sha256.as_str())
                            && source.canonical_root_confinement == IrPathConfinementV2::Confined
                            && source.revalidated_before_seal
                    }
                    IrSourceVerificationV2::Stale => {
                        source
                            .observed_sha256
                            .as_deref()
                            .is_some_and(|observed| observed != source.whole_file_sha256)
                            && source.canonical_root_confinement == IrPathConfinementV2::Confined
                            && source.revalidated_before_seal
                    }
                    IrSourceVerificationV2::Unavailable => source.revalidated_before_seal,
                    IrSourceVerificationV2::Unsupported => !source.revalidated_before_seal,
                };
                if !live_proof_valid {
                    return Err(AtlasError::InvalidConfig(
                        "V2 source reference contradicts its live verification proof".into(),
                    ));
                }
            } else if item.cost.source_bytes != 0 {
                return Err(AtlasError::InvalidConfig(
                    "source-byte cost lacks a source reference".into(),
                ));
            }
        }
        if self
            .relationships
            .iter()
            .any(|relationship| !valid_evidence(&relationship.evidence))
            || self
                .effects
                .iter()
                .any(|effect| !valid_evidence(&effect.evidence))
            || self
                .evidence_lease
                .dependencies
                .iter()
                .any(|dependency| dependency.key.is_empty() || !is_hex64(&dependency.digest))
            || self.evidence_lease.dependencies.windows(2).any(|pair| {
                (pair[0].kind.as_str(), pair[0].key.as_str())
                    >= (pair[1].kind.as_str(), pair[1].key.as_str())
            })
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR contains invalid qualified evidence".into(),
            ));
        }
        let selected_source_bytes = self.working_set.iter().try_fold(0_u64, |total, item| {
            total.checked_add(item.cost.source_bytes)
        });
        let selected_estimated_tokens = self.working_set.iter().try_fold(0_u64, |total, item| {
            total.checked_add(item.cost.estimated_tokens)
        });
        let selected_records = u64::try_from(self.working_set.len()).map_err(|_| {
            AtlasError::InvalidConfig("deep Context IR selected record count overflow".into())
        })?;
        let selected_uncertainty_records = u64::try_from(
            self.working_set
                .iter()
                .filter(|item| item.role == ItemRole::Uncertainty)
                .count(),
        )
        .map_err(|_| {
            AtlasError::InvalidConfig("deep Context IR uncertainty record count overflow".into())
        })?;
        let selected_relationship_depth = self
            .working_set
            .iter()
            .map(|item| item.distance)
            .max()
            .unwrap_or(0);
        if selected_source_bytes != Some(self.cost.selected_source_bytes)
            || selected_estimated_tokens != Some(self.cost.selected_estimated_tokens)
            || self.cost.selected_records != selected_records
            || self.cost.selected_records > budget.max_records
            || self.cost.selected_source_bytes > budget.max_source_bytes
            || self.cost.selected_estimated_tokens > budget.max_estimated_tokens
            || self.cost.relationship_depth_reached != selected_relationship_depth
            || self.cost.relationship_depth_reached > budget.max_relationship_depth
            || self.cost.work_units_consumed > budget.max_work_units
            || self.cost.uncertainty_records_selected != selected_uncertainty_records
            || self.cost.uncertainty_records_selected > self.cost.uncertainty_records_reserved
            || self.cost.uncertainty_records_reserved != expected_uncertainty_reserve
            || self.cost.selected_records - self.cost.uncertainty_records_selected
                > budget.max_records - expected_uncertainty_reserve
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR semantic cost ledger does not reconcile".into(),
            ));
        }
        let coverage_total = self
            .coverage
            .complete
            .checked_add(self.coverage.partial)
            .and_then(|total| total.checked_add(self.coverage.unsupported))
            .and_then(|total| total.checked_add(self.coverage.excluded))
            .and_then(|total| total.checked_add(self.coverage.failed));
        if coverage_total != Some(self.coverage.eligible) {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR coverage ledger does not reconcile".into(),
            ));
        }
        let omitted_total = self
            .omissions
            .by_reason
            .values()
            .try_fold(0_u64, |total, count| total.checked_add(*count));
        let work_unit_omissions = self
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::WorkUnitBudget)
            .copied()
            .unwrap_or(0);
        let charged_candidates = self
            .omissions
            .candidates_considered
            .checked_sub(work_unit_omissions);
        let ordinary_records_selected =
            self.cost.selected_records - self.cost.uncertainty_records_selected;
        let record_budget_was_exhausted = self
            .omissions
            .by_reason
            .contains_key(&IrOmissionReasonV2::RecordBudget);
        let uncertainty_reserve_was_exhausted = self
            .omissions
            .by_reason
            .contains_key(&IrOmissionReasonV2::UncertaintyReserve);
        if omitted_total != Some(self.omissions.omitted)
            || self.omissions.selected != selected_records
            || self.omissions.candidates_considered
                > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS
            || Some(self.omissions.candidates_considered)
                != self.omissions.selected.checked_add(self.omissions.omitted)
            || self.omissions.truncated != (self.omissions.omitted != 0)
            || self.omissions.by_reason.values().any(|count| *count == 0)
            || charged_candidates.is_none_or(|count| self.cost.work_units_consumed < count)
            || (record_budget_was_exhausted
                && ordinary_records_selected != budget.max_records - expected_uncertainty_reserve)
            || (uncertainty_reserve_was_exhausted
                && self.cost.uncertainty_records_selected != expected_uncertainty_reserve)
        {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR omission ledger does not reconcile".into(),
            ));
        }
        let strings_are_strictly_sorted =
            |values: &[String]| values.windows(2).all(|pair| pair[0] < pair[1]);
        let canonical_collections = self.status.reasons.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .coverage
                .deficits
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self
                .status
                .role_sufficiency
                .iter()
                .all(|entry| strings_are_strictly_sorted(&entry.evidence_item_ids))
            && strings_are_strictly_sorted(&self.temporal_constraints.evidence_item_ids)
            && self
                .relationships
                .windows(2)
                .all(|pair| pair[0].relationship_id < pair[1].relationship_id)
            && self.effects.windows(2).all(|pair| {
                (&pair[0].subject_entity_id, &pair[0].effect_type)
                    < (&pair[1].subject_entity_id, &pair[1].effect_type)
            })
            && self
                .uncertainty
                .windows(2)
                .all(|pair| pair[0].code < pair[1].code)
            && self
                .uncertainty
                .iter()
                .all(|notice| strings_are_strictly_sorted(&notice.entity_ids))
            && self
                .validation_plan
                .windows(2)
                .all(|pair| pair[0].target_entity_id < pair[1].target_entity_id)
            && self
                .evidence_lease
                .dependencies
                .windows(2)
                .all(|pair| pair[0].key < pair[1].key);
        if !canonical_collections {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR collections are not canonically ordered".into(),
            ));
        }
        let recipe = crate::task_compiler::deep_v2_role_recipe(self.task.task_kind);
        let matrix_matches_policy = self.status.role_sufficiency.len()
            == recipe.role_matrix().count()
            && self
                .status
                .role_sufficiency
                .iter()
                .zip(recipe.role_matrix())
                .all(|(entry, expected)| entry.role == expected.0 && entry.required == expected.1);
        if !matrix_matches_policy {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR role matrix does not match planner-v2.0.0".into(),
            ));
        }
        let qualified_state = |state: EvidenceQuality| {
            matches!(
                state,
                EvidenceQuality::Verified | EvidenceQuality::Supported | EvidenceQuality::Resolved
            )
        };
        let reason_matches_state =
            |state: IrRequirementStateV2, reason: Option<IrOmissionReasonV2>| match state {
                IrRequirementStateV2::Satisfied => reason.is_none(),
                IrRequirementStateV2::Missing => {
                    reason == Some(IrOmissionReasonV2::MissingRequiredRole)
                }
                IrRequirementStateV2::Unresolved => {
                    reason == Some(IrOmissionReasonV2::UnresolvedEvidence)
                }
                IrRequirementStateV2::Stale => {
                    matches!(
                        reason,
                        Some(
                            IrOmissionReasonV2::StaleEvidence
                                | IrOmissionReasonV2::SourceVerificationFailed
                        )
                    )
                }
                IrRequirementStateV2::Unavailable => {
                    matches!(
                        reason,
                        Some(
                            IrOmissionReasonV2::UnavailableEvidence
                                | IrOmissionReasonV2::TemporalEvidenceUnavailable
                                | IrOmissionReasonV2::ValidationTargetUnavailable
                        )
                    )
                }
                IrRequirementStateV2::Unsupported => {
                    reason == Some(IrOmissionReasonV2::UnsupportedEvidence)
                }
                IrRequirementStateV2::Conflicting => {
                    reason == Some(IrOmissionReasonV2::ConflictingEvidence)
                }
                IrRequirementStateV2::BudgetOmitted => matches!(
                    reason,
                    Some(
                        IrOmissionReasonV2::RecordBudget
                            | IrOmissionReasonV2::SourceByteBudget
                            | IrOmissionReasonV2::EstimatedTokenBudget
                            | IrOmissionReasonV2::RelationshipDepthBudget
                            | IrOmissionReasonV2::WorkUnitBudget
                            | IrOmissionReasonV2::UncertaintyReserve
                    )
                ),
            };
        let role_evidence_is_valid = self.status.role_sufficiency.iter().all(|entry| {
            let referenced_items_match = entry.evidence_item_ids.iter().all(|item_id| {
                self.working_set
                    .iter()
                    .any(|item| item.item_id == *item_id && item.role == entry.role)
            });
            reason_matches_state(entry.state, entry.reason)
                && referenced_items_match
                && (entry.state != IrRequirementStateV2::Satisfied
                    || (!entry.evidence_item_ids.is_empty()
                        && entry.evidence_item_ids.iter().all(|item_id| {
                            self.working_set.iter().any(|item| {
                                item.item_id == *item_id && qualified_state(item.evidence.state)
                            })
                        })))
        });
        let role_entry = |role: ItemRole| {
            self.status
                .role_sufficiency
                .iter()
                .find(|entry| entry.role == role)
        };
        let primary_entry = role_entry(ItemRole::PrimaryImplementation);
        let has_verified_exact_source = primary_entry.is_some_and(|entry| {
            entry.evidence_item_ids.iter().any(|item_id| {
                self.working_set.iter().any(|item| {
                    item.item_id == *item_id
                        && item.source.as_ref().is_some_and(|source| {
                            source.verification_status == IrSourceVerificationV2::Verified
                                && source.canonical_root_confinement
                                    == IrPathConfinementV2::Confined
                                && source.revalidated_before_seal
                        })
                })
            })
        });
        let selected_sources_current = self
            .working_set
            .iter()
            .filter_map(|item| item.source.as_ref())
            .all(|source| {
                source.verification_status == IrSourceVerificationV2::Verified
                    && source.canonical_root_confinement == IrPathConfinementV2::Confined
                    && source.revalidated_before_seal
            });
        let validation_witness = role_entry(ItemRole::ValidationTarget).is_some_and(|entry| {
            self.validation_plan.iter().any(|validation| {
                validation.required
                    && !validation.target_entity_id.is_empty()
                    && entry.evidence_item_ids.iter().any(|item_id| {
                        self.working_set.iter().any(|item| {
                            item.item_id == *item_id
                                && item.entity_id == validation.target_entity_id
                        })
                    })
            })
        });
        let effect_witness = role_entry(ItemRole::Effect).is_some_and(|entry| {
            self.effects.iter().any(|effect| {
                qualified_state(effect.evidence.state)
                    && entry.evidence_item_ids.iter().any(|item_id| {
                        self.working_set.iter().any(|item| {
                            item.item_id == *item_id && item.entity_id == effect.subject_entity_id
                        })
                    })
            })
        });
        let historical_witness = role_entry(ItemRole::HistoricalConstraint).is_some_and(|entry| {
            self.temporal_constraints.state == IrTemporalStateV2::VerifiedAncestor
                && self.temporal_constraints.baseline_generation_id.is_some()
                && self.temporal_constraints.omission_reason.is_none()
                && !self.temporal_constraints.evidence_item_ids.is_empty()
                && self
                    .temporal_constraints
                    .evidence_item_ids
                    .iter()
                    .all(|item_id| entry.evidence_item_ids.contains(item_id))
        });
        let temporal_references_exist = self
            .temporal_constraints
            .evidence_item_ids
            .iter()
            .all(|item_id| self.working_set.iter().any(|item| item.item_id == *item_id));
        let exact_status_matches = (self.status.exact_source == IrRequirementStateV2::Satisfied)
            == has_verified_exact_source;
        let validation_status_matches =
            (self.status.validation == IrRequirementStateV2::Satisfied) == validation_witness;
        let effect_status_matches = role_entry(ItemRole::Effect)
            .is_none_or(|entry| entry.state != IrRequirementStateV2::Satisfied || effect_witness);
        let historical_status_matches =
            role_entry(ItemRole::HistoricalConstraint).is_none_or(|entry| {
                entry.state != IrRequirementStateV2::Satisfied || historical_witness
            });
        let semantic_sections_match_status = role_evidence_is_valid
            && exact_status_matches
            && validation_status_matches
            && (self.status.working_set_status != WorkingSetStatus::Complete
                || selected_sources_current)
            && effect_status_matches
            && historical_status_matches
            && temporal_references_exist;
        if !semantic_sections_match_status {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR sufficiency is not backed by qualified evidence".into(),
            ));
        }
        let required_roles = self
            .status
            .role_sufficiency
            .iter()
            .filter(|role| role.required);
        let required_count = required_roles.clone().count();
        let satisfied_count = required_roles
            .clone()
            .filter(|role| role.state == IrRequirementStateV2::Satisfied)
            .count();
        let required_cross_cutting_satisfied = (!recipe.requires_exact_source
            || self.status.exact_source == IrRequirementStateV2::Satisfied)
            && (!recipe.requires_validation
                || self.status.validation == IrRequirementStateV2::Satisfied);
        let is_required_deficit = |reason: &IrOmissionReasonV2| {
            matches!(
                reason,
                IrOmissionReasonV2::MissingRequiredRole
                    | IrOmissionReasonV2::SourceVerificationFailed
                    | IrOmissionReasonV2::ValidationTargetUnavailable
            ) || (*reason == IrOmissionReasonV2::TemporalEvidenceUnavailable
                && recipe
                    .required_roles
                    .contains(&ItemRole::HistoricalConstraint))
        };
        let complete_has_no_required_deficits =
            !self.omissions.by_reason.keys().any(&is_required_deficit)
                && !self.coverage.deficits.iter().any(&is_required_deficit)
                && !self
                    .uncertainty
                    .iter()
                    .map(|notice| &notice.code)
                    .any(&is_required_deficit);
        let required_reasons_reported = required_roles
            .filter(|role| role.state != IrRequirementStateV2::Satisfied)
            .all(|role| {
                role.reason
                    .is_some_and(|reason| self.status.reasons.contains(&reason))
            })
            && (!recipe.requires_exact_source
                || self.status.exact_source == IrRequirementStateV2::Satisfied
                || self
                    .status
                    .reasons
                    .contains(&IrOmissionReasonV2::SourceVerificationFailed))
            && (!recipe.requires_validation
                || self.status.validation == IrRequirementStateV2::Satisfied
                || self
                    .status
                    .reasons
                    .contains(&IrOmissionReasonV2::ValidationTargetUnavailable));
        let primary_is_safe = primary_entry.is_some_and(|entry| {
            entry.state == IrRequirementStateV2::Satisfied
                && (!recipe.requires_exact_source
                    || self.status.exact_source == IrRequirementStateV2::Satisfied)
        });
        let all_requirements_satisfied =
            satisfied_count == required_count && required_cross_cutting_satisfied;
        let has_selected_source_deficit = self
            .uncertainty
            .iter()
            .find(|notice| notice.code == IrOmissionReasonV2::SourceVerificationFailed)
            .is_some_and(|notice| {
                !notice.entity_ids.is_empty()
                    && notice.entity_ids.iter().all(|entity_id| {
                        self.working_set
                            .iter()
                            .any(|item| item.entity_id == *entity_id)
                    })
            });
        let status_valid = match self.status.working_set_status {
            WorkingSetStatus::Complete => {
                all_requirements_satisfied
                    && self.status.reasons.is_empty()
                    && complete_has_no_required_deficits
            }
            WorkingSetStatus::Partial => {
                primary_is_safe
                    && (!all_requirements_satisfied || has_selected_source_deficit)
                    && !self.status.reasons.is_empty()
                    && required_reasons_reported
            }
            WorkingSetStatus::Blocked => {
                !primary_is_safe && !self.status.reasons.is_empty() && required_reasons_reported
            }
        };
        if !status_valid {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR status contradicts required-role sufficiency".into(),
            ));
        }
        if !is_hex64(&self.context_hash) || self.context_hash != self.expected_hash() {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR canonical hash mismatch".into(),
            ));
        }
        Ok(())
    }
}

/// Explicit legacy reader. It never accepts or interprets V2 semantics.
pub fn read_context_ir_v1(raw: &str) -> Result<ContextIr> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some(CONTEXT_SCHEMA_VERSION)
    {
        return Err(AtlasError::InvalidConfig(format!(
            "unsupported legacy Context IR version; supported: {CONTEXT_SCHEMA_VERSION}"
        )));
    }
    Ok(serde_json::from_value(value)?)
}

/// Strict deep-only V2 reader. Canonical identity is checked before the
/// document can be consumed.
pub fn read_deep_context_ir_v2(raw: &str) -> Result<DeepContextIrV2> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some(CONTEXT_SCHEMA_V2_VERSION)
    {
        return Err(AtlasError::InvalidConfig(format!(
            "unsupported deep Context IR version; supported: {CONTEXT_SCHEMA_V2_VERSION}"
        )));
    }
    let document: DeepContextIrV2 = serde_json::from_value(value)?;
    document.validate()?;
    Ok(document)
}

/// Compatibility entry point for an explicitly V2-aware internal caller.
/// Legacy input remains tagged as legacy and gains no V2 sufficiency claim.
pub fn read_context_ir_v2_compatible(raw: &str) -> Result<ContextIrDocument> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
    {
        Some(CONTEXT_SCHEMA_VERSION) => {
            read_context_ir_v1(raw).map(ContextIrDocument::LegacyV1)
        }
        Some(CONTEXT_SCHEMA_V2_VERSION) => {
            read_deep_context_ir_v2(raw).map(ContextIrDocument::DeepV2)
        }
        _ => Err(AtlasError::InvalidConfig(format!(
            "unsupported Context IR version; supported: {CONTEXT_SCHEMA_VERSION}, {CONTEXT_SCHEMA_V2_VERSION}"
        ))),
    }
}

/// Strict V2 writer. It has no persistence side effect and rejects a stale or
/// forged canonical identity.
pub fn write_deep_context_ir_v2(document: &DeepContextIrV2) -> Result<String> {
    document.validate()?;
    Ok(serde_json::to_string(document)?)
}

/// Convert a validated deep semantic document into the frozen execution
/// payload reference. Session/attempt identity is intentionally absent, so
/// equivalent documents from direct and escalated execution can share the
/// canonical IR hash.
pub fn deep_context_payload(
    document: &DeepContextIrV2,
) -> Result<crate::context_route::ContextPayload> {
    document.validate()?;
    Ok(crate::context_route::ContextPayload::DeepContextIr {
        result: crate::context_route::DeepResultReference {
            context_ir_version: CONTEXT_SCHEMA_V2_VERSION.to_string(),
            context_hash: document.context_hash.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> String {
        // 64 lowercase hex chars, e.g. h(0) = "000...0", h(1) = "111...1".
        std::iter::repeat_n(char::from_digit(n as u32, 16).unwrap(), 64).collect()
    }

    #[test]
    fn task_session_round_trips_contract_fixture() {
        let raw = include_str!("../tests/fixtures/context_ir/task-session.example.json");
        let parsed: TaskSession = serde_json::from_str(raw).unwrap();
        parsed.validate().unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        let round_tripped = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            round_tripped, expected,
            "TaskSession must round-trip byte-for-structure against its contract fixture"
        );
    }

    #[test]
    fn symbol_card_round_trips_contract_fixture() {
        let raw = include_str!("../tests/fixtures/context_ir/symbol-card.example.json");
        let parsed: SymbolCard = serde_json::from_str(raw).unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        let round_tripped = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            round_tripped, expected,
            "SymbolCard must round-trip byte-for-structure against its contract fixture"
        );
    }

    #[test]
    fn context_use_event_round_trips_contract_fixture() {
        let raw = include_str!("../tests/fixtures/context_ir/context-use-event.example.json");
        let parsed: ContextUseEvent = serde_json::from_str(raw).unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        let round_tripped = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            round_tripped, expected,
            "ContextUseEvent must round-trip byte-for-structure against its contract fixture"
        );
    }

    #[test]
    fn generation_delta_round_trips_contract_fixture() {
        let raw = include_str!("../tests/fixtures/context_ir/generation-delta.example.json");
        let parsed: GenerationDelta = serde_json::from_str(raw).unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        let round_tripped = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            round_tripped, expected,
            "GenerationDelta must round-trip byte-for-structure against its contract fixture"
        );
    }

    #[test]
    fn context_ir_round_trips_contract_fixture() {
        let raw = include_str!("../tests/fixtures/context_ir/context-ir.example.json");
        let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
        let expected = fixture["legacy_v1"].clone();
        let parsed: ContextIr = serde_json::from_value(expected.clone()).unwrap();
        let round_tripped = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            round_tripped, expected,
            "ContextIr must round-trip byte-for-structure against its contract fixture"
        );
    }

    #[test]
    fn task_session_rejects_raw_task_without_local_opt_in() {
        let ts = TaskSession {
            schema_version: CONTEXT_SCHEMA_VERSION.to_string(),
            task_session_id: "t1".into(),
            workspace_id: "ws1".into(),
            start_generation_id: "gen1".into(),
            end_generation_id: None,
            task_hash: h(0),
            normalized_goal_hash: h(1),
            raw_task_retention: RawTaskRetention::HashOnly,
            raw_task: Some("do the thing".into()),
            task_kind: TaskKind::BugFix,
            state: TaskSessionState::Created,
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
            planner_policy_version: "p1".into(),
            context_ir_version: "1.0.0".into(),
            accepted: None,
            tests_passed: None,
            outcome_code: None,
            start_tree_hash: None,
            end_tree_hash: None,
        };
        assert!(
            ts.validate().is_err(),
            "raw_task must be rejected without local_opt_in retention"
        );
    }

    #[test]
    fn task_session_default_retention_is_privacy_preserving() {
        assert_eq!(RawTaskRetention::default(), RawTaskRetention::None);
    }

    #[test]
    fn symbol_card_seal_is_deterministic() {
        let build = || {
            SymbolCard::seal(
                "serve_gen_1".into(),
                "ws1".into(),
                "gen1".into(),
                "symbol:Foo.bar".into(),
                "src/foo.ts".into(),
                SymbolRange {
                    start_byte: 0,
                    end_byte: 10,
                    start_line: 1,
                    end_line: 2,
                },
                "method".into(),
                Some("typescript".into()),
                None,
                PreferredEvidenceSummary {
                    fact_id: "f1".into(),
                    provider_tier: "semantic_index".into(),
                    confidence: 1.0,
                    state: "resolved".into(),
                },
                SymbolCounts {
                    callers: 0,
                    callees: 0,
                    references: 0,
                    tests: 0,
                    configs: 0,
                    effects: 0,
                    conflicts: 0,
                    unresolved: 0,
                },
                CoverageState::Complete,
                SymbolCost {
                    source_bytes: 10,
                    estimated_tokens: 5,
                    metadata_bytes: 20,
                },
            )
        };
        let a = build();
        let b = build();
        assert_eq!(
            a.card_hash, b.card_hash,
            "identical inputs must produce a byte-identical card_hash"
        );
        assert!(is_hex64(&a.card_hash));
    }

    #[test]
    fn generation_delta_rejects_identical_from_and_to_generation() {
        let err = GenerationDelta::seal(
            "delta1".into(),
            "ws1".into(),
            "gen1".into(),
            "gen1".into(),
            "policy1".into(),
            DeltaChangeSet::empty(),
            DeltaChangeSet::empty(),
            DeltaChangeSet::empty(),
            DeltaChangeSet::empty(),
            DeltaChangeSet::empty(),
            DeltaChangeSet::empty(),
            Vec::new(),
            Vec::new(),
        );
        assert!(
            err.is_err(),
            "from_generation_id == to_generation_id must be rejected"
        );
    }

    #[test]
    fn context_ir_seal_is_deterministic_across_repeated_compilation() {
        let build = || {
            ContextIr::seal(
                "ctx1".into(),
                IrWorkspace {
                    workspace_id: "ws1".into(),
                    generation_id: "gen1".into(),
                    generation_sequence: 1,
                    source_tree_hash: Some(h(0)),
                    configuration_hash: h(1),
                    provider_set_hash: h(2),
                    serving_generation_id: None,
                },
                IrTask {
                    task_session_id: "t1".into(),
                    task_hash: h(0),
                    normalized_goal_hash: h(1),
                    task_kind: TaskKind::BugFix,
                    kind_source: TaskKindSource::Declared,
                    kind_rule_id: None,
                    seed_paths: vec!["src/foo.ts".into()],
                    seed_symbols: vec![],
                },
                IrPolicy::new(
                    "planner-v1".into(),
                    "projection-v1".into(),
                    "standard".into(),
                ),
                IrStatus {
                    working_set_status: WorkingSetStatus::Complete,
                    reasons: vec![],
                    serving_fallback: false,
                },
                vec![],
                vec![],
                vec![],
                vec![],
                IrCoverage {
                    eligible: 1,
                    complete: 1,
                    partial: 0,
                    unsupported: 0,
                    excluded: 0,
                    failed: 0,
                    claim_strength: ClaimStrength::Bounded,
                    details: vec![],
                },
                IrOmissions {
                    candidates_considered: 1,
                    selected: 1,
                    omitted: 0,
                    truncated: false,
                    by_reason: Default::default(),
                },
                vec![],
                IrCost {
                    max_records: 10,
                    selected_records: 0,
                    max_source_bytes: 1000,
                    selected_source_bytes: 0,
                    max_estimated_tokens: 100,
                    selected_estimated_tokens: 0,
                    soft_latency_ms: 50,
                    hard_latency_ms: 200,
                    elapsed_ms: 1,
                },
            )
        };
        let a = build();
        let b = build();
        assert_eq!(
            a.context_hash, b.context_hash,
            "identical compilation inputs must produce a byte-identical context_hash"
        );
        assert!(is_hex64(&a.context_hash));
    }
}
