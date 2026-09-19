# EXP-JEV-001: JEV Governor Shadow

**Status:** Research only  
**Branch:** `research/jev-governor-shadow`  
**Production authority:** None  
**Runtime routing authority:** None  
**Truth Plane authority:** None

## Question

Can TypeSafe JEV predict the minimum useful Workspace Atlas
context-acquisition depth and selected policy flags better than the current
deterministic policy, without weakening Atlas truth, privacy, reproducibility,
or source verification?

The first target is route prediction:

```text
DIRECT | ATLAS_LIGHT | ATLAS_DEEP
```

Secondary shadow judgments cover task kind, whether the current route is likely
to require escalation, whether history is necessary, whether a nontrivial
validation plan is needed, and graded project-context risk.

## Official TypeSafe programming model

This experiment follows the official `typesafe-ai` agent skill and current
TypeSafe Python SDK contract.

JEV is treated as a System One decision model, not a chat generator:

- **Choice** selects one value from a defined set and returns a probability
  distribution plus Choice confidence.
- **Noul** returns the probability of a yes/true statement. It has no separate
  confidence field; values near 0.5 are uncertain.
- **Score** evaluates an ordered rubric and returns a probability-weighted score,
  its distribution, and Score confidence.
- Independent questions over the same state are sent together in one
  `system_one` evaluation.
- Typed output guarantees the interface, not truth. Thresholds and policy remain
  Atlas/application decisions and must be evaluated on Atlas task data.

Launch implementation reference: official Python SDK `typesafe-sdk 0.7.0`.
The default model alias for this experiment is `jev-latest`.

## Non-negotiable architecture

```text
                ATLAS TRUTH / SERVING
                         │
                         ▼
                 deterministic Governor
                         │
                         ├───────────────► real route/result
                         │
                         ▼
              evaluation observation
                         │
                         ▼
                JEV SHADOW ADAPTER
                         │
                         ▼
                typed judgments only
                         │
                         ▼
                 comparison / metrics
```

JEV never sits on the authoritative execution path in this experiment.

The authoritative Atlas run happens exactly as it would without JEV. The shadow
adapter consumes a bounded observation of the task and already-derived Atlas
metadata and produces typed judgments for later comparison.

## First judgment pack

One shared state is evaluated with these independent questions:

| ID | Primitive | Meaning |
|---|---|---|
| `route` | Choice | Minimum useful `DIRECT | ATLAS_LIGHT | ATLAS_DEEP` route |
| `task_kind` | Choice | Atlas task-kind classification |
| `needs_escalation` | Noul | Probability that a deeper route is needed than the observed/current route |
| `requires_history` | Noul | Probability that temporal evidence is necessary |
| `requires_validation_plan` | Noul | Probability that a nontrivial validation plan is needed |
| `risk` | Score | Ordered project-context risk from minimal → bounded → broad → uncertain |

No free-form rationale or chain-of-thought is requested. The experiment retains
raw typed judgments and their probability distributions so later policy
thresholds can be tested without rerunning inference.

## Data boundary

Allowed by default:

- task text;
- deterministic task-kind result, including `unknown`;
- current/observed Atlas route;
- explicit-target flag;
- bounded counts such as direct callers/callees/tests;
- package/module span;
- conflict count;
- unresolved-edge count;
- history availability;
- public-API indicator;
- estimated source/context cost;
- generation identifier only if useful for experiment provenance.

Not allowed by default:

- raw file contents;
- exact source bodies;
- secrets or credentials;
- environment dumps;
- catalogue contents;
- arbitrary repository excerpts;
- diffs/patches;
- provider outputs containing source text.

The harness rejects obvious raw-source fields unless a future research revision
explicitly changes this contract.

## Provider boundary

The live experiment uses the official TypeSafe Python SDK and therefore remains
explicitly opt-in.

Requirements:

1. Network execution requires the explicit `call` action.
2. `dry-run` works without the SDK or credentials and prints the exact state,
   model alias, and typed questions that would be submitted.
3. Credential resolution/network transport belongs to the official SDK, not
   Atlas or the shadow adapter.
4. Persisted output hashes task/input identity by default rather than copying
   raw task text into the result.
5. Atlas remains fully usable with no TypeSafe/JEV configuration.

