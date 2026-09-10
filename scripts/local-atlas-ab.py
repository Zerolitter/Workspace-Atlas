#!/usr/bin/env python3
"""Run a bounded, operator-supplied local model command in paired Atlas OFF/ON arms."""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
if os.name == "nt":
    from ctypes import wintypes
from typing import Any

SCHEMA_VERSION = "1.0.0"
MAX_TASKS = 20
MAX_REPETITIONS = 10
MAX_TIMEOUT_SECONDS = 3600.0
MAX_DOCUMENT_BYTES = 1024 * 1024
MAX_METRIC = (1 << 63) - 1
MAX_RUNNER_OUTPUT_BYTES = 1024 * 1024
TASK_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
SECRET_OPTION = re.compile(r"(?i)(?:secret|token|password|api[-_]?key|credential)")
METRIC_FIELDS = (
    "elapsed_ms", "tool_calls", "files_read", "source_bytes_read", "atlas_runtime_ms"
)

if os.name == "nt":
    class _JobBasicLimitInformation(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong),
            ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class _IoCounters(ctypes.Structure):
        _fields_ = [
            (name, ctypes.c_ulonglong)
            for name in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
            )
        ]

    class _JobExtendedLimitInformation(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", _JobBasicLimitInformation),
            ("IoInfo", _IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    class _ThreadEntry32(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
            ("th32ThreadID", wintypes.DWORD),
            ("th32OwnerProcessID", wintypes.DWORD),
            ("tpBasePri", wintypes.LONG), ("tpDeltaPri", wintypes.LONG),
            ("dwFlags", wintypes.DWORD),
        ]

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _kernel32.CreateJobObjectW.argtypes = (ctypes.c_void_p, wintypes.LPCWSTR)
    _kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    _kernel32.SetInformationJobObject.argtypes = (
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
    )
    _kernel32.SetInformationJobObject.restype = wintypes.BOOL
    _kernel32.AssignProcessToJobObject.argtypes = (wintypes.HANDLE, wintypes.HANDLE)
    _kernel32.AssignProcessToJobObject.restype = wintypes.BOOL
    _kernel32.TerminateJobObject.argtypes = (wintypes.HANDLE, wintypes.UINT)
    _kernel32.TerminateJobObject.restype = wintypes.BOOL
    _kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    _kernel32.CloseHandle.restype = wintypes.BOOL
    _kernel32.CreateToolhelp32Snapshot.argtypes = (wintypes.DWORD, wintypes.DWORD)
    _kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    _kernel32.Thread32First.argtypes = (
        wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32),
    )
    _kernel32.Thread32First.restype = wintypes.BOOL
    _kernel32.Thread32Next.argtypes = (
        wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32),
    )
    _kernel32.Thread32Next.restype = wintypes.BOOL
    _kernel32.OpenThread.argtypes = (
        wintypes.DWORD, wintypes.BOOL, wintypes.DWORD,
    )
    _kernel32.OpenThread.restype = wintypes.HANDLE
    _kernel32.ResumeThread.argtypes = (wintypes.HANDLE,)
    _kernel32.ResumeThread.restype = wintypes.DWORD


class HarnessError(RuntimeError):
    """The campaign cannot be run within its declared safety contract."""


def _duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise HarnessError(f"duplicate JSON field: {key}")
        result[key] = value
    return result

def _invalid_constant(value: str) -> Any:
    raise HarnessError(f"non-finite JSON number is forbidden: {value}")


def _load_object(path: Path, role: str) -> tuple[dict[str, Any], bytes]:
    try:
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400 or not stat.S_ISREG(metadata.st_mode):
            raise HarnessError(f"{role} must be a regular non-link file")
        if metadata.st_size > MAX_DOCUMENT_BYTES:
            raise HarnessError(f"{role} exceeds byte bound")
        data = path.read_bytes()
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=_duplicates,
            parse_constant=_invalid_constant,
        )
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


def _identity_display(value: str) -> str:
    if (
        SECRET_OPTION.search(value)
        or value.startswith(("/", "\\"))
        or re.match(r"^[A-Za-z]:[\\/]", value)
    ):
        return "<redacted>"
    return value


def _command_display(command: list[str]) -> list[str]:
    display: list[str] = []
    redact_next = False
    for index, argument in enumerate(command):
        if redact_next:
            display.append("<redacted>")
            redact_next = False
            continue
        if argument.startswith("-") and "=" in argument:
            display.append(argument.split("=", 1)[0] + "=<redacted>")
            continue
        if argument.startswith("-"):
            display.append(argument)
            redact_next = SECRET_OPTION.search(argument) is not None
            continue
        path = Path(argument)
        if index == 0 or path.is_absolute():
            display.append(path.name)
        else:
            display.append("<redacted>")
    return display


