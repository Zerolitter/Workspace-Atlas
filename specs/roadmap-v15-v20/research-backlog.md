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

- **Classification:** **SAFE TO PREPARE AND RUN IN SHADOW ONLY** under the dedicated contract in [`EXP-jev-governor-shadow.md`](EXP-jev-governor-shadow.md). **RUNTIME/PRODUCTION AUTHORITY DEFERRED** unless later evidence and a separately accepted ADR authorize it.
- **Research question:** Can a structured decision model predict the minimum useful `DIRECT | ATLAS_LIGHT | ATLAS_DEEP` acquisition depth, escalation need, and selected task-policy flags better than the current deterministic policy without weakening correctness or Atlas invariants?
- **Authorized preparation:** an external research adapter, bounded task/metadata input contract, closed decision vocabulary, dry-run inspection, explicit opt-in network call, redacted/hash-based local output, and comparison against existing Atlas route/Context Yield evidence.
- **Not authorized:** modifying `atlas governor run`; giving JEV Truth Plane, Serving, source-verification, Context IR, lifecycle, catalogue, MCP, mutation, or destructive authority; adding a production dependency or catalogue migration; transmitting raw source by default; treating JEV confidence or predictions as project evidence.
- **Preserved invariants:** deterministic Atlas routing remains authoritative; source stays authoritative; prediction never becomes evidence; exact-source live verification remains mandatory; Atlas works fully offline with no JEV/OpenRouter configuration.
- **Data boundary:** task text plus bounded derived Atlas metadata only for the first experiment. Raw source, file contents, diffs, snippets, credentials, arbitrary catalogue contents, and environment dumps are excluded by default.
- **Evaluation:** compare JEV prediction/confidence to the route Atlas actually required, later escalation, supplied/used working sets, context expansion, local work/latency, accepted outcome, and Context Yield where valid.
- **Promotion gates:** useful empirical calibration; no accepted-task correctness regression; preserved uncertainty/coverage behavior; privacy/data-export review; explicit offline/failure behavior; deterministic fallback; replay/provenance; separate human approval and ADR before any runtime authority.
- **Authority:** JEV output is research evidence about policy quality, never repository truth and never the label by definition. Actual task outcomes and observed context use remain the evaluation target.
