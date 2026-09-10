use assert_cmd::Command;
use std::collections::BTreeMap;

use workspace_atlas::catalogue::{
    apply_unregister, init_catalogue, UnregisterFault, UnregisterTarget,
};
use workspace_atlas::config::Config;
use workspace_atlas::context_application::{
    build_governor_run_request, decode_application_cursor, encode_application_cursor,
    run_catalogue_governor, show_context_yield, ApplicationCursorBinding, ApplicationCursorError,
    ApplicationCursorFilter, ApplicationCursorOperation, GovernorDeepLimits, GovernorRunRequest,
    GovernorRunSemanticInput,
};
use workspace_atlas::context_ir::{
    TaskKind, TaskSessionState, CONTEXT_SCHEMA_V2_VERSION, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::context_route::{
    discover_context_capabilities, AtlasIntent, CallerCapabilityState, ContextRouteError,
    GenerationObservation, NegotiationRequest, RequiredCapability, Route,
    CONTEXT_CAPABILITIES_VERSION, CONTEXT_EXECUTION_VERSION, CONTEXT_ROUTE_POLICY_VERSION,
    DEFAULT_APPLICATION_PAGE_LIMIT, MAX_APPLICATION_CURSOR_BYTES, MAX_APPLICATION_PAGE_LIMIT,
    MAX_GOVERNOR_REQUEST_BYTES, MAX_GOVERNOR_TARGETS, MAX_GOVERNOR_TARGET_BYTES,
};
use workspace_atlas::discovery;
use workspace_atlas::task_session::{
    abandon_legacy_task_session, apply_privacy_compaction, complete_legacy_task_session,
    load_task_session, privacy_compaction_preview, show_legacy_task_session,
    start_legacy_task_session, transition_task_session, LegacyLifecycleError,
    LegacyTaskAbandonReason, LegacyTaskAbandonRequest, LegacyTaskCompleteRequest,
    LegacyTaskShowRequest, LegacyTaskStartRequest, PrivacyCompactionAction, PrivacyCompactionFault,
    PrivacyCompactionResult,
};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

struct Fixture {
    _database_directory: tempfile::TempDir,
    workspace_directory: tempfile::TempDir,
    connection: rusqlite::Connection,
    workspace: WorkspaceRecord,
    config: Config,
    start_generation_id: String,
    start_tree_hash: String,
}

fn fixture(name: &str) -> Fixture {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 1 }\n",
    )
    .unwrap();
    let config = Config::parse(&format!(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"{name}\"\n"
    ))
    .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    let workspace = register_workspace(
        &connection,
        &workspace_directory.path().canonicalize().unwrap(),
        &config,
        &catalogue,
        "1.0.0",
    )
    .unwrap();
    let reconciled = discovery::reconcile(&workspace, &connection, &config).unwrap();
    Fixture {
        _database_directory: database_directory,
        workspace_directory,
        connection,
        workspace,
        config,
        start_generation_id: reconciled.candidate_generation_id,
        start_tree_hash: reconciled.source_tree_hash,
    }
}

fn supported_versions() -> NegotiationRequest {
    NegotiationRequest {
        capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_string()],
        route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
        ir_versions: vec![CONTEXT_SCHEMA_V2_VERSION.to_string()],
        operation_versions: vec![
            "query-v1.0.0".to_string(),
            "source-reference-v1.0.0".to_string(),
        ],
    }
}

fn governor_request(ceiling: Route) -> GovernorRunRequest {
    GovernorRunRequest {
        supported_versions: supported_versions(),
        semantic: GovernorRunSemanticInput {
            task: "Fix the deterministic route builder".to_string(),
            declared_kind: None,
            path_targets: vec!["src/lib.rs".to_string(), "src/lib.rs".to_string()],
            symbol_targets: vec!["crate::answer".to_string()],
            caller_capabilities: BTreeMap::from([(
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            )]),
            atlas_intent: AtlasIntent::Allow,
            route_floor: Route::Direct,
            route_ceiling: ceiling,
            legacy_task_session_id: None,
            deep_limits: (ceiling == Route::AtlasDeep).then_some(GovernorDeepLimits {
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
    }
}

fn start_request(task: &str) -> LegacyTaskStartRequest {
    LegacyTaskStartRequest {
        context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
        task: task.to_string(),
        declared_kind: Some(TaskKind::BugFix),
    }
}

fn show_request(session_id: &str, limit: Option<usize>) -> LegacyTaskShowRequest {
    LegacyTaskShowRequest {
        context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
        task_session_id: session_id.to_string(),
        limit,
        cursor: None,
    }
}

fn advance_to_active(connection: &rusqlite::Connection, session_id: &str) {
    transition_task_session(connection, session_id, TaskSessionState::ContextCompiled).unwrap();
    transition_task_session(connection, session_id, TaskSessionState::Active).unwrap();
}

fn append_event(
    connection: &rusqlite::Connection,
    session_id: &str,
    event_id: &str,
    occurred_at: &str,
) {
    connection
        .execute(
            "INSERT INTO context_use_event (
                event_id, task_session_id, context_id, item_id, entity_id,
                event_type, observation_source, byte_count, details_json, occurred_at
             ) VALUES (?1, ?2, NULL, NULL, NULL,
                       'context_supplied', 'atlas_observed', NULL, '{}', ?3)",
            rusqlite::params![event_id, session_id, occurred_at],
        )
        .unwrap();
}
fn append_metric_affecting_event(
    connection: &rusqlite::Connection,
    session_id: &str,
    event_id: &str,
    occurred_at: &str,
) {
    connection
        .execute(
            "INSERT INTO context_use_event (
                event_id, task_session_id, context_id, item_id, entity_id,
                event_type, observation_source, byte_count, details_json, occurred_at
             ) VALUES (?1, ?2, NULL, NULL, 'metric-file',
                       'context_supplied', 'atlas_observed', 7,
                       '{\"entity_kind\":\"file\",\"source_bytes\":7}', ?3)",
            rusqlite::params![event_id, session_id, occurred_at],
        )
        .unwrap();
}

fn forge_application_cursor(token: &str, mutate: impl FnOnce(&mut serde_json::Value)) -> String {
    let (encoded, _) = token.split_once('.').unwrap();
    let bytes = hex::decode(encoded).unwrap();
    let mut payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    mutate(&mut payload);
    let bytes = workspace_atlas::provider_contract::canonical_json_bytes(&payload);
    let mut checksum_input = b"workspace-atlas/application-cursor-v1\0".to_vec();
    checksum_input.extend_from_slice(&bytes);
    format!(
        "{}.{}",
        hex::encode(&bytes),
        workspace_atlas::hashing::content_hash_of_bytes(&checksum_input)
    )
}

#[test]
fn governor_builder_negotiates_versions_separately_and_derives_route_identity() {
    let fixture = fixture("surface-governor");
    let mut request = governor_request(Route::AtlasDeep);
    request.semantic.legacy_task_session_id = Some("task_legacy".to_string());
    let built =
        build_governor_run_request(request, &fixture.connection, &fixture.workspace).unwrap();

    assert_eq!(
        built.negotiated_versions.route_version,
        CONTEXT_ROUTE_POLICY_VERSION
    );
    assert_eq!(
        built.route_request.starting_generation,
        GenerationObservation::Observed {
            generation_id: fixture.start_generation_id.clone(),
        }
    );
    assert_eq!(built.route_request.classifier_kind, TaskKind::BugFix);
    assert_eq!(built.route_request.explicit_seeds.path_count, 1);
    assert_eq!(built.route_request.explicit_seeds.symbol_count, 1);
    assert_eq!(built.catalogue_input.seed_paths, vec!["src/lib.rs"]);
    assert_eq!(built.catalogue_input.seed_symbols, vec!["crate::answer"]);
    assert_eq!(
        built.catalogue_input.task_session_id.as_deref(),
        Some("task_legacy")
    );
    assert!(built.route_request.deep_budget.is_some());
    assert_ne!(
        built.route_request.normalized_task_hash,
        built.route_request.explicit_seeds.digest
    );

    let rebuilt = build_governor_run_request(
        governor_request(Route::AtlasDeep),
        &fixture.connection,
        &fixture.workspace,
    )
    .unwrap();
    assert_eq!(built.route_request, rebuilt.route_request);

    let mut unsupported = governor_request(Route::AtlasLight);
    unsupported.supported_versions.route_versions = vec!["context-route-v1.0.0".into()];
    assert!(matches!(
        build_governor_run_request(unsupported, &fixture.connection, &fixture.workspace),
        Err(
            workspace_atlas::context_application::CatalogueContextApplicationError::Route(
                ContextRouteError::NoCommonVersion { .. }
            )
        )
    ));
}

#[test]
fn bound_governor_validates_exact_identity_and_activates_only_success() {
    let mut fixture = fixture("surface-governor-lifecycle");
    let task = "Fix the deterministic route builder";
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request(task),
    )
    .unwrap();
    let mut bound = governor_request(Route::Direct);
    bound.semantic.task = task.to_string();
    bound.semantic.declared_kind = Some(TaskKind::BugFix);
    bound.semantic.legacy_task_session_id = Some(session.task_session_id.clone());
    bound.semantic.caller_capabilities.insert(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Satisfied,
    );
    let completed =
        run_catalogue_governor(bound.clone(), &mut fixture.connection, &fixture.workspace).unwrap();
    assert_eq!(
        completed.execution.state(),
        workspace_atlas::context_route::ExecutionState::Completed
    );
    assert!(matches!(
        completed.execution.payload(),
        Some(workspace_atlas::context_route::ContextPayload::DirectNone {})
    ));
    assert_eq!(
        load_task_session(&fixture.connection, &session.task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Active
    );
    run_catalogue_governor(bound.clone(), &mut fixture.connection, &fixture.workspace).unwrap();

    let mut mismatched = bound.clone();
    mismatched.semantic.task = "A different task identity".to_string();
    assert!(
        run_catalogue_governor(mismatched, &mut fixture.connection, &fixture.workspace)
            .unwrap_err()
            .to_string()
            .contains("identity mismatch")
    );
    assert_eq!(
        load_task_session(&fixture.connection, &session.task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Active
    );

    abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &LegacyTaskAbandonRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            reason: LegacyTaskAbandonReason::UserRequested,
        },
    )
    .unwrap();
    assert!(
        run_catalogue_governor(bound, &mut fixture.connection, &fixture.workspace)
            .unwrap_err()
            .to_string()
            .contains("is terminal")
    );

    let unbound_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Unbound governor leaves lifecycle alone"),
    )
    .unwrap();
    let mut unbound = governor_request(Route::Direct);
    unbound.semantic.task = "Unbound governor leaves lifecycle alone".to_string();
    unbound.semantic.declared_kind = Some(TaskKind::BugFix);
    unbound.semantic.caller_capabilities.insert(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Satisfied,
    );
    run_catalogue_governor(unbound, &mut fixture.connection, &fixture.workspace).unwrap();
    assert_eq!(
        load_task_session(&fixture.connection, &unbound_session.task_session_id)
            .unwrap()
            .unwrap()
            .state,
        TaskSessionState::Created
    );
}

