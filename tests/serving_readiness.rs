use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::cli::build_serving_status_output;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    ContextIr, RawTaskRetention, TaskKind, TaskKindSource, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::query_metrics::MetricClock;
use workspace_atlas::resolution::ResolutionStatus;
use workspace_atlas::serving::{
    build_serving_generation, build_serving_generation_with_policy, delete_serving_generation,
    serving_status, serving_status_with_policy, ServingReadinessState, PROJECTION_POLICY_VERSION,
    SERVING_SCHEMA_VERSION,
};
use workspace_atlas::task_compiler::{
    compile_context_ir, compile_context_ir_with_clock, CompileRequest,
};
use workspace_atlas::task_session::create_task_session;
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

fn fixture() -> (
    tempfile::TempDir,
    rusqlite::Connection,
    WorkspaceRecord,
    Config,
    String,
) {
    fixture_with_unrelated_files(0)
}

fn fixture_with_unrelated_files(
    unrelated_files: usize,
) -> (
    tempfile::TempDir,
    rusqlite::Connection,
    WorkspaceRecord,
    Config,
    String,
) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src/a.ts"),
        "import { beta } from './b';\nexport function alpha() { return beta(); }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/b.ts"),
        "export function beta() { return 2; }\n",
    )
    .unwrap();
    for index in 0..unrelated_files {
        std::fs::write(
            directory
                .path()
                .join("src")
                .join(format!("unrelated-{index:04}.ts")),
            format!("export function unrelated{index:04}() {{ return {index}; }}\n"),
        )
        .unwrap();
    }
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"serving readiness\"\n",
    )
    .unwrap();
    let catalogue = directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    let workspace =
        register_workspace(&connection, directory.path(), &config, &catalogue, "1.0.0").unwrap();
    let generation = discovery::reconcile(&workspace, &connection, &config)
        .unwrap()
        .candidate_generation_id;
    (directory, connection, workspace, config, generation)
}

