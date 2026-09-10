//! V1.3 deterministic task compiler: task classification, evidence recipes,
//! and generation-bound Context IR compilation.
//!
//! Classification uses disclosed keyword rules and records
//! `TaskKindSource::DeterministicRule` plus `kind_rule_id`. Working-set
//! selection uses breadth-first distance and fixed role priority; there are no
//! embeddings, learned weights, or model-specific tuning.

use rusqlite::{params, Connection, OptionalExtension};

use crate::context_ir::{
    ClaimStrength, ContextIr, DeepContextIrV2, EffectPhase, EntityKind, EvidenceQuality, IrCost,
    IrCostV2, IrCoverage, IrCoverageV2, IrEffect, IrEvidence, IrEvidenceLeaseV2, IrItemCost,
    IrLeaseDependencyKindV2, IrLeaseDependencyV2, IrNoticeV2, IrOmissionReasonV2, IrOmissions,
    IrOmissionsV2, IrPathConfinementV2, IrPolicy, IrPolicyV2, IrRelationship, IrRequirementStateV2,
    IrRoleSufficiencyV2, IrSemanticBudgetV2, IrSourceRefV2, IrSourceVerificationV2, IrStatus,
    IrStatusV2, IrTask, IrTaskV2, IrTemporalConstraintsV2, IrTemporalStateV2, IrValidationReasonV2,
    IrValidationV2, IrWorkingSetItem, IrWorkingSetItemV2, IrWorkspace, IrWorkspaceV2, ItemRole,
    NoticeSeverity, SelectionReason, SourceVerificationStatus, TaskKind, TaskKindSource,
    ValidationKind, WorkingSetStatus,
};
use crate::error::{AtlasError, Result};
use crate::query_metrics::{
    MetricClock, QueryMetricCollector, QueryMetricIdentity, QueryStage, SystemMetricClock,
};
use crate::resolution::ResolutionStatus;
use crate::serving;
use crate::workspace::WorkspaceRecord;

pub const PLANNER_POLICY_VERSION: &str = "planner-v1.1.0";
/// Fixed semantic-policy identity for the internal ATLAS_DEEP Context IR
/// 2.0.0 contract. Public CLI and MCP compilation continue to use
/// [`PLANNER_POLICY_VERSION`].
pub const PLANNER_POLICY_V2_VERSION: &str = "planner-v2.0.0";

const SERVING_VALIDATION_MISMATCH: &str = "serving_projection_validation_mismatch";

/// Canonical task identity shared by the legacy CLI and internal progressive
/// application path. Raw task text remains transient.
pub fn normalized_task_hash(task: &str) -> String {
    crate::hashing::content_hash_of_bytes(task.trim().to_ascii_lowercase().as_bytes())
}

/// One already-ranked V2 semantic candidate presented to the deterministic
/// budget gate. Transient execution measurements are deliberately absent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepV2BudgetCandidate {
    pub candidate_id: String,
    pub rank: u64,
    pub source_bytes: u64,
    pub estimated_tokens: u64,
    pub relationship_depth: u32,
    pub work_units: u64,
    pub uncertainty: bool,
}

/// T11-frozen semantic fields produced by enforcing one explicit V2 budget.
#[derive(Debug, Clone)]
pub struct DeepV2BudgetOutcome {
    pub selected_candidate_ids: Vec<String>,
    pub omitted_candidates: Vec<(String, IrOmissionReasonV2)>,
    pub semantic_budget: IrSemanticBudgetV2,
    pub cost: IrCostV2,
    pub omissions: IrOmissionsV2,
}

#[derive(Debug, Clone, Copy)]
struct DeepWorkReservation {
    units: u64,
}

/// Request-local deterministic semantic-work ledger.
///
/// SQL reservations are charged before issuing a bounded query and settled to
/// the number of rows actually returned. `sql_rows_fetched` is deliberately a
/// physical result-row count, not a claim about SQLite VM steps or wall time.
#[derive(Debug)]
struct DeepWorkLedger {
    limit: u64,
    consumed: u64,
    sql_rows_fetched: u64,
}

impl DeepWorkLedger {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            consumed: 0,
            sql_rows_fetched: 0,
        }
    }

    fn continue_from(limit: u64, consumed: u64) -> Result<Self> {
        if consumed > limit {
            return Err(AtlasError::InvalidConfig(
                "V2 prior work exceeds its request budget".into(),
            ));
        }
        Ok(Self {
            limit,
            consumed,
            sql_rows_fetched: 0,
        })
    }

    fn remaining(&self) -> u64 {
        self.limit - self.consumed
    }

    fn try_charge(&mut self, units: u64) -> Result<bool> {
        let Some(next) = self.consumed.checked_add(units) else {
            return Err(AtlasError::InvalidConfig(
                "V2 work-unit accounting overflow".into(),
            ));
        };
        if next > self.limit {
            return Ok(false);
        }
        self.consumed = next;
        Ok(true)
    }

    fn reserve_sql_rows(&mut self, units: u64) -> Result<DeepWorkReservation> {
        if units == 0 || !self.try_charge(units)? {
            return Err(AtlasError::InvalidConfig(
                "V2 seed query exceeds its remaining work-unit budget".into(),
            ));
        }
        Ok(DeepWorkReservation { units })
    }

    fn settle_sql_rows(
        &mut self,
        reservation: DeepWorkReservation,
        rows_fetched: u64,
    ) -> Result<()> {
        if rows_fetched > reservation.units {
            return Err(AtlasError::InvalidConfig(
                "V2 seed query exceeded its reserved row bound".into(),
            ));
        }
        self.consumed -= reservation.units - rows_fetched;
        self.sql_rows_fetched =
            self.sql_rows_fetched
                .checked_add(rows_fetched)
                .ok_or_else(|| {
                    AtlasError::InvalidConfig("V2 SQL result-row accounting overflow".into())
                })?;
        Ok(())
    }
}

impl crate::generation_delta::CanonicalHistoryLedger for DeepWorkLedger {
    fn try_charge_history_input_row(&mut self) -> Result<bool> {
        self.try_charge(1)
    }
}

/// Capability token for response-lifetime exact-source materialization.
///
/// Construction is the caller's explicit authorization. The token is neither
/// serializable nor stored in Context IR or the catalogue.
#[derive(Debug, Clone, Copy)]
pub struct DeepV2SourceMaterializationAuthorization {
    max_bytes: u64,
}

impl DeepV2SourceMaterializationAuthorization {
    pub fn new(max_bytes: u64) -> Result<Self> {
        if max_bytes == 0 || max_bytes > crate::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES
        {
            return Err(AtlasError::InvalidConfig(format!(
                "exact-source materialization exceeds the {} envelope",
                crate::context_route::CONTEXT_EXECUTION_VERSION
            )));
        }
        Ok(Self { max_bytes })
    }
}

/// Transient result of final live source verification and deep IR sealing.
///
/// `materialization` belongs only to the response execution envelope. The IR
/// contains digest/range references and never owns source bytes.
#[derive(Debug, Clone)]
pub struct SealedDeepContextIrV2 {
    pub context_ir: DeepContextIrV2,
    pub materialization: Option<crate::context_route::ExplicitMaterialization>,
}

/// Task-conditioned, application-only input for one V2 deep compilation.
///
/// The caller supplies an existing legacy lifecycle session. The resulting V2
/// document is never accepted by the persistence layer.
#[derive(Debug, Clone)]
pub struct DeepV2CompileRequest {
    pub task_session_id: String,
    pub task_hash: String,
    pub normalized_goal_hash: String,
    pub task_kind: TaskKind,
    pub kind_source: TaskKindSource,
    pub kind_rule_id: Option<String>,
    pub seed_paths: Vec<String>,
    pub seed_symbols: Vec<String>,
    pub baseline_generation_id: Option<String>,
}

/// Revalidate every selected exact-source reference immediately before sealing.
///
/// The active generation, indexed whole-file digest, canonical-root
/// confinement, and zero-based half-open byte range all fail closed. Source
/// drift is represented by the frozen stale/blocked IR semantics and transient
/// source-verification status; it can never seal as complete.
pub fn seal_deep_context_ir_v2_with_live_sources(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    mut document: DeepContextIrV2,
    authorization: Option<DeepV2SourceMaterializationAuthorization>,
) -> Result<SealedDeepContextIrV2> {
    if document.workspace.workspace_id != ws.workspace_id {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR workspace does not match source authority".into(),
        ));
    }
    let expected_generation = document.workspace.generation_id.clone();
    let source_count = document
        .working_set
        .iter()
        .filter(|item| item.source.is_some())
        .count();
    if authorization.is_some() && source_count > crate::context_route::MAX_MATERIALIZED_SOURCES {
        return Err(AtlasError::InvalidConfig(
            "exact-source materialization exceeds the execution source-count bound".into(),
        ));
    }
    let session = crate::task_session::load_task_session(conn, &document.task.task_session_id)?
        .ok_or_else(|| {
            AtlasError::InvalidConfig(format!(
                "task session {} does not exist",
                document.task.task_session_id
            ))
        })?;
    if session.workspace_id != ws.workspace_id {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR task session belongs to another workspace".into(),
        ));
    }
    if session.start_generation_id != expected_generation {
        return Err(AtlasError::ActiveGenerationMismatch {
            catalog: session.start_generation_id,
            request: expected_generation.clone(),
        });
    }
    require_active_generation(conn, ws, &expected_generation)?;

    let requested_bytes = document
        .working_set
        .iter()
        .filter_map(|item| item.source.as_ref())
        .try_fold(0_u64, |total, source| {
            source
                .end_byte
                .checked_sub(source.start_byte)
                .and_then(|length| total.checked_add(length))
        })
        .ok_or_else(|| AtlasError::InvalidConfig("invalid exact-source byte range".into()))?;
    if authorization.is_some_and(|authorization| requested_bytes > authorization.max_bytes) {
        return Err(AtlasError::InvalidConfig(
            "exact-source materialization exceeds its authorized byte bound".into(),
        ));
    }

    let mut materialized_sources = Vec::new();
    let mut source_failure = None;
    let mut has_verified_primary_source = false;
    let mut source_failure_item_ids = Vec::new();
    for item in &mut document.working_set {
        let Some(mut source) = item.source.take() else {
            continue;
        };
        let verification = crate::query::verify_exact_source_at_generation(
            conn,
            ws,
            &expected_generation,
            &source.canonical_path,
            Some((source.start_byte, source.end_byte)),
            authorization.is_some(),
        )?;
        if !verification.range_valid {
            return Err(AtlasError::InvalidConfig(format!(
                "invalid zero-based half-open byte range for {}",
                source.canonical_path
            )));
        }

        let Some(indexed_hash) = verification.indexed_hash else {
            if source_failure != Some(IrRequirementStateV2::Stale) {
                source_failure = Some(IrRequirementStateV2::Unavailable);
            }
            source_failure_item_ids.push(item.entity_id.clone());
            item.cost.source_bytes = 0;
            continue;
        };
        source.whole_file_sha256 = indexed_hash;
        source.observed_sha256 = verification.observed_hash;
        source.canonical_root_confinement = verification.confinement;
        source.revalidated_before_seal = true;
        source.verification_status = match verification.status {
            "verified" => IrSourceVerificationV2::Verified,
            "hash_mismatch" => {
                source_failure = Some(IrRequirementStateV2::Stale);
                IrSourceVerificationV2::Stale
            }
            _ => {
                if source_failure != Some(IrRequirementStateV2::Stale) {
                    source_failure = Some(IrRequirementStateV2::Unavailable);
                }
                IrSourceVerificationV2::Unavailable
            }
        };
        if source.verification_status != IrSourceVerificationV2::Verified {
            source_failure_item_ids.push(item.entity_id.clone());
        } else if item.role == ItemRole::PrimaryImplementation {
            has_verified_primary_source = true;
        }
        if source.verification_status == IrSourceVerificationV2::Verified {
            if let Some(bytes) = verification.materialized_bytes {
                materialized_sources.push(crate::context_route::MaterializedSource {
                    source_digest: source.whole_file_sha256.clone(),
                    start_byte: source.start_byte,
                    end_byte: source.end_byte,
                    bytes,
                });
            }
        }
        item.source = Some(source);
    }
    document.cost.selected_source_bytes = document
        .working_set
        .iter()
        .try_fold(0_u64, |total, item| {
            total.checked_add(item.cost.source_bytes)
        })
        .ok_or_else(|| AtlasError::InvalidConfig("exact-source cost overflow".into()))?;

    require_active_generation(conn, ws, &expected_generation)?;
    if let Some(requirement_state) = source_failure {
        if has_verified_primary_source {
            document.status.exact_source = IrRequirementStateV2::Satisfied;
            document.status.working_set_status = WorkingSetStatus::Partial;
        } else {
            document.status.exact_source = requirement_state;
            document.status.working_set_status = WorkingSetStatus::Blocked;
        }
        document
            .status
            .reasons
            .push(IrOmissionReasonV2::SourceVerificationFailed);
        document.status.reasons.sort();
        source_failure_item_ids.sort();
        source_failure_item_ids.dedup();
        if let Some(notice) = document
            .uncertainty
            .iter_mut()
            .find(|notice| notice.code == IrOmissionReasonV2::SourceVerificationFailed)
        {
            notice.entity_ids.extend(source_failure_item_ids);
            notice.entity_ids.sort();
            notice.entity_ids.dedup();
            notice.severity = NoticeSeverity::Error;
        } else {
            document.uncertainty.push(IrNoticeV2 {
                code: IrOmissionReasonV2::SourceVerificationFailed,
                severity: NoticeSeverity::Error,
                entity_ids: source_failure_item_ids,
            });
            document.uncertainty.sort_by_key(|notice| notice.code);
        }
        document.status.reasons.dedup();
        materialized_sources.clear();
    }
    let context_ir = document.seal()?;
    let materialization = authorization.and_then(|authorization| {
        (source_failure.is_none() && !materialized_sources.is_empty()).then_some(
            crate::context_route::ExplicitMaterialization {
                authorized: true,
                max_bytes: authorization.max_bytes,
                sources: materialized_sources,
            },
        )
    });
    Ok(SealedDeepContextIrV2 {
        context_ir,
        materialization,
    })
}

fn require_active_generation(
    conn: &Connection,
    ws: &WorkspaceRecord,
    expected_generation: &str,
) -> Result<()> {
    let active = active_generation(conn, &ws.workspace_id)?.ok_or_else(|| {
        AtlasError::GenerationStateInvalid {
            found: "unavailable".into(),
            required: "active".into(),
        }
    })?;
    if active.0 != expected_generation {
        return Err(AtlasError::ActiveGenerationMismatch {
            catalog: active.0,
            request: expected_generation.to_string(),
        });
    }
    Ok(())
}

/// Apply every V2 semantic limit in canonical candidate-rank order.
///
/// Omission precedence is work units, relationship depth, the applicable
/// uncertainty/ordinary record partition, source bytes, then estimated
/// tokens. Each candidate's deterministic work is charged when it can be
/// performed, even if a later semantic bound omits that candidate. A
/// candidate whose work itself exceeds the remaining work-unit budget is
/// omitted without charging partial work. The uncertainty reserve rounds up
/// to a whole record, is dedicated to uncertainty, and cannot be borrowed by
/// ordinary records.
pub fn enforce_deep_v2_budget(
    budget: &crate::context_route::DeepSemanticBudget,
    candidates: &[DeepV2BudgetCandidate],
) -> Result<DeepV2BudgetOutcome> {
    let mut work_ledger = DeepWorkLedger::new(budget.max_work_units);
    enforce_deep_v2_budget_with_ledger(budget, candidates, &mut work_ledger)
}

fn enforce_deep_v2_budget_with_ledger(
    budget: &crate::context_route::DeepSemanticBudget,
    candidates: &[DeepV2BudgetCandidate],
    work_ledger: &mut DeepWorkLedger,
) -> Result<DeepV2BudgetOutcome> {
    const MAX_CANDIDATE_ID_BYTES: usize = 512;

    budget.validate().map_err(|error| {
        AtlasError::InvalidConfig(format!("invalid explicit V2 semantic budget: {error}"))
    })?;
    if work_ledger.limit != budget.max_work_units || work_ledger.consumed > budget.max_work_units {
        return Err(AtlasError::InvalidConfig(
            "V2 request work ledger does not match its semantic budget".into(),
        ));
    }
    let candidates_considered = u64::try_from(candidates.len()).map_err(|_| {
        AtlasError::InvalidConfig("V2 candidate count exceeds the supported range".into())
    })?;
    if candidates_considered > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS {
        return Err(AtlasError::InvalidConfig(
            "V2 candidate count exceeds the execution envelope".into(),
        ));
    }

    let uncertainty_records_reserved = crate::context_ir::uncertainty_reserve_records(
        budget.max_records,
        u32::from(budget.uncertainty_reserve_percent),
    )?;
    let ordinary_record_limit = budget.max_records - uncertainty_records_reserved;
    let mut candidate_ids = std::collections::HashSet::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let expected_rank = u64::try_from(index).map_err(|_| {
            AtlasError::InvalidConfig("V2 candidate rank exceeds the supported range".into())
        })?;
        if candidate.candidate_id.is_empty()
            || candidate.candidate_id.len() > MAX_CANDIDATE_ID_BYTES
            || candidate.rank != expected_rank
            || !candidate_ids.insert(candidate.candidate_id.as_str())
        {
            return Err(AtlasError::InvalidConfig(
                "V2 candidates must have bounded unique identities in canonical rank order".into(),
            ));
        }
    }

    let mut selected_candidate_ids = Vec::new();
    let mut selected_source_bytes = 0_u64;
    let mut selected_estimated_tokens = 0_u64;
    let mut relationship_depth_reached = 0_u32;
    let mut ordinary_records_selected = 0_u64;
    let mut uncertainty_records_selected = 0_u64;
    let mut by_reason = std::collections::BTreeMap::new();
    let mut omitted_candidates = Vec::new();

    for candidate in candidates {
        if candidate.work_units == 0 {
            return Err(AtlasError::InvalidConfig(
                "V2 candidate work_units must be > 0".into(),
            ));
        }

        let omission = if !work_ledger.try_charge(candidate.work_units)? {
            Some(IrOmissionReasonV2::WorkUnitBudget)
        } else {
            if candidate.relationship_depth > budget.max_depth {
                Some(IrOmissionReasonV2::RelationshipDepthBudget)
            } else if candidate.uncertainty
                && uncertainty_records_selected >= uncertainty_records_reserved
            {
                Some(IrOmissionReasonV2::UncertaintyReserve)
            } else if !candidate.uncertainty && ordinary_records_selected >= ordinary_record_limit {
                Some(IrOmissionReasonV2::RecordBudget)
            } else if selected_source_bytes
                .checked_add(candidate.source_bytes)
                .is_none_or(|value| value > budget.max_source_bytes)
            {
                Some(IrOmissionReasonV2::SourceByteBudget)
            } else if selected_estimated_tokens
                .checked_add(candidate.estimated_tokens)
                .is_none_or(|value| value > budget.max_estimated_tokens)
            {
                Some(IrOmissionReasonV2::EstimatedTokenBudget)
            } else {
                None
            }
        };

        if let Some(reason) = omission {
            let count = by_reason.entry(reason).or_insert(0_u64);
            *count = count.checked_add(1).ok_or_else(|| {
                AtlasError::InvalidConfig("V2 omission count exceeds the supported range".into())
            })?;
            omitted_candidates.push((candidate.candidate_id.clone(), reason));
            continue;
        }

        selected_candidate_ids.push(candidate.candidate_id.clone());
        selected_source_bytes = selected_source_bytes
            .checked_add(candidate.source_bytes)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 source-byte cost overflow".into()))?;
        selected_estimated_tokens = selected_estimated_tokens
            .checked_add(candidate.estimated_tokens)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 token cost overflow".into()))?;
        relationship_depth_reached = relationship_depth_reached.max(candidate.relationship_depth);
        if candidate.uncertainty {
            uncertainty_records_selected += 1;
        } else {
            ordinary_records_selected += 1;
        }
    }

    let selected_records = u64::try_from(selected_candidate_ids.len()).map_err(|_| {
        AtlasError::InvalidConfig("V2 selected record count exceeds the supported range".into())
    })?;
    let omitted = candidates_considered
        .checked_sub(selected_records)
        .ok_or_else(|| AtlasError::InvalidConfig("V2 omission count does not reconcile".into()))?;
    Ok(DeepV2BudgetOutcome {
        selected_candidate_ids,
        omitted_candidates,
        semantic_budget: IrSemanticBudgetV2::from_explicit_budget(budget)?,
        cost: IrCostV2 {
            selected_records,
            selected_source_bytes,
            selected_estimated_tokens,
            relationship_depth_reached,
            work_units_consumed: work_ledger.consumed,
            uncertainty_records_reserved,
            uncertainty_records_selected,
        },
        omissions: IrOmissionsV2 {
            candidates_considered,
            selected: selected_records,
            omitted,
            truncated: omitted != 0,
            by_reason,
        },
    })
}
const TEMPORAL_RECIPE_MAX_RECORDS_PER_SECTION: i64 = 50;

#[derive(Debug)]
struct TemporalCandidate {
    budget: DeepV2BudgetCandidate,
    item: IrWorkingSetItemV2,
}

/// Populate the temporal slots frozen by T11 for deep review, bug-fix, API-
/// change, and behavior-change recipes.
///
/// The document's baseline field is the optional requested baseline; `None`
/// selects the captured active generation's committed parent. All history
/// truth comes from [`crate::temporal::explain_temporal_state`]'s canonical
/// derivation. This operation is intentionally V2/deep-only and leaves route
/// selection, legacy Context IR, and standalone temporal output unchanged.
pub fn populate_deep_v2_temporal_evidence(
    conn: &Connection,
    ws: &WorkspaceRecord,
    document: DeepContextIrV2,
) -> Result<DeepContextIrV2> {
    let mut work_ledger = DeepWorkLedger::continue_from(
        document.policy.semantic_budget.max_work_units,
        document.cost.work_units_consumed,
    )?;
    populate_deep_v2_temporal_evidence_with_ledger(conn, ws, document, &mut work_ledger)
}

fn populate_deep_v2_temporal_evidence_with_ledger(
    conn: &Connection,
    ws: &WorkspaceRecord,
    mut document: DeepContextIrV2,
    work_ledger: &mut DeepWorkLedger,
) -> Result<DeepContextIrV2> {
    if work_ledger.limit != document.policy.semantic_budget.max_work_units
        || work_ledger.consumed != document.cost.work_units_consumed
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR work ledger does not continue its request cost".into(),
        ));
    }
    if !matches!(
        document.task.task_kind,
        TaskKind::Review | TaskKind::BugFix | TaskKind::ApiChange | TaskKind::BehaviorChange
    ) {
        return Ok(document);
    }
    validate_temporal_recipe_input(conn, ws, &document)?;
    let remaining_work = work_ledger.remaining();
    if remaining_work < 3 {
        mark_temporal_evidence_budget_omitted(&mut document)?;
        reconcile_deep_v2_status(&mut document);
        return Ok(document);
    }
    let temporal_records_per_section = i64::try_from(
        ((remaining_work - 1) / 2).min(TEMPORAL_RECIPE_MAX_RECORDS_PER_SECTION as u64),
    )
    .map_err(|_| AtlasError::InvalidConfig("temporal work bound overflow".into()))?;

    let Some(report) = crate::temporal::explain_temporal_state_for_generation_with_ledger(
        conn,
        ws,
        &document.workspace.generation_id,
        document
            .temporal_constraints
            .baseline_generation_id
            .as_deref(),
        temporal_records_per_section,
        work_ledger,
    )?
    else {
        document.cost.work_units_consumed = work_ledger.consumed;
        mark_temporal_evidence_budget_omitted(&mut document)?;
        reconcile_deep_v2_status(&mut document);
        return Ok(document);
    };
    if report.to_generation_id.as_deref() != Some(document.workspace.generation_id.as_str()) {
        return Err(AtlasError::InvalidConfig(
            "canonical temporal evidence does not match the deep Context IR generation".into(),
        ));
    }
    document.cost.work_units_consumed = work_ledger.consumed;

    match report.history_state {
        crate::temporal::TemporalHistoryState::Available => {
            populate_available_temporal_evidence(&mut document, &report, work_ledger)?;
        }
        crate::temporal::TemporalHistoryState::BaselineOnly => {
            mark_temporal_evidence_unavailable(&mut document)?;
        }
        crate::temporal::TemporalHistoryState::NoActiveGeneration => {
            return Err(AtlasError::InvalidConfig(
                "deep Context IR cannot target a missing active generation".into(),
            ));
        }
    }
    reconcile_deep_v2_status(&mut document);
    Ok(document)
}
fn validate_temporal_recipe_input(
    conn: &Connection,
    ws: &WorkspaceRecord,
    document: &DeepContextIrV2,
) -> Result<()> {
    if document.workspace.workspace_id != ws.workspace_id
        || document.temporal_constraints.current_generation_id != document.workspace.generation_id
        || document.evidence_lease.generation_id != document.workspace.generation_id
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR temporal generation identity is inconsistent".into(),
        ));
    }
    let generation_sequence: i64 = conn
        .query_row(
            "SELECT sequence_no FROM index_generation
             WHERE generation_id = ?1 AND workspace_id = ?2 AND state = 'committed'",
            params![document.workspace.generation_id, ws.workspace_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            AtlasError::InvalidConfig(
                "deep Context IR generation is not committed for its workspace".into(),
            )
        })?;
    let evidence_generation_matches = document
        .working_set
        .iter()
        .all(|item| item.evidence.generation_id == document.workspace.generation_id)
        && document
            .relationships
            .iter()
            .all(|item| item.evidence.generation_id == document.workspace.generation_id)
        && document
            .effects
            .iter()
            .all(|item| item.evidence.generation_id == document.workspace.generation_id);
    if generation_sequence != document.workspace.generation_sequence || !evidence_generation_matches
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR contains mixed-generation evidence".into(),
        ));
    }

    let recipe = deep_v2_role_recipe(document.task.task_kind);
    let role_matrix_matches = document.status.role_sufficiency.len()
        == recipe.role_matrix().count()
        && document
            .status
            .role_sufficiency
            .iter()
            .zip(recipe.role_matrix())
            .all(|(actual, expected)| actual.role == expected.0 && actual.required == expected.1);
    if !role_matrix_matches {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR role matrix does not match planner-v2.0.0".into(),
        ));
    }
    if document.working_set.iter().any(|item| {
        matches!(
            item.role,
            ItemRole::HistoricalConstraint | ItemRole::Uncertainty
        )
    }) || document
        .status
        .role_sufficiency
        .iter()
        .filter(|entry| {
            matches!(
                entry.role,
                ItemRole::HistoricalConstraint | ItemRole::Uncertainty
            )
        })
        .any(|entry| !entry.evidence_item_ids.is_empty())
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR temporal roles are already populated".into(),
        ));
    }
    if !document.temporal_constraints.evidence_item_ids.is_empty()
        || document
            .working_set
            .iter()
            .any(|item| item.selection_reason == SelectionReason::GenerationDeltaRelevance)
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR temporal evidence is already populated".into(),
        ));
    }

    let selected_records = u64::try_from(document.working_set.len())
        .map_err(|_| AtlasError::InvalidConfig("deep Context IR record count overflow".into()))?;
    let selected_source_bytes = document.working_set.iter().try_fold(0_u64, |total, item| {
        total.checked_add(item.cost.source_bytes)
    });
    let selected_estimated_tokens = document.working_set.iter().try_fold(0_u64, |total, item| {
        total.checked_add(item.cost.estimated_tokens)
    });
    let uncertainty_records = u64::try_from(
        document
            .working_set
            .iter()
            .filter(|item| item.role == ItemRole::Uncertainty)
            .count(),
    )
    .map_err(|_| AtlasError::InvalidConfig("deep Context IR record count overflow".into()))?;
    let relationship_depth = document
        .working_set
        .iter()
        .map(|item| item.distance)
        .max()
        .unwrap_or(0);
    let omitted_by_reason = document
        .omissions
        .by_reason
        .values()
        .try_fold(0_u64, |total, count| total.checked_add(*count));
    let expected_uncertainty_reserve = crate::context_ir::uncertainty_reserve_records(
        document.policy.semantic_budget.max_records,
        document.policy.semantic_budget.uncertainty_reserve_percent,
    )?;
    let expected_candidates = document
        .omissions
        .selected
        .checked_add(document.omissions.omitted)
        .ok_or_else(|| {
            AtlasError::InvalidConfig("deep Context IR omission count overflow".into())
        })?;
    if document.cost.selected_records != selected_records
        || selected_source_bytes != Some(document.cost.selected_source_bytes)
        || selected_estimated_tokens != Some(document.cost.selected_estimated_tokens)
        || document.cost.uncertainty_records_selected != uncertainty_records
        || document.cost.relationship_depth_reached != relationship_depth
        || document.cost.work_units_consumed < selected_records
        || document.cost.selected_records > document.policy.semantic_budget.max_records
        || document.cost.selected_source_bytes > document.policy.semantic_budget.max_source_bytes
        || document.cost.selected_estimated_tokens
            > document.policy.semantic_budget.max_estimated_tokens
        || document.cost.work_units_consumed > document.policy.semantic_budget.max_work_units
        || document.cost.relationship_depth_reached
            > document.policy.semantic_budget.max_relationship_depth
        || document.cost.uncertainty_records_reserved != expected_uncertainty_reserve
        || document.cost.uncertainty_records_selected > document.cost.uncertainty_records_reserved
        || document.omissions.selected != selected_records
        || omitted_by_reason != Some(document.omissions.omitted)
        || document.omissions.truncated != (document.omissions.omitted != 0)
        || document.omissions.candidates_considered != expected_candidates
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR input cost or omission ledger is inconsistent".into(),
        ));
    }
    Ok(())
}

