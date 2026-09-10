#!/usr/bin/env python3
"""Fail-closed, no-publish distribution-readiness gate for Workspace Atlas."""
from __future__ import annotations

import argparse
import contextlib
import dataclasses
import fnmatch
import gzip
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zlib
from collections.abc import Callable
sys.dont_write_bytecode = True


ROOT = pathlib.Path(__file__).resolve().parents[1]
MAX_OUTPUT = 1_048_576
MAX_ARCHIVE_BYTES = 64 * 1_048_576
MAX_ARCHIVE_EXPANDED_BYTES = 64 * 1_048_576
EXPECTED_WORKFLOW_SHA256 = "3183234a0eac8b4f19d551f2e22813ba10646a4b940e0e8aa655c35b61d253af"
EXPECTED_QUALITY_WORKFLOW_SHA256 = "2128987b46a4583355de8218287fd9396db72009538fd3dfc3cf1f29f0e1fe97"
EXPECTED_GITATTRIBUTES = b"* text=auto\n\n*.rs text eol=lf\n*.toml text eol=lf\n*.md text eol=lf\n*.sql text eol=lf\n*.json text eol=lf\n*.jsonl text eol=lf\n*.yml text eol=lf\n*.yaml text eol=lf\n*.py text eol=lf\n*.ts text eol=lf\n*.tsx text eol=lf\n*.js text eol=lf\n*.jsx text eol=lf\nLICENSE text eol=lf\n\n*.png binary\n*.jpg binary\n*.jpeg binary\n*.gif binary\n*.webp binary\n*.sqlite binary\n*.scip binary\n*.zip binary\n*.7z binary\n"
TIMEOUT_SECONDS = 180
BASELINE_PACKAGE_PATHS = (
    '.cargo_vcs_info.json',
    'Cargo.lock',
    'Cargo.toml',
    'Cargo.toml.orig',
    'CONTRIBUTING.md',
    'LICENSE',
    'LICENSE-APACHE',
    'LICENSE-MIT',
    'OPERATIONS.md',
    'README.md',
    'SECURITY.md',
    'bin/atlas-bench.rs',
    'bin/atlas-mcp.rs',
    'bin/atlas.rs',
    'config/mcp-client-matrix.toml',
    'config/workspace-atlas-v1.1.config.example.toml',
    'docs/CHANGELOG.md',
    'docs/README.md',
    'docs/adr/015-workspace-identity-and-catalogue-routing.md',
    'docs/adr/017-shared-cli-mcp-application-layer.md',
    'docs/adr/020-caller-driven-reconciliation.md',
    'docs/adr/022-dual-license.md',
    'docs/adr/023-context-compiler-contract.md',
    'docs/adr/README.md',
    'docs/adr/legacy-decision-catalogue.md',
    'migrations/0001_foundation.sql',
    'migrations/0002_semantic_provider_foundation.sql',
    'migrations/0003_context_intelligence_foundation.sql',
    'migrations/0004_file_classification_identity.sql',
    'rust-toolchain.toml',
    'schemas/scip/PROVENANCE.md',
    'schemas/scip/scip.proto',
    'src/broker.rs',
    'src/catalogue.rs',
    'src/cli.rs',
    'src/config.rs',
    'src/context_application.rs',
    'src/context_ir.rs',
    'src/context_metrics.rs',
    'src/context_route.rs',
    'src/context_yield.rs',
    'src/discovery.rs',
    'src/error.rs',
    'src/generation.rs',
    'src/generation_delta.rs',
    'src/hashing.rs',
    'src/ids.rs',
    'src/lib.rs',
    'src/lifecycle.rs',
    'src/mcp_adapter.rs',
    'src/migrations.rs',
    'src/paths.rs',
    'src/project_scope.rs',
    'src/provider_contract.rs',
    'src/provider_persistence.rs',
    'src/provider_runtime.rs',
    'src/providers.rs',
    'src/query.rs',
    'src/query_metrics.rs',
    'src/resolution.rs',
    'src/scip_decoder.rs',
    'src/scip_mapping.rs',
    'src/semantic_reconcile.rs',
    'src/serving.rs',
    'src/task_compiler.rs',
    'src/task_session.rs',
    'src/temporal.rs',
    'src/workspace.rs',
    'tests/benchmark_manifest.rs',
    'tests/catalogue_routing.rs',
    'tests/compiler_composition.rs',
    'tests/compiler_lifecycle.rs',
    'tests/compiler_surface_parity.rs',
    'tests/config_continuity.rs',
    'tests/config_privacy.rs',
    'tests/context_metrics.rs',
    'tests/context_observation.rs',
    'tests/context_route_decision.rs',
    'tests/context_seed_resolution.rs',
    'tests/context_temporal_recipes.rs',
    'tests/context_yield_experiments.rs',
    'tests/context_yield_v15.rs',
    'tests/contract_examples/provider-descriptor.example.json',
    'tests/contract_examples/provider-execution-result.example.json',
    'tests/contract_examples/provider-probe-result.example.json',
    'tests/contract_examples/provider-process-request.example.json',
    'tests/contract_examples/semantic-resolution-record.example.json',
    'tests/delta_lease_v20.rs',
    'tests/exact_source_compiler.rs',
    'tests/fixtures/catalogues/v1.4.sqlite',
    'tests/fixtures/config/historical-registered-config-1c9c12.json',
    'tests/fixtures/context_ir/context-budgets.example.json',
    'tests/fixtures/context_ir/context-ir.example.json',
    'tests/fixtures/context_ir/context-use-event.example.json',
    'tests/fixtures/context_ir/generation-delta.example.json',
    'tests/fixtures/context_ir/seed-resolution.example.json',
    'tests/fixtures/context_ir/symbol-card.example.json',
    'tests/fixtures/context_ir/task-session.example.json',
    'tests/fixtures/context_route/decision-v1.example.json',
    'tests/fixtures/context_yield/benchmark-scenario.json',
    'tests/fixtures/context_yield/context-profiles.example.json',
    'tests/fixtures/context_yield/context-yield.example.json',
    'tests/fixtures/pilot/generate_fixture.py',
    'tests/mcp_compiler_parity.rs',
    'tests/mcp_conformance.rs',
    'tests/mcp_sdk_boundary.rs',
    'tests/profile_budgets.rs',
    'tests/profile_manifests.rs',
    'tests/provider_runtime/mock_provider.py',
    'tests/retention_unregister.rs',
    'tests/retrieval_instrumentation.rs',
    'tests/scip_fixtures/oracle.json',
    'tests/scip_fixtures/semantic_fixture_v1.scip',
    'tests/serving_readiness.rs',
    'tests/source_telemetry_surfaces.rs',
    'tests/task_compiler_v13.rs',
    'tests/task_compiler_v20.rs',
    'tests/task_outcome_delta.rs',
    'tests/temporal_intelligence_v14.rs',
)
T23_PACKAGE_DELTA = ("tests/distribution_readiness.rs",)
EXPECTED_PACKAGE_PATHS = tuple(sorted((*BASELINE_PACKAGE_PATHS, *T23_PACKAGE_DELTA)))
EXPECTED_PACKAGE_COUNT = len(EXPECTED_PACKAGE_PATHS)
EXPECTED_ARCHIVE_STEM = "workspace_atlas-2.0.0"
CARGO_GENERATED_PACKAGE_PATHS = frozenset({".cargo_vcs_info.json", "Cargo.toml.orig"})
PINNED_TEXT_AUTO_LF_PATHS = frozenset({
    "Cargo.lock", "LICENSE-APACHE", "LICENSE-MIT", "schemas/scip/scip.proto",
})
EXPECTED_BINS = {
    "atlas": "bin/atlas.rs",
    "atlas-bench": "bin/atlas-bench.rs",
    "atlas-mcp": "bin/atlas-mcp.rs",
}
EXPECTED_INCLUDE = [
    "Cargo.toml", "Cargo.lock", "README.md", "CONTRIBUTING.md", "LICENSE",
    "LICENSE-APACHE", "LICENSE-MIT", "OPERATIONS.md", "SECURITY.md", "docs/**",
    "rust-toolchain.toml", "src/**", "bin/atlas.rs", "bin/atlas-bench.rs",
    "bin/atlas-mcp.rs", "config/**", "migrations/**", "schemas/**", "tests/**",
    "!tests/fixtures/capacity/**",
    "!specs/roadmap-v15-v20/Data-for-analysis.7z",
]
SOURCE_PATHS = (
    "Cargo.toml",
    ".github/workflows/distribution-readiness.yml",
    ".github/workflows/quality.yml",
    "scripts/check-distribution-readiness.py",
    "tests/distribution_readiness.rs",
    "README.md",
    "OPERATIONS.md",
    "config/mcp-client-matrix.toml",
    "config/workspace-atlas-v1.1.config.example.toml",
    "src/config.rs",
    "src/context_route.rs",
    "src/context_ir.rs",
    "src/context_yield.rs",
    "src/mcp_adapter.rs",
    "src/migrations.rs",
    "src/provider_contract.rs",
    "src/providers.rs",
    "src/serving.rs",
    "src/task_compiler.rs",
    "bin/atlas-mcp.rs",
    "bin/atlas-bench.rs",
    "tests/fixtures/config/historical-registered-config-1c9c12.json",
    "tests/fixtures/context_ir/context-ir.example.json",
    "tests/fixtures/context_route/decision-v1.example.json",
    "tests/fixtures/context_yield/benchmark-scenario.json",
    "tests/fixtures/context_yield/context-yield.example.json",
    "tests/contract_examples/provider-descriptor.example.json",
    "tests/contract_examples/provider-execution-result.example.json",
    "tests/contract_examples/provider-probe-result.example.json",
    "tests/contract_examples/provider-process-request.example.json",
    "tests/benchmark_manifest.rs",
    "tests/context_yield_experiments.rs",
)


class GateFailure(RuntimeError):
    def __init__(self, check_id: str, detail: str):
        super().__init__(detail)
        self.check_id = check_id
        self.detail = detail


@dataclasses.dataclass(frozen=True)
class CheckResult:
    check_id: str
    detail: str


@dataclasses.dataclass
class Sources:
    files: dict[str, str]

    @classmethod
    def load(cls, root: pathlib.Path) -> "Sources":
        # Bootstrap boundary: this process can detect index/worktree drift in its
        # own bytes, but cannot authenticate instructions that are already executing.
        # A trusted launcher or fresh CI checkout remains the first-invocation authority.
        modes, oids = stage_zero_entries(root, list(SOURCE_PATHS), "DR-INPUT")
        require(all(mode in {"100644", "100755"} for mode in modes.values()),
                "DR-INPUT", "checked sources must be stage-zero regular files")
        for self_authority in (
            "scripts/check-distribution-readiness.py", "tests/distribution_readiness.rs"
        ):
            require(modes[self_authority] == "100644", "DR-INPUT",
                    f"self-authority source mode must be 100644: {self_authority}")
        policies = indexed_attribute_policies(root, SOURCE_PATHS)
        files: dict[str, str] = {}
        for relative in SOURCE_PATHS:
            indexed = indexed_blob_bytes(root, oids[relative], "DR-INPUT")
            observed = working_tree_file_bytes(root, relative, "DR-INPUT")
            require(package_bytes_equivalent(relative, indexed, observed, policies),
                    "DR-INPUT", f"working tree differs from stage zero: {relative}")
            files[relative] = indexed.decode("utf-8")
        return cls(files)

    def clone(self) -> "Sources":
        return Sources(dict(self.files))


def require(condition: bool, check_id: str, detail: str) -> None:
    if not condition:
        raise GateFailure(check_id, detail)


def constant(text: str, name: str, check_id: str) -> str:
    match = re.search(rf"(?:pub )?const {re.escape(name)}: &str = \"([^\"]+)\";", text)
    require(match is not None, check_id, f"constant {name} is absent")
    return match.group(1)


def rust_tokens(source: str) -> list[str]:
    tokens: list[str] = []
    index = 0
    while index < len(source):
        if source[index].isspace():
            index += 1
            continue
        if source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = len(source) if newline < 0 else newline + 1
            continue
        if source.startswith("/*", index):
            depth = 1
            cursor = index + 2
            while depth:
                opening = source.find("/*", cursor)
                closing = source.find("*/", cursor)
                require(closing >= 0, "DR-COMPATIBILITY", "unterminated Rust block comment")
                if 0 <= opening < closing:
                    depth += 1
                    cursor = opening + 2
                else:
                    depth -= 1
                    cursor = closing + 2
            index = cursor
            continue
        raw = re.match(r'(?:br|rb|r)(#*)"', source[index:])
        if raw is not None:
            delimiter = '"' + raw.group(1)
            end = source.find(delimiter, index + raw.end())
            require(end >= 0, "DR-COMPATIBILITY", "unterminated Rust raw string literal")
            end += len(delimiter)
            tokens.append(source[index:end])
            index = end
            continue
        quote_index = index + 1 if source[index:index + 1] == "b" else index
        quote = source[quote_index:quote_index + 1]
        is_character = quote == "'" and (
            source[quote_index + 1:quote_index + 2] == "\\"
            or source[quote_index + 2:quote_index + 3] == "'"
        )
        if quote == '"' or is_character:
            cursor = quote_index + 1
            escaped = False
            while cursor < len(source):
                character = source[cursor]
                cursor += 1
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == quote:
                    break
            else:
                require(False, "DR-COMPATIBILITY", "unterminated Rust quoted literal")
            tokens.append(source[index:cursor])
            index = cursor
            continue
        identifier = re.match(r"[A-Za-z_][A-Za-z0-9_]*", source[index:])
        if identifier is not None:
            token = identifier.group(0)
            tokens.append(token)
            index += len(token)
            continue
        tokens.append(source[index])
        index += 1
    return tokens


