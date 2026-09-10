//! Additive, transport-neutral Context Governor contracts.
//!
//! This module freezes decision, execution, and discovery DTOs. It performs no
//! repository dispatch, persistence, source reads, adaptive selection, or
//! public CLI/MCP parsing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::context_ir::TaskKind;
use crate::context_metrics::{
    ContextExecutionCounters, MAX_CONTEXT_EXECUTION_ATLAS_CALLS,
    MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS, MAX_CONTEXT_EXECUTION_RECORDS,
    MAX_CONTEXT_EXECUTION_SOURCE_BYTES, MAX_CONTEXT_EXECUTION_WORK_UNITS,
};

pub const CONTEXT_ROUTE_POLICY_VERSION: &str = "context-route-v2.0.0";
pub const CONTEXT_EXECUTION_VERSION: &str = "context-execution-v2.0.0";
pub const CONTEXT_CAPABILITIES_VERSION: &str = "context-capabilities-v2.0.0";
pub const TASK_CLASSIFIER_VERSION: &str = "task-classifier-v1.0.0";
pub const CONTEXT_IR_VERSION: &str = "2.0.0";
pub const DEEP_PLANNER_VERSION: &str = "planner-v2.0.0";
pub const DEEP_PROJECTION_VERSION: &str = "projection-v2.0.0";
pub const DEEP_ESTIMATOR_VERSION: &str = "context-estimator-v1.0.0";
pub const QUERY_OPERATION_VERSION: &str = "query-v1.0.0";
pub const SOURCE_REFERENCE_OPERATION_VERSION: &str = "source-reference-v1.0.0";
pub const MAX_ROUTE_CAPABILITIES: usize = 8;
pub const MAX_ROUTE_ATTEMPTS: usize = 8;
pub const MAX_EXECUTION_DIAGNOSTICS: usize = 32;
pub const MAX_MATERIALIZED_SOURCES: usize = 64;
pub const MAX_DEEP_DEPTH: u32 = 64;
pub const MAX_GOVERNOR_REQUEST_BYTES: usize = 1024 * 1024;
pub const DEFAULT_APPLICATION_PAGE_LIMIT: usize = 50;
pub const MAX_APPLICATION_PAGE_LIMIT: usize = 200;
pub const MAX_APPLICATION_CURSOR_BYTES: usize = 4096;
pub const MAX_COMPILED_CONTEXT_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_APPLICATION_NESTED_VECTOR_ITEMS: usize = 10_000;
pub const MAX_APPLICATION_STRING_BYTES: usize = 65_536;
pub const MAX_APPLICATION_JSON_DEPTH: usize = 64;
pub const MAX_GOVERNOR_TARGET_BYTES: usize = 512;
pub const MAX_GOVERNOR_TARGETS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Route {
    #[serde(rename = "DIRECT")]
    Direct,
    #[serde(rename = "ATLAS_LIGHT")]
    AtlasLight,
    #[serde(rename = "ATLAS_DEEP")]
    AtlasDeep,
}

impl Route {
    fn ascii(self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::AtlasLight => "ATLAS_LIGHT",
            Self::AtlasDeep => "ATLAS_DEEP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCapability {
    IdentityLookup,
    ExactSource,
    BoundedRelationships,
    ImpactFrontier,
    BasicCoverageConflicts,
    TemporalHistory,
    RequiredRoleClosure,
    ValidationPlan,
}

impl RequiredCapability {
    fn ascii(self) -> &'static str {
        match self {
            Self::IdentityLookup => "identity_lookup",
            Self::ExactSource => "exact_source",
            Self::BoundedRelationships => "bounded_relationships",
            Self::ImpactFrontier => "impact_frontier",
            Self::BasicCoverageConflicts => "basic_coverage_conflicts",
            Self::TemporalHistory => "temporal_history",
            Self::RequiredRoleClosure => "required_role_closure",
            Self::ValidationPlan => "validation_plan",
        }
    }

