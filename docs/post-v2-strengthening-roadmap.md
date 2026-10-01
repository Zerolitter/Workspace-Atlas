# Workspace Atlas post-v2 strengthening roadmap

## Strategic direction

Workspace Atlas should continue toward one central thesis:

> **Compile a changing repository into the minimum sufficient, current,
> evidence-backed working view required by a model—without forcing the model to
> rediscover the project.**

Atlas is not trying to become a general memory system, vector database,
autonomous reasoning model, or replacement for source truth. Its long-term
value is the project-context compiler between repositories and models.

The six forms of project truth remain the foundation: current, structural,
temporal, operational, epistemic, and task truth. The next development cycle
strengthens these capabilities before adding another intelligence layer.

## Architecture rule

```text
WORKSPACE
    |
    v
TRUTH PLANE — authoritative, conservative
    |
    v
immutable generation
    |
    v
SERVING PLANE — derived, rebuildable, optimized
    |
    v
CONTEXT PLANE <--- TASK
    |
DIRECT / ATLAS_LIGHT / ATLAS_DEEP
    |
    v
minimum sufficient Context IR
    |
    v
MODEL
```

> **Compile project truth eagerly; compile task context lazily. Cache everything
> whose validity can be tied to a generation.**

Ordinary retrieval remains proportional to the bounded working-set frontier,
not total repository size.

## Stage 1 — Core contract hardening

Make the trusted core difficult to misinterpret or accidentally weaken.
Lifecycle, generation, provider, Serving, Governor and Context IR assumptions
become explicit guarantees in the [core operation contracts](core-contracts.md).

Define one-sentence guarantees for `reconcile_workspace()`,
`activate_generation()`, `accept_provider_result()`,
`build_serving_projection()`, `resolve_relationship()`,
`compile_context_ir()`, `governor_run()`, `verify_exact_source()`,
`complete_task()`, and `abandon_task()`.

For example, a candidate may become active only after its activation
requirements succeed; failure preserves the preceding active generation.

The conceptual sequence is candidate, validated prerequisites, committed, then
active authority. In the existing schema, validated is a prerequisite and
active is a workspace pointer, not additional stored generation states.
Failed and abandoned generations must never become authoritative.

Property/state-machine qualification must cover:

- failure during reconciliation;
- a provider finishing after cancellation;
- a generation changing during a request;
- a Serving projection becoming stale;
- source changing after Context IR compilation;
- an interrupted task lifecycle;
- concurrent readers and writers; and
- unsupported contract versions.

**Exit gate:** The important state transitions are explainable without
referring to implementation behavior. The specification defines what is legal.

## Stage 2 — Evidence and context qualification harness

Turn Atlas into its own scientific measurement platform. Measure what was
supplied, what was useful, and whether the task succeeded before optimizing
the working set.

Create frozen repository tasks for single-file changes, cross-module behavior,
API changes, configuration changes, bug fixes, refactors, renames,
test-sensitive changes, historical/regression questions, high fan-out changes,
and ambiguous/conflicting evidence. Each task records repository commit,
Atlas generation, task specification, required evidence where knowable,
validation method, and accepted outcome.

Measure four layers separately:

| Layer | Measurements |
| --- | --- |
| Atlas cost | CPU, I/O, database work, graph edges visited, files/provider scopes processed |
| Context cost | Records supplied, source bytes, estimated tokens, working-set size |
| Model/agent cost | Model tokens, tool operations, independent file reads, rediscovery, context expansion |
| Task result | Accepted/rejected, tests, correct target, correctness, time to accepted result |

Wall-clock latency remains environment-qualified, separate from algorithmic
work. Retain context precision, recall, expansion, rediscovery rate, source
efficiency, working-set size, accepted-change rate, wrong-target rate, and time
to correct target.

```text
Useful Context Throughput
    = evidence actually used in accepted tasks
      / Atlas retrieval work or environment-qualified latency
```

**Exit gate:** Reproducible benchmark evidence supports optimization decisions.

### Implementation status — 2026-10-01

