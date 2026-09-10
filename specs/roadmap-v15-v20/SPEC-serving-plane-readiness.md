# Spec: `serving-plane-readiness`

## Objective

Make the disposable Serving Plane a measurable, generation/policy-keyed acceleration layer with explicit readiness, invalidation, bounded fallback equivalence, and operator visibility. It must never redefine Truth Plane evidence.

## Current-state delta

The base transactionally builds generation-bound SymbolCards, relationship edges, and coverage rollups; prefers one ready projection; reports fallback; can delete/rebuild deterministically; and leaves no partial ready state on injected failure.

Incomplete:

- only manual build/delete; no explicit status/rebuild surface or lazy scheduling;
- no automatic supersession/invalidation when truth/policy changes;
- build report lacks stage timing/input/output bytes/hash summary;
- rollup scope is incomplete relative to workspace/scope/package/file/provider/capability goals;
- conflict summaries/cost estimator version are incomplete;
- L0/L2/L3 cache concepts are not implemented;
- fallback is semantically tested in compiler paths but not systematically compared to ready projections;
- doctor checks only stale `building` rows.

## Contract

### Identity/readiness

Projection identity is `(workspace_id, generation_id, serving_schema_version, projection_policy_version)`. States are explicit `building|ready|failed` plus derived `absent|stale/superseded` status. A ready row becomes immutable; a new Truth generation/policy creates a new projection rather than mutating old rows.

Build captures one committed generation, reads canonical facts for it, writes in one derived transaction, validates counts/references/hashes, then marks ready. Failure leaves Truth readable and no partial result discoverable as ready. Retry/rebuild is idempotent.

### Minimum projections

- SymbolCard with preferred/alternative/conflict, direct adjacency counts, tests/config/effects/unresolved, coverage, and versioned costs;
- inbound/outbound ServingEdge preserving relationship type, resolution/trust/evidence/range/order;
- CoverageRollup at approved scopes;
- compact conflict/unresolved summary;
- versioned cost estimate.

Optional source-fragment caching remains disabled by default. If approved later, it is content-addressed and still live-hash checks before returning change-mode bytes.

### Invalidation and fallback

Activation/policy drift marks old projection non-current by identity; it does not corrupt/delete it in-place. A caller may synchronously build only under explicit command or use bounded Truth fallback and request/schedule a rebuild. No daemon is required.

Ready projection and fallback over the same generation/policy/budget MUST return the same canonical evidence identities/states/order/omissions where the same work completes. Differences are execution metrics/readiness reasons, not truth. Fallback may return typed partial at its hard budget; it may not scan/reparse/resolve the repository.

### Observability

Status/build reports include identity/state, canonical input fingerprint/counts, stage time, output rows/bytes/hash, cache hit/miss/fallback, failure code, and rebuild recommendation. Stage details are excluded from deterministic projection content hash.

## Interfaces

Subject to H6:

- `atlas serving-status <root>` / MCP equivalent;
- existing `serving-build` remains idempotent;
- additive explicit `--rebuild` semantics only if build cannot already express it clearly;
- `status`/`doctor` summarize absent/stale/failed/orphaned states and safe repair.

Do not introduce an always-on watcher/daemon.

## RED–GREEN–REFACTOR acceptance

**RED**

- activate a new generation and show stale projection lacks explicit current-state handling;
- build report cannot show stage/byte/hash metrics;
- ready versus fallback equivalence matrix is absent;
- corruption/orphan/policy drift is not fully diagnosed;
- large fixture exposes any fallback repository-scale work.

**GREEN**

- status resolves current/absent/building/ready/failed/superseded deterministically;
- build/rebuild records metrics and identical content hash/counts;
- fault points never expose partial ready data and retry succeeds;
- ready/fallback semantic equivalence holds across task kinds/budgets;
- new generation/policy invalidates by identity and preserves captured readers.

**REFACTOR**

- keep projection builder separate from scheduling/status;
- reuse canonical evidence ranking/resolution state; no second policy;
- avoid optional caches until measurements justify maintenance cost.

## Verification commands

```sh
cargo test --locked serving::tests
cargo test --locked task_compiler::tests
cargo test --locked --test task_compiler_v13
cargo run --release --locked --bin atlas-bench
cargo test --locked
```

## Boundaries

- Always: committed generation, transactional build, immutable ready projection, explicit fallback, deterministic rebuild, no truth mutation.
- Ask first: automatic/lazy rebuild trigger, projection schema/policy version, cache levels, rollup scopes.
- Never: provider/parser/resolver in serving build/query beyond canonical facts; hidden fallback; mandatory daemon; source cache as authority; optional cache without measured win.

## Success criteria

1. Operators and callers can determine/repair serving readiness safely.
2. Projection/fallback preserve semantic output under equivalent completed work.
3. Build/invalidation costs are measured and bounded.
4. Generation/policy changes cannot serve stale projections as current.
5. Deleting all serving data leaves canonical operations correct.

## Open decisions

- **H3/H4 — resolved baseline:** measure with H3-A discipline; promote no runtime profile or optional cache.
- **H5/H7:** Context IR `2.0.0` does not require a projection change; any projection schema/durable change remains H7-gated.
- **H6B:** exact status/rebuild and additive governor command/tool shapes.
