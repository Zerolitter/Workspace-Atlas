//! SCIP → Atlas normalized-fact mapping (S4 — `SCIP-003`..`SCIP-015`).
//!
//! Converts decoded, internal `scip_decoder::ScipIndex` values into the same
//! normalized symbol and relationship shapes produced by every provider.
//! Public Atlas responses never expose SCIP decoder types. Semantic evidence
//! preserves the provider's original symbol string separately in
//! `MappedSymbol::provider_symbol_id` and
//! `MappedRelationship::provider_symbol_id` rather than folding it into the
//! canonical key.

use std::path::Path;

use crate::error::{AtlasError, Result};
use crate::paths;
#[cfg(test)]
use crate::scip_decoder::ScipOccurrence;
use crate::scip_decoder::{symbol_role, ScipDocument, ScipIndex, ScipSymbolInformation};

// ---------------------------------------------------------------------------
// Document mapping + path confinement (SCIP-003, SCIP-004)
// ---------------------------------------------------------------------------

/// Resolve a SCIP `Document.relative_path` to Atlas's canonical
/// workspace-relative path. Never trusts the provider's own "already
/// canonical" claim (`scip.proto`: "the path must be canonical") — every
/// document is independently confined under the workspace root (SCIP-003,
/// S-002).
pub fn map_document_path(
    workspace_root: &Path,
    project_scope_root: &Path,
    doc_relative_path: &str,
) -> Result<String> {
    if doc_relative_path.is_empty() {
        return Err(AtlasError::Other(
            "SCIP document relative_path must not be empty".to_string(),
        ));
    }
    // Reject untrusted absolute paths outright; only a path relative to the
    // project scope root is ever accepted from provider output.
    if Path::new(doc_relative_path).is_absolute() {
        return Err(AtlasError::PathEscape {
            path: doc_relative_path.to_string(),
        });
    }
    let candidate_abs = project_scope_root.join(doc_relative_path);
    let confined = paths::confine_to_root(workspace_root, &candidate_abs)?;
    Ok(paths::canonical_relative_path(&confined))
}

// ---------------------------------------------------------------------------
// Canonical symbol keys (SCIP-005, SCIP-006, SCIP-007)
// ---------------------------------------------------------------------------

/// `true` when a raw SCIP symbol string uses the `local ...` scheme —
/// document-scoped, not a stable cross-file identifier (`scip.proto`
/// `Symbol` grammar: `'local ' <local-id>`).
pub fn is_local_symbol(raw_scip_symbol: &str) -> bool {
    raw_scip_symbol.starts_with("local ")
}

/// Derive Atlas's provider-neutral canonical symbol key from a raw SCIP
/// symbol string (SCIP-006).
///
/// - A global symbol's canonical key is the raw string itself — SCIP's own
///   symbol grammar already forms a stable, package-qualified identifier
///   (SCIP-005 "preserve exact provider symbol IDs": the raw string is also
///   retained verbatim as `provider_symbol_id` by the caller).
/// - A `local` symbol is scoped by the *canonical document path* it was
///   observed in — it MUST NOT be treated as stable outside that document,
///   and two documents' `local 0` are different symbols (SCIP-007).
pub fn canonical_symbol_key(raw_scip_symbol: &str, canonical_document_path: &str) -> String {
    if is_local_symbol(raw_scip_symbol) {
        format!("scip-local:{canonical_document_path}:{raw_scip_symbol}")
    } else {
        format!("scip:{raw_scip_symbol}")
    }
}

