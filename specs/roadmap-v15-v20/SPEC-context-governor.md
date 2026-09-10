# Spec: `context-governor`

## Objective

Add a deterministic Context Governor to the Context Plane before V2 interfaces freeze. It progressively selects `DIRECT` → `ATLAS_LIGHT` → `ATLAS_DEEP`, may succeed with zero Atlas context, and never makes full Context IR compilation mandatory. It chooses context acquisition; it does not modify Truth Plane evidence, choose a model, reinterpret Context IR, or learn from outcomes.

## Current component mapping

| Route | Current component mapping | Result boundary |
|---|---|---|
| `DIRECT` | No Atlas context component runs. The caller uses already-supplied context and ordinary source/build/test tools. The routing observation still records the repository generation seen when deciding, or typed unavailability. | Successful decision with no Atlas context identity, Context Packet, or Context IR. |
| `ATLAS_LIGHT` | Version-pinned `query-v1.0.0` and `source-reference-v1.0.0` operations over explicit targets. Query owns bounded identity, direct-relationship, impact, and basic coverage/conflict results; source reference is live verified and target bounded. | Bounded query/source-reference results only; no legacy Context Packet and no Context IR required-role claim. |
| `ATLAS_DEEP` | Existing `task_compiler::compile_context_ir` / CLI `context-ir` / MCP `atlas_context_ir`, extended by V2 role, temporal, source-reference, omission, and validation semantics. | Context IR `2.0.0`, or a separately versioned transient execution envelope. |

Ready Serving Plane projections may accelerate both Atlas routes; same-generation Truth fallback preserves semantics. Serving is not a fourth route. Truth Plane remains immutable, task-independent, and unchanged.

## H6B-A0 public request and transport contract

The owner accepted the corrected H6B-A0 boundary. CLI and MCP use one transport-neutral `GovernorRunRequest` application builder. Its negotiation metadata carries separate supported Atlas capability/route/decision/execution/IR/operation version sets; its semantic inputs are task, optional declared kind, explicit path/symbol targets, caller-capability states, Atlas intent, floor/ceiling, optional legacy session ID, and all six positive DEEP semantic limits whenever ceiling permits DEEP, plus optional CLI-only materialization authorization/cap. The application normalizes semantic inputs, negotiates the Atlas contract sets, then derives classifier output, normalized hashes, seed digest, starting generation, negotiated fixed identities, and budget digest before dispatch; adapters never construct route identity or own policy.

Request documents are capped at 1 MiB before allocation/decoding. H3-A profile names/defaults remain experiment-only: no runtime profile/default, mandatory DEEP, public SLO, future Agent Skill implementation, or durable V2 state is authorized. Every existing V1 command and all 13 MCP tools remain unchanged without routing or deprecation.

MCP adds only six V2 tools: `atlas_governor_capabilities`, `atlas_governor_run`, `atlas_task_show`, `atlas_compiled_context_show`, `atlas_context_yield_show`, and `atlas_serving_status`. Those six expose no source materialization, task/source/lifecycle mutation, retention/unregister, serving build/rebuild, backup/export, caller-path or other filesystem write, or destructive authority. The 13 unchanged V1 tools include the compatibility-retained `atlas_serving_build` derived-Serving projection operation, which does not mutate workspace source or committed Truth. Selection changes no advertised runtime availability; tools appear only after their implementation gates.


## Deterministic decision and execution contracts

### Immutable route request and decision

Introduce a closed typed `ContextRouteDecision` seam before dispatch. The repaired internal policy is `context-route-v2.0.0`. It supersedes the unreleased `context-route-v1.0.0` decision seam rather than running beside it: V2 removes legacy Context Packet from the governor LIGHT operation set, while leaving DIRECT, progressive escalation, and DEEP semantic selection unchanged. There is no V1 route reader/writer shim or parallel implementation.

