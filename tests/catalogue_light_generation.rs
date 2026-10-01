use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_int, c_uint, c_void, CStr};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use workspace_atlas::catalogue::{init_catalogue, open_connection};
use workspace_atlas::config::Config;
use workspace_atlas::context_application::{
    build_governor_run_request, dispatch_catalogue_context_application, run_catalogue_governor,
    CatalogueContextApplicationOutput, GovernorRunOutput, GovernorRunRequest,
    GovernorRunSemanticInput,
};
use workspace_atlas::context_ir::{TaskKind, TaskSessionState, CONTEXT_SCHEMA_VERSION};
use workspace_atlas::context_metrics::ContextExecutionCounters;
use workspace_atlas::context_route::{
    AtlasIntent, AttemptState, CallerCapabilityState, CapabilityDeficitReason, ContextExecution,
    ContextPayload, ExecutionAttempt, LightOperationResult, NegotiationRequest, RequiredCapability,
    Route, RouteDeficit, RouteFailureReason, TerminalReason, CONTEXT_CAPABILITIES_VERSION,
    CONTEXT_EXECUTION_VERSION, CONTEXT_IR_VERSION, CONTEXT_ROUTE_POLICY_VERSION,
    QUERY_OPERATION_VERSION, SOURCE_REFERENCE_OPERATION_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::task_session::{
    load_task_session, start_legacy_task_session, LegacyTaskStartRequest,
};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

const FIRST_PATH: &str = "src/first.ts";
const SECOND_PATH: &str = "src/second.ts";
const FIRST_A: &str = "export const first = 'generation-a-first';\n";
const SECOND_A: &str = "export const second = 'generation-a-second';\n";
const FIRST_B: &str = "export const first = 'generation-b-first';\n";
const SECOND_B: &str = "export const second = 'generation-b-second';\n";
const SIZE_SELECT_MARKER: &str = "SELECT fr.byte_size FROM current_file cf";
const HASH_SELECT_MARKER: &str =
    "SELECT content_hash FROM current_file WHERE generation_id = ?1 AND canonical_path = ?2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TracePhase {
    FirstSize,
    FirstHash,
    SecondSize,
}

#[derive(Debug, Clone)]
struct ReconcileProof {
    generation_b: String,
    active_generation: String,
    hashes_b: BTreeMap<String, String>,
}

struct GenerationTraceState {
    phase: TracePhase,
    writer: Connection,
    workspace: WorkspaceRecord,
    config: Config,
    workspace_root: PathBuf,
    size_statements: usize,
    hash_statements: usize,
    size_statements_at_trigger: Option<usize>,
    hash_statements_at_trigger: Option<usize>,
    trigger_count: usize,
    triggered_sql: Option<String>,
    reconcile_result: Option<Result<ReconcileProof, String>>,
    callback_panicked: bool,
}

impl GenerationTraceState {
    fn observe_statement(&mut self, sql: &str) {
        let size_lookup = sql.contains(SIZE_SELECT_MARKER);
        let hash_lookup = sql.contains(HASH_SELECT_MARKER);
        if size_lookup {
            self.size_statements += 1;
        }
        if hash_lookup {
            self.hash_statements += 1;
        }
        let should_trigger = match self.phase {
            TracePhase::FirstSize => size_lookup && self.size_statements == 1,
            TracePhase::FirstHash => hash_lookup && self.hash_statements == 1,
            TracePhase::SecondSize => size_lookup && self.size_statements == 2,
        };
        if should_trigger && self.trigger_count == 0 {
            self.trigger_count = 1;
            self.size_statements_at_trigger = Some(self.size_statements);
            self.hash_statements_at_trigger = Some(self.hash_statements);
            self.triggered_sql = Some(sql.to_owned());
            let result = reconcile_generation_b(
                &self.writer,
                &self.workspace,
                &self.config,
                &self.workspace_root,
            );
            self.reconcile_result = Some(result);
        }
    }
}

