# Decision Checkpoint: H1–H8-E0 and pre-release G1/G2 direction

## Status

| Gate | Status | Authority | Effect |
|---|---|---|---|
| **H1 — scope/map** | **Accepted — 2026-09-02** | Project owner in the controlling conversation | The stable module map, dependency direction, V1.5/V2.0 boundaries, and no-runtime-learning interpretation may be used for downstream planning. |
| **H2 — privacy/config** | **H2-A accepted — 2026-09-02** | Project owner in the controlling conversation | T01/T02 implementation is complete at this checkpoint; later privacy/public-surface gates remain separate. |
| **H3 — benchmark profiles** | **H3-A accepted — 2026-09-02** | Project owner in the controlling conversation | T26 and measurement work use private acceptance budgets; H4 promotes none and creates no public SLO. |
| **H4 — planner/profile policy** | **Accepted: retain current policy — 2026-09-02** | Project owner in the controlling conversation | Keep public `planner-v1.1.0`/explicit defaults; no H3-A promotion. V2 required-role/sufficiency uses fixed `planner-v2.0.0`; later changes require new identity. |
| **H5 — Context contract** | **Accepted: Context IR `2.0.0` — 2026-09-02** | Project owner in the controlling conversation | T28 then T11 may proceed with in-memory/application contracts; durable V2 use remains H7-gated. |
| **H6A — MCP protocol/client/schema** | **H6A-A accepted — 2026-09-02** | Project owner in the controlling conversation | T27/T03 implementation is complete at this checkpoint. T22 remains separately gated. |
| **H6B — additive compiler/lifecycle surfaces** | **H6B-A0 accepted — 2026-09-03** | Project owner in the coordinator decision gate | Freezes the corrected local-CLI/read-only-MCP contract below and authorizes T20 only in disposable/test catalogues; capability availability is unchanged. |
| **H7-C/PRE — file classification identity** | **Scoped pre-edit direction accepted — 2026-09-03** | Project owner in the controlling conversation | Authorizes only the reviewed G1 implementation/review increment; `H7-C/POST` remains required before live catalogue use. |
| **G2 — governor LIGHT operation set** | **Accepted — 2026-09-03** | Project owner in the controlling conversation | Replace the unreleased route/discovery/execution payload seams with their `v2.0.0` identities; governor LIGHT excludes legacy Context Packet. |
| **H8-E0 — pre-H8 evidence closure** | **Accepted — 2026-09-05** | Project owner in the controlling conversation (`Approve A-C`) | Bounded pre-authorization for bundle A–C only; final H8 remains unsatisfied and bundle D remains withheld. |
| **V2.0 product-outcome reset** | **Accepted — 2026-09-10** | Project owner in the controlling conversation | Pre-v2.0 evidence is limited to the owner-observable product outcome and one bounded representative capacity smoke; T34 hosted execution and the T35/T36 campaign/reviews move to post-v2.0 qualification. |

This checkpoint records decisions only. It changes no production behavior, schema, Cargo metadata, package membership, catalogue, repository visibility, branch authority, tag, release, or artifact.

The owner selected H2-A, H3-A, H4 retain-current, H5 Context IR `2.0.0`, H6A-A, and H6B-A0 without amendments, accepted the scoped pre-release G1/G2 repair direction, and on 2026-09-05 approved H8-E0 bundle A–C. These decisions authorize only their bounded dependent work; they do not approve final H8, unrelated H7 persistence, publication, or release actions.

## H8-E0 — Pre-H8 evidence-closure pre-authorization

**Accepted — 2026-09-05.** The project owner said `Approve A-C` for the bounded preparation packet in `target/rp2/h8/evidence-closure-preparation/decision-packet.md`. H8-E0 records a pre-authorization, not H8 acceptance: **Final H8 remains unsatisfied** until the dependency-ordered work and fresh review in [`implementation-plan.md`](implementation-plan.md) produce accepted evidence. Authority is bundle A–C only.