- normalized task hash, declared kind when present, deterministic classifier result, and `task-classifier-v1.0.0`; raw task text is never retained;
- canonical explicit-seed digest/counts;
- closed caller-context sufficiency by required capability: `satisfied`, `unsatisfied`, or `unknown`;
- explicit Atlas intent: `none`, `allow`, or `require`;
- route floor and ceiling, each one of `DIRECT`, `ATLAS_LIGHT`, or `ATLAS_DEEP`;
- fixed capability/cost profile identity plus immutable registry digest when supplied;
- starting-generation observation: `observed` with repository generation ID, or `unavailable` with stable reason;
- fixed operation-contract versions for every allowed LIGHT operation and fixed planner/projection/IR/estimator versions for DEEP;
- explicit deep semantic-budget vector/digest (record, source-byte, token, depth, work-unit, uncertainty reserve) whenever ceiling permits DEEP; all values positive;

The decision repeats effective floor/ceiling, requested capability states, and deep-budget digest, then records initial route, exact matched signals/reasons, starting generation, policy version, and `decision_hash`. Attempts/final route/post-execution deficits/state/timing/cache/retry/cancellation/interruption stay only in the envelope.

`decision_hash` is SHA-256 over the immutable request fields plus decision output fields **excluding `decision_hash` itself**, using T28's domain-separated length-prefixed canonical encoding: versions first; fixed ASCII enums; UTF-8 NFC text; sorted/deduplicated sets; absent distinct from default. Transports hash the shared DTO fields, not wire bytes.

DIRECT observes starting generation through the governor's workspace-status reader but does not pin it, query context evidence, create an Atlas context identity, or compile anything. Typed observation failure remains in the decision. DIRECT can still succeed when every requested capability is satisfied by caller context.

### Closed signal vocabulary

- `caller_capabilities_satisfied`
- `caller_capabilities_unsatisfied`
- `caller_capabilities_unknown`
- `atlas_intent_none`
- `atlas_intent_allow`
- `atlas_intent_require`
- `route_floor_direct`
- `route_floor_light`
- `route_floor_deep`
- `route_ceiling_direct`
- `route_ceiling_light`
- `route_ceiling_deep`
- `explicit_seeds_present`
- `identity_or_source_requested`
- `relationships_or_impact_requested`
- `basic_coverage_conflicts_requested`
- `deep_synthesis_requested`
- `starting_generation_observed`
- `starting_generation_unavailable`

- `capability_profile_applied`
- `cost_profile_applied`

Signal inclusion is exact: emit each caller-capability state present across the per-capability map; exactly one intent, floor, ceiling, and generation signal; seed/profile signals iff supplied; and each requested-category signal iff its capability is present.

Closed required-capability vocabulary:

- `identity_lookup`
- `exact_source`
- `bounded_relationships`
- `impact_frontier`
- `basic_coverage_conflicts`
- `temporal_history`
- `required_role_closure`
- `validation_plan`

Closed decision-reason vocabulary:

- `caller_context_sufficient`
- `caller_context_incomplete`
- `atlas_not_requested`
- `explicit_atlas_request`
- `light_capability_required`
- `deep_capability_required`
- `route_floor_applied`
- `starting_generation_unavailable`

Minimum route for unsatisfied/unknown capabilities is fixed:

| Capability | Minimum Atlas route |
|---|---|
| `identity_lookup`, `exact_source`, `bounded_relationships`, `impact_frontier`, `basic_coverage_conflicts` | `ATLAS_LIGHT` |
| `temporal_history`, `required_role_closure`, `validation_plan` | `ATLAS_DEEP` |

Precedence is normative:

1. Reject floor above ceiling, unknown version/vocabulary/profile, missing input, intent `none` with non-DIRECT floor, intent `require` with DIRECT ceiling, or a DEEP-permitting request without the explicit budget vector.
2. Intent `none` forces effective floor/ceiling/initial route DIRECT.
3. Intent `allow` uses explicit floor/ceiling and starts at the floor; intent `require` raises effective floor/initial route to at least LIGHT.
4. Execution, not the immutable decision, progressively evaluates deficits one route at a time up to the effective ceiling.

Decision reasons are exact: `caller_context_sufficient` iff all capabilities are satisfied, otherwise `caller_context_incomplete`; `atlas_not_requested` iff intent `none`; `explicit_atlas_request` iff intent `require`; light/deep capability reasons iff an unsatisfied/unknown capability has that minimum route; `route_floor_applied` iff effective floor exceeds DIRECT; `starting_generation_unavailable` iff typed unavailable. Ceiling exhaustion is execution-only.

