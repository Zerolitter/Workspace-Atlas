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
  "command": ["local-runner", "--json-stdio"],
  "executables": [
    {"name": "adapter", "path": "<ABSOLUTE_ADAPTER>", "version_args": []},
    {"name": "atlas-mcp", "path": "<ABSOLUTE_ATLAS_MCP>", "version_args": []},
    {"name": "omp", "path": "<ABSOLUTE_OMP>", "version_args": ["--version"]}
  ]
}
```

For the workstation OMP/Ollama path, use the dependency-free
`scripts/local-omp-json-stdio.py` adapter. Put the operator-specific adapter JSON
outside the checkout (for example under `<LOCAL_TEMP_ROOT>`) and use absolute
paths because each arm runs from its own directory:

```json
{
  "schema_version": "1.0.0",
  "adapter": "omp-json-stdio",
  "model": "ollama/<INSTALLED_TOOL_CAPABLE_MODEL>",
  "command": [
    "py",
    "-3",
    "<WORKSPACE>\\scripts\\local-omp-json-stdio.py",
    "--model",
    "ollama/<INSTALLED_TOOL_CAPABLE_MODEL>",
    "--atlas-mcp",
    "<WORKSPACE>\\target\\debug\\atlas-mcp.exe",
    "--workspace-root",
    "<WORKSPACE>",
    "--catalogue",
    "<LOCAL_TEMP_ROOT>\\atlas-campaign.sqlite",
    "--omp-command",
    "<ABSOLUTE_OMP>"
  ],
  "executables": [
    {"name": "adapter", "path": "<WORKSPACE>\\scripts\\local-omp-json-stdio.py", "version_args": []},
    {"name": "atlas-mcp", "path": "<WORKSPACE>\\target\\debug\\atlas-mcp.exe", "version_args": []},
    {"name": "omp", "path": "<ABSOLUTE_OMP>", "version_args": ["--version"]}
  ]
}
```

The adapter validates that the request arm agrees with `ATLAS_ENABLED`, creates
an isolated credential-free OMP configuration in the arm directory, and exposes
the local `atlas-mcp` command only for the ON arm. It leaves OMP tools enabled
and presents MCP tools directly rather than as dynamic-device descriptions.
Use an already-installed Ollama model that demonstrates native tool calls; model
availability alone is insufficient. OMP JSON events, diagnostics, the exact
request/prompt, actual Atlas request/result events, model result (when valid),
and generated configuration remain in that arm directory. Stdout contains only
the single harness response object. A true ON acceptance is supported only when
the task explicitly requests Atlas status, the model returns a schema-valid
decision, a unique matched non-error `atlas_status` request/result pair has a
valid payload, and the model's result exactly matches the bounded receipt derived
from that payload. Unsupported task claims remain unavailable. An unrelated
Atlas result, configuration presence, model text naming a tool, or process
success never becomes acceptance. Duplicate, malformed, mismatched, unpaired,
error, or invalid-payload tool evidence fails closed. Tokens, duration, tool/file
counts, source bytes, and Atlas fields come only from actual OMP events.
Unsupported measurements remain `null`.

Do not put credentials in the adapter command, model name, task IDs, executable
paths, or version commands. Executable provenance must contain unique `adapter`,
`atlas-mcp`, and versioned `omp` roles; up to five additional roles are allowed.
The harness hashes each declared executable's contents, probes the declared OMP
version command, and binds the sanitized executable records into the campaign
identity. It hashes command, model, adapter, and task identities separately. The
campaign identity also binds the timeout, repetition count, task bound, and
selected tasks. Only sanitized command/model displays and executable basenames
are retained. The exporter recomputes executable and campaign identities and
cannot mark evidence complete when a mandatory role or concrete OMP version is
missing; contradictions fail export.
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

Only the exported five-file bundle is portable and sanitized for sharing. The
raw campaign tree is private local diagnostic evidence: it retains OMP databases,
WAL/SHM state, full event streams, prompts, local paths, and model reasoning.
Never publish or distribute the raw campaign tree.

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
