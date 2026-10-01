# Context qualification harness foundation

The Stage 2 qualification work has two distinct layers. The reporting and
normalization layer is offline and dependency-free: it validates retained
observations, derives typed metrics, emits content-addressed reports, and
rejects incomparable pairs without running agents, Atlas routes, validators,
tests, or shell commands. Separately,
`scripts/run-context-qualification-pilot.py` executes baseline, model, and
candidate validation for exactly two closed tasks; its `verify` phase is
offline integrity reconstruction, not another execution. Packaged JSON inputs
remain synthetic contract fixtures unless explicitly identified as retained
runtime evidence.

The harness schemas are unpublished harness contracts. They do not modify the
public Context IR `1.0.0` or `2.0.0`, Context Yield, Truth Plane, Serving Plane,
CLI, or MCP contracts.

## Files and versions

- `tests/fixtures/context_qualification/corpus-v1.json` is the frozen eleven-task
  corpus at Git commit `f5f5c1e500c34fb9eeda6b8aa97ef24bd964246a`.
- `tests/fixtures/context_qualification/accepted-a-v1.json` and
  `accepted-b-v1.json` are synthetic, attested input examples. They differ only
  in the declared controlled budget dimension.
- `scripts/qualify-context-evidence.py` implements harness, input, observation,
  receipt, report, comparison, and metric-policy version `1.0.0` contracts.
- `scripts/test_qualify_context_evidence.py` covers the bounded parser, metrics,
  receipts, report reconstruction, comparison gates, frozen Git objects, and
  the file-based CLI path.
- `scripts/normalize-native-context-evidence.py` converts retained native
  Context IR `1.0.0` acquisitions into a closed acquisition-only normalization;
  `scripts/test_normalize_native_context_evidence.py` qualifies that private
  adapter contract.
- `scripts/run-context-qualification-pilot.py` implements the fixed two-task
  phased execution/verification contract; its trusted registry, encoded probes,
  and validator contracts live under `scripts/fixtures/context_execution/`,
  with focused qualification in
  `scripts/test_run_context_qualification_pilot.py`.

The scripts are repository-source tools and are intentionally outside the Cargo
package. This guide and the JSON fixtures are packaged.

## Commands

Run the focused suite from a repository checkout:

```powershell
python -B -m unittest scripts.test_qualify_context_evidence
python -B -m unittest scripts.test_run_context_qualification_pilot
```

Generate two reports and compare them offline. Output files must not already
exist; all paths must remain inside `--workspace`.

```powershell
New-Item -ItemType Directory -Force .local-atlas-runs/qualification
python -B scripts/qualify-context-evidence.py report --workspace . --corpus tests/fixtures/context_qualification/corpus-v1.json --input tests/fixtures/context_qualification/accepted-a-v1.json --output .local-atlas-runs/qualification/report-a.json
python -B scripts/qualify-context-evidence.py report --workspace . --corpus tests/fixtures/context_qualification/corpus-v1.json --input tests/fixtures/context_qualification/accepted-b-v1.json --output .local-atlas-runs/qualification/report-b.json
python -B scripts/qualify-context-evidence.py compare --workspace . --left .local-atlas-runs/qualification/report-a.json --right .local-atlas-runs/qualification/report-b.json --output .local-atlas-runs/qualification/comparison.json
```

The CLI reads only bounded JSON documents. It rejects duplicate members,
non-finite numbers, links/reparse points, path escapes, unknown fields,
bool-as-integer values, unsupported versions, oversized collections, and
signed-63-bit counter overflow. Corpus and input documents contain no executable
command field.

## Native acquisition normalization adapter

The source-only native adapter normalizes retained acquisition evidence without
turning it into a benchmark task execution. Its closed output schema is
`atlas-native-acquisition-normalization-v1.0.0`, with
`native_operation="context-ir"`, `governor_route=null`,
`task_execution=null`, and `acceptance=null`. A successful CLI run proves only
that retained inputs satisfied this adapter contract. It does not make an
acquisition sufficient, accepted, current, model-supplied, or useful.

Run it with a caller-created descriptor and evidence directory:

