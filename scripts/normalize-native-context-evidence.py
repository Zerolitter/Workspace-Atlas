#!/usr/bin/env python3
"""Normalize retained native Context IR acquisition evidence without qualifying a task."""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path, PurePosixPath, PureWindowsPath
import sqlite3
import stat
import sys
from typing import Any

ADAPTER_VERSION = "atlas-native-acquisition-adapter-v1.0.0"
DESCRIPTOR_VERSION = "atlas-native-acquisition-descriptor-v1.0.0"
OUTPUT_VERSION = "atlas-native-acquisition-normalization-v1.0.0"
MAX_DESCRIPTOR_BYTES = 1024 * 1024
MAX_JSON_BYTES = 8 * 1024 * 1024
MAX_CATALOGUE_BYTES = 64 * 1024 * 1024
MAX_SOURCE_BYTES = 16 * 1024 * 1024
MAX_AGGREGATE_SOURCE_BYTES = 32 * 1024 * 1024
MAX_OUTPUT_BYTES = 16 * 1024 * 1024
MAX_EVIDENCE = 4096
MAX_STRING = 4096
MAX_SIGNED_63 = (1 << 63) - 1
SHA256 = __import__("re").compile(r"[0-9a-f]{64}\Z")
COMMIT = __import__("re").compile(r"[0-9a-f]{40}\Z")
ENTITY_KINDS = frozenset({"file", "symbol", "config", "test", "document", "external"})
WORKING_SET_STATES = frozenset({"complete", "partial", "blocked"})
TASK_KINDS = frozenset({
    "explore", "bug_fix", "behavior_change", "api_change", "refactor",
    "configuration_change", "test_change", "review", "audit", "unknown",
})
TASK_KIND_SOURCES = frozenset({"declared", "deterministic_rule", "unknown"})
ITEM_ROLES = frozenset({
    "PRIMARY_IMPLEMENTATION", "DIRECT_DEPENDENCY", "DIRECT_DEPENDENT",
    "TYPE_CONTRACT", "TEST_CONTRACT", "CONFIGURATION_INPUT", "EFFECT",
    "HISTORICAL_CONSTRAINT", "UNCERTAINTY", "VALIDATION_TARGET",
    "DOCUMENTATION",
})
SELECTION_REASONS = frozenset({
    "EXPLICIT_SEED", "EXACT_SYMBOL_MATCH", "EXACT_PATH_MATCH", "DIRECT_CALLER",
    "DIRECT_CALLEE", "IMPORT_NEIGHBOR", "IMPLEMENTATION_RELATION",
    "TEST_RELATION", "CONFIG_RELATION", "EFFECT_RELATION",
    "CONFLICT_RELEVANCE", "UNRESOLVED_RELEVANCE",
    "GENERATION_DELTA_RELEVANCE", "TASK_RECIPE_REQUIREMENT",
})
EVIDENCE_STATES = frozenset({
    "verified", "supported", "inferred", "resolved", "ambiguous", "unresolved",
    "external", "stale", "conflicting", "partial", "unsupported", "excluded",
})
VERIFICATION_STATES = frozenset({
    "not_requested", "verified", "hash_mismatch", "read_failed", "excluded",
})
NOTICE_SEVERITIES = frozenset({"info", "warning", "error"})
CLAIM_STRENGTHS = frozenset({"none", "bounded", "exhaustive_within_scope"})
VALIDATION_KINDS = frozenset({
    "test", "build", "lint", "source_verify", "manual_review", "other",
})
EFFECT_PHASES = frozenset({"direct", "transitive", "declared", "observed"})
RESOLUTION_STATES = frozenset({
    "resolved_symbol", "resolved_file", "external", "ambiguous", "unresolved",
    "invalid", "stale",
})


class AdapterError(RuntimeError):
    """Retained evidence cannot satisfy the private adapter contract."""


def _load_qualification() -> Any:
    path = Path(__file__).with_name("qualify-context-evidence.py")
    spec = importlib.util.spec_from_file_location("atlas_native_adapter_qualification", path)
    if spec is None or spec.loader is None:
        raise AdapterError("qualification safety helpers are unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


_QUALIFICATION = _load_qualification()
_EXPORTER = _QUALIFICATION._EXPORTER


def _compact_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise AdapterError("canonical JSON contains an unsupported value") from error


def _document_bytes(value: Any) -> bytes:
    return _compact_bytes(value) + b"\n"


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _exact(value: Any, fields: set[str] | frozenset[str], role: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(fields):
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise AdapterError(
            f"{role} fields must be exactly {sorted(fields)!r}; got {actual!r}"
        )
    return value


def _string(value: Any, role: str, maximum: int = MAX_STRING) -> str:
    if (
        not isinstance(value, str)
        or not value
        or "\0" in value
        or len(value.encode("utf-8")) > maximum
    ):
        raise AdapterError(f"{role} must be a nonempty bounded string")
    return value


def _optional_string(value: Any, role: str, maximum: int = MAX_STRING) -> str | None:
    if value is None:
        return None
    return _string(value, role, maximum)


def _integer(value: Any, role: str, *, nullable: bool = False) -> int | None:
    if value is None and nullable:
        return None
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or value < 0
        or value > MAX_SIGNED_63
    ):
        raise AdapterError(f"{role} must be a bounded nonnegative integer")
    return value


def _number(value: Any, role: str) -> int | float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value < 0
        or value > MAX_SIGNED_63
    ):
        raise AdapterError(f"{role} must be a bounded nonnegative finite number")
    return value


def _boolean(value: Any, role: str) -> bool:
    if not isinstance(value, bool):
        raise AdapterError(f"{role} must be a boolean")
    return value


def _hash(value: Any, role: str) -> str:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        raise AdapterError(f"{role} must be a lowercase SHA-256")
    return value


def _commit(value: Any, role: str) -> str:
    if not isinstance(value, str) or COMMIT.fullmatch(value) is None:
        raise AdapterError(f"{role} must be a lowercase 40-character Git commit")
    return value


def _relative_path(value: Any, role: str) -> str:
    path = _string(value, role, 512)
    posix = PurePosixPath(path)
    windows = PureWindowsPath(path)
    components = path.split("/")
    if (
        "\\" in path
        or ":" in path
        or posix.is_absolute()
        or bool(windows.drive)
        or bool(windows.root)
        or any(component in ("", ".", "..") for component in components)
        or posix.as_posix() != path
    ):
        raise AdapterError(f"{role} must be a canonical relative path")
    return path


def _json_load(data: bytes, role: str) -> Any:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AdapterError(f"{role} is not UTF-8") from error
    try:
        return _EXPORTER._json_load(text, role)
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error


def _safe_root(path: Path, role: str) -> Path:
    absolute = path.absolute()
    try:
        _EXPORTER._safe_metadata(absolute, role, "directory")
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error
    return absolute


def _contained_path(root: Path, relative: str, role: str) -> Path:
    try:
        path = _EXPORTER._contained(root / relative, root, role)
        _EXPORTER._safe_ancestors(path, root, False)
        return path
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error


