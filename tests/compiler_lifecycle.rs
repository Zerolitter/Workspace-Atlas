use std::collections::VecDeque;
use std::time::Duration;

use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_application::dispatch_context_application;
use workspace_atlas::context_ir::{
    deep_context_payload, read_deep_context_ir_v2, RawTaskRetention, TaskKind,
    CONTEXT_SCHEMA_V2_VERSION,
};
use workspace_atlas::context_metrics::ContextExecutionCounters;
use workspace_atlas::context_route::{
    AtlasIntent, CallerCapabilityState, CapabilityDeficitReason, ContextDiagnosticCode,
    ContextExecution, ContextPayload, ContextRouteDecision, ContextRouteDispatcher,
    ContextRouteError, ContextRouteRequest, DeepContractVersions, DeepSemanticBudget,
    GenerationObservation, LightOperation, LightOperationResult, RequiredCapability, Route,
    RouteAttemptOutcome, RouteDeficit, RouteFailureReason, SeedSummary, TerminalReason,
    VersionedOperation, CONTEXT_ROUTE_POLICY_VERSION,
};
use workspace_atlas::task_session::create_task_session;
use workspace_atlas::workspace::register_workspace;

fn budget() -> DeepSemanticBudget {
    DeepSemanticBudget::new(50, 65_536, 16_384, 4, 10_000, 12).unwrap()
}

