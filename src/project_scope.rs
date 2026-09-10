//! Project-scoped provider discovery and input fingerprinting.
//!
//! A project-scoped semantic indexer (for example, `scip-typescript`) needs
//! project manifests, path aliases, and the full program graph; it cannot be
//! modelled as a stateless per-file parser. This module discovers scopes,
//! resolves each source file's deterministic owner, and computes the project
//! input-manifest hash and full execution fingerprint.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::hashing::content_hash_of_file;
use crate::provider_contract::canonical_json_bytes;

/// Directories never descended into while searching for project markers —
/// mirrors `policy.exclude.vendor_paths`/`build_paths` defaults
/// (`config.rs`), applied defensively here even without a loaded `Config` so
/// marker discovery never walks into `node_modules`.
const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".git",
    ".workspace_atlas",
];

/// One discovered project scope (`project_scope` table row shape, minus
/// generation/workspace linkage which the persistence layer attaches later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectScope {
    /// Canonical, workspace-root-relative directory this scope owns.
    pub canonical_root: PathBuf,
    pub scope_kind: ScopeKind,
    pub primary_manifest_path: PathBuf,
    pub primary_manifest_hash: String,
    /// Other config/lockfiles that participate in this scope's identity
    /// (e.g. a sibling `package.json` next to `tsconfig.json`), sorted by
    /// relative path.
    pub relevant_config_hashes: Vec<(PathBuf, String)>,
    pub language_family: Option<String>,
    pub owning_policy_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Project,
    Package,
    Workspace,
}

impl ScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Package => "package",
            Self::Workspace => "workspace",
        }
    }
}

/// Current owning-scope resolution policy. Bumping this string is itself an
/// invalidation event (`provider_invalidation_event.reason_code =
/// 'mapping_policy_changed'`-adjacent; scope-specific policy lives here).
pub const SCOPE_POLICY_VERSION: &str = "project-scope-1.0.0";

/// A marker filename and the scope kind/language family it implies, in
/// priority order (first match wins when a directory has multiple markers).
#[derive(Debug, Clone)]
pub struct MarkerRule {
    pub file_name: &'static str,
    pub scope_kind: ScopeKind,
    pub language_family: Option<&'static str>,
}

/// Default TypeScript/JavaScript project marker set.
pub fn default_typescript_markers() -> Vec<MarkerRule> {
    vec![
        MarkerRule {
            file_name: "tsconfig.json",
            scope_kind: ScopeKind::Project,
            language_family: Some("typescript"),
        },
        MarkerRule {
            file_name: "jsconfig.json",
            scope_kind: ScopeKind::Project,
            language_family: Some("javascript"),
        },
        MarkerRule {
            file_name: "package.json",
            scope_kind: ScopeKind::Package,
            language_family: Some("typescript"),
        },
    ]
}

pub fn default_rust_markers() -> Vec<MarkerRule> {
    vec![MarkerRule {
        file_name: "Cargo.toml",
        scope_kind: ScopeKind::Project,
        language_family: Some("rust"),
    }]
}

pub fn default_project_markers() -> Vec<MarkerRule> {
    let mut markers = default_typescript_markers();
    markers.extend(default_rust_markers());
    markers
}

