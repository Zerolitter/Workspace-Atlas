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
