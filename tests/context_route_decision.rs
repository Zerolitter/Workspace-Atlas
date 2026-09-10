use std::collections::BTreeMap;

use workspace_atlas::context_ir::TaskKind;
use workspace_atlas::context_metrics::{
    ContextExecutionCounters, MAX_CONTEXT_EXECUTION_SOURCE_BYTES,
};
use workspace_atlas::context_route::{
    decide_context_route, discover_context_capabilities, negotiate_context_versions, AtlasIntent,
    AttemptState, Availability, CallerCapabilityState, CapabilityDeficitReason, CapabilityFeature,
    ContextExecution, ContextPayload, ContextRouteError, ContextRouteRequest, DeepContractVersions,
    DeepResultReference, DeepSemanticBudget, ExecutionAttempt, ExecutionState,
    ExplicitMaterialization, GenerationObservation, GenerationUnavailableReason,
    InterruptionReason, LightOperation, LightOperationResult, MaterializedSource,
    NegotiationRequest, RequiredCapability, Route, RouteDeficit, RouteFailureReason, RouteReason,
    RouteSignal, SeedSummary, VersionedOperation, CONTEXT_CAPABILITIES_VERSION,
    CONTEXT_EXECUTION_VERSION, CONTEXT_ROUTE_POLICY_VERSION, MAX_MATERIALIZED_SOURCES,
};

const FIXTURE: &str = include_str!("fixtures/context_route/decision-v1.example.json");

fn budget() -> DeepSemanticBudget {
    DeepSemanticBudget::new(50, 65_536, 16_384, 4, 10_000, 12).unwrap()
}

fn base_request() -> ContextRouteRequest {
    ContextRouteRequest {
        schema_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        normalized_task_hash: "11".repeat(32),
        declared_kind: Some(TaskKind::BugFix),
        classifier_kind: TaskKind::BugFix,
        classifier_version: "task-classifier-v1.0.0".to_string(),
        explicit_seeds: SeedSummary {
            digest: "22".repeat(32),
            path_count: 0,
            symbol_count: 0,
        },
        caller_capabilities: BTreeMap::from([
            (
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Satisfied,
            ),
            (
                RequiredCapability::ExactSource,
                CallerCapabilityState::Satisfied,
            ),
        ]),
        atlas_intent: AtlasIntent::Allow,
        route_floor: Route::Direct,
        route_ceiling: Route::AtlasDeep,
        capability_profile: None,
        cost_profile: None,
        profile_registry_digest: None,
        starting_generation: GenerationObservation::Observed {
            generation_id: "gen_01JTEST".to_string(),
        },
        light_operations: vec![
            VersionedOperation::new(LightOperation::Query),
            VersionedOperation::new(LightOperation::SourceReference),
        ],
        deep_contracts: DeepContractVersions::fixed(),
        deep_budget: Some(budget()),
    }
}

#[test]
fn direct_decision_is_deterministic_and_has_a_frozen_canonical_hash() {
    let first = decide_context_route(base_request()).unwrap();
    let second = decide_context_route(base_request()).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.initial_route, Route::Direct);
    assert_eq!(first.effective_floor, Route::Direct);
    assert_eq!(first.effective_ceiling, Route::AtlasDeep);
    assert_eq!(first.reasons, vec![RouteReason::CallerContextSufficient]);
    assert!(first
        .signals
        .contains(&RouteSignal::StartingGenerationObserved));
    assert_eq!(
        first.decision_hash,
        "4f31f412f2b6bf8c4a492f97a0b0dade7a8cc523da1ac4ef73548529be18ee2d"
    );
}

