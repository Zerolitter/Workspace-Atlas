use workspace_atlas::context_yield::{
    compare_context_yield_reports_v15, ContextYieldReportV15, ExperimentalProfileManifest,
    YieldIncomparableReason, EXPERIMENTAL_PROFILE_MANIFEST_SCHEMA_VERSION, H3_A_BENCHMARK_MANIFEST,
    H3_A_PROFILE_VERSION,
};

const PROFILE_FIXTURE: &str = include_str!("fixtures/context_yield/context-profiles.example.json");
const REPORT_FIXTURE: &str = include_str!("fixtures/context_yield/context-yield.example.json");

fn profile_manifest() -> ExperimentalProfileManifest {
    serde_json::from_str(PROFILE_FIXTURE).expect("the experimental profile fixture must decode")
}

fn report() -> ContextYieldReportV15 {
    serde_json::from_str(REPORT_FIXTURE).expect("the Context Yield fixture must decode")
}

#[test]
fn h3_a_profiles_are_frozen_bounded_and_round_trip() {
    let manifest = profile_manifest();
    manifest
        .validate()
        .expect("the accepted H3-A profile contract must validate");

    assert_eq!(
        manifest.schema_version,
        EXPERIMENTAL_PROFILE_MANIFEST_SCHEMA_VERSION
    );
    assert_eq!(manifest.benchmark_manifest, H3_A_BENCHMARK_MANIFEST);
    assert_eq!(manifest.profiles.len(), 3);
    assert_eq!(manifest.profiles[0].name, "small");
    assert_eq!(manifest.profiles[1].name, "standard");
    assert_eq!(manifest.profiles[2].name, "audit");
    assert!(manifest
        .profiles
        .iter()
        .all(|profile| profile.version == H3_A_PROFILE_VERSION));
    assert_eq!(manifest.profiles[0].uncertainty_reserve_percent, 15);
    assert_eq!(manifest.profiles[1].uncertainty_reserve_percent, 12);
    assert_eq!(manifest.profiles[2].work_units, 100_000);

    let fixture_value: serde_json::Value = serde_json::from_str(PROFILE_FIXTURE).unwrap();
    assert_eq!(serde_json::to_value(&manifest).unwrap(), fixture_value);
}

#[test]
fn profile_manifest_is_closed_and_fails_closed_on_drift() {
    let mut unknown: serde_json::Value = serde_json::from_str(PROFILE_FIXTURE).unwrap();
    unknown["profiles"][0]["production_default"] = true.into();
    let error = serde_json::from_value::<ExperimentalProfileManifest>(unknown).unwrap_err();
    assert!(error
        .to_string()
        .contains("unknown field `production_default`"));

    let mut reserve_drift = profile_manifest();
    reserve_drift.profiles[0].uncertainty_reserve_percent = 100;
    assert!(reserve_drift.validate().is_err());

    let mut benchmark_drift = profile_manifest();
    benchmark_drift.benchmark_manifest = "other.json".to_string();
    assert!(benchmark_drift.validate().is_err());
}

#[test]
fn yield_reports_record_and_compare_one_declared_profile_variable() {
    let manifest = profile_manifest();
    manifest.validate().unwrap();

    let mut small = report();
    small.content.experimental_profile = Some(manifest.profiles[0].clone());
    let mut audit = report();
    audit.content.experimental_profile = Some(manifest.profiles[2].clone());

    let comparison = compare_context_yield_reports_v15(&small, &audit)
        .expect("accepted reports may vary the declared experimental profile");
    assert_eq!(comparison.profile_a.as_ref().unwrap().name, "small");
    assert_eq!(comparison.profile_b.as_ref().unwrap().name, "audit");

    let recorded = serde_json::to_string(&small).unwrap();
    let round_tripped: ContextYieldReportV15 = serde_json::from_str(&recorded).unwrap();
    assert_eq!(
        round_tripped.content.experimental_profile,
        small.content.experimental_profile
    );

    let mut undeclared = report();
    undeclared.content.experimental_profile = None;
    assert_eq!(
        compare_context_yield_reports_v15(&small, &undeclared).unwrap_err(),
        YieldIncomparableReason::DifferentExperimentalProfileContract
    );
}
