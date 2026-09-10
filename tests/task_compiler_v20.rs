use serde_json::Value;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    read_context_ir_v1, read_context_ir_v2_compatible, read_deep_context_ir_v2,
    write_deep_context_ir_v2, ContextIrDocument, DeepContextIrV2, EvidenceQuality,
    IrOmissionReasonV2, IrRequirementStateV2, ItemRole, TaskKind, WorkingSetStatus,
    CONTEXT_SCHEMA_V2_VERSION, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES;
use workspace_atlas::context_route::CONTEXT_EXECUTION_VERSION;
use workspace_atlas::task_compiler::{
    deep_v2_role_recipe, DeepV2SourceMaterializationAuthorization, PLANNER_POLICY_V2_VERSION,
    PLANNER_POLICY_VERSION,
};

fn fixture_documents() -> (Value, Value) {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/context_ir/context-ir.example.json")).unwrap();
    (fixture["legacy_v1"].clone(), fixture["deep_v2"].clone())
}

#[test]
fn old_and_new_readers_are_explicit_and_fail_closed() {
    let (legacy, deep) = fixture_documents();
    let legacy_json = serde_json::to_string(&legacy).unwrap();
    let deep_json = serde_json::to_string(&deep).unwrap();

    let parsed_v1 = read_context_ir_v1(&legacy_json).unwrap();
    assert_eq!(parsed_v1.schema_version, CONTEXT_SCHEMA_VERSION);
    assert!(read_context_ir_v1(&deep_json).is_err());
    assert!(read_deep_context_ir_v2(&legacy_json).is_err());

    let parsed_v2 = read_deep_context_ir_v2(&deep_json).unwrap();
    assert_eq!(parsed_v2.schema_version, CONTEXT_SCHEMA_V2_VERSION);
    assert_eq!(
        parsed_v2.policy.planner_policy_version,
        PLANNER_POLICY_V2_VERSION
    );

    assert!(matches!(
        read_context_ir_v2_compatible(&legacy_json).unwrap(),
        ContextIrDocument::LegacyV1(_)
    ));
    assert!(matches!(
        read_context_ir_v2_compatible(&deep_json).unwrap(),
        ContextIrDocument::DeepV2(_)
    ));
}

#[test]
fn deep_v2_fixture_round_trips_and_matches_canonical_hash_vector() {
    let (_, deep) = fixture_documents();
    let deep_json = serde_json::to_string(&deep).unwrap();
    let parsed = read_deep_context_ir_v2(&deep_json).unwrap();

    assert_eq!(
        parsed.context_hash,
        "4afac2c1269a7f70747f93e0563ca0deff6eae69c77288609222520b41932a01"
    );
    assert_eq!(
        parsed.policy.semantic_budget.budget_digest,
        "eb931d1d9b48fb3d8af2de25fa7b3378df6328dad304f9b16137ee556efa77ba"
    );
    let written = write_deep_context_ir_v2(&parsed).unwrap();
    let written_value: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(written_value, deep);
}

#[test]
fn complete_v2_matrix_reserves_later_semantic_slots_but_rejects_execution_state() {
    let (_, mut deep) = fixture_documents();
    for field in [
        "workspace",
        "task",
        "policy",
        "status",
        "working_set",
        "relationships",
        "effects",
        "temporal_constraints",
        "uncertainty",
        "coverage",
        "omissions",
        "validation_plan",
        "evidence_lease",
        "cost",
        "context_hash",
    ] {
        assert!(deep.get(field).is_some(), "missing frozen V2 field {field}");
    }

    for forbidden in [
        "route_decision",
        "route_attempts",
        "final_route",
        "attempt_id",
        "deadline_ms",
        "elapsed_ms",
        "retry_count",
        "cancelled",
        "cache_hit",
        "materialized_source",
    ] {
        deep.as_object_mut()
            .unwrap()
            .insert(forbidden.to_string(), Value::Null);
        let raw = serde_json::to_string(&deep).unwrap();
        assert!(
            read_deep_context_ir_v2(&raw).is_err(),
            "transient execution field {forbidden} entered Context IR"
        );
        deep.as_object_mut().unwrap().remove(forbidden);
    }

    let mut missing_nested_slot = deep.clone();
    missing_nested_slot["status"]
        .as_object_mut()
        .unwrap()
        .remove("reasons");
    assert!(
        read_deep_context_ir_v2(&serde_json::to_string(&missing_nested_slot).unwrap()).is_err()
    );

    let mut forged_identity = deep;
    forged_identity["context_hash"] = Value::String("0".repeat(64));
    assert!(serde_json::from_value::<DeepContextIrV2>(forged_identity).is_err());
}

