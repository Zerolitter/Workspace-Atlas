//! Shared application boundary for progressive context execution.
//!
//! Owns context route decisions and catalogue-backed execution. Transport
//! adapters remain responsible only for validating and serializing requests.

use rusqlite::OptionalExtension;

use crate::error::{AtlasError, Result};
use crate::workspace;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernorRunRequest {
    pub supported_versions: crate::context_route::NegotiationRequest,
    pub semantic: GovernorRunSemanticInput,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernorRunSemanticInput {
    pub task: String,
    pub declared_kind: Option<crate::context_ir::TaskKind>,
    #[serde(default)]
    pub path_targets: Vec<String>,
    #[serde(default)]
    pub symbol_targets: Vec<String>,
    pub caller_capabilities: std::collections::BTreeMap<
        crate::context_route::RequiredCapability,
        crate::context_route::CallerCapabilityState,
    >,
    pub atlas_intent: crate::context_route::AtlasIntent,
    pub route_floor: crate::context_route::Route,
    pub route_ceiling: crate::context_route::Route,
    #[serde(default)]
    pub legacy_task_session_id: Option<String>,
    #[serde(default)]
    pub deep_limits: Option<GovernorDeepLimits>,
    #[serde(default)]
    pub materialize_source: bool,
    #[serde(default)]
    pub max_materialized_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernorDeepLimits {
    pub max_records: u64,
    pub max_source_bytes: u64,
    pub max_estimated_tokens: u64,
    pub max_relationship_depth: u32,
    pub max_work_units: u64,
    pub uncertainty_reserve_percent: u8,
}

#[derive(Debug, Clone)]
pub struct GovernorRunApplicationRequest {
    pub negotiated_versions: crate::context_route::NegotiatedVersions,
    pub route_request: crate::context_route::ContextRouteRequest,
    pub catalogue_input: CatalogueContextApplicationInput,
    pub max_materialized_bytes: Option<u64>,
}

/// Build the one transport-neutral H6B-A0 governor request.
///
/// Version negotiation is completed independently before semantic route
/// identity is derived. Callers provide task/target semantics, never hashes,
/// classifier output, generation observations, fixed contracts, or digests.
pub fn build_governor_run_request(
    request: GovernorRunRequest,
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
) -> std::result::Result<GovernorRunApplicationRequest, CatalogueContextApplicationError> {
    let starting_generation = catalogue_generation_observation(connection, workspace);
    let negotiated_versions =
        crate::context_route::negotiate_context_versions(&request.supported_versions)?;
    let semantic = request.semantic;
    if semantic.task.trim().is_empty()
        || semantic.task.len() > crate::context_route::MAX_GOVERNOR_REQUEST_BYTES
    {
        return Err(AtlasError::InvalidConfig(
            "governor task must contain 1..=1048576 bytes".into(),
        )
        .into());
    }
    let raw_target_count = semantic
        .path_targets
        .len()
        .checked_add(semantic.symbol_targets.len())
        .ok_or_else(|| AtlasError::InvalidConfig("governor target count overflow".into()))?;
    if raw_target_count > crate::context_route::MAX_GOVERNOR_TARGETS
        || semantic
            .path_targets
            .iter()
            .chain(&semantic.symbol_targets)
            .any(|target| {
                target.is_empty() || target.len() > crate::context_route::MAX_GOVERNOR_TARGET_BYTES
            })
    {
        return Err(AtlasError::InvalidConfig(
            "governor targets exceed the advertised application bounds".into(),
        )
        .into());
    }
    let path_targets = canonical_targets(&semantic.path_targets);
    let symbol_targets = canonical_targets(&semantic.symbol_targets);
    if semantic
        .legacy_task_session_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128 || !id.is_ascii())
    {
        return Err(AtlasError::InvalidConfig(
            "legacy task session id exceeds the application bound".into(),
        )
        .into());
    }
    match (semantic.materialize_source, semantic.max_materialized_bytes) {
        (false, None) => {}
        (true, Some(limit))
            if limit > 0 && limit <= crate::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES => {
        }
        _ => {
            return Err(AtlasError::InvalidConfig(
                "materialization authorization requires one positive advertised byte cap".into(),
            )
            .into());
        }
    }
    let deep_budget = match (semantic.route_ceiling, semantic.deep_limits) {
        (crate::context_route::Route::AtlasDeep, Some(limits)) => {
            Some(crate::context_route::DeepSemanticBudget::new(
                limits.max_records,
                limits.max_source_bytes,
                limits.max_estimated_tokens,
                limits.max_relationship_depth,
                limits.max_work_units,
                limits.uncertainty_reserve_percent,
            )?)
        }
        (crate::context_route::Route::AtlasDeep, None) => {
            return Err(crate::context_route::ContextRouteError::DeepBudgetRequired.into());
        }
        (_, Some(_)) => {
            return Err(AtlasError::InvalidConfig(
                "DEEP semantic limits require an ATLAS_DEEP ceiling".into(),
            )
            .into());
        }
        (_, None) => None,
    };
    let normalized_task_hash = crate::task_compiler::normalized_task_hash(&semantic.task);
    let (classifier_kind, _, _) =
        crate::task_compiler::classify_task(semantic.declared_kind, &semantic.task);
    let explicit_seeds =
        crate::context_route::SeedSummary::from_targets(&path_targets, &symbol_targets)?;
    let route_request = crate::context_route::ContextRouteRequest {
        schema_version: negotiated_versions.route_version.clone(),
        normalized_task_hash,
        declared_kind: semantic.declared_kind,
        classifier_kind,
        classifier_version: crate::context_route::TASK_CLASSIFIER_VERSION.to_string(),
        explicit_seeds,
        caller_capabilities: semantic.caller_capabilities,
        atlas_intent: semantic.atlas_intent,
        route_floor: semantic.route_floor,
        route_ceiling: semantic.route_ceiling,
        capability_profile: None,
        cost_profile: None,
        profile_registry_digest: None,
        starting_generation,
        light_operations: vec![
            crate::context_route::VersionedOperation::new(
                crate::context_route::LightOperation::Query,
            ),
            crate::context_route::VersionedOperation::new(
                crate::context_route::LightOperation::SourceReference,
            ),
        ],
        deep_contracts: crate::context_route::DeepContractVersions::fixed(),
        deep_budget,
    };
    crate::context_route::decide_context_route(route_request.clone())?;
    let catalogue_input = CatalogueContextApplicationInput {
        task: semantic.task,
        declared_task_kind: semantic.declared_kind,
        task_session_id: semantic.legacy_task_session_id,
        seed_paths: path_targets,
        seed_symbols: symbol_targets,
        baseline_generation_id: None,
    };
    Ok(GovernorRunApplicationRequest {
        negotiated_versions,
        route_request,
        catalogue_input,
        max_materialized_bytes: semantic.max_materialized_bytes,
    })
}

/// Shared internal application entrypoint for progressive context execution.
/// It owns decision/progression only; public CLI/MCP grammar remains unchanged
/// until H6B.
pub fn dispatch_context_application<D: crate::context_route::ContextRouteDispatcher>(
    request: crate::context_route::ContextRouteRequest,
    dispatcher: &mut D,
) -> std::result::Result<crate::context_route::ContextExecution, D::Error> {
    let decision = crate::context_route::decide_context_route(request).map_err(D::Error::from)?;
    crate::context_route::execute_context_route(decision, dispatcher)
}

/// Transient task and target input consumed by the internal catalogue-backed
/// progressive application path. It is deliberately not serializable.
#[derive(Debug, Clone)]
pub struct CatalogueContextApplicationInput {
    pub task: String,
    pub declared_task_kind: Option<crate::context_ir::TaskKind>,
    /// Optional caller-owned legacy lifecycle attribution. Unattributed DEEP
    /// execution uses a rollback-only response-lifetime compiler identity.
    pub task_session_id: Option<String>,
    pub seed_paths: Vec<String>,
    pub seed_symbols: Vec<String>,
    pub baseline_generation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightIdentityMatch {
    pub canonical_path: String,
    pub canonical_symbol_key: Option<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightRelationship {
    pub relationship_type: String,
    pub source_ref_kind: String,
    pub source_ref_value: String,
    pub target_ref_kind: String,
    pub target_ref_value: String,
    pub evidence_method: String,
    pub confidence_millionths: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightImpactTarget {
    pub source_ref_kind: String,
    pub source_ref_value: String,
    pub resolution_status: String,
    pub resolved_ref_kind: Option<String>,
    pub resolved_ref_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightCoverage {
    pub scope_kind: String,
    pub scope_key: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightConflict {
    pub subject_kind: String,
    pub subject_key: String,
    pub conflict_type: String,
    pub status: String,
    pub preferred_fact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightQueryTargetResult {
    pub target_kind: String,
    pub target: String,
    pub identity_matches: Vec<CatalogueLightIdentityMatch>,
    pub relationships: Option<Vec<CatalogueLightRelationship>>,
    pub relationships_truncated: bool,
    pub impact_targets: Option<Vec<CatalogueLightImpactTarget>>,
    pub impact_truncated: bool,
    pub coverage: Option<Vec<CatalogueLightCoverage>>,
    pub coverage_truncated: bool,
    pub conflicts: Option<Vec<CatalogueLightConflict>>,
    pub conflicts_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightSourceReference {
    pub canonical_path: String,
    pub indexed_hash: Option<String>,
    pub observed_hash: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueLightResults {
    pub query: Option<Vec<CatalogueLightQueryTargetResult>>,
    pub source_references: Option<Vec<CatalogueLightSourceReference>>,
}

#[derive(Debug)]
pub struct CatalogueContextApplicationOutput {
    pub execution: crate::context_route::ContextExecution,
    pub light_results: Option<CatalogueLightResults>,
    pub deep_context_ir: Option<crate::context_ir::DeepContextIrV2>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationBoundaryError {
    #[error("{operation} requires literal {required} confirmation")]
    LiteralConfirmation {
        operation: &'static str,
        required: &'static str,
    },
    #[error("{operation} manifest mismatch: expected {expected}, found {found}")]
    ManifestMismatch {
        operation: &'static str,
        expected: String,
        found: String,
    },
    #[error("unregister_unavailable: {reason}")]
    UnregisterUnavailable { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogueContextApplicationError {
    #[error(transparent)]
    Route(#[from] crate::context_route::ContextRouteError),
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error(transparent)]
    Cursor(#[from] ApplicationCursorError),
    #[error(transparent)]
    Lifecycle(#[from] crate::task_session::LegacyLifecycleError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Boundary(#[from] ApplicationBoundaryError),
}

struct ValidatedCatalogueContextInput {
    task_hash: String,
    normalized_goal_hash: String,
    task_kind: crate::context_ir::TaskKind,
    kind_source: crate::context_ir::TaskKindSource,
    kind_rule_id: Option<String>,
    seed_paths: Vec<String>,
    seed_symbols: Vec<String>,
}

fn canonical_targets(values: &[String]) -> Vec<String> {
    let mut canonical = values.to_vec();
    canonical.sort();
    canonical.dedup();
    canonical
}

fn validate_catalogue_application_input(
    request: &crate::context_route::ContextRouteRequest,
    input: &CatalogueContextApplicationInput,
) -> std::result::Result<ValidatedCatalogueContextInput, CatalogueContextApplicationError> {
    if input.task.trim().is_empty() {
        return Err(AtlasError::InvalidConfig("context task must not be empty".into()).into());
    }
    let task_hash = crate::hashing::content_hash_of_bytes(input.task.as_bytes());
    let normalized_goal_hash = crate::task_compiler::normalized_task_hash(&input.task);
    let (task_kind, kind_source, kind_rule_id) =
        crate::task_compiler::classify_task(input.declared_task_kind, &input.task);
    let seed_paths = canonical_targets(&input.seed_paths);
    let seed_symbols = canonical_targets(&input.seed_symbols);
    let target_count = seed_paths
        .len()
        .checked_add(seed_symbols.len())
        .ok_or_else(|| AtlasError::InvalidConfig("context target count overflow".into()))?;
    if target_count > crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS as usize
        || seed_paths
            .iter()
            .chain(&seed_symbols)
            .any(|target| target.is_empty() || target.len() > 512)
    {
        return Err(AtlasError::InvalidConfig(
            "context targets exceed the bounded application envelope".into(),
        )
        .into());
    }
    let seed_summary = crate::context_route::SeedSummary::from_targets(&seed_paths, &seed_symbols)?;
    if request.normalized_task_hash != normalized_goal_hash
        || request.declared_kind != input.declared_task_kind
        || request.classifier_kind != task_kind
        || request.explicit_seeds != seed_summary
    {
        return Err(AtlasError::InvalidConfig(
            "route identity does not match the actual task and targets".into(),
        )
        .into());
    }
    Ok(ValidatedCatalogueContextInput {
        task_hash,
        normalized_goal_hash,
        task_kind,
        kind_source,
        kind_rule_id,
        seed_paths,
        seed_symbols,
    })
}

struct CatalogueContextRouteDispatcher<'a> {
    connection: &'a mut rusqlite::Connection,
    workspace: &'a workspace::WorkspaceRecord,
    input: &'a CatalogueContextApplicationInput,
    task_hash: String,
    normalized_goal_hash: String,
    task_kind: crate::context_ir::TaskKind,
    kind_source: crate::context_ir::TaskKindSource,
    kind_rule_id: Option<String>,
    seed_paths: Vec<String>,
    seed_symbols: Vec<String>,
    light_results: CatalogueLightResults,
    deep_context_ir: Option<crate::context_ir::DeepContextIrV2>,
    materialization_authorization:
        Option<crate::task_compiler::DeepV2SourceMaterializationAuthorization>,
}

const MAX_LIGHT_ROWS_PER_TARGET: usize = 2;
const MAX_LIGHT_RECORDS_PER_TARGET: usize = 10;

#[derive(Debug, serde::Serialize)]
struct LightQueryProjection {
    #[serde(flatten)]
    result: CatalogueLightQueryTargetResult,
    #[serde(skip)]
    relationship_evidence: bool,
    #[serde(skip)]
    impact_evidence: bool,
    #[serde(skip)]
    impact_conflicting: bool,
    #[serde(skip)]
    coverage_complete: bool,
    #[serde(skip)]
    coverage_limited: bool,
    #[serde(skip)]
    relationship_conflicting: bool,
    #[serde(skip)]
    relationship_omitted: bool,
    #[serde(skip)]
    impact_omitted: bool,
    #[serde(skip)]
    coverage_omitted: bool,
    #[serde(skip)]
    coverage_conflicting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepCapabilityOutcome {
    Satisfied,
    Deficit(crate::context_route::CapabilityDeficitReason),
}

impl LightQueryProjection {
    fn record_count(&self) -> usize {
        self.result.identity_matches.len()
            + self.result.relationships.as_ref().map_or(0, Vec::len)
            + self.result.impact_targets.as_ref().map_or(0, Vec::len)
            + self.result.coverage.as_ref().map_or(0, Vec::len)
            + self.result.conflicts.as_ref().map_or(0, Vec::len)
    }
}

impl CatalogueContextRouteDispatcher<'_> {
    fn expected_generation(
        decision: &crate::context_route::ContextRouteDecision,
    ) -> std::result::Result<&str, CatalogueContextApplicationError> {
        match &decision.starting_generation {
            crate::context_route::GenerationObservation::Observed { generation_id } => {
                Ok(generation_id)
            }
            crate::context_route::GenerationObservation::Unavailable { .. } => {
                Err(AtlasError::InvalidConfig(
                    "Atlas dispatch requires an observed starting generation".into(),
                )
                .into())
            }
        }
    }

    fn bounded_query_projection(
        &self,
        generation_id: &str,
        target: &str,
        symbol: bool,
        include_relationships: bool,
        include_impact: bool,
        include_coverage: bool,
    ) -> Result<LightQueryProjection> {
        let target_kind = if symbol { "symbol" } else { "path" };
        let mut identity_statement = if symbol {
            self.connection.prepare(
                "SELECT cf.canonical_path, sf.canonical_symbol_key, cf.content_hash
                 FROM current_file cf
                 JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
                 WHERE cf.generation_id = ?1 AND sf.canonical_symbol_key = ?2
                 ORDER BY cf.canonical_path, sf.symbol_fact_id
                 LIMIT 2",
            )?
        } else {
            self.connection.prepare(
                "SELECT canonical_path, NULL, content_hash
                 FROM current_file
                 WHERE generation_id = ?1 AND canonical_path = ?2
                 ORDER BY canonical_path
                 LIMIT 2",
            )?
        };
        let identity_matches = identity_statement
            .query_map(rusqlite::params![generation_id, target], |row| {
                Ok(CatalogueLightIdentityMatch {
                    canonical_path: row.get(0)?,
                    canonical_symbol_key: row.get(1)?,
                    content_hash: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(identity_statement);
        let owner: Option<(String, String)> = if identity_matches.len() == 1 {
            self.connection
                .query_row(
                    "SELECT file_id, canonical_path FROM current_file
                     WHERE generation_id = ?1 AND canonical_path = ?2 LIMIT 1",
                    rusqlite::params![generation_id, identity_matches[0].canonical_path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        } else {
            None
        };

        let (relationships, relationships_truncated) = if include_relationships {
            let mut statement = self.connection.prepare(
                "SELECT rf.relationship_type, rf.source_ref_kind, rf.source_ref_value,
                        rf.target_ref_kind, rf.target_ref_value, rf.evidence_method,
                        CAST(ROUND(rf.confidence * 1000000.0) AS INTEGER)
                 FROM relationship_fact rf
                 JOIN current_file cf ON cf.revision_id = rf.revision_id
                 WHERE cf.generation_id = ?1
                   AND ((rf.source_ref_kind = ?2 AND rf.source_ref_value = ?3)
                     OR (rf.target_ref_kind = ?2 AND rf.target_ref_value = ?3))
                 ORDER BY rf.relationship_type, rf.source_ref_kind, rf.source_ref_value,
                          rf.target_ref_kind, rf.target_ref_value, rf.relationship_fact_id
                 LIMIT 3",
            )?;
            let mut rows = statement
                .query_map(
                    rusqlite::params![generation_id, target_kind, target],
                    |row| {
                        Ok(CatalogueLightRelationship {
                            relationship_type: row.get(0)?,
                            source_ref_kind: row.get(1)?,
                            source_ref_value: row.get(2)?,
                            target_ref_kind: row.get(3)?,
                            target_ref_value: row.get(4)?,
                            evidence_method: row.get(5)?,
                            confidence_millionths: row.get(6)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let truncated = rows.len() > MAX_LIGHT_ROWS_PER_TARGET;
            rows.truncate(MAX_LIGHT_ROWS_PER_TARGET);
            (Some(rows), truncated)
        } else {
            (None, false)
        };
        let (impact_targets, impact_truncated) = if include_impact {
            let mut statement = self.connection.prepare(
                "SELECT rf.source_ref_kind, rf.source_ref_value,
                        COALESCE(rr.status, 'unresolved'),
                        rr.resolved_ref_kind, rr.resolved_ref_value
                 FROM relationship_fact rf
                 JOIN current_file cf ON cf.revision_id = rf.revision_id
                 LEFT JOIN relationship_resolution rr
                   ON rr.relationship_fact_id = rf.relationship_fact_id
                  AND rr.generation_id = ?1
                 WHERE cf.generation_id = ?1
                   AND rf.target_ref_kind = ?2 AND rf.target_ref_value = ?3
                 ORDER BY rf.source_ref_kind, rf.source_ref_value, rf.relationship_fact_id
                 LIMIT 3",
            )?;
            let mut rows = statement
                .query_map(
                    rusqlite::params![generation_id, target_kind, target],
                    |row| {
                        Ok(CatalogueLightImpactTarget {
                            source_ref_kind: row.get(0)?,
                            source_ref_value: row.get(1)?,
                            resolution_status: row.get(2)?,
                            resolved_ref_kind: row.get(3)?,
                            resolved_ref_value: row.get(4)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let truncated = rows.len() > MAX_LIGHT_ROWS_PER_TARGET;
            rows.truncate(MAX_LIGHT_ROWS_PER_TARGET);
            (Some(rows), truncated)
        } else {
            (None, false)
        };
        let (coverage, coverage_truncated) =
            if let (true, Some((file_id, canonical_path))) = (include_coverage, owner.as_ref()) {
                let mut statement = self.connection.prepare(
                    "SELECT scope_kind, scope_key, status
                     FROM coverage_record
                     WHERE generation_id = ?1
                       AND (file_id = ?2 OR (scope_kind = 'file' AND scope_key = ?3))
                     ORDER BY scope_kind, scope_key, coverage_id
                     LIMIT 3",
                )?;
                let mut rows = statement
                    .query_map(
                        rusqlite::params![generation_id, file_id, canonical_path],
                        |row| {
                            Ok(CatalogueLightCoverage {
                                scope_kind: row.get(0)?,
                                scope_key: row.get(1)?,
                                status: row.get(2)?,
                            })
                        },
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let truncated = rows.len() > MAX_LIGHT_ROWS_PER_TARGET;
                rows.truncate(MAX_LIGHT_ROWS_PER_TARGET);
                (Some(rows), truncated)
            } else if include_coverage {
                (Some(Vec::new()), false)
            } else {
                (None, false)
            };
        let include_conflicts = include_relationships || include_impact || include_coverage;
        let (conflicts, conflicts_truncated) = if include_conflicts {
            let mut statement = self.connection.prepare(
                "SELECT subject_kind, subject_key, conflict_type, status, preferred_fact_id
                 FROM evidence_conflict
                 WHERE workspace_id = ?1 AND generation_id = ?2 AND subject_key = ?3
                   AND status IN ('open', 'preferred_with_conflict', 'unresolved')
                   AND ((?4 = 1 AND subject_kind IN ('relationship', 'effect'))
                     OR (?5 = 1 AND subject_kind = 'coverage'))
                 ORDER BY subject_kind, conflict_type, evidence_conflict_id
                 LIMIT 3",
            )?;
            let mut rows = statement
                .query_map(
                    rusqlite::params![
                        self.workspace.workspace_id,
                        generation_id,
                        target,
                        i64::from(include_relationships || include_impact),
                        i64::from(include_coverage),
                    ],
                    |row| {
                        Ok(CatalogueLightConflict {
                            subject_kind: row.get(0)?,
                            subject_key: row.get(1)?,
                            conflict_type: row.get(2)?,
                            status: row.get(3)?,
                            preferred_fact_id: row.get(4)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let truncated = rows.len() > MAX_LIGHT_ROWS_PER_TARGET;
            rows.truncate(MAX_LIGHT_ROWS_PER_TARGET);
            (Some(rows), truncated)
        } else {
            (None, false)
        };

        let relationship_conflicting = conflicts.as_ref().is_some_and(|rows| {
            rows.iter()
                .any(|row| matches!(row.subject_kind.as_str(), "relationship" | "effect"))
        });
        let relationship_evidence = relationships
            .as_ref()
            .is_some_and(|rows| !rows.is_empty() && !relationships_truncated);
        let impact_row_is_qualified =
            |row: &CatalogueLightImpactTarget| match row.resolution_status.as_str() {
                "resolved_symbol" => {
                    row.resolved_ref_kind.as_deref() == Some("symbol")
                        && row
                            .resolved_ref_value
                            .as_deref()
                            .is_some_and(|value| !value.is_empty())
                }
                "resolved_file" => {
                    row.resolved_ref_kind.as_deref() == Some("path")
                        && row
                            .resolved_ref_value
                            .as_deref()
                            .is_some_and(|value| !value.is_empty())
                }
                _ => false,
            };
        let impact_conflicting = impact_targets
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|row| !impact_row_is_qualified(row)))
            || relationship_conflicting;
        let impact_evidence = impact_targets
            .as_ref()
            .is_some_and(|rows| !rows.is_empty() && rows.iter().all(impact_row_is_qualified));
        let coverage_conflicting = conflicts
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|row| row.subject_kind == "coverage"));
        let coverage_complete = coverage.as_ref().is_some_and(|rows| {
            !rows.is_empty()
                && !coverage_truncated
                && rows.iter().all(|row| row.status == "complete")
        });
        let coverage_limited = coverage_truncated
            || coverage
                .as_ref()
                .is_some_and(|rows| rows.iter().any(|row| row.status != "complete"));
        Ok(LightQueryProjection {
            result: CatalogueLightQueryTargetResult {
                target_kind: target_kind.to_string(),
                target: target.to_string(),
                identity_matches,
                relationships,
                relationships_truncated,
                impact_targets,
                impact_truncated,
                coverage,
                coverage_truncated,
                conflicts,
                conflicts_truncated,
            },
            relationship_evidence,
            impact_evidence,
            impact_conflicting,
            coverage_complete,
            coverage_limited,
            relationship_conflicting,
            relationship_omitted: relationships_truncated || conflicts_truncated,
            impact_omitted: impact_truncated || conflicts_truncated,
            coverage_omitted: coverage_truncated || conflicts_truncated,
            coverage_conflicting,
        })
    }

    fn dispatch_real_light(
        &mut self,
        decision: &crate::context_route::ContextRouteDecision,
        operations: &[crate::context_route::VersionedOperation],
    ) -> Result<crate::context_route::RouteAttemptOutcome> {
        use crate::context_route::{
            CallerCapabilityState, CapabilityDeficitReason, ContextPayload, LightOperation,
            LightOperationResult, RequiredCapability, RouteAttemptOutcome, RouteDeficit,
        };

        let generation_id = Self::expected_generation(decision)
            .map_err(|error| AtlasError::InvalidConfig(error.to_string()))?;
        let needs_atlas = |capability| {
            decision
                .requested_capabilities
                .get(&capability)
                .is_some_and(|state| *state != CallerCapabilityState::Satisfied)
        };
        let relationships_requested = needs_atlas(RequiredCapability::BoundedRelationships);
        let impact_requested = needs_atlas(RequiredCapability::ImpactFrontier);
        let coverage_requested = needs_atlas(RequiredCapability::BasicCoverageConflicts);
        let query_requested = needs_atlas(RequiredCapability::IdentityLookup)
            || relationships_requested
            || impact_requested
            || coverage_requested;
        let source_requested = needs_atlas(RequiredCapability::ExactSource);
        let caller_satisfied = decision
            .requested_capabilities
            .values()
            .all(|state| *state == CallerCapabilityState::Satisfied);
        let query_target_count = self
            .seed_paths
            .len()
            .checked_add(self.seed_symbols.len())
            .ok_or_else(|| AtlasError::InvalidConfig("LIGHT target count overflow".into()))?;
        let query_invoked = (query_requested || caller_satisfied) && query_target_count > 0;

        let mut results = Vec::with_capacity(2);
        let mut atlas_calls = 0_u64;
        let mut records = 0_u64;
        let mut work_units = 0_u64;
        let max_query_targets =
            usize::try_from(crate::context_metrics::MAX_CONTEXT_EXECUTION_RECORDS)
                .map_err(|_| AtlasError::InvalidConfig("LIGHT record bound is invalid".into()))?
                / MAX_LIGHT_RECORDS_PER_TARGET;
        let mut query_projections = Vec::with_capacity(query_target_count.min(max_query_targets));
        let mut query_record_count = 0_usize;
        let mut query_semantic_omitted = false;
        if query_invoked {
            let operation = operations
                .iter()
                .find(|operation| operation.operation == LightOperation::Query)
                .ok_or_else(|| AtlasError::InvalidConfig("missing pinned LIGHT query".into()))?;
            for (target, symbol) in self
                .seed_paths
                .iter()
                .map(|target| (target, false))
                .chain(self.seed_symbols.iter().map(|target| (target, true)))
                .take(max_query_targets)
            {
                let projection = self.bounded_query_projection(
                    generation_id,
                    target,
                    symbol,
                    relationships_requested,
                    impact_requested,
                    coverage_requested,
                )?;
                query_record_count = query_record_count
                    .checked_add(projection.record_count())
                    .ok_or_else(|| {
                        AtlasError::InvalidConfig("LIGHT query count overflow".into())
                    })?;
                query_projections.push(projection);
            }
            query_semantic_omitted = query_projections.len() < query_target_count;
            atlas_calls = atlas_calls
                .checked_add(1)
                .ok_or_else(|| AtlasError::InvalidConfig("LIGHT call count overflow".into()))?;
            work_units =
                work_units
                    .checked_add(u64::try_from(query_projections.len()).map_err(|_| {
                        AtlasError::InvalidConfig("LIGHT work count overflow".into())
                    })?)
                    .ok_or_else(|| AtlasError::InvalidConfig("LIGHT work count overflow".into()))?;
            records = records
                .checked_add(
                    u64::try_from(query_record_count).map_err(|_| {
                        AtlasError::InvalidConfig("LIGHT query count overflow".into())
                    })?,
                )
                .ok_or_else(|| AtlasError::InvalidConfig("LIGHT record count overflow".into()))?;
            results.push(LightOperationResult::Query {
                operation_version: operation.version.clone(),
                result_digest: crate::hashing::content_hash_of_bytes(&serde_json::to_vec(
                    &query_projections,
                )?),
                record_count: u64::try_from(query_record_count)
                    .map_err(|_| AtlasError::InvalidConfig("LIGHT query count overflow".into()))?,
            });
            self.light_results.query = Some(
                query_projections
                    .iter()
                    .map(|projection| projection.result.clone())
                    .collect(),
            );
        }

        let mut source_result_count = 0_usize;
        let mut source_missing_target = false;
        let mut source_had_stale_target = false;
        let mut source_had_unavailable_target = false;
        if source_requested && !self.seed_paths.is_empty() {
            let operation = operations
                .iter()
                .find(|operation| operation.operation == LightOperation::SourceReference)
                .ok_or_else(|| {
                    AtlasError::InvalidConfig("missing pinned LIGHT source reference".into())
                })?;
            let defaults = crate::task_compiler::CompileRequest::default();
            let mut observations = Vec::with_capacity(self.seed_paths.len());
            let mut source_bytes_scheduled = 0_i64;
            for path in &self.seed_paths {
                let byte_size: Option<i64> = self
                    .connection
                    .query_row(
                        "SELECT fr.byte_size FROM current_file cf
                         JOIN file_revision fr ON fr.revision_id = cf.revision_id
                         WHERE cf.generation_id = ?1 AND cf.canonical_path = ?2",
                        rusqlite::params![generation_id, path],
                        |row| row.get(0),
                    )
                    .optional()?;
                let verification = if let Some(byte_size) = byte_size.filter(|size| *size >= 0) {
                    let next_total = source_bytes_scheduled.checked_add(byte_size);
                    if next_total.is_some_and(|total| total <= defaults.max_source_bytes) {
                        source_bytes_scheduled = next_total.expect("bounded source byte sum");
                        crate::query::verify_exact_source_at_generation(
                            self.connection,
                            self.workspace,
                            generation_id,
                            path,
                            None,
                            false,
                        )?
                    } else {
                        crate::query::ExactSourceVerification {
                            indexed_hash: None,
                            observed_hash: None,
                            status: "read_failed",
                            confinement: crate::context_ir::IrPathConfinementV2::Unavailable,
                            materialized_bytes: None,
                            range_valid: true,
                        }
                    }
                } else {
                    crate::query::ExactSourceVerification {
                        indexed_hash: None,
                        observed_hash: None,
                        status: "not_found",
                        confinement: crate::context_ir::IrPathConfinementV2::Unavailable,
                        materialized_bytes: None,
                        range_valid: true,
                    }
                };
                source_missing_target |= verification.status == "not_found";
                source_had_stale_target |= verification.status == "hash_mismatch";
                source_had_unavailable_target |= !matches!(
                    verification.status,
                    "verified" | "not_found" | "hash_mismatch"
                );
                source_result_count += usize::from(
                    verification.status == "verified"
                        && verification.indexed_hash.is_some()
                        && verification.indexed_hash == verification.observed_hash,
                );
                observations.push(CatalogueLightSourceReference {
                    canonical_path: path.clone(),
                    indexed_hash: verification.indexed_hash,
                    observed_hash: verification.observed_hash,
                    status: verification.status.to_string(),
                });
            }
            let source_record_count = observations.len();
            atlas_calls = atlas_calls
                .checked_add(1)
                .ok_or_else(|| AtlasError::InvalidConfig("LIGHT call count overflow".into()))?;
            work_units =
                work_units
                    .checked_add(u64::try_from(self.seed_paths.len()).map_err(|_| {
                        AtlasError::InvalidConfig("LIGHT work count overflow".into())
                    })?)
                    .ok_or_else(|| AtlasError::InvalidConfig("LIGHT work count overflow".into()))?;
            records = records
                .checked_add(
                    u64::try_from(source_record_count).map_err(|_| {
                        AtlasError::InvalidConfig("LIGHT source count overflow".into())
                    })?,
                )
                .ok_or_else(|| AtlasError::InvalidConfig("LIGHT record count overflow".into()))?;
            results.push(LightOperationResult::SourceReference {
                operation_version: operation.version.clone(),
                result_digest: crate::hashing::content_hash_of_bytes(&serde_json::to_vec(
                    &observations,
                )?),
                reference_count: u64::try_from(source_record_count)
                    .map_err(|_| AtlasError::InvalidConfig("LIGHT source count overflow".into()))?,
            });
            self.light_results.source_references = Some(observations);
        }

        let query_identity_deficit = if query_projections.is_empty()
            || query_projections
                .iter()
                .any(|projection| projection.result.identity_matches.is_empty())
        {
            Some(CapabilityDeficitReason::Unsupported)
        } else if query_projections
            .iter()
            .any(|projection| projection.result.identity_matches.len() > 1)
        {
            Some(CapabilityDeficitReason::Ambiguous)
        } else {
            None
        };
        let mut satisfied_capabilities = Vec::new();
        let mut deficits = Vec::new();
        for (capability, state) in &decision.requested_capabilities {
            let outcome = if *state == CallerCapabilityState::Satisfied {
                Ok(())
            } else {
                match capability {
                    RequiredCapability::IdentityLookup if query_semantic_omitted => {
                        Err(CapabilityDeficitReason::SemanticBudgetOmitted)
                    }
                    RequiredCapability::IdentityLookup => {
                        query_identity_deficit.map_or(Ok(()), Err)
                    }
                    RequiredCapability::BoundedRelationships => {
                        if query_semantic_omitted
                            || query_projections
                                .iter()
                                .any(|projection| projection.relationship_omitted)
                        {
                            Err(CapabilityDeficitReason::SemanticBudgetOmitted)
                        } else if let Some(reason) = query_identity_deficit {
                            Err(reason)
                        } else if query_projections
                            .iter()
                            .any(|projection| projection.relationship_conflicting)
                        {
                            Err(CapabilityDeficitReason::Conflicting)
                        } else if query_projections
                            .iter()
                            .all(|projection| projection.relationship_evidence)
                        {
                            Ok(())
                        } else {
                            Err(CapabilityDeficitReason::Unavailable)
                        }
                    }
                    RequiredCapability::ImpactFrontier => {
                        if query_semantic_omitted
                            || query_projections
                                .iter()
                                .any(|projection| projection.impact_omitted)
                        {
                            Err(CapabilityDeficitReason::SemanticBudgetOmitted)
                        } else if let Some(reason) = query_identity_deficit {
                            Err(reason)
                        } else if query_projections.iter().any(|projection| {
                            projection.relationship_conflicting || projection.impact_conflicting
                        }) {
                            Err(CapabilityDeficitReason::Conflicting)
                        } else if query_projections
                            .iter()
                            .all(|projection| projection.impact_evidence)
                        {
                            Ok(())
                        } else {
                            Err(CapabilityDeficitReason::Unavailable)
                        }
                    }
                    RequiredCapability::BasicCoverageConflicts => {
                        if query_semantic_omitted
                            || query_projections
                                .iter()
                                .any(|projection| projection.coverage_omitted)
                        {
                            Err(CapabilityDeficitReason::SemanticBudgetOmitted)
                        } else if let Some(reason) = query_identity_deficit {
                            Err(reason)
                        } else if query_projections.iter().any(|projection| {
                            projection.coverage_conflicting
                                || (projection.coverage_complete && projection.coverage_limited)
                        }) {
                            Err(CapabilityDeficitReason::Conflicting)
                        } else if query_projections.iter().all(|projection| {
                            projection.coverage_complete && !projection.coverage_limited
                        }) {
                            Ok(())
                        } else {
                            Err(CapabilityDeficitReason::Unavailable)
                        }
                    }
                    RequiredCapability::ExactSource
                        if !self.seed_paths.is_empty()
                            && source_result_count == self.seed_paths.len()
                            && !source_missing_target
                            && !source_had_stale_target
                            && !source_had_unavailable_target =>
                    {
                        Ok(())
                    }
                    RequiredCapability::ExactSource if source_missing_target => {
                        Err(CapabilityDeficitReason::Unsupported)
                    }
                    RequiredCapability::ExactSource if source_had_stale_target => {
                        Err(CapabilityDeficitReason::Stale)
                    }
                    RequiredCapability::ExactSource if source_had_unavailable_target => {
                        Err(CapabilityDeficitReason::Unavailable)
                    }
                    RequiredCapability::ExactSource => Err(CapabilityDeficitReason::Unsupported),
                    RequiredCapability::TemporalHistory
                    | RequiredCapability::RequiredRoleClosure
                    | RequiredCapability::ValidationPlan => {
                        Err(CapabilityDeficitReason::Unavailable)
                    }
                }
            };
            match outcome {
                Ok(()) => satisfied_capabilities.push(*capability),
                Err(reason) => deficits.push(RouteDeficit::Capability {
                    capability: *capability,
                    reason,
                }),
            }
        }

        let counters = crate::context_metrics::ContextExecutionCounters {
            atlas_calls,
            records,
            source_bytes: 0,
            estimated_tokens: 0,
            work_units,
        };
        let payload = (!results.is_empty()).then_some(ContextPayload::LightBundle {
            operations: results,
        });
        if deficits.is_empty() {
            Ok(RouteAttemptOutcome::Completed {
                payload: payload.ok_or_else(|| {
                    AtlasError::InvalidConfig("completed LIGHT route has no payload".into())
                })?,
                counters,
                diagnostics: Vec::new(),
                materialization: None,
            })
        } else {
            Ok(RouteAttemptOutcome::Deficient {
                payload,
                satisfied_capabilities,
                deficits,
                counters,
                diagnostics: Vec::new(),
                materialization: None,
            })
        }
    }

    fn deficit_priority(reason: crate::context_route::CapabilityDeficitReason) -> u8 {
        use crate::context_route::CapabilityDeficitReason;

        match reason {
            CapabilityDeficitReason::Conflicting => 0,
            CapabilityDeficitReason::Stale => 1,
            CapabilityDeficitReason::Ambiguous => 2,
            CapabilityDeficitReason::Unsupported => 3,
            CapabilityDeficitReason::SemanticBudgetOmitted => 4,
            CapabilityDeficitReason::Unavailable => 5,
        }
    }

    fn preferred_deficit(
        current: Option<crate::context_route::CapabilityDeficitReason>,
        candidate: crate::context_route::CapabilityDeficitReason,
    ) -> Option<crate::context_route::CapabilityDeficitReason> {
        match current {
            Some(current)
                if Self::deficit_priority(current) <= Self::deficit_priority(candidate) =>
            {
                Some(current)
            }
            _ => Some(candidate),
        }
    }

    fn omission_deficit(
        reason: crate::context_ir::IrOmissionReasonV2,
    ) -> crate::context_route::CapabilityDeficitReason {
        use crate::context_ir::IrOmissionReasonV2;
        use crate::context_route::CapabilityDeficitReason;

        match reason {
            IrOmissionReasonV2::ConflictingEvidence => CapabilityDeficitReason::Conflicting,
            IrOmissionReasonV2::StaleEvidence | IrOmissionReasonV2::SourceVerificationFailed => {
                CapabilityDeficitReason::Stale
            }
            IrOmissionReasonV2::UnresolvedEvidence
            | IrOmissionReasonV2::AmbiguousSeed
            | IrOmissionReasonV2::FtsCandidateLimit => CapabilityDeficitReason::Ambiguous,
            IrOmissionReasonV2::UnsupportedEvidence => CapabilityDeficitReason::Unsupported,
            IrOmissionReasonV2::RecordBudget
            | IrOmissionReasonV2::SourceByteBudget
            | IrOmissionReasonV2::EstimatedTokenBudget
            | IrOmissionReasonV2::RelationshipDepthBudget
            | IrOmissionReasonV2::WorkUnitBudget
            | IrOmissionReasonV2::UncertaintyReserve => {
                CapabilityDeficitReason::SemanticBudgetOmitted
            }
            IrOmissionReasonV2::MissingRequiredRole
            | IrOmissionReasonV2::UnavailableEvidence
            | IrOmissionReasonV2::TemporalEvidenceUnavailable
            | IrOmissionReasonV2::ValidationTargetUnavailable => {
                CapabilityDeficitReason::Unavailable
            }
        }
    }

    fn requirement_deficit(
        state: crate::context_ir::IrRequirementStateV2,
        reason: Option<crate::context_ir::IrOmissionReasonV2>,
    ) -> Option<crate::context_route::CapabilityDeficitReason> {
        use crate::context_ir::IrRequirementStateV2;
        use crate::context_route::CapabilityDeficitReason;

        let state_deficit = match state {
            IrRequirementStateV2::Satisfied => None,
            IrRequirementStateV2::Unresolved => Some(CapabilityDeficitReason::Ambiguous),
            IrRequirementStateV2::Stale => Some(CapabilityDeficitReason::Stale),
            IrRequirementStateV2::Unsupported => Some(CapabilityDeficitReason::Unsupported),
            IrRequirementStateV2::Conflicting => Some(CapabilityDeficitReason::Conflicting),
            IrRequirementStateV2::BudgetOmitted => {
                Some(CapabilityDeficitReason::SemanticBudgetOmitted)
            }
            IrRequirementStateV2::Missing | IrRequirementStateV2::Unavailable => {
                Some(CapabilityDeficitReason::Unavailable)
            }
        };
        match reason {
            Some(reason) => Self::preferred_deficit(state_deficit, Self::omission_deficit(reason)),
            None => state_deficit,
        }
    }

    fn evidence_deficit(
        quality: crate::context_ir::EvidenceQuality,
    ) -> Option<crate::context_route::CapabilityDeficitReason> {
        use crate::context_ir::EvidenceQuality;
        use crate::context_route::CapabilityDeficitReason;

        match quality {
            EvidenceQuality::Verified | EvidenceQuality::Supported | EvidenceQuality::Resolved => {
                None
            }
            EvidenceQuality::Ambiguous
            | EvidenceQuality::Unresolved
            | EvidenceQuality::Inferred
            | EvidenceQuality::Partial
            | EvidenceQuality::External => Some(CapabilityDeficitReason::Ambiguous),
            EvidenceQuality::Stale => Some(CapabilityDeficitReason::Stale),
            EvidenceQuality::Conflicting => Some(CapabilityDeficitReason::Conflicting),
            EvidenceQuality::Unsupported | EvidenceQuality::Excluded => {
                Some(CapabilityDeficitReason::Unsupported)
            }
        }
    }

    fn role_capability_outcome(
        document: &crate::context_ir::DeepContextIrV2,
        role: crate::context_ir::ItemRole,
    ) -> DeepCapabilityOutcome {
        use crate::context_route::CapabilityDeficitReason;

        let Some(entry) = document
            .status
            .role_sufficiency
            .iter()
            .find(|entry| entry.role == role)
        else {
            return DeepCapabilityOutcome::Deficit(CapabilityDeficitReason::Unavailable);
        };
        let mut deficit = Self::requirement_deficit(entry.state, entry.reason);
        if entry.evidence_item_ids.is_empty() {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
        }
        for item_id in &entry.evidence_item_ids {
            let Some(item) = document
                .working_set
                .iter()
                .find(|item| item.item_id == *item_id && item.role == role)
            else {
                deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
                continue;
            };
            if let Some(reason) = Self::evidence_deficit(item.evidence.state) {
                deficit = Self::preferred_deficit(deficit, reason);
            }
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn exact_source_outcome(
        document: &crate::context_ir::DeepContextIrV2,
    ) -> DeepCapabilityOutcome {
        use crate::context_ir::{IrPathConfinementV2, IrSourceVerificationV2};
        use crate::context_route::CapabilityDeficitReason;

        let mut deficit = Self::requirement_deficit(document.status.exact_source, None);
        let mut selected_source_count = 0usize;
        for source in document
            .working_set
            .iter()
            .filter_map(|item| item.source.as_ref())
        {
            selected_source_count += 1;
            let verification_deficit = match source.verification_status {
                IrSourceVerificationV2::Verified
                    if source.revalidated_before_seal
                        && source.observed_sha256.as_deref()
                            == Some(source.whole_file_sha256.as_str()) =>
                {
                    None
                }
                IrSourceVerificationV2::Verified => Some(CapabilityDeficitReason::Stale),
                IrSourceVerificationV2::Stale => Some(CapabilityDeficitReason::Stale),
                IrSourceVerificationV2::Unavailable => Some(CapabilityDeficitReason::Unavailable),
                IrSourceVerificationV2::Unsupported => Some(CapabilityDeficitReason::Unsupported),
            };
            if let Some(reason) = verification_deficit {
                deficit = Self::preferred_deficit(deficit, reason);
            }
            let confinement_deficit = match source.canonical_root_confinement {
                IrPathConfinementV2::Confined => None,
                IrPathConfinementV2::Unavailable => Some(CapabilityDeficitReason::Unavailable),
                IrPathConfinementV2::OutsideCanonicalRoot | IrPathConfinementV2::SymlinkEscape => {
                    Some(CapabilityDeficitReason::Unsupported)
                }
            };
            if let Some(reason) = confinement_deficit {
                deficit = Self::preferred_deficit(deficit, reason);
            }
        }
        if selected_source_count == 0 {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
            if [
                crate::context_ir::IrOmissionReasonV2::RecordBudget,
                crate::context_ir::IrOmissionReasonV2::SourceByteBudget,
                crate::context_ir::IrOmissionReasonV2::EstimatedTokenBudget,
                crate::context_ir::IrOmissionReasonV2::WorkUnitBudget,
            ]
            .iter()
            .any(|reason| document.omissions.by_reason.contains_key(reason))
            {
                deficit = Self::preferred_deficit(
                    deficit,
                    CapabilityDeficitReason::SemanticBudgetOmitted,
                );
            }
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn relationship_outcome(
        document: &crate::context_ir::DeepContextIrV2,
    ) -> DeepCapabilityOutcome {
        use crate::context_route::CapabilityDeficitReason;
        use crate::resolution::ResolutionStatus;

        if document.relationships.is_empty() {
            return DeepCapabilityOutcome::Deficit(CapabilityDeficitReason::Unavailable);
        }
        let mut deficit = None;
        for relationship in &document.relationships {
            let resolution_deficit = match relationship.resolution_state {
                ResolutionStatus::ResolvedSymbol | ResolutionStatus::ResolvedFile => None,
                ResolutionStatus::Stale => Some(CapabilityDeficitReason::Stale),
                ResolutionStatus::Ambiguous
                | ResolutionStatus::Unresolved
                | ResolutionStatus::Invalid
                | ResolutionStatus::External => Some(CapabilityDeficitReason::Ambiguous),
            };
            if let Some(reason) = resolution_deficit {
                deficit = Self::preferred_deficit(deficit, reason);
            }
            if let Some(reason) = Self::evidence_deficit(relationship.evidence.state) {
                deficit = Self::preferred_deficit(deficit, reason);
            }
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn effect_outcome(document: &crate::context_ir::DeepContextIrV2) -> DeepCapabilityOutcome {
        use crate::context_route::CapabilityDeficitReason;

        if document.effects.is_empty() {
            return DeepCapabilityOutcome::Deficit(CapabilityDeficitReason::Unavailable);
        }
        let deficit = document
            .effects
            .iter()
            .filter_map(|effect| Self::evidence_deficit(effect.evidence.state))
            .fold(None, Self::preferred_deficit);
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn coverage_outcome(document: &crate::context_ir::DeepContextIrV2) -> DeepCapabilityOutcome {
        use crate::context_ir::ClaimStrength;
        use crate::context_route::CapabilityDeficitReason;

        let mut deficit = document
            .coverage
            .deficits
            .iter()
            .copied()
            .map(Self::omission_deficit)
            .fold(None, Self::preferred_deficit);
        if document.coverage.partial > 0 || document.coverage.failed > 0 {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
        }
        if document.coverage.unsupported > 0 {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unsupported);
        }
        if document.coverage.eligible == 0
            || document.coverage.complete == 0
            || document.coverage.claim_strength == ClaimStrength::None
        {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn validation_outcome(document: &crate::context_ir::DeepContextIrV2) -> DeepCapabilityOutcome {
        use crate::context_route::CapabilityDeficitReason;

        let mut deficit = Self::requirement_deficit(document.status.validation, None);
        if document.validation_plan.is_empty() {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
        }
        if let DeepCapabilityOutcome::Deficit(reason) =
            Self::role_capability_outcome(document, crate::context_ir::ItemRole::ValidationTarget)
        {
            deficit = Self::preferred_deficit(deficit, reason);
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn required_role_closure_outcome(
        document: &crate::context_ir::DeepContextIrV2,
    ) -> DeepCapabilityOutcome {
        use crate::context_route::CapabilityDeficitReason;

        let mut deficit = None;
        let mut required_role_count = 0usize;
        for entry in document
            .status
            .role_sufficiency
            .iter()
            .filter(|entry| entry.required)
        {
            required_role_count += 1;
            if let DeepCapabilityOutcome::Deficit(reason) =
                Self::role_capability_outcome(document, entry.role)
            {
                deficit = Self::preferred_deficit(deficit, reason);
            }
        }
        if required_role_count == 0 {
            deficit = Self::preferred_deficit(deficit, CapabilityDeficitReason::Unavailable);
        }
        for outcome in [
            Self::exact_source_outcome(document),
            Self::validation_outcome(document),
        ] {
            if let DeepCapabilityOutcome::Deficit(reason) = outcome {
                deficit = Self::preferred_deficit(deficit, reason);
            }
        }
        deficit.map_or(
            DeepCapabilityOutcome::Satisfied,
            DeepCapabilityOutcome::Deficit,
        )
    }

    fn deep_capability_outcome(
        document: &crate::context_ir::DeepContextIrV2,
        capability: crate::context_route::RequiredCapability,
    ) -> DeepCapabilityOutcome {
        use crate::context_ir::ItemRole;
        use crate::context_route::RequiredCapability;

        match capability {
            RequiredCapability::IdentityLookup => {
                Self::role_capability_outcome(document, ItemRole::PrimaryImplementation)
            }
            RequiredCapability::ExactSource => Self::exact_source_outcome(document),
            RequiredCapability::BoundedRelationships => Self::relationship_outcome(document),
            RequiredCapability::ImpactFrontier => Self::effect_outcome(document),
            RequiredCapability::BasicCoverageConflicts => Self::coverage_outcome(document),
            RequiredCapability::TemporalHistory => {
                Self::role_capability_outcome(document, ItemRole::HistoricalConstraint)
            }
            RequiredCapability::RequiredRoleClosure => {
                Self::required_role_closure_outcome(document)
            }
            RequiredCapability::ValidationPlan => Self::validation_outcome(document),
        }
    }

    fn compile_deep_for_session(
        &mut self,
        generation_id: &str,
        task_session_id: String,
        budget: &crate::context_route::DeepSemanticBudget,
    ) -> Result<crate::task_compiler::SealedDeepContextIrV2> {
        crate::task_compiler::compile_deep_context_ir_v2(
            self.connection,
            self.workspace,
            generation_id,
            &crate::task_compiler::DeepV2CompileRequest {
                task_session_id,
                task_hash: self.task_hash.clone(),
                normalized_goal_hash: self.normalized_goal_hash.clone(),
                task_kind: self.task_kind,
                kind_source: self.kind_source,
                kind_rule_id: self.kind_rule_id.clone(),
                seed_paths: self.seed_paths.clone(),
                seed_symbols: self.seed_symbols.clone(),
                baseline_generation_id: self.input.baseline_generation_id.clone(),
            },
            budget,
            self.materialization_authorization,
        )
    }

    fn dispatch_real_deep(
        &mut self,
        decision: &crate::context_route::ContextRouteDecision,
        budget: &crate::context_route::DeepSemanticBudget,
    ) -> Result<crate::context_route::RouteAttemptOutcome> {
        use crate::context_route::{
            CallerCapabilityState, ContextDiagnosticCode, RouteAttemptOutcome, RouteDeficit,
        };

        let generation_id = Self::expected_generation(decision)
            .map_err(|error| AtlasError::InvalidConfig(error.to_string()))?;
        let target_count = u64::try_from(
            self.seed_paths
                .len()
                .checked_add(self.seed_symbols.len())
                .ok_or_else(|| AtlasError::InvalidConfig("V2 target count overflow".into()))?,
        )
        .map_err(|_| AtlasError::InvalidConfig("V2 target count overflow".into()))?;
        if target_count > budget.max_records || target_count > budget.max_work_units {
            return Err(AtlasError::InvalidConfig(
                "V2 targets exceed their explicit semantic work bounds".into(),
            ));
        }
        let task_session_id = self.input.task_session_id.clone();
        let sealed = if let Some(task_session_id) = task_session_id {
            self.compile_deep_for_session(generation_id, task_session_id, budget)?
        } else {
            // The compiler validates its response identity through the legacy
            // table. Keep that compatibility row inside one rollback-only
            // savepoint so unattributed execution cannot persist or activate it.
            self.connection
                .execute_batch("SAVEPOINT atlas_response_lifetime_deep")?;
            let compilation = (|| {
                let session = crate::task_session::create_classified_task_session(
                    self.connection,
                    self.workspace,
                    generation_id,
                    &self.task_hash,
                    &self.normalized_goal_hash,
                    crate::context_ir::RawTaskRetention::None,
                    None,
                    self.task_kind,
                    self.kind_source,
                    self.kind_rule_id.as_deref(),
                    crate::context_ir::CONTEXT_SCHEMA_VERSION,
                )?;
                self.compile_deep_for_session(generation_id, session.task_session_id, budget)
            })();
            let cleanup = self.connection.execute_batch(
                "ROLLBACK TO atlas_response_lifetime_deep; \
                 RELEASE atlas_response_lifetime_deep",
            );
            cleanup?;
            compilation?
        };
        let payload = crate::context_ir::deep_context_payload(&sealed.context_ir)?;
        let mut satisfied_capabilities = Vec::new();
        let mut deficits = Vec::new();
        for (capability, state) in &decision.requested_capabilities {
            let outcome = if *state == CallerCapabilityState::Satisfied {
                DeepCapabilityOutcome::Satisfied
            } else {
                Self::deep_capability_outcome(&sealed.context_ir, *capability)
            };
            match outcome {
                DeepCapabilityOutcome::Satisfied => satisfied_capabilities.push(*capability),
                DeepCapabilityOutcome::Deficit(reason) => {
                    deficits.push(RouteDeficit::Capability {
                        capability: *capability,
                        reason,
                    });
                }
            }
        }
        let counters = crate::context_metrics::ContextExecutionCounters {
            atlas_calls: 1,
            records: sealed.context_ir.cost.selected_records,
            source_bytes: sealed.context_ir.cost.selected_source_bytes,
            estimated_tokens: sealed.context_ir.cost.selected_estimated_tokens,
            work_units: sealed.context_ir.cost.work_units_consumed,
        };
        let diagnostics = if sealed.context_ir.omissions.truncated {
            vec![ContextDiagnosticCode::EvidenceOmitted]
        } else {
            Vec::new()
        };
        let useful = deficits.is_empty() || !satisfied_capabilities.is_empty();
        let materialization = useful.then_some(sealed.materialization).flatten();
        self.deep_context_ir = Some(sealed.context_ir);
        if deficits.is_empty() {
            Ok(RouteAttemptOutcome::Completed {
                payload,
                counters,
                diagnostics,
                materialization,
            })
        } else {
            Ok(RouteAttemptOutcome::Deficient {
                payload: Some(payload),
                satisfied_capabilities,
                deficits,
                counters,
                diagnostics,
                materialization,
            })
        }
    }
}

fn catalogue_generation_observation(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
) -> crate::context_route::GenerationObservation {
    match connection
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            [&workspace.workspace_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
    {
        Ok(Some(Some(generation_id))) => {
            crate::context_route::GenerationObservation::Observed { generation_id }
        }
        Ok(Some(None)) | Ok(None) => crate::context_route::GenerationObservation::Unavailable {
            reason: crate::context_route::GenerationUnavailableReason::NoActiveGeneration,
        },
        Err(_) => crate::context_route::GenerationObservation::Unavailable {
            reason: crate::context_route::GenerationUnavailableReason::ObservationFailed,
        },
    }
}

impl crate::context_route::ContextRouteDispatcher for CatalogueContextRouteDispatcher<'_> {
    type Error = CatalogueContextApplicationError;

    fn observe_generation(&mut self) -> crate::context_route::GenerationObservation {
        catalogue_generation_observation(self.connection, self.workspace)
    }

    fn dispatch_light(
        &mut self,
        decision: &crate::context_route::ContextRouteDecision,
        operations: &[crate::context_route::VersionedOperation],
    ) -> std::result::Result<
        crate::context_route::RouteAttemptOutcome,
        CatalogueContextApplicationError,
    > {
        self.dispatch_real_light(decision, operations)
            .map_err(CatalogueContextApplicationError::from)
    }

    fn dispatch_deep(
        &mut self,
        decision: &crate::context_route::ContextRouteDecision,
        contracts: &crate::context_route::DeepContractVersions,
        budget: &crate::context_route::DeepSemanticBudget,
    ) -> std::result::Result<
        crate::context_route::RouteAttemptOutcome,
        CatalogueContextApplicationError,
    > {
        if contracts != &crate::context_route::DeepContractVersions::fixed() {
            return Err(crate::context_route::ContextRouteError::InvalidExecution.into());
        }
        self.dispatch_real_deep(decision, budget)
            .map_err(CatalogueContextApplicationError::from)
    }
}

/// Execute the frozen progressive governor without source materialization.
pub fn dispatch_catalogue_context_application(
    request: crate::context_route::ContextRouteRequest,
    input: CatalogueContextApplicationInput,
    connection: &mut rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
) -> std::result::Result<CatalogueContextApplicationOutput, CatalogueContextApplicationError> {
    dispatch_catalogue_context_application_with_materialization(
        request, input, connection, workspace, None,
    )
}

/// Execute the frozen progressive governor against real bounded catalogue
/// operations and task-conditioned V2 composition. No public command grammar
/// is added by this internal application boundary.
fn dispatch_catalogue_context_application_with_materialization(
    mut request: crate::context_route::ContextRouteRequest,
    input: CatalogueContextApplicationInput,
    connection: &mut rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    materialization_authorization: Option<
        crate::task_compiler::DeepV2SourceMaterializationAuthorization,
    >,
) -> std::result::Result<CatalogueContextApplicationOutput, CatalogueContextApplicationError> {
    let validated = validate_catalogue_application_input(&request, &input)?;
    let bound_start_generation_id = if let Some(task_session_id) = input.task_session_id.as_deref()
    {
        let start_generation_id = match &request.starting_generation {
            crate::context_route::GenerationObservation::Observed { generation_id } => {
                generation_id.clone()
            }
            crate::context_route::GenerationObservation::Unavailable { .. } => {
                return Err(AtlasError::InvalidConfig(
                    "bound legacy task session requires an observed starting generation".into(),
                )
                .into());
            }
        };
        crate::task_session::validate_legacy_task_session_identity(
            connection,
            task_session_id,
            &crate::task_session::LegacyTaskSessionIdentity {
                workspace_id: &workspace.workspace_id,
                start_generation_id: &start_generation_id,
                task_hash: &validated.task_hash,
                normalized_goal_hash: &validated.normalized_goal_hash,
                task_kind: validated.task_kind,
                kind_source: validated.kind_source,
                kind_rule_id: validated.kind_rule_id.as_deref(),
            },
        )?;
        Some(start_generation_id)
    } else {
        None
    };
    let observed_generation = catalogue_generation_observation(connection, workspace);
    let initial_decision = crate::context_route::decide_context_route(request.clone())?;
    let direct_succeeds_without_atlas = initial_decision.initial_route
        == crate::context_route::Route::Direct
        && initial_decision
            .requested_capabilities
            .values()
            .all(|state| *state == crate::context_route::CallerCapabilityState::Satisfied);
    let decision =
        if direct_succeeds_without_atlas && request.starting_generation != observed_generation {
            request.starting_generation = observed_generation;
            crate::context_route::decide_context_route(request)?
        } else {
            initial_decision
        };
    let mut dispatcher = CatalogueContextRouteDispatcher {
        connection,
        workspace,
        input: &input,
        task_hash: validated.task_hash,
        normalized_goal_hash: validated.normalized_goal_hash,
        task_kind: validated.task_kind,
        kind_source: validated.kind_source,
        kind_rule_id: validated.kind_rule_id,
        seed_paths: validated.seed_paths,
        seed_symbols: validated.seed_symbols,
        light_results: CatalogueLightResults::default(),
        deep_context_ir: None,
        materialization_authorization,
    };
    let execution = crate::context_route::execute_context_route(decision, &mut dispatcher)?;
    let light_results_match = match execution.payload() {
        Some(crate::context_route::ContextPayload::LightBundle { operations }) => {
            let expected_count = usize::from(dispatcher.light_results.query.is_some())
                + usize::from(dispatcher.light_results.source_references.is_some());
            operations.len() == expected_count
                && operations.iter().all(|operation| match operation {
                    crate::context_route::LightOperationResult::Query { result_digest, .. } => {
                        dispatcher
                            .light_results
                            .query
                            .as_ref()
                            .is_some_and(|results| {
                                serde_json::to_vec(results).is_ok_and(|bytes| {
                                    crate::hashing::content_hash_of_bytes(&bytes) == *result_digest
                                })
                            })
                    }
                    crate::context_route::LightOperationResult::SourceReference {
                        result_digest,
                        ..
                    } => dispatcher
                        .light_results
                        .source_references
                        .as_ref()
                        .is_some_and(|results| {
                            serde_json::to_vec(results).is_ok_and(|bytes| {
                                crate::hashing::content_hash_of_bytes(&bytes) == *result_digest
                            })
                        }),
                })
        }
        _ => false,
    };
    let light_results = light_results_match.then(|| std::mem::take(&mut dispatcher.light_results));
    let deep_hash = match execution.payload() {
        Some(crate::context_route::ContextPayload::DeepContextIr { result }) => {
            Some(result.context_hash.as_str())
        }
        _ => None,
    };
    let deep_context_ir = dispatcher
        .deep_context_ir
        .take()
        .filter(|document| deep_hash == Some(document.context_hash.as_str()));
    let acquired = match &execution {
        crate::context_route::ContextExecution::Completed { .. } => true,
        crate::context_route::ContextExecution::Partial { payload, .. } => {
            !matches!(payload, crate::context_route::ContextPayload::DirectNone {})
        }
        crate::context_route::ContextExecution::Blocked { .. }
        | crate::context_route::ContextExecution::Interrupted { .. } => false,
    };
    if acquired {
        if let (Some(task_session_id), Some(start_generation_id)) = (
            input.task_session_id.as_deref(),
            bound_start_generation_id.as_deref(),
        ) {
            crate::task_session::activate_legacy_task_session(
                dispatcher.connection,
                task_session_id,
                &crate::task_session::LegacyTaskSessionIdentity {
                    workspace_id: &workspace.workspace_id,
                    start_generation_id,
                    task_hash: &dispatcher.task_hash,
                    normalized_goal_hash: &dispatcher.normalized_goal_hash,
                    task_kind: dispatcher.task_kind,
                    kind_source: dispatcher.kind_source,
                    kind_rule_id: dispatcher.kind_rule_id.as_deref(),
                },
            )?;
        }
    }
    Ok(CatalogueContextApplicationOutput {
        execution,
        light_results,
        deep_context_ir,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct GovernorRunOutput {
    pub negotiated_versions: crate::context_route::NegotiatedVersions,
    pub execution: crate::context_route::ContextExecution,
    pub light_results: Option<CatalogueLightResults>,
    pub deep_context_ir: Option<crate::context_ir::DeepContextIrV2>,
}

/// Non-serializable proof that the CLI transport validated an explicit
/// materialization flag pair independently from the request document.
pub(crate) struct CliSourceMaterializationAuthorization(
    crate::task_compiler::DeepV2SourceMaterializationAuthorization,
);

impl CliSourceMaterializationAuthorization {
    pub(crate) fn new(max_bytes: u64) -> Result<Self> {
        Ok(Self(
            crate::task_compiler::DeepV2SourceMaterializationAuthorization::new(max_bytes)?,
        ))
    }
}

/// Execute a complete transport-neutral governor request without source
/// materialization. Serializable request fields never confer authority.
pub fn run_catalogue_governor(
    request: GovernorRunRequest,
    connection: &mut rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
) -> std::result::Result<GovernorRunOutput, CatalogueContextApplicationError> {
    if request.semantic.materialize_source || request.semantic.max_materialized_bytes.is_some() {
        return Err(AtlasError::InvalidConfig(
            "public governor request cannot grant source materialization authority".into(),
        )
        .into());
    }
    run_catalogue_governor_inner(request, connection, workspace, None)
}

/// Execute the CLI-authorized governor path. Only the CLI adapter can obtain
/// this seam's non-serializable provenance token.
pub(crate) fn run_catalogue_governor_from_cli(
    request: GovernorRunRequest,
    connection: &mut rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    authorization: Option<CliSourceMaterializationAuthorization>,
) -> std::result::Result<GovernorRunOutput, CatalogueContextApplicationError> {
    run_catalogue_governor_inner(
        request,
        connection,
        workspace,
        authorization.map(|authorization| authorization.0),
    )
}

fn run_catalogue_governor_inner(
    request: GovernorRunRequest,
    connection: &mut rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    materialization_authorization: Option<
        crate::task_compiler::DeepV2SourceMaterializationAuthorization,
    >,
) -> std::result::Result<GovernorRunOutput, CatalogueContextApplicationError> {
    let application = build_governor_run_request(request, connection, workspace)?;
    let negotiated_versions = application.negotiated_versions;
    let output = dispatch_catalogue_context_application_with_materialization(
        application.route_request,
        application.catalogue_input,
        connection,
        workspace,
        materialization_authorization,
    )?;
    Ok(GovernorRunOutput {
        negotiated_versions,
        execution: output.execution,
        light_results: output.light_results,
        deep_context_ir: output.deep_context_ir,
    })
}

pub const APPLICATION_CURSOR_VERSION: &str = "application-cursor-v1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationCursorOperation {
    TaskShow,
    CompiledContextShow,
    ContextYieldShow,
    ServingStatus,
    RetentionStatus,
    RetentionCompactPreview,
    UnregisterPreview,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplicationCursorFilter<'a> {
    TaskSession {
        task_session_id: &'a str,
    },
    ServingPolicy {
        serving_schema_version: &'a str,
        projection_policy_version: &'a str,
    },
    Retention {},
    Unregister {},
}

#[derive(Debug, Clone)]
pub struct ApplicationCursorBinding<'a> {
    pub workspace_id: &'a str,
    pub operation: ApplicationCursorOperation,
    pub contract_version: &'a str,
    pub filter: ApplicationCursorFilter<'a>,
    pub captured_state: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationCursorError {
    #[error("cursor_invalid")]
    Invalid,
    #[error("cursor_stale")]
    Stale,
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedApplicationCursor {
    pub captured_state: String,
    pub ordering_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationCursor {
    version: String,
    workspace_hash: String,
    catalogue_hash: String,
    operation: ApplicationCursorOperation,
    contract_version: String,
    filter_hash: String,
    captured_state: String,
    ordering_key: String,
}

fn application_cursor_catalogue_hash(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
) -> std::result::Result<String, ApplicationCursorError> {
    let database_path: Option<String> = connection
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let identity = database_path
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| workspace.catalogue_path.clone());
    Ok(crate::hashing::content_hash_of_bytes(identity.as_bytes()))
}

fn application_cursor_filter_hash(filter: &ApplicationCursorFilter<'_>) -> String {
    crate::hashing::content_hash_of_bytes(&crate::provider_contract::canonical_json_bytes(filter))
}

fn application_cursor_checksum(bytes: &[u8]) -> String {
    let mut input = b"workspace-atlas/application-cursor-v1\0".to_vec();
    input.extend_from_slice(bytes);
    crate::hashing::content_hash_of_bytes(&input)
}
pub(crate) struct ApplicationSnapshotHasher(sha2::Sha256);

impl ApplicationSnapshotHasher {
    pub(crate) fn new(domain: &[u8]) -> Self {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"workspace-atlas/application-snapshot-v1\0");
        hasher.update((domain.len() as u64).to_be_bytes());
        hasher.update(domain);
        Self(hasher)
    }

    pub(crate) fn record<T: serde::Serialize>(&mut self, value: &T) {
        use sha2::Digest;
        let bytes = crate::provider_contract::canonical_json_bytes(value);
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    pub(crate) fn finish(self) -> String {
        use sha2::Digest;
        hex::encode(self.0.finalize())
    }
}

pub fn encode_application_cursor(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    binding: &ApplicationCursorBinding<'_>,
    ordering_key: &str,
) -> std::result::Result<String, ApplicationCursorError> {
    if binding.workspace_id != workspace.workspace_id
        || ordering_key.is_empty()
        || ordering_key.len() > crate::context_route::MAX_APPLICATION_CURSOR_BYTES / 2
        || !ordering_key.is_ascii()
        || ordering_key.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ApplicationCursorError::Invalid);
    }
    let cursor = ApplicationCursor {
        version: APPLICATION_CURSOR_VERSION.to_string(),
        workspace_hash: crate::hashing::content_hash_of_bytes(workspace.workspace_id.as_bytes()),
        catalogue_hash: application_cursor_catalogue_hash(connection, workspace)?,
        operation: binding.operation,
        contract_version: binding.contract_version.to_string(),
        filter_hash: application_cursor_filter_hash(&binding.filter),
        captured_state: binding.captured_state.to_string(),
        ordering_key: ordering_key.to_string(),
    };
    let bytes = crate::provider_contract::canonical_json_bytes(&cursor);
    let token = format!(
        "{}.{}",
        hex::encode(&bytes),
        application_cursor_checksum(&bytes)
    );
    if token.len() > crate::context_route::MAX_APPLICATION_CURSOR_BYTES {
        return Err(ApplicationCursorError::Invalid);
    }
    Ok(token)
}

fn decode_application_cursor_token(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    binding: &ApplicationCursorBinding<'_>,
    token: &str,
) -> std::result::Result<DecodedApplicationCursor, ApplicationCursorError> {
    if token.len() > crate::context_route::MAX_APPLICATION_CURSOR_BYTES {
        return Err(ApplicationCursorError::Invalid);
    }
    let (encoded, checksum) = token
        .split_once('.')
        .ok_or(ApplicationCursorError::Invalid)?;
    let bytes = hex::decode(encoded).map_err(|_| ApplicationCursorError::Invalid)?;
    if application_cursor_checksum(&bytes) != checksum {
        return Err(ApplicationCursorError::Invalid);
    }
    let cursor: ApplicationCursor =
        serde_json::from_slice(&bytes).map_err(|_| ApplicationCursorError::Invalid)?;
    if crate::provider_contract::canonical_json_bytes(&cursor) != bytes
        || binding.workspace_id != workspace.workspace_id
        || cursor.version != APPLICATION_CURSOR_VERSION
        || cursor.workspace_hash
            != crate::hashing::content_hash_of_bytes(workspace.workspace_id.as_bytes())
        || cursor.catalogue_hash != application_cursor_catalogue_hash(connection, workspace)?
        || cursor.operation != binding.operation
        || cursor.contract_version != binding.contract_version
        || cursor.filter_hash != application_cursor_filter_hash(&binding.filter)
        || cursor.ordering_key.is_empty()
        || !cursor.ordering_key.is_ascii()
        || cursor
            .ordering_key
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(ApplicationCursorError::Invalid);
    }
    Ok(DecodedApplicationCursor {
        captured_state: cursor.captured_state,
        ordering_key: cursor.ordering_key,
    })
}

pub fn decode_application_cursor_snapshot(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    binding: &ApplicationCursorBinding<'_>,
    token: &str,
) -> std::result::Result<DecodedApplicationCursor, ApplicationCursorError> {
    decode_application_cursor_token(connection, workspace, binding, token)
}
pub fn decode_application_cursor(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    binding: &ApplicationCursorBinding<'_>,
    token: &str,
) -> std::result::Result<String, ApplicationCursorError> {
    let cursor = decode_application_cursor_token(connection, workspace, binding, token)?;
    if cursor.captured_state != binding.captured_state {
        return Err(ApplicationCursorError::Stale);
    }
    Ok(cursor.ordering_key)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledContextShowRequest {
    pub context_id: Option<String>,
    pub task_session_id: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CompiledContextShowPage {
    pub contexts: Vec<crate::context_ir::ContextIr>,
    pub next_cursor: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledContextWatermark {
    max_rowid: i64,
    row_count: i64,
    snapshot_digest: String,
    prefix_digest: String,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CompiledContextOrderingKey {
    created_at: String,
    context_id: String,
}
fn compiled_context_snapshot_digest(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    task_session_id: &str,
    max_rowid: i64,
    through: Option<&CompiledContextOrderingKey>,
) -> std::result::Result<(String, i64, Option<CompiledContextOrderingKey>), rusqlite::Error> {
    let mut statement = conn.prepare(
        "SELECT rowid, canonical_json, context_hash, context_id, created_at
         FROM context_ir
         WHERE workspace_id = ?1 AND task_session_id = ?2
           AND context_ir_version = '1.0.0' AND rowid <= ?3
           AND (?4 IS NULL OR created_at < ?4
                OR (created_at = ?4 AND context_id <= ?5))
         ORDER BY created_at, context_id, rowid",
    )?;
    let mut rows = statement.query(rusqlite::params![
        workspace_id,
        task_session_id,
        max_rowid,
        through.map(|key| key.created_at.as_str()),
        through.map(|key| key.context_id.as_str()),
    ])?;
    let mut digest = ApplicationSnapshotHasher::new(b"compiled-context");
    let mut count = 0_i64;
    let mut last = None;
    while let Some(row) = rows.next()? {
        let record = (
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        );
        last = Some(CompiledContextOrderingKey {
            context_id: record.3.clone(),
            created_at: record.4.clone(),
        });
        digest.record(&record);
        count += 1;
    }
    Ok((digest.finish(), count, last))
}

fn validate_json_bounds(value: &serde_json::Value, depth: usize) -> Result<()> {
    if depth > crate::context_route::MAX_APPLICATION_JSON_DEPTH {
        return Err(AtlasError::InvalidConfig(
            "stored Context IR exceeds the nested depth bound".into(),
        ));
    }
    match value {
        serde_json::Value::String(value)
            if value.len() > crate::context_route::MAX_APPLICATION_STRING_BYTES =>
        {
            Err(AtlasError::InvalidConfig(
                "stored Context IR exceeds the string bound".into(),
            ))
        }
        serde_json::Value::Array(values) => {
            if values.len() > crate::context_route::MAX_APPLICATION_NESTED_VECTOR_ITEMS {
                return Err(AtlasError::InvalidConfig(
                    "stored Context IR exceeds the nested vector bound".into(),
                ));
            }
            for value in values {
                validate_json_bounds(value, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            if values.len() > crate::context_route::MAX_APPLICATION_NESTED_VECTOR_ITEMS {
                return Err(AtlasError::InvalidConfig(
                    "stored Context IR exceeds the nested object bound".into(),
                ));
            }
            for (key, value) in values {
                if key.len() > crate::context_route::MAX_APPLICATION_STRING_BYTES {
                    return Err(AtlasError::InvalidConfig(
                        "stored Context IR exceeds the string bound".into(),
                    ));
                }
                validate_json_bounds(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validated_legacy_context_ir(
    document: String,
    stored_hash: &str,
    workspace: &workspace::WorkspaceRecord,
    expected_context_id: &str,
    expected_task_session_id: Option<&str>,
) -> Result<crate::context_ir::ContextIr> {
    if document.len() > crate::context_route::MAX_COMPILED_CONTEXT_DOCUMENT_BYTES {
        return Err(AtlasError::InvalidConfig(
            "stored Context IR exceeds the serialized document bound".into(),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&document)?;
    validate_json_bounds(&value, 1)?;
    let context: crate::context_ir::ContextIr = serde_json::from_value(value)?;
    if context.schema_version != crate::context_ir::CONTEXT_SCHEMA_VERSION
        || context.context_id != expected_context_id
        || context.workspace.workspace_id != workspace.workspace_id
        || expected_task_session_id
            .is_some_and(|session_id| context.task.task_session_id != session_id)
        || context.context_hash != stored_hash
        || serde_json::to_string(&context)? != document
    {
        return Err(AtlasError::InvalidConfig(
            "stored Context IR canonical contract is invalid".into(),
        ));
    }
    let resealed = crate::context_ir::ContextIr::seal(
        context.context_id.clone(),
        context.workspace.clone(),
        context.task.clone(),
        context.policy.clone(),
        context.status.clone(),
        context.working_set.clone(),
        context.relationships.clone(),
        context.effects.clone(),
        context.uncertainty.clone(),
        context.coverage.clone(),
        context.omissions.clone(),
        context.validation_plan.clone(),
        context.cost.clone(),
    );
    if resealed.context_hash != context.context_hash {
        return Err(AtlasError::InvalidConfig(
            "stored Context IR hash integrity is invalid".into(),
        ));
    }
    Ok(context)
}

/// Read one bounded, snapshot-stable page of validated legacy Context IR.
pub fn show_compiled_context(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    request: &CompiledContextShowRequest,
) -> std::result::Result<CompiledContextShowPage, CatalogueContextApplicationError> {
    if request.context_id.is_some() == request.task_session_id.is_some() {
        return Err(AtlasError::InvalidConfig(
            "exactly one context_id or task_session_id is required".into(),
        )
        .into());
    }
    if request.context_id.is_some() && request.cursor.is_some() {
        return Err(ApplicationCursorError::Invalid.into());
    }
    let limit = request
        .limit
        .unwrap_or(crate::context_route::DEFAULT_APPLICATION_PAGE_LIMIT);
    if limit == 0 || limit > crate::context_route::MAX_APPLICATION_PAGE_LIMIT {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        ))
        .into());
    }
    let tx = connection.unchecked_transaction()?;
    if let Some(context_id) = &request.context_id {
        let row = tx
            .query_row(
                "SELECT length(canonical_json), canonical_json, context_hash, task_session_id
                 FROM context_ir
                 WHERE workspace_id = ?1 AND context_id = ?2 AND context_ir_version = '1.0.0'",
                rusqlite::params![workspace.workspace_id, context_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let contexts = row
            .map(|(length, document, hash, task_session_id)| {
                if length < 0
                    || usize::try_from(length).ok()
                        > Some(crate::context_route::MAX_COMPILED_CONTEXT_DOCUMENT_BYTES)
                {
                    return Err(AtlasError::InvalidConfig(
                        "stored Context IR exceeds the serialized document bound".into(),
                    ));
                }
                validated_legacy_context_ir(
                    document,
                    &hash,
                    workspace,
                    context_id,
                    Some(&task_session_id),
                )
            })
            .transpose()?
            .into_iter()
            .collect();
        tx.commit()?;
        return Ok(CompiledContextShowPage {
            contexts,
            next_cursor: None,
        });
    }

    let task_session_id = request.task_session_id.as_deref().unwrap_or_default();
    let filter = ApplicationCursorFilter::TaskSession { task_session_id };
    let identity_binding = ApplicationCursorBinding {
        workspace_id: &workspace.workspace_id,
        operation: ApplicationCursorOperation::CompiledContextShow,
        contract_version: crate::context_ir::CONTEXT_SCHEMA_VERSION,
        filter,
        captured_state: "",
    };
    let (watermark, ordering) = if let Some(token) = &request.cursor {
        let decoded = decode_application_cursor_snapshot(&tx, workspace, &identity_binding, token)?;
        let watermark: CompiledContextWatermark = serde_json::from_str(&decoded.captured_state)
            .map_err(|_| ApplicationCursorError::Invalid)?;
        let ordering: CompiledContextOrderingKey = serde_json::from_str(&decoded.ordering_key)
            .map_err(|_| ApplicationCursorError::Invalid)?;
        let (snapshot_digest, count, _) = compiled_context_snapshot_digest(
            &tx,
            &workspace.workspace_id,
            task_session_id,
            watermark.max_rowid,
            None,
        )?;
        let (prefix_digest, _, last) = compiled_context_snapshot_digest(
            &tx,
            &workspace.workspace_id,
            task_session_id,
            watermark.max_rowid,
            Some(&ordering),
        )?;
        if count != watermark.row_count
            || snapshot_digest != watermark.snapshot_digest
            || prefix_digest != watermark.prefix_digest
            || last.as_ref() != Some(&ordering)
        {
            return Err(ApplicationCursorError::Stale.into());
        }
        (watermark, ordering)
    } else {
        let (max_rowid, row_count) = tx.query_row(
            "SELECT COALESCE(MAX(rowid), 0), COUNT(*) FROM context_ir
             WHERE workspace_id = ?1 AND task_session_id = ?2
               AND context_ir_version = '1.0.0'",
            rusqlite::params![workspace.workspace_id, task_session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (snapshot_digest, digest_count, _) = compiled_context_snapshot_digest(
            &tx,
            &workspace.workspace_id,
            task_session_id,
            max_rowid,
            None,
        )?;
        if digest_count != row_count {
            return Err(ApplicationCursorError::Stale.into());
        }
        (
            CompiledContextWatermark {
                max_rowid,
                row_count,
                snapshot_digest,
                prefix_digest: String::new(),
            },
            CompiledContextOrderingKey {
                created_at: String::new(),
                context_id: String::new(),
            },
        )
    };
    let fetch = i64::try_from(limit + 1)
        .map_err(|_| AtlasError::InvalidConfig("limit is too large".into()))?;
    let mut statement = tx.prepare(
        "SELECT length(canonical_json), canonical_json, context_hash, context_id, created_at
         FROM context_ir
         WHERE workspace_id = ?1 AND task_session_id = ?2
           AND context_ir_version = '1.0.0' AND rowid <= ?3
           AND (created_at > ?4 OR (created_at = ?4 AND context_id > ?5))
         ORDER BY created_at, context_id LIMIT ?6",
    )?;
    let mut rows = statement.query(rusqlite::params![
        workspace.workspace_id,
        task_session_id,
        watermark.max_rowid,
        ordering.created_at,
        ordering.context_id,
        fetch
    ])?;
    let mut contexts = Vec::with_capacity(limit + 1);
    let mut keys = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next()? {
        let length: i64 = row.get(0)?;
        if length < 0
            || usize::try_from(length).ok()
                > Some(crate::context_route::MAX_COMPILED_CONTEXT_DOCUMENT_BYTES)
        {
            return Err(AtlasError::InvalidConfig(
                "stored Context IR exceeds the serialized document bound".into(),
            )
            .into());
        }
        let document: String = row.get(1)?;
        let hash: String = row.get(2)?;
        let context_id: String = row.get(3)?;
        let created_at: String = row.get(4)?;
        contexts.push(validated_legacy_context_ir(
            document,
            &hash,
            workspace,
            &context_id,
            Some(task_session_id),
        )?);
        keys.push(CompiledContextOrderingKey {
            created_at,
            context_id,
        });
    }
    let has_more = contexts.len() > limit;
    contexts.truncate(limit);
    keys.truncate(limit);
    let next_cursor = if has_more {
        let last = keys
            .last()
            .ok_or_else(|| AtlasError::InvalidConfig("cursor_invalid".into()))?;
        let (prefix_digest, _, prefix_last) = compiled_context_snapshot_digest(
            &tx,
            &workspace.workspace_id,
            task_session_id,
            watermark.max_rowid,
            Some(last),
        )?;
        if prefix_last.as_ref() != Some(last) {
            return Err(ApplicationCursorError::Stale.into());
        }
        let state = serde_json::to_string(&CompiledContextWatermark {
            prefix_digest,
            ..watermark
        })?;
        let binding = ApplicationCursorBinding {
            captured_state: &state,
            ..identity_binding
        };
        Some(encode_application_cursor(
            &tx,
            workspace,
            &binding,
            &serde_json::to_string(last)?,
        )?)
    } else {
        None
    };
    drop(rows);
    drop(statement);
    tx.commit()?;
    Ok(CompiledContextShowPage {
        contexts,
        next_cursor,
    })
}

fn context_yield_metric(
    name: crate::context_yield::ContextYieldMetricName,
    metric: &crate::context_metrics::MetricFraction,
) -> crate::context_yield::ContextYieldRawMetric {
    use crate::context_metrics::{ContextMetricInvalidity, EvidenceClass, MetricUnit};
    use crate::context_yield::{
        ContextYieldEvidenceClass, ContextYieldMetricInvalidity, ContextYieldMetricUnit,
        ContextYieldRawMetric,
    };

    let unit = match metric.unit {
        MetricUnit::Items => ContextYieldMetricUnit::Items,
        MetricUnit::Requests => ContextYieldMetricUnit::Requests,
        MetricUnit::Bytes => ContextYieldMetricUnit::Bytes,
    };
    let evidence_class = match metric.evidence_class {
        EvidenceClass::AtlasObserved => ContextYieldEvidenceClass::AtlasObserved,
        EvidenceClass::ReportedUse => ContextYieldEvidenceClass::ReportedUse,
        EvidenceClass::ObservedAndReported => ContextYieldEvidenceClass::ObservedAndReported,
    };
    let invalidity = metric
        .invalidity
        .as_ref()
        .map(|invalidity| match invalidity {
            ContextMetricInvalidity::ZeroDenominator => {
                ContextYieldMetricInvalidity::ZeroDenominator
            }
            ContextMetricInvalidity::InvalidIdentity => {
                ContextYieldMetricInvalidity::InvalidIdentity
            }
            ContextMetricInvalidity::InvalidRange { .. } => {
                ContextYieldMetricInvalidity::InvalidRange
            }
            ContextMetricInvalidity::InvalidSourceBytes { .. } => {
                ContextYieldMetricInvalidity::InvalidSourceBytes
            }
            ContextMetricInvalidity::MissingEvidenceIdentity { .. } => {
                ContextYieldMetricInvalidity::MissingEvidenceIdentity
            }
            ContextMetricInvalidity::UnsupportedEntityKind { .. } => {
                ContextYieldMetricInvalidity::UnsupportedEntityKind
            }
            ContextMetricInvalidity::ConflictingSuppliedBytes { .. } => {
                ContextYieldMetricInvalidity::ConflictingSuppliedBytes
            }
            ContextMetricInvalidity::CapacityExceeded { .. } => {
                ContextYieldMetricInvalidity::CapacityExceeded
            }
            ContextMetricInvalidity::ArithmeticOverflow => {
                ContextYieldMetricInvalidity::ArithmeticOverflow
            }
        });
    ContextYieldRawMetric {
        name,
        numerator: metric.numerator,
        denominator: metric.denominator,
        unit,
        evidence_class,
        invalidity,
    }
}

fn project_context_yield_report_v15(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    session: &crate::context_ir::TaskSession,
    metrics: &crate::context_metrics::ContextMetrics,
) -> Result<Option<crate::context_yield::ContextYieldReportV15>> {
    use crate::context_ir::TaskSessionState;
    use crate::context_yield::{
        ContextYieldExecutionMetadata, ContextYieldLimitation, ContextYieldMetricName,
        ContextYieldObservationCoverage, ContextYieldOutcome, ContextYieldOutcomeAcceptance,
        ContextYieldRawMeasures, ContextYieldRawSample, ContextYieldReportContentV15,
        ContextYieldReportV15, ContextYieldValidity, CONTEXT_YIELD_REPORT_SCHEMA_VERSION,
    };

    if session.state != TaskSessionState::Completed || session.accepted != Some(true) {
        return Ok(None);
    }
    let completed_at = session.completed_at.as_deref().ok_or_else(|| {
        AtlasError::InvalidConfig("accepted completed task has no completion time".into())
    })?;
    let end_generation_id = session.end_generation_id.as_deref().ok_or_else(|| {
        AtlasError::InvalidConfig("accepted completed task has no end generation".into())
    })?;
    let end_tree_hash = session.end_tree_hash.as_deref().ok_or_else(|| {
        AtlasError::InvalidConfig("accepted completed task has no end tree hash".into())
    })?;
    let outcome_code = session.outcome_code.as_deref().ok_or_else(|| {
        AtlasError::InvalidConfig("accepted completed task has no outcome code".into())
    })?;

    let stored = connection
        .query_row(
            "SELECT length(canonical_json), canonical_json, context_hash, context_id
             FROM context_ir
             WHERE workspace_id = ?1 AND task_session_id = ?2
               AND generation_id = ?3 AND context_ir_version = '1.0.0'
               AND working_set_status != 'blocked'
             ORDER BY created_at DESC, context_id DESC
             LIMIT 1",
            rusqlite::params![
                workspace.workspace_id,
                session.task_session_id,
                session.start_generation_id
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((length, document, context_hash, context_id)) = stored else {
        return Ok(None);
    };
    if length < 0
        || usize::try_from(length).ok()
            > Some(crate::context_route::MAX_COMPILED_CONTEXT_DOCUMENT_BYTES)
    {
        return Err(AtlasError::InvalidConfig(
            "stored Context IR exceeds the serialized document bound".into(),
        ));
    }
    let context = validated_legacy_context_ir(
        document,
        &context_hash,
        workspace,
        &context_id,
        Some(&session.task_session_id),
    )?;

    let raw_metrics = vec![
        context_yield_metric(
            ContextYieldMetricName::ContextPrecisionObserved,
            &metrics.context_precision_observed,
        ),
        context_yield_metric(
            ContextYieldMetricName::ContextPrecisionReported,
            &metrics.context_precision_reported,
        ),
        context_yield_metric(
            ContextYieldMetricName::ContextExpansionCount,
            &metrics.context_expansion_count,
        ),
        context_yield_metric(
            ContextYieldMetricName::RediscoveryRate,
            &metrics.rediscovery_rate,
        ),
        context_yield_metric(
            ContextYieldMetricName::SourceEfficiencyObserved,
            &metrics.source_efficiency_observed,
        ),
    ];
    let sample = ContextYieldRawSample {
        context_hash: context.context_hash,
        working_set_size: i64::try_from(context.working_set.len())
            .map_err(|_| AtlasError::Other("Context Yield working-set size overflow".into()))?,
        selected_source_bytes: context.cost.selected_source_bytes,
        selected_estimated_tokens: context.cost.selected_estimated_tokens,
        serving_fallback: context.status.serving_fallback,
        truncated: context.omissions.truncated,
    };
    let observation_coverage = ContextYieldObservationCoverage {
        observed_events: metrics.context_precision_observed.numerator,
        eligible_events: metrics.context_precision_observed.denominator,
        complete: metrics.context_precision_observed.numerator
            == metrics.context_precision_observed.denominator,
    };
    let mut limitations = Vec::new();
    if !observation_coverage.complete {
        limitations.push(ContextYieldLimitation::PartialObservationCoverage);
    }
    limitations.push(ContextYieldLimitation::SmallSampleSize);
    if sample.serving_fallback {
        limitations.push(ContextYieldLimitation::ServingFallback);
    }
    if sample.truncated {
        limitations.push(ContextYieldLimitation::TruncatedContext);
    }
    limitations.push(ContextYieldLimitation::SingleEnvironment);
    let is_valid = raw_metrics.iter().all(|metric| metric.invalidity.is_none());
    let outcome_bytes = serde_json::to_vec(&(
        "context-yield-accepted-outcome-v1",
        &session.task_session_id,
        end_generation_id,
        end_tree_hash,
        session.tests_passed,
        outcome_code,
    ))?;
    let outcome_hash = crate::hashing::content_hash_of_bytes(&outcome_bytes);
    let report_id = crate::resolution::deterministic_id(
        "yield",
        &[
            &workspace.workspace_id,
            &session.start_generation_id,
            &session.task_session_id,
            &session.task_hash,
            &outcome_hash,
            &sample.context_hash,
        ],
    );
    let run_id = crate::resolution::deterministic_id("run", &[&report_id]);

    Ok(Some(ContextYieldReportV15 {
        schema_version: CONTEXT_YIELD_REPORT_SCHEMA_VERSION.to_string(),
        content: ContextYieldReportContentV15 {
            report_id,
            workspace_id: workspace.workspace_id.clone(),
            generation_id: session.start_generation_id.clone(),
            task_hash: session.task_hash.clone(),
            task_kind: session.task_kind,
            planner_policy_version: context.policy.planner_policy_version,
            projection_policy_version: context.policy.projection_policy_version,
            estimator_policy_version: "estimator-v1".to_string(),
            metric_policy_version: "context-yield-v1.5".to_string(),
            experimental_profile: None,
            accepted_outcome: ContextYieldOutcome {
                acceptance: ContextYieldOutcomeAcceptance::Accepted,
                outcome_hash,
            },
            sample_size: 1,
            raw_measures: ContextYieldRawMeasures {
                samples: vec![sample],
                metrics: raw_metrics,
            },
            validity: ContextYieldValidity {
                is_valid,
                observation_coverage,
                limitations,
            },
        },
        execution: ContextYieldExecutionMetadata {
            run_id,
            generated_at: completed_at.to_string(),
        },
    }))
}

#[derive(Debug, serde::Serialize)]
pub struct ContextYieldShowPage {
    pub task_session: crate::context_ir::TaskSession,
    pub events: Vec<crate::context_ir::ContextUseEvent>,
    pub metrics: serde_json::Value,
    pub report: Option<crate::context_yield::ContextYieldReportV15>,
    pub next_cursor: Option<String>,
}

/// Project bounded persisted lifecycle evidence into a Context Yield page.
pub fn show_context_yield(
    connection: &rusqlite::Connection,
    workspace: &workspace::WorkspaceRecord,
    request: &crate::task_session::LegacyTaskShowRequest,
) -> std::result::Result<ContextYieldShowPage, CatalogueContextApplicationError> {
    let snapshot = crate::task_session::show_legacy_task_session_snapshot(
        connection,
        workspace,
        request,
        ApplicationCursorOperation::ContextYieldShow,
        true,
    )?;
    let page = snapshot.page;
    let metrics = snapshot.metrics.ok_or_else(|| {
        AtlasError::Other("context-yield metrics snapshot was not captured".into())
    })?;
    let report =
        project_context_yield_report_v15(connection, workspace, &page.task_session, &metrics)?;
    let metric = |value: &crate::context_metrics::MetricFraction| {
        serde_json::json!({
            "numerator": value.numerator,
            "denominator": value.denominator,
        })
    };
    let metrics = serde_json::json!({
        "supplied_count": metrics.supplied.len(),
        "observed_count": metrics.observed.len(),
        "reported_count": metrics.reported.len(),
        "changed_count": metrics.changed.len(),
        "expanded_count": metrics.expanded.len(),
        "rediscovered_count": metrics.rediscovered.len(),
        "context_precision_observed": metric(&metrics.context_precision_observed),
        "context_precision_reported": metric(&metrics.context_precision_reported),
        "context_expansion_count": metric(&metrics.context_expansion_count),
        "rediscovery_rate": metric(&metrics.rediscovery_rate),
        "source_efficiency_observed": metric(&metrics.source_efficiency_observed),
    });
    Ok(ContextYieldShowPage {
        task_session: page.task_session,
        events: page.events,
        metrics,
        report,
        next_cursor: page.next_cursor,
    })
}
