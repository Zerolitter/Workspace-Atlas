//! Application configuration loaded from TOML.
//!
//! The public shape is defined by these `serde` types and
//! `config/workspace-atlas-v1.1.config.example.toml`. Parsing validates the
//! structural shape; `Config::validate` and discovery enforce semantics.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{AtlasError, Result};

/// Default schema version emitted by `Config::default_v1_1_template` and
/// validated by new configs. Configs pinned at `"1.0.0"` remain valid through
/// `SUPPORTED_SCHEMA_VERSIONS`; later sections are additive and defaulted.
pub const SCHEMA_VERSION: &str = "1.1.0";

/// Every `schema_version` this binary accepts.
pub const SUPPORTED_SCHEMA_VERSIONS: &[&str] = &["1.0.0", "1.1.0"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: String,
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub reconcile: ReconcileConfig,
    /// V1.1 (schema 1.1.0+). Absent entirely in a V1 (`1.0.0`) config, which
    /// parses with the section defaulted.
    #[serde(default)]
    pub provider_runtime: ProviderRuntimeConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfigEntry>,
    #[serde(default)]
    pub evidence: EvidenceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub display_name: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub canonical_root_label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub busy_timeout_ms: Option<u32>,
    #[serde(default)]
    pub journal_mode: Option<String>,
    #[serde(default)]
    pub synchronous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default = "default_symlink_policy")]
    pub symlink_policy: SymlinkPolicy,
    #[serde(default = "default_max_file_size")]
    pub max_file_size_bytes: u64,
    #[serde(default = "default_encoding")]
    pub default_encoding: String,
    #[serde(default = "default_newlines")]
    pub newlines: Vec<String>,
    #[serde(default)]
    pub exclude: ExcludeConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkPolicy {
    Reject,
    Allow,
    LogOnly,
}

fn default_symlink_policy() -> SymlinkPolicy {
    SymlinkPolicy::Reject
}
fn default_max_file_size() -> u64 {
    4 * 1024 * 1024
}
fn default_encoding() -> String {
    "utf-8".to_string()
}
fn default_newlines() -> Vec<String> {
    vec!["lf".to_string(), "crlf".to_string(), "cr".to_string()]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludeConfig {
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub vendor_paths: Vec<String>,
    #[serde(default)]
    pub build_paths: Vec<String>,
    #[serde(default)]
    pub generated_paths: Vec<String>,
    #[serde(default)]
    pub binary_paths: Vec<String>,
    #[serde(default)]
    pub archive_paths: Vec<String>,
    #[serde(skip)]
    compiled: std::sync::OnceLock<CompiledPatternPolicy>,
}

#[derive(Debug, Clone)]
struct CompiledPatternPolicy {
    patterns: regex::RegexSet,
    vendor_paths: regex::RegexSet,
    build_paths: regex::RegexSet,
    generated_paths: regex::RegexSet,
    binary_paths: regex::RegexSet,
    archive_paths: regex::RegexSet,
}

impl ExcludeConfig {
    fn compile_field(field: &str, patterns: &[String]) -> Result<regex::RegexSet> {
        for (index, pattern) in patterns.iter().enumerate() {
            regex::Regex::new(pattern).map_err(|error| {
                AtlasError::InvalidConfig(format!(
                    "policy.exclude.{field}[{index}] is an invalid regular expression: {error}"
                ))
            })?;
        }
        regex::RegexSet::new(patterns).map_err(|error| {
            AtlasError::InvalidConfig(format!(
                "policy.exclude.{field} contains an invalid regular expression: {error}"
            ))
        })
    }

    fn compiled(&self) -> Result<&CompiledPatternPolicy> {
        if let Some(compiled) = self.compiled.get() {
            return Ok(compiled);
        }
        let compiled = CompiledPatternPolicy {
            patterns: Self::compile_field("patterns", &self.patterns)?,
            vendor_paths: Self::compile_field("vendor_paths", &self.vendor_paths)?,
            build_paths: Self::compile_field("build_paths", &self.build_paths)?,
            generated_paths: Self::compile_field("generated_paths", &self.generated_paths)?,
            binary_paths: Self::compile_field("binary_paths", &self.binary_paths)?,
            archive_paths: Self::compile_field("archive_paths", &self.archive_paths)?,
        };
        let _ = self.compiled.set(compiled);
        Ok(self
            .compiled
            .get()
            .expect("compiled pattern policy was initialized"))
    }

    pub(crate) fn pattern_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .patterns
            .matches(path)
            .iter()
            .next()
            .map(|index| self.patterns[index].as_str())
    }

    pub(crate) fn vendor_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .vendor_paths
            .matches(path)
            .iter()
            .next()
            .map(|index| self.vendor_paths[index].as_str())
    }

    pub(crate) fn build_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .build_paths
            .matches(path)
            .iter()
            .next()
            .map(|index| self.build_paths[index].as_str())
    }

    pub(crate) fn generated_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .generated_paths
            .matches(path)
            .iter()
            .next()
            .map(|index| self.generated_paths[index].as_str())
    }

    pub(crate) fn binary_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .binary_paths
            .matches(path)
            .iter()
            .next()
            .map(|index| self.binary_paths[index].as_str())
    }

    pub(crate) fn archive_match(&self, path: &str) -> Option<&str> {
        self.compiled()
            .expect("configuration patterns were validated")
            .archive_paths
            .matches(path)
            .iter()
            .next()
            .map(|index| self.archive_paths[index].as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_hash_suspect_on_metadata_change")]
    pub hash_suspect_on_metadata_change: bool,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            interval_seconds: default_interval_seconds(),
            hash_suspect_on_metadata_change: default_hash_suspect_on_metadata_change(),
        }
    }
}

