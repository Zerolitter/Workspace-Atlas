# Security readiness draft

## Status and authority

This is a repository-only, package-excluded pre-release draft. It does not announce a public release, supported distribution channel, security contact, response-time commitment, signing system, provenance service, or SBOM tool. Those remain H8 decisions under the [distribution-gate specification](../roadmap-v15-v20/SPEC-ci-distribution-gates.md).

The [README compatibility table](../../README.md#from-a-task-to-evidence) is the single authority for route, capability, interface, and version relationships. This document does not reproduce that table.

## Threat model

### Assets

- Workspace source and credentials, which Atlas must not mutate, copy into telemetry, or expose without live source authorization.
- The local SQLite catalogue, its immutable generations, registered configuration snapshot, locator, and derived Serving state.
- Configuration and provider identity, hashes, execution evidence, lifecycle records, and bounded diagnostic output.
- The local CLI/MCP process boundary and operator-owned provider executables.
- Package and private-CI integrity: locked dependencies, exact package membership, and package-excluded private plans and raw evidence.

### Trust boundaries and threats

- **Workspace input is untrusted.** Paths, source, configuration, archives, SCIP payloads, and repository content may be malformed or hostile. Canonical-root confinement, symlink/reparse rejection, bounded decoding, fail-closed configuration parsing, and live hash verification limit traversal, confusion, and stale-source use.
- **MCP/model output is untrusted data.** It grants no shell, source, destructive, or filesystem authority. The stdio adapter exposes only its documented operations; local destructive commands remain outside MCP.
- **External providers are operator-installed code.** Atlas direct-spawns configured executables without a shell, filters their environment, bounds output and time, and records identity/evidence. These controls do not sandbox filesystem or network access. A provider or its installer can execute code and may access the network under host policy.
- **The catalogue is local mutable state.** Single-writer leases, atomic activation, checked migrations, route identity, and complete destructive manifests limit concurrent or partial state changes. They do not replace host access control, backup discipline, or disk protection.
- **Build and supply-chain inputs are privileged.** Cargo/npm/provider installation can fetch and execute third-party code. Private CI pins or locks reviewed inputs, uses least privilege, and has no publication authority. No artifact signing, attestation service, or SBOM generator has been selected.
- **Diagnostics can disclose private data.** Reports must omit credentials, raw prompt/source bodies, unnecessary absolute paths, environment values, and caller-owned backups. Share only the minimum bounded JSON fields needed to reproduce a failure.

## Security invariants

- Raw task text and source bodies are not persisted by default; telemetry upload is disabled.
- Exact source for change-mode use comes only from `atlas source` after live content-hash verification. Indexed ranges and Context IR references are evidence, not edit authority.
- Configuration expressions fail closed; built-in secret exclusions include `.env`, `.pem`, and `.key`; registered configuration continuity is hash-bound.
- Optional provider failure remains visible as degraded coverage; required-provider failure blocks candidate activation.
- Context routing is deterministic, bounded, provider/model neutral, and capability-discovered. `DIRECT` may return zero Atlas context; `ATLAS_LIGHT` is explicit-target bounded; currently unavailable `ATLAS_DEEP` must not be inferred from its reserved vocabulary.
- Privacy compaction requires a reviewed complete manifest and literal confirmation. Unregister is separate whole-catalogue loss and confirmed apply currently fails closed before deletion.
- Package membership remains an exact allowlist. `specs/`, private workflows/scripts, raw reports, credentials, orchestration data, and any future Agent Skill stay outside the Cargo package.

Operational details and current limitations are in the [operator guide](../../OPERATIONS.md#current-limitations). Configuration privacy requirements are normative in the [privacy-boundary specification](../roadmap-v15-v20/SPEC-configuration-privacy-boundary.md).

## Vulnerability reporting decision

No contact or supported vulnerability intake channel has been approved. Therefore this draft cannot direct reporters to an email address, issue tracker, response team, embargo process, or response deadline. Do not place credentials, exploit material, private source, or sensitive catalogue content in a public forum.

Before any public release, H8 must select and verify a private intake route, named owner, acknowledgement and disclosure policy, supported versions, severity/triage method, and safe evidence-transfer procedure. Until then, vulnerability reporting readiness is **incomplete**, not silently delegated to a guessed contact.

## Dependency and provider review

For a private candidate review:

1. Build and test with `--locked`; review `Cargo.lock`, provider provenance, and exact package membership.
2. Install optional providers only from the operator-reviewed source and exact version. The pinned TypeScript provider command and schema provenance are recorded in [SCIP provenance](../../schemas/scip/PROVENANCE.md#typescriptjavascript-pilot-provider); Atlas itself never installs it.
3. Treat npm/Cargo installation as network-capable supply-chain execution. Use host network isolation or an approved mirror when required; Atlas's `network_isolation_policy = "best_effort_allowed"` is not a sandbox guarantee.
4. Record provider version, availability, environment class, and failure/degradation without environment values or raw source.
5. Run the no-publish distribution checker and public-hygiene checks. Their success is private readiness evidence, not signing, provenance, SBOM, publication, or release evidence.

## Remaining security gates

H8 still owns supported platforms/channels, public visibility, security/support contacts, signing/provenance/SBOM tools, public disclosure policy, and the final independent security review. No ordinary CI job may receive release credentials or publication authority.
