//! V1.2 Generation Delta and evidence leases (G7).
//!
//! `compute_generation_delta` diffs two committed generations' truth
//! (`current_file`/`symbol_fact`/`relationship_fact`/`effect_fact`/
//! `coverage_record`/`evidence_conflict`) into a
//! `context_ir::GenerationDelta` document, persisted into the
//! `generation_delta` and `generation_delta_item` tables defined by
//! `migrations/0003_context_intelligence_foundation.sql`. An unchanged claim
//! requires both an identical content hash and identical resolved outbound
//! relationship targets; it is never inferred from an absence of observations.
//!
//! Evidence leases (`evidence_lease`) record that some piece of evidence
//! (a symbol or relationship) was valid as of a specific
//! `source_revision_hash`. `verify_evidence_lease` re-hashes the live file
//! on disk and auto-invalidates the lease on any mismatch — a lease can
//! never be trusted without this check; it is not an optional add-on
//! (G7's "cannot bypass source verification" gate).

use rusqlite::{params, Connection, OptionalExtension};

use crate::context_ir::{
    ChangeKind, ContextUseEvent, ContextUseEventType, DeltaChange, DeltaChangeSet,
    DeltaEvidenceState, GenerationDelta, IrEvidenceLeaseV2, IrLeaseDependencyKindV2,
    ObservationSource, UnchangedContract,
};
use crate::error::{AtlasError, Result};
use crate::resolution::deterministic_id;
use crate::workspace::WorkspaceRecord;

pub const DELTA_POLICY_VERSION: &str = "delta-v1.0.0";

/// Request-local accounting seam for canonical history input rows.
///
/// A charge is attempted before every `Rows::next` call, including the empty
/// sentinel that proves a result set is complete. These are semantic result-row
/// examinations only; they do not bound SQLite VM steps, internal sorting,
/// physical I/O, or wall time.
pub(crate) trait CanonicalHistoryLedger {
    fn try_charge_history_input_row(&mut self) -> Result<bool>;
}

pub(crate) struct UnboundedCanonicalHistoryLedger;

impl CanonicalHistoryLedger for UnboundedCanonicalHistoryLedger {
    fn try_charge_history_input_row(&mut self) -> Result<bool> {
        Ok(true)
    }
}

fn collect_canonical_rows<T>(
    rows: &mut rusqlite::Rows<'_>,
    ledger: &mut impl CanonicalHistoryLedger,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Option<Vec<T>>> {
    let mut values = Vec::new();
    loop {
        if !ledger.try_charge_history_input_row()? {
            return Ok(None);
        }
        let Some(row) = rows.next()? else {
            return Ok(Some(values));
        };
        values.push(map(row)?);
    }
}

fn require_unbounded<T>(value: Option<T>) -> Result<T> {
    value.ok_or_else(|| {
        AtlasError::Other("unbounded canonical history input read exhausted".to_string())
    })
}

fn change_kind_str(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
        ChangeKind::Modified => "modified",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Retargeted => "retargeted",
        ChangeKind::StateChanged => "state_changed",
    }
}

fn evidence_state_str(s: DeltaEvidenceState) -> &'static str {
    match s {
        DeltaEvidenceState::Verified => "verified",
        DeltaEvidenceState::Supported => "supported",
        DeltaEvidenceState::Inferred => "inferred",
        DeltaEvidenceState::Uncertain => "uncertain",
    }
}

/// A generation's file identity: `(file_id, canonical_path, content_hash)`.
struct FileRow {
    file_id: String,
    canonical_path: String,
    content_hash: String,
}

fn files_for_with_ledger(
    conn: &Connection,
    gen_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<Vec<FileRow>>> {
    // `current_file` is generation-scoped only for the *active* generation
    // (its view definition joins through `workspace.active_generation_id`).
    // Diffing two arbitrary generations therefore reads `generation_file`
    // directly instead of the view.
    let mut stmt = conn.prepare(
        "SELECT gf.file_id, gf.canonical_path, fr.content_hash
         FROM generation_file gf
         LEFT JOIN file_revision fr ON fr.revision_id = gf.revision_id
         WHERE gf.generation_id = ?1 AND gf.presence_state = 'present'
         ORDER BY gf.canonical_path",
    )?;
    let mut rows = stmt.query(params![gen_id])?;
    collect_canonical_rows(&mut rows, ledger, |r| {
        Ok(FileRow {
            file_id: r.get(0)?,
            canonical_path: r.get(1)?,
            content_hash: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        })
    })
}

fn files_for(conn: &Connection, gen_id: &str) -> Result<Vec<FileRow>> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(files_for_with_ledger(conn, gen_id, &mut ledger)?)
}

#[derive(serde::Serialize)]
struct SymbolSemanticIdentity<'a> {
    symbol_kind: &'a str,
    display_name: &'a str,
    qualified_name: &'a str,
    signature: Option<&'a str>,
    visibility: Option<&'a str>,
    documentation: Option<&'a str>,
    attributes_json: &'a str,
}

#[derive(serde::Serialize)]
struct SymbolEvidenceIdentity<'a> {
    evidence_method: &'a str,
    confidence_bits: u64,
    evidence_reason: &'a str,
}

struct SymbolSnapshot {
    canonical_symbol_key: String,
    symbol_fact_id: String,
    canonical_path: String,
    file_id: String,
    file_content_hash: String,
    extractor_complete: bool,
    provider_key: String,
    semantic_digest: String,
    evidence_digest: String,
}

fn semantic_digest<T: serde::Serialize>(value: &T) -> String {
    crate::hashing::content_hash_of_bytes(&crate::provider_contract::canonical_json_bytes(value))
}

fn symbols_for_with_ledger(
    conn: &Connection,
    gen_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<std::collections::BTreeMap<String, SymbolSnapshot>>> {
    let mut stmt = conn.prepare(
        "SELECT sf.canonical_symbol_key, sf.symbol_fact_id, sf.symbol_kind,
                sf.display_name, sf.qualified_name, sf.signature, sf.visibility,
                sf.documentation, sf.attributes_json, sf.evidence_method,
                sf.confidence, sf.evidence_reason, gf.file_id, gf.canonical_path,
                fr.content_hash, er.status, er.provider_name, er.provider_version
         FROM generation_file gf
         JOIN file_revision fr ON fr.revision_id = gf.revision_id
         JOIN symbol_fact sf ON sf.revision_id = gf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE gf.generation_id = ?1 AND gf.presence_state = 'present'
         ORDER BY sf.canonical_symbol_key, sf.symbol_fact_id",
    )?;
    let mut rows = stmt.query(params![gen_id])?;
    let rows = match collect_canonical_rows(&mut rows, ledger, |row| {
        let symbol_kind: String = row.get(2)?;
        let display_name: String = row.get(3)?;
        let qualified_name: String = row.get(4)?;
        let signature: Option<String> = row.get(5)?;
        let visibility: Option<String> = row.get(6)?;
        let documentation: Option<String> = row.get(7)?;
        let attributes_json: String = row.get(8)?;
        let evidence_method: String = row.get(9)?;
        let confidence: f64 = row.get(10)?;
        let evidence_reason: String = row.get(11)?;
        let semantic_identity = SymbolSemanticIdentity {
            symbol_kind: &symbol_kind,
            display_name: &display_name,
            qualified_name: &qualified_name,
            signature: signature.as_deref(),
            visibility: visibility.as_deref(),
            documentation: documentation.as_deref(),
            attributes_json: &attributes_json,
        };
        let evidence_identity = SymbolEvidenceIdentity {
            evidence_method: &evidence_method,
            confidence_bits: confidence.to_bits(),
            evidence_reason: &evidence_reason,
        };
        Ok(SymbolSnapshot {
            canonical_symbol_key: row.get(0)?,
            symbol_fact_id: row.get(1)?,
            file_id: row.get(12)?,
            canonical_path: row.get(13)?,
            file_content_hash: row.get(14)?,
            extractor_complete: row.get::<_, String>(15)? == "complete",
            provider_key: format!(
                "{}@{}",
                row.get::<_, String>(16)?,
                row.get::<_, String>(17)?
            ),
            semantic_digest: semantic_digest(&semantic_identity),
            evidence_digest: semantic_digest(&evidence_identity),
        })
    })? {
        Some(rows) => rows,
        None => return Ok(None),
    };
    let mut map = std::collections::BTreeMap::new();
    for row in rows {
        map.insert(row.canonical_symbol_key.clone(), row);
    }
    Ok(Some(map))
}

fn symbols_for(
    conn: &Connection,
    gen_id: &str,
) -> Result<std::collections::BTreeMap<String, SymbolSnapshot>> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(symbols_for_with_ledger(conn, gen_id, &mut ledger)?)
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct RelationshipState {
    target: String,
    attributes_json: String,
    evidence_method: String,
    confidence_bits: u64,
    evidence_reason: String,
    resolution_status: Option<String>,
    resolved_target: Option<String>,
    resolver_policy_version: Option<String>,
    resolution_reason: Option<String>,
    candidates_json: Option<String>,
}

type RelationshipMap = std::collections::BTreeMap<String, Vec<RelationshipState>>;

fn relationship_identity(source_kind: &str, source: &str, relationship_type: &str) -> String {
    format!("{source_kind}:{source}\u{1f}{relationship_type}")
}

fn relationships_for_with_ledger(
    conn: &Connection,
    gen_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<RelationshipMap>> {
    let mut stmt = conn.prepare(
        "SELECT rf.source_ref_kind, rf.source_ref_value, rf.relationship_type,
                rf.target_ref_kind, rf.target_ref_value, rf.attributes_json,
                rf.evidence_method, rf.confidence, rf.evidence_reason,
                rr.status, rr.resolved_ref_kind, rr.resolved_ref_value,
                rr.resolver_policy_version, rr.reason_code, rr.candidate_refs_json
         FROM generation_file gf
         JOIN relationship_fact rf ON rf.revision_id = gf.revision_id
         LEFT JOIN relationship_resolution rr
           ON rr.generation_id = gf.generation_id
          AND rr.relationship_fact_id = rf.relationship_fact_id
         WHERE gf.generation_id = ?1 AND gf.presence_state = 'present'
         ORDER BY rf.source_ref_kind, rf.source_ref_value, rf.relationship_type,
                  rf.target_ref_kind, rf.target_ref_value, rr.resolver_policy_version",
    )?;
    let mut rows = stmt.query(params![gen_id])?;
    let rows = match collect_canonical_rows(&mut rows, ledger, |row| {
        let target_kind: String = row.get(3)?;
        let target_value: String = row.get(4)?;
        let resolved_kind: Option<String> = row.get(10)?;
        let resolved_value: Option<String> = row.get(11)?;
        Ok((
            relationship_identity(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
            ),
            RelationshipState {
                target: format!("{target_kind}:{target_value}"),
                attributes_json: row.get(5)?,
                evidence_method: row.get(6)?,
                confidence_bits: row.get::<_, f64>(7)?.to_bits(),
                evidence_reason: row.get(8)?,
                resolution_status: row.get(9)?,
                resolved_target: resolved_kind
                    .zip(resolved_value)
                    .map(|(kind, value)| format!("{kind}:{value}")),
                resolver_policy_version: row.get(12)?,
                resolution_reason: row.get(13)?,
                candidates_json: row.get(14)?,
            },
        ))
    })? {
        Some(rows) => rows,
        None => return Ok(None),
    };
    let mut relationships = RelationshipMap::new();
    for (identity, state) in rows {
        relationships.entry(identity).or_default().push(state);
    }
    for states in relationships.values_mut() {
        states.sort();
    }
    Ok(Some(relationships))
}

fn relationships_for(conn: &Connection, gen_id: &str) -> Result<RelationshipMap> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(relationships_for_with_ledger(conn, gen_id, &mut ledger)?)
}

type RelationshipResolutionIdentity<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

impl RelationshipState {
    fn effective_target(&self) -> &str {
        self.resolved_target
            .as_deref()
            .unwrap_or(self.target.as_str())
    }

    fn resolution_identity(&self) -> RelationshipResolutionIdentity<'_> {
        (
            self.resolution_status.as_deref(),
            self.resolved_target.as_deref(),
            self.resolver_policy_version.as_deref(),
            self.resolution_reason.as_deref(),
            self.candidates_json.as_deref(),
        )
    }
}

