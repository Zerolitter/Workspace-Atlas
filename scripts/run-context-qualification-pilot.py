#!/usr/bin/env python3
"""Execute or verify one of two frozen Stage 2 context-qualification pilots."""
from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import subprocess
import tomllib
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import sys
import time
from typing import Any, Callable

MANIFEST_VERSION = "atlas-context-pilot-run-manifest-v1.0.0"
RECEIPT_VERSION = "atlas-context-pilot-executed-receipt-v1.0.0"
REGISTRY_VERSION = "atlas-context-pilot-registry-v1.0.0"
FROZEN_COMMIT = "f5f5c1e500c34fb9eeda6b8aa97ef24bd964246a"
FIXTURE_ROOT = Path(__file__).with_name("fixtures") / "context_execution"
REGISTRY_PATH = FIXTURE_ROOT / "registry-v1.json"
LOCKFILE_PATH = "Cargo.lock"
LOCKFILE_CANONICAL_SHA256 = (
    "c83e54eb1be0feac9e4f819e010a0e2d27f9e084ae59f0015fa11b1000e105f0"
)
LOCKFILE_CANONICAL_BYTES = 39_546
LOCKFILE_REPRESENTATIONS = [
    "canonical_lf",
    "deterministic_crlf",
]
FROZEN_CHECKOUT_ROLES = (
    "baseline_checkout",
    "model_checkout",
    "validation_checkout",
    "acquisition_checkout",
)
CARGO_TARGET_CHILDREN = {
    "baseline_checkout": "baseline",
    "validation_checkout": "validation",
}
MAX_JSON_BYTES = 16 * 1024 * 1024
MAX_STREAM_BYTES = 16 * 1024 * 1024
MAX_EXECUTABLE_BYTES = 256 * 1024 * 1024
MAX_SESSION_BYTES = 64 * 1024 * 1024
MAX_SAFE_TEXT_BYTES = 256 * 1024
MAX_DEPENDENCIES = 4096
class _AllIntegerReturnCodes:
    def __contains__(self, value: object) -> bool:
        return isinstance(value, int) and not isinstance(value, bool)


ALL_RETURN_CODES = _AllIntegerReturnCodes()
BUILD_AFFECTING_IGNORED = (
    ".cargo", ".cargo/config", ".cargo/config.toml", "Cargo.toml", "Cargo.lock",
    "rust-toolchain", "rust-toolchain.toml", "build.rs",
    "config/workspace-atlas.local.toml",
)
USAGE_FIELDS = (
    "input", "output", "totalTokens", "reasoningTokens", "cacheRead", "cacheWrite",
)


class PilotError(RuntimeError):
    """The pilot cannot satisfy its frozen execution contract."""


class PilotTransportError(PilotError):
    """A contained subprocess did not complete with qualified transport."""

