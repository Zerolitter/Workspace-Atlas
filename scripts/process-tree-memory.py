#!/usr/bin/env python3
"""Bounded platform-native process-tree memory evidence for Atlas capacity runs."""
from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
from dataclasses import dataclass
import hashlib
import json
import sqlite3
import os
import stat
from pathlib import Path
import platform
import signal
import subprocess
import sys
import tempfile
if os.name == "nt":
    import msvcrt
import time
from typing import BinaryIO, Callable, Iterable

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_VERSION = "1.0.0"
SEGMENTED_SCHEMA_VERSION = "2.0.0"
RAW_KIND = "process-tree-memory-sample"
SUMMARY_KIND = "process-tree-memory-summary"
SEGMENTED_SUMMARY_KIND = "process-tree-memory-segmented-summary"
CADENCE_NS = 50_000_000
IDLE_DURATION_NS = 5_000_000_000
MAX_BYTES = (1 << 63) - 1
MAX_PROCESSES = 4096
MAX_RAW_SAMPLES = 200_000
MAX_RAW_BYTES = 512 * 1024 * 1024
MAX_LABEL_BYTES = 16 * 1024
MAX_SEGMENTS = 1024
MAX_RAW_RECORD_BYTES = 8 * 1024 * 1024
LEGACY_RAW_NAME = "capacity-memory-raw-v1.ndjson"
LEGACY_SUMMARY_NAME = "capacity-memory-summary-v1.json"
RAW_NAME = "capacity-memory-raw-v2"
SUMMARY_NAME = "capacity-memory-summary-v2.json"
VALIDATED_BUNDLE_MAGIC = b"ATLAS_MEMORY_BUNDLE_V1\n"
VALIDATED_BUNDLE_FRAME = b"ATLAS_MEMORY_ARTIFACT "
VALIDATED_BUNDLE_COMPLETE = b"ATLAS_MEMORY_VALIDATED "
LABEL_KEYS = frozenset({
    "phase", "filesystem_cache_state", "serving_state",
    "provider_cache_install_state", "build_state", "catalogue_state",
})
CAPACITY_PROGRESS_TOTAL = 135_000
CAPACITY_PROGRESS_INTERVAL = 1_000
CAPACITY_SHARD_COUNT = 4
CAPACITY_SHARD_TOTAL = 33_750
CAPACITY_GLOBAL_MATRIX_ROWS = 360
CAPACITY_SHARD_ROWS = 90
CAPACITY_ATTEMPTS_PER_MATRIX_ROW = 375
PROGRESS_KEYS = frozenset({
    "progress_state", "progress_ordinal", "progress_total",
})
PROGRESS_PHASES = frozenset({
    "harness-startup",
    "cold-initialization-reconcile",
    "no-change-reconcile",
    "fixed-incremental-reconcile",
    "ready-serving-query-compiler",
    "truth-fallback-query-compiler",
    "capability-discovery",
    "direct-route-compiler",
    "light-route-compiler",
    "progressive-deep-route-compiler",
    "catalogue-growth",
})
PROGRESS_PREFIX = "ATLAS_CAPACITY_PROGRESS"


def _valid_progress_total(total: object) -> bool:
    return (
        isinstance(total, int)
        and not isinstance(total, bool)
        and total in (CAPACITY_PROGRESS_TOTAL, CAPACITY_SHARD_TOTAL)
    )


def _split_label_state(state: object) -> tuple[dict[str, str], dict[str, object] | None]:
    if not isinstance(state, dict):
        raise EvidenceError("state labels are missing, extra, or malformed")
    keys = set(state)
    if keys not in (LABEL_KEYS, LABEL_KEYS | PROGRESS_KEYS):
        raise EvidenceError("state labels are missing, extra, or malformed")
    labels = {key: state[key] for key in LABEL_KEYS}
    validate_labels(labels)
    if keys == LABEL_KEYS:
        return labels, None
    progress = {key: state[key] for key in PROGRESS_KEYS}
    progress["phase"] = labels["phase"]
    phase = progress["phase"]
    status = progress["progress_state"]
    ordinal = progress["progress_ordinal"]
    total = progress["progress_total"]
    if (
        phase not in PROGRESS_PHASES
        or status not in ("starting", "active", "complete")
        or not isinstance(ordinal, int)
        or isinstance(ordinal, bool)
        or not _valid_progress_total(total)
        or not 0 <= ordinal <= total
        or (status == "starting" and (ordinal != 0 or phase != "harness-startup"))
        or (status == "active" and ordinal == 0)
        or (status == "complete" and ordinal != total)
    ):
        raise EvidenceError("capacity progress state is malformed or outside its fixed bound")
    return labels, progress


def _emit_progress(line: str) -> None:
    print(line, flush=True)


class ProgressReporter:
    def __init__(self, writer: Callable[[str], None] = _emit_progress):
        self.writer = writer
        self.last_observed_ordinal = -1
        self.last_emitted_ordinal = -CAPACITY_PROGRESS_INTERVAL
        self.last_phase: str | None = None
        self.last_state: str | None = None
        self.progress_total: int | None = None

    def observe(self, label_state: object) -> None:
        _, progress = _split_label_state(label_state)
        self._observe(progress)

    def _observe(self, progress: dict[str, object] | None) -> None:
        if progress is None:
            if self.last_state in ("complete", "stopped"):
                raise EvidenceError("capacity progress changed after its final state")
            return
        ordinal = int(progress["progress_ordinal"])
        total = int(progress["progress_total"])
        phase = str(progress["phase"])
        state = str(progress["progress_state"])
        if self.last_state in ("complete", "stopped"):
            if (
                self.last_state == "complete"
                and state == self.last_state
                and ordinal == self.last_observed_ordinal
                and total == self.progress_total
                and phase == self.last_phase
            ):
                return
            raise EvidenceError("capacity progress changed after its final state")
        if (
            (self.progress_total is not None and total != self.progress_total)
            or ordinal < self.last_observed_ordinal
            or (ordinal == self.last_observed_ordinal and phase != self.last_phase)
        ):
            raise EvidenceError("capacity progress ordinal, total, or final state regressed")
        self.progress_total = total
        should_emit = (
            state == "complete"
            or (state == "starting" and self.last_state != "starting")
            or phase != self.last_phase
            or ordinal - self.last_emitted_ordinal >= CAPACITY_PROGRESS_INTERVAL
        )
        self.last_observed_ordinal = ordinal
        self.last_phase = phase
        self.last_state = state
        if should_emit:
            self.writer(
                f"{PROGRESS_PREFIX} state={state} ordinal={ordinal} "
                f"total={total} phase={phase}"
            )
            self.last_emitted_ordinal = ordinal

    def finish(self, exit_code: int) -> None:
        if self.last_state is None or self.last_state in ("complete", "stopped"):
            return
        if exit_code == 0:
            raise EvidenceError("clean capacity exit lacks complete progress state")
        self.writer(
            f"{PROGRESS_PREFIX} state=stopped ordinal={self.last_observed_ordinal} "
            f"total={self.progress_total} phase={self.last_phase}"
        )
        self.last_state = "stopped"


class EvidenceError(RuntimeError):
    """Malformed, inconsistent, unsafe, or unbounded evidence."""


class UnsupportedPlatformError(EvidenceError):
    """The host has no approved native observation method."""


@dataclass(frozen=True)
class ProcessNode:
    pid: int
    parent_pid: int
    identity: str | None
    resident_bytes: int | None
    private_bytes: int | None = None
    high_water_bytes: int | None = None
    process_group_id: int | None = None


@dataclass(frozen=True)
class TreeSnapshot:
    atlas: dict[str, object]
    providers: list[dict[str, object]]
    tree_status: str = "observed"
    tree_error: dict[str, str] | None = None

def _strict_json(data: bytes | str, description: str) -> object:
    def closed_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise EvidenceError(f"{description} contains duplicate field {key}")
            result[key] = value
        return result

    try:
        return json.loads(data, object_pairs_hook=closed_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"{description} is malformed JSON") from error


def _checked_bytes(value: int, unit: int = 1) -> int:
    if value < 0:
        raise EvidenceError("memory value must be non-negative")
    if value > MAX_BYTES // unit:
        raise EvidenceError("memory value overflow")
    return value * unit


def parse_linux_status(text: str) -> tuple[int, int | None]:
    values: dict[str, int] = {}
    for line in text.splitlines():
        if not (line.startswith("VmRSS:") or line.startswith("VmHWM:")):
            continue
        parts = line.split()
        if len(parts) != 3 or parts[2] != "kB":
            raise EvidenceError(f"malformed Linux {parts[0] if parts else 'memory'} value")
        try:
            value = int(parts[1], 10)
        except ValueError as error:
            raise EvidenceError("malformed Linux memory integer") from error
        values[parts[0].removesuffix(":")] = _checked_bytes(value, 1024)
    if "VmRSS" not in values:
        raise EvidenceError("Linux status is missing VmRSS")
    return values["VmRSS"], values.get("VmHWM")


def parse_linux_stat(text: str) -> tuple[int, int, str]:
    close = text.rfind(")")
    if close < 2 or close + 2 >= len(text):
        raise EvidenceError("malformed Linux stat process name")
    fields = text[close + 2:].split()
    if len(fields) < 20:
        raise EvidenceError("malformed Linux stat field count")
    try:
        parent_pid = int(fields[1], 10)
        process_group_id = int(fields[2], 10)
        starttime = int(fields[19], 10)
    except ValueError as error:
        raise EvidenceError("malformed Linux stat identity") from error
    if parent_pid < 0 or process_group_id < 0 or starttime < 0:
        raise EvidenceError("Linux stat identity must be non-negative")
    return parent_pid, process_group_id, f"linux-starttime:{starttime}"


def parse_macos_ps(text: str) -> list[ProcessNode]:
    rows: list[ProcessNode] = []
    for line in text.splitlines():
        parts = line.split()
        if not parts:
            continue
        if len(parts) != 9:
            raise EvidenceError("malformed macOS ps row")
        try:
            pid, parent_pid, process_group_id, rss_kib = (
                int(parts[index], 10) for index in range(4)
            )
        except ValueError as error:
            raise EvidenceError("malformed macOS ps integer") from error
        if pid <= 0 or parent_pid < 0 or process_group_id < 0:
            raise EvidenceError("macOS ps PIDs are outside the valid range")
        resident = _checked_bytes(rss_kib, 1024)
        identity = "macos-lstart:" + " ".join(parts[4:])
        rows.append(
            ProcessNode(
                pid, parent_pid, identity, resident,
                process_group_id=process_group_id,
            )
        )
    if len(rows) > MAX_PROCESSES:
        raise EvidenceError("macOS ps process bound exceeded")
    return rows


def descendant_pids(nodes: Iterable[ProcessNode], root_pid: int, maximum: int) -> list[int]:
    if root_pid <= 0 or maximum <= 0:
        raise EvidenceError("invalid process-tree root or process bound")
    children: dict[int, list[int]] = {}
    for node in nodes:
        if node.pid == 0:
            continue
        if node.pid < 0 or node.parent_pid < 0:
            raise EvidenceError("invalid process-tree PID")
        children.setdefault(node.parent_pid, []).append(node.pid)
    result: list[int] = []
    seen = {root_pid}
    pending = list(sorted(children.get(root_pid, ()), reverse=True))
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        result.append(pid)
        if len(result) > maximum:
            raise EvidenceError("process bound exceeded while enumerating descendants")
        pending.extend(sorted(children.get(pid, ()), reverse=True))
    return sorted(result)


def platform_metadata(system: str | None = None) -> dict[str, object]:
    system = system or sys.platform
    common = {
        "architecture": platform.machine() or "unavailable",
        "host_release": platform.release() or "unavailable",
        "processor": platform.processor() or "unavailable",
        "logical_cpu_count": os.cpu_count(),
        "python_version": platform.python_version(),
    }
    if system == "win32":
        return {
            "os": "windows", **common,
            "method": "Job Object active-process list + GetProcessMemoryInfo WorkingSet64 and PrivateMemorySize64",
            "advisory": False,
            "limitations": [
                "working-set-includes-shared-pages",
                "private-bytes-omit-shared-and-mapped-pressure",
                "sub-50ms-descendants-may-be-missed",
                "observation-calls-within-one-sample-are-sequential",
            ],
        }
    if system.startswith("linux"):
        return {
            "os": "linux", **common,
            "method": "/proc/<pid>/status VmRSS and VmHWM with /proc/<pid>/stat process-group/starttime",
            "advisory": False,
            "limitations": [
                "rss-includes-shared-pages",
                "VmHWM-is-retained-last-observed-at-exit",
                "sub-50ms-descendants-may-be-missed",
                "proc-reads-can-race-process-exit",
                "one-raw-timestamp-labels-a-bounded-non-atomic-sequential-observation-window",
                "process-group-containment-excludes-members-that-create-a-new-session-or-group",
            ],
        }
    if system == "darwin":
        return {
            "os": "macos", **common,
            "method": "/bin/ps -axo pid=,ppid=,pgid=,rss=,lstart= resident bytes",
            "advisory": True,
            "limitations": [
                "runner-default", "second-resolution",
                "runner-default-values-are-advisory-until-pinned",
                "rss-includes-shared-pages",
                "ps-snapshot-has-no-private-bytes-or-high-water-mark",
                "sub-50ms-descendants-may-be-missed",
                "one-raw-timestamp-labels-a-bounded-non-atomic-sequential-observation-window",
                "process-group-containment-excludes-members-that-create-a-new-session-or-group",
            ],
        }
    raise UnsupportedPlatformError(f"unsupported platform {system!r}")


