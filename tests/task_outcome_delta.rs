use serde::{Deserialize, Serialize};
use serde_json::Value;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    ContextUseEventType, ObservationSource, RawTaskRetention, TaskKind, TaskSessionState,
    CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::generation::{
    activate_candidate, begin_candidate, fail_candidate, TriggerKind,
};
use workspace_atlas::generation_delta::compute_generation_delta;
use workspace_atlas::task_session::{
    complete_task_session, create_task_session, load_task_session, task_session_events,
    transition_task_session,
};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BaseEventType {
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
enum BaseObservationSource {
    AtlasObserved,
    AgentReported,
    OperatorReported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseEvent {
    schema_version: String,
    event_id: String,
    task_session_id: String,
    context_id: Option<String>,
    item_id: Option<String>,
    entity_id: Option<String>,
    event_type: BaseEventType,
    observation_source: BaseObservationSource,
    occurred_at: String,
    bytes: Option<i64>,
    details: Value,
}

unsafe extern "C" fn collect_sqlite_statements(
    trace_code: std::ffi::c_uint,
    context: *mut std::ffi::c_void,
    statement: *mut std::ffi::c_void,
    _expanded_sql: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if trace_code == rusqlite::ffi::SQLITE_TRACE_STMT as std::ffi::c_uint {
        // SAFETY: `SqliteStatementTrace` keeps the borrowed Vec alive and
        // unregisters this synchronous callback before releasing it.
        let statements = unsafe { &mut *context.cast::<Vec<String>>() };
        let statement = statement.cast::<rusqlite::ffi::sqlite3_stmt>();
        // SAFETY: SQLite supplies a live statement pointer for STMT events.
        let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement) };
        if !sql.is_null() {
            // SAFETY: sqlite3_sql returns a NUL-terminated string owned by the
            // live statement for the duration of this callback.
            statements.push(
                unsafe { std::ffi::CStr::from_ptr(sql) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    0
}

struct SqliteStatementTrace<'a> {
    connection: &'a rusqlite::Connection,
    _context: std::marker::PhantomData<&'a mut Vec<String>>,
}

impl<'a> SqliteStatementTrace<'a> {
    fn new(connection: &'a rusqlite::Connection, statements: &'a mut Vec<String>) -> Self {
        // SAFETY: the callback is synchronous on this non-shared Connection;
        // `_context` prevents the Vec from moving or dropping before `Drop`.
        let result = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                connection.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT as std::ffi::c_uint,
                Some(collect_sqlite_statements),
                (statements as *mut Vec<String>).cast(),
            )
        };
        assert_eq!(result, rusqlite::ffi::SQLITE_OK);
        Self {
            connection,
            _context: std::marker::PhantomData,
        }
    }
}

impl Drop for SqliteStatementTrace<'_> {
    fn drop(&mut self) {
        // SAFETY: this uses the same live Connection that registered the
        // callback and clears it before the borrowed context is released.
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                self.connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
    }
}

struct Fixture {
    _database_directory: tempfile::TempDir,
    workspace_directory: tempfile::TempDir,
    connection: rusqlite::Connection,
    workspace: WorkspaceRecord,
    start_generation_id: String,
}

fn setup() -> Fixture {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/a.rs"),
        "pub fn answer() -> u8 { 1 }\n",
    )
    .unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"task-outcome-delta\"\n",
    )
    .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    let workspace = register_workspace(
        &connection,
        workspace_directory.path(),
        &config,
        &catalogue,
        "1.0.0",
    )
    .unwrap();
    let start_generation_id = discovery::reconcile(&workspace, &connection, &config)
        .unwrap()
        .candidate_generation_id;
    Fixture {
        _database_directory: database_directory,
        workspace_directory,
        connection,
        workspace,
        start_generation_id,
    }
}