The approved candidate hypothesis requires correctness and deterministic identity on runner-default Windows, Ubuntu, and macOS while recording actual architecture, runner image, CPU, and resolved toolchain. Rust `1.94` and `stable` are required. Latency becomes enforced only on later selected, dedicated/pinned Windows x64 and Linux x64 runners after the H3-A baseline exists; runner-default macOS latency remains advisory. Exact Inspector `2.5.0` and Codex CLI `0.151.0` clients are required. The built-in provider and `scip-typescript` `0.4.0` are required; `rust-analyzer` `1.94.1` is optional/degraded until it completes semantically, and timeout, unsupported output, or absence is a recorded non-success rather than zero-cost success.

The approved private fixture and memory hypotheses are normative in [`../release-readiness/capacity.md`](../release-readiness/capacity.md#h8-e0-approved-private-candidate-hypotheses): `128`, `1,024`, and `8,192` eligible/indexed files; deterministic `2`, `11`, and `82`-file incremental sets; exact manifest, digest, distribution, provider, correctness, and ready-versus-Truth fields; and 50 ms platform-native process-tree sampling with concurrent Atlas/provider/aggregate peaks, baseline overhead, cache/Serving/provider labels, raw private evidence, and explicit cross-platform comparability limitations. These private labels are not supported limits or public SLOs. This authority adds no Atlas runtime RSS/page-fault schema and no EXP-DPPM behavior.

For the V2.0 candidate, response-lifetime progressive execution and ATLAS_DEEP Context IR `2.0.0` are required. The same cutover includes bounded, explicit, CLI-only opt-in source materialization; MCP materialization remains prohibited. Durable V2 lookup or persistence is not required: V2 lookup continues to return `durable_contract_unavailable`, and any durable V2 lifecycle remains behind a separate H7 gate. DIRECT and target-bounded LIGHT remain valid; DEEP is never mandatory for every request.

The first hosted raw-evidence retention method is bounded redacted private Actions log emission through the two existing pull-request workflows, with no new upload-artifact dependency. If complete evidence cannot fit the reviewed log bounds, execution stops for a separate private artifact-upload decision; it must not truncate evidence or silently change retention method.

Bundle D remains withheld. H8-E0 grants no merge, tag, release, public artifact, publication, visibility change, protected-branch action, supported-limit or SLO claim, distribution channel, external-adopter contact, support/security contact identity, SBOM/signing/provenance/checksum service, durable V2 persistence, runtime learning, EXP-DPPM implementation, or Ponytail adoption.

## V2.0 product-outcome reset

**Accepted — 2026-09-10.** The owner supersedes the prior pre-v2.0 T34–T36 sequencing and the hosted/capacity prerequisites in the former definition of done. T34 private-PR/hosted execution, the 135,000-observation four-shard campaign, and statistical/H8 capacity review are post-v2.0 qualification unless a direct release-critical requirement cannot be proved with smaller local evidence. The prior T34 record, quarantined partial campaign, and all qualification tooling remain retained history; this ruling accepts none of them, resumes no shard, and leaves `bin/atlas-bench.rs` unchanged.

Pre-v2.0 evidence is limited to Truth Plane correctness and non-weakening; architecture and security; fail-closed stale/error behavior; persistence and fresh-session recovery; normal OMP-to-Atlas end-to-end use; one bounded representative performance/capacity smoke; and focused regressions for behavior changed. Every retained test must name the exact requirement it protects. Benchmark refactors, dashboards, speculative abstractions, and reopening accepted work without contradictory runtime evidence are outside this boundary.

This reset changes sequencing and acceptance scope only. Its pre-v2.0 evidence list is exhaustive and forbids GitHub or hosted execution. It does not delete qualification tooling or evidence, accept the current implementation or prior T34 work, authorize release/publication, weaken the Truth/Serving or CLI/MCP contracts, or grant any withheld bundle-D authority.

## H1 decision record

The owner approved [`capability-map.md`](capability-map.md) as the stable decomposition for the combined initiative. H1 therefore resolves:

1. module IDs and dependency direction are accepted;
2. V1.5 means measured, accepted-outcome-gated evaluation and human-reviewed deterministic policy changes—not runtime learned ranking;
3. V2.0 completes the typed, deterministic, bounded Context IR compiler without replacing source authority;
4. local-first, provider-neutral, verified-source, explicit-uncertainty, and no-hidden-fallback invariants remain mandatory;
5. no implementation dispatch may bypass the remaining H2/H3/H6A gates shown in the DAG.

H1 does **not** approve a privacy policy, performance promise, MCP compatibility set, migration, public interface, Cargo version, or release action.

## H2 — Privacy/config recommendation

### Common H2 invariants

All selectable H2 options reject credential-bearing config values from persisted snapshots/metadata, never persist raw task text or source bodies, use a closed typed event-detail allowlist, and perform privacy deletion transactionally without creating a copy or backup containing deleted/expired data. Differences below concern pattern/config UX, telemetry enablement, and bounded structured-metadata retention.

### Accepted H2-A — Registered strict policy

- **Pattern language:** keep Rust regular expressions, matching current implementation direction. Compile all patterns once at configuration load; any invalid expression rejects the whole config with field/index detail. Correct the shipped examples to valid expressions and include `.key` in built-in secret defaults.
- **`.gitignore`:** explicitly do not honor it in the first stable public contract. Atlas policy remains self-contained and auditable; adding VCS-ignore semantics later requires a separate precedence/compatibility decision.
- **Config continuity:** `init --config` creates an atomic application-owned normalized snapshot in the per-workspace application-data area, records its content hash, and applies owner-only filesystem permissions/ACLs. Credential-bearing values are rejected from the snapshot; external provider credentials remain operator-managed environment/credential references, never copied values. Reconcile uses the registered snapshot by default. A caller-supplied differing config fails with `configuration_mismatch`; replacing the policy requires an explicit later H6B-approved rotation operation.
- **Privacy defaults:** raw task text persistence is disabled for the first stable public contract, including opt-in; task telemetry never persists raw source bodies. Event details become a closed typed allowlist, remain capped at the existing 4,096 bytes, reject prompt/source snippets and credential-bearing keys/values, and hash/normalize identifiers where exact text is unnecessary.
- **Retention/compaction:** completed structured session/events/contexts/metrics retain for 30 days or 1,000 completed sessions per workspace, whichever is tighter; hash-only outcome summaries retain for 180 days. An incomplete session with no event/renewal for 24 hours becomes stale; on the next Atlas interaction it is marked abandoned unless the caller explicitly renews it, then follows completed retention. Lazy caller-driven privacy compaction removes at most 100 expired sessions per invocation in a rollback-before-commit transaction, creates no backup containing expired data, and never removes Truth Plane evidence or retained generations.
- **Why recommended:** smallest behavior change from current regex/config-hash architecture; fail-closed privacy; repeatable reconcile; no dependency on Git-specific ignore precedence; bounded local data lifetime and no raw prompt/source channel.

### Option H2-B — VCS-familiar policy

- **Pattern/precedence:** use standard gitignore-style globs from root and nested `.gitignore` files. Precedence is immutable secret/self exclusions → explicit Atlas exclude → explicit Atlas include → `.gitignore` → included by default; no rule may re-include immutable safety exclusions.
- **Config continuity:** persist an application-owned normalized owner-only snapshot and hash; reject credential-bearing values; reconcile uses it by default and a differing caller config fails `configuration_mismatch`.
- **Retention:** no raw task/source; typed event details; structured sessions retain 90 days or 5,000 completed sessions, hash-only summaries 365 days, stale/renewal threshold 24 hours, and transactional caller-driven compaction removes at most 100 expired sessions per invocation without backup.
- **Tradeoff:** familiar onboarding, but a new pattern engine and multi-source precedence create migration, portability, and privacy-audit complexity.

### Option H2-C — Stateless explicit policy

- **Pattern/config:** keep fail-closed regex, do not honor `.gitignore`, and persist only the approved config hash—not a config snapshot. After custom-config init, every reconcile requires `--config`; omission is `configuration_required`, and hash mismatch is `configuration_mismatch`.
- **Telemetry/retention:** task-use telemetry is disabled by default. An explicitly enabled session still stores no raw task/source and uses typed details; structured rows retain seven days or 100 completed sessions, hash-only summaries 30 days, stale/renewal threshold four hours, and transactional caller-driven compaction removes at most 100 expired sessions per invocation without backup.
- **Tradeoff:** minimal persisted policy/data, but poor unattended operation and reduced V1.5 measurement coverage.

### H2 decision record

The owner accepted `H2-A` on 2026-09-02 with every value and invariant above unchanged.

## H3 — Benchmark/profile recommendation

### Accepted H3-A — Pinned profile budgets with staged enforcement

**Profile candidates**

| Profile | Records | Source bytes | Estimated tokens | Relationship depth | Deterministic work units | Uncertainty reserve | Ready-serving p95 target | Wall cancellation |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `small` | 30 | 12,000 | 4,000 | 1 | 5,000 | 15% | 150 ms | 1,000 ms |
| `standard` | 60 | 40,000 | 8,000 | 2 | 20,000 | 12% | 250 ms | 5,000 ms |
| `audit` | 250 | 120,000 | 24,000 | 4 | 100,000 | 15% | 2,000 ms | 10,000 ms |

- **Semantic limits:** one deterministic work unit is one seed-candidate examination, relationship-edge examination, or record-packing attempt. Record/source/token/depth/work-unit limits own canonical partial IR and participate in policy identity. Wall time is only cancellation; expiry returns a typed non-canonical interrupted envelope without a Context IR hash.
- **Scenario and attestation:** committed `tests/fixtures/context_yield/benchmark-scenario.json` pins stable dataset/procedure/profile/acceptance/estimator requirements, not HEAD. Every run generates a package-excluded attestation containing actual revision/toolchain/platform/CPU, fixture/tree/generation/provider/Serving/cache state, raw samples, and percentile method; acceptance validates attestation HEAD against the tested checkout.
- **Runners:** enforce correctness/determinism on Windows, Linux, and macOS; enforce latency on dedicated pinned Windows x64 and Linux x64 runners; record macOS latency as advisory until a stable dedicated runner exists.
- **Samples:** five unscored warmups, 100 scored warm samples per profile/condition, and 20 fresh-fixture/catalogue cold samples. Run three independent repetitions. Enforce nearest-rank warm p50/p95; keep cold percentiles advisory until retained evidence supports a separate gate.
- **Gate:** hard fail correctness, deterministic identity, bounds, or prohibited hot-path work. Fail latency only when a profile ceiling or regression boundary is breached in at least two of three runs. In milliseconds: `candidate_p95 - baseline_p95 > max(0.15 × baseline_p95, 2 × baseline_MAD_ms)`. Establish the baseline from three accepted scenario/attestation runs before enforcing regression deltas.
- **Statistics:** each independent repetition yields one warm p95 from its 100 scored samples. Per runner/profile/condition, `baseline_p95` is the median of three accepted baseline-repetition p95 values; `baseline_MAD_ms` is the median absolute deviation of those same three p95 values. For three candidate repetitions, count `absolute_breach = candidate_p95 > profile_ceiling` and `relative_breach = candidate_p95 - baseline_p95 > max(0.15 × baseline_p95, 2 × baseline_MAD_ms)`. H3-A fails when either breach is true in at least two of three candidate repetitions.
- **Claims:** profile ceilings are private acceptance budgets, not public SLOs. Golden/model-side results remain separate from compiler/bootstrap timing and require equal accepted outcomes.
- **Why recommended:** concrete release discipline without pretending one workstation sample is a universal promise; stable semantic determinism despite wall-clock noise.

### Option H3-B — Relative-only performance gate

- Inherit every H3-A profile, work-unit definition, scenario/attestation rule, runner, warm/cold sample, percentile, and three-repetition parameter. Enforce correctness/bounds and fail only when `relative_breach` is true in at least two of three candidate repetitions; absolute 150/250/2,000 ms ceilings remain advisory.
- **Tradeoff:** least flaky during early optimization, but weaker capacity expectations for a stable public release.

### Option H3-C — Strict cross-platform absolute gate

- Inherit every H3-A parameter, but enforce the absolute profile ceilings on Windows, Linux, and macOS hosted runners immediately; enforce the same relative rule after the three-run baseline exists. Either absolute or relative breach in at least two of three repetitions fails that OS/profile condition.
- **Tradeoff:** strongest headline discipline, but hosted-runner variance can block correct changes and makes results less reproducible than H3-A.

### H3 decision record

The owner accepted `H3-A` on 2026-09-02 with every profile, sample, runner, attestation, statistical, and private-claim boundary above unchanged.

## H6A — MCP protocol/client/schema recommendation

The current adapter must stop returning Atlas capability `1.3` as MCP `protocolVersion`. The audit’s `2025-11-25` initialization finding remains valid for legacy clients; the current stable `2026-07-28` protocol uses per-request version metadata and `server/discover`. MCP protocol dates and Atlas schema/capability versions remain independent. Official sources:

- https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
- https://modelcontextprotocol.io/specification/2025-11-25/server/tools
- https://github.com/modelcontextprotocol/rust-sdk (workspace release `3.2.0`, Rust `1.88+`, current `2026-07-28` plus `2025-11-25` compatibility at this checkpoint)

### Common H6A client strategy

T03 always proves the real spawned binary’s selected lifecycle/version/schema behavior without external clients. T22 pins exact versions and automates only clients with documented deterministic headless support: official MCP Inspector CLI and Codex CLI. Claude Desktop/Claude Code is recorded manual compatibility evidence unless a deterministic headless path is verified; it is not assumed to be a blocking automated gate. Every automated client case covers discovery/initialize as applicable, tool listing, representative valid call, malformed/unknown argument rejection, and clean stdin/EOF shutdown.

### Accepted H6A-A — Official dual-era SDK boundary

- **Protocol set:** support exactly modern MCP `2026-07-28` and legacy `2025-11-25` over stdio. Modern requests use per-request version metadata and `server/discover`; legacy clients use `initialize` → `notifications/initialized`. Unsupported modern versions receive `UnsupportedProtocolVersionError` listing both dates.
- **Dependency:** add official `rmcp = 3.2.0` with `default-features = false` and features `server`, `transport-io`, and `schemars`; its Rust `1.88` floor is below Atlas `1.94`. A separate exact Cargo/adapter boundary task must land before T03 and review all lockfile/transitive changes.
- **Schemas:** authoritative typed tool-input structs derive JSON Schema 2020-12 through the SDK/schemars path; the 13 existing tools use exact properties/required/types/enums/bounds and closed unknown-field behavior. Contract tests prove discovery schemas match application parsing. Add `outputSchema` only for authoritative stable result types.
- **Architecture:** SDK owns transport/lifecycle/version mechanics; existing shared application functions retain all Atlas truth, selection, persistence, and source-verification semantics. Retain stdio only.
- **Why recommended:** current stable protocol plus legacy audit compatibility, maintained official conformance machinery, exact schema generation, and best eventual public-client interoperability. Cost: reviewed async/dependency/lockfile integration before T03.

### Option H6A-B — Static legacy-only adapter

- **Protocol set:** support exactly legacy `2025-11-25`; require `initialize` and `notifications/initialized`, echo the requested supported date, and return the supported legacy date for an unsupported initialize request. Retain stdio and existing dependencies only.
- **Schemas:** one central static Rust `ToolDefinition` table using existing `serde_json`, with exact closed schemas and parser/schema parity tests for all 13 tools.
- **Client strategy:** apply the common T22 strategy only to clients that explicitly support `2025-11-25`; modern-only clients are documented unsupported.
- **Tradeoff:** smallest code/supply-chain change, but deliberately omits the current stable modern protocol and weakens eventual public compatibility.

### Option H6A-C — Static dual-era adapter

- **Protocol set:** implement exactly modern `2026-07-28` plus legacy `2025-11-25` over stdio without an SDK, including `server/discover`, per-request modern metadata/errors, and the full legacy initialize state machine.
- **Schemas/client strategy:** use the H6A-B static closed schema authority and the complete common T22 dual-era client matrix.
- **Tradeoff:** no new dependency, but Atlas owns two lifecycle eras, negotiation, schemas, and future protocol maintenance; highest bespoke-conformance risk.

### H6A decision record

The owner accepted `H6A-A` on 2026-09-02 with the exact dependency, feature, protocol-date, schema, transport, and T22 separation above unchanged.

## H4 — Planner/profile policy decision

The owner reviewed the V1.5 evidence and accepted the conservative H4 outcome on 2026-09-02:

1. retain `planner-v1.1.0` and the current explicit compiler defaults as the runtime baseline;
2. promote none of the H3-A `small`/`standard`/`audit` experiment profiles into runtime selection;
3. add no adaptive, learned, probabilistic, latency-racing, or outcome-fed runtime selection;
4. require a new fixed policy identity before changed ordering, required roles, sufficiency, or route semantics emit results: `planner-v2.0.0` for reviewed V2 role/sufficiency, `context-route-v1.0.0` for the initial governor contract, and successors for later changes; a route change that also changes deep semantic selection updates both;
5. keep old policy output selectable/readable for rollback until a later compatibility cutover is approved.

H3-A remains the private benchmark/acceptance discipline. It does not itself authorize a runtime profile, default, or public SLO. A future policy proposal needs equal accepted outcomes, frozen inputs, deterministic results, privacy and compatibility review, rollback, and a new human decision.

## H5 — Context contract decision

The owner accepted Context IR `2.0.0` as a deterministic, generation-bound semantic contract. It carries verified digest/range references plus deterministic semantic budgets, selected results, typed sufficiency, uncertainty, coverage, and omissions. It excludes source bodies, prompts, wall deadlines, measured time, retries, cancellation/interruption, cache/Serving-hit state, route attempts, and other transient execution state.

H5 initially fixed transient materialization and execution under bounded `context-execution-v1.0.0`. That envelope may carry live-verified materialized bytes, route decisions, attempts, timings/counters, deadline/retry/cancellation/interruption, and cache observations, but it never gives timing-dependent partial contents a canonical Context IR hash or completeness claim. The later G2 record below explains why the closed payload change advances only this envelope identity.

Selective routing does not cause or weaken the schema decision. Context IR `2.0.0` is required by changed semantics and emitted only for `ATLAS_DEEP`; DIRECT may succeed with zero Atlas context, and LIGHT returns bounded typed payloads without IR sufficiency. T11 proceeds with the complete V2 field matrix and fixed successor `planner-v2.0.0` for changed required-role/sufficiency semantics. Public `planner-v1.1.0`/explicit defaults remain unchanged; no H3-A profile or mandatory deep route is introduced.

Compatibility is explicit: a `1.0.0` reader rejects `2.0.0`; a V2 reader may explicitly read legacy without overclaiming; new V2 writes do not rewrite legacy. T11/T28 establish in-memory/application contracts only. No V2 durable rows or lifecycle references may land until H7 approves mixed-catalogue old-binary behavior, cleanup/referential integrity, backup/restore, and rollback. Pre-H7 rollback drops transient V2 data and selects legacy surfaces/`planner-v1.1.0`, leaving Truth untouched.

## V2 Context Governor architectural constraint

Before V2 interfaces freeze, the Context Plane adds the deterministic governor specified in [`SPEC-context-governor.md`](SPEC-context-governor.md). It evaluates `DIRECT` → `ATLAS_LIGHT` → `ATLAS_DEEP` progressively; zero Atlas context is valid success; full Context IR compilation is never mandatory. It does not route on file size alone, may use disclosed versioned capability/cost profiles without model/vendor names, keeps Context IR model-neutral, and leaves Truth Plane untouched.

`ContextRouteDecision` is the immutable pre-dispatch seam. T28 initially implemented `context-route-v1.0.0`; the accepted G2 repair supersedes that unreleased seam with `context-route-v2.0.0`, `context-capabilities-v2.0.0`, and `context-execution-v2.0.0`, without parallel readers/writers or shims. The execution state machine, counters, transient fields, and materialization semantics are unchanged, but its closed LIGHT payload union removes the obsolete Context Packet variant and therefore requires a successor envelope identity. DIRECT records observed generation or typed unavailability without pinning/compiling or creating a context identity. No learned routing is approved; any later decision or envelope semantic change requires its own successor identity.

The reviewed future Agent Skill architecture is a compatibility requirement, not a new milestone. The skill will teach procedural cooperation with Atlas and remain separately versioned/packaged; it will not contain repository facts or become project truth. T28/T17/T18/T19 must keep capability discovery and DIRECT/LIGHT/DEEP semantics transport-neutral across CLI, MCP, and any future SDK. H6B-A0 now freezes the public application/CLI/MCP contract below; actual skill implementation still waits for separately provided reviewed content and authority.

## H6B-A0 — corrected additive surface contract

The owner selected H6B-A0 without amendment in the coordinator decision gate:

> **H6B-A0 accepted.** Approve the corrected additive CLI grammar and invariants in `target/reports/h6b-decision-review.md`: every new workspace command uses explicit optional catalogue selection; `governor capabilities|run` uses one shared semantic request/application builder with separate Atlas version negotiation and explicit V2 budgets; task lifecycle remains legacy-only with exact legal transitions and idempotent/conflicting replay; all growing reads and previews use bounded restart-stable non-authority cursors; privacy compaction and unregister use complete manifest digests, explicit literal confirmation, active-writer exclusion, and fail-closed recomputation. Remove first-contract caller-path Context Yield export and Atlas-created unregister backup; bounded output is stdout-only and backup remains a separately verified caller-owned operation. Privacy compaction never deletes Truth or generation history; irreversible unregister explicitly may delete the complete Atlas catalogue, including indexed Truth and generation history, but never workspace source, and remains unavailable unless T20 proves an exclusion protocol spanning SQLite close and Windows file removal. MCP adds only the six V2 tools `atlas_governor_capabilities`, `atlas_governor_run`, `atlas_task_show`, `atlas_compiled_context_show`, `atlas_context_yield_show`, and `atlas_serving_status`; those six expose no source materialization, task/source/lifecycle mutation, retention/unregister, serving build/rebuild, backup/export, caller-path or other filesystem write, or destructive authority. Preserve all 13 existing V1 MCP tools unchanged, including `atlas_serving_build`, the existing derived-Serving projection operation that does not mutate workspace source or committed Truth; preserve all V1 CLI behavior without silent routing or deprecation; create no runtime profile/default and no durable V2 state. This decision authorizes T20 implementation only in disposable/test catalogues; T18 remains blocked on T20 and H7-C/POST and must first receive an approved application/lifecycle ownership split, while T19, T21, T22, T23, T25, H8, live use, release, publication, and every successor remain gated by their exact predecessors and separate authority.

The exact grammar, shared `GovernorRunRequest`, bounds, lifecycle transitions, manifest/exclusion protocol, and MCP boundary are normative in [`SPEC-compiler-surfaces.md`](SPEC-compiler-surfaces.md) and [`SPEC-context-governor.md`](SPEC-context-governor.md). Every new workspace command has optional `--catalogue`; request documents are capped at 1 MiB before allocation/decoding; the two raw target-vector lengths are checked-added and every raw target is validated before cloning, sorting, or deduplication; collection limits default to 50 and are `1..=200`; cursor wire size is at most 4 KiB; and every nested vector/string/blob has an advertised hard bound. Cursors remain restart-stable opaque canonical tokens bound to the resolved catalogue/workspace, operation, contract, filter, captured state, ordering key, and checksum; they confer no authority. The minimal public and durable legacy abandon-reason vocabulary is exactly `user_requested` for an explicit task-abandon request and `inactivity_timeout` for the accepted 24-hour lazy stale-session abandonment path. `superseded`, `no_longer_needed`, aliases, fallbacks, and all other codes are excluded; T18B must reject unknown codes before any CLI exposure.

The accepted grammar also requires a deterministic legacy reachability interpretation. A supplied legacy session is fail-closed unless the application validates its exact workspace, `1.0.0` contract, task/normalized-goal/classification identity, nonterminal state, and applicable start generation. Successful explicitly bound acquisition is the activation boundary: completed DIRECT `direct_none` intentionally satisfies acquisition without fabricating IR; LIGHT/DEEP activates only for completed or useful partial output; blocked/interrupted/error/no-useful-payload does not; non-blocked sealed legacy Context IR requires its delivery events to persist; attributed source/query preserves its exact result/event behavior. Candidate activation reconciles all and only older-generation active legacy sessions for that workspace, ordered by session ID, in the same immediate transaction as candidate commit and active-pointer change. A post-commit session update permits crash-split state, a session selector changes the frozen grammar, changed-file overlap falsely implies causality, and test-only transition injection cannot establish public reachability. This clarifies H6B-A0; it neither adds a release nor completes V2.0.


Selection records a contract only. It does not advertise a tool, expose a command, enable governor capabilities, authorize live schema-4 catalogue use, or create durable V2 state. T20 is the only newly authorized implementation task and must prove the external per-catalogue lock/quarantine/fault boundary and truthful whole-catalogue unregister deletion on disposable/test catalogues. T18 remains blocked until T20 and separately owner-approved `H7-C/POST` complete, and its shared application/lifecycle ownership slice must precede CLI parsing. T19 follows T18; T21/T22 follow T19; T23 follows T21/T22 and its existing fan-in; T25 follows T23; H8 follows T25 and remains review-only.

## Pre-release repair direction — G1/G2 and `H7-C/PRE`

After the G1/G2 choices and consequences were explained, the owner stated:

> \"atlas has not been released yet - the project is currently private - changes will only affect this system - if changes deliver an upgrade it's something i can live with\"

This accepts local compatibility upgrades needed for the scoped G1/G2 repairs. It is not release approval, data-deletion permission, public-interface approval, or blanket authorization for H6B, H7, or H8. The design therefore removes defective unreleased internal compatibility instead of maintaining it for hypothetical external consumers, while preserving actual source, catalogue history, and explicit legacy commands.

### G1 — producer classification and immutable revision identity

The exact contract is [`SPEC-file-classification-identity.md`](SPEC-file-classification-identity.md):

1. discovery owns minimal Rust integration-test classification under `file-classification-v2.0.0`: a normalized repository-relative `.rs` path is a test only when a directory segment is exactly `tests`;
2. matching paths emit `artifact_class = 'test'` and `is_test = 1`; non-matching paths retain the existing extension classification and `is_test = 0`; no body parsing, general classifier framework, or other-language promise is added;
3. `file-revision-identity-v2.0.0` hashes the full content hash plus every material classification field and is guarded by forward-only migration `0004_file_classification_identity`, catalogue schema `1.3.0`, and `PRAGMA user_version = 4`;
4. old rows and historical generation references are never rewritten. Exact six-field matches are reused; a formerly source-classified test path receives a distinct V2 revision while its old row remains readable;
5. a maximum-version-3 binary refuses version `4`. Migration body/header atomicity, fault rollback, explicit user-owned backup/restore, and old-catalogue reuse must pass focused review.

This records classification-only `H7-C/PRE`. Implementation may proceed in the five exact files frozen by the linked spec, followed by fresh review. No migration may be applied to a live catalogue until explicit `H7-C/POST` owner approval; all unrelated V2 persistence remains prohibited.

### G2 — target-bounded LIGHT without legacy Context Packet

The current normative contract is [`SPEC-context-governor.md`](SPEC-context-governor.md):

1. `context-route-v2.0.0` replaces the unreleased `context-route-v1.0.0` seam without a parallel implementation, compatibility alias, or shim;
2. governor LIGHT permits only `query-v1.0.0` and `source-reference-v1.0.0` over explicit targets. It neither invokes nor advertises `context-packet-v1.0.0`;
3. capability discovery becomes `context-capabilities-v2.0.0` because the advertised route/operation set changes;
4. the closed LIGHT payload union removes Context Packet, so the clean unreleased cutover uses `context-execution-v2.0.0` even though its states, counters, transient fields, and materialization semantics are unchanged;
5. the existing explicit legacy `context` command and Context Packet behavior remain unchanged outside the governor;
6. DIRECT zero Atlas context and progressive escalation remain intact. Context IR `2.0.0` and `planner-v2.0.0` remain unchanged because G2 does not change deep selection or IR shape.

G2 selects only the internal LIGHT operation-set subset. H6B-A0 now owns the final additive CLI/MCP names, bounds, lifecycle, and destructive-authority contract; its selection changes no capability availability. H8 and separate authorization still own adoption, publication, and release.

## Next implementation boundary

H8-E0 bundle A–C is resolved. The sole next step is fresh independent T29 review; only its acceptance unlocks the two bounded implementation chains frozen in [`implementation-plan.md`](implementation-plan.md#t30a--capacity-fixture-generator-and-immutable-manifests). Final H8, unrelated H7 persistence, merge, tag, release, publication/artifact sharing, visibility/protected-branch change, supported-limit/SLO claims, and every other bundle-D value remain closed.
