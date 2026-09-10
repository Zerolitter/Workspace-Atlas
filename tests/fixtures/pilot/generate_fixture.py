#!/usr/bin/env python3
"""Generate a deterministic mixed workspace for Workspace Atlas acceptance tests."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path
import tempfile


MARKER = ".workspace-atlas-fixture-marker"


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def write_text(root: Path, relative: str, text: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def prepare_root(root: Path, force: bool) -> None:
    if root.exists() and any(root.iterdir()):
        marker = root / MARKER
        if not force:
            raise SystemExit(
                f"Refusing to overwrite non-empty output: {root}. Use --force only for a generated fixture."
            )
        if not marker.exists():
            raise SystemExit(
                f"Refusing --force because {root} lacks {MARKER}; this protects unrelated directories."
            )
        shutil.rmtree(root)
    root.mkdir(parents=True, exist_ok=True)
    (root / MARKER).write_bytes(b"workspace-atlas synthetic fixture v1\r\n")


def build_large_controller() -> tuple[str, int]:
    lines: list[str] = [
        "import { setConnectionState } from './state';",
        "import { RetryPolicy } from './retry_policy';",
        "",
        "type TimerHandle = { cancel(): void };",
        "",
        "function createBoundedTimer(delayMs: number): TimerHandle {",
        "  return { cancel(): void { void delayMs; } };",
        "}",
        "",
        "export const controllerVersion = 'fixture-v1';",
    ]

    while len(lines) < 3149:
        line_no = len(lines) + 1
        lines.append(f"// deterministic controller filler line {line_no:04d}")

    lines.append("/** Schedule one bounded reconnect attempt. */")  # line 3150
    target_line = len(lines) + 1
    lines.extend(
        [
            "export function scheduleReconnect(policy: RetryPolicy): TimerHandle {",
            "  if (policy.maxAttempts < 1) {",
            "    throw new Error('maxAttempts must be positive');",
            "  }",
            "",
            "  setConnectionState('reconnecting');",
            "  const handle = createBoundedTimer(policy.delayMs);",
            "  return handle;",
            "}",
            "",
            "export function cancelReconnect(handle: TimerHandle): void {",
            "  handle.cancel();",
            "  setConnectionState('idle');",
            "}",
        ]
    )

    while len(lines) < 3512:
        line_no = len(lines) + 1
        lines.append(f"// deterministic post-target filler line {line_no:04d}")

    return "\n".join(lines) + "\n", target_line


def generate(root: Path) -> dict[str, object]:
    large_text, target_line = build_large_controller()
    write_text(root, "src/controller/large_controller.ts", large_text)

    write_text(
        root,
        "src/controller/state.ts",
        """export type ConnectionState = 'idle' | 'connecting' | 'reconnecting' | 'connected';
let currentState: ConnectionState = 'idle';

export function setConnectionState(next: ConnectionState): void {
  currentState = next;
}

export function getConnectionState(): ConnectionState {
  return currentState;
}
""",
    )
    write_text(
        root,
        "src/controller/retry_policy.ts",
        """export interface RetryPolicy {
  maxAttempts: number;
  delayMs: number;
}

export const defaultRetryPolicy: RetryPolicy = {
  maxAttempts: 3,
  delayMs: 250,
};
""",
    )
    write_text(
        root,
        "src/controller/reconnect_legacy.ts",
        """// Superseded-looking fixture file; do not archive automatically.
export function reconnectLoop(): void {
  throw new Error('legacy fixture path');
}
""",
    )
    write_text(
        root,
        "src/controller/adapter_registry.ts",
        """type Adapter = { connect(): void };
const registry: Record<string, Adapter> = {};

export function loadAdapter(adapterName: string): Adapter | undefined {
  return registry[adapterName];
}
""",
    )
    write_text(
        root,
        "src/auth/login_session.ts",
        """import { getConnectionState } from '../controller/state';

export function resumeLoginSession(): boolean {
  return getConnectionState() === 'connected';
}
""",
    )
    write_text(
        root,
        "src/ui/status_panel.ts",
        """import { getConnectionState } from '../controller/state';

export function renderConnectionStatus(): string {
  return `connection:${getConnectionState()}`;
}
""",
    )

    for index in range(1, 97):
        next_index = 1 if index == 96 else index + 1
        write_text(
            root,
            f"src/modules/module_{index:03d}.ts",
            f"""import {{ moduleValue as nextValue }} from './module_{next_index:03d}';

