use std::collections::BTreeMap;

use rusqlite::{types::Value, Connection};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_application::{
    dispatch_catalogue_context_application, run_catalogue_governor,
    CatalogueContextApplicationInput, GovernorDeepLimits, GovernorRunRequest,
    GovernorRunSemanticInput,
};
use workspace_atlas::context_ir::{
    DeepContextIrV2, IrOmissionReasonV2, IrRequirementStateV2, IrSourceVerificationV2, ItemRole,
    RawTaskRetention, TaskKind, WorkingSetStatus, CONTEXT_SCHEMA_V2_VERSION,
    CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::context_metrics::ContextExecutionCounters;
use workspace_atlas::context_route::{
    AtlasIntent, CallerCapabilityState, CapabilityDeficitReason, ContextExecution, ContextPayload,
    ContextRouteRequest, DeepContractVersions, DeepSemanticBudget, GenerationObservation,
    LightOperation, LightOperationResult, NegotiationRequest, RequiredCapability, Route,
    RouteDeficit, SeedSummary, VersionedOperation, CONTEXT_CAPABILITIES_VERSION,
    CONTEXT_EXECUTION_VERSION, CONTEXT_IR_VERSION, CONTEXT_ROUTE_POLICY_VERSION,
    QUERY_OPERATION_VERSION, SOURCE_REFERENCE_OPERATION_VERSION, TASK_CLASSIFIER_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::serving::build_serving_generation;
use workspace_atlas::task_compiler::{
    classify_task, compile_deep_context_ir_v2, normalized_task_hash,
    seal_deep_context_ir_v2_with_live_sources, DeepV2CompileRequest,
};
use workspace_atlas::task_session::create_classified_task_session;
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

struct Fixture {
    _database_directory: tempfile::TempDir,
    _workspace_directory: tempfile::TempDir,
    connection: Connection,
    workspace: WorkspaceRecord,
    generation_id: String,
}

impl Fixture {
    fn new() -> Self {
        Self::with_unrelated_files(0)
    }

    fn with_unrelated_files(unrelated_file_count: usize) -> Self {
        let database_directory = tempfile::tempdir().unwrap();
        let workspace_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("tests")).unwrap();
        std::fs::write(
            workspace_directory.path().join("src/lib.rs"),
            "pub fn calculate(value: i64) -> i64 { value + 1 }\n",
        )
        .unwrap();
        std::fs::write(
            workspace_directory.path().join("src/other.rs"),
            "pub fn other(value: i64) -> i64 { value - 1 }\n",
        )
        .unwrap();
        std::fs::write(
            workspace_directory.path().join("tests/calculate.rs"),
            "#[test]\nfn calculates() { assert_eq!(2, 2); }\n",
        )
        .unwrap();
        std::fs::write(
            workspace_directory.path().join("Cargo.toml"),
            "[package]\nname = \"compiler-composition\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        for index in 0..unrelated_file_count {
            std::fs::write(
                workspace_directory
                    .path()
                    .join(format!("src/unrelated_{index:03}.rs")),
                format!("pub fn unrelated_{index:03}() -> usize {{ {index} }}\n"),
            )
            .unwrap();
        }

        let config = Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"compiler-composition\"\n",
        )
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
        let generation_id = discovery::reconcile(&workspace, &connection, &config)
            .unwrap()
            .candidate_generation_id;

        Self {
            _database_directory: database_directory,
            _workspace_directory: workspace_directory,
            connection,
            workspace,
            generation_id,
        }
    }

    fn reconcile_again(&mut self) -> String {
        std::fs::write(
            std::path::Path::new(&self.workspace.canonical_root).join("src/lib.rs"),
            "pub fn calculate(value: i64) -> i64 { value + 2 }\n",
        )
        .unwrap();
        let config = Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"compiler-composition\"\n",
        )
        .unwrap();
        discovery::reconcile(&self.workspace, &self.connection, &config)
            .unwrap()
            .candidate_generation_id
    }
}

fn budget() -> DeepSemanticBudget {
    DeepSemanticBudget::new(50, 65_536, 16_384, 4, 10_000, 12).unwrap()
}

fn input(task: &str, paths: &[&str]) -> CatalogueContextApplicationInput {
    CatalogueContextApplicationInput {
        task: task.to_string(),
        declared_task_kind: Some(TaskKind::Explore),
        task_session_id: None,
        seed_paths: paths.iter().map(|path| (*path).to_string()).collect(),
        seed_symbols: vec![],
        baseline_generation_id: None,
    }
}

fn with_existing_session(
    fixture: &Fixture,
    generation_id: &str,
    mut input: CatalogueContextApplicationInput,
) -> CatalogueContextApplicationInput {
    let task_hash = workspace_atlas::hashing::content_hash_of_bytes(input.task.as_bytes());
    let normalized_goal_hash = normalized_task_hash(&input.task);
    let (task_kind, kind_source, kind_rule_id) =
        classify_task(input.declared_task_kind, &input.task);
    let session = create_classified_task_session(
        &fixture.connection,
        &fixture.workspace,
        generation_id,
        &task_hash,
        &normalized_goal_hash,
        RawTaskRetention::None,
        None,
        task_kind,
        kind_source,
        kind_rule_id.as_deref(),
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();
    input.task_session_id = Some(session.task_session_id);
    input
}

fn deep_compile_request(input: &CatalogueContextApplicationInput) -> DeepV2CompileRequest {
    let task_hash = workspace_atlas::hashing::content_hash_of_bytes(input.task.as_bytes());
    let normalized_goal_hash = normalized_task_hash(&input.task);
    let (task_kind, kind_source, kind_rule_id) =
        classify_task(input.declared_task_kind, &input.task);
    DeepV2CompileRequest {
        task_session_id: input.task_session_id.clone().unwrap(),
        task_hash,
        normalized_goal_hash,
        task_kind,
        kind_source,
        kind_rule_id,
        seed_paths: input.seed_paths.clone(),
        seed_symbols: input.seed_symbols.clone(),
        baseline_generation_id: input.baseline_generation_id.clone(),
    }
}

fn insert_primary_symbol_truth(fixture: &Fixture) -> String {
    let canonical_symbol_key = "rp8::calculate".to_string();
    let (revision_id, extractor_run_id) =
        revision_and_run(&fixture.connection, &fixture.generation_id, "src/lib.rs");
    fixture
        .connection
        .execute(
            "INSERT INTO symbol_fact (
                symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
                symbol_kind, display_name, qualified_name, signature,
                start_byte, end_byte, start_line, start_column, end_line, end_column,
                attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (?1,?2,?3,?4,'function','calculate','calculate',
                       'fn calculate(i64) -> i64',0,50,1,1,1,51,'{}',
                       'semantic',1.0,'RP8 controlled Truth')",
            rusqlite::params![
                format!("rp8-symbol-{}", fixture.generation_id),
                extractor_run_id,
                revision_id,
                canonical_symbol_key,
            ],
        )
        .unwrap();
    canonical_symbol_key
}

fn revision_and_run(
    connection: &Connection,
    generation_id: &str,
    canonical_path: &str,
) -> (String, String) {
    connection
        .query_row(
            "SELECT cf.revision_id, er.extractor_run_id
             FROM current_file cf
             JOIN extractor_run er ON er.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = ?2
             ORDER BY er.extractor_run_id
             LIMIT 1",
            rusqlite::params![generation_id, canonical_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn insert_relationship_truth(
    fixture: &Fixture,
    fact_id: &str,
    source_path: &str,
    relationship_type: &str,
    source_kind: &str,
    source_value: &str,
    target_kind: &str,
    target_value: &str,
    resolution_status: &str,
    resolved_kind: &str,
    resolved_value: &str,
) {
    let (revision_id, extractor_run_id) =
        revision_and_run(&fixture.connection, &fixture.generation_id, source_path);
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'{}','semantic',1.0,'RP8 controlled Truth')",
            rusqlite::params![
                fact_id,
                extractor_run_id,
                revision_id,
                relationship_type,
                source_kind,
                source_value,
                target_kind,
                target_value,
            ],
        )
        .unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_resolution (
                relationship_resolution_id, workspace_id, generation_id,
                relationship_fact_id, resolver_policy_version, status,
                resolved_ref_kind, resolved_ref_value, reason_code,
                candidate_refs_json, confidence, evidence_json, created_at
             ) VALUES (?1,?2,?3,?4,'fixture-resolution-v1',?5,?6,?7,
                       'fixture','[]',1.0,'[]','2026-09-03T00:00:00Z')",
            rusqlite::params![
                format!("resolution-{fact_id}"),
                fixture.workspace.workspace_id,
                fixture.generation_id,
                fact_id,
                resolution_status,
                resolved_kind,
                resolved_value,
            ],
        )
        .unwrap();
}

fn insert_effect_truth(fixture: &Fixture, subject_symbol: &str) {
    let (revision_id, extractor_run_id) =
        revision_and_run(&fixture.connection, &fixture.generation_id, "src/lib.rs");
    fixture
        .connection
        .execute(
            "INSERT INTO effect_fact (
                effect_fact_id, extractor_run_id, revision_id, subject_ref_kind,
                subject_ref_value, effect_type, phase, attributes_json,
                evidence_method, confidence, evidence_reason
             ) VALUES ('rp8-effect',?1,?2,'symbol',?3,'writes_state','direct','{}',
                       'semantic',1.0,'RP8 controlled Truth')",
            rusqlite::params![extractor_run_id, revision_id, subject_symbol],
        )
        .unwrap();
}

fn install_role_closure_truth(fixture: &Fixture) -> String {
    let calculate = insert_primary_symbol_truth(fixture);
    insert_relationship_truth(
        fixture,
        "rp8-caller",
        "src/other.rs",
        "calls",
        "path",
        "src/other.rs",
        "symbol",
        &calculate,
        "resolved_symbol",
        "symbol",
        &calculate,
    );
    insert_relationship_truth(
        fixture,
        "rp8-type",
        "src/lib.rs",
        "implements",
        "symbol",
        &calculate,
        "path",
        "src/other.rs",
        "resolved_file",
        "path",
        "src/other.rs",
    );
    insert_relationship_truth(
        fixture,
        "rp8-test",
        "src/lib.rs",
        "tests",
        "symbol",
        &calculate,
        "path",
        "tests/calculate.rs",
        "resolved_file",
        "path",
        "tests/calculate.rs",
    );
    insert_relationship_truth(
        fixture,
        "rp8-config",
        "src/lib.rs",
        "configures",
        "symbol",
        &calculate,
        "path",
        "Cargo.toml",
        "resolved_file",
        "path",
        "Cargo.toml",
    );
    insert_effect_truth(fixture, &calculate);
    calculate
}

fn insert_unresolved_test_relationship_truth(fixture: &Fixture, source_symbol: &str) {
    let (revision_id, extractor_run_id) =
        revision_and_run(&fixture.connection, &fixture.generation_id, "src/lib.rs");
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES ('rp8-unresolved-test',?1,?2,'tests','symbol',?3,
                       'path','tests/calculate.rs','{}','semantic',1.0,
                       'RP8 controlled unresolved Truth')",
            rusqlite::params![extractor_run_id, revision_id, source_symbol],
        )
        .unwrap();
}

fn deep_input_for_kind(
    fixture: &Fixture,
    task_kind: TaskKind,
    calculate_symbol: &str,
) -> CatalogueContextApplicationInput {
    let mut application_input = input("RP8 role closure", &["src/lib.rs"]);
    application_input.declared_task_kind = Some(task_kind);
    application_input.seed_symbols = vec![calculate_symbol.to_string()];
    with_existing_session(fixture, &fixture.generation_id, application_input)
}