fn relationship_entity_id(identity: &str, target: &str) -> String {
    format!("{identity}\u{1f}{target}")
}
fn outbound_relationship_targets<'a>(
    relationships: &'a RelationshipMap,
    source_prefix: &str,
) -> std::collections::BTreeMap<&'a str, std::collections::BTreeSet<&'a str>> {
    relationships
        .iter()
        .filter(|(identity, _)| identity.starts_with(source_prefix))
        .map(|(identity, states)| {
            (
                identity.as_str(),
                states
                    .iter()
                    .map(RelationshipState::effective_target)
                    .collect(),
            )
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct EffectState {
    phase: String,
    attributes_json: String,
    evidence_method: String,
    confidence_bits: u64,
    evidence_reason: String,
}

type EffectMap = std::collections::BTreeMap<String, Vec<EffectState>>;

fn effect_identity(subject_kind: &str, subject: &str, effect_type: &str) -> String {
    format!("{subject_kind}:{subject}\u{1f}{effect_type}")
}

fn effects_for_with_ledger(
    conn: &Connection,
    gen_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<EffectMap>> {
    let mut stmt = conn.prepare(
        "SELECT ef.subject_ref_kind, ef.subject_ref_value, ef.effect_type,
                ef.phase, ef.attributes_json, ef.evidence_method,
                ef.confidence, ef.evidence_reason
         FROM generation_file gf
         JOIN effect_fact ef ON ef.revision_id = gf.revision_id
         WHERE gf.generation_id = ?1 AND gf.presence_state = 'present'
         ORDER BY ef.subject_ref_kind, ef.subject_ref_value, ef.effect_type, ef.effect_fact_id",
    )?;
    let mut rows = stmt.query(params![gen_id])?;
    let rows = match collect_canonical_rows(&mut rows, ledger, |row| {
        Ok((
            effect_identity(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
            ),
            EffectState {
                phase: row.get(3)?,
                attributes_json: row.get(4)?,
                evidence_method: row.get(5)?,
                confidence_bits: row.get::<_, f64>(6)?.to_bits(),
                evidence_reason: row.get(7)?,
            },
        ))
    })? {
        Some(rows) => rows,
        None => return Ok(None),
    };
    let mut effects = EffectMap::new();
    for (identity, state) in rows {
        effects.entry(identity).or_default().push(state);
    }
    for states in effects.values_mut() {
        states.sort();
    }
    Ok(Some(effects))
}

fn effects_for(conn: &Connection, gen_id: &str) -> Result<EffectMap> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(effects_for_with_ledger(conn, gen_id, &mut ledger)?)
}

/// Diff two committed generations of the same workspace into a
/// `GenerationDelta`. Both generations must exist and belong to `ws`;
/// diffing a generation against itself is rejected by
/// `GenerationDelta::seal`.
pub fn compute_generation_delta(
    conn: &Connection,
    ws: &WorkspaceRecord,
    from_generation_id: &str,
    to_generation_id: &str,
) -> Result<GenerationDelta> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    match compute_generation_delta_with_ledger(
        conn,
        ws,
        from_generation_id,
        to_generation_id,
        &mut ledger,
    )? {
        Some(delta) => Ok(delta),
        None => Err(AtlasError::Other(
            "unbounded canonical Generation Delta derivation exhausted".into(),
        )),
    }
}

/// Canonical Generation Delta derivation with caller-owned semantic-row work.
///
/// Any exhausted read discards every partially collected map. Only a complete
/// derivation can reach the unchanged/add/remove comparison and sealing path.
pub(crate) fn compute_generation_delta_with_ledger(
    conn: &Connection,
    ws: &WorkspaceRecord,
    from_generation_id: &str,
    to_generation_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<GenerationDelta>> {
    for gen_id in [from_generation_id, to_generation_id] {
        if !ledger.try_charge_history_input_row()? {
            return Ok(None);
        }
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id = ?1 AND workspace_id = ?2",
                params![gen_id, ws.workspace_id],
                |r| r.get(0),
            )
            .optional()?;
        match state.as_deref() {
            Some("committed") => {}
            Some(other) => {
                return Err(AtlasError::Other(format!(
                    "generation {gen_id} is {other}, not committed"
                )));
            }
            None => {
                return Err(AtlasError::Other(format!(
                    "generation {gen_id} not found for workspace {}",
                    ws.workspace_id
                )));
            }
        }
    }

    // Files
    let Some(from_files) = files_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_files) = files_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let from_by_id: std::collections::BTreeMap<&str, &FileRow> =
        from_files.iter().map(|f| (f.file_id.as_str(), f)).collect();
    let to_by_id: std::collections::BTreeMap<&str, &FileRow> =
        to_files.iter().map(|f| (f.file_id.as_str(), f)).collect();

    let mut file_changes = Vec::new();
    for (id, f) in &to_by_id {
        match from_by_id.get(id) {
            None => file_changes.push(DeltaChange {
                entity_id: f.canonical_path.clone(),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(f.file_id.clone()),
                reason_codes: vec!["new_file_identity".to_string()],
            }),
            Some(old) => {
                if old.canonical_path != f.canonical_path {
                    file_changes.push(DeltaChange {
                        entity_id: f.canonical_path.clone(),
                        change_kind: ChangeKind::Renamed,
                        evidence_state: DeltaEvidenceState::Verified,
                        before_id: Some(old.canonical_path.clone()),
                        after_id: Some(f.canonical_path.clone()),
                        reason_codes: vec!["file_identity_continuity_different_path".to_string()],
                    });
                } else if old.content_hash != f.content_hash {
                    file_changes.push(DeltaChange {
                        entity_id: f.canonical_path.clone(),
                        change_kind: ChangeKind::Modified,
                        evidence_state: DeltaEvidenceState::Verified,
                        before_id: Some(old.content_hash.clone()),
                        after_id: Some(f.content_hash.clone()),
                        reason_codes: vec!["content_hash_changed".to_string()],
                    });
                }
            }
        }
    }
    for (id, f) in &from_by_id {
        if !to_by_id.contains_key(id) {
            file_changes.push(DeltaChange {
                entity_id: f.canonical_path.clone(),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(f.file_id.clone()),
                after_id: None,
                reason_codes: vec!["file_identity_no_longer_present".to_string()],
            });
        }
    }
    file_changes.sort_by(|a, b| {
        a.entity_id
            .cmp(&b.entity_id)
            .then(change_kind_str(a.change_kind).cmp(change_kind_str(b.change_kind)))
    });

    let Some(from_symbols) = symbols_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_symbols) = symbols_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(from_rels) = relationships_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_rels) = relationships_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(from_effects) = effects_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_effects) = effects_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(from_coverage) = coverage_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_coverage) = coverage_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(from_conflicts) = conflicts_for_with_ledger(conn, from_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(to_conflicts) = conflicts_for_with_ledger(conn, to_generation_id, ledger)? else {
        return Ok(None);
    };
    let Some(from_provider_fingerprint) =
        generation_provider_fingerprint_with_ledger(conn, from_generation_id, ledger)?
    else {
        return Ok(None);
    };
    let Some(to_provider_fingerprint) =
        generation_provider_fingerprint_with_ledger(conn, to_generation_id, ledger)?
    else {
        return Ok(None);
    };
    let provider_fingerprint_unchanged = from_provider_fingerprint == to_provider_fingerprint;

    let mut symbol_changes = Vec::new();
    let mut unchanged_verified_contracts = Vec::new();
    let mut unchanged_omissions = std::collections::BTreeSet::new();
    for (key, to_sym) in &to_symbols {
        match from_symbols.get(key) {
            None => symbol_changes.push(DeltaChange {
                entity_id: key.clone(),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(to_sym.symbol_fact_id.clone()),
                reason_codes: vec!["new_symbol_fact".to_string()],
            }),
            Some(from_sym) => {
                let outbound_prefix = format!("symbol:{key}\u{1f}");
                let outbound_from = outbound_relationship_targets(&from_rels, &outbound_prefix);
                let outbound_to = outbound_relationship_targets(&to_rels, &outbound_prefix);
                let changed = if from_sym.canonical_path != to_sym.canonical_path {
                    Some((
                        ChangeKind::Renamed,
                        "symbol_source_path_changed",
                        from_sym.canonical_path.clone(),
                        to_sym.canonical_path.clone(),
                    ))
                } else if from_sym.file_id != to_sym.file_id {
                    Some((
                        ChangeKind::StateChanged,
                        "symbol_source_identity_changed",
                        from_sym.file_id.clone(),
                        to_sym.file_id.clone(),
                    ))
                } else if from_sym.semantic_digest != to_sym.semantic_digest {
                    Some((
                        ChangeKind::Modified,
                        "symbol_semantic_identity_changed",
                        from_sym.semantic_digest.clone(),
                        to_sym.semantic_digest.clone(),
                    ))
                } else if from_sym.evidence_digest != to_sym.evidence_digest {
                    Some((
                        ChangeKind::StateChanged,
                        "symbol_evidence_state_changed",
                        from_sym.evidence_digest.clone(),
                        to_sym.evidence_digest.clone(),
                    ))
                } else if outbound_from != outbound_to {
                    Some((
                        ChangeKind::StateChanged,
                        "relationship_set_changed",
                        semantic_digest(&outbound_from),
                        semantic_digest(&outbound_to),
                    ))
                } else {
                    None
                };
                if let Some((change_kind, reason_code, before_id, after_id)) = changed {
                    symbol_changes.push(DeltaChange {
                        entity_id: key.clone(),
                        change_kind,
                        evidence_state: DeltaEvidenceState::Verified,
                        before_id: Some(before_id),
                        after_id: Some(after_id),
                        reason_codes: vec![reason_code.to_string()],
                    });
                    continue;
                }

                let from_coverage = symbol_has_complete_coverage(&from_coverage, from_sym);
                let to_coverage = symbol_has_complete_coverage(&to_coverage, to_sym);
                let live_source_verified = live_source_digest(ws, &to_sym.canonical_path)
                    .as_deref()
                    == Some(to_sym.file_content_hash.as_str());
                if provider_fingerprint_unchanged
                    && from_coverage
                    && to_coverage
                    && live_source_verified
                {
                    unchanged_verified_contracts.push(UnchangedContract {
                        entity_id: key.clone(),
                        claim_scope:
                            "semantic identity, source identity and path, provider fingerprint, and resolved outbound relationship set"
                                .to_string(),
                        evidence: vec![
                            "semantic_identity_unchanged".to_string(),
                            "source_path_unchanged".to_string(),
                            "source_identity_unchanged".to_string(),
                            "live_to_source_hash_verified".to_string(),
                            "provider_fingerprint_unchanged".to_string(),
                            "relationship_set_unchanged".to_string(),
                            "coverage_complete".to_string(),
                        ],
                    });
                } else {
                    if !provider_fingerprint_unchanged {
                        unchanged_omissions.insert("provider fingerprint changed".to_string());
                    }
                    if !from_coverage || !to_coverage {
                        unchanged_omissions.insert(
                            "complete symbol-scoped coverage evidence is unavailable".to_string(),
                        );
                    }
                    if !live_source_verified {
                        unchanged_omissions.insert(
                            "live source no longer matches the captured generation".to_string(),
                        );
                    }
                }
            }
        }
    }
    for (key, from_sym) in &from_symbols {
        if !to_symbols.contains_key(key) {
            symbol_changes.push(DeltaChange {
                entity_id: key.clone(),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(from_sym.symbol_fact_id.clone()),
                after_id: None,
                reason_codes: vec!["symbol_no_longer_present".to_string()],
            });
        }
    }
    symbol_changes.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
    unchanged_verified_contracts.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));

    let relationship_changes = diff_relationships(&from_rels, &to_rels);
    let effect_changes = diff_effects(&from_effects, &to_effects);
    let mut coverage_changes = diff_coverage(&from_coverage, &to_coverage);
    if !provider_fingerprint_unchanged {
        coverage_changes.push(DeltaChange {
            entity_id: "provider_set".to_string(),
            change_kind: ChangeKind::StateChanged,
            evidence_state: DeltaEvidenceState::Verified,
            before_id: Some(from_provider_fingerprint),
            after_id: Some(to_provider_fingerprint),
            reason_codes: vec!["provider_fingerprint_changed".to_string()],
        });
        coverage_changes.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
    }
    let conflict_changes = diff_conflicts(&from_conflicts, &to_conflicts);
    let uncertainty = unchanged_omissions.into_iter().collect();

    let delta_id = deterministic_id(
        "delta",
        &[
            &ws.workspace_id,
            from_generation_id,
            to_generation_id,
            DELTA_POLICY_VERSION,
        ],
    );
    GenerationDelta::seal(
        delta_id,
        ws.workspace_id.clone(),
        from_generation_id.to_string(),
        to_generation_id.to_string(),
        DELTA_POLICY_VERSION.to_string(),
        DeltaChangeSet {
            changes: file_changes,
            omitted_count: 0,
        },
        DeltaChangeSet {
            changes: symbol_changes,
            omitted_count: 0,
        },
        DeltaChangeSet {
            changes: relationship_changes,
            omitted_count: 0,
        },
        DeltaChangeSet {
            changes: effect_changes,
            omitted_count: 0,
        },
        DeltaChangeSet {
            changes: coverage_changes,
            omitted_count: 0,
        },
        DeltaChangeSet {
            changes: conflict_changes,
            omitted_count: 0,
        },
        unchanged_verified_contracts,
        uncertainty,
    )
    .map(Some)
}

