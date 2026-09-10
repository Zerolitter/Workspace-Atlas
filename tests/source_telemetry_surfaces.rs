use assert_cmd::Command;
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{RawTaskRetention, TaskKind, CONTEXT_SCHEMA_VERSION};
use workspace_atlas::discovery;
use workspace_atlas::task_session::{create_task_session, task_session_events};
use workspace_atlas::workspace::register_workspace;

fn run_cli_source(
    workspace_root: &std::path::Path,
    catalogue: &std::path::Path,
    task_session_id: Option<&str>,
) -> Value {
    let mut command = Command::cargo_bin("atlas").unwrap();
    command.args([
        "source",
        workspace_root.to_str().unwrap(),
        "src/a.ts",
        "--catalogue",
        catalogue.to_str().unwrap(),
    ]);
    if let Some(task_session_id) = task_session_id {
        command.args(["--task-session-id", task_session_id]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "atlas source failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_mcp_source(
    workspace_root: &std::path::Path,
    catalogue: &std::path::Path,
    task_session_id: Option<&str>,
) -> Value {
    let mut arguments = json!({
        "workspace_root": workspace_root,
        "path": "src/a.ts",
        "catalogue": catalogue,
    });
    if let Some(task_session_id) = task_session_id {
        arguments["task_session_id"] = json!(task_session_id);
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "atlas_source", "arguments": arguments},
    });
    let mut command = Command::cargo_bin("atlas-mcp").unwrap();
    let output = command
        .write_stdin(format!("{request}\n"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "atlas-mcp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        response.get("error").is_none(),
        "MCP returned an error: {response}"
    );
    response["result"]["structuredContent"].clone()
}

#[test]
fn source_cli_and_mcp_share_optional_task_session_attribution_and_legacy_behavior() {
    let db_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_dir.path().join("src")).unwrap();
    std::fs::write(
        workspace_dir.path().join("src/a.ts"),
        "export function alpha() { return 1; }\n",
    )
    .unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"source-telemetry\"\n",
    )
    .unwrap();
    let catalogue = db_dir.path().join("atlas.sqlite");
    let conn = init_catalogue(&catalogue, &config).unwrap();
    let workspace =
        register_workspace(&conn, workspace_dir.path(), &config, &catalogue, "1.0.0").unwrap();
    discovery::reconcile(&workspace, &conn, &config).unwrap();
    let generation_id: String = conn
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            [&workspace.workspace_id],
            |row| row.get(0),
        )
        .unwrap();
    let session = create_task_session(
        &conn,
        &workspace,
        &generation_id,
        &format!("{:064x}", 1),
        &format!("{:064x}", 2),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();

    let cli_attributed = run_cli_source(
        workspace_dir.path(),
        &catalogue,
        Some(&session.task_session_id),
    );
    let mcp_attributed = run_mcp_source(
        workspace_dir.path(),
        &catalogue,
        Some(&session.task_session_id),
    );
    assert_eq!(cli_attributed, mcp_attributed);
    let attributed_events = task_session_events(&conn, &session.task_session_id).unwrap();
    assert_eq!(attributed_events.len(), 2);
    assert_ne!(attributed_events[0].event_id, attributed_events[1].event_id);

    let cli_legacy = run_cli_source(workspace_dir.path(), &catalogue, None);
    let mcp_legacy = run_mcp_source(workspace_dir.path(), &catalogue, None);
    assert_eq!(cli_legacy, mcp_legacy);
    assert_eq!(
        cli_attributed, cli_legacy,
        "telemetry must not change SourceOutput"
    );
    assert_eq!(
        task_session_events(&conn, &session.task_session_id)
            .unwrap()
            .len(),
        2,
        "omitting task_session_id must follow the legacy no-telemetry path"
    );

    let malformed_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "atlas_source",
            "arguments": {
                "workspace_root": workspace_dir.path(),
                "path": "src/a.ts",
                "catalogue": catalogue,
                "task_session_id": 42,
            },
        },
    });
    let malformed_output = Command::cargo_bin("atlas-mcp")
        .unwrap()
        .write_stdin(format!("{malformed_request}\n"))
        .output()
        .unwrap();
    assert!(malformed_output.status.success());
    let malformed_response: Value = serde_json::from_slice(&malformed_output.stdout).unwrap();
    assert_eq!(
        malformed_response["error"]["data"]["kind"],
        "invalid_params"
    );
    assert_eq!(
        task_session_events(&conn, &session.task_session_id)
            .unwrap()
            .len(),
        2,
        "malformed attribution must fail closed"
    );
}
