# Capability Map: V1.5 Working-Set Optimization and V2.0 Project Context Compiler

## Authority and scope

This initiative is planned from repository revision `17240edbcedf501f0dabfc29fff6d91b132a9eb8`. Roadmap labels are capability milestones, not Cargo semantic versions. This checkpoint changes no production source, schema, version, behavior, release, tag, or artifact.

The repository is intentionally private. Unauthenticated GitHub HTTP 404 is expected and is not a defect for this checkpoint. Configuration/privacy, MCP conformance, data lifecycle, CI, and distribution-readiness findings from the private adoption audit remain prerequisites for any future public adoption claim. Repository visibility, publication, tagging, and release creation require separate human authorization.

## Initiative outcomes

- **V1.5 — Working-Set Optimization:** measure what a task was supplied, what Atlas can observe being used, what was requested later, and whether the outcome was accepted; compare only frozen, compatible conditions; use results to propose deterministic versioned policy changes. Runtime learned ranking, embeddings, model calls, co-access prediction, and hidden adaptive state are excluded unless a later ADR and explicit human approval authorize them.
- **V2.0 — Project Context Compiler:** complete the existing Context IR compiler so `repository truth + task + explicit seeds + bounded policy` produces deterministic, typed, generation-bound, evidence-qualified working memory for any model or agent. Source remains authoritative; exact source remains separately live-verified; adapters may render the IR but may not change its semantics.

## Stable module map

Module IDs are permanent selectors for specs, tasks, review gates, and future Orca dispatches.