fn enforce_precharged_additional_deep_v2_budget(
    document: &DeepContextIrV2,
    candidates: &[DeepV2BudgetCandidate],
    paid_work_sentinel_id: Option<&str>,
    work_ledger: &mut DeepWorkLedger,
) -> Result<DeepV2BudgetOutcome> {
    let budget = &document.policy.semantic_budget;
    if work_ledger.limit != budget.max_work_units
        || work_ledger.consumed != document.cost.work_units_consumed
    {
        return Err(AtlasError::InvalidConfig(
            "additional V2 budget work does not continue the request ledger".into(),
        ));
    }
    let ordinary_record_limit = budget.max_records - document.cost.uncertainty_records_reserved;
    let mut selected_candidate_ids: Vec<String> = document
        .working_set
        .iter()
        .map(|item| item.item_id.clone())
        .collect();
    let mut candidate_ids: std::collections::HashSet<String> =
        selected_candidate_ids.iter().cloned().collect();
    let mut selected_source_bytes = document.cost.selected_source_bytes;
    let mut selected_estimated_tokens = document.cost.selected_estimated_tokens;
    let mut relationship_depth_reached = document.cost.relationship_depth_reached;
    let mut uncertainty_records_selected = document.cost.uncertainty_records_selected;
    let mut ordinary_records_selected = document
        .cost
        .selected_records
        .checked_sub(uncertainty_records_selected)
        .ok_or_else(|| AtlasError::InvalidConfig("V2 record accounting underflow".into()))?;
    let mut omissions = document.omissions.clone();
    let mut omitted_candidates = Vec::new();

    for (offset, candidate) in candidates.iter().enumerate() {
        let expected_rank = document
            .working_set
            .len()
            .checked_add(offset)
            .and_then(|rank| u64::try_from(rank).ok())
            .ok_or_else(|| AtlasError::InvalidConfig("V2 candidate rank overflow".into()))?;
        if candidate.work_units != 1
            || candidate.candidate_id.is_empty()
            || candidate.candidate_id.len() > 512
            || candidate.rank != expected_rank
            || !candidate_ids.insert(candidate.candidate_id.clone())
        {
            return Err(AtlasError::InvalidConfig(
                "precharged V2 candidates must use one work unit and have bounded unique identities in canonical rank order"
                    .into(),
            ));
        }
        omissions.candidates_considered = omissions
            .candidates_considered
            .checked_add(1)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 candidate count overflow".into()))?;

        let omission = if paid_work_sentinel_id == Some(candidate.candidate_id.as_str()) {
            Some(IrOmissionReasonV2::WorkUnitBudget)
        } else if candidate.relationship_depth > budget.max_relationship_depth {
            Some(IrOmissionReasonV2::RelationshipDepthBudget)
        } else if candidate.uncertainty
            && uncertainty_records_selected >= document.cost.uncertainty_records_reserved
        {
            Some(IrOmissionReasonV2::UncertaintyReserve)
        } else if !candidate.uncertainty && ordinary_records_selected >= ordinary_record_limit {
            Some(IrOmissionReasonV2::RecordBudget)
        } else if selected_source_bytes
            .checked_add(candidate.source_bytes)
            .is_none_or(|value| value > budget.max_source_bytes)
        {
            Some(IrOmissionReasonV2::SourceByteBudget)
        } else if selected_estimated_tokens
            .checked_add(candidate.estimated_tokens)
            .is_none_or(|value| value > budget.max_estimated_tokens)
        {
            Some(IrOmissionReasonV2::EstimatedTokenBudget)
        } else {
            None
        };

        if let Some(reason) = omission {
            omissions.omitted = omissions
                .omitted
                .checked_add(1)
                .ok_or_else(|| AtlasError::InvalidConfig("V2 omission count overflow".into()))?;
            let count = omissions.by_reason.entry(reason).or_insert(0);
            *count = count
                .checked_add(1)
                .ok_or_else(|| AtlasError::InvalidConfig("V2 omission count overflow".into()))?;
            omitted_candidates.push((candidate.candidate_id.clone(), reason));
            continue;
        }

        selected_candidate_ids.push(candidate.candidate_id.clone());
        selected_source_bytes = selected_source_bytes
            .checked_add(candidate.source_bytes)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 source-byte cost overflow".into()))?;
        selected_estimated_tokens = selected_estimated_tokens
            .checked_add(candidate.estimated_tokens)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 token cost overflow".into()))?;
        relationship_depth_reached = relationship_depth_reached.max(candidate.relationship_depth);
        if candidate.uncertainty {
            uncertainty_records_selected += 1;
        } else {
            ordinary_records_selected += 1;
        }
    }

    let selected_records = u64::try_from(selected_candidate_ids.len())
        .map_err(|_| AtlasError::InvalidConfig("V2 selected record count overflow".into()))?;
    omissions.selected = selected_records;
    omissions.truncated = omissions.omitted != 0;
    Ok(DeepV2BudgetOutcome {
        selected_candidate_ids,
        omitted_candidates,
        semantic_budget: budget.clone(),
        cost: IrCostV2 {
            selected_records,
            selected_source_bytes,
            selected_estimated_tokens,
            relationship_depth_reached,
            work_units_consumed: work_ledger.consumed,
            uncertainty_records_reserved: document.cost.uncertainty_records_reserved,
            uncertainty_records_selected,
        },
        omissions,
    })
}

fn populate_available_temporal_evidence(
    document: &mut DeepContextIrV2,
    report: &crate::temporal::TemporalReport,
    work_ledger: &mut DeepWorkLedger,
) -> Result<()> {
    let remaining_packing_work = usize::try_from(work_ledger.remaining())
        .map_err(|_| AtlasError::InvalidConfig("temporal packing work overflow".into()))?;
    if remaining_packing_work == 0 {
        mark_temporal_evidence_budget_omitted(document)?;
        return Ok(());
    }
    let candidate_frontier = temporal_candidate_count(report)?.min(remaining_packing_work);
    let reserved_work = u64::try_from(candidate_frontier)
        .map_err(|_| AtlasError::InvalidConfig("temporal packing work overflow".into()))?;
    if !work_ledger.try_charge(reserved_work)? {
        return Err(AtlasError::InvalidConfig(
            "temporal packing reservation exceeded the request ledger".into(),
        ));
    }
    document.cost.work_units_consumed = work_ledger.consumed;
    let original_item_count = document.working_set.len();
    let (temporal_candidates, work_truncated) = temporal_candidates(
        report,
        &document.workspace.generation_id,
        original_item_count,
        candidate_frontier,
    )?;
    let paid_work_sentinel_id = work_truncated
        .then(|| {
            temporal_candidates
                .last()
                .map(|candidate| candidate.item.item_id.clone())
        })
        .flatten();
    let budget_candidates: Vec<DeepV2BudgetCandidate> = temporal_candidates
        .iter()
        .map(|candidate| candidate.budget.clone())
        .collect();
    let outcome = enforce_precharged_additional_deep_v2_budget(
        document,
        &budget_candidates,
        paid_work_sentinel_id.as_deref(),
        work_ledger,
    )?;

    let selected_ids: std::collections::HashSet<&str> = outcome
        .selected_candidate_ids
        .iter()
        .map(String::as_str)
        .collect();
    let omitted_reasons: std::collections::HashMap<&str, IrOmissionReasonV2> = outcome
        .omitted_candidates
        .iter()
        .map(|(candidate_id, reason)| (candidate_id.as_str(), *reason))
        .collect();
    let historical_budget_omission = temporal_candidates
        .iter()
        .filter(|candidate| candidate.item.role == ItemRole::HistoricalConstraint)
        .find_map(|candidate| {
            omitted_reasons
                .get(candidate.item.item_id.as_str())
                .copied()
        });
    let uncertainty_budget_omission = temporal_candidates
        .iter()
        .filter(|candidate| candidate.item.role == ItemRole::Uncertainty)
        .find_map(|candidate| {
            omitted_reasons
                .get(candidate.item.item_id.as_str())
                .copied()
        });
    let has_uncertainty_candidates = temporal_candidates
        .iter()
        .any(|candidate| candidate.item.role == ItemRole::Uncertainty);
    let canonical_truncated = report.changes.truncated || report.validity.truncated;
    let historical_omission_reason = if work_truncated {
        Some(IrOmissionReasonV2::WorkUnitBudget)
    } else if canonical_truncated {
        Some(IrOmissionReasonV2::RecordBudget)
    } else {
        historical_budget_omission
    };
    let mut historical_item_ids = Vec::new();
    let mut uncertainty_item_ids = Vec::new();
    for candidate in temporal_candidates {
        if selected_ids.contains(candidate.item.item_id.as_str()) {
            if candidate.item.role == ItemRole::Uncertainty {
                uncertainty_item_ids.push(candidate.item.item_id.clone());
            } else {
                historical_item_ids.push(candidate.item.item_id.clone());
            }
            document.working_set.push(candidate.item);
        }
    }
    historical_item_ids.sort();
    uncertainty_item_ids.sort();
    for (rank, item) in document.working_set.iter_mut().enumerate() {
        item.rank = rank as u64;
    }

    document.cost = outcome.cost;
    document.omissions = outcome.omissions;
    document.temporal_constraints.current_generation_id = document.workspace.generation_id.clone();
    document.temporal_constraints.baseline_generation_id = report.from_generation_id.clone();
    document.temporal_constraints.state = IrTemporalStateV2::VerifiedAncestor;
    document.temporal_constraints.evidence_item_ids = historical_item_ids.clone();
    document.temporal_constraints.omission_reason = historical_omission_reason;

    let role = temporal_role_entry(document)?;
    role.evidence_item_ids = historical_item_ids;
    if role.evidence_item_ids.is_empty() || historical_omission_reason.is_some() {
        role.state = IrRequirementStateV2::BudgetOmitted;
        role.reason = historical_omission_reason.or(Some(IrOmissionReasonV2::RecordBudget));
    } else {
        role.state = IrRequirementStateV2::Satisfied;
        role.reason = None;
    }
    if has_uncertainty_candidates {
        let uncertainty_role = document
            .status
            .role_sufficiency
            .iter_mut()
            .find(|entry| entry.role == ItemRole::Uncertainty)
            .ok_or_else(|| {
                AtlasError::InvalidConfig(
                    "deep Context IR lacks the frozen uncertainty role".into(),
                )
            })?;
        uncertainty_role.evidence_item_ids = uncertainty_item_ids;
        if let Some(reason) = uncertainty_budget_omission {
            uncertainty_role.state = IrRequirementStateV2::BudgetOmitted;
            uncertainty_role.reason = Some(reason);
        } else {
            uncertainty_role.state = IrRequirementStateV2::Unresolved;
            uncertainty_role.reason = Some(IrOmissionReasonV2::UnresolvedEvidence);
        }
    } else {
        let uncertainty_role = document
            .status
            .role_sufficiency
            .iter_mut()
            .find(|entry| entry.role == ItemRole::Uncertainty)
            .ok_or_else(|| {
                AtlasError::InvalidConfig(
                    "deep Context IR lacks the frozen uncertainty role".into(),
                )
            })?;
        uncertainty_role.state = IrRequirementStateV2::Unavailable;
        uncertainty_role.evidence_item_ids.clear();
        uncertainty_role.reason = Some(IrOmissionReasonV2::UnavailableEvidence);
    }

    if canonical_truncated {
        add_temporal_notice(document, IrOmissionReasonV2::RecordBudget, Vec::new());
    }
    if work_truncated {
        add_temporal_notice(document, IrOmissionReasonV2::WorkUnitBudget, Vec::new());
    }
    let stale_entities = document
        .working_set
        .iter()
        .filter(|item| {
            item.selection_reason == SelectionReason::GenerationDeltaRelevance
                && item.evidence.state == EvidenceQuality::Stale
        })
        .map(|item| item.entity_id.clone())
        .collect();
    add_temporal_notice(document, IrOmissionReasonV2::StaleEvidence, stale_entities);
    let unavailable_entities = document
        .working_set
        .iter()
        .filter(|item| {
            item.selection_reason == SelectionReason::GenerationDeltaRelevance
                && (item.entity_id.starts_with("temporal_validity:unavailable:")
                    || item.entity_id.contains(":live_unavailable:"))
        })
        .map(|item| item.entity_id.clone())
        .collect();
    add_temporal_notice(
        document,
        IrOmissionReasonV2::UnavailableEvidence,
        unavailable_entities,
    );
    Ok(())
}
fn temporal_candidate_count(report: &crate::temporal::TemporalReport) -> Result<usize> {
    report
        .changes
        .records
        .len()
        .checked_add(report.validity.records.len())
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| AtlasError::InvalidConfig("temporal evidence count overflow".into()))
}

fn temporal_candidates(
    report: &crate::temporal::TemporalReport,
    generation_id: &str,
    starting_rank: usize,
    max_candidates: usize,
) -> Result<(Vec<TemporalCandidate>, bool)> {
    let total_candidates = temporal_candidate_count(report)?;
    let work_truncated = total_candidates > max_candidates;
    let mut candidates = Vec::with_capacity(total_candidates.min(max_candidates));
    let delta_id = report.provenance.delta_id.as_deref().ok_or_else(|| {
        AtlasError::InvalidConfig("available temporal report lacks a Generation Delta".into())
    })?;
    let summary_identity = format!("summary:{}", report.report_hash);
    let summary_payload = serde_json::to_string(&serde_json::json!({
        "kind": "generation_delta_summary",
        "delta_id": delta_id,
        "delta_hash": report.provenance.delta_hash,
        "from_generation_id": report.from_generation_id,
        "to_generation_id": report.to_generation_id,
        "change_totals": report.changes.category_totals,
        "change_omitted_count": report.changes.omitted_count,
        "validity_total": report.validity.total_matched,
        "validity_omitted_count": report.validity.omitted_count,
        "risk": report.risk,
        "report_hash": report.report_hash,
    }))?;
    push_temporal_candidate(
        &mut candidates,
        generation_id,
        starting_rank,
        format!("temporal_report:{delta_id}:{}", report.report_hash),
        summary_payload.len(),
        summary_identity,
        ItemRole::HistoricalConstraint,
        EvidenceQuality::Verified,
        1.0,
        None,
        None,
    )?;
    for (index, record) in report
        .changes
        .records
        .iter()
        .take(max_candidates.saturating_sub(candidates.len()))
        .enumerate()
    {
        let payload = serde_json::to_string(record)?;
        let identity = format!(
            "change:{index}:{}",
            crate::hashing::content_hash_of_bytes(payload.as_bytes())
        );
        let qualification = crate::temporal::qualify_change_evidence(record);
        push_temporal_candidate(
            &mut candidates,
            generation_id,
            starting_rank,
            crate::temporal::change_record_reference(record),
            payload.len(),
            identity,
            if qualification.uncertainty {
                ItemRole::Uncertainty
            } else {
                ItemRole::HistoricalConstraint
            },
            qualification.quality,
            qualification.confidence,
            None,
            record.indexed_content_hash.clone(),
        )?;
    }
    for (index, record) in report
        .validity
        .records
        .iter()
        .take(max_candidates.saturating_sub(candidates.len()))
        .enumerate()
    {
        let payload = serde_json::to_string(record)?;
        let identity = format!(
            "validity:{index}:{}",
            crate::hashing::content_hash_of_bytes(payload.as_bytes())
        );
        let qualification = crate::temporal::qualify_validity_evidence(record.status);
        push_temporal_candidate(
            &mut candidates,
            generation_id,
            starting_rank,
            crate::temporal::validity_record_reference(record),
            payload.len(),
            identity,
            if qualification.uncertainty {
                ItemRole::Uncertainty
            } else {
                ItemRole::HistoricalConstraint
            },
            qualification.quality,
            qualification.confidence,
            None,
            record.indexed_content_hash.clone(),
        )?;
    }
    Ok((candidates, work_truncated))
}

fn temporal_item_id(identity: &str) -> String {
    format!(
        "temporal_{}",
        crate::hashing::content_hash_of_bytes(identity.as_bytes())
    )
}

#[allow(clippy::too_many_arguments)]
fn push_temporal_candidate(
    candidates: &mut Vec<TemporalCandidate>,
    generation_id: &str,
    starting_rank: usize,
    entity_id: String,
    evidence_bytes: usize,
    identity: String,
    role: ItemRole,
    quality: EvidenceQuality,
    confidence: f64,
    origin_id: Option<String>,
    source_revision_hash: Option<String>,
) -> Result<()> {
    let item_id = temporal_item_id(&identity);
    let metadata_bytes = evidence_bytes
        .checked_add(item_id.len())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| AtlasError::InvalidConfig("temporal evidence size overflow".into()))?;
    let estimated_tokens = metadata_bytes.div_ceil(4).max(1);
    let rank = starting_rank
        .checked_add(candidates.len())
        .and_then(|rank| u64::try_from(rank).ok())
        .ok_or_else(|| AtlasError::InvalidConfig("temporal evidence rank overflow".into()))?;
    candidates.push(TemporalCandidate {
        budget: DeepV2BudgetCandidate {
            candidate_id: item_id.clone(),
            rank,
            relationship_depth: 0,
            uncertainty: role == ItemRole::Uncertainty,
            source_bytes: 0,
            estimated_tokens,
            work_units: 1,
        },
        item: IrWorkingSetItemV2 {
            item_id,
            entity_kind: EntityKind::Document,
            entity_id,
            role,
            selection_reason: SelectionReason::GenerationDeltaRelevance,
            origin_id,
            distance: 0,
            rank,
            evidence: IrEvidence {
                state: quality,
                confidence,
                provider_fingerprint: None,
                source_revision_hash,
                generation_id: generation_id.to_string(),
                preferred: Some(role != ItemRole::Uncertainty),
                alternative_count: Some(0),
            },
            cost: crate::context_ir::IrItemCostV2 {
                metadata_bytes,
                source_bytes: 0,
                estimated_tokens,
            },
            source: None,
        },
    });
    Ok(())
}

fn merge_prior_omissions(omissions: &mut IrOmissionsV2, prior: &IrOmissionsV2) -> Result<()> {
    omissions.candidates_considered = omissions
        .candidates_considered
        .checked_add(prior.omitted)
        .ok_or_else(|| AtlasError::InvalidConfig("temporal omission count overflow".into()))?;
    omissions.omitted = omissions
        .omitted
        .checked_add(prior.omitted)
        .ok_or_else(|| AtlasError::InvalidConfig("temporal omission count overflow".into()))?;
    for (reason, count) in &prior.by_reason {
        let merged = omissions.by_reason.entry(*reason).or_insert(0);
        *merged = merged
            .checked_add(*count)
            .ok_or_else(|| AtlasError::InvalidConfig("temporal omission count overflow".into()))?;
    }
    omissions.truncated = omissions.omitted != 0;
    Ok(())
}

fn mark_temporal_evidence_budget_omitted(document: &mut DeepContextIrV2) -> Result<()> {
    document.temporal_constraints.current_generation_id = document.workspace.generation_id.clone();
    document.temporal_constraints.state = IrTemporalStateV2::Unavailable;
    document.temporal_constraints.evidence_item_ids.clear();
    document.temporal_constraints.omission_reason = Some(IrOmissionReasonV2::WorkUnitBudget);
    let role = temporal_role_entry(document)?;
    role.state = IrRequirementStateV2::BudgetOmitted;
    role.evidence_item_ids.clear();
    role.reason = Some(IrOmissionReasonV2::WorkUnitBudget);
    let uncertainty_role = document
        .status
        .role_sufficiency
        .iter_mut()
        .find(|entry| entry.role == ItemRole::Uncertainty)
        .ok_or_else(|| {
            AtlasError::InvalidConfig("deep Context IR lacks the frozen uncertainty role".into())
        })?;
    uncertainty_role.state = IrRequirementStateV2::BudgetOmitted;
    uncertainty_role.evidence_item_ids.clear();
    uncertainty_role.reason = Some(IrOmissionReasonV2::WorkUnitBudget);
    add_temporal_notice(document, IrOmissionReasonV2::WorkUnitBudget, Vec::new());
    Ok(())
}

fn mark_temporal_evidence_unavailable(document: &mut DeepContextIrV2) -> Result<()> {
    document.temporal_constraints.current_generation_id = document.workspace.generation_id.clone();
    document.temporal_constraints.baseline_generation_id = None;
    document.temporal_constraints.state = IrTemporalStateV2::Unavailable;
    document.temporal_constraints.evidence_item_ids.clear();
    document.temporal_constraints.omission_reason =
        Some(IrOmissionReasonV2::TemporalEvidenceUnavailable);
    let role = temporal_role_entry(document)?;
    role.state = IrRequirementStateV2::Unavailable;
    role.evidence_item_ids.clear();
    role.reason = Some(IrOmissionReasonV2::TemporalEvidenceUnavailable);
    let uncertainty_role = document
        .status
        .role_sufficiency
        .iter_mut()
        .find(|entry| entry.role == ItemRole::Uncertainty)
        .ok_or_else(|| {
            AtlasError::InvalidConfig("deep Context IR lacks the frozen uncertainty role".into())
        })?;
    uncertainty_role.state = IrRequirementStateV2::Unavailable;
    uncertainty_role.evidence_item_ids.clear();
    uncertainty_role.reason = Some(IrOmissionReasonV2::TemporalEvidenceUnavailable);
    add_temporal_notice(
        document,
        IrOmissionReasonV2::TemporalEvidenceUnavailable,
        Vec::new(),
    );
    Ok(())
}

fn temporal_role_entry(
    document: &mut DeepContextIrV2,
) -> Result<&mut crate::context_ir::IrRoleSufficiencyV2> {
    document
        .status
        .role_sufficiency
        .iter_mut()
        .find(|entry| entry.role == ItemRole::HistoricalConstraint)
        .ok_or_else(|| {
            AtlasError::InvalidConfig(
                "deep Context IR lacks the frozen historical-constraint role".into(),
            )
        })
}

fn add_temporal_notice(
    document: &mut DeepContextIrV2,
    code: IrOmissionReasonV2,
    mut entity_ids: Vec<String>,
) {
    if entity_ids.is_empty()
        && code != IrOmissionReasonV2::RecordBudget
        && code != IrOmissionReasonV2::TemporalEvidenceUnavailable
        && code != IrOmissionReasonV2::WorkUnitBudget
    {
        return;
    }
    entity_ids.sort();
    entity_ids.dedup();
    if let Some(notice) = document
        .uncertainty
        .iter_mut()
        .find(|notice| notice.code == code)
    {
        notice.entity_ids.extend(entity_ids);
        notice.entity_ids.sort();
        notice.entity_ids.dedup();
    } else {
        document.uncertainty.push(IrNoticeV2 {
            code,
            severity: NoticeSeverity::Warning,
            entity_ids,
        });
        document.uncertainty.sort_by_key(|notice| notice.code);
    }
}

fn reconcile_deep_v2_status(document: &mut DeepContextIrV2) {
    let recipe = deep_v2_role_recipe(document.task.task_kind);
    let mut reasons: Vec<IrOmissionReasonV2> = document
        .status
        .role_sufficiency
        .iter()
        .filter(|entry| entry.required && entry.state != IrRequirementStateV2::Satisfied)
        .filter_map(|entry| entry.reason)
        .collect();
    if recipe.requires_exact_source
        && document.status.exact_source != IrRequirementStateV2::Satisfied
    {
        reasons.push(IrOmissionReasonV2::SourceVerificationFailed);
    }
    if recipe.requires_validation && document.status.validation != IrRequirementStateV2::Satisfied {
        reasons.push(IrOmissionReasonV2::ValidationTargetUnavailable);
    }
    reasons.sort();
    reasons.dedup();
    document.status.reasons = reasons;
    let primary_safe = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::PrimaryImplementation)
        .is_some_and(|entry| {
            entry.state == IrRequirementStateV2::Satisfied
                && (!recipe.requires_exact_source
                    || document.status.exact_source == IrRequirementStateV2::Satisfied)
        });
    document.status.working_set_status = if document.status.reasons.is_empty() {
        WorkingSetStatus::Complete
    } else if primary_safe {
        WorkingSetStatus::Partial
    } else {
        WorkingSetStatus::Blocked
    };
}