#[test]
fn governor_builder_and_discovery_enforce_every_frozen_request_bound() {
    let fixture = fixture("surface-governor-bounds");
    let capabilities = discover_context_capabilities();
    assert_eq!(
        capabilities.bounds.max_request_bytes,
        MAX_GOVERNOR_REQUEST_BYTES
    );
    assert_eq!(
        capabilities.bounds.default_page_limit,
        DEFAULT_APPLICATION_PAGE_LIMIT
    );
    assert_eq!(
        capabilities.bounds.max_page_limit,
        MAX_APPLICATION_PAGE_LIMIT
    );
    assert_eq!(
        capabilities.bounds.max_cursor_bytes,
        MAX_APPLICATION_CURSOR_BYTES
    );
    assert_eq!(capabilities.bounds.max_targets, MAX_GOVERNOR_TARGETS);
    let serialized_bounds = serde_json::to_value(&capabilities.bounds).unwrap();
    for frozen_v2_extension in [
        "max_compiled_context_document_bytes",
        "max_nested_vector_items",
        "max_string_bytes",
        "max_json_depth",
    ] {
        assert!(serialized_bounds.get(frozen_v2_extension).is_none());
    }

    let mut missing_deep = governor_request(Route::AtlasDeep);
    missing_deep.semantic.deep_limits = None;
    assert!(matches!(
        build_governor_run_request(missing_deep, &fixture.connection, &fixture.workspace),
        Err(
            workspace_atlas::context_application::CatalogueContextApplicationError::Route(
                ContextRouteError::DeepBudgetRequired
            )
        )
    ));

    let mut zero_limit = governor_request(Route::AtlasDeep);
    zero_limit
        .semantic
        .deep_limits
        .as_mut()
        .unwrap()
        .max_work_units = 0;
    assert!(matches!(
        build_governor_run_request(zero_limit, &fixture.connection, &fixture.workspace),
        Err(
            workspace_atlas::context_application::CatalogueContextApplicationError::Route(
                ContextRouteError::InvalidDeepBudget
            )
        )
    ));

    let mut excess_task = governor_request(Route::AtlasLight);
    excess_task.semantic.task = "x".repeat(MAX_GOVERNOR_REQUEST_BYTES + 1);
    assert!(
        build_governor_run_request(excess_task, &fixture.connection, &fixture.workspace).is_err()
    );

    let mut excess_targets = governor_request(Route::AtlasLight);
    excess_targets.semantic.path_targets = (0..=MAX_GOVERNOR_TARGETS)
        .map(|index| format!("src/{index}.rs"))
        .collect();
    assert!(
        build_governor_run_request(excess_targets, &fixture.connection, &fixture.workspace)
            .is_err()
    );

    let mut duplicate_flood = governor_request(Route::AtlasLight);
    duplicate_flood.semantic.path_targets = vec!["src/lib.rs".to_string(); 201];
    duplicate_flood.semantic.symbol_targets.clear();
    assert!(
        build_governor_run_request(duplicate_flood, &fixture.connection, &fixture.workspace)
            .is_err()
    );

    let mut combined_overflow = governor_request(Route::AtlasLight);
    combined_overflow.semantic.path_targets = (0..MAX_GOVERNOR_TARGETS / 2)
        .map(|index| format!("src/path-{index}.rs"))
        .collect();
    combined_overflow.semantic.symbol_targets = (0..=(MAX_GOVERNOR_TARGETS / 2))
        .map(|index| format!("crate::symbol_{index}"))
        .collect();
    assert!(
        build_governor_run_request(combined_overflow, &fixture.connection, &fixture.workspace)
            .is_err()
    );

    let mut combined_boundary = governor_request(Route::AtlasLight);
    combined_boundary.semantic.path_targets = (0..MAX_GOVERNOR_TARGETS / 2)
        .map(|index| format!("src/path-{index}.rs"))
        .collect();
    combined_boundary.semantic.symbol_targets = (0..MAX_GOVERNOR_TARGETS / 2)
        .map(|index| format!("crate::symbol_{index}"))
        .collect();
    let built =
        build_governor_run_request(combined_boundary, &fixture.connection, &fixture.workspace)
            .unwrap();
    assert_eq!(
        built.catalogue_input.seed_paths.len() + built.catalogue_input.seed_symbols.len(),
        MAX_GOVERNOR_TARGETS
    );

    let mut oversized_raw_target = governor_request(Route::AtlasLight);
    oversized_raw_target.semantic.path_targets = vec!["x".repeat(MAX_GOVERNOR_TARGET_BYTES + 1)];
    oversized_raw_target.semantic.symbol_targets.clear();
    assert!(build_governor_run_request(
        oversized_raw_target,
        &fixture.connection,
        &fixture.workspace,
    )
    .is_err());

    let mut unauthorized_materialization = governor_request(Route::AtlasLight);
    unauthorized_materialization.semantic.max_materialized_bytes = Some(1024);
    assert!(build_governor_run_request(
        unauthorized_materialization,
        &fixture.connection,
        &fixture.workspace,
    )
    .is_err());

    let mut missing_materialization_cap = governor_request(Route::AtlasLight);
    missing_materialization_cap.semantic.materialize_source = true;
    assert!(build_governor_run_request(
        missing_materialization_cap,
        &fixture.connection,
        &fixture.workspace,
    )
    .is_err());

    let mut materialization = governor_request(Route::AtlasLight);
    materialization.semantic.materialize_source = true;
    materialization.semantic.max_materialized_bytes = Some(1024);
    assert_eq!(
        build_governor_run_request(materialization, &fixture.connection, &fixture.workspace,)
            .unwrap()
            .max_materialized_bytes,
        Some(1024)
    );
}

#[test]
fn legacy_start_is_atomic_legacy_only_and_identically_replayed() {
    let mut fixture = fixture("surface-start");
    let request = start_request("Fix start replay");
    let first =
        start_legacy_task_session(&mut fixture.connection, &fixture.workspace, &request).unwrap();
    let replay =
        start_legacy_task_session(&mut fixture.connection, &fixture.workspace, &request).unwrap();

    assert_eq!(first.task_session_id, replay.task_session_id);
    assert_eq!(first.start_generation_id, fixture.start_generation_id);
    assert_eq!(
        first.start_tree_hash.as_deref(),
        Some(fixture.start_tree_hash.as_str())
    );
    assert_eq!(first.state, TaskSessionState::Created);
    assert_eq!(first.context_ir_version, CONTEXT_SCHEMA_VERSION);
    assert!(first.raw_task.is_none());

    let mut v2 = request.clone();
    v2.context_ir_version = CONTEXT_SCHEMA_V2_VERSION.to_string();
    assert!(matches!(
        start_legacy_task_session(&mut fixture.connection, &fixture.workspace, &v2),
        Err(LegacyLifecycleError::DurableContractUnavailable)
    ));

    fixture
        .connection
        .execute(
            "UPDATE task_session SET task_hash = ?2 WHERE task_session_id = ?1",
            rusqlite::params![first.task_session_id, "ff".repeat(32)],
        )
        .unwrap();
    assert!(
        start_legacy_task_session(&mut fixture.connection, &fixture.workspace, &request)
            .unwrap_err()
            .to_string()
            .contains("conflicting task start replay identity")
    );
}

#[test]
fn legacy_abandon_accepts_only_active_or_reconciled_and_replay_is_exact() {
    let mut fixture = fixture("surface-abandon");
    let created = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix abandon lifecycle"),
    )
    .unwrap();
    let abandon = LegacyTaskAbandonRequest {
        context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
        reason: LegacyTaskAbandonReason::UserRequested,
    };
    assert!(abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &abandon,
    )
    .unwrap_err()
    .to_string()
    .contains("created -> abandoned"));

    transition_task_session(
        &fixture.connection,
        &created.task_session_id,
        TaskSessionState::ContextCompiled,
    )
    .unwrap();
    assert!(abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &abandon,
    )
    .unwrap_err()
    .to_string()
    .contains("context_compiled -> abandoned"));
    transition_task_session(
        &fixture.connection,
        &created.task_session_id,
        TaskSessionState::Active,
    )
    .unwrap();
    let abandoned = abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &abandon,
    )
    .unwrap();
    assert_eq!(abandoned.state, TaskSessionState::Abandoned);
    assert_eq!(abandoned.outcome_code.as_deref(), Some("user_requested"));
    assert!(abandoned.completed_at.is_some());
    let replayed = abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &abandon,
    )
    .unwrap();
    assert_eq!(replayed.completed_at, abandoned.completed_at);

    let conflict = LegacyTaskAbandonRequest {
        reason: LegacyTaskAbandonReason::InactivityTimeout,
        ..abandon.clone()
    };
    assert!(abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &conflict,
    )
    .unwrap_err()
    .to_string()
    .contains("conflicting abandon replay"));
    assert!(complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &created.task_session_id,
        &LegacyTaskCompleteRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            accepted: false,
            tests_passed: None,
            outcome_code: "abandoned".to_string(),
        },
    )
    .unwrap_err()
    .to_string()
    .contains("abandoned -> completed"));

    let mut v2_abandon = abandon.clone();
    v2_abandon.context_ir_version = CONTEXT_SCHEMA_V2_VERSION.to_string();
    assert!(matches!(
        abandon_legacy_task_session(
            &mut fixture.connection,
            &fixture.workspace,
            &created.task_session_id,
            &v2_abandon,
        ),
        Err(LegacyLifecycleError::DurableContractUnavailable)
    ));

    let second = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix reconciled abandon"),
    )
    .unwrap();
    advance_to_active(&fixture.connection, &second.task_session_id);
    transition_task_session(
        &fixture.connection,
        &second.task_session_id,
        TaskSessionState::Reconciled,
    )
    .unwrap();
    let abandoned = abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &second.task_session_id,
        &LegacyTaskAbandonRequest {
            reason: LegacyTaskAbandonReason::InactivityTimeout,
            ..abandon
        },
    )
    .unwrap();
    assert_eq!(abandoned.state, TaskSessionState::Abandoned);
    assert_eq!(
        abandoned.outcome_code.as_deref(),
        Some("inactivity_timeout")
    );
}