| Module ID | Milestone | Responsibility | Existing foundation at base | Depends on |
|---|---|---|---|---|
| `configuration-privacy-boundary` | prerequisite | Fail-closed path-pattern semantics, durable configuration identity, safe traversal, and privacy-preserving telemetry configuration. | Canonical-root confinement, default exclusions, direct provider spawn; audit found invalid custom patterns are silently ignored, `.key` default mismatch, full excluded-tree traversal, and config continuity gaps. | — |
| `mcp-conformance` | prerequisite | Negotiate a real MCP protocol version, publish accurate tool schemas, and prove lifecycle/tool behavior with a real client while keeping Atlas capability versions separate. | Shared CLI/MCP application layer and 13 tools; current adapter returns Atlas capability `1.3` as `protocolVersion` and advertises uninformative object schemas. | — |
| `context-use-observation` | V1.5 | Complete local task-session observation for supplied items, exact source, entity queries, traversals, validation selection, artifact changes, and outcomes without conflating observed and reported use. | Task sessions, closed event vocabulary, compiled-context events, optional source attribution. | `configuration-privacy-boundary` |
| `retrieval-instrumentation` | V1.5 | Record actual total and stage latency, candidate/edge counts, cache/fallback, bytes, estimated tokens, truncation, serving-build costs, and bounded-frontier proof. | `query_stage_metric` schema exists but is unwritten; `IrCost.elapsed_ms` is always zero; `ServingBuildReport` has final row counts only. | — |
| `context-yield-metrics` | V1.5 | Derive supplied/used/expanded/rediscovered sets and source efficiency with numerators, denominators, evidence classes, estimator version, and accepted-outcome gating. | Context-use events and a repeatability-only `ContextYieldReport`; no metric computation. | `context-use-observation`, `retrieval-instrumentation` |
| `policy-experiments` | V1.5 | Run frozen, comparable, local experiments across budget profiles and representations; retain limitations and variance; produce reviewable deterministic policy proposals, never automatic ranking updates. | Frozen-generation repeatability and five-dimension comparability; Golden Nugget A/B evidence; no profiles, accepted-outcome gate, retained bundles, or representation A/B. | `context-yield-metrics` |
| `context-governor` | V2.0 | Deterministically choose `DIRECT` → `ATLAS_LIGHT` → `ATLAS_DEEP` through a versioned decision seam; allow zero-context success and keep deep compilation optional. | Explicit direct tools, bounded query/source operations, legacy Context Packet, and Context IR compiler exist independently; no shared route decision, escalation contract, or route telemetry. | `policy-experiments`, `retrieval-instrumentation` |
| `context-ir-completion` | V2.0 | Define and fill deep-only Context IR `2.0.0`: required-role sufficiency, effects, temporal constraints, verified digest/range source references, uncertainty, validation, cost, omissions, and deterministic identity. | Closed Context IR schema `1.0.0` and typed fields; effects and validation are empty, source references are indexed rather than live-authorized, and sufficiency is coarse. | `context-governor` |
| `bounded-context-compiler` | V2.0 | Complete canonical seed resolution, recipe-driven evidence selection, uncertainty reservation, profile budgets, hard-time cutoffs, deterministic packing, and truthful partial/blocked results. | Fixed task kinds/recipes, canonicalized seeds, bounded BFS, record/source/token limits, Serving/Truth fallback; no time enforcement, explicit uncertainty reserve, bounded FTS fallback, or full required-role closure. | `context-ir-completion` |
| `temporal-evidence-integration` | V2.0 | Feed bounded Generation Delta, verified unchanged contracts, lifecycle history, effects, and structural lease validity into review/bug/API recipes without inventing history or freshness. | V1.4 temporal report and committed-generation checks; compiler does not consume temporal evidence and leases do not validate all policy dimensions. | `context-ir-completion` |
| `serving-plane-readiness` | V2.0 | Make ready projections measurable, generation/policy-invalidated, status-visible, lazily rebuildable, and equivalent to bounded Truth fallback. | Disposable SymbolCards, edges, coverage rollups, manual build/delete, explicit fallback; no build-stage timing, automatic supersession/rebuild, complete rollups, or status surface. | `retrieval-instrumentation` |
| `lifecycle-operations` | prerequisite + V2.0 | Expose bounded legacy task start/show/complete/abandon, privacy compaction, and truthful catalogue removal semantics. Compaction preserves Truth/history; irreversible unregister may remove the complete Atlas catalogue but never workspace source. | Internal task lifecycle and forward-only migrations; no public task lifecycle or retention/compaction/unregister path. | `context-yield-metrics`, `serving-plane-readiness` |
| `compiler-surfaces` | V2.0 | Preserve one shared application layer for Context IR compile/show, source attribution, Context Yield, serving status, and typed partial/error semantics through CLI and MCP. Existing serving-build behavior remains a separate unchanged V1 surface; H6B-A0 adds no MCP rebuild. | CLI/MCP parity for compile, serving build, delta, temporal; missing show/yield/lifecycle/status surfaces. | `bounded-context-compiler`, `temporal-evidence-integration`, `serving-plane-readiness`, `lifecycle-operations`, `mcp-conformance` |
| `ci-distribution-gates` | prerequisite | Continuously verify supported platforms, MSRV/stable, privacy config, MCP clients, providers, upgrades, package contents, capacity evidence, checksums/SBOM design, and public-hygiene boundaries. It prepares evidence only; publishing remains separately authorized. | Locked local checks and package hygiene; no workflow, platform matrix, release supply chain, or adoption envelope. | all other implementation modules |

## Dependency order

```text
configuration-privacy-boundary ──► context-use-observation ──► context-yield-metrics ──► policy-experiments
retrieval-instrumentation ────────► context-yield-metrics
retrieval-instrumentation ────────► serving-plane-readiness

policy-experiments ───────────────► context-governor ──► context-ir-completion ──► bounded-context-compiler
context-governor ─────────────────► compiler-surfaces
context-ir-completion ────────────► temporal-evidence-integration
context-yield-metrics ────────────► lifecycle-operations ◄── serving-plane-readiness

mcp-conformance ───────────────────────────────────────────────────────────┐
bounded-context-compiler ─────────────────────────────────────────────────┤
temporal-evidence-integration ────────────────────────────────────────────┼─► compiler-surfaces
serving-plane-readiness ──────────────────────────────────────────────────┤
lifecycle-operations ─────────────────────────────────────────────────────┘

all other implementation modules ──► ci-distribution-gates
```

Recommended build order:

1. Privacy/configuration boundary, MCP conformance, and retrieval instrumentation may start independently.
2. Complete observation, then metrics, then comparable experiments.
3. Human review retained `planner-v1.1.0` and explicit defaults; no H3-A profile is promoted and no runtime learning/adaptation is introduced.
4. Freeze the deterministic Context Governor seam and route semantics before V2 interfaces.
5. Complete deep-only Context IR `2.0.0`, then bounded compiler and temporal integration slices.
6. Finish serving readiness and lifecycle operations before public compiler surfaces.
7. Run the cross-platform/distribution gate; publication remains out of scope.

