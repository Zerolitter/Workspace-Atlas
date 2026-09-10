# Contributing

Workspace Atlas accepts bug reports and focused change proposals through
[GitHub Issues](https://github.com/Zerolitter/Workspace-Atlas/issues). Search for
an existing issue before opening one, describe the observable behavior and
version, and use a minimized fixture instead of private repository data.
Security findings must follow [SECURITY.md](SECURITY.md), not a public issue.

v2.0.0 is the first public release. V1-labelled commands, Context IR
`1.0.0`/`2.0.0`, planners, lifecycle records, capability milestones, and the
19 advertised MCP tools are internal contract generations shipped together in
v2.0.0, not evidence of earlier public releases or an installed V1 user base.

Build and verify changes from the repository root:

```sh
cargo fmt --all -- --check
cargo build --locked --bins
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
python -m unittest scripts/test_check_public_hygiene.py
python scripts/check-public-hygiene.py
cargo package --list --locked
```

Keep changes focused and dependency-free unless a dependency is necessary.
Before submitting a public behavior change, check all of these invariants:

- Source, builds, tests, and runtime evidence remain authoritative.
- Explicit operations coexist with Governor-routed operations and are never
  silently redirected.
- Context IR versions remain strictly separated. Unsupported newer schemas
  fail closed; parsing a supported older document does not satisfy a V2
  requirement unless that use is explicitly permitted.
- Required-provider failure blocks candidate activation. Optional-provider
  failure remains visible as degraded coverage.
- Reconciliation activates Truth Plane generations atomically. The Serving
  Plane is derived and rebuildable.
- Change-mode source is live-verified. Atlas cannot edit workspace source, and
  MCP has no destructive, filesystem-write, or source-materialization
  authority.
- All 19 advertised MCP tools remain supported v2 public interfaces; this
  release removes none of them.

Do not commit credentials, private data, local catalogues, benchmark evidence,
machine-specific paths, generated artifacts, or orchestration state. Public
changes should include user-facing compatibility notes when behavior changes.