#[test]
fn precedence_applies_intent_then_floor_without_capability_route_overrides() {
    let mut none = base_request();
    none.atlas_intent = AtlasIntent::None;
    none.route_ceiling = Route::Direct;
    none.deep_budget = None;
    let none = decide_context_route(none).unwrap();
    assert_eq!(
        (
            none.effective_floor,
            none.effective_ceiling,
            none.initial_route
        ),
        (Route::Direct, Route::Direct, Route::Direct)
    );
    assert!(none.reasons.contains(&RouteReason::AtlasNotRequested));

    let mut required = base_request();
    required.atlas_intent = AtlasIntent::Require;
    required.route_ceiling = Route::AtlasLight;
    required.deep_budget = None;
    let required = decide_context_route(required).unwrap();
    assert_eq!(required.initial_route, Route::AtlasLight);
    assert!(required
        .reasons
        .contains(&RouteReason::ExplicitAtlasRequest));

    let mut deep_capability = base_request();
    deep_capability.caller_capabilities.insert(
        RequiredCapability::ValidationPlan,
        CallerCapabilityState::Unsatisfied,
    );
    let deep_capability = decide_context_route(deep_capability).unwrap();
    assert_eq!(deep_capability.initial_route, Route::Direct);
    assert!(deep_capability
        .reasons
        .contains(&RouteReason::DeepCapabilityRequired));
}

#[test]
fn invalid_floor_ceiling_intent_budget_versions_and_profiles_fail_closed() {
    let mut inverted = base_request();
    inverted.route_floor = Route::AtlasDeep;
    inverted.route_ceiling = Route::AtlasLight;
    assert_eq!(
        decide_context_route(inverted).unwrap_err(),
        ContextRouteError::FloorAboveCeiling
    );

    let mut forbidden = base_request();
    forbidden.atlas_intent = AtlasIntent::None;
    forbidden.route_floor = Route::AtlasLight;
    assert_eq!(
        decide_context_route(forbidden).unwrap_err(),
        ContextRouteError::IntentFloorConflict
    );

    let mut missing_budget = base_request();
    missing_budget.deep_budget = None;
    assert_eq!(
        decide_context_route(missing_budget).unwrap_err(),
        ContextRouteError::DeepBudgetRequired
    );

    let mut unknown_version = base_request();
    unknown_version.schema_version = "context-route-v9.0.0".to_string();
    assert!(matches!(
        decide_context_route(unknown_version),
        Err(ContextRouteError::UnsupportedVersion { .. })
    ));

    let mut unknown_profile = base_request();
    unknown_profile.capability_profile = Some(workspace_atlas::context_route::ProfileIdentity {
        profile_id: "unregistered".to_string(),
        profile_version: "1.0.0".to_string(),
        manifest_digest: "33".repeat(32),
    });
    unknown_profile.profile_registry_digest = Some("44".repeat(32));
    assert_eq!(
        decide_context_route(unknown_profile).unwrap_err(),
        ContextRouteError::UnknownProfile
    );
}

#[test]
fn v1_route_request_and_removed_packet_vocabulary_fail_closed() {
    let mut v1 = base_request();
    v1.schema_version = "context-route-v1.0.0".to_string();
    assert!(matches!(
        decide_context_route(v1),
        Err(ContextRouteError::UnsupportedVersion { contract, supplied })
            if contract == "route" && supplied == "context-route-v1.0.0"
    ));

    let packet = serde_json::json!({
        "operation": "context_packet",
        "operation_version": "context-packet-v1.0.0",
        "result_digest": "cc".repeat(32),
        "item_count": 1
    });
    assert!(serde_json::from_value::<LightOperationResult>(packet).is_err());
}

#[test]
fn direct_success_supports_observed_or_typed_unavailable_generation_and_no_payload() {
    let observed = decide_context_route(base_request()).unwrap();
    let completed = ContextExecution::completed(
        observed,
        vec![ExecutionAttempt::completed(
            "attempt-1",
            Route::Direct,
            ContextExecutionCounters::default(),
        )],
        ContextPayload::DirectNone {},
        ContextExecutionCounters::default(),
        vec![],
        None,
    )
    .unwrap();
    assert_eq!(completed.state(), ExecutionState::Completed);
    assert_eq!(completed.payload(), Some(&ContextPayload::DirectNone {}));

    let mut unavailable = base_request();
    unavailable.starting_generation = GenerationObservation::Unavailable {
        reason: GenerationUnavailableReason::WorkspaceUnregistered,
    };
    let unavailable = decide_context_route(unavailable).unwrap();
    assert!(unavailable
        .reasons
        .contains(&RouteReason::StartingGenerationUnavailable));
    assert!(unavailable
        .signals
        .contains(&RouteSignal::StartingGenerationUnavailable));
}