```powershell
python -B scripts/normalize-native-context-evidence.py --evidence-workspace .local-atlas-runs/stage2-native-20261001 --source-root <frozen-source-root> --descriptor descriptor.json --output normalized-context.json
python -B -m unittest scripts.test_normalize_native_context_evidence
```

The descriptor names bounded capture streams, before/after status documents,
the current catalogue snapshot, retained null model/agent configurations,
optional command-capture metadata, frozen source files, and optional benchmark
corpus/candidate locators. Every dependency carries an expected SHA-256 and is
rechecked while read. Paths are canonical relative paths contained by either
the evidence workspace or the separately constrained source root. Output is an
exclusive new file under the evidence workspace. The adapter executes no
captured command, opens no network connection, mutates no catalogue or source,
and materializes no source for a model.

`descriptor.repository_commit` is a declared value, not a verified source-root
Git binding. The adapter executes no Git command and reads no Git object. Output
therefore labels it `declared_only` and leaves `source_root_git_proof=null`.
When a benchmark corpus is supplied, its repository commit must match the
descriptor, but that consistency check still does not prove the source root's
Git identity. Source bytes, catalogue state, workspace, configuration, and
generation bindings retain their separately verified scopes.

Adapter capability is deliberately limited to native Context IR `1.0.0`.
Context IR `2.0.0` returns an adapter-unsupported error because its
domain-separated, length-prefixed identity and proof model differ. Unknown
future versions fail closed. This adapter limit does not narrow Atlas's public
support for Context IR `1.0.0` or `2.0.0`.

For IR `1.0.0`, `context_hash` is SHA-256 over compact, recursively
sorted-key UTF-8 JSON for the document excluding `context_hash`, with no
terminating newline. Native file and source-tree content hashes are SHA-256 over
raw bytes and the documented source-tree record stream. Blake3 is limited to
specific opaque native identifiers; a 64-hex spelling alone never selects a
hash algorithm.

The adapter preserves native entity kind and `entity_id` verbatim. Canonical
entity identity has `range=null`; native exact-source byte offsets remain
mapping/provenance. Every source reference retains its full native byte range,
indexed hash, observed hash, evidence state, uncertainty/conflict data, and
`verification_status`. Bounded frozen source reads verify raw byte hashes,
UTF-8 boundaries, and line mapping, but remain acquisition-time mapping work:
they are not model source delivery or live source verification.

The adapter derives the referenced source-path set from native IR before source
reads. Missing descriptors and unrelated descriptor sources fail closed.
Declared source sizes must satisfy both the per-file 16 MiB bound and an
independent 32 MiB aggregate bound before any source content is opened; actual
bytes are accumulated and rechecked against the same aggregate bound after
stable reads. `MAX_EVIDENCE` never multiplies the memory ceiling.

Descriptor and output CLI arguments must themselves use canonical
forward-slash root-relative spelling; absolute paths, `..`, dot components, and
linked/reparse ancestors fail before file access or creation.

An item without a native source reference must report zero source-span cost.
Selected record, source-byte, and token totals must not exceed their native
`max_*` budgets, even when a resealed stream and catalogue agree.

Selected source-span bytes and delivered source bytes remain separate. Query
metrics may establish returned records/tokens, expanded edges, stage timings,
and `source_bytes_read` only when exactly one row binds the Context IR
`request_hash`, workspace, generation, and task session. Native
`elapsed_ms=0` is a hardcoded legacy value and is emitted as unavailable timing.
CPU, I/O, database steps, provider scopes, Governor route/work units, source
fence, task execution, and acceptance remain null when not measured.

Before/after status must bind one stable committed generation, workspace, and
configuration. The catalogue is accepted only as a stable, digest-bound
read-only immutable snapshot with no WAL ambiguity. Exactly one persisted
Context IR row, task session, committed generation, workspace, and query metric
row must agree with the capture. The generation's stored source-tree hash is
labelled indexed generation evidence; a null IR source-tree hash stays null and
no live before/after source fence is inferred.

