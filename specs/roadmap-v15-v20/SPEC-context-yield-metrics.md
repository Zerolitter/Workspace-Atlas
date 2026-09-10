# Spec: `context-yield-metrics`

## Objective

Derive transparent working-set and Context Yield measures from typed task/session evidence. A smaller packet counts as better only when compared conditions reach the same accepted outcome and preserve correctness/trust coverage.

## Current-state delta

The base `ContextYieldReport` proves repeated compilation against one frozen generation and compares workspace, generation, task kind, planner policy, and projection policy. It reports selected size/bytes/estimated tokens and fallback. It does not compute supplied/used/expanded/rediscovered sets, stage costs, estimator version, accepted-outcome validity, sample size, or limitations.

## Typed metric contract

Every ratio carries numerator, denominator, unit, evidence class, and validity status. Division by zero returns a typed unavailable reason, never `0`, `1`, NaN, or an invented score.

### Entity sets

- **Supplied working set:** canonical entities/ranges in the initial Context IR delivery.
- **Observed downstream set:** supplied or additional evidence later retrieved/queried/traversed/selected through Atlas.
- **Reported-use set:** evidence explicitly reported by agent/operator as reasoning/modification/test use.
- **Changed-artifact set:** end-generation artifacts linked by Generation Delta.
- **Expanded set:** observed/reported entities requested after initial IR and absent from supplied set.
- **Rediscovered set:** repeated request for still-valid supplied evidence under the same generation/evidence identity.

Sets preserve file/symbol/relationship/range identity separately. File-level change does not mark every symbol/range in that file used.

### Measures

```text
context_precision_observed = observed_used_supplied_items / supplied_items
context_precision_reported = reported_used_supplied_items / supplied_items
context_expansion_count = unique additional items requested after initial delivery
source_efficiency_observed = observed used supplied exact-source bytes / supplied exact-source bytes
rediscovery_rate = repeated still-valid supplied-evidence requests / comparable evidence requests
accepted_change_rate = accepted completed comparable tasks / completed comparable tasks
```

Also report raw working-set counts by entity kind/role/reason/evidence state, relationships, source bytes, estimated tokens with estimator version, omissions, uncertainty reserve, fallback, and validation/outcome state.

A source read contributes to observed retrieval, not reported reasoning. Changed artifacts and selected tests are separate supporting signals. No aggregate “useful” score combines these classes by default.

## Comparison validity

Two reports are comparable only when the following frozen dimensions match unless the experiment explicitly varies one:

- workspace/tree and generation;
- task/acceptance contract and task kind;
- model/agent/tool configuration for model-side A/B;
- planner, projection, Context IR, estimator, and metric-policy versions;
- budget profile except in a declared profile experiment;
- retention/observation coverage sufficient for the metric.

Both outcomes must be accepted and their required validation contract must pass before any efficiency improvement or useful-throughput result is valid. Invalid comparisons return typed reasons listing every mismatch.

## Output

Define one closed, versioned `ContextYieldReport` extension or successor with:

- identity/version/policy fields;
- frozen-input manifest hashes;
- sample/repeat counts;
- correctness and accepted-outcome status;
- raw supplied/observed/reported/changed/expanded/rediscovered counts;
- metric fractions and validity reasons;
- retrieval instrumentation summary;
- limitations and missing-observation coverage;
- no raw prompt/source by default;
- deterministic report-content hash excluding timestamps/timing noise where appropriate.

Context Yield remains a separately versioned derived/local report and may be regenerated while source sessions exist. H5's Context IR `2.0.0` choice does not silently change this report; any report-schema change receives ordinary contract review and cannot embed source, prompt, route-execution, or model/vendor data.

## RED–GREEN–REFACTOR acceptance

**RED**

- same supplied set with one exact-source read currently cannot yield a typed observed precision fraction;
- read-only evidence is intentionally misclassified as reasoning use in a naive test and must fail;
- rejected versus accepted outcomes currently compare numerically;
- zero supplied items currently lacks typed unavailable handling;
- policy/estimator/profile mismatch beyond the existing five dimensions is not rejected.

**GREEN**

- each metric returns exact sets/counts/fractions and source classifications;
- expansion and rediscovery distinguish new from repeated still-valid evidence;
- rejected/failed/unequal-outcome comparisons are invalid;
- report records sample size, limitations, missing observation coverage, and estimator version;
- no raw prompt/source or upload path exists.

**REFACTOR**

- keep pure set derivation separate from persistence and transport;
- reuse canonical entity/evidence identities;
- avoid one polymorphic score that hides evidence classes.

## Verification commands

```sh
cargo test --locked context_yield::tests
cargo test --locked task_session::tests
cargo test --locked --test source_telemetry_surfaces
cargo test --locked --test temporal_intelligence_v14
cargo test --locked
```

## Boundaries

- Always: numerator/denominator, typed invalidity, equal accepted outcomes, source-class separation, local privacy.
- Ask first: metric/report schema version, utility weighting, accepted-outcome definition per task class.
- Never: treat reads as reasoning, treat changed files as proof all supplied items mattered, compare unequal outcomes, report token savings from unlabeled byte estimates, learn ranking from raw events.

## Success criteria

1. Every V1.5 metric is reproducible from retained typed evidence.
2. Missing/partial observation produces explicit invalid/partial status.
3. Equal-outcome and policy/profile comparability is machine-enforced.
4. Raw sets support audit; aggregate metrics do not erase evidence distinctions.
5. Report retention/compaction cannot delete Truth Plane records.

## Open decisions

- **H3/H4 — resolved baseline:** use H3-A experiment discipline; H4 promotes no profile/weighted runtime policy.
- **Report contract:** additive fields versus successor report schema remains a task-local compatibility decision, separate from H5.
- **H7:** persistence/retention of reports versus regeneration only.
