use std::collections::BTreeSet;

use assert_cmd::Command;
use serde_json::Value;
use sha2::{Digest, Sha256};
use workspace_atlas::context_route::{Availability, CapabilityFeature};

const SCENARIO: &str = "tests/fixtures/context_yield/benchmark-scenario.json";
const ATTESTATION_PREFIX: &str = "run attestation: ";
const CAPACITY_PLAN_PREFIX: &str = "capacity execution plan: ";

fn canonical_json_sha256(value: &Value) -> String {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    hex::encode(Sha256::digest(bytes))
}

fn copy_capacity_manifests(destination: &std::path::Path) {
    for name in [
        "repo-small-v1.json",
        "repo-medium-v1.json",
        "repo-large-v1.json",
        "accepted-outcomes-v2.json",
    ] {
        std::fs::copy(
            std::path::Path::new("tests/fixtures/capacity").join(name),
            destination.join(name),
        )
        .unwrap();
    }
}

fn capacity_plan(shard_index: Option<u64>) -> Value {
    let evidence_root = tempfile::Builder::new()
        .prefix("t34-shard-plan-")
        .tempdir_in("target")
        .unwrap();
    let mut arguments = vec![
        "--capacity-plan".to_string(),
        "--manifest".to_string(),
        SCENARIO.to_string(),
        "--capacity-manifest-dir".to_string(),
        "tests/fixtures/capacity".to_string(),
        "--evidence-dir".to_string(),
        evidence_root.path().to_string_lossy().into_owned(),
    ];
    if let Some(index) = shard_index {
        arguments.extend([
            "--capacity-shard-index".to_string(),
            index.to_string(),
            "--capacity-shard-count".to_string(),
            "4".to_string(),
        ]);
    }
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args(arguments)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    let plan_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix(CAPACITY_PLAN_PREFIX))
        .expect("capacity planner prints its bounded evidence path");
    serde_json::from_str(&std::fs::read_to_string(plan_path).unwrap()).unwrap()
}

