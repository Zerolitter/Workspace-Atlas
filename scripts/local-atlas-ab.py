#!/usr/bin/env python3
"""Run a bounded, operator-supplied local model command in paired Atlas OFF/ON arms."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Any

SCHEMA_VERSION = "1.0.0"
MAX_TASKS = 20
MAX_REPETITIONS = 10
MAX_TIMEOUT_SECONDS = 3600.0
MAX_DOCUMENT_BYTES = 1024 * 1024
MAX_RUNNER_OUTPUT_BYTES = 1024 * 1024
TASK_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
SECRET_OPTION = re.compile(r"(?i)(?:secret|token|password|api[-_]?key|credential)")
METRIC_FIELDS = (
    "elapsed_ms", "tool_calls", "files_read", "source_bytes_read", "atlas_runtime_ms"
)


class HarnessError(RuntimeError):
    """The campaign cannot be run within its declared safety contract."""


def _duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise HarnessError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def _load_object(path: Path, role: str) -> tuple[dict[str, Any], bytes]:
    try:
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400 or not stat.S_ISREG(metadata.st_mode):
            raise HarnessError(f"{role} must be a regular non-link file")
        if metadata.st_size > MAX_DOCUMENT_BYTES:
            raise HarnessError(f"{role} exceeds byte bound")
        data = path.read_bytes()
        value = json.loads(data.decode("utf-8"), object_pairs_hook=_duplicates)
    except HarnessError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise HarnessError(f"{role} is malformed or unreadable") from error
    if not isinstance(value, dict):
        raise HarnessError(f"{role} must be a JSON object")
    return value, data


def _workspace_path(path: Path, workspace: Path, role: str) -> Path:
    absolute = path.absolute()
    try:
        absolute.relative_to(workspace)
    except ValueError as error:
        raise HarnessError(f"{role} escapes workspace") from error
    current = absolute.parent
    while True:
        if current.exists() or current.is_symlink():
            metadata = current.lstat()
            if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400 or not stat.S_ISDIR(metadata.st_mode):
                raise HarnessError(f"{role} has ambiguous path ancestry")
        if current == workspace:
            break
        if current.parent == current:
            raise HarnessError(f"{role} escapes workspace")
        current = current.parent
    return absolute


def _canonical(value: Any, *, pretty: bool = False) -> bytes:
    if pretty:
        text = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False)
    else:
        text = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return (text + "\n").encode("utf-8")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _tasks(document: dict[str, Any], selected: list[str], maximum: int) -> list[dict[str, str]]:
    if set(document) != {"schema_version", "tasks"} or document["schema_version"] != SCHEMA_VERSION or not isinstance(document["tasks"], list):
        raise HarnessError("task manifest contract is malformed")
    available: dict[str, dict[str, str]] = {}
    for value in document["tasks"]:
        if not isinstance(value, dict) or set(value) != {"id", "prompt"}:
            raise HarnessError("task entry contract is malformed")
        task_id, prompt = value["id"], value["prompt"]
        if not isinstance(task_id, str) or not TASK_ID.fullmatch(task_id) or not isinstance(prompt, str) or not prompt or len(prompt.encode("utf-8")) > 64 * 1024 or task_id in available:
            raise HarnessError("task identity or prompt is malformed")
        available[task_id] = {"id": task_id, "prompt": prompt}
    if not selected or len(selected) != len(set(selected)):
        raise HarnessError("tasks must be explicitly listed once")
    if len(selected) > maximum:
        raise HarnessError("selected task count exceeds bound")
    try:
        return [available[task_id] for task_id in selected]
    except KeyError as error:
        raise HarnessError("selected task is absent from manifest") from error


def _adapter(document: dict[str, Any]) -> tuple[str, str, list[str]]:
    if set(document) != {"schema_version", "adapter", "model", "command"} or document["schema_version"] != SCHEMA_VERSION:
        raise HarnessError("adapter contract is malformed")
    adapter, model, command = document["adapter"], document["model"], document["command"]
    if not isinstance(adapter, str) or not adapter or len(adapter) > 128 or not isinstance(model, str) or not model or len(model) > 256:
        raise HarnessError("adapter or model identity is malformed")
    if not isinstance(command, list) or not 1 <= len(command) <= 32 or not all(isinstance(arg, str) and arg and len(arg.encode("utf-8")) <= 8192 for arg in command):
        raise HarnessError("adapter command is malformed or outside bounds")
    return adapter, model, command


def _command_display(command: list[str]) -> list[str]:
    display: list[str] = []
    redact_next = False
    for index, argument in enumerate(command):
        if redact_next:
            display.append("<redacted>")
            redact_next = False
            continue
        if argument.startswith("-") and "=" in argument and SECRET_OPTION.search(argument.split("=", 1)[0]):
            display.append(argument.split("=", 1)[0] + "=<redacted>")
            continue
        if argument.startswith("-") and SECRET_OPTION.search(argument):
            display.append(argument)
            redact_next = True
            continue
        path = Path(argument)
        if index == 0 or path.is_absolute():
            display.append(path.name)
        else:
            display.append(argument)
    return display


def _nonnegative(value: Any) -> int | float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise HarnessError("runner metric is malformed")
    return value


def _capture(output: dict[str, Any], exit_code: int) -> dict[str, Any]:
    outcome = output.get("accepted_outcome")
    if outcome is None:
        accepted_outcome = {"accepted": None, "state": "unavailable"}
    elif isinstance(outcome, dict) and set(outcome) == {"accepted", "state"} and (outcome["accepted"] is None or isinstance(outcome["accepted"], bool)) and isinstance(outcome["state"], str):
        accepted_outcome = {"accepted": outcome["accepted"], "state": outcome["state"]}
    else:
        raise HarnessError("accepted outcome is malformed")
    captured: dict[str, Any] = {"accepted_outcome": accepted_outcome}
    for field in METRIC_FIELDS:
        captured[field] = _nonnegative(output.get(field))
    tokens = output.get("tokens")
    if tokens is None:
        captured["tokens"] = None
    elif isinstance(tokens, dict) and set(tokens).issubset({"input", "output", "total"}):
        captured["tokens"] = {key: _nonnegative(tokens.get(key)) for key in ("input", "output", "total")}
    else:
        raise HarnessError("runner token metrics are malformed")
    route = output.get("atlas_route")
    if route is not None and not isinstance(route, str):
        raise HarnessError("Atlas route is malformed")
    captured["atlas_route"] = route
    expansion = output.get("context_expansion")
    if expansion is not None and not isinstance(expansion, dict):
        raise HarnessError("context expansion is malformed")
    captured["context_expansion"] = expansion
    captured["error"] = None if exit_code == 0 else {"kind": "runner_exit"}
    return captured


def _run(command: list[str], request: dict[str, Any], cwd: Path, timeout: float) -> tuple[int | None, dict[str, Any]]:
    environment = {
        key: value for key, value in os.environ.items()
        if key.upper() in {"PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "LANG", "LC_ALL"}
    }
    environment.update({"ATLAS_ENABLED": "1" if request["arm"] == "on" else "0", "ATLAS_AB_ARM": request["arm"], "PYTHONIOENCODING": "utf-8"})
    try:
        result = subprocess.run(
            command, cwd=cwd, input=_canonical(request), stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, timeout=timeout, check=False, env=environment,
        )
    except subprocess.TimeoutExpired:
        return None, {
            "accepted_outcome": {"accepted": None, "state": "unavailable"},
            **{field: None for field in METRIC_FIELDS}, "tokens": None,
            "atlas_route": None, "context_expansion": None, "error": {"kind": "timeout"},
        }
    if len(result.stdout) > MAX_RUNNER_OUTPUT_BYTES:
        return result.returncode, {
            "accepted_outcome": {"accepted": None, "state": "unavailable"},
            **{field: None for field in METRIC_FIELDS}, "tokens": None,
            "atlas_route": None, "context_expansion": None, "error": {"kind": "runner_output_too_large"},
        }
    try:
        value = json.loads(result.stdout.decode("utf-8"), object_pairs_hook=_duplicates)
        if not isinstance(value, dict):
            raise HarnessError("runner output is not an object")
        return result.returncode, _capture(value, result.returncode)
    except (UnicodeDecodeError, json.JSONDecodeError, HarnessError):
        return result.returncode, {
            "accepted_outcome": {"accepted": None, "state": "unavailable"},
            **{field: None for field in METRIC_FIELDS}, "tokens": None,
            "atlas_route": None, "context_expansion": None,
            "error": {"kind": "malformed_runner_output"},
        }


def campaign(workspace: Path, tasks_path: Path, adapter_path: Path, destination: Path, selected: list[str], maximum_tasks: int, repetitions: int, timeout: float) -> dict[str, Any]:
    workspace = workspace.absolute()
    metadata = workspace.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise HarnessError("workspace must be a non-link directory")
    tasks_path = _workspace_path(tasks_path, workspace, "task manifest")
    adapter_path = _workspace_path(adapter_path, workspace, "adapter")
    destination = _workspace_path(destination, workspace, "destination")
    if destination.exists() or destination.is_symlink():
        raise HarnessError("destination already exists")
    if not 1 <= maximum_tasks <= MAX_TASKS or not 1 <= repetitions <= MAX_REPETITIONS or not 0 < timeout <= MAX_TIMEOUT_SECONDS:
        raise HarnessError("campaign bounds are invalid")
    task_document, task_bytes = _load_object(tasks_path, "task manifest")
    adapter_document, _ = _load_object(adapter_path, "adapter")
    chosen = _tasks(task_document, selected, maximum_tasks)
    adapter, model, command = _adapter(adapter_document)
    command_identity = _sha256(_canonical({"adapter": adapter, "command": command, "model": model}))
    task_manifest_identity = _sha256(task_bytes)
    campaign_identity = _sha256(_canonical({
        "command_identity_sha256": command_identity, "repetitions": repetitions,
        "task_manifest_sha256": task_manifest_identity, "tasks": selected,
    }))

    destination.mkdir()
    records: list[dict[str, Any]] = []
    try:
        for task in chosen:
            task_identity = _sha256(_canonical(task))
            for repetition in range(1, repetitions + 1):
                for arm in ("off", "on"):
                    relative = Path("arms") / task["id"] / str(repetition) / arm
                    arm_workspace = destination / relative
                    arm_workspace.mkdir(parents=True)
                    request = {
                        "arm": arm, "prompt": task["prompt"], "repetition": repetition,
                        "schema_version": SCHEMA_VERSION, "task_id": task["id"],
                    }
                    exit_code, captured = _run(command, request, arm_workspace, timeout)
                    record = {
                        "arm": arm, "arm_workspace": relative.as_posix(),
                        "campaign_identity_sha256": campaign_identity,
                        "command_identity_sha256": command_identity, "exit_code": exit_code,
                        "kind": "local-atlas-ab-observation", "model": model,
                        "repetition": repetition, "schema_version": SCHEMA_VERSION,
                        "task_id": task["id"], "task_identity_sha256": task_identity,
                        **captured,
                    }
                    records.append(record)
                    (arm_workspace / "result.json").write_bytes(_canonical(record, pretty=True))
        raw = b"".join(_canonical(record) for record in records)
        (destination / "raw.ndjson").write_bytes(raw)
        manifest = {
            "adapter": adapter, "campaign_identity_sha256": campaign_identity,
            "command_display": _command_display(command),
            "command_identity_sha256": command_identity, "model": model,
            "raw_count": len(records), "raw_sha256": _sha256(raw),
            "repetitions": repetitions, "schema_version": SCHEMA_VERSION,
            "state": "complete", "task_manifest": {
                "name": tasks_path.name, "sha256": task_manifest_identity,
            }, "tasks": selected,
        }
        (destination / "harness-manifest.json").write_bytes(_canonical(manifest, pretty=True))
        return manifest
    except BaseException:
        # The exclusive destination deliberately remains partial evidence rather than being hidden.
        raise


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--tasks", type=Path, required=True)
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--task", action="append", default=[])
    parser.add_argument("--max-tasks", type=int, default=8)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=300.0)
    options = parser.parse_args(arguments)
    try:
        manifest = campaign(options.workspace, options.tasks, options.adapter, options.destination, options.task, options.max_tasks, options.repetitions, options.timeout)
    except (HarnessError, OSError) as error:
        print(json.dumps({"error": str(error), "kind": "local_atlas_ab"}, sort_keys=True), file=sys.stderr)
        return 2
    print(json.dumps(manifest, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
