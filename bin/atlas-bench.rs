//! Workspace Atlas deterministic acceptance benchmark.
//!
//! Generates the public pilot fixture, drives it through the production
//! pipeline, and checks the fixture manifest's catalogue, query, lifecycle,
//! and incremental-reconcile expectations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use workspace_atlas::config::Config;
use workspace_atlas::context_application::{
    run_catalogue_governor, GovernorDeepLimits, GovernorRunRequest, GovernorRunSemanticInput,
};
use workspace_atlas::context_ir::{
    RawTaskRetention, TaskKind, TaskKindSource, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::context_metrics::ContextExecutionCounters;
use workspace_atlas::context_route::{
    decide_context_route, discover_context_capabilities, AtlasIntent, AttemptState, Availability,
    CallerCapabilityState, CapabilityDeficitReason, CapabilityFeature, ContextExecution,
    ContextPayload, ContextRouteRequest, DeepContractVersions, GenerationObservation,
    LightOperation, NegotiationRequest, RequiredCapability, Route, RouteDeficit, SeedSummary,
    TerminalReason, VersionedOperation, CONTEXT_CAPABILITIES_VERSION, CONTEXT_EXECUTION_VERSION,
    CONTEXT_IR_VERSION, CONTEXT_ROUTE_POLICY_VERSION, QUERY_OPERATION_VERSION,
    SOURCE_REFERENCE_OPERATION_VERSION, TASK_CLASSIFIER_VERSION,
};
use workspace_atlas::context_yield::{
    ExperimentArmSamples, ExperimentCondition, ExperimentDimension, ExperimentLimitation,
    ExperimentRawSample, ExperimentRunResult, ExperimentServingState, FrozenExperimentBundle,
    CONTEXT_YIELD_REPORT_SCHEMA_VERSION, H3_A_PROFILE_VERSION,
};
use workspace_atlas::error::{AtlasError, Result};
use workspace_atlas::serving::{self, PROJECTION_POLICY_VERSION};
use workspace_atlas::task_compiler::{compile_context_ir, CompileRequest, PLANNER_POLICY_VERSION};
use workspace_atlas::task_session::create_task_session;
use workspace_atlas::{catalogue, cli, discovery, workspace};

const SCENARIO_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkScenario {
    schema_version: String,
    dataset: DatasetScenario,
    procedure: ProcedureScenario,
    profiles: Vec<ProfileScenario>,
    acceptance: AcceptanceScenario,
    estimator: EstimatorScenario,
    experiment_bundle: FrozenExperimentBundle,
    capacity: CapacityScenario,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetScenario {
    generator: String,
    fixture_manifest: String,
    generation: String,
    provider_state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcedureScenario {
    warmups: u64,
    warm_samples: u64,
    cold_samples: u64,
    repetitions: u64,
    percentile_method: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileScenario {
    name: String,
    records: u64,
    source_bytes: u64,
    estimated_tokens: u64,
    relationship_depth: u64,
    work_units: u64,
    uncertainty_reserve_percent: u64,
    candidate_warm_p95_ms: u64,
    wall_cancellation_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceScenario {
    correctness_required: bool,
    deterministic_identity_required: bool,
    bounds_required: bool,
    prohibited_hot_path_work_required: bool,
    latency_breach_repetitions: u64,
    baseline_repetitions: u64,
    relative_regression_fraction: f64,
    relative_regression_mad_multiplier: f64,
    candidate_budgets_are_public_slo: bool,
    cold_latency_advisory: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EstimatorScenario {
    name: String,
    version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityScenario {
    schema_version: String,
    manifest_directory: String,
    accepted_outcome_contract: CapacityAcceptedOutcomeReference,
    raw_evidence_relative_root: String,
    maximum_raw_records: u64,
    strata: Vec<CapacityStratum>,
    phases: Vec<String>,
    provider_conditions: Vec<String>,
    outcome_states: Vec<String>,
    canonical_identity_fields: Vec<String>,
    transient_evidence_fields: Vec<String>,
    setup_fields: Vec<String>,
    atlas_work_fields: Vec<String>,
    limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityStratum {
    name: String,
    manifest: String,
    file_sha256: String,
    manifest_sha256: String,
    eligible_files: u64,
    indexed_files: u64,
    changed_files: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityAcceptedOutcomeReference {
    path: String,
    file_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityAcceptedOutcomeContract {
    schema_version: String,
    kind: String,
    supersedes: CapacityAcceptedOutcomeSupersession,
    truth_projection: CapacityTruthProjection,
    preflight_profile: String,
    outcomes: Vec<CapacityAcceptedOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityAcceptedOutcomeSupersession {
    contract: String,
    reason: String,
    manifests: Vec<CapacityHistoricalAcceptedOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityHistoricalAcceptedOutcome {
    dataset_name: String,
    manifest_sha256: String,
    evidence_counts: Value,
    expected_accepted_outcome_hash: String,
    expected_ready_versus_truth_result_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityTruthProjection {
    generation: String,
    target_identity: String,
    rows: BTreeMap<String, String>,
    canonical_hash: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityAcceptedOutcome {
    dataset_name: String,
    manifest_sha256: String,
    provider_condition: String,
    provider_members: Vec<String>,
    configuration_hash: String,
    truth_state: String,
    phases: Vec<String>,
    preflight_phase: String,
    expected_observation_state: String,
    expected_eligible_files: u64,
    expected_indexed_files: u64,
    expected_source_bytes: u64,
    evidence_counts: Value,
    target_identity: Value,
    expected_accepted_outcome_hash: String,
    expected_ready_versus_truth_result_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CapacityPhase {
    ColdInitializationReconcile,
    NoChangeReconcile,
    FixedIncrementalReconcile,
    ReadyServingQueryCompiler,

    TruthFallbackQueryCompiler,
    CapabilityDiscovery,
    DirectRouteCompiler,
    LightRouteCompiler,
    ProgressiveDeepRouteCompiler,
    CatalogueGrowth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapacityReconcileMeasurements {
    NotApplicable,
    Observed,
    NoChange,
    FixedIncremental,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapacityPreparationPolicy {
    Stateless,
    PerAttemptCatalogue,
    PreparedCatalogue,
    PreparedBaseline,
    PreparedBaselineWithServing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapacityLifecycleIdentityPolicy {
    SharedWarm,
    PerAttempt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapacityResetPolicy {
    None,
    ReleaseCatalogue,
    RestoreFixedFixture,
    RemoveServing,
    EnsureServing,
}

#[derive(Clone, Copy)]
struct CapacityPhaseDescriptor {
    dispatch: CapacityPhase,
    name: &'static str,
    fixture_lifecycle: &'static str,
    catalogue_lifecycle: &'static str,
    serving_state: &'static str,
    warm_cache_state: &'static str,
    cold_cache_state: &'static str,
    action: &'static str,
    preparation: CapacityPreparationPolicy,
    lifecycle_identity: CapacityLifecycleIdentityPolicy,
    reset: CapacityResetPolicy,
    route_attempts: u64,
    catalogue_measurements: bool,
    provider_measurements: bool,
    reconcile_measurements: CapacityReconcileMeasurements,
    ready_truth_measurements: bool,
    compiler_fallback: Option<bool>,
}

impl CapacityPhaseDescriptor {
    fn cache_state(self, sample_class: &str) -> Result<&'static str> {
        match sample_class {
            "warmup" | "warm" => Ok(self.warm_cache_state),
            "cold" => Ok(self.cold_cache_state),
            _ => Err(AtlasError::InvalidConfig(format!(
                "unknown capacity sample class {sample_class}"
            ))),
        }
    }

    fn lifecycle_id(
        self,
        dataset_name: &str,
        profile: &str,
        provider_condition: &str,
        repetition: u64,
        sample_class: &str,
        sample_index: u64,
    ) -> Result<String> {
        let (identity_class, identity_index) = match sample_class {
            "cold" => ("cold", sample_index),
            "warmup" | "warm"
                if self.lifecycle_identity == CapacityLifecycleIdentityPolicy::PerAttempt =>
            {
                (sample_class, sample_index)
            }
            "warmup" | "warm" => ("warm", 0),
            _ => {
                return Err(AtlasError::InvalidConfig(format!(
                    "unknown capacity sample class {sample_class}"
                )))
            }
        };
        Ok(capacity_lifecycle_id(
            dataset_name,
            profile,
            provider_condition,
            self.name,
            repetition,
            identity_class,
            identity_index,
        ))
    }
}

const CAPACITY_PHASES: [CapacityPhaseDescriptor; 10] = [
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::ColdInitializationReconcile,
        name: "cold-initialization-reconcile",
        fixture_lifecycle: "a unique prepared fixture/configuration lifecycle for every warmup, warm, and cold sample",
        catalogue_lifecycle: "a fresh catalogue is initialized, registered, reconciled, and released inside every observation",
        serving_state: "absent",
        warm_cache_state: "fresh-fixture-cold",
        cold_cache_state: "fresh-fixture-cold",
        action: "catalogue::init_catalogue + workspace::register_workspace + discovery::reconcile",
        preparation: CapacityPreparationPolicy::PerAttemptCatalogue,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::PerAttempt,
        reset: CapacityResetPolicy::ReleaseCatalogue,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::Observed,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::NoChangeReconcile,
        name: "no-change-reconcile",
        fixture_lifecycle: "one prepared fixture/catalogue per row/repetition for warmup and warm; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "baseline reconcile before repeated measured unchanged reconciles",
        serving_state: "absent",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "discovery::reconcile with byte-identical fixture",
        preparation: CapacityPreparationPolicy::PreparedBaseline,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::None,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::NoChange,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::FixedIncrementalReconcile,
        name: "fixed-incremental-reconcile",
        fixture_lifecycle: "restore baseline bytes, reconcile, reapply every immutable change, and verify after_sha256 for each sample",
        catalogue_lifecycle: "one baseline catalogue per warm repetition; each sample restores and reapplies the exact mutation",
        serving_state: "absent",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "discovery::reconcile after deterministic semantic edits, rename, and delete",
        preparation: CapacityPreparationPolicy::PreparedBaseline,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::RestoreFixedFixture,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::FixedIncremental,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::ReadyServingQueryCompiler,
        name: "ready-serving-query-compiler",
        fixture_lifecycle: "one prepared fixture/catalogue per warm repetition; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "baseline reconcile then one ready serving generation retained across warm samples",
        serving_state: "ready",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "cli::build_find_output then compile_context_ir against ready Serving",
        preparation: CapacityPreparationPolicy::PreparedBaselineWithServing,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::EnsureServing,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: true,
        compiler_fallback: Some(false),
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::TruthFallbackQueryCompiler,
        name: "truth-fallback-query-compiler",
        fixture_lifecycle: "one prepared fixture/catalogue per warm repetition; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "baseline reconcile; each sample removes disposable Serving after equivalence validation",
        serving_state: "forced-absent",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "cli::build_find_output then compile_context_ir with Truth fallback",
        preparation: CapacityPreparationPolicy::PreparedBaseline,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::RemoveServing,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: true,
        compiler_fallback: Some(true),
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::CapabilityDiscovery,
        name: "capability-discovery",
        fixture_lifecycle: "validated immutable manifest; one stateless preparation identity per warm repetition and unique cold identity",
        catalogue_lifecycle: "none",
        serving_state: "not-applicable",
        warm_cache_state: "not-applicable",
        cold_cache_state: "not-applicable",
        action: "context_route::discover_context_capabilities + closed-contract validation",
        preparation: CapacityPreparationPolicy::Stateless,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::None,
        route_attempts: 0,
        catalogue_measurements: false,
        provider_measurements: false,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::DirectRouteCompiler,
        name: "direct-route-compiler",
        fixture_lifecycle: "validated immutable manifest; one stateless preparation identity per warm repetition and unique cold identity",
        catalogue_lifecycle: "none",
        serving_state: "not-applicable",
        warm_cache_state: "not-applicable",
        cold_cache_state: "not-applicable",
        action: "context_route::decide_context_route with DIRECT floor/ceiling and zero Atlas compiler calls",
        preparation: CapacityPreparationPolicy::Stateless,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::None,
        route_attempts: 1,
        catalogue_measurements: false,
        provider_measurements: false,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::LightRouteCompiler,
        name: "light-route-compiler",
        fixture_lifecycle: "one prepared fixture/catalogue per warm repetition; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "baseline reconcile before target-bounded LIGHT governor execution",
        serving_state: "forced-absent",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "context_application::run_catalogue_governor with ATLAS_LIGHT floor/ceiling over explicit bounded targets",
        preparation: CapacityPreparationPolicy::PreparedBaseline,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::None,
        route_attempts: 1,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::ProgressiveDeepRouteCompiler,
        name: "progressive-deep-route-compiler",
        fixture_lifecycle: "one prepared fixture/catalogue per warm repetition; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "baseline reconcile before response-lifetime progressive, direct-DEEP, and ready-Serving equivalence executions",
        serving_state: "forced-absent then ready-equivalence",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "context_application::run_catalogue_governor through DIRECT -> ATLAS_LIGHT -> ATLAS_DEEP Context IR 2.0.0",
        preparation: CapacityPreparationPolicy::PreparedBaseline,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::RemoveServing,
        route_attempts: 3,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::NotApplicable,
        ready_truth_measurements: true,
        compiler_fallback: Some(true),
    },
    CapacityPhaseDescriptor {
        dispatch: CapacityPhase::CatalogueGrowth,
        name: "catalogue-growth",
        fixture_lifecycle: "one prepared fixture/catalogue per row/repetition for warmup and warm; a unique fresh preparation per cold sample",
        catalogue_lifecycle: "measure durable and disposable rows/bytes before and after reconcile plus Serving build",
        serving_state: "built-disposable",
        warm_cache_state: "atlas-warm",
        cold_cache_state: "fresh-fixture-cold",
        action: "discovery::reconcile + serving::build_serving_generation with before/after measurements",
        preparation: CapacityPreparationPolicy::PreparedCatalogue,
        lifecycle_identity: CapacityLifecycleIdentityPolicy::SharedWarm,
        reset: CapacityResetPolicy::RemoveServing,
        route_attempts: 0,
        catalogue_measurements: true,
        provider_measurements: true,
        reconcile_measurements: CapacityReconcileMeasurements::Observed,
        ready_truth_measurements: false,
        compiler_fallback: None,
    },
];

impl CapacityPhase {
    fn parse(name: &str) -> Result<Self> {
        CAPACITY_PHASES
            .iter()
            .find(|descriptor| descriptor.name == name)
            .map(|descriptor| descriptor.dispatch)
            .ok_or_else(|| AtlasError::InvalidConfig(format!("unknown capacity phase {name}")))
    }

    fn descriptor(self) -> &'static CapacityPhaseDescriptor {
        CAPACITY_PHASES
            .iter()
            .find(|descriptor| descriptor.dispatch == self)
            .expect("closed capacity phase descriptor")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapacityDataset {
    Small,
    Medium,
    Large,
}

impl CapacityDataset {
    const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    fn name(self) -> &'static str {
        match self {
            Self::Small => "repo-small-v1",
            Self::Medium => "repo-medium-v1",
            Self::Large => "repo-large-v1",
        }
    }

    fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|dataset| dataset.name() == name)
            .ok_or_else(|| AtlasError::InvalidConfig(format!("unknown capacity dataset {name}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapacityProviderCondition {
    BuiltinOnly,
    TypeScriptOnly,
    RustOnly,
    Combined,
}

impl CapacityProviderCondition {
    const ALL: [Self; 4] = [
        Self::BuiltinOnly,
        Self::TypeScriptOnly,
        Self::RustOnly,
        Self::Combined,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::BuiltinOnly => "builtin-only",
            Self::TypeScriptOnly => "typescript-only",
            Self::RustOnly => "rust-only",
            Self::Combined => "combined",
        }
    }

    fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|condition| condition.name() == name)
            .ok_or_else(|| {
                AtlasError::InvalidConfig(format!("unknown capacity provider condition {name}"))
            })
    }

    fn resolve(self, dataset: CapacityDataset) -> ResolvedCapacityProviderCondition {
        let outcome_policy = match (self, dataset) {
            (Self::RustOnly, CapacityDataset::Small) => {
                CapacityProviderOutcomePolicy::NotApplicable
            }
            (Self::RustOnly, _) => CapacityProviderOutcomePolicy::Optional,
            (Self::Combined, _) => CapacityProviderOutcomePolicy::RequiredWithOptional,
            (Self::BuiltinOnly | Self::TypeScriptOnly, _) => {
                CapacityProviderOutcomePolicy::Required
            }
        };
        ResolvedCapacityProviderCondition {
            dataset,
            condition: self,
            outcome_policy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CapacityProviderApplicability {
    All,
    NonSmall,
}

impl CapacityProviderApplicability {
    fn applies_to(self, dataset: CapacityDataset) -> bool {
        self == Self::All || dataset != CapacityDataset::Small
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapacityProviderOutcomePolicy {
    Required,
    Optional,
    NotApplicable,
    RequiredWithOptional,
}

#[derive(Clone, Copy)]
struct ResolvedCapacityProviderCondition {
    dataset: CapacityDataset,
    condition: CapacityProviderCondition,
    outcome_policy: CapacityProviderOutcomePolicy,
}

impl ResolvedCapacityProviderCondition {
    fn parse(dataset_name: &str, condition_name: &str) -> Result<Self> {
        Ok(CapacityProviderCondition::parse(condition_name)?
            .resolve(CapacityDataset::parse(dataset_name)?))
    }

    fn name(&self) -> &'static str {
        self.condition.name()
    }

    fn providers(&self) -> impl Iterator<Item = &'static CapacityProviderDescriptor> {
        let dataset = self.dataset;
        let condition = self.condition;
        CAPACITY_PROVIDERS.iter().filter(move |provider| {
            let selected = match condition {
                CapacityProviderCondition::BuiltinOnly => provider.kind == "builtin",
                CapacityProviderCondition::TypeScriptOnly => provider.name == "scip-typescript",
                CapacityProviderCondition::RustOnly => provider.name == "rust-analyzer",
                CapacityProviderCondition::Combined => true,
            };
            selected
                && (condition == CapacityProviderCondition::RustOnly
                    || provider.applicability.applies_to(dataset))
        })
    }

    fn matches_members(&self, members: &[String]) -> bool {
        members.len() == self.providers().count()
            && members
                .iter()
                .zip(self.providers())
                .all(|(member, provider)| {
                    member.split_once('@').is_some_and(|(name, version)| {
                        name == provider.name && version == provider.version
                    })
                })
    }
    fn is_applicable(&self) -> bool {
        self.outcome_policy != CapacityProviderOutcomePolicy::NotApplicable
    }

    fn configuration_hash(&self) -> Result<Option<String>> {
        if self.is_applicable() {
            Ok(Some(capacity_config(self)?.configuration_hash()))
        } else {
            Ok(None)
        }
    }

    fn allows_non_success(&self, outcome: &str, required_provider_failure: Option<bool>) -> bool {
        match self.outcome_policy {
            CapacityProviderOutcomePolicy::Required => false,
            CapacityProviderOutcomePolicy::Optional => true,
            CapacityProviderOutcomePolicy::NotApplicable => outcome == "unsupported",
            CapacityProviderOutcomePolicy::RequiredWithOptional => {
                outcome == "degraded" && required_provider_failure == Some(false)
            }
        }
    }

    fn members(&self) -> Vec<String> {
        self.providers()
            .map(|provider| format!("{}@{}", provider.name, provider.version))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct CapacityProviderDescriptor {
    name: &'static str,
    version: &'static str,
    kind: &'static str,
    tier: &'static str,
    scope: &'static str,
    required: bool,
    priority: i64,
    languages: &'static [&'static str],
    project_markers: &'static [&'static str],
    command: Option<&'static str>,
    arguments: &'static [&'static str],
    probe_arguments: &'static [&'static str],
    version_arguments: &'static [&'static str],
    output_format: Option<&'static str>,
    applicability: CapacityProviderApplicability,
}

const CAPACITY_PROVIDERS: [CapacityProviderDescriptor; 6] = [
    CapacityProviderDescriptor {
        name: "file_metadata",
        version: "1.0.0",
        kind: "builtin",
        tier: "document_config",
        scope: "file",
        required: true,
        priority: 950,
        languages: &[],
        project_markers: &[],
        command: None,
        arguments: &[],
        probe_arguments: &[],
        version_arguments: &[],
        output_format: None,
        applicability: CapacityProviderApplicability::All,
    },
    CapacityProviderDescriptor {
        name: "document_config",
        version: "1.0.0",
        kind: "builtin",
        tier: "document_config",
        scope: "file",
        required: false,
        priority: 700,
        languages: &[],
        project_markers: &[],
        command: None,
        arguments: &[],
        probe_arguments: &[],
        version_arguments: &[],
        output_format: None,
        applicability: CapacityProviderApplicability::All,
    },
    CapacityProviderDescriptor {
        name: "structural_typescript",
        version: "1.0.0",
        kind: "builtin",
        tier: "structural",
        scope: "file",
        required: false,
        priority: 500,
        languages: &["typescript", "javascript"],
        project_markers: &[],
        command: None,
        arguments: &[],
        probe_arguments: &[],
        version_arguments: &[],
        output_format: None,
        applicability: CapacityProviderApplicability::All,
    },
    CapacityProviderDescriptor {
        name: "safe_text_fallback",
        version: "1.0.0",
        kind: "builtin",
        tier: "textual",
        scope: "file",
        required: false,
        priority: 100,
        languages: &[],
        project_markers: &[],
        command: None,
        arguments: &[],
        probe_arguments: &[],
        version_arguments: &[],
        output_format: None,
        applicability: CapacityProviderApplicability::All,
    },
    CapacityProviderDescriptor {
        name: "scip-typescript",
        version: "0.4.0",
        kind: "external_scip",
        tier: "semantic_index",
        scope: "project",
        required: true,
        priority: 800,
        languages: &["typescript", "javascript"],
        project_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
        command: Some("scip-typescript"),
        arguments: &["index", "--output", "{output_file}"],
        probe_arguments: &["index", "--help"],
        version_arguments: &["--version"],
        output_format: Some("scip"),
        applicability: CapacityProviderApplicability::All,
    },
    CapacityProviderDescriptor {
        name: "rust-analyzer",
        version: "1.94.1",
        kind: "external_scip",
        tier: "semantic_index",
        scope: "project",
        required: false,
        priority: 800,
        languages: &["rust"],
        project_markers: &["Cargo.toml"],
        command: Some("rust-analyzer"),
        arguments: &["scip", ".", "--output", "{output_file}"],
        probe_arguments: &["--version"],
        version_arguments: &["--version"],
        output_format: Some("scip"),
        applicability: CapacityProviderApplicability::NonSmall,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CapacityShard {
    index: u64,
}

impl CapacityShard {
    fn parse(index: &str, count: &str) -> Result<Self> {
        let index = index.parse::<u64>().map_err(|_| {
            AtlasError::InvalidConfig("capacity shard index must be an integer from 0 to 3".into())
        })?;
        let count = count.parse::<u64>().map_err(|_| {
            AtlasError::InvalidConfig("capacity shard count must be exactly 4".into())
        })?;
        if count != CAPACITY_SHARD_COUNT {
            return Err(AtlasError::InvalidConfig(
                "capacity shard count must be exactly 4".into(),
            ));
        }
        if index >= CAPACITY_SHARD_COUNT {
            return Err(AtlasError::InvalidConfig(
                "capacity shard index must be an integer from 0 to 3".into(),
            ));
        }
        Ok(Self { index })
    }

    fn selects(self, global_matrix_row_index: u64) -> bool {
        global_matrix_row_index % CAPACITY_SHARD_COUNT == self.index
    }
}

enum Invocation {
    Legacy {
        scenario_path: PathBuf,
    },
    CapacityPlan {
        scenario_path: PathBuf,
        capacity_manifest_dir: PathBuf,
        evidence_dir: PathBuf,
        shard: Option<CapacityShard>,
    },
    CapacityPreflight {
        scenario_path: PathBuf,
        capacity_manifest_dir: PathBuf,
        evidence_dir: PathBuf,
    },
    CapacityRun {
        scenario_path: PathBuf,
        capacity_manifest_dir: PathBuf,
        evidence_dir: PathBuf,
        memory_label_state: Option<PathBuf>,
        shard: Option<CapacityShard>,
    },
    CapacityMemory {
        scenario_path: PathBuf,
        capacity_manifest_dir: PathBuf,
        evidence_dir: PathBuf,
        shard: Option<CapacityShard>,
    },
    CapacityLog {
        scenario_path: PathBuf,
        capacity_manifest_dir: PathBuf,
        evidence_dir: PathBuf,
        shard: Option<CapacityShard>,
    },
}

#[derive(Default)]
struct RunEvidence {
    cold_bootstrap_ms: Vec<f64>,
    source_tree_hash: String,
    generation_id: String,
    experiment_results: Vec<ExperimentRunResult>,
}

#[derive(Serialize)]
struct RunAttestation<'a> {
    attestation_schema_version: &'static str,
    scenario_schema_version: &'a str,
    scenario_path: String,
    scenario_sha256: String,
    revision: String,
    working_tree_clean: bool,
    toolchain: String,
    platform: PlatformAttestation,
    policies: PolicyAttestation,
    fixture: FixtureAttestation<'a>,
    cache_state: &'static str,
    procedure: &'a ProcedureScenario,
    profiles: &'a [ProfileScenario],
    samples: SampleAttestation,
    experiment_results: &'a [ExperimentRunResult],
    generated_at_unix_seconds: u64,
}

#[derive(Serialize)]
struct PlatformAttestation {
    os: &'static str,
    architecture: &'static str,
    cpu: String,
}

#[derive(Serialize)]
struct PolicyAttestation {
    planner: &'static str,
    projection: &'static str,
    context_ir: &'static str,
    context_yield: &'static str,
    profile: &'static str,
}

#[derive(Serialize)]
struct FixtureAttestation<'a> {
    generator: &'a str,
    generation: &'a str,
    provider_state: &'a str,
    source_tree_hash: &'a str,
    generation_id: &'a str,
}

#[derive(Serialize)]
struct SampleAttestation {
    baseline_reconcile_ms: Vec<f64>,
    total_ms: Vec<f64>,
    correctness_checks_passed: usize,
    correctness_checks_total: usize,
}

struct Check {
    name: String,
    ok: bool,
    detail: String,
}

fn check(checks: &mut Vec<Check>, name: &str, ok: bool, detail: impl Into<String>) {
    checks.push(Check {
        name: name.to_string(),
        ok,
        detail: detail.into(),
    });
}

fn main() {
    if std::env::var_os("ATLAS_BENCH_TEST_EXECUTOR").is_some() {
        println!("[FAIL] manifest - ATLAS_BENCH_TEST_EXECUTOR is forbidden in shipped binaries");
        std::process::exit(1);
    }
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let invocation = match parse_invocation(&repository_root) {
        Ok(invocation) => invocation,
        Err(error) => {
            println!("[FAIL] manifest - {error}");
            std::process::exit(1);
        }
    };
    let scenario_path = match &invocation {
        Invocation::Legacy { scenario_path }
        | Invocation::CapacityPlan { scenario_path, .. }
        | Invocation::CapacityPreflight { scenario_path, .. }
        | Invocation::CapacityRun { scenario_path, .. }
        | Invocation::CapacityMemory { scenario_path, .. }
        | Invocation::CapacityLog { scenario_path, .. } => scenario_path,
    };
    let scenario = match load_scenario(scenario_path) {
        Ok(scenario) => scenario,
        Err(error) => {
            println!("[FAIL] manifest - {error}");
            std::process::exit(1);
        }
    };
    match invocation {
        Invocation::Legacy { scenario_path } => {
            run_legacy_main(&repository_root, &scenario_path, &scenario)
        }
        Invocation::CapacityPlan {
            scenario_path,
            capacity_manifest_dir,
            evidence_dir,
            shard,
        } => match write_capacity_plan(
            &repository_root,
            &scenario_path,
            &scenario,
            &capacity_manifest_dir,
            &evidence_dir,
            shard,
        ) {
            Ok(path) => {
                println!("capacity execution plan: {}", path.display());
                std::process::exit(0);
            }
            Err(error) => {
                println!("[FAIL] capacity plan - {error}");
                std::process::exit(1);
            }
        },
        Invocation::CapacityPreflight {
            scenario_path,
            capacity_manifest_dir,
            evidence_dir,
        } => match write_capacity_accepted_outcome_preflight(
            &repository_root,
            &scenario_path,
            &scenario,
            &capacity_manifest_dir,
            &evidence_dir,
        ) {
            Ok(path) => {
                println!("capacity accepted-outcome preflight: {}", path.display());
                std::process::exit(0);
            }
            Err(error) => {
                let detail =
                    redact_capacity_log_text(&error.to_string(), &repository_root, &evidence_dir);
                println!("[FAIL] capacity preflight - {detail}");
                std::process::exit(1);
            }
        },
        Invocation::CapacityRun {
            scenario_path,
            capacity_manifest_dir,
            evidence_dir,
            memory_label_state,
            shard,
        } => match run_capacity_campaign(
            &repository_root,
            &scenario_path,
            &scenario,
            &capacity_manifest_dir,
            &evidence_dir,
            memory_label_state.as_deref(),
            shard,
        ) {
            Ok((_raw_path, _summary_path)) => {
                println!("capacity raw evidence retained: capacity-raw-observations-v1.ndjson");
                println!("capacity evidence summary retained: capacity-summary-v1.json");
                std::process::exit(0);
            }
            Err(error) => {
                let detail =
                    redact_capacity_log_text(&error.to_string(), &repository_root, &evidence_dir);
                println!("[FAIL] capacity run - {detail}");
                std::process::exit(1);
            }
        },
        Invocation::CapacityMemory {
            scenario_path,
            capacity_manifest_dir,
            evidence_dir,
            shard,
        } => match run_capacity_memory_sampler(
            &repository_root,
            &scenario_path,
            &scenario,
            &capacity_manifest_dir,
            &evidence_dir,
            shard,
        ) {
            Ok((segments, _summary_path)) => {
                println!(
                    "process-tree memory raw evidence retained: {} segments",
                    segments.len()
                );
                println!("process-tree memory summary retained: {MEMORY_SUMMARY_FILE}");
                std::process::exit(0);
            }
            Err(error) => {
                let detail =
                    redact_capacity_log_text(&error.to_string(), &repository_root, &evidence_dir);
                println!("[FAIL] capacity memory - {detail}");
                std::process::exit(1);
            }
        },
        Invocation::CapacityLog {
            scenario_path,
            capacity_manifest_dir,
            evidence_dir,
            shard,
        } => match emit_capacity_log(
            &repository_root,
            &scenario_path,
            &scenario,
            &capacity_manifest_dir,
            &evidence_dir,
            shard,
        ) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                let detail =
                    redact_capacity_log_text(&error.to_string(), &repository_root, &evidence_dir);
                println!("[FAIL] capacity log - {detail}");
                std::process::exit(1);
            }
        },
    }
}

fn run_legacy_main(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
) -> ! {
    let mut checks: Vec<Check> = Vec::new();
    let mut evidence = RunEvidence::default();
    let start = Instant::now();
    let outcome = run(scenario, &mut checks, &mut evidence);
    let elapsed = start.elapsed();

    println!("=== atlas-bench acceptance matrix ===");
    for check in &checks {
        println!(
            "[{}] {} - {}",
            if check.ok { "PASS" } else { "FAIL" },
            check.name,
            check.detail
        );
    }
    if let Err(error) = &outcome {
        println!("[FAIL] harness - {error}");
    }
    let passed = checks.iter().filter(|check| check.ok).count();
    let total = checks.len();
    let attestation = if outcome.is_ok() {
        match write_attestation(
            repository_root,
            scenario_path,
            scenario,
            &evidence,
            passed,
            total,
            elapsed,
        ) {
            Ok(path) => {
                println!("run attestation: {}", path.display());
                Some(path)
            }
            Err(error) => {
                println!("[FAIL] attestation - {error}");
                None
            }
        }
    } else {
        None
    };
    println!(
        "--- {passed}/{total} checks passed in {:.2}s ---",
        elapsed.as_secs_f64()
    );

    let all_ok = outcome.is_ok() && attestation.is_some() && passed == total && total > 0;
    std::process::exit(if all_ok { 0 } else { 1 });
}

fn parse_invocation(repository_root: &Path) -> Result<Invocation> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let default_scenario =
        repository_root.join("tests/fixtures/context_yield/benchmark-scenario.json");
    match arguments.as_slice() {
        [] => Ok(Invocation::Legacy {
            scenario_path: default_scenario,
        }),
        [flag, path] if flag == "--manifest" => Ok(Invocation::Legacy {
            scenario_path: PathBuf::from(path),
        }),
        [flag] if flag == "--capacity-plan" => Ok(Invocation::CapacityPlan {
            scenario_path: default_scenario,
            capacity_manifest_dir: repository_root.join("tests/fixtures/capacity"),
            evidence_dir: repository_root.join("target/atlas-bench/capacity"),
            shard: None,
        }),
        [flag] if flag == "--capacity-preflight" => Ok(Invocation::CapacityPreflight {
            scenario_path: default_scenario,
            capacity_manifest_dir: repository_root.join("tests/fixtures/capacity"),
            evidence_dir: repository_root.join("target/atlas-bench/capacity-preflight"),
        }),
        [flag, rest @ ..]
            if flag == "--capacity-plan"
                || flag == "--capacity-preflight"
                || flag == "--capacity-run"
                || flag == "--capacity-memory"
                || flag == "--capacity-log" =>
        {
            let CapacityOptions {
                scenario_path,
                capacity_manifest_dir,
                evidence_dir,
                memory_label_state,
                shard,
            } = parse_capacity_options(repository_root, rest, flag == "--capacity-run")?;
            if flag == "--capacity-preflight" && shard.is_some() {
                return Err(AtlasError::InvalidConfig(
                    "capacity accepted-outcome preflight must cover the complete unsharded contract"
                        .into(),
                ));
            }
            match flag.as_str() {
                "--capacity-plan" => Ok(Invocation::CapacityPlan {
                    scenario_path,
                    capacity_manifest_dir,
                    evidence_dir,
                    shard,
                }),
                "--capacity-preflight" => Ok(Invocation::CapacityPreflight {
                    scenario_path,
                    capacity_manifest_dir,
                    evidence_dir,
                }),
                "--capacity-run" => Ok(Invocation::CapacityRun {
                    scenario_path,
                    capacity_manifest_dir,
                    evidence_dir,
                    memory_label_state,
                    shard,
                }),
                "--capacity-memory" => Ok(Invocation::CapacityMemory {
                    scenario_path,
                    capacity_manifest_dir,
                    evidence_dir,
                    shard,
                }),
                "--capacity-log" => Ok(Invocation::CapacityLog {
                    scenario_path,
                    capacity_manifest_dir,
                    evidence_dir,
                    shard,
                }),
                _ => unreachable!("closed capacity invocation"),
            }
        }
        _ => Err(AtlasError::InvalidConfig(
            "usage: atlas-bench [--manifest <scenario>] | --capacity-plan|--capacity-preflight|\
             --capacity-run|--capacity-memory|--capacity-log [--manifest <scenario>] \
             [--capacity-manifest-dir <directory>] [--evidence-dir <target-directory>] \
             [--capacity-shard-index <0..3> --capacity-shard-count 4]"
                .into(),
        )),
    }
}

struct CapacityOptions {
    scenario_path: PathBuf,
    capacity_manifest_dir: PathBuf,
    evidence_dir: PathBuf,
    memory_label_state: Option<PathBuf>,
    shard: Option<CapacityShard>,
}

fn parse_capacity_options(
    repository_root: &Path,
    arguments: &[String],
    allow_memory_label_state: bool,
) -> Result<CapacityOptions> {
    let mut scenario_path =
        repository_root.join("tests/fixtures/context_yield/benchmark-scenario.json");
    let mut capacity_manifest_dir = repository_root.join("tests/fixtures/capacity");
    let mut evidence_dir = repository_root.join("target/atlas-bench/capacity");
    let mut memory_label_state = None;
    let mut shard_index = None;
    let mut shard_count = None;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        if !seen.insert(option) {
            return Err(AtlasError::InvalidConfig(format!(
                "duplicate capacity option {option}"
            )));
        }
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| AtlasError::InvalidConfig(format!("{option} requires a value")))?;
        if value.is_empty() {
            return Err(AtlasError::InvalidConfig(format!(
                "{option} requires a non-empty value"
            )));
        }
        match option {
            "--manifest" => scenario_path = PathBuf::from(value),
            "--capacity-manifest-dir" => capacity_manifest_dir = PathBuf::from(value),
            "--evidence-dir" => evidence_dir = PathBuf::from(value),
            "--memory-label-state" if allow_memory_label_state => {
                memory_label_state = Some(PathBuf::from(value));
            }
            "--capacity-shard-index" => shard_index = Some(value.as_str()),
            "--capacity-shard-count" => shard_count = Some(value.as_str()),
            option => {
                return Err(AtlasError::InvalidConfig(format!(
                    "unsupported capacity option {option}"
                )))
            }
        }
        index += 2;
    }
    let shard = match (shard_index, shard_count) {
        (None, None) => None,
        (Some(index), Some(count)) => Some(CapacityShard::parse(index, count)?),
        _ => {
            return Err(AtlasError::InvalidConfig(
                "capacity shard index and count must be supplied together".into(),
            ))
        }
    };
    Ok(CapacityOptions {
        scenario_path,
        capacity_manifest_dir,
        evidence_dir,
        memory_label_state,
        shard,
    })
}

fn load_scenario(path: &Path) -> Result<BenchmarkScenario> {
    let text = std::fs::read_to_string(path)?;
    let scenario: BenchmarkScenario = serde_json::from_str(&text)?;
    validate_scenario(&scenario)?;
    Ok(scenario)
}

fn validate_scenario(scenario: &BenchmarkScenario) -> Result<()> {
    let invalid = |message: &str| AtlasError::InvalidConfig(message.to_string());
    if scenario.schema_version != SCENARIO_SCHEMA_VERSION {
        return Err(invalid("schema_version must equal 1.0.0"));
    }
    if scenario.dataset.generator != "tests/fixtures/pilot/generate_fixture.py"
        || scenario.dataset.fixture_manifest != "fixture_manifest.json"
        || scenario.dataset.generation != "fresh_per_run"
        || scenario.dataset.provider_state != "builtin_deterministic"
    {
        return Err(invalid(
            "dataset must equal the authoritative pilot fixture contract",
        ));
    }
    if scenario.procedure.warmups != 5 {
        return Err(invalid("warmups must equal 5"));
    }
    if scenario.procedure.warm_samples != 100 {
        return Err(invalid("warm_samples must equal 100"));
    }
    if scenario.procedure.cold_samples != 20 {
        return Err(invalid("cold_samples must equal 20"));
    }
    if scenario.procedure.repetitions != 3 {
        return Err(invalid("repetitions must equal 3"));
    }
    if scenario.procedure.percentile_method != "nearest_rank" {
        return Err(invalid("percentile_method must equal nearest_rank"));
    }
    let expected_profiles = [
        ("small", 30, 12_000, 4_000, 1, 5_000, 15, 150, 1_000),
        ("standard", 60, 40_000, 8_000, 2, 20_000, 12, 250, 5_000),
        ("audit", 250, 120_000, 24_000, 4, 100_000, 15, 2_000, 10_000),
    ];
    if scenario.profiles.len() != expected_profiles.len() {
        return Err(invalid(
            "profiles must contain exactly small, standard, and audit",
        ));
    }
    for (profile, expected) in scenario.profiles.iter().zip(expected_profiles) {
        let actual = (
            profile.name.as_str(),
            profile.records,
            profile.source_bytes,
            profile.estimated_tokens,
            profile.relationship_depth,
            profile.work_units,
            profile.uncertainty_reserve_percent,
            profile.candidate_warm_p95_ms,
            profile.wall_cancellation_ms,
        );
        if actual != expected {
            return Err(invalid(&format!(
                "profile {} differs from the accepted H3-A contract",
                profile.name
            )));
        }
    }
    let acceptance = &scenario.acceptance;
    if !acceptance.correctness_required
        || !acceptance.deterministic_identity_required
        || !acceptance.bounds_required
        || !acceptance.prohibited_hot_path_work_required
        || acceptance.latency_breach_repetitions != 2
        || acceptance.baseline_repetitions != 3
        || acceptance.relative_regression_fraction != 0.15
        || acceptance.relative_regression_mad_multiplier != 2.0
        || acceptance.candidate_budgets_are_public_slo
        || !acceptance.cold_latency_advisory
    {
        return Err(invalid(
            "acceptance differs from the accepted H3-A contract",
        ));
    }
    scenario
        .experiment_bundle
        .validate()
        .map_err(|error| invalid(&format!("invalid experiment bundle: {error}")))?;
    if scenario.estimator.name != "utf8_bytes_div_4_ceiling"
        || scenario.estimator.version != "1.0.0"
    {
        return Err(invalid(
            "estimator must equal utf8_bytes_div_4_ceiling 1.0.0",
        ));
    }
    validate_capacity_scenario(&scenario.capacity)?;
    Ok(())
}

fn validate_capacity_scenario(capacity: &CapacityScenario) -> Result<()> {
    let invalid = |message: &str| AtlasError::InvalidConfig(message.to_string());
    if capacity.schema_version != "1.0.0"
        || capacity.manifest_directory != "tests/fixtures/capacity"
        || capacity.raw_evidence_relative_root != "target/atlas-bench/capacity"
    {
        return Err(invalid(
            "capacity roots and schema must equal the frozen T30B contract",
        ));
    }
    let expected_strata = [
        (
            "repo-small-v1",
            "repo-small-v1.json",
            "883e84026b956e97ea42550802e86dc3ffd748d45b50ad84fbcff84921578cb6",
            "6209c40d6345987d4373acf29ed6ca34056c718b7a0b87cead74a87952260549",
            128,
            128,
            2,
        ),
        (
            "repo-medium-v1",
            "repo-medium-v1.json",
            "aee43db257ea8b78a1c5aa690f938d5bd17f9bf1c069cf0caf528d442b97d858",
            "c4160323a9930d92190854427d24debfaa2e81c9a8e165b98b9e0fcb5e648a9e",
            1_024,
            1_024,
            11,
        ),
        (
            "repo-large-v1",
            "repo-large-v1.json",
            "0da921308656d30a2183274a75dede42b4703b72923a919a27786a8b6ecf80d1",
            "81f8b8672d6a0e0aa237f1fbe1d73bcb86c89f3b24ed33eb463389c0c2e7d33b",
            8_192,
            8_192,
            82,
        ),
    ];
    if capacity.strata.len() != expected_strata.len() {
        return Err(invalid(
            "capacity strata must contain exactly three entries",
        ));
    }
    for (stratum, expected) in capacity.strata.iter().zip(expected_strata) {
        let actual = (
            stratum.name.as_str(),
            stratum.manifest.as_str(),
            stratum.file_sha256.as_str(),
            stratum.manifest_sha256.as_str(),
            stratum.eligible_files,
            stratum.indexed_files,
            stratum.changed_files,
        );
        if actual != expected {
            return Err(invalid(&format!(
                "capacity stratum {} differs from the immutable T30A identity",
                stratum.name
            )));
        }
    }
    let exact = |actual: &[String], expected: &[&str]| {
        actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    };
    let phase_names = CAPACITY_PHASES
        .iter()
        .map(|descriptor| descriptor.name)
        .collect::<Vec<_>>();
    if !exact(&capacity.phases, &phase_names) {
        return Err(invalid(
            "capacity phases differ from the frozen T30B matrix",
        ));
    }
    let capabilities = discover_context_capabilities();
    capabilities.validate().map_err(|error| {
        invalid(&format!(
            "invalid advertised governor capability set: {error}"
        ))
    })?;
    let deep_advertised = capabilities.routes.contains(&Route::AtlasDeep)
        && capabilities
            .availability
            .get(&CapabilityFeature::ProgressiveExecution)
            == Some(&Availability::Available)
        && capabilities
            .availability
            .get(&CapabilityFeature::DeepContextIr)
            == Some(&Availability::Available);
    let deep_executed = capacity
        .phases
        .iter()
        .any(|phase| phase == "progressive-deep-route-compiler");
    if deep_advertised != deep_executed {
        return Err(invalid(
            "advertised ProgressiveExecution/DeepContextIr must equal executed progressive DEEP capacity coverage",
        ));
    }
    let provider_condition_names = CapacityProviderCondition::ALL
        .iter()
        .map(|condition| condition.name())
        .collect::<Vec<_>>();
    if !exact(&capacity.provider_conditions, &provider_condition_names) {
        return Err(invalid(
            "capacity provider conditions differ from the frozen T30B matrix",
        ));
    }
    if !exact(
        &capacity.outcome_states,
        &[
            "success",
            "timeout",
            "absent",
            "unsupported",
            "degraded",
            "cancelled",
            "failed",
        ],
    ) {
        return Err(invalid(
            "capacity outcome states must preserve every non-success",
        ));
    }
    if !exact(
        &capacity.canonical_identity_fields,
        &[
            "dataset_name",
            "manifest_sha256",
            "target_identity",
            "expected_accepted_outcome_hash",
            "expected_ready_versus_truth_result_hash",
            "phase",
            "provider_condition",
            "profile",
            "repetition",
            "sample_class",
            "sample_index",
            "deterministic_work_units",
            "correctness",
            "accepted_outcome",
            "ready_versus_truth",
        ],
    ) {
        return Err(invalid(
            "capacity canonical identity fields differ from the frozen evidence contract",
        ));
    }
    if !exact(
        &capacity.transient_evidence_fields,
        &[
            "wall_duration_ms",
            "provider_install_duration_ms",
            "provider_download_duration_ms",
            "atlas_runtime_duration_ms",
            "cache_state",
            "retry_count",
            "route_attempts",
            "cancellation",
        ],
    ) || !exact(
        &capacity.setup_fields,
        &[
            "provider_install_duration_ms",
            "provider_download_duration_ms",
        ],
    ) || !exact(
        &capacity.atlas_work_fields,
        &["atlas_runtime_duration_ms", "deterministic_work_units"],
    ) {
        return Err(invalid(
            "capacity timing and setup/work separation differs from the frozen evidence contract",
        ));
    }
    let matrix_rows = u64::try_from(
        capacity.strata.len() * capacity.phases.len() * capacity.provider_conditions.len() * 3,
    )
    .map_err(|_| invalid("capacity matrix size exceeds u64"))?;
    let samples_per_row = 3 * (5 + 100 + 20);
    if capacity.maximum_raw_records != matrix_rows * samples_per_row
        || capacity.maximum_raw_records != CAPACITY_PROGRESS_TOTAL
        || !exact(
            &capacity.limitations,
            &[
                "private dataset labels are not supported limits or public SLOs",
                "planned records are not execution or capacity evidence",
                "wall time, cache state, retries, route attempts, and cancellation are transient",
                "non-success records remain explicit and durations remain null when unobserved",
                "provider installation and download are separate from Atlas runtime and work units",
                "rust-analyzer is not applicable for repo-small-v1 and optional/degraded otherwise",
                "DEEP runs only in the dedicated progressive response-lifetime condition; DIRECT and LIGHT remain independent valid outcomes",
            ],
        )
    {
        return Err(invalid(
            "capacity evidence bounds or limitations differ from the frozen T30B contract",
        ));
    }
    Ok(())
}

fn resolve_capacity_directory(repository_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repository_root.join(path)
    }
}

struct CapacityEvidenceDirectory {
    path: PathBuf,
    ancestor_guards: Vec<CapacityDirectoryGuard>,
}

impl CapacityEvidenceDirectory {
    fn guard(&self) -> &CapacityDirectoryGuard {
        self.ancestor_guards
            .last()
            .expect("capacity evidence chain always includes target")
    }
}

fn package_excluded_evidence_directory(
    repository_root: &Path,
    path: &Path,
) -> Result<CapacityEvidenceDirectory> {
    let resolved = resolve_capacity_directory(repository_root, path);
    let target = repository_root.join("target");
    if !resolved.starts_with(&target)
        || resolved
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(AtlasError::InvalidConfig(
            "capacity evidence directory must remain under package-excluded target/**".into(),
        ));
    }
    let mut current_path = target.clone();
    let mut ancestor_guards = vec![capacity_directory_guard(&current_path)?];
    let canonical_target = ancestor_guards[0].identity.canonical_path.clone();
    for component in resolved
        .strip_prefix(&target)
        .map_err(|_| AtlasError::InvalidConfig("capacity evidence path escaped target".into()))?
        .components()
    {
        let Component::Normal(name) = component else {
            return Err(AtlasError::InvalidConfig(
                "capacity evidence path contains a non-normal component".into(),
            ));
        };
        validate_capacity_directory_guard(
            &current_path,
            ancestor_guards
                .last()
                .expect("capacity evidence ancestor guard"),
        )?;
        let child_path = current_path.join(name);
        match std::fs::symlink_metadata(&child_path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&child_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        let child_guard = capacity_directory_guard(&child_path)?;
        if !child_guard
            .identity
            .canonical_path
            .starts_with(&canonical_target)
        {
            return Err(AtlasError::InvalidConfig(
                "capacity evidence directory resolves outside package-excluded target/**".into(),
            ));
        }
        current_path = child_path;
        ancestor_guards.push(child_guard);
    }
    Ok(CapacityEvidenceDirectory {
        path: current_path,
        ancestor_guards,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapacityDirectoryIdentity {
    canonical_path: PathBuf,
    #[cfg(windows)]
    volume_serial_number: u64,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

struct CapacityDirectoryGuard {
    identity: CapacityDirectoryIdentity,
    #[cfg(windows)]
    handle: File,
}

#[cfg(windows)]
fn windows_handle_identity(file: &File) -> Result<(u64, u64, u32, u32)> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: the borrowed live handle and exact Windows output structure remain valid
    // for this non-owning system call.
    let succeeded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful GetFileInformationByHandle initializes every field.
    let information = unsafe { information.assume_init() };
    Ok((
        u64::from(information.volume_serial_number),
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low),
        information.attributes,
        information.number_of_links,
    ))
}

fn capacity_directory_guard(path: &Path) -> Result<CapacityDirectoryGuard> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AtlasError::InvalidConfig(format!(
            "capacity evidence directory {} is not a physical directory",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AtlasError::InvalidConfig(format!(
                "capacity evidence directory {} is a Windows reparse point",
                path.display()
            )));
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let handle = options.open(path)?;
        let (volume_serial_number, file_index, attributes, _) = windows_handle_identity(&handle)?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AtlasError::InvalidConfig(format!(
                "capacity evidence directory {} handle is a Windows reparse point",
                path.display()
            )));
        }
        Ok(CapacityDirectoryGuard {
            identity: CapacityDirectoryIdentity {
                canonical_path: std::fs::canonicalize(path)?,
                volume_serial_number,
                file_index,
            },
            handle,
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(CapacityDirectoryGuard {
            identity: CapacityDirectoryIdentity {
                canonical_path: std::fs::canonicalize(path)?,
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(CapacityDirectoryGuard {
            identity: CapacityDirectoryIdentity {
                canonical_path: std::fs::canonicalize(path)?,
            },
        })
    }
}

fn capacity_directory_identity(path: &Path) -> Result<CapacityDirectoryIdentity> {
    let guard = capacity_directory_guard(path)?;
    Ok(guard.identity)
}

fn validate_capacity_directory_guard(path: &Path, guard: &CapacityDirectoryGuard) -> Result<()> {
    #[cfg(windows)]
    {
        let (volume_serial_number, file_index, attributes, _) =
            windows_handle_identity(&guard.handle)?;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || volume_serial_number != guard.identity.volume_serial_number
            || file_index != guard.identity.file_index
            || capacity_directory_identity(path)? != guard.identity
        {
            return Err(AtlasError::Other(format!(
                "capacity evidence directory {} changed while its parent handle was held",
                path.display()
            )));
        }
    }
    #[cfg(not(windows))]
    if capacity_directory_identity(path)? != guard.identity {
        return Err(AtlasError::Other(format!(
            "capacity evidence directory {} changed while writing output",
            path.display()
        )));
    }
    Ok(())
}

fn create_capacity_output(
    evidence_directory: &Path,
    directory_guard: &CapacityDirectoryGuard,
    file_name: &str,
) -> Result<(PathBuf, File)> {
    validate_capacity_directory_guard(evidence_directory, directory_guard)?;
    let output_path = evidence_directory.join(file_name);
    match std::fs::symlink_metadata(&output_path) {
        Ok(_) => {
            return Err(AtlasError::Other(format!(
                "capacity evidence destination {} already exists",
                output_path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&output_path).map_err(|error| {
        let detail = if error.kind() == std::io::ErrorKind::AlreadyExists {
            "already exists".to_string()
        } else {
            error.to_string()
        };
        AtlasError::Other(format!(
            "capacity evidence destination {} cannot be exclusively created: {detail}",
            output_path.display()
        ))
    })?;
    validate_capacity_directory_guard(evidence_directory, directory_guard)?;
    Ok((output_path, file))
}

fn finish_capacity_output(
    evidence_directory: &Path,
    directory_guard: &CapacityDirectoryGuard,
    mut file: File,
    content: &[u8],
) -> Result<File> {
    validate_capacity_directory_guard(evidence_directory, directory_guard)?;
    file.write_all(content)?;
    file.sync_all()?;
    validate_capacity_directory_guard(evidence_directory, directory_guard)?;
    Ok(file)
}

fn validate_capacity_manifests(
    repository_root: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
) -> Result<Vec<Value>> {
    const ACCEPTED_GENERATOR_SHA256: &str =
        "16587e9c6586d5e47b5f95ad1780f125158843d74bfbc3390f0841f8637341b5";
    let generator = repository_root.join(&scenario.dataset.generator);
    if file_sha256(&generator)? != ACCEPTED_GENERATOR_SHA256 {
        return Err(AtlasError::InvalidConfig(
            "capacity generator identity differs from accepted T30A".into(),
        ));
    }
    let manifest_directory = resolve_capacity_directory(repository_root, manifest_directory);
    let mut manifests = Vec::with_capacity(scenario.capacity.strata.len());
    for stratum in &scenario.capacity.strata {
        let path = manifest_directory.join(&stratum.manifest);
        let raw = std::fs::read(&path)?;
        let digest = hex::encode(Sha256::digest(&raw));
        if digest != stratum.file_sha256 {
            return Err(AtlasError::InvalidConfig(format!(
                "{} immutable file digest mismatch",
                stratum.name
            )));
        }
        let manifest: Value = serde_json::from_slice(&raw)?;
        if manifest["dataset_name"] != stratum.name
            || manifest["manifest_sha256"] != stratum.manifest_sha256
            || manifest["counts"]["eligible_files"] != stratum.eligible_files
            || manifest["counts"]["indexed_files"] != stratum.indexed_files
            || manifest["counts"]["changed_files"] != stratum.changed_files
            || manifest["expectations"]["correctness_required"] != true
            || manifest["expectations"]["deterministic_identity_required"] != true
        {
            return Err(AtlasError::InvalidConfig(format!(
                "{} manifest identity differs from the frozen T30A contract",
                stratum.name
            )));
        }
        manifests.push(manifest);
    }
    let generator = repository_root.join(&scenario.dataset.generator);
    for stratum in &scenario.capacity.strata {
        let path = manifest_directory.join(&stratum.manifest);
        let output = Command::new("python3")
            .arg(&generator)
            .arg("--validate-manifest")
            .arg(&path)
            .output()?;
        if !output.status.success() {
            return Err(AtlasError::InvalidConfig(format!(
                "{} regenerated manifest validation failed",
                stratum.name
            )));
        }
    }
    load_capacity_accepted_outcome_contract(
        repository_root,
        scenario,
        &manifest_directory,
        &manifests,
    )?;
    Ok(manifests)
}

fn load_capacity_accepted_outcome_contract(
    repository_root: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    manifests: &[Value],
) -> Result<CapacityAcceptedOutcomeContract> {
    let reference = &scenario.capacity.accepted_outcome_contract;
    if reference.path != "accepted-outcomes-v2.json" {
        return Err(AtlasError::InvalidConfig(
            "capacity accepted-outcome contract path differs from the versioned v2 contract".into(),
        ));
    }
    let directory = resolve_capacity_directory(repository_root, manifest_directory);
    let path = directory.join(&reference.path);
    let raw = std::fs::read(&path)?;
    if hex::encode(Sha256::digest(&raw)) != reference.file_sha256 {
        return Err(AtlasError::InvalidConfig(
            "capacity accepted-outcome contract file digest mismatch".into(),
        ));
    }
    let contract: CapacityAcceptedOutcomeContract = serde_json::from_slice(&raw)?;
    validate_capacity_accepted_outcome_contract(&contract, scenario, manifests)?;
    Ok(contract)
}

fn validate_capacity_accepted_outcome_contract(
    contract: &CapacityAcceptedOutcomeContract,
    scenario: &BenchmarkScenario,
    manifests: &[Value],
) -> Result<()> {
    let invalid = |detail: &str| {
        AtlasError::InvalidConfig(format!(
            "capacity accepted-outcome contract is invalid: {detail}"
        ))
    };
    if contract.schema_version != "2.0.0"
        || contract.kind != "capacity-accepted-outcome-contract"
        || contract.preflight_profile != "small"
        || contract.supersedes.contract != "capacity-manifest-v1-accepted-outcome"
        || contract.supersedes.reason
            != "the v1 fields preserve fixture arithmetic provenance but do not describe production Truth rows"
    {
        return Err(invalid("identity or explicit supersession differs"));
    }
    if contract.truth_projection.generation
        != "active committed generation selected from the registered workspace after production reconcile"
        || contract.truth_projection.target_identity
            != "exactly one current_symbol association for the immutable manifest path/symbol plus the live source SHA-256"
        || contract.truth_projection.canonical_hash
            != ["dataset_name", "evidence_counts", "target_identity"]
    {
        return Err(invalid("Truth projection identity differs"));
    }
    let expected_rows = BTreeMap::from([
        (
            "conflicts".to_string(),
            "all evidence_conflict rows whose generation_id is the active generation".to_string(),
        ),
        (
            "coverage".to_string(),
            "all coverage_record rows whose generation_id is the active generation".to_string(),
        ),
        (
            "diagnostics".to_string(),
            "all diagnostic rows whose generation_id is the active generation".to_string(),
        ),
        (
            "effects".to_string(),
            "distinct effect_fact_id values joined through extractor_run and generation_file to the active generation".to_string(),
        ),
        (
            "relationships".to_string(),
            "distinct relationship_fact_id values joined through extractor_run and generation_file to the active generation".to_string(),
        ),
        (
            "symbols".to_string(),
            "distinct symbol_fact_id values joined through extractor_run and generation_file to the active generation".to_string(),
        ),
    ]);
    if contract.truth_projection.rows != expected_rows {
        return Err(invalid("immutable Truth row projection differs"));
    }
    if manifests.len() != scenario.capacity.strata.len()
        || contract.supersedes.manifests.len() != manifests.len()
    {
        return Err(invalid("historical manifest coverage differs"));
    }
    for ((historical, manifest), stratum) in contract
        .supersedes
        .manifests
        .iter()
        .zip(manifests)
        .zip(&scenario.capacity.strata)
    {
        if historical.dataset_name != stratum.name
            || historical.manifest_sha256 != manifest["manifest_sha256"]
            || historical.evidence_counts != manifest["evidence_counts"]
            || historical.expected_accepted_outcome_hash
                != manifest["expected_accepted_outcome_hash"]
            || historical.expected_ready_versus_truth_result_hash
                != manifest["expected_ready_versus_truth_result_hash"]
        {
            return Err(invalid("v1 provenance is not preserved exactly"));
        }
    }

    let baseline_phases = [
        CapacityPhase::ColdInitializationReconcile.descriptor().name,
        CapacityPhase::NoChangeReconcile.descriptor().name,
        CapacityPhase::ReadyServingQueryCompiler.descriptor().name,
        CapacityPhase::TruthFallbackQueryCompiler.descriptor().name,
        CapacityPhase::LightRouteCompiler.descriptor().name,
        CapacityPhase::ProgressiveDeepRouteCompiler
            .descriptor()
            .name,
        CapacityPhase::CatalogueGrowth.descriptor().name,
    ];
    let incremental_phases = [CapacityPhase::FixedIncrementalReconcile.descriptor().name];
    let mut expected_index = 0_usize;
    for (manifest, stratum) in manifests.iter().zip(&scenario.capacity.strata) {
        for condition in &scenario.capacity.provider_conditions {
            let provider = resolve_capacity_provider_condition(&stratum.name, condition)?;
            if !provider.is_applicable() {
                continue;
            }
            for (truth_state, phases) in [
                ("baseline", baseline_phases.as_slice()),
                ("fixed_incremental", incremental_phases.as_slice()),
            ] {
                let outcome = contract
                    .outcomes
                    .get(expected_index)
                    .ok_or_else(|| invalid("an applicable dataset/provider/state row is absent"))?;
                expected_index += 1;
                let expected_hash = canonical_json_sha256(&serde_json::json!({
                    "dataset_name": stratum.name,
                    "evidence_counts": outcome.evidence_counts,
                    "target_identity": outcome.target_identity,
                }))?;
                let expected_ready = ready_truth_identity(&expected_hash, true)?
                    .expect("equivalent accepted outcome always has an identity");
                if outcome.dataset_name != stratum.name
                    || outcome.manifest_sha256 != manifest["manifest_sha256"]
                    || outcome.provider_condition != provider.name()
                    || outcome.provider_members != provider.members()
                    || outcome.configuration_hash
                        != provider.configuration_hash()?.ok_or_else(|| {
                            invalid("applicable provider configuration hash is absent")
                        })?
                    || outcome.truth_state != truth_state
                    || outcome
                        .phases
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        != phases
                    || outcome.preflight_phase
                        != if truth_state == "baseline" {
                            CapacityPhase::NoChangeReconcile.descriptor().name
                        } else {
                            CapacityPhase::FixedIncrementalReconcile.descriptor().name
                        }
                    || !["success", "degraded"]
                        .contains(&outcome.expected_observation_state.as_str())
                    || outcome.expected_eligible_files == 0
                    || outcome.expected_indexed_files != outcome.expected_eligible_files
                    || outcome.expected_eligible_files
                        > manifest["counts"]["eligible_files"].as_u64().unwrap_or(0)
                    || outcome.expected_source_bytes == 0
                    || (truth_state == "baseline"
                        && (outcome.expected_eligible_files
                            != manifest["counts"]["eligible_files"].as_u64().unwrap_or(0)
                            || outcome.expected_indexed_files
                                != manifest["counts"]["indexed_files"].as_u64().unwrap_or(0)
                            || outcome.expected_source_bytes
                                != manifest["counts"]["total_bytes"].as_u64().unwrap_or(0)))
                    || outcome.target_identity != manifest["target_identity"]
                    || outcome.expected_accepted_outcome_hash != expected_hash
                    || outcome.expected_ready_versus_truth_result_hash != expected_ready
                {
                    return Err(invalid(
                        "dataset/provider/state identity or derived outcome differs",
                    ));
                }
            }
        }
    }
    if expected_index != contract.outcomes.len() {
        return Err(invalid("unexpected or duplicate outcome rows are present"));
    }
    Ok(())
}

fn capacity_accepted_outcome<'a>(
    manifest: &Value,
    contract: &'a CapacityAcceptedOutcomeContract,
    provider_condition: &str,
    phase: CapacityPhase,
) -> Result<&'a CapacityAcceptedOutcome> {
    let dataset_name = manifest["dataset_name"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest dataset name absent".into()))?;
    contract
        .outcomes
        .iter()
        .find(|outcome| {
            outcome.dataset_name == dataset_name
                && outcome.provider_condition == provider_condition
                && outcome
                    .phases
                    .iter()
                    .any(|name| name == phase.descriptor().name)
        })
        .ok_or_else(|| {
            AtlasError::InvalidConfig(format!(
                "production accepted outcome absent for {dataset_name}/{provider_condition}/{}",
                phase.descriptor().name
            ))
        })
}

fn resolve_capacity_provider_condition(
    dataset_name: &str,
    condition_name: &str,
) -> Result<ResolvedCapacityProviderCondition> {
    ResolvedCapacityProviderCondition::parse(dataset_name, condition_name)
}

fn capacity_phase_contract(phase_name: &str) -> Result<Value> {
    let descriptor = CapacityPhase::parse(phase_name)?.descriptor();
    Ok(serde_json::json!({
        "fixture_lifecycle": descriptor.fixture_lifecycle,
        "catalogue_lifecycle": descriptor.catalogue_lifecycle,
        "serving_state": descriptor.serving_state,
        "cache_state": {
            "warmup": descriptor.warm_cache_state,
            "warm": descriptor.warm_cache_state,
            "cold": descriptor.cold_cache_state,
        },
        "action": descriptor.action,
        "preparation": descriptor.preparation,
        "reset": descriptor.reset,
        "route_attempts": descriptor.route_attempts,
        "measurements": {
            "catalogue": descriptor.catalogue_measurements,
            "provider": descriptor.provider_measurements,
            "reconcile": descriptor.reconcile_measurements,
            "ready_versus_truth": descriptor.ready_truth_measurements,
            "compiler": {
                "applicable": descriptor.compiler_fallback.is_some(),
                "fallback": descriptor.compiler_fallback,
            },
        },
    }))
}

fn capacity_global_plan_sha256(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
) -> Result<String> {
    let mut rows = Vec::with_capacity(CAPACITY_GLOBAL_MATRIX_ROWS as usize);
    let mut global_matrix_row_index = 0_u64;
    for stratum in &scenario.capacity.strata {
        for profile in &scenario.profiles {
            for provider_condition in &scenario.capacity.provider_conditions {
                let resolved =
                    resolve_capacity_provider_condition(&stratum.name, provider_condition)?;
                for phase in &scenario.capacity.phases {
                    rows.push(serde_json::json!({
                        "global_matrix_row_index": global_matrix_row_index,
                        "dataset_name": stratum.name,
                        "manifest_sha256": stratum.manifest_sha256,
                        "profile": profile.name,
                        "provider_condition": resolved.name(),
                        "phase": phase,
                    }));
                    global_matrix_row_index += 1;
                }
            }
        }
    }
    if global_matrix_row_index != CAPACITY_GLOBAL_MATRIX_ROWS {
        return Err(AtlasError::Other(
            "capacity global plan does not contain exactly 360 matrix rows".into(),
        ));
    }
    canonical_json_sha256(&serde_json::json!({
        "schema_version": "1.0.0",
        "kind": "capacity-global-plan-identity",
        "scenario_sha256": file_sha256(scenario_path)?,
        "generator_sha256": file_sha256(&repository_root.join(&scenario.dataset.generator))?,
        "capacity_schema_version": scenario.capacity.schema_version,
        "procedure": scenario.procedure,
        "strata": scenario.capacity.strata,
        "rows": rows,
        "global_matrix_rows": CAPACITY_GLOBAL_MATRIX_ROWS,
        "global_attempts": scenario.capacity.maximum_raw_records,
    }))
}

fn capacity_execution_scope(shard: Option<CapacityShard>, global_plan_sha256: &str) -> Value {
    let selected_matrix_rows = if shard.is_some() {
        CAPACITY_SHARD_MATRIX_ROWS
    } else {
        CAPACITY_GLOBAL_MATRIX_ROWS
    };
    let selected_attempts = if shard.is_some() {
        CAPACITY_SHARD_ATTEMPTS
    } else {
        CAPACITY_PROGRESS_TOTAL
    };
    serde_json::json!({
        "mode": if shard.is_some() { "shard" } else { "full" },
        "shard_index": shard.map(|value| value.index),
        "shard_count": shard.map(|_| CAPACITY_SHARD_COUNT),
        "index_base": 0,
        "assignment": "global-matrix-row-index-modulo-4",
        "global_matrix_rows": CAPACITY_GLOBAL_MATRIX_ROWS,
        "selected_matrix_rows": selected_matrix_rows,
        "attempts_per_matrix_row": CAPACITY_ATTEMPTS_PER_MATRIX_ROW,
        "global_attempts": CAPACITY_PROGRESS_TOTAL,
        "selected_attempts": selected_attempts,
        "global_plan_sha256": global_plan_sha256,
    })
}

fn capacity_execution_total(shard: Option<CapacityShard>) -> u64 {
    if shard.is_some() {
        CAPACITY_SHARD_ATTEMPTS
    } else {
        CAPACITY_PROGRESS_TOTAL
    }
}

fn write_capacity_plan(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
    shard: Option<CapacityShard>,
) -> Result<PathBuf> {
    let manifests = validate_capacity_manifests(repository_root, scenario, manifest_directory)?;
    let accepted_outcomes = load_capacity_accepted_outcome_contract(
        repository_root,
        scenario,
        manifest_directory,
        &manifests,
    )?;
    let global_plan_sha256 = capacity_global_plan_sha256(repository_root, scenario_path, scenario)?;
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    let revision = command_text("git", &["rev-parse", "HEAD"]);
    if revision == "unavailable" {
        return Err(AtlasError::Other(
            "cannot plan capacity evidence without the tested Git revision".into(),
        ));
    }
    let mut matrix = Vec::with_capacity(CAPACITY_GLOBAL_MATRIX_ROWS as usize);
    let mut global_matrix_row_index = 0_u64;
    for stratum in &scenario.capacity.strata {
        for profile in &scenario.profiles {
            for provider_condition in &scenario.capacity.provider_conditions {
                let resolved =
                    resolve_capacity_provider_condition(&stratum.name, provider_condition)?;
                let configuration_hash = resolved.configuration_hash()?;
                for phase in &scenario.capacity.phases {
                    let row_index = global_matrix_row_index;
                    global_matrix_row_index += 1;
                    if shard.is_some_and(|value| !value.selects(row_index)) {
                        continue;
                    }
                    matrix.push(serde_json::json!({
                        "global_matrix_row_index": row_index,
                        "dataset_name": stratum.name,
                        "manifest_sha256": stratum.manifest_sha256,
                        "profile": profile.name,
                        "provider_condition": resolved.name(),
                        "provider_applicability": resolved.outcome_policy,
                        "phase": phase,
                        "provider_members": resolved.members(),
                        "provider_contract": resolved.providers().collect::<Vec<_>>(),
                        "configuration_hash": configuration_hash,
                        "provider_setup": {
                            "ownership": "preinstalled by the private runner; Atlas never installs or downloads providers",
                            "install_duration_field": "provider_install_duration_ms",
                            "download_duration_field": "provider_download_duration_ms",
                            "unobserved_value": null,
                        },
                        "profile_inputs": profile,
                        "execution": capacity_phase_contract(phase)?,
                        "sample_plan": {
                            "unscored_warmups": scenario.procedure.warmups,
                            "scored_warm_samples": scenario.procedure.warm_samples,
                            "cold_samples": scenario.procedure.cold_samples,
                            "independent_repetitions": scenario.procedure.repetitions,
                            "percentile_method": scenario.procedure.percentile_method,
                        }
                    }));
                }
            }
        }
    }
    if global_matrix_row_index != CAPACITY_GLOBAL_MATRIX_ROWS
        || matrix.len() as u64
            != if shard.is_some() {
                CAPACITY_SHARD_MATRIX_ROWS
            } else {
                CAPACITY_GLOBAL_MATRIX_ROWS
            }
    {
        return Err(AtlasError::Other(
            "capacity shard selection did not retain its exact matrix row contract".into(),
        ));
    }
    let plan = serde_json::json!({
        "schema_version": "1.0.0",
        "kind": "capacity-execution-plan",
        "provenance": {
            "revision": revision,
            "scenario_path": scenario_path.to_string_lossy().replace('\\', "/"),
            "scenario_sha256": file_sha256(scenario_path)?,
            "generator": scenario.dataset.generator,
            "generator_version": "capacity-fixture-generator-v1.0.0",
            "platform": {
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
                "cpu": cpu_description(),
                "toolchain": command_text("rustc", &["--version", "--verbose"]),
            },
            "claim": "execution plan only; no capacity campaign or supported-limit/SLO claim",
        },
        "procedure": scenario.procedure,
        "strata": manifests,
        "accepted_outcome_contract": {
            "path": scenario.capacity.accepted_outcome_contract.path,
            "file_sha256": scenario.capacity.accepted_outcome_contract.file_sha256,
            "schema_version": accepted_outcomes.schema_version,
            "kind": accepted_outcomes.kind,
            "supersedes": accepted_outcomes.supersedes,
            "truth_projection": accepted_outcomes.truth_projection,
            "outcome_count": accepted_outcomes.outcomes.len(),
        },
        "matrix": matrix,
        "execution_scope": capacity_execution_scope(shard, &global_plan_sha256),
        "evidence_contract": {
            "raw_evidence_relative_root": scenario.capacity.raw_evidence_relative_root,
            "maximum_raw_records": scenario.capacity.maximum_raw_records,
            "outcome_states": scenario.capacity.outcome_states,
            "non_success_retention": "required; never zero-filled, omitted, or aggregated into success",
            "canonical_identity_fields": scenario.capacity.canonical_identity_fields,
            "transient_evidence_fields": scenario.capacity.transient_evidence_fields,
            "timing_is_canonical_identity": false,
            "setup_fields": scenario.capacity.setup_fields,
            "atlas_work_fields": scenario.capacity.atlas_work_fields,
            "compiler_measurement_fields": CAPACITY_COMPILER_MEASUREMENT_FIELDS,
            "unobserved_duration_value": null,
            "record_bound_policy": format!(
                "fail before writing record {}; never truncate or change retention method",
                scenario.capacity.maximum_raw_records + 1
            ),
            "outcome_policy": {
                "required_builtin_and_typescript": "every non-success or correctness/identity mismatch fails after complete evidence retention",
                "optional_rust": "repo-small rust-only is not-applicable/unsupported; non-small rust-only and combined optional-Rust degradation remain explicit",
                "aggregation": "only fully validated successful scored-warm observations",
            },
            "accepted_outcome_preflight": {
                "required_before_campaign": true,
                "executor": "production",
                "fixture": "clean-disposable",
                "attempts": "one clean production attempt per applicable dataset/provider/Truth-state outcome; baseline uses no-change and changed Truth uses fixed-incremental",
                "exact_match": true,
                "evidence_file": CAPACITY_ACCEPTED_OUTCOME_PREFLIGHT_FILE,
            },
            "phase_applicability": CAPACITY_PHASES.iter().map(|phase| serde_json::json!({
                "phase": phase.name,
                "route_attempts": phase.route_attempts,
                "catalogue": phase.catalogue_measurements,
                "provider": phase.provider_measurements,
                "reconcile": phase.reconcile_measurements,
                "ready_versus_truth": phase.ready_truth_measurements,
                "compiler": phase.compiler_fallback.is_some(),
            })).collect::<Vec<_>>(),
            "required_result_fields": [
                "lifecycle_id",
                "outcome",
                "correctness",
                "deterministic_identity",
                "observed_target_identity",
                "observed_evidence_counts",
                "accepted_outcome",
                "ready_versus_truth",
                "deterministic_work_units",
                "eligible_files",
                "indexed_files",
                "parsed_files",
                "changed_files",
                "changed_bytes",
                "source_bytes",
                "compiler_selected_records",
                "compiler_selected_source_bytes",
                "compiler_selected_estimated_tokens",
                "compiler_omitted_records",
                "compiler_truncated",
                "compiler_work_units_consumed",
                "compiler_fallback",
                "provider_probes",
                "provider_executions_run",
                "provider_outputs",
                "provider_output_bytes",
                "provider_executions_reused",
                "required_provider_failure",
                "configuration_hash",
                "catalogue_rows_before",
                "catalogue_rows_after",
                "catalogue_bytes_before",
                "catalogue_bytes_after",
                "durable_catalogue_rows_before",
                "durable_catalogue_rows_after",
                "durable_catalogue_bytes_before",
                "durable_catalogue_bytes_after",
                "derived_catalogue_rows_before",
                "derived_catalogue_rows_after",
                "derived_catalogue_bytes_before",
                "derived_catalogue_bytes_after",
                "provider_install_duration_ms",
                "provider_download_duration_ms",
                "atlas_runtime_duration_ms"
            ],
        },
        "limitations": scenario.capacity.limitations,
    });
    let (output_path, file) = create_capacity_output(
        &evidence.path,
        evidence.guard(),
        "capacity-execution-plan-v1.json",
    )?;
    let _file = finish_capacity_output(
        &evidence.path,
        evidence.guard(),
        file,
        &serde_json::to_vec_pretty(&plan)?,
    )?;
    Ok(output_path)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CapacityObservationKey {
    dataset_name: String,
    profile: String,
    provider_condition: String,
    phase: String,
    repetition: u64,
    sample_class: String,
    sample_index: u64,
}

fn capacity_attempt_belongs_to_scope(
    key: &CapacityObservationKey,
    scenario: &BenchmarkScenario,
    shard: Option<CapacityShard>,
) -> bool {
    let row_index = scenario
        .capacity
        .strata
        .iter()
        .position(|value| value.name == key.dataset_name)
        .zip(
            scenario
                .profiles
                .iter()
                .position(|value| value.name == key.profile),
        )
        .zip(
            scenario
                .capacity
                .provider_conditions
                .iter()
                .position(|value| value == &key.provider_condition),
        )
        .zip(
            scenario
                .capacity
                .phases
                .iter()
                .position(|value| value == &key.phase),
        )
        .map(|(((stratum, profile), provider), phase)| {
            (((stratum * scenario.profiles.len() + profile)
                * scenario.capacity.provider_conditions.len()
                + provider)
                * scenario.capacity.phases.len()
                + phase) as u64
        });
    let sample_count = match key.sample_class.as_str() {
        "warmup" => scenario.procedure.warmups,
        "warm" => scenario.procedure.warm_samples,
        "cold" => scenario.procedure.cold_samples,
        _ => return false,
    };
    row_index.is_some_and(|index| shard.is_none_or(|value| value.selects(index)))
        && (1..=scenario.procedure.repetitions).contains(&key.repetition)
        && (1..=sample_count).contains(&key.sample_index)
}

#[derive(Clone, Debug, Serialize)]
struct CapacityRawObservation {
    schema_version: &'static str,
    kind: &'static str,
    provenance_sha256: String,
    dataset_name: String,
    manifest_sha256: String,
    target_identity: Value,
    expected_eligible_files: u64,
    expected_configuration_hash: Option<String>,
    expected_indexed_files: u64,
    expected_changed_files: u64,
    expected_changed_bytes: u64,
    expected_source_bytes: u64,
    expected_accepted_outcome_hash: String,
    expected_ready_versus_truth_result_hash: String,
    phase: String,
    provider_condition: String,
    provider_members: Vec<String>,
    profile: String,
    profile_inputs: Value,
    repetition: u64,
    sample_class: String,
    sample_index: u64,
    lifecycle_id: String,
    outcome: String,
    preparation_failure: Option<String>,
    correctness: Option<bool>,
    deterministic_identity: Option<String>,
    observed_target_identity: Option<Value>,
    observed_evidence_counts: Option<Value>,
    accepted_outcome: Option<String>,
    ready_versus_truth: Option<String>,
    deterministic_work_units: Option<u64>,
    eligible_files: Option<u64>,
    indexed_files: Option<u64>,
    parsed_files: Option<u64>,
    changed_files: Option<u64>,
    changed_bytes: Option<u64>,
    source_bytes: Option<u64>,
    compiler_selected_records: Option<u64>,
    compiler_selected_source_bytes: Option<u64>,
    compiler_selected_estimated_tokens: Option<u64>,
    compiler_omitted_records: Option<u64>,
    compiler_truncated: Option<bool>,
    compiler_work_units_consumed: Option<u64>,
    compiler_fallback: Option<bool>,
    provider_probes: Option<u64>,
    provider_executions_run: Option<u64>,
    provider_outputs: Option<u64>,
    provider_output_bytes: Option<u64>,
    provider_executions_reused: Option<u64>,
    required_provider_failure: Option<bool>,
    configuration_hash: Option<String>,
    catalogue_rows_before: Option<u64>,
    catalogue_rows_after: Option<u64>,
    catalogue_bytes_before: Option<u64>,
    catalogue_bytes_after: Option<u64>,
    durable_catalogue_rows_before: Option<u64>,
    durable_catalogue_rows_after: Option<u64>,
    durable_catalogue_bytes_before: Option<u64>,
    durable_catalogue_bytes_after: Option<u64>,
    derived_catalogue_rows_before: Option<u64>,
    derived_catalogue_rows_after: Option<u64>,
    derived_catalogue_bytes_before: Option<u64>,
    derived_catalogue_bytes_after: Option<u64>,
    provider_install_duration_ms: Option<u64>,
    provider_download_duration_ms: Option<u64>,
    atlas_runtime_duration_ms: Option<u64>,
    wall_duration_ms: Option<u64>,
    cache_state: String,
    retry_count: u64,
    route_attempts: u64,
    cancellation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CapacityAggregationKey {
    dataset_name: String,
    profile: String,
    provider_condition: String,
    phase: String,
    repetition: u64,
}

impl From<&CapacityRawObservation> for CapacityAggregationKey {
    fn from(observation: &CapacityRawObservation) -> Self {
        Self {
            dataset_name: observation.dataset_name.clone(),
            profile: observation.profile.clone(),
            provider_condition: observation.provider_condition.clone(),
            phase: observation.phase.clone(),
            repetition: observation.repetition,
        }
    }
}

impl CapacityRawObservation {
    fn key(&self) -> CapacityObservationKey {
        CapacityObservationKey {
            dataset_name: self.dataset_name.clone(),
            profile: self.profile.clone(),
            provider_condition: self.provider_condition.clone(),
            phase: self.phase.clone(),
            repetition: self.repetition,
            sample_class: self.sample_class.clone(),
            sample_index: self.sample_index,
        }
    }
}

struct CapacityRawWriter {
    path: PathBuf,
    directory: PathBuf,
    file: BufWriter<File>,
    seen: BTreeSet<CapacityObservationKey>,
    maximum_records: u64,
}

#[derive(Debug)]
struct FinishedCapacityRaw {
    path: PathBuf,
    file: File,
    sha256: String,
}

fn sha256_file_handle(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

impl CapacityRawWriter {
    fn create(
        evidence_directory: &Path,
        directory_guard: &CapacityDirectoryGuard,
        maximum_records: u64,
        raw_file_name: &str,
    ) -> Result<Self> {
        let (path, file) =
            create_capacity_output(evidence_directory, directory_guard, raw_file_name)?;
        Ok(Self {
            path,
            directory: evidence_directory.to_path_buf(),
            file: BufWriter::new(file),
            seen: BTreeSet::new(),
            maximum_records,
        })
    }

    fn write(&mut self, observation: &CapacityRawObservation) -> Result<()> {
        let attempted = u64::try_from(self.seen.len())
            .map_err(|_| AtlasError::Other("raw observation count exceeds u64".into()))?;
        if attempted >= self.maximum_records {
            return Err(AtlasError::Other(format!(
                "raw observation ceiling {} would be exceeded",
                self.maximum_records
            )));
        }
        if !self.seen.insert(observation.key()) {
            return Err(AtlasError::Other(
                "duplicate raw observation key rejected".into(),
            ));
        }
        serde_json::to_writer(&mut self.file, observation)?;
        self.file.write_all(b"\n")?;
        Ok(())
    }

    fn finish(
        mut self,
        expected: &BTreeSet<CapacityObservationKey>,
        directory_guard: &CapacityDirectoryGuard,
    ) -> Result<FinishedCapacityRaw> {
        if &self.seen != expected {
            return Err(AtlasError::Other(format!(
                "missing required raw observations: expected {}, recorded {}",
                expected.len(),
                self.seen.len()
            )));
        }
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        validate_capacity_directory_guard(&self.directory, directory_guard)?;
        let mut file = self
            .file
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        let sha256 = sha256_file_handle(&mut file)?;
        Ok(FinishedCapacityRaw {
            path: self.path,
            file,
            sha256,
        })
    }
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn nearest_rank(values: &mut [u64], percentile: u64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let numerator = percentile.saturating_mul(values.len() as u64);
    let rank = numerator.div_ceil(100).max(1);
    values.get(usize::try_from(rank - 1).ok()?).copied()
}

fn nearest_rank_summary(values: &[u64]) -> Value {
    let mut p50_values = values.to_vec();
    let mut p95_values = values.to_vec();
    let mut p99_values = values.to_vec();
    serde_json::json!({
        "count": values.len(),
        "p50": nearest_rank(&mut p50_values, 50),
        "p95": nearest_rank(&mut p95_values, 95),
        "p99": nearest_rank(&mut p99_values, 99),
    })
}

#[derive(Clone, Copy)]
struct CapacityEvidenceNames {
    raw_file: &'static str,
    summary_file: &'static str,
    raw_kind: &'static str,
    summary_kind: &'static str,
}

const CAPACITY_ACCEPTED_OUTCOME_PREFLIGHT_FILE: &str =
    "capacity-accepted-outcome-preflight-v2.json";
const PRODUCTION_CAPACITY_EVIDENCE: CapacityEvidenceNames = CapacityEvidenceNames {
    raw_file: "capacity-raw-observations-v1.ndjson",
    summary_file: "capacity-summary-v1.json",
    raw_kind: "capacity-raw-observation",
    summary_kind: "capacity-evidence-summary",
};

#[cfg(test)]
const TEST_CAPACITY_EVIDENCE: CapacityEvidenceNames = CapacityEvidenceNames {
    raw_file: "test-capacity-raw-observations-v1.ndjson",
    summary_file: "test-capacity-summary-v1.json",
    raw_kind: "test-capacity-raw-observation",
    summary_kind: "test-capacity-evidence-summary",
};

const MEMORY_LABEL_STATE_FILE: &str = "capacity-memory-label-state-v1.json";
const MEMORY_RAW_PREFIX: &str = "capacity-memory-raw-v2";
const MEMORY_SUMMARY_FILE: &str = "capacity-memory-summary-v2.json";
const MEMORY_MAX_SEGMENT_SAMPLES: u64 = 200_000;
const MEMORY_MAX_RAW_BYTES: u64 = 512 * 1024 * 1024;
const MEMORY_MAX_SUMMARY_BYTES: u64 = 1024 * 1024;
const MEMORY_MAX_SEGMENTS: usize = 1024;
#[cfg(test)]
const MEMORY_LEGACY_RAW_FILE: &str = "capacity-memory-raw-v1.ndjson";
#[cfg(test)]
const MEMORY_LEGACY_SUMMARY_FILE: &str = "capacity-memory-summary-v1.json";
const CAPACITY_PROGRESS_TOTAL: u64 = 135_000;
const CAPACITY_GLOBAL_MATRIX_ROWS: u64 = 360;
const CAPACITY_ATTEMPTS_PER_MATRIX_ROW: u64 = 375;
const CAPACITY_SHARD_COUNT: u64 = 4;
const CAPACITY_SHARD_MATRIX_ROWS: u64 = 90;
const CAPACITY_SHARD_ATTEMPTS: u64 = 33_750;

fn valid_capacity_progress_total(total: u64) -> bool {
    matches!(total, CAPACITY_PROGRESS_TOTAL | CAPACITY_SHARD_ATTEMPTS)
}
const CAPACITY_PROGRESS_INTERVAL: u64 = 1_000;
const CAPACITY_PROGRESS_PREFIX: &str = "ATLAS_CAPACITY_PROGRESS";
const SAMPLER_PROGRESS_MAX_LINE_BYTES: usize = 160;
const SAMPLER_PROGRESS_MAX_LINES: usize = 2_000;
const SAMPLER_DIAGNOSTIC_MAX_BYTES: usize = 2_000;

const CAPACITY_LOG_PREFIX: &str = "ATLAS_CAPACITY_LOG ";
const CAPACITY_LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;
const CAPACITY_LOG_CHUNK_BYTES: usize = 45 * 1024;
const CAPACITY_LOG_MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const CAPACITY_LOG_MAX_RAW_BYTES: u64 = 512 * 1024 * 1024;
const CAPACITY_LOG_MAX_SUMMARY_BYTES: u64 = 4 * 1024 * 1024;
const CAPACITY_COMPILER_MEASUREMENT_FIELDS: &[&str] = &[
    "compiler_selected_records",
    "compiler_selected_source_bytes",
    "compiler_selected_estimated_tokens",
    "compiler_omitted_records",
    "compiler_truncated",
    "compiler_work_units_consumed",
    "compiler_fallback",
];

const CAPACITY_RAW_FIELDS: &[&str] = &[
    "schema_version",
    "kind",
    "provenance_sha256",
    "dataset_name",
    "manifest_sha256",
    "target_identity",
    "expected_eligible_files",
    "expected_configuration_hash",
    "expected_indexed_files",
    "expected_changed_files",
    "expected_changed_bytes",
    "expected_source_bytes",
    "expected_accepted_outcome_hash",
    "expected_ready_versus_truth_result_hash",
    "phase",
    "provider_condition",
    "provider_members",
    "profile",
    "profile_inputs",
    "repetition",
    "sample_class",
    "sample_index",
    "lifecycle_id",
    "outcome",
    "preparation_failure",
    "correctness",
    "deterministic_identity",
    "observed_target_identity",
    "observed_evidence_counts",
    "accepted_outcome",
    "ready_versus_truth",
    "deterministic_work_units",
    "eligible_files",
    "indexed_files",
    "parsed_files",
    "changed_files",
    "changed_bytes",
    "source_bytes",
    "compiler_selected_records",
    "compiler_selected_source_bytes",
    "compiler_selected_estimated_tokens",
    "compiler_omitted_records",
    "compiler_truncated",
    "compiler_work_units_consumed",
    "compiler_fallback",
    "provider_probes",
    "provider_executions_run",
    "provider_outputs",
    "provider_output_bytes",
    "provider_executions_reused",
    "required_provider_failure",
    "configuration_hash",
    "catalogue_rows_before",
    "catalogue_rows_after",
    "catalogue_bytes_before",
    "catalogue_bytes_after",
    "durable_catalogue_rows_before",
    "durable_catalogue_rows_after",
    "durable_catalogue_bytes_before",
    "durable_catalogue_bytes_after",
    "derived_catalogue_rows_before",
    "derived_catalogue_rows_after",
    "derived_catalogue_bytes_before",
    "derived_catalogue_bytes_after",
    "provider_install_duration_ms",
    "provider_download_duration_ms",
    "atlas_runtime_duration_ms",
    "wall_duration_ms",
    "cache_state",
    "retry_count",
    "route_attempts",
    "cancellation",
];

#[derive(Clone, Serialize)]
struct CapacityLogChunk {
    kind: &'static str,
    stream: String,
    index: u64,
    count: u64,
    data: String,
}

#[derive(Clone)]
struct CapacityLogStream {
    name: String,
    source_bytes: u64,
    source_sha256: String,
    redacted_bytes: u64,
    redacted_sha256: String,
    compressed_bytes: u64,
    compressed_sha256: String,
    record_count: u64,
    chunk_bytes_bound: u64,
    chunks: Vec<CapacityLogChunk>,
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let first = group[0];
        let second = group.get(1).copied().unwrap_or(0);
        let third = group.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if group.len() > 1 {
            ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if group.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let decode = |byte: u8| -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    if !text.len().is_multiple_of(4) {
        return Err(AtlasError::Other(
            "capacity log base64 length is invalid".into(),
        ));
    }
    let mut output = Vec::with_capacity(text.len() / 4 * 3);
    for group in text.as_bytes().as_chunks::<4>().0 {
        let first = decode(group[0])
            .ok_or_else(|| AtlasError::Other("capacity log base64 is invalid".into()))?;
        let second = decode(group[1])
            .ok_or_else(|| AtlasError::Other("capacity log base64 is invalid".into()))?;
        let third = if group[2] == b'=' {
            0
        } else {
            decode(group[2])
                .ok_or_else(|| AtlasError::Other("capacity log base64 is invalid".into()))?
        };
        let fourth = if group[3] == b'=' {
            0
        } else {
            decode(group[3])
                .ok_or_else(|| AtlasError::Other("capacity log base64 is invalid".into()))?
        };
        output.push((first << 2) | (second >> 4));
        if group[2] != b'=' {
            output.push((second << 4) | (third >> 2));
        }
        if group[3] != b'=' {
            output.push((third << 6) | fourth);
        }
    }
    Ok(output)
}

fn chunk_capacity_log_stream(
    name: &str,
    compressed: &[u8],
    redacted_bytes: u64,
    redacted_sha256: String,
    chunk_bytes: usize,
) -> Result<CapacityLogStream> {
    if compressed.is_empty() || chunk_bytes == 0 || chunk_bytes > CAPACITY_LOG_CHUNK_BYTES {
        return Err(AtlasError::Other(
            "capacity log compressed payload or chunk bound is invalid".into(),
        ));
    }
    let chunk_count = u64::try_from(compressed.len().div_ceil(chunk_bytes))
        .map_err(|_| AtlasError::Other("capacity log chunk count exceeds u64".into()))?;
    let chunks = compressed
        .chunks(chunk_bytes)
        .enumerate()
        .map(|(index, bytes)| {
            Ok(CapacityLogChunk {
                kind: "capacity-evidence-chunk",
                stream: name.to_string(),
                index: u64::try_from(index + 1).map_err(|_| {
                    AtlasError::Other("capacity log chunk index exceeds u64".into())
                })?,
                count: chunk_count,
                data: base64_encode(bytes),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CapacityLogStream {
        name: name.to_string(),
        source_bytes: redacted_bytes,
        source_sha256: redacted_sha256.clone(),
        redacted_bytes,
        redacted_sha256,
        compressed_bytes: compressed.len() as u64,
        compressed_sha256: hex::encode(Sha256::digest(compressed)),
        record_count: 0,
        chunk_bytes_bound: chunk_bytes as u64,
        chunks,
    })
}

#[cfg(test)]
fn reconstruct_capacity_log_stream(stream: &CapacityLogStream) -> Result<Vec<u8>> {
    let expected_count = u64::try_from(stream.chunks.len())
        .map_err(|_| AtlasError::Other("capacity log test chunk count exceeds u64".into()))?;
    let mut reconstructed = Vec::new();
    for (offset, chunk) in stream.chunks.iter().enumerate() {
        if chunk.kind != "capacity-evidence-chunk"
            || chunk.stream != stream.name
            || chunk.index != u64::try_from(offset + 1).unwrap_or(u64::MAX)
            || chunk.count != expected_count
            || chunk.count == 0
        {
            return Err(AtlasError::Other(
                "capacity log chunk sequence is incomplete or reordered".into(),
            ));
        }
        reconstructed.extend(base64_decode(&chunk.data)?);
    }
    if reconstructed.len() as u64 != stream.compressed_bytes
        || hex::encode(Sha256::digest(&reconstructed)) != stream.compressed_sha256
    {
        return Err(AtlasError::Other(
            "capacity log reconstructed digest or size mismatch".into(),
        ));
    }
    Ok(reconstructed)
}

fn gzip_capacity_log(bytes: &[u8]) -> Result<Vec<u8>> {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(python)
        .args([
            "-B",
            "-c",
            "import gzip,shutil,sys; output=gzip.GzipFile(fileobj=sys.stdout.buffer,mode='wb',compresslevel=1,mtime=0); shutil.copyfileobj(sys.stdin.buffer,output); output.close()",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| AtlasError::Other("capacity log compressor stdin is unavailable".into()))?;
    let (write_result, output_result) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let result = input.write_all(bytes);
            drop(input);
            result
        });
        let output = child.wait_with_output();
        let write = writer.join().unwrap_or_else(|_| {
            Err(std::io::Error::other(
                "capacity log compressor writer panicked",
            ))
        });
        (write, output)
    });
    write_result?;
    let output = output_result?;
    if !output.status.success() {
        return Err(AtlasError::Other(
            "capacity log deterministic gzip compression failed".into(),
        ));
    }
    if output.stdout.is_empty() || output.stdout.len() as u64 > CAPACITY_LOG_MAX_BYTES {
        return Err(AtlasError::Other(format!(
            "capacity log compressed stream exceeds the {} byte project bound",
            CAPACITY_LOG_MAX_BYTES
        )));
    }
    Ok(output.stdout)
}

fn redact_capacity_log_text(
    text: &str,
    repository_root: &Path,
    evidence_directory: &Path,
) -> String {
    let resolved_evidence = resolve_capacity_directory(repository_root, evidence_directory);
    let temporary = std::env::temp_dir();
    let mut redacted = text.to_string();
    for (path, marker) in [
        (repository_root, "<repo>"),
        (resolved_evidence.as_path(), "<evidence>"),
        (temporary.as_path(), "<temp>"),
    ] {
        let raw = path.to_string_lossy();
        if !raw.is_empty() {
            redacted = redacted.replace(raw.as_ref(), marker);
            redacted = redacted.replace(&raw.replace('\\', "/"), marker);
        }
    }
    redacted
}

fn redact_capacity_log_value(
    value: &mut Value,
    repository_root: &Path,
    evidence_directory: &Path,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let normalized = key.to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "prompt"
                        | "source_body"
                        | "source_text"
                        | "credential"
                        | "credentials"
                        | "secret"
                        | "secrets"
                        | "environment"
                        | "environment_values"
                ) {
                    *child = Value::String("<redacted-private-field>".to_string());
                    continue;
                }
                if normalized == "validation_failures" {
                    let failures = child.as_array_mut().ok_or_else(|| {
                        AtlasError::Other(
                            "capacity log validation failures field must be an array".into(),
                        )
                    })?;
                    for failure in failures {
                        *failure = Value::String("<redacted-diagnostic>".to_string());
                    }
                } else if matches!(normalized.as_str(), "preparation_failure" | "detail")
                    && !child.is_null()
                {
                    *child = Value::String("<redacted-diagnostic>".to_string());
                } else {
                    redact_capacity_log_value(child, repository_root, evidence_directory)?;
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                redact_capacity_log_value(child, repository_root, evidence_directory)?;
            }
        }
        Value::String(text) => {
            *text = redact_capacity_log_text(text, repository_root, evidence_directory);
            if Path::new(text).is_absolute() {
                *text = "<redacted-absolute-path>".to_string();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn validate_capacity_raw_record(
    value: &Value,
    scenario: &BenchmarkScenario,
    observed_strata: &mut BTreeSet<String>,
) -> Result<CapacityObservationKey> {
    let object = value
        .as_object()
        .ok_or_else(|| AtlasError::Other("capacity raw log record must be an object".into()))?;
    let observed_fields = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected_fields = CAPACITY_RAW_FIELDS.iter().copied().collect::<BTreeSet<_>>();
    if observed_fields != expected_fields
        || value["schema_version"] != "1.0.0"
        || value["kind"] != PRODUCTION_CAPACITY_EVIDENCE.raw_kind
    {
        return Err(AtlasError::Other(
            "capacity raw log record schema is inconsistent".into(),
        ));
    }
    let dataset = value["dataset_name"]
        .as_str()
        .ok_or_else(|| AtlasError::Other("capacity raw log dataset is missing".into()))?;
    if !scenario
        .capacity
        .strata
        .iter()
        .any(|item| item.name == dataset)
    {
        return Err(AtlasError::Other(
            "capacity raw log contains an unapproved stratum".into(),
        ));
    }
    let outcome = value["outcome"]
        .as_str()
        .ok_or_else(|| AtlasError::Other("capacity raw log outcome is missing".into()))?;
    if !scenario
        .capacity
        .outcome_states
        .iter()
        .any(|item| item == outcome)
    {
        return Err(AtlasError::Other(
            "capacity raw log outcome is outside the closed state set".into(),
        ));
    }
    let phase_name = value["phase"]
        .as_str()
        .ok_or_else(|| AtlasError::Other("capacity raw log phase is missing".into()))?;
    let phase = CapacityPhase::parse(phase_name)?;
    let descriptor = phase.descriptor();
    let compiler_fields = [
        "compiler_selected_records",
        "compiler_selected_source_bytes",
        "compiler_selected_estimated_tokens",
        "compiler_omitted_records",
        "compiler_truncated",
        "compiler_work_units_consumed",
        "compiler_fallback",
    ];
    if outcome == "success" {
        if let Some(expected_fallback) = descriptor.compiler_fallback {
            let selected_records = value["compiler_selected_records"].as_u64();
            let selected_source_bytes = value["compiler_selected_source_bytes"].as_u64();
            let selected_estimated_tokens = value["compiler_selected_estimated_tokens"].as_u64();
            let omitted_records = value["compiler_omitted_records"].as_u64();
            let truncated = value["compiler_truncated"].as_bool();
            let work_units = value["compiler_work_units_consumed"].as_u64();
            let fallback = value["compiler_fallback"].as_bool();
            let maximum_records = value["profile_inputs"]["records"].as_u64();
            let maximum_source_bytes = value["profile_inputs"]["source_bytes"].as_u64();
            let maximum_estimated_tokens = value["profile_inputs"]["estimated_tokens"].as_u64();
            let fields_complete = [
                selected_records,
                selected_source_bytes,
                selected_estimated_tokens,
                omitted_records,
                work_units,
            ]
            .iter()
            .all(Option::is_some)
                && truncated.is_some()
                && fallback == Some(expected_fallback);
            let values_consistent = selected_records
                .zip(maximum_records)
                .is_some_and(|(selected, maximum)| selected <= maximum)
                && selected_source_bytes
                    .zip(maximum_source_bytes)
                    .is_some_and(|(selected, maximum)| selected <= maximum)
                && selected_estimated_tokens
                    .zip(maximum_estimated_tokens)
                    .is_some_and(|(selected, maximum)| selected <= maximum)
                && truncated == omitted_records.map(|omitted| omitted != 0);
            let candidates_considered = selected_records
                .zip(omitted_records)
                .and_then(|(selected, omitted)| selected.checked_add(omitted));
            let work_reconciles = if phase == CapacityPhase::ProgressiveDeepRouteCompiler {
                value["deterministic_work_units"].as_u64()
                    == work_units.and_then(|compiler| compiler.checked_add(1))
            } else {
                work_units == candidates_considered
                    && value["deterministic_work_units"].as_u64() == work_units
            };
            if !fields_complete || !values_consistent || !work_reconciles {
                return Err(AtlasError::Other(
                    "capacity raw log compiler measurements are incomplete, aliased, or inconsistent"
                        .into(),
                ));
            }
        } else if compiler_fields.iter().any(|field| !value[*field].is_null()) {
            return Err(AtlasError::Other(
                "capacity raw log non-compiler phase contains compiler measurements".into(),
            ));
        }
    }
    observed_strata.insert(dataset.to_string());
    Ok(CapacityObservationKey {
        dataset_name: dataset.to_string(),
        profile: value["profile"]
            .as_str()
            .ok_or_else(|| AtlasError::Other("capacity raw log profile is missing".into()))?
            .to_string(),
        provider_condition: value["provider_condition"]
            .as_str()
            .ok_or_else(|| AtlasError::Other("capacity raw log provider is missing".into()))?
            .to_string(),
        phase: phase_name.to_string(),
        repetition: value["repetition"]
            .as_u64()
            .ok_or_else(|| AtlasError::Other("capacity raw log repetition is missing".into()))?,
        sample_class: value["sample_class"]
            .as_str()
            .ok_or_else(|| AtlasError::Other("capacity raw log sample class is missing".into()))?
            .to_string(),
        sample_index: value["sample_index"]
            .as_u64()
            .ok_or_else(|| AtlasError::Other("capacity raw log sample index is missing".into()))?,
    })
}

struct CapacityLogSourceSpec<'a> {
    path: &'a Path,
    file: Option<File>,
    aggregate_digest: Option<&'a mut Sha256>,
    name: &'a str,
    maximum_bytes: u64,
    expected_kind: &'a str,
    ndjson: bool,
}

fn prepare_capacity_log_stream(
    source: CapacityLogSourceSpec<'_>,
    repository_root: &Path,
    evidence_directory: &Path,
    scenario: &BenchmarkScenario,
    shard: Option<CapacityShard>,
    capacity_provenance_sha256: Option<&str>,
) -> Result<CapacityLogStream> {
    let CapacityLogSourceSpec {
        path,
        file,
        mut aggregate_digest,
        name,
        maximum_bytes,
        expected_kind,
        ndjson,
    } = source;
    let retained_memory_handle = file.is_some();
    let mut file = match file {
        Some(file) => file,
        None => {
            let path_metadata = std::fs::symlink_metadata(path).map_err(|error| {
                AtlasError::Other(format!("capacity log source {name} is missing: {error}"))
            })?;
            if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
                return Err(AtlasError::Other(format!(
                    "capacity log source {name} is linked or not a physical file"
                )));
            }
            File::open(path)?
        }
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(AtlasError::Other(format!(
            "capacity log source {name} is empty or exceeds its {} byte input bound",
            maximum_bytes
        )));
    }
    let source_bytes = if retained_memory_handle {
        validate_open_memory_file(&file, path, maximum_bytes)?
    } else {
        metadata.len()
    };
    file.seek(SeekFrom::Start(0))?;
    let mut source_digest = Sha256::new();
    let mut observed_source_bytes = 0_u64;
    let mut redacted = Vec::new();
    let mut record_count = 0_u64;
    let mut observed_strata = BTreeSet::new();
    let mut observed_attempts = BTreeSet::new();
    if ndjson {
        let mut reader = BufReader::new(&mut file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            if read > CAPACITY_LOG_MAX_RECORD_BYTES || !line.ends_with(b"\n") || line == b"\n" {
                return Err(AtlasError::Other(format!(
                    "capacity log source {name} has an empty, unterminated, or over-bound record"
                )));
            }
            source_digest.update(&line);
            if let Some(digest) = aggregate_digest.as_deref_mut() {
                digest.update(&line);
            }
            observed_source_bytes =
                observed_source_bytes
                    .checked_add(read as u64)
                    .ok_or_else(|| {
                        AtlasError::Other("capacity log source byte count overflow".into())
                    })?;
            line.pop();
            if line.ends_with(b"\r") {
                return Err(AtlasError::Other(format!(
                    "capacity log source {name} is not canonical LF NDJSON"
                )));
            }
            let mut value: Value = serde_json::from_slice(&line)?;
            if value["kind"] != expected_kind {
                return Err(AtlasError::Other(format!(
                    "capacity log source {name} contains the wrong record kind"
                )));
            }
            if expected_kind == PRODUCTION_CAPACITY_EVIDENCE.raw_kind {
                if value["provenance_sha256"].as_str() != capacity_provenance_sha256 {
                    return Err(AtlasError::Other(
                        "capacity raw evidence is not bound to its summary provenance".into(),
                    ));
                }
                let key = validate_capacity_raw_record(&value, scenario, &mut observed_strata)?;
                if !capacity_attempt_belongs_to_scope(&key, scenario, shard)
                    || !observed_attempts.insert(key)
                {
                    return Err(AtlasError::Other(
                        "capacity raw evidence contains an out-of-scope or duplicate attempt identity"
                            .into(),
                    ));
                }
            }
            redact_capacity_log_value(&mut value, repository_root, evidence_directory)?;
            serde_json::to_writer(&mut redacted, &value)?;
            redacted.push(b'\n');
            record_count += 1;
        }
    } else {
        let mut source = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut source)?;
        source_digest.update(&source);
        if let Some(digest) = aggregate_digest {
            digest.update(&source);
        }
        observed_source_bytes = source.len() as u64;
        let mut value: Value = serde_json::from_slice(&source)?;
        if value["kind"] != expected_kind {
            return Err(AtlasError::Other(format!(
                "capacity log source {name} contains the wrong summary kind"
            )));
        }
        redact_capacity_log_value(&mut value, repository_root, evidence_directory)?;
        serde_json::to_writer(&mut redacted, &value)?;
        redacted.push(b'\n');
        record_count = 1;
    }
    let final_bytes = if retained_memory_handle {
        validate_open_memory_file(&file, path, maximum_bytes)?
    } else {
        file.metadata()?.len()
    };
    if observed_source_bytes != source_bytes || final_bytes != source_bytes {
        return Err(AtlasError::Other(format!(
            "capacity log source {name} changed while its accepted handle was consumed"
        )));
    }
    if expected_kind == PRODUCTION_CAPACITY_EVIDENCE.raw_kind {
        let expected_strata = scenario
            .capacity
            .strata
            .iter()
            .map(|item| item.name.clone())
            .collect::<BTreeSet<_>>();
        if observed_strata != expected_strata
            || observed_attempts.len() as u64 != capacity_execution_total(shard)
        {
            return Err(AtlasError::Other(
                "capacity raw log does not contain the exact complete selected attempt set".into(),
            ));
        }
    }
    let redacted_bytes = redacted.len() as u64;
    let redacted_sha256 = hex::encode(Sha256::digest(&redacted));
    let compressed = gzip_capacity_log(&redacted)?;
    let mut stream = chunk_capacity_log_stream(
        name,
        &compressed,
        redacted_bytes,
        redacted_sha256,
        CAPACITY_LOG_CHUNK_BYTES,
    )?;
    stream.source_bytes = source_bytes;
    stream.source_sha256 = hex::encode(source_digest.finalize());
    stream.record_count = record_count;
    Ok(stream)
}

fn valid_nearest_rank_summary(value: &Value, expected_count: Option<u64>) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["count", "p50", "p95", "p99"])
    {
        return false;
    }
    let Some(count) = value["count"].as_u64() else {
        return false;
    };
    if expected_count.is_some_and(|expected| count != expected) {
        return false;
    }
    ["p50", "p95", "p99"].iter().all(|field| {
        if count == 0 {
            value[*field].is_null()
        } else {
            value[*field].as_u64().is_some()
        }
    })
}

fn validate_capacity_log_summary(
    summary: &Value,
    scenario: &BenchmarkScenario,
    shard: Option<CapacityShard>,
    global_plan_sha256: &str,
) -> Result<()> {
    let object = summary.as_object().ok_or_else(|| {
        AtlasError::Other("capacity evidence summary for logging must be an object".into())
    })?;
    let execution_total = capacity_execution_total(shard);
    let expected_fields = BTreeSet::from([
        "schema_version",
        "kind",
        "provenance",
        "provenance_sha256",
        "raw_sha256",
        "attempted_records",
        "missing_records",
        "duplicate_records",
        "maximum_raw_records",
        "outcome_counts",
        "percentile_method",
        "aggregation_input",
        "campaign_accepted",
        "validation_failure_count",
        "validation_failures",
        "compiler_measurement_fields",
        "condition_summaries",
        "limitations",
        "claim",
    ]);
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_fields
        || summary["schema_version"] != "1.0.0"
        || summary["kind"] != PRODUCTION_CAPACITY_EVIDENCE.summary_kind
        || summary["attempted_records"] != execution_total
        || summary["maximum_raw_records"] != execution_total
        || summary["missing_records"] != 0
        || summary["duplicate_records"] != 0
        || summary["compiler_measurement_fields"]
            != serde_json::json!(CAPACITY_COMPILER_MEASUREMENT_FIELDS)
        || !summary["campaign_accepted"].is_boolean()
    {
        return Err(AtlasError::Other(
            "capacity evidence summary for logging is incomplete or inconsistent".into(),
        ));
    }
    let expected_execution = capacity_execution_scope(shard, global_plan_sha256);
    let provenance_bytes = serde_json::to_vec(&summary["provenance"])?;
    let provenance_sha256 = hex::encode(Sha256::digest(provenance_bytes));
    if summary["provenance_sha256"] != provenance_sha256
        || summary["provenance"]["global_plan_sha256"] != global_plan_sha256
        || summary["provenance"]["execution_scope"] != expected_execution
    {
        return Err(AtlasError::Other(
            "capacity evidence provenance is not bound to its global plan and execution scope"
                .into(),
        ));
    }
    let outcomes = summary["outcome_counts"].as_object().ok_or_else(|| {
        AtlasError::Other("capacity evidence summary outcome counts are missing".into())
    })?;
    let mut global_outcome_counts = BTreeMap::new();
    let mut global_outcome_total = 0_u64;
    for (outcome, count) in outcomes {
        if !scenario
            .capacity
            .outcome_states
            .iter()
            .any(|item| item == outcome)
        {
            return Err(AtlasError::Other(
                "capacity evidence summary contains an unknown outcome".into(),
            ));
        }
        let count = count.as_u64().ok_or_else(|| {
            AtlasError::Other("capacity evidence summary outcome count is invalid".into())
        })?;
        global_outcome_total = global_outcome_total
            .checked_add(count)
            .ok_or_else(|| AtlasError::Other("capacity evidence outcome count overflow".into()))?;
        global_outcome_counts.insert(outcome.clone(), count);
    }
    if global_outcome_total != execution_total
        || global_outcome_counts
            .get("unsupported")
            .is_none_or(|count| *count == 0)
    {
        return Err(AtlasError::Other(
            "capacity evidence summary does not retain the closed outcomes".into(),
        ));
    }
    let scheduled_samples = scenario
        .procedure
        .warmups
        .checked_add(scenario.procedure.warm_samples)
        .and_then(|count| count.checked_add(scenario.procedure.cold_samples))
        .ok_or_else(|| AtlasError::Other("capacity scheduled sample count overflow".into()))?;
    let mut expected_condition_keys = BTreeSet::new();
    let mut global_matrix_row_index = 0_u64;
    for stratum in &scenario.capacity.strata {
        for profile in &scenario.profiles {
            for provider_condition in &scenario.capacity.provider_conditions {
                let resolved =
                    resolve_capacity_provider_condition(&stratum.name, provider_condition)?;
                for phase_name in &scenario.capacity.phases {
                    let selected = shard.is_none_or(|value| value.selects(global_matrix_row_index));
                    global_matrix_row_index += 1;
                    if !selected {
                        continue;
                    }
                    CapacityPhase::parse(phase_name)?;
                    for repetition in 1..=scenario.procedure.repetitions {
                        expected_condition_keys.insert(CapacityAggregationKey {
                            dataset_name: stratum.name.clone(),
                            profile: profile.name.clone(),
                            provider_condition: resolved.name().to_string(),
                            phase: phase_name.clone(),
                            repetition,
                        });
                    }
                }
            }
        }
    }
    let expected_condition_count = if shard.is_some() {
        CAPACITY_SHARD_MATRIX_ROWS
    } else {
        CAPACITY_GLOBAL_MATRIX_ROWS
    } * scenario.procedure.repetitions;
    if global_matrix_row_index != CAPACITY_GLOBAL_MATRIX_ROWS
        || expected_condition_keys.len() as u64 != expected_condition_count
    {
        return Err(AtlasError::Other(
            "capacity scenario does not derive the exact selected condition summaries".into(),
        ));
    }
    let condition_summaries = summary["condition_summaries"].as_array().ok_or_else(|| {
        AtlasError::Other("capacity evidence condition summaries are missing".into())
    })?;
    let mut observed_condition_keys = BTreeSet::new();
    let mut reconciled_outcome_counts = BTreeMap::<String, u64>::new();
    for condition in condition_summaries {
        let condition_object = condition.as_object().ok_or_else(|| {
            AtlasError::Other("capacity evidence condition summary must be an object".into())
        })?;
        if condition_object
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "condition",
                "outcome_counts",
                "scored_success_atlas_runtime_ms",
                "compiler_measurements",
            ])
            || !valid_nearest_rank_summary(&condition["scored_success_atlas_runtime_ms"], None)
        {
            return Err(AtlasError::Other(
                "capacity evidence condition summary schema is inconsistent".into(),
            ));
        }
        let key_object = condition["condition"].as_object().ok_or_else(|| {
            AtlasError::Other("capacity evidence condition key must be an object".into())
        })?;
        if key_object
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "dataset_name",
                "phase",
                "profile",
                "provider_condition",
                "repetition",
            ])
        {
            return Err(AtlasError::Other(
                "capacity evidence condition key is malformed".into(),
            ));
        }
        let phase_name = condition["condition"]["phase"]
            .as_str()
            .ok_or_else(|| AtlasError::Other("capacity condition phase is missing".into()))?;
        let phase = CapacityPhase::parse(phase_name)?;
        let key = CapacityAggregationKey {
            dataset_name: condition["condition"]["dataset_name"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("capacity condition dataset is missing".into()))?
                .to_string(),
            profile: condition["condition"]["profile"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("capacity condition profile is missing".into()))?
                .to_string(),
            provider_condition: condition["condition"]["provider_condition"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("capacity condition provider is missing".into()))?
                .to_string(),
            phase: phase_name.to_string(),
            repetition: condition["condition"]["repetition"]
                .as_u64()
                .ok_or_else(|| {
                    AtlasError::Other("capacity condition repetition is missing".into())
                })?,
        };
        if !observed_condition_keys.insert(key.clone()) {
            return Err(AtlasError::Other(
                "capacity evidence contains a duplicate condition summary".into(),
            ));
        }
        if !expected_condition_keys.contains(&key) {
            return Err(AtlasError::Other(
                "capacity evidence contains an unknown condition summary".into(),
            ));
        }
        let condition_outcomes = condition["outcome_counts"].as_object().ok_or_else(|| {
            AtlasError::Other("capacity condition outcome counts are missing".into())
        })?;
        let mut condition_outcome_total = 0_u64;
        for (outcome, count) in condition_outcomes {
            if !scenario
                .capacity
                .outcome_states
                .iter()
                .any(|item| item == outcome)
            {
                return Err(AtlasError::Other(
                    "capacity condition contains an unknown outcome".into(),
                ));
            }
            let count = count.as_u64().ok_or_else(|| {
                AtlasError::Other("capacity condition outcome count is invalid".into())
            })?;
            condition_outcome_total =
                condition_outcome_total.checked_add(count).ok_or_else(|| {
                    AtlasError::Other("capacity condition outcome count overflow".into())
                })?;
            let reconciled = reconciled_outcome_counts
                .entry(outcome.clone())
                .or_default();
            *reconciled = reconciled.checked_add(count).ok_or_else(|| {
                AtlasError::Other("capacity reconciled outcome count overflow".into())
            })?;
        }
        if condition_outcome_total != scheduled_samples {
            return Err(AtlasError::Other(
                "capacity condition outcome total differs from the scheduled samples".into(),
            ));
        }
        let scored_success_count = condition["scored_success_atlas_runtime_ms"]["count"]
            .as_u64()
            .expect("validated nearest-rank count");
        let compiler = &condition["compiler_measurements"];
        let compiler_applicable = phase.descriptor().compiler_fallback.is_some();
        if compiler.is_null() {
            if compiler_applicable && scored_success_count != 0 {
                return Err(AtlasError::Other(
                    "capacity compiler summary is missing for scored successes".into(),
                ));
            }
            continue;
        }
        if !compiler_applicable {
            return Err(AtlasError::Other(
                "capacity non-compiler condition contains compiler measurements".into(),
            ));
        }
        let compiler_object = compiler.as_object().ok_or_else(|| {
            AtlasError::Other("capacity compiler summary must be an object or null".into())
        })?;
        let compiler_fields = BTreeSet::from([
            "count",
            "compiler_selected_records",
            "compiler_selected_source_bytes",
            "compiler_selected_estimated_tokens",
            "compiler_omitted_records",
            "compiler_truncated",
            "compiler_work_units_consumed",
            "compiler_fallback",
        ]);
        let count = compiler["count"].as_u64().unwrap_or(0);
        let boolean_counts_valid = |field: &str| {
            let value = &compiler[field];
            value.as_object().is_some_and(|object| {
                object.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == BTreeSet::from(["false_count", "true_count"])
            }) && value["true_count"]
                .as_u64()
                .zip(value["false_count"].as_u64())
                .and_then(|(true_count, false_count)| true_count.checked_add(false_count))
                == Some(count)
        };
        if count == 0
            || count != scored_success_count
            || compiler_object
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != compiler_fields
            || [
                "compiler_selected_records",
                "compiler_selected_source_bytes",
                "compiler_selected_estimated_tokens",
                "compiler_omitted_records",
                "compiler_work_units_consumed",
            ]
            .iter()
            .any(|field| !valid_nearest_rank_summary(&compiler[*field], Some(count)))
            || !boolean_counts_valid("compiler_truncated")
            || !boolean_counts_valid("compiler_fallback")
        {
            return Err(AtlasError::Other(
                "capacity compiler summary is incomplete or inconsistent".into(),
            ));
        }
    }
    if observed_condition_keys != expected_condition_keys
        || reconciled_outcome_counts != global_outcome_counts
    {
        return Err(AtlasError::Other(
            "capacity condition summaries are incomplete or do not reconcile".into(),
        ));
    }
    let provenance = summary["provenance"].as_object().ok_or_else(|| {
        AtlasError::Other("capacity evidence provenance for logging is missing".into())
    })?;
    let revision = provenance
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AtlasError::Other("capacity evidence revision for logging is missing".into())
        })?;
    let platform = provenance
        .get("platform")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AtlasError::Other("capacity evidence platform for logging is missing".into())
        })?;
    let toolchain = provenance
        .get("resolved_local_tools")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AtlasError::Other("capacity evidence toolchain for logging is missing".into())
        })?;
    if revision.len() != 40
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        || ["os", "architecture", "cpu"].iter().any(|field| {
            platform
                .get(*field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        })
        || ["rustc", "cargo", "python", "node"].iter().any(|field| {
            toolchain
                .get(*field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        })
        || toolchain
            .get("providers")
            .is_none_or(|value| !value.is_array())
    {
        return Err(AtlasError::Other(
            "capacity evidence revision, platform, or toolchain for logging is malformed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn validate_memory_log_evidence(repository_root: &Path, summary_path: &Path) -> Result<()> {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = Command::new(python)
        .args([
            "-B",
            "-c",
            "import pathlib,runpy,sys; module=runpy.run_path(sys.argv[1]); module['validate_segmented_evidence'](pathlib.Path(sys.argv[2]))",
        ])
        .arg(repository_root.join("scripts/process-tree-memory.py"))
        .arg(summary_path)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(AtlasError::Other(
            "process-tree memory evidence failed its authoritative validation before logging"
                .into(),
        ));
    }
    Ok(())
}

fn required_runner_log_value(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .map_err(|_| AtlasError::Other(format!("capacity log runner field {name} is missing")))?;
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| character.is_control())
    {
        return Err(AtlasError::Other(format!(
            "capacity log runner field {name} is empty or over-bound"
        )));
    }
    Ok(value)
}

fn validate_capacity_log_size(log_bytes: u64) -> Result<()> {
    if log_bytes > CAPACITY_LOG_MAX_BYTES {
        return Err(AtlasError::Other(format!(
            "complete redacted capacity log requires {log_bytes} bytes, exceeding the {} byte project bound; a separate private artifact-upload decision is required",
            CAPACITY_LOG_MAX_BYTES
        )));
    }
    Ok(())
}

fn emit_capacity_log(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
    shard: Option<CapacityShard>,
) -> Result<()> {
    validate_capacity_manifests(repository_root, scenario, manifest_directory)?;
    let global_plan_sha256 = capacity_global_plan_sha256(repository_root, scenario_path, scenario)?;
    let execution_scope = capacity_execution_scope(shard, &global_plan_sha256);
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    let capacity_raw_path = evidence.path.join(PRODUCTION_CAPACITY_EVIDENCE.raw_file);
    let capacity_summary_path = evidence
        .path
        .join(PRODUCTION_CAPACITY_EVIDENCE.summary_file);
    let memory_summary_path = evidence.path.join(MEMORY_SUMMARY_FILE);
    let capacity_summary: Value =
        serde_json::from_reader(BufReader::new(File::open(&capacity_summary_path).map_err(
            |error| AtlasError::Other(format!("capacity log summary is missing: {error}")),
        )?))?;
    validate_capacity_log_summary(&capacity_summary, scenario, shard, &global_plan_sha256)?;
    let capacity_provenance_sha256 = capacity_summary["provenance_sha256"]
        .as_str()
        .ok_or_else(|| AtlasError::Other("capacity summary provenance digest is missing".into()))?
        .to_string();
    let validated_memory = validate_memory_sampler_outputs(
        &evidence.path,
        &memory_summary_path,
        &repository_root.join("scripts/process-tree-memory.py"),
        &execution_scope,
    )?;
    if validated_memory.byte_length > CAPACITY_LOG_MAX_RAW_BYTES {
        return Err(AtlasError::Other(
            "memory evidence exceeds the reviewed bounded private log source limit".into(),
        ));
    }
    let ValidatedMemorySegments {
        segments: validated_segments,
        summary: memory_summary,
        summary_file,
        summary_path: memory_summary_spool_path,
        _summary_spool_path,
        record_count: validated_memory_records,
        byte_length: validated_memory_bytes,
        sha256: validated_memory_sha256,
    } = validated_memory;
    if memory_summary["capacity_execution"] != execution_scope {
        return Err(AtlasError::Other(
            "capacity and process-memory evidence execution identities do not match".into(),
        ));
    }

    let mut streams = vec![
        prepare_capacity_log_stream(
            CapacityLogSourceSpec {
                path: &capacity_raw_path,
                file: None,
                aggregate_digest: None,
                name: PRODUCTION_CAPACITY_EVIDENCE.raw_file,
                maximum_bytes: CAPACITY_LOG_MAX_RAW_BYTES,
                expected_kind: PRODUCTION_CAPACITY_EVIDENCE.raw_kind,
                ndjson: true,
            },
            repository_root,
            &evidence.path,
            scenario,
            shard,
            Some(&capacity_provenance_sha256),
        )?,
        prepare_capacity_log_stream(
            CapacityLogSourceSpec {
                path: &capacity_summary_path,
                file: None,
                aggregate_digest: None,
                name: PRODUCTION_CAPACITY_EVIDENCE.summary_file,
                maximum_bytes: CAPACITY_LOG_MAX_SUMMARY_BYTES,
                expected_kind: PRODUCTION_CAPACITY_EVIDENCE.summary_kind,
                ndjson: false,
            },
            repository_root,
            &evidence.path,
            scenario,
            shard,
            None,
        )?,
    ];
    let memory_stream_start = streams.len();
    let mut logged_memory_digest = Sha256::new();
    for segment in validated_segments {
        let ValidatedMemorySegment {
            name,
            path,
            file,
            _spool_path,
            ..
        } = segment;
        streams.push(prepare_capacity_log_stream(
            CapacityLogSourceSpec {
                path: &path,
                file: Some(file),
                aggregate_digest: Some(&mut logged_memory_digest),
                name: &name,
                maximum_bytes: MEMORY_MAX_RAW_BYTES,
                expected_kind: "process-tree-memory-sample",
                ndjson: true,
            },
            repository_root,
            &evidence.path,
            scenario,
            shard,
            None,
        )?);
    }
    let memory_stream_end = streams.len();
    streams.push(prepare_capacity_log_stream(
        CapacityLogSourceSpec {
            path: &memory_summary_spool_path,
            file: Some(summary_file),
            aggregate_digest: None,
            name: MEMORY_SUMMARY_FILE,
            maximum_bytes: MEMORY_MAX_SUMMARY_BYTES,
            expected_kind: "process-tree-memory-segmented-summary",
            ndjson: false,
        },
        repository_root,
        &evidence.path,
        scenario,
        shard,
        None,
    )?);
    if capacity_summary["raw_sha256"].as_str() != Some(streams[0].source_sha256.as_str())
        || capacity_summary["attempted_records"].as_u64() != Some(streams[0].record_count)
    {
        return Err(AtlasError::Other(
            "capacity raw evidence digest or count changed before logging".into(),
        ));
    }
    let (logged_memory_records, logged_memory_bytes) = streams
        [memory_stream_start..memory_stream_end]
        .iter()
        .try_fold((0_u64, 0_u64), |(records, bytes), stream| {
            Ok::<_, AtlasError>((
                records
                    .checked_add(stream.record_count)
                    .ok_or_else(|| AtlasError::Other("memory log record count overflow".into()))?,
                bytes
                    .checked_add(stream.source_bytes)
                    .ok_or_else(|| AtlasError::Other("memory log byte count overflow".into()))?,
            ))
        })?;
    let logged_memory_sha256 = hex::encode(logged_memory_digest.finalize());
    if logged_memory_records != validated_memory_records
        || logged_memory_bytes != validated_memory_bytes
        || logged_memory_sha256 != validated_memory_sha256
        || memory_summary["raw_sha256"] != logged_memory_sha256
        || memory_summary["raw_byte_length"] != logged_memory_bytes
        || memory_summary["raw_record_count"] != logged_memory_records
        || memory_summary["capacity_execution"] != execution_scope
    {
        return Err(AtlasError::Other(
            "memory bytes consumed for logging changed from the validated digest, length, record count, or execution"
                .into(),
        ));
    }
    let mut allowed_memory_artifacts = BTreeSet::from([MEMORY_SUMMARY_FILE.to_string()]);
    for descriptor in memory_summary["segments"].as_array().ok_or_else(|| {
        AtlasError::Other("process-tree memory segment manifest disappeared before logging".into())
    })? {
        let name = descriptor["path"].as_str().ok_or_else(|| {
            AtlasError::Other("process-tree memory segment name disappeared before logging".into())
        })?;
        allowed_memory_artifacts.insert(name.to_string());
    }
    validate_memory_artifact_namespace(&evidence.path, &allowed_memory_artifacts)?;
    validate_capacity_directory_guard(&evidence.path, evidence.guard())?;

    let runner = serde_json::json!({
        "name": required_runner_log_value("ATLAS_RUNNER_NAME")?,
        "os": required_runner_log_value("ATLAS_RUNNER_OS")?,
        "architecture": required_runner_log_value("ATLAS_RUNNER_ARCH")?,
        "image_os": required_runner_log_value("ImageOS")?,
        "image_version": required_runner_log_value("ImageVersion")?,
    });
    let mut toolchain = capacity_summary["provenance"]["resolved_local_tools"].clone();
    redact_capacity_log_value(&mut toolchain, repository_root, &evidence.path)?;
    let mut observed_platform = capacity_summary["provenance"]["platform"].clone();
    redact_capacity_log_value(&mut observed_platform, repository_root, &evidence.path)?;
    let revision = capacity_summary["provenance"]["revision"].clone();
    let total_chunks = streams.iter().try_fold(0_u64, |total, stream| {
        total
            .checked_add(stream.chunks.len() as u64)
            .ok_or_else(|| AtlasError::Other("capacity log total chunk count overflow".into()))
    })?;
    let encoded_payload_bytes = streams.iter().try_fold(0_u64, |total, stream| {
        let bytes = stream
            .chunks
            .iter()
            .map(|chunk| chunk.data.len() as u64)
            .sum::<u64>();
        total
            .checked_add(bytes)
            .ok_or_else(|| AtlasError::Other("capacity log encoded byte count overflow".into()))
    })?;
    let compressed_payload_bytes = streams.iter().try_fold(0_u64, |total, stream| {
        total
            .checked_add(stream.compressed_bytes)
            .ok_or_else(|| AtlasError::Other("capacity log compressed byte count overflow".into()))
    })?;
    let mut reconstructed_digest = Sha256::new();
    for stream in &streams {
        reconstructed_digest.update(stream.name.as_bytes());
        reconstructed_digest.update([0]);
        reconstructed_digest.update(stream.compressed_bytes.to_le_bytes());
        for chunk in &stream.chunks {
            reconstructed_digest.update(base64_decode(&chunk.data)?);
        }
    }
    let reconstructed_sha256 = hex::encode(reconstructed_digest.finalize());
    let header = serde_json::json!({
        "schema_version": "1.0.0",
        "kind": "capacity-evidence-log-header",
        "runner": runner,
        "toolchain": toolchain,
        "observed_platform": observed_platform,
        "revision": revision,
        "global_plan_sha256": global_plan_sha256,
        "execution_scope": execution_scope,
        "stream_count": streams.len(),
        "total_chunks": total_chunks,
        "encoded_payload_bytes": encoded_payload_bytes,
        "compressed_payload_bytes": compressed_payload_bytes,
        "whole_reconstructed_sha256": reconstructed_sha256,
        "maximum_log_bytes": CAPACITY_LOG_MAX_BYTES,
        "chunk_decoded_bytes_bound": CAPACITY_LOG_CHUNK_BYTES,
        "compression": "gzip-1-mtime-0",
        "encoding": "base64",
        "redaction": "closed schemas; private absolute paths replaced; diagnostics replaced before compression",
        "claim": "complete bounded redacted execution-local private evidence; global acceptance of sharded evidence requires the exact four-shard union for each required OS; no supported-limit, SLO, platform, or release claim",
    });
    let mut lines = vec![format!(
        "{CAPACITY_LOG_PREFIX}{}",
        serde_json::to_string(&header)?
    )];
    for stream in &streams {
        let manifest = serde_json::json!({
            "schema_version": "1.0.0",
            "kind": "capacity-evidence-stream",
            "global_plan_sha256": global_plan_sha256,
            "execution_scope": execution_scope,
            "name": stream.name,
            "source_bytes": stream.source_bytes,
            "source_sha256": stream.source_sha256,
            "redacted_bytes": stream.redacted_bytes,
            "redacted_sha256": stream.redacted_sha256,
            "compressed_bytes": stream.compressed_bytes,
            "compressed_sha256": stream.compressed_sha256,
            "record_count": stream.record_count,
            "chunk_count": stream.chunks.len(),
            "chunk_decoded_bytes_bound": stream.chunk_bytes_bound,
        });
        lines.push(format!(
            "{CAPACITY_LOG_PREFIX}{}",
            serde_json::to_string(&manifest)?
        ));
        for chunk in &stream.chunks {
            lines.push(format!(
                "{CAPACITY_LOG_PREFIX}{}",
                serde_json::to_string(chunk)?
            ));
        }
    }
    let footer = serde_json::json!({
        "schema_version": "1.0.0",
        "kind": "capacity-evidence-log-complete",
        "global_plan_sha256": global_plan_sha256,
        "execution_scope": execution_scope,
        "stream_count": streams.len(),
        "total_chunks": total_chunks,
        "encoded_payload_bytes": encoded_payload_bytes,
        "compressed_payload_bytes": compressed_payload_bytes,
        "whole_reconstructed_sha256": reconstructed_sha256,
    });
    lines.push(format!(
        "{CAPACITY_LOG_PREFIX}{}",
        serde_json::to_string(&footer)?
    ));
    let log_bytes = lines.iter().try_fold(0_u64, |total, line| {
        total
            .checked_add(line.len() as u64 + 1)
            .ok_or_else(|| AtlasError::Other("capacity log emitted byte count overflow".into()))
    })?;
    validate_capacity_log_size(log_bytes)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

#[derive(PartialEq, Serialize)]
struct CapacityMemoryLabels<'a> {
    phase: &'a str,
    filesystem_cache_state: &'a str,
    serving_state: &'a str,
    provider_cache_install_state: String,
    build_state: &'static str,
    catalogue_state: &'a str,
    progress_state: &'static str,
    progress_ordinal: u64,
    progress_total: u64,
}

impl<'a> CapacityMemoryLabels<'a> {
    fn for_attempt(
        descriptor: &'a CapacityPhaseDescriptor,
        provider_condition: &ResolvedCapacityProviderCondition,
        sample_class: &str,
        ordinal: u64,
        total: u64,
    ) -> Result<Self> {
        if !valid_capacity_progress_total(total) || !(1..=total).contains(&ordinal) {
            return Err(AtlasError::Other(
                "capacity progress ordinal exceeds its closed execution bound".into(),
            ));
        }
        let provider_state = match provider_condition.outcome_policy {
            CapacityProviderOutcomePolicy::NotApplicable => "not-applicable",
            CapacityProviderOutcomePolicy::Required => "required-version-probed",
            CapacityProviderOutcomePolicy::Optional => "optional-version-probed",
            CapacityProviderOutcomePolicy::RequiredWithOptional => {
                "required-and-optional-versions-probed"
            }
        };
        Ok(Self {
            phase: descriptor.name,
            filesystem_cache_state: descriptor.cache_state(sample_class)?,
            serving_state: descriptor.serving_state,
            provider_cache_install_state: format!("{}:{provider_state}", provider_condition.name()),
            build_state: "prebuilt-atlas-bench",
            catalogue_state: descriptor.catalogue_lifecycle,
            progress_state: "active",
            progress_ordinal: ordinal,
            progress_total: total,
        })
    }

    fn startup(total: u64) -> Self {
        Self {
            phase: "harness-startup",
            filesystem_cache_state: "uncontrolled",
            serving_state: "not-applicable",
            provider_cache_install_state: "version-probe-pending".to_string(),
            build_state: "prebuilt-atlas-bench",
            catalogue_state: "not-applicable",
            progress_state: "starting",
            progress_ordinal: 0,
            progress_total: total,
        }
    }

    fn completed(
        descriptor: &'a CapacityPhaseDescriptor,
        provider_condition: &ResolvedCapacityProviderCondition,
        sample_class: &str,
        total: u64,
    ) -> Result<Self> {
        let mut labels =
            Self::for_attempt(descriptor, provider_condition, sample_class, total, total)?;
        labels.progress_state = "complete";
        Ok(labels)
    }

    fn closed_identity(&self) -> [&str; 6] {
        [
            self.phase,
            self.filesystem_cache_state,
            self.serving_state,
            &self.provider_cache_install_state,
            self.build_state,
            self.catalogue_state,
        ]
    }
}

#[derive(Default)]
struct CapacityMemoryPublicationPolicy {
    last_closed_labels: Option<[String; 6]>,
    last_observed_ordinal: u64,
    last_published_ordinal: u64,
    progress_total: Option<u64>,
}

impl CapacityMemoryPublicationPolicy {
    fn publication_required(&mut self, labels: &CapacityMemoryLabels<'_>) -> Result<bool> {
        let valid_progress = valid_capacity_progress_total(labels.progress_total)
            && self
                .progress_total
                .is_none_or(|total| total == labels.progress_total)
            && (1..=labels.progress_total).contains(&labels.progress_ordinal)
            && matches!(labels.progress_state, "active" | "complete")
            && (labels.progress_state != "complete"
                || labels.progress_ordinal == labels.progress_total);
        if !valid_progress || labels.progress_ordinal < self.last_observed_ordinal {
            return Err(AtlasError::Other(
                "capacity progress state regressed or exceeded its closed bound".into(),
            ));
        }
        self.progress_total = Some(labels.progress_total);
        let closed_labels_changed = self
            .last_closed_labels
            .as_ref()
            .is_none_or(|last| last.iter().map(String::as_str).ne(labels.closed_identity()));
        let interval_due = labels
            .progress_ordinal
            .is_multiple_of(CAPACITY_PROGRESS_INTERVAL)
            && labels.progress_ordinal != self.last_published_ordinal;
        self.last_observed_ordinal = labels.progress_ordinal;
        Ok(closed_labels_changed || interval_due || labels.progress_state == "complete")
    }

    fn record_publication(&mut self, labels: &CapacityMemoryLabels<'_>) {
        self.last_closed_labels = Some(labels.closed_identity().map(str::to_owned));
        self.last_published_ordinal = labels.progress_ordinal;
    }
}

struct CapacityMemoryLabelPublisher {
    path: PathBuf,
    temporary_path: PathBuf,
    policy: CapacityMemoryPublicationPolicy,
}

impl CapacityMemoryLabelPublisher {
    fn open(evidence_directory: &Path, path: &Path) -> Result<Self> {
        let expected = evidence_directory.join(MEMORY_LABEL_STATE_FILE);
        if path != expected {
            return Err(AtlasError::InvalidConfig(format!(
                "memory label state must be exactly {}",
                expected.display()
            )));
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(AtlasError::InvalidConfig(
                "memory label state must be a physical regular file".into(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            temporary_path: evidence_directory.join(".capacity-memory-label-state-v1.tmp"),
            policy: CapacityMemoryPublicationPolicy::default(),
        })
    }

    fn publish(&mut self, labels: &CapacityMemoryLabels<'_>) -> Result<()> {
        if !self.policy.publication_required(labels)? {
            return Ok(());
        }

        let bytes = serde_json::to_vec(labels)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut temporary = options.open(&self.temporary_path)?;
        temporary.write_all(&bytes)?;
        temporary.sync_all()?;
        drop(temporary);
        if let Err(error) = replace_memory_label_state(&self.path, &self.temporary_path) {
            let _ = std::fs::remove_file(&self.temporary_path);
            return Err(error);
        }
        self.policy.record_publication(labels);
        Ok(())
    }
}

#[cfg(windows)]
fn replace_memory_label_state(destination: &Path, replacement: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    retry_windows_replace(|| {
        // SAFETY: both UTF-16 paths are NUL-terminated and live for this call.
        if unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    Ok(())
}

#[cfg(windows)]
fn retry_windows_replace(mut replace: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    const ERROR_UNABLE_TO_REMOVE_REPLACED: i32 = 1175;
    const RETRIES: usize = 64;
    for retry in 0..=RETRIES {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error)
                if error.raw_os_error() == Some(ERROR_UNABLE_TO_REMOVE_REPLACED)
                    && retry < RETRIES =>
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded replacement loop always returns")
}

#[cfg(not(windows))]
fn replace_memory_label_state(destination: &Path, replacement: &Path) -> Result<()> {
    std::fs::rename(replacement, destination)?;
    Ok(())
}

struct CapacityPreparationSpec<'a> {
    repository_root: &'a Path,
    scenario: &'a BenchmarkScenario,
    manifest: &'a Value,
    provider_condition: &'a ResolvedCapacityProviderCondition,
    phase: CapacityPhase,
    lifecycle_id: &'a str,
}

struct CapacityPreparationFailure {
    outcome: &'static str,
    detail: String,
}

struct ReadyCapacityPreparation<P> {
    prepared: P,
    configuration_hash: Option<String>,
}

enum CapacityPreparation<P> {
    Ready(ReadyCapacityPreparation<P>),
    Failed(CapacityPreparationFailure),
}

impl<P> CapacityPreparation<P> {
    fn configuration_hash(&self) -> Option<&str> {
        match self {
            Self::Ready(ready) => ready.configuration_hash.as_deref(),
            Self::Failed(_) => None,
        }
    }
}

fn failed_capacity_preparation(error: AtlasError) -> CapacityPreparationFailure {
    let detail = error.to_string();
    let normalized = detail.to_ascii_lowercase();
    let outcome = if normalized.contains("timed out") || normalized.contains("timeout") {
        "timeout"
    } else if normalized.contains("unavailable")
        || normalized.contains("not found")
        || normalized.contains("cannot find")
    {
        "absent"
    } else {
        "failed"
    };
    CapacityPreparationFailure { outcome, detail }
}

trait CapacityExecutor {
    type Prepared;
    const EVIDENCE_NAMES: CapacityEvidenceNames;

    fn prepare(&mut self, spec: CapacityPreparationSpec<'_>)
        -> CapacityPreparation<Self::Prepared>;

    fn execute(
        &mut self,
        prepared: &mut Self::Prepared,
        observation: CapacityRawObservation,
    ) -> CapacityRawObservation;
}

fn execute_capacity_preparation<E: CapacityExecutor>(
    executor: &mut E,
    prepared: &mut CapacityPreparation<E::Prepared>,
    mut observation: CapacityRawObservation,
) -> CapacityRawObservation {
    match prepared {
        CapacityPreparation::Ready(ready) => executor.execute(&mut ready.prepared, observation),
        CapacityPreparation::Failed(failure) => {
            observation.outcome = failure.outcome.to_string();
            observation.correctness = Some(false);
            observation.preparation_failure = Some(failure.detail.clone());
            observation
        }
    }
}

struct CapacityAttemptSpec<'a> {
    evidence_kind: &'static str,
    provider_condition: &'a str,
    provider_members: Vec<String>,
    expected_configuration_hash: Option<String>,
    phase: &'a str,
    repetition: u64,
    sample_class: &'a str,
    sample_index: u64,
    lifecycle_id: String,
}

fn empty_capacity_observation(
    provenance_sha256: &str,
    manifest: &Value,
    accepted_outcome: Option<&CapacityAcceptedOutcome>,
    profile: &ProfileScenario,
    attempt: CapacityAttemptSpec<'_>,
) -> Result<CapacityRawObservation> {
    Ok(CapacityRawObservation {
        schema_version: "1.0.0",
        kind: attempt.evidence_kind,
        provenance_sha256: provenance_sha256.to_string(),
        dataset_name: manifest["dataset_name"]
            .as_str()
            .ok_or_else(|| AtlasError::InvalidConfig("manifest dataset_name absent".into()))?
            .to_string(),
        manifest_sha256: manifest["manifest_sha256"]
            .as_str()
            .ok_or_else(|| AtlasError::InvalidConfig("manifest digest absent".into()))?
            .to_string(),
        target_identity: accepted_outcome
            .map(|outcome| outcome.target_identity.clone())
            .unwrap_or_else(|| manifest["target_identity"].clone()),
        expected_eligible_files: accepted_outcome
            .map(|outcome| outcome.expected_eligible_files)
            .or_else(|| manifest["counts"]["eligible_files"].as_u64())
            .ok_or_else(|| AtlasError::InvalidConfig("manifest eligible count absent".into()))?,
        expected_configuration_hash: attempt.expected_configuration_hash,
        expected_indexed_files: accepted_outcome
            .map(|outcome| outcome.expected_indexed_files)
            .or_else(|| manifest["counts"]["indexed_files"].as_u64())
            .ok_or_else(|| AtlasError::InvalidConfig("manifest indexed count absent".into()))?,
        expected_changed_files: manifest["counts"]["changed_files"]
            .as_u64()
            .ok_or_else(|| AtlasError::InvalidConfig("manifest changed count absent".into()))?,
        expected_changed_bytes: manifest["counts"]["changed_bytes"]
            .as_u64()
            .ok_or_else(|| AtlasError::InvalidConfig("manifest changed bytes absent".into()))?,
        expected_source_bytes: accepted_outcome
            .map(|outcome| outcome.expected_source_bytes)
            .or_else(|| manifest["counts"]["total_bytes"].as_u64())
            .ok_or_else(|| AtlasError::InvalidConfig("manifest total bytes absent".into()))?,
        expected_accepted_outcome_hash: accepted_outcome
            .map(|outcome| outcome.expected_accepted_outcome_hash.clone())
            .or_else(|| {
                manifest["expected_accepted_outcome_hash"]
                    .as_str()
                    .map(str::to_owned)
            })
            .ok_or_else(|| AtlasError::InvalidConfig("accepted outcome hash absent".into()))?,
        expected_ready_versus_truth_result_hash: accepted_outcome
            .map(|outcome| outcome.expected_ready_versus_truth_result_hash.clone())
            .or_else(|| {
                manifest["expected_ready_versus_truth_result_hash"]
                    .as_str()
                    .map(str::to_owned)
            })
            .ok_or_else(|| AtlasError::InvalidConfig("Truth/Serving hash absent".into()))?,
        phase: attempt.phase.to_string(),
        provider_condition: attempt.provider_condition.to_string(),
        provider_members: attempt.provider_members,
        profile: profile.name.clone(),
        profile_inputs: serde_json::to_value(profile)?,
        repetition: attempt.repetition,
        sample_class: attempt.sample_class.to_string(),
        sample_index: attempt.sample_index,
        lifecycle_id: attempt.lifecycle_id,
        outcome: "failed".to_string(),
        preparation_failure: None,
        correctness: None,
        deterministic_identity: None,
        observed_target_identity: None,
        observed_evidence_counts: None,
        accepted_outcome: None,
        ready_versus_truth: None,
        deterministic_work_units: None,
        eligible_files: None,
        indexed_files: None,
        parsed_files: None,
        changed_files: None,
        changed_bytes: None,
        source_bytes: None,
        compiler_selected_records: None,
        compiler_selected_source_bytes: None,
        compiler_selected_estimated_tokens: None,
        compiler_omitted_records: None,
        compiler_truncated: None,
        compiler_work_units_consumed: None,
        compiler_fallback: None,
        provider_probes: None,
        provider_executions_run: None,
        provider_outputs: None,
        provider_output_bytes: None,
        provider_executions_reused: None,
        required_provider_failure: None,
        configuration_hash: None,
        catalogue_rows_before: None,
        catalogue_rows_after: None,
        catalogue_bytes_before: None,
        catalogue_bytes_after: None,
        durable_catalogue_rows_before: None,
        durable_catalogue_rows_after: None,
        durable_catalogue_bytes_before: None,
        durable_catalogue_bytes_after: None,
        derived_catalogue_rows_before: None,
        derived_catalogue_rows_after: None,
        derived_catalogue_bytes_before: None,
        derived_catalogue_bytes_after: None,
        provider_install_duration_ms: None,
        provider_download_duration_ms: None,
        atlas_runtime_duration_ms: None,
        wall_duration_ms: None,
        cache_state: CapacityPhase::parse(attempt.phase)?
            .descriptor()
            .cache_state(attempt.sample_class)?
            .to_string(),
        retry_count: 0,
        route_attempts: CapacityPhase::parse(attempt.phase)?
            .descriptor()
            .route_attempts,
        cancellation: false,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CapacityDeterminismKey {
    dataset_name: String,
    profile: String,
    provider_condition: String,
    phase: String,
}

fn capacity_lifecycle_id(
    dataset_name: &str,
    profile: &str,
    provider_condition: &str,
    phase: &str,
    repetition: u64,
    class: &str,
    sample_index: u64,
) -> String {
    identity_hash(&[
        dataset_name,
        profile,
        provider_condition,
        phase,
        &repetition.to_string(),
        class,
        &sample_index.to_string(),
    ])
}

fn capacity_observation_failure(
    observation: &CapacityRawObservation,
    deterministic_identities: &mut BTreeMap<CapacityDeterminismKey, String>,
) -> Option<String> {
    let phase = match CapacityPhase::parse(&observation.phase) {
        Ok(phase) => phase,
        Err(error) => return Some(error.to_string()),
    };
    let provider = match resolve_capacity_provider_condition(
        &observation.dataset_name,
        &observation.provider_condition,
    ) {
        Ok(provider) => provider,
        Err(error) => return Some(error.to_string()),
    };
    if ![
        "success",
        "timeout",
        "absent",
        "unsupported",
        "degraded",
        "cancelled",
        "failed",
    ]
    .contains(&observation.outcome.as_str())
    {
        return Some(format!(
            "observation uses an unknown outcome for {:?}",
            observation.key()
        ));
    }
    if !provider.matches_members(&observation.provider_members) {
        return Some(format!(
            "observation provider membership differs from its typed condition for {:?}",
            observation.key()
        ));
    }
    let descriptor = phase.descriptor();
    let expected_lifecycle = match descriptor.lifecycle_id(
        &observation.dataset_name,
        &observation.profile,
        provider.name(),
        observation.repetition,
        &observation.sample_class,
        observation.sample_index,
    ) {
        Ok(identity) => identity,
        Err(error) => return Some(error.to_string()),
    };
    if observation.lifecycle_id != expected_lifecycle {
        return Some(format!(
            "observation lifecycle identity differs from its schedule key for {:?}",
            observation.key()
        ));
    }
    if observation.route_attempts != descriptor.route_attempts
        || descriptor.cache_state(&observation.sample_class).ok()
            != Some(observation.cache_state.as_str())
    {
        return Some(format!(
            "observation route/cache metadata differs from its phase descriptor for {:?}",
            observation.key()
        ));
    }
    if observation.outcome != "success" {
        return (!provider
            .allows_non_success(&observation.outcome, observation.required_provider_failure))
        .then(|| {
            format!(
                "required condition retained non-success {}{} for {:?}",
                observation.outcome,
                observation
                    .preparation_failure
                    .as_deref()
                    .map(|detail| format!(" after preparation failure: {detail}"))
                    .unwrap_or_default(),
                observation.key()
            )
        });
    }
    if !provider.is_applicable() {
        return Some(format!(
            "not-applicable provider condition reported success for {:?}",
            observation.key()
        ));
    }
    if observation.preparation_failure.is_some()
        || observation.correctness != Some(true)
        || observation.deterministic_work_units.is_none()
        || observation
            .deterministic_identity
            .as_deref()
            .is_none_or(str::is_empty)
        || observation.atlas_runtime_duration_ms.is_none()
        || observation.wall_duration_ms.is_none()
        || observation.provider_install_duration_ms.is_some()
        || observation.provider_download_duration_ms.is_some()
        || observation.retry_count != 0
        || observation.cancellation
    {
        return Some(format!(
            "successful observation lacks required identity/runtime fields or violates explicit null policy for {:?}",
            observation.key()
        ));
    }
    if descriptor.provider_measurements {
        let complete = observation.provider_probes.is_some()
            && observation.provider_executions_run.is_some()
            && observation.provider_outputs.is_some()
            && observation.provider_output_bytes.is_some()
            && observation.provider_executions_reused.is_some()
            && observation.required_provider_failure == Some(false);
        let reconciled = observation.provider_probes
            == observation
                .provider_executions_run
                .zip(observation.provider_executions_reused)
                .map(|(run, reused)| run.saturating_add(reused));
        if !complete || !reconciled {
            return Some(format!(
                "provider measurements are incomplete or do not reconcile for {:?}",
                observation.key()
            ));
        }
    } else if observation.provider_probes.is_some()
        || observation.provider_executions_run.is_some()
        || observation.provider_outputs.is_some()
        || observation.provider_output_bytes.is_some()
        || observation.provider_executions_reused.is_some()
        || observation.required_provider_failure.is_some()
    {
        return Some(format!(
            "non-provider phase fabricated provider measurements for {:?}",
            observation.key()
        ));
    }
    if descriptor.catalogue_measurements {
        let Some(expected_configuration_hash) = observation.expected_configuration_hash.as_deref()
        else {
            return Some(format!(
                "catalogue observation lacks typed expected configuration identity for {:?}",
                observation.key()
            ));
        };
        if observation.observed_target_identity.is_none()
            || observation.observed_evidence_counts.is_none()
            || observation.accepted_outcome.as_deref()
                != Some(observation.expected_accepted_outcome_hash.as_str())
            || observation.configuration_hash.as_deref() != Some(expected_configuration_hash)
            || observation.eligible_files.is_none()
            || observation.indexed_files.is_none()
            || observation.source_bytes.is_none()
            || observation.catalogue_rows_before.is_none()
            || observation.catalogue_rows_after.is_none()
            || observation.catalogue_bytes_before.is_none()
            || observation.catalogue_bytes_after.is_none()
            || observation.durable_catalogue_rows_before.is_none()
            || observation.durable_catalogue_rows_after.is_none()
            || observation.durable_catalogue_bytes_before.is_none()
            || observation.durable_catalogue_bytes_after.is_none()
            || observation.derived_catalogue_rows_before.is_none()
            || observation.derived_catalogue_rows_after.is_none()
            || observation.derived_catalogue_bytes_before.is_none()
            || observation.derived_catalogue_bytes_after.is_none()
        {
            return Some(format!(
                "catalogue observation lacks required identity, configuration, or measurements for {:?}",
                observation.key()
            ));
        }
        let before_rows = observation
            .durable_catalogue_rows_before
            .unwrap_or(0)
            .saturating_add(observation.derived_catalogue_rows_before.unwrap_or(0));
        let after_rows = observation
            .durable_catalogue_rows_after
            .unwrap_or(0)
            .saturating_add(observation.derived_catalogue_rows_after.unwrap_or(0));
        let before_pages = observation
            .durable_catalogue_bytes_before
            .unwrap_or(0)
            .saturating_add(observation.derived_catalogue_bytes_before.unwrap_or(0));
        let after_pages = observation
            .durable_catalogue_bytes_after
            .unwrap_or(0)
            .saturating_add(observation.derived_catalogue_bytes_after.unwrap_or(0));
        if observation.catalogue_rows_before != Some(before_rows)
            || observation.catalogue_rows_after != Some(after_rows)
            || observation
                .catalogue_bytes_before
                .is_none_or(|total| total < before_pages)
            || observation
                .catalogue_bytes_after
                .is_none_or(|total| total < after_pages)
        {
            return Some(format!(
                "catalogue totals disagree with durable/derived category sums for {:?}",
                observation.key()
            ));
        }
        let derived_accepted = canonical_json_sha256(&serde_json::json!({
            "dataset_name": observation.dataset_name,
            "evidence_counts": observation.observed_evidence_counts,
            "target_identity": observation.observed_target_identity,
        }))
        .ok();
        if derived_accepted.as_deref() != observation.accepted_outcome.as_deref()
            || observation.observed_target_identity.as_ref() != Some(&observation.target_identity)
            || observation.eligible_files != Some(observation.expected_eligible_files)
            || observation.indexed_files != Some(observation.expected_indexed_files)
            || observation.source_bytes != Some(observation.expected_source_bytes)
        {
            return Some(format!(
                "observed target, evidence, or file counts differ from frozen expectations for {:?}",
                observation.key()
            ));
        }
    } else if observation.observed_target_identity.is_some()
        || observation.observed_evidence_counts.is_some()
        || observation.accepted_outcome.is_some()
        || observation.configuration_hash.is_some()
        || observation.eligible_files.is_some()
        || observation.indexed_files.is_some()
        || observation.source_bytes.is_some()
        || observation.catalogue_rows_before.is_some()
        || observation.catalogue_rows_after.is_some()
        || observation.catalogue_bytes_before.is_some()
        || observation.catalogue_bytes_after.is_some()
        || observation.durable_catalogue_rows_before.is_some()
        || observation.durable_catalogue_rows_after.is_some()
        || observation.durable_catalogue_bytes_before.is_some()
        || observation.durable_catalogue_bytes_after.is_some()
        || observation.derived_catalogue_rows_before.is_some()
        || observation.derived_catalogue_rows_after.is_some()
        || observation.derived_catalogue_bytes_before.is_some()
        || observation.derived_catalogue_bytes_after.is_some()
    {
        return Some(format!(
            "stateless phase fabricated catalogue identity or measurements for {:?}",
            observation.key()
        ));
    }
    match descriptor.reconcile_measurements {
        CapacityReconcileMeasurements::NotApplicable => {
            if observation.parsed_files.is_some()
                || observation.changed_files.is_some()
                || observation.changed_bytes.is_some()
            {
                return Some(format!(
                    "non-reconcile phase fabricated reconcile measurements for {:?}",
                    observation.key()
                ));
            }
        }
        CapacityReconcileMeasurements::Observed => {
            if observation.parsed_files.is_none()
                || observation.changed_files != Some(0)
                || observation.changed_bytes != Some(0)
            {
                return Some(format!(
                    "reconcile phase lacks observed parsed/zero-change measurements for {:?}",
                    observation.key()
                ));
            }
        }
        CapacityReconcileMeasurements::NoChange => {
            if observation.parsed_files != Some(0)
                || observation.changed_files != Some(0)
                || observation.changed_bytes != Some(0)
            {
                return Some(format!(
                    "no-change reconcile reported changed work for {:?}",
                    observation.key()
                ));
            }
        }
        CapacityReconcileMeasurements::FixedIncremental => {
            let expected_parsed = if observation.sample_class == "cold"
                || (observation.sample_class == "warmup" && observation.sample_index == 1)
            {
                observation.expected_changed_files.saturating_sub(1)
            } else {
                0
            };
            if observation.changed_files != Some(observation.expected_changed_files)
                || observation.changed_bytes != Some(observation.expected_changed_bytes)
                || observation.parsed_files != Some(expected_parsed)
            {
                return Some(format!(
                    "fixed incremental parsed/changed files or bytes differ from frozen expectations for {:?}",
                    observation.key()
                ));
            }
        }
    }
    if descriptor.ready_truth_measurements {
        let expected_ready_truth = if phase == CapacityPhase::ProgressiveDeepRouteCompiler {
            observation
                .accepted_outcome
                .as_deref()
                .and_then(|accepted| ready_truth_identity(accepted, true).ok())
                .flatten()
        } else {
            Some(observation.expected_ready_versus_truth_result_hash.clone())
        };
        if observation.ready_versus_truth.as_deref() != expected_ready_truth.as_deref() {
            return Some(format!(
                "ready/Truth equivalence failed for {:?}",
                observation.key()
            ));
        }
    } else if observation.ready_versus_truth.is_some() {
        return Some(format!(
            "unrelated phase fabricated ready/Truth identity for {:?}",
            observation.key()
        ));
    }
    if let Some(expected_fallback) = descriptor.compiler_fallback {
        let compiler_complete = observation.compiler_selected_records.is_some()
            && observation.compiler_selected_source_bytes.is_some()
            && observation.compiler_selected_estimated_tokens.is_some()
            && observation.compiler_omitted_records.is_some()
            && observation.compiler_truncated.is_some()
            && observation.compiler_work_units_consumed.is_some()
            && observation.compiler_fallback == Some(expected_fallback);
        let omission_exact = observation.compiler_truncated
            == observation
                .compiler_omitted_records
                .map(|omitted| omitted != 0);
        let selected_within_budget = observation
            .profile_inputs
            .get("records")
            .and_then(Value::as_u64)
            .zip(observation.compiler_selected_records)
            .is_some_and(|(maximum, selected)| selected <= maximum)
            && observation
                .profile_inputs
                .get("source_bytes")
                .and_then(Value::as_u64)
                .zip(observation.compiler_selected_source_bytes)
                .is_some_and(|(maximum, selected)| selected <= maximum)
            && observation
                .profile_inputs
                .get("estimated_tokens")
                .and_then(Value::as_u64)
                .zip(observation.compiler_selected_estimated_tokens)
                .is_some_and(|(maximum, selected)| selected <= maximum);
        if !compiler_complete || !omission_exact || !selected_within_budget {
            return Some(format!(
                "compiler phase lacks observed byte/token/omission/fallback evidence for {:?}",
                observation.key()
            ));
        }
        let candidates_considered = observation.compiler_selected_records.and_then(|selected| {
            selected.checked_add(observation.compiler_omitted_records.unwrap_or(0))
        });
        let work_reconciles = if phase == CapacityPhase::ProgressiveDeepRouteCompiler {
            observation.deterministic_work_units
                == observation
                    .compiler_work_units_consumed
                    .and_then(|compiler| compiler.checked_add(1))
        } else {
            observation.compiler_work_units_consumed == candidates_considered
                && observation.deterministic_work_units == observation.compiler_work_units_consumed
        };
        if !work_reconciles {
            return Some(format!(
                "compiler actual work does not match observed selection and route work for {:?}",
                observation.key()
            ));
        }
    } else if observation.compiler_selected_records.is_some()
        || observation.compiler_selected_source_bytes.is_some()
        || observation.compiler_selected_estimated_tokens.is_some()
        || observation.compiler_omitted_records.is_some()
        || observation.compiler_truncated.is_some()
        || observation.compiler_work_units_consumed.is_some()
        || observation.compiler_fallback.is_some()
    {
        return Some(format!(
            "non-compiler phase fabricated compiler measurements for {:?}",
            observation.key()
        ));
    }
    let key = CapacityDeterminismKey {
        dataset_name: observation.dataset_name.clone(),
        profile: observation.profile.clone(),
        provider_condition: observation.provider_condition.clone(),
        phase: observation.phase.clone(),
    };
    let identity = observation
        .deterministic_identity
        .as_ref()
        .expect("checked above");
    if let Some(expected) = deterministic_identities.get(&key) {
        if expected != identity {
            return Some(format!(
                "deterministic identity changed from {expected} to {identity} for {:?}",
                observation.key()
            ));
        }
    } else {
        deterministic_identities.insert(key, identity.clone());
    }
    None
}

#[derive(Default)]
struct CapacityScoredSuccess {
    durations: Vec<u64>,
    compiler: Vec<CompilerMeasurement>,
}

#[allow(clippy::too_many_arguments)]
fn retain_capacity_observation(
    writer: &mut CapacityRawWriter,
    expected: &mut BTreeSet<CapacityObservationKey>,
    outcome_counts: &mut BTreeMap<String, u64>,
    condition_scored_success: &mut BTreeMap<CapacityAggregationKey, CapacityScoredSuccess>,
    condition_outcomes: &mut BTreeMap<CapacityAggregationKey, BTreeMap<String, u64>>,
    deterministic_identities: &mut BTreeMap<CapacityDeterminismKey, String>,
    campaign_failures: &mut Vec<String>,
    observation: CapacityRawObservation,
) -> Result<()> {
    expected.insert(observation.key());
    writer.write(&observation)?;
    if let Some(failure) = capacity_observation_failure(&observation, deterministic_identities) {
        campaign_failures.push(failure);
    } else if observation.outcome == "success" && observation.sample_class == "warm" {
        let scored = condition_scored_success
            .entry(CapacityAggregationKey::from(&observation))
            .or_default();
        if let Some(duration) = observation.atlas_runtime_duration_ms {
            scored.durations.push(duration);
        }
        if let (
            Some(selected_records),
            Some(selected_source_bytes),
            Some(selected_estimated_tokens),
            Some(omitted_records),
            Some(truncated),
            Some(work_units_consumed),
            Some(fallback),
        ) = (
            observation.compiler_selected_records,
            observation.compiler_selected_source_bytes,
            observation.compiler_selected_estimated_tokens,
            observation.compiler_omitted_records,
            observation.compiler_truncated,
            observation.compiler_work_units_consumed,
            observation.compiler_fallback,
        ) {
            scored.compiler.push(CompilerMeasurement {
                selected_records,
                selected_source_bytes,
                selected_estimated_tokens,
                omitted_records,
                truncated,
                fallback,
                work_units_consumed,
            });
        }
    }
    *condition_outcomes
        .entry(CapacityAggregationKey::from(&observation))
        .or_default()
        .entry(observation.outcome.clone())
        .or_default() += 1;
    *outcome_counts
        .entry(observation.outcome.clone())
        .or_default() += 1;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn retain_scheduled_capacity_attempt<E: CapacityExecutor>(
    executor: &mut E,
    prepared: &mut CapacityPreparation<E::Prepared>,
    writer: &mut CapacityRawWriter,
    expected: &mut BTreeSet<CapacityObservationKey>,
    outcome_counts: &mut BTreeMap<String, u64>,
    condition_scored_success: &mut BTreeMap<CapacityAggregationKey, CapacityScoredSuccess>,
    condition_outcomes: &mut BTreeMap<CapacityAggregationKey, BTreeMap<String, u64>>,
    deterministic_identities: &mut BTreeMap<CapacityDeterminismKey, String>,
    campaign_failures: &mut Vec<String>,
    provenance_sha256: &str,
    manifest: &Value,
    accepted_outcome: Option<&CapacityAcceptedOutcome>,
    profile: &ProfileScenario,
    attempt: CapacityAttemptSpec<'_>,
) -> Result<()> {
    let empty = empty_capacity_observation(
        provenance_sha256,
        manifest,
        accepted_outcome,
        profile,
        attempt,
    )?;
    let observation = execute_capacity_preparation(executor, prepared, empty);
    retain_capacity_observation(
        writer,
        expected,
        outcome_counts,
        condition_scored_success,
        condition_outcomes,
        deterministic_identities,
        campaign_failures,
        observation,
    )
}

fn ensure_memory_output_absent(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(AtlasError::Other(format!(
            "memory evidence destination {} already exists",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryArtifactKind {
    RawV1,
    SummaryV1,
    RawV2,
    SummaryV2,
}

fn classify_memory_artifact_name(name: &str) -> Option<MemoryArtifactKind> {
    if name.contains(['/', '\\']) {
        return None;
    }
    for (suffix, kind) in [
        ("-memory-summary-v1.json", MemoryArtifactKind::SummaryV1),
        ("-memory-summary-v2.json", MemoryArtifactKind::SummaryV2),
        ("-memory-raw-v1.ndjson", MemoryArtifactKind::RawV1),
    ] {
        if name
            .strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty())
        {
            return Some(kind);
        }
    }
    let stem = name.strip_suffix(".ndjson")?;
    let (prefix, ordinal) = stem.rsplit_once("-memory-raw-v2-")?;
    (!prefix.is_empty() && ordinal.len() == 6 && ordinal.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(MemoryArtifactKind::RawV2)
}

fn validate_memory_artifact_namespace(
    evidence_directory: &Path,
    allowed: &BTreeSet<String>,
) -> Result<()> {
    let mut actual = BTreeSet::new();
    for entry in std::fs::read_dir(evidence_directory)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            AtlasError::Other("memory artifact namespace contains a non-UTF-8 name".into())
        })?;
        if classify_memory_artifact_name(&name).is_some() {
            actual.insert(name);
        }
    }
    if &actual != allowed {
        return Err(AtlasError::Other(
            "memory artifact namespace contains missing, mixed, or undeclared entries".into(),
        ));
    }
    Ok(())
}

fn validate_open_memory_file(file: &File, path: &Path, maximum_bytes: u64) -> Result<u64> {
    let metadata = file.metadata()?;
    let linked = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            metadata.nlink() != 1
        }
        #[cfg(windows)]
        {
            let (_, _, attributes, links) = windows_handle_identity(file)?;
            attributes & 0x0400 != 0 || links != 1
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    };
    if !metadata.file_type().is_file()
        || linked
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        return Err(AtlasError::Other(format!(
            "memory evidence {} is missing, externally linked, a reparse point, empty, or exceeds its byte bound",
            path.display()
        )));
    }
    Ok(metadata.len())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatedMemoryFrame {
    byte_length: u64,
    name: String,
    order: usize,
    record_count: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatedMemoryCompletion {
    artifact_count: usize,
    raw_byte_length: u64,
    raw_record_count: u64,
    raw_sha256: String,
}

#[derive(Debug)]
struct ValidatedMemorySegment {
    name: String,
    path: PathBuf,
    file: File,
    _spool_path: tempfile::TempPath,
    frame: ValidatedMemoryFrame,
}

#[derive(Debug)]
struct ValidatedMemorySegments {
    segments: Vec<ValidatedMemorySegment>,
    summary: Value,
    summary_file: File,
    summary_path: PathBuf,
    _summary_spool_path: tempfile::TempPath,
    record_count: u64,
    byte_length: u64,
    sha256: String,
}

#[derive(Debug)]
struct ReceivedMemoryBundle {
    segments: Vec<ValidatedMemorySegment>,
    summary: Value,
    summary_file: File,
    summary_path: PathBuf,
    summary_spool_path: tempfile::TempPath,
    completion: ValidatedMemoryCompletion,
    whole_sha256: String,
}

fn read_memory_protocol_line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line)?;
    if read == 0 || read > 16 * 1024 || !line.ends_with(b"\n") || line.ends_with(b"\r\n") {
        return Err(AtlasError::Other(
            "validated memory stream contains a missing, over-bound, or non-LF frame".into(),
        ));
    }
    Ok(line)
}

fn spool_validated_memory_artifact(
    reader: &mut impl Read,
    evidence_directory: &Path,
    frame: &ValidatedMemoryFrame,
    maximum_bytes: u64,
    mut aggregate_digest: Option<&mut Sha256>,
) -> Result<(File, tempfile::TempPath)> {
    if frame.byte_length == 0
        || frame.byte_length > maximum_bytes
        || frame.sha256.len() != 64
        || !frame.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AtlasError::Other(
            "validated memory artifact frame exceeds its bounds or has a malformed digest".into(),
        ));
    }
    let temporary = tempfile::Builder::new()
        .prefix(".atlas-memory-log-spool-")
        .tempfile_in(evidence_directory)?;
    let (mut file, spool_path) = temporary.into_parts();
    let mut remaining = frame.byte_length;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| AtlasError::Other("memory spool byte bound overflow".into()))?;
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(AtlasError::Other(
                "validated memory artifact payload is truncated".into(),
            ));
        }
        file.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
        if let Some(aggregate) = aggregate_digest.as_deref_mut() {
            aggregate.update(&buffer[..read]);
        }
        remaining -= read as u64;
    }
    file.flush()?;
    let observed_sha256 = hex::encode(digest.finalize());
    if observed_sha256 != frame.sha256
        || validate_open_memory_file(&file, spool_path.as_ref(), maximum_bytes)?
            != frame.byte_length
    {
        return Err(AtlasError::Other(
            "validated memory artifact payload length or digest is inconsistent".into(),
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok((file, spool_path))
}

trait MemoryValidatorChildCleanup {
    type Status: std::fmt::Display;

    fn kill_for_cleanup(&mut self) -> std::io::Result<()>;
    fn wait_for_cleanup(&mut self) -> std::io::Result<Self::Status>;
    fn try_wait_for_cleanup(&mut self) -> std::io::Result<Option<Self::Status>>;
}

impl MemoryValidatorChildCleanup for std::process::Child {
    type Status = std::process::ExitStatus;

    fn kill_for_cleanup(&mut self) -> std::io::Result<()> {
        self.kill()
    }

    fn wait_for_cleanup(&mut self) -> std::io::Result<Self::Status> {
        self.wait()
    }

    fn try_wait_for_cleanup(&mut self) -> std::io::Result<Option<Self::Status>> {
        self.try_wait()
    }
}

const MEMORY_VALIDATOR_REAP_ATTEMPTS: usize = 4;
const MEMORY_VALIDATOR_CLEANUP_DETAIL_MAX_CHARS: usize = 256;

fn bounded_memory_validator_cleanup_detail(error: &std::io::Error) -> String {
    error
        .to_string()
        .chars()
        .take(MEMORY_VALIDATOR_CLEANUP_DETAIL_MAX_CHARS)
        .collect()
}

fn recover_memory_validator_wait_failure<C, F>(
    initial_wait_error: std::io::Error,
    child: &mut C,
    join_stderr: F,
) -> AtlasError
where
    C: MemoryValidatorChildCleanup,
    F: FnOnce() -> std::thread::Result<std::io::Result<()>>,
{
    let initial_wait_error = bounded_memory_validator_cleanup_detail(&initial_wait_error);
    let (kill, kill_succeeded) = match child.kill_for_cleanup() {
        Ok(()) => ("ok".to_string(), true),
        Err(error) => (
            format!("error: {}", bounded_memory_validator_cleanup_detail(&error)),
            false,
        ),
    };
    let mut reap_attempts = 0;
    let (reap_method, reap) = if kill_succeeded {
        (
            "wait",
            loop {
                reap_attempts += 1;
                match child.wait_for_cleanup() {
                    Ok(status) => break Ok(Some(status)),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::Interrupted
                            && reap_attempts < MEMORY_VALIDATOR_REAP_ATTEMPTS =>
                    {
                        continue;
                    }
                    Err(error) => break Err(error),
                }
            },
        )
    } else {
        reap_attempts = 1;
        ("try_wait", child.try_wait_for_cleanup())
    };
    let (reap, drainer) = match reap {
        Ok(Some(status)) => {
            let drainer = match join_stderr() {
                Ok(Ok(())) => "completed".to_string(),
                Ok(Err(error)) => format!(
                    "io error: {}",
                    bounded_memory_validator_cleanup_detail(&error)
                ),
                Err(_) => "thread panicked".to_string(),
            };
            (
                status
                    .to_string()
                    .chars()
                    .take(MEMORY_VALIDATOR_CLEANUP_DETAIL_MAX_CHARS)
                    .collect(),
                drainer,
            )
        }
        Ok(None) => (
            "still running".to_string(),
            "not joined because reap was not established".to_string(),
        ),
        Err(error) => (
            format!("error: {}", bounded_memory_validator_cleanup_detail(&error)),
            "not joined because reap was not established".to_string(),
        ),
    };
    AtlasError::Other(format!(
        "process-tree memory validator initial wait failed: {initial_wait_error}; kill={kill}; reap_method={reap_method}; reap={reap}; reap_attempts={reap_attempts}; drainer={drainer}"
    ))
}

fn validate_memory_sampler_outputs(
    evidence_directory: &Path,
    summary_path: &Path,
    sampler_path: &Path,
    expected_execution: &Value,
) -> Result<ValidatedMemorySegments> {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(python)
        .args([
            "-B",
            "-c",
            "import pathlib,runpy,sys; module=runpy.run_path(sys.argv[1]); module['validate_segmented_evidence'](pathlib.Path(sys.argv[2]), validated_output=sys.stdout.buffer)",
        ])
        .arg(sampler_path)
        .arg(summary_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AtlasError::Other(
                "validated memory stream did not expose its private stdout pipe".into(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Err(AtlasError::Other(
                "validated memory stream did not expose its private stderr pipe".into(),
            ));
        }
    };
    let stderr_thread = match std::thread::Builder::new()
        .name("atlas-memory-validator-stderr".into())
        .spawn(move || consume_sampler_stderr(stderr))
    {
        Ok(thread) => thread,
        Err(error) => {
            drop(stdout);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
    };
    let mut reader = BufReader::new(stdout);
    let parsed = (|| -> Result<ReceivedMemoryBundle> {
        if read_memory_protocol_line(&mut reader)? != b"ATLAS_MEMORY_BUNDLE_V1\n" {
            return Err(AtlasError::Other(
                "validated memory stream has the wrong protocol version".into(),
            ));
        }
        let mut segments = Vec::new();
        let mut summary_spool = None;
        let mut whole_digest = Sha256::new();
        let mut streamed_raw_bytes = 0_u64;
        loop {
            let line = read_memory_protocol_line(&mut reader)?;
            if let Some(payload) = line.strip_prefix(b"ATLAS_MEMORY_VALIDATED ") {
                if summary_spool.is_none() {
                    return Err(AtlasError::Other(
                        "validated memory stream completed without a summary".into(),
                    ));
                }
                let completion: ValidatedMemoryCompletion =
                    serde_json::from_slice(&payload[..payload.len() - 1])?;
                let mut extra = [0_u8; 1];
                if reader.read(&mut extra)? != 0 {
                    return Err(AtlasError::Other(
                        "validated memory stream contains bytes after its completion frame".into(),
                    ));
                }
                let (summary, summary_file, summary_path, summary_temp_path) =
                    summary_spool.unwrap();
                return Ok(ReceivedMemoryBundle {
                    segments,
                    summary,
                    summary_file,
                    summary_path,
                    summary_spool_path: summary_temp_path,
                    completion,
                    whole_sha256: hex::encode(whole_digest.finalize()),
                });
            }
            let payload = line
                .strip_prefix(b"ATLAS_MEMORY_ARTIFACT ")
                .ok_or_else(|| {
                    AtlasError::Other("validated memory stream contains an unknown frame".into())
                })?;
            let frame: ValidatedMemoryFrame =
                serde_json::from_slice(&payload[..payload.len() - 1])?;
            if frame.order != segments.len() || summary_spool.is_some() {
                return Err(AtlasError::Other(
                    "validated memory artifacts are duplicated, missing, or reordered".into(),
                ));
            }
            if frame.name == MEMORY_SUMMARY_FILE {
                if segments.is_empty() || frame.record_count != 1 {
                    return Err(AtlasError::Other(
                        "validated memory summary frame is malformed or out of order".into(),
                    ));
                }
                let (mut file, spool_path) = spool_validated_memory_artifact(
                    &mut reader,
                    evidence_directory,
                    &frame,
                    MEMORY_MAX_SUMMARY_BYTES,
                    None,
                )?;
                if validate_open_memory_file(&file, spool_path.as_ref(), MEMORY_MAX_SUMMARY_BYTES)?
                    != frame.byte_length
                {
                    return Err(AtlasError::Other(
                        "validated memory summary spool changed before parsing".into(),
                    ));
                }
                let mut bytes = Vec::with_capacity(frame.byte_length as usize);
                file.read_to_end(&mut bytes)?;
                if bytes.len() as u64 != frame.byte_length
                    || validate_open_memory_file(
                        &file,
                        spool_path.as_ref(),
                        MEMORY_MAX_SUMMARY_BYTES,
                    )? != frame.byte_length
                {
                    return Err(AtlasError::Other(
                        "validated memory summary spool changed while parsing".into(),
                    ));
                }
                let summary: Value = serde_json::from_slice(&bytes)?;
                file.seek(SeekFrom::Start(0))?;
                let path = spool_path.to_path_buf();
                summary_spool = Some((summary, file, path, spool_path));
                continue;
            }
            if segments.len() >= MEMORY_MAX_SEGMENTS
                || frame.name != format!("{MEMORY_RAW_PREFIX}-{:06}.ndjson", segments.len())
                || frame.record_count == 0
                || frame.record_count > MEMORY_MAX_SEGMENT_SAMPLES
            {
                return Err(AtlasError::Other(
                    "validated memory segment identity, order, or count is inconsistent".into(),
                ));
            }
            streamed_raw_bytes = streamed_raw_bytes
                .checked_add(frame.byte_length)
                .filter(|bytes| *bytes <= CAPACITY_LOG_MAX_RAW_BYTES)
                .ok_or_else(|| {
                    AtlasError::Other(
                        "validated memory stream exceeds the total logging source bound".into(),
                    )
                })?;
            let (file, spool_path) = spool_validated_memory_artifact(
                &mut reader,
                evidence_directory,
                &frame,
                MEMORY_MAX_RAW_BYTES,
                Some(&mut whole_digest),
            )?;
            let path = spool_path.to_path_buf();
            segments.push(ValidatedMemorySegment {
                name: frame.name.clone(),
                path,
                file,
                _spool_path: spool_path,
                frame,
            });
        }
    })();
    drop(reader);
    let kill_error = if parsed.is_err() {
        child.kill().err()
    } else {
        None
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            return Err(recover_memory_validator_wait_failure(
                error,
                &mut child,
                move || stderr_thread.join().map(|result| result.map(|_| ())),
            ));
        }
    };
    let stderr = stderr_thread
        .join()
        .map_err(|_| AtlasError::Other("validator stderr draining thread panicked".into()))??;
    let diagnostic = String::from_utf8_lossy(&stderr);
    let diagnostic = diagnostic.trim();
    let received = match parsed {
        Ok(received) => received,
        Err(error) => {
            return Err(AtlasError::Other(format!(
                "process-tree memory evidence failed authoritative streaming validation: {error}; child={status}; kill_error={kill_error:?}; diagnostic={diagnostic}"
            )));
        }
    };
    if !status.success() {
        return Err(AtlasError::Other(format!(
            "process-tree memory evidence failed authoritative streaming validation with {status}: diagnostic={diagnostic}"
        )));
    }
    let ReceivedMemoryBundle {
        segments,
        summary,
        summary_file,
        summary_path,
        summary_spool_path,
        completion,
        whole_sha256,
    } = received;
    let object = summary.as_object().ok_or_else(|| {
        AtlasError::Other("process-tree memory summary must be a JSON object".into())
    })?;
    let expected_fields = BTreeSet::from([
        "baseline",
        "baseline_visible_not_subtracted",
        "cadence_ms",
        "campaign",
        "claim",
        "capacity_execution",
        "events",
        "exit",
        "idle_control_duration_ms",
        "kind",
        "limitations",
        "maximum_segment_bytes",
        "maximum_segment_samples",
        "maximum_segments",
        "platform",
        "raw_byte_length",
        "raw_record_count",
        "raw_sha256",
        "schema_version",
        "segment_name_prefix",
        "segments",
        "tool",
    ]);
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_fields
        || summary["schema_version"] != "2.0.0"
        || summary["kind"] != "process-tree-memory-segmented-summary"
        || summary["cadence_ms"] != 50
        || summary["idle_control_duration_ms"] != 5_000
        || summary["segment_name_prefix"] != MEMORY_RAW_PREFIX
        || summary["maximum_segment_bytes"] != MEMORY_MAX_RAW_BYTES
        || summary["maximum_segment_samples"] != MEMORY_MAX_SEGMENT_SAMPLES
        || summary["maximum_segments"] != MEMORY_MAX_SEGMENTS as u64
        || summary["baseline_visible_not_subtracted"] != true
        || summary["exit"]["status"] != "clean"
        || summary["exit"]["code"] != 0
        || summary["platform"]["os"] != std::env::consts::OS
        || summary["raw_record_count"]
            .as_u64()
            .is_none_or(|count| count <= 101)
        || summary["tool"]["name"] != "process-tree-memory.py"
        || summary["tool"]["version"] != "1.0.0"
        || summary["tool"]["sha256"] != file_sha256(sampler_path)?
        || summary["capacity_execution"] != *expected_execution
    {
        return Err(AtlasError::Other(
            "process-tree memory segmented summary schema or fixed method contract is inconsistent"
                .into(),
        ));
    }
    let descriptors = summary["segments"].as_array().ok_or_else(|| {
        AtlasError::Other("process-tree memory segment manifest is missing".into())
    })?;
    let record_count = segments.iter().try_fold(0_u64, |total, segment| {
        total
            .checked_add(segment.frame.record_count)
            .ok_or_else(|| AtlasError::Other("memory segment record count overflow".into()))
    })?;
    let byte_length = segments.iter().try_fold(0_u64, |total, segment| {
        total
            .checked_add(segment.frame.byte_length)
            .ok_or_else(|| AtlasError::Other("memory segment byte count overflow".into()))
    })?;
    if descriptors.len() != segments.len()
        || descriptors
            .iter()
            .zip(&segments)
            .any(|(descriptor, segment)| {
                descriptor["path"] != segment.name
                    || descriptor["order"].as_u64() != Some(segment.frame.order as u64)
                    || descriptor["record_count"].as_u64() != Some(segment.frame.record_count)
                    || descriptor["byte_length"].as_u64() != Some(segment.frame.byte_length)
                    || descriptor["sha256"] != segment.frame.sha256
            })
        || completion.artifact_count != segments.len() + 1
        || completion.raw_record_count != record_count
        || completion.raw_byte_length != byte_length
        || completion.raw_sha256 != whole_sha256
        || summary["raw_record_count"].as_u64() != Some(record_count)
        || summary["raw_byte_length"].as_u64() != Some(byte_length)
        || summary["raw_sha256"] != whole_sha256
    {
        return Err(AtlasError::Other(
            "validated memory protocol framing or whole-run binding is inconsistent".into(),
        ));
    }
    let allowed_memory_artifacts = std::iter::once(MEMORY_SUMMARY_FILE.to_string())
        .chain(segments.iter().map(|segment| segment.name.clone()))
        .collect::<BTreeSet<_>>();
    validate_memory_artifact_namespace(evidence_directory, &allowed_memory_artifacts)?;
    Ok(ValidatedMemorySegments {
        segments,
        summary,
        summary_file,
        summary_path,
        _summary_spool_path: summary_spool_path,
        record_count,
        byte_length,
        sha256: whole_sha256,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapacityProgressLogState {
    Starting,
    Active,
    Complete,
    Stopped,
}

struct ParsedCapacityProgress<'a> {
    state: CapacityProgressLogState,
    ordinal: u64,
    total: u64,
    phase: &'a str,
}

fn parse_capacity_progress_line(line: &str) -> Option<ParsedCapacityProgress<'_>> {
    if line.len() > SAMPLER_PROGRESS_MAX_LINE_BYTES {
        return None;
    }
    let mut fields = line.split_ascii_whitespace();
    if fields.next()? != CAPACITY_PROGRESS_PREFIX {
        return None;
    }
    let state = match fields.next()?.strip_prefix("state=")? {
        "starting" => CapacityProgressLogState::Starting,
        "active" => CapacityProgressLogState::Active,
        "complete" => CapacityProgressLogState::Complete,
        "stopped" => CapacityProgressLogState::Stopped,
        _ => return None,
    };
    let ordinal = fields
        .next()?
        .strip_prefix("ordinal=")?
        .parse::<u64>()
        .ok()?;
    let total = fields.next()?.strip_prefix("total=")?.parse::<u64>().ok()?;
    let phase = fields.next()?.strip_prefix("phase=")?;
    if fields.next().is_some()
        || !valid_capacity_progress_total(total)
        || !(phase == "harness-startup"
            || CAPACITY_PHASES
                .iter()
                .any(|descriptor| descriptor.name == phase))
        || match state {
            CapacityProgressLogState::Starting => ordinal != 0 || phase != "harness-startup",
            CapacityProgressLogState::Active => ordinal == 0 || ordinal > total,
            CapacityProgressLogState::Complete => ordinal != total,
            CapacityProgressLogState::Stopped => ordinal > total,
        }
    {
        return None;
    }
    Some(ParsedCapacityProgress {
        state,
        ordinal,
        total,
        phase,
    })
}

fn append_sampler_diagnostic(diagnostic: &mut Vec<u8>, bytes: &[u8]) {
    let remaining = SAMPLER_DIAGNOSTIC_MAX_BYTES.saturating_sub(diagnostic.len());
    diagnostic.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

#[derive(Default)]
struct CapacityProgressForwardingState {
    last_ordinal: Option<u64>,
    progress_total: Option<u64>,
    last_emitted_ordinal: Option<u64>,
    last_phase: String,
    emitted_lines: usize,
    final_seen: bool,
}

fn forward_sampler_line<W: Write>(
    raw_line: &[u8],
    diagnostic: &mut Vec<u8>,
    progress_output: &mut W,
    forwarding: &mut CapacityProgressForwardingState,
) -> std::io::Result<()> {
    let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
    if let Some(progress) = std::str::from_utf8(line)
        .ok()
        .and_then(parse_capacity_progress_line)
    {
        let monotonic = forwarding
            .progress_total
            .is_none_or(|total| total == progress.total)
            && forwarding.last_ordinal.is_none_or(|last| {
                progress.ordinal > last
                    || (matches!(
                        progress.state,
                        CapacityProgressLogState::Complete | CapacityProgressLogState::Stopped
                    ) && progress.ordinal == last)
            });
        let rate_limited = matches!(
            progress.state,
            CapacityProgressLogState::Starting
                | CapacityProgressLogState::Complete
                | CapacityProgressLogState::Stopped
        ) || progress.phase != forwarding.last_phase
            || forwarding.last_emitted_ordinal.is_none_or(|last| {
                progress.ordinal.saturating_sub(last) >= CAPACITY_PROGRESS_INTERVAL
            });
        if monotonic && !forwarding.final_seen {
            forwarding.progress_total = Some(progress.total);
            forwarding.last_ordinal = Some(progress.ordinal);
            if rate_limited {
                if forwarding.emitted_lines >= SAMPLER_PROGRESS_MAX_LINES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed capacity progress protocol",
                    ));
                }
                progress_output.write_all(line)?;
                progress_output.write_all(b"\n")?;
                progress_output.flush()?;
                forwarding.last_emitted_ordinal = Some(progress.ordinal);
                progress.phase.clone_into(&mut forwarding.last_phase);
                forwarding.emitted_lines += 1;
                forwarding.final_seen = matches!(
                    progress.state,
                    CapacityProgressLogState::Complete | CapacityProgressLogState::Stopped
                );
            }
            return Ok(());
        }
    }
    if line.starts_with(CAPACITY_PROGRESS_PREFIX.as_bytes()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "malformed capacity progress protocol",
        ));
    }
    append_sampler_diagnostic(diagnostic, line);
    append_sampler_diagnostic(diagnostic, b"\n");
    Ok(())
}

fn consume_sampler_stdout<R: Read, W: Write>(
    reader: R,
    mut progress_output: W,
) -> std::io::Result<Vec<u8>> {
    let mut diagnostic = Vec::with_capacity(SAMPLER_DIAGNOSTIC_MAX_BYTES);
    let mut forwarding = CapacityProgressForwardingState::default();
    for line in BufReader::new(reader).split(b'\n') {
        forward_sampler_line(
            &line?,
            &mut diagnostic,
            &mut progress_output,
            &mut forwarding,
        )?;
    }
    Ok(diagnostic)
}

fn consume_sampler_stderr<R: Read>(mut reader: R) -> std::io::Result<Vec<u8>> {
    let mut diagnostic = Vec::with_capacity(SAMPLER_DIAGNOSTIC_MAX_BYTES);
    reader
        .by_ref()
        .take(SAMPLER_DIAGNOSTIC_MAX_BYTES as u64)
        .read_to_end(&mut diagnostic)?;
    std::io::copy(&mut reader, &mut std::io::sink())?;
    Ok(diagnostic)
}

fn run_sampler_command<W: Write + Send + 'static>(
    command: &mut Command,
    progress_output: W,
) -> std::io::Result<std::process::Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("sampler stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("sampler stderr pipe is unavailable"))?;
    let stdout_thread = std::thread::spawn(move || consume_sampler_stdout(stdout, progress_output));
    let stderr_thread = std::thread::spawn(move || consume_sampler_stderr(stderr));
    let status = child.wait()?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| std::io::Error::other("sampler stdout forwarding thread panicked"))??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| std::io::Error::other("sampler stderr forwarding thread panicked"))??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn run_capacity_memory_sampler(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
    shard: Option<CapacityShard>,
) -> Result<(Vec<PathBuf>, PathBuf)> {
    let sampler = repository_root.join("scripts/process-tree-memory.py");
    let sampler_metadata = std::fs::symlink_metadata(&sampler)?;
    if !sampler_metadata.file_type().is_file() || sampler_metadata.file_type().is_symlink() {
        return Err(AtlasError::InvalidConfig(
            "process-tree memory sampler must be a tracked physical file".into(),
        ));
    }
    validate_capacity_manifests(repository_root, scenario, manifest_directory)?;
    let global_plan_sha256 = capacity_global_plan_sha256(repository_root, scenario_path, scenario)?;
    let execution_total = capacity_execution_total(shard);
    let execution_scope = capacity_execution_scope(shard, &global_plan_sha256);
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    validate_memory_artifact_namespace(&evidence.path, &BTreeSet::new())?;
    let summary_path = evidence.path.join(MEMORY_SUMMARY_FILE);

    let initial_labels = serde_json::to_vec(&CapacityMemoryLabels::startup(execution_total))?;
    let (label_path, label_file) =
        create_capacity_output(&evidence.path, evidence.guard(), MEMORY_LABEL_STATE_FILE)?;
    let label_file = finish_capacity_output(
        &evidence.path,
        evidence.guard(),
        label_file,
        &initial_labels,
    )?;
    drop(label_file);

    let executable = std::env::current_exe()?;
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut command = Command::new(python);
    command
        .arg("-B")
        .arg(&sampler)
        .arg("--evidence-dir")
        .arg(&evidence.path)
        .arg("--label-state")
        .arg(&label_path)
        .arg("--atlas-executable")
        .arg(&executable)
        .arg("--scenario")
        .arg(scenario_path)
        .arg("--capacity-manifest-dir")
        .arg(manifest_directory)
        .arg("--global-plan-sha256")
        .arg(&global_plan_sha256);
    if let Some(shard) = shard {
        command
            .arg("--capacity-shard-index")
            .arg(shard.index.to_string())
            .arg("--capacity-shard-count")
            .arg(CAPACITY_SHARD_COUNT.to_string());
    }
    command.stdin(Stdio::null());
    let output = run_sampler_command(&mut command, std::io::stdout());
    let label_removal = std::fs::remove_file(&label_path);
    validate_capacity_directory_guard(&evidence.path, evidence.guard())?;
    let output = output?;
    label_removal?;
    if !output.status.success() {
        let diagnostic = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        let redacted = redact_capacity_log_text(
            &String::from_utf8_lossy(diagnostic),
            repository_root,
            &evidence.path,
        );
        let bounded = redacted
            .chars()
            .take(SAMPLER_DIAGNOSTIC_MAX_BYTES)
            .collect::<String>();
        return Err(AtlasError::Other(format!(
            "process-tree memory sampler failed with {}; retained any bounded raw evidence; diagnostic={bounded}",
            output.status
        )));
    }
    let validated =
        validate_memory_sampler_outputs(&evidence.path, &summary_path, &sampler, &execution_scope)?;
    validate_capacity_directory_guard(&evidence.path, evidence.guard())?;
    Ok((
        validated
            .segments
            .into_iter()
            .map(|segment| segment.path)
            .collect(),
        summary_path,
    ))
}

fn write_capacity_accepted_outcome_preflight(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
) -> Result<PathBuf> {
    let manifests = validate_capacity_manifests(repository_root, scenario, manifest_directory)?;
    let accepted_outcomes = load_capacity_accepted_outcome_contract(
        repository_root,
        scenario,
        manifest_directory,
        &manifests,
    )?;
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    ensure_memory_output_absent(&evidence.path.join(CAPACITY_ACCEPTED_OUTCOME_PREFLIGHT_FILE))?;
    let preflight = run_capacity_accepted_outcome_preflight(
        repository_root,
        scenario_path,
        scenario,
        &manifests,
        &accepted_outcomes,
    )?;

    let (path, file) = create_capacity_output(
        &evidence.path,
        evidence.guard(),
        CAPACITY_ACCEPTED_OUTCOME_PREFLIGHT_FILE,
    )?;
    let _file = finish_capacity_output(
        &evidence.path,
        evidence.guard(),
        file,
        &serde_json::to_vec_pretty(&preflight)?,
    )?;
    Ok(path)
}

fn run_capacity_campaign(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
    memory_label_state: Option<&Path>,
    shard: Option<CapacityShard>,
) -> Result<(PathBuf, PathBuf)> {
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    ensure_memory_output_absent(&evidence.path.join(PRODUCTION_CAPACITY_EVIDENCE.raw_file))?;
    ensure_memory_output_absent(
        &evidence
            .path
            .join(PRODUCTION_CAPACITY_EVIDENCE.summary_file),
    )?;
    if let Some(path) = memory_label_state {
        CapacityMemoryLabelPublisher::open(&evidence.path, path)?;
    }
    let preflight_path = write_capacity_accepted_outcome_preflight(
        repository_root,
        scenario_path,
        scenario,
        manifest_directory,
        evidence_directory,
    )?;
    let mut executor = ProductionCapacityExecutor::default();
    let result = run_capacity_campaign_with_executor(
        repository_root,
        scenario_path,
        scenario,
        manifest_directory,
        evidence_directory,
        memory_label_state,
        shard,
        &mut executor,
    )?;
    if !preflight_path.is_file() {
        return Err(AtlasError::Other(
            "production accepted-outcome preflight evidence disappeared before campaign completion"
                .into(),
        ));
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn run_capacity_campaign_with_executor<E: CapacityExecutor>(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifest_directory: &Path,
    evidence_directory: &Path,
    memory_label_state: Option<&Path>,
    shard: Option<CapacityShard>,
    executor: &mut E,
) -> Result<(PathBuf, PathBuf)> {
    let evidence_names = E::EVIDENCE_NAMES;
    let manifests = validate_capacity_manifests(repository_root, scenario, manifest_directory)?;
    let accepted_outcomes = load_capacity_accepted_outcome_contract(
        repository_root,
        scenario,
        manifest_directory,
        &manifests,
    )?;
    let global_plan_sha256 = capacity_global_plan_sha256(repository_root, scenario_path, scenario)?;
    let execution_total = capacity_execution_total(shard);
    let evidence = package_excluded_evidence_directory(repository_root, evidence_directory)?;
    let mut memory_labels = memory_label_state
        .map(|path| CapacityMemoryLabelPublisher::open(&evidence.path, path))
        .transpose()?;
    let revision = command_text("git", &["rev-parse", "HEAD"]);
    if revision == "unavailable" {
        return Err(AtlasError::Other(
            "cannot execute capacity evidence without the tested Git revision".into(),
        ));
    }
    let provider_tools = CAPACITY_PROVIDERS
        .iter()
        .map(|provider| {
            let observed = provider
                .command
                .map(|command| command_text(command, provider.version_arguments))
                .unwrap_or_else(|| "builtin".to_string());
            serde_json::json!({
                "name": provider.name,
                "expected_version": provider.version,
                "observed_state_or_version": observed,
                "required": provider.required,
                "applicability": provider.applicability,
            })
        })
        .collect::<Vec<_>>();
    let provenance = serde_json::json!({
        "global_plan_sha256": global_plan_sha256,
        "execution_scope": capacity_execution_scope(shard, &global_plan_sha256),
        "revision": revision,
        "scenario_sha256": file_sha256(scenario_path)?,
        "generator": scenario.dataset.generator,
        "generator_sha256": file_sha256(&repository_root.join(&scenario.dataset.generator))?,
        "generator_version": "capacity-fixture-generator-v1.0.0",
        "manifest_file_sha256": scenario.capacity.strata.iter().map(|stratum| {
            serde_json::json!({"dataset_name": stratum.name, "file_sha256": stratum.file_sha256})
        }).collect::<Vec<_>>(),
        "accepted_outcome_contract": {
            "path": scenario.capacity.accepted_outcome_contract.path,
            "file_sha256": scenario.capacity.accepted_outcome_contract.file_sha256,
            "schema_version": accepted_outcomes.schema_version,
            "supersedes": accepted_outcomes.supersedes.contract,
        },
        "platform": {
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "cpu": cpu_description(),
        },
        "resolved_local_tools": {
            "rustc": command_text("rustc", &["--version", "--verbose"]),
            "cargo": command_text("cargo", &["--version"]),
            "python": command_text(if cfg!(windows) { "python" } else { "python3" }, &["--version"]),
            "node": command_text("node", &["--version"]),
            "providers": provider_tools,
        },
        "claim": "raw private observations only; no capacity, supported-limit, SLO, platform, or release claim",
    });
    let provenance_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&provenance)?));
    let mut writer = CapacityRawWriter::create(
        &evidence.path,
        evidence.guard(),
        execution_total,
        evidence_names.raw_file,
    )?;
    let mut expected = BTreeSet::new();
    let mut outcome_counts = BTreeMap::<String, u64>::new();
    let mut condition_scored_success =
        BTreeMap::<CapacityAggregationKey, CapacityScoredSuccess>::new();
    let mut condition_outcomes = BTreeMap::<CapacityAggregationKey, BTreeMap<String, u64>>::new();
    let mut deterministic_identities = BTreeMap::<CapacityDeterminismKey, String>::new();
    let mut campaign_failures = Vec::new();
    let mut ordinal = 0_u64;
    let mut global_matrix_row_index = 0_u64;

    for (stratum_index, manifest) in manifests.iter().enumerate() {
        let stratum = &scenario.capacity.strata[stratum_index];
        for profile in &scenario.profiles {
            for provider_condition in &scenario.capacity.provider_conditions {
                let resolved =
                    resolve_capacity_provider_condition(&stratum.name, provider_condition)?;
                let provider_members = resolved.members();
                for phase_name in &scenario.capacity.phases {
                    let row_index = global_matrix_row_index;
                    global_matrix_row_index += 1;
                    if shard.is_some_and(|value| !value.selects(row_index)) {
                        continue;
                    }
                    let phase = CapacityPhase::parse(phase_name)?;
                    let descriptor = phase.descriptor();
                    let lifecycle_policy = descriptor.lifecycle_identity;
                    let accepted_outcome =
                        if descriptor.catalogue_measurements && resolved.is_applicable() {
                            Some(capacity_accepted_outcome(
                                manifest,
                                &accepted_outcomes,
                                resolved.name(),
                                phase,
                            )?)
                        } else {
                            None
                        };
                    for repetition in 1..=scenario.procedure.repetitions {
                        let shared_lifecycle_id = descriptor.lifecycle_id(
                            &stratum.name,
                            &profile.name,
                            resolved.name(),
                            repetition,
                            "warm",
                            0,
                        )?;
                        if lifecycle_policy == CapacityLifecycleIdentityPolicy::SharedWarm {
                            if let Some(publisher) = memory_labels.as_mut() {
                                publisher.publish(&CapacityMemoryLabels::for_attempt(
                                    descriptor,
                                    &resolved,
                                    "warmup",
                                    ordinal + 1,
                                    execution_total,
                                )?)?;
                            }
                        }
                        let mut shared_prepared = (lifecycle_policy
                            == CapacityLifecycleIdentityPolicy::SharedWarm)
                            .then(|| {
                                executor.prepare(CapacityPreparationSpec {
                                    repository_root,
                                    scenario,
                                    manifest,
                                    provider_condition: &resolved,
                                    phase,
                                    lifecycle_id: &shared_lifecycle_id,
                                })
                            });
                        for (sample_class, sample_count) in [
                            ("warmup", scenario.procedure.warmups),
                            ("warm", scenario.procedure.warm_samples),
                        ] {
                            for sample_index in 1..=sample_count {
                                let lifecycle_id = descriptor.lifecycle_id(
                                    &stratum.name,
                                    &profile.name,
                                    resolved.name(),
                                    repetition,
                                    sample_class,
                                    sample_index,
                                )?;
                                if let Some(publisher) = memory_labels.as_mut() {
                                    publisher.publish(&CapacityMemoryLabels::for_attempt(
                                        descriptor,
                                        &resolved,
                                        sample_class,
                                        ordinal + 1,
                                        execution_total,
                                    )?)?;
                                }
                                let mut per_attempt_prepared = (lifecycle_policy
                                    == CapacityLifecycleIdentityPolicy::PerAttempt)
                                    .then(|| {
                                        executor.prepare(CapacityPreparationSpec {
                                            repository_root,
                                            scenario,
                                            manifest,
                                            provider_condition: &resolved,
                                            phase,
                                            lifecycle_id: &lifecycle_id,
                                        })
                                    });
                                let prepared = per_attempt_prepared
                                    .as_mut()
                                    .or(shared_prepared.as_mut())
                                    .expect("descriptor requires a lifecycle preparation");
                                let expected_configuration_hash =
                                    prepared.configuration_hash().map(str::to_owned);
                                retain_scheduled_capacity_attempt(
                                    executor,
                                    prepared,
                                    &mut writer,
                                    &mut expected,
                                    &mut outcome_counts,
                                    &mut condition_scored_success,
                                    &mut condition_outcomes,
                                    &mut deterministic_identities,
                                    &mut campaign_failures,
                                    &provenance_sha256,
                                    manifest,
                                    accepted_outcome,
                                    profile,
                                    CapacityAttemptSpec {
                                        evidence_kind: evidence_names.raw_kind,
                                        provider_condition: resolved.name(),
                                        provider_members: provider_members.clone(),
                                        expected_configuration_hash,
                                        phase: phase_name,
                                        repetition,
                                        sample_class,
                                        sample_index,
                                        lifecycle_id,
                                    },
                                )?;
                                ordinal += 1;
                            }
                        }
                        for sample_index in 1..=scenario.procedure.cold_samples {
                            let lifecycle_id = phase.descriptor().lifecycle_id(
                                &stratum.name,
                                &profile.name,
                                resolved.name(),
                                repetition,
                                "cold",
                                sample_index,
                            )?;
                            if let Some(publisher) = memory_labels.as_mut() {
                                publisher.publish(&CapacityMemoryLabels::for_attempt(
                                    descriptor,
                                    &resolved,
                                    "cold",
                                    ordinal + 1,
                                    execution_total,
                                )?)?;
                            }
                            let mut prepared = executor.prepare(CapacityPreparationSpec {
                                repository_root,
                                scenario,
                                manifest,
                                provider_condition: &resolved,
                                phase,
                                lifecycle_id: &lifecycle_id,
                            });
                            let expected_configuration_hash =
                                prepared.configuration_hash().map(str::to_owned);
                            retain_scheduled_capacity_attempt(
                                executor,
                                &mut prepared,
                                &mut writer,
                                &mut expected,
                                &mut outcome_counts,
                                &mut condition_scored_success,
                                &mut condition_outcomes,
                                &mut deterministic_identities,
                                &mut campaign_failures,
                                &provenance_sha256,
                                manifest,
                                accepted_outcome,
                                profile,
                                CapacityAttemptSpec {
                                    evidence_kind: evidence_names.raw_kind,
                                    provider_condition: resolved.name(),
                                    provider_members: provider_members.clone(),
                                    expected_configuration_hash,
                                    phase: phase_name,
                                    repetition,
                                    sample_class: "cold",
                                    sample_index,
                                    lifecycle_id,
                                },
                            )?;
                            ordinal += 1;
                            if ordinal == execution_total {
                                if let Some(publisher) = memory_labels.as_mut() {
                                    publisher.publish(&CapacityMemoryLabels::completed(
                                        descriptor,
                                        &resolved,
                                        "cold",
                                        execution_total,
                                    )?)?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if global_matrix_row_index != CAPACITY_GLOBAL_MATRIX_ROWS || ordinal != execution_total {
        return Err(AtlasError::Other(format!(
            "capacity scheduler selected {global_matrix_row_index} global rows and produced \
             {ordinal} attempts instead of the closed {execution_total}-attempt scope"
        )));
    }
    let mut raw = writer.finish(&expected, evidence.guard())?;
    let condition_summaries = condition_outcomes
        .into_iter()
        .map(|(key, outcomes)| {
            let scored = condition_scored_success.remove(&key).unwrap_or_default();
            let compiler_measurements = (!scored.compiler.is_empty()).then(|| {
                let truncated_true = scored
                    .compiler
                    .iter()
                    .filter(|measurement| measurement.truncated)
                    .count();
                let fallback_true = scored
                    .compiler
                    .iter()
                    .filter(|measurement| measurement.fallback)
                    .count();
                serde_json::json!({
                    "count": scored.compiler.len(),
                    "compiler_selected_records": nearest_rank_summary(
                        &scored.compiler.iter().map(|value| value.selected_records).collect::<Vec<_>>()
                    ),
                    "compiler_selected_source_bytes": nearest_rank_summary(
                        &scored.compiler.iter().map(|value| value.selected_source_bytes).collect::<Vec<_>>()
                    ),
                    "compiler_selected_estimated_tokens": nearest_rank_summary(
                        &scored.compiler.iter().map(|value| value.selected_estimated_tokens).collect::<Vec<_>>()
                    ),
                    "compiler_omitted_records": nearest_rank_summary(
                        &scored.compiler.iter().map(|value| value.omitted_records).collect::<Vec<_>>()
                    ),
                    "compiler_truncated": {
                        "true_count": truncated_true,
                        "false_count": scored.compiler.len() - truncated_true,
                    },
                    "compiler_work_units_consumed": nearest_rank_summary(
                        &scored.compiler.iter().map(|value| value.work_units_consumed).collect::<Vec<_>>()
                    ),
                    "compiler_fallback": {
                        "true_count": fallback_true,
                        "false_count": scored.compiler.len() - fallback_true,
                    },
                })
            });
            serde_json::json!({
                "condition": key,
                "outcome_counts": outcomes,
                "scored_success_atlas_runtime_ms": nearest_rank_summary(&scored.durations),
                "compiler_measurements": compiler_measurements,
            })
        })
        .collect::<Vec<_>>();
    let summary = serde_json::json!({
        "schema_version": "1.0.0",
        "kind": evidence_names.summary_kind,
        "provenance": provenance,
        "provenance_sha256": provenance_sha256,
        "raw_sha256": raw.sha256,
        "attempted_records": ordinal,
        "missing_records": 0,
        "duplicate_records": 0,
        "maximum_raw_records": execution_total,
        "outcome_counts": outcome_counts,
        "percentile_method": scenario.procedure.percentile_method,
        "aggregation_input": "validated_scored_success_only",
        "campaign_accepted": campaign_failures.is_empty(),
        "validation_failure_count": campaign_failures.len(),
        "validation_failures": campaign_failures,
        "compiler_measurement_fields": CAPACITY_COMPILER_MEASUREMENT_FIELDS,
        "condition_summaries": condition_summaries,
        "limitations": scenario.capacity.limitations,
        "claim": "no capacity, supported-limit, SLO, platform, or release claim",
    });
    let (summary_path, summary_file) = create_capacity_output(
        &evidence.path,
        evidence.guard(),
        evidence_names.summary_file,
    )?;
    let summary_bytes = serde_json::to_vec_pretty(&summary)?;
    let mut summary_file = finish_capacity_output(
        &evidence.path,
        evidence.guard(),
        summary_file,
        &summary_bytes,
    )?;
    let verified_raw_sha256 = sha256_file_handle(&mut raw.file)?;
    if verified_raw_sha256 != raw.sha256 {
        return Err(AtlasError::Other(
            "raw evidence digest changed before summary final validation".into(),
        ));
    }
    summary_file.seek(SeekFrom::Start(0))?;
    let mut verified_summary_bytes = Vec::new();
    summary_file.read_to_end(&mut verified_summary_bytes)?;
    let verified_summary: Value = serde_json::from_slice(&verified_summary_bytes)?;
    if verified_summary["raw_sha256"].as_str() != Some(raw.sha256.as_str()) {
        return Err(AtlasError::Other(
            "capacity summary does not bind the actual raw evidence digest".into(),
        ));
    }
    validate_capacity_directory_guard(&evidence.path, evidence.guard())?;
    let raw_path = raw.path.clone();
    if summary["campaign_accepted"] != true {
        return Err(AtlasError::Other(format!(
            "capacity campaign failed after retaining complete raw evidence at {} and summary at {}",
            raw_path.display(),
            summary_path.display()
        )));
    }
    Ok((raw_path, summary_path))
}

#[derive(Clone, Copy, Default)]
struct CatalogueCategoryMeasurement {
    durable_rows: u64,
    durable_bytes: u64,
    derived_rows: u64,
    derived_bytes: u64,
}

#[derive(Clone, Copy, Default)]
struct ProviderMeasurement {
    probes: u64,
    executions_run: u64,
    outputs: u64,
    output_bytes: u64,
    executions_reused: u64,
    required_failure: bool,
}

#[derive(Clone, Copy, Default)]
struct CompilerMeasurement {
    selected_records: u64,
    selected_source_bytes: u64,
    selected_estimated_tokens: u64,
    omitted_records: u64,
    truncated: bool,
    fallback: bool,
    work_units_consumed: u64,
}

struct CapacityMeasurement {
    degraded: bool,
    provider_outcome: Option<String>,
    provider: Option<ProviderMeasurement>,
    correctness: bool,
    deterministic_identity: String,
    observed_target_identity: Option<Value>,
    observed_evidence_counts: Option<Value>,
    accepted_outcome: Option<String>,
    ready_versus_truth: Option<String>,
    work_units: u64,
    eligible_files: Option<u64>,
    indexed_files: Option<u64>,
    parsed_files: Option<u64>,
    changed_files: Option<u64>,
    changed_bytes: Option<u64>,
    source_bytes: Option<u64>,
    compiler: Option<CompilerMeasurement>,
    catalogue_before: Option<CatalogueCategoryMeasurement>,
    catalogue_after: Option<CatalogueCategoryMeasurement>,
    catalogue_bytes_before: Option<u64>,
    catalogue_bytes_after: Option<u64>,
    atlas_runtime_duration_ms: u64,
    wall_duration_ms: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CapacityObservationPolicy {
    ReusableLifecycle,
    PreflightOneShot,
}

#[derive(Default)]
struct CapacityActionCounts {
    catalogue_initializations: u64,
    workspace_registrations: u64,
    reconciliations: u64,
    #[cfg(test)]
    production_actions: u64,
    #[cfg(test)]
    post_observation_resets: u64,
}

struct ProductionCapacityState {
    repository_root: PathBuf,
    scenario: BenchmarkScenario,
    manifest: Value,
    phase: CapacityPhase,
    provider_condition: ResolvedCapacityProviderCondition,
    lifecycle_id: String,
    _temporary: Option<tempfile::TempDir>,
    fixture_root: PathBuf,
    catalogue_path: PathBuf,
    config: Option<Config>,
    connection: Option<rusqlite::Connection>,
    workspace: Option<workspace::WorkspaceRecord>,
    lifecycle_failure: Option<String>,
    preparation_report: Option<discovery::ReconcileReport>,
    action_counts: CapacityActionCounts,
}

type CapacityConfigBuilder = fn(&ResolvedCapacityProviderCondition) -> Result<Config>;

struct ProductionCapacityExecutor {
    build_config: CapacityConfigBuilder,
}

impl Default for ProductionCapacityExecutor {
    fn default() -> Self {
        Self {
            build_config: capacity_config,
        }
    }
}

impl CapacityExecutor for ProductionCapacityExecutor {
    type Prepared = ProductionCapacityState;
    const EVIDENCE_NAMES: CapacityEvidenceNames = PRODUCTION_CAPACITY_EVIDENCE;

    fn prepare(
        &mut self,
        spec: CapacityPreparationSpec<'_>,
    ) -> CapacityPreparation<Self::Prepared> {
        match prepare_production_capacity_state_with(spec, self.build_config) {
            Ok(prepared) => CapacityPreparation::Ready(prepared),
            Err(error) => CapacityPreparation::Failed(failed_capacity_preparation(error)),
        }
    }

    fn execute(
        &mut self,
        prepared: &mut Self::Prepared,
        observation: CapacityRawObservation,
    ) -> CapacityRawObservation {
        execute_capacity_observation(prepared, observation)
    }
}

fn generate_capacity_fixture(
    repository_root: &Path,
    scenario: &BenchmarkScenario,
    manifest: &Value,
    fixture_root: &Path,
) -> Result<()> {
    let dataset_name = manifest["dataset_name"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest dataset_name absent".into()))?;
    if fixture_root.exists() {
        std::fs::remove_dir_all(fixture_root)?;
    }
    let fixture_manifest = fixture_root.join(&scenario.dataset.fixture_manifest);
    let generated = Command::new("python3")
        .arg(repository_root.join(&scenario.dataset.generator))
        .arg("--output")
        .arg(fixture_root)
        .arg("--capacity")
        .arg(dataset_name)
        .arg("--force")
        .output()?;
    if !generated.status.success() {
        return Err(AtlasError::Other(format!(
            "capacity fixture generation failed for {dataset_name}"
        )));
    }
    let generated_manifest: Value = serde_json::from_slice(&std::fs::read(&fixture_manifest)?)?;
    if &generated_manifest != manifest {
        return Err(AtlasError::InvalidConfig(format!(
            "{dataset_name} generated fixture identity differs from its validated immutable manifest"
        )));
    }
    std::fs::remove_file(fixture_manifest)?;
    std::fs::remove_file(fixture_root.join(".workspace-atlas-fixture-marker"))?;
    Ok(())
}

fn initialize_capacity_catalogue(state: &mut ProductionCapacityState) -> Result<()> {
    if state.connection.is_some() {
        return Ok(());
    }
    let config = state
        .config
        .as_ref()
        .ok_or_else(|| AtlasError::Other("capacity configuration absent".into()))?;
    let connection = catalogue::init_catalogue(&state.catalogue_path, config)?;
    state.action_counts.catalogue_initializations += 1;
    let workspace = workspace::register_workspace(
        &connection,
        &state.fixture_root,
        config,
        &state.catalogue_path,
        "1.0.0",
    )?;
    state.action_counts.workspace_registrations += 1;
    state.connection = Some(connection);
    state.workspace = Some(workspace);
    Ok(())
}

#[cfg(test)]
fn prepare_production_capacity_state(
    spec: CapacityPreparationSpec<'_>,
) -> Result<ReadyCapacityPreparation<ProductionCapacityState>> {
    prepare_production_capacity_state_with(spec, capacity_config)
}

fn prepare_production_capacity_state_with(
    spec: CapacityPreparationSpec<'_>,
    build_config: CapacityConfigBuilder,
) -> Result<ReadyCapacityPreparation<ProductionCapacityState>> {
    let descriptor = spec.phase.descriptor();
    let config = spec
        .provider_condition
        .is_applicable()
        .then(|| build_config(spec.provider_condition))
        .transpose()?;
    let configuration_hash = config.as_ref().map(Config::configuration_hash);
    if descriptor.preparation == CapacityPreparationPolicy::Stateless
        || !spec.provider_condition.is_applicable()
    {
        return Ok(ReadyCapacityPreparation {
            prepared: ProductionCapacityState {
                repository_root: spec.repository_root.to_path_buf(),
                scenario: spec.scenario.clone(),
                manifest: spec.manifest.clone(),
                phase: spec.phase,
                provider_condition: *spec.provider_condition,
                lifecycle_id: spec.lifecycle_id.to_string(),
                _temporary: None,
                fixture_root: PathBuf::new(),
                catalogue_path: PathBuf::new(),
                config: None,
                connection: None,
                workspace: None,
                lifecycle_failure: None,
                preparation_report: None,
                action_counts: CapacityActionCounts::default(),
            },
            configuration_hash,
        });
    }
    let temporary = tempfile::Builder::new()
        .prefix("atlas-capacity-lifecycle-")
        .tempdir()?;
    let fixture_root = temporary.path().join("fixture");
    let catalogue_path = temporary.path().join("catalogue.sqlite");
    generate_capacity_fixture(
        spec.repository_root,
        spec.scenario,
        spec.manifest,
        &fixture_root,
    )?;
    let fixture_root = canonicalize_owned_benchmark_root(&fixture_root)?;
    let mut state = ProductionCapacityState {
        repository_root: spec.repository_root.to_path_buf(),
        scenario: spec.scenario.clone(),
        manifest: spec.manifest.clone(),
        phase: spec.phase,
        provider_condition: *spec.provider_condition,
        lifecycle_id: spec.lifecycle_id.to_string(),
        _temporary: Some(temporary),
        fixture_root,
        catalogue_path,
        config,
        connection: None,
        workspace: None,
        lifecycle_failure: None,
        preparation_report: None,
        action_counts: CapacityActionCounts::default(),
    };
    match descriptor.preparation {
        CapacityPreparationPolicy::Stateless | CapacityPreparationPolicy::PerAttemptCatalogue => {}
        CapacityPreparationPolicy::PreparedCatalogue => {
            initialize_capacity_catalogue(&mut state)?;
        }
        CapacityPreparationPolicy::PreparedBaseline
        | CapacityPreparationPolicy::PreparedBaselineWithServing => {
            initialize_capacity_catalogue(&mut state)?;
            let baseline = discovery::reconcile(
                state.workspace.as_ref().expect("registered"),
                state.connection.as_ref().expect("initialized"),
                state.config.as_ref().expect("configured"),
            )?;
            state.action_counts.reconciliations += 1;
            if baseline.activation != "committed" || baseline.failed_file_count != 0 {
                return Err(AtlasError::Other(
                    baseline
                        .failure_message
                        .clone()
                        .unwrap_or_else(|| "baseline reconcile failed".to_string()),
                ));
            }
            state.preparation_report = Some(baseline);
            if descriptor.preparation == CapacityPreparationPolicy::PreparedBaselineWithServing {
                serving::build_serving_generation(
                    state.connection.as_ref().expect("initialized"),
                    state.workspace.as_ref().expect("registered"),
                )?;
            }
        }
    }
    Ok(ReadyCapacityPreparation {
        prepared: state,
        configuration_hash,
    })
}

fn reset_fixed_incremental_state(state: &mut ProductionCapacityState) -> Result<()> {
    generate_capacity_fixture(
        &state.repository_root,
        &state.scenario,
        &state.manifest,
        &state.fixture_root,
    )?;
    let report = discovery::reconcile(
        state
            .workspace
            .as_ref()
            .ok_or_else(|| AtlasError::Other("fixed incremental workspace absent".into()))?,
        state
            .connection
            .as_ref()
            .ok_or_else(|| AtlasError::Other("fixed incremental catalogue absent".into()))?,
        state
            .config
            .as_ref()
            .ok_or_else(|| AtlasError::Other("fixed incremental configuration absent".into()))?,
    )?;
    if report.activation != "committed" || report.failed_file_count != 0 {
        return Err(AtlasError::Other(
            "fixed incremental baseline restoration failed".into(),
        ));
    }
    Ok(())
}

fn release_capacity_catalogue(state: &mut ProductionCapacityState) -> Result<()> {
    state.workspace = None;
    drop(state.connection.take());
    for suffix in ["", "-wal", "-shm"] {
        let mut path = state.catalogue_path.as_os_str().to_os_string();
        path.push(suffix);
        match std::fs::remove_file(PathBuf::from(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reset_capacity_phase_state(state: &mut ProductionCapacityState) -> Result<()> {
    match state.phase.descriptor().reset {
        CapacityResetPolicy::RestoreFixedFixture => reset_fixed_incremental_state(state),
        CapacityResetPolicy::RemoveServing => {
            let connection = state.connection.as_ref().ok_or_else(|| {
                AtlasError::Other("capacity catalogue absent during reset".into())
            })?;
            let workspace = state.workspace.as_ref().ok_or_else(|| {
                AtlasError::Other("capacity workspace absent during reset".into())
            })?;
            let mut statement = connection.prepare(
                "SELECT serving_generation_id FROM serving_generation
                 WHERE workspace_id = ?1",
            )?;
            let serving_ids = statement
                .query_map([&workspace.workspace_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            for serving_id in serving_ids {
                serving::delete_serving_generation(connection, &serving_id)?;
            }
            Ok(())
        }
        CapacityResetPolicy::EnsureServing => {
            serving::build_serving_generation(
                state.connection.as_ref().ok_or_else(|| {
                    AtlasError::Other("ready catalogue absent during reset".into())
                })?,
                state.workspace.as_ref().ok_or_else(|| {
                    AtlasError::Other("ready workspace absent during reset".into())
                })?,
            )?;
            Ok(())
        }
        CapacityResetPolicy::ReleaseCatalogue => release_capacity_catalogue(state),
        CapacityResetPolicy::None => Ok(()),
    }
}

fn execute_capacity_observation(
    state: &mut ProductionCapacityState,
    observation: CapacityRawObservation,
) -> CapacityRawObservation {
    execute_capacity_observation_with_policy(
        state,
        observation,
        CapacityObservationPolicy::ReusableLifecycle,
    )
}

fn execute_capacity_observation_with_policy(
    state: &mut ProductionCapacityState,
    mut observation: CapacityRawObservation,
    policy: CapacityObservationPolicy,
) -> CapacityRawObservation {
    debug_assert_eq!(state.lifecycle_id, observation.lifecycle_id);
    if !state.provider_condition.is_applicable() {
        observation.outcome = "unsupported".to_string();
        observation.correctness = None;
        observation.required_provider_failure = Some(false);
        return observation;
    }
    if let Some(error) = state.lifecycle_failure.as_ref() {
        observation.outcome = if error.to_ascii_lowercase().contains("unavailable") {
            "absent".to_string()
        } else {
            "failed".to_string()
        };
        observation.correctness = Some(false);
        return observation;
    }
    #[cfg(test)]
    {
        state.action_counts.production_actions += 1;
    }
    match execute_capacity_action(state, &observation) {
        Ok(measurement) => {
            observation.outcome = measurement.provider_outcome.clone().unwrap_or_else(|| {
                if measurement.degraded {
                    "degraded".to_string()
                } else {
                    "success".to_string()
                }
            });
            observation.correctness = Some(measurement.correctness);
            observation.deterministic_identity = Some(measurement.deterministic_identity);
            observation.observed_target_identity = measurement.observed_target_identity;
            observation.observed_evidence_counts = measurement.observed_evidence_counts;
            observation.accepted_outcome = measurement.accepted_outcome;
            observation.ready_versus_truth = measurement.ready_versus_truth;
            observation.deterministic_work_units = Some(measurement.work_units);
            observation.eligible_files = measurement.eligible_files;
            observation.indexed_files = measurement.indexed_files;
            observation.parsed_files = measurement.parsed_files;
            observation.changed_files = measurement.changed_files;
            observation.changed_bytes = measurement.changed_bytes;
            observation.source_bytes = measurement.source_bytes;
            if let Some(compiler) = measurement.compiler {
                observation.compiler_selected_records = Some(compiler.selected_records);
                observation.compiler_selected_source_bytes = Some(compiler.selected_source_bytes);
                observation.compiler_selected_estimated_tokens =
                    Some(compiler.selected_estimated_tokens);
                observation.compiler_omitted_records = Some(compiler.omitted_records);
                observation.compiler_truncated = Some(compiler.truncated);
                observation.compiler_work_units_consumed = Some(compiler.work_units_consumed);
                observation.compiler_fallback = Some(compiler.fallback);
            }
            if let Some(provider) = measurement.provider {
                observation.provider_probes = Some(provider.probes);
                observation.provider_executions_run = Some(provider.executions_run);
                observation.provider_outputs = Some(provider.outputs);
                observation.provider_output_bytes = Some(provider.output_bytes);
                observation.provider_executions_reused = Some(provider.executions_reused);
                observation.required_provider_failure = Some(provider.required_failure);
            }
            observation.configuration_hash = state.config.as_ref().map(Config::configuration_hash);
            if let Some(before) = measurement.catalogue_before {
                observation.durable_catalogue_rows_before = Some(before.durable_rows);
                observation.durable_catalogue_bytes_before = Some(before.durable_bytes);
                observation.derived_catalogue_rows_before = Some(before.derived_rows);
                observation.derived_catalogue_bytes_before = Some(before.derived_bytes);
                observation.catalogue_rows_before =
                    Some(before.durable_rows.saturating_add(before.derived_rows));
            }
            if let Some(after) = measurement.catalogue_after {
                observation.durable_catalogue_rows_after = Some(after.durable_rows);
                observation.durable_catalogue_bytes_after = Some(after.durable_bytes);
                observation.derived_catalogue_rows_after = Some(after.derived_rows);
                observation.derived_catalogue_bytes_after = Some(after.derived_bytes);
                observation.catalogue_rows_after =
                    Some(after.durable_rows.saturating_add(after.derived_rows));
            }
            observation.catalogue_bytes_before = measurement.catalogue_bytes_before;
            observation.catalogue_bytes_after = measurement.catalogue_bytes_after;
            observation.atlas_runtime_duration_ms = Some(measurement.atlas_runtime_duration_ms);
            observation.wall_duration_ms = Some(measurement.wall_duration_ms);
        }
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            observation.outcome = if message.contains("timed out") || message.contains("timeout") {
                "timeout"
            } else if message.contains("cancel") {
                observation.cancellation = true;
                "cancelled"
            } else if message.contains("unsupported") {
                "unsupported"
            } else if message.contains("unavailable")
                || message.contains("not found")
                || message.contains("cannot find")
            {
                "absent"
            } else {
                "failed"
            }
            .to_string();
            observation.correctness = Some(false);
        }
    }
    let reset_required = policy == CapacityObservationPolicy::ReusableLifecycle
        || state.phase.descriptor().reset != CapacityResetPolicy::RestoreFixedFixture;
    if reset_required {
        #[cfg(test)]
        {
            state.action_counts.post_observation_resets += 1;
        }
        if let Err(error) = reset_capacity_phase_state(state) {
            observation.outcome = "failed".to_string();
            observation.correctness = Some(false);
            observation.cancellation = false;
            observation.wall_duration_ms = None;
            observation.atlas_runtime_duration_ms = None;
            state.lifecycle_failure = Some(error.to_string());
        }
    }
    observation
}

fn run_capacity_accepted_outcome_preflight(
    repository_root: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    manifests: &[Value],
    contract: &CapacityAcceptedOutcomeContract,
) -> Result<Value> {
    let profile = scenario
        .profiles
        .iter()
        .find(|profile| profile.name == contract.preflight_profile)
        .ok_or_else(|| {
            AtlasError::InvalidConfig(
                "capacity accepted-outcome preflight profile is absent".into(),
            )
        })?;
    let mut rows = Vec::new();
    for outcome in &contract.outcomes {
        let manifest = manifests
            .iter()
            .find(|manifest| manifest["dataset_name"] == outcome.dataset_name)
            .ok_or_else(|| {
                AtlasError::InvalidConfig(
                    "capacity accepted-outcome preflight manifest is absent".into(),
                )
            })?;
        let provider = resolve_capacity_provider_condition(
            &outcome.dataset_name,
            &outcome.provider_condition,
        )?;
        let phase_name = &outcome.preflight_phase;
        let phase = CapacityPhase::parse(phase_name)?;
        let lifecycle_id = phase.descriptor().lifecycle_id(
            &outcome.dataset_name,
            &profile.name,
            provider.name(),
            1,
            "warmup",
            1,
        )?;
        let mut prepared = prepare_production_capacity_state_with(
            CapacityPreparationSpec {
                repository_root,
                scenario,
                manifest,
                provider_condition: &provider,
                phase,
                lifecycle_id: &lifecycle_id,
            },
            capacity_config,
        )?;
        if prepared.prepared._temporary.is_none()
            || prepared.prepared.fixture_root.as_os_str().is_empty()
            || prepared
                .prepared
                .fixture_root
                .join("fixture_manifest.json")
                .exists()
            || prepared
                .prepared
                .fixture_root
                .join(".workspace-atlas-fixture-marker")
                .exists()
        {
            return Err(AtlasError::Other(
                "production accepted-outcome preflight did not use a clean disposable fixture"
                    .into(),
            ));
        }
        let empty = empty_capacity_observation(
            "accepted-outcome-preflight-v2",
            manifest,
            Some(outcome),
            profile,
            CapacityAttemptSpec {
                evidence_kind: PRODUCTION_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: prepared.configuration_hash.clone(),
                phase: phase.descriptor().name,
                repetition: 1,
                sample_class: "warmup",
                sample_index: 1,
                lifecycle_id,
            },
        )?;
        let observation = execute_capacity_observation_with_policy(
            &mut prepared.prepared,
            empty,
            CapacityObservationPolicy::PreflightOneShot,
        );
        let validation_failure = capacity_observation_failure(&observation, &mut BTreeMap::new());
        let matched = observation.outcome == outcome.expected_observation_state
            && observation.correctness == Some(true)
            && observation.observed_target_identity.as_ref() == Some(&outcome.target_identity)
            && observation.observed_evidence_counts.as_ref() == Some(&outcome.evidence_counts)
            && observation.accepted_outcome.as_deref()
                == Some(outcome.expected_accepted_outcome_hash.as_str())
            && observation.configuration_hash.as_deref()
                == Some(outcome.configuration_hash.as_str())
            && observation.expected_configuration_hash.as_deref()
                == Some(outcome.configuration_hash.as_str())
            && validation_failure.is_none();
        rows.push(serde_json::json!({
            "dataset_name": outcome.dataset_name,
            "manifest_sha256": outcome.manifest_sha256,
            "provider_condition": outcome.provider_condition,
            "provider_members": outcome.provider_members,
            "configuration_hash": outcome.configuration_hash,
            "truth_state": outcome.truth_state,
            "phase": phase_name,
            "expected_observation_state": outcome.expected_observation_state,
            "observed_observation_state": observation.outcome,
            "expected_evidence_counts": outcome.evidence_counts,
            "observed_evidence_counts": observation.observed_evidence_counts,
            "expected_target_identity": outcome.target_identity,
            "observed_target_identity": observation.observed_target_identity,
            "expected_accepted_outcome_hash": outcome.expected_accepted_outcome_hash,
            "observed_accepted_outcome_hash": observation.accepted_outcome,
            "correctness": observation.correctness,
            "eligible_files": observation.eligible_files,
            "indexed_files": observation.indexed_files,
            "parsed_files": observation.parsed_files,
            "changed_files": observation.changed_files,
            "changed_bytes": observation.changed_bytes,
            "source_bytes": observation.source_bytes,
            "provider_probes": observation.provider_probes,
            "provider_executions_run": observation.provider_executions_run,
            "provider_executions_reused": observation.provider_executions_reused,
            "required_provider_failure": observation.required_provider_failure,
            "validation_failure": validation_failure,
            "matched": matched,
        }));
        if !matched {
            return Err(AtlasError::Other(format!(
                "production accepted-outcome preflight mismatch for {}/{}/{}: {}",
                outcome.dataset_name,
                outcome.provider_condition,
                phase_name,
                serde_json::to_string(rows.last().expect("mismatch row retained"))?
            )));
        }
    }
    Ok(serde_json::json!({
        "schema_version": "2.0.0",
        "kind": "capacity-accepted-outcome-preflight",
        "revision": command_text("git", &["rev-parse", "HEAD"]),
        "scenario_sha256": file_sha256(scenario_path)?,
        "generator_sha256": file_sha256(&repository_root.join(&scenario.dataset.generator))?,
        "contract_file_sha256": scenario.capacity.accepted_outcome_contract.file_sha256,
        "clean_disposable_fixture_required": true,
        "production_one_attempt_per_provider_truth_state": true,
        "row_count": rows.len(),
        "all_matched": true,
        "rows": rows,
    }))
}

fn toml_string_array(values: &[&str]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn capacity_config(provider_condition: &ResolvedCapacityProviderCondition) -> Result<Config> {
    let mut text = String::from(
        "schema_version = \"1.1.0\"\n\
         [workspace]\n\
         display_name = \"atlas-capacity-fixture\"\n\
         [provider_runtime]\n\
         allow_shell = false\n\
         inherit_environment = false\n\
         allowed_environment = [\"PATH\", \"PATHEXT\", \"SYSTEMROOT\", \"TEMP\", \"TMP\", \"HOME\", \"USERPROFILE\"]\n\
         default_timeout_ms = 120000\n\
         graceful_cancel_ms = 1500\n\
         max_stdout_bytes = 1048576\n\
         max_stderr_bytes = 1048576\n\
         max_output_bytes = 536870912\n\
         temporary_root_policy = \"application_private\"\n\
         network_isolation_policy = \"best_effort_allowed\"\n\
         retain_raw_output = false\n",
    );
    for provider in provider_condition.providers() {
        text.push_str("[[providers]]\n");
        text.push_str(&format!(
            "name = \"{}\"\nversion = \"{}\"\nkind = \"{}\"\ntier = \"{}\"\nscope = \"{}\"\nenabled = true\nrequired = {}\npriority = {}\nlanguages = {}\nproject_markers = {}\nconfiguration = {{}}\n",
            provider.name,
            provider.version,
            provider.kind,
            provider.tier,
            provider.scope,
            provider.required,
            provider.priority,
            toml_string_array(provider.languages),
            toml_string_array(provider.project_markers),
        ));
        if let Some(command) = provider.command {
            text.push_str(&format!(
                "command = \"{command}\"\narguments = {}\nprobe_arguments = {}\ntimeout_ms = 120000\nmax_output_bytes = 536870912\n",
                toml_string_array(provider.arguments),
                toml_string_array(provider.probe_arguments),
            ));
        }
        if let Some(output_format) = provider.output_format {
            text.push_str(&format!("output_format = \"{output_format}\"\n"));
        }
    }
    Config::parse(&text)
}

fn capacity_provider_measurement(
    connection: &rusqlite::Connection,
    report: Option<&discovery::ReconcileReport>,
    provider_condition: &ResolvedCapacityProviderCondition,
) -> Result<(Option<String>, ProviderMeasurement)> {
    let Some(report) = report else {
        return Ok((None, ProviderMeasurement::default()));
    };
    let mut measurement = ProviderMeasurement {
        probes: u64::try_from(report.semantic_scopes_considered).unwrap_or(0),
        executions_run: u64::try_from(report.semantic_executions_run).unwrap_or(0),
        executions_reused: u64::try_from(report.semantic_executions_reused).unwrap_or(0),
        outputs: u64::try_from(report.semantic_documents_processed).unwrap_or(0),
        ..ProviderMeasurement::default()
    };
    let mut statement = connection.prepare(
        "SELECT d.provider_name, e.status, COALESCE(e.output_bytes, 0)
         FROM provider_execution e
         JOIN provider_descriptor d ON d.provider_key = e.provider_key
         WHERE e.generation_id = ?1
         ORDER BY d.provider_name, e.provider_execution_id",
    )?;
    let rows = statement.query_map([&report.candidate_generation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut required_outcome = None;
    let mut optional_non_success = false;
    for row in rows {
        let (name, status, output_bytes) = row?;
        measurement.output_bytes = measurement
            .output_bytes
            .saturating_add(u64::try_from(output_bytes).unwrap_or(0));
        let Some(descriptor) = provider_condition
            .providers()
            .find(|provider| provider.name == name)
        else {
            return Err(AtlasError::Other(format!(
                "provider execution {name} is outside the typed capacity provider set"
            )));
        };
        let outcome = match status.as_str() {
            "complete" => None,
            "planned" | "running" => Some("failed"),
            "timed_out" => Some("timeout"),
            "cancelled" => Some("cancelled"),
            "unavailable" => Some("absent"),
            "unsupported" => Some("unsupported"),
            "partial" => Some("degraded"),
            _ => Some("failed"),
        };
        if let Some(outcome) = outcome {
            if descriptor.required {
                measurement.required_failure = true;
                required_outcome.get_or_insert_with(|| outcome.to_string());
            } else {
                optional_non_success = true;
            }
        }
    }
    let outcome = required_outcome.or_else(|| optional_non_success.then(|| "degraded".to_string()));
    Ok((outcome, measurement))
}

fn catalogue_category_measurement(
    connection: &rusqlite::Connection,
) -> Result<CatalogueCategoryMeasurement> {
    let mut table_statement = connection.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut measurement = CatalogueCategoryMeasurement::default();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let rows: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
                row.get(0)
            })?;
        let bytes: i64 = connection.query_row(
            "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat
             WHERE name = ?1
                OR name IN (
                    SELECT name FROM sqlite_master
                    WHERE type = 'index' AND tbl_name = ?1
                )",
            [&table],
            |row| row.get(0),
        )?;
        let rows = u64::try_from(rows).unwrap_or(0);
        let bytes = u64::try_from(bytes).unwrap_or(0);
        let derived = matches!(
            table.as_str(),
            "serving_generation"
                | "symbol_serving_projection"
                | "relationship_serving_edge"
                | "serving_coverage_rollup"
        );
        if derived {
            measurement.derived_rows = measurement.derived_rows.saturating_add(rows);
            measurement.derived_bytes = measurement.derived_bytes.saturating_add(bytes);
        } else {
            measurement.durable_rows = measurement.durable_rows.saturating_add(rows);
            measurement.durable_bytes = measurement.durable_bytes.saturating_add(bytes);
        }
    }
    Ok(measurement)
}

fn catalogue_bytes(path: &Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| std::fs::metadata(format!("{}{suffix}", path.display())).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn canonical_json_sha256(value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn count_generation_rows(
    connection: &rusqlite::Connection,
    sql: &str,
    generation_id: &str,
) -> Result<u64> {
    let count: i64 = connection.query_row(sql, [generation_id], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn observed_capacity_identity(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    fixture_root: &Path,
    manifest: &Value,
) -> Result<(Value, Value, String, u64)> {
    let generation_id: String = connection.query_row(
        "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
        [&workspace.workspace_id],
        |row| row.get(0),
    )?;
    let symbols = count_generation_rows(
        connection,
        "SELECT COUNT(DISTINCT s.symbol_fact_id)
         FROM symbol_fact s
         JOIN extractor_run r ON r.extractor_run_id = s.extractor_run_id
         JOIN generation_file f ON f.revision_id = r.revision_id
         WHERE f.generation_id = ?1",
        &generation_id,
    )?;
    let relationships = count_generation_rows(
        connection,
        "SELECT COUNT(DISTINCT v.relationship_fact_id)
         FROM relationship_fact v
         JOIN extractor_run r ON r.extractor_run_id = v.extractor_run_id
         JOIN generation_file f ON f.revision_id = r.revision_id
         WHERE f.generation_id = ?1",
        &generation_id,
    )?;
    let effects = count_generation_rows(
        connection,
        "SELECT COUNT(DISTINCT e.effect_fact_id)
         FROM effect_fact e
         JOIN extractor_run r ON r.extractor_run_id = e.extractor_run_id
         JOIN generation_file f ON f.revision_id = r.revision_id
         WHERE f.generation_id = ?1",
        &generation_id,
    )?;
    let diagnostics = count_generation_rows(
        connection,
        "SELECT COUNT(*) FROM diagnostic WHERE generation_id = ?1",
        &generation_id,
    )?;
    let coverage = count_generation_rows(
        connection,
        "SELECT COUNT(*) FROM coverage_record WHERE generation_id = ?1",
        &generation_id,
    )?;
    let conflicts = count_generation_rows(
        connection,
        "SELECT COUNT(*) FROM evidence_conflict WHERE generation_id = ?1",
        &generation_id,
    )?;
    let source_bytes = count_generation_rows(
        connection,
        "SELECT COALESCE(SUM(r.byte_size), 0)
         FROM generation_file f
         JOIN file_revision r ON r.revision_id = f.revision_id
         WHERE f.generation_id = ?1 AND f.presence_state = 'present'",
        &generation_id,
    )?;
    let expected_path = manifest["target_identity"]["relative_path"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest target path absent".into()))?;
    let expected_symbol = manifest["target_identity"]["symbol"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest target symbol absent".into()))?;
    let mut target_statement = connection.prepare(
        "SELECT DISTINCT canonical_path, display_name
         FROM current_symbol
         WHERE workspace_id = ?1 AND generation_id = ?2 AND canonical_path = ?3
         ORDER BY canonical_path, display_name",
    )?;
    let target_associations = target_statement
        .query_map(
            rusqlite::params![workspace.workspace_id, generation_id, expected_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let matching = target_associations
        .iter()
        .filter(|(_, observed_symbol)| observed_symbol == expected_symbol)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(AtlasError::Other(format!(
            "active generation target association must contain exactly one {expected_path} / {expected_symbol} pair; observed {}",
            matching.len()
        )));
    }
    let (observed_path, observed_symbol) = matching[0];
    let target_identity = serde_json::json!({
        "relative_path": observed_path,
        "sha256": file_sha256(&fixture_root.join(observed_path))?,
        "symbol": observed_symbol,
    });
    let evidence_counts = serde_json::json!({
        "symbols": symbols,
        "relationships": relationships,
        "effects": effects,
        "diagnostics": diagnostics,
        "coverage": coverage,
        "conflicts": conflicts,
    });
    let accepted_outcome = canonical_json_sha256(&serde_json::json!({
        "dataset_name": manifest["dataset_name"],
        "evidence_counts": evidence_counts,
        "target_identity": target_identity,
    }))?;
    Ok((
        target_identity,
        evidence_counts,
        accepted_outcome,
        source_bytes,
    ))
}

fn ready_truth_identity(accepted_outcome: &str, equivalent: bool) -> Result<Option<String>> {
    if !equivalent {
        return Ok(None);
    }
    Ok(Some(canonical_json_sha256(&serde_json::json!({
        "accepted_outcome_hash": accepted_outcome,
        "equivalence": "ready-serving-equals-truth-fallback",
    }))?))
}

#[derive(Clone, Copy)]
struct CapacityChangeMeasurement {
    files: u64,
    bytes: u64,
}

fn apply_capacity_changes(root: &Path, manifest: &Value) -> Result<CapacityChangeMeasurement> {
    let changes = manifest["changed_set"]
        .as_array()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest changed_set absent".into()))?;
    let mut semantic_index = 0_u64;
    let mut measured_bytes = 0_u64;
    for change in changes {
        let relative = change["relative_path"]
            .as_str()
            .ok_or_else(|| AtlasError::InvalidConfig("changed path absent".into()))?;
        let operations = change["operation_classes"]
            .as_array()
            .ok_or_else(|| AtlasError::InvalidConfig("changed operations absent".into()))?;
        let has = |name: &str| operations.iter().any(|value| value.as_str() == Some(name));
        let declared_changed_bytes = change["changed_bytes"]
            .as_u64()
            .ok_or_else(|| AtlasError::InvalidConfig("changed byte count absent".into()))?;
        let source = root.join(relative);
        if has("delete") {
            let content = std::fs::read(&source)?;
            let actual_before = hex::encode(Sha256::digest(&content));
            if change["before_sha256"].as_str() != Some(actual_before.as_str()) {
                return Err(AtlasError::InvalidConfig(format!(
                    "fixed incremental operation for {relative} does not match immutable before_sha256"
                )));
            }
            std::fs::remove_file(&source)?;
            measured_bytes = measured_bytes.saturating_add(declared_changed_bytes);
            continue;
        }
        let mut content = std::fs::read(&source)?;
        let actual_before = hex::encode(Sha256::digest(&content));
        if change["before_sha256"].as_str() != Some(actual_before.as_str()) {
            return Err(AtlasError::InvalidConfig(format!(
                "fixed incremental operation for {relative} does not match immutable before_sha256"
            )));
        }
        if has("rename") {
            let replaced = String::from_utf8_lossy(&content)
                .replace("before", "after")
                .into_bytes();
            if replaced == content {
                content.extend_from_slice(b"// deterministic semantic rename edit\n");
            } else {
                content = replaced;
            }
        } else if has("semantic_edit") {
            semantic_index += 1;
            content.extend_from_slice(
                format!("// deterministic semantic capacity edit {semantic_index:05}\n").as_bytes(),
            );
        }
        let result_relative = change["result_relative_path"]
            .as_str()
            .ok_or_else(|| AtlasError::InvalidConfig("changed result path absent".into()))?;
        let destination = root.join(result_relative);
        if destination != source {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::remove_file(&source)?;
        }
        std::fs::write(&destination, &content)?;
        measured_bytes = measured_bytes.saturating_add(declared_changed_bytes);
        let actual = hex::encode(Sha256::digest(&content));
        if change["after_sha256"].as_str() != Some(actual.as_str()) {
            return Err(AtlasError::InvalidConfig(format!(
                "fixed incremental operation for {relative} does not match immutable after_sha256"
            )));
        }
    }
    let measured = CapacityChangeMeasurement {
        files: u64::try_from(changes.len())
            .map_err(|_| AtlasError::Other("changed file count exceeds u64".into()))?,
        bytes: measured_bytes,
    };
    if manifest["counts"]["changed_files"].as_u64() != Some(measured.files)
        || manifest["counts"]["changed_bytes"].as_u64() != Some(measured.bytes)
    {
        return Err(AtlasError::InvalidConfig(
            "applied fixed incremental files/bytes differ from the immutable manifest".into(),
        ));
    }
    Ok(measured)
}

fn route_request(
    target_path: &str,
    target_symbol: &str,
    generation_id: &str,
    route: Route,
) -> Result<ContextRouteRequest> {
    let paths = if route == Route::Direct {
        Vec::new()
    } else {
        vec![target_path.to_string()]
    };
    let symbols = if route == Route::Direct {
        Vec::new()
    } else {
        vec![target_symbol.to_string()]
    };
    Ok(ContextRouteRequest {
        schema_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        normalized_task_hash: identity_hash(&[target_path, target_symbol, "capacity-route"]),
        declared_kind: Some(TaskKind::BugFix),
        classifier_kind: TaskKind::BugFix,
        classifier_version: TASK_CLASSIFIER_VERSION.to_string(),
        explicit_seeds: SeedSummary::from_targets(&paths, &symbols)
            .map_err(|error| AtlasError::Other(error.to_string()))?,
        caller_capabilities: BTreeMap::from([(
            RequiredCapability::IdentityLookup,
            if route == Route::Direct {
                CallerCapabilityState::Satisfied
            } else {
                CallerCapabilityState::Unsatisfied
            },
        )]),
        atlas_intent: if route == Route::Direct {
            AtlasIntent::None
        } else {
            AtlasIntent::Require
        },
        route_floor: route,
        route_ceiling: route,
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
        deep_budget: None,
    })
}

fn capacity_governor_request(
    profile: &ProfileScenario,
    target_path: &str,
    target_symbol: &str,
    floor: Route,
    ceiling: Route,
) -> Result<GovernorRunRequest> {
    let caller_capabilities = if ceiling == Route::AtlasDeep {
        BTreeMap::from([
            (
                RequiredCapability::IdentityLookup,
                CallerCapabilityState::Unsatisfied,
            ),
            (
                RequiredCapability::ValidationPlan,
                CallerCapabilityState::Unsatisfied,
            ),
        ])
    } else {
        BTreeMap::from([(
            RequiredCapability::IdentityLookup,
            CallerCapabilityState::Unsatisfied,
        )])
    };
    let deep_limits = if ceiling == Route::AtlasDeep {
        Some(GovernorDeepLimits {
            max_records: profile.records,
            max_source_bytes: profile.source_bytes,
            max_estimated_tokens: profile.estimated_tokens,
            max_relationship_depth: u32::try_from(profile.relationship_depth).map_err(|_| {
                AtlasError::InvalidConfig("capacity profile relationship depth exceeds u32".into())
            })?,
            max_work_units: profile.work_units,
            uncertainty_reserve_percent: u8::try_from(profile.uncertainty_reserve_percent)
                .map_err(|_| {
                    AtlasError::InvalidConfig(
                        "capacity profile uncertainty reserve exceeds u8".into(),
                    )
                })?,
        })
    } else {
        None
    };
    Ok(GovernorRunRequest {
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
            task: format!("Inspect {target_path}::{target_symbol} capacity behavior"),
            declared_kind: Some(TaskKind::BugFix),
            path_targets: Vec::new(),
            symbol_targets: vec![target_symbol.to_string()],
            caller_capabilities,
            atlas_intent: AtlasIntent::Allow,
            route_floor: floor,
            route_ceiling: ceiling,
            legacy_task_session_id: None,
            deep_limits,
            materialize_source: false,
            max_materialized_bytes: None,
        },
    })
}

fn durable_compiler_state_counts(connection: &rusqlite::Connection) -> Result<(u64, u64, u64)> {
    let count = |table: &str| -> Result<u64> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let value = connection.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
        u64::try_from(value)
            .map_err(|_| AtlasError::Other(format!("{table} row count is negative")))
    };
    Ok((
        count("task_session")?,
        count("context_ir")?,
        count("context_use_event")?,
    ))
}

fn observed_capacity_route_symbol_key(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    canonical_path: &str,
    display_name: &str,
) -> Result<String> {
    let mut statement = connection.prepare(
        "SELECT canonical_symbol_key
         FROM current_symbol
         WHERE workspace_id = ?1
           AND generation_id = (
               SELECT active_generation_id FROM workspace WHERE workspace_id = ?1
           )
           AND canonical_path = ?2
           AND display_name = ?3
         ORDER BY canonical_symbol_key",
    )?;
    let keys = statement
        .query_map(
            rusqlite::params![workspace.workspace_id, canonical_path, display_name],
            |row| row.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    keys.into_iter().next().ok_or_else(|| {
        AtlasError::Other("observed route target does not resolve to a current symbol key".into())
    })
}

fn semantic_deep_context_hash(
    context: &workspace_atlas::context_ir::DeepContextIrV2,
) -> Result<String> {
    let mut value = serde_json::to_value(context)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| AtlasError::Other("deep Context IR must serialize as an object".into()))?;
    object.remove("context_id");
    object.remove("context_hash");
    let workspace = object
        .get_mut("workspace")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AtlasError::Other("deep Context IR workspace is absent".into()))?;
    workspace.remove("workspace_id");
    workspace.remove("generation_id");
    workspace.remove("generation_sequence");
    let task = object
        .get_mut("task")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AtlasError::Other("deep Context IR task is absent".into()))?;
    task.remove("task_session_id");
    let temporal = object
        .get_mut("temporal_constraints")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            AtlasError::Other("deep Context IR temporal constraints are absent".into())
        })?;
    temporal.remove("current_generation_id");
    temporal.remove("baseline_generation_id");
    let lease = object
        .get_mut("evidence_lease")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AtlasError::Other("deep Context IR evidence lease is absent".into()))?;
    lease.remove("generation_id");
    canonical_json_sha256(&value)
}

fn semantic_context_hash(context: &workspace_atlas::context_ir::ContextIr) -> Result<String> {
    // Workspace, generation, task-session and context identifiers vary across
    // deliberately fresh cold preparations. Compare returned semantic content
    // while excluding lifecycle generation labels and transient fallback state.
    let evidence = |value: &workspace_atlas::context_ir::IrEvidence| {
        serde_json::json!({
            "state": value.state,
            "confidence": value.confidence,
            "provider_fingerprint": value.provider_fingerprint,
            "source_revision_hash": value.source_revision_hash,
            "preferred": value.preferred,
            "alternative_count": value.alternative_count,
        })
    };
    let working_set = context
        .working_set
        .iter()
        .map(|item| {
            serde_json::json!({
                "item_id": item.item_id,
                "entity_kind": item.entity_kind,
                "entity_id": item.entity_id,
                "role": item.role,
                "selection_reason": item.selection_reason,
                "origin_id": item.origin_id,
                "distance": item.distance,
                "rank": item.rank,
                "evidence": evidence(&item.evidence),
                "cost": item.cost,
                "source": item.source,
            })
        })
        .collect::<Vec<_>>();
    let relationships = context
        .relationships
        .iter()
        .map(|item| {
            serde_json::json!({
                "relationship_id": item.relationship_id,
                "source_entity_id": item.source_entity_id,
                "target_entity_id": item.target_entity_id,
                "relationship_type": item.relationship_type,
                "resolution_state": item.resolution_state,
                "evidence": evidence(&item.evidence),
            })
        })
        .collect::<Vec<_>>();
    let effects = context
        .effects
        .iter()
        .map(|item| {
            serde_json::json!({
                "subject_entity_id": item.subject_entity_id,
                "effect_type": item.effect_type,
                "phase": item.phase,
                "evidence": evidence(&item.evidence),
            })
        })
        .collect::<Vec<_>>();
    let semantic = serde_json::json!({
        "policy": context.policy,
        "working_set": working_set,
        "relationships": relationships,
        "effects": effects,
        "coverage": context.coverage,
        "omissions": context.omissions,
        "validation_plan": context.validation_plan,
        "cost": {
            "selected_records": context.cost.selected_records,
            "selected_source_bytes": context.cost.selected_source_bytes,
            "selected_estimated_tokens": context.cost.selected_estimated_tokens,
        },
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&semantic)?)))
}

fn compile_capacity_context(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    scenario: &BenchmarkScenario,
    observation: &CapacityRawObservation,
    target_path: &str,
) -> Result<workspace_atlas::context_ir::ContextIr> {
    let task_hash = identity_hash(&[
        &observation.dataset_name,
        &observation.profile,
        &observation.provider_condition,
        "capacity-task",
    ]);
    let goal_hash = identity_hash(&[
        &observation.dataset_name,
        &observation.profile,
        &observation.provider_condition,
        "capacity-goal",
    ]);
    let generation_id: String = connection.query_row(
        "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
        [&workspace.workspace_id],
        |row| row.get(0),
    )?;
    let session = create_task_session(
        connection,
        workspace,
        &generation_id,
        &task_hash,
        &goal_hash,
        RawTaskRetention::None,
        None,
        TaskKind::BugFix,
        CONTEXT_SCHEMA_VERSION,
    )?;
    let request = profile_request(scenario, &observation.profile, target_path)?;
    compile_context_ir(
        connection,
        workspace,
        &session.task_session_id,
        &session.task_hash,
        &session.normalized_goal_hash,
        TaskKind::BugFix,
        TaskKindSource::Declared,
        None,
        &request,
    )
}

fn compiler_measurement(context: &workspace_atlas::context_ir::ContextIr) -> CompilerMeasurement {
    CompilerMeasurement {
        selected_records: u64::try_from(context.cost.selected_records).unwrap_or(0),
        selected_source_bytes: u64::try_from(context.cost.selected_source_bytes).unwrap_or(0),
        selected_estimated_tokens: u64::try_from(context.cost.selected_estimated_tokens)
            .unwrap_or(0),
        omitted_records: u64::try_from(context.omissions.omitted).unwrap_or(0),
        truncated: context.omissions.truncated,
        fallback: context.status.serving_fallback,
        // Legacy planner-v1.1.0 charges one deterministic work unit per
        // candidate actually examined. Never substitute the profile ceiling.
        work_units_consumed: u64::try_from(context.omissions.candidates_considered).unwrap_or(0),
    }
}

fn deep_compiler_measurement(
    context: &workspace_atlas::context_ir::DeepContextIrV2,
    fallback: bool,
) -> CompilerMeasurement {
    CompilerMeasurement {
        selected_records: context.cost.selected_records,
        selected_source_bytes: context.cost.selected_source_bytes,
        selected_estimated_tokens: context.cost.selected_estimated_tokens,
        omitted_records: context.omissions.omitted,
        truncated: context.omissions.truncated,
        fallback,
        work_units_consumed: context.cost.work_units_consumed,
    }
}

fn execute_capacity_action(
    state: &mut ProductionCapacityState,
    observation: &CapacityRawObservation,
) -> Result<CapacityMeasurement> {
    let descriptor = state.phase.descriptor();
    let phase = descriptor.dispatch;
    if descriptor.dispatch == CapacityPhase::CapabilityDiscovery {
        let started = Instant::now();
        let capabilities = discover_context_capabilities();
        capabilities
            .validate()
            .map_err(|error| AtlasError::Other(error.to_string()))?;
        let identity = hex::encode(Sha256::digest(serde_json::to_vec(&capabilities)?));
        let elapsed = duration_millis(started.elapsed());
        return Ok(CapacityMeasurement {
            degraded: false,
            provider_outcome: None,
            provider: None,
            correctness: true,
            deterministic_identity: identity,
            observed_target_identity: None,
            observed_evidence_counts: None,
            accepted_outcome: None,
            ready_versus_truth: None,
            work_units: 0,
            eligible_files: None,
            indexed_files: None,
            parsed_files: None,
            changed_files: None,
            changed_bytes: None,
            source_bytes: None,
            compiler: None,
            catalogue_before: None,
            catalogue_after: None,
            catalogue_bytes_before: None,
            catalogue_bytes_after: None,
            atlas_runtime_duration_ms: elapsed,
            wall_duration_ms: elapsed,
        });
    }
    let target_path = state.manifest["target_identity"]["relative_path"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest target path absent".into()))?
        .to_string();
    let target_symbol = state.manifest["target_identity"]["symbol"]
        .as_str()
        .ok_or_else(|| AtlasError::InvalidConfig("manifest target symbol absent".into()))?
        .to_string();
    if descriptor.dispatch == CapacityPhase::DirectRouteCompiler {
        let started = Instant::now();
        let request = route_request(
            &target_path,
            &target_symbol,
            observation.manifest_sha256.as_str(),
            Route::Direct,
        )?;
        let decision =
            decide_context_route(request).map_err(|error| AtlasError::Other(error.to_string()))?;
        let elapsed = duration_millis(started.elapsed());
        return Ok(CapacityMeasurement {
            degraded: false,
            provider_outcome: None,
            provider: None,
            correctness: decision.initial_route == Route::Direct,
            deterministic_identity: decision.decision_hash,
            observed_target_identity: None,
            observed_evidence_counts: None,
            accepted_outcome: None,
            ready_versus_truth: None,
            work_units: 0,
            eligible_files: None,
            indexed_files: None,
            parsed_files: None,
            changed_files: None,
            changed_bytes: None,
            source_bytes: None,
            compiler: None,
            catalogue_before: None,
            catalogue_after: None,
            catalogue_bytes_before: None,
            catalogue_bytes_after: None,
            atlas_runtime_duration_ms: elapsed,
            wall_duration_ms: elapsed,
        });
    }

    let action_started = Instant::now();
    initialize_capacity_catalogue(state)?;
    let config = state.config.as_ref().expect("catalogue phase configured");
    let connection = state.connection.as_mut().expect("catalogue initialized");
    let workspace = state.workspace.as_ref().expect("workspace registered");
    let catalogue_before = catalogue_category_measurement(connection)?;
    let catalogue_bytes_before = catalogue_bytes(&state.catalogue_path);
    let timed_started = Instant::now();
    let mut report = state.preparation_report.clone();
    let mut compiler = None;
    let mut phase_correctness;
    let phase_identity;
    let mut measured_runtime = None;
    let mut applied_change = None;
    let mut serving_to_delete = None;
    let mut route_work_units = None;

    match phase {
        CapacityPhase::ColdInitializationReconcile | CapacityPhase::NoChangeReconcile => {
            let current = discovery::reconcile(workspace, connection, config)?;
            state.action_counts.reconciliations += 1;
            let expected_eligible = state.manifest["counts"]["eligible_files"]
                .as_i64()
                .unwrap_or(-1);
            let expected_indexed = state.manifest["counts"]["indexed_files"]
                .as_i64()
                .unwrap_or(-1);
            let no_change_ok = descriptor.reconcile_measurements
                != CapacityReconcileMeasurements::NoChange
                || (current.parsed_file_count == 0
                    && current.renamed_file_count == 0
                    && current.deleted_file_count == 0);
            phase_correctness = current.activation == "committed"
                && current.eligible_file_count == expected_eligible
                && current.indexed_file_count == expected_indexed
                && current.failed_file_count == 0
                && no_change_ok;
            phase_identity = current.source_tree_hash.clone();
            report = Some(current);
        }
        CapacityPhase::FixedIncrementalReconcile => {
            let change = apply_capacity_changes(&state.fixture_root, &state.manifest)?;
            let incremental_started = Instant::now();
            let current = discovery::reconcile(workspace, connection, config)?;
            measured_runtime = Some(duration_millis(incremental_started.elapsed()));
            let non_deleted = state.manifest["changed_set"]
                .as_array()
                .expect("validated changed set")
                .iter()
                .filter(|entry| {
                    !entry["operation_classes"]
                        .as_array()
                        .is_some_and(|classes| classes.iter().any(|class| class == "delete"))
                })
                .count();
            phase_correctness = current.activation == "committed"
                && current.failed_file_count == 0
                && current.renamed_file_count == 0
                && current.deleted_file_count == 2
                && current.parsed_file_count == i64::try_from(non_deleted).unwrap_or(-1)
                && (current.semantic_scopes_considered
                    == current.semantic_executions_run + current.semantic_executions_reused
                    || (current.semantic_degraded
                        && matches!(
                            state.provider_condition.outcome_policy,
                            CapacityProviderOutcomePolicy::Optional
                                | CapacityProviderOutcomePolicy::RequiredWithOptional
                        )));
            phase_identity = current.source_tree_hash.clone();
            applied_change = Some(change);
            report = Some(current);
        }
        CapacityPhase::ReadyServingQueryCompiler => {
            let timed = Instant::now();
            let find = cli::build_find_output(
                &state.fixture_root,
                &target_symbol,
                10,
                Some(&state.catalogue_path),
            )?;
            let ready = compile_capacity_context(
                connection,
                workspace,
                &state.scenario,
                observation,
                &target_path,
            )?;
            measured_runtime = Some(duration_millis(timed.elapsed()));
            let serving_report = serving::build_serving_generation(connection, workspace)?;
            serving::delete_serving_generation(connection, &serving_report.serving_generation_id)?;
            let truth = compile_capacity_context(
                connection,
                workspace,
                &state.scenario,
                observation,
                &target_path,
            )?;
            serving::build_serving_generation(connection, workspace)?;
            let ready_identity = semantic_context_hash(&ready)?;
            let truth_identity = semantic_context_hash(&truth)?;
            phase_correctness = !ready.status.serving_fallback
                && truth.status.serving_fallback
                && find
                    .results
                    .iter()
                    .any(|result| result.canonical_path == target_path)
                && ready_identity == truth_identity;
            phase_identity = identity_hash(&[
                &hex::encode(Sha256::digest(serde_json::to_vec(&find)?)),
                &ready_identity,
            ]);
            compiler = Some(compiler_measurement(&ready));
        }
        CapacityPhase::TruthFallbackQueryCompiler => {
            let timed = Instant::now();
            let find = cli::build_find_output(
                &state.fixture_root,
                &target_symbol,
                10,
                Some(&state.catalogue_path),
            )?;
            let truth = compile_capacity_context(
                connection,
                workspace,
                &state.scenario,
                observation,
                &target_path,
            )?;
            measured_runtime = Some(duration_millis(timed.elapsed()));
            let serving_report = serving::build_serving_generation(connection, workspace)?;
            let ready = compile_capacity_context(
                connection,
                workspace,
                &state.scenario,
                observation,
                &target_path,
            )?;
            let truth_identity = semantic_context_hash(&truth)?;
            let ready_identity = semantic_context_hash(&ready)?;
            phase_correctness = truth.status.serving_fallback
                && !ready.status.serving_fallback
                && find
                    .results
                    .iter()
                    .any(|result| result.canonical_path == target_path)
                && ready_identity == truth_identity;
            phase_identity = identity_hash(&[
                &hex::encode(Sha256::digest(serde_json::to_vec(&find)?)),
                &truth_identity,
            ]);
            compiler = Some(compiler_measurement(&truth));
            serving_to_delete = Some(serving_report.serving_generation_id);
        }
        CapacityPhase::LightRouteCompiler => {
            let (live_target, _, _, _) = observed_capacity_identity(
                connection,
                workspace,
                &state.fixture_root,
                &state.manifest,
            )?;
            let live_path = live_target["relative_path"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("observed LIGHT target path is absent".into()))?;
            let live_symbol = observed_capacity_route_symbol_key(
                connection,
                workspace,
                live_path,
                live_target["symbol"].as_str().ok_or_else(|| {
                    AtlasError::Other("observed LIGHT target symbol is absent".into())
                })?,
            )?;
            let live_sha256 = live_target["sha256"].as_str().ok_or_else(|| {
                AtlasError::Other("observed LIGHT source digest is absent".into())
            })?;
            let profile = state
                .scenario
                .profiles
                .iter()
                .find(|profile| profile.name == observation.profile)
                .ok_or_else(|| AtlasError::InvalidConfig("capacity profile is absent".into()))?;
            let durable_before = durable_compiler_state_counts(connection)?;
            let output = run_catalogue_governor(
                capacity_governor_request(
                    profile,
                    live_path,
                    &live_symbol,
                    Route::AtlasLight,
                    Route::AtlasLight,
                )?,
                connection,
                workspace,
            )
            .map_err(|error| AtlasError::Other(error.to_string()))?;
            output
                .execution
                .validate()
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            let (attempts, counters, operations) = match &output.execution {
                ContextExecution::Completed {
                    attempts,
                    final_route: Route::AtlasLight,
                    counters,
                    payload: ContextPayload::LightBundle { operations },
                    ..
                } => (attempts, counters, operations),
                other => {
                    return Err(AtlasError::Other(format!(
                        "target-bounded LIGHT capacity execution did not complete: {other:#?}"
                    )))
                }
            };
            let payload_records = operations.iter().try_fold(0_u64, |total, operation| {
                let count = match operation {
                    workspace_atlas::context_route::LightOperationResult::Query {
                        record_count,
                        ..
                    } => *record_count,
                    workspace_atlas::context_route::LightOperationResult::SourceReference {
                        reference_count,
                        ..
                    } => *reference_count,
                };
                total
                    .checked_add(count)
                    .ok_or_else(|| AtlasError::Other("LIGHT result count overflow".into()))
            })?;
            let light_results = output
                .light_results
                .as_ref()
                .ok_or_else(|| AtlasError::Other("completed LIGHT results are absent".into()))?;
            let target_observed = light_results.query.as_ref().is_some_and(|queries| {
                queries.iter().any(|query| {
                    query.identity_matches.iter().any(|identity| {
                        identity.canonical_path == live_path && identity.content_hash == live_sha256
                    })
                })
            });
            let source_observed = light_results.source_references.is_none();
            phase_correctness = attempts.len() == 1
                && attempts[0].route == Route::AtlasLight
                && attempts[0].state == AttemptState::Completed
                && attempts[0].deficits.is_empty()
                && attempts[0].counters == *counters
                && counters.atlas_calls == 1
                && counters.records == payload_records
                && counters.source_bytes == 0
                && counters.estimated_tokens == 0
                && counters.work_units == 1
                && target_observed
                && source_observed
                && output.deep_context_ir.is_none()
                && durable_compiler_state_counts(connection)? == durable_before;
            phase_identity = identity_hash(&[
                phase.descriptor().name,
                &hex::encode(Sha256::digest(serde_json::to_vec(light_results)?)),
                &counters.atlas_calls.to_string(),
                &counters.records.to_string(),
                &counters.work_units.to_string(),
            ]);
            route_work_units = Some(counters.work_units);
        }
        CapacityPhase::ProgressiveDeepRouteCompiler => {
            let (live_target, _, _, _) = observed_capacity_identity(
                connection,
                workspace,
                &state.fixture_root,
                &state.manifest,
            )?;
            let live_path = live_target["relative_path"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("observed DEEP target path is absent".into()))?;
            let live_symbol = observed_capacity_route_symbol_key(
                connection,
                workspace,
                live_path,
                live_target["symbol"].as_str().ok_or_else(|| {
                    AtlasError::Other("observed DEEP target symbol is absent".into())
                })?,
            )?;
            let live_sha256 = live_target["sha256"]
                .as_str()
                .ok_or_else(|| AtlasError::Other("observed DEEP source digest is absent".into()))?;
            let profile = state
                .scenario
                .profiles
                .iter()
                .find(|profile| profile.name == observation.profile)
                .ok_or_else(|| AtlasError::InvalidConfig("capacity profile is absent".into()))?;
            let progressive_request = capacity_governor_request(
                profile,
                live_path,
                &live_symbol,
                Route::Direct,
                Route::AtlasDeep,
            )?;
            let primary_truth_fallback = connection.query_row(
                "SELECT COUNT(*) FROM serving_generation WHERE workspace_id = ?1",
                [&workspace.workspace_id],
                |row| row.get::<_, i64>(0),
            )? == 0;
            let durable_before = durable_compiler_state_counts(connection)?;
            let progressive_started = Instant::now();
            let progressive =
                run_catalogue_governor(progressive_request.clone(), connection, workspace)
                    .map_err(|error| AtlasError::Other(error.to_string()))?;
            measured_runtime = Some(duration_millis(progressive_started.elapsed()));
            progressive
                .execution
                .validate()
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            let progressive_ir = progressive.deep_context_ir.as_ref().ok_or_else(|| {
                AtlasError::Other(format!(
                    "progressive DEEP Context IR is absent: {:#?}",
                    progressive.execution
                ))
            })?;
            let (attempts, counters, payload_hash, satisfied, final_deficits, terminal_reason) =
                match &progressive.execution {
                    ContextExecution::Partial {
                        attempts,
                        final_route: Route::AtlasDeep,
                        payload: ContextPayload::DeepContextIr { result },
                        satisfied_capabilities,
                        deficits,
                        counters,
                        terminal_reason,
                        ..
                    } => (
                        attempts,
                        counters,
                        result.context_hash.as_str(),
                        satisfied_capabilities,
                        deficits,
                        terminal_reason,
                    ),
                    other => {
                        return Err(AtlasError::Other(format!(
                            "progressive DEEP capacity execution had the wrong terminal state: {other:#?}"
                        )))
                    }
                };
            let direct_deficits = vec![
                RouteDeficit::Capability {
                    capability: RequiredCapability::IdentityLookup,
                    reason: CapabilityDeficitReason::Unavailable,
                },
                RouteDeficit::Capability {
                    capability: RequiredCapability::ValidationPlan,
                    reason: CapabilityDeficitReason::Unavailable,
                },
            ];
            let light_deficits = vec![RouteDeficit::Capability {
                capability: RequiredCapability::ValidationPlan,
                reason: CapabilityDeficitReason::Unavailable,
            }];
            let attempts_exact = attempts.len() == 3
                && attempts[0].route == Route::Direct
                && attempts[0].state == AttemptState::Partial
                && attempts[0].deficits == direct_deficits
                && attempts[0].counters == ContextExecutionCounters::default()
                && attempts[1].route == Route::AtlasLight
                && attempts[1].state == AttemptState::Partial
                && attempts[1].deficits == light_deficits
                && attempts[1].counters.atlas_calls == 1
                && attempts[1].counters.records > 0
                && attempts[1].counters.source_bytes == 0
                && attempts[1].counters.estimated_tokens == 0
                && attempts[1].counters.work_units == 1
                && attempts[2].route == Route::AtlasDeep
                && attempts[2].state == AttemptState::Partial
                && attempts[2].deficits == light_deficits
                && attempts[2].counters.atlas_calls == 1
                && attempts[2].counters.records == progressive_ir.cost.selected_records
                && attempts[2].counters.source_bytes == progressive_ir.cost.selected_source_bytes
                && attempts[2].counters.estimated_tokens
                    == progressive_ir.cost.selected_estimated_tokens
                && attempts[2].counters.work_units == progressive_ir.cost.work_units_consumed
                && satisfied.as_slice() == [RequiredCapability::IdentityLookup]
                && final_deficits == &light_deficits
                && *terminal_reason == TerminalReason::RouteCeilingReached
                && counters.atlas_calls == 2
                && counters.work_units
                    == attempts[1]
                        .counters
                        .work_units
                        .saturating_add(progressive_ir.cost.work_units_consumed);
            let source_observed = progressive_ir.working_set.iter().any(|item| {
                item.source.as_ref().is_some_and(|source| {
                    source.canonical_path == live_path
                        && source.whole_file_sha256 == live_sha256
                        && source.observed_sha256.as_deref() == Some(live_sha256)
                        && source.revalidated_before_seal
                })
            });

            let mut direct_request = progressive_request.clone();
            direct_request.semantic.route_floor = Route::AtlasDeep;
            let direct = run_catalogue_governor(direct_request, connection, workspace)
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            direct
                .execution
                .validate()
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            let direct_ir = direct
                .deep_context_ir
                .as_ref()
                .ok_or_else(|| AtlasError::Other("direct DEEP Context IR is absent".into()))?;
            let direct_exact = matches!(
                &direct.execution,
                ContextExecution::Partial {
                    attempts,
                    final_route: Route::AtlasDeep,
                    satisfied_capabilities,
                    deficits,
                    terminal_reason: TerminalReason::RouteCeilingReached,
                    ..
                } if attempts.len() == 1
                    && attempts[0].route == Route::AtlasDeep
                    && attempts[0].state == AttemptState::Partial
                    && attempts[0].deficits == light_deficits
                    && satisfied_capabilities == &[RequiredCapability::IdentityLookup]
                    && deficits == &light_deficits
            ) && direct_ir.context_hash == progressive_ir.context_hash
                && serde_json::to_vec(direct_ir)? == serde_json::to_vec(progressive_ir)?;

            let serving_report = serving::build_serving_generation(connection, workspace)?;
            let ready = run_catalogue_governor(progressive_request, connection, workspace)
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            ready
                .execution
                .validate()
                .map_err(|error| AtlasError::Other(error.to_string()))?;
            let ready_ir = ready.deep_context_ir.as_ref().ok_or_else(|| {
                AtlasError::Other("ready-Serving DEEP Context IR is absent".into())
            })?;
            let ready_exact = ready_ir.context_hash == progressive_ir.context_hash
                && serde_json::to_vec(ready_ir)? == serde_json::to_vec(progressive_ir)?;
            serving::delete_serving_generation(connection, &serving_report.serving_generation_id)?;
            phase_correctness = attempts_exact
                && payload_hash == progressive_ir.context_hash
                && progressive_ir.schema_version == CONTEXT_IR_VERSION
                && source_observed
                && progressive.light_results.is_none()
                && direct_exact
                && ready_exact
                && primary_truth_fallback
                && durable_compiler_state_counts(connection)? == durable_before;
            phase_identity = identity_hash(&[
                phase.descriptor().name,
                &semantic_deep_context_hash(progressive_ir)?,
            ]);
            compiler = Some(deep_compiler_measurement(
                progressive_ir,
                primary_truth_fallback,
            ));
            route_work_units = Some(counters.work_units);
        }
        CapacityPhase::CatalogueGrowth => {
            let current = discovery::reconcile(workspace, connection, config)?;
            let serving_report = serving::build_serving_generation(connection, workspace)?;
            phase_correctness = current.activation == "committed" && current.failed_file_count == 0;
            phase_identity = current.source_tree_hash.clone();
            serving_to_delete = Some(serving_report.serving_generation_id);
            report = Some(current);
        }
        CapacityPhase::CapabilityDiscovery | CapacityPhase::DirectRouteCompiler => {
            return Err(AtlasError::Other(
                "stateless capacity dispatch reached catalogue execution".into(),
            ))
        }
    }

    let (observed_target_identity, observed_evidence_counts, accepted_outcome, source_bytes) =
        observed_capacity_identity(connection, workspace, &state.fixture_root, &state.manifest)?;
    let accepted_matches = observed_target_identity == observation.target_identity
        && accepted_outcome == observation.expected_accepted_outcome_hash;
    let ready_truth_equivalent = descriptor
        .ready_truth_measurements
        .then_some(phase_correctness);
    let ready_versus_truth = match ready_truth_equivalent {
        Some(equivalent) => ready_truth_identity(&accepted_outcome, equivalent)?,
        None => None,
    };
    let catalogue_after = catalogue_category_measurement(connection)?;
    let catalogue_bytes_after = catalogue_bytes(&state.catalogue_path);
    if descriptor.dispatch == CapacityPhase::CatalogueGrowth {
        phase_correctness &= catalogue_after.derived_rows > catalogue_before.derived_rows
            && catalogue_after.derived_bytes >= catalogue_before.derived_bytes;
    }
    if let Some(serving_generation_id) = serving_to_delete {
        serving::delete_serving_generation(connection, &serving_generation_id)?;
    }
    let (provider_outcome, provider) =
        capacity_provider_measurement(connection, report.as_ref(), &state.provider_condition)?;
    let degraded = report
        .as_ref()
        .is_some_and(|current| current.semantic_degraded)
        || provider_outcome.as_deref() == Some("degraded");
    let reconcile_work_units = |current: &discovery::ReconcileReport| {
        u64::try_from(
            current
                .parsed_file_count
                .saturating_add(current.renamed_file_count)
                .saturating_add(current.deleted_file_count),
        )
        .unwrap_or(0)
    };
    let work_units = route_work_units
        .or_else(|| compiler.map(|value| value.work_units_consumed))
        .or_else(|| report.as_ref().map(reconcile_work_units))
        .unwrap_or(0);
    let eligible_files = report
        .as_ref()
        .map(|current| u64::try_from(current.eligible_file_count).unwrap_or(0))
        .or_else(|| state.manifest["counts"]["eligible_files"].as_u64());
    let indexed_files = report
        .as_ref()
        .map(|current| u64::try_from(current.indexed_file_count).unwrap_or(0))
        .or_else(|| state.manifest["counts"]["indexed_files"].as_u64());
    let (parsed_files, changed_files, changed_bytes) = match descriptor.reconcile_measurements {
        CapacityReconcileMeasurements::NotApplicable => (None, None, None),
        CapacityReconcileMeasurements::Observed | CapacityReconcileMeasurements::NoChange => (
            report
                .as_ref()
                .map(|current| u64::try_from(current.parsed_file_count).unwrap_or(0)),
            Some(0),
            Some(0),
        ),
        CapacityReconcileMeasurements::FixedIncremental => (
            report
                .as_ref()
                .map(|current| u64::try_from(current.parsed_file_count).unwrap_or(0)),
            applied_change.map(|change| change.files),
            applied_change.map(|change| change.bytes),
        ),
    };
    let measured = measured_runtime.unwrap_or_else(|| duration_millis(timed_started.elapsed()));
    let result = CapacityMeasurement {
        degraded,
        provider_outcome,
        provider: Some(provider),
        correctness: phase_correctness && accepted_matches,
        deterministic_identity: identity_hash(&[
            phase.descriptor().name,
            &accepted_outcome,
            &phase_identity,
        ]),
        observed_target_identity: Some(observed_target_identity),
        observed_evidence_counts: Some(observed_evidence_counts),
        accepted_outcome: Some(accepted_outcome),
        ready_versus_truth,
        work_units,
        eligible_files,
        indexed_files,
        parsed_files,
        changed_files,
        changed_bytes,
        source_bytes: Some(source_bytes),
        compiler,
        catalogue_before: Some(catalogue_before),
        catalogue_after: Some(catalogue_after),
        catalogue_bytes_before: Some(catalogue_bytes_before),
        catalogue_bytes_after: Some(catalogue_bytes_after),
        atlas_runtime_duration_ms: measured,
        wall_duration_ms: duration_millis(action_started.elapsed()),
    };
    Ok(result)
}

fn command_text(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn cpu_description() -> String {
    std::env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|text| {
                    text.lines().find_map(|line| {
                        line.strip_prefix("model name")
                            .and_then(|value| value.split_once(':'))
                            .map(|(_, value)| value.trim().to_string())
                    })
                })
        })
        .or_else(|| {
            let value = command_text("sysctl", &["-n", "machdep.cpu.brand_string"]);
            (value != "unavailable").then_some(value)
        })
        .unwrap_or_else(|| "unavailable".to_string())
}

fn file_sha256(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn write_attestation(
    manifest_dir: &Path,
    scenario_path: &Path,
    scenario: &BenchmarkScenario,
    evidence: &RunEvidence,
    passed: usize,
    total: usize,
    elapsed: std::time::Duration,
) -> Result<PathBuf> {
    let revision = command_text("git", &["rev-parse", "HEAD"]);
    if revision == "unavailable" {
        return Err(AtlasError::Other(
            "cannot attest benchmark without the tested Git revision".into(),
        ));
    }
    let working_tree_clean = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| output.stdout.is_empty());
    let output_directory = manifest_dir.join("target/atlas-bench");
    std::fs::create_dir_all(&output_directory)?;
    let output_path = output_directory.join(format!("run-attestation-{}.json", std::process::id()));
    let attestation = RunAttestation {
        policies: PolicyAttestation {
            planner: PLANNER_POLICY_VERSION,
            projection: PROJECTION_POLICY_VERSION,
            context_ir: CONTEXT_SCHEMA_VERSION,
            context_yield: CONTEXT_YIELD_REPORT_SCHEMA_VERSION,
            profile: H3_A_PROFILE_VERSION,
        },
        attestation_schema_version: "1.0.0",
        scenario_schema_version: &scenario.schema_version,
        scenario_path: scenario_path.to_string_lossy().replace('\\', "/"),
        scenario_sha256: file_sha256(scenario_path)?,
        revision,
        working_tree_clean,
        toolchain: command_text("rustc", &["--version", "--verbose"]),
        platform: PlatformAttestation {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            cpu: cpu_description(),
        },
        fixture: FixtureAttestation {
            generator: &scenario.dataset.generator,
            generation: &scenario.dataset.generation,
            provider_state: &scenario.dataset.provider_state,
            source_tree_hash: &evidence.source_tree_hash,
            generation_id: &evidence.generation_id,
        },
        cache_state: "uncontrolled",
        procedure: &scenario.procedure,
        experiment_results: &evidence.experiment_results,
        profiles: &scenario.profiles,
        samples: SampleAttestation {
            baseline_reconcile_ms: evidence.cold_bootstrap_ms.clone(),
            total_ms: vec![elapsed.as_secs_f64() * 1000.0],
            correctness_checks_passed: passed,
            correctness_checks_total: total,
        },
        generated_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AtlasError::Other(format!("system clock before UNIX epoch: {error}")))?
            .as_secs(),
    };
    std::fs::write(&output_path, serde_json::to_vec_pretty(&attestation)?)?;
    Ok(output_path)
}

fn profile_request(
    scenario: &BenchmarkScenario,
    name: &str,
    seed_path: &str,
) -> Result<CompileRequest> {
    let profile = scenario
        .profiles
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| AtlasError::InvalidConfig(format!("unknown experiment profile {name}")))?;
    Ok(CompileRequest {
        known_paths: vec![seed_path.to_string()],
        known_symbols: Vec::new(),
        max_records: i64::try_from(profile.records)
            .map_err(|_| AtlasError::InvalidConfig("profile records exceed i64".into()))?,
        max_source_bytes: i64::try_from(profile.source_bytes)
            .map_err(|_| AtlasError::InvalidConfig("profile source bytes exceed i64".into()))?,
        max_estimated_tokens: i64::try_from(profile.estimated_tokens)
            .map_err(|_| AtlasError::InvalidConfig("profile tokens exceed i64".into()))?,
        soft_latency_ms: i64::try_from(profile.candidate_warm_p95_ms)
            .map_err(|_| AtlasError::InvalidConfig("profile soft latency exceeds i64".into()))?,
        hard_latency_ms: i64::try_from(profile.wall_cancellation_ms)
            .map_err(|_| AtlasError::InvalidConfig("profile cancellation exceeds i64".into()))?,
    })
}

fn identity_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[derive(Clone, Copy)]
struct ExperimentWorkspace<'a> {
    connection: &'a rusqlite::Connection,
    workspace: &'a workspace::WorkspaceRecord,
    generation_id: &'a str,
    catalogue_file_count: u64,
    seed_path: &'a str,
}

fn run_experiment_arm(
    environment: ExperimentWorkspace<'_>,
    scenario: &BenchmarkScenario,
    experiment_id: &str,
    arm_name: &str,
    condition: &ExperimentCondition,
) -> Result<Vec<ExperimentRawSample>> {
    let task_hash = identity_hash(&[experiment_id, "task"]);
    let normalized_goal_hash = identity_hash(&[experiment_id, "goal"]);
    let session = create_task_session(
        environment.connection,
        environment.workspace,
        environment.generation_id,
        &task_hash,
        &normalized_goal_hash,
        RawTaskRetention::None,
        None,
        condition.task_kind,
        CONTEXT_SCHEMA_VERSION,
    )?;
    let request = profile_request(scenario, &condition.profile, environment.seed_path)?;
    for _ in 0..scenario.procedure.warmups {
        compile_context_ir(
            environment.connection,
            environment.workspace,
            &session.task_session_id,
            &session.task_hash,
            &session.normalized_goal_hash,
            condition.task_kind,
            TaskKindSource::Declared,
            None,
            &request,
        )?;
    }
    let sample_count = scenario.procedure.warm_samples * scenario.procedure.repetitions;
    let mut samples = Vec::with_capacity(sample_count as usize);
    for _ in 0..sample_count {
        let started = Instant::now();
        let ir = compile_context_ir(
            environment.connection,
            environment.workspace,
            &session.task_session_id,
            &session.task_hash,
            &session.normalized_goal_hash,
            condition.task_kind,
            TaskKindSource::Declared,
            None,
            &request,
        )?;
        let expected_fallback =
            condition.serving_state == ExperimentServingState::DirectTruthFallback;
        if ir.status.serving_fallback != expected_fallback {
            return Err(AtlasError::Other(format!(
                "experiment {experiment_id}/{arm_name} expected serving_fallback={expected_fallback}"
            )));
        }
        samples.push(ExperimentRawSample {
            context_hash: ir.context_hash,
            elapsed_micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            working_set_size: ir.working_set.len() as u64,
            selected_source_bytes: ir.cost.selected_source_bytes,
            selected_estimated_tokens: ir.cost.selected_estimated_tokens,
            serving_fallback: ir.status.serving_fallback,
            truncated: ir.omissions.truncated,
        });
    }
    Ok(samples)
}

fn run_frozen_experiments(
    baseline: ExperimentWorkspace<'_>,
    scaled_graph: ExperimentWorkspace<'_>,
    scenario: &BenchmarkScenario,
) -> Result<Vec<ExperimentRunResult>> {
    let fallback = scenario
        .experiment_bundle
        .experiments
        .iter()
        .find(|experiment| experiment.independent_variable == ExperimentDimension::Fallback)
        .ok_or_else(|| AtlasError::InvalidConfig("fallback experiment is absent".into()))?;
    let fallback_candidate = run_experiment_arm(
        baseline,
        scenario,
        &fallback.id,
        "candidate",
        &fallback.candidate,
    )?;

    serving::build_serving_generation(baseline.connection, baseline.workspace)?;
    serving::build_serving_generation(scaled_graph.connection, scaled_graph.workspace)?;

    let limitations = vec![
        ExperimentLimitation::SingleEnvironment,
        ExperimentLimitation::ColdBootstrapSeparate,
        ExperimentLimitation::ModelSideDiscoverySeparate,
    ];
    let mut results = Vec::with_capacity(scenario.experiment_bundle.experiments.len());
    for experiment in &scenario.experiment_bundle.experiments {
        let control = run_experiment_arm(
            baseline,
            scenario,
            &experiment.id,
            "control",
            &experiment.control,
        )?;
        let (candidate, candidate_environment) =
            if experiment.independent_variable == ExperimentDimension::Fallback {
                (fallback_candidate.clone(), baseline)
            } else if experiment.independent_variable == ExperimentDimension::GraphScale {
                (
                    run_experiment_arm(
                        scaled_graph,
                        scenario,
                        &experiment.id,
                        "candidate",
                        &experiment.candidate,
                    )?,
                    scaled_graph,
                )
            } else if experiment.independent_variable == ExperimentDimension::Representation {
                (control.clone(), baseline)
            } else {
                (
                    run_experiment_arm(
                        baseline,
                        scenario,
                        &experiment.id,
                        "candidate",
                        &experiment.candidate,
                    )?,
                    baseline,
                )
            };
        let mut result_limitations = limitations.clone();
        if experiment.independent_variable == ExperimentDimension::Representation {
            result_limitations.push(ExperimentLimitation::RepresentationAdapterPendingH5);
        }
        let result = ExperimentRunResult::from_samples(
            experiment,
            ExperimentArmSamples {
                generation_id: baseline.generation_id.to_string(),
                catalogue_file_count: baseline.catalogue_file_count,
                raw_samples: control,
            },
            ExperimentArmSamples {
                generation_id: candidate_environment.generation_id.to_string(),
                catalogue_file_count: candidate_environment.catalogue_file_count,
                raw_samples: candidate,
            },
            result_limitations,
        );
        result
            .validate_against(experiment)
            .map_err(|error| AtlasError::Other(format!("invalid experiment evidence: {error}")))?;
        results.push(result);
    }
    Ok(results)
}

fn copy_graph_scale(source: &Path, destination: &Path, scale: u64) -> Result<()> {
    for replica in 0..scale {
        let replica_root = destination.join(format!("replica-{replica}"));
        copy_directory(source, &replica_root)?;
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination_path)?;
        } else {
            std::fs::copy(entry.path(), destination_path)?;
        }
    }
    Ok(())
}

fn canonicalize_owned_benchmark_root(root: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(root)?)
}

fn collect_cold_bootstrap_samples(
    generator: &Path,
    config: &Config,
    requested_samples: u64,
    initial_sample_ms: f64,
) -> Result<Vec<f64>> {
    let mut samples = vec![initial_sample_ms];
    for sample_index in 1..requested_samples {
        let suffix = format!("{}-{sample_index}", std::process::id());
        let fixture_root = std::env::temp_dir().join(format!("atlas-bench-cold-{suffix}"));
        let catalogue_path = std::env::temp_dir().join(format!("atlas-bench-cold-{suffix}.sqlite"));
        let _ = std::fs::remove_dir_all(&fixture_root);
        let _ = std::fs::remove_file(&catalogue_path);
        let generated = Command::new("python3")
            .arg(generator)
            .arg("--output")
            .arg(&fixture_root)
            .arg("--force")
            .output()?;
        if !generated.status.success() {
            return Err(AtlasError::Other(format!(
                "cold bootstrap fixture generation failed for sample {sample_index}"
            )));
        }
        let fixture_root = canonicalize_owned_benchmark_root(&fixture_root)?;
        let connection = catalogue::init_catalogue(&catalogue_path, config)?;
        let workspace = workspace::register_workspace(
            &connection,
            &fixture_root,
            config,
            &catalogue_path,
            "1.0.0",
        )?;
        let started = Instant::now();
        discovery::reconcile(&workspace, &connection, config)?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(connection);
        let _ = std::fs::remove_dir_all(&fixture_root);
        let _ = std::fs::remove_file(&catalogue_path);
    }
    Ok(samples)
}

fn run(
    scenario: &BenchmarkScenario,
    checks: &mut Vec<Check>,
    evidence: &mut RunEvidence,
) -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let generator = manifest_dir.join(&scenario.dataset.generator);
    let fixture_root =
        std::env::temp_dir().join(format!("atlas-bench-fixture-{}", std::process::id()));
    let cat_path = std::env::temp_dir().join(format!("atlas-bench-{}.sqlite", std::process::id()));
    let scaled_fixture_root =
        std::env::temp_dir().join(format!("atlas-bench-fixture-10x-{}", std::process::id()));
    let scaled_cat_path =
        std::env::temp_dir().join(format!("atlas-bench-10x-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_dir_all(&fixture_root);
    let _ = std::fs::remove_file(&cat_path);
    let _ = std::fs::remove_dir_all(&scaled_fixture_root);
    let _ = std::fs::remove_file(&scaled_cat_path);

    // Generate a fresh deterministic public pilot fixture.
    let gen = Command::new("python3")
        .arg(&generator)
        .arg("--output")
        .arg(&fixture_root)
        .arg("--force")
        .output();
    let gen_ok = matches!(&gen, Ok(o) if o.status.success());
    check(
        checks,
        "generate_fixture.py",
        gen_ok,
        match &gen {
            Ok(o) => format!("exit {:?}", o.status.code()),
            Err(e) => format!("failed to spawn python3: {e}"),
        },
    );
    if !gen_ok {
        return Ok(());
    }
    let fixture_root = canonicalize_owned_benchmark_root(&fixture_root)?;

    let manifest_text =
        std::fs::read_to_string(fixture_root.join(&scenario.dataset.fixture_manifest))?;
    let manifest: Value = serde_json::from_str(&manifest_text)?;
    let target_path = manifest["target"]["path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let target_symbol = manifest["target"]["symbol"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expected_line = manifest["target"]["expected_declaration_line"]
        .as_i64()
        .unwrap_or(-1);
    let expected_sha256 = manifest["target"]["content_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if scenario
        .experiment_bundle
        .experiments
        .iter()
        .any(|experiment| experiment.accepted_outcome_hash != expected_sha256)
    {
        return Err(AtlasError::InvalidConfig(
            "experiment accepted outcome does not match the fixture target contract".into(),
        ));
    }
    let rename_from = manifest["operations"]["rename"]["from"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let rename_to = manifest["operations"]["rename"]["to"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let delete_path = manifest["operations"]["delete"]["path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let change_path = manifest["operations"]["change"]["path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expected_provider_runs = manifest["operations"]["change"]
        ["expect_changed_file_provider_runs"]
        .as_i64()
        .unwrap_or(-1);
    let expected_exclusions: Vec<String> = manifest["expected_exclusions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // --- Init + baseline reconcile (real pipeline, not a subprocess). ---
    let cfg = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"atlas-bench-fixture\"\n",
    )?;
    let conn = catalogue::init_catalogue(&cat_path, &cfg)?;
    let ws = workspace::register_workspace(&conn, &fixture_root, &cfg, &cat_path, "1.0.0")?;

    let t0 = Instant::now();
    let baseline = discovery::reconcile(&ws, &conn, &cfg)?;
    let baseline_ms = t0.elapsed().as_secs_f64() * 1000.0;
    evidence
        .generation_id
        .clone_from(&baseline.candidate_generation_id);
    evidence.cold_bootstrap_ms = collect_cold_bootstrap_samples(
        &generator,
        &cfg,
        scenario.procedure.cold_samples,
        baseline_ms,
    )?;
    copy_graph_scale(&fixture_root, &scaled_fixture_root, 10)?;
    let scaled_fixture_root = canonicalize_owned_benchmark_root(&scaled_fixture_root)?;
    let scaled_conn = catalogue::init_catalogue(&scaled_cat_path, &cfg)?;
    let scaled_ws = workspace::register_workspace(
        &scaled_conn,
        &scaled_fixture_root,
        &cfg,
        &scaled_cat_path,
        "1.0.0",
    )?;
    let scaled_reconcile = discovery::reconcile(&scaled_ws, &scaled_conn, &cfg)?;
    let scaled_target_path = format!("replica-0/{target_path}");
    evidence
        .source_tree_hash
        .clone_from(&baseline.source_tree_hash);
    check(
        checks,
        "baseline reconcile commits",
        baseline.activation == "committed",
        format!(
            "{} files eligible, {} indexed, {:.1}ms",
            baseline.eligible_file_count, baseline.indexed_file_count, baseline_ms
        ),
    );

    evidence.experiment_results = run_frozen_experiments(
        ExperimentWorkspace {
            connection: &conn,
            workspace: &ws,
            generation_id: &baseline.candidate_generation_id,
            catalogue_file_count: baseline.eligible_file_count as u64,
            seed_path: &target_path,
        },
        ExperimentWorkspace {
            connection: &scaled_conn,
            workspace: &scaled_ws,
            generation_id: &scaled_reconcile.candidate_generation_id,
            catalogue_file_count: scaled_reconcile.eligible_file_count as u64,
            seed_path: &scaled_target_path,
        },
        scenario,
    )?;

    // --- Target symbol resolves at the exact manifest-declared line. ---
    let find_out = cli::build_find_output(&fixture_root, &target_symbol, 10, Some(&cat_path))?;
    let hit = find_out
        .results
        .iter()
        .find(|r| r.canonical_path == target_path);
    check(
        checks,
        "target symbol found",
        hit.is_some(),
        format!("{target_symbol} in {target_path}"),
    );
    if let Some(h) = hit {
        check(
            checks,
            "target declaration line matches manifest",
            h.start_line == expected_line,
            format!("expected {expected_line}, got {}", h.start_line),
        );
    }

    // --- Exact source hash matches the manifest's SHA-256. ---
    let inspect_out = cli::build_inspect_output(&fixture_root, &target_path, Some(&cat_path))?;
    check(
        checks,
        "target content hash matches manifest",
        inspect_out.content_hash.as_deref() == Some(expected_sha256.as_str()),
        format!(
            "expected {expected_sha256}, got {:?}",
            inspect_out.content_hash
        ),
    );

    // --- Default exclusions cover every manifest-declared pattern. ---
    let excluded_concrete: Vec<&str> = vec![
        ".env",
        ".atlas/index.db",
        "dist/bundle.js",
        "archive/old_controller.ts",
        "assets/logo.bin",
    ];
    for path in &excluded_concrete {
        let present: bool = conn.query_row(
            "SELECT COUNT(*) FROM generation_file gf JOIN index_generation g ON g.generation_id = gf.generation_id
             WHERE g.workspace_id = ?1 AND gf.canonical_path = ?2 AND gf.presence_state = 'present'",
            rusqlite::params![ws.workspace_id, path],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0;
        check(
            checks,
            &format!("excluded: {path}"),
            !present,
            format!(
                "expected_exclusions covers this ({} patterns declared)",
                expected_exclusions.len()
            ),
        );
    }

    // --- Rename: exact-hash identity continuity. ---
    let old_id: Option<String> = conn
        .query_row(
            "SELECT file_id FROM file_identity WHERE workspace_id = ?1 AND last_known_path = ?2",
            rusqlite::params![ws.workspace_id, rename_from],
            |r| r.get(0),
        )
        .ok();
    std::fs::rename(
        fixture_root.join(&rename_from),
        fixture_root.join(&rename_to),
    )?;
    let rename_report = discovery::reconcile(&ws, &conn, &cfg)?;
    check(
        checks,
        "rename detected exactly once",
        rename_report.renamed_file_count == 1,
        format!("renamed_file_count={}", rename_report.renamed_file_count),
    );
    let new_id: Option<String> = conn
        .query_row(
            "SELECT file_id FROM file_identity WHERE workspace_id = ?1 AND last_known_path = ?2",
            rusqlite::params![ws.workspace_id, rename_to],
            |r| r.get(0),
        )
        .ok();
    check(
        checks,
        "rename preserves file identity",
        old_id.is_some() && old_id == new_id,
        format!("{old_id:?} -> {new_id:?}"),
    );

    // --- Delete: tombstone recorded. ---
    std::fs::remove_file(fixture_root.join(&delete_path))?;
    let delete_report = discovery::reconcile(&ws, &conn, &cfg)?;
    check(
        checks,
        "delete detected exactly once",
        delete_report.deleted_file_count == 1,
        format!("deleted_file_count={}", delete_report.deleted_file_count),
    );
    let history_out =
        cli::build_history_output(&fixture_root, Some(&delete_path), 10, Some(&cat_path))?;
    let has_tombstone = history_out
        .events
        .iter()
        .any(|e| e.event_type == "deleted" && e.tombstone.is_some());
    check(
        checks,
        "deletion produced a tombstone",
        has_tombstone,
        format!(
            "{} history event(s) for {delete_path}",
            history_out.events.len()
        ),
    );

    // --- Change: exactly one file re-parsed. ---
    let change_abs = fixture_root.join(&change_path);
    let original = std::fs::read_to_string(&change_abs)?;
    std::fs::write(
        &change_abs,
        format!("{original}\n// atlas-bench edit marker\n"),
    )?;
    let change_report = discovery::reconcile(&ws, &conn, &cfg)?;
    check(
        checks,
        "edit reparses exactly the changed file",
        change_report.parsed_file_count == expected_provider_runs,
        format!(
            "expected {expected_provider_runs}, got {}",
            change_report.parsed_file_count
        ),
    );

    let _ = std::fs::remove_dir_all(&fixture_root);
    let _ = std::fs::remove_file(&cat_path);
    let _ = std::fs::remove_dir_all(&scaled_fixture_root);
    let _ = std::fs::remove_file(&scaled_cat_path);
    Ok(())
}

#[cfg(test)]
mod capacity_unit_tests {
    use super::*;
    use std::io::BufRead;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn owned_benchmark_root_is_canonical_before_registration() {
        let temporary = tempfile::tempdir().unwrap();
        let alias_component = temporary.path().join("alias");
        std::fs::create_dir(&alias_component).unwrap();
        let aliased_root = alias_component.join("..");

        let resolved_root = canonicalize_owned_benchmark_root(&aliased_root).unwrap();
        assert_ne!(aliased_root, resolved_root);
        assert_eq!(
            resolved_root,
            std::fs::canonicalize(temporary.path()).unwrap()
        );

        let config = Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"atlas-bench-fixture\"\n",
        )
        .unwrap();
        let catalogue_path = temporary.path().join("catalogue.sqlite");
        let connection = catalogue::init_catalogue(&catalogue_path, &config).unwrap();
        let workspace = workspace::register_workspace(
            &connection,
            &resolved_root,
            &config,
            &catalogue_path,
            "1.0.0",
        )
        .unwrap();
        assert_eq!(
            Path::new(&workspace.canonical_root),
            resolved_root.as_path()
        );
        assert!(
            workspace::load_workspace_by_root(&connection, &resolved_root)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn memory_label_publisher_atomically_emits_closed_phase_state() {
        let evidence = tempfile::Builder::new()
            .prefix("t30c-memory-labels-")
            .tempdir()
            .unwrap();
        let path = evidence.path().join(MEMORY_LABEL_STATE_FILE);
        std::fs::write(
            &path,
            serde_json::to_vec(&CapacityMemoryLabels::startup(CAPACITY_PROGRESS_TOTAL)).unwrap(),
        )
        .unwrap();
        let mut publisher = CapacityMemoryLabelPublisher::open(evidence.path(), &path).unwrap();
        let resolved = CapacityProviderCondition::TypeScriptOnly.resolve(CapacityDataset::Small);
        publisher
            .publish(
                &CapacityMemoryLabels::for_attempt(
                    CapacityPhase::ReadyServingQueryCompiler.descriptor(),
                    &resolved,
                    "warm",
                    7,
                    CAPACITY_PROGRESS_TOTAL,
                )
                .unwrap(),
            )
            .unwrap();
        let labels: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            labels,
            serde_json::json!({
                "phase": "ready-serving-query-compiler",
                "filesystem_cache_state": "atlas-warm",
                "serving_state": "ready",
                "provider_cache_install_state": "typescript-only:required-version-probed",
                "build_state": "prebuilt-atlas-bench",
                "catalogue_state": "baseline reconcile then one ready serving generation retained across warm samples",
                "progress_state": "active",
                "progress_ordinal": 7,
                "progress_total": 135_000,
            })
        );
        assert!(!evidence
            .path()
            .join(".capacity-memory-label-state-v1.tmp")
            .exists());
    }

    #[cfg(windows)]
    #[test]
    fn production_publisher_and_python_reader_remain_closed_beyond_1125_cycles() {
        let evidence = tempfile::Builder::new()
            .prefix("t35-production-label-pair-")
            .tempdir()
            .unwrap();
        let path = evidence.path().join(MEMORY_LABEL_STATE_FILE);
        std::fs::write(
            &path,
            serde_json::to_vec(&CapacityMemoryLabels::startup(CAPACITY_PROGRESS_TOTAL)).unwrap(),
        )
        .unwrap();

        let python = if cfg!(windows) { "python" } else { "python3" };
        let reader_script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts/process-tree-memory.py")
            .to_string_lossy()
            .into_owned();
        let label_path = path.to_string_lossy().into_owned();
        let script = concat!(
            "import importlib.util,json,pathlib,sys,time\n",
            "spec=importlib.util.spec_from_file_location('process_tree_memory',sys.argv[1])\n",
            "module=importlib.util.module_from_spec(spec)\n",
            "sys.modules[spec.name]=module\n",
            "spec.loader.exec_module(module)\n",
            "path=pathlib.Path(sys.argv[2]);reads=0;maximum=0;intermediate_reads=0\n",
            "print('READY',flush=True)\n",
            "deadline=time.monotonic()+20\n",
            "while time.monotonic()<deadline:\n",
            " state=module.read_labels(path);reads+=1\n",
            " ordinal=int(state['progress_ordinal']);maximum=max(maximum,ordinal)\n",
            " if 0<ordinal<1127:intermediate_reads+=1\n",
            " if maximum>=1127 and reads>1125 and intermediate_reads>0:break\n",
            "original_open=module._open_label_reader;attempts=0\n",
            "def persistent_transition(_):\n",
            " global attempts\n",
            " attempts+=1;raise module.ctypes.WinError(32)\n",
            "module._open_label_reader=persistent_transition\n",
            "try:\n",
            " try:module.read_labels(path);read_failed_closed=False\n",
            " except module.EvidenceError:read_failed_closed=True\n",
            "finally:module._open_label_reader=original_open\n",
            "result={'reads':reads,'valid_closed_reads':reads,'maximum_ordinal':maximum,",
            "'intermediate_reads':intermediate_reads,'bounded_failure_attempts':attempts}\n",
            "print(json.dumps(result,separators=(',',':')),flush=True)\n",
            "raise SystemExit(0 if reads>1125 and maximum>=1127 and intermediate_reads>0 ",
            "and read_failed_closed and attempts==65 else 2)\n",
        );
        let mut child = Command::new(python)
            .args(["-B", "-c", script, &reader_script, &label_path])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "READY");

        let mut publisher = CapacityMemoryLabelPublisher::open(evidence.path(), &path).unwrap();
        let resolved = CapacityProviderCondition::TypeScriptOnly.resolve(CapacityDataset::Small);
        for ordinal in 1..=1_127 {
            let phase = if ordinal % 2 == 0 {
                CapacityPhase::NoChangeReconcile
            } else {
                CapacityPhase::CapabilityDiscovery
            };
            publisher
                .publish(
                    &CapacityMemoryLabels::for_attempt(
                        phase.descriptor(),
                        &resolved,
                        "warm",
                        ordinal,
                        CAPACITY_PROGRESS_TOTAL,
                    )
                    .unwrap(),
                )
                .unwrap();
        }

        let mut result_line = String::new();
        stdout.read_line(&mut result_line).unwrap();
        let status = child.wait().unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "Python reader failed: {stderr}");
        let result: Value = serde_json::from_str(result_line.trim()).unwrap();
        println!("production-pair-stress {result}");
        assert_eq!(result.as_object().unwrap().len(), 5);
        assert!(result["reads"].as_u64().unwrap() > 1_125);
        assert_eq!(result["valid_closed_reads"], result["reads"]);
        assert!(result["maximum_ordinal"].as_u64().unwrap() >= 1_127);
        assert!(result["intermediate_reads"].as_u64().unwrap() > 0);
        assert_eq!(result["bounded_failure_attempts"], 65);
        assert!(!evidence
            .path()
            .join(".capacity-memory-label-state-v1.tmp")
            .exists());

        let failure = tempfile::Builder::new()
            .prefix("t35-production-label-failure-")
            .tempdir()
            .unwrap();
        let failure_path = failure.path().join(MEMORY_LABEL_STATE_FILE);
        std::fs::write(
            &failure_path,
            serde_json::to_vec(&CapacityMemoryLabels::startup(CAPACITY_PROGRESS_TOTAL)).unwrap(),
        )
        .unwrap();
        let mut failing_publisher =
            CapacityMemoryLabelPublisher::open(failure.path(), &failure_path).unwrap();
        std::fs::remove_file(&failure_path).unwrap();
        std::fs::create_dir(&failure_path).unwrap();
        let attempts = std::cell::Cell::new(0);
        let exhausted = retry_windows_replace(|| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from_raw_os_error(1175))
        })
        .unwrap_err();
        assert_eq!(exhausted.raw_os_error(), Some(1175));
        assert_eq!(attempts.get(), 65);
        let error = failing_publisher
            .publish(
                &CapacityMemoryLabels::for_attempt(
                    CapacityPhase::CapabilityDiscovery.descriptor(),
                    &resolved,
                    "warm",
                    1,
                    CAPACITY_PROGRESS_TOTAL,
                )
                .unwrap(),
            )
            .unwrap_err();
        assert!(matches!(error, AtlasError::Io(_)));
        assert_eq!(failing_publisher.policy.last_published_ordinal, 0);
        assert!(failure_path.is_dir());
        assert!(!failure
            .path()
            .join(".capacity-memory-label-state-v1.tmp")
            .exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_replace_retries_only_error_1175_with_a_fixed_bound() {
        let attempts = std::cell::Cell::new(0);
        retry_windows_replace(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 4 {
                Err(std::io::Error::from_raw_os_error(1175))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(attempts.get(), 4);

        let attempts = std::cell::Cell::new(0);
        let unrelated = retry_windows_replace(|| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from_raw_os_error(5))
        })
        .unwrap_err();
        assert_eq!(unrelated.raw_os_error(), Some(5));
        assert_eq!(attempts.get(), 1);

        let attempts = std::cell::Cell::new(0);
        let exhausted = retry_windows_replace(|| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from_raw_os_error(1175))
        })
        .unwrap_err();
        assert_eq!(exhausted.raw_os_error(), Some(1175));
        assert_eq!(attempts.get(), 65);
    }

    fn write_segmented_memory_fixture(evidence: &Path, execution: &Value) -> (PathBuf, PathBuf) {
        let sampler =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/process-tree-memory.py");
        let summary = evidence.join(MEMORY_SUMMARY_FILE);
        let python = if cfg!(windows) { "python" } else { "python3" };
        let script = r#"
import copy,json,pathlib,runpy,sys
module=runpy.run_path(sys.argv[1])
root=pathlib.Path(sys.argv[2])
execution=json.loads(sys.argv[3])
labels={
 "phase":"fixture",
 "filesystem_cache_state":"warm",
 "serving_state":"absent",
 "provider_cache_install_state":"not-applicable",
 "build_state":"fixture",
 "catalogue_state":"fixture",
}
records=module["fixture_evidence_records"](labels)
writer=module["SegmentedRawWriter"](
 root,
 "capacity-memory-raw-v2",
 maximum_segment_samples=60,
 maximum_segment_bytes=512*1024*1024,
)
for record in records:
 writer.write(copy.deepcopy(record))
segments=writer.finish()
summary=module["build_segmented_summary"](
 records,
 segments,
 module["platform_metadata"](),
 segment_name_prefix="capacity-memory-raw-v2",
 exit_code=0,
 cadence_ns=module["CADENCE_NS"],
 idle_duration_ns=module["IDLE_DURATION_NS"],
 maximum_segment_samples=200000,
 maximum_segment_bytes=512*1024*1024,
 raw_sha256=writer.digest.hexdigest(),
 raw_byte_length=writer.bytes_written,
 capacity_execution=execution,
)
(root/"capacity-memory-summary-v2.json").write_text(
 json.dumps(summary,sort_keys=True,indent=2)+"\n",
 encoding="utf-8",
 newline="\n",
)
"#;
        let output = Command::new(python)
            .args(["-B", "-c", script])
            .arg(&sampler)
            .arg(evidence)
            .arg(serde_json::to_string(execution).unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (sampler, summary)
    }

    #[test]
    fn rust_validation_consumes_every_bounded_memory_segment_and_rejects_tampering() {
        let global_plan = "a".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let evidence = tempfile::Builder::new()
            .prefix("t35-segmented-memory-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary) = write_segmented_memory_fixture(evidence.path(), &execution);
        let validated =
            validate_memory_sampler_outputs(evidence.path(), &summary, &sampler, &execution)
                .unwrap();
        assert_eq!(validated.segments.len(), 2);
        assert_eq!(validated.record_count, 103);
        assert!(validated.byte_length > 0);

        let first = evidence
            .path()
            .join(format!("{MEMORY_RAW_PREFIX}-000000.ndjson"));
        let mut bytes = std::fs::read(&first).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&first, bytes).unwrap();
        assert!(
            validate_memory_sampler_outputs(evidence.path(), &summary, &sampler, &execution)
                .is_err()
        );
    }
    #[test]
    fn rust_memory_validation_rejects_hard_links_mixed_generations_and_forged_lines() {
        let global_plan = "b".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);

        let hard_link_root = tempfile::Builder::new()
            .prefix("t35-memory-hard-link-")
            .tempdir_in("target")
            .unwrap();
        let evidence = hard_link_root.path().join("evidence");
        std::fs::create_dir(&evidence).unwrap();
        let (sampler, summary) = write_segmented_memory_fixture(&evidence, &execution);
        let first = evidence.join(format!("{MEMORY_RAW_PREFIX}-000000.ndjson"));
        let external = hard_link_root.path().join("external.ndjson");
        std::fs::copy(&first, &external).unwrap();
        std::fs::remove_file(&first).unwrap();
        std::fs::hard_link(&external, &first).unwrap();
        let links = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&first).unwrap().nlink()
            }
            #[cfg(windows)]
            {
                let file = File::open(&first).unwrap();
                u64::from(windows_handle_identity(&file).unwrap().3)
            }
        };
        assert_eq!(links, 2);
        assert!(
            validate_memory_sampler_outputs(&evidence, &summary, &sampler, &execution)
                .unwrap_err()
                .to_string()
                .contains("linked")
        );

        let mixed = tempfile::Builder::new()
            .prefix("t35-memory-mixed-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary) = write_segmented_memory_fixture(mixed.path(), &execution);
        let extra = mixed.path().join("other-memory-raw-v2-999999.ndjson");
        std::fs::write(&extra, b"extra\n").unwrap();
        assert!(
            validate_memory_sampler_outputs(mixed.path(), &summary, &sampler, &execution).is_err()
        );
        std::fs::remove_file(extra).unwrap();
        let extra_summary = mixed.path().join("other-memory-summary-v2.json");
        std::fs::write(&extra_summary, b"{}\n").unwrap();
        assert!(
            validate_memory_sampler_outputs(mixed.path(), &summary, &sampler, &execution).is_err()
        );
        std::fs::remove_file(extra_summary).unwrap();
        let near_match = mixed.path().join("capacity-memory-summary-v20.json");
        std::fs::write(&near_match, b"not a memory artifact\n").unwrap();
        assert!(
            validate_memory_sampler_outputs(mixed.path(), &summary, &sampler, &execution).is_ok()
        );
        std::fs::remove_file(near_match).unwrap();
        let legacy_raw = mixed.path().join(MEMORY_LEGACY_RAW_FILE);
        std::fs::write(&legacy_raw, b"legacy\n").unwrap();
        assert!(
            validate_memory_sampler_outputs(mixed.path(), &summary, &sampler, &execution).is_err()
        );
        std::fs::remove_file(legacy_raw).unwrap();
        let legacy_summary = mixed.path().join(MEMORY_LEGACY_SUMMARY_FILE);
        std::fs::write(&legacy_summary, b"{}\n").unwrap();
        assert!(
            validate_memory_sampler_outputs(mixed.path(), &summary, &sampler, &execution).is_err()
        );
        std::fs::remove_file(legacy_summary).unwrap();

        assert_eq!(
            classify_memory_artifact_name("x-memory-raw-v1.ndjson"),
            Some(MemoryArtifactKind::RawV1)
        );
        assert_eq!(
            classify_memory_artifact_name("x-memory-summary-v1.json"),
            Some(MemoryArtifactKind::SummaryV1)
        );
        assert_eq!(
            classify_memory_artifact_name("x-memory-raw-v2-000000.ndjson"),
            Some(MemoryArtifactKind::RawV2)
        );
        assert_eq!(
            classify_memory_artifact_name("x-memory-summary-v2.json"),
            Some(MemoryArtifactKind::SummaryV2)
        );
        assert_eq!(
            classify_memory_artifact_name("x-memory-summary-v20.json"),
            None
        );
        let broken_namespace = tempfile::Builder::new()
            .prefix("t35-memory-broken-link-")
            .tempdir_in("target")
            .unwrap();
        let broken = broken_namespace.path().join("other-memory-summary-v2.json");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink("missing-summary.json", &broken).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file("missing-summary.json", &broken).is_ok();
        if linked {
            assert!(
                validate_memory_artifact_namespace(broken_namespace.path(), &BTreeSet::new())
                    .is_err()
            );
        }

        let forged = tempfile::Builder::new()
            .prefix("t35-memory-lines-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary_path) = write_segmented_memory_fixture(forged.path(), &execution);
        let first = forged
            .path()
            .join(format!("{MEMORY_RAW_PREFIX}-000000.ndjson"));
        let mut bytes = std::fs::read(&first).unwrap();
        let newline = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        bytes[newline] = b' ';
        std::fs::write(&first, &bytes).unwrap();
        let mut summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        summary["segments"][0]["sha256"] = Value::String(hex::encode(Sha256::digest(&bytes)));
        let concatenated = summary["segments"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|segment| {
                std::fs::read(forged.path().join(segment["path"].as_str().unwrap())).unwrap()
            })
            .collect::<Vec<_>>();
        summary["raw_sha256"] = Value::String(hex::encode(Sha256::digest(&concatenated)));
        std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
        assert!(validate_memory_sampler_outputs(
            forged.path(),
            &summary_path,
            &sampler,
            &execution
        )
        .is_err());
    }

    #[test]
    fn exact_memory_artifact_grammar_is_shared_by_ingestion_and_new_run_preflight() {
        let global_plan = "e".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let evidence = tempfile::Builder::new()
            .prefix("t35-memory-exact-grammar-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary) = write_segmented_memory_fixture(evidence.path(), &execution);
        let ignored_names = [
            "notes-memory-summary-v2.md",
            "guide-memory-raw-v2-format.txt",
            "metrics-memory-raw-v1.csv",
            "capacity-memory-summary-v2.json.bak",
            "capacity-memory-raw-v1.ndjson.tmp",
            "capacity-memory-raw-v2-123456.ndjson.bak",
            "capacity-memory-raw-v2.ndjson",
            "capacity-memory-raw-v2-12345.ndjson",
            "capacity-memory-raw-v2-1234567.ndjson",
            "capacity-memory-raw-v2-12x456.ndjson",
            "-memory-summary-v1.json",
            "-memory-summary-v2.json",
            "-memory-raw-v1.ndjson",
            "-memory-raw-v2-123456.ndjson",
            "directory/capacity-memory-summary-v2.json",
        ];
        for name in ignored_names {
            assert_eq!(classify_memory_artifact_name(name), None, "{name}");
            if name.contains('/') || name.contains('\\') {
                continue;
            }
            let path = evidence.path().join(Path::new(name).file_name().unwrap());
            std::fs::write(&path, b"not a memory artifact\n").unwrap();
            assert!(
                validate_memory_sampler_outputs(evidence.path(), &summary, &sampler, &execution)
                    .is_ok(),
                "{name}"
            );
            std::fs::remove_file(path).unwrap();
        }

        let classified_names = [
            (
                "alternate-memory-summary-v1.json",
                MemoryArtifactKind::SummaryV1,
            ),
            (
                "alternate-memory-summary-v2.json",
                MemoryArtifactKind::SummaryV2,
            ),
            ("alternate-memory-raw-v1.ndjson", MemoryArtifactKind::RawV1),
            (
                "alternate-memory-raw-v2-123456.ndjson",
                MemoryArtifactKind::RawV2,
            ),
        ];
        for (name, kind) in classified_names {
            assert_eq!(classify_memory_artifact_name(name), Some(kind), "{name}");
            let path = evidence.path().join(name);
            std::fs::write(&path, b"undeclared memory artifact\n").unwrap();
            assert!(
                validate_memory_sampler_outputs(evidence.path(), &summary, &sampler, &execution)
                    .is_err(),
                "{name}"
            );
            std::fs::remove_file(path).unwrap();
        }

        let preflight = tempfile::Builder::new()
            .prefix("t35-memory-preflight-grammar-")
            .tempdir_in("target")
            .unwrap();
        for name in ignored_names {
            if name.contains('/') || name.contains('\\') {
                continue;
            }
            let path = preflight.path().join(name);
            std::fs::write(path, b"not a memory artifact\n").unwrap();
        }
        validate_memory_artifact_namespace(preflight.path(), &BTreeSet::new()).unwrap();
        for (name, _) in classified_names {
            let path = preflight.path().join(name);
            std::fs::write(&path, b"undeclared memory artifact\n").unwrap();
            assert!(
                validate_memory_artifact_namespace(preflight.path(), &BTreeSet::new()).is_err(),
                "{name}"
            );
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn authoritative_validator_drains_large_stderr_without_hanging_or_leaking_spools() {
        let evidence = tempfile::Builder::new()
            .prefix("t35-validator-stderr-")
            .tempdir_in("target")
            .unwrap();
        let sampler = evidence.path().join("validator-helper.py");
        std::fs::write(
            &sampler,
            r#"
import json,os,threading,time

def validate_segmented_evidence(summary_path, validated_output):
    def watchdog():
        time.sleep(5)
        os._exit(97)
    threading.Thread(target=watchdog, daemon=True).start()
    import sys
    sys.stderr.buffer.write(b"validator-stderr-start\n")
    block = b"x" * 65536
    for _ in range(256):
        sys.stderr.buffer.write(block)
    sys.stderr.buffer.flush()
    if summary_path.name == "truncated.json":
        header = {
            "byte_length": 65536,
            "name": "capacity-memory-raw-v2-000000.ndjson",
            "order": 0,
            "record_count": 1,
            "sha256": "0" * 64,
        }
        validated_output.write(b"ATLAS_MEMORY_BUNDLE_V1\n")
        validated_output.write(
            b"ATLAS_MEMORY_ARTIFACT "
            + json.dumps(header, separators=(",", ":")).encode()
            + b"\ntruncated"
        )
        validated_output.flush()
        return
    if summary_path.name == "malformed.json":
        validated_output.write(b"NOT_THE_MEMORY_PROTOCOL\n")
        validated_output.flush()
        return
    raise SystemExit(23)
"#,
        )
        .unwrap();
        let execution = capacity_execution_scope(None, &"f".repeat(64));

        for summary_name in ["truncated.json", "malformed.json", "nonzero.json"] {
            let started = std::time::Instant::now();
            let error = validate_memory_sampler_outputs(
                evidence.path(),
                &evidence.path().join(summary_name),
                &sampler,
                &execution,
            )
            .unwrap_err()
            .to_string();
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{summary_name} did not fail within the fixed bound"
            );
            assert!(error.contains("validator-stderr-start"), "{error}");
            assert!(
                error.len() <= SAMPLER_DIAGNOSTIC_MAX_BYTES + 1_024,
                "{}-byte error was not bounded",
                error.len()
            );
            assert!(!error.contains(CAPACITY_LOG_PREFIX), "{error}");
            assert!(!std::fs::read_dir(evidence.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".atlas-memory-log-spool-")
            }));
        }
    }

    #[test]
    fn validator_wait_failure_kills_reaps_then_joins_or_leaves_drainer_detached() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct InjectedChild {
            events: Rc<RefCell<Vec<&'static str>>>,
            reap_error: Option<std::io::Error>,
            kill_error: Option<std::io::Error>,
            try_reap: Option<std::io::Result<Option<&'static str>>>,
        }

        impl MemoryValidatorChildCleanup for InjectedChild {
            type Status = &'static str;

            fn kill_for_cleanup(&mut self) -> std::io::Result<()> {
                self.events.borrow_mut().push("kill");
                match self.kill_error.take() {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            }

            fn wait_for_cleanup(&mut self) -> std::io::Result<Self::Status> {
                self.events.borrow_mut().push("reap");
                match self.reap_error.take() {
                    Some(error) => Err(error),
                    None => Ok("exit status: 23"),
                }
            }

            fn try_wait_for_cleanup(&mut self) -> std::io::Result<Option<Self::Status>> {
                self.events.borrow_mut().push("try-reap");
                self.try_reap
                    .take()
                    .expect("unexpected extra nonblocking reap attempt")
            }
        }

        let events = Rc::new(RefCell::new(vec!["initial-wait-error"]));
        let mut child = InjectedChild {
            events: Rc::clone(&events),
            reap_error: None,
            kill_error: None,
            try_reap: None,
        };
        let join_events = Rc::clone(&events);
        let error = recover_memory_validator_wait_failure(
            std::io::Error::other("injected initial wait failure"),
            &mut child,
            move || {
                join_events.borrow_mut().push("stderr-join");
                Ok(Ok(()))
            },
        )
        .to_string();
        assert_eq!(
            *events.borrow(),
            ["initial-wait-error", "kill", "reap", "stderr-join"]
        );
        assert!(error.contains("injected initial wait failure"), "{error}");
        assert!(error.contains("kill=ok"), "{error}");
        assert!(error.contains("reap=exit status: 23"), "{error}");
        assert!(error.contains("drainer=completed"), "{error}");

        let events = Rc::new(RefCell::new(vec!["initial-wait-error"]));
        let mut child = InjectedChild {
            events: Rc::clone(&events),
            reap_error: Some(std::io::Error::from(std::io::ErrorKind::Interrupted)),
            kill_error: None,
            try_reap: None,
        };
        let join_events = Rc::clone(&events);
        let error = recover_memory_validator_wait_failure(
            std::io::Error::other("injected initial wait failure"),
            &mut child,
            move || {
                join_events.borrow_mut().push("stderr-join");
                Ok(Ok(()))
            },
        )
        .to_string();
        assert_eq!(
            *events.borrow(),
            ["initial-wait-error", "kill", "reap", "reap", "stderr-join"]
        );
        assert!(error.contains("reap_attempts=2"), "{error}");

        let events = Rc::new(RefCell::new(vec!["initial-wait-error"]));
        let mut child = InjectedChild {
            events: Rc::clone(&events),
            reap_error: Some(std::io::Error::other(
                "blocking reap must not follow kill failure",
            )),
            kill_error: Some(std::io::Error::other("injected kill failure")),
            try_reap: Some(Ok(None)),
        };
        let join_events = Rc::clone(&events);
        let error = recover_memory_validator_wait_failure(
            std::io::Error::other("injected initial wait failure"),
            &mut child,
            move || {
                join_events.borrow_mut().push("stderr-join");
                Ok(Ok(()))
            },
        )
        .to_string();
        assert_eq!(*events.borrow(), ["initial-wait-error", "kill", "try-reap"]);
        assert!(
            error.contains("kill=error: injected kill failure"),
            "{error}"
        );
        assert!(error.contains("reap=still running"), "{error}");
        assert!(error.contains("reap_method=try_wait"), "{error}");
        assert!(error.contains("reap_attempts=1"), "{error}");
        assert!(error.contains("drainer=not joined"), "{error}");

        let events = Rc::new(RefCell::new(vec!["initial-wait-error"]));
        let mut child = InjectedChild {
            events: Rc::clone(&events),
            reap_error: Some(std::io::Error::other(
                "blocking reap must not follow kill failure",
            )),
            kill_error: Some(std::io::Error::other("injected kill failure")),
            try_reap: Some(Ok(Some("exit status: 23"))),
        };
        let join_events = Rc::clone(&events);
        let error = recover_memory_validator_wait_failure(
            std::io::Error::other("injected initial wait failure"),
            &mut child,
            move || {
                join_events.borrow_mut().push("stderr-join");
                Ok(Ok(()))
            },
        )
        .to_string();
        assert_eq!(
            *events.borrow(),
            ["initial-wait-error", "kill", "try-reap", "stderr-join"]
        );
        assert!(error.contains("reap=exit status: 23"), "{error}");
        assert!(error.contains("reap_method=try_wait"), "{error}");
        assert!(error.contains("reap_attempts=1"), "{error}");
        assert!(error.contains("drainer=completed"), "{error}");

        let events = Rc::new(RefCell::new(vec!["initial-wait-error"]));
        let mut child = InjectedChild {
            events: Rc::clone(&events),
            reap_error: Some(std::io::Error::other(
                "blocking reap must not follow kill failure",
            )),
            kill_error: Some(std::io::Error::other("injected kill failure")),
            try_reap: Some(Err(std::io::Error::other("injected try-reap failure"))),
        };
        let join_events = Rc::clone(&events);
        let error = recover_memory_validator_wait_failure(
            std::io::Error::other("injected initial wait failure"),
            &mut child,
            move || {
                join_events.borrow_mut().push("stderr-join");
                Ok(Ok(()))
            },
        )
        .to_string();
        assert_eq!(*events.borrow(), ["initial-wait-error", "kill", "try-reap"]);
        assert!(
            error.contains("reap=error: injected try-reap failure"),
            "{error}"
        );
        assert!(error.contains("reap_method=try_wait"), "{error}");
        assert!(error.contains("reap_attempts=1"), "{error}");
        assert!(error.contains("drainer=not joined"), "{error}");
    }

    #[test]
    fn rust_memory_validation_rejects_segment_descriptor_count_above_limit() {
        let global_plan = "d".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let evidence = tempfile::Builder::new()
            .prefix("t35-memory-segment-limit-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary_path) = write_segmented_memory_fixture(evidence.path(), &execution);
        let mut summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        let template = summary["segments"][0].clone();
        let segments = summary["segments"].as_array_mut().unwrap();
        while segments.len() <= MEMORY_MAX_SEGMENTS {
            segments.push(template.clone());
        }
        std::fs::write(&summary_path, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert!(validate_memory_sampler_outputs(
            evidence.path(),
            &summary_path,
            &sampler,
            &execution,
        )
        .is_err());
    }

    #[test]
    fn memory_logging_consumes_the_same_validated_handle_after_path_replacement() {
        let global_plan = "c".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let evidence = tempfile::Builder::new()
            .prefix("t35-memory-same-handle-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary_path) = write_segmented_memory_fixture(evidence.path(), &execution);
        let validated =
            validate_memory_sampler_outputs(evidence.path(), &summary_path, &sampler, &execution)
                .unwrap();
        let expected_sha256 = validated.summary["segments"][0]["sha256"]
            .as_str()
            .unwrap()
            .to_string();
        let first = validated.segments.into_iter().next().unwrap();
        let retained_path = evidence.path().join("retained-open-object.ndjson");
        std::fs::rename(&first.path, &retained_path).unwrap();
        std::fs::write(&first.path, b"{\"kind\":\"replacement\"}\n").unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scenario =
            load_scenario(&root.join("tests/fixtures/context_yield/benchmark-scenario.json"))
                .unwrap();
        let mut consumed_digest = Sha256::new();
        let stream = prepare_capacity_log_stream(
            CapacityLogSourceSpec {
                path: &first.path,
                file: Some(first.file),
                aggregate_digest: Some(&mut consumed_digest),
                name: MEMORY_RAW_PREFIX,
                maximum_bytes: MEMORY_MAX_RAW_BYTES,
                expected_kind: "process-tree-memory-sample",
                ndjson: true,
            },
            &root,
            evidence.path(),
            &scenario,
            None,
            None,
        )
        .unwrap();
        assert_eq!(stream.source_sha256, expected_sha256);
        assert_eq!(hex::encode(consumed_digest.finalize()), expected_sha256);
    }

    #[test]
    fn memory_logging_rechecks_live_links_for_segment_and_summary_handles() {
        let global_plan = "e".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scenario =
            load_scenario(&root.join("tests/fixtures/context_yield/benchmark-scenario.json"))
                .unwrap();

        for artifact in ["segment", "summary"] {
            let evidence = tempfile::Builder::new()
                .prefix("t35-memory-live-link-")
                .tempdir_in("target")
                .unwrap();
            let (sampler, summary_path) =
                write_segmented_memory_fixture(evidence.path(), &execution);
            let mut validated = validate_memory_sampler_outputs(
                evidence.path(),
                &summary_path,
                &sampler,
                &execution,
            )
            .unwrap();
            let external = evidence.path().join(format!("external-{artifact}"));
            let result = if artifact == "segment" {
                let segment = validated.segments.remove(0);
                std::fs::hard_link(&segment.path, &external).unwrap();
                prepare_capacity_log_stream(
                    CapacityLogSourceSpec {
                        path: &segment.path,
                        file: Some(segment.file),
                        aggregate_digest: None,
                        name: MEMORY_RAW_PREFIX,
                        maximum_bytes: MEMORY_MAX_RAW_BYTES,
                        expected_kind: "process-tree-memory-sample",
                        ndjson: true,
                    },
                    &root,
                    evidence.path(),
                    &scenario,
                    None,
                    None,
                )
            } else {
                std::fs::hard_link(&validated.summary_path, &external).unwrap();
                prepare_capacity_log_stream(
                    CapacityLogSourceSpec {
                        path: &summary_path,
                        file: Some(validated.summary_file),
                        aggregate_digest: None,
                        name: MEMORY_SUMMARY_FILE,
                        maximum_bytes: MEMORY_MAX_SUMMARY_BYTES,
                        expected_kind: "process-tree-memory-segmented-summary",
                        ndjson: false,
                    },
                    &root,
                    evidence.path(),
                    &scenario,
                    None,
                    None,
                )
            };
            let error = match result {
                Ok(_) => panic!("linked {artifact} was accepted"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("linked"));
        }
    }

    #[test]
    fn memory_logging_rejects_resealed_semantically_invalid_generation() {
        let global_plan = "f".repeat(64);
        let execution = capacity_execution_scope(None, &global_plan);
        let evidence = tempfile::Builder::new()
            .prefix("t35-memory-generation-swap-")
            .tempdir_in("target")
            .unwrap();
        let (sampler, summary_path) = write_segmented_memory_fixture(evidence.path(), &execution);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        validate_memory_log_evidence(&root, &summary_path).unwrap();

        let first = evidence
            .path()
            .join(format!("{MEMORY_RAW_PREFIX}-000000.ndjson"));
        let bytes = std::fs::read(&first).unwrap();
        let newline = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        let mut record: Value = serde_json::from_slice(&bytes[..newline]).unwrap();
        record["monotonic_ns"] = Value::from(-1_i64);
        let mut mutated = serde_json::to_vec(&record).unwrap();
        mutated.push(b'\n');
        mutated.extend_from_slice(&bytes[newline + 1..]);
        std::fs::write(&first, &mutated).unwrap();

        let mut summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        summary["segments"][0]["byte_length"] = Value::from(mutated.len() as u64);
        summary["segments"][0]["sha256"] = Value::String(hex::encode(Sha256::digest(&mutated)));
        let concatenated = summary["segments"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|segment| {
                std::fs::read(evidence.path().join(segment["path"].as_str().unwrap())).unwrap()
            })
            .collect::<Vec<_>>();
        summary["raw_byte_length"] = Value::from(concatenated.len() as u64);
        summary["raw_sha256"] = Value::String(hex::encode(Sha256::digest(&concatenated)));
        let mut summary_bytes = serde_json::to_vec_pretty(&summary).unwrap();
        summary_bytes.push(b'\n');
        std::fs::write(&summary_path, summary_bytes).unwrap();

        assert!(validate_memory_sampler_outputs(
            evidence.path(),
            &summary_path,
            &sampler,
            &execution,
        )
        .is_err());
    }

    #[test]
    fn progress_publications_are_bounded_and_preserve_phase_and_completion() {
        let resolved = CapacityProviderCondition::TypeScriptOnly.resolve(CapacityDataset::Small);
        let mut policy = CapacityMemoryPublicationPolicy::default();
        let mut publication_count = 0_u64;
        let mut phase_transition_published = false;
        let mut labels = CapacityMemoryLabels::for_attempt(
            CapacityPhase::CapabilityDiscovery.descriptor(),
            &resolved,
            "warm",
            1,
            CAPACITY_PROGRESS_TOTAL,
        )
        .unwrap();
        for ordinal in 1..=CAPACITY_PROGRESS_TOTAL {
            if ordinal == 67_500 {
                labels = CapacityMemoryLabels::for_attempt(
                    CapacityPhase::DirectRouteCompiler.descriptor(),
                    &resolved,
                    "warm",
                    ordinal,
                    CAPACITY_PROGRESS_TOTAL,
                )
                .unwrap();
            } else {
                labels.progress_ordinal = ordinal;
            }
            if policy.publication_required(&labels).unwrap() {
                policy.record_publication(&labels);
                publication_count += 1;
                if ordinal == 67_500 {
                    phase_transition_published = true;
                }
            }
        }
        let completed = CapacityMemoryLabels::completed(
            CapacityPhase::DirectRouteCompiler.descriptor(),
            &resolved,
            "warm",
            CAPACITY_PROGRESS_TOTAL,
        )
        .unwrap();
        assert!(policy.publication_required(&completed).unwrap());
        policy.record_publication(&completed);
        publication_count += 1;
        assert!(phase_transition_published);
        assert_eq!(completed.progress_state, "complete");
        assert_eq!(completed.progress_ordinal, CAPACITY_PROGRESS_TOTAL);
        assert!(
            publication_count < 200,
            "{publication_count} physical publications are not bounded"
        );
    }

    #[test]
    fn shard_progress_is_bounded_and_closes_at_33750_without_global_completion() {
        let resolved = CapacityProviderCondition::BuiltinOnly.resolve(CapacityDataset::Medium);
        let mut policy = CapacityMemoryPublicationPolicy::default();
        let active = CapacityMemoryLabels::for_attempt(
            CapacityPhase::CatalogueGrowth.descriptor(),
            &resolved,
            "cold",
            CAPACITY_SHARD_ATTEMPTS - 1,
            CAPACITY_SHARD_ATTEMPTS,
        )
        .unwrap();
        assert!(policy.publication_required(&active).unwrap());
        policy.record_publication(&active);
        let complete = CapacityMemoryLabels::completed(
            CapacityPhase::CatalogueGrowth.descriptor(),
            &resolved,
            "cold",
            CAPACITY_SHARD_ATTEMPTS,
        )
        .unwrap();
        assert!(policy.publication_required(&complete).unwrap());
        assert_eq!(complete.progress_ordinal, 33_750);
        assert_eq!(complete.progress_total, 33_750);
        assert_ne!(complete.progress_total, CAPACITY_PROGRESS_TOTAL);
    }

    #[test]
    fn sampler_progress_is_filtered_bounded_and_visible_before_early_exit() {
        let evidence = tempfile::Builder::new()
            .prefix("t34-progress-forwarding-")
            .tempdir()
            .unwrap();
        let stop = evidence.path().join("stop");
        let progress_path = evidence.path().join("progress.log");
        let progress_file = File::create(&progress_path).unwrap();
        let stop_argument = stop.to_string_lossy().into_owned();
        let sampler_argument = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts/process-tree-memory.py")
            .to_string_lossy()
            .into_owned();
        let handle = thread::spawn(move || {
            let python = if cfg!(windows) { "python" } else { "python3" };
            let script = concat!(
                "import importlib.util,pathlib,sys,time\n",
                "spec=importlib.util.spec_from_file_location('process_tree_memory',sys.argv[1])\n",
                "module=importlib.util.module_from_spec(spec)\n",
                "sys.modules[spec.name]=module\n",
                "spec.loader.exec_module(module)\n",
                "labels={'filesystem_cache_state':'atlas-warm','serving_state':'not-applicable',",
                "'provider_cache_install_state':'not-applicable','build_state':'prebuilt-atlas-bench',",
                "'catalogue_state':'none'}\n",
                "reporter=module.ProgressReporter()\n",
                "reporter.observe({**labels,'phase':'harness-startup','progress_state':'starting',",
                "'progress_ordinal':0,'progress_total':135000})\n",
                "reporter.observe({**labels,'phase':'capability-discovery','progress_state':'active',",
                "'progress_ordinal':42,'progress_total':135000})\n",
                "stop=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10\n",
                "while not stop.exists() and time.monotonic()<deadline:time.sleep(0.01)\n",
                "code=23 if stop.exists() else 24\n",
                "reporter.finish(code)\n",
                "raise SystemExit(code)\n",
            );
            let mut command = Command::new(python);
            command.args(["-B", "-c", script, &sampler_argument, &stop_argument]);
            run_sampler_command(&mut command, progress_file)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(&progress_path).unwrap_or_default();
            if text.contains("ordinal=42") {
                break;
            }
            assert!(Instant::now() < deadline, "progress was not forwarded live");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!handle.is_finished());
        std::fs::write(&stop, b"stop").unwrap();
        let output = handle.join().unwrap().unwrap();
        assert_eq!(output.status.code(), Some(23));
        let forwarded = std::fs::read_to_string(&progress_path).unwrap();
        assert!(forwarded.contains("state=starting ordinal=0 total=135000"));
        assert!(forwarded.contains("state=active ordinal=42 total=135000"));
        assert!(forwarded.contains("state=stopped ordinal=42 total=135000"));
        assert!(output.stdout.len() <= SAMPLER_DIAGNOSTIC_MAX_BYTES);
        assert!(!std::fs::read_dir(evidence.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{MEMORY_RAW_PREFIX}-"))
        }));
        assert!(!evidence.path().join(MEMORY_SUMMARY_FILE).exists());
    }

    #[test]
    fn progress_protocol_rejects_non_neutral_or_out_of_bound_lines() {
        assert!(parse_capacity_progress_line(
            "ATLAS_CAPACITY_PROGRESS state=complete ordinal=135000 total=135000 phase=catalogue-growth"
        )
        .is_some());
        assert!(parse_capacity_progress_line(
            "ATLAS_CAPACITY_PROGRESS state=complete ordinal=33750 total=33750 phase=catalogue-growth"
        )
        .is_some());
        for line in [
            "ATLAS_CAPACITY_PROGRESS state=active ordinal=135001 total=135000 phase=catalogue-growth",
            "ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=134999 phase=catalogue-growth",
            "ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=33749 phase=catalogue-growth",
            "ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=135000 phase=private/path",
            "ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=135000 phase=catalogue-growth secret=value",
            concat!("D", ":/private/evidence"),
        ] {
            assert!(
                parse_capacity_progress_line(line).is_none(),
                "unsafe progress line accepted: {line}"
            );
        }
    }

    #[test]
    fn malformed_progress_is_rejected_without_echoing_private_payload() {
        let mut forwarded = Vec::new();
        let error = consume_sampler_stdout(
            std::io::Cursor::new(
                b"ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=135000 phase=private/path\n",
            ),
            &mut forwarded,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "malformed capacity progress protocol");
        assert!(!error.to_string().contains("private/path"));
        assert!(forwarded.is_empty());
    }

    #[derive(Default)]
    struct TestExecutor {
        preparations: Vec<(String, CapacityPhase)>,
        executions: Vec<(String, CapacityPhase, u64)>,
        perturb_first_required: bool,
        perturbed: bool,
        fail_first_preparation: bool,
        preparation_failed: bool,
        accepted_outcomes: Option<CapacityAcceptedOutcomeContract>,
    }

    struct TestPrepared {
        lifecycle_id: String,
        phase: CapacityPhase,
        manifest: Value,
        calls: u64,
        mutation_applied: bool,
    }

    impl CapacityExecutor for TestExecutor {
        type Prepared = TestPrepared;
        const EVIDENCE_NAMES: CapacityEvidenceNames = TEST_CAPACITY_EVIDENCE;

        fn prepare(
            &mut self,
            spec: CapacityPreparationSpec<'_>,
        ) -> CapacityPreparation<Self::Prepared> {
            self.preparations
                .push((spec.lifecycle_id.to_string(), spec.phase));
            if self.fail_first_preparation && !self.preparation_failed {
                self.preparation_failed = true;
                return CapacityPreparation::Failed(failed_capacity_preparation(
                    AtlasError::Other("injected required lifecycle preparation failure".into()),
                ));
            }
            let configuration_hash = match spec.provider_condition.configuration_hash() {
                Ok(configuration_hash) => configuration_hash,
                Err(error) => {
                    return CapacityPreparation::Failed(failed_capacity_preparation(error));
                }
            };
            if self.accepted_outcomes.is_none() {
                let path = resolve_capacity_directory(
                    spec.repository_root,
                    Path::new(&spec.scenario.capacity.manifest_directory),
                )
                .join(&spec.scenario.capacity.accepted_outcome_contract.path);
                let contract = std::fs::read(&path)
                    .map_err(AtlasError::from)
                    .and_then(|raw| serde_json::from_slice(&raw).map_err(AtlasError::from));
                match contract {
                    Ok(contract) => self.accepted_outcomes = Some(contract),
                    Err(error) => {
                        return CapacityPreparation::Failed(failed_capacity_preparation(error));
                    }
                }
            }
            CapacityPreparation::Ready(ReadyCapacityPreparation {
                prepared: TestPrepared {
                    lifecycle_id: spec.lifecycle_id.to_string(),
                    phase: spec.phase,
                    manifest: spec.manifest.clone(),
                    calls: 0,
                    mutation_applied: false,
                },
                configuration_hash,
            })
        }

        fn execute(
            &mut self,
            prepared: &mut Self::Prepared,
            mut observation: CapacityRawObservation,
        ) -> CapacityRawObservation {
            assert_eq!(prepared.lifecycle_id, observation.lifecycle_id);
            assert!(!prepared.mutation_applied, "phase state was not reset");
            let descriptor = prepared.phase.descriptor();
            prepared.mutation_applied = prepared.phase == CapacityPhase::FixedIncrementalReconcile;
            prepared.calls += 1;
            self.executions.push((
                prepared.lifecycle_id.clone(),
                prepared.phase,
                prepared.calls,
            ));
            let provider = resolve_capacity_provider_condition(
                &observation.dataset_name,
                &observation.provider_condition,
            )
            .unwrap();
            if !provider.is_applicable() {
                observation.outcome = "unsupported".to_string();
                observation.required_provider_failure = Some(false);
                prepared.mutation_applied = false;
                return observation;
            }
            observation.outcome = "success".to_string();
            observation.correctness = Some(true);
            observation.deterministic_identity = Some(identity_hash(&[
                &observation.dataset_name,
                &observation.profile,
                &observation.provider_condition,
                &observation.phase,
                "test-observed-result",
            ]));
            observation.deterministic_work_units = Some(0);
            if descriptor.catalogue_measurements {
                let accepted_outcome = capacity_accepted_outcome(
                    &prepared.manifest,
                    self.accepted_outcomes
                        .as_ref()
                        .expect("test accepted outcomes loaded"),
                    provider.name(),
                    prepared.phase,
                )
                .unwrap();
                observation.observed_target_identity = Some(observation.target_identity.clone());
                observation.observed_evidence_counts =
                    Some(accepted_outcome.evidence_counts.clone());
                observation.accepted_outcome = canonical_json_sha256(&serde_json::json!({
                    "dataset_name": observation.dataset_name,
                    "evidence_counts": observation.observed_evidence_counts,
                    "target_identity": observation.observed_target_identity,
                }))
                .ok();
                observation.eligible_files = Some(observation.expected_eligible_files);
                observation.indexed_files = Some(observation.expected_indexed_files);
                observation.source_bytes = Some(observation.expected_source_bytes);
                observation.provider_probes = Some(0);
                observation.provider_executions_run = Some(0);
                observation.provider_outputs = Some(0);
                observation.provider_output_bytes = Some(0);
                observation.provider_executions_reused = Some(0);
                observation.required_provider_failure = Some(false);
                observation.configuration_hash = observation.expected_configuration_hash.clone();
                observation.durable_catalogue_rows_before = Some(10);
                observation.durable_catalogue_rows_after = Some(11);
                observation.durable_catalogue_bytes_before = Some(4096);
                observation.durable_catalogue_bytes_after = Some(4096);
                observation.derived_catalogue_rows_before = Some(0);
                observation.derived_catalogue_rows_after = Some(1);
                observation.derived_catalogue_bytes_before = Some(0);
                observation.derived_catalogue_bytes_after = Some(4096);
                observation.catalogue_rows_before = Some(10);
                observation.catalogue_rows_after = Some(12);
                observation.catalogue_bytes_before = Some(4096);
                observation.catalogue_bytes_after = Some(8192);
                match descriptor.reconcile_measurements {
                    CapacityReconcileMeasurements::NotApplicable => {}
                    CapacityReconcileMeasurements::Observed
                    | CapacityReconcileMeasurements::NoChange => {
                        observation.parsed_files = Some(0);
                        observation.changed_files = Some(0);
                        observation.changed_bytes = Some(0);
                    }
                    CapacityReconcileMeasurements::FixedIncremental => {
                        observation.parsed_files = Some(if prepared.calls == 1 {
                            observation.expected_changed_files.saturating_sub(1)
                        } else {
                            0
                        });
                        observation.changed_files = Some(observation.expected_changed_files);
                        observation.changed_bytes = Some(observation.expected_changed_bytes);
                        observation.provider_probes = Some(1);
                        observation.provider_executions_reused = Some(1);
                    }
                }
                if let Some(expected_fallback) = descriptor.compiler_fallback {
                    observation.compiler_selected_records = Some(3);
                    observation.compiler_selected_source_bytes = Some(100);
                    observation.compiler_selected_estimated_tokens = Some(25);
                    observation.compiler_omitted_records = Some(2);
                    observation.compiler_truncated = Some(true);
                    observation.compiler_work_units_consumed = Some(5);
                    observation.compiler_fallback = Some(expected_fallback);
                    observation.deterministic_work_units = Some(
                        if prepared.phase == CapacityPhase::ProgressiveDeepRouteCompiler {
                            6
                        } else {
                            5
                        },
                    );
                }
                if descriptor.ready_truth_measurements {
                    observation.ready_versus_truth = observation
                        .accepted_outcome
                        .as_deref()
                        .and_then(|accepted| ready_truth_identity(accepted, true).ok())
                        .flatten();
                }
            }
            if self.perturb_first_required && !self.perturbed {
                observation.correctness = Some(false);
                self.perturbed = true;
            }
            observation.atlas_runtime_duration_ms = Some(17);
            observation.wall_duration_ms = Some(19);
            prepared.mutation_applied = false;
            observation
        }
    }

    fn scenario_and_manifest() -> (PathBuf, BenchmarkScenario, Value) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scenario =
            load_scenario(&root.join("tests/fixtures/context_yield/benchmark-scenario.json"))
                .unwrap();
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(root.join("tests/fixtures/capacity/repo-small-v1.json")).unwrap(),
        )
        .unwrap();
        (root, scenario, manifest)
    }

    fn small_builtin_provider() -> ResolvedCapacityProviderCondition {
        resolve_capacity_provider_condition("repo-small-v1", "builtin-only").unwrap()
    }

    fn guarded_raw_writer(
        directory: &Path,
        maximum_records: u64,
        name: &str,
    ) -> (CapacityDirectoryGuard, CapacityRawWriter) {
        let guard = capacity_directory_guard(directory).unwrap();
        let writer = CapacityRawWriter::create(directory, &guard, maximum_records, name).unwrap();
        (guard, writer)
    }

    fn test_observation(phase: CapacityPhase) -> CapacityRawObservation {
        let (root, scenario, manifest) = scenario_and_manifest();
        let phase_name = phase.descriptor().name;
        let provider =
            resolve_capacity_provider_condition("repo-small-v1", "builtin-only").unwrap();
        let mut executor = TestExecutor::default();
        let sample_class = if phase == CapacityPhase::FixedIncrementalReconcile {
            "warmup"
        } else {
            "warm"
        };
        let lifecycle_id = phase
            .descriptor()
            .lifecycle_id(
                "repo-small-v1",
                &scenario.profiles[0].name,
                provider.name(),
                1,
                sample_class,
                1,
            )
            .unwrap();
        let mut prepared = executor.prepare(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase,
            lifecycle_id: &lifecycle_id,
        });
        let accepted_outcome = if phase.descriptor().catalogue_measurements {
            Some(
                capacity_accepted_outcome(
                    &manifest,
                    executor
                        .accepted_outcomes
                        .as_ref()
                        .expect("test accepted outcomes loaded"),
                    provider.name(),
                    phase,
                )
                .unwrap(),
            )
        } else {
            None
        };
        let empty = empty_capacity_observation(
            "a",
            &manifest,
            accepted_outcome,
            &scenario.profiles[0],
            CapacityAttemptSpec {
                evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: prepared.configuration_hash().map(str::to_owned),
                phase: phase_name,
                repetition: 1,
                sample_class,
                sample_index: 1,
                lifecycle_id,
            },
        )
        .unwrap();
        execute_capacity_preparation(&mut executor, &mut prepared, empty)
    }

    #[test]
    fn compiler_capacity_raw_evidence_requires_selected_cost_and_truncation() {
        let (_, scenario, _) = scenario_and_manifest();
        let mut valid =
            serde_json::to_value(test_observation(CapacityPhase::ReadyServingQueryCompiler))
                .unwrap();
        valid["kind"] = Value::from(PRODUCTION_CAPACITY_EVIDENCE.raw_kind);
        assert!(validate_capacity_raw_record(&valid, &scenario, &mut BTreeSet::new()).is_ok());
        for field in [
            "compiler_selected_source_bytes",
            "compiler_selected_estimated_tokens",
            "compiler_truncated",
        ] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(
                validate_capacity_raw_record(&invalid, &scenario, &mut BTreeSet::new()).is_err(),
                "compiler raw evidence without {field} was accepted"
            );
        }
        for field in CAPACITY_COMPILER_MEASUREMENT_FIELDS {
            let mut invalid = valid.clone();
            invalid[*field] = Value::Null;
            assert!(
                validate_capacity_raw_record(&invalid, &scenario, &mut BTreeSet::new()).is_err(),
                "compiler raw evidence with null {field} was accepted"
            );
        }
        for invalid in [
            {
                let mut value = valid.clone();
                value["compiler_truncated"] = Value::from(false);
                value
            },
            {
                let mut value = valid.clone();
                value["compiler_selected_source_bytes"] = value["expected_source_bytes"].clone();
                value
            },
            {
                let mut value = valid.clone();
                value["compiler_selected_estimated_tokens"] = Value::from(
                    value["profile_inputs"]["estimated_tokens"]
                        .as_u64()
                        .unwrap()
                        + 1,
                );
                value
            },
            {
                let mut value = valid.clone();
                value["compiler_work_units_consumed"] = Value::from(6);
                value
            },
        ] {
            assert!(
                validate_capacity_raw_record(&invalid, &scenario, &mut BTreeSet::new()).is_err(),
                "inconsistent compiler raw evidence was accepted: {invalid:#}"
            );
        }
    }

    #[test]
    fn production_provider_contract_drives_membership_and_config() {
        for dataset in CapacityDataset::ALL {
            for condition in CapacityProviderCondition::ALL {
                let resolved = condition.resolve(dataset);
                let expected = resolved.providers().collect::<Vec<_>>();
                assert_eq!(
                    resolved.members(),
                    expected
                        .iter()
                        .map(|provider| format!("{}@{}", provider.name, provider.version))
                        .collect::<Vec<_>>()
                );
                let config = capacity_config(&resolved).unwrap();
                assert_eq!(config.providers.len(), expected.len());
                for (configured, descriptor) in config.providers.iter().zip(&expected) {
                    assert_eq!(configured.name, descriptor.name);
                    assert_eq!(
                        configured.resolved_version().as_deref(),
                        Some(descriptor.version)
                    );
                    assert_eq!(configured.kind, descriptor.kind);
                    assert_eq!(configured.tier, descriptor.tier);
                    assert_eq!(configured.scope, descriptor.scope);
                    assert_eq!(configured.required, descriptor.required);
                    assert_eq!(configured.priority, descriptor.priority);
                    assert_eq!(
                        configured.languages,
                        descriptor
                            .languages
                            .iter()
                            .map(|value| value.to_string())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        configured.project_markers,
                        descriptor
                            .project_markers
                            .iter()
                            .map(|value| value.to_string())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(configured.command.as_deref(), descriptor.command);
                    assert_eq!(
                        configured.arguments,
                        descriptor
                            .arguments
                            .iter()
                            .map(|value| value.to_string())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        configured.probe_arguments,
                        descriptor
                            .probe_arguments
                            .iter()
                            .map(|value| value.to_string())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        configured.output_format.as_deref(),
                        descriptor.output_format
                    );
                }
            }
        }
    }

    #[test]
    fn sharded_scheduler_retains_complete_local_evidence_and_exact_identity_set() {
        let (root, scenario, _) = scenario_and_manifest();
        let evidence = tempfile::Builder::new()
            .prefix("t34-shard-campaign-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let shard = CapacityShard { index: 2 };
        run_capacity_campaign_with_executor(
            &root,
            &root.join("tests/fixtures/context_yield/benchmark-scenario.json"),
            &scenario,
            &root.join("tests/fixtures/capacity"),
            evidence.path(),
            None,
            Some(shard),
            &mut TestExecutor::default(),
        )
        .unwrap();

        let raw_count = std::io::BufReader::new(
            File::open(evidence.path().join(TEST_CAPACITY_EVIDENCE.raw_file)).unwrap(),
        )
        .lines()
        .count();
        assert_eq!(raw_count, CAPACITY_SHARD_ATTEMPTS as usize);
        let summary: Value = serde_json::from_slice(
            &std::fs::read(evidence.path().join(TEST_CAPACITY_EVIDENCE.summary_file)).unwrap(),
        )
        .unwrap();
        assert_eq!(summary["attempted_records"], CAPACITY_SHARD_ATTEMPTS);
        assert_eq!(summary["maximum_raw_records"], CAPACITY_SHARD_ATTEMPTS);
        assert_eq!(
            summary["condition_summaries"].as_array().unwrap().len(),
            (CAPACITY_SHARD_MATRIX_ROWS * scenario.procedure.repetitions) as usize
        );
        assert_eq!(summary["provenance"]["execution_scope"]["shard_index"], 2);

        let mut production_summary = summary;
        production_summary["kind"] = Value::from(PRODUCTION_CAPACITY_EVIDENCE.summary_kind);
        let global_plan_sha256 = production_summary["provenance"]["global_plan_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        validate_capacity_log_summary(
            &production_summary,
            &scenario,
            Some(shard),
            &global_plan_sha256,
        )
        .unwrap();
        production_summary["provenance"]["execution_scope"]["shard_index"] = Value::from(1);
        production_summary["provenance_sha256"] = Value::from(hex::encode(Sha256::digest(
            serde_json::to_vec(&production_summary["provenance"]).unwrap(),
        )));
        assert!(validate_capacity_log_summary(
            &production_summary,
            &scenario,
            Some(shard),
            &global_plan_sha256,
        )
        .is_err());
    }

    #[test]
    fn in_process_scheduler_reuses_warm_state_and_isolates_cold_and_repetitions() {
        let (root, scenario, _) = scenario_and_manifest();
        let evidence = tempfile::Builder::new()
            .prefix("t30b-in-process-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let mut executor = TestExecutor {
            perturb_first_required: true,
            ..TestExecutor::default()
        };
        let failure = run_capacity_campaign_with_executor(
            &root,
            &root.join("tests/fixtures/context_yield/benchmark-scenario.json"),
            &scenario,
            &root.join("tests/fixtures/capacity"),
            evidence.path(),
            None,
            None,
            &mut executor,
        )
        .unwrap_err();
        assert!(failure
            .to_string()
            .contains("after retaining complete raw evidence"));
        let raw_path = evidence.path().join(TEST_CAPACITY_EVIDENCE.raw_file);
        let summary_path = evidence.path().join(TEST_CAPACITY_EVIDENCE.summary_file);
        assert_eq!(
            raw_path.file_name().unwrap(),
            TEST_CAPACITY_EVIDENCE.raw_file
        );
        assert_eq!(
            summary_path.file_name().unwrap(),
            TEST_CAPACITY_EVIDENCE.summary_file
        );
        assert!(!evidence
            .path()
            .join(PRODUCTION_CAPACITY_EVIDENCE.raw_file)
            .exists());
        assert!(!evidence
            .path()
            .join(PRODUCTION_CAPACITY_EVIDENCE.summary_file)
            .exists());
        let raw_count = std::io::BufReader::new(File::open(&raw_path).unwrap())
            .lines()
            .count();
        let summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        assert_eq!(summary["campaign_accepted"], false);
        assert_eq!(summary["validation_failure_count"], 1);
        assert_eq!(
            summary["compiler_measurement_fields"],
            serde_json::json!(CAPACITY_COMPILER_MEASUREMENT_FIELDS)
        );
        let mut production_summary = summary.clone();
        production_summary["kind"] = Value::from(PRODUCTION_CAPACITY_EVIDENCE.summary_kind);
        let global_plan_sha256 = production_summary["provenance"]["global_plan_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        validate_capacity_log_summary(&production_summary, &scenario, None, &global_plan_sha256)
            .unwrap();
        let condition_index = |summary: &Value, phase: &str| {
            summary["condition_summaries"]
                .as_array()
                .unwrap()
                .iter()
                .position(|condition| condition["condition"]["phase"] == phase)
                .unwrap()
        };
        let progressive_index =
            condition_index(&production_summary, "progressive-deep-route-compiler");
        let light_index = condition_index(&production_summary, "light-route-compiler");
        let progressive_compiler = production_summary["condition_summaries"][progressive_index]
            ["compiler_measurements"]
            .clone();
        let mut mutations = Vec::new();
        let mut progressive_compiler_null = production_summary.clone();
        progressive_compiler_null["condition_summaries"][progressive_index]
            ["compiler_measurements"] = Value::Null;
        mutations.push((
            "progressive compiler null with scored successes",
            progressive_compiler_null,
        ));
        let mut light_compiler = production_summary.clone();
        light_compiler["condition_summaries"][light_index]["compiler_measurements"] =
            progressive_compiler;
        mutations.push(("compiler data on LIGHT", light_compiler));
        let mut missing_condition = production_summary.clone();
        missing_condition["condition_summaries"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        mutations.push(("missing condition", missing_condition));
        let mut duplicate_condition = production_summary.clone();
        let duplicate = duplicate_condition["condition_summaries"][0].clone();
        duplicate_condition["condition_summaries"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        mutations.push(("duplicate condition", duplicate_condition));
        let mut unknown_condition = production_summary.clone();
        unknown_condition["condition_summaries"][0]["condition"]["dataset_name"] =
            Value::from("repo-unknown-v1");
        mutations.push(("unknown condition", unknown_condition));
        let mut condition_total_mismatch = production_summary.clone();
        let first_outcome = condition_total_mismatch["condition_summaries"][0]["outcome_counts"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        *first_outcome = Value::from(first_outcome.as_u64().unwrap() + 1);
        mutations.push((
            "per-condition outcome total mismatch",
            condition_total_mismatch,
        ));
        let mut global_reconciliation_mismatch = production_summary.clone();
        let outcomes = global_reconciliation_mismatch["condition_summaries"][0]["outcome_counts"]
            .as_object_mut()
            .unwrap();
        let success = outcomes.get_mut("success").unwrap();
        *success = Value::from(success.as_u64().unwrap() - 1);
        outcomes.insert("timeout".to_string(), Value::from(1));
        mutations.push((
            "global outcome reconciliation mismatch",
            global_reconciliation_mismatch,
        ));
        let accepted_mutations = mutations
            .into_iter()
            .filter_map(|(name, mutation)| {
                validate_capacity_log_summary(&mutation, &scenario, None, &global_plan_sha256)
                    .is_ok()
                    .then_some(name)
            })
            .collect::<Vec<_>>();
        assert!(
            accepted_mutations.is_empty(),
            "capacity summary validator accepted mutations: {accepted_mutations:?}"
        );
        let conditions = summary["condition_summaries"].as_array().unwrap();
        let progressive = conditions
            .iter()
            .find(|condition| {
                condition["condition"]["dataset_name"] == "repo-small-v1"
                    && condition["condition"]["profile"] == "small"
                    && condition["condition"]["provider_condition"] == "builtin-only"
                    && condition["condition"]["phase"] == "progressive-deep-route-compiler"
                    && condition["condition"]["repetition"] == 1
            })
            .unwrap();
        assert_eq!(progressive["compiler_measurements"]["count"], 100);
        assert_eq!(
            progressive["compiler_measurements"]["compiler_selected_source_bytes"]["p50"],
            100
        );
        assert_eq!(
            progressive["compiler_measurements"]["compiler_selected_estimated_tokens"]["p50"],
            25
        );
        assert_eq!(
            progressive["compiler_measurements"]["compiler_truncated"]["true_count"],
            100
        );
        assert_eq!(
            progressive["compiler_measurements"]["compiler_fallback"]["true_count"],
            100
        );
        let light = conditions
            .iter()
            .find(|condition| {
                condition["condition"]["dataset_name"] == "repo-small-v1"
                    && condition["condition"]["profile"] == "small"
                    && condition["condition"]["provider_condition"] == "builtin-only"
                    && condition["condition"]["phase"] == "light-route-compiler"
                    && condition["condition"]["repetition"] == 1
            })
            .unwrap();
        assert!(light["compiler_measurements"].is_null());
        assert_eq!(raw_count, 135_000);
        assert_eq!(summary["raw_sha256"], file_sha256(&raw_path).unwrap());
        let cold_warm_lifecycles = std::io::BufReader::new(File::open(&raw_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .filter(|observation| {
                observation["dataset_name"] == "repo-small-v1"
                    && observation["profile"] == scenario.profiles[0].name
                    && observation["provider_condition"] == "builtin-only"
                    && observation["phase"] == "cold-initialization-reconcile"
                    && observation["repetition"] == 1
                    && matches!(
                        observation["sample_class"].as_str(),
                        Some("warmup" | "warm")
                    )
            })
            .map(|observation| observation["lifecycle_id"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            cold_warm_lifecycles.len(),
            105,
            "each cold-initialization action needs a distinct lifecycle identity"
        );

        assert_eq!(executor.preparations.len(), 33_912);
        let mut lifecycle_calls = BTreeMap::<String, u64>::new();
        for (lifecycle_id, _, call) in &executor.executions {
            *lifecycle_calls.entry(lifecycle_id.clone()).or_default() += 1;
            assert_eq!(
                *call, lifecycle_calls[lifecycle_id],
                "each prepared lifecycle has one monotonic execution sequence"
            );
        }
        assert_eq!(
            lifecycle_calls
                .values()
                .filter(|count| **count == 105)
                .count(),
            972
        );
        assert_eq!(
            lifecycle_calls
                .values()
                .filter(|count| **count == 1)
                .count(),
            32_940
        );
        assert_eq!(lifecycle_calls.len(), 33_912);
        assert!(executor
            .executions
            .iter()
            .filter(|(_, phase, _)| *phase == CapacityPhase::FixedIncrementalReconcile)
            .all(|(_, _, call)| *call <= 105));
        assert!(executor
            .executions
            .iter()
            .filter(|(_, phase, _)| *phase == CapacityPhase::ColdInitializationReconcile)
            .all(|(_, _, call)| *call == 1));
    }

    #[test]
    fn configuration_construction_failure_retains_complete_digest_bound_evidence() {
        fn fail_configuration(_: &ResolvedCapacityProviderCondition) -> Result<Config> {
            Err(AtlasError::InvalidConfig(
                "injected capacity configuration construction failure".into(),
            ))
        }

        let (root, scenario, _) = scenario_and_manifest();
        let evidence = tempfile::Builder::new()
            .prefix("t30b-config-failure-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let mut executor = ProductionCapacityExecutor {
            build_config: fail_configuration,
        };
        let failure = run_capacity_campaign_with_executor(
            &root,
            &root.join("tests/fixtures/context_yield/benchmark-scenario.json"),
            &scenario,
            &root.join("tests/fixtures/capacity"),
            evidence.path(),
            None,
            None,
            &mut executor,
        )
        .unwrap_err();
        assert!(failure
            .to_string()
            .contains("after retaining complete raw evidence"));
        let raw_path = evidence.path().join(PRODUCTION_CAPACITY_EVIDENCE.raw_file);
        let summary_path = evidence
            .path()
            .join(PRODUCTION_CAPACITY_EVIDENCE.summary_file);
        let records = std::io::BufReader::new(File::open(&raw_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 135_000);
        let failed_records = records
            .iter()
            .filter(|observation| observation["preparation_failure"].is_string())
            .inspect(|observation| {
                assert_eq!(observation["outcome"], "failed");
                assert_eq!(observation["correctness"], false);
                assert!(observation["expected_configuration_hash"].is_null());
                assert!(observation["configuration_hash"].is_null());
                assert!(observation["preparation_failure"]
                    .as_str()
                    .unwrap()
                    .contains("injected capacity configuration construction failure"));
            })
            .count();
        assert!(failed_records > 0);
        assert_eq!(
            failed_records
                + records
                    .iter()
                    .filter(|observation| observation["outcome"] == "unsupported")
                    .count(),
            135_000
        );
        let summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        assert_eq!(summary["attempted_records"], 135_000);
        assert_eq!(summary["campaign_accepted"], false);
        assert!(
            summary["validation_failure_count"].as_u64().unwrap() > 0,
            "required configuration failures must reject the campaign"
        );
        assert_eq!(
            summary["validation_failures"].as_array().unwrap().len(),
            usize::try_from(summary["validation_failure_count"].as_u64().unwrap()).unwrap()
        );
        assert_eq!(
            summary["outcome_counts"]["failed"].as_u64().unwrap(),
            u64::try_from(failed_records).unwrap()
        );
        assert_eq!(summary["raw_sha256"], file_sha256(&raw_path).unwrap());
    }

    #[test]
    fn preparation_failure_retains_every_scheduled_observation_and_summary() {
        let (root, scenario, _) = scenario_and_manifest();
        let evidence = tempfile::Builder::new()
            .prefix("t30b-prepare-failure-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let mut executor = TestExecutor {
            fail_first_preparation: true,
            ..TestExecutor::default()
        };
        let failure = run_capacity_campaign_with_executor(
            &root,
            &root.join("tests/fixtures/context_yield/benchmark-scenario.json"),
            &scenario,
            &root.join("tests/fixtures/capacity"),
            evidence.path(),
            None,
            None,
            &mut executor,
        )
        .unwrap_err();
        assert!(failure
            .to_string()
            .contains("after retaining complete raw evidence"));
        let raw_path = evidence.path().join(TEST_CAPACITY_EVIDENCE.raw_file);
        let summary_path = evidence.path().join(TEST_CAPACITY_EVIDENCE.summary_file);
        assert_eq!(
            std::io::BufReader::new(File::open(&raw_path).unwrap())
                .lines()
                .count(),
            135_000
        );
        let summary: Value = serde_json::from_slice(&std::fs::read(summary_path).unwrap()).unwrap();
        assert_eq!(summary["attempted_records"], 135_000);
        assert_eq!(summary["campaign_accepted"], false);
        assert_eq!(summary["validation_failure_count"], 1);
        assert_eq!(summary["raw_sha256"], file_sha256(&raw_path).unwrap());
    }

    #[test]
    fn observed_values_fail_closed_without_changing_frozen_expectations() {
        let valid = test_observation(CapacityPhase::ReadyServingQueryCompiler);
        assert!(capacity_observation_failure(&valid, &mut BTreeMap::new()).is_none());

        let mut wrong_correctness = valid.clone();
        wrong_correctness.correctness = Some(false);
        assert!(capacity_observation_failure(&wrong_correctness, &mut BTreeMap::new()).is_some());

        let mut wrong_target = valid.clone();
        wrong_target.observed_target_identity = Some(serde_json::json!({"wrong": true}));
        assert!(capacity_observation_failure(&wrong_target, &mut BTreeMap::new()).is_some());

        let mut wrong_evidence = valid.clone();
        wrong_evidence.observed_evidence_counts.as_mut().unwrap()["symbols"] = Value::from(0);
        assert!(capacity_observation_failure(&wrong_evidence, &mut BTreeMap::new()).is_some());

        let mut wrong_work = valid.clone();
        wrong_work.deterministic_work_units = Some(6);
        assert!(capacity_observation_failure(&wrong_work, &mut BTreeMap::new()).is_some());

        let mut wrong_truth = valid.clone();
        wrong_truth.ready_versus_truth = Some("0".repeat(64));
        assert!(capacity_observation_failure(&wrong_truth, &mut BTreeMap::new()).is_some());

        let mut identities = BTreeMap::new();
        assert!(capacity_observation_failure(&valid, &mut identities).is_none());
        let mut wrong_identity = valid.clone();
        wrong_identity.sample_index = 2;
        wrong_identity.deterministic_identity = Some("different".to_string());
        assert!(capacity_observation_failure(&wrong_identity, &mut identities).is_some());

        let fixed = test_observation(CapacityPhase::FixedIncrementalReconcile);
        assert!(capacity_observation_failure(&fixed, &mut BTreeMap::new()).is_none());
        for mut wrong in [
            {
                let mut value = fixed.clone();
                value.parsed_files = Some(0);
                value
            },
            {
                let mut value = fixed.clone();
                value.changed_files = Some(0);
                value
            },
            {
                let mut value = fixed.clone();
                value.changed_bytes = Some(0);
                value
            },
            {
                let mut value = fixed.clone();
                value.provider_executions_reused = Some(0);
                value
            },
        ] {
            assert!(capacity_observation_failure(&wrong, &mut BTreeMap::new()).is_some());
            wrong.correctness = Some(false);
        }
    }

    #[test]
    fn success_measurement_applicability_is_complete_and_closed() {
        let valid = test_observation(CapacityPhase::ReadyServingQueryCompiler);
        for field in 0..6 {
            let mut invalid = valid.clone();
            match field {
                0 => invalid.provider_probes = None,
                1 => invalid.provider_executions_run = None,
                2 => invalid.provider_outputs = None,
                3 => invalid.provider_output_bytes = None,
                4 => invalid.provider_executions_reused = None,
                5 => invalid.required_provider_failure = None,
                _ => unreachable!(),
            }
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }
        for field in 0..20 {
            let mut invalid = valid.clone();
            match field {
                0 => invalid.observed_target_identity = None,
                1 => invalid.observed_evidence_counts = None,
                2 => invalid.accepted_outcome = None,
                3 => invalid.eligible_files = None,
                4 => invalid.indexed_files = None,
                5 => invalid.source_bytes = None,
                6 => invalid.catalogue_rows_before = None,
                7 => invalid.catalogue_rows_after = None,
                8 => invalid.catalogue_bytes_before = None,
                9 => invalid.catalogue_bytes_after = None,
                10 => invalid.durable_catalogue_rows_before = None,
                11 => invalid.durable_catalogue_rows_after = None,
                12 => invalid.durable_catalogue_bytes_before = None,
                13 => invalid.durable_catalogue_bytes_after = None,
                14 => invalid.derived_catalogue_rows_before = None,
                15 => invalid.derived_catalogue_rows_after = None,
                16 => invalid.derived_catalogue_bytes_before = None,
                17 => invalid.derived_catalogue_bytes_after = None,
                18 => invalid.ready_versus_truth = None,
                19 => invalid.expected_configuration_hash = None,
                _ => unreachable!(),
            }
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }
        for field in 0..7 {
            let mut invalid = valid.clone();
            match field {
                0 => invalid.compiler_selected_records = None,
                1 => invalid.compiler_selected_source_bytes = None,
                2 => invalid.compiler_selected_estimated_tokens = None,
                3 => invalid.compiler_omitted_records = None,
                4 => invalid.compiler_truncated = None,
                5 => invalid.compiler_work_units_consumed = None,
                6 => invalid.compiler_fallback = None,
                _ => unreachable!(),
            }
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }
        for invalid in [
            {
                let mut value = valid.clone();
                value.compiler_selected_estimated_tokens = value.profile_inputs["estimated_tokens"]
                    .as_u64()
                    .map(|tokens| tokens + 1);
                value
            },
            {
                let mut value = valid.clone();
                value.compiler_truncated = Some(false);
                value
            },
        ] {
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }
        let mut wrong_configuration = valid.clone();
        wrong_configuration.configuration_hash = Some("different-non-empty-hash".to_string());
        assert!(capacity_observation_failure(&wrong_configuration, &mut BTreeMap::new()).is_some());
        let mut aliased_rows = valid.clone();
        aliased_rows.catalogue_rows_before = Some(1);
        assert!(capacity_observation_failure(&aliased_rows, &mut BTreeMap::new()).is_some());
        let mut aliased_bytes = valid;
        aliased_bytes.catalogue_bytes_after = Some(1);
        assert!(capacity_observation_failure(&aliased_bytes, &mut BTreeMap::new()).is_some());

        let fixed = test_observation(CapacityPhase::FixedIncrementalReconcile);
        for field in 0..3 {
            let mut invalid = fixed.clone();
            match field {
                0 => invalid.parsed_files = None,
                1 => invalid.changed_files = None,
                2 => invalid.changed_bytes = None,
                _ => unreachable!(),
            }
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }

        let stateless = test_observation(CapacityPhase::CapabilityDiscovery);
        for field in 0..8 {
            let mut invalid = stateless.clone();
            match field {
                0 => invalid.provider_probes = Some(0),
                1 => invalid.catalogue_rows_before = Some(0),
                2 => invalid.parsed_files = Some(0),
                3 => invalid.compiler_selected_records = Some(0),
                4 => invalid.ready_versus_truth = Some("invalid".to_string()),
                5 => invalid.configuration_hash = Some("invalid".to_string()),
                6 => invalid.provider_install_duration_ms = Some(0),
                7 => invalid.observed_target_identity = Some(serde_json::json!({})),
                _ => unreachable!(),
            }
            assert!(capacity_observation_failure(&invalid, &mut BTreeMap::new()).is_some());
        }
    }

    #[test]
    fn perturbed_observations_are_retained_before_campaign_failure() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let valid = test_observation(CapacityPhase::ReadyServingQueryCompiler);
        let fixed = test_observation(CapacityPhase::FixedIncrementalReconcile);
        let mut mutations = Vec::new();
        let mut wrong_target = valid.clone();
        wrong_target.observed_target_identity = Some(serde_json::json!({"wrong": true}));
        mutations.push(wrong_target);
        let mut wrong_evidence = valid.clone();
        wrong_evidence.observed_evidence_counts.as_mut().unwrap()["symbols"] = Value::from(0);
        mutations.push(wrong_evidence);
        let mut wrong_work = valid.clone();
        wrong_work.deterministic_work_units = Some(6);
        mutations.push(wrong_work);
        let mut wrong_truth = valid;
        wrong_truth.ready_versus_truth = Some("0".repeat(64));
        mutations.push(wrong_truth);
        let mut wrong_changed = fixed.clone();
        wrong_changed.changed_bytes = Some(0);
        mutations.push(wrong_changed);
        let mut wrong_reuse = fixed;
        wrong_reuse.provider_executions_reused = Some(0);
        mutations.push(wrong_reuse);
        for (index, observation) in mutations.iter_mut().enumerate() {
            observation.sample_index = u64::try_from(index + 1).unwrap();
        }

        let evidence = tempfile::Builder::new()
            .prefix("t30b-negative-retention-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let (guard, mut writer) = guarded_raw_writer(evidence.path(), 6, "test-negative.ndjson");
        let mut expected = BTreeSet::new();
        let mut outcomes = BTreeMap::new();
        let mut scored = BTreeMap::new();
        let mut condition_outcomes = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut failures = Vec::new();
        for observation in mutations {
            retain_capacity_observation(
                &mut writer,
                &mut expected,
                &mut outcomes,
                &mut scored,
                &mut condition_outcomes,
                &mut identities,
                &mut failures,
                observation,
            )
            .unwrap();
        }
        let raw = writer.finish(&expected, &guard).unwrap();
        assert_eq!(
            std::io::BufReader::new(File::open(raw.path).unwrap())
                .lines()
                .count(),
            6
        );
        assert_eq!(failures.len(), 6);
    }

    #[test]
    fn fixed_incremental_change_bytes_are_measured_and_fixture_restores() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let temporary = tempfile::tempdir().unwrap();
        let fixture = temporary.path().join("fixture");
        generate_capacity_fixture(&root, &scenario, &manifest, &fixture).unwrap();
        let before = file_sha256(
            &fixture.join(
                manifest["target_identity"]["relative_path"]
                    .as_str()
                    .unwrap(),
            ),
        )
        .unwrap();
        let measured = apply_capacity_changes(&fixture, &manifest).unwrap();
        assert_eq!(measured.files, manifest["counts"]["changed_files"]);
        assert_eq!(measured.bytes, manifest["counts"]["changed_bytes"]);
        generate_capacity_fixture(&root, &scenario, &manifest, &fixture).unwrap();
        assert_eq!(
            file_sha256(
                &fixture.join(
                    manifest["target_identity"]["relative_path"]
                        .as_str()
                        .unwrap()
                )
            )
            .unwrap(),
            before
        );
    }

    #[test]
    fn production_fixed_incremental_state_restores_before_reapplication() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let provider = small_builtin_provider();
        let lifecycle_id = "production-reset-regression".to_string();
        let mut state = prepare_production_capacity_state(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase: CapacityPhase::FixedIncrementalReconcile,
            lifecycle_id: &lifecycle_id,
        })
        .unwrap()
        .prepared;
        for sample_index in 1..=2 {
            let empty = empty_capacity_observation(
                "a",
                &manifest,
                None,
                &scenario.profiles[0],
                CapacityAttemptSpec {
                    evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                    provider_condition: provider.name(),
                    provider_members: provider.members(),
                    expected_configuration_hash: provider.configuration_hash().unwrap(),
                    phase: CapacityPhase::FixedIncrementalReconcile.descriptor().name,
                    repetition: 1,
                    sample_class: "warm",
                    sample_index,
                    lifecycle_id: lifecycle_id.clone(),
                },
            )
            .unwrap();
            let observed = execute_capacity_observation(&mut state, empty);
            assert_eq!(
                observed.changed_files,
                Some(observed.expected_changed_files)
            );
            assert_eq!(
                observed.changed_bytes,
                Some(observed.expected_changed_bytes)
            );
            assert_eq!(
                observed.parsed_files,
                Some(if sample_index == 1 {
                    observed.expected_changed_files - 1
                } else {
                    0
                })
            );
            assert_eq!(state.action_counts.production_actions, sample_index);
            assert_eq!(state.action_counts.post_observation_resets, sample_index);
            for file in manifest["files"].as_array().unwrap() {
                let relative = file["relative_path"].as_str().unwrap();
                assert_eq!(
                    file_sha256(&state.fixture_root.join(relative)).unwrap(),
                    file["sha256"].as_str().unwrap(),
                    "fixture reset failed for {relative}"
                );
            }
            assert!(!state
                .fixture_root
                .join("src/controller/reconnect_policy.ts")
                .exists());
        }
    }

    #[test]
    fn production_serving_phase_state_is_reset_or_reused_as_declared() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let provider = small_builtin_provider();
        for phase in [
            CapacityPhase::TruthFallbackQueryCompiler,
            CapacityPhase::ReadyServingQueryCompiler,
        ] {
            let lifecycle_id = format!("serving-reset-{}", phase.descriptor().name);

            let mut state = prepare_production_capacity_state(CapacityPreparationSpec {
                repository_root: &root,
                scenario: &scenario,
                manifest: &manifest,
                provider_condition: &provider,
                phase,
                lifecycle_id: &lifecycle_id,
            })
            .unwrap()
            .prepared;
            for sample_index in 1..=2 {
                let empty = empty_capacity_observation(
                    "a",
                    &manifest,
                    None,
                    &scenario.profiles[0],
                    CapacityAttemptSpec {
                        evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                        provider_condition: provider.name(),
                        provider_members: provider.members(),
                        expected_configuration_hash: provider.configuration_hash().unwrap(),
                        phase: phase.descriptor().name,
                        repetition: 1,
                        sample_class: "warmup",
                        sample_index,
                        lifecycle_id: lifecycle_id.clone(),
                    },
                )
                .unwrap();
                let observed = execute_capacity_observation(&mut state, empty);
                assert_eq!(
                    observed.compiler_fallback,
                    Some(phase == CapacityPhase::TruthFallbackQueryCompiler)
                );
                let serving_rows: i64 = state
                    .connection
                    .as_ref()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM serving_generation", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(
                    serving_rows,
                    if phase == CapacityPhase::ReadyServingQueryCompiler {
                        1
                    } else {
                        0
                    }
                );
            }
        }
    }
    #[test]
    fn scaled_fixed_incremental_uses_immutable_change_measurement() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scenario =
            load_scenario(&root.join("tests/fixtures/context_yield/benchmark-scenario.json"))
                .unwrap();
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(root.join("tests/fixtures/capacity/repo-medium-v1.json")).unwrap(),
        )
        .unwrap();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let accepted_outcomes = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &manifest_directory,
            &manifests,
        )
        .unwrap();
        let provider =
            resolve_capacity_provider_condition("repo-medium-v1", "builtin-only").unwrap();
        let phase = CapacityPhase::FixedIncrementalReconcile;
        let lifecycle_id = "scaled-fixed-incremental-regression";
        let mut state = prepare_production_capacity_state(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase,
            lifecycle_id,
        })
        .unwrap()
        .prepared;
        let accepted_outcome =
            capacity_accepted_outcome(&manifest, &accepted_outcomes, provider.name(), phase)
                .unwrap();
        let observation = empty_capacity_observation(
            "scaled-fixed-incremental-regression",
            &manifest,
            Some(accepted_outcome),
            &scenario.profiles[0],
            CapacityAttemptSpec {
                evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: provider.configuration_hash().unwrap(),
                phase: phase.descriptor().name,
                repetition: 1,
                sample_class: "warmup",
                sample_index: 1,
                lifecycle_id: lifecycle_id.to_string(),
            },
        )
        .unwrap();
        let measured = execute_capacity_action(&mut state, &observation).unwrap();
        assert_eq!(
            measured.changed_files,
            manifest["counts"]["changed_files"].as_u64()
        );
        assert_eq!(
            measured.changed_bytes,
            manifest["counts"]["changed_bytes"].as_u64()
        );
        assert!(measured.correctness);
    }

    #[test]
    fn production_route_conditions_execute_real_light_and_progressive_deep_paths() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let accepted_outcomes = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &manifest_directory,
            &manifests,
        )
        .unwrap();
        for provider in [
            small_builtin_provider(),
            resolve_capacity_provider_condition("repo-small-v1", "typescript-only").unwrap(),
            resolve_capacity_provider_condition("repo-small-v1", "combined").unwrap(),
        ] {
            for phase in [
                CapacityPhase::LightRouteCompiler,
                CapacityPhase::ProgressiveDeepRouteCompiler,
            ] {
                let lifecycle_id = phase
                    .descriptor()
                    .lifecycle_id(
                        "repo-small-v1",
                        &scenario.profiles[0].name,
                        provider.name(),
                        1,
                        "warmup",
                        1,
                    )
                    .unwrap();
                let mut prepared = prepare_production_capacity_state(CapacityPreparationSpec {
                    repository_root: &root,
                    scenario: &scenario,
                    manifest: &manifest,
                    provider_condition: &provider,
                    phase,
                    lifecycle_id: &lifecycle_id,
                })
                .unwrap()
                .prepared;
                let accepted_outcome = capacity_accepted_outcome(
                    &manifest,
                    &accepted_outcomes,
                    provider.name(),
                    phase,
                )
                .unwrap();
                let empty = empty_capacity_observation(
                    "a",
                    &manifest,
                    Some(accepted_outcome),
                    &scenario.profiles[0],
                    CapacityAttemptSpec {
                        evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                        provider_condition: provider.name(),
                        provider_members: provider.members(),
                        expected_configuration_hash: provider.configuration_hash().unwrap(),
                        phase: phase.descriptor().name,
                        repetition: 1,
                        sample_class: "warmup",
                        sample_index: 1,
                        lifecycle_id,
                    },
                )
                .unwrap();
                let observed = execute_capacity_observation(&mut prepared, empty);
                assert_eq!(
                    observed.outcome,
                    "success",
                    "{} failed: {observed:#?}",
                    phase.descriptor().name
                );
                assert_eq!(
                    observed.correctness,
                    Some(true),
                    "{} incorrect: {observed:#?}",
                    phase.descriptor().name
                );
                let validation_failure =
                    capacity_observation_failure(&observed, &mut BTreeMap::new());
                assert!(
                    validation_failure.is_none(),
                    "invalid {} observation: {validation_failure:?}\n{observed:#?}",
                    phase.descriptor().name
                );
                if phase == CapacityPhase::LightRouteCompiler {
                    assert_eq!(observed.route_attempts, 1);
                    assert_eq!(observed.deterministic_work_units, Some(1));
                    assert!(observed.compiler_selected_records.is_none());
                    assert!(observed.ready_versus_truth.is_none());
                } else {
                    assert_eq!(observed.route_attempts, 3);
                    assert_eq!(
                        observed.deterministic_work_units,
                        observed
                            .compiler_work_units_consumed
                            .and_then(|work| work.checked_add(1))
                    );
                    assert!(observed.compiler_selected_records.is_some());
                    assert!(observed.compiler_selected_source_bytes.is_some());
                    assert!(observed.compiler_selected_estimated_tokens.is_some());
                    assert!(observed.compiler_truncated.is_some());
                    assert_eq!(
                        observed.ready_versus_truth,
                        observed
                            .accepted_outcome
                            .as_deref()
                            .and_then(|accepted| ready_truth_identity(accepted, true).unwrap())
                    );
                }
            }
        }
    }

    #[test]
    fn first_small_cold_attempt_matches_production_accepted_outcome() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let accepted_outcomes = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &manifest_directory,
            &manifests,
        )
        .unwrap();
        let provider = small_builtin_provider();
        let phase = CapacityPhase::ColdInitializationReconcile;
        let lifecycle_id = phase
            .descriptor()
            .lifecycle_id(
                "repo-small-v1",
                &scenario.profiles[0].name,
                provider.name(),
                1,
                "warmup",
                1,
            )
            .unwrap();
        let mut prepared = prepare_production_capacity_state(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase,
            lifecycle_id: &lifecycle_id,
        })
        .unwrap();
        assert!(!prepared
            .prepared
            .fixture_root
            .join("fixture_manifest.json")
            .exists());
        assert!(!prepared
            .prepared
            .fixture_root
            .join(".workspace-atlas-fixture-marker")
            .exists());
        let accepted_outcome =
            capacity_accepted_outcome(&manifest, &accepted_outcomes, provider.name(), phase)
                .unwrap();
        let empty = empty_capacity_observation(
            "a",
            &manifest,
            Some(accepted_outcome),
            &scenario.profiles[0],
            CapacityAttemptSpec {
                evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: prepared.configuration_hash.clone(),
                phase: phase.descriptor().name,
                repetition: 1,
                sample_class: "warmup",
                sample_index: 1,
                lifecycle_id,
            },
        )
        .unwrap();

        let observed = execute_capacity_observation(&mut prepared.prepared, empty);

        assert_eq!(
            observed.eligible_files,
            Some(observed.expected_eligible_files)
        );
        assert_eq!(
            observed.indexed_files,
            Some(observed.expected_indexed_files)
        );
        assert_eq!(observed.source_bytes, Some(observed.expected_source_bytes));
        assert_eq!(
            observed.accepted_outcome,
            Some(observed.expected_accepted_outcome_hash.clone())
        );
        assert_eq!(observed.correctness, Some(true));
        assert!(
            capacity_observation_failure(&observed, &mut BTreeMap::new()).is_none(),
            "production preflight observation must satisfy the accepted outcome contract: {observed:#?}"
        );
    }

    #[test]
    fn fixed_incremental_preflight_one_shot_skips_only_terminal_reset() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let accepted_outcomes = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &manifest_directory,
            &manifests,
        )
        .unwrap();
        let provider = small_builtin_provider();
        let phase = CapacityPhase::FixedIncrementalReconcile;
        let lifecycle_id = phase
            .descriptor()
            .lifecycle_id(
                "repo-small-v1",
                &scenario.profiles[0].name,
                provider.name(),
                1,
                "warmup",
                1,
            )
            .unwrap();
        let mut prepared = prepare_production_capacity_state(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase,
            lifecycle_id: &lifecycle_id,
        })
        .unwrap();
        let accepted_outcome =
            capacity_accepted_outcome(&manifest, &accepted_outcomes, provider.name(), phase)
                .unwrap();
        let empty = empty_capacity_observation(
            "accepted-outcome-preflight-v2",
            &manifest,
            Some(accepted_outcome),
            &scenario.profiles[0],
            CapacityAttemptSpec {
                evidence_kind: PRODUCTION_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: prepared.configuration_hash.clone(),
                phase: phase.descriptor().name,
                repetition: 1,
                sample_class: "warmup",
                sample_index: 1,
                lifecycle_id,
            },
        )
        .unwrap();

        let observed = execute_capacity_observation_with_policy(
            &mut prepared.prepared,
            empty,
            CapacityObservationPolicy::PreflightOneShot,
        );

        assert_eq!(prepared.prepared.action_counts.production_actions, 1);
        assert_eq!(prepared.prepared.action_counts.post_observation_resets, 0);
        assert_eq!(
            observed.accepted_outcome,
            Some(observed.expected_accepted_outcome_hash.clone())
        );
        assert_eq!(observed.correctness, Some(true));
        assert!(
            capacity_observation_failure(&observed, &mut BTreeMap::new()).is_none(),
            "one-shot production observation must retain the accepted outcome: {observed:#?}"
        );
        assert!(
            !prepared
                .prepared
                .fixture_root
                .join("src/controller/reconnect_legacy.ts")
                .exists(),
            "one-shot state must remain consumed until its immediate drop"
        );
    }

    #[test]
    fn preflight_provenance_hashes_the_selected_scenario_path() {
        let (root, _, _) = scenario_and_manifest();
        let canonical_path = root.join("tests/fixtures/context_yield/benchmark-scenario.json");
        let scenario_directory = tempfile::Builder::new()
            .prefix("t30ab-scenario-provenance-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let scenario_path = scenario_directory
            .path()
            .join("benchmark-scenario-copy.json");
        let mut scenario_bytes = std::fs::read(&canonical_path).unwrap();
        scenario_bytes.push(b'\n');
        std::fs::write(&scenario_path, &scenario_bytes).unwrap();
        let scenario = load_scenario(&scenario_path).unwrap();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let mut contract = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &manifest_directory,
            &manifests,
        )
        .unwrap();
        contract.outcomes.clear();

        let preflight = run_capacity_accepted_outcome_preflight(
            &root,
            &scenario_path,
            &scenario,
            &manifests,
            &contract,
        )
        .unwrap();
        assert_eq!(
            preflight["scenario_sha256"],
            file_sha256(&scenario_path).unwrap()
        );
        assert_ne!(
            preflight["scenario_sha256"],
            file_sha256(&canonical_path).unwrap()
        );
    }

    #[test]
    fn production_preflight_rejects_stale_outcome_before_campaign_execution() {
        let (root, scenario, _) = scenario_and_manifest();
        let manifest_directory = root.join("tests/fixtures/capacity");
        let manifests = validate_capacity_manifests(&root, &scenario, &manifest_directory).unwrap();
        let mut contract = load_capacity_accepted_outcome_contract(
            &root,
            &scenario,
            &root.join("tests/fixtures/capacity"),
            &manifests,
        )
        .unwrap();
        contract.outcomes[0].expected_accepted_outcome_hash = "0".repeat(64);

        let error = run_capacity_accepted_outcome_preflight(
            &root,
            &root.join("tests/fixtures/context_yield/benchmark-scenario.json"),
            &scenario,
            &manifests,
            &contract,
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("production accepted-outcome preflight mismatch"),
            "{error}"
        );
    }

    #[test]
    fn cold_initialization_executes_105_distinct_warm_lifecycles() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let provider = small_builtin_provider();
        let phase = CapacityPhase::ColdInitializationReconcile;
        assert_eq!(
            phase.descriptor().lifecycle_identity,
            CapacityLifecycleIdentityPolicy::PerAttempt
        );
        let mut lifecycle_ids = BTreeSet::new();
        let mut action_counts = CapacityActionCounts::default();
        for (sample_class, sample_count) in [
            ("warmup", scenario.procedure.warmups),
            ("warm", scenario.procedure.warm_samples),
        ] {
            for sample_index in 1..=sample_count {
                let lifecycle_id = phase
                    .descriptor()
                    .lifecycle_id(
                        "repo-small-v1",
                        &scenario.profiles[0].name,
                        provider.name(),
                        1,
                        sample_class,
                        sample_index,
                    )
                    .unwrap();
                assert!(
                    lifecycle_ids.insert(lifecycle_id.clone()),
                    "cold-initialization lifecycle identity was reused"
                );
                let mut prepared = prepare_production_capacity_state(CapacityPreparationSpec {
                    repository_root: &root,
                    scenario: &scenario,
                    manifest: &manifest,
                    provider_condition: &provider,
                    phase,
                    lifecycle_id: &lifecycle_id,
                })
                .unwrap();
                assert_eq!(
                    prepared.configuration_hash,
                    provider.configuration_hash().unwrap()
                );
                let empty = empty_capacity_observation(
                    "a",
                    &manifest,
                    None,
                    &scenario.profiles[0],
                    CapacityAttemptSpec {
                        evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                        provider_condition: provider.name(),
                        provider_members: provider.members(),
                        expected_configuration_hash: prepared.configuration_hash.clone(),
                        phase: phase.descriptor().name,
                        repetition: 1,
                        sample_class,
                        sample_index,
                        lifecycle_id: lifecycle_id.clone(),
                    },
                )
                .unwrap();
                let observed = execute_capacity_observation(&mut prepared.prepared, empty);
                assert_eq!(observed.outcome, "success");
                assert_eq!(observed.sample_class, sample_class);
                assert_eq!(observed.lifecycle_id, lifecycle_id);
                assert!(
                    prepared.prepared.connection.is_none() && prepared.prepared.workspace.is_none(),
                    "every cold-initialization observation must release its per-attempt catalogue"
                );
                assert_eq!(prepared.prepared.action_counts.catalogue_initializations, 1);
                assert_eq!(prepared.prepared.action_counts.workspace_registrations, 1);
                assert_eq!(prepared.prepared.action_counts.reconciliations, 1);
                action_counts.catalogue_initializations +=
                    prepared.prepared.action_counts.catalogue_initializations;
                action_counts.workspace_registrations +=
                    prepared.prepared.action_counts.workspace_registrations;
                action_counts.reconciliations += prepared.prepared.action_counts.reconciliations;
            }
        }
        assert_eq!(lifecycle_ids.len(), 105);
        assert_eq!(action_counts.catalogue_initializations, 105);
        assert_eq!(action_counts.workspace_registrations, 105);
        assert_eq!(action_counts.reconciliations, 105);
    }

    #[test]
    fn catalogue_categories_report_actual_rows_and_pages() {
        let temporary = tempfile::tempdir().unwrap();
        let catalogue_path = temporary.path().join("catalogue.sqlite");
        let config = capacity_config(&small_builtin_provider()).unwrap();
        let connection = catalogue::init_catalogue(&catalogue_path, &config).unwrap();
        let measured = catalogue_category_measurement(&connection).unwrap();
        assert!(measured.durable_rows > 0);
        assert!(measured.durable_bytes > 0);
        assert_eq!(measured.derived_rows, 0);
    }

    #[test]
    fn live_target_symbol_aliasing_fails_closed_with_retained_evidence() {
        let (root, scenario, manifest) = scenario_and_manifest();
        let provider = small_builtin_provider();
        let lifecycle_id = "live-symbol-alias-regression".to_string();
        let mut state = prepare_production_capacity_state(CapacityPreparationSpec {
            repository_root: &root,
            scenario: &scenario,
            manifest: &manifest,
            provider_condition: &provider,
            phase: CapacityPhase::NoChangeReconcile,
            lifecycle_id: &lifecycle_id,
        })
        .unwrap()
        .prepared;
        let connection = state.connection.as_ref().unwrap();
        let rows = connection
            .execute(
                "UPDATE symbol_fact SET display_name = 'aliasedTarget'
                 WHERE display_name = ?1 AND revision_id IN (
                     SELECT revision_id FROM generation_file WHERE generation_id = (
                         SELECT active_generation_id FROM workspace WHERE workspace_id = ?2
                     ) AND canonical_path = ?3
                 )",
                rusqlite::params![
                    manifest["target_identity"]["symbol"].as_str().unwrap(),
                    state.workspace.as_ref().unwrap().workspace_id,
                    manifest["target_identity"]["relative_path"]
                        .as_str()
                        .unwrap(),
                ],
            )
            .unwrap();
        assert!(rows > 0);
        let evidence = tempfile::Builder::new()
            .prefix("t30b-live-symbol-alias-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let (guard, mut writer) =
            guarded_raw_writer(evidence.path(), 1, "test-live-symbol-alias.ndjson");
        let empty = empty_capacity_observation(
            "a",
            &manifest,
            None,
            &scenario.profiles[0],
            CapacityAttemptSpec {
                evidence_kind: TEST_CAPACITY_EVIDENCE.raw_kind,
                provider_condition: provider.name(),
                provider_members: provider.members(),
                expected_configuration_hash: provider.configuration_hash().unwrap(),
                phase: CapacityPhase::NoChangeReconcile.descriptor().name,
                repetition: 1,
                sample_class: "warm",
                sample_index: 1,
                lifecycle_id,
            },
        )
        .unwrap();
        let observation = execute_capacity_observation(&mut state, empty);
        assert_ne!(observation.outcome, "success");
        let mut expected = BTreeSet::new();
        let mut outcomes = BTreeMap::new();
        let mut scored = BTreeMap::new();
        let mut condition_outcomes = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut failures = Vec::new();
        retain_capacity_observation(
            &mut writer,
            &mut expected,
            &mut outcomes,
            &mut scored,
            &mut condition_outcomes,
            &mut identities,
            &mut failures,
            observation,
        )
        .unwrap();
        writer.finish(&expected, &guard).unwrap();
        assert_eq!(failures.len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_handle_blocks_final_open_rename_and_replacement() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let parent = tempfile::Builder::new()
            .prefix("t30b-parent-handle-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let evidence = parent.path().join("evidence");
        let replacement = parent.path().join("replacement");
        let moved = parent.path().join("moved");
        std::fs::create_dir(&evidence).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        let guard = capacity_directory_guard(&evidence).unwrap();
        let original_identity = guard.identity.clone();
        let rename = std::fs::rename(&evidence, &moved).unwrap_err();
        assert!(
            matches!(
                rename.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
            ) || rename.raw_os_error() == Some(32)
                || rename.raw_os_error() == Some(5),
            "parent rename must be blocked while the non-share-delete handle is live: {rename}"
        );
        let remove = std::fs::remove_dir(&evidence).unwrap_err();
        assert!(
            matches!(
                remove.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
            ) || remove.raw_os_error() == Some(32)
                || remove.raw_os_error() == Some(5),
            "parent deletion/junction replacement must be blocked while the guard is live: {remove}"
        );
        let (path, file) =
            create_capacity_output(&evidence, &guard, "final-open-boundary.json").unwrap();
        finish_capacity_output(&evidence, &guard, file, b"bound").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"bound");
        drop(guard);
        std::fs::rename(&evidence, &moved).unwrap();
        std::fs::rename(&replacement, &evidence).unwrap();
        assert_ne!(
            capacity_directory_identity(&evidence).unwrap(),
            original_identity,
            "a replacement after releasing the guard must have a distinct volume/file identity"
        );
    }

    #[cfg(windows)]
    #[test]
    fn one_guard_binds_raw_and_summary_and_blocks_inter_output_replacement() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let parent = tempfile::Builder::new()
            .prefix("t30b-paired-output-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let evidence = parent.path().join("evidence");
        std::fs::create_dir(&evidence).unwrap();
        let guard = capacity_directory_guard(&evidence).unwrap();
        let observation = test_observation(CapacityPhase::CapabilityDiscovery);
        let mut writer =
            CapacityRawWriter::create(&evidence, &guard, 1, "paired-raw.ndjson").unwrap();
        writer.write(&observation).unwrap();
        let expected = BTreeSet::from([observation.key()]);
        let mut raw = writer.finish(&expected, &guard).unwrap();
        assert_eq!(sha256_file_handle(&mut raw.file).unwrap(), raw.sha256);

        let moved_parent = parent.path().join("moved-evidence");
        let parent_rename = std::fs::rename(&evidence, &moved_parent).unwrap_err();
        assert!(
            parent_rename.raw_os_error() == Some(32)
                || parent_rename.raw_os_error() == Some(5)
                || matches!(
                    parent_rename.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
                )
        );
        let raw_delete = std::fs::remove_file(&raw.path).unwrap_err();
        assert!(
            raw_delete.raw_os_error() == Some(32)
                || raw_delete.raw_os_error() == Some(5)
                || matches!(
                    raw_delete.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
                )
        );
        let raw_rename =
            std::fs::rename(&raw.path, evidence.join("replaced-raw.ndjson")).unwrap_err();
        assert!(
            raw_rename.raw_os_error() == Some(32)
                || raw_rename.raw_os_error() == Some(5)
                || matches!(
                    raw_rename.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Other
                )
        );

        let summary = serde_json::json!({"raw_sha256": raw.sha256});
        let (summary_path, summary_file) =
            create_capacity_output(&evidence, &guard, "paired-summary.json").unwrap();
        let mut summary_file = finish_capacity_output(
            &evidence,
            &guard,
            summary_file,
            &serde_json::to_vec(&summary).unwrap(),
        )
        .unwrap();
        summary_file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        summary_file.read_to_end(&mut bytes).unwrap();
        let retained: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(retained["raw_sha256"], raw.sha256);
        assert_eq!(sha256_file_handle(&mut raw.file).unwrap(), raw.sha256);
        validate_capacity_directory_guard(&evidence, &guard).unwrap();
        assert!(summary_path.exists());
    }

    #[test]
    fn raw_writer_rejects_duplicate_missing_and_over_bound_records() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let observation = test_observation(CapacityPhase::CapabilityDiscovery);

        let duplicate_dir = tempfile::Builder::new()
            .prefix("t30b-duplicate-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let (_duplicate_guard, mut duplicate) =
            guarded_raw_writer(duplicate_dir.path(), 2, "test-duplicate.ndjson");
        duplicate.write(&observation).unwrap();
        assert!(duplicate
            .write(&observation)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        let missing_dir = tempfile::Builder::new()
            .prefix("t30b-missing-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let (missing_guard, mut missing) =
            guarded_raw_writer(missing_dir.path(), 2, "test-missing.ndjson");
        missing.write(&observation).unwrap();
        let mut expected = BTreeSet::from([observation.key()]);
        let mut second = observation.clone();
        second.sample_index = 2;
        expected.insert(second.key());
        assert!(missing
            .finish(&expected, &missing_guard)
            .unwrap_err()
            .to_string()
            .contains("missing"));

        let over_dir = tempfile::Builder::new()
            .prefix("t30b-over-")
            .tempdir_in(root.join("target"))
            .unwrap();
        let (_over_guard, mut over) = guarded_raw_writer(over_dir.path(), 1, "test-over.ndjson");
        over.write(&observation).unwrap();
        assert!(over
            .write(&second)
            .unwrap_err()
            .to_string()
            .contains("ceiling"));
    }

    #[test]
    fn outcome_policy_is_closed_for_required_optional_and_not_applicable_conditions() {
        fn refresh_lifecycle(observation: &mut CapacityRawObservation) {
            let phase = CapacityPhase::parse(&observation.phase).unwrap();
            observation.lifecycle_id = phase
                .descriptor()
                .lifecycle_id(
                    &observation.dataset_name,
                    &observation.profile,
                    &observation.provider_condition,
                    observation.repetition,
                    &observation.sample_class,
                    observation.sample_index,
                )
                .unwrap();
        }
        let valid = test_observation(CapacityPhase::CapabilityDiscovery);
        for outcome in [
            "timeout",
            "absent",
            "unsupported",
            "degraded",
            "cancelled",
            "failed",
        ] {
            let mut required = valid.clone();
            required.outcome = outcome.to_string();
            required.correctness = None;
            assert!(
                capacity_observation_failure(&required, &mut BTreeMap::new()).is_some(),
                "required {outcome} must fail"
            );

            let optional_provider =
                CapacityProviderCondition::RustOnly.resolve(CapacityDataset::Medium);
            let mut optional = required.clone();
            optional.dataset_name = CapacityDataset::Medium.name().to_string();
            optional.provider_condition = optional_provider.name().to_string();
            optional.provider_members = optional_provider.members();
            refresh_lifecycle(&mut optional);
            assert!(
                capacity_observation_failure(&optional, &mut BTreeMap::new()).is_none(),
                "optional Rust {outcome} remains explicit but permitted"
            );
        }
        let not_applicable_provider =
            CapacityProviderCondition::RustOnly.resolve(CapacityDataset::Small);
        let mut not_applicable = valid.clone();
        not_applicable.provider_condition = not_applicable_provider.name().to_string();
        not_applicable.provider_members = not_applicable_provider.members();
        refresh_lifecycle(&mut not_applicable);
        not_applicable.outcome = "unsupported".to_string();
        not_applicable.correctness = None;
        assert!(capacity_observation_failure(&not_applicable, &mut BTreeMap::new()).is_none());
        not_applicable.outcome = "absent".to_string();
        assert!(capacity_observation_failure(&not_applicable, &mut BTreeMap::new()).is_some());

        let combined_provider = CapacityProviderCondition::Combined.resolve(CapacityDataset::Small);
        let mut combined_degraded = valid;
        combined_degraded.provider_condition = combined_provider.name().to_string();
        combined_degraded.provider_members = combined_provider.members();
        refresh_lifecycle(&mut combined_degraded);
        combined_degraded.outcome = "degraded".to_string();
        combined_degraded.correctness = None;
        combined_degraded.required_provider_failure = Some(false);
        assert!(capacity_observation_failure(&combined_degraded, &mut BTreeMap::new()).is_none());
        combined_degraded.required_provider_failure = Some(true);
        assert!(capacity_observation_failure(&combined_degraded, &mut BTreeMap::new()).is_some());
    }

    #[test]
    fn capacity_log_redacts_private_paths_and_diagnostics_before_encoding() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let evidence = root.join("target/t31-private-evidence");
        let mut value = serde_json::json!({
            "relative_path": "src/controller/retry_policy.ts",
            "absolute_path": root.join("private/source.rs").to_string_lossy(),
            "preparation_failure": format!("failed under {}", evidence.display()),
            "validation_failures": [
                format!("failure under {}", root.display()),
                "provider emitted a private diagnostic"
            ],
            "error": {"kind": "permission", "detail": "credential-shaped private detail"}
        });
        redact_capacity_log_value(&mut value, &root, &evidence).unwrap();
        let redacted = serde_json::to_string(&value).unwrap();
        assert!(redacted.contains("src/controller/retry_policy.ts"));
        assert!(!redacted.contains(root.to_string_lossy().as_ref()));
        assert!(!redacted.contains("credential-shaped private detail"));
        assert!(redacted.contains("<redacted-diagnostic>"));
        assert!(!redacted.contains("provider emitted a private diagnostic"));

        let mut prohibited = serde_json::json!({"prompt": "private request body"});
        redact_capacity_log_value(&mut prohibited, &root, &evidence).unwrap();
        assert_eq!(prohibited["prompt"], "<redacted-private-field>");
    }

    #[test]
    fn capacity_log_chunks_reconstruct_exactly_and_reject_transport_mutations() {
        let compressed = b"complete redacted compressed evidence bytes";
        let stream = chunk_capacity_log_stream(
            "capacity-raw-observations-v1.ndjson",
            compressed,
            123,
            "a".repeat(64),
            3,
        )
        .unwrap();
        assert_eq!(
            reconstruct_capacity_log_stream(&stream).unwrap(),
            compressed
        );

        let mut tampered = stream.clone();
        tampered.chunks[0].data.replace_range(0..1, "A");
        assert!(reconstruct_capacity_log_stream(&tampered).is_err());

        let mut missing = stream.clone();
        missing.chunks.pop();
        assert!(reconstruct_capacity_log_stream(&missing).is_err());

        let mut duplicate = stream.clone();
        duplicate.chunks.insert(1, duplicate.chunks[0].clone());
        assert!(reconstruct_capacity_log_stream(&duplicate).is_err());

        let mut unknown = stream.clone();
        unknown.chunks[0].kind = "unknown-capacity-frame";
        assert!(reconstruct_capacity_log_stream(&unknown).is_err());

        let mut reordered = chunk_capacity_log_stream(
            "capacity-raw-observations-v1.ndjson",
            &(0_u8..32).collect::<Vec<_>>(),
            123,
            "b".repeat(64),
            3,
        )
        .unwrap();
        reordered.chunks.swap(0, 1);
        assert!(reconstruct_capacity_log_stream(&reordered).is_err());
    }

    #[test]
    fn capacity_log_gzip_closes_input_and_is_deterministic() {
        let input = b"bounded redacted evidence\n".repeat(1_024);
        let first = gzip_capacity_log(&input).unwrap();
        let second = gzip_capacity_log(&input).unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..3], &[0x1f, 0x8b, 0x08]);
        assert!(first.len() < input.len());
    }

    #[test]
    fn capacity_log_project_bound_stops_instead_of_truncating() {
        validate_capacity_log_size(CAPACITY_LOG_MAX_BYTES).unwrap();
        let error = validate_capacity_log_size(CAPACITY_LOG_MAX_BYTES + 1)
            .unwrap_err()
            .to_string();
        assert!(error.contains("67108865 bytes"));
        assert!(error.contains("separate private artifact-upload decision"));
    }

    #[test]
    fn nearest_rank_uses_only_present_success_durations() {
        let mut values = vec![1, 2, 3, 4, 100];
        assert_eq!(nearest_rank(&mut values, 50), Some(3));
        assert_eq!(nearest_rank(&mut values, 95), Some(100));
        assert_eq!(nearest_rank(&mut values, 99), Some(100));
        assert_eq!(nearest_rank(&mut [], 50), None);
    }
}
