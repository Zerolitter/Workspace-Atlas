---
name: workspace-atlas
description: "Operate the Workspace Atlas CLI/MCP tool (atlas, atlas-mcp, atlas-bench) to index a repository and query bounded, evidence-backed context instead of re-reading the whole codebase. Use when working inside the Workspace-Atlas project itself, or when using atlas as a coding-agent memory layer against any other repository — initializing/reconciling a catalogue, searching symbols, tracing relationships, computing change impact, or building context packets."
---

## What it is

Workspace Atlas (`workspace_atlas` crate, this repo) is a local-first memory
layer for coding agents: a verified, incrementally updated SQLite catalogue of
a project's files, symbols, relationships, effects, and history. It answers
queries with bounded, evidence-backed context instead of requiring an agent to
rediscover the repo from scratch each time.

Binaries: `atlas` (CLI), `atlas-mcp` (stdio JSON-RPC MCP adapter), `atlas-bench`
(deterministic acceptance benchmark). This project builds and ships them; other
projects consume `atlas`/`atlas-mcp` as a context tool.

## Build

```sh
cargo build --release --locked
```

Produces `target/release/atlas(.exe)`, `atlas-mcp`, `atlas-bench`. On Windows,
`run-atlas.bat` forwards args to the release `atlas.exe`.

## Using atlas against a repository (as a context tool)

```sh
atlas init <path-to-repo>          # register workspace, create catalogue
atlas reconcile <path-to-repo>     # index / re-index; run before relying on queries
atlas status <path-to-repo>        # identity, active generation, integrity, lease state
atlas find <path-to-repo> <query>  # search current symbol display names
atlas inspect <path-to-repo> [path] [--symbol KEY]
atlas trace <path-to-repo> <path>          # bounded relationship graph traversal
atlas impact <path-to-repo> <path>...      # bounded change frontier
atlas source <path-to-repo> <path>         # exact source, live-hash verified
atlas context <path-to-repo> "<task>" --mode map|change|audit   # bounded context packet
atlas context-ir <path-to-repo> "<task>"   # deterministic generation-bound Context IR
atlas generation-delta <path-to-repo> <from> <to>
atlas history <path-to-repo>
atlas doctor <path-to-repo>        # health check, abandon interrupted-reconcile candidates
atlas providers <path-to-repo>     # configured provider capabilities/state
atlas serving-build <path-to-repo> # rebuild active generation's serving projection
```

All commands take a workspace root, emit JSON (`--human` pretty-prints), and
use `atlas <command> --help` for full flags. `--catalogue <path>` overrides
catalogue location explicitly.

Key operating rules:
- **No daemon/watcher.** Reconciliation is caller-driven (ADR-020) — always
  run `atlas reconcile` before trusting query freshness; `atlas source`
  independently blocks on a live content-hash mismatch.
- **Single-writer lease.** Reconcile holds a 60s lease; a concurrent writer
  fails fast. The active-generation pointer only advances on a successful
  cycle, so a crash mid-reconcile leaves readers on the prior complete
  generation. Run `atlas doctor` to mark/report abandoned candidates.
- **Catalogue routing is platform-fixed** (ADR-015), not CWD-relative:
  - Windows: `%LOCALAPPDATA%\WorkspaceAtlas\catalogues\<workspace_id>.sqlite`
  - macOS: `$HOME/Library/Application Support/WorkspaceAtlas/catalogues/<workspace_id>.sqlite`
  - Linux: `$XDG_DATA_HOME/workspace-atlas/catalogues/` or `~/.local/share/workspace-atlas/catalogues/<workspace_id>.sqlite`
  Missing/empty/relative platform paths are hard errors; there is no CWD fallback.
- Atlas never moves/renames/deletes files in the indexed project.
- Backups: copy the catalogue SQLite file only while reconciliation is idle;
  run `atlas doctor` (does `PRAGMA integrity_check`) after restore. Migrations
  are forward-only, no downgrade path — restore a pre-upgrade backup instead.

## MCP adapter

`atlas-mcp` speaks newline-delimited JSON-RPC 2.0 over stdio; send
`initialize` first. Tools mirror CLI application functions 1:1 (shared
application layer, ADR-017): `workspace_root` required, `catalogue` optional.

## Developing this repo

```sh
cargo test --locked
cargo run --release --locked --bin atlas-bench   # deterministic pilot fixture acceptance check
python scripts/check-public-hygiene.py           # maintainers, before packaging (repo checkout only)
```

`atlas-bench` generates its fixture via `tests/fixtures/pilot/generate_fixture.py`
(needs Python 3 on PATH) and checks catalogue/query/lifecycle/incremental-reconcile
behavior against the manifest.

Source layout (`src/`): catalogue + generation state machine, structural
providers, optional SCIP semantic providers (`scip_decoder.rs`, `scip_mapping.rs`),
query/history/impact/source/context services, task-session + Context IR +
serving-plane + generation-delta contracts (`task_session.rs`, `task_compiler.rs`,
`serving.rs`, `query.rs`, `resolution.rs`, `semantic_reconcile.rs`).

Design contracts live in `docs/adr/` — read the relevant ADR before changing
catalogue routing, CLI/MCP parity, reconciliation triggering, or licensing.
Notably `docs/adr/015-workspace-identity-and-catalogue-routing.md` (identity/
routing, authoritative) and `docs/adr/020-caller-driven-reconciliation.md`.

## Current limitations (don't relitigate as bugs)

- Dynamic/reflective edges need provider evidence to resolve.
- Rename detection requires an unambiguous exact-content-hash match.
- Deleted-file tombstones keep metadata only, never raw snapshots.
- No automatic history retention/compaction.
- Excluded directories are classified but may still be traversed (reconcile
  time cost on large build/dependency trees).