## Overlap and sequencing

| Shared area | V1.5 responsibility | V2.0 responsibility | Sequencing constraint |
|---|---|---|---|
| Task sessions | Capture trustworthy, privacy-bounded observations and accepted outcomes. | Anchor compiler identity, lifecycle, show/complete, and retention. | Observation semantics must stabilize before public lifecycle surfaces. |
| Context IR | Measure supplied items and actual cost without changing semantic identity. | Complete required fields, sufficiency, temporal evidence, and deterministic packing. | V1.5 metrics must understand the existing schema before any reviewed schema/policy revision. |
| Planner policy | Compare fixed profiles and recipes under accepted outcomes. | Execute the approved fixed policy. | V1.5 may recommend; only a human-approved versioned policy may change V2 selection. |
| Serving Plane | Measure hits, misses, fallback, build cost, and boundedness. | Provide ready/invalidation/status behavior and semantic equivalence. | Instrument before optimizing; neutral or noisy changes are reverted. |
| Generation Delta | Link observed changed artifacts to task outcomes. | Supply bounded historical evidence to task recipes. | Keep metric attribution separate from claims of causation or reasoning use. |
| CLI/MCP | Expose measurements only after privacy and conformance gates. | Deliver the compiler contract through the shared application layer. | Protocol conformance precedes new MCP tools. |
| Context Governor | Compare fixed direct/light/deep conditions and accepted outcomes without changing runtime policy. | Record deterministic route signals/reasons, starting-generation observation, progressive escalation, ceilings, and execution envelope. | Route seam freezes before Context IR/public surfaces; no file-size-only, learned, model/vendor-specific, or timing-driven selection. |
| Future Agent Skill | Compare procedural cooperation patterns without embedding project facts. | Consume transport-neutral capability discovery and DIRECT/LIGHT/DEEP application semantics through public CLI/MCP/future SDK only. | Compatibility requirement across T28/T17/T18/T19/T21/T22; no new milestone, direct catalogue access, or implementation before reviewed skill content and separate authority arrive. |

## Compatibility and migration policy

| Surface | Default decision | Migration/compatibility rule |
|---|---|---|
| Cargo version | Unchanged during planning. Future bump requires release/version review. | Roadmap labels do not imply Cargo `1.5` or `2.0`. |
| Context IR | `1.0.0` remains legacy; H5 approves deep-only semantic `2.0.0`. | Old/new readers fail closed; no V2 durable rows before H7 mixed-catalogue/rollback approval. |
| Planner policy | Public baseline `planner-v1.1.0`/explicit defaults; no H3-A promotion. V2 role/sufficiency uses fixed `planner-v2.0.0`. | Later ordering/recipe/role/estimator/sufficiency changes use another fixed identity. |
| Route/execution/capability contracts | Immutable `context-route-v2.0.0`; transient `context-execution-v2.0.0`; discovery `context-capabilities-v2.0.0`. | One shared semantic `GovernorRunRequest` application builder negotiates Atlas versions separately; no transport constructs derived route identity, and no V2 state is durable. |
| Catalogue schema | Prefer existing `0003` tables for events and stage metrics. | Add a forward-only migration only for durable data that cannot fit existing closed vocabularies; include old-catalogue fixtures, backup/restore, checksum, and no-downgrade evidence. |
| CLI | Existing names/arguments/output remain supported. H6B-A0 freezes additive governor, legacy lifecycle/read, retention, and unregister grammar with optional explicit catalogue selection on every new workspace command. | No existing command is routed or deprecated; first-contract output is stdout-only with no caller-path export or Atlas-created backup. |
| MCP | Correct lifecycle negotiation precedes exactly six additive H6B-A0 tools; all 13 existing tools remain compatible, including the derived-Serving `atlas_serving_build` operation. | The six additions expose only governor capability/run plus bounded task/context/yield/serving reads: no source materialization, task/source/lifecycle mutation, retention, unregister, serving build/rebuild, backup/export, caller-path or other filesystem write, or destructive authority. The retained `atlas_serving_build` does not mutate workspace source or committed Truth. |
| Providers | No provider runs on the compiler query path. | No required provider, provider installation, or network dependency is introduced. |
| Telemetry | Local-only, bounded, no raw prompt/source by default. | Observation-source distinctions and deletion/retention behavior are contractual. |
| Package | Planning files live outside Cargo `include`. | Future public docs/code enter package only through reviewed allowlist changes; no release artifact is produced here. |

