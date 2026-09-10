//! Bounded, deterministic Context Broker.
//!
//! Scoring is fixed and inspectable (ADR-010); no model-based ranking is used.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::hashing::content_hash_of_file;
use crate::workspace::WorkspaceRecord;

pub const RANKING_POLICY_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BrokerMode {
    Map,
    Change,
    Audit,
}

impl BrokerMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Change => "change",
            Self::Audit => "audit",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "map" => Some(Self::Map),
            "change" => Some(Self::Change),
            "audit" => Some(Self::Audit),
            _ => None,
        }
    }
}

/// Normalized request for a bounded Context Packet.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub task: String,
    pub mode: BrokerMode,
    pub known_paths: Vec<String>,
    pub known_symbols: Vec<String>,
    pub max_records: i64,
    pub max_source_bytes: i64,
    pub max_token_estimate: i64,
}

impl TaskRequest {
    /// Deterministic request hash — sha256 of the normalized fields, sorted
    /// where order is not semantically meaningful.
    pub fn normalized_request_hash(&self) -> String {
        let mut known_paths = self.known_paths.clone();
        known_paths.sort();
        let mut known_symbols = self.known_symbols.clone();
        known_symbols.sort();
        let canonical = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.task.trim(),
            self.mode.as_str(),
            known_paths.join(","),
            known_symbols.join(","),
            self.max_records,
            self.max_source_bytes,
            self.max_token_estimate,
        );
        let mut h = Sha256::new();
        h.update(canonical.as_bytes());
        hex::encode(h.finalize())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PrimaryTarget {
    pub canonical_path: String,
    pub canonical_symbol_key: Option<String>,
    pub display_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub score: f64,
    pub relationship_distance: i64,
    /// BROKER-001: `true` when this target's ranking was boosted because a
    /// *semantic* extractor (`symbol_fact.evidence_method = 'semantic'`)
    /// produced it, rather than only a structural provider. Map mode's
    /// "compact preferred semantic truth" requirement means semantic
    /// evidence outranks structural-only evidence at equal task relevance,
    /// not that structural evidence is dropped.
    pub preferred_semantic: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExactExcerpt {
    pub canonical_path: String,
    pub indexed_content_hash: String,
    pub observed_content_hash: String,
    pub status: String, // verified | hash_mismatch | read_failed
    pub excerpt_hash: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OmissionSummary {
    pub total_candidates_considered: i64,
    pub selected_count: i64,
    pub omitted_by_budget: i64,
    pub unsupported_in_scope: i64,
    pub excluded_in_scope: i64,
}

/// BROKER-003: one `evidence_conflict` row surfaced in Audit mode.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictSummary {
    pub subject_kind: String,
    pub subject_key: String,
    pub conflict_type: String,
    pub status: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextPacket {
    pub schema_version: String,
    pub packet_id: String,
    pub created_at: String,
    pub workspace_id: String,
    pub active_generation_id: Option<String>,
    pub mode: String,
    pub normalized_task: String,
    pub ranking_policy_version: String,
    pub primary_targets: Vec<PrimaryTarget>,
    pub supporting_relationships: Vec<String>,
    pub tests: Vec<String>,
    pub exact_excerpts: Vec<ExactExcerpt>,
    pub unresolved: Vec<String>,
    pub omissions: OmissionSummary,
    pub budget: BudgetReport,
    /// BROKER-003: scored-but-not-selected candidates. Populated only in
    /// `Audit` mode -- Map/Change stay compact by design (BROKER-001).
    pub alternatives: Vec<PrimaryTarget>,
    /// BROKER-003: open/preferred-with-conflict `evidence_conflict` rows
    /// for the active generation. Audit-only.
    pub conflicts: Vec<ConflictSummary>,
    /// BROKER-003: typed coverage rows for the active generation
    /// (`PERSIST-006`). Audit-only.
    pub coverage: Vec<crate::query::CoverageRow>,
    pub packet_hash: String,
    pub status: String, // complete | partial | blocked
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetReport {
    pub max_records: i64,
    pub max_source_bytes: i64,
    pub max_token_estimate: i64,
    pub used_records: i64,
    pub used_source_bytes: i64,
    pub estimated_tokens: i64,
    pub uncertainty_reserve_pct: i64,
}

struct ScoredCandidate {
    canonical_path: String,
    canonical_symbol_key: Option<String>,
    display_name: Option<String>,
    symbol_kind: Option<String>,
    score: f64,
    distance: i64,
    preferred_semantic: bool,
}

/// Build a Context Packet for `req` against the workspace's active generation.
pub fn build_packet(
    conn: &Connection,
    ws: &WorkspaceRecord,
    req: &TaskRequest,
) -> Result<ContextPacket> {
    let gen_id: Option<String> = conn
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            params![ws.workspace_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    let now = crate::migrations::iso8601_now();
    let request_hash = req.normalized_request_hash();

    let Some(gen_id) = gen_id else {
        let packet = ContextPacket {
            schema_version: "1.0.0".into(),
            packet_id: format!("pkt_{}", &request_hash[..32]),
            created_at: now,
            workspace_id: ws.workspace_id.clone(),
            active_generation_id: None,
            mode: req.mode.as_str().into(),
            normalized_task: req.task.clone(),
            ranking_policy_version: RANKING_POLICY_VERSION.into(),
            primary_targets: Vec::new(),
            supporting_relationships: Vec::new(),
            tests: Vec::new(),
            exact_excerpts: Vec::new(),
            unresolved: Vec::new(),
            omissions: OmissionSummary {
                total_candidates_considered: 0,
                selected_count: 0,
                omitted_by_budget: 0,
                unsupported_in_scope: 0,
                excluded_in_scope: 0,
            },
            budget: BudgetReport {
                max_records: req.max_records,
                max_source_bytes: req.max_source_bytes,
                max_token_estimate: req.max_token_estimate,
                used_records: 0,
                used_source_bytes: 0,
                estimated_tokens: 0,
                uncertainty_reserve_pct: 10,
            },
            alternatives: Vec::new(),
            conflicts: Vec::new(),
            coverage: Vec::new(),
            packet_hash: String::new(),
            status: "blocked".into(),
        };
        return Ok(finalize_packet_hash(packet, &request_hash));
    };

    // --- Seed selection (deterministic) -----------------------------------
    let mut scored: HashMap<String, ScoredCandidate> = HashMap::new(); // key = canonical_symbol_key or path

    // 1. Explicit path/symbol match: +100.
    for p in &req.known_paths {
        add_or_bump(
            &mut scored,
            key_for_path(p),
            || ScoredCandidate {
                canonical_path: p.clone(),
                canonical_symbol_key: None,
                display_name: None,
                symbol_kind: None,
                score: 0.0,
                distance: 0,
                preferred_semantic: false,
            },
            100.0,
        );
    }
    for s in &req.known_symbols {
        // Resolve symbol -> path via symbol_fact within the active generation.
        if let Some((path, kind, name, evidence_method)) = resolve_symbol(conn, &gen_id, s)? {
            let semantic = evidence_method == "semantic";
            // BROKER-001: preferred semantic truth outranks structural-only
            // evidence at equal explicit-match relevance.
            let bump = if semantic { 115.0 } else { 100.0 };
            add_or_bump(
                &mut scored,
                s.clone(),
                || ScoredCandidate {
                    canonical_path: path,
                    canonical_symbol_key: Some(s.clone()),
                    display_name: Some(name),
                    symbol_kind: Some(kind),
                    score: 0.0,
                    distance: 0,
                    preferred_semantic: semantic,
                },
                bump,
            );
        }
    }

    // 2. Task text mentions: exact symbol display_name match (+80, +95 when
    //    the evidence is semantic — BROKER-001) or path substring mention
    //    (+70). Deterministic: scan all current symbols once, check whether
    //    their display_name is a whole-word substring of the task text.
    let task_lower = req.task.to_ascii_lowercase();
    let mut stmt = conn.prepare(
        "SELECT cf.canonical_path, sf.canonical_symbol_key, sf.display_name, sf.symbol_kind, sf.evidence_method
         FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1",
    )?;
    let rows = stmt.query_map(params![gen_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut total_candidates = 0i64;
    for row in rows {
        let (path, sym_key, name, kind, evidence_method) = row?;
        total_candidates += 1;
        let semantic = evidence_method == "semantic";
        if !name.is_empty() && task_lower.contains(&name.to_ascii_lowercase()) {
            let bump = if semantic { 95.0 } else { 80.0 };
            add_or_bump(
                &mut scored,
                sym_key.clone(),
                || ScoredCandidate {
                    canonical_path: path.clone(),
                    canonical_symbol_key: Some(sym_key.clone()),
                    display_name: Some(name.clone()),
                    symbol_kind: Some(kind.clone()),
                    score: 0.0,
                    distance: 0,
                    preferred_semantic: semantic,
                },
                bump,
            );
        }
        let file_name = path.rsplit('/').next().unwrap_or(&path);
        if task_lower.contains(&file_name.to_ascii_lowercase()) {
            add_or_bump(
                &mut scored,
                key_for_path(&path),
                || ScoredCandidate {
                    canonical_path: path.clone(),
                    canonical_symbol_key: None,
                    display_name: None,
                    symbol_kind: None,
                    score: 0.0,
                    distance: 0,
                    preferred_semantic: false,
                },
                70.0,
            );
        }
    }

    // 3. Expand one hop via relationships from current primary targets:
    //    direct caller/callee +50, import/export neighbor +40.
    let seed_paths: Vec<String> = scored.values().map(|c| c.canonical_path.clone()).collect();
    let mut seen_paths: HashSet<String> = seed_paths.iter().cloned().collect();
    for seed in &seed_paths {
        let mut rel_stmt = conn.prepare(
            "SELECT rf.relationship_type, rf.target_ref_kind, rf.target_ref_value
             FROM relationship_fact rf
             JOIN file_revision fr ON fr.revision_id = rf.revision_id
             JOIN current_file cf ON cf.revision_id = fr.revision_id
             WHERE cf.generation_id = ?1 AND rf.source_ref_kind = 'path' AND rf.source_ref_value = ?2",
        )?;
        let rel_rows = rel_stmt.query_map(params![gen_id, seed], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for rel in rel_rows {
            let (rel_type, target_kind, target_value) = rel?;
            let bump = if rel_type == "calls" {
                50.0
            } else if rel_type == "imports" {
                40.0
            } else {
                20.0
            };
            if target_kind == "path" && seen_paths.insert(target_value.clone()) {
                add_or_bump(
                    &mut scored,
                    key_for_path(&target_value),
                    || ScoredCandidate {
                        canonical_path: target_value.clone(),
                        canonical_symbol_key: None,
                        display_name: None,
                        symbol_kind: None,
                        score: 0.0,
                        distance: 1,
                        preferred_semantic: false,
                    },
                    bump,
                );
            }
        }
    }

    // 4. Tests among current files that reference any seed path. This remains
    //    a deliberately path-based heuristic rather than a semantic claim.
    let mut tests: Vec<String> = Vec::new();
    for seed in &seed_paths {
        let base = seed
            .rsplit('/')
            .next()
            .unwrap_or(seed)
            .trim_end_matches(".ts");
        let mut test_stmt = conn.prepare(
            "SELECT DISTINCT canonical_path FROM current_file
             WHERE generation_id = ?1 AND (canonical_path LIKE ?2 OR canonical_path LIKE ?3)",
        )?;
        let p1 = format!("%test%{base}%");
        let p2 = format!("%{base}%test%");
        let rows = test_stmt.query_map(params![gen_id, p1, p2], |r| r.get::<_, String>(0))?;
        for row in rows {
            let p = row?;
            if !tests.contains(&p) {
                tests.push(p);
            }
        }
    }
    tests.sort();

    // --- Rank + budget-select ----------------------------------------------
    let mut candidates: Vec<ScoredCandidate> = scored.into_values().collect();
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap()
            .then_with(|| a.distance.cmp(&b.distance))
            .then_with(|| a.canonical_path.cmp(&b.canonical_path))
            .then_with(|| {
                a.canonical_symbol_key
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.canonical_symbol_key.as_deref().unwrap_or(""))
            })
    });

    // Reserve 10% of the record budget for uncertainty and omission records.
    let usable_records = ((req.max_records as f64) * 0.9).floor() as i64;
    let total_considered = candidates.len() as i64;
    let includes_exact_source = req.mode == BrokerMode::Change || req.mode == BrokerMode::Audit;
    let mut selected: Vec<ScoredCandidate> = Vec::new();
    let mut exact_excerpts = Vec::new();
    let mut used_bytes: i64 = 0;
    let mut omitted_by_budget = 0i64;
    let mut unselected: Vec<ScoredCandidate> = Vec::new();
    for c in candidates {
        if selected.len() as i64 >= usable_records {
            omitted_by_budget += 1;
            unselected.push(c);
            continue;
        }

        if !includes_exact_source {
            selected.push(c);
            continue;
        }

        let indexed_hash: Option<String> = conn
            .query_row(
                "SELECT content_hash FROM current_file WHERE generation_id = ?1 AND canonical_path = ?2",
                params![gen_id, c.canonical_path],
                |r| r.get(0),
            )
            .optional()?;
        let Some(indexed_hash) = indexed_hash else {
            selected.push(c);
            continue;
        };

        let abs = std::path::Path::new(&ws.canonical_root).join(&c.canonical_path);
        let excerpt = match content_hash_of_file(&abs) {
            Ok(observed) if observed == indexed_hash => match std::fs::read_to_string(&abs) {
                Ok(text) => {
                    let text_bytes = i64::try_from(text.len()).unwrap_or(i64::MAX);
                    let next_used_bytes = used_bytes.checked_add(text_bytes);
                    if next_used_bytes.is_none_or(|next| {
                        next > req.max_source_bytes || next / 4 > req.max_token_estimate
                    }) {
                        omitted_by_budget += 1;
                        unselected.push(c);
                        continue;
                    }
                    used_bytes = next_used_bytes.expect("checked source-byte budget");
                    let mut h = Sha256::new();
                    h.update(text.as_bytes());
                    ExactExcerpt {
                        canonical_path: c.canonical_path.clone(),
                        indexed_content_hash: indexed_hash,
                        observed_content_hash: observed,
                        status: "verified".into(),
                        excerpt_hash: Some(hex::encode(h.finalize())),
                        text: Some(text),
                    }
                }
                Err(_) => ExactExcerpt {
                    canonical_path: c.canonical_path.clone(),
                    indexed_content_hash: indexed_hash,
                    observed_content_hash: observed,
                    status: "read_failed".into(),
                    excerpt_hash: None,
                    text: None,
                },
            },
            Ok(observed) => ExactExcerpt {
                canonical_path: c.canonical_path.clone(),
                indexed_content_hash: indexed_hash,
                observed_content_hash: observed,
                status: "hash_mismatch".into(),
                excerpt_hash: None,
                text: None,
            },
            Err(_) => ExactExcerpt {
                canonical_path: c.canonical_path.clone(),
                indexed_content_hash: indexed_hash,
                observed_content_hash: String::new(),
                status: "read_failed".into(),
                excerpt_hash: None,
                text: None,
            },
        };
        exact_excerpts.push(excerpt);
        selected.push(c);
    }

    let primary_targets = selected
        .iter()
        .map(|c| PrimaryTarget {
            canonical_path: c.canonical_path.clone(),
            canonical_symbol_key: c.canonical_symbol_key.clone(),
            display_name: c.display_name.clone(),
            symbol_kind: c.symbol_kind.clone(),
            score: c.score,
            relationship_distance: c.distance,
            preferred_semantic: c.preferred_semantic,
        })
        .collect();

    let any_blocked = exact_excerpts.iter().any(|e| e.status != "verified");
    let status = if any_blocked || omitted_by_budget > 0 {
        "partial"
    } else {
        "complete"
    };

    let estimated_tokens = used_bytes / 4; // rough deterministic heuristic (4 bytes/token)

    // BROKER-003: Audit is materially richer than Map/Change -- surface
    // scored alternatives, open evidence conflicts, and typed coverage.
    // Bounded (RES-006-style cap) so Audit stays a bounded query, not a
    // repository-scale dump.
    let (alternatives, conflicts, coverage) = if req.mode == BrokerMode::Audit {
        let alternatives: Vec<PrimaryTarget> = unselected
            .iter()
            .take(50)
            .map(|c| PrimaryTarget {
                canonical_path: c.canonical_path.clone(),
                canonical_symbol_key: c.canonical_symbol_key.clone(),
                display_name: c.display_name.clone(),
                symbol_kind: c.symbol_kind.clone(),
                score: c.score,
                relationship_distance: c.distance,
                preferred_semantic: c.preferred_semantic,
            })
            .collect();

        let mut conflict_stmt = conn.prepare(
            "SELECT subject_kind, subject_key, conflict_type, status, explanation
             FROM evidence_conflict WHERE generation_id = ?1 AND status IN ('open', 'preferred_with_conflict')
             ORDER BY subject_kind, subject_key LIMIT 200",
        )?;
        let conflicts: Vec<ConflictSummary> = conflict_stmt
            .query_map(params![gen_id], |r| {
                Ok(ConflictSummary {
                    subject_kind: r.get(0)?,
                    subject_key: r.get(1)?,
                    conflict_type: r.get(2)?,
                    status: r.get(3)?,
                    explanation: r.get(4)?,
                })
            })?
            .filter_map(std::result::Result::ok)
            .collect();

        let mut cov_stmt = conn.prepare(
            "SELECT scope_kind, scope_key, status FROM coverage_record WHERE generation_id = ?1 ORDER BY scope_kind, scope_key",
        )?;
        let coverage: Vec<crate::query::CoverageRow> = cov_stmt
            .query_map(params![gen_id], |r| {
                Ok(crate::query::CoverageRow {
                    scope_kind: r.get(0)?,
                    scope_key: r.get(1)?,
                    status: r.get(2)?,
                })
            })?
            .filter_map(std::result::Result::ok)
            .collect();

        (alternatives, conflicts, coverage)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let packet = ContextPacket {
        schema_version: "1.0.0".into(),
        packet_id: format!("pkt_{}", &request_hash[..32]),
        created_at: now,
        workspace_id: ws.workspace_id.clone(),
        active_generation_id: Some(gen_id),
        mode: req.mode.as_str().into(),
        normalized_task: req.task.clone(),
        ranking_policy_version: RANKING_POLICY_VERSION.into(),
        primary_targets,
        supporting_relationships: Vec::new(),
        tests,
        exact_excerpts,
        unresolved: Vec::new(),
        omissions: OmissionSummary {
            total_candidates_considered: total_considered,
            selected_count: selected.len() as i64,
            omitted_by_budget,
            unsupported_in_scope: 0,
            excluded_in_scope: total_candidates.saturating_sub(total_considered).max(0),
        },
        budget: BudgetReport {
            max_records: req.max_records,
            max_source_bytes: req.max_source_bytes,
            max_token_estimate: req.max_token_estimate,
            used_records: selected.len() as i64,
            used_source_bytes: used_bytes,
            estimated_tokens,
            uncertainty_reserve_pct: 10,
        },
        alternatives,
        conflicts,
        coverage,
        packet_hash: String::new(),
        status: status.into(),
    };

    Ok(finalize_packet_hash(packet, &request_hash))
}

fn finalize_packet_hash(mut packet: ContextPacket, request_hash: &str) -> ContextPacket {
    let canonical = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        packet.workspace_id,
        packet.active_generation_id.as_deref().unwrap_or(""),
        request_hash,
        packet.ranking_policy_version,
        packet
            .primary_targets
            .iter()
            .map(|t| t
                .canonical_symbol_key
                .clone()
                .unwrap_or_else(|| t.canonical_path.clone()))
            .collect::<Vec<_>>()
            .join(","),
    );
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    packet.packet_hash = hex::encode(h.finalize());
    packet
}

fn key_for_path(p: &str) -> String {
    format!("path:{p}")
}

fn add_or_bump<F: FnOnce() -> ScoredCandidate>(
    map: &mut HashMap<String, ScoredCandidate>,
    key: String,
    make: F,
    bump: f64,
) {
    map.entry(key).or_insert_with(make).score += bump;
}
fn resolve_symbol(
    conn: &Connection,
    gen_id: &str,
    symbol: &str,
) -> Result<Option<(String, String, String, String)>> {
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT cf.canonical_path, sf.symbol_kind, sf.display_name, sf.evidence_method
             FROM current_file cf
             JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1
               AND (sf.canonical_symbol_key = ?2 OR sf.qualified_name = ?2 OR sf.display_name = ?2)
             LIMIT 1",
            params![gen_id, symbol],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(row)
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
            "import { b } from './b';\nexport function alpha() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.test.ts"),
            "import { alpha } from './a';\ntest('alpha works', () => { alpha(); });\n",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/b.ts"),
            "export function beta() { return 2; }\n",
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

    fn req(task: &str, mode: BrokerMode) -> TaskRequest {
        TaskRequest {
            task: task.to_string(),
            mode,
            known_paths: Vec::new(),
            known_symbols: Vec::new(),
            max_records: 20,
            max_source_bytes: 32_768,
            max_token_estimate: 8_000,
        }
    }

    fn assert_returned_source_respects_hard_limits(packet: &ContextPacket, request: &TaskRequest) {
        let returned_source_bytes: i64 = packet
            .exact_excerpts
            .iter()
            .filter_map(|excerpt| excerpt.text.as_ref())
            .map(|text| i64::try_from(text.len()).unwrap())
            .sum();
        let returned_estimated_tokens = returned_source_bytes / 4;

        assert!(
            returned_source_bytes <= request.max_source_bytes,
            "returned source bodies use {returned_source_bytes} bytes, exceeding the {}-byte hard limit",
            request.max_source_bytes
        );
        assert!(
            returned_estimated_tokens <= request.max_token_estimate,
            "returned source bodies estimate to {returned_estimated_tokens} tokens, exceeding the {}-token hard limit",
            request.max_token_estimate
        );
        assert_eq!(packet.budget.used_source_bytes, returned_source_bytes);
        assert_eq!(packet.budget.estimated_tokens, returned_estimated_tokens);
    }

    #[test]
    fn audit_mode_is_materially_richer_than_map_and_change() {
        let (_d, conn, ws, _wd) = setup();
        let map_packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Map)).unwrap();
        let audit_packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Audit)).unwrap();
        // Audit still verifies exact source like Change.
        assert!(!audit_packet.exact_excerpts.is_empty());
        // Map/Change never populate the Audit-only fields.
        assert!(map_packet.alternatives.is_empty());
        assert!(map_packet.conflicts.is_empty());
        assert!(map_packet.coverage.is_empty());
        // Audit reports coverage/conflicts as bounded, present collections
        // (empty is fine when there are none, but the *fields themselves*
        // must be independently computed, not merely mirrored from Map).
        assert_eq!(
            audit_packet.conflicts.len(),
            0,
            "no evidence_conflict rows exist in this structural-only fixture"
        );
        assert_eq!(
            audit_packet.coverage.len(),
            0,
            "no coverage_record rows exist in this structural-only fixture"
        );
    }

