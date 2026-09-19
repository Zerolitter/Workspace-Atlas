# EXP-JEV-001: JEV Governor Shadow

**Status:** Completed diagnostic study — no production promotion  
**Branch:** `research/jev-governor-shadow`  
**Production authority:** None  
**Runtime routing authority:** None  
**Truth Plane authority:** None

## Original question

Can TypeSafe JEV predict the minimum useful Workspace Atlas context-acquisition depth
(`DIRECT | ATLAS_LIGHT | ATLAS_DEEP`) and selected policy flags better than the
current deterministic policy, without weakening Atlas truth, privacy,
reproducibility, or source verification?

## Outcome

This experiment is closed as a **diagnostic research study**.

It successfully established:

- direct TypeSafe/JEV API integration;
- fixed-model execution and response-model verification;
- typed Choice / Noul / Score handling;
- privacy and secret-isolation boundaries;
- reproducible JSONL evidence capture;
- pre-decision sealing and leakage auditing;
- shadow-only execution with no Atlas production authority;
- provider latency/token/cost measurement.

It did **not** justify JEV influence over the Atlas Context Governor.

### Phase 1 finding

The first live study showed high apparent agreement with Atlas routing, but the
input packet included route/outcome/counter information that was too closely
related to the labels being predicted. That phase is therefore retained as
integration, privacy, reproducibility, and cost evidence — **not** as a valid
blind routing benchmark.

### Phase 2A finding

The blind protocol removed post-decision fields and sealed the JEV input before
Atlas execution.

Under that protocol:

- JEV no longer tracked the operator-selected Atlas command bucket well;
- confidence no longer tracked agreement reliably;
- the attempted minimum-sufficient-route replay was invalid as a hard label
  because forcing a Governor route only proved that the route was accepted, not
  that the returned evidence was sufficient for the task;
- no genuine progressive escalation events occurred, so escalation quality could
  not be evaluated from real positive events.

The study therefore does not support promotion of JEV into authoritative
`DIRECT / ATLAS_LIGHT / ATLAS_DEEP` routing.

## Interpretation

The negative result is useful.

The experiment showed that asking JEV to infer the whole Atlas acquisition depth
from compact pre-route metadata is not currently supported by trustworthy
evidence. It also exposed benchmark requirements that any future routing study
would need:

1. task-specific, predeclared sufficiency validators;
2. real progressive escalation events;
3. ground truth derived from task outcome/evidence sufficiency rather than route
   acceptance or operator intent;
4. explicit comparison against deterministic/simple baselines;
5. multi-workspace evaluation.

Those items are preserved as future methodology notes, not as an active product
roadmap.

## Architecture preserved

Throughout EXP-JEV-001:

- Atlas deterministic behavior remained authoritative;
- JEV remained shadow-only;
- JEV predictions never became project evidence;
- no JEV catalogue migration was added;
- no JEV MCP authority was granted;
- no Truth Plane or Serving Plane authority was transferred;
- exact-source verification remained unchanged;
- Atlas continued to operate fully without TypeSafe/JEV.

## Why the active JEV research is being rescoped

The TypeSafe cookbook
[Classifying RAG passages](https://docs.typesafe.ai/cookbooks/classifying_rag_passages)
demonstrates a different and more Atlas-aligned System One pattern:

1. retrieve a high-recall candidate set;
2. evaluate each query × candidate pair with multiple independent probabilistic
   questions;
3. keep raw probabilities;
4. apply final thresholds and routing in ordinary deterministic code;
5. preserve conflicting evidence separately rather than collapsing everything
   into one relevance score.

That pattern maps directly onto Atlas's Context Plane and its central research
question: how small can the working set become without reducing correctness?

The active successor is therefore **EXP-JEV-002 — Evidence Utility / Context
Precision**.

See: [EXP-jev-evidence-utility.md](EXP-jev-evidence-utility.md).

## Historical implementation notes

The original shadow adapter and evidence remain useful research artifacts for:

- TypeSafe API integration;
- typed primitive handling;
- replay/provenance patterns;
- privacy checks;
- model pinning;
- offline dry-run inspection;
- benchmark instrumentation.

They must not be interpreted as an authorization to revive JEV Governor authority.

## Promotion status

**Closed: no promotion.**

Any future proposal to give JEV routing authority requires a new experiment,
new evidence, and a separately accepted ADR. EXP-JEV-001 does not authorize it.