fn default_debounce_ms() -> u64 {
    250
}
fn default_interval_seconds() -> u64 {
    300
}
fn default_hash_suspect_on_metadata_change() -> bool {
    true
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            symlink_policy: default_symlink_policy(),
            max_file_size_bytes: default_max_file_size(),
            default_encoding: default_encoding(),
            newlines: default_newlines(),
            exclude: ExcludeConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// V1.1 provider-runtime configuration (CFG-003)
// ---------------------------------------------------------------------------

/// Matches `config/workspace-atlas-v1.1.config.example.toml`'s
/// `[provider_runtime]` section. Controls include direct spawn only
/// (`allow_shell` must be `false`), a minimal environment allowlist, no
/// inherited credentials by default, and bounded output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRuntimeConfig {
    #[serde(default = "default_allow_shell")]
    pub allow_shell: bool,
    #[serde(default = "default_inherit_environment")]
    pub inherit_environment: bool,
    #[serde(default = "default_allowed_environment")]
    pub allowed_environment: Vec<String>,
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: u64,
    #[serde(default = "default_graceful_cancel_ms")]
    pub graceful_cancel_ms: u64,
    #[serde(default = "default_max_stdout_bytes")]
    pub max_stdout_bytes: u64,
    #[serde(default = "default_max_stderr_bytes")]
    pub max_stderr_bytes: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: u64,
    #[serde(default = "default_temporary_root_policy")]
    pub temporary_root_policy: String,
    #[serde(default = "default_network_isolation_policy")]
    pub network_isolation_policy: NetworkIsolationPolicy,
    #[serde(default)]
    pub retain_raw_output: bool,
}

impl Default for ProviderRuntimeConfig {
    fn default() -> Self {
        Self {
            allow_shell: default_allow_shell(),
            inherit_environment: default_inherit_environment(),
            allowed_environment: default_allowed_environment(),
            default_timeout_ms: default_timeout_ms(),
            graceful_cancel_ms: default_graceful_cancel_ms(),
            max_stdout_bytes: default_max_stdout_bytes(),
            max_stderr_bytes: default_max_stderr_bytes(),
            max_output_bytes: default_max_output_bytes(),
            temporary_root_policy: default_temporary_root_policy(),
            network_isolation_policy: default_network_isolation_policy(),
            retain_raw_output: false,
        }
    }
}

