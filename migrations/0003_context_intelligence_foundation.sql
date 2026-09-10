-- Workspace Atlas V1.2 Context Intelligence Foundation
--
-- Additive to 0001/0002. Serving Plane projections, Context IR,
-- task sessions, context-use telemetry, Generation Delta, evidence
-- leases, and query-stage metrics.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS serving_generation (
    serving_generation_id       TEXT PRIMARY KEY,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id               TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    serving_schema_version      TEXT NOT NULL,
    projection_policy_version   TEXT NOT NULL,
    state                       TEXT NOT NULL CHECK(state IN (
                                    'building', 'ready', 'failed', 'stale', 'deleted'
                                )),
    source_fact_fingerprint     TEXT NOT NULL,
    card_count                  INTEGER NOT NULL DEFAULT 0 CHECK(card_count >= 0),
    edge_count                  INTEGER NOT NULL DEFAULT 0 CHECK(edge_count >= 0),
    coverage_rollup_count       INTEGER NOT NULL DEFAULT 0 CHECK(coverage_rollup_count >= 0),
    build_started_at            TEXT NOT NULL,
    build_finished_at           TEXT,
    failure_code                TEXT,
    failure_message             TEXT,
    UNIQUE(workspace_id, generation_id, serving_schema_version, projection_policy_version)
);

CREATE INDEX IF NOT EXISTS idx_serving_generation_lookup
ON serving_generation(workspace_id, generation_id, state);

CREATE TABLE IF NOT EXISTS symbol_serving_projection (
    serving_generation_id       TEXT NOT NULL REFERENCES serving_generation(serving_generation_id) ON DELETE CASCADE,
    canonical_symbol_key        TEXT NOT NULL,
    symbol_fact_id              TEXT REFERENCES symbol_fact(symbol_fact_id) ON DELETE SET NULL,
    file_revision_id            TEXT REFERENCES file_revision(revision_id) ON DELETE SET NULL,
    canonical_path              TEXT NOT NULL,
    symbol_kind                 TEXT NOT NULL,
    language                    TEXT,
    signature                   TEXT,
    start_byte                  INTEGER NOT NULL CHECK(start_byte >= 0),
    end_byte                    INTEGER NOT NULL CHECK(end_byte >= start_byte),
    start_line                  INTEGER NOT NULL CHECK(start_line >= 1),
    end_line                    INTEGER NOT NULL CHECK(end_line >= start_line),
    preferred_fact_id           TEXT NOT NULL,
    preferred_provider_tier     TEXT NOT NULL,
    preferred_confidence        REAL NOT NULL CHECK(preferred_confidence >= 0.0 AND preferred_confidence <= 1.0),
    evidence_state              TEXT NOT NULL,
    caller_count                INTEGER NOT NULL DEFAULT 0 CHECK(caller_count >= 0),
    callee_count                INTEGER NOT NULL DEFAULT 0 CHECK(callee_count >= 0),
    reference_count             INTEGER NOT NULL DEFAULT 0 CHECK(reference_count >= 0),
    test_count                  INTEGER NOT NULL DEFAULT 0 CHECK(test_count >= 0),
    config_count                INTEGER NOT NULL DEFAULT 0 CHECK(config_count >= 0),
    effect_count                INTEGER NOT NULL DEFAULT 0 CHECK(effect_count >= 0),
    conflict_count              INTEGER NOT NULL DEFAULT 0 CHECK(conflict_count >= 0),
    unresolved_count            INTEGER NOT NULL DEFAULT 0 CHECK(unresolved_count >= 0),
    coverage_state              TEXT NOT NULL CHECK(coverage_state IN (
                                    'complete', 'partial', 'unsupported', 'failed'
                                )),
    source_bytes                INTEGER NOT NULL DEFAULT 0 CHECK(source_bytes >= 0),
    estimated_tokens            INTEGER NOT NULL DEFAULT 0 CHECK(estimated_tokens >= 0),
    metadata_bytes              INTEGER NOT NULL DEFAULT 0 CHECK(metadata_bytes >= 0),
    compact_json                TEXT NOT NULL,
    card_hash                   TEXT NOT NULL,
    PRIMARY KEY(serving_generation_id, canonical_symbol_key)
);