unsafe extern "C" fn generation_trace_callback(
    trace_code: c_uint,
    context: *mut c_void,
    statement: *mut c_void,
    _expanded_sql: *mut c_void,
) -> c_int {
    if trace_code != rusqlite::ffi::SQLITE_TRACE_STMT as c_uint
        || context.is_null()
        || statement.is_null()
    {
        return 0;
    }

    let callback_result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: with_generation_trace keeps the state fixed and alive until
        // after this synchronous callback is unregistered.
        let state = unsafe { &mut *context.cast::<GenerationTraceState>() };
        let statement = statement.cast::<rusqlite::ffi::sqlite3_stmt>();
        // SAFETY: SQLite supplies a live statement pointer for STMT events.
        let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement) };
        if !sql.is_null() {
            // SAFETY: sqlite3_sql returns a NUL-terminated string owned by the
            // live statement for the duration of this callback.
            let sql = unsafe { CStr::from_ptr(sql) }.to_string_lossy();
            state.observe_statement(&sql);
        }
    }));
    if callback_result.is_err() {
        // SAFETY: the same state remains live until trace unregistration.
        unsafe { &mut *context.cast::<GenerationTraceState>() }.callback_panicked = true;
    }
    0
}

fn with_generation_trace<T>(
    reader: &mut Connection,
    state: &mut GenerationTraceState,
    operation: impl FnOnce(&mut Connection) -> T,
) -> T {
    // SAFETY: callback execution is synchronous; state remains fixed and live,
    // and reader is exclusively borrowed until the callback is unregistered.
    let register_result = unsafe {
        rusqlite::ffi::sqlite3_trace_v2(
            reader.handle(),
            rusqlite::ffi::SQLITE_TRACE_STMT as c_uint,
            Some(generation_trace_callback),
            (state as *mut GenerationTraceState).cast(),
        )
    };
    assert_eq!(register_result, rusqlite::ffi::SQLITE_OK);

    let operation_result = catch_unwind(AssertUnwindSafe(|| operation(reader)));

    // SAFETY: use the same exclusively borrowed live connection and clear the
    // callback before state or reader can be released.
    let unregister_result =
        unsafe { rusqlite::ffi::sqlite3_trace_v2(reader.handle(), 0, None, std::ptr::null_mut()) };
    assert_eq!(unregister_result, rusqlite::ffi::SQLITE_OK);

    match operation_result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

struct Fixture {
    _database_directory: tempfile::TempDir,
    workspace_directory: tempfile::TempDir,
    database_path: PathBuf,
    reader: Connection,
    workspace: WorkspaceRecord,
    config: Config,
    generation_a: String,
    hashes_a: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        let database_directory = tempfile::tempdir().unwrap();
        let workspace_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
        std::fs::write(workspace_directory.path().join(FIRST_PATH), FIRST_A).unwrap();
        std::fs::write(workspace_directory.path().join(SECOND_PATH), SECOND_A).unwrap();
        std::fs::write(
            workspace_directory.path().join("tsconfig.json"),
            "{\"compilerOptions\":{\"strict\":true}}\n",
        )
        .unwrap();

        let config = Config::parse(
            "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"light generation validator\"\n[database]\nbusy_timeout_ms = 5000\njournal_mode = \"WAL\"\nsynchronous = \"NORMAL\"\n",
        )
        .unwrap();
        let database_path = database_directory.path().join("atlas.sqlite");
        let reader = init_catalogue(&database_path, &config).unwrap();
        let workspace = register_workspace(
            &reader,
            workspace_directory.path(),
            &config,
            &database_path,
            "light-generation-validator-v1",
        )
        .unwrap();
        let reconcile = discovery::reconcile(&workspace, &reader, &config).unwrap();
        assert_eq!(reconcile.activation, "committed");
        let generation_a = reconcile.candidate_generation_id;
        let hashes_a = generation_hashes(&reader, &generation_a);
        assert_eq!(hashes_a.len(), 2);

        Self {
            _database_directory: database_directory,
            workspace_directory,
            database_path,
            reader,
            workspace,
            config,
            generation_a,
            hashes_a,
        }
    }

    fn trace_state(&self, phase: TracePhase) -> GenerationTraceState {
        GenerationTraceState {
            phase,
            writer: open_connection(&self.database_path, &self.config).unwrap(),
            workspace: self.workspace.clone(),
            config: self.config.clone(),
            workspace_root: self.workspace_directory.path().to_path_buf(),
            size_statements: 0,
            hash_statements: 0,
            size_statements_at_trigger: None,
            hash_statements_at_trigger: None,
            trigger_count: 0,
            triggered_sql: None,
            reconcile_result: None,
            callback_panicked: false,
        }
    }

    fn reconcile_b(&self) -> ReconcileProof {
        let writer = open_connection(&self.database_path, &self.config).unwrap();
        reconcile_generation_b(
            &writer,
            &self.workspace,
            &self.config,
            self.workspace_directory.path(),
        )
        .unwrap()
    }

    fn write_b_without_reconcile(&self) {
        write_generation_b(self.workspace_directory.path()).unwrap();
    }
}