def _read_path(path: Path, maximum: int, role: str) -> tuple[bytes, tuple[int, int, int, int]]:
    try:
        data = _EXPORTER._read_bounded_file(path, maximum, role)
        metadata = _EXPORTER._safe_metadata(path, role, "file")
        return data, _EXPORTER._identity(metadata)
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error


def _file_reference(value: Any, role: str) -> dict[str, Any]:
    reference = _exact(value, {"path", "sha256"}, role)
    _relative_path(reference["path"], f"{role}.path")
    _hash(reference["sha256"], f"{role}.sha256")
    return reference


def _read_dependency(
    root: Path,
    reference: dict[str, Any],
    role: str,
    maximum: int,
) -> tuple[bytes, Path, dict[str, Any], tuple[int, int, int, int]]:
    path = _contained_path(root, reference["path"], role)
    data, identity = _read_path(path, maximum, role)
    observed = _sha256(data)
    if observed != reference["sha256"]:
        raise AdapterError(f"{role} digest mismatch")
    dependency = {
        "role": role,
        "path": reference["path"],
        "bytes": len(data),
        "sha256": observed,
    }
    return data, path, dependency, identity


def _descriptor_document(evidence_root: Path, descriptor_path: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    relative = _relative_path(descriptor_path.as_posix(), "descriptor argument")
    candidate = evidence_root / relative
    try:
        candidate = _EXPORTER._contained(candidate, evidence_root, "descriptor")
        _EXPORTER._safe_ancestors(candidate, evidence_root, False)
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error
    data, _ = _read_path(candidate, MAX_DESCRIPTOR_BYTES, "descriptor")
    value = _json_load(data, "descriptor")
    return value, {
        "role": "descriptor",
        "path": relative,
        "bytes": len(data),
        "sha256": _sha256(data),
    }


def _validate_descriptor(value: Any) -> dict[str, Any]:
    descriptor = _exact(
        value,
        {
            "schema_version", "kind", "synthetic", "repository_commit",
            "native_operation", "files", "expected", "source_files",
            "benchmark_locator_candidates",
        },
        "descriptor",
    )
    if (
        descriptor["schema_version"] != DESCRIPTOR_VERSION
        or descriptor["kind"] != "atlas-native-acquisition-descriptor"
    ):
        raise AdapterError("descriptor schema is incompatible")
    _boolean(descriptor["synthetic"], "descriptor.synthetic")
    _commit(descriptor["repository_commit"], "descriptor.repository_commit")
    if descriptor["native_operation"] != "context-ir":
        raise AdapterError("descriptor native operation is unsupported")
    files = _exact(
        descriptor["files"],
        {
            "context_stream", "status_before", "status_after",
            "catalogue_snapshot", "model_configuration", "agent_configuration",
            "capture_metadata", "benchmark_corpus",
        },
        "descriptor.files",
    )
    for field in (
        "context_stream", "status_before", "status_after", "catalogue_snapshot",
        "model_configuration", "agent_configuration",
    ):
        _file_reference(files[field], f"descriptor.files.{field}")
    for field in ("capture_metadata", "benchmark_corpus"):
        if files[field] is not None:
            _file_reference(files[field], f"descriptor.files.{field}")
    expected = _exact(
        descriptor["expected"],
        {
            "context_id", "context_hash", "workspace_id", "generation_id",
            "task_session_id", "request_hash", "configuration_hash",
            "workspace_root_fingerprint",
        },
        "descriptor.expected",
    )
    for field in ("context_id", "workspace_id", "generation_id", "task_session_id"):
        _string(expected[field], f"descriptor.expected.{field}", 256)
    for field in (
        "context_hash", "request_hash", "configuration_hash",
        "workspace_root_fingerprint",
    ):
        _hash(expected[field], f"descriptor.expected.{field}")
    source_files = descriptor["source_files"]
    if not isinstance(source_files, list) or len(source_files) > MAX_EVIDENCE:
        raise AdapterError("descriptor.source_files must be a bounded array")
    seen_sources: set[str] = set()
    for index, source in enumerate(source_files):
        role = f"descriptor.source_files[{index}]"
        item = _exact(source, {"path", "sha256", "bytes", "encoding"}, role)
        path = _relative_path(item["path"], f"{role}.path")
        if path in seen_sources:
            raise AdapterError("descriptor source path is duplicated")
        seen_sources.add(path)
        _hash(item["sha256"], f"{role}.sha256")
        source_bytes = _integer(item["bytes"], f"{role}.bytes")
        assert source_bytes is not None
        if source_bytes > MAX_SOURCE_BYTES:
            raise AdapterError(f"{role}.bytes exceeds the per-file source bound")
        if item["encoding"] != "utf-8":
            raise AdapterError(f"{role}.encoding is unsupported")
    candidates = descriptor["benchmark_locator_candidates"]
    if not isinstance(candidates, list) or len(candidates) > MAX_EVIDENCE:
        raise AdapterError("benchmark locator candidates must be a bounded array")
    seen_candidates: set[str] = set()
    for index, candidate in enumerate(candidates):
        role = f"descriptor.benchmark_locator_candidates[{index}]"
        item = _exact(candidate, {"item_id", "task_id", "locator_id"}, role)
        item_id = _string(item["item_id"], f"{role}.item_id", 256)
        _string(item["task_id"], f"{role}.task_id", 128)
        _string(item["locator_id"], f"{role}.locator_id", 1024)
        if item_id in seen_candidates:
            raise AdapterError("benchmark candidate item_id is duplicated")
        seen_candidates.add(item_id)
    return descriptor


def _validate_null_configuration(data: bytes, field: str) -> None:
    expected_value = {field: None}
    if data != _document_bytes(expected_value):
        raise AdapterError(
            f"{field} configuration must be the retained canonical null document"
        )


def _validate_status(value: Any, role: str) -> dict[str, Any]:
    status = _exact(
        value,
        {
            "ok", "workspace_id", "canonical_root", "config_hash",
            "registered_config_hash", "effective_config_hash", "schema_version",
            "migration_version", "active_generation_id", "active_generation_sequence",
            "active_generation_state", "committed_generation_count",
            "failed_generation_count", "abandoned_generation_count",
            "candidate_generation_count", "integrity_ok", "writer_lease_holder",
            "writer_lease_expires_at",
        },
        role,
    )
    if status["ok"] is not True or status["integrity_ok"] is not True:
        raise AdapterError(f"{role} is not an integrity-valid success status")
    for field in ("workspace_id", "canonical_root", "schema_version", "active_generation_id", "active_generation_state"):
        _string(status[field], f"{role}.{field}", 1024)
    for field in ("config_hash", "registered_config_hash", "effective_config_hash"):
        _hash(status[field], f"{role}.{field}")
    for field in (
        "migration_version", "active_generation_sequence", "committed_generation_count",
        "failed_generation_count", "abandoned_generation_count", "candidate_generation_count",
    ):
        _integer(status[field], f"{role}.{field}")
    if status["active_generation_state"] != "committed":
        raise AdapterError(f"{role} active generation is not committed")
    if status["writer_lease_holder"] is not None or status["writer_lease_expires_at"] is not None:
        raise AdapterError(f"{role} reports an active writer lease")
    return status


def _validate_string_array(value: Any, role: str, maximum: int = MAX_EVIDENCE) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum:
        raise AdapterError(f"{role} must be a bounded array")
    return [_string(item, f"{role} item") for item in value]


def _validate_evidence(value: Any, role: str, generation_id: str) -> dict[str, Any]:
    evidence = _exact(
        value,
        {
            "state", "confidence", "provider_fingerprint", "source_revision_hash",
            "generation_id", "preferred", "alternative_count",
        },
        role,
    )
    if evidence["state"] not in EVIDENCE_STATES:
        raise AdapterError(f"{role}.state is unsupported")
    confidence = _number(evidence["confidence"], f"{role}.confidence")
    if confidence > 1:
        raise AdapterError(f"{role}.confidence exceeds one")
    _optional_string(evidence["provider_fingerprint"], f"{role}.provider_fingerprint")
    if evidence["source_revision_hash"] is not None:
        _hash(evidence["source_revision_hash"], f"{role}.source_revision_hash")
    if evidence["generation_id"] != generation_id:
        raise AdapterError(f"{role} generation disagrees with the Context IR")
    if evidence["preferred"] is not None:
        _boolean(evidence["preferred"], f"{role}.preferred")
    _integer(evidence["alternative_count"], f"{role}.alternative_count", nullable=True)
    return evidence


def _validate_item(value: Any, index: int, generation_id: str) -> dict[str, Any]:
    role = f"context_ir.working_set[{index}]"
    item = _exact(
        value,
        {
            "item_id", "entity_kind", "entity_id", "role", "selection_reason",
            "origin_id", "distance", "rank", "evidence", "cost", "source",
        },
        role,
    )
    for field in ("item_id", "entity_id", "role", "selection_reason"):
        _string(item[field], f"{role}.{field}", 1024)
    if item["entity_kind"] not in ENTITY_KINDS:
        raise AdapterError(f"{role}.entity_kind is unsupported")
    if item["role"] not in ITEM_ROLES:
        raise AdapterError(f"{role}.role is unsupported")
    if item["selection_reason"] not in SELECTION_REASONS:
        raise AdapterError(f"{role}.selection_reason is unsupported")
    _optional_string(item["origin_id"], f"{role}.origin_id", 1024)
    _integer(item["distance"], f"{role}.distance")
    _integer(item["rank"], f"{role}.rank")
    _validate_evidence(item["evidence"], f"{role}.evidence", generation_id)
    cost = _exact(item["cost"], {"metadata_bytes", "source_bytes", "estimated_tokens"}, f"{role}.cost")
    for field in cost:
        _integer(cost[field], f"{role}.cost.{field}")
    if item["source"] is None and cost["source_bytes"] != 0:
        raise AdapterError(f"{role} source-less item has positive source cost")
    if item["source"] is not None:
        source = _exact(
            item["source"],
            {
                "path", "start_byte", "end_byte", "indexed_hash",
                "observed_hash", "verification_status",
            },
            f"{role}.source",
        )
        _relative_path(source["path"], f"{role}.source.path")
        _integer(source["start_byte"], f"{role}.source.start_byte")
        _integer(source["end_byte"], f"{role}.source.end_byte")
        _hash(source["indexed_hash"], f"{role}.source.indexed_hash")
        if source["observed_hash"] is not None:
            _hash(source["observed_hash"], f"{role}.source.observed_hash")
        if source["verification_status"] not in VERIFICATION_STATES:
            raise AdapterError(f"{role}.source.verification_status is unsupported")
    return item


def _validate_notice(value: Any, index: int) -> dict[str, Any]:
    role = f"context_ir.uncertainty[{index}]"
    notice = _exact(value, {"code", "severity", "message", "entity_ids"}, role)
    for field in ("code", "severity", "message"):
        _string(notice[field], f"{role}.{field}")
    if notice["severity"] not in NOTICE_SEVERITIES:
        raise AdapterError(f"{role}.severity is unsupported")
    _validate_string_array(notice["entity_ids"], f"{role}.entity_ids")
    return notice


def _validate_native_stream(value: Any) -> tuple[dict[str, Any], dict[str, Any]]:
    stream = _exact(
        value,
        {"ok", "task_session_id", "task_kind", "task_kind_source", "kind_rule_id", "context_ir"},
        "native stream",
    )
    if stream["ok"] is not True:
        raise AdapterError("native stream is not successful acquisition output")
    for field in ("task_session_id", "task_kind", "task_kind_source"):
        _string(stream[field], f"native stream.{field}", 256)
    _optional_string(stream["kind_rule_id"], "native stream.kind_rule_id", 256)
    if not isinstance(stream["context_ir"], dict):
        raise AdapterError("native stream.context_ir must be an object")
    supplied_version = stream["context_ir"].get("schema_version")
    if supplied_version == "2.0.0":
        raise AdapterError("adapter does not support Context IR version 2.0.0")
    if supplied_version != "1.0.0":
        raise AdapterError(f"unknown Context IR version: {supplied_version!r}")
    ir = _exact(
        stream["context_ir"],
        {
            "schema_version", "context_id", "workspace", "task", "policy", "status",
            "working_set", "relationships", "effects", "uncertainty", "coverage",
            "omissions", "validation_plan", "cost", "context_hash",
        },
        "context_ir",
    )
    _string(ir["context_id"], "context_ir.context_id", 256)
    _hash(ir["context_hash"], "context_ir.context_hash")
    workspace = _exact(
        ir["workspace"],
        {
            "workspace_id", "generation_id", "generation_sequence", "source_tree_hash",
            "configuration_hash", "provider_set_hash", "serving_generation_id",
        },
        "context_ir.workspace",
    )
    for field in ("workspace_id", "generation_id", "provider_set_hash"):
        _string(workspace[field], f"context_ir.workspace.{field}", 256)
    _integer(workspace["generation_sequence"], "context_ir.workspace.generation_sequence")
    if workspace["source_tree_hash"] is not None:
        _hash(workspace["source_tree_hash"], "context_ir.workspace.source_tree_hash")
    _hash(workspace["configuration_hash"], "context_ir.workspace.configuration_hash")
    _optional_string(workspace["serving_generation_id"], "context_ir.workspace.serving_generation_id", 256)
    task = _exact(
        ir["task"],
        {
            "task_session_id", "task_hash", "normalized_goal_hash", "task_kind",
            "kind_source", "kind_rule_id", "seed_paths", "seed_symbols",
        },
        "context_ir.task",
    )
    for field in ("task_session_id", "task_kind", "kind_source"):
        _string(task[field], f"context_ir.task.{field}", 256)
    if task["task_kind"] not in TASK_KINDS:
        raise AdapterError("context_ir.task.task_kind is unsupported")
    if task["kind_source"] not in TASK_KIND_SOURCES:
        raise AdapterError("context_ir.task.kind_source is unsupported")
    for field in ("task_hash", "normalized_goal_hash"):
        _hash(task[field], f"context_ir.task.{field}")
    _optional_string(task["kind_rule_id"], "context_ir.task.kind_rule_id", 256)
    for index, path in enumerate(_validate_string_array(task["seed_paths"], "context_ir.task.seed_paths")):
        _relative_path(path, f"context_ir.task.seed_paths[{index}]")
    _validate_string_array(task["seed_symbols"], "context_ir.task.seed_symbols")
    policy = _exact(
        ir["policy"],
        {"planner_policy_version", "projection_policy_version", "budget_profile", "deterministic"},
        "context_ir.policy",
    )
    for field in ("planner_policy_version", "projection_policy_version", "budget_profile"):
        _string(policy[field], f"context_ir.policy.{field}", 256)
    if policy["deterministic"] is not True:
        raise AdapterError("context_ir.policy.deterministic must be true")
    status = _exact(ir["status"], {"working_set_status", "reasons", "serving_fallback"}, "context_ir.status")
    if status["working_set_status"] not in WORKING_SET_STATES:
        raise AdapterError("context_ir.status.working_set_status is unsupported")
    _validate_string_array(status["reasons"], "context_ir.status.reasons")
    _boolean(status["serving_fallback"], "context_ir.status.serving_fallback")
    working_set = ir["working_set"]
    if not isinstance(working_set, list) or len(working_set) > MAX_EVIDENCE:
        raise AdapterError("context_ir.working_set must be a bounded array")
    seen_items: set[str] = set()
    for index, item in enumerate(working_set):
        validated = _validate_item(item, index, workspace["generation_id"])
        if validated["item_id"] in seen_items:
            raise AdapterError("context_ir item_id is duplicated")
        seen_items.add(validated["item_id"])
    for name in ("relationships", "effects"):
        if not isinstance(ir[name], list) or len(ir[name]) > MAX_EVIDENCE:
            raise AdapterError(f"context_ir.{name} must be a bounded array")
    for index, relationship in enumerate(ir["relationships"]):
        role = f"context_ir.relationships[{index}]"
        item = _exact(
            relationship,
            {
                "relationship_id", "source_entity_id", "target_entity_id",
                "relationship_type", "resolution_state", "evidence",
            },
            role,
        )
        for field in ("relationship_id", "source_entity_id", "target_entity_id", "relationship_type", "resolution_state"):
            _string(item[field], f"{role}.{field}")
        if item["resolution_state"] not in RESOLUTION_STATES:
            raise AdapterError(f"{role}.resolution_state is unsupported")
        _validate_evidence(item["evidence"], f"{role}.evidence", workspace["generation_id"])
    for index, effect in enumerate(ir["effects"]):
        role = f"context_ir.effects[{index}]"
        item = _exact(effect, {"subject_entity_id", "effect_type", "phase", "evidence"}, role)
        for field in ("subject_entity_id", "effect_type", "phase"):
            _string(item[field], f"{role}.{field}")
        if item["phase"] not in EFFECT_PHASES:
            raise AdapterError(f"{role}.phase is unsupported")
        _validate_evidence(item["evidence"], f"{role}.evidence", workspace["generation_id"])
    uncertainty = ir["uncertainty"]
    if not isinstance(uncertainty, list) or len(uncertainty) > MAX_EVIDENCE:
        raise AdapterError("context_ir.uncertainty must be a bounded array")
    for index, notice in enumerate(uncertainty):
        _validate_notice(notice, index)
    coverage = _exact(
        ir["coverage"],
        {"eligible", "complete", "partial", "unsupported", "excluded", "failed", "claim_strength", "details"},
        "context_ir.coverage",
    )
    for field in ("eligible", "complete", "partial", "unsupported", "excluded", "failed"):
        _integer(coverage[field], f"context_ir.coverage.{field}")
    _string(coverage["claim_strength"], "context_ir.coverage.claim_strength", 128)
    if coverage["claim_strength"] not in CLAIM_STRENGTHS:
        raise AdapterError("context_ir.coverage.claim_strength is unsupported")
    _validate_string_array(coverage["details"], "context_ir.coverage.details")
    omissions = _exact(
        ir["omissions"],
        {"candidates_considered", "selected", "omitted", "truncated", "by_reason"},
        "context_ir.omissions",
    )
    for field in ("candidates_considered", "selected", "omitted"):
        _integer(omissions[field], f"context_ir.omissions.{field}")
    _boolean(omissions["truncated"], "context_ir.omissions.truncated")
    if not isinstance(omissions["by_reason"], dict) or len(omissions["by_reason"]) > 128:
        raise AdapterError("context_ir.omissions.by_reason must be a bounded object")
    for key, count in omissions["by_reason"].items():
        _string(key, "context_ir.omissions.by_reason key", 256)
        _integer(count, f"context_ir.omissions.by_reason.{key}")
    validation_plan = ir["validation_plan"]
    if not isinstance(validation_plan, list) or len(validation_plan) > MAX_EVIDENCE:
        raise AdapterError("context_ir.validation_plan must be a bounded array")
    for index, validation in enumerate(validation_plan):
        role = f"context_ir.validation_plan[{index}]"
        item = _exact(validation, {"kind", "target", "reason", "required"}, role)
        for field in ("kind", "target", "reason"):
            _string(item[field], f"{role}.{field}")
        if item["kind"] not in VALIDATION_KINDS:
            raise AdapterError(f"{role}.kind is unsupported")
        _boolean(item["required"], f"{role}.required")
    cost = _exact(
        ir["cost"],
        {
            "max_records", "selected_records", "max_source_bytes", "selected_source_bytes",
            "max_estimated_tokens", "selected_estimated_tokens", "soft_latency_ms",
            "hard_latency_ms", "elapsed_ms",
        },
        "context_ir.cost",
    )
    for field in cost:
        _integer(cost[field], f"context_ir.cost.{field}")
    for selected_field, maximum_field, label in (
        ("selected_records", "max_records", "records"),
        ("selected_source_bytes", "max_source_bytes", "source bytes"),
        ("selected_estimated_tokens", "max_estimated_tokens", "estimated tokens"),
    ):
        if cost[selected_field] > cost[maximum_field]:
            raise AdapterError(
                f"selected {label} exceeds native budget {maximum_field}"
            )
    if stream["task_session_id"] != task["task_session_id"]:
        raise AdapterError("native stream task session disagrees with Context IR")
    if stream["task_kind"] != task["task_kind"] or stream["task_kind_source"] != task["kind_source"] or stream["kind_rule_id"] != task["kind_rule_id"]:
        raise AdapterError("native stream task identity disagrees with Context IR")
    expected_hash = _sha256(_compact_bytes({key: value for key, value in ir.items() if key != "context_hash"}))
    if ir["context_hash"] != expected_hash:
        raise AdapterError("native Context IR hash mismatch")
    return stream, ir


def _root_key(value: str) -> str:
    normalized = value.replace("\\", "/")
    if normalized.startswith("//?/"):
        normalized = normalized[4:]
    return normalized.rstrip("/").casefold()


def _line_column(data: bytes, offset: int, role: str) -> tuple[int, int]:
    if offset < 0 or offset > len(data):
        raise AdapterError(f"{role} source byte range is out of bounds")
    if 0 < offset < len(data) and data[offset - 1:offset] == b"\r" and data[offset:offset + 1] == b"\n":
        raise AdapterError(f"{role} source byte range splits a CRLF sequence")
    try:
        prefix = data[:offset].decode("utf-8")
    except UnicodeDecodeError as error:
        raise AdapterError(f"{role} source byte range splits a UTF-8 code point") from error
    line = 1
    column = 1
    index = 0
    while index < len(prefix):
        character = prefix[index]
        if character == "\r":
            if index + 1 < len(prefix) and prefix[index + 1] == "\n":
                index += 1
            line += 1
            column = 1
        elif character == "\n":
            line += 1
            column = 1
        else:
            column += 1
        index += 1
    return line, column


def _source_line_range(data: bytes, start: int, end: int, role: str) -> dict[str, int]:
    if start > end or end > len(data):
        raise AdapterError(f"{role} source byte range is invalid or out of bounds")
    start_line, start_column = _line_column(data, start, role)
    end_line, end_column = _line_column(data, end, role)
    return {
        "start_line": start_line,
        "start_column": start_column,
        "end_line": end_line,
        "end_column": end_column,
    }


def _required_line_bytes(data: bytes, start_line: int, end_line: int) -> tuple[int, int]:
    try:
        data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AdapterError("benchmark source is not UTF-8") from error
    lines = data.splitlines(keepends=True)
    if not 1 <= start_line <= end_line <= len(lines):
        raise AdapterError("benchmark locator line range exceeds frozen source")
    start = sum(len(line) for line in lines[: start_line - 1])
    ending = lines[end_line - 1]
    content_end = len(ending.rstrip(b"\r\n"))
    end = sum(len(line) for line in lines[: end_line - 1]) + content_end
    return start, end


def _capture_metadata(value: Any, context_sha256: str) -> dict[str, Any]:
    capture = _exact(
        value,
        {
            "arguments", "environment_qualified_wall_us", "exit_code",
            "stderr_sha256", "stdout_sha256", "task_acceptance",
        },
        "capture metadata",
    )
    _validate_string_array(capture["arguments"], "capture metadata.arguments", 128)
    _integer(capture["environment_qualified_wall_us"], "capture metadata.environment_qualified_wall_us")
    _integer(capture["exit_code"], "capture metadata.exit_code")
    _hash(capture["stderr_sha256"], "capture metadata.stderr_sha256")
    _hash(capture["stdout_sha256"], "capture metadata.stdout_sha256")
    if capture["stdout_sha256"] != context_sha256:
        raise AdapterError("capture metadata stdout digest disagrees with context stream")
    if capture["task_acceptance"] is not None:
        raise AdapterError("capture metadata must not claim task acceptance")
    return capture


def _one_row(connection: sqlite3.Connection, query: str, parameters: tuple[Any, ...], role: str) -> sqlite3.Row:
    rows = connection.execute(query, parameters).fetchall()
    if len(rows) != 1:
        adjective = "ambiguous" if len(rows) > 1 else "missing"
        raise AdapterError(f"{adjective} {role} binding")
    return rows[0]


def _metric_integer(row: sqlite3.Row, field: str) -> int:
    value = row[field]
    validated = _integer(value, f"query metric.{field}")
    assert validated is not None
    return validated


def _catalogue_bindings(
    path: Path,
    expected_identity: tuple[int, int, int, int],
    expected: dict[str, Any],
    source_root: Path,
    ir: dict[str, Any],
) -> dict[str, Any]:
    for suffix in ("-wal", "-shm", "-journal"):
        sidecar = Path(str(path) + suffix)
        if sidecar.exists() or sidecar.is_symlink():
            raise AdapterError("catalogue snapshot has live WAL or journal ambiguity")
    uri = path.absolute().as_uri() + "?mode=ro&immutable=1"
    try:
        connection = sqlite3.connect(uri, uri=True)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA query_only=ON")
        context = _one_row(
            connection,
            "SELECT * FROM context_ir WHERE context_id=?",
            (expected["context_id"],),
            "context_ir row",
        )
        task = _one_row(
            connection,
            "SELECT * FROM task_session WHERE task_session_id=?",
            (expected["task_session_id"],),
            "task session row",
        )
        workspace = _one_row(
            connection,
            "SELECT * FROM workspace WHERE workspace_id=?",
            (expected["workspace_id"],),
            "workspace row",
        )
        generation = _one_row(
            connection,
            "SELECT * FROM index_generation WHERE generation_id=? AND workspace_id=?",
            (expected["generation_id"], expected["workspace_id"]),
            "generation row",
        )
        metrics = connection.execute(
            "SELECT * FROM query_stage_metric WHERE workspace_id=? AND generation_id=? AND task_session_id=? AND request_id=? AND operation='context_ir_compile'",
            (
                expected["workspace_id"], expected["generation_id"],
                expected["task_session_id"], expected["request_hash"],
            ),
        ).fetchall()
        if len(metrics) != 1:
            adjective = "ambiguous" if len(metrics) > 1 else "missing"
            raise AdapterError(f"{adjective} query metric row binding")
        metric = metrics[0]
    except sqlite3.DatabaseError as error:
        raise AdapterError(f"catalogue snapshot query failed: {error}") from error
    finally:
        if "connection" in locals():
            connection.close()
    try:
        current_identity = _EXPORTER._identity(_EXPORTER._safe_metadata(path, "catalogue snapshot", "file"))
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error
    if current_identity != expected_identity:
        raise AdapterError("catalogue snapshot changed during read")

    identity_checks = {
        "context_id": context["context_id"],
        "context_hash": context["context_hash"],
        "workspace_id": context["workspace_id"],
        "generation_id": context["generation_id"],
        "task_session_id": context["task_session_id"],
        "request_hash": context["request_hash"],
    }
    for field, actual in identity_checks.items():
        if actual != expected[field]:
            raise AdapterError(f"expected identity mismatch: {field}")
    if workspace["workspace_id"] != expected["workspace_id"] or workspace["configuration_hash"] != expected["configuration_hash"] or workspace["root_fingerprint"] != expected["workspace_root_fingerprint"] or workspace["active_generation_id"] != expected["generation_id"]:
        raise AdapterError("workspace row disagrees with expected identities")
    if _root_key(workspace["canonical_root"]) != _root_key(str(source_root)):
        raise AdapterError("workspace root disagrees with constrained source root")
    if generation["state"] != "committed" or generation["sequence_no"] != ir["workspace"]["generation_sequence"] or generation["provider_set_hash"] != ir["workspace"]["provider_set_hash"]:
        raise AdapterError("generation row disagrees with committed Context IR binding")
    indexed_tree_hash = generation["source_tree_hash"]
    if indexed_tree_hash is not None:
        _hash(indexed_tree_hash, "catalogue generation.source_tree_hash")
    native_task = ir["task"]
    if (
        task["workspace_id"] != expected["workspace_id"]
        or task["start_generation_id"] != expected["generation_id"]
        or task["task_hash"] != native_task["task_hash"]
        or task["normalized_goal_hash"] != native_task["normalized_goal_hash"]
        or task["task_kind"] != native_task["task_kind"]
        or task["task_kind_source"] != native_task["kind_source"]
        or task["task_kind_rule_id"] != native_task["kind_rule_id"]
        or task["planner_policy_version"] != ir["policy"]["planner_policy_version"]
        or task["context_ir_version"] != "1.0.0"
        or task["accepted"] is not None
        or task["tests_passed"] is not None
        or task["outcome_code"] is not None
    ):
        raise AdapterError("task session row disagrees with acquisition-only Context IR")
    cost = ir["cost"]
    context_checks = {
        "context_ir_version": "1.0.0",
        "planner_policy_version": ir["policy"]["planner_policy_version"],
        "projection_policy_version": ir["policy"]["projection_policy_version"],
        "budget_profile": ir["policy"]["budget_profile"],
        "working_set_status": ir["status"]["working_set_status"],
        "serving_fallback": int(ir["status"]["serving_fallback"]),
        "max_records": cost["max_records"],
        "selected_records": cost["selected_records"],
        "max_source_bytes": cost["max_source_bytes"],
        "selected_source_bytes": cost["selected_source_bytes"],
        "max_estimated_tokens": cost["max_estimated_tokens"],
        "selected_estimated_tokens": cost["selected_estimated_tokens"],
        "soft_latency_ms": cost["soft_latency_ms"],
        "hard_latency_ms": cost["hard_latency_ms"],
        "elapsed_ms": cost["elapsed_ms"],
    }
    for field, expected_value in context_checks.items():
        if context[field] != expected_value:
            raise AdapterError(f"context_ir row disagrees with native field: {field}")
    canonical_row = _json_load(context["canonical_json"].encode("utf-8"), "catalogue context canonical_json")
    if canonical_row != ir:
        raise AdapterError("catalogue canonical Context IR disagrees with retained stream")
    if metric["context_id"] not in (None, expected["context_id"]):
        raise AdapterError("query metric context identity disagrees with retained Context IR")
    metric_values = {
        field: _metric_integer(metric, field)
        for field in (
            "seed_resolution_us", "serving_lookup_us", "graph_expansion_us",
            "ranking_us", "source_verify_us", "source_read_us", "packing_us",
            "candidate_count", "expanded_edge_count", "source_bytes_read",
            "returned_records", "returned_estimated_tokens", "cache_hits",
            "cache_misses", "truncated",
        )
    }
    if metric_values["returned_records"] != cost["selected_records"] or metric_values["returned_estimated_tokens"] != cost["selected_estimated_tokens"]:
        raise AdapterError("query metric returned records or tokens disagree with Context IR")
    if bool(metric_values["truncated"]) != ir["omissions"]["truncated"]:
        raise AdapterError("query metric truncation disagrees with Context IR")
    if metric_values["candidate_count"] != ir["omissions"]["candidates_considered"]:
        raise AdapterError("query metric candidate count disagrees with Context IR")
    if bool(metric["serving_fallback"]) != ir["status"]["serving_fallback"]:
        raise AdapterError("query metric Serving fallback disagrees with Context IR")
    return {
        "workspace_root_fingerprint": workspace["root_fingerprint"],
        "indexed_generation_source_tree_hash": indexed_tree_hash,
        "generation_sequence": generation["sequence_no"],
        "generation_state": generation["state"],
        "metric": metric_values,
    }


def _benchmark_locators(corpus: dict[str, Any]) -> dict[tuple[str, str], dict[str, Any]]:
    locators: dict[tuple[str, str], dict[str, Any]] = {}
    for task in corpus["tasks"]:
        for locator in task["required_evidence"]:
            key = (task["task_id"], locator["id"])
            if key in locators:
                raise AdapterError("benchmark corpus locator is duplicated within a task")
            locators[key] = locator
    return locators


def _mapping_result(
    item: dict[str, Any],
    source_output: dict[str, Any] | None,
    candidate: dict[str, Any] | None,
    locator: dict[str, Any] | None,
    source_data: bytes | None,
) -> tuple[dict[str, Any] | None, str]:
    if candidate is None:
        return None, "not_requested"
    if locator is None:
        raise AdapterError("benchmark locator candidate is absent from corpus")
    if item["entity_kind"] != locator["kind"] or item["entity_id"] != locator["id"]:
        return None, "identity_mismatch"
    if source_output is None or source_data is None:
        return None, "source_unavailable"
    required_range = locator["range"]
    if source_output["path"] != required_range["path"]:
        return None, "path_mismatch"
    if source_output["whole_file_sha256"] != locator["source_revision_sha256"]:
        return None, "source_revision_mismatch"
    required_start, required_end = _required_line_bytes(
        source_data, required_range["start_line"], required_range["end_line"]
    )
    if source_output["native_start_byte"] > required_start or source_output["native_end_byte"] < required_end:
        return None, "required_range_not_fully_covered"
    return {
        "task_id": candidate["task_id"],
        "locator_id": candidate["locator_id"],
        "locator_kind": locator["kind"],
        "required_range": required_range,
        "source_revision_sha256": locator["source_revision_sha256"],
    }, "verified_exact_mapping"


def normalize(
    evidence_workspace: Path,
    source_root: Path,
    descriptor_path: Path,
) -> dict[str, Any]:
    evidence_root = _safe_root(evidence_workspace, "evidence workspace")
    constrained_source_root = _safe_root(source_root, "source root")
    try:
        raw_descriptor, descriptor_dependency = _descriptor_document(
            evidence_root, descriptor_path
        )
        descriptor = _validate_descriptor(raw_descriptor)

        dependencies = [descriptor_dependency]
        loaded: dict[str, tuple[bytes, Path, dict[str, Any], tuple[int, int, int, int]]] = {}
        maximums = {
            "context_stream": MAX_JSON_BYTES,
            "status_before": MAX_JSON_BYTES,
            "status_after": MAX_JSON_BYTES,
            "catalogue_snapshot": MAX_CATALOGUE_BYTES,
            "model_configuration": MAX_JSON_BYTES,
            "agent_configuration": MAX_JSON_BYTES,
            "capture_metadata": MAX_JSON_BYTES,
            "benchmark_corpus": MAX_JSON_BYTES,
        }
        for role, reference in descriptor["files"].items():
            if reference is None:
                continue
            loaded[role] = _read_dependency(
                evidence_root, reference, role, maximums[role]
            )
            dependencies.append(loaded[role][2])

        _validate_null_configuration(loaded["model_configuration"][0], "model")
        _validate_null_configuration(loaded["agent_configuration"][0], "agent")
        stream_value = _json_load(loaded["context_stream"][0], "native stream")
        _, ir = _validate_native_stream(stream_value)
        referenced_source_paths = {
            item["source"]["path"]
            for item in ir["working_set"]
            if item["source"] is not None
        }
        source_descriptors = {
            item["path"]: item for item in descriptor["source_files"]
        }
        unrelated_sources = sorted(
            set(source_descriptors) - referenced_source_paths
        )
        if unrelated_sources:
            raise AdapterError(
                f"unrelated descriptor source paths: {unrelated_sources!r}"
            )
        missing_sources = sorted(
            referenced_source_paths - set(source_descriptors)
        )
        if missing_sources:
            raise AdapterError(
                f"native source paths lack descriptors: {missing_sources!r}"
            )
        declared_source_bytes = 0
        for path_value in sorted(referenced_source_paths):
            declared_source_bytes += source_descriptors[path_value]["bytes"]
            if declared_source_bytes > MAX_AGGREGATE_SOURCE_BYTES:
                raise AdapterError(
                    "declared sources exceed the aggregate source byte bound"
                )

        corpus = None
        locators: dict[tuple[str, str], dict[str, Any]] = {}
        benchmark_corpus_sha256 = None
        commit_consistency = "not_available"
        if "benchmark_corpus" in loaded:
            try:
                corpus = _QUALIFICATION._validate_corpus_value(
                    _json_load(loaded["benchmark_corpus"][0], "benchmark corpus")
                )
            except _QUALIFICATION.QualificationError as error:
                raise AdapterError(str(error)) from error
            corpus_commit = corpus["repository"]["commit"]
            if corpus_commit != descriptor["repository_commit"]:
                raise AdapterError(
                    "descriptor repository commit disagrees with benchmark corpus"
                )
            commit_consistency = "matched_declared_benchmark_corpus"
            locators = _benchmark_locators(corpus["document"])
            benchmark_corpus_sha256 = corpus["corpus_sha256"]
        candidates = {
            item["item_id"]: item
            for item in descriptor["benchmark_locator_candidates"]
        }
        if candidates and corpus is None:
            raise AdapterError(
                "benchmark locator candidates require a benchmark corpus"
            )
        expected = descriptor["expected"]
        actual_expected = {
            "context_id": ir["context_id"],
            "context_hash": ir["context_hash"],
            "workspace_id": ir["workspace"]["workspace_id"],
            "generation_id": ir["workspace"]["generation_id"],
            "task_session_id": ir["task"]["task_session_id"],
            "configuration_hash": ir["workspace"]["configuration_hash"],
        }
        for field, actual in actual_expected.items():
            if expected[field] != actual:
                raise AdapterError(f"expected identity mismatch: {field}")

        before = _validate_status(
            _json_load(loaded["status_before"][0], "status before"),
            "status before",
        )
        after = _validate_status(
            _json_load(loaded["status_after"][0], "status after"),
            "status after",
        )
        if before != after:
            raise AdapterError("before/after status changed during acquisition")
        if (
            before["workspace_id"] != expected["workspace_id"]
            or before["active_generation_id"] != expected["generation_id"]
            or before["config_hash"] != expected["configuration_hash"]
            or before["registered_config_hash"] != expected["configuration_hash"]
            or before["effective_config_hash"] != expected["configuration_hash"]
        ):
            raise AdapterError("status binding disagrees with expected workspace/generation/config")
        if _root_key(before["canonical_root"]) != _root_key(str(constrained_source_root)):
            raise AdapterError("status workspace root disagrees with constrained source root")

        capture = None
        if "capture_metadata" in loaded:
            capture = _capture_metadata(
                _json_load(loaded["capture_metadata"][0], "capture metadata"),
                loaded["context_stream"][2]["sha256"],
            )

        catalogue = _catalogue_bindings(
            loaded["catalogue_snapshot"][1],
            loaded["catalogue_snapshot"][3],
            expected,
            constrained_source_root,
            ir,
        )
        if catalogue["workspace_root_fingerprint"] != expected["workspace_root_fingerprint"]:
            raise AdapterError("expected identity mismatch: workspace_root_fingerprint")
        if before["active_generation_sequence"] != catalogue["generation_sequence"]:
            raise AdapterError("status generation sequence disagrees with catalogue")
        if (
            ir["workspace"]["source_tree_hash"] is not None
            and ir["workspace"]["source_tree_hash"]
            != catalogue["indexed_generation_source_tree_hash"]
        ):
            raise AdapterError(
                "IR source tree hash disagrees with committed generation tree hash"
            )

        source_contents: dict[str, bytes] = {}
        actual_source_bytes = 0
        for path_value in sorted(referenced_source_paths):
            source_descriptor = source_descriptors[path_value]
            path = _contained_path(
                constrained_source_root,
                path_value,
                f"source file {path_value}",
            )
            data, _ = _read_path(
                path, MAX_SOURCE_BYTES, f"source file {path_value}"
            )
            if len(data) != source_descriptor["bytes"]:
                raise AdapterError(
                    f"source file byte count mismatch: {path_value}"
                )
            actual_source_bytes += len(data)
            if actual_source_bytes > MAX_AGGREGATE_SOURCE_BYTES:
                raise AdapterError(
                    "read sources exceed the aggregate source byte bound"
                )
            observed_hash = _sha256(data)
            if observed_hash != source_descriptor["sha256"]:
                raise AdapterError(f"source file digest mismatch: {path_value}")
            try:
                data.decode("utf-8")
            except UnicodeDecodeError as error:
                raise AdapterError(
                    f"source file is not UTF-8: {path_value}"
                ) from error
            source_contents[path_value] = data
            dependencies.append({
                "role": f"source:{path_value}",
                "path": path_value,
                "bytes": len(data),
                "sha256": observed_hash,
            })
        if actual_source_bytes != declared_source_bytes:
            raise AdapterError(
                "aggregate source bytes changed between declaration and read"
            )

        evidence_output: list[dict[str, Any]] = []
        for ordinal, item in enumerate(ir["working_set"]):
            source_output = None
            source_data = None
            native_source = item["source"]
            if native_source is not None:
                path_value = native_source["path"]
                source_data = source_contents.get(path_value)
                if source_data is None:
                    raise AdapterError(f"native source path lacks a descriptor: {path_value}")
                start = native_source["start_byte"]
                end = native_source["end_byte"]
                line_range = _source_line_range(
                    source_data, start, end, f"working_set item {item['item_id']}"
                )
                if end - start != item["cost"]["source_bytes"]:
                    raise AdapterError("native source byte range disagrees with item source cost")
                whole_hash = _sha256(source_data)
                if native_source["indexed_hash"] != whole_hash:
                    raise AdapterError("native indexed source hash disagrees with frozen raw bytes")
                if native_source["verification_status"] != "not_requested" or native_source["observed_hash"] is not None:
                    raise AdapterError("adapter scope requires not_requested native source verification")
                source_output = {
                    "path": path_value,
                    "native_start_byte": start,
                    "native_end_byte": end,
                    "indexed_hash": {
                        "algorithm": "sha256",
                        "value": native_source["indexed_hash"],
                    },
                    "observed_hash": native_source["observed_hash"],
                    "verification_status": native_source["verification_status"],
                    "whole_file_bytes": len(source_data),
                    "whole_file_sha256": whole_hash,
                    "line_range": line_range,
                }
            candidate = candidates.get(item["item_id"])
            locator = None
            if candidate is not None:
                locator = locators.get((candidate["task_id"], candidate["locator_id"]))
            mapping, mapping_reason = _mapping_result(
                item, source_output, candidate, locator, source_data
            )
            evidence_output.append({
                "ordinal": ordinal,
                "item_id": item["item_id"],
                "identity": {
                    "kind": item["entity_kind"],
                    "id": item["entity_id"],
                    "range": None,
                },
                "native": {
                    "role": item["role"],
                    "selection_reason": item["selection_reason"],
                    "origin_id": item["origin_id"],
                    "distance": item["distance"],
                    "rank": item["rank"],
                    "evidence": item["evidence"],
                    "cost": item["cost"],
                },
                "source": source_output,
                "benchmark_mapping": mapping,
                "benchmark_mapping_reason": mapping_reason,
            })
        if set(candidates) - {item["item_id"] for item in ir["working_set"]}:
            raise AdapterError("benchmark candidate references an unknown native item")

        selected_records = len(ir["working_set"])
        if (
            ir["cost"]["selected_records"] != selected_records
            or ir["omissions"]["selected"] != selected_records
        ):
            raise AdapterError(
                "selected record count disagrees with native working set"
            )
        selected_tokens = sum(
            item["cost"]["estimated_tokens"] for item in ir["working_set"]
        )
        if selected_tokens != ir["cost"]["selected_estimated_tokens"]:
            raise AdapterError(
                "selected token sum disagrees with Context IR cost"
            )
        selected_span_bytes = sum(item["cost"]["source_bytes"] for item in ir["working_set"])
        if selected_span_bytes != ir["cost"]["selected_source_bytes"]:
            raise AdapterError("selected source span sum disagrees with Context IR cost")
        metric = catalogue["metric"]
        if metric["source_bytes_read"] != 0:
            raise AdapterError("delivered source bytes contradict not_requested source verification")
        if ir["cost"]["elapsed_ms"] != 0:
            raise AdapterError("legacy elapsed_ms is not the expected hardcoded zero")

        output: dict[str, Any] = {
            "schema_version": OUTPUT_VERSION,
            "kind": "atlas-native-acquisition-normalization",
            "adapter_version": ADAPTER_VERSION,
            "synthetic": descriptor["synthetic"],
            "native_operation": "context-ir",
            "governor_route": None,
            "task_execution": None,
            "acceptance": None,
            "bindings": {
                "repository_commit": {
                    "value": descriptor["repository_commit"],
                    "verification": "declared_only",
                    "source_root_git_proof": None,
                    "benchmark_corpus_consistency": commit_consistency,
                },
                "workspace_id": expected["workspace_id"],
                "workspace_root_fingerprint": expected["workspace_root_fingerprint"],
                "generation_id": expected["generation_id"],
                "generation_sequence": catalogue["generation_sequence"],
                "generation_state": catalogue["generation_state"],
                "configuration_hash": expected["configuration_hash"],
                "provider_set_hash": ir["workspace"]["provider_set_hash"],
                "native_context_ir_version": "1.0.0",
                "context_id": expected["context_id"],
                "context_hash": expected["context_hash"],
                "task_session_id": expected["task_session_id"],
                "request_hash": expected["request_hash"],
                "ir_source_tree_hash": ir["workspace"]["source_tree_hash"],
                "indexed_generation_source_tree_hash": catalogue["indexed_generation_source_tree_hash"],
                "status_before_sha256": loaded["status_before"][2]["sha256"],
                "status_after_sha256": loaded["status_after"][2]["sha256"],
                "catalogue_snapshot_sha256": loaded["catalogue_snapshot"][2]["sha256"],
                "model_configuration_sha256": loaded["model_configuration"][2]["sha256"],
                "agent_configuration_sha256": loaded["agent_configuration"][2]["sha256"],
                "benchmark_corpus_sha256": benchmark_corpus_sha256,
            },
            "acquisition": {
                "status": ir["status"]["working_set_status"],
                "coverage": ir["coverage"],
                "omissions": ir["omissions"],
                "uncertainty": ir["uncertainty"],
                "source_fence": {
                    "state": "unavailable",
                    "reason": "no_live_before_after_source_verification",
                },
                "cli_exit_code": capture["exit_code"] if capture else None,
            },
            "cost": {
                "stage_timings_us": {
                    "seed_resolution": metric["seed_resolution_us"],
                    "serving_lookup": metric["serving_lookup_us"],
                    "graph_expansion": metric["graph_expansion_us"],
                    "ranking": metric["ranking_us"],
                    "source_verify": metric["source_verify_us"],
                    "source_read": metric["source_read_us"],
                    "packing": metric["packing_us"],
                },
                "candidate_count": metric["candidate_count"],
                "expanded_edges": metric["expanded_edge_count"],
                "selected_records": metric["returned_records"],
                "selected_estimated_tokens": metric["returned_estimated_tokens"],
                "selected_span_bytes": ir["cost"]["selected_source_bytes"],
                "delivered_source_bytes": metric["source_bytes_read"],
                "legacy_elapsed_ms": None,
                "legacy_elapsed_reason": "hardcoded_zero_not_timing_evidence",
                "command_wall_time_us": capture["environment_qualified_wall_us"] if capture else None,
                "command_wall_time_scope": "environment_qualified_command" if capture else None,
                "cpu_ms": None,
                "io_read_bytes": None,
                "io_write_bytes": None,
                "database_steps": None,
                "provider_scopes": None,
                "work_units": None,
            },
            "evidence": evidence_output,
            "native_relationships": ir["relationships"],
            "native_effects": ir["effects"],
            "dependencies": sorted(dependencies, key=lambda item: (item["role"], item["path"])),
            "coverage": {
                "context_stream": True,
                "status_before_after": True,
                "catalogue_snapshot": True,
                "query_metrics": True,
                "frozen_source_mapping": bool(source_descriptors),
                "benchmark_mapping": corpus is not None,
                "command_capture": capture is not None,
                "model_execution": False,
                "agent_execution": False,
            },
            "limitations": [
                "acquisition_only_no_task_execution_or_acceptance",
                "context_ir_2_adapter_unsupported",
                "no_live_before_after_source_fence",
                "no_governor_route_or_work_units",
                "no_cpu_io_database_step_or_provider_scope_measurement",
                "frozen_source_reads_are_mapping_not_model_delivery",
            ],
        }
        output["normalization_sha256"] = _sha256(_document_bytes(output))
        if len(_document_bytes(output)) > MAX_OUTPUT_BYTES:
            raise AdapterError("normalized output exceeds output byte bound")
        return output
    except _QUALIFICATION.QualificationError as error:
        raise AdapterError(str(error)) from error


def _write_output(evidence_workspace: Path, output_path: Path, value: Any) -> None:
    root = _safe_root(evidence_workspace, "evidence workspace")
    relative = _relative_path(output_path.as_posix(), "output argument")
    candidate = root / relative
    try:
        candidate = _EXPORTER._contained(candidate, root, "output")
        if candidate.exists() or candidate.is_symlink():
            raise AdapterError("output already exists")
        _EXPORTER._safe_ancestors(candidate, root, False)
        payload = _document_bytes(value)
        if len(payload) > MAX_OUTPUT_BYTES:
            raise AdapterError("output byte bound exceeded")
        _EXPORTER._write_exclusive(candidate, payload)
    except _EXPORTER.ExportError as error:
        raise AdapterError(str(error)) from error
    except OSError as error:
        raise AdapterError("output could not be written") from error


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-workspace", type=Path, required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--descriptor", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(arguments)
    try:
        normalized = normalize(
            args.evidence_workspace,
            args.source_root,
            args.descriptor,
        )
        _write_output(args.evidence_workspace, args.output, normalized)
    except AdapterError as error:
        print(f"native acquisition normalization error: {error}", file=sys.stderr)
        return 2
    print(json.dumps({
        "kind": normalized["kind"],
        "sha256": normalized["normalization_sha256"],
        "status": normalized["acquisition"]["status"],
        "acceptance": None,
    }, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
