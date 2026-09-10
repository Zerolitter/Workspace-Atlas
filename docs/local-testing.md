# Local testing and benchmark evidence

These dependency-free Python tools are checkout-local operator utilities. They do not
change Atlas CLI, MCP, routing, or Rust benchmark contracts. The Windows examples
below are PowerShell commands run from the repository root with Python 3.

## Audit disposable development artifacts

Preview the exact files eligible for removal:

```powershell
py -3 scripts/local-artifact-cleanup.py --workspace .
```

The JSON report is deterministic and separates `proposed`, `deleted`, `protected`,
and `unknown` paths with byte counts and artifact classes. Preview is the default;
removal requires the explicit `--apply` flag. The fixed candidates are Cargo
`target/debug`, `target/release`, and `target/doc` outputs, generated fixture roots,
and provider temporary/output roots. A benchmark scratch directory is eligible only
when explicitly supplied as a workspace-relative path:

```powershell
py -3 scripts/local-artifact-cleanup.py --workspace . --benchmark-scratch .local-atlas-scratch --apply
```

Apply uses identity-bound deletion on Windows. On platforms where the exact
validated directory entry cannot be bound through deletion, apply refuses that
entry and reports it as `unknown` rather than weakening the safety guarantee.

The tool never runs `cargo clean` or removes the whole `target` directory. Links,
reparse points, containment escapes, unclassified `target` content, raw observations,
manifests, accepted-outcome records, evidence, portable bundles, results CSV files,
and directories containing `.atlas-preserve` are not deleted. Review `unknown` and
`protected` entries manually; do not reinterpret them as cleanup authorization.

## Run a bounded local Atlas OFF/ON comparison

Start from [`local-ab-example-tasks.json`](../scripts/local-ab-example-tasks.json) and
create an operator-local adapter JSON:

```json
{
  "schema_version": "1.0.0",
  "adapter": "json-stdio",
  "model": "operator-local-model",
  "command": ["local-runner", "--json-stdio"]
}
```

Do not put credentials in the adapter command, model name, or task IDs. The harness
stores separate SHA-256 identities for the command, model, adapter, and task
content. The campaign identity also binds the timeout, repetition count, task
bound, and selected tasks. Only sanitized command/model displays are retained.
The harness invokes the command directly, never through a shell, and contains
the process tree for both successful and timed-out runs. For each explicitly
selected task and repetition it runs `off`
and then `on`, with a separate arm directory and `ATLAS_ENABLED` set to `0` or `1`
respectively:

```powershell
py -3 scripts/local-atlas-ab.py --workspace . --tasks scripts/local-ab-example-tasks.json --adapter config/local-adapter.json --destination .local-atlas-runs/campaign-001 --task inspect-route-source --repetitions 1 --timeout 300
```

Task count, repetitions, input/output sizes, and per-arm timeout are bounded. The
runner receives one JSON request on stdin with `schema_version`, `task_id`, `prompt`,
`repetition`, and `arm`, and returns one JSON object on stdout. Supported observation
fields are:

- `accepted_outcome`: `{ "accepted": true|false|null, "state": "..." }`
- adapter-reported `elapsed_ms`, retained separately as `adapter_elapsed_ms`
- independently measured `runner_wall_ms`
- `tool_calls`, `files_read`, and `source_bytes_read`
- `atlas_route`, `atlas_runtime_ms`, and `context_expansion`

Missing measurements remain JSON `null`; they are never inferred. Process exit zero
does not imply acceptance. Timeout, non-zero exit, oversized output, non-finite
numbers, and malformed output remain explicit unavailable/failure observations.
The campaign incrementally binds `raw.ndjson` to `harness-manifest.json`; an
unexpected interruption leaves the completed observations marked `partial` rather
than promoting them to a complete run. Each isolated arm also contains a sanitized
`result.json`. The harness makes no network or model selection decision; the
operator supplies the local command.

## Export a portable evidence bundle

Export Atlas JSON or NDJSON observations without modifying the input:

```powershell
py -3 scripts/benchmark-evidence-export.py --workspace . --input .local-atlas-runs/campaign-001/raw.ndjson --destination .local-atlas-runs/campaign-001-bundle
```

The destination must be new, workspace-contained, and free of link/reparse ambiguity.
The exporter rejects malformed input, duplicate JSON fields, non-finite numbers,
count/hash/state mismatches, existing destinations, and containment escapes. For
harness raw output it validates the adjacent harness manifest; if that manifest is
missing, the bundle is partial and lists missing identity, count, and pairing
provenance. For Atlas capacity raw output it validates the adjacent capacity summary
when present and otherwise marks the input partial. It preserves failures, nulls,
and unknown fields in the raw payload. `environment.json` contains only fixed host
fields and an optional allowlist from `--environment`. Allowlisted strings matching
absolute/home paths, control characters, or obvious secret-bearing assignments such
as `password=...` are omitted; arbitrary environment variables are never dumped.

Every bundle contains exactly:

- `manifest.json`: bundle/input schemas and identity, hashes, byte counts, record
  counts, source schema versions, and partial/complete state
- `environment.json`: allowlisted, sanitized environment identity
- `raw.json`: lossless parsed observation objects
- `summary.json`: accepted, rejected, unavailable, failure, and total counts
- `results.csv`: stable ordering plus bounded observation fields and canonical
  `record_json` that preserves detailed failures, unknown fields, and null/missing
  distinctions

All text files use stable UTF-8 LF newlines. Bundle creation is exclusive: rerunning
requires a new destination rather than overwriting accepted evidence.