export const moduleValue = {index};

export function calculateModule{index:03d}(input: number): number {{
  return input + moduleValue + nextValue;
}}
""",
        )

    write_text(
        root,
        "tests/controller/reconnect.test.ts",
        """import { scheduleReconnect } from '../../src/controller/large_controller';
import { defaultRetryPolicy } from '../../src/controller/retry_policy';

export function reconnectTestFixture(): void {
  const handle = scheduleReconnect(defaultRetryPolicy);
  handle.cancel();
}
""",
    )
    write_text(
        root,
        "tests/auth/login_resume.test.ts",
        """import { resumeLoginSession } from '../../src/auth/login_session';

export function loginResumeFixture(): boolean {
  return resumeLoginSession();
}
""",
    )
    for index in range(1, 11):
        write_text(
            root,
            f"tests/modules/module_{index:03d}.test.ts",
            f"""import {{ calculateModule{index:03d} }} from '../../src/modules/module_{index:03d}';
export const fixtureResult = calculateModule{index:03d}(1);
""",
        )

    write_text(
        root,
        "config/reconnect.json",
        json.dumps({"maxAttempts": 3, "delayMs": 250, "preserveLoginResume": True}, indent=2) + "\n",
    )
    write_text(
        root,
        "config/project.toml",
        """[project]
name = "workspace-atlas-fixture"

[controller]
entry = "src/controller/large_controller.ts"
""",
    )
    write_text(
        root,
        "config/features.yaml",
        """features:
  boundedReconnect: true
  dynamicAdapters: true
""",
    )

    for index in range(1, 9):
        write_text(
            root,
            f"docs/section_{index:02d}.md",
            f"""# Fixture Section {index}

This deterministic document references `src/controller/large_controller.ts`.

