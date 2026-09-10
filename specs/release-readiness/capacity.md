# Capacity evidence and methodology draft

## Claim boundary

This repository-only, package-excluded draft defines how to gather reviewable capacity evidence. It does not establish a public SLO, supported repository size, hardware requirement, release readiness, runtime profile/default, or performance guarantee. The fixed H3-A profile ceilings and regression rule are private acceptance budgets in the [decision checkpoint](../roadmap-v15-v20/decision-checkpoint.md#h3-benchmarkprofile-recommendation), not user-facing promises. The [README compatibility table](../../README.md#from-a-task-to-evidence) remains the shared compatibility authority.

## Evidence currently available

The accepted T22/T23 local evidence observed one Windows environment only:

- Windows 11 Professional x64 `10.0.26200`, AMD Ryzen 7 7800X3D;
- Rust/Cargo `1.94.1`, Python `3.14.3`, Node `24.14.1`;
- the deterministic pilot benchmark passed `15/15` checks;
- its generated baseline reconciled `128` eligible/indexed files in `36.8 ms`;
- the complete benchmark invocation took `17.99 s` after build.

These are historical observations bound to the accepted run's revision, fixture, toolchain, cache/build state, and runner. They are not a current rerun, capacity envelope, cross-platform result, or public SLO. The configured Windows, macOS, and Linux private-CI legs were source-audited, but hosted macOS/Linux/Windows jobs were **not executed** for that acceptance. Configuration evidence must not be described as observed platform evidence.

No accepted small/medium/large repository dataset series currently reports all required reconcile, route, memory, catalogue-growth, and provider-overhead measures. Capacity readiness therefore remains incomplete.

## H8-E0 approved private candidate hypotheses

H8-E0 was **accepted on 2026-09-05** as bundle A–C only. It freezes test hypotheses and bounded evidence work; it does not satisfy final H8, declare a supported platform or repository limit, or authorize release/publication.

### Candidate platform, client, and provider matrix

Correctness, deterministic identity, bounds, package, privacy, lifecycle, and ready-versus-Truth equivalence are required on GitHub-hosted runner-default Windows, Ubuntu, and macOS. Every result records actual `runner.os`, `runner.arch`, image, CPU, and resolved Rust patch version. Rust `1.94` and `stable` are both required on all three operating-system legs. Enforced latency waits for later selected, dedicated/pinned Windows x64 and Linux x64 runners and three accepted H3-A baseline repetitions; macOS runner-default latency is advisory.

Exact Inspector `2.5.0` and Codex CLI `0.151.0` client paths are required on all three hosted operating systems. The built-in deterministic provider and TypeScript provider `scip-typescript` `0.4.0` are required. Rust provider `rust-analyzer` `1.94.1` is optional/degraded until it completes semantically; timeout, unsupported semantic output, or platform absence is retained as explicit non-success evidence. Installation/download time is reported separately from Atlas work, and isolated provider results remain separate from the combined provider set.

### Immutable private fixture strata

The existing [`generate_fixture.py`](../../tests/fixtures/pilot/generate_fixture.py) is parameterized rather than replaced by a second generator. Counts below are exact eligible/indexed-file counts, and changed sets are deterministic manifests containing the same reviewed semantic-edit, rename, and delete operation classes.

`repo-small-v1` retains the existing pilot language/artifact distribution and excluded generated/vendor/dist/archive/secret/binary cases; built-in and TypeScript are required, while Rust is not applicable unless a Rust slice is added. `repo-medium-v1` and `repo-large-v1` each use 75% TypeScript source/test, 15% Rust source/test, 5% documentation, and 5% configuration with excluded classes outside the eligible count; built-in and TypeScript are required, Rust is optional/degraded, and the combined provider condition is reported separately.

| Private dataset label | Eligible/indexed files | Changed files |
|---|---:|---:|
| `repo-small-v1` | 128 | 2 |
| `repo-medium-v1` | 1,024 | 11 |
| `repo-large-v1` | 8,192 | 82 |

Each immutable v1 fixture manifest records these exact closed fields: `schema_version`; `generator_version`; `seed`; a full sorted `relative_path` plus `sha256` list; `manifest_sha256`; eligible, indexed, excluded, initial, changed-file, changed-byte, and total-byte counts; language and artifact-class distribution; symbol, relationship, effect, diagnostic, coverage, and conflict counts; project markers; exact provider descriptors, versions, and required/optional state; target identity; expected accepted-outcome hash; correctness/deterministic-identity expectations; and expected ready-versus-Truth result hash. These files and their original accepted provenance remain immutable. Their accepted-outcome fields are historical fixture arithmetic, not production Truth-row cardinalities.

The superseding [`accepted-outcomes-v2.json`](../../tests/fixtures/capacity/accepted-outcomes-v2.json) contract binds each applicable dataset/provider condition to separate baseline and fixed-incremental production outcomes. It counts distinct active-generation `symbol_fact`, `relationship_fact`, and `effect_fact` identities through their extractor revisions, plus generation-bound diagnostic, coverage, and conflict rows; it also binds the exact current-symbol target, live source digest, provider membership, configuration hash, eligible/indexed count, and source bytes. Before any campaign writes raw evidence, the production executor must run one clean disposable no-change or fixed-incremental attempt for every applicable dataset/provider/Truth-state outcome and match every bound field exactly. A missing, stale, resealed, or mismatched contract fails before campaign evidence; test executors cannot satisfy this gate.

A digest or count mismatch fails before execution.

The private dataset labels and their measured outcomes are not supported limits or public SLOs.

### Platform-native process-tree memory method

The package-excluded capacity harness samples every **50 ms** from process start through clean exit. Each sample records monotonic timestamp, phase, Atlas PID, descendant provider PIDs, Atlas resident measure, provider resident measures, and the aggregate concurrent resident measure. Reports derive Atlas-only, provider-only, and aggregate concurrent peak values without adding peaks observed at different timestamps; exited or missed children and sampling gaps remain explicit.

- Windows records `WorkingSet64` and `PrivateMemorySize64` and enumerates descendants by parent PID.
- Linux records `/proc/<pid>/status` `VmRSS` and retains `VmHWM` at exit when available.
- macOS records resident bytes through the platform `ps` interface; runner-default values remain advisory until a stable pinned runner/method is accepted.
- A five-second idle control at the same cadence records sampler/tool baseline overhead and operating-system noise; it is not subtracted invisibly.
- Every run labels cold/warm filesystem cache, Atlas Serving state, provider cache/install state, build state, and catalogue state. Raw private samples are retained and summaries report p50, p95, and peak.

Successful new runs use the `process-tree-memory-segmented-summary` **2.0.0** contract. Raw samples remain in one globally ordered sequence but are written to deterministic ordinal `capacity-memory-raw-v2-*.ndjson` segments; each physical segment independently retains at most 200,000 samples and 512 MiB. A run has an explicit maximum of **1,024 segments**: the writer fails before creating segment 1,025, keeping the fixed seven-field descriptor list, validation handles, six-digit ordinal namespace, and serialized summary comfortably within the 1 MiB summary and practical filesystem/runtime bounds. The summary binds that limit and every segment's relative path, ordinal, first and last global sequence, record count, byte length, and SHA-256, plus the concatenated whole-run record count, byte length, and SHA-256. Sampling retains only current-record/scalar state plus at most 1,024 bounded descriptors and uses a package-excluded temporary SQLite metric store for exact whole-run percentiles; neither sampling nor validation retains the full record sequence in memory. The authoritative Python state machine opens every selected summary and segment through a no-follow, non-reparse physical handle, checks regular-file, byte-length, and live single-link state immediately before and after consuming the exact bytes, reconstructs baseline/campaign/events and all record semantics, and streams those accepted bytes through a private framed pipe. Rust writes the frames directly to bounded package-excluded temporary files on the evidence volume, rejects malformed, truncated, extra, duplicate, missing, or reordered frames and child failure, and consumes the same still-open spool handles only after validation succeeds, with another pre/post physical check. No memory payload is logged before the complete framed generation succeeds; spool files are removed on success or error. A centralized filename classifier closes the v2 namespace to the selected summary and declared segments, including broken linked entries, while explicit legacy validation rejects every colocated v2 raw or summary artifact. The prior single-file **1.0.0** evidence validator remains available for explicitly selected already-retained short-run evidence, but new production evidence is always schema v2.

RSS/working set is not private ownership and is not directly comparable across operating systems; private bytes omit shared/mapped pressure. Reports state both limitations and never claim that operating-system page cache is Atlas residency. This observer adds no Atlas runtime RSS/page-fault field, schema change, scheduler, cache mode, or EXP-DPPM behavior.

### Raw hosted evidence retention

The first method is **bounded redacted private Actions log emission** from the two existing pull-request workflows, with no new upload-artifact dependency. Logs must include every required raw sample or explicit non-success state while redacting credentials, environment values, private absolute paths, prompts, and source bodies. If later evidence cannot be retained completely within reviewed bounded logs, execution stops for a separate private artifact-upload decision rather than truncating evidence or silently changing method.

Segmented memory streams are authoritatively validated while their exact bytes are streamed into bounded disk spools and are emitted as separately named bounded streams from those same open spools only while their concatenated source remains within the reviewed private-log source limit. Exceeding that separate log-retention limit or any source, framing, semantic, namespace, physical-link, or child-process failure discards the spools and fails before any payload line; it never terminates sampling, truncates a segment, or changes the 50 ms method.

## Capacity methodology

### Dataset strata

Use at least three fixed, versioned repository fixtures representing small, medium, and large workloads. Do not assign those labels from source bytes alone. Each fixture attestation must record:

- generator/source revision and immutable manifest digest;
- eligible/indexed/excluded file counts and bytes;
- language and artifact-class distribution;
- symbol, relationship, effect, diagnostic, coverage, and conflict counts;
- initial and changed-file/change-byte counts for the incremental case;
- provider set, exact versions, required/optional status, and project markers.

Until those manifests and boundaries are reviewed, “small”, “medium”, and “large” are dataset labels only—not supported limits.

### Environment and procedure

For every OS/architecture/toolchain/provider condition, record the exact Atlas revision, Rust/Cargo/Python/Node versions as applicable, CPU, logical cores, memory, storage class, runner identity, power/virtualization constraints, application-data/catalogue path class, configuration hash, provider state, Serving state, and cache state.

Use the stable benchmark scenario in [`benchmark-scenario.json`](../../tests/fixtures/context_yield/benchmark-scenario.json) for its defined fixture/profile procedure. Preserve its five warmups, 100 scored warm samples, 20 fresh-fixture cold samples, three independent repetitions, nearest-rank percentiles, accepted-outcome requirement, and attestation rules. Do not silently mix unequal outcomes, manifests, generations, profiles, providers, or cache states.

For each repository stratum, separately measure:

1. fresh initialization and cold reconcile;
2. no-change reconcile;
3. fixed changed-file incremental reconcile;
4. ready-Serving and Truth-fallback query/compiler behavior;
5. capability discovery and fixed DIRECT/LIGHT route cases; DEEP only if discovery actually advertises it;
6. built-in-only, each optional provider, and the reviewed combined provider set;
7. catalogue size before/after reconcile and disposable Serving build/rebuild.

### Required measures

Retain raw per-sample data and compute warm p50/p95 with the scenario's nearest-rank method. Report, by named condition:

- wall duration and deterministic work units for reconcile and route/compiler stages;
- eligible/indexed/changed counts, selected records, source bytes, estimated tokens, truncation/omission and fallback state;
- peak process memory using one documented platform-specific sampling method and cadence, plus baseline/tool overhead;
- catalogue bytes and row counts by durable versus disposable/derived category;
- provider probe/run duration, peak memory, output bytes, exit/degradation state, and incremental reuse;
- correctness, deterministic identity, accepted outcome, and ready-versus-Truth equivalence.

Measured wall time, retries, cancellation, caches, and route attempts are transient evidence and never part of canonical Context IR identity.

### Statistics and comparison

Apply the accepted H3-A arithmetic exactly: each repetition yields one warm p95; the baseline is the median of three baseline-repetition p95 values; baseline MAD is the median absolute deviation of those values. A private acceptance breach requires the accepted absolute or relative condition in at least two of three candidate repetitions. Cold percentiles remain advisory. Report missing samples, failures, optional-provider degradation, confidence limitations, and environmental drift; never replace them with a successful aggregate.

## Provider and network effects

Atlas does not install external providers. Provider installation and execution can download packages, execute third-party code, populate caches, and use the network. Measure install/download separately from Atlas probe/index time. Record offline/warm-cache/cold-cache state and host isolation; do not attribute package-manager work to compiler latency or claim Atlas's best-effort network policy is isolation. A provider unavailable locally is evidence of degraded coverage, not a zero-cost successful provider run.

## Memory and EXP-DPPM restrictions

[EXP-DPPM-001](../roadmap-v15-v20/research-backlog.md#exp-dppm-001-demand-paged-project-memory) is research-only. Current eager/lazy behavior has not been established; SQLite and the operating system may already demand-page data. This capacity methodology authorizes no prototype, production implementation, RSS/page-fault metric, schema field, cache mode, scheduler, runtime selector, fragment cache, or provider/model/history/co-access policy. New memory instrumentation and any eager/lazy/hybrid experiment require the backlog's baseline prerequisites and separate human approval.

## Evidence publication and remaining gates

Raw results belong in private, package-excluded evidence bound to exact revisions and named runners. Redact credentials, environment values, private absolute paths, prompts, and source bodies. Do not upload or publish reports from ordinary CI.

Capacity readiness requires completed small/medium/large datasets, locally and hosted-executed platform legs, reviewed peak-memory methodology, provider overhead, cross-run variance, limitations, and independent H8 review. Signing, SBOM, support channels, distribution channels, public artifacts, and release claims remain separate unapproved decisions.