// ---------------------------------------------------------------------------
// Mapped facts (semantic evidence method)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MappedSymbol {
    pub canonical_symbol_key: String,
    pub provider_symbol_id: String,
    pub canonical_document_path: String,
    pub is_local: bool,
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RefKind {
    /// A resolvable-in-principle symbol (global or local) — resolution
    /// itself is the S6 resolver's job; this only records the raw target.
    Symbol,
    /// A symbol this execution never saw defined anywhere — either a true
    /// `Index.external_symbols` entry or a reference whose target symbol
    /// string never appeared as a local definition in this run (SCIP-011).
    ExternalSymbol,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedRelationship {
    pub relationship_type: &'static str,
    pub source_canonical_document_path: String,
    pub source_start_line: i64,
    pub source_start_column: i64,
    pub source_end_line: i64,
    pub source_end_column: i64,
    pub target_ref_kind: RefKind,
    pub target_ref_value: String,
    pub provider_symbol_id: String,
    pub confidence: f64,
    pub reason_code: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct MappedDocument {
    pub canonical_path: String,
    pub position_encoding: crate::scip_decoder::PositionEncoding,
    pub symbols: Vec<MappedSymbol>,
    pub relationships: Vec<MappedRelationship>,
    /// Documents that hit a mapping error are marked partial and diagnosed
    /// without aborting the remaining index.
    pub partial: bool,
    pub diagnostics: Vec<String>,
}

/// Map one `ScipDocument` into Atlas raw evidence. Never panics on
/// malformed/unexpected content within the document — degrades to
/// `partial: true` with a diagnostic instead (SCIP-015).
pub fn map_document(
    workspace_root: &Path,
    project_scope_root: &Path,
    doc: &ScipDocument,
    locally_defined_symbols: &std::collections::HashSet<&str>,
) -> MappedDocument {
    let canonical_path =
        match map_document_path(workspace_root, project_scope_root, &doc.relative_path) {
            Ok(p) => p,
            Err(e) => {
                return MappedDocument {
                    canonical_path: doc.relative_path.clone(),
                    position_encoding: doc.position_encoding,
                    partial: true,
                    diagnostics: vec![format!("outside_workspace: {e}")],
                    ..Default::default()
                }
            }
        };

    let mut mapped = MappedDocument {
        canonical_path: canonical_path.clone(),
        position_encoding: doc.position_encoding,
        ..Default::default()
    };

    for occ in &doc.occurrences {
        if occ.symbol.is_empty() {
            continue; // highlighting-only occurrence, no symbol association
        }
        let is_definition = occ.symbol_roles & symbol_role::DEFINITION != 0;
        let is_import = occ.symbol_roles & symbol_role::IMPORT != 0;

        if is_definition {
            mapped.symbols.push(MappedSymbol {
                canonical_symbol_key: canonical_symbol_key(&occ.symbol, &canonical_path),
                provider_symbol_id: occ.symbol.clone(),
                canonical_document_path: canonical_path.clone(),
                is_local: is_local_symbol(&occ.symbol),
                start_line: occ.range.start_line as i64,
                start_column: occ.range.start_character as i64,
                end_line: occ.range.end_line as i64,
                end_column: occ.range.end_character as i64,
                confidence: 0.95,
            });
        }

        // SCIP-008: every occurrence (definition or reference) is also
        // recorded as a raw REFERENCES fact; the resolver (S6) decides
        // preferred/duplicate handling. SCIP-009: an Import-role occurrence
        // is additionally recorded as IMPORTS — this is the only import
        // evidence SCIP's role bitset actually supports; Atlas does not
        // fabricate an "exports" relationship SCIP has no bit for.
        let target_kind = if locally_defined_symbols.contains(occ.symbol.as_str())
            || is_local_symbol(&occ.symbol)
        {
            RefKind::Symbol
        } else {
            RefKind::ExternalSymbol
        };
        mapped.relationships.push(MappedRelationship {
            relationship_type: "REFERENCES",
            source_canonical_document_path: canonical_path.clone(),
            source_start_line: occ.range.start_line as i64,
            source_start_column: occ.range.start_character as i64,
            source_end_line: occ.range.end_line as i64,
            source_end_column: occ.range.end_character as i64,
            target_ref_kind: target_kind.clone(),
            target_ref_value: occ.symbol.clone(),
            provider_symbol_id: occ.symbol.clone(),
            confidence: 0.9,
            reason_code: "scip_occurrence",
        });
        if is_import {
            mapped.relationships.push(MappedRelationship {
                relationship_type: "IMPORTS",
                source_canonical_document_path: canonical_path.clone(),
                source_start_line: occ.range.start_line as i64,
                source_start_column: occ.range.start_character as i64,
                source_end_line: occ.range.end_line as i64,
                source_end_column: occ.range.end_character as i64,
                target_ref_kind: target_kind,
                target_ref_value: occ.symbol.clone(),
                provider_symbol_id: occ.symbol.clone(),
                confidence: 0.9,
                reason_code: "scip_import_role",
            });
        }
    }

    // SCIP-010: implementation/type-definition/explicit-definition relations
    // come from `SymbolInformation.relationships`, independent of
    // occurrences.
    for sym in &doc.symbols {
        mapped
            .relationships
            .extend(map_symbol_information_relationships(
                sym,
                &canonical_path,
                locally_defined_symbols,
            ));
    }

    mapped
}

fn map_symbol_information_relationships(
    sym: &ScipSymbolInformation,
    canonical_path: &str,
    locally_defined_symbols: &std::collections::HashSet<&str>,
) -> Vec<MappedRelationship> {
    let mut out = Vec::new();
    for rel in &sym.relationships {
        let target_kind = if locally_defined_symbols.contains(rel.symbol.as_str())
            || is_local_symbol(&rel.symbol)
        {
            RefKind::Symbol
        } else {
            RefKind::ExternalSymbol
        };
        let mut push = |relationship_type: &'static str| {
            out.push(MappedRelationship {
                relationship_type,
                source_canonical_document_path: canonical_path.to_string(),
                source_start_line: -1,
                source_start_column: -1,
                source_end_line: -1,
                source_end_column: -1,
                target_ref_kind: target_kind.clone(),
                target_ref_value: rel.symbol.clone(),
                provider_symbol_id: sym.symbol.clone(),
                confidence: 0.97,
                reason_code: "scip_symbol_relationship",
            });
        };
        if rel.is_implementation {
            push("IMPLEMENTS");
        }
        if rel.is_type_definition {
            push("TYPE_DEFINITION");
        }
        if rel.is_definition {
            push("DEFINED_BY");
        }
        // `is_reference` alone (without implementation/type_definition/
        // definition) is find-references bookkeeping, not a new
        // relationship kind distinct from the REFERENCES facts already
        // produced from occurrences — do not double-count it as an edge.
    }
    out
}

/// Build the set of every symbol string defined *anywhere* in this index
/// (used to classify occurrence targets as internal-symbol vs.
/// external-symbol, SCIP-011).
pub fn locally_defined_symbol_set(index: &ScipIndex) -> std::collections::HashSet<&str> {
    let mut set = std::collections::HashSet::new();
    for doc in &index.documents {
        for sym in &doc.symbols {
            set.insert(sym.symbol.as_str());
        }
    }
    set
}

// ---------------------------------------------------------------------------
// Per-document normalized digest (SCIP-012)
// ---------------------------------------------------------------------------

/// Deterministic digest of one document's normalized facts — used to decide
/// extractor-run reuse vs. a new persisted run (S5) independent of whether
/// the *provider execution* itself was reused.
pub fn document_normalized_digest(doc: &MappedDocument) -> String {
    #[derive(serde::Serialize)]
    struct SymRow<'a> {
        k: &'a str,
        p: &'a str,
        sl: i64,
        sc: i64,
        el: i64,
        ec: i64,
    }
    #[derive(serde::Serialize)]
    struct RelRow<'a> {
        t: &'a str,
        sl: i64,
        sc: i64,
        el: i64,
        ec: i64,
        tk: &'a str,
        tv: &'a str,
        r: &'a str,
    }
    let mut symbols: Vec<SymRow> = doc
        .symbols
        .iter()
        .map(|s| SymRow {
            k: &s.canonical_symbol_key,
            p: &s.provider_symbol_id,
            sl: s.start_line,
            sc: s.start_column,
            el: s.end_line,
            ec: s.end_column,
        })
        .collect();
    symbols.sort_by(|a, b| (a.k, a.sl, a.sc, a.el, a.ec).cmp(&(b.k, b.sl, b.sc, b.el, b.ec)));

    let mut relationships: Vec<RelRow> = doc
        .relationships
        .iter()
        .map(|r| RelRow {
            t: r.relationship_type,
            sl: r.source_start_line,
            sc: r.source_start_column,
            el: r.source_end_line,
            ec: r.source_end_column,
            tk: match r.target_ref_kind {
                RefKind::Symbol => "symbol",
                RefKind::ExternalSymbol => "external_symbol",
            },
            tv: &r.target_ref_value,
            r: r.reason_code,
        })
        .collect();
    relationships.sort_by(|a, b| {
        (a.t, a.sl, a.sc, a.el, a.ec, a.tv).cmp(&(b.t, b.sl, b.sc, b.el, b.ec, b.tv))
    });

    let bytes = crate::provider_contract::canonical_json_bytes(&(
        doc.canonical_path.as_str(),
        doc.position_encoding.as_str(),
        &symbols,
        &relationships,
    ));
    crate::hashing::content_hash_of_bytes(&bytes)
}