When IR carries a non-null SHA-256 `source_tree_hash`, it must equal the
committed generation's stored tree hash. A match remains indexed generation
evidence—not a live before/after source proof.

Optional benchmark association remains separate from task execution. A mapping
requires exact native identity correspondence, matching source path and
revision, and coverage of the entire required corpus range. A partial TOML key
does not map to a whole-file configuration locator. Unmatched evidence retains
`benchmark_mapping=null` plus an explicit reason, and all native evidence
records remain present.

Malformed or changed digests, unsupported versions, duplicate/unknown JSON,
non-finite or overflowing numbers, path escapes/links/reparse points, invalid
UTF-8 or byte offsets, changed bindings, ambiguous database rows, and false
mapping/delivery/timing/acceptance claims fail closed. Synthetic test captures
exercise blocked empty and partial 40-record inputs but are not measured Atlas
qualification evidence.

## Executed two-task pilot runner

`scripts/run-context-qualification-pilot.py` is a source-only execution runner
for exactly `single-file-canonical-id-whitespace` and
`configuration-project-marker-validation`. Its registry, trusted Rust probes,
test names, source allowlists, Cargo targets, task prompts, and acceptance
partitions are fixed under `scripts/fixtures/context_execution/`. Any other task
or input field fails before a subprocess or workspace mutation.

The CLI has four phases:

```text
run-context-qualification-pilot.py baseline <all frozen paths and executable identities>
run-context-qualification-pilot.py model --manifest <frozen-run-manifest>
run-context-qualification-pilot.py validate --manifest <frozen-run-manifest>
run-context-qualification-pilot.py verify --manifest <frozen-run-manifest> --receipt <executed-receipt>
```