fn request(
    capabilities: &[(RequiredCapability, CallerCapabilityState)],
    floor: Route,
    ceiling: Route,
    generation_id: &str,
) -> ContextRouteRequest {
    ContextRouteRequest {
        schema_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        normalized_task_hash: "11".repeat(32),
        declared_kind: Some(TaskKind::BugFix),
        classifier_kind: TaskKind::BugFix,
        classifier_version: "task-classifier-v1.0.0".to_string(),
        explicit_seeds: SeedSummary {
            digest: "22".repeat(32),
            path_count: 1,
            symbol_count: 0,
        },
        caller_capabilities: capabilities.iter().copied().collect(),
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

fn light_payload() -> ContextPayload {
    ContextPayload::LightBundle {
        operations: vec![LightOperationResult::Query {
            operation_version: "query-v1.0.0".to_string(),
            result_digest: "33".repeat(32),
            record_count: 1,
        }],
    }
}

fn counters(atlas_calls: u64, records: u64) -> ContextExecutionCounters {
    ContextExecutionCounters {
        atlas_calls,
        records,
        source_bytes: 0,
        estimated_tokens: 0,
        work_units: records,
    }
}

fn completed(payload: ContextPayload, counters: ContextExecutionCounters) -> RouteAttemptOutcome {
    RouteAttemptOutcome::Completed {
        payload,
        counters,
        diagnostics: vec![],
        materialization: None,
    }
}

fn deficient(
    payload: Option<ContextPayload>,
    satisfied_capabilities: Vec<RequiredCapability>,
    deficits: Vec<RouteDeficit>,
) -> RouteAttemptOutcome {
    RouteAttemptOutcome::Deficient {
        payload,
        satisfied_capabilities,
        deficits,
        counters: counters(1, 1),
        diagnostics: vec![],
        materialization: None,
    }
}

#[derive(Default)]
struct FakeDispatcher {
    generation_observations: VecDeque<GenerationObservation>,
    light_outcomes: VecDeque<RouteAttemptOutcome>,
    deep_outcomes: VecDeque<RouteAttemptOutcome>,
    observed_light_operations: Vec<Vec<VersionedOperation>>,
    observed_deep_contracts: Vec<DeepContractVersions>,
    observed_deep_budgets: Vec<DeepSemanticBudget>,
    light_delay: Duration,
    deep_delay: Duration,
}

impl ContextRouteDispatcher for FakeDispatcher {
    type Error = ContextRouteError;

    fn observe_generation(&mut self) -> GenerationObservation {
        self.generation_observations
            .pop_front()
            .unwrap_or(GenerationObservation::Unavailable {
                reason:
                    workspace_atlas::context_route::GenerationUnavailableReason::ObservationFailed,
            })
    }

    fn dispatch_light(
        &mut self,
        _decision: &ContextRouteDecision,
        operations: &[VersionedOperation],
    ) -> Result<RouteAttemptOutcome, Self::Error> {
        if !self.light_delay.is_zero() {
            std::thread::sleep(self.light_delay);
        }
        self.observed_light_operations.push(operations.to_vec());
        self.light_outcomes
            .pop_front()
            .ok_or(ContextRouteError::InvalidExecution)
    }

    fn dispatch_deep(
        &mut self,
        _decision: &ContextRouteDecision,
        contracts: &DeepContractVersions,
        budget: &DeepSemanticBudget,
    ) -> Result<RouteAttemptOutcome, Self::Error> {
        if !self.deep_delay.is_zero() {
            std::thread::sleep(self.deep_delay);
        }
        self.observed_deep_contracts.push(contracts.clone());
        self.observed_deep_budgets.push(budget.clone());
        self.deep_outcomes
            .pop_front()
            .ok_or(ContextRouteError::InvalidExecution)
    }
}

fn attempts(execution: &ContextExecution) -> &[workspace_atlas::context_route::ExecutionAttempt] {
    match execution {
        ContextExecution::Completed { attempts, .. }
        | ContextExecution::Partial { attempts, .. }
        | ContextExecution::Blocked { attempts, .. } => attempts,
        ContextExecution::Interrupted {
            completed_attempts, ..
        } => completed_attempts,
    }
}

#[test]
fn direct_success_observes_starting_generation_without_atlas_context_identity() {
    let observed_request = request(
        &[(
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Satisfied,
        )],
        Route::Direct,
        Route::AtlasDeep,
        "gen-a",
    );
    let mut dispatcher = FakeDispatcher::default();

    let execution = dispatch_context_application(observed_request, &mut dispatcher).unwrap();

    match &execution {
        ContextExecution::Completed {
            decision,
            final_route,
            payload,
            counters,
            materialization,
            ..
        } => {
            assert_eq!(decision.initial_route, Route::Direct);
            assert_eq!(*final_route, Route::Direct);
            assert_eq!(*payload, ContextPayload::DirectNone {});
            assert_eq!(*counters, ContextExecutionCounters::default());
            assert!(materialization.is_none());
            assert_eq!(
                decision.starting_generation,
                GenerationObservation::Observed {
                    generation_id: "gen-a".to_string()
                }
            );
        }
        other => panic!("expected direct completion, got {other:?}"),
    }
    assert_eq!(attempts(&execution).len(), 1);
    assert_eq!(attempts(&execution)[0].route, Route::Direct);
    assert!(dispatcher.generation_observations.is_empty());
    assert!(dispatcher.observed_light_operations.is_empty());
    assert!(dispatcher.observed_deep_contracts.is_empty());

    let mut unavailable_request = request(
        &[(
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Satisfied,
        )],
        Route::Direct,
        Route::AtlasDeep,
        "ignored-for-unavailable",
    );
    unavailable_request.starting_generation = GenerationObservation::Unavailable {
        reason: workspace_atlas::context_route::GenerationUnavailableReason::CatalogueUnavailable,
    };
    let unavailable = dispatch_context_application(unavailable_request, &mut dispatcher).unwrap();
    assert!(matches!(
        unavailable,
        ContextExecution::Completed {
            final_route: Route::Direct,
            payload: ContextPayload::DirectNone {},
            ..
        }
    ));

    let serialized = serde_json::to_string(&execution).unwrap();
    for forbidden in [
        "raw task secret",
        "src/private.key",
        "openai",
        "gpt-5.6",
        "source body secret",
        "context_id",
    ] {
        assert!(!serialized.contains(forbidden));
    }
}

#[test]
fn direct_to_light_to_deep_progresses_with_pinned_contracts() {
    let request = request(
        &[
            (
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            ),
            (
                RequiredCapability::ValidationPlan,
                CallerCapabilityState::Unknown,
            ),
        ],
        Route::Direct,
        Route::AtlasDeep,
        "gen-a",
    );
    let deep_document = deep_fixture();
    let deep_payload = deep_context_payload(&deep_document).unwrap();
    let mut dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([
            GenerationObservation::Observed {
                generation_id: "gen-a".to_string(),
            },
            GenerationObservation::Observed {
                generation_id: "gen-a".to_string(),
            },
        ]),
        light_outcomes: VecDeque::from([deficient(
            Some(light_payload()),
            vec![RequiredCapability::IdentityLookup],
            vec![RouteDeficit::Capability {
                capability: RequiredCapability::ValidationPlan,
                reason: CapabilityDeficitReason::Unavailable,
            }],
        )]),
        deep_outcomes: VecDeque::from([completed(deep_payload.clone(), counters(1, 4))]),
        light_delay: Duration::from_millis(2),
        deep_delay: Duration::from_millis(2),
        ..Default::default()
    };

    let execution = dispatch_context_application(request, &mut dispatcher).unwrap();

    match &execution {
        ContextExecution::Completed {
            decision,
            final_route,
            payload,
            ..
        } => {
            assert_eq!(decision.initial_route, Route::Direct);
            assert_eq!(*final_route, Route::AtlasDeep);
            assert_eq!(payload, &deep_payload);
        }
        other => panic!("expected deep completion, got {other:?}"),
    }
    assert_eq!(
        attempts(&execution)
            .iter()
            .map(|attempt| attempt.route)
            .collect::<Vec<_>>(),
        vec![Route::Direct, Route::AtlasLight, Route::AtlasDeep]
    );
    assert!(
        attempts(&execution)[1].elapsed_micros >= 1_000,
        "LIGHT attempt must report measured dispatch time"
    );
    assert!(
        attempts(&execution)[2].elapsed_micros >= 1_000,
        "DEEP attempt must report measured dispatch time"
    );
    assert_eq!(
        dispatcher.observed_light_operations,
        vec![vec![
            VersionedOperation::new(LightOperation::Query),
            VersionedOperation::new(LightOperation::SourceReference),
        ]]
    );
    assert_eq!(
        dispatcher.observed_deep_contracts,
        vec![DeepContractVersions::fixed()]
    );
    assert_eq!(dispatcher.observed_deep_budgets, vec![budget()]);
}

#[test]
fn light_ceiling_returns_partial_with_useful_payload_or_blocked_without_it() {
    let capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ValidationPlan,
            CallerCapabilityState::Unknown,
        ),
    ];
    let mut partial_dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-a".to_string(),
        }]),
        light_outcomes: VecDeque::from([deficient(
            Some(light_payload()),
            vec![RequiredCapability::IdentityLookup],
            vec![RouteDeficit::Capability {
                capability: RequiredCapability::ValidationPlan,
                reason: CapabilityDeficitReason::Unavailable,
            }],
        )]),
        ..Default::default()
    };
    let partial = dispatch_context_application(
        request(&capabilities, Route::Direct, Route::AtlasLight, "gen-a"),
        &mut partial_dispatcher,
    )
    .unwrap();
    assert!(matches!(
        partial,
        ContextExecution::Partial {
            final_route: Route::AtlasLight,
            terminal_reason: TerminalReason::RouteCeilingReached,
            ..
        }
    ));

    let mut blocked_dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-a".to_string(),
        }]),
        light_outcomes: VecDeque::from([deficient(
            None,
            vec![],
            vec![
                RouteDeficit::Capability {
                    capability: RequiredCapability::IdentityLookup,
                    reason: CapabilityDeficitReason::Unavailable,
                },
                RouteDeficit::Capability {
                    capability: RequiredCapability::ValidationPlan,
                    reason: CapabilityDeficitReason::Unavailable,
                },
            ],
        )]),
        ..Default::default()
    };
    let blocked = dispatch_context_application(
        request(&capabilities, Route::Direct, Route::AtlasLight, "gen-a"),
        &mut blocked_dispatcher,
    )
    .unwrap();
    assert!(matches!(
        blocked,
        ContextExecution::Blocked {
            final_route: Some(Route::AtlasLight),
            terminal_reason: TerminalReason::RouteCeilingReached,
            ..
        }
    ));
}
#[test]
fn stale_exact_source_stops_at_light_even_when_deep_is_permitted() {
    let capabilities = [(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    )];
    let mut dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-a".to_string(),
        }]),
        light_outcomes: VecDeque::from([deficient(
            None,
            vec![],
            vec![RouteDeficit::Capability {
                capability: RequiredCapability::ExactSource,
                reason: CapabilityDeficitReason::Stale,
            }],
        )]),
        ..Default::default()
    };

    let execution = dispatch_context_application(
        request(&capabilities, Route::AtlasLight, Route::AtlasDeep, "gen-a"),
        &mut dispatcher,
    )
    .unwrap();

    assert!(matches!(
        execution,
        ContextExecution::Blocked {
            final_route: Some(Route::AtlasLight),
            terminal_reason: TerminalReason::CapabilityUnavailable,
            ..
        }
    ));
    assert!(dispatcher.observed_deep_contracts.is_empty());
}