#[test]
fn capacity_manifest_validation_is_independent_of_default_text_newlines() {
    let script = r#"
import runpy
from pathlib import Path

original_write_text = Path.write_text

def write_text_with_lf_default(self, data, encoding=None, errors=None, newline=None):
    return original_write_text(
        self,
        data,
        encoding=encoding,
        errors=errors,
        newline="\n" if newline is None else newline,
    )

Path.write_text = write_text_with_lf_default
generator = runpy.run_path("tests/fixtures/pilot/generate_fixture.py")
manifest = Path("tests/fixtures/capacity/repo-small-v1.json").read_bytes()
generator["validate_capacity_manifest_bytes"](manifest)
"#;
    let output = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-B", "-c", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn capacity_plan_covers_all_immutable_strata_conditions_and_samples() {
    let evidence_root = tempfile::Builder::new()
        .prefix("t30b-plan-")
        .tempdir_in("target")
        .unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-plan",
            "--manifest",
            SCENARIO,
            "--capacity-manifest-dir",
            "tests/fixtures/capacity",
            "--evidence-dir",
            evidence_root.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    let plan_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix(CAPACITY_PLAN_PREFIX))
        .expect("capacity planner prints its bounded evidence path");
    let plan: Value = serde_json::from_str(&std::fs::read_to_string(plan_path).unwrap()).unwrap();
    assert_eq!(plan["strata"].as_array().unwrap().len(), 3);
    assert_eq!(plan["matrix"].as_array().unwrap().len(), 360);
    assert_eq!(plan["procedure"]["warmups"], 5);
    assert_eq!(plan["procedure"]["warm_samples"], 100);
    assert_eq!(plan["procedure"]["cold_samples"], 20);
    assert_eq!(plan["procedure"]["repetitions"], 3);
    assert_eq!(plan["evidence_contract"]["maximum_raw_records"], 135_000);
    assert_eq!(plan["accepted_outcome_contract"]["schema_version"], "2.0.0");
    assert_eq!(plan["accepted_outcome_contract"]["outcome_count"], 22);
    assert_eq!(
        plan["accepted_outcome_contract"]["supersedes"]["contract"],
        "capacity-manifest-v1-accepted-outcome"
    );
    assert_eq!(
        plan["accepted_outcome_contract"]["supersedes"]["manifests"][0]["evidence_counts"],
        serde_json::json!({
            "conflicts": 0,
            "coverage": 115,
            "diagnostics": 0,
            "effects": 1,
            "relationships": 114,
            "symbols": 115,
        })
    );
    assert_eq!(
        plan["evidence_contract"]["accepted_outcome_preflight"]["required_before_campaign"],
        true
    );
    assert_eq!(
        plan["evidence_contract"]["accepted_outcome_preflight"]["executor"],
        "production"
    );
    let strata = plan["strata"].as_array().unwrap();
    assert_eq!(
        strata
            .iter()
            .map(|manifest| (
                manifest["dataset_name"].as_str().unwrap(),
                manifest["counts"]["eligible_files"].as_u64().unwrap(),
                manifest["counts"]["indexed_files"].as_u64().unwrap(),
                manifest["counts"]["changed_files"].as_u64().unwrap(),
                manifest["files"].as_array().unwrap().len(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("repo-small-v1", 128, 128, 2, 128),
            ("repo-medium-v1", 1024, 1024, 11, 1024),
            ("repo-large-v1", 8192, 8192, 82, 8192),
        ]
    );
    let matrix = plan["matrix"].as_array().unwrap();
    let phases = matrix
        .iter()
        .map(|row| row["phase"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let providers = matrix
        .iter()
        .map(|row| row["provider_condition"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let profiles = matrix
        .iter()
        .map(|row| row["profile"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(phases.len(), 10);
    assert_eq!(providers.len(), 4);
    assert_eq!(profiles, BTreeSet::from(["audit", "small", "standard"]));
    let capabilities = workspace_atlas::context_route::discover_context_capabilities();
    assert_eq!(
        capabilities.availability[&CapabilityFeature::ProgressiveExecution],
        Availability::Available
    );
    assert_eq!(
        capabilities.availability[&CapabilityFeature::DeepContextIr],
        Availability::Available
    );
    assert!(phases.contains("progressive-deep-route-compiler"));
    assert!(matrix.iter().all(|row| {
        row["sample_plan"]["unscored_warmups"] == 5
            && row["sample_plan"]["scored_warm_samples"] == 100
            && row["sample_plan"]["cold_samples"] == 20
            && row["sample_plan"]["independent_repetitions"] == 3
            && row["sample_plan"]["percentile_method"] == "nearest_rank"
    }));
    assert!(matrix.iter().all(|row| {
        !row["provider_members"].as_array().unwrap().is_empty()
            && row["profile_inputs"]["records"].as_u64().unwrap() > 0
            && row["execution"]["action"].as_str().unwrap().contains("::")
            && row["execution"]["fixture_lifecycle"].is_string()
            && row["execution"]["catalogue_lifecycle"].is_string()
            && row["execution"]["cache_state"]["warm"].is_string()
            && row["execution"]["cache_state"]["cold"].is_string()
            && row["execution"]["serving_state"].is_string()
    }));
    let cold_initialization = matrix
        .iter()
        .find(|row| row["phase"] == "cold-initialization-reconcile")
        .unwrap();
    assert_eq!(
        cold_initialization["execution"]["fixture_lifecycle"],
        "a unique prepared fixture/configuration lifecycle for every warmup, warm, and cold sample"
    );

    assert_eq!(
        cold_initialization["execution"]["preparation"],
        "per_attempt_catalogue"
    );
    assert_eq!(
        cold_initialization["execution"]["reset"],
        "release_catalogue"
    );
    assert_eq!(cold_initialization["execution"]["route_attempts"], 0);
    assert_eq!(
        cold_initialization["execution"]["measurements"]["reconcile"],
        "observed"
    );
    assert_eq!(
        cold_initialization["execution"]["cache_state"]["warm"],
        "fresh-fixture-cold"
    );
    assert_eq!(
        cold_initialization["execution"]["cache_state"]["cold"],
        "fresh-fixture-cold"
    );
    let direct = matrix
        .iter()
        .find(|row| row["phase"] == "direct-route-compiler")
        .unwrap();
    assert_eq!(direct["execution"]["route_attempts"], 1);
    assert_eq!(direct["execution"]["measurements"]["catalogue"], false);
    let light = matrix
        .iter()
        .find(|row| row["phase"] == "light-route-compiler")
        .unwrap();
    assert_eq!(
        light["execution"]["action"],
        "context_application::run_catalogue_governor with ATLAS_LIGHT floor/ceiling over explicit bounded targets"
    );
    assert_eq!(light["execution"]["route_attempts"], 1);
    assert_eq!(
        light["execution"]["measurements"]["compiler"]["applicable"],
        false
    );
    let progressive = matrix
        .iter()
        .find(|row| row["phase"] == "progressive-deep-route-compiler")
        .unwrap();
    assert_eq!(
        progressive["execution"]["action"],
        "context_application::run_catalogue_governor through DIRECT -> ATLAS_LIGHT -> ATLAS_DEEP Context IR 2.0.0"
    );
    assert_eq!(progressive["execution"]["route_attempts"], 3);
    assert_eq!(
        progressive["execution"]["measurements"]["compiler"]["applicable"],
        true
    );
    assert_eq!(
        progressive["execution"]["measurements"]["ready_versus_truth"],
        true
    );
    let small_rust = matrix
        .iter()
        .find(|row| {
            row["dataset_name"] == "repo-small-v1" && row["provider_condition"] == "rust-only"
        })
        .unwrap();
    assert_eq!(small_rust["provider_applicability"], "not_applicable");
    assert!(small_rust["configuration_hash"].is_null());
    let small_combined = matrix
        .iter()
        .find(|row| {
            row["dataset_name"] == "repo-small-v1" && row["provider_condition"] == "combined"
        })
        .unwrap();
    assert_eq!(
        small_combined["provider_members"],
        serde_json::json!([
            "file_metadata@1.0.0",
            "document_config@1.0.0",
            "structural_typescript@1.0.0",
            "safe_text_fallback@1.0.0",
            "scip-typescript@0.4.0"
        ])
    );
    assert!(!small_combined["provider_members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|member| member.as_str().unwrap().starts_with("rust-analyzer")));
    let medium_combined = matrix
        .iter()
        .find(|row| {
            row["dataset_name"] == "repo-medium-v1" && row["provider_condition"] == "combined"
        })
        .unwrap();
    assert_eq!(
        medium_combined["provider_members"],
        serde_json::json!([
            "file_metadata@1.0.0",
            "document_config@1.0.0",
            "structural_typescript@1.0.0",
            "safe_text_fallback@1.0.0",
            "scip-typescript@0.4.0",
            "rust-analyzer@1.94.1"
        ])
    );
    let contract = &plan["evidence_contract"];
    assert_eq!(
        contract["outcome_states"],
        serde_json::json!([
            "success",
            "timeout",
            "absent",
            "unsupported",
            "degraded",
            "cancelled",
            "failed"
        ])
    );
    assert_eq!(contract["timing_is_canonical_identity"], false);
    assert!(contract["non_success_retention"]
        .as_str()
        .unwrap()
        .contains("never zero-filled"));
    let canonical = contract["canonical_identity_fields"].as_array().unwrap();
    let transient = contract["transient_evidence_fields"].as_array().unwrap();
    assert!(!canonical.iter().any(|field| transient.contains(field)));
    assert_eq!(
        contract["setup_fields"],
        serde_json::json!([
            "provider_install_duration_ms",
            "provider_download_duration_ms"
        ])
    );
    assert_eq!(
        contract["atlas_work_fields"],
        serde_json::json!(["atlas_runtime_duration_ms", "deterministic_work_units"])
    );
    assert_eq!(
        contract["compiler_measurement_fields"],
        serde_json::json!([
            "compiler_selected_records",
            "compiler_selected_source_bytes",
            "compiler_selected_estimated_tokens",
            "compiler_omitted_records",
            "compiler_truncated",
            "compiler_work_units_consumed",
            "compiler_fallback"
        ])
    );
    assert_eq!(
        contract["record_bound_policy"],
        "fail before writing record 135001; never truncate or change retention method"
    );
    let required = contract["required_result_fields"].as_array().unwrap();
    for field in [
        "compiler_selected_source_bytes",
        "compiler_selected_estimated_tokens",
        "compiler_truncated",
    ] {
        assert!(required.contains(&Value::from(field)), "missing {field}");
    }
    assert!(std::path::Path::new(plan_path).starts_with(evidence_root.path()));
}

#[test]
fn four_capacity_shards_are_a_stable_exact_disjoint_union_of_every_attempt() {
    let full = capacity_plan(None);
    let full_rows = full["matrix"].as_array().unwrap();
    assert_eq!(full_rows.len(), 360);
    assert_eq!(full["execution_scope"]["mode"], "full");
    assert_eq!(full["execution_scope"]["global_attempts"], 135_000);
    assert_eq!(full["execution_scope"]["selected_attempts"], 135_000);
    let global_plan_sha256 = full["execution_scope"]["global_plan_sha256"]
        .as_str()
        .unwrap();
    assert_eq!(global_plan_sha256.len(), 64);

    let full_row_identities = full_rows
        .iter()
        .map(|row| {
            serde_json::to_string(&serde_json::json!([
                row["dataset_name"],
                row["profile"],
                row["provider_condition"],
                row["phase"],
            ]))
            .unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(full_row_identities.len(), 360);

    let mut union_rows = BTreeSet::new();
    let mut union_attempts = BTreeSet::new();
    for shard_index in 0..4 {
        let shard = capacity_plan(Some(shard_index));
        let scope = &shard["execution_scope"];
        assert_eq!(scope["mode"], "shard");
        assert_eq!(scope["shard_index"], shard_index);
        assert_eq!(scope["shard_count"], 4);
        assert_eq!(scope["global_matrix_rows"], 360);
        assert_eq!(scope["selected_matrix_rows"], 90);
        assert_eq!(scope["global_attempts"], 135_000);
        assert_eq!(scope["selected_attempts"], 33_750);
        assert_eq!(scope["global_plan_sha256"], global_plan_sha256);
        let rows = shard["matrix"].as_array().unwrap();
        assert_eq!(rows.len(), 90);
        for row in rows {
            let global_row_index = row["global_matrix_row_index"].as_u64().unwrap();
            assert_eq!(global_row_index % 4, shard_index);
            let row_identity = serde_json::to_string(&serde_json::json!([
                row["dataset_name"],
                row["profile"],
                row["provider_condition"],
                row["phase"],
            ]))
            .unwrap();
            assert!(
                union_rows.insert(row_identity.clone()),
                "row overlaps another shard: {row_identity}"
            );
            for repetition in 1..=3 {
                for (sample_class, samples) in [("warmup", 5), ("warm", 100), ("cold", 20)] {
                    for sample_index in 1..=samples {
                        let attempt = serde_json::to_string(&serde_json::json!([
                            row_identity,
                            repetition,
                            sample_class,
                            sample_index,
                        ]))
                        .unwrap();
                        assert!(
                            union_attempts.insert(attempt),
                            "attempt identity overlaps another shard"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(union_rows, full_row_identities);
    assert_eq!(union_attempts.len(), 135_000);
}

#[test]
fn capacity_shard_arguments_fail_closed_unless_the_closed_pair_is_valid() {
    for arguments in [
        vec!["--capacity-shard-index", "0"],
        vec!["--capacity-shard-count", "4"],
        vec!["--capacity-shard-index", "0", "--capacity-shard-count", "3"],
        vec!["--capacity-shard-index", "4", "--capacity-shard-count", "4"],
        vec![
            "--capacity-shard-index",
            "not-a-number",
            "--capacity-shard-count",
            "4",
        ],
    ] {
        let evidence_root = tempfile::Builder::new()
            .prefix("t34-invalid-shard-")
            .tempdir_in("target")
            .unwrap();
        let mut command = Command::cargo_bin("atlas-bench").unwrap();
        command
            .arg("--capacity-plan")
            .args(arguments)
            .args(["--evidence-dir", evidence_root.path().to_str().unwrap()]);
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{stdout}");
        assert!(stdout.contains("capacity shard"), "{stdout}");
        assert!(!evidence_root
            .path()
            .join("capacity-execution-plan-v1.json")
            .exists());
    }
}

#[test]
fn capacity_manifest_mutation_fails_before_evidence_is_written() {
    let manifests = tempfile::tempdir().unwrap();
    copy_capacity_manifests(manifests.path());
    let small = manifests.path().join("repo-small-v1.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&small).unwrap()).unwrap();
    manifest["providers"][1]["version"] = Value::from("0.4.1");
    std::fs::write(&small, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let evidence_root = tempfile::Builder::new()
        .prefix("t30b-mutation-")
        .tempdir_in("target")
        .unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-plan",
            "--manifest",
            SCENARIO,
            "--capacity-manifest-dir",
            manifests.path().to_str().unwrap(),
            "--evidence-dir",
            evidence_root.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("repo-small-v1 immutable file digest mismatch"),
        "{stdout}"
    );
    assert!(!stdout.contains(CAPACITY_PLAN_PREFIX), "{stdout}");
    assert!(!evidence_root
        .path()
        .join("capacity-execution-plan-v1.json")
        .exists());
}

#[test]
fn resealed_stale_outcome_contract_stops_campaign_at_real_preflight() {
    let manifests = tempfile::tempdir().unwrap();
    copy_capacity_manifests(manifests.path());
    let contract_path = manifests.path().join("accepted-outcomes-v2.json");
    let mut contract: Value =
        serde_json::from_slice(&std::fs::read(&contract_path).unwrap()).unwrap();
    let outcome = &mut contract["outcomes"][0];
    outcome["evidence_counts"]["symbols"] = Value::from(0);
    let accepted = canonical_json_sha256(&serde_json::json!({
        "dataset_name": outcome["dataset_name"],
        "evidence_counts": outcome["evidence_counts"],
        "target_identity": outcome["target_identity"],
    }));
    outcome["expected_accepted_outcome_hash"] = Value::from(accepted.clone());
    outcome["expected_ready_versus_truth_result_hash"] =
        Value::from(canonical_json_sha256(&serde_json::json!({
            "accepted_outcome_hash": accepted,
            "equivalence": "ready-serving-equals-truth-fallback",
        })));
    let contract_bytes = serde_json::to_vec_pretty(&contract).unwrap();
    std::fs::write(&contract_path, &contract_bytes).unwrap();

    let scenario_root = tempfile::tempdir().unwrap();
    let scenario_path = scenario_root.path().join("scenario.json");
    let mut scenario: Value =
        serde_json::from_str(&std::fs::read_to_string(SCENARIO).unwrap()).unwrap();
    scenario["capacity"]["accepted_outcome_contract"]["file_sha256"] =
        Value::from(hex::encode(Sha256::digest(&contract_bytes)));
    std::fs::write(
        &scenario_path,
        serde_json::to_vec_pretty(&scenario).unwrap(),
    )
    .unwrap();
    let evidence = tempfile::Builder::new()
        .prefix("t30ab-stale-preflight-")
        .tempdir_in("target")
        .unwrap();

    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-run",
            "--manifest",
            scenario_path.to_str().unwrap(),
            "--capacity-manifest-dir",
            manifests.path().to_str().unwrap(),
            "--evidence-dir",
            evidence.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("production accepted-outcome preflight mismatch"),
        "{stdout}"
    );
    assert!(!evidence
        .path()
        .join("capacity-accepted-outcome-preflight-v2.json")
        .exists());
    assert!(!evidence
        .path()
        .join("capacity-raw-observations-v1.ndjson")
        .exists());
}

#[test]
fn capacity_scenario_matrix_and_record_bound_mutations_fail_closed() {
    for (field, value, expected) in [
        ("maximum_raw_records", Value::from(134_999), "bounds"),
        (
            "phases",
            serde_json::json!([
                "cold-initialization-reconcile",
                "no-change-reconcile",
                "fixed-incremental-reconcile",
                "ready-serving-query-compiler",
                "truth-fallback-query-compiler",
                "capability-discovery",
                "direct-route-compiler",
                "light-route-compiler",
                "catalogue-growth"
            ]),
            "phases",
        ),
        (
            "provider_conditions",
            serde_json::json!(["builtin-only"]),
            "provider conditions",
        ),
        (
            "outcome_states",
            serde_json::json!(["success"]),
            "outcome states",
        ),
        (
            "transient_evidence_fields",
            serde_json::json!(["wall_duration_ms"]),
            "timing and setup/work separation",
        ),
        (
            "limitations",
            serde_json::json!(["private dataset labels are supported limits"]),
            "bounds or limitations",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let invalid_path = directory.path().join("invalid-capacity-scenario.json");
        let mut scenario: Value =
            serde_json::from_str(&std::fs::read_to_string(SCENARIO).unwrap()).unwrap();
        scenario["capacity"][field] = value;
        std::fs::write(&invalid_path, serde_json::to_vec_pretty(&scenario).unwrap()).unwrap();
        let output = Command::cargo_bin("atlas-bench")
            .unwrap()
            .args([
                "--capacity-plan",
                "--manifest",
                invalid_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{field}: {stdout}");
        assert!(stdout.contains(expected), "{field}: {stdout}");
        assert!(!stdout.contains(CAPACITY_PLAN_PREFIX), "{field}: {stdout}");
    }
}

#[test]
fn capacity_evidence_path_outside_target_is_rejected() {
    let outside_target = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-plan",
            "--manifest",
            SCENARIO,
            "--evidence-dir",
            outside_target.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("package-excluded target/**"), "{stdout}");
    assert!(!stdout.contains(CAPACITY_PLAN_PREFIX), "{stdout}");
    assert!(!outside_target
        .path()
        .join("capacity-execution-plan-v1.json")
        .exists());
}

#[test]
fn manifest_validation_fails_closed_before_benchmark_execution() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_path = directory.path().join("invalid-scenario.json");
    let mut scenario: Value =
        serde_json::from_str(&std::fs::read_to_string(SCENARIO).unwrap()).unwrap();
    scenario["procedure"]["warm_samples"] = Value::from(99);
    std::fs::write(&invalid_path, serde_json::to_vec_pretty(&scenario).unwrap()).unwrap();

    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args(["--manifest", invalid_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("warm_samples must equal 100"), "{stdout}");
    assert!(!stdout.contains(ATTESTATION_PREFIX), "{stdout}");
}

#[test]
fn stable_scenario_generates_checkout_bound_run_attestation() {
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args(["--manifest", SCENARIO])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("--- 15/15 checks passed"), "{stdout}");

    let attestation_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix(ATTESTATION_PREFIX))
        .expect("benchmark prints generated attestation path");
    let attestation_text = std::fs::read_to_string(attestation_path).unwrap();
    let attestation: Value = serde_json::from_str(&attestation_text).unwrap();

    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap();
    assert_eq!(attestation["revision"], head.trim());
    assert_eq!(attestation["scenario_schema_version"], "1.0.0");
    assert_eq!(attestation["procedure"]["warm_samples"], 100);
    assert_eq!(attestation["procedure"]["cold_samples"], 20);
    assert_eq!(attestation["procedure"]["repetitions"], 3);
    assert_eq!(attestation["samples"]["correctness_checks_passed"], 15);
    assert_eq!(attestation["samples"]["correctness_checks_total"], 15);
    assert!(attestation["toolchain"].as_str().unwrap().contains("rustc"));
    assert!(attestation["platform"]["cpu"].as_str().is_some());
    assert_eq!(attestation["cache_state"], "uncontrolled");
    assert!(!attestation_text.contains("task_text"));
    assert!(!attestation_text.contains("source_body"));
    assert!(!attestation_text.contains("prompt"));

    std::fs::remove_file(attestation_path).unwrap();
}

#[test]
fn shipped_debug_binary_rejects_every_test_executor_value_before_evidence_creation() {
    for value in [
        "mixed",
        "duplicate",
        "missing",
        "over-bound",
        "",
        "attacker-controlled",
    ] {
        let evidence_root = tempfile::Builder::new()
            .prefix("t30b-env-reject-")
            .tempdir_in("target")
            .unwrap();
        let destination = evidence_root.path().join("not-created");
        let output = Command::cargo_bin("atlas-bench")
            .unwrap()
            .env("ATLAS_BENCH_TEST_EXECUTOR", value)
            .args([
                "--capacity-run",
                "--manifest",
                SCENARIO,
                "--capacity-manifest-dir",
                "tests/fixtures/capacity",
                "--evidence-dir",
                destination.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{value:?}: {stdout}");
        assert!(
            stdout.contains("ATLAS_BENCH_TEST_EXECUTOR is forbidden"),
            "{value:?}: {stdout}"
        );
        assert!(
            !destination.exists(),
            "{value:?}: rejection must precede evidence directory creation"
        );
    }
}

#[test]
fn capacity_plan_outputs_are_exclusive_and_never_overwrite() {
    let evidence_root = tempfile::Builder::new()
        .prefix("t30b-exclusive-")
        .tempdir_in("target")
        .unwrap();
    let command = || {
        Command::cargo_bin("atlas-bench")
            .unwrap()
            .args([
                "--capacity-plan",
                "--manifest",
                SCENARIO,
                "--capacity-manifest-dir",
                "tests/fixtures/capacity",
                "--evidence-dir",
                evidence_root.path().to_str().unwrap(),
            ])
            .output()
            .unwrap()
    };
    let first = command();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let plan_path = evidence_root.path().join("capacity-execution-plan-v1.json");
    let original = std::fs::read(&plan_path).unwrap();
    let second = command();
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(!second.status.success(), "{stdout}");
    assert!(stdout.contains("already exists"), "{stdout}");
    assert_eq!(std::fs::read(&plan_path).unwrap(), original);

    #[cfg(windows)]
    {
        std::fs::remove_file(&plan_path).unwrap();
        let sink = evidence_root.path().join("hard-link-sink.json");
        std::fs::write(&sink, b"do not overwrite").unwrap();
        std::fs::hard_link(&sink, &plan_path).unwrap();
        let linked = command();
        let stdout = String::from_utf8_lossy(&linked.stdout);
        assert!(!linked.status.success(), "{stdout}");
        assert!(stdout.contains("already exists"), "{stdout}");
        assert_eq!(std::fs::read(&sink).unwrap(), b"do not overwrite");
        std::fs::remove_file(&plan_path).unwrap();
        match std::os::windows::fs::symlink_file(&sink, &plan_path) {
            Ok(()) => {
                let linked = command();
                let stdout = String::from_utf8_lossy(&linked.stdout);
                assert!(!linked.status.success(), "{stdout}");
                assert!(stdout.contains("already exists"), "{stdout}");
                assert_eq!(std::fs::read(&sink).unwrap(), b"do not overwrite");
            }
            Err(error) => {
                assert!(
                    error.raw_os_error() == Some(1314)
                        || matches!(
                            error.kind(),
                            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
                        ),
                    "unexpected file-symlink creation failure: {error}"
                );
            }
        }
    }
}

#[test]
fn capacity_raw_output_rejects_preexisting_final_component() {
    let evidence_root = tempfile::Builder::new()
        .prefix("t30b-raw-exclusive-")
        .tempdir_in("target")
        .unwrap();
    let raw_path = evidence_root
        .path()
        .join("capacity-raw-observations-v1.ndjson");
    std::fs::write(&raw_path, b"existing raw sink").unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-run",
            "--manifest",
            SCENARIO,
            "--capacity-manifest-dir",
            "tests/fixtures/capacity",
            "--evidence-dir",
            evidence_root.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("already exists"), "{stdout}");
    assert_eq!(std::fs::read(&raw_path).unwrap(), b"existing raw sink");
}

#[cfg(windows)]
#[test]
fn capacity_plan_rejects_windows_directory_junction_escape() {
    let evidence_parent = tempfile::Builder::new()
        .prefix("t30b-junction-")
        .tempdir_in("target")
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let junction = evidence_parent.path().join("escape");
    let status = std::process::Command::new("cmd")
        .args([
            "/c",
            "mklink",
            "/J",
            junction.to_str().unwrap(),
            outside.path().to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "create test directory junction");
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-plan",
            "--manifest",
            SCENARIO,
            "--evidence-dir",
            junction.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("package-excluded target/**")
            || stdout.contains("reparse point")
            || stdout.contains("not a physical directory"),
        "{stdout}"
    );
    assert!(!outside
        .path()
        .join("capacity-execution-plan-v1.json")
        .exists());
    std::fs::remove_dir(&junction).unwrap();
}

#[cfg(windows)]
#[test]
fn capacity_plan_rejects_nested_windows_junction_without_outside_side_effect() {
    let evidence_parent = tempfile::Builder::new()
        .prefix("t30b-nested-junction-")
        .tempdir_in("target")
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let junction = evidence_parent.path().join("escape");
    let status = std::process::Command::new("cmd")
        .args([
            "/c",
            "mklink",
            "/J",
            junction.to_str().unwrap(),
            outside.path().to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "create test directory junction");
    let nested = junction.join("must-not-be-created").join("evidence");
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-plan",
            "--manifest",
            SCENARIO,
            "--evidence-dir",
            nested.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("package-excluded target/**")
            || stdout.contains("physical directory")
            || stdout.contains("reparse point"),
        "{stdout}"
    );
    assert!(!outside.path().join("must-not-be-created").exists());
    std::fs::remove_dir(&junction).unwrap();
}

#[test]
fn process_tree_memory_entrypoint_is_private_and_package_excluded() {
    let output = std::process::Command::new("cargo")
        .args(["package", "--list", "--locked", "--allow-dirty"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths = String::from_utf8(output.stdout).unwrap();
    assert_eq!(paths.lines().count(), 120);
    assert!(!paths
        .lines()
        .any(|path| path == "scripts/process-tree-memory.py"));
    assert!(!paths
        .lines()
        .any(|path| path == "scripts/test_process_tree_memory.py"));
    assert!(std::path::Path::new("scripts/process-tree-memory.py").is_file());
    assert!(std::path::Path::new("scripts/test_process_tree_memory.py").is_file());
}

#[test]
fn segmented_process_memory_contract_rolls_over_and_rejects_mutations() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = std::process::Command::new(python)
        .args([
            "-B",
            "-m",
            "unittest",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segmented_evidence_round_trips_count_and_byte_rollovers",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segmented_manifest_rejects_every_binding_and_sequence_mutation",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segmented_evidence_rejects_external_hard_link",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segmented_validation_binds_consumption_to_open_handle",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_memory_generations_and_undeclared_v2_outputs_cannot_mix",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segmented_namespace_rejects_broken_memory_artifact_link",
            "scripts.test_process_tree_memory.BaselineAndEvidenceTests.test_segment_count_bound_fails_before_extra_file_and_bounds_descriptors",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Ran 7 tests"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn capacity_memory_cli_rejects_internal_and_duplicate_options_before_evidence() {
    for arguments in [
        vec![
            "--capacity-memory",
            "--memory-label-state",
            "target/attacker-labels.json",
        ],
        vec![
            "--capacity-memory",
            "--evidence-dir",
            "target/one",
            "--evidence-dir",
            "target/two",
        ],
    ] {
        let output = Command::cargo_bin("atlas-bench")
            .unwrap()
            .args(arguments)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{stdout}");
        assert!(
            stdout.contains("unsupported capacity option")
                || stdout.contains("duplicate capacity option"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("process-tree memory raw evidence:"),
            "{stdout}"
        );
    }
}

#[test]
fn direct_capacity_run_rejects_unbound_memory_label_state_before_raw_evidence() {
    let evidence_root = tempfile::Builder::new()
        .prefix("t30c-label-binding-")
        .tempdir_in("target")
        .unwrap();
    let unrelated = evidence_root.path().join("unrelated-labels.json");
    std::fs::write(&unrelated, b"{}").unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .args([
            "--capacity-run",
            "--manifest",
            SCENARIO,
            "--capacity-manifest-dir",
            "tests/fixtures/capacity",
            "--evidence-dir",
            evidence_root.path().to_str().unwrap(),
            "--memory-label-state",
            unrelated.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("memory label state must be exactly"),
        "{stdout}"
    );
    assert!(!evidence_root
        .path()
        .join("capacity-raw-observations-v1.ndjson")
        .exists());
}

#[test]
fn capacity_log_cli_refuses_incomplete_evidence_before_any_payload() {
    let evidence_root = tempfile::Builder::new()
        .prefix("t31-incomplete-log-")
        .tempdir_in("target")
        .unwrap();
    let output = Command::cargo_bin("atlas-bench")
        .unwrap()
        .env("ATLAS_RUNNER_NAME", "local-red-fixture")
        .env("ATLAS_RUNNER_OS", "Windows")
        .env("ATLAS_RUNNER_ARCH", "X64")
        .env("ImageOS", "win25")
        .env("ImageVersion", "20260901.1")
        .args([
            "--capacity-log",
            "--manifest",
            SCENARIO,
            "--evidence-dir",
            evidence_root.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("[FAIL] capacity log -"), "{stdout}");
    assert!(stdout.contains("missing"), "{stdout}");
    assert!(!stdout.contains("ATLAS_CAPACITY_LOG"), "{stdout}");
}
