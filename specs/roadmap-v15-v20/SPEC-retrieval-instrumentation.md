# Spec: `retrieval-instrumentation`

## Objective

Make V1.5 performance claims measurable and reproducible. Instrument actual compiler and serving stages, enforce bounded work, and retain hardware/profile/variance context without turning one local run into a public latency promise.

## Baseline evidence at planning base

These baselines answer different questions and MUST NOT be merged into one number.

| Evidence | Frozen condition | Observed baseline | Limitation |
|---|---|---:|---|
| Current `atlas-bench` run | 128-file deterministic fixture, release/locked, Windows host used for this checkpoint | baseline reconcile `72.7 ms`; 15/15 checks; whole benchmark `0.85 s` after build | One run; correctness/incrementality fixture, not Context IR latency distribution. |
| Retained earlier benchmark | same 128-file fixture on the same class of workstation | baseline reconcile `664.3 ms`; 15/15; total `2.85 s` | Older revision/build conditions; demonstrates variance, not regression. |
| Golden Nugget pilot | one frozen planning task, large semantic catalogue, direct-Truth fallback | hot Context IR `6.92 s`, 330 candidates, 40 items, 40 relationships, 812 estimated source bytes/~187 estimated tokens | One task/model; Serving Plane unavailable; timing agent-run, not dedicated harness. |
| Golden Nugget Atlas condition | same accepted planning outcome as control | ~`9m07s` discovery, 32 top-level calls, 9 searches, 13 exact reads, 9 files, ~57 KB exact source | Model-side discovery measurement, not compiler-only latency. |
| Golden Nugget control | conventional discovery, same accepted outcome | `17m49.585s`, 69 calls, 28 searches, 25 exact reads, 19 files, ~105 KB source | One A/B pair; cannot establish general percentage promise. |

Checkpoint environment: revision `17240edbcedf501f0dabfc29fff6d91b132a9eb8`; `cargo run --release --locked --bin atlas-bench`; `rustc 1.94.1 (e408947bf 2026-03-25)`, `x86_64-pc-windows-msvc`, LLVM `21.1.8`; Windows 11 Pro x64; AMD Ryzen 7 7800X3D; freshly generated fixture and catalogue. CPU/filesystem cache state was not controlled, so this is a one-sample observation, not a reproducible threshold. H3 must select a stable benchmark scenario (dataset/procedure/profile/acceptance rules) and a generated package-excluded run attestation (actual commit/toolchain/platform/hardware, tree/generation/provider/Serving/cache state, raw samples, percentile method).

Current implementation gap: `IrCost.elapsed_ms` is always `0`; `query_stage_metric` exists in schema but has no writer; serving reports rows but no stage times; compiler hard latency inputs are validated but not enforced.

## Measurement contract

Record monotonic elapsed time and counts for:

- seed resolution;
- Serving Plane lookup;
- graph expansion;
- deterministic ranking/order;
- exact-source verification and read separately;
- packing/serialization;
- candidate and expanded-edge counts;
- cache/serving hits and misses/fallback;
- selected/verified source bytes;
- estimated tokens plus estimator version;
- returned records and omissions/truncation;
- Serving build stages, canonical input rows, output rows/bytes, and rebuild hash;
- immutable governor decision/start-generation separately from execution attempts/final route/deficits/state, Atlas calls, and per-route work.

Stage/route metrics are diagnostic, excluded from decision/IR hashes. Every attempt records starting generation observed/unavailable; DIRECT creates no context identity. Timing/cache/retry/interruption never changes decision. Counters/reasons are closed and non-conflated.

## H3-selected target measurements

H3-A fixes the private scenario, profile budgets, samples, runners, and statistical gate. H4 promotes none of those profiles into runtime defaults. The measurements below enforce acceptance evidence, not a public SLO or adaptive route input.

