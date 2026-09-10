# ADR-020: Caller-driven reconciliation

**Status:** Accepted

## Decision

Workspace Atlas requires no watcher or daemon. Callers run `atlas reconcile`
when freshness is required. Content hashes prove change, unchanged files reuse
existing facts, and `atlas source` rejects stale source at the trust boundary.

## Consequences

- No always-on process or filesystem-event attack surface is required.
- Missed filesystem events cannot create false freshness claims.
- Automation decides when to reconcile; the catalogue records exactly which
  generation is active.