#[test]
fn light_payload_is_a_closed_two_operation_union_in_deterministic_order() {
    let payload = ContextPayload::LightBundle {
        operations: vec![
            LightOperationResult::Query {
                operation_version: "query-v1.0.0".into(),
                result_digest: "aa".repeat(32),
                record_count: 2,
            },
            LightOperationResult::SourceReference {
                operation_version: "source-reference-v1.0.0".into(),
                result_digest: "bb".repeat(32),
                reference_count: 1,
            },
        ],
    };
    payload.validate().unwrap();

    let reversed = ContextPayload::LightBundle {
        operations: vec![
            LightOperationResult::SourceReference {
                operation_version: "source-reference-v1.0.0".into(),
                result_digest: "bb".repeat(32),
                reference_count: 1,
            },
            LightOperationResult::Query {
                operation_version: "query-v1.0.0".into(),
                result_digest: "aa".repeat(32),
                record_count: 2,
            },
        ],
    };
    assert!(reversed.validate().is_err());
}

#[test]
fn execution_state_invariants_separate_attempts_and_final_route_from_decision() {
    let decision = decide_context_route(base_request()).unwrap();
    let deficit = RouteDeficit::Capability {
        capability: RequiredCapability::ExactSource,
        reason: CapabilityDeficitReason::Stale,
    };
    assert!(ContextExecution::partial(
        decision.clone(),
        vec![],
        Route::Direct,
        ContextPayload::DirectNone {},
        vec![],
        vec![deficit.clone()],
        ContextExecutionCounters::default(),
        vec![],
        None,
        workspace_atlas::context_route::TerminalReason::RouteCeilingReached,
    )
    .is_err());
    assert!(ContextExecution::blocked(
        decision,
        vec![],
        None,
        None,
        vec![],
        vec![RouteDeficit::Route {
            reason: RouteFailureReason::NoActiveGeneration,
        }],
        ContextExecutionCounters::default(),
        vec![],
        None,
        workspace_atlas::context_route::TerminalReason::GenerationUnavailable,
    )
    .is_ok());
}

#[test]
fn route_ceiling_distinguishes_useful_partial_from_payloadless_blocked() {
    let mut request = base_request();
    request.route_ceiling = Route::AtlasLight;
    request.deep_budget = None;
    request.caller_capabilities.insert(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    );
    let decision = decide_context_route(request).unwrap();
    let deficit = RouteDeficit::Capability {
        capability: RequiredCapability::ExactSource,
        reason: CapabilityDeficitReason::Stale,
    };
    let attempts = vec![
        ExecutionAttempt {
            attempt_id: "direct-1".into(),
            route: Route::Direct,
            state: AttemptState::Partial,
            deficits: vec![deficit.clone()],
            counters: ContextExecutionCounters::default(),
            elapsed_micros: 5,
        },
        ExecutionAttempt {
            attempt_id: "light-1".into(),
            route: Route::AtlasLight,
            state: AttemptState::Partial,
            deficits: vec![deficit.clone()],
            counters: ContextExecutionCounters {
                atlas_calls: 1,
                records: 1,
                ..ContextExecutionCounters::default()
            },
            elapsed_micros: 9,
        },
    ];
    let payload = ContextPayload::LightBundle {
        operations: vec![LightOperationResult::Query {
            operation_version: "query-v1.0.0".into(),
            result_digest: "aa".repeat(32),
            record_count: 1,
        }],
    };
    assert!(ContextExecution::partial(
        decision,
        attempts,
        Route::AtlasLight,
        payload,
        vec![RequiredCapability::IdentityLookup],
        vec![deficit],
        ContextExecutionCounters {
            atlas_calls: 1,
            records: 1,
            ..ContextExecutionCounters::default()
        },
        vec![],
        None,
        workspace_atlas::context_route::TerminalReason::RouteCeilingReached,
    )
    .is_ok());
}