fn write_generation_b(workspace_root: &Path) -> Result<(), String> {
    std::fs::write(workspace_root.join(FIRST_PATH), FIRST_B).map_err(|error| error.to_string())?;
    std::fs::write(workspace_root.join(SECOND_PATH), SECOND_B)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn generation_hashes(connection: &Connection, generation_id: &str) -> BTreeMap<String, String> {
    [FIRST_PATH, SECOND_PATH]
        .into_iter()
        .map(|path| {
            let hash = connection
                .query_row(
                    "SELECT content_hash FROM current_file WHERE generation_id = ?1 AND canonical_path = ?2",
                    rusqlite::params![generation_id, path],
                    |row| row.get(0),
                )
                .unwrap();
            (path.to_owned(), hash)
        })
        .collect()
}

fn reconcile_generation_b(
    writer: &Connection,
    workspace: &WorkspaceRecord,
    config: &Config,
    workspace_root: &Path,
) -> Result<ReconcileProof, String> {
    write_generation_b(workspace_root)?;
    let reconcile =
        discovery::reconcile(workspace, writer, config).map_err(|error| error.to_string())?;
    if reconcile.activation != "committed" {
        return Err(format!(
            "generation B did not commit: {}",
            reconcile.activation
        ));
    }
    let generation_b = reconcile.candidate_generation_id;
    let active_generation: Option<String> = writer
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            [&workspace.workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let active_generation =
        active_generation.ok_or_else(|| "generation B is not active".to_owned())?;
    let hashes_b = generation_hashes(writer, &generation_b);
    Ok(ReconcileProof {
        generation_b,
        active_generation,
        hashes_b,
    })
}

fn supported_versions() -> NegotiationRequest {
    NegotiationRequest {
        capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_owned()],
        route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_owned()],
        decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_owned()],
        execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_owned()],
        ir_versions: vec![CONTEXT_IR_VERSION.to_owned()],
        operation_versions: vec![
            QUERY_OPERATION_VERSION.to_owned(),
            SOURCE_REFERENCE_OPERATION_VERSION.to_owned(),
        ],
    }
}

fn governor_request(paths: &[&str]) -> GovernorRunRequest {
    GovernorRunRequest {
        supported_versions: supported_versions(),
        semantic: GovernorRunSemanticInput {
            task: "Verify exact source through real LIGHT acquisition".to_owned(),
            declared_kind: Some(TaskKind::BugFix),
            path_targets: paths.iter().map(|path| (*path).to_owned()).collect(),
            symbol_targets: vec![],
            caller_capabilities: BTreeMap::from([(
                RequiredCapability::ExactSource,
                CallerCapabilityState::Unsatisfied,
            )]),
            atlas_intent: AtlasIntent::Allow,
            route_floor: Route::AtlasLight,
            route_ceiling: Route::AtlasLight,
            legacy_task_session_id: None,
            deep_limits: None,
            materialize_source: false,
            max_materialized_bytes: None,
        },
    }
}

fn bound_context_counts(connection: &Connection, task_session_id: &str) -> (i64, i64, i64, i64) {
    let contexts = connection
        .query_row(
            "SELECT COUNT(*) FROM context_ir WHERE task_session_id = ?1",
            [task_session_id],
            |row| row.get(0),
        )
        .unwrap();
    let items = connection
        .query_row(
            "SELECT COUNT(*) FROM context_ir_item item
             JOIN context_ir context ON context.context_id = item.context_id
             WHERE context.task_session_id = ?1",
            [task_session_id],
            |row| row.get(0),
        )
        .unwrap();
    let events = connection
        .query_row(
            "SELECT COUNT(*) FROM context_use_event WHERE task_session_id = ?1",
            [task_session_id],
            |row| row.get(0),
        )
        .unwrap();
    let metrics = connection
        .query_row(
            "SELECT COUNT(*) FROM query_stage_metric WHERE task_session_id = ?1",
            [task_session_id],
            |row| row.get(0),
        )
        .unwrap();
    (contexts, items, events, metrics)
}