fn generation_provider_fingerprint_with_ledger(
    conn: &Connection,
    generation_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<String>> {
    if !ledger.try_charge_history_input_row()? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT provider_set_hash FROM index_generation WHERE generation_id = ?1",
        params![generation_id],
        |row| row.get(0),
    )
    .map(Some)
    .map_err(Into::into)
}

fn generation_provider_fingerprint(conn: &Connection, generation_id: &str) -> Result<String> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(generation_provider_fingerprint_with_ledger(
        conn,
        generation_id,
        &mut ledger,
    )?)
}

fn symbol_has_complete_coverage(coverage: &CoverageMap, symbol: &SymbolSnapshot) -> bool {
    let workspace_key = "workspace:all";
    let file_path_key = format!("file:{}", symbol.canonical_path);
    let provider_key = format!("provider:{}", symbol.provider_key);
    let capability_keys = [
        "capability:symbols",
        "capability:relationships",
        "capability:resolution",
    ];
    let mut witness_count = 0_usize;
    let mut incomplete = false;
    for (identity, states) in coverage {
        if identity == workspace_key
            || identity == &file_path_key
            || identity == &provider_key
            || capability_keys.contains(&identity.as_str())
        {
            witness_count += states.len();
            incomplete |= states.iter().any(|state| state.status != "complete");
            continue;
        }
        if identity.starts_with("file:") {
            for state in states
                .iter()
                .filter(|state| state.file_id.as_deref() == Some(symbol.file_id.as_str()))
            {
                witness_count += 1;
                incomplete |= state.status != "complete";
            }
        }
    }
    symbol.extractor_complete && witness_count > 0 && !incomplete
}
fn diff_relationships(from: &RelationshipMap, to: &RelationshipMap) -> Vec<DeltaChange> {
    let identities: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    let mut changes = Vec::new();
    for identity in identities {
        let before = from.get(identity).map(Vec::as_slice).unwrap_or_default();
        let after = to.get(identity).map(Vec::as_slice).unwrap_or_default();
        let before_by_target = before.iter().fold(
            std::collections::BTreeMap::<&str, Vec<&RelationshipState>>::new(),
            |mut grouped, state| {
                grouped
                    .entry(state.effective_target())
                    .or_default()
                    .push(state);
                grouped
            },
        );
        let after_by_target = after.iter().fold(
            std::collections::BTreeMap::<&str, Vec<&RelationshipState>>::new(),
            |mut grouped, state| {
                grouped
                    .entry(state.effective_target())
                    .or_default()
                    .push(state);
                grouped
            },
        );

        let mut removed = Vec::new();
        let mut added = Vec::new();
        let targets: std::collections::BTreeSet<_> = before_by_target
            .keys()
            .chain(after_by_target.keys())
            .collect();
        for target in targets {
            let before_states = before_by_target
                .get(target)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let after_states = after_by_target
                .get(target)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut unmatched_before = Vec::new();
            let mut unmatched_after = Vec::new();
            let mut before_index = 0;
            let mut after_index = 0;
            while before_index < before_states.len() && after_index < after_states.len() {
                match before_states[before_index].cmp(after_states[after_index]) {
                    std::cmp::Ordering::Less => {
                        unmatched_before.push(before_states[before_index]);
                        before_index += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        unmatched_after.push(after_states[after_index]);
                        after_index += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        before_index += 1;
                        after_index += 1;
                    }
                }
            }
            unmatched_before.extend_from_slice(&before_states[before_index..]);
            unmatched_after.extend_from_slice(&after_states[after_index..]);
            let paired = unmatched_before.len().min(unmatched_after.len());
            for index in 0..paired {
                let before_state = unmatched_before[index];
                let after_state = unmatched_after[index];
                let (change_kind, reason_code) =
                    if before_state.resolution_identity() != after_state.resolution_identity() {
                        (
                            ChangeKind::StateChanged,
                            "relationship_resolution_state_changed",
                        )
                    } else if before_state.target != after_state.target {
                        (ChangeKind::Modified, "relationship_semantic_state_changed")
                    } else {
                        (ChangeKind::Modified, "relationship_evidence_changed")
                    };
                changes.push(DeltaChange {
                    entity_id: relationship_entity_id(identity, target),
                    change_kind,
                    evidence_state: DeltaEvidenceState::Verified,
                    before_id: Some(semantic_digest(before_state)),
                    after_id: Some(semantic_digest(after_state)),
                    reason_codes: vec![reason_code.to_string()],
                });
            }
            removed.extend(unmatched_before[paired..].iter().copied());
            added.extend(unmatched_after[paired..].iter().copied());
        }

        if removed.len() == 1 && added.len() == 1 {
            changes.push(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Retargeted,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(semantic_digest(removed[0])),
                after_id: Some(semantic_digest(added[0])),
                reason_codes: vec!["relationship_target_changed".to_string()],
            });
        } else {
            changes.extend(removed.into_iter().map(|state| DeltaChange {
                entity_id: relationship_entity_id(identity, state.effective_target()),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(semantic_digest(state)),
                after_id: None,
                reason_codes: vec!["edge_no_longer_present".to_string()],
            }));
            changes.extend(added.into_iter().map(|state| DeltaChange {
                entity_id: relationship_entity_id(identity, state.effective_target()),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(semantic_digest(state)),
                reason_codes: vec!["resolved_edge_added".to_string()],
            }));
        }
    }
    changes.sort_by(|left, right| {
        left.entity_id
            .cmp(&right.entity_id)
            .then(change_kind_str(left.change_kind).cmp(change_kind_str(right.change_kind)))
            .then(left.before_id.cmp(&right.before_id))
            .then(left.after_id.cmp(&right.after_id))
    });
    changes
}