#[test]
fn planner_v2_role_matrix_is_total_and_reserves_downstream_roles() {
    let expected: &[(TaskKind, &[ItemRole])] = &[
        (TaskKind::Explore, &[ItemRole::PrimaryImplementation]),
        (
            TaskKind::BugFix,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::TestContract,
                ItemRole::HistoricalConstraint,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::BehaviorChange,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::DirectDependent,
                ItemRole::TestContract,
                ItemRole::Effect,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::ApiChange,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::TypeContract,
                ItemRole::DirectDependent,
                ItemRole::TestContract,
                ItemRole::Effect,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::Refactor,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::DirectDependent,
                ItemRole::TestContract,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::ConfigurationChange,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::ConfigurationInput,
                ItemRole::TestContract,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::TestChange,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::TestContract,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::Review,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::TestContract,
                ItemRole::Effect,
                ItemRole::HistoricalConstraint,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::Audit,
            &[
                ItemRole::PrimaryImplementation,
                ItemRole::TestContract,
                ItemRole::ConfigurationInput,
                ItemRole::HistoricalConstraint,
                ItemRole::ValidationTarget,
            ],
        ),
        (
            TaskKind::Unknown,
            &[ItemRole::PrimaryImplementation, ItemRole::ValidationTarget],
        ),
    ];

    for (kind, expected_required) in expected {
        let recipe = deep_v2_role_recipe(*kind);
        assert_eq!(recipe.required_roles, *expected_required);
        assert!(recipe
            .optional_roles()
            .all(|role| !recipe.required_roles.contains(&role)));
        assert_eq!(recipe.role_matrix().count(), 11);
        assert!(recipe.requires_exact_source);
        assert!(recipe.requires_validation);
    }
    assert_eq!(PLANNER_POLICY_VERSION, "planner-v1.1.0");
    assert_eq!(PLANNER_POLICY_V2_VERSION, "planner-v2.0.0");
}

#[test]
fn v2_seal_rejects_a_role_matrix_that_overclaims_task_sufficiency() {
    let (_, deep) = fixture_documents();
    let mut parsed = read_deep_context_ir_v2(&serde_json::to_string(&deep).unwrap()).unwrap();
    parsed
        .status
        .role_sufficiency
        .retain(|role| role.role == ItemRole::PrimaryImplementation);
    assert!(parsed.seal().is_err());
}

#[test]
fn v2_seal_rejects_unverified_source_claims_and_nonpositive_budgets() {
    let (_, deep) = fixture_documents();
    let parsed = read_deep_context_ir_v2(&serde_json::to_string(&deep).unwrap()).unwrap();

    let mut stale_source = parsed.clone();
    stale_source.working_set[0]
        .source
        .as_mut()
        .unwrap()
        .observed_sha256 =
        Some("7777777777777777777777777777777777777777777777777777777777777777".into());
    assert!(stale_source.seal().is_err());

    let mut missing_budget = parsed;
    missing_budget.policy.semantic_budget.max_work_units = 0;
    assert!(missing_budget.seal().is_err());
}

