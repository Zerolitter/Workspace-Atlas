#!/usr/bin/env python3
"""Portable, credential-free Atlas/provider and catalogue lifecycle smoke."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
from typing import BinaryIO

if os.name == "nt":
    import ctypes
    from ctypes import wintypes

ROOT = Path(__file__).resolve().parents[1]
MATRIX = ROOT / "config" / "mcp-client-matrix.toml"
DEFAULT_FIXTURE = ROOT / "tests" / "fixtures" / "catalogues" / "v1.4.sqlite"
MAX_OUTPUT = 1_048_576
TIMEOUT = 180
PRIVATE_MARKERS = ("pa" + "ddy", "workspace-atlas\\roadmap", "\\\\?\\", "api_key", "bearer ")


class BoundedProcessError(RuntimeError):
    def __init__(self, message: str, result: subprocess.CompletedProcess[str]):
        super().__init__(message)
        self.result = result


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
            ("ReadOperationCount", ctypes.c_ulonglong),
            ("WriteOperationCount", ctypes.c_ulonglong),
            ("OtherOperationCount", ctypes.c_ulonglong),
            ("ReadTransferCount", ctypes.c_ulonglong),
            ("WriteTransferCount", ctypes.c_ulonglong),
            ("OtherTransferCount", ctypes.c_ulonglong),
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
            ("dwSize", wintypes.DWORD),
            ("cntUsage", wintypes.DWORD),
            ("th32ThreadID", wintypes.DWORD),
            ("th32OwnerProcessID", wintypes.DWORD),
            ("tpBasePri", wintypes.LONG),
            ("tpDeltaPri", wintypes.LONG),
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
    _kernel32.QueryInformationJobObject.argtypes = (
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p,
    )
    _kernel32.QueryInformationJobObject.restype = wintypes.BOOL
    _kernel32.TerminateJobObject.argtypes = (wintypes.HANDLE, wintypes.UINT)
    _kernel32.TerminateJobObject.restype = wintypes.BOOL
    _kernel32.CreateToolhelp32Snapshot.argtypes = (wintypes.DWORD, wintypes.DWORD)
    _kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
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
    _kernel32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    _kernel32.WaitForSingleObject.restype = wintypes.DWORD
    _kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    _kernel32.CloseHandle.restype = wintypes.BOOL


def _resume_suspended_process(process: subprocess.Popen[bytes]) -> None:
    snapshot = _kernel32.CreateToolhelp32Snapshot(0x00000004, 0)
    if snapshot == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    thread_handle = None
    try:
        entry = _ThreadEntry32()
        entry.dwSize = ctypes.sizeof(entry)
        present = _kernel32.Thread32First(snapshot, ctypes.byref(entry))
        while present:
            if entry.th32OwnerProcessID == process.pid:
                thread_handle = _kernel32.OpenThread(0x0002, False, entry.th32ThreadID)
                if not thread_handle:
                    raise ctypes.WinError(ctypes.get_last_error())
                break
            present = _kernel32.Thread32Next(snapshot, ctypes.byref(entry))
        if thread_handle is None:
            raise RuntimeError("suspended process primary thread was not found")
        if _kernel32.ResumeThread(thread_handle) == 0xFFFFFFFF:
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        if thread_handle is not None:
            _kernel32.CloseHandle(thread_handle)
        _kernel32.CloseHandle(snapshot)


class ProcessOwner:
    """Own a complete spawned tree and expose a measured liveness boundary."""

    def __init__(self, process: subprocess.Popen[bytes], job=None):
        self.process = process
        self.job = job
        self.pgid = process.pid
        self._lock = threading.Lock()
        self._closed = False
        self._permission_error: PermissionError | None = None

    @classmethod
    def spawn(cls, command: list[str], **kwargs) -> tuple[subprocess.Popen[bytes], ProcessOwner]:
        if os.name != "nt":
            process = subprocess.Popen(command, start_new_session=True, **kwargs)
            return process, cls(process)
        job = _kernel32.CreateJobObjectW(None, None)
        if not job:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = _JobExtendedLimitInformation()
        limits.BasicLimitInformation.LimitFlags = 0x00002000
        if not _kernel32.SetInformationJobObject(
            job, 9, ctypes.byref(limits), ctypes.sizeof(limits)
        ):
            error = ctypes.WinError(ctypes.get_last_error())
            _kernel32.CloseHandle(job)
            raise error
        observed_limits = _JobExtendedLimitInformation()
        if not _kernel32.QueryInformationJobObject(
            job, 9, ctypes.byref(observed_limits), ctypes.sizeof(observed_limits), None
        ) or not observed_limits.BasicLimitInformation.LimitFlags & 0x00002000:
            error = ctypes.WinError(ctypes.get_last_error())
            _kernel32.CloseHandle(job)
            raise RuntimeError("Windows job kill-on-close limit was not observed") from error
        process = None
        try:
            process = subprocess.Popen(
                command, creationflags=0x00000004, **kwargs
            )
            process_handle = wintypes.HANDLE(int(process._handle))
            if not _kernel32.AssignProcessToJobObject(job, process_handle):
                raise ctypes.WinError(ctypes.get_last_error())
            _resume_suspended_process(process)
            return process, cls(process, job)
        except BaseException:
            if process is not None:
                _kernel32.TerminateJobObject(job, 1)
                if process.poll() is None:
                    process.kill()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
            _kernel32.CloseHandle(job)
            raise

    @property
    def containment(self) -> str:
        return "windows-job-object" if os.name == "nt" else "posix-process-group"

    @property
    def measurement(self) -> str:
        if os.name == "nt":
            return "QueryInformationJobObject(JobObjectBasicAccountingInformation).ActiveProcesses"
        return "killpg(pgid, 0) existence probe"

    def active_process_count(self) -> int:
        if self._closed:
            raise RuntimeError("process owner is closed")
        if os.name == "nt":
            accounting = _JobBasicAccountingInformation()
            if not _kernel32.QueryInformationJobObject(
                self.job, 1, ctypes.byref(accounting), ctypes.sizeof(accounting), None
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            return int(accounting.ActiveProcesses)
        try:
            os.killpg(self.pgid, 0)
        except ProcessLookupError:
            return 0
        except PermissionError as error:
            if self._permission_error is None:
                self._permission_error = error
            return 1
        return 1

    def terminate(self, *, force: bool = False) -> None:
        with self._lock:
            if self._closed:
                return
            if os.name == "nt":
                if not _kernel32.TerminateJobObject(self.job, 1):
                    raise ctypes.WinError(ctypes.get_last_error())
                return
            signal_number = signal.SIGKILL if force else signal.SIGTERM
            try:
                os.killpg(self.pgid, signal_number)
            except ProcessLookupError:
                pass
            except PermissionError as error:
                if self._permission_error is None:
                    self._permission_error = error
                try:
                    self.process.send_signal(signal_number)
                except ProcessLookupError:
                    pass
                except PermissionError as root_error:
                    if self._permission_error is None:
                        self._permission_error = root_error

    def wait_empty(self, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while True:
            if self.active_process_count() == 0:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.01)

    def close(self) -> None:
        if self._closed:
            return
        if self.active_process_count() != 0:
            raise RuntimeError("cannot close a non-empty process owner")
        if os.name == "nt" and not _kernel32.CloseHandle(self.job):
            raise ctypes.WinError(ctypes.get_last_error())
        self._closed = True


def terminate_process_tree(process: subprocess.Popen[bytes], owner: ProcessOwner) -> None:
    owner.terminate()
    try:
        process.wait(timeout=0.5)
    except subprocess.TimeoutExpired:
        pass
    if not owner.wait_empty(0.5):
        owner.terminate(force=True)
    if process.poll() is None:
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("spawned root process did not terminate") from error
    if not owner.wait_empty(2):
        error = RuntimeError("spawned process tree did not terminate")
        if owner._permission_error is not None:
            raise error from owner._permission_error
        raise error


def _self_test_posix_permission_denial() -> str:
    class PermissionDeniedProcess:
        pid = 424_242

        def __init__(self):
            self.returncode: int | None = None
            self.signals: list[int] = []

        def poll(self) -> int | None:
            return self.returncode

        def send_signal(self, signal_number: int) -> None:
            if self.returncode is None:
                self.signals.append(signal_number)
                self.returncode = -signal_number

        def wait(self, timeout: float) -> int:
            if self.returncode is None:
                raise subprocess.TimeoutExpired("permission-denial-self-test", timeout)
            return self.returncode

    original_name = os.name
    original_killpg = getattr(os, "killpg", None)
    original_sigkill = getattr(signal, "SIGKILL", None)
    original_monotonic = time.monotonic
    original_sleep = time.sleep
    tick = 0.0

    recoverable_signal_denial = PermissionError(1, "Operation not permitted")
    recoverable_group_calls: list[int] = []

    def recoverable_group_signal(_pgid: int, signal_number: int) -> None:
        recoverable_group_calls.append(signal_number)
        if signal_number == signal.SIGTERM:
            raise recoverable_signal_denial
        assert signal_number == 0
        raise ProcessLookupError

    persistent_term_denial = PermissionError(1, "Operation not permitted")
    persistent_force_denial = PermissionError(1, "Operation not permitted")
    persistent_probe_denial = PermissionError(1, "Operation not permitted")
    persistent_group_calls: list[int] = []

    def deny_group_signal(_pgid: int, signal_number: int) -> None:
        persistent_group_calls.append(signal_number)
        if signal_number == signal.SIGTERM:
            raise persistent_term_denial
        if signal_number == signal.SIGKILL:
            raise persistent_force_denial
        assert signal_number == 0
        raise persistent_probe_denial

    def advance_clock() -> float:
        nonlocal tick
        tick += 10
        return tick

    try:
        os.name = "posix"
        if original_sigkill is None:
            signal.SIGKILL = 9
        time.monotonic = advance_clock
        time.sleep = lambda _seconds: None

        os.killpg = recoverable_group_signal
        recoverable_process = PermissionDeniedProcess()
        recoverable_owner = ProcessOwner(recoverable_process)
        assert terminate_process_tree(recoverable_process, recoverable_owner) is None
        assert recoverable_process.signals == [signal.SIGTERM]
        assert recoverable_group_calls == [signal.SIGTERM, 0, 0]
        recoverable_owner.close()
        assert recoverable_group_calls == [signal.SIGTERM, 0, 0, 0]

        os.killpg = deny_group_signal
        persistent_process = PermissionDeniedProcess()
        persistent_owner = ProcessOwner(persistent_process)
        try:
            terminate_process_tree(persistent_process, persistent_owner)
        except RuntimeError as error:
            assert str(error) == "spawned process tree did not terminate"
            assert error.__cause__ is persistent_term_denial
        else:
            raise AssertionError("permission-denied process group reported successful cleanup")
        assert persistent_process.signals == [signal.SIGTERM]
        assert persistent_group_calls == [signal.SIGTERM, 0, signal.SIGKILL, 0]
        assert persistent_owner.active_process_count() == 1
    finally:
        os.name = original_name
        if original_killpg is None:
            del os.killpg
        else:
            os.killpg = original_killpg
        if original_sigkill is None:
            del signal.SIGKILL
        time.monotonic = original_monotonic
        time.sleep = original_sleep
    return "root-fallback-group-present-fail-closed"


def process_is_running(pid: int) -> bool:
    if os.name == "nt":
        handle = _kernel32.OpenProcess(0x00100000, False, pid)
        if not handle:
            return False
        try:
            return _kernel32.WaitForSingleObject(handle, 0) == 0x00000102
        finally:
            _kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def wait_process_stopped(pid: int, timeout: float = 2) -> bool:
    deadline = time.monotonic() + timeout
    while process_is_running(pid):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)
    return True


def bounded_run(argv: list[str], *, environment: dict[str, str], cwd: Path | None = None,
                ok: tuple[int, ...] = (0,), timeout: int = TIMEOUT,
                output_limit: int = MAX_OUTPUT) -> subprocess.CompletedProcess[str]:
    if not argv or any(not isinstance(value, str) or not value or "\0" in value for value in argv):
        raise ValueError("command arguments must be non-empty NUL-free strings")
    if cwd is not None and not cwd.is_dir():
        raise ValueError(f"command working directory does not exist: {cwd}")
    executable = shutil.which(argv[0], path=environment.get("PATH"))
    if executable is None:
        raise RuntimeError(f"required executable is unavailable: {argv[0]}")
    command = [executable, *argv[1:]]
    process, owner = ProcessOwner.spawn(
        command, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False,
    )
    captured = [bytearray(), bytearray()]
    exceeded = threading.Event()

    def read_stream(stream: BinaryIO, index: int) -> None:
        while chunk := stream.read(65_536):
            remaining = output_limit - len(captured[index])
            if remaining > 0:
                captured[index].extend(chunk[:remaining])
            if len(chunk) > remaining:
                exceeded.set()
                owner.terminate()
                return

    readers = [
        threading.Thread(target=read_stream, args=(process.stdout, 0), daemon=True),
        threading.Thread(target=read_stream, args=(process.stderr, 1), daemon=True),
    ]
    for reader in readers:
        reader.start()
    deadline = time.monotonic() + timeout
    failure: str | None = None
    while process.poll() is None:
        if exceeded.wait(timeout=0.02):
            failure = f"process output exceeded {output_limit} bytes"
            break
        if time.monotonic() >= deadline:
            failure = f"process timed out after {timeout} seconds"
            break
    if exceeded.is_set() and failure is None:
        failure = f"process output exceeded {output_limit} bytes"
    if failure is not None:
        terminate_process_tree(process, owner)
    for reader in readers:
        reader.join(timeout=2)
    if exceeded.is_set() and failure is None:
        failure = f"process output exceeded {output_limit} bytes"
        terminate_process_tree(process, owner)
        for reader in readers:
            reader.join(timeout=2)
    if any(reader.is_alive() for reader in readers):
        terminate_process_tree(process, owner)
        owner.close()
        raise RuntimeError("process output pipes did not close after bounded termination")
    owned_processes = owner.active_process_count()
    if failure is None and owned_processes != 0 and owner.wait_empty(2):
        owned_processes = 0
    if owned_processes != 0:
        if failure is None:
            failure = f"process owner retained {owned_processes} process(es) after root exit"
        terminate_process_tree(process, owner)
    stdout = bytes(captured[0]).decode("utf-8", errors="replace")
    stderr = bytes(captured[1]).decode("utf-8", errors="replace")
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    owner.close()
    if failure is not None:
        raise BoundedProcessError(failure, result)
    if result.returncode not in ok:
        message = redact(result.stderr or result.stdout)
        raise RuntimeError(f"command failed ({result.returncode}): {Path(command[0]).name}: {message[:2000]}")
    return result


def safe_environment(scratch_root: Path, *, include_rust: bool = False) -> dict[str, str]:
    home = scratch_root / "home"
    temporary = scratch_root / "temp"
    home.mkdir(parents=True)
    temporary.mkdir()
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "LANG"}
    result = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    result.update({
        "HOME": str(home),
        "USERPROFILE": str(home),
        "TEMP": str(temporary),
        "TMP": str(temporary),
    })
    if include_rust:
        real_home = Path.home()
        result["CARGO_HOME"] = os.environ.get("CARGO_HOME", str(real_home / ".cargo"))
        result["RUSTUP_HOME"] = os.environ.get("RUSTUP_HOME", str(real_home / ".rustup"))
        if cargo_target := os.environ.get("CARGO_TARGET_DIR"):
            result["CARGO_TARGET_DIR"] = cargo_target
    return result


def redact(value: str) -> str:
    text = value.replace(str(ROOT), "<repo>")
    home = str(Path.home())
    return text.replace(home, "<home>") if home else text


def load_matrix() -> dict:
    with MATRIX.open("rb") as handle:
        return tomllib.load(handle)


def fixture_check(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    expected = load_matrix()["fixture"]
    if digest != expected["sha256"]:
        raise RuntimeError(f"fixture hash mismatch: {digest}")
    connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True)
    try:
        integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
        version = connection.execute("PRAGMA user_version").fetchone()[0]
        workspace = connection.execute(
            "SELECT workspace_id, display_name, canonical_root, root_fingerprint FROM workspace"
        ).fetchone()
        searchable = "\n".join(
            value
            for row in connection.iterdump()
            for value in [row.lower()]
        )
    finally:
        connection.close()
    if integrity != "ok" or version != 3:
        raise RuntimeError(f"fixture invalid: integrity={integrity!r}, user_version={version}")
    if workspace != ("ws_fixture_v1_4", "v1.4 scrubbed fixture", "/fixture/workspace", "0" * 64):
        raise RuntimeError("fixture scrub identity is not exact")
    leaked = [marker for marker in PRIVATE_MARKERS if marker in searchable]
    if leaked:
        raise RuntimeError(f"fixture contains prohibited private markers: {leaked}")
    return {"sha256": digest, "bytes": len(data), "user_version": version, "integrity": integrity}
def sqlite_backup(source_path: Path, destination_path: Path) -> None:
    if destination_path.exists():
        raise RuntimeError(f"backup destination must be fresh: {destination_path}")
    source = sqlite3.connect(f"file:{source_path.as_posix()}?mode=ro", uri=True)
    destination = sqlite3.connect(destination_path)
    try:
        source.backup(destination)
    finally:
        destination.close()
        source.close()


def canonical_cell(value: object) -> list[object]:
    if value is None:
        return ["null", ""]
    if isinstance(value, bytes):
        return ["blob", value.hex()]
    if isinstance(value, int):
        return ["integer", str(value)]
    if isinstance(value, float):
        return ["real", value.hex()]
    return ["text", str(value)]


def database_snapshot(path: Path) -> dict[str, object]:
    connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True)
    try:
        integrity = connection.execute("PRAGMA integrity_check").fetchall()
        quick = connection.execute("PRAGMA quick_check").fetchall()
        foreign_keys = connection.execute("PRAGMA foreign_key_check").fetchall()
        version = connection.execute("PRAGMA user_version").fetchone()[0]
        schema = connection.execute(
            "SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name"
        ).fetchall()
        schema_counts: dict[str, int] = {}
        for kind, *_ in schema:
            schema_counts[kind] = schema_counts.get(kind, 0) + 1
        tables = [row[1] for row in schema if row[0] == "table"]
        row_counts: dict[str, int] = {}
        content: list[object] = [["schema", *row] for row in schema]
        for table in tables:
            quoted = table.replace('"', '""')
            rows = connection.execute(f'SELECT * FROM "{quoted}"').fetchall()
            encoded = [
                json.dumps([canonical_cell(value) for value in row], separators=(",", ":"))
                for row in rows
            ]
            encoded.sort()
            row_counts[table] = len(encoded)
            content.append(["table", table, encoded])
    finally:
        connection.close()
    digest = hashlib.sha256(
        json.dumps(content, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return {
        "integrity_check": integrity, "quick_check": quick,
        "foreign_key_check": foreign_keys, "user_version": version,
        "schema_counts": schema_counts, "row_counts": row_counts,
        "canonical_digest": digest,
    }


def require_healthy_v3(snapshot: dict[str, object], label: str) -> None:
    if snapshot["integrity_check"] != [("ok",)] or snapshot["quick_check"] != [("ok",)]:
        raise RuntimeError(f"{label} SQLite integrity checks failed")
    if snapshot["foreign_key_check"] != [] or snapshot["user_version"] != 3:
        raise RuntimeError(f"{label} is not a healthy schema-v3 catalogue")


def prove_restore(source_path: Path, backup_path: Path, restored_path: Path) -> dict[str, object]:
    source = database_snapshot(source_path)
    backup = database_snapshot(backup_path)
    require_healthy_v3(source, "source")
    require_healthy_v3(backup, "backup")
    if backup != source:
        raise RuntimeError("pre-migration SQLite backup differs from schema-v3 source")
    sqlite_backup(backup_path, restored_path)
    restored = database_snapshot(restored_path)
    require_healthy_v3(restored, "restored destination")
    if restored != source:
        raise RuntimeError("restored SQLite destination differs from schema-v3 source")
    usable = sqlite3.connect(restored_path)
    try:
        usable.execute("BEGIN IMMEDIATE")
        usable.execute("CREATE TABLE restore_usability_probe(value INTEGER)")
        usable.rollback()
    finally:
        usable.close()
    if database_snapshot(restored_path) != source:
        raise RuntimeError("restored SQLite destination changed during usability proof")
    return {
        "source_backup_restored_equal": True,
        "user_version": 3,
        "integrity_check": "ok", "quick_check": "ok",
        "foreign_key_check_rows": 0,
        "schema_counts": source["schema_counts"], "row_counts": source["row_counts"],
        "canonical_digest": source["canonical_digest"], "destination_usable": True,
    }


def catalogue_failure_matrix(atlas: Path, fixture: Path, temporary: Path,
                             environment: dict[str, str]) -> dict[str, object]:
    pre_migration_backup = temporary / "schema-v3.backup.sqlite"
    sqlite_backup(fixture, pre_migration_backup)
    migration = temporary / "migration.sqlite"
    sqlite_backup(pre_migration_backup, migration)
    probe_root = temporary / "migration-workspace"
    probe_root.mkdir()
    bootstrap = temporary / "bootstrap.sqlite"
    json_command(atlas, ["init", str(probe_root), "--catalogue", str(bootstrap)], environment)
    bootstrap_connection = sqlite3.connect(bootstrap)
    try:
        identity = bootstrap_connection.execute(
            "SELECT workspace_id, display_name, canonical_root, root_fingerprint, "
            "configuration_hash FROM workspace"
        ).fetchone()
    finally:
        bootstrap_connection.close()
    migration_connection = sqlite3.connect(migration)
    try:
        old_workspace_id = migration_connection.execute(
            "SELECT workspace_id FROM workspace"
        ).fetchone()[0]
        tables = [
            row[0] for row in migration_connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
        ]
        for table in tables:
            columns = [row[1] for row in migration_connection.execute(f'PRAGMA table_info("{table}")')]
            if "workspace_id" in columns:
                migration_connection.execute(
                    f'UPDATE "{table}" SET workspace_id = ? WHERE workspace_id = ?',
                    (identity[0], old_workspace_id),
                )
        migration_connection.execute(
            "UPDATE workspace SET display_name = ?, canonical_root = ?, "
            "root_fingerprint = ?, configuration_hash = ?",
            identity[1:],
        )
        migration_connection.commit()
    finally:
        migration_connection.close()
    before = table_counts(migration, exclude_migrations=True)
    json_command(atlas, ["init", str(probe_root), "--catalogue", str(migration)], environment)
    result = bounded_run(
        [str(atlas), "doctor", str(probe_root), "--catalogue", str(migration)],
        environment=environment,
    )
    after_version = scalar(migration, "PRAGMA user_version")
    after = table_counts(migration, exclude_migrations=True)
    if after_version != 4 or after != before:
        differences = {name: (before.get(name), after.get(name)) for name in before.keys() | after.keys()
                       if before.get(name) != after.get(name)}
        raise RuntimeError(
            f"old catalogue migration did not preserve non-migration rows: version={after_version}, differences={differences}"
        )
    if json.loads(result.stdout).get("ok") is not True:
        raise RuntimeError("migrated catalogue doctor result was not healthy")

    restored = temporary / "schema-v3.restored.sqlite"
    backup_restore = prove_restore(fixture, pre_migration_backup, restored)

    newer = temporary / "newer.sqlite"
    shutil.copyfile(migration, newer)
    execute(newer, "PRAGMA user_version = 5")
    newer_result = bounded_run(
        [str(atlas), "init", str(probe_root), "--catalogue", str(newer)],
        environment=environment,
        ok=(1,),
    )
    if "newer" not in newer_result.stderr.lower():
        raise RuntimeError("newer catalogue did not fail closed")

    tampered = temporary / "tampered.sqlite"
    shutil.copyfile(migration, tampered)
    execute(tampered, "UPDATE migration_integrity SET sha256 = '" + "f" * 64 + "' WHERE version = 2")
    tampered_result = bounded_run(
        [str(atlas), "init", str(probe_root), "--catalogue", str(tampered)],
        environment=environment,
        ok=(1,),
    )
    if "checksum" not in tampered_result.stderr.lower():
        raise RuntimeError("tampered catalogue did not fail closed")

    readonly = temporary / "readonly.sqlite"
    shutil.copyfile(migration, readonly)
    readonly_uri = f"file:{readonly.as_posix()}?mode=ro"
    ro = sqlite3.connect(readonly_uri, uri=True)
    try:
        try:
            ro.execute("CREATE TABLE forbidden_write(value TEXT)")
        except sqlite3.OperationalError as error:
            if "readonly" not in str(error).lower():
                raise
        else:
            raise RuntimeError("read-only catalogue unexpectedly accepted a write")
    finally:
        ro.close()
    return {
        "old_user_version": 3,
        "migrated_user_version": after_version,
        "old_tables": len(before),
        "backup_restore": backup_restore,
        "newer": "rejected",
        "tampered": "rejected",
        "read_only": "write-rejected",
    }


def table_counts(path: Path, *, exclude_migrations: bool = False) -> dict[str, int]:
    connection = sqlite3.connect(path)
    try:
        names = [row[0] for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )]
        if exclude_migrations:
            names = [name for name in names if name not in {"schema_migration", "migration_integrity"}]
        return {name: connection.execute(f'SELECT COUNT(*) FROM "{name}"').fetchone()[0] for name in names}
    finally:
        connection.close()


def scalar(path: Path, query: str) -> object:
    connection = sqlite3.connect(path)
    try:
        return connection.execute(query).fetchone()[0]
    finally:
        connection.close()


def execute(path: Path, statement: str) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute(statement)
        connection.commit()
    finally:
        connection.close()


def json_command(atlas: Path, args: list[str], environment: dict[str, str]) -> dict:
    result = bounded_run([str(atlas), *args], environment=environment)
    value = json.loads(result.stdout)
    if not isinstance(value, dict) or ("ok" in value and value["ok"] is not True):
        raise RuntimeError(f"Atlas command returned non-success JSON: {args[0]}: {redact(json.dumps(value))[:2000]}")
    return value


def provider_version(command: str, arguments: list[str], environment: dict[str, str]) -> str:
    result = bounded_run([command, *arguments], environment=environment)
    text = (result.stdout or result.stderr).strip().splitlines()
    return redact(text[0]) if text else "unknown"
def provision_typescript(temporary: Path, environment: dict[str, str]) -> dict[str, object]:
    installer = ROOT / "scripts" / "mcp-client-smoke.py"
    prefix = temporary / "verified-typescript"
    result = bounded_run(
        [sys.executable, str(installer), "--provision-typescript", str(prefix)],
        environment=environment, cwd=ROOT, timeout=TIMEOUT,
    )
    provisioned = json.loads(result.stdout)
    command = provisioned.get("command")
    if not isinstance(command, list) or len(command) != 2 or not all(isinstance(v, str) for v in command):
        raise RuntimeError("verified TypeScript provisioner returned an invalid command")
    if not Path(command[0]).is_file() or not Path(command[1]).is_file():
        raise RuntimeError("verified TypeScript provider executable is unavailable")
    return provisioned


def exact_typescript_pin() -> tuple[str, str, str]:
    section = load_matrix()["providers"]["typescript"]
    actual = (section.get("package"), section.get("version"), section.get("npm_integrity"))
    expected = (
        "@sourcegraph/scip-typescript", "0.4.0",
        "sha512-k+AtsrqmS41Sd5qjkZlHcmvoSQIvBOonRj4jpgp0KNFM6aqvMGpdSuPUqrUcg8ENTKjUbfaUVszgQwq3bCOvwA==",
    )
    if actual != expected:
        raise RuntimeError("reviewed scip-typescript pin changed")
    return expected


def provider_config(provider: str, version: str, executable: str,
                    prefix_arguments: list[str]) -> str:
    command = json.dumps(executable)
    if provider == "typescript":
        arguments = json.dumps([*prefix_arguments, "index", "--output", "{output_file}"])
        probe_arguments = json.dumps([*prefix_arguments, "index", "--help"])
        block = f'''name = "scip-typescript"\nversion = "{version}"\nkind = "external_scip"\ntier = "semantic_index"\nscope = "project"\nenabled = true\nrequired = true\npriority = 800\nlanguages = ["typescript", "javascript"]\nproject_markers = ["tsconfig.json"]\ncommand = {command}\narguments = {arguments}\noutput_format = "scip"\nprobe_arguments = {probe_arguments}\ntimeout_ms = 120000\nmax_output_bytes = 536870912\n'''
        allowed_environment = ["PATH", "PATHEXT", "SYSTEMROOT", "TEMP", "TMP", "HOME", "USERPROFILE"]
    else:
        block = f'''name = "rust-analyzer"\nversion = "{version}"\nkind = "external_scip"\ntier = "semantic_index"\nscope = "project"\nenabled = true\nrequired = false\npriority = 800\nlanguages = ["rust"]\nproject_markers = ["Cargo.toml"]\ncommand = {command}\narguments = ["scip", ".", "--output", "{{output_file}}"]\noutput_format = "scip"\nprobe_arguments = ["--version"]\ntimeout_ms = 120000\nmax_output_bytes = 536870912\n'''
        allowed_environment = ["PATH", "PATHEXT", "SYSTEMROOT", "TEMP", "TMP", "HOME", "USERPROFILE", "CARGO_HOME", "CARGO_TARGET_DIR", "RUSTUP_HOME"]
    return f'''schema_version = "1.1.0"\n[workspace]\ndisplay_name = "provider-smoke"\n[provider_runtime]\nallow_shell = false\ninherit_environment = false\nallowed_environment = {json.dumps(allowed_environment)}\ndefault_timeout_ms = 120000\ngraceful_cancel_ms = 1500\nmax_stdout_bytes = 1048576\nmax_stderr_bytes = 1048576\nmax_output_bytes = 536870912\ntemporary_root_policy = "application_private"\nnetwork_isolation_policy = "best_effort_allowed"\nretain_raw_output = false\n[[providers]]\n{block}\n'''




def run_provider(atlas: Path, provider: str, temporary: Path) -> dict[str, object]:
    workspace = temporary / provider
    environment = safe_environment(
        temporary / f"{provider}-process-environment",
        include_rust=provider == "rust",
    )
    workspace.mkdir()
    npm_evidence: dict[str, object] | None = None
    if provider == "typescript":
        (workspace / "package.json").write_text('{"private":true,"name":"atlas-smoke","version":"1.0.0"}\n', encoding="utf-8")
        (workspace / "tsconfig.json").write_text('{"compilerOptions":{"strict":true},"include":["index.ts"]}\n', encoding="utf-8")
        (workspace / "index.ts").write_text("export function atlasSmoke(value: number): number { return value + 1; }\n", encoding="utf-8")
        npm_evidence = provision_typescript(temporary, environment)
        provider_command = npm_evidence["command"]
        executable = provider_command[0]
        prefix_arguments = provider_command[1:]
        version_args = [*prefix_arguments, "--version"]
    else:
        (workspace / "Cargo.toml").write_text('[package]\nname="atlas-smoke"\nversion="0.1.0"\nedition="2021"\n', encoding="utf-8")
        (workspace / "src").mkdir()
        (workspace / "src" / "lib.rs").write_text("pub fn atlas_smoke(value: i64) -> i64 { value + 1 }\n", encoding="utf-8")
        resolved = shutil.which("rust-analyzer")
        if resolved is None:
            return {"provider": provider, "supported": False, "reason": "binary-not-installed"}
        executable = resolved
        prefix_arguments = []
        version_args = ["--version"]
    version = provider_version(executable, version_args, environment)
    if provider == "rust":
        fields = version.split()
        if len(fields) < 2 or fields[0] != "rust-analyzer":
            raise RuntimeError(f"unexpected rust-analyzer version output: {version}")
        version = fields[1]
        if version != "1.94.1":
            raise RuntimeError(f"rust-analyzer version mismatch: {version}")
    if provider == "typescript" and version != exact_typescript_pin()[1]:
        raise RuntimeError(f"scip-typescript version mismatch: {version}")
    config = temporary / f"{provider}.toml"
    config.write_text(
        provider_config(provider, version, executable, prefix_arguments), encoding="utf-8"
    )
    catalogue = temporary / f"{provider}.sqlite"
    json_command(atlas, ["init", str(workspace), "--config", str(config), "--catalogue", str(catalogue)], environment)
    first = json_command(atlas, ["reconcile", str(workspace), "--catalogue", str(catalogue)], environment)
    status = json_command(atlas, ["status", str(workspace), "--catalogue", str(catalogue)], environment)
    doctor = json_command(atlas, ["doctor", str(workspace), "--catalogue", str(catalogue)], environment)
    providers = json_command(atlas, ["providers", str(workspace), "--catalogue", str(catalogue)], environment)
    relative_source = "index.ts" if provider == "typescript" else "src/lib.rs"
    query = "atlasSmoke" if provider == "typescript" else "atlas_smoke"
    found = json_command(atlas, ["find", str(workspace), query, "--catalogue", str(catalogue)], environment)
    source = json_command(atlas, ["source", str(workspace), relative_source, "--catalogue", str(catalogue)], environment)
    compiled = json_command(
        atlas,
        ["context-ir", str(workspace), "review atlas smoke", "--path", relative_source,
         "--catalogue", str(catalogue)],
        environment,
    )
    source_path = workspace / relative_source
    source_path.write_text(source_path.read_text(encoding="utf-8") + "\n", encoding="utf-8")
    second = json_command(atlas, ["reconcile", str(workspace), "--catalogue", str(catalogue)], environment)
    temporal = json_command(atlas, ["temporal", str(workspace), "--catalogue", str(catalogue)], environment)
    serving = json_command(atlas, ["serving-build", str(workspace), "--catalogue", str(catalogue)], environment)
    context_hash = compiled.get("context_ir", {}).get("context_hash")
    provider_rows = providers.get("providers", [])
    provider_status = provider_rows[0].get("last_execution_status") if provider_rows else None
    if doctor.get("integrity_ok") is not True or source.get("status") != "verified":
        raise RuntimeError(f"{provider} lifecycle health/source verification failed")
    if not isinstance(context_hash, str) or len(context_hash) != 64:
        raise RuntimeError(f"{provider} compiler did not emit a canonical context hash")
    if first.get("candidate_generation_id") == second.get("candidate_generation_id"):
        raise RuntimeError(f"{provider} incremental reconcile did not advance generation")
    if provider == "typescript" and (
        provider_status != "complete" or not found.get("results")
    ):
        raise RuntimeError("pinned TypeScript provider did not produce queryable semantic evidence")
    return {
        "provider": provider,
        "supported": provider_status == "complete",
        "version": version,
        "platform": sys.platform,
        "environment_forwarded": sorted(environment),
        "environment_isolation": "harness-owned-scratch",
        "first_generation": first.get("candidate_generation_id"),
        "second_generation": second.get("candidate_generation_id"),
        "schema_version": status.get("schema_version"),
        "doctor_integrity": doctor.get("integrity_ok"),
        "provider_state": provider_rows,
        "query_result_count": len(found.get("results", [])),
        "source_verified": source.get("status") == "verified",
        "compiler_context_hash": context_hash,
        "temporal_from": temporal.get("from_generation_id"),
        "serving_generation": serving.get("serving_generation_id"),
        "npm_supply_chain": {
            "integrity": npm_evidence["root_integrity"],
            "registry": npm_evidence["registry"], "tarball": npm_evidence["tarball"],
        } if npm_evidence else None,
    }


def self_test() -> dict[str, object]:
    matrix = load_matrix()
    assert matrix["fixture"]["path"] == "tests/fixtures/catalogues/v1.4.sqlite"
    assert matrix["clients"]["inspector"]["install_scripts"] is False
    exact_typescript_pin()
    expected_redaction = "<repo>\\x" if os.name == "nt" else "<repo>/x"
    assert redact(str(ROOT / "x")) == expected_redaction
    permission_denial = _self_test_posix_permission_denial()
    with tempfile.TemporaryDirectory(prefix="atlas-provider-self-test-") as raw:
        scratch = Path(raw) / "process-environment"
        environment = safe_environment(scratch)
        assert environment["HOME"] == environment["USERPROFILE"] == str(scratch / "home")
        assert environment["TEMP"] == environment["TMP"] == str(scratch / "temp")
        assert not {"CARGO_HOME", "CARGO_TARGET_DIR", "RUSTUP_HOME"} & environment.keys()
        started = time.monotonic()
        try:
            bounded_run([
                sys.executable, "-c",
                "import os,sys,time\nwhile True:\n os.write(sys.stderr.fileno(), b'x'*65536)\n time.sleep(.001)",
            ], environment=environment, output_limit=4096, timeout=10)
        except BoundedProcessError as error:
            assert "output exceeded" in str(error)
            assert len(error.result.stdout.encode()) <= 4096
            assert len(error.result.stderr.encode()) <= 4096
            assert time.monotonic() - started < 5
        else:
            raise AssertionError("noisy child did not trigger the streaming output cap")
        pid_file = Path(raw) / "delayed-output.pid"
        descendant = (
            "import os,time\n"
            "time.sleep(.25)\n"
            "try:\n os.write(2,b'x'*131072)\n"
            "except OSError:\n pass\n"
            "time.sleep(30)\n"
        )
        parent = (
            "import pathlib,subprocess,sys\n"
            f"p=subprocess.Popen([sys.executable,'-c',{descendant!r}],"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=sys.stderr)\n"
            f"pathlib.Path({str(pid_file)!r}).write_text(str(p.pid))\n"
        )
        try:
            bounded_run(
                [sys.executable, "-c", parent], environment=environment,
                output_limit=4096, timeout=5,
            )
        except BoundedProcessError as error:
            assert "output exceeded" in str(error)
        else:
            raise AssertionError("post-root descendant output did not trigger the cap")
        descendant_pid = int(pid_file.read_text(encoding="utf-8"))
        assert wait_process_stopped(descendant_pid)
    return {
        "self_test": "passed", "fixture": fixture_check(DEFAULT_FIXTURE),
        "noisy_child": "cap-triggered-tree-terminated",
        "delayed_descendant": "post-root-cap-triggered-job-settled",
        "posix_permission_denial": permission_denial,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--atlas", type=Path)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--provider", choices=("typescript", "rust", "all"), default="all")
    args = parser.parse_args()
    if args.self_test:
        print(json.dumps(self_test(), sort_keys=True))
        return 0
    if args.atlas is None or not args.atlas.is_file():
        parser.error("--atlas must name an existing Atlas binary")
    with tempfile.TemporaryDirectory(prefix="atlas-provider-smoke-") as raw:
        temporary = Path(raw)
        catalogue_environment = safe_environment(temporary / "catalogue-process-environment")
        result: dict[str, object] = {
            "fixture": fixture_check(args.fixture),
            "catalogue_failures": catalogue_failure_matrix(
                args.atlas.resolve(), args.fixture.resolve(), temporary, catalogue_environment
            ),
            "providers": [],
        }
        requested = ("typescript", "rust") if args.provider == "all" else (args.provider,)
        result["providers"] = [run_provider(args.atlas.resolve(), provider, temporary) for provider in requested]
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
