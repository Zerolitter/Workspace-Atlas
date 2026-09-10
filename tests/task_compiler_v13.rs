use assert_cmd::Command;
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::discovery;
use workspace_atlas::workspace::register_workspace;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/a.ts"),
        "export function alpha() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        workspace_directory.path().join("src/b.ts"),
        "export function beta() { return 2; }\n",
    )
    .unwrap();

    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"v1.3-e2e\"\n")
            .unwrap();
    let database_path = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&database_path, &config).unwrap();
    let workspace = register_workspace(
        &connection,
        workspace_directory.path(),
        &config,
        &database_path,
        "1.0.0",
    )
    .unwrap();
    discovery::reconcile(&workspace, &connection, &config).unwrap();
    let generation_id: String = connection
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            rusqlite::params![workspace.workspace_id],
            |row| row.get(0),
        )
        .unwrap();
    let alpha: String = connection
        .query_row(
            "SELECT canonical_symbol_key FROM current_symbol
             WHERE display_name = 'alpha' ORDER BY canonical_symbol_key LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (beta, beta_revision, beta_run): (String, String, String) = connection
        .query_row(
            "SELECT sf.canonical_symbol_key, cf.revision_id, sf.extractor_run_id
             FROM current_file cf
             JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
             WHERE sf.display_name = 'beta' ORDER BY sf.canonical_symbol_key LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE file_revision SET artifact_class = 'test', is_test = 1
             WHERE revision_id = ?1",
            rusqlite::params![beta_revision],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                start_byte, end_byte, attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (
                'rel_v13_test_import', ?1, ?2, 'imports', 'symbol', ?3, 'symbol',
                'raw-alpha', NULL, NULL, '{}', 'semantic', 0.95, 'V1.3 transport fixture'
             )",
            rusqlite::params![beta_run, beta_revision, beta],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO relationship_resolution (
                relationship_resolution_id, workspace_id, generation_id,
                relationship_fact_id, resolver_execution_id, resolver_policy_version,
                status, resolved_ref_kind, resolved_ref_value, reason_code,
                candidate_refs_json, confidence, evidence_json, created_at
             ) VALUES (
                'resolution_v13_test_import', ?1, ?2, 'rel_v13_test_import', NULL,
                'test-policy', 'resolved_symbol', 'symbol', ?3, 'test', '[]', 0.95,
                '[]', '2026-09-02T00:00:00Z'
             )",
            rusqlite::params![workspace.workspace_id, generation_id, alpha],
        )
        .unwrap();
    drop(connection);

    (database_directory, workspace_directory, database_path)
}

fn run_cli_context_ir(
    workspace_root: &std::path::Path,
    catalogue: &std::path::Path,
    task: &str,
    symbols: &[&str],
    max_records: Option<i64>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atlas"));
    command
        .arg("context-ir")
        .arg(workspace_root)
        .arg(task)
        .arg("--catalogue")
        .arg(catalogue);
    for symbol in symbols {
        command.arg("--symbol").arg(symbol);
    }
    if let Some(max_records) = max_records {
        command.arg(format!("--max-records={max_records}"));
    }
    command.output().unwrap()
}

fn run_mcp_context_ir(
    workspace_root: &std::path::Path,
    catalogue: &std::path::Path,
    task: &str,
    symbols: &[&str],
    max_records: Option<i64>,
) -> Value {
    let mut arguments = json!({
        "workspace_root": workspace_root,
        "catalogue": catalogue,
        "task": task,
        "symbol": symbols,
    });
    if let Some(max_records) = max_records {
        arguments["max_records"] = json!(max_records);
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "atlas_context_ir",
            "arguments": arguments,
        },
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_atlas-mcp"));
    let output = command
        .write_stdin(format!("{request}\n"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "atlas-mcp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_and_mcp_compile_identical_explainable_context() {
    let (_database_directory, workspace_directory, database_path) = fixture();
    let task = "improve capitalization metadata semantics";

    let cli_output = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        task,
        &["alpha"],
        None,
    );
    assert!(
        cli_output.status.success(),
        "atlas failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).unwrap();
    let mcp = run_mcp_context_ir(
        workspace_directory.path(),
        &database_path,
        task,
        &["alpha"],
        None,
    );
    let mcp_content = &mcp["result"]["structuredContent"];

    assert_eq!(cli, *mcp_content);
    assert_eq!(cli["task_kind"], "unknown");
    assert_eq!(cli["task_kind_source"], "deterministic_rule");
    assert_eq!(cli["kind_rule_id"], "rule_default_unknown");
    assert_eq!(
        cli["context_ir"]["policy"]["planner_policy_version"],
        "planner-v1.1.0"
    );
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let persisted: (String, Option<String>, String) = connection
        .query_row(
            "SELECT task_kind_source, task_kind_rule_id, planner_policy_version
             FROM task_session WHERE task_session_id = ?1",
            rusqlite::params![cli["task_session_id"].as_str().unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        persisted,
        (
            "deterministic_rule".to_string(),
            Some("rule_default_unknown".to_string()),
            "planner-v1.1.0".to_string(),
        )
    );
}

#[test]
fn equivalent_seed_order_and_duplicates_are_transport_stable() {
    let (_database_directory, workspace_directory, database_path) = fixture();
    let task = "explore the public symbols";

    let first_output = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        task,
        &["beta", "alpha"],
        None,
    );
    let second_output = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        task,
        &["alpha", "beta", "alpha"],
        None,
    );
    assert!(first_output.status.success());
    assert!(second_output.status.success());
    let first: Value = serde_json::from_slice(&first_output.stdout).unwrap();
    let second: Value = serde_json::from_slice(&second_output.stdout).unwrap();

    assert_eq!(
        first["context_ir"]["context_id"],
        second["context_ir"]["context_id"]
    );
    assert_eq!(
        first["context_ir"]["context_hash"],
        second["context_ir"]["context_hash"]
    );
    assert_eq!(
        first["context_ir"]["task"]["seed_symbols"],
        json!(["alpha", "beta"])
    );
}

#[test]
fn different_task_intents_receive_different_required_evidence() {
    let (_database_directory, workspace_directory, database_path) = fixture();
    let bug_fix_output = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        "fix alpha behavior",
        &["alpha"],
        None,
    );
    let explore_output = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        "explore alpha behavior",
        &["alpha"],
        None,
    );
    assert!(bug_fix_output.status.success());
    assert!(explore_output.status.success());
    let bug_fix: Value = serde_json::from_slice(&bug_fix_output.stdout).unwrap();
    let explore: Value = serde_json::from_slice(&explore_output.stdout).unwrap();

    assert!(bug_fix["context_ir"]["working_set"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| {
            item["role"] == "TEST_CONTRACT" && item["selection_reason"] == "TEST_RELATION"
        }));
    assert!(!explore["context_ir"]["working_set"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["role"] == "TEST_CONTRACT"));
}

#[test]
fn negative_budgets_fail_closed_on_cli_and_mcp() {
    let (_database_directory, workspace_directory, database_path) = fixture();
    let cli = run_cli_context_ir(
        workspace_directory.path(),
        &database_path,
        "explore alpha",
        &["alpha"],
        Some(-1),
    );
    assert!(!cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stderr).contains("max_records must be > 0"));

    let mcp = run_mcp_context_ir(
        workspace_directory.path(),
        &database_path,
        "explore alpha",
        &["alpha"],
        Some(-1),
    );
    assert_eq!(mcp["error"]["code"], -32602);
    assert_eq!(mcp["error"]["data"]["kind"], "budget_invalid");
}
