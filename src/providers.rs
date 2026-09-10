//! Provider boundary.
//!
//! Providers translate artifact-specific structure into normalized symbols
//! and relationships. Providers never write to the catalogue directly;
//! `discovery::reconcile` persists their output.
//!
//! V1 providers (ADR-013r1):
//! - `file_metadata`: path/size/mtime/hash facts for every present file.
//! - `document_config`: Markdown heading extraction; JSON/YAML/TOML top-level
//!   key extraction.
//! - `typescript`: structural provider for `.ts`/`.tsx`/`.js`/`.jsx` — regex +
//!   brace-depth extraction of function/class/const/export declarations.
//!   Evidence tier `structural` (not `semantic` — no type resolution).
//! - `text_fallback`: byte-count-only textual evidence for anything else.

use serde::{Deserialize, Serialize};

/// Evidence tier ordered by the fixed preferred-projection policy and
/// constrained by the `provider_descriptor.provider_tier` database domain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTier {
    Compiler,
    SemanticIndex,
    LanguageServer,
    Structural,
    DocumentConfig,
    Textual,
    Heuristic,
    RuntimeObservation,
}

impl ProviderTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Compiler => "compiler",
            Self::SemanticIndex => "semantic_index",
            Self::LanguageServer => "language_server",
            Self::Structural => "structural",
            Self::DocumentConfig => "document_config",
            Self::Textual => "textual",
            Self::Heuristic => "heuristic",
            Self::RuntimeObservation => "runtime_observation",
        }
    }

    /// Fixed evidence rank used by preferred projections.
    pub fn default_rank(&self) -> i64 {
        match self {
            Self::Compiler => 900,
            Self::RuntimeObservation => 850,
            Self::SemanticIndex => 800,
            Self::LanguageServer => 700,
            Self::Structural => 500,
            Self::DocumentConfig => 300,
            Self::Textual => 100,
            Self::Heuristic => 50,
        }
    }
}

/// Evidence method attached to a normalized fact.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum EvidenceMethod {
    Structural,
    Textual,
    Declared,
}

impl EvidenceMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Textual => "textual",
            Self::Declared => "declared",
        }
    }
}

/// A normalized symbol fact, mirroring `symbol_fact` table columns.
#[derive(Debug, Clone, Serialize)]
pub struct NormalizedSymbol {
    pub canonical_symbol_key: String,
    pub symbol_kind: String,
    pub display_name: String,
    pub qualified_name: String,
    pub signature: Option<String>,
    pub start_byte: i64,
    pub end_byte: i64,
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
    pub evidence_method: EvidenceMethod,
    pub confidence: f64,
    pub evidence_reason: String,
}

/// A normalized relationship fact, mirroring `relationship_fact`.
#[derive(Debug, Clone, Serialize)]
pub struct NormalizedRelationship {
    pub relationship_type: String,
    pub source_ref_kind: String,
    pub source_ref_value: String,
    pub target_ref_kind: String,
    pub target_ref_value: String,
    pub evidence_method: EvidenceMethod,
    pub confidence: f64,
    pub evidence_reason: String,
}

/// Output of one provider run over one file (INV-003: providers never write
/// to the catalogue; `discovery::reconcile` persists this batch).
#[derive(Debug, Clone, Serialize)]
pub struct NormalizedBatch {
    pub provider_name: String,
    pub provider_version: String,
    pub provider_tier: ProviderTier,
    pub deterministic: bool,
    pub symbols: Vec<NormalizedSymbol>,
    pub relationships: Vec<NormalizedRelationship>,
    pub diagnostics: Vec<String>,
}

