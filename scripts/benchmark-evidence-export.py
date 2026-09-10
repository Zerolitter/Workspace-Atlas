#!/usr/bin/env python3
"""Export bounded Atlas JSON or NDJSON observations as a portable evidence bundle."""
from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import stat
import sys
from typing import Any

SCHEMA_VERSION = "1.0.0"
MAX_INPUT_BYTES = 512 * 1024 * 1024
MAX_RECORD_BYTES = 8 * 1024 * 1024
MAX_DOCUMENT_BYTES = 1024 * 1024
MAX_RECORDS = 200_000
ENVIRONMENT_KEYS = frozenset({
    "atlas_version", "rustc_version", "toolchain", "os", "architecture", "cpu"
})
CSV_COLUMNS = (
    "task_id", "repetition", "arm", "accepted", "accepted_state", "exit_code",
    "elapsed_ms", "runner_wall_time_ms", "adapter_elapsed_ms",
    "tokens_input", "tokens_output", "tokens_total", "tool_calls",
    "files_read", "source_bytes_read", "atlas_route", "atlas_runtime_ms",
    "context_expansion", "error_kind", "record_json",
)

CSV_NULL_TOKEN = "<null>"
CSV_MISSING_TOKEN = "<missing>"
HOME_OR_ABSOLUTE = re.compile(
    r"(?:^[A-Za-z]:[\\/]|^/|^\\\\|^~|%(?:USERPROFILE|HOME)%|"
    r"\$(?:HOME|USERPROFILE)|/(?:home|Users)/|[\\/]Users[\\/])",
    re.IGNORECASE,
)


SECRET_VALUE_PATTERN = re.compile(
    r"(?i)(?:^|[\s,;|])(?:password|passwd|pwd|secret|api[-_]?key|access[-_]?key|"
    r"credential|auth[-_]?token|bearer|private[-_]?key|token)\s*[:=]\s*[^,;|\s][^,;|]*"
    r"|(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{6,}",
)


class ExportError(RuntimeError):
    """Input or output cannot satisfy the evidence bundle contract."""


def _duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ExportError(f"duplicate JSON field: {key}")
        result[key] = value
    return result

def _invalid_constant(value: str) -> Any:
    raise ExportError(f"non-finite JSON number is forbidden: {value}")


def _json_load(text: str, role: str) -> Any:
    try:
        return json.loads(
            text, object_pairs_hook=_duplicates, parse_constant=_invalid_constant
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ExportError(f"{role} is malformed JSON") from error


def _is_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & 0x400)


def _identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def _safe_metadata(path: Path, role: str, kind: str) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ExportError(f"cannot inspect {role}") from error
    if stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
        raise ExportError(f"{role} must not be a link or reparse point")
    if kind == "file" and not stat.S_ISREG(metadata.st_mode):
        raise ExportError(f"{role} must be a regular file")
    if kind == "directory" and not stat.S_ISDIR(metadata.st_mode):
        raise ExportError(f"{role} must be a directory")
    return metadata


def _contained(path: Path, workspace: Path, role: str) -> Path:
    absolute = path.absolute()
    try:
        absolute.relative_to(workspace)
    except ValueError as error:
        raise ExportError(f"{role} escapes workspace") from error
    return absolute


def _safe_ancestors(path: Path, workspace: Path, include_path: bool) -> None:
    current = path if include_path else path.parent
    while True:
        if current.exists() or current.is_symlink():
            _safe_metadata(current, "path component", "directory")
        if current == workspace:
            break
        if current.parent == current:
            raise ExportError("path ancestry escapes workspace")
        current = current.parent


def _read_bounded_file(path: Path, maximum: int, role: str) -> bytes:
    before = _safe_metadata(path, role, "file")
    if before.st_size > maximum:
        raise ExportError(f"{role} exceeds byte bound")
    expected = _identity(before)
    try:
        with path.open("rb") as source:
            opened = os.fstat(source.fileno())
            if _identity(opened) != expected or not stat.S_ISREG(opened.st_mode):
                raise ExportError(f"{role} changed before read")
            data = source.read(maximum + 1)
            if len(data) > maximum:
                raise ExportError(f"{role} exceeds byte bound")
            if _identity(os.fstat(source.fileno())) != expected:
                raise ExportError(f"{role} changed while read")
    except OSError as error:
        raise ExportError(f"{role} is unreadable") from error
    if _identity(_safe_metadata(path, role, "file")) != expected:
        raise ExportError(f"{role} changed while read")
    return data


