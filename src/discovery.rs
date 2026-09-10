//! Discovery, classification, hashing, and atomic reconciliation.
//!
//! Public surface:
//! - `reconcile(ws, conn, policy)`: walk the workspace, classify each candidate,
//!   hash its content, store `file_identity` + `file_revision` + `generation_file`
//!   rows inside a candidate generation, compute a `source_tree_hash`, and
//!   atomically activate the candidate if all writes succeeded.
//! - `Classification`: per-file outcome (Present / Excluded / Unsupported / Failed).
//!
//! The walker respects INV-014 (root confinement) and the policy's
//! include/exclude rules (ADR-016). The default policy treats anything
//! matching `.env`, `.env.<x>`, `*.pem`, `*.key`, `id_rsa*`, `secrets*.y?ml`,
//! `credentials*.json`, `cookies*.sqlite|json`, `netrc`, `service-account.json`,
//! `.npmrc`, `.pypirc`, plus vendor / build / generated / binary / archive
//! patterns from the config. Secret paths are recorded as `excluded` with
//! `exclusion_code = "secret_path_excluded"` (no bytes read).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{AtlasError, Result};
use crate::generation::{self, GenerationState, TriggerKind};
use crate::hashing::{content_hash_of_file, source_tree_hash};
use crate::migrations;
use crate::paths::{confine_to_root, lexically_canonical};
use crate::workspace::WorkspaceRecord;

/// Per-file outcome reported by the walker.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Classification {
    /// File was discovered, hashed, and added to the candidate generation.
    Present,
    /// File matched a policy exclusion (vendor/build/generated/binary/archive).
    Excluded { code: String, detail: String },
    /// File matched a secret-bearing pattern. Never read.
    SecretExcluded { code: String, detail: String },
    /// File's artifact class is not supported by any registered provider.
    Unsupported { code: String, detail: String },
    /// Discovery failed for this file (e.g. permission error).
    Failed { code: String, detail: String },
}

/// A single file's classification + (if Present) content hash.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifiedFile {
    pub canonical_relative_path: String,
    pub classification: Classification,
    pub content_hash: Option<String>,
    pub byte_size: Option<i64>,
    pub observed_mtime_ns: Option<i64>,
    pub artifact_class: Option<String>,
}

/// Default set of secret-bearing regex patterns (ADR-016). Each pattern is
/// matched against the canonical relative path (POSIX form).
pub const DEFAULT_SECRET_PATTERNS: &[&str] = &[
    r"\.env(\.[^/]+)?$",
    r"(^|/)id_rsa$",
    r"(^|/)id_ed25519$",
    r"\.pem$",
    r"\.key$",
    r"(^|/)secrets?\.ya?ml$",
    r"(^|/)credentials?\.json$",
    r"(^|/)cookies?\.(sqlite|json)$",
    r"(^|/)netrc$",
    r"(^|/)service-account\.json$",
    r"(^|/)\.npmrc$",
    r"(^|/)\.pypirc$",
];

/// Default vendor / build / generated / binary / archive path globs (ADR-016).
pub const DEFAULT_VENDOR_PATTERNS: &[&str] = &[
    "(^|/)node_modules/",
    "(^|/)vendor/",
    "(^|/)\\.venv/",
    "(^|/)venv/",
    "(^|/)__pycache__/",
    "(^|/)\\.uv-cache/",
];

pub const DEFAULT_BUILD_PATTERNS: &[&str] = &[
    "(^|/)dist/",
    "(^|/)build/",
    "(^|/)out/",
    "(^|/)target/",
    "(^|/)\\.next/",
    "(^|/)\\.turbo/",
    "(^|/)\\.cache/",
];

pub const DEFAULT_GENERATED_PATTERNS: &[&str] = &[
    "(^|/).*\\.min\\.js$",
    "(^|/).*\\.min\\.css$",
    "(^|/).*\\.gen\\.ts$",
    "(^|/)generated/",
    "(^|/).*\\.pb\\.go$",
];

pub const DEFAULT_BINARY_PATTERNS: &[&str] = &[
    "(^|/).*\\.png$",
    "(^|/).*\\.jpg$",
    "(^|/).*\\.jpeg$",
    "(^|/).*\\.gif$",
    "(^|/).*\\.webp$",
    "(^|/).*\\.pdf$",
    "(^|/).*\\.zip$",
    "(^|/).*\\.7z$",
    "(^|/).*\\.tar(\\.[a-z0-9]+)?$",
    "(^|/).*\\.exe$",
    "(^|/).*\\.dll$",
    "(^|/).*\\.so$",
    "(^|/).*\\.dylib$",
    "(^|/).*\\.bin$",
];

pub const DEFAULT_ARCHIVE_PATTERNS: &[(&str, &str)] = &[
    ("(.*)\\.jar$", "archive_jar"),
    ("(.*)\\.war$", "archive_war"),
    ("(.*)\\.whl$", "archive_wheel"),
    ("(.*)\\.gem$", "archive_gem"),
    ("(.*)\\.deb$", "archive_deb"),
    ("(.*)\\.rpm$", "archive_rpm"),
];

/// Directories holding superseded/archived project source (distinct from
/// archive file formats above). Archived source is excluded from current
/// indexing by default.
pub const DEFAULT_ARCHIVE_DIR_PATTERNS: &[&str] = &["(^|/)archive/"];

/// Atlas's own catalogue directory, when using in-workspace portable mode
/// (ADR-015). Always excluded (INV-023 "self-pollution avoidance") — this is
/// not a policy-configurable pattern.
pub const ATLAS_SELF_EXCLUSION_PATTERNS: &[&str] = &["(^|/)\\.atlas/", "(^|/)\\.workspace_atlas/"];

/// Artifact class inferred from the file extension and stored in
/// `file_revision.artifact_class`.
fn artifact_class_for(rel: &str) -> &'static str {
    let lower = rel.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "md" | "markdown" | "txt" | "rst" | "adoc" => "documentation",
        "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" => "configuration",
        "lock" => "build_manifest",
        "py" | "rs" | "ts" | "tsx" | "js" | "jsx" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
        | "cs" | "rb" | "php" | "sh" | "sql" => "source",
        "test.py" | "_test.py" | "tests.py" | "spec.ts" | "spec.js" => "source", // not realistic; real test detection via dir/prefix
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" | "zip" | "7z" | "tar" | "gz" | "bz2"
        | "xz" | "exe" | "dll" | "so" | "dylib" | "class" | "jar" | "war" => "binary_metadata",
        _ => "other",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RevisionClassification {
    artifact_class: &'static str,
    language: Option<&'static str>,
    is_generated: bool,
    is_test: bool,
}