CREATE INDEX IF NOT EXISTS idx_symbol_serving_path
ON symbol_serving_projection(serving_generation_id, canonical_path, start_byte);

CREATE INDEX IF NOT EXISTS idx_symbol_serving_kind
ON symbol_serving_projection(serving_generation_id, symbol_kind, canonical_symbol_key);

CREATE TABLE IF NOT EXISTS relationship_serving_edge (
    serving_generation_id       TEXT NOT NULL REFERENCES serving_generation(serving_generation_id) ON DELETE CASCADE,
    edge_id                     TEXT NOT NULL,
    source_entity_id            TEXT NOT NULL,
    target_entity_id            TEXT NOT NULL,
    relationship_type           TEXT NOT NULL,
    resolution_state            TEXT NOT NULL CHECK(resolution_state IN (
                                    'resolved_symbol', 'resolved_file', 'external',
                                    'ambiguous', 'unresolved', 'invalid', 'stale'
                                )),
    trust_class                 TEXT NOT NULL CHECK(trust_class IN (
                                    'verified', 'supported', 'inferred', 'uncertain'
                                )),
    preferred_fact_id           TEXT,
    provider_tier               TEXT,
    confidence                  REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    canonical_path              TEXT,
    start_byte                  INTEGER CHECK(start_byte IS NULL OR start_byte >= 0),
    end_byte                    INTEGER CHECK(
                                    end_byte IS NULL OR
                                    (start_byte IS NOT NULL AND end_byte >= start_byte)
                                ),
    stable_order_key            TEXT NOT NULL,
    edge_json                   TEXT NOT NULL,
    PRIMARY KEY(serving_generation_id, edge_id)
);

CREATE INDEX IF NOT EXISTS idx_serving_edge_source
ON relationship_serving_edge(
    serving_generation_id, source_entity_id, relationship_type, stable_order_key
);

CREATE INDEX IF NOT EXISTS idx_serving_edge_target
ON relationship_serving_edge(
    serving_generation_id, target_entity_id, relationship_type, stable_order_key
);

CREATE TABLE IF NOT EXISTS serving_coverage_rollup (
    serving_generation_id       TEXT NOT NULL REFERENCES serving_generation(serving_generation_id) ON DELETE CASCADE,
    scope_kind                  TEXT NOT NULL CHECK(scope_kind IN (
                                    'workspace', 'project', 'package', 'file',
                                    'provider', 'capability'
                                )),
    scope_key                   TEXT NOT NULL,
    eligible_count              INTEGER NOT NULL DEFAULT 0 CHECK(eligible_count >= 0),
    complete_count              INTEGER NOT NULL DEFAULT 0 CHECK(complete_count >= 0),
    partial_count               INTEGER NOT NULL DEFAULT 0 CHECK(partial_count >= 0),
    unsupported_count           INTEGER NOT NULL DEFAULT 0 CHECK(unsupported_count >= 0),
    excluded_count              INTEGER NOT NULL DEFAULT 0 CHECK(excluded_count >= 0),
    failed_count                INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
    unresolved_count            INTEGER NOT NULL DEFAULT 0 CHECK(unresolved_count >= 0),
    ambiguous_count             INTEGER NOT NULL DEFAULT 0 CHECK(ambiguous_count >= 0),
    conflict_count              INTEGER NOT NULL DEFAULT 0 CHECK(conflict_count >= 0),
    details_json                TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(serving_generation_id, scope_kind, scope_key)
);

