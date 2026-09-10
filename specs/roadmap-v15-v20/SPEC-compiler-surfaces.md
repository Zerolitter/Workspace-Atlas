# Spec: `compiler-surfaces`

## Objective

Expose the deterministic Context Governor, deep-only Project Context Compiler, and lifecycle through stable application, CLI, and conforming MCP contracts without duplicating policy or changing existing explicit behavior unexpectedly.

## Current-state delta

The base exposes conforming explicit `find`/`inspect`/`trace`/`impact`/`source`, legacy `context`, `context-ir`, `serving-build`, `generation-delta`, and `temporal` CLI/MCP paths. Missing surfaces are governor/capability negotiation, lifecycle/show, Context Yield, serving status, retention, and coherent workflow.

## Interface design

### Canonical application layer

Shared application functions own operations/governor dispatch. CLI/MCP/future SDK only validate/serialize the same discriminated DTOs and negotiate Atlas contract versions separately from MCP protocol. No transport or future skill owns policy.

### Existing compatibility

Preserve existing explicit command/tool names, arguments, JSON envelopes, and errors. Context IR `1.0.0` remains a legacy explicit contract; `2.0.0` is an explicit deep-only successor, never silent reinterpretation. Existing users need not adopt governor/lifecycle operations.

### H6B-A0 coherent workflow and authority boundary

The owner accepted H6B-A0. Every new workspace command accepts optional explicit `--catalogue <path>` and resolves it through the canonical workspace/catalogue validation path; the resolved identity participates in cursors and destructive manifests. The exact additive CLI grammar is:

```text
atlas governor capabilities <workspace-root> [--catalogue <path>]
atlas governor run <workspace-root> --request <json-file|-> [--catalogue <path>] [--materialize-source --max-materialized-bytes <n>]

atlas task start <workspace-root> --request <json-file|-> [--catalogue <path>]
atlas task show <workspace-root> <session-id> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas task complete <workspace-root> <session-id> --request <json-file|-> [--catalogue <path>]
atlas task abandon <workspace-root> <session-id> --reason-code <closed-code> [--catalogue <path>]

atlas compiled-context show <workspace-root> (--context-id <id>|--session-id <id>) [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas context-yield show <workspace-root> <session-id> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas serving-status <workspace-root> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]

atlas retention status <workspace-root> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas retention compact <workspace-root> --dry-run [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas retention compact <workspace-root> --confirm-manifest <sha256> --confirm-privacy-deletion [--catalogue <path>]
atlas unregister <workspace-root> --dry-run [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas unregister <workspace-root> --confirm-manifest <sha256> --irreversible [--catalogue <path>]
```

Dry-run and apply flags are mutually exclusive. First-contract output is bounded JSON on stdout only: no Context Yield caller-path export, Atlas-created unregister backup, or other new caller-selected filesystem write. A backup, if desired, is independently performed and verified by the caller after writers stop.

The shared application layer owns one `GovernorRunRequest` builder for CLI and MCP. The transport-neutral request carries negotiation metadata as separate supported capability/route/decision/execution/IR/operation version sets, plus semantic task, optional declared kind, explicit path/symbol targets, caller-capability states, Atlas intent, floor/ceiling, optional legacy session ID, all six positive record/source-byte/estimated-token/relationship-depth/work-unit/uncertainty-reserve limits when the ceiling permits DEEP, and optional CLI-only materialization authorization/cap. The application—not either adapter—normalizes semantic inputs, negotiates the Atlas contract sets, then derives the classifier result, normalized task hash, seed digest, starting generation observation, route request, and catalogue input. Before cloning, sorting, or deduplicating targets, it checked-adds the two raw vector lengths, rejects a raw total above 200, and rejects every raw empty target or target above 512 bytes. Canonical identity sorting/deduplication happens only after this raw boundary passes.

