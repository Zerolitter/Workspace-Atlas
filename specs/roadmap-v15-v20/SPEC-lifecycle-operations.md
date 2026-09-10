# Spec: `lifecycle-operations`

## Objective

Provide bounded, local lifecycle operations for task sessions, compiled contexts, metrics, catalogues, upgrades, backups, restore, retention, and removal without deleting canonical project truth accidentally or implying unsupported downgrade behavior.

## Current-state delta

The base has a tested internal task state machine, context/event persistence, generation history, one-file catalogue backup guidance, forward-only checksum migrations, and `doctor` recovery for abandoned generation candidates. It lacks:

- public task start/show/complete/abandon and context show;
- session/context/metric retention and compaction;
- unregister/uninstall/locator cleanup guidance or command;
- complete WAL/SHM/backup/provider-temp cleanup semantics;
- documented binary upgrade/migration trigger and before/after verification;
- doctor integrity checks for task/context/delta/lease/metric derived state;
- rollback beyond restoring a pre-upgrade catalogue backup.

## Contract

### Task/context lifecycle

- `task start` creates or identically reuses only a legacy Context IR `1.0.0` session from the active committed generation. Conflicting task/session identity replay fails.
- `task show` is bounded/paginated and rejects V2 durable lookup as `durable_contract_unavailable`; it never exposes raw task/source unless explicit authorized local opt-in applies.
- `task complete` is legal only from `reconciled`. The application derives and verifies the end Generation Delta and committed tree, then atomically completes under the existing identical-replay contract; conflicting replay fails.
- `task abandon` is legal only from `active|reconciled`. It atomically persists `completed_at` and one closed reason code in `outcome_code`; identical replay is idempotent and a different reason or state conflicts. The exact public and durable reason codes are `user_requested` for an explicit task-abandon request and `inactivity_timeout` for the accepted 24-hour lazy stale-session abandonment path. No other reason exists; T18B must reject every unknown CLI reason code without aliasing or fallback. There is no public `failed` command.
- `compiled-context show` retrieves the exact persisted canonical legacy IR by ID or session under workspace identity and bounds.

### Retention/compaction

Retention policy independently covers raw opt-in text, events, compiled contexts, stage metrics, yield reports, serving generations, deltas, and backups. Deletion order follows foreign keys and produces exact counts. Privacy compaction may remove derived/session data but MUST NOT remove workspace/file/revision/provider/fact/current-generation Truth Plane evidence or generation history.

A dry run computes the complete canonical privacy manifest. Pagination bounds presentation only: every page carries the same complete-manifest digest and identity, and timestamps, page boundaries, and presentation fields do not affect hashing. Apply requires the digest plus literal `--confirm-privacy-deletion`, recomputes the complete manifest inside one rollback-before-commit transaction, fails closed on mismatch or any precondition failure, and creates no backup containing expired data.

Unregister is separate and may irreversibly delete the complete Atlas catalogue, including its indexed Truth Plane evidence and generation history, but never workspace source. Its dry run and literal `--irreversible` confirmation disclose that loss. Atlas creates no unregister backup; any backup is independently created, verified, owned, and retained by the caller outside this operation.

### Catalogue upgrade/restore

Document and test:

1. stop/avoid reconciliation writer;
2. verify status/doctor and backup path;
3. upgrade binary;
4. invoke the documented migration entrypoint;
5. verify schema/checksums/workspace/generation/config/serving state;
6. rebuild disposable projections if needed;
7. on failure, restore the pre-upgrade backup with the old binary.

There is no downgrade migration. “Rollback” means binary plus compatible pre-upgrade backup.

### Unregister/uninstall

The complete canonical unregister manifest identifies the root locator, catalogue plus WAL/SHM, application-owned backups, application-private provider temporary data, and optional config copies; binaries remain outside Atlas authority unless installed through a documented channel, and every caller-owned backup and all workspace source are excluded. Apply acquires an application-owned exclusive per-catalogue lock outside the files being removed before preview revalidation and holds it through SQLite close and Windows-safe quarantine/removal. While holding that lock it canonically re-resolves workspace/catalogue identity, rejects symlink/reparse/non-regular replacements, recomputes the complete manifest, requires exact digest equality plus literal `--irreversible`, and atomically quarantines application-owned files where the platform permits. Any failed precondition, close, quarantine, or removal fails without partial logical unregister.

## Interfaces

H6B-A0 fixes the additive CLI grammar:

- `atlas governor capabilities <workspace-root> [--catalogue <path>]`;
- `atlas governor run <workspace-root> --request <json-file|-> [--catalogue <path>] [--materialize-source --max-materialized-bytes <n>]`;
- `atlas task start <workspace-root> --request <json-file|-> [--catalogue <path>]`;
- `atlas task show <workspace-root> <session-id> [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas task complete <workspace-root> <session-id> --request <json-file|-> [--catalogue <path>]`;
- `atlas task abandon <workspace-root> <session-id> --reason-code <closed-code> [--catalogue <path>]`;
- `atlas compiled-context show <workspace-root> (--context-id <id>|--session-id <id>) [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas retention status <workspace-root> [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas context-yield show <workspace-root> <session-id> [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas serving-status <workspace-root> [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas retention compact <workspace-root> --dry-run [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas retention compact <workspace-root> --confirm-manifest <sha256> --confirm-privacy-deletion [--catalogue <path>]`;
- `atlas unregister <workspace-root> --dry-run [--limit <n> --cursor <opaque>] [--catalogue <path>]`;
- `atlas unregister <workspace-root> --confirm-manifest <sha256> --irreversible [--catalogue <path>]`.

