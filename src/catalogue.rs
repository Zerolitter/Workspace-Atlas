//! SQLite catalogue connection wrapper.
//!
//! INV-020: only one writer may operate on a workspace catalogue at a time.
//! INV-014: every read resolves under an approved root.
//! INV-005: candidate activations happen inside `BEGIN IMMEDIATE` with the
//!   active-generation pointer update in the same transaction.

use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::{AtlasError, Result};
use crate::migrations;

const SCHEMA_WRITER_VERSION: i64 = 4;

/// Default writer lease duration: comfortably longer than a normal reconcile,
/// but short enough to recover a stale lease after a crash.
pub const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(300);

/// Default heartbeat staleness threshold. The lease is considered stale if
/// the heartbeat is older than this when another writer attempts to acquire.
pub const DEFAULT_HEARTBEAT_STALE: Duration = Duration::from_secs(60);

/// Open a SQLite connection at `path`, applying the required foundation
/// PRAGMAs: foreign keys, WAL, normal synchronization, a bounded busy timeout,
/// and the migration-controlled schema version.
///
/// If the file does not exist, it is created.
pub fn open_connection(path: &Path, config: &Config) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    register_schema_writer_version(&conn)?;
    apply_required_pragmas(&conn, config)?;
    Ok(conn)
}

/// Register the connection-local schema writer identity consumed by schema-4
/// compatibility triggers. Connections from preserved older binaries do not
/// expose this function and therefore fail closed before guarded mutation.
pub(crate) fn register_schema_writer_version(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "atlas_schema_writer_version",
        0,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(SCHEMA_WRITER_VERSION),
    )?;
    Ok(())
}

/// Apply the required safety and concurrency PRAGMAs to a new connection.
pub fn apply_required_pragmas(conn: &Connection, _config: &Config) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// Initialise a catalogue: create the schema (fresh database) or apply any
/// pending migrations (existing database) and return a validated connection.
///
/// Migration backup and retention are caller-owned. Live migration requires
/// an explicit, writer-stopped, WAL-consistent external backup.
pub fn init_catalogue(path: &Path, config: &Config) -> Result<Connection> {
    let conn = open_connection(path, config)?;
    migrations::apply_all(&conn)?;
    Ok(conn)
}

/// Acquire the writer lease for a workspace.
#[derive(Debug)]
pub struct WriterLeaseGuard {
    pub holder_id: String,
    pub workspace_id: String,
    pub acquired_at: String,
    pub expires_at: String,
}

pub fn acquire_writer_lease(
    conn: &Connection,
    workspace_id: &str,
    holder_id: &str,
    duration: Duration,
    heartbeat_stale: Duration,
) -> Result<WriterLeaseGuard> {
    let now = migrations::iso8601_now();
    let expires = now_plus_iso8601(duration);
    let heartbeat = now.clone();

    let tx = conn.unchecked_transaction()?;
    let existing: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT holder_id, acquired_at, expires_at, heartbeat_at
             FROM writer_lease WHERE workspace_id = ?1",
            params![workspace_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    let acquire = match &existing {
        None => true,
        Some((existing_holder, _, _, hb)) => {
            existing_holder == holder_id || iso8601_age_seconds(hb, heartbeat_stale)?
        }
    };

    if !acquire {
        let holder = existing
            .as_ref()
            .map(|(h, _, _, _)| h.clone())
            .unwrap_or_default();
        return Err(AtlasError::WriterLeaseHeld { holder });
    }

    tx.execute(
        "INSERT INTO writer_lease(
            workspace_id, holder_id, acquired_at, expires_at, heartbeat_at,
            schema_writer_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, 4)
         ON CONFLICT(workspace_id) DO UPDATE SET
            acquired_at = excluded.acquired_at,
            expires_at = excluded.expires_at,
            heartbeat_at = excluded.heartbeat_at,
            schema_writer_version = excluded.schema_writer_version",
        params![workspace_id, holder_id, now, expires, heartbeat],
    )?;
    tx.commit()?;

    Ok(WriterLeaseGuard {
        holder_id: holder_id.to_string(),
        workspace_id: workspace_id.to_string(),
        acquired_at: now,
        expires_at: expires,
    })
}