def _read_input(path: Path) -> tuple[bytes, list[dict[str, Any]], str, str]:
    raw_bytes = _read_bounded_file(path, MAX_INPUT_BYTES, "input")
    try:
        text = raw_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ExportError("input is unreadable UTF-8") from error
    state = "complete"
    if path.suffix.lower() == ".ndjson":
        records: list[dict[str, Any]] = []
        for number, line in enumerate(text.splitlines(), 1):
            if not line.strip():
                raise ExportError(f"NDJSON line {number} is empty")
            if len(line.encode("utf-8")) > MAX_RECORD_BYTES:
                raise ExportError("NDJSON record exceeds byte bound")
            value = _json_load(line, f"NDJSON line {number}")
            if not isinstance(value, dict):
                raise ExportError("every observation must be a JSON object")
            records.append(value)
            if len(records) > MAX_RECORDS:
                raise ExportError("input exceeds record bound")
        input_format = "ndjson"
    else:
        value = _json_load(text, "input")
        if isinstance(value, list):
            records = value
        elif isinstance(value, dict) and "records" in value:
            records = value["records"]
            state = value.get("state", "complete")
            count = value.get("raw_count", len(records) if isinstance(records, list) else None)
            if isinstance(records, list) and count != len(records):
                raise ExportError("declared raw_count does not match records")
        elif isinstance(value, dict):
            records = [value]
        else:
            raise ExportError("JSON input must be an object or object array")
        if state not in ("partial", "complete"):
            raise ExportError("input state must be partial or complete")
        if not isinstance(records, list) or len(records) > MAX_RECORDS or not all(isinstance(row, dict) for row in records):
            raise ExportError("records must be a bounded array of objects")
        for record in records:
            if len(json.dumps(record, ensure_ascii=False).encode("utf-8")) > MAX_RECORD_BYTES:
                raise ExportError("JSON record exceeds byte bound")
        input_format = "json"
    return raw_bytes, records, input_format, state


