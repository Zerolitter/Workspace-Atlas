//! V1.2 task session lifecycle and context-use telemetry (G5).
//!
//! Persists `context_ir::TaskSession` and `ContextUseEvent` documents into the
//! corresponding tables in `migrations/0003_context_intelligence_foundation.sql`.
//! Every write re-validates the same privacy invariant
//! `TaskSession::validate` checks in-memory (`raw_task` only present under
//! `local_opt_in` retention) — the persistence layer is not a second,
//! looser gate than the document layer.
//!
//! State machine (`task_session.state`, mirrors `TaskSessionState`):
//!
//! ```text
//! created -> context_compiled -> active -> reconciled -> completed
//!                                              \-> abandoned
//!    \-> failed (from any non-terminal state)
//! ```
//!
//! `context_use_event` rows are strictly append-only telemetry: a session's
//! full timeline is `task_session_events`, ordered by `occurred_at` then
//! `event_id` for determinism when timestamps collide.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::context_ir::{
    ContextIr, ContextUseEvent, ContextUseEventType, GenerationDelta, ObservationSource,
    RawTaskRetention, TaskKind, TaskKindSource, TaskSession, TaskSessionState, WorkingSetStatus,
};
use crate::context_metrics::{
    derive_context_metrics, CanonicalEvidenceId, CanonicalEvidenceKind, ContextMetricEvidence,
    ContextMetricInvalidity, ContextMetrics, ObservedEvidence, SuppliedEvidence,
    MAX_CONTEXT_METRIC_EVIDENCE,
};
use crate::error::{AtlasError, Result};
use crate::hashing::content_hash_of_bytes;
use crate::resolution::deterministic_id;
use crate::workspace::WorkspaceRecord;