## Cross-cutting invariants

1. Local-first, provider-neutral, verified-source guarantees remain mandatory: source, builds, tests, runtime evidence, and live hashes are authoritative.
2. Truth Plane evidence is immutable by generation and never depends on a task.
3. Serving and Context planes are derived, versioned, disposable, and rebuildable.
4. Privacy compaction preserves Truth Plane evidence and generation history. Irreversible unregister is a distinct whole-catalogue deletion that may remove indexed Truth/history but never workspace source, and remains unavailable until T20 proves external exclusion across SQLite close and Windows removal.
5. Every returned claim preserves provider, evidence, coverage, conflict, resolution, generation, omission, and staleness state where applicable.
6. Ordinary compiler work is `O(seed lookup + bounded frontier + selected verification/packing)`, never repository-scale discovery, provider execution, parsing, or resolution.
7. Equivalent inputs under deterministic semantic work budgets produce identical selected IDs, ordering, omissions, policy identity, and deterministic hash; timestamps, measured wall time, and cache observations stay outside that hash.
8. Every immutable route decision records starting generation observed/unavailable, initial route/reasons, and canonical hash. DIRECT may succeed without pinning/compiling or creating context identity.
9. Attempts, final route/deficits/state, time, cache, retry/cancellation/interruption, and materialization exist only in the bounded execution envelope.
10. Semantic limits return deterministic typed partial/blocked IR. Wall expiry returns interrupted envelope without canonical timing-dependent partial IR.
11. Raw task/seed/path text, prompts, source bodies/materialized bytes, and model/vendor names are not telemetry. No telemetry leaves the machine.
12. CLI, MCP, and future SDK use shared discriminated DTOs/application functions and Atlas contract-version negotiation. Transports own no policy.
13. H6B-A0 bounds request documents to 1 MiB, collections to default 50/max 200, cursors to 4 KiB, and every nested value to advertised hard limits. Cursors are restart-stable non-authority tokens bound to exact resolved catalogue/workspace state.
14. Context IR is model-neutral. Future immutable capability/cost manifests may describe disclosed operational classes without model/vendor names; none is registered/promoted now.
15. A future Agent Skill is separately versioned/packaged procedure, contains no repository truth, uses public discovery/application contracts only, and never changes semantic identity merely by invocation.
16. No learned/model/file-size-only/hidden adaptive state without new ADR, evidence, privacy, rollback, and explicit human approval.

## Gate status

| Gate | Status | Durable record |
|---|---|---|
| **H1 — Scope/map** | **Accepted — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H2 — Privacy/config** | **H2-A accepted — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H3 — Performance profiles** | **H3-A accepted — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H4 — Planner/profile policy** | **Accepted: retain `planner-v1.1.0`/explicit defaults — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H5 — Context contract** | **Accepted: Context IR `2.0.0`/separate execution envelope — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H6A — MCP protocol/client/schema** | **H6A-A accepted — 2026-09-02** | [`decision-checkpoint.md`](decision-checkpoint.md) |
| **H6B — additive compiler/lifecycle surfaces** | **H6B-A0 accepted — 2026-09-03** | [`decision-checkpoint.md`](decision-checkpoint.md) |

H1, H2-A, H3-A, H4, H5, H6A-A, and H6B-A0 authorize only their mapped dependency work. H6B-A0 authorizes T20 only in disposable/test catalogues; it does not resolve or bypass `H7-C/POST`, unrelated H7, H8, live use, or separate execution authorization.

## Assumptions to validate

