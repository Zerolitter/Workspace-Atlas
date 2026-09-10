# Spec: `configuration-privacy-boundary`

## Objective

Make repository inclusion/exclusion and telemetry privacy a durable, fail-closed project contract before optimization data is trusted. A user must be able to initialize once, reconcile later, and prove which configuration governed each generation without silently weakening secret or provider policy.

## Current-state delta

At base revision:

- canonical-root confinement, default symlink rejection, direct provider spawn, environment filtering, and local catalogue placement already exist;
- configured exclusion entries are treated as regular expressions, while the shipped example contains glob-like `*.pem` and `*.key`; invalid expressions are silently skipped;
- the built-in secret set includes `.pem` but not `.key`, despite the module-level safety claim;
- directory traversal descends non-VCS trees before classifying files;
- `init --config` records a hash, but later reconciliation uses minimal defaults unless the config is supplied again;
- the accepted ADR mentions an unavailable portable mode.

These are architectural prerequisites from the private adoption audit. The intentionally private repository/expected unauthenticated 404 is not part of this module.

## Contract

### Pattern language

Choose exactly one documented language for each pattern field. Recommended default: Rust regex, matching current production semantics. Config parsing MUST compile every expression once and reject the entire config on any invalid expression. No invalid pattern may be ignored. If glob syntax is chosen instead, migration and escaping semantics require H2 approval.

Safe defaults MUST include `.key` alongside the documented credential patterns. The shipped example MUST parse and its examples MUST actually exclude representative files.

One bounded continuity reader exists for the creation-era application-owned
registered snapshot whose normalized configuration SHA-256 is
`1c9c12cedc368263b3b813905cf637860af20ab7d677ccc591f762f84c38e3b3`.
It applies only when the catalogue stores that same hash, policy `1.2.0`, a
creation time before regex validation commit `ce9cd8d`, a display name equal to
the snapshot, and workspace ID/root fingerprint values derived from the stored
root and display name. The exact ordered legacy entries `.env`, `*.pem`, and
`*.key` are then compiled as basename globs without changing their serialized
bytes or normalized hash. Any drift in hash, provenance, identity, or pattern
spelling fails closed. New or caller-supplied configs still use only the Rust
regex contract above; this reader does not authorize a second pattern language,
re-registration, or a configuration-continuity exception.

### Traversal

Directory-level vendor/build/generated/archive/secret exclusions MUST prune traversal before descending. `.gitignore` behavior is an H2 decision: either explicitly unsupported or implemented with documented precedence. It MUST NOT be implied by examples while ignored by the implementation.

### Configuration continuity

Initialization MUST register a configuration identity sufficient to detect later mismatch. Every generation records the effective configuration/provider-set hashes. A reconcile without a required registered config MUST fail with a typed actionable error or explicitly use a persisted application-owned copy; it MUST NOT silently substitute minimal defaults.

Persisting an absolute user path alone is insufficient for portable/reproducible use. H2 chooses one of:

1. application-owned bounded config copy plus content hash (recommended); or
2. registered canonical path plus hash and mandatory mismatch acknowledgement.

### Telemetry privacy

Raw task text and source bodies remain absent by default. Local opt-in requires a maximum byte length and retention/expiry value. Telemetry upload remains hard-disabled. Event details are closed/bounded data, not arbitrary source or prompt storage.

## Public interfaces

Potential additive surfaces, subject to H2/H6:

- `atlas init --config <path>` returns registered config identity and retention policy;
- `atlas reconcile` uses registered policy or returns typed `configuration_required` / `configuration_mismatch`;
- `atlas status` and `atlas doctor` report registered/effective hashes and mismatch without exposing raw config secrets;
- MCP mirrors shared application results after `mcp-conformance`.

No existing command is removed or silently reinterpreted.

## Persistence and migration

A new forward-only migration is allowed only if the current workspace/generation records cannot represent registered config continuity and retention. If required, it MUST be isolated from V1.5 metrics schema changes and prove:

- old catalogues open and retain truth;
- fresh and repeat open are idempotent;
- backup occurs before migration;
- checksum tampering/newer schema/read-only failure remains fail-closed;
- restore of the pre-migration backup is the rollback path; no downgrade claim.

## RED–GREEN–REFACTOR acceptance

**RED**

- shipped example fails because invalid patterns are detected rather than silently skipped;
- `.key` fixture is shown indexable under the current default;
- initialized custom config followed by reconcile without config demonstrates silent policy drift;
- excluded large directory demonstrates traversal proportional to excluded contents.

**GREEN**

- invalid expressions fail config parse with the field/index identified;
- `.key`, `.pem`, and example patterns exclude without reading bytes;
- reconcile reuses or requires the registered configuration exactly;
- excluded directories are pruned before descent;
- no raw task/source is retained without explicit bounded opt-in.

**REFACTOR**

- one compiled pattern-policy type is shared by parse and discovery;
- remove stale example/ADR claims or implement them—never retain two conventions;
- focused and full locked tests remain green.

## Verification commands

```sh
cargo test --locked discovery::
cargo test --locked cli::tests
cargo test --locked
cargo fmt --all -- --check
python scripts/check-public-hygiene.py
```

Add a focused integration test for shipped-config parse, config continuity, and directory pruning; do not use source-text assertions as behavioral proof.

## Boundaries

- Always: canonical confinement, symlink rejection default, parameterized SQL, bounded inputs, local-only storage, explicit config/evidence hashes.
- Ask first: pattern-language change, `.gitignore` support, portable catalogue mode, raw retention defaults, migration.
- Never: silently ignore invalid patterns, weaken secret defaults, upload telemetry, store raw prompt/source by default, execute configured commands through a shell.

## Success criteria

1. The shipped config is executable documentation and all exclusion examples behave as stated.
2. No reconcile can silently change effective config/provider policy from initialization.
3. Excluded directory cost tracks directory roots rather than contained file count.
4. Status/doctor expose configuration integrity without sensitive content.
5. Privacy retention is bounded, local, deletable, and covered by lifecycle tests.

## Open decisions

- **H2:** regex or glob; `.gitignore` semantics; application-owned config copy versus canonical-path registration; raw-task byte/TTL limits; whether stale ADR portable-mode text is removed or implemented.