/// Discover every project scope under `workspace_root` by walking the tree
/// (skipping `SKIP_DIR_NAMES`) and matching `markers` against each
/// directory's immediate file listing. Deterministic: directories and marker
/// rules are both visited in a fixed sorted order.
pub fn discover_project_scopes(
    workspace_root: &Path,
    markers: &[MarkerRule],
) -> Result<Vec<ProjectScope>> {
    let mut scopes = Vec::new();
    let mut dirs = vec![workspace_root.to_path_buf()];
    let mut visited_dirs: Vec<PathBuf> = Vec::new();
    while let Some(dir) = dirs.pop() {
        visited_dirs.push(dir.clone());
        let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => continue,
        };
        entries.sort_by_key(|e| e.file_name());

        let file_names: Vec<String> = entries
            .iter()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        for rule in markers {
            if file_names.iter().any(|n| n == rule.file_name) {
                let manifest_path = dir.join(rule.file_name);
                let manifest_hash = content_hash_of_file(&manifest_path)?;
                let mut relevant = Vec::new();
                for sibling in [
                    "package.json",
                    "package-lock.json",
                    "pnpm-lock.yaml",
                    "yarn.lock",
                    "Cargo.lock",
                ] {
                    if sibling == rule.file_name {
                        continue;
                    }
                    if file_names.iter().any(|n| n == sibling) {
                        let p = dir.join(sibling);
                        let h = content_hash_of_file(&p)?;
                        relevant.push((PathBuf::from(sibling), h));
                    }
                }
                relevant.sort_by(|a, b| a.0.cmp(&b.0));

                scopes.push(ProjectScope {
                    canonical_root: dir.clone(),
                    scope_kind: rule.scope_kind,
                    primary_manifest_path: manifest_path,
                    primary_manifest_hash: manifest_hash,
                    relevant_config_hashes: relevant,
                    language_family: rule.language_family.map(str::to_string),
                    owning_policy_version: SCOPE_POLICY_VERSION.to_string(),
                });
                // First matching rule per directory wins (priority order).
                break;
            }
        }

        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if SKIP_DIR_NAMES.iter().any(|s| *s == name_str) {
                    continue;
                }
                dirs.push(path);
            }
        }
    }

    scopes.sort_by(|a, b| a.canonical_root.cmp(&b.canonical_root));
    Ok(scopes)
}

/// The outcome of resolving one file's owning project scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwningScopeResolution {
    Owned(usize), // index into the scopes slice passed to `resolve_owning_scope`
    NoScope,
    /// Two or more scopes are equally specific ancestors of the file. The
    /// ambiguity must be retained as a diagnostic, never silently resolved.
    Ambiguous(Vec<usize>),
}

/// Resolve the smallest (most specific / deepest) enclosing scope for
/// `file_path`. Nested scopes are allowed (SCOPE-003); the deepest
/// `canonical_root` that is an ancestor of the file wins deterministically.
/// A true tie (two scopes with the identical `canonical_root`, e.g. a
/// `tsconfig.json` and `jsconfig.json` disagreement resolved elsewhere) is
/// reported as `Ambiguous` rather than picked arbitrarily.
pub fn resolve_owning_scope(file_path: &Path, scopes: &[ProjectScope]) -> OwningScopeResolution {
    let mut best_depth: Option<usize> = None;
    let mut best_indices: Vec<usize> = Vec::new();

    for (i, scope) in scopes.iter().enumerate() {
        if !file_path.starts_with(&scope.canonical_root) {
            continue;
        }
        let depth = scope.canonical_root.components().count();
        match best_depth {
            None => {
                best_depth = Some(depth);
                best_indices = vec![i];
            }
            Some(d) if depth > d => {
                best_depth = Some(depth);
                best_indices = vec![i];
            }
            Some(d) if depth == d => {
                best_indices.push(i);
            }
            _ => {}
        }
    }

    match best_indices.len() {
        0 => OwningScopeResolution::NoScope,
        1 => OwningScopeResolution::Owned(best_indices[0]),
        _ => OwningScopeResolution::Ambiguous(best_indices),
    }
}

// ---------------------------------------------------------------------------
// Fingerprints (SCOPE-004, SCOPE-005)
// ---------------------------------------------------------------------------

/// SCOPE-004: `input_manifest_hash` — the project scope's own identity,
/// independent of any provider. `project_scope.input_manifest_hash` column.
pub fn project_input_manifest_hash(scope: &ProjectScope) -> String {
    #[derive(serde::Serialize)]
    struct Row<'a> {
        canonical_root: &'a str,
        scope_kind: &'a str,
        primary_manifest_relative: String,
        primary_manifest_hash: &'a str,
        relevant: Vec<(String, &'a str)>,
        owning_policy_version: &'a str,
    }
    let row = Row {
        canonical_root: &scope.canonical_root.to_string_lossy(),
        scope_kind: scope.scope_kind.as_str(),
        primary_manifest_relative: scope
            .primary_manifest_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        primary_manifest_hash: &scope.primary_manifest_hash,
        relevant: scope
            .relevant_config_hashes
            .iter()
            .map(|(p, h)| (p.to_string_lossy().into_owned(), h.as_str()))
            .collect(),
        owning_policy_version: &scope.owning_policy_version,
    };
    crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&row))
}

