#!/usr/bin/env python3
"""Validate, report, and compare bounded offline Atlas qualification evidence."""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import sys
from typing import Any

HARNESS_VERSION = "atlas-context-qualification-harness-v1.0.0"
CORPUS_VERSION = "atlas-context-qualification-corpus-v1.0.0"
INPUT_VERSION = "atlas-context-qualification-input-v1.0.0"
OBSERVATION_VERSION = "atlas-context-qualification-observation-v1.0.0"
RECEIPT_VERSION = "atlas-context-qualification-validation-receipt-v1.0.0"
REPORT_VERSION = "atlas-context-qualification-report-v1.0.0"
COMPARISON_VERSION = "atlas-context-qualification-comparison-v1.0.0"
METRIC_POLICY_VERSION = "context-qualification-v1.0.0"
MAX_DOCUMENT_BYTES = 4 * 1024 * 1024
MAX_EVIDENCE = 4096
MAX_TASKS = 256
MAX_STRING = 4096
MAX_SIGNED_63 = (1 << 63) - 1
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
TASK_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
EVIDENCE_KINDS = frozenset({
    "file", "symbol", "config", "test", "query", "document", "external",
    "relationship", "range",
})
TASK_CLASSES = frozenset({
    "single_file_change", "cross_module_behavior", "api", "configuration",
    "bug_fix", "refactor", "rename", "test_sensitive_change",
    "historical_regression_question", "high_fan_out_change",
    "ambiguous_conflicting_evidence",
})
COVERAGE_FIELDS = frozenset({
    "atlas_cost", "context_cost", "agent_cost", "read_requests",
    "reported_use", "task_result", "validation",
})
BUDGET_FIELDS = (
    "max_records", "max_source_bytes", "max_estimated_tokens", "max_work_units",
)


class QualificationError(RuntimeError):
    """An input cannot satisfy the qualification evidence contract."""


def _load_exporter() -> Any:
    path = Path(__file__).with_name("benchmark-evidence-export.py")
    spec = importlib.util.spec_from_file_location("atlas_qualification_export_safety", path)
    if spec is None or spec.loader is None:
        raise QualificationError("benchmark evidence safeguards are unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


_EXPORTER = _load_exporter()


def _canonical_bytes(value: Any) -> bytes:
    try:
        text = json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
    except (TypeError, ValueError) as error:
        raise QualificationError("canonical content contains an unsupported value") from error
    return (text + "\n").encode("utf-8")


def canonical_sha256(value: Any) -> str:
    return _EXPORTER._hash(_canonical_bytes(value))


def _exact_fields(value: Any, fields: set[str] | frozenset[str], role: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(fields):
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise QualificationError(
            f"{role} fields must be exactly {sorted(fields)!r}; got {actual!r}"
        )
    return value


def _string(value: Any, role: str, *, maximum: int = MAX_STRING) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > maximum
        or "\0" in value
    ):
        raise QualificationError(f"{role} must be a nonempty bounded string")
    return value


def _hash(value: Any, role: str) -> str:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        raise QualificationError(f"{role} must be a lowercase SHA-256")
    return value


def _commit(value: Any, role: str) -> str:
    if not isinstance(value, str) or COMMIT.fullmatch(value) is None:
        raise QualificationError(f"{role} must be a lowercase 40-character Git commit")
    return value


def _integer(value: Any, role: str, *, nullable: bool = False) -> int | None:
    if value is None and nullable:
        return None
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or value < 0
        or value > MAX_SIGNED_63
    ):
        raise QualificationError(
            f"{role} must be a nonnegative integer no greater than {MAX_SIGNED_63}"
        )
    return value


def _number(value: Any, role: str, *, nullable: bool = False) -> float | int | None:
    if value is None and nullable:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise QualificationError(f"{role} must be a bounded nonnegative finite number")
    if value < 0 or value > MAX_SIGNED_63 or not math.isfinite(value):
        raise QualificationError(f"{role} must be a bounded nonnegative finite number")
    return value


def _boolean(value: Any, role: str, *, nullable: bool = False) -> bool | None:
    if value is None and nullable:
        return None
    if not isinstance(value, bool):
        raise QualificationError(f"{role} must be a boolean")
    return value


def _string_list(value: Any, role: str, *, maximum: int = 128) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum:
        raise QualificationError(f"{role} must be a bounded string array")
    return [_string(item, f"{role} item") for item in value]

def _sum_bounded(values: Any, role: str) -> int:
    total = 0
    for value in values:
        total += value
        if total > MAX_SIGNED_63:
            raise QualificationError(f"{role} exceeds the signed 63-bit bound")
    return total


def load_json_document(workspace: Path, path: Path, role: str) -> Any:
    workspace = workspace.absolute()
    candidate = path if path.is_absolute() else workspace / path
    try:
        candidate = _EXPORTER._contained(candidate, workspace, role)
        _EXPORTER._safe_ancestors(candidate, workspace, False)
        raw = _EXPORTER._read_bounded_file(candidate, MAX_DOCUMENT_BYTES, role)
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise QualificationError(f"{role} is not UTF-8") from error
        return _EXPORTER._json_load(text, role)
    except QualificationError:
        raise
    except _EXPORTER.ExportError as error:
        raise QualificationError(str(error)) from error


def _validate_range(value: Any, role: str) -> dict[str, Any] | None:
    if value is None:
        return None
    item = _exact_fields(value, {"path", "start_line", "end_line"}, role)
    path = _string(item["path"], f"{role}.path", maximum=512)
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
        raise QualificationError(
            f"{role}.path must use canonical forward-slash-separated relative spelling"
        )
    start = _integer(item["start_line"], f"{role}.start_line")
    end = _integer(item["end_line"], f"{role}.end_line")
    if start == 0 or end == 0 or start > end:
        raise QualificationError(f"{role} line range is invalid")
    return item