def _canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def _hash(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _identity_sidecar(
    source: Path,
    workspace: Path,
    raw_bytes: bytes,
    records: list[dict[str, Any]],
) -> tuple[str | None, dict[str, Any] | None]:
    sidecar: Path | None = None
    sidecar_kind = ""
    record_count = len(records)
    if source.name == "raw.ndjson":
        candidate = source.with_name("harness-manifest.json")
        if candidate.exists() or candidate.is_symlink():
            sidecar, sidecar_kind = candidate, "harness"
        elif any(
            record.get("kind") == "local-atlas-ab-observation"
            for record in records
        ):
            return "partial", {
                "kind": "missing_harness_identity",
                "name": "harness-manifest.json",
                "raw_shape": "harness",
                "record_count": record_count,
            }
        else:
            return None, None
    elif source.name in (
        "capacity-raw-observations-v1.ndjson",
        "test-capacity-raw-observations-v1.ndjson",
    ):
        name = (
            "capacity-summary-v1.json"
            if source.name.startswith("capacity-")
            else "test-capacity-summary-v1.json"
        )
        candidate = source.with_name(name)
        if candidate.exists() or candidate.is_symlink():
            sidecar, sidecar_kind = candidate, "capacity"
        else:
            return "partial", {
                "kind": "missing_capacity_identity",
                "name": name,
                "raw_shape": "capacity",
                "record_count": record_count,
            }
    if sidecar is None:
        return None, None
    _safe_ancestors(sidecar, workspace, False)
    sidecar_bytes = _read_bounded_file(sidecar, MAX_DOCUMENT_BYTES, "identity sidecar")
    try:
        value = _json_load(sidecar_bytes.decode("utf-8"), "identity sidecar")
    except UnicodeDecodeError as error:
        raise ExportError("identity sidecar is unreadable UTF-8") from error
    if not isinstance(value, dict) or value.get("schema_version") != SCHEMA_VERSION:
        raise ExportError("identity sidecar schema is malformed")
    raw_sha256 = _hash(raw_bytes)
    if value.get("raw_sha256") != raw_sha256:
        raise ExportError("identity sidecar raw hash mismatch")
    if sidecar_kind == "harness":
        observed = value.get("raw_count")
        expected = value.get("expected_raw_count", observed)
        state = value.get("state")
        if (
            not isinstance(observed, int)
            or isinstance(observed, bool)
            or observed != record_count
            or not isinstance(expected, int)
            or isinstance(expected, bool)
            or observed > expected
            or state not in ("partial", "complete")
            or (state == "complete" and observed != expected)
        ):
            raise ExportError("harness identity count or state mismatch")
    else:
        attempted = value.get("attempted_records")
        missing = value.get("missing_records")
        duplicate = value.get("duplicate_records")
        if (
            value.get("kind") != "capacity-evidence-summary"
            or not all(isinstance(item, int) and not isinstance(item, bool) and item >= 0 for item in (attempted, missing, duplicate))
            or attempted != record_count
        ):
            raise ExportError("capacity summary count or schema mismatch")
        state = "complete" if missing == 0 and duplicate == 0 else "partial"
    return state, {
        "bytes": len(sidecar_bytes),
        "kind": sidecar_kind,
        "name": sidecar.name,
        "sha256": _hash(sidecar_bytes),
    }


def _accepted(record: dict[str, Any]) -> tuple[bool | None, str]:
    outcome = record.get("accepted_outcome")
    if isinstance(outcome, dict):
        accepted = outcome.get("accepted")
        state = outcome.get("state", "unavailable" if accepted is None else "accepted" if accepted else "rejected")
    elif record.get("kind") in ("capacity-raw-observation", "test-capacity-raw-observation") and isinstance(record.get("correctness"), bool):
        accepted = record["correctness"]
        state = str(record.get("outcome", "unavailable"))
    elif isinstance(record.get("accepted"), bool):
        accepted = record["accepted"]
        state = "accepted" if accepted else "rejected"
    else:
        accepted, state = None, "unavailable"
    if accepted is not None and not isinstance(accepted, bool):
        accepted = None
        state = "unavailable"
    return accepted, str(state)


def _record_failed(record: dict[str, Any]) -> bool:
    exit_code = record.get("exit_code")
    capacity = record.get("kind") in (
        "capacity-raw-observation", "test-capacity-raw-observation"
    )
    return bool(
        record.get("error") is not None
        or record.get("failure") is not None
        or record.get("preparation_failure") is not None
        or (isinstance(exit_code, int) and not isinstance(exit_code, bool) and exit_code != 0)
        or (capacity and record.get("outcome") != "success")
    )


def _counts(records: list[dict[str, Any]]) -> dict[str, int]:
    accepted = rejected = unavailable = failed = 0
    for record in records:
        outcome, _ = _accepted(record)
        accepted += outcome is True
        rejected += outcome is False
        unavailable += outcome is None
        failed += _record_failed(record)
    return {"accepted": accepted, "failed": failed, "records": len(records), "rejected": rejected, "unavailable": unavailable}


def _cell(value: Any, *, missing: bool = False) -> str:
    if missing and value is __missing__:
        return CSV_MISSING_TOKEN
    if value is None:
        return CSV_NULL_TOKEN
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (dict, list)):
        return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return str(value)


class _MissingSentinel:
    __slots__ = ()

    def __repr__(self) -> str:
        return "<missing>"


__missing__ = _MissingSentinel()


def _or_missing(record: dict[str, Any], *keys: str) -> Any:
    for key in keys:
        if key in record:
            return record[key]
    return __missing__


def _sort_value(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)




def _csv_record(record: dict[str, Any]) -> str:
    return json.dumps(
        record,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )


def _csv_bytes(records: list[dict[str, Any]]) -> bytes:
    output = io.StringIO(newline="")
    writer = csv.DictWriter(output, fieldnames=list(CSV_COLUMNS), lineterminator="\n")
    writer.writeheader()
    order = {"off": 0, "on": 1}
    for record in sorted(records, key=lambda row: (
        str(row.get("task_id", row.get("dataset_name", ""))),
        _sort_value(row.get("repetition")),
        order.get(str(row.get("arm", "")), 2),
        _sort_value(row),
    )):
        accepted, accepted_state = _accepted(record)
        tokens = record.get("tokens") if isinstance(record.get("tokens"), dict) else {}
        error = record.get("error") if isinstance(record.get("error"), dict) else {}
        context = record.get("context_expansion")
        if context is None and record.get("kind") in ("capacity-raw-observation", "test-capacity-raw-observation"):
            context = {
                "records": record.get("compiler_selected_records"),
                "source_bytes": record.get("compiler_selected_source_bytes"),
                "estimated_tokens": record.get("compiler_selected_estimated_tokens"),
            }
        row = {
            "task_id": _or_missing(record, "task_id", "dataset_name"),
            "repetition": _or_missing(record, "repetition"),
            "arm": _or_missing(record, "arm"),
            "accepted": accepted, "accepted_state": accepted_state,
            "exit_code": _or_missing(record, "exit_code"),
            "elapsed_ms": _or_missing(record, "elapsed_ms", "wall_duration_ms"),
            "runner_wall_time_ms": _or_missing(
                record, "runner_wall_time_ms", "runner_wall_ms"
            ),
            "adapter_elapsed_ms": _or_missing(record, "adapter_elapsed_ms"),
            "tokens_input": _or_missing(tokens, "input"),
            "tokens_output": _or_missing(tokens, "output"),
            "tokens_total": _or_missing(tokens, "total"),
            "tool_calls": _or_missing(record, "tool_calls"),
            "files_read": _or_missing(record, "files_read"),
            "source_bytes_read": _or_missing(record, "source_bytes_read", "source_bytes"),
            "atlas_route": _or_missing(record, "atlas_route"),
            "atlas_runtime_ms": _or_missing(record, "atlas_runtime_ms", "atlas_runtime_duration_ms"),
            "context_expansion": context,
            "error_kind": _or_missing(error, "kind"),
            "record_json": _csv_record(record),
        }
        writer.writerow({key: _cell(value, missing=True) for key, value in row.items()})
    return output.getvalue().encode("utf-8")


def _safe_environment_value(value: Any) -> bool:
    if isinstance(value, bool) or isinstance(value, (int, float)):
        return True
    if not isinstance(value, str):
        return False
    encoded = value.encode("utf-8")
    if len(encoded) > 512:
        return False
    if any(ord(character) < 32 for character in value):
        return False
    if HOME_OR_ABSOLUTE.search(value):
        return False
    if SECRET_VALUE_PATTERN.search(value):
        return False
    return True


def _environment(path: Path | None) -> dict[str, Any]:
    supplied: dict[str, Any] = {}
    if path is not None:
        data = _read_bounded_file(path, MAX_DOCUMENT_BYTES, "environment input")
        try:
            value = _json_load(data.decode("utf-8"), "environment")
        except UnicodeDecodeError as error:
            raise ExportError("environment input is unreadable UTF-8") from error
        if not isinstance(value, dict):
            raise ExportError("environment input must be an object")
        for key in sorted(ENVIRONMENT_KEYS):
            item = value.get(key)
            if _safe_environment_value(item):
                supplied[key] = item
    return {
        "allowlisted": supplied,
        "host": {
            "architecture": platform.machine() or "unknown",
            "os": platform.system() or "unknown",
            "python_version": platform.python_version(),
        },
        "schema_version": SCHEMA_VERSION,
    }


def _directory_guard(path: Path) -> tuple[int, int]:
    metadata = _safe_metadata(path, "destination", "directory")
    return metadata.st_dev, metadata.st_ino