/// Required/optional semantic roles frozen for the deep V2 planner. Later
/// compiler tasks may populate these roles but may not reinterpret the matrix.
#[derive(Debug, Clone, Copy)]
pub struct DeepV2RoleRecipe {
    pub required_roles: &'static [ItemRole],
    pub requires_exact_source: bool,
    pub requires_validation: bool,
}

const DEEP_V2_ROLES: &[ItemRole] = &[
    ItemRole::PrimaryImplementation,
    ItemRole::DirectDependency,
    ItemRole::DirectDependent,
    ItemRole::TypeContract,
    ItemRole::TestContract,
    ItemRole::ConfigurationInput,
    ItemRole::Effect,
    ItemRole::HistoricalConstraint,
    ItemRole::Uncertainty,
    ItemRole::ValidationTarget,
    ItemRole::Documentation,
];

impl DeepV2RoleRecipe {
    /// Roles not required by this recipe remain explicitly optional. The
    /// iterator borrows the fixed matrix and allocates nothing.
    pub fn optional_roles(&self) -> impl Iterator<Item = ItemRole> + '_ {
        DEEP_V2_ROLES
            .iter()
            .copied()
            .filter(|role| !self.required_roles.contains(role))
    }

    /// Complete frozen role order paired with each task kind's requirement.
    pub fn role_matrix(&self) -> impl Iterator<Item = (ItemRole, bool)> + '_ {
        DEEP_V2_ROLES
            .iter()
            .copied()
            .map(|role| (role, self.required_roles.contains(&role)))
    }
}

/// Total, fixed role/sufficiency policy for `planner-v2.0.0`.
///
/// Exact-source and validation requirements are cross-cutting semantic slots,
/// not synthetic working-set roles. Route selection and execution state do not
/// participate in this policy.
pub fn deep_v2_role_recipe(task_kind: TaskKind) -> DeepV2RoleRecipe {
    let required_roles: &'static [ItemRole] = match task_kind {
        TaskKind::Explore => &[ItemRole::PrimaryImplementation],
        TaskKind::BugFix => &[
            ItemRole::PrimaryImplementation,
            ItemRole::TestContract,
            ItemRole::HistoricalConstraint,
            ItemRole::ValidationTarget,
        ],
        TaskKind::BehaviorChange => &[
            ItemRole::PrimaryImplementation,
            ItemRole::DirectDependent,
            ItemRole::TestContract,
            ItemRole::Effect,
            ItemRole::ValidationTarget,
        ],
        TaskKind::ApiChange => &[
            ItemRole::PrimaryImplementation,
            ItemRole::TypeContract,
            ItemRole::DirectDependent,
            ItemRole::TestContract,
            ItemRole::Effect,
            ItemRole::ValidationTarget,
        ],
        TaskKind::Refactor => &[
            ItemRole::PrimaryImplementation,
            ItemRole::DirectDependent,
            ItemRole::TestContract,
            ItemRole::ValidationTarget,
        ],
        TaskKind::ConfigurationChange => &[
            ItemRole::PrimaryImplementation,
            ItemRole::ConfigurationInput,
            ItemRole::TestContract,
            ItemRole::ValidationTarget,
        ],
        TaskKind::TestChange => &[
            ItemRole::PrimaryImplementation,
            ItemRole::TestContract,
            ItemRole::ValidationTarget,
        ],
        TaskKind::Review => &[
            ItemRole::PrimaryImplementation,
            ItemRole::TestContract,
            ItemRole::Effect,
            ItemRole::HistoricalConstraint,
            ItemRole::ValidationTarget,
        ],
        TaskKind::Audit => &[
            ItemRole::PrimaryImplementation,
            ItemRole::TestContract,
            ItemRole::ConfigurationInput,
            ItemRole::HistoricalConstraint,
            ItemRole::ValidationTarget,
        ],
        TaskKind::Unknown => &[ItemRole::PrimaryImplementation, ItemRole::ValidationTarget],
    };
    DeepV2RoleRecipe {
        required_roles,
        requires_exact_source: true,
        requires_validation: true,
    }
}

/// A bounded, per-task-kind evidence policy: what relationship categories
/// contribute to the working set and how far the graph is walked. Fixed,
/// hand-authored table — never learned or tuned from observed outcomes.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceRecipe {
    pub task_kind: TaskKind,
    pub include_callers: bool,
    pub include_callees: bool,
    pub include_tests: bool,
    pub include_configs: bool,
    pub max_relationship_distance: i64,
}

/// The recipe for a task kind. Every `TaskKind` has exactly one recipe —
/// `recipe_for` is total, never falls through to a guessed default.
pub fn recipe_for(task_kind: TaskKind) -> EvidenceRecipe {
    use TaskKind::*;
    let (
        include_callers,
        include_callees,
        include_tests,
        include_configs,
        max_relationship_distance,
    ) = match task_kind {
        BugFix => (true, true, true, false, 2),
        BehaviorChange => (true, true, true, false, 2),
        ApiChange => (true, false, true, false, 3),
        Refactor => (true, true, true, false, 2),
        ConfigurationChange => (false, false, true, true, 1),
        TestChange => (true, false, true, false, 1),
        Review => (true, true, true, true, 1),
        Audit => (true, true, true, true, 3),
        Explore => (true, true, false, false, 1),
        Unknown => (true, true, true, false, 1),
    };
    EvidenceRecipe {
        task_kind,
        include_callers,
        include_callees,
        include_tests,
        include_configs,
        max_relationship_distance,
    }
}

pub(crate) fn task_kind_identity(task_kind: TaskKind) -> &'static str {
    match task_kind {
        TaskKind::Explore => "explore",
        TaskKind::BugFix => "bug_fix",
        TaskKind::BehaviorChange => "behavior_change",
        TaskKind::ApiChange => "api_change",
        TaskKind::Refactor => "refactor",
        TaskKind::ConfigurationChange => "configuration_change",
        TaskKind::TestChange => "test_change",
        TaskKind::Review => "review",
        TaskKind::Audit => "audit",
        TaskKind::Unknown => "unknown",
    }
}

pub(crate) fn task_kind_source_identity(kind_source: TaskKindSource) -> &'static str {
    match kind_source {
        TaskKindSource::Declared => "declared",
        TaskKindSource::DeterministicRule => "deterministic_rule",
        TaskKindSource::Unknown => "unknown",
    }
}

fn has_task_keyword(raw_task_text: &str, keyword: &str) -> bool {
    raw_task_text
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word.eq_ignore_ascii_case(keyword))
}

/// Deterministic keyword classification: never guesses when the caller
/// already declared a kind (`declared` wins outright, `kind_source =
/// Declared`, no rule id). Otherwise applies a fixed, ordered whole-word
/// keyword table case-insensitively — first match wins, so the rule set is
/// deterministic regardless of how many keywords co-occur. Every rule
/// carries a stable `kind_rule_id` so the decision is auditable, not a
/// black box.
pub fn classify_task(
    declared: Option<TaskKind>,
    raw_task_text: &str,
) -> (TaskKind, TaskKindSource, Option<String>) {
    if let Some(kind) = declared {
        return (kind, TaskKindSource::Declared, None);
    }
    const RULES: &[(&str, TaskKind, &str)] = &[
        ("fix", TaskKind::BugFix, "rule_bugfix_keyword_fix"),
        ("fixed", TaskKind::BugFix, "rule_bugfix_keyword_fix"),
        ("fixes", TaskKind::BugFix, "rule_bugfix_keyword_fix"),
        ("bugfix", TaskKind::BugFix, "rule_bugfix_keyword_fix"),
        ("bug", TaskKind::BugFix, "rule_bugfix_keyword_bug"),
        ("bugs", TaskKind::BugFix, "rule_bugfix_keyword_bug"),
        ("broken", TaskKind::BugFix, "rule_bugfix_keyword_bug"),
        ("refactor", TaskKind::Refactor, "rule_refactor_keyword"),
        ("refactoring", TaskKind::Refactor, "rule_refactor_keyword"),
        ("rename", TaskKind::Refactor, "rule_refactor_keyword_rename"),
        (
            "renamed",
            TaskKind::Refactor,
            "rule_refactor_keyword_rename",
        ),
        ("api", TaskKind::ApiChange, "rule_api_change_keyword"),
        (
            "interface",
            TaskKind::ApiChange,
            "rule_api_change_keyword_interface",
        ),
        (
            "configuration",
            TaskKind::ConfigurationChange,
            "rule_configuration_change_keyword",
        ),
        (
            "configure",
            TaskKind::ConfigurationChange,
            "rule_configuration_change_keyword",
        ),
        (
            "config",
            TaskKind::ConfigurationChange,
            "rule_configuration_change_keyword",
        ),
        ("tests", TaskKind::TestChange, "rule_test_change_keyword"),
        ("test", TaskKind::TestChange, "rule_test_change_keyword"),
        ("review", TaskKind::Review, "rule_review_keyword"),
        ("audit", TaskKind::Audit, "rule_audit_keyword"),
        ("explore", TaskKind::Explore, "rule_explore_keyword"),
        (
            "investigate",
            TaskKind::Explore,
            "rule_explore_keyword_investigate",
        ),
        (
            "understand",
            TaskKind::Explore,
            "rule_explore_keyword_understand",
        ),
        (
            "changes",
            TaskKind::BehaviorChange,
            "rule_behavior_change_keyword",
        ),
        (
            "change",
            TaskKind::BehaviorChange,
            "rule_behavior_change_keyword",
        ),
        (
            "adding",
            TaskKind::BehaviorChange,
            "rule_behavior_change_keyword_add",
        ),
        (
            "add",
            TaskKind::BehaviorChange,
            "rule_behavior_change_keyword_add",
        ),
    ];
    for (keyword, kind, rule_id) in RULES {
        if has_task_keyword(raw_task_text, keyword) {
            return (
                *kind,
                TaskKindSource::DeterministicRule,
                Some((*rule_id).to_string()),
            );
        }
    }
    (
        TaskKind::Unknown,
        TaskKindSource::DeterministicRule,
        Some("rule_default_unknown".to_string()),
    )
}

/// Bounded request for one Context IR compilation.
#[derive(Debug, Clone)]
pub struct CompileRequest {
    pub known_paths: Vec<String>,
    pub known_symbols: Vec<String>,
    pub max_records: i64,
    pub max_source_bytes: i64,
    pub max_estimated_tokens: i64,
    pub soft_latency_ms: i64,
    pub hard_latency_ms: i64,
}

impl Default for CompileRequest {
    fn default() -> Self {
        CompileRequest {
            known_paths: Vec::new(),
            known_symbols: Vec::new(),
            max_records: 40,
            max_source_bytes: 32_768,
            max_estimated_tokens: 8_000,
            soft_latency_ms: 500,
            hard_latency_ms: 5_000,
        }
    }
}

impl CompileRequest {
    fn canonical_values(values: &[String]) -> Vec<String> {
        let mut canonical = values.to_vec();
        canonical.sort();
        canonical.dedup();
        canonical
    }

    fn canonical_paths(&self) -> Vec<String> {
        Self::canonical_values(&self.known_paths)
    }

    fn canonical_symbols(&self) -> Vec<String> {
        Self::canonical_values(&self.known_symbols)
    }

    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("max_records", self.max_records),
            ("max_source_bytes", self.max_source_bytes),
            ("max_estimated_tokens", self.max_estimated_tokens),
        ] {
            if value <= 0 {
                return Err(AtlasError::InvalidConfig(format!(
                    "{name} must be > 0, got {value}"
                )));
            }
        }
        for (name, value) in [
            ("soft_latency_ms", self.soft_latency_ms),
            ("hard_latency_ms", self.hard_latency_ms),
        ] {
            if value <= 0 {
                return Err(AtlasError::InvalidConfig(format!(
                    "{name} must be > 0, got {value}"
                )));
            }
        }
        if self.soft_latency_ms > self.hard_latency_ms {
            return Err(AtlasError::InvalidConfig(format!(
                "soft_latency_ms ({}) must not exceed hard_latency_ms ({})",
                self.soft_latency_ms, self.hard_latency_ms
            )));
        }
        Ok(())
    }

    pub(crate) fn deterministic_hash(&self) -> String {
        let canonical = serde_json::json!({
            "known_paths": self.canonical_paths(),
            "known_symbols": self.canonical_symbols(),
            "max_records": self.max_records,
            "max_source_bytes": self.max_source_bytes,
            "max_estimated_tokens": self.max_estimated_tokens,
            "soft_latency_ms": self.soft_latency_ms,
            "hard_latency_ms": self.hard_latency_ms,
        });
        crate::hashing::content_hash_of_bytes(canonical.to_string().as_bytes())
    }
}

