# Troubleshooting draft

## Start with executable discovery

This repository-only draft is package-excluded and describes current private pre-release behavior. Build the debug binaries with `cargo build --locked --bins`, then use the platform spelling of the same binary:

```text
# Unix-like shell
target/debug/atlas governor capabilities <workspace-root>

# Windows PowerShell
.\target\debug\atlas.exe governor capabilities <workspace-root>
```

Add `--catalogue <path>` when the workspace was registered with an explicit catalogue. `atlas <command> --help` is the executable grammar reference. The [README compatibility table](../../README.md#from-a-task-to-evidence) remains the single authority for route, capability, interface, and version mapping; do not copy a version table from this draft.

## Baseline diagnostic sequence

Run these against the same workspace root and catalogue selection:

```text
atlas status <workspace-root>
atlas doctor <workspace-root>
atlas providers <workspace-root>
atlas governor capabilities <workspace-root>
```

Use the built debug path in place of `atlas` when it is not installed on `PATH`. Preserve typed JSON and stderr, but redact private absolute paths and never collect raw source, prompts, credentials, environment values, or whole catalogues by default.

- `status` checks registered/effective workspace and configuration identity.
- `doctor` reports catalogue integrity and abandoned candidates; it does not repair arbitrary corruption or prove source freshness.
- `providers` distinguishes built-in/external, required/optional, available/degraded states.
- `governor capabilities` is authoritative for available routes, closed vocabularies, versions, operations, and bounds at that binary revision.

## DIRECT, ATLAS_LIGHT, and ATLAS_DEEP

Route names are context-acquisition depth, not model tiers.

- **`DIRECT`:** `atlas_intent = none` forces DIRECT. With a DIRECT floor and caller-satisfied capabilities, `direct_none` is success with zero Atlas calls and no Atlas context identity, packet, or IR. An observed or typed-unavailable starting generation is still reported. Do not troubleshoot zero context as missing output when the result is `direct_none`.
- **`ATLAS_LIGHT`:** `allow` begins at the explicit floor; `require` raises the effective floor to at least LIGHT. LIGHT requires explicit targets and returns bounded `query` and/or exact source-reference payloads, never a legacy Context Packet or Context IR. A changed or unavailable captured generation requires reconciliation and request resubmission.
- **`ATLAS_DEEP`:** the vocabulary reserves deep Context IR semantics, but current discovery reports progressive execution and deep Context IR unavailable. A ceiling that permits DEEP still requires all six positive semantic limits. Do not claim availability, fabricate an IR, make DEEP mandatory, or infer future skill support.

If a request fails, compare its supported-version sets, intent, floor/ceiling, targets, caller capability states, and six DEEP limits to the fresh discovery document. Availability overrides vocabulary. The possible future Agent Skill is absent and would be separately supplied, reviewed, versioned, and packaged; it is not a fallback for a missing route.

## Source and Truth failures

The catalogue's immutable generation is repository Truth at reconciliation time, but indexed ranges are not live source authority. Before change-mode use:

```text
atlas source <workspace-root> <repository-relative-path>
```

Use the exact syntax from `atlas source --help`. If live content differs from the indexed hash, `source` blocks as stale. Reconcile, inspect the new generation, and resubmit any generation-bound route request. Missing, unreadable, ambiguous, or unconfined source remains unavailable; do not substitute cached text or an older snapshot. Failed candidates never replace the last complete active generation.

## Catalogue, configuration, and restore

- Missing/relative platform application-data paths, corrupt locators, catalogue/root mismatch, escaping paths, and ambiguous routes fail closed. Supply the original root and explicit catalogue where applicable.
- Reconciliation is caller-driven and guarded by a 60-second writer lease. Stop concurrent writers; interruption leaves readers on the prior complete generation.
- Registered configuration continuity is hash-bound. A differing supplied configuration is a mismatch, not permission to silently use defaults.
- Before upgrade, stop writers and create and independently verify a caller-owned copy of the SQLite catalogue. Migrations are forward-only; there is no downgrade. On failed upgrade, restore the pre-upgrade backup and old binary, then run `status` and `doctor`.
- Serving projections are disposable accelerators. A not-ready projection should fall back to equivalent Truth behavior where documented; rebuild it rather than treating it as canonical state.

See [backup and rollback](../../OPERATIONS.md#backup-upgrade-migration-and-rollback) for the complete procedure.

## Retention, unregister, and removal

`retention compact --dry-run` presents a bounded view of one complete deletion manifest. Review every page under the same cursor, then apply only with its exact digest and literal confirmation. Compaction deletes eligible session-derived data but preserves indexed Truth and generation history. Atlas creates no backup because that would retain data selected for privacy deletion; only an intentional caller-owned pre-compaction backup can restore it.

`unregister --dry-run` is separate and previews loss of the complete Atlas catalogue, Truth/history, locator, registered configuration, and temporary Atlas-owned state; it never includes workspace source or caller-owned backups. Confirmed unregister currently returns `unregister_unavailable` before moving or deleting anything. A successful preview is not evidence of removal. Do not manually delete a partial subset and call it unregister.

## Provider and network failures

Atlas never installs providers. Install the exact operator-reviewed provider separately, ensure its executable resolves on `PATH`, and compare `atlas providers` with the [configuration example](../../config/workspace-atlas-v1.1.config.example.toml) and [SCIP provenance](../../schemas/scip/PROVENANCE.md).

Provider installers and provider processes may use the network. Atlas direct-spawns without a shell, filters environment variables, and bounds time/output, but `best_effort_allowed` is not network or filesystem sandboxing. Enforce offline/mirror/firewall/container policy outside Atlas when required. Required-provider failure blocks activation; optional-provider failure should remain visible as degraded coverage, not be reported as semantic success.

## MCP failures

`atlas-mcp` is newline-delimited JSON-RPC 2.0 over stdio. Initialize before listing/calling tools, keep logs off stdout, and diagnose protocol negotiation separately from Atlas capability negotiation. MCP has no retention, unregister, backup, Serving rebuild, caller-path write, or explicit task mutation command. Governor execution can still activate a validated explicitly bound legacy session; do not describe the entire MCP surface as mutation-free.

## Escalation evidence

Follow the bounded [support diagnostic bundle](support.md#safe-diagnostic-bundle). No support channel or contact is approved. Sensitive vulnerability evidence must follow the incomplete reporting boundary in the [security draft](security.md#vulnerability-reporting-decision), not a public forum.
