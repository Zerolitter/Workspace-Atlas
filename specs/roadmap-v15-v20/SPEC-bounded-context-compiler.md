# Spec: `bounded-context-compiler`

## Objective

Compile H5-approved Context IR `2.0.0` for the ATLAS_DEEP route from pre-resolved generation evidence with deterministic seed resolution, task-recipe sufficiency, bounded expansion, uncertainty reservation, and truthful hard-limit behavior. DIRECT and ATLAS_LIGHT remain valid non-IR outcomes under the Context Governor.

## Current-state delta

At base, the compiler has 10 fixed task kinds, caller declaration precedence, stable whole-word classifier rules, fixed recipes, canonicalized seed sets, explicit-symbol-first bounded BFS, role/reason/rank, record/source/token budgeting, omission accounting, and ready-Serving/same-generation Truth fallback. It invokes no provider/parser/resolver and uses no learning.

Gaps relative to the intended compiler:

- seed matching still allows display-name paths without a first-class typed ambiguity contract;
- no bounded FTS candidate step;
- recipes mostly toggle callers/callees/tests/config/depth and do not require effects/history/exact source/validation roles;
- no explicit uncertainty reserve;
- no hard-time cutoff despite budget fields;
- no small/standard/audit profiles;
- broad path enumeration may consume budget without utility/sufficiency feedback;
- current item ordering is fixed but not measured against required-role closure; changing it, roles, or sufficiency requires a new fixed planner identity.

## Compiler contract

### Inputs

Internal V2 deep request requires explicit positive record/source-byte/token/depth/work-unit limits and uncertainty reserve, plus fixed `planner-v2.0.0`, projection/IR/estimator versions, task/seeds, and retained generation if allowed. Missing limits fail closed; no H3-A/public default fills them. Route/start generation and transient execution fields stay outside IR.

### Seed resolution order

1. exact canonical symbol key;
2. exact canonical path;
3. exact qualified name;
4. exact display name with all ambiguity retained;
5. bounded FTS candidate list with candidate/omission counts;
6. partial/blocked—never broad repository scan.

Explicit seeds remain ahead of derived candidates. Ambiguous display/FTS matches are not promoted to verified primary targets without disambiguation evidence.

### Selection

- Read ready Serving projection or same-generation canonical evidence only.
- Evaluate `planner-v2.0.0` required/optional roles for the declared/classified kind.
- Expand version-pinned ready direct edges only; no default transitive closure.
- Select by the frozen required-role deficit, trust, distance, role priority, cost, canonical identity order.
- Stop at required-role satisfaction or explicit semantic bound; later selection change requires new planner identity.
- Preserve conflicts/unresolved/coverage/staleness/omissions; no predictive truth.

### Budgets

Record, source-byte, estimated-token, relationship-depth, deterministic work-unit, and uncertainty-reserve limits are explicit and independently enforced. Effective budgets/estimator identity participate in IR policy identity. Unused reserve may be released only by the fixed `planner-v2.0.0` rule.

Wall deadline is execution cancellation only. T24 enforces T28's `context-execution-v2.0.0`; expiry returns interrupted envelope without timing-dependent partial IR/hash and never reroutes.

### Profiles

H4 retains legacy explicit defaults and promotes no H3-A profile. V2 requires explicit semantic limits until a future reviewed runtime profile/default exists. Future manifests are immutable/digested and can only be tightened by caller overrides.

## No learning or adaptive routing

Candidate order and recipes are fixed source/config policy. Context Yield data may support a reviewed proposal but is never read by runtime selection. Context Governor uses fixed `context-route-v2.0.0` signals/reasons and never model/vendor names, file size alone, measured latency, embeddings, frequency, co-access, outcomes, or user-specific hidden state.

## RED–GREEN–REFACTOR acceptance

**RED**

- duplicate display names demonstrate ambiguous selection;
- bounded FTS candidate fixture has no supported path;
- required effect/test/source/validation evidence can be absent without precise status;
- uncertainty can be dropped after record exhaustion;
- forced slow stage exceeds hard deadline;
- generated 10× graph exposes any repository-scale work.

**GREEN**

- seed-order and ambiguity fixtures follow the exact typed contract;
- recipes satisfy or name every required-role deficit;
- all five budgets and uncertainty reserve are enforced with exact omissions;
- ready-serving and fallback preserve semantics;
- prohibited hot-path operations remain zero;
- repeated runs that complete under the same deterministic semantic budgets produce identical IR/hash; wall-clock interruption returns the typed non-canonical envelope.

**REFACTOR**

- split policy/seed/selection/packing from orchestration if needed rather than enlarge `src/task_compiler.rs` indefinitely;
- use borrowed/canonical data and bounded collections; avoid repeated SQL preparation or copies in hot loops;
- one omission ledger reconciles candidates, selected, deferred, and dropped counts.

## Verification commands

```sh
cargo test --locked task_compiler::tests
cargo test --locked --test task_compiler_v13
cargo test --locked serving::tests
cargo run --release --locked --bin atlas-bench
cargo test --locked
cargo fmt --all -- --check
```

## Boundaries

- Always: explicit seeds first, bounded pre-resolved evidence, deterministic policy, uncertainty reserve, typed partial/blocked, exact omissions.
- Ask first: any successor planner/route policy, FTS policy, deterministic work-unit change, execution-envelope change, or runtime profile promotion.
- Never: repository walk/parse/provider/SCIP/resolution on query path; learned/adaptive/file-size-only routing; silent ambiguity; unbounded closure; timing-dependent partial contents presented as canonical IR.

## Success criteria

1. All required task recipes produce sufficient evidence or exact deficits.
2. Work tracks explicit semantic bounds/frontier, not repository size.
3. Every semantic omission is typed/countable, and every wall-clock cutoff is typed without a canonical partial hash.
4. No runtime feedback or learned state influences selection.
5. Context identity changes only through explicit versioned inputs/policy.

## Decision status and remaining gates

- **H3/H4 resolved:** legacy defaults stay; V2 uses explicit limits and `planner-v2.0.0`; no H3-A promotion.
- **H5 resolved:** IR owns semantics; T28 envelope owns FTS diagnostics/routes/materialization/deadline/retry/cancellation/cache/time.
- **H6B:** any public V2 budget defaults/profile selection.
- **Context Governor:** deep remains conditional under [`SPEC-context-governor.md`](SPEC-context-governor.md).
