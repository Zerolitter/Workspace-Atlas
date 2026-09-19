# Research backlog

This backlog records research questions only. Backlog status, including H8 status, does not authorize an experiment or implementation; each item remains subject to its stated gates and separate human approval.

## EXP-DPPM-001 — Demand-paged project memory

- **Classification:** **SAFE TO RECORD.** **SAFE TO PREPARE** only through neutral observability already required by [`SPEC-retrieval-instrumentation.md`](SPEC-retrieval-instrumentation.md), [`SPEC-serving-plane-readiness.md`](SPEC-serving-plane-readiness.md), and [`SPEC-context-yield-metrics.md`](SPEC-context-yield-metrics.md). **PROTOTYPE/PRODUCTION IMPLEMENTATION DEFERRED** until a trustworthy baseline exists and a human separately approves the experiment.
- **Research question:** Under fixed, versioned conditions, how do eager, lazy, and hybrid Serving Plane residency compare? Measure current behavior before calling it eager: SQLite and the operating system may already demand-page the relevant data.
- **Existing allowed preparation only:** stage timings and counts; cache hit, miss, and fallback; serving rows, bytes, and hash; cold/warm labels; and accepted outcomes. This entry authorizes no metric, schema, workflow, or code change.
- **Not authorized:** new RSS or page-fault counters; fragment-specific fields; durable schema; cache mode; scheduler; runtime selector; source-fragment cache; model-, vendor-, history-, or co-access-based policy; prototype or production code.
- **Preserved invariants:** Truth Plane authority; ready-versus-Truth equivalence; Context IR and decision identity and routing; Context Yield; source live-hash rules; H4 fixed, versioned, nonadaptive candidates; the H7 schema gate; and ask-first boundaries for lazy scheduling and caches.
- **Resident Knowledge Efficiency:** experiment-local raw observed/reported numerators and denominators only, with accepted-outcome validity. It is never a default composite score.
- **Prerequisites:** current-roadmap local acceptance; a trusted correctness/work-unit/cache/memory/latency instrumentation baseline frozen at an exact commit; accepted-outcome Context Yield; H3-A attestation; a cold/warm protocol; a cross-platform memory method; serving readiness equivalence; and separate human experiment approval.
- **Candidate order:** first compare fixed A/B/C eager, lazy, and hybrid candidates. OS/database-assisted paging remains a later, separate candidate.
- **Authority:** the downloaded proposal is evidence and input, not normative authority. H8 or research-backlog status does not auto-authorize an experiment or implementation.

## EXP-JEV-001 — JEV Governor Shadow

- **Classification:** **COMPLETED DIAGNOSTIC STUDY — NO PRODUCTION PROMOTION.**
- **Contract:** [`EXP-jev-governor-shadow.md`](EXP-jev-governor-shadow.md).
- **Original question:** Could TypeSafe JEV predict the minimum useful `DIRECT | ATLAS_LIGHT | ATLAS_DEEP` acquisition depth, escalation need, and related policy flags better than the deterministic Atlas policy?
- **Outcome:** The study established direct TypeSafe integration, typed Choice/Noul/Score handling, model pinning, privacy boundaries, replay/provenance mechanics, evidence logging, and shadow-only operation. The first live phase contained target leakage and is retained only as integration/cost/privacy evidence. A blind follow-up removed the leakage but did not produce evidence strong enough to justify JEV routing authority; minimum-sufficient-route and escalation ground truth also remained methodologically unresolved.
- **Decision:** Do not promote JEV into the Context Governor from this experiment. Do not interpret the historical shadow adapter as production authorization.
- **Preserved value:** the harness remains useful for TypeSafe API integration, typed primitive semantics, dry-run inspection, privacy checks, provider/model drift detection, and research evidence capture.
- **Future routing work:** would require a new experiment with task-specific sufficiency validators, genuine progressive escalation events, explicit deterministic baselines, and independent multi-workspace ground truth.
- **Authority:** closed research evidence only. Any future runtime-routing proposal requires separate evidence and an accepted ADR.

## EXP-JEV-002 — JEV Evidence Utility / Context Precision

- **Classification:** **ACTIVE RESEARCH — SHADOW/EXPERIMENTAL ONLY.** **NO PRODUCTION CONTEXT-SELECTION AUTHORITY.**
- **Contract:** [`EXP-jev-evidence-utility.md`](EXP-jev-evidence-utility.md).
- **Research question:** Can JEV improve Atlas context precision by judging the utility of already-retrieved candidate evidence, while deterministic Atlas policy remains responsible for what enters the working set?
- **Programming pattern:** follow the TypeSafe [Classifying RAG passages](https://docs.typesafe.ai/cookbooks/classifying_rag_passages) pattern at the architectural level: deterministic retrieval first; one task × candidate pair per evaluation; multiple independent probabilistic judgments; raw probabilities retained; fixed policy thresholds applied in ordinary code; conflicts preserved separately instead of collapsed into ordinary relevance.
- **Initial JEV judgments:** `is_task_relevant`, `contains_actionable_evidence`, `contradicts_task_assumption`, `important_to_preserve`, `is_validation_relevant`, and `contains_model_instruction` as independent Noul probabilities. No question asks JEV to make the final include/drop decision.
- **Candidate source:** Atlas Truth/Serving/graph mechanisms produce the candidates. JEV may score existing candidates but may not invent repository facts.
- **Deterministic authority:** Atlas-owned policy converts JEV probabilities into experimental outcomes such as KEEP, KEEP_AS_CONFLICT, KEEP_AS_VALIDATION, KEEP_AS_UNCERTAINTY, or DROP. Atlas conflict, unresolved, coverage, provenance, and exact-source rules take precedence over probabilistic utility.
- **First privacy boundary:** task text plus bounded deterministic evidence cards and source-reference metadata. Full source bodies, arbitrary files, diffs/patches, secrets, environment dumps, catalogue contents, and raw provider dumps remain excluded by default. A raw-source/passage arm requires a separate privacy/data-export revision and explicit approval.
- **Evaluation:** compare the same frozen Atlas candidate pool under (A) normal deterministic Atlas selection and (B) JEV-scored candidates plus one frozen deterministic threshold policy. Measure accepted outcome, working-set precision/recall, supplied tokens/bytes, unused evidence, context expansion, rediscovery, downstream exploration, wrong targets, tests/validation evidence, conflict retention, and JEV latency/token/cost.
- **Baselines:** normal Atlas deterministic selection and a transparent deterministic utility rule set using the same non-JEV candidate features. JEV must add value beyond a cheap rule set, not merely beyond an unfiltered dump.
- **Pilot:** approximately 10–20 real tasks with candidate sets containing both useful and distracting evidence. Freeze one question pack and threshold policy; retain all raw probabilities so alternative policies can be replayed offline without more JEV calls.
- **Success condition:** smaller or more precise context with preserved or improved accepted-task correctness and preserved required-evidence/conflict/uncertainty recall.
- **Not authorized:** modifying Truth Plane evidence; changing production Governor routing; adding JEV to migrations/MCP/source authority; treating JEV output as repository truth; per-task threshold tuning; silently exporting raw repository source.
- **Promotion gates:** multi-task and multi-workspace evidence; no accepted-task correctness regression; preserved required-evidence recall; privacy review; deterministic fallback; model/version drift handling; justified cost/latency; and a separately accepted ADR before any runtime use.
- **Authority:** JEV output is research evidence about candidate utility only. Prediction never becomes project truth.