Task lifecycle is legacy `1.0.0` only. `start` creates or identically reuses a session from the active committed generation; conflicting identity fails. `complete` is legal only from `reconciled`, with application-derived/verified end Generation Delta and committed tree, and identical replay is idempotent. `abandon` is legal only from `active|reconciled`; it atomically records `completed_at` plus one closed `outcome_code`, identical replay is idempotent, and different reason/state conflicts. The exact public and durable abandon-reason vocabulary is `user_requested` (an explicit task-abandon request) and `inactivity_timeout` (the accepted 24-hour lazy stale-session abandonment path), with no other values. T18B CLI parsing must reject every unknown reason code rather than preserving, aliasing, or mapping it. There is no `failed` command. `show` rejects V2 durable lookup as `durable_contract_unavailable`.

Any supplied legacy session identity is validated by the shared application layer before mutation against exact workspace, legacy `1.0.0` contract, task hash, normalized goal, classification identity, nonterminal state, and applicable start generation; a foreign or mismatched identity fails closed, while absence stays unattributed and changes no session. Successful explicitly bound context acquisition advances monotonically through `created -> context_compiled -> active`. Completed DIRECT `direct_none` is intentionally satisfied zero-context acquisition and creates no IR; LIGHT/DEEP activates only on completed or useful partial output, never blocked/interrupted/error/no-useful-payload. Explicit legacy Context IR activates only when a non-blocked sealed IR and its delivery events persist; attributed source/query activation preserves exact result and event behavior, with state rolled back if event persistence fails.

Committing a generation reconciles every and only active legacy session in the workspace whose start-generation sequence is strictly below the candidate sequence, deterministically ordered by session ID. Candidate commit, active-generation pointer, and all selected `active -> reconciled` updates share the same immediate transaction; failed/abandoned candidates and any fault change neither pointer nor session. Sessions on the candidate generation and all non-active or terminal states remain unchanged. This selector does not infer changed-file causality. Post-commit session updates are rejected because a crash can split pointer/session state; a reconcile-session flag is rejected because it changes the frozen grammar; changed-file filtering is rejected because it invents causality; test-only transition injection is rejected because it cannot prove public reachability. This is an H6B-A0 grammar-preserving clarification, not release approval or V2.0 completion.


MCP adds exactly six V2 tools—`atlas_governor_capabilities`, `atlas_governor_run`, `atlas_task_show`, `atlas_compiled_context_show`, `atlas_context_yield_show`, and `atlas_serving_status`—with no aliases. Those six additive tools expose no source materialization, task/source/lifecycle mutation, caller-path or other filesystem write, retention/compaction/unregister, backup/export, serving build/rebuild, or destructive authority. Existing 13 MCP tools retain names, arguments, outputs, errors, and behavior; this compatibility set includes `atlas_serving_build`, the existing derived-Serving projection operation, which does not mutate workspace source or committed Truth. Every V1 CLI command likewise remains unchanged without routing or deprecation.

The operator flow is progressive governor run (DIRECT may complete with zero context) → optional target-bounded LIGHT query/source-reference → optional DEEP Context IR `2.0.0` → exact source/query with optional legacy session attribution → validation/outcome → reconcile → legacy task complete. Legacy task abandon is a separate terminal branch from `active|reconciled`. A completed or abandoned session may then be read through bounded Context Yield show. Selection alone exposes none of these additions and changes no capability availability.

### Bounded outputs and previews

Every growing output is bounded. Collection `limit` defaults to 50 and is `1..=200`; cursor wire size is at most 4 KiB; every returned nested vector/string/blob has an advertised hard bound. Context Yield `show` returns one bounded report section/page, never wholesale growing raw vectors. Envelopes have closed `completed`, `partial`, `blocked`, `interrupted` variants; interruption has no canonical partial IR. Raw task/seed/path/source/model/vendor data stays absent. Optional materialized bytes are CLI-authorized, response-lifetime only, capped no higher than discovery advertises, and never persisted/logged.

Cursors are versioned opaque canonical tokens containing only hashed/opaque workspace and resolved catalogue, operation, contract, filter, captured generation/state watermark, ordering key, and integrity checksum. They remain usable across CLI processes and are neither authority tokens nor durability claims. Malformed/mismatched cursors are `cursor_invalid`; changed captured state is `cursor_stale`.