## Input contract

Minimal JSON:

```json
{
  "task": "Fix reconnect timeout without changing login semantics.",
  "atlas": {
    "observed_route": "ATLAS_LIGHT",
    "task_kind": "bug_fix",
    "explicit_target": true,
    "estimated_source_tokens": 940,
    "direct_callers": 7,
    "direct_callees": 3,
    "tests": 4,
    "packages_spanned": 3,
    "conflicts": 0,
    "unresolved_edges": 2,
    "history_available": true,
    "public_api": true
  }
}
```

The first experiment deliberately does not send Context IR or raw source.

## Shadow output contract

The adapter records raw primitive semantics rather than collapsing everything
into one synthetic confidence:

```json
{
  "schema_version": "jev-governor-shadow-v0.2.0",
  "authoritative": false,
  "requested_model": "jev-latest",
  "response_model": "jev-latest",
  "judgments": {
    "route": {
      "type": "choice",
      "choice": "ATLAS_DEEP",
      "confidence": 0.84,
      "probabilities": {
        "DIRECT": 0.04,
        "ATLAS_LIGHT": 0.12,
        "ATLAS_DEEP": 0.84
      }
    },
    "needs_escalation": {
      "type": "noul",
      "noul": 0.88
    },
    "risk": {
      "type": "score",
      "score": 2.1,
      "confidence": 0.74
    }
  }
}
```

Choice confidence must not be treated as overall workflow correctness. A Noul
has no separate confidence field and must not be mislabeled as one.

## Evaluation

For every eligible task retain both:

```text
A. what Atlas actually did
B. what JEV predicted in shadow
```

Compare against observed task evidence and outcomes, not against JEV itself.

Minimum useful fields:

- deterministic Atlas initial/final route;
- JEV route distribution and Choice confidence;
- JEV escalation/history/validation Noul probabilities;
- JEV risk Score and distribution;
- whether Atlas actually escalated;
- supplied working-set size;
- used working-set size;
- context expansion;
- source bytes/tokens;
- local retrieval work/latency;
- TypeSafe input/output token usage;
- accepted/rejected outcome when available;
- Context Yield validity/report when available.

## Primary hypotheses

### H1 — Route prediction

JEV route probabilities correlate with the minimum route that produced
sufficient evidence for accepted tasks.

### H2 — Escalation prediction

The `needs_escalation` Noul meaningfully predicts when an initial Atlas route
later required a deeper route.

### H3 — Cost usefulness

The System One evaluation is cheap enough that a future policy experiment could
plausibly save more Atlas/model work than the decision call costs.

### H4 — Calibration

Choice/Score confidence and Noul probabilities have useful empirical meaning on
Atlas tasks. Calibration is measured; it is not assumed from model claims.

## Promotion gates

No JEV result may influence production routing until all are true:

- sufficient task volume exists to compare policies;
- accepted-task correctness does not regress;
- uncertainty/coverage handling is preserved;
- data-export/privacy review passes;
- failure/offline behavior is explicit;
- replay/provenance is adequate for research;
- deterministic policy remains available as a full fallback;
- an accepted ADR authorizes any runtime authority.

Even after promotion, prediction remains separate from project truth.

## Harness

`scripts/jev-governor-shadow.py` is the research adapter.

Offline contract test:

```powershell
python scripts/jev-governor-shadow.py self-test
```

Inspect the exact TypeSafe state/questions without a network call:

```powershell
python scripts/jev-governor-shadow.py dry-run --input .\specs\roadmap-v15-v20\jev-shadow-example.json
```

For a live launch test, install the official SDK using your chosen Python
environment, configure it according to TypeSafe's current SDK documentation, and
run:

```powershell
python scripts/jev-governor-shadow.py call --input .\specs\roadmap-v15-v20\jev-shadow-example.json
```

## Explicit exclusions

This experiment does not:

- modify `atlas governor run`;
- add JEV to Context IR;
- add a catalogue migration;
- persist JEV output in Truth;
- add MCP authority;
- transmit source by default;
- replace deterministic task classification;
- convert JEV predictions into evidence;
- make performance claims;
- train or fine-tune a model.

Its only job is to generate comparable shadow evidence for deciding whether a
later adaptive-policy experiment is justified.