fn active_generation(conn: &Connection, workspace_id: &str) -> Result<Option<(String, i64)>> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT g.generation_id, g.sequence_no FROM workspace w
             JOIN index_generation g ON g.generation_id = w.active_generation_id
             WHERE w.workspace_id = ?1 AND g.state = 'committed'",
            params![workspace_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

#[allow(dead_code)] // `provider_tier` is fetched now for a future G8 research-harness breakdown by tier
struct SeedSymbol {
    canonical_symbol_key: String,
    canonical_path: String,
    symbol_kind: String,
    fact_id: String,
    provider_tier: String,
    confidence: f64,
    evidence_state: String,
    start_byte: i64,
    end_byte: i64,
    artifact_class: String,
    is_test: bool,
    content_hash: String,
}

impl From<serving::ServingSymbolEvidence> for SeedSymbol {
    fn from(value: serving::ServingSymbolEvidence) -> Self {
        Self {
            canonical_symbol_key: value.canonical_symbol_key,
            canonical_path: value.canonical_path,
            symbol_kind: value.symbol_kind,
            fact_id: value.fact_id,
            provider_tier: value.provider_tier,
            confidence: value.confidence,
            evidence_state: value.evidence_state,
            start_byte: value.start_byte,
            end_byte: value.end_byte,
            artifact_class: value.artifact_class,
            is_test: value.is_test,
            content_hash: value.content_hash,
        }
    }
}

impl From<crate::query::BoundedFtsSymbolMatch> for SeedSymbol {
    fn from(value: crate::query::BoundedFtsSymbolMatch) -> Self {
        Self {
            canonical_symbol_key: value.canonical_symbol_key,
            canonical_path: value.canonical_path,
            symbol_kind: value.symbol_kind,
            fact_id: value.fact_id,
            provider_tier: value.provider_tier,
            confidence: value.confidence,
            evidence_state: value.evidence_state,
            start_byte: value.start_byte,
            end_byte: value.end_byte,
            artifact_class: value.artifact_class,
            is_test: value.is_test,
            content_hash: value.content_hash,
        }
    }
}

fn task_specific_artifact_role(artifact_class: &str, is_test: bool) -> Option<ItemRole> {
    if is_test || artifact_class == "test" {
        Some(ItemRole::TestContract)
    } else if matches!(
        artifact_class,
        "configuration" | "build_manifest" | "infrastructure"
    ) {
        Some(ItemRole::ConfigurationInput)
    } else {
        None
    }
}

/// Deterministic role assignment for a resolved symbol. Artifact identity
/// wins over the provider's symbol kind so test/configuration evidence is
/// labelled consistently across structural and semantic providers.
fn role_for_seed_symbol(symbol: &SeedSymbol) -> ItemRole {
    task_specific_artifact_role(&symbol.artifact_class, symbol.is_test).unwrap_or(
        match symbol.symbol_kind.as_str() {
            "test" | "test_case" => ItemRole::TestContract,
            "type" | "interface" | "class" => ItemRole::TypeContract,
            _ => ItemRole::PrimaryImplementation,
        },
    )
}

/// Resolve one legacy symbol seed without ever selecting an ambiguous display
/// name. V2 callers use [`resolve_deep_v2_seed_candidates`] to retain typed
/// ambiguity and bounded FTS candidates.
fn resolve_seed_symbol(
    conn: &Connection,
    gen_id: &str,
    serving_reader: &mut serving::ServingReader<'_>,
    seed: &str,
) -> Result<Vec<SeedSymbol>> {
    let prior_examinations = serving_reader.examinations().total();
    let maximum_examinations = serving_reader.maximum_symbol_examinations();
    let canonical = serving_reader
        .symbol(seed, 1)?
        .into_iter()
        .map(SeedSymbol::from)
        .collect::<Vec<_>>();
    debug_assert!(
        serving_reader
            .examinations()
            .total()
            .saturating_sub(prior_examinations)
            <= maximum_examinations
    );
    if serving_reader.validation_mismatch() {
        return Err(AtlasError::Other(SERVING_VALIDATION_MISMATCH.into()));
    }
    if !canonical.is_empty() {
        return Ok(canonical);
    }
    let qualified = direct_seed_symbols(conn, gen_id, DeepSeedMatch::QualifiedName, seed)?;
    if !qualified.is_empty() {
        return Ok(qualified);
    }
    let display = direct_seed_symbols(conn, gen_id, DeepSeedMatch::DisplayName, seed)?;
    Ok(if display.len() == 1 {
        display
    } else {
        Vec::new()
    })
}

/// One T11-frozen seed candidate plus the T24 budget input derived from the
/// same immutable indexed fact.
#[derive(Debug, Clone)]
pub struct DeepV2SeedCandidate {
    pub budget: DeepV2BudgetCandidate,
    pub item: IrWorkingSetItemV2,
}

/// Deterministic, generation-bound seed resolution result. Candidates use only
/// frozen Context IR V2 fields; ambiguity and FTS truncation use the frozen
/// omission/notice vocabulary.
#[derive(Debug, Clone)]
pub struct DeepV2SeedResolution {
    pub candidates: Vec<DeepV2SeedCandidate>,
    pub uncertainty: Vec<IrNoticeV2>,
    pub omissions: IrOmissionsV2,
    /// Seed-candidate examinations charged by the request ledger.
    pub work_units_consumed: u64,
    /// SQL result rows fetched while resolving seeds. This excludes SQLite
    /// internal VM work and makes no latency claim.
    pub sql_rows_fetched: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepSeedMatch {
    CanonicalSymbol,
    CanonicalPath,
    QualifiedName,
    DisplayName,
    Fts,
}

fn direct_seed_symbols(
    conn: &Connection,
    gen_id: &str,
    field: DeepSeedMatch,
    value: &str,
) -> Result<Vec<SeedSymbol>> {
    let predicate = match field {
        DeepSeedMatch::CanonicalSymbol => "sf.canonical_symbol_key = ?2",
        DeepSeedMatch::QualifiedName => "sf.qualified_name = ?2",
        DeepSeedMatch::DisplayName => "sf.display_name = ?2",
        DeepSeedMatch::CanonicalPath | DeepSeedMatch::Fts => {
            return Err(AtlasError::InvalidConfig(
                "invalid direct symbol seed field".into(),
            ));
        }
    };
    let sql = format!(
        "SELECT sf.canonical_symbol_key, cf.canonical_path, sf.symbol_kind, sf.symbol_fact_id,
                er.provider_tier, sf.confidence, sf.evidence_method, sf.start_byte, sf.end_byte,
                cf.artifact_class, cf.is_test, cf.content_hash
         FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE cf.generation_id = ?1 AND {predicate}
         ORDER BY sf.canonical_symbol_key, (sf.evidence_method = 'semantic') DESC,
                  sf.confidence DESC, sf.symbol_fact_id"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params![gen_id, value], map_seed_symbol)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(preferred_seed_symbols(rows))
}

struct BoundedSeedSymbolSlice {
    rows: Vec<SeedSymbol>,
    sentinel: Option<SeedSymbol>,
    rows_fetched: u64,
}

fn bounded_direct_seed_symbols(
    conn: &Connection,
    gen_id: &str,
    field: DeepSeedMatch,
    value: &str,
    max_retained_candidates: u64,
    max_examined_rows: u64,
) -> Result<BoundedSeedSymbolSlice> {
    if max_examined_rows == 0 {
        return Err(AtlasError::InvalidConfig(
            "seed examination limit must be > 0".into(),
        ));
    }
    let predicate = match field {
        DeepSeedMatch::CanonicalSymbol => "sf.canonical_symbol_key = ?2",
        DeepSeedMatch::QualifiedName => "sf.qualified_name = ?2",
        DeepSeedMatch::DisplayName => "sf.display_name = ?2",
        DeepSeedMatch::CanonicalPath | DeepSeedMatch::Fts => {
            return Err(AtlasError::InvalidConfig(
                "invalid bounded direct symbol seed field".into(),
            ));
        }
    };
    let fetch_limit = i64::try_from(max_examined_rows)
        .map_err(|_| AtlasError::InvalidConfig("seed examination bound overflow".into()))?;
    let sql = format!(
        "SELECT sf.canonical_symbol_key, cf.canonical_path, sf.symbol_kind,
                sf.symbol_fact_id, er.provider_tier, sf.confidence,
                sf.evidence_method, sf.start_byte, sf.end_byte,
                cf.artifact_class, cf.is_test, cf.content_hash
         FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE cf.generation_id = ?1 AND {predicate}
         ORDER BY sf.canonical_symbol_key, (sf.evidence_method = 'semantic') DESC,
                  sf.confidence DESC, sf.symbol_fact_id
         LIMIT ?3"
    );
    let mut statement = conn.prepare(&sql)?;
    let mut query = statement.query(params![gen_id, value, fetch_limit])?;
    let mut rows = Vec::new();
    let mut retained_entities = std::collections::BTreeSet::new();
    let mut sentinel = None;
    let mut rows_fetched = 0_u64;
    while rows_fetched < max_examined_rows {
        let Some(row) = query.next()? else {
            break;
        };
        rows_fetched += 1;
        let candidate = map_seed_symbol(row)?;
        let new_entity = !retained_entities.contains(&candidate.canonical_symbol_key);
        if new_entity
            && u64::try_from(retained_entities.len())
                .is_ok_and(|count| count >= max_retained_candidates)
        {
            sentinel = Some(candidate);
            break;
        }
        retained_entities.insert(candidate.canonical_symbol_key.clone());
        rows.push(candidate);
    }
    if sentinel.is_none() && rows_fetched == max_examined_rows {
        sentinel = rows.pop();
    }
    Ok(BoundedSeedSymbolSlice {
        rows,
        sentinel,
        rows_fetched,
    })
}

fn map_seed_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<SeedSymbol> {
    Ok(SeedSymbol {
        canonical_symbol_key: row.get(0)?,
        canonical_path: row.get(1)?,
        symbol_kind: row.get(2)?,
        fact_id: row.get(3)?,
        provider_tier: row.get(4)?,
        confidence: row.get(5)?,
        evidence_state: row.get(6)?,
        start_byte: row.get(7)?,
        end_byte: row.get(8)?,
        artifact_class: row.get(9)?,
        is_test: row.get(10)?,
        content_hash: row.get(11)?,
    })
}

fn preferred_seed_symbols(rows: Vec<SeedSymbol>) -> Vec<SeedSymbol> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|symbol| seen.insert(symbol.canonical_symbol_key.clone()))
        .collect()
}

fn seed_item_id(seed: &str, entity_id: &str, match_kind: DeepSeedMatch) -> String {
    let identity = format!("seed:{match_kind:?}:{seed}:{entity_id}");
    format!(
        "seed_{}",
        crate::hashing::content_hash_of_bytes(identity.as_bytes())
    )
}

#[allow(clippy::too_many_arguments)]
fn symbol_seed_candidate(
    seed: &str,
    generation_id: &str,
    symbol: SeedSymbol,
    match_kind: DeepSeedMatch,
    rank: u64,
    ambiguous: bool,
    alternative_count: i64,
) -> Result<DeepV2SeedCandidate> {
    let entity_id = format!("symbol:{}", symbol.canonical_symbol_key);
    let item_id = seed_item_id(seed, &entity_id, match_kind);
    let source_bytes = if ambiguous {
        0
    } else {
        u64::try_from(symbol.end_byte.saturating_sub(symbol.start_byte))
            .map_err(|_| AtlasError::InvalidConfig("seed source range is invalid".into()))?
    };
    let metadata_bytes = u64::try_from(
        item_id
            .len()
            .checked_add(entity_id.len())
            .and_then(|value| value.checked_add(symbol.canonical_path.len()))
            .ok_or_else(|| AtlasError::InvalidConfig("seed metadata size overflow".into()))?,
    )
    .map_err(|_| AtlasError::InvalidConfig("seed metadata size overflow".into()))?;
    let estimated_tokens = metadata_bytes
        .checked_add(source_bytes)
        .ok_or_else(|| AtlasError::InvalidConfig("seed token estimate overflow".into()))?
        .div_ceil(4)
        .max(1);
    let evidence_state = if match_kind == DeepSeedMatch::Fts {
        EvidenceQuality::Unresolved
    } else if ambiguous {
        EvidenceQuality::Ambiguous
    } else {
        evidence_quality_for(&symbol.evidence_state, symbol.confidence)
    };
    let uncertainty = ambiguous || match_kind == DeepSeedMatch::Fts;
    let resolved_role = role_for_seed_symbol(&symbol);
    let source = if !uncertainty && source_bytes > 0 {
        Some(IrSourceRefV2 {
            canonical_path: symbol.canonical_path,
            whole_file_sha256: symbol.content_hash.clone(),
            start_byte: u64::try_from(symbol.start_byte)
                .map_err(|_| AtlasError::InvalidConfig("seed source range is invalid".into()))?,
            end_byte: u64::try_from(symbol.end_byte)
                .map_err(|_| AtlasError::InvalidConfig("seed source range is invalid".into()))?,
            observed_sha256: None,
            verification_status: IrSourceVerificationV2::Unavailable,
            canonical_root_confinement: IrPathConfinementV2::Unavailable,
            revalidated_before_seal: false,
        })
    } else {
        None
    };
    let item = IrWorkingSetItemV2 {
        item_id: item_id.clone(),
        entity_kind: EntityKind::Symbol,
        entity_id,
        role: if uncertainty {
            ItemRole::Uncertainty
        } else {
            resolved_role
        },
        selection_reason: if match_kind == DeepSeedMatch::Fts {
            SelectionReason::UnresolvedRelevance
        } else {
            SelectionReason::ExactSymbolMatch
        },
        origin_id: None,
        distance: 0,
        rank,
        evidence: IrEvidence {
            state: evidence_state,
            confidence: symbol.confidence,
            provider_fingerprint: None,
            source_revision_hash: Some(symbol.content_hash),
            generation_id: generation_id.to_string(),
            preferred: Some(!uncertainty),
            alternative_count: Some(alternative_count),
        },
        cost: crate::context_ir::IrItemCostV2 {
            metadata_bytes,
            source_bytes,
            estimated_tokens,
        },
        source,
    };
    Ok(DeepV2SeedCandidate {
        budget: DeepV2BudgetCandidate {
            candidate_id: item_id,
            rank,
            relationship_depth: 0,
            uncertainty,
            source_bytes,
            estimated_tokens,
            work_units: 1,
        },
        item,
    })
}

fn path_seed_candidate(
    conn: &Connection,
    generation_id: &str,
    seed: &str,
    rank: u64,
) -> Result<Option<DeepV2SeedCandidate>> {
    let file: Option<(String, i64, String, bool)> = conn
        .query_row(
            "SELECT cf.content_hash, fr.byte_size, cf.artifact_class, cf.is_test
             FROM current_file cf
             JOIN file_revision fr ON fr.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = ?2",
            params![generation_id, seed],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((content_hash, byte_size, artifact_class, is_test)) = file else {
        return Ok(None);
    };
    let source_bytes = u64::try_from(byte_size)
        .map_err(|_| AtlasError::InvalidConfig("seed file size is invalid".into()))?;
    let entity_id = format!("path:{seed}");
    let item_id = seed_item_id(seed, &entity_id, DeepSeedMatch::CanonicalPath);
    let metadata_bytes = u64::try_from(item_id.len() + entity_id.len())
        .map_err(|_| AtlasError::InvalidConfig("seed metadata size overflow".into()))?;
    let estimated_tokens = metadata_bytes
        .checked_add(source_bytes)
        .ok_or_else(|| AtlasError::InvalidConfig("seed token estimate overflow".into()))?
        .div_ceil(4)
        .max(1);
    let role = task_specific_artifact_role(&artifact_class, is_test)
        .unwrap_or(ItemRole::PrimaryImplementation);
    let item = IrWorkingSetItemV2 {
        item_id: item_id.clone(),
        entity_kind: EntityKind::File,
        entity_id,
        role,
        selection_reason: SelectionReason::ExactPathMatch,
        origin_id: None,
        distance: 0,
        rank,
        evidence: IrEvidence {
            state: EvidenceQuality::Verified,
            confidence: 1.0,
            provider_fingerprint: None,
            source_revision_hash: Some(content_hash.clone()),
            generation_id: generation_id.to_string(),
            preferred: Some(true),
            alternative_count: Some(0),
        },
        cost: crate::context_ir::IrItemCostV2 {
            metadata_bytes,
            source_bytes,
            estimated_tokens,
        },
        source: (source_bytes > 0).then_some(IrSourceRefV2 {
            canonical_path: seed.to_string(),
            whole_file_sha256: content_hash,
            start_byte: 0,
            end_byte: source_bytes,
            observed_sha256: None,
            verification_status: IrSourceVerificationV2::Unavailable,
            canonical_root_confinement: IrPathConfinementV2::Unavailable,
            revalidated_before_seal: false,
        }),
    };
    Ok(Some(DeepV2SeedCandidate {
        budget: DeepV2BudgetCandidate {
            candidate_id: item_id,
            rank,
            relationship_depth: 0,
            uncertainty: false,
            source_bytes,
            estimated_tokens,
            work_units: 1,
        },
        item,
    }))
}

fn push_unique_seed_candidate(
    candidates: &mut Vec<DeepV2SeedCandidate>,
    seen_entities: &mut std::collections::HashMap<String, usize>,
    mut candidate: DeepV2SeedCandidate,
) -> bool {
    if let Some(index) = seen_entities.get(&candidate.item.entity_id).copied() {
        let existing_is_uncertain = candidates[index].item.evidence.preferred == Some(false);
        let candidate_is_exact = candidate.item.evidence.preferred == Some(true);
        if existing_is_uncertain && candidate_is_exact {
            let rank = candidates[index].item.rank;
            candidate.item.rank = rank;
            candidate.budget.rank = rank;
            candidates[index] = candidate;
        }
        return true;
    }
    seen_entities.insert(candidate.item.entity_id.clone(), candidates.len());
    candidates.push(candidate);
    false
}

fn record_seed_omission(omissions: &mut IrOmissionsV2, reason: IrOmissionReasonV2) -> Result<()> {
    omissions.omitted = omissions
        .omitted
        .checked_add(1)
        .ok_or_else(|| AtlasError::InvalidConfig("seed omission count overflow".into()))?;
    omissions.truncated = true;
    let count = omissions.by_reason.entry(reason).or_insert(0);
    *count = count
        .checked_add(1)
        .ok_or_else(|| AtlasError::InvalidConfig("seed omission count overflow".into()))?;
    Ok(())
}

fn record_seed_limit_omission(
    omissions: &mut IrOmissionsV2,
    uncertainty: &mut Vec<IrNoticeV2>,
    seed: &str,
    reason: IrOmissionReasonV2,
) -> Result<()> {
    record_seed_omission(omissions, reason)?;
    uncertainty.push(IrNoticeV2 {
        code: reason,
        severity: NoticeSeverity::Warning,
        entity_ids: vec![format!("seed:{seed}")],
    });
    Ok(())
}

fn reserve_seed_slice(
    work_ledger: &mut DeepWorkLedger,
    retained_candidates: usize,
    future_seed_count: usize,
    candidate_limit: u64,
) -> Result<(u64, u64, DeepWorkReservation)> {
    let packing_reserve = u64::try_from(
        retained_candidates
            .checked_add(future_seed_count)
            .ok_or_else(|| AtlasError::InvalidConfig("V2 seed reserve overflow".into()))?,
    )
    .map_err(|_| AtlasError::InvalidConfig("V2 seed reserve overflow".into()))?;
    let available = work_ledger
        .remaining()
        .checked_sub(packing_reserve)
        .ok_or_else(|| {
            AtlasError::InvalidConfig("V2 seed work reserve exceeds its request budget".into())
        })?;
    if available == 0 {
        return Err(AtlasError::InvalidConfig(
            "V2 seed resolution exhausted its request work budget".into(),
        ));
    }
    let max_retained_by_work = available.saturating_sub(1) / 2;
    let max_retained_candidates = candidate_limit.min(max_retained_by_work);
    let max_examined_rows = available - max_retained_candidates;
    let reservation = work_ledger.reserve_sql_rows(max_examined_rows)?;
    Ok((max_retained_candidates, max_examined_rows, reservation))
}

#[allow(clippy::too_many_arguments)]
fn append_seed_symbol_rows(
    seed: &str,
    generation_id: &str,
    match_kind: DeepSeedMatch,
    rows: Vec<SeedSymbol>,
    ambiguous: bool,
    alternative_count: i64,
    candidates: &mut Vec<DeepV2SeedCandidate>,
    seen_entities: &mut std::collections::HashMap<String, usize>,
    omissions: &mut IrOmissionsV2,
) -> Result<Vec<String>> {
    let mut matched_entity_ids = Vec::new();
    for symbol in rows {
        matched_entity_ids.push(format!("symbol:{}", symbol.canonical_symbol_key));
        let rank = u64::try_from(candidates.len())
            .map_err(|_| AtlasError::InvalidConfig("seed rank overflow".into()))?;
        let candidate = symbol_seed_candidate(
            seed,
            generation_id,
            symbol,
            match_kind,
            rank,
            ambiguous,
            alternative_count,
        )?;
        if push_unique_seed_candidate(candidates, seen_entities, candidate) {
            record_seed_omission(omissions, IrOmissionReasonV2::UnresolvedEvidence)?;
        }
    }
    matched_entity_ids.sort();
    matched_entity_ids.dedup();
    Ok(matched_entity_ids)
}

#[allow(clippy::too_many_arguments)]
fn resolve_bounded_direct_seed_stage(
    conn: &Connection,
    generation_id: &str,
    seed: &str,
    match_kind: DeepSeedMatch,
    allow_ambiguity: bool,
    candidate_limit: u64,
    future_seed_count: usize,
    work_ledger: &mut DeepWorkLedger,
    candidates: &mut Vec<DeepV2SeedCandidate>,
    seen_entities: &mut std::collections::HashMap<String, usize>,
    omissions: &mut IrOmissionsV2,
    uncertainty: &mut Vec<IrNoticeV2>,
) -> Result<bool> {
    let (retained_candidates, examined_rows, reservation) = reserve_seed_slice(
        work_ledger,
        candidates.len(),
        future_seed_count,
        candidate_limit,
    )?;
    let matched = bounded_direct_seed_symbols(
        conn,
        generation_id,
        match_kind,
        seed,
        retained_candidates,
        examined_rows,
    )?;
    work_ledger.settle_sql_rows(reservation, matched.rows_fetched)?;
    let had_match = !matched.rows.is_empty() || matched.sentinel.is_some();
    let mut known_keys: std::collections::BTreeSet<&str> = matched
        .rows
        .iter()
        .map(|row| row.canonical_symbol_key.as_str())
        .collect();
    if let Some(sentinel) = matched.sentinel.as_ref() {
        known_keys.insert(sentinel.canonical_symbol_key.as_str());
        let duplicate = matched
            .rows
            .iter()
            .any(|row| row.canonical_symbol_key == sentinel.canonical_symbol_key)
            || seen_entities.contains_key(&format!("symbol:{}", sentinel.canonical_symbol_key));
        if duplicate {
            record_seed_omission(omissions, IrOmissionReasonV2::UnresolvedEvidence)?;
        } else {
            record_seed_limit_omission(
                omissions,
                uncertainty,
                seed,
                if retained_candidates < candidate_limit {
                    IrOmissionReasonV2::WorkUnitBudget
                } else {
                    IrOmissionReasonV2::RecordBudget
                },
            )?;
        }
    }
    if !had_match {
        return Ok(false);
    }

    let ambiguous = allow_ambiguity && known_keys.len() > 1;
    let alternatives = i64::try_from(known_keys.len().saturating_sub(1))
        .map_err(|_| AtlasError::InvalidConfig("seed alternative count overflow".into()))?;
    let matched_entity_ids = append_seed_symbol_rows(
        seed,
        generation_id,
        match_kind,
        matched.rows,
        ambiguous,
        alternatives,
        candidates,
        seen_entities,
        omissions,
    )?;
    if ambiguous && !matched_entity_ids.is_empty() {
        uncertainty.push(IrNoticeV2 {
            code: IrOmissionReasonV2::AmbiguousSeed,
            severity: NoticeSeverity::Warning,
            entity_ids: matched_entity_ids,
        });
    }
    Ok(true)
}

/// Resolve explicit deep-context seeds in the fixed planner-v2 order:
/// canonical symbol, canonical path, qualified symbol, display name, then the
/// bounded `atlas_fts` accelerator. Display-name ambiguity and FTS candidates
/// are uncertainty only and are never promoted to verified primary evidence.
pub fn resolve_deep_v2_seed_candidates(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    seeds: &[String],
    fts_candidate_limit: u64,
    max_work_units: u64,
) -> Result<DeepV2SeedResolution> {
    if fts_candidate_limit == 0 {
        return Err(AtlasError::InvalidConfig(
            "FTS candidate limit must be > 0".into(),
        ));
    }
    if max_work_units == 0 || max_work_units > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS
    {
        return Err(AtlasError::InvalidConfig(
            "V2 seed work limit exceeds the bounded execution envelope".into(),
        ));
    }
    let mut work_ledger = DeepWorkLedger::new(max_work_units);
    resolve_deep_v2_seed_candidates_with_ledger(
        conn,
        ws,
        generation_id,
        seeds,
        fts_candidate_limit,
        &mut work_ledger,
    )
}

fn resolve_deep_v2_seed_candidates_with_ledger(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    seeds: &[String],
    fts_candidate_limit: u64,
    work_ledger: &mut DeepWorkLedger,
) -> Result<DeepV2SeedResolution> {
    require_active_generation(conn, ws, generation_id)?;
    let mut canonical_seeds = seeds.to_vec();
    canonical_seeds.sort();
    canonical_seeds.dedup();
    let seed_count = u64::try_from(canonical_seeds.len())
        .map_err(|_| AtlasError::InvalidConfig("V2 seed count overflow".into()))?;
    if fts_candidate_limit > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS
        || canonical_seeds.len() > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS as usize
        || seed_count > work_ledger.limit
        || canonical_seeds
            .iter()
            .any(|seed| seed.is_empty() || seed.len() > 512)
    {
        return Err(AtlasError::InvalidConfig(
            "V2 seed resolution exceeds the bounded execution envelope".into(),
        ));
    }
    let seed_total = canonical_seeds.len();
    let mut candidates = Vec::new();
    let mut seen_entities = std::collections::HashMap::new();
    let mut uncertainty = Vec::new();
    let mut omissions = IrOmissionsV2 {
        candidates_considered: 0,
        selected: 0,
        omitted: 0,
        truncated: false,
        by_reason: std::collections::BTreeMap::new(),
    };

    for (seed_index, seed) in canonical_seeds.into_iter().enumerate() {
        let future_seed_count = seed_total - seed_index - 1;
        if resolve_bounded_direct_seed_stage(
            conn,
            generation_id,
            &seed,
            DeepSeedMatch::CanonicalSymbol,
            false,
            fts_candidate_limit,
            future_seed_count,
            work_ledger,
            &mut candidates,
            &mut seen_entities,
            &mut omissions,
            &mut uncertainty,
        )? {
            continue;
        }
        let packing_reserve = u64::try_from(candidates.len() + future_seed_count)
            .map_err(|_| AtlasError::InvalidConfig("V2 seed reserve overflow".into()))?;
        let available_for_path = work_ledger
            .remaining()
            .checked_sub(packing_reserve)
            .ok_or_else(|| {
                AtlasError::InvalidConfig("V2 seed work reserve exceeds its request budget".into())
            })?;
        let can_retain_path = available_for_path >= 2;
        let path_reservation = work_ledger.reserve_sql_rows(1)?;
        let path = path_seed_candidate(conn, generation_id, &seed, candidates.len() as u64)?;
        work_ledger.settle_sql_rows(path_reservation, u64::from(path.is_some()))?;
        if let Some(candidate) = path {
            if can_retain_path {
                if push_unique_seed_candidate(&mut candidates, &mut seen_entities, candidate) {
                    record_seed_omission(&mut omissions, IrOmissionReasonV2::UnresolvedEvidence)?;
                }
            } else {
                record_seed_limit_omission(
                    &mut omissions,
                    &mut uncertainty,
                    &seed,
                    IrOmissionReasonV2::WorkUnitBudget,
                )?;
            }
            continue;
        }

        if resolve_bounded_direct_seed_stage(
            conn,
            generation_id,
            &seed,
            DeepSeedMatch::QualifiedName,
            true,
            fts_candidate_limit,
            future_seed_count,
            work_ledger,
            &mut candidates,
            &mut seen_entities,
            &mut omissions,
            &mut uncertainty,
        )? {
            continue;
        }

        if resolve_bounded_direct_seed_stage(
            conn,
            generation_id,
            &seed,
            DeepSeedMatch::DisplayName,
            true,
            fts_candidate_limit,
            future_seed_count,
            work_ledger,
            &mut candidates,
            &mut seen_entities,
            &mut omissions,
            &mut uncertainty,
        )? {
            continue;
        }

        let (retained_rows, examined_rows, reservation) = reserve_seed_slice(
            work_ledger,
            candidates.len(),
            future_seed_count,
            fts_candidate_limit,
        )?;
        let fts = crate::query::bounded_symbol_fts_matches(
            conn,
            ws,
            generation_id,
            &seed,
            retained_rows,
            examined_rows,
        )?;
        work_ledger.settle_sql_rows(reservation, fts.rows_fetched)?;
        let fts_had_match = !fts.matches.is_empty() || fts.sentinel.is_some();
        let mut fts_keys: std::collections::BTreeSet<&str> = fts
            .matches
            .iter()
            .map(|row| row.canonical_symbol_key.as_str())
            .collect();
        if let Some(sentinel) = fts.sentinel.as_ref() {
            fts_keys.insert(sentinel.canonical_symbol_key.as_str());
            let duplicate = fts
                .matches
                .iter()
                .any(|row| row.canonical_symbol_key == sentinel.canonical_symbol_key)
                || seen_entities.contains_key(&format!("symbol:{}", sentinel.canonical_symbol_key));
            if duplicate {
                record_seed_omission(&mut omissions, IrOmissionReasonV2::UnresolvedEvidence)?;
            } else {
                record_seed_limit_omission(
                    &mut omissions,
                    &mut uncertainty,
                    &seed,
                    if retained_rows < fts_candidate_limit {
                        IrOmissionReasonV2::WorkUnitBudget
                    } else {
                        IrOmissionReasonV2::FtsCandidateLimit
                    },
                )?;
            }
        }
        if fts_had_match {
            let alternatives = i64::try_from(fts_keys.len().saturating_sub(1))
                .map_err(|_| AtlasError::InvalidConfig("seed alternative count overflow".into()))?;
            append_seed_symbol_rows(
                &seed,
                generation_id,
                DeepSeedMatch::Fts,
                fts.matches.into_iter().map(SeedSymbol::from).collect(),
                true,
                alternatives,
                &mut candidates,
                &mut seen_entities,
                &mut omissions,
            )?;
        } else {
            if !work_ledger.try_charge(1)? {
                return Err(AtlasError::InvalidConfig(
                    "V2 unresolved seed exceeded its request work budget".into(),
                ));
            }
            record_seed_omission(&mut omissions, IrOmissionReasonV2::UnresolvedEvidence)?;
        }
    }

    let unresolved_entities: std::collections::HashSet<&str> = candidates
        .iter()
        .filter(|candidate| candidate.item.role == ItemRole::Uncertainty)
        .map(|candidate| candidate.item.entity_id.as_str())
        .collect();
    for notice in &mut uncertainty {
        if notice.code == IrOmissionReasonV2::AmbiguousSeed {
            notice
                .entity_ids
                .retain(|entity_id| unresolved_entities.contains(entity_id.as_str()));
        }
    }
    uncertainty.retain(|notice| {
        notice.code != IrOmissionReasonV2::AmbiguousSeed || !notice.entity_ids.is_empty()
    });
    omissions.selected = u64::try_from(candidates.len())
        .map_err(|_| AtlasError::InvalidConfig("seed candidate count overflow".into()))?;
    omissions.candidates_considered = omissions
        .selected
        .checked_add(omissions.omitted)
        .ok_or_else(|| AtlasError::InvalidConfig("seed candidate count overflow".into()))?;
    uncertainty.sort_by_key(|notice| notice.code);
    Ok(DeepV2SeedResolution {
        candidates,
        uncertainty,
        omissions,
        work_units_consumed: work_ledger.consumed,
        sql_rows_fetched: work_ledger.sql_rows_fetched,
    })
}
fn deep_v2_validation_candidate(
    source: &DeepV2SeedCandidate,
    rank: u64,
) -> Result<DeepV2SeedCandidate> {
    let item_id = crate::resolution::deterministic_id(
        "item",
        &["validation", &source.item.entity_id, &source.item.item_id],
    );
    let metadata_bytes = u64::try_from(
        item_id
            .len()
            .checked_add(source.item.entity_id.len())
            .ok_or_else(|| AtlasError::InvalidConfig("validation metadata size overflow".into()))?,
    )
    .map_err(|_| AtlasError::InvalidConfig("validation metadata size overflow".into()))?;
    let estimated_tokens = metadata_bytes.div_ceil(4).max(1);
    let item = IrWorkingSetItemV2 {
        item_id: item_id.clone(),
        entity_kind: source.item.entity_kind,
        entity_id: source.item.entity_id.clone(),
        role: ItemRole::ValidationTarget,
        selection_reason: SelectionReason::TaskRecipeRequirement,
        origin_id: Some(source.item.item_id.clone()),
        distance: source.item.distance,
        rank,
        evidence: source.item.evidence.clone(),
        cost: crate::context_ir::IrItemCostV2 {
            metadata_bytes,
            source_bytes: 0,
            estimated_tokens,
        },
        source: None,
    };
    Ok(DeepV2SeedCandidate {
        budget: DeepV2BudgetCandidate {
            candidate_id: item_id,
            rank,
            source_bytes: 0,
            estimated_tokens,
            relationship_depth: item.distance,
            work_units: 1,
            uncertainty: false,
        },
        item,
    })
}

type DeepV2RoleDeficit = (ItemRole, IrOmissionReasonV2);

fn role_deficit_priority(reason: IrOmissionReasonV2) -> u8 {
    match reason {
        IrOmissionReasonV2::ConflictingEvidence => 0,
        IrOmissionReasonV2::StaleEvidence => 1,
        IrOmissionReasonV2::UnresolvedEvidence => 2,
        IrOmissionReasonV2::UnsupportedEvidence => 3,
        IrOmissionReasonV2::RecordBudget
        | IrOmissionReasonV2::SourceByteBudget
        | IrOmissionReasonV2::EstimatedTokenBudget
        | IrOmissionReasonV2::RelationshipDepthBudget
        | IrOmissionReasonV2::WorkUnitBudget
        | IrOmissionReasonV2::UncertaintyReserve => 4,
        _ => 5,
    }
}

fn closure_role_priority(recipe: DeepV2RoleRecipe, role: ItemRole) -> usize {
    if let Some(position) = recipe
        .required_roles
        .iter()
        .position(|required| *required == role)
    {
        position
    } else if recipe.requires_validation && role == ItemRole::TestContract {
        recipe.required_roles.len()
    } else if recipe.requires_validation && role == ItemRole::ValidationTarget {
        recipe.required_roles.len() + 1
    } else {
        recipe.required_roles.len()
            + 2
            + DEEP_V2_ROLES
                .iter()
                .position(|candidate| *candidate == role)
                .unwrap_or(DEEP_V2_ROLES.len())
    }
}

fn graph_requirements_satisfied(
    recipe: DeepV2RoleRecipe,
    working_set: &[IrWorkingSetItemV2],
) -> bool {
    recipe
        .required_roles
        .iter()
        .filter(|role| {
            !matches!(
                role,
                ItemRole::PrimaryImplementation
                    | ItemRole::Effect
                    | ItemRole::HistoricalConstraint
                    | ItemRole::ValidationTarget
            )
        })
        .all(|role| {
            working_set.iter().any(|item| {
                item.role == *role && evidence_reason_for_quality(item.evidence.state).is_none()
            })
        })
        && (!recipe.requires_validation
            || working_set.iter().any(|item| {
                item.role == ItemRole::ValidationTarget
                    && evidence_reason_for_quality(item.evidence.state).is_none()
            }))
}

fn record_role_deficit(
    deficits: &mut Vec<DeepV2RoleDeficit>,
    role: ItemRole,
    reason: IrOmissionReasonV2,
) {
    if let Some(existing) = deficits.iter_mut().find(|entry| entry.0 == role) {
        if role_deficit_priority(reason) < role_deficit_priority(existing.1) {
            existing.1 = reason;
        }
    } else {
        deficits.push((role, reason));
    }
}

fn requirement_state_for_reason(reason: IrOmissionReasonV2) -> IrRequirementStateV2 {
    match reason {
        IrOmissionReasonV2::StaleEvidence | IrOmissionReasonV2::SourceVerificationFailed => {
            IrRequirementStateV2::Stale
        }
        IrOmissionReasonV2::UnsupportedEvidence => IrRequirementStateV2::Unsupported,
        IrOmissionReasonV2::ConflictingEvidence => IrRequirementStateV2::Conflicting,
        IrOmissionReasonV2::RecordBudget
        | IrOmissionReasonV2::SourceByteBudget
        | IrOmissionReasonV2::EstimatedTokenBudget
        | IrOmissionReasonV2::RelationshipDepthBudget
        | IrOmissionReasonV2::WorkUnitBudget
        | IrOmissionReasonV2::UncertaintyReserve => IrRequirementStateV2::BudgetOmitted,
        _ => IrRequirementStateV2::Unresolved,
    }
}

fn record_closure_omission(
    document: &mut DeepContextIrV2,
    reason: IrOmissionReasonV2,
    count: u64,
) -> Result<()> {
    document.omissions.candidates_considered = document
        .omissions
        .candidates_considered
        .checked_add(count)
        .ok_or_else(|| AtlasError::InvalidConfig("role closure candidate count overflow".into()))?;
    document.omissions.omitted = document
        .omissions
        .omitted
        .checked_add(count)
        .ok_or_else(|| AtlasError::InvalidConfig("role closure omission count overflow".into()))?;
    let reason_count = document.omissions.by_reason.entry(reason).or_insert(0);
    *reason_count = reason_count
        .checked_add(count)
        .ok_or_else(|| AtlasError::InvalidConfig("role closure omission count overflow".into()))?;
    document.omissions.truncated = true;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn deep_v2_closure_candidate(
    generation_id: &str,
    fact_id: &str,
    neighbor: &HopEndpoint,
    role: ItemRole,
    reason: SelectionReason,
    origin_id: &str,
    distance: u32,
    rank: u64,
    evidence: IrEvidence,
) -> Result<DeepV2SeedCandidate> {
    let entity_id = neighbor.entity_id();
    let item_id = crate::resolution::deterministic_id(
        "item",
        &["role-closure", generation_id, fact_id, &entity_id],
    );
    let metadata_bytes = u64::try_from(
        item_id
            .len()
            .checked_add(entity_id.len())
            .ok_or_else(|| AtlasError::InvalidConfig("role closure metadata overflow".into()))?,
    )
    .map_err(|_| AtlasError::InvalidConfig("role closure metadata overflow".into()))?;
    let estimated_tokens = metadata_bytes.div_ceil(4).max(1);
    let item = IrWorkingSetItemV2 {
        item_id: item_id.clone(),
        entity_kind: neighbor.kind,
        entity_id,
        role,
        selection_reason: reason,
        origin_id: Some(origin_id.to_string()),
        distance,
        rank,
        evidence,
        cost: crate::context_ir::IrItemCostV2 {
            metadata_bytes,
            source_bytes: 0,
            estimated_tokens,
        },
        source: None,
    };
    Ok(DeepV2SeedCandidate {
        budget: DeepV2BudgetCandidate {
            candidate_id: item_id,
            rank,
            source_bytes: 0,
            estimated_tokens,
            relationship_depth: distance,
            work_units: 1,
            uncertainty: false,
        },
        item,
    })
}

fn effect_phase_from_db(value: &str) -> EffectPhase {
    match value {
        "transitive" => EffectPhase::Transitive,
        "declared" => EffectPhase::Declared,
        "observed" => EffectPhase::Observed,
        _ => EffectPhase::Direct,
    }
}

fn evidence_reason_for_quality(quality: EvidenceQuality) -> Option<IrOmissionReasonV2> {
    match quality {
        EvidenceQuality::Stale => Some(IrOmissionReasonV2::StaleEvidence),
        EvidenceQuality::Conflicting => Some(IrOmissionReasonV2::ConflictingEvidence),
        EvidenceQuality::Unsupported | EvidenceQuality::Excluded => {
            Some(IrOmissionReasonV2::UnsupportedEvidence)
        }
        EvidenceQuality::Inferred
        | EvidenceQuality::Ambiguous
        | EvidenceQuality::Unresolved
        | EvidenceQuality::Partial
        | EvidenceQuality::External => Some(IrOmissionReasonV2::UnresolvedEvidence),
        EvidenceQuality::Verified | EvidenceQuality::Supported | EvidenceQuality::Resolved => None,
    }
}

fn populate_deep_v2_effect_closure(
    conn: &Connection,
    document: &mut DeepContextIrV2,
    work_ledger: &mut DeepWorkLedger,
    deficits: &mut Vec<DeepV2RoleDeficit>,
) -> Result<()> {
    if !deep_v2_role_recipe(document.task.task_kind)
        .required_roles
        .contains(&ItemRole::Effect)
    {
        return Ok(());
    }
    let mut subjects: Vec<(String, String)> = document
        .working_set
        .iter()
        .filter_map(|item| match item.entity_kind {
            EntityKind::Symbol => item
                .entity_id
                .strip_prefix("symbol:")
                .map(|value| ("symbol".to_string(), value.to_string())),
            EntityKind::File => item
                .entity_id
                .strip_prefix("path:")
                .map(|value| ("path".to_string(), value.to_string())),
            _ => None,
        })
        .collect();
    subjects.sort();
    subjects.dedup();
    let mut seen_effects = std::collections::BTreeSet::new();
    for (subject_kind, subject_value) in subjects {
        let remaining = work_ledger.remaining();
        let row_limit = remaining
            .saturating_sub(1)
            .checked_div(2)
            .unwrap_or(0)
            .min(document.policy.semantic_budget.max_records);
        if row_limit == 0 {
            record_role_deficit(
                deficits,
                ItemRole::Effect,
                IrOmissionReasonV2::WorkUnitBudget,
            );
            record_closure_omission(document, IrOmissionReasonV2::WorkUnitBudget, 1)?;
            add_temporal_notice(document, IrOmissionReasonV2::WorkUnitBudget, Vec::new());
            break;
        }
        let fetch_limit = row_limit
            .checked_add(1)
            .ok_or_else(|| AtlasError::InvalidConfig("effect frontier overflow".into()))?;
        let reservation = work_ledger.reserve_sql_rows(fetch_limit)?;
        let mut statement = conn.prepare(
            "SELECT ef.effect_fact_id, ef.effect_type, ef.phase, ef.evidence_method,
                    ef.confidence, fr.content_hash,
                    EXISTS(
                        SELECT 1 FROM evidence_conflict ec
                        WHERE ec.generation_id = ?1
                          AND ec.subject_kind = 'effect'
                          AND ec.subject_key = ef.effect_fact_id
                          AND ec.status IN ('open', 'preferred_with_conflict', 'unresolved')
                    )
             FROM effect_fact ef INDEXED BY idx_effect_subject
             JOIN generation_file gf
               ON gf.generation_id = ?1 AND gf.revision_id = ef.revision_id
              AND gf.presence_state = 'present'
             JOIN file_revision fr ON fr.revision_id = gf.revision_id
             WHERE ef.subject_ref_kind = ?2 AND ef.subject_ref_value = ?3
             ORDER BY ef.effect_type, ef.effect_fact_id
             LIMIT ?4",
        )?;
        let mut rows = statement
            .query_map(
                params![
                    document.workspace.generation_id,
                    subject_kind,
                    subject_value,
                    i64::try_from(fetch_limit).map_err(|_| {
                        AtlasError::InvalidConfig("effect frontier exceeds SQLite range".into())
                    })?,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        work_ledger.settle_sql_rows(
            reservation,
            u64::try_from(rows.len())
                .map_err(|_| AtlasError::InvalidConfig("effect frontier overflow".into()))?,
        )?;
        let truncated = rows.len() > row_limit as usize;
        if truncated {
            rows.pop();
            record_role_deficit(
                deficits,
                ItemRole::Effect,
                IrOmissionReasonV2::WorkUnitBudget,
            );
            record_closure_omission(document, IrOmissionReasonV2::WorkUnitBudget, 1)?;
            add_temporal_notice(document, IrOmissionReasonV2::WorkUnitBudget, Vec::new());
        }

        let starting_rank = document.working_set.len();
        let mut candidates = Vec::new();
        let mut effects = Vec::new();
        for (fact_id, effect_type, phase, method, confidence, revision_hash, conflicting) in rows {
            let subject_entity_id = format!("{subject_kind}:{subject_value}");
            if !seen_effects.insert((subject_entity_id.clone(), effect_type.clone())) {
                continue;
            }
            let quality = if conflicting {
                EvidenceQuality::Conflicting
            } else {
                evidence_quality_for(&method, confidence)
            };
            if let Some(reason) = evidence_reason_for_quality(quality) {
                record_role_deficit(deficits, ItemRole::Effect, reason);
                add_temporal_notice(document, reason, vec![subject_entity_id.clone()]);
            }
            let evidence = IrEvidence {
                state: quality,
                confidence,
                provider_fingerprint: None,
                source_revision_hash: Some(revision_hash),
                generation_id: document.workspace.generation_id.clone(),
                preferred: Some(!conflicting),
                alternative_count: Some(i64::from(conflicting)),
            };
            let rank = u64::try_from(starting_rank + candidates.len())
                .map_err(|_| AtlasError::InvalidConfig("effect rank overflow".into()))?;
            let endpoint = HopEndpoint::from_entity_id(&subject_entity_id);
            let candidate = deep_v2_closure_candidate(
                &document.workspace.generation_id,
                &fact_id,
                &endpoint,
                ItemRole::Effect,
                SelectionReason::EffectRelation,
                &fact_id,
                0,
                rank,
                evidence.clone(),
            )?;
            effects.push((
                candidate.item.item_id.clone(),
                IrEffect {
                    subject_entity_id,
                    effect_type,
                    phase: effect_phase_from_db(&phase),
                    evidence,
                },
            ));
            candidates.push(candidate);
        }
        let packing_work = u64::try_from(candidates.len())
            .map_err(|_| AtlasError::InvalidConfig("effect packing overflow".into()))?;
        if !work_ledger.try_charge(packing_work)? {
            return Err(AtlasError::InvalidConfig(
                "effect packing exceeded its reserved request work".into(),
            ));
        }
        document.cost.work_units_consumed = work_ledger.consumed;
        let budgets: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.budget.clone())
            .collect();
        let outcome =
            enforce_precharged_additional_deep_v2_budget(document, &budgets, None, work_ledger)?;
        let selected: std::collections::HashSet<&str> = outcome
            .selected_candidate_ids
            .iter()
            .map(String::as_str)
            .collect();
        for candidate in &candidates {
            if selected.contains(candidate.item.item_id.as_str()) {
                document.working_set.push(candidate.item.clone());
            }
        }
        for (item_id, effect) in effects {
            if selected.contains(item_id.as_str()) {
                document.effects.push(effect);
            }
        }
        for (candidate_id, reason) in &outcome.omitted_candidates {
            if let Some(candidate) = budgets
                .iter()
                .find(|candidate| candidate.candidate_id == *candidate_id)
            {
                let role = candidates
                    .iter()
                    .find(|value| value.budget.candidate_id == candidate.candidate_id)
                    .map(|value| value.item.role)
                    .unwrap_or(ItemRole::Effect);
                record_role_deficit(deficits, role, *reason);
            }
        }
        document.cost = outcome.cost;
        document.omissions = outcome.omissions;
    }
    document.effects.sort_by(|left, right| {
        (&left.subject_entity_id, &left.effect_type)
            .cmp(&(&right.subject_entity_id, &right.effect_type))
    });
    Ok(())
}

fn mark_graph_work_budget_deficits(
    document: &mut DeepContextIrV2,
    deficits: &mut Vec<DeepV2RoleDeficit>,
) -> Result<()> {
    let recipe = deep_v2_role_recipe(document.task.task_kind);
    for role in recipe.required_roles {
        if !matches!(
            role,
            ItemRole::PrimaryImplementation
                | ItemRole::Effect
                | ItemRole::HistoricalConstraint
                | ItemRole::ValidationTarget
        ) {
            record_role_deficit(deficits, *role, IrOmissionReasonV2::WorkUnitBudget);
        }
    }
    record_role_deficit(
        deficits,
        ItemRole::ValidationTarget,
        IrOmissionReasonV2::WorkUnitBudget,
    );
    record_closure_omission(document, IrOmissionReasonV2::WorkUnitBudget, 1)?;
    add_temporal_notice(document, IrOmissionReasonV2::WorkUnitBudget, Vec::new());
    Ok(())
}

fn populate_deep_v2_relationship_closure(
    conn: &Connection,
    ws: &WorkspaceRecord,
    document: &mut DeepContextIrV2,
    work_ledger: &mut DeepWorkLedger,
    deficits: &mut Vec<DeepV2RoleDeficit>,
) -> Result<()> {
    let role_recipe = deep_v2_role_recipe(document.task.task_kind);
    let mut evidence_recipe = recipe_for(document.task.task_kind);
    evidence_recipe.include_tests |= role_recipe.requires_validation;
    evidence_recipe.max_relationship_distance = evidence_recipe.max_relationship_distance.min(
        i64::from(document.policy.semantic_budget.max_relationship_depth),
    );
    let generation_id = document.workspace.generation_id.clone();
    let examinations = std::cell::Cell::new(serving::ServingReadExaminations::default());
    let mut reader =
        serving::ServingReader::new(conn, &ws.workspace_id, &generation_id, false, &examinations)?;
    let mut frontier = std::collections::VecDeque::new();
    for item in &document.working_set {
        if item.entity_kind == EntityKind::Symbol {
            if let Some(symbol) = item.entity_id.strip_prefix("symbol:") {
                frontier.push_back((symbol.to_string(), item.item_id.clone(), item.distance));
            }
        }
    }
    let mut visited = std::collections::BTreeSet::new();

    while let Some((current_symbol, origin_item_id, distance)) = frontier.pop_front() {
        if !visited.insert(current_symbol.clone())
            || i64::from(distance) >= evidence_recipe.max_relationship_distance
        {
            continue;
        }
        let remaining = work_ledger.remaining();
        let row_limit = remaining
            .saturating_sub(2)
            .checked_div(5)
            .unwrap_or(0)
            .min(document.policy.semantic_budget.max_records);
        if row_limit == 0 {
            mark_graph_work_budget_deficits(document, deficits)?;
            break;
        }
        let row_limit = usize::try_from(row_limit)
            .map_err(|_| AtlasError::InvalidConfig("relationship frontier overflow".into()))?;
        let maximum_rows = row_limit
            .checked_add(1)
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| AtlasError::InvalidConfig("relationship frontier overflow".into()))?;
        let reservation = work_ledger
            .reserve_sql_rows(u64::try_from(maximum_rows).map_err(|_| {
                AtlasError::InvalidConfig("relationship frontier overflow".into())
            })?)?;
        let before = examinations.get().total();
        let slice = reader.relationships(
            &current_symbol,
            row_limit,
            serving::RelationshipReadFilter {
                include_callers: evidence_recipe.include_callers,
                include_callees: evidence_recipe.include_callees,
                include_tests: evidence_recipe.include_tests,
                include_configs: evidence_recipe.include_configs,
            },
        )?;
        let examined = examinations.get().total().saturating_sub(before);
        work_ledger.settle_sql_rows(
            reservation,
            u64::try_from(examined)
                .map_err(|_| AtlasError::InvalidConfig("relationship frontier overflow".into()))?,
        )?;
        if slice.truncated {
            mark_graph_work_budget_deficits(document, deficits)?;
        }

        let starting_rank = document.working_set.len();
        let mut candidates = Vec::new();
        let mut relationships = Vec::new();
        for row in slice.rows {
            let hop = Hop {
                relationship_fact_id: row.relationship_fact_id,
                source: HopEndpoint::from_entity_id(&row.source_entity_id),
                target: HopEndpoint::from_entity_id(&row.target_entity_id),
                raw_target_ref_kind: row.raw_target_ref_kind,
                raw_target_ref_value: row.raw_target_ref_value,
                relationship_type: row.relationship_type,
                resolution_state: ResolutionStatus::from_db_str(&row.resolution_state)
                    .unwrap_or(ResolutionStatus::Unresolved),
                confidence: row.confidence,
            };
            let Some((neighbor, inbound)) = hop.neighbor_of(&current_symbol) else {
                continue;
            };
            if !work_ledger.try_charge(1)? {
                mark_graph_work_budget_deficits(document, deficits)?;
                break;
            }
            let artifact =
                artifact_for_endpoint(conn, &document.workspace.generation_id, neighbor)?;
            let artifact_role = artifact.as_ref().and_then(|(artifact_class, is_test)| {
                task_specific_artifact_role(artifact_class, *is_test)
            });
            if !recipe_includes(
                &evidence_recipe,
                &hop.relationship_type,
                inbound,
                artifact_role,
            ) {
                continue;
            }
            let (role, selection_reason) = role_and_reason_for(
                &hop.relationship_type,
                inbound,
                neighbor.kind,
                artifact_role,
            );
            let endpoint_missing = matches!(neighbor.kind, EntityKind::Symbol | EntityKind::File)
                && artifact.is_none();
            let quality = if endpoint_missing {
                EvidenceQuality::Unresolved
            } else {
                match hop.resolution_state {
                    ResolutionStatus::ResolvedSymbol | ResolutionStatus::ResolvedFile => {
                        EvidenceQuality::Resolved
                    }
                    ResolutionStatus::External => EvidenceQuality::External,
                    ResolutionStatus::Stale => EvidenceQuality::Stale,
                    ResolutionStatus::Ambiguous => EvidenceQuality::Ambiguous,
                    ResolutionStatus::Unresolved | ResolutionStatus::Invalid => {
                        EvidenceQuality::Unresolved
                    }
                }
            };
            let closure_reason = if endpoint_missing {
                Some(IrOmissionReasonV2::UnavailableEvidence)
            } else {
                evidence_reason_for_quality(quality)
            };
            let evidence = IrEvidence {
                state: quality,
                confidence: hop.confidence,
                provider_fingerprint: None,
                source_revision_hash: None,
                generation_id: document.workspace.generation_id.clone(),
                preferred: Some(closure_reason.is_none()),
                alternative_count: Some(0),
            };
            let relationship = IrRelationship {
                relationship_id: crate::resolution::deterministic_id(
                    "rel",
                    &[&document.workspace.generation_id, &hop.relationship_fact_id],
                ),
                source_entity_id: hop.source.entity_id(),
                target_entity_id: hop.target.entity_id(),
                relationship_type: hop.relationship_type.clone(),
                resolution_state: hop.resolution_state,
                evidence: evidence.clone(),
            };
            if let Some(reason) = closure_reason {
                record_role_deficit(deficits, role, reason);
                add_temporal_notice(
                    document,
                    reason,
                    vec![
                        relationship.source_entity_id.clone(),
                        relationship.target_entity_id.clone(),
                    ],
                );
                relationships.push((None, relationship));
                continue;
            }
            let rank = u64::try_from(starting_rank + candidates.len())
                .map_err(|_| AtlasError::InvalidConfig("relationship rank overflow".into()))?;
            let candidate = deep_v2_closure_candidate(
                &document.workspace.generation_id,
                &hop.relationship_fact_id,
                neighbor,
                role,
                selection_reason,
                &origin_item_id,
                distance + 1,
                rank,
                evidence,
            )?;
            let item_id = candidate.item.item_id.clone();
            let validation_candidate = if role == ItemRole::TestContract {
                Some(deep_v2_validation_candidate(
                    &candidate,
                    rank.checked_add(1).ok_or_else(|| {
                        AtlasError::InvalidConfig("validation rank overflow".into())
                    })?,
                )?)
            } else {
                None
            };
            candidates.push(candidate);
            relationships.push((Some(item_id), relationship));
            if let Some(validation_candidate) = validation_candidate {
                candidates.push(validation_candidate);
            }
        }
        candidates.sort_by(|left, right| {
            closure_role_priority(role_recipe, left.item.role)
                .cmp(&closure_role_priority(role_recipe, right.item.role))
                .then_with(|| left.item.distance.cmp(&right.item.distance))
                .then_with(|| left.item.item_id.cmp(&right.item.item_id))
        });
        for (offset, candidate) in candidates.iter_mut().enumerate() {
            let rank = u64::try_from(starting_rank + offset)
                .map_err(|_| AtlasError::InvalidConfig("relationship rank overflow".into()))?;
            candidate.item.rank = rank;
            candidate.budget.rank = rank;
        }

        let packing_work = u64::try_from(candidates.len())
            .map_err(|_| AtlasError::InvalidConfig("relationship packing overflow".into()))?;
        if !work_ledger.try_charge(packing_work)? {
            return Err(AtlasError::InvalidConfig(
                "relationship packing exceeded its reserved request work".into(),
            ));
        }
        document.cost.work_units_consumed = work_ledger.consumed;
        let budgets: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.budget.clone())
            .collect();
        let outcome =
            enforce_precharged_additional_deep_v2_budget(document, &budgets, None, work_ledger)?;
        let selected: std::collections::HashSet<&str> = outcome
            .selected_candidate_ids
            .iter()
            .map(String::as_str)
            .collect();
        for (candidate_id, reason) in &outcome.omitted_candidates {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.item.item_id == *candidate_id)
            {
                record_role_deficit(deficits, candidate.item.role, *reason);
            }
        }
        for candidate in &candidates {
            if selected.contains(candidate.item.item_id.as_str()) {
                if candidate.item.entity_kind == EntityKind::Symbol {
                    if let Some(symbol) = candidate.item.entity_id.strip_prefix("symbol:") {
                        frontier.push_back((
                            symbol.to_string(),
                            candidate.item.item_id.clone(),
                            candidate.item.distance,
                        ));
                    }
                }
                document.working_set.push(candidate.item.clone());
            }
        }
        for (item_id, relationship) in relationships {
            if item_id
                .as_deref()
                .is_none_or(|item_id| selected.contains(item_id))
            {
                document.relationships.push(relationship);
            }
        }
        document.cost = outcome.cost;
        document.omissions = outcome.omissions;
        if graph_requirements_satisfied(role_recipe, &document.working_set) {
            break;
        }
    }
    document
        .relationships
        .sort_by(|left, right| left.relationship_id.cmp(&right.relationship_id));
    document
        .relationships
        .dedup_by(|left, right| left.relationship_id == right.relationship_id);
    Ok(())
}

fn rebuild_deep_v2_validation_plan(document: &mut DeepContextIrV2, deficits: &[DeepV2RoleDeficit]) {
    let validation_deficit = deficits
        .iter()
        .filter(|entry| entry.0 == ItemRole::ValidationTarget)
        .map(|entry| entry.1)
        .min_by_key(|reason| role_deficit_priority(*reason));
    document.validation_plan = if validation_deficit.is_some() {
        Vec::new()
    } else {
        document
            .working_set
            .iter()
            .filter(|item| item.role == ItemRole::ValidationTarget)
            .map(|item| IrValidationV2 {
                kind: ValidationKind::Test,
                target_entity_id: item.entity_id.clone(),
                reason: IrValidationReasonV2::AffectedTest,
                required: true,
            })
            .collect()
    };
    document
        .validation_plan
        .sort_by(|left, right| left.target_entity_id.cmp(&right.target_entity_id));
    document
        .validation_plan
        .dedup_by(|left, right| left.target_entity_id == right.target_entity_id);
    document.status.validation = if let Some(reason) = validation_deficit {
        requirement_state_for_reason(reason)
    } else if document.validation_plan.is_empty() {
        IrRequirementStateV2::Unavailable
    } else {
        IrRequirementStateV2::Satisfied
    };
}

fn deep_v2_role_sufficiency(
    recipe: DeepV2RoleRecipe,
    working_set: &[IrWorkingSetItemV2],
    deficits: &[DeepV2RoleDeficit],
) -> Vec<IrRoleSufficiencyV2> {
    recipe
        .role_matrix()
        .map(|(role, required)| {
            let mut evidence_item_ids: Vec<String> = working_set
                .iter()
                .filter(|item| item.role == role)
                .map(|item| item.item_id.clone())
                .collect();
            evidence_item_ids.sort();
            let evidence_reason = working_set
                .iter()
                .filter(|item| item.role == role)
                .filter_map(|item| evidence_reason_for_quality(item.evidence.state))
                .min_by_key(|reason| role_deficit_priority(*reason));
            let recorded_reason = deficits
                .iter()
                .filter(|entry| entry.0 == role)
                .map(|entry| entry.1)
                .min_by_key(|reason| role_deficit_priority(*reason));
            let reason = evidence_reason.or(recorded_reason);
            let qualified = !evidence_item_ids.is_empty()
                && evidence_reason.is_none()
                && recorded_reason.is_none();
            let (state, reason) = if qualified {
                (IrRequirementStateV2::Satisfied, None)
            } else if let Some(reason) = reason {
                (requirement_state_for_reason(reason), Some(reason))
            } else if required {
                (
                    IrRequirementStateV2::Missing,
                    Some(IrOmissionReasonV2::MissingRequiredRole),
                )
            } else {
                (
                    IrRequirementStateV2::Unavailable,
                    Some(IrOmissionReasonV2::UnavailableEvidence),
                )
            };
            IrRoleSufficiencyV2 {
                role,
                required,
                state,
                evidence_item_ids,
                reason,
            }
        })
        .collect()
}

fn deep_v2_coverage(conn: &Connection, generation_id: &str) -> Result<IrCoverageV2> {
    let (complete, partial, unsupported, excluded, failed): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT indexed_file_count, partial_file_count, unsupported_file_count,
                    excluded_file_count, failed_file_count
             FROM index_generation WHERE generation_id = ?1 AND state = 'committed'",
            [generation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let convert = |value: i64| {
        u64::try_from(value)
            .map_err(|_| AtlasError::InvalidConfig("invalid generation coverage count".into()))
    };
    let complete = convert(complete)?;
    let partial = convert(partial)?;
    let unsupported = convert(unsupported)?;
    let excluded = convert(excluded)?;
    let failed = convert(failed)?;
    let eligible = complete
        .checked_add(partial)
        .and_then(|value| value.checked_add(unsupported))
        .and_then(|value| value.checked_add(excluded))
        .and_then(|value| value.checked_add(failed))
        .ok_or_else(|| AtlasError::InvalidConfig("generation coverage count overflow".into()))?;
    let mut deficits = Vec::new();
    if partial > 0 || failed > 0 {
        deficits.push(IrOmissionReasonV2::UnavailableEvidence);
    }
    if unsupported > 0 {
        deficits.push(IrOmissionReasonV2::UnsupportedEvidence);
    }
    deficits.sort();
    deficits.dedup();
    Ok(IrCoverageV2 {
        eligible,
        complete,
        partial,
        unsupported,
        excluded,
        failed,
        claim_strength: if eligible == 0 {
            ClaimStrength::None
        } else if deficits.is_empty() {
            ClaimStrength::ExhaustiveWithinScope
        } else {
            ClaimStrength::Bounded
        },
        deficits,
    })
}

fn validate_deep_v2_compile_request(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    request: &DeepV2CompileRequest,
) -> Result<()> {
    let session = crate::task_session::load_task_session(conn, &request.task_session_id)?
        .ok_or_else(|| {
            AtlasError::InvalidConfig(format!(
                "task session {} does not exist",
                request.task_session_id
            ))
        })?;
    let expected_session_id = crate::resolution::deterministic_id(
        "task",
        &[
            &ws.workspace_id,
            generation_id,
            &request.task_hash,
            &request.normalized_goal_hash,
            task_kind_identity(request.task_kind),
            task_kind_source_identity(request.kind_source),
            request.kind_rule_id.as_deref().unwrap_or(""),
            "none",
            PLANNER_POLICY_VERSION,
            crate::context_ir::CONTEXT_SCHEMA_VERSION,
        ],
    );
    if request.task_session_id != expected_session_id
        || session.workspace_id != ws.workspace_id
        || session.start_generation_id != generation_id
        || session.task_hash != request.task_hash
        || session.normalized_goal_hash != request.normalized_goal_hash
        || session.task_kind != request.task_kind
        || session.context_ir_version != crate::context_ir::CONTEXT_SCHEMA_VERSION
    {
        return Err(AtlasError::InvalidConfig(
            "deep Context IR request does not match its legacy lifecycle session".into(),
        ));
    }
    let seed_count = request
        .seed_paths
        .len()
        .checked_add(request.seed_symbols.len())
        .ok_or_else(|| AtlasError::InvalidConfig("V2 seed count overflow".into()))?;
    if seed_count > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS as usize {
        return Err(AtlasError::InvalidConfig(
            "V2 seed count exceeds the execution envelope".into(),
        ));
    }
    require_active_generation(conn, ws, generation_id)
}

/// Compose and seal one task-conditioned Context IR 2.0.0 document from the
/// captured catalogue generation. This path never converts or persists legacy
/// Context IR.
pub fn compile_deep_context_ir_v2(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    request: &DeepV2CompileRequest,
    budget: &crate::context_route::DeepSemanticBudget,
    authorization: Option<DeepV2SourceMaterializationAuthorization>,
) -> Result<SealedDeepContextIrV2> {
    budget.validate().map_err(|error| {
        AtlasError::InvalidConfig(format!("invalid explicit V2 semantic budget: {error}"))
    })?;
    validate_deep_v2_compile_request(conn, ws, generation_id, request)?;

    let mut seed_paths = request.seed_paths.clone();
    seed_paths.sort();
    seed_paths.dedup();
    let mut seed_symbols = request.seed_symbols.clone();
    seed_symbols.sort();
    seed_symbols.dedup();
    let mut seeds = seed_paths.clone();
    seeds.extend(seed_symbols.iter().cloned());
    seeds.sort();
    seeds.dedup();
    let seed_count = u64::try_from(seeds.len())
        .map_err(|_| AtlasError::InvalidConfig("V2 seed count overflow".into()))?;
    if seed_count > budget.max_records || seed_count > budget.max_work_units {
        return Err(AtlasError::InvalidConfig(
            "V2 seed count exceeds its explicit semantic work bounds".into(),
        ));
    }
    let per_seed_candidate_limit = budget
        .max_records
        .checked_div(seed_count)
        .unwrap_or(budget.max_records)
        .max(1);

    let mut work_ledger = DeepWorkLedger::new(budget.max_work_units);
    let mut resolution = resolve_deep_v2_seed_candidates_with_ledger(
        conn,
        ws,
        generation_id,
        &seeds,
        per_seed_candidate_limit,
        &mut work_ledger,
    )?;
    if let Some(test_candidate) = resolution
        .candidates
        .iter()
        .find(|candidate| candidate.item.role == ItemRole::TestContract)
        .cloned()
    {
        let rank = u64::try_from(resolution.candidates.len())
            .map_err(|_| AtlasError::InvalidConfig("validation rank overflow".into()))?;
        resolution
            .candidates
            .push(deep_v2_validation_candidate(&test_candidate, rank)?);
    }

    let budget_candidates: Vec<DeepV2BudgetCandidate> = resolution
        .candidates
        .iter()
        .map(|candidate| candidate.budget.clone())
        .collect();
    let mut budget_outcome =
        enforce_deep_v2_budget_with_ledger(budget, &budget_candidates, &mut work_ledger)?;
    let mut role_deficits = Vec::new();
    for (candidate_id, reason) in &budget_outcome.omitted_candidates {
        if let Some(candidate) = resolution
            .candidates
            .iter()
            .find(|candidate| candidate.item.item_id == *candidate_id)
        {
            record_role_deficit(&mut role_deficits, candidate.item.role, *reason);
        }
    }
    let selected_ids: std::collections::HashSet<&str> = budget_outcome
        .selected_candidate_ids
        .iter()
        .map(String::as_str)
        .collect();
    let mut working_set: Vec<IrWorkingSetItemV2> = resolution
        .candidates
        .into_iter()
        .filter(|candidate| selected_ids.contains(candidate.item.item_id.as_str()))
        .map(|candidate| candidate.item)
        .collect();
    for (rank, item) in working_set.iter_mut().enumerate() {
        item.rank = u64::try_from(rank)
            .map_err(|_| AtlasError::InvalidConfig("selected rank overflow".into()))?;
    }
    merge_prior_omissions(&mut budget_outcome.omissions, &resolution.omissions)?;

    let (generation_sequence, source_tree_hash, provider_set_hash): (i64, Option<String>, String) =
        conn.query_row(
            "SELECT sequence_no, source_tree_hash, provider_set_hash
             FROM index_generation
             WHERE generation_id = ?1 AND workspace_id = ?2 AND state = 'committed'",
            params![generation_id, ws.workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let configuration_hash: String = conn.query_row(
        "SELECT configuration_hash FROM workspace WHERE workspace_id = ?1",
        [&ws.workspace_id],
        |row| row.get(0),
    )?;
    let provider_set_hash = if provider_set_hash.len() == 64
        && provider_set_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        provider_set_hash
    } else {
        crate::hashing::content_hash_of_bytes(provider_set_hash.as_bytes())
    };

    let recipe = deep_v2_role_recipe(request.task_kind);
    let validation_target = working_set
        .iter()
        .find(|item| item.role == ItemRole::ValidationTarget)
        .map(|item| item.entity_id.clone());
    let validation_plan = validation_target
        .as_ref()
        .map(|target| {
            vec![IrValidationV2 {
                kind: ValidationKind::Test,
                target_entity_id: target.clone(),
                reason: IrValidationReasonV2::TaskRecipe,
                required: true,
            }]
        })
        .unwrap_or_default();
    let exact_source = if working_set
        .iter()
        .any(|item| item.role == ItemRole::PrimaryImplementation && item.source.is_some())
    {
        IrRequirementStateV2::Satisfied
    } else {
        IrRequirementStateV2::Unavailable
    };
    let validation = if validation_plan.is_empty() {
        IrRequirementStateV2::Unavailable
    } else {
        IrRequirementStateV2::Satisfied
    };
    let role_sufficiency = deep_v2_role_sufficiency(recipe, &working_set, &role_deficits);

    let source_dependencies: std::collections::BTreeMap<String, String> = working_set
        .iter()
        .filter_map(|item| {
            item.source.as_ref().map(|source| {
                (
                    source.canonical_path.clone(),
                    source.whole_file_sha256.clone(),
                )
            })
        })
        .collect();
    let dependencies = source_dependencies
        .into_iter()
        .map(|(key, digest)| IrLeaseDependencyV2 {
            kind: IrLeaseDependencyKindV2::SourceDigest,
            key,
            digest,
        })
        .collect();
    let seed_summary = crate::context_route::SeedSummary::from_targets(&seed_paths, &seed_symbols)
        .map_err(|error| AtlasError::InvalidConfig(format!("invalid V2 seeds: {error}")))?;
    let context_id = crate::resolution::deterministic_id(
        "ctx-v2",
        &[
            &ws.workspace_id,
            generation_id,
            &request.task_hash,
            &request.normalized_goal_hash,
            task_kind_identity(request.task_kind),
            task_kind_source_identity(request.kind_source),
            request.kind_rule_id.as_deref().unwrap_or(""),
            PLANNER_POLICY_V2_VERSION,
            &budget.budget_digest,
            &seed_summary.digest,
        ],
    );
    let mut document = DeepContextIrV2 {
        schema_version: crate::context_ir::CONTEXT_SCHEMA_V2_VERSION.to_string(),
        context_id,
        workspace: IrWorkspaceV2 {
            workspace_id: ws.workspace_id.clone(),
            generation_id: generation_id.to_string(),
            generation_sequence,
            source_tree_hash,
            configuration_hash,
            provider_set_hash,
        },
        task: IrTaskV2 {
            task_session_id: request.task_session_id.clone(),
            task_hash: request.task_hash.clone(),
            normalized_goal_hash: request.normalized_goal_hash.clone(),
            task_kind: request.task_kind,
            kind_source: request.kind_source,
            kind_rule_id: request.kind_rule_id.clone(),
            seed_paths,
            seed_symbols,
        },
        policy: IrPolicyV2 {
            planner_policy_version: PLANNER_POLICY_V2_VERSION.to_string(),
            projection_policy_version: crate::context_route::DEEP_PROJECTION_VERSION.to_string(),
            estimator_version: crate::context_route::DEEP_ESTIMATOR_VERSION.to_string(),
            deterministic: true,
            semantic_budget: budget_outcome.semantic_budget,
        },
        status: IrStatusV2 {
            working_set_status: WorkingSetStatus::Blocked,
            role_sufficiency,
            exact_source,
            validation,
            reasons: Vec::new(),
        },
        working_set,
        relationships: Vec::<IrRelationship>::new(),
        effects: Vec::<IrEffect>::new(),
        temporal_constraints: IrTemporalConstraintsV2 {
            current_generation_id: generation_id.to_string(),
            baseline_generation_id: request.baseline_generation_id.clone(),
            state: IrTemporalStateV2::NotRequired,
            evidence_item_ids: Vec::new(),
            omission_reason: None,
        },
        uncertainty: resolution.uncertainty,
        coverage: deep_v2_coverage(conn, generation_id)?,
        omissions: budget_outcome.omissions,
        validation_plan,
        evidence_lease: IrEvidenceLeaseV2 {
            generation_id: generation_id.to_string(),
            dependencies,
        },
        cost: budget_outcome.cost,
        context_hash: "0".repeat(64),
    };
    populate_deep_v2_relationship_closure(
        conn,
        ws,
        &mut document,
        &mut work_ledger,
        &mut role_deficits,
    )?;
    populate_deep_v2_effect_closure(conn, &mut document, &mut work_ledger, &mut role_deficits)?;
    rebuild_deep_v2_validation_plan(&mut document, &role_deficits);
    document.status.role_sufficiency =
        deep_v2_role_sufficiency(recipe, &document.working_set, &role_deficits);
    document =
        populate_deep_v2_temporal_evidence_with_ledger(conn, ws, document, &mut work_ledger)?;
    document
        .evidence_lease
        .dependencies
        .sort_by(|left, right| left.key.cmp(&right.key));
    document
        .evidence_lease
        .dependencies
        .dedup_by(|left, right| left.key == right.key);
    reconcile_deep_v2_status(&mut document);
    seal_deep_context_ir_v2_with_live_sources(conn, ws, document, authorization)
}

/// One canonical endpoint of a relationship. `value` never includes the
/// `symbol:`/`path:`/`external:` transport prefix.
#[derive(Debug, Clone)]
struct HopEndpoint {
    kind: EntityKind,
    value: String,
}

impl HopEndpoint {
    fn entity_id(&self) -> String {
        match self.kind {
            EntityKind::Symbol => format!("symbol:{}", self.value),
            EntityKind::File => format!("path:{}", self.value),
            _ => format!("external:{}", self.value),
        }
    }

    fn from_entity_id(entity_id: &str) -> Self {
        if let Some(value) = entity_id.strip_prefix("symbol:") {
            Self {
                kind: EntityKind::Symbol,
                value: value.to_string(),
            }
        } else if let Some(value) = entity_id.strip_prefix("path:") {
            Self {
                kind: EntityKind::File,
                value: value.to_string(),
            }
        } else if let Some(value) = entity_id.strip_prefix("external:") {
            Self {
                kind: EntityKind::External,
                value: value.to_string(),
            }
        } else {
            Self {
                kind: EntityKind::External,
                value: entity_id.to_string(),
            }
        }
    }
}

fn artifact_for_endpoint(
    conn: &Connection,
    gen_id: &str,
    endpoint: &HopEndpoint,
) -> Result<Option<(String, bool)>> {
    match endpoint.kind {
        EntityKind::Symbol => Ok(conn
            .query_row(
                "SELECT fr.artifact_class, fr.is_test
                 FROM symbol_fact AS sf INDEXED BY idx_symbol_key
                 CROSS JOIN generation_file AS gf INDEXED BY idx_generation_file_revision
                 JOIN file_revision fr ON fr.revision_id = gf.revision_id
                 WHERE sf.canonical_symbol_key = ?2
                   AND gf.generation_id = ?1 AND gf.revision_id = sf.revision_id
                   AND gf.presence_state = 'present'
                 ORDER BY (sf.evidence_method = 'semantic') DESC,
                          sf.confidence DESC, sf.symbol_fact_id
                 LIMIT 1",
                params![gen_id, endpoint.value],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?),
        EntityKind::File => Ok(conn
            .query_row(
                "SELECT fr.artifact_class, fr.is_test
                 FROM generation_file gf
                 JOIN file_revision fr ON fr.revision_id = gf.revision_id
                 WHERE gf.generation_id = ?1 AND gf.canonical_path = ?2
                   AND gf.presence_state = 'present'",
                params![gen_id, endpoint.value],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?),
        _ => Ok(None),
    }
}

fn task_specific_role_for_endpoint(
    conn: &Connection,
    gen_id: &str,
    endpoint: &HopEndpoint,
) -> Result<Option<ItemRole>> {
    Ok(
        artifact_for_endpoint(conn, gen_id, endpoint)?.and_then(|(artifact_class, is_test)| {
            task_specific_artifact_role(&artifact_class, is_test)
        }),
    )
}

/// One relationship adjacent to a seeded symbol. The raw target remains
/// provenance; canonical endpoints come only from relationship resolution.
struct Hop {
    relationship_fact_id: String,
    source: HopEndpoint,
    target: HopEndpoint,
    raw_target_ref_kind: String,
    raw_target_ref_value: String,
    relationship_type: String,
    resolution_state: ResolutionStatus,
    confidence: f64,
}

impl Hop {
    fn neighbor_of(&self, canonical_symbol_key: &str) -> Option<(&HopEndpoint, bool)> {
        if self.source.kind == EntityKind::Symbol && self.source.value == canonical_symbol_key {
            Some((&self.target, false))
        } else if self.target.kind == EntityKind::Symbol
            && self.target.value == canonical_symbol_key
        {
            Some((&self.source, true))
        } else {
            None
        }
    }
}

fn expand_hops(
    serving_reader: &mut serving::ServingReader<'_>,
    canonical_symbol_key: &str,
    row_limit: usize,
    filter: serving::RelationshipReadFilter,
) -> Result<(Vec<Hop>, bool)> {
    let prior_examinations = serving_reader.examinations().total();
    let maximum_examinations = serving_reader.maximum_relationship_examinations(row_limit);
    let slice = serving_reader.relationships(canonical_symbol_key, row_limit, filter)?;
    let hops = slice
        .rows
        .into_iter()
        .map(|row| Hop {
            relationship_fact_id: row.relationship_fact_id,
            source: HopEndpoint::from_entity_id(&row.source_entity_id),
            target: HopEndpoint::from_entity_id(&row.target_entity_id),
            raw_target_ref_kind: row.raw_target_ref_kind,
            raw_target_ref_value: row.raw_target_ref_value,
            relationship_type: row.relationship_type,
            resolution_state: ResolutionStatus::from_db_str(&row.resolution_state)
                .unwrap_or(ResolutionStatus::Unresolved),
            confidence: row.confidence,
        })
        .collect();
    debug_assert!(
        serving_reader
            .examinations()
            .total()
            .saturating_sub(prior_examinations)
            <= maximum_examinations
    );
    if serving_reader.validation_mismatch() {
        return Err(AtlasError::Other(SERVING_VALIDATION_MISMATCH.into()));
    }
    Ok((hops, slice.truncated))
}

fn recipe_includes(
    recipe: &EvidenceRecipe,
    relationship_type: &str,
    inbound: bool,
    artifact_role: Option<ItemRole>,
) -> bool {
    match artifact_role {
        Some(ItemRole::TestContract) if !recipe.include_tests => return false,
        Some(ItemRole::ConfigurationInput) if !recipe.include_configs => return false,
        _ => {}
    }
    match relationship_type {
        "calls" if inbound => recipe.include_callers,
        "calls" => recipe.include_callees,
        "tests" => recipe.include_tests,
        "configures" => recipe.include_configs,
        "references" | "implements" | "imports" | "extends" => true,
        _ => false,
    }
}

fn evidence_quality_for(evidence_state: &str, confidence: f64) -> EvidenceQuality {
    match evidence_state {
        "semantic" | "exact" if confidence >= 0.9 => EvidenceQuality::Verified,
        "structural" | "declared" | "observed" => EvidenceQuality::Supported,
        _ if confidence >= 0.5 => EvidenceQuality::Inferred,
        _ => EvidenceQuality::Unsupported,
    }
}

#[derive(Debug, Clone, Copy)]
enum BudgetLimit {
    Records,
    SourceBytes,
    EstimatedTokens,
}

#[derive(Debug, Clone, Copy)]
enum CandidateOrigin {
    ExplicitSeed,
    PathSeed,
    Relationship,
}

fn budget_rejection(
    request: &CompileRequest,
    selected_records: usize,
    selected_source_bytes: i64,
    selected_estimated_tokens: i64,
    cost: &IrItemCost,
) -> Option<BudgetLimit> {
    if selected_records as i64 >= request.max_records {
        Some(BudgetLimit::Records)
    } else if selected_source_bytes + cost.source_bytes > request.max_source_bytes {
        Some(BudgetLimit::SourceBytes)
    } else if selected_estimated_tokens + cost.estimated_tokens > request.max_estimated_tokens {
        Some(BudgetLimit::EstimatedTokens)
    } else {
        None
    }
}

fn budget_omission_reason(origin: CandidateOrigin, limit: BudgetLimit) -> &'static str {
    match (origin, limit) {
        (CandidateOrigin::ExplicitSeed, BudgetLimit::Records) => "explicit_seed_record_budget",
        (CandidateOrigin::ExplicitSeed, BudgetLimit::SourceBytes) => "explicit_seed_source_budget",
        (CandidateOrigin::ExplicitSeed, BudgetLimit::EstimatedTokens) => {
            "explicit_seed_token_budget"
        }
        (CandidateOrigin::PathSeed, BudgetLimit::Records) => "path_seed_record_budget",
        (CandidateOrigin::PathSeed, BudgetLimit::SourceBytes) => "path_seed_source_budget",
        (CandidateOrigin::PathSeed, BudgetLimit::EstimatedTokens) => "path_seed_token_budget",
        (CandidateOrigin::Relationship, _) => "relationship_record_budget",
    }
}

fn record_omission(by_reason: &mut std::collections::BTreeMap<String, i64>, reason: &str) {
    if let Some(count) = by_reason.get_mut(reason) {
        *count += 1;
    } else {
        by_reason.insert(reason.to_string(), 1);
    }
}

fn push_seed_symbol(
    working_set: &mut Vec<IrWorkingSetItem>,
    seen_entities: &mut std::collections::HashSet<String>,
    ordinal: &mut i64,
    gen_id: &str,
    symbol: &SeedSymbol,
    selection_reason: SelectionReason,
) -> bool {
    let entity_id = format!("symbol:{}", symbol.canonical_symbol_key);
    if !seen_entities.insert(entity_id) {
        return false;
    }
    let rank = *ordinal;
    working_set.push(IrWorkingSetItem {
        item_id: format!("item_{rank}"),
        entity_kind: EntityKind::Symbol,
        entity_id: symbol.canonical_symbol_key.clone(),
        role: role_for_seed_symbol(symbol),
        selection_reason,
        origin_id: None,
        distance: 0,
        rank,
        evidence: IrEvidence {
            state: evidence_quality_for(&symbol.evidence_state, symbol.confidence),
            confidence: symbol.confidence,
            provider_fingerprint: None,
            source_revision_hash: None,
            generation_id: gen_id.to_string(),
            preferred: Some(true),
            alternative_count: Some(0),
        },
        cost: estimate_item_cost(symbol.end_byte - symbol.start_byte),
        source: Some(crate::context_ir::IrSourceRef {
            path: symbol.canonical_path.clone(),
            start_byte: symbol.start_byte,
            end_byte: symbol.end_byte,
            indexed_hash: symbol.content_hash.clone(),
            observed_hash: None,
            verification_status: SourceVerificationStatus::NotRequested,
        }),
    });
    *ordinal += 1;
    true
}

fn defer_relationship(
    deferred_relationships: &mut Vec<IrRelationship>,
    relationship: IrRelationship,
    max_records: i64,
) {
    if (deferred_relationships.len() as i64) < max_records {
        deferred_relationships.push(relationship);
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_relationship_frontier(
    conn: &Connection,
    gen_id: &str,
    serving_reader: &mut serving::ServingReader<'_>,
    recipe: &EvidenceRecipe,
    relationship_row_bound: usize,
    frontier: &mut std::collections::VecDeque<(String, i64)>,
    request: &CompileRequest,
    working_set: &mut Vec<IrWorkingSetItem>,
    relationships: &mut Vec<IrRelationship>,
    deferred_relationships: &mut Vec<IrRelationship>,
    uncertainty: &mut Vec<crate::context_ir::IrNotice>,
    seen_entities: &mut std::collections::HashSet<String>,
    seen_relationships: &mut std::collections::HashSet<String>,
    candidates_considered: &mut i64,
    omission_reasons: &mut std::collections::BTreeMap<String, i64>,
    selected_source_bytes: &mut i64,
    selected_estimated_tokens: &mut i64,
    ordinal: &mut i64,
) -> Result<()> {
    let relationship_filter = serving::RelationshipReadFilter {
        include_callers: recipe.include_callers,
        include_callees: recipe.include_callees,
        include_tests: recipe.include_tests,
        include_configs: recipe.include_configs,
    };
    while let Some((current_key, distance)) = frontier.pop_front() {
        if distance >= recipe.max_relationship_distance {
            continue;
        }
        let (hops, read_truncated) = expand_hops(
            serving_reader,
            &current_key,
            relationship_row_bound,
            relationship_filter,
        )?;
        if read_truncated {
            *candidates_considered += 1;
            record_omission(omission_reasons, "relationship_record_budget");
            uncertainty.push(crate::context_ir::IrNotice {
                code: "relationship_frontier_truncated".to_string(),
                severity: crate::context_ir::NoticeSeverity::Warning,
                message: format!(
                    "relationship frontier exceeded the bounded read limit: {current_key}"
                ),
                entity_ids: vec![format!("symbol:{current_key}")],
            });
        }
        for hop in hops {
            let neighbor_context = hop.neighbor_of(&current_key);
            let artifact_role = if let Some((neighbor, inbound)) = neighbor_context {
                let artifact_role = task_specific_role_for_endpoint(conn, gen_id, neighbor)?;
                if !recipe_includes(recipe, &hop.relationship_type, inbound, artifact_role) {
                    continue;
                }
                artifact_role
            } else {
                None
            };
            if !seen_relationships.insert(hop.relationship_fact_id.clone()) {
                continue;
            }
            *candidates_considered += 1;

            let relationship = IrRelationship {
                relationship_id: crate::resolution::deterministic_id(
                    "rel",
                    &[gen_id, &hop.relationship_fact_id],
                ),
                source_entity_id: hop.source.entity_id(),
                target_entity_id: hop.target.entity_id(),
                relationship_type: hop.relationship_type.clone(),
                resolution_state: hop.resolution_state,
                evidence: IrEvidence {
                    state: match hop.resolution_state {
                        ResolutionStatus::ResolvedSymbol
                        | ResolutionStatus::ResolvedFile
                        | ResolutionStatus::External => EvidenceQuality::Resolved,
                        _ => EvidenceQuality::Unresolved,
                    },
                    confidence: hop.confidence,
                    provider_fingerprint: None,
                    source_revision_hash: None,
                    generation_id: gen_id.to_string(),
                    preferred: Some(true),
                    alternative_count: Some(0),
                },
            };

            if matches!(
                hop.resolution_state,
                ResolutionStatus::Ambiguous
                    | ResolutionStatus::Unresolved
                    | ResolutionStatus::Invalid
                    | ResolutionStatus::Stale
            ) {
                uncertainty.push(crate::context_ir::IrNotice {
                    code: format!("relationship_{}", hop.resolution_state.as_str()),
                    severity: crate::context_ir::NoticeSeverity::Warning,
                    message: format!(
                        "{} {} target remains {}: {}",
                        hop.relationship_type,
                        hop.raw_target_ref_kind,
                        hop.resolution_state.as_str(),
                        hop.raw_target_ref_value,
                    ),
                    entity_ids: vec![hop.source.entity_id(), hop.target.entity_id()],
                });
                let omission_reason = match hop.resolution_state {
                    ResolutionStatus::Ambiguous => "relationship_ambiguous",
                    ResolutionStatus::Unresolved => "relationship_unresolved",
                    ResolutionStatus::Invalid => "relationship_invalid",
                    ResolutionStatus::Stale => "relationship_stale",
                    _ => unreachable!("guarded unresolved relationship state"),
                };
                record_omission(omission_reasons, omission_reason);
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            }

            let Some((neighbor, inbound)) = neighbor_context else {
                record_omission(omission_reasons, "relationship_not_adjacent");
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            };
            let neighbor_entity_id = neighbor.entity_id();
            if neighbor.kind == EntityKind::External {
                record_omission(omission_reasons, "relationship_external");
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            }
            if seen_entities.contains(&neighbor_entity_id) {
                record_omission(omission_reasons, "duplicate_entity");
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            }

            let cost = IrItemCost {
                metadata_bytes: 64,
                source_bytes: 0,
                estimated_tokens: 0,
            };
            if let Some(limit) = budget_rejection(
                request,
                working_set.len(),
                *selected_source_bytes,
                *selected_estimated_tokens,
                &cost,
            ) {
                record_omission(
                    omission_reasons,
                    budget_omission_reason(CandidateOrigin::Relationship, limit),
                );
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            }
            if relationships.len() as i64 >= request.max_records {
                record_omission(omission_reasons, "relationship_record_budget");
                defer_relationship(deferred_relationships, relationship, request.max_records);
                continue;
            }

            relationships.push(relationship);
            seen_entities.insert(neighbor_entity_id);
            let (role, reason) = role_and_reason_for(
                &hop.relationship_type,
                inbound,
                neighbor.kind,
                artifact_role,
            );
            let rank = *ordinal;
            working_set.push(IrWorkingSetItem {
                item_id: format!("item_{rank}"),
                entity_kind: neighbor.kind,
                entity_id: neighbor.value.clone(),
                role,
                selection_reason: reason,
                origin_id: Some(current_key.clone()),
                distance: distance + 1,
                rank,
                evidence: IrEvidence {
                    state: EvidenceQuality::Resolved,
                    confidence: hop.confidence,
                    provider_fingerprint: None,
                    source_revision_hash: None,
                    generation_id: gen_id.to_string(),
                    preferred: Some(true),
                    alternative_count: Some(0),
                },
                cost,
                source: if neighbor.kind == EntityKind::File {
                    Some(crate::context_ir::IrSourceRef {
                        path: neighbor.value.clone(),
                        start_byte: 0,
                        end_byte: 0,
                        indexed_hash: String::new(),
                        observed_hash: None,
                        verification_status: SourceVerificationStatus::NotRequested,
                    })
                } else {
                    None
                },
            });
            *ordinal += 1;
            if neighbor.kind == EntityKind::Symbol {
                frontier.push_back((neighbor.value.clone(), distance + 1));
            }
        }
    }
    Ok(())
}

fn enforce_hard_deadline(
    metrics: &QueryMetricCollector<'_>,
    connection: &Connection,
    identity: &QueryMetricIdentity<'_>,
    hard_latency_ms: i64,
) -> Result<()> {
    if metrics.exceeds_millis(hard_latency_ms) {
        metrics.persist(connection, identity, "context_ir_compile_interrupted")?;
        return Err(AtlasError::Other(
            "context_ir_interrupted: hard latency budget elapsed; no canonical Context IR was produced"
                .into(),
        ));
    }
    Ok(())
}

/// Compile a bounded, deterministic Context IR for one task session against
/// the workspace's current active generation. No embeddings, no learned
/// ranking: selection is BFS distance (seed=0, direct neighbors=1, ...)
/// bounded by `recipe.max_relationship_distance`, with role/selection-
/// reason assigned by a fixed priority table.
#[allow(clippy::too_many_arguments)]
pub fn compile_context_ir(
    conn: &Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<String>,
    request: &CompileRequest,
) -> Result<ContextIr> {
    let clock = SystemMetricClock::start();
    compile_context_ir_with_clock(
        conn,
        ws,
        task_session_id,
        task_hash,
        normalized_goal_hash,
        task_kind,
        kind_source,
        kind_rule_id,
        request,
        &clock,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn compile_context_ir_with_clock(
    conn: &Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<String>,
    request: &CompileRequest,
    clock: &dyn MetricClock,
) -> Result<ContextIr> {
    let mut metrics = QueryMetricCollector::new(clock);
    let serving_examinations = std::cell::Cell::new(serving::ServingReadExaminations::default());
    match compile_context_ir_attempt(
        conn,
        ws,
        task_session_id,
        task_hash,
        normalized_goal_hash,
        task_kind,
        kind_source,
        kind_rule_id.clone(),
        request,
        &mut metrics,
        &serving_examinations,
        true,
    ) {
        Err(AtlasError::Other(message)) if message == SERVING_VALIDATION_MISMATCH => {
            compile_context_ir_attempt(
                conn,
                ws,
                task_session_id,
                task_hash,
                normalized_goal_hash,
                task_kind,
                kind_source,
                kind_rule_id,
                request,
                &mut metrics,
                &serving_examinations,
                false,
            )
        }
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_context_ir_attempt(
    conn: &Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<String>,
    request: &CompileRequest,
    metrics: &mut QueryMetricCollector<'_>,
    serving_examinations: &std::cell::Cell<serving::ServingReadExaminations>,
    allow_serving: bool,
) -> Result<ContextIr> {
    request.validate()?;
    let canonical_paths = request.canonical_paths();
    let canonical_symbols = request.canonical_symbols();
    let Some((gen_id, gen_seq)) = active_generation(conn, &ws.workspace_id)? else {
        return Ok(blocked_ir(
            ws,
            task_session_id,
            task_hash,
            normalized_goal_hash,
            task_kind,
            kind_source,
            kind_rule_id,
            request,
            "no_active_generation",
        ));
    };

    let config_hash: String = conn
        .query_row(
            "SELECT configuration_hash FROM workspace WHERE workspace_id = ?1",
            params![ws.workspace_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| placeholder_hash());
    let provider_set_hash: String = conn
        .query_row(
            "SELECT provider_set_hash FROM index_generation WHERE generation_id = ?1",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| placeholder_hash());

    // Prefer a ready Serving Plane projection under the default V1.2
    // policy; fall back to direct truth (and mark `serving_fallback`)
    // when none exists yet -- Context IR compilation never requires the
    // Serving Plane to function (G3's fallback-compatibility gate).
    let mut serving_reader = serving::ServingReader::new(
        conn,
        &ws.workspace_id,
        &gen_id,
        allow_serving,
        serving_examinations,
    )?;
    let serving_generation_id = serving_reader.serving_generation_id().map(str::to_owned);
    let serving_fallback = serving_generation_id.is_none();
    let request_hash = request.deterministic_hash();
    let metric_identity = QueryMetricIdentity {
        workspace_id: &ws.workspace_id,
        generation_id: &gen_id,
        task_session_id,
        request_id: &request_hash,
        serving_fallback,
    };
    metrics.checkpoint(QueryStage::ServingLookup);
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;

    let recipe = recipe_for(task_kind);
    let relationship_row_bound = usize::try_from(request.max_records)
        .map_err(|_| AtlasError::InvalidConfig("Serving read bound overflow".into()))?;

    let mut working_set: Vec<IrWorkingSetItem> = Vec::new();
    let mut relationships: Vec<IrRelationship> = Vec::new();
    let mut deferred_relationships: Vec<IrRelationship> = Vec::new();
    let mut uncertainty: Vec<crate::context_ir::IrNotice> = Vec::new();
    let mut seen_entities: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_relationships: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let has_explicit_seeds = !canonical_paths.is_empty() || !canonical_symbols.is_empty();
    let mut unresolved_explicit_seeds: Vec<(&'static str, String)> = Vec::new();
    let mut candidates_considered: i64 = 0;
    let mut ordinal: i64 = 0;
    let mut omission_reasons: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    let mut selected_source_bytes = 0;
    let mut selected_estimated_tokens = 0;
    let mut explicit_budget_omitted = false;

    // Step 1: resolve and select every explicit symbol before any broad path
    // enumeration or graph expansion can consume its record budget.
    let explicit_queries = canonical_symbols.clone();
    let mut explicit_matches = Vec::new();
    for seed in &explicit_queries {
        let mut matches = resolve_seed_symbol(conn, &gen_id, &mut serving_reader, seed)?;
        matches.sort_by(|a, b| {
            (&a.canonical_symbol_key, &a.canonical_path, &a.fact_id).cmp(&(
                &b.canonical_symbol_key,
                &b.canonical_path,
                &b.fact_id,
            ))
        });
        if matches.is_empty() {
            unresolved_explicit_seeds.push(("symbol", seed.clone()));
        }
        candidates_considered += matches.len() as i64;
        explicit_matches.extend(matches);
    }

    let mut explicit_frontier = std::collections::VecDeque::new();
    let explicit_canonical_keys: std::collections::HashSet<String> = explicit_matches
        .iter()
        .map(|symbol| symbol.canonical_symbol_key.clone())
        .collect();
    for symbol in &explicit_matches {
        let cost = estimate_item_cost(symbol.end_byte - symbol.start_byte);
        if let Some(limit) = budget_rejection(
            request,
            working_set.len(),
            selected_source_bytes,
            selected_estimated_tokens,
            &cost,
        ) {
            let reason = budget_omission_reason(CandidateOrigin::ExplicitSeed, limit);
            record_omission(&mut omission_reasons, reason);
            explicit_budget_omitted = true;
            uncertainty.push(crate::context_ir::IrNotice {
                code: "explicit_symbol_seed_budget_omitted".to_string(),
                severity: crate::context_ir::NoticeSeverity::Warning,
                message: format!(
                    "resolved explicit symbol omitted by {reason}: {}",
                    symbol.canonical_symbol_key,
                ),
                entity_ids: vec![format!("symbol:{}", symbol.canonical_symbol_key)],
            });
            continue;
        }
        if push_seed_symbol(
            &mut working_set,
            &mut seen_entities,
            &mut ordinal,
            &gen_id,
            symbol,
            SelectionReason::ExplicitSeed,
        ) {
            selected_source_bytes += cost.source_bytes;
            selected_estimated_tokens += cost.estimated_tokens;
            explicit_frontier.push_back((symbol.canonical_symbol_key.clone(), 0));
        } else {
            record_omission(&mut omission_reasons, "duplicate_entity");
        }
    }
    metrics.checkpoint(QueryStage::SeedResolution);
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;

    // Step 2: use one deterministic breadth-first frontier seeded by all named
    // symbols. This prevents the first explicit seed from starving later
    // explicit seeds and keeps every distance-0 seed ahead of graph evidence.
    expand_relationship_frontier(
        conn,
        &gen_id,
        &mut serving_reader,
        &recipe,
        relationship_row_bound,
        &mut explicit_frontier,
        request,
        &mut working_set,
        &mut relationships,
        &mut deferred_relationships,
        &mut uncertainty,
        &mut seen_entities,
        &mut seen_relationships,
        &mut candidates_considered,
        &mut omission_reasons,
        &mut selected_source_bytes,
        &mut selected_estimated_tokens,
        &mut ordinal,
    )?;
    metrics.checkpoint(QueryStage::GraphExpansion);
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;

    // Step 3: enumerate path-derived symbols only after explicit-symbol
    // expansion. The full unique set is known up front so budget omissions
    // remain exact even when selection stops early.
    let mut path_derived_keys = Vec::new();
    for path in &canonical_paths {
        let mut stmt = conn.prepare(
            "SELECT sf.canonical_symbol_key FROM current_file cf
             JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = ?2
             ORDER BY sf.canonical_symbol_key",
        )?;
        let keys: Vec<String> = stmt
            .query_map(params![gen_id, path], |r| r.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        if keys.is_empty() {
            unresolved_explicit_seeds.push(("path", path.clone()));
        }
        path_derived_keys.extend(keys);
    }
    path_derived_keys.sort();
    path_derived_keys.dedup();
    path_derived_keys.retain(|key| !explicit_canonical_keys.contains(key));
    // SCIP local identities are document-scoped by construction. Preserve
    // canonical order within both groups, but give globally meaningful path
    // symbols and their graph expansion a budget opportunity before broad
    // document-local enumeration.
    path_derived_keys.sort_unstable_by(|a, b| {
        a.starts_with("scip-local:")
            .cmp(&b.starts_with("scip-local:"))
            .then_with(|| a.cmp(b))
    });
    candidates_considered += path_derived_keys.len() as i64;

    for key in &path_derived_keys {
        let entity_id = format!("symbol:{key}");
        if seen_entities.contains(&entity_id) {
            record_omission(&mut omission_reasons, "duplicate_entity");
            continue;
        }

        let zero_cost = IrItemCost {
            metadata_bytes: 0,
            source_bytes: 0,
            estimated_tokens: 0,
        };
        if let Some(BudgetLimit::Records) = budget_rejection(
            request,
            working_set.len(),
            selected_source_bytes,
            selected_estimated_tokens,
            &zero_cost,
        ) {
            record_omission(
                &mut omission_reasons,
                budget_omission_reason(CandidateOrigin::PathSeed, BudgetLimit::Records),
            );
            continue;
        }

        let mut matches = resolve_seed_symbol(conn, &gen_id, &mut serving_reader, key)?;
        matches.sort_by(|a, b| {
            (&a.canonical_symbol_key, &a.canonical_path, &a.fact_id).cmp(&(
                &b.canonical_symbol_key,
                &b.canonical_path,
                &b.fact_id,
            ))
        });
        if matches.is_empty() {
            record_omission(&mut omission_reasons, "path_seed_resolution_missing");
            continue;
        }

        let mut path_frontier = std::collections::VecDeque::new();
        let mut selected_path_key = false;
        let mut terminal_budget_limit = None;
        for symbol in &matches {
            let cost = estimate_item_cost(symbol.end_byte - symbol.start_byte);
            if let Some(limit) = budget_rejection(
                request,
                working_set.len(),
                selected_source_bytes,
                selected_estimated_tokens,
                &cost,
            ) {
                terminal_budget_limit.get_or_insert(limit);
                continue;
            }
            if push_seed_symbol(
                &mut working_set,
                &mut seen_entities,
                &mut ordinal,
                &gen_id,
                symbol,
                SelectionReason::ExactPathMatch,
            ) {
                selected_path_key = true;
                selected_source_bytes += cost.source_bytes;
                selected_estimated_tokens += cost.estimated_tokens;
                path_frontier.push_back((symbol.canonical_symbol_key.clone(), 0));
                break;
            }
        }
        if !selected_path_key {
            if let Some(limit) = terminal_budget_limit {
                record_omission(
                    &mut omission_reasons,
                    budget_omission_reason(CandidateOrigin::PathSeed, limit),
                );
            } else {
                record_omission(&mut omission_reasons, "duplicate_entity");
            }
            continue;
        }

        expand_relationship_frontier(
            conn,
            &gen_id,
            &mut serving_reader,
            &recipe,
            relationship_row_bound,
            &mut path_frontier,
            request,
            &mut working_set,
            &mut relationships,
            &mut deferred_relationships,
            &mut uncertainty,
            &mut seen_entities,
            &mut seen_relationships,
            &mut candidates_considered,
            &mut omission_reasons,
            &mut selected_source_bytes,
            &mut selected_estimated_tokens,
            &mut ordinal,
        )?;
    }
    metrics.checkpoint(QueryStage::GraphExpansion);
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;
    let remaining_relationship_budget =
        (request.max_records - relationships.len() as i64).max(0) as usize;
    relationships.extend(
        deferred_relationships
            .into_iter()
            .take(remaining_relationship_budget),
    );
    let selected = working_set.len() as i64;
    let omitted = (candidates_considered - selected).max(0);
    let omission_reason_total: i64 = omission_reasons.values().sum();
    debug_assert_eq!(omission_reason_total, omitted);
    debug_assert_eq!(
        selected_source_bytes,
        working_set
            .iter()
            .map(|item| item.cost.source_bytes)
            .sum::<i64>(),
    );
    debug_assert_eq!(
        selected_estimated_tokens,
        working_set
            .iter()
            .map(|item| item.cost.estimated_tokens)
            .sum::<i64>(),
    );

    let truncated = omitted > 0;

    let unresolved_seed_count = unresolved_explicit_seeds.len();
    let unresolved_seed_severity = if selected == 0 {
        crate::context_ir::NoticeSeverity::Error
    } else {
        crate::context_ir::NoticeSeverity::Warning
    };
    for (kind, value) in unresolved_explicit_seeds {
        uncertainty.push(crate::context_ir::IrNotice {
            code: format!("explicit_{kind}_seed_unresolved"),
            severity: unresolved_seed_severity,
            message: format!("explicit {kind} seed failed to resolve: {value}"),
            entity_ids: vec![format!("{kind}:{value}")],
        });
    }
    uncertainty.sort_by(|a, b| {
        (&a.code, &a.entity_ids, &a.message).cmp(&(&b.code, &b.entity_ids, &b.message))
    });
    metrics.checkpoint(QueryStage::Ranking);
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;

    // `planner-v1.1.0` includes the fallback completeness marker in legacy
    // Context IR. T16 preserves that reader/writer contract; V2 canonical IR
    // keeps cache/readiness state exclusively in the execution envelope.
    let working_set_status = if selected == 0 && has_explicit_seeds {
        WorkingSetStatus::Blocked
    } else if unresolved_seed_count > 0 || truncated || serving_fallback {
        WorkingSetStatus::Partial
    } else {
        WorkingSetStatus::Complete
    };

    let by_reason = omission_reasons;
    let mut status_reasons = Vec::new();
    if serving_fallback {
        status_reasons.push("serving_plane_unavailable_used_direct_truth".to_string());
    }
    if explicit_budget_omitted {
        status_reasons.push("explicit_seed_budget_exhausted".to_string());
    }

    let context_id = crate::resolution::deterministic_id(
        "ctx",
        &[
            &ws.workspace_id,
            &gen_id,
            task_session_id,
            task_hash,
            task_kind_identity(task_kind),
            task_kind_source_identity(kind_source),
            kind_rule_id.as_deref().unwrap_or(""),
            PLANNER_POLICY_VERSION,
            &request_hash,
        ],
    );

    let ir = ContextIr::seal(
        context_id,
        IrWorkspace {
            workspace_id: ws.workspace_id.clone(),
            generation_id: gen_id.clone(),
            generation_sequence: gen_seq,
            source_tree_hash: None,
            configuration_hash: config_hash,
            provider_set_hash,
            serving_generation_id: serving_generation_id.clone(),
        },
        IrTask {
            task_session_id: task_session_id.to_string(),
            task_hash: task_hash.to_string(),
            normalized_goal_hash: normalized_goal_hash.to_string(),
            task_kind,
            kind_source,
            kind_rule_id,
            seed_paths: canonical_paths,
            seed_symbols: canonical_symbols,
        },
        IrPolicy::new(
            PLANNER_POLICY_VERSION.to_string(),
            serving::PROJECTION_POLICY_VERSION.to_string(),
            "standard".to_string(),
        ),
        IrStatus {
            working_set_status,
            reasons: status_reasons,
            serving_fallback,
        },
        working_set,
        relationships,
        vec![],
        uncertainty,
        IrCoverage {
            eligible: candidates_considered,
            complete: selected,
            partial: 0,
            unsupported: 0,
            excluded: 0,
            failed: 0,
            claim_strength: if candidates_considered == 0 {
                ClaimStrength::None
            } else if truncated {
                ClaimStrength::Bounded
            } else {
                ClaimStrength::ExhaustiveWithinScope
            },
            details: vec![],
        },
        IrOmissions {
            candidates_considered,
            selected,
            omitted,
            truncated,
            by_reason,
        },
        vec![],
        IrCost {
            max_records: request.max_records,
            selected_records: selected,
            max_source_bytes: request.max_source_bytes,
            selected_source_bytes,
            max_estimated_tokens: request.max_estimated_tokens,
            selected_estimated_tokens,
            soft_latency_ms: request.soft_latency_ms,
            hard_latency_ms: request.hard_latency_ms,
            elapsed_ms: 0,
        },
    );
    metrics.checkpoint(QueryStage::Packing);
    {
        let recorded = metrics.metrics_mut();
        recorded.candidate_count = candidates_considered;
        recorded.expanded_edge_count = seen_relationships.len() as i64;
        recorded.source_bytes_read = 0;
        recorded.returned_records = selected;
        recorded.returned_estimated_tokens = selected_estimated_tokens;
        recorded.cache_hits = i64::from(!serving_fallback);
        recorded.cache_misses = i64::from(serving_fallback);
        recorded.truncated = truncated;
    }
    enforce_hard_deadline(metrics, conn, &metric_identity, request.hard_latency_ms)?;
    debug_assert_eq!(metrics.metrics().prohibited_hot_path_work(), 0);
    metrics.persist(conn, &metric_identity, "context_ir_compile")?;
    Ok(ir)
}

fn placeholder_hash() -> String {
    "0".repeat(64)
}

fn estimate_item_cost(byte_span: i64) -> IrItemCost {
    let source_bytes = byte_span.max(0);
    IrItemCost {
        metadata_bytes: 96,
        source_bytes,
        estimated_tokens: source_bytes / 4,
    }
}

fn role_and_reason_for(
    relationship_type: &str,
    inbound: bool,
    neighbor_kind: EntityKind,
    artifact_role: Option<ItemRole>,
) -> (ItemRole, SelectionReason) {
    match artifact_role {
        Some(ItemRole::TestContract) => {
            return (ItemRole::TestContract, SelectionReason::TestRelation);
        }
        Some(ItemRole::ConfigurationInput) => {
            return (
                ItemRole::ConfigurationInput,
                SelectionReason::ConfigRelation,
            );
        }
        _ => {}
    }
    if inbound {
        return if relationship_type == "calls" && neighbor_kind == EntityKind::Symbol {
            (ItemRole::DirectDependent, SelectionReason::DirectCaller)
        } else if relationship_type == "tests" {
            (ItemRole::TestContract, SelectionReason::TestRelation)
        } else if relationship_type == "configures" {
            (
                ItemRole::ConfigurationInput,
                SelectionReason::ConfigRelation,
            )
        } else {
            (
                ItemRole::DirectDependent,
                SelectionReason::TaskRecipeRequirement,
            )
        };
    }
    match relationship_type {
        "calls" => (ItemRole::DirectDependency, SelectionReason::DirectCallee),
        "tests" => (ItemRole::TestContract, SelectionReason::TestRelation),
        "configures" => (
            ItemRole::ConfigurationInput,
            SelectionReason::ConfigRelation,
        ),
        "implements" | "extends" => (
            ItemRole::TypeContract,
            SelectionReason::ImplementationRelation,
        ),
        "imports" => (ItemRole::DirectDependency, SelectionReason::ImportNeighbor),
        _ => (
            ItemRole::DirectDependency,
            SelectionReason::TaskRecipeRequirement,
        ),
    }
}

/// No active generation: honestly blocked, not a fabricated empty-but-
/// complete result.
#[allow(clippy::too_many_arguments)]
fn blocked_ir(
    ws: &WorkspaceRecord,
    task_session_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<String>,
    request: &CompileRequest,
    reason: &str,
) -> ContextIr {
    let request_hash = request.deterministic_hash();
    let context_id = crate::resolution::deterministic_id(
        "ctx",
        &[
            &ws.workspace_id,
            "no_generation",
            task_session_id,
            task_hash,
            task_kind_identity(task_kind),
            task_kind_source_identity(kind_source),
            kind_rule_id.as_deref().unwrap_or(""),
            PLANNER_POLICY_VERSION,
            &request_hash,
        ],
    );
    ContextIr::seal(
        context_id,
        IrWorkspace {
            workspace_id: ws.workspace_id.clone(),
            generation_id: "none".to_string(),
            generation_sequence: 0,
            source_tree_hash: None,
            configuration_hash: placeholder_hash(),
            provider_set_hash: placeholder_hash(),
            serving_generation_id: None,
        },
        IrTask {
            task_session_id: task_session_id.to_string(),
            task_hash: task_hash.to_string(),
            normalized_goal_hash: normalized_goal_hash.to_string(),
            task_kind,
            kind_source,
            kind_rule_id,
            seed_paths: request.canonical_paths(),
            seed_symbols: request.canonical_symbols(),
        },
        IrPolicy::new(
            PLANNER_POLICY_VERSION.to_string(),
            serving::PROJECTION_POLICY_VERSION.to_string(),
            "standard".to_string(),
        ),
        IrStatus {
            working_set_status: WorkingSetStatus::Blocked,
            reasons: vec![reason.to_string()],
            serving_fallback: true,
        },
        vec![],
        vec![],
        vec![],
        vec![],
        IrCoverage {
            eligible: 0,
            complete: 0,
            partial: 0,
            unsupported: 0,
            excluded: 0,
            failed: 0,
            claim_strength: ClaimStrength::None,
            details: vec![reason.to_string()],
        },
        IrOmissions {
            candidates_considered: 0,
            selected: 0,
            omitted: 0,
            truncated: false,
            by_reason: Default::default(),
        },
        vec![],
        IrCost {
            max_records: request.max_records,
            selected_records: 0,
            max_source_bytes: request.max_source_bytes,
            selected_source_bytes: 0,
            max_estimated_tokens: request.max_estimated_tokens,
            selected_estimated_tokens: 0,
            soft_latency_ms: request.soft_latency_ms,
            hard_latency_ms: request.hard_latency_ms,
            elapsed_ms: 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::discovery;
    use crate::workspace::register_workspace;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, rusqlite::Connection, WorkspaceRecord) {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "import { b } from './b';\nexport function alpha() { return b(); }\n",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/b.ts"),
            "export function b() { return 2; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();
        (db_dir, conn, ws)
    }

    #[test]
    fn request_ledger_reserves_sql_rows_before_fetch_and_settles_actual_rows() {
        let mut ledger = DeepWorkLedger::new(3);
        let reservation = ledger.reserve_sql_rows(3).unwrap();
        assert_eq!(ledger.consumed, 3);
        assert_eq!(ledger.remaining(), 0);

        ledger.settle_sql_rows(reservation, 1).unwrap();
        assert_eq!(ledger.consumed, 1);
        assert_eq!(ledger.sql_rows_fetched, 1);
        assert_eq!(ledger.remaining(), 2);
    }

    #[test]
    fn request_ledger_never_exceeds_its_limit() {
        let mut ledger = DeepWorkLedger::new(2);
        assert!(ledger.try_charge(2).unwrap());
        assert!(!ledger.try_charge(1).unwrap());
        assert_eq!(ledger.consumed, 2);
        assert_eq!(ledger.remaining(), 0);
    }

    #[test]
    fn classify_task_prefers_declared_kind_over_keywords() {
        let (kind, source, rule) = classify_task(Some(TaskKind::Audit), "fix the bug");
        assert_eq!(kind, TaskKind::Audit);
        assert_eq!(source, TaskKindSource::Declared);
        assert!(rule.is_none());
    }

    #[test]
    fn classify_task_applies_deterministic_keyword_rules() {
        let (kind, source, rule) = classify_task(None, "please fix the broken retry logic");
        assert_eq!(kind, TaskKind::BugFix);
        assert_eq!(source, TaskKindSource::DeterministicRule);
        assert_eq!(rule.as_deref(), Some("rule_bugfix_keyword_fix"));
    }

    #[test]
    fn classify_task_is_deterministic_across_repeated_calls() {
        let a = classify_task(None, "refactor the connection controller");
        let b = classify_task(None, "refactor the connection controller");
        assert_eq!(a, b);
    }

    #[test]
    fn classify_task_does_not_match_keywords_inside_unrelated_words() {
        let (kind, source, rule) = classify_task(None, "improve capitalization metadata semantics");
        assert_eq!(kind, TaskKind::Unknown);
        assert_eq!(source, TaskKindSource::DeterministicRule);
        assert_eq!(rule.as_deref(), Some("rule_default_unknown"));
    }

    #[test]
    fn recipe_for_is_total_and_never_learned() {
        // Every TaskKind has an explicit, fixed recipe -- exercising all
        // ten proves `recipe_for` never falls through to a guessed default.
        for kind in [
            TaskKind::Explore,
            TaskKind::BugFix,
            TaskKind::BehaviorChange,
            TaskKind::ApiChange,
            TaskKind::Refactor,
            TaskKind::ConfigurationChange,
            TaskKind::TestChange,
            TaskKind::Review,
            TaskKind::Audit,
            TaskKind::Unknown,
        ] {
            let recipe = recipe_for(kind);
            assert_eq!(recipe.task_kind, kind);
            assert!(recipe.max_relationship_distance >= 1);
        }
    }

    #[test]
    fn compile_context_ir_is_bounded_and_deterministic() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            max_records: 5,
            ..Default::default()
        };
        let ir1 = compile_context_ir(
            &conn,
            &ws,
            "task_1",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        let ir2 = compile_context_ir(
            &conn,
            &ws,
            "task_1",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();

        assert_eq!(
            ir1.context_hash, ir2.context_hash,
            "identical compilation inputs must produce a byte-identical context_hash"
        );
        assert!(
            ir1.working_set.len() as i64 <= request.max_records,
            "working set must never exceed max_records"
        );
        assert!(
            ir1.working_set
                .iter()
                .any(|i| i.entity_id.contains("alpha")),
            "the explicit seed symbol must be in the working set"
        );
        assert!(ir1.policy.deterministic);
    }

    #[test]
    fn compile_context_ir_never_treats_unresolved_relationships_as_verified() {
        let (_d, conn, ws) = setup();
        // This fixture is structural-only; no relationship_resolution rows
        // exist, so every expanded relationship must surface as an
        // uncertainty notice, never silently added to the working set as
        // if it had been resolved.
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            max_records: 20,
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_2",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Refactor,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert!(ir
            .relationships
            .iter()
            .all(|r| r.resolution_state == crate::resolution::ResolutionStatus::Unresolved));
    }

    fn active_generation_id(conn: &Connection, ws: &WorkspaceRecord) -> String {
        conn.query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            params![ws.workspace_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn canonical_symbol(conn: &Connection, display_name: &str) -> String {
        conn.query_row(
            "SELECT canonical_symbol_key FROM current_symbol
             WHERE display_name = ?1 ORDER BY canonical_symbol_key LIMIT 1",
            params![display_name],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn revision_and_run(conn: &Connection, path: &str) -> (String, String) {
        conn.query_row(
            "SELECT cf.revision_id, sf.extractor_run_id
             FROM current_file cf
             JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
             WHERE cf.canonical_path = ?1
             ORDER BY sf.symbol_fact_id LIMIT 1",
            params![path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn add_relationship(
        conn: &Connection,
        ws: &WorkspaceRecord,
        fact_id: &str,
        source_path: &str,
        source_kind: &str,
        source_value: &str,
        target_kind: &str,
        raw_target_value: &str,
        resolution: Option<(&str, Option<&str>, Option<&str>)>,
    ) {
        add_typed_relationship(
            conn,
            ws,
            fact_id,
            source_path,
            "references",
            source_kind,
            source_value,
            target_kind,
            raw_target_value,
            resolution,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add_typed_relationship(
        conn: &Connection,
        ws: &WorkspaceRecord,
        fact_id: &str,
        source_path: &str,
        relationship_type: &str,
        source_kind: &str,
        source_value: &str,
        target_kind: &str,
        raw_target_value: &str,
        resolution: Option<(&str, Option<&str>, Option<&str>)>,
    ) {
        let (revision_id, extractor_run_id) = revision_and_run(conn, source_path);
        conn.execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                start_byte, end_byte, attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,NULL,'{}','semantic',0.95,'test fixture')",
            params![
                fact_id,
                extractor_run_id,
                revision_id,
                relationship_type,
                source_kind,
                source_value,
                target_kind,
                raw_target_value,
            ],
        )
        .unwrap();
        if let Some((status, resolved_kind, resolved_value)) = resolution {
            conn.execute(
                "INSERT INTO relationship_resolution (
                    relationship_resolution_id, workspace_id, generation_id,
                    relationship_fact_id, resolver_execution_id, resolver_policy_version,
                    status, resolved_ref_kind, resolved_ref_value, reason_code,
                    candidate_refs_json, confidence, evidence_json, created_at
                 ) VALUES (?1,?2,?3,?4,NULL,'test-policy',?5,?6,?7,'test','[]',0.95,'[]','2025-01-01T00:00:00Z')",
                params![
                    format!("resolution_{fact_id}"),
                    ws.workspace_id,
                    active_generation_id(conn, ws),
                    fact_id,
                    status,
                    resolved_kind,
                    resolved_value,
                ],
            )
            .unwrap();
        }
    }

    fn compile_for(conn: &Connection, ws: &WorkspaceRecord, request: &CompileRequest) -> ContextIr {
        compile_for_kind(conn, ws, TaskKind::Explore, request)
    }

    fn compile_for_kind(
        conn: &Connection,
        ws: &WorkspaceRecord,
        task_kind: TaskKind,
        request: &CompileRequest,
    ) -> ContextIr {
        compile_context_ir(
            conn,
            ws,
            "task_relationship",
            &"0".repeat(64),
            &"1".repeat(64),
            task_kind,
            TaskKindSource::Declared,
            None,
            request,
        )
        .unwrap()
    }

    #[test]
    fn equivalent_seed_sets_produce_identical_context_identity() {
        let (_d, conn, ws) = setup();
        let first = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec!["b".to_string(), "alpha".to_string()],
                ..Default::default()
            },
        );
        let reordered_with_duplicate = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec!["alpha".to_string(), "b".to_string(), "alpha".to_string()],
                ..Default::default()
            },
        );

        assert_eq!(first.context_id, reordered_with_duplicate.context_id);
        assert_eq!(first.context_hash, reordered_with_duplicate.context_hash);
        assert_eq!(
            first.task.seed_symbols,
            reordered_with_duplicate.task.seed_symbols
        );
    }

    #[test]
    fn task_kind_is_part_of_context_identity() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let explore = compile_for_kind(&conn, &ws, TaskKind::Explore, &request);
        let bug_fix = compile_for_kind(&conn, &ws, TaskKind::BugFix, &request);

        assert_ne!(explore.context_id, bug_fix.context_id);
        assert_ne!(explore.context_hash, bug_fix.context_hash);
    }

    #[test]
    fn invalid_compile_budgets_are_rejected_before_compilation() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            max_records: -1,
            ..Default::default()
        };
        let error = compile_context_ir(
            &conn,
            &ws,
            "task_invalid_budget",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap_err();

        assert!(matches!(error, crate::error::AtlasError::InvalidConfig(_)));
    }

    #[test]
    fn api_change_recipe_selects_callers_but_not_callees() {
        let (_caller_dir, caller_conn, caller_ws) = setup();
        let caller_alpha = canonical_symbol(&caller_conn, "alpha");
        let caller_b = canonical_symbol(&caller_conn, "b");
        add_typed_relationship(
            &caller_conn,
            &caller_ws,
            "rel_api_inbound",
            "src/b.ts",
            "calls",
            "symbol",
            &caller_b,
            "symbol",
            "raw-alpha",
            Some(("resolved_symbol", Some("symbol"), Some(&caller_alpha))),
        );
        let caller_ir = compile_for_kind(
            &caller_conn,
            &caller_ws,
            TaskKind::ApiChange,
            &CompileRequest {
                known_symbols: vec![caller_alpha],
                ..Default::default()
            },
        );
        assert!(caller_ir
            .working_set
            .iter()
            .any(|item| item.entity_id == caller_b));

        let (_callee_dir, callee_conn, callee_ws) = setup();
        let callee_alpha = canonical_symbol(&callee_conn, "alpha");
        let callee_b = canonical_symbol(&callee_conn, "b");
        add_typed_relationship(
            &callee_conn,
            &callee_ws,
            "rel_api_outbound",
            "src/a.ts",
            "calls",
            "symbol",
            &callee_alpha,
            "symbol",
            "raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&callee_b))),
        );
        let callee_ir = compile_for_kind(
            &callee_conn,
            &callee_ws,
            TaskKind::ApiChange,
            &CompileRequest {
                known_symbols: vec![callee_alpha],
                ..Default::default()
            },
        );
        assert!(!callee_ir
            .working_set
            .iter()
            .any(|item| item.entity_id == callee_b));
    }

    #[test]
    fn task_recipes_gate_and_label_test_and_configuration_artifacts() {
        let (_test_dir, test_conn, test_ws) = setup();
        let test_alpha = canonical_symbol(&test_conn, "alpha");
        let test_b = canonical_symbol(&test_conn, "b");
        let (test_revision, _) = revision_and_run(&test_conn, "src/b.ts");
        test_conn
            .execute(
                "UPDATE file_revision SET artifact_class = 'test', is_test = 1
                 WHERE revision_id = ?1",
                params![test_revision],
            )
            .unwrap();
        add_typed_relationship(
            &test_conn,
            &test_ws,
            "rel_test_import",
            "src/b.ts",
            "imports",
            "symbol",
            &test_b,
            "symbol",
            "raw-alpha",
            Some(("resolved_symbol", Some("symbol"), Some(&test_alpha))),
        );
        let bug_fix = compile_for_kind(
            &test_conn,
            &test_ws,
            TaskKind::BugFix,
            &CompileRequest {
                known_symbols: vec![test_alpha.clone()],
                ..Default::default()
            },
        );
        let test_item = bug_fix
            .working_set
            .iter()
            .find(|item| item.entity_id == test_b)
            .expect("bug-fix recipe must include a directly related test artifact");
        assert_eq!(test_item.role, ItemRole::TestContract);
        let explore = compile_for_kind(
            &test_conn,
            &test_ws,
            TaskKind::Explore,
            &CompileRequest {
                known_symbols: vec![test_alpha],
                ..Default::default()
            },
        );
        assert!(!explore
            .working_set
            .iter()
            .any(|item| item.entity_id == test_b));

        let (_config_dir, config_conn, config_ws) = setup();
        let config_alpha = canonical_symbol(&config_conn, "alpha");
        let config_b = canonical_symbol(&config_conn, "b");
        let (config_revision, _) = revision_and_run(&config_conn, "src/b.ts");
        config_conn
            .execute(
                "UPDATE file_revision SET artifact_class = 'configuration'
                 WHERE revision_id = ?1",
                params![config_revision],
            )
            .unwrap();
        add_typed_relationship(
            &config_conn,
            &config_ws,
            "rel_config_reference",
            "src/b.ts",
            "references",
            "symbol",
            &config_b,
            "symbol",
            "raw-alpha",
            Some(("resolved_symbol", Some("symbol"), Some(&config_alpha))),
        );
        let config_change = compile_for_kind(
            &config_conn,
            &config_ws,
            TaskKind::ConfigurationChange,
            &CompileRequest {
                known_symbols: vec![config_alpha.clone()],
                ..Default::default()
            },
        );
        let config_item = config_change
            .working_set
            .iter()
            .find(|item| item.entity_id == config_b)
            .expect("configuration-change recipe must include related configuration");
        assert_eq!(config_item.role, ItemRole::ConfigurationInput);
        let explore = compile_for_kind(
            &config_conn,
            &config_ws,
            TaskKind::Explore,
            &CompileRequest {
                known_symbols: vec![config_alpha],
                ..Default::default()
            },
        );
        assert!(!explore
            .working_set
            .iter()
            .any(|item| item.entity_id == config_b));
    }

    #[test]
    fn resolved_path_to_symbol_relationship_preserves_file_source() {
        let (_d, conn, ws) = setup();
        let target = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_path_symbol",
            "src/a.ts",
            "path",
            "src/a.ts",
            "symbol",
            "scip:raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&target))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![target.clone()],
                ..Default::default()
            },
        );
        assert!(ir.relationships.iter().any(|r| {
            r.source_entity_id == "path:src/a.ts"
                && r.target_entity_id == format!("symbol:{target}")
                && r.resolution_state == ResolutionStatus::ResolvedSymbol
        }));
        let source_file = ir
            .working_set
            .iter()
            .find(|item| item.entity_id == "src/a.ts")
            .unwrap();
        assert_eq!(source_file.entity_kind, EntityKind::File);
        assert_eq!(source_file.role, ItemRole::DirectDependent);
        assert_eq!(
            source_file.selection_reason,
            SelectionReason::TaskRecipeRequirement
        );
    }

    #[test]
    fn symbol_seed_does_not_inherit_unrelated_same_file_relationships() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_unrelated",
            "src/a.ts",
            "path",
            "src/a.ts",
            "symbol",
            "scip:raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha],
                ..Default::default()
            },
        );
        assert!(ir
            .relationships
            .iter()
            .all(|r| r.target_entity_id != format!("symbol:{b}")));
    }

    #[test]
    fn raw_scip_target_never_becomes_a_working_set_entity() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_raw",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "scip:raw-target-id",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha],
                ..Default::default()
            },
        );
        assert!(ir
            .working_set
            .iter()
            .all(|item| item.entity_id != "scip:raw-target-id"));
        assert!(ir
            .working_set
            .iter()
            .all(|item| item.entity_id != "symbol:scip:raw-target-id"));
    }

    #[test]
    fn resolved_ref_value_is_used_as_the_canonical_target() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_canonical",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "scip:raw-target-id",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha.clone()],
                ..Default::default()
            },
        );
        assert!(ir.relationships.iter().any(|r| {
            r.source_entity_id == format!("symbol:{alpha}")
                && r.target_entity_id == format!("symbol:{b}")
        }));
    }

    #[test]
    fn resolved_files_and_external_targets_keep_their_entity_kinds() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        add_relationship(
            &conn,
            &ws,
            "fact_file_target",
            "src/a.ts",
            "symbol",
            &alpha,
            "path",
            "raw/relative.ts",
            Some(("resolved_file", Some("file"), Some("src/b.ts"))),
        );
        add_relationship(
            &conn,
            &ws,
            "fact_external_target",
            "src/a.ts",
            "symbol",
            &alpha,
            "external",
            "scip:external-crate",
            Some(("external", Some("external_symbol"), Some("crate:external"))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha],
                ..Default::default()
            },
        );
        assert!(ir.relationships.iter().any(|r| {
            r.target_entity_id == "path:src/b.ts"
                && r.resolution_state == ResolutionStatus::ResolvedFile
        }));
        assert!(ir
            .working_set
            .iter()
            .any(|item| { item.entity_id == "src/b.ts" && item.entity_kind == EntityKind::File }));
        assert!(ir.relationships.iter().any(|r| {
            r.target_entity_id == "external:crate:external"
                && r.resolution_state == ResolutionStatus::External
        }));
        assert!(ir
            .working_set
            .iter()
            .all(|item| item.entity_id != "crate:external"));
    }

    #[test]
    fn ambiguous_relationship_target_remains_uncertainty_only() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        add_relationship(
            &conn,
            &ws,
            "fact_ambiguous",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "scip:ambiguous-target",
            Some(("ambiguous", None, None)),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha],
                ..Default::default()
            },
        );
        assert!(ir.uncertainty.iter().any(|notice| {
            notice.code == "relationship_ambiguous"
                && notice.message.contains("scip:ambiguous-target")
        }));
        assert!(ir
            .working_set
            .iter()
            .all(|item| !item.entity_id.contains("ambiguous-target")));
    }

    #[test]
    fn genuine_symbol_to_symbol_relationship_still_expands() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_symbol_symbol",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "scip:raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha.clone()],
                ..Default::default()
            },
        );
        assert!(ir.relationships.iter().any(|r| {
            r.source_entity_id == format!("symbol:{alpha}")
                && r.target_entity_id == format!("symbol:{b}")
        }));
        assert!(ir.working_set.iter().any(|item| {
            item.entity_id == b && item.entity_kind == EntityKind::Symbol && item.distance == 1
        }));
    }

    #[test]
    fn serving_and_direct_truth_relationships_are_semantically_equivalent() {
        let (_d, conn, ws) = setup();
        let target = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_parity",
            "src/a.ts",
            "path",
            "src/a.ts",
            "symbol",
            "scip:raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&target))),
        );
        let request = CompileRequest {
            known_symbols: vec![target],
            ..Default::default()
        };
        let direct = compile_for(&conn, &ws, &request);
        crate::serving::build_serving_generation(&conn, &ws).unwrap();
        let served = compile_for(&conn, &ws, &request);
        let semantic = |ir: &ContextIr| {
            let mut rows: Vec<_> = ir
                .relationships
                .iter()
                .map(|r| {
                    (
                        r.source_entity_id.clone(),
                        r.target_entity_id.clone(),
                        r.relationship_type.clone(),
                        r.resolution_state.as_str(),
                    )
                })
                .collect();
            rows.sort();
            rows
        };
        assert_eq!(semantic(&direct), semantic(&served));
    }

    fn assert_omission_partition(ir: &ContextIr) {
        let by_reason_total: i64 = ir.omissions.by_reason.values().sum();
        assert_eq!(
            by_reason_total, ir.omissions.omitted,
            "terminal omission reasons must partition omitted candidates exactly",
        );
        assert_eq!(
            ir.omissions.omitted,
            ir.omissions.candidates_considered - ir.omissions.selected,
        );
    }

    fn add_path_symbols(conn: &Connection, count: usize) {
        let (revision_id, extractor_run_id) = revision_and_run(conn, "src/a.ts");
        for index in 0..count {
            let key = format!("scip-local:src/a.ts:local {index:02}");
            conn.execute(
                "INSERT INTO symbol_fact (
                    symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
                    provider_symbol_id, symbol_kind, display_name, qualified_name, signature,
                    visibility, start_byte, end_byte, start_line, start_column, end_line,
                    end_column, documentation, attributes_json, evidence_method, confidence,
                    evidence_reason
                 ) VALUES (?1,?2,?3,?4,NULL,'function',?5,?5,NULL,NULL,0,8,1,1,1,1,NULL,'{}','semantic',0.95,'test fixture')",
                params![
                    format!("dummy_fact_{index:02}"), extractor_run_id, revision_id, key,
                    format!("path_symbol_{index:02}"),
                ],
            )
            .unwrap();
        }
    }

    #[test]
    fn explicit_symbols_and_their_graph_precede_large_path_seed_sets() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 10);
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_priority",
            "src/b.ts",
            "symbol",
            &b,
            "symbol",
            "scip:raw-alpha",
            Some(("resolved_symbol", Some("symbol"), Some(&alpha))),
        );
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![b.clone()],
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 3,
                ..Default::default()
            },
        );
        assert_eq!(ir.working_set.len(), 3);
        assert!(ir.working_set.iter().any(|item| {
            item.entity_id == b && item.selection_reason == SelectionReason::ExplicitSeed
        }));
        assert!(ir
            .working_set
            .iter()
            .any(|item| { item.entity_id == alpha && item.distance == 1 }));
        assert_eq!(
            ir.omissions.by_reason.get("path_seed_record_budget"),
            Some(&9)
        );
        assert_omission_partition(&ir);
    }

    #[test]
    fn cross_file_graph_precedes_broad_path_local_enumeration() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 20);
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_cross_file_priority",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        for index in 0..12 {
            add_relationship(
                &conn,
                &ws,
                &format!("fact_blocking_{index:02}"),
                "src/a.ts",
                "symbol",
                &alpha,
                "symbol",
                &format!("unresolved-{index:02}"),
                None,
            );
        }
        let request = CompileRequest {
            known_paths: vec!["src/a.ts".to_string()],
            max_records: 8,
            ..Default::default()
        };

        let first = compile_for(&conn, &ws, &request);
        let second = compile_for(&conn, &ws, &request);
        let graph_item = first
            .working_set
            .iter()
            .find(|item| item.entity_id == b)
            .expect("resolved cross-file graph evidence must enter the bounded packet");
        let first_local = first
            .working_set
            .iter()
            .find(|item| item.entity_id.starts_with("scip-local:"))
            .expect("path-local evidence remains eligible after higher-priority evidence");

        assert!(graph_item.rank < first_local.rank);
        assert_eq!(graph_item.distance, 1);
        assert!(first.relationships.iter().any(|relationship| {
            relationship.source_entity_id == format!("symbol:{alpha}")
                && relationship.target_entity_id == format!("symbol:{b}")
                && relationship.resolution_state == ResolutionStatus::ResolvedSymbol
        }));
        assert!(first
            .relationships
            .iter()
            .any(|relationship| { relationship.resolution_state == ResolutionStatus::Unresolved }));
        assert!(first
            .uncertainty
            .iter()
            .any(|notice| notice.code == "relationship_unresolved"));
        assert!(first
            .working_set
            .iter()
            .all(|item| !item.entity_id.starts_with("unresolved-")));
        assert_eq!(first.working_set.len(), request.max_records as usize);
        assert!(
            first
                .working_set
                .iter()
                .filter(|item| item.entity_id.starts_with("scip-local:"))
                .count()
                < request.max_records as usize,
            "path-local symbols must not monopolize the packet",
        );
        assert!(first.omissions.by_reason["path_seed_record_budget"] > 0);
        assert_omission_partition(&first);
        assert_eq!(first.context_hash, second.context_hash);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn path_local_evidence_appears_when_budget_allows_without_duplicate_entities() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 3);
        let alpha = canonical_symbol(&conn, "alpha");
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha.clone()],
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 20,
                ..Default::default()
            },
        );
        let entity_ids: std::collections::HashSet<_> = ir
            .working_set
            .iter()
            .map(|item| format!("{:?}:{}", item.entity_kind, item.entity_id))
            .collect();

        assert_eq!(ir.working_set[0].entity_id, alpha);
        assert_eq!(
            ir.working_set[0].selection_reason,
            SelectionReason::ExplicitSeed
        );
        assert!(ir
            .working_set
            .iter()
            .any(|item| item.entity_id.starts_with("scip-local:")));
        assert_eq!(entity_ids.len(), ir.working_set.len());
        assert!(ir.working_set.len() as i64 <= ir.cost.max_records);
        assert_omission_partition(&ir);
    }

    #[test]
    fn path_selection_enforces_source_and_token_budgets_truthfully() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        conn.execute(
            "UPDATE symbol_fact SET start_byte = 0, end_byte = 100
             WHERE canonical_symbol_key = ?1",
            params![alpha],
        )
        .unwrap();
        let source_limited = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 20,
                max_source_bytes: 20,
                max_estimated_tokens: 100,
                ..Default::default()
            },
        );
        let token_limited = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 20,
                max_source_bytes: 200,
                max_estimated_tokens: 5,
                ..Default::default()
            },
        );

        assert!(source_limited.cost.selected_source_bytes <= source_limited.cost.max_source_bytes);
        assert!(!source_limited
            .working_set
            .iter()
            .any(|item| item.entity_id == alpha));
        assert!(source_limited
            .omissions
            .by_reason
            .get("path_seed_source_budget")
            .is_some_and(|count| *count > 0));
        assert_omission_partition(&source_limited);
        assert!(
            token_limited.cost.selected_estimated_tokens <= token_limited.cost.max_estimated_tokens
        );
        assert!(!token_limited
            .working_set
            .iter()
            .any(|item| item.entity_id == alpha));
        assert!(token_limited
            .omissions
            .by_reason
            .get("path_seed_token_budget")
            .is_some_and(|count| *count > 0));
        assert_omission_partition(&token_limited);
    }

    #[test]
    fn explicit_seed_obeys_source_and_token_budgets_without_losing_resolved_identity() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        conn.execute(
            "UPDATE symbol_fact SET start_byte = 0, end_byte = 100
             WHERE canonical_symbol_key = ?1",
            params![alpha],
        )
        .unwrap();
        let source_limited = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha.clone()],
                max_source_bytes: 1,
                max_estimated_tokens: 100,
                ..Default::default()
            },
        );
        let token_limited = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha.clone()],
                max_source_bytes: 100,
                max_estimated_tokens: 1,
                ..Default::default()
            },
        );

        assert!(source_limited.cost.selected_source_bytes <= 1);
        assert!(token_limited.cost.selected_estimated_tokens <= 1);
        for ir in [&source_limited, &token_limited] {
            assert!(!ir.working_set.iter().any(|item| item.entity_id == alpha));
            assert!(ir.uncertainty.iter().any(|notice| {
                notice.entity_ids == vec![format!("symbol:{alpha}")]
                    && notice.code == "explicit_symbol_seed_budget_omitted"
            }));
            assert_omission_partition(ir);
        }
    }

    #[test]
    fn explicit_graph_and_path_items_share_one_hard_budget() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 3);
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        conn.execute(
            "UPDATE symbol_fact SET start_byte = 0, end_byte = 4
             WHERE canonical_symbol_key = ?1",
            params![alpha],
        )
        .unwrap();
        add_relationship(
            &conn,
            &ws,
            "fact_unified_budget",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let request = CompileRequest {
            known_symbols: vec![alpha.clone()],
            known_paths: vec!["src/a.ts".to_string()],
            max_records: 4,
            max_source_bytes: 4,
            max_estimated_tokens: 1,
            ..Default::default()
        };

        let first = compile_for(&conn, &ws, &request);
        let second = compile_for(&conn, &ws, &request);
        assert_eq!(first.working_set[0].entity_id, alpha);
        assert!(first
            .working_set
            .iter()
            .any(|item| item.entity_id == b && item.distance == 1));
        assert!(first.cost.selected_records <= request.max_records);
        assert!(first.cost.selected_source_bytes <= request.max_source_bytes);
        assert!(first.cost.selected_estimated_tokens <= request.max_estimated_tokens);
        assert_omission_partition(&first);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn mixed_budget_pressure_uses_only_terminal_path_reasons() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 6);
        let alpha = canonical_symbol(&conn, "alpha");
        conn.execute(
            "UPDATE symbol_fact SET start_byte = 0, end_byte = 4
             WHERE canonical_symbol_key = ?1",
            params![alpha],
        )
        .unwrap();
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_symbols: vec![alpha],
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 2,
                max_source_bytes: 4,
                max_estimated_tokens: 1,
                ..Default::default()
            },
        );

        assert_omission_partition(&ir);
        for aggregate in ["record_budget", "source_budget", "token_budget"] {
            assert!(
                !ir.omissions.by_reason.contains_key(aggregate),
                "aggregate reasons must not double-count terminal path reasons",
            );
        }
    }

    #[test]
    fn max_records_remains_hard_and_path_omissions_are_exact() {
        let (_d, conn, ws) = setup();
        add_path_symbols(&conn, 5);
        let ir = compile_for(
            &conn,
            &ws,
            &CompileRequest {
                known_paths: vec!["src/a.ts".to_string()],
                max_records: 2,
                ..Default::default()
            },
        );
        assert_eq!(ir.working_set.len(), 2);
        assert!(ir.omissions.truncated);
        assert_eq!(
            ir.omissions.by_reason.get("path_seed_record_budget"),
            Some(&4)
        );
        assert_omission_partition(&ir);
    }

    #[test]
    fn relationship_compilation_is_context_hash_deterministic() {
        let (_d, conn, ws) = setup();
        let alpha = canonical_symbol(&conn, "alpha");
        let b = canonical_symbol(&conn, "b");
        add_relationship(
            &conn,
            &ws,
            "fact_deterministic",
            "src/a.ts",
            "symbol",
            &alpha,
            "symbol",
            "scip:raw-b",
            Some(("resolved_symbol", Some("symbol"), Some(&b))),
        );
        let request = CompileRequest {
            known_symbols: vec![alpha],
            ..Default::default()
        };
        let first = compile_for(&conn, &ws, &request);
        let second = compile_for(&conn, &ws, &request);
        assert_eq!(first.context_hash, second.context_hash);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn zero_seeds_and_zero_eligible_never_claim_exhaustive_evidence() {
        let (_d, conn, ws) = setup();
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_zero",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &CompileRequest::default(),
        )
        .unwrap();
        assert_eq!(ir.coverage.eligible, 0);
        assert_eq!(ir.coverage.claim_strength, ClaimStrength::None);
    }

    #[test]
    fn unknown_symbol_seed_blocks_with_identifying_notice() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["missing::symbol".to_string()],
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_symbol",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert_eq!(ir.status.working_set_status, WorkingSetStatus::Blocked);
        assert_eq!(ir.uncertainty.len(), 1);
        assert_eq!(ir.uncertainty[0].code, "explicit_symbol_seed_unresolved");
        assert_eq!(
            ir.uncertainty[0].severity,
            crate::context_ir::NoticeSeverity::Error
        );
        assert_eq!(ir.uncertainty[0].entity_ids, vec!["symbol:missing::symbol"]);
        assert!(ir.uncertainty[0].message.contains("missing::symbol"));
    }

    #[test]
    fn unknown_path_seed_blocks_with_identifying_notice() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_paths: vec!["src/missing.ts".to_string()],
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_path",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert_eq!(ir.status.working_set_status, WorkingSetStatus::Blocked);
        assert_eq!(ir.uncertainty.len(), 1);
        assert_eq!(ir.uncertainty[0].code, "explicit_path_seed_unresolved");
        assert_eq!(
            ir.uncertainty[0].severity,
            crate::context_ir::NoticeSeverity::Error
        );
        assert_eq!(ir.uncertainty[0].entity_ids, vec!["path:src/missing.ts"]);
        assert!(ir.uncertainty[0].message.contains("src/missing.ts"));
    }

    #[test]
    fn mixed_valid_and_invalid_seeds_retain_evidence_and_report_uncertainty() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string(), "missing::symbol".to_string()],
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_mixed",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert!(ir
            .working_set
            .iter()
            .any(|item| item.entity_id.contains("alpha")));
        let notice = ir
            .uncertainty
            .iter()
            .find(|notice| notice.code == "explicit_symbol_seed_unresolved")
            .unwrap();
        assert_eq!(notice.severity, crate::context_ir::NoticeSeverity::Warning);
        assert_eq!(notice.entity_ids, vec!["symbol:missing::symbol"]);
    }

    #[test]
    fn unresolved_seed_notice_order_and_context_hash_are_deterministic() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_paths: vec!["z/missing.ts".to_string(), "a/missing.ts".to_string()],
            known_symbols: vec!["z::missing".to_string(), "a::missing".to_string()],
            ..Default::default()
        };
        let compile = || {
            compile_context_ir(
                &conn,
                &ws,
                "task_order",
                &"0".repeat(64),
                &"1".repeat(64),
                TaskKind::Explore,
                TaskKindSource::Declared,
                None,
                &request,
            )
            .unwrap()
        };
        let first = compile();
        let second = compile();
        let identities: Vec<&str> = first
            .uncertainty
            .iter()
            .map(|notice| notice.entity_ids[0].as_str())
            .collect();
        assert_eq!(
            identities,
            vec![
                "path:a/missing.ts",
                "path:z/missing.ts",
                "symbol:a::missing",
                "symbol:z::missing",
            ]
        );
        assert_eq!(first.context_hash, second.context_hash);
        assert_eq!(
            serde_json::to_vec(&first.uncertainty).unwrap(),
            serde_json::to_vec(&second.uncertainty).unwrap(),
        );
    }

    #[test]
    fn successful_seeded_compilation_keeps_exhaustive_within_scope_claim() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_success",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert!(ir.coverage.eligible > 0);
        assert_eq!(
            ir.coverage.claim_strength,
            ClaimStrength::ExhaustiveWithinScope
        );
    }

    #[test]
    fn compile_context_ir_reports_blocked_with_no_active_generation() {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let ir = compile_context_ir(
            &conn,
            &ws,
            "task_3",
            &"0".repeat(64),
            &"1".repeat(64),
            TaskKind::Explore,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert_eq!(ir.status.working_set_status, WorkingSetStatus::Blocked);
        assert!(ir.working_set.is_empty());
    }
}
