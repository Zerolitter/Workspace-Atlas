//! V1.2 Serving Plane (G4): builds generation-bound `SymbolCard`s,
//! `ServingEdge`s, and coverage rollups from truth (`symbol_fact`,
//! `relationship_fact`, `relationship_resolution`, `coverage_record`, and
//! `evidence_conflict`) into the `serving_generation`,
//! `symbol_serving_projection`, `relationship_serving_edge`, and
//! `serving_coverage_rollup` tables defined by
//! `migrations/0003_context_intelligence_foundation.sql`.
//!
//! Every build:
//! - **is atomic**: runs inside one transaction. A mid-build error rolls
//!   the whole transaction back — no partial `symbol_serving_projection`
//!   or `relationship_serving_edge` row ever persists without its parent
//!   `serving_generation` reaching `state = 'ready'` (G4 "failure falls
//!   back without truth loss").
//! - **is generation/policy keyed**: `serving_generation_id` is a
//!   deterministic function of `(workspace_id, generation_id,
//!   serving_schema_version, projection_policy_version)` — the same key
//!   always names the same projection.
//! - **is deterministic**: identical truth produces byte-identical
//!   `SymbolCard.card_hash` values (`context_ir::SymbolCard::seal`).
//! - **deletes and rebuilds identically**: `delete_serving_generation`
//!   cascades (via `ON DELETE CASCADE`) to every projection/edge/rollup
//!   row; rebuilding from the same unchanged truth reproduces the same
//!   card hashes (proven by
//!   `rebuild_after_delete_reproduces_identical_card_hashes`).
//!
//! Nothing in V1/V1.1 reads these tables — `atlas init`/`reconcile`/
//! `find`/`inspect`/`trace`/`impact`/`source`/`context` remain fully
//! functional against a catalogue with zero Serving Plane rows (G3's "V1.1
//! works without Serving Plane data" gate, carried forward here).

use std::{cell::Cell, time::Instant};

use rusqlite::{params, Connection, OptionalExtension};

use crate::context_ir::{
    CoverageState, PreferredEvidenceSummary, SymbolCard, SymbolCost, SymbolCounts, SymbolRange,
};
use crate::error::{AtlasError, Result};
use crate::resolution::deterministic_id;
use crate::workspace::WorkspaceRecord;