fn create_session_at(fixture: &Fixture, generation_id: &str, hash_digit: u8) -> String {
    let hash = format!("{hash_digit:064x}");
    create_task_session(
        &fixture.connection,
        &fixture.workspace,
        generation_id,
        &hash,
        &format!("{:064x}", hash_digit + 1),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap()
    .task_session_id
}

fn create_session(fixture: &Fixture, hash_digit: u8) -> String {
    create_session_at(fixture, &fixture.start_generation_id, hash_digit)
}

fn advance_to_active(connection: &rusqlite::Connection, task_session_id: &str) {
    transition_task_session(
        connection,
        task_session_id,
        TaskSessionState::ContextCompiled,
    )
    .unwrap();
    transition_task_session(connection, task_session_id, TaskSessionState::Active).unwrap();
}

#[test]
fn completion_links_exact_post_reconcile_artifacts_and_replay_is_intent_exact() {
    let mut fixture = setup();
    let task_session_id = create_session(&fixture, 1);
    advance_to_active(&fixture.connection, &task_session_id);

    std::fs::write(
        fixture.workspace_directory.path().join("src/a.rs"),
        "pub fn answer() -> u8 { 2 }\n",
    )
    .unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"task-outcome-delta\"\n",
    )
    .unwrap();
    let reconcile = discovery::reconcile(&fixture.workspace, &fixture.connection, &config).unwrap();
    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.start_generation_id,
        &reconcile.candidate_generation_id,
    )
    .unwrap();
    assert_eq!(delta.files.changes.len(), 1);
    assert_eq!(delta.files.changes[0].entity_id, "src/a.rs");

    let mut forged_delta = delta.clone();
    forged_delta.files.changes[0].entity_id = "src/forged.rs".to_string();
    let forged_error = complete_task_session(
        &mut fixture.connection,
        &task_session_id,
        &forged_delta,
        true,
        Some(true),
        "accepted_change",
        &reconcile.source_tree_hash,
    )
    .unwrap_err();
    assert!(forged_error.to_string().contains("delta_hash"));
    assert_eq!(
        load_task_session(&fixture.connection, &task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Reconciled
    );
    assert!(task_session_events(&fixture.connection, &task_session_id)
        .unwrap()
        .is_empty());

    complete_task_session(
        &mut fixture.connection,
        &task_session_id,
        &delta,
        true,
        Some(true),
        "accepted_change",
        &reconcile.source_tree_hash,
    )
    .unwrap();

    let completed = load_task_session(&fixture.connection, &task_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, TaskSessionState::Completed);
    assert_eq!(
        completed.end_generation_id.as_deref(),
        Some(delta.to_generation_id.as_str())
    );
    assert_eq!(
        completed.end_tree_hash.as_deref(),
        Some(reconcile.source_tree_hash.as_str())
    );
    assert_eq!(completed.accepted, Some(true));
    assert_eq!(completed.tests_passed, Some(true));
    assert_eq!(completed.outcome_code.as_deref(), Some("accepted_change"));

    let first_events = task_session_events(&fixture.connection, &task_session_id).unwrap();
    let outcome_events: Vec<_> = first_events
        .iter()
        .filter(|event| event.event_type == ContextUseEventType::OutcomeRecorded)
        .collect();
    assert_eq!(outcome_events.len(), 1);
    assert_eq!(
        outcome_events[0].observation_source,
        ObservationSource::AgentReported
    );
    assert_eq!(outcome_events[0].details["accepted"], true);
    assert_eq!(outcome_events[0].details["tests_passed"], true);
    assert_eq!(outcome_events[0].details["outcome_code"], "accepted_change");
    assert_eq!(
        outcome_events[0].details["end_generation_id"],
        delta.to_generation_id
    );
    assert_eq!(
        outcome_events[0].details["end_tree_hash"],
        reconcile.source_tree_hash
    );

    let artifact_events: Vec<_> = first_events
        .iter()
        .filter(|event| event.event_type == ContextUseEventType::ArtifactChanged)
        .collect();
    assert_eq!(artifact_events.len(), 1);
    assert_eq!(artifact_events[0].entity_id.as_deref(), Some("src/a.rs"));
    assert_eq!(
        artifact_events[0].observation_source,
        ObservationSource::AtlasObserved
    );
    assert_eq!(artifact_events[0].details["delta_id"], delta.delta_id);
    assert_eq!(artifact_events[0].details["change_kind"], "modified");
    assert_eq!(artifact_events[0].details["evidence_state"], "verified");
    assert!(artifact_events[0].details.get("caused_by").is_none());
    assert!(artifact_events[0].details.get("causality").is_none());

    for event in &first_events {
        let encoded = serde_json::to_value(event).unwrap();
        let decoded: BaseEvent = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    }

    complete_task_session(
        &mut fixture.connection,
        &task_session_id,
        &delta,
        true,
        Some(true),
        "accepted_change",
        &reconcile.source_tree_hash,
    )
    .unwrap();
    assert_eq!(
        task_session_events(&fixture.connection, &task_session_id)
            .unwrap()
            .len(),
        first_events.len(),
        "identical replay must be a no-op"
    );

    let error = complete_task_session(
        &mut fixture.connection,
        &task_session_id,
        &delta,
        false,
        Some(true),
        "rejected_change",
        &reconcile.source_tree_hash,
    )
    .unwrap_err();
    assert!(error.to_string().contains("conflicting completion replay"));
    assert_eq!(
        task_session_events(&fixture.connection, &task_session_id)
            .unwrap()
            .len(),
        first_events.len()
    );
}

#[test]
fn illegal_completion_is_end_to_end_fail_closed() {
    let mut fixture = setup();
    let task_session_id = create_session(&fixture, 3);

    std::fs::write(
        fixture.workspace_directory.path().join("src/a.rs"),
        "pub fn answer() -> u8 { 3 }\n",
    )
    .unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"task-outcome-delta\"\n",
    )
    .unwrap();
    let reconcile = discovery::reconcile(&fixture.workspace, &fixture.connection, &config).unwrap();
    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.start_generation_id,
        &reconcile.candidate_generation_id,
    )
    .unwrap();

    let error = complete_task_session(
        &mut fixture.connection,
        &task_session_id,
        &delta,
        true,
        None,
        "accepted_without_lifecycle",
        &reconcile.source_tree_hash,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("illegal task_session transition"));

    let unchanged = load_task_session(&fixture.connection, &task_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.state, TaskSessionState::Created);
    assert_eq!(unchanged.end_generation_id, None);
    assert_eq!(unchanged.accepted, None);
    assert!(task_session_events(&fixture.connection, &task_session_id)
        .unwrap()
        .is_empty());
    let persisted_delta_count: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM generation_delta WHERE delta_id = ?1",
            [&delta.delta_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        persisted_delta_count, 0,
        "illegal completion must persist nothing"
    );
}