The baseline phase is the only phase accepting operator bindings. It freezes
the task, corpus specification and criteria, base commit, registry, probe,
validator contract, model prompt, runner, unchanged process guard, Git/Cargo/
Atlas/OMP executables, retained model/agent configuration documents, explicit
OMP instruction file, acquisition checkout/catalogue, three pre-created
checkouts, scratch/target/evidence roots, and the exact inherited qualified
toolchain environment. Following Cargo's documented
[hierarchical lookup](https://doc.rust-lang.org/cargo/reference/config.html#hierarchical-structure),
the runner enumerates both `.cargo/config` names at the baseline and validation
Cargo working directories and every ancestor, plus both Cargo-home names and
Rustup `settings.toml`. Present and absent paths are frozen; additions,
removals, content changes, environment drift, links/reparse points, and
overlapping Cargo roots fail closed before execution. A discovered top-level
Cargo `include` fails with an explicit unsupported-input error rather than
silently omitting its recursive inputs. Only path, presence, size, and digest
observations are retained, never external configuration contents. Offline
verification reconstructs those sealed observations but does not claim to
re-prove live absence. The target container must be fresh and empty at freeze.
The runner reserves fixed `baseline` and `validation` children, binds each Cargo
argv to its checkout role, and permits reuse only for retries or subsequent
checks against that same frozen source tree. Cross-role, foreign, container-root,
and pre-populated targets fail closed. This private contract has no reused-cache
mode and makes no cold-latency claim. Exact weights revision remains nullable.
Model/agent configuration is declared metadata, not proof that OMP applied it.
Explicitly operator-bound instruction and configuration files are private run
evidence, not public package content. They retain exact bounded UTF-8 bytes,
may contain legitimate local absolute paths, and still reject NUL, credentials,
links/reparse points, identity drift, and unknown configuration fields. Public
source and fixture hygiene remains unchanged.

The execution contract is:

1. **Freeze before model dispatch.** Every executable, fixture, prompt,
   configuration, instruction, runner, guard, corpus/task/criteria, and input
   path is content-addressed in one closed manifest. Later phases accept only
   that manifest and recheck every identity; they cannot replace arguments.
2. **Separate mutation and validation roots.** Baseline, model, and candidate
   validation checkouts must be distinct pristine, detached checkouts of
   `f5f5c1e500c34fb9eeda6b8aa97ef24bd964246a` inside the declared scratch root.
   Every Git fence uses `--no-optional-locks`; branch/tag-relative replay and
   automatic optional-lock refresh are excluded. Checkouts cannot overlap each
   other or evidence, target, trusted-fixture, acquisition, executable, or
   catalogue roots. Links/reparse ancestors, wrong history, staged or
   unexpected files, and ignored build-affecting inputs such as `.cargo/config*`
   fail closed. Tests never run in the model checkout.
   Validation reconstructs only the captured allowlisted source bytes in the
   fresh candidate checkout and injects the runner-owned trusted probe.
   Cargo outputs are also isolated by source tree: baseline commands use only
   the derived baseline target, while candidate acceptance and regression use
   only the derived validation target. Manifest and receipt reconstruction
   derive these paths from the frozen target container rather than trusting a
   captured argv.
   `Cargo.lock` has a separate byte contract: the frozen commit's canonical LF
   blob remains the canonical authority, while a checkout may contain only
   those exact LF bytes or their exact deterministic LF-to-CRLF expansion.
   Freeze records each checkout role, absolute lockfile path, representation,
   byte count, and physical SHA-256; later phases and receipt verification
   compare those physical bytes exactly. No other protected file, model source,
   evidence source, probe, transform, or Git filter receives this exception.
3. **Fixed native acquisition.** The runner captures the fixed capabilities,
   status, Governor, status sequence for the exact frozen task and source
   targets. It requires supported V2 envelopes, completed `DIRECT` execution,
   `direct_none`, null LIGHT/DEEP payloads, zero Atlas counters, stable committed
   generation/workspace/configuration, and unchanged frozen source and Git
   identities before and after. Literal outputs remain caller-forced DIRECT
   baseline evidence—not automatic sufficiency, Atlas benefit, or a recomputed
   decision-hash claim.
4. **Exact negative controls.** Baseline authority is the registry's complete
   named pass/fail partition, Cargo exit 101, runtime assertion evidence,
   successful test-binary launch, successful containment, and pristine
   before/allowlisted-after Git and source fences. Compilation, launch, timeout,
   malformed output, containment failure, duplicates, ignored, filtered,
   missing, or zero tests cannot satisfy the control.
5. **Candidate acceptance.** Acceptance requires successful contained model
   execution, exact allowlisted mutation scope, retained audited source bytes,
   reconstruction into the fresh validation checkout, exact trusted acceptance
   and focused regression results, and unchanged final source/probe/protected
   fences. Model final claims and candidate-authored tests have no acceptance
   authority. Functional failures produce rejected outcomes; transport/model
   failures remain unsuccessful execution and cannot become accepted.
6. **Capture before interpretation.** Every attempt first retains fixed argv,
   resolved executable argv, cwd, exit code, bounded decoded stdout/stderr,
   hashes, duration, containment, cleanup status, and failure. Streams are UTF-8
   transport text decoded with replacement and re-encoded for retention; they
   are not original raw bytes. Arbitrarily large integer process return codes,
   including Windows values above 255, are preserved. Return-code interpretation
   happens only after capture, preserving Cargo 101 compile failures and every
   unsuccessful attempt.
7. **Finite retry.** Each Cargo target permits at most one identical retry, only
   after the exact expected named runtime result plus an owned-child containment
   error whose guard cleanup completed. Every fence is rechecked first. No
   assertion, compile, timeout, model, or changed-input retry is allowed. Every
   failed attempt remains a receipt dependency. There is no child whitelist,
   guard relaxation, global process killing, or OS workaround.
8. **Actual OMP observations.** The runner retains full JSONL/session evidence.
   It selects completed persistent `type=message` assistant rows only, excludes
   user/tool/aborted/incomplete rows, and reconciles response identity, provider,
   model, completion, usage, and tool-call identities exactly against stdout
   `message_end` rows. Stream updates are never counted as responses and model
   acceptance claims carry no authority. Provider/model labels and
   per-response usage remain raw.
   A usage field is aggregated only when known for every applicable response;
   otherwise the aggregate is null with a separately labelled observed partial
   sum. Cache counters remain separate. Tool requests are requests;
   tool-formatted output bytes are not raw source bytes or reasoning use. Exact
   source bytes, CPU/I/O/database steps, Atlas advantage, and usefulness remain
   unavailable unless actually measured.
9. **Executed receipt.** Only a successful real execution path can emit the
   closed `atlas-context-pilot-executed-receipt-v1.0.0` schema. It binds native
   acquisition, baseline, candidate, outcome, all attempts/logs/configuration,
   validator identities, exact named results, and final fences. Read-only
   `verify` reparses the closed manifest/receipt and verifies every dependency;
   it establishes artifact integrity, not newly executed validation. Importing
   an older private artifact cannot mint an executed receipt. The legacy
   `qualify-context-evidence.py` attestation contract stays closed and keeps
   `validation_execution_verified=false`.

   Focused unit tests exercise the same pure reconstruction pipeline with an
   explicitly synthetic manifest/receipt flag that is unavailable from the CLI;
   synthetic artifacts cannot mint a real executed receipt.

## Frozen corpus and locator semantics

The corpus covers all eleven roadmap classes: single-file, cross-module, API,
configuration, bug fix, refactor, rename, test-sensitive,
historical/regression, high fan-out, and ambiguous/conflicting evidence.
Every locator names a real range whose file SHA-256 is verified against the
frozen Git object.

Corpus labels are **benchmark locators**, not native Atlas canonical evidence
identities. Composite regions use the `corpus-region:<path>:<purpose>`
namespace; actual symbols and tests use their source names. A normalized
observation retains its actual evidence identity and can bind it to a corpus
locator only through an explicit, verified mapping hash. File evidence is never
treated as equivalent to every symbol in that file. The acquisition adapter
keeps native identities and locator mappings separate; translation into a
validated benchmark task execution remains an unimplemented campaign gate.

The corpus is a plan. Its task specification, validation method, and accepted
criteria are not an execution, result, receipt, or accepted outcome.

## Observation bindings

Every observation packet freezes these identities separately:

- corpus and task-specification hashes;
- validation-method and accepted-criteria hash;
- workspace, repository commit, Atlas generation, and source snapshot;
- configuration and provider-configuration fingerprints;
- planner, projection, Context IR, estimator, and metric policies;
- model, agent, and tool-configuration identities;
- route, budget, controlled dimension, sample/case set, and capture coverage.

When no model or agent ran, its fingerprint may identify a retained canonical
UTF-8 null-configuration document such as `{"model":null}` or
`{"agent":null}`. Canonical bytes include the declared terminating newline.
This records absence; it does not mean a model or agent task ran and must not be
replaced by a fictitious identity. Capture evidence must retain the exact bytes
that were hashed.

Only Context IR `1.0.0` and `2.0.0` are accepted. Before/after commit,
generation, and source bindings must match. The packet hash remains distinct
from the full observation hash. The full observation hash covers acquisition,
supplied/read/reported evidence, all four cost/result layers, and the final
binding, so changing a work counter or evidence record invalidates a receipt.
Runtime timing is retained in the full artifact but excluded from the semantic
report identity.

Acquisition status is explicit: `sufficient`, `partial`, `blocked`, or null with
unavailable coverage. A zero-context `DIRECT` acquisition can be sufficient,
but it never implies task acceptance. CLI exit success does not force a
sufficient acquisition. A blocked acquisition cannot carry a passed receipt.

## Four measurement layers

| Layer | Retained fields |
| --- | --- |
| Atlas cost | CPU time; read/write I/O; database work; visited edges; processed files; provider scopes; bounded work units; environment-qualified retrieval duration |
| Context cost | supplied records; actual supplied source bytes; selected source-span bytes separately; estimated tokens; working-set identities |
| Agent cost | model input/output tokens; tool calls; independent reads; expansion; rediscovery |
| Task result | agent-reported disposition; test/validation labels; correct target; correctness; outcome artifact; time to target and attested result |

Unavailable counters remain null. Zero means an observed zero. DEEP selected
source-span bytes are not necessarily delivered source bytes. LIGHT contract
zeros do not prove agent context was free. A read proves acquisition, not
reasoning use.

## Typed metrics

The metric policy keeps canonical kind, identifier, and range identities
distinct and accepts at most 4096 raw evidence records. Conflicting costs,
source revisions, or verified locator mappings fail closed.

- observed precision = unique `(read ∩ supplied)` / unique supplied;
- reported-use precision = unique `(reported ∩ supplied)` / unique supplied;
- required-evidence recall = verified required benchmark locators supplied at
  the required source revision / required locators, qualified as known or
  partial requirements;
- expansion = unique `(reads ∪ reported) - supplied`;
- rediscovery rate = read requests for initially supplied identities / all read
  requests, with repetitions retained;
- observed source efficiency = exact source-byte costs of supplied identities
  read / exact supplied source bytes.

Every zero or unavailable denominator produces a typed unavailable value—never
`0`, `1`, or `NaN`. Context capture gates working-set, recall, precision,
expansion, rediscovery, and source-efficiency metrics. Atlas capture gates work
and latency denominators. Task-result plus validation capture gates the reported
outcome. Missing outcome fields remain unavailable rather than becoming false.

## Receipts and usefulness limits

A validation receipt binds the full observation hash, packet, frozen validation
contract, workspace, commit, generation, source snapshot, policies, and actual
outcome artifact. Passed receipts contradicting an error, failed task claim,
wrong target, incorrect result, or blocked acquisition are rejected. Missing
receipts yield unavailable; failed receipts yield reported rejected.

The harness does not execute or independently verify validators. Receipt kinds
are operator-reported or validator-reported attestations. Reports therefore set
`validation_execution_verified=false` and
`qualified_for_optimization=false`. Actual Useful Context Throughput remains
typed unavailable with `independent_execution_unverified`. Separately named
`reported_useful_items_per_attested_*` metrics describe attested inputs only;
they are not scientific usefulness evidence.

Reports retain the bounded normalized input and frozen corpus, then rebuild the
entire report before comparison. Recomputing an outer hash cannot legitimize a
fabricated nested metric or outcome. Comparisons require matching task, corpus,
validation contract, workspace, generation, commit, source snapshot, policies,
model/agent/tool configuration, route, acquisition, capture coverage,
sample/case set, and non-null outcome artifact. Both sides must be attested
reported-accepted. Budgets may differ only in the shared declared controlled
dimension. A comparable result remains
`qualified_for_optimization=false`.

## Current evidence and next campaign gate

**Implementation status — 2026-10-01.**

Read the status in three layers: implementation is delivered only in draft,
scoped validation records what actually passed, and roadmap completion remains
blocked by the open scientific gate. Passing tests does not make Stage 2
complete.

- **Native acquisition smoke remains acquisition-only.** The retained
  `.local-atlas-runs/stage2-native-20261001` captures include no model result,
  task acceptance, reasoning-use observation, or utility result.
- **Both reusable-runner recipes executed, were runner-accepted, and verified.**
  Runner `113fc1a1…` with registry `2a406f15…` completed
  `baseline → model → validate → verify` on fresh role-isolated targets with
  the settled Rust 1.98.1 Cargo/rustc/Rustup/component-metadata profile. All
  four outer phases exited zero under containment for each recipe. `verify`
  established retained integrity only; it did not execute validation again.

  | Closed recipe | Exact baseline control | Candidate validation | Executed receipt |
  | --- | --- | --- | --- |
  | `single-file-canonical-id-whitespace` | 8 pass, 5 semantic runtime failures | 13 acceptance and 5 `context_metrics` regression tests passed; zero ignored/filtered | `7c83ea72a19cd8e33ce551c427ea38f4cc2b36d8e1a6a80d1e2a15dc397831ed` |
  | `configuration-project-marker-validation` | 3 pass, 11 semantic runtime failures | 14 acceptance and 4 `config_privacy` regression tests passed; zero ignored/filtered | `c61b350b5c66b9e4a5db49c00597f44faa4fa751411f939d5d6d22e43135f056` |

  The whitespace independent audit matched all 48 receipt dependencies and 18
  manifest dependencies, checked all 169 tracked files with only one allowlisted
  source file changed, matched all four physical lockfile bindings, and
  confirmed the retained identical retry path after owned-child cleanup
  failures. Its actual OMP trace contains four completed responses and six tool
  requests using the default `openai-codex/gpt-5.6-sol`: 24,009 input, 807
  output, 56,832 cache-read, zero cache-write, 81,648 total, and 435
  `reasoningTokens`. That provider counter is not a claim about actual
  reasoning use.

  The marker independent audit is also complete. All 48 receipt and 18 manifest
  dependencies match; the baseline has 3 passes and 11 semantic runtime
  failures; and all 14 acceptance plus 4 regression tests pass. All three
  cleanup-only first failures are retained with identical second attempts
  selected. All four physical lockfile bindings and both exclusive role targets
  match. Its actual trace has nine completed responses and 12 successful tools
  using the default `openai-codex/gpt-5.6-sol`, with no private validator reads.

  Both acquisitions were caller-forced `DIRECT`/`direct_none` with all five
  integer Atlas counters observed at zero. This does not establish automatic
  routing, sufficiency, Atlas benefit, or usefulness. The complete run retained
  144 declared files plus a retention manifest under
  `.local-atlas-runs/stage2-executed-runner-pilots-20261001`. The text evidence
  archive preserves streams, JSONL, manifests, source helpers, and original
  path/configuration/executable identity records; it does not copy every
  executable binary. Original live identities remain separately fenced and
  scoped to the recorded environment. Retention does not rebind evidence or
  mint receipts.

  The rejected private-instruction and canonical-LF/physical-CRLF preflights
  remain failed evidence and were never accepted. A separate old-compiler
  1.94.1→1.98.1 build attempt retained all streams but produced no observation
  or model result. No frozen source, Git line-ending setting, or system resource
  was changed.
- **Other private evidence remains scoped.** Seven earlier private task-class
  captures, plus the retained refactor (`30` checks) and equal-rank (`21`
  checks) exclusive rechecks, preserve their recorded outcomes but are not new
  runner recipes or optimization evidence. Source attribution for older
  shared-target Rust captures remains limited.

  The CAP and stale-source HIGH trials were both forced
  `DIRECT`/`direct_none`, protected 169 source paths, used role-isolated
  targets, and recorded a Windows Rust 1.98.1 environment. In the baseline,
  model, and valid-candidate checkouts, protected source bytes were unchanged
  except the exact append-only CAP test changes and one permitted new HIGH test
  file. Isolated negative-control checkouts contain only their frozen, declared
  production-source overlays plus the reviewed candidate tests. Those overlays
  are test inputs, not production fixes or changes left in the public diff.
  Cold transport remains unqualified. These are private test-only coverage
  observations, not public runner recipes or completed task classes.

  CAP has 11 positive baseline checks. Its repaired candidate passes 8 tests:
  5 preserved and 3 new. All seven frozen negative controls are valid: each
  reaches a reviewed semantic assertion failure in the new tests and satisfies
  the strict controller gates. Source review accepted all seven criteria, but
  mutation kills do not establish every criterion or later loop case, so this
  is not a full causal-coverage claim.
  The first source-invalid candidate, same-session
  blind repair, and cleanup attempts remain preserved. `candidate_runtime_valid`
  is true; the older candidate/result `task_acceptance` fields remain null, and
  no executed public-runner receipt was minted. Production behavior was already
  correct at baseline and remains unchanged.
  CAP is qualified only as a private test-only coverage trial.

  HIGH has 12 positive baseline checks. Its repaired candidate passes 11 tests:
  one new matrix test, one preserved telemetry test, and nine preserved compiler
  tests. The new test detects all four frozen mutations at reviewed semantic
  assertions. Three controls are strict-valid; `wrong_observed_hash` remains
  `INVALID` because its pre-model prediction expected no old failures, while the
  existing `source_drift_seals_blocked_stale_ir_without_durable_telemetry` test
  also fails with a V2 source-reference/live-proof contradiction. The
  observation was sealed before prediction comparison. The original invalid
  record remains preserved without retrocertification. The trial is therefore
  neither 4/4 valid nor fully qualified. Shared-helper assertions receive no
  consumer-specific causal credit, and no production bug or fix is claimed.
  Runtime validation is complete; HIGH qualification remains partial because
  the frozen old-test prediction failed.

  Independent review of the base archive is complete: 759 source/text files are
  declared and 761 files are present including `retention-manifest.json` and
  `root-assessment.json`, with exact byte, original, and inventory matches. The
  review covered all 107 frozen inputs for each pilot, 338 baseline physical
  source copies, 24 actual role overlays, and 6 original compiler-drift files.
  It found eight supporting original-path copy gaps.

  Root left the base archive unchanged and added a nine-file supplement (eight
  dependencies plus its coordinator), a new manifest, and a separate
  original-to-archive map under the explicitly ignored
  `.local-atlas-runs/stage2-test-only-final-supplement-20261001`. Root byte
  verification and the independent supplement inventory/hash review both
  passed: all recorded sizes, hashes, original bytes, and original-path
  resolutions match, with no missing or extra files.
  The archives exclude build targets and binaries and neither rebind original
  identity records nor overwrite prior freezes or results.
- **Clean public checks completed.** Root's clean v3 snapshot at commit
  `5b7e301a28e84668dbdf2f2b55ce073deda12dab` and tree
  `3fa66abbd2c698849aaeec82ccb11e3f8b2b07b9` matched all 186 tracked
  physical source hashes. Windows selected Cargo/rustc 1.98.1 identity and
  environment pre/post fences passed; default MSVC/shared-DLL identity remains
  excluded. Formatting passed. Six Python modules ran 102 tests: 95 passed and
  seven `scripts.test_legacy_init_fence` skips reported the unavailable
  preserved RP7 executable. Public hygiene passed for 186 tracked and 127
  packaged paths. The frozen runner suite remains 21 passing tests.

  All 12 distribution tests passed their exact complete partition. The first
  cold attempt returned zero with all 12 passing, but owned-descendant
  containment failed; the unchanged guard cleaned only that owned Job. An
  identical same-source, same-argv retry passed all 12 and containment. Both
  attempts remain retained; cold transport is not qualified.

  The checker rejected all 87 negative mutations, including CRLF encoded-probe
  bytes and altered/broadened rule mutations, without a waiver. The exact LF
  positive control passed. The repository rule pins
  `scripts/fixtures/context_execution/*.rs.b64` to LF, and source-authority
  checks require exact physical bytes.
- **Product work remains separate.** PRs
  [#6](https://github.com/Zerolitter/Workspace-Atlas/pull/6),
  [#7](https://github.com/Zerolitter/Workspace-Atlas/pull/7), and
  [#8](https://github.com/Zerolitter/Workspace-Atlas/pull/8) are draft and
  unmerged. PR #8 is the already-pushed verified production fix from its source
  owner; no shipping or merge claim is made here.
  Historical Stage 1 draft PR #6 contains the core guarantees/legal
  transitions and the provider cancellation-race fix with its regressions.
  Earlier scoped Windows qualification reported 700 unique Rust tests, 19
  Python hygiene tests, build/format/Clippy/hygiene/package checks, and three
  existing ignored helper probes; those are not a new current total or a
  shipped/merged claim, and Linux runtime qualification remains open.
- **Campaign still required.** Both runner receipts establish their two closed
  task outcomes, not the full eleven-class controlled campaign. Exact source-byte
  cost, unmeasured CPU/I/O/database work, actual reasoning use, Atlas benefit,
  Useful Context Throughput, cold transport, and Linux qualification remain
  unknown or open. No Stage 3 optimization or JEV work advances from these
  results.

Native source-content and source-tree hashes are SHA-256 under
`src/hashing.rs`; Blake3 applies only to specific opaque native identifiers.
Hash algorithms are labelled by contract rather than inferred from hex length.

**The Stage 2 scientific exit gate is OPEN.** Closing it requires the full
frozen-class campaign, independently executed validation bound to each outcome,
and controlled context/utility comparisons under recorded environment, policy,
budget, and coverage identities.
