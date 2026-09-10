# Workspace Atlas contributor guidance

## Public-release invariant

v2.0.0 is the first public Workspace Atlas release. V1-labelled CLI commands,
all 19 advertised MCP tools, Context IR `1.0.0`/`2.0.0`, planners, lifecycle
records, and milestone labels are internal contract generations shipped inside
v2.0.0. Do not describe them as earlier public releases, an installed external
V1 user base, or obsolete interfaces. All 19 MCP tools are supported v2 public
interfaces; none is being removed.

## Contributor checks

- Treat source, builds, tests, and runtime evidence as authoritative.
- Preserve explicit operations alongside Governor-routed operations; never
  silently redirect explicit commands.
- Keep Context IR versions strictly separated. Unsupported newer schemas fail
  closed, and parsing a supported older document satisfies V2 only where the
  contract explicitly permits it.
- Block candidate activation when a required provider fails. Keep optional
  provider failure visible as degraded coverage.
- Require live verification before change-mode source use. Atlas cannot edit
  workspace source.
- Keep Truth Plane activation atomic and the Serving Plane derived and
  rebuildable.
- Give MCP no destructive, filesystem-write, or source-materialization
  authority.
- Keep changes focused; run relevant formatting, tests, and
  `python scripts/check-public-hygiene.py` before committing.