pub const PLANNER_POLICY_VERSION: &str = crate::task_compiler::PLANNER_POLICY_VERSION;
pub const LEGACY_LIFECYCLE_CONTRACT_VERSION: &str = crate::context_ir::CONTEXT_SCHEMA_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum LegacyLifecycleError {
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("durable_contract_unavailable")]
    DurableContractUnavailable,
    #[error("cursor_invalid")]
    CursorInvalid,
    #[error("cursor_stale")]
    CursorStale,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskStartRequest {
    pub context_ir_version: String,
    pub task: String,
    pub declared_kind: Option<TaskKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskCompleteRequest {
    pub context_ir_version: String,
    pub accepted: bool,
    pub tests_passed: Option<bool>,
    pub outcome_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyTaskAbandonReason {
    UserRequested,
    InactivityTimeout,
}

impl LegacyTaskAbandonReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserRequested => "user_requested",
            Self::InactivityTimeout => "inactivity_timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskAbandonRequest {
    pub context_ir_version: String,
    pub reason: LegacyTaskAbandonReason,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskShowRequest {
    pub context_ir_version: String,
    pub task_session_id: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LegacyTaskShowPage {
    pub task_session: TaskSession,
    pub events: Vec<ContextUseEvent>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTaskShowCursorState {
    session_state: String,
    captured_event_rowid: i64,
    captured_event_count: i64,
    snapshot_digest: String,
    prefix_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTaskShowOrderingKey {
    last_occurred_at: String,
    last_event_id: String,
}
fn task_event_snapshot_digest(
    conn: &Connection,
    task_session_id: &str,
    captured_event_rowid: i64,
    through: Option<&LegacyTaskShowOrderingKey>,
) -> Result<(String, i64, Option<LegacyTaskShowOrderingKey>)> {
    let mut statement = conn.prepare(
        "SELECT rowid, event_id, context_id, item_id, entity_id, event_type,
                observation_source, occurred_at, byte_count, details_json
         FROM context_use_event
         WHERE task_session_id = ?1 AND rowid <= ?2
           AND (?3 IS NULL OR occurred_at < ?3
                OR (occurred_at = ?3 AND event_id <= ?4))
         ORDER BY occurred_at, event_id, rowid",
    )?;
    let mut rows = statement.query(params![
        task_session_id,
        captured_event_rowid,
        through.map(|key| key.last_occurred_at.as_str()),
        through.map(|key| key.last_event_id.as_str()),
    ])?;
    let mut digest =
        crate::context_application::ApplicationSnapshotHasher::new(b"task-context-events");
    let mut count = 0_i64;
    let mut last = None;
    while let Some(row) = rows.next()? {
        let record = (
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, String>(9)?,
        );
        last = Some(LegacyTaskShowOrderingKey {
            last_event_id: record.1.clone(),
            last_occurred_at: record.7.clone(),
        });
        digest.record(&record);
        count += 1;
    }
    Ok((digest.finish(), count, last))
}

fn require_legacy_contract(version: &str) -> std::result::Result<(), LegacyLifecycleError> {
    if version == LEGACY_LIFECYCLE_CONTRACT_VERSION {
        Ok(())
    } else {
        Err(LegacyLifecycleError::DurableContractUnavailable)
    }
}
fn lifecycle_cursor_error(
    error: crate::context_application::ApplicationCursorError,
) -> LegacyLifecycleError {
    match error {
        crate::context_application::ApplicationCursorError::Invalid => {
            LegacyLifecycleError::CursorInvalid
        }
        crate::context_application::ApplicationCursorError::Stale => {
            LegacyLifecycleError::CursorStale
        }
        crate::context_application::ApplicationCursorError::Atlas(error) => error.into(),
        crate::context_application::ApplicationCursorError::Sqlite(error) => error.into(),
    }
}

/// Create or identically reuse one legacy lifecycle session from the active
/// committed generation. The generation/tree snapshot and session identity
/// are established under the same immediate transaction.
pub fn start_legacy_task_session(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    request: &LegacyTaskStartRequest,
) -> std::result::Result<TaskSession, LegacyLifecycleError> {
    require_legacy_contract(&request.context_ir_version)?;
    if request.task.trim().is_empty()
        || request.task.len() > crate::context_route::MAX_GOVERNOR_REQUEST_BYTES
    {
        return Err(AtlasError::InvalidConfig("task must contain 1..=1048576 bytes".into()).into());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT g.generation_id, g.state, g.source_tree_hash
             FROM workspace w
             JOIN index_generation g ON g.generation_id = w.active_generation_id
             WHERE w.workspace_id = ?1",
            [&ws.workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((generation_id, state, Some(start_tree_hash))) = active else {
        return Err(AtlasError::InvalidConfig(
            "task start requires an active committed generation with a source tree hash".into(),
        )
        .into());
    };
    if state != "committed" {
        return Err(AtlasError::InvalidConfig(
            "task start requires an active committed generation".into(),
        )
        .into());
    }
    let task_hash = content_hash_of_bytes(request.task.as_bytes());
    let normalized_goal_hash = crate::task_compiler::normalized_task_hash(&request.task);
    let (task_kind, kind_source, kind_rule_id) =
        crate::task_compiler::classify_task(request.declared_kind, &request.task);
    let session = create_classified_task_session(
        &tx,
        ws,
        &generation_id,
        &task_hash,
        &normalized_goal_hash,
        RawTaskRetention::None,
        None,
        task_kind,
        kind_source,
        kind_rule_id.as_deref(),
        LEGACY_LIFECYCLE_CONTRACT_VERSION,
    )?;
    type IdentityRow = (String, String, String, String, String, Option<String>);
    let identity: IdentityRow = tx.query_row(
        "SELECT start_generation_id, task_hash, normalized_goal_hash,
                task_kind_source, context_ir_version, task_kind_rule_id
         FROM task_session WHERE task_session_id = ?1",
        [&session.task_session_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    if identity
        != (
            generation_id,
            task_hash,
            normalized_goal_hash,
            task_kind_source_str(kind_source).to_string(),
            LEGACY_LIFECYCLE_CONTRACT_VERSION.to_string(),
            kind_rule_id,
        )
    {
        return Err(
            AtlasError::InvalidConfig("conflicting task start replay identity".into()).into(),
        );
    }
    tx.execute(
        "UPDATE task_session SET start_tree_hash = ?2
         WHERE task_session_id = ?1 AND start_tree_hash IS NULL",
        params![session.task_session_id, start_tree_hash],
    )?;
    let session = load_task_session(&tx, &session.task_session_id)?
        .ok_or_else(|| AtlasError::Other("task session disappeared during start".into()))?;
    if session.start_tree_hash.as_deref() != Some(start_tree_hash.as_str()) {
        return Err(AtlasError::InvalidConfig(
            "conflicting task start replay tree identity".into(),
        )
        .into());
    }
    tx.commit()?;
    Ok(session)
}

/// Complete a reconciled legacy session using the application's current
/// committed generation and canonical Generation Delta.
pub fn complete_legacy_task_session(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    request: &LegacyTaskCompleteRequest,
) -> std::result::Result<TaskSession, LegacyLifecycleError> {
    require_legacy_contract(&request.context_ir_version)?;
    let session = load_task_session(conn, task_session_id)?
        .ok_or_else(|| AtlasError::Other(format!("task_session {task_session_id} not found")))?;
    if session.workspace_id != ws.workspace_id
        || session.context_ir_version != LEGACY_LIFECYCLE_CONTRACT_VERSION
    {
        return Err(LegacyLifecycleError::DurableContractUnavailable);
    }
    if session.state == TaskSessionState::Completed {
        if session.accepted == Some(request.accepted)
            && session.tests_passed == request.tests_passed
            && session.outcome_code.as_deref() == Some(request.outcome_code.as_str())
        {
            return Ok(session);
        }
        return Err(AtlasError::InvalidConfig(format!(
            "conflicting completion replay for task_session {task_session_id}"
        ))
        .into());
    }
    if session.state != TaskSessionState::Reconciled {
        return Err(AtlasError::InvalidConfig(format!(
            "illegal task_session transition: {} -> completed",
            state_str(session.state)
        ))
        .into());
    }
    let active: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT g.generation_id, g.state, g.source_tree_hash
             FROM workspace w
             JOIN index_generation g ON g.generation_id = w.active_generation_id
             WHERE w.workspace_id = ?1",
            [&ws.workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((end_generation_id, state, Some(end_tree_hash))) = active else {
        return Err(AtlasError::InvalidConfig(
            "task completion requires an active committed end generation/tree".into(),
        )
        .into());
    };
    if state != "committed" {
        return Err(AtlasError::InvalidConfig(
            "task completion requires an active committed end generation".into(),
        )
        .into());
    }
    let delta = crate::generation_delta::compute_generation_delta(
        conn,
        ws,
        &session.start_generation_id,
        &end_generation_id,
    )?;
    complete_task_session(
        conn,
        task_session_id,
        &delta,
        request.accepted,
        request.tests_passed,
        &request.outcome_code,
        &end_tree_hash,
    )?;
    load_task_session(conn, task_session_id)?.ok_or_else(|| {
        AtlasError::Other("task session disappeared during completion".into()).into()
    })
}

/// Atomically abandon an active or reconciled legacy session.
pub fn abandon_legacy_task_session(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    request: &LegacyTaskAbandonRequest,
) -> std::result::Result<TaskSession, LegacyLifecycleError> {
    require_legacy_contract(&request.context_ir_version)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let session = load_task_session(&tx, task_session_id)?
        .ok_or_else(|| AtlasError::Other(format!("task_session {task_session_id} not found")))?;
    if session.workspace_id != ws.workspace_id
        || session.context_ir_version != LEGACY_LIFECYCLE_CONTRACT_VERSION
    {
        return Err(LegacyLifecycleError::DurableContractUnavailable);
    }
    let reason = request.reason.as_str();
    if session.state == TaskSessionState::Abandoned {
        if session.outcome_code.as_deref() == Some(reason) && session.completed_at.is_some() {
            tx.commit()?;
            return Ok(session);
        }
        return Err(AtlasError::InvalidConfig(format!(
            "conflicting abandon replay for task_session {task_session_id}"
        ))
        .into());
    }
    if !matches!(
        session.state,
        TaskSessionState::Active | TaskSessionState::Reconciled
    ) {
        return Err(AtlasError::InvalidConfig(format!(
            "illegal task_session transition: {} -> abandoned",
            state_str(session.state)
        ))
        .into());
    }
    let completed_at = crate::migrations::iso8601_now();
    let updated = tx.execute(
        "UPDATE task_session
         SET state = 'abandoned', outcome_code = ?2, completed_at = ?3
         WHERE task_session_id = ?1 AND state IN ('active', 'reconciled')",
        params![task_session_id, reason, completed_at],
    )?;
    if updated != 1 {
        return Err(AtlasError::Other(format!(
            "task_session {task_session_id} changed during abandon"
        ))
        .into());
    }
    let abandoned = load_task_session(&tx, task_session_id)?
        .ok_or_else(|| AtlasError::Other("task session disappeared during abandon".into()))?;
    tx.commit()?;
    Ok(abandoned)
}

/// Return one bounded, frozen page of a legacy session timeline.
pub fn show_legacy_task_session(
    conn: &Connection,
    ws: &WorkspaceRecord,
    request: &LegacyTaskShowRequest,
) -> std::result::Result<LegacyTaskShowPage, LegacyLifecycleError> {
    Ok(show_legacy_task_session_snapshot(
        conn,
        ws,
        request,
        crate::context_application::ApplicationCursorOperation::TaskShow,
        false,
    )?
    .page)
}

pub struct LegacyTaskShowSnapshot {
    pub page: LegacyTaskShowPage,
    pub captured_event_rowid: i64,
    pub metrics: Option<ContextMetrics>,
}

pub fn show_legacy_task_session_snapshot(
    conn: &Connection,
    ws: &WorkspaceRecord,
    request: &LegacyTaskShowRequest,
    operation: crate::context_application::ApplicationCursorOperation,
    include_metrics: bool,
) -> std::result::Result<LegacyTaskShowSnapshot, LegacyLifecycleError> {
    show_legacy_task_session_snapshot_inner(conn, ws, request, operation, include_metrics)
}

fn show_legacy_task_session_snapshot_inner(
    conn: &Connection,
    ws: &WorkspaceRecord,
    request: &LegacyTaskShowRequest,
    operation: crate::context_application::ApplicationCursorOperation,
    include_metrics: bool,
) -> std::result::Result<LegacyTaskShowSnapshot, LegacyLifecycleError> {
    require_legacy_contract(&request.context_ir_version)?;
    let limit = request
        .limit
        .unwrap_or(crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT);
    if limit == 0 || limit > crate::context_route::MAX_APPLICATION_PAGE_LIMIT {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        ))
        .into());
    }
    let tx = conn.unchecked_transaction()?;
    let session = load_task_session(&tx, &request.task_session_id)?.ok_or_else(|| {
        AtlasError::Other(format!(
            "task_session {} not found",
            request.task_session_id
        ))
    })?;
    if session.workspace_id != ws.workspace_id
        || session.context_ir_version != LEGACY_LIFECYCLE_CONTRACT_VERSION
    {
        return Err(LegacyLifecycleError::DurableContractUnavailable);
    }
    let current_state = state_str(session.state).to_string();
    let identity_binding = crate::context_application::ApplicationCursorBinding {
        workspace_id: &ws.workspace_id,
        operation,
        contract_version: LEGACY_LIFECYCLE_CONTRACT_VERSION,
        filter: crate::context_application::ApplicationCursorFilter::TaskSession {
            task_session_id: &request.task_session_id,
        },
        captured_state: "",
    };
    let (cursor_state, ordering) = if let Some(token) = &request.cursor {
        let decoded = crate::context_application::decode_application_cursor_snapshot(
            &tx,
            ws,
            &identity_binding,
            token,
        )
        .map_err(lifecycle_cursor_error)?;
        let state: LegacyTaskShowCursorState = serde_json::from_str(&decoded.captured_state)
            .map_err(|_| LegacyLifecycleError::CursorInvalid)?;
        let ordering: LegacyTaskShowOrderingKey = serde_json::from_str(&decoded.ordering_key)
            .map_err(|_| LegacyLifecycleError::CursorInvalid)?;
        if state.session_state != current_state {
            return Err(LegacyLifecycleError::CursorStale);
        }
        let (snapshot_digest, captured_count, _) = task_event_snapshot_digest(
            &tx,
            &request.task_session_id,
            state.captured_event_rowid,
            None,
        )?;
        let (prefix_digest, _, last) = task_event_snapshot_digest(
            &tx,
            &request.task_session_id,
            state.captured_event_rowid,
            Some(&ordering),
        )?;
        if captured_count != state.captured_event_count
            || snapshot_digest != state.snapshot_digest
            || prefix_digest != state.prefix_digest
            || last.as_ref() != Some(&ordering)
        {
            return Err(LegacyLifecycleError::CursorStale);
        }
        (state, ordering)
    } else {
        let (captured_event_rowid, captured_event_count): (i64, i64) = tx.query_row(
            "SELECT COALESCE(MAX(rowid), 0), COUNT(*) FROM context_use_event
             WHERE task_session_id = ?1",
            [&request.task_session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (snapshot_digest, digest_count, _) =
            task_event_snapshot_digest(&tx, &request.task_session_id, captured_event_rowid, None)?;
        if digest_count != captured_event_count {
            return Err(LegacyLifecycleError::CursorStale);
        }
        (
            LegacyTaskShowCursorState {
                session_state: current_state,
                captured_event_rowid,
                captured_event_count,
                snapshot_digest,
                prefix_digest: String::new(),
            },
            LegacyTaskShowOrderingKey {
                last_occurred_at: String::new(),
                last_event_id: String::new(),
            },
        )
    };
    let query_limit = i64::try_from(limit + 1)
        .map_err(|_| AtlasError::InvalidConfig("limit conversion overflow".into()))?;
    let mut statement = tx.prepare(
        "SELECT event_id, context_id, item_id, entity_id, event_type,
                observation_source, occurred_at, byte_count, details_json
         FROM context_use_event
         WHERE task_session_id = ?1
           AND rowid <= ?2
           AND (occurred_at > ?3 OR (occurred_at = ?3 AND event_id > ?4))
         ORDER BY occurred_at, event_id
         LIMIT ?5",
    )?;
    type StoredEventRow = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        Option<i64>,
        String,
    );
    let rows = statement.query_map(
        params![
            request.task_session_id,
            cursor_state.captured_event_rowid,
            ordering.last_occurred_at,
            ordering.last_event_id,
            query_limit,
        ],
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
    )?;
    let mut events = Vec::with_capacity(limit + 1);
    for row in rows {
        let (
            event_id,
            context_id,
            item_id,
            entity_id,
            event_type,
            observation_source,
            occurred_at,
            bytes,
            details,
        ): StoredEventRow = row?;
        events.push(ContextUseEvent {
            schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
            event_id,
            task_session_id: request.task_session_id.clone(),
            context_id,
            item_id,
            entity_id,
            event_type: event_type_from_str(&event_type).ok_or_else(|| {
                AtlasError::InvalidConfig("stored context-use event type is invalid".into())
            })?,
            observation_source: observation_source_from_str(&observation_source).ok_or_else(
                || AtlasError::InvalidConfig("stored observation source is invalid".into()),
            )?,
            occurred_at,
            bytes,
            details: serde_json::from_str(&details).map_err(|_| {
                AtlasError::InvalidConfig("stored context-use event details are invalid".into())
            })?,
        });
    }
    let has_more = events.len() > limit;
    events.truncate(limit);
    let captured_event_rowid = cursor_state.captured_event_rowid;
    let next_cursor = if has_more {
        let last = events.last().ok_or(LegacyLifecycleError::CursorInvalid)?;
        let ordering = LegacyTaskShowOrderingKey {
            last_occurred_at: last.occurred_at.clone(),
            last_event_id: last.event_id.clone(),
        };
        let (prefix_digest, _, prefix_last) = task_event_snapshot_digest(
            &tx,
            &request.task_session_id,
            cursor_state.captured_event_rowid,
            Some(&ordering),
        )?;
        if prefix_last.as_ref() != Some(&ordering) {
            return Err(LegacyLifecycleError::CursorStale);
        }
        let captured_state = serde_json::to_string(&LegacyTaskShowCursorState {
            prefix_digest,
            ..cursor_state.clone()
        })
        .map_err(AtlasError::from)?;
        let binding = crate::context_application::ApplicationCursorBinding {
            captured_state: &captured_state,
            ..identity_binding
        };
        Some(
            crate::context_application::encode_application_cursor(
                &tx,
                ws,
                &binding,
                &serde_json::to_string(&ordering).map_err(AtlasError::from)?,
            )
            .map_err(lifecycle_cursor_error)?,
        )
    } else {
        None
    };
    let page = LegacyTaskShowPage {
        task_session: session,
        events,
        next_cursor,
    };
    let metrics = include_metrics
        .then(|| {
            task_session_context_metrics_through(
                &tx,
                &request.task_session_id,
                Some(cursor_state.captured_event_rowid),
            )
        })
        .transpose()?;
    drop(statement);
    tx.commit()?;
    Ok(LegacyTaskShowSnapshot {
        page,
        captured_event_rowid,
        metrics,
    })
}

/// Compatibility constructor for callers that explicitly supply a task kind.
#[allow(clippy::too_many_arguments)]
pub fn create_task_session(
    conn: &Connection,
    ws: &WorkspaceRecord,
    start_generation_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    raw_task_retention: RawTaskRetention,
    raw_task: Option<&str>,
    task_kind: TaskKind,
    context_ir_version: &str,
) -> Result<TaskSession> {
    create_classified_task_session(
        conn,
        ws,
        start_generation_id,
        task_hash,
        normalized_goal_hash,
        raw_task_retention,
        raw_task,
        task_kind,
        TaskKindSource::Declared,
        None,
        context_ir_version,
    )
}

/// Create a new task session in state `created`, retaining the disclosed task
/// classification but never raw task text without local opt-in.
#[allow(clippy::too_many_arguments)]
pub fn create_classified_task_session(
    conn: &Connection,
    ws: &WorkspaceRecord,
    start_generation_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    raw_task_retention: RawTaskRetention,
    raw_task: Option<&str>,
    task_kind: TaskKind,
    task_kind_source: TaskKindSource,
    task_kind_rule_id: Option<&str>,
    context_ir_version: &str,
) -> Result<TaskSession> {
    if context_ir_version != crate::context_ir::CONTEXT_SCHEMA_VERSION {
        return Err(AtlasError::InvalidConfig(format!(
            "durable task lifecycle supports only legacy Context IR {}; transient V2 lifecycle must not be persisted before H7",
            crate::context_ir::CONTEXT_SCHEMA_VERSION
        )));
    }
    let task_session_id = deterministic_id(
        "task",
        &[
            &ws.workspace_id,
            start_generation_id,
            task_hash,
            normalized_goal_hash,
            task_kind_str(task_kind),
            task_kind_source_str(task_kind_source),
            task_kind_rule_id.unwrap_or(""),
            retention_str(raw_task_retention),
            PLANNER_POLICY_VERSION,
            context_ir_version,
        ],
    );
    let created_at = crate::migrations::iso8601_now();

    let session = TaskSession {
        schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
        task_session_id: task_session_id.clone(),
        workspace_id: ws.workspace_id.clone(),
        start_generation_id: start_generation_id.to_string(),
        end_generation_id: None,
        task_hash: task_hash.to_string(),
        normalized_goal_hash: normalized_goal_hash.to_string(),
        raw_task_retention,
        raw_task: raw_task.map(str::to_string),
        task_kind,
        state: TaskSessionState::Created,
        created_at: created_at.clone(),
        completed_at: None,
        planner_policy_version: PLANNER_POLICY_VERSION.to_string(),
        context_ir_version: context_ir_version.to_string(),
        accepted: None,
        tests_passed: None,
        outcome_code: None,
        start_tree_hash: None,
        end_tree_hash: None,
    };
    session.validate()?;

    conn.execute(
        "INSERT OR IGNORE INTO task_session (
            task_session_id, workspace_id, start_generation_id, end_generation_id,
            task_hash, normalized_goal_hash, raw_task_retention, raw_task, task_kind,
            task_kind_source, task_kind_rule_id, planner_policy_version, context_ir_version,
            state, accepted, tests_passed, outcome_code, start_tree_hash, end_tree_hash, created_at, completed_at
        ) VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,?8,?9,?10,?11,?12,'created',NULL,NULL,NULL,NULL,NULL,?13,NULL)",
        params![
            session.task_session_id,
            session.workspace_id,
            session.start_generation_id,
            session.task_hash,
            session.normalized_goal_hash,
            retention_str(session.raw_task_retention),
            session.raw_task,
            task_kind_str(session.task_kind),
            task_kind_source_str(task_kind_source),
            task_kind_rule_id,
            session.planner_policy_version,
            session.context_ir_version,
            created_at,
        ],
    )?;

    // The identity includes classification and policy, so an identical
    // request reuses only the exact same task session.
    load_task_session(conn, &task_session_id)?.ok_or_else(|| {
        AtlasError::Other(format!(
            "task_session {task_session_id} missing immediately after insert"
        ))
    })
}

/// Load a task session's current persisted state.
pub fn load_task_session(conn: &Connection, task_session_id: &str) -> Result<Option<TaskSession>> {
    let out = conn
        .query_row(
            "SELECT task_session_id, workspace_id, start_generation_id, end_generation_id, task_hash,
                    normalized_goal_hash, raw_task_retention, raw_task, task_kind, state, created_at,
                    completed_at, planner_policy_version, context_ir_version, accepted, tests_passed,
                    outcome_code, start_tree_hash, end_tree_hash
             FROM task_session WHERE task_session_id = ?1",
            params![task_session_id],
            |r| {
                let retention_raw: String = r.get(6)?;
                let kind_raw: String = r.get(8)?;
                let state_raw: String = r.get(9)?;
                Ok(TaskSession {
                    schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
                    task_session_id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    start_generation_id: r.get(2)?,
                    end_generation_id: r.get(3)?,
                    task_hash: r.get(4)?,
                    normalized_goal_hash: r.get(5)?,
                    raw_task_retention: retention_from_str(&retention_raw),
                    raw_task: r.get(7)?,
                    task_kind: task_kind_from_str(&kind_raw),
                    state: state_from_str(&state_raw).unwrap_or(TaskSessionState::Created),
                    created_at: r.get(10)?,
                    completed_at: r.get(11)?,
                    planner_policy_version: r.get(12)?,
                    context_ir_version: r.get(13)?,
                    accepted: r.get(14)?,
                    tests_passed: r.get(15)?,
                    outcome_code: r.get(16)?,
                    start_tree_hash: r.get(17)?,
                    end_tree_hash: r.get(18)?,
                })
            },
        )
        .optional()?;
    Ok(out)
}

fn retention_str(r: RawTaskRetention) -> &'static str {
    match r {
        RawTaskRetention::None => "none",
        RawTaskRetention::HashOnly => "hash_only",
        RawTaskRetention::LocalOptIn => "local_opt_in",
    }
}

fn task_kind_str(k: TaskKind) -> &'static str {
    match k {
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

fn task_kind_source_str(source: TaskKindSource) -> &'static str {
    match source {
        TaskKindSource::Declared => "declared",
        TaskKindSource::DeterministicRule => "deterministic_rule",
        TaskKindSource::Unknown => "unknown",
    }
}

fn retention_from_str(s: &str) -> RawTaskRetention {
    match s {
        "hash_only" => RawTaskRetention::HashOnly,
        "local_opt_in" => RawTaskRetention::LocalOptIn,
        _ => RawTaskRetention::None,
    }
}

fn task_kind_from_str(s: &str) -> TaskKind {
    match s {
        "explore" => TaskKind::Explore,
        "bug_fix" => TaskKind::BugFix,
        "behavior_change" => TaskKind::BehaviorChange,
        "api_change" => TaskKind::ApiChange,
        "refactor" => TaskKind::Refactor,
        "configuration_change" => TaskKind::ConfigurationChange,
        "test_change" => TaskKind::TestChange,
        "review" => TaskKind::Review,
        "audit" => TaskKind::Audit,
        _ => TaskKind::Unknown,
    }
}

fn state_str(s: TaskSessionState) -> &'static str {
    match s {
        TaskSessionState::Created => "created",
        TaskSessionState::ContextCompiled => "context_compiled",
        TaskSessionState::Active => "active",
        TaskSessionState::Reconciled => "reconciled",
        TaskSessionState::Completed => "completed",
        TaskSessionState::Abandoned => "abandoned",
        TaskSessionState::Failed => "failed",
    }
}

fn state_from_str(s: &str) -> Option<TaskSessionState> {
    Some(match s {
        "created" => TaskSessionState::Created,
        "context_compiled" => TaskSessionState::ContextCompiled,
        "active" => TaskSessionState::Active,
        "reconciled" => TaskSessionState::Reconciled,
        "completed" => TaskSessionState::Completed,
        "abandoned" => TaskSessionState::Abandoned,
        "failed" => TaskSessionState::Failed,
        _ => return None,
    })
}

pub(crate) struct LegacyTaskSessionIdentity<'a> {
    pub workspace_id: &'a str,
    pub start_generation_id: &'a str,
    pub task_hash: &'a str,
    pub normalized_goal_hash: &'a str,
    pub task_kind: TaskKind,
    pub kind_source: TaskKindSource,
    pub kind_rule_id: Option<&'a str>,
}

fn validate_legacy_session(
    conn: &Connection,
    task_session_id: &str,
    workspace_id: Option<&str>,
    identity: Option<&LegacyTaskSessionIdentity<'_>>,
) -> Result<TaskSessionState> {
    type BindingRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
    );
    let row: Option<BindingRow> = conn
        .query_row(
            "SELECT ts.workspace_id, ts.start_generation_id, ts.task_hash,
                    ts.normalized_goal_hash, ts.task_kind, ts.task_kind_source,
                    ts.context_ir_version, ts.task_kind_rule_id, ts.state, g.state
             FROM task_session ts
             JOIN index_generation g ON g.generation_id = ts.start_generation_id
             WHERE ts.task_session_id = ?1",
            params![task_session_id],
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
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(AtlasError::InvalidConfig(format!(
            "task session {task_session_id} does not exist"
        )));
    };
    let state = state_from_str(&row.8).ok_or_else(|| {
        AtlasError::Other(format!(
            "task_session {task_session_id} has an unrecognized state {:?}",
            row.8
        ))
    })?;
    if row.6 != LEGACY_LIFECYCLE_CONTRACT_VERSION
        || row.9 != "committed"
        || workspace_id.is_some_and(|workspace_id| row.0 != workspace_id)
        || identity.is_some_and(|identity| {
            row.0 != identity.workspace_id
                || row.1 != identity.start_generation_id
                || row.2 != identity.task_hash
                || row.3 != identity.normalized_goal_hash
                || row.4 != task_kind_str(identity.task_kind)
                || row.5 != task_kind_source_str(identity.kind_source)
                || row.7.as_deref() != identity.kind_rule_id
        })
    {
        return Err(AtlasError::InvalidConfig(format!(
            "legacy task session identity mismatch for {task_session_id}"
        )));
    }
    if matches!(
        state,
        TaskSessionState::Completed | TaskSessionState::Abandoned | TaskSessionState::Failed
    ) {
        return Err(AtlasError::InvalidConfig(format!(
            "legacy task session {task_session_id} is terminal ({})",
            state_str(state)
        )));
    }
    Ok(state)
}

pub(crate) fn validate_legacy_task_session_identity(
    conn: &Connection,
    task_session_id: &str,
    identity: &LegacyTaskSessionIdentity<'_>,
) -> Result<()> {
    validate_legacy_session(
        conn,
        task_session_id,
        Some(identity.workspace_id),
        Some(identity),
    )?;
    Ok(())
}

fn activate_validated_legacy_session(
    conn: &Connection,
    task_session_id: &str,
    workspace_id: Option<&str>,
    identity: Option<&LegacyTaskSessionIdentity<'_>>,
) -> Result<()> {
    let state = validate_legacy_session(conn, task_session_id, workspace_id, identity)?;
    if state == TaskSessionState::Created {
        let updated = conn.execute(
            "UPDATE task_session SET state = 'context_compiled'
             WHERE task_session_id = ?1 AND state = 'created'",
            params![task_session_id],
        )?;
        if updated != 1 {
            return Err(AtlasError::Other(format!(
                "task_session {task_session_id} changed during activation"
            )));
        }
    }
    if matches!(
        state,
        TaskSessionState::Created | TaskSessionState::ContextCompiled
    ) {
        let updated = conn.execute(
            "UPDATE task_session SET state = 'active'
             WHERE task_session_id = ?1 AND state = 'context_compiled'",
            params![task_session_id],
        )?;
        if updated != 1 {
            return Err(AtlasError::Other(format!(
                "task_session {task_session_id} changed during activation"
            )));
        }
    }
    Ok(())
}

pub(crate) fn activate_legacy_task_session(
    conn: &Connection,
    task_session_id: &str,
    identity: &LegacyTaskSessionIdentity<'_>,
) -> Result<()> {
    validate_legacy_task_session_identity(conn, task_session_id, identity)?;
    let explicitly_bound: bool = conn.query_row(
        "SELECT start_tree_hash IS NOT NULL FROM task_session
         WHERE task_session_id = ?1",
        params![task_session_id],
        |row| row.get(0),
    )?;
    if explicitly_bound {
        activate_validated_legacy_session(
            conn,
            task_session_id,
            Some(identity.workspace_id),
            Some(identity),
        )?;
    }
    Ok(())
}

pub(crate) fn reconcile_active_legacy_sessions(
    conn: &Connection,
    workspace_id: &str,
    candidate_generation_id: &str,
) -> Result<()> {
    let candidate_sequence: i64 = conn.query_row(
        "SELECT sequence_no FROM index_generation
         WHERE generation_id = ?1 AND workspace_id = ?2",
        params![candidate_generation_id, workspace_id],
        |row| row.get(0),
    )?;
    let session_ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT ts.task_session_id
             FROM task_session ts
             JOIN index_generation start ON start.generation_id = ts.start_generation_id
             WHERE ts.workspace_id = ?1
               AND ts.context_ir_version = ?2
               AND ts.state = 'active'
               AND start.sequence_no < ?3
             ORDER BY ts.task_session_id",
        )?;
        let rows = statement.query_map(
            params![
                workspace_id,
                LEGACY_LIFECYCLE_CONTRACT_VERSION,
                candidate_sequence
            ],
            |row| row.get(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for task_session_id in session_ids {
        let updated = conn.execute(
            "UPDATE task_session SET state = 'reconciled'
             WHERE task_session_id = ?1 AND state = 'active'",
            params![task_session_id],
        )?;
        if updated != 1 {
            return Err(AtlasError::Other(format!(
                "task_session {task_session_id} changed during reconciliation"
            )));
        }
    }
    Ok(())
}

/// Is `to` a legal transition from `from`? `failed` is reachable from any
/// non-terminal state (a session can fail at any point); `completed`/
/// `abandoned`/`failed` are terminal — no transition leaves them.
fn is_valid_transition(from: TaskSessionState, to: TaskSessionState) -> bool {
    use TaskSessionState::*;
    if matches!(from, Completed | Abandoned | Failed) {
        return false; // terminal states never transition further
    }
    if to == Failed {
        return true; // failure is reachable from any non-terminal state
    }
    matches!(
        (from, to),
        (Created, ContextCompiled)
            | (ContextCompiled, Active)
            | (Active, Reconciled)
            | (Reconciled, Completed)
            | (Reconciled, Abandoned)
            | (Active, Abandoned)
    )
}

/// Move a task session to a new lifecycle state. Rejects an illegal
/// transition (e.g. `created` -> `completed` directly, or any transition
/// out of a terminal state) rather than silently accepting it.
pub fn transition_task_session(
    conn: &Connection,
    task_session_id: &str,
    to: TaskSessionState,
) -> Result<()> {
    let current: Option<String> = conn
        .query_row(
            "SELECT state FROM task_session WHERE task_session_id = ?1",
            params![task_session_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(AtlasError::Other(format!(
            "task_session {task_session_id} not found"
        )));
    };
    let from = state_from_str(&current).ok_or_else(|| {
        AtlasError::Other(format!(
            "task_session {task_session_id} has an unrecognized state {current:?}"
        ))
    })?;

    if !is_valid_transition(from, to) {
        return Err(AtlasError::InvalidConfig(format!(
            "illegal task_session transition: {} -> {}",
            state_str(from),
            state_str(to)
        )));
    }

    conn.execute(
        "UPDATE task_session SET state = ?2 WHERE task_session_id = ?1",
        params![task_session_id, state_str(to)],
    )?;
    Ok(())
}

/// Terminal completion and exact post-reconcile artifact linkage.
///
/// The session update, Generation Delta persistence, reported outcome, and
/// Atlas-observed artifact events commit together. An identical replay is a
/// no-op; any conflicting replay or illegal lifecycle state fails closed.
#[allow(clippy::too_many_arguments)]
pub fn complete_task_session(
    conn: &mut Connection,
    task_session_id: &str,
    delta: &GenerationDelta,
    accepted: bool,
    tests_passed: Option<bool>,
    outcome_code: &str,
    end_tree_hash: &str,
) -> Result<()> {
    const MAX_OUTCOME_CODE_BYTES: usize = 128;
    if outcome_code.is_empty() || outcome_code.len() > MAX_OUTCOME_CODE_BYTES {
        return Err(AtlasError::InvalidConfig(format!(
            "outcome_code must contain 1..={MAX_OUTCOME_CODE_BYTES} bytes"
        )));
    }
    if end_tree_hash.len() != 64
        || !end_tree_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AtlasError::InvalidConfig(
            "end_tree_hash must be 64 lowercase hex characters".to_string(),
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    type CompletionRow = (
        String,
        String,
        String,
        Option<String>,
        Option<bool>,
        Option<bool>,
        Option<String>,
        Option<String>,
    );
    let current: Option<CompletionRow> = tx
        .query_row(
            "SELECT workspace_id, start_generation_id, state, end_generation_id,
                    accepted, tests_passed, outcome_code, end_tree_hash
             FROM task_session WHERE task_session_id = ?1",
            params![task_session_id],
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
                ))
            },
        )
        .optional()?;
    let Some((
        workspace_id,
        start_generation_id,
        state,
        persisted_end_generation_id,
        persisted_accepted,
        persisted_tests_passed,
        persisted_outcome_code,
        persisted_end_tree_hash,
    )) = current
    else {
        return Err(AtlasError::Other(format!(
            "task_session {task_session_id} not found"
        )));
    };

    if state == "completed" {
        let persisted_delta_hash: Option<String> = tx
            .query_row(
                "SELECT delta_hash FROM generation_delta WHERE delta_id = ?1",
                params![delta.delta_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let identical = workspace_id == delta.workspace_id
            && start_generation_id == delta.from_generation_id
            && persisted_end_generation_id.as_deref() == Some(delta.to_generation_id.as_str())
            && persisted_accepted == Some(accepted)
            && persisted_tests_passed == tests_passed
            && persisted_outcome_code.as_deref() == Some(outcome_code)
            && persisted_end_tree_hash.as_deref() == Some(end_tree_hash)
            && persisted_delta_hash.as_deref() == Some(delta.delta_hash.as_str());
        if identical {
            tx.commit()?;
            return Ok(());
        }
        return Err(AtlasError::InvalidConfig(format!(
            "conflicting completion replay for task_session {task_session_id}"
        )));
    }

    let from = state_from_str(&state).ok_or_else(|| {
        AtlasError::Other(format!(
            "task_session {task_session_id} has an unrecognized state {state:?}"
        ))
    })?;
    if from != TaskSessionState::Reconciled {
        return Err(AtlasError::InvalidConfig(format!(
            "illegal task_session transition: {} -> completed",
            state_str(from)
        )));
    }
    if workspace_id != delta.workspace_id || start_generation_id != delta.from_generation_id {
        return Err(AtlasError::InvalidConfig(
            "completion delta does not match the task session workspace/start generation"
                .to_string(),
        ));
    }

    let end_generation: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT g.workspace_id, g.state, g.source_tree_hash
             FROM index_generation g
             JOIN workspace w
               ON w.workspace_id = g.workspace_id
              AND w.active_generation_id = g.generation_id
             WHERE g.generation_id = ?1",
            params![delta.to_generation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    match end_generation {
        Some((generation_workspace_id, generation_state, source_tree_hash))
            if generation_workspace_id == workspace_id
                && generation_state == "committed"
                && source_tree_hash.as_deref() == Some(end_tree_hash) => {}
        _ => {
            return Err(AtlasError::InvalidConfig(
                "completion requires the matching active committed end generation/tree hash"
                    .to_string(),
            ));
        }
    }

    crate::generation_delta::persist_generation_delta_in_transaction(&tx, delta)?;
    let occurred_at = crate::migrations::iso8601_now();
    for event in
        crate::generation_delta::artifact_change_events(task_session_id, delta, &occurred_at)
    {
        record_context_use_event(&tx, task_session_id, &event)?;
    }
    let outcome_event = ContextUseEvent {
        schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
        event_id: event_id_for(
            task_session_id,
            ContextUseEventType::OutcomeRecorded,
            "completion",
        ),
        task_session_id: task_session_id.to_string(),
        context_id: None,
        item_id: Some(delta.delta_id.clone()),
        entity_id: None,
        event_type: ContextUseEventType::OutcomeRecorded,
        observation_source: ObservationSource::AgentReported,
        occurred_at: occurred_at.clone(),
        bytes: None,
        details: serde_json::json!({
            "accepted": accepted,
            "tests_passed": tests_passed,
            "outcome_code": outcome_code,
            "end_generation_id": delta.to_generation_id,
            "end_tree_hash": end_tree_hash,
            "delta_id": delta.delta_id,
            "delta_hash": delta.delta_hash,
        }),
    };
    record_context_use_event(&tx, task_session_id, &outcome_event)?;

    let updated = tx.execute(
        "UPDATE task_session
         SET state = 'completed', end_generation_id = ?2, accepted = ?3, tests_passed = ?4,
             outcome_code = ?5, end_tree_hash = ?6, completed_at = ?7
         WHERE task_session_id = ?1 AND state = 'reconciled'",
        params![
            task_session_id,
            delta.to_generation_id,
            accepted,
            tests_passed,
            outcome_code,
            end_tree_hash,
            occurred_at,
        ],
    )?;
    if updated != 1 {
        return Err(AtlasError::Other(format!(
            "task_session {task_session_id} changed during completion"
        )));
    }
    tx.commit()?;
    Ok(())
}

fn event_type_str(t: crate::context_ir::ContextUseEventType) -> &'static str {
    use crate::context_ir::ContextUseEventType::*;
    match t {
        ContextSupplied => "context_supplied",
        ExactSourceRequested => "exact_source_requested",
        EntityQueried => "entity_queried",
        RelationshipTraversed => "relationship_traversed",
        ContextRecompiled => "context_recompiled",
        ArtifactChanged => "artifact_changed",
        ValidationSelected => "validation_selected",
        ReasoningUseReported => "reasoning_use_reported",
        ModificationTargetReported => "modification_target_reported",
        TestConsideredReported => "test_considered_reported",
        OutcomeRecorded => "outcome_recorded",
    }
}

fn event_type_from_str(s: &str) -> Option<crate::context_ir::ContextUseEventType> {
    use crate::context_ir::ContextUseEventType::*;
    Some(match s {
        "context_supplied" => ContextSupplied,
        "exact_source_requested" => ExactSourceRequested,
        "entity_queried" => EntityQueried,
        "relationship_traversed" => RelationshipTraversed,
        "context_recompiled" => ContextRecompiled,
        "artifact_changed" => ArtifactChanged,
        "validation_selected" => ValidationSelected,
        "reasoning_use_reported" => ReasoningUseReported,
        "modification_target_reported" => ModificationTargetReported,
        "test_considered_reported" => TestConsideredReported,
        "outcome_recorded" => OutcomeRecorded,
        _ => return None,
    })
}

fn observation_source_str(s: ObservationSource) -> &'static str {
    match s {
        ObservationSource::AtlasObserved => "atlas_observed",
        ObservationSource::AgentReported => "agent_reported",
        ObservationSource::OperatorReported => "operator_reported",
    }
}

fn observation_source_from_str(s: &str) -> Option<ObservationSource> {
    Some(match s {
        "atlas_observed" => ObservationSource::AtlasObserved,
        "agent_reported" => ObservationSource::AgentReported,
        "operator_reported" => ObservationSource::OperatorReported,
        _ => return None,
    })
}

/// Bound on a `details_json` payload -- context-use telemetry is metadata
/// about context usage, never a place to smuggle raw source bodies (SEC-001
/// bar, applied here to a new table).
const MAX_DETAILS_BYTES: usize = 4096;

/// Record one context-use event. `event.details` is bounded (never a raw
/// source-body dump) and the event's `observation_source` is always
/// persisted, so telemetry is source-distinguished by construction, not by
/// convention alone.
pub fn record_context_use_event(
    conn: &Connection,
    task_session_id: &str,
    event: &ContextUseEvent,
) -> Result<()> {
    fn insert_event(
        conn: &Connection,
        task_session_id: &str,
        event: &ContextUseEvent,
    ) -> Result<()> {
        let details_str = serde_json::to_string(&event.details)?;
        if details_str.len() > MAX_DETAILS_BYTES {
            return Err(AtlasError::InvalidConfig(format!(
                "context_use_event.details exceeds the {MAX_DETAILS_BYTES}-byte bound ({} bytes) -- telemetry must stay bounded metadata, not a raw payload dump",
                details_str.len()
            )));
        }
        conn.execute(
            "INSERT INTO context_use_event (
                event_id, task_session_id, context_id, item_id, entity_id, event_type,
                observation_source, byte_count, details_json, occurred_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                event.event_id,
                task_session_id,
                event.context_id,
                event.item_id,
                event.entity_id,
                event_type_str(event.event_type),
                observation_source_str(event.observation_source),
                event.bytes,
                details_str,
                event.occurred_at,
            ],
        )?;
        Ok(())
    }

    let activation_kind = match event.event_type {
        ContextUseEventType::ContextSupplied
        | ContextUseEventType::RelationshipTraversed
        | ContextUseEventType::ContextRecompiled => Some(true),
        ContextUseEventType::EntityQueried => Some(false),
        ContextUseEventType::ExactSourceRequested
            if event
                .details
                .get("status")
                .and_then(serde_json::Value::as_str)
                == Some("verified") =>
        {
            Some(false)
        }
        _ => None,
    };
    let write = |connection: &Connection| -> Result<()> {
        let explicitly_bound = if activation_kind == Some(true) {
            connection
                .query_row(
                    "SELECT start_tree_hash IS NOT NULL FROM task_session
                     WHERE task_session_id = ?1",
                    params![task_session_id],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false)
        } else {
            true
        };
        let activates = event.observation_source == ObservationSource::AtlasObserved
            && activation_kind.is_some()
            && explicitly_bound;
        if activates {
            validate_legacy_session(connection, task_session_id, None, None)?;
        }
        insert_event(connection, task_session_id, event)?;
        if activates {
            activate_validated_legacy_session(connection, task_session_id, None, None)?;
        }
        Ok(())
    };
    if conn.is_autocommit() {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        match write(conn) {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    } else {
        write(conn)
    }
}

/// Build a deterministic event id from the session, event type, and a
/// caller-supplied ordinal/discriminator (so repeated events of the same
/// type in one session get distinct, stable ids).
pub fn event_id_for(
    task_session_id: &str,
    event_type: crate::context_ir::ContextUseEventType,
    discriminator: &str,
) -> String {
    deterministic_id(
        "evt",
        &[task_session_id, event_type_str(event_type), discriminator],
    )
}

/// Exact-source telemetry accepts only these two bounded request shapes.
/// The line bounds are request metadata, never returned source.
pub(crate) enum ExactSourceRequestKind {
    Source,
    SourceRange { start_line: i64, end_line: i64 },
}

/// Record an ordinary exact-source response while preserving its established
/// path-linked V1 observation contract.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_exact_source_request(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    canonical_path: &str,
    request_kind: ExactSourceRequestKind,
    status: &str,
    bytes: Option<i64>,
) -> Result<()> {
    record_exact_source_observation(
        conn,
        ws,
        task_session_id,
        Some(canonical_path),
        request_kind,
        status,
        bytes,
    )
}

/// Append one Atlas-observed exact-source event after a typed source result
/// has been completed. Ownership validation, per-session ordinal allocation,
/// and insertion share one immediate transaction so persistence failure is
/// fail-closed and never consumes an ordinal.
#[allow(clippy::too_many_arguments)]
fn record_exact_source_observation(
    conn: &mut Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    canonical_path: Option<&str>,
    request_kind: ExactSourceRequestKind,
    status: &str,
    bytes: Option<i64>,
) -> Result<()> {
    if !matches!(
        status,
        "verified" | "hash_mismatch" | "not_found" | "read_failed"
    ) {
        return Err(AtlasError::InvalidConfig(format!(
            "unsupported exact-source status: {status}"
        )));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let session_workspace: Option<String> = tx
        .query_row(
            "SELECT workspace_id FROM task_session WHERE task_session_id = ?1",
            params![task_session_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(session_workspace) = session_workspace else {
        return Err(AtlasError::InvalidConfig(format!(
            "task session {task_session_id} does not exist"
        )));
    };
    if session_workspace != ws.workspace_id {
        return Err(AtlasError::InvalidConfig(format!(
            "task session {task_session_id} does not belong to workspace {}",
            ws.workspace_id
        )));
    }

    let prior_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM context_use_event
         WHERE task_session_id = ?1
           AND event_type = 'exact_source_requested'
           AND observation_source = 'atlas_observed'",
        params![task_session_id],
        |row| row.get(0),
    )?;
    let request_ordinal = prior_count
        .checked_add(1)
        .ok_or_else(|| AtlasError::Other("exact-source request ordinal overflow".to_string()))?;
    let event_id = event_id_for(
        task_session_id,
        ContextUseEventType::ExactSourceRequested,
        &format!("request:{request_ordinal}"),
    );
    let mut details = serde_json::json!({
        "request_ordinal": request_ordinal,
        "status": status,
    });
    if let Some(canonical_path) = canonical_path {
        details["path_hash"] = serde_json::json!(content_hash_of_bytes(canonical_path.as_bytes()));
    }
    match request_kind {
        ExactSourceRequestKind::Source => {
            details["request_kind"] = serde_json::json!("source");
        }
        ExactSourceRequestKind::SourceRange {
            start_line,
            end_line,
        } => {
            details["request_kind"] = serde_json::json!("source_range");
            details["requested_start_line"] = serde_json::json!(start_line);
            details["requested_end_line"] = serde_json::json!(end_line);
        }
    }
    let event = ContextUseEvent {
        schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
        event_id: event_id.clone(),
        task_session_id: task_session_id.to_string(),
        context_id: None,
        item_id: None,
        entity_id: canonical_path.is_none().then_some(event_id),
        event_type: ContextUseEventType::ExactSourceRequested,
        observation_source: ObservationSource::AtlasObserved,
        occurred_at: crate::migrations::iso8601_now(),
        bytes,
        details,
    };
    record_context_use_event(&tx, task_session_id, &event)?;
    tx.commit()?;
    Ok(())
}

fn serialized_enum<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| AtlasError::Other("serialized enum was not a string".to_string()))
}

fn persist_context_ir(
    tx: &rusqlite::Transaction<'_>,
    request_hash: &str,
    context_ir: &ContextIr,
) -> Result<()> {
    let canonical_json = serde_json::to_string(context_ir)?;
    tx.execute(
        "INSERT OR IGNORE INTO context_ir (
            context_id, task_session_id, workspace_id, generation_id, serving_generation_id,
            context_ir_version, planner_policy_version, projection_policy_version, budget_profile,
            request_hash, working_set_status, serving_fallback, max_records, selected_records,
            max_source_bytes, selected_source_bytes, max_estimated_tokens, selected_estimated_tokens,
            soft_latency_ms, hard_latency_ms, elapsed_ms, coverage_json, omissions_json,
            uncertainty_json, validation_plan_json, canonical_json, context_hash, created_at
         ) VALUES (
            ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
            ?19,?20,?21,?22,?23,?24,?25,?26,?27,?28
         )",
        params![
            context_ir.context_id,
            context_ir.task.task_session_id,
            context_ir.workspace.workspace_id,
            context_ir.workspace.generation_id,
            context_ir.workspace.serving_generation_id,
            context_ir.schema_version,
            context_ir.policy.planner_policy_version,
            context_ir.policy.projection_policy_version,
            context_ir.policy.budget_profile,
            request_hash,
            serialized_enum(&context_ir.status.working_set_status)?,
            context_ir.status.serving_fallback,
            context_ir.cost.max_records,
            context_ir.cost.selected_records,
            context_ir.cost.max_source_bytes,
            context_ir.cost.selected_source_bytes,
            context_ir.cost.max_estimated_tokens,
            context_ir.cost.selected_estimated_tokens,
            context_ir.cost.soft_latency_ms,
            context_ir.cost.hard_latency_ms,
            context_ir.cost.elapsed_ms,
            serde_json::to_string(&context_ir.coverage)?,
            serde_json::to_string(&context_ir.omissions)?,
            serde_json::to_string(&context_ir.uncertainty)?,
            serde_json::to_string(&context_ir.validation_plan)?,
            canonical_json,
            context_ir.context_hash,
            crate::migrations::iso8601_now(),
        ],
    )?;

    let stored_hash: String = tx.query_row(
        "SELECT context_hash FROM context_ir WHERE context_id = ?1",
        params![context_ir.context_id],
        |r| r.get(0),
    )?;
    if stored_hash != context_ir.context_hash {
        return Err(AtlasError::Other(format!(
            "context_id {} already identifies a different sealed Context IR",
            context_ir.context_id,
        )));
    }

    for (ordinal, item) in context_ir.working_set.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO context_ir_item (
                context_id, ordinal, item_id, entity_kind, entity_id, role, selection_reason,
                origin_id, graph_distance, evidence_state, confidence, provider_fingerprint,
                source_revision_hash, metadata_bytes, source_bytes, estimated_tokens, source_path,
                source_start_byte, source_end_byte, item_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                context_ir.context_id,
                ordinal as i64,
                item.item_id,
                serialized_enum(&item.entity_kind)?,
                item.entity_id,
                serialized_enum(&item.role)?,
                serialized_enum(&item.selection_reason)?,
                item.origin_id,
                item.distance,
                serialized_enum(&item.evidence.state)?,
                item.evidence.confidence,
                item.evidence.provider_fingerprint,
                item.evidence.source_revision_hash,
                item.cost.metadata_bytes,
                item.cost.source_bytes,
                item.cost.estimated_tokens,
                item.source.as_ref().map(|source| source.path.as_str()),
                item.source.as_ref().map(|source| source.start_byte),
                item.source.as_ref().map(|source| source.end_byte),
                serde_json::to_string(item)?,
            ],
        )?;
    }
    Ok(())
}