#[test]
fn v2_hash_excludes_session_identity_and_complete_status_requires_backing_evidence() {
    let (_, deep) = fixture_documents();
    let parsed = read_deep_context_ir_v2(&serde_json::to_string(&deep).unwrap()).unwrap();

    let canonical_hash = parsed.context_hash.clone();
    let mut another_session = parsed.clone();
    another_session.context_id = "ctx_v2_other_attempt".into();
    another_session.task.task_session_id = "task_v2_other_attempt".into();
    assert_eq!(another_session.seal().unwrap().context_hash, canonical_hash);

    let mut missing_source = parsed.clone();
    missing_source.working_set[0].source = None;
    missing_source.working_set[0].cost.source_bytes = 0;
    missing_source.cost.selected_source_bytes = 0;
    assert!(missing_source.seal().is_err());

    let mut missing_effect = parsed.clone();
    missing_effect.effects.clear();
    assert!(missing_effect.seal().is_err());

    let mut optional_only_validation = parsed.clone();
    optional_only_validation.validation_plan[0].required = false;
    assert!(optional_only_validation.seal().is_err());

    let mut unqualified_effect = parsed.clone();
    unqualified_effect.effects[0].evidence.state = EvidenceQuality::Unresolved;
    assert!(unqualified_effect.seal().is_err());

    let mut required_omission = parsed.clone();
    required_omission
        .omissions
        .by_reason
        .insert(IrOmissionReasonV2::MissingRequiredRole, 1);
    required_omission.omissions.omitted = 1;
    required_omission.omissions.candidates_considered += 1;
    assert!(required_omission.seal().is_err());
    let mut missing_validation = parsed.clone();
    missing_validation.validation_plan.clear();
    assert!(missing_validation.seal().is_err());

    let mut dangling_role_evidence = parsed;
    dangling_role_evidence.status.role_sufficiency[0].evidence_item_ids =
        vec!["missing_item".into()];
    assert!(dangling_role_evidence.seal().is_err());
}

#[test]
fn v2_status_precedence_is_exclusive_and_reason_pairs_are_closed() {
    let (_, deep) = fixture_documents();
    let parsed = read_deep_context_ir_v2(&serde_json::to_string(&deep).unwrap()).unwrap();

    let mut blocked = parsed.clone();
    blocked.working_set[0].source = None;
    blocked.working_set[0].cost.source_bytes = 0;
    blocked.cost.selected_source_bytes = 0;
    blocked.status.exact_source = IrRequirementStateV2::Missing;
    blocked.status.reasons = vec![IrOmissionReasonV2::SourceVerificationFailed];
    blocked.status.working_set_status = WorkingSetStatus::Blocked;
    let blocked = blocked.seal().unwrap();

    let mut false_partial = blocked;
    false_partial.status.working_set_status = WorkingSetStatus::Partial;
    assert!(false_partial.seal().is_err());

    let mut mismatched_reason = parsed;
    let optional = mismatched_reason
        .status
        .role_sufficiency
        .iter_mut()
        .find(|role| role.role == ItemRole::DirectDependency)
        .unwrap();
    optional.reason = Some(IrOmissionReasonV2::RecordBudget);
    assert!(mismatched_reason.seal().is_err());
}

#[test]
fn internal_v2_reader_writer_creates_no_durable_v2_rows() {
    let (_, deep) = fixture_documents();
    let parsed = read_deep_context_ir_v2(&serde_json::to_string(&deep).unwrap()).unwrap();
    write_deep_context_ir_v2(&parsed).unwrap();

    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("atlas.sqlite");
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"v2-no-persistence\"\n",
    )
    .unwrap();
    let connection = init_catalogue(&database_path, &config).unwrap();
    let durable_v2_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM context_ir WHERE context_ir_version = '2.0.0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(durable_v2_rows, 0);
}

#[test]
fn source_materialization_limit_reports_the_current_execution_contract() {
    let error =
        DeepV2SourceMaterializationAuthorization::new(MAX_CONTEXT_EXECUTION_SOURCE_BYTES + 1)
            .unwrap_err()
            .to_string();
    assert!(error.contains(CONTEXT_EXECUTION_VERSION), "{error}");
    assert!(!error.contains("context-execution-v1.0.0"), "{error}");
}