impl NormalizedBatch {
    fn empty(provider_name: &str, provider_version: &str, tier: ProviderTier) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            provider_version: provider_version.to_string(),
            provider_tier: tier,
            deterministic: true,
            symbols: Vec::new(),
            relationships: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

/// Compute 1-based line/column for a byte offset in `text` (UTF-8, LF or
/// CRLF). O(n) per call, bounded by one source file.
fn line_col_at(text: &str, byte_offset: usize) -> (i64, i64) {
    let mut line: i64 = 1;
    let mut col: i64 = 1;
    for (i, ch) in text.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

// ---------------------------------------------------------------------------
// file_metadata provider
// ---------------------------------------------------------------------------

pub const FILE_METADATA_PROVIDER: &str = "file_metadata";
pub const FILE_METADATA_VERSION: &str = "1.0.0";

/// Emits exactly one declared "file" symbol per present file. Always
/// succeeds; carries no relationship evidence.
pub fn run_file_metadata(rel_path: &str, byte_len: usize) -> NormalizedBatch {
    let mut batch = NormalizedBatch::empty(
        FILE_METADATA_PROVIDER,
        FILE_METADATA_VERSION,
        ProviderTier::DocumentConfig,
    );
    batch.symbols.push(NormalizedSymbol {
        canonical_symbol_key: format!("file:{rel_path}"),
        symbol_kind: "file".into(),
        display_name: rel_path.rsplit('/').next().unwrap_or(rel_path).to_string(),
        qualified_name: rel_path.to_string(),
        signature: None,
        start_byte: 0,
        end_byte: byte_len as i64,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 1,
        evidence_method: EvidenceMethod::Declared,
        confidence: 1.0,
        evidence_reason: "file discovered and hashed".into(),
    });
    batch
}

// ---------------------------------------------------------------------------
// document_config provider
// ---------------------------------------------------------------------------

pub const DOCUMENT_CONFIG_PROVIDER: &str = "document_config";
pub const DOCUMENT_CONFIG_VERSION: &str = "1.0.0";

/// Extract Markdown ATX headings (`# Title`) or top-level JSON/YAML/TOML keys.
pub fn run_document_config(rel_path: &str, text: &str) -> NormalizedBatch {
    let mut batch = NormalizedBatch::empty(
        DOCUMENT_CONFIG_PROVIDER,
        DOCUMENT_CONFIG_VERSION,
        ProviderTier::DocumentConfig,
    );

    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        extract_markdown_headings(rel_path, text, &mut batch);
    } else if lower.ends_with(".json") {
        extract_json_top_level_keys(rel_path, text, &mut batch);
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        extract_yaml_toplevel_keys(rel_path, text, &mut batch);
    } else if lower.ends_with(".toml") {
        extract_toml_top_level_keys(rel_path, text, &mut batch);
    }

    batch
}

fn extract_markdown_headings(rel_path: &str, text: &str, batch: &mut NormalizedBatch) {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let stripped = trimmed.trim_start();
        if let Some(hashes_end) = stripped.find(|c: char| c != '#') {
            if hashes_end > 0
                && hashes_end <= 6
                && stripped.as_bytes().get(hashes_end) == Some(&b' ')
            {
                let title = stripped[hashes_end..].trim().to_string();
                let leading_ws = line.len() - line.trim_start().len();
                let start = offset + leading_ws;
                let (start_line, start_col) = line_col_at(text, start);
                batch.symbols.push(NormalizedSymbol {
                    canonical_symbol_key: format!("md-heading:{rel_path}#{title}@{offset}"),
                    symbol_kind: "heading".into(),
                    display_name: title.clone(),
                    qualified_name: format!("{rel_path}#{title}"),
                    signature: Some(format!("h{hashes_end}")),
                    start_byte: start as i64,
                    end_byte: (offset + trimmed.len()) as i64,
                    start_line,
                    start_column: start_col,
                    end_line: start_line,
                    end_column: start_col + trimmed.len() as i64,
                    evidence_method: EvidenceMethod::Structural,
                    confidence: 0.9,
                    evidence_reason: "ATX heading pattern".into(),
                });
            }
        }
        offset += line.len();
    }
}

