# SCIP Schema Provenance (SCIP-001)

## Protobuf schema

| Field | Value |
|---|---|
| Upstream repository | `github.com/scip-code/scip` (formerly `sourcegraph/scip`) |
| Pinned commit | `e01e97efac2f6b8c266b4d04825f1f1eab7b8f6c` |
| Commit date | 2026-07-06T05:34:18Z |
| Commit message | "schema: Add Odin language (#441)" |
| File | `scip.proto` |
| Local copy | `schemas/scip/scip.proto` |
| SHA-256 | `b38021b65ef90cbbf6af9c829ff75192859ad9b5da05439ef154bea4ceb2bf03` |
| Licence | Apache License 2.0 |
| Review date | 2026-08-29 |
| Retrieval | `https://raw.githubusercontent.com/scip-code/scip/e01e97efac2f6b8c266b4d04825f1f1eab7b8f6c/scip.proto` |

## Binding strategy

Workspace Atlas does **not** vendor a generated protobuf binding (`prost`,
`protobuf`, or similar codegen) for this schema. `src/scip_decoder.rs`
implements a minimal, hand-written, bounded protobuf wire-format reader
scoped to exactly the messages Atlas needs (`Index`, `Metadata`, `ToolInfo`,
`Document`, `Occurrence`, `SingleLineRange`, `MultiLineRange`,
`SymbolInformation`, `Relationship`). This is a deliberate choice, not an
oversight:

1. A hand-rolled reader keeps every length/count/nesting limit
   (`SCIP-002` "bounded protobuf decode") in one auditable place, enforced
   *before* any Atlas fact is created, rather than trusting a general-purpose
   codegen'd deserializer's own (looser) limits.
2. It avoids a build-time dependency on the external `protoc` compiler,
   which `prost-build`/`protobuf-codegen` normally require and which is not
   guaranteed present on every operator machine.
3. SCIP's wire shapes used here are simple (varints, length-delimited
   strings/submessages, repeated fields, one `oneof` for typed ranges) —
   well within what a bounded reader can implement correctly without a full
   protobuf reflection/codegen stack.

Unknown fields (including a schema evolution the pinned `.proto` doesn't yet
describe) are skipped via the wire-type-driven skip routine, never silently
misinterpreted as a different field.

## TypeScript/JavaScript pilot provider

| Field | Value |
|---|---|
| Package | `@sourcegraph/scip-typescript` |
| Pinned version | `0.4.0` |
| Upstream repository | `github.com/sourcegraph/scip-typescript` |
| Release tag | `v0.4.0` |
| Licence | Apache License 2.0 |
| Review date | 2026-08-29 |
| Provider descriptor | `providers::scip_typescript_descriptor()` (`src/providers.rs`) |

Operator installation (not performed by the Atlas binary — `RUN-012`/ADR-026):

```text
npm install --global @sourcegraph/scip-typescript@0.4.0
```

Atlas never runs this command itself. Availability is probed
(`provider_runtime::probe_provider`) and reported; an unavailable optional
provider degrades coverage rather than failing the workspace.

## Update procedure

A future SCIP schema or `scip-typescript` version bump must:

1. Re-fetch `scip.proto` from a new pinned commit and update the checksum,
   commit hash, version, and license record above.
2. Re-run the bounded-decoder corpus tests in `src/scip_decoder.rs`.
3. Regenerate and review the checked semantic fixture under
   `tests/scip_fixtures/`, then run the real provider acceptance test.
4. Record the before/after versions, checksums, commands, and results in the
   public change review.
5. Emit a `provider_invalidation_event` with
   `provider_version_changed` or `normalized_schema_changed`; never substitute
   the runtime silently.