- **A1:** repository/roadmap remain private; unauthenticated 404 is expected.
- **A2 — Resolved by H1:** V1.5 means offline measurement/human-reviewed fixed policy, not runtime learning.
- **A3 — Resolved by H5:** Context IR `2.0.0` is deep-only semantic output; transient execution/source state stays outside its hash.
- **A4:** existing `0003` may host route metrics, but no V2 context/decision/envelope persists before H7 approves mixed-version durability and rollback.
- **A5:** roadmap labels stay independent of Cargo semantic version.
- **A6 — Resolved by H3-A/H4:** private small/standard/audit budgets and 150/250/2,000 ms acceptance ceilings are not runtime defaults; H4 promotes none.
- **A7 — Resolved by H6A-A:** stdio supports MCP `2026-07-28` and legacy `2025-11-25` through `rmcp 3.2.0`; T22 owns external clients.
- **A8 — Resolved by H5:** routing does not cause schema `2.0.0`; immutable decisions observe generation/unavailability, and DIRECT can succeed with zero context.

## Human review gates

- **H1 — Scope/map (accepted):** the owner approved module boundaries, dependency direction, and the interpretation that V1.5 “learn” means measured deterministic policy evaluation, not runtime learned ranking. See [`decision-checkpoint.md`](decision-checkpoint.md).
- **H2 — Privacy/config (H2-A accepted):** fail-closed regex, self-contained policy without implicit `.gitignore`, credential-safe registered snapshots, no raw task/source persistence, closed 4,096-byte typed details, and bounded structured/hash retention are approved.
- **H3 — Performance profiles (H3-A accepted):** the pinned budgets, sample counts, three repetitions, stable scenario/generated-attestation separation, and private acceptance wording are approved.
- **H4 — Policy (accepted):** retain `planner-v1.1.0` and explicit defaults; promote no H3-A profile; add no adaptive runtime selection. Changed ordering, roles, sufficiency, or route semantics require a new fixed identity.
- **H5 — Context contract (accepted):** use deep-only Context IR `2.0.0` for generation-bound deterministic semantics and verified digest/range references; use `context-execution-v2.0.0` for the repaired transient materialization/execution contract. Compatibility/rollback are explicit and durable migration remains H7-gated.
- **H6 — Public surfaces (accepted contract, implementation gated):** H6A-A fixes the dual-era stdio SDK/schema boundary. H6B-A0 fixes the corrected additive local-CLI/read-only-MCP contract, bounded restart-stable cursors, legacy-only lifecycle, and fail-closed destructive semantics. Selection changes no availability; T20 alone may begin on disposable/test catalogues, while T18 still requires T20 plus `H7-C/POST`.
- **H7 — Migration:** two-step gate for each proposed migration: pre-edit authorization after exact DDL/fixtures/backup/restore/no-downgrade design and RED compatibility proof; post-implementation acceptance after old-catalogue, idempotency, fault, backup/restore, checksum, and rollback evidence. No downstream surface consumes the migration between those gates.
- **H8 — Adoption/release:** review configuration/privacy, MCP, lifecycle, CI, capacity, supply-chain, and support evidence before any visibility change, tag, public artifact, crate, or release. This checkpoint grants none of those actions.

## Spec index

- [`decision checkpoint`](decision-checkpoint.md)
- [`configuration-privacy-boundary`](SPEC-configuration-privacy-boundary.md)
- [`mcp-conformance`](SPEC-mcp-conformance.md)
- [`context-use-observation`](SPEC-context-use-observation.md)
- [`retrieval-instrumentation`](SPEC-retrieval-instrumentation.md)
- [`context-yield-metrics`](SPEC-context-yield-metrics.md)
- [`policy-experiments`](SPEC-policy-experiments.md)
- [`context-governor`](SPEC-context-governor.md)
- [`context-ir-completion`](SPEC-context-ir-completion.md)
- [`bounded-context-compiler`](SPEC-bounded-context-compiler.md)
- [`temporal-evidence-integration`](SPEC-temporal-evidence-integration.md)
- [`serving-plane-readiness`](SPEC-serving-plane-readiness.md)
- [`lifecycle-operations`](SPEC-lifecycle-operations.md)
- [`compiler-surfaces`](SPEC-compiler-surfaces.md)
- [`ci-distribution-gates`](SPEC-ci-distribution-gates.md)