fn assert_hook_proof(
    state: &GenerationTraceState,
    generation_a: &str,
    expected_size_count: usize,
    expected_hash_count: usize,
) -> ReconcileProof {
    assert!(!state.callback_panicked, "trace callback caught a panic");
    assert_eq!(state.trigger_count, 1, "trace must fire exactly once");
    assert_eq!(
        state.size_statements_at_trigger,
        Some(expected_size_count),
        "unexpected size-query ordinal at trigger"
    );
    assert_eq!(
        state.hash_statements_at_trigger,
        Some(expected_hash_count),
        "unexpected hash-query ordinal at trigger"
    );
    assert!(state.triggered_sql.is_some());
    let proof = state
        .reconcile_result
        .as_ref()
        .expect("trace must record reconcile result")
        .as_ref()
        .expect("trace reconcile must succeed")
        .clone();
    assert_ne!(proof.generation_b, generation_a);
    assert_eq!(proof.active_generation, proof.generation_b);
    assert_eq!(proof.hashes_b.len(), 2);
    proof
}

fn execute_race(
    phase: TracePhase,
    paths: &[&str],
    expected_size_count: usize,
    expected_hash_count: usize,
) -> (GovernorRunOutput, ReconcileProof) {
    let mut fixture = Fixture::new();
    let generation_a = fixture.generation_a.clone();
    let workspace = fixture.workspace.clone();
    let request = governor_request(paths);
    let mut state = fixture.trace_state(phase);
    let output = with_generation_trace(&mut fixture.reader, &mut state, |reader| {
        run_catalogue_governor(request, reader, &workspace)
    })
    .unwrap();
    let proof = assert_hook_proof(
        &state,
        &generation_a,
        expected_size_count,
        expected_hash_count,
    );
    (output, proof)
}

fn sum_attempt_counters(attempts: &[ExecutionAttempt]) -> ContextExecutionCounters {
    attempts
        .iter()
        .fold(ContextExecutionCounters::default(), |mut total, attempt| {
            total.atlas_calls = total
                .atlas_calls
                .checked_add(attempt.counters.atlas_calls)
                .expect("attempt Atlas-call counter sum");
            total.records = total
                .records
                .checked_add(attempt.counters.records)
                .expect("attempt record counter sum");
            total.source_bytes = total
                .source_bytes
                .checked_add(attempt.counters.source_bytes)
                .expect("attempt source-byte counter sum");
            total.estimated_tokens = total
                .estimated_tokens
                .checked_add(attempt.counters.estimated_tokens)
                .expect("attempt estimated-token counter sum");
            total.work_units = total
                .work_units
                .checked_add(attempt.counters.work_units)
                .expect("attempt work-unit counter sum");
            total
        })
}

fn assert_generation_changed_execution(execution: &ContextExecution, require_zero_work: bool) {
    execution
        .validate()
        .expect("GenerationChanged execution must satisfy public invariants");
    match execution {
        ContextExecution::Blocked {
            attempts,
            final_route: Some(Route::AtlasLight),
            payload: None,
            deficits,
            counters,
            materialization: None,
            terminal_reason: TerminalReason::GenerationChanged,
            ..
        } => {
            assert_eq!(
                deficits,
                &[RouteDeficit::Route {
                    reason: RouteFailureReason::GenerationChanged,
                }]
            );
            assert!(
                attempts
                    .iter()
                    .all(|attempt| attempt.route != Route::AtlasDeep),
                "generation change must not escalate to DEEP"
            );
            let light = attempts
                .iter()
                .find(|attempt| attempt.route == Route::AtlasLight)
                .expect("LIGHT terminal attempt");
            assert_eq!(light.state, AttemptState::Blocked);
            assert_eq!(light.deficits, *deficits);
            assert_eq!(
                *counters,
                sum_attempt_counters(attempts),
                "aggregate counters must equal the actual attempt counters"
            );
            if require_zero_work {
                assert_eq!(*counters, ContextExecutionCounters::default());
                assert!(
                    attempts
                        .iter()
                        .all(|attempt| attempt.counters == ContextExecutionCounters::default()),
                    "pre-dispatch stale control must perform no Atlas work"
                );
            }
        }
        other => panic!("expected blocked LIGHT GenerationChanged execution, got {other:#?}"),
    }
}

