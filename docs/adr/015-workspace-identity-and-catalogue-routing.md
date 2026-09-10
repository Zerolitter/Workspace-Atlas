# ADR-015: Workspace identity and catalogue routing

**Status:** Accepted

## Decision

`workspace_id` is:

```text
"ws_" + first_32_lowercase_hex(blake3(
  canonical_root_path_utf8 + 0x1f + workspace_display_name_utf8
))
```

Default catalogues are outside the indexed workspace:

- Windows: `%LOCALAPPDATA%\WorkspaceAtlas\catalogues\<workspace_id>.sqlite`
- macOS: `~/Library/Application Support/WorkspaceAtlas/catalogues/<workspace_id>.sqlite`
- Linux: `${XDG_DATA_HOME:-~/.local/share}/workspace-atlas/catalogues/<workspace_id>.sqlite`

Windows requires `LOCALAPPDATA`; macOS requires `HOME`. Linux uses a non-empty
`XDG_DATA_HOME`, otherwise `HOME/.local/share`. The selected base directory must
be absolute. Missing or relative platform inputs are errors and never fall back
to the current working directory.

Paths are normalized before physical canonicalization. Workspace reads must
remain under the approved canonical root. Symlinks and junction escapes are
rejected by default.

Portable mode is explicit (`atlas init --in-workspace`). It stores the catalogue
at `<workspace_root>/.workspace_atlas/catalogue.sqlite`; that directory is an
immutable discovery exclusion.

## Root-addressed routing

`atlas init` atomically writes an application-owned locator keyed only by the
canonical-root fingerprint. The locator records the canonical root, workspace
ID, and expected catalogue path. Root-addressed commands physically resolve the
locator and target, constrain both to the canonical platform catalogue
directory, and verify the catalogue workspace row. `--catalogue` remains an
explicit authoritative override.

For an installation without a locator, migration performs a locking, read-only
scan. It succeeds only when exactly one catalogue has an absolute, already
canonical stored root that matches the requested root. Relative or
non-canonical stored roots are rejected without interpreting them relative to
the current directory. Zero or multiple matches fail closed. Recovery requires
`atlas init <absolute-root>` or an explicit `--catalogue <path>`.

## Consequences

- The same root with a different display name has a different workspace ID.
- Moving a workspace to another canonical root registers a new identity.
- Local catalogues cannot pollute their own index.
- Ambiguous or escaping routes never select an arbitrary catalogue.