/// Material classification produced by `file-classification-v2.0.0`.
///
/// Only Rust files beneath an exact, case-sensitive `tests` directory
/// segment are tests. The filename is never considered a directory segment.
fn revision_classification_for(rel: &str) -> RevisionClassification {
    let filename = rel.rsplit('/').next().unwrap_or(rel);
    let is_rust = filename
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("rs"));
    let has_tests_directory = rel
        .rsplit_once('/')
        .is_some_and(|(directories, _)| directories.split('/').any(|segment| segment == "tests"));
    let is_test = is_rust && has_tests_directory;

    RevisionClassification {
        artifact_class: if is_test {
            "test"
        } else {
            artifact_class_for(rel)
        },
        language: None,
        is_generated: false,
        is_test,
    }
}

/// Match `path` against a precompiled `regex::RegexSet`, returning the first
/// matching pattern's source text if any.
fn matches_set(path: &str, set: &(regex::RegexSet, &'static [&'static str])) -> Option<String> {
    let (regex_set, sources) = set;
    regex_set
        .matches(path)
        .iter()
        .next()
        .map(|i| sources[i].to_string())
}

static SECRET_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_SECRET_PATTERNS)
                .expect("static secret patterns are valid regex"),
            DEFAULT_SECRET_PATTERNS,
        )
    });

static VENDOR_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_VENDOR_PATTERNS)
                .expect("static vendor patterns are valid regex"),
            DEFAULT_VENDOR_PATTERNS,
        )
    });

static BUILD_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_BUILD_PATTERNS)
                .expect("static build patterns are valid regex"),
            DEFAULT_BUILD_PATTERNS,
        )
    });

static GENERATED_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_GENERATED_PATTERNS)
                .expect("static generated patterns are valid regex"),
            DEFAULT_GENERATED_PATTERNS,
        )
    });

static BINARY_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_BINARY_PATTERNS)
                .expect("static binary patterns are valid regex"),
            DEFAULT_BINARY_PATTERNS,
        )
    });

static ARCHIVE_PATTERNS_SET: std::sync::LazyLock<(
    regex::RegexSet,
    &'static [(&'static str, &'static str)],
)> = std::sync::LazyLock::new(|| {
    let pats: Vec<&str> = DEFAULT_ARCHIVE_PATTERNS.iter().map(|(p, _)| *p).collect();
    (
        regex::RegexSet::new(&pats).expect("static archive patterns are valid regex"),
        DEFAULT_ARCHIVE_PATTERNS,
    )
});

static ARCHIVE_DIR_PATTERNS_SET: std::sync::LazyLock<(regex::RegexSet, &'static [&'static str])> =
    std::sync::LazyLock::new(|| {
        (
            regex::RegexSet::new(DEFAULT_ARCHIVE_DIR_PATTERNS)
                .expect("static archive-directory patterns are valid regex"),
            DEFAULT_ARCHIVE_DIR_PATTERNS,
        )
    });

static ATLAS_SELF_EXCLUSION_PATTERNS_SET: std::sync::LazyLock<(
    regex::RegexSet,
    &'static [&'static str],
)> = std::sync::LazyLock::new(|| {
    (
        regex::RegexSet::new(ATLAS_SELF_EXCLUSION_PATTERNS)
            .expect("static Atlas self-exclusion patterns are valid regex"),
        ATLAS_SELF_EXCLUSION_PATTERNS,
    )
});

/// Determine the artifact class + the set of classifications for `path` under
/// the given policy.
pub fn classify(rel: &str, policy: &Config) -> Classification {
    let rel = rel.replace('\\', "/");
    let patterns = &policy.policy.exclude;

    if let Some(pattern) = matches_set(&rel, &ATLAS_SELF_EXCLUSION_PATTERNS_SET) {
        return Classification::Excluded {
            code: "atlas_self_exclusion".into(),
            detail: format!("matched self-exclusion pattern {pattern}"),
        };
    }

    if let Some(pattern) = patterns.pattern_match(&rel) {
        return Classification::SecretExcluded {
            code: "secret_path_excluded".into(),
            detail: format!("matched pattern {pattern}"),
        };
    }
    if let Some(pattern) = matches_set(&rel, &SECRET_PATTERNS_SET) {
        return Classification::SecretExcluded {
            code: "secret_path_excluded".into(),
            detail: format!("matched default secret pattern {pattern}"),
        };
    }

    if let Some(pattern) = patterns
        .vendor_match(&rel)
        .map(str::to_owned)
        .or_else(|| matches_set(&rel, &VENDOR_PATTERNS_SET))
    {
        return Classification::Excluded {
            code: "vendor_path".into(),
            detail: format!("vendor match: {pattern}"),
        };
    }
    if let Some(pattern) = patterns
        .build_match(&rel)
        .map(str::to_owned)
        .or_else(|| matches_set(&rel, &BUILD_PATTERNS_SET))
    {
        return Classification::Excluded {
            code: "build_path".into(),
            detail: format!("build match: {pattern}"),
        };
    }
    if let Some(pattern) = patterns
        .generated_match(&rel)
        .map(str::to_owned)
        .or_else(|| matches_set(&rel, &GENERATED_PATTERNS_SET))
    {
        return Classification::Excluded {
            code: "generated_path".into(),
            detail: format!("generated match: {pattern}"),
        };
    }
    if let Some(pattern) = patterns
        .binary_match(&rel)
        .map(str::to_owned)
        .or_else(|| matches_set(&rel, &BINARY_PATTERNS_SET))
    {
        return Classification::Excluded {
            code: "binary_path".into(),
            detail: format!("binary match: {pattern}"),
        };
    }
    {
        let (regex_set, sources) = &*ARCHIVE_PATTERNS_SET;
        if let Some(index) = regex_set.matches(&rel).iter().next() {
            let (pattern, code) = sources[index];
            return Classification::Excluded {
                code: code.into(),
                detail: format!("archive match: {pattern}"),
            };
        }
    }
    if let Some(pattern) = patterns
        .archive_match(&rel)
        .map(str::to_owned)
        .or_else(|| matches_set(&rel, &ARCHIVE_DIR_PATTERNS_SET))
    {
        return Classification::Excluded {
            code: "archive_directory".into(),
            detail: format!("archive directory match: {pattern}"),
        };
    }

    Classification::Present
}