fn assert_generation_changed_output(output: &GovernorRunOutput) {
    assert_generation_changed_execution(&output.execution, false);
    assert!(
        output.light_results.is_none(),
        "generation change must suppress LIGHT result delivery"
    );
    assert!(
        output.deep_context_ir.is_none(),
        "generation change must not expose DEEP output"
    );
}

fn assert_verified_light_sources(
    output: &GovernorRunOutput,
    expected_hashes: &BTreeMap<String, String>,
) {
    match &output.execution {
        ContextExecution::Completed {
            final_route: Route::AtlasLight,
            payload: ContextPayload::LightBundle { operations },
            counters,
            materialization: None,
            ..
        } => {
            assert!(counters.atlas_calls > 0);
            assert!(counters.records > 0);
            assert!(counters.work_units > 0);
            assert!(matches!(
                operations.as_slice(),
                [LightOperationResult::SourceReference { reference_count, .. }]
                    if *reference_count == expected_hashes.len() as u64
            ));
        }
        other => panic!("expected completed verified LIGHT execution, got {other:#?}"),
    }
    assert!(output.deep_context_ir.is_none());
    let light = output
        .light_results
        .as_ref()
        .expect("verified LIGHT results");
    assert!(light.query.is_none());
    let references = light
        .source_references
        .as_ref()
        .expect("verified source references");
    assert_eq!(references.len(), expected_hashes.len());
    let actual_paths: BTreeSet<&str> = references
        .iter()
        .map(|reference| reference.canonical_path.as_str())
        .collect();
    let expected_paths: BTreeSet<&str> = expected_hashes.keys().map(String::as_str).collect();
    assert_eq!(
        actual_paths.len(),
        references.len(),
        "verified LIGHT references must not contain duplicate paths"
    );
    assert_eq!(
        actual_paths, expected_paths,
        "verified LIGHT references must contain exactly the expected canonical paths"
    );
    for reference in references {
        let expected = expected_hashes
            .get(&reference.canonical_path)
            .expect("unexpected source reference path");
        assert_eq!(reference.status, "verified");
        assert_eq!(reference.indexed_hash.as_deref(), Some(expected.as_str()));
        assert_eq!(reference.observed_hash.as_deref(), Some(expected.as_str()));
    }
}

fn assert_stale_source_output(output: &GovernorRunOutput) {
    match &output.execution {
        ContextExecution::Blocked {
            final_route: Some(Route::AtlasLight),
            payload: None,
            deficits,
            counters,
            materialization: None,
            ..
        } => {
            assert!(deficits.contains(&RouteDeficit::Capability {
                capability: RequiredCapability::ExactSource,
                reason: CapabilityDeficitReason::Stale,
            }));
            assert!(counters.atlas_calls > 0);
            assert!(counters.records > 0);
            assert!(counters.work_units > 0);
        }
        other => panic!("expected blocked stale-source LIGHT execution, got {other:#?}"),
    }
    assert!(output.light_results.is_none());
    assert!(output.deep_context_ir.is_none());
}

#[test]
fn generation_change_before_first_size_lookup_is_explicit() {
    let (output, proof) = execute_race(TracePhase::FirstSize, &[FIRST_PATH], 1, 0);
    assert_eq!(proof.active_generation, proof.generation_b);
    assert_generation_changed_output(&output);
}

#[test]
fn generation_change_between_size_and_hash_lookup_is_explicit() {
    let (output, proof) = execute_race(TracePhase::FirstHash, &[FIRST_PATH], 1, 1);
    assert_eq!(proof.active_generation, proof.generation_b);
    assert_generation_changed_output(&output);
}

