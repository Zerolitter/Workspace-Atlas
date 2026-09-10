//! Deterministic, generation-bound query service.
//!
//! `find`, `inspect`, `trace`, `impact`, and `source` operate against one
//! captured active generation and return coverage-bounded results.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{AtlasError, Result};
use crate::workspace::WorkspaceRecord;

static OBSERVATION_ORDINAL: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
pub struct QueryAttribution<'a> {
    pub task_session_id: &'a str,
}

impl<'a> QueryAttribution<'a> {
    pub fn new(task_session_id: &'a str) -> Self {
        Self { task_session_id }
    }
}

struct ObservationRecord<'a> {
    event_type: crate::context_ir::ContextUseEventType,
    operation: &'static str,
    input: &'a str,
    status: &'static str,
    result_count: i64,
    validation_kind: Option<&'a str>,
}

fn record_query_observation(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    attribution: QueryAttribution<'_>,
    observation: ObservationRecord<'_>,
) -> Result<()> {
    let ObservationRecord {
        event_type,
        operation,
        input,
        status,
        result_count,
        validation_kind,
    } = observation;
    let session_workspace: Option<String> = connection
        .query_row(
            "SELECT workspace_id FROM task_session WHERE task_session_id = ?1",
            params![attribution.task_session_id],
            |row| row.get(0),
        )
        .optional()?;
    if session_workspace.as_deref() != Some(workspace.workspace_id.as_str()) {
        return Err(AtlasError::InvalidConfig(format!(
            "task session {} does not belong to workspace {}",
            attribution.task_session_id, workspace.workspace_id
        )));
    }
    let input_hash = crate::hashing::content_hash_of_bytes(input.as_bytes());
    let ordinal = OBSERVATION_ORDINAL.fetch_add(1, Ordering::Relaxed);
    let mut details = serde_json::json!({
        "operation": operation,
        "input_hash": input_hash,
        "status": status,
        "result_count": result_count.max(0),
    });
    if let Some(kind) = validation_kind {
        details["validation_kind"] = serde_json::Value::String(kind.to_string());
    }
    let event = crate::context_ir::ContextUseEvent {
        schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
        event_id: crate::task_session::event_id_for(
            attribution.task_session_id,
            event_type,
            &format!("{operation}:{input_hash}:{}:{ordinal}", std::process::id()),
        ),
        task_session_id: attribution.task_session_id.to_string(),
        context_id: None,
        item_id: None,
        entity_id: Some(format!("query:{input_hash}")),
        event_type,
        observation_source: crate::context_ir::ObservationSource::AtlasObserved,
        occurred_at: crate::migrations::iso8601_now(),
        bytes: None,
        details,
    };
    crate::task_session::record_context_use_event(connection, attribution.task_session_id, &event)
}

pub fn record_validation_selection(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    attribution: QueryAttribution<'_>,
    kind: crate::context_ir::ValidationKind,
    target: &str,
    selected: bool,
) -> Result<()> {
    let kind = serde_json::to_value(kind)?
        .as_str()
        .expect("validation kind serializes as a string")
        .to_string();
    record_query_observation(
        connection,
        workspace,
        attribution,
        ObservationRecord {
            event_type: crate::context_ir::ContextUseEventType::ValidationSelected,
            operation: "validation_selection",
            input: &format!("{kind}:{target}"),
            status: if selected { "selected" } else { "rejected" },
            result_count: i64::from(selected),
            validation_kind: Some(&kind),
        },
    )
}

pub fn find_attributed(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    query: &str,
    limit: i64,
    attribution: QueryAttribution<'_>,
) -> Result<FindOutput> {
    let output = find(connection, workspace, query, limit)?;
    record_query_observation(
        connection,
        workspace,
        attribution,
        ObservationRecord {
            event_type: crate::context_ir::ContextUseEventType::EntityQueried,
            operation: "find",
            input: query,
            status: if output.results.is_empty() {
                "not_found"
            } else {
                "succeeded"
            },
            result_count: output.results.len() as i64,
            validation_kind: None,
        },
    )?;
    Ok(output)
}

pub fn inspect_attributed(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    path: &str,
    attribution: QueryAttribution<'_>,
) -> Result<InspectOutput> {
    let output = inspect(connection, workspace, path)?;
    record_query_observation(
        connection,
        workspace,
        attribution,
        ObservationRecord {
            event_type: crate::context_ir::ContextUseEventType::EntityQueried,
            operation: "inspect_path",
            input: path,
            status: if output.content_hash.is_some() {
                "succeeded"
            } else {
                "not_found"
            },
            result_count: output.symbols.len() as i64,
            validation_kind: None,
        },
    )?;
    Ok(output)
}

pub fn inspect_symbol_attributed(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    symbol: &str,
    attribution: QueryAttribution<'_>,
) -> Result<InspectOutput> {
    let output = inspect_symbol(connection, workspace, symbol)?;
    record_query_observation(
        connection,
        workspace,
        attribution,
        ObservationRecord {
            event_type: crate::context_ir::ContextUseEventType::EntityQueried,
            operation: "inspect_symbol",
            input: symbol,
            status: if output.content_hash.is_some() {
                "succeeded"
            } else {
                "not_found"
            },
            result_count: output.symbols.len() as i64,
            validation_kind: None,
        },
    )?;
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
pub fn trace_attributed(
    connection: &Connection,
    workspace: &WorkspaceRecord,
    seed_path: &str,
    direction: &str,
    max_depth: i64,
    max_records: i64,
    attribution: QueryAttribution<'_>,
) -> Result<TraceOutput> {
    let output = trace(
        connection,
        workspace,
        seed_path,
        direction,
        max_depth,
        max_records,
    )?;
    record_query_observation(
        connection,
        workspace,
        attribution,
        ObservationRecord {
            event_type: crate::context_ir::ContextUseEventType::RelationshipTraversed,
            operation: "trace",
            input: seed_path,
            status: if output.edges.is_empty() {
                "no_edges"
            } else {
                "succeeded"
            },
            result_count: output.edges.len() as i64,
            validation_kind: None,
        },
    )?;
    Ok(output)
}

/// Captured active generation for a query — every query binds to exactly one
/// generation (INV-011 "one active generation, one normalized request").
fn active_generation_id(conn: &Connection, workspace_id: &str) -> Result<Option<String>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            params![workspace_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(id)
}

// ---------------------------------------------------------------------------
// QUERY-001: shared semantic evidence envelope
// ---------------------------------------------------------------------------

/// Shared semantic evidence envelope. Populated only when the workspace has an
/// active generation and kept bounded and sorted for deterministic results.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceEnvelope {
    pub generation_id: String,
    /// Distinct provider keys that contributed any execution to this
    /// generation, sorted for determinism.
    pub provider_state: Vec<String>,
    pub coverage: Vec<CoverageRow>,
    /// Count of `evidence_conflict` rows with an unresolved disagreement
    /// (`status = 'open'`) for this generation.
    pub conflicts: i64,
    /// Count of `relationship_resolution` rows whose status is not a
    /// resolved/external terminal state for this generation.
    pub unresolved: i64,
}

