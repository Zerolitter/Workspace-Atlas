# Spec: `temporal-evidence-integration`

## Objective

Make review, bug-fix, API-change, and behavior-change compilation consume bounded temporal evidence while preserving V1.4’s committed-history, live-verification, and no-invention guarantees.

## Current-state delta

V1.4 compares the active committed generation with its retained parent or explicit retained committed ancestor, reuses Generation Delta categories/reason codes, bounds returned change/validity details, live-hash verifies returned current evidence, and reports risk/omissions. The task compiler currently reads current-generation relationships only; review does not consume Generation Delta, effects are absent, and task completion is not linked to post-task changes. Evidence leases recheck source hashes but do not compare every stored provider/resolver/projection/coverage/conflict dependency.

## Contract

### Temporal selection

- Default baseline is the active generation’s retained committed parent.
- An explicit baseline must be a retained committed ancestor of the captured active generation.
- Missing history yields typed baseline/no-history state, not invented “all added” evidence.
- Compilation captures one active generation and one temporal baseline; neither may drift mid-request.

### Recipe integration

**Review requires where available:** bounded Generation Delta, changed relationships/effects/coverage/conflicts, affected tests, verified unchanged contracts, validation evidence, and temporal risk/omissions.

**Bug fix requires:** recent relevant change/history around explicit targets, current implementation/dependencies/tests/source, and uncertainty.

**API/behavior change requires:** changed contracts/dependents/effects/tests/config/docs and verified unchanged constraints where provable.

Temporal items use existing roles/reasons such as historical constraint and Generation Delta relevance. They compete under explicit temporal/record/source/token budgets and have independent omissions so current evidence cannot silently starve history or vice versa.

### Semantic change quality

Future delta hardening must distinguish:

- symbol semantic property change from line-only movement;
- relationship add/remove/retarget/resolution-state change;
- effect, provider, coverage, conflict, unresolved/ambiguous change;
- tests newly/no-longer affected.

“Unchanged” requires stable source/evidence identity and sufficient coverage. Returned current evidence is live-hash verified. Omitted evidence is risk, not proof.

### Evidence leases

A lease is generation/evidence structural validity, not wall-clock freshness. Verify on use against source hash and relevant provider fingerprint, target universe, resolver/projection policy, coverage/conflict state, and workspace identity. A lease never authorizes stale source bytes. Carry-forward to a newer generation requires explicit compatibility proof and is not default.

### Task outcome linkage

After a task reconciles, compare start/end committed generations and record artifact-change events linked to the task session. This supports measurement but does not claim that supplied evidence caused a change or that unchanged files were irrelevant.

## Interfaces and compatibility

Reuse existing temporal/delta contracts. Deep V2 integration populates T11-frozen roles under `planner-v2.0.0`; it does not change route policy or schema. Durable temporal changes require H7; H5 authorizes no migration.

## RED–GREEN–REFACTOR acceptance

**RED**

- review Context IR currently lacks delta/history;
- non-ancestor/candidate/failed baseline is rejected by temporal but must also fail compiler integration;
- line movement currently may appear as semantic symbol modification;
- relationship retarget/effect/provider-state fixtures lack precise categories;
- lease remains apparently valid across policy/fingerprint drift;
- task completion has no linked artifact-change events.

**GREEN**

- review/bug/API/behavior fixtures contain required bounded temporal roles or typed deficits;
- baseline-only/no-history remains honest;
- changed and unchanged claims meet live/coverage evidence rules;
- lease invalidation covers all declared dependency keys;
- end-generation delta links exact changed artifacts to session without causality overclaim;
- temporal omissions/risk remain visible in IR.

**REFACTOR**

- reuse one canonical delta/temporal derivation; no compiler-side reimplementation;
- keep current and temporal budget ledgers explicit;
- preserve stable identity helpers and parameterized persistence.

## Verification commands

```sh
cargo test --locked --test temporal_intelligence_v14
cargo test --locked generation_delta::tests
cargo test --locked task_compiler::tests
cargo test --locked task_session::tests
cargo test --locked
```

## Boundaries

- Always: committed ancestor only, one captured generation pair, bounded details, live current-source verification, typed risk/omissions.
- Ask first: delta/schema vocabulary expansion, carry-forward leases, planner policy change, task-completion public contract.
- Never: reconstruct missing history, compare non-ancestors, call VCS/model/provider on query path, equate changed artifact with evidence use, bypass source verification via lease.

## Success criteria

1. Relevant compiler recipes consume temporal evidence through one canonical implementation.
2. Missing/partial history cannot produce false completeness.
3. Semantic delta categories and unchanged claims are evidence-qualified.
4. Leases invalidate on every declared dependency dimension.
5. Task change linkage supports metrics without causation claims.

## Open decisions

- **H4/H5 resolved:** temporal roles use `planner-v2.0.0` and Context IR `2.0.0`; transient routing stays outside IR.
- **H7:** any delta/lease schema extension and old-catalogue migration.