Every new workspace command uses the same optional `--catalogue` selection and canonical validation path. Dry-run and apply flags are mutually exclusive. Growing reads and previews use restart-stable opaque non-authority cursors: collection `limit` defaults to 50 and is `1..=200`, cursor wire size is at most 4 KiB, mismatch or malformed input is `cursor_invalid`, and changed captured state is `cursor_stale`. Every returned nested vector/string/blob has an advertised hard bound.

MCP adds exactly six read-oriented V2 tools: `atlas_governor_capabilities`, `atlas_governor_run`, `atlas_task_show`, `atlas_compiled_context_show`, `atlas_context_yield_show`, and `atlas_serving_status`. Those six additions expose no source materialization, task/source/lifecycle mutation, retention, compaction, unregister, serving build/rebuild, backup, export, caller-path or other filesystem write, or destructive authority. Existing CLI commands and the 13 existing MCP tools remain unchanged and are not deprecated; the retained set includes `atlas_serving_build`, the existing derived-Serving projection operation, which does not mutate workspace source or committed Truth.

This contract freeze does not expose or advertise any command or tool. T20 may implement only the retention/unregister internals against disposable/test catalogues after the record repair is freshly accepted; T18A/T18B and T19 own later application, CLI, and MCP exposure under their remaining gates.

## Persistence/migration

Prefer current tables and foreign keys. H6B-A0 authorizes no durable V2 state. Any retention configuration/manifest persistence or other durable change needs its applicable H7 approval with old-catalogue, referential-integrity/cleanup, fault-injection, backup/restore, and rollback evidence before use.

## RED–GREEN–REFACTOR acceptance

**RED**

- complete or abandon a session from a disallowed source state, replay with conflicting identity/reason/state, or request V2 durable lookup;
- compact derived data with a paged rather than complete manifest, a naive cascade, or backup-first flow and demonstrate risk to retained Truth/history or preservation of expired telemetry;
- interrupt compaction, close, quarantine, removal, or upgrade at each fault boundary;
- let canonical identity change, replace a catalogue with a symlink/reparse/non-regular file, omit catalogue/WAL/SHM/application-owned state from the removal manifest, include workspace source or a caller-owned backup, or permit partial logical unregister.

**GREEN**

- legacy lifecycle commands enforce the exact legal states, atomic fields, identical-replay idempotency, conflicting-replay failure, privacy, and bounds;
- compaction dry-run/apply use one complete canonical manifest; apply recomputes it transactionally, fails closed, and rolls back before commit without copying expired data;
- Truth Plane and generation history remain readable and unchanged after privacy compaction;
- upgrade/restore survives the fault matrix and doctor reports exact repair;
- unregister holds external exclusion through SQLite close and Windows-safe quarantine/removal, truthfully removes only the complete manifested Atlas-owned state including catalogue Truth/history, never touches workspace source or creates a backup, and fails without partial logical unregister.

**REFACTOR**

- one typed cleanup manifest drives dry-run/apply;
- separate generation-history policy from task telemetry retention;
- keep destructive authority out of transport adapters.

## Verification commands

```sh
cargo test --locked task_session::tests
cargo test --locked migrations::tests
cargo test --locked catalogue::tests
cargo test --locked --test temporal_intelligence_v14
cargo test --locked
cargo run --release --locked --bin atlas-bench -- --manifest tests/fixtures/context_yield/benchmark-scenario.json
```

## Boundaries

- Always: complete canonical manifests and digests, literal confirmations, canonical re-resolution, replacement checks, fail-closed recomputation, external unregister exclusion through SQLite close/removal, transactional privacy compaction without backup, backup-before-migrate, and workspace source untouched.
- Ask first: retention defaults, generation compaction, migration, or any future MCP mutation, caller-path write, Atlas-created backup/export, or durable V2 state.
- Never: downgrade claim; delete workspace source or a caller-owned backup; preserve expired telemetry through a compaction backup; compact Truth Plane or generation history as privacy cleanup; partially unregister; expose a frozen command/tool before its implementation gate; or retain raw data indefinitely.

## Success criteria

1. Task/context lifecycle is usable and compatible through supported interfaces.
2. Every local data category has discoverable location, retention, export/delete/cleanup semantics.
3. Derived compaction leaves Truth Plane and required temporal history intact.
4. Upgrade/restore/unregister paths are executable and fault-tested.
5. No lifecycle operation broadens Atlas source authority.

## Open decisions

- **H2/H7:** retention periods, generation-history policy outside privacy compaction, config-copy cleanup, and any persistence/migration.
- **H6B-A0 resolved:** exact additive CLI grammar, legacy lifecycle semantics, and six read-oriented MCP additions with no source materialization or mutation/build/destructive authority; the compatibility-retained 13-tool set still includes the existing derived-Serving `atlas_serving_build` operation. Complete-manifest confirmations, privacy compaction preservation, and truthful irreversible unregister remain unchanged.
- **H8:** install-channel-specific uninstall steps after a distribution path is authorized.