fn default_allow_shell() -> bool {
    false
}
fn default_inherit_environment() -> bool {
    false
}
fn default_allowed_environment() -> Vec<String> {
    [
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "HOME",
        "USERPROFILE",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
fn default_timeout_ms() -> u64 {
    120_000
}
fn default_graceful_cancel_ms() -> u64 {
    1_500
}
fn default_max_stdout_bytes() -> u64 {
    1_048_576
}
fn default_max_stderr_bytes() -> u64 {
    1_048_576
}
fn default_max_output_bytes() -> u64 {
    536_870_912
}
fn default_temporary_root_policy() -> String {
    "application_private".to_string()
}
fn default_network_isolation_policy() -> NetworkIsolationPolicy {
    NetworkIsolationPolicy::BestEffortAllowed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkIsolationPolicy {
    NotRequired,
    RequireEnforced,
    BestEffortAllowed,
}

/// One `[[providers]]` table entry. `kind = "builtin"` providers need no
/// `command`/`arguments`; `kind = "external_process"` (template's
/// `external_scip` etc.) requires an explicit `command` per RUN-001 (never a
/// shell string).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigEntry {
    pub name: String,
    pub kind: String,
    pub tier: String,
    pub scope: String,
    #[serde(default = "default_provider_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub project_markers: Vec<String>,
    /// Pinned external provider version used in provenance identity.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub probe_arguments: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<u64>,
    #[serde(default = "default_provider_configuration")]
    pub configuration: serde_json::Value,
}

fn default_provider_enabled() -> bool {
    true
}
fn default_provider_configuration() -> serde_json::Value {
    serde_json::json!({})
}

impl ProviderConfigEntry {
    /// Deterministic SHA-256 over the entry's `configuration` table alone
    /// (matches `workspace_provider.configuration_hash` /
    /// `ProviderRef.configuration_hash`). Uses the same sorted-key canonical
    /// JSON as `provider_contract::canonical_json_bytes`.
    pub fn configuration_hash(&self) -> String {
        crate::hashing::content_hash_of_bytes(&crate::provider_contract::canonical_json_bytes(
            &self.configuration,
        ))
    }

    /// Explicit version, with the historical TypeScript pin retained for
    /// existing configurations.
    pub fn resolved_version(&self) -> Option<String> {
        self.version
            .clone()
            .or_else(|| (self.name == "scip-typescript").then(|| "0.4.0".to_string()))
    }
}

/// Mirrors `[evidence]` / `[evidence.default_priority]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceConfig {
    #[serde(default = "default_projection_policy_version")]
    pub projection_policy_version: String,
    #[serde(default = "default_resolution_policy_version")]
    pub resolution_policy_version: String,
    #[serde(default)]
    pub default_priority: EvidencePriorityConfig,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            projection_policy_version: default_projection_policy_version(),
            resolution_policy_version: default_resolution_policy_version(),
            default_priority: EvidencePriorityConfig::default(),
        }
    }
}

fn default_projection_policy_version() -> String {
    "preferred-evidence-1.0.0".to_string()
}
fn default_resolution_policy_version() -> String {
    "semantic-resolution-1.0.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePriorityConfig {
    #[serde(default = "default_priority_compiler")]
    pub compiler: i64,
    #[serde(default = "default_priority_runtime_observation")]
    pub runtime_observation: i64,
    #[serde(default = "default_priority_semantic_index")]
    pub semantic_index: i64,
    #[serde(default = "default_priority_language_server")]
    pub language_server: i64,
    #[serde(default = "default_priority_structural")]
    pub structural: i64,
    #[serde(default = "default_priority_document_config")]
    pub document_config: i64,
    #[serde(default = "default_priority_textual")]
    pub textual: i64,
    #[serde(default = "default_priority_heuristic")]
    pub heuristic: i64,
}