fn diff_effects(from: &EffectMap, to: &EffectMap) -> Vec<DeltaChange> {
    let identities: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    identities
        .into_iter()
        .filter_map(|identity| match (from.get(identity), to.get(identity)) {
            (None, Some(after)) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(semantic_digest(after)),
                reason_codes: vec!["effect_added".to_string()],
            }),
            (Some(before), None) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(semantic_digest(before)),
                after_id: None,
                reason_codes: vec!["effect_no_longer_present".to_string()],
            }),
            (Some(before), Some(after)) if before != after => {
                let before_phases: std::collections::BTreeSet<_> =
                    before.iter().map(|state| state.phase.as_str()).collect();
                let after_phases: std::collections::BTreeSet<_> =
                    after.iter().map(|state| state.phase.as_str()).collect();
                let (change_kind, reason_code) = if before_phases != after_phases {
                    (ChangeKind::StateChanged, "effect_phase_changed")
                } else {
                    (ChangeKind::Modified, "effect_semantic_state_changed")
                };
                Some(DeltaChange {
                    entity_id: identity.clone(),
                    change_kind,
                    evidence_state: DeltaEvidenceState::Verified,
                    before_id: Some(semantic_digest(before)),
                    after_id: Some(semantic_digest(after)),
                    reason_codes: vec![reason_code.to_string()],
                })
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct CoverageStateSnapshot {
    status: String,
    file_id: Option<String>,
    capabilities_json: String,
    limitations_json: String,
    details_json: String,
}

type CoverageMap = std::collections::BTreeMap<String, Vec<CoverageStateSnapshot>>;

fn coverage_for_with_ledger(
    conn: &Connection,
    generation_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<CoverageMap>> {
    let mut statement = conn.prepare(
        "SELECT scope_kind || ':' || scope_key, status, file_id,
                capabilities_json, limitations_json, details_json
         FROM coverage_record WHERE generation_id = ?1
         ORDER BY scope_kind, scope_key, coverage_id",
    )?;
    let mut rows = statement.query(params![generation_id])?;
    let rows = match collect_canonical_rows(&mut rows, ledger, |row| {
        Ok((
            row.get::<_, String>(0)?,
            CoverageStateSnapshot {
                status: row.get(1)?,
                file_id: row.get(2)?,
                capabilities_json: row.get(3)?,
                limitations_json: row.get(4)?,
                details_json: row.get(5)?,
            },
        ))
    })? {
        Some(rows) => rows,
        None => return Ok(None),
    };
    let mut coverage = CoverageMap::new();
    for (identity, state) in rows {
        coverage.entry(identity).or_default().push(state);
    }
    for states in coverage.values_mut() {
        states.sort();
    }
    Ok(Some(coverage))
}

fn coverage_for(conn: &Connection, generation_id: &str) -> Result<CoverageMap> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(coverage_for_with_ledger(conn, generation_id, &mut ledger)?)
}

fn diff_coverage(from: &CoverageMap, to: &CoverageMap) -> Vec<DeltaChange> {
    let identities: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    identities
        .into_iter()
        .filter_map(|identity| match (from.get(identity), to.get(identity)) {
            (None, Some(after)) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(semantic_digest(after)),
                reason_codes: vec!["new_coverage_scope".to_string()],
            }),
            (Some(before), None) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(semantic_digest(before)),
                after_id: None,
                reason_codes: vec!["coverage_scope_no_longer_reported".to_string()],
            }),
            (Some(before), Some(after)) if before != after => {
                let before_statuses: std::collections::BTreeSet<_> =
                    before.iter().map(|state| state.status.as_str()).collect();
                let after_statuses: std::collections::BTreeSet<_> =
                    after.iter().map(|state| state.status.as_str()).collect();
                let (change_kind, reason_code) = if before_statuses != after_statuses {
                    (ChangeKind::StateChanged, "coverage_status_changed")
                } else {
                    (ChangeKind::Modified, "coverage_evidence_changed")
                };
                Some(DeltaChange {
                    entity_id: identity.clone(),
                    change_kind,
                    evidence_state: DeltaEvidenceState::Verified,
                    before_id: Some(semantic_digest(before)),
                    after_id: Some(semantic_digest(after)),
                    reason_codes: vec![reason_code.to_string()],
                })
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct ConflictStateSnapshot {
    status: String,
    participating_fact_ids_json: String,
    preferred_fact_id: Option<String>,
    projection_policy_version: String,
}

type ConflictMap = std::collections::BTreeMap<String, Vec<ConflictStateSnapshot>>;

fn conflict_identity(subject_kind: &str, subject: &str, conflict_type: &str) -> String {
    format!("{subject_kind}:{subject}\u{1f}{conflict_type}")
}

fn conflicts_for_with_ledger(
    conn: &Connection,
    generation_id: &str,
    ledger: &mut impl CanonicalHistoryLedger,
) -> Result<Option<ConflictMap>> {
    let mut statement = conn.prepare(
        "SELECT subject_kind, subject_key, conflict_type, status,
                participating_fact_ids_json, preferred_fact_id, projection_policy_version
         FROM evidence_conflict WHERE generation_id = ?1
         ORDER BY subject_kind, subject_key, conflict_type, evidence_conflict_id",
    )?;
    let mut rows = statement.query(params![generation_id])?;
    let rows = match collect_canonical_rows(&mut rows, ledger, |row| {
        Ok((
            conflict_identity(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
            ),
            ConflictStateSnapshot {
                status: row.get(3)?,
                participating_fact_ids_json: row.get(4)?,
                preferred_fact_id: row.get(5)?,
                projection_policy_version: row.get(6)?,
            },
        ))
    })? {
        Some(rows) => rows,
        None => return Ok(None),
    };
    let mut conflicts = ConflictMap::new();
    for (identity, state) in rows {
        conflicts.entry(identity).or_default().push(state);
    }
    for states in conflicts.values_mut() {
        states.sort();
    }
    Ok(Some(conflicts))
}

fn conflicts_for(conn: &Connection, generation_id: &str) -> Result<ConflictMap> {
    let mut ledger = UnboundedCanonicalHistoryLedger;
    require_unbounded(conflicts_for_with_ledger(conn, generation_id, &mut ledger)?)
}

fn diff_conflicts(from: &ConflictMap, to: &ConflictMap) -> Vec<DeltaChange> {
    let identities: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    identities
        .into_iter()
        .filter_map(|identity| match (from.get(identity), to.get(identity)) {
            (None, Some(after)) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Added,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: None,
                after_id: Some(semantic_digest(after)),
                reason_codes: vec!["conflict_added".to_string()],
            }),
            (Some(before), None) => Some(DeltaChange {
                entity_id: identity.clone(),
                change_kind: ChangeKind::Removed,
                evidence_state: DeltaEvidenceState::Verified,
                before_id: Some(semantic_digest(before)),
                after_id: None,
                reason_codes: vec!["conflict_no_longer_reported".to_string()],
            }),
            (Some(before), Some(after)) if before != after => {
                let before_statuses: std::collections::BTreeSet<_> =
                    before.iter().map(|state| state.status.as_str()).collect();
                let after_statuses: std::collections::BTreeSet<_> =
                    after.iter().map(|state| state.status.as_str()).collect();
                let (change_kind, reason_code) = if before_statuses != after_statuses {
                    (ChangeKind::StateChanged, "conflict_status_changed")
                } else {
                    (ChangeKind::Modified, "conflict_evidence_changed")
                };
                Some(DeltaChange {
                    entity_id: identity.clone(),
                    change_kind,
                    evidence_state: DeltaEvidenceState::Verified,
                    before_id: Some(semantic_digest(before)),
                    after_id: Some(semantic_digest(after)),
                    reason_codes: vec![reason_code.to_string()],
                })
            }
            _ => None,
        })
        .collect()
}