1. **Instrumentation coverage:** 100% of successful, deterministic-semantic-partial, and interrupted compiler executions write a metric record or typed unavailable reason with internally consistent non-negative stage/count totals; no stage field is fabricated. Controlled-clock tests prove attribution without requiring every real stage to round above zero.
2. **Determinism:** the H3-selected repeated completed compilations per frozen scenario produce one context hash/order/omission result; timing varies and is excluded from identity. Wall-clock-interrupted executions do not claim a canonical Context IR hash.
3. **Hot path:** ready-serving/fallback measurements prove zero workspace walks, parses, provider spawns, and resolution calls.
4. **Canonical fallback:** honor the H3-selected wall cancellation by returning a typed interrupted execution envelope rather than timing-dependent canonical partial IR; deterministic work-unit budgets own semantic partial/omission output. The Golden `6.92 s` result remains a one-task baseline to beat or bound, not a ceiling.
5. **Scaling:** with constant selected profile/budget, p95 compile time and returned records remain bounded when the generated graph grows by 10×; repository-scale stages remain absent.
6. **Serving rebuild:** identical generation/schema/policy rebuild yields identical content hashes/counts; record p50/p95 build time and bytes under H3-A, but do not promote an optional cache or runtime profile without a future decision.
7. **Regression discipline:** use the H3-selected scenario/attestation samples and formula; changes inside noise, worse, or correctness-red are reverted and logged.
8. **Product baseline:** rerun the Golden task or approved equivalent with the same accepted outcome. Report raw A/B counts and limitations; no percentage claim from unequal outcomes or changed model/tool conditions.
9. **Route matrix:** freeze request, expected initial/final route, reasons/deficits/state/outcome, and versions for DIRECT, LIGHT, DEEP, ceilings, caller-resubmitted generation change, Serving fallback, interruption, profiles, and discovery negotiation. Equal deep semantic inputs must yield the same IR.
10. **Exclusions:** prove time/cache/file size/model/vendor/prior outcome cannot select/escalate. No H3-A promotion.

## Persistence

T28 persists no decision/envelope. T17 may write only existing typed numeric/opaque-request route stages when current schemas and H2 allow; it never stores raw task/seed/path, predictable identifier hashes, prompt/source/materialized bytes/digests, model/vendor data, or arbitrary detail. Any new durable route field/vocabulary blocks for H7.

## RED–GREEN–REFACTOR acceptance

**RED**

- assert metric presence and controlled-clock attribution;
- detect prohibited workspace/provider/parse/resolution work on compiler hot path;
- demonstrate wall overrun lacks typed interrupted envelope;
- grow a deterministic unrelated-evidence fixture under fixed budget;
- demonstrate DIRECT generation observation/unavailability and zero-context success cannot coexist;
- demonstrate time/cache/file size can alter decision or initial/final route is conflated.

**GREEN**

- stage/count metrics are consistent on ready/fallback/partial/interrupted paths;
- semantic exhaustion yields canonical partial; wall expiry yields interrupted envelope;
- prohibited hot-path work remains zero;
- frozen repetitions preserve decision, envelope semantics, and deep IR;
- attestations retain raw samples/versions/expected reasons/outcomes outside package;
- DIRECT emits zero context while observing generation/unavailability.

**REFACTOR**

- one low-overhead timer/counter collector owns stage accounting;
- no avoidable allocation/copy is added to the hot loop;
- instrumentation can be disabled only if the public cost contract remains accurate; never fill zeros as success.

## Verification commands

```sh
cargo test --locked task_compiler::tests
cargo test --locked serving::tests
cargo test --locked context_yield::tests
cargo run --release --locked --bin atlas-bench -- --manifest tests/fixtures/context_yield/benchmark-scenario.json
cargo test --locked
```

Add a dedicated release-mode performance harness for distributions; ordinary unit tests assert invariants, not wall-clock thresholds.

## Boundaries

- Always: monotonic clocks, frozen inputs, cache-state labels, raw values/variance, accepted-outcome gate, local-only storage.
- Ask first: normative profile thresholds, persistent metric schema changes, CI hardware gates.
- Never: optimize before measuring, hide cold/bootstrap cost, claim tokens from bytes without estimator label, keep neutral complexity, publish one-run performance promises.

## Success criteria

1. Every compiler/serving result reports real internally consistent execution measurements or a typed unavailable reason.
2. Frozen semantic output stays deterministic while timing remains outside identity.
3. Hard budgets return truthful bounded partial results.
4. Hot-path cost tracks the bounded frontier, not repository size.
5. Performance decisions use retained raw samples/variance and revert neutral or worse complexity.

## Open decisions

- **H3:** benchmark machine classes, sample counts, permitted variance, and whether 150/250/2000 ms profile ceilings become normative.
- **H7:** any metric-schema extension and retention period.