impl Default for EvidencePriorityConfig {
    fn default() -> Self {
        Self {
            compiler: default_priority_compiler(),
            runtime_observation: default_priority_runtime_observation(),
            semantic_index: default_priority_semantic_index(),
            language_server: default_priority_language_server(),
            structural: default_priority_structural(),
            document_config: default_priority_document_config(),
            textual: default_priority_textual(),
            heuristic: default_priority_heuristic(),
        }
    }
}

fn default_priority_compiler() -> i64 {
    900
}
fn default_priority_runtime_observation() -> i64 {
    850
}
fn default_priority_semantic_index() -> i64 {
    800
}
fn default_priority_language_server() -> i64 {
    700
}
fn default_priority_structural() -> i64 {
    500
}
fn default_priority_document_config() -> i64 {
    300
}
fn default_priority_textual() -> i64 {
    100
}
fn default_priority_heuristic() -> i64 {
    50
}

impl Config {
    /// Load + parse + validate a config from a TOML file. Returns an
    /// `InvalidConfig` error if the schema version does not match
    /// `SCHEMA_VERSION` or any required field is missing.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse a config from an in-memory TOML string. Useful for tests.
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if !SUPPORTED_SCHEMA_VERSIONS.contains(&self.schema_version.as_str()) {
            return Err(AtlasError::InvalidConfig(format!(
                "schema_version must be one of {:?}, got {:?}",
                SUPPORTED_SCHEMA_VERSIONS, self.schema_version
            )));
        }
        if self.workspace.display_name.trim().is_empty() {
            return Err(AtlasError::InvalidConfig(
                "workspace.display_name must not be empty".into(),
            ));
        }
        for nl in &self.policy.newlines {
            if !matches!(nl.as_str(), "lf" | "crlf" | "cr") {
                return Err(AtlasError::InvalidConfig(format!(
                    "policy.newlines contains unknown value {:?}; allowed: lf, crlf, cr",
                    nl
                )));
            }
        }
        self.policy.exclude.compiled()?;
        // RUN-001 / ADR-025: direct spawn only, never a shell.
        if self.provider_runtime.allow_shell {
            return Err(AtlasError::InvalidConfig(
                "provider_runtime.allow_shell must be false — Atlas invokes providers \
                 directly and never through a shell"
                    .into(),
            ));
        }
        let mut seen_names = std::collections::HashSet::new();
        for provider in &self.providers {
            if !seen_names.insert(provider.name.clone()) {
                return Err(AtlasError::InvalidConfig(format!(
                    "duplicate [[providers]] entry for name {:?}",
                    provider.name
                )));
            }
            if !matches!(
                provider.kind.as_str(),
                "builtin" | "external_process" | "external_scip"
            ) {
                return Err(AtlasError::InvalidConfig(format!(
                    "providers[{:?}].kind {:?} is not a supported provider kind",
                    provider.name, provider.kind
                )));
            }
            if provider.kind != "builtin" && provider.command.is_none() {
                return Err(AtlasError::InvalidConfig(format!(
                    "providers[{:?}] is an external provider (kind = {:?}) and must set an \
                     explicit `command` — Atlas never resolves a shell-expanded PATH lookup",
                    provider.name, provider.kind
                )));
            }
            if !matches!(
                provider.scope.as_str(),
                "file" | "project" | "package" | "workspace"
            ) {
                return Err(AtlasError::InvalidConfig(format!(
                    "providers[{:?}].scope {:?} is not a supported execution scope",
                    provider.name, provider.scope
                )));
            }
            if provider.kind == "external_scip" && provider.resolved_version().is_none() {
                return Err(AtlasError::InvalidConfig(format!(
                    "providers[{:?}] is an external SCIP provider and must set an explicit `version` pin",
                    provider.name
                )));
            }
            if provider.kind == "external_scip" && provider.languages.is_empty() {
                return Err(AtlasError::InvalidConfig(format!(
                    "providers[{:?}] is an external SCIP provider and must declare at least one `languages` entry",
                    provider.name
                )));
            }
        }
        Ok(())
    }

    /// Validate the one creation-era registered policy whose exclusion
    /// entries were basename globs before the current regex contract existed.
    ///
    /// Callers must first prove the registered configuration hash and
    /// workspace provenance. This method deliberately accepts no other glob
    /// spelling and leaves the serialized fields unchanged, so configuration
    /// identity remains the historical normalized SHA-256.
    pub(crate) fn validate_historical_registered_globs(&self) -> Result<()> {
        const HISTORICAL_GLOBS: [&str; 3] = [".env", "*.pem", "*.key"];
        const HISTORICAL_REGEX: [&str; 3] =
            [r"(^|/)\.env$", r"(^|/)[^/]*\.pem$", r"(^|/)[^/]*\.key$"];

        if !self
            .policy
            .exclude
            .patterns
            .iter()
            .map(String::as_str)
            .eq(HISTORICAL_GLOBS)
        {
            return Err(AtlasError::InvalidConfig(
                "configuration_mismatch: registered configuration does not match the bounded pre-regex pattern contract"
                    .into(),
            ));
        }

        let mut validated = self.clone();
        validated.policy.exclude.patterns = HISTORICAL_REGEX
            .iter()
            .map(|pattern| (*pattern).into())
            .collect();
        validated.policy.exclude.compiled = std::sync::OnceLock::new();
        validated.validate()?;
        let compiled = validated
            .policy
            .exclude
            .compiled
            .into_inner()
            .expect("validated patterns were compiled");
        let _ = self.policy.exclude.compiled.set(compiled);
        Ok(())
    }

    /// Compute the configuration hash used to invalidate revisions whenever
    /// policy changes. Deterministically serializes the complete validated
    /// configuration, so every behavior-affecting field participates.
    pub fn configuration_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).expect("config serialises");
        let mut h = Sha256::new();
        h.update(&bytes);
        hex::encode(h.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        schema_version = "1.0.0"
        [workspace]
        display_name = "Test"
        [policy]
        max_file_size_bytes = 1024
        "#;

    #[test]
    fn parses_minimal_config() {
        let cfg = Config::parse(VALID).unwrap();
        assert_eq!(cfg.workspace.display_name, "Test");
        assert_eq!(cfg.policy.max_file_size_bytes, 1024);
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let bad = r#"
            schema_version = "0.9.0"
            [workspace]
            display_name = "Test"
            "#;
        let err = Config::parse(bad).unwrap_err();
        assert!(matches!(err, AtlasError::InvalidConfig(_)));
    }

    #[test]
    fn rejects_unknown_newline() {
        let bad = r#"
            schema_version = "1.0.0"
            [workspace]
            display_name = "Test"
            [policy]
            newlines = ["lf", "mixed"]
            "#;
        assert!(Config::parse(bad).is_err());
    }

    #[test]
    fn configuration_hash_is_deterministic() {
        let a = Config::parse(VALID).unwrap().configuration_hash();
        let b = Config::parse(VALID).unwrap().configuration_hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn parses_public_provider_config_with_four_providers() {
        let text = std::fs::read_to_string("config/workspace-atlas-v1.1.config.example.toml")
            .expect("public provider config must be present in config/");
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.schema_version, "1.1.0");
        assert_eq!(cfg.providers.len(), 4);
        assert!(cfg.providers.iter().any(|p| p.name == "scip-typescript"));
        assert!(cfg.providers.iter().any(|p| p.name == "rust-analyzer"));
        let scip = cfg
            .providers
            .iter()
            .find(|p| p.name == "scip-typescript")
            .unwrap();
        assert_eq!(scip.command.as_deref(), Some("scip-typescript"));
        assert_eq!(scip.scope, "project");
        assert!(!cfg.provider_runtime.allow_shell);
        assert_eq!(
            cfg.provider_runtime.network_isolation_policy,
            NetworkIsolationPolicy::BestEffortAllowed
        );
        assert_eq!(cfg.evidence.default_priority.compiler, 900);
        assert_eq!(cfg.evidence.default_priority.semantic_index, 800);
    }

    #[test]
    fn v1_config_without_provider_sections_still_parses() {
        // Pre-V1.1 configs never mention [provider_runtime]/[[providers]] —
        // they must keep parsing with defaulted V1.1 sections.
        let cfg = Config::parse(VALID).unwrap();
        assert!(cfg.providers.is_empty());
        assert!(!cfg.provider_runtime.allow_shell);
    }

    #[test]
    fn rejects_allow_shell_true() {
        let bad = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "Test"
            [provider_runtime]
            allow_shell = true
            "#;
        let err = Config::parse(bad).unwrap_err();
        assert!(matches!(err, AtlasError::InvalidConfig(_)));
    }

    #[test]
    fn rejects_external_provider_without_command() {
        let bad = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "Test"
            [[providers]]
            name = "scip-typescript"
            kind = "external_scip"
            tier = "semantic_index"
            scope = "project"
            "#;
        let err = Config::parse(bad).unwrap_err();
        assert!(matches!(err, AtlasError::InvalidConfig(_)));
    }

    #[test]
    fn rejects_unknown_provider_field_fail_closed() {
        let bad = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "Test"
            [[providers]]
            name = "x"
            kind = "builtin"
            tier = "structural"
            scope = "file"
            totally_unexpected_field = true
            "#;
        assert!(
            Config::parse(bad).is_err(),
            "unknown [[providers]] field must fail closed (CFG-009)"
        );
    }

    #[test]
    fn rejects_duplicate_provider_name() {
        let bad = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "Test"
            [[providers]]
            name = "dup"
            kind = "builtin"
            tier = "structural"
            scope = "file"
            [[providers]]
            name = "dup"
            kind = "builtin"
            tier = "structural"
            scope = "file"
            "#;
        let err = Config::parse(bad).unwrap_err();
        assert!(matches!(err, AtlasError::InvalidConfig(_)));
    }

    #[test]
    fn rust_external_scip_requires_a_pinned_version() {
        let missing = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "Rust fixture"
            [[providers]]
            name = "rust-analyzer"
            kind = "external_scip"
            tier = "semantic_index"
            scope = "project"
            languages = ["rust"]
            project_markers = ["Cargo.toml"]
            command = "rust-analyzer"
            arguments = ["scip", ".", "--output", "{output_file}"]
            "#;
        assert!(Config::parse(missing).is_err());
        let pinned = missing.replace(
            "command = \"rust-analyzer\"",
            "version = \"1.94.1\"\n            command = \"rust-analyzer\"",
        );
        let cfg = Config::parse(&pinned).unwrap();
        assert_eq!(
            cfg.providers[0].resolved_version().as_deref(),
            Some("1.94.1")
        );
    }

    #[test]
    fn external_scip_provider_requires_declared_languages() {
        let text = r#"
            schema_version = "1.1.0"
            [workspace]
            display_name = "No language fixture"
            [[providers]]
            name = "rust-analyzer"
            version = "1.94.1"
            kind = "external_scip"
            tier = "semantic_index"
            scope = "project"
            project_markers = ["Cargo.toml"]
            command = "rust-analyzer"
            "#;
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn example_config_declares_pinned_rust_analyzer_scip_provider() {
        let text =
            std::fs::read_to_string("config/workspace-atlas-v1.1.config.example.toml").unwrap();
        let cfg = Config::parse(&text).unwrap();
        let rust = cfg
            .providers
            .iter()
            .find(|p| p.name == "rust-analyzer")
            .unwrap();
        assert_eq!(rust.resolved_version().as_deref(), Some("1.94.1"));
        assert_eq!(rust.languages, vec!["rust"]);
        assert_eq!(rust.project_markers, vec!["Cargo.toml"]);
        assert_eq!(
            rust.arguments,
            vec!["scip", ".", "--output", "{output_file}"]
        );
    }

    #[test]
    fn provider_configuration_hash_is_deterministic() {
        let text =
            std::fs::read_to_string("config/workspace-atlas-v1.1.config.example.toml").unwrap();
        let cfg = Config::parse(&text).unwrap();
        let scip = cfg
            .providers
            .iter()
            .find(|p| p.name == "scip-typescript")
            .unwrap();
        let a = scip.configuration_hash();
        let b = scip.configuration_hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }
}