#[test]
fn light_to_deep_progression_accepts_only_disclosed_semantic_deficits() {
    let mut request = base_request();
    request.route_floor = Route::AtlasLight;
    request.caller_capabilities.insert(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    );
    let decision = decide_context_route(request).unwrap();
    let light_counters = ContextExecutionCounters {
        atlas_calls: 1,
        records: 1,
        ..ContextExecutionCounters::default()
    };
    let deep_counters = ContextExecutionCounters {
        atlas_calls: 1,
        records: 1,
        work_units: 1,
        ..ContextExecutionCounters::default()
    };
    let attempts = vec![
        ExecutionAttempt {
            attempt_id: "light-1".into(),
            route: Route::AtlasLight,
            state: AttemptState::Partial,
            deficits: vec![RouteDeficit::Capability {
                capability: RequiredCapability::ExactSource,
                reason: CapabilityDeficitReason::Stale,
            }],
            counters: light_counters,
            elapsed_micros: 1,
        },
        ExecutionAttempt::completed("deep-1", Route::AtlasDeep, deep_counters),
    ];
    assert!(ContextExecution::completed(
        decision,
        attempts,
        ContextPayload::DeepContextIr {
            result: DeepResultReference {
                context_ir_version: "2.0.0".into(),
                context_hash: "dd".repeat(32),
            },
        },
        ContextExecutionCounters {
            atlas_calls: 2,
            records: 2,
            work_units: 1,
            ..ContextExecutionCounters::default()
        },
        vec![],
        None,
    )
    .is_err());
}

#[test]
fn interruption_keeps_only_prior_completed_payload_and_noncanonical_timing() {
    let decision = decide_context_route(base_request()).unwrap();
    let execution = ContextExecution::interrupted(
        decision,
        vec![ExecutionAttempt::completed(
            "direct-1",
            Route::Direct,
            ContextExecutionCounters::default(),
        )],
        Some(ContextPayload::DirectNone {}),
        vec![],
        ContextExecutionCounters::default(),
        vec![],
        None,
        InterruptionReason::DeadlineExceeded,
    )
    .unwrap();
    assert_eq!(execution.state(), ExecutionState::Interrupted);
    assert_eq!(execution.payload(), Some(&ContextPayload::DirectNone {}));

    let mut request = base_request();
    request.route_floor = Route::AtlasLight;
    request.caller_capabilities.insert(
        RequiredCapability::ValidationPlan,
        CallerCapabilityState::Unsatisfied,
    );
    let decision = decide_context_route(request).unwrap();
    let counters = ContextExecutionCounters {
        atlas_calls: 1,
        records: 1,
        ..ContextExecutionCounters::default()
    };
    let partial_attempt = ExecutionAttempt {
        attempt_id: "light-1".into(),
        route: Route::AtlasLight,
        state: AttemptState::Partial,
        deficits: vec![RouteDeficit::Capability {
            capability: RequiredCapability::ValidationPlan,
            reason: CapabilityDeficitReason::Unavailable,
        }],
        counters,
        elapsed_micros: 4,
    };
    let prior_payload = ContextPayload::LightBundle {
        operations: vec![LightOperationResult::Query {
            operation_version: "query-v1.0.0".into(),
            result_digest: "aa".repeat(32),
            record_count: 1,
        }],
    };
    assert!(ContextExecution::interrupted(
        decision,
        vec![partial_attempt],
        Some(prior_payload),
        vec![RequiredCapability::IdentityLookup],
        counters,
        vec![],
        None,
        InterruptionReason::Cancelled,
    )
    .is_ok());
}

