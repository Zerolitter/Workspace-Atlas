//! Canonical path handling. INV-014 (root confinement) and platform-safe
//! normalization.

use std::path::{Component, Path, PathBuf};

use crate::error::{AtlasError, Result};

/// Canonicalize a path **without** requiring it to exist. Resolves `.`/`..`
/// components and returns the lexical canonical form.
///
/// Note: this is *not* the same as `std::fs::canonicalize` (which requires the
/// path to exist and resolves symlinks). We deliberately do not resolve
/// symlinks here — symlink policy is enforced at the discovery layer.
pub fn lexically_canonical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(Component::CurDir.as_os_str());
    }
    out
}

/// Borrow a path as UTF-8 for catalogue identity and persistence fields.
/// ADR-015 defines those fields over UTF-8 bytes, so lossy conversion would
/// make distinct filesystem paths collide.
pub(crate) fn path_as_utf8<'a>(path: &'a Path, context: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| {
        AtlasError::Other(format!(
            "{context} is not valid UTF-8; ADR-015 requires UTF-8 paths and Atlas refuses lossy path conversion"
        ))
    })
}

/// Reject any path that is not inside `canonical_root`. Both inputs MUST be
/// lexically canonical (no trailing `.`/`..` segments). Returns the canonical
/// relative path on success.
pub fn confine_to_root(canonical_root: &Path, candidate: &Path) -> Result<PathBuf> {
    let root_canon = lexically_canonical(canonical_root);
    let cand_canon = lexically_canonical(candidate);

    // Reject absolute paths whose drive / root prefix differs.
    if cand_canon.has_root() != root_canon.has_root() {
        return Err(AtlasError::PathEscape {
            path: cand_canon.display().to_string(),
        });
    }

    let mut root_components = root_canon.components().peekable();
    let mut cand_components = cand_canon.components().peekable();

    loop {
        match (root_components.peek(), cand_components.peek()) {
            (Some(r), Some(c)) => {
                if r != c {
                    return Err(AtlasError::PathEscape {
                        path: cand_canon.display().to_string(),
                    });
                }
                root_components.next();
                cand_components.next();
            }
            (None, _) => {
                // Root exhausted — rest is the relative path inside.
                let mut rel = PathBuf::new();
                for c in cand_components {
                    rel.push(c.as_os_str());
                }
                return Ok(rel);
            }
            (Some(_), None) => {
                // Candidate is shorter than root.
                return Err(AtlasError::PathEscape {
                    path: cand_canon.display().to_string(),
                });
            }
        }
    }
}

/// Compute a canonical relative path for storage in the catalogue. The path
/// stored in `generation_file.canonical_path` is forward-slash and relative to
/// the workspace root, regardless of host OS.
pub fn canonical_relative_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.replace('\\', "/")
}

/// Derive the application-owned root locator path from the same canonical
/// root fingerprint used by catalogue routing.
pub fn catalogue_locator_path(
    application_catalogue_root: &Path,
    canonical_root: &Path,
) -> Result<PathBuf> {
    Ok(application_catalogue_root.join("locators").join(format!(
        "{}.json",
        crate::workspace::blake3_root_fingerprint(canonical_root)?
    )))
}

/// Resolve an existing destructive-operation target without following a
/// symlink, Windows reparse point, or non-regular replacement.
pub fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        AtlasError::Other(format!(
            "{label} {} cannot be inspected: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(AtlasError::Other(format!(
            "{label} {} is not a regular file",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AtlasError::Other(format!(
                "{label} {} is a Windows reparse point",
                path.display()
            )));
        }
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        AtlasError::Other(format!(
            "{label} {} cannot be resolved: {error}",
            path.display()
        ))
    })?;
    let resolved_metadata = std::fs::metadata(&canonical)?;
    if !resolved_metadata.is_file() {
        return Err(AtlasError::Other(format!(
            "{label} {} resolved to a non-regular file",
            path.display()
        )));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_canonical_strips_dot_segments() {
        let p = std::path::Path::new("./a/./b/../c");
        assert_eq!(lexically_canonical(p), PathBuf::from("a/c"));
    }

    #[test]
    fn lexical_canonical_preserves_absolute() {
        let p = std::path::Path::new("/a/b/../c");
        assert_eq!(lexically_canonical(p), PathBuf::from("/a/c"));
    }

    #[test]
    fn confine_accepts_descendant() {
        let root = std::path::Path::new("/projects/example");
        let cand = std::path::Path::new("/projects/example/src/lib.rs");
        let rel = confine_to_root(root, cand).unwrap();
        assert_eq!(rel, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn confine_rejects_escape() {
        let root = std::path::Path::new("/projects/example");
        let cand = std::path::Path::new("/projects/other/file.rs");
        let err = confine_to_root(root, cand).unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn confine_rejects_traversal() {
        let root = std::path::Path::new("/projects/example");
        let cand = std::path::Path::new("/projects/example/../other/file.rs");
        let err = confine_to_root(root, cand).unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn canonical_relative_path_uses_forward_slash() {
        let p = std::path::Path::new("src\\module\\file.rs");
        assert_eq!(canonical_relative_path(p), "src/module/file.rs");
    }
}
