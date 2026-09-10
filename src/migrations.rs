//! Migration runner for the four embedded, ordered SQL migrations under
//! `migrations/`.
//!
//! Leading connection-safety PRAGMAs from migration files are stripped from
//! transactional bodies; catalogue connections configure those settings
//! before migration.

use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AtlasError, Result};

/// Embedded SQL for migration `0001_foundation`. Loaded verbatim from
/// `migrations/0001_foundation.sql` at build time.
pub const MIGRATION_0001_SQL: &str = include_str!("../migrations/0001_foundation.sql");

/// Embedded SQL for migration `0002_semantic_provider_foundation`. Additive to
/// `migrations/0001_foundation.sql`; existing foundation tables and rows remain intact.
pub const MIGRATION_0002_SQL: &str =
    include_str!("../migrations/0002_semantic_provider_foundation.sql");

/// Embedded SQL for migration `0003_context_intelligence_foundation`.
/// Additive to `0001` and `0002`; adds the Context Intelligence Serving Plane,
/// Context IR, task-session, telemetry, Generation Delta, and evidence-lease
/// tables without rewriting earlier tables.
pub const MIGRATION_0003_SQL: &str =
    include_str!("../migrations/0003_context_intelligence_foundation.sql");

/// Embedded SQL for migration `0004_file_classification_identity`.
/// Replaces only the immutable file-revision identity index.
pub const MIGRATION_0004_SQL: &str =
    include_str!("../migrations/0004_file_classification_identity.sql");

pub const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_foundation", MIGRATION_0001_SQL),
    ("0002_semantic_provider_foundation", MIGRATION_0002_SQL),
    ("0003_context_intelligence_foundation", MIGRATION_0003_SQL),
    ("0004_file_classification_identity", MIGRATION_0004_SQL),
];

/// The current schema version reported by `atlas status`.
pub const CURRENT_SCHEMA_VERSION: &str = "1.3.0";

/// Parse the leading `NNNN` version number out of a migration name.
fn parse_version(name: &str) -> Result<i64> {
    name.split('_')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AtlasError::Migration(format!("invalid migration name {name:?}")))
}

/// Highest migration version this binary knows how to apply.
pub fn max_supported_version() -> i64 {
    MIGRATIONS
        .iter()
        .filter_map(|(name, _)| parse_version(name).ok())
        .max()
        .unwrap_or(0)
}

