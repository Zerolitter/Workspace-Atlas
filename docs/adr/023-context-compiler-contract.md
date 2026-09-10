# ADR-023: Context compiler compatibility and authority contract

**Status:** Accepted

**Date:** 2026-09-04

## Context

Workspace Atlas now exposes a Context Governor contract and discovery surface,
a legacy task lifecycle, bounded lifecycle reads, privacy compaction, and
matching MCP surfaces. These additions
cross public application, CLI, MCP, catalogue, and operator boundaries. Without
one decision, route depth could be mistaken for model selection, existing V1
calls could be silently reinterpreted, transport adapters could acquire policy
or destructive authority, and a future procedural skill could become a second
source of project truth.

The contract must preserve explicit legacy behavior while allowing callers to
negotiate new transient semantics. It must also distinguish privacy deletion of
eligible derived/session data from whole-catalogue unregister, whose loss and
rollback boundaries differ materially.

## Decision

Adopt the H6B-A0 application contract as an additive, transport-neutral
boundary. The [README canonical table](../../README.md#from-a-task-to-evidence)
is the single public route/capability/compatibility/authority mapping, and the
[operator guide](../../OPERATIONS.md#context-governor-and-lifecycle-commands)
contains the exact command grammar and request workflow.

The Context Governor progresses only as permitted from `DIRECT` to
`ATLAS_LIGHT` to `ATLAS_DEEP`. Route is context-acquisition depth, not a cloud,
local, vendor, price, or model tier; Atlas does not select a model. `DIRECT` may
succeed with `direct_none`, a starting-generation observation, zero Atlas
context, and no Atlas context identity. LIGHT is target-bounded and contains
only versioned query/source-reference operations. DEEP alone may return Context
IR `2.0.0`, paired with the separately versioned transient execution envelope.

Capability discovery is authoritative for runtime availability. It reports
route decision, LIGHT payload, progressive execution, DEEP Context IR, and
bounded materialization available. Materialization is an explicit, CLI-only,
response-lifetime capability; MCP materialization remains disabled. Availability
does not imply durable V2 lifecycle storage or a runtime route default.

Callers negotiate capability, route, decision, execution, IR, and operation
versions independently. MCP protocol, Cargo, catalogue, configuration,
provider, planner, and any future profile versions remain separate as well.
The application layer owns normalization, negotiation, route decisions,
lifecycle policy, bounds, manifests, and typed failures. CLI and MCP parse and
serialize these shared semantics rather than implementing policy.

Existing explicit V1 commands and the original 13 MCP tools remain unchanged,
are not silently routed, and are not deprecated. Legacy Context Packet and
Context IR `1.0.0` remain explicit. The public lifecycle persists legacy
`1.0.0` state only; governor V2 decisions, execution envelopes, and deep IR do
not introduce durable V2 rows or a catalogue migration. Durable V2 lookup and
persistence remain unavailable; bounded lifecycle reads reject V2 lookup as
`durable_contract_unavailable`.

A legacy session binding is accepted only after application-layer validation
against its task-start identity, workspace, `1.0.0` contract, classification,
nonterminal state, and applicable starting generation. Successful acquisition
is the activation boundary: completed `DIRECT` `direct_none` activates without
fabricating Context IR; completed or useful partial LIGHT/DEEP may activate;
blocked, interrupted, error, and no-useful-payload outcomes do not. Successful
generation activation reconciles all and only older-generation active legacy
sessions in the workspace, in deterministic session-ID order, in the same
`BEGIN IMMEDIATE` transaction as candidate commit and the active-generation
pointer. Completion and abandonment remain explicit public terminal
transitions.

Every additive workspace CLI command accepts optional explicit catalogue
selection and emits bounded JSON success output on stdout. Growing reads use
bounded, restart-stable, non-authority cursors. CLI materialized source is
explicit, response-lifetime, and byte-bounded. Confirmed privacy compaction and
unregister require complete manifest digests plus distinct literal
confirmations. Compaction is transactional, creates no backup containing
expired data, and preserves Truth and generation history. Unregister describes
irreversible loss of the complete Atlas catalogue, excludes workspace source
and caller-owned backups, and creates no Atlas backup; its currently unproven
removal path fails closed before deletion.

MCP adds exactly six tools. Governor execution shares the application layer's
validated acquisition-bound legacy activation semantics; materialization is
disabled. The other five additions are reads or capability discovery. MCP
receives no explicit task start/complete/abandon command, retention,
compaction, unregister, Serving build/rebuild, backup/export, caller-path
write, filesystem mutation, source materialization, or destructive authority.

No runtime route profile or default is introduced. Existing registered
configuration and explicit provider configuration continue to govern discovery
and reconciliation; capability/cost profile fields remain unconfigured contract
reservations.

A possible future Agent Skill is outside this implementation. If separately
authorized, it will be separately packaged, discovered, and versioned
procedural guidance that consumes public capability discovery and application
semantics. It will contain no repository facts or project truth, will not read
catalogue internals, replace the governor, bypass source verification, or gain
mutation authority. It is not implemented or shipped by this decision.

## Rejected alternatives and tradeoffs

### Make deep compilation mandatory

Rejected because caller-supplied context can already satisfy a task. Mandatory
DEEP would spend context and work without evidence of need and would make
zero-context success impossible.

### Treat route depth as model tier

Rejected because model availability, locality, vendor, and price are caller
concerns and timing-dependent inputs. Coupling them to semantic routing would
make decisions less reproducible and leak deployment policy into Context IR.

### Silently route or deprecate V1 calls

Rejected because existing explicit consumers depend on their syntax, envelopes,
errors, and legacy semantics. Additive negotiation provides a clean opt-in
without a compatibility shim or implicit reinterpretation.

### Give MCP parity through mutation

Rejected because semantic read parity does not require remote destructive
or filesystem authority. Keeping lifecycle mutation and destructive requests
local limits authority and leaves MCP discovery auditable.

### Back up automatically before privacy deletion or unregister

Rejected because a compaction backup would preserve data explicitly selected
for deletion, while an unregister backup would contradict irreversible-loss
disclosure and transfer retention responsibility to Atlas. Callers own backup
creation, verification, storage, and deletion after stopping writers.

### Ship a skill with embedded project knowledge

Rejected because duplicated repository facts drift and can overrule qualified
Atlas evidence. Procedural guidance may adapt through public discovery later,
but it cannot become a truth plane or transport-specific policy engine.

## Consequences

- Callers can choose the minimum sufficient route and can complete successfully
  with zero Atlas context.
- Route, capability, decision, execution, planner, IR, operation, transport,
  configuration, catalogue, and package identities must be checked separately.
- Existing V1 integrations require no governor adoption or durable migration.
- Deep-permitting requests must provide explicit semantic limits; bounded reads
  and materialization fail closed outside advertised limits.
- Privacy compaction can remove eligible private derived/session data without
  losing indexed Truth or generation history, but recovery requires a
  deliberately retained caller-owned backup.
- Whole-catalogue unregister has a different authority boundary. Until writer
  exclusion and cross-root removal rollback are proven, confirmed apply remains
  unavailable rather than risking partial unregister.
- MCP integrations gain discovery, governor execution, and bounded reads. They
  receive no explicit task start/complete/abandon command or destructive/filesystem
  authority; governor execution can perform validated acquisition-bound legacy
  activation.
- Future skill distribution remains a separate content, packaging, authority,
  and release decision.

## Compatibility, migration, and rollback

This decision is additive. Explicit V1 behavior remains the compatibility
baseline. A `1.0.0` Context IR reader rejects `2.0.0`; V2-aware code may invoke
a legacy reader explicitly but cannot relabel a legacy document as V2
sufficiency. No durable V2 state, runtime profile/default, or downgrade path is
created.

Catalogue migrations remain forward-only and checksum-verified. Before an
upgrade, callers stop writers and create and verify a compatible backup.
Rollback means restoring that pre-upgrade backup with the old binary; it does
not mean migrating a newer catalogue backward. Operational failure before
commit preserves the prior valid state, and privacy compaction rolls back its
single transaction before commit.

The documentation and ADR roll back atomically with any corresponding public
interface rollback. Removing the transient governor path leaves explicit V1
operations and Truth/generations intact. Unregister rollback after a future
successful removal depends solely on a caller-owned backup; workspace source is
never part of the removal set. The future Agent Skill remains absent unless a
separate decision authorizes its implementation and distribution.