def _error_kind(error: BaseException) -> str:
    if isinstance(error, PermissionError) or getattr(error, "winerror", None) == 5:
        return "permission_denied"
    if isinstance(error, (FileNotFoundError, ProcessLookupError)) or getattr(error, "winerror", None) in (87, 1168):
        return "exited"
    if isinstance(error, subprocess.TimeoutExpired):
        return "timeout"
    return "unavailable"


def measurement_gap(pid: int | None, kind: str, detail: str, method: str,
                    *, identity: str | None = None) -> dict[str, object]:
    return {
        "status": "gap", "pid": pid, "identity": identity,
        "resident_bytes": None, "private_bytes": None, "high_water_bytes": None,
        "method": method, "error": {"kind": kind, "detail": detail[:512]},
    }


def not_applicable_measurement(method: str) -> dict[str, object]:
    return {
        "status": "not_applicable", "pid": None, "identity": None,
        "resident_bytes": None, "private_bytes": None, "high_water_bytes": None,
        "method": method, "error": None,
    }


def observed_measurement(node: ProcessNode, method: str) -> dict[str, object]:
    return {
        "status": "observed", "pid": node.pid, "identity": node.identity,
        "resident_bytes": node.resident_bytes, "private_bytes": node.private_bytes,
        "high_water_bytes": node.high_water_bytes, "method": method, "error": None,
    }

def tracked_process_measurements(
    nodes: Iterable[ProcessNode],
    member_pids: Iterable[int],
    retained: dict[int, str],
    method: str,
    observe_process: Callable[[int], dict[str, object]],
    *,
    atlas_pid: int,
) -> list[dict[str, object]]:
    by_pid = {node.pid: node for node in nodes}
    members = set(member_pids)
    members.discard(atlas_pid)
    candidates = members | set(retained)
    if len(candidates) > MAX_PROCESSES:
        raise EvidenceError("contained process bound exceeded")
    measurements: list[dict[str, object]] = []
    for pid in sorted(candidates):
        if pid <= 0:
            raise EvidenceError("contained process PID is invalid")
        node = by_pid.get(pid)
        if (
            pid in members
            and node is not None
            and node.identity is not None
            and node.resident_bytes is not None
        ):
            measurement = observed_measurement(node, method)
        else:
            measurement = observe_process(pid)
        expected_identity = retained.get(pid)
        if expected_identity is not None:
            if (
                measurement["status"] == "observed"
                and measurement["identity"] != expected_identity
            ):
                measurement = measurement_gap(
                    pid, "pid_reuse", "retained descendant PID identity changed",
                    method, identity=str(measurement["identity"]),
                )
            elif measurement["status"] == "gap" and measurement["identity"] is None:
                measurement = {**measurement, "identity": expected_identity}
        measurements.append(measurement)
    return measurements


def retained_descendant_identities(
    previous: dict[int, str],
    current: list[dict[str, object]],
) -> dict[int, str]:
    retained = dict(previous)
    for measurement in current:
        pid = measurement.get("pid")
        identity = measurement.get("identity")
        if not isinstance(pid, int):
            continue
        if measurement.get("status") == "observed" and isinstance(identity, str):
            retained[pid] = identity
            continue
        error = measurement.get("error")
        kind = error.get("kind") if isinstance(error, dict) else None
        if kind in ("exited", "pid_reuse"):
            retained.pop(pid, None)
        elif isinstance(identity, str):
            retained[pid] = identity
    if len(retained) > MAX_PROCESSES:
        raise EvidenceError("retained descendant process bound exceeded")
    return retained


def retained_tree_failure_measurements(
    retained: dict[int, str],
    error: BaseException,
    method: str,
) -> list[dict[str, object]]:
    kind = _error_kind(error)
    if kind == "exited":
        kind = "unavailable"
    return [
        measurement_gap(
            pid, kind, f"contained-tree observation failed: {error}",
            method, identity=identity,
        )
        for pid, identity in sorted(retained.items())
    ]

def validate_measurement(value: object) -> None:
    if not isinstance(value, dict) or set(value) != {
        "status", "pid", "identity", "resident_bytes", "private_bytes",
        "high_water_bytes", "method", "error",
    }:
        raise EvidenceError("measurement schema is malformed")
    status = value["status"]
    if status not in ("observed", "gap", "not_applicable"):
        raise EvidenceError("measurement status is invalid")
    if not isinstance(value["method"], str) or not value["method"]:
        raise EvidenceError("measurement method is missing")
    if status == "observed":
        if not isinstance(value["pid"], int) or value["pid"] <= 0:
            raise EvidenceError("observed measurement PID is invalid")
        if not isinstance(value["identity"], str) or not value["identity"]:
            raise EvidenceError("observed measurement identity is missing")
        if value["error"] is not None:
            raise EvidenceError("observed measurement cannot contain an error")
        if value["resident_bytes"] is None:
            raise EvidenceError("observed measurement requires resident bytes")
    elif status == "gap":
        error = value["error"]
        if not isinstance(error, dict) or set(error) != {"kind", "detail"}:
            raise EvidenceError("measurement gap requires a typed error")
        if not all(isinstance(error[key], str) and error[key] for key in error):
            raise EvidenceError("measurement gap error is malformed")
        if any(value[key] is not None for key in ("resident_bytes", "private_bytes", "high_water_bytes")):
            raise EvidenceError("measurement gap cannot zero-fill values")
    else:
        if value["pid"] is not None or value["identity"] is not None or value["error"] is not None:
            raise EvidenceError("not-applicable measurement contains process data")
        if any(value[key] is not None for key in ("resident_bytes", "private_bytes", "high_water_bytes")):
            raise EvidenceError("not-applicable measurement contains memory data")
    for field in ("resident_bytes", "private_bytes", "high_water_bytes"):
        number = value[field]
        if number is not None and (not isinstance(number, int) or isinstance(number, bool) or number < 0 or number > MAX_BYTES):
            raise EvidenceError(f"measurement {field} is negative, overflowed, or not an integer")


class FixtureObserver:
    def __init__(self, measurement: dict[str, object]):
        self.measurement = measurement

    def observe_process(self, pid: int) -> dict[str, object]:
        result = dict(self.measurement)
        result["pid"] = pid
        return result

    def observe_tree(self, pid: int) -> TreeSnapshot:
        return TreeSnapshot(self.observe_process(pid), [])


class FakeClock:
    def __init__(self) -> None:
        self.now = 0

    def monotonic_ns(self) -> int:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += round(seconds * 1_000_000_000)


if os.name == "nt":
    class _ProcessEntry32W(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
            ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
            ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
            ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", wintypes.LONG),
            ("dwFlags", wintypes.DWORD), ("szExeFile", wintypes.WCHAR * 260),
        ]

    class _ProcessMemoryCountersEx(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t),
            ("PrivateUsage", ctypes.c_size_t),
        ]

    class _FileTime(ctypes.Structure):
        _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

    class _JobBasicLimitInformation(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong), ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class _IoCounters(ctypes.Structure):
        _fields_ = [(name, ctypes.c_ulonglong) for name in (
            "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
            "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
        )]

    class _JobExtendedLimitInformation(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", _JobBasicLimitInformation), ("IoInfo", _IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    class _JobBasicAccountingInformation(ctypes.Structure):
        _fields_ = [
            ("TotalUserTime", ctypes.c_longlong),
            ("TotalKernelTime", ctypes.c_longlong),
            ("ThisPeriodTotalUserTime", ctypes.c_longlong),
            ("ThisPeriodTotalKernelTime", ctypes.c_longlong),
            ("TotalPageFaultCount", wintypes.DWORD),
            ("TotalProcesses", wintypes.DWORD),
            ("ActiveProcesses", wintypes.DWORD),
            ("TotalTerminatedProcesses", wintypes.DWORD),
        ]

    class _ThreadEntry32(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
            ("th32ThreadID", wintypes.DWORD), ("th32OwnerProcessID", wintypes.DWORD),
            ("tpBasePri", wintypes.LONG), ("tpDeltaPri", wintypes.LONG),
            ("dwFlags", wintypes.DWORD),
        ]

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _psapi = ctypes.WinDLL("psapi", use_last_error=True)
    _kernel32.CreateToolhelp32Snapshot.argtypes = (wintypes.DWORD, wintypes.DWORD)
    _kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    _kernel32.Process32FirstW.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ProcessEntry32W))
    _kernel32.Process32FirstW.restype = wintypes.BOOL
    _kernel32.Process32NextW.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ProcessEntry32W))
    _kernel32.Process32NextW.restype = wintypes.BOOL
    _kernel32.Thread32First.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32))
    _kernel32.Thread32First.restype = wintypes.BOOL
    _kernel32.Thread32Next.argtypes = (wintypes.HANDLE, ctypes.POINTER(_ThreadEntry32))
    _kernel32.Thread32Next.restype = wintypes.BOOL
    _kernel32.OpenThread.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    _kernel32.OpenThread.restype = wintypes.HANDLE
    _kernel32.ResumeThread.argtypes = (wintypes.HANDLE,)
    _kernel32.ResumeThread.restype = wintypes.DWORD
    _kernel32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    _kernel32.OpenProcess.restype = wintypes.HANDLE
    _kernel32.GetProcessTimes.argtypes = (
        wintypes.HANDLE, ctypes.POINTER(_FileTime), ctypes.POINTER(_FileTime),
        ctypes.POINTER(_FileTime), ctypes.POINTER(_FileTime),
    )
    _kernel32.GetProcessTimes.restype = wintypes.BOOL
    _kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    _kernel32.CloseHandle.restype = wintypes.BOOL
    _kernel32.CreateJobObjectW.argtypes = (ctypes.c_void_p, wintypes.LPCWSTR)
    _kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    _kernel32.SetInformationJobObject.argtypes = (wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD)
    _kernel32.SetInformationJobObject.restype = wintypes.BOOL
    _kernel32.AssignProcessToJobObject.argtypes = (wintypes.HANDLE, wintypes.HANDLE)
    _kernel32.AssignProcessToJobObject.restype = wintypes.BOOL
    _kernel32.TerminateJobObject.argtypes = (wintypes.HANDLE, wintypes.UINT)
    _kernel32.TerminateJobObject.restype = wintypes.BOOL
    _kernel32.QueryInformationJobObject.argtypes = (
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p,
    )
    _kernel32.QueryInformationJobObject.restype = wintypes.BOOL
    _psapi.GetProcessMemoryInfo.argtypes = (wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD)
    _psapi.GetProcessMemoryInfo.restype = wintypes.BOOL


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


