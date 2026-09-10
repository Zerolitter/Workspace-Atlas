//! Workspace registry.
//!
//! A workspace binds:
//! - a `workspace_id` (BLAKE3 of canonical_root + display_name, ADR-015),
//! - a canonical_root label (operator-visible),
//! - a configuration hash (invalidates revisions when policy changes),
//! - the catalogue path (outside the indexed root by default — INV-023),
//! - the active generation pointer (NULL until generation 1 is activated).
//!
//! INV-014: root confinement is enforced via `paths::confine_to_root` for every
//! path read during indexing; the workspace row itself stores the canonical
//! root for use as the confiner.

use rusqlite::{params, Connection, OptionalExtension};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{AtlasError, Result};
use crate::ids::workspace_id;
use crate::migrations;
use crate::paths::{confine_to_root, lexically_canonical, path_as_utf8};

/// In-memory representation of a registered workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceRecord {
    pub workspace_id: String,
    pub display_name: String,
    pub canonical_root: String,
    pub root_fingerprint: String,
    pub configuration_hash: String,
    pub policy_version: String,
    pub active_generation_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub catalogue_path: String,
}

/// Register a new workspace in the catalogue. Returns `Err(WorkspaceAlreadyRegistered)`
/// if the canonical root is already registered under a different `workspace_id`
/// (or if the same `workspace_id` already exists — `INSERT OR IGNORE` is
/// explicit so re-running `atlas init` on the same root is a no-op).
pub fn register_workspace(
    conn: &Connection,
    canonical_root: &Path,
    config: &Config,
    catalogue_path: &Path,
    policy_version: &str,
) -> Result<WorkspaceRecord> {
    let canonical_root_str = path_as_utf8(canonical_root, "canonical workspace root")?;
    let catalogue_path_str = path_as_utf8(catalogue_path, "catalogue path")?;
    let computed_id = workspace_id(canonical_root_str, &config.workspace.display_name);

    let root_fingerprint = blake3_root_fingerprint(canonical_root)?;
    let configuration_hash = config.configuration_hash();
    let now = migrations::iso8601_now();

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO workspace (
            workspace_id, display_name, canonical_root, root_fingerprint,
            configuration_hash, policy_version, active_generation_id,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7)",
        params![
            computed_id,
            config.workspace.display_name,
            canonical_root_str,
            root_fingerprint,
            configuration_hash,
            policy_version,
            now,
        ],
    )?;
    tx.commit()?;

    let record = load_workspace(conn, &computed_id)?.ok_or(AtlasError::WorkspaceNotFound {
        workspace_id: computed_id.clone(),
    })?;
    if record.configuration_hash != configuration_hash {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: workspace {} registered hash {} but initialization supplied {}",
            record.workspace_id, record.configuration_hash, configuration_hash
        )));
    }

    // The schema does not persist catalogue paths. Registration returns the
    // selected path as an out-of-band value.
    let mut record = record;
    record.catalogue_path = catalogue_path_str.to_owned();
    Ok(record)
}

const MAX_REGISTERED_CONFIG_BYTES: u64 = 1024 * 1024;
const HISTORICAL_PRE_REGEX_CONFIG_HASH: &str =
    "1c9c12cedc368263b3b813905cf637860af20ab7d677ccc591f762f84c38e3b3";
const REGEX_VALIDATION_CUTOFF: &str = "2026-09-02T17:41:39Z";