fn workspace_evidence_envelope(conn: &Connection, gen_id: &str) -> Result<EvidenceEnvelope> {
    let mut stmt = conn.prepare(
        "SELECT provider_key FROM provider_execution WHERE generation_id = ?1
         UNION
         SELECT scope_key FROM coverage_record WHERE generation_id = ?1 AND scope_kind = 'provider'
         ORDER BY 1",
    )?;
    let provider_state: Vec<String> = stmt
        .query_map(params![gen_id], |r| r.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(stmt);

    let mut cov_stmt = conn.prepare(
        "SELECT scope_kind, scope_key, status FROM coverage_record WHERE generation_id = ?1 ORDER BY scope_kind, scope_key",
    )?;
    let coverage: Vec<CoverageRow> = cov_stmt
        .query_map(params![gen_id], |r| {
            Ok(CoverageRow {
                scope_kind: r.get(0)?,
                scope_key: r.get(1)?,
                status: r.get(2)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(cov_stmt);

    let conflicts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM evidence_conflict WHERE generation_id = ?1 AND status IN ('open', 'preferred_with_conflict')",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let unresolved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM relationship_resolution WHERE generation_id = ?1 AND status IN ('unresolved', 'ambiguous', 'invalid', 'stale')",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(EvidenceEnvelope {
        generation_id: gen_id.to_string(),
        provider_state,
        coverage,
        conflicts,
        unresolved,
    })
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FindResult {
    pub canonical_path: String,
    pub canonical_symbol_key: String,
    pub display_name: String,
    pub symbol_kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub content_hash: String,
    pub generation_id: String,
    pub provider_name: String,
    pub evidence_method: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindOutput {
    pub ok: bool,
    pub generation_id: Option<String>,
    pub query: String,
    pub results: Vec<FindResult>,
    pub total_matched: i64,
    pub truncated: bool,
    pub coverage_note: String,
    /// QUERY-002: opaque cursor to resume after the last returned result,
    /// generation- and query-bound (`QueryCursor`). `None` when the result
    /// set was exhausted within `limit`.
    pub next_cursor: Option<String>,
    /// QUERY-001: shared semantic evidence envelope. `None` when the
    /// workspace has no active generation.
    pub evidence: Option<EvidenceEnvelope>,
}

/// QUERY-002: additive `find` filters. Every field is optional and
/// preserves V1 `find` behavior when left at its default.
#[derive(Debug, Clone, Default)]
pub struct FindFilter<'a> {
    pub path_prefix: Option<&'a str>,
    pub min_confidence: Option<f64>,
    /// Opaque `QueryCursor` token from a previous `FindOutput::next_cursor`.
    pub cursor: Option<&'a str>,
}

const FIND_ORDERING_VERSION: &str = "find-v1.1.0";

#[derive(Debug)]
pub(crate) struct BoundedFtsSymbolMatch {
    pub canonical_symbol_key: String,
    pub canonical_path: String,
    pub symbol_kind: String,
    pub fact_id: String,
    pub provider_tier: String,
    pub confidence: f64,
    pub evidence_state: String,
    pub start_byte: i64,
    pub end_byte: i64,
    pub artifact_class: String,
    pub is_test: bool,
    pub content_hash: String,
}

#[derive(Debug)]
pub(crate) struct BoundedFtsSymbolMatches {
    pub matches: Vec<BoundedFtsSymbolMatch>,
    pub sentinel: Option<BoundedFtsSymbolMatch>,
    /// Rows returned by this bounded SQL statement, including the optional
    /// sentinel and duplicate evidence rows. This is not a SQLite VM-step or
    /// wall-time measurement.
    pub rows_fetched: u64,
}

/// Query only the existing FTS5 accelerator, retaining at most
/// `max_retained_candidates` distinct symbol identities and returning at most
/// `max_examined_rows` physical result rows. Duplicate rows are preserved for
/// caller accounting. The first distinct row beyond the candidate limit, or
/// the final paid row at the physical bound, becomes the sole sentinel.
pub(crate) fn bounded_symbol_fts_matches(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    query: &str,
    max_retained_candidates: u64,
    max_examined_rows: u64,
) -> Result<BoundedFtsSymbolMatches> {
    if max_examined_rows == 0 {
        return Err(AtlasError::InvalidConfig(
            "FTS examination limit must be > 0".into(),
        ));
    }
    if query.trim().is_empty() {
        return Ok(BoundedFtsSymbolMatches {
            matches: Vec::new(),
            sentinel: None,
            rows_fetched: 0,
        });
    }
    let sql_limit = i64::try_from(max_examined_rows)
        .map_err(|_| AtlasError::InvalidConfig("FTS examination limit is too large".into()))?;
    let match_query = format!("\"{}\"", query.trim().replace('"', "\"\""));
    let mut statement = conn.prepare(
        "SELECT sf.canonical_symbol_key, cf.canonical_path, sf.symbol_kind,
                sf.symbol_fact_id, er.provider_tier, sf.confidence,
                sf.evidence_method, sf.start_byte, sf.end_byte,
                cf.artifact_class, cf.is_test, cf.content_hash
         FROM atlas_fts
         JOIN symbol_fact sf ON sf.symbol_fact_id = atlas_fts.record_id
         JOIN current_file cf ON cf.revision_id = sf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE atlas_fts MATCH ?1
           AND atlas_fts.record_kind = 'symbol'
           AND atlas_fts.workspace_id = ?2
           AND cf.generation_id = ?3
         ORDER BY sf.canonical_symbol_key,
                  (sf.evidence_method = 'semantic') DESC,
                  sf.confidence DESC, sf.symbol_fact_id, atlas_fts.rowid
         LIMIT ?4",
    )?;
    let mut rows = statement.query(params![
        match_query,
        ws.workspace_id,
        generation_id,
        sql_limit
    ])?;
    let mut matches = Vec::new();
    let mut retained_entities = std::collections::BTreeSet::new();
    let mut sentinel = None;
    let mut rows_fetched = 0_u64;
    while rows_fetched < max_examined_rows {
        let Some(row) = rows.next()? else {
            break;
        };
        rows_fetched += 1;
        let candidate = BoundedFtsSymbolMatch {
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
        };
        let new_entity = !retained_entities.contains(&candidate.canonical_symbol_key);
        if new_entity
            && u64::try_from(retained_entities.len())
                .is_ok_and(|count| count >= max_retained_candidates)
        {
            sentinel = Some(candidate);
            break;
        }
        retained_entities.insert(candidate.canonical_symbol_key.clone());
        matches.push(candidate);
    }
    if sentinel.is_none() && rows_fetched == max_examined_rows {
        sentinel = matches.pop();
    }
    Ok(BoundedFtsSymbolMatches {
        matches,
        sentinel,
        rows_fetched,
    })
}

/// `atlas find <text>` searches current committed symbol display names for a
/// case-insensitive substring match; it never mixes historical generations
/// into the default result.
pub fn find(
    conn: &Connection,
    ws: &WorkspaceRecord,
    query: &str,
    limit: i64,
) -> Result<FindOutput> {
    find_filtered(conn, ws, query, limit, &FindFilter::default())
}

/// QUERY-002: `find` with optional path-prefix/confidence filters and a
/// deterministic, generation-bound resume cursor. A cursor minted for one
/// generation is silently ignored (treated as absent, i.e. restart) if it
/// does not validate against the current query/generation — it is never
/// honored against different truth.
pub fn find_filtered(
    conn: &Connection,
    ws: &WorkspaceRecord,
    query: &str,
    limit: i64,
    filter: &FindFilter<'_>,
) -> Result<FindOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(FindOutput {
            ok: true,
            generation_id: None,
            query: query.to_string(),
            results: Vec::new(),
            total_matched: 0,
            truncated: false,
            coverage_note: "no active generation; workspace has not been reconciled yet".into(),
            next_cursor: None,
            evidence: None,
        });
    };

    let query_hash = crate::hashing::content_hash_of_bytes(
        format!(
            "{}\u{1f}{:?}\u{1f}{:?}",
            query.to_ascii_lowercase(),
            filter.path_prefix,
            filter.min_confidence
        )
        .as_bytes(),
    );
    let resume_after: Option<String> = filter.cursor.and_then(|tok| {
        QueryCursor::decode(tok)
            .filter(|c| {
                c.is_valid_for(
                    &ws.workspace_id,
                    &gen_id,
                    &query_hash,
                    FIND_ORDERING_VERSION,
                )
            })
            .map(|c| c.last_sort_key)
    });

    let like = format!("%{}%", query.to_ascii_lowercase());
    let path_prefix_like = filter.path_prefix.map(|p| format!("{p}%"));
    let min_confidence = filter.min_confidence.unwrap_or(0.0);
    let resume_key = resume_after.clone().unwrap_or_default();
    let mut stmt = conn.prepare(
        "SELECT cf.canonical_path, sf.canonical_symbol_key, sf.display_name, sf.symbol_kind,
                sf.start_line, sf.end_line, fr.content_hash, er.provider_name,
                sf.evidence_method, sf.confidence
         FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         JOIN file_revision fr ON fr.revision_id = cf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE cf.generation_id = ?1 AND LOWER(sf.display_name) LIKE ?2
           AND (?3 IS NULL OR cf.canonical_path LIKE ?3)
           AND sf.confidence >= ?4
           AND (?5 = '' OR cf.canonical_path > ?5)
         ORDER BY cf.canonical_path, sf.start_byte
         LIMIT ?6",
    )?;
    let rows = stmt.query_map(
        params![
            gen_id,
            like,
            path_prefix_like,
            min_confidence,
            resume_key,
            limit + 1
        ],
        |r| {
            Ok(FindResult {
                canonical_path: r.get(0)?,
                canonical_symbol_key: r.get(1)?,
                display_name: r.get(2)?,
                symbol_kind: r.get(3)?,
                start_line: r.get(4)?,
                end_line: r.get(5)?,
                content_hash: r.get(6)?,
                generation_id: gen_id.clone(),
                provider_name: r.get(7)?,
                evidence_method: r.get(8)?,
                confidence: r.get(9)?,
            })
        },
    )?;
    let mut results: Vec<FindResult> = rows.filter_map(std::result::Result::ok).collect();
    let truncated = results.len() as i64 > limit;
    if truncated {
        results.truncate(limit as usize);
    }
    let total_matched = results.len() as i64;

    let next_cursor = if truncated {
        results.last().map(|r| {
            QueryCursor {
                workspace_id: ws.workspace_id.clone(),
                generation_id: gen_id.clone(),
                query_hash: query_hash.clone(),
                ordering_version: FIND_ORDERING_VERSION.to_string(),
                last_sort_key: r.canonical_path.clone(),
            }
            .encode()
        })
    } else {
        None
    };

    let coverage_note = if results.is_empty() {
        format!(
            "No matching reference was found in active indexed coverage for generation {gen_id}."
        )
    } else {
        format!("{total_matched} match(es) in active indexed coverage for generation {gen_id}.")
    };

    let evidence = Some(workspace_evidence_envelope(conn, &gen_id)?);

    Ok(FindOutput {
        ok: true,
        generation_id: Some(gen_id),
        query: query.to_string(),
        results,
        total_matched,
        truncated,
        coverage_note,
        next_cursor,
        evidence,
    })
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InspectSymbol {
    pub canonical_symbol_key: String,
    pub display_name: String,
    pub symbol_kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub evidence_method: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectRelationship {
    pub relationship_type: String,
    pub direction: String, // "outbound" | "inbound"
    pub other_ref_kind: String,
    pub other_ref_value: String,
    pub evidence_method: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectOutput {
    pub ok: bool,
    pub generation_id: Option<String>,
    pub canonical_path: String,
    pub content_hash: Option<String>,
    pub artifact_class: Option<String>,
    pub symbols: Vec<InspectSymbol>,
    pub relationships: Vec<InspectRelationship>,
    pub diagnostics_count: i64,
    pub coverage_note: String,
    /// QUERY-001: shared semantic evidence envelope.
    pub evidence: Option<EvidenceEnvelope>,
}

/// `atlas inspect <path>`: current identity, symbols ordered by span, inbound
/// + outbound relationships, diagnostics count.
pub fn inspect(conn: &Connection, ws: &WorkspaceRecord, rel_path: &str) -> Result<InspectOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(InspectOutput {
            ok: true,
            generation_id: None,
            canonical_path: rel_path.to_string(),
            content_hash: None,
            artifact_class: None,
            symbols: Vec::new(),
            relationships: Vec::new(),
            diagnostics_count: 0,
            coverage_note: "no active generation; workspace has not been reconciled yet".into(),
            evidence: None,
        });
    };

    let file_row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT revision_id, content_hash, artifact_class
             FROM current_file WHERE generation_id = ?1 AND canonical_path = ?2",
            params![gen_id, rel_path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    let Some((revision_id, content_hash, artifact_class)) = file_row else {
        return Ok(InspectOutput {
            ok: true,
            generation_id: Some(gen_id.clone()),
            canonical_path: rel_path.to_string(),
            content_hash: None,
            artifact_class: None,
            symbols: Vec::new(),
            relationships: Vec::new(),
            diagnostics_count: 0,
            coverage_note: format!(
                "No matching reference was found in active indexed coverage for generation {gen_id}; path not present or excluded."
            ),
            evidence: Some(workspace_evidence_envelope(conn, &gen_id)?),
        });
    };

    let mut sym_stmt = conn.prepare(
        "SELECT canonical_symbol_key, display_name, symbol_kind, start_line, end_line,
                evidence_method, confidence
         FROM symbol_fact WHERE revision_id = ?1 ORDER BY start_byte",
    )?;
    let symbols: Vec<InspectSymbol> = sym_stmt
        .query_map(params![revision_id], |r| {
            Ok(InspectSymbol {
                canonical_symbol_key: r.get(0)?,
                display_name: r.get(1)?,
                symbol_kind: r.get(2)?,
                start_line: r.get(3)?,
                end_line: r.get(4)?,
                evidence_method: r.get(5)?,
                confidence: r.get(6)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut out_stmt = conn.prepare(
        "SELECT relationship_type, target_ref_kind, target_ref_value, evidence_method, confidence
         FROM relationship_fact WHERE revision_id = ?1 AND source_ref_kind = 'path' AND source_ref_value = ?2",
    )?;
    let mut relationships: Vec<InspectRelationship> = out_stmt
        .query_map(params![revision_id, rel_path], |r| {
            Ok(InspectRelationship {
                relationship_type: r.get(0)?,
                direction: "outbound".into(),
                other_ref_kind: r.get(1)?,
                other_ref_value: r.get(2)?,
                evidence_method: r.get(3)?,
                confidence: r.get(4)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut in_stmt = conn.prepare(
        "SELECT relationship_type, source_ref_kind, source_ref_value, evidence_method, confidence
         FROM relationship_fact WHERE target_ref_kind = 'path' AND target_ref_value = ?1",
    )?;
    let inbound: Vec<InspectRelationship> = in_stmt
        .query_map(params![rel_path], |r| {
            Ok(InspectRelationship {
                relationship_type: r.get(0)?,
                direction: "inbound".into(),
                other_ref_kind: r.get(1)?,
                other_ref_value: r.get(2)?,
                evidence_method: r.get(3)?,
                confidence: r.get(4)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    relationships.extend(inbound);

    let diagnostics_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM diagnostic d
             JOIN extractor_run er ON er.extractor_run_id = d.extractor_run_id
             WHERE er.revision_id = ?1",
            params![revision_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(InspectOutput {
        ok: true,
        generation_id: Some(gen_id.clone()),
        canonical_path: rel_path.to_string(),
        content_hash: Some(content_hash),
        artifact_class: Some(artifact_class),
        symbols,
        relationships,
        diagnostics_count,
        coverage_note: format!("resolved in active indexed coverage for generation {gen_id}"),
        evidence: Some(workspace_evidence_envelope(conn, &gen_id)?),
    })
}

/// QUERY-003: `atlas inspect --symbol <canonical_symbol_key>` — the same
/// identity/relationships/diagnostics shape as path-based `inspect`, keyed
/// by canonical symbol instead of canonical path. Multiple symbols may
/// legitimately share a display name across files; only the exact
/// canonical key is ever matched (never a display-name lookup).
pub fn inspect_symbol(
    conn: &Connection,
    ws: &WorkspaceRecord,
    canonical_symbol_key: &str,
) -> Result<InspectOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(InspectOutput {
            ok: true,
            generation_id: None,
            canonical_path: String::new(),
            content_hash: None,
            artifact_class: None,
            symbols: Vec::new(),
            relationships: Vec::new(),
            diagnostics_count: 0,
            coverage_note: "no active generation; workspace has not been reconciled yet".into(),
            evidence: None,
        });
    };

    let owner: Option<String> = conn
        .query_row(
            "SELECT cf.canonical_path FROM symbol_fact sf
             JOIN current_file cf ON cf.revision_id = sf.revision_id
             WHERE cf.generation_id = ?1 AND sf.canonical_symbol_key = ?2
             LIMIT 1",
            params![gen_id, canonical_symbol_key],
            |r| r.get(0),
        )
        .optional()?;

    let Some(canonical_path) = owner else {
        return Ok(InspectOutput {
            ok: true,
            generation_id: Some(gen_id.clone()),
            canonical_path: String::new(),
            content_hash: None,
            artifact_class: None,
            symbols: Vec::new(),
            relationships: Vec::new(),
            diagnostics_count: 0,
            coverage_note: format!(
                "No matching reference was found in active indexed coverage for generation {gen_id}; canonical symbol not present."
            ),
            evidence: Some(workspace_evidence_envelope(conn, &gen_id)?),
        });
    };

    inspect(conn, ws, &canonical_path)
}

// ---------------------------------------------------------------------------
// trace
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TraceEdge {
    pub depth: i64,
    pub relationship_type: String,
    pub from_ref: String,
    pub to_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceOutput {
    pub ok: bool,
    pub generation_id: Option<String>,
    pub seed: String,
    pub direction: String,
    pub max_depth: i64,
    pub edges: Vec<TraceEdge>,
    pub visited_count: i64,
    pub truncated_by_depth: bool,
    pub truncated_by_records: bool,
}

/// `atlas trace <path>`: bounded relationship traversal, cycle-safe via a
/// visited-set keyed by canonical ref value.
pub fn trace(
    conn: &Connection,
    ws: &WorkspaceRecord,
    seed_path: &str,
    direction: &str, // "outbound" | "inbound" | "both"
    max_depth: i64,
    max_records: i64,
) -> Result<TraceOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(TraceOutput {
            ok: true,
            generation_id: None,
            seed: seed_path.to_string(),
            direction: direction.to_string(),
            max_depth,
            edges: Vec::new(),
            visited_count: 0,
            truncated_by_depth: false,
            truncated_by_records: false,
        });
    };

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(seed_path.to_string());
    let mut queue: VecDeque<(String, i64)> = VecDeque::new();
    queue.push_back((seed_path.to_string(), 0));
    let mut edges = Vec::new();
    let mut truncated_by_depth = false;
    let mut truncated_by_records = false;

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            // Only mark truncated if there would have been more to explore.
            continue;
        }
        if edges.len() as i64 >= max_records {
            truncated_by_records = true;
            break;
        }

        if direction == "outbound" || direction == "both" {
            let mut stmt = conn.prepare(
                "SELECT rf.relationship_type, rf.target_ref_value
                 FROM relationship_fact rf
                 JOIN file_revision fr ON fr.revision_id = rf.revision_id
                 JOIN current_file cf ON cf.revision_id = fr.revision_id
                 WHERE cf.generation_id = ?1 AND rf.source_ref_kind = 'path' AND rf.source_ref_value = ?2",
            )?;
            let rows = stmt.query_map(params![gen_id, current], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (rel_type, target) = row?;
                if edges.len() as i64 >= max_records {
                    truncated_by_records = true;
                    break;
                }
                edges.push(TraceEdge {
                    depth: depth + 1,
                    relationship_type: rel_type,
                    from_ref: current.clone(),
                    to_ref: target.clone(),
                });
                if visited.insert(target.clone()) {
                    if depth + 1 >= max_depth {
                        truncated_by_depth = true;
                    } else {
                        queue.push_back((target, depth + 1));
                    }
                }
            }
        }

        if direction == "inbound" || direction == "both" {
            let mut stmt = conn.prepare(
                "SELECT rf.relationship_type, rf.source_ref_value
                 FROM relationship_fact rf
                 JOIN file_revision fr ON fr.revision_id = rf.revision_id
                 JOIN current_file cf ON cf.revision_id = fr.revision_id
                 WHERE cf.generation_id = ?1 AND rf.target_ref_kind = 'path' AND rf.target_ref_value = ?2",
            )?;
            let rows = stmt.query_map(params![gen_id, current], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (rel_type, source) = row?;
                if edges.len() as i64 >= max_records {
                    truncated_by_records = true;
                    break;
                }
                edges.push(TraceEdge {
                    depth: depth + 1,
                    relationship_type: rel_type,
                    from_ref: source.clone(),
                    to_ref: current.clone(),
                });
                if visited.insert(source.clone()) {
                    if depth + 1 >= max_depth {
                        truncated_by_depth = true;
                    } else {
                        queue.push_back((source, depth + 1));
                    }
                }
            }
        }
    }

    Ok(TraceOutput {
        ok: true,
        generation_id: Some(gen_id),
        seed: seed_path.to_string(),
        direction: direction.to_string(),
        max_depth,
        visited_count: visited.len() as i64,
        edges,
        truncated_by_depth,
        truncated_by_records,
    })
}

/// QUERY-004: `atlas trace --resolved <path>` — the same bounded,
/// cycle-safe BFS as `trace`, but only follows edges with a *resolved*
/// relationship-resolution outcome (`resolved_symbol`/`resolved_file`),
/// reporting the canonicalized `resolved_ref_value` rather than the raw
/// provider target string. Ambiguous/unresolved/external/stale/invalid
/// edges are never silently traversed as if verified.
pub fn trace_resolved(
    conn: &Connection,
    ws: &WorkspaceRecord,
    seed_path: &str,
    direction: &str,
    max_depth: i64,
    max_records: i64,
) -> Result<TraceOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(TraceOutput {
            ok: true,
            generation_id: None,
            seed: seed_path.to_string(),
            direction: direction.to_string(),
            max_depth,
            edges: Vec::new(),
            visited_count: 0,
            truncated_by_depth: false,
            truncated_by_records: false,
        });
    };

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(seed_path.to_string());
    let mut queue: VecDeque<(String, i64)> = VecDeque::new();
    queue.push_back((seed_path.to_string(), 0));
    let mut edges = Vec::new();
    let mut truncated_by_depth = false;
    let mut truncated_by_records = false;

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if edges.len() as i64 >= max_records {
            truncated_by_records = true;
            break;
        }

        if direction == "outbound" || direction == "both" {
            let mut stmt = conn.prepare(
                "SELECT rf.relationship_type, rr.resolved_ref_value
                 FROM relationship_fact rf
                 JOIN file_revision fr ON fr.revision_id = rf.revision_id
                 JOIN current_file cf ON cf.revision_id = fr.revision_id
                 JOIN relationship_resolution rr ON rr.relationship_fact_id = rf.relationship_fact_id AND rr.generation_id = ?1
                 WHERE cf.generation_id = ?1 AND rf.source_ref_kind = 'path' AND rf.source_ref_value = ?2
                   AND rr.status IN ('resolved_symbol', 'resolved_file') AND rr.resolved_ref_value IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![gen_id, current], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (rel_type, target) = row?;
                if edges.len() as i64 >= max_records {
                    truncated_by_records = true;
                    break;
                }
                edges.push(TraceEdge {
                    depth: depth + 1,
                    relationship_type: rel_type,
                    from_ref: current.clone(),
                    to_ref: target.clone(),
                });
                if visited.insert(target.clone()) {
                    if depth + 1 >= max_depth {
                        truncated_by_depth = true;
                    } else {
                        queue.push_back((target, depth + 1));
                    }
                }
            }
        }

        if direction == "inbound" || direction == "both" {
            let mut stmt = conn.prepare(
                "SELECT rf.relationship_type, rf.source_ref_value
                 FROM relationship_fact rf
                 JOIN relationship_resolution rr ON rr.relationship_fact_id = rf.relationship_fact_id AND rr.generation_id = ?1
                 WHERE rr.status IN ('resolved_symbol', 'resolved_file') AND rr.resolved_ref_value = ?2",
            )?;
            let rows = stmt.query_map(params![gen_id, current], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (rel_type, source) = row?;
                if edges.len() as i64 >= max_records {
                    truncated_by_records = true;
                    break;
                }
                edges.push(TraceEdge {
                    depth: depth + 1,
                    relationship_type: rel_type,
                    from_ref: source.clone(),
                    to_ref: current.clone(),
                });
                if visited.insert(source.clone()) {
                    if depth + 1 >= max_depth {
                        truncated_by_depth = true;
                    } else {
                        queue.push_back((source, depth + 1));
                    }
                }
            }
        }
    }

    Ok(TraceOutput {
        ok: true,
        generation_id: Some(gen_id),
        seed: seed_path.to_string(),
        direction: direction.to_string(),
        max_depth,
        visited_count: visited.len() as i64,
        edges,
        truncated_by_depth,
        truncated_by_records,
    })
}

// ---------------------------------------------------------------------------
// impact
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ImpactOutput {
    pub ok: bool,
    pub generation_id: Option<String>,
    pub seed_paths: Vec<String>,
    pub direct_dependents: Vec<String>,
    pub tests: Vec<String>,
    pub unresolved: Vec<String>,
    /// QUERY-005: resolved (`resolved_symbol`/`resolved_file`) inbound
    /// dependents — the frontier a caller can act on with confidence.
    pub verified_dependents: Vec<String>,
    /// QUERY-005: ambiguous/unresolved/external/stale/invalid candidate
    /// dependents that *may* be affected — never merged into `verified_dependents`.
    pub uncertain_dependents: Vec<String>,
}

/// `atlas impact <path>...`: bounded change frontier — direct inbound
/// dependents (files that import/reference the seed) plus tests among them.
pub fn impact(
    conn: &Connection,
    ws: &WorkspaceRecord,
    seed_paths: &[String],
) -> Result<ImpactOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(ImpactOutput {
            ok: true,
            generation_id: None,
            seed_paths: seed_paths.to_vec(),
            direct_dependents: Vec::new(),
            tests: Vec::new(),
            unresolved: Vec::new(),
            verified_dependents: Vec::new(),
            uncertain_dependents: Vec::new(),
        });
    };

    let mut dependents: HashSet<String> = HashSet::new();
    let mut resolution_outcomes: Vec<(String, crate::resolution::ResolutionOutcome)> = Vec::new();
    for seed in seed_paths {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT cf.canonical_path
             FROM relationship_fact rf
             JOIN file_revision fr ON fr.revision_id = rf.revision_id
             JOIN current_file cf ON cf.revision_id = fr.revision_id
             WHERE cf.generation_id = ?1 AND rf.target_ref_kind IN ('path', 'external')
               AND (rf.target_ref_value = ?2 OR rf.target_ref_value LIKE ?3)",
        )?;
        let like = format!("%{}", seed.trim_start_matches("./"));
        let rows = stmt.query_map(params![gen_id, seed, like.clone()], |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            dependents.insert(row?);
        }

        // QUERY-005: the same seed's *resolved* semantic evidence, split
        // into verified vs. uncertain via `resolution::split_impact_frontier`.
        let mut res_stmt = conn.prepare(
            "SELECT rf.source_ref_value, rr.status
             FROM relationship_resolution rr
             JOIN relationship_fact rf ON rf.relationship_fact_id = rr.relationship_fact_id
             WHERE rr.generation_id = ?1 AND rf.source_ref_kind = 'path'
               AND (rf.target_ref_value = ?2 OR rf.target_ref_value LIKE ?3)",
        )?;
        let res_rows = res_stmt.query_map(params![gen_id, seed, like], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in res_rows {
            let (source_path, status_str) = row?;
            let Some(status) = crate::resolution::ResolutionStatus::from_db_str(&status_str) else {
                continue;
            };
            resolution_outcomes.push((
                source_path,
                crate::resolution::ResolutionOutcome {
                    status,
                    resolved: None,
                    reason_code: "persisted",
                    confidence: 0.0,
                    candidates: vec![],
                },
            ));
        }
    }
    // Never report the seed as its own dependent.
    for seed in seed_paths {
        dependents.remove(seed);
    }

    let tests: Vec<String> = dependents
        .iter()
        .filter(|p| p.contains("test") || p.contains("spec"))
        .cloned()
        .collect();

    let mut direct_dependents: Vec<String> = dependents.into_iter().collect();
    direct_dependents.sort();
    let mut tests_sorted = tests;
    tests_sorted.sort();

    let frontier = crate::resolution::split_impact_frontier(&resolution_outcomes);
    let mut verified_dependents: Vec<String> = frontier
        .verified
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    verified_dependents.sort();
    let mut uncertain_dependents: Vec<String> = frontier
        .uncertain
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    uncertain_dependents.sort();

    Ok(ImpactOutput {
        ok: true,
        generation_id: Some(gen_id),
        seed_paths: seed_paths.to_vec(),
        direct_dependents,
        tests: tests_sorted,
        unresolved: Vec::new(),
        verified_dependents,
        uncertain_dependents,
    })
}

// ---------------------------------------------------------------------------
// source
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SourceOutput {
    pub ok: bool,
    pub canonical_path: String,
    pub indexed_content_hash: Option<String>,
    pub observed_content_hash: Option<String>,
    pub status: String, // "verified" | "hash_mismatch" | "not_found" | "read_failed"
    pub excerpt: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ExactSourceVerification {
    pub indexed_hash: Option<String>,
    pub observed_hash: Option<String>,
    pub status: &'static str,
    pub confinement: crate::context_ir::IrPathConfinementV2,
    pub materialized_bytes: Option<Vec<u8>>,
    pub range_valid: bool,
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (left.metadata(), right.metadata()) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(windows)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;

    #[allow(non_snake_case)]
    #[repr(C)]
    struct FileTime {
        dwLowDateTime: u32,
        dwHighDateTime: u32,
    }
    #[allow(non_snake_case)]
    #[repr(C)]
    struct ByHandleFileInformation {
        dwFileAttributes: u32,
        ftCreationTime: FileTime,
        ftLastAccessTime: FileTime,
        ftLastWriteTime: FileTime,
        dwVolumeSerialNumber: u32,
        nFileSizeHigh: u32,
        nFileSizeLow: u32,
        nNumberOfLinks: u32,
        nFileIndexHigh: u32,
        nFileIndexLow: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }
    fn identity(file: &std::fs::File) -> Option<(u32, u64)> {
        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: `information` points to writable storage of the exact Win32
        // structure shape, and the borrowed file keeps its handle valid.
        let succeeded = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
        };
        if succeeded == 0 {
            return None;
        }
        // SAFETY: a successful `GetFileInformationByHandle` initializes every
        // field of `ByHandleFileInformation`.
        let information = unsafe { information.assume_init() };
        Some((
            information.dwVolumeSerialNumber,
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        ))
    }

    identity(left)
        .zip(identity(right))
        .is_some_and(|(left, right)| left == right)
}

#[cfg(unix)]
fn source_mutation_signature(metadata: &std::fs::Metadata) -> (u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(windows)]
fn source_mutation_signature(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::windows::fs::MetadataExt;
    (metadata.len(), metadata.last_write_time())
}
/// Canonical live source verifier shared by `atlas source` and deep compilation.
///
/// Hashing is over exact file bytes. When requested, only the authorized byte
/// range is retained; verification-only calls never retain a source body.
pub(crate) fn verify_exact_source_at_generation(
    conn: &Connection,
    ws: &WorkspaceRecord,
    generation_id: &str,
    rel_path: &str,
    byte_range: Option<(u64, u64)>,
    materialize: bool,
) -> Result<ExactSourceVerification> {
    use sha2::Digest;
    use std::io::Read;

    let indexed_hash: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM current_file WHERE generation_id = ?1 AND canonical_path = ?2",
            params![generation_id, rel_path],
            |row| row.get(0),
        )
        .optional()?;
    let Some(indexed_hash) = indexed_hash else {
        return Ok(ExactSourceVerification {
            indexed_hash: None,
            observed_hash: None,
            status: "not_found",
            confinement: crate::context_ir::IrPathConfinementV2::Unavailable,
            materialized_bytes: None,
            range_valid: true,
        });
    };

    let root = match std::fs::canonicalize(&ws.canonical_root) {
        Ok(root) => root,
        Err(_) => {
            return Ok(ExactSourceVerification {
                indexed_hash: Some(indexed_hash),
                observed_hash: None,
                status: "read_failed",
                confinement: crate::context_ir::IrPathConfinementV2::Unavailable,
                materialized_bytes: None,
                range_valid: true,
            })
        }
    };
    let joined = root.join(rel_path);
    let absolute = match std::fs::canonicalize(&joined) {
        Ok(absolute) => absolute,
        Err(_) => {
            return Ok(ExactSourceVerification {
                indexed_hash: Some(indexed_hash),
                observed_hash: None,
                status: "read_failed",
                confinement: crate::context_ir::IrPathConfinementV2::Unavailable,
                materialized_bytes: None,
                range_valid: true,
            })
        }
    };
    if crate::paths::confine_to_root(&root, &absolute).is_err() {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: None,
            status: "read_failed",
            confinement: crate::context_ir::IrPathConfinementV2::SymlinkEscape,
            materialized_bytes: None,
            range_valid: true,
        });
    }

    let mut file = match std::fs::File::open(&absolute) {
        Ok(file) => file,
        Err(_) => {
            return Ok(ExactSourceVerification {
                indexed_hash: Some(indexed_hash),
                observed_hash: None,
                status: "read_failed",
                confinement: crate::context_ir::IrPathConfinementV2::Confined,
                materialized_bytes: None,
                range_valid: true,
            })
        }
    };
    let opened_metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok(ExactSourceVerification {
                indexed_hash: Some(indexed_hash),
                observed_hash: None,
                status: "read_failed",
                confinement: crate::context_ir::IrPathConfinementV2::Confined,
                materialized_bytes: None,
                range_valid: true,
            })
        }
    };
    let file_len = opened_metadata.len();
    if byte_range.is_some_and(|(start, end)| start >= end) {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: None,
            status: "read_failed",
            confinement: crate::context_ir::IrPathConfinementV2::Confined,
            materialized_bytes: None,
            range_valid: false,
        });
    }

    let materialized_capacity = if materialize {
        match byte_range {
            Some((start, end)) => usize::try_from(end - start).ok(),
            None => usize::try_from(file_len).ok(),
        }
    } else {
        Some(0)
    };
    let Some(materialized_capacity) = materialized_capacity else {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: None,
            status: "read_failed",
            confinement: crate::context_ir::IrPathConfinementV2::Confined,
            materialized_bytes: None,
            range_valid: true,
        });
    };
    let mut materialized_bytes = materialize.then(|| Vec::with_capacity(materialized_capacity));
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => {
                return Ok(ExactSourceVerification {
                    indexed_hash: Some(indexed_hash),
                    observed_hash: None,
                    status: "read_failed",
                    confinement: crate::context_ir::IrPathConfinementV2::Confined,
                    materialized_bytes: None,
                    range_valid: true,
                })
            }
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        if let Some(bytes) = materialized_bytes.as_mut() {
            if let Some((start, end)) = byte_range {
                let chunk_end = offset + read as u64;
                let copy_start = start.max(offset);
                let copy_end = end.min(chunk_end);
                if copy_start < copy_end {
                    bytes.extend_from_slice(
                        &buffer[(copy_start - offset) as usize..(copy_end - offset) as usize],
                    );
                }
            } else {
                bytes.extend_from_slice(&buffer[..read]);
            }
        }
        offset += read as u64;
    }
    let observed_hash = hex::encode(hasher.finalize());
    let range_valid = byte_range.is_none_or(|(start, end)| {
        let expected_len = end - start;
        end <= offset
            && materialized_bytes
                .as_ref()
                .is_none_or(|bytes| bytes.len() as u64 == expected_len)
    });
    let opened_source_unchanged = file.metadata().ok().is_some_and(|metadata| {
        metadata.len() == offset
            && source_mutation_signature(&metadata) == source_mutation_signature(&opened_metadata)
    });
    if !opened_source_unchanged {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: Some(observed_hash),
            status: "read_failed",
            confinement: crate::context_ir::IrPathConfinementV2::Confined,
            materialized_bytes: None,
            range_valid,
        });
    }
    let (opened_path_is_current, post_confinement) = match std::fs::canonicalize(&joined) {
        Err(_) => (false, crate::context_ir::IrPathConfinementV2::Unavailable),
        Ok(post_absolute) if crate::paths::confine_to_root(&root, &post_absolute).is_err() => {
            (false, crate::context_ir::IrPathConfinementV2::SymlinkEscape)
        }
        Ok(post_absolute) => {
            let same_opened_file = post_absolute == absolute
                && std::fs::File::open(&post_absolute)
                    .ok()
                    .is_some_and(|post_file| same_file_identity(&file, &post_file));
            (
                same_opened_file,
                crate::context_ir::IrPathConfinementV2::Confined,
            )
        }
    };
    if !opened_path_is_current {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: Some(observed_hash),
            status: "read_failed",
            confinement: post_confinement,
            materialized_bytes: None,
            range_valid,
        });
    }
    if !range_valid {
        return Ok(ExactSourceVerification {
            indexed_hash: Some(indexed_hash),
            observed_hash: Some(observed_hash),
            status: "read_failed",
            confinement: crate::context_ir::IrPathConfinementV2::Confined,
            materialized_bytes: None,
            range_valid: false,
        });
    }
    let status = if observed_hash == indexed_hash {
        "verified"
    } else {
        materialized_bytes = None;
        "hash_mismatch"
    };
    Ok(ExactSourceVerification {
        indexed_hash: Some(indexed_hash),
        observed_hash: Some(observed_hash),
        status,
        confinement: crate::context_ir::IrPathConfinementV2::Confined,
        materialized_bytes,
        range_valid: true,
    })
}