#[test]
fn deep_interruption_retains_only_the_prior_completed_light_payload() {
    let capabilities = [
        (
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        ),
        (
            RequiredCapability::ValidationPlan,
            CallerCapabilityState::Unknown,
        ),
    ];
    let expected_prior = light_payload();
    let mut dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([
            GenerationObservation::Observed {
                generation_id: "gen-a".to_string(),
            },
            GenerationObservation::Observed {
                generation_id: "gen-a".to_string(),
            },
        ]),
        light_outcomes: VecDeque::from([deficient(
            Some(expected_prior.clone()),
            vec![RequiredCapability::IdentityLookup],
            vec![RouteDeficit::Capability {
                capability: RequiredCapability::ValidationPlan,
                reason: CapabilityDeficitReason::Unavailable,
            }],
        )]),
        deep_outcomes: VecDeque::from([RouteAttemptOutcome::Interrupted {
            diagnostics: vec![],
            interruption_reason:
                workspace_atlas::context_route::InterruptionReason::DeadlineExceeded,
        }]),
        ..Default::default()
    };

    let execution = dispatch_context_application(
        request(&capabilities, Route::Direct, Route::AtlasDeep, "gen-a"),
        &mut dispatcher,
    )
    .unwrap();

    match execution {
        ContextExecution::Interrupted {
            completed_attempts,
            prior_payload,
            satisfied_capabilities,
            ..
        } => {
            assert_eq!(
                completed_attempts
                    .iter()
                    .map(|attempt| attempt.route)
                    .collect::<Vec<_>>(),
                vec![Route::Direct, Route::AtlasLight]
            );
            assert_eq!(prior_payload, Some(expected_prior));
            assert_eq!(
                satisfied_capabilities,
                vec![RequiredCapability::IdentityLookup]
            );
        }
        other => panic!("expected interrupted deep execution, got {other:?}"),
    }
}

