-- Workspace Atlas foundation schema v1.0.0
-- SQLite 3.x. The application must enable foreign_keys on every connection.

PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA user_version = 1;

CREATE TABLE IF NOT EXISTS schema_migration (
    version                 INTEGER PRIMARY KEY,
    name                    TEXT NOT NULL,
    applied_at              TEXT NOT NULL
);

INSERT OR IGNORE INTO schema_migration(version, name, applied_at)
VALUES (1, 'workspace_atlas_foundation', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE IF NOT EXISTS workspace (
    workspace_id            TEXT PRIMARY KEY,
    display_name            TEXT NOT NULL,
    canonical_root          TEXT NOT NULL UNIQUE,
    root_fingerprint        TEXT NOT NULL,
    configuration_hash      TEXT NOT NULL,
    policy_version          TEXT NOT NULL,
    active_generation_id    TEXT,
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    CHECK(length(workspace_id) > 0),
    CHECK(length(canonical_root) > 0)
);

CREATE TABLE IF NOT EXISTS workspace_policy (
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    policy_key              TEXT NOT NULL,
    policy_value_json       TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    PRIMARY KEY(workspace_id, policy_key)
);

CREATE TABLE IF NOT EXISTS index_generation (
    generation_id           TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    parent_generation_id    TEXT REFERENCES index_generation(generation_id),
    sequence_no             INTEGER NOT NULL,
    state                   TEXT NOT NULL CHECK(state IN ('candidate', 'committed', 'failed', 'abandoned')),
    trigger_kind            TEXT NOT NULL CHECK(trigger_kind IN ('baseline', 'watcher', 'reconcile', 'manual', 'provider_invalidation', 'full_rebuild')),
    source_tree_hash        TEXT,
    provider_set_hash       TEXT NOT NULL,
    schema_version          TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    committed_at            TEXT,
    failure_code            TEXT,
    failure_message         TEXT,
    eligible_file_count     INTEGER NOT NULL DEFAULT 0 CHECK(eligible_file_count >= 0),
    indexed_file_count      INTEGER NOT NULL DEFAULT 0 CHECK(indexed_file_count >= 0),
    partial_file_count      INTEGER NOT NULL DEFAULT 0 CHECK(partial_file_count >= 0),
    unsupported_file_count  INTEGER NOT NULL DEFAULT 0 CHECK(unsupported_file_count >= 0),
    excluded_file_count     INTEGER NOT NULL DEFAULT 0 CHECK(excluded_file_count >= 0),
    failed_file_count       INTEGER NOT NULL DEFAULT 0 CHECK(failed_file_count >= 0),
    UNIQUE(workspace_id, sequence_no)
);

CREATE INDEX IF NOT EXISTS idx_generation_workspace_state
    ON index_generation(workspace_id, state, sequence_no DESC);

CREATE TABLE IF NOT EXISTS writer_lease (
    workspace_id            TEXT PRIMARY KEY REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    holder_id               TEXT NOT NULL,
    acquired_at             TEXT NOT NULL,
    expires_at              TEXT NOT NULL,
    heartbeat_at            TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS file_identity (
    file_id                 TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    identity_basis          TEXT NOT NULL CHECK(identity_basis IN ('created', 'git', 'exact_hash_rename', 'similarity_approved', 'user_assigned')),
    first_seen_at           TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    lifecycle_state         TEXT NOT NULL CHECK(lifecycle_state IN ('current', 'deleted', 'archived', 'unknown')),
    last_known_path         TEXT NOT NULL,
    CHECK(length(file_id) > 0)
);

CREATE INDEX IF NOT EXISTS idx_file_identity_workspace_path
    ON file_identity(workspace_id, last_known_path);

CREATE TABLE IF NOT EXISTS file_revision (
    revision_id             TEXT PRIMARY KEY,
    file_id                 TEXT NOT NULL REFERENCES file_identity(file_id) ON DELETE CASCADE,
    content_hash            TEXT NOT NULL,
    byte_size               INTEGER NOT NULL CHECK(byte_size >= 0),
    artifact_class          TEXT NOT NULL CHECK(artifact_class IN (
                                'source', 'test', 'documentation', 'configuration',
                                'schema', 'migration', 'build_manifest', 'infrastructure',
                                'prompt_instruction', 'generated_metadata', 'binary_metadata', 'other'
                             )),
    language                TEXT,
    encoding                TEXT,
    newline_style           TEXT,
    project_key             TEXT,
    package_key             TEXT,
    is_generated            INTEGER NOT NULL DEFAULT 0 CHECK(is_generated IN (0, 1)),
    is_test                 INTEGER NOT NULL DEFAULT 0 CHECK(is_test IN (0, 1)),
    discovered_at           TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_file_revision_identity
    ON file_revision(file_id, content_hash, artifact_class, IFNULL(language, ''));

CREATE INDEX IF NOT EXISTS idx_file_revision_hash
    ON file_revision(content_hash);

CREATE TABLE IF NOT EXISTS generation_file (
    generation_id           TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    file_id                 TEXT NOT NULL REFERENCES file_identity(file_id) ON DELETE CASCADE,
    revision_id             TEXT REFERENCES file_revision(revision_id),
    canonical_path          TEXT NOT NULL,
    presence_state          TEXT NOT NULL CHECK(presence_state IN ('present', 'excluded', 'unsupported', 'failed')),
    exclusion_code          TEXT,
    exclusion_detail        TEXT,
    observed_mtime_ns       INTEGER,
    observed_size           INTEGER CHECK(observed_size IS NULL OR observed_size >= 0),
    PRIMARY KEY(generation_id, file_id),
    UNIQUE(generation_id, canonical_path),
    CHECK(
        (presence_state = 'present' AND revision_id IS NOT NULL)
        OR presence_state IN ('excluded', 'unsupported', 'failed')
    )
);

CREATE INDEX IF NOT EXISTS idx_generation_file_revision
    ON generation_file(generation_id, revision_id);

CREATE TABLE IF NOT EXISTS extractor_run (
    extractor_run_id        TEXT PRIMARY KEY,
    revision_id             TEXT NOT NULL REFERENCES file_revision(revision_id) ON DELETE CASCADE,
    provider_name           TEXT NOT NULL,
    provider_version        TEXT NOT NULL,
    provider_tier           TEXT NOT NULL CHECK(provider_tier IN (
                                'compiler', 'semantic_index', 'language_server', 'structural',
                                'document_config', 'textual', 'heuristic', 'runtime_observation'
                             )),
    configuration_hash      TEXT NOT NULL,
    normalized_schema_version TEXT NOT NULL,
    deterministic           INTEGER NOT NULL CHECK(deterministic IN (0, 1)),
    status                  TEXT NOT NULL CHECK(status IN ('complete', 'partial', 'unsupported', 'failed', 'cancelled')),
    started_at              TEXT NOT NULL,
    finished_at             TEXT,
    bytes_total             INTEGER NOT NULL CHECK(bytes_total >= 0),
    bytes_processed         INTEGER NOT NULL CHECK(bytes_processed >= 0),
    capabilities_json       TEXT NOT NULL,
    limitations_json        TEXT NOT NULL,
    UNIQUE(revision_id, provider_name, provider_version, configuration_hash, normalized_schema_version)
);

CREATE INDEX IF NOT EXISTS idx_extractor_run_revision_status
    ON extractor_run(revision_id, status);

CREATE TABLE IF NOT EXISTS symbol_fact (
    symbol_fact_id          TEXT PRIMARY KEY,
    extractor_run_id        TEXT NOT NULL REFERENCES extractor_run(extractor_run_id) ON DELETE CASCADE,
    revision_id             TEXT NOT NULL REFERENCES file_revision(revision_id) ON DELETE CASCADE,
    canonical_symbol_key    TEXT NOT NULL,
    provider_symbol_id      TEXT,
    symbol_kind             TEXT NOT NULL,
    display_name            TEXT NOT NULL,
    qualified_name          TEXT NOT NULL,
    signature               TEXT,
    visibility              TEXT,
    start_byte              INTEGER NOT NULL CHECK(start_byte >= 0),
    end_byte                INTEGER NOT NULL CHECK(end_byte >= start_byte),
    start_line              INTEGER NOT NULL CHECK(start_line >= 1),
    start_column            INTEGER NOT NULL CHECK(start_column >= 1),
    end_line                INTEGER NOT NULL CHECK(end_line >= start_line),
    end_column              INTEGER NOT NULL CHECK(end_column >= 1),
    documentation           TEXT,
    attributes_json         TEXT NOT NULL DEFAULT '{}',
    evidence_method         TEXT NOT NULL CHECK(evidence_method IN ('exact', 'semantic', 'structural', 'textual', 'inferred', 'observed', 'declared')),
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence_reason         TEXT NOT NULL,
    UNIQUE(extractor_run_id, canonical_symbol_key, start_byte, end_byte)
);

CREATE INDEX IF NOT EXISTS idx_symbol_key
    ON symbol_fact(canonical_symbol_key);
CREATE INDEX IF NOT EXISTS idx_symbol_name
    ON symbol_fact(display_name, qualified_name);
CREATE INDEX IF NOT EXISTS idx_symbol_revision_span
    ON symbol_fact(revision_id, start_byte, end_byte);

CREATE TABLE IF NOT EXISTS relationship_fact (
    relationship_fact_id    TEXT PRIMARY KEY,
    extractor_run_id        TEXT NOT NULL REFERENCES extractor_run(extractor_run_id) ON DELETE CASCADE,
    revision_id             TEXT NOT NULL REFERENCES file_revision(revision_id) ON DELETE CASCADE,
    relationship_type       TEXT NOT NULL CHECK(relationship_type IN (
                                'defines', 'contains', 'imports', 'exports', 'calls', 'references',
                                'implements', 'extends', 'reads', 'writes', 'configures', 'tests',
                                'generates', 'documents', 'depends_on', 'replaces', 'moved_to', 'other'
                             )),
    source_ref_kind         TEXT NOT NULL CHECK(source_ref_kind IN ('symbol', 'path', 'external')),
    source_ref_value        TEXT NOT NULL,
    target_ref_kind         TEXT NOT NULL CHECK(target_ref_kind IN ('symbol', 'path', 'external')),
    target_ref_value        TEXT NOT NULL,
    start_byte              INTEGER CHECK(start_byte IS NULL OR start_byte >= 0),
    end_byte                INTEGER CHECK(end_byte IS NULL OR (start_byte IS NOT NULL AND end_byte >= start_byte)),
    attributes_json         TEXT NOT NULL DEFAULT '{}',
    evidence_method         TEXT NOT NULL CHECK(evidence_method IN ('exact', 'semantic', 'structural', 'textual', 'inferred', 'observed', 'declared')),
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence_reason         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_relationship_source
    ON relationship_fact(source_ref_kind, source_ref_value, relationship_type);
CREATE INDEX IF NOT EXISTS idx_relationship_target
    ON relationship_fact(target_ref_kind, target_ref_value, relationship_type);
CREATE INDEX IF NOT EXISTS idx_relationship_revision
    ON relationship_fact(revision_id);

CREATE TABLE IF NOT EXISTS effect_fact (
    effect_fact_id          TEXT PRIMARY KEY,
    extractor_run_id        TEXT NOT NULL REFERENCES extractor_run(extractor_run_id) ON DELETE CASCADE,
    revision_id             TEXT NOT NULL REFERENCES file_revision(revision_id) ON DELETE CASCADE,
    subject_ref_kind        TEXT NOT NULL CHECK(subject_ref_kind IN ('symbol', 'path', 'external')),
    subject_ref_value       TEXT NOT NULL,
    effect_type             TEXT NOT NULL,
    phase                   TEXT NOT NULL CHECK(phase IN ('direct', 'transitive', 'declared', 'observed')),
    attributes_json         TEXT NOT NULL DEFAULT '{}',
    evidence_method         TEXT NOT NULL CHECK(evidence_method IN ('exact', 'semantic', 'structural', 'textual', 'inferred', 'observed', 'declared')),
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence_reason         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_effect_subject
    ON effect_fact(subject_ref_kind, subject_ref_value, effect_type);

CREATE TABLE IF NOT EXISTS diagnostic (
    diagnostic_id           TEXT PRIMARY KEY,
    extractor_run_id        TEXT REFERENCES extractor_run(extractor_run_id) ON DELETE CASCADE,
    generation_id           TEXT REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    severity                TEXT NOT NULL CHECK(severity IN ('info', 'warning', 'error')),
    code                    TEXT NOT NULL,
    message                 TEXT NOT NULL,
    canonical_path          TEXT,
    start_byte              INTEGER,
    end_byte                INTEGER,
    details_json            TEXT NOT NULL DEFAULT '{}',
    created_at              TEXT NOT NULL,
    CHECK(extractor_run_id IS NOT NULL OR generation_id IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_diagnostic_generation_severity
    ON diagnostic(generation_id, severity);

CREATE TABLE IF NOT EXISTS coverage_record (
    coverage_id             TEXT PRIMARY KEY,
    generation_id           TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    file_id                 TEXT REFERENCES file_identity(file_id) ON DELETE CASCADE,
    scope_kind              TEXT NOT NULL CHECK(scope_kind IN ('workspace', 'file', 'provider', 'capability')),
    scope_key               TEXT NOT NULL,
    status                  TEXT NOT NULL CHECK(status IN ('complete', 'partial', 'unsupported', 'excluded', 'failed', 'stale')),
    capabilities_json       TEXT NOT NULL DEFAULT '[]',
    limitations_json        TEXT NOT NULL DEFAULT '[]',
    details_json            TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_coverage_generation_status
    ON coverage_record(generation_id, status, scope_kind);

CREATE TABLE IF NOT EXISTS lifecycle_event (
    event_id                TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    event_type              TEXT NOT NULL CHECK(event_type IN (
                                'created', 'content_changed', 'renamed', 'deleted',
                                'replacement_proposed', 'replacement_confirmed',
                                'excluded', 'included', 'archived', 'restored',
                                'provider_invalidated', 'full_rebuild'
                             )),
    occurred_at             TEXT NOT NULL,
    actor                   TEXT NOT NULL CHECK(actor IN ('filesystem', 'git', 'atlas', 'user', 'external_tool')),
    file_id                 TEXT REFERENCES file_identity(file_id) ON DELETE SET NULL,
    old_path                TEXT,
    new_path                TEXT,
    old_content_hash        TEXT,
    new_content_hash        TEXT,
    evidence_method         TEXT,
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    details_json            TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_lifecycle_workspace_time
    ON lifecycle_event(workspace_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_lifecycle_file_time
    ON lifecycle_event(file_id, occurred_at DESC);

CREATE TABLE IF NOT EXISTS tombstone (
    tombstone_id            TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    file_id                 TEXT NOT NULL REFERENCES file_identity(file_id) ON DELETE CASCADE,
    deletion_event_id       TEXT NOT NULL REFERENCES lifecycle_event(event_id) ON DELETE CASCADE,
    last_path               TEXT NOT NULL,
    last_revision_id        TEXT REFERENCES file_revision(revision_id) ON DELETE SET NULL,
    last_content_hash       TEXT NOT NULL,
    artifact_class          TEXT NOT NULL,
    symbol_summary_json     TEXT NOT NULL DEFAULT '[]',
    relationship_summary_json TEXT NOT NULL DEFAULT '[]',
    raw_snapshot_state      TEXT NOT NULL CHECK(raw_snapshot_state IN ('not_retained', 'retained_encrypted', 'retention_expired', 'forbidden_secret')),
    raw_snapshot_locator    TEXT,
    created_at              TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tombstone_workspace_path
    ON tombstone(workspace_id, last_path);

CREATE TABLE IF NOT EXISTS replacement_candidate (
    replacement_candidate_id TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    old_file_id             TEXT NOT NULL REFERENCES file_identity(file_id) ON DELETE CASCADE,
    new_file_id             TEXT NOT NULL REFERENCES file_identity(file_id) ON DELETE CASCADE,
    status                  TEXT NOT NULL CHECK(status IN ('proposed', 'confirmed', 'rejected', 'expired')),
    confidence              REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence_json           TEXT NOT NULL,
    proposed_at             TEXT NOT NULL,
    decided_at              TEXT,
    decided_by              TEXT,
    CHECK(old_file_id <> new_file_id)
);

CREATE TABLE IF NOT EXISTS context_packet (
    packet_id               TEXT PRIMARY KEY,
    workspace_id            TEXT NOT NULL REFERENCES workspace(workspace_id) ON DELETE CASCADE,
    generation_id           TEXT NOT NULL REFERENCES index_generation(generation_id) ON DELETE CASCADE,
    request_id              TEXT NOT NULL,
    request_hash            TEXT NOT NULL,
    mode                    TEXT NOT NULL CHECK(mode IN ('map', 'change', 'audit')),
    ranking_policy_version  TEXT NOT NULL,
    budget_json             TEXT NOT NULL,
    coverage_json           TEXT NOT NULL,
    omissions_json          TEXT NOT NULL,
    packet_hash             TEXT NOT NULL,
    status                  TEXT NOT NULL CHECK(status IN ('complete', 'partial', 'blocked')),
    created_at              TEXT NOT NULL,
    UNIQUE(workspace_id, generation_id, request_hash, ranking_policy_version, packet_hash)
);

CREATE INDEX IF NOT EXISTS idx_packet_workspace_time
    ON context_packet(workspace_id, created_at DESC);

CREATE TABLE IF NOT EXISTS context_packet_item (
    packet_id               TEXT NOT NULL REFERENCES context_packet(packet_id) ON DELETE CASCADE,
    ordinal                 INTEGER NOT NULL CHECK(ordinal >= 0),
    item_id                 TEXT NOT NULL,
    section                 TEXT NOT NULL CHECK(section IN ('primary', 'supporting', 'effect', 'history', 'unresolved', 'omission')),
    canonical_path          TEXT,
    canonical_symbol_key    TEXT,
    score                   REAL,
    relationship_distance   INTEGER CHECK(relationship_distance IS NULL OR relationship_distance >= 0),
    item_json               TEXT NOT NULL,
    PRIMARY KEY(packet_id, ordinal),
    UNIQUE(packet_id, item_id)
);

CREATE TABLE IF NOT EXISTS exact_excerpt (
    excerpt_id              TEXT PRIMARY KEY,
    packet_id               TEXT NOT NULL REFERENCES context_packet(packet_id) ON DELETE CASCADE,
    canonical_path          TEXT NOT NULL,
    canonical_symbol_key    TEXT,
    indexed_content_hash    TEXT NOT NULL,
    observed_content_hash   TEXT NOT NULL,
    start_byte              INTEGER NOT NULL CHECK(start_byte >= 0),
    end_byte                INTEGER NOT NULL CHECK(end_byte >= start_byte),
    start_line              INTEGER NOT NULL CHECK(start_line >= 1),
    end_line                INTEGER NOT NULL CHECK(end_line >= start_line),
    verification_status     TEXT NOT NULL CHECK(verification_status IN ('verified', 'hash_mismatch', 'read_failed', 'excluded')),
    excerpt_hash            TEXT NOT NULL,
    verified_at             TEXT NOT NULL,
    excerpt_text            TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_excerpt_packet_path
    ON exact_excerpt(packet_id, canonical_path, start_byte);

-- FTS is a derived search accelerator. Canonical facts remain in normal tables.
CREATE VIRTUAL TABLE IF NOT EXISTS atlas_fts USING fts5(
    record_kind UNINDEXED,
    record_id UNINDEXED,
    workspace_id UNINDEXED,
    canonical_path,
    display_name,
    qualified_name,
    documentation,
    searchable_text,
    tokenize = 'unicode61'
);

CREATE VIEW IF NOT EXISTS active_generation AS
SELECT g.*
FROM workspace w
JOIN index_generation g ON g.generation_id = w.active_generation_id
WHERE g.state = 'committed';

CREATE VIEW IF NOT EXISTS current_file AS
SELECT
    w.workspace_id,
    g.generation_id,
    g.sequence_no AS generation_sequence,
    gf.file_id,
    gf.revision_id,
    gf.canonical_path,
    gf.presence_state,
    gf.exclusion_code,
    fr.content_hash,
    fr.byte_size,
    fr.artifact_class,
    fr.language,
    fr.project_key,
    fr.package_key,
    fr.is_generated,
    fr.is_test
FROM workspace w
JOIN index_generation g ON g.generation_id = w.active_generation_id
JOIN generation_file gf ON gf.generation_id = g.generation_id
LEFT JOIN file_revision fr ON fr.revision_id = gf.revision_id
WHERE g.state = 'committed';

CREATE VIEW IF NOT EXISTS current_symbol AS
SELECT
    cf.workspace_id,
    cf.generation_id,
    cf.generation_sequence,
    cf.file_id,
    cf.canonical_path,
    cf.content_hash,
    sf.*,
    er.provider_name,
    er.provider_version,
    er.provider_tier,
    er.configuration_hash
FROM current_file cf
JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
JOIN extractor_run er ON er.extractor_run_id = sf.extractor_run_id
WHERE cf.presence_state = 'present'
  AND er.status IN ('complete', 'partial');

CREATE VIEW IF NOT EXISTS current_relationship AS
SELECT
    cf.workspace_id,
    cf.generation_id,
    cf.canonical_path,
    rf.*,
    er.provider_name,
    er.provider_version,
    er.provider_tier
FROM current_file cf
JOIN relationship_fact rf ON rf.revision_id = cf.revision_id
JOIN extractor_run er ON er.extractor_run_id = rf.extractor_run_id
WHERE cf.presence_state = 'present'
  AND er.status IN ('complete', 'partial');

CREATE TRIGGER IF NOT EXISTS validate_active_generation_insert
BEFORE INSERT ON workspace
WHEN NEW.active_generation_id IS NOT NULL
BEGIN
    SELECT CASE
        WHEN NOT EXISTS (
            SELECT 1 FROM index_generation g
            WHERE g.generation_id = NEW.active_generation_id
              AND g.workspace_id = NEW.workspace_id
              AND g.state = 'committed'
        )
        THEN RAISE(ABORT, 'active generation must be committed and belong to workspace')
    END;
END;

CREATE TRIGGER IF NOT EXISTS validate_active_generation_update
BEFORE UPDATE OF active_generation_id ON workspace
WHEN NEW.active_generation_id IS NOT NULL
BEGIN
    SELECT CASE
        WHEN NOT EXISTS (
            SELECT 1 FROM index_generation g
            WHERE g.generation_id = NEW.active_generation_id
              AND g.workspace_id = NEW.workspace_id
              AND g.state = 'committed'
        )
        THEN RAISE(ABORT, 'active generation must be committed and belong to workspace')
    END;
END;