def _nonnegative(value: Any) -> int | float | None:
    if value is None:
        return None
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or value < 0
        or value > MAX_METRIC
        or (isinstance(value, float) and not math.isfinite(value))
    ):
        raise HarnessError("runner metric is malformed")
    return value


def _capture(output: dict[str, Any], exit_code: int) -> dict[str, Any]:
    outcome = output.get("accepted_outcome")
    if outcome is None:
        accepted_outcome = {"accepted": None, "state": "unavailable"}
    elif (
        isinstance(outcome, dict)
        and set(outcome) == {"accepted", "state"}
        and (outcome["accepted"] is None or isinstance(outcome["accepted"], bool))
        and isinstance(outcome["state"], str)
        and TASK_ID.fullmatch(outcome["state"])
    ):
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
    if route is not None and (
        not isinstance(route, str)
        or len(route) > 128
        or TASK_ID.fullmatch(route) is None
    ):
        raise HarnessError("Atlas route is malformed")
    captured["atlas_route"] = route
    expansion = output.get("context_expansion")
    if expansion is None:
        captured["context_expansion"] = None
    elif (
        isinstance(expansion, dict)
        and len(expansion) <= 32
        and all(
            isinstance(key, str)
            and TASK_ID.fullmatch(key)
            and _nonnegative(value) is value
            for key, value in expansion.items()
        )
    ):
        captured["context_expansion"] = expansion
    else:
        raise HarnessError("context expansion is malformed")
    captured["error"] = None if exit_code == 0 else {"kind": "runner_exit"}
    return captured


def _unavailable(kind: str) -> dict[str, Any]:
    return {
        "accepted_outcome": {"accepted": None, "state": "unavailable"},
        **{field: None for field in METRIC_FIELDS},
        "tokens": None,
        "atlas_route": None,
        "context_expansion": None,
        "error": {"kind": kind},
    }


def _resume_windows_process(process: subprocess.Popen[bytes]) -> None:
    snapshot = _kernel32.CreateToolhelp32Snapshot(0x00000004, 0)
    if snapshot == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    thread = None
    try:
        entry = _ThreadEntry32()
        entry.dwSize = ctypes.sizeof(entry)
        present = _kernel32.Thread32First(snapshot, ctypes.byref(entry))
        while present:
            if entry.th32OwnerProcessID == process.pid:
                thread = _kernel32.OpenThread(0x0002, False, entry.th32ThreadID)
                break
            present = _kernel32.Thread32Next(snapshot, ctypes.byref(entry))
        if not thread or _kernel32.ResumeThread(thread) == 0xFFFFFFFF:
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        if thread:
            _kernel32.CloseHandle(thread)
        _kernel32.CloseHandle(snapshot)