#[test]
fn legacy_abandon_reason_wire_vocabulary_is_exact_and_closed() {
    assert_eq!(
        serde_json::to_string(&LegacyTaskAbandonReason::UserRequested).unwrap(),
        "\"user_requested\""
    );
    assert_eq!(
        serde_json::to_string(&LegacyTaskAbandonReason::InactivityTimeout).unwrap(),
        "\"inactivity_timeout\""
    );
    for unknown in ["superseded", "no_longer_needed", "unknown"] {
        assert!(
            serde_json::from_str::<LegacyTaskAbandonReason>(&format!("\"{unknown}\"")).is_err()
        );
    }
}

#[test]
fn legacy_complete_derives_generation_delta_and_replay_is_exact() {
    let mut fixture = fixture("surface-complete");
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix completion lifecycle"),
    )
    .unwrap();
    let request = LegacyTaskCompleteRequest {
        context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
        accepted: true,
        tests_passed: Some(true),
        outcome_code: "accepted_change".to_string(),
    };
    assert!(complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &request,
    )
    .unwrap_err()
    .to_string()
    .contains("created -> completed"));

    advance_to_active(&fixture.connection, &session.task_session_id);
    assert!(complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &request,
    )
    .unwrap_err()
    .to_string()
    .contains("active -> completed"));
    std::fs::write(
        fixture.workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 2 }\n",
    )
    .unwrap();
    let end =
        discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config).unwrap();

    let completed = complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &request,
    )
    .unwrap();
    assert_eq!(completed.state, TaskSessionState::Completed);
    assert_eq!(
        completed.end_generation_id.as_deref(),
        Some(end.candidate_generation_id.as_str())
    );
    assert_eq!(
        completed.end_tree_hash.as_deref(),
        Some(end.source_tree_hash.as_str())
    );
    assert_eq!(completed.accepted, Some(true));
    assert_eq!(completed.tests_passed, Some(true));

    let replay = complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &request,
    )
    .unwrap();
    assert_eq!(replay.completed_at, completed.completed_at);

    let conflict = LegacyTaskCompleteRequest {
        accepted: false,
        outcome_code: "rejected_change".to_string(),
        ..request.clone()
    };
    assert!(complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &conflict,
    )
    .unwrap_err()
    .to_string()
    .contains("conflicting completion replay"));
    assert!(abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &LegacyTaskAbandonRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            reason: LegacyTaskAbandonReason::UserRequested,
        },
    )
    .unwrap_err()
    .to_string()
    .contains("completed -> abandoned"));

    let mut v2 = request;
    v2.context_ir_version = CONTEXT_SCHEMA_V2_VERSION.to_string();
    assert!(matches!(
        complete_legacy_task_session(
            &mut fixture.connection,
            &fixture.workspace,
            &session.task_session_id,
            &v2,
        ),
        Err(LegacyLifecycleError::DurableContractUnavailable)
    ));
}

#[test]
fn legacy_show_is_bounded_restart_stable_and_frozen_under_growth() {
    let mut fixture = fixture("surface-show");
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Show bounded task events"),
    )
    .unwrap();
    append_event(
        &fixture.connection,
        &session.task_session_id,
        "event-a",
        "2026-09-04T00:00:00.000Z",
    );
    append_event(
        &fixture.connection,
        &session.task_session_id,
        "event-b",
        "2026-09-04T00:00:01.000Z",
    );
    append_event(
        &fixture.connection,
        &session.task_session_id,
        "event-c",
        "2026-09-04T00:00:02.000Z",
    );

    let first = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&session.task_session_id, Some(2)),
    )
    .unwrap();
    assert_eq!(
        first
            .events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["event-a", "event-b"]
    );
    let cursor = first.next_cursor.unwrap();
    assert!(cursor.len() <= MAX_APPLICATION_CURSOR_BYTES);

    append_event(
        &fixture.connection,
        &session.task_session_id,
        "event-after-freeze",
        "2026-09-04T00:00:00.500Z",
    );
    let second = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some(cursor.clone()),
            ..show_request(&session.task_session_id, Some(2))
        },
    )
    .unwrap();
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["event-c"]
    );
    assert!(second.next_cursor.is_none());

    let malformed = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some("not-a-cursor".to_string()),
            ..show_request(&session.task_session_id, Some(2))
        },
    )
    .unwrap_err();
    assert!(matches!(malformed, LegacyLifecycleError::CursorInvalid));
    assert!(show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&session.task_session_id, Some(0)),
    )
    .is_err());
    assert!(show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &show_request(
            &session.task_session_id,
            Some(MAX_APPLICATION_PAGE_LIMIT + 1)
        ),
    )
    .is_err());

    let default_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Show default bounded task events"),
    )
    .unwrap();
    for index in 0..=DEFAULT_APPLICATION_PAGE_LIMIT {
        append_event(
            &fixture.connection,
            &default_session.task_session_id,
            &format!("default-event-{index:03}"),
            &format!("2026-09-04T01:00:{index:03}.000Z"),
        );
    }
    let default_page = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&default_session.task_session_id, None),
    )
    .unwrap();
    assert_eq!(default_page.events.len(), DEFAULT_APPLICATION_PAGE_LIMIT);
    assert!(default_page.next_cursor.is_some());

    let mut v2 = show_request(&session.task_session_id, None);
    v2.context_ir_version = CONTEXT_SCHEMA_V2_VERSION.to_string();
    assert!(matches!(
        show_legacy_task_session(&fixture.connection, &fixture.workspace, &v2),
        Err(LegacyLifecycleError::DurableContractUnavailable)
    ));

    advance_to_active(&fixture.connection, &session.task_session_id);
    let stale = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some(cursor),
            ..show_request(&session.task_session_id, Some(2))
        },
    )
    .unwrap_err();
    assert!(matches!(stale, LegacyLifecycleError::CursorStale));
}

