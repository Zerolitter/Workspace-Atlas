use std::cell::Cell;

use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    RawTaskRetention, TaskKind, TaskKindSource, WorkingSetStatus, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::query_metrics::MetricClock;
use workspace_atlas::serving::build_serving_generation;
use workspace_atlas::task_compiler::{
    compile_context_ir, compile_context_ir_with_clock, CompileRequest,
};
use workspace_atlas::task_session::create_task_session;
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

struct StepClock {
    now: Cell<u64>,
    step: u64,
}

impl StepClock {
    fn new(step: u64) -> Self {
        Self {
            now: Cell::new(0),
            step,
        }
    }
}

impl MetricClock for StepClock {
    fn now_micros(&self) -> u64 {
        let current = self.now.get();
        self.now.set(current + self.step);
        current
    }
}

fn fixture() -> (
    tempfile::TempDir,
    rusqlite::Connection,
    WorkspaceRecord,
    String,
) {
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/a.ts"),
        "export function alpha() { return 1; }\nexport const gamma = 2;\n",
    )
    .unwrap();
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"metrics\"\n")
            .unwrap();
    let connection =
        init_catalogue(&workspace_directory.path().join("atlas.sqlite"), &config).unwrap();
    let workspace = register_workspace(
        &connection,
        workspace_directory.path(),
        &config,
        &workspace_directory.path().join("atlas.sqlite"),
        "1.0.0",
    )
    .unwrap();
    let report = discovery::reconcile(&workspace, &connection, &config).unwrap();
    (
        workspace_directory,
        connection,
        workspace,
        report.candidate_generation_id,
    )
}

fn session(
    connection: &rusqlite::Connection,
    workspace: &WorkspaceRecord,
    generation_id: &str,
    salt: u8,
) -> workspace_atlas::context_ir::TaskSession {
    create_task_session(
        connection,
        workspace,
        generation_id,
        &format!("{salt:064x}"),
        &format!("{:064x}", salt + 1),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap()
}
struct MetricCounts {
    serving_fallback: i64,
    candidates: i64,
    edges: i64,
    returned: i64,
    tokens: i64,
    cache_hits: i64,
    cache_misses: i64,
    truncated: i64,
}

#[test]
fn compiler_persists_consistent_numeric_metrics_for_fallback_partial_and_ready_results() {
    let (_directory, connection, workspace, generation_id) = fixture();
    let request = CompileRequest {
        known_paths: vec!["src/a.ts".into()],
        max_records: 1,
        ..CompileRequest::default()
    };

    let fallback_session = session(&connection, &workspace, &generation_id, 1);
    let fallback = compile_context_ir(
        &connection,
        &workspace,
        &fallback_session.task_session_id,
        &fallback_session.task_hash,
        &fallback_session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &request,
    )
    .unwrap();
    assert!(fallback.status.serving_fallback);

    build_serving_generation(&connection, &workspace).unwrap();
    let ready_session = session(&connection, &workspace, &generation_id, 3);
    let ready = compile_context_ir(
        &connection,
        &workspace,
        &ready_session.task_session_id,
        &ready_session.task_hash,
        &ready_session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &request,
    )
    .unwrap();
    assert!(!ready.status.serving_fallback);
    assert!(ready.omissions.truncated);
    let complete_session = session(&connection, &workspace, &generation_id, 11);
    let complete_request = CompileRequest {
        known_paths: vec!["src/a.ts".into()],
        ..CompileRequest::default()
    };
    let complete = compile_context_ir(
        &connection,
        &workspace,
        &complete_session.task_session_id,
        &complete_session.task_hash,
        &complete_session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &complete_request,
    )
    .unwrap();
    assert_eq!(
        complete.status.working_set_status,
        WorkingSetStatus::Complete
    );

    let rows: Vec<MetricCounts> = {
        let mut statement = connection
            .prepare(
                "SELECT serving_fallback, candidate_count, expanded_edge_count, returned_records,
                        returned_estimated_tokens, cache_hits, cache_misses, truncated
                 FROM query_stage_metric ORDER BY created_at, metric_id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(MetricCounts {
                    serving_fallback: row.get(0)?,
                    candidates: row.get(1)?,
                    edges: row.get(2)?,
                    returned: row.get(3)?,
                    tokens: row.get(4)?,
                    cache_hits: row.get(5)?,
                    cache_misses: row.get(6)?,
                    truncated: row.get(7)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().filter(|row| row.serving_fallback == 1).count(),
        1
    );
    assert_eq!(rows.iter().filter(|row| row.cache_hits == 1).count(), 2);
    assert_eq!(rows.iter().filter(|row| row.cache_misses == 1).count(), 1);
    assert!(rows.iter().any(|row| row.truncated == 1));
    assert!(rows.iter().any(|row| row.truncated == 0));
    for row in rows {
        assert!(
            row.candidates >= row.returned
                && row.edges >= 0
                && row.returned >= 0
                && row.tokens >= 0
        );
        assert!(matches!(row.truncated, 0 | 1));
    }

    let non_numeric: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM query_stage_metric
             WHERE typeof(seed_resolution_us) != 'integer'
                OR typeof(serving_lookup_us) != 'integer'
                OR typeof(graph_expansion_us) != 'integer'
                OR typeof(ranking_us) != 'integer'
                OR typeof(source_verify_us) != 'integer'
                OR typeof(source_read_us) != 'integer'
                OR typeof(packing_us) != 'integer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(non_numeric, 0);
}

#[test]
fn controlled_clock_attributes_stages_and_records_typed_interruption() {
    let (_directory, connection, workspace, generation_id) = fixture();
    let timed_session = session(&connection, &workspace, &generation_id, 5);
    let request = CompileRequest {
        known_paths: vec!["src/a.ts".into()],
        ..CompileRequest::default()
    };
    compile_context_ir_with_clock(
        &connection,
        &workspace,
        &timed_session.task_session_id,
        &timed_session.task_hash,
        &timed_session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &request,
        &StepClock::new(100),
    )
    .unwrap();
    let (serving, seed, graph, ranking, packing): (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT serving_lookup_us, seed_resolution_us, graph_expansion_us, ranking_us, packing_us
             FROM query_stage_metric WHERE task_session_id = ?1",
            [&timed_session.task_session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert!(serving > 0 && seed > 0 && graph > 0 && ranking > 0 && packing > 0);

    let interrupted_session = session(&connection, &workspace, &generation_id, 7);
    let interrupted_request = CompileRequest {
        hard_latency_ms: 1,
        soft_latency_ms: 1,
        known_paths: vec!["src/a.ts".into()],
        ..CompileRequest::default()
    };
    let error = compile_context_ir_with_clock(
        &connection,
        &workspace,
        &interrupted_session.task_session_id,
        &interrupted_session.task_hash,
        &interrupted_session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &interrupted_request,
        &StepClock::new(2_000),
    )
    .unwrap_err();
    assert!(error.to_string().contains("context_ir_interrupted"));
    let operation: String = connection
        .query_row(
            "SELECT operation FROM query_stage_metric WHERE task_session_id = ?1",
            [&interrupted_session.task_session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(operation, "context_ir_compile_interrupted");
}

#[test]
fn serving_build_reports_nonnegative_rows_bytes_and_elapsed_time() {
    let (_directory, connection, workspace, _generation_id) = fixture();
    let report = build_serving_generation(&connection, &workspace).unwrap();

    assert!(report.build_elapsed_us >= 0);
    assert_eq!(
        report.output_rows,
        report.card_count + report.edge_count + report.coverage_rollup_count
    );
    assert!(report.output_bytes >= 0);
}