class _ProcessOwner:
    def __init__(self, process: subprocess.Popen[bytes], job: object | None):
        self.process = process
        self.job = job

    @classmethod
    def spawn(
        cls, command: list[str], **options: Any
    ) -> tuple[subprocess.Popen[bytes], "_ProcessOwner"]:
        if os.name != "nt":
            process = subprocess.Popen(command, start_new_session=True, **options)
            return process, cls(process, None)
        job = _kernel32.CreateJobObjectW(None, None)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = _JobExtendedLimitInformation()
        limits.BasicLimitInformation.LimitFlags = 0x00002000
        if not _kernel32.SetInformationJobObject(
            job, 9, ctypes.byref(limits), ctypes.sizeof(limits)
        ):
            _kernel32.CloseHandle(job)
            raise ctypes.WinError(ctypes.get_last_error())
        process = None
        try:
            process = subprocess.Popen(
                command, creationflags=0x00000004, **options
            )
            if not _kernel32.AssignProcessToJobObject(
                job, wintypes.HANDLE(int(process._handle))
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            _resume_windows_process(process)
            return process, cls(process, job)
        except BaseException:
            if process is not None:
                _kernel32.TerminateJobObject(job, 1)
                process.wait(timeout=5)
            _kernel32.CloseHandle(job)
            raise

    def terminate(self) -> None:
        try:
            if os.name == "nt":
                if not _kernel32.TerminateJobObject(self.job, 1):
                    raise ctypes.WinError(ctypes.get_last_error())
            else:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            self.process.wait(timeout=5)
        finally:
            self.close()

    def close(self) -> None:
        if os.name == "nt":
            if self.job is not None and not _kernel32.CloseHandle(self.job):
                raise ctypes.WinError(ctypes.get_last_error())
            self.job = None
        else:
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def _run(command: list[str], request: dict[str, Any], cwd: Path, timeout: float) -> tuple[int | None, dict[str, Any]]:
    environment = {
        key: value for key, value in os.environ.items()
        if key.upper() in {"PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "LANG", "LC_ALL"}
    }
    environment.update({"ATLAS_ENABLED": "1" if request["arm"] == "on" else "0", "ATLAS_AB_ARM": request["arm"], "PYTHONIOENCODING": "utf-8"})
    with tempfile.TemporaryFile(dir=cwd) as runner_input, tempfile.TemporaryFile(
        dir=cwd
    ) as runner_output:
        runner_input.write(_canonical(request))
        runner_input.seek(0)
        try:
            process, owner = _ProcessOwner.spawn(
                command,
                cwd=cwd,
                stdin=runner_input,
                stdout=runner_output,
                stderr=subprocess.DEVNULL,
                env=environment,
            )
        except OSError:
            return None, _unavailable("runner_launch")
        deadline = time.monotonic() + timeout
        failure: str | None = None
        while process.poll() is None:
            if runner_output.tell() > MAX_RUNNER_OUTPUT_BYTES:
                failure = "runner_output_too_large"
                break
            if time.monotonic() >= deadline:
                failure = "timeout"
                break
            time.sleep(0.01)
        if failure is not None:
            owner.terminate()
            return None if failure == "timeout" else process.returncode, _unavailable(failure)
        try:
            owner.close()
        except OSError:
            return process.returncode, _unavailable("runner_containment_failure")
        if runner_output.tell() > MAX_RUNNER_OUTPUT_BYTES:
            return process.returncode, _unavailable("runner_output_too_large")
        runner_output.seek(0)
        stdout = runner_output.read(MAX_RUNNER_OUTPUT_BYTES + 1)
    try:
        value = json.loads(
            stdout.decode("utf-8"),
            object_pairs_hook=_duplicates,
            parse_constant=_invalid_constant,
        )
        if not isinstance(value, dict):
            raise HarnessError("runner output is not an object")
        return process.returncode, _capture(value, process.returncode)
    except (UnicodeDecodeError, json.JSONDecodeError, HarnessError):
        return process.returncode, _unavailable("malformed_runner_output")

def _write_manifest(destination: Path, manifest: dict[str, Any]) -> None:
    temporary = destination / ".harness-manifest.tmp"
    final = destination / "harness-manifest.json"
    try:
        with temporary.open("xb") as output:
            output.write(_canonical(manifest, pretty=True))
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, final)
    except BaseException:
        try:
            temporary.unlink()
        except OSError:
            pass
        raise


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
    model_identity = _sha256(_canonical(model))
    task_manifest_identity = _sha256(task_bytes)
    expected_raw_count = len(chosen) * repetitions * 2
    campaign_identity = _sha256(_canonical({
        "command_identity_sha256": command_identity, "repetitions": repetitions,
        "task_manifest_sha256": task_manifest_identity, "tasks": selected,
    }))

    destination.mkdir()
    raw_path = destination / "raw.ndjson"
    raw_digest = hashlib.sha256()
    records: list[dict[str, Any]] = []

    def manifest(state: str) -> dict[str, Any]:
        return {
            "adapter": _identity_display(adapter),
            "campaign_identity_sha256": campaign_identity,
            "command_display": _command_display(command),
            "command_identity_sha256": command_identity,
            "expected_raw_count": expected_raw_count,
            "model": _identity_display(model),
            "model_identity_sha256": model_identity,
            "raw_count": len(records),
            "raw_sha256": raw_digest.hexdigest(),
            "repetitions": repetitions,
            "schema_version": SCHEMA_VERSION,
            "state": state,
            "task_manifest": {
                "name": tasks_path.name, "sha256": task_manifest_identity,
            },
            "tasks": selected,
        }

    with raw_path.open("xb") as raw_output:
        _write_manifest(destination, manifest("partial"))
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
                        "command_identity_sha256": command_identity,
                        "exit_code": exit_code,
                        "kind": "local-atlas-ab-observation",
                        "model": _identity_display(model),
                        "model_identity_sha256": model_identity,
                        "repetition": repetition, "schema_version": SCHEMA_VERSION,
                        "task_id": task["id"], "task_identity_sha256": task_identity,
                        **captured,
                    }
                    record_bytes = _canonical(record)
                    raw_output.write(record_bytes)
                    raw_output.flush()
                    os.fsync(raw_output.fileno())
                    raw_digest.update(record_bytes)
                    records.append(record)
                    _write_manifest(destination, manifest("partial"))
                    (arm_workspace / "result.json").write_bytes(_canonical(record, pretty=True))
    complete = manifest("complete")
    _write_manifest(destination, complete)
    return complete


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
