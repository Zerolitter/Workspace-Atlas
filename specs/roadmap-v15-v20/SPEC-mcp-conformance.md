# Spec: `mcp-conformance`

## Objective

Make `atlas-mcp` a conforming, discoverable stdio MCP server before V1.5/V2.0 add tools. Preserve ADR-017: MCP remains a thin transport over the same typed application functions as the CLI.

## Current-state delta

At base revision, 13 tools expose strong shared application behavior, structured JSON results, and typed Atlas errors. The adapter nevertheless:

- returns Atlas capability version `1.3` as MCP `protocolVersion` without reading the client's requested version;
- does not enforce the legacy initialize → initialized → operation lifecycle and has no modern per-request version/discovery behavior;
- advertises every tool as `inputSchema: {"type":"object"}`, hiding required and optional parameters;
- has repository tests using ad hoc newline JSON-RPC, not a current real MCP client.

The audit’s official MCP `2025-11-25` lifecycle evidence remains valid for legacy clients. Current stable `2026-07-28` uses per-request version metadata and `server/discover`; a dual-era server may support both. Tool definitions require valid JSON Schema and accurate arguments. Sources:

- https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
- https://modelcontextprotocol.io/specification/2025-11-25/server/tools

## Contract

### Protocol negotiation

- Maintain the exact H6A-selected protocol-version/era set independent of Atlas capability/schema versions.
- For modern `2026-07-28`, validate per-request version metadata, implement `server/discover`, and return `UnsupportedProtocolVersionError` with supported dates.
- For legacy `2025-11-25`, parse/validate initialize fields, negotiate the supported date, and require `notifications/initialized` before operation.
- Keep `atlas_capability_protocol_version`, Context IR schema, temporal schema, and other Atlas versions under a namespaced capability/result field, never as an MCP protocol date.
- stdio remains the only transport; selected SDK/static framing and lifecycle are H6A decisions.
- Modern and legacy state machines, when both selected, remain isolated and deterministic for one stdio process.

### Tool discovery

Each tool definition MUST have:

- unique stable name and description;
- closed JSON Schema input with `properties`, `required`, types, enums, bounds, and `additionalProperties: false` where compatible;
- output schema when stable structured output can be described without duplicating canonical Rust types;
- typed validation errors for malformed/unknown arguments;
- no selection, ranking, persistence, or source-authority logic in the adapter.

Schemas MUST be generated from or tested against one canonical typed definition. H6A chooses official SDK/schemars versus static `serde_json`; any SDK/dependency path receives a separate exact dependency/lockfile task before adapter mutation.

### Lifecycle and cancellation

- Handle the selected modern requests and legacy initialized/cancellation notifications without producing responses to notifications.
- Bound requests; respect Atlas hard budgets and provider/process timeouts.
- Shut down cleanly when stdin closes.
- Do not add network transport in this initiative.

## Public interfaces

Existing 13 tool names and result semantics remain supported. Correcting protocol behavior is not a change to Atlas capability `1.3`. T03 proves binary-level conformance; T22 separately proves H6A-selected real-client compatibility before new V1.5/V2.0 tools are release-ready.

## RED–GREEN–REFACTOR acceptance

**RED**

- current initialize probe requesting `2025-11-25` receives `1.3`;
- current `tools/list` lacks required `workspace_root`/command properties;
- legacy tool call before initialized currently reaches operation handling, and modern discovery/version handling is absent;
- selected current clients cannot discover/invoke a versioned accurate adapter contract.

**GREEN**

- selected modern and/or legacy version behavior matches the official contracts;
- unsupported/misordered requests fail typed without executing tools;
- all tools advertise accurate closed input schemas and reject unknown/wrong-typed fields;
- binary-level status, seeded Context IR, source attribution, and typed error cases pass; T22 owns external client execution;
- CLI and MCP structured results remain logically identical.

**REFACTOR**

- centralize version negotiation and schema definitions;
- keep one call-tool dispatch to shared application functions;
- remove ad hoc duplicated parameter parsing where a canonical boundary helper earns its weight.

## Verification commands

```sh
cargo test --locked --test catalogue_routing
cargo test --locked --test task_compiler_v13
cargo test --locked --test temporal_intelligence_v14
cargo test --locked --test source_telemetry_surfaces
cargo test --locked
cargo fmt --all -- --check
```

Add a focused spawned-binary MCP conformance integration suite. T22 owns pinned automated/manual real-client evidence and its exact version matrix.

## Boundaries

- Always: official protocol date/version semantics; typed/closed inputs; shared CLI/MCP application layer; local stdio; bounded requests.
- Ask first: supported protocol-version set, framing change, output schemas, new tools, dependency/SDK adoption.
- Never: advertise Atlas version as MCP version, duplicate compiler policy in adapter, add network/cloud transport, expose raw task/source by default.

## Success criteria

1. The selected MCP era(s) discover/initialize as applicable, list accurate schemas, and invoke representative tools through the real binary.
2. Unsupported/malformed/misordered lifecycle requests fail predictably without executing a tool.
3. All existing tool names remain and structured results match CLI application results.
4. Capability versions are unambiguous and independently testable.
5. New compiler tools cannot regress lifecycle/schema conformance.

## Open decisions

- **H6A:** select H6A-A/H6A-B/H6A-C in [`decision-checkpoint.md`](decision-checkpoint.md), covering exact protocol dates, SDK/static schema authority, dependency task, and T22 automated/manual clients.