/// Persist the sealed Context IR and append Atlas-observed events for the
/// items and relationships this production call actually supplied.
///
/// Repeated identical calls are distinct observable deliveries. Their
/// deterministic `delivery_ordinal` is allocated inside one SQLite
/// transaction; `ContextRecompiled` is intentionally not inferred.
pub(crate) fn record_compiled_context_use(
    conn: &mut Connection,
    task_session_id: &str,
    request_hash: &str,
    context_ir: &ContextIr,
) -> Result<()> {
    if context_ir.task.task_session_id != task_session_id {
        return Err(AtlasError::InvalidConfig(
            "Context IR task_session_id does not match telemetry session".to_string(),
        ));
    }
    let identity = LegacyTaskSessionIdentity {
        workspace_id: &context_ir.workspace.workspace_id,
        start_generation_id: &context_ir.workspace.generation_id,
        task_hash: &context_ir.task.task_hash,
        normalized_goal_hash: &context_ir.task.normalized_goal_hash,
        task_kind: context_ir.task.task_kind,
        kind_source: context_ir.task.kind_source,
        kind_rule_id: context_ir.task.kind_rule_id.as_deref(),
    };
    validate_legacy_task_session_identity(conn, task_session_id, &identity)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    persist_context_ir(&tx, request_hash, context_ir)?;
    if context_ir.status.working_set_status == WorkingSetStatus::Blocked {
        tx.commit()?;
        return Ok(());
    }

    let prior_details: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT details_json FROM context_use_event
             WHERE task_session_id = ?1 AND context_id = ?2
               AND event_type IN ('context_supplied', 'relationship_traversed')",
        )?;
        let rows = stmt.query_map(params![task_session_id, context_ir.context_id], |r| {
            r.get(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let delivery_ordinal = prior_details
        .iter()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .filter_map(|details| details.get("delivery_ordinal")?.as_i64())
        .max()
        .unwrap_or(0)
        + 1;
    let occurred_at = crate::migrations::iso8601_now();

    for item in &context_ir.working_set {
        let event = ContextUseEvent {
            schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
            event_id: event_id_for(
                task_session_id,
                ContextUseEventType::ContextSupplied,
                &format!(
                    "{}:{delivery_ordinal}:{}",
                    context_ir.context_hash, item.item_id
                ),
            ),
            task_session_id: task_session_id.to_string(),
            context_id: Some(context_ir.context_id.clone()),
            item_id: Some(item.item_id.clone()),
            entity_id: Some(item.entity_id.clone()),
            event_type: ContextUseEventType::ContextSupplied,
            observation_source: ObservationSource::AtlasObserved,
            occurred_at: occurred_at.clone(),
            bytes: Some(
                item.cost
                    .metadata_bytes
                    .saturating_add(item.cost.source_bytes),
            ),
            details: serde_json::json!({
                "delivery_ordinal": delivery_ordinal,
                "entity_kind": item.entity_kind,
                "selection_reason": item.selection_reason,
                "distance": item.distance,
                "metadata_bytes": item.cost.metadata_bytes,
                "source_bytes": item.cost.source_bytes,
                "estimated_tokens": item.cost.estimated_tokens,
            }),
        };
        record_context_use_event(&tx, task_session_id, &event)?;
    }

    for relationship in &context_ir.relationships {
        let event = ContextUseEvent {
            schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
            event_id: event_id_for(
                task_session_id,
                ContextUseEventType::RelationshipTraversed,
                &format!(
                    "{}:{delivery_ordinal}:{}",
                    context_ir.context_hash, relationship.relationship_id,
                ),
            ),
            task_session_id: task_session_id.to_string(),
            context_id: Some(context_ir.context_id.clone()),
            item_id: None,
            entity_id: Some(relationship.target_entity_id.clone()),
            event_type: ContextUseEventType::RelationshipTraversed,
            observation_source: ObservationSource::AtlasObserved,
            occurred_at: occurred_at.clone(),
            bytes: None,
            details: serde_json::json!({
                "delivery_ordinal": delivery_ordinal,
                "relationship_id": relationship.relationship_id,
                "source_entity_id": relationship.source_entity_id,
                "target_entity_id": relationship.target_entity_id,
                "relationship_type": relationship.relationship_type,
                "resolution_state": relationship.resolution_state,
            }),
        };
        record_context_use_event(&tx, task_session_id, &event)?;
    }

    tx.commit()?;
    Ok(())
}

/// Full, ordered timeline of context-use events for one task session.
/// Deterministic ordering (`occurred_at`, then `event_id` as a stable
/// tie-break for same-timestamp events).
pub fn task_session_events(
    conn: &Connection,
    task_session_id: &str,
) -> Result<Vec<ContextUseEvent>> {
    task_session_events_through(conn, task_session_id, None)
}

fn task_session_events_through(
    conn: &Connection,
    task_session_id: &str,
    captured_event_rowid: Option<i64>,
) -> Result<Vec<ContextUseEvent>> {
    let mut stmt = conn.prepare(
        "SELECT event_id, context_id, item_id, entity_id, event_type, observation_source,
                byte_count, details_json, occurred_at
         FROM context_use_event
         WHERE task_session_id = ?1 AND (?2 IS NULL OR rowid <= ?2)
         ORDER BY occurred_at, event_id",
    )?;
    let rows: Vec<ContextUseEvent> = stmt
        .query_map(params![task_session_id, captured_event_rowid], |r| {
            let event_type_raw: String = r.get(4)?;
            let observation_source_raw: String = r.get(5)?;
            let details_raw: String = r.get(7)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                event_type_raw,
                observation_source_raw,
                r.get::<_, Option<i64>>(6)?,
                details_raw,
                r.get::<_, String>(8)?,
            ))
        })?
        .filter_map(std::result::Result::ok)
        .filter_map(
            |(
                event_id,
                context_id,
                item_id,
                entity_id,
                event_type_raw,
                obs_raw,
                bytes,
                details_raw,
                occurred_at,
            )| {
                Some(ContextUseEvent {
                    schema_version: crate::context_ir::CONTEXT_SCHEMA_VERSION.to_string(),
                    event_id,
                    task_session_id: task_session_id.to_string(),
                    context_id,
                    item_id,
                    entity_id,
                    event_type: event_type_from_str(&event_type_raw)?,
                    observation_source: observation_source_from_str(&obs_raw)?,
                    occurred_at,
                    bytes,
                    details: serde_json::from_str(&details_raw)
                        .unwrap_or_else(|_| serde_json::json!({})),
                })
            },
        )
        .collect();
    Ok(rows)
}