/// SCOPE-005 full per-execution input fingerprint:
/// `hash(workspace_id + scope identity + sorted relevant file identities and
/// content hashes + relevant manifest/config/lockfile hashes + provider
/// descriptor fingerprint + provider configuration hash + provider protocol
/// version + normalized schema version + mapping policy version)`.
#[allow(clippy::too_many_arguments)]
pub fn execution_input_fingerprint(
    workspace_id: &str,
    scope: &ProjectScope,
    sorted_relevant_files: &[(String, String)], // (relative_path, content_hash), caller-sorted
    provider_descriptor_fingerprint: &str,
    provider_configuration_hash: &str,
    provider_protocol_version: &str,
    normalized_schema_version: &str,
    mapping_policy_version: &str,
) -> String {
    #[derive(serde::Serialize)]
    struct Row<'a> {
        workspace_id: &'a str,
        scope_identity: String,
        relevant_files: &'a [(String, String)],
        provider_descriptor_fingerprint: &'a str,
        provider_configuration_hash: &'a str,
        provider_protocol_version: &'a str,
        normalized_schema_version: &'a str,
        mapping_policy_version: &'a str,
    }
    let row = Row {
        workspace_id,
        scope_identity: project_input_manifest_hash(scope),
        relevant_files: sorted_relevant_files,
        provider_descriptor_fingerprint,
        provider_configuration_hash,
        provider_protocol_version,
        normalized_schema_version,
        mapping_policy_version,
    };
    crate::hashing::content_hash_of_bytes(&canonical_json_bytes(&row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_single_project_root() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(&dir.path().join("src/index.ts"), "export {}");
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].scope_kind, ScopeKind::Project);
        assert_eq!(scopes[0].language_family.as_deref(), Some("typescript"));
    }

    #[test]
    fn cargo_toml_discovers_a_rust_project_scope_and_hashes_cargo_lock() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        );
        write(&dir.path().join("Cargo.lock"), "version = 4\n");
        write(&dir.path().join("src/lib.rs"), "pub fn fixture() {}\n");
        let scopes = discover_project_scopes(dir.path(), &default_rust_markers()).unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].language_family.as_deref(), Some("rust"));
        assert_eq!(
            scopes[0].relevant_config_hashes[0].0,
            PathBuf::from("Cargo.lock")
        );
        let first = project_input_manifest_hash(&scopes[0]);
        write(&dir.path().join("Cargo.lock"), "version = 4\n# changed\n");
        let changed = discover_project_scopes(dir.path(), &default_rust_markers()).unwrap();
        assert_ne!(first, project_input_manifest_hash(&changed[0]));
    }

    #[test]
    fn tsconfig_wins_over_package_json_in_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(&dir.path().join("package.json"), "{}");
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        assert_eq!(
            scopes.len(),
            1,
            "one directory must produce exactly one scope, not one per marker"
        );
        assert_eq!(scopes[0].scope_kind, ScopeKind::Project);
        assert_eq!(scopes[0].relevant_config_hashes.len(), 1);
        assert_eq!(
            scopes[0].relevant_config_hashes[0].0,
            PathBuf::from("package.json")
        );
    }

    #[test]
    fn nested_project_scopes_are_both_discovered() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{\"root\":true}");
        write(
            &dir.path().join("packages/app/tsconfig.json"),
            "{\"nested\":true}",
        );
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        assert_eq!(scopes.len(), 2);
    }

    #[test]
    fn skips_node_modules_when_searching_for_markers() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(
            &dir.path().join("node_modules/some-dep/tsconfig.json"),
            "{}",
        );
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        assert_eq!(
            scopes.len(),
            1,
            "node_modules must never be treated as a project scope"
        );
    }

    #[test]
    fn owning_scope_picks_the_deepest_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        write(&dir.path().join("packages/app/tsconfig.json"), "{}");
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        let file = dir.path().join("packages/app/src/index.ts");
        let resolution = resolve_owning_scope(&file, &scopes);
        let OwningScopeResolution::Owned(idx) = resolution else {
            panic!("expected Owned, got {resolution:?}")
        };
        assert_eq!(scopes[idx].canonical_root, dir.path().join("packages/app"));
    }

    #[test]
    fn owning_scope_is_none_outside_any_project_root() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("packages/app/tsconfig.json"), "{}");
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        let file = dir.path().join("README.md");
        assert_eq!(
            resolve_owning_scope(&file, &scopes),
            OwningScopeResolution::NoScope
        );
    }

    #[test]
    fn owning_scope_reports_ambiguity_on_a_true_tie() {
        // Two scopes with the identical canonical_root can only occur if the
        // caller merges markers from more than one rule set; simulate it
        // directly to prove the ambiguity path is reachable and never
        // silently resolved.
        let dir = tempfile::tempdir().unwrap();
        let scope_a = ProjectScope {
            canonical_root: dir.path().to_path_buf(),
            scope_kind: ScopeKind::Project,
            primary_manifest_path: dir.path().join("tsconfig.json"),
            primary_manifest_hash: "a".repeat(64),
            relevant_config_hashes: vec![],
            language_family: Some("typescript".to_string()),
            owning_policy_version: SCOPE_POLICY_VERSION.to_string(),
        };
        let scope_b = ProjectScope {
            primary_manifest_hash: "b".repeat(64),
            ..scope_a.clone()
        };
        let file = dir.path().join("src/index.ts");
        let resolution = resolve_owning_scope(&file, &[scope_a, scope_b]);
        assert_eq!(resolution, OwningScopeResolution::Ambiguous(vec![0, 1]));
    }

    #[test]
    fn project_input_manifest_hash_is_deterministic_and_changes_on_manifest_edit() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{\"a\":1}");
        let scopes_a = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        let hash_a = project_input_manifest_hash(&scopes_a[0]);
        let hash_a2 = project_input_manifest_hash(&scopes_a[0]);
        assert_eq!(hash_a, hash_a2);

        write(&dir.path().join("tsconfig.json"), "{\"a\":2}");
        let scopes_b = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        let hash_b = project_input_manifest_hash(&scopes_b[0]);
        assert_ne!(
            hash_a, hash_b,
            "editing tsconfig.json must change the project input manifest hash"
        );
    }

    #[test]
    fn execution_input_fingerprint_changes_with_any_dependency() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("tsconfig.json"), "{}");
        let scopes = discover_project_scopes(dir.path(), &default_typescript_markers()).unwrap();
        let files = vec![("src/index.ts".to_string(), "h1".to_string())];

        let base = execution_input_fingerprint(
            "ws_1",
            &scopes[0],
            &files,
            "descfp",
            "cfghash",
            "1.0.0",
            "normalized-1.0.0",
            "mapping-1.0.0",
        );

        let changed_files = vec![("src/index.ts".to_string(), "h2".to_string())];
        let a = execution_input_fingerprint(
            "ws_1",
            &scopes[0],
            &changed_files,
            "descfp",
            "cfghash",
            "1.0.0",
            "normalized-1.0.0",
            "mapping-1.0.0",
        );
        assert_ne!(
            base, a,
            "changed file content hash must change the fingerprint"
        );

        let b = execution_input_fingerprint(
            "ws_1",
            &scopes[0],
            &files,
            "descfp-v2",
            "cfghash",
            "1.0.0",
            "normalized-1.0.0",
            "mapping-1.0.0",
        );
        assert_ne!(
            base, b,
            "changed provider descriptor fingerprint must change the fingerprint"
        );

        let c = execution_input_fingerprint(
            "ws_1",
            &scopes[0],
            &files,
            "descfp",
            "cfghash",
            "1.0.0",
            "normalized-1.0.0",
            "mapping-2.0.0",
        );
        assert_ne!(
            base, c,
            "changed mapping policy version must change the fingerprint"
        );

        let same = execution_input_fingerprint(
            "ws_1",
            &scopes[0],
            &files,
            "descfp",
            "cfghash",
            "1.0.0",
            "normalized-1.0.0",
            "mapping-1.0.0",
        );
        assert_eq!(
            base, same,
            "identical inputs must reuse the identical fingerprint"
        );
    }
}