/// Walk the workspace without configurable pruning.
pub fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    walk_internal(root, None)
}

fn walk_with_policy(root: &Path, policy: &Config) -> Result<Vec<PathBuf>> {
    walk_internal(root, Some(policy))
}

fn walk_internal(root: &Path, policy: Option<&Config>) -> Result<Vec<PathBuf>> {
    let root_canon = lexically_canonical(root);
    let mut out: Vec<PathBuf> = Vec::new();
    walk_recursive(&root_canon, &root_canon, policy, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_recursive(
    root: &Path,
    current: &Path,
    policy: Option<&Config>,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AtlasError::Other(format!(
                "read_dir {}: {error}",
                current.display()
            )))
        }
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let directory_name = entry.file_name();
            let directory_name = directory_name.to_string_lossy();
            if matches!(directory_name.as_ref(), ".git" | ".hg" | ".svn" | ".jj") {
                continue;
            }
            if let Some(policy) = policy {
                let relative = path
                    .strip_prefix(root)
                    .expect("walked directory remains under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                if !matches!(
                    classify(&format!("{relative}/"), policy),
                    Classification::Present
                ) {
                    continue;
                }
            }
            walk_recursive(root, &path, policy, out)?;
        } else if file_type.is_file() && confine_to_root(root, &path).is_ok() {
            out.push(path);
        }
    }
    Ok(())
}

/// Result of a reconcile cycle.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileReport {
    pub workspace_id: String,
    pub previous_active_generation_id: Option<String>,
    pub candidate_generation_id: String,
    pub activation: String,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub source_tree_hash: String,
    pub eligible_file_count: i64,
    pub indexed_file_count: i64,
    pub excluded_file_count: i64,
    pub unsupported_file_count: i64,
    pub failed_file_count: i64,
    pub secret_excluded_file_count: i64,
    pub parsed_file_count: i64,
    pub renamed_file_count: i64,
    pub deleted_file_count: i64,
    /// V1.1 semantic reconcile metrics (ADR-038: never collapse these into
    /// one ambiguous "processed" count).
    pub semantic_scopes_considered: i64,
    pub semantic_executions_run: i64,
    pub semantic_executions_reused: i64,
    pub semantic_documents_processed: i64,
    pub semantic_symbols_persisted: i64,
    pub semantic_relationships_persisted: i64,
    pub semantic_resolutions_persisted: i64,
    pub semantic_conflicts_persisted: i64,
    pub semantic_degraded: bool,
}

/// OBS-003 / ADR-038: report core changed files, provider executions,
/// emitted documents, and resolution outcomes as separate, never-conflated
/// categories. A single "processed" count cannot distinguish "5 files
/// changed" from "1 provider execution emitted 5 documents" from "12
/// relationships resolved" -- collapsing them was the exact defect ADR-038
/// replaced.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionMetricsReport {
    /// Files this reconcile pass newly indexed, renamed, or deleted --
    /// independent of whether any provider ran.
    pub core_changed_files: i64,
    pub provider_executions_run: i64,
    pub provider_executions_reused: i64,
    /// Documents a *fresh* (non-reused) provider execution emitted this
    /// pass. Zero whenever every execution was reused, even if the
    /// underlying project scope contains many documents.
    pub emitted_documents: i64,
    pub resolutions_persisted: i64,
    pub conflicts_persisted: i64,
}

pub fn metrics_report(report: &ReconcileReport) -> ExecutionMetricsReport {
    ExecutionMetricsReport {
        core_changed_files: report.indexed_file_count
            + report.renamed_file_count
            + report.deleted_file_count,
        provider_executions_run: report.semantic_executions_run,
        provider_executions_reused: report.semantic_executions_reused,
        emitted_documents: report.semantic_documents_processed,
        resolutions_persisted: report.semantic_resolutions_persisted,
        conflicts_persisted: report.semantic_conflicts_persisted,
    }
}
/// Persist a provider's `NormalizedBatch` as an `extractor_run` plus its
/// symbol, relationship, and diagnostic facts. Core validates and persists
/// the batch; providers never receive direct database authority (INV-003).
fn persist_provider_batch(
    tx: &rusqlite::Transaction<'_>,
    revision_id: &str,
    byte_size: i64,
    batch: &crate::providers::NormalizedBatch,
) -> Result<()> {
    let now = migrations::iso8601_now();
    let config_hash = "default"; // Built-in providers have no per-provider config.
    let extractor_run_id = {
        use blake3::Hasher;
        let mut h = Hasher::new();
        h.update(revision_id.as_bytes());
        h.update(&[0x1f]);
        h.update(batch.provider_name.as_bytes());
        h.update(&[0x1f]);
        h.update(batch.provider_version.as_bytes());
        format!("run_{}", &h.finalize().to_hex().as_str()[..32])
    };

    // Reuse if this exact (revision, provider, version, config) run already
    // exists (defensive; `is_new_revision` already prevents most repeats).
    let exists: Option<String> = tx
        .query_row(
            "SELECT extractor_run_id FROM extractor_run WHERE extractor_run_id = ?1",
            params![extractor_run_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Ok(());
    }

    let status = if batch.symbols.is_empty()
        && batch.relationships.is_empty()
        && !batch.diagnostics.is_empty()
    {
        "partial"
    } else {
        "complete"
    };

    tx.execute(
        "INSERT INTO extractor_run (
            extractor_run_id, revision_id, provider_name, provider_version, provider_tier,
            configuration_hash, normalized_schema_version, deterministic, status,
            started_at, finished_at, bytes_total, bytes_processed, capabilities_json, limitations_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?11, '[]', '[]')",
        params![
            extractor_run_id,
            revision_id,
            batch.provider_name,
            batch.provider_version,
            batch.provider_tier.as_str(),
            config_hash,
            "1.0.0",
            if batch.deterministic { 1 } else { 0 },
            status,
            now,
            byte_size,
        ],
    )?;

    for sym in &batch.symbols {
        let symbol_fact_id = {
            use blake3::Hasher;
            let mut h = Hasher::new();
            h.update(extractor_run_id.as_bytes());
            h.update(&[0x1f]);
            h.update(sym.canonical_symbol_key.as_bytes());
            h.update(&[0x1f]);
            h.update(sym.start_byte.to_le_bytes().as_slice());
            h.update(sym.end_byte.to_le_bytes().as_slice());
            format!("sym_{}", &h.finalize().to_hex().as_str()[..32])
        };
        tx.execute(
            "INSERT OR IGNORE INTO symbol_fact (
                symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
                provider_symbol_id, symbol_kind, display_name, qualified_name, signature,
                visibility, start_byte, end_byte, start_line, start_column, end_line, end_column,
                documentation, attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13, ?14, NULL, '{}', ?15, ?16, ?17)",
            params![
                symbol_fact_id,
                extractor_run_id,
                revision_id,
                sym.canonical_symbol_key,
                sym.symbol_kind,
                sym.display_name,
                sym.qualified_name,
                sym.signature,
                sym.start_byte,
                sym.end_byte,
                sym.start_line,
                sym.start_column,
                sym.end_line,
                sym.end_column,
                sym.evidence_method.as_str(),
                sym.confidence,
                sym.evidence_reason,
            ],
        )?;
    }

    for rel in &batch.relationships {
        let relationship_fact_id = {
            use blake3::Hasher;
            let mut h = Hasher::new();
            h.update(extractor_run_id.as_bytes());
            h.update(&[0x1f]);
            h.update(rel.source_ref_value.as_bytes());
            h.update(&[0x1f]);
            h.update(rel.target_ref_value.as_bytes());
            h.update(&[0x1f]);
            h.update(rel.relationship_type.as_bytes());
            format!("rel_{}", &h.finalize().to_hex().as_str()[..32])
        };
        tx.execute(
            "INSERT OR IGNORE INTO relationship_fact (
                relationship_fact_id, extractor_run_id, revision_id, relationship_type,
                source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
                start_byte, end_byte, attributes_json, evidence_method, confidence, evidence_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, '{}', ?9, ?10, ?11)",
            params![
                relationship_fact_id,
                extractor_run_id,
                revision_id,
                rel.relationship_type,
                rel.source_ref_kind,
                rel.source_ref_value,
                rel.target_ref_kind,
                rel.target_ref_value,
                rel.evidence_method.as_str(),
                rel.confidence,
                rel.evidence_reason,
            ],
        )?;
    }

    for diag in &batch.diagnostics {
        let diagnostic_id = {
            use blake3::Hasher;
            let mut h = Hasher::new();
            h.update(extractor_run_id.as_bytes());
            h.update(&[0x1f]);
            h.update(diag.as_bytes());
            format!("diag_{}", &h.finalize().to_hex().as_str()[..32])
        };
        tx.execute(
            "INSERT OR IGNORE INTO diagnostic (
                diagnostic_id, extractor_run_id, generation_id, severity, code, message,
                canonical_path, start_byte, end_byte, details_json, created_at
             ) VALUES (?1, ?2, NULL, 'info', 'provider_note', ?3, NULL, NULL, NULL, '{}', ?4)",
            params![diagnostic_id, extractor_run_id, diag, now],
        )?;
    }

    Ok(())
}