Task kind is evidence for deterministic capability derivation under the versioned classifier/recipe, never a sole route override. File size may be non-canonical diagnostic telemetry but MUST NOT select/escalate a route alone.

### Progressive execution

`context-execution-v2.0.0` is a discriminated response. It retains the V1 execution states, counters, transient fields, and materialization semantics; the successor identity reflects removal of the obsolete Context Packet variant from the closed LIGHT payload union:

- `completed`: decision, ordered attempts, final route, required closed payload, empty deficits, counters, bounded diagnostics;
- `partial`: decision, attempts, final route, required useful payload, non-empty deficits, terminal reason;
- `blocked`: decision, attempts/final route when present, optional payload, non-empty capability or route-failure deficits, terminal reason;
- `interrupted`: decision/completed-attempt metadata, optional prior completed payload, interruption reason, and no timing-dependent semantic partial IR.

Payload is closed: `direct_none`; `light_bundle` containing a deterministic operation-order list of version-pinned `query` or `source_reference` results; or `deep_context_ir` containing IR `2.0.0`. Successful deep returns envelope plus IR.

Capability deficits use the required-capability enum plus `unavailable`, `ambiguous`, `conflicting`, `stale`, `unsupported`, or `semantic_budget_omitted`. Route-failure deficits are `starting_generation_unavailable`, `no_active_generation`, or `generation_changed`. Starting-generation unavailable reasons are closed: `workspace_unregistered`, `catalogue_unavailable`, `no_active_generation`, `observation_failed`; diagnostic text stays outside decision/hash/telemetry.

Execution rules:

1. Run initial route. DIRECT completes only when all capabilities are satisfied; with intent `allow`, incomplete capabilities progress if ceiling permits; intent `none` never escalates.
2. LIGHT executes the minimal deterministic operation list ordered `identity → relationships → impact → coverage/conflicts → source`, omitting unrequested operations. Those results are projections of the two pinned target-bounded contracts, `query-v1.0.0` then `source-reference-v1.0.0`; LIGHT never invokes `context-packet-v1.0.0` and never claims IR sufficiency.
LIGHT never turns task text into an implicit catalogue search. It canonicalizes and deduplicates the request's explicit path/symbol targets, and charges one LIGHT work unit for each target actually examined by each invoked operation. `atlas_calls` counts invoked operations and record/source counters count returned results. A requested LIGHT capability with no usable explicit target yields the existing typed `unsupported` deficit; it does not trigger a repository scan or fabricate work accounting.
3. DIRECT capability deficits under `allow` progress to LIGHT. LIGHT always progresses to DEEP for unresolved deep-minimum capabilities. For light capabilities, only identity ambiguity, relationship/impact/coverage conflict, or semantic-budget omission may progress to DEEP; exact-source stale/unavailable and any unsupported deficit stop.
4. At ceiling, partial requires useful payload/at least one satisfied requested capability; otherwise blocked. Completed requires zero deficits.
5. Missing/unavailable/changed generation blocks Atlas with route-failure deficit and optional no payload; caller resubmits for a new decision.
6. Wall/cancellation interruption returns interrupted; time/cache/retry/transient failure/prior outcomes never reroute.

The execution envelope has no canonical semantic identity. It uses an opaque attempt ID and integrity-links the immutable decision hash and any IR/context/source digest. Deadlines, retries, timings, cancellation, cache, and materialized-byte digests never enter route or IR identity.

## Capability and cost profiles

The request reserves optional capability/cost profile references, but H4 registers/promotes no runtime profiles now. Any future profile is an immutable closed manifest with schema/version, stable ID, canonical digest, direct/Atlas tool access, context-window class, permitted route range, and deterministic semantic-budget ceilings. It MUST NOT contain or derive from model/vendor/product names, account IDs, runtime prices, or hidden per-user history.

Profile resolution precedes decision hashing. Explicit caller floor/ceiling may only narrow the manifest's permitted range; widening or conflicting values fail closed. Explicit semantic budgets may only tighten profile ceilings. The manifest digest participates in decision identity, and any field/rebinding change requires a new profile ID/version. Context IR stays model-neutral: profile/route identity remains outside IR except for the effective semantic budget and estimator identity already represented by deep policy.

