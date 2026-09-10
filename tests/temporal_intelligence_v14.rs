use assert_cmd::Command;
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::discovery;
use workspace_atlas::workspace::{load_workspace, register_workspace};

struct Fixture {
    _database_directory: tempfile::TempDir,
    _workspace_directory: tempfile::TempDir,
    workspace_root: std::path::PathBuf,
    database_path: std::path::PathBuf,
    config: Config,
    connection: rusqlite::Connection,
    workspace_id: String,
}
impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let database_directory = tempfile::tempdir().unwrap();
        let _workspace_directory = tempfile::tempdir().unwrap();
        // Canonicalize the workspace root before any identity-bearing call.
        // On macOS `tempfile::tempdir()` returns a symlink-backed lexical
        // path whose canonical spelling differs; on Windows a short-name
        // alias can resolve to a different spelling. CLI `resolve_catalogue`
        // canonicalizes the supplied root and rejects a stored root that
        // does not match via `paths_have_same_identity`, so the fixture
        // must register and re-supply the same canonical spelling
        // production would persist; otherwise status/supersession/CLI
        // tests fail closed on the canonical root mismatch instead of
        // exercising the temporal path.
        let workspace_root = std::fs::canonicalize(_workspace_directory.path()).unwrap();
        for (path, source) in files {
            let absolute = workspace_root.join(path);
            std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            std::fs::write(absolute, source).unwrap();
        }
        let config =
            Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"v1.4-e2e\"\n")
                .unwrap();
        let database_path = database_directory.path().join("atlas.sqlite");
        let connection = init_catalogue(&database_path, &config).unwrap();
        let workspace = register_workspace(
            &connection,
            &workspace_root,
            &config,
            &database_path,
            "1.0.0",
        )
        .unwrap();

        Self {
            _database_directory: database_directory,
            _workspace_directory,
            workspace_root,
            database_path,
            config,
            connection,
            workspace_id: workspace.workspace_id,
        }
    }

    fn reconcile(&self) -> String {
        let workspace = load_workspace(&self.connection, &self.workspace_id)
            .unwrap()
            .unwrap();
        discovery::reconcile(&workspace, &self.connection, &self.config)
            .unwrap()
            .candidate_generation_id
    }

    fn insert_controlled_complete_workspace_coverage(
        &self,
        generation_id: &str,
        coverage_id: &str,
    ) {
        self.connection
            .execute(
                "INSERT INTO coverage_record (
                    coverage_id, generation_id, scope_kind, scope_key, status,
                    capabilities_json, limitations_json, details_json
                 ) VALUES (?1,?2,'workspace','all','complete','[\"symbols\"]','[]','{}')",
                rusqlite::params![coverage_id, generation_id],
            )
            .unwrap();
    }
}

fn run_cli_temporal(
    fixture: &Fixture,
    from: Option<&str>,
    max_records: Option<i64>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atlas"));
    command
        .arg("temporal")
        .arg(&fixture.workspace_root)
        .arg("--catalogue")
        .arg(&fixture.database_path);
    if let Some(from) = from {
        command.arg("--from").arg(from);
    }
    if let Some(max_records) = max_records {
        command.arg(format!("--max-records={max_records}"));
    }
    command.output().unwrap()
}

fn run_mcp_temporal(fixture: &Fixture, from: Option<&str>, max_records: Option<i64>) -> Value {
    let mut arguments = json!({
        "workspace_root": fixture.workspace_root.to_string_lossy().into_owned(),
        "catalogue": fixture.database_path,
    });
    if let Some(from) = from {
        arguments["from"] = json!(from);
    }
    if let Some(max_records) = max_records {
        arguments["max_records"] = json!(max_records);
    }
    run_mcp_request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "atlas_temporal",
            "arguments": arguments,
        },
    }))
}

