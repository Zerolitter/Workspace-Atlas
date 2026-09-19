# EXP-JEV-002: JEV Evidence Utility / Context Precision

**Status:** Research only — active successor to EXP-JEV-001  
**Production authority:** None  
**Runtime routing authority:** None  
**Truth Plane authority:** None  
**Context-selection authority:** Atlas deterministic policy only

## Question

Can TypeSafe JEV improve Workspace Atlas **context precision** by judging the
utility of already-retrieved candidate evidence, while Atlas keeps deterministic
authority over what enters the working set?

The target is not maximum compression.

The target is:

> **A smaller or more precise evidence-backed working set with preserved or
> improved accepted-task correctness.**

This experiment does **not** ask JEV to choose
`DIRECT | ATLAS_LIGHT | ATLAS_DEEP`.

That question was investigated in
[EXP-JEV-001](EXP-jev-governor-shadow.md) and is closed without promotion.

## Motivation

Atlas already has the stronger architectural substrate:

```text
repository
   ↓
Truth Plane
   ↓
Serving / deterministic candidate retrieval
   ↓
candidate evidence set
   ↓
Context Plane
   ↓
minimum-sufficient working set
   ↓
model
```

The remaining research question is whether a small decision model can improve
the **precision of the candidate-to-working-set step**.