fn compile(
    connection: &rusqlite::Connection,
    workspace: &WorkspaceRecord,
    generation_id: &str,
    task_kind: TaskKind,
    request: &CompileRequest,
    salt: u8,
) -> ContextIr {
    let session = create_task_session(
        connection,
        workspace,
        generation_id,
        &format!("{salt:064x}"),
        &format!("{:064x}", salt + 1),
        RawTaskRetention::None,
        None,
        task_kind,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();
    compile_context_ir(
        connection,
        workspace,
        &session.task_session_id,
        &session.task_hash,
        &session.normalized_goal_hash,
        task_kind,
        TaskKindSource::Declared,
        None,
        request,
    )
    .unwrap()
}

fn semantic_projection(ir: &ContextIr) -> serde_json::Value {
    serde_json::json!({
        "context_id": ir.context_id,
        "workspace_id": ir.workspace.workspace_id,
        "generation_id": ir.workspace.generation_id,
        "generation_sequence": ir.workspace.generation_sequence,
        "configuration_hash": ir.workspace.configuration_hash,
        "provider_set_hash": ir.workspace.provider_set_hash,
        "task": ir.task,
        "policy": ir.policy,
        "working_set": ir.working_set,
        "relationships": ir.relationships,
        "effects": ir.effects,
        "uncertainty": ir.uncertainty,
        "coverage": ir.coverage,
        "omissions": ir.omissions,
        "cost": ir.cost,
    })
}

fn symbol_identity(
    connection: &rusqlite::Connection,
    generation_id: &str,
    path: &str,
) -> (String, String, String) {
    connection
        .query_row(
            "SELECT sf.canonical_symbol_key, sf.extractor_run_id, sf.revision_id
             FROM current_file cf
             JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = ?2
             ORDER BY sf.canonical_symbol_key
             LIMIT 1",
            rusqlite::params![generation_id, path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn insert_consumed_relationship(
    connection: &rusqlite::Connection,
    workspace: &WorkspaceRecord,
    generation_id: &str,
) -> String {
    let (alpha, extractor_run_id, revision_id) =
        symbol_identity(connection, generation_id, "src/a.ts");
    let (beta, _, _) = symbol_identity(connection, generation_id, "src/b.ts");
    connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                start_byte, end_byte, attributes_json, evidence_method, confidence,
                evidence_reason
             ) VALUES (
                'rp4-consumed-edge', ?1, ?2, 'calls', 'symbol', ?3, 'symbol', ?4,
                NULL, NULL, '{}', 'semantic', 0.99, 'RP4 test fixture'
             )",
            rusqlite::params![extractor_run_id, revision_id, alpha, beta],
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
                'rp4-consumed-resolution', ?1, ?2, 'rp4-consumed-edge', NULL,
                'rp4-test', 'resolved_symbol', 'symbol', ?3, 'rp4_test',
                '[]', 0.99, '[]', ?4
             )",
            rusqlite::params![
                workspace.workspace_id,
                generation_id,
                beta,
                workspace_atlas::migrations::iso8601_now(),
            ],
        )
        .unwrap();
    alpha
}

fn recompile(
    connection: &rusqlite::Connection,
    workspace: &WorkspaceRecord,
    original: &ContextIr,
    task_kind: TaskKind,
    request: &CompileRequest,
) -> ContextIr {
    compile_context_ir(
        connection,
        workspace,
        &original.task.task_session_id,
        &original.task.task_hash,
        &original.task.normalized_goal_hash,
        task_kind,
        TaskKindSource::Declared,
        None,
        request,
    )
    .unwrap()
}

unsafe extern "C" fn collect_sqlite_statement_steps(
    trace_code: std::ffi::c_uint,
    context: *mut std::ffi::c_void,
    statement: *mut std::ffi::c_void,
    _elapsed: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if trace_code == rusqlite::ffi::SQLITE_TRACE_PROFILE as std::ffi::c_uint {
        // SAFETY: `SqliteTraceGuard` keeps the borrowed Vec at this stable
        // address and unregisters the synchronous callback before releasing it.
        let statements = unsafe { &mut *context.cast::<Vec<(String, u64)>>() };
        let statement = statement.cast::<rusqlite::ffi::sqlite3_stmt>();
        // SAFETY: SQLite supplies a live statement pointer for PROFILE events.
        let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement) };
        if !sql.is_null() {
            // SAFETY: sqlite3_sql returns a NUL-terminated string owned by the
            // live statement for the duration of this callback.
            let sql = unsafe { std::ffi::CStr::from_ptr(sql) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: the PROFILE callback's statement remains live here.
            let steps = unsafe {
                rusqlite::ffi::sqlite3_stmt_status(
                    statement,
                    rusqlite::ffi::SQLITE_STMTSTATUS_VM_STEP,
                    0,
                )
            };
            statements.push((sql, u64::try_from(steps).unwrap_or(0)));
        }
    }
    0
}

struct SqliteTraceGuard<'a> {
    connection: &'a rusqlite::Connection,
    _context: std::marker::PhantomData<&'a mut Vec<(String, u64)>>,
}

impl<'a> SqliteTraceGuard<'a> {
    fn new(connection: &'a rusqlite::Connection, statements: &'a mut Vec<(String, u64)>) -> Self {
        // SAFETY: the callback is synchronous on this non-shared Connection;
        // `_context` prevents the Vec from moving or dropping before `Drop`
        // unregisters the callback.
        let result = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                connection.handle(),
                rusqlite::ffi::SQLITE_TRACE_PROFILE as std::ffi::c_uint,
                Some(collect_sqlite_statement_steps),
                (statements as *mut Vec<(String, u64)>).cast(),
            )
        };
        assert_eq!(result, rusqlite::ffi::SQLITE_OK);
        Self {
            connection,
            _context: std::marker::PhantomData,
        }
    }
}

impl Drop for SqliteTraceGuard<'_> {
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

fn measured_recompile(
    connection: &rusqlite::Connection,
    workspace: &WorkspaceRecord,
    original: &ContextIr,
    task_kind: TaskKind,
    request: &CompileRequest,
) -> (ContextIr, u64) {
    let mut statements = Vec::new();
    let trace = SqliteTraceGuard::new(connection, &mut statements);
    let ir = recompile(connection, workspace, original, task_kind, request);
    drop(trace);
    let steps = statements.iter().map(|(_, steps)| steps).sum();
    (ir, steps)
}

