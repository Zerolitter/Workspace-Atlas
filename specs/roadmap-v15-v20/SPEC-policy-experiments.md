# Spec: `policy-experiments`

## Objective

Turn V1.5 observations into reproducible evidence for human-reviewed, deterministic working-set policy changes. “Learn what models actually use” means controlled measurement and explicit policy revision—not online learning or hidden runtime adaptation.

## Current-state delta

The base can repeat a compile against one generation and reject comparison across workspace, generation, task kind, planner policy, or projection policy. The retained Golden Nugget A/B reached the same implementation boundary and reduced model discovery work for one task, but it was one model/task, used direct-Truth fallback, and is not a universal claim.

Missing:

- small/standard/audit profile implementation;
- accepted-outcome and validation gating;
- multi-task retained experiments;
- stage latency and full Context Yield measures;
- flat versus relationship-preserving representation A/B;
- machine-readable local export with sample/limitations;
- variance/repeat protocol and policy-promotion gate.

## Experiment contract

### Frozen scenario and run attestation

Each committed benchmark scenario records stable dataset/procedure/profile/acceptance/estimator requirements. Each run generates a package-excluded attestation with actual repository tree/generation, task/acceptance contract, model/agent/tools, resource limits, planner/projection/IR/metric/estimator policy, Serving state, provider configuration, toolchain/hardware/cache state, and raw samples. One experiment varies exactly one declared independent variable.

### Required conditions

- DIRECT: ordinary discovery with zero Atlas context;
- ATLAS_LIGHT: bounded catalogue/source/legacy Context Packet operations without Context IR;
- ATLAS_DEEP: Context IR plus existing exact-source/query tools;
- compared routes must reach the same accepted task outcome and evidence requirement before efficiency comparison; cold bootstrap, Serving build, decision, query/compile, and model-side discovery remain separate.

### Profiles

H3-A fixes private experiment/acceptance profile values, scenario, samples, runners, and statistical gate. H4 promotes none of its small/standard/audit profiles; current compiler defaults and `planner-v1.1.0` remain runtime baseline.

Profile identity participates in experiment/report identity. Future route capability/cost profiles are closed, versioned operational classes without model/vendor names; they affect production only after fixed semantics and identity receive review.

### Required experiment set

1. Reproduce the accepted reconnect/change baseline.
2. Behavior-change under small/standard/audit.
3. API-change task.
4. Review task consuming Generation Delta.
5. Flat versus relationship-preserving rendering of the same evidence manifest and budget.
6. Golden planning task or approved equivalent under ready Serving Plane and direct-Truth fallback, reported separately.
7. Generated 10× graph boundedness experiment.
8. Context Governor matrix from [`SPEC-context-governor.md`](SPEC-context-governor.md): frozen request plus expected initial/final route, reason/deficit/state/outcome/version for DIRECT/LIGHT/DEEP, ceilings, resubmitted generation change, interruption, immutable profile manifests, negotiation, exclusions, and future-skill-compatible discovery.

Sample counts, warm/cold procedure, percentile method, and enforcement are exactly the H3-selected scenario contract. Raw attestations, variance, failures, reverted ideas, and limitations remain local and package-excluded.

## Policy promotion

An experiment may produce an inert proposal containing changed fixed recipe/profile/order/route values, evidence, compatibility impact, expected IDs changed, and rollback. Runtime never reads it.

The reviewed H4 decision rejects current promotion and retains `planner-v1.1.0`/explicit defaults. Any future promotion needs a new H4 decision plus:

- equal accepted outcomes and validation;
- improvement beyond variance on the target metric;
- no worse coverage/uncertainty/omission contract;
- no disproportionate regression across required task kinds/profiles;
- a new fixed planner/route/profile/estimator identity wherever observable selection or escalation changes;
- reproducible fixtures and negative cases;
- rollback to the previous policy version.

Neutral/worse proposals are rejected and logged. Runtime learned/adaptive/file-size-only/model-vendor routing remains prohibited. Any future learned system requires a new ADR, explicit data/retention/privacy model, offline evaluation, shadow mode, bias/poisoning analysis, deterministic fallback, rollback, and separate human approval.

## Output and interfaces

Provide a local machine-readable experiment bundle plus a human-readable summary generated from the same typed data. No raw prompt/source by default. A future CLI/MCP Context Yield surface is owned by `compiler-surfaces`; MCP export may return structured local data but never upload it.

## RED–GREEN–REFACTOR acceptance

**RED**

- rejected and accepted outcomes currently compare;
- profile/presentation variables cannot be represented/frozen;
- one run is currently sufficient for a report;
- policy can be changed without an evidence/promotion record in a naive workflow.

**GREEN**

- required experiment matrix runs from frozen manifests;
- invalid comparisons return typed reasons;
- profiles enforce record/source/token/time/uncertainty bounds;
- results include raw samples, variance, limitations, cold/hot separation, and accepted-outcome proof;
- policy/route proposal remains inert pending a future decision; H4's current outcome promotes nothing.

**REFACTOR**

- separate experiment orchestration, typed results, and policy definition;
- avoid a second compiler implementation in the harness;
- keep render comparisons on an identical canonical evidence manifest.

## Verification commands

```sh
cargo test --locked context_yield::tests
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo run --release --locked --bin atlas-bench -- --manifest tests/fixtures/context_yield/benchmark-scenario.json
cargo test --locked
```

## Boundaries

- Always: one variable, frozen manifest, equal accepted outcome, raw sample/variance, limitations, deterministic proposal/versioning.
- Ask first: any future runtime profile/policy/route promotion, task/model matrix, or utility weighting.
- Never: automatic promotion, online learning, embeddings, model calls, co-access/file-size/latency racing, model/vendor route rules, unequal-outcome claims, hidden bootstrap cost.

## Success criteria

1. V1.5 produces defensible measurements for every required task class and profile.
2. Ready-serving and fallback costs are separately measured.
3. Every policy change is explicit, versioned, reviewed, and reversible.
4. No experiment data becomes canonical project truth or leaves the machine.
5. V2 keeps public `planner-v1.1.0`/explicit defaults while deep V2 uses reviewed fixed `planner-v2.0.0`; later semantic changes mint successors.

## Open decisions

- **H3 — resolved:** H3-A private experiment/acceptance contract.
- **H4 — resolved:** retain current runtime policy/defaults; promote no profile.
- **H5 — resolved:** Context IR `2.0.0` is semantic; rendering/materialization/route execution stays in separately versioned adapters/envelope.