/// Persist a computed `GenerationDelta` into `generation_delta` +
/// `generation_delta_item`. Re-persisting an identical delta is a no-op;
/// reusing its deterministic identity for different content fails closed.
pub fn persist_generation_delta(conn: &Connection, delta: &GenerationDelta) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    persist_generation_delta_rows(&tx, delta)?;
    tx.commit()?;
    Ok(())
}

/// Persist delta rows inside a caller-owned transaction.
pub(crate) fn persist_generation_delta_in_transaction(
    conn: &Connection,
    delta: &GenerationDelta,
) -> Result<()> {
    persist_generation_delta_rows(conn, delta)
}
fn validate_generation_delta(delta: &GenerationDelta) -> Result<()> {
    let expected_id = deterministic_id(
        "delta",
        &[
            &delta.workspace_id,
            &delta.from_generation_id,
            &delta.to_generation_id,
            &delta.delta_policy_version,
        ],
    );
    if delta.schema_version != crate::context_ir::CONTEXT_SCHEMA_VERSION
        || delta.delta_id != expected_id
    {
        return Err(AtlasError::InvalidConfig(
            "generation_delta identity/version does not match its persisted contract".to_string(),
        ));
    }
    let resealed = GenerationDelta::seal(
        delta.delta_id.clone(),
        delta.workspace_id.clone(),
        delta.from_generation_id.clone(),
        delta.to_generation_id.clone(),
        delta.delta_policy_version.clone(),
        delta.files.clone(),
        delta.symbols.clone(),
        delta.relationships.clone(),
        delta.effects.clone(),
        delta.coverage.clone(),
        delta.conflicts.clone(),
        delta.unchanged_verified_contracts.clone(),
        delta.uncertainty.clone(),
    )?;
    if resealed.delta_hash != delta.delta_hash {
        return Err(AtlasError::InvalidConfig(
            "generation_delta delta_hash does not match its canonical content".to_string(),
        ));
    }
    Ok(())
}

fn persist_generation_delta_rows(conn: &Connection, delta: &GenerationDelta) -> Result<()> {
    validate_generation_delta(delta)?;
    let existing_hash: Option<String> = conn
        .query_row(
            "SELECT delta_hash FROM generation_delta WHERE delta_id = ?1",
            params![delta.delta_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(existing_hash) = existing_hash {
        if existing_hash == delta.delta_hash {
            return Ok(());
        }
        return Err(AtlasError::InvalidConfig(format!(
            "generation_delta {} conflicts with persisted delta content",
            delta.delta_id
        )));
    }

    let now = crate::migrations::iso8601_now();
    conn.execute(
        "INSERT INTO generation_delta (
            delta_id, workspace_id, from_generation_id, to_generation_id, delta_policy_version,
            state, canonical_json, delta_hash, created_at, finished_at
        ) VALUES (?1,?2,?3,?4,?5,'ready',?6,?7,?8,?8)",
        params![
            delta.delta_id,
            delta.workspace_id,
            delta.from_generation_id,
            delta.to_generation_id,
            delta.delta_policy_version,
            serde_json::to_string(delta)?,
            delta.delta_hash,
            now,
        ],
    )?;

    let mut ordinal = 0i64;
    for (category, set) in [
        ("file", &delta.files),
        ("symbol", &delta.symbols),
        ("relationship", &delta.relationships),
        ("effect", &delta.effects),
        ("coverage", &delta.coverage),
        ("conflict", &delta.conflicts),
    ] {
        for change in &set.changes {
            conn.execute(
                "INSERT INTO generation_delta_item (
                    delta_id, ordinal, category, entity_id, change_kind, evidence_state,
                    before_id, after_id, reason_codes_json, item_json
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    delta.delta_id,
                    ordinal,
                    category,
                    change.entity_id,
                    change_kind_str(change.change_kind),
                    evidence_state_str(change.evidence_state),
                    change.before_id,
                    change.after_id,
                    serde_json::to_string(&change.reason_codes)?,
                    serde_json::to_string(change)?,
                ],
            )?;
            ordinal += 1;
        }
    }
    for u in &delta.unchanged_verified_contracts {
        conn.execute(
            "INSERT INTO generation_delta_item (
                delta_id, ordinal, category, entity_id, change_kind, evidence_state,
                before_id, after_id, reason_codes_json, item_json
            ) VALUES (?1,?2,'unchanged_contract',?3,'unchanged','verified',NULL,NULL,?4,?5)",
            params![
                delta.delta_id,
                ordinal,
                u.entity_id,
                serde_json::to_string(&u.evidence)?,
                serde_json::to_string(u)?
            ],
        )?;
        ordinal += 1;
    }
    for note in &delta.uncertainty {
        conn.execute(
            "INSERT INTO generation_delta_item (
                delta_id, ordinal, category, entity_id, change_kind, evidence_state,
                before_id, after_id, reason_codes_json, item_json
            ) VALUES (?1,?2,'uncertainty','delta_wide','uncertain','uncertain',NULL,NULL,'[]',?3)",
            params![delta.delta_id, ordinal, serde_json::to_string(note)?],
        )?;
        ordinal += 1;
    }

    Ok(())
}