fn extract_json_top_level_keys(rel_path: &str, text: &str, batch: &mut NormalizedBatch) {
    // Minimal, dependency-free top-level key scan: any `"..."` string that
    // immediately follows `{` or `,` at brace/bracket depth 1 is treated as
    // a key. Not a full JSON parser (structural evidence, not semantic).
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut expect_key = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' => {
                let key_start = i;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if depth == 1 && expect_key && j < bytes.len() {
                    let key = &text[key_start + 1..j];
                    let (start_line, start_col) = line_col_at(text, key_start);
                    batch.symbols.push(NormalizedSymbol {
                        canonical_symbol_key: format!("json-key:{rel_path}#{key}@{key_start}"),
                        symbol_kind: "config_key".into(),
                        display_name: key.to_string(),
                        qualified_name: format!("{rel_path}#{key}"),
                        signature: None,
                        start_byte: key_start as i64,
                        end_byte: (j + 1) as i64,
                        start_line,
                        start_column: start_col,
                        end_line: start_line,
                        end_column: start_col + (j - key_start) as i64,
                        evidence_method: EvidenceMethod::Structural,
                        confidence: 0.85,
                        evidence_reason: "top-level JSON key".into(),
                    });
                }
                expect_key = false;
                i = (j + 1).min(bytes.len());
                continue;
            }
            b'{' => {
                depth += 1;
                expect_key = depth == 1;
            }
            b'[' => {
                depth += 1;
                expect_key = false;
            }
            b'}' | b']' => {
                depth -= 1;
                expect_key = false;
            }
            b',' if depth == 1 => {
                expect_key = true;
            }
            _ => {}
        }
        i += 1;
    }
}

fn extract_yaml_toplevel_keys(rel_path: &str, text: &str, batch: &mut NormalizedBatch) {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        // Top-level key: no leading whitespace, not a comment, contains ':'.
        if !trimmed.starts_with(' ')
            && !trimmed.starts_with('\t')
            && !trimmed.trim_start().starts_with('#')
            && !trimmed.trim_start().starts_with('-')
        {
            if let Some(colon) = trimmed.find(':') {
                let key = trimmed[..colon].trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
                {
                    let (start_line, start_col) = line_col_at(text, offset);
                    batch.symbols.push(NormalizedSymbol {
                        canonical_symbol_key: format!("yaml-key:{rel_path}#{key}@{offset}"),
                        symbol_kind: "config_key".into(),
                        display_name: key.to_string(),
                        qualified_name: format!("{rel_path}#{key}"),
                        signature: None,
                        start_byte: offset as i64,
                        end_byte: (offset + colon) as i64,
                        start_line,
                        start_column: start_col,
                        end_line: start_line,
                        end_column: start_col + colon as i64,
                        evidence_method: EvidenceMethod::Structural,
                        confidence: 0.8,
                        evidence_reason: "top-level YAML key (no indentation)".into(),
                    });
                }
            }
        }
        offset += line.len();
    }
}

fn extract_toml_top_level_keys(rel_path: &str, text: &str, batch: &mut NormalizedBatch) {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let t = trimmed.trim_start();
        if t.starts_with('[') {
            // Section header, e.g. [dependencies]
            if let Some(end) = t.find(']') {
                let section = t[1..end].trim().to_string();
                let (start_line, start_col) = line_col_at(text, offset);
                batch.symbols.push(NormalizedSymbol {
                    canonical_symbol_key: format!("toml-section:{rel_path}#{section}@{offset}"),
                    symbol_kind: "config_section".into(),
                    display_name: section.clone(),
                    qualified_name: format!("{rel_path}#[{section}]"),
                    signature: None,
                    start_byte: offset as i64,
                    end_byte: (offset + trimmed.len()) as i64,
                    start_line,
                    start_column: start_col,
                    end_line: start_line,
                    end_column: start_col + trimmed.len() as i64,
                    evidence_method: EvidenceMethod::Structural,
                    confidence: 0.9,
                    evidence_reason: "TOML section header".into(),
                });
            }
        } else if !t.starts_with('#') && !t.is_empty() {
            if let Some(eq) = t.find('=') {
                let key = t[..eq].trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
                {
                    let (start_line, start_col) = line_col_at(text, offset);
                    batch.symbols.push(NormalizedSymbol {
                        canonical_symbol_key: format!("toml-key:{rel_path}#{key}@{offset}"),
                        symbol_kind: "config_key".into(),
                        display_name: key.to_string(),
                        qualified_name: format!("{rel_path}#{key}"),
                        signature: None,
                        start_byte: offset as i64,
                        end_byte: (offset + eq) as i64,
                        start_line,
                        start_column: start_col,
                        end_line: start_line,
                        end_column: start_col + eq as i64,
                        evidence_method: EvidenceMethod::Structural,
                        confidence: 0.85,
                        evidence_reason: "TOML key-value".into(),
                    });
                }
            }
        }
        offset += line.len();
    }
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript structural provider (ADR-013r1)
// ---------------------------------------------------------------------------

