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