fn run_mcp_request(request: Value) -> Value {
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
fn temporal_report_is_deterministic_bounded_and_transport_stable() {
    let fixture = Fixture::new(&[
        ("src/a.ts", "export function alpha() { return 1; }\n"),
        ("src/b.ts", "export function beta() { return 2; }\n"),
        ("src/c.ts", "export function gamma() { return 3; }\n"),
    ]);
    let first_generation = fixture.reconcile();
    fixture.insert_controlled_complete_workspace_coverage(
        &first_generation,
        "controlled-temporal-before",
    );
    std::fs::rename(
        fixture.workspace_root.join("src/a.ts"),
        fixture.workspace_root.join("src/renamed.ts"),
    )
    .unwrap();
    std::fs::write(
        fixture.workspace_root.join("src/b.ts"),
        "export function beta() { return 20; }\n",
    )
    .unwrap();
    let second_generation = fixture.reconcile();
    fixture.insert_controlled_complete_workspace_coverage(
        &second_generation,
        "controlled-temporal-after",
    );

    let first_output = run_cli_temporal(&fixture, Some(&first_generation), Some(50));
    assert!(
        first_output.status.success(),
        "atlas failed: {}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    let first: Value = serde_json::from_slice(&first_output.stdout).unwrap();
    let second_output = run_cli_temporal(&fixture, Some(&first_generation), Some(50));
    assert!(second_output.status.success());
    let second: Value = serde_json::from_slice(&second_output.stdout).unwrap();
    let mcp = run_mcp_temporal(&fixture, Some(&first_generation), Some(50));

    assert_eq!(first, second);
    assert_eq!(first, mcp["result"]["structuredContent"]);
    assert_eq!(first["schema_version"], "1.0.0");
    assert_eq!(first["temporal_policy_version"], "temporal-v1.0.0");
    assert_eq!(first["history_state"], "available");
    assert_eq!(first["from_generation_id"], first_generation);
    assert_eq!(first["to_generation_id"], second_generation);
    assert_eq!(first["provenance"]["derivation"], "local_generation_delta");
    assert_eq!(first["provenance"]["delta_policy_version"], "delta-v1.0.0");
    assert!(first["provenance"]["history_event_count"].as_i64().unwrap() >= 1);
    assert!(first["changes"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| change["change_kind"] == "renamed"));
    assert!(first["changes"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .all(|change| {
            change["evidence_state"].is_string()
                && change["reason_codes"]
                    .as_array()
                    .is_some_and(|codes| !codes.is_empty())
        }));
    assert!(first["validity"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|claim| claim["status"] == "verified_current"));
    assert_eq!(first["report_hash"], second["report_hash"]);

    let bounded_output = run_cli_temporal(&fixture, Some(&first_generation), Some(1));
    assert!(bounded_output.status.success());
    let bounded: Value = serde_json::from_slice(&bounded_output.stdout).unwrap();
    assert!(bounded["changes"]["records"].as_array().unwrap().len() <= 1);
    assert!(bounded["validity"]["records"].as_array().unwrap().len() <= 1);
    assert_eq!(bounded["max_records_per_section"], 1);
    assert_eq!(bounded["changes"]["truncated"], true);
}

#[test]
fn temporal_report_marks_unreconciled_live_drift_as_stale_high_risk() {
    let fixture = Fixture::new(&[
        ("src/a.ts", "export function alpha() { return 1; }\n"),
        ("src/b.ts", "export function beta() { return 2; }\n"),
    ]);
    let first_generation = fixture.reconcile();
    std::fs::write(
        fixture.workspace_root.join("src/a.ts"),
        "export function alpha() { return 10; }\n",
    )
    .unwrap();
    fixture.reconcile();
    std::fs::write(
        fixture.workspace_root.join("src/a.ts"),
        "export function alpha() { return 99; }\n",
    )
    .unwrap();
    std::fs::write(
        fixture.workspace_root.join("src/b.ts"),
        "export function beta() { return 99; }\n",
    )
    .unwrap();

    let output = run_cli_temporal(&fixture, Some(&first_generation), Some(50));
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert!(!report["validity"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|claim| claim["canonical_path"] == "src/b.ts"));
    assert!(report["risk"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal["code"] == "delta_uncertainty"));
    assert!(report["changes"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| {
            change["entity_id"] == "src/a.ts" && change["current_source_status"] == "stale"
        }));
    assert_eq!(report["risk"]["level"], "high");
    assert!(report["risk"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal["code"] == "live_source_stale"));

    let repeated = run_cli_temporal(&fixture, Some(&first_generation), Some(50));
    assert!(repeated.status.success());
    let repeated_report: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(report, repeated_report);
}

#[test]
fn temporal_report_is_honest_when_no_active_or_prior_generation_exists() {
    let fixture = Fixture::new(&[("src/a.ts", "export function alpha() { return 1; }\n")]);

    let empty_output = run_cli_temporal(&fixture, None, None);
    assert!(empty_output.status.success());
    let empty_report: Value = serde_json::from_slice(&empty_output.stdout).unwrap();
    assert_eq!(empty_report["history_state"], "no_active_generation");
    assert_eq!(empty_report["to_generation_id"], Value::Null);
    assert_eq!(empty_report["risk"]["level"], "unknown");
    assert!(empty_report["risk"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal["code"] == "active_generation_unavailable"));

    let active_generation = fixture.reconcile();

    let output = run_cli_temporal(&fixture, None, None);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(report["history_state"], "baseline_only");
    assert_eq!(report["from_generation_id"], Value::Null);
    assert_eq!(report["to_generation_id"], active_generation);
    assert_eq!(report["changes"]["total_matched"], 0);
    assert_eq!(report["validity"]["total_matched"], 0);
    assert_eq!(report["risk"]["level"], "unknown");
    assert!(report["risk"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|signal| signal["code"] == "prior_generation_unavailable"));
    assert_eq!(report["provenance"]["delta_id"], Value::Null);
}

#[test]
fn temporal_inputs_fail_closed_and_existing_mcp_tools_remain_available() {
    let fixture = Fixture::new(&[("src/a.ts", "export function alpha() { return 1; }\n")]);
    let active_generation = fixture.reconcile();

    let negative = run_cli_temporal(&fixture, None, Some(-1));
    assert!(!negative.status.success());
    assert!(String::from_utf8_lossy(&negative.stderr).contains("max_records must be > 0"));

    let missing_generation = run_cli_temporal(&fixture, Some("gen_missing"), Some(10));
    assert!(!missing_generation.status.success());
    assert!(String::from_utf8_lossy(&missing_generation.stderr).contains("not found"));

    let candidate = workspace_atlas::generation::begin_candidate(
        &fixture.connection,
        &fixture.workspace_id,
        workspace_atlas::generation::TriggerKind::Manual,
        &"a".repeat(64),
        "1.0.0",
    )
    .unwrap();
    let workspace = load_workspace(&fixture.connection, &fixture.workspace_id)
        .unwrap()
        .unwrap();
    let delta_error = workspace_atlas::generation_delta::compute_generation_delta(
        &fixture.connection,
        &workspace,
        &candidate.generation_id,
        &active_generation,
    )
    .unwrap_err();
    assert!(delta_error.to_string().contains("not committed"));

    let non_committed = run_cli_temporal(&fixture, Some(&candidate.generation_id), Some(10));
    assert!(!non_committed.status.success());
    assert!(String::from_utf8_lossy(&non_committed.stderr).contains("not committed"));

    let mcp_negative = run_mcp_temporal(&fixture, None, Some(-1));
    assert_eq!(mcp_negative["error"]["code"], -32602);
    assert_eq!(mcp_negative["error"]["data"]["kind"], "budget_invalid");

    let tools = run_mcp_request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "atlas_status",
        "atlas_history",
        "atlas_generation_delta",
        "atlas_context_ir",
        "atlas_temporal",
    ] {
        assert!(names.contains(&expected), "missing MCP tool {expected}");
    }
}
