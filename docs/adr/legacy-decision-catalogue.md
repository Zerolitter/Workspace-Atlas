# Referenced implementation decisions

**Status:** Accepted

This catalogue preserves the durable contracts behind ADR identifiers retained
in source comments. Detailed user-facing contracts with operational impact have
their own ADR files; these entries describe bounded implementation decisions.

## ADR-010: Deterministic Context Broker ranking

Broker scoring uses fixed, inspectable rules. It does not use a model, learned
weights, or hidden ranking state.

## ADR-013r1: Initial structural source provider

The built-in TypeScript/JavaScript provider uses deterministic structural
matching and brace-depth tracking. Its evidence is labelled `structural`, never
semantic. Stronger type-aware evidence is supplied separately through SCIP.

## ADR-016: Discovery path classification

Discovery applies explicit secret-bearing and vendor/build/generated path
patterns before indexing. Paths remain confined to the canonical workspace;
symlinks are rejected by default. Atlas's own local catalogue directories are
immutable exclusions.

## ADR-024: Project-scoped provider execution

A project provider execution is keyed by provider, scope, input fingerprint,
protocol, schema, mapping policy, and configuration—not by an individual file.
Multiple changed files in one scope produce one execution with a per-file input
manifest.

## ADR-025: Direct external-provider spawn

External providers are invoked directly with an argument vector. Shell
execution is forbidden by configuration validation.

## ADR-026: Operator-managed provider installation

Atlas probes explicitly configured executables but never invokes a package
manager or installer. Provider installation and trust remain operator choices.

## ADR-031: Structural and semantic call composition

A semantic reference is not automatically a call. `CALLS` evidence requires a
structural call-site and semantic reference to agree on document, source
revision, and exact span.

## ADR-032: Provider failure and activation

An optional provider failure degrades coverage while allowing activation. A
required provider failure blocks candidate activation and leaves the previous
active generation unchanged.

## ADR-038: Non-conflated execution metrics

Core changed files, provider executions, emitted documents, resolutions, and
conflicts are separate metric fields. They must not be collapsed into one
ambiguous processed count.

## ADR-P005: Serving projection policy

Serving builds are transactional and deterministic. One preferred fact is
projected per canonical symbol; file coverage is inherited rather than
fabricated per symbol. Relationship trust classes derive from resolution state,
evidence method, and confidence. Missing resolution remains `unresolved`.

## ADR-P007: Deterministic task compilation policy

Context compilation uses a fixed, versioned recipe for every public task kind.
Declared kinds take precedence; fallback classification uses ordered
whole-word rules with stable rule IDs and reports `unknown` when no rule
matches. Recipes independently gate caller, callee, test, and configuration
evidence and cap graph depth. Indexed artifact classification labels and gates
test/configuration neighbors even when the provider emitted a generic import or
reference.

Seed paths and symbols are sorted and deduplicated before hashing. Selection is
bounded breadth-first traversal over pre-resolved evidence from one immutable
generation. Every item retains a role and selection reason; unresolved evidence
remains uncertainty or an omission. No model, embedding, learned weight, or
provider execution participates in the query path.

## ADR-P008: Generation Delta and evidence leases

Generation Delta compares files by stable file identity, symbols by canonical
key, and relationships by a composite source/type/target identity because fact
rows do not survive re-extraction. An unchanged contract requires unchanged
source content and an identical outbound relationship set. Evidence leases are
trusted only through verify-on-use live-file hashing; database state alone is
never sufficient.

## ADR-P009: Bounded temporal explanation

Temporal Intelligence compares the active committed generation only with a
retained committed ancestor. It derives changes from Generation Delta and never
reconstructs absent history. A first generation therefore yields an explicit
baseline-only result rather than treating all current facts as newly added.

Returned change and unchanged-contract details are independently bounded and
deterministically ordered. An unchanged symbol contract remains valid only when
its active-generation source hash matches the live file at query time; returned
current-file changes receive the same verification. Omitted, stale, unreadable,
ambiguous, or unconfined evidence cannot be promoted to current validity. Risk
is a fixed reason-code classification over recorded change, coverage, conflict,
relationship-resolution, uncertainty, and live-hash evidence; it is not a
learned or predictive score.

## Public serialized contract authority

The versioned `serde` types in `src/provider_contract.rs` and
`src/context_ir.rs` are the authoritative JSON contracts for provider runtime
messages and Context Intelligence documents. They reject unknown fields, pin
their protocol or schema version constants, and are exercised by the public
examples under `tests/contract_examples/` and `tests/fixtures/context_ir/`.

Persistence uses the same closed vocabularies. The authoritative database
constraints are the packaged migrations
`migrations/0002_semantic_provider_foundation.sql` and
`migrations/0003_context_intelligence_foundation.sql`; code and migration
changes must remain compatible and land together.
