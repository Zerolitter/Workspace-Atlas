use serde::Deserialize;
use serde_json::Value;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    read_deep_context_ir_v2, IrOmissionReasonV2, IrOmissionsV2, CONTEXT_SCHEMA_V2_VERSION,
};
use workspace_atlas::context_route::{ContextRouteError, DeepSemanticBudget};
use workspace_atlas::task_compiler::{
    enforce_deep_v2_budget, DeepV2BudgetCandidate, PLANNER_POLICY_V2_VERSION,
    PLANNER_POLICY_VERSION,
};

const BUDGET_FIXTURE: &str = include_str!("fixtures/context_ir/context-budgets.example.json");
const IR_FIXTURE: &str = include_str!("fixtures/context_ir/context-ir.example.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    budget: BudgetLimits,
    candidates: Vec<DeepV2BudgetCandidate>,
    expected: Expected,
    transient_execution_variants: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetLimits {
    max_records: u64,
    max_source_bytes: u64,
    max_estimated_tokens: u64,
    max_depth: u32,
    max_work_units: u64,
    uncertainty_reserve_percent: u8,
}

impl BudgetLimits {
    fn build(&self) -> Result<DeepSemanticBudget, ContextRouteError> {
        DeepSemanticBudget::new(
            self.max_records,
            self.max_source_bytes,
            self.max_estimated_tokens,
            self.max_depth,
            self.max_work_units,
            self.uncertainty_reserve_percent,
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    selected_candidate_ids: Vec<String>,
    selected_records: u64,
    selected_source_bytes: u64,
    selected_estimated_tokens: u64,
    relationship_depth_reached: u32,
    work_units_consumed: u64,
    uncertainty_records_reserved: u64,
    uncertainty_records_selected: u64,
    omissions: IrOmissionsV2,
}

fn fixture() -> Fixture {
    serde_json::from_str(BUDGET_FIXTURE).unwrap()
}

#[test]
fn v2_budget_vector_enforces_every_limit_and_stable_omission_accounting() {
    let fixture = fixture();
    let outcome = enforce_deep_v2_budget(&fixture.budget.build().unwrap(), &fixture.candidates)
        .expect("valid explicit budget vector");

    assert_eq!(
        outcome.selected_candidate_ids,
        fixture.expected.selected_candidate_ids
    );
    assert_eq!(
        outcome.cost.selected_records,
        fixture.expected.selected_records
    );
    assert_eq!(
        outcome.cost.selected_source_bytes,
        fixture.expected.selected_source_bytes
    );
    assert_eq!(
        outcome.cost.selected_estimated_tokens,
        fixture.expected.selected_estimated_tokens
    );
    assert_eq!(
        outcome.cost.relationship_depth_reached,
        fixture.expected.relationship_depth_reached
    );
    assert_eq!(
        outcome.cost.work_units_consumed,
        fixture.expected.work_units_consumed
    );
    assert_eq!(
        outcome.cost.uncertainty_records_reserved,
        fixture.expected.uncertainty_records_reserved
    );
    assert_eq!(
        outcome.cost.uncertainty_records_selected,
        fixture.expected.uncertainty_records_selected
    );
    assert_eq!(
        serde_json::to_value(&outcome.omissions).unwrap(),
        serde_json::to_value(&fixture.expected.omissions).unwrap()
    );
    assert_eq!(outcome.semantic_budget.max_relationship_depth, 2);
    assert_eq!(outcome.semantic_budget.max_work_units, 20);
    assert_eq!(outcome.semantic_budget.uncertainty_reserve_percent, 25);
}

#[test]
fn v2_budget_requires_all_explicit_positive_limits() {
    let budget = serde_json::to_value(fixture().budget.build().unwrap()).unwrap();
    for field in [
        "max_records",
        "max_source_bytes",
        "max_estimated_tokens",
        "max_depth",
        "max_work_units",
        "uncertainty_reserve_percent",
    ] {
        let mut absent = budget.clone();
        absent.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<DeepSemanticBudget>(absent).is_err(),
            "production V2 input accepted absent {field}"
        );

        let mut zero = budget.clone();
        zero[field] = Value::from(0);
        let supplied: DeepSemanticBudget = serde_json::from_value(zero).unwrap();
        assert_eq!(
            supplied.validate().unwrap_err(),
            ContextRouteError::InvalidDeepBudget,
            "production V2 input accepted nonpositive {field}"
        );
    }
}

#[test]
fn semantic_budget_outcome_is_independent_of_transient_execution_inputs() {
    let fixture = fixture();
    let budget = fixture.budget.build().unwrap();
    let candidate = serde_json::to_value(&fixture.candidates[0]).unwrap();
    for transient in &fixture.transient_execution_variants {
        for (field, value) in transient.as_object().unwrap() {
            let mut polluted = candidate.clone();
            polluted
                .as_object_mut()
                .unwrap()
                .insert(field.clone(), value.clone());
            assert!(
                serde_json::from_value::<DeepV2BudgetCandidate>(polluted).is_err(),
                "transient execution field {field} entered semantic budget input"
            );
        }
    }

    let outcome = enforce_deep_v2_budget(&budget, &fixture.candidates).unwrap();
    assert_eq!(
        outcome.selected_candidate_ids,
        fixture.expected.selected_candidate_ids
    );

    let ir_fixture: Value = serde_json::from_str(IR_FIXTURE).unwrap();
    let deep = serde_json::to_string(&ir_fixture["deep_v2"]).unwrap();
    let parsed = read_deep_context_ir_v2(&deep).unwrap();
    let hash = parsed.context_hash.clone();
    let mut another_execution = parsed;
    another_execution.context_id = "ctx_transient_attempt_b".into();
    another_execution.task.task_session_id = "session_transient_attempt_b".into();
    assert_eq!(another_execution.seal().unwrap().context_hash, hash);
}

#[test]
fn public_legacy_defaults_remain_v1_and_no_runtime_profile_is_added() {
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"legacy\"\n")
            .unwrap();
    assert_eq!(PLANNER_POLICY_VERSION, "planner-v1.1.0");
    assert_eq!(PLANNER_POLICY_V2_VERSION, "planner-v2.0.0");
    assert_eq!(CONTEXT_SCHEMA_V2_VERSION, "2.0.0");
    assert_eq!(config.schema_version, "1.0.0");

    let promoted_profile = "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"legacy\"\n[context_profile]\nname = \"experimental\"\n";
    assert!(Config::parse(promoted_profile).is_err());
}

#[test]
fn invalid_candidate_costs_fail_closed() {
    let fixture = fixture();
    let budget = fixture.budget.build().unwrap();
    let invalid = DeepV2BudgetCandidate {
        candidate_id: "invalid-zero-work".into(),
        rank: 0,
        source_bytes: 0,
        estimated_tokens: 0,
        relationship_depth: 0,
        work_units: 0,
        uncertainty: false,
    };
    assert!(enforce_deep_v2_budget(&budget, &[invalid]).is_err());

    let counts = enforce_deep_v2_budget(&budget, &fixture.candidates)
        .unwrap()
        .omissions
        .by_reason;
    for reason in [
        IrOmissionReasonV2::RecordBudget,
        IrOmissionReasonV2::SourceByteBudget,
        IrOmissionReasonV2::EstimatedTokenBudget,
        IrOmissionReasonV2::RelationshipDepthBudget,
        IrOmissionReasonV2::WorkUnitBudget,
        IrOmissionReasonV2::UncertaintyReserve,
    ] {
        assert_eq!(counts.get(&reason), Some(&1));
    }
}

#[test]
fn v2_candidates_must_have_canonical_unique_ranked_identity() {
    let fixture = fixture();
    let budget = fixture.budget.build().unwrap();

    let mut wrong_rank = fixture.candidates.clone();
    wrong_rank[1].rank = 0;
    assert!(enforce_deep_v2_budget(&budget, &wrong_rank).is_err());

    let mut duplicate = fixture.candidates;
    duplicate[1].candidate_id = duplicate[0].candidate_id.clone();
    assert!(enforce_deep_v2_budget(&budget, &duplicate).is_err());
}

#[test]
fn pure_allocator_counts_all_supplied_candidates_without_exceeding_work_budget() {
    let budget = DeepSemanticBudget::new(10, 1_000, 1_000, 1, 2, 10).unwrap();
    let candidates: Vec<_> = (0..4)
        .map(|rank| DeepV2BudgetCandidate {
            candidate_id: format!("candidate-{rank}"),
            rank,
            source_bytes: 0,
            estimated_tokens: 1,
            relationship_depth: 0,
            work_units: 1,
            uncertainty: false,
        })
        .collect();

    let outcome = enforce_deep_v2_budget(&budget, &candidates).unwrap();

    assert_eq!(outcome.cost.work_units_consumed, 2);
    assert_eq!(outcome.selected_candidate_ids.len(), 2);
    assert_eq!(outcome.omissions.candidates_considered, 4);
    assert_eq!(outcome.omissions.selected, 2);
    assert_eq!(outcome.omissions.omitted, 2);
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::WorkUnitBudget),
        Some(&2)
    );
}
#[test]
fn v2_ir_rejects_noncanonical_reserve_and_out_of_envelope_limits() {
    let ir_fixture: Value = serde_json::from_str(IR_FIXTURE).unwrap();
    let deep = serde_json::to_string(&ir_fixture["deep_v2"]).unwrap();
    let parsed = read_deep_context_ir_v2(&deep).unwrap();

    let mut reserve_drift = parsed.clone();
    reserve_drift.cost.uncertainty_records_reserved -= 1;
    assert!(reserve_drift.seal().is_err());

    let mut excessive_records = parsed;
    excessive_records.policy.semantic_budget.max_records =
        workspace_atlas::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS + 1;
    excessive_records.cost.uncertainty_records_reserved =
        excessive_records.policy.semantic_budget.max_records / 10;
    assert!(excessive_records.seal().is_err());
}

#[test]
fn v2_ir_rejects_inconsistent_truncation_and_zero_omission_counts() {
    let ir_fixture: Value = serde_json::from_str(IR_FIXTURE).unwrap();
    let deep = serde_json::to_string(&ir_fixture["deep_v2"]).unwrap();
    let parsed = read_deep_context_ir_v2(&deep).unwrap();

    let mut false_truncation = parsed.clone();
    false_truncation.omissions.truncated = true;
    assert!(false_truncation.seal().is_err());

    let mut zero_reason = parsed;
    zero_reason
        .omissions
        .by_reason
        .insert(IrOmissionReasonV2::RecordBudget, 0);
    assert!(zero_reason.seal().is_err());
}