def historical_provider_fallback(config_source: str) -> tuple[str, str]:
    tokens = rust_tokens(config_source)
    method_names = [
        index for index, token in enumerate(tokens)
        if token == "resolved_version" and index > 0 and tokens[index - 1] == "fn"
    ]
    require(len(method_names) == 1, "DR-COMPATIBILITY",
            "ProviderConfigEntry::resolved_version must have one authoritative implementation")
    name_index = method_names[0]
    signature = rust_tokens(
        "pub fn resolved_version(&self) -> Option<String> {"
    )
    start = name_index - 2
    require(start >= 0 and tokens[start:start + len(signature)] == signature,
            "DR-COMPATIBILITY", "ProviderConfigEntry::resolved_version signature drifted")
    opening = start + len(signature) - 1
    depth = 1
    closing = opening + 1
    while closing < len(tokens) and depth:
        if tokens[closing] == "{":
            depth += 1
        elif tokens[closing] == "}":
            depth -= 1
        closing += 1
    require(depth == 0, "DR-COMPATIBILITY",
            "ProviderConfigEntry::resolved_version body is unterminated")
    actual = tokens[opening + 1:closing - 1]
    expected = rust_tokens('''
        self.version
            .clone()
            .or_else(|| (self.name == "scip-typescript").then(|| "0.4.0".to_string()))
        ''')
    require(actual == expected, "DR-COMPATIBILITY",
            "ProviderConfigEntry::resolved_version semantics drifted from the authoritative fallback")
    return "scip-typescript", "0.4.0"


def check_allowlist(sources: Sources) -> CheckResult:
    cargo = tomllib.loads(sources.files["Cargo.toml"])
    package = cargo["package"]
    require(package.get("include") == EXPECTED_INCLUDE, "DR-ALLOWLIST", "Cargo include allowlist drifted")
    bins = {entry["name"]: entry["path"] for entry in cargo.get("bin", [])}
    require(bins == EXPECTED_BINS, "DR-ALLOWLIST", f"executable allowlist drifted: {bins!r}")
    forbidden = ("scripts/", ".github/", "specs/", "target/", "skill", "evidence")
    includes = [value.lower() for value in package["include"] if not value.startswith("!")]
    require(not any(token in value for value in includes for token in forbidden),
            "DR-ALLOWLIST", "private or procedural path entered Cargo include")
    return CheckResult("DR-ALLOWLIST", "3 binaries and 21 Cargo include entries exact")


def check_compatibility(sources: Sources) -> CheckResult:
    cargo = tomllib.loads(sources.files["Cargo.toml"])["package"]
    matrix = tomllib.loads(sources.files["config/mcp-client-matrix.toml"])
    route = sources.files["src/context_route.rs"]
    task = sources.files["src/task_compiler.rs"]
    providers = sources.files["src/providers.rs"]
    readme = sources.files["README.md"]
    core_identities = {
        "cargo": cargo["version"],
        "msrv": cargo["rust-version"],
        "catalogue": constant(sources.files["src/migrations.rs"], "CURRENT_SCHEMA_VERSION", "DR-COMPATIBILITY"),
        "config": constant(sources.files["src/config.rs"], "SCHEMA_VERSION", "DR-COMPATIBILITY"),
        "mcp-modern": constant(sources.files["src/mcp_adapter.rs"], "MODERN_PROTOCOL_DATE", "DR-COMPATIBILITY"),
        "mcp-legacy": constant(sources.files["src/mcp_adapter.rs"], "LEGACY_PROTOCOL_DATE", "DR-COMPATIBILITY"),
        "atlas-capability": constant(sources.files["bin/atlas-mcp.rs"], "ATLAS_CAPABILITY_PROTOCOL_VERSION", "DR-COMPATIBILITY"),
        "capability": constant(route, "CONTEXT_CAPABILITIES_VERSION", "DR-COMPATIBILITY"),
        "route": constant(route, "CONTEXT_ROUTE_POLICY_VERSION", "DR-COMPATIBILITY"),
        "execution": constant(route, "CONTEXT_EXECUTION_VERSION", "DR-COMPATIBILITY"),
        "context-ir": constant(route, "CONTEXT_IR_VERSION", "DR-COMPATIBILITY"),
        "context-ir-authority": constant(sources.files["src/context_ir.rs"], "CONTEXT_SCHEMA_V2_VERSION", "DR-COMPATIBILITY"),
        "planner-v1": constant(task, "PLANNER_POLICY_VERSION", "DR-COMPATIBILITY"),
        "planner-v2": constant(route, "DEEP_PLANNER_VERSION", "DR-COMPATIBILITY"),
        "planner-v2-authority": constant(task, "PLANNER_POLICY_V2_VERSION", "DR-COMPATIBILITY"),
        "projection-v1": constant(sources.files["src/serving.rs"], "PROJECTION_POLICY_VERSION", "DR-COMPATIBILITY"),
        "provider-protocol": constant(sources.files["src/provider_contract.rs"], "PROTOCOL_VERSION", "DR-COMPATIBILITY"),
    }
    expected_core = {
        "cargo": "2.0.0", "msrv": "1.94", "catalogue": "1.3.0", "config": "1.1.0",
        "mcp-modern": "2026-07-28", "mcp-legacy": "2025-11-25", "atlas-capability": "1.3",
        "capability": "context-capabilities-v2.0.0", "route": "context-route-v2.0.0",
        "execution": "context-execution-v2.0.0", "context-ir": "2.0.0",
        "context-ir-authority": "2.0.0", "planner-v1": "planner-v1.1.0",
        "planner-v2": "planner-v2.0.0", "planner-v2-authority": "planner-v2.0.0",
        "projection-v1": "projection-v1.0.0", "provider-protocol": "1.0.0",
    }
    require(core_identities == expected_core, "DR-COMPATIBILITY",
            f"compatibility identities drifted: {core_identities!r}")

    projection = constant(route, "DEEP_PROJECTION_VERSION", "DR-COMPATIBILITY")
    estimator = constant(route, "DEEP_ESTIMATOR_VERSION", "DR-COMPATIBILITY")
    context_ir_example = json.loads(sources.files["tests/fixtures/context_ir/context-ir.example.json"])
    decision_example = json.loads(sources.files["tests/fixtures/context_route/decision-v1.example.json"])
    ir_policy = context_ir_example["deep_v2"]["policy"]
    decision_versions = decision_example["decision"]["request"]["deep_contracts"]
    require(
        ir_policy["projection_policy_version"] == projection
        and decision_versions["projection_version"] == projection,
        "DR-COMPATIBILITY", "projection production and shipped fixture identities disagree",
    )
    require(
        ir_policy["estimator_version"] == estimator
        and decision_versions["estimator_version"] == estimator,
        "DR-COMPATIBILITY", "estimator production and shipped fixture identities disagree",
    )

    yield_schema = constant(
        sources.files["src/context_yield.rs"], "CONTEXT_YIELD_REPORT_SCHEMA_VERSION",
        "DR-COMPATIBILITY",
    )
    yield_example = json.loads(sources.files["tests/fixtures/context_yield/context-yield.example.json"])
    metric = yield_example["content"]["metric_policy_version"]
    require(yield_example["schema_version"] == yield_schema, "DR-COMPATIBILITY",
            "Context Yield production and fixture schema identities disagree")
    require(metric == f"context-yield-v{yield_schema.removesuffix('.0')}", "DR-COMPATIBILITY",
            "metric policy is not bound to the authoritative production report version")

    builtin_pairs = {
        constant(providers, "FILE_METADATA_PROVIDER", "DR-COMPATIBILITY"):
            constant(providers, "FILE_METADATA_VERSION", "DR-COMPATIBILITY"),
        constant(providers, "DOCUMENT_CONFIG_PROVIDER", "DR-COMPATIBILITY"):
            constant(providers, "DOCUMENT_CONFIG_VERSION", "DR-COMPATIBILITY"),
        constant(providers, "TYPESCRIPT_PROVIDER", "DR-COMPATIBILITY"):
            constant(providers, "TYPESCRIPT_VERSION", "DR-COMPATIBILITY"),
        constant(providers, "TEXT_FALLBACK_PROVIDER", "DR-COMPATIBILITY"):
            constant(providers, "TEXT_FALLBACK_VERSION", "DR-COMPATIBILITY"),
    }
    require(len(builtin_pairs) == 4 and all(builtin_pairs.values()), "DR-COMPATIBILITY",
            "built-in provider production identities are incomplete")
    for name_constant, version_constant in (
        ("FILE_METADATA_PROVIDER", "FILE_METADATA_VERSION"),
        ("DOCUMENT_CONFIG_PROVIDER", "DOCUMENT_CONFIG_VERSION"),
        ("TYPESCRIPT_PROVIDER", "TYPESCRIPT_VERSION"),
        ("TEXT_FALLBACK_PROVIDER", "TEXT_FALLBACK_VERSION"),
    ):
        require(re.search(rf"seal\(\s*{name_constant},\s*{version_constant},", providers) is not None,
                "DR-COMPATIBILITY", f"built-in descriptor omits {name_constant}/{version_constant}")

    shipped_config = tomllib.loads(sources.files["config/workspace-atlas-v1.1.config.example.toml"])
    configured = shipped_config["providers"]
    configured_builtins = {entry["name"] for entry in configured if entry["kind"] == "builtin"}
    expected_configured_builtins = {
        constant(providers, "FILE_METADATA_PROVIDER", "DR-COMPATIBILITY"),
        constant(providers, "TYPESCRIPT_PROVIDER", "DR-COMPATIBILITY"),
    }
    require(configured_builtins == expected_configured_builtins, "DR-COMPATIBILITY",
            "shipped built-in provider identities disagree with production")

    external_match = re.search(
        r"pub fn scip_typescript_descriptor\(.*?external_scip_descriptor\(\s*"
        r'"([^"]+)",\s*"([^"]+)",', providers, re.DOTALL,
    )
    require(external_match is not None, "DR-COMPATIBILITY",
            "authoritative external provider identity is absent")
    external_name, external_version = external_match.groups()
    provider_pin = matrix["providers"]["typescript"]
    external_config = next(
        (entry for entry in configured if entry["kind"] == "external_scip"
         and entry["name"] == external_name), None,
    )
    require(external_config is not None and external_config["command"] == external_name,
            "DR-COMPATIBILITY", "shipped external provider config identity disagrees with production")
    require(provider_pin["package"].rsplit("/", 1)[-1] == external_name
            and provider_pin["version"] == external_version,
            "DR-COMPATIBILITY", "external provider package/version disagrees with production")
    package_name = provider_pin["package"]
    package_leaf = package_name.rsplit("/", 1)[-1]
    registry_origin = "https://registry.npmjs.org"
    require(provider_pin["registry"] == f"{registry_origin}/{package_name}/{external_version}",
            "DR-COMPATIBILITY", "external provider registry identity is not package/version-derived")
    require(provider_pin["tarball"] ==
            f"{registry_origin}/{package_name}/-/{package_leaf}-{external_version}.tgz",
            "DR-COMPATIBILITY", "external provider tarball identity is not package/version-derived")

    descriptors = [
        json.loads(sources.files["tests/contract_examples/provider-descriptor.example.json"]),
        json.loads(sources.files["tests/contract_examples/provider-execution-result.example.json"])["provider"],
        json.loads(sources.files["tests/contract_examples/provider-process-request.example.json"])["provider"],
    ]
    require(all(value["name"] == external_name and value["version"] == external_version
                for value in descriptors), "DR-COMPATIBILITY",
            "external provider production and contract example identities disagree")
    probe = json.loads(sources.files["tests/contract_examples/provider-probe-result.example.json"])
    require(probe["provider_name"] == external_name and probe["observed_version"] == external_version,
            "DR-COMPATIBILITY", "external provider probe example identity disagrees with production")

    fallback_name, fallback_version = historical_provider_fallback(sources.files["src/config.rs"])
    require(fallback_name == external_name and fallback_version == external_version,
            "DR-COMPATIBILITY",
            "historical external-provider fallback disagrees with production")
    historical_config = json.loads(
        sources.files["tests/fixtures/config/historical-registered-config-1c9c12.json"]
    )
    historical_matches = [
        entry for entry in historical_config["providers"]
        if entry["name"] == fallback_name and entry["kind"] == "external_scip"
    ]
    require(len(historical_matches) == 1
            and historical_matches[0]["version"] is None
            and historical_matches[0]["command"] == external_name,
            "DR-COMPATIBILITY",
            "historical config does not exercise the authoritative external-provider fallback")

    require(matrix["modern_protocol"] == core_identities["mcp-modern"] and
            matrix["legacy_protocol"] == core_identities["mcp-legacy"],
            "DR-COMPATIBILITY", "MCP client and server protocol identities disagree")
    supported = re.search(r"SUPPORTED_SCHEMA_VERSIONS: &\[&str\] = &\[([^]]+)\]", sources.files["src/config.rs"])
    require(supported is not None and '"1.0.0", "1.1.0"' in supported.group(1),
            "DR-COMPATIBILITY", "config compatibility range drifted")
    for value in (core_identities["capability"], core_identities["route"],
                  core_identities["execution"], core_identities["context-ir"],
                  core_identities["planner-v2"]):
        require(value in readme, "DR-COMPATIBILITY", f"canonical README table omits {value}")
    require(re.search(r"are independent\s+contracts", readme) is not None, "DR-COMPATIBILITY",
            "canonical compatibility source no longer separates identities")
    checked = len(core_identities) + 3 + len(builtin_pairs) + 7
    return CheckResult("DR-COMPATIBILITY", f"{checked} authoritative and shipped identities consistent")

