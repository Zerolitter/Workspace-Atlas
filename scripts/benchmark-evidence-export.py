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
    "elapsed_ms", "tokens_input", "tokens_output", "tokens_total", "tool_calls",
    "files_read", "source_bytes_read", "atlas_route", "atlas_runtime_ms",
    "context_expansion", "error_kind",
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


def _json_load(text: str, role: str) -> Any:
    try:
        return json.loads(text, object_pairs_hook=_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ExportError(f"{role} is malformed JSON") from error


def _is_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & 0x400)


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
            _safe_metadata(current, "path component", "directory" if current != path or not include_path else "directory")
        if current == workspace:
            break
        if current.parent == current:
            raise ExportError("path ancestry escapes workspace")
        current = current.parent


def _read_input(path: Path) -> tuple[bytes, list[dict[str, Any]], str, str]:
    metadata = _safe_metadata(path, "input", "file")
    if metadata.st_size > MAX_INPUT_BYTES:
        raise ExportError("input exceeds byte bound")
    try:
        raw_bytes = path.read_bytes()
        text = raw_bytes.decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
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
        input_format = "json"
    return raw_bytes, records, input_format, state


def _canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def _hash(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _accepted(record: dict[str, Any]) -> tuple[bool | None, str]:
    outcome = record.get("accepted_outcome")
    if isinstance(outcome, dict):
        accepted = outcome.get("accepted")
        state = outcome.get("state", "unavailable" if accepted is None else "accepted" if accepted else "rejected")
    elif isinstance(record.get("accepted"), bool):
        accepted = record["accepted"]
        state = "accepted" if accepted else "rejected"
    else:
        accepted, state = None, "unavailable"
    if accepted is not None and not isinstance(accepted, bool):
        accepted = None
        state = "unavailable"
    return accepted, str(state)


def _counts(records: list[dict[str, Any]]) -> dict[str, int]:
    accepted = rejected = unavailable = failed = 0
    for record in records:
        outcome, _ = _accepted(record)
        accepted += outcome is True
        rejected += outcome is False
        unavailable += outcome is None
        exit_code = record.get("exit_code")
        failed += bool(record.get("error") is not None or record.get("failure") is not None or (isinstance(exit_code, int) and exit_code != 0))
    return {"accepted": accepted, "failed": failed, "records": len(records), "rejected": rejected, "unavailable": unavailable}


def _cell(value: Any) -> str:
    if value is None:
        return ""
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (dict, list)):
        return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return str(value)


def _csv_bytes(records: list[dict[str, Any]]) -> bytes:
    output = io.StringIO(newline="")
    writer = csv.DictWriter(output, fieldnames=CSV_COLUMNS, lineterminator="\n")
    writer.writeheader()
    order = {"off": 0, "on": 1}
    for record in sorted(records, key=lambda row: (str(row.get("task_id", "")), int(row.get("repetition", 0) or 0), order.get(str(row.get("arm", "")), 2), json.dumps(row, sort_keys=True))):
        accepted, accepted_state = _accepted(record)
        tokens = record.get("tokens") if isinstance(record.get("tokens"), dict) else {}
        error = record.get("error") if isinstance(record.get("error"), dict) else {}
        row = {
            "task_id": record.get("task_id"), "repetition": record.get("repetition"),
            "arm": record.get("arm"), "accepted": accepted, "accepted_state": accepted_state,
            "exit_code": record.get("exit_code"), "elapsed_ms": record.get("elapsed_ms"),
            "tokens_input": tokens.get("input"), "tokens_output": tokens.get("output"),
            "tokens_total": tokens.get("total"), "tool_calls": record.get("tool_calls"),
            "files_read": record.get("files_read"), "source_bytes_read": record.get("source_bytes_read"),
            "atlas_route": record.get("atlas_route"), "atlas_runtime_ms": record.get("atlas_runtime_ms"),
            "context_expansion": record.get("context_expansion"), "error_kind": error.get("kind"),
        }
        writer.writerow({key: _cell(value) for key, value in row.items()})
    return output.getvalue().encode("utf-8")


def _environment(path: Path | None) -> dict[str, Any]:
    supplied: dict[str, Any] = {}
    if path is not None:
        metadata = _safe_metadata(path, "environment input", "file")
        if metadata.st_size > MAX_DOCUMENT_BYTES:
            raise ExportError("environment input exceeds byte bound")
        try:
            text = path.read_bytes().decode("utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise ExportError("environment input is unreadable UTF-8") from error
        value = _json_load(text, "environment")
        if not isinstance(value, dict):
            raise ExportError("environment input must be an object")
        for key in sorted(ENVIRONMENT_KEYS):
            item = value.get(key)
            if isinstance(item, (str, int, float, bool)) and not (isinstance(item, str) and ("\\" in item or ":/" in item or item.startswith("/"))):
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


def export(workspace: Path, source: Path, destination: Path, environment_path: Path | None) -> dict[str, Any]:
    workspace = workspace.absolute()
    _safe_metadata(workspace, "workspace", "directory")
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
    environment = _environment(environment_path)
    counts = _counts(records)
    schemas = sorted({str(row["schema_version"]) for row in records if isinstance(row.get("schema_version"), str)})
    payloads = {
        "environment.json": _canonical_bytes(environment),
        "raw.json": _canonical_bytes({"records": records, "schema_version": SCHEMA_VERSION}),
        "summary.json": _canonical_bytes({"counts": counts, "schema_version": SCHEMA_VERSION, "state": state}),
        "results.csv": _csv_bytes(records),
    }
    manifest = {
        "bundle_schema_version": SCHEMA_VERSION,
        "counts": counts,
        "files": {name: {"bytes": len(data), "sha256": _hash(data)} for name, data in sorted(payloads.items())},
        "input": {"bytes": len(raw_input), "format": input_format, "name": source.name, "sha256": _hash(raw_input)},
        "source_schema_versions": schemas,
        "state": state,
    }
    destination.mkdir()
    try:
        for name, data in payloads.items():
            (destination / name).write_bytes(data)
        (destination / "manifest.json").write_bytes(_canonical_bytes(manifest))
    except BaseException:
        for child in destination.iterdir():
            child.unlink()
        destination.rmdir()
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