    fn minimum_route(self) -> Route {
        match self {
            Self::IdentityLookup
            | Self::ExactSource
            | Self::BoundedRelationships
            | Self::ImpactFrontier
            | Self::BasicCoverageConflicts => Route::AtlasLight,
            Self::TemporalHistory | Self::RequiredRoleClosure | Self::ValidationPlan => {
                Route::AtlasDeep
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerCapabilityState {
    Satisfied,
    Unsatisfied,
    Unknown,
}

impl CallerCapabilityState {
    fn ascii(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Unsatisfied => "unsatisfied",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtlasIntent {
    None,
    Allow,
    Require,
}

impl AtlasIntent {
    fn ascii(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Allow => "allow",
            Self::Require => "require",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationUnavailableReason {
    WorkspaceUnregistered,
    CatalogueUnavailable,
    NoActiveGeneration,
    ObservationFailed,
}

impl GenerationUnavailableReason {
    fn ascii(self) -> &'static str {
        match self {
            Self::WorkspaceUnregistered => "workspace_unregistered",
            Self::CatalogueUnavailable => "catalogue_unavailable",
            Self::NoActiveGeneration => "no_active_generation",
            Self::ObservationFailed => "observation_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenerationObservation {
    Observed { generation_id: String },
    Unavailable { reason: GenerationUnavailableReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSignal {
    CallerCapabilitiesSatisfied,
    CallerCapabilitiesUnsatisfied,
    CallerCapabilitiesUnknown,
    AtlasIntentNone,
    AtlasIntentAllow,
    AtlasIntentRequire,
    RouteFloorDirect,
    RouteFloorLight,
    RouteFloorDeep,
    RouteCeilingDirect,
    RouteCeilingLight,
    RouteCeilingDeep,
    ExplicitSeedsPresent,
    IdentityOrSourceRequested,
    RelationshipsOrImpactRequested,
    BasicCoverageConflictsRequested,
    DeepSynthesisRequested,
    StartingGenerationObserved,
    StartingGenerationUnavailable,
    CapabilityProfileApplied,
    CostProfileApplied,
}

impl RouteSignal {
    fn ascii(self) -> &'static str {
        match self {
            Self::CallerCapabilitiesSatisfied => "caller_capabilities_satisfied",
            Self::CallerCapabilitiesUnsatisfied => "caller_capabilities_unsatisfied",
            Self::CallerCapabilitiesUnknown => "caller_capabilities_unknown",
            Self::AtlasIntentNone => "atlas_intent_none",
            Self::AtlasIntentAllow => "atlas_intent_allow",
            Self::AtlasIntentRequire => "atlas_intent_require",
            Self::RouteFloorDirect => "route_floor_direct",
            Self::RouteFloorLight => "route_floor_light",
            Self::RouteFloorDeep => "route_floor_deep",
            Self::RouteCeilingDirect => "route_ceiling_direct",
            Self::RouteCeilingLight => "route_ceiling_light",
            Self::RouteCeilingDeep => "route_ceiling_deep",
            Self::ExplicitSeedsPresent => "explicit_seeds_present",
            Self::IdentityOrSourceRequested => "identity_or_source_requested",
            Self::RelationshipsOrImpactRequested => "relationships_or_impact_requested",
            Self::BasicCoverageConflictsRequested => "basic_coverage_conflicts_requested",
            Self::DeepSynthesisRequested => "deep_synthesis_requested",
            Self::StartingGenerationObserved => "starting_generation_observed",
            Self::StartingGenerationUnavailable => "starting_generation_unavailable",
            Self::CapabilityProfileApplied => "capability_profile_applied",
            Self::CostProfileApplied => "cost_profile_applied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReason {
    CallerContextSufficient,
    CallerContextIncomplete,
    AtlasNotRequested,
    ExplicitAtlasRequest,
    LightCapabilityRequired,
    DeepCapabilityRequired,
    RouteFloorApplied,
    StartingGenerationUnavailable,
}

impl RouteReason {
    fn ascii(self) -> &'static str {
        match self {
            Self::CallerContextSufficient => "caller_context_sufficient",
            Self::CallerContextIncomplete => "caller_context_incomplete",
            Self::AtlasNotRequested => "atlas_not_requested",
            Self::ExplicitAtlasRequest => "explicit_atlas_request",
            Self::LightCapabilityRequired => "light_capability_required",
            Self::DeepCapabilityRequired => "deep_capability_required",
            Self::RouteFloorApplied => "route_floor_applied",
            Self::StartingGenerationUnavailable => "starting_generation_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedSummary {
    pub digest: String,
    pub path_count: u32,
    pub symbol_count: u32,
}

impl SeedSummary {
    /// Canonical digest/counts for the actual explicit path and symbol targets.
    pub fn from_targets(paths: &[String], symbols: &[String]) -> Result<Self, ContextRouteError> {
        let mut paths = paths.to_vec();
        paths.sort();
        paths.dedup();
        let mut symbols = symbols.to_vec();
        symbols.sort();
        symbols.dedup();
        let path_count =
            u32::try_from(paths.len()).map_err(|_| ContextRouteError::InvalidIdentity)?;
        let symbol_count =
            u32::try_from(symbols.len()).map_err(|_| ContextRouteError::InvalidIdentity)?;
        let mut encoder = CanonicalEncoder::new("workspace-atlas/context-route-seeds-v1");
        encoder.u64(u64::from(path_count));
        for path in &paths {
            encoder.text(path);
        }
        encoder.u64(u64::from(symbol_count));
        for symbol in &symbols {
            encoder.text(symbol);
        }
        Ok(Self {
            digest: encoder.finish(),
            path_count,
            symbol_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileIdentity {
    pub profile_id: String,
    pub profile_version: String,
    pub manifest_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightOperation {
    Query,
    SourceReference,
}

impl LightOperation {
    fn ascii(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::SourceReference => "source_reference",
        }
    }

    fn version(self) -> &'static str {
        match self {
            Self::Query => QUERY_OPERATION_VERSION,
            Self::SourceReference => SOURCE_REFERENCE_OPERATION_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedOperation {
    pub operation: LightOperation,
    pub version: String,
}

impl VersionedOperation {
    pub fn new(operation: LightOperation) -> Self {
        Self {
            operation,
            version: operation.version().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepContractVersions {
    pub planner_version: String,
    pub projection_version: String,
    pub context_ir_version: String,
    pub estimator_version: String,
}

impl DeepContractVersions {
    pub fn fixed() -> Self {
        Self {
            planner_version: DEEP_PLANNER_VERSION.to_string(),
            projection_version: DEEP_PROJECTION_VERSION.to_string(),
            context_ir_version: CONTEXT_IR_VERSION.to_string(),
            estimator_version: DEEP_ESTIMATOR_VERSION.to_string(),
        }
    }

    fn validate(&self) -> Result<(), ContextRouteError> {
        if self.planner_version != DEEP_PLANNER_VERSION
            || self.projection_version != DEEP_PROJECTION_VERSION
            || self.context_ir_version != CONTEXT_IR_VERSION
            || self.estimator_version != DEEP_ESTIMATOR_VERSION
        {
            return Err(ContextRouteError::UnsupportedVersion {
                contract: "deep_contract".to_string(),
                supplied: format!(
                    "{}/{}/{}/{}",
                    self.planner_version,
                    self.projection_version,
                    self.context_ir_version,
                    self.estimator_version
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepSemanticBudget {
    pub max_records: u64,
    pub max_source_bytes: u64,
    pub max_estimated_tokens: u64,
    pub max_depth: u32,
    pub max_work_units: u64,
    pub uncertainty_reserve_percent: u8,
    pub budget_digest: String,
}

impl DeepSemanticBudget {
    pub fn new(
        max_records: u64,
        max_source_bytes: u64,
        max_estimated_tokens: u64,
        max_depth: u32,
        max_work_units: u64,
        uncertainty_reserve_percent: u8,
    ) -> Result<Self, ContextRouteError> {
        let mut value = Self {
            max_records,
            max_source_bytes,
            max_estimated_tokens,
            max_depth,
            max_work_units,
            uncertainty_reserve_percent,
            budget_digest: String::new(),
        };
        value.validate_values()?;
        value.budget_digest = value.expected_digest();
        Ok(value)
    }

    fn validate_values(&self) -> Result<(), ContextRouteError> {
        if self.max_records == 0
            || self.max_records > MAX_CONTEXT_EXECUTION_RECORDS
            || self.max_source_bytes == 0
            || self.max_source_bytes > MAX_CONTEXT_EXECUTION_SOURCE_BYTES
            || self.max_estimated_tokens == 0
            || self.max_estimated_tokens > MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS
            || self.max_depth == 0
            || self.max_depth > MAX_DEEP_DEPTH
            || self.max_work_units == 0
            || self.max_work_units > MAX_CONTEXT_EXECUTION_WORK_UNITS
            || self.uncertainty_reserve_percent == 0
            || self.uncertainty_reserve_percent > 100
        {
            return Err(ContextRouteError::InvalidDeepBudget);
        }
        Ok(())
    }

    fn expected_digest(&self) -> String {
        let mut encoder = CanonicalEncoder::new("workspace-atlas/context-deep-budget-v1");
        encoder.u64(self.max_records);
        encoder.u64(self.max_source_bytes);
        encoder.u64(self.max_estimated_tokens);
        encoder.u64(u64::from(self.max_depth));
        encoder.u64(self.max_work_units);
        encoder.u64(u64::from(self.uncertainty_reserve_percent));
        encoder.finish()
    }

    pub fn validate(&self) -> Result<(), ContextRouteError> {
        self.validate_values()?;
        if self.budget_digest != self.expected_digest() {
            return Err(ContextRouteError::InvalidDeepBudgetDigest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRouteRequest {
    pub schema_version: String,
    pub normalized_task_hash: String,
    pub declared_kind: Option<TaskKind>,
    pub classifier_kind: TaskKind,
    pub classifier_version: String,
    pub explicit_seeds: SeedSummary,
    pub caller_capabilities: BTreeMap<RequiredCapability, CallerCapabilityState>,
    pub atlas_intent: AtlasIntent,
    pub route_floor: Route,
    pub route_ceiling: Route,
    pub capability_profile: Option<ProfileIdentity>,
    pub cost_profile: Option<ProfileIdentity>,
    pub profile_registry_digest: Option<String>,
    pub starting_generation: GenerationObservation,
    pub light_operations: Vec<VersionedOperation>,
    pub deep_contracts: DeepContractVersions,
    pub deep_budget: Option<DeepSemanticBudget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRouteDecision {
    pub schema_version: String,
    pub request: ContextRouteRequest,
    pub effective_floor: Route,
    pub effective_ceiling: Route,
    pub requested_capabilities: BTreeMap<RequiredCapability, CallerCapabilityState>,
    pub deep_budget_digest: Option<String>,
    pub initial_route: Route,
    pub signals: Vec<RouteSignal>,
    pub reasons: Vec<RouteReason>,
    pub starting_generation: GenerationObservation,
    pub policy_version: String,
    pub decision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextRouteError {
    #[error("unsupported {contract} version {supplied}")]
    UnsupportedVersion { contract: String, supplied: String },
    #[error("no common Atlas contract version")]
    NoCommonVersion { supported: Box<SupportedVersions> },
    #[error("route floor is above route ceiling")]
    FloorAboveCeiling,
    #[error("Atlas intent conflicts with the route floor")]
    IntentFloorConflict,
    #[error("Atlas intent requires a route excluded by the ceiling")]
    IntentCeilingConflict,
    #[error("a DEEP-permitting request requires explicit semantic budgets")]
    DeepBudgetRequired,
    #[error("deep semantic budgets must be positive and within advertised bounds")]
    InvalidDeepBudget,
    #[error("deep semantic budget digest is invalid")]
    InvalidDeepBudgetDigest,
    #[error("hash or identifier is malformed")]
    InvalidIdentity,
    #[error("LIGHT operation contracts are incomplete, duplicated, unordered, or unsupported")]
    InvalidLightOperations,
    #[error("no capability or cost profiles are registered by context-route-v2.0.0")]
    UnknownProfile,
    #[error("execution envelope violates its state, route, payload, or bound invariants")]
    InvalidExecution,
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.is_ascii()
        && value.bytes().all(|byte| !byte.is_ascii_control())
}

fn validate_request(request: &ContextRouteRequest) -> Result<(), ContextRouteError> {
    if request.schema_version != CONTEXT_ROUTE_POLICY_VERSION {
        return Err(ContextRouteError::UnsupportedVersion {
            contract: "route".to_string(),
            supplied: request.schema_version.clone(),
        });
    }
    if request.classifier_version != TASK_CLASSIFIER_VERSION {
        return Err(ContextRouteError::UnsupportedVersion {
            contract: "classifier".to_string(),
            supplied: request.classifier_version.clone(),
        });
    }
    if !valid_digest(&request.normalized_task_hash)
        || !valid_digest(&request.explicit_seeds.digest)
        || request.caller_capabilities.len() > MAX_ROUTE_CAPABILITIES
    {
        return Err(ContextRouteError::InvalidIdentity);
    }
    if let GenerationObservation::Observed { generation_id } = &request.starting_generation {
        if !valid_opaque_id(generation_id) {
            return Err(ContextRouteError::InvalidIdentity);
        }
    }
    if request.route_floor > request.route_ceiling {
        return Err(ContextRouteError::FloorAboveCeiling);
    }
    if request.atlas_intent == AtlasIntent::None && request.route_floor != Route::Direct {
        return Err(ContextRouteError::IntentFloorConflict);
    }
    if request.atlas_intent == AtlasIntent::Require && request.route_ceiling == Route::Direct {
        return Err(ContextRouteError::IntentCeilingConflict);
    }
    if request.route_ceiling == Route::AtlasDeep && request.deep_budget.is_none() {
        return Err(ContextRouteError::DeepBudgetRequired);
    }
    if let Some(budget) = &request.deep_budget {
        budget.validate()?;
    }
    if request.capability_profile.is_some()
        || request.cost_profile.is_some()
        || request.profile_registry_digest.is_some()
    {
        return Err(ContextRouteError::UnknownProfile);
    }
    request.deep_contracts.validate()?;
    let expected = [LightOperation::Query, LightOperation::SourceReference];
    if request.light_operations.len() != expected.len()
        || request
            .light_operations
            .iter()
            .zip(expected)
            .any(|(actual, operation)| {
                actual.operation != operation || actual.version != operation.version()
            })
    {
        return Err(ContextRouteError::InvalidLightOperations);
    }
    Ok(())
}

pub fn decide_context_route(
    request: ContextRouteRequest,
) -> Result<ContextRouteDecision, ContextRouteError> {
    validate_request(&request)?;

    let (effective_floor, effective_ceiling, initial_route) = match request.atlas_intent {
        AtlasIntent::None => (Route::Direct, Route::Direct, Route::Direct),
        AtlasIntent::Allow => (
            request.route_floor,
            request.route_ceiling,
            request.route_floor,
        ),
        AtlasIntent::Require => {
            let floor = request.route_floor.max(Route::AtlasLight);
            (floor, request.route_ceiling, floor)
        }
    };

    let states: BTreeSet<_> = request.caller_capabilities.values().copied().collect();
    let mut signals = BTreeSet::new();
    for state in states {
        signals.insert(match state {
            CallerCapabilityState::Satisfied => RouteSignal::CallerCapabilitiesSatisfied,
            CallerCapabilityState::Unsatisfied => RouteSignal::CallerCapabilitiesUnsatisfied,
            CallerCapabilityState::Unknown => RouteSignal::CallerCapabilitiesUnknown,
        });
    }
    signals.insert(match request.atlas_intent {
        AtlasIntent::None => RouteSignal::AtlasIntentNone,
        AtlasIntent::Allow => RouteSignal::AtlasIntentAllow,
        AtlasIntent::Require => RouteSignal::AtlasIntentRequire,
    });
    signals.insert(match effective_floor {
        Route::Direct => RouteSignal::RouteFloorDirect,
        Route::AtlasLight => RouteSignal::RouteFloorLight,
        Route::AtlasDeep => RouteSignal::RouteFloorDeep,
    });
    signals.insert(match effective_ceiling {
        Route::Direct => RouteSignal::RouteCeilingDirect,
        Route::AtlasLight => RouteSignal::RouteCeilingLight,
        Route::AtlasDeep => RouteSignal::RouteCeilingDeep,
    });
    if request.explicit_seeds.path_count > 0 || request.explicit_seeds.symbol_count > 0 {
        signals.insert(RouteSignal::ExplicitSeedsPresent);
    }
    let requested: BTreeSet<_> = request.caller_capabilities.keys().copied().collect();
    if requested.contains(&RequiredCapability::IdentityLookup)
        || requested.contains(&RequiredCapability::ExactSource)
    {
        signals.insert(RouteSignal::IdentityOrSourceRequested);
    }
    if requested.contains(&RequiredCapability::BoundedRelationships)
        || requested.contains(&RequiredCapability::ImpactFrontier)
    {
        signals.insert(RouteSignal::RelationshipsOrImpactRequested);
    }
    if requested.contains(&RequiredCapability::BasicCoverageConflicts) {
        signals.insert(RouteSignal::BasicCoverageConflictsRequested);
    }
    if requested
        .iter()
        .any(|capability| capability.minimum_route() == Route::AtlasDeep)
    {
        signals.insert(RouteSignal::DeepSynthesisRequested);
    }
    signals.insert(match request.starting_generation {
        GenerationObservation::Observed { .. } => RouteSignal::StartingGenerationObserved,
        GenerationObservation::Unavailable { .. } => RouteSignal::StartingGenerationUnavailable,
    });

    let all_satisfied = request
        .caller_capabilities
        .values()
        .all(|state| *state == CallerCapabilityState::Satisfied);
    let incomplete: Vec<_> = request
        .caller_capabilities
        .iter()
        .filter_map(|(capability, state)| {
            (*state != CallerCapabilityState::Satisfied).then_some(*capability)
        })
        .collect();
    let mut reasons = BTreeSet::new();
    reasons.insert(if all_satisfied {
        RouteReason::CallerContextSufficient
    } else {
        RouteReason::CallerContextIncomplete
    });
    if request.atlas_intent == AtlasIntent::None {
        reasons.insert(RouteReason::AtlasNotRequested);
    }
    if request.atlas_intent == AtlasIntent::Require {
        reasons.insert(RouteReason::ExplicitAtlasRequest);
    }
    if incomplete
        .iter()
        .any(|capability| capability.minimum_route() == Route::AtlasLight)
    {
        reasons.insert(RouteReason::LightCapabilityRequired);
    }
    if incomplete
        .iter()
        .any(|capability| capability.minimum_route() == Route::AtlasDeep)
    {
        reasons.insert(RouteReason::DeepCapabilityRequired);
    }
    if effective_floor > Route::Direct {
        reasons.insert(RouteReason::RouteFloorApplied);
    }
    if matches!(
        request.starting_generation,
        GenerationObservation::Unavailable { .. }
    ) {
        reasons.insert(RouteReason::StartingGenerationUnavailable);
    }

    let mut decision = ContextRouteDecision {
        schema_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        requested_capabilities: request.caller_capabilities.clone(),
        deep_budget_digest: request
            .deep_budget
            .as_ref()
            .map(|budget| budget.budget_digest.clone()),
        starting_generation: request.starting_generation.clone(),
        request,
        effective_floor,
        effective_ceiling,
        initial_route,
        signals: signals.into_iter().collect(),
        reasons: reasons.into_iter().collect(),
        policy_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        decision_hash: String::new(),
    };
    decision.decision_hash = canonical_decision_hash(&decision);
    Ok(decision)
}

impl ContextRouteDecision {
    pub fn validate(&self) -> Result<(), ContextRouteError> {
        validate_request(&self.request)?;
        if self.schema_version != CONTEXT_ROUTE_POLICY_VERSION
            || self.policy_version != CONTEXT_ROUTE_POLICY_VERSION
            || self.requested_capabilities != self.request.caller_capabilities
            || self.starting_generation != self.request.starting_generation
            || self.deep_budget_digest
                != self
                    .request
                    .deep_budget
                    .as_ref()
                    .map(|budget| budget.budget_digest.clone())
            || self.decision_hash != canonical_decision_hash(self)
        {
            return Err(ContextRouteError::InvalidIdentity);
        }
        let rebuilt = decide_context_route(self.request.clone())?;
        if rebuilt != *self {
            return Err(ContextRouteError::InvalidIdentity);
        }
        Ok(())
    }
}

struct CanonicalEncoder {
    hasher: Sha256,
}

impl CanonicalEncoder {
    fn new(domain: &str) -> Self {
        let mut value = Self {
            hasher: Sha256::new(),
        };
        value.text(domain);
        value
    }

    fn bytes(&mut self, value: &[u8]) {
        self.hasher.update((value.len() as u64).to_be_bytes());
        self.hasher.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn optional_text(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.bytes(&[1]);
                self.text(value);
            }
            None => self.bytes(&[0]),
        }
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    fn finish(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

fn task_kind_ascii(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Explore => "explore",
        TaskKind::BugFix => "bug_fix",
        TaskKind::BehaviorChange => "behavior_change",
        TaskKind::ApiChange => "api_change",
        TaskKind::Refactor => "refactor",
        TaskKind::ConfigurationChange => "configuration_change",
        TaskKind::TestChange => "test_change",
        TaskKind::Review => "review",
        TaskKind::Audit => "audit",
        TaskKind::Unknown => "unknown",
    }
}

fn encode_generation(encoder: &mut CanonicalEncoder, generation: &GenerationObservation) {
    match generation {
        GenerationObservation::Observed { generation_id } => {
            encoder.text("observed");
            encoder.text(generation_id);
        }
        GenerationObservation::Unavailable { reason } => {
            encoder.text("unavailable");
            encoder.text(reason.ascii());
        }
    }
}

fn canonical_decision_hash(decision: &ContextRouteDecision) -> String {
    let request = &decision.request;
    let mut encoder = CanonicalEncoder::new("workspace-atlas/context-route-decision-v1");
    // Contract and policy versions are intentionally encoded first.
    encoder.text(&request.schema_version);
    encoder.text(&decision.schema_version);
    encoder.text(&decision.policy_version);
    encoder.text(&request.classifier_version);
    for operation in &request.light_operations {
        encoder.text(operation.operation.ascii());
        encoder.text(&operation.version);
    }
    encoder.text(&request.deep_contracts.planner_version);
    encoder.text(&request.deep_contracts.projection_version);
    encoder.text(&request.deep_contracts.context_ir_version);
    encoder.text(&request.deep_contracts.estimator_version);

    encoder.text(&request.normalized_task_hash);
    encoder.optional_text(request.declared_kind.map(task_kind_ascii));
    encoder.text(task_kind_ascii(request.classifier_kind));
    encoder.text(&request.explicit_seeds.digest);
    encoder.u64(u64::from(request.explicit_seeds.path_count));
    encoder.u64(u64::from(request.explicit_seeds.symbol_count));
    encoder.u64(request.caller_capabilities.len() as u64);
    for (capability, state) in &request.caller_capabilities {
        encoder.text(capability.ascii());
        encoder.text(state.ascii());
    }
    encoder.text(request.atlas_intent.ascii());
    encoder.text(request.route_floor.ascii());
    encoder.text(request.route_ceiling.ascii());
    for profile in [&request.capability_profile, &request.cost_profile] {
        if let Some(profile) = profile {
            encoder.bytes(&[1]);
            encoder.text(&profile.profile_id);
            encoder.text(&profile.profile_version);
            encoder.text(&profile.manifest_digest);
        } else {
            encoder.bytes(&[0]);
        }
    }
    encoder.optional_text(request.profile_registry_digest.as_deref());
    encode_generation(&mut encoder, &request.starting_generation);
    if let Some(budget) = &request.deep_budget {
        encoder.bytes(&[1]);
        encoder.u64(budget.max_records);
        encoder.u64(budget.max_source_bytes);
        encoder.u64(budget.max_estimated_tokens);
        encoder.u64(u64::from(budget.max_depth));
        encoder.u64(budget.max_work_units);
        encoder.u64(u64::from(budget.uncertainty_reserve_percent));
        encoder.text(&budget.budget_digest);
    } else {
        encoder.bytes(&[0]);
    }

    encoder.text(decision.effective_floor.ascii());
    encoder.text(decision.effective_ceiling.ascii());
    encoder.text(decision.initial_route.ascii());
    encoder.optional_text(decision.deep_budget_digest.as_deref());
    encoder.u64(decision.requested_capabilities.len() as u64);
    for (capability, state) in &decision.requested_capabilities {
        encoder.text(capability.ascii());
        encoder.text(state.ascii());
    }
    for signal in &decision.signals {
        encoder.text(signal.ascii());
    }
    for reason in &decision.reasons {
        encoder.text(reason.ascii());
    }
    encode_generation(&mut encoder, &decision.starting_generation);
    encoder.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDeficitReason {
    Unavailable,
    Ambiguous,
    Conflicting,
    Stale,
    Unsupported,
    SemanticBudgetOmitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteFailureReason {
    StartingGenerationUnavailable,
    NoActiveGeneration,
    GenerationChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "deficit_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteDeficit {
    Capability {
        capability: RequiredCapability,
        reason: CapabilityDeficitReason,
    },
    Route {
        reason: RouteFailureReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Completed,
    Partial,
    Blocked,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Completed,
    Partial,
    Blocked,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAttempt {
    pub attempt_id: String,
    pub route: Route,
    pub state: AttemptState,
    pub deficits: Vec<RouteDeficit>,
    pub counters: ContextExecutionCounters,
    pub elapsed_micros: u64,
}

impl ExecutionAttempt {
    pub fn completed(
        attempt_id: impl Into<String>,
        route: Route,
        counters: ContextExecutionCounters,
    ) -> Self {
        Self {
            attempt_id: attempt_id.into(),
            route,
            state: AttemptState::Completed,
            deficits: Vec::new(),
            counters,
            elapsed_micros: 0,
        }
    }

    fn validate(&self) -> Result<(), ContextRouteError> {
        let deficit_shape_valid = match self.state {
            AttemptState::Completed => self.deficits.is_empty(),
            AttemptState::Partial | AttemptState::Blocked => !self.deficits.is_empty(),
            AttemptState::Interrupted => false,
        };
        if !valid_opaque_id(&self.attempt_id)
            || self.counters.validate().is_err()
            || !deficit_shape_valid
            || self.deficits.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(ContextRouteError::InvalidExecution);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LightOperationResult {
    Query {
        operation_version: String,
        result_digest: String,
        record_count: u64,
    },
    SourceReference {
        operation_version: String,
        result_digest: String,
        reference_count: u64,
    },
}

impl LightOperationResult {
    fn operation(&self) -> LightOperation {
        match self {
            Self::Query { .. } => LightOperation::Query,
            Self::SourceReference { .. } => LightOperation::SourceReference,
        }
    }

    fn validate(&self) -> bool {
        match self {
            Self::Query {
                operation_version,
                result_digest,
                ..
            } => operation_version == QUERY_OPERATION_VERSION && valid_digest(result_digest),
            Self::SourceReference {
                operation_version,
                result_digest,
                ..
            } => {
                operation_version == SOURCE_REFERENCE_OPERATION_VERSION
                    && valid_digest(result_digest)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepResultReference {
    pub context_ir_version: String,
    pub context_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "payload_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextPayload {
    DirectNone {},
    LightBundle {
        operations: Vec<LightOperationResult>,
    },
    DeepContextIr {
        result: DeepResultReference,
    },
}

impl ContextPayload {
    pub fn validate(&self) -> Result<(), ContextRouteError> {
        match self {
            Self::DirectNone {} => Ok(()),
            Self::LightBundle { operations } => {
                if operations.is_empty()
                    || operations.len() > 2
                    || operations.iter().any(|operation| !operation.validate())
                    || operations
                        .windows(2)
                        .any(|pair| pair[0].operation() >= pair[1].operation())
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                Ok(())
            }
            Self::DeepContextIr { result } => {
                if result.context_ir_version != CONTEXT_IR_VERSION
                    || !valid_digest(&result.context_hash)
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                Ok(())
            }
        }
    }

    fn route(&self) -> Route {
        match self {
            Self::DirectNone {} => Route::Direct,
            Self::LightBundle { .. } => Route::AtlasLight,
            Self::DeepContextIr { .. } => Route::AtlasDeep,
        }
    }

    fn useful(&self) -> bool {
        !matches!(self, Self::DirectNone {})
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDiagnosticCode {
    ServingFallback,
    EvidenceOmitted,
    MaterializationOmitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializedSource {
    pub source_digest: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitMaterialization {
    pub authorized: bool,
    pub max_bytes: u64,
    pub sources: Vec<MaterializedSource>,
}

impl ExplicitMaterialization {
    fn validate(&self) -> Result<(), ContextRouteError> {
        if !self.authorized
            || self.max_bytes == 0
            || self.max_bytes > MAX_CONTEXT_EXECUTION_SOURCE_BYTES
            || self.sources.len() > MAX_MATERIALIZED_SOURCES
        {
            return Err(ContextRouteError::InvalidExecution);
        }
        let mut total = 0_u64;
        for source in &self.sources {
            let byte_len = u64::try_from(source.bytes.len())
                .map_err(|_| ContextRouteError::InvalidExecution)?;
            if !valid_digest(&source.source_digest)
                || source.start_byte >= source.end_byte
                || source.end_byte - source.start_byte != byte_len
            {
                return Err(ContextRouteError::InvalidExecution);
            }
            total = total
                .checked_add(byte_len)
                .ok_or(ContextRouteError::InvalidExecution)?;
        }
        if total > self.max_bytes {
            return Err(ContextRouteError::InvalidExecution);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    RouteCeilingReached,
    CapabilityUnavailable,
    GenerationUnavailable,
    GenerationChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionReason {
    Cancelled,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextExecution {
    Completed {
        schema_version: String,
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        final_route: Route,
        payload: ContextPayload,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
    },
    Partial {
        schema_version: String,
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        final_route: Route,
        payload: ContextPayload,
        satisfied_capabilities: Vec<RequiredCapability>,
        deficits: Vec<RouteDeficit>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        terminal_reason: TerminalReason,
    },
    Blocked {
        schema_version: String,
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        final_route: Option<Route>,
        payload: Option<ContextPayload>,
        satisfied_capabilities: Vec<RequiredCapability>,
        deficits: Vec<RouteDeficit>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        terminal_reason: TerminalReason,
    },
    Interrupted {
        schema_version: String,
        decision: ContextRouteDecision,
        completed_attempts: Vec<ExecutionAttempt>,
        prior_payload: Option<ContextPayload>,
        satisfied_capabilities: Vec<RequiredCapability>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        interruption_reason: InterruptionReason,
    },
}

fn checked_counter_sum(
    attempts: &[ExecutionAttempt],
) -> Result<ContextExecutionCounters, ContextRouteError> {
    attempts
        .iter()
        .try_fold(ContextExecutionCounters::default(), |mut total, attempt| {
            total.atlas_calls = total
                .atlas_calls
                .checked_add(attempt.counters.atlas_calls)
                .ok_or(ContextRouteError::InvalidExecution)?;
            total.records = total
                .records
                .checked_add(attempt.counters.records)
                .ok_or(ContextRouteError::InvalidExecution)?;
            total.source_bytes = total
                .source_bytes
                .checked_add(attempt.counters.source_bytes)
                .ok_or(ContextRouteError::InvalidExecution)?;
            total.estimated_tokens = total
                .estimated_tokens
                .checked_add(attempt.counters.estimated_tokens)
                .ok_or(ContextRouteError::InvalidExecution)?;
            total.work_units = total
                .work_units
                .checked_add(attempt.counters.work_units)
                .ok_or(ContextRouteError::InvalidExecution)?;
            Ok(total)
        })
}

fn validate_satisfied_capabilities(
    decision: &ContextRouteDecision,
    satisfied: &[RequiredCapability],
    deficits: &[RouteDeficit],
    satisfaction_route: Option<Route>,
    require_useful: bool,
    require_accounting: bool,
) -> Result<(), ContextRouteError> {
    let satisfied_set: BTreeSet<_> = satisfied.iter().copied().collect();
    if satisfied_set.len() != satisfied.len()
        || (require_useful && satisfied_set.is_empty())
        || satisfied_set.iter().any(|capability| {
            let Some(state) = decision.requested_capabilities.get(capability) else {
                return true;
            };
            *state != CallerCapabilityState::Satisfied
                && satisfaction_route.is_none_or(|route| route < capability.minimum_route())
        })
    {
        return Err(ContextRouteError::InvalidExecution);
    }
    let mut deficit_capabilities = BTreeSet::new();
    for deficit in deficits {
        if let RouteDeficit::Capability { capability, .. } = deficit {
            if !decision.requested_capabilities.contains_key(capability)
                || !deficit_capabilities.insert(*capability)
                || satisfied_set.contains(capability)
            {
                return Err(ContextRouteError::InvalidExecution);
            }
        }
    }
    if require_accounting
        && satisfied_set.len() + deficit_capabilities.len() != decision.requested_capabilities.len()
    {
        return Err(ContextRouteError::InvalidExecution);
    }
    Ok(())
}

fn validate_terminal_reason(
    reason: TerminalReason,
    final_route: Option<Route>,
    ceiling: Route,
    deficits: &[RouteDeficit],
) -> Result<(), ContextRouteError> {
    let matches = match reason {
        TerminalReason::RouteCeilingReached => final_route == Some(ceiling),
        TerminalReason::CapabilityUnavailable => deficits.iter().any(|deficit| {
            matches!(
                deficit,
                RouteDeficit::Capability {
                    reason: CapabilityDeficitReason::Unavailable
                        | CapabilityDeficitReason::Stale
                        | CapabilityDeficitReason::Unsupported,
                    ..
                }
            )
        }),
        TerminalReason::GenerationUnavailable => deficits.iter().any(|deficit| {
            matches!(
                deficit,
                RouteDeficit::Route {
                    reason: RouteFailureReason::StartingGenerationUnavailable
                        | RouteFailureReason::NoActiveGeneration
                }
            )
        }),
        TerminalReason::GenerationChanged => deficits.iter().any(|deficit| {
            matches!(
                deficit,
                RouteDeficit::Route {
                    reason: RouteFailureReason::GenerationChanged
                }
            )
        }),
    };
    if matches {
        Ok(())
    } else {
        Err(ContextRouteError::InvalidExecution)
    }
}

fn validate_materialization(
    materialization: Option<&ExplicitMaterialization>,
    payload: Option<&ContextPayload>,
) -> Result<(), ContextRouteError> {
    if let Some(materialization) = materialization {
        materialization.validate()?;
        if payload.is_none_or(|payload| payload.route() == Route::Direct) {
            return Err(ContextRouteError::InvalidExecution);
        }
    }
    Ok(())
}

fn valid_attempt_transition(
    previous: &ExecutionAttempt,
    next: &ExecutionAttempt,
    decision: &ContextRouteDecision,
) -> bool {
    if previous.route == next.route {
        return previous.state == AttemptState::Partial;
    }
    if previous.state != AttemptState::Partial {
        return false;
    }
    match (previous.route, next.route) {
        (Route::Direct, Route::AtlasLight) => {
            !previous.deficits.is_empty()
                && previous.deficits.iter().all(|deficit| match deficit {
                    RouteDeficit::Capability { capability, reason } => {
                        matches!(
                            decision.requested_capabilities.get(capability),
                            Some(
                                CallerCapabilityState::Unsatisfied | CallerCapabilityState::Unknown
                            )
                        ) && *reason != CapabilityDeficitReason::Unsupported
                    }
                    RouteDeficit::Route { .. } => false,
                })
        }
        (Route::AtlasLight, Route::AtlasDeep) => {
            !previous.deficits.is_empty()
                && previous.deficits.iter().all(|deficit| match deficit {
                    RouteDeficit::Capability { capability, reason } => {
                        if !matches!(
                            decision.requested_capabilities.get(capability),
                            Some(
                                CallerCapabilityState::Unsatisfied | CallerCapabilityState::Unknown
                            )
                        ) || *reason == CapabilityDeficitReason::Unsupported
                        {
                            return false;
                        }
                        capability.minimum_route() == Route::AtlasDeep
                            || *reason == CapabilityDeficitReason::SemanticBudgetOmitted
                            || (*capability == RequiredCapability::IdentityLookup
                                && *reason == CapabilityDeficitReason::Ambiguous)
                            || (matches!(
                                capability,
                                RequiredCapability::BoundedRelationships
                                    | RequiredCapability::ImpactFrontier
                                    | RequiredCapability::BasicCoverageConflicts
                            ) && *reason == CapabilityDeficitReason::Conflicting)
                    }
                    RouteDeficit::Route { .. } => false,
                })
        }
        _ => false,
    }
}

impl ContextExecution {
    pub fn completed(
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        payload: ContextPayload,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
    ) -> Result<Self, ContextRouteError> {
        let final_route = attempts
            .last()
            .map_or(decision.initial_route, |attempt| attempt.route);
        let value = Self::Completed {
            schema_version: CONTEXT_EXECUTION_VERSION.to_string(),
            decision,
            attempts,
            final_route,
            payload,
            counters,
            diagnostics,
            materialization,
        };
        value.validate()?;
        Ok(value)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn partial(
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        final_route: Route,
        payload: ContextPayload,
        satisfied_capabilities: Vec<RequiredCapability>,
        deficits: Vec<RouteDeficit>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        terminal_reason: TerminalReason,
    ) -> Result<Self, ContextRouteError> {
        let value = Self::Partial {
            schema_version: CONTEXT_EXECUTION_VERSION.to_string(),
            decision,
            attempts,
            final_route,
            payload,
            satisfied_capabilities,
            deficits,
            counters,
            diagnostics,
            materialization,
            terminal_reason,
        };
        value.validate()?;
        Ok(value)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blocked(
        decision: ContextRouteDecision,
        attempts: Vec<ExecutionAttempt>,
        final_route: Option<Route>,
        payload: Option<ContextPayload>,
        satisfied_capabilities: Vec<RequiredCapability>,
        deficits: Vec<RouteDeficit>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        terminal_reason: TerminalReason,
    ) -> Result<Self, ContextRouteError> {
        let value = Self::Blocked {
            schema_version: CONTEXT_EXECUTION_VERSION.to_string(),
            decision,
            attempts,
            final_route,
            payload,
            satisfied_capabilities,
            deficits,
            counters,
            diagnostics,
            materialization,
            terminal_reason,
        };
        value.validate()?;
        Ok(value)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn interrupted(
        decision: ContextRouteDecision,
        completed_attempts: Vec<ExecutionAttempt>,
        prior_payload: Option<ContextPayload>,
        satisfied_capabilities: Vec<RequiredCapability>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
        interruption_reason: InterruptionReason,
    ) -> Result<Self, ContextRouteError> {
        let value = Self::Interrupted {
            schema_version: CONTEXT_EXECUTION_VERSION.to_string(),
            decision,
            completed_attempts,
            prior_payload,
            satisfied_capabilities,
            counters,
            diagnostics,
            materialization,
            interruption_reason,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn state(&self) -> ExecutionState {
        match self {
            Self::Completed { .. } => ExecutionState::Completed,
            Self::Partial { .. } => ExecutionState::Partial,
            Self::Blocked { .. } => ExecutionState::Blocked,
            Self::Interrupted { .. } => ExecutionState::Interrupted,
        }
    }

    pub fn payload(&self) -> Option<&ContextPayload> {
        match self {
            Self::Completed { payload, .. } | Self::Partial { payload, .. } => Some(payload),
            Self::Blocked { payload, .. } => payload.as_ref(),
            Self::Interrupted { prior_payload, .. } => prior_payload.as_ref(),
        }
    }

    pub fn validate(&self) -> Result<(), ContextRouteError> {
        let (schema_version, decision, attempts, counters, diagnostics) = match self {
            Self::Completed {
                schema_version,
                decision,
                attempts,
                counters,
                diagnostics,
                ..
            }
            | Self::Partial {
                schema_version,
                decision,
                attempts,
                counters,
                diagnostics,
                ..
            }
            | Self::Blocked {
                schema_version,
                decision,
                attempts,
                counters,
                diagnostics,
                ..
            } => (schema_version, decision, attempts, counters, diagnostics),
            Self::Interrupted {
                schema_version,
                decision,
                completed_attempts,
                counters,
                diagnostics,
                ..
            } => (
                schema_version,
                decision,
                completed_attempts,
                counters,
                diagnostics,
            ),
        };
        if schema_version != CONTEXT_EXECUTION_VERSION {
            return Err(ContextRouteError::UnsupportedVersion {
                contract: "execution".to_string(),
                supplied: schema_version.clone(),
            });
        }
        decision.validate()?;
        if attempts.len() > MAX_ROUTE_ATTEMPTS
            || diagnostics.len() > MAX_EXECUTION_DIAGNOSTICS
            || counters.validate().is_err()
            || attempts.iter().any(|attempt| attempt.validate().is_err())
            || attempts
                .windows(2)
                .any(|pair| !valid_attempt_transition(&pair[0], &pair[1], decision))
            || attempts
                .first()
                .is_some_and(|attempt| attempt.route != decision.initial_route)
            || attempts.iter().any(|attempt| {
                attempt.route < decision.effective_floor
                    || attempt.route > decision.effective_ceiling
                    || (attempt.route == Route::Direct
                        && attempt.counters != ContextExecutionCounters::default())
            })
            || (matches!(
                decision.starting_generation,
                GenerationObservation::Unavailable { .. }
            ) && attempts
                .iter()
                .any(|attempt| attempt.route != Route::Direct))
        {
            return Err(ContextRouteError::InvalidExecution);
        }
        let summed_counters = checked_counter_sum(attempts)?;
        let caller_satisfied = decision
            .requested_capabilities
            .values()
            .all(|state| *state == CallerCapabilityState::Satisfied);
        if caller_satisfied
            && decision.initial_route == Route::Direct
            && attempts
                .iter()
                .any(|attempt| attempt.route != Route::Direct)
        {
            return Err(ContextRouteError::InvalidExecution);
        }

        match self {
            Self::Completed {
                attempts,
                final_route,
                payload,
                counters,
                materialization,
                ..
            } => {
                payload.validate()?;
                if attempts.is_empty()
                    || attempts
                        .last()
                        .map(|attempt| (attempt.route, attempt.state))
                        != Some((*final_route, AttemptState::Completed))
                    || payload.route() != *final_route
                    || counters != &summed_counters
                    || decision
                        .requested_capabilities
                        .iter()
                        .any(|(capability, state)| {
                            *state != CallerCapabilityState::Satisfied
                                && *final_route < capability.minimum_route()
                        })
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                validate_materialization(materialization.as_ref(), Some(payload))?;
            }
            Self::Partial {
                attempts,
                final_route,
                payload,
                satisfied_capabilities,
                deficits,
                counters,
                materialization,
                terminal_reason,
                ..
            } => {
                payload.validate()?;
                if attempts.is_empty()
                    || attempts
                        .last()
                        .map(|attempt| (attempt.route, attempt.state))
                        != Some((*final_route, AttemptState::Partial))
                    || payload.route() != *final_route
                    || !payload.useful()
                    || deficits
                        .iter()
                        .any(|deficit| matches!(deficit, RouteDeficit::Route { .. }))
                    || attempts.last().map(|attempt| &attempt.deficits) != Some(deficits)
                    || counters != &summed_counters
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                validate_satisfied_capabilities(
                    decision,
                    satisfied_capabilities,
                    deficits,
                    Some(*final_route),
                    true,
                    true,
                )?;
                validate_terminal_reason(
                    *terminal_reason,
                    Some(*final_route),
                    decision.effective_ceiling,
                    deficits,
                )?;
                validate_materialization(materialization.as_ref(), Some(payload))?;
            }
            Self::Blocked {
                attempts,
                final_route,
                payload,
                satisfied_capabilities,
                deficits,
                counters,
                materialization,
                terminal_reason,
                ..
            } => {
                if deficits.is_empty()
                    || final_route.is_some() == attempts.is_empty()
                    || final_route.is_some_and(|route| {
                        attempts
                            .last()
                            .map(|attempt| (attempt.route, attempt.state))
                            != Some((route, AttemptState::Blocked))
                    })
                    || attempts
                        .last()
                        .is_some_and(|attempt| attempt.deficits != *deficits)
                    || counters != &summed_counters
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                validate_satisfied_capabilities(
                    decision,
                    satisfied_capabilities,
                    deficits,
                    *final_route,
                    payload.is_some(),
                    !deficits
                        .iter()
                        .any(|deficit| matches!(deficit, RouteDeficit::Route { .. })),
                )?;
                if let Some(payload) = payload {
                    payload.validate()?;
                    if !payload.useful() || final_route.is_none_or(|route| payload.route() != route)
                    {
                        return Err(ContextRouteError::InvalidExecution);
                    }
                }
                validate_terminal_reason(
                    *terminal_reason,
                    *final_route,
                    decision.effective_ceiling,
                    deficits,
                )?;
                validate_materialization(materialization.as_ref(), payload.as_ref())?;
            }
            Self::Interrupted {
                completed_attempts,
                prior_payload,
                satisfied_capabilities,
                counters,
                materialization,
                ..
            } => {
                if completed_attempts.iter().any(|attempt| {
                    matches!(
                        attempt.state,
                        AttemptState::Blocked | AttemptState::Interrupted
                    )
                }) || counters != &summed_counters
                {
                    return Err(ContextRouteError::InvalidExecution);
                }
                validate_satisfied_capabilities(
                    decision,
                    satisfied_capabilities,
                    &[],
                    completed_attempts.last().map(|attempt| attempt.route),
                    prior_payload.as_ref().is_some_and(ContextPayload::useful),
                    false,
                )?;
                if let Some(payload) = prior_payload {
                    payload.validate()?;
                    if completed_attempts
                        .last()
                        .is_none_or(|attempt| payload.route() != attempt.route)
                    {
                        return Err(ContextRouteError::InvalidExecution);
                    }
                }
                validate_materialization(materialization.as_ref(), prior_payload.as_ref())?;
            }
        }
        Ok(())
    }
}

/// One bounded Atlas route result returned to the progressive application
/// dispatcher. Route selection and terminal-state policy remain owned by this
/// module; adapters only execute the version-pinned operation contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAttemptOutcome {
    Completed {
        payload: ContextPayload,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
    },
    Deficient {
        payload: Option<ContextPayload>,
        satisfied_capabilities: Vec<RequiredCapability>,
        deficits: Vec<RouteDeficit>,
        counters: ContextExecutionCounters,
        diagnostics: Vec<ContextDiagnosticCode>,
        materialization: Option<ExplicitMaterialization>,
    },
    Interrupted {
        diagnostics: Vec<ContextDiagnosticCode>,
        interruption_reason: InterruptionReason,
    },
}

/// Transport-neutral execution seam for application-owned LIGHT and DEEP
/// operations. Implementations receive only the frozen decision and pinned
/// contracts; routing policy never enters CLI, MCP, provider, or skill code.
pub trait ContextRouteDispatcher {
    type Error: From<ContextRouteError>;

    fn observe_generation(&mut self) -> GenerationObservation;

    fn dispatch_light(
        &mut self,
        decision: &ContextRouteDecision,
        operations: &[VersionedOperation],
    ) -> Result<RouteAttemptOutcome, Self::Error>;

    fn dispatch_deep(
        &mut self,
        decision: &ContextRouteDecision,
        contracts: &DeepContractVersions,
        budget: &DeepSemanticBudget,
    ) -> Result<RouteAttemptOutcome, Self::Error>;
}

fn caller_satisfaction(
    decision: &ContextRouteDecision,
) -> (Vec<RequiredCapability>, Vec<RouteDeficit>) {
    let mut satisfied = Vec::new();
    let mut deficits = Vec::new();
    for (capability, state) in &decision.requested_capabilities {
        match state {
            CallerCapabilityState::Satisfied => satisfied.push(*capability),
            CallerCapabilityState::Unsatisfied | CallerCapabilityState::Unknown => {
                deficits.push(RouteDeficit::Capability {
                    capability: *capability,
                    reason: CapabilityDeficitReason::Unavailable,
                });
            }
        }
    }
    (satisfied, deficits)
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn elapsed_micros(started: Instant) -> u64 {
    duration_micros(started.elapsed()).max(1)
}

fn route_attempt(
    ordinal: usize,
    route: Route,
    state: AttemptState,
    deficits: Vec<RouteDeficit>,
    counters: ContextExecutionCounters,
    started: Instant,
) -> ExecutionAttempt {
    ExecutionAttempt {
        attempt_id: format!("context-attempt-{ordinal}"),
        route,
        state,
        deficits,
        counters,
        elapsed_micros: elapsed_micros(started),
    }
}

fn generation_failure(
    expected: &GenerationObservation,
    observed: GenerationObservation,
) -> Option<(RouteFailureReason, TerminalReason)> {
    match (expected, observed) {
        (
            GenerationObservation::Observed {
                generation_id: expected,
            },
            GenerationObservation::Observed {
                generation_id: observed,
            },
        ) if expected == &observed => None,
        (GenerationObservation::Observed { .. }, GenerationObservation::Observed { .. }) => Some((
            RouteFailureReason::GenerationChanged,
            TerminalReason::GenerationChanged,
        )),
        (
            GenerationObservation::Observed { .. },
            GenerationObservation::Unavailable {
                reason: GenerationUnavailableReason::NoActiveGeneration,
            },
        ) => Some((
            RouteFailureReason::NoActiveGeneration,
            TerminalReason::GenerationUnavailable,
        )),
        (GenerationObservation::Observed { .. }, GenerationObservation::Unavailable { .. })
        | (GenerationObservation::Unavailable { .. }, _) => Some((
            RouteFailureReason::StartingGenerationUnavailable,
            TerminalReason::GenerationUnavailable,
        )),
    }
}

fn light_deficits_can_escalate(deficits: &[RouteDeficit]) -> bool {
    !deficits.is_empty()
        && deficits.iter().all(|deficit| match deficit {
            RouteDeficit::Route { .. } => false,
            RouteDeficit::Capability { capability, reason } => {
                *reason != CapabilityDeficitReason::Unsupported
                    && (capability.minimum_route() == Route::AtlasDeep
                        || *reason == CapabilityDeficitReason::SemanticBudgetOmitted
                        || (*capability == RequiredCapability::IdentityLookup
                            && *reason == CapabilityDeficitReason::Ambiguous)
                        || (matches!(
                            capability,
                            RequiredCapability::BoundedRelationships
                                | RequiredCapability::ImpactFrontier
                                | RequiredCapability::BasicCoverageConflicts
                        ) && *reason == CapabilityDeficitReason::Conflicting))
            }
        })
}

fn terminal_reason_for(route: Route, ceiling: Route, deficits: &[RouteDeficit]) -> TerminalReason {
    if route == ceiling {
        TerminalReason::RouteCeilingReached
    } else if deficits.iter().any(|deficit| {
        matches!(
            deficit,
            RouteDeficit::Capability {
                reason: CapabilityDeficitReason::Unavailable
                    | CapabilityDeficitReason::Stale
                    | CapabilityDeficitReason::Unsupported,
                ..
            }
        )
    }) {
        TerminalReason::CapabilityUnavailable
    } else {
        TerminalReason::RouteCeilingReached
    }
}

fn dispatch_outcome_valid(
    decision: &ContextRouteDecision,
    route: Route,
    outcome: &RouteAttemptOutcome,
) -> Result<(), ContextRouteError> {
    match outcome {
        RouteAttemptOutcome::Completed {
            payload,
            counters,
            diagnostics,
            materialization,
        } => {
            if diagnostics.len() > MAX_EXECUTION_DIAGNOSTICS
                || counters.validate().is_err()
                || payload.validate().is_err()
                || payload.route() != route
            {
                return Err(ContextRouteError::InvalidExecution);
            }
            validate_materialization(materialization.as_ref(), Some(payload))
        }
        RouteAttemptOutcome::Deficient {
            payload,
            satisfied_capabilities,
            deficits,
            counters,
            diagnostics,
            materialization,
        } => {
            if diagnostics.len() > MAX_EXECUTION_DIAGNOSTICS
                || counters.validate().is_err()
                || payload
                    .as_ref()
                    .is_some_and(|payload| payload.validate().is_err() || payload.route() != route)
                || deficits.is_empty()
                || deficits.windows(2).any(|pair| pair[0] >= pair[1])
                || deficits
                    .iter()
                    .any(|deficit| matches!(deficit, RouteDeficit::Route { .. }))
            {
                return Err(ContextRouteError::InvalidExecution);
            }
            validate_materialization(materialization.as_ref(), payload.as_ref())?;
            validate_satisfied_capabilities(
                decision,
                satisfied_capabilities,
                deficits,
                Some(route),
                false,
                true,
            )
        }
        RouteAttemptOutcome::Interrupted { diagnostics, .. } => {
            if diagnostics.len() > MAX_EXECUTION_DIAGNOSTICS {
                return Err(ContextRouteError::InvalidExecution);
            }
            Ok(())
        }
    }
}

fn summed_counters<E: From<ContextRouteError>>(
    attempts: &[ExecutionAttempt],
) -> Result<ContextExecutionCounters, E> {
    checked_counter_sum(attempts).map_err(E::from)
}

/// Execute one immutable route decision through bounded DIRECT → LIGHT → DEEP
/// progression. Generation changes stop before the next Atlas operation; the
/// caller must submit a new request to obtain a new immutable decision.
pub fn execute_context_route<D: ContextRouteDispatcher>(
    decision: ContextRouteDecision,
    dispatcher: &mut D,
) -> Result<ContextExecution, D::Error> {
    decision.validate().map_err(D::Error::from)?;
    let direct_attempt_started = Instant::now();
    let (caller_satisfied, caller_deficits) = caller_satisfaction(&decision);
    let caller_context_sufficient = caller_deficits.is_empty();

    if matches!(
        decision.starting_generation,
        GenerationObservation::Unavailable { .. }
    ) && !(decision.initial_route == Route::Direct && caller_context_sufficient)
    {
        return ContextExecution::blocked(
            decision,
            Vec::new(),
            None,
            None,
            caller_satisfied,
            vec![RouteDeficit::Route {
                reason: RouteFailureReason::StartingGenerationUnavailable,
            }],
            ContextExecutionCounters::default(),
            Vec::new(),
            None,
            TerminalReason::GenerationUnavailable,
        )
        .map_err(D::Error::from);
    }

    let mut attempts = Vec::with_capacity(MAX_ROUTE_ATTEMPTS);
    let mut diagnostics = Vec::new();
    let mut route = decision.initial_route;
    let mut prior_payload = None;
    let mut prior_satisfied = caller_satisfied.clone();
    let mut prior_materialization = None;

    if route == Route::Direct {
        if caller_context_sufficient {
            attempts.push(route_attempt(
                1,
                Route::Direct,
                AttemptState::Completed,
                Vec::new(),
                ContextExecutionCounters::default(),
                direct_attempt_started,
            ));
            return ContextExecution::completed(
                decision,
                attempts,
                ContextPayload::DirectNone {},
                ContextExecutionCounters::default(),
                diagnostics,
                None,
            )
            .map_err(D::Error::from);
        }
        if decision.effective_ceiling == Route::Direct {
            attempts.push(route_attempt(
                1,
                Route::Direct,
                AttemptState::Blocked,
                caller_deficits.clone(),
                ContextExecutionCounters::default(),
                direct_attempt_started,
            ));
            return ContextExecution::blocked(
                decision,
                attempts,
                Some(Route::Direct),
                None,
                caller_satisfied,
                caller_deficits,
                ContextExecutionCounters::default(),
                diagnostics,
                None,
                TerminalReason::RouteCeilingReached,
            )
            .map_err(D::Error::from);
        }
        attempts.push(route_attempt(
            1,
            Route::Direct,
            AttemptState::Partial,
            caller_deficits,
            ContextExecutionCounters::default(),
            direct_attempt_started,
        ));
        route = Route::AtlasLight;
    }

    loop {
        let attempt_started = Instant::now();
        if let Some((failure, terminal_reason)) = generation_failure(
            &decision.starting_generation,
            dispatcher.observe_generation(),
        ) {
            let deficits = vec![RouteDeficit::Route { reason: failure }];
            attempts.push(route_attempt(
                attempts.len() + 1,
                route,
                AttemptState::Blocked,
                deficits.clone(),
                ContextExecutionCounters::default(),
                attempt_started,
            ));
            let counters = summed_counters::<D::Error>(&attempts)?;
            return ContextExecution::blocked(
                decision,
                attempts,
                Some(route),
                None,
                caller_satisfied,
                deficits,
                counters,
                diagnostics,
                None,
                terminal_reason,
            )
            .map_err(D::Error::from);
        }

        let outcome = match route {
            Route::Direct => unreachable!("DIRECT is handled before Atlas dispatch"),
            Route::AtlasLight => {
                let operations = decision.request.light_operations.clone();
                dispatcher.dispatch_light(&decision, &operations)?
            }
            Route::AtlasDeep => {
                let contracts = decision.request.deep_contracts.clone();
                let budget = decision
                    .request
                    .deep_budget
                    .clone()
                    .ok_or(ContextRouteError::DeepBudgetRequired)
                    .map_err(D::Error::from)?;
                dispatcher.dispatch_deep(&decision, &contracts, &budget)?
            }
        };
        dispatch_outcome_valid(&decision, route, &outcome).map_err(D::Error::from)?;

        match outcome {
            RouteAttemptOutcome::Completed {
                payload,
                counters,
                diagnostics: attempt_diagnostics,
                materialization,
            } => {
                diagnostics.extend(attempt_diagnostics);
                attempts.push(route_attempt(
                    attempts.len() + 1,
                    route,
                    AttemptState::Completed,
                    Vec::new(),
                    counters,
                    attempt_started,
                ));
                let counters = summed_counters::<D::Error>(&attempts)?;
                return ContextExecution::completed(
                    decision,
                    attempts,
                    payload,
                    counters,
                    diagnostics,
                    materialization,
                )
                .map_err(D::Error::from);
            }
            RouteAttemptOutcome::Interrupted {
                diagnostics: attempt_diagnostics,
                interruption_reason,
            } => {
                diagnostics.extend(attempt_diagnostics);
                let counters = summed_counters::<D::Error>(&attempts)?;
                return ContextExecution::interrupted(
                    decision,
                    attempts,
                    prior_payload,
                    prior_satisfied,
                    counters,
                    diagnostics,
                    prior_materialization,
                    interruption_reason,
                )
                .map_err(D::Error::from);
            }
            RouteAttemptOutcome::Deficient {
                payload,
                satisfied_capabilities,
                deficits,
                counters,
                diagnostics: attempt_diagnostics,
                materialization,
            } => {
                diagnostics.extend(attempt_diagnostics);
                let can_escalate = route == Route::AtlasLight
                    && route < decision.effective_ceiling
                    && light_deficits_can_escalate(&deficits);
                if can_escalate {
                    attempts.push(route_attempt(
                        attempts.len() + 1,
                        route,
                        AttemptState::Partial,
                        deficits,
                        counters,
                        attempt_started,
                    ));
                    if payload.is_some() && !satisfied_capabilities.is_empty() {
                        prior_payload = payload;
                        prior_satisfied = satisfied_capabilities;
                        prior_materialization = materialization;
                    }
                    route = Route::AtlasDeep;
                    continue;
                }

                let useful = payload.as_ref().is_some_and(ContextPayload::useful)
                    && !satisfied_capabilities.is_empty();
                let state = if useful {
                    AttemptState::Partial
                } else {
                    AttemptState::Blocked
                };
                attempts.push(route_attempt(
                    attempts.len() + 1,
                    route,
                    state,
                    deficits.clone(),
                    counters,
                    attempt_started,
                ));
                let counters = summed_counters::<D::Error>(&attempts)?;
                let terminal_reason =
                    terminal_reason_for(route, decision.effective_ceiling, &deficits);
                return if useful {
                    ContextExecution::partial(
                        decision,
                        attempts,
                        route,
                        payload.expect("useful payload checked above"),
                        satisfied_capabilities,
                        deficits,
                        counters,
                        diagnostics,
                        materialization,
                        terminal_reason,
                    )
                    .map_err(D::Error::from)
                } else {
                    ContextExecution::blocked(
                        decision,
                        attempts,
                        Some(route),
                        None,
                        satisfied_capabilities,
                        deficits,
                        counters,
                        diagnostics,
                        None,
                        terminal_reason,
                    )
                    .map_err(D::Error::from)
                };
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFeature {
    RouteDecision,
    LightPayload,
    ProgressiveExecution,
    DeepContextIr,
    Materialization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCapabilityBounds {
    pub max_required_capabilities: usize,
    pub max_route_attempts: usize,
    pub max_execution_diagnostics: usize,
    pub max_materialized_sources: usize,
    pub max_materialized_bytes: u64,
    pub max_request_bytes: usize,
    pub default_page_limit: usize,
    pub max_page_limit: usize,
    pub max_cursor_bytes: usize,
    pub max_target_bytes: usize,
    pub max_targets: usize,
    pub max_atlas_calls: u64,
    pub max_records: u64,
    pub max_source_bytes: u64,
    pub max_estimated_tokens: u64,
    pub max_depth: u32,
    pub max_work_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCapabilities {
    pub schema_version: String,
    pub supported_route_policy_versions: Vec<String>,
    pub supported_decision_versions: Vec<String>,
    pub supported_execution_versions: Vec<String>,
    pub supported_ir_versions: Vec<String>,
    pub supported_operation_versions: Vec<String>,
    pub routes: Vec<Route>,
    pub required_capabilities: Vec<RequiredCapability>,
    pub signals: Vec<RouteSignal>,
    pub reasons: Vec<RouteReason>,
    pub capability_deficits: Vec<CapabilityDeficitReason>,
    pub route_failures: Vec<RouteFailureReason>,
    pub terminal_states: Vec<ExecutionState>,
    pub bounds: ContextCapabilityBounds,
    pub availability: BTreeMap<CapabilityFeature, Availability>,
}

impl ContextCapabilities {
    pub fn validate(&self) -> Result<(), ContextRouteError> {
        if self != &discover_context_capabilities() {
            return Err(ContextRouteError::UnsupportedVersion {
                contract: "capabilities".to_string(),
                supplied: self.schema_version.clone(),
            });
        }
        Ok(())
    }
}

pub fn discover_context_capabilities() -> ContextCapabilities {
    ContextCapabilities {
        schema_version: CONTEXT_CAPABILITIES_VERSION.to_string(),
        supported_route_policy_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        supported_decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        supported_execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
        supported_ir_versions: vec![CONTEXT_IR_VERSION.to_string()],
        supported_operation_versions: vec![
            QUERY_OPERATION_VERSION.to_string(),
            SOURCE_REFERENCE_OPERATION_VERSION.to_string(),
        ],
        routes: vec![Route::Direct, Route::AtlasLight, Route::AtlasDeep],
        required_capabilities: vec![
            RequiredCapability::IdentityLookup,
            RequiredCapability::ExactSource,
            RequiredCapability::BoundedRelationships,
            RequiredCapability::ImpactFrontier,
            RequiredCapability::BasicCoverageConflicts,
            RequiredCapability::TemporalHistory,
            RequiredCapability::RequiredRoleClosure,
            RequiredCapability::ValidationPlan,
        ],
        signals: vec![
            RouteSignal::CallerCapabilitiesSatisfied,
            RouteSignal::CallerCapabilitiesUnsatisfied,
            RouteSignal::CallerCapabilitiesUnknown,
            RouteSignal::AtlasIntentNone,
            RouteSignal::AtlasIntentAllow,
            RouteSignal::AtlasIntentRequire,
            RouteSignal::RouteFloorDirect,
            RouteSignal::RouteFloorLight,
            RouteSignal::RouteFloorDeep,
            RouteSignal::RouteCeilingDirect,
            RouteSignal::RouteCeilingLight,
            RouteSignal::RouteCeilingDeep,
            RouteSignal::ExplicitSeedsPresent,
            RouteSignal::IdentityOrSourceRequested,
            RouteSignal::RelationshipsOrImpactRequested,
            RouteSignal::BasicCoverageConflictsRequested,
            RouteSignal::DeepSynthesisRequested,
            RouteSignal::StartingGenerationObserved,
            RouteSignal::StartingGenerationUnavailable,
            RouteSignal::CapabilityProfileApplied,
            RouteSignal::CostProfileApplied,
        ],
        reasons: vec![
            RouteReason::CallerContextSufficient,
            RouteReason::CallerContextIncomplete,
            RouteReason::AtlasNotRequested,
            RouteReason::ExplicitAtlasRequest,
            RouteReason::LightCapabilityRequired,
            RouteReason::DeepCapabilityRequired,
            RouteReason::RouteFloorApplied,
            RouteReason::StartingGenerationUnavailable,
        ],
        capability_deficits: vec![
            CapabilityDeficitReason::Unavailable,
            CapabilityDeficitReason::Ambiguous,
            CapabilityDeficitReason::Conflicting,
            CapabilityDeficitReason::Stale,
            CapabilityDeficitReason::Unsupported,
            CapabilityDeficitReason::SemanticBudgetOmitted,
        ],
        route_failures: vec![
            RouteFailureReason::StartingGenerationUnavailable,
            RouteFailureReason::NoActiveGeneration,
            RouteFailureReason::GenerationChanged,
        ],
        terminal_states: vec![
            ExecutionState::Completed,
            ExecutionState::Partial,
            ExecutionState::Blocked,
            ExecutionState::Interrupted,
        ],
        bounds: ContextCapabilityBounds {
            max_required_capabilities: MAX_ROUTE_CAPABILITIES,
            max_route_attempts: MAX_ROUTE_ATTEMPTS,
            max_execution_diagnostics: MAX_EXECUTION_DIAGNOSTICS,
            max_materialized_sources: MAX_MATERIALIZED_SOURCES,
            max_materialized_bytes: MAX_CONTEXT_EXECUTION_SOURCE_BYTES,
            max_request_bytes: MAX_GOVERNOR_REQUEST_BYTES,
            default_page_limit: DEFAULT_APPLICATION_PAGE_LIMIT,
            max_page_limit: MAX_APPLICATION_PAGE_LIMIT,
            max_cursor_bytes: MAX_APPLICATION_CURSOR_BYTES,
            max_target_bytes: MAX_GOVERNOR_TARGET_BYTES,
            max_targets: MAX_GOVERNOR_TARGETS,
            max_atlas_calls: MAX_CONTEXT_EXECUTION_ATLAS_CALLS,
            max_records: MAX_CONTEXT_EXECUTION_RECORDS,
            max_source_bytes: MAX_CONTEXT_EXECUTION_SOURCE_BYTES,
            max_estimated_tokens: MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS,
            max_depth: MAX_DEEP_DEPTH,
            max_work_units: MAX_CONTEXT_EXECUTION_WORK_UNITS,
        },
        availability: BTreeMap::from([
            (CapabilityFeature::RouteDecision, Availability::Available),
            (CapabilityFeature::LightPayload, Availability::Available),
            (
                CapabilityFeature::ProgressiveExecution,
                Availability::Available,
            ),
            (CapabilityFeature::DeepContextIr, Availability::Available),
            (CapabilityFeature::Materialization, Availability::Available),
        ]),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiationRequest {
    pub capability_versions: Vec<String>,
    pub route_versions: Vec<String>,
    pub decision_versions: Vec<String>,
    pub execution_versions: Vec<String>,
    pub ir_versions: Vec<String>,
    pub operation_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiatedVersions {
    pub capability_version: String,
    pub route_version: String,
    pub decision_version: String,
    pub execution_version: String,
    pub ir_version: String,
    pub operation_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportedVersions {
    pub capability_versions: Vec<String>,
    pub route_versions: Vec<String>,
    pub decision_versions: Vec<String>,
    pub execution_versions: Vec<String>,
    pub ir_versions: Vec<String>,
    pub operation_versions: Vec<String>,
}

pub fn negotiate_context_versions(
    request: &NegotiationRequest,
) -> Result<NegotiatedVersions, ContextRouteError> {
    let operation_versions = vec![
        QUERY_OPERATION_VERSION.to_string(),
        SOURCE_REFERENCE_OPERATION_VERSION.to_string(),
    ];
    let supported = SupportedVersions {
        capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_string()],
        route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
        execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
        ir_versions: vec![CONTEXT_IR_VERSION.to_string()],
        operation_versions: operation_versions.clone(),
    };
    if !request
        .capability_versions
        .iter()
        .any(|version| version == CONTEXT_CAPABILITIES_VERSION)
        || !request
            .route_versions
            .iter()
            .any(|version| version == CONTEXT_ROUTE_POLICY_VERSION)
        || !request
            .decision_versions
            .iter()
            .any(|version| version == CONTEXT_ROUTE_POLICY_VERSION)
        || !request
            .execution_versions
            .iter()
            .any(|version| version == CONTEXT_EXECUTION_VERSION)
        || !request
            .ir_versions
            .iter()
            .any(|version| version == CONTEXT_IR_VERSION)
        || operation_versions.iter().any(|required| {
            !request
                .operation_versions
                .iter()
                .any(|offered| offered == required)
        })
    {
        return Err(ContextRouteError::NoCommonVersion {
            supported: Box::new(supported),
        });
    }
    Ok(NegotiatedVersions {
        capability_version: CONTEXT_CAPABILITIES_VERSION.to_string(),
        route_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        decision_version: CONTEXT_ROUTE_POLICY_VERSION.to_string(),
        execution_version: CONTEXT_EXECUTION_VERSION.to_string(),
        ir_version: CONTEXT_IR_VERSION.to_string(),
        operation_versions,
    })
}

#[cfg(test)]
mod tests {
    use super::duration_micros;
    use std::time::Duration;

    #[test]
    fn attempt_duration_conversion_saturates_at_the_wire_limit() {
        assert_eq!(duration_micros(Duration::MAX), u64::MAX);
    }
}