/// Run one reconcile cycle: walk, classify, hash, store inside a candidate
/// generation, activate atomically. Fails the candidate (leaving the previous
/// committed generation active) if anything goes wrong mid-cycle.
pub fn reconcile(
    ws: &WorkspaceRecord,
    conn: &Connection,
    config: &Config,
) -> Result<ReconcileReport> {
    let root = PathBuf::from(&ws.canonical_root);
    let files = walk_with_policy(&root, config)?;

    // The current committed generation is the candidate's reuse parent.
    let previous_active = ws.active_generation_id.clone();
    let candidate = generation::begin_candidate(
        conn,
        &ws.workspace_id,
        TriggerKind::Reconcile,
        "p_reconcile_v1",
        migrations::CURRENT_SCHEMA_VERSION,
    )?;

    // Phase A (read-only, no DB writes yet): classify + hash every walked
    // file exactly once. Reused below both for lifecycle (rename/deletion)
    // detection and for the present/excluded/... persistence loop, so a
    // Present file's content is never hashed twice.
    struct Walked {
        rel_str: String,
        cls: Classification,
        revision_classification: Option<RevisionClassification>,
        hash: Option<String>,
        byte_size: Option<i64>,
        mtime_ns: Option<i64>,
    }
    let mut walked: Vec<Walked> = Vec::with_capacity(files.len());
    let mut walked_paths: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(files.len());
    for abs in &files {
        let rel = match confine_to_root(&root, abs) {
            Ok(r) => r,
            Err(_) => continue, // already filtered by walk(), defensive
        };
        let rel_str = rel.to_string_lossy().into_owned().replace('\\', "/");
        let cls = classify(&rel_str, config);
        let revision_classification =
            matches!(cls, Classification::Present).then(|| revision_classification_for(&rel_str));
        let (hash, byte_size, mtime_ns) = if revision_classification.is_some() {
            let hash = content_hash_of_file(abs)?;
            let metadata = std::fs::metadata(abs)?;
            let byte_size = metadata.len() as i64;
            let mtime_ns = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            (Some(hash), Some(byte_size), Some(mtime_ns))
        } else {
            (None, None, None)
        };
        walked_paths.insert(rel_str.clone());
        walked.push(Walked {
            rel_str,
            cls,
            revision_classification,
            hash,
            byte_size,
            mtime_ns,
        });
    }

    let mut eligible: i64 = 0;
    let mut indexed: i64 = 0;
    let mut excluded: i64 = 0;
    let mut unsupported: i64 = 0;
    let mut failed: i64 = 0;
    let mut secret_excluded: i64 = 0;
    let mut parsed: i64 = 0;
    let mut indexed_entries: Vec<(String, String)> = Vec::new(); // (path, hash)
    let mut classified: Vec<ClassifiedFile> = Vec::with_capacity(walked.len());

    let tx = conn.unchecked_transaction()?;

    // Phase B: lifecycle detection against the generation about to be
    // superseded (still visible pre-activation). Renames move the
    // identity's `last_known_path` up front so the Phase C present-file
    // upsert below finds the existing identity by its new path instead of
    // minting a fresh one.
    let prev_present = crate::lifecycle::snapshot_previous_present(&tx, &ws.workspace_id)?;
    let new_present_hashes: std::collections::HashMap<String, String> = walked
        .iter()
        .filter(|w| matches!(w.cls, Classification::Present))
        .map(|w| (w.rel_str.clone(), w.hash.clone().unwrap()))
        .collect();
    let renames =
        crate::lifecycle::detect_renames(&prev_present, &walked_paths, &new_present_hashes);
    let renamed_old_paths: std::collections::HashSet<String> =
        renames.iter().map(|r| r.old_path.clone()).collect();
    for r in &renames {
        crate::lifecycle::record_rename(&tx, &ws.workspace_id, &candidate.generation_id, r)?;
    }

    for w in &walked {
        let rel_str = w.rel_str.clone();
        let cls = w.cls.clone();

        let mut cf = ClassifiedFile {
            canonical_relative_path: rel_str.clone(),
            classification: cls.clone(),
            content_hash: None,
            byte_size: None,
            observed_mtime_ns: None,
            artifact_class: None,
        };

        // Helper: register exclusion / failure rows for non-Present files.
        let ensure_file_id_for_present = |tx: &rusqlite::Transaction<'_>,
                                          path: &str,
                                          classification: RevisionClassification,
                                          hash: &str,
                                          byte_size: i64,
                                          mtime_ns: i64|
         -> Result<(String, String, bool)> {
            // Find or create file_identity keyed by (workspace_id, canonical path).
            let existing: Option<String> = tx
                .query_row(
                    "SELECT file_id FROM file_identity WHERE workspace_id = ?1 AND last_known_path = ?2",
                    params![ws.workspace_id, path],
                    |r| r.get(0),
                )
                .optional()?;
            let file_id = if let Some(id) = existing {
                id
            } else {
                let id = crate::ids::file_id_from_path(&ws.workspace_id, path);
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT INTO file_identity (file_id, workspace_id, identity_basis,
                        first_seen_at, last_seen_at, lifecycle_state, last_known_path)
                     VALUES (?1, ?2, 'created', ?3, ?3, 'current', ?4)",
                    params![id, ws.workspace_id, now, path],
                )?;
                id
            };
            // Reuse an immutable revision only when its complete material
            // identity matches; unchanged content never reruns providers.
            let language = classification.language.unwrap_or("");
            let existing_rev: Option<String> = tx
                .query_row(
                    "SELECT revision_id FROM file_revision
                     WHERE file_id = ?1 AND content_hash = ?2
                       AND artifact_class = ?3 AND COALESCE(language, '') = ?4
                       AND is_generated = ?5 AND is_test = ?6",
                    params![
                        file_id,
                        hash,
                        classification.artifact_class,
                        language,
                        classification.is_generated as i64,
                        classification.is_test as i64
                    ],
                    |r| r.get(0),
                )
                .optional()?;
            let (revision_id, is_new_revision) = if let Some(r) = existing_rev {
                (r, false)
            } else {
                let revision_id = crate::ids::file_revision_id(crate::ids::FileRevisionIdentity {
                    file_id: &file_id,
                    content_hash: hash,
                    artifact_class: classification.artifact_class,
                    language: classification.language,
                    is_generated: classification.is_generated,
                    is_test: classification.is_test,
                });
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT INTO file_revision (revision_id, file_id, content_hash, byte_size,
                        artifact_class, language, encoding, newline_style, project_key,
                        package_key, is_generated, is_test, discovered_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'utf-8', NULL, NULL, NULL, ?7, ?8, ?9)",
                    params![
                        revision_id,
                        file_id,
                        hash,
                        byte_size,
                        classification.artifact_class,
                        classification.language,
                        classification.is_generated as i64,
                        classification.is_test as i64,
                        now
                    ],
                )?;
                (revision_id, true)
            };
            // A present generation member must always point to a revision.
            tx.execute(
                "INSERT INTO generation_file (generation_id, file_id, revision_id, canonical_path,
                    presence_state, exclusion_code, exclusion_detail, observed_mtime_ns, observed_size)
                 VALUES (?1, ?2, ?3, ?4, 'present', NULL, NULL, ?5, ?6)
                 ON CONFLICT(generation_id, file_id) DO UPDATE SET
                    revision_id = excluded.revision_id,
                    presence_state = 'present',
                    exclusion_code = NULL,
                    exclusion_detail = NULL,
                    observed_mtime_ns = excluded.observed_mtime_ns,
                    observed_size = excluded.observed_size",
                params![candidate.generation_id, file_id, revision_id, path, mtime_ns, byte_size],
            )?;
            Ok((file_id, revision_id, is_new_revision))
        };

        match &cls {
            Classification::Present => {
                eligible += 1;
                let hash = w.hash.clone().unwrap();
                let byte_size = w.byte_size.unwrap();
                let mtime_ns = w.mtime_ns.unwrap();
                let revision_classification = w
                    .revision_classification
                    .expect("present files have material classification");
                let (_file_id, revision_id, is_new_revision) = ensure_file_id_for_present(
                    &tx,
                    &rel_str,
                    revision_classification,
                    &hash,
                    byte_size,
                    mtime_ns,
                )?;

                if is_new_revision {
                    // INV-006: unchanged content is reusable — only run the
                    // provider when the revision is genuinely new. Providers
                    // never write to the catalogue directly (INV-003); we
                    // persist their NormalizedBatch output here.
                    parsed += 1;
                    let provider_name = crate::providers::select_provider_for(&rel_str);
                    let text = std::fs::read_to_string(root.join(&rel_str)).ok();
                    let batch = match (provider_name, text.as_deref()) {
                        (crate::providers::DOCUMENT_CONFIG_PROVIDER, Some(t)) => {
                            crate::providers::run_document_config(&rel_str, t)
                        }
                        (crate::providers::TYPESCRIPT_PROVIDER, Some(t)) => {
                            crate::providers::run_typescript(&rel_str, t)
                        }
                        _ => crate::providers::run_text_fallback(&rel_str),
                    };
                    persist_provider_batch(&tx, &revision_id, byte_size, &batch)?;
                }

                cf.content_hash = Some(hash.clone());
                cf.byte_size = Some(byte_size);
                cf.observed_mtime_ns = Some(mtime_ns);
                cf.artifact_class = Some(revision_classification.artifact_class.to_string());
                indexed += 1;
                indexed_entries.push((rel_str, hash));
            }
            Classification::Excluded { code, detail } => {
                excluded += 1;
                // Insert file_identity + a generation_file row with presence_state='excluded'.
                let file_id = crate::ids::file_id_from_path(&ws.workspace_id, &rel_str);
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT OR IGNORE INTO file_identity (file_id, workspace_id, identity_basis,
                        first_seen_at, last_seen_at, lifecycle_state, last_known_path)
                     VALUES (?1, ?2, 'created', ?3, ?3, 'current', ?4)",
                    params![file_id, ws.workspace_id, now, rel_str],
                )?;
                tx.execute(
                    "INSERT INTO generation_file (generation_id, file_id, revision_id, canonical_path,
                        presence_state, exclusion_code, exclusion_detail, observed_mtime_ns, observed_size)
                     VALUES (?1, ?2, NULL, ?3, 'excluded', ?4, ?5, NULL, NULL)
                     ON CONFLICT(generation_id, file_id) DO UPDATE SET
                        presence_state = 'excluded',
                        exclusion_code = excluded.exclusion_code,
                        exclusion_detail = excluded.exclusion_detail",
                    params![candidate.generation_id, file_id, rel_str, code, detail],
                )?;
            }
            Classification::SecretExcluded { code, detail } => {
                secret_excluded += 1;
                excluded += 1;
                let file_id = crate::ids::file_id_from_path(&ws.workspace_id, &rel_str);
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT OR IGNORE INTO file_identity (file_id, workspace_id, identity_basis,
                        first_seen_at, last_seen_at, lifecycle_state, last_known_path)
                     VALUES (?1, ?2, 'created', ?3, ?3, 'current', ?4)",
                    params![file_id, ws.workspace_id, now, rel_str],
                )?;
                tx.execute(
                    "INSERT INTO generation_file (generation_id, file_id, revision_id, canonical_path,
                        presence_state, exclusion_code, exclusion_detail, observed_mtime_ns, observed_size)
                     VALUES (?1, ?2, NULL, ?3, 'excluded', ?4, ?5, NULL, NULL)
                     ON CONFLICT(generation_id, file_id) DO UPDATE SET
                        presence_state = 'excluded',
                        exclusion_code = excluded.exclusion_code,
                        exclusion_detail = excluded.exclusion_detail",
                    params![candidate.generation_id, file_id, rel_str, code, detail],
                )?;
            }
            Classification::Unsupported { code, detail } => {
                unsupported += 1;
                let file_id = crate::ids::file_id_from_path(&ws.workspace_id, &rel_str);
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT OR IGNORE INTO file_identity (file_id, workspace_id, identity_basis,
                        first_seen_at, last_seen_at, lifecycle_state, last_known_path)
                     VALUES (?1, ?2, 'created', ?3, ?3, 'current', ?4)",
                    params![file_id, ws.workspace_id, now, rel_str],
                )?;
                tx.execute(
                    "INSERT INTO generation_file (generation_id, file_id, revision_id, canonical_path,
                        presence_state, exclusion_code, exclusion_detail, observed_mtime_ns, observed_size)
                     VALUES (?1, ?2, NULL, ?3, 'unsupported', ?4, ?5, NULL, NULL)
                     ON CONFLICT(generation_id, file_id) DO UPDATE SET
                        presence_state = 'unsupported',
                        exclusion_code = excluded.exclusion_code,
                        exclusion_detail = excluded.exclusion_detail",
                    params![candidate.generation_id, file_id, rel_str, code, detail],
                )?;
            }
            Classification::Failed { code, detail } => {
                failed += 1;
                let file_id = crate::ids::file_id_from_path(&ws.workspace_id, &rel_str);
                let now = migrations::iso8601_now();
                tx.execute(
                    "INSERT OR IGNORE INTO file_identity (file_id, workspace_id, identity_basis,
                        first_seen_at, last_seen_at, lifecycle_state, last_known_path)
                     VALUES (?1, ?2, 'created', ?3, ?3, 'current', ?4)",
                    params![file_id, ws.workspace_id, now, rel_str],
                )?;
                tx.execute(
                    "INSERT INTO generation_file (generation_id, file_id, revision_id, canonical_path,
                        presence_state, exclusion_code, exclusion_detail, observed_mtime_ns, observed_size)
                     VALUES (?1, ?2, NULL, ?3, 'failed', ?4, ?5, NULL, NULL)
                     ON CONFLICT(generation_id, file_id) DO UPDATE SET
                        presence_state = 'failed',
                        exclusion_code = excluded.exclusion_code,
                        exclusion_detail = excluded.exclusion_detail",
                    params![candidate.generation_id, file_id, rel_str, code, detail],
                )?;
            }
        }

        classified.push(cf);
    }

    // Phase D: deletions -- previously-present paths neither walked this
    // cycle nor consumed by a rename are gone from disk.
    let deleted_file_count = crate::lifecycle::record_deletions(
        &tx,
        &ws.workspace_id,
        &candidate.generation_id,
        &prev_present,
        &walked_paths,
        &renamed_old_paths,
    )?;
    let renamed_file_count = renames.len() as i64;

    // Semantic reconcile (V1.1): project-scoped provider execution, SCIP
    // decode/map, persistence, and relationship resolution — inside the
    // same candidate-generation transaction, after file_identity/
    // file_revision rows for present files exist so SCIP documents can be
    // linked to a live revision (SCIP-004).
    let semantic_outcome = crate::semantic_reconcile::run_semantic_reconcile(
        &tx,
        ws,
        &candidate.generation_id,
        &root,
        config,
    )?;

    // Persist candidate counters before activation so status remains accurate.
    tx.execute(
        "UPDATE index_generation SET
            eligible_file_count = ?2,
            indexed_file_count = ?3,
            excluded_file_count = ?4,
            unsupported_file_count = ?5,
            failed_file_count = ?6
         WHERE generation_id = ?1",
        params![
            candidate.generation_id,
            eligible,
            indexed,
            excluded,
            unsupported,
            failed
        ],
    )?;

    tx.commit()?;

    // Compute the source-tree hash from the indexed files (deterministic).
    let source_tree_hash = source_tree_hash(indexed_entries);

    // Atomic activation. A required semantic provider's failure blocks
    // activation outright (RUN-014/INV-009/S-003) -- the candidate's facts
    // remain persisted (for diagnosis) but the previous committed
    // generation stays active, exactly like any other activation failure.
    let mut activation = "committed".to_string();
    let mut failure_code = None;
    let mut failure_message = None;
    if semantic_outcome.required_failure {
        let reason = semantic_outcome
            .failure_reason
            .clone()
            .unwrap_or_else(|| "required semantic provider failed".to_string());
        let _ = generation::fail_candidate(
            conn,
            &candidate.generation_id,
            "E_SEMANTIC_REQUIRED_PROVIDER",
            &reason,
        );
        activation = "failed".into();
        failure_code = Some("E_SEMANTIC_REQUIRED_PROVIDER".into());
        failure_message = Some(reason);
    } else if let Err(e) = generation::activate_candidate(
        conn,
        &ws.workspace_id,
        &candidate.generation_id,
        Some(&source_tree_hash),
    ) {
        // Roll the candidate to failed so we never present partial state.
        let _ = generation::fail_candidate(
            conn,
            &candidate.generation_id,
            "E_ACTIVATE",
            &format!("{e}"),
        );
        activation = "failed".into();
        failure_code = Some("E_ACTIVATE".into());
        failure_message = Some(format!("{e}"));
    }

    // Update per-file-class counters in generation counts (already updated
    // at candidate level above; the report returns them).
    let _ = GenerationState::Committed; // silence unused warning if any

    Ok(ReconcileReport {
        workspace_id: ws.workspace_id.clone(),
        previous_active_generation_id: previous_active,
        candidate_generation_id: candidate.generation_id,
        activation,
        failure_code,
        failure_message,
        source_tree_hash,
        eligible_file_count: eligible,
        indexed_file_count: indexed,
        excluded_file_count: excluded,
        unsupported_file_count: unsupported,
        failed_file_count: failed,
        secret_excluded_file_count: secret_excluded,
        parsed_file_count: parsed,
        renamed_file_count,
        deleted_file_count,
        semantic_scopes_considered: semantic_outcome.scopes_considered as i64,
        semantic_executions_run: semantic_outcome.executions_run as i64,
        semantic_executions_reused: semantic_outcome.executions_reused as i64,
        semantic_documents_processed: semantic_outcome.documents_processed as i64,
        semantic_symbols_persisted: semantic_outcome.symbols_persisted as i64,
        semantic_relationships_persisted: semantic_outcome.relationships_persisted as i64,
        semantic_resolutions_persisted: semantic_outcome.resolutions_persisted as i64,
        semantic_conflicts_persisted: semantic_outcome.conflicts_persisted as i64,
        semantic_degraded: semantic_outcome.degraded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_present_for_normal_source() {
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        assert_eq!(classify("src/lib.rs", &cfg), Classification::Present);
        assert_eq!(classify("README.md", &cfg), Classification::Present);
    }

    #[test]
    fn classify_secret_excluded_always() {
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        assert!(matches!(
            classify(".env", &cfg),
            Classification::SecretExcluded { .. }
        ));
        assert!(matches!(
            classify(".env.production", &cfg),
            Classification::SecretExcluded { .. }
        ));
        assert!(matches!(
            classify("keys/id_rsa", &cfg),
            Classification::SecretExcluded { .. }
        ));
        assert!(matches!(
            classify("certs/server.pem", &cfg),
            Classification::SecretExcluded { .. }
        ));
        assert!(matches!(
            classify("config/secrets.yaml", &cfg),
            Classification::SecretExcluded { .. }
        ));
    }

    #[test]
    fn classify_excludes_vendor_build_binary() {
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        assert!(matches!(
            classify("node_modules/lodash/index.js", &cfg),
            Classification::Excluded { .. }
        ));
        assert!(matches!(
            classify("target/release/app.exe", &cfg),
            Classification::Excluded { .. }
        ));
        assert!(matches!(
            classify("dist/bundle.min.js", &cfg),
            Classification::Excluded { .. }
        ));
        assert!(matches!(
            classify("img/logo.png", &cfg),
            Classification::Excluded { .. }
        ));
    }

    #[test]
    fn walk_finds_files_and_respects_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"// hi").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), b"ref:...").unwrap();
        // Symlink target outside root — should be rejected by the walker.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", root.join("evil")).ok();
        }
        let walked = walk(root).unwrap();
        let names: Vec<String> = walked
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
                    .replace('\\', "/")
            })
            .collect();
        assert!(names.iter().any(|n| n == "src/lib.rs"));
        assert!(!names.iter().any(|n| n.starts_with(".git/")));
        assert!(!names.iter().any(|n| n == "evil"));
    }

    #[test]
    fn file_classification_v2_matches_only_rust_under_exact_tests_segments() {
        let assert_classification = |path: &str, expected_class: &str, expected_is_test: bool| {
            let classification = revision_classification_for(path);
            assert_eq!(
                classification.artifact_class, expected_class,
                "unexpected artifact class for {path}"
            );
            assert_eq!(
                classification.is_test, expected_is_test,
                "unexpected test flag for {path}"
            );
            assert_eq!(classification.language, None);
            assert!(!classification.is_generated);
        };

        assert_classification("tests/calculate.rs", "test", true);
        assert_classification("crates/math/tests/integration.RS", "test", true);
        assert_classification("src/tests/calculate.rS", "test", true);

        assert_classification("src/tests.rs", "source", false);
        assert_classification("src/unit_test.rs", "source", false);
        assert_classification("Tests/calculate.rs", "source", false);
        assert_classification("src/calculate.rs", "source", false);
        assert_classification("tests/calculate.py", "source", false);

        for extension in ["md", "markdown", "txt", "rst", "adoc"] {
            assert_classification(&format!("tests/file.{extension}"), "documentation", false);
        }
        for extension in ["json", "yaml", "yml", "toml", "ini", "cfg", "conf", "env"] {
            assert_classification(&format!("tests/file.{extension}"), "configuration", false);
        }
        assert_classification("tests/Cargo.lock", "build_manifest", false);
        for extension in [
            "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp", "cs", "rb",
            "php", "sh", "sql",
        ] {
            assert_classification(&format!("tests/file.{extension}"), "source", false);
        }
        for extension in [
            "png", "jpg", "jpeg", "gif", "webp", "pdf", "zip", "7z", "tar", "gz", "bz2", "xz",
            "exe", "dll", "so", "dylib", "class", "jar", "war",
        ] {
            assert_classification(&format!("tests/file.{extension}"), "binary_metadata", false);
        }
        assert_classification("tests/file.unknown", "other", false);
    }

    #[test]
    fn unchanged_rust_test_reuses_producer_created_revision() {
        let (_database_directory, conn, ws, workspace_directory) = setup();
        std::fs::create_dir_all(workspace_directory.path().join("tests")).unwrap();
        std::fs::write(
            workspace_directory.path().join("tests/calculate.rs"),
            "#[test]\nfn calculates() { assert_eq!(2, 2); }\n",
        )
        .unwrap();
        let config =
            Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
                .unwrap();

        let first_generation = reconcile(&ws, &conn, &config)
            .unwrap()
            .candidate_generation_id;
        let first_revision: (String, String, i64) = conn
            .query_row(
                "SELECT revision_id, artifact_class, is_test FROM current_file
                 WHERE generation_id = ?1 AND canonical_path = 'tests/calculate.rs'",
                [&first_generation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(first_revision.1, "test");
        assert_eq!(first_revision.2, 1);

        std::fs::write(
            workspace_directory.path().join("src/a.ts"),
            "export const a = 2;\n",
        )
        .unwrap();
        let second_generation = reconcile(&ws, &conn, &config)
            .unwrap()
            .candidate_generation_id;
        let second_revision: String = conn
            .query_row(
                "SELECT revision_id FROM current_file
                 WHERE generation_id = ?1 AND canonical_path = 'tests/calculate.rs'",
                [&second_generation],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(second_revision, first_revision.0);
    }

    fn setup() -> (
        tempfile::TempDir,
        Connection,
        WorkspaceRecord,
        tempfile::TempDir,
    ) {
        use crate::catalogue::init_catalogue;
        use crate::workspace::register_workspace;
        let db_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let db_path = db_dir.path().join("atlas.sqlite");
        let conn = init_catalogue(&db_path, &cfg).unwrap();
        let ws = register_workspace(&conn, ws_dir.path(), &cfg, &db_path, "1.0.0").unwrap();
        reconcile(&ws, &conn, &cfg).unwrap();
        (db_dir, conn, ws, ws_dir)
    }

    #[test]
    fn rename_detected_via_exact_hash_moves_identity() {
        let (_d, conn, ws, ws_dir) = setup();
        let old_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/a.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        std::fs::create_dir_all(ws_dir.path().join("lib")).unwrap();
        std::fs::rename(
            ws_dir.path().join("src/a.ts"),
            ws_dir.path().join("lib/a.ts"),
        )
        .unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let report = reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(report.renamed_file_count, 1);
        assert_eq!(report.deleted_file_count, 0);

        // Same identity, moved -- not a fresh file_id, not a tombstone.
        let new_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'lib/a.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_id, new_id);
        let lifecycle_state: String = conn
            .query_row(
                "SELECT lifecycle_state FROM file_identity WHERE file_id = ?1",
                params![old_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lifecycle_state, "current");
        let event_type: String = conn
            .query_row(
                "SELECT event_type FROM lifecycle_event WHERE file_id = ?1 ORDER BY occurred_at DESC LIMIT 1",
                params![old_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event_type, "renamed");
        let tombstones: i64 = conn
            .query_row("SELECT COUNT(*) FROM tombstone", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tombstones, 0);
    }

    #[test]
    fn metrics_report_separates_file_provider_document_and_resolution_counts() {
        let (_d, conn, ws, ws_dir) = setup();
        std::fs::write(ws_dir.path().join("src/b.ts"), "export const b = 2;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let report = reconcile(&ws, &conn, &cfg).unwrap();

        let metrics = metrics_report(&report);
        // A structural-only config never configures a semantic provider --
        // core_changed_files must be nonzero while every provider/document/
        // resolution count stays at zero, proving they are independently
        // computed, not derived from one another.
        assert!(
            metrics.core_changed_files >= 1,
            "b.ts was newly indexed this pass"
        );
        assert_eq!(metrics.provider_executions_run, 0);
        assert_eq!(metrics.provider_executions_reused, 0);
        assert_eq!(metrics.emitted_documents, 0);
        assert_eq!(metrics.resolutions_persisted, 0);
        assert_eq!(metrics.conflicts_persisted, 0);
    }

    #[test]
    fn deletion_creates_metadata_only_tombstone() {
        let (_d, conn, ws, ws_dir) = setup();
        let file_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/a.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        std::fs::remove_file(ws_dir.path().join("src/a.ts")).unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        let report = reconcile(&ws, &conn, &cfg).unwrap();
        assert_eq!(report.deleted_file_count, 1);
        assert_eq!(report.renamed_file_count, 0);

        let lifecycle_state: String = conn
            .query_row(
                "SELECT lifecycle_state FROM file_identity WHERE file_id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lifecycle_state, "deleted");

        let (last_path, raw_state, artifact_class): (String, String, String) = conn
            .query_row(
                "SELECT last_path, raw_snapshot_state, artifact_class FROM tombstone WHERE file_id = ?1",
                params![file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(last_path, "src/a.ts");
        assert_eq!(raw_state, "not_retained");
        assert_eq!(artifact_class, "source");

        // Current-view cleanup: the deleted file no longer appears present.
        let present_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM current_file WHERE file_id = ?1 AND presence_state = 'present'",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(present_count, 0);
    }

    #[test]
    fn ambiguous_rename_does_not_merge_identities() {
        let (_d, conn, ws, ws_dir) = setup();
        // Add a second file identical in content to src/a.ts up front, then
        // in the same cycle delete both originals and create two more
        // identical-content files elsewhere. Two candidates on each side of
        // an exact-hash match is ambiguous (INV-024: no hidden inference) --
        // must be treated as independent deletions + creations, never a
        // guessed rename.
        std::fs::write(ws_dir.path().join("src/dup.ts"), "export const a = 1;\n").unwrap();
        let cfg = Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"t\"\n")
            .unwrap();
        reconcile(&ws, &conn, &cfg).unwrap();
        let orig_a_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/a.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let orig_dup_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/dup.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        std::fs::remove_file(ws_dir.path().join("src/a.ts")).unwrap();
        std::fs::remove_file(ws_dir.path().join("src/dup.ts")).unwrap();
        std::fs::write(ws_dir.path().join("src/moved1.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(ws_dir.path().join("src/moved2.ts"), "export const a = 1;\n").unwrap();
        let report = reconcile(&ws, &conn, &cfg).unwrap();

        // No rename asserted -- ambiguous on both sides.
        assert_eq!(report.renamed_file_count, 0);
        assert_eq!(report.deleted_file_count, 2);

        // Both original identities are marked deleted, not silently reused.
        for id in [&orig_a_id, &orig_dup_id] {
            let state: String = conn
                .query_row(
                    "SELECT lifecycle_state FROM file_identity WHERE file_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, "deleted");
        }

        // The two new paths got fresh identities distinct from the deleted
        // ones (no false merge onto either original).
        let new1_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/moved1.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new2_id: String = conn
            .query_row(
                "SELECT file_id FROM file_identity WHERE last_known_path = 'src/moved2.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(new1_id, orig_a_id);
        assert_ne!(new1_id, orig_dup_id);
        assert_ne!(new2_id, orig_a_id);
        assert_ne!(new2_id, orig_dup_id);
        assert_ne!(new1_id, new2_id);

        // Two tombstones recorded, one per genuinely deleted original.
        let tombstones: i64 = conn
            .query_row("SELECT COUNT(*) FROM tombstone", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tombstones, 2);
    }
}