fn context_metric_kind(
    event: &ContextUseEvent,
    raw: &str,
) -> std::result::Result<CanonicalEvidenceKind, ContextMetricInvalidity> {
    match raw {
        "file" => Ok(CanonicalEvidenceKind::File),
        "symbol" => Ok(CanonicalEvidenceKind::Symbol),
        "config" => Ok(CanonicalEvidenceKind::Config),
        "test" => Ok(CanonicalEvidenceKind::Test),
        "document" => Ok(CanonicalEvidenceKind::Document),
        "external" => Ok(CanonicalEvidenceKind::External),
        _ => Err(ContextMetricInvalidity::UnsupportedEntityKind {
            event_id: event.event_id.clone(),
            entity_kind: raw.to_string(),
        }),
    }
}

fn metric_identity(
    event: &ContextUseEvent,
    supplied_by_entity: &std::collections::BTreeMap<String, CanonicalEvidenceId>,
) -> std::result::Result<CanonicalEvidenceId, ContextMetricInvalidity> {
    if let Some(entity_id) = event.entity_id.as_deref() {
        if let Some(identity) = supplied_by_entity.get(entity_id) {
            return Ok(identity.clone());
        }
    }

    if let Some(relationship_id) = event.details["relationship_id"].as_str() {
        return CanonicalEvidenceId::relationship(relationship_id);
    }

    if event.event_type == ContextUseEventType::ExactSourceRequested {
        if let Some(path_hash) = event.details["path_hash"].as_str() {
            if event.details["request_kind"].as_str() == Some("source") {
                if let Some(identity) = supplied_by_entity.get(path_hash) {
                    return Ok(identity.clone());
                }
            }
            let request_kind = event.details["request_kind"].as_str();
            if request_kind == Some("source_range") {
                let start = event.details["requested_start_line"].as_u64();
                let end = event.details["requested_end_line"].as_u64();
                if let (Some(start), Some(end)) = (start, end) {
                    return CanonicalEvidenceId::range(path_hash, start, end);
                }
            }
            return CanonicalEvidenceId::entity(CanonicalEvidenceKind::File, path_hash);
        }
    }

    let entity_id = event.entity_id.as_deref().ok_or_else(|| {
        ContextMetricInvalidity::MissingEvidenceIdentity {
            event_id: event.event_id.clone(),
        }
    })?;
    let kind = match event.event_type {
        ContextUseEventType::ArtifactChanged => CanonicalEvidenceKind::File,
        ContextUseEventType::RelationshipTraversed => CanonicalEvidenceKind::Relationship,
        _ if entity_id.starts_with("query:") => CanonicalEvidenceKind::Query,
        _ => match event.details["operation"].as_str() {
            Some("inspect_path") => CanonicalEvidenceKind::File,
            Some("inspect_symbol") | Some("find") => CanonicalEvidenceKind::Query,
            _ => CanonicalEvidenceKind::File,
        },
    };
    CanonicalEvidenceId::entity(kind, entity_id)
}