class ProcessOwner:
    def __init__(self, process: subprocess.Popen[bytes], job: object | None = None):
        self.process = process
        self.job = job

    @classmethod
    def spawn(cls, command: list[str], cwd: Path) -> tuple[subprocess.Popen[bytes], "ProcessOwner"]:
        options = dict(cwd=cwd, stdin=subprocess.DEVNULL, shell=False)
        if os.name != "nt":
            process = subprocess.Popen(command, start_new_session=True, **options)
            return process, cls(process)
        job = _kernel32.CreateJobObjectW(None, None)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = _JobExtendedLimitInformation()
        limits.BasicLimitInformation.LimitFlags = 0x00002000
        if not _kernel32.SetInformationJobObject(job, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            _kernel32.CloseHandle(job)
            raise ctypes.WinError(ctypes.get_last_error())
        process = None
        try:
            process = subprocess.Popen(command, creationflags=0x00000004, **options)
            if not _kernel32.AssignProcessToJobObject(job, wintypes.HANDLE(int(process._handle))):
                raise ctypes.WinError(ctypes.get_last_error())
            _resume_windows_process(process)
            return process, cls(process, job)
        except BaseException:
            if process is not None:
                _kernel32.TerminateJobObject(job, 1)
                process.wait(timeout=5)
            _kernel32.CloseHandle(job)
            raise

    def active_process_count(self) -> int:
        if os.name == "nt":
            accounting = _JobBasicAccountingInformation()
            if not _kernel32.QueryInformationJobObject(
                self.job, 1, ctypes.byref(accounting), ctypes.sizeof(accounting), None
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            return int(accounting.ActiveProcesses)
        try:
            os.killpg(self.process.pid, 0)
        except ProcessLookupError:
            return 0
        return 1

    def contained_group_id(self) -> int | None:
        return None if os.name == "nt" else self.process.pid

    def contained_pids(self) -> list[int] | None:
        if os.name != "nt":
            return None
        header_size = ctypes.sizeof(wintypes.DWORD) * 2
        buffer_size = header_size + MAX_PROCESSES * ctypes.sizeof(ctypes.c_size_t)
        buffer = ctypes.create_string_buffer(buffer_size)
        if not _kernel32.QueryInformationJobObject(
            self.job, 3, buffer, buffer_size, None
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        header = (wintypes.DWORD * 2).from_buffer(buffer)
        assigned, returned = int(header[0]), int(header[1])
        if assigned > MAX_PROCESSES or returned > MAX_PROCESSES:
            raise EvidenceError("Windows Job Object process bound exceeded")
        identifiers = (ctypes.c_size_t * returned).from_buffer(buffer, header_size)
        return sorted(int(identifier) for identifier in identifiers)

    def terminate(self) -> None:
        if self.active_process_count() == 0:
            return
        if os.name == "nt":
            if not _kernel32.TerminateJobObject(self.job, 1):
                raise ctypes.WinError(ctypes.get_last_error())
        else:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            if os.name == "nt":
                _kernel32.TerminateJobObject(self.job, 1)
            else:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            self.process.wait(timeout=5)
        deadline = time.monotonic() + 5
        while self.active_process_count() and time.monotonic() < deadline:
            time.sleep(0.01)
        if self.active_process_count():
            raise EvidenceError("sampled process tree did not terminate")

    def close(self) -> None:
        if self.process.poll() is None or self.active_process_count() != 0:
            raise EvidenceError("cannot release a live sampled process tree")
        if os.name == "nt" and self.job and not _kernel32.CloseHandle(self.job):
            raise ctypes.WinError(ctypes.get_last_error())
        self.job = None


class WindowsObserver:
    method = "GetProcessMemoryInfo(WorkingSet64,PrivateMemorySize64)"

    def _nodes(self) -> list[ProcessNode]:
        snapshot = _kernel32.CreateToolhelp32Snapshot(0x00000002, 0)
        if snapshot == wintypes.HANDLE(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        nodes: list[ProcessNode] = []
        try:
            entry = _ProcessEntry32W()
            entry.dwSize = ctypes.sizeof(entry)
            present = _kernel32.Process32FirstW(snapshot, ctypes.byref(entry))
            while present:
                nodes.append(ProcessNode(int(entry.th32ProcessID), int(entry.th32ParentProcessID), None, None))
                if len(nodes) > MAX_PROCESSES:
                    raise EvidenceError("Windows process snapshot process bound exceeded")
                present = _kernel32.Process32NextW(snapshot, ctypes.byref(entry))
        finally:
            _kernel32.CloseHandle(snapshot)
        return nodes

    def observe_process(self, pid: int) -> dict[str, object]:
        handle = _kernel32.OpenProcess(0x0400 | 0x0010, False, pid)
        if not handle:
            error = ctypes.WinError(ctypes.get_last_error())
            return measurement_gap(pid, _error_kind(error), str(error), self.method)
        try:
            counters = _ProcessMemoryCountersEx()
            counters.cb = ctypes.sizeof(counters)
            created, exited, kernel, user = (_FileTime() for _ in range(4))
            if not _kernel32.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(kernel), ctypes.byref(user)):
                raise ctypes.WinError(ctypes.get_last_error())
            if not _psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), ctypes.sizeof(counters)):
                raise ctypes.WinError(ctypes.get_last_error())
            node = ProcessNode(
                pid, 0, f"windows-creation-time:{(int(created.high) << 32) | int(created.low)}",
                _checked_bytes(int(counters.WorkingSetSize)),
                _checked_bytes(int(counters.PrivateUsage)), None,
            )
            return observed_measurement(node, self.method)
        except BaseException as error:
            return measurement_gap(pid, _error_kind(error), str(error), self.method)
        finally:
            _kernel32.CloseHandle(handle)

    def observe_tree(
        self,
        atlas_pid: int,
        *,
        retained: dict[int, str] | None = None,
        contained_group_id: int | None = None,
        contained_pids: list[int] | None = None,
    ) -> TreeSnapshot:
        atlas = self.observe_process(atlas_pid)
        try:
            nodes = self._nodes() if contained_pids is None else []
            members = (
                contained_pids
                if contained_pids is not None
                else descendant_pids(nodes, atlas_pid, MAX_PROCESSES)
            )
            providers = tracked_process_measurements(
                nodes, members, retained or {}, self.method, self.observe_process,
                atlas_pid=atlas_pid,
            )
        except BaseException as error:
            return TreeSnapshot(
                atlas,
                retained_tree_failure_measurements(retained or {}, error, self.method),
                "gap",
                {"kind": _error_kind(error), "detail": str(error)[:512]},
            )
        return TreeSnapshot(atlas, providers)


class LinuxObserver:
    method = "/proc/status(VmRSS,VmHWM)+/proc/stat(process-group,starttime)"

    def _node(self, pid: int) -> ProcessNode:
        stat_text = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
        parent_pid, process_group_id, identity = parse_linux_stat(stat_text)
        status_text = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
        resident, high_water = parse_linux_status(status_text)
        return ProcessNode(
            pid, parent_pid, identity, resident, None, high_water,
            process_group_id,
        )

    def _nodes(self) -> tuple[list[ProcessNode], list[str]]:
        nodes: list[ProcessNode] = []
        skipped: list[str] = []
        with os.scandir("/proc") as entries:
            for entry in entries:
                if not entry.name.isdecimal():
                    continue
                try:
                    nodes.append(self._node(int(entry.name, 10)))
                except (FileNotFoundError, ProcessLookupError, PermissionError, EvidenceError) as error:
                    if len(skipped) < 16:
                        skipped.append(_error_kind(error))
                if len(nodes) > MAX_PROCESSES:
                    raise EvidenceError("Linux process snapshot process bound exceeded")
        return nodes, skipped

    def observe_process(self, pid: int) -> dict[str, object]:
        try:
            return observed_measurement(self._node(pid), self.method)
        except BaseException as error:
            return measurement_gap(pid, _error_kind(error), str(error), self.method)

    def observe_tree(
        self,
        atlas_pid: int,
        *,
        retained: dict[int, str] | None = None,
        contained_group_id: int | None = None,
        contained_pids: list[int] | None = None,
    ) -> TreeSnapshot:
        try:
            nodes, skipped = self._nodes()
            by_pid = {node.pid: node for node in nodes}
            atlas_node = by_pid.get(atlas_pid)
            atlas = (
                observed_measurement(atlas_node, self.method)
                if atlas_node is not None else self.observe_process(atlas_pid)
            )
            if contained_pids is not None:
                members = contained_pids
            elif contained_group_id is not None:
                members = [
                    node.pid for node in nodes
                    if node.process_group_id == contained_group_id
                ]
            else:
                members = descendant_pids(nodes, atlas_pid, MAX_PROCESSES)
            providers = tracked_process_measurements(
                nodes, members, retained or {}, self.method, self.observe_process,
                atlas_pid=atlas_pid,
            )
            if skipped:
                detail = f"{len(skipped)} process rows raced exit, denied access, or were malformed"
                return TreeSnapshot(atlas, providers, "gap", {
                    "kind": "partial_process_snapshot", "detail": detail,
                })
            return TreeSnapshot(atlas, providers)
        except BaseException as error:
            atlas = self.observe_process(atlas_pid)
            return TreeSnapshot(
                atlas,
                retained_tree_failure_measurements(retained or {}, error, self.method),
                "gap",
                {"kind": _error_kind(error), "detail": str(error)[:512]},
            )


class MacOSObserver:
    method = "/bin/ps(pid,ppid,pgid,rss,lstart)"

    def _nodes(self) -> list[ProcessNode]:
        output = subprocess.run(
            ["/bin/ps", "-axo", "pid=,ppid=,pgid=,rss=,lstart="],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            timeout=5, check=False, shell=False,
        )
        if output.returncode != 0:
            raise EvidenceError(f"macOS ps failed with exit {output.returncode}")
        if len(output.stdout) > 8 * 1024 * 1024:
            raise EvidenceError("macOS ps output bound exceeded")
        return parse_macos_ps(output.stdout.decode("utf-8", errors="strict"))

    def observe_process(self, pid: int) -> dict[str, object]:
        try:
            node = next(node for node in self._nodes() if node.pid == pid)
            return observed_measurement(node, self.method)
        except StopIteration:
            return measurement_gap(pid, "exited", "process absent from ps snapshot", self.method)
        except BaseException as error:
            return measurement_gap(pid, _error_kind(error), str(error), self.method)

    def observe_tree(
        self,
        atlas_pid: int,
        *,
        retained: dict[int, str] | None = None,
        contained_group_id: int | None = None,
        contained_pids: list[int] | None = None,
    ) -> TreeSnapshot:
        try:
            nodes = self._nodes()
            by_pid = {node.pid: node for node in nodes}
            atlas_node = by_pid.get(atlas_pid)
            atlas = (
                observed_measurement(atlas_node, self.method)
                if atlas_node is not None
                else measurement_gap(
                    atlas_pid, "exited", "Atlas absent from ps snapshot", self.method
                )
            )
            if contained_pids is not None:
                members = contained_pids
            elif contained_group_id is not None:
                members = [
                    node.pid for node in nodes
                    if node.process_group_id == contained_group_id
                ]
            else:
                members = descendant_pids(nodes, atlas_pid, MAX_PROCESSES)

            def observe_snapshot_process(pid: int) -> dict[str, object]:
                node = by_pid.get(pid)
                return (
                    observed_measurement(node, self.method)
                    if node is not None
                    else measurement_gap(
                        pid, "exited", "process absent from ps snapshot", self.method
                    )
                )

            providers = tracked_process_measurements(
                nodes, members, retained or {}, self.method, observe_snapshot_process,
                atlas_pid=atlas_pid,
            )
            return TreeSnapshot(atlas, providers)
        except BaseException as error:
            atlas = measurement_gap(
                atlas_pid, _error_kind(error), str(error), self.method
            )
            return TreeSnapshot(
                atlas,
                retained_tree_failure_measurements(retained or {}, error, self.method),
                "gap",
                {"kind": _error_kind(error), "detail": str(error)[:512]},
            )


def native_observer() -> WindowsObserver | LinuxObserver | MacOSObserver:
    if sys.platform == "win32":
        return WindowsObserver()
    if sys.platform.startswith("linux"):
        return LinuxObserver()
    if sys.platform == "darwin":
        return MacOSObserver()
    raise UnsupportedPlatformError(f"unsupported platform {sys.platform!r}")


def concurrent_totals(
    atlas: dict[str, object],
    providers: list[dict[str, object]],
    *,
    tree_complete: bool = True,
) -> dict[str, object]:
    validate_measurement(atlas)
    for provider in providers:
        validate_measurement(provider)
    providers_observed = tree_complete and all(
        item["status"] == "observed" for item in providers
    )
    complete = atlas["status"] == "observed" and providers_observed
    result: dict[str, object] = {"status": "observed" if complete else "gap"}
    for field in ("resident_bytes", "private_bytes"):
        atlas_value = atlas[field] if atlas["status"] == "observed" else None
        provider_values = [item[field] for item in providers] if providers_observed else None
        atlas_total = int(atlas_value) if atlas_value is not None else None
        if provider_values is None or any(value is None for value in provider_values):
            provider_total = None
        else:
            provider_total = sum(int(value) for value in provider_values)
            if provider_total > MAX_BYTES:
                raise EvidenceError("provider memory aggregate overflow")
        total = (
            atlas_total + provider_total
            if complete and atlas_total is not None and provider_total is not None
            else None
        )
        if total is not None and total > MAX_BYTES:
            raise EvidenceError("concurrent memory aggregate overflow")
        result[f"atlas_{field}"] = atlas_total
        result[f"provider_{field}"] = provider_total
        result[field] = total
    return result


def descendant_events(previous: dict[int, str], current: list[dict[str, object]]) -> list[dict[str, object]]:
    events: list[dict[str, object]] = []
    current_identities: dict[int, str] = {}
    for item in current:
        pid = item.get("pid")
        identity = item.get("identity")
        if isinstance(pid, int) and isinstance(identity, str):
            current_identities[pid] = identity
        error = item.get("error")
        kind = error.get("kind") if isinstance(error, dict) else None
        if isinstance(pid, int) and kind == "pid_reuse" and pid in previous:
            events.append({
                "type": "pid_reuse", "pid": pid,
                "previous_identity": previous[pid], "current_identity": identity,
            })
        elif isinstance(pid, int) and kind == "exited" and pid in previous:
            events.append({
                "type": "descendant_exited", "pid": pid,
                "identity": previous[pid],
            })
        elif item.get("status") == "gap" and isinstance(pid, int):
            events.append({
                "type": "descendant_measurement_gap", "pid": pid,
                "error": item.get("error"),
            })
        elif (
            isinstance(pid, int)
            and isinstance(identity, str)
            and pid in previous
            and previous[pid] != identity
        ):
            events.append({
                "type": "pid_reuse", "pid": pid,
                "previous_identity": previous[pid], "current_identity": identity,
            })
    for pid, identity in sorted(previous.items()):
        if pid not in current_identities and not any(event.get("pid") == pid for event in events):
            events.append({"type": "descendant_exited", "pid": pid, "identity": identity})
    return events


def sampling_gap(scheduled_ns: int, actual_ns: int, cadence_ns: int) -> dict[str, object]:
    if min(scheduled_ns, actual_ns) < 0 or cadence_ns <= 0:
        raise EvidenceError("invalid monotonic sampling timestamps")
    lateness = max(0, actual_ns - scheduled_ns)
    missed = lateness // cadence_ns
    return {
        "status": "missed_deadline" if missed else "on_schedule",
        "lateness_ns": lateness, "missed_intervals": missed,
    }


def validate_labels(labels: object) -> None:
    if not isinstance(labels, dict) or set(labels) != LABEL_KEYS:
        raise EvidenceError("state labels are missing, extra, or malformed")
    for key, value in labels.items():
        if not isinstance(value, str) or not value or len(value) > 256 or "\x00" in value:
            raise EvidenceError(f"state label {key} is empty or unbounded")


def _idle_labels() -> dict[str, str]:
    return {
        "phase": "idle-control", "filesystem_cache_state": "uncontrolled",
        "serving_state": "not-applicable", "provider_cache_install_state": "not-applicable",
        "build_state": "sampler-idle", "catalogue_state": "not-applicable",
    }


def _idle_aggregate() -> dict[str, object]:
    return {
        "status": "not_applicable", "atlas_resident_bytes": None,
        "provider_resident_bytes": None, "resident_bytes": None,
        "atlas_private_bytes": None, "provider_private_bytes": None,
        "private_bytes": None,
    }

def _aggregate_gap() -> dict[str, object]:
    return {
        "status": "gap", "atlas_resident_bytes": None,
        "provider_resident_bytes": None, "resident_bytes": None,
        "atlas_private_bytes": None, "provider_private_bytes": None,
        "private_bytes": None,
    }

def wait_until_scheduled(scheduled_ns: int, clock_ns: Callable[[], int],
                         sleeper: Callable[[float], None]) -> int:
    while True:
        actual = clock_ns()
        if actual >= scheduled_ns:
            return actual
        sleeper((scheduled_ns - actual) / 1_000_000_000)


def collect_idle_control(observer: object, clock_ns: Callable[[], int], sleeper: Callable[[float], None],
                         *, sampler_pid: int, duration_ns: int, cadence_ns: int,
                         max_samples: int, sequence_start: int = 0) -> list[dict[str, object]]:
    if duration_ns != IDLE_DURATION_NS or cadence_ns != CADENCE_NS:
        raise EvidenceError("idle control must be five seconds at 50 ms cadence")
    required = duration_ns // cadence_ns + 1
    if required > max_samples:
        raise EvidenceError("sample bound cannot retain the five-second idle control")
    start = clock_ns()
    records: list[dict[str, object]] = []
    for index in range(required):
        scheduled = start + index * cadence_ns
        actual = wait_until_scheduled(scheduled, clock_ns, sleeper)
        tree_snapshot = observer.observe_tree(sampler_pid)
        observer_measurement = tree_snapshot.atlas
        validate_measurement(observer_measurement)
        records.append({
            "schema_version": SCHEMA_VERSION, "kind": RAW_KIND,
            "sequence": sequence_start + index, "control": "idle",
            "scheduled_monotonic_ns": scheduled, "monotonic_ns": actual,
            "atlas_pid": None, "observed_descendant_provider_pids": [],
            "labels": _idle_labels(), "observer": observer_measurement,
            "atlas": not_applicable_measurement("idle-control"), "providers": [],
            "aggregate": _idle_aggregate(),
            "sampling_gap": sampling_gap(scheduled, actual, cadence_ns), "child_events": [],
            "tree_observation": {
                "status": tree_snapshot.tree_status,
                "error": tree_snapshot.tree_error,
            },
        })
    return records


def statistics(values: list[int]) -> dict[str, int | None]:
    if not values:
        return {"count": 0, "p50": None, "p95": None, "peak": None}
    ordered = sorted(values)
    def rank(percentile: int) -> int:
        index = max(1, (percentile * len(ordered) + 99) // 100) - 1
        return ordered[index]
    return {"count": len(ordered), "p50": rank(50), "p95": rank(95), "peak": ordered[-1]}


def summarize_campaign(aggregates: list[dict[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for category, prefix in (("atlas_only", "atlas_"), ("provider_only", "provider_"),
                             ("concurrent_aggregate", "")):
        result[category] = {}
        for field in ("resident_bytes", "private_bytes"):
            key = prefix + field
            values = [
                int(item[key]) for item in aggregates if item.get(key) is not None
            ]
            result[category][field] = statistics(values)
    return result

def _campaign_summary(records: list[dict[str, object]]) -> dict[str, object]:
    summary = summarize_campaign([record["aggregate"] for record in records])
    summary["atlas_only"]["high_water_bytes"] = statistics([
        int(record["atlas"]["high_water_bytes"]) for record in records
        if record["atlas"]["status"] == "observed"
        and record["atlas"]["high_water_bytes"] is not None
    ])
    summary["provider_only"]["high_water_bytes"] = statistics([
        int(provider["high_water_bytes"]) for record in records
        for provider in record["providers"]
        if provider["status"] == "observed" and provider["high_water_bytes"] is not None
    ])
    return summary


def _tool_metadata() -> dict[str, str]:
    return {
        "name": "process-tree-memory.py",
        "version": SCHEMA_VERSION,
        "sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }


def _baseline_summary(records: list[dict[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for field in ("resident_bytes", "private_bytes"):
        result[field] = statistics([
            int(record["observer"][field]) for record in records
            if record["observer"]["status"] == "observed" and record["observer"][field] is not None
        ])
    return result


def _event_counts(records: list[dict[str, object]]) -> dict[str, int]:
    counts = {"sampling_gaps": 0, "descendant_exits": 0, "descendant_measurement_gaps": 0,
              "pid_reuse": 0, "tree_observation_gaps": 0}
    for record in records:
        if record["sampling_gap"]["status"] == "missed_deadline":
            counts["sampling_gaps"] += 1
        if record["tree_observation"]["status"] == "gap":
            counts["tree_observation_gaps"] += 1
        for event in record["child_events"]:
            key = {"descendant_exited": "descendant_exits",
                   "descendant_measurement_gap": "descendant_measurement_gaps",
                   "pid_reuse": "pid_reuse"}[event["type"]]
            counts[key] += 1
    return counts


class StreamingSummaryAccumulator:
    def __init__(self, directory: Path):
        self.temporary = tempfile.TemporaryDirectory(
            prefix=".memory-summary-", dir=directory
        )
        self.connection = sqlite3.connect(
            str(Path(self.temporary.name) / "metrics.sqlite")
        )
        self.connection.execute("PRAGMA journal_mode=OFF")
        self.connection.execute("PRAGMA synchronous=OFF")
        self.closed = False
        self.connection.execute("PRAGMA temp_store=FILE")
        self.connection.execute("PRAGMA cache_size=-2048")
        self.connection.execute(
            "CREATE TABLE metric (name TEXT NOT NULL, value INTEGER NOT NULL)"
        )
        self.record_count = 0
        self.events = {
            "sampling_gaps": 0,
            "descendant_exits": 0,
            "descendant_measurement_gaps": 0,
            "pid_reuse": 0,
            "tree_observation_gaps": 0,
        }


    def observe(self, record: dict[str, object]) -> None:
        values: list[tuple[str, int]] = []
        if record["control"] == "idle":
            observer = record["observer"]
            if observer["status"] == "observed":
                for field in ("resident_bytes", "private_bytes"):
                    if observer[field] is not None:
                        values.append((f"baseline.{field}", int(observer[field])))
        else:
            aggregate = record["aggregate"]
            for category, prefix in (
                ("atlas_only", "atlas_"),
                ("provider_only", "provider_"),
                ("concurrent_aggregate", ""),
            ):
                for field in ("resident_bytes", "private_bytes"):
                    value = aggregate.get(prefix + field)
                    if value is not None:
                        values.append((f"campaign.{category}.{field}", int(value)))
            atlas = record["atlas"]
            if atlas["status"] == "observed" and atlas["high_water_bytes"] is not None:
                values.append((
                    "campaign.atlas_only.high_water_bytes",
                    int(atlas["high_water_bytes"]),
                ))
            for provider in record["providers"]:
                if (
                    provider["status"] == "observed"
                    and provider["high_water_bytes"] is not None
                ):
                    values.append((
                        "campaign.provider_only.high_water_bytes",
                        int(provider["high_water_bytes"]),
                    ))
        self.connection.executemany(
            "INSERT INTO metric (name, value) VALUES (?, ?)", values
        )
        if record["sampling_gap"]["status"] == "missed_deadline":
            self.events["sampling_gaps"] += 1
        if record["tree_observation"]["status"] == "gap":
            self.events["tree_observation_gaps"] += 1
        for event in record["child_events"]:
            key = {
                "descendant_exited": "descendant_exits",
                "descendant_measurement_gap": "descendant_measurement_gaps",
                "pid_reuse": "pid_reuse",
            }[event["type"]]
            self.events[key] += 1
        self.record_count += 1

    def _statistics(self, name: str) -> dict[str, int | None]:
        count = int(self.connection.execute(
            "SELECT COUNT(*) FROM metric WHERE name = ?", (name,)
        ).fetchone()[0])
        if count == 0:
            return {"count": 0, "p50": None, "p95": None, "peak": None}

        def rank(percentile: int) -> int:
            offset = max(1, (percentile * count + 99) // 100) - 1
            row = self.connection.execute(
                "SELECT value FROM metric WHERE name = ? ORDER BY value LIMIT 1 OFFSET ?",
                (name, offset),
            ).fetchone()
            assert row is not None
            return int(row[0])

        peak = self.connection.execute(
            "SELECT MAX(value) FROM metric WHERE name = ?", (name,)
        ).fetchone()[0]
        return {"count": count, "p50": rank(50), "p95": rank(95), "peak": int(peak)}

    def summary_parts(
        self,
    ) -> tuple[dict[str, object], dict[str, object], dict[str, int]]:
        self.connection.execute(
            "CREATE INDEX metric_name_value ON metric (name, value)"
        )
        self.connection.commit()
        baseline = {
            field: self._statistics(f"baseline.{field}")
            for field in ("resident_bytes", "private_bytes")
        }
        campaign: dict[str, object] = {}
        for category in ("atlas_only", "provider_only", "concurrent_aggregate"):
            campaign[category] = {
                field: self._statistics(f"campaign.{category}.{field}")
                for field in ("resident_bytes", "private_bytes")
            }
        campaign["atlas_only"]["high_water_bytes"] = self._statistics(
            "campaign.atlas_only.high_water_bytes"
        )
        campaign["provider_only"]["high_water_bytes"] = self._statistics(
            "campaign.provider_only.high_water_bytes"
        )
        return baseline, campaign, dict(self.events)

    def close(self) -> None:
        if not self.closed:
            self.connection.close()
            self.temporary.cleanup()
            self.closed = True


def capacity_execution_contract(global_plan_sha256: str, shard_index: int | None = None,
                                shard_count: int | None = None) -> dict[str, object]:
    if (
        not isinstance(global_plan_sha256, str)
        or len(global_plan_sha256) != 64
        or any(character not in "0123456789abcdef" for character in global_plan_sha256)
    ):
        raise EvidenceError("global capacity plan identity is malformed")
    if (shard_index is None) != (shard_count is None):
        raise EvidenceError("capacity shard index and count must be supplied together")
    if shard_index is None:
        mode = "full"
        selected_matrix_rows = CAPACITY_GLOBAL_MATRIX_ROWS
        selected_attempts = CAPACITY_PROGRESS_TOTAL
    else:
        if (
            isinstance(shard_index, bool)
            or not isinstance(shard_index, int)
            or isinstance(shard_count, bool)
            or not isinstance(shard_count, int)
            or shard_count != CAPACITY_SHARD_COUNT
            or not 0 <= shard_index < CAPACITY_SHARD_COUNT
        ):
            raise EvidenceError("capacity shard contract requires index 0..3 and count 4")
        mode = "shard"
        selected_matrix_rows = CAPACITY_SHARD_ROWS
        selected_attempts = CAPACITY_SHARD_TOTAL
    return {
        "mode": mode,
        "shard_index": shard_index,
        "shard_count": shard_count,
        "index_base": 0,
        "assignment": "global-matrix-row-index-modulo-4",
        "global_matrix_rows": CAPACITY_GLOBAL_MATRIX_ROWS,
        "selected_matrix_rows": selected_matrix_rows,
        "attempts_per_matrix_row": CAPACITY_ATTEMPTS_PER_MATRIX_ROW,
        "global_attempts": CAPACITY_PROGRESS_TOTAL,
        "selected_attempts": selected_attempts,
        "global_plan_sha256": global_plan_sha256,
    }


def validate_capacity_execution(value: object) -> None:
    if not isinstance(value, dict):
        raise EvidenceError("capacity execution identity is missing or malformed")
    try:
        expected = capacity_execution_contract(
            value["global_plan_sha256"], value["shard_index"], value["shard_count"]
        )
    except (KeyError, TypeError) as error:
        raise EvidenceError("capacity execution identity is missing or malformed") from error
    if value != expected:
        raise EvidenceError("capacity execution identity is missing or malformed")


def build_summary(records: list[dict[str, object]], raw: bytes, metadata: dict[str, object],
                  *, exit_code: int, cadence_ns: int, idle_duration_ns: int,
                  maximum_raw_samples: int,
                  capacity_execution: dict[str, object] | None = None) -> dict[str, object]:
    validate_raw_records(records, maximum_raw_samples=maximum_raw_samples)
    campaign_records = [record for record in records if record["control"] == "campaign"]
    idle_records = [record for record in records if record["control"] == "idle"]
    summary = {
        "schema_version": SCHEMA_VERSION, "kind": SUMMARY_KIND,
        "platform": metadata, "tool": _tool_metadata(),
        "cadence_ms": cadence_ns // 1_000_000,
        "idle_control_duration_ms": idle_duration_ns // 1_000_000,
        "maximum_raw_samples": maximum_raw_samples, "maximum_raw_bytes": MAX_RAW_BYTES,
        "raw_record_count": len(records), "raw_sha256": hashlib.sha256(raw).hexdigest(),
        "exit": {"status": "clean" if exit_code == 0 else "failed", "code": exit_code},
        "baseline_visible_not_subtracted": True, "baseline": _baseline_summary(idle_records),
        "campaign": _campaign_summary(campaign_records),
        "events": _event_counts(records),
        "limitations": metadata["limitations"],
        "claim": "instrumentation evidence only; no capacity, provider-limit, scaling, memory-budget, SLO, cross-platform-equivalence, release, or public-readiness claim",
    }
    if capacity_execution is not None:
        validate_capacity_execution(capacity_execution)
        summary["capacity_execution"] = dict(capacity_execution)
    return summary

def _build_segmented_summary(
    *,
    record_count: int,
    segments: list[dict[str, object]],
    metadata: dict[str, object],
    baseline: dict[str, object],
    campaign: dict[str, object],
    events: dict[str, int],
    segment_name_prefix: str,
    exit_code: int,
    cadence_ns: int,
    idle_duration_ns: int,
    maximum_segment_samples: int,
    maximum_segment_bytes: int,
    raw_sha256: str,
    raw_byte_length: int,
    capacity_execution: dict[str, object] | None = None,
) -> dict[str, object]:
    summary = {
        "schema_version": SEGMENTED_SCHEMA_VERSION,
        "kind": SEGMENTED_SUMMARY_KIND,
        "platform": metadata,
        "tool": _tool_metadata(),
        "cadence_ms": cadence_ns // 1_000_000,
        "idle_control_duration_ms": idle_duration_ns // 1_000_000,
        "segment_name_prefix": segment_name_prefix,
        "maximum_segment_samples": maximum_segment_samples,
        "maximum_segment_bytes": maximum_segment_bytes,
        "maximum_segments": MAX_SEGMENTS,
        "raw_record_count": record_count,
        "raw_byte_length": raw_byte_length,
        "raw_sha256": raw_sha256,
        "segments": segments,
        "exit": {"status": "clean" if exit_code == 0 else "failed", "code": exit_code},
        "baseline_visible_not_subtracted": True,
        "baseline": baseline,
        "campaign": campaign,
        "events": events,
        "limitations": metadata["limitations"],
        "claim": "instrumentation evidence only; no capacity, provider-limit, scaling, memory-budget, SLO, cross-platform-equivalence, release, or public-readiness claim",
    }
    if capacity_execution is not None:
        validate_capacity_execution(capacity_execution)
        summary["capacity_execution"] = dict(capacity_execution)
    return summary


def build_segmented_summary(
    records: list[dict[str, object]],
    segments: list[dict[str, object]],
    metadata: dict[str, object],
    *,
    segment_name_prefix: str,
    exit_code: int,
    cadence_ns: int,
    idle_duration_ns: int,
    maximum_segment_samples: int,
    maximum_segment_bytes: int,
    raw_sha256: str,
    raw_byte_length: int,
    capacity_execution: dict[str, object] | None = None,
) -> dict[str, object]:
    validate_raw_records(
        records, maximum_raw_samples=len(records), enforce_maximum_limit=False
    )
    campaign_records = [record for record in records if record["control"] == "campaign"]
    idle_records = [record for record in records if record["control"] == "idle"]
    return _build_segmented_summary(
        record_count=len(records),
        segments=segments,
        metadata=metadata,
        baseline=_baseline_summary(idle_records),
        campaign=_campaign_summary(campaign_records),
        events=_event_counts(records),
        segment_name_prefix=segment_name_prefix,
        exit_code=exit_code,
        cadence_ns=cadence_ns,
        idle_duration_ns=idle_duration_ns,
        maximum_segment_samples=maximum_segment_samples,
        maximum_segment_bytes=maximum_segment_bytes,
        raw_sha256=raw_sha256,
        raw_byte_length=raw_byte_length,
        capacity_execution=capacity_execution,
    )



def encode_raw_records(records: list[dict[str, object]]) -> bytes:
    return b"".join(
        json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8") + b"\n"
        for record in records
    )


class RawRecordStreamValidator:
    expected_keys = {
        "schema_version", "kind", "sequence", "control", "scheduled_monotonic_ns",
        "monotonic_ns", "atlas_pid", "observed_descendant_provider_pids", "labels",
        "observer", "atlas", "providers", "aggregate", "sampling_gap", "child_events",
        "tree_observation",
    }

    def __init__(self, maximum_raw_samples: int, *, enforce_maximum_limit: bool = True):
        if (
            not isinstance(maximum_raw_samples, int)
            or maximum_raw_samples <= 0
            or (enforce_maximum_limit and maximum_raw_samples > MAX_RAW_SAMPLES)
        ):
            raise EvidenceError("raw sample count is missing or exceeds its bound")
        self.maximum_raw_samples = maximum_raw_samples
        self.count = 0
        self.previous_time = -1
        self.previous_scheduled = -1
        self.previous_providers: dict[int, str] = {}
        self.previous_control: str | None = None
        self.atlas_identity: str | None = None
        self.idle_count = 0
        self.campaign_count = 0
        self.first_idle_scheduled: int | None = None
        self.last_idle_scheduled: int | None = None
        self.final_atlas: object = None

    def observe(self, record: object) -> None:
        index = self.count
        if index >= self.maximum_raw_samples:
            raise EvidenceError("raw sample count is missing or exceeds its bound")
        if not isinstance(record, dict) or set(record) != self.expected_keys:
            raise EvidenceError("raw sample schema is malformed")
        if (
            record["schema_version"] != SCHEMA_VERSION
            or record["kind"] != RAW_KIND
            or record["sequence"] != index
        ):
            raise EvidenceError("raw sample identity or sequence is inconsistent")
        control = record["control"]
        if control not in ("idle", "campaign") or (self.campaign_count and control == "idle"):
            raise EvidenceError("raw sample control ordering is invalid")
        for field in ("scheduled_monotonic_ns", "monotonic_ns"):
            if (
                not isinstance(record[field], int)
                or isinstance(record[field], bool)
                or record[field] < 0
            ):
                raise EvidenceError("raw monotonic timestamps are invalid")
        if record["monotonic_ns"] < record["scheduled_monotonic_ns"]:
            raise EvidenceError("raw actual timestamp precedes its schedule")
        if record["monotonic_ns"] < self.previous_time:
            raise EvidenceError("raw monotonic timestamps are not ordered")
        if self.previous_scheduled >= 0:
            scheduled_delta = record["scheduled_monotonic_ns"] - self.previous_scheduled
            if control == self.previous_control and scheduled_delta != CADENCE_NS:
                raise EvidenceError(f"{control} cadence is inconsistent")
            if control != self.previous_control and record["scheduled_monotonic_ns"] <= self.previous_time:
                raise EvidenceError("idle-to-campaign ordering overlaps or reverses")
        self.previous_time = record["monotonic_ns"]
        self.previous_scheduled = record["scheduled_monotonic_ns"]
        self.previous_control = control
        validate_labels(record["labels"])
        validate_measurement(record["observer"])
        validate_measurement(record["atlas"])
        if not isinstance(record["providers"], list):
            raise EvidenceError("provider measurements are malformed")
        for provider in record["providers"]:
            validate_measurement(provider)
        pids = [
            provider["pid"] for provider in record["providers"]
            if isinstance(provider.get("pid"), int)
        ]
        if record["observed_descendant_provider_pids"] != pids or pids != sorted(set(pids)):
            raise EvidenceError("descendant provider PID identity is inconsistent")
        tree = record["tree_observation"]
        if (
            not isinstance(tree, dict)
            or set(tree) != {"status", "error"}
            or tree["status"] not in ("observed", "gap", "not_applicable")
        ):
            raise EvidenceError("tree observation record is malformed")
        if tree["status"] == "gap":
            error = tree["error"]
            if (
                not isinstance(error, dict)
                or set(error) != {"kind", "detail"}
                or not all(isinstance(value, str) and value for value in error.values())
            ):
                raise EvidenceError("tree observation gap lacks a typed error")
        elif tree["error"] is not None:
            raise EvidenceError("successful tree observation contains an error")
        if control == "idle":
            self.idle_count += 1
            scheduled = int(record["scheduled_monotonic_ns"])
            if self.first_idle_scheduled is None:
                self.first_idle_scheduled = scheduled
            self.last_idle_scheduled = scheduled
            if (
                record["atlas_pid"] is not None
                or record["atlas"]["status"] != "not_applicable"
                or record["providers"]
                or tree["status"] not in ("observed", "gap")
            ):
                raise EvidenceError("idle control contains Atlas process data")
            if record["aggregate"] != _idle_aggregate() or record["child_events"]:
                raise EvidenceError("idle aggregate or child events are malformed")
        else:
            self.campaign_count += 1
            if (
                not isinstance(record["atlas_pid"], int)
                or record["atlas_pid"] <= 0
                or record["atlas_pid"] != record["atlas"]["pid"]
            ):
                raise EvidenceError("Atlas PID identity is inconsistent")
            if record["atlas"]["status"] == "observed":
                current_identity = str(record["atlas"]["identity"])
                if self.atlas_identity is None:
                    self.atlas_identity = current_identity
                elif current_identity != self.atlas_identity:
                    raise EvidenceError("Atlas PID identity changed within one run")
            expected_aggregate = concurrent_totals(
                record["atlas"], record["providers"],
                tree_complete=tree["status"] == "observed",
            )
            if record["aggregate"] != expected_aggregate:
                raise EvidenceError("cross-time or inconsistent concurrent aggregate")
            expected_events = descendant_events(self.previous_providers, record["providers"])
            if record["child_events"] != expected_events:
                raise EvidenceError("descendant churn or PID reuse events are inconsistent")
            self.previous_providers = retained_descendant_identities(
                self.previous_providers, record["providers"]
            )
        gap = sampling_gap(
            record["scheduled_monotonic_ns"], record["monotonic_ns"], CADENCE_NS
        )
        if record["sampling_gap"] != gap:
            raise EvidenceError("sampling gap is inconsistent")
        self.final_atlas = record["atlas"]
        self.count += 1

    def finish(self) -> None:
        if self.count == 0:
            raise EvidenceError("raw sample count is missing or exceeds its bound")
        if self.idle_count != IDLE_DURATION_NS // CADENCE_NS + 1:
            raise EvidenceError("five-second idle control sample count is inconsistent")
        if (
            self.first_idle_scheduled is None
            or self.last_idle_scheduled is None
            or self.last_idle_scheduled - self.first_idle_scheduled != IDLE_DURATION_NS
        ):
            raise EvidenceError("five-second idle control duration is inconsistent")
        if self.campaign_count < 2:
            raise EvidenceError("campaign samples are missing process start or exit")
        if (
            not isinstance(self.final_atlas, dict)
            or self.final_atlas["status"] != "gap"
            or not isinstance(self.final_atlas["error"], dict)
            or self.final_atlas["error"]["kind"] not in ("exited", "pid_reuse")
        ):
            raise EvidenceError("final Atlas exit or PID-identity gap is missing")


def validate_raw_records(
    records: list[dict[str, object]],
    *,
    maximum_raw_samples: int,
    enforce_maximum_limit: bool = True,
) -> None:
    validator = RawRecordStreamValidator(
        maximum_raw_samples, enforce_maximum_limit=enforce_maximum_limit
    )
    for record in records:
        validator.observe(record)
    validator.finish()


def validate_evidence(
    raw: bytes,
    summary: object,
    *,
    evidence_directory: Path | None = None,
) -> None:
    if evidence_directory is not None:
        _reject_mixed_memory_generation(evidence_directory, segmented=False)
    if not raw or len(raw) > MAX_RAW_BYTES:
        raise EvidenceError("raw evidence byte bound exceeded")
    records = [_strict_json(line, "raw evidence record") for line in raw.splitlines()]
    if not isinstance(summary, dict):
        raise EvidenceError("summary is malformed")
    required = {
        "schema_version", "kind", "platform", "tool", "cadence_ms",
        "idle_control_duration_ms", "maximum_raw_samples", "maximum_raw_bytes",
        "raw_record_count", "raw_sha256", "exit", "baseline_visible_not_subtracted",
        "baseline", "campaign", "events", "limitations", "claim",
    }
    capacity_execution = summary.get("capacity_execution")
    if capacity_execution is not None:
        validate_capacity_execution(capacity_execution)
        required.add("capacity_execution")
    if (
        set(summary) != required
        or summary.get("schema_version") != SCHEMA_VERSION
        or summary.get("kind") != SUMMARY_KIND
        or summary.get("cadence_ms") != 50
        or summary.get("idle_control_duration_ms") != 5_000
        or summary.get("maximum_raw_bytes") != MAX_RAW_BYTES
        or summary.get("baseline_visible_not_subtracted") is not True
    ):
        raise EvidenceError("summary schema or fixed method is malformed")
    metadata = summary["platform"]
    if (
        not isinstance(metadata, dict)
        or set(metadata) != {
            "os", "architecture", "host_release", "processor", "logical_cpu_count",
            "python_version", "method", "advisory", "limitations",
        }
        or metadata["os"] not in ("windows", "linux", "macos")
        or not all(
            isinstance(metadata[field], str) and metadata[field]
            for field in ("architecture", "host_release", "processor", "python_version", "method")
        )
        or not isinstance(metadata["advisory"], bool)
        or not isinstance(metadata["limitations"], list)
        or not metadata["limitations"]
        or not all(isinstance(item, str) and item for item in metadata["limitations"])
        or (
            metadata["logical_cpu_count"] is not None
            and (
                not isinstance(metadata["logical_cpu_count"], int)
                or metadata["logical_cpu_count"] <= 0
            )
        )
    ):
        raise EvidenceError("platform and method metadata is malformed")
    if summary["tool"] != _tool_metadata():
        raise EvidenceError("sampler tool metadata or digest is inconsistent")
    exit_record = summary["exit"]
    if (
        not isinstance(exit_record, dict)
        or set(exit_record) != {"status", "code"}
        or not isinstance(exit_record["code"], int)
        or isinstance(exit_record["code"], bool)
        or exit_record["status"] != ("clean" if exit_record["code"] == 0 else "failed")
    ):
        raise EvidenceError("sampled Atlas exit record is malformed")
    maximum = summary.get("maximum_raw_samples")
    if not isinstance(maximum, int) or maximum <= 0 or maximum > MAX_RAW_SAMPLES:
        raise EvidenceError("summary sample bound is invalid")
    validate_raw_records(records, maximum_raw_samples=maximum)
    if summary["raw_sha256"] != hashlib.sha256(raw).hexdigest():
        raise EvidenceError("raw digest binding mismatch")
    if summary["raw_record_count"] != len(records):
        raise EvidenceError("raw record count mismatch")
    expected = build_summary(
        records, raw, metadata, exit_code=exit_record["code"],
        cadence_ns=CADENCE_NS, idle_duration_ns=IDLE_DURATION_NS,
        maximum_raw_samples=maximum,
        capacity_execution=capacity_execution,
    )
    if summary != expected:
        raise EvidenceError("summary does not match validated raw samples")


def _windows_evidence_handle_info(descriptor: int) -> tuple[int, int]:
    if os.name != "nt":
        raise AssertionError("Windows handle information requested on another platform")

    class FileTime(ctypes.Structure):
        _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

    class ByHandleFileInformation(ctypes.Structure):
        _fields_ = [
            ("attributes", wintypes.DWORD),
            ("creation_time", FileTime),
            ("last_access_time", FileTime),
            ("last_write_time", FileTime),
            ("volume_serial_number", wintypes.DWORD),
            ("file_size_high", wintypes.DWORD),
            ("file_size_low", wintypes.DWORD),
            ("number_of_links", wintypes.DWORD),
            ("file_index_high", wintypes.DWORD),
            ("file_index_low", wintypes.DWORD),
        ]

    information = ByHandleFileInformation()
    raw_handle = msvcrt.get_osfhandle(descriptor)
    if not _kernel32.GetFileInformationByHandle(
        wintypes.HANDLE(raw_handle), ctypes.byref(information)
    ):
        raise ctypes.WinError()
    return int(information.attributes), int(information.number_of_links)


def _validate_physical_evidence_handle(handle: object, maximum_bytes: int) -> os.stat_result:
    metadata = os.fstat(handle.fileno())
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_size <= 0
        or metadata.st_size > maximum_bytes
        or metadata.st_nlink != 1
    ):
        raise EvidenceError("evidence file is missing, externally linked, empty, or unbounded")
    if os.name == "nt":
        attributes, links = _windows_evidence_handle_info(handle.fileno())
        if attributes & 0x0400 or links != 1:
            raise EvidenceError("evidence file is a reparse point or externally linked")
    return metadata


def _open_physical_evidence_file(path: Path, maximum_bytes: int):
    if os.name == "nt":
        create_file = _kernel32.CreateFileW
        create_file.argtypes = (
            wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
            wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
        )
        create_file.restype = wintypes.HANDLE
        raw_handle = create_file(
            str(path),
            0x80000000,
            0x00000001 | 0x00000002 | 0x00000004,
            None,
            3,
            0x00200000,
            None,
        )
        if raw_handle == wintypes.HANDLE(-1).value:
            raise ctypes.WinError()
        try:
            descriptor = msvcrt.open_osfhandle(raw_handle, os.O_RDONLY | os.O_BINARY)
        except BaseException:
            _kernel32.CloseHandle(raw_handle)
            raise
    else:
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
    handle = os.fdopen(descriptor, "rb")
    try:
        _validate_physical_evidence_handle(handle, maximum_bytes)
    except BaseException:
        handle.close()
        raise
    return handle


def classify_memory_artifact_name(name: str) -> tuple[str, int] | None:
    if "/" in name or "\\" in name:
        return None
    for suffix, classification in (
        ("-memory-summary-v1.json", ("summary", 1)),
        ("-memory-summary-v2.json", ("summary", 2)),
        ("-memory-raw-v1.ndjson", ("raw", 1)),
    ):
        prefix = name.removesuffix(suffix)
        if prefix != name and prefix:
            return classification
    stem = name.removesuffix(".ndjson")
    prefix, marker, ordinal = stem.rpartition("-memory-raw-v2-")
    if (
        stem != name
        and marker
        and prefix
        and len(ordinal) == 6
        and all("0" <= digit <= "9" for digit in ordinal)
    ):
        return "raw", 2
    return None


def _memory_artifact_names(directory: Path) -> set[str]:
    return {
        entry.name
        for entry in directory.iterdir()
        if classify_memory_artifact_name(entry.name) is not None
    }


def _reject_mixed_memory_generation(
    directory: Path,
    *,
    segmented: bool,
    selected_summary: str | None = None,
) -> None:
    artifacts = _memory_artifact_names(directory)
    if segmented:
        forbidden = {
            name
            for name in artifacts
            if classify_memory_artifact_name(name) == ("raw", 1)
            or classify_memory_artifact_name(name) == ("summary", 1)
            or (
                classify_memory_artifact_name(name) == ("summary", 2)
                and name != selected_summary
            )
        }
    else:
        forbidden = {
            name
            for name in artifacts
            if classify_memory_artifact_name(name) in (("raw", 2), ("summary", 2))
        }
    if forbidden:
        raise EvidenceError("memory evidence generations contain mixed or undeclared extras")


def validate_segmented_evidence(
    summary_path: Path,
    *,
    before_segment_read: Callable[[Path, int], None] | None = None,
    validated_output: BinaryIO | None = None,
) -> None:
    _reject_mixed_memory_generation(
        summary_path.parent,
        segmented=True,
        selected_summary=summary_path.name,
    )
    with _open_physical_evidence_file(summary_path, 1024 * 1024) as summary_handle:
        summary_bytes = summary_handle.read(1024 * 1024 + 1)
        summary_metadata = _validate_physical_evidence_handle(
            summary_handle, 1024 * 1024
        )
    if len(summary_bytes) != summary_metadata.st_size:
        raise EvidenceError("segmented summary changed while its handle was held")
    summary = _strict_json(summary_bytes, "process-tree memory segmented summary")
    if not isinstance(summary, dict):
        raise EvidenceError("segmented summary is malformed")


    required = {
        "schema_version", "kind", "platform", "tool", "cadence_ms",
        "idle_control_duration_ms", "segment_name_prefix",
        "maximum_segment_samples", "maximum_segment_bytes", "maximum_segments",
        "raw_record_count", "raw_byte_length", "raw_sha256", "segments", "exit",
        "baseline_visible_not_subtracted", "baseline", "campaign", "events",
        "limitations", "claim",
    }
    capacity_execution = summary.get("capacity_execution")
    if capacity_execution is not None:
        validate_capacity_execution(capacity_execution)
        required.add("capacity_execution")
    prefix = summary.get("segment_name_prefix")
    maximum_samples = summary.get("maximum_segment_samples")
    maximum_bytes = summary.get("maximum_segment_bytes")
    maximum_segments = summary.get("maximum_segments")
    segments = summary.get("segments")
    if (
        set(summary) != required
        or summary.get("schema_version") != SEGMENTED_SCHEMA_VERSION
        or summary.get("kind") != SEGMENTED_SUMMARY_KIND
        or summary.get("cadence_ms") != 50
        or summary.get("idle_control_duration_ms") != 5_000
        or summary.get("baseline_visible_not_subtracted") is not True
        or not isinstance(prefix, str)
        or not prefix
        or Path(prefix).name != prefix
        or not isinstance(maximum_samples, int)
        or isinstance(maximum_samples, bool)
        or not 0 < maximum_samples <= MAX_RAW_SAMPLES
        or not isinstance(maximum_bytes, int)
        or isinstance(maximum_bytes, bool)
        or not 0 < maximum_bytes <= MAX_RAW_BYTES
        or maximum_segments != MAX_SEGMENTS
        or not isinstance(segments, list)
        or not segments
        or len(segments) > MAX_SEGMENTS
    ):
        raise EvidenceError("segmented summary schema or fixed method is malformed")
    metadata = summary["platform"]
    if (
        not isinstance(metadata, dict)
        or set(metadata) != {
            "os", "architecture", "host_release", "processor", "logical_cpu_count",
            "python_version", "method", "advisory", "limitations",
        }
        or metadata["os"] not in ("windows", "linux", "macos")
        or not all(
            isinstance(metadata[field], str) and metadata[field]
            for field in ("architecture", "host_release", "processor", "python_version", "method")
        )
        or not isinstance(metadata["advisory"], bool)
        or not isinstance(metadata["limitations"], list)
        or not metadata["limitations"]
        or not all(isinstance(item, str) and item for item in metadata["limitations"])
        or (
            metadata["logical_cpu_count"] is not None
            and (
                not isinstance(metadata["logical_cpu_count"], int)
                or isinstance(metadata["logical_cpu_count"], bool)
                or metadata["logical_cpu_count"] <= 0
            )
        )
        or summary["tool"] != _tool_metadata()
    ):
        raise EvidenceError("segmented platform or sampler metadata is malformed")
    exit_record = summary["exit"]
    if (
        not isinstance(exit_record, dict)
        or set(exit_record) != {"status", "code"}
        or not isinstance(exit_record["code"], int)
        or isinstance(exit_record["code"], bool)
        or exit_record["status"] != ("clean" if exit_record["code"] == 0 else "failed")
    ):
        raise EvidenceError("segmented sampled Atlas exit record is malformed")

    declared_total = summary.get("raw_record_count")
    if (
        not isinstance(declared_total, int)
        or isinstance(declared_total, bool)
        or declared_total <= 0
    ):
        raise EvidenceError("whole-run raw record count is malformed")
    validator = RawRecordStreamValidator(
        declared_total, enforce_maximum_limit=False
    )
    accumulator = StreamingSummaryAccumulator(summary_path.parent)
    whole_digest = hashlib.sha256()
    whole_bytes = 0
    if validated_output is not None:
        validated_output.write(VALIDATED_BUNDLE_MAGIC)
    expected_names: set[str] = set()
    next_sequence = 0
    try:
        for order, segment in enumerate(segments):
            expected_name = f"{prefix}-{order:06d}.ndjson"
            expected_names.add(expected_name)
            if (
                not isinstance(segment, dict)
                or set(segment) != {
                    "path", "order", "first_sequence", "last_sequence",
                    "record_count", "byte_length", "sha256",
                }
                or segment["path"] != expected_name
                or segment["order"] != order
                or segment["first_sequence"] != next_sequence
                or not isinstance(segment["record_count"], int)
                or isinstance(segment["record_count"], bool)
                or not 0 < segment["record_count"] <= maximum_samples
                or segment["last_sequence"] != next_sequence + segment["record_count"] - 1
                or not isinstance(segment["byte_length"], int)
                or isinstance(segment["byte_length"], bool)
                or not 0 < segment["byte_length"] <= maximum_bytes
                or not isinstance(segment["sha256"], str)
                or len(segment["sha256"]) != 64
            ):
                raise EvidenceError("raw segment manifest identity or bounds are inconsistent")
            segment_path = summary_path.parent / expected_name
            segment_digest = hashlib.sha256()
            segment_bytes = 0
            segment_records = 0
            with _open_physical_evidence_file(segment_path, maximum_bytes) as handle:
                if before_segment_read is not None:
                    before_segment_read(segment_path, order)
                pre_read_metadata = _validate_physical_evidence_handle(
                    handle, maximum_bytes
                )
                if pre_read_metadata.st_size != segment["byte_length"]:
                    raise EvidenceError("raw segment changed before its handle was consumed")
                if validated_output is not None:
                    frame = {
                        "byte_length": segment["byte_length"],
                        "name": expected_name,
                        "order": order,
                        "record_count": segment["record_count"],
                        "sha256": segment["sha256"],
                    }
                    validated_output.write(
                        VALIDATED_BUNDLE_FRAME
                        + json.dumps(frame, separators=(",", ":"), sort_keys=True).encode("utf-8")
                        + b"\n"
                    )
                for line in handle:
                    if (
                        len(line) > MAX_RAW_RECORD_BYTES
                        or not line.endswith(b"\n")
                        or line.endswith(b"\r\n")
                        or line == b"\n"
                    ):
                        raise EvidenceError(
                            "raw segment record is empty, unterminated, non-LF, or over-bound"
                        )
                    segment_digest.update(line)
                    whole_digest.update(line)
                    segment_bytes += len(line)
                    record = _strict_json(line, "raw segment record")
                    validator.observe(record)
                    accumulator.observe(record)
                    if validated_output is not None:
                        validated_output.write(line)
                    segment_records += 1
                final_metadata = _validate_physical_evidence_handle(
                    handle, maximum_bytes
                )
            if final_metadata.st_size != segment["byte_length"]:
                raise EvidenceError("raw segment changed while its handle was held")
            if (
                segment_bytes != segment["byte_length"]
                or segment_digest.hexdigest() != segment["sha256"]
            ):
                raise EvidenceError("raw segment byte length or digest binding mismatch")
            if segment_records != segment["record_count"]:
                raise EvidenceError("raw segment record count mismatch")
            whole_bytes += segment_bytes
            next_sequence += segment_records
        actual_names = {
            name
            for name in _memory_artifact_names(summary_path.parent)
            if classify_memory_artifact_name(name) == ("raw", 2)
        }
        if actual_names != expected_names:
            raise EvidenceError("raw segments are missing, duplicated, or contain extras")
        validator.finish()
        baseline, campaign, events = accumulator.summary_parts()
        if (
            summary["raw_record_count"] != validator.count
            or summary["raw_byte_length"] != whole_bytes
            or summary["raw_sha256"] != whole_digest.hexdigest()
        ):
            raise EvidenceError("whole-run raw totals or digest binding mismatch")
        expected = _build_segmented_summary(
            record_count=validator.count,
            segments=segments,
            metadata=metadata,
            baseline=baseline,
            campaign=campaign,
            events=events,
            segment_name_prefix=prefix,
            exit_code=exit_record["code"],
            cadence_ns=CADENCE_NS,
            idle_duration_ns=IDLE_DURATION_NS,
            maximum_segment_samples=maximum_samples,
            maximum_segment_bytes=maximum_bytes,
            raw_sha256=whole_digest.hexdigest(),
            raw_byte_length=whole_bytes,
            capacity_execution=capacity_execution,
        )
        if summary != expected:
            raise EvidenceError("segmented summary does not match validated raw samples")
        if validated_output is not None:
            summary_frame = {
                "byte_length": len(summary_bytes),
                "name": summary_path.name,
                "order": len(segments),
                "record_count": 1,
                "sha256": hashlib.sha256(summary_bytes).hexdigest(),
            }
            validated_output.write(
                VALIDATED_BUNDLE_FRAME
                + json.dumps(summary_frame, separators=(",", ":"), sort_keys=True).encode("utf-8")
                + b"\n"
            )
            validated_output.write(summary_bytes)
            completion = {
                "artifact_count": len(segments) + 1,
                "raw_byte_length": whole_bytes,
                "raw_record_count": validator.count,
                "raw_sha256": whole_digest.hexdigest(),
            }
            validated_output.write(
                VALIDATED_BUNDLE_COMPLETE
                + json.dumps(completion, separators=(",", ":"), sort_keys=True).encode("utf-8")
                + b"\n"
            )
            validated_output.flush()
    finally:
        accumulator.close()




def fixture_evidence_records(labels: dict[str, str]) -> list[dict[str, object]]:
    clock = FakeClock()
    observer = observed_measurement(ProcessNode(900, 0, "observer", 10, 5), "fixture")
    records = collect_idle_control(
        FixtureObserver(observer), clock.monotonic_ns, clock.sleep, sampler_pid=900,
        duration_ns=IDLE_DURATION_NS, cadence_ns=CADENCE_NS, max_samples=101,
    )
    atlas = observed_measurement(ProcessNode(10, 0, "atlas", 100, 80), "fixture")
    provider = observed_measurement(ProcessNode(11, 10, "provider", 50, 30), "fixture")
    scheduled = IDLE_DURATION_NS + CADENCE_NS
    records.append({
        "schema_version": SCHEMA_VERSION, "kind": RAW_KIND, "sequence": len(records),
        "control": "campaign", "scheduled_monotonic_ns": scheduled,
        "monotonic_ns": scheduled, "atlas_pid": 10,
        "observed_descendant_provider_pids": [11], "labels": dict(labels),
        "observer": observer, "atlas": atlas, "providers": [provider],
        "aggregate": concurrent_totals(atlas, [provider]),
        "sampling_gap": sampling_gap(scheduled, scheduled, CADENCE_NS),
        "child_events": [], "tree_observation": {"status": "observed", "error": None},
    })
    scheduled += CADENCE_NS
    atlas_exit = measurement_gap(
        10, "exited", "Atlas exited after the prior sample", "fixture", identity="atlas"
    )
    records.append({
        "schema_version": SCHEMA_VERSION, "kind": RAW_KIND, "sequence": len(records),
        "control": "campaign", "scheduled_monotonic_ns": scheduled,
        "monotonic_ns": scheduled, "atlas_pid": 10,
        "observed_descendant_provider_pids": [], "labels": dict(labels),
        "observer": observer, "atlas": atlas_exit, "providers": [],
        "aggregate": concurrent_totals(atlas_exit, []),
        "sampling_gap": sampling_gap(scheduled, scheduled, CADENCE_NS),
        "child_events": [{"type": "descendant_exited", "pid": 11, "identity": "provider"}],
        "tree_observation": {"status": "observed", "error": None},
    })
    return records


def _open_label_reader(path: Path):
    if sys.platform != "win32":
        return path.open("rb")

    import msvcrt

    create_file = ctypes.windll.kernel32.CreateFileW
    create_file.argtypes = (
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    )
    create_file.restype = wintypes.HANDLE
    handle = create_file(
        str(path),
        0x80000000,  # GENERIC_READ
        0x00000001 | 0x00000002 | 0x00000004,  # FILE_SHARE_READ|WRITE|DELETE
        None,
        3,  # OPEN_EXISTING
        0x00000080,  # FILE_ATTRIBUTE_NORMAL
        None,
    )
    if handle == wintypes.HANDLE(-1).value:
        raise ctypes.WinError()
    try:
        descriptor = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
    except BaseException:
        ctypes.windll.kernel32.CloseHandle(handle)
        raise
    return os.fdopen(descriptor, "rb")




def _read_label_bytes(path: Path) -> bytes:
    for retry in range(65):
        try:
            with _open_label_reader(path) as reader:
                return reader.read(MAX_LABEL_BYTES + 1)
        except OSError as error:
            if (
                sys.platform == "win32"
                and getattr(error, "winerror", None) in (2, 5, 32)
                and retry < 64
            ):
                time.sleep(0.001)
                continue
            raise
    raise AssertionError("bounded label read loop always returns or raises")


def read_labels(path: Path) -> dict[str, str]:
    try:
        data = _read_label_bytes(path)
    except OSError as error:
        raise EvidenceError(f"cannot read state labels: {_error_kind(error)}: {error}") from error
    if not data or len(data) > MAX_LABEL_BYTES:
        raise EvidenceError("state label record is empty or exceeds its bound")
    labels = _strict_json(data, "state label record")
    _split_label_state(labels)
    return labels


def build_atlas_command(executable: Path, scenario: Path, manifests: Path,
                        evidence: Path, labels: Path, shard_index: int | None = None,
                        shard_count: int | None = None) -> list[str]:
    command = [
        str(executable), "--capacity-run", "--manifest", str(scenario),
        "--capacity-manifest-dir", str(manifests), "--evidence-dir", str(evidence),
        "--memory-label-state", str(labels),
    ]
    if (shard_index is None) != (shard_count is None):
        raise EvidenceError("capacity shard index and count must be supplied together")
    if shard_index is not None:
        if shard_count != CAPACITY_SHARD_COUNT or not 0 <= shard_index < CAPACITY_SHARD_COUNT:
            raise EvidenceError("capacity shard contract requires index 0..3 and count 4")
        command.extend([
            "--capacity-shard-index", str(shard_index),
            "--capacity-shard-count", str(shard_count),
        ])
    return command


def subprocess_invocation_contract() -> str:
    return "argument-vector-only"


class SegmentedRawWriter:
    def __init__(
        self,
        directory: Path,
        prefix: str,
        *,
        maximum_segment_samples: int,
        maximum_segment_bytes: int,
        maximum_segments: int = MAX_SEGMENTS,
    ):
        if (
            not prefix
            or Path(prefix).name != prefix
            or maximum_segment_samples <= 0
            or maximum_segment_samples > MAX_RAW_SAMPLES
            or maximum_segment_bytes <= 0
            or maximum_segment_bytes > MAX_RAW_BYTES
            or maximum_segments <= 0
            or maximum_segments > MAX_SEGMENTS
        ):
            raise EvidenceError("raw segment name or bounds are invalid")
        self.directory = directory
        self.prefix = prefix
        self.maximum_segment_samples = maximum_segment_samples
        self.maximum_segment_bytes = maximum_segment_bytes
        self.maximum_segments = maximum_segments
        self.count = 0
        self.bytes_written = 0
        self.digest = hashlib.sha256()
        self.segments: list[dict[str, object]] = []
        self.segment_order = 0
        self.segment_count = 0
        self.segment_bytes = 0
        self.segment_digest = hashlib.sha256()
        self.path = self._segment_path(self.segment_order)
        self.file = self.path.open("xb")

    def _segment_path(self, order: int) -> Path:
        return self.directory / f"{self.prefix}-{order:06d}.ndjson"

    def _finish_segment(self) -> None:
        if self.segment_count == 0:
            raise EvidenceError("raw segment is empty")
        self.file.flush()
        os.fsync(self.file.fileno())
        self.file.close()
        first_sequence = self.count - self.segment_count
        self.segments.append({
            "path": self.path.name,
            "order": self.segment_order,
            "first_sequence": first_sequence,
            "last_sequence": self.count - 1,
            "record_count": self.segment_count,
            "byte_length": self.segment_bytes,
            "sha256": self.segment_digest.hexdigest(),
        })

    def _rollover(self) -> None:
        if self.segment_order + 1 >= self.maximum_segments:
            raise EvidenceError("whole-run raw segment count exceeds its bound")
        self._finish_segment()
        self.segment_order += 1
        self.segment_count = 0
        self.segment_bytes = 0
        self.segment_digest = hashlib.sha256()
        self.path = self._segment_path(self.segment_order)
        self.file = self.path.open("xb")

    def write(self, record: dict[str, object]) -> None:
        if not isinstance(record, dict):
            raise EvidenceError("raw evidence record is not an object")
        record["sequence"] = self.count
        encoded_record = json.dumps(
            record, sort_keys=True, separators=(",", ":"), ensure_ascii=True
        ).encode("utf-8") + b"\n"
        if len(encoded_record) > self.maximum_segment_bytes:
            raise EvidenceError("one raw sample exceeds the segment byte bound")
        if (
            self.segment_count >= self.maximum_segment_samples
            or self.segment_bytes + len(encoded_record) > self.maximum_segment_bytes
        ):
            self._rollover()
        self.file.write(encoded_record)
        self.digest.update(encoded_record)
        self.segment_digest.update(encoded_record)
        self.count += 1
        self.bytes_written += len(encoded_record)
        self.segment_count += 1
        self.segment_bytes += len(encoded_record)

    def finish(self) -> list[dict[str, object]]:
        self._finish_segment()
        return list(self.segments)

    def close_partial(self) -> None:
        if not self.file.closed:
            self.file.flush()
            os.fsync(self.file.fileno())
            self.file.close()


def _campaign_record(sequence: int, scheduled: int, actual: int, atlas_pid: int,
                     labels: dict[str, str], observer_measurement: dict[str, object],
                     snapshot: TreeSnapshot, previous: dict[int, str]) -> dict[str, object]:
    providers = sorted(snapshot.providers, key=lambda value: int(value["pid"]))
    events = descendant_events(previous, providers)
    aggregate = concurrent_totals(
        snapshot.atlas, providers, tree_complete=snapshot.tree_status == "observed"
    )
    return {
        "schema_version": SCHEMA_VERSION, "kind": RAW_KIND, "sequence": sequence,
        "control": "campaign", "scheduled_monotonic_ns": scheduled, "monotonic_ns": actual,
        "atlas_pid": atlas_pid, "observed_descendant_provider_pids": [item["pid"] for item in providers],
        "labels": labels, "observer": observer_measurement, "atlas": snapshot.atlas,
        "providers": providers, "aggregate": aggregate,
        "sampling_gap": sampling_gap(scheduled, actual, CADENCE_NS), "child_events": events,
        "tree_observation": {"status": snapshot.tree_status, "error": snapshot.tree_error},
    }


def _safe_evidence_directory(path: Path) -> Path:
    resolved = path.resolve(strict=True)
    target = (ROOT / "target").resolve(strict=True)
    try:
        resolved.relative_to(target)
    except ValueError as error:
        raise EvidenceError("memory evidence directory must remain under package-excluded target/**") from error
    current = target
    for part in resolved.relative_to(target).parts:
        current = current / part
        if current.is_symlink() or not current.is_dir():
            raise EvidenceError("memory evidence path contains a link or non-directory component")
    return resolved


def run_measurement(
    command: list[str],
    evidence_dir: Path,
    label_reader: Callable[[], dict[str, str]],
    *,
    raw_name: str = RAW_NAME,
    summary_name: str = SUMMARY_NAME,
    maximum_samples: int = MAX_RAW_SAMPLES,
    maximum_bytes: int = MAX_RAW_BYTES,
    capacity_execution: dict[str, object] | None = None,
) -> tuple[Path, Path, int]:
    metadata = platform_metadata()
    observer = native_observer()
    evidence = _safe_evidence_directory(evidence_dir)
    summary_path = evidence / summary_name
    if (
        summary_path.exists()
        or any(evidence.glob(f"{raw_name}-*.ndjson"))
        or (evidence / raw_name).exists()
        or (evidence / LEGACY_RAW_NAME).exists()
        or (evidence / LEGACY_SUMMARY_NAME).exists()
    ):
        raise EvidenceError("memory evidence destination already exists")
    idle_records = collect_idle_control(
        observer, time.monotonic_ns, time.sleep, sampler_pid=os.getpid(),
        duration_ns=IDLE_DURATION_NS, cadence_ns=CADENCE_NS,
        max_samples=IDLE_DURATION_NS // CADENCE_NS + 1,
    )
    progress = ProgressReporter()
    _, initial_progress = _split_label_state(label_reader())
    progress._observe(initial_progress)
    accumulator = StreamingSummaryAccumulator(evidence)
    writer: SegmentedRawWriter | None = None
    process = None
    owner = None
    try:
        writer = SegmentedRawWriter(
            evidence,
            raw_name,
            maximum_segment_samples=maximum_samples,
            maximum_segment_bytes=maximum_bytes,
        )
        validator = RawRecordStreamValidator(MAX_BYTES, enforce_maximum_limit=False)
        for record in idle_records:
            writer.write(record)
            validator.observe(record)
            accumulator.observe(record)
        process, owner = ProcessOwner.spawn(command, ROOT)
        previous: dict[int, str] = {}
        atlas_identity: str | None = None
        scheduled = time.monotonic_ns()
        while True:
            actual = wait_until_scheduled(scheduled, time.monotonic_ns, time.sleep)
            exit_code = process.poll()
            known_tree_empty = owner.active_process_count() == 0
            label_state = label_reader()
            labels, current_progress = _split_label_state(label_state)
            progress._observe(current_progress)
            observer_measurement = observer.observe_process(os.getpid())
            snapshot = observer.observe_tree(
                process.pid,
                retained=previous,
                contained_group_id=owner.contained_group_id(),
                contained_pids=owner.contained_pids(),
            )
            if snapshot.atlas["status"] == "observed":
                current_identity = str(snapshot.atlas["identity"])
                if atlas_identity is None:
                    atlas_identity = current_identity
                elif current_identity != atlas_identity:
                    snapshot = TreeSnapshot(
                        measurement_gap(
                            process.pid, "pid_reuse", "Atlas PID identity changed after spawn",
                            snapshot.atlas["method"], identity=current_identity,
                        ),
                        snapshot.providers,
                        "gap",
                        {"kind": "pid_reuse", "detail": "Atlas PID identity changed after spawn"},
                    )
            exit_code = process.poll()
            if exit_code is not None:
                providers = snapshot.providers
                if known_tree_empty and previous:
                    providers = [
                        measurement_gap(
                            pid, "exited", "contained process group exited",
                            observer.method, identity=identity,
                        )
                        for pid, identity in sorted(previous.items())
                    ]
                snapshot = TreeSnapshot(
                    measurement_gap(
                        process.pid, "exited", "Atlas exit observed by owned process handle",
                        snapshot.atlas["method"], identity=atlas_identity,
                    ),
                    providers,
                    snapshot.tree_status,
                    snapshot.tree_error,
                )
            record = _campaign_record(
                writer.count, scheduled, actual, process.pid, labels,
                observer_measurement, snapshot, previous,
            )
            writer.write(record)
            validator.observe(record)
            accumulator.observe(record)
            previous = retained_descendant_identities(previous, snapshot.providers)
            if exit_code is not None and known_tree_empty:
                break
            scheduled += CADENCE_NS
        progress.finish(exit_code)
        process.wait(timeout=5)
        owner.close()
        owner = None
        validator.finish()
        segments = writer.finish()
        baseline, campaign, events = accumulator.summary_parts()
        summary = _build_segmented_summary(
            record_count=validator.count,
            segments=segments,
            metadata=metadata,
            baseline=baseline,
            campaign=campaign,
            events=events,
            segment_name_prefix=raw_name,
            exit_code=exit_code,
            cadence_ns=CADENCE_NS,
            idle_duration_ns=IDLE_DURATION_NS,
            maximum_segment_samples=maximum_samples,
            maximum_segment_bytes=maximum_bytes,
            raw_sha256=writer.digest.hexdigest(),
            raw_byte_length=writer.bytes_written,
            capacity_execution=capacity_execution,
        )
        summary_text = json.dumps(summary, indent=2, sort_keys=True) + "\n"
        with summary_path.open("x", encoding="utf-8", errors="strict", newline="\n") as handle:
            handle.write(summary_text)
            handle.flush()
            os.fsync(handle.fileno())
        accumulator.close()
        validate_segmented_evidence(summary_path)
        return evidence / str(segments[0]["path"]), summary_path, exit_code
    except BaseException:
        if owner is not None:
            owner.terminate()
            owner.close()
        if writer is not None:
            writer.close_partial()
        raise
    finally:
        accumulator.close()


def _regular_command(args: argparse.Namespace) -> list[str]:
    executable = args.atlas_executable.resolve(strict=True)
    if not executable.is_file():
        raise EvidenceError("Atlas executable is not a regular file")
    return build_atlas_command(
        executable, args.scenario, args.capacity_manifest_dir, args.evidence_dir,
        args.label_state, args.capacity_shard_index, args.capacity_shard_count,
    )


def _host_smoke_command() -> list[str]:
    source = (
        "import subprocess,sys,time;"
        "subprocess.Popen([sys.executable,'-c','import time;time.sleep(1)']);"
        "time.sleep(.35)"
    )
    return [sys.executable, "-c", source]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--label-state", type=Path)
    parser.add_argument("--atlas-executable", type=Path)
    parser.add_argument("--scenario", type=Path)
    parser.add_argument("--capacity-manifest-dir", type=Path)
    parser.add_argument("--global-plan-sha256")
    parser.add_argument("--capacity-shard-index", type=int)
    parser.add_argument("--capacity-shard-count", type=int)
    parser.add_argument("--host-smoke", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.host_smoke:
            if sys.platform != "win32":
                raise UnsupportedPlatformError("available-host smoke is Windows-only for this task")
            if any(value is not None for value in (
                args.label_state, args.atlas_executable, args.scenario,
                args.capacity_manifest_dir, args.global_plan_sha256,
                args.capacity_shard_index, args.capacity_shard_count,
            )):
                raise EvidenceError("host smoke does not accept capacity command inputs")
            labels = {
                "phase": "windows-host-smoke", "filesystem_cache_state": "uncontrolled",
                "serving_state": "not-applicable", "provider_cache_install_state": "not-applicable",
                "build_state": "python-prebuilt", "catalogue_state": "not-applicable",
            }
            raw, summary, code = run_measurement(
                _host_smoke_command(), args.evidence_dir, lambda: labels,
                raw_name="host-smoke-memory-raw-v2",
                summary_name="host-smoke-memory-summary-v2.json", maximum_samples=256,
            )
        else:
            if any(value is None for value in (
                args.label_state, args.atlas_executable, args.scenario,
                args.capacity_manifest_dir, args.global_plan_sha256,
            )):
                raise EvidenceError("capacity sampling requires label, executable, scenario, manifest, and global plan inputs")
            execution = capacity_execution_contract(
                args.global_plan_sha256, args.capacity_shard_index, args.capacity_shard_count
            )
            command = _regular_command(args)
            raw, summary, code = run_measurement(
                command, args.evidence_dir, lambda: read_labels(args.label_state),
                capacity_execution=execution,
            )
        print(f"process-tree memory raw evidence: {raw}")
        print(f"process-tree memory summary: {summary}")
        return 0 if code == 0 else 1
    except (EvidenceError, OSError, subprocess.SubprocessError) as error:
        print(f"process-tree memory failure: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