fn compile_before_and_after_serving(fixture: &mut Fixture) -> (DeepContextIrV2, DeepContextIrV2) {
    let generation_id = fixture.generation_id.clone();
    let application_input = with_existing_session(
        fixture,
        &generation_id,
        input("Explore one stable target", &["src/lib.rs"]),
    );
    let request = deep_compile_request(&application_input);
    let budget = DeepSemanticBudget::new(10, 65_536, 16_384, 4, 10, 10).unwrap();
    let truth = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &budget,
        None,
    )
    .unwrap()
    .context_ir;
    build_serving_generation(&fixture.connection, &fixture.workspace).unwrap();
    let ready = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &budget,
        None,
    )
    .unwrap()
    .context_ir;
    (truth, ready)
}

fn request(
    input: &CatalogueContextApplicationInput,
    capabilities: &[(RequiredCapability, CallerCapabilityState)],
    floor: Route,
    ceiling: Route,
    generation_id: &str,
) -> ContextRouteRequest {
    let (classifier_kind, _, _) = classify_task(input.declared_task_kind, &input.task);
    ContextRouteRequest {
        schema_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        normalized_task_hash: normalized_task_hash(&input.task),
        declared_kind: input.declared_task_kind,
        classifier_kind,
        classifier_version: TASK_CLASSIFIER_VERSION.to_string(),
        explicit_seeds: SeedSummary::from_targets(&input.seed_paths, &input.seed_symbols).unwrap(),
        caller_capabilities: capabilities.iter().copied().collect::<BTreeMap<_, _>>(),
        atlas_intent: AtlasIntent::Allow,
        route_floor: floor,
        route_ceiling: ceiling,
        capability_profile: None,
        cost_profile: None,
        profile_registry_digest: None,
        starting_generation: GenerationObservation::Observed {
            generation_id: generation_id.to_string(),
        },
        light_operations: vec![
            VersionedOperation::new(LightOperation::Query),
            VersionedOperation::new(LightOperation::SourceReference),
        ],
        deep_contracts: DeepContractVersions::fixed(),
        deep_budget: (ceiling == Route::AtlasDeep).then(budget),
    }
}

fn table_counts(connection: &Connection) -> (i64, i64, i64) {
    (
        connection
            .query_row("SELECT COUNT(*) FROM task_session", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM context_use_event", [], |row| {
                row.get(0)
            })
            .unwrap(),
    )
}

#[derive(Debug, PartialEq)]
struct DurableCompilerState {
    task_sessions: Vec<Vec<Value>>,
    context_irs: Vec<Vec<Value>>,
    context_use_events: Vec<Vec<Value>>,
    exact_source_ordinals: Vec<(String, i64)>,
}

