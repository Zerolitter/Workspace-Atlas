use assert_cmd::Command;
use serde_json::Value;
use workspace_atlas::context_yield::{
    ExperimentArmSamples, ExperimentComparisonError, ExperimentDimension, ExperimentLimitation,
    ExperimentRawSample, ExperimentRepresentation, ExperimentRunResult, FrozenExperimentBundle,
};

const SCENARIO: &str = "tests/fixtures/context_yield/benchmark-scenario.json";
const ATTESTATION_PREFIX: &str = "run attestation: ";

fn scenario_value() -> Value {
    serde_json::from_str(&std::fs::read_to_string(SCENARIO).unwrap()).unwrap()
}

fn sample(hash: &str, elapsed_micros: u64) -> ExperimentRawSample {
    ExperimentRawSample {
        context_hash: hash.to_string(),
        elapsed_micros,
        working_set_size: 3,
        selected_source_bytes: 128,
        selected_estimated_tokens: 32,
        serving_fallback: false,
        truncated: false,
    }
}

#[test]
fn stable_scenario_freezes_every_required_single_variable_experiment() {
    let value = scenario_value();
    let bundle: FrozenExperimentBundle =
        serde_json::from_value(value["experiment_bundle"].clone()).unwrap();
    bundle.validate().unwrap();

    let dimensions: Vec<_> = bundle
        .experiments
        .iter()
        .map(|experiment| experiment.independent_variable)
        .collect();
    assert_eq!(
        dimensions,
        vec![
            ExperimentDimension::Task,
            ExperimentDimension::Task,
            ExperimentDimension::Profile,
            ExperimentDimension::Representation,
            ExperimentDimension::Fallback,
            ExperimentDimension::GraphScale,
        ]
    );

    let mut drift = bundle.clone();
    drift.experiments[1].candidate.representation = ExperimentRepresentation::Flat;
    assert!(drift.validate().is_err());
}

#[test]
fn comparison_validation_fails_closed_on_misleading_evidence() {
    let value = scenario_value();
    let bundle: FrozenExperimentBundle =
        serde_json::from_value(value["experiment_bundle"].clone()).unwrap();
    let experiment = &bundle.experiments[1];
    let samples = vec![sample(&"a".repeat(64), 10), sample(&"a".repeat(64), 12)];
    let sample_result = ExperimentRunResult::from_samples(
        experiment,
        ExperimentArmSamples {
            generation_id: "generation".to_string(),
            catalogue_file_count: 128,
            raw_samples: samples.clone(),
        },
        ExperimentArmSamples {
            generation_id: "generation".to_string(),
            catalogue_file_count: 128,
            raw_samples: samples,
        },
        vec![ExperimentLimitation::SingleEnvironment],
    );
    let mut single_run = sample_result.clone();
    single_run.control.raw_samples.truncate(1);
    assert_eq!(
        single_run.validate_against(experiment).unwrap_err(),
        ExperimentComparisonError::InsufficientSamples
    );

    let mut unequal = sample_result.clone();
    unequal.candidate.accepted_outcome.outcome_hash = "f".repeat(64);
    assert_eq!(
        unequal.validate_against(experiment).unwrap_err(),
        ExperimentComparisonError::DifferentAcceptedOutcome
    );

    let mut mismatched_profile = sample_result.clone();
    mismatched_profile.candidate.condition.profile = "small".to_string();
    assert_eq!(
        mismatched_profile.validate_against(experiment).unwrap_err(),
        ExperimentComparisonError::ProfileMismatch
    );

    let mut fabricated_variance = sample_result.clone();
    fabricated_variance.candidate.variance.p95_micros += 1;
    assert_eq!(
        fabricated_variance
            .validate_against(experiment)
            .unwrap_err(),
        ExperimentComparisonError::InvalidVariance
    );

    let mut fabricated_scale = sample_result;
    fabricated_scale.candidate.catalogue_file_count = 1_280;
    assert_eq!(
        fabricated_scale.validate_against(experiment).unwrap_err(),
        ExperimentComparisonError::ConditionMismatch
    );
}

#[test]
fn benchmark_retains_private_current_run_experiment_evidence() {
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args(["--manifest", SCENARIO])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");

    let attestation_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix(ATTESTATION_PREFIX))
        .expect("benchmark prints generated attestation path");
    let attestation_text = std::fs::read_to_string(attestation_path).unwrap();
    let attestation: Value = serde_json::from_str(&attestation_text).unwrap();

    assert_eq!(
        attestation["experiment_results"].as_array().unwrap().len(),
        6
    );
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        attestation["revision"],
        String::from_utf8(head.stdout).unwrap().trim()
    );
    assert_eq!(
        attestation["procedure"]["percentile_method"],
        "nearest_rank"
    );
    assert!(attestation["fixture"]["generation_id"].as_str().is_some());
    assert!(attestation["policies"]["planner"].as_str().is_some());
    assert_eq!(
        attestation["samples"]["baseline_reconcile_ms"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert!(attestation["policies"]["projection"].as_str().is_some());

    let results = attestation["experiment_results"].as_array().unwrap();
    for result in results {
        assert_eq!(
            result["control"]["raw_samples"].as_array().unwrap().len(),
            300
        );
        assert_eq!(
            result["candidate"]["raw_samples"].as_array().unwrap().len(),
            300
        );
        assert!(result["control"]["variance"]["p95_micros"].is_number());
        assert!(result["candidate"]["variance"]["mad_micros"].is_number());
        assert_eq!(
            result["control"]["accepted_outcome"]["outcome_hash"],
            result["candidate"]["accepted_outcome"]["outcome_hash"]
        );
    }
    assert_eq!(results[5]["control"]["catalogue_file_count"], 128);
    assert_eq!(results[5]["candidate"]["catalogue_file_count"], 1280);
    assert_ne!(
        results[5]["control"]["generation_id"],
        results[5]["candidate"]["generation_id"]
    );
    assert_eq!(
        results[4]["control"]["raw_samples"][0]["serving_fallback"],
        false
    );
    assert_eq!(
        results[4]["candidate"]["raw_samples"][0]["serving_fallback"],
        true
    );
    assert_eq!(
        results[3]["control"]["raw_samples"][0]["context_hash"],
        results[3]["candidate"]["raw_samples"][0]["context_hash"]
    );

    for prohibited in [
        "prompt",
        "source_content",
        "source_body",
        "task_text",
        "raw_task",
    ] {
        assert!(!attestation_text.contains(prohibited), "found {prohibited}");
    }
    assert!(attestation_path
        .replace('\\', "/")
        .contains("target/atlas-bench/"));
    std::fs::remove_file(attestation_path).unwrap();
}