def _validate_identity(value: Any, role: str) -> dict[str, Any]:
    item = _exact_fields(value, {"kind", "id", "range"}, role)
    if item["kind"] not in EVIDENCE_KINDS:
        raise QualificationError(f"{role}.kind is unsupported")
    _string(item["id"], f"{role}.id", maximum=1024)
    source_range = _validate_range(item["range"], f"{role}.range")
    if item["kind"] == "range" and source_range is None:
        raise QualificationError(f"{role} range evidence requires a range")
    return item


def _identity_key(identity: dict[str, Any]) -> str:
    return _canonical_bytes(identity).decode("utf-8")


def _validate_required_evidence(value: Any, role: str) -> dict[str, Any]:
    item = _exact_fields(
        value, {"kind", "id", "range", "source_revision_sha256"}, role
    )
    _validate_identity(
        {"kind": item["kind"], "id": item["id"], "range": item["range"]}, role
    )
    _hash(item["source_revision_sha256"], f"{role}.source_revision_sha256")
    return item

def _validation_contract_sha256(task: dict[str, Any]) -> str:
    return canonical_sha256({
        "validation_method": task["validation_method"],
        "accepted_outcome_criteria": task["accepted_outcome_criteria"],
    })


def _validate_corpus_value(value: Any) -> dict[str, Any]:
    corpus = _exact_fields(
        value,
        {"schema_version", "kind", "repository", "locator_contract", "tasks"},
        "corpus",
    )
    if (
        corpus["schema_version"] != CORPUS_VERSION
        or corpus["kind"] != "atlas-context-qualification-corpus"
    ):
        raise QualificationError("corpus schema is incompatible")
    repository = _exact_fields(
        corpus["repository"], {"commit", "object_format", "source_content_hash"},
        "corpus.repository",
    )
    _commit(repository["commit"], "corpus.repository.commit")
    if repository["object_format"] != "sha1" or repository["source_content_hash"] != "sha256":
        raise QualificationError("corpus repository identity algorithms are unsupported")
    locator_contract = _exact_fields(
        corpus["locator_contract"],
        {"namespace", "native_identity_equivalence", "native_normalization_adapter"},
        "corpus.locator_contract",
    )
    if (
        locator_contract["namespace"] != "benchmark"
        or locator_contract["native_identity_equivalence"] is not False
        or locator_contract["native_normalization_adapter"] != "required_not_implemented"
    ):
        raise QualificationError("corpus benchmark locator contract is incompatible")
    tasks = corpus["tasks"]
    if not isinstance(tasks, list) or not 1 <= len(tasks) <= MAX_TASKS:
        raise QualificationError("corpus tasks must be a nonempty bounded array")
    tasks_by_id: dict[str, dict[str, Any]] = {}
    for index, task_value in enumerate(tasks):
        role = f"corpus.tasks[{index}]"
        task = _exact_fields(
            task_value,
            {
                "task_id", "task_class", "specification", "requirement_coverage",
                "required_evidence", "uncertainty_boundaries", "validation_method",
                "accepted_outcome_criteria",
            },
            role,
        )
        task_id = task["task_id"]
        if not isinstance(task_id, str) or TASK_ID.fullmatch(task_id) is None:
            raise QualificationError(f"{role}.task_id is malformed")
        if task_id in tasks_by_id:
            raise QualificationError("corpus task_id is duplicated")
        if task["task_class"] not in TASK_CLASSES:
            raise QualificationError(f"{role}.task_class is unsupported")
        _string(task["specification"], f"{role}.specification")
        if task["requirement_coverage"] not in ("known", "partial"):
            raise QualificationError(f"{role}.requirement_coverage is unsupported")
        required = task["required_evidence"]
        if not isinstance(required, list) or not 1 <= len(required) <= MAX_EVIDENCE:
            raise QualificationError(f"{role}.required_evidence is invalid")
        required_ids: set[str] = set()
        for evidence_index, evidence in enumerate(required):
            validated = _validate_required_evidence(
                evidence, f"{role}.required_evidence[{evidence_index}]"
            )
            if validated["id"] in required_ids:
                raise QualificationError(f"{role}.required_evidence locator is duplicated")
            if validated["kind"] == "range" and not validated["id"].startswith(
                "corpus-region:"
            ):
                raise QualificationError(f"{role} composite range lacks corpus-region namespace")
            required_ids.add(validated["id"])
        _string_list(task["uncertainty_boundaries"], f"{role}.uncertainty_boundaries")
        method = _exact_fields(
            task["validation_method"], {"kind", "description"},
            f"{role}.validation_method",
        )
        _string(method["kind"], f"{role}.validation_method.kind", maximum=128)
        _string(method["description"], f"{role}.validation_method.description")
        criteria = _string_list(
            task["accepted_outcome_criteria"], f"{role}.accepted_outcome_criteria"
        )
        if not criteria:
            raise QualificationError(f"{role}.accepted_outcome_criteria must not be empty")
        task_copy = dict(task)
        task_copy["task_spec_sha256"] = canonical_sha256(task)
        task_copy["validation_contract_sha256"] = _validation_contract_sha256(task)
        tasks_by_id[task_id] = task_copy
    if {task["task_class"] for task in tasks} != TASK_CLASSES:
        raise QualificationError("corpus does not cover the complete eleven-class task set")
    result = dict(corpus)
    result["corpus_sha256"] = canonical_sha256(corpus)
    result["tasks_by_id"] = tasks_by_id
    result["document"] = corpus
    return result


def load_corpus(workspace: Path, path: Path) -> dict[str, Any]:
    return _validate_corpus_value(load_json_document(workspace, path, "corpus"))


def _validate_policies(value: Any, role: str) -> dict[str, Any]:
    policies = _exact_fields(
        value,
        {
            "configuration_sha256", "provider_configuration_sha256",
            "model_identity_sha256", "agent_identity_sha256",
            "tool_configuration_sha256", "planner", "projection", "context_ir",
            "estimator", "metric",
        },
        role,
    )
    for field in (
        "configuration_sha256", "provider_configuration_sha256",
        "model_identity_sha256", "agent_identity_sha256",
        "tool_configuration_sha256",
    ):
        _hash(policies[field], f"{role}.{field}")
    for field in ("planner", "projection", "context_ir", "estimator", "metric"):
        _string(policies[field], f"{role}.{field}", maximum=256)
    if policies["metric"] != METRIC_POLICY_VERSION:
        raise QualificationError("metric policy version is incompatible")
    if policies["context_ir"] not in ("1.0.0", "2.0.0"):
        raise QualificationError("Context IR version is incompatible")
    return policies


