# Changelog

This changelog records crate releases and capability compatibility separately.

## [2.0.0] - 2026-09-10

### Added
- The first public, source-first Workspace Atlas release, including the local
  `atlas` CLI, stdio `atlas-mcp` adapter, and retained `atlas-bench`
  qualification tool.

- Public guidance for the discoverable Context Governor contract, legacy
  task/context/yield lifecycle, Serving readiness, fixed privacy retention, and
  truthful unregister boundary now matches implemented CLI help, capability
  availability, and application contracts.
- The six MCP additions are documented with their exact lifecycle,
  capability and authority boundary:
  `atlas_governor_capabilities`, `atlas_governor_run`, `atlas_task_show`,
  `atlas_compiled_context_show`, `atlas_context_yield_show`, and
  `atlas_serving_status`.
- [ADR-023](adr/023-context-compiler-contract.md) records the context compiler
  compatibility, versioning, migration, rollback, privacy, and future-skill
  decisions.

### Changed

- The README now provides the single canonical mapping for `DIRECT`,
  `ATLAS_LIGHT`, and `ATLAS_DEEP`, including intent/floor precedence,
  zero-context and generation behavior, capability discovery, CLI/MCP
  availability, and MCP's excluded explicit lifecycle commands and destructive
  authority.
- Operator guidance now records exact command grammar, request limits,
  stdout-only bounded success output, literal confirmations, legacy lifecycle
  transitions/replay, caller-owned backup responsibility, and the currently
  fail-closed confirmed unregister path.
- The documented legacy lifecycle now identifies validated task-start binding
  and successful acquisition as the activation boundary: completed
  `DIRECT`/`direct_none` and completed or useful partial LIGHT/DEEP can
  activate, while blocked, interrupted, error, and no-useful-payload outcomes
  cannot. Successful generation activation reconciles exactly older active
  legacy sessions in the same immediate transaction as generation commit.

### Compatibility and migration

- Existing explicit V1 CLI commands and the original 13 MCP tools remain
  unchanged, are not silently routed, and are not deprecated.
- Capability, route/decision, execution, Context IR, planner, operation, MCP,
  Cargo, catalogue, configuration, provider, and future profile versions remain
  separate contracts. No durable V2 state, runtime profile/default, or
  catalogue migration is introduced by this documentation contract.
- Privacy compaction preserves indexed Truth and generation history. Whole-
  catalogue unregister instead discloses irreversible Truth/history loss,
  excludes workspace source and caller-owned backups, creates no Atlas backup,
  and remains unavailable before deletion while writer exclusion/removal
  rollback is unproven.
- A possible future Agent Skill remains unimplemented and unshipped; any later
  procedural guidance is separately packaged/versioned and contains no project
  truth.

## V1.4 capability milestone — 2026-09-02

### Added

- `atlas temporal` and MCP `atlas_temporal` produce the same deterministic,
  bounded report for the active generation relative to its retained parent or
  an explicitly selected committed ancestor.
- Change records reuse Generation Delta categories, evidence states, reason
  codes, stable IDs, hashes, and policy provenance.
- Unchanged symbol contracts are live-hash verified before being reported as
  `verified_current`; returned current-file changes receive the same check, and
  stale or unavailable source remains explicit.

### Changed

- The MCP capability protocol is `1.3` and advertises temporal report schema
  `1.0.0`.
- Generation Delta now enforces its documented committed-generation boundary
  instead of accepting candidate, failed, or abandoned generations.

### Compatibility

- The CLI and MCP additions are additive. Existing command names, arguments,
  transport envelopes, Context IR schema `1.0.0`, planner policy `v1.1.0`, and
  Generation Delta schema/policy remain unchanged.
- Existing catalogues open without migration; V1.4 adds no table, column, or
  persisted-format requirement. Previously persisted V1.3 task/context
  identifiers remain valid.
- Temporal reports never infer missing history. A first committed generation is
  reported as `baseline_only` with unknown risk until an ancestor exists.

## V1.3 capability milestone — 2026-09-02

### Added

- Deterministic task-kind recipes that independently select callers, callees,
  test contracts, configuration inputs, and bounded relationship depth.
- Artifact-aware test and configuration selection for generic imports and
  references.
- End-to-end CLI/MCP acceptance coverage for task-specific evidence,
  deterministic seed handling, classifier explanations, and invalid budgets.

### Changed

- Task fallback classification now matches whole words and reports a stable
  rule ID, preventing incidental substrings from changing task kind.
- Equivalent seed sets are sorted and deduplicated before request and Context
  IR hashing.
- Task kind, classification provenance, and planner policy now participate in
  context identity, preventing distinct recipes from reusing one persisted ID.
- The planner policy is `planner-v1.1.0`; Context IR schema `1.0.0`, CLI/MCP
  command names and arguments, provider behavior, and crate version are
  unchanged.
- Opaque task-session and context identifiers change under the new planner
  policy because classification provenance now participates in identity;
  serialized field names and transport envelopes are unchanged.
- Indexed source references now carry the file content hash rather than a
  symbol-fact identifier; live source still requires `atlas source`
  verification.
