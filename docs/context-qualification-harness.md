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
treated as equivalent to every symbol in that file. A production native
normalization adapter remains unimplemented and is a campaign gate.

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

Retained native smoke captures at the portable, ignored checkout-relative path
`.local-atlas-runs/stage2-native-20261001` bind the frozen commit and committed
Atlas generation `gen_9e8ee67bea359639f2561523ba0d5168`. The Rust
`provider_runtime` path produced text fallback with partial coverage and zero
semantic symbols, then blocked Context IR, an empty packet, and CLI exit zero.
The configuration TOML path produced partial coverage and a bounded 40-record
packet with CLI exit zero. No model task ran; no receipt, acceptance, reasoning
use, or usefulness was observed. CPU, disk, database-step, and model-token
counters were not measured. Native Blake3 source hashes remain Blake3; this
foundation does not relabel them as SHA-256.

The next real campaign gate is a production normalization adapter plus an
actually executed independent validator bound to its outcome artifact. Only
then can frozen eleven-class model-task runs establish accepted outcomes and
actual Useful Context Throughput under recorded environment, policy, budget,
and coverage identities. Until that campaign succeeds, Stage 2 infrastructure
is tested but task qualification and the Stage 2 scientific exit gate remain
unqualified.
