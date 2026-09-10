# ADR-017: Shared CLI and MCP application layer

**Status:** Accepted

## Decision

`atlas-mcp` is a newline-delimited JSON-RPC 2.0 stdio adapter. CLI and MCP
handlers call the same public application functions and return the same typed
results. Indexing, resolution, ranking, and source-verification logic must not
live in the transport adapter.

## Consequences

- Adding a capability requires transport mapping, not a second implementation.
- CLI/MCP parity can be asserted byte-for-byte for equivalent requests.
- The adapter remains provider-neutral and does not gain filesystem authority
  beyond the underlying application call.