fn complete_table_rows(connection: &Connection, table: &str, order_by: &str) -> Vec<Vec<Value>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order_by}"))
        .unwrap();
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn durable_compiler_state(connection: &Connection) -> DurableCompilerState {
    let exact_source_ordinals = connection
        .prepare(
            "SELECT task_session_id,
                    CAST(json_extract(details_json, '$.request_ordinal') AS INTEGER)
             FROM context_use_event
             WHERE event_type = 'exact_source_requested'
             ORDER BY task_session_id, 2, event_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    DurableCompilerState {
        task_sessions: complete_table_rows(connection, "task_session", "task_session_id"),
        context_irs: complete_table_rows(connection, "context_ir", "context_id"),
        context_use_events: complete_table_rows(
            connection,
            "context_use_event",
            "task_session_id, occurred_at, event_id",
        ),
        exact_source_ordinals,
    }
}

fn execution_counters(execution: &ContextExecution) -> &ContextExecutionCounters {
    match execution {
        ContextExecution::Completed { counters, .. }
        | ContextExecution::Partial { counters, .. }
        | ContextExecution::Blocked { counters, .. }
        | ContextExecution::Interrupted { counters, .. } => counters,
    }
}

fn dispatch_direct_and_escalated_deep(
    fixture: &mut Fixture,
    application_input: &CatalogueContextApplicationInput,
    capabilities: &[(RequiredCapability, CallerCapabilityState)],
    deep_budget: DeepSemanticBudget,
) -> (
    workspace_atlas::context_application::CatalogueContextApplicationOutput,
    workspace_atlas::context_application::CatalogueContextApplicationOutput,
) {
    let mut direct_request = request(
        application_input,
        capabilities,
        Route::AtlasDeep,
        Route::AtlasDeep,
        &fixture.generation_id,
    );
    direct_request.deep_budget = Some(deep_budget.clone());
    let direct = dispatch_catalogue_context_application(
        direct_request,
        application_input.clone(),
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();

    let mut escalated_request = request(
        application_input,
        capabilities,
        Route::Direct,
        Route::AtlasDeep,
        &fixture.generation_id,
    );
    escalated_request.deep_budget = Some(deep_budget);
    let escalated = dispatch_catalogue_context_application(
        escalated_request,
        application_input.clone(),
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    (direct, escalated)
}

fn assert_blocked_deep_deficits(
    output: &workspace_atlas::context_application::CatalogueContextApplicationOutput,
    expected_deficits: &[RouteDeficit],
    expected_atlas_calls: u64,
) {
    match &output.execution {
        ContextExecution::Blocked {
            final_route: Some(Route::AtlasDeep),
            payload: None,
            deficits,
            counters,
            ..
        } => {
            assert_eq!(deficits, expected_deficits);
            assert_eq!(counters.atlas_calls, expected_atlas_calls);
            assert!(counters.work_units > 0);
        }
        other => panic!("expected blocked DEEP execution, got {other:#?}"),
    }
    output.execution.validate().unwrap();
    assert!(output.execution.payload().is_none());
    assert!(
        output.deep_context_ir.is_none(),
        "a blocked capability outcome must not expose canonical Truth"
    );
}

#[test]
fn public_governor_runs_complete_bounded_progressive_journey_without_persistence() {
    let mut fixture = Fixture::new();
    let calculate_symbol = install_role_closure_truth(&fixture);
    let durable_before = durable_compiler_state(&fixture.connection);
    let request = GovernorRunRequest {
        supported_versions: NegotiationRequest {
            capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_string()],
            route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
            decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
            execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
            ir_versions: vec![CONTEXT_IR_VERSION.to_string()],
            operation_versions: vec![
                QUERY_OPERATION_VERSION.to_string(),
                SOURCE_REFERENCE_OPERATION_VERSION.to_string(),
            ],
        },
        semantic: GovernorRunSemanticInput {
            task: "Explore the bounded calculation behavior and validation closure".to_string(),
            declared_kind: Some(TaskKind::Explore),
            path_targets: vec!["src/lib.rs".to_string()],
            symbol_targets: vec![calculate_symbol],
            caller_capabilities: BTreeMap::from([
                (
                    RequiredCapability::IdentityLookup,
                    CallerCapabilityState::Unsatisfied,
                ),
                (
                    RequiredCapability::RequiredRoleClosure,
                    CallerCapabilityState::Unsatisfied,
                ),
                (
                    RequiredCapability::ValidationPlan,
                    CallerCapabilityState::Unsatisfied,
                ),
            ]),
            atlas_intent: AtlasIntent::Allow,
            route_floor: Route::Direct,
            route_ceiling: Route::AtlasDeep,
            legacy_task_session_id: None,
            deep_limits: Some(GovernorDeepLimits {
                max_records: 50,
                max_source_bytes: 65_536,
                max_estimated_tokens: 16_384,
                max_relationship_depth: 4,
                max_work_units: 10_000,
                uncertainty_reserve_percent: 12,
            }),
            materialize_source: false,
            max_materialized_bytes: None,
        },
    };

    let progressive =
        run_catalogue_governor(request.clone(), &mut fixture.connection, &fixture.workspace)
            .unwrap();

    match &progressive.execution {
        ContextExecution::Completed {
            attempts,
            final_route: Route::AtlasDeep,
            ..
        } => {
            assert_eq!(
                attempts
                    .iter()
                    .map(|attempt| attempt.route)
                    .collect::<Vec<_>>(),
                vec![Route::Direct, Route::AtlasLight, Route::AtlasDeep]
            );
            assert!(
                attempts.iter().all(|attempt| attempt.elapsed_micros > 0),
                "real DIRECT, LIGHT, and DEEP attempts must expose measured elapsed time"
            );
            assert_eq!(attempts[0].counters, ContextExecutionCounters::default());
            assert_eq!(attempts[1].counters.atlas_calls, 1);
            assert_eq!(attempts[1].counters.work_units, 2);
            assert_eq!(attempts[2].counters.atlas_calls, 1);
            assert!(attempts[2].counters.work_units > 0);
        }
        other => panic!("expected complete progressive DEEP execution, got {other:#?}"),
    }
    assert!(progressive.light_results.is_none());
    let progressive_ir = progressive
        .deep_context_ir
        .expect("completed progressive execution returns Context IR");
    assert_eq!(progressive_ir.schema_version, CONTEXT_SCHEMA_V2_VERSION);

    let mut direct_request = request.clone();
    direct_request.semantic.route_floor = Route::AtlasDeep;
    let direct =
        run_catalogue_governor(direct_request, &mut fixture.connection, &fixture.workspace)
            .unwrap();
    assert!(matches!(
        direct.execution,
        ContextExecution::Completed {
            ref attempts,
            final_route: Route::AtlasDeep,
            ..
        } if attempts.len() == 1 && attempts[0].route == Route::AtlasDeep
    ));
    let direct_ir = direct
        .deep_context_ir
        .expect("completed direct DEEP execution returns Context IR");
    assert_eq!(direct_ir.context_hash, progressive_ir.context_hash);
    assert_eq!(
        serde_json::to_vec(&direct_ir).unwrap(),
        serde_json::to_vec(&progressive_ir).unwrap()
    );

    build_serving_generation(&fixture.connection, &fixture.workspace).unwrap();
    let serving =
        run_catalogue_governor(request, &mut fixture.connection, &fixture.workspace).unwrap();
    let serving_ir = serving
        .deep_context_ir
        .expect("ready Serving execution returns Context IR");
    assert_eq!(serving_ir.context_hash, progressive_ir.context_hash);
    assert_eq!(
        serde_json::to_vec(&serving_ir).unwrap(),
        serde_json::to_vec(&progressive_ir).unwrap()
    );
    assert_eq!(
        durable_compiler_state(&fixture.connection),
        durable_before,
        "unattributed DIRECT, progressive, and Serving execution must stay response-lifetime only"
    );
}

#[test]
fn real_catalogue_direct_and_escalated_deep_generate_identical_canonical_ir() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let produced_test_classification: (String, bool) = fixture
        .connection
        .query_row(
            "SELECT artifact_class, is_test FROM current_file
             WHERE generation_id = ?1 AND canonical_path = 'tests/calculate.rs'",
            [&generation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        produced_test_classification,
        ("test".to_string(), true),
        "discovery must produce test Truth without fixture mutation"
    );
    let calculate_symbol = install_role_closure_truth(&fixture);
    let application_input =
        deep_input_for_kind(&fixture, TaskKind::BehaviorChange, &calculate_symbol);
    let capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ValidationPlan,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let durable_before = durable_compiler_state(&fixture.connection);

    let deep_request = request(
        &application_input,
        &capabilities,
        Route::AtlasDeep,
        Route::AtlasDeep,
        &fixture.generation_id,
    );
    let direct = dispatch_catalogue_context_application(
        deep_request.clone(),
        application_input.clone(),
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    let durable_after_success = durable_compiler_state(&fixture.connection);
    let replayed = dispatch_catalogue_context_application(
        deep_request,
        application_input.clone(),
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    let durable_after_replay = durable_compiler_state(&fixture.connection);
    let escalated = dispatch_catalogue_context_application(
        request(
            &application_input,
            &capabilities,
            Route::Direct,
            Route::AtlasDeep,
            &fixture.generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert_eq!(
        durable_after_replay, durable_before,
        "same-input V2 DEEP replay must not allocate events or ordinals"
    );
    assert_eq!(
        durable_compiler_state(&fixture.connection),
        durable_before,
        "escalated V2 DEEP execution must remain transient"
    );
    assert_eq!(
        durable_after_success, durable_before,
        "successful V2 DEEP execution must remain transient"
    );
    assert_eq!(
        fixture
            .connection
            .query_row("SELECT COUNT(*) FROM task_session", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1,
        "replayed DEEP routes must reuse the caller-owned lifecycle session"
    );
    assert_eq!(
        fixture
            .connection
            .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    assert!(
        matches!(
            &direct.execution,
            ContextExecution::Completed {
                final_route: Route::AtlasDeep,
                ..
            }
        ),
        "{:#?}",
        direct.execution
    );
    assert!(
        matches!(
            &escalated.execution,
            ContextExecution::Completed {
                final_route: Route::AtlasDeep,
                ..
            }
        ),
        "{:#?}",
        escalated.execution
    );
    assert_eq!(
        execution_counters(&direct.execution),
        execution_counters(&replayed.execution),
        "same-input V2 replay must return identical transient counters"
    );
    let direct_ir = direct.deep_context_ir.unwrap();
    let escalated_ir = escalated.deep_context_ir.unwrap();
    let replayed_ir = replayed.deep_context_ir.unwrap();
    assert_eq!(direct_ir.schema_version, CONTEXT_SCHEMA_V2_VERSION);
    assert_eq!(direct_ir.policy.planner_policy_version, "planner-v2.0.0");
    assert!(direct_ir
        .working_set
        .iter()
        .any(|item| item.entity_id == "path:src/lib.rs"));
    let test_contract = direct_ir
        .working_set
        .iter()
        .find(|item| item.entity_id == "path:tests/calculate.rs")
        .expect("producer-created test Truth must enter the working set");
    assert_eq!(
        test_contract.role,
        workspace_atlas::context_ir::ItemRole::TestContract
    );
    let test_role = direct_ir
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == workspace_atlas::context_ir::ItemRole::TestContract)
        .expect("behavior-change recipe must report test-contract sufficiency");
    assert_eq!(
        test_role.state,
        workspace_atlas::context_ir::IrRequirementStateV2::Satisfied
    );
    assert!(test_role.evidence_item_ids.contains(&test_contract.item_id));
    assert!(direct_ir
        .validation_plan
        .iter()
        .any(|validation| validation.target_entity_id == test_contract.entity_id));
    assert!(direct_ir.status.role_sufficiency.iter().any(|entry| {
        entry.role == ItemRole::DirectDependent && entry.state == IrRequirementStateV2::Satisfied
    }));
    assert!(direct_ir.status.role_sufficiency.iter().any(|entry| {
        entry.role == ItemRole::Effect && entry.state == IrRequirementStateV2::Satisfied
    }));
    assert!(direct_ir
        .relationships
        .iter()
        .any(|relationship| relationship.relationship_type == "calls"));
    assert!(direct_ir
        .effects
        .iter()
        .any(|effect| effect.effect_type == "writes_state"));
    assert_eq!(direct_ir.context_hash, replayed_ir.context_hash);
    assert_eq!(
        serde_json::to_vec(&direct_ir).unwrap(),
        serde_json::to_vec(&replayed_ir).unwrap()
    );
    assert_eq!(direct_ir.context_hash, escalated_ir.context_hash);
    assert_eq!(direct.execution.payload(), escalated.execution.payload());
    assert_eq!(
        serde_json::to_vec(&direct_ir).unwrap(),
        serde_json::to_vec(&escalated_ir).unwrap()
    );
    let verified_sources: Vec<_> = direct_ir
        .working_set
        .iter()
        .filter_map(|item| item.source.as_ref())
        .collect();
    assert!(verified_sources.len() >= 2);
    assert!(verified_sources.iter().all(|source| {
        source.revalidated_before_seal
            && source.verification_status == IrSourceVerificationV2::Verified
    }));
    let counters = execution_counters(&direct.execution);
    assert_eq!(counters.records, direct_ir.cost.selected_records);
    assert_eq!(counters.source_bytes, direct_ir.cost.selected_source_bytes);
    assert_eq!(
        counters.estimated_tokens,
        direct_ir.cost.selected_estimated_tokens
    );
    assert_eq!(counters.work_units, direct_ir.cost.work_units_consumed);
    let mut different_execution_identity = direct_ir.clone();
    different_execution_identity.context_id = "ctx_attempt_local_only".to_string();
    different_execution_identity.task.task_session_id = "task_attempt_local_only".to_string();
    let different_execution_identity = different_execution_identity.seal().unwrap();
    assert_eq!(
        direct_ir.context_hash,
        different_execution_identity.context_hash
    );
    let canonical_document = serde_json::to_string(&direct_ir).unwrap();
    for transient_field in [
        "attempt_id",
        "elapsed_micros",
        "cache",
        "deadline",
        "retry",
        "cancellation",
    ] {
        assert!(!canonical_document.contains(transient_field));
    }
}

#[test]
fn deep_v2_task_kinds_close_available_current_graph_effect_and_validation_roles() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let produced_config_classification: (String, bool) = fixture
        .connection
        .query_row(
            "SELECT artifact_class, is_test FROM current_file
             WHERE generation_id = ?1 AND canonical_path = 'Cargo.toml'",
            [&generation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        produced_config_classification,
        ("configuration".to_string(), false),
        "discovery must produce configuration Truth without fixture mutation"
    );
    let calculate_symbol = install_role_closure_truth(&fixture);

    for task_kind in [
        TaskKind::Explore,
        TaskKind::BugFix,
        TaskKind::BehaviorChange,
        TaskKind::ApiChange,
        TaskKind::Review,
        TaskKind::Audit,
    ] {
        let application_input = deep_input_for_kind(&fixture, task_kind, &calculate_symbol);
        let request = deep_compile_request(&application_input);
        let document = compile_deep_context_ir_v2(
            &mut fixture.connection,
            &fixture.workspace,
            &generation_id,
            &request,
            &budget(),
            None,
        )
        .unwrap()
        .context_ir;
        let role_state = |role| {
            document
                .status
                .role_sufficiency
                .iter()
                .find(|entry| entry.role == role)
                .unwrap()
                .state
        };

        assert_eq!(
            role_state(ItemRole::TestContract),
            IrRequirementStateV2::Satisfied,
            "{task_kind:?} omitted producer-classified test evidence"
        );
        assert_eq!(
            role_state(ItemRole::ValidationTarget),
            IrRequirementStateV2::Satisfied,
            "{task_kind:?} omitted validation closure"
        );
        assert_eq!(document.status.validation, IrRequirementStateV2::Satisfied);
        assert!(document.validation_plan.iter().any(|validation| {
            validation.target_entity_id == "path:tests/calculate.rs" && validation.required
        }));
        assert!(!document.relationships.is_empty());

        if matches!(
            task_kind,
            TaskKind::BehaviorChange | TaskKind::ApiChange | TaskKind::Review
        ) {
            assert_eq!(
                role_state(ItemRole::Effect),
                IrRequirementStateV2::Satisfied,
                "{task_kind:?} omitted qualified effect evidence"
            );
            assert!(document
                .effects
                .iter()
                .any(|effect| effect.effect_type == "writes_state"));
        }
        if matches!(task_kind, TaskKind::BehaviorChange | TaskKind::ApiChange) {
            assert_eq!(
                role_state(ItemRole::DirectDependent),
                IrRequirementStateV2::Satisfied,
                "{task_kind:?} omitted caller closure"
            );
        }
        if task_kind == TaskKind::ApiChange {
            assert_eq!(
                role_state(ItemRole::TypeContract),
                IrRequirementStateV2::Satisfied
            );
        }
        if task_kind == TaskKind::Audit {
            assert_eq!(
                role_state(ItemRole::ConfigurationInput),
                IrRequirementStateV2::Satisfied
            );
        }
    }
}

#[test]
fn deep_v2_excludes_prior_generation_role_evidence_and_reports_absence() {
    let mut fixture = Fixture::new();
    install_role_closure_truth(&fixture);
    fixture.generation_id = fixture.reconcile_again();
    let generation_id = fixture.generation_id.clone();
    let calculate_symbol = insert_primary_symbol_truth(&fixture);
    let application_input =
        deep_input_for_kind(&fixture, TaskKind::BehaviorChange, &calculate_symbol);
    let request = deep_compile_request(&application_input);

    let document = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &budget(),
        None,
    )
    .unwrap()
    .context_ir;

    assert!(document.relationships.is_empty());
    assert!(document.effects.is_empty());
    for role in [
        ItemRole::DirectDependent,
        ItemRole::TestContract,
        ItemRole::Effect,
        ItemRole::ValidationTarget,
    ] {
        let entry = document
            .status
            .role_sufficiency
            .iter()
            .find(|entry| entry.role == role)
            .unwrap();
        assert_ne!(entry.state, IrRequirementStateV2::Satisfied);
        assert!(entry.evidence_item_ids.is_empty());
    }
    assert_eq!(
        document.status.working_set_status,
        WorkingSetStatus::Partial
    );
    assert!(document
        .status
        .reasons
        .contains(&IrOmissionReasonV2::MissingRequiredRole));
}

#[test]
fn deep_v2_keeps_unresolved_test_edges_out_of_validation_closure() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let calculate_symbol = insert_primary_symbol_truth(&fixture);
    insert_unresolved_test_relationship_truth(&fixture, &calculate_symbol);
    let application_input = deep_input_for_kind(&fixture, TaskKind::Explore, &calculate_symbol);
    let request = deep_compile_request(&application_input);

    let document = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &budget(),
        None,
    )
    .unwrap()
    .context_ir;

    let test_role = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::TestContract)
        .unwrap();
    assert_eq!(test_role.state, IrRequirementStateV2::Unresolved);
    assert_eq!(
        test_role.reason,
        Some(IrOmissionReasonV2::UnresolvedEvidence)
    );
    assert!(test_role.evidence_item_ids.is_empty());
    assert!(document.validation_plan.is_empty());
    assert_eq!(
        document.status.validation,
        IrRequirementStateV2::Unavailable
    );
    assert_eq!(
        document.status.working_set_status,
        WorkingSetStatus::Partial
    );
}

#[test]
fn deep_v2_conflicting_effect_truth_prevents_false_completion() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let calculate_symbol = install_role_closure_truth(&fixture);
    fixture
        .connection
        .execute(
            "INSERT INTO evidence_conflict (
                evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
                conflict_type, participating_fact_ids_json, preferred_fact_id,
                projection_policy_version, status, explanation, created_at
             ) VALUES ('rp8-effect-conflict',?1,?2,'effect','rp8-effect',
                       'provider_disagreement','[\"rp8-effect\"]',NULL,
                       'projection-v2.0.0','open','RP8 controlled conflict',
                       '2026-09-03T00:00:00Z')",
            rusqlite::params![fixture.workspace.workspace_id, generation_id],
        )
        .unwrap();
    let application_input =
        deep_input_for_kind(&fixture, TaskKind::BehaviorChange, &calculate_symbol);
    let request = deep_compile_request(&application_input);

    let document = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &budget(),
        None,
    )
    .unwrap()
    .context_ir;
    let effect_role = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::Effect)
        .unwrap();
    assert_eq!(effect_role.state, IrRequirementStateV2::Conflicting);
    assert_eq!(
        effect_role.reason,
        Some(IrOmissionReasonV2::ConflictingEvidence)
    );
    assert_eq!(
        document.status.working_set_status,
        WorkingSetStatus::Partial
    );
    assert!(document
        .status
        .reasons
        .contains(&IrOmissionReasonV2::ConflictingEvidence));
}