fn validate_registered_config(config: &Config, workspace: &WorkspaceRecord) -> Result<()> {
    match config.validate() {
        Ok(()) => Ok(()),
        Err(current_error) => {
            let effective_hash = config.configuration_hash();
            let derived_workspace_id =
                workspace_id(&workspace.canonical_root, &workspace.display_name);
            let derived_root_fingerprint =
                blake3_root_fingerprint(Path::new(&workspace.canonical_root))?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&workspace.created_at).ok();
            let cutoff = chrono::DateTime::parse_from_rfc3339(REGEX_VALIDATION_CUTOFF)
                .expect("fixed regex validation cutoff is valid");
            let has_legacy_provenance = effective_hash == HISTORICAL_PRE_REGEX_CONFIG_HASH
                && workspace.configuration_hash == HISTORICAL_PRE_REGEX_CONFIG_HASH
                && workspace.policy_version == "1.2.0"
                && config.workspace.display_name == workspace.display_name
                && workspace.workspace_id == derived_workspace_id
                && workspace.root_fingerprint == derived_root_fingerprint
                && created_at.is_some_and(|created_at| created_at < cutoff);

            if !has_legacy_provenance {
                return Err(current_error);
            }
            config.validate_historical_registered_globs()
        }
    }
}

/// Path of the application-owned configuration copy associated with a
/// catalogue. This uses the catalogue's existing storage directory and does
/// not alter the catalogue schema.
pub fn registered_config_path(catalogue_path: &Path) -> Result<PathBuf> {
    let file_name = catalogue_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AtlasError::InvalidConfig(format!(
                "configuration_required: catalogue path {} has no UTF-8 file name",
                catalogue_path.display()
            ))
        })?;
    let parent = catalogue_path.parent().ok_or_else(|| {
        AtlasError::InvalidConfig(format!(
            "configuration_required: catalogue path {} has no parent directory",
            catalogue_path.display()
        ))
    })?;
    Ok(parent
        .join("registered-configs")
        .join(format!("{file_name}.json")))
}

/// Load and verify the application-owned configuration copy, returning
/// `None` only when an older catalogue has not registered one yet.
pub fn inspect_registered_config(
    catalogue_path: &Path,
    workspace: &WorkspaceRecord,
) -> Result<Option<Config>> {
    let path = registered_config_path(catalogue_path)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: registered configuration {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_REGISTERED_CONFIG_BYTES {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: registered configuration exceeds {MAX_REGISTERED_CONFIG_BYTES} bytes"
        )));
    }
    let bytes = std::fs::read(&path)?;
    let config: Config = serde_json::from_slice(&bytes).map_err(|error| {
        AtlasError::InvalidConfig(format!(
            "configuration_mismatch: registered configuration cannot be decoded: {error}"
        ))
    })?;
    validate_registered_config(&config, workspace)?;
    let effective_hash = config.configuration_hash();
    if effective_hash != workspace.configuration_hash {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: registered hash {} does not match effective hash {}",
            workspace.configuration_hash, effective_hash
        )));
    }
    Ok(Some(config))
}

/// Load the exact policy registered for a workspace.
pub fn load_registered_config(
    catalogue_path: &Path,
    workspace: &WorkspaceRecord,
) -> Result<Config> {
    inspect_registered_config(catalogue_path, workspace)?.ok_or_else(|| {
        AtlasError::InvalidConfig(
            "configuration_required: this catalogue predates durable configuration registration; rerun reconcile once with --config <original-config>".into(),
        )
    })
}