#[test]
fn application_cursors_are_tamper_proof_and_operation_bound() {
    let mut fixture = fixture("surface-application-cursor");
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Bind every application cursor"),
    )
    .unwrap();
    for index in 0..3 {
        append_event(
            &fixture.connection,
            &session.task_session_id,
            &format!("cursor-event-{index}"),
            &format!("2026-09-04T02:00:0{index}.000Z"),
        );
    }
    fixture
        .connection
        .execute(
            "UPDATE context_use_event SET event_type = 'context_recompiled'
             WHERE task_session_id = ?1",
            [&session.task_session_id],
        )
        .unwrap();
    let binding = ApplicationCursorBinding {
        workspace_id: &fixture.workspace.workspace_id,
        operation: ApplicationCursorOperation::CompiledContextShow,
        contract_version: CONTEXT_SCHEMA_VERSION,
        filter: ApplicationCursorFilter::TaskSession {
            task_session_id: &session.task_session_id,
        },
        captured_state: "watermark",
    };
    let token = encode_application_cursor(
        &fixture.connection,
        &fixture.workspace,
        &binding,
        "2026-09-04T00:00:00.000Z|context-a",
    )
    .unwrap();
    assert!(token.len() <= MAX_APPLICATION_CURSOR_BYTES);
    assert_eq!(
        decode_application_cursor(&fixture.connection, &fixture.workspace, &binding, &token,)
            .unwrap(),
        "2026-09-04T00:00:00.000Z|context-a"
    );
    let mut tampered = token.into_bytes();
    tampered[8] ^= 1;
    let tampered = String::from_utf8(tampered).unwrap();
    assert!(matches!(
        decode_application_cursor(&fixture.connection, &fixture.workspace, &binding, &tampered,),
        Err(ApplicationCursorError::Invalid)
    ));
    let wrong_operation = ApplicationCursorBinding {
        workspace_id: &fixture.workspace.workspace_id,
        operation: ApplicationCursorOperation::ServingStatus,
        contract_version: CONTEXT_SCHEMA_VERSION,
        filter: ApplicationCursorFilter::TaskSession {
            task_session_id: &session.task_session_id,
        },
        captured_state: "watermark",
    };
    assert!(matches!(
        decode_application_cursor(
            &fixture.connection,
            &fixture.workspace,
            &wrong_operation,
            &encode_application_cursor(&fixture.connection, &fixture.workspace, &binding, "key",)
                .unwrap(),
        ),
        Err(ApplicationCursorError::Invalid)
    ));

    let task_page = show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&session.task_session_id, Some(1)),
    )
    .unwrap();
    let task_cursor = task_page.next_cursor.unwrap();
    assert!(show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some(task_cursor.clone()),
            ..show_request(&session.task_session_id, Some(1))
        },
    )
    .is_err());
    append_metric_affecting_event(
        &fixture.connection,
        &session.task_session_id,
        "cursor-event-forged-growth",
        "2026-09-04T02:00:03.000Z",
    );
    let (captured_event_rowid, captured_event_count): (i64, i64) = fixture
        .connection
        .query_row(
            "SELECT MAX(rowid), COUNT(*) FROM context_use_event WHERE task_session_id = ?1",
            [&session.task_session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let forged_watermark = forge_application_cursor(&task_cursor, |payload| {
        let mut state: serde_json::Value =
            serde_json::from_str(payload["captured_state"].as_str().unwrap()).unwrap();
        state["captured_event_rowid"] = captured_event_rowid.into();
        state["captured_event_count"] = captured_event_count.into();
        payload["captured_state"] = serde_json::to_string(&state).unwrap().into();
    });
    let forged_ordering = forge_application_cursor(&task_cursor, |payload| {
        payload["ordering_key"] = serde_json::json!({
            "last_occurred_at": "2026-09-04T02:00:01.000Z",
            "last_event_id": "cursor-event-1"
        })
        .to_string()
        .into();
    });
    for forged in [forged_watermark, forged_ordering] {
        assert!(matches!(
            show_legacy_task_session(
                &fixture.connection,
                &fixture.workspace,
                &LegacyTaskShowRequest {
                    cursor: Some(forged),
                    ..show_request(&session.task_session_id, Some(1))
                },
            ),
            Err(LegacyLifecycleError::CursorStale)
        ));
    }
    let yield_page = show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&session.task_session_id, Some(1)),
    )
    .unwrap();
    let yield_cursor = yield_page.next_cursor.unwrap();
    let frozen_metrics = yield_page.metrics.clone();
    append_metric_affecting_event(
        &fixture.connection,
        &session.task_session_id,
        "cursor-event-after-yield-freeze",
        "2026-09-04T02:00:04.000Z",
    );
    let continued_yield = show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some(yield_cursor.clone()),
            ..show_request(&session.task_session_id, Some(1))
        },
    )
    .unwrap();
    assert!(continued_yield
        .events
        .iter()
        .all(|event| event.event_id != "cursor-event-after-yield-freeze"));
    assert_eq!(continued_yield.metrics, frozen_metrics);
    assert!(show_legacy_task_session(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: Some(yield_cursor.clone()),
            ..show_request(&session.task_session_id, Some(1))
        },
    )
    .is_err());
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap();
    let rejected = run_atlas(&[
        "task",
        "show",
        root,
        &session.task_session_id,
        "--cursor",
        &yield_cursor,
        "--catalogue",
        catalogue,
    ]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["kind"], "cursor_invalid");
}