def check_capacity(sources: Sources) -> CheckResult:
    scenario = json.loads(sources.files["tests/fixtures/context_yield/benchmark-scenario.json"])
    require(scenario["schema_version"] == "1.0.0", "DR-CAPACITY", "benchmark schema drifted")
    require(scenario["dataset"] == {
        "generator": "tests/fixtures/pilot/generate_fixture.py",
        "fixture_manifest": "fixture_manifest.json",
        "generation": "fresh_per_run",
        "provider_state": "builtin_deterministic",
    }, "DR-CAPACITY", "benchmark environment semantics drifted")
    require(scenario["procedure"] == {
        "warmups": 5, "warm_samples": 100, "cold_samples": 20,
        "repetitions": 3, "percentile_method": "nearest_rank",
    }, "DR-CAPACITY", "benchmark sample or variance procedure drifted")
    require([profile["name"] for profile in scenario["profiles"]] == ["small", "standard", "audit"],
            "DR-CAPACITY", "capacity profiles drifted")
    acceptance = scenario["acceptance"]
    require(acceptance["candidate_budgets_are_public_slo"] is False and
            acceptance["cold_latency_advisory"] is True and
            acceptance["latency_breach_repetitions"] == 2 and
            acceptance["baseline_repetitions"] == 3 and
            acceptance["relative_regression_fraction"] == 0.15 and
            acceptance["relative_regression_mad_multiplier"] == 2.0,
            "DR-CAPACITY", "private ceiling or non-public-SLO semantics drifted")
    experiments = scenario["experiment_bundle"]["experiments"]
    scale = next((item for item in experiments if item["id"] == "graph-scale-10x"), None)
    require(len(experiments) == 6 and scale is not None and
            scale["control"]["graph_scale"] == 1 and scale["candidate"]["graph_scale"] == 10,
            "DR-CAPACITY", "fixed 10x experiment is absent or changed")
    evidence = sources.files["tests/context_yield_experiments.rs"]
    for token in ("raw_samples", "variance", "SingleEnvironment", "p95_micros", "mad_micros"):
        require(token in evidence, "DR-CAPACITY", f"raw evidence semantics omit {token}")
    benchmark = sources.files["bin/atlas-bench.rs"]
    for token in ("working_tree_clean", "toolchain", "PlatformAttestation", "architecture", "cpu"):
        require(token in benchmark, "DR-CAPACITY", f"run environment attestation omits {token}")
    benchmark = sources.files["bin/atlas-bench.rs"]
    for token in (
        "const CAPACITY_LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;",
        "const CAPACITY_LOG_CHUNK_BYTES: usize = 45 * 1024;",
        "whole_reconstructed_sha256",
        "redact_capacity_log_value",
        '*child = Value::String("<redacted-private-field>".to_string());',
        '*failure = Value::String("<redacted-diagnostic>".to_string());',
        '"observed_platform": observed_platform,',
        "capacity-evidence-log-complete",
        "gzip-1-mtime-0",
        "no supported-limit, SLO, platform, or release claim",
    ):
        require(token in benchmark, "DR-CAPACITY",
                f"bounded capacity log implementation omits {token}")
    return CheckResult(
        "DR-CAPACITY",
        "retained post-v2 qualification contracts remain bounded and non-public-SLO",
    )


def workflow_run_steps(
    workflow: str, expected_jobs: set[str]
) -> list[tuple[str, str, str]]:
    require("\t" not in workflow, "DR-NO-PUBLISH", "workflow tabs are unsupported")
    sections = workflow.split("\njobs:\n")
    require(len(sections) == 2, "DR-NO-PUBLISH", "workflow must have one jobs mapping")
    body = sections[1]
    job_starts = list(re.finditer(r"(?m)^  ([a-z0-9-]+):\s*$", body))
    require(bool(job_starts), "DR-NO-PUBLISH", "workflow defines no jobs")
    job_names = [match.group(1) for match in job_starts]
    require(
        set(job_names) == expected_jobs,
        "DR-NO-PUBLISH",
        "workflow job set is unsupported or incomplete",
    )
    require(len(re.findall(r"(?m)^  \S.*:\s*$", body)) == len(job_starts),
            "DR-NO-PUBLISH", "workflow jobs mapping contains unsupported structure")
    steps: list[tuple[str, str, str]] = []
    for job_index, job_start in enumerate(job_starts):
        job_name = job_start.group(1)
        job_end = job_starts[job_index + 1].start() if job_index + 1 < len(job_starts) else len(body)
        job_block = body[job_start.end():job_end]
        require(re.search(r"(?m)^    (?:if|continue-on-error|uses):|^      (?:exclude|include):", job_block) is None,
                "DR-NO-PUBLISH", f"job {job_name} has a conditional or skip mechanism")
        require(len(re.findall(r"(?m)^    steps:\s*$", job_block)) == 1,
                "DR-NO-PUBLISH", f"job {job_name} must have one steps sequence")
        step_starts = list(re.finditer(r"(?m)^      - name:\s*(\S.*)$", job_block))
        require(bool(step_starts), "DR-NO-PUBLISH", f"job {job_name} defines no named steps")
        require(len(re.findall(r"(?m)^      - ", job_block)) == len(step_starts), "DR-NO-PUBLISH", f"job {job_name} contains an unsupported unnamed step")
        for step_index, step_start in enumerate(step_starts):
            step_name = step_start.group(1).strip()
            step_end = step_starts[step_index + 1].start() if step_index + 1 < len(step_starts) else len(job_block)
            step_block = job_block[step_start.end():step_end]
            fields = re.findall(r"(?m)^        ([a-z0-9-]+):", step_block)
            require(len(fields) == len(set(fields)) and set(fields) <= {"uses", "with", "shell", "run", "env"}, "DR-NO-PUBLISH", f"workflow step has unsupported structure: {job_name}/{step_name}")
            require(re.search(r"(?m)^        (?:if|continue-on-error):", step_block) is None,
                    "DR-NO-PUBLISH", f"required workflow step can be skipped: {job_name}/{step_name}")
            run_match = re.search(r"(?m)^        run:\s*(.*)$", step_block)
            if run_match is None:
                continue
            scalar = run_match.group(1).strip()
            if scalar in {"|", "|-", ">", ">-"}:
                lines: list[str] = []
                for line in step_block[run_match.end():].splitlines():
                    if not line.strip():
                        continue
                    require(line.startswith("          "), "DR-NO-PUBLISH",
                            f"unsupported run scalar indentation: {job_name}/{step_name}")
                    lines.append(line.strip())
                scalar = " ".join(lines)
            require(bool(scalar) and "${{ false }}" not in scalar,
                    "DR-NO-PUBLISH", f"workflow run step is disabled: {job_name}/{step_name}")
            steps.append((job_name, step_name, re.sub(r"\s+", " ", scalar)))
    return steps


def require_workflow_command(steps: list[tuple[str, str, str]], command: str, check_id: str) -> tuple[str, str, str]:
    normalized = re.sub(r"\s+", " ", command).strip()
    matches = [step for step in steps if (step[2] == normalized if normalized == "cargo test --locked" else normalized in step[2])]
    require(len(matches) == 1, check_id,
            f"required command must occur in one enabled job step: {command}")
    return matches[0]