def _validate_budget(value: Any, role: str) -> dict[str, Any]:
    budget = _exact_fields(value, {*BUDGET_FIELDS, "controlled_dimension"}, role)
    for field in BUDGET_FIELDS:
        _integer(budget[field], f"{role}.{field}")
    controlled = budget["controlled_dimension"]
    if controlled is not None and controlled not in BUDGET_FIELDS:
        raise QualificationError(f"{role}.controlled_dimension is unsupported")
    return budget


def _validate_coverage(value: Any, role: str) -> dict[str, bool]:
    coverage = _exact_fields(value, COVERAGE_FIELDS, role)
    for field in COVERAGE_FIELDS:
        _boolean(coverage[field], f"{role}.{field}")
    return coverage


def _validate_packet(value: Any, corpus: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    packet = _exact_fields(
        value,
        {
            "task_id", "corpus_sha256", "task_spec_sha256",
            "validation_contract_sha256", "workspace_identity_sha256",
            "repository_commit_before", "atlas_generation_before",
            "source_snapshot_sha256_before", "policies", "route", "budget",
            "observation_coverage", "sample_case_set_sha256",
        },
        "observation.packet",
    )
    task_id = packet["task_id"]
    if not isinstance(task_id, str) or TASK_ID.fullmatch(task_id) is None:
        raise QualificationError("observation.packet.task_id is malformed")
    task = corpus["tasks_by_id"].get(task_id)
    if task is None:
        raise QualificationError("observation task is absent from corpus")
    if packet["corpus_sha256"] != corpus["corpus_sha256"]:
        raise QualificationError("observation corpus hash does not match loaded corpus")
    if packet["task_spec_sha256"] != task["task_spec_sha256"]:
        raise QualificationError("observation task specification hash mismatch")
    if packet["validation_contract_sha256"] != task["validation_contract_sha256"]:
        raise QualificationError("observation validation contract hash mismatch")
    _hash(packet["workspace_identity_sha256"], "observation.packet.workspace_identity_sha256")
    _commit(packet["repository_commit_before"], "observation.packet.repository_commit_before")
    if packet["repository_commit_before"] != corpus["repository"]["commit"]:
        raise QualificationError("observation repository commit is not the frozen corpus commit")
    _string(
        packet["atlas_generation_before"],
        "observation.packet.atlas_generation_before",
        maximum=256,
    )
    _hash(
        packet["source_snapshot_sha256_before"],
        "observation.packet.source_snapshot_sha256_before",
    )
    _validate_policies(packet["policies"], "observation.packet.policies")
    if packet["route"] not in ("DIRECT", "ATLAS_LIGHT", "ATLAS_DEEP"):
        raise QualificationError("observation.packet.route is unsupported")
    _validate_budget(packet["budget"], "observation.packet.budget")
    _validate_coverage(
        packet["observation_coverage"],
        "observation.packet.observation_coverage",
    )
    _hash(packet["sample_case_set_sha256"], "observation.packet.sample_case_set_sha256")
    return packet, task


def _validate_locator_binding(
    value: Any,
    identity: dict[str, Any],
    source_revision_sha256: str,
    role: str,
) -> dict[str, Any] | None:
    if value is None:
        return None
    binding = _exact_fields(
        value, {"id", "mapping_sha256", "verified"}, f"{role}.benchmark_locator"
    )
    _string(binding["id"], f"{role}.benchmark_locator.id", maximum=1024)
    verified = _boolean(
        binding["verified"], f"{role}.benchmark_locator.verified"
    )
    if not verified:
        if binding["mapping_sha256"] is not None:
            raise QualificationError(
                f"{role}.benchmark_locator unverified mapping must not carry a hash"
            )
        return binding
    _hash(
        binding["mapping_sha256"],
        f"{role}.benchmark_locator.mapping_sha256",
    )
    expected = canonical_sha256({
        "benchmark_locator_id": binding["id"],
        "identity": identity,
        "source_revision_sha256": source_revision_sha256,
    })
    if binding["mapping_sha256"] != expected:
        raise QualificationError(f"{role}.benchmark_locator mapping hash mismatch")
    return binding


def _validate_evidence_ref(value: Any, role: str) -> dict[str, Any]:
    item = _exact_fields(
        value,
        {
            "identity", "source_revision_sha256", "provenance_sha256",
            "benchmark_locator",
        },
        role,
    )
    _validate_identity(item["identity"], f"{role}.identity")
    _hash(item["source_revision_sha256"], f"{role}.source_revision_sha256")
    _hash(item["provenance_sha256"], f"{role}.provenance_sha256")
    _validate_locator_binding(
        item["benchmark_locator"],
        item["identity"],
        item["source_revision_sha256"],
        role,
    )
    return item


def _validate_supplied(value: Any, role: str) -> dict[str, Any]:
    item = _exact_fields(
        value,
        {
            "identity", "source_revision_sha256", "provenance_sha256",
            "benchmark_locator", "source_bytes", "selected_span_bytes",
        },
        role,
    )
    _validate_evidence_ref(
        {
            "identity": item["identity"],
            "source_revision_sha256": item["source_revision_sha256"],
            "provenance_sha256": item["provenance_sha256"],
            "benchmark_locator": item["benchmark_locator"],
        },
        role,
    )
    _integer(item["source_bytes"], f"{role}.source_bytes")
    _integer(item["selected_span_bytes"], f"{role}.selected_span_bytes")
    return item


def _validate_atlas_cost(value: Any) -> dict[str, Any]:
    role = "observation.atlas_cost"
    fields = {
        "cpu_ms", "io_read_bytes", "io_write_bytes", "db_work_units",
        "visited_edges", "processed_files", "provider_scopes", "work_units",
        "retrieval_duration_ms", "retrieval_environment_sha256",
    }
    cost = _exact_fields(value, fields, role)
    _number(cost["cpu_ms"], f"{role}.cpu_ms", nullable=True)
    for field in (
        "io_read_bytes", "io_write_bytes", "db_work_units", "visited_edges",
        "processed_files", "provider_scopes", "work_units",
    ):
        _integer(cost[field], f"{role}.{field}", nullable=True)
    duration = _number(
        cost["retrieval_duration_ms"], f"{role}.retrieval_duration_ms", nullable=True
    )
    environment = cost["retrieval_environment_sha256"]
    if environment is not None:
        _hash(environment, f"{role}.retrieval_environment_sha256")
    if (duration is None) != (environment is None):
        raise QualificationError("retrieval duration and environment identity must be reported together")
    return cost


def _validate_context_cost(value: Any) -> dict[str, Any]:
    role = "observation.context_cost"
    cost = _exact_fields(
        value,
        {
            "records", "supplied_source_bytes", "selected_source_span_bytes",
            "estimated_tokens", "working_set",
        },
        role,
    )
    for field in (
        "records", "supplied_source_bytes", "selected_source_span_bytes",
        "estimated_tokens",
    ):
        _integer(cost[field], f"{role}.{field}", nullable=True)
    if cost["working_set"] is not None:
        if (
            not isinstance(cost["working_set"], list)
            or len(cost["working_set"]) > MAX_EVIDENCE
        ):
            raise QualificationError(
                f"{role}.working_set must be null or a bounded array"
            )
        for index, identity in enumerate(cost["working_set"]):
            _validate_identity(identity, f"{role}.working_set[{index}]")
    return cost


def _validate_agent_cost(value: Any) -> dict[str, Any]:
    role = "observation.agent_cost"
    cost = _exact_fields(
        value,
        {
            "input_tokens", "output_tokens", "tool_calls", "independent_reads",
            "expansion", "rediscovery",
        },
        role,
    )
    for field in cost:
        _integer(cost[field], f"{role}.{field}", nullable=True)
    return cost

def _validate_acquisition(value: Any) -> dict[str, Any]:
    acquisition = _exact_fields(
        value, {"status", "coverage", "native"}, "observation.acquisition"
    )
    status = acquisition["status"]
    if status not in (None, "blocked", "partial", "sufficient"):
        raise QualificationError("observation.acquisition.status is unsupported")
    if acquisition["coverage"] not in ("complete", "partial", "unavailable"):
        raise QualificationError("observation.acquisition.coverage is unsupported")
    _boolean(acquisition["native"], "observation.acquisition.native")
    if status is None and acquisition["coverage"] != "unavailable":
        raise QualificationError(
            "null acquisition status requires unavailable acquisition coverage"
        )
    return acquisition


def _validate_task_result(value: Any) -> dict[str, Any]:
    role = "observation.task_result"
    result = _exact_fields(
        value,
        {
            "agent_reported_accepted", "tests_validation", "correct_target",
            "correctness", "outcome_artifact_sha256", "time_to_correct_target_ms",
            "time_to_accepted_result_ms",
        },
        role,
    )
    _boolean(result["agent_reported_accepted"], f"{role}.agent_reported_accepted", nullable=True)
    _string_list(result["tests_validation"], f"{role}.tests_validation")
    _boolean(result["correct_target"], f"{role}.correct_target", nullable=True)
    _boolean(result["correctness"], f"{role}.correctness", nullable=True)
    if result["outcome_artifact_sha256"] is not None:
        _hash(result["outcome_artifact_sha256"], f"{role}.outcome_artifact_sha256")
    _number(result["time_to_correct_target_ms"], f"{role}.time_to_correct_target_ms", nullable=True)
    _number(result["time_to_accepted_result_ms"], f"{role}.time_to_accepted_result_ms", nullable=True)
    return result


def _validate_error(value: Any) -> dict[str, Any] | None:
    if value is None:
        return None
    error = _exact_fields(value, {"kind", "detail"}, "observation.error")
    _string(error["kind"], "observation.error.kind", maximum=128)
    _string(error["detail"], "observation.error.detail")
    return error


def _validate_receipt(
    value: Any,
    packet: dict[str, Any],
    packet_sha256: str,
    observation_sha256: str,
    task_result: dict[str, Any],
    acquisition: dict[str, Any],
    error: dict[str, Any] | None,
) -> dict[str, Any] | None:
    if value is None:
        return None
    receipt = _exact_fields(
        value,
        {
            "schema_version", "kind", "receipt_id", "task_id", "corpus_sha256",
            "task_spec_sha256", "validation_contract_sha256", "packet_sha256",
            "observation_sha256", "workspace_identity_sha256",
            "repository_commit", "atlas_generation", "source_snapshot_sha256",
            "policies_sha256", "outcome_artifact_sha256",
            "validation_outcome", "attestation_kind", "evidence_summary",
            "validator_identity_sha256",
        },
        "validation_receipt",
    )
    if (
        receipt["schema_version"] != RECEIPT_VERSION
        or receipt["kind"] != "atlas-context-qualification-validation-receipt"
    ):
        raise QualificationError("validation receipt schema is incompatible")
    _string(receipt["receipt_id"], "validation_receipt.receipt_id", maximum=256)
    if task_result["outcome_artifact_sha256"] is None:
        raise QualificationError(
            "validation receipt requires a non-null outcome artifact hash"
        )
    expected = {
        "task_id": packet["task_id"],
        "corpus_sha256": packet["corpus_sha256"],
        "task_spec_sha256": packet["task_spec_sha256"],
        "validation_contract_sha256": packet["validation_contract_sha256"],
        "packet_sha256": packet_sha256,
        "observation_sha256": observation_sha256,
        "workspace_identity_sha256": packet["workspace_identity_sha256"],
        "repository_commit": packet["repository_commit_before"],
        "atlas_generation": packet["atlas_generation_before"],
        "source_snapshot_sha256": packet["source_snapshot_sha256_before"],
        "policies_sha256": canonical_sha256(packet["policies"]),
        "outcome_artifact_sha256": task_result["outcome_artifact_sha256"],
    }
    for field, expected_value in expected.items():
        if receipt[field] != expected_value:
            raise QualificationError(f"validation receipt binding mismatch: {field}")
    if receipt["validation_outcome"] not in ("passed", "failed"):
        raise QualificationError("validation receipt outcome is unsupported")
    if receipt["attestation_kind"] not in (
        "operator_reported", "validator_reported",
    ):
        raise QualificationError("validation receipt attestation kind is unsupported")
    _string(receipt["evidence_summary"], "validation_receipt.evidence_summary")
    _hash(
        receipt["validator_identity_sha256"],
        "validation_receipt.validator_identity_sha256",
    )
    if receipt["validation_outcome"] == "passed":
        if (
            error is not None
            or task_result["agent_reported_accepted"] is False
            or task_result["correct_target"] is not True
            or task_result["correctness"] is not True
            or acquisition["status"] == "blocked"
        ):
            raise QualificationError(
                "passed validation receipt contradicts the observed task result"
            )
    return receipt


def _fraction(
    numerator: int | float | None,
    denominator: int | float | None,
    unit: str,
    *,
    unavailable_reason: str | None = None,
) -> dict[str, Any]:
    if unavailable_reason is not None:
        return {
            "state": "unavailable", "reason": unavailable_reason,
            "numerator": numerator, "denominator": denominator, "unit": unit,
            "ratio": None,
        }
    if denominator is None:
        return {
            "state": "unavailable", "reason": "denominator_unavailable",
            "numerator": numerator, "denominator": None, "unit": unit,
            "ratio": None,
        }
    if denominator == 0:
        return {
            "state": "unavailable", "reason": "zero_denominator",
            "numerator": numerator, "denominator": denominator, "unit": unit,
            "ratio": None,
        }
    if numerator is None:
        raise QualificationError("available metric lacks a numerator")
    return {
        "state": "available", "numerator": numerator, "denominator": denominator,
        "unit": unit, "ratio": numerator / denominator,
    }


def _unavailable_value(reason: str) -> dict[str, Any]:
    return {"state": "unavailable", "reason": reason, "value": None}


def _reported_value(value: bool | None, coverage: bool) -> dict[str, Any]:
    if not coverage:
        return _unavailable_value("coverage_unavailable")
    if value is None:
        return _unavailable_value("not_reported")
    return {"state": "available", "value": value}


def _metrics(
    task: dict[str, Any],
    packet: dict[str, Any],
    supplied_by_key: dict[str, dict[str, Any]],
    reads: list[dict[str, Any]],
    reported: list[dict[str, Any]],
    atlas_cost: dict[str, Any],
    task_state: str,
    receipt: dict[str, Any] | None,
) -> dict[str, Any]:
    coverage = packet["observation_coverage"]
    supplied_keys = set(supplied_by_key)
    read_keys = {_identity_key(item["identity"]) for item in reads}
    reported_keys = {_identity_key(item["identity"]) for item in reported}

    observed_reason = (
        None
        if coverage["context_cost"] and coverage["read_requests"]
        else "capture_coverage_unavailable"
    )
    reported_reason = (
        None
        if coverage["context_cost"] and coverage["reported_use"]
        else "capture_coverage_unavailable"
    )
    observed_precision = _fraction(
        len(read_keys & supplied_keys),
        len(supplied_keys),
        "items",
        unavailable_reason=observed_reason,
    )
    reported_precision = _fraction(
        len(reported_keys & supplied_keys),
        len(supplied_keys),
        "items",
        unavailable_reason=reported_reason,
    )

    required = {
        item["id"]: item["source_revision_sha256"]
        for item in task["required_evidence"]
    }
    supplied_locators = {
        item["benchmark_locator"]["id"]: item["source_revision_sha256"]
        for item in supplied_by_key.values()
        if item["benchmark_locator"] is not None
        and item["benchmark_locator"]["verified"]
    }
    required_found = sum(
        supplied_locators.get(locator_id) == revision
        for locator_id, revision in required.items()
    )
    recall = _fraction(
        required_found,
        len(required),
        "items",
        unavailable_reason=(
            None if coverage["context_cost"] else "capture_coverage_unavailable"
        ),
    )
    recall["requirement_coverage"] = task["requirement_coverage"]

    expansion_keys = (read_keys | reported_keys) - supplied_keys
    expansion = (
        {"state": "available", "count": len(expansion_keys), "unit": "items"}
        if (
            coverage["context_cost"]
            and coverage["read_requests"]
            and coverage["reported_use"]
        )
        else {
            "state": "unavailable",
            "reason": "capture_coverage_unavailable",
            "count": None,
            "unit": "items",
        }
    )
    rediscovery = _fraction(
        sum(_identity_key(item["identity"]) in supplied_keys for item in reads),
        len(reads),
        "requests",
        unavailable_reason=observed_reason,
    )
    supplied_bytes = _sum_bounded(
        (item["source_bytes"] for item in supplied_by_key.values()),
        "supplied source-byte sum",
    )
    observed_bytes = _sum_bounded(
        (
            item["source_bytes"]
            for key, item in supplied_by_key.items()
            if key in read_keys
        ),
        "observed source-byte sum",
    )
    efficiency = _fraction(
        observed_bytes,
        supplied_bytes,
        "bytes",
        unavailable_reason=observed_reason,
    )
    working_set = (
        {
            "state": "available",
            "identities": len(supplied_keys),
            "source_bytes": supplied_bytes,
            "unit": "identities",
        }
        if coverage["context_cost"]
        else {
            "state": "unavailable",
            "reason": "capture_coverage_unavailable",
            "identities": None,
            "source_bytes": None,
            "unit": "identities",
        }
    )

    useful_items = len(reported_keys & supplied_keys)
    actual_work = _fraction(
        None,
        atlas_cost["work_units"],
        "items_per_work_unit",
        unavailable_reason="independent_execution_unverified",
    )
    actual_latency = _fraction(
        None,
        atlas_cost["retrieval_duration_ms"],
        "items_per_ms",
        unavailable_reason="independent_execution_unverified",
    )
    actual_latency["environment_identity_sha256"] = (
        atlas_cost["retrieval_environment_sha256"]
    )

    reported_throughput_reason = None
    if task_state != "reported_accepted" or receipt is None:
        reported_throughput_reason = "reported_acceptance_unavailable"
    elif not (
        coverage["atlas_cost"]
        and coverage["context_cost"]
        and coverage["reported_use"]
        and coverage["task_result"]
        and coverage["validation"]
    ):
        reported_throughput_reason = "capture_coverage_unavailable"
    reported_work = _fraction(
        useful_items if reported_throughput_reason is None else None,
        atlas_cost["work_units"],
        "reported_items_per_work_unit",
        unavailable_reason=reported_throughput_reason,
    )
    reported_latency = _fraction(
        useful_items if reported_throughput_reason is None else None,
        atlas_cost["retrieval_duration_ms"],
        "reported_items_per_ms",
        unavailable_reason=reported_throughput_reason,
    )
    reported_latency["environment_identity_sha256"] = (
        atlas_cost["retrieval_environment_sha256"]
    )
    return {
        "observed_precision": observed_precision,
        "reported_use_precision": reported_precision,
        "required_evidence_recall": recall,
        "expansion": expansion,
        "rediscovery_rate": rediscovery,
        "observed_source_efficiency": efficiency,
        "working_set": working_set,
        "useful_context_throughput_per_work_unit": actual_work,
        "environment_qualified_useful_context_throughput_per_ms": actual_latency,
        "reported_useful_items_per_attested_work_unit": reported_work,
        "reported_environment_qualified_useful_items_per_attested_ms": reported_latency,
    }


def _semantic_report_material(
    identity: dict[str, Any],
    measurements: dict[str, Any],
    metrics: dict[str, Any],
    task_outcome: dict[str, Any],
    acquisition: dict[str, Any],
) -> dict[str, Any]:
    semantic_identity = {
        key: value
        for key, value in identity.items()
        if key not in {"observation_id", "observation_sha256"}
    }
    semantic_atlas_cost = {
        key: value
        for key, value in measurements["atlas_cost"].items()
        if key not in {
            "cpu_ms",
            "retrieval_duration_ms",
            "retrieval_environment_sha256",
        }
    }
    semantic_task_result = {
        key: value
        for key, value in measurements["task_result"].items()
        if key not in {"time_to_correct_target_ms", "time_to_accepted_result_ms"}
    }
    semantic_metrics = {
        key: value
        for key, value in metrics.items()
        if key not in {
            "environment_qualified_useful_context_throughput_per_ms",
            "reported_environment_qualified_useful_items_per_attested_ms",
        }
    }
    return {
        "identity": semantic_identity,
        "measurements": {
            "atlas_cost": semantic_atlas_cost,
            "context_cost": measurements["context_cost"],
            "agent_cost": measurements["agent_cost"],
            "task_result": semantic_task_result,
        },
        "metrics": semantic_metrics,
        "task_outcome": task_outcome,
        "acquisition": acquisition,
    }


def build_report(corpus: dict[str, Any], value: Any) -> dict[str, Any]:
    document = _exact_fields(
        value,
        {"schema_version", "kind", "observation", "validation_receipt"},
        "input",
    )
    if (
        document["schema_version"] != INPUT_VERSION
        or document["kind"] != "atlas-context-qualification-input"
    ):
        raise QualificationError("input schema is incompatible")
    observation = _exact_fields(
        document["observation"],
        {
            "schema_version", "kind", "observation_id", "packet", "packet_sha256",
            "after_binding", "supplied", "read_requests", "reported_used",
            "atlas_cost", "context_cost", "agent_cost", "acquisition",
            "task_result", "error",
        },
        "observation",
    )
    if (
        observation["schema_version"] != OBSERVATION_VERSION
        or observation["kind"] != "atlas-context-qualification-observation"
    ):
        raise QualificationError("observation schema is incompatible")
    _string(
        observation["observation_id"],
        "observation.observation_id",
        maximum=256,
    )
    packet, task = _validate_packet(observation["packet"], corpus)
    _hash(observation["packet_sha256"], "observation.packet_sha256")
    expected_packet_hash = canonical_sha256(packet)
    if observation["packet_sha256"] != expected_packet_hash:
        raise QualificationError("observation packet hash mismatch")

    after = _exact_fields(
        observation["after_binding"],
        {
            "repository_commit_after", "atlas_generation_after",
            "source_snapshot_sha256_after",
        },
        "observation.after_binding",
    )
    for after_field, before_field in (
        ("repository_commit_after", "repository_commit_before"),
        ("atlas_generation_after", "atlas_generation_before"),
        ("source_snapshot_sha256_after", "source_snapshot_sha256_before"),
    ):
        if after[after_field] != packet[before_field]:
            raise QualificationError(
                f"{before_field.removesuffix('_before')} changed during observation"
            )

    supplied = observation["supplied"]
    reads = observation["read_requests"]
    reported = observation["reported_used"]
    if (
        not isinstance(supplied, list)
        or not isinstance(reads, list)
        or not isinstance(reported, list)
    ):
        raise QualificationError("observation evidence collections must be arrays")
    if len(supplied) + len(reads) + len(reported) > MAX_EVIDENCE:
        raise QualificationError(
            f"observation evidence exceeds the {MAX_EVIDENCE} record bound"
        )

    supplied_by_key: dict[str, dict[str, Any]] = {}
    revisions_by_identity: dict[str, str] = {}
    locator_mappings: dict[str, tuple[str, str]] = {}

    def register_reference(item: dict[str, Any], role: str) -> None:
        key = _identity_key(item["identity"])
        revision = item["source_revision_sha256"]
        previous_revision = revisions_by_identity.setdefault(key, revision)
        if previous_revision != revision:
            raise QualificationError(
                f"{role} conflicts with another source revision for the same identity"
            )
        locator = item["benchmark_locator"]
        if locator is not None and locator["verified"]:
            mapping = (key, revision)
            previous_mapping = locator_mappings.setdefault(locator["id"], mapping)
            if previous_mapping != mapping:
                raise QualificationError(
                    f"{role} conflicts with another verified benchmark locator mapping"
                )

    for index, item in enumerate(supplied):
        role = f"observation.supplied[{index}]"
        validated = _validate_supplied(item, role)
        register_reference(validated, role)
        key = _identity_key(validated["identity"])
        previous = supplied_by_key.get(key)
        if previous is not None and (
            previous["source_revision_sha256"]
            != validated["source_revision_sha256"]
            or previous["source_bytes"] != validated["source_bytes"]
            or previous["selected_span_bytes"] != validated["selected_span_bytes"]
        ):
            raise QualificationError("conflicting supplied costs or source revisions")
        supplied_by_key.setdefault(key, validated)
    for collection_name, collection in (
        ("read_requests", reads),
        ("reported_used", reported),
    ):
        for index, item in enumerate(collection):
            role = f"observation.{collection_name}[{index}]"
            validated = _validate_evidence_ref(item, role)
            register_reference(validated, role)

    atlas_cost = _validate_atlas_cost(observation["atlas_cost"])
    context_cost = _validate_context_cost(observation["context_cost"])
    agent_cost = _validate_agent_cost(observation["agent_cost"])
    acquisition = _validate_acquisition(observation["acquisition"])
    task_result = _validate_task_result(observation["task_result"])
    error = _validate_error(observation["error"])
    context_coverage = packet["observation_coverage"]["context_cost"]
    expected_context = {
        "records": len(supplied),
        "supplied_source_bytes": _sum_bounded(
            (item["source_bytes"] for item in supplied_by_key.values()),
            "context supplied source-byte sum",
        ),
        "selected_source_span_bytes": _sum_bounded(
            (item["selected_span_bytes"] for item in supplied_by_key.values()),
            "context selected span-byte sum",
        ),
    }
    for field, expected in expected_context.items():
        actual = context_cost[field]
        if actual is None:
            if context_coverage:
                raise QualificationError(
                    f"context coverage requires observed {field}"
                )
        elif actual != expected:
            raise QualificationError(
                f"context cost {field} does not match supplied evidence"
            )
    working_set = context_cost["working_set"]
    if working_set is None:
        if context_coverage:
            raise QualificationError(
                "context coverage requires observed working set identities"
            )
    else:
        working_set_keys = [_identity_key(item) for item in working_set]
        if (
            len(working_set_keys) != len(set(working_set_keys))
            or set(working_set_keys) != set(supplied_by_key)
        ):
            raise QualificationError(
                "context working set identities do not match supplied identities"
            )

    observation_sha256 = canonical_sha256(observation)
    receipt = _validate_receipt(
        document["validation_receipt"],
        packet,
        expected_packet_hash,
        observation_sha256,
        task_result,
        acquisition,
        error,
    )
    coverage = packet["observation_coverage"]
    if receipt is None or not (
        coverage["task_result"] and coverage["validation"]
    ):
        task_state = "unavailable"
    elif receipt["validation_outcome"] == "passed":
        task_state = "reported_accepted"
    else:
        task_state = "reported_rejected"

    task_outcome = {
        "state": task_state,
        "attestation_kind": receipt["attestation_kind"] if receipt else None,
        "validation_execution_verified": False,
        "qualified_for_optimization": False,
        "outcome_artifact_sha256": task_result["outcome_artifact_sha256"],
        "correct_target": _reported_value(
            task_result["correct_target"], coverage["task_result"]
        ),
        "correctness": _reported_value(
            task_result["correctness"], coverage["task_result"]
        ),
    }
    metrics = _metrics(
        task,
        packet,
        supplied_by_key,
        reads,
        reported,
        atlas_cost,
        task_state,
        receipt,
    )
    identity = {
        "observation_id": observation["observation_id"],
        "observation_sha256": observation_sha256,
        "task_id": packet["task_id"],
        "corpus_sha256": packet["corpus_sha256"],
        "task_spec_sha256": packet["task_spec_sha256"],
        "validation_contract_sha256": packet["validation_contract_sha256"],
        "packet_sha256": expected_packet_hash,
        "workspace_identity_sha256": packet["workspace_identity_sha256"],
        "repository_commit": packet["repository_commit_before"],
        "atlas_generation": packet["atlas_generation_before"],
        "source_snapshot_sha256": packet["source_snapshot_sha256_before"],
        "policies": packet["policies"],
        "policies_sha256": canonical_sha256(packet["policies"]),
        "route": packet["route"],
        "budget": packet["budget"],
        "observation_coverage": packet["observation_coverage"],
        "sample_case_set_sha256": packet["sample_case_set_sha256"],
    }
    measurements = {
        "atlas_cost": atlas_cost,
        "context_cost": context_cost,
        "agent_cost": agent_cost,
        "task_result": task_result,
    }
    report: dict[str, Any] = {
        "schema_version": REPORT_VERSION,
        "kind": "atlas-context-qualification-report",
        "harness_version": HARNESS_VERSION,
        "corpus": corpus["document"],
        "normalized_input": document,
        "identity": identity,
        "measurements": measurements,
        "metrics": metrics,
        "acquisition": acquisition,
        "task_outcome": task_outcome,
    }
    report["semantic_report_sha256"] = canonical_sha256(
        _semantic_report_material(
            identity,
            measurements,
            metrics,
            task_outcome,
            acquisition,
        )
    )
    report["report_sha256"] = canonical_sha256(report)
    if len(_canonical_bytes(report)) > MAX_DOCUMENT_BYTES:
        raise QualificationError("report exceeds the document byte bound")
    return report


def _validate_report(report: Any, role: str) -> dict[str, Any]:
    value = _exact_fields(
        report,
        {
            "schema_version", "kind", "harness_version", "corpus",
            "normalized_input", "identity", "measurements", "metrics",
            "acquisition", "task_outcome", "semantic_report_sha256",
            "report_sha256",
        },
        role,
    )
    if (
        value["schema_version"] != REPORT_VERSION
        or value["kind"] != "atlas-context-qualification-report"
        or value["harness_version"] != HARNESS_VERSION
    ):
        raise QualificationError(f"{role} version is incompatible")
    _hash(value["semantic_report_sha256"], f"{role}.semantic_report_sha256")
    _hash(value["report_sha256"], f"{role}.report_sha256")
    corpus = _validate_corpus_value(value["corpus"])
    expected = build_report(corpus, value["normalized_input"])
    if value != expected:
        raise QualificationError(
            f"{role} content, nested schema, or hash does not rebuild exactly"
        )
    return value


def _comparison_budget(left: dict[str, Any], right: dict[str, Any]) -> str | None:
    left_budget = left["budget"]
    right_budget = right["budget"]
    if left_budget == right_budget:
        return left_budget["controlled_dimension"]
    controlled = left_budget["controlled_dimension"]
    if controlled is None or controlled != right_budget["controlled_dimension"]:
        raise QualificationError("budgets differ without one declared controlled dimension")
    differences = {
        field for field in BUDGET_FIELDS if left_budget[field] != right_budget[field]
    }
    if differences != {controlled}:
        raise QualificationError("budgets differ outside the declared controlled dimension")
    return controlled


def compare_reports(left: Any, right: Any) -> dict[str, Any]:
    left_report = _validate_report(left, "left report")
    right_report = _validate_report(right, "right report")
    if (
        left_report["task_outcome"]["state"] != "reported_accepted"
        or right_report["task_outcome"]["state"] != "reported_accepted"
    ):
        raise QualificationError(
            "comparison requires two attested reported-accepted outcomes"
        )
    left_identity = left_report["identity"]
    right_identity = right_report["identity"]
    for field, description in (
        ("task_id", "task identity"),
        ("corpus_sha256", "corpus identity"),
        ("task_spec_sha256", "task specification identity"),
        ("validation_contract_sha256", "validation contract identity"),
        ("workspace_identity_sha256", "workspace identity"),
        ("atlas_generation", "generation"),
        ("repository_commit", "commit"),
        ("source_snapshot_sha256", "source"),
        ("policies", "policies"),
        ("route", "route"),
        ("observation_coverage", "coverage"),
        ("sample_case_set_sha256", "sample/case set"),
    ):
        if left_identity[field] != right_identity[field]:
            raise QualificationError(f"comparison {description} mismatch")
    if left_report["acquisition"] != right_report["acquisition"]:
        raise QualificationError("comparison acquisition status or coverage mismatch")
    left_outcome = left_report["task_outcome"]["outcome_artifact_sha256"]
    right_outcome = right_report["task_outcome"]["outcome_artifact_sha256"]
    if left_outcome is None or right_outcome is None or left_outcome != right_outcome:
        raise QualificationError("comparison outcome identity mismatch")
    controlled = _comparison_budget(left_identity, right_identity)
    comparison: dict[str, Any] = {
        "schema_version": COMPARISON_VERSION,
        "kind": "atlas-context-qualification-comparison",
        "harness_version": HARNESS_VERSION,
        "state": "comparable_attested_inputs",
        "qualified_for_optimization": False,
        "task_id": left_identity["task_id"],
        "left_report_sha256": left_report["report_sha256"],
        "right_report_sha256": right_report["report_sha256"],
        "outcome_artifact_sha256": left_outcome,
        "controlled_dimension": controlled,
        "left_metrics": left_report["metrics"],
        "right_metrics": right_report["metrics"],
    }
    comparison["comparison_sha256"] = canonical_sha256(comparison)
    return comparison


def _load_report(workspace: Path, path: Path, role: str) -> dict[str, Any]:
    return _validate_report(load_json_document(workspace, path, role), role)


def _write_output(workspace: Path, path: Path, value: Any) -> None:
    workspace = workspace.absolute()
    candidate = path if path.is_absolute() else workspace / path
    try:
        candidate = _EXPORTER._contained(candidate, workspace, "output")
        if candidate.exists() or candidate.is_symlink():
            raise QualificationError("output already exists")
        _EXPORTER._safe_ancestors(candidate, workspace, False)
        payload = _canonical_bytes(value)
        if len(payload) > MAX_DOCUMENT_BYTES:
            raise QualificationError("output exceeds the document byte bound")
        _EXPORTER._write_exclusive(candidate, payload)
    except QualificationError:
        raise
    except _EXPORTER.ExportError as error:
        raise QualificationError(str(error)) from error
    except OSError as error:
        raise QualificationError("output could not be written") from error


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    report_parser = subparsers.add_parser("report", help="validate one input and write a report")
    report_parser.add_argument("--workspace", type=Path, required=True)
    report_parser.add_argument("--corpus", type=Path, required=True)
    report_parser.add_argument("--input", type=Path, required=True)
    report_parser.add_argument("--output", type=Path, required=True)
    compare_parser = subparsers.add_parser("compare", help="compare two validated reports")
    compare_parser.add_argument("--workspace", type=Path, required=True)
    compare_parser.add_argument("--left", type=Path, required=True)
    compare_parser.add_argument("--right", type=Path, required=True)
    compare_parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(arguments)
    try:
        if args.command == "report":
            corpus = load_corpus(args.workspace, args.corpus)
            input_value = load_json_document(args.workspace, args.input, "input")
            output = build_report(corpus, input_value)
        else:
            left = _load_report(args.workspace, args.left, "left report")
            right = _load_report(args.workspace, args.right, "right report")
            output = compare_reports(left, right)
        _write_output(args.workspace, args.output, output)
    except QualificationError as error:
        print(f"qualification error: {error}", file=sys.stderr)
        return 2
    print(json.dumps({
        "kind": output["kind"],
        "sha256": output.get("report_sha256", output.get("comparison_sha256")),
        "state": output.get("state", output.get("task_outcome", {}).get("state")),
    }, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
