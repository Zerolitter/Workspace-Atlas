# Spec: `context-ir-completion`

## Objective

Complete Context IR `2.0.0` as the canonical, model-neutral semantic output of the `ATLAS_DEEP` route. The IR describes selected evidence, verified digest/range references, deterministic budgets/results/omissions, and sufficiency; it is not a prompt, model response, raw memory dump, execution trace, embedding, or second source of truth. DIRECT and ATLAS_LIGHT never fabricate an IR.

## Current-state delta

Context IR `1.0.0` already has closed typed sections for identity, task, policy, status, working set, relationships, effects, uncertainty, coverage, omissions, validation, cost, and hash. It is deterministic, generation-bound, seed-canonicalized, budgeted, persisted, and shared by CLI/MCP. H5 selects a successor `2.0.0` semantic contract rather than silently reinterpreting these fields.

Incomplete semantics:

- `effects` and `validation_plan` are emitted empty;
- source references carry indexed range/hash and are `not_requested`, not live authorization;
- status is based mainly on empty/unresolved seeds, truncation, and Serving fallback rather than recipe-required role sufficiency;
- `source_tree_hash` is not populated;
- cost elapsed time is zero;
- coverage details are coarse;
- temporal/history evidence is absent;
- creation/execution time, route attempts, retries, cancellation/interruption, cache, and other transient state must remain outside the semantic IR.

## Canonical contract

### Identity and determinism

For identical immutable generation, normalized task/seed identity, `planner-v2.0.0`, projection/estimator/IR versions, and deterministic semantic budgets, IR MUST produce identical selected IDs/order/omissions/reasons/hash. T11 freezes domain-separated canonical encoding/hash fixtures. Route decision, profiles used only for routing, time/deadline/retry/cancellation/interruption/cache/attempts/materialization stay outside IR.

### Required sections

All current sections remain present. Each selected item preserves canonical identity, role, reason, origin, distance, deterministic rank, evidence state/confidence/provider/revision/generation, cost, and optional exact-source reference.

Populate:

- effects from qualified Truth/Serving evidence;
- validation targets from task recipe and affected tests/config/build contracts;
- coverage/uncertainty/omissions with typed reason codes and exact counts;
- source-tree/provider/policy identities when available;
- selected exact-source references as SHA-256 whole-file digest plus zero-based half-open byte range over exact bytes, live observed digest/status, canonical-root confinement, and final pre-seal revalidation; never source bodies;
- temporal constraints through `temporal-evidence-integration`.

### Sufficiency

Each task recipe declares required and optional roles. Status rules:

- `complete`: every required role is satisfied by qualified current evidence and no required evidence was omitted;
- `partial`: at least one useful required role is present, but required evidence is unresolved/stale/unavailable/unsupported/conflicting or budget-omitted;
- `blocked`: no safe primary target/current exact source exists where required, no active generation exists, or the task cannot proceed under the declared contract.

Serving fallback alone may be a performance reason in the execution envelope but MUST NOT make semantically equivalent complete evidence partial unless the fallback hits a deterministic bound or cannot prove the same coverage. Context Governor escalation may request deep compilation, but route attempts do not change these semantic status rules.

### Exact source boundary

The IR stores verified digest/range references, not source bodies. `atlas source` remains byte authority. Materialization is explicitly authorized/byte-bounded, live-verified, response-lifetime only, and prohibited from persistence/logs/WAL/crash diagnostics under `context-execution-v2.0.0`.

### Compatibility

Context IR `2.0.0` is an explicit successor. A `1.0.0` reader rejects it; V2 may explicitly read legacy but cannot claim V2 sufficiency. T11 freezes the complete V2 field matrix—including fields T12/T14/T15/T24 later populate—and old/new fixtures. Later field/vocabulary changes require renewed contract/version review.

T11 uses a separate internal V2 reader/writer and `planner-v2.0.0`. H6B-A0 has frozen the shared additive public application/CLI/MCP semantics, but selection exposes or advertises nothing. T20 remains blocked until this documentation repair is freshly accepted and then covers only retention/unregister internals on disposable/test catalogues; T18A still requires completed T20 plus separately approved `H7-C/POST`, with T18B and T19 following before the new surfaces become available. Existing explicit CLI/MCP remain `1.0.0`/`planner-v1.1.0`. No V2 durable row or lifecycle reference lands until its applicable H7 gate approves mixed-catalogue old-binary, cleanup/referential-integrity, backup/restore, and rollback evidence. Selective routing does not alter schema choice; DIRECT/LIGHT have discriminated non-IR payloads.

## RED–GREEN–REFACTOR acceptance

**RED**

- behavior/API/review fixtures currently emit empty effects/validation;
- a required missing test/effect/exact-source role may still report complete/coarse status;
- stale selected source cannot be distinguished as a sufficiency failure during compile;
- ready Serving and equivalent Truth fallback status semantics may differ only because fallback exists.

**GREEN**

- recipe matrix fills every frozen V2 field and exact typed reason;
- stale/drifting required source yields partial/blocked after final revalidation;
- repeated compilation is hash deterministic excluding execution data;
- ready-serving and Truth fallback produce equal semantics;
- `1.0.0`/`2.0.0` readers fail closed as specified; public legacy output remains unchanged; no V2 persistence occurs.

**REFACTOR**

- separate canonical semantic IR from `context-execution-v2.0.0` and `ContextRouteDecision`;
- keep sufficiency in fixed `planner-v2.0.0`; `planner-v1.1.0`/explicit defaults remain legacy rollback and H3-A profiles remain unpromoted;
- avoid growing the already-large compiler file when focused modules can own typed policy/packing.

## Verification commands

```sh
cargo test --locked context_ir::tests
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo test --locked
cargo fmt --all -- --check
```

## Boundaries

- Always: closed typed contract, deterministic identity, qualified evidence, explicit uncertainty/omissions, verified source boundary.
- Ask first: any additional schema/vocabulary change beyond reviewed `2.0.0`, exact V2 public cutover, or new durable persistence.
- Never: natural-language prompt/source body/transient execution state in canonical IR, hidden learned score, evidence-state flattening, treating indexed source as edit authority, or mandatory deep compilation for every request.

## Success criteria

1. Every documented Context IR section carries real or explicitly unavailable typed data.
2. Required-role sufficiency drives truthful complete/partial/blocked status.
3. IR remains model/provider neutral and deterministic.
4. Exact source stays separately live-authorized and privacy bounded.
5. Existing consumers have an explicit compatibility path.

## Decision status and remaining gates

- **H5 resolved:** Context IR `2.0.0`, exact verified references, separate execution envelope, explicit compatibility.
- **H4 resolved:** public baseline stays `planner-v1.1.0`; V2 role/sufficiency uses fixed `planner-v2.0.0`.
- **H6B-A0 resolved; implementation/H7 gates remain:** the shared additive public contract is frozen, but its commands/tools remain unavailable until their implementation gates; durable V2 rows and lifecycle references remain separately H7-gated.
- **Context Governor:** [`SPEC-context-governor.md`](SPEC-context-governor.md) keeps deep optional and route identity separate.