struct SequenceClock {
    values: Vec<u64>,
    next: std::cell::Cell<usize>,
}

impl SequenceClock {
    fn new(values: Vec<u64>) -> Self {
        Self {
            values,
            next: std::cell::Cell::new(0),
        }
    }
}

impl MetricClock for SequenceClock {
    fn now_micros(&self) -> u64 {
        let index = self.next.get();
        self.next.set(index.saturating_add(1));
        self.values
            .get(index)
            .copied()
            .or_else(|| self.values.last().copied())
            .unwrap_or(0)
    }
}

fn assert_consumed_projection_mutation_falls_back(
    mutate: impl FnOnce(&rusqlite::Connection, &str),
) {
    let (_directory, connection, workspace, _config, generation) = fixture();
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    let request = CompileRequest {
        known_symbols: vec![alpha],
        ..CompileRequest::default()
    };
    let fallback = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &request,
        41,
    );
    assert!(fallback.status.serving_fallback);
    let build = build_serving_generation(&connection, &workspace).unwrap();
    let ready = recompile(
        &connection,
        &workspace,
        &fallback,
        TaskKind::Explore,
        &request,
    );
    assert!(!ready.status.serving_fallback);
    assert_eq!(semantic_projection(&fallback), semantic_projection(&ready));

    mutate(&connection, &build.serving_generation_id);
    let repaired = recompile(
        &connection,
        &workspace,
        &fallback,
        TaskKind::Explore,
        &request,
    );
    assert_eq!(
        serde_json::to_value(&fallback).unwrap(),
        serde_json::to_value(&repaired).unwrap(),
        "a consumed projection mismatch must discard request-local Serving state"
    );
}

#[test]
fn status_tracks_absent_ready_failed_policy_drift_and_generation_supersession() {
    let (directory, connection, workspace, config, first_generation) = fixture();

    let absent = serving_status(&connection, &workspace).unwrap();
    assert_eq!(absent.state, ServingReadinessState::Absent);
    assert_eq!(
        absent.active_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(absent.serving_schema_version, SERVING_SCHEMA_VERSION);
    assert_eq!(absent.projection_policy_version, PROJECTION_POLICY_VERSION);
    assert!(absent.rebuild_recommended);

    let first_build = build_serving_generation(&connection, &workspace).unwrap();
    let ready = serving_status(&connection, &workspace).unwrap();
    assert_eq!(ready.state, ServingReadinessState::Ready);
    assert_eq!(
        ready.current.as_ref().unwrap().serving_generation_id,
        first_build.serving_generation_id
    );
    assert!(!ready.rebuild_recommended);

    let policy_drift = serving_status_with_policy(
        &connection,
        &workspace,
        SERVING_SCHEMA_VERSION,
        "projection-v1.0.1-test",
    )
    .unwrap();
    assert_eq!(policy_drift.state, ServingReadinessState::Absent);
    assert_eq!(policy_drift.superseded.len(), 1);
    assert_eq!(
        policy_drift.superseded[0].state,
        ServingReadinessState::Superseded
    );

    std::fs::write(
        directory.path().join("src/b.ts"),
        "export function beta() { return 3; }\n",
    )
    .unwrap();
    let second_generation = discovery::reconcile(&workspace, &connection, &config)
        .unwrap()
        .candidate_generation_id;
    assert_ne!(first_generation, second_generation);
    let stale = build_serving_status_output(
        directory.path(),
        Some(&directory.path().join("atlas.sqlite")),
    )
    .unwrap();
    assert_eq!(stale.state, ServingReadinessState::Absent);
    assert_eq!(
        stale.active_generation_id.as_deref(),
        Some(second_generation.as_str())
    );
    assert_eq!(stale.superseded.len(), 1);

    let expected_id = stale.expected_serving_generation_id.clone().unwrap();
    connection
        .execute(
            "INSERT INTO serving_generation (
                serving_generation_id, workspace_id, generation_id, serving_schema_version,
                projection_policy_version, state, source_fact_fingerprint, card_count, edge_count,
                coverage_rollup_count, build_started_at, build_finished_at, failure_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'failed', 'failed-input', 0, 0, 0, ?6, ?6, 'injected')",
            rusqlite::params![
                expected_id,
                workspace.workspace_id,
                second_generation,
                SERVING_SCHEMA_VERSION,
                PROJECTION_POLICY_VERSION,
                workspace_atlas::migrations::iso8601_now(),
            ],
        )
        .unwrap();
    let failed = serving_status(&connection, &workspace).unwrap();
    assert_eq!(failed.state, ServingReadinessState::Failed);
    assert_eq!(
        failed.current.as_ref().unwrap().failure_code.as_deref(),
        Some("injected")
    );
    assert!(failed.rebuild_recommended);

    let retried = build_serving_generation(&connection, &workspace).unwrap();
    assert_eq!(retried.state, "ready");
    assert_eq!(
        serving_status(&connection, &workspace).unwrap().state,
        ServingReadinessState::Ready
    );

    connection
        .execute(
            "DELETE FROM symbol_serving_projection
             WHERE rowid IN (
                 SELECT rowid FROM symbol_serving_projection
                 WHERE serving_generation_id = ?1 LIMIT 1
             )",
            rusqlite::params![retried.serving_generation_id],
        )
        .unwrap();
    let corrupt = serving_status(&connection, &workspace).unwrap();
    assert_eq!(corrupt.state, ServingReadinessState::Failed);
    assert_eq!(
        corrupt.current.as_ref().unwrap().failure_code.as_deref(),
        Some("projection_count_mismatch")
    );
    let error = build_serving_generation(&connection, &workspace).unwrap_err();
    assert!(error
        .to_string()
        .contains("immutable ready projection is inconsistent"));
}

