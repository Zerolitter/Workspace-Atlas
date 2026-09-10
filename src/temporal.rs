//! V1.4 temporal intelligence: bounded, deterministic explanations of what
//! changed between retained generations and which unchanged contracts remain
//! valid against live source.
//!
//! Temporal analysis never invents history. It compares the active committed
//! generation with a retained committed ancestor through `GenerationDelta`.
//! When no ancestor exists it returns an explicit baseline-only state. Claims
//! that a symbol contract remains current require a live content-hash match;
//! an indexed unchanged claim alone is insufficient.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::context_ir::{
    ChangeKind, DeltaChange, DeltaChangeSet, DeltaEvidenceState, EvidenceQuality, GenerationDelta,
    UnchangedContract,
};
use crate::error::{AtlasError, Result};
use crate::provider_contract::canonical_json_bytes;
use crate::workspace::WorkspaceRecord;

pub const TEMPORAL_SCHEMA_VERSION: &str = "1.0.0";
pub const TEMPORAL_POLICY_VERSION: &str = "temporal-v1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalHistoryState {
    NoActiveGeneration,
    BaselineOnly,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalValidityStatus {
    VerifiedCurrent,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalRiskLevel {
    None,
    Unknown,
    Low,
    Moderate,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalCategoryTotal {
    pub category: String,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalChangeRecord {
    pub category: String,
    pub entity_id: String,
    pub change_kind: ChangeKind,
    pub evidence_state: DeltaEvidenceState,
    #[serde(default)]
    pub before_id: Option<String>,
    #[serde(default)]
    pub after_id: Option<String>,
    pub reason_codes: Vec<String>,
    #[serde(default)]
    pub indexed_content_hash: Option<String>,
    #[serde(default)]
    pub observed_content_hash: Option<String>,
    #[serde(default)]
    pub current_source_status: Option<TemporalValidityStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalChangeSummary {
    pub records: Vec<TemporalChangeRecord>,
    pub category_totals: Vec<TemporalCategoryTotal>,
    pub total_matched: i64,
    pub omitted_count: i64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalValidityRecord {
    pub entity_id: String,
    #[serde(default)]
    pub canonical_path: Option<String>,
    #[serde(default)]
    pub indexed_content_hash: Option<String>,
    #[serde(default)]
    pub observed_content_hash: Option<String>,
    pub status: TemporalValidityStatus,
    pub claim_scope: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalValiditySummary {
    pub records: Vec<TemporalValidityRecord>,
    pub total_matched: i64,
    pub omitted_count: i64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalRiskSignal {
    pub code: String,
    pub level: TemporalRiskLevel,
    pub count: i64,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalRiskSummary {
    pub level: TemporalRiskLevel,
    pub signals: Vec<TemporalRiskSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalProvenance {
    pub derivation: String,
    #[serde(default)]
    pub delta_id: Option<String>,
    #[serde(default)]
    pub delta_hash: Option<String>,
    #[serde(default)]
    pub delta_policy_version: Option<String>,
    pub history_event_count: i64,
    pub live_verification_scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalReport {
    pub schema_version: String,
    pub temporal_policy_version: String,
    pub workspace_id: String,
    #[serde(default)]
    pub from_generation_id: Option<String>,
    #[serde(default)]
    pub to_generation_id: Option<String>,
    pub history_state: TemporalHistoryState,
    pub deterministic: bool,
    pub max_records_per_section: i64,
    pub changes: TemporalChangeSummary,
    pub validity: TemporalValiditySummary,
    pub risk: TemporalRiskSummary,
    pub provenance: TemporalProvenance,
    pub report_hash: String,
}

#[derive(Serialize)]
struct TemporalReportIdentity<'a> {
    schema_version: &'a str,
    temporal_policy_version: &'a str,
    workspace_id: &'a str,
    from_generation_id: &'a Option<String>,
    to_generation_id: &'a Option<String>,
    history_state: TemporalHistoryState,
    deterministic: bool,
    max_records_per_section: i64,
    changes: &'a TemporalChangeSummary,
    validity: &'a TemporalValiditySummary,
    risk: &'a TemporalRiskSummary,
    provenance: &'a TemporalProvenance,
}

impl TemporalReport {
    #[allow(clippy::too_many_arguments)]
    fn seal(
        workspace_id: String,
        from_generation_id: Option<String>,
        to_generation_id: Option<String>,
        history_state: TemporalHistoryState,
        max_records_per_section: i64,
        changes: TemporalChangeSummary,
        validity: TemporalValiditySummary,
        risk: TemporalRiskSummary,
        provenance: TemporalProvenance,
    ) -> Self {
        let identity = TemporalReportIdentity {
            schema_version: TEMPORAL_SCHEMA_VERSION,
            temporal_policy_version: TEMPORAL_POLICY_VERSION,
            workspace_id: &workspace_id,
            from_generation_id: &from_generation_id,
            to_generation_id: &to_generation_id,
            history_state,
            deterministic: true,
            max_records_per_section,
            changes: &changes,
            validity: &validity,
            risk: &risk,
            provenance: &provenance,
        };
        let report_hash = crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&identity));
        Self {
            schema_version: TEMPORAL_SCHEMA_VERSION.to_string(),
            temporal_policy_version: TEMPORAL_POLICY_VERSION.to_string(),
            workspace_id,
            from_generation_id,
            to_generation_id,
            history_state,
            deterministic: true,
            max_records_per_section,
            changes,
            validity,
            risk,
            provenance,
            report_hash,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TemporalEvidenceQualification {
    pub quality: EvidenceQuality,
    pub confidence: f64,
    pub uncertainty: bool,
}

pub(crate) fn change_record_reference(record: &TemporalChangeRecord) -> String {
    let change_kind = match record.change_kind {
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
        ChangeKind::Modified => "modified",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Retargeted => "retargeted",
        ChangeKind::StateChanged => "state_changed",
    };
    let live_status = match record.current_source_status {
        Some(TemporalValidityStatus::VerifiedCurrent) => "live_verified",
        Some(TemporalValidityStatus::Stale) => "live_stale",
        Some(TemporalValidityStatus::Unavailable) => "live_unavailable",
        None => "live_not_applicable",
    };
    format!(
        "temporal_change:{}:{change_kind}:{live_status}:{}",
        record.category, record.entity_id
    )
}

pub(crate) fn validity_record_reference(record: &TemporalValidityRecord) -> String {
    let status = match record.status {
        TemporalValidityStatus::VerifiedCurrent => "verified_current",
        TemporalValidityStatus::Stale => "stale",
        TemporalValidityStatus::Unavailable => "unavailable",
    };
    format!("temporal_validity:{status}:{}", record.entity_id)
}

pub(crate) fn qualify_change_evidence(
    record: &TemporalChangeRecord,
) -> TemporalEvidenceQualification {
    match record.current_source_status {
        Some(TemporalValidityStatus::Stale) => {
            qualify_validity_evidence(TemporalValidityStatus::Stale)
        }
        Some(TemporalValidityStatus::Unavailable) => {
            qualify_validity_evidence(TemporalValidityStatus::Unavailable)
        }
        _ => qualify_delta_evidence(record.evidence_state),
    }
}

pub(crate) fn qualify_delta_evidence(state: DeltaEvidenceState) -> TemporalEvidenceQualification {
    match state {
        DeltaEvidenceState::Verified => TemporalEvidenceQualification {
            quality: EvidenceQuality::Verified,
            confidence: 1.0,
            uncertainty: false,
        },
        DeltaEvidenceState::Supported => TemporalEvidenceQualification {
            quality: EvidenceQuality::Supported,
            confidence: 0.9,
            uncertainty: false,
        },
        DeltaEvidenceState::Inferred => TemporalEvidenceQualification {
            quality: EvidenceQuality::Inferred,
            confidence: 0.5,
            uncertainty: true,
        },
        DeltaEvidenceState::Uncertain => TemporalEvidenceQualification {
            quality: EvidenceQuality::Partial,
            confidence: 0.0,
            uncertainty: true,
        },
    }
}

pub(crate) fn qualify_validity_evidence(
    status: TemporalValidityStatus,
) -> TemporalEvidenceQualification {
    match status {
        TemporalValidityStatus::VerifiedCurrent => TemporalEvidenceQualification {
            quality: EvidenceQuality::Verified,
            confidence: 1.0,
            uncertainty: false,
        },
        TemporalValidityStatus::Stale => TemporalEvidenceQualification {
            quality: EvidenceQuality::Stale,
            confidence: 0.0,
            uncertainty: true,
        },
        TemporalValidityStatus::Unavailable => TemporalEvidenceQualification {
            quality: EvidenceQuality::Partial,
            confidence: 0.0,
            uncertainty: true,
        },
    }
}

#[derive(Debug, Clone)]
struct GenerationMeta {
    generation_id: String,
    parent_generation_id: Option<String>,
    sequence_no: i64,
}

fn committed_generation(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<GenerationMeta>> {
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    let generation = conn
        .query_row(
            "SELECT generation_id, parent_generation_id, sequence_no
             FROM index_generation
             WHERE generation_id = ?1 AND workspace_id = ?2 AND state = 'committed'",
            params![generation_id, workspace_id],
            |row| {
                Ok(GenerationMeta {
                    generation_id: row.get(0)?,
                    parent_generation_id: row.get(1)?,
                    sequence_no: row.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            AtlasError::Other(format!(
                "generation {generation_id} not found or not committed for workspace {workspace_id}"
            ))
        })?;
    Ok(Some(generation))
}

fn ensure_ancestor(
    conn: &Connection,
    workspace_id: &str,
    from_generation_id: &str,
    to_generation: &GenerationMeta,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<GenerationMeta>> {
    let Some(from_generation) =
        committed_generation(conn, workspace_id, from_generation_id, ledger)?
    else {
        return Ok(None);
    };
    let mut cursor = to_generation.parent_generation_id.clone();
    let mut visited = BTreeSet::new();
    while let Some(generation_id) = cursor {
        if !visited.insert(generation_id.clone()) {
            return Err(AtlasError::Other(
                "generation ancestry contains a cycle".to_string(),
            ));
        }
        let Some(generation) = committed_generation(conn, workspace_id, &generation_id, ledger)?
        else {
            return Ok(None);
        };
        if generation.generation_id == from_generation.generation_id {
            return Ok(Some(from_generation));
        }
        cursor = generation.parent_generation_id;
    }
    Err(AtlasError::InvalidConfig(format!(
        "generation {from_generation_id} is not an ancestor of active generation {}",
        to_generation.generation_id
    )))
}

fn empty_changes() -> TemporalChangeSummary {
    TemporalChangeSummary {
        records: Vec::new(),
        category_totals: Vec::new(),
        total_matched: 0,
        omitted_count: 0,
        truncated: false,
    }
}

fn empty_validity() -> TemporalValiditySummary {
    TemporalValiditySummary {
        records: Vec::new(),
        total_matched: 0,
        omitted_count: 0,
        truncated: false,
    }
}

fn unavailable_risk(code: &str) -> TemporalRiskSummary {
    TemporalRiskSummary {
        level: TemporalRiskLevel::Unknown,
        signals: vec![TemporalRiskSignal {
            code: code.to_string(),
            level: TemporalRiskLevel::Unknown,
            count: 1,
            evidence: vec!["local_generation_ancestry".to_string()],
        }],
    }
}

fn unavailable_report(
    ws: &WorkspaceRecord,
    to_generation_id: Option<String>,
    history_state: TemporalHistoryState,
    max_records: i64,
    signal_code: &str,
) -> TemporalReport {
    TemporalReport::seal(
        ws.workspace_id.clone(),
        None,
        to_generation_id,
        history_state,
        max_records,
        empty_changes(),
        empty_validity(),
        unavailable_risk(signal_code),
        TemporalProvenance {
            derivation: "local_generation_ancestry".to_string(),
            delta_id: None,
            delta_hash: None,
            delta_policy_version: None,
            history_event_count: 0,
            live_verification_scope: "none".to_string(),
        },
    )
}

fn push_category(
    records: &mut Vec<TemporalChangeRecord>,
    totals: &mut Vec<TemporalCategoryTotal>,
    category: &str,
    set: &DeltaChangeSet,
) {
    totals.push(TemporalCategoryTotal {
        category: category.to_string(),
        total: set.changes.len() as i64,
    });
    records.extend(set.changes.iter().map(|change| TemporalChangeRecord {
        category: category.to_string(),
        entity_id: change.entity_id.clone(),
        change_kind: change.change_kind,
        evidence_state: change.evidence_state,
        before_id: change.before_id.clone(),
        after_id: change.after_id.clone(),
        reason_codes: change.reason_codes.clone(),
        indexed_content_hash: None,
        observed_content_hash: None,
        current_source_status: None,
    }));
}

fn verify_live_hash(
    ws: &WorkspaceRecord,
    canonical_path: &str,
    indexed_content_hash: &str,
) -> (TemporalValidityStatus, Option<String>, &'static str) {
    let root = Path::new(&ws.canonical_root);
    let candidate = root.join(canonical_path);
    let Ok(relative) = crate::paths::confine_to_root(root, &candidate) else {
        return (
            TemporalValidityStatus::Unavailable,
            None,
            "source_path_not_confined",
        );
    };
    match crate::hashing::content_hash_of_file(&root.join(relative)) {
        Ok(observed) if observed == indexed_content_hash => (
            TemporalValidityStatus::VerifiedCurrent,
            Some(observed),
            "live_content_hash_matched",
        ),
        Ok(observed) => (
            TemporalValidityStatus::Stale,
            Some(observed),
            "live_content_hash_mismatch",
        ),
        Err(_) => (
            TemporalValidityStatus::Unavailable,
            None,
            "live_source_unreadable_or_missing",
        ),
    }
}

fn annotate_current_file_source(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    record: &mut TemporalChangeRecord,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<bool> {
    if record.category != "file" || record.change_kind == ChangeKind::Removed {
        return Ok(true);
    }
    if !ledger.try_charge_history_input_row()? {
        return Ok(false);
    }
    let indexed_content_hash: Option<String> = conn
        .query_row(
            "SELECT fr.content_hash
             FROM generation_file gf
             JOIN file_revision fr ON fr.revision_id = gf.revision_id
             WHERE gf.generation_id = ?1
               AND gf.canonical_path = ?2
               AND gf.presence_state = 'present'",
            params![generation_id, record.entity_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(indexed_content_hash) = indexed_content_hash else {
        record.current_source_status = Some(TemporalValidityStatus::Unavailable);
        return Ok(true);
    };
    let (status, observed_content_hash, _) =
        verify_live_hash(ws, &record.entity_id, &indexed_content_hash);
    record.indexed_content_hash = Some(indexed_content_hash);
    record.observed_content_hash = observed_content_hash;
    record.current_source_status = Some(status);
    Ok(true)
}

fn summarize_changes(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    delta: &GenerationDelta,
    max_records: usize,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalChangeSummary>> {
    let mut records = Vec::new();
    let mut category_totals = Vec::new();
    for (category, set) in [
        ("file", &delta.files),
        ("symbol", &delta.symbols),
        ("relationship", &delta.relationships),
        ("effect", &delta.effects),
        ("coverage", &delta.coverage),
        ("conflict", &delta.conflicts),
    ] {
        push_category(&mut records, &mut category_totals, category, set);
    }
    let total_matched = records.len() as i64;
    records.truncate(max_records);
    for record in &mut records {
        if !annotate_current_file_source(conn, ws, generation_id, record, ledger)? {
            return Ok(None);
        }
    }
    let omitted_count = total_matched - records.len() as i64;
    Ok(Some(TemporalChangeSummary {
        records,
        category_totals,
        total_matched,
        omitted_count,
        truncated: omitted_count > 0,
    }))
}

fn symbol_source_bindings(
    conn: &Connection,
    generation_id: &str,
    canonical_symbol_key: &str,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<Vec<(String, String)>>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT gf.canonical_path, fr.content_hash
         FROM generation_file gf
         JOIN file_revision fr ON fr.revision_id = gf.revision_id
         JOIN symbol_fact sf ON sf.revision_id = gf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE gf.generation_id = ?1
           AND gf.presence_state = 'present'
           AND sf.canonical_symbol_key = ?2
           AND er.status IN ('complete', 'partial')
         ORDER BY gf.canonical_path, fr.content_hash
         LIMIT 2",
    )?;
    let mut rows = statement.query(params![generation_id, canonical_symbol_key])?;
    let mut bindings = Vec::new();
    loop {
        if !ledger.try_charge_history_input_row()? {
            return Ok(None);
        }
        let Some(row) = rows.next()? else {
            return Ok(Some(bindings));
        };
        bindings.push((row.get(0)?, row.get(1)?));
    }
}

fn verify_contract(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    contract: &UnchangedContract,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalValidityRecord>> {
    let Some(bindings) = symbol_source_bindings(conn, generation_id, &contract.entity_id, ledger)?
    else {
        return Ok(None);
    };
    let Some((canonical_path, indexed_content_hash)) = (bindings.len() == 1)
        .then(|| bindings.into_iter().next())
        .flatten()
    else {
        let mut evidence = contract.evidence.clone();
        evidence.push("source_binding_missing_or_ambiguous".to_string());
        return Ok(Some(TemporalValidityRecord {
            entity_id: contract.entity_id.clone(),
            canonical_path: None,
            indexed_content_hash: None,
            observed_content_hash: None,
            status: TemporalValidityStatus::Unavailable,
            claim_scope: contract.claim_scope.clone(),
            evidence,
        }));
    };

    let (status, observed_content_hash, evidence_code) =
        verify_live_hash(ws, &canonical_path, &indexed_content_hash);
    let mut evidence = contract.evidence.clone();
    evidence.push(evidence_code.to_string());
    Ok(Some(TemporalValidityRecord {
        entity_id: contract.entity_id.clone(),
        canonical_path: Some(canonical_path),
        indexed_content_hash: Some(indexed_content_hash),
        observed_content_hash,
        status,
        claim_scope: contract.claim_scope.clone(),
        evidence,
    }))
}

fn summarize_validity(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    contracts: &[UnchangedContract],
    max_records: usize,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalValiditySummary>> {
    let total_matched = contracts.len() as i64;
    let mut records = Vec::with_capacity(contracts.len().min(max_records));
    for contract in contracts.iter().take(max_records) {
        let Some(record) = verify_contract(conn, ws, generation_id, contract, ledger)? else {
            return Ok(None);
        };
        records.push(record);
    }
    let omitted_count = total_matched - records.len() as i64;
    Ok(Some(TemporalValiditySummary {
        records,
        total_matched,
        omitted_count,
        truncated: omitted_count > 0,
    }))
}

fn add_risk_signal(
    signals: &mut Vec<TemporalRiskSignal>,
    code: &str,
    level: TemporalRiskLevel,
    count: i64,
    evidence: &[&str],
) {
    if count > 0 {
        signals.push(TemporalRiskSignal {
            code: code.to_string(),
            level,
            count,
            evidence: evidence.iter().map(|value| (*value).to_string()).collect(),
        });
    }
}

fn risk_rank(level: TemporalRiskLevel) -> u8 {
    match level {
        TemporalRiskLevel::None => 0,
        TemporalRiskLevel::Unknown => 1,
        TemporalRiskLevel::Low => 2,
        TemporalRiskLevel::Moderate => 3,
        TemporalRiskLevel::High => 4,
    }
}

fn summarize_risk(
    conn: &Connection,
    generation_id: &str,
    delta: &GenerationDelta,
    changes: &TemporalChangeSummary,
    validity: &TemporalValiditySummary,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalRiskSummary>> {
    let stale_count = validity
        .records
        .iter()
        .filter(|record| record.status == TemporalValidityStatus::Stale)
        .count() as i64
        + changes
            .records
            .iter()
            .filter(|record| record.current_source_status == Some(TemporalValidityStatus::Stale))
            .count() as i64;
    let unavailable_count = validity
        .records
        .iter()
        .filter(|record| record.status == TemporalValidityStatus::Unavailable)
        .count() as i64
        + changes
            .records
            .iter()
            .filter(|record| {
                record.current_source_status == Some(TemporalValidityStatus::Unavailable)
            })
            .count() as i64;
    let core_changes: Vec<&DeltaChange> = [
        &delta.files,
        &delta.symbols,
        &delta.relationships,
        &delta.effects,
    ]
    .into_iter()
    .flat_map(|set| &set.changes)
    .collect();
    let disruptive_change_count = core_changes
        .iter()
        .filter(|change| {
            matches!(
                change.change_kind,
                ChangeKind::Removed
                    | ChangeKind::Renamed
                    | ChangeKind::Retargeted
                    | ChangeKind::StateChanged
                    | ChangeKind::Modified
            )
        })
        .count() as i64;
    let additive_change_count = core_changes
        .iter()
        .filter(|change| change.change_kind == ChangeKind::Added)
        .count() as i64;
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    let unhealthy_coverage_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM coverage_record
         WHERE generation_id = ?1 AND status IN ('failed', 'stale')",
        params![generation_id],
        |row| row.get(0),
    )?;
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    let unresolved_relationship_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM relationship_resolution
         WHERE generation_id = ?1
           AND status IN ('unresolved', 'ambiguous', 'invalid', 'stale')",
        params![generation_id],
        |row| row.get(0),
    )?;
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    let open_conflict_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM evidence_conflict
         WHERE generation_id = ?1 AND status IN ('open', 'preferred_with_conflict')",
        params![generation_id],
        |row| row.get(0),
    )?;

    let mut signals = Vec::new();
    add_risk_signal(
        &mut signals,
        "live_source_stale",
        TemporalRiskLevel::High,
        stale_count,
        &["returned_changes_and_validity.live_content_hash"],
    );
    add_risk_signal(
        &mut signals,
        "live_source_unavailable",
        TemporalRiskLevel::High,
        unavailable_count,
        &["returned_changes_and_validity.live_content_hash"],
    );
    add_risk_signal(
        &mut signals,
        "delta_uncertainty",
        TemporalRiskLevel::High,
        delta.uncertainty.len() as i64,
        &["generation_delta.uncertainty"],
    );
    add_risk_signal(
        &mut signals,
        "open_evidence_conflicts",
        TemporalRiskLevel::High,
        open_conflict_count,
        &["evidence_conflict"],
    );
    add_risk_signal(
        &mut signals,
        "failed_or_stale_coverage",
        TemporalRiskLevel::High,
        unhealthy_coverage_count,
        &["coverage_record"],
    );
    add_risk_signal(
        &mut signals,
        "validity_evidence_omitted",
        TemporalRiskLevel::Moderate,
        validity.omitted_count,
        &["validity.omitted_count"],
    );
    add_risk_signal(
        &mut signals,
        "change_evidence_omitted",
        TemporalRiskLevel::Moderate,
        changes.omitted_count,
        &["changes.omitted_count"],
    );
    add_risk_signal(
        &mut signals,
        "disruptive_changes",
        TemporalRiskLevel::Moderate,
        disruptive_change_count,
        &["generation_delta"],
    );
    add_risk_signal(
        &mut signals,
        "unresolved_relationships",
        TemporalRiskLevel::Moderate,
        unresolved_relationship_count,
        &["relationship_resolution"],
    );
    add_risk_signal(
        &mut signals,
        "additive_changes",
        TemporalRiskLevel::Low,
        additive_change_count,
        &["generation_delta"],
    );
    if validity.total_matched == 0 && changes.total_matched == 0 {
        add_risk_signal(
            &mut signals,
            "unchanged_contracts_unavailable",
            TemporalRiskLevel::Unknown,
            1,
            &["generation_delta.unchanged_verified_contracts"],
        );
    }
    signals.sort_by(|left, right| left.code.cmp(&right.code));
    let level = signals
        .iter()
        .map(|signal| signal.level)
        .max_by_key(|level| risk_rank(*level))
        .unwrap_or(TemporalRiskLevel::None);
    Ok(Some(TemporalRiskSummary { level, signals }))
}

fn history_event_count(
    conn: &Connection,
    workspace_id: &str,
    from_sequence: i64,
    to_sequence: i64,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<i64>> {
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT COUNT(*)
         FROM lifecycle_event le
         JOIN index_generation generation ON generation.generation_id = le.generation_id
         WHERE le.workspace_id = ?1
           AND generation.sequence_no > ?2
           AND generation.sequence_no <= ?3",
        params![workspace_id, from_sequence, to_sequence],
        |row| row.get(0),
    )
    .map(Some)
    .map_err(Into::into)
}

/// Explain changes from a retained committed ancestor to the current active
/// generation. `max_records` independently bounds returned change and validity
/// details; aggregate counts and risk signals remain complete for indexed
/// evidence, while live verification is explicitly limited to returned
/// validity contracts.
pub fn explain_temporal_state(
    conn: &Connection,
    ws: &WorkspaceRecord,
    requested_from_generation_id: Option<&str>,
    max_records: i64,
) -> Result<TemporalReport> {
    let mut ledger = crate::generation_delta::UnboundedCanonicalHistoryLedger;
    if !crate::generation_delta::CanonicalHistoryLedger::try_charge_history_input_row(&mut ledger)?
    {
        return Err(AtlasError::Other(
            "unbounded active-generation observation exhausted".into(),
        ));
    }
    let active_generation_id = captured_active_generation(conn, &ws.workspace_id)?;
    match explain_temporal_state_for_captured_generation(
        conn,
        ws,
        active_generation_id,
        requested_from_generation_id,
        max_records,
        true,
        &mut ledger,
    )? {
        Some(report) => Ok(report),
        None => Err(AtlasError::Other(
            "unbounded canonical temporal derivation exhausted".into(),
        )),
    }
}

/// Canonical temporal derivation for a compiler that already captured its
/// generation identity. The active generation is read exactly once and must
/// still match that identity, so a deep document cannot mix generation pairs.
pub(crate) fn explain_temporal_state_for_generation_with_ledger(
    conn: &Connection,
    ws: &WorkspaceRecord,
    expected_active_generation_id: &str,
    requested_from_generation_id: Option<&str>,
    max_records: i64,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalReport>> {
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    let active_generation_id = captured_active_generation(conn, &ws.workspace_id)?;
    if active_generation_id.as_deref() != Some(expected_active_generation_id) {
        return Err(AtlasError::InvalidConfig(format!(
            "deep Context IR generation {} is not the active committed generation",
            expected_active_generation_id
        )));
    }
    explain_temporal_state_for_captured_generation(
        conn,
        ws,
        active_generation_id,
        requested_from_generation_id,
        max_records,
        false,
        ledger,
    )
}

fn captured_active_generation(conn: &Connection, workspace_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
        params![workspace_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn explain_temporal_state_for_captured_generation(
    conn: &Connection,
    ws: &WorkspaceRecord,
    active_generation_id: Option<String>,
    requested_from_generation_id: Option<&str>,
    max_records: i64,
    persist_delta: bool,
    ledger: &mut impl crate::generation_delta::CanonicalHistoryLedger,
) -> Result<Option<TemporalReport>> {
    if max_records <= 0 {
        return Err(AtlasError::InvalidConfig(format!(
            "max_records must be > 0, got {max_records}"
        )));
    }
    let Some(active_generation_id) = active_generation_id else {
        if requested_from_generation_id.is_some() {
            return Err(AtlasError::InvalidConfig(
                "cannot select a from generation without an active generation".to_string(),
            ));
        }
        return Ok(Some(unavailable_report(
            ws,
            None,
            TemporalHistoryState::NoActiveGeneration,
            max_records,
            "active_generation_unavailable",
        )));
    };
    let Some(to_generation) =
        committed_generation(conn, &ws.workspace_id, &active_generation_id, ledger)?
    else {
        return Ok(None);
    };
    let from_generation = match requested_from_generation_id {
        Some(generation_id) => {
            let Some(generation) = ensure_ancestor(
                conn,
                &ws.workspace_id,
                generation_id,
                &to_generation,
                ledger,
            )?
            else {
                return Ok(None);
            };
            Some(generation)
        }
        None => match to_generation.parent_generation_id.as_deref() {
            Some(generation_id) => {
                let Some(generation) =
                    committed_generation(conn, &ws.workspace_id, generation_id, ledger)?
                else {
                    return Ok(None);
                };
                Some(generation)
            }
            None => None,
        },
    };
    let Some(from_generation) = from_generation else {
        return Ok(Some(unavailable_report(
            ws,
            Some(active_generation_id),
            TemporalHistoryState::BaselineOnly,
            max_records,
            "prior_generation_unavailable",
        )));
    };

    let Some(delta) = crate::generation_delta::compute_generation_delta_with_ledger(
        conn,
        ws,
        &from_generation.generation_id,
        &to_generation.generation_id,
        ledger,
    )?
    else {
        return Ok(None);
    };
    if persist_delta {
        crate::generation_delta::persist_generation_delta(conn, &delta)?;
    }
    let max_records_usize = usize::try_from(max_records).unwrap_or(usize::MAX);
    let Some(changes) = summarize_changes(
        conn,
        ws,
        &to_generation.generation_id,
        &delta,
        max_records_usize,
        ledger,
    )?
    else {
        return Ok(None);
    };
    let Some(validity) = summarize_validity(
        conn,
        ws,
        &to_generation.generation_id,
        &delta.unchanged_verified_contracts,
        max_records_usize,
        ledger,
    )?
    else {
        return Ok(None);
    };
    let Some(risk) = summarize_risk(
        conn,
        &to_generation.generation_id,
        &delta,
        &changes,
        &validity,
        ledger,
    )?
    else {
        return Ok(None);
    };
    let Some(history_event_count) = history_event_count(
        conn,
        &ws.workspace_id,
        from_generation.sequence_no,
        to_generation.sequence_no,
        ledger,
    )?
    else {
        return Ok(None);
    };

    Ok(Some(TemporalReport::seal(
        ws.workspace_id.clone(),
        Some(from_generation.generation_id),
        Some(to_generation.generation_id),
        TemporalHistoryState::Available,
        max_records,
        changes,
        validity,
        risk,
        TemporalProvenance {
            derivation: "local_generation_delta".to_string(),
            delta_id: Some(delta.delta_id),
            delta_hash: Some(delta.delta_hash),
            delta_policy_version: Some(delta.delta_policy_version),
            history_event_count,
            live_verification_scope: "returned_changes_and_unchanged_contracts".to_string(),
        },
    )))
}