#[test]
fn changed_generation_blocks_until_the_caller_resubmits_a_new_decision() {
    let capabilities = [(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Unsatisfied,
    )];
    let mut changed = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-b".to_string(),
        }]),
        ..Default::default()
    };
    let blocked = dispatch_context_application(
        request(&capabilities, Route::AtlasLight, Route::AtlasLight, "gen-a"),
        &mut changed,
    )
    .unwrap();
    assert!(matches!(
        blocked,
        ContextExecution::Blocked {
            deficits,
            terminal_reason: TerminalReason::GenerationChanged,
            ..
        } if deficits == vec![RouteDeficit::Route { reason: RouteFailureReason::GenerationChanged }]
    ));
    assert!(changed.observed_light_operations.is_empty());

    let mut resubmitted = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-b".to_string(),
        }]),
        light_outcomes: VecDeque::from([completed(light_payload(), counters(1, 1))]),
        ..Default::default()
    };
    let completed = dispatch_context_application(
        request(&capabilities, Route::AtlasLight, Route::AtlasLight, "gen-b"),
        &mut resubmitted,
    )
    .unwrap();
    assert!(matches!(
        completed,
        ContextExecution::Completed {
            final_route: Route::AtlasLight,
            ..
        }
    ));
}

#[test]
fn generic_dispatch_preserves_a_canonical_deep_reference_across_routes() {
    let direct_document = deep_fixture();
    let expected_payload = deep_context_payload(&direct_document).unwrap();
    let mut escalated_document = direct_document.clone();
    escalated_document.context_id = "ctx-v2-escalated-attempt".to_string();
    escalated_document.task.task_session_id = "task-v2-escalated-attempt".to_string();
    let escalated_document = escalated_document.seal().unwrap();
    let escalated_payload = deep_context_payload(&escalated_document).unwrap();
    assert_eq!(expected_payload, escalated_payload);

    let capabilities = [(
        RequiredCapability::ValidationPlan,
        CallerCapabilityState::Unsatisfied,
    )];
    let mut direct_dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen_01JTEST".to_string(),
        }]),
        deep_outcomes: VecDeque::from([completed(expected_payload.clone(), counters(1, 4))]),
        ..Default::default()
    };
    let direct = dispatch_context_application(
        request(
            &capabilities,
            Route::AtlasDeep,
            Route::AtlasDeep,
            "gen_01JTEST",
        ),
        &mut direct_dispatcher,
    )
    .unwrap();

    let mut escalated_dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([
            GenerationObservation::Observed {
                generation_id: "gen_01JTEST".to_string(),
            },
            GenerationObservation::Observed {
                generation_id: "gen_01JTEST".to_string(),
            },
        ]),
        light_outcomes: VecDeque::from([deficient(
            Some(light_payload()),
            vec![],
            vec![RouteDeficit::Capability {
                capability: RequiredCapability::ValidationPlan,
                reason: CapabilityDeficitReason::Unavailable,
            }],
        )]),
        deep_outcomes: VecDeque::from([completed(escalated_payload, counters(1, 4))]),
        ..Default::default()
    };
    let escalated = dispatch_context_application(
        request(
            &capabilities,
            Route::Direct,
            Route::AtlasDeep,
            "gen_01JTEST",
        ),
        &mut escalated_dispatcher,
    )
    .unwrap();

    assert_eq!(direct.payload(), escalated.payload());
    assert_eq!(attempts(&direct)[0].route, Route::AtlasDeep);
    assert_eq!(attempts(&escalated)[0].route, Route::Direct);
    let direct_decision_hash = match &direct {
        ContextExecution::Completed { decision, .. } => &decision.decision_hash,
        _ => unreachable!("direct deep execution completed"),
    };
    let escalated_decision_hash = match &escalated {
        ContextExecution::Completed { decision, .. } => &decision.decision_hash,
        _ => unreachable!("escalated deep execution completed"),
    };
    assert_ne!(direct_decision_hash, escalated_decision_hash);
    for execution in [&direct, &escalated] {
        let serialized = serde_json::to_string(execution).unwrap();
        for forbidden in [
            "src/controller/connection.ts",
            "ConnectionController.scheduleReconnect",
            "task_v2_example_001",
            "ctx_v2_example_001",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }
}