The reconnect behavior must preserve login resume semantics.
""",
        )

    write_text(
        root,
        "generated/client_bindings.ts",
        "// generated fixture output\nexport const generatedBinding = true;\n",
    )
    write_text(
        root,
        "vendor/dependency.ts",
        "// vendored fixture dependency\nexport const dependencyValue = 1;\n",
    )
    write_text(root, "dist/bundle.js", "// built fixture output\n")
    write_text(
        root,
        "archive/old_controller.ts",
        "// archived fixture source that must not pollute current indexing\n",
    )
    write_text(root, ".atlas/index.db", "synthetic self-exclusion marker; not a real database\n")
    write_text(root, ".env", "ATLAS_FIXTURE_TOKEN=synthetic-do-not-persist\n")
    write_text(root, "assets/logo.bin", "not-a-real-binary\u0000fixture\n")

    files_before_manifest = sorted(path for path in root.rglob("*") if path.is_file())
    max_line_file = max(
        files_before_manifest,
        key=lambda path: len(path.read_text(encoding="utf-8", errors="replace").splitlines()),
    )
    max_line_count = len(max_line_file.read_text(encoding="utf-8", errors="replace").splitlines())

    manifest: dict[str, object] = {
        "schema_version": "1.0.0",
        "fixture_name": "workspace-atlas-synthetic-v1",
        "file_count_including_manifest": len(files_before_manifest) + 1,
        "maximum_line_file": max_line_file.relative_to(root).as_posix(),
        "maximum_line_count": max_line_count,
        "target": {
            "path": "src/controller/large_controller.ts",
            "symbol": "scheduleReconnect",
            "expected_declaration_line": target_line,
            "content_sha256": sha256_text(large_text),
        },
        "operations": {
            "rename": {
                "from": "src/controller/retry_policy.ts",
                "to": "src/controller/reconnect_policy.ts",
                "expect_identity_continuity": True,
            },
            "delete": {
                "path": "src/controller/reconnect_legacy.ts",
                "expect_tombstone": True,
            },
            "change": {
                "path": "src/controller/large_controller.ts",
                "symbol": "scheduleReconnect",
                "expect_changed_file_provider_runs": 1,
            },
        },
        "expected_exclusions": [
            ".env",
            ".atlas/**",
            "generated/**",
            "vendor/**",
            "dist/**",
            "archive/**",
            "assets/*.bin",
        ],
        "known_limitation": {
            "path": "src/controller/adapter_registry.ts",
            "reason": "dynamic registry lookup cannot be exhaustively resolved structurally",
        },
    }
    write_text(root, "fixture_manifest.json", json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


GENERATOR_VERSION = "capacity-fixture-generator-v1.0.0"
CAPACITY_SCHEMA_VERSION = "1.0.0"
CAPACITY_SEED = "workspace-atlas-capacity-v1"
CAPACITY_SPECS = {
    "repo-small-v1": {
        "eligible": 128,
        "changed": 2,
        "language": {"typescript": 115, "rust": 0, "documentation": 8, "configuration": 5},
        "basis_points": {"typescript": 8984, "rust": 0, "documentation": 625, "configuration": 391},
    },
    "repo-medium-v1": {
        "eligible": 1024,
        "changed": 11,
        "language": {"typescript": 768, "rust": 154, "documentation": 51, "configuration": 51},
        "basis_points": {"typescript": 7500, "rust": 1500, "documentation": 500, "configuration": 500},
    },
    "repo-large-v1": {
        "eligible": 8192,
        "changed": 82,
        "language": {"typescript": 6144, "rust": 1229, "documentation": 410, "configuration": 409},
        "basis_points": {"typescript": 7500, "rust": 1500, "documentation": 500, "configuration": 500},
    },
}
CAPACITY_MANIFEST_KEYS = {
    "schema_version",
    "generator_version",
    "seed",
    "dataset_name",
    "files",
    "manifest_sha256",
    "counts",
    "distributions",
    "evidence_counts",
    "project_markers",
    "providers",
    "target_identity",
    "changed_set",
    "expected_accepted_outcome_hash",
    "expectations",
    "expected_ready_versus_truth_result_hash",
    "expected_exclusions",
}
EXPECTED_EXCLUSIONS = [
    ".atlas/**",
    ".env",
    ".workspace-atlas-fixture-marker",
    "archive/**",
    "assets/*.bin",
    "dist/**",
    "generated/**",
    "vendor/**",
]


def sha256_bytes(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n"
    ).encode("utf-8")


def manifest_digest(manifest: dict[str, object]) -> str:
    digest_input = dict(manifest)
    digest_input["manifest_sha256"] = ""
    return sha256_bytes(canonical_json(digest_input))


def is_excluded(relative_path: str) -> bool:
    return (
        relative_path == ".workspace-atlas-fixture-marker"
        or relative_path == ".env"
        or relative_path.startswith((".atlas/", "archive/", "dist/", "generated/", "vendor/"))
        or (relative_path.startswith("assets/") and relative_path.endswith(".bin"))
    )


def write_excluded_cases(root: Path) -> None:
    write_text(
        root,
        "generated/client_bindings.ts",
        "// generated fixture output\nexport const generatedBinding = true;\n",
    )
    write_text(
        root,
        "vendor/dependency.ts",
        "// vendored fixture dependency\nexport const dependencyValue = 1;\n",
    )
    write_text(root, "dist/bundle.js", "// built fixture output\n")
    write_text(
        root,
        "archive/old_controller.ts",
        "// archived fixture source that must not pollute current indexing\n",
    )
    write_text(root, ".atlas/index.db", "synthetic self-exclusion marker; not a real database\n")
    write_text(root, ".env", "ATLAS_FIXTURE_TOKEN=synthetic-do-not-persist\n")
    write_text(root, "assets/logo.bin", "not-a-real-binary\u0000fixture\n")


def generate_scaled_capacity_files(root: Path, dataset_name: str) -> None:
    counts = CAPACITY_SPECS[dataset_name]["language"]
    assert isinstance(counts, dict)
    typescript_count = counts["typescript"]
    rust_count = counts["rust"]
    documentation_count = counts["documentation"]
    configuration_count = counts["configuration"]
    assert all(isinstance(count, int) for count in counts.values())

    write_text(
        root,
        "src/ts/module_00000.ts",
        "export const capacityTarget = 0;\nexport function capacityValue(): number { return capacityTarget; }\n",
    )
    write_text(
        root,
        "src/ts/rename_candidate.ts",
        "export const renameCandidate = 'before';\n",
    )
    write_text(
        root,
        "src/ts/delete_candidate.ts",
        "export const deleteCandidate = true;\n",
    )
    test_count = typescript_count // 5
    source_count = typescript_count - test_count
    for index in range(3, source_count):
        write_text(
            root,
            f"src/ts/module_{index:05d}.ts",
            f"export const module{index:05d} = {index};\n",
        )
    for index in range(test_count):
        write_text(
            root,
            f"tests/ts/module_{index:05d}.test.ts",
            f"export const testValue{index:05d} = {index};\n",
        )

    rust_test_count = rust_count // 5
    rust_source_count = rust_count - rust_test_count
    for index in range(rust_source_count):
        write_text(
            root,
            f"src/rust/module_{index:05d}.rs",
            f"pub const MODULE_{index:05d}: usize = {index};\n",
        )
    for index in range(rust_test_count):
        write_text(
            root,
            f"tests/rust/module_{index:05d}_test.rs",
            f"pub const TEST_VALUE_{index:05d}: usize = {index};\n",
        )

    for index in range(documentation_count):
        write_text(
            root,
            f"docs/section_{index:05d}.md",
            f"# Capacity Section {index}\n\nPrivate deterministic fixture content.\n",
        )

    markers = {
        "package.json": '{"name":"workspace-atlas-capacity-fixture","private":true}\n',
        "tsconfig.json": '{"compilerOptions":{"strict":true}}\n',
        "Cargo.toml": '[package]\nname = "workspace-atlas-capacity-fixture"\nversion = "0.0.0"\n',
    }
    for path, content in markers.items():
        write_text(root, path, content)
    for index in range(configuration_count - len(markers)):
        write_text(
            root,
            f"config/setting_{index:05d}.json",
            json.dumps({"capacitySetting": index}, separators=(",", ":")) + "\n",
        )
    write_excluded_cases(root)


def generate_capacity_files(root: Path, dataset_name: str) -> None:
    if dataset_name == "repo-small-v1":
        generate(root)
        (root / "fixture_manifest.json").unlink()
        write_text(
            root,
            "package.json",
            '{"name":"workspace-atlas-capacity-fixture","private":true}\n',
        )
        write_text(root, "tsconfig.json", '{"compilerOptions":{"strict":true}}\n')
        return
    generate_scaled_capacity_files(root, dataset_name)


def language_class(relative_path: str) -> str:
    if relative_path.endswith(".ts"):
        return "typescript"
    if relative_path.endswith(".rs"):
        return "rust"
    if relative_path.endswith(".md"):
        return "documentation"
    return "configuration"


def artifact_class(relative_path: str) -> str:
    if relative_path.startswith("tests/"):
        return "test"
    if relative_path.startswith("docs/"):
        return "documentation"
    if language_class(relative_path) == "configuration":
        return "configuration"
    return "source"


def capacity_changed_set(
    root: Path, dataset_name: str, eligible_paths: list[str]
) -> list[dict[str, object]]:
    wanted = CAPACITY_SPECS[dataset_name]["changed"]
    assert isinstance(wanted, int)
    if dataset_name == "repo-small-v1":
        rename_path = "src/controller/retry_policy.ts"
        renamed_path = "src/controller/reconnect_policy.ts"
        delete_path = "src/controller/reconnect_legacy.ts"
    else:
        rename_path = "src/ts/rename_candidate.ts"
        renamed_path = "src/ts/renamed_candidate.ts"
        delete_path = "src/ts/delete_candidate.ts"

    before = (root / rename_path).read_bytes()
    after = before.replace(b"before", b"after")
    if after == before:
        after += b"// deterministic semantic rename edit\n"
    changed: list[dict[str, object]] = [
        {
            "relative_path": rename_path,
            "result_relative_path": renamed_path,
            "operation_classes": ["rename", "semantic_edit"],
            "before_sha256": sha256_bytes(before),
            "after_sha256": sha256_bytes(after),
            "changed_bytes": max(len(before), len(after)),
        }
    ]

    deleted = (root / delete_path).read_bytes()
    changed.append(
        {
            "relative_path": delete_path,
            "result_relative_path": None,
            "operation_classes": ["delete"],
            "before_sha256": sha256_bytes(deleted),
            "after_sha256": None,
            "changed_bytes": len(deleted),
        }
    )

    reserved = {rename_path, delete_path}
    candidates = [
        path
        for path in eligible_paths
        if path not in reserved and language_class(path) in {"typescript", "rust"}
    ]
    for index, path in enumerate(candidates[: wanted - 2], start=1):
        content = (root / path).read_bytes()
        suffix = f"// deterministic semantic capacity edit {index:05d}\n".encode()
        after_content = content + suffix
        changed.append(
            {
                "relative_path": path,
                "result_relative_path": path,
                "operation_classes": ["semantic_edit"],
                "before_sha256": sha256_bytes(content),
                "after_sha256": sha256_bytes(after_content),
                "changed_bytes": max(len(content), len(after_content)),
            }
        )
    assert len(changed) == wanted
    return sorted(changed, key=lambda item: str(item["relative_path"]))


def build_capacity_manifest(root: Path, dataset_name: str) -> dict[str, object]:
    spec = CAPACITY_SPECS[dataset_name]
    all_paths = sorted(
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and path.name != "fixture_manifest.json"
    )
    eligible_paths = [path for path in all_paths if not is_excluded(path)]
    excluded_paths = [path for path in all_paths if is_excluded(path)]
    expected_eligible = spec["eligible"]
    assert isinstance(expected_eligible, int)
    if len(eligible_paths) != expected_eligible:
        raise ValueError(
            f"{dataset_name}: expected {expected_eligible} eligible files, found {len(eligible_paths)}"
        )

    files = [
        {
            "relative_path": path,
            "sha256": sha256_bytes((root / path).read_bytes()),
        }
        for path in eligible_paths
    ]
    language_counts = {
        name: sum(language_class(path) == name for path in eligible_paths)
        for name in ("typescript", "rust", "documentation", "configuration")
    }
    expected_language = spec["language"]
    if language_counts != expected_language:
        raise ValueError(
            f"{dataset_name}: language distribution {language_counts} != {expected_language}"
        )
    artifact_counts = {
        name: sum(artifact_class(path) == name for path in eligible_paths)
        for name in ("source", "test", "documentation", "configuration")
    }
    changed_set = capacity_changed_set(root, dataset_name, eligible_paths)
    changed_bytes = sum(int(item["changed_bytes"]) for item in changed_set)
    total_bytes = sum((root / path).stat().st_size for path in eligible_paths)
    excluded_bytes = sum((root / path).stat().st_size for path in excluded_paths)
    project_markers = [
        path for path in ("Cargo.toml", "package.json", "tsconfig.json") if path in eligible_paths
    ]
    providers = [
        {"name": "builtin-deterministic", "version": "0.4.0", "state": "required"},
        {"name": "scip-typescript", "version": "0.4.0", "state": "required"},
        {
            "name": "rust-analyzer",
            "version": "1.94.1",
            "state": "not_applicable" if dataset_name == "repo-small-v1" else "optional_degraded",
        },
    ]
    target_path = (
        "src/controller/large_controller.ts"
        if dataset_name == "repo-small-v1"
        else "src/ts/module_00000.ts"
    )
    target_symbol = "scheduleReconnect" if dataset_name == "repo-small-v1" else "capacityValue"
    target_identity = {
        "relative_path": target_path,
        "symbol": target_symbol,
        "sha256": sha256_bytes((root / target_path).read_bytes()),
    }
    semantic_changes = sum(
        "semantic_edit" in item["operation_classes"] for item in changed_set
    )
    evidence_counts = {
        "symbols": language_counts["typescript"] + language_counts["rust"],
        "relationships": max(0, language_counts["typescript"] + language_counts["rust"] - 1),
        "effects": semantic_changes,
        "diagnostics": 0,
        "coverage": language_counts["typescript"] + language_counts["rust"],
        "conflicts": 0,
    }
    accepted_outcome_hash = sha256_bytes(
        canonical_json(
            {
                "dataset_name": dataset_name,
                "evidence_counts": evidence_counts,
                "target_identity": target_identity,
            }
        )
    )
    ready_truth_hash = sha256_bytes(
        canonical_json(
            {
                "accepted_outcome_hash": accepted_outcome_hash,
                "equivalence": "ready-serving-equals-truth-fallback",
            }
        )
    )
    manifest: dict[str, object] = {
        "schema_version": CAPACITY_SCHEMA_VERSION,
        "generator_version": GENERATOR_VERSION,
        "seed": CAPACITY_SEED,
        "dataset_name": dataset_name,
        "files": files,
        "manifest_sha256": "",
        "counts": {
            "eligible_files": len(eligible_paths),
            "indexed_files": len(eligible_paths),
            "excluded_files": len(excluded_paths),
            "initial_files": len(eligible_paths),
            "changed_files": len(changed_set),
            "changed_bytes": changed_bytes,
            "total_bytes": total_bytes,
            "excluded_bytes": excluded_bytes,
        },
        "distributions": {
            "language": {
                "target_basis_points": spec["basis_points"],
                "files": language_counts,
            },
            "artifact_class": {"files": artifact_counts},
        },
        "evidence_counts": evidence_counts,
        "project_markers": project_markers,
        "providers": providers,
        "target_identity": target_identity,
        "changed_set": changed_set,
        "expected_accepted_outcome_hash": accepted_outcome_hash,
        "expectations": {
            "correctness_required": True,
            "deterministic_identity_required": True,
        },
        "expected_ready_versus_truth_result_hash": ready_truth_hash,
        "expected_exclusions": EXPECTED_EXCLUSIONS,
    }
    manifest["manifest_sha256"] = manifest_digest(manifest)
    return manifest


def create_capacity_fixture(root: Path, dataset_name: str, force: bool) -> dict[str, object]:
    if dataset_name not in CAPACITY_SPECS:
        raise ValueError(f"unknown capacity dataset: {dataset_name}")
    prepare_root(root, force)
    generate_capacity_files(root, dataset_name)
    manifest = build_capacity_manifest(root, dataset_name)
    (root / "fixture_manifest.json").write_bytes(canonical_json(manifest))
    return manifest


def expected_capacity_manifest(dataset_name: str) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="workspace-atlas-capacity-") as temporary:
        root = Path(temporary)
        prepare_root(root, False)
        generate_capacity_files(root, dataset_name)
        return build_capacity_manifest(root, dataset_name)


def validate_capacity_manifest_bytes(raw: bytes) -> dict[str, object]:
    try:
        manifest = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"manifest is not canonical UTF-8 JSON: {error}") from error
    if not isinstance(manifest, dict):
        raise ValueError("manifest root must be an object")
    if set(manifest) != CAPACITY_MANIFEST_KEYS:
        raise ValueError("manifest fields are not the closed capacity schema")
    if canonical_json(manifest) != raw:
        raise ValueError("manifest bytes are not canonical sorted JSON")
    dataset_name = manifest.get("dataset_name")
    if not isinstance(dataset_name, str) or dataset_name not in CAPACITY_SPECS:
        raise ValueError("manifest dataset_name is not a frozen capacity stratum")
    files = manifest.get("files")
    if not isinstance(files, list):
        raise ValueError("manifest files must be a list")
    paths = [
        item.get("relative_path")
        for item in files
        if isinstance(item, dict)
    ]
    if len(paths) != len(files) or any(not isinstance(path, str) for path in paths):
        raise ValueError("every manifest file must have a relative_path")
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise ValueError("manifest paths must be unique and sorted")
    if any("\\" in path or path.startswith("/") or ".." in Path(path).parts for path in paths):
        raise ValueError("manifest paths must be canonical relative POSIX paths")
    if manifest.get("manifest_sha256") != manifest_digest(manifest):
        raise ValueError("manifest self-digest mismatch")
    expected = expected_capacity_manifest(dataset_name)
    if manifest != expected:
        raise ValueError("manifest differs from the frozen generated capacity contract")
    return manifest


def capacity_self_test() -> list[str]:
    accepted: dict[str, bytes] = {}
    for dataset_name in CAPACITY_SPECS:
        manifest = expected_capacity_manifest(dataset_name)
        raw = canonical_json(manifest)
        validate_capacity_manifest_bytes(raw)
        accepted[dataset_name] = raw

    baseline = json.loads(accepted["repo-small-v1"])
    mutations: list[tuple[str, dict[str, object] | bytes]] = []

    def mutate(name: str, transform: object, *, reseal: bool = True) -> None:
        document = json.loads(canonical_json(baseline))
        assert callable(transform)
        transform(document)
        if reseal:
            document["manifest_sha256"] = manifest_digest(document)
        mutations.append((name, document))

    mutate("unexpected-field", lambda value: value.__setitem__("unexpected", True))
    mutate("duplicate-path", lambda value: value["files"][1].__setitem__("relative_path", value["files"][0]["relative_path"]))
    mutate("non-posix-path", lambda value: value["files"][0].__setitem__("relative_path", "config\\features.yaml"))
    mutate("path-order", lambda value: value["files"].reverse())
    mutate("path-hash", lambda value: value["files"][0].__setitem__("sha256", "0" * 64))
    for count_name in baseline["counts"]:
        mutate(f"count-{count_name}", lambda value, key=count_name: value["counts"].__setitem__(key, value["counts"][key] + 1))
    mutate("language-target", lambda value: value["distributions"]["language"]["target_basis_points"].__setitem__("typescript", 1))
    mutate("language-count", lambda value: value["distributions"]["language"]["files"].__setitem__("typescript", 1))
    mutate("artifact-count", lambda value: value["distributions"]["artifact_class"]["files"].__setitem__("source", 1))
    mutate("provider", lambda value: value["providers"][0].__setitem__("version", "wrong"))
    mutate("target", lambda value: value["target_identity"].__setitem__("symbol", "wrong"))
    mutate("accepted-outcome", lambda value: value.__setitem__("expected_accepted_outcome_hash", "0" * 64))
    mutate("correctness", lambda value: value["expectations"].__setitem__("correctness_required", False))
    mutate("deterministic-identity", lambda value: value["expectations"].__setitem__("deterministic_identity_required", False))
    mutate("ready-versus-truth", lambda value: value.__setitem__("expected_ready_versus_truth_result_hash", "0" * 64))
    mutate("changed-set", lambda value: value["changed_set"][0].__setitem__("changed_bytes", 0))
    mutate("evidence-count", lambda value: value["evidence_counts"].__setitem__("symbols", 0))
    mutate("project-marker", lambda value: value["project_markers"].clear())
    mutate("generator-version", lambda value: value.__setitem__("generator_version", "wrong"))
    mutate("seed", lambda value: value.__setitem__("seed", "wrong"))
    mutate(
        "self-digest",
        lambda value: value.__setitem__("manifest_sha256", "0" * 64),
        reseal=False,
    )
    mutations.append(("non-canonical-json", json.dumps(baseline, indent=2, sort_keys=True).encode()))

    rejected: list[str] = []
    for name, mutation in mutations:
        raw = mutation if isinstance(mutation, bytes) else canonical_json(mutation)
        try:
            validate_capacity_manifest_bytes(raw)
        except ValueError:
            rejected.append(name)
        else:
            raise AssertionError(f"mutation was accepted: {name}")
    return rejected



def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="Fixture output directory")
    parser.add_argument("--force", action="store_true", help="Replace a prior generated fixture")
    parser.add_argument("--capacity", choices=sorted(CAPACITY_SPECS), help="Generate a capacity stratum")
    parser.add_argument("--manifest-output", type=Path, help="Also write the capacity manifest here")
    parser.add_argument("--validate-manifest", type=Path, help="Validate one committed capacity manifest")
    parser.add_argument("--self-test", action="store_true", help="Run deterministic fail-closed manifest checks")
    args = parser.parse_args()

    if args.self_test:
        rejected = capacity_self_test()
        print(f"Capacity self-test: 3 manifests accepted; {len(rejected)} mutations rejected")
        print("Rejected: " + ", ".join(rejected))
        return 0

    if args.validate_manifest:
        manifest = validate_capacity_manifest_bytes(args.validate_manifest.read_bytes())
        print(
            f"Validated capacity manifest: {manifest['dataset_name']} "
            f"({manifest['manifest_sha256']})"
        )
        return 0

    if args.output is None:
        parser.error("--output is required when generating a fixture")
    root = args.output.expanduser().resolve()
    if args.capacity:
        manifest = create_capacity_fixture(root, args.capacity, args.force)
        if args.manifest_output:
            args.manifest_output.parent.mkdir(parents=True, exist_ok=True)
            args.manifest_output.write_bytes(canonical_json(manifest))
        print(f"Generated capacity fixture: {root}")
        print(
            f"Eligible/indexed: {manifest['counts']['eligible_files']}; "
            f"changed: {manifest['counts']['changed_files']}"
        )
        print(f"Manifest SHA-256: {manifest['manifest_sha256']}")
        return 0

    if args.manifest_output:
        parser.error("--manifest-output requires --capacity")
    prepare_root(root, args.force)
    manifest = generate(root)

    print(f"Generated fixture: {root}")
    print(f"Files: {manifest['file_count_including_manifest']}")
    print(
        f"Largest: {manifest['maximum_line_file']} ({manifest['maximum_line_count']} lines)"
    )
    target = manifest["target"]
    assert isinstance(target, dict)
    print(f"Target: {target['path']}::{target['symbol']} at line {target['expected_declaration_line']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