/// Normalize the existing closed task-session event vocabulary into the four
/// evidence classes consumed by pure Context Yield set algebra.
///
/// Only successful Atlas observations become reads. Only agent/operator
/// reported-use variants become reported evidence, so a source read can never
/// be promoted to a reasoning claim.
pub fn context_metric_evidence_from_events(
    events: &[ContextUseEvent],
) -> std::result::Result<ContextMetricEvidence, ContextMetricInvalidity> {
    let mut evidence = ContextMetricEvidence::default();
    let mut supplied_by_entity = std::collections::BTreeMap::<String, CanonicalEvidenceId>::new();

    for event in events {
        if event.observation_source != ObservationSource::AtlasObserved {
            continue;
        }
        let is_supplied_item = event.event_type == ContextUseEventType::ContextSupplied;
        let is_supplied_relationship = event.event_type
            == ContextUseEventType::RelationshipTraversed
            && event.details.get("delivery_ordinal").is_some();
        if !is_supplied_item && !is_supplied_relationship {
            continue;
        }

        let identity = if is_supplied_relationship {
            metric_identity(event, &supplied_by_entity)?
        } else {
            let entity_kind = event.details["entity_kind"].as_str().ok_or_else(|| {
                ContextMetricInvalidity::MissingEvidenceIdentity {
                    event_id: event.event_id.clone(),
                }
            })?;
            let entity_id = event.entity_id.as_deref().ok_or_else(|| {
                ContextMetricInvalidity::MissingEvidenceIdentity {
                    event_id: event.event_id.clone(),
                }
            })?;
            CanonicalEvidenceId::entity(context_metric_kind(event, entity_kind)?, entity_id)?
        };
        if let Some(entity_id) = event.entity_id.as_ref() {
            supplied_by_entity.insert(entity_id.clone(), identity.clone());
            if identity.kind() == CanonicalEvidenceKind::File {
                supplied_by_entity.insert(
                    content_hash_of_bytes(entity_id.as_bytes()),
                    identity.clone(),
                );
            }
        }
        let source_bytes = if is_supplied_relationship {
            0
        } else {
            let source_bytes = event.details["source_bytes"].as_i64().ok_or_else(|| {
                ContextMetricInvalidity::InvalidSourceBytes {
                    identity: identity.clone(),
                }
            })?;
            u64::try_from(source_bytes).map_err(|_| {
                ContextMetricInvalidity::InvalidSourceBytes {
                    identity: identity.clone(),
                }
            })?
        };
        evidence
            .supplied
            .push(SuppliedEvidence::new(identity, source_bytes));
    }

    for event in events {
        match event.event_type {
            ContextUseEventType::ContextSupplied => {}
            ContextUseEventType::RelationshipTraversed
                if event.details.get("delivery_ordinal").is_some() => {}
            ContextUseEventType::ExactSourceRequested
            | ContextUseEventType::EntityQueried
            | ContextUseEventType::RelationshipTraversed
                if event.observation_source == ObservationSource::AtlasObserved =>
            {
                let successful = match event.event_type {
                    ContextUseEventType::ExactSourceRequested => {
                        event.details["status"].as_str() == Some("verified")
                    }
                    ContextUseEventType::EntityQueried => {
                        event.details["status"].as_str() == Some("succeeded")
                    }
                    ContextUseEventType::RelationshipTraversed => {
                        event.details["status"].as_str() == Some("succeeded")
                    }
                    _ => false,
                };
                if successful {
                    evidence.reads.push(ObservedEvidence::new(metric_identity(
                        event,
                        &supplied_by_entity,
                    )?));
                }
            }
            ContextUseEventType::ReasoningUseReported
            | ContextUseEventType::ModificationTargetReported
            | ContextUseEventType::TestConsideredReported
                if matches!(
                    event.observation_source,
                    ObservationSource::AgentReported | ObservationSource::OperatorReported
                ) =>
            {
                evidence
                    .reported
                    .push(metric_identity(event, &supplied_by_entity)?);
            }
            ContextUseEventType::ArtifactChanged
                if event.observation_source == ObservationSource::AtlasObserved =>
            {
                evidence
                    .changed
                    .push(metric_identity(event, &supplied_by_entity)?);
            }
            _ => {}
        }
    }
    Ok(evidence)
}