/// `atlas source <path>`: resolve against the captured active generation,
/// verify the current on-disk hash matches the indexed revision hash before
/// returning exact source (INV-001 / INV-012).
pub fn source(conn: &Connection, ws: &WorkspaceRecord, rel_path: &str) -> Result<SourceOutput> {
    source_unobserved(conn, ws, rel_path)
}

fn source_unobserved(
    conn: &Connection,
    ws: &WorkspaceRecord,
    rel_path: &str,
) -> Result<SourceOutput> {
    let Some(generation_id) = active_generation_id(conn, &ws.workspace_id)? else {
        return Ok(SourceOutput {
            ok: false,
            canonical_path: rel_path.to_string(),
            indexed_content_hash: None,
            observed_content_hash: None,
            status: "not_found".into(),
            excerpt: None,
        });
    };
    let verification =
        verify_exact_source_at_generation(conn, ws, &generation_id, rel_path, None, true)?;
    let excerpt = verification
        .materialized_bytes
        .and_then(|bytes| String::from_utf8(bytes).ok());
    Ok(SourceOutput {
        ok: verification.status == "verified",
        canonical_path: rel_path.to_string(),
        indexed_content_hash: verification.indexed_hash,
        observed_content_hash: verification.observed_hash,
        status: verification.status.into(),
        excerpt,
    })
}