#[test]
fn deep_v2_packs_required_validation_before_optional_graph_evidence() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let calculate_symbol = install_role_closure_truth(&fixture);
    let application_input = deep_input_for_kind(&fixture, TaskKind::Explore, &calculate_symbol);
    let request = deep_compile_request(&application_input);
    let record_limited_budget = DeepSemanticBudget::new(5, 65_536, 16_384, 4, 100, 12).unwrap();

    let document = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &record_limited_budget,
        None,
    )
    .unwrap()
    .context_ir;

    assert_eq!(
        document.status.working_set_status,
        WorkingSetStatus::Complete
    );
    assert_eq!(document.status.validation, IrRequirementStateV2::Satisfied);
    assert!(document
        .validation_plan
        .iter()
        .any(|validation| validation.target_entity_id == "path:tests/calculate.rs"));
    assert_eq!(
        document
            .status
            .role_sufficiency
            .iter()
            .find(|entry| entry.role == ItemRole::TestContract)
            .unwrap()
            .state,
        IrRequirementStateV2::Satisfied
    );
}

#[test]
fn deep_v2_work_exhaustion_reports_budget_omitted_role_closure() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let calculate_symbol = install_role_closure_truth(&fixture);
    let application_input =
        deep_input_for_kind(&fixture, TaskKind::BehaviorChange, &calculate_symbol);
    let request = deep_compile_request(&application_input);
    let limited_budget = DeepSemanticBudget::new(50, 65_536, 16_384, 4, 8, 12).unwrap();

    let document = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &limited_budget,
        None,
    )
    .unwrap()
    .context_ir;

    assert!(document.cost.work_units_consumed <= limited_budget.max_work_units);
    assert!(document
        .omissions
        .by_reason
        .contains_key(&IrOmissionReasonV2::WorkUnitBudget));
    for role in [
        ItemRole::DirectDependent,
        ItemRole::TestContract,
        ItemRole::ValidationTarget,
    ] {
        let entry = document
            .status
            .role_sufficiency
            .iter()
            .find(|entry| entry.role == role)
            .unwrap();
        assert_eq!(entry.state, IrRequirementStateV2::BudgetOmitted);
        assert_eq!(entry.reason, Some(IrOmissionReasonV2::WorkUnitBudget));
    }
    assert_eq!(
        document.status.validation,
        IrRequirementStateV2::BudgetOmitted
    );
    assert!(document.validation_plan.is_empty());
    assert_eq!(
        document.status.working_set_status,
        WorkingSetStatus::Partial
    );
}

#[test]
fn catalogue_deep_preserves_unavailable_unresolved_and_conflicting_deficits() {
    let mut unavailable = Fixture::new();
    let unavailable_generation = unavailable.generation_id.clone();
    let unavailable_input = with_existing_session(
        &unavailable,
        &unavailable_generation,
        input("Explore unavailable role evidence", &["src/lib.rs"]),
    );
    let unavailable_capabilities = [
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ValidationPlan,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let unavailable_before = durable_compiler_state(&unavailable.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut unavailable,
        &unavailable_input,
        &unavailable_capabilities,
        budget(),
    );
    let unavailable_deficits = [
        RouteDeficit::Capability {
            capability: RequiredCapability::RequiredRoleClosure,
            reason: CapabilityDeficitReason::Unavailable,
        },
        RouteDeficit::Capability {
            capability: RequiredCapability::ValidationPlan,
            reason: CapabilityDeficitReason::Unavailable,
        },
    ];
    assert_blocked_deep_deficits(&direct, &unavailable_deficits, 1);
    assert_blocked_deep_deficits(&escalated, &unavailable_deficits, 1);
    assert_eq!(
        durable_compiler_state(&unavailable.connection),
        unavailable_before
    );

    let mut unresolved = Fixture::new();
    let unresolved_symbol = insert_primary_symbol_truth(&unresolved);
    insert_unresolved_test_relationship_truth(&unresolved, &unresolved_symbol);
    let mut unresolved_input = input("Explore unresolved relationship evidence", &[]);
    unresolved_input.seed_symbols = vec![unresolved_symbol.clone()];
    let unresolved_input =
        with_existing_session(&unresolved, &unresolved.generation_id, unresolved_input);
    let unresolved_capabilities = [
        (
            RequiredCapability::BoundedRelationships,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let unresolved_before = durable_compiler_state(&unresolved.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut unresolved,
        &unresolved_input,
        &unresolved_capabilities,
        budget(),
    );
    let unresolved_deficits = [
        RouteDeficit::Capability {
            capability: RequiredCapability::BoundedRelationships,
            reason: CapabilityDeficitReason::Ambiguous,
        },
        RouteDeficit::Capability {
            capability: RequiredCapability::RequiredRoleClosure,
            reason: CapabilityDeficitReason::Unavailable,
        },
    ];
    assert_blocked_deep_deficits(&direct, &unresolved_deficits, 1);
    assert_blocked_deep_deficits(&escalated, &unresolved_deficits, 2);
    assert_eq!(
        durable_compiler_state(&unresolved.connection),
        unresolved_before
    );

    let mut conflicting = Fixture::new();
    let conflicting_symbol = install_role_closure_truth(&conflicting);
    insert_relationship_truth(
        &conflicting,
        "rp10-unresolved-impact",
        "src/other.rs",
        "depends_on",
        "path",
        "src/other.rs",
        "path",
        "src/lib.rs",
        "unresolved",
        "",
        "",
    );
    conflicting
        .connection
        .execute(
            "INSERT INTO evidence_conflict (
                evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
                conflict_type, participating_fact_ids_json, preferred_fact_id,
                projection_policy_version, status, explanation, created_at
             ) VALUES ('rp10-effect-conflict',?1,?2,'effect','rp8-effect',
                       'provider_disagreement','[\"rp8-effect\"]',NULL,
                       'projection-v2.0.0','open','RP10 controlled conflict',
                       '2026-09-03T00:00:00Z')",
            rusqlite::params![
                conflicting.workspace.workspace_id,
                conflicting.generation_id
            ],
        )
        .unwrap();
    let conflicting_input =
        deep_input_for_kind(&conflicting, TaskKind::BehaviorChange, &conflicting_symbol);
    let conflicting_capabilities = [(
        RequiredCapability::ImpactFrontier,
        CallerCapabilityState::Unsatisfied,
    )];
    let conflicting_before = durable_compiler_state(&conflicting.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut conflicting,
        &conflicting_input,
        &conflicting_capabilities,
        budget(),
    );
    let conflicting_deficits = [RouteDeficit::Capability {
        capability: RequiredCapability::ImpactFrontier,
        reason: CapabilityDeficitReason::Conflicting,
    }];
    assert_blocked_deep_deficits(&direct, &conflicting_deficits, 1);
    assert_blocked_deep_deficits(&escalated, &conflicting_deficits, 2);
    assert_eq!(
        durable_compiler_state(&conflicting.connection),
        conflicting_before
    );
}

#[test]
fn catalogue_deep_requires_positive_coverage_and_qualified_identity_evidence() {
    let mut no_coverage = Fixture::new();
    no_coverage
        .connection
        .execute(
            "UPDATE index_generation
             SET indexed_file_count = 0, partial_file_count = 0,
                 unsupported_file_count = 0, excluded_file_count = 0,
                 failed_file_count = 0
             WHERE generation_id = ?1",
            [&no_coverage.generation_id],
        )
        .unwrap();
    let no_coverage_generation = no_coverage.generation_id.clone();
    let no_coverage_input = with_existing_session(
        &no_coverage,
        &no_coverage_generation,
        input("Inspect absent coverage evidence", &["src/lib.rs"]),
    );
    let no_coverage_capabilities = [(
        RequiredCapability::BasicCoverageConflicts,
        CallerCapabilityState::Unsatisfied,
    )];
    let mut no_coverage_request = request(
        &no_coverage_input,
        &no_coverage_capabilities,
        Route::AtlasDeep,
        Route::AtlasDeep,
        &no_coverage.generation_id,
    );
    no_coverage_request.deep_budget = Some(budget());
    let no_coverage_output = dispatch_catalogue_context_application(
        no_coverage_request,
        no_coverage_input,
        &mut no_coverage.connection,
        &no_coverage.workspace,
    )
    .unwrap();
    assert_blocked_deep_deficits(
        &no_coverage_output,
        &[RouteDeficit::Capability {
            capability: RequiredCapability::BasicCoverageConflicts,
            reason: CapabilityDeficitReason::Unavailable,
        }],
        1,
    );

    let mut unsupported = Fixture::new();
    let unsupported_symbol = insert_primary_symbol_truth(&unsupported);
    unsupported
        .connection
        .execute(
            "UPDATE symbol_fact
             SET evidence_method = 'inferred', confidence = 0.1
             WHERE canonical_symbol_key = ?1",
            [&unsupported_symbol],
        )
        .unwrap();
    let unsupported_input =
        deep_input_for_kind(&unsupported, TaskKind::Explore, &unsupported_symbol);
    let unsupported_capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let unsupported_before = durable_compiler_state(&unsupported.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut unsupported,
        &unsupported_input,
        &unsupported_capabilities,
        budget(),
    );
    let unsupported_deficits = [
        RouteDeficit::Capability {
            capability: RequiredCapability::IdentityLookup,
            reason: CapabilityDeficitReason::Unsupported,
        },
        RouteDeficit::Capability {
            capability: RequiredCapability::RequiredRoleClosure,
            reason: CapabilityDeficitReason::Unsupported,
        },
    ];
    assert_blocked_deep_deficits(&direct, &unsupported_deficits, 1);
    assert_blocked_deep_deficits(&escalated, &unsupported_deficits, 2);
    assert_eq!(
        durable_compiler_state(&unsupported.connection),
        unsupported_before
    );
}

#[test]
fn catalogue_deep_preserves_partial_source_and_semantic_budget_deficits() {
    let mut stale_source = Fixture::new();
    let stale_generation = stale_source.generation_id.clone();
    let stale_input = with_existing_session(
        &stale_source,
        &stale_generation,
        input(
            "Explore partially stale selected source",
            &["src/lib.rs", "src/other.rs"],
        ),
    );
    std::fs::write(
        std::path::Path::new(&stale_source.workspace.canonical_root).join("src/other.rs"),
        "pub fn other(value: i64) -> i64 { value - 2 }\n",
    )
    .unwrap();
    let stale_capabilities = [(
        RequiredCapability::RequiredRoleClosure,
        CallerCapabilityState::Unsatisfied,
    )];
    let stale_before = durable_compiler_state(&stale_source.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut stale_source,
        &stale_input,
        &stale_capabilities,
        budget(),
    );
    let stale_deficits = [RouteDeficit::Capability {
        capability: RequiredCapability::RequiredRoleClosure,
        reason: CapabilityDeficitReason::Stale,
    }];
    assert_blocked_deep_deficits(&direct, &stale_deficits, 1);
    assert_blocked_deep_deficits(&escalated, &stale_deficits, 1);
    assert_eq!(
        durable_compiler_state(&stale_source.connection),
        stale_before
    );

    for (name, limited_budget) in [
        (
            "record",
            DeepSemanticBudget::new(2, 65_536, 16_384, 4, 10_000, 12).unwrap(),
        ),
        (
            "work",
            DeepSemanticBudget::new(50, 65_536, 16_384, 4, 8, 12).unwrap(),
        ),
        (
            "source",
            DeepSemanticBudget::new(50, 1, 16_384, 4, 10_000, 12).unwrap(),
        ),
    ] {
        let mut fixture = Fixture::new();
        let symbol = install_role_closure_truth(&fixture);
        let application_input = deep_input_for_kind(&fixture, TaskKind::BehaviorChange, &symbol);
        let capabilities = [(
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        )];
        let durable_before = durable_compiler_state(&fixture.connection);
        let (direct, escalated) = dispatch_direct_and_escalated_deep(
            &mut fixture,
            &application_input,
            &capabilities,
            limited_budget,
        );
        let deficits = [RouteDeficit::Capability {
            capability: RequiredCapability::RequiredRoleClosure,
            reason: CapabilityDeficitReason::SemanticBudgetOmitted,
        }];
        assert_blocked_deep_deficits(&direct, &deficits, 1);
        assert_blocked_deep_deficits(&escalated, &deficits, 1);
        assert_eq!(
            durable_compiler_state(&fixture.connection),
            durable_before,
            "{name} budget cut mutated durable compiler state"
        );
    }

    let mut exact_source = Fixture::new();
    let exact_symbol = install_role_closure_truth(&exact_source);
    let exact_input = deep_input_for_kind(&exact_source, TaskKind::BehaviorChange, &exact_symbol);
    let exact_capabilities = [
        (
            RequiredCapability::ExactSource,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut exact_source,
        &exact_input,
        &exact_capabilities,
        DeepSemanticBudget::new(50, 1, 16_384, 4, 10_000, 12).unwrap(),
    );
    let deficits = [
        RouteDeficit::Capability {
            capability: RequiredCapability::ExactSource,
            reason: CapabilityDeficitReason::SemanticBudgetOmitted,
        },
        RouteDeficit::Capability {
            capability: RequiredCapability::RequiredRoleClosure,
            reason: CapabilityDeficitReason::SemanticBudgetOmitted,
        },
    ];
    assert_blocked_deep_deficits(&direct, &deficits, 1);
    assert_blocked_deep_deficits(&escalated, &deficits, 2);
}

#[test]
fn catalogue_deep_keeps_caller_satisfaction_without_overclaiming_other_capabilities() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let application_input = with_existing_session(
        &fixture,
        &generation_id,
        input("Explore with caller-satisfied identity", &["src/lib.rs"]),
    );
    let capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Satisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let durable_before = durable_compiler_state(&fixture.connection);
    let (direct, escalated) = dispatch_direct_and_escalated_deep(
        &mut fixture,
        &application_input,
        &capabilities,
        budget(),
    );
    let expected_deficits = [RouteDeficit::Capability {
        capability: RequiredCapability::RequiredRoleClosure,
        reason: CapabilityDeficitReason::Unavailable,
    }];
    for output in [&direct, &escalated] {
        match &output.execution {
            ContextExecution::Partial {
                final_route: Route::AtlasDeep,
                satisfied_capabilities,
                deficits,
                counters,
                ..
            } => {
                assert_eq!(
                    satisfied_capabilities,
                    &[RequiredCapability::IdentityLookup]
                );
                assert_eq!(deficits, &expected_deficits);
                assert_eq!(counters.atlas_calls, 1);
            }
            other => panic!("expected useful partial DEEP result, got {other:#?}"),
        }
        output.execution.validate().unwrap();
        assert!(output.deep_context_ir.is_some());
    }
    assert_eq!(direct.execution.payload(), escalated.execution.payload());
    assert_eq!(
        direct.deep_context_ir.as_ref().unwrap().context_hash,
        escalated.deep_context_ir.as_ref().unwrap().context_hash
    );
    assert_eq!(durable_compiler_state(&fixture.connection), durable_before);
}

#[test]
fn mid_verification_failure_keeps_v2_results_transient() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let application_input = with_existing_session(
        &fixture,
        &generation_id,
        input(
            "Explore multiple source verifications",
            &["src/lib.rs", "src/other.rs"],
        ),
    );
    std::fs::write(
        std::path::Path::new(&fixture.workspace.canonical_root).join("src/other.rs"),
        "pub fn other(value: i64) -> i64 { value - 2 }\n",
    )
    .unwrap();
    let durable_before = durable_compiler_state(&fixture.connection);

    let result = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[],
            Route::AtlasDeep,
            Route::AtlasDeep,
            &generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();

    assert_eq!(
        durable_compiler_state(&fixture.connection),
        durable_before,
        "a later source verification failure must not leak earlier observations"
    );
    let context_ir = result.deep_context_ir.unwrap();
    let source_statuses = context_ir
        .working_set
        .iter()
        .filter_map(|item| {
            item.source
                .as_ref()
                .map(|source| (source.canonical_path.as_str(), source.verification_status))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        source_statuses.get("src/lib.rs"),
        Some(&IrSourceVerificationV2::Verified)
    );
    assert_eq!(
        source_statuses.get("src/other.rs"),
        Some(&IrSourceVerificationV2::Stale)
    );
    assert_eq!(
        context_ir.status.working_set_status,
        WorkingSetStatus::Partial
    );
    let counters = execution_counters(&result.execution);
    assert_eq!(counters.records, context_ir.cost.selected_records);
    assert_eq!(counters.source_bytes, context_ir.cost.selected_source_bytes);
    assert_eq!(
        counters.estimated_tokens,
        context_ir.cost.selected_estimated_tokens
    );
    assert_eq!(counters.work_units, context_ir.cost.work_units_consumed);
}

#[test]
fn post_verification_seal_failure_keeps_v2_results_transient() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let application_input = with_existing_session(
        &fixture,
        &generation_id,
        input(
            "Explore multiple source verifications before sealing",
            &["src/lib.rs", "src/other.rs"],
        ),
    );
    let successful = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[],
            Route::AtlasDeep,
            Route::AtlasDeep,
            &generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    let mut document = successful.deep_context_ir.unwrap();
    document.task.task_hash = "invalid-after-source-verification".to_string();
    std::fs::remove_file(
        std::path::Path::new(&fixture.workspace.canonical_root).join("src/other.rs"),
    )
    .unwrap();
    let durable_before = durable_compiler_state(&fixture.connection);

    let error = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        document,
        None,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("deep Context IR contains an invalid workspace digest"),
        "{error}"
    );
    assert_eq!(
        durable_compiler_state(&fixture.connection),
        durable_before,
        "a final seal failure after live verification must not leak observations"
    );
}