def _load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise PilotError(f"required module is unavailable: {path.name}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


PROCESS_GUARD = _load_module(
    "atlas_context_pilot_process_guard", Path(__file__).with_name("provider-smoke.py")
)
QUALIFICATION = _load_module(
    "atlas_context_pilot_qualification", Path(__file__).with_name("qualify-context-evidence.py")
)
PUBLIC_HYGIENE = _load_module(
    "atlas_context_pilot_public_hygiene", Path(__file__).with_name("check-public-hygiene.py")
)
_EXPORTER = QUALIFICATION._EXPORTER


def canonical_document_bytes(value: Any) -> bytes:
    try:
        return (
            json.dumps(
                value,
                sort_keys=True,
                separators=(",", ":"),
                ensure_ascii=False,
                allow_nan=False,
            )
            + "\n"
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise PilotError("canonical document contains an unsupported value") from error


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def seal_document(value: dict[str, Any], hash_field: str) -> dict[str, Any]:
    if hash_field in value:
        raise PilotError(f"unsealed document already contains {hash_field}")
    sealed = dict(value)
    sealed[hash_field] = sha256(canonical_document_bytes(value))
    return sealed


def _verify_seal(value: dict[str, Any], hash_field: str, role: str) -> None:
    digest = value.get(hash_field)
    if not isinstance(digest, str) or len(digest) != 64:
        raise PilotError(f"{role} hash is missing or malformed")
    material = {key: item for key, item in value.items() if key != hash_field}
    if sha256(canonical_document_bytes(material)) != digest:
        raise PilotError(f"{role} hash mismatch")


def _exact(value: Any, fields: set[str] | frozenset[str], role: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(fields):
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise PilotError(
            f"{role} fields must be exactly {sorted(fields)!r}; got {actual!r}"
        )
    return value


def _string(value: Any, role: str, maximum: int = 4096) -> str:
    if (
        not isinstance(value, str)
        or not value
        or "\0" in value
        or len(value.encode("utf-8")) > maximum
    ):
        raise PilotError(f"{role} must be a nonempty bounded string")
    return value


def _string_array(value: Any, role: str, maximum: int = 4096) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum:
        raise PilotError(f"{role} must be a bounded array")
    result = [_string(item, f"{role} item") for item in value]
    if len(result) != len(set(result)):
        raise PilotError(f"{role} contains duplicates")
    return result


def _hash(value: Any, role: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise PilotError(f"{role} must be a lowercase SHA-256")
    return value


def _relative_path(value: Any, role: str) -> str:
    path = _string(value, role, 512)
    posix = PurePosixPath(path)
    windows = PureWindowsPath(path)
    if (
        "\\" in path
        or ":" in path
        or posix.is_absolute()
        or windows.drive
        or windows.root
        or any(part in ("", ".", "..") for part in path.split("/"))
        or posix.as_posix() != path
    ):
        raise PilotError(f"{role} must be a canonical relative path")
    return path


def _json_load(data: bytes, role: str) -> Any:
    try:
        return _EXPORTER._json_load(data.decode("utf-8"), role)
    except UnicodeDecodeError as error:
        raise PilotError(f"{role} is not UTF-8") from error
    except _EXPORTER.ExportError as error:
        raise PilotError(str(error)) from error


def _read_bounded(path: Path, maximum: int, role: str) -> bytes:
    try:
        safe_path = _safe_existing(path, role, "file")
        return _EXPORTER._read_bounded_file(safe_path, maximum, role)
    except _EXPORTER.ExportError as error:
        raise PilotError(str(error)) from error


def _safe_metadata(path: Path, role: str, kind: str) -> Any:
    try:
        return _EXPORTER._safe_metadata(path, role, kind)
    except _EXPORTER.ExportError as error:
        raise PilotError(str(error)) from error


def _safe_existing(path: Path, role: str, kind: str) -> Path:
    raw = str(path).replace("\\", "/")
    if any(component == ".." for component in raw.split("/")):
        raise PilotError(f"{role} contains a traversal alias")
    absolute = path.absolute()
    current = absolute
    while True:
        if current.exists() or current.is_symlink():
            metadata = _safe_metadata(
                current,
                role,
                "directory" if current != absolute else kind,
            )
            if getattr(metadata, "st_file_attributes", 0) & 0x400:
                raise PilotError(f"{role} has a link or reparse ancestor")
        parent = current.parent
        if parent == current:
            break
        current = parent
    try:
        return absolute.resolve(strict=True)
    except OSError as error:
        raise PilotError(f"{role} cannot be resolved") from error


def _checkout_member(
    checkout: Path,
    relative: str,
    role: str,
    *,
    must_exist: bool,
) -> Path:
    root = _safe_existing(checkout, f"{role} checkout", "directory")
    normalized = _relative_path(relative, role)
    candidate = root / normalized
    current = candidate if must_exist else candidate.parent
    while True:
        if current.exists() or current.is_symlink():
            _safe_metadata(
                current,
                role,
                "file" if must_exist and current == candidate else "directory",
            )
        if current == root:
            break
        if not _is_within(current, root):
            raise PilotError(f"{role} escapes checkout")
        current = current.parent
    if must_exist:
        _safe_metadata(candidate, role, "file")
    elif candidate.exists() or candidate.is_symlink():
        raise PilotError(f"{role} destination already exists")
    return candidate


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _overlap(left: Path, right: Path) -> bool:
    return left == right or _is_within(left, right) or _is_within(right, left)


def validate_path_layout(layout: dict[str, Path]) -> dict[str, Path]:
    required = {
        "scratch_root", "baseline_checkout", "model_checkout", "validation_checkout",
        "evidence_root", "target_root", "trusted_root", "acquisition_checkout",
    }
    if set(layout) != required:
        raise PilotError("path layout fields are incomplete or unknown")
    normalized = {
        key: _safe_existing(path, key.replace("_", " "), "directory")
        for key, path in layout.items()
    }
    scratch = normalized["scratch_root"]
    checkouts = [
        normalized["baseline_checkout"], normalized["model_checkout"],
        normalized["validation_checkout"],
    ]
    if len(set(checkouts)) != 3 or any(_overlap(a, b) for index, a in enumerate(checkouts) for b in checkouts[index + 1 :]):
        raise PilotError("baseline, model, and validation checkouts must be distinct")
    if any(not _is_within(checkout, scratch) for checkout in checkouts):
        raise PilotError("all mutation and validation checkouts must be inside scratch root")
    external = [
        normalized["evidence_root"], normalized["target_root"],
        normalized["trusted_root"], normalized["acquisition_checkout"],
    ]
    for checkout in checkouts:
        if any(_overlap(checkout, path) for path in external):
            raise PilotError("checkouts overlap evidence, target, trusted, or acquisition roots")
    if any(_overlap(a, b) for index, a in enumerate(external) for b in external[index + 1 :]):
        raise PilotError("evidence, target, trusted, and acquisition roots must be distinct")
    return normalized


def _derived_cargo_targets(target_root: Path) -> dict[str, Path]:
    return {
        role: target_root / child
        for role, child in CARGO_TARGET_CHILDREN.items()
    }


def _reserve_cargo_targets(target_root: Path) -> dict[str, Path]:
    root = _safe_existing(target_root, "Cargo target container", "directory")
    if any(root.iterdir()):
        raise PilotError("Cargo target container must be fresh and empty")
    targets = _derived_cargo_targets(root)
    try:
        for path in targets.values():
            path.mkdir()
    except OSError as error:
        raise PilotError("could not reserve isolated Cargo targets") from error
    if set(root.iterdir()) != set(targets.values()):
        raise PilotError("Cargo target container reservation changed")
    return {
        role: _safe_existing(path, f"{role} Cargo target", "directory")
        for role, path in targets.items()
    }


def _cargo_target_path(
    manifest: dict[str, Any], checkout_name: str
) -> Path:
    if checkout_name not in CARGO_TARGET_CHILDREN:
        raise PilotError(f"unsupported Cargo target role: {checkout_name}")
    target_root = _safe_existing(
        _manifest_path(manifest, "target_root"),
        "Cargo target container",
        "directory",
    )
    expected = _derived_cargo_targets(target_root)
    role_targets = manifest["cache"]["role_targets"]
    if set(role_targets) != set(CARGO_TARGET_CHILDREN):
        raise PilotError("Cargo role target bindings are incomplete")
    for role, path in expected.items():
        if Path(role_targets[role]).absolute() != path.absolute():
            raise PilotError(f"Cargo target role binding mismatch: {role}")
    if set(target_root.iterdir()) != set(expected.values()):
        raise PilotError("Cargo target container contains foreign entries")
    return _safe_existing(
        expected[checkout_name],
        f"{checkout_name} Cargo target",
        "directory",
    )


def _require_empty_cargo_target(
    manifest: dict[str, Any], checkout_name: str
) -> None:
    path = _cargo_target_path(manifest, checkout_name)
    if any(path.iterdir()):
        raise PilotError(
            f"{checkout_name} Cargo target is not fresh and empty"
        )


def validate_git_fence(
    *,
    head: str,
    branch: str,
    porcelain: str,
    diff_names: list[str],
    cached_names: list[str],
    untracked_names: list[str],
    ignored_porcelain: str,
    allowed_modified: set[str],
    allowed_untracked: set[str],
) -> None:
    if head != FROZEN_COMMIT:
        raise PilotError("checkout HEAD is not the frozen commit")
    if branch:
        raise PilotError("checkout must be detached from every branch")
    if set(cached_names):
        raise PilotError(f"checkout has staged files: {sorted(set(cached_names))!r}")
    if set(diff_names) != allowed_modified:
        raise PilotError(f"checkout modified-file fence mismatch: {sorted(set(diff_names))!r}")
    if set(untracked_names) != allowed_untracked:
        raise PilotError(f"checkout untracked-file fence mismatch: {sorted(set(untracked_names))!r}")
    porcelain_modified = set()
    porcelain_untracked = set()
    for line in porcelain.splitlines():
        if len(line) < 4 or line[2] != " ":
            raise PilotError(f"checkout porcelain record is malformed: {line!r}")
        status = line[:2]
        path = line[3:].replace("\\", "/")
        if " -> " in path:
            raise PilotError("checkout porcelain rename is not permitted")
        if status == "??":
            porcelain_untracked.add(path)
        elif status == " M":
            porcelain_modified.add(path)
        else:
            raise PilotError(
                f"checkout porcelain status is not an allowed unstaged modification: {status!r}"
            )
    if (
        porcelain_modified != allowed_modified
        or porcelain_untracked != allowed_untracked
    ):
        raise PilotError("checkout porcelain facts disagree with expected sets")
    for line in ignored_porcelain.splitlines():
        path = line[3:] if len(line) > 3 else ""
        normalized = path.replace("\\", "/").rstrip("/")
        if line.startswith("!! ") and any(
            normalized == marker or normalized.startswith(marker + "/")
            for marker in BUILD_AFFECTING_IGNORED
        ):
            raise PilotError(f"checkout contains ignored build-affecting input: {normalized}")


def reject_mutable_checkout_authorities(
    file_authorities: dict[str, Path],
    checkouts: list[Path],
) -> None:
    for role, path in file_authorities.items():
        absolute = _safe_existing(path, role, "file")
        if any(_is_within(absolute, checkout) for checkout in checkouts):
            raise PilotError(
                f"{role} must not be inside a mutable checkout"
            )


def _registry_task_fields() -> set[str]:
    return {
        "task_class", "specification", "candidate_source", "candidate_source_sha256",
        "candidate_source_allowlist", "acquisition_source_sha256", "corpus_sha256",
        "task_spec_sha256", "validation_contract_sha256", "probe_fixture",
        "probe_sha256", "probe_injection_path", "validator_contract_fixture",
        "validator_contract_file_sha256", "acceptance_target", "acceptance_names",
        "baseline_passed_names", "baseline_failed_names", "regression_target",
        "regression_names", "regression_source", "regression_source_sha256",
        "protected_file_sha256", "protected_lockfile_contract",
        "required_outcomes",
    }



def validate_registry(value: Any, raw: bytes) -> dict[str, Any]:
    registry = _exact(
        value,
        {"schema_version", "kind", "frozen_commit", "tasks"},
        "registry",
    )
    if registry["schema_version"] != REGISTRY_VERSION or registry["kind"] != "atlas-context-pilot-registry":
        raise PilotError("registry schema is incompatible")
    if registry["frozen_commit"] != FROZEN_COMMIT:
        raise PilotError("registry frozen commit mismatch")
    expected_tasks = {
        "single-file-canonical-id-whitespace",
        "configuration-project-marker-validation",
    }
    if not isinstance(registry["tasks"], dict) or set(registry["tasks"]) != expected_tasks:
        raise PilotError("registry task set must contain exactly the two supported tasks")
    for task_id, recipe in registry["tasks"].items():
        _exact(recipe, _registry_task_fields(), f"registry task {task_id}")
        for field in (
            "task_class", "specification", "candidate_source", "probe_fixture",
            "probe_injection_path", "validator_contract_fixture", "acceptance_target",
            "regression_target", "regression_source",
        ):
            _string(recipe[field], f"registry task {task_id}.{field}")
        for field in (
            "candidate_source_sha256", "corpus_sha256", "task_spec_sha256",
            "validation_contract_sha256", "probe_sha256",
            "validator_contract_file_sha256", "regression_source_sha256",
        ):
            _hash(recipe[field], f"registry task {task_id}.{field}")
        for field in (
            "candidate_source_allowlist", "acceptance_names", "baseline_passed_names",
            "baseline_failed_names", "regression_names", "required_outcomes",
        ):
            _string_array(recipe[field], f"registry task {task_id}.{field}")
        if recipe["candidate_source_allowlist"] != [recipe["candidate_source"]]:
            raise PilotError("registry candidate source allowlist is not exact")
        if set(recipe["baseline_passed_names"]) | set(recipe["baseline_failed_names"]) != set(recipe["acceptance_names"]):
            raise PilotError("registry baseline partition is incomplete")
        if set(recipe["baseline_passed_names"]) & set(recipe["baseline_failed_names"]):
            raise PilotError("registry baseline partition overlaps")
        for mapping_field in ("acquisition_source_sha256", "protected_file_sha256"):
            mapping = recipe[mapping_field]
            if not isinstance(mapping, dict) or not mapping:
                raise PilotError(f"registry task {task_id}.{mapping_field} must be a nonempty object")
            for path, digest in mapping.items():
                _relative_path(path, f"registry task {task_id}.{mapping_field} path")
                _hash(digest, f"registry task {task_id}.{mapping_field}.{path}")
        if LOCKFILE_PATH in recipe["protected_file_sha256"]:
            raise PilotError(
                "registry lockfile must use its dedicated byte contract"
            )
        lockfile = _exact(
            recipe["protected_lockfile_contract"],
            {
                "path", "canonical_blob_sha256", "canonical_bytes",
                "canonical_line_endings",
                "allowed_checkout_representations",
            },
            f"registry task {task_id}.protected_lockfile_contract",
        )
        if (
            lockfile["path"] != LOCKFILE_PATH
            or lockfile["canonical_blob_sha256"]
            != LOCKFILE_CANONICAL_SHA256
            or lockfile["canonical_bytes"] != LOCKFILE_CANONICAL_BYTES
            or isinstance(lockfile["canonical_bytes"], bool)
            or lockfile["canonical_line_endings"] != "lf"
            or lockfile["allowed_checkout_representations"]
            != LOCKFILE_REPRESENTATIONS
        ):
            raise PilotError(
                f"registry task {task_id} lockfile contract is incompatible"
            )
    result = dict(registry)
    result["registry_sha256"] = sha256(raw)
    return result


def load_registry() -> dict[str, Any]:
    raw = _read_bounded(REGISTRY_PATH, MAX_JSON_BYTES, "pilot registry")
    return validate_registry(_json_load(raw, "pilot registry"), raw)


def trusted_recipe(registry: dict[str, Any], task_id: str) -> dict[str, Any]:
    recipe = registry["tasks"].get(task_id)
    if recipe is None:
        raise PilotError(f"unsupported task: {task_id}")
    return recipe


def decode_probe(recipe: dict[str, Any]) -> bytes:
    path = FIXTURE_ROOT / _relative_path(recipe["probe_fixture"], "probe fixture")
    encoded = _read_bounded(path, MAX_JSON_BYTES, "trusted probe fixture")
    try:
        data = base64.b64decode(encoded.strip(), validate=True)
    except ValueError as error:
        raise PilotError("trusted probe fixture is malformed base64") from error
    if sha256(data) != recipe["probe_sha256"]:
        raise PilotError("trusted probe hash mismatch")
    return data


def _validate_trusted_contract(recipe: dict[str, Any]) -> bytes:
    path = FIXTURE_ROOT / _relative_path(
        recipe["validator_contract_fixture"], "validator contract fixture"
    )
    data = _read_bounded(path, MAX_JSON_BYTES, "trusted validator contract")
    if sha256(data) != recipe["validator_contract_file_sha256"]:
        raise PilotError("trusted validator contract hash mismatch")
    _json_load(data, "trusted validator contract")
    return data


def parse_cargo_test_output(stdout: str, stderr: str, expected_names: list[str]) -> dict[str, Any]:
    if not expected_names:
        raise PilotError("expected named test set is empty")
    log = stdout + "\n" + stderr
    matches = re.findall(r"^test ([A-Za-z0-9_]+) \.\.\. (ok|FAILED)\s*$", log, re.MULTILINE)
    names = [name for name, _ in matches]
    if len(matches) != len(expected_names) or len(names) != len(set(names)) or set(names) != set(expected_names):
        raise PilotError("Cargo named test output is missing, duplicated, or unexpected")
    if re.search(rf"\brunning {len(expected_names)} tests\b", log) is None:
        raise PilotError("Cargo named test output has a wrong or zero test count")
    passed = sorted(name for name, state in matches if state == "ok")
    failed = sorted(name for name, state in matches if state == "FAILED")
    summary = re.search(
        r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
        r"(\d+) ignored; (\d+) measured; (\d+) filtered out",
        log,
    )
    if summary is None:
        raise PilotError("Cargo test summary is missing")
    counts = tuple(int(value) for value in summary.groups())
    if counts != (len(passed), len(failed), 0, 0, 0):
        raise PilotError("Cargo test summary contains ignored, filtered, or inconsistent counts")
    return {
        "tests": len(expected_names),
        "passed": passed,
        "failed": failed,
        "ignored": 0,
        "measured": 0,
        "filtered_out": 0,
        "runtime_assertion_evidence": "panicked at" in log,
    }


def validate_baseline_control(
    recipe: dict[str, Any],
    exit_code: int,
    observed: dict[str, Any],
    *,
    containment_verified: bool,
) -> None:
    if exit_code != 101:
        raise PilotError("baseline negative control requires Cargo exit 101")
    if not containment_verified:
        raise PilotError("baseline negative control requires successful containment")
    if not observed["runtime_assertion_evidence"]:
        raise PilotError("baseline negative control lacks runtime assertion evidence")
    if set(observed["passed"]) != set(recipe["baseline_passed_names"]) or set(observed["failed"]) != set(recipe["baseline_failed_names"]):
        raise PilotError("baseline named pass/fail partition mismatch")


def evaluate_cargo_attempts(
    attempts: list[dict[str, Any]],
    *,
    expected_passed: set[str],
    expected_failed: set[str],
    expected_exit: int,
) -> dict[str, Any]:
    if not 1 <= len(attempts) <= 2:
        raise PilotError("Cargo target must have one attempt and at most one retry")
    first_argv = attempts[0].get("argv")
    first_cwd = attempts[0].get("cwd")
    for index, attempt in enumerate(attempts):
        observed = attempt.get("observed")
        if observed is None:
            raise PilotError("Cargo attempt lacks exact named runtime results")
        exact = (
            set(observed["passed"]) == expected_passed
            and set(observed["failed"]) == expected_failed
            and attempt.get("exit_code") == expected_exit
        )
        if not exact:
            raise PilotError("Cargo attempt result does not match its frozen partition")
        if index == 0 and len(attempts) == 2:
            failure = attempt.get("transport_failure")
            if (
                not isinstance(failure, str)
                or not failure.startswith("process owner retained ")
                or attempt.get("containment_cleanup_completed") is not True
            ):
                raise PilotError("Cargo retry was not an eligible retained-owned-child failure")
        if index == 1 and (
            attempt.get("argv") != first_argv or attempt.get("cwd") != first_cwd
        ):
            raise PilotError("Cargo retry changed argv or cwd")
    last = attempts[-1]
    if last.get("transport_failure") is not None:
        raise PilotError("final Cargo attempt has a transport failure")
    return {
        "attempts": attempts,
        "observed": last["observed"],
        "successful_transport": True,
    }


def aggregate_omp_messages(messages: list[dict[str, Any]]) -> dict[str, Any]:
    applicable = len(messages)
    providers = sorted({
        message.get("provider")
        for message in messages
        if isinstance(message.get("provider"), str)
    })
    models = sorted({
        message.get("model")
        for message in messages
        if isinstance(message.get("model"), str)
    })
    usage_summary: dict[str, Any] = {}
    for field in USAGE_FIELDS:
        observed = []
        for message in messages:
            usage = message.get("usage")
            value = usage.get(field) if isinstance(usage, dict) else None
            if value is None:
                continue
            if (
                not isinstance(value, int)
                or isinstance(value, bool)
                or value < 0
            ):
                raise PilotError(
                    f"OMP usage field {field} must be a nonnegative integer or null"
                )
            observed.append(value)
        usage_summary[field] = {
            "state": "complete" if len(observed) == applicable else "partial",
            "value": sum(observed) if len(observed) == applicable else None,
            "observed_partial_sum": sum(observed) if observed else None,
            "known_responses": len(observed),
            "applicable_responses": applicable,
        }
    tool_requests = 0
    for message in messages:
        content = message.get("content")
        if isinstance(content, list):
            tool_requests += sum(
                1
                for item in content
                if isinstance(item, dict)
                and item.get("type") in ("toolCall", "tool_call")
            )
    return {
        "completed_assistant_responses": applicable,
        "provider_labels": providers,
        "model_labels": models,
        "usage": usage_summary,
        "tool_requests": tool_requests,
        "cache_counters_separate": True,
        "exact_source_bytes": None,
        "reasoning_use": None,
    }


def validate_direct_acquisition(
    status_before: dict[str, Any],
    status_after: dict[str, Any],
    envelope: dict[str, Any],
    *,
    expected_task_hash: str,
    expected_path_count: int,
) -> None:
    if status_before != status_after:
        raise PilotError("acquisition status changed before/after Governor")
    required_status = {
        "workspace_id", "config_hash", "registered_config_hash",
        "effective_config_hash", "active_generation_id",
        "active_generation_state",
    }
    if not required_status.issubset(status_before):
        raise PilotError("acquisition status omits required identity fields")
    if (
        status_before["active_generation_state"] != "committed"
        or not all(
            isinstance(status_before[field], str) and status_before[field]
            for field in ("workspace_id", "active_generation_id")
        )
        or re.fullmatch(r"[0-9a-f]{64}", status_before["config_hash"]) is None
        or status_before["registered_config_hash"] != status_before["config_hash"]
        or status_before["effective_config_hash"] != status_before["config_hash"]
    ):
        raise PilotError("acquisition status identities are incomplete or inconsistent")
    negotiated = envelope.get("negotiated_versions")
    if not isinstance(negotiated, dict) or (
        negotiated.get("capability_version") != "context-capabilities-v2.0.0"
        or negotiated.get("execution_version") != "context-execution-v2.0.0"
        or negotiated.get("ir_version") != "2.0.0"
        or negotiated.get("route_version") != "context-route-v2.0.0"
        or negotiated.get("decision_version") != "context-route-v2.0.0"
        or negotiated.get("operation_versions")
        != ["query-v1.0.0", "source-reference-v1.0.0"]
    ):
        raise PilotError("Governor negotiation is not the supported V2 envelope")
    execution = envelope.get("execution")
    if not isinstance(execution, dict):
        raise PilotError("Governor execution is missing")
    if (
        execution.get("state") != "completed"
        or execution.get("schema_version") != "context-execution-v2.0.0"
        or execution.get("final_route") != "DIRECT"
        or execution.get("payload") != {"payload_type": "direct_none"}
        or execution.get("materialization") is not None
        or envelope.get("light_results") is not None
        or envelope.get("deep_context_ir") is not None
    ):
        raise PilotError("Governor execution contradicts completed DIRECT/direct_none")
    counter_fields = {
        "atlas_calls", "records", "source_bytes", "estimated_tokens", "work_units"
    }
    counters = execution.get("counters")
    if (
        not isinstance(counters, dict)
        or set(counters) != counter_fields
        or any(
            not isinstance(value, int)
            or isinstance(value, bool)
            or value < 0
            or value != 0
            for value in counters.values()
        )
    ):
        raise PilotError("Governor execution contains malformed or nonzero Atlas counters")
    attempts = execution.get("attempts")
    if not isinstance(attempts, list) or len(attempts) != 1:
        raise PilotError("Governor DIRECT execution must contain one attempt")
    attempt = attempts[0]
    attempt_counters = attempt.get("counters")
    if (
        attempt.get("route") != "DIRECT"
        or attempt.get("state") != "completed"
        or not isinstance(attempt_counters, dict)
        or set(attempt_counters) != counter_fields
        or any(
            not isinstance(value, int)
            or isinstance(value, bool)
            or value < 0
            or value != 0
            for value in attempt_counters.values()
        )
    ):
        raise PilotError("Governor attempt contradicts DIRECT zero-context execution")
    decision = execution.get("decision")
    if not isinstance(decision, dict) or decision.get("schema_version") != "context-route-v2.0.0":
        raise PilotError("Governor decision schema is incompatible")
    request = decision.get("request")
    seeds = request.get("explicit_seeds") if isinstance(request, dict) else None
    if (
        not isinstance(request, dict)
        or request.get("schema_version") != "context-route-v2.0.0"
        or request.get("normalized_task_hash") != expected_task_hash
        or request.get("declared_kind") is not None
        or request.get("atlas_intent") != "none"
        or request.get("route_floor") != "DIRECT"
        or request.get("route_ceiling") != "DIRECT"
        or request.get("deep_budget") is not None
        or not isinstance(seeds, dict)
        or seeds.get("path_count") != expected_path_count
        or seeds.get("symbol_count") != 0
    ):
        raise PilotError("Governor decision request contradicts the frozen DIRECT task")
    starting_generation = decision.get("starting_generation")
    if (
        not isinstance(starting_generation, dict)
        or starting_generation.get("state") != "observed"
        or starting_generation.get("generation_id")
        != status_before["active_generation_id"]
    ):
        raise PilotError("Governor decision generation disagrees with stable status")


def validate_capabilities(capabilities: dict[str, Any]) -> None:
    availability = capabilities.get("availability")
    expected_versions = {
        "schema_version": "context-capabilities-v2.0.0",
        "supported_route_policy_versions": ["context-route-v2.0.0"],
        "supported_decision_versions": ["context-route-v2.0.0"],
        "supported_execution_versions": ["context-execution-v2.0.0"],
        "supported_ir_versions": ["2.0.0"],
        "supported_operation_versions": [
            "query-v1.0.0",
            "source-reference-v1.0.0",
        ],
    }
    if (
        not isinstance(capabilities, dict)
        or any(
            capabilities.get(field) != expected
            for field, expected in expected_versions.items()
        )
        or not isinstance(availability, dict)
        or availability.get("route_decision") != "available"
    ):
        raise PilotError(
            "Governor capabilities do not advertise the supported V2 route"
        )


def file_dependency(root: Path, path: Path, role: str) -> dict[str, Any]:
    absolute_root = root.absolute()
    absolute = path.absolute()
    try:
        relative = absolute.relative_to(absolute_root).as_posix()
    except ValueError as error:
        raise PilotError(f"dependency escapes evidence root: {role}") from error
    data = _read_bounded(absolute, MAX_JSON_BYTES, role)
    return {"role": role, "path": relative, "bytes": len(data), "sha256": sha256(data)}


def _load_json_file(path: Path, role: str, maximum: int = MAX_JSON_BYTES) -> dict[str, Any]:
    value = _json_load(_read_bounded(path, maximum, role), role)
    if not isinstance(value, dict):
        raise PilotError(f"{role} must be an object")
    return value


def verify_executed_receipt(
    manifest_path: Path,
    receipt_path: Path,
    evidence_root: Path,
    *,
    _allow_synthetic_test_fixture: bool = False,
) -> dict[str, Any]:
    manifest = _load_manifest(
        manifest_path, allow_synthetic=_allow_synthetic_test_fixture
    )
    manifest_evidence_root = _safe_existing(
        _manifest_path(manifest, "evidence_root"),
        "manifest evidence root",
        "directory",
    )
    supplied_evidence_root = _safe_existing(
        evidence_root, "receipt evidence root", "directory"
    )
    if supplied_evidence_root != manifest_evidence_root:
        raise PilotError(
            "receipt evidence root differs from the frozen manifest"
        )
    receipt = _load_json_file(receipt_path, "executed receipt")
    _verify_seal(manifest, "manifest_sha256", "run manifest")
    _verify_seal(receipt, "receipt_sha256", "executed receipt")
    if manifest["synthetic"] is not _allow_synthetic_test_fixture:
        raise PilotError(
            "executed receipt manifest synthetic state is not authorized"
        )
    _exact(
        receipt,
        {
            "schema_version", "kind", "synthetic", "manifest_sha256", "outcome",
            "execution", "dependencies", "receipt_sha256",
        },
        "executed receipt",
    )
    if receipt["schema_version"] != RECEIPT_VERSION or receipt["kind"] != "atlas-context-pilot-executed-receipt":
        raise PilotError("executed receipt schema is incompatible")
    if receipt["synthetic"] is not _allow_synthetic_test_fixture:
        raise PilotError("executed receipt synthetic state is not authorized")
    if receipt["manifest_sha256"] != manifest["manifest_sha256"]:
        raise PilotError("executed receipt manifest binding mismatch")
    if receipt["outcome"] not in ("accepted", "rejected"):
        raise PilotError("executed receipt outcome is unsupported")
    execution = _exact(
        receipt["execution"],
        {
            "task_id", "acquisition", "baseline", "model",
            "candidate_acceptance", "focused_regression",
            "named_acceptance_results", "named_regression_results",
            "final_fences", "fence_records",
            "validation_execution_verified", "atlas_benefit",
            "actual_useful_context_throughput", "reasoning_use",
        },
        "executed receipt execution",
    )
    if execution["validation_execution_verified"] is not (
        receipt["outcome"] == "accepted"
    ):
        raise PilotError("executed receipt outcome contradicts validation status")
    dependencies = receipt["dependencies"]
    if (
        not isinstance(dependencies, list)
        or not dependencies
        or len(dependencies) > MAX_DEPENDENCIES
    ):
        raise PilotError("executed receipt dependencies are invalid")
    seen: set[tuple[str, str]] = set()
    for dependency in dependencies:
        item = _exact(dependency, {"role", "path", "bytes", "sha256"}, "receipt dependency")
        key = (
            _string(item["role"], "dependency role"),
            _relative_path(item["path"], "dependency path"),
        )
        if key in seen:
            raise PilotError("executed receipt dependency is duplicated")
        seen.add(key)
        path = evidence_root / item["path"]
        data = _read_bounded(path, MAX_SESSION_BYTES, f"receipt dependency {item['role']}")
        if len(data) != item["bytes"] or sha256(data) != item["sha256"]:
            raise PilotError(f"dependency digest mismatch: {item['role']}")
    _verify_manifest_dependencies(manifest)
    dependency_paths = {item["path"] for item in dependencies}
    mandatory_paths = {
        "run-manifest.json", "acquisition-result.json",
        "baseline-result.json", "model-result.json",
        "candidate-source.bin", "candidate.patch",
        "frozen-instruction.txt", "frozen-model-configuration.json",
        "frozen-agent-configuration.json", "model-prompt.md",
        "governor-request.json",
    }
    if not mandatory_paths.issubset(dependency_paths):
        raise PilotError(
            f"executed receipt omits mandatory artifacts: "
            f"{sorted(mandatory_paths - dependency_paths)!r}"
        )
    acquisition = validate_acquisition_artifact(
        manifest,
        _load_json_file(
            evidence_root / "acquisition-result.json",
            "acquisition artifact",
        ),
    )
    baseline = validate_baseline_artifact(
        manifest,
        _load_json_file(
            evidence_root / "baseline-result.json", "baseline artifact"
        ),
    )
    model = validate_model_artifact(
        manifest,
        _load_json_file(
            evidence_root / "model-result.json", "model artifact"
        ),
    )
    if (
        execution["task_id"] != manifest["task_id"]
        or execution["acquisition"] != "acquisition-result.json"
        or execution["baseline"] != baseline
        or execution["model"] != model
        or execution["atlas_benefit"] is not None
        or execution["actual_useful_context_throughput"] is not None
        or execution["reasoning_use"] is not None
    ):
        raise PilotError("executed receipt phase chain does not reconstruct")
    recipe = manifest["recipe"]
    accepted = receipt["outcome"] == "accepted"
    if accepted:
        acceptance = _validate_cargo_result(
            execution["candidate_acceptance"],
            names=recipe["acceptance_names"],
            expected_passed=set(recipe["acceptance_names"]),
            expected_failed=set(),
            expected_exit=0,
            evidence_root=evidence_root,
            dependency_prefix="candidate-acceptance",
            expected_argv=fixed_cargo_argv(
                manifest,
                "validation_checkout",
                recipe["acceptance_target"],
            ),
            expected_cwd=_manifest_path(manifest, "validation_checkout"),
            manifest=manifest,
            checkout_name="validation_checkout",
            allowed_modified=set(recipe["candidate_source_allowlist"]),
            allowed_untracked={recipe["probe_injection_path"]},
        )
        regression = _validate_cargo_result(
            execution["focused_regression"],
            names=recipe["regression_names"],
            expected_passed=set(recipe["regression_names"]),
            expected_failed=set(),
            expected_exit=0,
            evidence_root=evidence_root,
            dependency_prefix="candidate-regression",
            expected_argv=fixed_cargo_argv(
                manifest,
                "validation_checkout",
                recipe["regression_target"],
            ),
            expected_cwd=_manifest_path(manifest, "validation_checkout"),
            manifest=manifest,
            checkout_name="validation_checkout",
            allowed_modified=set(recipe["candidate_source_allowlist"]),
            allowed_untracked={recipe["probe_injection_path"]},
        )
        if (
            execution["named_acceptance_results"] != acceptance["observed"]
            or execution["named_regression_results"] != regression["observed"]
        ):
            raise PilotError("executed receipt named results are fabricated")
    else:
        if execution["validation_execution_verified"] is not False:
            raise PilotError("rejected receipt claims completed validation")
        partial_acceptance = execution["candidate_acceptance"]
        if partial_acceptance is None:
            if execution["named_acceptance_results"] is not None:
                raise PilotError("rejected receipt fabricates acceptance results")
        else:
            reconstructed_acceptance = _validate_cargo_result(
                partial_acceptance,
                names=recipe["acceptance_names"],
                expected_passed=set(recipe["acceptance_names"]),
                expected_failed=set(),
                expected_exit=0,
                evidence_root=evidence_root,
                dependency_prefix="candidate-acceptance",
                expected_argv=fixed_cargo_argv(
                    manifest,
                    "validation_checkout",
                    recipe["acceptance_target"],
                ),
                expected_cwd=_manifest_path(
                    manifest, "validation_checkout"
                ),
                manifest=manifest,
                checkout_name="validation_checkout",
                allowed_modified=set(recipe["candidate_source_allowlist"]),
                allowed_untracked={recipe["probe_injection_path"]},
            )
            if (
                execution["named_acceptance_results"]
                != reconstructed_acceptance["observed"]
            ):
                raise PilotError(
                    "rejected receipt acceptance results are fabricated"
                )
        if (
            execution["focused_regression"] is not None
            or execution["named_regression_results"] is not None
        ):
            raise PilotError("rejected receipt contains completed regression")
        if "validation-functional-rejection.json" not in dependency_paths:
            raise PilotError("rejected receipt omits functional rejection artifact")
        rejection = _load_json_file(
            evidence_root / "validation-functional-rejection.json",
            "functional rejection artifact",
        )
        _exact(
            rejection,
            {
                "schema_version", "task_id", "manifest_sha256",
                "detail", "outcome",
            },
            "functional rejection artifact",
        )
        if (
            rejection["schema_version"]
            != "atlas-context-pilot-functional-rejection-v1.0.0"
            or rejection["task_id"] != manifest["task_id"]
            or rejection["manifest_sha256"] != manifest["manifest_sha256"]
            or rejection["outcome"] != "rejected"
        ):
            raise PilotError("functional rejection artifact binding mismatch")
    validate_fence_sequence(
        execution["fence_records"],
        manifest,
        "validation_checkout",
        [
            (set(), set()),
            (
                set(recipe["candidate_source_allowlist"]),
                {recipe["probe_injection_path"]},
            ),
            (
                set(recipe["candidate_source_allowlist"]),
                {recipe["probe_injection_path"]},
            ),
        ],
    )
    fences = _exact(
        execution["final_fences"],
        {
            "candidate_source_sha256", "trusted_probe_sha256",
            "allowlisted_modified", "allowlisted_untracked",
        },
        "executed receipt final fences",
    )
    if (
        fences["candidate_source_sha256"] != model["candidate_source_sha256"]
        or fences["trusted_probe_sha256"] != recipe["probe_sha256"]
        or fences["allowlisted_modified"] != recipe["candidate_source_allowlist"]
        or fences["allowlisted_untracked"] != [recipe["probe_injection_path"]]
    ):
        raise PilotError("executed receipt final fences do not reconstruct")
    return receipt


def _write_exclusive(path: Path, data: bytes, role: str) -> None:
    if path.exists() or path.is_symlink():
        raise PilotError(f"{role} already exists")
    try:
        _EXPORTER._write_exclusive(path, data)
    except (_EXPORTER.ExportError, OSError) as error:
        raise PilotError(f"could not write {role}") from error


def _process_record(
    argv: list[str],
    cwd: Path,
    *,
    environment: dict[str, str],
    timeout: int,
    output_limit: int,
    process_runner: Callable[..., Any] | None = None,
) -> tuple[Any, dict[str, Any]]:
    runner = process_runner or PROCESS_GUARD.bounded_run
    started = time.perf_counter_ns()
    transport_failure = None
    failure_kind = None
    cleanup_completed = True
    try:
        result = runner(
            argv,
            environment=environment,
            cwd=cwd,
            ok=ALL_RETURN_CODES,
            timeout=timeout,
            output_limit=output_limit,
        )
    except PROCESS_GUARD.BoundedProcessError as error:
        result = error.result
        transport_failure = str(error)
        failure_kind = "bounded_transport"
    except Exception as error:
        result = subprocess.CompletedProcess(argv, None, "", "")
        transport_failure = f"{type(error).__name__}: {error}"
        failure_kind = "launch_or_stream"
        cleanup_completed = False
    duration_us = (time.perf_counter_ns() - started) // 1000
    stdout_bytes = result.stdout.encode("utf-8", errors="replace")
    stderr_bytes = result.stderr.encode("utf-8", errors="replace")
    record = {
        "argv": argv,
        "resolved_argv": result.args,
        "cwd": str(cwd),
        "exit_code": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "stdout_sha256": sha256(stdout_bytes),
        "stderr_sha256": sha256(stderr_bytes),
        "duration_us": duration_us,
        "transport_failure": transport_failure,
        "failure_kind": failure_kind,
        "containment_cleanup_completed": cleanup_completed,
        "stream_encoding": "UTF-8 decoded transport output re-encoded for retention",
    }
    return result, record


PROCESS_RECORD_FIELDS = {
    "argv", "resolved_argv", "cwd", "exit_code", "stdout", "stderr",
    "stdout_sha256", "stderr_sha256", "duration_us", "transport_failure",
    "failure_kind", "containment_cleanup_completed", "stream_encoding",
}


def _command_path_key(value: str) -> str:
    path = Path(value)
    if path.is_absolute():
        return os.path.normcase(os.path.normpath(str(path.absolute())))
    return os.path.normcase(value.replace("\\", "/"))


def reconstruct_process_record(
    record: Any,
    *,
    expected_argv: list[str],
    expected_cwd: Path,
    stdout: str | None = None,
    stderr: str | None = None,
) -> dict[str, Any]:
    if not isinstance(record, dict):
        raise PilotError("captured process record must be an object")
    allowed = PROCESS_RECORD_FIELDS | ({"name", "checkout"} & set(record))
    _exact(record, allowed, "captured process record")
    if (
        record["argv"] != expected_argv
        or _command_path_key(record["cwd"])
        != _command_path_key(str(expected_cwd))
    ):
        raise PilotError("captured process argv or cwd contradicts the fixed command")
    if (
        not isinstance(record["resolved_argv"], list)
        or len(record["resolved_argv"]) != len(expected_argv)
        or record["resolved_argv"][1:] != expected_argv[1:]
        or _command_path_key(record["resolved_argv"][0])
        != _command_path_key(expected_argv[0])
    ):
        raise PilotError("captured resolved argv contradicts the frozen executable")
    if record["exit_code"] is not None and (
        not isinstance(record["exit_code"], int)
        or isinstance(record["exit_code"], bool)
    ):
        raise PilotError("captured return code is not an integer or null")
    if (
        not isinstance(record["duration_us"], int)
        or isinstance(record["duration_us"], bool)
        or record["duration_us"] < 0
    ):
        raise PilotError("captured duration is invalid")
    if not isinstance(record["containment_cleanup_completed"], bool):
        raise PilotError("captured containment cleanup state is invalid")
    if record["transport_failure"] is None:
        if (
            record["failure_kind"] is not None
            or record["containment_cleanup_completed"] is not True
        ):
            raise PilotError(
                "successful capture has inconsistent failure or cleanup state"
            )
    elif not isinstance(record["failure_kind"], str):
        raise PilotError("failed capture lacks a typed failure kind")
    if record["stream_encoding"] != "UTF-8 decoded transport output re-encoded for retention":
        raise PilotError("captured stream encoding disclosure is invalid")
    captured_stdout = record["stdout"] if stdout is None else stdout
    captured_stderr = record["stderr"] if stderr is None else stderr
    if not isinstance(captured_stdout, str) or not isinstance(captured_stderr, str):
        raise PilotError("captured process streams must be decoded strings")
    if (
        sha256(captured_stdout.encode("utf-8", errors="replace"))
        != record["stdout_sha256"]
        or sha256(captured_stderr.encode("utf-8", errors="replace"))
        != record["stderr_sha256"]
    ):
        raise PilotError("captured process stream hash mismatch")
    if stdout is not None and record.get("stdout") not in (None, stdout):
        raise PilotError("captured stdout contradicts retained stream")
    if stderr is not None and record.get("stderr") not in (None, stderr):
        raise PilotError("captured stderr contradicts retained stream")
    return record


def reconstruct_attempt_capture(
    evidence_root: Path,
    prefix: str,
    attempt_number: int,
    artifact_record: dict[str, Any],
    *,
    expected_argv: list[str],
    expected_cwd: Path,
) -> tuple[str, str, dict[str, Any]]:
    stem = f"{prefix}-attempt-{attempt_number}"
    stdout = _read_bounded(
        evidence_root / f"{stem}.stdout.txt",
        MAX_STREAM_BYTES,
        f"{stem} stdout",
    ).decode("utf-8", errors="replace")
    stderr = _read_bounded(
        evidence_root / f"{stem}.stderr.txt",
        MAX_STREAM_BYTES,
        f"{stem} stderr",
    ).decode("utf-8", errors="replace")
    retained = _load_json_file(
        evidence_root / f"{stem}.execution.json",
        f"{stem} execution",
    )
    expected_record = {
        key: value
        for key, value in artifact_record.items()
        if key != "observed"
    }
    if retained != expected_record:
        raise PilotError("Cargo artifact record differs from retained execution capture")
    expanded = {**retained, "stdout": stdout, "stderr": stderr}
    reconstruct_process_record(
        expanded,
        expected_argv=expected_argv,
        expected_cwd=expected_cwd,
    )
    return stdout, stderr, retained


def _capture_attempt(
    evidence_root: Path,
    name: str,
    argv: list[str],
    cwd: Path,
    *,
    environment: dict[str, str],
    timeout: int,
    output_limit: int = MAX_STREAM_BYTES,
    process_runner: Callable[..., Any] | None = None,
) -> tuple[Any, dict[str, Any], list[dict[str, Any]]]:
    result, record = _process_record(
        argv,
        cwd,
        environment=environment,
        timeout=timeout,
        output_limit=output_limit,
        process_runner=process_runner,
    )
    stdout_path = evidence_root / f"{name}.stdout.txt"
    stderr_path = evidence_root / f"{name}.stderr.txt"
    record_path = evidence_root / f"{name}.execution.json"
    _write_exclusive(
        stdout_path,
        result.stdout.encode("utf-8", errors="replace"),
        f"{name} stdout",
    )
    _write_exclusive(
        stderr_path,
        result.stderr.encode("utf-8", errors="replace"),
        f"{name} stderr",
    )
    retained = {
        key: value
        for key, value in record.items()
        if key not in ("stdout", "stderr")
    }
    _write_exclusive(
        record_path,
        canonical_document_bytes(retained),
        f"{name} execution record",
    )
    dependencies = [
        file_dependency(evidence_root, stdout_path, f"{name}_stdout"),
        file_dependency(evidence_root, stderr_path, f"{name}_stderr"),
        file_dependency(evidence_root, record_path, f"{name}_execution"),
    ]
    return result, retained, dependencies


def reconstruct_named_capture(
    evidence_root: Path,
    name: str,
    *,
    expected_argv: list[str],
    expected_cwd: Path,
    json_output: bool,
) -> tuple[Any, dict[str, Any]]:
    stdout = _read_bounded(
        evidence_root / f"{name}.stdout.txt",
        MAX_STREAM_BYTES,
        f"{name} stdout",
    ).decode("utf-8", errors="replace")
    stderr = _read_bounded(
        evidence_root / f"{name}.stderr.txt",
        MAX_STREAM_BYTES,
        f"{name} stderr",
    ).decode("utf-8", errors="replace")
    retained = _load_json_file(
        evidence_root / f"{name}.execution.json",
        f"{name} execution",
    )
    expanded = {**retained, "stdout": stdout, "stderr": stderr}
    reconstruct_process_record(
        expanded,
        expected_argv=expected_argv,
        expected_cwd=expected_cwd,
    )
    if retained["transport_failure"] is not None or retained["exit_code"] != 0:
        raise PilotError(f"{name} capture is not a successful contained process")
    if not json_output:
        return stdout, retained
    value = _json_load(
        stdout.encode("utf-8", errors="replace"),
        f"{name} retained stdout",
    )
    if not isinstance(value, dict):
        raise PilotError(f"{name} retained stdout must be a JSON object")
    return value, retained


def _git(
    executable: str,
    cwd: Path,
    args: list[str],
    environment: dict[str, str],
    *,
    process_runner: Callable[..., Any] | None = None,
) -> tuple[str, dict[str, Any]]:
    result, record = _process_record(
        [executable, *args],
        cwd,
        environment=environment,
        timeout=60,
        output_limit=1024 * 1024,
        process_runner=process_runner,
    )
    if result.returncode != 0 or record["transport_failure"] is not None:
        raise PilotError(f"Git preflight failed: {' '.join(args)}")
    return result.stdout.rstrip("\r\n"), record


def _validate_declared_configuration(data: bytes, kind: str) -> dict[str, Any]:
    value = _json_load(data, f"{kind} configuration")
    document = _exact(value, {kind}, f"{kind} configuration")
    configured = document[kind]
    if configured is not None:
        _string(configured, f"{kind} configuration value", 256)
    return document


GIT_FENCE_COMMANDS = {
    "head": ["--no-optional-locks", "rev-parse", "HEAD"],
    "branch": ["--no-optional-locks", "branch", "--show-current"],
    "porcelain": [
        "--no-optional-locks", "status", "--porcelain=v1",
        "--untracked-files=all",
    ],
    "diff": ["--no-optional-locks", "diff", "--name-only"],
    "cached": [
        "--no-optional-locks", "diff", "--cached", "--name-only"
    ],
    "untracked": [
        "--no-optional-locks", "ls-files", "--others",
        "--exclude-standard",
    ],
    "ignored": [
        "--no-optional-locks", "status", "--porcelain=v1",
        "--ignored=matching", "--untracked-files=all",
    ],
}


def _git_fence_snapshot(
    executable: str,
    cwd: Path,
    environment: dict[str, str],
    *,
    process_runner: Callable[..., Any] | None = None,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    records = []
    outputs = {}
    commands = GIT_FENCE_COMMANDS
    for name, args in commands.items():
        output, record = _git(
            executable, cwd, args, environment, process_runner=process_runner
        )
        outputs[name] = output
        record["name"] = name
        records.append(record)
    return {
        "head": outputs["head"],
        "branch": outputs["branch"],
        "porcelain": outputs["porcelain"],
        "diff_names": outputs["diff"].splitlines() if outputs["diff"] else [],
        "cached_names": outputs["cached"].splitlines() if outputs["cached"] else [],
        "untracked_names": outputs["untracked"].splitlines() if outputs["untracked"] else [],
        "ignored_porcelain": outputs["ignored"],
    }, records

def reconstruct_git_fence(
    records: Any,
    *,
    git_executable: str,
    checkout: Path,
    checkout_name: str,
    allowed_modified: set[str],
    allowed_untracked: set[str],
) -> dict[str, Any]:
    if (
        not isinstance(records, list)
        or len(records) != len(GIT_FENCE_COMMANDS)
    ):
        raise PilotError(
            "Git fence must contain the complete command record set"
        )
    by_name = {}
    outputs = {}
    for record in records:
        if record.get("checkout") != checkout_name:
            raise PilotError("Git fence checkout binding mismatch")
        name = record.get("name")
        if name not in GIT_FENCE_COMMANDS or name in by_name:
            raise PilotError("Git fence command set is duplicated or unknown")
        reconstruct_process_record(
            record,
            expected_argv=[git_executable, *GIT_FENCE_COMMANDS[name]],
            expected_cwd=checkout,
        )
        if (
            record["exit_code"] != 0
            or record["transport_failure"] is not None
            or record["containment_cleanup_completed"] is not True
        ):
            raise PilotError("Git fence command did not complete successfully")
        by_name[name] = record
        outputs[name] = record["stdout"].rstrip("\r\n")
    if set(by_name) != set(GIT_FENCE_COMMANDS):
        raise PilotError("Git fence command set is incomplete")
    snapshot = {
        "head": outputs["head"],
        "branch": outputs["branch"],
        "porcelain": outputs["porcelain"],
        "diff_names": outputs["diff"].splitlines() if outputs["diff"] else [],
        "cached_names": outputs["cached"].splitlines() if outputs["cached"] else [],
        "untracked_names": (
            outputs["untracked"].splitlines() if outputs["untracked"] else []
        ),
        "ignored_porcelain": outputs["ignored"],
    }
    validate_git_fence(
        **snapshot,
        allowed_modified=allowed_modified,
        allowed_untracked=allowed_untracked,
    )
    return snapshot

def validate_fence_sequence(
    records: Any,
    manifest: dict[str, Any],
    checkout_name: str,
    allowed_states: list[tuple[set[str], set[str]]],
) -> None:
    command_count = len(GIT_FENCE_COMMANDS)
    if (
        not isinstance(records, list)
        or len(records) != command_count * len(allowed_states)
    ):
        raise PilotError("Git fence sequence has a wrong capture count")
    for index, (allowed_modified, allowed_untracked) in enumerate(
        allowed_states
    ):
        reconstruct_git_fence(
            records[
                index * command_count : (index + 1) * command_count
            ],
            git_executable=manifest["executables"]["git"]["path"],
            checkout=_manifest_path(manifest, checkout_name),
            checkout_name=checkout_name,
            allowed_modified=allowed_modified,
            allowed_untracked=allowed_untracked,
        )




def _hash_paths(root: Path, mapping: dict[str, str], role: str) -> None:
    for relative, expected in mapping.items():
        path = _checkout_member(
            root,
            relative,
            f"{role} {relative}",
            must_exist=True,
        )
        data = _read_bounded(path, MAX_STREAM_BYTES, f"{role} {relative}")
        if sha256(data) != expected:
            raise PilotError(f"{role} hash mismatch: {relative}")


def _protected_lockfile_identity(
    checkout: Path,
    recipe: dict[str, Any],
    checkout_name: str,
) -> dict[str, Any]:
    if checkout_name not in FROZEN_CHECKOUT_ROLES:
        raise PilotError(f"unsupported lockfile checkout role: {checkout_name}")
    contract = recipe["protected_lockfile_contract"]
    path = _checkout_member(
        checkout,
        contract["path"],
        f"{checkout_name} protected lockfile",
        must_exist=True,
    )
    data = _read_bounded(
        path, MAX_STREAM_BYTES, f"{checkout_name} protected lockfile"
    )
    physical_sha256 = sha256(data)
    if (
        len(data) == contract["canonical_bytes"]
        and physical_sha256 == contract["canonical_blob_sha256"]
        and b"\r" not in data
    ):
        representation = "canonical_lf"
    else:
        normalized = data.replace(b"\r\n", b"\n")
        if (
            b"\r" in normalized
            or len(normalized) != contract["canonical_bytes"]
            or sha256(normalized) != contract["canonical_blob_sha256"]
            or data != normalized.replace(b"\n", b"\r\n")
        ):
            raise PilotError(
                f"{checkout_name} protected lockfile representation is unsupported"
            )
        representation = "deterministic_crlf"
    if representation not in contract["allowed_checkout_representations"]:
        raise PilotError(
            f"{checkout_name} protected lockfile representation is not allowed"
        )
    return {
        "checkout": checkout_name,
        "path": str(path),
        "representation": representation,
        "bytes": len(data),
        "sha256": physical_sha256,
        "canonical_blob_sha256": contract["canonical_blob_sha256"],
    }


def _verify_frozen_lockfile(
    manifest: dict[str, Any], checkout_name: str
) -> None:
    observed = _protected_lockfile_identity(
        _manifest_path(manifest, checkout_name),
        manifest["recipe"],
        checkout_name,
    )
    if observed != manifest["protected_lockfiles"][checkout_name]:
        raise PilotError(
            f"{checkout_name} protected lockfile changed after freeze"
        )




def _bounded_private_text_file(path: Path, role: str) -> bytes:
    data = _read_bounded(path, MAX_SAFE_TEXT_BYTES, role)
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise PilotError(f"{role} must be UTF-8") from error
    if "\0" in text:
        raise PilotError(f"{role} must not contain NUL")
    if re.search(
        r"(?i)(?:(?:password|secret|api[-_]?key|access[-_]?token|"
        r"auth[-_]?token|client[-_]?secret|private[-_]?key|bearer)"
        r"\s*[:=]|-----BEGIN [A-Z ]*PRIVATE KEY-----)",
        text,
    ):
        raise PilotError(f"{role} appears to contain credentials")
    return data


def _safe_public_text_file(path: Path, role: str) -> bytes:
    data = _bounded_private_text_file(path, role)
    reason = PUBLIC_HYGIENE.find_forbidden_content(data)
    if reason is not None:
        raise PilotError(f"{role} contains unsafe public content: {reason}")
    return data


def _model_prompt(recipe: dict[str, Any], checkout: Path, instruction: bytes) -> bytes:
    outcomes = "\n".join(f"- {item}" for item in recipe["required_outcomes"])
    text = (
        instruction.decode("utf-8")
        + "\n\n# Frozen repository benchmark: "
        + recipe["candidate_source"]
        + "\n\n"
        + f"Candidate checkout: {checkout}\nBase commit: {FROZEN_COMMIT}\n"
        + f"Task: {recipe['specification']}\n\nRequired outcomes:\n{outcomes}\n\n"
        + f"Change only {recipe['candidate_source']}. Inline tests in that file are allowed. "
        "Read/search/edit/write only. Do not run shells, builds, tests, Atlas, MCP, other models, "
        "workers, network, Git mutation, or OS/hardware configuration. Root independently validates "
        "captured source bytes. Do not inspect validators, baseline logs, evidence, or sibling checkouts. "
        "Do not claim acceptance or tests. Finish with one concise JSON object containing task_id, "
        "changed_files, result_state, explanation, and tests_you_ran=[] .\n"
    )
    return text.encode("utf-8")


def _governor_request(recipe: dict[str, Any]) -> dict[str, Any]:
    return {
        "supported_versions": {
            "capability_versions": ["context-capabilities-v2.0.0"],
            "route_versions": ["context-route-v2.0.0"],
            "decision_versions": ["context-route-v2.0.0"],
            "execution_versions": ["context-execution-v2.0.0"],
            "ir_versions": ["2.0.0"],
            "operation_versions": ["query-v1.0.0", "source-reference-v1.0.0"],
        },
        "semantic": {
            "task": recipe["specification"],
            "declared_kind": None,
            "path_targets": sorted(recipe["acquisition_source_sha256"]),
            "symbol_targets": [],
            "caller_capabilities": {},
            "atlas_intent": "none",
            "route_floor": "DIRECT",
            "route_ceiling": "DIRECT",
            "legacy_task_session_id": None,
            "deep_limits": None,
        },
    }


def _environment_snapshot(environment: dict[str, str]) -> dict[str, Any]:
    keys = (
        "PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "LANG",
        "HOME", "USERPROFILE", "CARGO_HOME", "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR", "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET", "RUSTC",
        "RUSTDOC", "RUSTDOCFLAGS", "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER", "CARGO", "CC", "CXX", "OMP_CONFIG",
        "OMP_PROVIDER", "OMP_MODEL",
    )
    return {key: environment.get(key) for key in keys}


def _absolute_environment_root(
    environment: dict[str, Any],
    variable: str,
    default_name: str,
) -> Path:
    value = environment.get(variable)
    if value:
        path = Path(value)
        if not path.is_absolute():
            raise PilotError(f"{variable} must be absolute for replay")
        return path.absolute()
    home_value = environment.get("USERPROFILE") or environment.get("HOME")
    home = Path(home_value) if home_value else Path.home()
    if not home.is_absolute():
        raise PilotError("qualified home directory must be absolute")
    return (home / default_name).absolute()


def _external_configuration_candidates(
    environment: dict[str, Any],
    cargo_cwds: dict[str, Path],
) -> list[dict[str, Any]]:
    if set(cargo_cwds) != {
        "baseline_checkout",
        "validation_checkout",
    }:
        raise PilotError("Cargo configuration roots are incomplete")
    roots = {
        name: _safe_existing(path, f"{name} Cargo root", "directory")
        for name, path in cargo_cwds.items()
    }
    if _overlap(
        roots["baseline_checkout"], roots["validation_checkout"]
    ):
        raise PilotError("Cargo configuration roots overlap")
    candidates: dict[str, dict[str, Any]] = {}

    def add(path: Path, kind: str, source: str) -> None:
        absolute = path.absolute()
        key = _command_path_key(str(absolute))
        item = candidates.setdefault(
            key,
            {
                "kind": kind,
                "path": str(absolute),
                "sources": set(),
            },
        )
        if item["kind"] != kind:
            raise PilotError("external configuration path kind is ambiguous")
        item["sources"].add(source)

    for name, root in roots.items():
        for ancestor in (root, *root.parents):
            add(
                ancestor / ".cargo" / "config",
                "cargo_config",
                f"{name}_hierarchy",
            )
            add(
                ancestor / ".cargo" / "config.toml",
                "cargo_config",
                f"{name}_hierarchy",
            )
    cargo_home = _absolute_environment_root(
        environment, "CARGO_HOME", ".cargo"
    )
    add(cargo_home / "config", "cargo_config", "cargo_home")
    add(cargo_home / "config.toml", "cargo_config", "cargo_home")
    rustup_home = _absolute_environment_root(
        environment, "RUSTUP_HOME", ".rustup"
    )
    add(
        rustup_home / "settings.toml",
        "rustup_settings",
        "rustup_home",
    )
    return [
        {
            "kind": item["kind"],
            "path": item["path"],
            "sources": sorted(item["sources"]),
        }
        for _, item in sorted(candidates.items())
    ]


def _validate_absent_path_ancestry(path: Path, role: str) -> None:
    current = path.parent
    while True:
        if current.exists() or current.is_symlink():
            _safe_metadata(current, role, "directory")
        parent = current.parent
        if parent == current:
            return
        current = parent


def _external_build_configuration(
    environment: dict[str, Any],
    cargo_cwds: dict[str, Path],
) -> list[dict[str, Any]]:
    observations = []
    for candidate in _external_configuration_candidates(
        environment, cargo_cwds
    ):
        path = Path(candidate["path"])
        if path.exists() or path.is_symlink():
            absolute = _safe_existing(
                path, "external build configuration", "file"
            )
            data = _read_bounded(
                absolute,
                MAX_SAFE_TEXT_BYTES,
                "external build configuration",
            )
            if candidate["kind"] == "cargo_config":
                try:
                    parsed = tomllib.loads(data.decode("utf-8"))
                except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                    raise PilotError(
                        "Cargo configuration is not valid UTF-8 TOML"
                    ) from error
                if "include" in parsed:
                    raise PilotError(
                        "Cargo configuration include is unsupported; "
                        "transitive build inputs cannot be frozen"
                    )
            observations.append({
                **candidate,
                "exists": True,
                "bytes": len(data),
                "sha256": sha256(data),
            })
        else:
            _validate_absent_path_ancestry(
                path, "external build configuration ancestry"
            )
            observations.append({
                **candidate,
                "exists": False,
                "bytes": None,
                "sha256": None,
            })
    return observations


def _executable_identity(path: Path, role: str) -> dict[str, Any]:
    absolute = _safe_existing(path, role, "file")
    data = _read_bounded(absolute, MAX_EXECUTABLE_BYTES, role)
    return {"path": str(absolute), "bytes": len(data), "sha256": sha256(data)}


def _manifest_fields() -> set[str]:
    return {
        "schema_version", "kind", "synthetic", "task_id", "frozen_commit",
        "registry", "recipe", "paths", "executables", "configuration",
        "environment", "cache", "prompt", "acquisition_request",
        "protected_lockfiles", "preflight_records", "dependencies",
        "manifest_sha256",
    }


def _load_manifest(
    path: Path, *, allow_synthetic: bool = False
) -> dict[str, Any]:
    manifest = _load_json_file(path, "run manifest")
    _exact(manifest, _manifest_fields(), "run manifest")
    _verify_seal(manifest, "manifest_sha256", "run manifest")
    if (
        manifest["schema_version"] != MANIFEST_VERSION
        or manifest["kind"] != "atlas-context-pilot-run-manifest"
        or (
            manifest["synthetic"] is not False
            and not (allow_synthetic and manifest["synthetic"] is True)
        )
        or manifest["frozen_commit"] != FROZEN_COMMIT
    ):
        raise PilotError("run manifest schema or frozen identity is incompatible")
    registry = load_registry()
    recipe = trusted_recipe(registry, manifest["task_id"])
    if manifest["recipe"] != recipe:
        raise PilotError("run manifest recipe differs from the trusted registry")
    registry_binding = _exact(
        manifest["registry"], {"path", "sha256"}, "run manifest registry"
    )
    if (
        Path(registry_binding["path"]).absolute() != REGISTRY_PATH.absolute()
        or registry_binding["sha256"] != registry["registry_sha256"]
    ):
        raise PilotError("run manifest registry binding mismatch")
    path_fields = {
        "scratch_root", "baseline_checkout", "model_checkout",
        "validation_checkout", "evidence_root", "target_root",
        "trusted_root", "acquisition_checkout", "catalogue",
    }
    paths = _exact(manifest["paths"], path_fields, "run manifest paths")
    validate_path_layout({
        key: Path(paths[key])
        for key in path_fields
        if key != "catalogue"
    })
    _safe_existing(Path(paths["catalogue"]), "manifest catalogue", "file")
    protected_lockfiles = _exact(
        manifest["protected_lockfiles"],
        set(FROZEN_CHECKOUT_ROLES),
        "run manifest protected lockfiles",
    )
    for checkout_name in FROZEN_CHECKOUT_ROLES:
        identity = _exact(
            protected_lockfiles[checkout_name],
            {
                "checkout", "path", "representation", "bytes", "sha256",
                "canonical_blob_sha256",
            },
            f"run manifest {checkout_name} protected lockfile",
        )
        if identity != _protected_lockfile_identity(
            Path(paths[checkout_name]),
            recipe,
            checkout_name,
        ):
            raise PilotError(
                f"{checkout_name} protected lockfile changed after freeze"
            )
    executables = _exact(
        manifest["executables"], {"git", "cargo", "atlas", "omp"},
        "run manifest executables",
    )
    for name, identity in executables.items():
        item = _exact(
            identity, {"path", "bytes", "sha256"},
            f"run manifest {name} executable",
        )
        _string(item["path"], f"run manifest {name} executable path")
        if (
            not isinstance(item["bytes"], int)
            or isinstance(item["bytes"], bool)
            or item["bytes"] < 0
        ):
            raise PilotError("run manifest executable byte count is invalid")
        _hash(item["sha256"], f"run manifest {name} executable hash")
    reject_mutable_checkout_authorities(
        {
            "manifest catalogue": Path(paths["catalogue"]),
            **{
                f"manifest {name} executable": Path(identity["path"])
                for name, identity in executables.items()
            },
        },
        [
            Path(paths["baseline_checkout"]),
            Path(paths["model_checkout"]),
            Path(paths["validation_checkout"]),
        ],
    )
    configuration = _exact(
        manifest["configuration"],
        {
            "instruction", "model", "agent", "weights_revision",
            "execution_binding",
        },
        "run manifest configuration",
    )
    if configuration["execution_binding"] != "declared_metadata_only":
        raise PilotError("model/agent metadata must be labelled declared-only")
    for field in ("instruction", "model", "agent"):
        _exact(
            configuration[field], {"path", "bytes", "sha256"},
            f"run manifest configuration {field}",
        )
    if configuration["weights_revision"] is not None:
        _string(
            configuration["weights_revision"],
            "run manifest weights revision",
        )
    environment = _exact(
        manifest["environment"],
        {
            "qualified_inherited_toolchain", "external_configuration",
            "mutated_by_runner",
        },
        "run manifest environment",
    )
    if environment["mutated_by_runner"] is not False:
        raise PilotError("run manifest claims runner-mutated environment")
    cache = _exact(
        manifest["cache"],
        {
            "state", "sharing", "cold_latency_claim", "role_targets",
        },
        "run manifest cache",
    )
    role_targets = _exact(
        cache["role_targets"],
        set(CARGO_TARGET_CHILDREN),
        "run manifest Cargo role targets",
    )
    if (
        cache["state"] != "fresh_empty"
        or cache["sharing"] != "role_isolated"
        or cache["cold_latency_claim"] is not False
    ):
        raise PilotError("run manifest cache contract is invalid")
    for role, path in role_targets.items():
        _string(path, f"run manifest {role} Cargo target path")
    for role in CARGO_TARGET_CHILDREN:
        _cargo_target_path(manifest, role)
    frozen_environment = environment["qualified_inherited_toolchain"]
    expected_environment_fields = set(_environment_snapshot({}))
    _exact(
        frozen_environment,
        expected_environment_fields,
        "run manifest qualified environment",
    )
    if any(
        value is not None and not isinstance(value, str)
        for value in frozen_environment.values()
    ):
        raise PilotError("run manifest qualified environment value is invalid")
    external_configuration = environment["external_configuration"]
    if not isinstance(external_configuration, list):
        raise PilotError("run manifest external configuration must be an array")
    expected_candidates = _external_configuration_candidates(
        frozen_environment,
        {
            "baseline_checkout": Path(paths["baseline_checkout"]),
            "validation_checkout": Path(paths["validation_checkout"]),
        },
    )
    observed_candidates = []
    for identity in external_configuration:
        item = _exact(
            identity,
            {"kind", "path", "sources", "exists", "bytes", "sha256"},
            "external build configuration",
        )
        _string(item["path"], "external configuration path")
        _string_array(item["sources"], "external configuration sources")
        if item["kind"] not in ("cargo_config", "rustup_settings"):
            raise PilotError("external configuration kind is unsupported")
        if not isinstance(item["exists"], bool):
            raise PilotError("external configuration existence is invalid")
        if item["exists"]:
            if (
                not isinstance(item["bytes"], int)
                or isinstance(item["bytes"], bool)
                or item["bytes"] < 0
            ):
                raise PilotError("external configuration byte count is invalid")
            _hash(item["sha256"], "external configuration hash")
        elif item["bytes"] is not None or item["sha256"] is not None:
            raise PilotError(
                "absent external configuration carries file identity"
            )
        observed_candidates.append({
            "kind": item["kind"],
            "path": item["path"],
            "sources": item["sources"],
        })
    if observed_candidates != expected_candidates:
        raise PilotError(
            "external configuration candidate paths are incomplete"
        )
    for field in ("prompt", "acquisition_request"):
        _exact(
            manifest[field], {"path", "bytes", "sha256"},
            f"run manifest {field}",
        )
    preflight = manifest["preflight_records"]
    expected_preflight_count = 4 * len(GIT_FENCE_COMMANDS)
    if (
        not isinstance(preflight, list)
        or len(preflight) != expected_preflight_count
    ):
        raise PilotError(
            "run manifest must retain the complete Git preflight"
        )
    expected_preflight = {
        (checkout, name)
        for checkout in (
            "baseline_checkout", "model_checkout",
            "validation_checkout", "acquisition_checkout",
        )
        for name in GIT_FENCE_COMMANDS
    }
    observed_preflight = set()
    for record in preflight:
        item = _exact(
            record,
            {
                "checkout", "name", "argv", "resolved_argv", "cwd",
                "exit_code", "stdout", "stderr", "stdout_sha256",
                "stderr_sha256", "duration_us", "transport_failure",
                "failure_kind", "containment_cleanup_completed",
                "stream_encoding",
            },
            "Git preflight record",
        )
        key = (item["checkout"], item["name"])
        if key in observed_preflight:
            raise PilotError("Git preflight record is duplicated")
        observed_preflight.add(key)
        if (
            item["exit_code"] != 0
            or item["transport_failure"] is not None
            or item["containment_cleanup_completed"] is not True
        ):
            raise PilotError("Git preflight record is unsuccessful")
    if observed_preflight != expected_preflight:
        raise PilotError("Git preflight record set is incomplete or unexpected")
    for checkout_name in (
        "baseline_checkout", "model_checkout",
        "validation_checkout", "acquisition_checkout",
    ):
        reconstruct_git_fence(
            [
                record
                for record in preflight
                if record["checkout"] == checkout_name
            ],
            git_executable=executables["git"]["path"],
            checkout=Path(paths[checkout_name]),
            checkout_name=checkout_name,
            allowed_modified=set(),
            allowed_untracked=set(),
        )
    dependencies = manifest["dependencies"]
    if (
        not isinstance(dependencies, list)
        or len(dependencies) > MAX_DEPENDENCIES
    ):
        raise PilotError("run manifest dependencies are invalid")
    roles = []
    by_role = {}
    for dependency in dependencies:
        item = _exact(
            dependency, {"role", "path", "bytes", "sha256"},
            "run manifest dependency",
        )
        role = _string(item["role"], "manifest dependency role")
        roles.append(role)
        by_role[role] = item
    if len(roles) != len(set(roles)):
        raise PilotError("run manifest dependency roles must be unique")
    required_roles = {
        "registry", "validator_contract", "trusted_probe_fixture_encoded",
        "runner", "process_guard", "qualification_helper",
        "exporter_helper", "hygiene_helper", "catalogue", "instruction",
        "model_configuration", "agent_configuration", "prompt",
        "acquisition_request", "git_executable", "cargo_executable",
        "atlas_executable", "omp_executable",
    }
    if set(roles) != required_roles:
        raise PilotError("run manifest dependency roles are incomplete or unexpected")
    authoritative_paths = {
        "registry": REGISTRY_PATH,
        "validator_contract":
            FIXTURE_ROOT / recipe["validator_contract_fixture"],
        "trusted_probe_fixture_encoded":
            FIXTURE_ROOT / recipe["probe_fixture"],
        "runner": Path(__file__),
        "process_guard": Path(__file__).with_name("provider-smoke.py"),
        "qualification_helper":
            Path(__file__).with_name("qualify-context-evidence.py"),
        "exporter_helper":
            Path(__file__).with_name("benchmark-evidence-export.py"),
        "hygiene_helper":
            Path(__file__).with_name("check-public-hygiene.py"),
        "catalogue": Path(paths["catalogue"]),
    }
    for role, path in authoritative_paths.items():
        if Path(by_role[role]["path"]).absolute() != path.absolute():
            raise PilotError(
                f"run manifest authoritative dependency path mismatch: {role}"
            )
    nested_bindings = {
        "instruction": configuration["instruction"],
        "model_configuration": configuration["model"],
        "agent_configuration": configuration["agent"],
        "prompt": manifest["prompt"],
        "acquisition_request": manifest["acquisition_request"],
    }
    for role, identity in nested_bindings.items():
        dependency = by_role[role]
        if any(dependency[field] != identity[field] for field in ("path", "bytes", "sha256")):
            raise PilotError(f"run manifest nested binding mismatch: {role}")
    for name, identity in executables.items():
        dependency = by_role[f"{name}_executable"]
        if any(dependency[field] != identity[field] for field in ("path", "bytes", "sha256")):
            raise PilotError(f"run manifest executable dependency mismatch: {name}")
    return manifest


def _manifest_path(manifest: dict[str, Any], name: str) -> Path:
    return Path(manifest["paths"][name])


def _manifest_environment(manifest: dict[str, Any]) -> dict[str, str]:
    current = dict(os.environ)
    observed = _environment_snapshot(current)
    frozen = manifest["environment"]["qualified_inherited_toolchain"]
    if observed != frozen:
        changed = sorted(
            key for key in set(observed) | set(frozen)
            if observed.get(key) != frozen.get(key)
        )
        raise PilotError(
            f"qualified toolchain environment drifted: {changed!r}"
        )
    observed_external = _external_build_configuration(
        current,
        {
            "baseline_checkout":
                _manifest_path(manifest, "baseline_checkout"),
            "validation_checkout":
                _manifest_path(manifest, "validation_checkout"),
        },
    )
    frozen_external = manifest["environment"]["external_configuration"]
    if observed_external != frozen_external:
        raise PilotError(
            "external Cargo/Rust configuration set changed after freeze"
        )
    return current


def _manifest_dependency(path: Path, role: str) -> dict[str, Any]:
    data = _read_bounded(path, MAX_EXECUTABLE_BYTES, role)
    return {"role": role, "path": str(path), "bytes": len(data), "sha256": sha256(data)}


def _verify_manifest_dependencies(manifest: dict[str, Any]) -> None:
    for dependency in manifest["dependencies"]:
        item = _exact(dependency, {"role", "path", "bytes", "sha256"}, "manifest dependency")
        path = Path(item["path"])
        data = _read_bounded(path, MAX_EXECUTABLE_BYTES, f"manifest dependency {item['role']}")
        if len(data) != item["bytes"] or sha256(data) != item["sha256"]:
            raise PilotError(f"manifest dependency changed: {item['role']}")


def freeze_manifest(args: argparse.Namespace, *, process_runner: Callable[..., Any] | None = None) -> tuple[dict[str, Any], Path]:
    registry = load_registry()
    recipe = trusted_recipe(registry, args.task_id)
    probe = decode_probe(recipe)
    contract = _validate_trusted_contract(recipe)
    paths = validate_path_layout({
        "scratch_root": args.scratch_root,
        "baseline_checkout": args.baseline_checkout,
        "model_checkout": args.model_checkout,
        "validation_checkout": args.validation_checkout,
        "evidence_root": args.evidence_root,
        "target_root": args.target_root,
        "trusted_root": FIXTURE_ROOT,
        "acquisition_checkout": args.acquisition_checkout,
    })
    evidence_root = paths["evidence_root"]
    if any(evidence_root.iterdir()):
        raise PilotError("evidence root must be pre-created and empty")
    if any(paths["target_root"].iterdir()):
        raise PilotError("Cargo target container must be fresh and empty")
    catalogue = _safe_existing(args.catalogue, "catalogue", "file")
    instruction = _bounded_private_text_file(
        args.instruction_file, "OMP instruction file"
    )
    if sha256(instruction) != args.instruction_sha256:
        raise PilotError("OMP instruction identity mismatch")
    model_configuration = _bounded_private_text_file(
        args.model_configuration, "model configuration"
    )
    agent_configuration = _bounded_private_text_file(
        args.agent_configuration, "agent configuration"
    )
    _validate_declared_configuration(model_configuration, "model")
    _validate_declared_configuration(agent_configuration, "agent")
    executables = {
        name: _executable_identity(getattr(args, f"{name}_executable"), f"{name} executable")
        for name in ("git", "cargo", "atlas", "omp")
    }
    reject_mutable_checkout_authorities(
        {
            "catalogue": catalogue,
            **{
                f"{name} executable": Path(identity["path"])
                for name, identity in executables.items()
            },
        },
        [
            paths["baseline_checkout"],
            paths["model_checkout"],
            paths["validation_checkout"],
        ],
    )
    environment = dict(os.environ)
    external_configuration = _external_build_configuration(
        environment,
        {
            "baseline_checkout": paths["baseline_checkout"],
            "validation_checkout": paths["validation_checkout"],
        },
    )
    preflight_records = []
    protected_lockfiles = {}
    for name in FROZEN_CHECKOUT_ROLES:
        snapshot, records = _git_fence_snapshot(
            executables["git"]["path"], paths[name], environment,
            process_runner=process_runner,
        )
        validate_git_fence(
            **snapshot, allowed_modified=set(), allowed_untracked=set()
        )
        preflight_records.extend({"checkout": name, **record} for record in records)
        _hash_paths(paths[name], recipe["protected_file_sha256"], f"{name} protected file")
        _hash_paths(
            paths[name],
            {recipe["candidate_source"]: recipe["candidate_source_sha256"]},
            f"{name} frozen candidate source",
        )
        protected_lockfiles[name] = _protected_lockfile_identity(
            paths[name], recipe, name
        )
    _hash_paths(
        paths["acquisition_checkout"],
        recipe["acquisition_source_sha256"],
        "acquisition source",
    )
    cargo_targets = _reserve_cargo_targets(paths["target_root"])
    prompt = _model_prompt(recipe, paths["model_checkout"], instruction)
    request = _governor_request(recipe)
    frozen_files = {
        "instruction": ("frozen-instruction.txt", instruction),
        "model_configuration": ("frozen-model-configuration.json", model_configuration),
        "agent_configuration": ("frozen-agent-configuration.json", agent_configuration),
        "prompt": ("model-prompt.md", prompt),
        "acquisition_request": ("governor-request.json", canonical_document_bytes(request)),
    }
    dependencies = [
        _manifest_dependency(REGISTRY_PATH, "registry"),
        _manifest_dependency(FIXTURE_ROOT / recipe["validator_contract_fixture"], "validator_contract"),
        _manifest_dependency(
            FIXTURE_ROOT / recipe["probe_fixture"],
            "trusted_probe_fixture_encoded",
        ),
        _manifest_dependency(Path(__file__), "runner"),
        _manifest_dependency(Path(__file__).with_name("provider-smoke.py"), "process_guard"),
        _manifest_dependency(
            Path(__file__).with_name("qualify-context-evidence.py"),
            "qualification_helper",
        ),
        _manifest_dependency(
            Path(__file__).with_name("benchmark-evidence-export.py"),
            "exporter_helper",
        ),
        _manifest_dependency(
            Path(__file__).with_name("check-public-hygiene.py"),
            "hygiene_helper",
        ),
        _manifest_dependency(catalogue, "catalogue"),
    ]
    retained = {}
    for role, (name, data) in frozen_files.items():
        path = evidence_root / name
        _write_exclusive(path, data, role)
        retained[role] = {"path": str(path), "bytes": len(data), "sha256": sha256(data)}
        dependencies.append(_manifest_dependency(path, role))
    for name, identity in executables.items():
        dependencies.append({"role": f"{name}_executable", **identity})
    manifest_material = {
        "schema_version": MANIFEST_VERSION,
        "kind": "atlas-context-pilot-run-manifest",
        "synthetic": False,
        "task_id": args.task_id,
        "frozen_commit": FROZEN_COMMIT,
        "registry": {"path": str(REGISTRY_PATH), "sha256": registry["registry_sha256"]},
        "recipe": recipe,
        "paths": {**{key: str(value) for key, value in paths.items()}, "catalogue": str(catalogue)},
        "executables": executables,
        "configuration": {
            "instruction": retained["instruction"],
            "model": retained["model_configuration"],
            "agent": retained["agent_configuration"],
            "weights_revision": None,
            "execution_binding": "declared_metadata_only",
        },
        "environment": {
            "qualified_inherited_toolchain": _environment_snapshot(environment),
            "external_configuration": external_configuration,
            "mutated_by_runner": False,
        },
        "cache": {
            "state": "fresh_empty",
            "sharing": "role_isolated",
            "cold_latency_claim": False,
            "role_targets": {
                role: str(path)
                for role, path in cargo_targets.items()
            },
        },
        "protected_lockfiles": protected_lockfiles,
        "prompt": retained["prompt"],
        "acquisition_request": retained["acquisition_request"],
        "preflight_records": preflight_records,
        "dependencies": dependencies,
    }
    manifest = seal_document(manifest_material, "manifest_sha256")
    path = evidence_root / "run-manifest.json"
    _write_exclusive(path, canonical_document_bytes(manifest), "run manifest")
    return manifest, path


def _checkout_fence(
    manifest: dict[str, Any],
    checkout_name: str,
    *,
    allowed_modified: set[str],
    allowed_untracked: set[str],
    process_runner: Callable[..., Any] | None = None,
) -> list[dict[str, Any]]:
    environment = _manifest_environment(manifest)
    _snapshot, records = _git_fence_snapshot(
        manifest["executables"]["git"]["path"],
        _manifest_path(manifest, checkout_name),
        environment,
        process_runner=process_runner,
    )
    annotated = [{"checkout": checkout_name, **record} for record in records]
    reconstruct_git_fence(
        annotated,
        git_executable=manifest["executables"]["git"]["path"],
        checkout=_manifest_path(manifest, checkout_name),
        checkout_name=checkout_name,
        allowed_modified=allowed_modified,
        allowed_untracked=allowed_untracked,
    )
    return annotated


def _capture_json_command(
    manifest: dict[str, Any],
    name: str,
    argv: list[str],
    cwd: Path,
    *,
    process_runner: Callable[..., Any] | None = None,
) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any]]:
    evidence = _manifest_path(manifest, "evidence_root")
    result, record, dependencies = _capture_attempt(
        evidence, name, argv, cwd,
        environment=_manifest_environment(manifest), timeout=180,
        output_limit=4 * 1024 * 1024, process_runner=process_runner,
    )
    if record["transport_failure"] is not None or result.returncode != 0:
        raise PilotError(f"{name} subprocess failed")
    value = _json_load(result.stdout.encode("utf-8", errors="replace"), name)
    if not isinstance(value, dict):
        raise PilotError(f"{name} output must be a JSON object")
    return value, dependencies, record


def fixed_acquisition_commands(
    manifest: dict[str, Any],
) -> dict[str, list[str]]:
    checkout = _manifest_path(manifest, "acquisition_checkout")
    catalogue = _manifest_path(manifest, "catalogue")
    request = Path(manifest["acquisition_request"]["path"])
    atlas = manifest["executables"]["atlas"]["path"]
    return {
        "capabilities": [
            atlas,
            "governor",
            "capabilities",
            str(checkout),
            "--catalogue",
            str(catalogue),
        ],
        "status-before": [
            atlas,
            "status",
            str(checkout),
            "--catalogue",
            str(catalogue),
        ],
        "governor": [
            atlas,
            "governor",
            "run",
            str(checkout),
            "--request",
            str(request),
            "--catalogue",
            str(catalogue),
        ],
        "status-after": [
            atlas,
            "status",
            str(checkout),
            "--catalogue",
            str(catalogue),
        ],
    }


def run_acquisition(
    manifest: dict[str, Any],
    *,
    process_runner: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    recipe = manifest["recipe"]
    checkout = _manifest_path(manifest, "acquisition_checkout")
    commands = fixed_acquisition_commands(manifest)
    fence_records = _checkout_fence(manifest, "acquisition_checkout", allowed_modified=set(), allowed_untracked=set(), process_runner=process_runner)
    _hash_paths(checkout, recipe["acquisition_source_sha256"], "acquisition source before")
    _verify_frozen_lockfile(manifest, "acquisition_checkout")
    captured = {}
    dependencies = []
    for name, argv in commands.items():
        value, deps, _record = _capture_json_command(
            manifest,
            f"acquisition-{name}",
            argv,
            checkout,
            process_runner=process_runner,
        )
        captured[name] = value
        dependencies.extend(deps)
    capabilities = captured["capabilities"]
    before = captured["status-before"]
    envelope = captured["governor"]
    after = captured["status-after"]
    validate_capabilities(capabilities)
    validate_direct_acquisition(
        before,
        after,
        envelope,
        expected_task_hash=sha256(
            recipe["specification"].strip().lower().encode("utf-8")
        ),
        expected_path_count=len(recipe["acquisition_source_sha256"]),
    )
    _hash_paths(checkout, recipe["acquisition_source_sha256"], "acquisition source after")
    _verify_frozen_lockfile(manifest, "acquisition_checkout")
    fence_records.extend(_checkout_fence(manifest, "acquisition_checkout", allowed_modified=set(), allowed_untracked=set(), process_runner=process_runner))
    result = {
        "schema_version": "atlas-context-pilot-acquisition-v1.0.0",
        "synthetic": manifest["synthetic"],
        "task_id": manifest["task_id"],
        "route": "DIRECT",
        "supplied_packet": None,
        "counters": envelope["execution"]["counters"],
        "status_before": before,
        "status_after": after,
        "literal_capabilities": capabilities,
        "literal_governor": envelope,
        "decision_hash_recomputed": False,
        "automatic_sufficiency": False,
        "atlas_benefit": None,
        "fence_records": fence_records,
        "dependencies": dependencies,
    }
    validate_acquisition_artifact(manifest, result)
    path = _manifest_path(manifest, "evidence_root") / "acquisition-result.json"
    _write_exclusive(path, canonical_document_bytes(result), "acquisition result")
    return result


def _inject_probe(checkout: Path, recipe: dict[str, Any]) -> Path:
    destination = _checkout_member(
        checkout,
        recipe["probe_injection_path"],
        "trusted probe",
        must_exist=False,
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    _write_exclusive(destination, decode_probe(recipe), "trusted probe")
    return destination


def fixed_cargo_argv(
    manifest: dict[str, Any],
    checkout_name: str,
    target: str,
) -> list[str]:
    return [
        manifest["executables"]["cargo"]["path"],
        "test",
        "--locked",
        "--target-dir",
        str(_cargo_target_path(manifest, checkout_name)),
        "--test",
        target,
        "--",
        "--nocapture",
    ]


def _run_cargo_target(
    manifest: dict[str, Any],
    checkout_name: str,
    prefix: str,
    target: str,
    names: list[str],
    expected_passed: set[str],
    expected_failed: set[str],
    expected_exit: int,
    *,
    allowed_modified: set[str],
    expected_candidate_sha256: str,
    allowed_untracked: set[str],
    process_runner: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    checkout = _manifest_path(manifest, checkout_name)
    argv = fixed_cargo_argv(manifest, checkout_name, target)
    attempts = []
    fence_records = []
    dependencies = []
    for attempt_number in (1, 2):
        _verify_manifest_dependencies(manifest)
        fence_records.extend(_checkout_fence(
            manifest,
            checkout_name,
            allowed_modified=allowed_modified,
            allowed_untracked=allowed_untracked,
            process_runner=process_runner,
        ))
        _hash_paths(
            checkout,
            manifest["recipe"]["protected_file_sha256"],
            f"{prefix} protected file",
        )
        _verify_frozen_lockfile(manifest, checkout_name)
        _hash_paths(
            checkout,
            {
                manifest["recipe"]["candidate_source"]:
                expected_candidate_sha256
            },
            f"{prefix} candidate source",
        )
        _hash_paths(
            checkout,
            {
                manifest["recipe"]["probe_injection_path"]:
                manifest["recipe"]["probe_sha256"]
            },
            f"{prefix} trusted probe",
        )
        result, record, deps = _capture_attempt(
            _manifest_path(manifest, "evidence_root"),
            f"{prefix}-attempt-{attempt_number}",
            argv,
            checkout,
            environment=_manifest_environment(manifest),
            timeout=600,
            process_runner=process_runner,
        )
        dependencies.extend(deps)
        try:
            observed = parse_cargo_test_output(
                result.stdout, result.stderr, names
            )
        except PilotError:
            observed = None
        record["observed"] = observed
        attempts.append(record)
        if record["transport_failure"] is None:
            break
        eligible = (
            observed is not None
            and set(observed["passed"]) == expected_passed
            and set(observed["failed"]) == expected_failed
            and result.returncode == expected_exit
            and record["transport_failure"].startswith(
                "process owner retained "
            )
            and record["containment_cleanup_completed"] is True
        )
        if not eligible or attempt_number == 2:
            break
    if attempts[-1]["transport_failure"] is not None:
        raise PilotTransportError(
            f"{prefix} ended with an unsuccessful transport: "
            f"{attempts[-1]['transport_failure']}"
        )
    evaluation = evaluate_cargo_attempts(
        attempts,
        expected_passed=expected_passed,
        expected_failed=expected_failed,
        expected_exit=expected_exit,
    )
    evaluation["fence_records"] = fence_records
    evaluation["dependencies"] = dependencies
    return evaluation


def baseline_phase(
    manifest_path: Path,
    *,
    process_runner: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    manifest = _load_manifest(manifest_path)
    _verify_manifest_dependencies(manifest)
    recipe = manifest["recipe"]
    acquisition = run_acquisition(manifest, process_runner=process_runner)
    checkout = _manifest_path(manifest, "baseline_checkout")
    _require_empty_cargo_target(manifest, "baseline_checkout")
    start_fence_records = _checkout_fence(manifest, "baseline_checkout", allowed_modified=set(), allowed_untracked=set(), process_runner=process_runner)
    probe = _inject_probe(checkout, recipe)
    result = _run_cargo_target(
        manifest, "baseline_checkout", "baseline-control", recipe["acceptance_target"],
        recipe["acceptance_names"], set(recipe["baseline_passed_names"]),
        set(recipe["baseline_failed_names"]), 101,
        allowed_modified=set(), allowed_untracked={recipe["probe_injection_path"]},
        expected_candidate_sha256=recipe["candidate_source_sha256"],
        process_runner=process_runner,
    )
    validate_baseline_control(
        recipe,
        result["attempts"][-1]["exit_code"],
        result["observed"],
        containment_verified=result["successful_transport"],
    )
    _verify_manifest_dependencies(manifest)
    end_fence_records = _checkout_fence(
        manifest,
        "baseline_checkout",
        allowed_modified=set(),
        allowed_untracked={recipe["probe_injection_path"]},
        process_runner=process_runner,
    )
    _hash_paths(
        checkout,
        recipe["protected_file_sha256"],
        "baseline final protected file",
    )
    _verify_frozen_lockfile(manifest, "baseline_checkout")
    _hash_paths(
        checkout,
        {
            recipe["candidate_source"]:
            recipe["candidate_source_sha256"]
        },
        "baseline final candidate source",
    )
    if sha256(probe.read_bytes()) != recipe["probe_sha256"]:
        raise PilotError("baseline trusted probe changed")
    artifact = {
        "schema_version": "atlas-context-pilot-baseline-v1.0.0",
        "synthetic": manifest["synthetic"],
        "task_id": manifest["task_id"],
        "manifest_sha256": manifest["manifest_sha256"],
        "negative_control_satisfied": True,
        "acquisition_path": "acquisition-result.json",
        "cargo": result,
        "start_fence_records": start_fence_records,
        "end_fence_records": end_fence_records,
        "acceptance": None,
    }
    validate_baseline_artifact(manifest, artifact)
    path = _manifest_path(manifest, "evidence_root") / "baseline-result.json"
    _write_exclusive(path, canonical_document_bytes(artifact), "baseline result")
    return artifact


def _verify_retained_dependencies(
    dependencies: Any,
    evidence_root: Path,
    *,
    required_roles: set[str],
    required_paths: dict[str, set[str]] | None = None,
) -> None:
    if not isinstance(dependencies, list) or not dependencies:
        raise PilotError("phase dependencies must be a nonempty array")
    seen = set()
    roles = set()
    paths_by_role: dict[str, set[str]] = {}
    for dependency in dependencies:
        item = _exact(
            dependency,
            {"role", "path", "bytes", "sha256"},
            "phase dependency",
        )
        role = _string(item["role"], "phase dependency role")
        path_value = _relative_path(item["path"], "phase dependency path")
        key = (role, path_value)
        if key in seen:
            raise PilotError("phase dependency is duplicated")
        seen.add(key)
        roles.add(role)
        paths_by_role.setdefault(role, set()).add(path_value)
        path = evidence_root / path_value
        data = _read_bounded(
            path, MAX_SESSION_BYTES, f"phase dependency {role}"
        )
        if len(data) != item["bytes"] or sha256(data) != item["sha256"]:
            raise PilotError(f"phase dependency digest mismatch: {role}")
    if not required_roles.issubset(roles):
        raise PilotError(
            f"phase dependencies omit required roles: "
            f"{sorted(required_roles - roles)!r}"
        )
    if required_paths is not None:
        for role, expected_paths in required_paths.items():
            if paths_by_role.get(role, set()) != expected_paths:
                raise PilotError(
                    f"phase dependency role {role} is bound to wrong paths"
                )



def _validate_cargo_result(
    result: Any,
    *,
    names: list[str],
    expected_passed: set[str],
    expected_failed: set[str],
    expected_exit: int,
    evidence_root: Path,
    dependency_prefix: str,
    expected_argv: list[str],
    expected_cwd: Path,
    manifest: dict[str, Any],
    checkout_name: str,
    allowed_modified: set[str],
    allowed_untracked: set[str],
) -> dict[str, Any]:
    cargo = _exact(
        result,
        {
            "attempts", "observed", "successful_transport",
            "dependencies", "fence_records",
        },
        "Cargo phase result",
    )
    evaluated = evaluate_cargo_attempts(
        cargo["attempts"],
        expected_passed=expected_passed,
        expected_failed=expected_failed,
        expected_exit=expected_exit,
    )
    if (
        cargo["observed"] != evaluated["observed"]
        or cargo["successful_transport"] is not True
    ):
        raise PilotError("Cargo phase summary disagrees with retained attempts")
    for attempt_number, attempt in enumerate(cargo["attempts"], 1):
        stdout, stderr, _retained = reconstruct_attempt_capture(
            evidence_root,
            dependency_prefix,
            attempt_number,
            attempt,
            expected_argv=expected_argv,
            expected_cwd=expected_cwd,
        )
        observed = parse_cargo_test_output(stdout, stderr, names)
        if attempt["observed"] != observed:
            raise PilotError(
                "Cargo observed result is not reconstructed from capture"
            )
    _verify_retained_dependencies(
        cargo["dependencies"],
        evidence_root,
        required_roles={
            f"{dependency_prefix}-attempt-{attempt}_{suffix}"
            for attempt in range(1, len(cargo["attempts"]) + 1)
            for suffix in ("stdout", "stderr", "execution")
        },
        required_paths={
            f"{dependency_prefix}-attempt-{attempt}_{suffix}": {
                f"{dependency_prefix}-attempt-{attempt}.{suffix}.txt"
                if suffix in ("stdout", "stderr")
                else f"{dependency_prefix}-attempt-{attempt}.execution.json"
            }
            for attempt in range(1, len(cargo["attempts"]) + 1)
            for suffix in ("stdout", "stderr", "execution")
        },
    )
    validate_fence_sequence(
        cargo["fence_records"],
        manifest,
        checkout_name,
        [(allowed_modified, allowed_untracked)] * len(cargo["attempts"]),
    )
    return cargo


def validate_acquisition_artifact(
    manifest: dict[str, Any],
    artifact: Any,
) -> dict[str, Any]:
    acquisition = _exact(
        artifact,
        {
            "schema_version", "synthetic", "task_id", "route",
            "supplied_packet", "counters", "status_before", "status_after",
            "literal_capabilities", "literal_governor",
            "decision_hash_recomputed", "automatic_sufficiency",
            "atlas_benefit", "fence_records", "dependencies",
        },
        "acquisition artifact",
    )
    if (
        acquisition["schema_version"]
        != "atlas-context-pilot-acquisition-v1.0.0"
        or acquisition["synthetic"] is not manifest["synthetic"]
        or acquisition["task_id"] != manifest["task_id"]
        or acquisition["route"] != "DIRECT"
        or acquisition["supplied_packet"] is not None
        or acquisition["decision_hash_recomputed"] is not False
        or acquisition["automatic_sufficiency"] is not False
        or acquisition["atlas_benefit"] is not None
    ):
        raise PilotError("acquisition artifact contract mismatch")
    recipe = manifest["recipe"]
    evidence = _manifest_path(manifest, "evidence_root")
    _verify_retained_dependencies(
        acquisition["dependencies"],
        evidence,
        required_roles={
            f"acquisition-{name}_{suffix}"
            for name in (
                "capabilities", "status-before", "governor", "status-after"
            )
            for suffix in ("stdout", "stderr", "execution")
        },
        required_paths={
            f"acquisition-{name}_{suffix}": {
                f"acquisition-{name}.{suffix}.txt"
                if suffix in ("stdout", "stderr")
                else f"acquisition-{name}.execution.json"
            }
            for name in (
                "capabilities", "status-before", "governor", "status-after"
            )
            for suffix in ("stdout", "stderr", "execution")
        },
    )
    checkout = _manifest_path(manifest, "acquisition_checkout")
    reconstructed = {}
    commands = fixed_acquisition_commands(manifest)
    for name, argv in commands.items():
        reconstructed[name], _record = reconstruct_named_capture(
            evidence,
            f"acquisition-{name}",
            expected_argv=argv,
            expected_cwd=checkout,
            json_output=True,
        )
    if (
        reconstructed["capabilities"] != acquisition["literal_capabilities"]
        or reconstructed["status-before"] != acquisition["status_before"]
        or reconstructed["status-after"] != acquisition["status_after"]
        or reconstructed["governor"] != acquisition["literal_governor"]
    ):
        raise PilotError("acquisition artifact differs from retained captures")
    validate_capabilities(reconstructed["capabilities"])
    validate_direct_acquisition(
        reconstructed["status-before"],
        reconstructed["status-after"],
        reconstructed["governor"],
        expected_task_hash=sha256(
            recipe["specification"].strip().lower().encode("utf-8")
        ),
        expected_path_count=len(recipe["acquisition_source_sha256"]),
    )
    if (
        acquisition["counters"]
        != reconstructed["governor"]["execution"]["counters"]
    ):
        raise PilotError("acquisition counter summary disagrees with retained output")
    validate_fence_sequence(
        acquisition["fence_records"],
        manifest,
        "acquisition_checkout",
        [(set(), set()), (set(), set())],
    )
    return acquisition


def validate_baseline_artifact(
    manifest: dict[str, Any],
    artifact: Any,
) -> dict[str, Any]:
    baseline = _exact(
        artifact,
        {
            "schema_version", "synthetic", "task_id", "manifest_sha256",
            "negative_control_satisfied", "acquisition_path", "cargo",
            "start_fence_records", "end_fence_records", "acceptance",
        },
        "baseline artifact",
    )
    if (
        baseline["schema_version"] != "atlas-context-pilot-baseline-v1.0.0"
        or baseline["synthetic"] is not manifest["synthetic"]
        or baseline["task_id"] != manifest["task_id"]
        or baseline["manifest_sha256"] != manifest["manifest_sha256"]
        or baseline["acquisition_path"] != "acquisition-result.json"
        or baseline["acceptance"] is not None
    ):
        raise PilotError("baseline artifact binding mismatch")
    evidence = _manifest_path(manifest, "evidence_root")
    acquisition = _load_json_file(
        evidence / baseline["acquisition_path"], "acquisition artifact"
    )
    validate_acquisition_artifact(manifest, acquisition)
    validate_fence_sequence(
        baseline["start_fence_records"],
        manifest,
        "baseline_checkout",
        [(set(), set())],
    )
    validate_fence_sequence(
        baseline["end_fence_records"],
        manifest,
        "baseline_checkout",
        [
            (
                set(),
                {manifest["recipe"]["probe_injection_path"]},
            )
        ],
    )
    recipe = manifest["recipe"]
    cargo = _validate_cargo_result(
        baseline["cargo"],
        expected_argv=fixed_cargo_argv(
            manifest,
            "baseline_checkout",
            recipe["acceptance_target"],
        ),
        expected_cwd=_manifest_path(manifest, "baseline_checkout"),
        manifest=manifest,
        checkout_name="baseline_checkout",
        allowed_modified=set(),
        allowed_untracked={recipe["probe_injection_path"]},
        names=recipe["acceptance_names"],
        expected_passed=set(recipe["baseline_passed_names"]),
        expected_failed=set(recipe["baseline_failed_names"]),
        expected_exit=101,
        evidence_root=evidence,
        dependency_prefix="baseline-control",
    )
    validate_baseline_control(
        recipe,
        cargo["attempts"][-1]["exit_code"],
        cargo["observed"],
        containment_verified=cargo["successful_transport"],
    )
    if baseline["negative_control_satisfied"] is not True:
        raise PilotError("baseline artifact fabricates negative-control state")
    return baseline


def validate_model_artifact(
    manifest: dict[str, Any],
    artifact: Any,
) -> dict[str, Any]:
    model = _exact(
        artifact,
        {
            "schema_version", "synthetic", "task_id", "manifest_sha256",
            "execution_successful", "execution", "patch_capture",
            "fence_records", "candidate_source_sha256",
            "candidate_patch_sha256", "scope_verified",
            "omp_observations", "acceptance", "dependencies",
        },
        "model artifact",
    )
    recipe = manifest["recipe"]
    if (
        model["schema_version"] != "atlas-context-pilot-model-v1.0.0"
        or model["synthetic"] is not manifest["synthetic"]
        or model["task_id"] != manifest["task_id"]
        or model["manifest_sha256"] != manifest["manifest_sha256"]
        or model["execution_successful"] is not True
        or model["execution"].get("exit_code") != 0
        or model["execution"].get("transport_failure") is not None
        or model["scope_verified"] != recipe["candidate_source_allowlist"]
        or model["acceptance"] is not None
    ):
        raise PilotError("model artifact binding or execution mismatch")
    expected_argv = [
        manifest["executables"]["omp"]["path"], "-p",
        "--session-dir", str(_manifest_path(manifest, "evidence_root") / "model-sessions"),
        "--mode=json", "--no-title", "--no-extensions", "--no-lsp",
        "--no-pty", "--tools=read,grep,glob,edit,write",
        "--approval-mode=yolo", "--max-time=8m",
        f"@{manifest['prompt']['path']}",
    ]
    evidence = _manifest_path(manifest, "evidence_root")
    _verify_retained_dependencies(
        model["dependencies"],
        evidence,
        required_roles={
            "model_stdout", "model_stderr", "model_execution",
            "candidate-patch_stdout", "candidate-patch_stderr",
            "candidate-patch_execution", "candidate_source",
            "candidate_patch", "omp_session_jsonl",
        },
        required_paths={
            "model_stdout": {"model.stdout.txt"},
            "model_stderr": {"model.stderr.txt"},
            "model_execution": {"model.execution.json"},
            "candidate-patch_stdout": {"candidate-patch.stdout.txt"},
            "candidate-patch_stderr": {"candidate-patch.stderr.txt"},
            "candidate-patch_execution": {"candidate-patch.execution.json"},
            "candidate_source": {"candidate-source.bin"},
            "candidate_patch": {"candidate.patch"},
        },
    )
    model_stdout, retained_model = reconstruct_named_capture(
        evidence,
        "model",
        expected_argv=expected_argv,
        expected_cwd=_manifest_path(manifest, "model_checkout"),
        json_output=False,
    )
    if retained_model != model["execution"]:
        raise PilotError("model artifact execution differs from retained capture")
    patch_argv = [
        manifest["executables"]["git"]["path"],
        "--no-optional-locks",
        "diff", "--binary", "--no-ext-diff", "--",
        recipe["candidate_source"],
    ]
    patch_stdout, retained_patch = reconstruct_named_capture(
        evidence,
        "candidate-patch",
        expected_argv=patch_argv,
        expected_cwd=_manifest_path(manifest, "model_checkout"),
        json_output=False,
    )
    if retained_patch != model["patch_capture"]:
        raise PilotError("model patch capture differs from retained execution")
    candidate_source = _read_bounded(
        evidence / "candidate-source.bin",
        MAX_STREAM_BYTES,
        "retained candidate source",
    )
    candidate_patch = _read_bounded(
        evidence / "candidate.patch",
        MAX_STREAM_BYTES,
        "retained candidate patch",
    )
    if (
        patch_stdout.encode("utf-8", errors="replace") != candidate_patch
        or sha256(candidate_source) != model["candidate_source_sha256"]
        or sha256(candidate_patch) != model["candidate_patch_sha256"]
    ):
        raise PilotError("model candidate source or patch binding mismatch")
    observations = reconstruct_omp_observations(
        evidence, model["dependencies"], model_stdout
    )
    if model["omp_observations"] != observations:
        raise PilotError("model observations differ from retained OMP sessions")
    if (
        observations.get("completed_assistant_responses", 0) <= 0
        or not observations.get("provider_labels")
        or not observations.get("model_labels")
    ):
        raise PilotError("model artifact lacks completed provider/model observations")
    validate_fence_sequence(
        model["fence_records"],
        manifest,
        "model_checkout",
        [
            (set(), set()),
            (set(recipe["candidate_source_allowlist"]), set()),
        ],
    )
    _verify_frozen_lockfile(manifest, "model_checkout")
    return model


def validate_phase_chain(
    manifest: dict[str, Any],
    evidence_root: Path,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    acquisition = validate_acquisition_artifact(
        manifest,
        _load_json_file(
            evidence_root / "acquisition-result.json",
            "acquisition artifact",
        ),
    )
    baseline = validate_baseline_artifact(
        manifest,
        _load_json_file(
            evidence_root / "baseline-result.json",
            "baseline artifact",
        ),
    )
    model = validate_model_artifact(
        manifest,
        _load_json_file(
            evidence_root / "model-result.json",
            "model artifact",
        ),
    )
    if baseline["acquisition_path"] != "acquisition-result.json":
        raise PilotError("baseline predecessor binding is invalid")
    return acquisition, baseline, model


def _completed_assistant_messages(
    data: bytes,
    *,
    event_type: str,
    role: str,
) -> list[dict[str, Any]]:
    messages = []
    for number, line in enumerate(data.splitlines(), 1):
        if not line:
            raise PilotError(f"{role} JSONL line {number} is empty")
        event = _json_load(line, f"{role} line {number}")
        if not isinstance(event, dict) or event.get("type") != event_type:
            continue
        message = event.get("message")
        if not isinstance(message, dict) or message.get("role") != "assistant":
            continue
        completed_at = message.get("completedAt")
        if (
            message.get("stopReason") not in ("toolUse", "stop")
            or not isinstance(completed_at, int)
            or isinstance(completed_at, bool)
            or completed_at < 0
        ):
            continue
        for field in ("responseId", "provider", "model"):
            _string(message.get(field), f"{role} assistant {field}", 512)
        if not isinstance(message.get("usage"), dict):
            raise PilotError(f"{role} completed assistant usage is missing")
        content = message.get("content")
        if not isinstance(content, list):
            raise PilotError(f"{role} completed assistant content is missing")
        for item in content:
            if not isinstance(item, dict):
                raise PilotError(f"{role} assistant content item is malformed")
            if item.get("type") == "toolCall":
                _string(item.get("id"), f"{role} tool call id", 512)
                _string(item.get("name"), f"{role} tool call name", 512)
        messages.append(message)
    return messages


def _omp_message_projection(message: dict[str, Any]) -> dict[str, Any]:
    usage = message["usage"]
    tools = []
    for item in message["content"]:
        if item.get("type") == "toolCall":
            tools.append({"id": item["id"], "name": item["name"]})
    return {
        "response_id": message["responseId"],
        "provider": message["provider"],
        "model": message["model"],
        "stop_reason": message["stopReason"],
        "completed_at": message["completedAt"],
        "usage": [
            {
                "field": field,
                "present": field in usage,
                "value": usage.get(field),
            }
            for field in USAGE_FIELDS
        ],
        "tools": tools,
    }


def reconcile_omp_stdout(
    session_messages: list[dict[str, Any]], stdout: str
) -> None:
    stdout_messages = _completed_assistant_messages(
        stdout.encode("utf-8", errors="replace"),
        event_type="message_end",
        role="OMP stdout",
    )
    if (
        not stdout_messages
        or [_omp_message_projection(item) for item in session_messages]
        != [_omp_message_projection(item) for item in stdout_messages]
    ):
        raise PilotError(
            "OMP persistent sessions disagree with completed stdout messages"
        )


def _parse_session_messages(session_root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    messages = []
    dependencies = []
    total = 0
    files = sorted(session_root.rglob("*.jsonl"))
    if not files:
        raise PilotError("OMP session produced no JSONL evidence")
    for path in files:
        data = _read_bounded(path, MAX_SESSION_BYTES, "OMP session JSONL")
        total += len(data)
        if total > MAX_SESSION_BYTES:
            raise PilotError("OMP session evidence exceeds aggregate byte bound")
        dependencies.append({
            "role": "omp_session_jsonl",
            "path": path.relative_to(session_root.parent).as_posix(),
            "bytes": len(data),
            "sha256": sha256(data),
        })
        messages.extend(
            _completed_assistant_messages(
                data,
                event_type="message",
                role="OMP persistent session",
            )
        )
    if not messages:
        raise PilotError("OMP session contains no completed assistant messages")
    return messages, dependencies


def reconstruct_omp_observations(
    evidence_root: Path,
    dependencies: list[dict[str, Any]],
    stdout: str,
) -> dict[str, Any]:
    session_paths = [
        item["path"]
        for item in dependencies
        if item.get("role") == "omp_session_jsonl"
    ]
    if not session_paths or len(session_paths) != len(set(session_paths)):
        raise PilotError("model phase session dependencies are missing or duplicated")
    if any(
        not path.startswith("model-sessions/") or not path.endswith(".jsonl")
        for path in session_paths
    ):
        raise PilotError("OMP session dependency path is outside model-sessions")
    messages = []
    total = 0
    for relative in sorted(session_paths):
        path = evidence_root / _relative_path(relative, "OMP session path")
        data = _read_bounded(path, MAX_SESSION_BYTES, "OMP session JSONL")
        total += len(data)
        if total > MAX_SESSION_BYTES:
            raise PilotError("OMP session evidence exceeds aggregate byte bound")
        messages.extend(
            _completed_assistant_messages(
                data,
                event_type="message",
                role="OMP persistent session",
            )
        )
    if not messages:
        raise PilotError("OMP session contains no completed assistant messages")
    reconcile_omp_stdout(messages, stdout)
    return aggregate_omp_messages(messages)


def model_phase(
    manifest_path: Path,
    *,
    process_runner: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    manifest = _load_manifest(manifest_path)
    _verify_manifest_dependencies(manifest)
    evidence = _manifest_path(manifest, "evidence_root")
    baseline = validate_baseline_artifact(
        manifest,
        _load_json_file(evidence / "baseline-result.json", "baseline result"),
    )
    recipe = manifest["recipe"]
    fence_records = _checkout_fence(manifest, "model_checkout", allowed_modified=set(), allowed_untracked=set(), process_runner=process_runner)
    session_root = evidence / "model-sessions"
    session_root.mkdir()
    argv = [
        manifest["executables"]["omp"]["path"], "-p",
        "--session-dir", str(session_root), "--mode=json", "--no-title",
        "--no-extensions", "--no-lsp", "--no-pty",
        "--tools=read,grep,glob,edit,write", "--approval-mode=yolo",
        "--max-time=8m", f"@{manifest['prompt']['path']}",
    ]
    result, record, dependencies = _capture_attempt(
        evidence, "model", argv, _manifest_path(manifest, "model_checkout"),
        environment=_manifest_environment(manifest), timeout=540,
        process_runner=process_runner,
    )
    if record["transport_failure"] is not None or result.returncode != 0:
        artifact = {
            "schema_version": "atlas-context-pilot-model-v1.0.0",
            "synthetic": manifest["synthetic"],
            "task_id": manifest["task_id"],
            "manifest_sha256": manifest["manifest_sha256"],
            "execution_successful": False,
            "execution": record,
            "acceptance": None,
            "dependencies": dependencies,
        }
        _write_exclusive(evidence / "model-result.json", canonical_document_bytes(artifact), "model result")
        raise PilotError("model execution was unsuccessful")
    fence_records.extend(_checkout_fence(
        manifest, "model_checkout",
        allowed_modified=set(recipe["candidate_source_allowlist"]),
        allowed_untracked=set(), process_runner=process_runner,
    ))
    checkout = _manifest_path(manifest, "model_checkout")
    source_path = _checkout_member(
        checkout,
        recipe["candidate_source"],
        "model candidate source",
        must_exist=True,
    )
    source = _read_bounded(source_path, MAX_STREAM_BYTES, "candidate source")
    candidate_path = evidence / "candidate-source.bin"
    _write_exclusive(candidate_path, source, "candidate source")
    patch_result, patch_record, patch_dependencies = _capture_attempt(
        evidence,
        "candidate-patch",
        [manifest["executables"]["git"]["path"], "--no-optional-locks", "diff", "--binary", "--no-ext-diff", "--", recipe["candidate_source"]],
        checkout,
        environment=_manifest_environment(manifest),
        timeout=60,
        output_limit=MAX_STREAM_BYTES,
        process_runner=process_runner,
    )
    if patch_result.returncode != 0 or patch_record["transport_failure"] is not None:
        raise PilotError("candidate patch capture failed")
    patch_path = evidence / "candidate.patch"
    _write_exclusive(patch_path, patch_result.stdout.encode("utf-8", errors="replace"), "candidate patch")
    messages, session_dependencies = _parse_session_messages(session_root)
    reconcile_omp_stdout(messages, result.stdout)
    dependencies.extend(session_dependencies)
    dependencies.extend(patch_dependencies)
    dependencies.extend([
        file_dependency(evidence, candidate_path, "candidate_source"),
        file_dependency(evidence, patch_path, "candidate_patch"),
    ])
    artifact = {
        "schema_version": "atlas-context-pilot-model-v1.0.0",
        "synthetic": manifest["synthetic"],
        "task_id": manifest["task_id"],
        "manifest_sha256": manifest["manifest_sha256"],
        "execution_successful": True,
        "execution": record,
        "patch_capture": patch_record,
        "fence_records": fence_records,
        "candidate_source_sha256": sha256(source),
        "candidate_patch_sha256": sha256(patch_path.read_bytes()),
        "scope_verified": recipe["candidate_source_allowlist"],
        "omp_observations": aggregate_omp_messages(messages),
        "acceptance": None,
        "dependencies": dependencies,
    }
    validate_model_artifact(manifest, artifact)
    _write_exclusive(evidence / "model-result.json", canonical_document_bytes(artifact), "model result")
    return artifact


def _retained_evidence_files(evidence: Path) -> list[Path]:
    root = _safe_existing(evidence, "receipt evidence root", "directory")
    files = []
    for directory, child_directories, child_files in os.walk(
        root, followlinks=False
    ):
        current = Path(directory)
        _safe_metadata(current, "receipt evidence directory", "directory")
        for name in child_directories:
            _safe_metadata(
                current / name,
                "receipt evidence directory",
                "directory",
            )
        for name in child_files:
            path = current / name
            _safe_metadata(path, "receipt evidence file", "file")
            files.append(path)
            if len(files) > MAX_DEPENDENCIES:
                raise PilotError("receipt dependency count exceeds bound")
    return sorted(files)


def _receipt_dependencies(evidence: Path) -> list[dict[str, Any]]:
    dependencies = []
    excluded = {"executed-receipt.json"}
    for path in (
        item
        for item in _retained_evidence_files(evidence)
        if item.name not in excluded
    ):
        relative = path.relative_to(evidence).as_posix()
        data = _read_bounded(path, MAX_SESSION_BYTES, f"receipt dependency {relative}")
        dependencies.append({"role": "retained_artifact", "path": relative, "bytes": len(data), "sha256": sha256(data)})
        if len(dependencies) > MAX_DEPENDENCIES:
            raise PilotError("receipt dependency count exceeds bound")
    return dependencies


def validate_phase(
    manifest_path: Path,
    *,
    process_runner: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    manifest = _load_manifest(manifest_path)
    _verify_manifest_dependencies(manifest)
    if manifest["synthetic"]:
        raise PilotError("synthetic manifest cannot mint an executed receipt")
    evidence = _manifest_path(manifest, "evidence_root")
    _acquisition, baseline, model = validate_phase_chain(manifest, evidence)
    recipe = manifest["recipe"]
    checkout = _manifest_path(manifest, "validation_checkout")
    _require_empty_cargo_target(manifest, "validation_checkout")
    validation_fence_records = _checkout_fence(manifest, "validation_checkout", allowed_modified=set(), allowed_untracked=set(), process_runner=process_runner)
    candidate_source = _read_bounded(evidence / "candidate-source.bin", MAX_STREAM_BYTES, "candidate source")
    if sha256(candidate_source) != model["candidate_source_sha256"]:
        raise PilotError("candidate source dependency mismatch")
    destination = _checkout_member(
        checkout,
        recipe["candidate_source"],
        "candidate validation source",
        must_exist=True,
    )
    destination.write_bytes(candidate_source)
    probe = _inject_probe(checkout, recipe)
    allowed_modified = set(recipe["candidate_source_allowlist"])
    allowed_untracked = {recipe["probe_injection_path"]}
    validation_fence_records.extend(_checkout_fence(
        manifest, "validation_checkout",
        allowed_modified=allowed_modified, allowed_untracked=allowed_untracked,
        process_runner=process_runner,
    ))
    acceptance = None
    regression = None
    outcome = "rejected"
    try:
        acceptance = _run_cargo_target(
            manifest, "validation_checkout", "candidate-acceptance",
            recipe["acceptance_target"], recipe["acceptance_names"],
            set(recipe["acceptance_names"]), set(), 0,
            allowed_modified=allowed_modified, allowed_untracked=allowed_untracked,
            expected_candidate_sha256=model["candidate_source_sha256"],
            process_runner=process_runner,
        )
        regression = _run_cargo_target(
            manifest, "validation_checkout", "candidate-regression",
            recipe["regression_target"], recipe["regression_names"],
            set(recipe["regression_names"]), set(), 0,
            allowed_modified=allowed_modified, allowed_untracked=allowed_untracked,
            expected_candidate_sha256=model["candidate_source_sha256"],
            process_runner=process_runner,
        )
        outcome = "accepted"
    except PilotTransportError as error:
        unsuccessful = {
            "schema_version": "atlas-context-pilot-validation-unsuccessful-v1.0.0",
            "task_id": manifest["task_id"],
            "manifest_sha256": manifest["manifest_sha256"],
            "failure_kind": "transport",
            "detail": str(error),
            "acceptance": None,
        }
        _write_exclusive(
            evidence / "validation-unsuccessful.json",
            canonical_document_bytes(unsuccessful),
            "validation unsuccessful result",
        )
        raise
    except PilotError as error:
        functional_rejection = {
            "schema_version": "atlas-context-pilot-functional-rejection-v1.0.0",
            "task_id": manifest["task_id"],
            "manifest_sha256": manifest["manifest_sha256"],
            "detail": str(error),
            "outcome": "rejected",
        }
        _write_exclusive(
            evidence / "validation-functional-rejection.json",
            canonical_document_bytes(functional_rejection),
            "validation functional rejection",
        )
        outcome = "rejected"
    _verify_manifest_dependencies(manifest)
    validation_fence_records.extend(_checkout_fence(
        manifest, "validation_checkout",
        allowed_modified=allowed_modified, allowed_untracked=allowed_untracked,
        process_runner=process_runner,
    ))
    _verify_frozen_lockfile(manifest, "validation_checkout")
    if sha256(destination.read_bytes()) != model["candidate_source_sha256"] or sha256(probe.read_bytes()) != recipe["probe_sha256"]:
        raise PilotError("final candidate source or trusted probe fence changed")
    execution = {
        "task_id": manifest["task_id"],
        "acquisition": "acquisition-result.json",
        "baseline": baseline,
        "model": model,
        "candidate_acceptance": acceptance,
        "focused_regression": regression,
        "named_acceptance_results": acceptance["observed"] if acceptance else None,
        "named_regression_results": regression["observed"] if regression else None,
        "final_fences": {
            "candidate_source_sha256": sha256(destination.read_bytes()),
            "trusted_probe_sha256": sha256(probe.read_bytes()),
            "allowlisted_modified": sorted(allowed_modified),
            "allowlisted_untracked": sorted(allowed_untracked),
        },
        "fence_records": validation_fence_records,
        "validation_execution_verified": outcome == "accepted",
        "atlas_benefit": None,
        "actual_useful_context_throughput": None,
        "reasoning_use": None,
    }
    receipt_material = {
        "schema_version": RECEIPT_VERSION,
        "kind": "atlas-context-pilot-executed-receipt",
        "synthetic": False,
        "manifest_sha256": manifest["manifest_sha256"],
        "outcome": outcome,
        "execution": execution,
        "dependencies": _receipt_dependencies(evidence),
    }
    receipt = seal_document(receipt_material, "receipt_sha256")
    _write_exclusive(evidence / "executed-receipt.json", canonical_document_bytes(receipt), "executed receipt")
    return receipt


def _baseline_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser("baseline")
    parser.add_argument("--task-id", required=True)
    for name in ("git", "cargo", "atlas", "omp"):
        parser.add_argument(f"--{name}-executable", type=Path, required=True)
    parser.add_argument("--instruction-file", type=Path, required=True)
    parser.add_argument("--instruction-sha256", required=True)
    parser.add_argument("--model-configuration", type=Path, required=True)
    parser.add_argument("--agent-configuration", type=Path, required=True)
    parser.add_argument("--acquisition-checkout", type=Path, required=True)
    parser.add_argument("--catalogue", type=Path, required=True)
    parser.add_argument("--baseline-checkout", type=Path, required=True)
    parser.add_argument("--model-checkout", type=Path, required=True)
    parser.add_argument("--validation-checkout", type=Path, required=True)
    parser.add_argument("--scratch-root", type=Path, required=True)
    parser.add_argument("--target-root", type=Path, required=True)
    parser.add_argument("--evidence-root", type=Path, required=True)


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="phase", required=True)
    _baseline_parser(subparsers)
    for phase in ("model", "validate"):
        phase_parser = subparsers.add_parser(phase)
        phase_parser.add_argument("--manifest", type=Path, required=True)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--manifest", type=Path, required=True)
    verify_parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args(arguments)
    try:
        if args.phase == "baseline":
            manifest, manifest_path = freeze_manifest(args)
            result = baseline_phase(manifest_path)
            output = {
                "phase": "baseline", "task_id": manifest["task_id"],
                "manifest": str(manifest_path), "negative_control": result["negative_control_satisfied"],
            }
        elif args.phase == "model":
            result = model_phase(args.manifest)
            output = {"phase": "model", "task_id": result["task_id"], "execution_successful": True}
        elif args.phase == "validate":
            result = validate_phase(args.manifest)
            output = {"phase": "validate", "outcome": result["outcome"], "receipt_sha256": result["receipt_sha256"]}
        else:
            evidence = args.receipt.parent
            result = verify_executed_receipt(args.manifest, args.receipt, evidence)
            output = {"phase": "verify", "outcome": result["outcome"], "integrity_verified": True, "validation_executed": False}
    except PilotError as error:
        print(f"context pilot runner error: {error}", file=sys.stderr)
        return 2
    print(json.dumps(output, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