def check_workflow(sources: Sources) -> tuple[CheckResult, CheckResult, CheckResult]:
    workflows = (
        (
            ".github/workflows/distribution-readiness.yml",
            sources.files[".github/workflows/distribution-readiness.yml"],
            {
                "no-publish-contract",
                "accepted-module-evidence",
                "real-client-provider-evidence",
                "package-evidence",
            },
        ),
        (
            ".github/workflows/quality.yml",
            sources.files[".github/workflows/quality.yml"],
            {
                "rust-quality",
                "lifecycle-package-benchmark",
                "real-mcp-clients",
            },
        ),
    )
    parsed_steps: dict[str, list[tuple[str, str, str]]] = {}
    for workflow_path, workflow, expected_jobs in workflows:
        header = workflow.split("\njobs:\n", maxsplit=1)[0]
        trigger = re.search(r"(?ms)^on:\s*\n(.*?)^permissions:\s*$", header)
        require(
            trigger is not None and trigger.group(1) == "  pull_request:\n\n",
            "DR-NO-PUBLISH",
            f"{workflow_path} must have exactly one pull_request trigger",
        )
        permission = re.search(r"(?ms)^permissions:\s*\n(.*?)^concurrency:\s*$", header)
        require(
            permission is not None and permission.group(1) == "  contents: read\n\n",
            "DR-NO-PUBLISH",
            f"{workflow_path} must have exactly contents: read authority",
        )
        require(re.search(r"(?m)^on:\s*$\n\s{2}pull_request:\s*$", workflow) is not None,
                "DR-NO-PUBLISH", f"{workflow_path} must be pull_request-only")
        require(re.search(r"(?m)^permissions:\s*$\n\s{2}contents: read\s*$", workflow) is not None,
                "DR-NO-PUBLISH", f"{workflow_path} permissions must be contents: read")
        require("persist-credentials: false" in workflow, "DR-NO-PUBLISH",
                f"{workflow_path} checkout credentials persist")
        require("${{ secrets." not in workflow and "workflow_dispatch:" not in workflow,
                "DR-NO-PUBLISH",
                f"{workflow_path} can access secrets or be manually dispatched")
        forbidden = (
            r"(?m)^\s*push:\s*$", r"cargo\s+publish", r"gh\s+release", r"git\s+tag",
            r"upload-artifact", r"create-release", r"contents:\s*write",
            r"packages:\s*write",
        )
        require(not any(re.search(pattern, workflow, re.IGNORECASE) for pattern in forbidden),
                "DR-NO-PUBLISH",
                f"{workflow_path} contains publication, upload, tag, or write authority")
        uses = re.findall(r"(?m)^\s*uses:\s*([^\s#]+)", workflow)
        require(bool(uses) and all(re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", item) for item in uses),
                "DR-NO-PUBLISH",
                f"{workflow_path} action references must be immutable: {uses!r}")
        steps = workflow_run_steps(workflow, expected_jobs)
        parsed_steps[workflow_path] = steps
        jobs = workflow.split("\njobs:\n", maxsplit=1)[1]
        starts = list(re.finditer(r"(?m)^  ([a-z0-9-]+):\s*$", jobs))
        for index, start in enumerate(starts):
            end = starts[index + 1].start() if index + 1 < len(starts) else len(jobs)
            block = jobs[start.end():end]
            timeouts = re.findall(r"(?m)^    timeout-minutes:\s*(\d+)\s*$", block)
            require(len(timeouts) == 1 and 0 < int(timeouts[0]) <= 90,
                    "DR-NO-PUBLISH",
                    f"{workflow_path} job {start.group(1)} must have one bounded timeout")
        for token in ("windows-latest", "macos-latest", "ubuntu-latest"):
            require(token in workflow, "DR-NO-PUBLISH",
                    f"{workflow_path} platform matrix omits {token}")
        require(
            "--capacity-shard-index" not in workflow
            and "--capacity-shard-count" not in workflow
            and "--capacity-memory" not in workflow
            and "--capacity-log" not in workflow,
            "DR-CAPACITY",
            f"{workflow_path} must leave the four-shard campaign to post-v2 qualification",
        )
    for workflow_path, package_job, provider_job in (
        (
            ".github/workflows/distribution-readiness.yml",
            "package-evidence",
            "real-client-provider-evidence",
        ),
        (
            ".github/workflows/quality.yml",
            "lifecycle-package-benchmark",
            "lifecycle-package-benchmark",
        ),
    ):
        steps = parsed_steps[workflow_path]
        package_list = require_workflow_command(
            steps, "cargo package --list --locked", "DR-PACKAGE"
        )
        package_build = require_workflow_command(
            steps, "cargo package --locked", "DR-PACKAGE"
        )
        provider = require_workflow_command(
            steps, "python scripts/provider-smoke.py --atlas", "DR-LIFECYCLE"
        )
        require(
            package_list[0] == package_job
            and package_build[0] == package_job
            and provider[0] == provider_job,
            "DR-NO-PUBLISH",
            f"{workflow_path} moved provider or package responsibilities into shard jobs",
        )
    workflow = sources.files[".github/workflows/distribution-readiness.yml"]
    steps = parsed_steps[".github/workflows/distribution-readiness.yml"]
    require('rust: ["1.94", stable]' in workflow, "DR-NO-PUBLISH",
            "distribution workflow MSRV matrix omits Rust 1.94 or stable")
    lifecycle_tokens = (
        "config_privacy", "config_continuity", "mcp_conformance", "mcp_compiler_parity",
        "context_route_decision", "compiler_lifecycle", "compiler_composition",
        "compiler_surface_parity", "context_observation", "context_metrics",
        "context_yield_v15", "context_yield_experiments", "task_compiler_v13",
        "task_compiler_v20", "exact_source_compiler", "context_temporal_recipes",
        "delta_lease_v20", "temporal_intelligence_v14", "serving_readiness",
        "retention_unregister", "benchmark_manifest",
    )
    focused = require_workflow_command(steps, "cargo test --locked --test config_privacy", "DR-LIFECYCLE")
    for token in lifecycle_tokens:
        require(token in focused[2], "DR-LIFECYCLE", f"accepted lifecycle/module suite omitted: {token}")
    for command in (
        "cargo fmt --all -- --check",
        "cargo clippy --all-targets --all-features --locked -- -D warnings",
        "cargo test --locked",
        "python -m unittest scripts/test_check_public_hygiene.py",
        "python scripts/check-public-hygiene.py",
        "python scripts/provider-smoke.py --self-test",
        "python scripts/mcp-client-smoke.py --self-test",
        "python scripts/provider-smoke.py --atlas",
        "python scripts/mcp-client-smoke.py --client",
    ):
        require_workflow_command(steps, command, "DR-LIFECYCLE")
    lifecycle = CheckResult("DR-LIFECYCLE", f"{len(lifecycle_tokens)} accepted module suites and T22 smokes wired")

    for command in (
        "cargo package --list --locked", "cargo package --locked",
        "cargo build --locked --bins",
    ):
        require_workflow_command(steps, command, "DR-PACKAGE")
    require_workflow_command(
        parsed_steps[".github/workflows/quality.yml"],
        "cargo run --release --locked --bin atlas-bench",
        "DR-PACKAGE",
    )
    package = CheckResult(
        "DR-PACKAGE",
        "package list/build/unpack compile and bounded representative benchmark smoke wired",
    )

    checker_step = require_workflow_command(
        steps, "python scripts/check-distribution-readiness.py --no-publish --binary-dir", "DR-USABILITY")
    require("target/debug" in checker_step[2], "DR-USABILITY", "checker does not receive built binaries")
    require_workflow_command(
        steps, "python scripts/check-distribution-readiness.py --no-publish --self-test", "DR-USABILITY")
    usability = CheckResult("DR-USABILITY", "built-binary no-publish dry run wired")
    observed_digest = hashlib.sha256(workflow.encode("utf-8")).hexdigest()
    require(observed_digest == EXPECTED_WORKFLOW_SHA256, "DR-NO-PUBLISH",
            "indexed workflow bytes drifted from the explicitly reviewed authority")
    quality_workflow = sources.files[".github/workflows/quality.yml"]
    observed_quality_digest = hashlib.sha256(quality_workflow.encode("utf-8")).hexdigest()
    require(observed_quality_digest == EXPECTED_QUALITY_WORKFLOW_SHA256,
            "DR-NO-PUBLISH",
            "indexed quality workflow bytes drifted from the explicitly reviewed authority")
    return lifecycle, package, usability


def static_checks(sources: Sources) -> list[CheckResult]:
    results = [check_allowlist(sources), check_compatibility(sources), check_capacity(sources)]
    lifecycle, package, usability = check_workflow(sources)
    results.extend((lifecycle, package, usability))
    results.append(CheckResult("DR-NO-PUBLISH", "pull-request-only least-privilege immutable workflow"))
    return results


def load_hygiene_module(root: pathlib.Path):
    checker = root / "scripts/check-public-hygiene.py"
    spec = importlib.util.spec_from_file_location("atlas_public_hygiene", checker)
    require(spec is not None and spec.loader is not None, "DR-PACKAGE", "public hygiene checker unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module
def load_process_module(root: pathlib.Path):
    checker = root / "scripts/provider-smoke.py"
    spec = importlib.util.spec_from_file_location("atlas_distribution_process", checker)
    require(spec is not None and spec.loader is not None, "DR-USABILITY",
            "shared bounded process runner unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module




def bounded_run(argv: list[str], *, cwd: pathlib.Path, environment: dict[str, str] | None = None,
                timeout: int = TIMEOUT_SECONDS, check_id: str = "DR-USABILITY"):
    require(argv and all(isinstance(value, str) and value and "\0" not in value for value in argv),
            check_id, "invalid process argument")
    runner = load_process_module(ROOT)
    try:
        return runner.bounded_run(
            argv,
            environment=environment if environment is not None else dict(os.environ),
            cwd=cwd,
            timeout=timeout,
            output_limit=MAX_OUTPUT,
        )
    except Exception as error:
        raise GateFailure(check_id, str(error)) from error


def check_package_path_list(paths: list[str]) -> None:
    require(
        all(
            path
            and "\\" not in path
            and not pathlib.PurePosixPath(path).is_absolute()
            and ".." not in pathlib.PurePosixPath(path).parts
            and pathlib.PurePosixPath(path).as_posix() == path
            for path in paths
        ),
        "DR-PACKAGE", "package paths are not normalized relative paths",
    )
    require(len(paths) == len(set(paths)), "DR-PACKAGE",
            "package paths contain duplicates")
    require(paths == sorted(paths), "DR-PACKAGE",
            "package paths are not in canonical sorted order")
    require(len(paths) == EXPECTED_PACKAGE_COUNT, "DR-PACKAGE",
            f"package path count must equal {EXPECTED_PACKAGE_COUNT}, got {len(paths)}")
    require(tuple(paths) == EXPECTED_PACKAGE_PATHS, "DR-PACKAGE",
            f"exact package paths drifted; missing={sorted(set(EXPECTED_PACKAGE_PATHS) - set(paths))!r}, "
            f"extra={sorted(set(paths) - set(EXPECTED_PACKAGE_PATHS))!r}")


def check_package_source_identity(
    paths: list[str],
    filesystem_regular_paths: set[str],
    filesystem_symlink_paths: set[str],
    git_modes: dict[str, str],
) -> None:
    source_paths = set(paths) - CARGO_GENERATED_PACKAGE_PATHS
    observed_filesystem_paths = filesystem_regular_paths | filesystem_symlink_paths
    require(observed_filesystem_paths == source_paths, "DR-PACKAGE",
            f"packaged source filesystem identities incomplete; "
            f"missing={sorted(source_paths - observed_filesystem_paths)!r}, "
            f"extra={sorted(observed_filesystem_paths - source_paths)!r}")
    require(set(git_modes) == source_paths, "DR-PACKAGE",
            f"packaged source Git identities incomplete; "
            f"missing={sorted(source_paths - set(git_modes))!r}, "
            f"extra={sorted(set(git_modes) - source_paths)!r}")
    substitutions = sorted(
        filesystem_symlink_paths
        | (source_paths - filesystem_regular_paths - filesystem_symlink_paths)
        | {path for path, mode in git_modes.items() if mode not in {"100644", "100755"}}
    )
    require(not substitutions, "DR-PACKAGE",
            f"packaged source paths must be tracked regular files, not symlink substitutions: "
            f"{substitutions!r}")


def stage_zero_entries(
    root: pathlib.Path, paths: list[str], check_id: str = "DR-PACKAGE"
) -> tuple[dict[str, str], dict[str, str]]:
    expected = set(paths)
    result = bounded_run(["git", "ls-files", "--stage", "-z", "--", *sorted(expected)],
                         cwd=root, check_id=check_id)
    modes: dict[str, str] = {}
    oids: dict[str, str] = {}
    for entry in result.stdout.split("\0"):
        if not entry:
            continue
        prefix, separator, relative = entry.partition("\t")
        fields = prefix.split()
        normalized = relative.replace("\\", "/")
        require(separator == "\t" and len(fields) == 3 and fields[2] == "0",
                check_id, "Git index entry is malformed or unmerged")
        require(normalized in expected and normalized not in modes,
                check_id, f"unexpected or duplicate Git index entry: {normalized}")
        require(fields[0] in {"100644", "100755"}, check_id,
                f"unsupported Git index mode for {normalized}: {fields[0]}")
        require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", fields[1]) is not None,
                check_id, f"malformed Git object identity: {normalized}")
        modes[normalized] = fields[0]
        oids[normalized] = fields[1]
    require(set(modes) == expected, check_id,
            f"stage-zero source identities incomplete; missing={sorted(expected - set(modes))!r}")
    return modes, oids


def working_tree_file_bytes(root: pathlib.Path, relative: str, check_id: str) -> bytes:
    canonical_root = root.resolve(strict=True)
    candidate = canonical_root
    metadata = None
    for component in pathlib.PurePosixPath(relative).parts:
        candidate /= component
        try:
            metadata = candidate.lstat()
        except OSError as error:
            raise GateFailure(check_id, f"required working-tree source is absent: {relative}") from error
        attributes = getattr(metadata, "st_file_attributes", 0)
        require(not stat.S_ISLNK(metadata.st_mode)
                and not attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
                check_id, f"working-tree source has a link or reparse component: {relative}")
    require(metadata is not None and stat.S_ISREG(metadata.st_mode), check_id,
            f"working-tree source is not a regular file: {relative}")
    resolved = candidate.resolve(strict=True)
    require(resolved != canonical_root and resolved.is_relative_to(canonical_root), check_id,
            f"working-tree source resolves outside repository: {relative}")
    require(metadata.st_size <= MAX_OUTPUT, check_id,
            f"working-tree source exceeds bounded size: {relative}")
    data = candidate.read_bytes()
    require(len(data) <= MAX_OUTPUT, check_id,
            f"working-tree source exceeds bounded size while reading: {relative}")
    return data


def parse_indexed_attributes(content: bytes) -> list[tuple[str, tuple[str, ...]]]:
    require(b"\r" not in content, "DR-INPUT",
            "indexed .gitattributes must use the exact LF-only representation")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateFailure("DR-INPUT", "indexed .gitattributes is not UTF-8") from error
    rules: list[tuple[str, tuple[str, ...]]] = []
    supported = {
        ("text=auto",), ("text", "eol=lf"), ("binary",),
    }
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = tuple(line.split())
        require(len(fields) >= 2 and fields[1:] in supported, "DR-INPUT",
                f"unsupported indexed .gitattributes rule on line {number}")
        pattern = fields[0]
        require(not any(character in pattern for character in "[]?\\"), "DR-INPUT",
                f"unsupported indexed .gitattributes pattern on line {number}")
        rules.append((pattern, fields[1:]))
    require(bool(rules), "DR-INPUT", "indexed .gitattributes defines no supported rules")
    return rules


def indexed_attribute_policies(root: pathlib.Path, paths) -> dict[str, tuple[str, ...]]:
    _, attribute_oids = stage_zero_entries(root, [".gitattributes"], "DR-INPUT")
    content = indexed_blob_bytes(root, attribute_oids[".gitattributes"], "DR-INPUT")
    require(content == EXPECTED_GITATTRIBUTES, "DR-INPUT",
            "indexed .gitattributes drifted from the exact authorized LF-only contract")
    rules = parse_indexed_attributes(content)
    policies: dict[str, tuple[str, ...]] = {}
    for relative in paths:
        matches = [attributes for pattern, attributes in rules
                   if fnmatch.fnmatchcase(relative, pattern)]
        require(bool(matches), "DR-INPUT", f"indexed attributes do not cover source: {relative}")
        selected = matches[-1]
        if relative in PINNED_TEXT_AUTO_LF_PATHS:
            require(selected == ("text=auto",), "DR-INPUT", f"pinned text policy drifted: {relative}")
            selected = ("text=auto", "pinned-eol=lf")
        policies[relative] = selected
    return policies


def package_bytes_equivalent(
    relative: str, indexed: bytes, observed: bytes,
    policies: dict[str, tuple[str, ...]],
) -> bool:
    if observed == indexed:
        return True
    if policies.get(relative) not in {("text", "eol=lf"), ("text=auto", "pinned-eol=lf")}:
        return False
    try:
        indexed.decode("utf-8")
        observed.decode("utf-8")
    except UnicodeDecodeError:
        return False
    if b"\0" in indexed or b"\0" in observed or b"\r" in indexed:
        return False
    without_pairs = observed.replace(b"\r\n", b"")
    if b"\r" in without_pairs or b"\n" in without_pairs:
        return False
    return observed.replace(b"\r\n", b"\n") == indexed


def package_source_snapshot(
    root: pathlib.Path, paths: list[str]
) -> tuple[set[str], set[str], dict[str, str], dict[str, str]]:
    source_paths = sorted(set(paths) - CARGO_GENERATED_PACKAGE_PATHS)
    canonical_root = root.resolve(strict=True)
    regular_paths: set[str] = set()
    symlink_paths: set[str] = set()
    for relative in source_paths:
        candidate = canonical_root
        metadata = None
        for component in pathlib.PurePosixPath(relative).parts:
            candidate /= component
            metadata = candidate.lstat()
            attributes = getattr(metadata, "st_file_attributes", 0)
            if (stat.S_ISLNK(metadata.st_mode)
                    or attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)):
                symlink_paths.add(relative)
                break
        resolved = candidate.resolve(strict=True)
        require(resolved != canonical_root and resolved.is_relative_to(canonical_root), "DR-PACKAGE",
                f"packaged source resolves outside the canonical repository root: {relative}")
        if relative in symlink_paths:
            continue
        require(metadata is not None, "DR-PACKAGE", f"empty packaged source path: {relative}")
        if stat.S_ISREG(metadata.st_mode):
            regular_paths.add(relative)

    git_modes, git_oids = stage_zero_entries(root, source_paths)
    policies = indexed_attribute_policies(root, source_paths)
    dirty = [relative for relative in source_paths
             if not package_bytes_equivalent(
                 relative, indexed_blob_bytes(root, git_oids[relative]),
                 working_tree_file_bytes(root, relative, "DR-PACKAGE"), policies)]
    require(not dirty, "DR-PACKAGE",
            f"packaged working-tree sources differ from stage zero: {dirty!r}")
    return regular_paths, symlink_paths, git_modes, git_oids


def package_source_identity(root: pathlib.Path, paths: list[str]) -> tuple[set[str], set[str], dict[str, str]]:
    regular, symlinks, modes, _ = package_source_snapshot(root, paths)
    return regular, symlinks, modes

def remove_package_directory_link(
    path: pathlib.Path, *, platform_name: str = os.name
) -> None:
    if platform_name == "nt":
        path.rmdir()
    else:
        path.unlink()


def file_identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns


def package_archive_path(root: pathlib.Path) -> tuple[pathlib.Path, str]:
    package = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    if not target.is_absolute():
        target = root / target
    stem = f'{package["name"]}-{package["version"]}'
    require(stem == EXPECTED_ARCHIVE_STEM, "DR-PACKAGE",
            f"Cargo package identity is non-canonical: {stem}")
    return target.resolve(strict=False) / "package" / f"{stem}.crate", stem


@contextlib.contextmanager
def produce_package_archive(root: pathlib.Path, cargo_runner=None):
    archive, stem = package_archive_path(root)
    package_directory = archive.parent
    package_directory.mkdir(parents=True, exist_ok=True)
    directory_metadata = package_directory.lstat()
    require(stat.S_ISDIR(directory_metadata.st_mode)
            and not getattr(directory_metadata, "st_file_attributes", 0)
            & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
            "DR-PACKAGE", "Cargo package output directory is not a stable regular directory")
    if archive.exists() or archive.is_symlink():
        prior = archive.lstat()
        require(not stat.S_ISDIR(prior.st_mode), "DR-PACKAGE",
                f"refusing to remove non-file Cargo package artifact: {archive}")
        archive.unlink()
    (cargo_runner or bounded_run)(
        ["cargo", "package", "--locked", "--allow-dirty", "--no-verify"],
        cwd=root,
        check_id="DR-PACKAGE",
    )
    require(archive.is_file(), "DR-PACKAGE", f"Cargo package archive is absent: {archive}")
    path_metadata = archive.lstat()
    require(stat.S_ISREG(path_metadata.st_mode)
            and not getattr(path_metadata, "st_file_attributes", 0)
            & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
            "DR-PACKAGE", "Cargo package archive is not a regular file")
    handle = archive.open("rb")
    try:
        opened_metadata = os.fstat(handle.fileno())
        require(file_identity(path_metadata) == file_identity(opened_metadata), "DR-PACKAGE",
                "Cargo package archive changed before it could be opened")
        snapshot = handle.read(MAX_ARCHIVE_BYTES + 1)
        require(len(snapshot) <= MAX_ARCHIVE_BYTES, "DR-PACKAGE",
                "Cargo package archive exceeds bounded snapshot size")
        final_open_metadata = os.fstat(handle.fileno())
        require(file_identity(opened_metadata) == file_identity(final_open_metadata), "DR-PACKAGE",
                "Cargo package archive changed while its immutable snapshot was captured")
    finally:
        handle.close()
    completed = False
    try:
        yield io.BytesIO(snapshot), archive, stem + "/"
        completed = True
    finally:
        if completed:
            require(archive.exists(), "DR-PACKAGE",
                    "Cargo package archive disappeared during validation")
            final_path_metadata = archive.lstat()
            require(file_identity(opened_metadata) == file_identity(final_path_metadata), "DR-PACKAGE",
                    "Cargo package archive changed or was replaced during validation")


def bounded_raw_tar_bytes(archive) -> bytes:
    compressed = archive.read(MAX_ARCHIVE_BYTES + 1)
    require(len(compressed) <= MAX_ARCHIVE_BYTES, "DR-PACKAGE",
            "Cargo package archive exceeds bounded compressed-byte size")
    try:
        stream = zlib.decompressobj(wbits=16 + zlib.MAX_WBITS)
        expanded = stream.decompress(compressed, MAX_ARCHIVE_EXPANDED_BYTES + 1)
    except zlib.error as error:
        raise GateFailure("DR-PACKAGE", f"Cargo package archive gzip stream is invalid: {error}") from error
    require(len(expanded) <= MAX_ARCHIVE_EXPANDED_BYTES, "DR-PACKAGE",
            "Cargo package archive exceeds bounded raw-tar byte size")
    require(stream.eof, "DR-PACKAGE",
            "Cargo package archive gzip member is incomplete")
    require(not stream.unconsumed_tail, "DR-PACKAGE",
            "Cargo package archive gzip member exceeds bounded raw-tar byte size")
    require(not stream.unused_data, "DR-PACKAGE",
            "Cargo package archive contains data after its single gzip member")
    return expanded


def tar_string(field: bytes, label: str) -> str:
    end = field.find(b"\0")
    if end < 0:
        encoded = field
    else:
        encoded = field[:end]
        require(not field[end + 1:].strip(b"\0"), "DR-PACKAGE",
                f"Cargo package archive {label} has non-null bytes after its terminator")
    try:
        return encoded.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateFailure("DR-PACKAGE",
                          f"Cargo package archive {label} is not canonical UTF-8") from error


def canonical_tar_string(field: bytes, label: str) -> str:
    end = field.find(b"\0")
    require(end >= 0 and field[end:] == b"\0" * (len(field) - end), "DR-PACKAGE",
            f"Cargo package archive {label} is not canonical null-padded text")
    return tar_string(field, label)


def canonical_tar_octal(field: bytes, digits: int, label: str) -> int:
    require(len(field) == digits + 1 and field[-1:] == b"\0"
            and re.fullmatch(rb"[0-7]+", field[:-1]) is not None,
            "DR-PACKAGE", f"Cargo package archive {label} is not canonical Cargo octal")
    return int(field[:-1], 8)


def validate_cargo_tar_header(header: bytes) -> tuple[str, int, int]:
    name = canonical_tar_string(header[:100], "raw name")
    require(header[100:108] == b"0000644\0", "DR-PACKAGE",
            "Cargo package archive raw mode is non-canonical")
    require(header[108:116] in {b"\0" * 8, b"0000000\0"}
            and header[116:124] in {b"\0" * 8, b"0000000\0"}, "DR-PACKAGE",
            "Cargo package archive raw owner fields are non-canonical")
    size = canonical_tar_octal(header[124:136], 11, "raw member size")
    canonical_tar_octal(header[136:148], 11, "raw modification time")
    checksum = canonical_tar_octal(header[148:156], 7, "header checksum")
    require(header[157:257] == b"\0" * 100, "DR-PACKAGE",
            "Cargo package archive raw link name is non-canonical")
    require(header[257:263] == b"ustar ", "DR-PACKAGE",
            "Cargo package archive raw magic is not canonical Cargo ustar")
    require(header[263:265] == b" \0", "DR-PACKAGE",
            "Cargo package archive raw version is not canonical Cargo ustar")
    require(header[265:329] == b"\0" * 64, "DR-PACKAGE",
            "Cargo package archive raw owner names are non-canonical")
    require(header[329:337] in {b"\0" * 8, b"0000000\0"}
            and header[337:345] in {b"\0" * 8, b"0000000\0"}, "DR-PACKAGE",
            "Cargo package archive raw device fields are non-canonical")
    require(header[345:500] == b"\0" * 155, "DR-PACKAGE",
            "Cargo package archive raw prefix is non-canonical")
    require(header[500:512] == b"\0" * 12, "DR-PACKAGE",
            "Cargo package archive raw header padding is nonzero")
    return name, size, checksum


def raw_tar_paths(raw_tar: bytes, expected_prefix: str) -> list[str]:
    raw_paths: list[str] = []
    offset = 0
    zero_blocks = 0
    while offset < len(raw_tar):
        require(offset + 512 <= len(raw_tar), "DR-PACKAGE",
                "Cargo package archive has a truncated raw header")
        header = raw_tar[offset:offset + 512]
        offset += 512
        if header == b"\0" * 512:
            zero_blocks += 1
            require(zero_blocks <= 2, "DR-PACKAGE",
                    "Cargo package archive contains trailing raw-tar data after its canonical "
                    "two-block end marker")
            continue
        require(zero_blocks == 0, "DR-PACKAGE",
                "Cargo package archive contains data after its end marker")
        name, size, stored_checksum = validate_cargo_tar_header(header)
        computed_checksum = sum(header[:148]) + (8 * ord(" ")) + sum(header[156:])
        require(stored_checksum == computed_checksum, "DR-PACKAGE",
                "Cargo package archive raw header checksum is invalid")
        typeflag = header[156:157]
        require(typeflag not in {b"x", b"g"}, "DR-PACKAGE",
                "Cargo package archive contains PAX extension metadata")
        require(typeflag not in {b"L", b"K"}, "DR-PACKAGE",
                "Cargo package archive contains GNU long-name or long-link metadata")
        require(typeflag != b"S", "DR-PACKAGE",
                "Cargo package archive contains GNU sparse extension metadata")
        require(typeflag == b"0", "DR-PACKAGE",
                "Cargo package archive contains a non-regular raw entry")
        prefix = tar_string(header[345:500], "raw prefix")
        raw_name = f"{prefix}/{name}" if prefix else name
        require("\\" not in raw_name and raw_name.startswith(expected_prefix),
                "DR-PACKAGE", "Cargo package archive raw path spelling or root is non-canonical")
        relative = raw_name[len(expected_prefix):]
        require(relative and pathlib.PurePosixPath(relative).as_posix() == relative,
                "DR-PACKAGE", "Cargo package archive raw member spelling is non-canonical")
        require(size <= MAX_OUTPUT, "DR-PACKAGE",
                f"Cargo package archive raw member exceeds bounded size: {raw_name}")
        padded_size = ((size + 511) // 512) * 512
        require(offset + padded_size <= len(raw_tar), "DR-PACKAGE",
                f"Cargo package archive raw member is truncated: {raw_name}")
        require(raw_tar[offset + size:offset + padded_size] == b"\0" * (padded_size - size),
                "DR-PACKAGE", f"Cargo package archive raw member padding is nonzero: {raw_name}")
        raw_paths.append(raw_name)
        offset += padded_size
    require(zero_blocks >= 2, "DR-PACKAGE",
            "Cargo package archive lacks its canonical two-block end marker")
    return raw_paths


def package_archive_entries(archive, expected_prefix: str) -> tuple[list[str], dict[str, bytes], dict[str, int]]:
    entries: dict[str, bytes] = {}
    modes: dict[str, int] = {}
    raw_tar = bounded_raw_tar_bytes(archive)
    raw_paths = raw_tar_paths(raw_tar, expected_prefix)
    with tarfile.open(fileobj=io.BytesIO(raw_tar), mode="r:") as package:
        members = package.getmembers()
        require(len(members) == len(raw_paths), "DR-PACKAGE",
                "Cargo package archive effective entry count disagrees with raw headers")
        for member, raw_name in zip(members, raw_paths, strict=True):
            require(member.name == raw_name, "DR-PACKAGE",
                    "Cargo package archive effective path disagrees with its raw header path")
            require(member.isfile(), "DR-PACKAGE",
                    "Cargo package archive contains a non-regular entry")
            require(0 <= member.size <= MAX_OUTPUT, "DR-PACKAGE",
                    f"Cargo package archive member exceeds bounded size: {member.name}")
            # The stricter raw-tar byte cap already bounds the sum of member payloads.
            require("\\" not in member.name and member.name.startswith(expected_prefix),
                    "DR-PACKAGE", "Cargo package archive member spelling or root is non-canonical")
            relative = member.name[len(expected_prefix):]
            require(relative and pathlib.PurePosixPath(relative).as_posix() == relative,
                    "DR-PACKAGE", "Cargo package archive member spelling is non-canonical")
            require(relative not in entries, "DR-PACKAGE",
                    f"Cargo package archive contains duplicate entry: {relative}")
            stream = package.extractfile(member)
            require(stream is not None, "DR-PACKAGE",
                    f"Cargo package archive member is unreadable: {relative}")
            data = stream.read(member.size + 1)
            require(len(data) == member.size, "DR-PACKAGE",
                    f"Cargo package archive member length is inconsistent: {relative}")
            entries[relative] = data
            modes[relative] = member.mode
    require(bool(entries), "DR-PACKAGE", "Cargo package archive is empty")
    paths = sorted(entries)
    check_package_path_list(paths)
    return paths, entries, modes


def indexed_blob_bytes(root: pathlib.Path, oid: str, check_id: str = "DR-PACKAGE") -> bytes:
    require(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", oid) is not None,
            check_id, "unsupported Git object identity")
    size_result = bounded_run(
        ["git", "cat-file", "-s", oid], cwd=root, check_id=check_id
    )
    require(size_result.stdout.strip().isdigit()
            and int(size_result.stdout.strip()) <= MAX_OUTPUT,
            check_id, f"indexed Git blob exceeds the bounded size: {oid}")
    try:
        result = subprocess.run(
            ["git", "cat-file", "blob", oid], cwd=root, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=TIMEOUT_SECONDS, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise GateFailure(check_id, f"could not read indexed Git blob {oid}: {error}") from error
    require(result.returncode == 0 and len(result.stdout) <= MAX_OUTPUT,
            check_id, f"indexed Git blob is unreadable or exceeds the bounded size: {oid}")
    return result.stdout


def check_archive_source_identity(
    root: pathlib.Path,
    paths: list[str],
    entries: dict[str, bytes],
    archive_modes: dict[str, int],
    git_modes: dict[str, str],
    git_oids: dict[str, str],
) -> None:
    source_paths = sorted(set(paths) - CARGO_GENERATED_PACKAGE_PATHS)
    require(set(git_oids) == set(source_paths), "DR-PACKAGE",
            "packaged archive Git object identities are incomplete")
    policies = indexed_attribute_policies(root, source_paths)
    mismatches: list[str] = []
    for relative in source_paths:
        archive_relative = "Cargo.toml.orig" if relative == "Cargo.toml" else relative
        require(archive_relative in entries and archive_relative in archive_modes, "DR-PACKAGE",
                f"packaged archive source identity is absent: {archive_relative}")
        expected_mode = 0o755 if git_modes[relative] == "100755" else 0o644
        expected_bytes = indexed_blob_bytes(root, git_oids[relative])
        if (archive_modes[archive_relative] != expected_mode
                or not package_bytes_equivalent(
                    relative, expected_bytes, entries[archive_relative], policies)):
            mismatches.append(relative)
    require(not mismatches, "DR-PACKAGE",
            f"Cargo package archive raw bytes or modes disagree with stage-zero Git blobs: {mismatches!r}")


def check_package_contents(root: pathlib.Path) -> CheckResult:
    expected_paths = list(EXPECTED_PACKAGE_PATHS)
    regular_paths, symlink_paths, git_modes, git_oids = package_source_snapshot(root, expected_paths)
    check_package_source_identity(expected_paths, regular_paths, symlink_paths, git_modes)
    with produce_package_archive(root) as (archive, _archive_path, expected_prefix):
        paths, entries, archive_modes = package_archive_entries(archive, expected_prefix)
        check_archive_source_identity(root, paths, entries, archive_modes, git_modes, git_oids)
    hygiene = load_hygiene_module(root)
    rejected: list[str] = []
    for package_path in paths:
        if (hygiene.forbidden_path_reason(package_path) is not None
                or not hygiene.package_path_allowed(package_path)):
            rejected.append(package_path)
    require(not rejected, "DR-PACKAGE", f"package contains non-allowlisted paths: {rejected!r}")
    lowered = [package_path.lower() for package_path in paths]
    prohibited = ("scripts/", ".github/", "specs/", "target/", "raw-evidence", "attestation",
                  "credential", "orchestration", "agent-skill", "agent_skill")
    require(not any(token in package_path for token in prohibited for package_path in lowered),
            "DR-PACKAGE", "package contains private, machine, raw-evidence, credential, or future-skill path")
    actual_bins = {package_path for package_path in paths if package_path.startswith("bin/")}
    require(actual_bins == set(EXPECTED_BINS.values()), "DR-PACKAGE", f"packaged binaries drifted: {actual_bins!r}")
    return CheckResult(
        "DR-PACKAGE",
        f"exact {len(paths)}-path Cargo archive bound to stage-zero Git blobs "
        f"({len(BASELINE_PACKAGE_PATHS)} baseline + {len(T23_PACKAGE_DELTA)} T23 test)",
    )


def safe_environment(scratch: pathlib.Path) -> dict[str, str]:
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "LANG", "TMP", "TEMP"}
    environment = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    home = scratch / "home"
    temp = scratch / "temp"
    app = scratch / "app"
    for path in (home, temp, app):
        path.mkdir(parents=True, exist_ok=True)
    environment.update({
        "HOME": str(home), "USERPROFILE": str(home), "TMP": str(temp), "TEMP": str(temp),
        "LOCALAPPDATA": str(app), "XDG_DATA_HOME": str(app), "PYTHONDONTWRITEBYTECODE": "1",
    })
    return environment


def executable(binary_dir: pathlib.Path, name: str) -> pathlib.Path:
    suffix = ".exe" if os.name == "nt" else ""
    path = binary_dir / f"{name}{suffix}"
    require(path.is_file() and path.stat().st_size > 0, "DR-USABILITY", f"built binary absent: {path.name}")
    return path.resolve()


def json_command(atlas: pathlib.Path, arguments: list[str], cwd: pathlib.Path,
                 environment: dict[str, str]) -> dict[str, object]:
    result = bounded_run([str(atlas), *arguments], cwd=cwd, environment=environment)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise GateFailure("DR-USABILITY", f"command did not return JSON: {' '.join(arguments[:2])}") from error
    require(isinstance(value, dict) and "error" not in value, "DR-USABILITY", "command returned an error envelope")
    return value


def dry_run(root: pathlib.Path, binary_dir: pathlib.Path) -> CheckResult:
    atlas = executable(binary_dir, "atlas")
    atlas_mcp = executable(binary_dir, "atlas-mcp")
    executable(binary_dir, "atlas-bench")
    with tempfile.TemporaryDirectory(prefix="atlas-distribution-readiness-") as raw:
        scratch = pathlib.Path(raw)
        environment = safe_environment(scratch)
        for flag, expected in (("--help", "Workspace Atlas CLI"), ("--version", "2.0.0")):
            result = bounded_run([str(atlas), flag], cwd=scratch, environment=environment)
            require(result.returncode == 0 and expected in result.stdout,
                    "DR-USABILITY", f"atlas {flag} contract failed")
        mcp_exit = bounded_run([str(atlas_mcp)], cwd=scratch, environment=environment)
        require(mcp_exit.returncode == 0, "DR-USABILITY", "atlas-mcp did not close cleanly on EOF")
        workspace = scratch / "workspace"
        (workspace / "src").mkdir(parents=True)
        (workspace / "src/lib.rs").write_text("pub fn distribution_probe() -> u32 { 23 }\n", encoding="utf-8")
        catalogue = scratch / "catalogue.sqlite"
        json_command(atlas, ["init", str(workspace), "--catalogue", str(catalogue)], scratch, environment)
        reconciled = json_command(atlas, ["reconcile", str(workspace), "--catalogue", str(catalogue)], scratch, environment)
        status = json_command(atlas, ["status", str(workspace), "--catalogue", str(catalogue)], scratch, environment)
        doctor = json_command(atlas, ["doctor", str(workspace), "--catalogue", str(catalogue)], scratch, environment)
        found = json_command(atlas, ["find", str(workspace), "distribution_probe", "--catalogue", str(catalogue)], scratch, environment)
        capabilities = json_command(atlas, ["governor", "capabilities", str(workspace), "--catalogue", str(catalogue)], scratch, environment)
        require(reconciled.get("candidate_generation_id") == status.get("active_generation_id"),
                "DR-USABILITY", "status did not observe reconciled generation")
        require(doctor.get("ok") is True and doctor.get("integrity_ok") is True,
                "DR-USABILITY", "doctor did not report a healthy catalogue")
        require("distribution_probe" in json.dumps(found), "DR-USABILITY", "bounded find flow missed source symbol")
        require(capabilities.get("schema_version") == "context-capabilities-v2.0.0",
                "DR-USABILITY", "capability discovery identity drifted")
        request = {
            "supported_versions": {
                "capability_versions": [capabilities["schema_version"]],
                "route_versions": capabilities["supported_route_policy_versions"],
                "decision_versions": capabilities["supported_decision_versions"],
                "execution_versions": capabilities["supported_execution_versions"],
                "ir_versions": capabilities["supported_ir_versions"],
                "operation_versions": capabilities["supported_operation_versions"],
            },
            "semantic": {
                "task": "verify the disposable distribution flow",
                "declared_kind": None,
                "path_targets": [],
                "symbol_targets": [],
                "caller_capabilities": {},
                "atlas_intent": "none",
                "route_floor": "DIRECT",
                "route_ceiling": "DIRECT",
                "legacy_task_session_id": None,
                "deep_limits": None,
            },
        }
        request_path = scratch / "governor-request.json"
        request_path.write_text(json.dumps(request, sort_keys=True), encoding="utf-8")
        governed = json_command(
            atlas,
            ["governor", "run", str(workspace), "--request", str(request_path),
             "--catalogue", str(catalogue)],
            scratch,
            environment,
        )
        require(governed.get("negotiated_versions", {}).get("capability_version") ==
                capabilities["schema_version"], "DR-USABILITY",
                "governor run did not negotiate the discovered capability identity")
        execution = governed.get("execution", {})
        require(execution.get("state") == "completed" and
                execution.get("final_route") == "DIRECT" and
                execution.get("payload", {}).get("payload_type") == "direct_none" and
                execution.get("counters", {}).get("atlas_calls") == 0,
                "DR-USABILITY", "DIRECT governor flow did not remain zero-context")
    require(not scratch.exists(), "DR-USABILITY", "disposable dry-run scratch was not removed")
    return CheckResult("DR-USABILITY", "help/version/MCP EOF/status/doctor/find/discovery/DIRECT flow passed; scratch removed")


def write_expansion_bomb(path: pathlib.Path, prefix: str) -> None:
    data = b"x" * 600_000
    with tarfile.open(path, "w:gz") as archive:
        for relative in EXPECTED_PACKAGE_PATHS:
            member = tarfile.TarInfo(prefix + relative)
            member.size = len(data)
            member.mode = 0o644
            archive.addfile(member, io.BytesIO(data))



def write_gzip_test_archive(path: pathlib.Path, raw_tar: bytes) -> None:
    path.write_bytes(gzip.compress(raw_tar, mtime=0))


def raw_header_with_checksum(header: bytearray) -> bytes:
    header[148:156] = b"        "
    header[148:156] = f"{sum(header):07o}\0".encode("ascii")
    return bytes(header)


def mutate_first_raw_header(raw_tar: bytes, start: int, end: int, value: bytes) -> bytes:
    require(len(value) == end - start, "DR-NO-PUBLISH", "test header mutation has wrong width")
    header = bytearray(raw_tar[:512])
    header[start:end] = value
    return raw_header_with_checksum(header) + raw_tar[512:]


def mutate_first_raw_name(raw_tar: bytes, transform: Callable[[str], str]) -> bytes:
    header = bytearray(raw_tar[:512])
    name = canonical_tar_string(header[:100], "test raw name")
    encoded = transform(name).encode("utf-8")
    require(len(encoded) < 100, "DR-NO-PUBLISH", "test raw name mutation is too long")
    header[:100] = encoded + b"\0" * (100 - len(encoded))
    return raw_header_with_checksum(header) + raw_tar[512:]


def mutate_first_member_padding(raw_tar: bytes) -> bytes:
    size = canonical_tar_octal(raw_tar[124:136], 11, "test raw member size")
    padded_size = ((size + 511) // 512) * 512
    require(padded_size > size, "DR-NO-PUBLISH", "test member has no padding to mutate")
    mutated = bytearray(raw_tar)
    mutated[512 + size] = 1
    return bytes(mutated)


def pax_record(key: str, value: str) -> bytes:
    record = f" {key}={value}\n"
    length = len(record)
    while True:
        candidate = f"{length}{record}".encode("utf-8")
        if len(candidate) == length:
            return candidate
        length = len(candidate)


def insert_metadata_header(raw_tar: bytes, kind: str) -> bytes:
    payloads = {
        "pax-size": pax_record("size", str(MAX_OUTPUT + 1)),
        "pax-path": pax_record("path", "arbitrary-9.9.9/README.md"),
        "pax-global": pax_record("comment", "global metadata"),
        "pax-sparse": pax_record("GNU.sparse.map", "0,0") + pax_record("GNU.sparse.size", "0"),
        "gnu-longname": b"workspace_atlas-2.0.0/" + b"n" * 160 + b"\0",
        "gnu-longlink": b"l" * 160 + b"\0",
        "gnu-sparse": b"",
    }
    typeflags = {
        "pax-size": b"x", "pax-path": b"x", "pax-global": b"g", "pax-sparse": b"x",
        "gnu-longname": b"L", "gnu-longlink": b"K", "gnu-sparse": b"S",
    }
    payload = payloads[kind]
    header = bytearray(raw_tar[:512])
    name = f"workspace_atlas-2.0.0/.metadata-{kind}".encode("ascii")
    header[:100] = name + b"\0" * (100 - len(name))
    header[124:136] = f"{len(payload):011o}\0".encode("ascii")
    header[156:157] = typeflags[kind]
    header = raw_header_with_checksum(header)
    padding = b"\0" * ((-len(payload)) % 512)
    return header + payload + padding + raw_tar


def expect_archive_failure(path: pathlib.Path, mutation_name: str, expected_detail: str) -> str:
    try:
        with path.open("rb") as handle:
            package_archive_entries(handle, "workspace_atlas-2.0.0/")
    except GateFailure as error:
        require(error.check_id == "DR-PACKAGE" and error.detail == expected_detail,
                "DR-NO-PUBLISH",
                f"mutation {mutation_name} rejected at wrong stage: {error.check_id} {error.detail!r}")
        return f"MUTATION {mutation_name} rejected [DR-PACKAGE]"
    raise GateFailure("DR-NO-PUBLISH", f"mutation {mutation_name} was accepted")


def run_self_test(sources: Sources) -> list[str]:
    static_checks(sources)
    resolved_method = '''    pub fn resolved_version(&self) -> Option<String> {
        self.version
            .clone()
            .or_else(|| (self.name == "scip-typescript").then(|| "0.4.0".to_string()))
    }
'''
    mutations: list[tuple[str, str, Callable[[Sources], None]]] = [
        ("allowlist-extra-binary", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"] + '\n[[bin]]\nname="extra"\npath="bin/extra.rs"\n')),
        ("compatibility-route-version", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/context_route.rs", value.files["src/context_route.rs"].replace("context-route-v2.0.0", "context-route-v9.0.0"))),
        ("compatibility-context-ir-authority", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/context_ir.rs", value.files["src/context_ir.rs"].replace('CONTEXT_SCHEMA_V2_VERSION: &str = "2.0.0"', 'CONTEXT_SCHEMA_V2_VERSION: &str = "9.0.0"'))),
        ("compatibility-planner-authority", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/task_compiler.rs", value.files["src/task_compiler.rs"].replace('PLANNER_POLICY_V2_VERSION: &str = "planner-v2.0.0"', 'PLANNER_POLICY_V2_VERSION: &str = "planner-v9.0.0"'))),
        ("workflow-all-jobs-disabled", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    runs-on:", "    if: ${{ false }}\n    runs-on:"))),
        ("workflow-required-step-disabled", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("      - name: Checker mutation tests\n", "      - name: Checker mutation tests\n        if: ${{ false }}\n", 1))),
        ("compatibility-projection-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("tests/fixtures/context_route/decision-v1.example.json", value.files["tests/fixtures/context_route/decision-v1.example.json"].replace('"projection_version": "projection-v2.0.0"', '"projection_version": "projection-v9.0.0"'))),
        ("compatibility-estimator-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("tests/fixtures/context_ir/context-ir.example.json", value.files["tests/fixtures/context_ir/context-ir.example.json"].replace('"estimator_version": "context-estimator-v1.0.0"', '"estimator_version": "context-estimator-v9.0.0"'))),
        ("compatibility-metric-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("tests/fixtures/context_yield/context-yield.example.json", value.files["tests/fixtures/context_yield/context-yield.example.json"].replace('"metric_policy_version": "context-yield-v1.5"', '"metric_policy_version": "context-yield-v9.0"'))),
        ("compatibility-builtin-provider-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("config/workspace-atlas-v1.1.config.example.toml", value.files["config/workspace-atlas-v1.1.config.example.toml"].replace('name = "file_metadata"', 'name = "file_metadata_drift"', 1))),
        ("compatibility-external-provider-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("config/mcp-client-matrix.toml", value.files["config/mcp-client-matrix.toml"].replace('scip-typescript-0.4.0.tgz', 'scip-typescript-9.0.0.tgz'))),
        ("compatibility-external-provider-fallback", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/config.rs", value.files["src/config.rs"].replace('(self.name == "scip-typescript").then(|| "0.4.0".to_string())', '(self.name == "scip-typescript").then(|| "9.0.0".to_string())'))),
        ("compatibility-external-provider-early-return", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/config.rs", value.files["src/config.rs"].replace(
             "    pub fn resolved_version(&self) -> Option<String> {\n        self.version",
             "    pub fn resolved_version(&self) -> Option<String> {\n"
             "        if self.version.is_none() && self.name == \"scip-typescript\" {\n"
             "            return Some(\"9.0.0\".to_string());\n"
             "        }\n"
             "        self.version",
             1))),
        ("compatibility-external-provider-name-literal-spacing", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/config.rs", value.files["src/config.rs"].replace(
             '"scip-typescript").then', '"scip- typescript").then', 1))),
        ("compatibility-external-provider-version-literal-spacing", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/config.rs", value.files["src/config.rs"].replace(
             '"0.4.0".to_string()', '"0. 4.0".to_string()', 1))),
        ("compatibility-external-provider-method-duplicate", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__("src/config.rs", value.files["src/config.rs"].replace(
             resolved_method, resolved_method + "\n" + resolved_method, 1))),
        ("compatibility-external-provider-method-decoy", "DR-COMPATIBILITY",
         lambda value: value.files.__setitem__(
             "src/config.rs",
             value.files["src/config.rs"].replace(
                 '"0.4.0".to_string()', '"9.0.0".to_string()', 1
             ) + "\n/* decoy only:\n" + resolved_method + "*/\n"
         )),
        ("capacity-public-slo", "DR-CAPACITY",
         lambda value: value.files.__setitem__("tests/fixtures/context_yield/benchmark-scenario.json", value.files["tests/fixtures/context_yield/benchmark-scenario.json"].replace('"candidate_budgets_are_public_slo": false', '"candidate_budgets_are_public_slo": true'))),
        ("capacity-log-bound-drift", "DR-CAPACITY",
         lambda value: value.files.__setitem__("bin/atlas-bench.rs", value.files["bin/atlas-bench.rs"].replace("const CAPACITY_LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;", "const CAPACITY_LOG_MAX_BYTES: u64 = 0;", 1))),
        ("capacity-log-redaction-marker-removed", "DR-CAPACITY",
         lambda value: value.files.__setitem__("bin/atlas-bench.rs", value.files["bin/atlas-bench.rs"].replace("<redacted-private-field>", "<private-field>"))),
        ("capacity-log-platform-header-removed", "DR-CAPACITY",
         lambda value: value.files.__setitem__("bin/atlas-bench.rs", value.files["bin/atlas-bench.rs"].replace('"observed_platform": observed_platform,', '"omitted_platform": observed_platform,', 1))),
        ("post-v2-capacity-shard-restored", "DR-CAPACITY",
         lambda value: value.files.__setitem__(
             ".github/workflows/quality.yml",
             value.files[".github/workflows/quality.yml"]
             .replace(
                 "cargo run --release --locked --bin atlas-bench",
                 "cargo run --release --locked --bin atlas-bench -- --capacity-shard-index 1",
                 1,
             ),
         )),
        ("representative-benchmark-removed", "DR-PACKAGE",
         lambda value: value.files.__setitem__(
             ".github/workflows/quality.yml",
             value.files[".github/workflows/quality.yml"].replace(
                 "cargo run --release --locked --bin atlas-bench",
                 "cargo run --release --locked --bin atlas-disabled",
                 1,
             ),
         )),
        ("quality-workflow-extra-trigger", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(
             ".github/workflows/quality.yml",
             value.files[".github/workflows/quality.yml"].replace(
                 "  pull_request:\n", "  pull_request:\n  issues:\n", 1
             ),
         )),
        ("lifecycle-suite-removed", "DR-LIFECYCLE",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("retention_unregister", "retention_gate_removed"))),
        ("package-private-path", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace('"tests/**",', '"tests/**",\n    "scripts/**",'))),
        ("package-capacity-exclusion-removed", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace('    "!tests/fixtures/capacity/**",\n', "", 1))),
        ("package-capacity-exclusion-broadened", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace("!tests/fixtures/capacity/**", "!tests/fixtures/**", 1))),
        ("package-capacity-exclusion-reordered", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace('    "tests/**",\n    "!tests/fixtures/capacity/**",', '    "!tests/fixtures/capacity/**",\n    "tests/**",', 1))),
        ("package-capacity-exclusion-spelling-drift", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace("!tests/fixtures/capacity/**", "!tests/fixtures/capacities/**", 1))),
        ("package-capacity-exclusion-substituted", "DR-ALLOWLIST",
         lambda value: value.files.__setitem__("Cargo.toml", value.files["Cargo.toml"].replace("!tests/fixtures/capacity/**", "!tests/fixtures/capacity/*.json", 1))),
        ("usability-capability-step-removed", "DR-USABILITY",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace(" --binary-dir", " --checker-binary-dir"))),
        ("workflow-timeout-removed", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    timeout-minutes: 90\n", "", 1))),
        ("workflow-extra-trigger", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("  pull_request:\n", "  pull_request:\n  issues:\n", 1))),
        ("workflow-write-permission", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("  contents: read\n", "  contents: read\n  actions: write\n", 1))),
        ("workflow-shell-override", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("      - name: Checker mutation tests\n", "      - name: Checker mutation tests\n        shell: 'echo {0}'\n", 1))),
        ("workflow-null-shell", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("      - name: Checker mutation tests\n", "      - name: Checker mutation tests\n        shell: null\n", 1))),
        ("workflow-comment-only-matrix", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("os: [windows-latest, macos-latest, ubuntu-latest]", "os: [ubuntu-latest] # windows-latest macos-latest"))),
        ("workflow-nonexistent-needs", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    runs-on: ${{ matrix.os }}\n", "    needs: nonexistent-job\n    runs-on: ${{ matrix.os }}\n", 1))),
        ("workflow-duplicate-key", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    timeout-minutes: 90\n", "    runs-on: ubuntu-latest\n    timeout-minutes: 90\n", 1))),
        ("workflow-anchor", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    strategy:\n", "    x-review-anchor: &review-anchor true\n    strategy:\n", 1))),
        ("workflow-alias", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    timeout-minutes: 90\n", "    timeout-minutes: *review-anchor\n", 1))),
        ("workflow-unknown-field", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("    strategy:\n", "    unknown-field: true\n    strategy:\n", 1))),
        ("workflow-yaml-null", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("      fail-fast: false\n", "      fail-fast: null\n", 1))),
        ("workflow-uses-run-conflict", "DR-NO-PUBLISH",
         lambda value: value.files.__setitem__(".github/workflows/distribution-readiness.yml", value.files[".github/workflows/distribution-readiness.yml"].replace("        uses: actions/checkout@", "        run: echo bypass\n        uses: actions/checkout@", 1))),
    ]
    output: list[str] = []
    for name, expected, mutate in mutations:
        candidate = sources.clone()
        mutate(candidate)
        try:
            static_checks(candidate)
        except GateFailure as error:
            require(error.check_id == expected, "DR-NO-PUBLISH",
                    f"mutation {name} failed under {error.check_id}, expected {expected}")
            output.append(f"MUTATION {name} rejected [{error.check_id}]")
        else:
            raise GateFailure("DR-NO-PUBLISH", f"mutation {name} was accepted")
    formatted = sources.files["src/config.rs"].replace(
        resolved_method,
        '''    pub fn resolved_version ( & self ) -> Option < String > {
        self /* retained receiver */ . version
            . clone ( ) // formatting-only line comment
            . or_else ( || ( self . name == "scip-typescript" )
                . then ( || "0.4.0" . to_string ( ) ) )
    }
''', 1)
    require(historical_provider_fallback(formatted) == ("scip-typescript", "0.4.0"),
            "DR-NO-PUBLISH", "formatting/comment control was rejected")
    output.append("CONTROL compatibility-external-provider-formatting accepted")
    static_checks(sources.clone())
    output.append("CONTROL workflow-enabled-required-steps accepted")
    swapped_paths = list(EXPECTED_PACKAGE_PATHS)
    swapped_paths.remove("README.md")
    swapped_paths.append("src/otherwise-cargo-allowed.rs")
    swapped_paths.sort()
    try:
        check_package_path_list(swapped_paths)
    except GateFailure as error:
        require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                f"mutation package-same-count-substitution failed under {error.check_id}")
        output.append("MUTATION package-same-count-substitution rejected [DR-PACKAGE]")
    else:
        raise GateFailure("DR-NO-PUBLISH", "mutation package-same-count-substitution was accepted")

    source_paths = set(EXPECTED_PACKAGE_PATHS) - CARGO_GENERATED_PACKAGE_PATHS
    regular_paths = source_paths - {"README.md"}
    symlink_paths = {"README.md"}
    git_modes = {path: "100644" for path in source_paths}
    git_modes["README.md"] = "120000"
    try:
        check_package_source_identity(
            list(EXPECTED_PACKAGE_PATHS), regular_paths, symlink_paths, git_modes
        )
    except GateFailure as error:
        require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                f"mutation package-symlink-substitution failed under {error.check_id}")
        output.append("MUTATION package-symlink-substitution rejected [DR-PACKAGE]")
    else:
        raise GateFailure("DR-NO-PUBLISH", "mutation package-symlink-substitution was accepted")
    class PackageDirectoryLinkCleanupProbe:
        def __init__(self) -> None:
            self.operations: list[str] = []

        def rmdir(self) -> None:
            self.operations.append("rmdir")

        def unlink(self) -> None:
            self.operations.append("unlink")

    for platform_name, expected_operation in (("posix", "unlink"), ("nt", "rmdir")):
        probe = PackageDirectoryLinkCleanupProbe()
        remove_package_directory_link(probe, platform_name=platform_name)
        require(probe.operations == [expected_operation], "DR-NO-PUBLISH",
                f"{platform_name} package link cleanup used {probe.operations!r}")
    output.append("CONTROL package-directory-link-cleanup-operations accepted")

    link_root = pathlib.Path(tempfile.mkdtemp(prefix="atlas-package-link-"))
    junction = link_root / "repo" / "pkg"
    outside = link_root / "outside"
    directory_link_created = False
    try:
        repo = link_root / "repo"
        junction.mkdir(parents=True)
        outside.mkdir()
        (junction / "source.rs").write_text("inside\n", encoding="utf-8")
        bounded_run(["git", "init", "-q"], cwd=repo, check_id="DR-PACKAGE")
        bounded_run(["git", "add", "pkg/source.rs"], cwd=repo, check_id="DR-PACKAGE")
        (junction / "source.rs").unlink()
        junction.rmdir()
        (outside / "source.rs").write_text("outside\n", encoding="utf-8")
        if os.name == "nt":
            bounded_run(["cmd", "/c", "mklink", "/J", str(junction), str(outside)],
                        cwd=repo, check_id="DR-PACKAGE")
        else:
            os.symlink(outside, junction, target_is_directory=True)
        directory_link_created = True
        try:
            package_source_identity(repo, ["pkg/source.rs"])
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    f"mutation package-ancestor-link-substitution failed under {error.check_id}")
            output.append("MUTATION package-ancestor-link-substitution rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation package-ancestor-link-substitution was accepted")
    finally:
        def remove_readonly(function, target, _error):
            os.chmod(target, stat.S_IWRITE)
            function(target)
        try:
            if junction.exists() or junction.is_symlink():
                if directory_link_created:
                    remove_package_directory_link(junction)
                else:
                    junction.rmdir()
            if directory_link_created:
                outside_source = outside / "source.rs"
                require(not junction.exists() and not junction.is_symlink(), "DR-NO-PUBLISH",
                        "package directory link entry survived cleanup")
                require(outside_source.is_file()
                        and outside_source.read_text(encoding="utf-8") == "outside\n",
                        "DR-NO-PUBLISH", "package directory link cleanup changed outside source")
                output.append("CONTROL package-directory-link-cleanup-contained accepted")
        finally:
            shutil.rmtree(link_root, onexc=remove_readonly)

    archive_root = pathlib.Path(tempfile.mkdtemp(prefix="atlas-package-archive-"))
    try:
        (archive_root / "pkg").mkdir()
        (archive_root / "pkg" / "source.rs").write_text("stage zero\n", encoding="utf-8")
        (archive_root / "pkg" / "source.raw").write_bytes(b"stage zero\n")
        (archive_root / "Cargo.lock").write_bytes(
            b"version = 3\n\n[[package]]\n"
        )
        (archive_root / ".gitattributes").write_bytes(EXPECTED_GITATTRIBUTES)
        bounded_run(["git", "init", "-q"], cwd=archive_root, check_id="DR-PACKAGE")
        bounded_run([
            "git", "add", ".gitattributes", "Cargo.lock", "pkg/source.rs", "pkg/source.raw",
        ], cwd=archive_root, check_id="DR-PACKAGE")
        regular, symlinks, modes, oids = package_source_snapshot(
            archive_root, ["pkg/source.rs"]
        )
        check_package_source_identity(["pkg/source.rs"], regular, symlinks, modes)
        try:
            check_archive_source_identity(
                archive_root,
                ["pkg/source.rs"],
                {"pkg/source.rs": b"substituted\n"},
                {"pkg/source.rs": 0o644},
                modes,
                oids,
            )
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    f"mutation package-archive-content-substitution failed under {error.check_id}")
            output.append("MUTATION package-archive-content-substitution rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation package-archive-content-substitution was accepted")

        try:
            check_archive_source_identity(
                archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"stage zero\n"},
                {"pkg/source.rs": 0o600}, modes, oids,
            )
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    "noncanonical archive mode rejected under wrong check")
            output.append("MUTATION package-archive-noncanonical-mode rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH", "mutation package-archive-noncanonical-mode was accepted")
        check_archive_source_identity(
            archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"stage zero\n"},
            {"pkg/source.rs": 0o644}, modes, oids,
        )
        output.append("CONTROL package-archive-canonical-mode accepted")
        executable_modes = dict(modes)
        executable_modes["pkg/source.rs"] = "100755"
        try:
            check_archive_source_identity(
                archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"stage zero\n"},
                {"pkg/source.rs": 0o744}, executable_modes, oids,
            )
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    "noncanonical executable archive mode rejected under wrong check")
            output.append("MUTATION package-archive-noncanonical-executable-mode rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation package-archive-noncanonical-executable-mode was accepted")
        check_archive_source_identity(
            archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"stage zero\n"},
            {"pkg/source.rs": 0o755}, executable_modes, oids,
        )
        output.append("CONTROL package-archive-canonical-executable-mode accepted")

        check_archive_source_identity(
            archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"stage zero\r\n"},
            {"pkg/source.rs": 0o644}, modes, oids,
        )
        output.append("CONTROL package-archive-authorized-text-checkout accepted")
        raw_regular, raw_symlinks, raw_modes, raw_oids = package_source_snapshot(
            archive_root, ["pkg/source.raw"]
        )
        check_package_source_identity(["pkg/source.raw"], raw_regular, raw_symlinks, raw_modes)
        try:
            check_archive_source_identity(
                archive_root, ["pkg/source.raw"], {"pkg/source.raw": b"stage zero\r\n"},
                {"pkg/source.raw": 0o644}, raw_modes, raw_oids,
            )
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    "unauthorized CRLF mutation rejected under wrong check")
            output.append("MUTATION package-archive-unauthorized-crlf rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation package-archive-unauthorized-crlf was accepted")

        (archive_root / "Cargo.lock").write_bytes(
            b"version = 3\r\n\r\n[[package]]\r\n"
        )
        package_source_snapshot(archive_root, ["Cargo.lock"])
        output.append("CONTROL package-cargo-lock-authorized-checkout accepted")
        for name, relative, payload in (
            ("package-cargo-lock-mixed-newlines", "Cargo.lock",
             b"version = 3\r\n\n[[package]]\r\n"),
            ("package-cargo-lock-bare-cr", "Cargo.lock",
             b"version = 3\r[[package]]\r"),
            ("package-cargo-lock-binary", "Cargo.lock",
             b"version = 3\r\n\0\r\n"),
            ("package-cargo-lock-content-substitution", "Cargo.lock",
             b"version = 4\r\n\r\n[[package]]\r\n"),
            ("package-text-auto-working-tree-crlf", "pkg/source.raw",
             b"stage zero\r\n"),
        ):
            (archive_root / relative).write_bytes(payload)
            try:
                package_source_snapshot(archive_root, [relative])
            except GateFailure as error:
                require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                        f"mutation {name} rejected under wrong check")
                output.append(f"MUTATION {name} rejected [DR-PACKAGE]")
            else:
                raise GateFailure("DR-NO-PUBLISH", f"mutation {name} was accepted")

        filter_script = archive_root / "clean_filter.py"
        filter_script.write_text(
            'import sys\nsys.stdin.buffer.read()\nsys.stdout.buffer.write(b"stage zero\\n")\n',
            encoding="utf-8",
        )
        filter_command = f'"{sys.executable}" "{filter_script}"'
        bounded_run(["git", "config", "filter.spoof.clean", filter_command],
                    cwd=archive_root, check_id="DR-PACKAGE")
        (archive_root / ".gitattributes").write_text(
            "pkg/source.rs filter=spoof\n", encoding="utf-8"
        )
        substituted = archive_root / "substituted.rs"
        substituted.write_bytes(b"substituted\n")
        filtered_oid = bounded_run(
            ["git", "hash-object", "--path=pkg/source.rs", str(substituted)],
            cwd=archive_root, check_id="DR-PACKAGE",
        ).stdout.strip()
        require(filtered_oid == oids["pkg/source.rs"], "DR-NO-PUBLISH",
                "dirty attribute clean-filter mutation did not reproduce hash-object false authority")
        try:
            check_archive_source_identity(
                archive_root, ["pkg/source.rs"], {"pkg/source.rs": b"substituted\n"},
                {"pkg/source.rs": 0o644}, modes, oids,
            )
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    "package-archive-dirty-attributes-filter rejected under wrong check")
            output.append("MUTATION package-archive-dirty-attributes-filter rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation package-archive-dirty-attributes-filter was accepted")
    finally:
        shutil.rmtree(archive_root, onexc=remove_readonly)

    source_root = pathlib.Path(tempfile.mkdtemp(prefix="atlas-source-authority-"))
    try:
        (source_root / ".gitattributes").write_bytes(EXPECTED_GITATTRIBUTES)
        (source_root / "source.rs").write_text("indexed\n", encoding="utf-8")
        (source_root / "workflow.yml").write_text("enabled\n", encoding="utf-8")
        bounded_run(["git", "init", "-q"], cwd=source_root, check_id="DR-INPUT")
        bounded_run(["git", "add", ".gitattributes", "source.rs", "workflow.yml"],
                    cwd=source_root, check_id="DR-INPUT")
        modes, oids = stage_zero_entries(source_root, ["source.rs", "workflow.yml"], "DR-INPUT")
        policies = indexed_attribute_policies(source_root, ["source.rs", "workflow.yml"])
        (source_root / "source.rs").write_bytes(b"indexed\r\n")
        require(package_bytes_equivalent(
                    "source.rs", indexed_blob_bytes(source_root, oids["source.rs"], "DR-INPUT"),
                    working_tree_file_bytes(source_root, "source.rs", "DR-INPUT"), policies),
                "DR-NO-PUBLISH", "authorized text checkout representation was rejected")
        output.append("CONTROL source-authorized-text-checkout accepted")
        (source_root / "workflow.yml").write_text("disabled\n", encoding="utf-8")
        try:
            require(package_bytes_equivalent(
                        "workflow.yml", indexed_blob_bytes(source_root, oids["workflow.yml"], "DR-INPUT"),
                        working_tree_file_bytes(source_root, "workflow.yml", "DR-INPUT"), policies),
                    "DR-INPUT", "working tree differs from stage zero: workflow.yml")
        except GateFailure:
            output.append("MUTATION source-staged-worktree-divergence rejected [DR-INPUT]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation source-staged-worktree-divergence was accepted")
        (source_root / "source.rs").write_text("dirty\n", encoding="utf-8")
        try:
            require(package_bytes_equivalent(
                        "source.rs", indexed_blob_bytes(source_root, oids["source.rs"], "DR-INPUT"),
                        working_tree_file_bytes(source_root, "source.rs", "DR-INPUT"), policies),
                    "DR-INPUT", "working tree differs from stage zero: source.rs")
        except GateFailure:
            output.append("MUTATION source-dirty-worktree-divergence rejected [DR-INPUT]")
        else:
            raise GateFailure("DR-NO-PUBLISH",
                              "mutation source-dirty-worktree-divergence was accepted")
    finally:
        shutil.rmtree(source_root, onexc=remove_readonly)

    artifact_root = pathlib.Path(tempfile.mkdtemp(prefix="atlas-package-provenance-"))
    prior_target = os.environ.get("CARGO_TARGET_DIR")
    os.environ["CARGO_TARGET_DIR"] = str(artifact_root / "cargo-target")
    try:
        with produce_package_archive(ROOT) as (handle, _path, prefix):
            canonical_compressed = handle.getvalue()
            canonical_raw = bounded_raw_tar_bytes(io.BytesIO(canonical_compressed))
            package_archive_entries(handle, prefix)
        output.append("CONTROL package-archive-canonical-cargo accepted")

        canonical = artifact_root / "canonical.crate"
        canonical.write_bytes(canonical_compressed)
        with canonical.open("rb") as handle:
            package_archive_entries(handle, "workspace_atlas-2.0.0/")
        output.append("CONTROL package-archive-bounded-members accepted")

        arbitrary = artifact_root / "arbitrary.crate"
        write_gzip_test_archive(arbitrary, mutate_first_raw_name(
            canonical_raw, lambda name: name.replace("workspace_atlas-2.0.0/", "arbitrary-9.9.9/", 1)))
        output.append(expect_archive_failure(
            arbitrary, "package-archive-arbitrary-prefix",
            "Cargo package archive raw path spelling or root is non-canonical"))

        oversized = artifact_root / "oversized.crate"
        write_expansion_bomb(oversized, "workspace_atlas-2.0.0/")
        require(oversized.stat().st_size < MAX_OUTPUT, "DR-NO-PUBLISH",
                "compressed expansion mutation did not remain compact")
        output.append(expect_archive_failure(
            oversized, "package-archive-compressed-expansion-bomb",
            "Cargo package archive exceeds bounded raw-tar byte size"))

        header_mutations = (
            ("package-archive-noncanonical-magic", 257, 263, b"ustar\0",
             "Cargo package archive raw magic is not canonical Cargo ustar"),
            ("package-archive-noncanonical-version", 263, 265, b"00",
             "Cargo package archive raw version is not canonical Cargo ustar"),
        )
        for mutation_name, field_start, field_end, value, detail in header_mutations:
            mutated = artifact_root / f"{mutation_name}.crate"
            write_gzip_test_archive(
                mutated, mutate_first_raw_header(canonical_raw, field_start, field_end, value))
            output.append(expect_archive_failure(mutated, mutation_name, detail))

        padding = artifact_root / "nonzero-padding.crate"
        write_gzip_test_archive(padding, mutate_first_member_padding(canonical_raw))
        first_name = canonical_tar_string(canonical_raw[:100], "test raw name")
        output.append(expect_archive_failure(
            padding, "package-archive-nonzero-padding",
            f"Cargo package archive raw member padding is nonzero: {first_name}"))

        trailing_raw_tar_block = artifact_root / "trailing-raw-tar-zero-block.crate"
        write_gzip_test_archive(trailing_raw_tar_block, canonical_raw + b"\0" * 512)
        output.append(expect_archive_failure(
            trailing_raw_tar_block, "package-archive-trailing-raw-tar-zero-block",
            "Cargo package archive contains trailing raw-tar data after its canonical "
            "two-block end marker"))

        concatenated = artifact_root / "concatenated-gzip.crate"
        concatenated.write_bytes(canonical_compressed + gzip.compress(b"", mtime=0))
        output.append(expect_archive_failure(
            concatenated, "package-archive-concatenated-gzip",
            "Cargo package archive contains data after its single gzip member"))

        trailing_zero = artifact_root / "trailing-zero.crate"
        trailing_zero.write_bytes(canonical_compressed + b"\0")
        output.append(expect_archive_failure(
            trailing_zero, "package-archive-trailing-zero",
            "Cargo package archive contains data after its single gzip member"))

        bad_checksum = artifact_root / "bad-gzip-checksum.crate"
        checksum_bytes = bytearray(canonical_compressed)
        checksum_bytes[-8] ^= 1
        bad_checksum.write_bytes(checksum_bytes)
        try:
            with bad_checksum.open("rb") as handle:
                package_archive_entries(handle, "workspace_atlas-2.0.0/")
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE"
                    and error.detail.startswith("Cargo package archive gzip stream is invalid:"),
                    "DR-NO-PUBLISH", "gzip checksum mutation rejected at wrong stage")
            output.append("MUTATION package-archive-gzip-checksum rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH", "mutation package-archive-gzip-checksum was accepted")

        metadata_mutations = (
            ("package-archive-pax-size-expansion", "pax-size",
             "Cargo package archive contains PAX extension metadata"),
            ("package-archive-pax-path-override", "pax-path",
             "Cargo package archive contains PAX extension metadata"),
            ("package-archive-pax-global", "pax-global",
             "Cargo package archive contains PAX extension metadata"),
            ("package-archive-gnu-longname", "gnu-longname",
             "Cargo package archive contains GNU long-name or long-link metadata"),
            ("package-archive-gnu-longlink", "gnu-longlink",
             "Cargo package archive contains GNU long-name or long-link metadata"),
            ("package-archive-pax-sparse", "pax-sparse",
             "Cargo package archive contains PAX extension metadata"),
            ("package-archive-gnu-sparse", "gnu-sparse",
             "Cargo package archive contains GNU sparse extension metadata"),
        )
        for mutation_name, kind, detail in metadata_mutations:
            metadata_archive = artifact_root / f"{kind}.crate"
            write_gzip_test_archive(metadata_archive, insert_metadata_header(canonical_raw, kind))
            output.append(expect_archive_failure(metadata_archive, mutation_name, detail))

        backslash = artifact_root / "backslash.crate"
        write_gzip_test_archive(backslash, mutate_first_raw_name(
            canonical_raw, lambda name: name.replace("/", "\\", 1)))
        output.append(expect_archive_failure(
            backslash, "package-archive-backslash-member",
            "Cargo package archive raw path spelling or root is non-canonical"))

        package_root = artifact_root / "workspace"
        package_root.mkdir()
        (package_root / "Cargo.toml").write_text(
            '[package]\nname = "workspace_atlas"\nversion = "2.0.0"\n', encoding="utf-8"
        )
        artifact, _ = package_archive_path(package_root)
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.write_bytes(canonical_compressed)
        try:
            with produce_package_archive(package_root, lambda *_args, **_kwargs: None):
                pass
        except GateFailure:
            output.append("MUTATION package-stale-noop-cargo rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH", "mutation package-stale-noop-cargo was accepted")

        def create_archive(*_args, **_kwargs):
            path, _ = package_archive_path(package_root)
            path.write_bytes(canonical_compressed)

        try:
            with produce_package_archive(package_root, create_archive) as (handle, path, prefix):
                package_archive_entries(handle, prefix)
                path.write_bytes(b"concurrent mutation reached")
                require(path.read_bytes() == b"concurrent mutation reached", "DR-NO-PUBLISH",
                        "concurrent archive mutation was not performed")
        except GateFailure as error:
            require(error.check_id == "DR-PACKAGE", "DR-NO-PUBLISH",
                    "concurrent archive mutation rejected under wrong check")
            output.append("MUTATION package-concurrent-replacement rejected [DR-PACKAGE]")
        else:
            raise GateFailure("DR-NO-PUBLISH", "mutation package-concurrent-replacement was accepted")

        with produce_package_archive(package_root, create_archive) as (handle, path, prefix):
            original_snapshot = handle.getvalue()
            metadata = path.stat()
            changed = bytearray(path.read_bytes())
            changed[len(changed) // 2] ^= 1
            with path.open("r+b") as mutable:
                mutable.write(changed)
                mutable.flush()
                os.fsync(mutable.fileno())
            os.utime(path, ns=(metadata.st_atime_ns, metadata.st_mtime_ns))
            require(path.read_bytes() != original_snapshot
                    and file_identity(path.stat()) == file_identity(metadata),
                    "DR-NO-PUBLISH", "same-size restored-mtime mutation was not reproduced")
            package_archive_entries(handle, prefix)
        output.append("CONTROL package-concurrent-in-place-mutation-contained accepted")
    finally:
        if prior_target is None:
            os.environ.pop("CARGO_TARGET_DIR", None)
        else:
            os.environ["CARGO_TARGET_DIR"] = prior_target
        shutil.rmtree(artifact_root, onexc=remove_readonly)

    for name, payload in (
        ("attributes-bare-cr", b"* text=auto\r*.rs text eol=lf\r"),
        ("attributes-mixed-newlines", b"* text=auto\r\n*.rs text eol=lf\n"),
    ):
        try:
            parse_indexed_attributes(payload)
        except GateFailure as error:
            require(error.check_id == "DR-INPUT", "DR-NO-PUBLISH",
                    f"mutation {name} rejected under wrong check")
            output.append(f"MUTATION {name} rejected [DR-INPUT]")
        else:
            raise GateFailure("DR-NO-PUBLISH", f"mutation {name} was accepted")
    parse_indexed_attributes(EXPECTED_GITATTRIBUTES)
    output.append("CONTROL attributes-exact-lf accepted")

    total = len(mutations) + 36
    rejected = sum(line.startswith("MUTATION ") for line in output)
    output.append(f"SELF-TEST passed: {rejected}/{total} mutations rejected")
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description="Workspace Atlas no-publish distribution-readiness gate")
    parser.add_argument("--no-publish", action="store_true", help="required authority boundary")
    parser.add_argument("--static-only", action="store_true", help="validate source contracts without processes")
    parser.add_argument("--self-test", action="store_true", help="run fail-closed mutation tests")
    parser.add_argument("--binary-dir", type=pathlib.Path, help="directory containing built Atlas binaries")
    args = parser.parse_args()
    if not args.no_publish:
        parser.error("--no-publish is required; no publishing mode exists")
    if args.static_only and args.self_test:
        parser.error("--static-only and --self-test are mutually exclusive")
    try:
        sources = Sources.load(ROOT)
        if args.self_test:
            lines = run_self_test(sources)
        else:
            results = static_checks(sources)
            if not args.static_only:
                results.append(check_package_contents(ROOT))
                binary_dir = args.binary_dir
                require(binary_dir is not None, "DR-USABILITY", "--binary-dir is required for the executable dry run")
                results.append(dry_run(ROOT, binary_dir.resolve()))
            lines = [f"PASS {result.check_id}: {result.detail}" for result in results]
    except (GateFailure, OSError, UnicodeDecodeError, tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        if isinstance(error, GateFailure):
            print(f"FAIL {error.check_id}: {error.detail}", file=sys.stderr)
        else:
            print(f"FAIL DR-INPUT: {error}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