/// Refresh the heartbeat on the writer lease.
pub fn heartbeat_writer_lease(
    conn: &Connection,
    workspace_id: &str,
    holder_id: &str,
) -> Result<()> {
    let now = migrations::iso8601_now();
    let updated = conn.execute(
        "UPDATE writer_lease
         SET heartbeat_at = ?1, expires_at = ?2
         WHERE workspace_id = ?3 AND holder_id = ?4",
        params![
            now,
            now_plus_iso8601(DEFAULT_LEASE_DURATION),
            workspace_id,
            holder_id
        ],
    )?;
    if updated == 0 {
        return Err(AtlasError::WriterLeaseExpired { stale_secs: 0 });
    }
    Ok(())
}

/// Release the writer lease. Only succeeds if `holder_id` matches the row.
pub fn release_writer_lease(conn: &Connection, workspace_id: &str, holder_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM writer_lease WHERE workspace_id = ?1 AND holder_id = ?2",
        params![workspace_id, holder_id],
    )?;
    Ok(())
}

/// Holder identity used by the CLI: hostname + process id + a random suffix.
pub fn default_holder_id() -> String {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{hostname}-{pid}-{nanos:x}")
}

fn now_plus_iso8601(d: Duration) -> String {
    let now = SystemTime::now()
        .checked_add(d)
        .unwrap_or_else(SystemTime::now);
    let total_ms = now
        .duration_since(UNIX_EPOCH)
        .map(|x| x.as_millis())
        .unwrap_or(0);
    let secs = (total_ms / 1000) as i64;
    let ms = (total_ms % 1000) as u32;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
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

fn iso8601_age_seconds(iso: &str, stale_after: Duration) -> Result<bool> {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso)
        .map_err(|e| AtlasError::Other(format!("invalid timestamp {iso:?}: {e}")))?;
    let now = chrono::Utc::now();
    let age = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
    let limit = chrono::Duration::from_std(stale_after).unwrap_or(chrono::Duration::seconds(60));
    Ok(age > limit)
}

/// Catalogue handle combining the SQLite connection with a writer lease.
#[derive(Clone)]
pub struct Catalogue {
    inner: Rc<Connection>,
}

impl Catalogue {
    pub fn from_connection(conn: Connection) -> Self {
        Self {
            inner: Rc::new(conn),
        }
    }

    pub fn handle(&self) -> &Connection {
        &self.inner
    }

    pub fn acquire_lease(
        &self,
        workspace_id: &str,
        holder_id: &str,
        duration: Duration,
        heartbeat_stale: Duration,
    ) -> Result<WriterLeaseGuard> {
        acquire_writer_lease(
            &self.inner,
            workspace_id,
            holder_id,
            duration,
            heartbeat_stale,
        )
    }
}

/// Exact literal required by the internal irreversible-removal boundary.
pub const IRREVERSIBLE_CONFIRMATION: &str = "--irreversible";
const UNREGISTER_DISCLOSURE: &str = "Successful unregister may delete the complete Atlas catalogue, including indexed Truth Plane evidence and all generation history. Workspace source and caller-owned backups are excluded; Atlas creates no backup or export.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisterTarget {
    workspace_root: PathBuf,
    catalogue_path: PathBuf,
    locator_path: PathBuf,
    application_temp_root: PathBuf,
}

