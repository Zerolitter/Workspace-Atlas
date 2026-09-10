//! Content hashing (INV-002 — line numbers are not identity; we hash bytes).

use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{AtlasError, Result};

/// Compute the canonical content hash for a file. Uses SHA-256 over the raw
/// bytes (no normalisation — INV-002 treats line endings as content).
pub fn content_hash_of_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the content hash of an in-memory byte slice. Useful for tests +
/// provider output that has already been loaded into a buffer.
pub fn content_hash_of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Compute a source-tree hash from paths and content hashes sorted by canonical
/// path. Used by status and reconciliation determinism checks.
pub fn source_tree_hash<I>(entries: I) -> String
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut sorted: Vec<(String, String)> = entries.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (path, hash) in sorted {
        hasher.update(path.as_bytes());
        hasher.update([0x1f]);
        hasher.update(hash.as_bytes());
        hasher.update([0x1e]); // record separator
    }
    hex::encode(hasher.finalize())
}

/// Format a UNIX-epoch nanosecond timestamp for storage in `generation_file.observed_mtime_ns`.
pub fn mtime_ns_now() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() as i64) * 1_000_000_000 + now.subsec_nanos() as i64
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CataloguePlatform {
    Windows,
    MacOs,
    Linux,
}

fn required_env(name: &str, value: Option<&OsStr>) -> Result<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            AtlasError::Other(format!(
                "{name} is required to determine the ADR-015 catalogue directory"
            ))
        })
}

fn catalogue_directory_for(
    platform: CataloguePlatform,
    local_app_data: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
) -> Result<PathBuf> {
    match platform {
        CataloguePlatform::Windows => Ok(required_env("LOCALAPPDATA", local_app_data)?
            .join("WorkspaceAtlas")
            .join("catalogues")),
        CataloguePlatform::MacOs => Ok(required_env("HOME", home)?
            .join("Library")
            .join("Application Support")
            .join("WorkspaceAtlas")
            .join("catalogues")),
        CataloguePlatform::Linux => {
            let base = match xdg_data_home.filter(|value| !value.is_empty()) {
                Some(value) => required_env("XDG_DATA_HOME", Some(value))?,
                None => required_env("HOME", home)?.join(".local").join("share"),
            };
            Ok(base.join("workspace-atlas").join("catalogues"))
        }
    }
}

fn ensure_absolute_catalogue_directory(directory: PathBuf) -> Result<PathBuf> {
    if !directory.is_absolute() {
        return Err(AtlasError::Other(
            "the ADR-015 catalogue directory from platform environment variables must be absolute"
                .to_string(),
        ));
    }
    Ok(directory)
}

/// Detect the exact platform catalogue directory from ADR-015. Does not
/// require the directory to exist and never falls back to the current working
/// directory.
pub fn default_catalogue_directory() -> Result<PathBuf> {
    let platform = if cfg!(target_os = "windows") {
        CataloguePlatform::Windows
    } else if cfg!(target_os = "macos") {
        CataloguePlatform::MacOs
    } else {
        CataloguePlatform::Linux
    };
    ensure_absolute_catalogue_directory(catalogue_directory_for(
        platform,
        std::env::var_os("LOCALAPPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
    )?)
}

/// Detect the default platform catalogue path for a `workspace_id`, per
/// ADR-015. Does not require the directory to exist.
pub fn default_catalogue_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(default_catalogue_directory()?.join(format!("{workspace_id}.sqlite")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_of_bytes_is_stable() {
        assert_eq!(
            content_hash_of_bytes(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn content_hash_of_file_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(
            content_hash_of_file(&path).unwrap(),
            content_hash_of_bytes(b"hello world")
        );
    }

    #[test]
    fn source_tree_hash_is_order_independent() {
        let a = source_tree_hash(vec![
            ("src/lib.rs".into(), "h1".into()),
            ("src/main.rs".into(), "h2".into()),
        ]);
        let b = source_tree_hash(vec![
            ("src/main.rs".into(), "h2".into()),
            ("src/lib.rs".into(), "h1".into()),
        ]);
        assert_eq!(a, b);
    }

    #[test]
    fn source_tree_hash_detects_change() {
        let a = source_tree_hash(vec![("src/lib.rs".into(), "h1".into())]);
        let b = source_tree_hash(vec![("src/lib.rs".into(), "h2".into())]);
        assert_ne!(a, b);
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        ensure_dir(&nested).unwrap();
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn platform_catalogue_layouts_follow_adr_015() {
        let windows = catalogue_directory_for(
            CataloguePlatform::Windows,
            Some(OsStr::new("C:\\Users\\<user>\\AppData\\Local")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            windows,
            PathBuf::from("C:\\Users\\<user>\\AppData\\Local")
                .join("WorkspaceAtlas")
                .join("catalogues")
        );

        let macos = catalogue_directory_for(
            CataloguePlatform::MacOs,
            None,
            Some(OsStr::new("/Users/<user>")),
            None,
        )
        .unwrap();
        assert_eq!(
            macos,
            PathBuf::from("/Users/<user>")
                .join("Library")
                .join("Application Support")
                .join("WorkspaceAtlas")
                .join("catalogues")
        );

        let linux_xdg = catalogue_directory_for(
            CataloguePlatform::Linux,
            None,
            Some(OsStr::new("/home/<user>")),
            Some(OsStr::new("/var/lib/atlas")),
        )
        .unwrap();
        assert_eq!(
            linux_xdg,
            PathBuf::from("/var/lib/atlas")
                .join("workspace-atlas")
                .join("catalogues")
        );

        let linux_home = catalogue_directory_for(
            CataloguePlatform::Linux,
            None,
            Some(OsStr::new("/home/<user>")),
            Some(OsStr::new("")),
        )
        .unwrap();
        assert_eq!(
            linux_home,
            PathBuf::from("/home/<user>")
                .join(".local")
                .join("share")
                .join("workspace-atlas")
                .join("catalogues")
        );
    }

    #[test]
    fn platform_catalogue_layouts_reject_missing_or_relative_environment() {
        let missing_windows =
            catalogue_directory_for(CataloguePlatform::Windows, None, None, None).unwrap_err();
        assert!(missing_windows
            .to_string()
            .contains("LOCALAPPDATA is required"));

        let missing_macos =
            catalogue_directory_for(CataloguePlatform::MacOs, None, None, None).unwrap_err();
        assert!(missing_macos.to_string().contains("HOME is required"));

        let missing_linux =
            catalogue_directory_for(CataloguePlatform::Linux, None, None, None).unwrap_err();
        assert!(missing_linux.to_string().contains("HOME is required"));

        let relative_linux = catalogue_directory_for(
            CataloguePlatform::Linux,
            None,
            None,
            Some(OsStr::new("relative")),
        )
        .unwrap();
        let relative_linux = ensure_absolute_catalogue_directory(relative_linux).unwrap_err();
        assert!(relative_linux.to_string().contains("must be absolute"));
    }
}