/// Load a session timeline and derive its Context Yield metrics without
/// persisting a report or extending the closed event/schema vocabulary.
pub fn task_session_context_metrics(
    conn: &Connection,
    task_session_id: &str,
) -> Result<ContextMetrics> {
    task_session_context_metrics_through(conn, task_session_id, None)
}

fn task_session_context_metrics_through(
    conn: &Connection,
    task_session_id: &str,
    captured_event_rowid: Option<i64>,
) -> Result<ContextMetrics> {
    let event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM context_use_event
         WHERE task_session_id = ?1 AND (?2 IS NULL OR rowid <= ?2)",
        params![task_session_id, captured_event_rowid],
        |row| row.get(0),
    )?;
    let event_count = usize::try_from(event_count).map_err(|_| {
        AtlasError::InvalidConfig(format!(
            "task session {task_session_id} has an invalid context event count"
        ))
    })?;
    if event_count > MAX_CONTEXT_METRIC_EVIDENCE {
        return Err(AtlasError::InvalidConfig(format!(
            "task session {task_session_id} has {event_count} context events; metric limit is {MAX_CONTEXT_METRIC_EVIDENCE}"
        )));
    }
    let events = task_session_events_through(conn, task_session_id, captured_event_rowid)?;
    let evidence = context_metric_evidence_from_events(&events).map_err(|invalidity| {
        AtlasError::InvalidConfig(format!(
            "task session {task_session_id} has invalid context metric evidence: {invalidity:?}"
        ))
    })?;
    derive_context_metrics(&evidence).map_err(|invalidity| {
        AtlasError::InvalidConfig(format!(
            "task session {task_session_id} context metrics are invalid: {invalidity:?}"
        ))
    })
}