#[test]
fn materialization_requires_explicit_authority_and_byte_bounds() {
    let mut request = base_request();
    request.route_floor = Route::AtlasLight;
    let decision = decide_context_route(request).unwrap();
    let payload = ContextPayload::LightBundle {
        operations: vec![LightOperationResult::SourceReference {
            operation_version: "source-reference-v1.0.0".into(),
            result_digest: "bb".repeat(32),
            reference_count: 1,
        }],
    };
    let source = MaterializedSource {
        source_digest: "cc".repeat(32),
        start_byte: 0,
        end_byte: 2,
        bytes: vec![b'o', b'k'],
    };
    let valid = ContextExecution::completed(
        decision.clone(),
        vec![ExecutionAttempt::completed(
            "light-1",
            Route::AtlasLight,
            ContextExecutionCounters::default(),
        )],
        payload.clone(),
        ContextExecutionCounters::default(),
        vec![],
        Some(ExplicitMaterialization {
            authorized: true,
            max_bytes: 2,
            sources: vec![source.clone()],
        }),
    )
    .unwrap();
    assert_eq!(valid.state(), ExecutionState::Completed);

    assert!(ContextExecution::completed(
        decision,
        vec![ExecutionAttempt::completed(
            "light-1",
            Route::AtlasLight,
            ContextExecutionCounters::default(),
        )],
        payload,
        ContextExecutionCounters::default(),
        vec![],
        Some(ExplicitMaterialization {
            authorized: false,
            max_bytes: 2,
            sources: vec![source],
        }),
    )
    .is_err());
}

#[test]
fn execution_rejects_direct_work_incomplete_success_and_atlas_without_generation() {
    let mut incomplete = base_request();
    incomplete.route_ceiling = Route::Direct;
    incomplete.deep_budget = None;
    incomplete.caller_capabilities.insert(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    );
    let incomplete = decide_context_route(incomplete).unwrap();
    assert!(ContextExecution::completed(
        incomplete,
        vec![ExecutionAttempt::completed(
            "direct-1",
            Route::Direct,
            ContextExecutionCounters::default(),
        )],
        ContextPayload::DirectNone {},
        ContextExecutionCounters::default(),
        vec![],
        None,
    )
    .is_err());

    let decision = decide_context_route(base_request()).unwrap();
    let direct_work = ContextExecutionCounters {
        atlas_calls: 1,
        records: 1,
        ..ContextExecutionCounters::default()
    };
    assert!(ContextExecution::completed(
        decision,
        vec![ExecutionAttempt::completed(
            "direct-1",
            Route::Direct,
            direct_work,
        )],
        ContextPayload::DirectNone {},
        direct_work,
        vec![],
        None,
    )
    .is_err());

    let mut unavailable = base_request();
    unavailable.route_floor = Route::AtlasLight;
    unavailable.route_ceiling = Route::AtlasLight;
    unavailable.deep_budget = None;
    unavailable.starting_generation = GenerationObservation::Unavailable {
        reason: GenerationUnavailableReason::CatalogueUnavailable,
    };
    let unavailable = decide_context_route(unavailable).unwrap();
    let atlas_counters = ContextExecutionCounters {
        atlas_calls: 1,
        records: 1,
        ..ContextExecutionCounters::default()
    };
    assert!(ContextExecution::completed(
        unavailable,
        vec![ExecutionAttempt::completed(
            "light-1",
            Route::AtlasLight,
            atlas_counters,
        )],
        ContextPayload::LightBundle {
            operations: vec![LightOperationResult::Query {
                operation_version: "query-v1.0.0".into(),
                result_digest: "aa".repeat(32),
                record_count: 1,
            }],
        },
        atlas_counters,
        vec![],
        None,
    )
    .is_err());
}

#[test]
fn deep_payload_is_only_a_stable_result_reference() {
    let payload = ContextPayload::DeepContextIr {
        result: DeepResultReference {
            context_ir_version: "2.0.0".to_string(),
            context_hash: "dd".repeat(32),
        },
    };
    payload.validate().unwrap();
    let json = serde_json::to_string(&payload).unwrap();
    assert!(!json.contains("source_bytes"));
    assert!(!json.contains("materialized"));
}

