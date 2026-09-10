use workspace_atlas::context_yield::{
    compare_context_yield_reports_v15, ContextYieldEvidenceClass, ContextYieldLimitation,
    ContextYieldMetricName, ContextYieldReportV15, YieldIncomparableReason,
    CONTEXT_YIELD_REPORT_SCHEMA_VERSION,
};

const FIXTURE: &str = include_str!("fixtures/context_yield/context-yield.example.json");

fn fixture_report() -> ContextYieldReportV15 {
    serde_json::from_str(FIXTURE).expect("the versioned Context Yield fixture must decode")
}

#[test]
fn versioned_closed_fixture_round_trips_without_sensitive_content() {
    let report = fixture_report();
    assert_eq!(report.schema_version, CONTEXT_YIELD_REPORT_SCHEMA_VERSION);
    assert_eq!(report.content.sample_size, 3);
    assert_eq!(report.content.raw_measures.samples.len(), 3);
    assert!(!report.content.validity.observation_coverage.complete);
    assert_eq!(report.content.validity.limitations.len(), 2);
    assert_eq!(
        report.content.validity.limitations[0],
        ContextYieldLimitation::PartialObservationCoverage
    );
    assert_eq!(
        report.content.raw_measures.metrics[0].name,
        ContextYieldMetricName::ContextPrecisionObserved
    );

    let fixture_value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(serde_json::to_value(&report).unwrap(), fixture_value);

    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("\"prompt\""));
    assert!(!serialized.contains("source_content"));
}

#[test]
fn unknown_fields_are_rejected_at_every_report_boundary() {
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    value["content"]["raw_measures"]["samples"][0]["unknown"] = true.into();
    let error = serde_json::from_value::<ContextYieldReportV15>(value).unwrap_err();
    assert!(error.to_string().contains("unknown field `unknown`"));
}

#[test]
fn invalid_sample_and_observation_coverage_are_refused() {
    let report = fixture_report();

    let mut wrong_sample_size = report.clone();
    wrong_sample_size.content.sample_size = 4;
    assert_eq!(
        compare_context_yield_reports_v15(&report, &wrong_sample_size).unwrap_err(),
        YieldIncomparableReason::InvalidReport
    );

    let mut inconsistent_coverage = report.clone();
    inconsistent_coverage
        .content
        .validity
        .observation_coverage
        .complete = true;
    assert_eq!(
        compare_context_yield_reports_v15(&report, &inconsistent_coverage).unwrap_err(),
        YieldIncomparableReason::InvalidReport
    );

    let mut missing_metric = report.clone();
    missing_metric.content.raw_measures.metrics.pop();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &missing_metric).unwrap_err(),
        YieldIncomparableReason::InvalidReport
    );

    let mut negative_measure = report.clone();
    negative_measure.content.raw_measures.samples[0].working_set_size = -1;
    assert_eq!(
        compare_context_yield_reports_v15(&report, &negative_measure).unwrap_err(),
        YieldIncomparableReason::InvalidReport
    );

    let mut wrong_metric_semantics = report.clone();
    wrong_metric_semantics.content.raw_measures.metrics[0].evidence_class =
        ContextYieldEvidenceClass::ReportedUse;
    assert_eq!(
        compare_context_yield_reports_v15(&report, &wrong_metric_semantics).unwrap_err(),
        YieldIncomparableReason::InvalidReport
    );
}

#[test]
fn rejected_and_unequal_accepted_outcomes_cannot_compare() {
    let report = fixture_report();

    let mut rejected_value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    rejected_value["content"]["accepted_outcome"]["acceptance"] = "rejected".into();
    let rejected: ContextYieldReportV15 = serde_json::from_value(rejected_value).unwrap();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &rejected).unwrap_err(),
        YieldIncomparableReason::OutcomeNotAccepted
    );

    let mut unequal = report.clone();
    unequal.content.accepted_outcome.outcome_hash =
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &unequal).unwrap_err(),
        YieldIncomparableReason::DifferentAcceptedOutcome
    );
}

#[test]
fn full_comparability_includes_task_estimator_and_metric_policy() {
    let report = fixture_report();

    let mut different_task = report.clone();
    different_task.content.task_hash =
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &different_task).unwrap_err(),
        YieldIncomparableReason::DifferentTask
    );

    let mut different_estimator = report.clone();
    different_estimator.content.estimator_policy_version = "estimator-v2".to_string();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &different_estimator).unwrap_err(),
        YieldIncomparableReason::DifferentEstimatorPolicy
    );

    let mut different_metric = report.clone();
    different_metric.content.metric_policy_version = "context-yield-v2".to_string();
    assert_eq!(
        compare_context_yield_reports_v15(&report, &different_metric).unwrap_err(),
        YieldIncomparableReason::DifferentMetricPolicy
    );
}

#[test]
fn execution_metadata_is_not_part_of_canonical_comparison_data() {
    let report = fixture_report();
    let mut rerun = report.clone();
    rerun.execution.run_id = "run_later".to_string();
    rerun.execution.generated_at = "2026-09-02T01:00:00.000Z".to_string();

    assert_eq!(report.content, rerun.content);
    let comparison = compare_context_yield_reports_v15(&report, &rerun).unwrap();
    assert!(comparison.context_hashes_match);
    assert_eq!(comparison.sample_size, 3);
}