#[test]
fn rebuild_reports_deterministic_input_stage_and_output_metrics() {
    let (_directory, connection, workspace, _config, generation) = fixture();

    let first = build_serving_generation(&connection, &workspace).unwrap();
    let second = build_serving_generation(&connection, &workspace).unwrap();

    assert_eq!(first.generation_id, generation);
    assert_eq!(first.serving_schema_version, SERVING_SCHEMA_VERSION);
    assert_eq!(first.projection_policy_version, PROJECTION_POLICY_VERSION);
    assert!(first.input.symbol_fact_count > 0);
    assert!(first.input.relationship_fact_count > 0);
    assert_eq!(first.input.source_fact_fingerprint.len(), 64);
    assert_eq!(first.input.canonical_input_fingerprint.len(), 64);
    assert_eq!(
        first.output_rows,
        first.card_count + first.edge_count + first.coverage_rollup_count
    );
    assert_eq!(first.output_hash.len(), 64);
    assert_eq!(first.input, second.input);
    assert_eq!(first.output_hash, second.output_hash);
    assert_eq!(first.output_rows, second.output_rows);
    assert!(first.cache_miss);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert!(!second.cache_miss);
    assert_eq!(second.stages.symbol_cards_us, 0);
    assert_eq!(second.stages.relationship_edges_us, 0);
    assert_eq!(second.stages.coverage_rollups_us, 0);
    assert_eq!(first.output_bytes, second.output_bytes);
    assert!(first.stages.total_recorded_us() <= first.build_elapsed_us);

    let custom = build_serving_generation_with_policy(
        &connection,
        &workspace,
        SERVING_SCHEMA_VERSION,
        "projection-v1.0.1-test",
    )
    .unwrap();
    assert_ne!(first.serving_generation_id, custom.serving_generation_id);

    connection
        .execute(
            "UPDATE symbol_serving_projection SET compact_json = '{}'
             WHERE rowid IN (
                 SELECT rowid FROM symbol_serving_projection
                 WHERE serving_generation_id = ?1 LIMIT 1
             )",
            rusqlite::params![first.serving_generation_id],
        )
        .unwrap();
    let error = build_serving_generation(&connection, &workspace).unwrap_err();
    assert!(error
        .to_string()
        .contains("immutable ready projection is inconsistent"));
}