/// Per-`(event_type, observation_source)` count for one task session — the
/// "production-populated metrics" evidence G5's milestone gate requires:
/// proves telemetry is genuinely source-distinguished, not just typed.
pub fn event_counts_by_type_and_source(
    conn: &Connection,
    task_session_id: &str,
) -> Result<std::collections::BTreeMap<(String, String), i64>> {
    let mut stmt = conn.prepare(
        "SELECT event_type, observation_source, COUNT(*) FROM context_use_event
         WHERE task_session_id = ?1 GROUP BY event_type, observation_source",
    )?;
    let rows = stmt.query_map(params![task_session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    Ok(rows
        .filter_map(std::result::Result::ok)
        .map(|(t, s, c)| ((t, s), c))
        .collect())
}
/// Exact literal required by the internal privacy-deletion boundary.
pub const PRIVACY_DELETION_CONFIRMATION: &str = "--confirm-privacy-deletion";
pub const STRUCTURED_RETENTION_DAYS: i64 = 30;
pub const SUMMARY_RETENTION_DAYS: i64 = 180;
pub const MAX_COMPLETED_SESSIONS: i64 = 1_000;
pub const MAX_EXPIRED_SESSIONS_PER_COMPACTION: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyCompactionAction {
    DeleteDerived,
    DeleteSession,
    DeleteMetric,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PrivacyCompactionEntry {
    pub record_id: String,
    pub action: PrivacyCompactionAction,
    pub context_count: u64,
    pub event_count: u64,
    pub metric_count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyCompactionPage {
    pub workspace_id: String,
    pub manifest_digest: String,
    pub total_entries: usize,
    pub entries: Vec<PrivacyCompactionEntry>,
    pub next_cursor: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyCompactionFault {
    None,
    BeforeCommit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PrivacyCompactionResult {
    pub deleted_sessions: u64,
    pub deleted_contexts: u64,
    pub deleted_events: u64,
    pub deleted_metrics: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionStatusPage {
    pub workspace_id: String,
    pub structured_retention_days: i64,
    pub summary_retention_days: i64,
    pub max_completed_sessions: i64,
    pub max_expired_records_per_compaction: i64,
    pub manifest_digest: String,
    pub total_expired_entries: usize,
    pub entries: Vec<PrivacyCompactionEntry>,
    pub next_cursor: Option<usize>,
}

#[derive(serde::Serialize)]
struct PrivacyCompactionIdentity<'a> {
    schema: &'static str,
    catalogue_identity: &'a str,
    workspace_id: &'a str,
    entries: &'a [PrivacyCompactionEntry],
}

fn privacy_compaction_catalogue_identity(conn: &Connection) -> Result<String> {
    let selected_path = conn.path().ok_or_else(|| {
        AtlasError::Other("privacy compaction requires a file-backed catalogue".into())
    })?;
    let selected = crate::paths::canonical_regular_file(
        std::path::Path::new(selected_path),
        "privacy compaction catalogue",
    )?;

    let mut statement = conn.prepare("PRAGMA database_list")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let main_paths = rows
        .filter_map(|row| match row {
            Ok((name, path)) if name == "main" => Some(Ok(path)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let [opened_path] = main_paths.as_slice() else {
        return Err(AtlasError::Other(
            "privacy compaction catalogue identity is ambiguous".into(),
        ));
    };
    if opened_path.is_empty() {
        return Err(AtlasError::Other(
            "privacy compaction requires a file-backed catalogue".into(),
        ));
    }
    let opened = crate::paths::canonical_regular_file(
        std::path::Path::new(opened_path),
        "opened privacy compaction catalogue",
    )?;
    let revalidated = crate::paths::canonical_regular_file(
        std::path::Path::new(selected_path),
        "privacy compaction catalogue",
    )?;
    if !crate::workspace::paths_have_same_identity(&selected, &opened)
        || !crate::workspace::paths_have_same_identity(&selected, &revalidated)
    {
        return Err(AtlasError::Other(
            "privacy compaction catalogue identity changed".into(),
        ));
    }
    Ok(crate::paths::path_as_utf8(&selected, "canonical privacy compaction catalogue")?.to_owned())
}

fn privacy_compaction_manifest(
    conn: &Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
) -> Result<(Vec<PrivacyCompactionEntry>, String)> {
    let catalogue_identity = privacy_compaction_catalogue_identity(conn)?;
    let workspace_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace WHERE workspace_id = ?1)",
        params![workspace_id],
        |row| row.get(0),
    )?;
    if !workspace_exists {
        return Err(AtlasError::WorkspaceNotFound {
            workspace_id: workspace_id.to_string(),
        });
    }

    let structured_cutoff = (as_of - chrono::Duration::days(STRUCTURED_RETENTION_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let summary_cutoff = (as_of - chrono::Duration::days(SUMMARY_RETENTION_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut statement = conn.prepare(
        "WITH ranked AS (
            SELECT task_session_id, completed_at, raw_task,
                   ROW_NUMBER() OVER (
                       PARTITION BY workspace_id
                       ORDER BY julianday(completed_at) DESC, task_session_id
                   ) AS completed_rank
            FROM task_session
            WHERE workspace_id = ?1
              AND state IN ('completed', 'abandoned', 'failed')
              AND completed_at IS NOT NULL
         )
         SELECT task_session_id, completed_at
         FROM ranked
         WHERE julianday(completed_at) < julianday(?3)
            OR (
                (julianday(completed_at) < julianday(?2) OR completed_rank > ?4)
                AND (
                    raw_task IS NOT NULL
                    OR EXISTS (
                        SELECT 1 FROM context_ir
                        WHERE context_ir.task_session_id = ranked.task_session_id
                    )
                    OR EXISTS (
                        SELECT 1 FROM context_use_event
                        WHERE context_use_event.task_session_id = ranked.task_session_id
                    )
                    OR EXISTS (
                        SELECT 1 FROM query_stage_metric
                        WHERE query_stage_metric.task_session_id = ranked.task_session_id
                    )
                )
            )
         ORDER BY julianday(completed_at), task_session_id
         LIMIT ?5",
    )?;
    let rows = statement.query_map(
        params![
            workspace_id,
            structured_cutoff,
            summary_cutoff,
            MAX_COMPLETED_SESSIONS,
            MAX_EXPIRED_SESSIONS_PER_COMPACTION
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    let mut entries = Vec::new();
    for row in rows {
        let (record_id, completed_at) = row?;
        let completed_at = chrono::DateTime::parse_from_rfc3339(&completed_at)
            .map_err(|error| {
                AtlasError::Other(format!(
                    "invalid task session completion timestamp for {record_id}: {error}"
                ))
            })?
            .with_timezone(&chrono::Utc);
        let action = if completed_at < as_of - chrono::Duration::days(SUMMARY_RETENTION_DAYS) {
            PrivacyCompactionAction::DeleteSession
        } else {
            PrivacyCompactionAction::DeleteDerived
        };
        let context_count = conn.query_row(
            "SELECT COUNT(*) FROM context_ir WHERE task_session_id = ?1",
            params![record_id],
            |row| row.get::<_, u64>(0),
        )?;
        let event_count = conn.query_row(
            "SELECT COUNT(*) FROM context_use_event WHERE task_session_id = ?1",
            params![record_id],
            |row| row.get::<_, u64>(0),
        )?;
        let metric_count = conn.query_row(
            "SELECT COUNT(*) FROM query_stage_metric WHERE task_session_id = ?1",
            params![record_id],
            |row| row.get::<_, u64>(0),
        )?;
        entries.push(PrivacyCompactionEntry {
            record_id,
            action,
            context_count,
            event_count,
            metric_count,
        });
    }
    let mut metric_statement = conn.prepare(
        "SELECT metric_id
         FROM query_stage_metric
         WHERE workspace_id = ?1
           AND task_session_id IS NULL
           AND julianday(created_at) < julianday(?2)
         ORDER BY julianday(created_at), metric_id
         LIMIT ?3",
    )?;
    let metrics = metric_statement.query_map(
        params![
            workspace_id,
            structured_cutoff,
            MAX_EXPIRED_SESSIONS_PER_COMPACTION
        ],
        |row| row.get::<_, String>(0),
    )?;
    for metric in metrics {
        entries.push(PrivacyCompactionEntry {
            record_id: metric?,
            action: PrivacyCompactionAction::DeleteMetric,
            context_count: 0,
            event_count: 0,
            metric_count: 1,
        });
    }
    entries.sort_by(|left, right| {
        retention_action_discriminator(left.action)
            .cmp(retention_action_discriminator(right.action))
            .then_with(|| left.record_id.cmp(&right.record_id))
    });
    if entries
        .windows(2)
        .any(|pair| pair[0].action == pair[1].action && pair[0].record_id == pair[1].record_id)
    {
        return Err(AtlasError::Other(
            "privacy compaction manifest contains a duplicate ordering key".into(),
        ));
    }
    let revalidated_catalogue_identity = privacy_compaction_catalogue_identity(conn)?;
    if revalidated_catalogue_identity != catalogue_identity {
        return Err(AtlasError::Other(
            "privacy compaction catalogue identity changed".into(),
        ));
    }
    let identity = PrivacyCompactionIdentity {
        schema: "privacy-compaction-manifest-v2",
        catalogue_identity: &catalogue_identity,
        workspace_id,
        entries: &entries,
    };
    let digest = content_hash_of_bytes(&crate::provider_contract::canonical_json_bytes(&identity));
    Ok((entries, digest))
}

/// Compute a bounded presentation page over one complete canonical manifest.
/// `limit` and `cursor` never participate in the confirmation digest.
pub fn privacy_compaction_preview(
    conn: &Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
    limit: usize,
    cursor: Option<usize>,
) -> Result<PrivacyCompactionPage> {
    if !(1..=200).contains(&limit) {
        return Err(AtlasError::Other(
            "privacy compaction preview limit must be in 1..=200".into(),
        ));
    }
    let (entries, manifest_digest) = privacy_compaction_manifest(conn, workspace_id, as_of)?;
    let start = cursor.unwrap_or(0);
    if start > entries.len() {
        return Err(AtlasError::Other(
            "privacy compaction preview cursor is invalid".into(),
        ));
    }
    let end = start.saturating_add(limit).min(entries.len());
    Ok(PrivacyCompactionPage {
        workspace_id: workspace_id.to_string(),
        manifest_digest,
        total_entries: entries.len(),
        entries: entries[start..end].to_vec(),
        next_cursor: (end < entries.len()).then_some(end),
    })
}

/// Return the fixed retention policy and one bounded page of the current
/// complete privacy-compaction manifest.
pub fn retention_status(
    conn: &Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
    limit: usize,
    cursor: Option<usize>,
) -> Result<RetentionStatusPage> {
    let preview = privacy_compaction_preview(conn, workspace_id, as_of, limit, cursor)?;
    Ok(RetentionStatusPage {
        workspace_id: preview.workspace_id,
        structured_retention_days: STRUCTURED_RETENTION_DAYS,
        summary_retention_days: SUMMARY_RETENTION_DAYS,
        max_completed_sessions: MAX_COMPLETED_SESSIONS,
        max_expired_records_per_compaction: MAX_EXPIRED_SESSIONS_PER_COMPACTION,
        manifest_digest: preview.manifest_digest,
        total_expired_entries: preview.total_entries,
        entries: preview.entries,
        next_cursor: preview.next_cursor,
    })
}
/// Recompute and apply privacy deletion in one immediate transaction. Any
/// mismatch or fault returns before commit, so SQLite rolls the whole attempt
/// back and no backup containing expired data is created.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApplicationPrivacyCompactionPage {
    pub workspace_id: String,
    pub manifest_digest: String,
    pub total_entries: usize,
    pub entries: Vec<PrivacyCompactionEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApplicationRetentionStatusPage {
    pub workspace_id: String,
    pub structured_retention_days: i64,
    pub summary_retention_days: i64,
    pub max_completed_sessions: i64,
    pub max_expired_records_per_compaction: i64,
    pub manifest_digest: String,
    pub total_expired_entries: usize,
    pub entries: Vec<PrivacyCompactionEntry>,
    pub next_cursor: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionCursorState {
    as_of: String,
    manifest_digest: String,
    prefix_digest: String,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct RetentionOrderingKey {
    action: String,
    record_id: String,
}

fn retention_action_discriminator(action: PrivacyCompactionAction) -> &'static str {
    match action {
        PrivacyCompactionAction::DeleteDerived => "0_delete_derived",
        PrivacyCompactionAction::DeleteSession => "1_delete_session",
        PrivacyCompactionAction::DeleteMetric => "2_delete_metric",
    }
}

fn retention_entry_key(entry: &PrivacyCompactionEntry) -> RetentionOrderingKey {
    RetentionOrderingKey {
        action: retention_action_discriminator(entry.action).to_string(),
        record_id: entry.record_id.clone(),
    }
}
fn retention_prefix_digest(
    entries: &[PrivacyCompactionEntry],
    through: &RetentionOrderingKey,
) -> Option<String> {
    let mut digest =
        crate::context_application::ApplicationSnapshotHasher::new(b"retention-manifest-prefix");
    for entry in entries {
        digest.record(entry);
        if retention_entry_key(entry) == *through {
            return Some(digest.finish());
        }
    }
    None
}

pub fn application_privacy_compaction_preview(
    conn: &Connection,
    workspace: &WorkspaceRecord,
    requested_as_of: chrono::DateTime<chrono::Utc>,
    limit: usize,
    cursor: Option<&str>,
) -> std::result::Result<
    ApplicationPrivacyCompactionPage,
    crate::context_application::CatalogueContextApplicationError,
> {
    application_privacy_page(
        conn,
        workspace,
        requested_as_of,
        limit,
        cursor,
        crate::context_application::ApplicationCursorOperation::RetentionCompactPreview,
    )
}

fn application_privacy_page(
    conn: &Connection,
    workspace: &WorkspaceRecord,
    requested_as_of: chrono::DateTime<chrono::Utc>,
    limit: usize,
    cursor: Option<&str>,
    operation: crate::context_application::ApplicationCursorOperation,
) -> std::result::Result<
    ApplicationPrivacyCompactionPage,
    crate::context_application::CatalogueContextApplicationError,
> {
    if !(1..=crate::context_route::MAX_APPLICATION_PAGE_LIMIT).contains(&limit) {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        ))
        .into());
    }
    let identity_binding = crate::context_application::ApplicationCursorBinding {
        workspace_id: &workspace.workspace_id,
        operation,
        contract_version: LEGACY_LIFECYCLE_CONTRACT_VERSION,
        filter: crate::context_application::ApplicationCursorFilter::Retention {},
        captured_state: "",
    };
    let (as_of, expected_digest, expected_prefix_digest, ordering) = if let Some(token) = cursor {
        let decoded = crate::context_application::decode_application_cursor_snapshot(
            conn,
            workspace,
            &identity_binding,
            token,
        )?;
        let state: RetentionCursorState = serde_json::from_str(&decoded.captured_state)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        let ordering: RetentionOrderingKey = serde_json::from_str(&decoded.ordering_key)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        let as_of = chrono::DateTime::parse_from_rfc3339(&state.as_of)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?
            .with_timezone(&chrono::Utc);
        (
            as_of,
            Some(state.manifest_digest),
            Some(state.prefix_digest),
            Some(ordering),
        )
    } else {
        (requested_as_of, None, None, None)
    };
    let (entries, manifest_digest) =
        privacy_compaction_manifest(conn, &workspace.workspace_id, as_of)?;
    if expected_digest
        .as_ref()
        .is_some_and(|expected| expected != &manifest_digest)
    {
        return Err(crate::context_application::ApplicationCursorError::Stale.into());
    }
    if let (Some(expected), Some(ordering)) = (expected_prefix_digest.as_ref(), ordering.as_ref()) {
        if retention_prefix_digest(&entries, ordering).as_ref() != Some(expected) {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
    }
    let mut page_entries = entries
        .iter()
        .filter(|entry| {
            ordering
                .as_ref()
                .is_none_or(|ordering| retention_entry_key(entry) > *ordering)
        })
        .take(limit + 1)
        .cloned()
        .collect::<Vec<_>>();
    let has_more = page_entries.len() > limit;
    page_entries.truncate(limit);
    let next_cursor = if has_more {
        let ordering = retention_entry_key(
            page_entries
                .last()
                .ok_or_else(|| AtlasError::InvalidConfig("cursor_invalid".into()))?,
        );
        let prefix_digest = retention_prefix_digest(&entries, &ordering)
            .ok_or(crate::context_application::ApplicationCursorError::Stale)?;
        let state = serde_json::to_string(&RetentionCursorState {
            as_of: as_of.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            manifest_digest: manifest_digest.clone(),
            prefix_digest,
        })?;
        let binding = crate::context_application::ApplicationCursorBinding {
            captured_state: &state,
            ..identity_binding
        };
        Some(crate::context_application::encode_application_cursor(
            conn,
            workspace,
            &binding,
            &serde_json::to_string(&ordering)?,
        )?)
    } else {
        None
    };
    Ok(ApplicationPrivacyCompactionPage {
        workspace_id: workspace.workspace_id.clone(),
        manifest_digest,
        total_entries: entries.len(),
        entries: page_entries,
        next_cursor,
    })
}

pub fn application_retention_status(
    conn: &Connection,
    workspace: &WorkspaceRecord,
    requested_as_of: chrono::DateTime<chrono::Utc>,
    limit: usize,
    cursor: Option<&str>,
) -> std::result::Result<
    ApplicationRetentionStatusPage,
    crate::context_application::CatalogueContextApplicationError,
> {
    let preview = application_privacy_page(
        conn,
        workspace,
        requested_as_of,
        limit,
        cursor,
        crate::context_application::ApplicationCursorOperation::RetentionStatus,
    )?;
    Ok(ApplicationRetentionStatusPage {
        workspace_id: preview.workspace_id,
        structured_retention_days: STRUCTURED_RETENTION_DAYS,
        summary_retention_days: SUMMARY_RETENTION_DAYS,
        max_completed_sessions: MAX_COMPLETED_SESSIONS,
        max_expired_records_per_compaction: MAX_EXPIRED_SESSIONS_PER_COMPACTION,
        manifest_digest: preview.manifest_digest,
        total_expired_entries: preview.total_entries,
        entries: preview.entries,
        next_cursor: preview.next_cursor,
    })
}

#[derive(Debug)]
enum PrivacyCompactionApplyError {
    Atlas(AtlasError),
    LiteralConfirmation,
    ManifestMismatch { expected: String, found: String },
}

impl From<AtlasError> for PrivacyCompactionApplyError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<rusqlite::Error> for PrivacyCompactionApplyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Atlas(error.into())
    }
}

fn apply_privacy_compaction_inner(
    conn: &mut Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
    expected_digest: &str,
    confirmation: &str,
    fault: PrivacyCompactionFault,
) -> std::result::Result<PrivacyCompactionResult, PrivacyCompactionApplyError> {
    if confirmation != PRIVACY_DELETION_CONFIRMATION {
        return Err(PrivacyCompactionApplyError::LiteralConfirmation);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (entries, digest) = privacy_compaction_manifest(&tx, workspace_id, as_of)?;
    if digest != expected_digest {
        return Err(PrivacyCompactionApplyError::ManifestMismatch {
            expected: expected_digest.to_string(),
            found: digest,
        });
    }

    let mut result = PrivacyCompactionResult::default();
    for entry in entries {
        if entry.action == PrivacyCompactionAction::DeleteMetric {
            result.deleted_metrics += tx.execute(
                "DELETE FROM query_stage_metric WHERE metric_id = ?1 AND workspace_id = ?2",
                params![entry.record_id, workspace_id],
            )? as u64;
            continue;
        }
        result.deleted_metrics += tx.execute(
            "DELETE FROM query_stage_metric WHERE task_session_id = ?1",
            params![entry.record_id],
        )? as u64;
        result.deleted_events += tx.execute(
            "DELETE FROM context_use_event WHERE task_session_id = ?1",
            params![entry.record_id],
        )? as u64;
        result.deleted_contexts += tx.execute(
            "DELETE FROM context_ir WHERE task_session_id = ?1",
            params![entry.record_id],
        )? as u64;
        if entry.action == PrivacyCompactionAction::DeleteSession {
            result.deleted_sessions += tx.execute(
                "DELETE FROM task_session WHERE task_session_id = ?1 AND workspace_id = ?2",
                params![entry.record_id, workspace_id],
            )? as u64;
        } else {
            tx.execute(
                "UPDATE task_session SET raw_task = NULL, raw_task_retention = 'none'
                 WHERE task_session_id = ?1 AND workspace_id = ?2",
                params![entry.record_id, workspace_id],
            )?;
        }
    }
    if fault == PrivacyCompactionFault::BeforeCommit {
        return Err(
            AtlasError::Other("injected privacy compaction fault before commit".into()).into(),
        );
    }
    tx.commit()?;
    Ok(result)
}

pub fn application_apply_privacy_compaction(
    conn: &mut Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
    expected_digest: &str,
    confirmation: &str,
    fault: PrivacyCompactionFault,
) -> std::result::Result<
    PrivacyCompactionResult,
    crate::context_application::CatalogueContextApplicationError,
> {
    apply_privacy_compaction_inner(
        conn,
        workspace_id,
        as_of,
        expected_digest,
        confirmation,
        fault,
    )
    .map_err(|error| match error {
        PrivacyCompactionApplyError::Atlas(error) => error.into(),
        PrivacyCompactionApplyError::LiteralConfirmation => {
            crate::context_application::ApplicationBoundaryError::LiteralConfirmation {
                operation: "privacy deletion",
                required: PRIVACY_DELETION_CONFIRMATION,
            }
            .into()
        }
        PrivacyCompactionApplyError::ManifestMismatch { expected, found } => {
            crate::context_application::ApplicationBoundaryError::ManifestMismatch {
                operation: "privacy compaction",
                expected,
                found,
            }
            .into()
        }
    })
}

pub fn apply_privacy_compaction(
    conn: &mut Connection,
    workspace_id: &str,
    as_of: chrono::DateTime<chrono::Utc>,
    expected_digest: &str,
    confirmation: &str,
    fault: PrivacyCompactionFault,
) -> Result<PrivacyCompactionResult> {
    apply_privacy_compaction_inner(
        conn,
        workspace_id,
        as_of,
        expected_digest,
        confirmation,
        fault,
    )
    .map_err(|error| match error {
        PrivacyCompactionApplyError::Atlas(error) => error,
        PrivacyCompactionApplyError::LiteralConfirmation => {
            AtlasError::Other("privacy deletion requires literal --confirm-privacy-deletion".into())
        }
        PrivacyCompactionApplyError::ManifestMismatch { expected, found } => AtlasError::Other(
            format!("privacy compaction manifest mismatch: expected {expected}, found {found}"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::context_ir::ContextUseEventType;
    use crate::discovery;
    use crate::workspace::register_workspace;
    use tempfile::tempdir;

    fn setup() -> (
        tempfile::TempDir,
        rusqlite::Connection,
        WorkspaceRecord,
        String,
    ) {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        let report = discovery::reconcile(&ws, &conn, &cfg).unwrap();
        (db_dir, conn, ws, report.candidate_generation_id)
    }

    fn h(n: u8) -> String {
        std::iter::repeat_n(char::from_digit(n as u32, 16).unwrap(), 64).collect()
    }

    #[test]
    fn create_task_session_persists_and_round_trips() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::HashOnly,
            None,
            TaskKind::BugFix,
            "1.0.0",
        )
        .unwrap();

        let (state, retention): (String, String) = conn
            .query_row(
                "SELECT state, raw_task_retention FROM task_session WHERE task_session_id = ?1",
                params![session.task_session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "created");
        assert_eq!(retention, "hash_only");
    }

    #[test]
    fn create_task_session_rejects_raw_task_without_local_opt_in() {
        let (_d, conn, ws, gen_id) = setup();
        let err = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            Some("do the thing"),
            TaskKind::BugFix,
            "1.0.0",
        );
        assert!(err.is_err(), "the persistence layer must re-enforce the privacy invariant, not just the document constructor");
    }

    #[test]
    fn lifecycle_reaches_reconciled_before_transactional_completion() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            None,
            TaskKind::Refactor,
            "1.0.0",
        )
        .unwrap();

        transition_task_session(
            &conn,
            &session.task_session_id,
            TaskSessionState::ContextCompiled,
        )
        .unwrap();
        transition_task_session(&conn, &session.task_session_id, TaskSessionState::Active).unwrap();
        transition_task_session(
            &conn,
            &session.task_session_id,
            TaskSessionState::Reconciled,
        )
        .unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM task_session WHERE task_session_id = ?1",
                params![session.task_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "reconciled");
    }

    #[test]
    fn illegal_transition_is_rejected() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            None,
            TaskKind::Explore,
            "1.0.0",
        )
        .unwrap();
        // created -> completed directly, skipping the required intermediate states.
        let err =
            transition_task_session(&conn, &session.task_session_id, TaskSessionState::Completed);
        assert!(
            err.is_err(),
            "created -> completed must be rejected as an illegal transition"
        );
    }

    #[test]
    fn terminal_state_never_transitions_further() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            None,
            TaskKind::Explore,
            "1.0.0",
        )
        .unwrap();
        transition_task_session(&conn, &session.task_session_id, TaskSessionState::Failed).unwrap();
        let err =
            transition_task_session(&conn, &session.task_session_id, TaskSessionState::Active);
        assert!(err.is_err(), "a failed session must never transition again");
    }

    #[test]
    fn context_use_events_are_recorded_ordered_and_source_distinguished() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            None,
            TaskKind::BugFix,
            "1.0.0",
        )
        .unwrap();

        let ev1 = ContextUseEvent {
            schema_version: "1.0.0".into(),
            event_id: event_id_for(
                &session.task_session_id,
                ContextUseEventType::ContextSupplied,
                "1",
            ),
            task_session_id: session.task_session_id.clone(),
            context_id: None,
            item_id: None,
            entity_id: None,
            event_type: ContextUseEventType::ContextSupplied,
            observation_source: ObservationSource::AtlasObserved,
            occurred_at: "2026-01-01T00:00:00.000Z".into(),
            bytes: Some(1024),
            details: serde_json::json!({}),
        };
        let ev2 = ContextUseEvent {
            schema_version: "1.0.0".into(),
            event_id: event_id_for(
                &session.task_session_id,
                ContextUseEventType::ReasoningUseReported,
                "1",
            ),
            task_session_id: session.task_session_id.clone(),
            context_id: None,
            item_id: None,
            entity_id: None,
            event_type: ContextUseEventType::ReasoningUseReported,
            observation_source: ObservationSource::AgentReported,
            occurred_at: "2026-01-01T00:01:00.000Z".into(),
            bytes: None,
            details: serde_json::json!({"note": "used for X"}),
        };
        record_context_use_event(&conn, &session.task_session_id, &ev1).unwrap();
        record_context_use_event(&conn, &session.task_session_id, &ev2).unwrap();

        let events = task_session_events(&conn, &session.task_session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, ContextUseEventType::ContextSupplied);
        assert_eq!(
            events[0].observation_source,
            ObservationSource::AtlasObserved
        );
        assert_eq!(
            events[1].observation_source,
            ObservationSource::AgentReported
        );

        let counts = event_counts_by_type_and_source(&conn, &session.task_session_id).unwrap();
        assert_eq!(
            counts.get(&("context_supplied".to_string(), "atlas_observed".to_string())),
            Some(&1)
        );
        assert_eq!(
            counts.get(&(
                "reasoning_use_reported".to_string(),
                "agent_reported".to_string()
            )),
            Some(&1)
        );
    }

    #[test]
    fn oversized_details_payload_is_rejected() {
        let (_d, conn, ws, gen_id) = setup();
        let session = create_task_session(
            &conn,
            &ws,
            &gen_id,
            &h(0),
            &h(1),
            RawTaskRetention::None,
            None,
            TaskKind::BugFix,
            "1.0.0",
        )
        .unwrap();
        let ev = ContextUseEvent {
            schema_version: "1.0.0".into(),
            event_id: event_id_for(
                &session.task_session_id,
                ContextUseEventType::ContextSupplied,
                "big",
            ),
            task_session_id: session.task_session_id.clone(),
            context_id: None,
            item_id: None,
            entity_id: None,
            event_type: ContextUseEventType::ContextSupplied,
            observation_source: ObservationSource::AtlasObserved,
            occurred_at: "2026-01-01T00:00:00.000Z".into(),
            bytes: None,
            details: serde_json::json!({"blob": "x".repeat(MAX_DETAILS_BYTES + 1)}),
        };
        let err = record_context_use_event(&conn, &session.task_session_id, &ev);
        assert!(err.is_err(), "an oversized details payload must be rejected -- telemetry is bounded metadata, not a raw dump");
    }
}