#[test]
fn direct_concrete_dispatch_observes_only_and_creates_no_context_identity() {
    let mut fixture = Fixture::new();
    let application_input = input(
        "Explore caller-owned context without retaining raw secret",
        &["src/private.rs"],
    );
    let before = table_counts(&fixture.connection);
    let mut direct_request = request(
        &application_input,
        &[(
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Satisfied,
        )],
        Route::Direct,
        Route::AtlasDeep,
        &fixture.generation_id,
    );
    direct_request.starting_generation = GenerationObservation::Unavailable {
        reason: workspace_atlas::context_route::GenerationUnavailableReason::ObservationFailed,
    };
    let output = dispatch_catalogue_context_application(
        direct_request,
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();

    assert!(output.deep_context_ir.is_none());
    assert_eq!(
        output.execution.payload(),
        Some(&ContextPayload::DirectNone {})
    );
    match &output.execution {
        ContextExecution::Completed { decision, .. } => assert_eq!(
            decision.starting_generation,
            GenerationObservation::Observed {
                generation_id: fixture.generation_id.clone()
            }
        ),
        other => panic!("expected DIRECT completion, got {other:#?}"),
    }
    assert_eq!(table_counts(&fixture.connection), before);
    let serialized = serde_json::to_string(&output.execution).unwrap();
    assert!(!serialized.contains("raw secret"));
    assert!(!serialized.contains("context_id"));
}

#[test]
fn concrete_dispatch_blocks_stale_generation_then_compiles_on_resubmission() {
    let mut fixture = Fixture::new();
    let original_generation = fixture.generation_id.clone();
    let application_input = input(
        "Explore calculate after generation change",
        &["src/lib.rs", "tests/calculate.rs"],
    );
    let capabilities = [(
        RequiredCapability::RequiredRoleClosure,
        CallerCapabilityState::Unsatisfied,
    )];
    let stale_request = request(
        &application_input,
        &capabilities,
        Route::AtlasDeep,
        Route::AtlasDeep,
        &original_generation,
    );
    let new_generation = fixture.reconcile_again();

    let blocked = dispatch_catalogue_context_application(
        stale_request,
        application_input.clone(),
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    match &blocked.execution {
        ContextExecution::Blocked {
            final_route: Some(Route::AtlasDeep),
            payload: None,
            deficits,
            counters,
            terminal_reason: workspace_atlas::context_route::TerminalReason::GenerationChanged,
            ..
        } => {
            assert_eq!(
                deficits,
                &[RouteDeficit::Route {
                    reason: workspace_atlas::context_route::RouteFailureReason::GenerationChanged,
                }]
            );
            assert_eq!(counters, &ContextExecutionCounters::default());
        }
        other => panic!("expected stale-generation DEEP block, got {other:#?}"),
    }
    assert!(blocked.deep_context_ir.is_none());
    assert_eq!(table_counts(&fixture.connection).0, 0);

    let resubmitted_input = with_existing_session(&fixture, &new_generation, application_input);
    let completed = dispatch_catalogue_context_application(
        request(
            &resubmitted_input,
            &capabilities,
            Route::AtlasDeep,
            Route::AtlasDeep,
            &new_generation,
        ),
        resubmitted_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(
        matches!(
            &completed.execution,
            ContextExecution::Completed {
                final_route: Route::AtlasDeep,
                ..
            }
        ),
        "{:#?}",
        completed.execution
    );
    assert_eq!(
        completed
            .deep_context_ir
            .as_ref()
            .unwrap()
            .workspace
            .generation_id,
        new_generation
    );
}

#[test]
fn real_light_ceiling_is_partial_with_evidence_and_blocked_without_it() {
    let mut fixture = Fixture::new();
    let capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ValidationPlan,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let useful_input = input("Explore a bounded known target", &["src/lib.rs"]);
    let useful = dispatch_catalogue_context_application(
        request(
            &useful_input,
            &capabilities,
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        useful_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        useful.execution,
        ContextExecution::Partial {
            final_route: Route::AtlasLight,
            ..
        }
    ));
    assert!(useful.deep_context_ir.is_none());

    let missing_input = input("Explore a bounded missing target", &["src/missing.rs"]);
    let missing = dispatch_catalogue_context_application(
        request(
            &missing_input,
            &capabilities,
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        missing_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    match &missing.execution {
        ContextExecution::Blocked {
            deficits, counters, ..
        } => {
            assert!(deficits.contains(&RouteDeficit::Capability {
                capability: RequiredCapability::IdentityLookup,
                reason: CapabilityDeficitReason::Unsupported,
            }));
            assert_eq!(counters.atlas_calls, 1);
            assert_eq!(counters.records, 0);
            assert_eq!(counters.work_units, 1);
        }
        other => panic!("expected missing target to block, got {other:#?}"),
    }
    assert!(missing.deep_context_ir.is_none());
}

#[test]
fn real_light_deduplicates_explicit_targets_and_ignores_tenfold_unrelated_growth() {
    fn run(fixture: &mut Fixture) -> ContextExecution {
        let application_input = input(
            "Explore only explicit bounded targets",
            &["src/lib.rs", "src/lib.rs"],
        );
        let generation_id = fixture.generation_id.clone();
        dispatch_catalogue_context_application(
            request(
                &application_input,
                &[
                    (
                        RequiredCapability::IdentityLookup,
                        CallerCapabilityState::Unsatisfied,
                    ),
                    (
                        RequiredCapability::ExactSource,
                        CallerCapabilityState::Unsatisfied,
                    ),
                ],
                Route::AtlasLight,
                Route::AtlasLight,
                &generation_id,
            ),
            application_input,
            &mut fixture.connection,
            &fixture.workspace,
        )
        .unwrap()
        .execution
    }

    let mut small = Fixture::new();
    let mut grown = Fixture::with_unrelated_files(27);
    let small_execution = run(&mut small);
    let grown_execution = run(&mut grown);

    for execution in [&small_execution, &grown_execution] {
        let counters = execution_counters(execution);
        assert_eq!(counters.atlas_calls, 2);
        assert_eq!(counters.records, 2);
        assert_eq!(counters.source_bytes, 0);
        assert_eq!(counters.estimated_tokens, 0);
        assert_eq!(counters.work_units, 2);
        let operations = match execution.payload() {
            Some(ContextPayload::LightBundle { operations }) => operations,
            other => panic!("expected useful LIGHT payload, got {other:#?}"),
        };
        assert_eq!(operations.len(), 2);
        assert!(matches!(operations[0], LightOperationResult::Query { .. }));
        assert!(matches!(
            operations[1],
            LightOperationResult::SourceReference { .. }
        ));
        assert!(!serde_json::to_string(execution)
            .unwrap()
            .contains("context_packet"));
    }
    assert_eq!(
        serde_json::to_value(small_execution.payload()).unwrap(),
        serde_json::to_value(grown_execution.payload()).unwrap()
    );
}

#[test]
fn identity_only_light_does_not_read_unrequested_evidence_tables() {
    let mut fixture = Fixture::new();
    fixture
        .connection
        .execute_batch(
            "DROP TABLE relationship_resolution;
             DROP TABLE evidence_conflict;
             DROP TABLE coverage_record;
             DROP TABLE relationship_fact;",
        )
        .unwrap();
    let application_input = input("Resolve one explicit identity", &["src/lib.rs"]);
    let generation_id = fixture.generation_id.clone();
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[(
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        output.execution,
        ContextExecution::Completed {
            final_route: Route::AtlasLight,
            ..
        }
    ));
    let target = &output
        .light_results
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()[0];
    assert!(target.relationships.is_none());
    assert!(target.impact_targets.is_none());
    assert!(target.coverage.is_none());
    assert!(target.conflicts.is_none());
}

#[test]
fn light_query_digest_and_delivery_change_with_same_cardinality_evidence() {
    fn run(
        fixture: &mut Fixture,
    ) -> workspace_atlas::context_application::CatalogueContextApplicationOutput {
        let application_input = input("Inspect one direct relationship", &["src/lib.rs"]);
        let generation_id = fixture.generation_id.clone();
        dispatch_catalogue_context_application(
            request(
                &application_input,
                &[(
                    RequiredCapability::BoundedRelationships,
                    CallerCapabilityState::Unsatisfied,
                )],
                Route::AtlasLight,
                Route::AtlasLight,
                &generation_id,
            ),
            application_input,
            &mut fixture.connection,
            &fixture.workspace,
        )
        .unwrap()
    }

    let mut fixture = Fixture::new();
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                evidence_method, confidence, evidence_reason
             )
             SELECT 'light-relationship', er.extractor_run_id, cf.revision_id, 'depends_on',
                    'path', 'src/lib.rs', 'path', 'src/other.rs',
                    'structural', 1.0, 'controlled LIGHT relationship'
             FROM current_file cf
             JOIN extractor_run er ON er.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = 'src/lib.rs'
             ORDER BY er.extractor_run_id LIMIT 1",
            [&fixture.generation_id],
        )
        .unwrap();
    let before = run(&mut fixture);
    fixture
        .connection
        .execute(
            "UPDATE relationship_fact
             SET target_ref_value = 'tests/calculate.rs'
             WHERE relationship_fact_id = 'light-relationship'",
            [],
        )
        .unwrap();
    let after = run(&mut fixture);
    let digest = |execution: &ContextExecution| match execution.payload().unwrap() {
        ContextPayload::LightBundle { operations } => match &operations[0] {
            LightOperationResult::Query { result_digest, .. } => result_digest.clone(),
            other => panic!("expected query result, got {other:#?}"),
        },
        other => panic!("expected LIGHT payload, got {other:#?}"),
    };
    assert_ne!(digest(&before.execution), digest(&after.execution));
    for output in [&before, &after] {
        let counters = execution_counters(&output.execution);
        assert_eq!(counters.atlas_calls, 1);
        assert_eq!(counters.records, 2);
        assert_eq!(counters.work_units, 1);
    }
    let relationship_target =
        |output: workspace_atlas::context_application::CatalogueContextApplicationOutput| {
            output.light_results.unwrap().query.unwrap()[0]
                .relationships
                .as_ref()
                .unwrap()[0]
                .target_ref_value
                .clone()
        };
    assert_eq!(relationship_target(before), "src/other.rs");
    assert_eq!(relationship_target(after), "tests/calculate.rs");
}

#[test]
fn real_light_keeps_mixed_verified_and_missing_sources_incomplete() {
    let mut fixture = Fixture::new();
    let application_input = input(
        "Verify every explicit source target",
        &["src/lib.rs", "src/missing.rs"],
    );
    let generation_id = fixture.generation_id.clone();
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[(
                RequiredCapability::ExactSource,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    match output.execution {
        ContextExecution::Blocked {
            deficits, counters, ..
        } => {
            assert_eq!(
                deficits,
                vec![RouteDeficit::Capability {
                    capability: RequiredCapability::ExactSource,
                    reason: CapabilityDeficitReason::Unsupported,
                }]
            );
            assert_eq!(counters.atlas_calls, 1);
            assert_eq!(counters.records, 2);
            assert_eq!(counters.work_units, 2);
        }
        other => panic!("mixed verified/missing sources must remain incomplete: {other:#?}"),
    }
}

#[test]
fn real_light_without_any_explicit_target_is_unsupported_without_work() {
    let mut fixture = Fixture::new();
    let application_input = input("Explore without an explicit target", &[]);
    let generation_id = fixture.generation_id.clone();
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[(
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    match output.execution {
        ContextExecution::Blocked {
            deficits, counters, ..
        } => {
            assert_eq!(
                deficits,
                vec![RouteDeficit::Capability {
                    capability: RequiredCapability::IdentityLookup,
                    reason: CapabilityDeficitReason::Unsupported,
                }]
            );
            assert_eq!(counters, ContextExecutionCounters::default());
        }
        other => panic!("expected targetless LIGHT to block, got {other:#?}"),
    }
}

#[test]
fn real_light_requires_truthful_relationship_impact_and_coverage_evidence() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let capabilities = [
        (
            RequiredCapability::BoundedRelationships,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ImpactFrontier,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::BasicCoverageConflicts,
            CallerCapabilityState::Unsatisfied,
        ),
    ];
    let absent_input = input("Inspect bounded evidence for one target", &["src/lib.rs"]);
    let absent = dispatch_catalogue_context_application(
        request(
            &absent_input,
            &capabilities,
            Route::AtlasLight,
            Route::AtlasLight,
            &generation_id,
        ),
        absent_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    let absent_deficits = match absent.execution {
        ContextExecution::Blocked { deficits, .. } => deficits,
        other => panic!("absent evidence must not complete LIGHT: {other:#?}"),
    };
    for capability in [
        RequiredCapability::BoundedRelationships,
        RequiredCapability::ImpactFrontier,
        RequiredCapability::BasicCoverageConflicts,
    ] {
        assert!(absent_deficits.contains(&RouteDeficit::Capability {
            capability,
            reason: CapabilityDeficitReason::Unavailable,
        }));
    }

    let file_id: String = fixture
        .connection
        .query_row(
            "SELECT fr.file_id FROM current_file cf
             JOIN file_revision fr ON fr.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = 'src/lib.rs'",
            [&generation_id],
            |row| row.get(0),
        )
        .unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO coverage_record (
                coverage_id, generation_id, file_id, scope_kind, scope_key, status
             ) VALUES ('light-coverage-partial', ?1, ?2, 'file', 'src/lib.rs', 'partial')",
            rusqlite::params![generation_id, file_id],
        )
        .unwrap();
    let limited_input = input("Inspect limited target coverage", &["src/lib.rs"]);
    let limited = dispatch_catalogue_context_application(
        request(
            &limited_input,
            &[(
                RequiredCapability::BasicCoverageConflicts,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        limited_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        limited.execution,
        ContextExecution::Blocked { ref deficits, .. }
            if deficits == &vec![RouteDeficit::Capability {
                capability: RequiredCapability::BasicCoverageConflicts,
                reason: CapabilityDeficitReason::Unavailable,
            }]
    ));

    fixture
        .connection
        .execute(
            "INSERT INTO evidence_conflict (
                evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
                conflict_type, participating_fact_ids_json, preferred_fact_id,
                projection_policy_version, status, explanation, created_at
             ) VALUES (
                'light-coverage-conflict', ?1, ?2, 'coverage', 'src/lib.rs',
                'coverage_disagreement', '[]', NULL, 'projection-v2.0.0',
                'open', 'controlled LIGHT conflict', '2026-09-03T00:00:00Z'
             )",
            rusqlite::params![fixture.workspace.workspace_id, fixture.generation_id],
        )
        .unwrap();
    let conflict_input = input("Inspect conflicting target coverage", &["src/lib.rs"]);
    let conflict = dispatch_catalogue_context_application(
        request(
            &conflict_input,
            &[(
                RequiredCapability::BasicCoverageConflicts,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        conflict_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        conflict.execution,
        ContextExecution::Blocked { ref deficits, .. }
            if deficits == &vec![RouteDeficit::Capability {
                capability: RequiredCapability::BasicCoverageConflicts,
                reason: CapabilityDeficitReason::Conflicting,
            }]
    ));
}

#[test]
fn light_truncation_is_semantic_budget_omitted_and_counts_returned_records() {
    let mut relationships = Fixture::new();
    for index in 0..3 {
        relationships
            .connection
            .execute(
                "INSERT INTO relationship_fact (
                    relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                    source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                    evidence_method, confidence, evidence_reason
                 )
                 SELECT ?1, er.extractor_run_id, cf.revision_id, 'depends_on',
                        'path', 'src/lib.rs', 'path', ?2,
                        'structural', 1.0, 'controlled LIGHT truncation'
                 FROM current_file cf
                 JOIN extractor_run er ON er.revision_id = cf.revision_id
                 WHERE cf.generation_id = ?3 AND cf.canonical_path = 'src/lib.rs'
                 ORDER BY er.extractor_run_id LIMIT 1",
                rusqlite::params![
                    format!("light-truncated-relationship-{index}"),
                    format!("src/target_{index}.rs"),
                    relationships.generation_id,
                ],
            )
            .unwrap();
    }
    let relationship_input = input("Inspect every direct relationship", &["src/lib.rs"]);
    let relationship_output = dispatch_catalogue_context_application(
        request(
            &relationship_input,
            &[(
                RequiredCapability::BoundedRelationships,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &relationships.generation_id,
        ),
        relationship_input,
        &mut relationships.connection,
        &relationships.workspace,
    )
    .unwrap();
    assert!(matches!(
        relationship_output.execution,
        ContextExecution::Blocked {
            ref deficits,
            counters: ContextExecutionCounters {
                atlas_calls: 1,
                records: 3,
                work_units: 1,
                ..
            },
            ..
        } if deficits == &vec![RouteDeficit::Capability {
            capability: RequiredCapability::BoundedRelationships,
            reason: CapabilityDeficitReason::SemanticBudgetOmitted,
        }]
    ));

    let mut conflicts = Fixture::new();
    let file_id: String = conflicts
        .connection
        .query_row(
            "SELECT file_id FROM current_file
             WHERE generation_id = ?1 AND canonical_path = 'src/lib.rs'",
            [&conflicts.generation_id],
            |row| row.get(0),
        )
        .unwrap();
    conflicts
        .connection
        .execute(
            "INSERT INTO coverage_record (
                coverage_id, generation_id, file_id, scope_kind, scope_key, status
             ) VALUES ('light-complete-coverage', ?1, ?2, 'file', 'src/lib.rs', 'complete')",
            rusqlite::params![conflicts.generation_id, file_id],
        )
        .unwrap();
    for index in 0..3 {
        conflicts
            .connection
            .execute(
                "INSERT INTO evidence_conflict (
                    evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
                    conflict_type, participating_fact_ids_json, preferred_fact_id,
                    projection_policy_version, status, explanation, created_at
                 ) VALUES (
                    ?1, ?2, ?3, 'coverage', 'src/lib.rs', ?4, '[]', NULL,
                    'projection-v2.0.0', 'open', 'controlled conflict truncation',
                    '2026-09-03T00:00:00Z'
                 )",
                rusqlite::params![
                    format!("light-truncated-conflict-{index}"),
                    conflicts.workspace.workspace_id,
                    conflicts.generation_id,
                    format!("coverage_disagreement_{index}"),
                ],
            )
            .unwrap();
    }
    let coverage_input = input("Inspect every target coverage conflict", &["src/lib.rs"]);
    let coverage_output = dispatch_catalogue_context_application(
        request(
            &coverage_input,
            &[(
                RequiredCapability::BasicCoverageConflicts,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &conflicts.generation_id,
        ),
        coverage_input,
        &mut conflicts.connection,
        &conflicts.workspace,
    )
    .unwrap();
    assert!(matches!(
        coverage_output.execution,
        ContextExecution::Blocked {
            ref deficits,
            counters: ContextExecutionCounters {
                atlas_calls: 1,
                records: 4,
                work_units: 1,
                ..
            },
            ..
        } if deficits == &vec![RouteDeficit::Capability {
            capability: RequiredCapability::BasicCoverageConflicts,
            reason: CapabilityDeficitReason::SemanticBudgetOmitted,
        }]
    ));
}

#[test]
fn light_impact_requires_qualified_resolution_targets() {
    let mut fixture = Fixture::new();
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                evidence_method, confidence, evidence_reason
             )
             SELECT 'light-impact-relationship', er.extractor_run_id, cf.revision_id,
                    'depends_on', 'path', 'src/other.rs', 'path', 'src/lib.rs',
                    'structural', 1.0, 'controlled LIGHT impact'
             FROM current_file cf
             JOIN extractor_run er ON er.revision_id = cf.revision_id
             WHERE cf.generation_id = ?1 AND cf.canonical_path = 'src/other.rs'
             ORDER BY er.extractor_run_id LIMIT 1",
            [&fixture.generation_id],
        )
        .unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO relationship_resolution (
                relationship_resolution_id, workspace_id, generation_id,
                relationship_fact_id, resolver_policy_version, status,
                resolved_ref_kind, resolved_ref_value, reason_code, confidence, created_at
             ) VALUES (
                'light-impact-resolution', ?1, ?2, 'light-impact-relationship',
                'resolution-v1.0.0', 'resolved_file', NULL, NULL,
                'controlled_missing_target', 1.0, '2026-09-03T00:00:00Z'
             )",
            rusqlite::params![fixture.workspace.workspace_id, fixture.generation_id],
        )
        .unwrap();
    let application_input = input("Inspect a qualified impact frontier", &["src/lib.rs"]);
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[(
                RequiredCapability::ImpactFrontier,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        output.execution,
        ContextExecution::Blocked { ref deficits, .. }
            if deficits == &vec![RouteDeficit::Capability {
                capability: RequiredCapability::ImpactFrontier,
                reason: CapabilityDeficitReason::Conflicting,
            }]
    ));
}

#[test]
fn real_light_rejects_ambiguous_symbol_identity() {
    let mut fixture = Fixture::new();
    for (id, start) in [("light-ambiguous-1", 0_i64), ("light-ambiguous-2", 1_i64)] {
        fixture
            .connection
            .execute(
                "INSERT INTO symbol_fact (
                    symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
                    symbol_kind, display_name, qualified_name, start_byte, end_byte,
                    start_line, start_column, end_line, end_column, evidence_method,
                    confidence, evidence_reason
                 )
                 SELECT ?1, extractor_run_id, revision_id, 'light::ambiguous', 'function',
                        'ambiguous', 'light::ambiguous', ?2, ?2, 1, ?2 + 1, 1, ?2 + 1,
                        'structural', 1.0, 'controlled LIGHT ambiguity'
                 FROM extractor_run ORDER BY extractor_run_id LIMIT 1",
                rusqlite::params![id, start],
            )
            .unwrap();
    }
    let mut application_input = input("Resolve every exact identity", &["src/lib.rs"]);
    application_input.seed_symbols = vec!["light::ambiguous".to_string()];
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[(
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            )],
            Route::AtlasLight,
            Route::AtlasLight,
            &fixture.generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        output.execution,
        ContextExecution::Blocked { ref deficits, .. }
            if deficits == &vec![RouteDeficit::Capability {
                capability: RequiredCapability::IdentityLookup,
                reason: CapabilityDeficitReason::Ambiguous,
            }]
    ));
}

#[test]
fn unattributed_deep_uses_response_lifetime_session_without_lifecycle_writes() {
    let mut fixture = Fixture::new();
    let calculate_symbol = install_role_closure_truth(&fixture);
    let mut application_input = input(
        "Explore one target without an existing lifecycle session",
        &["src/lib.rs"],
    );
    application_input.seed_symbols = vec![calculate_symbol];
    let before = table_counts(&fixture.connection);
    let output = dispatch_catalogue_context_application(
        request(
            &application_input,
            &[
                (
                    RequiredCapability::IdentityLookup,
                    CallerCapabilityState::Unsatisfied,
                ),
                (
                    RequiredCapability::RequiredRoleClosure,
                    CallerCapabilityState::Unsatisfied,
                ),
                (
                    RequiredCapability::ValidationPlan,
                    CallerCapabilityState::Unsatisfied,
                ),
            ],
            Route::AtlasDeep,
            Route::AtlasDeep,
            &fixture.generation_id,
        ),
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(matches!(
        output.execution,
        ContextExecution::Completed {
            final_route: Route::AtlasDeep,
            ..
        }
    ));
    assert_eq!(output.deep_context_ir.unwrap().schema_version, "2.0.0");
    assert_eq!(table_counts(&fixture.connection), before);
}

#[test]
fn deep_target_enumeration_is_rejected_before_exceeding_explicit_work_bounds() {
    let mut fixture = Fixture::new();
    let application_input = input(
        "Explore two targets under a one-record budget",
        &["src/lib.rs", "tests/calculate.rs"],
    );
    let mut route_request = request(
        &application_input,
        &[(
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        )],
        Route::AtlasDeep,
        Route::AtlasDeep,
        &fixture.generation_id,
    );
    route_request.deep_budget = Some(DeepSemanticBudget::new(1, 1024, 1024, 1, 1, 100).unwrap());

    let error = dispatch_catalogue_context_application(
        route_request,
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap_err();
    assert!(error.to_string().contains("explicit semantic work bounds"));
    assert_eq!(table_counts(&fixture.connection), (0, 0, 0));
}

#[test]
fn deep_charges_seed_examination_and_packing_under_a_smaller_work_budget() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let application_input = with_existing_session(
        &fixture,
        &generation_id,
        input(
            "Explore one target under a two-unit work budget",
            &["src/lib.rs"],
        ),
    );
    let mut route_request = request(
        &application_input,
        &[],
        Route::AtlasDeep,
        Route::AtlasDeep,
        &generation_id,
    );
    route_request.deep_budget =
        Some(DeepSemanticBudget::new(10, 65_536, 16_384, 4, 2, 10).unwrap());

    let result = dispatch_catalogue_context_application(
        route_request,
        application_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert!(result.deep_context_ir.is_some(), "{:#?}", result.execution);
    let context_ir = result.deep_context_ir.unwrap();

    assert_eq!(context_ir.cost.work_units_consumed, 2);
    assert!(
        context_ir.cost.work_units_consumed <= context_ir.policy.semantic_budget.max_work_units
    );
    assert_eq!(
        context_ir.omissions.candidates_considered,
        context_ir.omissions.selected + context_ir.omissions.omitted
    );
    assert_eq!(
        execution_counters(&result.execution).work_units,
        context_ir.cost.work_units_consumed
    );
}

#[test]
fn paid_seed_sentinel_can_leave_an_empty_temporal_working_set() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let mut application_input = input(
        "Review one target under a one-unit work budget",
        &["src/lib.rs"],
    );
    application_input.declared_task_kind = Some(TaskKind::Review);
    let application_input = with_existing_session(&fixture, &generation_id, application_input);
    let request = deep_compile_request(&application_input);

    let sealed = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(10, 65_536, 16_384, 4, 1, 10).unwrap(),
        None,
    )
    .unwrap();
    let context_ir = sealed.context_ir;

    assert!(context_ir.working_set.is_empty());
    assert_eq!(context_ir.cost.selected_records, 0);
    assert_eq!(context_ir.cost.work_units_consumed, 1);
    assert_eq!(context_ir.omissions.candidates_considered, 1);
    assert_eq!(context_ir.omissions.selected, 0);
    assert_eq!(context_ir.omissions.omitted, 1);
    assert_eq!(
        context_ir
            .omissions
            .by_reason
            .get(&workspace_atlas::context_ir::IrOmissionReasonV2::WorkUnitBudget),
        Some(&1)
    );
}

#[test]
fn baseline_only_history_preserves_paid_generation_metadata_work() {
    let mut fixture = Fixture::new();
    let generation_id = fixture.generation_id.clone();
    let mut application_input = input(
        "Review a baseline generation with no retained parent",
        &["src/lib.rs"],
    );
    application_input.declared_task_kind = Some(TaskKind::Review);
    let application_input = with_existing_session(&fixture, &generation_id, application_input);
    let request = deep_compile_request(&application_input);

    let context_ir = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(50, 65_536, 16_384, 4, 50, 10).unwrap(),
        None,
    )
    .unwrap()
    .context_ir;

    assert_eq!(context_ir.cost.work_units_consumed, 4);
    assert_eq!(
        context_ir.temporal_constraints.state,
        workspace_atlas::context_ir::IrTemporalStateV2::Unavailable
    );
    assert_eq!(
        context_ir.temporal_constraints.omission_reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::TemporalEvidenceUnavailable)
    );
    assert_eq!(
        context_ir.omissions.candidates_considered,
        context_ir.omissions.selected + context_ir.omissions.omitted
    );
}

#[test]
fn history_discovery_exhausts_the_continued_request_ledger_before_packing() {
    let mut fixture = Fixture::new();
    let baseline_generation_id = fixture.generation_id.clone();
    let root = std::path::Path::new(&fixture.workspace.canonical_root);
    std::fs::write(
        root.join("src/other.rs"),
        "pub fn other(value: i64) -> i64 { value - 2 }\n",
    )
    .unwrap();
    for index in 0..32 {
        std::fs::write(
            root.join(format!("src/review_change_{index:02}.rs")),
            format!("pub fn review_change_{index:02}() -> usize {{ {index} }}\n"),
        )
        .unwrap();
    }
    let generation_id = fixture.reconcile_again();
    let generation_evidence_rows: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM generation_file
             WHERE generation_id IN (?1, ?2) AND presence_state = 'present'",
            [&baseline_generation_id, &generation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(generation_evidence_rows > 5);
    let mut application_input = input(
        "Review changed targets under five work units",
        &["src/lib.rs"],
    );
    application_input.declared_task_kind = Some(TaskKind::Review);
    application_input.baseline_generation_id = Some(baseline_generation_id);
    let application_input = with_existing_session(&fixture, &generation_id, application_input);
    let request = deep_compile_request(&application_input);

    let sealed = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(10, 65_536, 16_384, 4, 5, 10).unwrap(),
        None,
    )
    .unwrap();
    let context_ir = sealed.context_ir;

    assert_eq!(context_ir.cost.work_units_consumed, 5);
    assert_eq!(context_ir.policy.semantic_budget.max_records, 10);
    assert!(context_ir.cost.work_units_consumed < context_ir.policy.semantic_budget.max_records);
    assert!(!context_ir
        .working_set
        .iter()
        .any(|item| item.role == workspace_atlas::context_ir::ItemRole::HistoricalConstraint));
    assert_eq!(
        context_ir.temporal_constraints.state,
        workspace_atlas::context_ir::IrTemporalStateV2::Unavailable
    );
    assert_eq!(
        context_ir.temporal_constraints.omission_reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::WorkUnitBudget)
    );
    assert_eq!(context_ir.omissions.candidates_considered, 1);
    assert_eq!(
        context_ir.omissions.candidates_considered,
        context_ir.omissions.selected + context_ir.omissions.omitted
    );
}

#[test]
fn distant_explicit_baseline_stops_at_the_shared_history_work_limit() {
    let mut fixture = Fixture::new();
    let baseline_generation_id = fixture.generation_id.clone();
    let root = std::path::Path::new(&fixture.workspace.canonical_root);
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"compiler-composition\"\n",
    )
    .unwrap();
    let mut generation_id = baseline_generation_id.clone();
    for index in 0..12 {
        let value = index + 100;
        std::fs::write(
            root.join("src/lib.rs"),
            format!("pub fn calculate(value: i64) -> i64 {{ value + {value} }}\n"),
        )
        .unwrap();
        generation_id = discovery::reconcile(&fixture.workspace, &fixture.connection, &config)
            .unwrap()
            .candidate_generation_id;
    }
    let mut application_input = input(
        "Review a distant retained baseline under five work units",
        &["src/lib.rs"],
    );
    application_input.declared_task_kind = Some(TaskKind::Review);
    application_input.baseline_generation_id = Some(baseline_generation_id);
    let application_input = with_existing_session(&fixture, &generation_id, application_input);
    let request = deep_compile_request(&application_input);

    let context_ir = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(10, 65_536, 16_384, 4, 5, 10).unwrap(),
        None,
    )
    .unwrap()
    .context_ir;

    assert_eq!(context_ir.cost.work_units_consumed, 5);
    assert_eq!(
        context_ir.temporal_constraints.state,
        workspace_atlas::context_ir::IrTemporalStateV2::Unavailable
    );
    assert_eq!(
        context_ir.temporal_constraints.omission_reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::WorkUnitBudget)
    );
    assert_eq!(context_ir.omissions.candidates_considered, 1);
    assert_eq!(
        context_ir.omissions.candidates_considered,
        context_ir.omissions.selected + context_ir.omissions.omitted
    );
}