#[test]
fn ready_and_truth_fallback_match_semantics_across_task_kinds_and_budgets() {
    let (_directory, connection, workspace, _config, generation) = fixture();
    let cases = [
        (TaskKind::Explore, 20, 20_000, 8_000),
        (TaskKind::BugFix, 1, 1_000, 500),
        (TaskKind::ApiChange, 20, 20_000, 8_000),
        (TaskKind::Review, 2, 4_000, 1_000),
    ];

    for (index, (task_kind, max_records, max_source_bytes, max_estimated_tokens)) in
        cases.into_iter().enumerate()
    {
        let request = CompileRequest {
            known_paths: vec!["src/a.ts".into(), "src/b.ts".into()],
            max_records,
            max_source_bytes,
            max_estimated_tokens,
            ..CompileRequest::default()
        };
        let fallback = compile(
            &connection,
            &workspace,
            &generation,
            task_kind,
            &request,
            (index as u8) * 4 + 1,
        );
        assert!(fallback.status.serving_fallback);

        let build = build_serving_generation(&connection, &workspace).unwrap();
        let ready = compile_context_ir(
            &connection,
            &workspace,
            &fallback.task.task_session_id,
            &fallback.task.task_hash,
            &fallback.task.normalized_goal_hash,
            task_kind,
            TaskKindSource::Declared,
            None,
            &request,
        )
        .unwrap();
        assert!(!ready.status.serving_fallback);
        assert_eq!(semantic_projection(&fallback), semantic_projection(&ready));

        delete_serving_generation(&connection, &build.serving_generation_id).unwrap();
    }
}

#[test]
fn missing_consumed_card_retries_once_through_truth() {
    assert_consumed_projection_mutation_falls_back(|connection, serving_generation_id| {
        connection
            .execute(
                "DELETE FROM symbol_serving_projection
                 WHERE rowid IN (
                     SELECT rowid FROM symbol_serving_projection
                     WHERE serving_generation_id = ?1 AND canonical_path = 'src/a.ts'
                     LIMIT 1
                 )",
                [serving_generation_id],
            )
            .unwrap();
    });
}

#[test]
fn corrupt_consumed_card_retries_once_through_truth() {
    assert_consumed_projection_mutation_falls_back(|connection, serving_generation_id| {
        connection
            .execute(
                "UPDATE symbol_serving_projection
                 SET symbol_kind = CASE symbol_kind WHEN 'class' THEN 'function' ELSE 'class' END
                 WHERE rowid IN (
                     SELECT rowid FROM symbol_serving_projection
                     WHERE serving_generation_id = ?1 AND canonical_path = 'src/a.ts'
                     LIMIT 1
                 )",
                [serving_generation_id],
            )
            .unwrap();
    });
}

#[test]
fn missing_consumed_edge_retries_once_through_truth() {
    assert_consumed_projection_mutation_falls_back(|connection, serving_generation_id| {
        connection
            .execute(
                "DELETE FROM relationship_serving_edge
                 WHERE serving_generation_id = ?1
                   AND preferred_fact_id = 'rp4-consumed-edge'",
                [serving_generation_id],
            )
            .unwrap();
    });
}

#[test]
fn corrupt_consumed_edge_retries_once_through_truth() {
    assert_consumed_projection_mutation_falls_back(|connection, serving_generation_id| {
        connection
            .execute(
                "UPDATE relationship_serving_edge
                 SET relationship_type = 'references'
                 WHERE serving_generation_id = ?1
                   AND preferred_fact_id = 'rp4-consumed-edge'",
                [serving_generation_id],
            )
            .unwrap();
    });
}

#[test]
fn unused_coverage_projection_corruption_cannot_change_compile_output() {
    let (_directory, connection, workspace, _config, generation) = fixture();
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    let request = CompileRequest {
        known_symbols: vec![alpha],
        ..CompileRequest::default()
    };
    let build = build_serving_generation(&connection, &workspace).unwrap();
    let ready = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &request,
        51,
    );
    assert!(!ready.status.serving_fallback);

    connection
        .execute(
            "UPDATE serving_coverage_rollup SET details_json = 'corrupt'
             WHERE serving_generation_id = ?1",
            [&build.serving_generation_id],
        )
        .unwrap();
    let after_corruption = recompile(&connection, &workspace, &ready, TaskKind::Explore, &request);
    assert_eq!(
        serde_json::to_value(&ready).unwrap(),
        serde_json::to_value(&after_corruption).unwrap(),
        "legacy compiler does not consume coverage projections"
    );
}