#[test]
fn context_yield_projects_v15_report_only_for_completed_accepted_sessions() {
    let mut fixture = fixture("surface-context-yield-v15");
    let task = "Fix public Context Yield projection";
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request(task),
    )
    .unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let compiled = workspace_atlas::cli::build_context_ir_output(
        fixture.workspace_directory.path(),
        task.to_string(),
        Some("bug_fix".to_string()),
        Vec::new(),
        Vec::new(),
        40,
        32_768,
        8_000,
        Some(std::path::Path::new(&catalogue)),
    )
    .unwrap();
    assert_eq!(compiled.task_session_id, session.task_session_id);
    transition_task_session(
        &fixture.connection,
        &session.task_session_id,
        TaskSessionState::Active,
    )
    .unwrap();
    append_metric_affecting_event(
        &fixture.connection,
        &session.task_session_id,
        "yield-page-event-1",
        "2026-09-07T00:00:01.000Z",
    );
    append_metric_affecting_event(
        &fixture.connection,
        &session.task_session_id,
        "yield-page-event-2",
        "2026-09-07T00:00:02.000Z",
    );
    std::fs::write(
        fixture.workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 2 }\n",
    )
    .unwrap();
    discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config).unwrap();
    let completed = complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &LegacyTaskCompleteRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            accepted: true,
            tests_passed: Some(true),
            outcome_code: "accepted_change".to_string(),
        },
    )
    .unwrap();
    let first = show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&session.task_session_id, Some(1)),
    )
    .unwrap();
    assert!(first.next_cursor.is_some());
    let second = show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &LegacyTaskShowRequest {
            cursor: first.next_cursor.clone(),
            ..show_request(&session.task_session_id, Some(1))
        },
    )
    .unwrap();
    let report = first.report.as_ref().expect("accepted report");
    assert_eq!(first.report, second.report);
    assert_eq!(report.schema_version, "1.5.0");
    assert_eq!(report.content.workspace_id, completed.workspace_id);
    assert_eq!(report.content.generation_id, completed.start_generation_id);
    assert_eq!(report.content.task_hash, completed.task_hash);
    assert_eq!(report.content.task_kind, completed.task_kind);
    assert_eq!(
        report.content.accepted_outcome.acceptance,
        workspace_atlas::context_yield::ContextYieldOutcomeAcceptance::Accepted
    );
    let expected_outcome_hash = workspace_atlas::hashing::content_hash_of_bytes(
        &serde_json::to_vec(&(
            "context-yield-accepted-outcome-v1",
            &completed.task_session_id,
            completed.end_generation_id.as_deref().unwrap(),
            completed.end_tree_hash.as_deref().unwrap(),
            completed.tests_passed,
            completed.outcome_code.as_deref().unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(
        report.content.accepted_outcome.outcome_hash,
        expected_outcome_hash
    );
    assert_eq!(report.content.sample_size, 1);
    assert_eq!(report.content.raw_measures.samples.len(), 1);
    assert_eq!(
        report
            .content
            .raw_measures
            .metrics
            .iter()
            .map(|metric| serde_json::to_value(metric.evidence_class).unwrap())
            .collect::<Vec<_>>(),
        vec![
            serde_json::json!("atlas_observed"),
            serde_json::json!("reported_use"),
            serde_json::json!("observed_and_reported"),
            serde_json::json!("atlas_observed"),
            serde_json::json!("atlas_observed"),
        ]
    );
    assert!(report
        .content
        .raw_measures
        .metrics
        .iter()
        .any(|metric| metric.denominator == 0
            && metric.invalidity
                == Some(
                    workspace_atlas::context_yield::ContextYieldMetricInvalidity::ZeroDenominator
                )));
    assert!(!report.content.validity.is_valid);
    assert!(report
        .content
        .validity
        .limitations
        .contains(&workspace_atlas::context_yield::ContextYieldLimitation::SmallSampleSize));
    assert!(report
        .content
        .validity
        .limitations
        .contains(&workspace_atlas::context_yield::ContextYieldLimitation::SingleEnvironment));
    assert_eq!(
        report.execution.generated_at,
        completed.completed_at.unwrap()
    );
    let serialized = serde_json::to_string(report).unwrap();
    assert!(!serialized.contains(task));
    assert!(!serialized.contains("\"prompt\""));
    assert!(!serialized.contains("source_content"));
}

#[test]
fn context_yield_does_not_launder_nonaccepted_sessions_into_v15_reports() {
    let mut fixture = fixture("surface-context-yield-v15-rejection");
    let incomplete = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix incomplete report projection"),
    )
    .unwrap();
    assert!(show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&incomplete.task_session_id, None),
    )
    .unwrap()
    .report
    .is_none());

    let abandoned = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix abandoned report projection"),
    )
    .unwrap();
    advance_to_active(&fixture.connection, &abandoned.task_session_id);
    abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &abandoned.task_session_id,
        &LegacyTaskAbandonRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            reason: LegacyTaskAbandonReason::UserRequested,
        },
    )
    .unwrap();
    assert!(show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&abandoned.task_session_id, None),
    )
    .unwrap()
    .report
    .is_none());

    let rejected = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Fix rejected report projection"),
    )
    .unwrap();
    advance_to_active(&fixture.connection, &rejected.task_session_id);
    std::fs::write(
        fixture.workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 3 }\n",
    )
    .unwrap();
    discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config).unwrap();
    complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &rejected.task_session_id,
        &LegacyTaskCompleteRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            accepted: false,
            tests_passed: None,
            outcome_code: "rejected_change".to_string(),
        },
    )
    .unwrap();
    assert!(show_context_yield(
        &fixture.connection,
        &fixture.workspace,
        &show_request(&rejected.task_session_id, None),
    )
    .unwrap()
    .report
    .is_none());
}
#[test]
fn compiled_context_show_validates_typed_integrity_and_hard_bounds() {
    let fixture = fixture("surface-compiled-context-validation");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let compiled = assert_cli_success(&run_atlas(&[
        "context-ir",
        root,
        "Compile bounded legacy context",
        "--path",
        "src/lib.rs",
        "--catalogue",
        &catalogue,
    ]));
    let context_id = compiled["context_ir"]["context_id"].as_str().unwrap();
    let shown = assert_cli_success(&run_atlas(&[
        "compiled-context",
        "show",
        root,
        "--context-id",
        context_id,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(shown["contexts"][0]["context_id"], context_id);

    let cursor_rejected = run_atlas(&[
        "compiled-context",
        "show",
        root,
        "--context-id",
        context_id,
        "--cursor",
        "1",
        "--catalogue",
        &catalogue,
    ]);
    assert!(!cursor_rejected.status.success());
    assert!(cursor_rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&cursor_rejected.stderr).contains("cursor_invalid"));

    fixture
        .connection
        .execute(
            "UPDATE context_ir SET canonical_json = canonical_json || ' '
             WHERE context_id = ?1",
            [context_id],
        )
        .unwrap();
    let noncanonical = run_atlas(&[
        "compiled-context",
        "show",
        root,
        "--context-id",
        context_id,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!noncanonical.status.success());
    assert!(noncanonical.stdout.is_empty());

    fixture
        .connection
        .execute(
            "UPDATE context_ir SET canonical_json = ?1 WHERE context_id = ?2",
            rusqlite::params![
                "x".repeat(workspace_atlas::context_route::MAX_COMPILED_CONTEXT_DOCUMENT_BYTES + 1),
                context_id
            ],
        )
        .unwrap();
    let oversized = run_atlas(&[
        "compiled-context",
        "show",
        root,
        "--context-id",
        context_id,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!oversized.status.success());
    assert!(oversized.stdout.is_empty());
}

#[test]
fn serving_and_retention_cursors_reuse_after_restart_and_stale_on_state_change() {
    let mut fixture = fixture("surface-serving-retention-cursors");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let registered_config =
        workspace_atlas::workspace::registered_config_path(std::path::Path::new(&catalogue))
            .unwrap();
    std::fs::create_dir_all(registered_config.parent().unwrap()).unwrap();
    std::fs::write(
        registered_config,
        serde_json::to_vec(&fixture.config).unwrap(),
    )
    .unwrap();

    for index in 0..3 {
        assert_cli_success(&run_atlas(&[
            "serving-build",
            root,
            "--catalogue",
            &catalogue,
        ]));
        std::fs::write(
            fixture.workspace_directory.path().join("src/lib.rs"),
            format!("pub fn answer() -> u8 {{ {} }}\n", index + 2),
        )
        .unwrap();
        assert_cli_success(&run_atlas(&["reconcile", root, "--catalogue", &catalogue]));
    }
    let first = assert_cli_success(&run_atlas(&[
        "serving-status",
        root,
        "--limit",
        "1",
        "--catalogue",
        &catalogue,
    ]));
    let serving_cursor = first["next_cursor"].as_str().unwrap().to_string();
    let continued = assert_cli_success(&run_atlas(&[
        "serving-status",
        root,
        "--limit",
        "1",
        "--cursor",
        &serving_cursor,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(
        continued["status"]["superseded"].as_array().unwrap().len(),
        1
    );
    let forged_serving = forge_application_cursor(&serving_cursor, |payload| {
        let mut ordering: serde_json::Value =
            serde_json::from_str(payload["ordering_key"].as_str().unwrap()).unwrap();
        ordering["generation_id"] = "forged-generation".into();
        payload["ordering_key"] = serde_json::to_string(&ordering).unwrap().into();
    });
    let rejected = run_atlas(&[
        "serving-status",
        root,
        "--cursor",
        &forged_serving,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["kind"], "cursor_stale");
    std::fs::write(
        fixture.workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 9 }\n",
    )
    .unwrap();
    assert_cli_success(&run_atlas(&["reconcile", root, "--catalogue", &catalogue]));
    let stale = run_atlas(&[
        "serving-status",
        root,
        "--cursor",
        &serving_cursor,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("cursor_stale"));

    for index in 0..3 {
        let session = start_legacy_task_session(
            &mut fixture.connection,
            &fixture.workspace,
            &start_request(&format!("Expire retention session {index}")),
        )
        .unwrap();
        fixture
            .connection
            .execute(
                "UPDATE task_session
                 SET state = 'completed', completed_at = '2000-01-01T00:00:00.000Z'
                 WHERE task_session_id = ?1",
                [&session.task_session_id],
            )
            .unwrap();
    }
    let first = assert_cli_success(&run_atlas(&[
        "retention",
        "status",
        root,
        "--limit",
        "1",
        "--catalogue",
        &catalogue,
    ]));
    let retention_cursor = first["next_cursor"].as_str().unwrap().to_string();
    let continued = assert_cli_success(&run_atlas(&[
        "retention",
        "status",
        root,
        "--limit",
        "1",
        "--cursor",
        &retention_cursor,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(continued["entries"].as_array().unwrap().len(), 1);
    let forged_retention = forge_application_cursor(&retention_cursor, |payload| {
        let mut ordering: serde_json::Value =
            serde_json::from_str(payload["ordering_key"].as_str().unwrap()).unwrap();
        ordering["record_id"] = "forged-record".into();
        payload["ordering_key"] = serde_json::to_string(&ordering).unwrap().into();
    });
    let rejected = run_atlas(&[
        "retention",
        "status",
        root,
        "--cursor",
        &forged_retention,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["kind"], "cursor_stale");
    let new_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Change captured retention state"),
    )
    .unwrap();
    fixture
        .connection
        .execute(
            "UPDATE task_session
             SET state = 'completed', completed_at = '2000-01-01T00:00:00.000Z'
             WHERE task_session_id = ?1",
            [&new_session.task_session_id],
        )
        .unwrap();
    let stale = run_atlas(&[
        "retention",
        "status",
        root,
        "--cursor",
        &retention_cursor,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("cursor_stale"));
}

#[test]
fn retention_pages_follow_the_canonical_total_order_without_gaps() {
    let make_fixture = fixture;
    let mut fixture = make_fixture("surface-retention-total-order");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let registered_config =
        workspace_atlas::workspace::registered_config_path(std::path::Path::new(&catalogue))
            .unwrap();
    std::fs::create_dir_all(registered_config.parent().unwrap()).unwrap();
    std::fs::write(
        registered_config,
        serde_json::to_vec(&fixture.config).unwrap(),
    )
    .unwrap();

    let as_of = chrono::Utc::now();
    let completed_at = [
        (as_of - chrono::Duration::days(181)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        (as_of - chrono::Duration::days(60)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        (as_of - chrono::Duration::days(61)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        (as_of - chrono::Duration::days(182)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    ];
    let derived_event_at =
        (as_of - chrono::Duration::days(59)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let mut session_ids = Vec::new();
    for task in ["lexical-a", "lexical-b", "lexical-c", "lexical-d"] {
        let session = start_legacy_task_session(
            &mut fixture.connection,
            &fixture.workspace,
            &start_request(task),
        )
        .unwrap();
        session_ids.push(session.task_session_id);
    }
    session_ids.sort();
    for (index, completed_at) in completed_at.iter().enumerate() {
        if index == 1 || index == 2 {
            append_event(
                &fixture.connection,
                &session_ids[index],
                &format!("retention-event-{index}"),
                &derived_event_at,
            );
        }
        fixture
            .connection
            .execute(
                "UPDATE task_session
                 SET state = 'completed', completed_at = ?2
                 WHERE task_session_id = ?1",
                rusqlite::params![session_ids[index], completed_at],
            )
            .unwrap();
    }
    for (metric_id, created_at) in [
        (
            "z_metric_created_first",
            (as_of - chrono::Duration::days(184))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        (
            "a_metric_created_last",
            (as_of - chrono::Duration::days(183))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    ] {
        fixture
            .connection
            .execute(
                "INSERT INTO query_stage_metric(
                    metric_id, workspace_id, generation_id, task_session_id, context_id,
                    request_id, operation, serving_fallback, truncated, created_at
                 ) VALUES (?1, ?2, ?3, NULL, NULL, ?1, 'find', 0, 0, ?4)",
                rusqlite::params![
                    metric_id,
                    fixture.workspace.workspace_id,
                    fixture.start_generation_id,
                    created_at
                ],
            )
            .unwrap();
    }

    let canonical = privacy_compaction_preview(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        as_of,
        200,
        None,
    )
    .unwrap();
    assert_eq!(canonical.entries.len(), 6);
    assert_eq!(
        canonical
            .entries
            .iter()
            .map(|entry| entry.action)
            .collect::<Vec<_>>(),
        vec![
            PrivacyCompactionAction::DeleteDerived,
            PrivacyCompactionAction::DeleteDerived,
            PrivacyCompactionAction::DeleteSession,
            PrivacyCompactionAction::DeleteSession,
            PrivacyCompactionAction::DeleteMetric,
            PrivacyCompactionAction::DeleteMetric,
        ]
    );
    assert_eq!(
        canonical
            .entries
            .iter()
            .filter(|entry| entry.action == PrivacyCompactionAction::DeleteMetric)
            .map(|entry| entry.record_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a_metric_created_last", "z_metric_created_first"]
    );

    let mut cursor = None;
    let mut concatenated = Vec::new();
    let mut first_cursor = None;
    loop {
        let mut args = vec!["retention", "status", root, "--limit", "1"];
        if let Some(value) = cursor.as_deref() {
            args.extend(["--cursor", value]);
        }
        args.extend(["--catalogue", &catalogue]);
        let page = assert_cli_success(&run_atlas(&args));
        assert_eq!(page["manifest_digest"], canonical.manifest_digest);
        assert_eq!(page["total_expired_entries"], canonical.entries.len());
        let entries = page["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        concatenated.push(entries[0].clone());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        first_cursor.get_or_insert_with(|| cursor.clone().unwrap());
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        concatenated,
        serde_json::to_value(&canonical.entries)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    );

    let restarted = assert_cli_success(&run_atlas(&[
        "retention",
        "status",
        root,
        "--limit",
        "1",
        "--cursor",
        first_cursor.as_deref().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(
        restarted["entries"],
        serde_json::json!([canonical.entries[1]])
    );
    assert_eq!(restarted["manifest_digest"], canonical.manifest_digest);

    let forged = forge_application_cursor(first_cursor.as_deref().unwrap(), |payload| {
        let mut ordering: serde_json::Value =
            serde_json::from_str(payload["ordering_key"].as_str().unwrap()).unwrap();
        ordering["record_id"] = "correctly-checksummed-forged-boundary".into();
        payload["ordering_key"] = serde_json::to_string(&ordering).unwrap().into();
    });
    let rejected = run_atlas(&[
        "retention",
        "status",
        root,
        "--limit",
        "1",
        "--cursor",
        &forged,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["kind"], "cursor_stale");

    let mut empty_fixture = make_fixture("surface-retention-empty");
    let empty = privacy_compaction_preview(
        &empty_fixture.connection,
        &empty_fixture.workspace.workspace_id,
        as_of,
        1,
        None,
    )
    .unwrap();
    assert!(empty.entries.is_empty());
    assert!(empty.next_cursor.is_none());
    let empty_root = empty_fixture.workspace_directory.path().to_str().unwrap();
    let empty_catalogue = empty_fixture.connection.path().unwrap().to_string();
    let empty_registered_config =
        workspace_atlas::workspace::registered_config_path(std::path::Path::new(&empty_catalogue))
            .unwrap();
    std::fs::create_dir_all(empty_registered_config.parent().unwrap()).unwrap();
    std::fs::write(
        empty_registered_config,
        serde_json::to_vec(&empty_fixture.config).unwrap(),
    )
    .unwrap();
    let empty_page = assert_cli_success(&run_atlas(&[
        "retention",
        "status",
        empty_root,
        "--limit",
        "1",
        "--catalogue",
        &empty_catalogue,
    ]));
    assert_eq!(empty_page["manifest_digest"], empty.manifest_digest);
    assert_eq!(empty_page["entries"], serde_json::json!([]));
    assert!(empty_page["next_cursor"].is_null());
    let empty_error = apply_privacy_compaction(
        &mut empty_fixture.connection,
        &empty_fixture.workspace.workspace_id,
        as_of,
        &empty.manifest_digest,
        "not-confirmed",
        PrivacyCompactionFault::None,
    )
    .unwrap_err();
    assert_eq!(
        empty_error.to_string(),
        "privacy deletion requires literal --confirm-privacy-deletion"
    );
}

type ApplyPrivacyCompactionSignature =
    fn(
        &mut rusqlite::Connection,
        &str,
        chrono::DateTime<chrono::Utc>,
        &str,
        &str,
        PrivacyCompactionFault,
    ) -> workspace_atlas::error::Result<PrivacyCompactionResult>;

type ApplyUnregisterSignature = fn(
    rusqlite::Connection,
    &WorkspaceRecord,
    UnregisterTarget,
    &str,
    &str,
    UnregisterFault,
) -> workspace_atlas::error::Result<()>;

#[test]
fn legacy_destructive_application_signatures_and_errors_remain_compatible() {
    let _: ApplyPrivacyCompactionSignature = apply_privacy_compaction;
    let _: ApplyUnregisterSignature = apply_unregister;
    let mut application_fixture = fixture("surface-application-confirmation-error");
    let workspace_id = application_fixture.workspace.workspace_id.clone();
    let application_error = workspace_atlas::task_session::application_apply_privacy_compaction(
        &mut application_fixture.connection,
        &workspace_id,
        chrono::Utc::now(),
        "unused",
        "not-confirmed",
        PrivacyCompactionFault::None,
    )
    .unwrap_err();
    assert!(matches!(
        application_error,
        workspace_atlas::context_application::CatalogueContextApplicationError::Boundary(
            workspace_atlas::context_application::ApplicationBoundaryError::LiteralConfirmation {
                operation: "privacy deletion",
                required: "--confirm-privacy-deletion",
            }
        )
    ));

    let fixture = fixture("surface-legacy-unregister-error");
    let catalogue = std::path::PathBuf::from(fixture.connection.path().unwrap());
    let target = UnregisterTarget::new(
        fixture.workspace_directory.path().to_path_buf(),
        catalogue,
        tempfile::tempdir().unwrap().path().to_path_buf(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
    )
    .unwrap();
    let error = apply_unregister(
        fixture.connection,
        &fixture.workspace,
        target,
        "unused",
        "not-confirmed",
        UnregisterFault::None,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "unregister requires literal --irreversible confirmation"
    );
}

#[test]
fn lifecycle_rollback_compatible_internal_functions_remain_explicit() {
    let mut fixture = fixture("surface-rollback");
    let session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &start_request("Preserve explicit lifecycle"),
    )
    .unwrap();
    transition_task_session(
        &fixture.connection,
        &session.task_session_id,
        TaskSessionState::Failed,
    )
    .unwrap();
    let persisted = load_task_session(&fixture.connection, &session.task_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, TaskSessionState::Failed);
    assert!(abandon_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &LegacyTaskAbandonRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            reason: LegacyTaskAbandonReason::InactivityTimeout,
        },
    )
    .unwrap_err()
    .to_string()
    .contains("failed -> abandoned"));
    assert!(complete_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &session.task_session_id,
        &LegacyTaskCompleteRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            accepted: false,
            tests_passed: None,
            outcome_code: "failed".to_string(),
        },
    )
    .unwrap_err()
    .to_string()
    .contains("failed -> completed"));
}

#[test]
fn cli_exposes_additive_governor_capabilities_surface() {
    let fixture = fixture("surface-cli-capabilities");
    let catalogue = fixture.connection.path().unwrap().to_string();
    let output = Command::cargo_bin("atlas")
        .unwrap()
        .args([
            "governor",
            "capabilities",
            fixture.workspace_directory.path().to_str().unwrap(),
            "--catalogue",
            &catalogue,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], CONTEXT_CAPABILITIES_VERSION);
}

fn run_atlas(args: &[&str]) -> std::process::Output {
    Command::cargo_bin("atlas")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

fn assert_cli_success(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn serialized_source_byte_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter().map(serialized_source_byte_count).sum(),
        serde_json::Value::Object(fields) => fields
            .iter()
            .map(|(name, value)| {
                if name == "bytes" {
                    value.as_array().map_or(0, Vec::len)
                } else {
                    serialized_source_byte_count(value)
                }
            })
            .sum(),
        _ => 0,
    }
}

fn run_bound_direct_governor(
    root: &str,
    catalogue: &str,
    request_path: &std::path::Path,
    task: &str,
    task_session_id: &str,
) -> serde_json::Value {
    let mut request = governor_request(Route::Direct);
    request.semantic.task = task.to_string();
    request.semantic.declared_kind = Some(TaskKind::BugFix);
    request.semantic.legacy_task_session_id = Some(task_session_id.to_string());
    request.semantic.caller_capabilities.insert(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Satisfied,
    );
    std::fs::write(request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert_cli_success(&run_atlas(&[
        "governor",
        "run",
        root,
        "--request",
        request_path.to_str().unwrap(),
        "--catalogue",
        catalogue,
    ]))
}

#[test]
fn cli_governor_task_and_read_surfaces_delegate_with_stdout_only_json() {
    let fixture = fixture("surface-cli-delegation");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let governor_path = fixture.workspace_directory.path().join("governor.json");
    std::fs::write(
        &governor_path,
        serde_json::to_vec(&governor_request(Route::Direct)).unwrap(),
    )
    .unwrap();
    let governor = run_atlas(&[
        "governor",
        "run",
        root,
        "--request",
        governor_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]);
    let governor = assert_cli_success(&governor);
    assert!(governor["execution"].is_object());
    assert!(governor["materialized_sources"].is_null());

    let start_path = fixture.workspace_directory.path().join("start.json");
    std::fs::write(
        &start_path,
        serde_json::to_vec(&start_request("Exercise additive CLI lifecycle")).unwrap(),
    )
    .unwrap();
    let started = assert_cli_success(&run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        start_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    let session_id = started["task_session_id"].as_str().unwrap().to_string();
    let replayed = assert_cli_success(&run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        start_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(replayed["task_session_id"], session_id);
    assert_eq!(replayed["created_at"], started["created_at"]);
    let shown = assert_cli_success(&run_atlas(&[
        "task",
        "show",
        root,
        &session_id,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(shown["task_session"]["state"], "created");
    let activation_path = fixture.workspace_directory.path().join("activate.json");
    let activation = run_bound_direct_governor(
        root,
        &catalogue,
        &activation_path,
        "Exercise additive CLI lifecycle",
        &session_id,
    );
    assert_eq!(activation["execution"]["state"], "completed");
    assert_eq!(
        activation["execution"]["payload"]["payload_type"],
        "direct_none"
    );
    let shown = assert_cli_success(&run_atlas(&[
        "task",
        "show",
        root,
        &session_id,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(shown["task_session"]["state"], "active");
    let yielded = assert_cli_success(&run_atlas(&[
        "context-yield",
        "show",
        root,
        &session_id,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(yielded["task_session"]["task_session_id"], session_id);
    assert!(yielded["metrics"]["observed_count"].is_number());
    let abandoned = assert_cli_success(&run_atlas(&[
        "task",
        "abandon",
        root,
        &session_id,
        "--reason-code",
        "user_requested",
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(abandoned["outcome_code"], "user_requested");

    let compiled = assert_cli_success(&run_atlas(&[
        "compiled-context",
        "show",
        root,
        "--session-id",
        &session_id,
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(compiled["contexts"], serde_json::json!([]));
    assert_cli_success(&run_atlas(&[
        "serving-status",
        root,
        "--catalogue",
        &catalogue,
    ]));
    assert_cli_success(&run_atlas(&[
        "retention",
        "status",
        root,
        "--catalogue",
        &catalogue,
    ]));
    assert_cli_success(&run_atlas(&[
        "retention",
        "compact",
        root,
        "--dry-run",
        "--catalogue",
        &catalogue,
    ]));

    let second_path = fixture.workspace_directory.path().join("second-start.json");
    std::fs::write(
        &second_path,
        serde_json::to_vec(&start_request("Abandon stale task through CLI")).unwrap(),
    )
    .unwrap();
    let second = assert_cli_success(&run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        second_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    let second_id = second["task_session_id"].as_str().unwrap();
    run_bound_direct_governor(
        root,
        &catalogue,
        &activation_path,
        "Abandon stale task through CLI",
        second_id,
    );
    let inactivity = assert_cli_success(&run_atlas(&[
        "task",
        "abandon",
        root,
        second_id,
        "--reason-code",
        "inactivity_timeout",
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(inactivity["outcome_code"], "inactivity_timeout");

    let complete_path = fixture.workspace_directory.path().join("complete.json");
    std::fs::write(
        &complete_path,
        serde_json::to_vec(&LegacyTaskCompleteRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            accepted: true,
            tests_passed: Some(true),
            outcome_code: "accepted".to_string(),
        })
        .unwrap(),
    )
    .unwrap();
    let third_path = fixture.workspace_directory.path().join("third-start.json");
    std::fs::write(
        &third_path,
        serde_json::to_vec(&start_request("Complete through additive CLI")).unwrap(),
    )
    .unwrap();
    let third = assert_cli_success(&run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        third_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    let third_id = third["task_session_id"].as_str().unwrap();
    run_bound_direct_governor(
        root,
        &catalogue,
        &activation_path,
        "Complete through additive CLI",
        third_id,
    );
    std::fs::write(
        fixture.workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 2 }\n",
    )
    .unwrap();
    discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config).unwrap();
    let completed = assert_cli_success(&run_atlas(&[
        "task",
        "complete",
        root,
        third_id,
        "--request",
        complete_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(completed["state"], "completed");
}

#[test]
fn cli_request_documents_enforce_exact_wire_boundary_and_closed_decode() {
    let fixture = fixture("surface-cli-request-bound");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let prefix = r#"{"context_ir_version":"1.0.0","task":""#;
    let suffix = r#"","declared_kind":null}"#;
    let task_len = MAX_GOVERNOR_REQUEST_BYTES - prefix.len() - suffix.len();
    let exact = format!("{prefix}{}{suffix}", "x".repeat(task_len));
    assert_eq!(exact.len(), MAX_GOVERNOR_REQUEST_BYTES);
    let exact_path = fixture
        .workspace_directory
        .path()
        .join("exact-request.json");
    std::fs::write(&exact_path, exact).unwrap();
    assert_cli_success(&run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        exact_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]));

    let oversized_path = fixture
        .workspace_directory
        .path()
        .join("oversized-request.json");
    std::fs::write(&oversized_path, vec![b' '; MAX_GOVERNOR_REQUEST_BYTES + 1]).unwrap();
    let oversized = run_atlas(&[
        "task",
        "start",
        root,
        "--request",
        oversized_path.to_str().unwrap(),
        "--catalogue",
        &catalogue,
    ]);
    assert!(!oversized.status.success());
    assert!(oversized.stdout.is_empty());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("exceeds 1048576 bytes"));

    for document in [
        br#"{"context_ir_version":"1.0.0","task":"x","declared_kind":null,"unknown":true}"#
            .as_slice(),
        br#"{"context_ir_version":"1.0.0""#.as_slice(),
        br#"{"context_ir_version":"2.0.0","task":"x","declared_kind":null}"#.as_slice(),
    ] {
        let output = Command::cargo_bin("atlas")
            .unwrap()
            .args([
                "task",
                "start",
                root,
                "--request",
                "-",
                "--catalogue",
                &catalogue,
            ])
            .write_stdin(document)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn cli_bounds_confirmations_and_abandon_vocabulary_fail_closed() {
    let fixture = fixture("surface-cli-closed-inputs");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    for reason in [
        "superseded",
        "no_longer_needed",
        "USER_REQUESTED",
        "unknown",
    ] {
        let output = run_atlas(&[
            "task",
            "abandon",
            root,
            "task_missing",
            "--reason-code",
            reason,
            "--catalogue",
            &catalogue,
        ]);
        assert!(!output.status.success(), "{reason}");
        assert!(output.stdout.is_empty(), "{reason}");
    }
    for args in [
        vec![
            "task",
            "show",
            root,
            "task_missing",
            "--limit",
            "0",
            "--catalogue",
            &catalogue,
        ],
        vec![
            "serving-status",
            root,
            "--limit",
            "201",
            "--catalogue",
            &catalogue,
        ],
        vec![
            "retention",
            "status",
            root,
            "--cursor",
            &"x".repeat(MAX_APPLICATION_CURSOR_BYTES + 1),
            "--catalogue",
            &catalogue,
        ],
    ] {
        let output = run_atlas(&args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    for args in [
        vec![
            "retention",
            "compact",
            root,
            "--dry-run",
            "--confirm-manifest",
            "00",
            "--confirm-privacy-deletion",
            "--catalogue",
            &catalogue,
        ],
        vec![
            "unregister",
            root,
            "--dry-run",
            "--confirm-manifest",
            "00",
            "--irreversible",
            "--catalogue",
            &catalogue,
        ],
        vec![
            "governor",
            "run",
            root,
            "--request",
            "-",
            "--materialize-source",
            "--catalogue",
            &catalogue,
        ],
    ] {
        let output = run_atlas(&args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn legacy_v1_error_detail_remains_uncapped() {
    let long_missing_root = "x".repeat(2048);
    let output = run_atlas(&["status", &long_missing_root]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["kind"], "other");
    assert!(error["error"].as_str().unwrap().len() > 1024);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains(&long_missing_root));
}

#[test]
fn additive_cli_does_not_change_explicit_v1_status_bytes() {
    let fixture = fixture("surface-cli-v1-regression");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let before = run_atlas(&["status", root, "--catalogue", &catalogue]);
    assert!(before.status.success());
    assert!(before.stderr.is_empty());
    assert_cli_success(&run_atlas(&[
        "governor",
        "capabilities",
        root,
        "--catalogue",
        &catalogue,
    ]));
    let after = run_atlas(&["status", root, "--catalogue", &catalogue]);
    assert_eq!(before.stdout, after.stdout);
    assert_eq!(before.stderr, after.stderr);
}

#[test]
fn cli_progressive_deep_materialization_is_exact_capped_and_response_lifetime_only() {
    let fixture = fixture("surface-cli-materialization");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let mut request = governor_request(Route::AtlasDeep);
    request.semantic.caller_capabilities = BTreeMap::from([
        (
            RequiredCapability::ExactSource,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ]);
    request.semantic.symbol_targets.clear();
    let request_path = fixture.workspace_directory.path().join("materialize.json");
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let source_before =
        std::fs::read(fixture.workspace_directory.path().join("src/lib.rs")).unwrap();
    let cap = source_before.len().to_string();
    let output = assert_cli_success(&run_atlas(&[
        "governor",
        "run",
        root,
        "--request",
        request_path.to_str().unwrap(),
        "--materialize-source",
        "--max-materialized-bytes",
        &cap,
        "--catalogue",
        &catalogue,
    ]));

    assert_eq!(output["execution"]["final_route"], "ATLAS_DEEP");
    let materialization = &output["execution"]["materialization"];
    assert_eq!(materialization["authorized"], true);
    assert_eq!(materialization["max_bytes"], source_before.len() as u64);
    let sources = materialization["sources"].as_array().unwrap();
    assert!(!sources.is_empty());
    let materialized_bytes = serialized_source_byte_count(&output);
    assert!(materialized_bytes > 0);
    assert!(materialized_bytes <= source_before.len());

    let ir_sources: Vec<&serde_json::Value> = output["deep_context_ir"]["working_set"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source"].as_object().map(|_| &item["source"]))
        .collect();
    for source in sources {
        let start = source["start_byte"].as_u64().unwrap();
        let end = source["end_byte"].as_u64().unwrap();
        assert!(ir_sources.iter().any(|ir_source| {
            ir_source["whole_file_sha256"] == source["source_digest"]
                && ir_source["start_byte"] == start
                && ir_source["end_byte"] == end
                && ir_source["verification_status"] == "verified"
                && ir_source["revalidated_before_seal"] == true
        }));
        assert_eq!(
            source["bytes"],
            serde_json::to_value(&source_before[start as usize..end as usize]).unwrap()
        );
    }
    assert!(
        output.get("materialized_sources").is_none(),
        "source bytes must have one serialized representation"
    );
    assert_eq!(
        std::fs::read(fixture.workspace_directory.path().join("src/lib.rs")).unwrap(),
        source_before
    );
    let persisted_materialization: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM context_use_event WHERE details_json LIKE '%materialized%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_materialization, 0);
}
#[test]
fn cli_identical_content_paths_preserve_positional_source_identity() {
    let fixture = fixture("surface-cli-identical-source-identity");
    let twin_path = fixture.workspace_directory.path().join("src/twin.rs");
    let source_bytes =
        std::fs::read(fixture.workspace_directory.path().join("src/lib.rs")).unwrap();
    std::fs::write(&twin_path, &source_bytes).unwrap();
    discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config).unwrap();
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let mut request = governor_request(Route::AtlasDeep);
    request.semantic.caller_capabilities = BTreeMap::from([
        (
            RequiredCapability::ExactSource,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ]);
    request.semantic.path_targets = vec!["src/lib.rs".into(), "src/twin.rs".into()];
    request.semantic.symbol_targets.clear();
    let request_path = fixture
        .workspace_directory
        .path()
        .join("identical-sources.json");
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let cap = (source_bytes.len() * 2).to_string();

    let output = assert_cli_success(&run_atlas(&[
        "governor",
        "run",
        root,
        "--request",
        request_path.to_str().unwrap(),
        "--materialize-source",
        "--max-materialized-bytes",
        &cap,
        "--catalogue",
        &catalogue,
    ]));

    assert!(
        output.get("materialized_sources").is_none(),
        "an ambiguous duplicate path projection must not be serialized"
    );
    let sources = output["execution"]["materialization"]["sources"]
        .as_array()
        .unwrap();
    let ir_sources: Vec<&serde_json::Value> = output["deep_context_ir"]["working_set"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source"].as_object().map(|_| &item["source"]))
        .collect();
    assert_eq!(sources.len(), 2);
    assert_eq!(ir_sources.len(), sources.len());
    for (source, ir_source) in sources.iter().zip(ir_sources) {
        assert_eq!(source["source_digest"], ir_source["whole_file_sha256"]);
        assert_eq!(source["start_byte"], ir_source["start_byte"]);
        assert_eq!(source["end_byte"], ir_source["end_byte"]);
        let path = ir_source["canonical_path"].as_str().unwrap();
        assert_eq!(
            source["bytes"],
            serde_json::to_value(
                std::fs::read(fixture.workspace_directory.path().join(path)).unwrap()
            )
            .unwrap()
        );
    }
}

#[test]
fn public_application_materialization_self_grants_fail_closed_for_every_route_state() {
    let mut fixture = fixture("surface-application-materialization-route-boundary");

    let mut direct = governor_request(Route::Direct);
    direct.semantic.caller_capabilities.insert(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Satisfied,
    );
    direct.semantic.materialize_source = true;
    direct.semantic.max_materialized_bytes = Some(1024);

    let mut light = governor_request(Route::AtlasLight);
    light.semantic.materialize_source = true;
    light.semantic.max_materialized_bytes = Some(1024);

    let mut blocked = governor_request(Route::AtlasLight);
    blocked.semantic.path_targets = vec!["src/missing.rs".into()];
    blocked.semantic.symbol_targets.clear();
    blocked.semantic.materialize_source = true;
    blocked.semantic.max_materialized_bytes = Some(1024);

    for request in [direct, light, blocked] {
        let error = run_catalogue_governor(request, &mut fixture.connection, &fixture.workspace)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("public governor request cannot grant source materialization authority"),
            "{error}"
        );
    }
}
#[test]
fn public_governor_request_cannot_self_grant_deep_source_materialization() {
    let mut fixture = fixture("surface-public-materialization-self-grant");
    let mut request = governor_request(Route::AtlasDeep);
    request.semantic.caller_capabilities = BTreeMap::from([
        (
            RequiredCapability::ExactSource,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::RequiredRoleClosure,
            CallerCapabilityState::Unsatisfied,
        ),
    ]);
    request.semantic.symbol_targets.clear();
    request.semantic.materialize_source = true;
    request.semantic.max_materialized_bytes = Some(1024);

    let error =
        run_catalogue_governor(request, &mut fixture.connection, &fixture.workspace).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("public governor request cannot grant source materialization authority"),
        "{error}"
    );
}

#[test]
fn cli_materialization_pair_bounds_and_document_authority_fail_before_source_bytes() {
    let fixture = fixture("surface-cli-materialization-authority");
    let root = fixture.workspace_directory.path().to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let request_path = fixture.workspace_directory.path().join("request.json");
    let request = governor_request(Route::AtlasDeep);
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let source_path = fixture.workspace_directory.path().join("src/lib.rs");
    let source_before = std::fs::read(&source_path).unwrap();
    let above_envelope =
        (workspace_atlas::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES + 1).to_string();

    for extra in [
        &["--materialize-source"][..],
        &["--max-materialized-bytes", "1"][..],
        &["--materialize-source", "--max-materialized-bytes", "0"][..],
        &[
            "--materialize-source",
            "--max-materialized-bytes",
            above_envelope.as_str(),
        ][..],
        &[
            "--materialize-source",
            "--max-materialized-bytes",
            "not-a-number",
        ][..],
    ] {
        let mut args = vec![
            "governor",
            "run",
            root,
            "--request",
            request_path.to_str().unwrap(),
            "--catalogue",
            &catalogue,
        ];
        args.extend_from_slice(extra);
        let output = run_atlas(&args);
        assert!(!output.status.success(), "{extra:?}");
        assert!(output.stdout.is_empty(), "{extra:?}");
    }

    let mut self_authorizing = request;
    self_authorizing.semantic.materialize_source = true;
    self_authorizing.semantic.max_materialized_bytes = Some(1);
    let self_authorizing_path = fixture
        .workspace_directory
        .path()
        .join("self-authorizing.json");
    std::fs::write(
        &self_authorizing_path,
        serde_json::to_vec(&self_authorizing).unwrap(),
    )
    .unwrap();
    let rejected = run_atlas(&[
        "governor",
        "run",
        root,
        "--request",
        self_authorizing_path.to_str().unwrap(),
        "--materialize-source",
        "--max-materialized-bytes",
        "1",
        "--catalogue",
        &catalogue,
    ]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("request document cannot grant CLI materialization authority"));
    assert_eq!(std::fs::read(source_path).unwrap(), source_before);
}

#[test]
fn compact_and_unregister_use_exact_confirmations_without_touching_source() {
    let fixture = fixture("surface-cli-destructive-boundaries");
    let workspace_root = fixture.workspace_directory.path().canonicalize().unwrap();
    let root = workspace_root.to_str().unwrap();
    let catalogue = fixture.connection.path().unwrap().to_string();
    let source = workspace_root.join("src/lib.rs");
    let source_before = std::fs::read(&source).unwrap();

    let compact_preview = assert_cli_success(&run_atlas(&[
        "retention",
        "compact",
        root,
        "--dry-run",
        "--catalogue",
        &catalogue,
    ]));
    let compact_digest = compact_preview["manifest_digest"].as_str().unwrap();
    let long_wrong_digest = "é".repeat(700);
    let compact_mismatch = run_atlas(&[
        "retention",
        "compact",
        root,
        "--confirm-manifest",
        &long_wrong_digest,
        "--confirm-privacy-deletion",
        "--catalogue",
        &catalogue,
    ]);
    assert!(!compact_mismatch.status.success());
    assert!(compact_mismatch.stdout.is_empty());
    let compact_error: serde_json::Value =
        serde_json::from_slice(&compact_mismatch.stderr).unwrap();
    assert_eq!(compact_error["kind"], "manifest_mismatch");
    assert!(compact_error["error"].as_str().unwrap().len() <= 1024);
    let compact_apply = assert_cli_success(&run_atlas(&[
        "retention",
        "compact",
        root,
        "--confirm-manifest",
        compact_digest,
        "--confirm-privacy-deletion",
        "--catalogue",
        &catalogue,
    ]));
    assert_eq!(compact_apply["deleted_sessions"], 0);
    assert_eq!(std::fs::read(&source).unwrap(), source_before);

    let app_data = tempfile::tempdir().unwrap();
    let app_data_root = app_data.path().canonicalize().unwrap();
    let unregister_workspace = tempfile::tempdir().unwrap();
    let unregister_root = unregister_workspace.path().canonicalize().unwrap();
    let unregister_root = unregister_root.to_str().unwrap();
    let unregister_source = std::path::Path::new(unregister_root).join("source.rs");
    std::fs::write(&unregister_source, "pub fn retained() {}\n").unwrap();
    let unregister_source_before = std::fs::read(&unregister_source).unwrap();
    let initialized = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args(["init", unregister_root])
        .output()
        .unwrap();
    let initialized = assert_cli_success(&initialized);
    let unregister_catalogue = initialized["catalogue_path"].as_str().unwrap().to_string();
    let reconciled = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args(["reconcile", unregister_root])
        .output()
        .unwrap();
    assert_cli_success(&reconciled);
    let preview = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args(["unregister", unregister_root, "--dry-run", "--limit", "1"])
        .output()
        .unwrap();
    let preview = assert_cli_success(&preview);
    let digest = preview["manifest_digest"].as_str().unwrap();
    let cursor = preview["next_cursor"].as_str().unwrap().to_string();
    assert!(cursor.len() <= MAX_APPLICATION_CURSOR_BYTES);
    let continued = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args([
            "unregister",
            unregister_root,
            "--dry-run",
            "--limit",
            "1",
            "--cursor",
            &cursor,
        ])
        .output()
        .unwrap();
    let continued = assert_cli_success(&continued);
    assert_eq!(continued["manifest_digest"], digest);
    let forged_ordering = forge_application_cursor(&cursor, |payload| {
        let mut ordering: serde_json::Value =
            serde_json::from_str(payload["ordering_key"].as_str().unwrap()).unwrap();
        ordering["path_hash"] = "00".into();
        payload["ordering_key"] = serde_json::to_string(&ordering).unwrap().into();
    });
    let forged = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args([
            "unregister",
            unregister_root,
            "--dry-run",
            "--cursor",
            &forged_ordering,
        ])
        .output()
        .unwrap();
    assert!(!forged.status.success());
    assert!(forged.stdout.is_empty());
    let forged_error: serde_json::Value = serde_json::from_slice(&forged.stderr).unwrap();
    assert_eq!(forged_error["kind"], "cursor_stale");
    let mut tampered = cursor.clone().into_bytes();
    tampered[12] ^= 1;
    let tampered = String::from_utf8(tampered).unwrap();
    let rejected = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args([
            "unregister",
            unregister_root,
            "--dry-run",
            "--cursor",
            &tampered,
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("\"kind\": \"cursor_invalid\""));
    for args in [
        vec!["retention", "status", unregister_root, "--cursor", &cursor],
        vec!["serving-status", unregister_root, "--cursor", &cursor],
    ] {
        let rejected = Command::cargo_bin("atlas")
            .unwrap()
            .env("LOCALAPPDATA", &app_data_root)
            .args(args)
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(rejected.stdout.is_empty());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("cursor_invalid"));
    }
    let wrong_workspace = run_atlas(&[
        "unregister",
        root,
        "--dry-run",
        "--cursor",
        &cursor,
        "--catalogue",
        &catalogue,
    ]);
    assert!(!wrong_workspace.status.success());
    assert!(wrong_workspace.stdout.is_empty());
    assert!(preview["disclosure"]
        .as_str()
        .unwrap()
        .contains("Workspace source"));
    let apply = Command::cargo_bin("atlas")
        .unwrap()
        .env("LOCALAPPDATA", &app_data_root)
        .args([
            "unregister",
            unregister_root,
            "--confirm-manifest",
            digest,
            "--irreversible",
        ])
        .output()
        .unwrap();
    assert!(!apply.status.success());
    assert!(apply.stdout.is_empty());
    let apply_error: serde_json::Value = serde_json::from_slice(&apply.stderr).unwrap();
    assert_eq!(apply_error["kind"], "unregister_unavailable");
    assert_eq!(
        std::fs::read(&unregister_source).unwrap(),
        unregister_source_before
    );
    assert!(std::path::Path::new(&unregister_catalogue).exists());
    assert!(!std::path::Path::new(&format!("{unregister_catalogue}.bak")).exists());
}
