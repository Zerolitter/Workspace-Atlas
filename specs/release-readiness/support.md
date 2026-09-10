# Support and contribution readiness draft

## Current support status

Workspace Atlas is private and pre-release. No public release, supported installation channel, supported platform matrix, support channel, security contact, service-level objective, response-time commitment, maintenance term, or future Agent Skill availability has been approved. This repository-only draft is excluded from the Cargo package and records what a future support process must cover; it is not that process.

Use the [README compatibility table](../../README.md#from-a-task-to-evidence) as the single route, capability, version, interface, and authority source. Use the [operator guide](../../OPERATIONS.md) for executable grammar and recovery procedures. Do not infer support from a reserved contract, configured CI leg, or locally successful experiment.

## Request boundaries

A useful support report separates these categories:

- **Installation/build:** exact source revision, Rust/Cargo version, OS and architecture, locked command, and stderr. Optional provider installation is operator-owned and may require network access.
- **Workspace/catalogue:** command family, workspace identity as a redacted or hashed value, explicit-catalogue use, configuration hash, catalogue schema status, and `status`/`doctor` result.
- **Capability/route:** fresh `governor capabilities` output, requested floor/ceiling and Atlas intent, closed result/error code, and whether the failure occurred at `DIRECT`, `ATLAS_LIGHT`, or the currently unavailable `ATLAS_DEEP` boundary.
- **Provider:** `atlas providers` state, exact provider name/version, required versus optional status, and bounded diagnostic category. Provider executables and their package managers are outside Atlas installation authority.
- **MCP:** client name/version, negotiated MCP protocol, initialize/list/call phase, tool name, bounded JSON-RPC error, and clean-EOF behavior. Model output is untrusted and grants no Atlas authority.
- **Lifecycle/privacy:** session state and closed outcome/error code, retention preview manifest identity, or unregister preview state. Never treat preview as apply success.
- **Capacity:** named runner, revision/toolchain, dataset/generator, file and evidence counts, configuration/provider/Serving/cache state, cold/warm protocol, raw samples, percentile method, peak-memory method, and limitations. A single machine result is not a public SLO.

## Safe diagnostic bundle

Collect only the minimum needed:

1. exact Atlas revision and `rustc -Vv` / `cargo -V` output;
2. OS, architecture, and whether the run was local or hosted CI;
3. the failing command with workspace and catalogue paths replaced consistently;
4. bounded stdout/stderr and the typed Atlas error code;
5. `atlas status`, `atlas doctor`, `atlas providers`, and capability discovery output when relevant;
6. configuration **hash and schema version**, not the registered configuration body or environment values;
7. whether writers were stopped and whether a caller-owned backup exists for a restore/removal case.

Do not attach raw source, prompts, credentials, API keys, tokens, complete environment dumps, private absolute paths, whole catalogues, provider caches, or caller-owned backups by default. Reproduce against a disposable minimized fixture when possible. The [security draft](security.md#vulnerability-reporting-decision) governs sensitive findings; it deliberately names no reporting contact because none is approved.

## Contribution boundary

No public contribution path or supported review channel is approved. A future contribution guide must define ownership, review gates, licensing acknowledgement, tests, compatibility policy, security handling, and whether external contributions are accepted before inviting submissions.

Any authorized private change must still:

- preserve the Truth/Serving/Context authority split and exact-source live verification;
- use capability discovery instead of assuming route or future-skill availability;
- retain locked tests, strict linting, package exclusion, and no-publish gates;
- avoid new public defaults, SLOs, provider promises, signing/SBOM tools, or support commitments without H8;
- keep EXP-DPPM-001 research-only under its [backlog authority](../roadmap-v15-v20/research-backlog.md#exp-dppm-001-demand-paged-project-memory).

## Triage outcomes

A report is actionable only when its revision, environment, command, observed typed result, and expected documented result are distinguishable. Classify missing evidence as `needs-reproduction`, not product success or failure. Classify a configured-but-unexecuted CI leg as configuration evidence only. Classify optional-provider unavailability as degraded coverage unless a required-provider policy made activation fail.

H8 remains the decision point for actual supported channels, contacts, platforms, versions, response policy, publication, and release. Until those decisions are independently verified, this document defines diagnostic and contribution boundaries only.
