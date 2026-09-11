# Workspace Atlas operator guide

Workspace Atlas v2.0.0 is the first public release. V1-labelled commands,
Context IR `1.0.0`/`2.0.0`, planners, lifecycle records, capability milestones,
and all 19 MCP tools are internal contract generations shipped inside v2.0.0,
not earlier public releases or evidence of an installed external V1 user base.

## Build

```sh
cargo build --release --locked
```

This builds `atlas`, `atlas-mcp`, and `atlas-bench`.

## Operational invariants

Source, builds, tests, and runtime evidence are authoritative. Atlas never
edits workspace source. A candidate Truth Plane generation activates atomically
or readers remain on the previous complete generation; the Serving Plane is
derived and rebuildable. Live verification is required before using exact
source in change mode.

Explicit operations and Governor-routed operations coexist. Explicit commands
are not silently redirected through the Governor. Context IR `1.0.0` and
`2.0.0` are separate contracts: unsupported newer schemas fail closed, and
parsing a supported older document does not satisfy a V2 requirement unless
that use is explicitly permitted.

Required-provider failure blocks candidate activation. Optional-provider
failure remains visible as degraded coverage. MCP provides no destructive,
filesystem-write, or source-materialization authority.

## Catalogue routing

All commands use a workspace root and emit JSON (`--human` pretty-prints valid
JSON). `--catalogue <path>` is an explicit override. Otherwise Atlas follows
[ADR-015](docs/adr/015-workspace-identity-and-catalogue-routing.md):

- Windows: `%LOCALAPPDATA%\WorkspaceAtlas\catalogues\<workspace_id>.sqlite`
- macOS: `$HOME/Library/Application Support/WorkspaceAtlas/catalogues/<workspace_id>.sqlite`
- Linux: `$XDG_DATA_HOME/workspace-atlas/catalogues/<workspace_id>.sqlite`, or
  `$HOME/.local/share/workspace-atlas/catalogues/<workspace_id>.sqlite`

Missing, empty, or relative platform environment paths are errors. Atlas never
falls back to the current working directory. `atlas init` writes an atomic
root-to-catalogue locator; later root-addressed commands verify the locator,
physical path confinement, and catalogue workspace row. Missing, corrupt,
escaping, mismatched, or ambiguous routes fail closed.

## Commands

| Command family | Purpose |
|---|---|
| `atlas init|reconcile|status|doctor <root>` | Register, refresh, inspect, and recover a catalogue. |
| `atlas providers <root>` | Report configured provider capabilities and state. |
| `atlas find|inspect|trace|impact|source <root> ...` | Query indexed evidence or live-verified exact source. |
| `atlas context|context-ir <root> ...` | Invoke explicit versioned Context Packet or Context IR behavior. |
| `atlas serving-build|generation-delta|temporal|history <root> ...` | Build derived Serving state or inspect retained evidence. |
| `atlas governor capabilities|run <root> ...` | Discover the contract and invoke currently available context-acquisition behavior. |
| `atlas task start|show|complete|abandon <root> ...` | Operate the `1.0.0` task lifecycle. |
| `atlas compiled-context show|context-yield show|serving-status <root> ...` | Read bounded context, yield, and readiness state. |
| `atlas retention status|compact <root> ...` | Inspect or apply fixed privacy retention. |
| `atlas unregister <root> ...` | Preview catalogue removal or invoke its fail-closed confirmed boundary. |

