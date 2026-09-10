-- Workspace Atlas catalogue migration 0002
-- Semantic provider foundation. Additive to 0001_foundation.sql.
-- The application migration runner must execute this under the writer lease
-- and a transaction, then record and verify an embedded SHA-256 checksum.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS migration_integrity (
    version                 INTEGER PRIMARY KEY
                            REFERENCES schema_migration(version) ON DELETE CASCADE,
    migration_name          TEXT NOT NULL,
    sha256                  TEXT NOT NULL
                            CHECK(length(sha256) = 64),
    verified_at             TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS provider_descriptor (
    provider_key            TEXT PRIMARY KEY,
    provider_name           TEXT NOT NULL,
    provider_version        TEXT NOT NULL,
    provider_tier           TEXT NOT NULL CHECK(provider_tier IN (
                                'compiler',
                                'semantic_index',
                                'language_server',
                                'structural',
                                'document_config',
                                'textual',
                                'heuristic',
                                'runtime_observation'
                            )),
    execution_kind          TEXT NOT NULL CHECK(execution_kind IN (
                                'builtin',
                                'external_process'
                            )),
    execution_scope         TEXT NOT NULL CHECK(execution_scope IN (
                                'file',
                                'project',
                                'package',
                                'workspace'
                            )),
    protocol_version        TEXT NOT NULL,
    output_format           TEXT NOT NULL CHECK(output_format IN (
                                'normalized_json',
                                'scip',
                                'builtin'
                            )),
    executable_identity     TEXT,
    executable_hash         TEXT,
    deterministic           INTEGER NOT NULL CHECK(deterministic IN (0, 1)),
    capabilities_json       TEXT NOT NULL DEFAULT '[]',
    limitations_json        TEXT NOT NULL DEFAULT '[]',
    descriptor_hash         TEXT NOT NULL CHECK(length(descriptor_hash) = 64),
    registered_at           TEXT NOT NULL,
    UNIQUE(provider_name, provider_version, descriptor_hash)
);

CREATE INDEX IF NOT EXISTS idx_provider_descriptor_name_version
    ON provider_descriptor(provider_name, provider_version);

CREATE TABLE IF NOT EXISTS workspace_provider (
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    provider_key            TEXT NOT NULL
                            REFERENCES provider_descriptor(provider_key) ON DELETE RESTRICT,
    enabled                 INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    required                INTEGER NOT NULL CHECK(required IN (0, 1)),
    priority                INTEGER NOT NULL,
    configuration_json      TEXT NOT NULL DEFAULT '{}',
    configuration_hash      TEXT NOT NULL CHECK(length(configuration_hash) = 64),
    timeout_ms              INTEGER NOT NULL CHECK(timeout_ms > 0),
    max_stdout_bytes        INTEGER NOT NULL CHECK(max_stdout_bytes >= 0),
    max_stderr_bytes        INTEGER NOT NULL CHECK(max_stderr_bytes >= 0),
    max_output_bytes        INTEGER NOT NULL CHECK(max_output_bytes > 0),
    network_isolation_policy TEXT NOT NULL CHECK(network_isolation_policy IN (
                                'not_required',
                                'require_enforced',
                                'best_effort_allowed'
                            )),
    updated_at              TEXT NOT NULL,
    PRIMARY KEY(workspace_id, provider_key)
);

CREATE INDEX IF NOT EXISTS idx_workspace_provider_enabled
    ON workspace_provider(workspace_id, enabled, priority DESC);

CREATE TABLE IF NOT EXISTS project_scope (
    project_scope_id        TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL
                            REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    scope_kind              TEXT NOT NULL CHECK(scope_kind IN (
                                'project',
                                'package',
                                'workspace'
                            )),
    canonical_root          TEXT NOT NULL,
    primary_manifest_path   TEXT,
    primary_manifest_hash   TEXT,
    language_family         TEXT,
    owning_policy_version   TEXT NOT NULL,
    input_manifest_hash     TEXT NOT NULL CHECK(length(input_manifest_hash) = 64),
    status                  TEXT NOT NULL CHECK(status IN (
                                'active',
                                'ambiguous',
                                'unsupported',
                                'excluded',
                                'invalid'
                            )),
    details_json            TEXT NOT NULL DEFAULT '{}',
    created_at              TEXT NOT NULL,
    UNIQUE(workspace_id, generation_id, scope_kind, canonical_root)
);

CREATE INDEX IF NOT EXISTS idx_project_scope_generation
    ON project_scope(workspace_id, generation_id, status);

CREATE TABLE IF NOT EXISTS provider_execution (
    provider_execution_id   TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL
                            REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    provider_key            TEXT NOT NULL
                            REFERENCES provider_descriptor(provider_key) ON DELETE RESTRICT,
    project_scope_id        TEXT
                            REFERENCES project_scope(project_scope_id) ON DELETE SET NULL,
    scope_kind              TEXT NOT NULL CHECK(scope_kind IN (
                                'file',
                                'project',
                                'package',
                                'workspace'
                            )),
    scope_key               TEXT NOT NULL,
    input_fingerprint       TEXT NOT NULL CHECK(length(input_fingerprint) = 64),
    provider_set_hash       TEXT NOT NULL CHECK(length(provider_set_hash) = 64),
    normalized_schema_version TEXT NOT NULL,
    mapping_policy_version  TEXT NOT NULL,
    status                  TEXT NOT NULL CHECK(status IN (
                                'planned',
                                'running',
                                'complete',
                                'partial',
                                'unsupported',
                                'unavailable',
                                'failed',
                                'cancelled',
                                'timed_out',
                                'output_rejected',
                                'decode_failed',
                                'mapping_failed',
                                'stale'
                            )),
    network_isolation_state TEXT NOT NULL CHECK(network_isolation_state IN (
                                'enforced',
                                'best_effort',
                                'not_available',
                                'not_applicable'
                            )),
    started_at              TEXT,
    finished_at             TEXT,
    exit_code               INTEGER,
    stdout_hash             TEXT,
    stderr_hash             TEXT,
    stdout_bytes            INTEGER NOT NULL DEFAULT 0 CHECK(stdout_bytes >= 0),
    stderr_bytes            INTEGER NOT NULL DEFAULT 0 CHECK(stderr_bytes >= 0),
    stdout_truncated        INTEGER NOT NULL DEFAULT 0 CHECK(stdout_truncated IN (0, 1)),
    stderr_truncated        INTEGER NOT NULL DEFAULT 0 CHECK(stderr_truncated IN (0, 1)),
    output_hash             TEXT,
    output_bytes            INTEGER CHECK(output_bytes IS NULL OR output_bytes >= 0),
    diagnostic_summary_json TEXT NOT NULL DEFAULT '[]',
    created_at              TEXT NOT NULL,
    UNIQUE(
        workspace_id,
        provider_key,
        scope_kind,
        scope_key,
        input_fingerprint,
        normalized_schema_version,
        mapping_policy_version
    )
);

CREATE INDEX IF NOT EXISTS idx_provider_execution_generation
    ON provider_execution(workspace_id, generation_id, status);
CREATE INDEX IF NOT EXISTS idx_provider_execution_reuse
    ON provider_execution(
        workspace_id,
        provider_key,
        scope_key,
        input_fingerprint,
        status
    );

CREATE TABLE IF NOT EXISTS provider_execution_file (
    provider_execution_id   TEXT NOT NULL
                            REFERENCES provider_execution(provider_execution_id) ON DELETE CASCADE,
    file_id                 TEXT
                            REFERENCES file_identity(file_id) ON DELETE SET NULL,
    revision_id             TEXT
                            REFERENCES file_revision(revision_id) ON DELETE SET NULL,
    provider_document_path  TEXT NOT NULL,
    canonical_path          TEXT,
    document_status         TEXT NOT NULL CHECK(document_status IN (
                                'included',
                                'reused',
                                'partial',
                                'skipped',
                                'failed',
                                'unmapped',
                                'outside_workspace'
                            )),
    raw_document_hash       TEXT,
    normalized_batch_hash   TEXT,
    reused_extractor_run_id TEXT
                            REFERENCES extractor_run(extractor_run_id) ON DELETE SET NULL,
    details_json            TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(provider_execution_id, provider_document_path)
);

CREATE INDEX IF NOT EXISTS idx_provider_execution_file_revision
    ON provider_execution_file(revision_id, document_status);

CREATE TABLE IF NOT EXISTS provider_execution_run (
    provider_execution_id   TEXT NOT NULL
                            REFERENCES provider_execution(provider_execution_id) ON DELETE CASCADE,
    extractor_run_id        TEXT NOT NULL
                            REFERENCES extractor_run(extractor_run_id) ON DELETE CASCADE,
    PRIMARY KEY(provider_execution_id, extractor_run_id)
);

CREATE INDEX IF NOT EXISTS idx_provider_execution_run_extractor
    ON provider_execution_run(extractor_run_id);

CREATE TABLE IF NOT EXISTS provider_runtime_diagnostic (
    provider_runtime_diagnostic_id TEXT PRIMARY KEY,
    provider_execution_id   TEXT NOT NULL
                            REFERENCES provider_execution(provider_execution_id) ON DELETE CASCADE,
    severity                TEXT NOT NULL CHECK(severity IN (
                                'info',
                                'warning',
                                'error',
                                'fatal'
                            )),
    code                    TEXT NOT NULL,
    message                 TEXT NOT NULL,
    provider_document_path  TEXT,
    details_json            TEXT NOT NULL DEFAULT '{}',
    created_at              TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_runtime_diag_execution
    ON provider_runtime_diagnostic(provider_execution_id, severity);

CREATE TABLE IF NOT EXISTS provider_invalidation_event (
    provider_invalidation_event_id TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL
                            REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    provider_key            TEXT NOT NULL
                            REFERENCES provider_descriptor(provider_key) ON DELETE RESTRICT,
    scope_kind              TEXT NOT NULL,
    scope_key               TEXT NOT NULL,
    prior_execution_id      TEXT
                            REFERENCES provider_execution(provider_execution_id) ON DELETE SET NULL,
    reason_code             TEXT NOT NULL CHECK(reason_code IN (
                                'source_changed',
                                'project_inputs_changed',
                                'provider_version_changed',
                                'executable_changed',
                                'provider_config_changed',
                                'protocol_changed',
                                'normalized_schema_changed',
                                'mapping_policy_changed',
                                'resolution_policy_changed',
                                'manual_rebuild',
                                'prior_output_missing',
                                'prior_output_incompatible'
                            )),
    old_fingerprint         TEXT,
    new_fingerprint         TEXT NOT NULL CHECK(length(new_fingerprint) = 64),
    details_json            TEXT NOT NULL DEFAULT '{}',
    occurred_at             TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_invalidation_generation
    ON provider_invalidation_event(workspace_id, generation_id, provider_key);

CREATE TABLE IF NOT EXISTS relationship_resolution (
    relationship_resolution_id TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL
                            REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    relationship_fact_id    TEXT NOT NULL
                            REFERENCES relationship_fact(relationship_fact_id) ON DELETE CASCADE,
    resolver_execution_id   TEXT
                            REFERENCES provider_execution(provider_execution_id) ON DELETE SET NULL,
    resolver_policy_version TEXT NOT NULL,
    status                  TEXT NOT NULL CHECK(status IN (
                                'resolved_symbol',
                                'resolved_file',
                                'external',
                                'ambiguous',
                                'unresolved',
                                'invalid',
                                'stale'
                            )),
    resolved_ref_kind       TEXT,
    resolved_ref_value      TEXT,
    reason_code             TEXT NOT NULL,
    candidate_refs_json     TEXT NOT NULL DEFAULT '[]',
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence_json           TEXT NOT NULL DEFAULT '[]',
    created_at              TEXT NOT NULL,
    UNIQUE(generation_id, relationship_fact_id, resolver_policy_version)
);

CREATE INDEX IF NOT EXISTS idx_relationship_resolution_status
    ON relationship_resolution(workspace_id, generation_id, status);
CREATE INDEX IF NOT EXISTS idx_relationship_resolution_target
    ON relationship_resolution(resolved_ref_kind, resolved_ref_value);

CREATE TABLE IF NOT EXISTS evidence_conflict (
    evidence_conflict_id    TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL
                            REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL
                            REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    subject_kind            TEXT NOT NULL CHECK(subject_kind IN (
                                'symbol',
                                'relationship',
                                'effect',
                                'coverage'
                            )),
    subject_key             TEXT NOT NULL,
    conflict_type           TEXT NOT NULL,
    participating_fact_ids_json TEXT NOT NULL,
    preferred_fact_id       TEXT,
    projection_policy_version TEXT NOT NULL,
    status                  TEXT NOT NULL CHECK(status IN (
                                'open',
                                'preferred_with_conflict',
                                'corroborated',
                                'unresolved',
                                'superseded'
                            )),
    explanation             TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    UNIQUE(
        generation_id,
        subject_kind,
        subject_key,
        conflict_type,
        projection_policy_version
    )
);

CREATE INDEX IF NOT EXISTS idx_evidence_conflict_generation
    ON evidence_conflict(workspace_id, generation_id, status);

CREATE VIEW IF NOT EXISTS current_provider_execution AS
SELECT pe.*
FROM provider_execution pe
JOIN workspace w
  ON w.workspace_id = pe.workspace_id
 AND w.active_generation_id = pe.generation_id;

CREATE VIEW IF NOT EXISTS current_relationship_resolution AS
SELECT rr.*
FROM relationship_resolution rr
JOIN workspace w
  ON w.workspace_id = rr.workspace_id
 AND w.active_generation_id = rr.generation_id;

CREATE VIEW IF NOT EXISTS current_evidence_conflict AS
SELECT ec.*
FROM evidence_conflict ec
JOIN workspace w
  ON w.workspace_id = ec.workspace_id
 AND w.active_generation_id = ec.generation_id;

-- This view assigns a deterministic evidence rank but deliberately does not
-- delete alternatives. Final preference and conflict explanation remains in
-- the application projection layer.
CREATE VIEW IF NOT EXISTS current_ranked_symbol_evidence AS
SELECT
    cs.*,
    COALESCE(
        wp.priority,
        CASE cs.provider_tier
            WHEN 'compiler' THEN 900
            WHEN 'semantic_index' THEN 800
            WHEN 'language_server' THEN 700
            WHEN 'structural' THEN 500
            WHEN 'document_config' THEN 300
            WHEN 'textual' THEN 100
            WHEN 'heuristic' THEN 50
            WHEN 'runtime_observation' THEN 850
            ELSE 0
        END
    ) AS evidence_rank
FROM current_symbol cs
LEFT JOIN provider_execution_run per
       ON per.extractor_run_id = cs.extractor_run_id
LEFT JOIN provider_execution pe
       ON pe.provider_execution_id = per.provider_execution_id
LEFT JOIN workspace_provider wp
       ON wp.workspace_id = cs.workspace_id
      AND wp.provider_key = pe.provider_key;

PRAGMA user_version = 2;
