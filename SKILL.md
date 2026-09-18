---
name: workspace-atlas
description: "Operate Workspace Atlas v2.0 as a local-first project-context compiler for coding agents. Use it to reconcile repository truth, discover the minimum context-acquisition route, query bounded evidence, live-verify exact source, compile task-specific Context IR when needed, inspect impact/history, and validate project changes without making every agent rescan the workspace."
---

## What Workspace Atlas is

Workspace Atlas is a local-first, provider-neutral workspace intelligence layer.
It maintains an incrementally reconciled SQLite catalogue of project files,
revisions, symbols, relationships, effects, coverage, conflicts, lifecycle
history, and immutable generations, then exposes bounded evidence to models and
agents.

The operating principle is:

```text
compile project truth eagerly
compile task context lazily
```

Atlas is not the source of truth, an autonomous coding agent, or an opaque
memory system. Current source, builds, tests, and runtime evidence remain
authoritative. Atlas never gains permission to edit source merely because it
indexed or returned it.

Binaries:

- `atlas` — local CLI and authority boundary.
- `atlas-mcp` — stdio JSON-RPC MCP adapter for supported shared application
  operations.
- `atlas-bench` — deterministic local acceptance/evidence harness.

## Build

```sh
cargo build --release --locked
```

This produces `target/release/atlas(.exe)`, `atlas-mcp`, and `atlas-bench`.
On Windows, use the `.exe` binaries.

## Start with freshness

Before relying on Atlas evidence when repository freshness is unknown:

```sh
atlas init <repo>        # once per workspace registration
atlas reconcile <repo>   # refresh project truth
atlas status <repo>      # confirm active committed generation + integrity
```

Reconciliation is caller-driven. There is no required daemon or watcher.
A failed candidate generation must not replace the last valid active generation.

## Prefer the Context Governor for task-oriented work

Atlas v2.0 supports progressive context acquisition:

```text
DIRECT
  ↓
ATLAS_LIGHT
  ↓
ATLAS_DEEP
```

These are **context-acquisition depths**, not model tiers.

First discover the supported Governor contract:

```sh
atlas governor capabilities <repo>
```

Then use `atlas governor run <repo> ...` according to the executable
`--help` grammar and the capability response.

### DIRECT

Use DIRECT when the caller already has sufficient evidence for the task.
A valid DIRECT result may be `direct_none`: zero Atlas context and no Context
IR. Zero-context success is an intended Atlas outcome.

Typical case: the user supplied an exact file/range and the task does not need
repository discovery.

### ATLAS_LIGHT

Use LIGHT for target-bounded project evidence such as:

- exact target identity;
- source references;
- direct relationships;
- bounded impact frontier;
- basic coverage/conflict state.

LIGHT should stay small. It does not return a full Context Packet or Context IR.

### ATLAS_DEEP

Escalate to DEEP when the task needs synthesis across project structure, for
example:

- required-role closure;
- cross-module behavioral changes;
- temporal/history reasoning;
- validation planning;
- broader uncertainty resolution.

DEEP may produce transient Context IR `2.0.0` under explicit positive bounds.

### Progressive escalation

Do not force DEEP up front merely because it exists.

Prefer:

```text
sufficient caller evidence
        ↓
      DIRECT

need bounded project facts
        ↓
   ATLAS_LIGHT

still missing required evidence
        ↓
   ATLAS_DEEP
```

Escalate when the evidence requirement grows. Do not treat route depth as a
proxy for model quality, vendor, locality, or price.

## Exact-source safety

Indexed source ranges are references, not permission to edit and not proof that
the live file is unchanged.

Before change-mode use, retrieve exact source through:

```sh
atlas source <repo> <path> ...
```

`atlas source` live-checks the current file hash and fails closed on stale
indexed content.

After source changes, reconcile before treating the new project state as
current Atlas truth.

## Explicit query surfaces remain valid

Governor routing is additive. Explicit operations keep their own documented
contracts and are not silently redirected through the Governor.

Useful explicit commands include:

```sh
atlas find <repo> <query>
atlas inspect <repo> [path] [--symbol KEY]
atlas trace <repo> <path>
atlas impact <repo> <path>...
atlas source <repo> <path>
atlas context <repo> "<task>" --mode map|change|audit
atlas context-ir <repo> "<task>" ...
atlas serving-build <repo>
atlas generation-delta <repo> <from> <to>
atlas temporal <repo> ...
atlas history <repo>
atlas providers <repo>
atlas doctor <repo>
```

Use `atlas <command> --help` as the executable grammar reference.

## How to reason about Atlas evidence

Preserve these distinctions:

- **Truth Plane** — canonical project evidence, generations, provenance,
  coverage, conflicts, resolution state.
- **Serving Plane** — derived, generation-bound, disposable projections for
  efficient selection.
- **Context/Governor layer** — task-dependent route, selection, packing, and
  transient Context IR.

Never silently turn any of the following into project truth:

- ranking;
- task classification;
- relevance;
- future learned/adaptive policy;
- model-generated summaries;
- probabilistic suggestions.

Unresolved edges, partial coverage, conflicts, stale source, omissions, and
unsupported providers are evidence too. Do not erase uncertainty from the
answer.

## MCP boundary

`atlas-mcp` uses the same application semantics for the public operations it
exposes, but CLI and MCP do **not** have identical authority.

MCP is suitable for supported query, Governor, status, lifecycle-read, and
derived Serving operations. It does not gain general filesystem mutation,
source-edit authority, destructive catalogue authority, backup/export
authority, privacy-compaction/unregister authority, or CLI-only source
materialization.

Always send `initialize` before `tools/list` or tool calls. Use capability
discovery rather than assuming that a route feature is available.

## Operating invariants

- Source remains authoritative.
- Failure preserves the previous valid committed generation.
- Derived Serving data can be rebuilt from Truth.
- Prediction/ranking never becomes evidence.
- History is not current state.
- Exact source for change-mode work must be live-verified.
- Atlas indexes project state; it does not grant source mutation authority.
- Keep requests bounded; do not use repository-scale work on the ordinary hot
  retrieval path when a smaller evidence frontier is sufficient.

## Catalogue and concurrency

Without `--catalogue`, Atlas uses its platform application-data catalogue
location. Do not invent a CWD-relative catalogue path.

Reconciliation uses a single-writer lease; readers continue against the last
complete committed generation while a candidate is being built.

Back up a catalogue only while writers are stopped/idle. Migrations are
forward-only; rollback means restoring a verified pre-upgrade backup with the
compatible older binary.

## Developing Workspace Atlas itself

Run the locked product checks:

```sh
cargo test --locked
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo run --release --locked --bin atlas-bench
```

Maintainers should also run:

```sh
cargo fmt --all -- --check
python scripts/check-public-hygiene.py
```

Before changing routing, lifecycle, catalogue, CLI/MCP authority, or context
contracts, read the relevant ADRs under `docs/adr/`. In particular, keep the
Context Governor/compatibility boundary in ADR-023 and caller-driven freshness
in ADR-020 intact unless a new accepted decision explicitly replaces them.

## Current boundaries

Do not relitigate these as ordinary bugs without evidence:

- dynamic/reflective relationships remain unresolved without provider evidence;
- rename detection requires unambiguous exact-content-hash evidence;
- deleted-file tombstones retain metadata, not raw source snapshots;
- privacy retention exists, but configurable generation-history compaction is
  not introduced;
- confirmed whole-catalogue unregister currently fails closed before deletion
  until its writer-exclusion/removal guarantees are proven;
- temporal live verification is bounded to returned evidence and reports
  omitted evidence as risk;
- excluded directories may still be traversed, which can increase reconcile
  time on very large build/dependency trees.

## Default agent workflow

For repository-changing work, prefer this sequence:

```text
reconcile if freshness is unknown
        ↓
discover Governor capabilities
        ↓
acquire the minimum sufficient route
        ↓
inspect uncertainty / coverage / omissions
        ↓
live-verify exact source before change
        ↓
make and validate the change outside Atlas
        ↓
reconcile changed project state
        ↓
use generation/temporal evidence for review when needed
```

The goal is not maximum Atlas usage. The goal is the **minimum sufficient
trustworthy project assistance required for the task**.