pub const TYPESCRIPT_PROVIDER: &str = "structural_typescript";
pub const TYPESCRIPT_VERSION: &str = "1.0.0";

/// Structural, not semantic, extraction of function declarations, class
/// declarations, top-level `const`/`let` declarations, and `import`/`export`
/// statements. Uses a line-oriented regex plus brace-depth scan, not an AST.
/// Identical bytes produce identical output (INV-015); evidence is labelled
/// `structural` at confidence 0.7 so it remains distinguishable from semantic
/// provider evidence.
pub fn run_typescript(rel_path: &str, text: &str) -> NormalizedBatch {
    let mut batch = NormalizedBatch::empty(
        TYPESCRIPT_PROVIDER,
        TYPESCRIPT_VERSION,
        ProviderTier::Structural,
    );

    use regex::Regex;
    // Function declarations: `export? function name(...)`; `export? async function name(...)`.
    static FN_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^\s*(export\s+)?(async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(")
            .unwrap()
    });
    // Class declarations.
    static CLASS_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^\s*(export\s+)?(default\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
    });
    // Top-level const/let arrow-function or value declarations.
    static CONST_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^\s*(export\s+)?(const|let)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(:[^=]+)?=")
            .unwrap()
    });
    // Import statements: `import ... from 'module'` or `import 'module'`.
    static IMPORT_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?m)^\s*import\s+(?:[^'"]+\s+from\s+)?['"]([^'"]+)['"]"#).unwrap()
    });

    for cap in FN_RE.captures_iter(text) {
        let m = cap.get(0).unwrap();
        let name = cap.get(3).unwrap().as_str();
        push_fn_symbol(&mut batch, rel_path, text, m.start(), name, "function");
    }
    for cap in CLASS_RE.captures_iter(text) {
        let m = cap.get(0).unwrap();
        let name = cap.get(3).unwrap().as_str();
        push_fn_symbol(&mut batch, rel_path, text, m.start(), name, "class");
    }
    for cap in CONST_RE.captures_iter(text) {
        let m = cap.get(0).unwrap();
        let name = cap.get(3).unwrap().as_str();
        push_fn_symbol(&mut batch, rel_path, text, m.start(), name, "variable");
    }
    for cap in IMPORT_RE.captures_iter(text) {
        let module = cap.get(1).unwrap().as_str();
        batch.relationships.push(NormalizedRelationship {
            relationship_type: "imports".into(),
            source_ref_kind: "path".into(),
            source_ref_value: rel_path.to_string(),
            target_ref_kind: "external".into(),
            target_ref_value: module.to_string(),
            evidence_method: EvidenceMethod::Structural,
            confidence: 0.7,
            evidence_reason: "import statement pattern".into(),
        });
    }

    if batch.symbols.is_empty() && batch.relationships.is_empty() {
        batch
            .diagnostics
            .push("no structural declarations found".into());
    }

    batch
}