/// Task-session-associated exact source. A completed typed result is returned
/// only after its Atlas-observed event has committed successfully.
pub fn source_for_task_session(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    rel_path: &str,
    task_session_id: &str,
) -> Result<SourceOutput> {
    let output = source_unobserved(conn, ws, rel_path)?;
    let bytes = excerpt_byte_count(&output)?;
    crate::task_session::record_exact_source_request(
        conn,
        ws,
        task_session_id,
        &output.canonical_path,
        crate::task_session::ExactSourceRequestKind::Source,
        &output.status,
        bytes,
    )?;
    Ok(output)
}

/// QUERY-006: `atlas source <path> --lines <start>-<end>` — the same
/// hash-verification chain as `source`, but returns only the requested
/// 1-based inclusive line range instead of the whole file. An out-of-bounds
/// range is clamped to the file's actual line count rather than silently
/// returning nothing; the range actually returned is always reported.
pub fn source_range(
    conn: &Connection,
    ws: &WorkspaceRecord,
    rel_path: &str,
    start_line: i64,
    end_line: i64,
) -> Result<SourceOutput> {
    source_range_unobserved(conn, ws, rel_path, start_line, end_line)
}

fn source_range_unobserved(
    conn: &Connection,
    ws: &WorkspaceRecord,
    rel_path: &str,
    start_line: i64,
    end_line: i64,
) -> Result<SourceOutput> {
    let whole = source_unobserved(conn, ws, rel_path)?;
    if whole.status != "verified" {
        return Ok(whole);
    }
    let Some(full_text) = whole.excerpt.as_deref() else {
        return Ok(whole);
    };

    let lines: Vec<&str> = full_text.split_inclusive('\n').collect();
    let total = lines.len() as i64;
    let start = start_line.max(1);
    let end = end_line.min(total.max(1)).max(start);
    if start > total {
        return Ok(SourceOutput {
            excerpt: Some(String::new()),
            ..whole
        });
    }
    let excerpt: String = lines[(start - 1) as usize..end as usize].concat();

    Ok(SourceOutput {
        excerpt: Some(excerpt),
        ..whole
    })
}

