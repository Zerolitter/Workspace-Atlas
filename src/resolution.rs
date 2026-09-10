//! Relationship resolution and evidence merge/conflict projection.
//!
//! Provider facts are immutable evidence. Resolution is a separate,
//! generation-scoped record of the current entity a raw target identifies.
//! Raw evidence stays append-only; preference is a projection, never
//! destructive deduplication.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::migrations::iso8601_now;
use crate::scip_mapping::{MappedRelationship, RefKind};

// ---------------------------------------------------------------------------
// Resolution states + reason codes (RES-002, RES-003)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    ResolvedSymbol,
    ResolvedFile,
    External,
    Ambiguous,
    Unresolved,
    Invalid,
    Stale,
}

impl ResolutionStatus {
    pub fn as_str(&self) -> &'static str {
        use ResolutionStatus::*;
        match self {
            ResolvedSymbol => "resolved_symbol",
            ResolvedFile => "resolved_file",
            External => "external",
            Ambiguous => "ambiguous",
            Unresolved => "unresolved",
            Invalid => "invalid",
            Stale => "stale",
        }
    }

    /// Inverse of `as_str`, used by query-layer code reconstructing a
    /// `ResolutionStatus` from a persisted `relationship_resolution.status`
    /// DB value (e.g. `query::impact`'s verified/uncertain split). `None`
    /// for any value outside the bounded DB `CHECK` vocabulary.
    pub fn from_db_str(s: &str) -> Option<Self> {
        use ResolutionStatus::*;
        Some(match s {
            "resolved_symbol" => ResolvedSymbol,
            "resolved_file" => ResolvedFile,
            "external" => External,
            "ambiguous" => Ambiguous,
            "unresolved" => Unresolved,
            "invalid" => Invalid,
            "stale" => Stale,
            _ => return None,
        })
    }
}