/// Compute the span of a declaration starting at `start_byte` by finding the
/// matching closing brace (brace-depth tracking) or, for `variable`
/// declarations without a brace body, the end of the statement (`;` or EOL).
fn push_fn_symbol(
    batch: &mut NormalizedBatch,
    rel_path: &str,
    text: &str,
    start_byte: usize,
    name: &str,
    kind: &str,
) {
    let bytes = text.as_bytes();
    let end_byte = if kind == "variable" {
        // End at the first top-level ';' or newline that isn't inside braces.
        find_statement_end(bytes, start_byte)
    } else {
        find_brace_matched_end(bytes, start_byte).unwrap_or(bytes.len())
    };
    let (start_line, start_col) = line_col_at(text, start_byte);
    let (end_line, end_col) = line_col_at(text, end_byte);
    batch.symbols.push(NormalizedSymbol {
        canonical_symbol_key: format!("ts:{rel_path}::{qualified}", qualified = name),
        symbol_kind: kind.into(),
        display_name: name.to_string(),
        qualified_name: format!("{rel_path}::{name}"),
        signature: None,
        start_byte: start_byte as i64,
        end_byte: end_byte as i64,
        start_line,
        start_column: start_col,
        end_line,
        end_column: end_col,
        evidence_method: EvidenceMethod::Structural,
        confidence: 0.7,
        evidence_reason: format!("{kind} declaration pattern"),
    });
}

fn find_brace_matched_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == q {
                in_string = None;
            }
        } else {
            match c {
                b'"' | b'\'' | b'`' => in_string = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn find_statement_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == q {
                in_string = None;
            }
        } else {
            match c {
                b'"' | b'\'' | b'`' => in_string = Some(c),
                b'(' | b'{' | b'[' => depth += 1,
                b')' | b'}' | b']' => depth -= 1,
                b';' if depth <= 0 => return i + 1,
                b'\n' if depth <= 0 => return i,
                _ => {}
            }
        }
        i += 1;
    }
    bytes.len()
}

// ---------------------------------------------------------------------------
// text_fallback provider
// ---------------------------------------------------------------------------

pub const TEXT_FALLBACK_PROVIDER: &str = "safe_text_fallback";
pub const TEXT_FALLBACK_VERSION: &str = "1.0.0";

/// Lowest-tier provider: never fails, emits no symbols/relationships, only a
/// diagnostic noting the file was seen but not structurally parsed.
pub fn run_text_fallback(rel_path: &str) -> NormalizedBatch {
    let mut batch = NormalizedBatch::empty(
        TEXT_FALLBACK_PROVIDER,
        TEXT_FALLBACK_VERSION,
        ProviderTier::Textual,
    );
    batch.diagnostics.push(format!(
        "{rel_path}: no structural provider available; textual fallback only"
    ));
    batch
}

/// Select the built-in provider for a relative path. Source files use the
/// structural provider; documents/configuration use key or heading extraction;
/// other text receives the safe fallback.
pub fn select_provider_for(rel_path: &str) -> &'static str {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".md")
        || lower.ends_with(".markdown")
        || lower.ends_with(".json")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".toml")
    {
        DOCUMENT_CONFIG_PROVIDER
    } else if lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
    {
        TYPESCRIPT_PROVIDER
    } else {
        TEXT_FALLBACK_PROVIDER
    }
}

// ---------------------------------------------------------------------------
// V1.1 provider-contract wiring (CFG-005) — every V1 builtin provider gets a
// real `provider_contract::ProviderDescriptor` so it is visible to the same
// registry, fingerprinting, and status-reporting machinery as external V1.1
// semantic providers. Builtins never spawn a process: `execution_kind` is
// always `Builtin`, `executable` is always `None`.
// ---------------------------------------------------------------------------