/// Build one Atlas-observed event for every exact file artifact changed by
/// the delta. These events link facts only: they deliberately contain no
/// causality or reasoning-use claim.
pub(crate) fn artifact_change_events(
    task_session_id: &str,
    delta: &GenerationDelta,
    occurred_at: &str,
) -> Vec<ContextUseEvent> {
    delta
        .files
        .changes
        .iter()
        .map(|change| {
            let discriminator = format!(
                "{}:{}:{}",
                delta.delta_id,
                change_kind_str(change.change_kind),
                change.entity_id
            );
            ContextUseEvent {
                schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
                event_id: crate::task_session::event_id_for(
                    task_session_id,
                    ContextUseEventType::ArtifactChanged,
                    &discriminator,
                ),
                task_session_id: task_session_id.to_string(),
                context_id: None,
                item_id: Some(delta.delta_id.clone()),
                entity_id: Some(change.entity_id.clone()),
                event_type: ContextUseEventType::ArtifactChanged,
                observation_source: ObservationSource::AtlasObserved,
                occurred_at: occurred_at.to_string(),
                bytes: None,
                details: serde_json::json!({
                    "delta_id": delta.delta_id,
                    "from_generation_id": delta.from_generation_id,
                    "to_generation_id": delta.to_generation_id,
                    "change_kind": change_kind_str(change.change_kind),
                    "evidence_state": evidence_state_str(change.evidence_state),
                    "before_id": change.before_id,
                    "after_id": change.after_id,
                    "reason_codes": change.reason_codes,
                }),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Evidence leases
// ---------------------------------------------------------------------------

/// Create a `valid` evidence lease for one piece of evidence
/// (`evidence_kind`/`evidence_id`, e.g. `"symbol"`/canonical_symbol_key),
/// recording the exact `source_revision_hash` it was valid against.
#[allow(clippy::too_many_arguments)]
pub fn create_evidence_lease(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    evidence_kind: &str,
    evidence_id: &str,
    source_revision_hash: Option<&str>,
    provider_fingerprint: Option<&str>,
    resolver_policy_version: Option<&str>,
    projection_policy_version: Option<&str>,
) -> Result<String> {
    let lease_id = deterministic_id(
        "lease",
        &[
            &ws.workspace_id,
            generation_id,
            evidence_kind,
            evidence_id,
            source_revision_hash.unwrap_or(""),
            provider_fingerprint.unwrap_or(""),
            resolver_policy_version.unwrap_or(""),
            projection_policy_version.unwrap_or(""),
        ],
    );
    conn.execute(
        "INSERT OR IGNORE INTO evidence_lease (
            lease_id, workspace_id, generation_id, evidence_kind, evidence_id, source_revision_hash,
            provider_fingerprint, resolver_policy_version, projection_policy_version, state, created_at
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'valid',?10)",
        params![
            lease_id, ws.workspace_id, generation_id, evidence_kind, evidence_id, source_revision_hash,
            provider_fingerprint, resolver_policy_version, projection_policy_version, crate::migrations::iso8601_now(),
        ],
    )?;
    Ok(lease_id)
}

/// Mark a lease invalidated (superseded by a new generation, explicit
/// revocation, or a failed re-verification).
pub fn invalidate_evidence_lease(conn: &Connection, lease_id: &str, reason: &str) -> Result<()> {
    let updated = conn.execute(
        "UPDATE evidence_lease SET state = 'invalidated', invalidation_reason = ?2, invalidated_at = ?3
         WHERE lease_id = ?1 AND state = 'valid'",
        params![lease_id, reason, crate::migrations::iso8601_now()],
    )?;
    if updated == 0 {
        return Err(AtlasError::Other(format!(
            "evidence_lease {lease_id} not found or already not valid"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum LeaseVerification {
    Valid,
    StaleAutoInvalidated {
        observed_hash: String,
    },
    DependencyMismatch {
        dependency_kind: String,
        dependency_key: String,
        expected_digest: String,
        observed_digest: Option<String>,
    },
    DependencyMismatchAutoInvalidated {
        dependency_kind: String,
        dependency_key: String,
        expected_digest: String,
        observed_digest: Option<String>,
    },
    AlreadyInvalid,
    NoSourceToVerify,
}

fn lease_dependency_kind_str(kind: IrLeaseDependencyKindV2) -> &'static str {
    kind.as_str()
}

fn live_source_digest(ws: &WorkspaceRecord, canonical_path: &str) -> Option<String> {
    let root = std::path::Path::new(&ws.canonical_root);
    let candidate = root.join(canonical_path);
    let relative = crate::paths::confine_to_root(root, &candidate).ok()?;
    crate::hashing::content_hash_of_file(&root.join(relative)).ok()
}

#[derive(Clone, Copy)]
enum LeaseDependencyKey<'a> {
    Declared {
        kind: IrLeaseDependencyKindV2,
        key: &'a str,
    },
    Evidence {
        kind: &'a str,
        key: &'a str,
    },
    StoredProviderFingerprint,
    StoredResolverPolicy {
        evidence_kind: &'a str,
        evidence_id: &'a str,
    },
    StoredProjectionPolicy {
        evidence_kind: &'a str,
        evidence_id: &'a str,
    },
}

fn current_dependency_digest(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    dependency: LeaseDependencyKey<'_>,
) -> Result<Option<String>> {
    let digest = match dependency {
        LeaseDependencyKey::Declared { kind, key } => match kind {
            IrLeaseDependencyKindV2::SourceDigest => live_source_digest(ws, key),
            IrLeaseDependencyKindV2::Relationship => relationships_for(conn, generation_id)?
                .get(key)
                .map(semantic_digest),
            IrLeaseDependencyKindV2::Effect => effects_for(conn, generation_id)?
                .get(key)
                .map(semantic_digest),
            IrLeaseDependencyKindV2::ProviderFingerprint if key == "provider_set" => {
                let provider_set = generation_provider_fingerprint(conn, generation_id)?;
                Some(crate::hashing::content_hash_of_bytes(
                    provider_set.as_bytes(),
                ))
            }
            IrLeaseDependencyKindV2::ProviderFingerprint => {
                let mut statement = conn.prepare(
                    "SELECT input_fingerprint FROM provider_execution
                     WHERE generation_id = ?1 AND provider_key = ?2
                     ORDER BY scope_kind, scope_key, provider_execution_id",
                )?;
                let rows = statement.query_map(params![generation_id, key], |row| row.get(0))?;
                let fingerprints: Vec<String> = rows.collect::<std::result::Result<_, _>>()?;
                match fingerprints.as_slice() {
                    [] => None,
                    [fingerprint] => Some(fingerprint.clone()),
                    _ => Some(semantic_digest(&fingerprints)),
                }
            }
            IrLeaseDependencyKindV2::Coverage => coverage_for(conn, generation_id)?
                .get(key)
                .map(semantic_digest),
            IrLeaseDependencyKindV2::Conflict => conflicts_for(conn, generation_id)?
                .get(key)
                .map(semantic_digest),
            IrLeaseDependencyKindV2::PlannerPolicy => {
                (key == crate::task_compiler::PLANNER_POLICY_V2_VERSION).then(|| {
                    crate::hashing::content_hash_of_bytes(
                        crate::task_compiler::PLANNER_POLICY_V2_VERSION.as_bytes(),
                    )
                })
            }
            IrLeaseDependencyKindV2::ProjectionPolicy => {
                (key == crate::context_route::DEEP_PROJECTION_VERSION).then(|| {
                    crate::hashing::content_hash_of_bytes(
                        crate::context_route::DEEP_PROJECTION_VERSION.as_bytes(),
                    )
                })
            }
            IrLeaseDependencyKindV2::Estimator => {
                (key == crate::context_route::DEEP_ESTIMATOR_VERSION).then(|| {
                    crate::hashing::content_hash_of_bytes(
                        crate::context_route::DEEP_ESTIMATOR_VERSION.as_bytes(),
                    )
                })
            }
        },
        LeaseDependencyKey::Evidence { kind, key } => {
            let exists = match kind {
                "file" => files_for(conn, generation_id)?
                    .iter()
                    .any(|file| file.file_id == key || file.canonical_path == key),
                "symbol" => {
                    symbols_for(conn, generation_id)?.contains_key(key)
                        || conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM generation_file gf
                                JOIN symbol_fact sf ON sf.revision_id = gf.revision_id
                                WHERE gf.generation_id = ?1
                                  AND gf.presence_state = 'present'
                                  AND sf.symbol_fact_id = ?2
                            )",
                            params![generation_id, key],
                            |row| row.get::<_, bool>(0),
                        )?
                }
                "relationship" => {
                    relationships_for(conn, generation_id)?.contains_key(key)
                        || conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM generation_file gf
                                JOIN relationship_fact rf ON rf.revision_id = gf.revision_id
                                WHERE gf.generation_id = ?1
                                  AND gf.presence_state = 'present'
                                  AND rf.relationship_fact_id = ?2
                            )",
                            params![generation_id, key],
                            |row| row.get::<_, bool>(0),
                        )?
                }
                "effect" => {
                    effects_for(conn, generation_id)?.contains_key(key)
                        || conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM generation_file gf
                                JOIN effect_fact ef ON ef.revision_id = gf.revision_id
                                WHERE gf.generation_id = ?1
                                  AND gf.presence_state = 'present'
                                  AND ef.effect_fact_id = ?2
                            )",
                            params![generation_id, key],
                            |row| row.get::<_, bool>(0),
                        )?
                }
                "coverage" => {
                    coverage_for(conn, generation_id)?.contains_key(key)
                        || conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM coverage_record
                                WHERE generation_id = ?1 AND coverage_id = ?2
                            )",
                            params![generation_id, key],
                            |row| row.get::<_, bool>(0),
                        )?
                }
                "conflict" => {
                    conflicts_for(conn, generation_id)?.contains_key(key)
                        || conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM evidence_conflict
                                WHERE generation_id = ?1 AND evidence_conflict_id = ?2
                            )",
                            params![generation_id, key],
                            |row| row.get::<_, bool>(0),
                        )?
                }
                _ => false,
            };
            exists.then(|| crate::hashing::content_hash_of_bytes(key.as_bytes()))
        }
        LeaseDependencyKey::StoredProviderFingerprint => {
            Some(generation_provider_fingerprint(conn, generation_id)?)
        }
        LeaseDependencyKey::StoredResolverPolicy {
            evidence_kind,
            evidence_id,
        } => {
            if evidence_kind != "relationship" {
                Some(crate::semantic_reconcile::RESOLVER_POLICY_VERSION.to_string())
            } else if let Some(states) = relationships_for(conn, generation_id)?.get(evidence_id) {
                let policies: std::collections::BTreeSet<_> = states
                    .iter()
                    .filter_map(|state| state.resolver_policy_version.as_deref())
                    .collect();
                if policies.len() == 1 {
                    policies.into_iter().next().map(str::to_string)
                } else {
                    None
                }
            } else {
                conn.query_row(
                    "SELECT CASE
                                WHEN COUNT(DISTINCT rr.resolver_policy_version) = 1
                                THEN MIN(rr.resolver_policy_version)
                            END
                     FROM relationship_resolution rr
                     WHERE rr.generation_id = ?1 AND rr.relationship_fact_id = ?2",
                    params![generation_id, evidence_id],
                    |row| row.get::<_, Option<String>>(0),
                )?
            }
        }
        LeaseDependencyKey::StoredProjectionPolicy {
            evidence_kind,
            evidence_id,
        } => {
            if evidence_kind != "conflict" {
                Some(crate::semantic_reconcile::PROJECTION_POLICY_VERSION.to_string())
            } else if let Some(states) = conflicts_for(conn, generation_id)?.get(evidence_id) {
                let policies: std::collections::BTreeSet<_> = states
                    .iter()
                    .map(|state| state.projection_policy_version.as_str())
                    .collect();
                if policies.len() == 1 {
                    policies.into_iter().next().map(str::to_string)
                } else {
                    None
                }
            } else {
                conn.query_row(
                    "SELECT projection_policy_version
                     FROM evidence_conflict
                     WHERE generation_id = ?1 AND evidence_conflict_id = ?2",
                    params![generation_id, evidence_id],
                    |row| row.get(0),
                )
                .optional()?
            }
        }
    };
    Ok(digest)
}

