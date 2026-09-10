use assert_cmd::Command;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    ContextUseEventType, ObservationSource, RawTaskRetention, TaskKind, TaskSessionState,
    ValidationKind, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::query::{record_validation_selection, QueryAttribution};
use workspace_atlas::task_session::{create_task_session, load_task_session, task_session_events};
use workspace_atlas::workspace::register_workspace;

fn run_cli(arguments: &[&str]) -> Value {
    let output = Command::cargo_bin("atlas")
        .unwrap()
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "atlas failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_mcp(request: Value) -> Value {
    let output = Command::cargo_bin("atlas-mcp")
        .unwrap()
        .write_stdin(format!("{request}\n"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response["result"]["structuredContent"].clone()
}

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

#[test]
fn attributed_queries_and_validation_emit_existing_events_without_changing_results() {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/a.ts"),
        "import { beta } from './b';\nexport function alpha() { return beta(); }\n",
    )
    .unwrap();
    std::fs::write(
        workspace_directory.path().join("src/b.ts"),
        "export function beta() { return 1; }\n",
    )
    .unwrap();
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"observation\"\n")
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
    let reconcile = discovery::reconcile(&workspace, &connection, &config).unwrap();
    let session = create_task_session(
        &connection,
        &workspace,
        &reconcile.candidate_generation_id,
        &format!("{:064x}", 1),
        &format!("{:064x}", 2),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();
    drop(connection);

    let root = workspace_directory.path().to_str().unwrap();
    let catalogue_path = catalogue.to_str().unwrap();
    let legacy_find = run_cli(&["find", root, "alpha", "--catalogue", catalogue_path]);
    let attributed_find = run_cli(&[
        "find",
        root,
        "alpha",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    assert_eq!(attributed_find, legacy_find);
    let missing_find = run_cli(&[
        "find",
        root,
        "definitely_missing",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    assert!(missing_find["results"].as_array().unwrap().is_empty());

    let mcp_inspect = run_mcp(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "atlas_inspect",
            "arguments": {
                "workspace_root": root,
                "path": "src/a.ts",
                "task_session_id": session.task_session_id,
                "catalogue": catalogue_path
            }
        }
    }));
    let legacy_inspect = run_cli(&["inspect", root, "src/a.ts", "--catalogue", catalogue_path]);
    assert_eq!(mcp_inspect, legacy_inspect);

    let attributed_trace = run_cli(&[
        "trace",
        root,
        "src/a.ts",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    let legacy_trace = run_cli(&["trace", root, "src/a.ts", "--catalogue", catalogue_path]);
    assert_eq!(attributed_trace, legacy_trace);
    let missing_trace = run_cli(&[
        "trace",
        root,
        "missing.ts",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    assert!(missing_trace["edges"].as_array().unwrap().is_empty());

    let connection = workspace_atlas::catalogue::open_connection(&catalogue, &config).unwrap();
    record_validation_selection(
        &connection,
        &workspace,
        QueryAttribution::new(&session.task_session_id),
        ValidationKind::Test,
        "cargo test --locked --test context_observation",
        true,
    )
    .unwrap();
    record_validation_selection(
        &connection,
        &workspace,
        QueryAttribution::new(&session.task_session_id),
        ValidationKind::Lint,
        "cargo clippy",
        false,
    )
    .unwrap();

    let events = task_session_events(&connection, &session.task_session_id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == ContextUseEventType::EntityQueried)
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == ContextUseEventType::RelationshipTraversed)
            .count(),
        2
    );
    let mut validation_statuses: Vec<&str> = events
        .iter()
        .filter(|event| event.event_type == ContextUseEventType::ValidationSelected)
        .filter_map(|event| event.details["status"].as_str())
        .collect();
    validation_statuses.sort_unstable();
    assert_eq!(validation_statuses, ["rejected", "selected"]);
    let validation_kinds: Vec<&str> = events
        .iter()
        .filter(|event| event.event_type == ContextUseEventType::ValidationSelected)
        .filter_map(|event| event.details["validation_kind"].as_str())
        .collect();
    assert!(validation_kinds.contains(&"test") && validation_kinds.contains(&"lint"));
    assert!(events
        .iter()
        .all(|event| event.observation_source == ObservationSource::AtlasObserved));
    assert_eq!(
        load_task_session(&connection, &session.task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Active
    );
    let rollback_session = create_task_session(
        &connection,
        &workspace,
        &reconcile.candidate_generation_id,
        &format!("{:064x}", 11),
        &format!("{:064x}", 12),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER fail_attributed_event
             BEFORE INSERT ON context_use_event
             WHEN NEW.task_session_id = (
                 SELECT task_session_id FROM task_session WHERE task_hash =
                 '000000000000000000000000000000000000000000000000000000000000000b'
             )
             BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
        )
        .unwrap();
    assert!(workspace_atlas::query::find_attributed(
        &connection,
        &workspace,
        "alpha",
        10,
        QueryAttribution::new(&rollback_session.task_session_id),
    )
    .is_err());
    assert_eq!(
        load_task_session(&connection, &rollback_session.task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Created
    );
    assert!(
        task_session_events(&connection, &rollback_session.task_session_id)
            .unwrap()
            .is_empty()
    );
    connection
        .execute_batch("DROP TRIGGER fail_attributed_event")
        .unwrap();

    for event in events {
        let encoded = serde_json::to_value(event).unwrap();
        let decoded: BaseEvent = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    }
}

#[test]
fn failed_exact_source_remains_distinguishable_from_successful_retrieval() {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::write(workspace_directory.path().join("a.rs"), "pub fn a() {}\n").unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"source-status\"\n",
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
    let reconcile = discovery::reconcile(&workspace, &connection, &config).unwrap();
    let session = create_task_session(
        &connection,
        &workspace,
        &reconcile.candidate_generation_id,
        &format!("{:064x}", 9),
        &format!("{:064x}", 10),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();
    drop(connection);

    let root = workspace_directory.path().to_str().unwrap();
    let catalogue_path = catalogue.to_str().unwrap();
    let success = run_cli(&[
        "source",
        root,
        "a.rs",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    assert_eq!(success["status"], "verified");
    std::fs::write(
        workspace_directory.path().join("a.rs"),
        "pub fn changed() {}\n",
    )
    .unwrap();
    let failure = run_cli(&[
        "source",
        root,
        "a.rs",
        "--task-session-id",
        &session.task_session_id,
        "--catalogue",
        catalogue_path,
    ]);
    assert_eq!(failure["status"], "hash_mismatch");

    let connection = rusqlite::Connection::open(&catalogue).unwrap();
    let statuses: Vec<String> = task_session_events(&connection, &session.task_session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.details["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statuses, ["verified", "hash_mismatch"]);
}