PRs [#6](https://github.com/Zerolitter/Workspace-Atlas/pull/6),
[#7](https://github.com/Zerolitter/Workspace-Atlas/pull/7), and
[#8](https://github.com/Zerolitter/Workspace-Atlas/pull/8) are draft and
unmerged. PR #8 is the already-pushed verified production fix from its source
owner; it is not shipped or merged.

Historical Stage 1 draft PR #6 contains the core guarantees/legal transitions
and the provider cancellation-race fix with its regressions. Earlier scoped
Windows qualification reported 700 unique Rust tests, 19 Python hygiene tests,
build/format/Clippy/hygiene/package checks, and three existing ignored helper
probes; those are not a new current total or a shipped/merged claim, and Linux
runtime qualification remains open.

The reporting/normalization harness remains offline. The separate source-only
runner executes exactly two closed recipes; only its `verify` phase is offline
integrity reconstruction.

Runner `113fc1a1…` with registry `2a406f15…` completed both recipes through
`baseline → model → validate → verify` on exclusive source-role targets and a
settled Rust 1.98.1 Cargo/rustc/Rustup/component-metadata profile. All four outer
phases exited zero under containment for each recipe.

| Closed recipe | Baseline control | Candidate validation | Executed receipt |
| --- | --- | --- | --- |
| `single-file-canonical-id-whitespace` | 8 pass, 5 semantic runtime failures | 13 acceptance and 5 focused regressions passed; zero ignored/filtered | `7c83ea72a19cd8e33ce551c427ea38f4cc2b36d8e1a6a80d1e2a15dc397831ed` |
| `configuration-project-marker-validation` | 3 pass, 11 semantic runtime failures | 14 acceptance and 4 `config_privacy` regressions passed; zero ignored/filtered | `c61b350b5c66b9e4a5db49c00597f44faa4fa751411f939d5d6d22e43135f056` |

The whitespace independent audit matched all 48 receipt and 18 manifest
dependencies, checked all 169 tracked files with only one allowlisted source
file changed, matched all four physical lockfile bindings, and confirmed its
retained identical retry path. Its trace has four completed responses and six
tool requests.

The marker independent audit is also complete: all 48 receipt and 18 manifest
dependencies match; baseline is 3 pass/11 semantic runtime failures; all 14
acceptance and 4 regression tests pass; all three cleanup-only first failures
are retained with identical second attempts selected; and all four lockfile
bindings plus both exclusive role targets match. Its trace has nine completed
responses and 12 successful tools using the default
`openai-codex/gpt-5.6-sol`, with no private validator reads. Offline verification
checked retained integrity and did not execute validation again.

Both acquisitions were forced `DIRECT`/`direct_none` with all five integer
Atlas counters observed at zero. They make no automatic-routing, sufficiency,
Atlas-benefit, or usefulness claim. The complete evidence set retains 144
declared files plus a retention manifest under
`.local-atlas-runs/stage2-executed-runner-pilots-20261001`. The text archive
preserves streams, JSONL, manifests, source helpers, and original
path/configuration/executable identity records, not every executable binary.
Original live identities remain separately fenced and scoped to the recorded
environment; retention neither rebinds evidence nor mints receipts.

Rejected private-instruction and canonical-LF/physical-CRLF preflights remain
failed evidence. The separate old-compiler 1.94.1→1.98.1 attempt retained all
streams but produced no observation or model result. No frozen source, Git
line-ending setting, or system resource was changed.

Earlier private task-class captures, including the retained refactor 30-check
and equal-rank 21-check exclusive rechecks, preserve scoped outcomes but are not
new runner recipes or optimization evidence. Source attribution for older
shared-target Rust captures remains limited.

Private capacity followups separately have 11 positive CAP and 12 positive HIGH
baseline checks. Their first test-only candidates were rejected before runtime
for missing explicit public success assertions before `unwrap`; same-session
blind repairs are retained. Both repairs now pass their semantic source gates:
all 7 CAP criteria and all 10 HIGH criteria. Across all 169 protected files,
bytes are unchanged except the exact append-only CAP tests; HIGH adds only its
permitted new file. Candidate Cargo and mutation acceptance remain pending.
This work is not a production bug fix or completed task class.

Root's clean v3 snapshot at commit
`5b7e301a28e84668dbdf2f2b55ce073deda12dab` and tree
`3fa66abbd2c698849aaeec82ccb11e3f8b2b07b9` matched all 186 tracked
physical source hashes. Windows selected Cargo/rustc 1.98.1 identity and
environment pre/post fences passed; default MSVC/shared-DLL identity remains
excluded. Formatting passed. Six Python modules ran 102 tests: 95 passed and
seven legacy-init-fence skips reported the unavailable preserved RP7
executable. Hygiene passed 186 tracked and 127 packaged paths; the frozen
runner suite remains 21 passing tests.

All 12 distribution tests passed their exact complete partition. The first cold
attempt returned zero with all 12 passing but failed owned-descendant
containment; the unchanged guard cleaned that owned Job. An identical
same-source, same-argv retry passed all 12 and containment. Both attempts remain
retained, and cold transport is not qualified.

The checker rejected all 87 negative mutations, including CRLF encoded-probe
bytes and altered/broadened rule mutations, without a waiver. The exact LF
positive control passed. The narrow
`scripts/fixtures/context_execution/*.rs.b64` rule and source-authority checks
require exact LF physical bytes.

Both runner receipts establish their two task outcomes, not the full
eleven-class controlled campaign. Exact source-byte cost, unmeasured
CPU/I/O/database work, actual reasoning use, Atlas benefit, Useful Context
Throughput, cold transport, and Linux qualification remain unknown or open.
Legacy attested reports retain `validation_execution_verified=false`. No Stage
3 optimization or JEV work advances. **The Stage 2 scientific exit gate is
OPEN.**

## Stage 3 — Serving Plane performance

Make context cheap enough for the agent loop while preserving correctness.
Push reusable computation out of request time.

| Timing | Work |
| --- | --- |
| Eager/incremental | Content hashes, file revisions, canonical symbol identities, provider evidence, relationship resolution, preferred evidence, conflict state, coverage summaries, inbound/outbound adjacency |
| Generation-cached | SymbolCards, direct neighborhoods, test/configuration relationships, effect summaries, token estimates, compact Context IR fragments |
| Request-time | Task classification, seeds, bounded graph expansion, task ranking, token/latency packing, exact-source verification, justified historical lookup |

Every derived object declares invalidation inputs. Resolved relationships are
invalidated by relationship facts, candidate targets, target universe, or
resolution-policy changes. Prefer cache addresses containing generation,
entity, projection-policy version, and Context IR version.

> **Serving is disposable. Truth is not.**

The hot path is seed lookup plus bounded local expansion plus packing. No
normal retrieval performs repository-scale scanning or reconstruction.

**Exit gate:** Warm retrieval is dominated by bounded selection and source
verification rather than reconstruction of project knowledge.

## Stage 4 — Context Governor qualification

Demonstrate when DIRECT, LIGHT and DEEP are appropriate. Atlas supplies the
minimum assistance required by the current task.

Keep routing deterministic, with visible reason codes for explicit targets,
known source ranges, relationship degree, package span, test relationships,
configuration dependencies, public API exposure, ambiguity, conflict, history,
and expected raw-context cost. Introduce neither JEV nor learned routing here.

Preserve progressive escalation from DIRECT to LIGHT to DEEP only when needed.
Initial inspection can reveal difficulty the first decision could not know.

Executable routes are not proof of sufficiency. Benchmark tasks define their
required evidence. For a behavior change, this can include primary
implementation, dependents, configuration, effects, tests and exact source;
an unresolved dynamic implementation leaves the working set partial.

**Exit gate:** Atlas demonstrates route sufficiency and necessary escalation.

## Stage 5 — Context Compiler precision

Move from relevant retrieval toward evidence requirement planning: what
evidence must be present for a task class to be performed safely?

Use stable semantic roles:

```text
PRIMARY_IMPLEMENTATION
DIRECT_DEPENDENT
IMPLEMENTATION
CONFIGURATION_INPUT
OBSERVABLE_EFFECT
TEST_CONTRACT
HISTORICAL_CONSTRAINT
UNCERTAINTY
VALIDATION_TARGET
EXACT_SOURCE
```

Every selected item retains selection reason, origin, graph distance, evidence
state, generation, source cost and token cost. Determine whether the working
set is complete enough, not merely whether it fills a token budget.

Run identical tasks at controlled 2K, 4K, 8K, 16K and 32K budgets. Measure where
additional context stops materially improving accepted-task correctness.
The target is the minimum sufficient packet, not the smallest possible packet.

**Exit gate:** Bounded Context IR empirically preserves correctness while
reducing irrelevant context.

## Stage 6 — Temporal and epistemic intelligence

Represent what changed, what remains valid, what became stale, and what Atlas
cannot establish. Expand Generation Delta into a Change Graph between
generations: symbols and relationships added/removed/modified, changed effects,
affected tests, changed coverage, and conflicts introduced/resolved.

Bind evidence validity to evidence identity, generation, source-revision hash,
provider fingerprint and projection-policy version. Evidence remains reusable
while determining inputs remain compatible.

Distinguish not found from not found within verified coverage. For example,
complete TypeScript/Rust coverage cannot exclude unsupported Python or
unresolved dynamic registration. Supply the boundaries of each answer.

**Exit gate:** Atlas explains change, current proof, staleness, uncertainty and
what cannot be excluded without repository rediscovery.

## Stage 7 — Demand-paged project memory experiment

After the harness and Serving instrumentation are stable, compare:

- EAGER: all Serving projections warm;
- LAZY: materialize evidence on access; and
- HYBRID: critical metadata and direct adjacency warm, deeper evidence loaded
  on demand.

Measure resident memory, disk reads, pages touched, cache hit rate, p50/p95
latency, working-set build time, Context Yield and accepted-task correctness.
An approach feeling faster does not justify production changes.

**Exit gate:** Complete project truth stays logically available without keeping
the entire Serving working set physically resident unnecessarily.

## Stage 8 — Portable Context ABI

Make canonical Context IR consumer-independent. Coding agents, reviewers,
IDEs, automation and future skills may present the same evidence differently
without changing Atlas truth. Canonical evidence contains no vendor-specific
instructions, consumer-specific reasoning or agent-specific project truth.

Strengthen schema versioning, capability discovery, closed vocabularies,
forward-incompatibility behavior, deterministic serialization, stable
identities, bounded omissions, provenance and consumer-independent roles.

**Exit gate:** Different consumers use one project representation without
changing the truth model.

## Stage 9 — JEV evaluation gate

JEV stays isolated on its research branch through Stages 1–8. It is never a
mainline prerequisite. Atlas works correctly without JEV; optional JEV
observations can at most inform deterministic Context Plane policy.

Before an integration ADR, JEV must demonstrate value against upgraded Atlas:

- beat its deterministic utility baseline;
- preserve accepted-task correctness, required-evidence recall, conflicts and
  uncertainty;
- reduce downstream rediscovery;
- provide value greater than call latency/cost;
- work across repositories and task classes;
- handle model/version drift and provide deterministic fallback;
- have privacy approval;
- bind observations to generation and candidate/evidence identities; and
- fail closed when Atlas state changes.

Compare upgraded deterministic Atlas with upgraded Atlas plus JEV. Only
consistent advantage can justify an optional integration ADR. JEV may supply
a Context Plane optimization signal; it never supplies Truth Plane authority.

## Separate experiments

Keep JEV research, local-model performance, model-vs-model qualification,
learned/adaptive ranking, embeddings/vector retrieval and automatic persistent
agent memory outside this roadmap until independently evaluated. These may
consume Atlas and produce research evidence; Atlas does not depend on them.

## Overall sequencing and priorities

```text
public Atlas v2.0
  -> 1. core contracts
  -> 2. evidence/context qualification
  -> 3. Serving performance
  -> 4. Governor qualification
  -> 5. compiler precision
  -> 6. temporal/epistemic knowledge
  -> 7. residency experiment
  -> 8. portable ABI
  -> major mainline qualification boundary
  -> 9. optional JEV evaluation
       -> no advantage: remains research
       -> demonstrated advantage: optional integration ADR
```

Prioritize guarantees and qualification first, Serving performance second,
deterministic Governor/compiler precision third, temporal/epistemic knowledge
fourth, residency fifth, portable contracts sixth, and optional external
intelligence last. Do not optimize a weak or poorly measured baseline.

## Product laws

```text
Source remains authoritative.
Prediction is not evidence.
Ranking is not truth.
History is not current state.
Failed construction preserves valid state.
Derived Serving state is disposable.
Uncertainty is never silently discarded.
Exact source is verified before change use.
No ordinary retrieval performs repository-scale work.
Zero Atlas context is a valid successful decision.
A smaller packet that reduces correctness is a failure.
Atlas remains fully functional without optional intelligence layers.
```

## End state

Repository changes incrementally establish project truth, an immutable verified
generation supplies fast Serving projections, and the Governor and Task
Compiler select required evidence into a minimum sufficient Context IR.
Consumers work from that small trusted set. The next reconciliation explains
semantic change so the next task inherits verified project understanding.

The strongest claim is:

> **Workspace Atlas converts repository understanding from repeated model work
> into reusable, measurable, verified project infrastructure.**