#[test]
fn temporal_packing_uses_one_paid_sentinel_without_inventing_candidates() {
    let mut fixture = Fixture::new();
    let baseline_generation_id = fixture.generation_id.clone();
    let root = std::path::Path::new(&fixture.workspace.canonical_root);
    for index in 0..6 {
        std::fs::write(
            root.join(format!("src/packing_change_{index}.rs")),
            format!("pub fn packing_change_{index}() -> usize {{ {index} }}\n"),
        )
        .unwrap();
    }
    let generation_id = fixture.reconcile_again();
    let mut application_input = input(
        "Review temporal packing under a shared ledger",
        &["src/lib.rs"],
    );
    application_input.declared_task_kind = Some(TaskKind::Review);
    application_input.baseline_generation_id = Some(baseline_generation_id);
    let application_input = with_existing_session(&fixture, &generation_id, application_input);
    let request = deep_compile_request(&application_input);

    let ample = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(50, 65_536, 16_384, 4, 10_000, 10).unwrap(),
        None,
    )
    .unwrap()
    .context_ir;
    let discovery_work = ample
        .cost
        .work_units_consumed
        .checked_sub(ample.omissions.candidates_considered)
        .unwrap();
    let limited_work = discovery_work + 3;

    let limited = compile_deep_context_ir_v2(
        &mut fixture.connection,
        &fixture.workspace,
        &generation_id,
        &request,
        &DeepSemanticBudget::new(50, 65_536, 16_384, 4, limited_work, 10).unwrap(),
        None,
    )
    .unwrap()
    .context_ir;

    assert_eq!(limited.cost.work_units_consumed, limited_work);
    assert_eq!(limited.omissions.candidates_considered, 3);
    assert_eq!(limited.omissions.selected, 2);
    assert_eq!(limited.omissions.omitted, 1);
    assert_eq!(
        limited
            .omissions
            .by_reason
            .get(&workspace_atlas::context_ir::IrOmissionReasonV2::WorkUnitBudget),
        Some(&1)
    );
    assert_eq!(
        limited.omissions.candidates_considered,
        limited.omissions.selected + limited.omissions.omitted
    );
    assert_eq!(
        limited.temporal_constraints.omission_reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::WorkUnitBudget)
    );
}

