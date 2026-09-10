# Spec: `context-use-observation`

## Objective

Complete a privacy-bounded, local, evidence-classified task-session record that can answer what Atlas supplied and what Atlas later observed—without claiming that a read proves reasoning use.

## Current-state delta

The base has deterministic task sessions, closed event/source enums, bounded `details_json`, one `context_supplied` event per selected item, one `relationship_traversed` event per selected relationship, and optional `atlas source --task-session-id` attribution with unchanged legacy output.

Missing or incomplete:

- entity/path/symbol query attribution;
- validation-selection events;
- task completion linked to post-task Generation Delta/artifact changes;
- explicit outcome event semantics and public completion path;
- bounded raw-task retention/expiry;
- compaction/deletion behavior;
- proof that repeated delivery/retrieval semantics do not overcount or conflate categories.

## Contract

### Event taxonomy

Preserve the existing closed `ContextUseEventType` and `ObservationSource` vocabularies. Each event has deterministic or idempotency-safe identity, session/context/item/entity linkage where known, bounded byte count, bounded closed details, and occurrence time excluded from semantic identity where repeat delivery must remain distinguishable.

**Atlas-observed:** context supplied, exact source requested, entity queried, relationship traversed, context recompiled, artifact changed, validation selected.

**Agent/operator-reported:** reasoning use, modification target, test considered, outcome. Reported evidence never upgrades to Atlas-observed and is never project truth.

### Use semantics

- Source retrieval proves retrieval, not reasoning use.
- Entity query/traversal proves an Atlas operation, not usefulness.
- Generation Delta proves artifact change, not that every item for that file was useful.
- Explicit reported-use evidence remains separate in every metric.
- Failed/stale/unavailable exact-source attempts are recorded with status and zero/known bytes; they never count as successful exact-source use.
- Calls without a task session preserve existing behavior and create no event.

### Session lifecycle

The state machine remains `created → context_compiled → active → reconciled → completed|abandoned|failed`. Completion requires an end generation/tree hash, accepted outcome, optional validation result, and bounded outcome code. Illegal transitions fail closed. Repeated calls are idempotent only when the full completion intent matches; conflicting replay fails.

For the legacy `1.0.0` flow, the application validates every supplied session identity against exact workspace, task/normalized-goal/classification identity, applicable start generation, contract, and nonterminal state before mutation; absent identity remains unattributed. Successful explicitly bound acquisition is the activation boundary: completed DIRECT `direct_none` reaches active without fabricated context; LIGHT/DEEP requires completed or useful partial output; blocked/interrupted/error/no-useful-payload does not activate. A non-blocked sealed legacy Context IR and all delivery events commit with activation, and attributed successful source/query observation commits its existing event with activation so persistence failure rolls both back.

Successful generation activation reconciles every and only older-generation active legacy session in that workspace, ordered by session ID, in the same immediate transaction as candidate commit and the active pointer. Sessions created against the candidate generation and all created, context-compiled, reconciled, completed, abandoned, or failed sessions are untouched. Post-commit lifecycle mutation risks a crash split; a per-session reconcile flag adds grammar; changed-file overlap would overclaim causality; internal transition injection does not verify the public path. This is the H6B-A0 reachability clarification for the frozen flow, not release or V2.0 completion.


### Privacy and retention

No upload. No raw source in event details. Raw task text remains explicit local opt-in with H2-approved byte/TTL bounds. Event/session compaction is owned by `lifecycle-operations`; deletion may cascade derived session/context/events but never Truth Plane generations/facts.

## Interfaces

Library application functions should accept an optional typed attribution context rather than spreading optional string IDs. CLI/MCP public lifecycle is specified by `compiler-surfaces`; this module owns behavior, not transport.

No schema change is preferred: existing task/event tables and vocabularies already cover the required events. A vocabulary change requires migration and Context IR fixture review.

## RED–GREEN–REFACTOR acceptance

**RED**

- attributed `find`/`inspect`/`trace` or equivalent query produces no entity-query event;
- completion plus reconcile produces no artifact-change linkage;
- a failed exact-source request is indistinguishable from a successful read in naive counting;
- repeated conflicting completion is not explicitly rejected;
- raw opt-in accepts unbounded text.

**GREEN**

- representative source/query/traversal/validation paths emit exactly the typed events their observable behavior warrants;
- Generation Delta links changed artifacts after reconcile, with no causality overclaim;
- outcome and validation are captured once under legal lifecycle transitions;
- legacy unattributed calls are byte/logically unchanged;
- event details and raw opt-in are bounded and local.

**REFACTOR**

- centralize attribution/event construction in the owning application layer;
- preserve parameterized persistence transaction boundaries;
- keep query behavior independent from metric interpretation.

## Verification commands

```sh
cargo test --locked --test source_telemetry_surfaces
cargo test --locked task_session::tests
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo test --locked
```

Add focused integration cases for successful/failed source, entity query, validation selection, lifecycle replay, and delta-linked artifact changes.

## Boundaries

- Always: local-only bounded events, explicit observation source, legacy no-attribution behavior, parameterized persistence, no raw source in details.
- Ask first: new event vocabulary, public attribution flags, retention defaults, migration.
- Never: equate retrieval with reasoning, upload telemetry, store raw prompt/source by default, infer causation from changed artifacts.

## Success criteria

1. Supplied, retrieved, queried, traversed, changed, validated, and reported evidence remain distinguishable.
2. Every claimed event can be traced to an exercised application behavior.
3. No raw prompt/source leaks into default persistence or output.
4. Legacy calls remain unchanged when attribution is absent.
5. Derived events can be compacted without deleting Truth Plane evidence.

## Open decisions

- **H2:** raw-task byte/TTL bounds and default session/event retention.
- **H6:** which query commands accept optional session attribution publicly.
- **H7:** whether any idempotency/index constraint requires a migration; prefer application-level use of existing schema when sufficient.
