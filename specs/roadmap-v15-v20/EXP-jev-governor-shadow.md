# EXP-JEV-001: JEV Governor Shadow

**Status:** Research only  
**Branch:** `research/jev-governor-shadow`  
**Production authority:** None  
**Runtime routing authority:** None  
**Truth Plane authority:** None

## Question

Can a small structured decision model predict the minimum useful Workspace Atlas
context-acquisition depth for a task better than the current deterministic
policy, without weakening Atlas truth, privacy, reproducibility, or source
verification?

The first target is **route prediction**, not evidence generation:

```text
DIRECT | ATLAS_LIGHT | ATLAS_DEEP
```

Secondary shadow decisions may include task kind, whether temporal/history
evidence is likely to be useful, and whether a validation plan is likely to be
required.

## Why JEV is a plausible experiment

JEV 1.13 is currently exposed by OpenRouter as a TypeSafe structured decision
model intended for routing/classification-style software decisions. This
experiment treats that claim as a hypothesis to test, not as an Atlas
assumption.

JEV is never a repository evidence provider. A prediction from JEV is not a
symbol, relationship, source fact, coverage statement, conflict resolution, or
generation fact.

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
                predicted route only
                         │
                         ▼
                 comparison / metrics
```

JEV must not sit on the authoritative execution path in this experiment.

The authoritative Atlas run happens exactly as it would without JEV. The shadow
adapter may consume a bounded observation of the task and already-derived Atlas
metadata and produce a prediction for later comparison.

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
- provider outputs containing source text.

The harness rejects obvious raw-source fields unless a future research revision
explicitly changes this contract.

## Privacy and provider boundary

Using OpenRouter/JEV sends the permitted experiment payload to an external
service. It is therefore opt-in and must never be silently enabled by normal
Atlas commands.

Requirements:

1. `OPENROUTER_API_KEY` is read from the process environment only.
2. The key is never written to experiment output.
3. Network execution requires an explicit `call` action.
4. `dry-run` must work without credentials and show the exact outbound body.
5. Persisted output hashes task/input identity by default rather than copying
   raw task text into the result.
6. Atlas remains fully usable with no JEV/OpenRouter configuration.

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

The contract is intentionally small. Do not feed Context IR or raw source into
the first experiment simply because the model has a large context window.

## Shadow output contract

The adapter normalizes a JEV answer into:

```json
{
  "schema_version": "jev-governor-shadow-v0.1.0",
  "authoritative": false,
  "model": "typesafe/jev-1.13",
  "input_hash": "...",
  "task_hash": "...",
  "decision": {
    "route": "ATLAS_DEEP",
    "task_kind": "behavior_change",
    "requires_history": false,
    "requires_validation_plan": true,
    "risk": "broad",
    "confidence": 0.84,
    "reasons": [
      "cross_module_scope",
      "validation_requirement"
    ]
  }
}
```

No free-form rationale or chain-of-thought is required.

## Closed route vocabulary

- `DIRECT`
- `ATLAS_LIGHT`
- `ATLAS_DEEP`

## Initial task-kind vocabulary

Keep the first research vocabulary aligned with existing Atlas concepts rather
than inventing a new ontology:

- `bug_fix`
- `behavior_change`
- `api_change`
- `refactor`
- `configuration_change`
- `audit`
- `unknown`

## Initial reason codes

- `explicit_target_sufficient`
- `bounded_relationships_needed`
- `cross_module_scope`
- `public_api_surface`
- `unresolved_edges`
- `conflict_state`
- `temporal_requirement`
- `validation_requirement`
- `high_context_cost`
- `unknown_task_kind`
- `insufficient_metadata`

The adapter validates this closed vocabulary. New reason codes require an
experiment contract revision.

## Evaluation

For every eligible task, retain both:

```text
A. what Atlas actually did
B. what JEV predicted in shadow
```

Then compare JEV against observed task evidence, not against human preference.

Minimum useful fields:

- deterministic Atlas route;
- JEV predicted route;
- confidence;
- whether Atlas escalated after initial acquisition;
- supplied working-set size;
- used working-set size;
- context expansion;
- source bytes/tokens;
- local retrieval time/work units;
- accepted/rejected outcome when available;
- Context Yield validity/report when available.

## Primary hypotheses

### H1 — Route prediction

JEV predictions correlate with the minimum route that produced sufficient
evidence for accepted tasks.

### H2 — Escalation prediction

A low-confidence/light prediction plus Atlas metadata can identify tasks that
later require escalation to DEEP.

### H3 — Cost usefulness

Using JEV as a shadow predictor is cheap enough that a future policy experiment
could plausibly save more Atlas/model work than the decision call costs.

### H4 — Calibration

JEV confidence has useful empirical meaning. Confidence bins should be compared
against observed route/task outcomes; do not assume calibration from vendor
claims.

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

Even after promotion, prediction must remain separate from project truth.

## Harness

`scripts/jev-governor-shadow.py` is the first research adapter.

Examples:

```powershell
python scripts/jev-governor-shadow.py dry-run --input .\jev-task.json
```

```powershell
$env:OPENROUTER_API_KEY = "<key>"
python scripts/jev-governor-shadow.py call --input .\jev-task.json
```

Run the local parser/contract test without network access:

```powershell
python scripts/jev-governor-shadow.py self-test
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
- make performance claims;
- train or fine-tune a model.

Its only job is to generate **comparable shadow evidence** for deciding whether
a later adaptive-policy experiment is justified.
