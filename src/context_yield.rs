//! V1.2 Context Yield research harness (G8).
//!
//! Runs a repeatable, frozen-generation Context IR compilation experiment
//! and produces a machine-readable `ContextYieldReport`. "Frozen
//! generation" means every repeated run in one experiment compiles against
//! the exact same `generation_id` — the harness never silently reconciles
//! mid-experiment, so truth cannot shift under a comparison.
//!
//! `compare_context_yield_reports` only ever compares two reports that
//! share workspace, generation, task kind, and both policy versions;
//! anything else is rejected outright rather than producing a
//! misleading number (G8 "invalid comparisons are rejected").

use rusqlite::Connection;

use crate::context_ir::{TaskKind, TaskKindSource};
use crate::error::{AtlasError, Result};
use crate::resolution::deterministic_id;
use crate::task_compiler::{compile_context_ir, CompileRequest};
use crate::workspace::WorkspaceRecord;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextYieldReport {
    pub report_id: String,
    pub workspace_id: String,
    pub generation_id: String,
    pub task_session_id: String,
    pub task_kind: TaskKind,
    pub planner_policy_version: String,
    pub projection_policy_version: String,
    pub context_hash: String,
    pub repeated_runs: i64,
    pub all_runs_identical: bool,
    pub working_set_size: i64,
    pub selected_source_bytes: i64,
    pub selected_estimated_tokens: i64,
    pub serving_fallback: bool,
    pub coverage_claim_strength: String,
    pub truncated: bool,
    pub generated_at: String,
}
/// Closed V1.5 Context Yield report schema. This is a research report
/// contract, not Context IR, and does not participate in Context IR hashing.
pub const CONTEXT_YIELD_REPORT_SCHEMA_VERSION: &str = "1.5.0";
/// Closed schema for the H3-A experimental profile manifest. This contract is
/// harness-only: it neither configures nor changes production compiler policy.
pub const EXPERIMENTAL_PROFILE_MANIFEST_SCHEMA_VERSION: &str = "1.0.0";
pub const H3_A_PROFILE_VERSION: &str = "h3-a-1.0.0";
pub const H3_A_BENCHMARK_MANIFEST: &str = "tests/fixtures/context_yield/benchmark-scenario.json";

/// A frozen set of candidate profiles used by Context Yield experiments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalProfileManifest {
    pub schema_version: String,
    pub benchmark_manifest: String,
    pub profiles: Vec<ExperimentalProfile>,
}

/// One named and versioned H3-A profile. All limits are experiment inputs;
/// production compiler defaults and policy do not consume this type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalProfile {
    pub name: String,
    pub version: String,
    pub records: u64,
    pub source_bytes: u64,
    pub estimated_tokens: u64,
    pub relationship_depth: u64,
    pub work_units: u64,
    pub uncertainty_reserve_percent: u64,
    pub candidate_warm_p95_ms: u64,
    pub wall_cancellation_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExperimentalProfileManifestError {
    #[error("unsupported experimental profile manifest schema version")]
    UnsupportedSchemaVersion,
    #[error("experimental profiles reference an unexpected benchmark manifest")]
    UnexpectedBenchmarkManifest,
    #[error("experimental profiles differ from the accepted H3-A contract")]
    ProfilesDifferFromAcceptedH3A,
}

impl ExperimentalProfileManifest {
    pub fn validate(&self) -> std::result::Result<(), ExperimentalProfileManifestError> {
        if self.schema_version != EXPERIMENTAL_PROFILE_MANIFEST_SCHEMA_VERSION {
            return Err(ExperimentalProfileManifestError::UnsupportedSchemaVersion);
        }
        if self.benchmark_manifest != H3_A_BENCHMARK_MANIFEST {
            return Err(ExperimentalProfileManifestError::UnexpectedBenchmarkManifest);
        }
        if self.profiles.len() != 3
            || !self
                .profiles
                .iter()
                .all(ExperimentalProfile::is_h3_a_candidate)
            || self.profiles[0].name != "small"
            || self.profiles[1].name != "standard"
            || self.profiles[2].name != "audit"
        {
            return Err(ExperimentalProfileManifestError::ProfilesDifferFromAcceptedH3A);
        }
        Ok(())
    }
}