/// Task-session-associated exact source range. This records one range event;
/// the delegated whole-file verification remains deliberately unobserved.
pub fn source_range_for_task_session(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    rel_path: &str,
    start_line: i64,
    end_line: i64,
    task_session_id: &str,
) -> Result<SourceOutput> {
    let output = source_range_unobserved(conn, ws, rel_path, start_line, end_line)?;
    let bytes = excerpt_byte_count(&output)?;
    crate::task_session::record_exact_source_request(
        conn,
        ws,
        task_session_id,
        &output.canonical_path,
        crate::task_session::ExactSourceRequestKind::SourceRange {
            start_line,
            end_line,
        },
        &output.status,
        bytes,
    )?;
    Ok(output)
}

fn excerpt_byte_count(output: &SourceOutput) -> Result<Option<i64>> {
    output
        .excerpt
        .as_ref()
        .map(|excerpt| {
            i64::try_from(excerpt.len())
                .map_err(|_| AtlasError::Other("source excerpt byte count exceeds i64".to_string()))
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// history
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TombstoneSummary {
    pub last_path: String,
    pub last_content_hash: String,
    pub artifact_class: String,
    pub symbol_summary_json: String,
    pub relationship_summary_json: String,
    pub raw_snapshot_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEvent {
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub actor: String,
    pub file_id: Option<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_content_hash: Option<String>,
    pub new_content_hash: Option<String>,
    pub evidence_method: Option<String>,
    pub confidence: f64,
    pub tombstone: Option<TombstoneSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryOutput {
    pub events: Vec<HistoryEvent>,
    pub total_matched: i64,
    pub truncated: bool,
}

fn map_history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEvent> {
    Ok(HistoryEvent {
        event_id: r.get(0)?,
        event_type: r.get(1)?,
        occurred_at: r.get(2)?,
        actor: r.get(3)?,
        file_id: r.get(4)?,
        old_path: r.get(5)?,
        new_path: r.get(6)?,
        old_content_hash: r.get(7)?,
        new_content_hash: r.get(8)?,
        evidence_method: r.get(9)?,
        confidence: r.get(10)?,
        tombstone: None,
    })
}

/// Lifecycle history for a workspace: renames, deletions, and other
/// `lifecycle_event` rows, most recent first, optionally scoped to a single
/// path matched against either its old or new path. Deletions carry
/// metadata-only tombstones. Current and historical queries are never
/// combined; this function never reads `current_file` or `current_symbol`.
pub fn history(
    conn: &Connection,
    ws: &WorkspaceRecord,
    path: Option<&str>,
    limit: i64,
) -> Result<HistoryOutput> {
    let limit = limit.max(1);
    let where_path = if path.is_some() {
        " AND (old_path = ?2 OR new_path = ?2)"
    } else {
        ""
    };

    let total_matched: i64 = match path {
        Some(p) => conn.query_row(
            &format!("SELECT COUNT(*) FROM lifecycle_event WHERE workspace_id = ?1{where_path}"),
            params![ws.workspace_id, p],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM lifecycle_event WHERE workspace_id = ?1",
            params![ws.workspace_id],
            |r| r.get(0),
        )?,
    };

    let sql = format!(
        "SELECT event_id, event_type, occurred_at, actor, file_id, old_path, new_path,
                old_content_hash, new_content_hash, evidence_method, confidence
         FROM lifecycle_event WHERE workspace_id = ?1{where_path}
         ORDER BY occurred_at DESC, event_id DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut events: Vec<HistoryEvent> = match path {
        Some(p) => stmt
            .query_map(params![ws.workspace_id, p], map_history_row)?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt
            .query_map(params![ws.workspace_id], map_history_row)?
            .collect::<rusqlite::Result<_>>()?,
    };
    drop(stmt);

    for ev in &mut events {
        if ev.event_type == "deleted" {
            ev.tombstone = conn
                .query_row(
                    "SELECT last_path, last_content_hash, artifact_class, symbol_summary_json,
                            relationship_summary_json, raw_snapshot_state
                     FROM tombstone WHERE deletion_event_id = ?1",
                    params![ev.event_id],
                    |r| {
                        Ok(TombstoneSummary {
                            last_path: r.get(0)?,
                            last_content_hash: r.get(1)?,
                            artifact_class: r.get(2)?,
                            symbol_summary_json: r.get(3)?,
                            relationship_summary_json: r.get(4)?,
                            raw_snapshot_state: r.get(5)?,
                        })
                    },
                )
                .optional()?;
        }
    }

    Ok(HistoryOutput {
        truncated: total_matched > events.len() as i64,
        events,
        total_matched,
    })
}

// ---------------------------------------------------------------------------
// V1.1 additive: provider status (SURF-002), negative answers (QUERY-007),
// deterministic cursor (QUERY-002, SURF-003)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatusRow {
    pub provider_key: String,
    pub provider_name: String,
    pub provider_version: String,
    pub provider_tier: String,
    pub execution_kind: String,
    pub execution_scope: String,
    pub configured: bool,
    pub enabled: bool,
    pub required: bool,
    pub descriptor_fingerprint: String,
    pub last_execution_status: Option<String>,
    pub last_execution_finished_at: Option<String>,
    pub network_isolation_state: Option<String>,
    pub invalidation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatusOutput {
    pub active_generation_id: Option<String>,
    pub providers: Vec<ProviderStatusRow>,
}

/// `atlas providers` / MCP `atlas_providers`: capability/status for every
/// provider ever registered, joined against this workspace's policy and
/// most recent execution (SURF-002, `CFG-008`).
pub fn provider_status(conn: &Connection, ws: &WorkspaceRecord) -> Result<ProviderStatusOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;

    let mut stmt = conn.prepare(
        "SELECT
            pd.provider_key, pd.provider_name, pd.provider_version, pd.provider_tier,
            pd.execution_kind, pd.execution_scope, pd.descriptor_hash,
            wp.enabled, wp.required
         FROM provider_descriptor pd
         LEFT JOIN workspace_provider wp
           ON wp.provider_key = pd.provider_key AND wp.workspace_id = ?1
         ORDER BY pd.provider_name, pd.provider_version, pd.provider_key",
    )?;
    let rows = stmt.query_map(params![ws.workspace_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<i64>>(7)?,
            r.get::<_, Option<i64>>(8)?,
        ))
    })?;

    let mut providers = Vec::new();
    for row in rows {
        let (
            provider_key,
            provider_name,
            provider_version,
            provider_tier,
            execution_kind,
            execution_scope,
            descriptor_fingerprint,
            enabled,
            required,
        ) = row?;
        let configured = enabled.is_some();

        let last: Option<(String, String, String)> = conn
            .query_row(
                "SELECT status, finished_at, network_isolation_state FROM provider_execution
                 WHERE workspace_id = ?1 AND provider_key = ?2 AND finished_at IS NOT NULL
                 ORDER BY created_at DESC LIMIT 1",
                params![ws.workspace_id, provider_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let invalidation_reason: Option<String> = conn
            .query_row(
                "SELECT reason_code FROM provider_invalidation_event
                 WHERE workspace_id = ?1 AND provider_key = ?2
                 ORDER BY occurred_at DESC LIMIT 1",
                params![ws.workspace_id, provider_key],
                |r| r.get(0),
            )
            .optional()?;

        providers.push(ProviderStatusRow {
            provider_key,
            provider_name,
            provider_version,
            provider_tier,
            execution_kind,
            execution_scope,
            configured,
            enabled: enabled.map(|v| v != 0).unwrap_or(false),
            required: required.map(|v| v != 0).unwrap_or(false),
            descriptor_fingerprint,
            last_execution_status: last.as_ref().map(|(s, _, _)| s.clone()),
            last_execution_finished_at: last.as_ref().map(|(_, f, _)| f.clone()),
            network_isolation_state: last.as_ref().map(|(_, _, n)| n.clone()),
            invalidation_reason,
        });
    }

    Ok(ProviderStatusOutput {
        active_generation_id: gen_id,
        providers,
    })
}

// ---------------------------------------------------------------------------
// PERSIST-006: typed aggregate coverage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CoverageRow {
    pub scope_kind: String,
    pub scope_key: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageSummaryOutput {
    pub generation_id: Option<String>,
    /// `coverage_record.status` to count over the active generation. Distinct
    /// states remain separate rather than collapsing into an ambiguous flag.
    pub by_status: std::collections::BTreeMap<String, i64>,
    pub total: i64,
    pub rows: Vec<CoverageRow>,
}

/// `atlas coverage` / MCP `atlas_coverage` (PERSIST-006): every typed
/// coverage record persisted for the active generation, plus a status
/// breakdown. Reads `coverage_record` directly — no separate serving cache.
pub fn coverage_summary(conn: &Connection, ws: &WorkspaceRecord) -> Result<CoverageSummaryOutput> {
    let gen_id = active_generation_id(conn, &ws.workspace_id)?;
    let Some(gen_id) = gen_id else {
        return Ok(CoverageSummaryOutput {
            generation_id: None,
            by_status: std::collections::BTreeMap::new(),
            total: 0,
            rows: Vec::new(),
        });
    };

    let mut stmt = conn.prepare(
        "SELECT scope_kind, scope_key, status FROM coverage_record
         WHERE generation_id = ?1 ORDER BY scope_kind, scope_key",
    )?;
    let rows: Vec<CoverageRow> = stmt
        .query_map(params![gen_id], |r| {
            Ok(CoverageRow {
                scope_kind: r.get(0)?,
                scope_key: r.get(1)?,
                status: r.get(2)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut by_status: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in &rows {
        *by_status.entry(row.status.clone()).or_insert(0) += 1;
    }

    Ok(CoverageSummaryOutput {
        generation_id: Some(gen_id),
        total: rows.len() as i64,
        by_status,
        rows,
    })
}

/// QUERY-007 typed negative answers. Only `NotFoundWithinCoverage` may be
/// phrased as "no matching current fact exists", and only after an exhaustive,
/// non-truncated query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NegativeAnswer {
    NotFoundWithinCoverage,
    CoveragePartial,
    ProviderUnavailable,
    QueryTruncated,
    UnresolvedCandidatesExist,
}

impl NegativeAnswer {
    pub fn as_str(&self) -> &'static str {
        use NegativeAnswer::*;
        match self {
            NotFoundWithinCoverage => "not_found_within_coverage",
            CoveragePartial => "coverage_partial",
            ProviderUnavailable => "provider_unavailable",
            QueryTruncated => "query_truncated",
            UnresolvedCandidatesExist => "unresolved_candidates_exist",
        }
    }

    /// Only an exhaustive, non-truncated, fully-covered query may claim
    /// nothing matches; every other empty result must carry a qualifying
    /// negative answer instead of a bare "not found".
    pub fn for_empty_result(
        truncated: bool,
        coverage_partial: bool,
        any_provider_unavailable: bool,
    ) -> Self {
        if truncated {
            Self::QueryTruncated
        } else if any_provider_unavailable {
            Self::ProviderUnavailable
        } else if coverage_partial {
            Self::CoveragePartial
        } else {
            Self::NotFoundWithinCoverage
        }
    }
}

/// QUERY-002 / SURF-003: an opaque, deterministic pagination cursor tied to
/// workspace, active generation, the query's own hash, and an ordering
/// version. A cursor minted for one generation is rejected (`cursor_stale`)
/// against another — it must never silently continue against new truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCursor {
    pub workspace_id: String,
    pub generation_id: String,
    pub query_hash: String,
    pub ordering_version: String,
    pub last_sort_key: String,
}

impl QueryCursor {
    /// Opaque token: base16 of a `\x1f`-joined canonical representation.
    /// Not meant to be human-decoded; round-trips exactly via `decode`.
    pub fn encode(&self) -> String {
        let raw = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.workspace_id,
            self.generation_id,
            self.query_hash,
            self.ordering_version,
            self.last_sort_key
        );
        hex::encode(raw.as_bytes())
    }

    pub fn decode(token: &str) -> Option<Self> {
        let bytes = hex::decode(token).ok()?;
        let raw = String::from_utf8(bytes).ok()?;
        let mut parts = raw.split('\u{1f}');
        Some(Self {
            workspace_id: parts.next()?.to_string(),
            generation_id: parts.next()?.to_string(),
            query_hash: parts.next()?.to_string(),
            ordering_version: parts.next()?.to_string(),
            last_sort_key: parts.next()?.to_string(),
        })
    }

    /// `true` when a decoded cursor belongs to the query being re-issued
    /// against the *current* active generation. A cursor from a superseded
    /// generation must be rejected as `cursor_stale`, not silently honored.
    pub fn is_valid_for(
        &self,
        workspace_id: &str,
        generation_id: &str,
        query_hash: &str,
        ordering_version: &str,
    ) -> bool {
        self.workspace_id == workspace_id
            && self.generation_id == generation_id
            && self.query_hash == query_hash
            && self.ordering_version == ordering_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::context_ir::{ContextUseEventType, ObservationSource, RawTaskRetention, TaskKind};
    use crate::discovery;
    use crate::task_session::{create_task_session, task_session_events};
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

    fn create_session(
        conn: &Connection,
        ws: &WorkspaceRecord,
        discriminator: u8,
    ) -> crate::context_ir::TaskSession {
        let generation_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |row| row.get(0),
            )
            .unwrap();
        create_task_session(
            conn,
            ws,
            &generation_id,
            &format!("{discriminator:064x}"),
            &format!("{:064x}", discriminator.saturating_add(100)),
            RawTaskRetention::None,
            None,
            TaskKind::BugFix,
            crate::context_ir::CONTEXT_SCHEMA_VERSION,
        )
        .unwrap()
    }

    #[test]
    fn find_matches_symbol_by_substring() {
        let (_d, conn, ws, _wd) = setup();
        let out = find(&conn, &ws, "alpha", 10).unwrap();
        assert_eq!(out.total_matched, 1);
        assert_eq!(out.results[0].display_name, "alpha");
    }

    #[test]
    fn find_reports_coverage_bounded_negative() {
        let (_d, conn, ws, _wd) = setup();
        let out = find(&conn, &ws, "nonexistent_symbol_xyz", 10).unwrap();
        assert_eq!(out.total_matched, 0);
        assert!(out.coverage_note.contains("active indexed coverage"));
        assert!(!out
            .coverage_note
            .to_lowercase()
            .contains("no reference exists"));
    }

    #[test]
    fn inspect_returns_symbols_and_relationships() {
        let (_d, conn, ws, _wd) = setup();
        let out = inspect(&conn, &ws, "src/a.ts").unwrap();
        assert!(out.symbols.iter().any(|s| s.display_name == "alpha"));
        assert!(out
            .relationships
            .iter()
            .any(|r| r.relationship_type == "imports" && r.direction == "outbound"));
    }

    #[test]
    fn trace_follows_outbound_imports() {
        let (_d, conn, ws, _wd) = setup();
        let out = trace(&conn, &ws, "src/a.ts", "outbound", 3, 50).unwrap();
        assert!(out.edges.iter().any(|e| e.to_ref == "./b"));
    }

    #[test]
    fn source_verifies_hash_and_returns_excerpt() {
        let (_d, conn, ws, _wd) = setup();
        let out = source(&conn, &ws, "src/a.ts").unwrap();
        assert_eq!(out.status, "verified");
        assert!(out.excerpt.unwrap().contains("alpha"));
    }

    #[test]
    fn source_blocks_on_hash_mismatch() {
        let (_d, conn, ws, wd) = setup();
        // Mutate the file after indexing without reconciling.
        std::fs::write(wd.path().join("src/a.ts"), "// changed\n").unwrap();
        let out = source(&conn, &ws, "src/a.ts").unwrap();
        assert_eq!(out.status, "hash_mismatch");
        assert!(out.excerpt.is_none());
    }

    #[test]
    fn impact_finds_direct_dependents() {
        let (_d, conn, ws, _wd) = setup();
        let out = impact(&conn, &ws, &["./b".to_string()]).unwrap();
        assert!(out.direct_dependents.iter().any(|p| p == "src/a.ts"));
    }

    #[test]
    fn find_populates_evidence_envelope_for_active_generation() {
        let (_d, conn, ws, _wd) = setup();
        let out = find(&conn, &ws, "alpha", 10).unwrap();
        let evidence = out
            .evidence
            .expect("evidence envelope must be present when a generation is active");
        assert_eq!(evidence.generation_id, out.generation_id.unwrap());
        assert_eq!(evidence.conflicts, 0);
        assert_eq!(evidence.unresolved, 0);
    }

    #[test]
    fn find_filtered_paginates_with_a_generation_bound_cursor() {
        let (_d, conn, ws, _wd) = setup();
        // "a" matches both `alpha` and `beta` (both contain the letter 'a').
        let page1 = find_filtered(&conn, &ws, "a", 1, &FindFilter::default()).unwrap();
        assert_eq!(page1.results.len(), 1);
        assert!(page1.truncated);
        let cursor = page1
            .next_cursor
            .clone()
            .expect("truncated result must carry a resume cursor");

        let page2 = find_filtered(
            &conn,
            &ws,
            "a",
            1,
            &FindFilter {
                cursor: Some(&cursor),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page2.results.len(), 1);
        assert_ne!(
            page1.results[0].canonical_symbol_key, page2.results[0].canonical_symbol_key,
            "the cursor must resume past the first page, not repeat it"
        );

        // A cursor from a different query hash must not be honored -- it
        // silently restarts rather than resuming against mismatched truth.
        let mismatched = find_filtered(
            &conn,
            &ws,
            "different_query_xyz",
            1,
            &FindFilter {
                cursor: Some(&cursor),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(mismatched.total_matched, 0);
    }

    #[test]
    fn find_filtered_applies_path_prefix_filter() {
        let (_d, conn, ws, _wd) = setup();
        let out = find_filtered(
            &conn,
            &ws,
            "a",
            10,
            &FindFilter {
                path_prefix: Some("src/b"),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out
            .results
            .iter()
            .all(|r| r.canonical_path.starts_with("src/b")));
        assert!(out.results.iter().any(|r| r.display_name == "beta"));
    }

    #[test]
    fn inspect_symbol_returns_the_same_shape_as_path_inspect() {
        let (_d, conn, ws, _wd) = setup();
        let by_path = inspect(&conn, &ws, "src/a.ts").unwrap();
        let key = by_path
            .symbols
            .iter()
            .find(|s| s.display_name == "alpha")
            .unwrap()
            .canonical_symbol_key
            .clone();
        let by_symbol = inspect_symbol(&conn, &ws, &key).unwrap();
        assert_eq!(by_symbol.canonical_path, "src/a.ts");
        assert!(by_symbol.symbols.iter().any(|s| s.display_name == "alpha"));
    }

    #[test]
    fn inspect_symbol_reports_coverage_note_when_key_unknown() {
        let (_d, conn, ws, _wd) = setup();
        let out = inspect_symbol(&conn, &ws, "no-such-canonical-key").unwrap();
        assert!(out.symbols.is_empty());
        assert!(out.coverage_note.contains("active indexed coverage"));
    }

    #[test]
    fn trace_resolved_never_treats_unresolved_structural_edges_as_verified() {
        let (_d, conn, ws, _wd) = setup();
        // The structural-only fixture never persists a `relationship_resolution`
        // row (that table is only populated by S6 semantic resolution), so a
        // resolved-graph trace must find zero edges here -- never silently
        // fall back to the raw structural edge as if it had been resolved.
        let out = trace_resolved(&conn, &ws, "src/a.ts", "outbound", 3, 50).unwrap();
        assert!(
            out.edges.is_empty(),
            "no relationship_resolution rows exist; resolved trace must not fabricate an edge"
        );
    }

    #[test]
    fn impact_frontier_fields_are_present_and_independent_of_structural_dependents() {
        let (_d, conn, ws, _wd) = setup();
        let out = impact(&conn, &ws, &["./b".to_string()]).unwrap();
        assert!(out.direct_dependents.iter().any(|p| p == "src/a.ts"));
        // No semantic resolution ran in this structural-only fixture, so the
        // resolved frontier is empty -- distinct from (not derived from)
        // `direct_dependents`, which does have a match.
        assert!(out.verified_dependents.is_empty());
        assert!(out.uncertain_dependents.is_empty());
    }

    #[test]
    fn source_range_returns_only_the_requested_lines() {
        let (_d, conn, ws, _wd) = setup();
        let out = source_range(&conn, &ws, "src/a.ts", 2, 2).unwrap();
        assert_eq!(out.status, "verified");
        let excerpt = out.excerpt.unwrap();
        assert!(excerpt.contains("alpha"));
        assert!(
            !excerpt.contains("import"),
            "line 1 (the import) must not be included when only line 2 was requested"
        );
    }

    #[test]
    fn source_range_still_blocks_on_hash_mismatch() {
        let (_d, conn, ws, wd) = setup();
        std::fs::write(wd.path().join("src/a.ts"), "// changed\n").unwrap();
        let out = source_range(&conn, &ws, "src/a.ts", 1, 1).unwrap();
        assert_eq!(out.status, "hash_mismatch");
        assert!(out.excerpt.is_none());
    }

    #[test]
    fn attributed_source_records_every_typed_output_with_exact_bytes_and_one_range_event() {
        let (_d, mut conn, ws, wd) = setup();
        std::fs::write(wd.path().join("src/empty.ts"), "").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();
        let session = create_session(&conn, &ws, 1);

        let verified =
            source_for_task_session(&mut conn, &ws, "src/a.ts", &session.task_session_id).unwrap();
        let range = source_range_for_task_session(
            &mut conn,
            &ws,
            "src/a.ts",
            2,
            2,
            &session.task_session_id,
        )
        .unwrap();
        let not_found =
            source_for_task_session(&mut conn, &ws, "src/missing.ts", &session.task_session_id)
                .unwrap();
        std::fs::write(wd.path().join("src/a.ts"), "// changed\n").unwrap();
        let mismatch =
            source_for_task_session(&mut conn, &ws, "src/a.ts", &session.task_session_id).unwrap();
        std::fs::remove_file(wd.path().join("src/b.ts")).unwrap();
        let read_failed =
            source_for_task_session(&mut conn, &ws, "src/b.ts", &session.task_session_id).unwrap();
        let empty =
            source_for_task_session(&mut conn, &ws, "src/empty.ts", &session.task_session_id)
                .unwrap();

        assert_eq!(
            [
                verified.status.as_str(),
                range.status.as_str(),
                not_found.status.as_str(),
                mismatch.status.as_str(),
                read_failed.status.as_str(),
            ],
            [
                "verified",
                "verified",
                "not_found",
                "hash_mismatch",
                "read_failed"
            ]
        );
        assert_eq!(empty.excerpt.as_deref(), Some(""));

        let events = task_session_events(&conn, &session.task_session_id).unwrap();
        assert_eq!(
            events.len(),
            6,
            "a range must not double-count whole-file verification"
        );
        let mut by_ordinal = std::collections::BTreeMap::new();
        let mut event_ids = std::collections::HashSet::new();
        for event in events {
            assert_eq!(event.event_type, ContextUseEventType::ExactSourceRequested);
            assert_eq!(event.observation_source, ObservationSource::AtlasObserved);
            assert!(
                event.context_id.is_none() && event.item_id.is_none() && event.entity_id.is_none()
            );
            assert!(event_ids.insert(event.event_id));
            let metadata = event.details.to_string();
            assert!(!metadata.contains("src/a.ts") && !metadata.contains("alpha"));
            assert_eq!(event.details["path_hash"].as_str().unwrap().len(), 64);
            by_ordinal.insert(
                event.details["request_ordinal"].as_i64().unwrap(),
                (
                    event.details["request_kind"].as_str().unwrap().to_string(),
                    event.bytes,
                ),
            );
        }
        assert_eq!(by_ordinal.len(), 6);
        assert_eq!(
            by_ordinal[&1].1,
            Some(verified.excerpt.as_ref().unwrap().len() as i64)
        );
        assert_eq!(
            by_ordinal[&2],
            (
                "source_range".to_string(),
                Some(range.excerpt.as_ref().unwrap().len() as i64),
            )
        );
        assert_eq!(by_ordinal[&3].1, None);
        assert_eq!(by_ordinal[&4].1, None);
        assert_eq!(by_ordinal[&5].1, None);
        assert_eq!(by_ordinal[&6].1, Some(0));
    }

    #[test]
    fn attributed_source_failures_are_closed_without_partial_events_or_consumed_ordinals() {
        {
            let (_d, mut conn, ws, _wd) = setup();
            let session = create_session(&conn, &ws, 2);
            let mut wrong_workspace = ws.clone();
            wrong_workspace.workspace_id = "ws_wrong".to_string();
            let error = source_for_task_session(
                &mut conn,
                &wrong_workspace,
                "src/a.ts",
                &session.task_session_id,
            )
            .unwrap_err();
            assert!(error.to_string().contains("does not belong"));
            conn.execute_batch("DROP VIEW current_file").unwrap();
            assert!(
                source_for_task_session(&mut conn, &ws, "src/a.ts", &session.task_session_id,)
                    .is_err()
            );
            assert!(task_session_events(&conn, &session.task_session_id)
                .unwrap()
                .is_empty());
        }

        let (_d, mut conn, ws, _wd) = setup();
        let session = create_session(&conn, &ws, 3);
        conn.execute_batch(
            "CREATE TRIGGER reject_exact_source
             BEFORE INSERT ON context_use_event
             WHEN NEW.event_type = 'exact_source_requested'
             BEGIN SELECT RAISE(ABORT, 'rejected telemetry'); END;",
        )
        .unwrap();
        assert!(
            source_for_task_session(&mut conn, &ws, "src/a.ts", &session.task_session_id,).is_err()
        );
        assert!(task_session_events(&conn, &session.task_session_id)
            .unwrap()
            .is_empty());
        conn.execute_batch("DROP TRIGGER reject_exact_source")
            .unwrap();
        source_for_task_session(&mut conn, &ws, "src/a.ts", &session.task_session_id).unwrap();
        let events = task_session_events(&conn, &session.task_session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details["request_ordinal"], 1);
    }

    #[test]
    fn legacy_source_is_unobserved_and_each_session_has_independent_ordinals() {
        let (_d, mut conn, ws, _wd) = setup();
        let first = create_session(&conn, &ws, 4);
        let second = create_session(&conn, &ws, 5);
        source(&conn, &ws, "src/a.ts").unwrap();
        source_range(&conn, &ws, "src/a.ts", 1, 1).unwrap();
        assert!(task_session_events(&conn, &first.task_session_id)
            .unwrap()
            .is_empty());

        source_for_task_session(&mut conn, &ws, "src/a.ts", &first.task_session_id).unwrap();
        source_for_task_session(&mut conn, &ws, "src/a.ts", &second.task_session_id).unwrap();
        let first_events = task_session_events(&conn, &first.task_session_id).unwrap();
        let second_events = task_session_events(&conn, &second.task_session_id).unwrap();
        assert_eq!(first_events[0].details["request_ordinal"], 1);
        assert_eq!(second_events[0].details["request_ordinal"], 1);
        assert_ne!(first_events[0].event_id, second_events[0].event_id);
    }

    #[test]
    fn history_reports_deletion_with_tombstone() {
        let (_d, conn, ws, wd) = setup();
        std::fs::remove_file(wd.path().join("src/b.ts")).unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();

        let out = history(&conn, &ws, None, 50).unwrap();
        assert_eq!(out.total_matched, 1);
        assert!(!out.truncated);
        let ev = &out.events[0];
        assert_eq!(ev.event_type, "deleted");
        assert_eq!(ev.old_path.as_deref(), Some("src/b.ts"));
        let tomb = ev.tombstone.as_ref().unwrap();
        assert_eq!(tomb.last_path, "src/b.ts");
        assert_eq!(tomb.raw_snapshot_state, "not_retained");

        // Path-scoped query only returns events touching that path.
        let scoped = history(&conn, &ws, Some("src/a.ts"), 50).unwrap();
        assert_eq!(scoped.total_matched, 0);
    }

    #[test]
    fn provider_status_reflects_descriptor_and_workspace_policy() {
        let (_d, conn, ws, _wd) = setup();
        let descriptor = crate::providers::builtin_provider_descriptors().remove(0);
        let provider_key =
            crate::provider_persistence::register_provider_descriptor(&conn, &descriptor).unwrap();
        crate::provider_persistence::set_workspace_provider_policy(
            &conn,
            &ws.workspace_id,
            &provider_key,
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

        let out = provider_status(&conn, &ws).unwrap();
        let row = out
            .providers
            .iter()
            .find(|p| p.provider_key == provider_key)
            .unwrap();
        assert!(row.configured);
        assert!(row.enabled);
        assert!(row.required);
        assert_eq!(row.descriptor_fingerprint, descriptor.fingerprint);
    }

    #[test]
    fn coverage_summary_aggregates_by_status_for_the_active_generation() {
        let (_d, conn, ws, _wd) = setup();
        let gen_id: String = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![ws.workspace_id],
                |r| r.get(0),
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
        crate::provider_persistence::record_coverage(
            &conn,
            &gen_id,
            None,
            "provider",
            "another-provider@1.0.0",
            "failed",
            "[]",
            "[]",
            "{}",
        )
        .unwrap();
        crate::provider_persistence::record_coverage(
            &conn,
            &gen_id,
            None,
            "capability",
            "resolution",
            "partial",
            "[]",
            "[]",
            "{}",
        )
        .unwrap();

        let out = coverage_summary(&conn, &ws).unwrap();
        assert_eq!(out.generation_id.as_deref(), Some(gen_id.as_str()));
        assert_eq!(out.total, 3);
        assert_eq!(out.by_status.get("complete"), Some(&1));
        assert_eq!(out.by_status.get("failed"), Some(&1));
        assert_eq!(out.by_status.get("partial"), Some(&1));
        assert!(out
            .rows
            .iter()
            .any(|r| r.scope_kind == "capability" && r.scope_key == "resolution"));
    }

    #[test]
    fn provider_status_shows_unconfigured_when_never_registered_to_workspace() {
        let (_d, conn, ws, _wd) = setup();
        let descriptor = crate::providers::builtin_provider_descriptors().remove(1);
        crate::provider_persistence::register_provider_descriptor(&conn, &descriptor).unwrap();
        let out = provider_status(&conn, &ws).unwrap();
        let row = out
            .providers
            .iter()
            .find(|p| p.descriptor_fingerprint == descriptor.fingerprint)
            .unwrap();
        assert!(!row.configured);
        assert!(!row.enabled);
    }

    #[test]
    fn negative_answer_only_claims_not_found_when_fully_covered() {
        assert_eq!(
            NegativeAnswer::for_empty_result(false, false, false),
            NegativeAnswer::NotFoundWithinCoverage
        );
        assert_eq!(
            NegativeAnswer::for_empty_result(true, false, false),
            NegativeAnswer::QueryTruncated
        );
        assert_eq!(
            NegativeAnswer::for_empty_result(false, false, true),
            NegativeAnswer::ProviderUnavailable
        );
        assert_eq!(
            NegativeAnswer::for_empty_result(false, true, false),
            NegativeAnswer::CoveragePartial
        );
    }

    #[test]
    fn query_cursor_round_trips_and_detects_staleness() {
        let cursor = QueryCursor {
            workspace_id: "ws_1".to_string(),
            generation_id: "gen_1".to_string(),
            query_hash: "qh_1".to_string(),
            ordering_version: "v1".to_string(),
            last_sort_key: "src/z.ts".to_string(),
        };
        let token = cursor.encode();
        let decoded = QueryCursor::decode(&token).unwrap();
        assert_eq!(decoded, cursor);
        assert!(decoded.is_valid_for("ws_1", "gen_1", "qh_1", "v1"));
        assert!(
            !decoded.is_valid_for("ws_1", "gen_2", "qh_1", "v1"),
            "cursor from a superseded generation must be stale"
        );
    }

    #[test]
    fn query_cursor_decode_rejects_garbage_token() {
        assert!(QueryCursor::decode("not-a-valid-hex-token").is_none());
    }
}