## Context IR and H5 boundary

Selective routing does **not** alter H5's schema choice. H5 selects Context IR `2.0.0` because required-role sufficiency, deterministic semantic results/omissions, and verified digest/range references form a new semantic contract—not because the governor exists.

Context IR `2.0.0` is emitted only by `ATLAS_DEEP`. It contains deterministic generation-bound semantic inputs/results/omissions and verified references: SHA-256 whole-file digest plus zero-based, half-open byte ranges over exact file bytes (no newline normalization), with canonical-root/symlink confinement and live observed digest. Selected references are revalidated before sealing; drift yields typed stale/partial/blocked state or a caller restart, never mixed-time completeness.

`context-execution-v2.0.0` is the separately versioned bounded transient envelope defined above. Optional materialized source bytes are live-verified, explicitly authorized, byte-bounded, held only for the response lifetime, and prohibited from task/event/metric/context persistence, SQLite/WAL, logs, or crash diagnostics. Successful deep execution returns envelope plus IR; interruption returns envelope only, with no canonical timing-dependent partial IR/hash.

## Generation, identity, privacy, and telemetry

- Every immutable decision records starting generation as observed ID or typed unavailable state, including DIRECT.
- DIRECT creates no Atlas context identity and need not pin/compile that generation; zero Atlas context remains success only when requested capabilities are caller-satisfied.
- Atlas escalation pins one captured generation; unavailable/change stops the request and requires caller resubmission with a new decision.
- Route decision, Context IR, planner, generation, and opaque execution attempt identities remain separate. Identical deep semantic inputs produce identical IR regardless of initial DEEP versus progressive escalation.
- `planner-v1.1.0` and explicit compiler defaults remain public runtime baseline. H3-A profiles remain experiment inputs. V2 required-role/sufficiency semantics use the fixed successor `planner-v2.0.0`; changing those semantics requires another identity.
- Local telemetry may retain workspace-scoped opaque request/attempt IDs, immutable decision hash, starting-generation state, route/policy/profile versions, attempts/final route, typed deficits/reasons, semantic counters, and terminal state under H2-A limits.
- Telemetry never retains raw task/seed/path text, prompts, source bodies, materialized bytes/digests, model/vendor names, credentials, or arbitrary strings. Predictable identifiers are not persisted merely by hashing them. Any durable field/vocabulary outside existing closed contracts blocks for H7.

## Compatibility, rollback, and migration

- Existing explicit CLI/MCP operations keep behavior; they do not silently route or emit V2. In particular, the explicit legacy `context`/Context Packet behavior remains available unchanged outside the governor. H6B-A0 adds no routing or deprecation to any V1 surface, and T11 uses a separate internal V2 reader/writer entry point and `planner-v2.0.0`.
- Context IR `1.0.0` readers reject `2.0.0`. A V2 reader may explicitly invoke a legacy reader, but legacy documents remain legacy and cannot claim V2 sufficiency.
- T11 freezes the complete V2 field matrix, including fields later populated by T12/T14/T15/T24. Those later tasks may populate only frozen fields; needing a field/vocabulary change blocks for renewed contract/version review.
- T28 established the initial route, capability, and execution fixtures. The repair replaces them with `context-route-v2.0.0` request/decision fixtures, `context-capabilities-v2.0.0` discovery/negotiation fixtures, and `context-execution-v2.0.0` envelope fixtures. Route and capability identities change with the operation set; execution identity changes because its closed LIGHT payload union removes Context Packet. The envelope states, counters, transient fields, and materialization semantics remain unchanged.
- No V2 context/decision/envelope is durably persisted or referenced by lifecycle rows until H7 explicitly approves mixed-catalogue old-binary behavior, cleanup/referential integrity, backup/restore, and no-downgrade evidence. T11/T28 first prove in-memory/application contracts.
- Existing Context Packet schema/ranking policy `context-packet-v1.0.0` remains unchanged for explicit legacy callers but is not a `context-route-v2.0.0` LIGHT operation and is not advertised by `context-capabilities-v2.0.0`. A future bounded packet requires its own reviewed operation/packet and route identities.
- Rollback before durable approval selects explicit legacy surfaces/`planner-v1.1.0` and drops transient V2 data. Truth Plane/generations are untouched. There is no rollback to the superseded unreleased route seam; repair rollback removes the internal governor path while retaining explicit operations. Later durable rollback follows the H7-approved exact plan.
- No migration is approved by this governor specification. DDL, durable format rows, rewriting, or new persistence requires its scoped H7 pre-edit and post-implementation gates.