#[test]
fn unrelated_tenfold_growth_and_ready_serving_leave_v2_work_unchanged() {
    let mut small = Fixture::new();
    let mut grown = Fixture::with_unrelated_files(36);
    let small_file_count: i64 = small
        .connection
        .query_row(
            "SELECT COUNT(*) FROM current_file WHERE generation_id = ?1",
            [&small.generation_id],
            |row| row.get(0),
        )
        .unwrap();
    let grown_file_count: i64 = grown
        .connection
        .query_row(
            "SELECT COUNT(*) FROM current_file WHERE generation_id = ?1",
            [&grown.generation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(grown_file_count, small_file_count * 10);

    let (small_truth, small_ready) = compile_before_and_after_serving(&mut small);
    let (grown_truth, grown_ready) = compile_before_and_after_serving(&mut grown);

    assert_eq!(
        serde_json::to_vec(&small_truth).unwrap(),
        serde_json::to_vec(&small_ready).unwrap(),
        "V2 remains Truth-only when a ready Serving projection exists"
    );
    assert_eq!(
        serde_json::to_vec(&grown_truth).unwrap(),
        serde_json::to_vec(&grown_ready).unwrap(),
        "ready Serving cannot change V2 output after unrelated growth"
    );
    assert_eq!(
        serde_json::to_value(&small_truth.cost).unwrap(),
        serde_json::to_value(&grown_truth.cost).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&small_truth.omissions).unwrap(),
        serde_json::to_value(&grown_truth.omissions).unwrap()
    );
    assert_eq!(small_truth.cost.work_units_consumed, 2);
    assert_eq!(
        small_truth
            .working_set
            .iter()
            .map(|item| (&item.entity_id, item.role, item.selection_reason))
            .collect::<Vec<_>>(),
        grown_truth
            .working_set
            .iter()
            .map(|item| (&item.entity_id, item.role, item.selection_reason))
            .collect::<Vec<_>>()
    );
}

#[test]
fn generated_v2_is_task_conditioned_private_and_never_persisted_as_context_ir() {
    let mut fixture = Fixture::new();
    let secret_task = "Explore calculate with password=do-not-retain and model gpt-private";
    let first_input = with_existing_session(
        &fixture,
        &fixture.generation_id,
        input(secret_task, &["src/lib.rs", "tests/calculate.rs"]),
    );
    let capabilities = [(
        RequiredCapability::RequiredRoleClosure,
        CallerCapabilityState::Unsatisfied,
    )];
    let first = dispatch_catalogue_context_application(
        request(
            &first_input,
            &capabilities,
            Route::AtlasDeep,
            Route::AtlasDeep,
            &fixture.generation_id,
        ),
        first_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    let second_input = with_existing_session(
        &fixture,
        &fixture.generation_id,
        input(
            "Explore another real target",
            &["src/other.rs", "tests/calculate.rs"],
        ),
    );
    let second = dispatch_catalogue_context_application(
        request(
            &second_input,
            &capabilities,
            Route::AtlasDeep,
            Route::AtlasDeep,
            &fixture.generation_id,
        ),
        second_input,
        &mut fixture.connection,
        &fixture.workspace,
    )
    .unwrap();

    assert!(first.deep_context_ir.is_some(), "{:#?}", first.execution);
    assert!(second.deep_context_ir.is_some(), "{:#?}", second.execution);
    let first_ir = first.deep_context_ir.unwrap();
    let second_ir = second.deep_context_ir.unwrap();
    assert_ne!(first_ir.task.task_hash, second_ir.task.task_hash);
    assert_ne!(first_ir.context_hash, second_ir.context_hash);
    let serialized = serde_json::to_string(&first_ir).unwrap();
    assert!(!serialized.contains(secret_task));
    assert!(!serialized.contains("password=do-not-retain"));
    assert!(!serialized.contains("gpt-private"));
    assert!(!serialized.contains("value + 1"));

    assert_eq!(
        fixture
            .connection
            .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .connection
            .query_row(
                "SELECT COUNT(*) FROM task_session WHERE context_ir_version != ?1",
                [CONTEXT_SCHEMA_VERSION],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .connection
            .query_row(
                "SELECT COUNT(*) FROM task_session WHERE state = 'created'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2,
        "V2 composition must leave caller-owned lifecycle state unchanged"
    );
    assert_eq!(
        fixture
            .connection
            .query_row(
                "SELECT COUNT(*) FROM context_use_event e
                 WHERE NOT EXISTS (
                    SELECT 1 FROM task_session s WHERE s.task_session_id = e.task_session_id
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let persisted: String = fixture
        .connection
        .query_row(
            "SELECT COALESCE(GROUP_CONCAT(COALESCE(raw_task, '') || COALESCE(details_json, '')), '')\n             FROM task_session LEFT JOIN context_use_event USING (task_session_id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!persisted.contains("password=do-not-retain"));
    assert!(!persisted.contains("value + 1"));
}
