# Contributing

Workspace Atlas accepts bug reports and focused change proposals through
[GitHub Issues](https://github.com/Zerolitter/Workspace-Atlas/issues). Search for
an existing issue before opening one, describe the observable behavior and
version, and use a minimized fixture instead of private repository data.
Security findings must follow [SECURITY.md](SECURITY.md), not a public issue.

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
Preserve the Truth Plane, immutable-generation activation, fail-closed stale and
error behavior, exact-source verification, bounded outputs, local CLI authority,
and the read-only MCP authority boundary. Existing explicit V1 commands and the
original MCP tools remain compatibility contracts; versioned V2 contracts must
not be silently reinterpreted or persisted as durable V2 lifecycle state.

Do not commit credentials, private data, local catalogues, benchmark evidence,
machine-specific paths, generated artifacts, or orchestration state. Public
changes should include user-facing compatibility notes when behavior changes.