#[test]
fn corruption_outside_request_frontier_cannot_change_output() {
    let (_directory, connection, workspace, _config, generation) = fixture_with_unrelated_files(1);
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    let request = CompileRequest {
        known_symbols: vec![alpha],
        ..CompileRequest::default()
    };
    let build = build_serving_generation(&connection, &workspace).unwrap();
    let ready = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &request,
        61,
    );
    connection
        .execute(
            "UPDATE symbol_serving_projection SET symbol_kind = 'class'
             WHERE serving_generation_id = ?1
               AND canonical_path = 'src/unrelated-0000.ts'",
            [&build.serving_generation_id],
        )
        .unwrap();
    let after_corruption = recompile(&connection, &workspace, &ready, TaskKind::Explore, &request);
    assert_eq!(
        serde_json::to_value(&ready).unwrap(),
        serde_json::to_value(&after_corruption).unwrap()
    );
    assert!(!after_corruption.status.serving_fallback);
}

#[test]
fn serving_validation_vm_work_is_stable_under_unrelated_growth() {
    let measure = |unrelated_files: usize, salt: u8| {
        let (_directory, connection, workspace, _config, generation) =
            fixture_with_unrelated_files(unrelated_files);
        let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
        let request = CompileRequest {
            known_symbols: vec![alpha],
            max_records: 4,
            ..CompileRequest::default()
        };
        build_serving_generation(&connection, &workspace).unwrap();
        let template = compile(
            &connection,
            &workspace,
            &generation,
            TaskKind::Explore,
            &request,
            salt,
        );
        let (compiled, vm_steps) = measured_recompile(
            &connection,
            &workspace,
            &template,
            TaskKind::Explore,
            &request,
        );
        assert!(!compiled.status.serving_fallback);
        assert_eq!(compiled.working_set.len(), 2);
        assert_eq!(compiled.relationships.len(), 1);
        vm_steps
    };

    let baseline_steps = measure(0, 71);
    let scaled_steps = measure(128, 81);
    eprintln!("bounded SQLite VM steps: baseline={baseline_steps}, scaled={scaled_steps}");
    assert!(
        scaled_steps <= baseline_steps.saturating_add(256),
        "bounded request-local SQL work grew with unrelated projection rows: \
         baseline={baseline_steps}, scaled={scaled_steps}"
    );
}

#[test]
fn irrelevant_fanout_prefix_reports_bounded_frontier_cutoff() {
    let (_directory, connection, workspace, _config, generation) = fixture_with_unrelated_files(3);
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    let (_, extractor_run_id, revision_id) = symbol_identity(&connection, &generation, "src/a.ts");
    for index in 0..3 {
        let path = format!("src/unrelated-{index:04}.ts");
        let (target, _, target_revision_id) = symbol_identity(&connection, &generation, &path);
        connection
            .execute(
                "UPDATE file_revision SET artifact_class = 'test', is_test = 1
                 WHERE revision_id = ?1",
                [&target_revision_id],
            )
            .unwrap();
        let fact_id = format!("aaa-irrelevant-{index}");
        let resolution_id = format!("aaa-irrelevant-resolution-{index}");
        connection
            .execute(
                "INSERT INTO relationship_fact (
                    relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                    source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                    start_byte, end_byte, attributes_json, evidence_method, confidence,
                    evidence_reason
                 ) VALUES (?1, ?2, ?3, 'references', 'symbol', ?4, 'symbol', ?5,
                           NULL, NULL, '{}', 'semantic', 0.99, 'RP4 fanout fixture')",
                rusqlite::params![fact_id, extractor_run_id, revision_id, alpha, target],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO relationship_resolution (
                    relationship_resolution_id, workspace_id, generation_id,
                    relationship_fact_id, resolver_execution_id, resolver_policy_version,
                    status, resolved_ref_kind, resolved_ref_value, reason_code,
                    candidate_refs_json, confidence, evidence_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, 'rp4-test', 'resolved_symbol',
                           'symbol', ?5, 'rp4_test', '[]', 0.99, '[]', ?6)",
                rusqlite::params![
                    resolution_id,
                    workspace.workspace_id,
                    generation,
                    fact_id,
                    target,
                    workspace_atlas::migrations::iso8601_now(),
                ],
            )
            .unwrap();
    }
    let request = CompileRequest {
        known_symbols: vec![alpha],
        max_records: 2,
        ..CompileRequest::default()
    };
    let fallback = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &request,
        91,
    );
    assert!(fallback.omissions.truncated);
    assert_eq!(
        fallback
            .omissions
            .by_reason
            .get("relationship_record_budget"),
        Some(&1)
    );
    assert!(fallback
        .uncertainty
        .iter()
        .any(|notice| notice.code == "relationship_frontier_truncated"));
    assert_eq!(fallback.cost.selected_records, 1);
    assert!(fallback.relationships.is_empty());

    build_serving_generation(&connection, &workspace).unwrap();
    let ready = recompile(
        &connection,
        &workspace,
        &fallback,
        TaskKind::Explore,
        &request,
    );
    assert_eq!(semantic_projection(&fallback), semantic_projection(&ready));
    assert!(!ready.status.serving_fallback);
    assert!(ready.omissions.truncated);
}