#[test]
fn generation_change_after_first_verified_target_is_explicit() {
    let (output, proof) = execute_race(TracePhase::SecondSize, &[FIRST_PATH, SECOND_PATH], 2, 1);
    assert_eq!(proof.active_generation, proof.generation_b);
    assert_generation_changed_output(&output);
}

#[test]
fn stable_generation_returns_verified_a_source_references() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.clone();
    let output = run_catalogue_governor(
        governor_request(&[FIRST_PATH, SECOND_PATH]),
        &mut fixture.reader,
        &workspace,
    )
    .unwrap();
    assert_verified_light_sources(&output, &fixture.hashes_a);
}

#[test]
fn already_stale_a_request_blocks_before_light_work() {
    let mut fixture = Fixture::new();
    let application = build_governor_run_request(
        governor_request(&[FIRST_PATH]),
        &fixture.reader,
        &fixture.workspace,
    )
    .unwrap();
    let proof = fixture.reconcile_b();
    assert_ne!(proof.generation_b, fixture.generation_a);

    let output: CatalogueContextApplicationOutput = dispatch_catalogue_context_application(
        application.route_request,
        application.catalogue_input,
        &mut fixture.reader,
        &fixture.workspace,
    )
    .unwrap();
    assert_generation_changed_execution(&output.execution, true);
    assert!(output.light_results.is_none());
    assert!(output.deep_context_ir.is_none());
}

#[test]
fn fresh_resubmission_under_b_returns_verified_b_hashes() {
    let mut fixture = Fixture::new();
    let proof = fixture.reconcile_b();
    let workspace = fixture.workspace.clone();
    let output = run_catalogue_governor(
        governor_request(&[FIRST_PATH, SECOND_PATH]),
        &mut fixture.reader,
        &workspace,
    )
    .unwrap();
    assert_verified_light_sources(&output, &proof.hashes_b);
}

#[test]
fn source_change_without_reconcile_remains_stale_deficit() {
    let mut fixture = Fixture::new();
    fixture.write_b_without_reconcile();
    let workspace = fixture.workspace.clone();
    let output = run_catalogue_governor(
        governor_request(&[FIRST_PATH]),
        &mut fixture.reader,
        &workspace,
    )
    .unwrap();
    assert_stale_source_output(&output);
}

#[test]
fn bound_task_generation_race_blocks_without_activation_or_derived_context() {
    let mut fixture = Fixture::new();
    let task = "Verify exact source through real LIGHT acquisition";
    let session = start_legacy_task_session(
        &mut fixture.reader,
        &fixture.workspace,
        &LegacyTaskStartRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            task: task.to_string(),
            declared_kind: Some(TaskKind::BugFix),
        },
    )
    .unwrap();
    assert_eq!(session.state, TaskSessionState::Created);
    let before_session = serde_json::to_value(
        load_task_session(&fixture.reader, &session.task_session_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let before_context = bound_context_counts(&fixture.reader, &session.task_session_id);

    let generation_a = fixture.generation_a.clone();
    let workspace = fixture.workspace.clone();
    let mut request = governor_request(&[FIRST_PATH]);
    request.semantic.legacy_task_session_id = Some(session.task_session_id.clone());
    let mut state = fixture.trace_state(TracePhase::FirstHash);
    let output = with_generation_trace(&mut fixture.reader, &mut state, |reader| {
        run_catalogue_governor(request, reader, &workspace)
    })
    .unwrap();
    let proof = assert_hook_proof(&state, &generation_a, 1, 1);

    assert_eq!(proof.active_generation, proof.generation_b);
    assert_generation_changed_output(&output);
    assert!(
        output.execution.payload().is_none(),
        "invalidated LIGHT work must deliver no query, impact, source, or deep payload"
    );
    let after_session = serde_json::to_value(
        load_task_session(&fixture.reader, &session.task_session_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        after_session, before_session,
        "writer reconciliation leaves created sessions alone and blocked acquisition must not activate the bound session"
    );
    assert_eq!(
        bound_context_counts(&fixture.reader, &session.task_session_id),
        before_context,
        "blocked bound acquisition must not persist context, context items, events, or query metrics"
    );
}