Every destructive preview page carries one full-manifest identity and SHA-256 digest. Pagination bounds presentation only; canonical complete-manifest hashing excludes timestamps, page boundaries, and presentation fields. Apply recomputes the complete manifest under exclusion and fails closed on mismatch. Privacy compact requires literal `--confirm-privacy-deletion`, executes in one rollback-before-commit transaction, creates no backup, and preserves Truth/generation history. Unregister requires literal `--irreversible`; it may delete the complete Atlas catalogue including indexed Truth/history but never workspace source, and remains unavailable until T20 proves an external application-owned per-catalogue lock held across canonical re-resolution, replacement checks, recomputation, SQLite close, quarantine/removal, and Windows fault cases without partial logical unregister.

### Documentation

README gives one task-oriented “which command when” flow and clearly separates indexed references from live source authority. OPERATIONS includes lifecycle, config continuity, MCP client configs, status/doctor, retention, backup/upgrade/restore/removal, and performance interpretation. Changelog records compatibility and milestone labels separately from Cargo version.

## Versioning and migration

- Library types and shared functions are the authority.
- Context IR, planner, route decision, execution envelope, capability discovery, operation payload, MCP, Cargo, catalogue, config, provider, and profile versions are separate.
- Legacy explicit surfaces stay `1.0.0`/`planner-v1.1.0`; internal deep V2 uses `2.0.0`/`planner-v2.0.0`. No V2 durable row is authorized. H6B-A0 freezes additive grammar but exposes nothing until T20, `H7-C/POST`, and the applicable T18A/T18B or T19 implementation gate completes.
- No Cargo/tag/release version action occurs here.

## RED–GREEN–REFACTOR acceptance

**RED**

- missing route→source→complete→reconcile→yield flow;
- CLI/MCP DTO/schema/version-negotiation divergence;
- unbounded show/list or materialization;
- partial/blocked/interrupted result encoded as complete/error incorrectly;
- existing explicit syntax/output changes or any new capability appears merely because H6B-A0 was selected;
- DIRECT creates context identity, omits starting generation, or lacks `direct_none`;
- initial decision and execution outcome are conflated.

**GREEN**

- workflow runs through shared application DTOs;
- CLI/MCP/future SDK semantics match;
- existing commands/tests remain unchanged;
- bounds, closed variants, privacy, negotiation work across transports;
- docs/matrix match help/schemas;
- DIRECT/LIGHT/DEEP mapping and zero-context success are exact.

**REFACTOR**

- one dispatcher/application boundary per operation;
- no duplicate output structs or policy parsing in adapters;
- split large CLI/MCP files when adding behavior would bolt unrelated branches onto them.

## Verification commands

```sh
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo test --locked --test source_telemetry_surfaces
cargo test --locked
cargo fmt --all -- --check
cargo run --release --locked --bin atlas-bench
```

T19 adds spawned-binary parity; T22 separately adds pinned external-client smoke.

## Boundaries

- Always: shared DTO/application layer, version negotiation, positive bounds, typed route/envelope/error, starting generation, privacy, source verification.
- Ask first: successor contracts, any H7 persistence/live use, or authority beyond H6B-A0.
- Never: adapter/skill-owned policy, mandatory deep, fake IR, persistent materialization, model/vendor/file-size routing, breaking cutover.

## Success criteria

1. A caller can complete governor/compiler/task/yield lifecycle with documented commands.
2. Existing explicit commands/consumers remain compatible.
3. CLI/MCP/future SDK share schemas, negotiation, and semantics.
4. Outputs/materialization are bounded and stale/version identities fail closed.
5. No V2 durable data exists before H7.
6. Documentation matches behavior/version boundaries and future skill needs no transport-specific logic.

## Resolved and remaining decisions

- **H6B resolved:** H6B-A0 fixes the additive grammar, shared request/application ownership, bounded cursor/preview contract, legacy lifecycle, local destructive authority, and exactly six read-oriented MCP tools above. It authorizes only T20 on disposable/test catalogues and changes no capability availability.
- **H5 resolved:** Context IR `2.0.0` is deep-only; repaired materialization/execution use `context-execution-v2.0.0`.
- **Still gated:** T18 requires completed T20 plus separately owner-approved `H7-C/POST` and a bounded application/lifecycle ownership slice before CLI exposure. T19/T21/T22/T23/T25/H8, live use, release, publication, and future Agent Skill implementation follow their exact separate gates.