fn dependency_mismatch(
    kind: IrLeaseDependencyKindV2,
    key: &str,
    expected_digest: &str,
    observed_digest: Option<String>,
) -> Option<LeaseVerification> {
    (observed_digest.as_deref() != Some(expected_digest)).then(|| {
        LeaseVerification::DependencyMismatch {
            dependency_kind: lease_dependency_kind_str(kind).to_string(),
            dependency_key: key.to_string(),
            expected_digest: expected_digest.to_string(),
            observed_digest,
        }
    })
}

fn active_generation_for_workspace(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
        params![workspace_id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.flatten())
    .map_err(Into::into)
}

/// Verify every dependency declared by an in-memory V2 lease against the
/// captured Truth generation, current fixed policy identities, and live source.
/// A dependency with no current witness fails closed; no provider or resolver
/// runs on this read path.
fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn verify_declared_lease_dependencies(
    conn: &Connection,
    ws: &WorkspaceRecord,
    lease: &IrEvidenceLeaseV2,
) -> Result<LeaseVerification> {
    let generation_workspace: Option<String> = conn
        .query_row(
            "SELECT workspace_id FROM index_generation
             WHERE generation_id = ?1 AND state = 'committed'",
            params![lease.generation_id],
            |row| row.get(0),
        )
        .optional()?;
    if generation_workspace.as_deref() != Some(ws.workspace_id.as_str())
        || active_generation_for_workspace(conn, &ws.workspace_id)?.as_deref()
            != Some(lease.generation_id.as_str())
    {
        return Ok(LeaseVerification::DependencyMismatch {
            dependency_kind: "generation".to_string(),
            dependency_key: lease.generation_id.clone(),
            expected_digest: lease.generation_id.clone(),
            observed_digest: active_generation_for_workspace(conn, &ws.workspace_id)?,
        });
    }
    if lease.dependencies.is_empty() {
        return Ok(LeaseVerification::NoSourceToVerify);
    }
    let mut previous_key = None;
    for dependency in &lease.dependencies {
        if dependency.key.is_empty() || !is_hex_digest(&dependency.digest) {
            return Err(AtlasError::InvalidConfig(format!(
                "evidence lease dependency {}:{} requires a non-empty key and SHA-256 digest",
                dependency.kind.as_str(),
                dependency.key
            )));
        }
        let dependency_key = (dependency.kind.as_str(), dependency.key.as_str());
        if previous_key.is_some_and(|previous| previous >= dependency_key) {
            return Err(AtlasError::InvalidConfig(format!(
                "evidence lease dependencies are not in canonical order at {}:{}",
                dependency.kind.as_str(),
                dependency.key
            )));
        }
        previous_key = Some(dependency_key);
    }
    for dependency in &lease.dependencies {
        let observed = current_dependency_digest(
            conn,
            ws,
            &lease.generation_id,
            LeaseDependencyKey::Declared {
                kind: dependency.kind,
                key: &dependency.key,
            },
        )?;
        if let Some(mismatch) = dependency_mismatch(
            dependency.kind,
            &dependency.key,
            &dependency.digest,
            observed,
        ) {
            return Ok(mismatch);
        }
    }
    Ok(LeaseVerification::Valid)
}