#[test]
fn contradictory_resolved_kind_fails_closed_and_rejects_serving_projection() {
    let (_directory, connection, workspace, _config, generation) = fixture();
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    connection
        .execute(
            "UPDATE relationship_resolution SET resolved_ref_kind = 'path'
             WHERE relationship_fact_id = 'rp4-consumed-edge'",
            [],
        )
        .unwrap();
    let request = CompileRequest {
        known_symbols: vec![alpha],
        ..CompileRequest::default()
    };
    let fallback = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &request,
        101,
    );
    assert!(fallback
        .relationships
        .iter()
        .all(|relationship| relationship.resolution_state == ResolutionStatus::Invalid));
    assert_eq!(fallback.working_set.len(), 1);

    build_serving_generation(&connection, &workspace).unwrap();
    let repaired = recompile(
        &connection,
        &workspace,
        &fallback,
        TaskKind::Explore,
        &request,
    );
    assert_eq!(
        serde_json::to_value(&fallback).unwrap(),
        serde_json::to_value(&repaired).unwrap()
    );
}

#[test]
fn corruption_fallback_keeps_one_request_wide_hard_deadline() {
    let (_directory, connection, workspace, _config, generation) = fixture();
    let alpha = insert_consumed_relationship(&connection, &workspace, &generation);
    let template_request = CompileRequest {
        known_symbols: vec![alpha.clone()],
        ..CompileRequest::default()
    };
    let request = CompileRequest {
        known_symbols: vec![alpha],
        soft_latency_ms: 1,
        hard_latency_ms: 5,
        ..CompileRequest::default()
    };
    let template = compile(
        &connection,
        &workspace,
        &generation,
        TaskKind::Explore,
        &template_request,
        111,
    );
    let build = build_serving_generation(&connection, &workspace).unwrap();
    connection
        .execute(
            "DELETE FROM symbol_serving_projection
             WHERE serving_generation_id = ?1 AND canonical_path = 'src/a.ts'",
            [&build.serving_generation_id],
        )
        .unwrap();
    let clock = SequenceClock::new(vec![0, 1_000, 1_000, 6_000]);
    let error = compile_context_ir_with_clock(
        &connection,
        &workspace,
        &template.task.task_session_id,
        &template.task.task_hash,
        &template.task.normalized_goal_hash,
        TaskKind::Explore,
        TaskKindSource::Declared,
        None,
        &request,
        &clock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("context_ir_interrupted"));
    let (serving_fallback, serving_lookup_us): (i64, i64) = connection
        .query_row(
            "SELECT serving_fallback, serving_lookup_us
             FROM query_stage_metric
             WHERE operation = 'context_ir_compile_interrupted'
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(serving_fallback, 1);
    assert_eq!(serving_lookup_us, 6_000);
}
