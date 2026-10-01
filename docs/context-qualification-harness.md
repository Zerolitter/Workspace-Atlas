# Context qualification harness foundation

The Stage 2 qualification harness is an offline, dependency-free measurement
foundation. It validates normalized observations, derives typed metrics, emits
content-addressed reports, and rejects incomparable report pairs. It does not
run agents, Atlas routes, validators, tests, or shell commands. The packaged
JSON files are synthetic contract fixtures, not measured Atlas or model-task
qualification.

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

The scripts are repository-source tools and are intentionally outside the Cargo
package. This guide and the JSON fixtures are packaged.

## Commands

Run the focused suite from a repository checkout:

```powershell
python -B -m unittest scripts.test_qualify_context_evidence
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

Current evidence has three separate scopes:

- **Native acquisition smoke only.** Retained captures at the portable, ignored
  path `.local-atlas-runs/stage2-native-20261001` bind the committed Atlas
  generation `gen_9e8ee67bea359639f2561523ba0d5168`. The Rust
  `provider_runtime` path produced text fallback, partial coverage, zero
  semantic symbols, blocked Context IR, an empty packet, and CLI exit zero. The
  configuration path produced partial coverage and a bounded 40-record packet.
  No model ran in this smoke capture; no acceptance, reasoning use, or utility
  was observed. CPU, disk, database-step, and model-token counters were not
  measured.
- **Two independently validated DIRECT pilots.** Both real OMP model runs used
  caller-forced native Governor `DIRECT` with a zero Atlas packet. Candidate
  patches are benchmark artifacts; neither Rust change is shipped by this
  harness work.

  | Frozen corpus task | Candidate patch scope | Frozen pre-dispatch probe | Independently reconstructed candidate validation | Retained ledger |
  | --- | --- | --- | --- | --- |
  | `single-file-canonical-id-whitespace` | only `src/context_metrics.rs` | 8 passing tests, 5 runtime-failing tests | 13 acceptance tests and 5 focused `context_metrics` regression tests passed | `.local-atlas-runs/stage2-whitespace-pilot-20261001/independent-model-pilot-ledger.json` |
  | `configuration-project-marker-validation` | only `src/config.rs` | 3 positive tests passed, 11 semantic tests failed at runtime | all 14 acceptance tests and 4 `config_privacy` regression tests passed; exact names, zero ignored or filtered | `.local-atlas-runs/stage2-markers-pilot-20261001/independent-model-pilot-ledger.json` |

  Trusted probe names and contracts were frozen before dispatch. Root audited
  each candidate patch, reconstructed allowlisted source into a frozen
  validation checkout, and verified containment under an unchanged process
  guard. Successful warm retries passed that guard. Failed cold attempts remain
  retained; owned Visual Studio `vctip.exe` descendants outlived those attempts,
  and the guard cleaned only its own job. Cold-build transport remains
  unqualified; no guard waiver or OS, hardware, or performance setting changed.

  Actual OMP model traces and token observations are retained. Exact source-byte
  cost, actual reasoning use, Atlas benefit, and Useful Context Throughput
  remain null or unknown. These focused acceptances do not alter legacy
  attested reports, whose `validation_execution_verified=false` semantics
  remain unchanged.
- **Campaign still required.** Two of eleven frozen corpus tasks are accepted
  for focused scope; nine have not executed. Controlled context comparisons and
  actual Useful Context Throughput evidence have not run. Two zero-context
  DIRECT trials cannot qualify all task classes or establish an Atlas advantage.

Native source-content and source-tree hashes are SHA-256 under
`src/hashing.rs`; Blake3 applies only to specific opaque native identifiers.
Hash algorithms are labelled by contract rather than inferred from hex length.

**The Stage 2 scientific exit gate is OPEN.** Closing it requires the full
frozen-class campaign, independently executed validation bound to each outcome,
and controlled context/utility comparisons under recorded environment, policy,
budget, and coverage identities.