/// Every builtin V1 provider, sealed as a V1.1 `ProviderDescriptor`.
/// Deterministic: calling this twice yields identical fingerprints.
pub fn builtin_provider_descriptors() -> Vec<crate::provider_contract::ProviderDescriptor> {
    use crate::provider_contract::{
        Capability, ExecutionKind, ExecutionScopeKind, InvalidationDefaultScope,
        InvalidationPolicy, OutputFormat, ProviderDescriptor, ProviderUpgradeInvalidation,
        PROTOCOL_VERSION,
    };

    let seal = |name: &str, version: &str, tier: ProviderTier, capabilities: Vec<Capability>| {
        ProviderDescriptor::seal(
            PROTOCOL_VERSION.to_string(),
            name.to_string(),
            version.to_string(),
            tier,
            ExecutionKind::Builtin,
            ExecutionScopeKind::File,
            OutputFormat::Builtin,
            true,
            vec![],
            vec![],
            vec![],
            capabilities,
            vec![],
            InvalidationPolicy {
                default_scope: InvalidationDefaultScope::File,
                provider_upgrade: ProviderUpgradeInvalidation::AffectedScopes,
            },
            None,
            None,
            &serde_json::json!({}),
            "builtin-1.0.0",
            "builtin-1.0.0",
        )
    };

    vec![
        seal(
            FILE_METADATA_PROVIDER,
            FILE_METADATA_VERSION,
            ProviderTier::DocumentConfig,
            vec![Capability::Symbols],
        ),
        seal(
            DOCUMENT_CONFIG_PROVIDER,
            DOCUMENT_CONFIG_VERSION,
            ProviderTier::DocumentConfig,
            vec![Capability::Symbols],
        ),
        seal(
            TYPESCRIPT_PROVIDER,
            TYPESCRIPT_VERSION,
            ProviderTier::Structural,
            vec![
                Capability::Symbols,
                Capability::References,
                Capability::Imports,
                Capability::Exports,
            ],
        ),
        seal(
            TEXT_FALLBACK_PROVIDER,
            TEXT_FALLBACK_VERSION,
            ProviderTier::Textual,
            vec![],
        ),
    ]
}

pub fn scip_provider_capabilities() -> Vec<crate::provider_contract::Capability> {
    use crate::provider_contract::Capability;
    vec![
        Capability::Symbols,
        Capability::Definitions,
        Capability::References,
        Capability::Imports,
        Capability::Implementations,
        Capability::TypeDefinitions,
        Capability::Coverage,
        Capability::Diagnostics,
    ]
}

pub fn scip_provider_limitations() -> Vec<String> {
    vec![
        "dynamic dispatch is not exhaustively resolved".to_string(),
        "call-site classification requires structural composition".to_string(),
        "exports are not distinguished from other definitions by SCIP's SymbolRole bitset"
            .to_string(),
    ]
}

#[allow(clippy::too_many_arguments)]
pub fn external_scip_descriptor(
    name: &str,
    version: &str,
    languages: Vec<String>,
    project_markers: Vec<String>,
    command: &str,
    arguments: Vec<String>,
    executable_hash: Option<&str>,
    configuration: &serde_json::Value,
    mapping_policy_version: &str,
) -> crate::provider_contract::ProviderDescriptor {
    use crate::provider_contract::{
        ExecutableSpec, ExecutionKind, ExecutionScopeKind, InvalidationDefaultScope,
        InvalidationPolicy, OutputFormat, ProviderDescriptor, ProviderUpgradeInvalidation,
        PROTOCOL_VERSION,
    };
    ProviderDescriptor::seal(
        PROTOCOL_VERSION.to_string(),
        name.to_string(),
        version.to_string(),
        ProviderTier::SemanticIndex,
        ExecutionKind::ExternalProcess,
        ExecutionScopeKind::Project,
        OutputFormat::Scip,
        true,
        languages,
        vec!["source".to_string(), "test".to_string()],
        project_markers,
        scip_provider_capabilities(),
        scip_provider_limitations(),
        InvalidationPolicy {
            default_scope: InvalidationDefaultScope::Project,
            provider_upgrade: ProviderUpgradeInvalidation::AllProviderOutput,
        },
        Some(ExecutableSpec {
            command: command.to_string(),
            arguments,
            expected_output: Some("index.scip".to_string()),
        }),
        executable_hash,
        configuration,
        "scip-decoder-1.0.0",
        mapping_policy_version,
    )
}

