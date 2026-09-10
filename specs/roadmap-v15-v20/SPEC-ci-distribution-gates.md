# Spec: `ci-distribution-gates`

## Objective

Create reproducible private quality/distribution-readiness evidence for V1.5/V2.0 without changing repository visibility, publishing a crate/binary, creating a tag/release, or producing a production artifact. Public adoption remains a later human-authorized decision.

## Current-state delta

The base has locked Cargo builds/tests, format/hygiene commands, deterministic benchmark, package allowlist, dual licensing, and strong local core controls. The private adoption audit identified no public acquisition path (expected while private) plus real readiness gaps: no cross-platform/MSRV CI, real MCP client, real provider matrix, upgrade/restore/removal tests, capacity envelope, release provenance/checksums/SBOM design, security/support/contribution process, or end-to-end adopter workflow.

The expected unauthenticated GitHub 404 is not a bug and is not fixed by this module. Visibility/publication are explicit H8 actions outside this initiative.

## Private CI gate

On private branches, verify at minimum:

- Windows, macOS, Linux on declared architectures;
- Rust MSRV `1.94` and current stable;
- `cargo fmt --all -- --check`;
- clippy with warnings denied;
- locked focused/full tests;
- public-hygiene unit/script checks;
- fresh init/reconcile/status/doctor/query/source/temporal/compiler lifecycle smoke;
- current real MCP client initialize/list/call/error smoke;
- shipped configuration parse/privacy/config-continuity tests;
- optional real TypeScript and Rust SCIP provider smoke where supported, with install/version/environment recorded;
- old-catalogue migration, backup/restore, newer/tampered/read-only failure;
- derived projection delete/rebuild and task/metric compaction;
- `cargo package --list`/`cargo package --locked` contents and compile verification;
- benchmark raw results on named runners, not universal SLO claims.

CI must not publish, tag, upload public artifacts, change visibility, or require repository secrets in untrusted jobs. Dependency/download steps use pinned/locked versions and least-privilege credentials.

## Distribution-readiness evidence

Design, but do not execute public release work:

- supported OS/architecture/MSRV matrix;
- install/verify/PATH/uninstall procedures for each proposed channel;
- version matrix: Cargo, catalogue, config, MCP protocol, Atlas capability, Context IR, planner/projection/metric/estimator, providers;
- reproducible artifact inventory;
- SHA-256 checksums, signing/provenance, SBOM, source tag, release notes, and rollback design;
- upgrade/restore/unregister/data-removal lifecycle;
- SECURITY/vulnerability reporting, threat model, dependency scanning, provider network implications;
- contribution/support/troubleshooting/diagnostic path;
- capacity results for small/medium/large repositories: cold/incremental reconcile, ready/fallback query/compiler p50/p95, peak memory, catalogue growth, provider overhead;
- external-maintainer install/agent-connection usability test.

No artifact or URL is called public/release-ready until the full authorized journey is independently executable.

## Package contract

Planning files under `specs/roadmap-v15-v20/` remain outside Cargo `include`; package contents must be unchanged at this checkpoint. Future implementation source/tests/docs enter the package only through the reviewed existing allowlist. Workflows, internal benchmark raw bundles, machine paths, credentials, and orchestration metadata never enter release packages.

## Security/privacy gates

- no raw prompt/source telemetry by default or upload path;
- config patterns fail closed and secret defaults match claims;
- providers remain operator-installed/direct-spawn/bounded; network isolation limitations documented;
- CI logs redact credentials/paths where appropriate;
- release signing credentials, if later authorized, are human/release-environment scoped and unavailable to ordinary PR jobs;
- model/MCP outputs remain untrusted data; no shell/source authority expansion.

## RED–GREEN–REFACTOR acceptance

**RED**

- run current matrix definition and show missing workflows/platform evidence;
- current real MCP client fails lifecycle/schema;
- shipped config privacy and config-continuity failures reproduce;
- old catalogue/restore/removal/package/capacity gates are absent;
- public-hygiene check rejects private planning paths, proving they must be removed or explicitly excluded before a public branch.

**GREEN**

- private CI matrix passes every supported gate with pinned versions/raw evidence;
- package path list and built crate contain only authorized files and no planning/private data;
- no workflow has publication authority;
- capacity/release designs are complete and reviewable;
- H8 checklist clearly separates ready evidence from unauthorized publication actions.

**REFACTOR**

- reuse commands from OPERATIONS and one compatibility matrix;
- keep provider/release jobs opt-in and least privilege;
- avoid duplicating platform scripts when a portable command exists.

## Verification commands

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo run --release --locked --bin atlas-bench -- --manifest tests/fixtures/context_yield/benchmark-scenario.json
python -m unittest scripts/test_check_public_hygiene.py
python scripts/check-public-hygiene.py
cargo package --list --locked
```

The planning checkpoint MUST pass `python scripts/check-public-hygiene.py` without weakening its path, content, or package allowlists. Cargo membership must remain unchanged because `specs/` is outside `include`; this checkpoint uses package listing only and creates no package artifact. A later package-build gate requires explicit authorization and ephemeral output that is neither uploaded nor retained.

## Boundaries

- Always: private least-privilege CI, pinned inputs, raw evidence, package allowlist, secret scanning, no publish authority.
- Ask first: workflow creation, supported matrix, signing/SBOM tooling, public docs, release channel.
- Never: make repository public, push main/protected provider branch, tag, publish crate/binary/artifact, weaken hygiene to admit private plans, expose credentials.

## Success criteria

1. Every implementation module has a focused and matrix gate.
2. Supported platforms and compatibility versions are continuously verified.
3. Package and eventual artifact contents are explicit, reproducible, and private-data-free.
4. Performance/capacity evidence is reproducible and honestly scoped.
5. Publication remains impossible without H8 authorization.

## Open decisions

- **H8:** supported distribution channels/platforms, release signing/provenance/SBOM tools, public visibility/timing, support/security contacts.
- **H3:** benchmark runners and acceptance thresholds.