Use `atlas <command> --help` for complete flags. The
[README canonical table](README.md#from-a-task-to-evidence) is the single
route, compatibility, capability, interface, and authority mapping.

## Context Governor and lifecycle commands

The additive grammar is:

```text
atlas governor capabilities <workspace-root> [--catalogue <path>]
atlas governor run <workspace-root> --request <json-file|-> [--catalogue <path>] [--materialize-source --max-materialized-bytes <n>]

atlas task start <workspace-root> --request <json-file|-> [--catalogue <path>]
atlas task show <workspace-root> <session-id> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas task complete <workspace-root> <session-id> --request <json-file|-> [--catalogue <path>]
atlas task abandon <workspace-root> <session-id> --reason-code <user_requested|inactivity_timeout> [--catalogue <path>]

atlas compiled-context show <workspace-root> (--context-id <id>|--session-id <id>) [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas context-yield show <workspace-root> <session-id> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas serving-status <workspace-root> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]

atlas retention status <workspace-root> [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas retention compact <workspace-root> --dry-run [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas retention compact <workspace-root> --confirm-manifest <sha256> --confirm-privacy-deletion [--catalogue <path>]
atlas unregister <workspace-root> --dry-run [--limit <n>] [--cursor <opaque>] [--catalogue <path>]
atlas unregister <workspace-root> --confirm-manifest <sha256> --irreversible [--catalogue <path>]
```

Dry-run and apply forms are mutually exclusive. `--limit` and `--cursor`
apply only to reads and dry-run previews, not confirmed apply. Every additive
success response is bounded JSON on stdout. These commands add no human-output
mode, caller-selected export path, backup path, or other filesystem write.
Errors remain on stderr with a non-zero exit.

Growing collections default to 50 entries and accept `1..=200`. Opaque cursors
are at most 4 KiB, restart-stable, and bound to the workspace, resolved
catalogue, operation, contract, filters, captured state, ordering, and
checksum. They are not authority or durability tokens. Malformed or mismatched
tokens return `cursor_invalid`; changed captured state returns `cursor_stale`.
Returned strings, nested vectors, serialized documents, and optional source
materialization also have advertised hard bounds.

### Governor request

`governor capabilities` is the authoritative discovery document. Build
`governor run` requests from its advertised versions, routes, closed
vocabularies, availability, and bounds. A minimal zero-context request is:

```json
{
  "supported_versions": {
    "capability_versions": ["context-capabilities-v2.0.0"],
    "route_versions": ["context-route-v2.0.0"],
    "decision_versions": ["context-route-v2.0.0"],
    "execution_versions": ["context-execution-v2.0.0"],
    "ir_versions": ["2.0.0"],
    "operation_versions": ["query-v1.0.0", "source-reference-v1.0.0"]
  },
  "semantic": {
    "task": "answer from caller-supplied context",
    "declared_kind": null,
    "path_targets": [],
    "symbol_targets": [],
    "caller_capabilities": {},
    "atlas_intent": "none",
    "route_floor": "DIRECT",
    "route_ceiling": "DIRECT",
    "legacy_task_session_id": null,
    "deep_limits": null
  }
}
```

The request document is capped at 1 MiB before decoding. Path and symbol
targets are individually 1–512 UTF-8 bytes and together number at most 200;
raw lengths are validated before normalization and deduplication. A ceiling
that permits `ATLAS_DEEP` requires all six positive limits:
`max_records` (maximum 100,000), `max_source_bytes` (16,777,216),
`max_estimated_tokens` (4,000,000), `max_relationship_depth` (64),
`max_work_units` (10,000,000), and `uncertainty_reserve_percent` (100).
No common version, omitted DEEP limits, invalid target, or invalid limit fails
closed.

Bounded explicit source materialization is CLI-only and response-lifetime. It
requires both `--materialize-source` and a positive
`--max-materialized-bytes` no larger than discovery advertises, and is not
persisted or logged. MCP materialization remains disabled.

Current discovery reports route decision, LIGHT payload, progressive execution,
DEEP Context IR, and bounded materialization available. Availability describes
runtime features, not durable V2 lifecycle storage or a runtime default.

The contract flow, when discovery permits each step, is capability discovery,
governor execution through the allowed and available ceiling, target-bounded
LIGHT or DEEP evidence when available and required, live source verification,
outcome validation, reconciliation after project changes, and `1.0.0` task
completion. `DIRECT` can succeed with `direct_none`, an observed or typed
unavailable starting generation, zero Atlas calls, and no Atlas context
identity. Route depth is not model tier: Atlas does not select cloud/local
models or route by model/vendor.

### Versioned lifecycle and replay

Task mutation uses Context IR `1.0.0` only. Request files are closed JSON:

```json
{"context_ir_version":"1.0.0","task":"fix reconnect behavior","declared_kind":"bug_fix"}
{"context_ir_version":"1.0.0","accepted":true,"tests_passed":true,"outcome_code":"accepted_change"}
```

The legal progression is `created -> context_compiled -> active -> reconciled
-> completed`. Internal failure may terminate any non-terminal state, but
there is no public `task failed` command. `task complete` is legal only from
`reconciled`; the application derives and verifies the end Generation Delta
and committed tree. `task abandon` is legal only from `active` or `reconciled`
and atomically records `completed_at` plus exactly `user_requested` or
`inactivity_timeout`.

A supplied `1.0.0` session ID is task-start-bound: before any mutation, the
application validates its workspace, `1.0.0` contract, task and normalized-goal
hashes, classification identity, nonterminal state, and applicable starting
generation. For a validated explicit binding, successful context acquisition
is the activation boundary. Completed `DIRECT` `direct_none` advances to
`active` without creating Context IR. Completed or useful partial LIGHT/DEEP
acquisition can activate; blocked, interrupted, error, and no-useful-payload
outcomes do not.

Successful generation activation reconciles every and only those active
`1.0.0` sessions in the workspace whose starting generation is older than the
candidate. The deterministic session-ID-ordered updates share the same `BEGIN
IMMEDIATE` transaction as candidate commit and the active-generation pointer.
Candidate-generation sessions and created, `context_compiled`, reconciled,
completed, abandoned, or failed sessions remain unchanged. Reconciliation does
not infer changed-file causality, and it is never a post-commit lifecycle
update.

An identical start, complete, or abandon replay is idempotent. A changed task
identity, completion fields, abandon reason, or source state is a conflict and
fails without mutation. Completed, abandoned, and failed states are terminal.
Durable V2 persistence remains unavailable. Task/context/yield show operations
reject durable V2 lookup as `durable_contract_unavailable`.

`context-yield show` preserves the bounded `1.0.0` session, event, metric, and
cursor fields. When the session is terminal `completed` with `accepted=true`
and has a validated non-blocked Context IR `1.0.0` from its starting generation,
the response also includes `report`, a deterministic
`ContextYieldReportV15`. Its accepted-outcome hash binds the completion
identity, its single raw sample binds the retained context, and its typed
metrics retain evidence classes and zero-denominator invalidity. Other session
states, rejected completions, and accepted completions without a qualifying
context return `report: null`; Atlas never promotes them to accepted reports.

Explicit V1-labelled commands do not pass through the Governor and are not
silently reinterpreted. They remain supported v2.0.0 public operations. Context
Packet and Context IR `1.0.0` are explicit contracts. The Governor contract
uses transient Context IR `2.0.0` with `planner-v2.0.0` for DEEP; current
discovery reports DEEP Context IR available. The formats remain strictly
separated: unsupported newer versions fail closed, and successful parsing of a
supported `1.0.0` document does not satisfy V2 unless the operation explicitly
permits it.

## Privacy retention and unregister

`retention status` reports the fixed local policy and the current complete
privacy manifest. Structured session-derived data expires after 30 days or
beyond the newest 1,000 terminal sessions; summary sessions expire after 180
days. Selection uses strict cutoffs and bounds one complete apply manifest to
100 expired sessions plus 100 orphan metrics. Preview pagination changes only
presentation: each page carries the same complete manifest digest.

Run `retention compact --dry-run`, review every page under the captured cursor,
then use the digest with literal `--confirm-privacy-deletion`. Apply recomputes
the complete catalogue-bound manifest under one immediate transaction. Digest
mismatch, writer activity, a stale cursor, invalid confirmation, or any
pre-commit failure rolls back before commit. Atlas creates no backup because
that would preserve data selected for privacy deletion. Compaction removes
eligible raw opt-in text, events, compiled contexts, metrics, or old sessions;
it preserves workspace/file/revision/provider/fact Truth, the active
generation, and all generation history.

Unregister is deliberately separate. Dry-run discloses that successful apply
would remove the complete Atlas catalogue—including indexed Truth and
generation history—plus manifested Atlas-owned locator/config/temporary state.
It excludes all workspace source and caller-owned backups. Atlas creates no
unregister backup; stop writers and independently create, verify, retain, and
eventually delete any desired backup.

The confirmed form requires the exact complete-manifest digest and literal
`--irreversible`. The current implementation then fails closed with
`unregister_unavailable` before moving or deleting anything: external
per-catalogue writer exclusion and cross-root Windows-safe
quarantine/removal/rollback are not yet proven end to end. Do not interpret a
successful dry-run as removal availability. A future successful implementation
must re-resolve canonical identities, reject symlink/reparse/non-regular
replacement, hold application-owned exclusion through SQLite close and
removal, and avoid partial logical unregister.

Privacy compaction is reversible only by a caller-owned pre-compaction backup
that intentionally retains the deleted private data; unregister is
irreversible after a future successful apply unless such a caller-owned backup
exists. Neither operation can delete or modify workspace source.

## Deterministic task compilation

`context-ir` requires at least one explicit `--symbol` or `--path` for a
useful working set. A caller may declare `--task-kind`; otherwise Atlas applies
an ordered whole-word rule table and reports the selected `task_kind`,
`task_kind_source`, and stable `kind_rule_id`. Text without a recognized term
remains `unknown` rather than receiving a guessed intent.

```sh
atlas context-ir <root> "fix reconnect behavior" \
  --symbol ConnectionController.scheduleReconnect \
  --max-records 40 --max-source-bytes 32768 --max-estimated-tokens 8000
```

The fixed planner policy is:

| Task kind | Callers | Callees | Tests | Configuration | Graph depth |
|---|---:|---:|---:|---:|---:|
| `bug_fix` | yes | yes | yes | no | 2 |
| `behavior_change` | yes | yes | yes | no | 2 |
| `api_change` | yes | no | yes | no | 3 |
| `refactor` | yes | yes | yes | no | 2 |
| `configuration_change` | no | no | yes | yes | 1 |
| `test_change` | yes | no | yes | no | 1 |
| `review` | yes | yes | yes | yes | 1 |
| `audit` | yes | yes | yes | yes | 3 |
| `explore` | yes | yes | no | no | 1 |
| `unknown` | yes | yes | yes | no | 1 |

Related test and configuration artifacts are recognized from indexed file
classification even when their relationship is a generic import or reference.
Equivalent path/symbol sets are sorted and deduplicated before request hashing,
so argument order and duplicates do not change `context_id` or `context_hash`.
The selected task kind, classification source/rule, and planner policy are part
of context identity, so materially different recipes cannot collide.
CLI and MCP record/source-byte/token budgets must be positive and fail closed
otherwise. Each selected item records its role, selection reason,
distance, evidence state, generation, and cost. Unresolved relationships remain
uncertainty or omissions and are never promoted to verified working-set items.

Compilation prefers a ready Serving Plane projection and reports
`serving_fallback: true` when it reads the same generation's Truth Plane
directly. Source references in Context IR are indexed ranges, not live-source
authorization; use `atlas source` before change-mode use so the live hash is
verified. The MCP `atlas_context_ir` tool accepts the corresponding
`task_kind`, `path`, `symbol`, and budget arguments through the shared
application function.

## Temporal intelligence

`atlas temporal <root>` compares the active committed generation with its
retained parent. `--from <generation-id>` may select an older retained committed
ancestor; non-ancestors, missing generations, and non-committed generations fail
closed. `--max-records` is positive and independently bounds returned change and
validity details while aggregate indexed counts remain available.

The report derives change records from the existing Generation Delta policy and
retains category, evidence state, and reason codes. An unchanged symbol contract
is reported as `verified_current` only when its active-generation source hash
matches the live file. Returned current-file changes receive the same live-hash
status. Hash mismatch is `stale`; missing, unreadable, ambiguous, or unconfined
source is `unavailable`. Live verification covers only returned change and
validity records, and the report marks omitted evidence as risk rather than
generalizing from a partial sample.

Stale source, unavailable source, delta uncertainty, open conflicts, and
failed/stale coverage are high-risk signals. Disruptive changes, unresolved
relationships, and omitted change or validity evidence are moderate; additive-only
changes are low. A workspace without an active generation or retained parent
returns `no_active_generation` or `baseline_only` with `unknown` risk and no
invented delta or validity claims.

The MCP `atlas_temporal` tool accepts the same optional `from`, positive
`max_records`, and catalogue arguments through the shared application function.
The temporal report schema is `1.0.0`; the MCP capability protocol is `1.3`.
Existing catalogues require no migration.

## Freshness and crash recovery

There is no daemon or watcher; reconciliation is caller-driven by
[ADR-020](docs/adr/020-caller-driven-reconciliation.md). Run `atlas reconcile`
before relying on queries when freshness is unknown. `atlas source`
independently blocks when the live content hash differs from the indexed
revision.

Reconciliation acquires a 60-second single-writer lease. A concurrent writer
fails fast. The active generation pointer changes only after a successful
cycle, so interruption leaves readers on the prior complete generation.
`atlas doctor` marks abandoned candidate generations and reports their IDs.
Lifecycle and retention operations also fail closed around their writer and
transaction boundaries; they do not convert partial work into success.

## Backup, upgrade, migration, and rollback

The catalogue is one SQLite file. Stop or avoid reconciliation and every other
writer before copying it; verify the copy independently. Run `atlas status`
and `atlas doctor` before upgrade, preserve the old binary and backup, install
the new binary, then run `atlas init` with the same workspace root, display
name, reviewed configuration, and explicit catalogue selection (when used) to
apply pending ordered migrations. Re-run status/doctor and verify the workspace,
active generation, configuration hash, schema checksums, and Serving readiness;
rebuild disposable Serving projections when needed.

Migrations in `migrations/` are forward-only, ordered, idempotent, and
checksum-verified. There is no downgrade migration. On failure, stop writers
and restore the caller-owned pre-upgrade backup with the old binary. The
governor contract itself adds no durable V2 row or migration.

## Configuration boundaries

Configuration schema `1.0.0` remains accepted; `1.1.0` adds the explicit
provider runtime and provider entries shown in
[`config/workspace-atlas-v1.1.config.example.toml`](config/workspace-atlas-v1.1.config.example.toml).
Use the same reviewed configuration when reconcile behavior differs from
built-in structural discovery. Provider executables remain explicit and Atlas
does not install them.

Governor commands resolve the registered workspace and optional explicit
catalogue through the canonical route. They do not add a `--config` flag,
promote an experiment profile, infer a model tier, or establish a runtime route
profile/default. Capability and cost profile fields remain unconfigured
contract reservations. Configuration schema, provider, catalogue, Cargo, MCP,
route, capability, execution, operation, planner, and Context IR versions are
separate.

## Semantic providers

External providers are optional and configured explicitly. Atlas invokes them
directly without a shell, applies an environment allowlist, bounds output, and
records provider identity and execution evidence. The repository pins SCIP
schema/provider provenance in
[`schemas/scip/PROVENANCE.md`](schemas/scip/PROVENANCE.md). Atlas does not
install providers.

A required provider that fails blocks candidate activation. An optional
provider failure is retained in diagnostics and coverage so the active result
visibly reports degraded coverage.

## MCP adapter

`atlas-mcp` uses newline-delimited JSON-RPC 2.0 over stdio. Send an
`initialize` request first. `tools/list` returns exactly 19 advertised tools.
All 19 are supported v2 public interfaces, and none is being removed. The
registry ends with `atlas_governor_capabilities`, `atlas_governor_run`,
`atlas_task_show`, `atlas_compiled_context_show`,
`atlas_context_yield_show`, and `atlas_serving_status`.

`workspace_root` is required and `catalogue` is optional. MCP governor
execution uses the same application DTO and validated acquisition-bound
activation semantics as CLI; materialization is disabled. The other five
additions are reads or capability discovery. MCP exposes no explicit task
start/complete/abandon command, retention/compaction/unregister, export,
backup, caller-path write, filesystem mutation, or destructive authority. The
MCP `atlas_serving_build` tool among the 13 core evidence tools performs a
bounded derived-Serving build operation, which mutates only derived Serving
Plane state and not workspace source or committed Truth; the six
governor/lifecycle/status additions add no further build/rebuild authority. See the full ordered registry
and canonical capability mapping in
[`README.md`](README.md#semantic-providers-and-mcp) and the shared adapter
decision in [ADR-017](docs/adr/017-shared-cli-mcp-application-layer.md).

## Benchmark

`atlas-bench` runs the public deterministic fixture generator at
`tests/fixtures/pilot/generate_fixture.py`, then verifies catalogue, query,
lifecycle, and incremental-reconcile expectations from the generated manifest.
Python 3 must be on `PATH`.

```sh
cargo run --release --locked --bin atlas-bench
```

Dependency-free local artifact auditing, bounded operator-supplied local-model
OFF/ON runs, and portable evidence export are documented in
[Local testing and benchmark evidence](docs/local-testing.md). These scripts are
checkout-local tooling: cleanup defaults to dry-run, benchmark arms require
explicit task IDs and bounds, and exporters require a new contained destination.
They do not change `atlas-bench`, Atlas routing, or CLI/MCP authority.

## Current limitations

- Dynamic or reflective edges cannot be resolved without provider evidence.
- Rename detection requires an unambiguous exact-content-hash match.
- Deleted-file tombstones retain metadata only; raw snapshots are not stored.
- Privacy retention is fixed; no configurable runtime retention profile or
  generation-history compaction policy exists.
- Confirmed unregister currently returns `unregister_unavailable` without
  deleting anything.
- Temporal live verification is bounded to returned change and
  unchanged-contract records; omitted evidence is reported as risk rather than
  treated as current.
- Excluded directories are classified but may still be traversed, increasing
  reconcile time for very large build or dependency trees.


The possible future Agent Skill remains absent; see
[ADR-023](docs/adr/023-context-compiler-contract.md) for its separately gated
procedural and packaging boundary.