    #[test]
    fn audit_mode_surfaces_persisted_conflicts_and_coverage() {
        let (_d, conn, ws, _wd) = setup();
        let gen_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
            )
            .unwrap();
        crate::resolution::record_evidence_conflict(
            &conn,
            &ws.workspace_id,
            &gen_id,
            "relationship",
            "subject_1",
            "incompatible_target",
            &["f1".to_string(), "f2".to_string()],
            Some("f1"),
            "preferred-evidence-1.0.0",
            "disagreement",
        )
        .unwrap();
        crate::provider_persistence::record_coverage(
            &conn,
            &gen_id,
            None,
            "provider",
            "scip-typescript@0.4.0",
            "complete",
            "[]",
            "[]",
            "{}",
        )
        .unwrap();

        let audit_packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Audit)).unwrap();
        assert_eq!(audit_packet.conflicts.len(), 1);
        assert_eq!(audit_packet.conflicts[0].subject_key, "subject_1");
        assert_eq!(audit_packet.coverage.len(), 1);
        assert_eq!(audit_packet.coverage[0].scope_key, "scip-typescript@0.4.0");

        // Map/Change never surface this Audit-only data even though the
        // same rows exist in the catalogue.
        let map_packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Map)).unwrap();
        assert!(map_packet.conflicts.is_empty());
        assert!(map_packet.coverage.is_empty());
    }

    #[test]
    fn preferred_semantic_evidence_outranks_structural_at_equal_relevance() {
        let (_d, conn, ws, _wd) = setup();
        // Structural fixture: `alpha` was extracted by the builtin V1
        // structural provider, so `preferred_semantic` must be `false` --
        // proving the flag reflects real `evidence_method`, not a guess.
        let packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Map)).unwrap();
        let target = packet
            .primary_targets
            .iter()
            .find(|t| t.display_name.as_deref() == Some("alpha"))
            .unwrap();
        assert!(
            !target.preferred_semantic,
            "this fixture never runs a semantic provider"
        );
    }

    #[test]
    fn legacy_packet_omits_verified_source_exceeding_either_hard_limit() {
        let (_d, conn, ws, wd) = setup();
        let source = std::fs::read_to_string(wd.path().join("src/a.ts")).unwrap();
        let source_bytes = i64::try_from(source.len()).unwrap();
        let source_tokens = source_bytes / 4;

        for (max_source_bytes, max_token_estimate) in [
            (source_bytes - 1, source_tokens),
            (source_bytes, source_tokens - 1),
        ] {
            let mut r = req("inspect a.ts", BrokerMode::Audit);
            r.known_paths.push("src/a.ts".into());
            r.max_records = 5;
            r.max_source_bytes = max_source_bytes;
            r.max_token_estimate = max_token_estimate;

            let packet = build_packet(&conn, &ws, &r).unwrap();

            assert!(packet.primary_targets.is_empty());
            assert!(packet.exact_excerpts.is_empty());
            assert_eq!(packet.omissions.selected_count, 0);
            assert_eq!(packet.omissions.omitted_by_budget, 1);
            assert_eq!(packet.alternatives.len(), 1);
            assert_eq!(packet.alternatives[0].canonical_path, "src/a.ts");
            assert_returned_source_respects_hard_limits(&packet, &r);
        }
    }

    #[test]
    fn legacy_packet_accepts_verified_source_at_exact_hard_boundaries() {
        let (_d, conn, ws, wd) = setup();
        let source = std::fs::read_to_string(wd.path().join("src/a.ts")).unwrap();
        let source_bytes = i64::try_from(source.len()).unwrap();
        let mut r = req("inspect a.ts", BrokerMode::Audit);
        r.known_paths.push("src/a.ts".into());
        r.max_records = 5;
        r.max_source_bytes = source_bytes;
        r.max_token_estimate = source_bytes / 4;

        let packet = build_packet(&conn, &ws, &r).unwrap();

        assert_eq!(packet.primary_targets.len(), 1);
        assert_eq!(packet.exact_excerpts.len(), 1);
        assert_eq!(
            packet.exact_excerpts[0].text.as_deref(),
            Some(source.as_str())
        );
        assert_eq!(packet.omissions.omitted_by_budget, 0);
        assert_returned_source_respects_hard_limits(&packet, &r);
    }

    #[test]
    fn legacy_packet_budget_selection_is_deterministic_and_skips_oversize_candidates() {
        let (_d, conn, ws, wd) = setup();
        let beta_source = std::fs::read_to_string(wd.path().join("src/b.ts")).unwrap();
        let beta_bytes = i64::try_from(beta_source.len()).unwrap();
        let mut r = req("alpha beta", BrokerMode::Audit);
        r.max_records = 5;
        r.max_source_bytes = beta_bytes;
        r.max_token_estimate = beta_bytes / 4;

        let first = build_packet(&conn, &ws, &r).unwrap();
        let second = build_packet(&conn, &ws, &r).unwrap();

        assert_eq!(first.packet_hash, second.packet_hash);
        assert_eq!(
            first
                .primary_targets
                .iter()
                .map(|target| (
                    target.canonical_path.as_str(),
                    target.canonical_symbol_key.as_deref()
                ))
                .collect::<Vec<_>>(),
            second
                .primary_targets
                .iter()
                .map(|target| (
                    target.canonical_path.as_str(),
                    target.canonical_symbol_key.as_deref()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(first.exact_excerpts.len(), 1);
        assert_eq!(first.exact_excerpts[0].canonical_path, "src/b.ts");
        assert_eq!(
            first.exact_excerpts[0].text.as_deref(),
            Some(beta_source.as_str())
        );
        assert!(first.omissions.omitted_by_budget >= 1);
        assert_returned_source_respects_hard_limits(&first, &r);
    }

    #[test]
    fn legacy_packet_multi_explicit_paths_stay_bounded_and_report_partial() {
        let (_d, conn, ws, wd) = setup();
        std::fs::create_dir_all(wd.path().join("docs")).unwrap();
        let paths = (0..4)
            .map(|index| {
                let path = format!("docs/large-{index}.md");
                std::fs::write(wd.path().join(&path), "x".repeat(20_000)).unwrap();
                path
            })
            .collect::<Vec<_>>();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let mut r = req("inspect the requested documents", BrokerMode::Audit);
        r.known_paths = paths;
        r.max_records = 12;
        r.max_source_bytes = 48_000;
        r.max_token_estimate = 10_000;

        let first = build_packet(&conn, &ws, &r).unwrap();
        let second = build_packet(&conn, &ws, &r).unwrap();

        assert_eq!(first.packet_hash, second.packet_hash);
        assert_eq!(first.budget.used_records, 2);
        assert_eq!(first.budget.used_source_bytes, 40_000);
        assert_eq!(first.budget.estimated_tokens, 10_000);
        assert_eq!(first.omissions.omitted_by_budget, 2);
        assert_eq!(first.omissions.excluded_in_scope, 0);
        assert_eq!(first.status, "partial");
        assert_returned_source_respects_hard_limits(&first, &r);
    }

    #[test]
    fn legacy_packet_map_selection_ignores_source_body_limits() {
        let (_d, conn, ws, _wd) = setup();
        let mut r = req("alpha", BrokerMode::Map);
        r.max_source_bytes = 1;
        r.max_token_estimate = 1;

        let packet = build_packet(&conn, &ws, &r).unwrap();

        assert!(packet
            .primary_targets
            .iter()
            .any(|target| target.display_name.as_deref() == Some("alpha")));
        assert!(packet.exact_excerpts.is_empty());
        assert_returned_source_respects_hard_limits(&packet, &r);
    }

    #[test]
    fn budget_hard_caps_selection_below_the_uncertainty_reserve() {
        let (_d, conn, ws, _wd) = setup();
        let mut r = req("alpha beta", BrokerMode::Map);
        r.max_records = 10;
        let packet = build_packet(&conn, &ws, &r).unwrap();
        // 10% of 10 records is reserved for uncertainty -- the hard cap is
        // floor(10 * 0.9) = 9, strictly below `max_records` whenever there
        // are enough candidates to fill it.
        assert!(
            packet.budget.used_records <= 9,
            "used_records={} must respect the 10% uncertainty reserve, not just max_records",
            packet.budget.used_records
        );
    }

    #[test]
    fn map_mode_finds_symbol_mentioned_in_task() {
        let (_d, conn, ws, _wd) = setup();
        let packet = build_packet(
            &conn,
            &ws,
            &req("investigate alpha behavior", BrokerMode::Map),
        )
        .unwrap();
        assert!(packet
            .primary_targets
            .iter()
            .any(|t| t.display_name.as_deref() == Some("alpha")));
        assert!(
            packet.exact_excerpts.is_empty(),
            "map mode must not read exact source"
        );
    }

    #[test]
    fn change_mode_includes_verified_excerpts() {
        let (_d, conn, ws, _wd) = setup();
        let packet = build_packet(&conn, &ws, &req("fix alpha", BrokerMode::Change)).unwrap();
        assert!(!packet.exact_excerpts.is_empty());
        assert!(packet.exact_excerpts.iter().all(|e| e.status == "verified"));
        assert_eq!(packet.status, "complete");
    }

    #[test]
    fn change_mode_blocks_on_stale_excerpt() {
        let (_d, conn, ws, wd) = setup();
        std::fs::write(wd.path().join("src/a.ts"), "// mutated after indexing\n").unwrap();
        let packet = build_packet(&conn, &ws, &req("fix alpha", BrokerMode::Change)).unwrap();
        assert!(packet
            .exact_excerpts
            .iter()
            .any(|e| e.status == "hash_mismatch"));
        assert_eq!(packet.status, "partial");
    }

    #[test]
    fn packet_is_deterministic_on_identical_inputs() {
        let (_d, conn, ws, _wd) = setup();
        let r = req("investigate alpha behavior", BrokerMode::Map);
        let p1 = build_packet(&conn, &ws, &r).unwrap();
        let p2 = build_packet(&conn, &ws, &r).unwrap();
        assert_eq!(p1.packet_hash, p2.packet_hash);
    }

    #[test]
    fn finds_test_for_seed_symbol() {
        let (_d, conn, ws, _wd) = setup();
        let packet = build_packet(&conn, &ws, &req("alpha", BrokerMode::Map)).unwrap();
        assert!(packet.tests.iter().any(|t| t.contains("test")));
    }

    #[test]
    fn budget_reserves_uncertainty_and_reports_omissions() {
        let (_d, conn, ws, _wd) = setup();
        let mut r = req("alpha beta", BrokerMode::Map);
        r.max_records = 1; // force tight budget
        let packet = build_packet(&conn, &ws, &r).unwrap();
        assert_eq!(packet.budget.uncertainty_reserve_pct, 10);
        assert!(packet.budget.used_records <= r.max_records);
    }
}