impl UnregisterTarget {
    pub fn new(
        workspace_root: PathBuf,
        catalogue_path: PathBuf,
        application_catalogue_root: PathBuf,
        application_temp_root: PathBuf,
    ) -> Result<Self> {
        let workspace_root = std::fs::canonicalize(&workspace_root).map_err(|error| {
            AtlasError::Other(format!(
                "unregister workspace root {} cannot be resolved: {error}",
                workspace_root.display()
            ))
        })?;
        if !workspace_root.is_dir() {
            return Err(AtlasError::Other(format!(
                "unregister workspace root {} is not a directory",
                workspace_root.display()
            )));
        }
        let locator_path =
            crate::paths::catalogue_locator_path(&application_catalogue_root, &workspace_root)?;
        Ok(Self {
            workspace_root,
            catalogue_path,
            locator_path,
            application_temp_root,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UnregisterPathKind {
    Catalogue,
    CatalogueWal,
    CatalogueShm,
    RegisteredConfig,
    RootLocator,
    ProviderTemp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnregisterManifestEntry {
    pub kind: UnregisterPathKind,
    pub path: PathBuf,
    pub byte_len: u64,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisterPage {
    pub workspace_id: String,
    pub catalogue_path: PathBuf,
    pub manifest_digest: String,
    pub total_entries: usize,
    pub entries: Vec<UnregisterManifestEntry>,
    pub next_cursor: Option<usize>,
    pub disclosure: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnregisterFault {
    None,
    BeforeQuarantine,
    DuringQuarantine,
    DuringRemoval,
}

#[derive(serde::Deserialize)]
struct UnregisterLocator {
    schema_version: u32,
    canonical_root: String,
    workspace_id: String,
    catalogue_path: String,
}

#[derive(serde::Serialize)]
struct UnregisterIdentity<'a> {
    schema: &'static str,
    workspace_id: &'a str,
    canonical_root: &'a str,
    catalogue_path: &'a str,
    entries: &'a [UnregisterManifestEntry],
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn canonical_identity_text(path: &Path) -> Result<String> {
    let value = crate::paths::path_as_utf8(path, "unregister path")?.replace('\\', "/");
    #[cfg(windows)]
    let value = value.to_ascii_lowercase();
    Ok(value)
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn fingerprint_unregister_path(
    path: &Path,
    allow_directory: bool,
) -> Result<Option<(PathBuf, u64, String)>> {
    const MAX_TEMP_MANIFEST_FILES: usize = 10_000;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(AtlasError::Other(format!(
            "unregister target {} is a symlink or reparse point",
            path.display()
        )));
    }
    if metadata.is_file() {
        let canonical = crate::paths::canonical_regular_file(path, "unregister target")?;
        return Ok(Some((
            canonical.clone(),
            metadata.len(),
            crate::hashing::content_hash_of_file(&canonical)?,
        )));
    }
    if !allow_directory || !metadata.is_dir() {
        return Err(AtlasError::Other(format!(
            "unregister target {} is not a regular file",
            path.display()
        )));
    }

    let root = std::fs::canonicalize(path)?;
    let mut pending = vec![root.clone()];
    let mut files = Vec::new();
    let mut byte_len = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let entry_path = entry.path();
            let entry_metadata = std::fs::symlink_metadata(&entry_path)?;
            if entry_metadata.file_type().is_symlink() || is_reparse_point(&entry_metadata) {
                return Err(AtlasError::Other(format!(
                    "unregister provider temporary state contains a symlink or reparse point: {}",
                    entry_path.display()
                )));
            }
            let canonical = std::fs::canonicalize(&entry_path)?;
            if !canonical.starts_with(&root) {
                return Err(AtlasError::Other(format!(
                    "unregister provider temporary state escapes its root: {}",
                    canonical.display()
                )));
            }
            if entry_metadata.is_dir() {
                pending.push(canonical);
            } else if entry_metadata.is_file() {
                if files.len() == MAX_TEMP_MANIFEST_FILES {
                    return Err(AtlasError::Other(format!(
                        "unregister provider temporary state exceeds {MAX_TEMP_MANIFEST_FILES} files"
                    )));
                }
                byte_len = byte_len.checked_add(entry_metadata.len()).ok_or_else(|| {
                    AtlasError::Other("unregister provider temporary byte count overflow".into())
                })?;
                let relative = canonical.strip_prefix(&root).map_err(|_| {
                    AtlasError::Other(
                        "unregister provider temporary state escaped canonical root".into(),
                    )
                })?;
                files.push((
                    canonical_identity_text(relative)?,
                    entry_metadata.len(),
                    crate::hashing::content_hash_of_file(&canonical)?,
                ));
            } else {
                return Err(AtlasError::Other(format!(
                    "unregister provider temporary state contains a non-regular entry: {}",
                    entry_path.display()
                )));
            }
        }
    }
    files.sort();
    let content_digest = crate::hashing::content_hash_of_bytes(
        &crate::provider_contract::canonical_json_bytes(&files),
    );
    Ok(Some((root, byte_len, content_digest)))
}

fn validated_unregister_manifest(
    conn: &Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: &UnregisterTarget,
) -> Result<(Vec<UnregisterManifestEntry>, String, PathBuf)> {
    let root = std::fs::canonicalize(&target.workspace_root).map_err(|error| {
        AtlasError::Other(format!(
            "unregister workspace root {} cannot be resolved: {error}",
            target.workspace_root.display()
        ))
    })?;
    if !root.is_dir()
        || !crate::workspace::paths_have_same_identity(&root, Path::new(&workspace.canonical_root))
    {
        return Err(AtlasError::Other(
            "unregister workspace identity changed".into(),
        ));
    }

    let catalogue =
        crate::paths::canonical_regular_file(&target.catalogue_path, "unregister catalogue")?;
    let opened = conn
        .path()
        .ok_or_else(|| AtlasError::Other("unregister requires a file-backed catalogue".into()))?;
    let opened = crate::paths::canonical_regular_file(Path::new(opened), "opened catalogue")?;
    if !crate::workspace::paths_have_same_identity(&catalogue, &opened) {
        return Err(AtlasError::Other(
            "unregister catalogue identity changed".into(),
        ));
    }
    let current =
        crate::workspace::load_workspace(conn, &workspace.workspace_id)?.ok_or_else(|| {
            AtlasError::WorkspaceNotFound {
                workspace_id: workspace.workspace_id.clone(),
            }
        })?;
    if current.canonical_root != workspace.canonical_root
        || current.configuration_hash != workspace.configuration_hash
        || current.active_generation_id != workspace.active_generation_id
    {
        return Err(AtlasError::Other(
            "unregister catalogue workspace state changed".into(),
        ));
    }

    let locator = crate::paths::canonical_regular_file(&target.locator_path, "unregister locator")?;
    let locator_value: UnregisterLocator = serde_json::from_slice(&std::fs::read(&locator)?)
        .map_err(|error| AtlasError::Other(format!("unregister locator is corrupt: {error}")))?;
    if locator_value.schema_version != 1
        || locator_value.workspace_id != workspace.workspace_id
        || !crate::workspace::paths_have_same_identity(
            Path::new(&locator_value.canonical_root),
            &root,
        )
        || !crate::workspace::paths_have_same_identity(
            Path::new(&locator_value.catalogue_path),
            &catalogue,
        )
    {
        return Err(AtlasError::Other("unregister locator route changed".into()));
    }

    let mut provider_statement = conn.prepare(
        "SELECT provider_execution_id
         FROM provider_execution
         WHERE workspace_id = ?1
         ORDER BY provider_execution_id",
    )?;
    let provider_temp_paths = provider_statement
        .query_map(params![workspace.workspace_id], |row| {
            row.get::<_, String>(0)
        })?
        .map(|execution_id| {
            execution_id.map(|execution_id| target.application_temp_root.join(execution_id))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut candidates = vec![
        (UnregisterPathKind::Catalogue, catalogue.clone(), false),
        (
            UnregisterPathKind::CatalogueWal,
            sqlite_sidecar(&target.catalogue_path, "-wal"),
            false,
        ),
        (
            UnregisterPathKind::CatalogueShm,
            sqlite_sidecar(&target.catalogue_path, "-shm"),
            false,
        ),
        (
            UnregisterPathKind::RegisteredConfig,
            crate::workspace::registered_config_path(&target.catalogue_path)?,
            false,
        ),
        (UnregisterPathKind::RootLocator, locator, false),
    ];
    candidates.extend(
        provider_temp_paths
            .into_iter()
            .map(|path| (UnregisterPathKind::ProviderTemp, path, true)),
    );
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut entries = Vec::new();
    for (kind, path, allow_directory) in candidates {
        let Some((canonical, byte_len, content_digest)) =
            fingerprint_unregister_path(&path, allow_directory)?
        else {
            continue;
        };
        if canonical.starts_with(&root) {
            return Err(AtlasError::Other(format!(
                "unregister manifest cannot include workspace source {}",
                canonical.display()
            )));
        }
        entries.push(UnregisterManifestEntry {
            kind,
            path: canonical,
            byte_len,
            content_digest,
        });
    }
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.path.cmp(&right.path))
    });
    let root_text = canonical_identity_text(&root)?;
    let catalogue_text = canonical_identity_text(&catalogue)?;
    let identity = UnregisterIdentity {
        schema: "unregister-manifest-v1",
        workspace_id: &workspace.workspace_id,
        canonical_root: &root_text,
        catalogue_path: &catalogue_text,
        entries: &entries,
    };
    let digest = crate::hashing::content_hash_of_bytes(
        &crate::provider_contract::canonical_json_bytes(&identity),
    );
    Ok((entries, digest, catalogue))
}

/// Compute a bounded presentation page over the complete Atlas-owned removal
/// manifest. Adjacent caller-owned backups are never discovered or included.
pub fn unregister_preview(
    conn: &Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: &UnregisterTarget,
    limit: usize,
    cursor: Option<usize>,
) -> Result<UnregisterPage> {
    if !(1..=200).contains(&limit) {
        return Err(AtlasError::Other(
            "unregister preview limit must be in 1..=200".into(),
        ));
    }
    let (entries, manifest_digest, catalogue_path) =
        validated_unregister_manifest(conn, workspace, target)?;
    let start = cursor.unwrap_or(0);
    if start > entries.len() {
        return Err(AtlasError::Other(
            "unregister preview cursor is invalid".into(),
        ));
    }
    let end = start.saturating_add(limit).min(entries.len());
    Ok(UnregisterPage {
        workspace_id: workspace.workspace_id.clone(),
        catalogue_path,
        manifest_digest,
        total_entries: entries.len(),
        entries: entries[start..end].to_vec(),
        next_cursor: (end < entries.len()).then_some(end),
        disclosure: UNREGISTER_DISCLOSURE,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApplicationUnregisterPage {
    pub workspace_id: String,
    pub catalogue_path: PathBuf,
    pub manifest_digest: String,
    pub total_entries: usize,
    pub entries: Vec<UnregisterManifestEntry>,
    pub next_cursor: Option<String>,
    pub disclosure: &'static str,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UnregisterCursorState {
    manifest_digest: String,
    prefix_digest: String,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnregisterOrderingKey {
    kind: u8,
    path_hash: String,
}

fn unregister_entry_key(entry: &UnregisterManifestEntry) -> UnregisterOrderingKey {
    UnregisterOrderingKey {
        kind: match entry.kind {
            UnregisterPathKind::Catalogue => 0,
            UnregisterPathKind::CatalogueWal => 1,
            UnregisterPathKind::CatalogueShm => 2,
            UnregisterPathKind::RegisteredConfig => 3,
            UnregisterPathKind::RootLocator => 4,
            UnregisterPathKind::ProviderTemp => 5,
        },
        path_hash: crate::hashing::content_hash_of_bytes(entry.path.to_string_lossy().as_bytes()),
    }
}
fn unregister_prefix_digest(
    entries: &[UnregisterManifestEntry],
    through: &UnregisterOrderingKey,
) -> Option<String> {
    let mut digest =
        crate::context_application::ApplicationSnapshotHasher::new(b"unregister-manifest-prefix");
    for entry in entries {
        digest.record(entry);
        if unregister_entry_key(entry) == *through {
            return Some(digest.finish());
        }
    }
    None
}

pub fn application_unregister_preview(
    conn: &Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: &UnregisterTarget,
    limit: usize,
    cursor: Option<&str>,
) -> std::result::Result<
    ApplicationUnregisterPage,
    crate::context_application::CatalogueContextApplicationError,
> {
    if !(1..=crate::context_route::MAX_APPLICATION_PAGE_LIMIT).contains(&limit) {
        return Err(AtlasError::InvalidConfig(format!(
            "limit must be within 1..={}",
            crate::context_route::MAX_APPLICATION_PAGE_LIMIT
        ))
        .into());
    }
    let identity_binding = crate::context_application::ApplicationCursorBinding {
        workspace_id: &workspace.workspace_id,
        operation: crate::context_application::ApplicationCursorOperation::UnregisterPreview,
        contract_version: "unregister-manifest-v1",
        filter: crate::context_application::ApplicationCursorFilter::Unregister {},
        captured_state: "",
    };
    let decoded = if let Some(token) = cursor {
        Some(
            crate::context_application::decode_application_cursor_snapshot(
                conn,
                workspace,
                &identity_binding,
                token,
            )?,
        )
    } else {
        None
    };
    let (entries, manifest_digest, catalogue_path) =
        validated_unregister_manifest(conn, workspace, target)?;
    let ordering = if let Some(decoded) = decoded.as_ref() {
        let state: UnregisterCursorState = serde_json::from_str(&decoded.captured_state)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        let ordering: UnregisterOrderingKey = serde_json::from_str(&decoded.ordering_key)
            .map_err(|_| crate::context_application::ApplicationCursorError::Invalid)?;
        if state.manifest_digest != manifest_digest
            || unregister_prefix_digest(&entries, &ordering).as_ref() != Some(&state.prefix_digest)
        {
            return Err(crate::context_application::ApplicationCursorError::Stale.into());
        }
        Some(ordering)
    } else {
        None
    };
    let start = if let Some(ordering) = ordering {
        entries
            .iter()
            .position(|entry| unregister_entry_key(entry) == ordering)
            .map(|index| index + 1)
            .ok_or(crate::context_application::ApplicationCursorError::Stale)?
    } else {
        0
    };
    let end = start.saturating_add(limit).min(entries.len());
    let page_entries = entries[start..end].to_vec();
    let next_cursor = if end < entries.len() {
        let ordering = unregister_entry_key(
            page_entries
                .last()
                .ok_or_else(|| AtlasError::InvalidConfig("cursor_invalid".into()))?,
        );
        let prefix_digest = unregister_prefix_digest(&entries, &ordering)
            .ok_or(crate::context_application::ApplicationCursorError::Stale)?;
        let captured_state = serde_json::to_string(&UnregisterCursorState {
            manifest_digest: manifest_digest.clone(),
            prefix_digest,
        })?;
        let binding = crate::context_application::ApplicationCursorBinding {
            captured_state: &captured_state,
            ..identity_binding
        };
        Some(crate::context_application::encode_application_cursor(
            conn,
            workspace,
            &binding,
            &serde_json::to_string(&ordering)?,
        )?)
    } else {
        None
    };
    Ok(ApplicationUnregisterPage {
        workspace_id: workspace.workspace_id.clone(),
        catalogue_path,
        manifest_digest,
        total_entries: entries.len(),
        entries: page_entries,
        next_cursor,
        disclosure: UNREGISTER_DISCLOSURE,
    })
}

struct UnregisterLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl UnregisterLock {
    fn acquire(catalogue_path: &Path) -> Result<Self> {
        let lock_path = sqlite_sidecar(catalogue_path, ".unregister.lock");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                AtlasError::Other(format!(
                    "unregister exclusion lock {} unavailable: {error}",
                    lock_path.display()
                ))
            })?;
        Ok(Self {
            path: lock_path,
            _file: file,
        })
    }
}

impl Drop for UnregisterLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Fail-closed unregister application boundary.
///
/// The current storage layout spans independently rooted catalogue, config,
/// and locator files. No supported platform offers an atomic multi-root
/// quarantine plus rollback-safe recursive removal. The frozen contract
/// requires unavailability rather than a partial protocol, so this function
/// revalidates under external exclusion, closes SQLite, and returns
/// `unregister_unavailable` without moving or deleting any manifested file.
#[derive(Debug)]
enum UnregisterApplyError {
    Atlas(AtlasError),
    LiteralConfirmation,
    ManifestMismatch { expected: String, found: String },
    LockUnavailable(String),
    ActiveWriter,
    AtomicUnavailable(String),
}

impl From<AtlasError> for UnregisterApplyError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<rusqlite::Error> for UnregisterApplyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Atlas(error.into())
    }
}

fn apply_unregister_inner(
    conn: Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: UnregisterTarget,
    expected_digest: &str,
    confirmation: &str,
    fault: UnregisterFault,
) -> std::result::Result<(), UnregisterApplyError> {
    if confirmation != IRREVERSIBLE_CONFIRMATION {
        return Err(UnregisterApplyError::LiteralConfirmation);
    }
    let _lock = UnregisterLock::acquire(&target.catalogue_path)
        .map_err(|error| UnregisterApplyError::LockUnavailable(error.to_string()))?;
    let (_, digest, _) = validated_unregister_manifest(&conn, workspace, &target)?;
    if digest != expected_digest {
        return Err(UnregisterApplyError::ManifestMismatch {
            expected: expected_digest.to_string(),
            found: digest,
        });
    }
    let active_writer: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM writer_lease WHERE workspace_id = ?1)",
        params![workspace.workspace_id],
        |row| row.get(0),
    )?;
    if active_writer {
        return Err(UnregisterApplyError::ActiveWriter);
    }
    if matches!(
        fault,
        UnregisterFault::BeforeQuarantine | UnregisterFault::DuringQuarantine
    ) {
        let boundary = if fault == UnregisterFault::BeforeQuarantine {
            "before quarantine"
        } else {
            "during quarantine"
        };
        return Err(AtlasError::Other(format!("injected unregister fault {boundary}")).into());
    }
    conn.close()
        .map_err(|(_, error)| AtlasError::Sqlite(error))?;
    let boundary = match fault {
        UnregisterFault::DuringRemoval => "removal fault boundary",
        UnregisterFault::None
        | UnregisterFault::BeforeQuarantine
        | UnregisterFault::DuringQuarantine => "atomic removal protocol",
    };
    Err(UnregisterApplyError::AtomicUnavailable(format!(
        "{boundary} cannot preserve all-or-nothing logical registration for the current multi-root storage layout"
    )))
}

pub fn application_apply_unregister(
    conn: Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: UnregisterTarget,
    expected_digest: &str,
    confirmation: &str,
    fault: UnregisterFault,
) -> std::result::Result<(), crate::context_application::CatalogueContextApplicationError> {
    apply_unregister_inner(
        conn,
        workspace,
        target,
        expected_digest,
        confirmation,
        fault,
    )
    .map_err(|error| match error {
        UnregisterApplyError::Atlas(error) => error.into(),
        UnregisterApplyError::LiteralConfirmation => {
            crate::context_application::ApplicationBoundaryError::LiteralConfirmation {
                operation: "unregister",
                required: IRREVERSIBLE_CONFIRMATION,
            }
            .into()
        }
        UnregisterApplyError::ManifestMismatch { expected, found } => {
            crate::context_application::ApplicationBoundaryError::ManifestMismatch {
                operation: "unregister",
                expected,
                found,
            }
            .into()
        }
        UnregisterApplyError::LockUnavailable(reason) => {
            crate::context_application::ApplicationBoundaryError::UnregisterUnavailable { reason }
                .into()
        }
        UnregisterApplyError::ActiveWriter => {
            crate::context_application::ApplicationBoundaryError::UnregisterUnavailable {
                reason: "a catalogue writer is active".into(),
            }
            .into()
        }
        UnregisterApplyError::AtomicUnavailable(reason) => {
            crate::context_application::ApplicationBoundaryError::UnregisterUnavailable { reason }
                .into()
        }
    })
}

pub fn apply_unregister(
    conn: Connection,
    workspace: &crate::workspace::WorkspaceRecord,
    target: UnregisterTarget,
    expected_digest: &str,
    confirmation: &str,
    fault: UnregisterFault,
) -> Result<()> {
    apply_unregister_inner(
        conn,
        workspace,
        target,
        expected_digest,
        confirmation,
        fault,
    )
    .map_err(|error| match error {
        UnregisterApplyError::Atlas(error) => error,
        UnregisterApplyError::LiteralConfirmation => {
            AtlasError::Other("unregister requires literal --irreversible confirmation".into())
        }
        UnregisterApplyError::ManifestMismatch { expected, found } => AtlasError::Other(format!(
            "unregister manifest mismatch: expected {expected}, found {found}"
        )),
        UnregisterApplyError::LockUnavailable(reason) => AtlasError::Other(reason),
        UnregisterApplyError::ActiveWriter => {
            AtlasError::Other("unregister unavailable while a catalogue writer is active".into())
        }
        UnregisterApplyError::AtomicUnavailable(reason) => {
            AtlasError::Other(format!("unregister_unavailable: {reason}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_config() -> Config {
        let toml_text = r#"
            schema_version = "1.0.0"
            [workspace]
            display_name = "test"
            "#;
        Config::parse(toml_text).unwrap()
    }

    #[test]
    fn init_catalogue_creates_db_and_passes_integrity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config())
            .unwrap_or_else(|e| panic!("init_catalogue failed: {e:?}"));
        let v = migrations::applied_version(&conn).unwrap();
        assert_eq!(v, Some(4));
        let user = migrations::user_version(&conn).unwrap();
        assert_eq!(user, 4);
    }

    #[test]
    fn init_catalogue_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        init_catalogue(&path, &test_config()).unwrap();
        let conn = open_connection(&path, &test_config()).unwrap();
        migrations::integrity_check(&conn).unwrap();
    }

    #[test]
    fn reopening_existing_catalogue_creates_no_implicit_backup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        init_catalogue(&path, &test_config()).unwrap();
        drop(open_connection(&path, &test_config()));

        init_catalogue(&path, &test_config()).unwrap();

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".pre-migration-")
            })
            .collect();
        assert!(
            backups.is_empty(),
            "Atlas must not create or retain implicit migration backups: {backups:?}"
        );
    }

    #[test]
    fn foreign_keys_are_on_after_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config()).unwrap();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1);
    }

    #[test]
    fn writer_lease_blocks_second_holder() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config()).unwrap();
        let cfg = test_config();
        let ws = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/projects/example"),
            &cfg,
            &path,
            "1.0.0",
        )
        .unwrap();
        let ws_x: &str = &ws.workspace_id;
        let h1 = "holder-1";
        let h2 = "holder-2";

        acquire_writer_lease(
            &conn,
            ws_x,
            h1,
            Duration::from_secs(60),
            Duration::from_secs(30),
        )
        .unwrap();

        acquire_writer_lease(
            &conn,
            ws_x,
            h1,
            Duration::from_secs(60),
            Duration::from_secs(30),
        )
        .unwrap();

        let err = acquire_writer_lease(
            &conn,
            ws_x,
            h2,
            Duration::from_secs(60),
            Duration::from_secs(30),
        )
        .unwrap_err();
        assert!(matches!(err, AtlasError::WriterLeaseHeld { .. }));

        release_writer_lease(&conn, ws_x, h1).unwrap();
        acquire_writer_lease(
            &conn,
            ws_x,
            h2,
            Duration::from_secs(60),
            Duration::from_secs(30),
        )
        .unwrap();
    }

    #[test]
    fn current_writer_marker_fences_legacy_lease_upsert_before_conflict() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config()).unwrap();
        let cfg = test_config();
        let workspace = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/projects/example"),
            &cfg,
            &path,
            "1.3.0",
        )
        .unwrap();

        acquire_writer_lease(
            &conn,
            &workspace.workspace_id,
            "current-holder",
            Duration::from_secs(60),
            Duration::from_secs(30),
        )
        .unwrap();
        let before: (String, String, String, String, i64) = conn
            .query_row(
                "SELECT holder_id, acquired_at, expires_at, heartbeat_at, schema_writer_version
                 FROM writer_lease WHERE workspace_id = ?1",
                params![workspace.workspace_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(before.4, 4);

        let error = conn
            .execute(
                "INSERT INTO writer_lease(
                    workspace_id, holder_id, acquired_at, expires_at, heartbeat_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(workspace_id) DO UPDATE SET
                    acquired_at = excluded.acquired_at,
                    expires_at = excluded.expires_at,
                    heartbeat_at = excluded.heartbeat_at",
                params![
                    workspace.workspace_id,
                    "legacy-holder",
                    "2000-01-01T00:00:00.000Z",
                    "2000-01-01T00:01:00.000Z",
                    "2000-01-01T00:00:00.000Z"
                ],
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("schema-4 writer marker required"),
            "unexpected legacy writer refusal: {error}"
        );

        let after: (String, String, String, String, i64) = conn
            .query_row(
                "SELECT holder_id, acquired_at, expires_at, heartbeat_at, schema_writer_version
                 FROM writer_lease WHERE workspace_id = ?1",
                params![workspace.workspace_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            after, before,
            "legacy refusal must not update the lease row"
        );
    }

    #[test]
    fn guarded_mutation_rejects_absent_or_wrong_connection_writer_identity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config()).unwrap();
        let cfg = test_config();
        let workspace = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/projects/example"),
            &cfg,
            &path,
            "1.3.0",
        )
        .unwrap();
        let candidate = crate::generation::begin_candidate(
            &conn,
            &workspace.workspace_id,
            crate::generation::TriggerKind::Manual,
            "provider-set",
            "1.3.0",
        )
        .unwrap();
        drop(conn);

        let legacy = Connection::open(&path).unwrap();
        let absent_error = legacy
            .execute(
                "UPDATE index_generation SET state = 'abandoned'
                 WHERE generation_id = ?1",
                params![candidate.generation_id],
            )
            .unwrap_err();
        assert!(
            absent_error
                .to_string()
                .contains("no such function: atlas_schema_writer_version"),
            "unexpected absent writer identity refusal: {absent_error}"
        );
        legacy
            .create_scalar_function(
                "atlas_schema_writer_version",
                0,
                FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
                |_| Ok(3_i64),
            )
            .unwrap();
        let wrong_error = legacy
            .execute(
                "UPDATE index_generation SET state = 'abandoned'
                 WHERE generation_id = ?1",
                params![candidate.generation_id],
            )
            .unwrap_err();
        assert!(
            wrong_error
                .to_string()
                .contains("schema-4 writer marker required"),
            "unexpected wrong writer identity refusal: {wrong_error}"
        );
        let state: String = legacy
            .query_row(
                "SELECT state FROM index_generation WHERE generation_id = ?1",
                params![candidate.generation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "candidate", "refusal must precede mutation");
    }

    #[test]
    fn writer_lease_recovers_after_stale_heartbeat() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&path, &test_config()).unwrap();
        let cfg = test_config();
        let ws = crate::workspace::register_workspace(
            &conn,
            std::path::Path::new("/projects/example"),
            &cfg,
            &path,
            "1.0.0",
        )
        .unwrap();
        let ws_x: &str = &ws.workspace_id;
        let h1 = "holder-1";
        let h2 = "holder-2";

        acquire_writer_lease(
            &conn,
            ws_x,
            h1,
            Duration::from_secs(60),
            Duration::from_secs(0),
        )
        .unwrap();
        conn.execute(
            "UPDATE writer_lease SET heartbeat_at = '2000-01-01T00:00:00.000Z' WHERE workspace_id = ?1",
            params![ws_x],
        )
        .unwrap();
        acquire_writer_lease(
            &conn,
            ws_x,
            h2,
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
        .unwrap();
    }

    #[test]
    fn default_holder_id_is_unique_per_call() {
        let a = default_holder_id();
        let b = default_holder_id();
        assert_ne!(a, b);
    }
}