type StoredLease = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Re-verify every dependency stored by a durable evidence lease. Source bytes
/// are always checked live; provider, resolver, and projection identities are
/// checked against their current fixed or generation-bound Truth witnesses.
pub fn verify_evidence_lease(
    conn: &Connection,
    ws: &WorkspaceRecord,
    lease_id: &str,
    canonical_path: &str,
) -> Result<LeaseVerification> {
    let row: Option<StoredLease> = conn
        .query_row(
            "SELECT state, workspace_id, generation_id, evidence_kind, evidence_id,
                    source_revision_hash, provider_fingerprint,
                    resolver_policy_version, projection_policy_version
             FROM evidence_lease WHERE lease_id = ?1",
            params![lease_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        state,
        workspace_id,
        generation_id,
        evidence_kind,
        evidence_id,
        source_revision_hash,
        provider_fingerprint,
        resolver_policy_version,
        projection_policy_version,
    )) = row
    else {
        return Err(AtlasError::Other(format!(
            "evidence_lease {lease_id} not found"
        )));
    };
    if state != "valid" {
        return Ok(LeaseVerification::AlreadyInvalid);
    }

    let invalidate_dependency =
        |kind: &str, expected: String, observed: Option<String>| -> Result<LeaseVerification> {
            invalidate_evidence_lease(
                conn,
                lease_id,
                &format!("{kind}_mismatch_on_reverification"),
            )?;
            Ok(LeaseVerification::DependencyMismatchAutoInvalidated {
                dependency_kind: kind.to_string(),
                dependency_key: evidence_id.clone(),
                expected_digest: expected,
                observed_digest: observed,
            })
        };

    if workspace_id != ws.workspace_id {
        return invalidate_dependency(
            "workspace_identity",
            workspace_id,
            Some(ws.workspace_id.clone()),
        );
    }
    let active_generation = active_generation_for_workspace(conn, &ws.workspace_id)?;
    if active_generation.as_deref() != Some(generation_id.as_str()) {
        return invalidate_dependency("generation", generation_id, active_generation);
    }
    let expected_evidence = crate::hashing::content_hash_of_bytes(evidence_id.as_bytes());
    let observed_evidence = current_dependency_digest(
        conn,
        ws,
        &generation_id,
        LeaseDependencyKey::Evidence {
            kind: &evidence_kind,
            key: &evidence_id,
        },
    )?;
    if observed_evidence.as_deref() != Some(expected_evidence.as_str()) {
        return invalidate_dependency("evidence", expected_evidence, observed_evidence);
    }
    if let Some(expected_hash) = source_revision_hash {
        let observed_hash = current_dependency_digest(
            conn,
            ws,
            &generation_id,
            LeaseDependencyKey::Declared {
                kind: IrLeaseDependencyKindV2::SourceDigest,
                key: canonical_path,
            },
        )?
        .unwrap_or_default();
        if observed_hash != expected_hash {
            invalidate_evidence_lease(conn, lease_id, "source_hash_mismatch_on_reverification")?;
            return Ok(LeaseVerification::StaleAutoInvalidated { observed_hash });
        }
    } else if provider_fingerprint.is_none()
        && resolver_policy_version.is_none()
        && projection_policy_version.is_none()
    {
        return Ok(LeaseVerification::NoSourceToVerify);
    }
    if let Some(expected) = provider_fingerprint {
        let observed = current_dependency_digest(
            conn,
            ws,
            &generation_id,
            LeaseDependencyKey::StoredProviderFingerprint,
        )?;
        if observed.as_deref() != Some(expected.as_str()) {
            return invalidate_dependency("provider_fingerprint", expected, observed);
        }
    }
    if let Some(expected) = resolver_policy_version {
        let observed = current_dependency_digest(
            conn,
            ws,
            &generation_id,
            LeaseDependencyKey::StoredResolverPolicy {
                evidence_kind: &evidence_kind,
                evidence_id: &evidence_id,
            },
        )?;
        if observed.as_deref() != Some(expected.as_str()) {
            return invalidate_dependency("resolver_policy", expected, observed);
        }
    }
    if let Some(expected) = projection_policy_version {
        let observed = current_dependency_digest(
            conn,
            ws,
            &generation_id,
            LeaseDependencyKey::StoredProjectionPolicy {
                evidence_kind: &evidence_kind,
                evidence_id: &evidence_id,
            },
        )?;
        if observed.as_deref() != Some(expected.as_str()) {
            return invalidate_dependency("projection_policy", expected, observed);
        }
    }
    Ok(LeaseVerification::Valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::discovery;
    use crate::workspace::register_workspace;
    use tempfile::tempdir;

    fn setup() -> (
        tempfile::TempDir,
        rusqlite::Connection,
        WorkspaceRecord,
        tempfile::TempDir,
    ) {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
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
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();
        (db_dir, conn, ws, ws_dir)
    }
    fn first_symbol_key(conn: &Connection, generation_id: &str) -> String {
        conn.query_row(
            "SELECT sf.canonical_symbol_key
             FROM generation_file gf
             JOIN symbol_fact sf ON sf.revision_id = gf.revision_id
             WHERE gf.generation_id = ?1
             ORDER BY sf.canonical_symbol_key
             LIMIT 1",
            params![generation_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[derive(Debug)]
    struct TestHistoryLedger {
        remaining: u64,
        consumed: u64,
    }

    impl TestHistoryLedger {
        fn new(limit: u64) -> Self {
            Self {
                remaining: limit,
                consumed: 0,
            }
        }
    }

    impl CanonicalHistoryLedger for TestHistoryLedger {
        fn try_charge_history_input_row(&mut self) -> Result<bool> {
            if self.remaining == 0 {
                return Ok(false);
            }
            self.remaining -= 1;
            self.consumed += 1;
            Ok(true)
        }
    }

    #[test]
    fn canonical_row_collection_charges_the_empty_sentinel_before_completion() {
        let connection = Connection::open_in_memory().unwrap();
        let mut statement = connection
            .prepare("SELECT 1 AS value UNION ALL SELECT 2 ORDER BY value")
            .unwrap();
        let mut rows = statement.query([]).unwrap();
        let mut complete_ledger = TestHistoryLedger::new(3);
        let complete =
            collect_canonical_rows(&mut rows, &mut complete_ledger, |row| row.get::<_, i64>(0))
                .unwrap();
        assert_eq!(complete, Some(vec![1_i64, 2_i64]));
        assert_eq!(complete_ledger.consumed, 3);
        drop(rows);

        let mut rows = statement.query([]).unwrap();
        let mut exhausted_ledger = TestHistoryLedger::new(2);
        let exhausted =
            collect_canonical_rows(&mut rows, &mut exhausted_ledger, |row| row.get::<_, i64>(0))
                .unwrap();
        assert_eq!(exhausted, None);
        assert_eq!(exhausted_ledger.consumed, 2);
    }

    #[test]
    fn malformed_canonical_file_row_fails_without_comparing_a_partial_map() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE generation_file (
                    generation_id TEXT NOT NULL,
                    file_id INTEGER NOT NULL,
                    canonical_path TEXT NOT NULL,
                    revision_id TEXT,
                    presence_state TEXT NOT NULL
                 );
                 CREATE TABLE file_revision (
                    revision_id TEXT PRIMARY KEY,
                    content_hash TEXT
                 );
                 INSERT INTO file_revision (revision_id, content_hash)
                 VALUES ('revision-a', 'hash-a');
                 INSERT INTO generation_file (
                    generation_id, file_id, canonical_path, revision_id, presence_state
                 ) VALUES ('generation-a', 7, 'src/a.rs', 'revision-a', 'present');",
            )
            .unwrap();
        let mut ledger = TestHistoryLedger::new(10);

        let error = match files_for_with_ledger(&connection, "generation-a", &mut ledger) {
            Ok(_) => panic!("malformed canonical evidence must fail closed"),
            Err(error) => error,
        };

        assert!(matches!(error, AtlasError::Sqlite(_)));
        assert_eq!(ledger.consumed, 1);
    }

    #[test]
    fn symbol_coverage_filters_mixed_file_ids_per_witness_row() {
        let symbol = SymbolSnapshot {
            canonical_symbol_key: "alpha".to_string(),
            symbol_fact_id: "symbol-alpha".to_string(),
            canonical_path: "src/a.ts".to_string(),
            file_id: "file-alpha".to_string(),
            file_content_hash: "hash-alpha".to_string(),
            extractor_complete: true,
            provider_key: "tree-sitter@1".to_string(),
            semantic_digest: "semantic".to_string(),
            evidence_digest: "evidence".to_string(),
        };
        let witness = |status: &str, file_id: &str| CoverageStateSnapshot {
            status: status.to_string(),
            file_id: Some(file_id.to_string()),
            capabilities_json: "[]".to_string(),
            limitations_json: "[]".to_string(),
            details_json: "{}".to_string(),
        };
        let mut coverage = CoverageMap::new();
        coverage.insert(
            "file:shared-scope".to_string(),
            vec![
                witness("complete", "file-alpha"),
                witness("partial", "file-other"),
            ],
        );

        assert!(symbol_has_complete_coverage(&coverage, &symbol));
        coverage.get_mut("file:shared-scope").unwrap()[0].status = "partial".to_string();
        assert!(!symbol_has_complete_coverage(&coverage, &symbol));
    }

    #[test]
    fn bounded_delta_is_all_or_nothing_and_matches_legacy_when_complete() {
        let (_database, connection, workspace, workspace_directory) = setup();
        let from_generation_id: String = connection
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![workspace.workspace_id],
                |row| row.get(0),
            )
            .unwrap();
        std::fs::write(
            workspace_directory.path().join("src/b.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        let config =
            Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
                .unwrap();
        let to_generation_id = discovery::reconcile(&workspace, &connection, &config)
            .unwrap()
            .candidate_generation_id;
        let legacy = compute_generation_delta(
            &connection,
            &workspace,
            &from_generation_id,
            &to_generation_id,
        )
        .unwrap();

        let mut complete_ledger = TestHistoryLedger::new(10_000);
        let complete = compute_generation_delta_with_ledger(
            &connection,
            &workspace,
            &from_generation_id,
            &to_generation_id,
            &mut complete_ledger,
        )
        .unwrap();
        let Some(complete) = complete else {
            panic!("ample semantic-row work must complete the canonical delta");
        };
        assert_eq!(
            serde_json::to_vec(&complete).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        assert!(complete_ledger.consumed > 1);

        let mut exhausted_ledger = TestHistoryLedger::new(complete_ledger.consumed - 1);
        let exhausted = compute_generation_delta_with_ledger(
            &connection,
            &workspace,
            &from_generation_id,
            &to_generation_id,
            &mut exhausted_ledger,
        )
        .unwrap();
        assert!(exhausted.is_none());
        assert_eq!(exhausted_ledger.consumed, complete_ledger.consumed - 1);
    }

    #[test]
    fn delta_across_identical_generation_is_rejected() {
        let (_d, conn, ws, _wd) = setup();
        let gen_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        let err = compute_generation_delta(&conn, &ws, &gen_id, &gen_id);
        assert!(
            err.is_err(),
            "diffing a generation against itself must be rejected"
        );
    }

    #[test]
    fn delta_detects_added_file_and_symbol() {
        let (_d, conn, ws, wd) = setup();
        let gen1: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();

        std::fs::write(
            wd.path().join("src/b.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let report2 = discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(report2.activation, "committed");
        let gen2 = report2.candidate_generation_id;
        for (coverage_id, generation_id) in [
            ("coverage-before", gen1.as_str()),
            ("coverage-after", gen2.as_str()),
        ] {
            conn.execute(
                "INSERT INTO coverage_record (
                    coverage_id, generation_id, scope_kind, scope_key, status,
                    capabilities_json, limitations_json, details_json
                 ) VALUES (?1,?2,'workspace','all','complete','[\"symbols\"]','[]','{}')",
                params![coverage_id, generation_id],
            )
            .unwrap();
        }

        let delta = compute_generation_delta(&conn, &ws, &gen1, &gen2).unwrap();
        assert!(delta
            .files
            .changes
            .iter()
            .any(|c| c.entity_id == "src/b.ts" && c.change_kind == ChangeKind::Added));
        assert!(
            delta
                .symbols
                .changes
                .iter()
                .any(|c| c.change_kind == ChangeKind::Added),
            "the new symbol beta must be reported as added"
        );
        assert!(
            delta.unchanged_verified_contracts.iter().any(|claim| claim
                .evidence
                .iter()
                .any(|reason| reason == "coverage_complete")),
            "complete matching coverage qualifies explicit unchanged evidence"
        );
    }

    #[test]
    fn delta_persists_and_is_idempotent() {
        let (_d, conn, ws, wd) = setup();
        let gen1: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        std::fs::write(
            wd.path().join("src/b.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let gen2 = discovery::reconcile(&ws, &conn, &cfg)
            .unwrap()
            .candidate_generation_id;

        let delta = compute_generation_delta(&conn, &ws, &gen1, &gen2).unwrap();
        persist_generation_delta(&conn, &delta).unwrap();
        persist_generation_delta(&conn, &delta).unwrap(); // idempotent re-persist

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_delta WHERE delta_id = ?1",
                params![delta.delta_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let item_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_delta_item WHERE delta_id = ?1",
                params![delta.delta_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(item_count > 0);
    }

    #[test]
    fn evidence_lease_verifies_against_live_source_and_auto_invalidates_on_change() {
        let (_d, conn, ws, wd) = setup();
        let gen_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        let symbol_key = first_symbol_key(&conn, &gen_id);
        let hash = crate::hashing::content_hash_of_file(&wd.path().join("src/a.ts")).unwrap();

        let lease_id = create_evidence_lease(
            &conn,
            &ws,
            &gen_id,
            "symbol",
            &symbol_key,
            Some(&hash),
            None,
            None,
            None,
        )
        .unwrap();
        let v1 = verify_evidence_lease(&conn, &ws, &lease_id, "src/a.ts").unwrap();
        assert_eq!(v1, LeaseVerification::Valid);

        // Mutate the source after the lease was created, without reconciling.
        std::fs::write(wd.path().join("src/a.ts"), "// mutated\n").unwrap();
        let v2 = verify_evidence_lease(&conn, &ws, &lease_id, "src/a.ts").unwrap();
        match v2 {
            LeaseVerification::StaleAutoInvalidated { .. } => {}
            other => panic!("expected StaleAutoInvalidated, got {other:?}"),
        }

        let state: String = conn
            .query_row(
                "SELECT state FROM evidence_lease WHERE lease_id = ?1",
                params![lease_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "invalidated",
            "a stale lease must be auto-invalidated by verification, never left claiming valid"
        );

        // Once invalidated, re-verification reports AlreadyInvalid rather
        // than re-running the disk check as if it still mattered.
        let v3 = verify_evidence_lease(&conn, &ws, &lease_id, "src/a.ts").unwrap();
        assert_eq!(v3, LeaseVerification::AlreadyInvalid);
    }

    #[test]
    fn evidence_lease_without_source_hash_never_claims_valid() {
        let (_d, conn, ws, _wd) = setup();
        let gen_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        let symbol_key = first_symbol_key(&conn, &gen_id);
        let lease_id = create_evidence_lease(
            &conn,
            &ws,
            &gen_id,
            "symbol",
            &symbol_key,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let v = verify_evidence_lease(&conn, &ws, &lease_id, "src/a.ts").unwrap();
        assert_eq!(
            v,
            LeaseVerification::NoSourceToVerify,
            "a lease with no source hash must never be reported Valid"
        );
    }
}