#[test]
fn interruption_keeps_transient_state_outside_the_decision_and_ir_hashes() {
    let capabilities = [(
        RequiredCapability::IdentityLookup,
        CallerCapabilityState::Unsatisfied,
    )];
    let retry_request = request(&capabilities, Route::AtlasLight, Route::AtlasLight, "gen-a");
    let mut dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-a".to_string(),
        }]),
        light_outcomes: VecDeque::from([RouteAttemptOutcome::Interrupted {
            diagnostics: vec![ContextDiagnosticCode::EvidenceOmitted],
            interruption_reason: workspace_atlas::context_route::InterruptionReason::Cancelled,
        }]),
        ..Default::default()
    };
    let interrupted = dispatch_context_application(retry_request.clone(), &mut dispatcher).unwrap();
    let interrupted_decision_hash = match &interrupted {
        ContextExecution::Interrupted { decision, .. } => decision.decision_hash.clone(),
        other => panic!("expected interrupted execution, got {other:?}"),
    };

    let mut retry_dispatcher = FakeDispatcher {
        generation_observations: VecDeque::from([GenerationObservation::Observed {
            generation_id: "gen-a".to_string(),
        }]),
        light_outcomes: VecDeque::from([completed(light_payload(), counters(1, 1))]),
        ..Default::default()
    };
    let completed = dispatch_context_application(retry_request, &mut retry_dispatcher).unwrap();
    let completed_decision_hash = match &completed {
        ContextExecution::Completed { decision, .. } => decision.decision_hash.clone(),
        other => panic!("expected completed retry, got {other:?}"),
    };
    assert_eq!(interrupted_decision_hash, completed_decision_hash);
}

#[test]
fn direct_does_not_touch_a_real_catalogue_and_v2_cannot_enter_legacy_lifecycle() {
    let db_dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    std::fs::write(ws_dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n").unwrap();
    let db_path = db_dir.path().join("atlas.sqlite");
    let connection = init_catalogue(&db_path, &config).unwrap();
    let workspace =
        register_workspace(&connection, ws_dir.path(), &config, &db_path, "1.0.0").unwrap();
    let generation = workspace_atlas::discovery::reconcile(&workspace, &connection, &config)
        .unwrap()
        .candidate_generation_id;
    let before: (i64, i64, i64) = (
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
    );
    let mut direct_dispatcher = FakeDispatcher::default();
    let direct = dispatch_context_application(
        request(
            &[(
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Satisfied,
            )],
            Route::Direct,
            Route::AtlasDeep,
            &generation,
        ),
        &mut direct_dispatcher,
    )
    .unwrap();
    assert_eq!(direct.payload(), Some(&ContextPayload::DirectNone {}));
    let after_direct: (i64, i64, i64) = (
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
    );
    assert_eq!(after_direct, before);

    let error = create_task_session(
        &connection,
        &workspace,
        &generation,
        &"44".repeat(32),
        &"55".repeat(32),
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_V2_VERSION,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("durable task lifecycle supports only legacy Context IR"));
    let row_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM task_session", [], |row| row.get(0))
        .unwrap();
    assert_eq!(row_count, 0);
}

fn deep_fixture() -> workspace_atlas::context_ir::DeepContextIrV2 {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/context_ir/context-ir.example.json")).unwrap();
    read_deep_context_ir_v2(&serde_json::to_string(&fixture["deep_v2"]).unwrap()).unwrap()
}