pub fn scip_typescript_descriptor(
    executable_hash: Option<&str>,
    configuration: &serde_json::Value,
) -> crate::provider_contract::ProviderDescriptor {
    external_scip_descriptor(
        "scip-typescript",
        "0.4.0",
        vec!["typescript".to_string(), "javascript".to_string()],
        vec![
            "tsconfig.json".to_string(),
            "jsconfig.json".to_string(),
            "package.json".to_string(),
        ],
        "scip-typescript",
        vec![
            "index".to_string(),
            "--output".to_string(),
            "{output_file}".to_string(),
        ],
        executable_hash,
        configuration,
        "scip-typescript-mapping-1.0.0",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typescript_finds_exported_function() {
        let src = r#"
/** doc */
export function scheduleReconnect(policy: RetryPolicy): TimerHandle {
  if (policy.maxAttempts < 1) {
    throw new Error('x');
  }
  return handle;
}
"#;
        let batch = run_typescript("src/x.ts", src);
        let found = batch
            .symbols
            .iter()
            .find(|s| s.display_name == "scheduleReconnect");
        assert!(
            found.is_some(),
            "expected scheduleReconnect symbol, got {:?}",
            batch.symbols
        );
        let sym = found.unwrap();
        assert_eq!(sym.symbol_kind, "function");
        // Verify the span covers the full function body (contains the closing brace).
        let span_text = &src[sym.start_byte as usize..sym.end_byte as usize];
        assert!(span_text.starts_with("export function scheduleReconnect"));
        assert!(span_text.trim_end().ends_with('}'));
    }

    #[test]
    fn typescript_finds_class_and_import() {
        let src = "import { Foo } from './foo';\nexport class Bar {\n  x = 1;\n}\n";
        let batch = run_typescript("src/y.ts", src);
        assert!(batch
            .symbols
            .iter()
            .any(|s| s.display_name == "Bar" && s.symbol_kind == "class"));
        assert!(batch
            .relationships
            .iter()
            .any(|r| r.relationship_type == "imports" && r.target_ref_value == "./foo"));
    }

    #[test]
    fn typescript_deterministic_on_identical_bytes() {
        let src = "export function a() { return 1; }\n";
        let b1 = run_typescript("f.ts", src);
        let b2 = run_typescript("f.ts", src);
        assert_eq!(b1.symbols.len(), b2.symbols.len());
        assert_eq!(
            b1.symbols[0].canonical_symbol_key,
            b2.symbols[0].canonical_symbol_key
        );
        assert_eq!(b1.symbols[0].start_byte, b2.symbols[0].start_byte);
        assert_eq!(b1.symbols[0].end_byte, b2.symbols[0].end_byte);
    }

    #[test]
    fn document_config_extracts_markdown_headings() {
        let src = "# Title\n\nSome text.\n\n## Subheading\n";
        let batch = run_document_config("README.md", src);
        let names: Vec<&str> = batch
            .symbols
            .iter()
            .map(|s| s.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Title", "Subheading"]);
    }

    #[test]
    fn document_config_extracts_json_top_level_keys() {
        let src = r#"{"name": "x", "version": "1.0.0", "nested": {"a": 1}}"#;
        let batch = run_document_config("package.json", src);
        let names: Vec<&str> = batch
            .symbols
            .iter()
            .map(|s| s.display_name.as_str())
            .collect();
        assert!(names.contains(&"name"));
        assert!(names.contains(&"version"));
        assert!(names.contains(&"nested"));
        assert!(!names.contains(&"a")); // nested key must not appear at top level
    }

    #[test]
    fn document_config_extracts_toml_sections_and_keys() {
        let src = "[package]\nname = \"x\"\nversion = \"1.0\"\n\n[dependencies]\nserde = \"1\"\n";
        let batch = run_document_config("Cargo.toml", src);
        let names: Vec<&str> = batch
            .symbols
            .iter()
            .map(|s| s.display_name.as_str())
            .collect();
        assert!(names.contains(&"package"));
        assert!(names.contains(&"dependencies"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"serde"));
    }

    #[test]
    fn select_provider_routes_by_extension() {
        assert_eq!(select_provider_for("a.ts"), TYPESCRIPT_PROVIDER);
        assert_eq!(select_provider_for("a.md"), DOCUMENT_CONFIG_PROVIDER);
        assert_eq!(select_provider_for("a.png"), TEXT_FALLBACK_PROVIDER);
    }

    #[test]
    fn text_fallback_never_fails() {
        let batch = run_text_fallback("a.bin");
        assert!(batch.symbols.is_empty());
        assert_eq!(batch.provider_tier, ProviderTier::Textual);
    }

    #[test]
    fn builtin_descriptors_cover_all_four_v1_providers_as_builtin_execution() {
        let descriptors = builtin_provider_descriptors();
        assert_eq!(descriptors.len(), 4);
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&FILE_METADATA_PROVIDER));
        assert!(names.contains(&DOCUMENT_CONFIG_PROVIDER));
        assert!(names.contains(&TYPESCRIPT_PROVIDER));
        assert!(names.contains(&TEXT_FALLBACK_PROVIDER));
        for d in &descriptors {
            assert_eq!(
                d.execution_kind,
                crate::provider_contract::ExecutionKind::Builtin
            );
            assert!(
                d.executable.is_none(),
                "{} must never spawn a process",
                d.name
            );
            assert_eq!(d.fingerprint.len(), 64);
        }
    }

    #[test]
    fn builtin_descriptors_are_deterministic() {
        let a = builtin_provider_descriptors();
        let b = builtin_provider_descriptors();
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.fingerprint, y.fingerprint);
        }
    }

    #[test]
    fn scip_typescript_descriptor_is_external_project_scoped_semantic() {
        let d = scip_typescript_descriptor(None, &serde_json::json!({}));
        assert_eq!(d.name, "scip-typescript");
        assert_eq!(d.version, "0.4.0");
        assert_eq!(d.tier, ProviderTier::SemanticIndex);
        assert_eq!(
            d.execution_kind,
            crate::provider_contract::ExecutionKind::ExternalProcess
        );
        assert_eq!(
            d.execution_scope,
            crate::provider_contract::ExecutionScopeKind::Project
        );
        assert!(d.executable.is_some());
        assert_eq!(d.fingerprint.len(), 64);
    }

    #[test]
    fn external_scip_descriptor_preserves_rust_provider_identity() {
        let d = external_scip_descriptor(
            "rust-analyzer",
            "1.94.1",
            vec!["rust".to_string()],
            vec!["Cargo.toml".to_string()],
            "rust-analyzer",
            vec![
                "scip".to_string(),
                ".".to_string(),
                "--output".to_string(),
                "{output_file}".to_string(),
            ],
            Some(&"a".repeat(64)),
            &serde_json::json!({}),
            "rust-analyzer-scip-mapping-1.0.0",
        );
        assert_eq!(d.name, "rust-analyzer");
        assert_eq!(d.version, "1.94.1");
        assert_eq!(d.languages, vec!["rust"]);
        assert_eq!(d.project_markers, vec!["Cargo.toml"]);
        assert!(!d.provider_key().contains("scip-typescript"));
    }

    #[test]
    fn scip_typescript_descriptor_fingerprint_changes_with_executable_hash() {
        let a = scip_typescript_descriptor(None, &serde_json::json!({}));
        let b = scip_typescript_descriptor(Some(&"a".repeat(64)), &serde_json::json!({}));
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    /// Real end-to-end proof that the pinned provider is actually installed
    /// and reachable on this development machine, tying `probe_provider`
    /// (S3) to `scip_typescript_descriptor` (S2/S4): CFG-004 "TypeScript
    /// plan includes structural+semantic" at execution time, not only in
    /// configuration.
    #[test]
    fn scip_typescript_is_actually_installed_and_probes_available() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::provider_runtime::build_environment(
            &[
                "PATH".to_string(),
                "PATHEXT".to_string(),
                "SYSTEMROOT".to_string(),
            ],
            false,
        );
        let probe = crate::provider_runtime::probe_provider(
            "scip-typescript",
            "scip-typescript",
            &["--version".to_string()],
            dir.path(),
            env,
            std::time::Duration::from_secs(15),
        );
        assert_eq!(
            probe.status,
            crate::provider_contract::ProbeStatus::Available,
            "expected the operator-installed scip-typescript@0.4.0 to probe as available: {probe:?}"
        );
        let sealed =
            scip_typescript_descriptor(probe.executable_hash.as_deref(), &serde_json::json!({}));
        assert_eq!(sealed.fingerprint.len(), 64);
    }
}