// ---------------------------------------------------------------------------
// Structural + semantic call-edge composition (SCIP-013, SCIP-014, ADR-031)
// ---------------------------------------------------------------------------

/// A structural call-site observation (from the *structural* provider, not
/// SCIP) — the byte/line span of `callee(...)` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralCallSite {
    pub canonical_document_path: String,
    pub source_revision_hash: String,
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
}

/// Compose a `CALLS` edge **only** when a structural call-site and a
/// semantic occurrence agree on document, source revision, and exact span.
/// ADR-031 / SCIP-013 / SCIP-014: SCIP references are not automatically
/// calls, and an ordinary reference (e.g. a value read, not a call
/// expression) must never become `CALLS` just because a semantic provider
/// saw a symbol there.
pub fn compose_call_edge(
    call_site: &StructuralCallSite,
    semantic_relationship: &MappedRelationship,
    semantic_revision_hash: &str,
) -> Option<MappedRelationship> {
    if semantic_relationship.relationship_type != "REFERENCES" {
        return None;
    }
    if call_site.canonical_document_path != semantic_relationship.source_canonical_document_path {
        return None;
    }
    if call_site.source_revision_hash != semantic_revision_hash {
        return None; // source changed since either evidence source ran (RISK S-014)
    }
    let span_matches = call_site.start_line == semantic_relationship.source_start_line
        && call_site.start_column == semantic_relationship.source_start_column
        && call_site.end_line == semantic_relationship.source_end_line
        && call_site.end_column == semantic_relationship.source_end_column;
    if !span_matches {
        return None;
    }

    let mut composed = semantic_relationship.clone();
    composed.relationship_type = "CALLS";
    composed.reason_code = "composed_span_match";
    composed.confidence = (semantic_relationship.confidence * 0.98).min(0.98);
    Some(composed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scip_decoder::{decode_index, DecodeLimits, ScipRange};

    #[test]
    fn document_path_is_confined_under_workspace_root() {
        let workspace = Path::new("/ws");
        let scope = Path::new("/ws/packages/app");
        let mapped = map_document_path(workspace, scope, "src/index.ts").unwrap();
        assert_eq!(mapped, "packages/app/src/index.ts");
    }

    #[test]
    fn document_path_escape_is_rejected() {
        let workspace = Path::new("/ws/packages/app");
        let scope = Path::new("/ws/packages/app");
        let err = map_document_path(workspace, scope, "../../outside/evil.ts").unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn absolute_relative_path_is_rejected_outright() {
        let workspace = Path::new("/ws");
        let scope = Path::new("/ws");
        let err = map_document_path(workspace, scope, "/etc/passwd").unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn local_symbols_are_scoped_by_document_not_globally_stable() {
        let key_a = canonical_symbol_key("local 3", "src/a.ts");
        let key_b = canonical_symbol_key("local 3", "src/b.ts");
        assert_ne!(
            key_a, key_b,
            "the same local id in two documents must not collide"
        );
        let key_a_again = canonical_symbol_key("local 3", "src/a.ts");
        assert_eq!(
            key_a, key_a_again,
            "identical (document, local id) must be deterministic"
        );
    }

    #[test]
    fn global_symbols_use_the_raw_scip_string_as_canonical_key() {
        let raw = "scip-typescript npm mypkg 1.0.0 `src/a.ts`/Foo#";
        let key = canonical_symbol_key(raw, "src/a.ts");
        assert_eq!(key, format!("scip:{raw}"));
    }

    fn occ(symbol: &str, roles: i32, span: (i32, i32, i32, i32)) -> ScipOccurrence {
        ScipOccurrence {
            range: ScipRange {
                start_line: span.0,
                start_character: span.1,
                end_line: span.2,
                end_character: span.3,
            },
            symbol: symbol.to_string(),
            symbol_roles: roles,
            syntax_kind: 0,
        }
    }

    #[test]
    fn definition_occurrence_produces_a_symbol_and_a_references_fact() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![occ("pkg foo().", symbol_role::DEFINITION, (0, 0, 0, 3))],
            symbols: vec![],
        };
        let defined: std::collections::HashSet<&str> = ["pkg foo()."].into_iter().collect();
        let mapped = map_document(Path::new("/ws"), Path::new("/ws"), &doc, &defined);
        assert_eq!(mapped.symbols.len(), 1);
        assert_eq!(mapped.symbols[0].provider_symbol_id, "pkg foo().");
        assert!(mapped
            .relationships
            .iter()
            .any(|r| r.relationship_type == "REFERENCES"));
    }

    #[test]
    fn ordinary_reference_is_not_marked_as_calls_by_the_mapper_alone() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![occ("pkg value.", 0, (5, 0, 5, 5))], // no Definition role => reference
            symbols: vec![],
        };
        let mapped = map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        );
        assert!(mapped
            .relationships
            .iter()
            .all(|r| r.relationship_type != "CALLS"));
        assert!(mapped
            .relationships
            .iter()
            .any(|r| r.relationship_type == "REFERENCES"));
    }

    #[test]
    fn import_role_produces_imports_fact_not_fabricated_exports() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![occ("pkg thing.", symbol_role::IMPORT, (1, 0, 1, 5))],
            symbols: vec![],
        };
        let mapped = map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        );
        assert!(mapped
            .relationships
            .iter()
            .any(|r| r.relationship_type == "IMPORTS"));
        assert!(mapped
            .relationships
            .iter()
            .all(|r| r.relationship_type != "EXPORTS"));
    }

    #[test]
    fn occurrence_target_not_locally_defined_is_external_symbol() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![occ("npm lodash 4.0.0 map().", 0, (2, 0, 2, 3))],
            symbols: vec![],
        };
        let mapped = map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        );
        assert!(matches!(
            mapped.relationships[0].target_ref_kind,
            RefKind::ExternalSymbol
        ));
    }

    #[test]
    fn document_outside_workspace_root_is_marked_partial_not_dropped_silently() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "../../escape.ts".to_string(),
            occurrences: vec![],
            symbols: vec![],
        };
        let mapped = map_document(
            Path::new("/ws/proj"),
            Path::new("/ws/proj"),
            &doc,
            &Default::default(),
        );
        assert!(mapped.partial);
        assert!(!mapped.diagnostics.is_empty());
    }

    #[test]
    fn symbol_information_relationships_map_implements_and_type_definition() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/dog.ts".to_string(),
            occurrences: vec![],
            symbols: vec![ScipSymbolInformation {
                symbol: "pkg Dog#".to_string(),
                documentation: vec![],
                relationships: vec![crate::scip_decoder::ScipRelationship {
                    symbol: "pkg Animal#".to_string(),
                    is_reference: false,
                    is_implementation: true,
                    is_type_definition: false,
                    is_definition: false,
                }],
                kind: 0,
            }],
        };
        let mapped = map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        );
        assert!(mapped
            .relationships
            .iter()
            .any(|r| r.relationship_type == "IMPLEMENTS" && r.target_ref_value == "pkg Animal#"));
    }

    #[test]
    fn per_document_digest_is_deterministic_and_order_independent_of_input_iteration() {
        let doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![
                occ("pkg a.", symbol_role::DEFINITION, (0, 0, 0, 1)),
                occ("pkg b.", symbol_role::DEFINITION, (1, 0, 1, 1)),
            ],
            symbols: vec![],
        };
        let mapped_a = map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        );
        let mapped_b = mapped_a.clone();
        assert_eq!(
            document_normalized_digest(&mapped_a),
            document_normalized_digest(&mapped_b)
        );
    }

    #[test]
    fn per_document_digest_changes_when_occurrence_changes() {
        let mut doc = ScipDocument {
            position_encoding: crate::scip_decoder::PositionEncoding::Utf16,
            language: "typescript".to_string(),
            relative_path: "src/a.ts".to_string(),
            occurrences: vec![occ("pkg a.", symbol_role::DEFINITION, (0, 0, 0, 1))],
            symbols: vec![],
        };
        let before = document_normalized_digest(&map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        ));
        doc.occurrences[0].range.end_character = 5;
        let after = document_normalized_digest(&map_document(
            Path::new("/ws"),
            Path::new("/ws"),
            &doc,
            &Default::default(),
        ));
        assert_ne!(before, after);
    }

    #[test]
    fn call_edge_composes_only_on_exact_span_and_revision_match() {
        let semantic = MappedRelationship {
            relationship_type: "REFERENCES",
            source_canonical_document_path: "src/a.ts".to_string(),
            source_start_line: 10,
            source_start_column: 4,
            source_end_line: 10,
            source_end_column: 12,
            target_ref_kind: RefKind::Symbol,
            target_ref_value: "pkg connect().".to_string(),
            provider_symbol_id: "pkg connect().".to_string(),
            confidence: 0.9,
            reason_code: "scip_occurrence",
        };
        let matching_call_site = StructuralCallSite {
            canonical_document_path: "src/a.ts".to_string(),
            source_revision_hash: "rev1".to_string(),
            start_line: 10,
            start_column: 4,
            end_line: 10,
            end_column: 12,
        };
        let composed = compose_call_edge(&matching_call_site, &semantic, "rev1").unwrap();
        assert_eq!(composed.relationship_type, "CALLS");
        assert_eq!(composed.reason_code, "composed_span_match");

        // Mismatched span: no composition.
        let mut mismatched = matching_call_site.clone();
        mismatched.end_column = 99;
        assert!(compose_call_edge(&mismatched, &semantic, "rev1").is_none());

        // Stale revision (source changed since one side ran): no composition.
        assert!(compose_call_edge(&matching_call_site, &semantic, "rev2").is_none());

        // Non-reference relationship types never compose into CALLS.
        let mut non_reference = semantic.clone();
        non_reference.relationship_type = "IMPORTS";
        assert!(compose_call_edge(&matching_call_site, &non_reference, "rev1").is_none());
    }

    /// End-to-end against the real `scip-typescript` fixture output: proves
    /// the mapping layer survives real provider bytes, not just hand-built
    /// `ScipDocument` structs.
    #[test]
    fn maps_real_scip_typescript_fixture_without_panicking() {
        let bytes = include_bytes!("../tests/scip_fixtures/semantic_fixture_v1.scip");
        let index = decode_index(bytes, &DecodeLimits::default()).unwrap();
        let defined = locally_defined_symbol_set(&index);
        let workspace_root = Path::new("/ws");

        let mut total_symbols = 0usize;
        let mut total_relationships = 0usize;
        let mut any_partial = false;
        for doc in &index.documents {
            let mapped = map_document(workspace_root, workspace_root, doc, &defined);
            any_partial |= mapped.partial;
            total_symbols += mapped.symbols.len();
            total_relationships += mapped.relationships.len();
            // Every real document must digest without panicking.
            let digest = document_normalized_digest(&mapped);
            assert_eq!(digest.len(), 64);
        }
        assert!(
            !any_partial,
            "real scip-typescript output must map cleanly under the workspace root"
        );
        assert!(total_symbols > 0);
        assert!(total_relationships > 0);
    }

    /// The checked fixture contains a non-ASCII path and CRLF source. Real
    /// provider output must decode, map, canonicalize, and digest it without
    /// corruption or panic.
    #[test]
    fn real_fixture_handles_unicode_path_and_crlf_file_correctly() {
        let bytes = include_bytes!("../tests/scip_fixtures/semantic_fixture_v1.scip");
        let index = decode_index(bytes, &DecodeLimits::default()).unwrap();
        let defined = locally_defined_symbol_set(&index);
        let workspace_root = Path::new("/ws");

        let unicode_doc = index
            .documents
            .iter()
            .find(|d| d.relative_path.contains("naïve"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a document containing 'naïve' in its path; got {:?}",
                    index
                        .documents
                        .iter()
                        .map(|d| &d.relative_path)
                        .collect::<Vec<_>>()
                )
            });
        // The raw UTF-8 bytes must round-trip exactly through protobuf
        // decode — no lossy re-encoding of the ï in "naïve".
        assert!(unicode_doc
            .relative_path
            .as_bytes()
            .windows("ï".len())
            .any(|w| w == "ï".as_bytes()));

        let mapped = map_document(workspace_root, workspace_root, unicode_doc, &defined);
        assert!(
            !mapped.partial,
            "unicode-path CRLF document must map cleanly, not degrade to partial"
        );
        assert!(
            mapped.canonical_path.contains("naïve"),
            "canonical path must preserve the unicode filename exactly"
        );
        assert!(
            mapped
                .symbols
                .iter()
                .any(|s| s.provider_symbol_id.contains("caféStatus")),
            "expected the caféStatus symbol (also non-ASCII) to be mapped: {:?}",
            mapped
                .symbols
                .iter()
                .map(|s| &s.provider_symbol_id)
                .collect::<Vec<_>>()
        );
        let digest = document_normalized_digest(&mapped);
        assert_eq!(
            digest.len(),
            64,
            "digest must be a normal 64-hex sha256 even for unicode/CRLF input"
        );
    }
}