## Future Agent Skill compatibility

The reviewed Agent Skill architecture is a compatibility requirement, not a milestone or implementation task. The future optional skill teaches an agent **how to cooperate with Atlas**; it never contains repository facts, ranks evidence as truth, replaces the governor, or duplicates Context IR.

- **Discovery:** `context-capabilities-v2.0.0` is a closed allowlisted DTO containing supported route-policy/decision/envelope/IR/operation versions, routes, signals, reasons, deficits, terminal states, bounds, and coarse availability (`available`, `unavailable`, `unknown`). It advertises `query-v1.0.0` and `source-reference-v1.0.0` for LIGHT, never legacy Context Packet. It excludes provider/account/path/repository contents and other operational detail.
- **Negotiation:** callers declare supported Atlas contract versions; no common version fails closed with supported-version metadata. MCP protocol dates remain separate from Atlas contract negotiation.
- **Invocation:** the skill calls only approved public CLI, MCP, or future SDK application semantics. It does not read catalogue/database/internal Rust APIs, bypass verification, or mutate Truth.
- **Transport parity:** one shared discriminated DTO represents DIRECT `direct_none`, LIGHT payload variants, DEEP envelope-plus-IR, starting generation, deficits/reasons/ceilings/errors across transports.
- **Packaging:** the skill is separately versioned/discovered and absent from Context IR, route identity, Cargo/package contents, or project truth. Distribution needs separately reviewed content and H8/release authorization.
- **Identity/privacy:** skill version/invocation is transient metadata unless an approved capability profile affects routing. It does not enter Context IR; prompts/model/vendor names are not retained.
- **Interface freeze:** H6B-A0 freezes transport-neutral public application/CLI/MCP semantics. A future skill adapts through discovery but remains unimplemented and separately gated.

T28 established the transport-neutral decision/capability seam; the pre-release repair replaces its route and discovery identities without a compatibility branch. H6B-A0 freezes the corrected additive surface contract but changes no availability. T11 freezes deep IR, T17 and its repairs integrate application routing, T20 must first prove destructive internals, T18/T19 then prove CLI/MCP parity only after their remaining gates, T21 documents it, and T22 verifies clients.

## Benchmark additions

Freeze accepted-outcome cases for:

1. DIRECT success with generation observation and zero Atlas calls/context bytes;
2. DIRECT success with typed unavailable starting generation;
3. DIRECT → LIGHT identity/source lookup;
4. LIGHT relationships/impact without Context IR;
5. LIGHT → DEEP for each approved semantic deficit;
6. direct DEEP versus escalated DEEP producing the same IR hash;
7. route-ceiling partial/blocked behavior;
8. no-active-generation and caller-resubmitted generation-change behavior;
9. ready-Serving versus Truth fallback equivalence;
10. semantic-budget exhaustion versus non-canonical wall interruption;
11. deterministic 10× fixture growth where original candidates/order remain fixed and added unrelated evidence cannot change the bounded accepted result;
12. capability/cost profile manifests/digests without model/vendor IDs;
13. privacy and supported-version negotiation fixtures for decision/envelope/telemetry/discovery.

Each frozen case declares request DTO, fixture construction, expected initial/final route, reasons/deficits, allowed omissions, terminal state, accepted outcome, and contract versions. Report decision/per-route time, attempts, semantic work, Atlas calls, records, bytes, estimated tokens plus estimator identity, and omissions. Timings/cache are diagnostic; different evidence contracts are not compared as interchangeable.

## CLI, MCP, and future SDK implications