/// Atomically register an immutable application-owned copy of a validated
/// configuration. Existing copies must decode to the same registered hash.
pub fn persist_registered_config(
    catalogue_path: &Path,
    workspace: &WorkspaceRecord,
    config: &Config,
) -> Result<()> {
    config.validate()?;
    let effective_hash = config.configuration_hash();
    if effective_hash != workspace.configuration_hash {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: registered hash {} does not match supplied hash {}",
            workspace.configuration_hash, effective_hash
        )));
    }
    if inspect_registered_config(catalogue_path, workspace)?.is_some() {
        return Ok(());
    }

    let path = registered_config_path(catalogue_path)?;
    let parent = path.parent().ok_or_else(|| {
        AtlasError::InvalidConfig(
            "configuration_required: catalogue has no parent directory".into(),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(config)?;
    if bytes.len() as u64 > MAX_REGISTERED_CONFIG_BYTES {
        return Err(AtlasError::InvalidConfig(format!(
            "configuration_mismatch: configuration exceeds {MAX_REGISTERED_CONFIG_BYTES} bytes"
        )));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            inspect_registered_config(catalogue_path, workspace)?.ok_or_else(|| {
                AtlasError::InvalidConfig(
                    "configuration_required: registered configuration disappeared during creation"
                        .into(),
                )
            })?;
            Ok(())
        }
        Err(error) => Err(error.error.into()),
    }
}

pub(crate) fn blake3_root_fingerprint(path: &Path) -> Result<String> {
    use blake3::Hasher;
    let canonical = lexically_canonical(path);
    let canonical = path_as_utf8(&canonical, "canonical workspace root")?;
    let mut h = Hasher::new();
    h.update(canonical.as_bytes());
    Ok(hex::encode(h.finalize().as_bytes()))
}

/// Load a workspace by id.
pub fn load_workspace(conn: &Connection, workspace_id: &str) -> Result<Option<WorkspaceRecord>> {
    let row: Option<WorkspaceRecord> = conn
        .query_row(
            "SELECT workspace_id, display_name, canonical_root, root_fingerprint,
                    configuration_hash, policy_version, active_generation_id,
                    created_at, updated_at
             FROM workspace WHERE workspace_id = ?1",
            params![workspace_id],
            |r| {
                Ok(WorkspaceRecord {
                    workspace_id: r.get(0)?,
                    display_name: r.get(1)?,
                    canonical_root: r.get(2)?,
                    root_fingerprint: r.get(3)?,
                    configuration_hash: r.get(4)?,
                    policy_version: r.get(5)?,
                    active_generation_id: r.get(6)?,
                    created_at: r.get(7)?,
                    updated_at: r.get(8)?,
                    catalogue_path: String::new(),
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Load a workspace by its canonical root (the canonical lookup key from the
/// CLI's perspective). The exact stored spelling is preferred. The legacy
/// fallback only accepts stored roots that are absolute, resolvable, and
/// already physically canonical.
pub fn load_workspace_by_root(
    conn: &Connection,
    canonical_root: &Path,
) -> Result<Option<WorkspaceRecord>> {
    let root_str = path_as_utf8(canonical_root, "canonical workspace root")?;
    let workspace_id: Option<String> = conn
        .query_row(
            "SELECT workspace_id FROM workspace WHERE canonical_root = ?1",
            params![root_str],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(workspace_id) = workspace_id {
        return load_workspace(conn, &workspace_id);
    }

    let requested_root = match std::fs::canonicalize(canonical_root) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let mut stmt =
        conn.prepare("SELECT workspace_id, canonical_root FROM workspace ORDER BY workspace_id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matches = Vec::new();
    for row in rows {
        let (workspace_id, stored_root) = row?;
        let stored_path = Path::new(&stored_root);
        if !stored_path.is_absolute() {
            return Err(legacy_stored_root_error(&stored_root, "relative"));
        }
        let resolved_root = std::fs::canonicalize(stored_path).map_err(|error| {
            legacy_stored_root_error(
                &stored_root,
                &format!("not a resolvable canonical path ({error})"),
            )
        })?;
        if lexically_canonical(stored_path) != stored_path
            || !stored_root_matches_resolved_path(stored_path, &resolved_root)
        {
            return Err(legacy_stored_root_error(&stored_root, "not canonical"));
        }
        if resolved_root == requested_root {
            matches.push(workspace_id);
        }
    }
    match matches.as_slice() {
        [] => Ok(None),
        [workspace_id] => load_workspace(conn, workspace_id),
        _ => Err(AtlasError::Other(format!(
            "multiple workspace records resolve to canonical root {}",
            requested_root.display()
        ))),
    }
}

fn stored_root_matches_resolved_path(stored_path: &Path, resolved_path: &Path) -> bool {
    stored_path == resolved_path
        || cfg!(windows) && paths_have_same_identity(stored_path, resolved_path)
}

/// Compare canonical path spellings without treating the Windows verbatim
/// prefix or separator style as part of filesystem identity.
pub(crate) fn paths_have_same_identity(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let (Some(left), Some(right)) = (left.to_str(), right.to_str()) else {
        return false;
    };
    let (Some((left_kind, left_body)), Some((right_kind, right_body))) = (
        windows_absolute_path_body(left),
        windows_absolute_path_body(right),
    ) else {
        return false;
    };
    left_kind == right_kind && windows_path_text_eq(left_body, right_body)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowsPathKind {
    Drive,
    Unc,
}

fn windows_absolute_path_body(path: &str) -> Option<(WindowsPathKind, &str)> {
    if let Some(body) = strip_windows_prefix(path, r"\\?\UNC\") {
        return Some((WindowsPathKind::Unc, body));
    }
    if let Some(body) = strip_windows_prefix(path, r"\\?\") {
        return is_windows_drive_absolute(body).then_some((WindowsPathKind::Drive, body));
    }
    if path.len() >= 2 && windows_path_text_eq(&path[..2], r"\\") {
        return Some((WindowsPathKind::Unc, &path[2..]));
    }
    is_windows_drive_absolute(path).then_some((WindowsPathKind::Drive, path))
}

fn strip_windows_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = path.get(..prefix.len())?;
    windows_path_text_eq(candidate, prefix).then(|| &path[prefix.len()..])
}

fn is_windows_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn windows_path_text_eq(left: &str, right: &str) -> bool {
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right))
                if (matches!(left, '\\' | '/') && matches!(right, '\\' | '/'))
                    || left.eq_ignore_ascii_case(&right) => {}
            _ => return false,
        }
    }
}

fn legacy_stored_root_error(stored_root: &str, reason: &str) -> AtlasError {
    AtlasError::Other(format!(
        "legacy catalogue stored workspace root {stored_root:?} is {reason}; Atlas will not resolve stored roots against the current directory. Re-run `atlas init <absolute-root>` to re-initialize routing, or pass `--catalogue <path>` explicitly for recovery"
    ))
}

/// Reject any path that would escape the workspace's canonical root. Returns
/// the canonical relative path on success.
pub fn ensure_within_workspace(ws: &WorkspaceRecord, candidate: &Path) -> Result<PathBuf> {
    let root = PathBuf::from(&ws.canonical_root);
    confine_to_root(&root, candidate)
}

/// Set the active generation pointer on a workspace. INV-005 requires this to
/// happen atomically with candidate activation (see `generation::activate_candidate`).
pub fn set_active_generation(
    conn: &Connection,
    workspace_id: &str,
    generation_id: Option<&str>,
) -> Result<()> {
    let now = migrations::iso8601_now();
    let updated = conn.execute(
        "UPDATE workspace SET active_generation_id = ?2, updated_at = ?3
         WHERE workspace_id = ?1",
        params![workspace_id, generation_id, now],
    )?;
    if updated == 0 {
        return Err(AtlasError::WorkspaceNotFound {
            workspace_id: workspace_id.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use tempfile::tempdir;

    fn test_config(display_name: &str) -> Config {
        let toml_text = format!(
            r#"
            schema_version = "1.0.0"
            [workspace]
            display_name = "{}"
            "#,
            display_name
        );
        Config::parse(&toml_text).unwrap()
    }

    fn fresh_db() -> (tempfile::TempDir, PathBuf, rusqlite::Connection) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config("t")).unwrap();
        (dir, path, conn)
    }

    #[test]
    fn windows_path_identity_accepts_normal_and_extended_drive_paths() {
        assert!(paths_have_same_identity(
            Path::new(r"C:/Users/<user>/AppData/Local/WorkspaceAtlas/catalogues/ws_test.sqlite"),
            Path::new(
                r"\\?\C:\Users\<user>\AppData\Local\WorkspaceAtlas\catalogues\ws_test.sqlite",
            ),
        ));
    }

    #[test]
    fn windows_path_identity_accepts_normal_and_extended_unc_paths() {
        assert!(paths_have_same_identity(
            Path::new(r"\\server\share\WorkspaceAtlas\catalogues\ws_test.sqlite"),
            Path::new(r"\\?\UNC\server\share\WorkspaceAtlas\catalogues\ws_test.sqlite"),
        ));
    }

    #[test]
    fn windows_path_identity_rejects_distinct_paths() {
        assert!(!paths_have_same_identity(
            Path::new(r"C:\<catalogue-root>\ws_one.sqlite"),
            Path::new(r"\\?\C:\<catalogue-root>\ws_two.sqlite"),
        ));
        assert!(!paths_have_same_identity(
            Path::new(r"\\server\share-a\catalogues\ws_test.sqlite"),
            Path::new(r"\\?\UNC\server\share-b\catalogues\ws_test.sqlite"),
        ));
    }

    #[test]
    fn register_creates_row() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cat_path = std::path::Path::new("/some/atlas.sqlite");
        let cfg = test_config("Example");
        let ws = register_workspace(&conn, root, &cfg, cat_path, "1.0.0").unwrap();
        assert!(ws.workspace_id.starts_with("ws_"));
        assert_eq!(ws.display_name, "Example");
        assert_eq!(ws.canonical_root, "/projects/example");
        assert_eq!(ws.active_generation_id, None);
    }

    #[test]
    fn register_is_idempotent_on_same_root() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cfg = test_config("Example");
        let ws1 =
            register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let ws2 =
            register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        assert_eq!(ws1.workspace_id, ws2.workspace_id);
        // Exactly one row.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM workspace", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn different_canonical_root_yields_different_workspace_id() {
        // `workspace.canonical_root` is unique. Two distinct canonical roots
        // registered under the same display name get different workspace IDs
        // under ADR-015.
        let (_dir, _path, conn) = fresh_db();
        let root1 = std::path::Path::new("/projects/example");
        let root2 = std::path::Path::new("/projects/other");
        let cfg = test_config("Example");
        let ws1 =
            register_workspace(&conn, root1, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let ws2 =
            register_workspace(&conn, root2, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        assert_ne!(ws1.workspace_id, ws2.workspace_id);
    }

    #[test]
    fn ensure_within_workspace_accepts_descendant() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cfg = test_config("Example");
        let ws = register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let rel =
            ensure_within_workspace(&ws, std::path::Path::new("/projects/example/src/lib.rs"))
                .unwrap();
        assert_eq!(rel, std::path::PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn ensure_within_workspace_rejects_escape() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cfg = test_config("Example");
        let ws = register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let err = ensure_within_workspace(&ws, std::path::Path::new("/projects/other/file.rs"))
            .unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn load_by_root_returns_record() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cfg = test_config("Example");
        let registered =
            register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let loaded = load_workspace_by_root(&conn, root).unwrap().unwrap();
        assert_eq!(registered.workspace_id, loaded.workspace_id);
    }

    #[test]
    fn set_active_generation_round_trips() {
        let (_dir, _path, conn) = fresh_db();
        let root = std::path::Path::new("/projects/example");
        let cfg = test_config("Example");
        let ws = register_workspace(&conn, root, &cfg, std::path::Path::new("a"), "1.0.0").unwrap();
        let gen = crate::generation::begin_candidate(
            &conn,
            &ws.workspace_id,
            crate::generation::TriggerKind::Manual,
            "p1",
            "1.0.0",
        )
        .unwrap();
        crate::generation::activate_candidate(&conn, &ws.workspace_id, &gen.generation_id, None)
            .unwrap();
        let loaded = load_workspace(&conn, &ws.workspace_id).unwrap().unwrap();
        assert_eq!(
            loaded.active_generation_id.as_deref(),
            Some(gen.generation_id.as_str())
        );
        set_active_generation(&conn, &ws.workspace_id, None).unwrap();
    }
}