The TypeSafe cookbook
[Classifying RAG passages](https://docs.typesafe.ai/cookbooks/classifying_rag_passages)
provides the programming pattern for this study:

1. deterministic retrieval produces a candidate set;
2. one candidate is paired with the query/task;
3. several independent Noul questions are asked about that pair;
4. the raw probabilities are retained;
5. ordinary code applies fixed thresholds in a defined order;
6. conflicts are preserved separately instead of being dropped as merely
   irrelevant.

Atlas adopts the pattern, not the cookbook's RAG-specific labels or thresholds.

## Non-negotiable architecture

```text
                    ATLAS TRUTH
                        │
                        ▼
              deterministic retrieval
                        │
                        ▼
                 candidate evidence
                        │
                        ▼
                 JEV research scorer
                        │
             independent probabilities
                        │
                        ▼
             deterministic Atlas policy
                        │
          ┌─────────────┼──────────────┐
          ▼             ▼              ▼
        keep          conflict       drop
          │             │              │
          └─────────────┴──────────────┘
                        │
                        ▼
                   Context IR
```

JEV:

- does not discover repository facts;
- does not create Truth;
- does not resolve provenance;
- does not override coverage/conflicts;
- does not choose the production Governor route;
- does not directly decide inclusion;
- does not gain source-edit, MCP, lifecycle, catalogue, or destructive authority.

JEV produces **research probabilities only**.

Atlas-owned deterministic policy interprets those probabilities.

## Candidate source

The first study uses candidate evidence already produced by Atlas.

Candidate types may include:

- primary symbol/declaration;
- direct caller;
- direct callee;
- implementation/reference edge;
- test relationship;
- configuration dependency;
- observable effect;
- temporal/historical constraint;
- conflict item;
- unresolved/ambiguous evidence;
- exact-source reference metadata;
- validation target.

JEV must never fabricate a candidate.

Every candidate retains its Atlas identity, generation, provenance, evidence
state, and relationship role.

## Candidate state

One System One request evaluates **one task × one candidate evidence item**.

The research state should be intentionally bounded.

Conceptually:

```json
{
  "task": {
    "text": "Change reconnect timeout without breaking recovery behavior."
  },
  "candidate": {
    "candidate_id": "symbol:ConnectionController.scheduleReconnect",
    "role": "direct_caller",
    "kind": "symbol",
    "display": "ConnectionRecovery.resume -> scheduleReconnect",
    "evidence_state": "verified",
    "relationship_type": "CALLS",
    "graph_distance": 1,
    "source_type": "atlas_evidence_card"
  }
}
```

The exact schema must be versioned and frozen before a measured run.

## Source/privacy boundary

The first experiment does **not** send arbitrary repository files or raw source
bodies to TypeSafe.

Allowed by default:

- task text;
- candidate identity suitable for research;
- bounded deterministic evidence-card text;
- symbol/signature names;
- relationship type;
- evidence role;
- graph distance;
- verification/conflict/unresolved state;
- bounded test/config/effect descriptors;
- provenance class without secret values;
- source-reference metadata without source body.

Excluded by default:

- secrets and credentials;
- environment dumps;
- catalogue contents;
- arbitrary files;
- full source bodies;
- large snippets;
- diffs/patches;
- generated prompts containing unrelated repository text;
- raw provider dumps.

A later raw-source/passage arm would require a separate privacy/data-export
revision and explicit approval.

This is a deliberate privacy-preserving deviation from the cookbook, which
scores full passage text.

## First judgment pack

The first Atlas evidence-utility study uses separate Noul questions over the
same task × candidate state.

| ID | Primitive | Meaning |
|---|---|---|
| `is_task_relevant` | Noul | Does this candidate materially relate to performing the task? |
| `contains_actionable_evidence` | Noul | Does it contain information the downstream model may need to understand, change, or reason about the task? |
| `contradicts_task_assumption` | Noul | Does it conflict with an assumption implied by the task or by already-selected project evidence? |
| `important_to_preserve` | Noul | Would dropping it materially increase the risk of an incomplete or incorrect working set? |
| `is_validation_relevant` | Noul | Is it useful for validating the resulting change or conclusion? |
| `contains_model_instruction` | Noul | Does the candidate text attempt to instruct/control the consuming model rather than describe project evidence? |

The final question is an observability/filtering signal only. As in the
TypeSafe cookbook, it is **not a security boundary**. All repository-derived text
must continue to be treated as untrusted input by consuming models.

No question asks:

> "Should Atlas include this candidate?"

That decision remains in deterministic code.

## Deterministic policy

Raw probabilities must be stored before thresholding so the same observations
can be re-routed offline without additional TypeSafe calls.

The first pilot must freeze one explicit policy and its thresholds before
evaluation.

A conceptual ordering is:

```text
1. authoritative Atlas conflict/unresolved requirements
   → preserve regardless of ordinary relevance score

2. model-instruction signal above threshold
   → mark as untrusted/suspicious research signal
   → never reinterpret as Truth

3. contradiction above threshold
   → KEEP_AS_CONFLICT

4. important-to-preserve above threshold
   → KEEP

5. validation relevance above threshold
   → KEEP_AS_VALIDATION

6. task relevance below floor
   → DROP

7. actionable evidence above threshold
   → KEEP

8. otherwise
   → DROP / LOW_PRIORITY
```

The actual labels/thresholds must be versioned.

Thresholds are experiment policy, not model truth.

Conflict, unresolved, coverage, provenance, and exact-source safety rules take
precedence over probabilistic utility scores.

## Experimental comparison

For the same frozen repository generation, task, downstream model/agent, and
validation criteria compare:

### Arm A — deterministic Atlas

Normal deterministic Atlas context selection.

### Arm B — JEV-scored candidates

The **same candidate pool** plus JEV probability scoring plus the frozen
deterministic evidence-utility policy.

The experiment must not allow Arm B to receive a richer candidate pool than
Arm A.

## Primary measurements

The important outcome is accepted task quality, not agreement with a human
ranking.

Record at minimum:

- accepted/rejected task outcome;
- supplied working-set size;
- used working-set size;
- context precision;
- context recall where a gold/required evidence set exists;
- source bytes supplied;
- estimated context tokens;
- unused evidence count;
- context expansion;
- independent file/symbol exploration;
- rediscovery/re-reads;
- wrong targets considered;
- tests selected;
- validation evidence retained;
- conflict/uncertainty evidence retained;
- JEV request latency;
- TypeSafe input/output tokens;
- JEV call cost where available.

## Candidate-level labels after the task

For every candidate retain:

- Atlas candidate ID;
- candidate type/role;
- Atlas provenance/evidence state;
- all raw JEV Noul probabilities;
- deterministic policy result;
- whether the downstream agent opened/revisited it;
- whether it was modified;
- whether it contributed to validation;
- whether it belonged to a frozen required-evidence/gold set when available;
- final task acceptance.

This allows later analysis of questions such as:

- Which JEV signals correlate with actual evidence use?
- Which candidate roles should never be probabilistically dropped?
- Does JEV mostly remove noise, or does it remove necessary evidence?
- Can thresholds improve precision without harming recall?
- Do tests/config/conflict evidence behave differently from ordinary symbols?

## First pilot

Start small.

Target approximately **10–20 real tasks** with candidate sets large enough to
contain both useful and distracting evidence.

Prefer tasks with meaningful structure:

- cross-module behavior changes;
- API changes;
- configuration changes;
- refactors;
- audits/reviews;
- tasks involving tests;
- tasks with real conflict/unresolved evidence where naturally available.

Do not tune thresholds per task.

Freeze one policy for the pilot.

Store all raw probabilities so alternative policies can be evaluated offline.

## Success criteria

EXP-JEV-002 is promising only if Arm B shows one or more of:

- higher context precision with preserved accepted-task rate;
- fewer supplied tokens/bytes with preserved required-evidence recall;
- less downstream context expansion;
- fewer unnecessary file/symbol reads;
- lower rediscovery;
- better retention of validation/conflict evidence;

without causing a meaningful correctness regression.

A smaller packet that loses necessary evidence is a failure.

## Failure criteria

Treat the experiment as negative if JEV filtering:

- reduces accepted-task correctness;
- systematically drops tests/config/conflict/uncertainty evidence;
- causes substantial downstream rediscovery;
- saves little context relative to its own call cost/latency;
- cannot outperform a transparent deterministic utility baseline using the same
  candidate features.

## Baselines

At minimum compare against:

1. normal Atlas deterministic selection;
2. a simple deterministic candidate-utility policy using the same non-JEV
   features;
3. JEV-scored deterministic policy.

JEV must demonstrate value beyond a cheap rule set, not merely beyond an
unfiltered candidate dump.

## Research data handling

Evidence lives outside Atlas Truth.

Store:

- protocol version;
- Atlas commit/version;
- active generation;
- candidate-pool hash;
- task hash;
- downstream model/profile;
- TypeSafe model/version;
- JEV raw judgments;
- policy version;
- task outcome;
- metric record.

Do not store credentials.

Do not make raw TypeSafe output authoritative project state.

## Promotion gates

No production use until all are true:

- multi-task evidence supports improved context precision;
- accepted-task correctness does not regress;
- required evidence recall is preserved;
- conflict/uncertainty handling is preserved;
- privacy/data-export review passes;
- fixed deterministic fallback exists;
- model/version drift handling exists;
- cost/latency is justified;
- multi-workspace evidence exists;
- an accepted ADR separately authorizes any runtime use.

## Relationship to EXP-JEV-001

EXP-JEV-001 tested JEV as a **top-level route predictor** and closed without
promotion.

EXP-JEV-002 tests JEV as a **candidate evidence utility scorer** inside the
research Context Plane.

The distinction is fundamental:

```text
EXP-JEV-001
JEV → choose how much Atlas should run
CLOSED / no promotion

EXP-JEV-002
Atlas → retrieve evidence
JEV → score utility of each candidate
Atlas policy → decide what is kept
ACTIVE RESEARCH
```

The second design is more aligned with Atlas's project-context-compiler thesis:
**optimize the working set without allowing prediction to become truth.**