impl ExperimentalProfile {
    fn is_h3_a_candidate(&self) -> bool {
        if self.version != H3_A_PROFILE_VERSION {
            return false;
        }
        let limits = (
            self.records,
            self.source_bytes,
            self.estimated_tokens,
            self.relationship_depth,
            self.work_units,
            self.uncertainty_reserve_percent,
            self.candidate_warm_p95_ms,
            self.wall_cancellation_ms,
        );
        match self.name.as_str() {
            "small" => limits == (30, 12_000, 4_000, 1, 5_000, 15, 150, 1_000),
            "standard" => limits == (60, 40_000, 8_000, 2, 20_000, 12, 250, 5_000),
            "audit" => limits == (250, 120_000, 24_000, 4, 100_000, 15, 2_000, 10_000),
            _ => false,
        }
    }
}

pub const FROZEN_EXPERIMENT_BUNDLE_SCHEMA_VERSION: &str = "1.0.0";

/// The five controlled dimensions required by the H3-A experiment contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentDimension {
    Task,
    Profile,
    Representation,
    Fallback,
    GraphScale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentRepresentation {
    Flat,
    RelationshipPreserving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentServingState {
    Ready,
    DirectTruthFallback,
}

/// A compiler condition. These values are harness inputs only and never
/// mutate production defaults or Context IR serialization.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentCondition {
    pub task_kind: TaskKind,
    pub profile: String,
    pub representation: ExperimentRepresentation,
    pub serving_state: ExperimentServingState,
    pub graph_scale: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenExperiment {
    pub id: String,
    pub independent_variable: ExperimentDimension,
    pub control: ExperimentCondition,
    pub candidate: ExperimentCondition,
    pub accepted_outcome_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenExperimentBundle {
    pub schema_version: String,
    pub experiments: Vec<FrozenExperiment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrozenExperimentError {
    #[error("unsupported frozen experiment bundle schema version")]
    UnsupportedSchemaVersion,
    #[error("the required task/profile/representation/fallback/graph-scale matrix is absent")]
    RequiredMatrixMissing,
    #[error("an experiment must vary exactly its declared independent variable")]
    MoreThanOneVariable,
    #[error("an experiment condition is outside the frozen H3-A contract")]
    InvalidCondition,
    #[error("accepted outcome identity must be a SHA-256 value")]
    InvalidAcceptedOutcome,
}

impl FrozenExperimentBundle {
    pub fn validate(&self) -> std::result::Result<(), FrozenExperimentError> {
        if self.schema_version != FROZEN_EXPERIMENT_BUNDLE_SCHEMA_VERSION {
            return Err(FrozenExperimentError::UnsupportedSchemaVersion);
        }
        let expected = [
            ("task-kinds", ExperimentDimension::Task),
            ("review-task", ExperimentDimension::Task),
            ("profiles", ExperimentDimension::Profile),
            ("representations", ExperimentDimension::Representation),
            ("serving-fallback", ExperimentDimension::Fallback),
            ("graph-scale-10x", ExperimentDimension::GraphScale),
        ];
        if self.experiments.len() != expected.len()
            || !self
                .experiments
                .iter()
                .zip(expected)
                .all(|(experiment, (id, dimension))| {
                    experiment.id == id && experiment.independent_variable == dimension
                })
        {
            return Err(FrozenExperimentError::RequiredMatrixMissing);
        }
        for experiment in &self.experiments {
            experiment.validate()?;
        }
        Ok(())
    }
}

impl FrozenExperiment {
    fn validate(&self) -> std::result::Result<(), FrozenExperimentError> {
        if self.id.is_empty()
            || !is_condition_valid(&self.control)
            || !is_condition_valid(&self.candidate)
        {
            return Err(FrozenExperimentError::InvalidCondition);
        }
        if !is_sha256(&self.accepted_outcome_hash) {
            return Err(FrozenExperimentError::InvalidAcceptedOutcome);
        }
        let changes = [
            (
                ExperimentDimension::Task,
                self.control.task_kind != self.candidate.task_kind,
            ),
            (
                ExperimentDimension::Profile,
                self.control.profile != self.candidate.profile,
            ),
            (
                ExperimentDimension::Representation,
                self.control.representation != self.candidate.representation,
            ),
            (
                ExperimentDimension::Fallback,
                self.control.serving_state != self.candidate.serving_state,
            ),
            (
                ExperimentDimension::GraphScale,
                self.control.graph_scale != self.candidate.graph_scale,
            ),
        ];
        if changes
            .into_iter()
            .filter(|(_, changed)| *changed)
            .map(|(dimension, _)| dimension)
            .collect::<Vec<_>>()
            != [self.independent_variable]
        {
            return Err(FrozenExperimentError::MoreThanOneVariable);
        }
        if self.independent_variable == ExperimentDimension::GraphScale
            && (self.control.graph_scale != 1 || self.candidate.graph_scale != 10)
        {
            return Err(FrozenExperimentError::InvalidCondition);
        }
        Ok(())
    }
}

fn is_condition_valid(condition: &ExperimentCondition) -> bool {
    matches!(condition.profile.as_str(), "small" | "standard" | "audit")
        && matches!(condition.graph_scale, 1 | 10)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// One raw production-compiler observation. It intentionally cannot carry
/// task text, prompt text, source bodies, or package membership.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRawSample {
    pub context_hash: String,
    pub elapsed_micros: u64,
    pub working_set_size: u64,
    pub selected_source_bytes: i64,
    pub selected_estimated_tokens: i64,
    pub serving_fallback: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentVariance {
    pub minimum_micros: u64,
    pub maximum_micros: u64,
    pub mean_micros: u64,
    pub mad_micros: u64,
    pub p50_micros: u64,
    pub p95_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentLimitation {
    SingleEnvironment,
    ColdBootstrapSeparate,
    ModelSideDiscoverySeparate,
    RepresentationAdapterPendingH5,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentArmResult {
    pub condition: ExperimentCondition,
    pub generation_id: String,
    pub catalogue_file_count: u64,
    pub accepted_outcome: ContextYieldOutcome,
    pub raw_samples: Vec<ExperimentRawSample>,
    pub variance: ExperimentVariance,
    pub limitations: Vec<ExperimentLimitation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRunResult {
    pub experiment_id: String,
    pub independent_variable: ExperimentDimension,
    pub control: ExperimentArmResult,
    pub candidate: ExperimentArmResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExperimentComparisonError {
    #[error("experiment result does not match its frozen condition")]
    ConditionMismatch,
    #[error("experiment result uses a profile outside its frozen condition")]
    ProfileMismatch,
    #[error("each experiment arm requires at least two raw samples")]
    InsufficientSamples,
    #[error("an experiment arm produced non-deterministic Context IR")]
    NonDeterministicSamples,
    #[error("an experiment outcome was not accepted")]
    OutcomeNotAccepted,
    #[error("experiment arms reached different accepted outcomes")]
    DifferentAcceptedOutcome,
    #[error("recorded variance does not match the retained raw samples")]
    InvalidVariance,
}

pub struct ExperimentArmSamples {
    pub generation_id: String,
    pub catalogue_file_count: u64,
    pub raw_samples: Vec<ExperimentRawSample>,
}

impl ExperimentRunResult {
    pub fn from_samples(
        experiment: &FrozenExperiment,
        control: ExperimentArmSamples,
        candidate: ExperimentArmSamples,
        limitations: Vec<ExperimentLimitation>,
    ) -> Self {
        let outcome = ContextYieldOutcome {
            acceptance: ContextYieldOutcomeAcceptance::Accepted,
            outcome_hash: experiment.accepted_outcome_hash.clone(),
        };
        Self {
            experiment_id: experiment.id.clone(),
            independent_variable: experiment.independent_variable,
            control: ExperimentArmResult {
                condition: experiment.control.clone(),
                generation_id: control.generation_id,
                catalogue_file_count: control.catalogue_file_count,
                accepted_outcome: outcome.clone(),
                variance: ExperimentVariance::from_samples(&control.raw_samples),
                raw_samples: control.raw_samples,
                limitations: limitations.clone(),
            },
            candidate: ExperimentArmResult {
                condition: experiment.candidate.clone(),
                generation_id: candidate.generation_id,
                catalogue_file_count: candidate.catalogue_file_count,
                accepted_outcome: outcome,
                variance: ExperimentVariance::from_samples(&candidate.raw_samples),
                raw_samples: candidate.raw_samples,
                limitations,
            },
        }
    }

    pub fn validate_against(
        &self,
        experiment: &FrozenExperiment,
    ) -> std::result::Result<(), ExperimentComparisonError> {
        if self.control.condition.profile != experiment.control.profile
            || self.candidate.condition.profile != experiment.candidate.profile
        {
            return Err(ExperimentComparisonError::ProfileMismatch);
        }
        if self.experiment_id != experiment.id
            || self.independent_variable != experiment.independent_variable
            || self.control.condition != experiment.control
            || self.candidate.condition != experiment.candidate
        {
            return Err(ExperimentComparisonError::ConditionMismatch);
        }
        let file_counts_match =
            if experiment.independent_variable == ExperimentDimension::GraphScale {
                self.control
                    .catalogue_file_count
                    .checked_mul(10)
                    .is_some_and(|expected| expected == self.candidate.catalogue_file_count)
            } else {
                self.control.catalogue_file_count == self.candidate.catalogue_file_count
            };
        if !file_counts_match {
            return Err(ExperimentComparisonError::ConditionMismatch);
        }
        if self.control.raw_samples.len() < 2 || self.candidate.raw_samples.len() < 2 {
            return Err(ExperimentComparisonError::InsufficientSamples);
        }
        if !arm_is_deterministic(&self.control) || !arm_is_deterministic(&self.candidate) {
            return Err(ExperimentComparisonError::NonDeterministicSamples);
        }
        if self.control.variance != ExperimentVariance::from_samples(&self.control.raw_samples)
            || self.candidate.variance
                != ExperimentVariance::from_samples(&self.candidate.raw_samples)
        {
            return Err(ExperimentComparisonError::InvalidVariance);
        }
        if self.control.accepted_outcome.acceptance != ContextYieldOutcomeAcceptance::Accepted
            || self.candidate.accepted_outcome.acceptance != ContextYieldOutcomeAcceptance::Accepted
        {
            return Err(ExperimentComparisonError::OutcomeNotAccepted);
        }
        if self.control.accepted_outcome != self.candidate.accepted_outcome
            || self.control.accepted_outcome.outcome_hash != experiment.accepted_outcome_hash
        {
            return Err(ExperimentComparisonError::DifferentAcceptedOutcome);
        }
        Ok(())
    }
}

impl ExperimentVariance {
    fn from_samples(samples: &[ExperimentRawSample]) -> Self {
        if samples.is_empty() {
            return Self {
                minimum_micros: 0,
                maximum_micros: 0,
                mean_micros: 0,
                mad_micros: 0,
                p50_micros: 0,
                p95_micros: 0,
            };
        }
        let mut values: Vec<u64> = samples.iter().map(|sample| sample.elapsed_micros).collect();
        values.sort_unstable();
        let mean = (values.iter().map(|value| u128::from(*value)).sum::<u128>()
            / values.len() as u128) as u64;
        let median = nearest_rank(&values, 50);
        let mut deviations: Vec<u64> = values.iter().map(|value| value.abs_diff(median)).collect();
        deviations.sort_unstable();
        Self {
            minimum_micros: values[0],
            maximum_micros: *values.last().expect("samples is non-empty"),
            mean_micros: mean,
            mad_micros: nearest_rank(&deviations, 50),
            p50_micros: median,
            p95_micros: nearest_rank(&values, 95),
        }
    }
}

fn nearest_rank(values: &[u64], percentile: usize) -> u64 {
    let index = (percentile * values.len()).div_ceil(100).saturating_sub(1);
    values[index]
}

fn arm_is_deterministic(arm: &ExperimentArmResult) -> bool {
    arm.raw_samples
        .windows(2)
        .all(|pair| pair[0].context_hash == pair[1].context_hash)
}

/// A versioned Context Yield report with deterministic content kept separate
/// from execution metadata such as run identity and generation time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldReportV15 {
    pub schema_version: String,
    pub content: ContextYieldReportContentV15,
    pub execution: ContextYieldExecutionMetadata,
}

/// Canonical report data. None of these fields may contain prompt text or
/// source content; task and outcome identity are represented by hashes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldReportContentV15 {
    pub report_id: String,
    pub workspace_id: String,
    pub generation_id: String,
    pub task_hash: String,
    pub task_kind: TaskKind,
    pub planner_policy_version: String,
    pub projection_policy_version: String,
    pub estimator_policy_version: String,
    pub metric_policy_version: String,
    /// Present only for a declared profile experiment. Absence preserves the
    /// pre-T09 report representation byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_profile: Option<ExperimentalProfile>,
    pub accepted_outcome: ContextYieldOutcome,
    pub sample_size: u64,
    pub raw_measures: ContextYieldRawMeasures,
    pub validity: ContextYieldValidity,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldOutcome {
    pub acceptance: ContextYieldOutcomeAcceptance,
    pub outcome_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldOutcomeAcceptance {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldRawMeasures {
    pub samples: Vec<ContextYieldRawSample>,
    pub metrics: Vec<ContextYieldRawMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldRawSample {
    pub context_hash: String,
    pub working_set_size: i64,
    pub selected_source_bytes: i64,
    pub selected_estimated_tokens: i64,
    pub serving_fallback: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldRawMetric {
    pub name: ContextYieldMetricName,
    pub numerator: u64,
    pub denominator: u64,
    pub unit: ContextYieldMetricUnit,
    pub evidence_class: ContextYieldEvidenceClass,
    pub invalidity: Option<ContextYieldMetricInvalidity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldMetricName {
    ContextPrecisionObserved,
    ContextPrecisionReported,
    ContextExpansionCount,
    RediscoveryRate,
    SourceEfficiencyObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldMetricUnit {
    Items,
    Requests,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldEvidenceClass {
    AtlasObserved,
    ReportedUse,
    ObservedAndReported,
}

impl ContextYieldMetricName {
    fn index(self) -> u32 {
        match self {
            Self::ContextPrecisionObserved => 0,
            Self::ContextPrecisionReported => 1,
            Self::ContextExpansionCount => 2,
            Self::RediscoveryRate => 3,
            Self::SourceEfficiencyObserved => 4,
        }
    }

    fn expected_semantics(self) -> (ContextYieldMetricUnit, ContextYieldEvidenceClass) {
        match self {
            Self::ContextPrecisionObserved => (
                ContextYieldMetricUnit::Items,
                ContextYieldEvidenceClass::AtlasObserved,
            ),
            Self::ContextPrecisionReported => (
                ContextYieldMetricUnit::Items,
                ContextYieldEvidenceClass::ReportedUse,
            ),
            Self::ContextExpansionCount => (
                ContextYieldMetricUnit::Items,
                ContextYieldEvidenceClass::ObservedAndReported,
            ),
            Self::RediscoveryRate => (
                ContextYieldMetricUnit::Requests,
                ContextYieldEvidenceClass::AtlasObserved,
            ),
            Self::SourceEfficiencyObserved => (
                ContextYieldMetricUnit::Bytes,
                ContextYieldEvidenceClass::AtlasObserved,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldMetricInvalidity {
    ZeroDenominator,
    InvalidIdentity,
    InvalidRange,
    InvalidSourceBytes,
    MissingEvidenceIdentity,
    UnsupportedEntityKind,
    ConflictingSuppliedBytes,
    CapacityExceeded,
    ArithmeticOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldValidity {
    pub is_valid: bool,
    pub observation_coverage: ContextYieldObservationCoverage,
    pub limitations: Vec<ContextYieldLimitation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextYieldLimitation {
    PartialObservationCoverage,
    SmallSampleSize,
    ServingFallback,
    TruncatedContext,
    SingleEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldObservationCoverage {
    pub observed_events: u64,
    pub eligible_events: u64,
    pub complete: bool,
}

/// Non-canonical execution data. Changing these fields never changes report
/// content or comparability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextYieldExecutionMetadata {
    pub run_id: String,
    pub generated_at: String,
}

/// Run a Context IR compilation `repeat_runs` times against the workspace's
/// *current* active generation (captured once at the start and never
/// re-read mid-experiment) and report whether every run produced a
/// byte-identical `context_hash`. `repeat_runs` must be at least 2 — a
/// single-run "experiment" cannot demonstrate repeatability and is
/// rejected rather than silently reported as `all_runs_identical = true`.
#[allow(clippy::too_many_arguments)]
pub fn run_context_yield_experiment(
    conn: &Connection,
    ws: &WorkspaceRecord,
    task_session_id: &str,
    task_hash: &str,
    normalized_goal_hash: &str,
    task_kind: TaskKind,
    kind_source: TaskKindSource,
    kind_rule_id: Option<String>,
    request: &CompileRequest,
    repeat_runs: i64,
) -> Result<ContextYieldReport> {
    if repeat_runs < 2 {
        return Err(AtlasError::InvalidConfig(format!(
            "run_context_yield_experiment requires repeat_runs >= 2 to demonstrate repeatability, got {repeat_runs}"
        )));
    }

    let mut hashes = Vec::with_capacity(repeat_runs as usize);
    let mut last_ir = None;
    for _ in 0..repeat_runs {
        let ir = compile_context_ir(
            conn,
            ws,
            task_session_id,
            task_hash,
            normalized_goal_hash,
            task_kind,
            kind_source,
            kind_rule_id.clone(),
            request,
        )?;
        hashes.push(ir.context_hash.clone());
        last_ir = Some(ir);
    }
    let ir = last_ir.expect("repeat_runs >= 2 guarantees at least one compilation ran");
    let all_runs_identical = hashes.windows(2).all(|w| w[0] == w[1]);

    let report_id = deterministic_id(
        "yield",
        &[
            &ws.workspace_id,
            &ir.workspace.generation_id,
            task_session_id,
            task_hash,
            &ir.context_hash,
        ],
    );

    Ok(ContextYieldReport {
        report_id,
        workspace_id: ws.workspace_id.clone(),
        generation_id: ir.workspace.generation_id.clone(),
        task_session_id: task_session_id.to_string(),
        task_kind,
        planner_policy_version: ir.policy.planner_policy_version.clone(),
        projection_policy_version: ir.policy.projection_policy_version.clone(),
        context_hash: ir.context_hash.clone(),
        repeated_runs: repeat_runs,
        all_runs_identical,
        working_set_size: ir.working_set.len() as i64,
        selected_source_bytes: ir.cost.selected_source_bytes,
        selected_estimated_tokens: ir.cost.selected_estimated_tokens,
        serving_fallback: ir.status.serving_fallback,
        coverage_claim_strength: format!("{:?}", ir.coverage.claim_strength),
        truncated: ir.omissions.truncated,
        generated_at: crate::migrations::iso8601_now(),
    })
}

/// A yield comparison between two `ContextYieldReport`s that passed the
/// comparability check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextYieldComparison {
    pub report_a_id: String,
    pub report_b_id: String,
    pub context_hash_matches: bool,
    pub working_set_size_delta: i64,
    pub selected_source_bytes_delta: i64,
    pub selected_estimated_tokens_delta: i64,
}

/// Why a comparison was refused. Machine-distinguishable, never a bare
/// string error — a research harness that silently degrades an invalid
/// comparison into a number is worse than one that refuses outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum YieldIncomparableReason {
    DifferentWorkspace,
    DifferentGeneration,
    DifferentTask,
    DifferentTaskKind,
    DifferentPlannerPolicy,
    DifferentProjectionPolicy,
    DifferentEstimatorPolicy,
    DifferentMetricPolicy,
    DifferentSampleSize,
    OutcomeNotAccepted,
    DifferentAcceptedOutcome,
    InvalidReport,
    UnsupportedReportVersion,
    DifferentExperimentalProfileContract,
}

/// Compare two reports only when every comparability dimension matches:
/// same workspace, same frozen generation, same task kind, same planner
/// and projection policy versions. Any mismatch is rejected with a typed
/// reason rather than silently producing a delta between apples and
/// oranges.
pub fn compare_context_yield_reports(
    a: &ContextYieldReport,
    b: &ContextYieldReport,
) -> std::result::Result<ContextYieldComparison, YieldIncomparableReason> {
    if a.workspace_id != b.workspace_id {
        return Err(YieldIncomparableReason::DifferentWorkspace);
    }
    if a.generation_id != b.generation_id {
        return Err(YieldIncomparableReason::DifferentGeneration);
    }
    if a.task_kind != b.task_kind {
        return Err(YieldIncomparableReason::DifferentTaskKind);
    }
    if a.planner_policy_version != b.planner_policy_version {
        return Err(YieldIncomparableReason::DifferentPlannerPolicy);
    }
    if a.projection_policy_version != b.projection_policy_version {
        return Err(YieldIncomparableReason::DifferentProjectionPolicy);
    }

    Ok(ContextYieldComparison {
        report_a_id: a.report_id.clone(),
        report_b_id: b.report_id.clone(),
        context_hash_matches: a.context_hash == b.context_hash,
        working_set_size_delta: b.working_set_size - a.working_set_size,
        selected_source_bytes_delta: b.selected_source_bytes - a.selected_source_bytes,
        selected_estimated_tokens_delta: b.selected_estimated_tokens - a.selected_estimated_tokens,
    })
}

/// A comparison between two valid V1.5 reports. Raw sample hashes are compared
/// in their recorded order after all comparability dimensions pass.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ContextYieldComparisonV15 {
    pub report_a_id: String,
    pub report_b_id: String,
    pub sample_size: u64,
    pub context_hashes_match: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_a: Option<ExperimentalProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_b: Option<ExperimentalProfile>,
}

fn validate_v15_report(
    report: &ContextYieldReportV15,
) -> std::result::Result<(), YieldIncomparableReason> {
    if report.schema_version != CONTEXT_YIELD_REPORT_SCHEMA_VERSION {
        return Err(YieldIncomparableReason::UnsupportedReportVersion);
    }
    if report.content.accepted_outcome.acceptance != ContextYieldOutcomeAcceptance::Accepted {
        return Err(YieldIncomparableReason::OutcomeNotAccepted);
    }

    let content = &report.content;
    let sample_count = u64::try_from(content.raw_measures.samples.len())
        .map_err(|_| YieldIncomparableReason::InvalidReport)?;
    let coverage = &content.validity.observation_coverage;
    let coverage_is_consistent = coverage.observed_events <= coverage.eligible_events
        && coverage.complete == (coverage.observed_events == coverage.eligible_events);
    let samples_are_valid = content.raw_measures.samples.iter().all(|sample| {
        sample.working_set_size >= 0
            && sample.selected_source_bytes >= 0
            && sample.selected_estimated_tokens >= 0
    });
    let mut seen_metrics = 0_u8;
    let metrics_are_valid = content.raw_measures.metrics.len() == 5
        && content.raw_measures.metrics.iter().all(|metric| {
            let bit = 1_u8 << metric.name.index();
            let unique = seen_metrics & bit == 0;
            seen_metrics |= bit;
            let (unit, evidence_class) = metric.name.expected_semantics();
            let value_is_valid = match metric.name {
                ContextYieldMetricName::ContextExpansionCount => metric.denominator == 1,
                _ => metric.denominator > 0 && metric.numerator <= metric.denominator,
            };
            unique
                && metric.unit == unit
                && metric.evidence_class == evidence_class
                && metric.invalidity.is_none()
                && value_is_valid
        })
        && seen_metrics == 0b1_1111;
    let profile_is_valid = content
        .experimental_profile
        .as_ref()
        .is_none_or(ExperimentalProfile::is_h3_a_candidate);
    if !content.validity.is_valid
        || content.sample_size == 0
        || content.sample_size != sample_count
        || !coverage_is_consistent
        || !samples_are_valid
        || !metrics_are_valid
        || !profile_is_valid
    {
        return Err(YieldIncomparableReason::InvalidReport);
    }
    Ok(())
}

/// Compare only valid V1.5 reports for the same task, frozen generation,
/// policies, sample size, and accepted outcome. Execution metadata is
/// intentionally excluded.
pub fn compare_context_yield_reports_v15(
    a: &ContextYieldReportV15,
    b: &ContextYieldReportV15,
) -> std::result::Result<ContextYieldComparisonV15, YieldIncomparableReason> {
    validate_v15_report(a)?;
    validate_v15_report(b)?;
    let a = &a.content;
    let b = &b.content;

    if a.workspace_id != b.workspace_id {
        return Err(YieldIncomparableReason::DifferentWorkspace);
    }
    if a.generation_id != b.generation_id {
        return Err(YieldIncomparableReason::DifferentGeneration);
    }
    if a.task_hash != b.task_hash {
        return Err(YieldIncomparableReason::DifferentTask);
    }
    if a.task_kind != b.task_kind {
        return Err(YieldIncomparableReason::DifferentTaskKind);
    }
    if a.planner_policy_version != b.planner_policy_version {
        return Err(YieldIncomparableReason::DifferentPlannerPolicy);
    }
    if a.projection_policy_version != b.projection_policy_version {
        return Err(YieldIncomparableReason::DifferentProjectionPolicy);
    }
    if a.estimator_policy_version != b.estimator_policy_version {
        return Err(YieldIncomparableReason::DifferentEstimatorPolicy);
    }
    if a.metric_policy_version != b.metric_policy_version {
        return Err(YieldIncomparableReason::DifferentMetricPolicy);
    }
    if a.sample_size != b.sample_size {
        return Err(YieldIncomparableReason::DifferentSampleSize);
    }
    if a.accepted_outcome.outcome_hash != b.accepted_outcome.outcome_hash {
        return Err(YieldIncomparableReason::DifferentAcceptedOutcome);
    }
    match (
        a.experimental_profile.as_ref(),
        b.experimental_profile.as_ref(),
    ) {
        (Some(a_profile), Some(b_profile)) if a_profile.version == b_profile.version => {}
        (None, None) => {}
        _ => {
            return Err(YieldIncomparableReason::DifferentExperimentalProfileContract);
        }
    }

    let context_hashes_match = a
        .raw_measures
        .samples
        .iter()
        .zip(&b.raw_measures.samples)
        .all(|(a, b)| a.context_hash == b.context_hash);
    Ok(ContextYieldComparisonV15 {
        report_a_id: a.report_id.clone(),
        report_b_id: b.report_id.clone(),
        sample_size: a.sample_size,
        profile_a: a.experimental_profile.clone(),
        profile_b: b.experimental_profile.clone(),
        context_hashes_match,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::discovery;
    use crate::workspace::register_workspace;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, rusqlite::Connection, WorkspaceRecord) {
        let db_dir = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/a.ts"),
            "import { b } from './b';\nexport function alpha() { return b(); }\n",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/b.ts"),
            "export function b() { return 2; }\n",
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        discovery::reconcile(&ws, &conn, &cfg).unwrap();
        (db_dir, conn, ws)
    }

    fn h(n: u8) -> String {
        std::iter::repeat_n(char::from_digit(n as u32, 16).unwrap(), 64).collect()
    }

    #[test]
    fn experiment_rejects_fewer_than_two_repeats() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let err = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            1,
        );
        assert!(
            err.is_err(),
            "a single-run 'experiment' cannot demonstrate repeatability"
        );
    }

    #[test]
    fn experiment_reports_repeatable_identical_outcomes() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let report = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            5,
        )
        .unwrap();
        assert!(
            report.all_runs_identical,
            "5 repeated compilations against a frozen generation must be byte-identical"
        );
        assert_eq!(report.repeated_runs, 5);
        assert!(report.working_set_size > 0);
    }

    #[test]
    fn comparable_reports_produce_a_comparison() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let a = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            2,
        )
        .unwrap();
        let b = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            2,
        )
        .unwrap();
        let cmp = compare_context_yield_reports(&a, &b)
            .expect("identical experiments against the same frozen generation must be comparable");
        assert!(cmp.context_hash_matches);
        assert_eq!(cmp.working_set_size_delta, 0);
    }

    #[test]
    fn different_task_kind_is_rejected_as_incomparable() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let a = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            2,
        )
        .unwrap();
        let b = run_context_yield_experiment(
            &conn,
            &ws,
            "task_2",
            &h(2),
            &h(3),
            TaskKind::Refactor,
            TaskKindSource::Declared,
            None,
            &request,
            2,
        )
        .unwrap();
        let err = compare_context_yield_reports(&a, &b).unwrap_err();
        assert_eq!(err, YieldIncomparableReason::DifferentTaskKind);
    }

    #[test]
    fn different_generation_is_rejected_as_incomparable() {
        let (_d, conn, ws) = setup();
        let request = CompileRequest {
            known_symbols: vec!["alpha".to_string()],
            ..Default::default()
        };
        let a = run_context_yield_experiment(
            &conn,
            &ws,
            "task_1",
            &h(0),
            &h(1),
            TaskKind::BugFix,
            TaskKindSource::Declared,
            None,
            &request,
            2,
        )
        .unwrap();

        // Force a synthetic second report claiming a different generation
        // (simulating a report captured before/after a reconcile) to prove
        // the comparability check actually inspects generation_id, not
        // just object identity.
        let mut b = a.clone();
        b.generation_id = "gen_different".to_string();
        let err = compare_context_yield_reports(&a, &b).unwrap_err();
        assert_eq!(err, YieldIncomparableReason::DifferentGeneration);
    }
}