fn table_exists(conn: &Connection, table_name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table_name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

fn reject_ambiguous_revision_material(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "file_revision")? {
        return Ok(());
    }
    let duplicate: Option<(String, String, String, String, i64, i64, i64)> = conn
        .query_row(
            "SELECT file_id, content_hash, artifact_class, COALESCE(language, ''),
                    is_generated, is_test, COUNT(*)
             FROM file_revision
             GROUP BY file_id, content_hash, artifact_class, COALESCE(language, ''),
                      is_generated, is_test
             HAVING COUNT(*) > 1
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    if let Some((file_id, hash, class, language, generated, test, count)) = duplicate {
        return Err(AtlasError::Migration(format!(
            "ambiguous duplicate file revision material tuple ({file_id}, {hash}, {class}, \
             {language:?}, {generated}, {test}) has {count} rows; refusing to deduplicate or rewrite"
        )));
    }
    Ok(())
}

fn validate_catalogue_state(conn: &Connection, catalogue_version: i64) -> Result<()> {
    let schema_table_exists = table_exists(conn, "schema_migration")?;
    if catalogue_version == 0 {
        if schema_table_exists {
            return Err(AtlasError::Migration(
                "catalogue schema_migration body exists while user_version is 0; split migration state"
                    .into(),
            ));
        }
        return Ok(());
    }
    if !schema_table_exists {
        return Err(AtlasError::Migration(format!(
            "catalogue user_version is {catalogue_version} but schema_migration is absent; split migration state"
        )));
    }

    let applied_rows: Vec<(i64, String)> = {
        let mut statement =
            conn.prepare("SELECT version, name FROM schema_migration ORDER BY version")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };
    let expected_schema_names = [
        "workspace_atlas_foundation",
        "0002_semantic_provider_foundation",
        "0003_context_intelligence_foundation",
        "0004_file_classification_identity",
    ];
    if applied_rows.len() != usize::try_from(catalogue_version).unwrap_or(usize::MAX) {
        return Err(AtlasError::Migration(format!(
            "catalogue user_version {catalogue_version} disagrees with schema_migration rows {applied_rows:?}; split migration state"
        )));
    }
    for (index, (version, name)) in applied_rows.iter().enumerate() {
        let expected_version = i64::try_from(index + 1).unwrap_or(i64::MAX);
        let expected_name = expected_schema_names.get(index).copied();
        if *version != expected_version || expected_name != Some(name.as_str()) {
            return Err(AtlasError::Migration(format!(
                "schema_migration row ({version}, {name:?}) is not the expected ordered migration; split migration state"
            )));
        }
    }

    let has_integrity_table = table_exists(conn, "migration_integrity")?;
    if catalogue_version < 2 {
        if has_integrity_table {
            return Err(AtlasError::Migration(
                "migration_integrity exists before migration 0002; split checksum state".into(),
            ));
        }
    } else {
        if !has_integrity_table {
            return Err(AtlasError::Migration(
                "migration_integrity is absent after migration 0002; split checksum state".into(),
            ));
        }
        let checksum_rows: Vec<(i64, String)> = {
            let mut statement = conn.prepare(
                "SELECT version, migration_name FROM migration_integrity ORDER BY version",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            rows
        };
        if checksum_rows.len() != usize::try_from(catalogue_version).unwrap_or(usize::MAX) {
            return Err(AtlasError::Migration(format!(
                "catalogue user_version {catalogue_version} disagrees with migration checksum rows {checksum_rows:?}; split checksum state"
            )));
        }
        for (index, (version, migration_name)) in checksum_rows.iter().enumerate() {
            let expected_version = i64::try_from(index + 1).unwrap_or(i64::MAX);
            let expected_name = MIGRATIONS.get(index).map(|(name, _)| *name);
            if *version != expected_version || expected_name != Some(migration_name.as_str()) {
                return Err(AtlasError::Migration(format!(
                    "migration checksum row ({version}, {migration_name:?}) is not the expected ordered migration; split checksum state"
                )));
            }
        }
    }

    let index_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_file_revision_identity'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(index_sql) = index_sql else {
        return Err(AtlasError::Migration(
            "idx_file_revision_identity is absent; split index state".into(),
        ));
    };
    let normalized_index: String = index_sql
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let expected_columns = if catalogue_version >= 4 {
        "onfile_revision(file_id,content_hash,artifact_class,coalesce(language,''),is_generated,is_test)"
    } else {
        "onfile_revision(file_id,content_hash,artifact_class,ifnull(language,''))"
    };
    if !normalized_index.contains(expected_columns) {
        return Err(AtlasError::Migration(format!(
            "idx_file_revision_identity does not match user_version {catalogue_version}; split index state"
        )));
    }
    Ok(())
}

fn preflight_catalogue_state(conn: &Connection, catalogue_version: i64) -> Result<()> {
    integrity_check(conn)?;
    reject_ambiguous_revision_material(conn)?;
    validate_catalogue_state(conn, catalogue_version)?;
    let mismatched = verify_migration_checksums(conn)?;
    if !mismatched.is_empty() {
        return Err(AtlasError::Migration(format!(
            "migration checksum mismatch for {}",
            mismatched.join(", ")
        )));
    }
    Ok(())
}

/// Apply all migrations in order. Idempotent.
///
/// A newer catalogue is refused before any write. Existing integrity,
/// migration-row, checksum, identity-index, and header state is checked before
/// migration. Migration 0004 then commits its structural body, migration row,
/// checksum row, and `user_version` header in one transaction.
pub fn apply_all(conn: &Connection) -> Result<()> {
    crate::catalogue::register_schema_writer_version(conn)?;
    let binary_max = max_supported_version();
    let catalogue_version = user_version(conn)?;
    if catalogue_version > binary_max {
        return Err(AtlasError::Migration(format!(
            "catalogue schema version {catalogue_version} is newer than the {binary_max} \
             this binary supports; refusing to write. Upgrade the atlas binary."
        )));
    }
    preflight_catalogue_state(conn, catalogue_version)?;

    for (name, sql) in MIGRATIONS {
        let version = parse_version(name)?;
        let already: Option<i64> = if table_exists(conn, "schema_migration")? {
            conn.query_row(
                "SELECT version FROM schema_migration WHERE version = ?1",
                params![version],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        if already.is_some() {
            continue;
        }

        if version == 4 {
            // A fresh run reaches this point after 0001..0003. Establish all
            // historical checksums before the 0004 atomic boundary so any
            // 0004 fault returns a complete, valid version-3 catalogue.
            reconcile_migration_checksums(conn)?;
            let before_version = user_version(conn)?;
            preflight_catalogue_state(conn, before_version)?;
        }
        apply_one_migration(conn, name, sql, version)?;
    }

    reconcile_migration_checksums(conn)?;
    let final_version = user_version(conn)?;
    preflight_catalogue_state(conn, final_version)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationStage {
    Body,
    MigrationRow,
    Checksum,
    Header,
}

/// Apply a migration's body, row, checksum, and header in one transaction.
fn apply_one_migration(conn: &Connection, name: &str, sql: &str, version: i64) -> Result<()> {
    apply_one_migration_with_hook(conn, name, sql, version, |_| Ok(()))
}

fn apply_one_migration_with_hook<F>(
    conn: &Connection,
    name: &str,
    sql: &str,
    version: i64,
    mut after_stage: F,
) -> Result<()>
where
    F: FnMut(MigrationStage) -> Result<()>,
{
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&strip_pragmas(sql))?;
    after_stage(MigrationStage::Body)?;

    let present: Option<i64> = tx
        .query_row(
            "SELECT version FROM schema_migration WHERE version = ?1",
            params![version],
            |row| row.get(0),
        )
        .optional()?;
    if present.is_none() {
        tx.execute(
            "INSERT INTO schema_migration(version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![version, name, iso8601_now()],
        )?;
    }
    after_stage(MigrationStage::MigrationRow)?;

    if table_exists(&tx, "migration_integrity")? {
        let checksum = crate::hashing::content_hash_of_bytes(sql.as_bytes());
        tx.execute(
            "INSERT INTO migration_integrity(version, migration_name, sha256, verified_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![version, name, checksum, iso8601_now()],
        )?;
    }
    after_stage(MigrationStage::Checksum)?;

    tx.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    after_stage(MigrationStage::Header)?;
    tx.commit()?;
    Ok(())
}

/// Verify or backfill the SHA-256 checksum of every *applied* migration in
/// `migration_integrity`. A stored checksum that disagrees with the embedded
/// migration text is rejected (MIG-001). Pre-V1.1 catalogues that have not
/// yet applied `0002` have no `migration_integrity` table; this is a no-op
/// for them until `0002` runs.
fn reconcile_migration_checksums(conn: &Connection) -> Result<()> {
    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='migration_integrity'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(());
    }

    for (name, sql) in MIGRATIONS {
        let version = parse_version(name)?;
        let applied: Option<i64> = conn
            .query_row(
                "SELECT version FROM schema_migration WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .optional()?;
        if applied.is_none() {
            continue;
        }

        let checksum = crate::hashing::content_hash_of_bytes(sql.as_bytes());
        let existing: Option<String> = conn
            .query_row(
                "SELECT sha256 FROM migration_integrity WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .optional()?;

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO migration_integrity(version, migration_name, sha256, verified_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![version, *name, checksum, iso8601_now()],
                )?;
            }
            Some(stored) if stored == checksum => {}
            Some(stored) => {
                return Err(AtlasError::Migration(format!(
                    "migration {name} (version {version}) checksum mismatch: \
                     catalogue recorded {stored}, embedded migration hashes to {checksum}"
                )));
            }
        }
    }
    Ok(())
}

/// OBS-002: read-only variant of `reconcile_migration_checksums` for
/// `atlas doctor` -- verifies every applied migration's checksum against
/// the embedded SQL without inserting/backfilling anything. Returns the
/// names of any migrations whose stored checksum disagrees (empty = clean).
pub fn verify_migration_checksums(conn: &Connection) -> Result<Vec<String>> {
    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='migration_integrity'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(Vec::new());
    }

    let mut mismatched = Vec::new();
    for (name, sql) in MIGRATIONS {
        let version = parse_version(name)?;
        let applied: Option<i64> = conn
            .query_row(
                "SELECT version FROM schema_migration WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .optional()?;
        if applied.is_none() {
            continue;
        }
        let checksum = crate::hashing::content_hash_of_bytes(sql.as_bytes());
        let existing: Option<String> = conn
            .query_row(
                "SELECT sha256 FROM migration_integrity WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(stored) = existing {
            if stored != checksum {
                mismatched.push((*name).to_string());
            }
        }
    }
    Ok(mismatched)
}

/// Return the highest applied migration version, or `None` for a fresh
/// database.
pub fn applied_version(conn: &Connection) -> Result<Option<i64>> {
    let v: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migration", [], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    Ok(v)
}

/// Return the `PRAGMA user_version` value (the schema_version constant).
pub fn user_version(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(v)
}

#[cfg(test)]
/// Extract the leading `PRAGMA ...;` statements from a migration SQL string so
/// they can run outside any transaction.
fn split_pragmas(sql: &str) -> String {
    let mut out = String::new();
    for line in sql.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        if trimmed.to_ascii_uppercase().starts_with("PRAGMA") {
            out.push_str(line);
            out.push('\n');
        } else {
            break;
        }
    }
    out
}

/// Verify SQLite PRAGMA `integrity_check` returns `ok` and `foreign_key_check`
/// finds no violations. Accepts zero-row answers (treated as no errors).
pub fn integrity_check(conn: &Connection) -> Result<()> {
    let integrity: Option<String> = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
        .optional()?;
    let integrity = integrity.unwrap_or_else(|| "ok".to_string());
    if integrity != "ok" {
        return Err(AtlasError::Migration(format!(
            "PRAGMA integrity_check returned {integrity:?}"
        )));
    }
    let fk_violations: i64 = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    if fk_violations != 0 {
        return Err(AtlasError::Migration(format!(
            "PRAGMA foreign_key_check found {fk_violations} violations"
        )));
    }
    Ok(())
}

/// Return the migration SQL with all leading `PRAGMA ...;` statements removed.
fn strip_pragmas(sql: &str) -> String {
    let mut out = String::new();
    let mut past_pragmas = false;
    for line in sql.lines() {
        let trimmed = line.trim_start();
        if !past_pragmas && (trimmed.is_empty() || trimmed.starts_with("--")) {
            continue;
        }
        let upper = trimmed.to_ascii_uppercase();
        if !past_pragmas && !trimmed.is_empty() && !upper.starts_with("PRAGMA") {
            past_pragmas = true;
        }
        if past_pragmas {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// ISO 8601 / RFC 3339 UTC timestamp with millisecond precision.
pub fn iso8601_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_ms = now.as_millis();
    let secs = (total_ms / 1000) as i64;
    let ms = (total_ms % 1000) as u32;

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hour, minute, second, ms
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;
    use std::collections::BTreeMap;

    fn open_temp() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn
    }

    fn apply_through_0003(conn: &Connection) {
        for (name, sql, version) in [
            ("0001_foundation", MIGRATION_0001_SQL, 1),
            ("0002_semantic_provider_foundation", MIGRATION_0002_SQL, 2),
            (
                "0003_context_intelligence_foundation",
                MIGRATION_0003_SQL,
                3,
            ),
        ] {
            apply_one_migration(conn, name, sql, version).unwrap();
        }
        reconcile_migration_checksums(conn).unwrap();
        assert_eq!(user_version(conn).unwrap(), 3);
    }

    fn identity_index_sql(conn: &Connection) -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_file_revision_identity'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn seed_workspace_and_file(conn: &Connection, file_id: &str) {
        conn.execute(
            "INSERT INTO workspace (
                workspace_id, display_name, canonical_root, root_fingerprint,
                configuration_hash, policy_version, active_generation_id,
                created_at, updated_at
             ) VALUES (
                'ws_identity', 'identity', '/<workspace>/identity', 'fp', 'cfg',
                'p1', NULL, '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_identity (
                file_id, workspace_id, identity_basis, first_seen_at, last_seen_at,
                lifecycle_state, last_known_path
             ) VALUES (
                ?1, 'ws_identity', 'created', '2026-09-03T00:00:00Z',
                '2026-09-03T00:00:00Z', 'current', 'src/lib.rs'
             )",
            [file_id],
        )
        .unwrap();
    }

    fn non_migration_table_rows(conn: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
        let table_names: Vec<String> = {
            let mut statement = conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table'
                       AND name NOT LIKE 'sqlite_%'
                       AND name NOT IN ('schema_migration', 'migration_integrity')
                     ORDER BY name",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(|row| row.unwrap())
                .collect()
        };

        table_names
            .into_iter()
            .map(|table_name| {
                let quoted_name = table_name.replace('"', "\"\"");
                let mut statement = conn
                    .prepare(&format!("SELECT * FROM \"{quoted_name}\""))
                    .unwrap();
                let column_count = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..column_count)
                            .map(|column| row.get(column))
                            .collect::<rusqlite::Result<Vec<Value>>>()
                    })
                    .unwrap()
                    .map(|row| row.unwrap())
                    .collect();
                (table_name, rows)
            })
            .collect()
    }

    #[test]
    fn apply_all_is_idempotent() {
        let conn = open_temp();
        apply_all(&conn).unwrap();
        apply_all(&conn).unwrap();
        let v = applied_version(&conn).unwrap();
        assert_eq!(v, Some(4));
    }

    #[test]
    fn integrity_check_passes_on_fresh_db() {
        let conn = open_temp();
        apply_all(&conn).unwrap();
        integrity_check(&conn).unwrap();
    }

    #[test]
    fn schema_has_expected_objects() {
        let conn = open_temp();
        apply_all(&conn).unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(Result::ok)
                .collect()
        };
        for required in [
            "schema_migration",
            "workspace",
            "index_generation",
            "writer_lease",
            "file_identity",
            "file_revision",
            "generation_file",
            "extractor_run",
            "symbol_fact",
            "relationship_fact",
            "diagnostic",
            "coverage_record",
            "lifecycle_event",
            "tombstone",
            "replacement_candidate",
            "context_packet",
            "context_packet_item",
            "exact_excerpt",
        ] {
            assert!(
                tables.iter().any(|t| t == required),
                "missing table {required}; found {tables:?}"
            );
        }
        for required_v1_1 in [
            "migration_integrity",
            "provider_descriptor",
            "workspace_provider",
            "project_scope",
            "provider_execution",
            "provider_execution_file",
            "provider_execution_run",
            "provider_runtime_diagnostic",
            "provider_invalidation_event",
            "relationship_resolution",
            "evidence_conflict",
        ] {
            assert!(
                tables.iter().any(|t| t == required_v1_1),
                "missing V1.1 table {required_v1_1}; found {tables:?}"
            );
        }
        for required_v1_2 in [
            "serving_generation",
            "symbol_serving_projection",
            "relationship_serving_edge",
            "serving_coverage_rollup",
            "task_session",
            "context_ir",
            "context_ir_item",
            "context_use_event",
            "generation_delta",
            "generation_delta_item",
            "evidence_lease",
            "query_stage_metric",
        ] {
            assert!(
                tables.iter().any(|t| t == required_v1_2),
                "missing V1.2 table {required_v1_2}; found {tables:?}"
            );
        }
    }

    #[test]
    fn interrupted_migration_rolls_back_leaving_prior_state_usable() {
        let conn = open_temp();
        // Apply 0001 only, simulating the state right before 0002 would run.
        apply_one_migration(&conn, "0001_foundation", MIGRATION_0001_SQL, 1).unwrap();
        integrity_check(&conn).unwrap();

        // A malformed migration body must fail atomically: no partial DDL,
        // no schema_migration row, no user_version bump.
        let broken_sql = "PRAGMA foreign_keys = ON;\nCREATE TABLE this_is_not_valid_sql(;";
        let err = apply_one_migration(&conn, "0002_broken", broken_sql, 2);
        assert!(err.is_err(), "malformed migration body must fail");

        let applied = applied_version(&conn).unwrap();
        assert_eq!(
            applied,
            Some(1),
            "failed migration must not be recorded as applied"
        );
        let user = user_version(&conn).unwrap();
        assert_eq!(user, 1, "PRAGMA user_version must not advance on rollback");
        let leaked_table: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='this_is_not_valid_sql'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(
            leaked_table.is_none(),
            "no DDL from the failed migration may survive rollback"
        );

        // The pre-existing V1 database remains fully usable afterward.
        integrity_check(&conn).unwrap();
        apply_all(&conn).unwrap();
        assert_eq!(applied_version(&conn).unwrap(), Some(4));
    }

    #[test]
    fn migration_0002_preserves_lifecycle_and_context_packet_rows() {
        let conn = open_temp();
        apply_one_migration(&conn, "0001_foundation", MIGRATION_0001_SQL, 1).unwrap();
        conn.execute(
            "INSERT INTO workspace(workspace_id, display_name, canonical_root, root_fingerprint,
                                    configuration_hash, policy_version, active_generation_id,
                                    created_at, updated_at)
             VALUES ('ws_lc', 'lc', '/<workspace>/lc', 'fp', 'cfg', 'p1', NULL,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO index_generation(generation_id, workspace_id, sequence_no, state,
                                           trigger_kind, provider_set_hash, schema_version, created_at)
             VALUES ('gen_lc', 'ws_lc', 1, 'committed', 'baseline', 'psh', '1.0.0',
                     '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lifecycle_event(event_id, workspace_id, generation_id, event_type,
                                          occurred_at, actor, confidence, details_json)
             VALUES ('lc_1', 'ws_lc', 'gen_lc', 'created', '2026-01-01T00:00:00.000Z',
                     'atlas', 1.0, '{}')",
            [],
        )
        .unwrap();

        apply_all(&conn).unwrap();

        let lc_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_event WHERE event_id = 'lc_1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            lc_count, 1,
            "V1 lifecycle_event row must survive the 0002 migration"
        );
        integrity_check(&conn).unwrap();
    }

    #[test]
    fn populated_v1_1_catalogue_migrates_to_v1_3_without_row_loss() {
        let conn = open_temp();
        // Apply 0001+0002 only, simulating a populated V1.1 catalogue.
        for (name, sql, version) in [
            ("0001_foundation", MIGRATION_0001_SQL, 1),
            ("0002_semantic_provider_foundation", MIGRATION_0002_SQL, 2),
        ] {
            apply_one_migration(&conn, name, sql, version).unwrap();
        }
        reconcile_migration_checksums(&conn).unwrap();
        conn.execute(
            "INSERT INTO workspace(workspace_id, display_name, canonical_root, root_fingerprint,
                                    configuration_hash, policy_version, active_generation_id,
                                    created_at, updated_at)
             VALUES ('ws_v11', 'v11', '/<workspace>/v11', 'fp', 'cfg', 'p1', NULL,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO index_generation(generation_id, workspace_id, sequence_no, state,
                                           trigger_kind, provider_set_hash, schema_version, created_at)
             VALUES ('gen_v11', 'ws_v11', 1, 'committed', 'baseline', 'psh', '1.1.0',
                     '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        apply_all(&conn).unwrap();

        let user = user_version(&conn).unwrap();
        assert_eq!(
            user, 4,
            "0002-populated catalogue must migrate all the way to 0004"
        );
        let ws_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace WHERE workspace_id = 'ws_v11'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ws_count, 1,
            "pre-existing V1.1 row must survive migration to 0003"
        );
        let gen_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM index_generation WHERE generation_id = 'gen_v11'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            gen_count, 1,
            "pre-existing V1.1 generation row must survive migration to 0003"
        );
        integrity_check(&conn).unwrap();

        // The new V1.2 tables are usable and empty, not merely present.
        let serving_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM serving_generation", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serving_count, 0);
    }
    #[test]
    fn tampered_migration_checksum_is_rejected() {
        let conn = open_temp();
        apply_all(&conn).unwrap();
        conn.execute(
            "UPDATE migration_integrity SET sha256 = ?1 WHERE version = 1",
            params![
                "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string()
                    .chars()
                    .take(64)
                    .collect::<String>()
            ],
        )
        .unwrap();
        let err = apply_all(&conn).unwrap_err();
        assert!(
            format!("{err:?}").contains("checksum mismatch"),
            "expected checksum mismatch error, got {err:?}"
        );
    }

    #[test]
    fn newer_catalogue_schema_is_rejected() {
        let conn = open_temp();
        apply_all(&conn).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        let err = apply_all(&conn).unwrap_err();
        assert!(
            format!("{err:?}").contains("newer than"),
            "expected newer-schema rejection, got {err:?}"
        );
    }

    #[test]
    fn populated_v1_catalogue_migrates_to_v1_3_without_row_loss() {
        let conn = open_temp();
        // Apply only migration 0001 to simulate a populated V1 catalogue.
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch(&strip_pragmas(MIGRATION_0001_SQL))
            .unwrap();
        tx.commit().unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
        conn.execute(
            "INSERT INTO workspace(workspace_id, display_name, canonical_root, root_fingerprint,
                                    configuration_hash, policy_version, active_generation_id,
                                    created_at, updated_at)
             VALUES ('ws_test', 'test', '/<workspace>/ws', 'fp', 'cfg', 'p1', NULL,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        // Now run the full (V1.1-aware) migration runner against this
        // populated V1 database.
        apply_all(&conn).unwrap();

        let user = user_version(&conn).unwrap();
        assert_eq!(user, 4);
        let ws_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace WHERE workspace_id = 'ws_test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ws_count, 1,
            "pre-existing V1 row must survive migration to 0002"
        );
        integrity_check(&conn).unwrap();
    }

    #[test]
    fn migration_0004_commits_complete_state_and_six_field_uniqueness() {
        let conn = open_temp();
        apply_all(&conn).unwrap();

        assert_eq!(CURRENT_SCHEMA_VERSION, "1.3.0");
        assert_eq!(max_supported_version(), 4);
        assert_eq!(applied_version(&conn).unwrap(), Some(4));
        assert_eq!(user_version(&conn).unwrap(), 4);
        let migration: (String, String) = conn
            .query_row(
                "SELECT sm.name, mi.sha256
                 FROM schema_migration sm
                 JOIN migration_integrity mi USING (version)
                 WHERE sm.version = 4",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(migration.0, "0004_file_classification_identity");
        assert_eq!(
            migration.1,
            crate::hashing::content_hash_of_bytes(MIGRATION_0004_SQL.as_bytes())
        );
        let normalized_index: String = identity_index_sql(&conn)
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect();
        assert!(normalized_index.contains(
            "onfile_revision(file_id,content_hash,artifact_class,coalesce(language,''),is_generated,is_test)"
        ));

        seed_workspace_and_file(&conn, "file_material");
        let insert_revision =
            |revision_id: &str, language: Option<&str>, is_generated: i64, is_test: i64| {
                conn.execute(
                    "INSERT INTO file_revision (
                        revision_id, file_id, content_hash, byte_size, artifact_class,
                        language, encoding, newline_style, project_key, package_key,
                        is_generated, is_test, discovered_at
                     ) VALUES (
                        ?1, 'file_material',
                        'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                        1, 'source', ?2, 'utf-8', NULL, NULL, NULL, ?3, ?4,
                        '2026-09-03T00:00:00Z'
                     )",
                    params![revision_id, language, is_generated, is_test],
                )
            };
        insert_revision("rev_null_language", None, 0, 0).unwrap();
        assert!(
            insert_revision("rev_empty_language", Some(""), 0, 0).is_err(),
            "NULL and empty language must occupy one material identity"
        );
        insert_revision("rev_generated", None, 1, 0).unwrap();
        insert_revision("rev_test", None, 0, 1).unwrap();
        insert_revision("rev_rust_language", Some("rust"), 0, 0).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM file_revision WHERE file_id = 'file_material'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            4
        );
    }

    #[test]
    fn migration_0004_rejects_ambiguous_material_without_rewriting_rows() {
        let conn = open_temp();
        apply_through_0003(&conn);
        seed_workspace_and_file(&conn, "file_ambiguous");
        conn.execute(
            "INSERT INTO file_revision (
                revision_id, file_id, content_hash, byte_size, artifact_class, language,
                encoding, newline_style, project_key, package_key, is_generated, is_test,
                discovered_at
             ) VALUES (
                'opaque_a', 'file_ambiguous',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                1, 'source', NULL, 'utf-8', NULL, NULL, NULL, 0, 0,
                '2026-09-03T00:00:00Z'
             )",
            [],
        )
        .unwrap();
        conn.execute_batch("DROP INDEX idx_file_revision_identity;")
            .unwrap();
        conn.execute(
            "INSERT INTO file_revision (
                revision_id, file_id, content_hash, byte_size, artifact_class, language,
                encoding, newline_style, project_key, package_key, is_generated, is_test,
                discovered_at
             ) VALUES (
                'opaque_b', 'file_ambiguous',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                1, 'source', '', 'utf-8', NULL, NULL, NULL, 0, 0,
                '2026-09-03T00:00:00Z'
             )",
            [],
        )
        .unwrap();

        let before = non_migration_table_rows(&conn);
        let error = apply_all(&conn).unwrap_err();
        assert!(
            error.to_string().contains("ambiguous duplicate"),
            "unexpected error: {error}"
        );
        assert_eq!(non_migration_table_rows(&conn), before);
        assert_eq!(user_version(&conn).unwrap(), 3);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM schema_migration WHERE version = 4",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn migration_0004_injected_failures_restore_body_row_checksum_index_and_header() {
        for failure_stage in [
            MigrationStage::Body,
            MigrationStage::MigrationRow,
            MigrationStage::Checksum,
            MigrationStage::Header,
        ] {
            let conn = open_temp();
            apply_through_0003(&conn);
            let old_index = identity_index_sql(&conn);

            let result = apply_one_migration_with_hook(
                &conn,
                "0004_file_classification_identity",
                MIGRATION_0004_SQL,
                4,
                |completed_stage| {
                    if completed_stage == failure_stage {
                        Err(AtlasError::Migration(format!(
                            "injected failure after {completed_stage:?}"
                        )))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "{failure_stage:?} must fail");
            assert_eq!(user_version(&conn).unwrap(), 3);
            assert_eq!(applied_version(&conn).unwrap(), Some(3));
            assert_eq!(identity_index_sql(&conn), old_index);
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM migration_integrity WHERE version = 4",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0
            );
            integrity_check(&conn).unwrap();
        }
    }

    #[test]
    fn split_header_migration_checksum_and_index_states_are_rejected() {
        let header_split = open_temp();
        apply_through_0003(&header_split);
        header_split
            .execute_batch("PRAGMA user_version = 4;")
            .unwrap();
        let error = apply_all(&header_split).unwrap_err();
        assert!(error.to_string().contains("schema_migration"), "{error}");

        let migration_split = open_temp();
        apply_all(&migration_split).unwrap();
        migration_split
            .execute("DELETE FROM schema_migration WHERE version = 4", [])
            .unwrap();
        let error = apply_all(&migration_split).unwrap_err();
        assert!(error.to_string().contains("schema_migration"), "{error}");

        let checksum_split = open_temp();
        apply_all(&checksum_split).unwrap();
        checksum_split
            .execute("DELETE FROM migration_integrity WHERE version = 4", [])
            .unwrap();
        let error = apply_all(&checksum_split).unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");

        let index_split = open_temp();
        apply_all(&index_split).unwrap();
        index_split
            .execute_batch(
                "DROP INDEX idx_file_revision_identity;
                 CREATE UNIQUE INDEX idx_file_revision_identity
                 ON file_revision(
                    file_id, content_hash, artifact_class, COALESCE(language, '')
                 );",
            )
            .unwrap();
        let error = apply_all(&index_split).unwrap_err();
        assert!(error.to_string().contains("index"), "{error}");
    }

    #[test]
    fn migration_0004_preserves_history_and_reconcile_reuses_only_exact_old_material() {
        let database_directory = tempfile::tempdir().unwrap();
        let workspace_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("tests")).unwrap();
        let source_bytes = b"pub fn calculate(value: i64) -> i64 { value + 1 }\n";
        let test_bytes = b"#[test]\nfn calculates() { assert_eq!(2, 2); }\n";
        std::fs::write(workspace_directory.path().join("src/lib.rs"), source_bytes).unwrap();
        std::fs::write(
            workspace_directory.path().join("tests/calculate.rs"),
            test_bytes,
        )
        .unwrap();

        let database_path = database_directory.path().join("atlas.sqlite");
        let conn = Connection::open(&database_path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        apply_through_0003(&conn);
        let config = crate::config::Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"history\"\n",
        )
        .unwrap();
        let mut workspace = crate::workspace::register_workspace(
            &conn,
            workspace_directory.path(),
            &config,
            &database_path,
            "1.0.0",
        )
        .unwrap();
        let old_generation_id = "gen_historical";
        conn.execute(
            "INSERT INTO index_generation (
                generation_id, workspace_id, parent_generation_id, sequence_no, state,
                trigger_kind, source_tree_hash, provider_set_hash, schema_version,
                created_at, committed_at
             ) VALUES (
                ?1, ?2, NULL, 1, 'committed', 'baseline', 'old-tree', 'old-providers',
                '1.2.0', '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z'
             )",
            params![old_generation_id, workspace.workspace_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE workspace SET active_generation_id = ?1 WHERE workspace_id = ?2",
            params![old_generation_id, workspace.workspace_id],
        )
        .unwrap();
        workspace.active_generation_id = Some(old_generation_id.to_string());

        let source_file_id = crate::ids::file_id_from_path(&workspace.workspace_id, "src/lib.rs");
        let test_file_id =
            crate::ids::file_id_from_path(&workspace.workspace_id, "tests/calculate.rs");
        let source_hash = crate::hashing::content_hash_of_bytes(source_bytes);
        let test_hash = crate::hashing::content_hash_of_bytes(test_bytes);
        for (path, bytes, file_id, revision_id, content_hash) in [
            (
                "src/lib.rs",
                source_bytes.as_slice(),
                source_file_id.as_str(),
                "opaque_source_revision",
                source_hash.as_str(),
            ),
            (
                "tests/calculate.rs",
                test_bytes.as_slice(),
                test_file_id.as_str(),
                "opaque_historical_test_as_source",
                test_hash.as_str(),
            ),
        ] {
            conn.execute(
                "INSERT INTO file_identity (
                    file_id, workspace_id, identity_basis, first_seen_at, last_seen_at,
                    lifecycle_state, last_known_path
                 ) VALUES (
                    ?1, ?2, 'created', '2026-09-03T00:00:00Z',
                    '2026-09-03T00:00:00Z', 'current', ?3
                 )",
                params![file_id, workspace.workspace_id, path],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_revision (
                    revision_id, file_id, content_hash, byte_size, artifact_class,
                    language, encoding, newline_style, project_key, package_key,
                    is_generated, is_test, discovered_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, 'source', NULL, 'utf-8', NULL, NULL, NULL,
                    0, 0, '2026-09-03T00:00:00Z'
                 )",
                params![
                    revision_id,
                    file_id,
                    content_hash,
                    i64::try_from(bytes.len()).unwrap()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO generation_file (
                    generation_id, file_id, revision_id, canonical_path, presence_state,
                    exclusion_code, exclusion_detail, observed_mtime_ns, observed_size
                 ) VALUES (?1, ?2, ?3, ?4, 'present', NULL, NULL, 0, ?5)",
                params![
                    old_generation_id,
                    file_id,
                    revision_id,
                    path,
                    i64::try_from(bytes.len()).unwrap()
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO extractor_run (
                extractor_run_id, revision_id, provider_name, provider_version,
                provider_tier, configuration_hash, normalized_schema_version,
                deterministic, status, started_at, finished_at, bytes_total,
                bytes_processed, capabilities_json, limitations_json
             ) VALUES (
                'historical_extractor', 'opaque_historical_test_as_source', 'text-fallback',
                '1.0.0', 'textual', 'old-config', '1.0.0', 1, 'complete',
                '2026-09-03T00:00:00Z', '2026-09-03T00:00:00Z', ?1, ?1, '[]', '[]'
             )",
            [i64::try_from(test_bytes.len()).unwrap()],
        )
        .unwrap();

        let history_before = non_migration_table_rows(&conn);
        apply_all(&conn).unwrap();
        assert_eq!(
            non_migration_table_rows(&conn),
            history_before,
            "0004 must not rewrite any historical table row"
        );
        integrity_check(&conn).unwrap();

        let report = crate::discovery::reconcile(&workspace, &conn, &config).unwrap();
        let current_source_revision: String = conn
            .query_row(
                "SELECT revision_id FROM generation_file
                 WHERE generation_id = ?1 AND canonical_path = 'src/lib.rs'",
                [&report.candidate_generation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_source_revision, "opaque_source_revision");
        let current_test_revision: (String, String, i64) = conn
            .query_row(
                "SELECT fr.revision_id, fr.artifact_class, fr.is_test
                 FROM generation_file gf
                 JOIN file_revision fr ON fr.revision_id = gf.revision_id
                 WHERE gf.generation_id = ?1
                   AND gf.canonical_path = 'tests/calculate.rs'",
                [&report.candidate_generation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_ne!(current_test_revision.0, "opaque_historical_test_as_source");
        assert!(current_test_revision.0.starts_with("rev_"));
        assert_eq!(current_test_revision.0.len(), "rev_".len() + 64);
        assert_eq!(current_test_revision.1, "test");
        assert_eq!(current_test_revision.2, 1);

        let historical_test: (String, i64, String) = conn
            .query_row(
                "SELECT artifact_class, is_test, content_hash FROM file_revision
                 WHERE revision_id = 'opaque_historical_test_as_source'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(historical_test, ("source".to_string(), 0, test_hash));
        assert_eq!(
            conn.query_row(
                "SELECT revision_id FROM generation_file
                 WHERE generation_id = ?1 AND canonical_path = 'tests/calculate.rs'",
                [old_generation_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "opaque_historical_test_as_source"
        );
        assert_eq!(
            conn.query_row(
                "SELECT schema_version FROM index_generation WHERE generation_id = ?1",
                [old_generation_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "1.2.0"
        );
        assert_eq!(
            conn.query_row(
                "SELECT schema_version FROM index_generation WHERE generation_id = ?1",
                [&report.candidate_generation_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "1.3.0"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM extractor_run
                 WHERE extractor_run_id = 'historical_extractor'
                   AND revision_id = 'opaque_historical_test_as_source'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM file_revision WHERE file_id = ?1",
                [&test_file_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM workspace WHERE workspace_id = ?1",
                [&workspace.workspace_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM file_identity
                 WHERE file_id IN (?1, ?2) AND workspace_id = ?3",
                params![source_file_id, test_file_id, workspace.workspace_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            2
        );
        integrity_check(&conn).unwrap();
    }

    #[test]
    fn split_pragmas_extracts_leading_pragmas() {
        let sql = "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nCREATE TABLE x(a INT);";
        let p = split_pragmas(sql);
        assert!(p.contains("foreign_keys"));
        assert!(p.contains("journal_mode"));
        assert!(!p.contains("CREATE TABLE"));
    }

    #[test]
    fn strip_pragmas_removes_leading_pragmas() {
        let sql = "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nCREATE TABLE x(a INT);";
        let s = strip_pragmas(sql);
        assert!(!s.contains("PRAGMA"));
        assert!(s.contains("CREATE TABLE"));
    }
}
