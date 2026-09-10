-- Workspace Atlas file classification and immutable revision identity schema v1.3.0
-- Forward-only structural migration: historical rows remain byte-for-byte unchanged.

DROP INDEX idx_file_revision_identity;

CREATE UNIQUE INDEX idx_file_revision_identity
    ON file_revision(
        file_id,
        content_hash,
        artifact_class,
        COALESCE(language, ''),
        is_generated,
        is_test
    );

-- Schema-4 writers identify themselves on the existing database lease. The
-- preserved schema-3 INSERT omits this column and is rejected by the trigger
-- before SQLite evaluates its ON CONFLICT update.
ALTER TABLE writer_lease
    ADD COLUMN schema_writer_version INTEGER;

CREATE TRIGGER writer_lease_require_schema_writer_version
BEFORE INSERT ON writer_lease
WHEN NEW.schema_writer_version IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

-- Older mutators that did not acquire the writer lease are fenced at their
-- first persistent statement by a connection-local function that only
-- schema-4-capable binaries register.
CREATE TRIGGER index_generation_require_schema_writer_for_abandon
BEFORE UPDATE OF state ON index_generation
WHEN OLD.state = 'candidate'
 AND NEW.state = 'abandoned'
 AND atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

CREATE TRIGGER task_session_require_schema_writer
BEFORE INSERT ON task_session
WHEN atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

CREATE TRIGGER context_use_event_require_schema_writer
BEFORE INSERT ON context_use_event
WHEN atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

CREATE TRIGGER serving_generation_require_schema_writer_insert
BEFORE INSERT ON serving_generation
WHEN atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

CREATE TRIGGER serving_generation_require_schema_writer_delete
BEFORE DELETE ON serving_generation
WHEN atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;

CREATE TRIGGER generation_delta_require_schema_writer
BEFORE INSERT ON generation_delta
WHEN atlas_schema_writer_version() IS NOT 4
BEGIN
    SELECT RAISE(ABORT, 'schema-4 writer marker required');
END;