def _write_exclusive(path: Path, data: bytes) -> tuple[int, int, int, int]:
    try:
        with path.open("xb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            return _identity(os.fstat(output.fileno()))
    except FileExistsError as error:
        raise ExportError(f"bundle output already exists: {path.name}") from error


def _missing_provenance(
    identity: dict[str, Any] | None, record_count: int,
) -> list[str]:
    missing: list[str] = []
    if isinstance(identity, dict):
        kind = identity.get("kind")
        if kind == "missing_harness_identity":
            missing.extend(["harness_identity", "harness_expected_count", "harness_pairing"])
        elif kind == "missing_capacity_identity":
            missing.append("capacity_identity")
        elif kind and kind != "harness" and kind != "capacity":
            missing.append("identity_pairing")
    if record_count == 0:
        missing.append("record_count")
    return sorted(set(missing))


def export(workspace: Path, source: Path, destination: Path, environment_path: Path | None) -> dict[str, Any]:
    workspace = workspace.absolute()
    source = _contained(source, workspace, "input")
    destination = _contained(destination, workspace, "destination")
    _safe_ancestors(source, workspace, False)
    if destination.exists() or destination.is_symlink():
        raise ExportError("destination already exists")
    _safe_ancestors(destination, workspace, False)
    if environment_path is not None:
        environment_path = _contained(environment_path, workspace, "environment input")
        _safe_ancestors(environment_path, workspace, False)
        _safe_metadata(environment_path, "environment input", "file")
    raw_input, records, input_format, state = _read_input(source)
    sidecar_state, identity = _identity_sidecar(
        source, workspace, raw_input, records
    )
    if sidecar_state is not None:
        state = sidecar_state
    environment = _environment(environment_path)
    counts = _counts(records)
    schemas = sorted({str(row["schema_version"]) for row in records if isinstance(row.get("schema_version"), str)})
    payloads = {
        "environment.json": _canonical_bytes(environment),
        "raw.json": _canonical_bytes({"records": records, "schema_version": SCHEMA_VERSION}),
        "summary.json": _canonical_bytes({"counts": counts, "schema_version": SCHEMA_VERSION, "state": state}),
        "results.csv": _csv_bytes(records),
    }
    missing_provenance = _missing_provenance(identity, len(records))
    manifest: dict[str, Any] = {
        "bundle_schema_version": SCHEMA_VERSION,
        "counts": counts,
        "files": {name: {"bytes": len(data), "sha256": _hash(data)} for name, data in sorted(payloads.items())},
        "input": {"bytes": len(raw_input), "format": input_format, "name": source.name, "sha256": _hash(raw_input)},
        "missing_provenance": missing_provenance,
        "source_schema_versions": schemas,
        "state": state,
    }
    if (
        identity is not None
        and not str(identity.get("kind", "")).startswith("missing_")
    ):
        manifest["identity"] = identity
    manifest_bytes = _canonical_bytes(manifest)

    parent_guard = _directory_guard(destination.parent)
    destination.mkdir()
    if _directory_guard(destination.parent) != parent_guard:
        raise ExportError("destination parent changed during creation")
    destination_guard = _directory_guard(destination)
    created: dict[Path, tuple[int, int, int, int]] = {}
    try:
        for name, data in payloads.items():
            if _directory_guard(destination) != destination_guard:
                raise ExportError("destination changed during bundle creation")
            child = destination / name
            created[child] = _write_exclusive(child, data)
        if _directory_guard(destination) != destination_guard:
            raise ExportError("destination changed during bundle creation")
        manifest_path = destination / "manifest.json"
        created[manifest_path] = _write_exclusive(manifest_path, manifest_bytes)
        if _directory_guard(destination) != destination_guard:
            raise ExportError("destination changed during bundle creation")
    except BaseException:
        try:
            if _directory_guard(destination) == destination_guard:
                for child, expected in reversed(tuple(created.items())):
                    metadata = _safe_metadata(child, "partial bundle output", "file")
                    if _identity(metadata) == expected:
                        child.unlink()
                destination.rmdir()
        except (OSError, ExportError):
            pass
        raise
    return manifest


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--environment", type=Path)
    options = parser.parse_args(arguments)
    try:
        manifest = export(options.workspace, options.input, options.destination, options.environment)
    except (ExportError, OSError) as error:
        print(json.dumps({"error": str(error), "kind": "benchmark_evidence_export"}, sort_keys=True), file=sys.stderr)
        return 2
    print(json.dumps(manifest, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
