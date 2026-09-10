# Security policy

## Supported versions

Security fixes are provided for the current `2.0.x` release line. Older release
candidates and capability-milestone labels are not supported release lines.

## Reporting a vulnerability

Report suspected vulnerabilities through
[GitHub private vulnerability reporting](https://github.com/Zerolitter/Workspace-Atlas/security/advisories/new).
Do not open a public issue for a vulnerability.

Include the affected version, operating system, reproduction steps, observed
impact, and the smallest safe diagnostic material needed to reproduce the
problem. Do not submit credentials, private source, raw prompts, complete
catalogues, provider caches, environment dumps, or caller-owned backups. Use a
disposable minimized fixture where possible.

Workspace Atlas is maintained without a response-time or disclosure-time
service-level commitment. Maintainers will use the private advisory to
coordinate validation, remediation, and disclosure.

## Security boundary

Workspace Atlas is local-first, but its catalogue and configured provider
processes operate with the current user's authority. Protect catalogue files and
backups with host access controls, review provider executables and configuration,
and keep credentials outside Atlas configuration. Atlas does not mutate
workspace source; exact-source use remains live-hash verified, and failed
reconciliation cannot partially activate a candidate generation.