/// Stable reason-code vocabulary for resolution outcomes.
pub mod reason {
    pub const EXACT_PROVIDER_SYMBOL: &str = "exact_provider_symbol";
    pub const EXACT_CANONICAL_SYMBOL: &str = "exact_canonical_symbol";
    pub const EXACT_RELATIVE_PATH: &str = "exact_relative_path";
    pub const SCIP_EXTERNAL_SYMBOL: &str = "scip_external_symbol";
    pub const SCIP_LOCAL_DEFINITION: &str = "scip_local_definition";
    pub const ALIAS_OR_REEXPORT: &str = "alias_or_reexport";
    pub const COMPOSED_SPAN_MATCH: &str = "composed_span_match";
    pub const MULTIPLE_VALID_TARGETS: &str = "multiple_valid_targets";
    pub const DYNAMIC_DISPATCH: &str = "dynamic_dispatch";
    pub const REFLECTIVE_LOOKUP: &str = "reflective_lookup";
    pub const MISSING_DOCUMENT: &str = "missing_document";
    pub const EXCLUDED_TARGET: &str = "excluded_target";
    pub const GENERATED_UNMAPPED: &str = "generated_unmapped";
    pub const OUTSIDE_WORKSPACE: &str = "outside_workspace";
    pub const MALFORMED_PROVIDER_SYMBOL: &str = "malformed_provider_symbol";
    pub const SPAN_MISMATCH: &str = "span_mismatch";
    pub const PROVIDER_PARTIAL: &str = "provider_partial";
    pub const PROVIDER_FAILED: &str = "provider_failed";
    pub const POLICY_FILTERED: &str = "policy_filtered";
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRef {
    Symbol(String),
    File(String),
    ExternalSymbol(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub value: String,
    pub score: f64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionOutcome {
    pub status: ResolutionStatus,
    pub resolved: Option<ResolvedRef>,
    pub reason_code: &'static str,
    pub confidence: f64,
    pub candidates: Vec<Candidate>,
}

/// The generation-scoped symbol universe a resolver run needs: which
/// canonical symbol keys exist, and — separately — which ones this specific
/// provider execution itself emitted (for the "exact provider symbol within
/// the same validated execution" precedence rule).
pub struct SymbolUniverse<'a> {
    /// canonical_symbol_key -> defining canonical document path
    pub globally_defined: &'a HashMap<String, String>,
    /// canonical_symbol_key set emitted by *this* provider execution
    pub this_execution: &'a std::collections::HashSet<String>,
    /// display name -> candidate canonical symbol keys (for ambiguity)
    pub by_display_name: &'a HashMap<String, Vec<String>>,
    /// canonical file paths known in this generation
    pub known_files: &'a std::collections::HashSet<String>,
}

/// Deterministic resolution order:
/// 1. exact provider symbol within the same validated provider execution;
/// 2. exact canonical symbol;
/// 3. explicit alias/re-export/implementation relation;
/// 4. exact canonical path;
/// 5. cross-provider composition (already marked by
///    `scip_mapping::compose_call_edge` — carried through, not redone here);
/// 6. bounded candidate generation for ambiguity;
/// 7. unresolved with reason.
///
/// Display-name matching alone never produces a resolution (RES-005) — it
/// only ever contributes to the `Ambiguous` candidate list.
pub fn resolve_relationship(
    rel: &MappedRelationship,
    universe: &SymbolUniverse<'_>,
) -> ResolutionOutcome {
    // Composed structural+semantic call edges already carry
    // `composed_span_match`; a resolved target here is authoritative.
    if rel.reason_code == reason::COMPOSED_SPAN_MATCH
        && (universe
            .globally_defined
            .contains_key(&rel.target_ref_value)
            || universe.this_execution.contains(&rel.target_ref_value))
    {
        return ResolutionOutcome {
            status: ResolutionStatus::ResolvedSymbol,
            resolved: Some(ResolvedRef::Symbol(rel.target_ref_value.clone())),
            reason_code: reason::COMPOSED_SPAN_MATCH,
            confidence: rel.confidence,
            candidates: vec![],
        };
    }

    // 1. Exact provider symbol within the same validated provider execution.
    if universe.this_execution.contains(&rel.target_ref_value) {
        return ResolutionOutcome {
            status: ResolutionStatus::ResolvedSymbol,
            resolved: Some(ResolvedRef::Symbol(rel.target_ref_value.clone())),
            reason_code: reason::EXACT_PROVIDER_SYMBOL,
            confidence: 0.98,
            candidates: vec![],
        };
    }

    // 2. Exact canonical symbol (defined anywhere in this generation).
    if universe
        .globally_defined
        .contains_key(&rel.target_ref_value)
    {
        return ResolutionOutcome {
            status: ResolutionStatus::ResolvedSymbol,
            resolved: Some(ResolvedRef::Symbol(rel.target_ref_value.clone())),
            reason_code: reason::EXACT_CANONICAL_SYMBOL,
            confidence: 0.95,
            candidates: vec![],
        };
    }

    // 3. Explicit alias/re-export/implementation relation.
    if matches!(
        rel.relationship_type,
        "IMPLEMENTS" | "TYPE_DEFINITION" | "DEFINED_BY"
    ) {
        // The target wasn't found by exact key above, but SCIP itself
        // asserted this relationship explicitly — trust it as a lower-rank
        // resolved reference rather than discarding the evidence, unless
        // the target is genuinely external.
        if !matches!(rel.target_ref_kind, RefKind::ExternalSymbol) {
            return ResolutionOutcome {
                status: ResolutionStatus::ResolvedSymbol,
                resolved: Some(ResolvedRef::Symbol(rel.target_ref_value.clone())),
                reason_code: reason::ALIAS_OR_REEXPORT,
                confidence: 0.85,
                candidates: vec![],
            };
        }
    }

    // 4. Exact canonical path (path-kind target only).
    if universe.known_files.contains(&rel.target_ref_value) {
        return ResolutionOutcome {
            status: ResolutionStatus::ResolvedFile,
            resolved: Some(ResolvedRef::File(rel.target_ref_value.clone())),
            reason_code: reason::EXACT_RELATIVE_PATH,
            confidence: 0.9,
            candidates: vec![],
        };
    }

    // External: SCIP marked (or Atlas classified) this target as outside
    // the locally-defined symbol universe for this execution.
    if matches!(rel.target_ref_kind, RefKind::ExternalSymbol) {
        return ResolutionOutcome {
            status: ResolutionStatus::External,
            resolved: Some(ResolvedRef::ExternalSymbol(rel.target_ref_value.clone())),
            reason_code: reason::SCIP_EXTERNAL_SYMBOL,
            confidence: 0.7,
            candidates: vec![],
        };
    }

    // 6. Bounded candidate generation for ambiguity: same display name,
    // multiple defining locations, none an exact key match.
    if let Some(display_name) = extract_display_name(&rel.target_ref_value) {
        if let Some(keys) = universe.by_display_name.get(&display_name) {
            if keys.len() > 1 {
                let candidates: Vec<Candidate> = keys
                    .iter()
                    .take(16) // bounded — RES-006
                    .map(|k| Candidate {
                        value: k.clone(),
                        score: 0.4,
                        reason: "display_name_match".to_string(),
                    })
                    .collect();
                return ResolutionOutcome {
                    status: ResolutionStatus::Ambiguous,
                    resolved: None,
                    reason_code: reason::MULTIPLE_VALID_TARGETS,
                    confidence: 0.0,
                    candidates,
                };
            }
        }
    }

    // 7. Unresolved.
    ResolutionOutcome {
        status: ResolutionStatus::Unresolved,
        resolved: None,
        reason_code: reason::MISSING_DOCUMENT,
        confidence: 0.0,
        candidates: vec![],
    }
}

/// Best-effort last-descriptor-segment extraction from a SCIP symbol string,
/// used only to seed ambiguity candidate search — never to *resolve*
/// (RES-005: "never resolve by display name alone").
fn extract_display_name(scip_symbol: &str) -> Option<String> {
    let trimmed = scip_symbol.trim_end_matches(['.', '#', '(', ')', ':', '!']);
    trimmed
        .rsplit(['/', '#', '.', ' '])
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Explicit unresolved marker for dynamic/reflective dispatch — a resolver
/// must never silently guess a target for these (RES-007, RISK S-011).
pub fn unresolved_dynamic(kind: DynamicKind) -> ResolutionOutcome {
    ResolutionOutcome {
        status: ResolutionStatus::Unresolved,
        resolved: None,
        reason_code: match kind {
            DynamicKind::RegistryLookup => reason::DYNAMIC_DISPATCH,
            DynamicKind::Reflective => reason::REFLECTIVE_LOOKUP,
        },
        confidence: 0.0,
        candidates: vec![],
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DynamicKind {
    RegistryLookup,
    Reflective,
}

// ---------------------------------------------------------------------------
// Persistence (RES-001)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn record_relationship_resolution(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    relationship_fact_id: &str,
    resolver_execution_id: Option<&str>,
    resolver_policy_version: &str,
    outcome: &ResolutionOutcome,
) -> Result<String> {
    let id = deterministic_id(
        "res",
        &[
            workspace_id,
            generation_id,
            relationship_fact_id,
            resolver_policy_version,
        ],
    );
    let (resolved_ref_kind, resolved_ref_value): (Option<&'static str>, Option<String>) =
        match &outcome.resolved {
            Some(ResolvedRef::Symbol(v)) => (Some("symbol"), Some(v.clone())),
            Some(ResolvedRef::File(v)) => (Some("file"), Some(v.clone())),
            Some(ResolvedRef::ExternalSymbol(v)) => (Some("external_symbol"), Some(v.clone())),
            None => (None, None),
        };
    let candidates_json = serde_json::to_string(
        &outcome
            .candidates
            .iter()
            .map(|c| serde_json::json!({"value": c.value, "score": c.score, "reason": c.reason}))
            .collect::<Vec<_>>(),
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO relationship_resolution (
            relationship_resolution_id, workspace_id, generation_id, relationship_fact_id,
            resolver_execution_id, resolver_policy_version, status, resolved_ref_kind,
            resolved_ref_value, reason_code, candidate_refs_json, confidence, evidence_json, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'[]',?13)",
        params![
            id,
            workspace_id,
            generation_id,
            relationship_fact_id,
            resolver_execution_id,
            resolver_policy_version,
            outcome.status.as_str(),
            resolved_ref_kind,
            resolved_ref_value,
            outcome.reason_code,
            candidates_json,
            outcome.confidence,
            iso8601_now(),
        ],
    )?;
    Ok(id)
}

pub(crate) fn deterministic_id(prefix: &str, parts: &[&str]) -> String {
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
// Evidence clustering, corroboration, conflict (MERGE-001..006)
// ---------------------------------------------------------------------------

/// One candidate fact contributing to a cluster's preference decision.
#[derive(Debug, Clone)]
pub struct EvidenceFact {
    pub fact_id: String,
    pub provider_key: String,
    pub evidence_tier_rank: i64,
    pub provider_priority: Option<i64>,
    pub deterministic: bool,
    pub confidence: f64,
    /// The resolved target this fact claims, for clustering purposes
    /// (MERGE-001: facts corroborate when subject+relationship-kind+resolved
    /// target agree).
    pub resolved_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClusterOutcome {
    /// All facts in the cluster agree — corroborated, one preferred fact,
    /// confidence may rise but never converts to "proven" beyond the
    /// evidence (MERGE-002).
    Corroborated {
        preferred_fact_id: String,
        corroborating_fact_ids: Vec<String>,
    },
    /// Facts disagree on the resolved target — a conflict record is
    /// required; a preference *may* still be chosen but alternatives are
    /// never deleted (MERGE-003, MERGE-005).
    Conflict {
        preferred_fact_id: String,
        all_fact_ids: Vec<String>,
        explanation: String,
    },
}

/// Deterministic preference order (MERGE-004):
/// 1. fact validity / current revision (assumed pre-filtered by caller);
/// 2. resolution status (assumed pre-filtered — only resolved facts here);
/// 3. workspace-configured provider priority;
/// 4. evidence tier;
/// 5. confidence;
/// 6. deterministic provider flag;
/// 7. provider_key / fact_id as stable tie-breakers.
fn preference_key(f: &EvidenceFact) -> (i64, i64, i64, i64, &str, &str) {
    (
        f.provider_priority.unwrap_or(0),
        f.evidence_tier_rank,
        (f.confidence * 1_000_000.0) as i64,
        f.deterministic as i64,
        f.provider_key.as_str(),
        f.fact_id.as_str(),
    )
}

/// Cluster and project a set of facts targeting the same subject (already
/// grouped by the caller per MERGE-001's clustering key) into a single
/// deterministic preference decision.
pub fn project_cluster(facts: &[EvidenceFact]) -> Option<ClusterOutcome> {
    if facts.is_empty() {
        return None;
    }
    if facts.len() == 1 {
        return Some(ClusterOutcome::Corroborated {
            preferred_fact_id: facts[0].fact_id.clone(),
            corroborating_fact_ids: vec![],
        });
    }

    let distinct_targets: std::collections::HashSet<&str> = facts
        .iter()
        .filter_map(|f| f.resolved_target.as_deref())
        .collect();

    let mut ordered: Vec<&EvidenceFact> = facts.iter().collect();
    ordered.sort_by(|a, b| preference_key(b).cmp(&preference_key(a))); // highest-preference first
    let preferred = ordered[0];

    if distinct_targets.len() <= 1 {
        let corroborating: Vec<String> = facts
            .iter()
            .filter(|f| f.fact_id != preferred.fact_id)
            .map(|f| f.fact_id.clone())
            .collect();
        Some(ClusterOutcome::Corroborated {
            preferred_fact_id: preferred.fact_id.clone(),
            corroborating_fact_ids: corroborating,
        })
    } else {
        let all_fact_ids: Vec<String> = facts.iter().map(|f| f.fact_id.clone()).collect();
        Some(ClusterOutcome::Conflict {
            preferred_fact_id: preferred.fact_id.clone(),
            all_fact_ids,
            explanation: format!(
                "{} facts disagree on resolved target ({} distinct targets); preferred {} by provider priority/tier/confidence",
                facts.len(),
                distinct_targets.len(),
                preferred.provider_key
            ),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn record_cluster_projection(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    subject_kind: &str,
    subject_key: &str,
    conflict_type: &str,
    participating_fact_ids: &[String],
    preferred_fact_id: Option<&str>,
    projection_policy_version: &str,
    status: &str,
    explanation: &str,
) -> Result<String> {
    let id = deterministic_id(
        "evc",
        &[
            workspace_id,
            generation_id,
            subject_kind,
            subject_key,
            conflict_type,
            projection_policy_version,
        ],
    );
    let participating_json = serde_json::to_string(participating_fact_ids)?;
    conn.execute(
        "INSERT OR IGNORE INTO evidence_conflict (
            evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
            conflict_type, participating_fact_ids_json, preferred_fact_id, projection_policy_version,
            status, explanation, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            id,
            workspace_id,
            generation_id,
            subject_kind,
            subject_key,
            conflict_type,
            participating_json,
            preferred_fact_id,
            projection_policy_version,
            status,
            explanation,
            iso8601_now(),
        ],
    )?;
    Ok(id)
}

/// MERGE-003, MERGE-005: persist an incompatible-target conflict, keeping
/// every participating fact id and the chosen preferred fact (never
/// deleting alternatives).
#[allow(clippy::too_many_arguments)]
pub fn record_evidence_conflict(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    subject_kind: &str,
    subject_key: &str,
    conflict_type: &str,
    participating_fact_ids: &[String],
    preferred_fact_id: Option<&str>,
    projection_policy_version: &str,
    explanation: &str,
) -> Result<String> {
    let status = if preferred_fact_id.is_some() {
        "preferred_with_conflict"
    } else {
        "open"
    };
    record_cluster_projection(
        conn,
        workspace_id,
        generation_id,
        subject_kind,
        subject_key,
        conflict_type,
        participating_fact_ids,
        preferred_fact_id,
        projection_policy_version,
        status,
        explanation,
    )
}

/// MERGE-002: persist that a cluster of facts agreed (status
/// `'corroborated'`, per `evidence_conflict.status`'s own `CHECK`
/// vocabulary — this table is the generic per-subject evidence-cluster
/// projection ledger, not only for genuine conflicts).
#[allow(clippy::too_many_arguments)]
pub fn record_corroboration(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    subject_kind: &str,
    subject_key: &str,
    participating_fact_ids: &[String],
    preferred_fact_id: &str,
    projection_policy_version: &str,
) -> Result<String> {
    record_cluster_projection(
        conn,
        workspace_id,
        generation_id,
        subject_kind,
        subject_key,
        "corroboration",
        participating_fact_ids,
        Some(preferred_fact_id),
        projection_policy_version,
        "corroborated",
        &format!(
            "{} facts agree on the resolved target",
            participating_fact_ids.len()
        ),
    )
}

/// MERGE-006: dispatch a `project_cluster` outcome to the matching
/// persistence path. Re-running this for a new generation naturally
/// re-evaluates the cluster — nothing here copies a prior generation's row
/// forward.
pub fn persist_cluster_outcome(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
    subject_kind: &str,
    subject_key: &str,
    outcome: &ClusterOutcome,
    projection_policy_version: &str,
) -> Result<String> {
    match outcome {
        ClusterOutcome::Corroborated {
            preferred_fact_id,
            corroborating_fact_ids,
        } => {
            let mut all = vec![preferred_fact_id.clone()];
            all.extend(corroborating_fact_ids.iter().cloned());
            record_corroboration(
                conn,
                workspace_id,
                generation_id,
                subject_kind,
                subject_key,
                &all,
                preferred_fact_id,
                projection_policy_version,
            )
        }
        ClusterOutcome::Conflict {
            preferred_fact_id,
            all_fact_ids,
            explanation,
        } => record_evidence_conflict(
            conn,
            workspace_id,
            generation_id,
            subject_kind,
            subject_key,
            "incompatible_target",
            all_fact_ids,
            Some(preferred_fact_id),
            projection_policy_version,
            explanation,
        ),
    }
}

// ---------------------------------------------------------------------------
// Impact frontier (QUERY-005): verified vs uncertain
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactFrontier {
    /// Resolved, preferred-evidence inbound relationships — the frontier a
    /// caller can act on with confidence.
    pub verified: Vec<String>,
    /// Ambiguous/unresolved candidate paths that *may* be affected — must
    /// stay separately labelled and budgeted, never merged into `verified`.
    pub uncertain: Vec<String>,
}

/// Split resolution outcomes into separately labelled verified and uncertain
/// impact frontiers.
pub fn split_impact_frontier(outcomes: &[(String, ResolutionOutcome)]) -> ImpactFrontier {
    let mut verified = Vec::new();
    let mut uncertain = Vec::new();
    for (subject, outcome) in outcomes {
        match outcome.status {
            ResolutionStatus::ResolvedSymbol | ResolutionStatus::ResolvedFile => {
                verified.push(subject.clone())
            }
            ResolutionStatus::Ambiguous
            | ResolutionStatus::Unresolved
            | ResolutionStatus::External
            | ResolutionStatus::Stale
            | ResolutionStatus::Invalid => uncertain.push(subject.clone()),
        }
    }
    ImpactFrontier {
        verified,
        uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scip_mapping::RefKind;

    fn rel(
        relationship_type: &'static str,
        target: &str,
        target_kind: RefKind,
        reason_code: &'static str,
    ) -> MappedRelationship {
        MappedRelationship {
            relationship_type,
            source_canonical_document_path: "src/a.ts".to_string(),
            source_start_line: 1,
            source_start_column: 1,
            source_end_line: 1,
            source_end_column: 5,
            target_ref_kind: target_kind,
            target_ref_value: target.to_string(),
            provider_symbol_id: target.to_string(),
            confidence: 0.9,
            reason_code,
        }
    }

    #[allow(clippy::type_complexity)]
    fn empty_universe() -> (
        HashMap<String, String>,
        std::collections::HashSet<String>,
        HashMap<String, Vec<String>>,
        std::collections::HashSet<String>,
    ) {
        (
            HashMap::new(),
            std::collections::HashSet::new(),
            HashMap::new(),
            std::collections::HashSet::new(),
        )
    }

    #[test]
    fn resolves_exact_provider_symbol_from_same_execution_first() {
        let (globally_defined, mut this_execution, by_display_name, known_files) = empty_universe();
        this_execution.insert("pkg foo().".to_string());
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "REFERENCES",
            "pkg foo().",
            RefKind::Symbol,
            "scip_occurrence",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::ResolvedSymbol);
        assert_eq!(outcome.reason_code, reason::EXACT_PROVIDER_SYMBOL);
    }

    #[test]
    fn resolves_exact_canonical_symbol_when_defined_elsewhere_in_generation() {
        let (mut globally_defined, this_execution, by_display_name, known_files) = empty_universe();
        globally_defined.insert("pkg bar().".to_string(), "src/other.ts".to_string());
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "REFERENCES",
            "pkg bar().",
            RefKind::Symbol,
            "scip_occurrence",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::ResolvedSymbol);
        assert_eq!(outcome.reason_code, reason::EXACT_CANONICAL_SYMBOL);
    }

    #[test]
    fn resolves_implements_relation_as_alias_or_reexport() {
        let (globally_defined, this_execution, by_display_name, known_files) = empty_universe();
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "IMPLEMENTS",
            "pkg Animal#",
            RefKind::Symbol,
            "scip_symbol_relationship",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::ResolvedSymbol);
        assert_eq!(outcome.reason_code, reason::ALIAS_OR_REEXPORT);
    }

    #[test]
    fn external_target_resolves_to_external_not_missing() {
        let (globally_defined, this_execution, by_display_name, known_files) = empty_universe();
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "REFERENCES",
            "npm lodash 4.0.0 map().",
            RefKind::ExternalSymbol,
            "scip_occurrence",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::External);
        assert_eq!(outcome.reason_code, reason::SCIP_EXTERNAL_SYMBOL);
    }

    #[test]
    fn ambiguous_display_name_returns_bounded_candidates_never_a_resolution() {
        let (globally_defined, this_execution, mut by_display_name, known_files) = empty_universe();
        by_display_name.insert(
            "reset".to_string(),
            vec!["pkg a/reset.".to_string(), "pkg b/reset.".to_string()],
        );
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "REFERENCES",
            "pkg unknown/reset.",
            RefKind::Symbol,
            "scip_occurrence",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::Ambiguous);
        assert_eq!(outcome.reason_code, reason::MULTIPLE_VALID_TARGETS);
        assert_eq!(outcome.candidates.len(), 2);
        assert!(
            outcome.resolved.is_none(),
            "ambiguity must never silently pick a winner"
        );
    }

    #[test]
    fn truly_unknown_target_is_unresolved_not_fabricated() {
        let (globally_defined, this_execution, by_display_name, known_files) = empty_universe();
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let r = rel(
            "REFERENCES",
            "pkg totallyUnknownXyz.",
            RefKind::Symbol,
            "scip_occurrence",
        );
        let outcome = resolve_relationship(&r, &universe);
        assert_eq!(outcome.status, ResolutionStatus::Unresolved);
        assert!(outcome.resolved.is_none());
    }

    #[test]
    fn dynamic_registry_lookup_stays_explicitly_unresolved() {
        let outcome = unresolved_dynamic(DynamicKind::RegistryLookup);
        assert_eq!(outcome.status, ResolutionStatus::Unresolved);
        assert_eq!(outcome.reason_code, reason::DYNAMIC_DISPATCH);
    }

    fn fact(
        id: &str,
        tier: i64,
        priority: Option<i64>,
        confidence: f64,
        target: &str,
        deterministic: bool,
    ) -> EvidenceFact {
        EvidenceFact {
            fact_id: id.to_string(),
            provider_key: format!("provider_{id}"),
            evidence_tier_rank: tier,
            provider_priority: priority,
            deterministic,
            confidence,
            resolved_target: Some(target.to_string()),
        }
    }

    #[test]
    fn single_fact_cluster_is_trivially_corroborated() {
        let outcome = project_cluster(&[fact("f1", 800, None, 0.9, "target_a", true)]).unwrap();
        assert!(matches!(outcome, ClusterOutcome::Corroborated { .. }));
    }

    #[test]
    fn agreeing_facts_corroborate_with_a_deterministic_preferred_fact() {
        let facts = vec![
            fact("f1", 800, Some(800), 0.9, "target_a", true),
            fact("f2", 500, Some(500), 0.7, "target_a", true),
        ];
        let outcome = project_cluster(&facts).unwrap();
        match outcome {
            ClusterOutcome::Corroborated {
                preferred_fact_id,
                corroborating_fact_ids,
            } => {
                assert_eq!(preferred_fact_id, "f1", "higher provider priority must win");
                assert_eq!(corroborating_fact_ids, vec!["f2".to_string()]);
            }
            other => panic!("expected Corroborated, got {other:?}"),
        }
    }

    #[test]
    fn disagreeing_facts_produce_a_conflict_not_a_silent_pick() {
        let facts = vec![
            fact("f1", 800, Some(800), 0.9, "target_a", true),
            fact("f2", 500, Some(500), 0.9, "target_b", true),
        ];
        let outcome = project_cluster(&facts).unwrap();
        match outcome {
            ClusterOutcome::Conflict {
                preferred_fact_id,
                all_fact_ids,
                ..
            } => {
                assert_eq!(preferred_fact_id, "f1");
                assert_eq!(
                    all_fact_ids.len(),
                    2,
                    "conflict must retain every participating fact id"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn preference_is_deterministic_across_repeated_runs() {
        let facts = vec![
            fact("f2", 500, Some(500), 0.9, "target_b", true),
            fact("f1", 800, Some(800), 0.9, "target_a", true),
        ];
        let a = project_cluster(&facts);
        let b = project_cluster(&facts);
        assert_eq!(a, b);
    }

    #[test]
    fn evidence_conflict_persists_and_retains_all_fact_ids() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/ws"),
            &cfg,
            std::path::Path::new("/ws/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let gen = crate::generation::begin_candidate(
            &conn,
            &ws.workspace_id,
            crate::generation::TriggerKind::Baseline,
            "psh",
            "1.1.0",
        )
        .unwrap();

        let id = record_evidence_conflict(
            &conn,
            &ws.workspace_id,
            &gen.generation_id,
            "relationship",
            "subject_1",
            "incompatible_target",
            &["f1".to_string(), "f2".to_string()],
            Some("f1"),
            "preferred-evidence-1.0.0",
            "disagreement",
        )
        .unwrap();
        let stored_ids_json: String = conn.query_row("SELECT participating_fact_ids_json FROM evidence_conflict WHERE evidence_conflict_id=?1", params![id], |r| r.get(0)).unwrap();
        let stored_ids: Vec<String> = serde_json::from_str(&stored_ids_json).unwrap();
        assert_eq!(stored_ids, vec!["f1".to_string(), "f2".to_string()]);
    }

    #[test]
    fn corroboration_persists_with_corroborated_status() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/ws"),
            &cfg,
            std::path::Path::new("/ws/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let gen = crate::generation::begin_candidate(
            &conn,
            &ws.workspace_id,
            crate::generation::TriggerKind::Baseline,
            "psh",
            "1.1.0",
        )
        .unwrap();

        let facts = vec![
            fact("f1", 800, Some(800), 0.9, "target_a", true),
            fact("f2", 500, Some(500), 0.7, "target_a", true),
        ];
        let outcome = project_cluster(&facts).unwrap();
        let id = persist_cluster_outcome(
            &conn,
            &ws.workspace_id,
            &gen.generation_id,
            "symbol",
            "subject_2",
            &outcome,
            "preferred-evidence-1.0.0",
        )
        .unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM evidence_conflict WHERE evidence_conflict_id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "corroborated");
    }
    #[test]
    fn impact_frontier_separates_verified_from_uncertain() {
        let outcomes = vec![
            (
                "dep_a".to_string(),
                ResolutionOutcome {
                    status: ResolutionStatus::ResolvedSymbol,
                    resolved: Some(ResolvedRef::Symbol("x".into())),
                    reason_code: reason::EXACT_CANONICAL_SYMBOL,
                    confidence: 0.9,
                    candidates: vec![],
                },
            ),
            (
                "dep_b".to_string(),
                ResolutionOutcome {
                    status: ResolutionStatus::Ambiguous,
                    resolved: None,
                    reason_code: reason::MULTIPLE_VALID_TARGETS,
                    confidence: 0.0,
                    candidates: vec![],
                },
            ),
            (
                "dep_c".to_string(),
                ResolutionOutcome {
                    status: ResolutionStatus::Unresolved,
                    resolved: None,
                    reason_code: reason::MISSING_DOCUMENT,
                    confidence: 0.0,
                    candidates: vec![],
                },
            ),
        ];
        let frontier = split_impact_frontier(&outcomes);
        assert_eq!(frontier.verified, vec!["dep_a".to_string()]);
        assert_eq!(
            frontier.uncertain,
            vec!["dep_b".to_string(), "dep_c".to_string()]
        );
    }

    /// End-to-end proof (RES-001): a real `discovery::reconcile` run
    /// produces a real `file_revision`/`extractor_run`; a semantic
    /// `MappedRelationship` is persisted into the same `relationship_fact`
    /// table via `provider_persistence::record_semantic_relationship_fact`;
    /// resolving it and persisting the resolution round-trips through the
    /// real `relationship_resolution` table with its `relationship_fact_id`
    /// foreign key intact.
    #[test]
    fn relationship_resolution_persists_against_a_real_relationship_fact_row() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::migrations::apply_all(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "export function foo() {}\n").unwrap();
        let cfg = crate::config::Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"t\"\n",
        )
        .unwrap();
        let ws = crate::workspace::register_workspace(
            &conn,
            dir.path(),
            &cfg,
            std::path::Path::new("/ws/.atlas/db.sqlite"),
            "p1",
        )
        .unwrap();
        let report = crate::discovery::reconcile(&ws, &conn, &cfg).unwrap();
        assert!(!report.candidate_generation_id.is_empty());

        let (extractor_run_id, revision_id): (String, String) = conn
            .query_row(
                "SELECT extractor_run_id, revision_id FROM extractor_run LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        let semantic_rel = crate::scip_mapping::MappedRelationship {
            relationship_type: "REFERENCES",
            source_canonical_document_path: "a.ts".to_string(),
            source_start_line: 0,
            source_start_column: 0,
            source_end_line: 0,
            source_end_column: 3,
            target_ref_kind: RefKind::Symbol,
            target_ref_value: "scip:pkg foo().".to_string(),
            provider_symbol_id: "scip:pkg foo().".to_string(),
            confidence: 0.9,
            reason_code: "scip_occurrence",
        };
        let relationship_fact_id = crate::provider_persistence::record_semantic_relationship_fact(
            &conn,
            &extractor_run_id,
            &revision_id,
            &semantic_rel,
        )
        .unwrap();

        let (globally_defined, this_execution, by_display_name, known_files) = {
            let mut g = HashMap::new();
            g.insert("scip:pkg foo().".to_string(), "a.ts".to_string());
            (
                g,
                std::collections::HashSet::new(),
                HashMap::new(),
                std::collections::HashSet::new(),
            )
        };
        let universe = SymbolUniverse {
            globally_defined: &globally_defined,
            this_execution: &this_execution,
            by_display_name: &by_display_name,
            known_files: &known_files,
        };
        let outcome = resolve_relationship(&semantic_rel, &universe);
        assert_eq!(outcome.status, ResolutionStatus::ResolvedSymbol);

        let resolution_id = record_relationship_resolution(
            &conn,
            &ws.workspace_id,
            &report.candidate_generation_id,
            &relationship_fact_id,
            None,
            "semantic-resolution-1.0.0",
            &outcome,
        )
        .unwrap();

        let (status, fk): (String, String) = conn
            .query_row("SELECT status, relationship_fact_id FROM relationship_resolution WHERE relationship_resolution_id=?1", params![resolution_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(status, "resolved_symbol");
        assert_eq!(fk, relationship_fact_id);
    }
}