#[test]
fn capability_discovery_and_negotiation_are_privacy_bounded_and_fail_closed() {
    let capabilities = discover_context_capabilities();
    capabilities.validate().unwrap();
    assert_eq!(capabilities.schema_version, "context-capabilities-v2.0.0");
    assert_eq!(
        capabilities.supported_operation_versions,
        vec!["query-v1.0.0", "source-reference-v1.0.0"]
    );
    assert_eq!(
        capabilities.availability,
        BTreeMap::from([
            (CapabilityFeature::RouteDecision, Availability::Available),
            (CapabilityFeature::LightPayload, Availability::Available),
            (
                CapabilityFeature::ProgressiveExecution,
                Availability::Available,
            ),
            (CapabilityFeature::DeepContextIr, Availability::Available),
            (CapabilityFeature::Materialization, Availability::Available,),
        ])
    );
    assert_eq!(
        capabilities.bounds.max_materialized_sources,
        MAX_MATERIALIZED_SOURCES
    );
    assert_eq!(
        capabilities.bounds.max_materialized_bytes,
        MAX_CONTEXT_EXECUTION_SOURCE_BYTES
    );
    let json = serde_json::to_string(&capabilities).unwrap();
    for prohibited in [
        "provider",
        "account",
        "repository",
        "path",
        "vendor",
        "source_body",
        "source_content",
        "context-packet",
    ] {
        assert!(!json.contains(prohibited));
    }

    let negotiated = negotiate_context_versions(&NegotiationRequest {
        capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_string()],
        route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
        ir_versions: vec!["2.0.0".to_string()],
        operation_versions: vec![
            "query-v1.0.0".to_string(),
            "source-reference-v1.0.0".to_string(),
        ],
    })
    .unwrap();
    assert_eq!(negotiated.route_version, "context-route-v2.0.0");
    assert_eq!(negotiated.execution_version, "context-execution-v2.0.0");

    let error = negotiate_context_versions(&NegotiationRequest {
        capability_versions: vec!["context-capabilities-v1.0.0".into()],
        route_versions: vec!["context-route-v1.0.0".into()],
        decision_versions: vec!["context-route-v1.0.0".into()],
        execution_versions: vec!["context-execution-v1.0.0".into()],
        ir_versions: vec!["2.0.0".into()],
        operation_versions: vec![
            "query-v1.0.0".into(),
            "source-reference-v1.0.0".into(),
            "context-packet-v1.0.0".into(),
        ],
    })
    .unwrap_err();
    assert!(matches!(error, ContextRouteError::NoCommonVersion { .. }));
}

#[test]
fn unknown_enum_vocabulary_and_unknown_fields_are_rejected_by_serde() {
    let mut value = serde_json::to_value(base_request()).unwrap();
    value["atlas_intent"] = "adaptive".into();
    assert!(serde_json::from_value::<ContextRouteRequest>(value).is_err());

    let mut value = serde_json::to_value(base_request()).unwrap();
    value["raw_task"] = "must not be retained".into();
    assert!(serde_json::from_value::<ContextRouteRequest>(value).is_err());
    let mut payload = serde_json::to_value(ContextPayload::DirectNone {}).unwrap();
    payload["future_semantics"] = true.into();
    assert!(serde_json::from_value::<ContextPayload>(payload).is_err());
}

#[test]
fn unreleased_v1_execution_fixture_is_retained_only_as_rejected_input() {
    let fixture_value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    assert!(serde_json::from_value::<ContextExecution>(fixture_value).is_err());
}

#[test]
fn v2_execution_serialization_validates_and_round_trips_exactly() {
    let decision = decide_context_route(base_request()).unwrap();
    let execution = ContextExecution::completed(
        decision,
        vec![ExecutionAttempt::completed(
            "attempt-v2",
            Route::Direct,
            ContextExecutionCounters::default(),
        )],
        ContextPayload::DirectNone {},
        ContextExecutionCounters::default(),
        vec![],
        None,
    )
    .unwrap();
    let value = serde_json::to_value(&execution).unwrap();
    assert_eq!(value["schema_version"], "context-execution-v2.0.0");
    assert_eq!(value["decision"]["schema_version"], "context-route-v2.0.0");
    let decoded: ContextExecution = serde_json::from_value(value).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, execution);
}