CREATE TABLE IF NOT EXISTS task_session (
    task_session_id             TEXT PRIMARY KEY,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    start_generation_id         TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE RESTRICT,
    end_generation_id           TEXT REFERENCES index_generation(generation_id) ON DELETE SET NULL,
    task_hash                   TEXT NOT NULL,
    normalized_goal_hash        TEXT NOT NULL,
    raw_task_retention          TEXT NOT NULL CHECK(raw_task_retention IN (
                                    'none', 'hash_only', 'local_opt_in'
                                )),
    raw_task                    TEXT,
    task_kind                   TEXT NOT NULL CHECK(task_kind IN (
                                    'explore', 'bug_fix', 'behavior_change', 'api_change',
                                    'refactor', 'configuration_change', 'test_change',
                                    'review', 'audit', 'unknown'
                                )),
    task_kind_source            TEXT NOT NULL CHECK(task_kind_source IN (
                                    'declared', 'deterministic_rule', 'unknown'
                                )),
    task_kind_rule_id           TEXT,
    planner_policy_version      TEXT NOT NULL,
    context_ir_version          TEXT NOT NULL,
    state                       TEXT NOT NULL CHECK(state IN (
                                    'created', 'context_compiled', 'active',
                                    'reconciled', 'completed', 'abandoned', 'failed'
                                )),
    accepted                    INTEGER CHECK(accepted IS NULL OR accepted IN (0,1)),
    tests_passed                INTEGER CHECK(tests_passed IS NULL OR tests_passed IN (0,1)),
    outcome_code                TEXT,
    start_tree_hash             TEXT,
    end_tree_hash               TEXT,
    created_at                  TEXT NOT NULL,
    completed_at                TEXT,
    CHECK(raw_task_retention = 'local_opt_in' OR raw_task IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_task_session_workspace_time
ON task_session(workspace_id, created_at DESC);

CREATE TABLE IF NOT EXISTS context_ir (
    context_id                  TEXT PRIMARY KEY,
    task_session_id             TEXT NOT NULL REFERENCES task_session(task_session_id) ON DELETE CASCADE,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id               TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE RESTRICT,
    serving_generation_id       TEXT REFERENCES serving_generation(serving_generation_id) ON DELETE SET NULL,
    context_ir_version          TEXT NOT NULL,
    planner_policy_version      TEXT NOT NULL,
    projection_policy_version   TEXT NOT NULL,
    budget_profile              TEXT NOT NULL,
    request_hash                TEXT NOT NULL,
    working_set_status          TEXT NOT NULL CHECK(working_set_status IN (
                                    'complete', 'partial', 'blocked'
                                )),
    serving_fallback            INTEGER NOT NULL CHECK(serving_fallback IN (0,1)),
    max_records                 INTEGER NOT NULL CHECK(max_records >= 1),
    selected_records            INTEGER NOT NULL CHECK(selected_records >= 0),
    max_source_bytes            INTEGER NOT NULL CHECK(max_source_bytes >= 0),
    selected_source_bytes       INTEGER NOT NULL CHECK(selected_source_bytes >= 0),
    max_estimated_tokens        INTEGER NOT NULL CHECK(max_estimated_tokens >= 0),
    selected_estimated_tokens   INTEGER NOT NULL CHECK(selected_estimated_tokens >= 0),
    soft_latency_ms             INTEGER NOT NULL CHECK(soft_latency_ms >= 0),
    hard_latency_ms             INTEGER NOT NULL CHECK(hard_latency_ms >= 1),
    elapsed_ms                  INTEGER NOT NULL CHECK(elapsed_ms >= 0),
    coverage_json               TEXT NOT NULL,
    omissions_json              TEXT NOT NULL,
    uncertainty_json            TEXT NOT NULL,
    validation_plan_json        TEXT NOT NULL,
    canonical_json              TEXT NOT NULL,
    context_hash                TEXT NOT NULL,
    created_at                  TEXT NOT NULL,
    UNIQUE(
        workspace_id, generation_id, task_session_id,
        request_hash, planner_policy_version,
        projection_policy_version, budget_profile
    )
);

CREATE INDEX IF NOT EXISTS idx_context_ir_session
ON context_ir(task_session_id, created_at);

CREATE TABLE IF NOT EXISTS context_ir_item (
    context_id                  TEXT NOT NULL REFERENCES context_ir(context_id) ON DELETE CASCADE,
    ordinal                     INTEGER NOT NULL CHECK(ordinal >= 0),
    item_id                     TEXT NOT NULL,
    entity_kind                 TEXT NOT NULL,
    entity_id                   TEXT NOT NULL,
    role                        TEXT NOT NULL,
    selection_reason            TEXT NOT NULL,
    origin_id                   TEXT,
    graph_distance              INTEGER NOT NULL CHECK(graph_distance >= 0),
    evidence_state              TEXT NOT NULL,
    confidence                  REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    provider_fingerprint        TEXT,
    source_revision_hash        TEXT,
    metadata_bytes              INTEGER NOT NULL DEFAULT 0 CHECK(metadata_bytes >= 0),
    source_bytes                INTEGER NOT NULL DEFAULT 0 CHECK(source_bytes >= 0),
    estimated_tokens            INTEGER NOT NULL DEFAULT 0 CHECK(estimated_tokens >= 0),
    source_path                 TEXT,
    source_start_byte           INTEGER CHECK(source_start_byte IS NULL OR source_start_byte >= 0),
    source_end_byte             INTEGER CHECK(
                                    source_end_byte IS NULL OR
                                    (source_start_byte IS NOT NULL AND source_end_byte >= source_start_byte)
                                ),
    item_json                   TEXT NOT NULL,
    PRIMARY KEY(context_id, ordinal),
    UNIQUE(context_id, item_id)
);

CREATE INDEX IF NOT EXISTS idx_context_ir_item_entity
ON context_ir_item(context_id, entity_id);

CREATE TABLE IF NOT EXISTS context_use_event (
    event_id                    TEXT PRIMARY KEY,
    task_session_id             TEXT NOT NULL REFERENCES task_session(task_session_id) ON DELETE CASCADE,
    context_id                  TEXT REFERENCES context_ir(context_id) ON DELETE SET NULL,
    item_id                     TEXT,
    entity_id                   TEXT,
    event_type                  TEXT NOT NULL CHECK(event_type IN (
                                    'context_supplied', 'exact_source_requested',
                                    'entity_queried', 'relationship_traversed',
                                    'context_recompiled', 'artifact_changed',
                                    'validation_selected', 'reasoning_use_reported',
                                    'modification_target_reported',
                                    'test_considered_reported', 'outcome_recorded'
                                )),
    observation_source          TEXT NOT NULL CHECK(observation_source IN (
                                    'atlas_observed', 'agent_reported', 'operator_reported'
                                )),
    byte_count                  INTEGER CHECK(byte_count IS NULL OR byte_count >= 0),
    details_json                TEXT NOT NULL DEFAULT '{}',
    occurred_at                 TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_context_use_session_time
ON context_use_event(task_session_id, occurred_at, event_id);

CREATE TABLE IF NOT EXISTS generation_delta (
    delta_id                    TEXT PRIMARY KEY,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    from_generation_id          TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE RESTRICT,
    to_generation_id            TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE RESTRICT,
    delta_policy_version        TEXT NOT NULL,
    state                       TEXT NOT NULL CHECK(state IN (
                                    'building', 'ready', 'failed'
                                )),
    canonical_json              TEXT,
    delta_hash                  TEXT,
    created_at                  TEXT NOT NULL,
    finished_at                 TEXT,
    failure_code                TEXT,
    failure_message             TEXT,
    CHECK(from_generation_id <> to_generation_id),
    UNIQUE(workspace_id, from_generation_id, to_generation_id, delta_policy_version)
);

CREATE TABLE IF NOT EXISTS generation_delta_item (
    delta_id                    TEXT NOT NULL REFERENCES generation_delta(delta_id) ON DELETE CASCADE,
    ordinal                     INTEGER NOT NULL CHECK(ordinal >= 0),
    category                    TEXT NOT NULL CHECK(category IN (
                                    'file', 'symbol', 'relationship', 'effect',
                                    'coverage', 'conflict', 'unchanged_contract',
                                    'uncertainty'
                                )),
    entity_id                   TEXT NOT NULL,
    change_kind                 TEXT NOT NULL CHECK(change_kind IN (
                                    'added', 'removed', 'modified', 'renamed',
                                    'retargeted', 'state_changed', 'unchanged',
                                    'uncertain'
                                )),
    evidence_state              TEXT NOT NULL CHECK(evidence_state IN (
                                    'verified', 'supported', 'inferred', 'uncertain'
                                )),
    before_id                   TEXT,
    after_id                    TEXT,
    reason_codes_json           TEXT NOT NULL DEFAULT '[]',
    item_json                   TEXT NOT NULL,
    PRIMARY KEY(delta_id, ordinal)
);

CREATE INDEX IF NOT EXISTS idx_generation_delta_item_entity
ON generation_delta_item(delta_id, category, entity_id);

CREATE TABLE IF NOT EXISTS evidence_lease (
    lease_id                    TEXT PRIMARY KEY,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id               TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    evidence_kind               TEXT NOT NULL,
    evidence_id                 TEXT NOT NULL,
    source_revision_hash        TEXT,
    provider_fingerprint        TEXT,
    resolver_policy_version     TEXT,
    projection_policy_version   TEXT,
    state                       TEXT NOT NULL CHECK(state IN (
                                    'valid', 'invalidated', 'superseded'
                                )),
    invalidation_reason         TEXT,
    created_at                  TEXT NOT NULL,
    invalidated_at              TEXT,
    UNIQUE(
        workspace_id, generation_id, evidence_kind, evidence_id,
        source_revision_hash, provider_fingerprint,
        resolver_policy_version, projection_policy_version
    )
);

CREATE INDEX IF NOT EXISTS idx_evidence_lease_lookup
ON evidence_lease(workspace_id, generation_id, evidence_kind, evidence_id, state);

CREATE TABLE IF NOT EXISTS query_stage_metric (
    metric_id                   TEXT PRIMARY KEY,
    workspace_id                TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id               TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    task_session_id             TEXT REFERENCES task_session(task_session_id) ON DELETE SET NULL,
    context_id                  TEXT REFERENCES context_ir(context_id) ON DELETE SET NULL,
    request_id                  TEXT NOT NULL,
    operation                   TEXT NOT NULL,
    serving_fallback            INTEGER NOT NULL CHECK(serving_fallback IN (0,1)),
    seed_resolution_us          INTEGER NOT NULL DEFAULT 0 CHECK(seed_resolution_us >= 0),
    serving_lookup_us           INTEGER NOT NULL DEFAULT 0 CHECK(serving_lookup_us >= 0),
    graph_expansion_us          INTEGER NOT NULL DEFAULT 0 CHECK(graph_expansion_us >= 0),
    ranking_us                  INTEGER NOT NULL DEFAULT 0 CHECK(ranking_us >= 0),
    source_verify_us            INTEGER NOT NULL DEFAULT 0 CHECK(source_verify_us >= 0),
    source_read_us              INTEGER NOT NULL DEFAULT 0 CHECK(source_read_us >= 0),
    packing_us                  INTEGER NOT NULL DEFAULT 0 CHECK(packing_us >= 0),
    candidate_count             INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
    expanded_edge_count         INTEGER NOT NULL DEFAULT 0 CHECK(expanded_edge_count >= 0),
    source_bytes_read           INTEGER NOT NULL DEFAULT 0 CHECK(source_bytes_read >= 0),
    returned_records            INTEGER NOT NULL DEFAULT 0 CHECK(returned_records >= 0),
    returned_estimated_tokens   INTEGER NOT NULL DEFAULT 0 CHECK(returned_estimated_tokens >= 0),
    cache_hits                  INTEGER NOT NULL DEFAULT 0 CHECK(cache_hits >= 0),
    cache_misses                INTEGER NOT NULL DEFAULT 0 CHECK(cache_misses >= 0),
    truncated                   INTEGER NOT NULL CHECK(truncated IN (0,1)),
    created_at                  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_query_metric_session
ON query_stage_metric(task_session_id, created_at);

CREATE VIEW IF NOT EXISTS ready_serving_generation AS
SELECT sg.*
FROM serving_generation sg
JOIN workspace w ON w.workspace_id = sg.workspace_id
WHERE sg.generation_id = w.active_generation_id
  AND sg.state = 'ready';