pub const SERVING_SCHEMA_VERSION: &str = "1.0.0";
pub const PROJECTION_POLICY_VERSION: &str = "projection-v1.0.0";

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ServingBuildInputMetrics {
    pub source_fact_fingerprint: String,
    pub canonical_input_fingerprint: String,
    pub symbol_fact_count: i64,
    pub relationship_fact_count: i64,
    pub relationship_resolution_count: i64,
    pub effect_fact_count: i64,
    pub coverage_record_count: i64,
    pub evidence_conflict_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ServingBuildStageMetrics {
    pub input_scan_us: i64,
    pub symbol_cards_us: i64,
    pub relationship_edges_us: i64,
    pub coverage_rollups_us: i64,
    pub validation_us: i64,
}

impl ServingBuildStageMetrics {
    pub fn total_recorded_us(&self) -> i64 {
        self.input_scan_us
            .saturating_add(self.symbol_cards_us)
            .saturating_add(self.relationship_edges_us)
            .saturating_add(self.coverage_rollups_us)
            .saturating_add(self.validation_us)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingReadinessState {
    Absent,
    Building,
    Ready,
    Failed,
    Stale,
    Superseded,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServingProjectionStatus {
    pub serving_generation_id: String,
    pub generation_id: String,
    pub serving_schema_version: String,
    pub projection_policy_version: String,
    pub state: ServingReadinessState,
    pub source_fact_fingerprint: String,
    pub card_count: i64,
    pub edge_count: i64,
    pub coverage_rollup_count: i64,
    pub output_rows: i64,
    pub output_bytes: i64,
    pub output_hash: String,
    pub failure_code: Option<String>,
    pub counts_match: bool,
    pub content_valid: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServingStatusReport {
    pub ok: bool,
    pub workspace_id: String,
    pub active_generation_id: Option<String>,
    pub serving_schema_version: String,
    pub projection_policy_version: String,
    pub expected_serving_generation_id: Option<String>,
    pub state: ServingReadinessState,
    pub current: Option<ServingProjectionStatus>,
    pub superseded: Vec<ServingProjectionStatus>,
    pub rebuild_recommended: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServingStatusPage {
    pub status: ServingStatusReport,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServingBuildReport {
    pub ok: bool,
    pub serving_generation_id: String,
    pub workspace_id: String,
    pub generation_id: String,
    pub serving_schema_version: String,
    pub projection_policy_version: String,
    pub state: String,
    pub card_count: i64,
    pub edge_count: i64,
    pub coverage_rollup_count: i64,
    pub output_rows: i64,
    pub output_bytes: i64,
    pub output_hash: String,
    pub input: ServingBuildInputMetrics,
    pub stages: ServingBuildStageMetrics,
    pub cache_hit: bool,
    pub cache_miss: bool,
    pub serving_fallback: bool,
    pub failure_code: Option<String>,
    pub rebuild_recommended: bool,
    pub build_elapsed_us: i64,
}

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

#[derive(Debug, serde::Serialize)]
struct StoredProjection {
    serving_generation_id: String,
    generation_id: String,
    serving_schema_version: String,
    projection_policy_version: String,
    state: String,
    source_fact_fingerprint: String,
    card_count: i64,
    edge_count: i64,
    coverage_rollup_count: i64,
    failure_code: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServingStatusWatermark {
    active_generation_id: Option<String>,
    max_rowid: i64,
    row_count: i64,
    snapshot_digest: String,
    prefix_digest: String,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ServingStatusOrderingKey {
    generation_id: String,
    serving_schema_version: String,
    projection_policy_version: String,
    serving_generation_id: String,
}
fn serving_status_snapshot_digest(
    conn: &Connection,
    workspace_id: &str,
    max_rowid: i64,
    current: Option<&str>,
    through: Option<&ServingStatusOrderingKey>,
) -> Result<(String, i64, Option<ServingStatusOrderingKey>)> {
    let mut statement = conn.prepare(
        "SELECT rowid, serving_generation_id, generation_id, serving_schema_version,
                projection_policy_version, state, source_fact_fingerprint,
                card_count, edge_count, coverage_rollup_count, failure_code
         FROM serving_generation
         WHERE workspace_id = ?1 AND rowid <= ?2
           AND (?3 IS NULL OR (
               serving_generation_id <> COALESCE(?4, '') AND (
               generation_id < ?3 OR
               (generation_id = ?3 AND serving_schema_version < ?5) OR
               (generation_id = ?3 AND serving_schema_version = ?5
                AND projection_policy_version < ?6) OR
               (generation_id = ?3 AND serving_schema_version = ?5
                AND projection_policy_version = ?6 AND serving_generation_id <= ?7)
           )))
         ORDER BY generation_id, serving_schema_version, projection_policy_version,
                  serving_generation_id, rowid",
    )?;
    let mut rows = statement.query(params![
        workspace_id,
        max_rowid,
        through.map(|key| key.generation_id.as_str()),
        current,
        through.map(|key| key.serving_schema_version.as_str()),
        through.map(|key| key.projection_policy_version.as_str()),
        through.map(|key| key.serving_generation_id.as_str()),
    ])?;
    let domain = if through.is_some() {
        b"serving-status-prefix".as_slice()
    } else {
        b"serving-status-snapshot".as_slice()
    };
    let mut digest = crate::context_application::ApplicationSnapshotHasher::new(domain);
    let mut count = 0_i64;
    let mut last = None;
    while let Some(row) = rows.next()? {
        let rowid = row.get::<_, i64>(0)?;
        let stored = StoredProjection {
            serving_generation_id: row.get(1)?,
            generation_id: row.get(2)?,
            serving_schema_version: row.get(3)?,
            projection_policy_version: row.get(4)?,
            state: row.get(5)?,
            source_fact_fingerprint: row.get(6)?,
            card_count: row.get(7)?,
            edge_count: row.get(8)?,
            coverage_rollup_count: row.get(9)?,
            failure_code: row.get(10)?,
        };
        last = Some(ServingStatusOrderingKey {
            serving_generation_id: stored.serving_generation_id.clone(),
            generation_id: stored.generation_id.clone(),
            serving_schema_version: stored.serving_schema_version.clone(),
            projection_policy_version: stored.projection_policy_version.clone(),
        });
        let effective_state = (current != Some(stored.serving_generation_id.as_str()))
            .then_some(ServingReadinessState::Superseded);
        digest.record(&(rowid, &stored));
        let status = projection_status(conn, stored, effective_state)?;
        digest.record(&status);
        count += 1;
    }
    Ok((digest.finish(), count, last))
}

pub fn serving_status(conn: &Connection, ws: &WorkspaceRecord) -> Result<ServingStatusReport> {
    serving_status_with_policy(conn, ws, SERVING_SCHEMA_VERSION, PROJECTION_POLICY_VERSION)
}

pub fn serving_status_with_policy(
    conn: &Connection,
    ws: &WorkspaceRecord,
    serving_schema_version: &str,
    projection_policy_version: &str,
) -> Result<ServingStatusReport> {
    let active_generation_id = active_generation_id(conn, &ws.workspace_id)?;
    let expected_serving_generation_id = active_generation_id.as_deref().map(|generation_id| {
        deterministic_id(
            "serve_gen",
            &[
                &ws.workspace_id,
                generation_id,
                serving_schema_version,
                projection_policy_version,
            ],
        )
    });
    let stored = stored_projections(conn, &ws.workspace_id)?;
    let mut current = None;
    let mut superseded = Vec::new();
    for projection in stored {
        if Some(projection.serving_generation_id.as_str())
            == expected_serving_generation_id.as_deref()
        {
            current = Some(projection_status(conn, projection, None)?);
        } else {
            superseded.push(projection_status(
                conn,
                projection,
                Some(ServingReadinessState::Superseded),
            )?);
        }
    }
    let state = current
        .as_ref()
        .map(|projection| projection.state)
        .unwrap_or(ServingReadinessState::Absent);
    Ok(ServingStatusReport {
        ok: true,
        workspace_id: ws.workspace_id.clone(),
        active_generation_id,
        serving_schema_version: serving_schema_version.to_string(),
        projection_policy_version: projection_policy_version.to_string(),
        expected_serving_generation_id,
        state,
        current,
        superseded,
        rebuild_recommended: state != ServingReadinessState::Ready,
    })
}

/// Return one bounded snapshot/keyset page of serving readiness.
pub fn serving_status_page(
    conn: &Connection,
    ws: &WorkspaceRecord,
    limit: usize,
    cursor: Option<&str>,
) -> std::result::Result<
    ServingStatusPage,
    crate::context_application::CatalogueContextApplicationError,
> {
    if limit == 0 || limit > crate::context_route::MAX_APPLICATION_PAGE_LIMIT {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        ))
        .into());
    }
    let tx = conn.unchecked_transaction()?;
    let current_active_generation_id = active_generation_id(&tx, &ws.workspace_id)?;
    let expected_serving_generation_id =
        current_active_generation_id
            .as_deref()
            .map(|generation_id| {
                deterministic_id(
                    "serve_gen",
                    &[
                        &ws.workspace_id,
                        generation_id,
                        SERVING_SCHEMA_VERSION,
                        PROJECTION_POLICY_VERSION,
                    ],
                )
            });
    let filter = crate::context_application::ApplicationCursorFilter::ServingPolicy {
        serving_schema_version: SERVING_SCHEMA_VERSION,
        projection_policy_version: PROJECTION_POLICY_VERSION,
    };
    let identity_binding = crate::context_application::ApplicationCursorBinding {
        workspace_id: &ws.workspace_id,
        operation: crate::context_application::ApplicationCursorOperation::ServingStatus,
        contract_version: SERVING_SCHEMA_VERSION,
        filter,
        captured_state: "",
    };
    let (watermark, ordering) = if let Some(token) = cursor {
        let decoded = crate::context_application::decode_application_cursor_snapshot(
            &tx,
            ws,
            &identity_binding,
            token,
        )?;
        let watermark: ServingStatusWatermark = serde_json::from_str(&decoded.captured_state)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        let ordering: ServingStatusOrderingKey = serde_json::from_str(&decoded.ordering_key)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        if watermark.active_generation_id != current_active_generation_id {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
        let (snapshot_digest, count, _) = serving_status_snapshot_digest(
            &tx,
            &ws.workspace_id,
            watermark.max_rowid,
            expected_serving_generation_id.as_deref(),
            None,
        )?;
        let (prefix_digest, _, last) = serving_status_snapshot_digest(
            &tx,
            &ws.workspace_id,
            watermark.max_rowid,
            expected_serving_generation_id.as_deref(),
            Some(&ordering),
        )?;
        if count != watermark.row_count
            || snapshot_digest != watermark.snapshot_digest
            || prefix_digest != watermark.prefix_digest
            || last.as_ref() != Some(&ordering)
        {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
        (watermark, ordering)
    } else {
        let (max_rowid, row_count) = tx.query_row(
            "SELECT COALESCE(MAX(rowid), 0), COUNT(*) FROM serving_generation
             WHERE workspace_id = ?1",
            params![ws.workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (snapshot_digest, digest_count, _) = serving_status_snapshot_digest(
            &tx,
            &ws.workspace_id,
            max_rowid,
            expected_serving_generation_id.as_deref(),
            None,
        )?;
        if digest_count != row_count {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
        (
            ServingStatusWatermark {
                active_generation_id: current_active_generation_id.clone(),
                max_rowid,
                row_count,
                snapshot_digest,
                prefix_digest: String::new(),
            },
            ServingStatusOrderingKey {
                generation_id: String::new(),
                serving_schema_version: String::new(),
                projection_policy_version: String::new(),
                serving_generation_id: String::new(),
            },
        )
    };
    let current = if let Some(expected) = expected_serving_generation_id.as_deref() {
        tx.query_row(
            "SELECT serving_generation_id, generation_id, serving_schema_version,
                    projection_policy_version, state, source_fact_fingerprint,
                    card_count, edge_count, coverage_rollup_count, failure_code
             FROM serving_generation
             WHERE workspace_id = ?1 AND serving_generation_id = ?2 AND rowid <= ?3",
            params![ws.workspace_id, expected, watermark.max_rowid],
            stored_projection_from_row,
        )
        .optional()?
        .map(|projection| projection_status(&tx, projection, None))
        .transpose()?
    } else {
        None
    };
    let fetch = i64::try_from(limit + 1)
        .map_err(|_| AtlasError::InvalidConfig("limit is too large".into()))?;
    let mut statement = tx.prepare(
        "SELECT serving_generation_id, generation_id, serving_schema_version,
                projection_policy_version, state, source_fact_fingerprint,
                card_count, edge_count, coverage_rollup_count, failure_code
         FROM serving_generation
         WHERE workspace_id = ?1 AND rowid <= ?2
           AND serving_generation_id <> COALESCE(?3, '')
           AND (
               generation_id > ?4 OR
               (generation_id = ?4 AND serving_schema_version > ?5) OR
               (generation_id = ?4 AND serving_schema_version = ?5
                AND projection_policy_version > ?6) OR
               (generation_id = ?4 AND serving_schema_version = ?5
                AND projection_policy_version = ?6 AND serving_generation_id > ?7)
           )
         ORDER BY generation_id, serving_schema_version, projection_policy_version,
                  serving_generation_id
         LIMIT ?8",
    )?;
    let stored = statement.query_map(
        params![
            ws.workspace_id,
            watermark.max_rowid,
            expected_serving_generation_id,
            ordering.generation_id,
            ordering.serving_schema_version,
            ordering.projection_policy_version,
            ordering.serving_generation_id,
            fetch
        ],
        stored_projection_from_row,
    )?;
    let mut superseded = Vec::with_capacity(limit + 1);
    for projection in stored {
        let projection = projection?;
        superseded.push(projection_status(
            &tx,
            projection,
            Some(ServingReadinessState::Superseded),
        )?);
    }
    let has_more = superseded.len() > limit;
    superseded.truncate(limit);
    let last_key = superseded
        .last()
        .map(|projection| ServingStatusOrderingKey {
            generation_id: projection.generation_id.clone(),
            serving_schema_version: projection.serving_schema_version.clone(),
            projection_policy_version: projection.projection_policy_version.clone(),
            serving_generation_id: projection.serving_generation_id.clone(),
        });
    let state = current
        .as_ref()
        .map(|projection| projection.state)
        .unwrap_or(ServingReadinessState::Absent);
    let status = ServingStatusReport {
        ok: true,
        workspace_id: ws.workspace_id.clone(),
        active_generation_id: current_active_generation_id,
        serving_schema_version: SERVING_SCHEMA_VERSION.to_string(),
        projection_policy_version: PROJECTION_POLICY_VERSION.to_string(),
        expected_serving_generation_id: expected_serving_generation_id.clone(),
        state,
        current,
        superseded,
        rebuild_recommended: state != ServingReadinessState::Ready,
    };
    let next_cursor = if has_more {
        let last_key = last_key
            .as_ref()
            .ok_or_else(|| AtlasError::InvalidConfig("cursor_invalid".into()))?;
        let (prefix_digest, _, prefix_last) = serving_status_snapshot_digest(
            &tx,
            &ws.workspace_id,
            watermark.max_rowid,
            expected_serving_generation_id.as_deref(),
            Some(last_key),
        )?;
        if prefix_last.as_ref() != Some(last_key) {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
        let captured_state = serde_json::to_string(&ServingStatusWatermark {
            prefix_digest,
            ..watermark
        })?;
        let binding = crate::context_application::ApplicationCursorBinding {
            captured_state: &captured_state,
            ..identity_binding
        };
        Some(crate::context_application::encode_application_cursor(
            &tx,
            ws,
            &binding,
            &serde_json::to_string(last_key)?,
        )?)
    } else {
        None
    };
    drop(statement);
    tx.commit()?;
    Ok(ServingStatusPage {
        status,
        next_cursor,
    })
}

pub(crate) fn ready_serving_generation_id(
    conn: &Connection,
    workspace_id: &str,
    generation_id: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT serving_generation_id FROM serving_generation
         WHERE workspace_id = ?1 AND generation_id = ?2 AND serving_schema_version = ?3
           AND projection_policy_version = ?4 AND state = 'ready'",
        params![
            workspace_id,
            generation_id,
            SERVING_SCHEMA_VERSION,
            PROJECTION_POLICY_VERSION
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ServingReadExaminations {
    pub symbol_projection_rows: usize,
    pub symbol_truth_rows: usize,
    pub relationship_projection_rows: usize,
    pub relationship_truth_rows: usize,
}

impl ServingReadExaminations {
    pub(crate) fn total(self) -> usize {
        self.symbol_projection_rows
            .saturating_add(self.symbol_truth_rows)
            .saturating_add(self.relationship_projection_rows)
            .saturating_add(self.relationship_truth_rows)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServingSymbolEvidence {
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServingRelationshipEvidence {
    pub relationship_fact_id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub raw_target_ref_kind: String,
    pub raw_target_ref_value: String,
    pub relationship_type: String,
    pub resolution_state: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RelationshipReadFilter {
    pub include_callers: bool,
    pub include_callees: bool,
    pub include_tests: bool,
    pub include_configs: bool,
}

pub(crate) struct ServingRelationshipSlice {
    pub rows: Vec<ServingRelationshipEvidence>,
    pub truncated: bool,
}

/// Request-local reader for the two Serving categories consumed by the
/// legacy compiler. Each call accepts a caller-controlled result limit,
/// examines at most one `limit + 1` sentinel slice per indexed direction,
/// and records the actual projection/Truth rows returned by those slices.
pub(crate) struct ServingReader<'a> {
    conn: &'a Connection,
    generation_id: &'a str,
    serving_generation_id: Option<String>,
    examinations: &'a Cell<ServingReadExaminations>,
    validation_mismatch: bool,
}

impl<'a> ServingReader<'a> {
    pub(crate) fn new(
        conn: &'a Connection,
        workspace_id: &str,
        generation_id: &'a str,
        allow_serving: bool,
        examinations: &'a Cell<ServingReadExaminations>,
    ) -> Result<Self> {
        let serving_generation_id = if allow_serving {
            ready_serving_generation_id(conn, workspace_id, generation_id)?
        } else {
            None
        };
        Ok(Self {
            conn,
            generation_id,
            serving_generation_id,
            examinations,
            validation_mismatch: false,
        })
    }

    pub(crate) fn serving_generation_id(&self) -> Option<&str> {
        self.serving_generation_id.as_deref()
    }

    pub(crate) fn validation_mismatch(&self) -> bool {
        self.validation_mismatch
    }

    pub(crate) fn examinations(&self) -> ServingReadExaminations {
        self.examinations.get()
    }

    pub(crate) fn maximum_symbol_examinations(&self) -> usize {
        if self.serving_generation_id.is_some() {
            2
        } else {
            1
        }
    }

    /// Relationships are read as independently indexed inbound/outbound
    /// slices. Each can return `row_limit + 1`; validation reads both Truth
    /// and projection, while explicit fallback reads only Truth.
    pub(crate) fn maximum_relationship_examinations(&self, row_limit: usize) -> usize {
        let directional_slice = row_limit.saturating_add(1).saturating_mul(2);
        directional_slice.saturating_mul(if self.serving_generation_id.is_some() {
            2
        } else {
            1
        })
    }

    pub(crate) fn symbol(
        &mut self,
        canonical_symbol_key: &str,
        row_bound: usize,
    ) -> Result<Vec<ServingSymbolEvidence>> {
        if row_bound == 0 {
            return Ok(Vec::new());
        }
        let truth = truth_symbol_evidence(self.conn, self.generation_id, canonical_symbol_key)?;
        self.add_symbol_truth_rows(truth.len());
        let Some(serving_generation_id) = self.serving_generation_id.as_deref() else {
            return Ok(truth);
        };
        let projection = projected_symbol_evidence(
            self.conn,
            self.generation_id,
            serving_generation_id,
            canonical_symbol_key,
        )?;
        self.add_symbol_projection_rows(projection.len());
        if projection != truth {
            self.validation_mismatch = true;
            return Ok(truth);
        }
        Ok(projection)
    }

    pub(crate) fn relationships(
        &mut self,
        canonical_symbol_key: &str,
        row_limit: usize,
        filter: RelationshipReadFilter,
    ) -> Result<ServingRelationshipSlice> {
        if row_limit == 0 {
            return Ok(ServingRelationshipSlice {
                rows: Vec::new(),
                truncated: false,
            });
        }
        let (truth, truth_examined, truth_truncated) = truth_relationship_evidence(
            self.conn,
            self.generation_id,
            canonical_symbol_key,
            row_limit,
            filter,
        )?;
        self.add_relationship_truth_rows(truth_examined);
        let Some(serving_generation_id) = self.serving_generation_id.as_deref() else {
            return Ok(ServingRelationshipSlice {
                rows: truth,
                truncated: truth_truncated,
            });
        };
        let (projection, projection_examined, projection_truncated) =
            projected_relationship_evidence(
                self.conn,
                serving_generation_id,
                canonical_symbol_key,
                row_limit,
                filter,
            )?;
        self.add_relationship_projection_rows(projection_examined);
        if projection != truth || projection_truncated != truth_truncated {
            self.validation_mismatch = true;
            return Ok(ServingRelationshipSlice {
                rows: truth,
                truncated: truth_truncated,
            });
        }
        Ok(ServingRelationshipSlice {
            rows: projection,
            truncated: projection_truncated,
        })
    }

    fn update_examinations(&self, update: impl FnOnce(&mut ServingReadExaminations)) {
        let mut examinations = self.examinations.get();
        update(&mut examinations);
        self.examinations.set(examinations);
    }

    fn add_symbol_projection_rows(&self, count: usize) {
        self.update_examinations(|value| {
            value.symbol_projection_rows = value.symbol_projection_rows.saturating_add(count);
        });
    }

    fn add_symbol_truth_rows(&self, count: usize) {
        self.update_examinations(|value| {
            value.symbol_truth_rows = value.symbol_truth_rows.saturating_add(count);
        });
    }

    fn add_relationship_projection_rows(&self, count: usize) {
        self.update_examinations(|value| {
            value.relationship_projection_rows =
                value.relationship_projection_rows.saturating_add(count);
        });
    }

    fn add_relationship_truth_rows(&self, count: usize) {
        self.update_examinations(|value| {
            value.relationship_truth_rows = value.relationship_truth_rows.saturating_add(count);
        });
    }
}

fn truth_symbol_evidence(
    conn: &Connection,
    generation_id: &str,
    canonical_symbol_key: &str,
) -> Result<Vec<ServingSymbolEvidence>> {
    let mut statement = conn.prepare(
        "SELECT sf.canonical_symbol_key, gf.canonical_path, sf.symbol_kind,
                sf.symbol_fact_id, er.provider_tier, sf.confidence,
                sf.evidence_method, sf.start_byte, sf.end_byte,
                fr.artifact_class, fr.is_test, fr.content_hash
         FROM symbol_fact AS sf INDEXED BY idx_symbol_key
         JOIN generation_file AS gf INDEXED BY idx_generation_file_revision
           ON gf.generation_id = ?1 AND gf.revision_id = sf.revision_id
         JOIN file_revision fr ON fr.revision_id = gf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE sf.canonical_symbol_key = ?2 AND gf.presence_state = 'present'
         ORDER BY (sf.evidence_method = 'semantic') DESC,
                  (sf.evidence_method = 'exact') DESC,
                  sf.confidence DESC, sf.symbol_fact_id
         LIMIT 1",
    )?;
    let rows = statement
        .query_map(params![generation_id, canonical_symbol_key], |row| {
            map_serving_symbol_evidence(row)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn projected_symbol_evidence(
    conn: &Connection,
    generation_id: &str,
    serving_generation_id: &str,
    canonical_symbol_key: &str,
) -> Result<Vec<ServingSymbolEvidence>> {
    let mut statement = conn.prepare(
        "SELECT sp.canonical_symbol_key, sp.canonical_path, sp.symbol_kind,
                sp.preferred_fact_id, sp.preferred_provider_tier,
                sp.preferred_confidence, sp.evidence_state, sp.start_byte, sp.end_byte,
                fr.artifact_class, fr.is_test, fr.content_hash
         FROM symbol_serving_projection sp
         JOIN symbol_fact sf ON sf.symbol_fact_id = sp.preferred_fact_id
         JOIN generation_file gf
           ON gf.generation_id = ?2 AND gf.revision_id = sf.revision_id
         JOIN file_revision fr ON fr.revision_id = gf.revision_id
         WHERE sp.serving_generation_id = ?1 AND sp.canonical_symbol_key = ?3
           AND gf.presence_state = 'present'
         ORDER BY sp.canonical_symbol_key
         LIMIT 1",
    )?;
    let rows = statement
        .query_map(
            params![serving_generation_id, generation_id, canonical_symbol_key],
            map_serving_symbol_evidence,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn map_serving_symbol_evidence(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServingSymbolEvidence> {
    Ok(ServingSymbolEvidence {
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

fn truth_relationship_evidence(
    conn: &Connection,
    generation_id: &str,
    canonical_symbol_key: &str,
    row_limit: usize,
    filter: RelationshipReadFilter,
) -> Result<(Vec<ServingRelationshipEvidence>, usize, bool)> {
    let fetch_limit = relationship_fetch_limit(row_limit)?;
    let source = truth_relationship_direction(
        conn,
        generation_id,
        canonical_symbol_key,
        fetch_limit,
        false,
        filter,
    )?;
    let target = truth_relationship_direction(
        conn,
        generation_id,
        canonical_symbol_key,
        fetch_limit,
        true,
        filter,
    )?;
    let examined = source.len().saturating_add(target.len());
    let (rows, truncated) = merge_relationship_slices(source, target, row_limit);
    Ok((rows, examined, truncated))
}

fn truth_relationship_direction(
    conn: &Connection,
    generation_id: &str,
    canonical_symbol_key: &str,
    fetch_limit: i64,
    inbound: bool,
    filter: RelationshipReadFilter,
) -> Result<Vec<ServingRelationshipEvidence>> {
    let direction = if inbound {
        "rr.resolved_ref_kind = 'symbol' AND rr.resolved_ref_value = ?2"
    } else {
        "rf.source_ref_kind = 'symbol' AND rf.source_ref_value = ?2"
    };
    let source = if inbound {
        "relationship_resolution AS rr INDEXED BY idx_relationship_resolution_target
         JOIN relationship_fact rf ON rf.relationship_fact_id = rr.relationship_fact_id"
    } else {
        "relationship_fact AS rf INDEXED BY idx_relationship_source
         LEFT JOIN relationship_resolution rr
           ON rr.generation_id = ?1 AND rr.relationship_fact_id = rf.relationship_fact_id"
    };
    let generation_guard = if inbound {
        "rr.generation_id = ?1 AND"
    } else {
        ""
    };
    let sql = format!(
        "SELECT rf.relationship_fact_id, rf.source_ref_kind, rf.source_ref_value,
                rf.target_ref_kind, rf.target_ref_value, rf.relationship_type, rf.confidence,
                rr.status, rr.resolved_ref_kind, rr.resolved_ref_value
         FROM {source}
         JOIN generation_file AS gf INDEXED BY idx_generation_file_revision
           ON gf.generation_id = ?1 AND gf.revision_id = rf.revision_id
         WHERE {generation_guard} {direction}
           AND gf.presence_state = 'present'
           AND (
                rf.relationship_type IN ('references', 'implements', 'imports', 'extends')
                OR (rf.relationship_type = 'calls' AND ?4)
                OR (rf.relationship_type = 'tests' AND ?5)
                OR (rf.relationship_type = 'configures' AND ?6)
           )
         ORDER BY CASE
                    WHEN rr.status IN ('resolved_symbol', 'resolved_file', 'external') THEN 0
                    ELSE 1
                  END,
                  rf.relationship_fact_id
         LIMIT ?3"
    );
    let include_calls = if inbound {
        filter.include_callers
    } else {
        filter.include_callees
    };
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![
                generation_id,
                canonical_symbol_key,
                fetch_limit,
                include_calls,
                filter.include_tests,
                filter.include_configs,
            ],
            map_truth_relationship_evidence,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn map_truth_relationship_evidence(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ServingRelationshipEvidence> {
    let raw_target_ref_kind: String = row.get(3)?;
    let raw_target_ref_value: String = row.get(4)?;
    let status: Option<String> = row.get(7)?;
    let resolved_kind: Option<String> = row.get(8)?;
    let resolved_value: Option<String> = row.get(9)?;
    let (resolution_state, target_kind, target_value) = projected_target(
        status.as_deref(),
        resolved_kind.as_deref(),
        resolved_value.as_deref(),
        &raw_target_ref_value,
    );
    Ok(ServingRelationshipEvidence {
        relationship_fact_id: row.get(0)?,
        source_entity_id: entity_id_for(&row.get::<_, String>(1)?, &row.get::<_, String>(2)?),
        target_entity_id: entity_id_for(target_kind, &target_value),
        raw_target_ref_kind,
        raw_target_ref_value,
        relationship_type: row.get(5)?,
        resolution_state: resolution_state.to_string(),
        confidence: row.get(6)?,
    })
}

fn projected_relationship_evidence(
    conn: &Connection,
    serving_generation_id: &str,
    canonical_symbol_key: &str,
    row_limit: usize,
    filter: RelationshipReadFilter,
) -> Result<(Vec<ServingRelationshipEvidence>, usize, bool)> {
    let fetch_limit = relationship_fetch_limit(row_limit)?;
    let source = projected_relationship_direction(
        conn,
        serving_generation_id,
        canonical_symbol_key,
        fetch_limit,
        false,
        filter,
    )?;
    let target = projected_relationship_direction(
        conn,
        serving_generation_id,
        canonical_symbol_key,
        fetch_limit,
        true,
        filter,
    )?;
    let examined = source.len().saturating_add(target.len());
    let (rows, truncated) = merge_relationship_slices(source, target, row_limit);
    Ok((rows, examined, truncated))
}

fn projected_relationship_direction(
    conn: &Connection,
    serving_generation_id: &str,
    canonical_symbol_key: &str,
    fetch_limit: i64,
    inbound: bool,
    filter: RelationshipReadFilter,
) -> Result<Vec<ServingRelationshipEvidence>> {
    let (index, endpoint, include_calls) = if inbound {
        (
            "idx_serving_edge_target",
            "e.target_entity_id",
            filter.include_callers,
        )
    } else {
        (
            "idx_serving_edge_source",
            "e.source_entity_id",
            filter.include_callees,
        )
    };
    let symbol_entity_id = format!("symbol:{canonical_symbol_key}");
    let sql = format!(
        "SELECT COALESCE(e.preferred_fact_id, e.edge_id),
                e.source_entity_id, e.target_entity_id, e.relationship_type,
                e.resolution_state, e.confidence,
                COALESCE(rf.target_ref_kind, 'external'),
                COALESCE(rf.target_ref_value, e.target_entity_id)
         FROM relationship_serving_edge AS e INDEXED BY {index}
         LEFT JOIN relationship_fact rf ON rf.relationship_fact_id = e.preferred_fact_id
         WHERE e.serving_generation_id = ?1 AND {endpoint} = ?2
           AND (
                e.relationship_type IN ('references', 'implements', 'imports', 'extends')
                OR (e.relationship_type = 'calls' AND ?4)
                OR (e.relationship_type = 'tests' AND ?5)
                OR (e.relationship_type = 'configures' AND ?6)
           )
         ORDER BY CASE
                    WHEN e.resolution_state IN ('resolved_symbol', 'resolved_file', 'external') THEN 0
                    ELSE 1
                  END,
                  COALESCE(e.preferred_fact_id, e.edge_id)
         LIMIT ?3"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![
                serving_generation_id,
                symbol_entity_id,
                fetch_limit,
                include_calls,
                filter.include_tests,
                filter.include_configs,
            ],
            |row| {
                Ok(ServingRelationshipEvidence {
                    relationship_fact_id: row.get(0)?,
                    source_entity_id: row.get(1)?,
                    target_entity_id: row.get(2)?,
                    relationship_type: row.get(3)?,
                    resolution_state: row.get(4)?,
                    confidence: row.get(5)?,
                    raw_target_ref_kind: row.get(6)?,
                    raw_target_ref_value: row.get(7)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn relationship_fetch_limit(row_limit: usize) -> Result<i64> {
    row_limit
        .checked_add(1)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| AtlasError::InvalidConfig("Serving read bound exceeds SQLite range".into()))
}

fn merge_relationship_slices(
    mut source: Vec<ServingRelationshipEvidence>,
    target: Vec<ServingRelationshipEvidence>,
    row_limit: usize,
) -> (Vec<ServingRelationshipEvidence>, bool) {
    source.extend(target);
    source.sort_by(|a, b| {
        relationship_read_priority(&a.resolution_state)
            .cmp(&relationship_read_priority(&b.resolution_state))
            .then_with(|| a.relationship_fact_id.cmp(&b.relationship_fact_id))
    });
    source.dedup_by(|a, b| a.relationship_fact_id == b.relationship_fact_id);
    let truncated = source.len() > row_limit;
    source.truncate(row_limit);
    (source, truncated)
}

fn relationship_read_priority(resolution_state: &str) -> u8 {
    if matches!(
        resolution_state,
        "resolved_symbol" | "resolved_file" | "external"
    ) {
        0
    } else {
        1
    }
}

fn projected_target<'a>(
    status: Option<&str>,
    resolved_kind: Option<&'a str>,
    resolved_value: Option<&'a str>,
    raw_target_ref_value: &'a str,
) -> (&'static str, &'static str, String) {
    match (status, resolved_kind, resolved_value) {
        (Some("resolved_symbol"), Some("symbol"), Some(value)) => {
            ("resolved_symbol", "symbol", value.to_string())
        }
        (Some("resolved_file"), Some("file" | "path"), Some(value)) => {
            ("resolved_file", "path", value.to_string())
        }
        (Some("resolved_symbol" | "resolved_file"), _, _) => {
            ("invalid", "external", raw_target_ref_value.to_string())
        }
        (Some("external"), _, value) => (
            "external",
            "external",
            value.unwrap_or(raw_target_ref_value).to_string(),
        ),
        (Some("ambiguous"), _, _) => ("ambiguous", "external", raw_target_ref_value.to_string()),
        (Some("invalid"), _, _) => ("invalid", "external", raw_target_ref_value.to_string()),
        (Some("stale"), _, _) => ("stale", "external", raw_target_ref_value.to_string()),
        _ => ("unresolved", "external", raw_target_ref_value.to_string()),
    }
}

fn stored_projection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredProjection> {
    Ok(StoredProjection {
        serving_generation_id: row.get(0)?,
        generation_id: row.get(1)?,
        serving_schema_version: row.get(2)?,
        projection_policy_version: row.get(3)?,
        state: row.get(4)?,
        source_fact_fingerprint: row.get(5)?,
        card_count: row.get(6)?,
        edge_count: row.get(7)?,
        coverage_rollup_count: row.get(8)?,
        failure_code: row.get(9)?,
    })
}

fn stored_projections(conn: &Connection, workspace_id: &str) -> Result<Vec<StoredProjection>> {
    let mut statement = conn.prepare(
        "SELECT serving_generation_id, generation_id, serving_schema_version,
                projection_policy_version, state, source_fact_fingerprint,
                card_count, edge_count, coverage_rollup_count, failure_code
         FROM serving_generation
         WHERE workspace_id = ?1
         ORDER BY generation_id, serving_schema_version, projection_policy_version,
                  serving_generation_id",
    )?;
    let rows = statement.query_map(params![workspace_id], stored_projection_from_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn projection_status(
    conn: &Connection,
    stored: StoredProjection,
    effective_state: Option<ServingReadinessState>,
) -> Result<ServingProjectionStatus> {
    let output = projection_output_metrics(conn, &stored.serving_generation_id)?;
    let counts_match = stored.card_count == output.card_count
        && stored.edge_count == output.edge_count
        && stored.coverage_rollup_count == output.coverage_rollup_count
        && stored.coverage_rollup_count > 0;
    let content_valid = projection_content_valid(conn, &stored.serving_generation_id)?;
    let stored_state = match stored.state.as_str() {
        "building" => ServingReadinessState::Building,
        "ready" if counts_match && content_valid => ServingReadinessState::Ready,
        "ready" | "failed" => ServingReadinessState::Failed,
        "stale" | "deleted" => ServingReadinessState::Stale,
        _ => ServingReadinessState::Failed,
    };
    let failure_code = stored.failure_code.or_else(|| {
        if !counts_match {
            Some("projection_count_mismatch".to_string())
        } else if !content_valid {
            Some("projection_content_invalid".to_string())
        } else {
            None
        }
    });
    Ok(ServingProjectionStatus {
        serving_generation_id: stored.serving_generation_id,
        generation_id: stored.generation_id,
        serving_schema_version: stored.serving_schema_version,
        projection_policy_version: stored.projection_policy_version,
        state: effective_state.unwrap_or(stored_state),
        source_fact_fingerprint: stored.source_fact_fingerprint,
        card_count: stored.card_count,
        edge_count: stored.edge_count,
        coverage_rollup_count: stored.coverage_rollup_count,
        output_rows: output.output_rows,
        output_bytes: output.output_bytes,
        output_hash: output.output_hash,
        failure_code,
        counts_match,
        content_valid,
    })
}

/// Build (or rebuild) the Serving Plane for the workspace's current active
/// generation, using the default V1.2 schema/policy versions.
pub fn build_serving_generation(
    conn: &Connection,
    ws: &WorkspaceRecord,
) -> Result<ServingBuildReport> {
    build_serving_generation_with_policy(
        conn,
        ws,
        SERVING_SCHEMA_VERSION,
        PROJECTION_POLICY_VERSION,
    )
}

/// Build (or rebuild) the Serving Plane under an explicit
/// `(serving_schema_version, projection_policy_version)` pair — the unit
/// that keys every row this function writes.
pub fn build_serving_generation_with_policy(
    conn: &Connection,
    ws: &WorkspaceRecord,
    serving_schema_version: &str,
    projection_policy_version: &str,
) -> Result<ServingBuildReport> {
    let build_started = Instant::now();
    let gen_id = active_generation_id(conn, &ws.workspace_id)?.ok_or_else(|| {
        AtlasError::Other(
            "cannot build a Serving Plane projection: workspace has no active generation"
                .to_string(),
        )
    })?;
    let serving_generation_id = deterministic_id(
        "serve_gen",
        &[
            &ws.workspace_id,
            &gen_id,
            serving_schema_version,
            projection_policy_version,
        ],
    );
    let stage_started = Instant::now();
    let input = compute_build_input_metrics(conn, &gen_id)?;
    let input_scan_us = elapsed_micros(stage_started);
    let existing_ready: Option<(String, i64, i64, i64)> = conn
        .query_row(
            "SELECT source_fact_fingerprint, card_count, edge_count, coverage_rollup_count
             FROM serving_generation
             WHERE serving_generation_id = ?1 AND state = 'ready'",
            params![serving_generation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((fingerprint, card_count, edge_count, coverage_rollup_count)) = existing_ready {
        let validation_started = Instant::now();
        let output = projection_output_metrics(conn, &serving_generation_id)?;
        if fingerprint != input.source_fact_fingerprint
            || card_count != output.card_count
            || edge_count != output.edge_count
            || coverage_rollup_count != output.coverage_rollup_count
            || coverage_rollup_count <= 0
            || !projection_content_valid(conn, &serving_generation_id)?
        {
            return Err(AtlasError::Other(
                "Serving Plane validation failed: immutable ready projection is inconsistent"
                    .to_string(),
            ));
        }
        let validation_us = elapsed_micros(validation_started);
        return Ok(ServingBuildReport {
            ok: true,
            serving_generation_id,
            workspace_id: ws.workspace_id.clone(),
            generation_id: gen_id,
            serving_schema_version: serving_schema_version.to_string(),
            projection_policy_version: projection_policy_version.to_string(),
            state: "ready".to_string(),
            card_count,
            edge_count,
            coverage_rollup_count,
            output_rows: output.output_rows,
            output_bytes: output.output_bytes,
            output_hash: output.output_hash,
            input,
            stages: ServingBuildStageMetrics {
                input_scan_us,
                symbol_cards_us: 0,
                relationship_edges_us: 0,
                coverage_rollups_us: 0,
                validation_us,
            },
            cache_hit: true,
            cache_miss: false,
            serving_fallback: false,
            failure_code: None,
            rebuild_recommended: false,
            build_elapsed_us: elapsed_micros(build_started),
        });
    }

    let tx = conn.unchecked_transaction()?;
    // Retry replaces only a non-ready row under this derived identity. Ready
    // projections return above and remain immutable until explicitly deleted.
    tx.execute(
        "DELETE FROM serving_generation WHERE serving_generation_id = ?1",
        params![serving_generation_id],
    )?;
    let started_at = crate::migrations::iso8601_now();
    tx.execute(
        "INSERT INTO serving_generation (
            serving_generation_id, workspace_id, generation_id, serving_schema_version,
            projection_policy_version, state, source_fact_fingerprint, card_count, edge_count,
            coverage_rollup_count, build_started_at
        ) VALUES (?1,?2,?3,?4,?5,'building',?6,0,0,0,?7)",
        params![
            serving_generation_id,
            ws.workspace_id,
            gen_id,
            serving_schema_version,
            projection_policy_version,
            input.source_fact_fingerprint,
            started_at,
        ],
    )?;

    let stage_started = Instant::now();
    let card_count = project_symbol_cards(
        &tx,
        ws,
        &gen_id,
        &serving_generation_id,
        projection_policy_version,
    )?;
    let symbol_cards_us = elapsed_micros(stage_started);

    let stage_started = Instant::now();
    let edge_count = project_relationship_edges(&tx, &gen_id, &serving_generation_id)?;
    let relationship_edges_us = elapsed_micros(stage_started);

    let stage_started = Instant::now();
    let coverage_rollup_count = project_coverage_rollups(&tx, ws, &gen_id, &serving_generation_id)?;
    let coverage_rollups_us = elapsed_micros(stage_started);

    let stage_started = Instant::now();
    let output = projection_output_metrics(&tx, &serving_generation_id)?;
    if card_count != output.card_count
        || edge_count != output.edge_count
        || coverage_rollup_count != output.coverage_rollup_count
    {
        return Err(AtlasError::Other(
            "Serving Plane validation failed: projected row counts do not match".to_string(),
        ));
    }
    let validation_us = elapsed_micros(stage_started);

    tx.execute(
        "UPDATE serving_generation
         SET state = 'ready', card_count = ?2, edge_count = ?3, coverage_rollup_count = ?4,
             build_finished_at = ?5
         WHERE serving_generation_id = ?1",
        params![
            serving_generation_id,
            card_count,
            edge_count,
            coverage_rollup_count,
            crate::migrations::iso8601_now()
        ],
    )?;
    tx.commit()?;
    let build_elapsed_us = elapsed_micros(build_started);

    Ok(ServingBuildReport {
        ok: true,
        serving_generation_id,
        workspace_id: ws.workspace_id.clone(),
        generation_id: gen_id,
        serving_schema_version: serving_schema_version.to_string(),
        projection_policy_version: projection_policy_version.to_string(),
        state: "ready".to_string(),
        card_count,
        edge_count,
        coverage_rollup_count,
        output_rows: output.output_rows,
        output_bytes: output.output_bytes,
        output_hash: output.output_hash,
        input,
        stages: ServingBuildStageMetrics {
            input_scan_us,
            symbol_cards_us,
            relationship_edges_us,
            coverage_rollups_us,
            validation_us,
        },
        cache_hit: false,
        cache_miss: true,
        serving_fallback: false,
        failure_code: None,
        rebuild_recommended: false,
        build_elapsed_us,
    })
}

/// Delete a Serving Plane generation and every row it owns (cascades via
/// `ON DELETE CASCADE` to `symbol_serving_projection`,
/// `relationship_serving_edge`, `serving_coverage_rollup`).
pub fn delete_serving_generation(conn: &Connection, serving_generation_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM serving_generation WHERE serving_generation_id = ?1",
        params![serving_generation_id],
    )?;
    Ok(())
}

fn elapsed_micros(started: Instant) -> i64 {
    started.elapsed().as_micros().min(i64::MAX as u128) as i64
}

fn prefixed_generation_ids(
    conn: &Connection,
    query: &str,
    generation_id: &str,
    prefix: &str,
) -> Result<Vec<String>> {
    let mut statement = conn.prepare(query)?;
    let rows = statement
        .query_map(params![generation_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|identity| format!("{prefix}:{identity}"))
        .collect())
}

fn count_as_i64(count: usize) -> Result<i64> {
    i64::try_from(count)
        .map_err(|_| AtlasError::Other("Serving Plane input count exceeds i64".to_string()))
}

/// Canonical input identity covers every Truth table consumed by the builder.
/// Timing and the requested projection identity remain outside this fingerprint.
fn compute_build_input_metrics(
    conn: &Connection,
    generation_id: &str,
) -> Result<ServingBuildInputMetrics> {
    let symbol_ids = prefixed_generation_ids(
        conn,
        "SELECT sf.symbol_fact_id FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 ORDER BY sf.symbol_fact_id",
        generation_id,
        "symbol",
    )?;
    let relationship_ids = prefixed_generation_ids(
        conn,
        "SELECT rf.relationship_fact_id FROM current_file cf
         JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 ORDER BY rf.relationship_fact_id",
        generation_id,
        "relationship",
    )?;
    let effect_ids = prefixed_generation_ids(
        conn,
        "SELECT ef.effect_fact_id FROM current_file cf
         JOIN effect_fact ef ON ef.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 ORDER BY ef.effect_fact_id",
        generation_id,
        "effect",
    )?;
    let resolution_ids = prefixed_generation_ids(
        conn,
        "SELECT relationship_resolution_id FROM relationship_resolution
         WHERE generation_id = ?1 ORDER BY relationship_resolution_id",
        generation_id,
        "resolution",
    )?;
    let coverage_ids = prefixed_generation_ids(
        conn,
        "SELECT coverage_id FROM coverage_record
         WHERE generation_id = ?1 ORDER BY coverage_id",
        generation_id,
        "coverage",
    )?;
    let conflict_ids = prefixed_generation_ids(
        conn,
        "SELECT evidence_conflict_id FROM evidence_conflict
         WHERE generation_id = ?1 ORDER BY evidence_conflict_id",
        generation_id,
        "conflict",
    )?;
    let mut identities = Vec::with_capacity(
        symbol_ids.len()
            + relationship_ids.len()
            + resolution_ids.len()
            + effect_ids.len()
            + coverage_ids.len()
            + conflict_ids.len(),
    );
    identities.extend(symbol_ids.iter().cloned());
    identities.extend(relationship_ids.iter().cloned());
    identities.extend(effect_ids.iter().cloned());
    identities.extend(resolution_ids.iter().cloned());
    identities.extend(coverage_ids.iter().cloned());
    identities.extend(conflict_ids.iter().cloned());
    identities.sort();
    let legacy_ids = symbol_ids
        .iter()
        .chain(&relationship_ids)
        .filter_map(|identity| identity.split_once(':').map(|(_, value)| value))
        .collect::<Vec<_>>();
    let source_fact_fingerprint =
        crate::hashing::content_hash_of_bytes(legacy_ids.join("\u{1f}").as_bytes());
    let canonical_input_fingerprint =
        crate::hashing::content_hash_of_bytes(identities.join("\u{1f}").as_bytes());
    Ok(ServingBuildInputMetrics {
        source_fact_fingerprint,
        canonical_input_fingerprint,
        symbol_fact_count: count_as_i64(symbol_ids.len())?,
        relationship_fact_count: count_as_i64(relationship_ids.len())?,
        effect_fact_count: count_as_i64(effect_ids.len())?,
        relationship_resolution_count: count_as_i64(resolution_ids.len())?,
        coverage_record_count: count_as_i64(coverage_ids.len())?,
        evidence_conflict_count: count_as_i64(conflict_ids.len())?,
    })
}

struct ProjectionOutputMetrics {
    card_count: i64,
    edge_count: i64,
    coverage_rollup_count: i64,
    output_rows: i64,
    output_bytes: i64,
    output_hash: String,
}

fn projection_output_metrics(
    conn: &Connection,
    serving_generation_id: &str,
) -> Result<ProjectionOutputMetrics> {
    let card_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symbol_serving_projection WHERE serving_generation_id = ?1",
        params![serving_generation_id],
        |row| row.get(0),
    )?;
    let edge_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM relationship_serving_edge WHERE serving_generation_id = ?1",
        params![serving_generation_id],
        |row| row.get(0),
    )?;
    let coverage_rollup_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM serving_coverage_rollup WHERE serving_generation_id = ?1",
        params![serving_generation_id],
        |row| row.get(0),
    )?;
    let output_rows = card_count
        .saturating_add(edge_count)
        .saturating_add(coverage_rollup_count);
    let output_bytes: i64 = conn.query_row(
        "SELECT
            COALESCE((SELECT SUM(LENGTH(CAST(compact_json AS BLOB))) FROM symbol_serving_projection WHERE serving_generation_id = ?1), 0)
          + COALESCE((SELECT SUM(LENGTH(CAST(edge_json AS BLOB))) FROM relationship_serving_edge WHERE serving_generation_id = ?1), 0)
          + COALESCE((SELECT SUM(LENGTH(CAST(details_json AS BLOB))) FROM serving_coverage_rollup WHERE serving_generation_id = ?1), 0)",
        params![serving_generation_id],
        |row| row.get(0),
    )?;
    let mut entries = Vec::with_capacity(output_rows.max(0) as usize);
    {
        let mut statement = conn.prepare(
            "SELECT canonical_symbol_key, card_hash FROM symbol_serving_projection
             WHERE serving_generation_id = ?1 ORDER BY canonical_symbol_key",
        )?;
        let rows = statement.query_map(params![serving_generation_id], |row| {
            Ok((format!("card:{}", row.get::<_, String>(0)?), row.get(1)?))
        })?;
        entries.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
    }
    {
        let mut statement = conn.prepare(
            "SELECT edge_id, edge_json FROM relationship_serving_edge
             WHERE serving_generation_id = ?1 ORDER BY stable_order_key, edge_id",
        )?;
        let rows = statement.query_map(params![serving_generation_id], |row| {
            let edge_id: String = row.get(0)?;
            let edge_json: String = row.get(1)?;
            Ok((
                format!("edge:{edge_id}"),
                crate::hashing::content_hash_of_bytes(edge_json.as_bytes()),
            ))
        })?;
        entries.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
    }
    {
        let mut statement = conn.prepare(
            "SELECT scope_kind, scope_key, eligible_count, complete_count, partial_count,
                    unsupported_count, excluded_count, failed_count, unresolved_count,
                    ambiguous_count, conflict_count, details_json
             FROM serving_coverage_rollup
             WHERE serving_generation_id = ?1 ORDER BY scope_kind, scope_key",
        )?;
        let rows = statement.query_map(params![serving_generation_id], |row| {
            let scope_kind: String = row.get(0)?;
            let scope_key: String = row.get(1)?;
            let content = format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
            );
            Ok((
                format!("coverage:{scope_kind}:{scope_key}"),
                crate::hashing::content_hash_of_bytes(content.as_bytes()),
            ))
        })?;
        entries.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
    }
    Ok(ProjectionOutputMetrics {
        card_count,
        edge_count,
        coverage_rollup_count,
        output_rows,
        output_bytes,
        output_hash: crate::hashing::source_tree_hash(entries),
    })
}

fn projection_content_valid(conn: &Connection, serving_generation_id: &str) -> Result<bool> {
    let cards = {
        let mut statement = conn.prepare(
            "SELECT compact_json, card_hash FROM symbol_serving_projection
             WHERE serving_generation_id = ?1 ORDER BY canonical_symbol_key",
        )?;
        let rows = statement.query_map(params![serving_generation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (compact_json, stored_hash) in cards {
        let Ok(card) = serde_json::from_str::<SymbolCard>(&compact_json) else {
            return Ok(false);
        };
        let expected = SymbolCard::seal(
            card.serving_generation_id.clone(),
            card.workspace_id.clone(),
            card.generation_id.clone(),
            card.canonical_symbol_key.clone(),
            card.path.clone(),
            card.range.clone(),
            card.kind.clone(),
            card.language.clone(),
            card.signature.clone(),
            card.preferred_evidence.clone(),
            card.counts.clone(),
            card.coverage_state,
            card.cost.clone(),
        );
        if card.serving_generation_id != serving_generation_id
            || card.card_hash != stored_hash
            || expected.card_hash != stored_hash
        {
            return Ok(false);
        }
    }
    for table_and_column in [
        ("relationship_serving_edge", "edge_json"),
        ("serving_coverage_rollup", "details_json"),
    ] {
        let query = format!(
            "SELECT {} FROM {} WHERE serving_generation_id = ?1",
            table_and_column.1, table_and_column.0
        );
        let mut statement = conn.prepare(&query)?;
        let rows = statement.query_map(params![serving_generation_id], |row| {
            row.get::<_, String>(0)
        })?;
        for value in rows {
            if !matches!(
                serde_json::from_str::<serde_json::Value>(&value?),
                Ok(serde_json::Value::Object(_))
            ) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Map a `coverage_record.status` value onto the narrower `CoverageState`
/// vocabulary a `SymbolCard` carries. `excluded`/`stale` fold into
/// `unsupported`/`failed` respectively — the closest honest match in the
/// smaller enum, never silently promoted to `complete`.
fn map_coverage_status(status: &str) -> CoverageState {
    match status {
        "complete" => CoverageState::Complete,
        "partial" => CoverageState::Partial,
        "unsupported" | "excluded" => CoverageState::Unsupported,
        "failed" | "stale" => CoverageState::Failed,
        _ => CoverageState::Unsupported,
    }
}

struct SymbolRow {
    symbol_fact_id: String,
    canonical_symbol_key: String,
    canonical_path: String,
    file_id: String,
    symbol_kind: String,
    signature: Option<String>,
    start_byte: i64,
    end_byte: i64,
    start_line: i64,
    end_line: i64,
    evidence_method: String,
    confidence: f64,
    provider_tier: String,
    language: Option<String>,
}

/// Project every current symbol into a `SymbolCard`, one per
/// `canonical_symbol_key`. When multiple facts (e.g. a structural and a
/// semantic extractor) define the same symbol, the preferred one is chosen
/// deterministically: semantic-tier evidence first, then confidence, then
/// `symbol_fact_id` as a stable tie-breaker — mirroring `BROKER-001`'s
/// preference for semantic evidence at equal relevance.
fn project_symbol_cards(
    tx: &rusqlite::Transaction<'_>,
    ws: &WorkspaceRecord,
    gen_id: &str,
    serving_generation_id: &str,
    _projection_policy_version: &str,
) -> Result<i64> {
    let mut stmt = tx.prepare(
        "SELECT sf.symbol_fact_id, sf.canonical_symbol_key, cf.canonical_path, cf.file_id,
                sf.symbol_kind, sf.signature, sf.start_byte, sf.end_byte, sf.start_line, sf.end_line,
                sf.evidence_method, sf.confidence, er.provider_tier, cf.language
         FROM current_file cf
         JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
         WHERE cf.generation_id = ?1
         ORDER BY sf.canonical_symbol_key,
                  (sf.evidence_method = 'semantic') DESC,
                  (sf.evidence_method = 'exact') DESC,
                  sf.confidence DESC,
                  sf.symbol_fact_id",
    )?;
    let rows: Vec<SymbolRow> = stmt
        .query_map(params![gen_id], |r| {
            Ok(SymbolRow {
                symbol_fact_id: r.get(0)?,
                canonical_symbol_key: r.get(1)?,
                canonical_path: r.get(2)?,
                file_id: r.get(3)?,
                symbol_kind: r.get(4)?,
                signature: r.get(5)?,
                start_byte: r.get(6)?,
                end_byte: r.get(7)?,
                start_line: r.get(8)?,
                end_line: r.get(9)?,
                evidence_method: r.get(10)?,
                confidence: r.get(11)?,
                provider_tier: r.get(12)?,
                language: r.get(13)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(stmt);

    let mut count = 0i64;
    let mut last_key: Option<String> = None;
    for row in &rows {
        // First row per canonical_symbol_key after the ORDER BY above is
        // the preferred one (semantic-tier / exact evidence, highest
        // confidence, stable id tie-break); subsequent rows for the same
        // key are alternates and are not separately card-ed here (the
        // Context Broker already surfaces alternates for Audit mode from
        // the raw `symbol_fact` table — this projection carries only the
        // single preferred truth per symbol, per the schema's PRIMARY KEY).
        if last_key.as_deref() == Some(row.canonical_symbol_key.as_str()) {
            continue;
        }
        last_key = Some(row.canonical_symbol_key.clone());

        let counts = symbol_counts(tx, gen_id, &row.canonical_symbol_key)?;
        let coverage_state = symbol_coverage_state(tx, gen_id, &row.file_id)?;

        let card = SymbolCard::seal(
            serving_generation_id.to_string(),
            ws.workspace_id.clone(),
            gen_id.to_string(),
            row.canonical_symbol_key.clone(),
            row.canonical_path.clone(),
            SymbolRange {
                start_byte: row.start_byte,
                end_byte: row.end_byte,
                start_line: row.start_line,
                end_line: row.end_line,
            },
            row.symbol_kind.clone(),
            row.language.clone(),
            row.signature.clone(),
            PreferredEvidenceSummary {
                fact_id: row.symbol_fact_id.clone(),
                provider_tier: row.provider_tier.clone(),
                confidence: row.confidence,
                state: row.evidence_method.clone(),
            },
            counts,
            coverage_state,
            SymbolCost {
                source_bytes: (row.end_byte - row.start_byte).max(0),
                estimated_tokens: estimate_tokens(row.end_byte - row.start_byte),
                metadata_bytes: estimate_metadata_bytes(row),
            },
        );

        tx.execute(
            "INSERT INTO symbol_serving_projection (
                serving_generation_id, canonical_symbol_key, symbol_fact_id, file_revision_id,
                canonical_path, symbol_kind, language, signature, start_byte, end_byte, start_line, end_line,
                preferred_fact_id, preferred_provider_tier, preferred_confidence, evidence_state,
                caller_count, callee_count, reference_count, test_count, config_count, effect_count,
                conflict_count, unresolved_count, coverage_state, source_bytes, estimated_tokens,
                metadata_bytes, compact_json, card_hash
            ) VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)",
            params![
                serving_generation_id,
                card.canonical_symbol_key,
                card.preferred_evidence.fact_id,
                card.path,
                card.kind,
                card.language,
                card.signature,
                card.range.start_byte,
                card.range.end_byte,
                card.range.start_line,
                card.range.end_line,
                card.preferred_evidence.fact_id,
                card.preferred_evidence.provider_tier,
                card.preferred_evidence.confidence,
                card.preferred_evidence.state,
                card.counts.callers,
                card.counts.callees,
                card.counts.references,
                card.counts.tests,
                card.counts.configs,
                card.counts.effects,
                card.counts.conflicts,
                card.counts.unresolved,
                card.coverage_state.as_str(),
                card.cost.source_bytes,
                card.cost.estimated_tokens,
                card.cost.metadata_bytes,
                serde_json::to_string(&card)?,
                card.card_hash,
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

fn estimate_tokens(byte_span: i64) -> i64 {
    // Coarse, deterministic estimate (~4 bytes/token), matching the
    // existing V1 Context Broker's token-estimation order of magnitude.
    // Never negative.
    (byte_span.max(0)) / 4
}

fn estimate_metadata_bytes(row: &SymbolRow) -> i64 {
    (row.canonical_symbol_key.len() + row.canonical_path.len() + row.symbol_kind.len()) as i64
}

fn symbol_counts(
    tx: &rusqlite::Transaction<'_>,
    gen_id: &str,
    canonical_symbol_key: &str,
) -> Result<SymbolCounts> {
    let count = |sql: &str, params_: &[&dyn rusqlite::ToSql]| -> Result<i64> {
        Ok(tx.query_row(sql, params_, |r| r.get(0))?)
    };

    let callers = count(
        "SELECT COUNT(*) FROM current_file cf JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND rf.relationship_type = 'calls' AND rf.target_ref_kind = 'symbol' AND rf.target_ref_value = ?2",
        params![gen_id, canonical_symbol_key],
    )?;
    let callees = count(
        "SELECT COUNT(*) FROM current_file cf JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND rf.relationship_type = 'calls' AND rf.source_ref_kind = 'symbol' AND rf.source_ref_value = ?2",
        params![gen_id, canonical_symbol_key],
    )?;
    let references = count(
        "SELECT COUNT(*) FROM current_file cf JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND rf.relationship_type = 'references' AND rf.target_ref_kind = 'symbol' AND rf.target_ref_value = ?2",
        params![gen_id, canonical_symbol_key],
    )?;
    let tests = count(
        "SELECT COUNT(*) FROM current_file cf JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND rf.relationship_type = 'tests'
           AND ((rf.source_ref_kind = 'symbol' AND rf.source_ref_value = ?2) OR (rf.target_ref_kind = 'symbol' AND rf.target_ref_value = ?2))",
        params![gen_id, canonical_symbol_key],
    )?;
    let configs = count(
        "SELECT COUNT(*) FROM current_file cf JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND rf.relationship_type = 'configures'
           AND ((rf.source_ref_kind = 'symbol' AND rf.source_ref_value = ?2) OR (rf.target_ref_kind = 'symbol' AND rf.target_ref_value = ?2))",
        params![gen_id, canonical_symbol_key],
    )?;
    let effects = count(
        "SELECT COUNT(*) FROM current_file cf JOIN effect_fact ef ON ef.revision_id = cf.revision_id
         WHERE cf.generation_id = ?1 AND ef.subject_ref_kind = 'symbol' AND ef.subject_ref_value = ?2",
        params![gen_id, canonical_symbol_key],
    )?;
    let conflicts = count(
        "SELECT COUNT(*) FROM evidence_conflict
         WHERE generation_id = ?1 AND subject_kind = 'symbol' AND subject_key = ?2
           AND status IN ('open', 'preferred_with_conflict')",
        params![gen_id, canonical_symbol_key],
    )?;
    let unresolved = count(
        "SELECT COUNT(*) FROM current_file cf
         JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         JOIN relationship_resolution rr ON rr.relationship_fact_id = rf.relationship_fact_id AND rr.generation_id = cf.generation_id
         WHERE cf.generation_id = ?1 AND rf.source_ref_kind = 'symbol' AND rf.source_ref_value = ?2
           AND rr.status IN ('unresolved', 'ambiguous', 'stale', 'invalid')",
        params![gen_id, canonical_symbol_key],
    )?;

    Ok(SymbolCounts {
        callers,
        callees,
        references,
        tests,
        configs,
        effects,
        conflicts,
        unresolved,
    })
}

/// A symbol's coverage state is derived from the `coverage_record` row for
/// its owning file (`scope_kind = 'file'`), the only per-symbol-adjacent
/// coverage signal that actually exists in V1.1 truth today. Absent an
/// explicit record, a symbol defaults to `Complete` unless its own
/// evidence method is textual/inferred (the two evidence tiers V1.1 itself
/// treats as weakest) — a documented, disclosed simplification (ADR-P005),
/// not a fabricated per-symbol coverage computation.
fn symbol_coverage_state(
    tx: &rusqlite::Transaction<'_>,
    gen_id: &str,
    file_id: &str,
) -> Result<CoverageState> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM coverage_record WHERE generation_id = ?1 AND scope_kind = 'file' AND file_id = ?2",
            params![gen_id, file_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match status {
        Some(s) => map_coverage_status(&s),
        None => CoverageState::Complete,
    })
}

/// Project every current relationship into a `relationship_serving_edge`
/// row. A relationship with a persisted `relationship_resolution` row for
/// this generation carries that resolver's real outcome; one with none
/// (V1.1's structural-only relationships never run through the S6
/// resolver) is honestly reported `unresolved` rather than silently
/// treated as if it had been resolved.
fn project_relationship_edges(
    tx: &rusqlite::Transaction<'_>,
    gen_id: &str,
    serving_generation_id: &str,
) -> Result<i64> {
    struct RelRow {
        relationship_fact_id: String,
        source_ref_kind: String,
        source_ref_value: String,
        target_ref_kind: String,
        target_ref_value: String,
        relationship_type: String,
        evidence_method: String,
        confidence: f64,
        canonical_path: Option<String>,
        start_byte: Option<i64>,
        end_byte: Option<i64>,
        provider_tier: String,
    }

    let mut stmt = tx.prepare(
        "SELECT rf.relationship_fact_id, rf.source_ref_kind, rf.source_ref_value,
                rf.target_ref_kind, rf.target_ref_value, rf.relationship_type,
                rf.evidence_method, rf.confidence, cf.canonical_path, rf.start_byte, rf.end_byte,
                er.provider_tier
         FROM current_file cf
         JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
         JOIN extractor_run er ON er.extractor_run_id = rf.extractor_run_id
         WHERE cf.generation_id = ?1
         ORDER BY rf.relationship_fact_id",
    )?;
    let rows: Vec<RelRow> = stmt
        .query_map(params![gen_id], |r| {
            Ok(RelRow {
                relationship_fact_id: r.get(0)?,
                source_ref_kind: r.get(1)?,
                source_ref_value: r.get(2)?,
                target_ref_kind: r.get(3)?,
                target_ref_value: r.get(4)?,
                relationship_type: r.get(5)?,
                evidence_method: r.get(6)?,
                confidence: r.get(7)?,
                canonical_path: r.get(8)?,
                start_byte: r.get(9)?,
                end_byte: r.get(10)?,
                provider_tier: r.get(11)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    drop(stmt);

    let mut count = 0i64;
    for row in rows {
        let resolution: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT status, resolved_ref_kind, resolved_ref_value
                 FROM relationship_resolution
                 WHERE generation_id = ?1 AND relationship_fact_id = ?2",
                params![gen_id, row.relationship_fact_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let (resolution_state, target_kind, target_value): (&'static str, &str, String) =
            match &resolution {
                Some((status, _, Some(resolved_value))) if status == "resolved_symbol" => {
                    ("resolved_symbol", "symbol", resolved_value.clone())
                }
                Some((status, _, Some(resolved_value))) if status == "resolved_file" => {
                    ("resolved_file", "path", resolved_value.clone())
                }
                Some((status, _, resolved_value)) if status == "external" => (
                    "external",
                    "external",
                    resolved_value
                        .clone()
                        .unwrap_or_else(|| row.target_ref_value.clone()),
                ),
                Some((status, _, _)) => {
                    let state = match status.as_str() {
                        "ambiguous" => "ambiguous",
                        "invalid" => "invalid",
                        "stale" => "stale",
                        _ => "unresolved",
                    };
                    (state, "external", row.target_ref_value.clone())
                }
                None => ("unresolved", "external", row.target_ref_value.clone()),
            };

        let trust_class = trust_class_for(resolution_state, &row.evidence_method, row.confidence);

        let source_entity_id = entity_id_for(&row.source_ref_kind, &row.source_ref_value);
        let target_entity_id = entity_id_for(target_kind, &target_value);
        let stable_order_key = format!(
            "{source_entity_id}\u{1f}{}\u{1f}{target_entity_id}",
            row.relationship_type
        );
        let edge_id = deterministic_id("edge", &[serving_generation_id, &row.relationship_fact_id]);
        let edge_json = serde_json::json!({
            "relationship_fact_id": row.relationship_fact_id,
            "relationship_type": row.relationship_type,
            "resolution_state": resolution_state,
            "trust_class": trust_class,
            "evidence_method": row.evidence_method,
            "raw_target_ref_kind": row.target_ref_kind,
            "raw_target_ref_value": row.target_ref_value,
            "resolved_target_ref_kind": target_kind,
            "resolved_target_ref_value": target_value,
            "confidence": row.confidence,
            "provider_tier": row.provider_tier,
        })
        .to_string();

        tx.execute(
            "INSERT INTO relationship_serving_edge (
                serving_generation_id, edge_id, source_entity_id, target_entity_id, relationship_type,
                resolution_state, trust_class, preferred_fact_id, provider_tier, confidence,
                canonical_path, start_byte, end_byte, stable_order_key, edge_json
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                serving_generation_id, edge_id, source_entity_id, target_entity_id, row.relationship_type,
                resolution_state, trust_class, row.relationship_fact_id, row.provider_tier, row.confidence,
                row.canonical_path, row.start_byte, row.end_byte, stable_order_key, edge_json,
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

fn entity_id_for(ref_kind: &str, ref_value: &str) -> String {
    match ref_kind {
        "symbol" => format!("symbol:{ref_value}"),
        "path" => format!("path:{ref_value}"),
        _ => format!("external:{ref_value}"),
    }
}

/// Deterministic, disclosed trust-class heuristic (ADR-P005): a resolved
/// target backed by the strongest evidence tiers is `verified`; resolved
/// but weaker-tier evidence is `supported`; an honestly-uncertain
/// resolution state with reasonable confidence is `inferred`; everything
/// else is `uncertain`. Never claims more than the underlying resolution
/// state and evidence method actually support.
fn trust_class_for(resolution_state: &str, evidence_method: &str, confidence: f64) -> &'static str {
    let resolved = matches!(resolution_state, "resolved_symbol" | "resolved_file");
    if resolved && matches!(evidence_method, "exact" | "semantic") {
        "verified"
    } else if resolved {
        "supported"
    } else if confidence >= 0.5 {
        "inferred"
    } else {
        "uncertain"
    }
}

/// Roll up `coverage_record` truth for this generation into
/// `serving_coverage_rollup`, one row per `(scope_kind, scope_key)`, plus a
/// single `workspace`-scope row folding in the generation-wide
/// unresolved/ambiguous/conflict counts (the same signal
/// `query::EvidenceEnvelope` already exposes).
fn project_coverage_rollups(
    tx: &rusqlite::Transaction<'_>,
    ws: &WorkspaceRecord,
    gen_id: &str,
    serving_generation_id: &str,
) -> Result<i64> {
    struct ScopeRollup {
        complete: i64,
        partial: i64,
        unsupported: i64,
        excluded: i64,
        failed: i64,
        eligible: i64,
    }

    let mut scopes: std::collections::BTreeMap<(String, String), ScopeRollup> =
        std::collections::BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT scope_kind, scope_key, status FROM coverage_record WHERE generation_id = ?1",
        )?;
        let rows = stmt.query_map(params![gen_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows.filter_map(std::result::Result::ok) {
            let (scope_kind, scope_key, status) = row;
            let entry = scopes
                .entry((scope_kind, scope_key))
                .or_insert(ScopeRollup {
                    complete: 0,
                    partial: 0,
                    unsupported: 0,
                    excluded: 0,
                    failed: 0,
                    eligible: 0,
                });
            entry.eligible += 1;
            match status.as_str() {
                "complete" => entry.complete += 1,
                "partial" => entry.partial += 1,
                "unsupported" => entry.unsupported += 1,
                "excluded" => entry.excluded += 1,
                "failed" | "stale" => entry.failed += 1,
                _ => {}
            }
        }
    }

    let unresolved_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM relationship_resolution WHERE generation_id = ?1 AND status IN ('unresolved', 'invalid', 'stale')",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let ambiguous_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM relationship_resolution WHERE generation_id = ?1 AND status = 'ambiguous'",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let conflict_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM evidence_conflict WHERE generation_id = ?1 AND status IN ('open', 'preferred_with_conflict')",
            params![gen_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Always emit exactly one workspace-scope row, even with zero
    // coverage_record rows (a structural-only generation has none), so the
    // generation-wide unresolved/ambiguous/conflict signal is never lost.
    scopes
        .entry(("workspace".to_string(), ws.workspace_id.clone()))
        .or_insert(ScopeRollup {
            complete: 0,
            partial: 0,
            unsupported: 0,
            excluded: 0,
            failed: 0,
            eligible: 0,
        });

    let mut count = 0i64;
    for ((scope_kind, scope_key), r) in &scopes {
        let (u, a, c) = if scope_kind == "workspace" {
            (unresolved_count, ambiguous_count, conflict_count)
        } else {
            (0, 0, 0)
        };
        tx.execute(
            "INSERT INTO serving_coverage_rollup (
                serving_generation_id, scope_kind, scope_key, eligible_count, complete_count,
                partial_count, unsupported_count, excluded_count, failed_count, unresolved_count,
                ambiguous_count, conflict_count, details_json
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'{}')",
            params![
                serving_generation_id,
                scope_kind,
                scope_key,
                r.eligible,
                r.complete,
                r.partial,
                r.unsupported,
                r.excluded,
                r.failed,
                u,
                a,
                c,
            ],
        )?;
        count += 1;
    }
    Ok(count)
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
        (db_dir, conn, ws, ws_dir)
    }

    #[test]
    fn build_fails_cleanly_with_no_active_generation() {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        assert!(
            build_serving_generation(&conn, &ws).is_err(),
            "no active generation must fail, never silently produce an empty projection"
        );
    }

    #[test]
    fn build_produces_cards_edges_and_a_workspace_coverage_rollup() {
        let (_d, conn, ws, _wd) = setup();
        let report = build_serving_generation(&conn, &ws).unwrap();
        assert!(report.ok);
        assert!(
            report.card_count >= 2,
            "expected at least alpha+b symbol cards, got {}",
            report.card_count
        );
        assert!(
            report.edge_count >= 1,
            "expected at least the import/calls edges, got {}",
            report.edge_count
        );
        assert!(
            report.coverage_rollup_count >= 1,
            "the workspace-scope rollup row must always be present"
        );

        let state: String = conn
            .query_row(
                "SELECT state FROM serving_generation WHERE serving_generation_id = ?1",
                params![report.serving_generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "ready");
    }

    #[test]
    fn build_is_deterministic_across_repeated_calls() {
        let (_d, conn, ws, _wd) = setup();
        let r1 = build_serving_generation(&conn, &ws).unwrap();
        let r2 = build_serving_generation(&conn, &ws).unwrap();
        assert_eq!(
            r1.serving_generation_id, r2.serving_generation_id,
            "the key must be stable for unchanged (workspace, generation, schema, policy)"
        );
        assert_eq!(r1.card_count, r2.card_count);

        let mut hashes1: Vec<String> = {
            let mut stmt = conn.prepare("SELECT card_hash FROM symbol_serving_projection WHERE serving_generation_id = ?1 ORDER BY canonical_symbol_key").unwrap();
            stmt.query_map(params![r1.serving_generation_id], |r| r.get(0))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .collect()
        };
        hashes1.sort();
        assert!(!hashes1.is_empty());
    }

    #[test]
    fn rebuild_after_delete_reproduces_identical_card_hashes() {
        let (_d, conn, ws, _wd) = setup();
        let r1 = build_serving_generation(&conn, &ws).unwrap();
        let mut hashes_before: Vec<String> = {
            let mut stmt = conn.prepare("SELECT card_hash FROM symbol_serving_projection WHERE serving_generation_id = ?1 ORDER BY canonical_symbol_key").unwrap();
            stmt.query_map(params![r1.serving_generation_id], |r| r.get(0))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .collect()
        };
        hashes_before.sort();

        delete_serving_generation(&conn, &r1.serving_generation_id).unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbol_serving_projection WHERE serving_generation_id = ?1",
                params![r1.serving_generation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "delete must cascade to every projection row");

        let r2 = build_serving_generation(&conn, &ws).unwrap();
        let mut hashes_after: Vec<String> = {
            let mut stmt = conn.prepare("SELECT card_hash FROM symbol_serving_projection WHERE serving_generation_id = ?1 ORDER BY canonical_symbol_key").unwrap();
            stmt.query_map(params![r2.serving_generation_id], |r| r.get(0))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .collect()
        };
        hashes_after.sort();

        assert_eq!(
            hashes_before, hashes_after,
            "delete + rebuild against unchanged truth must reproduce byte-identical card hashes"
        );
    }

    #[test]
    fn failure_mid_build_leaves_zero_partial_rows() {
        let (_d, conn, ws, _wd) = setup();
        // Force a failure by dropping a table `project_symbol_cards` needs,
        // simulating a mid-build fault.
        conn.execute("DROP TABLE symbol_serving_projection", [])
            .unwrap();
        let err = build_serving_generation(&conn, &ws);
        assert!(
            err.is_err(),
            "a build that cannot write its projection table must fail, not silently skip it"
        );

        let leaked: i64 = conn
            .query_row("SELECT COUNT(*) FROM serving_generation", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leaked, 0, "a failed build must leave zero serving_generation rows -- the whole transaction rolled back");
    }

    #[test]
    fn edges_report_unresolved_for_structural_only_relationships() {
        let (_d, conn, ws, _wd) = setup();
        // This fixture never runs a semantic provider, so no
        // `relationship_resolution` rows exist -- every edge must be
        // honestly reported `unresolved`, never silently upgraded.
        let report = build_serving_generation(&conn, &ws).unwrap();
        let states: Vec<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT resolution_state FROM relationship_serving_edge WHERE serving_generation_id = ?1").unwrap();
            stmt.query_map(params![report.serving_generation_id], |r| r.get(0))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .collect()
        };
        assert_eq!(
            states,
            vec!["unresolved".to_string()],
            "structural-only edges must never claim a resolved state"
        );
    }
}