- Keep `find`, `inspect`, `trace`, `impact`, `source`, `context`, and `context-ir` explicit and compatible.
- H6B-A0 fixes additive `atlas governor capabilities|run` and the shared semantic request/application boundary. Separate compiler/lifecycle specs own the exact CLI grammar, bounded reads, and local-only mutations; transports only parse/serialize.
- MCP uses the H6A SDK's closed schema. CLI/MCP and a future SDK expose identical decisions, capability discovery, reasons, generation observation, terminal states, bounds, and envelope semantics.
- DIRECT success has absent Atlas payload, not an error, fake empty IR, or implicit fallback.
- Governor cannot reconcile, rebuild, compact, unregister, mutate source, or start providers.

## Implement now versus defer

Before V2 interface freeze:

- immutable request/decision, closed signals/capabilities/reasons, canonical hash, fixed `context-route-v2.0.0`, and `context-capabilities-v2.0.0`, with no parallel V1 route seam;
- discriminated `context-execution-v2.0.0` payload/deficit/state vocabularies, bounded non-persistent materialization, and rejection fixtures for the superseded V1 envelope;
- deterministic progression, floor/ceiling, starting-generation, resubmission, and source-revalidation fixtures;
- Context IR `2.0.0` complete field matrix and `planner-v2.0.0` deep semantics without changing legacy defaults;
- internal routing over pinned existing operation contracts;
- LIGHT dispatch limited to `query-v1.0.0` and `source-reference-v1.0.0`; legacy Context Packet remains explicit and outside governor dispatch;
- privacy-safe route/stage instrumentation using existing schemas where possible;
- route/capability benchmarks and later CLI/MCP parity fixtures under the H6B-A0 freeze;
- Agent Skill compatibility across T28/T17/T18/T19/T21/T22.

Defer the Agent Skill implementation/packaging until its full reviewed content arrives; learned/probabilistic/latency-racing/per-user routing; H3-A promotion; model/vendor-specific rules; file-size-only routing; every durable V2 change until its H7 gates; surface availability until T20/`H7-C/POST`/T18/T19 complete; and SLO/release/tag/publication/visibility work until H8 plus separate execution authorization.

## RED–GREEN–REFACTOR acceptance

**RED**

- zero-context success or starting-generation unavailability cannot be represented;
- initial decision and finalized execution outcome are conflated;
- timing/cache/file size can alter routing or hashes;
- direct-deep and escalated-deep cannot prove identical output;
- telemetry/envelope can persist raw task/seed/source/model/vendor detail;
- capability/version negotiation is transport-specific or would force a skill to read catalogue/repository facts.
- LIGHT routing can invoke or advertise legacy Context Packet, or can scan work not bounded by explicit targets;

**GREEN**

- identical request inputs produce one immutable decision hash/initial route/reasons; identical deterministic evidence produces one envelope route sequence/deficit result;
- every floor/ceiling/payload has typed complete/partial/blocked/interrupted behavior;
- every decision records starting generation or stable unavailability without requiring Atlas context;
- only deep emits Context IR `2.0.0`, paired with an envelope on success;
- route benchmarks preserve declared accepted outcomes and expected reasons;
- learned/model/vendor/transient inputs stay outside semantic identity;
- capability discovery/version negotiation and route DTOs are transport-neutral and future-skill-safe;
- LIGHT dispatch and discovery contain only the two target-bounded operation contracts; explicit legacy Context Packet behavior is unchanged outside the governor;
- V2 durable persistence remains stopped at H7.

**REFACTOR**

Keep pure decision separate from dispatch/transports; reuse query, broker, compiler, Serving fallback, telemetry, and source verification; keep one vocabulary and envelope.

## Success criteria

1. Work can finish at DIRECT with zero Atlas context and an explicit generation observation state.
2. Light requests avoid mandatory Context IR compilation.
3. Deep runs only for disclosed requirements/deficits and stays model-neutral.
4. Route, planner, IR, generation, telemetry, and execution identities do not overlap.
5. Future Agent Skill discovery/invocation remains transport-neutral, separately packaged, and unable to become project truth.
6. Any future adaptive proposal replaces fixed route-policy identity and passes reproducible accepted-outcome, privacy, compatibility, rollback, and human-review gates; none is implemented now.