#[test]
fn generation_activation_reconciles_all_and_only_older_active_legacy_sessions() {
    let fixture = setup();
    let first_active = create_session(&fixture, 10);
    let second_active = create_session(&fixture, 20);
    let created = create_session(&fixture, 30);
    let context_compiled = create_session(&fixture, 40);
    let failed = create_session(&fixture, 50);
    advance_to_active(&fixture.connection, &first_active);
    advance_to_active(&fixture.connection, &second_active);
    transition_task_session(
        &fixture.connection,
        &context_compiled,
        TaskSessionState::ContextCompiled,
    )
    .unwrap();
    transition_task_session(&fixture.connection, &failed, TaskSessionState::Failed).unwrap();

    std::fs::write(
        fixture.workspace_directory.path().join("src/a.rs"),
        "pub fn answer() -> u8 { 4 }\n",
    )
    .unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"task-outcome-delta\"\n",
    )
    .unwrap();
    let committed = discovery::reconcile(&fixture.workspace, &fixture.connection, &config).unwrap();

    for session_id in [&first_active, &second_active] {
        assert_eq!(
            load_task_session(&fixture.connection, session_id)
                .unwrap()
                .unwrap()
                .state,
            TaskSessionState::Reconciled
        );
    }
    for (session_id, expected) in [
        (&created, TaskSessionState::Created),
        (&context_compiled, TaskSessionState::ContextCompiled),
        (&failed, TaskSessionState::Failed),
    ] {
        assert_eq!(
            load_task_session(&fixture.connection, session_id)
                .unwrap()
                .unwrap()
                .state,
            expected
        );
    }
    let new_generation_session =
        create_session_at(&fixture, &committed.candidate_generation_id, 60);
    assert_eq!(
        load_task_session(&fixture.connection, &new_generation_session)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Created
    );
}

#[test]
fn generation_activation_acquires_immediate_transaction_before_verification() {
    let fixture = setup();
    let candidate = begin_candidate(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        TriggerKind::Reconcile,
        "immediate-provider-set",
        "1.0.0",
    )
    .unwrap();
    let mut statements = Vec::new();
    {
        let _trace = SqliteStatementTrace::new(&fixture.connection, &mut statements);
        activate_candidate(
            &fixture.connection,
            &fixture.workspace.workspace_id,
            &candidate.generation_id,
            Some("immediate-tree"),
        )
        .unwrap();
    }

    let transaction_statements: Vec<_> = statements
        .iter()
        .map(|sql| sql.trim().to_ascii_uppercase())
        .filter(|sql| sql.starts_with("BEGIN"))
        .collect();
    assert_eq!(transaction_statements, ["BEGIN IMMEDIATE"]);
}

#[test]
fn generation_activation_fault_rolls_back_pointer_candidate_and_session() {
    let fixture = setup();
    let active = create_session(&fixture, 70);
    advance_to_active(&fixture.connection, &active);
    let candidate = begin_candidate(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        TriggerKind::Reconcile,
        "fault-provider-set",
        "1.0.0",
    )
    .unwrap();
    fixture
        .connection
        .execute_batch(
            "CREATE TEMP TRIGGER fail_session_reconcile
             BEFORE UPDATE OF state ON task_session
             WHEN NEW.state = 'reconciled'
             BEGIN SELECT RAISE(ABORT, 'injected reconcile failure'); END;",
        )
        .unwrap();

    assert!(activate_candidate(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        &candidate.generation_id,
        Some("fault-tree"),
    )
    .is_err());
    let active_generation: String = fixture
        .connection
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            [&fixture.workspace.workspace_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_generation, fixture.start_generation_id);
    let candidate_state: String = fixture
        .connection
        .query_row(
            "SELECT state FROM index_generation WHERE generation_id = ?1",
            [&candidate.generation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(candidate_state, "candidate");
    assert_eq!(
        load_task_session(&fixture.connection, &active)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Active
    );

    fixture
        .connection
        .execute_batch("DROP TRIGGER fail_session_reconcile")
        .unwrap();
    fail_candidate(
        &fixture.connection,
        &candidate.generation_id,
        "injected",
        "abandoned after rollback proof",
    )
    .unwrap();
    assert!(activate_candidate(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        &candidate.generation_id,
        None,
    )
    .is_err());
    assert_eq!(
        load_task_session(&fixture.connection, &active)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Active
    );
}
