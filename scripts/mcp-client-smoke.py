#!/usr/bin/env python3
"""Run pinned real MCP Inspector and credential-free Codex client smokes."""
from __future__ import annotations

import argparse
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import hmac
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import signal
import subprocess
import socket
import struct
import sys
import tarfile
import tempfile
import threading
import time
import tomllib
from typing import Any, BinaryIO
import urllib.request
import urllib.parse
import uuid

if os.name == "nt":
    import ctypes
    from ctypes import wintypes
    import msvcrt

WINDOWS_JOB_SETTLE_SECONDS = 0.5
ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "config" / "mcp-client-matrix.toml"
MAX_OUTPUT = 1_048_576
TIMEOUT = 120
INSTALL_TIMEOUT = 300
SECRET_NAMES = ("API_KEY", "TOKEN", "PASSWORD", "SECRET", "CREDENTIAL", "AUTH")
MAX_DOWNLOAD = 536_870_912
REGISTRY_ORIGIN = "https://registry.npmjs.org"
EXPECTED_PINS = {
    "inspector": ("@modelcontextprotocol/inspector", "2.5.0",
                  "sha512-E7YQiWyuXLVPPbDRO5HpyCXcYnwG8EPHNbqNPn5u+AxDzfNi+6Idw2vRYW9CQplJpkxGkNtquTFXGZ0ZncksIg=="),
    "codex": ("@openai/codex", "0.151.0",
              "sha512-mhtWmOZRdmWD1jPbLDnQb59BsaVP/V+lXe/OFNR9ZcLZU0UCiBwn98Fcav1ss7sDIlHkuqj6nWd44IPeXoOhJA=="),
    "typescript": ("@sourcegraph/scip-typescript", "0.4.0",
                   "sha512-k+AtsrqmS41Sd5qjkZlHcmvoSQIvBOonRj4jpgp0KNFM6aqvMGpdSuPUqrUcg8ENTKjUbfaUVszgQwq3bCOvwA=="),
}
EXPECTED_CODEX_PLATFORMS = {
    "win32-x64": ("@openai/codex-win32-x64", "@openai/codex", "0.151.0-win32-x64",
                  "sha512-sLT7xvID3jhU6tkzcwRPnMEclKRwUPbpo0mtfxIF9KpdZH3VJV7sM2/kXWXyvUM7Zt/YeyOaeATTEysbRz8Yog=="),
    "win32-arm64": ("@openai/codex-win32-arm64", "@openai/codex", "0.151.0-win32-arm64",
                    "sha512-zDWzOoh9wHm+Om1Nhn7os47rAVeSGPh0SnM3YOttdq6iPJz2zn4vBnbGUZjeih1qW/3mvNF3Oyd4owlaHmphmg=="),
    "linux-x64": ("@openai/codex-linux-x64", "@openai/codex", "0.151.0-linux-x64",
                  "sha512-xcVyY1FtwvVYhh2JBmz8fX8CQqFAxO/lxJ2IXsh8x5uwxZVHVl5fZHFHf8JdRaOGG0vpkYmu/DKKVoLd56/DDQ=="),
    "linux-arm64": ("@openai/codex-linux-arm64", "@openai/codex", "0.151.0-linux-arm64",
                    "sha512-CsLgFeX4TQ6I2Gdrxd2r5UbgIbDLCdtcLAlnMYjr06bCL057MTNGec7Ewb3+Z2DBiMuXCljdTBGqLOePkMV0sQ=="),
    "darwin-x64": ("@openai/codex-darwin-x64", "@openai/codex", "0.151.0-darwin-x64",
                   "sha512-0y+g8TVpP+Fn10mjoKYXER6qYjn29w7xBUsbPXJ6Accu/FoM4Qp4WbKXQPmE0G0yUACTQVZRjzTSsdWUezNgkg=="),
    "darwin-arm64": ("@openai/codex-darwin-arm64", "@openai/codex", "0.151.0-darwin-arm64",
                     "sha512-g7YzpaCZGCw19R/gly3vRPjnLqaW7JcBAu2WQQ6e8PIlvBPmS/gMplIUURMgNO6gi8LsPzdlQtLqkwoeOOlIdg=="),
}
REQUIRED_TOOLS = [
    "atlas_status", "atlas_find", "atlas_inspect", "atlas_trace", "atlas_impact",
    "atlas_source", "atlas_context", "atlas_history", "atlas_providers",
    "atlas_context_ir", "atlas_serving_build", "atlas_generation_delta",
    "atlas_temporal", "atlas_governor_capabilities", "atlas_governor_run",
    "atlas_task_show", "atlas_compiled_context_show", "atlas_context_yield_show",
    "atlas_serving_status",
]


def matrix() -> dict[str, Any]:
    with MATRIX_PATH.open("rb") as handle:
        return tomllib.load(handle)


def clean_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP", "LANG"}
    result = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    if extra:
        result.update(extra)
    if any(any(marker in key.upper() for marker in SECRET_NAMES) for key in result):
        raise RuntimeError("credential-like environment variable crossed the client boundary")
    return result


def redact(text: str) -> str:
    for value, replacement in ((str(ROOT), "<repo>"), (str(Path.home()), "<home>")):
        if value:
            text = text.replace(value, replacement)
    return text


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
    _kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    _kernel32.DuplicateHandle.argtypes = (
        wintypes.HANDLE, wintypes.HANDLE, wintypes.HANDLE,
        ctypes.POINTER(wintypes.HANDLE), wintypes.DWORD, wintypes.BOOL, wintypes.DWORD,
    )
    _kernel32.DuplicateHandle.restype = wintypes.BOOL
    _kernel32.GetFileType.argtypes = (wintypes.HANDLE,)
    _kernel32.GetFileType.restype = wintypes.DWORD
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


def run(argv: list[str], *, env: dict[str, str] | None = None, cwd: Path | None = None,
        expected: tuple[int, ...] = (0,), timeout: int = TIMEOUT,
        output_limit: int = MAX_OUTPUT) -> subprocess.CompletedProcess[str]:
    if not argv or any(not isinstance(value, str) or not value or "\0" in value for value in argv):
        raise ValueError("command arguments must be non-empty NUL-free strings")
    if cwd is not None and not cwd.is_dir():
        raise ValueError(f"command working directory does not exist: {cwd}")
    child_env = env if env is not None else clean_environment()
    executable = shutil.which(argv[0], path=child_env.get("PATH"))
    if executable is None:
        raise RuntimeError(f"required executable is unavailable: {argv[0]}")
    command = [executable, *argv[1:]]
    process, owner = ProcessOwner.spawn(
        command, cwd=cwd, env=child_env, stdin=subprocess.DEVNULL,
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
    if result.returncode not in expected:
        detail = redact(result.stderr or result.stdout)
        raise RuntimeError(f"{Path(command[0]).name} exited {result.returncode}: {detail[:3000]}")
    return result


class RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        raise RuntimeError(f"registry redirect rejected: HTTP {code}")


def validate_matrix() -> dict[str, Any]:
    config = matrix()
    sections = {
        "inspector": config["clients"]["inspector"],
        "codex": config["clients"]["codex"],
        "typescript": config["providers"]["typescript"],
    }
    for name, expected in EXPECTED_PINS.items():
        actual = sections[name]
        if (actual.get("package"), actual.get("version"), actual.get("npm_integrity")) != expected:
            raise RuntimeError(f"{name} reviewed package pin changed")
        expected_tarball = (
            f"{REGISTRY_ORIGIN}/{expected[0]}/-/{expected[0].split('/')[-1]}-{expected[1]}.tgz"
        )
        expected_registry = f"{REGISTRY_ORIGIN}/{expected[0]}/{expected[1]}"
        if actual.get("tarball") != expected_tarball or actual.get("registry") != expected_registry:
            raise RuntimeError(f"{name} official registry URL changed")
    platforms = config["clients"]["codex"].get("platforms", {})
    if set(platforms) != set(EXPECTED_CODEX_PLATFORMS):
        raise RuntimeError("Codex supported platform mapping changed")
    for key, expected in EXPECTED_CODEX_PLATFORMS.items():
        actual = platforms[key]
        values = (actual.get("dependency"), actual.get("package"),
                  actual.get("version"), actual.get("npm_integrity"))
        if values != expected or f"{actual.get('os')}-{actual.get('arch')}" != key:
            raise RuntimeError(f"Codex reviewed platform pin changed: {key}")
        suffix = expected[2]
        expected_tarball = f"{REGISTRY_ORIGIN}/@openai/codex/-/codex-{suffix}.tgz"
        expected_registry = f"{REGISTRY_ORIGIN}/@openai/codex/{suffix}"
        if actual.get("tarball") != expected_tarball or actual.get("registry") != expected_registry:
            raise RuntimeError(f"Codex official platform registry URL changed: {key}")
    return config


def verify_tarball(path: Path, integrity: str) -> dict[str, Any]:
    try:
        algorithm, encoded = integrity.split("-", 1)
        expected = base64.b64decode(encoded, validate=True)
    except (ValueError, TypeError) as error:
        raise RuntimeError("invalid configured SRI") from error
    if algorithm != "sha512":
        raise RuntimeError(f"unsupported SRI algorithm: {algorithm}")
    digest = hashlib.sha512()
    with path.open("rb") as handle:
        while chunk := handle.read(1_048_576):
            digest.update(chunk)
    if not hmac.compare_digest(digest.digest(), expected):
        raise RuntimeError(f"tarball SRI mismatch: {path.name}")
    package_json: dict[str, Any] | None = None
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        if len(members) > 100_000:
            raise RuntimeError("npm archive member count exceeds bound")
        for member in members:
            pure = PurePosixPath(member.name)
            if pure.is_absolute() or ".." in pure.parts or member.issym() or member.islnk():
                raise RuntimeError(f"unsafe npm archive member: {member.name}")
            if member.name == "package/package.json":
                if member.size > MAX_OUTPUT:
                    raise RuntimeError("npm package manifest exceeds bound")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise RuntimeError("npm package manifest is unreadable")
                package_json = json.loads(extracted.read(MAX_OUTPUT + 1))
    if not isinstance(package_json, dict):
        raise RuntimeError("npm archive lacks package/package.json")
    return package_json


def download_verified(section: dict[str, Any], destination: Path) -> tuple[Path, dict[str, Any]]:
    url = str(section["tarball"])
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or parsed.netloc != "registry.npmjs.org" or parsed.username:
        raise RuntimeError(f"non-official npm tarball URL rejected: {url}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(url, headers={"Accept": "application/octet-stream"})
    opener = urllib.request.build_opener(RejectRedirects)
    started = time.monotonic()
    total = 0
    with opener.open(request, timeout=30) as response, destination.open("wb") as output:
        if response.status != 200 or response.geturl() != url:
            raise RuntimeError(f"unexpected npm registry response for {url}")
        while chunk := response.read(1_048_576):
            total += len(chunk)
            if total > MAX_DOWNLOAD or time.monotonic() - started > TIMEOUT:
                raise RuntimeError("npm tarball download exceeded byte/time bound")
            output.write(chunk)
    return destination, verify_tarball(destination, str(section["npm_integrity"]))


def npm_environment(root: Path) -> dict[str, str]:
    home = root / "home"
    cache = root / "cache"
    temporary = root / "temp"
    prefix = root / "prefix"
    for path in (home, cache, temporary, prefix):
        path.mkdir(parents=True, exist_ok=True)
    userconfig = root / "user.npmrc"
    globalconfig = root / "global.npmrc"
    userconfig.write_text("", encoding="utf-8")
    globalconfig.write_text("", encoding="utf-8")
    return clean_environment({
        "HOME": str(home), "USERPROFILE": str(home), "TEMP": str(temporary), "TMP": str(temporary),
        "npm_config_registry": REGISTRY_ORIGIN + "/", "npm_config_cache": str(cache),
        "npm_config_prefix": str(prefix), "npm_config_userconfig": str(userconfig),
        "npm_config_globalconfig": str(globalconfig), "npm_config_ignore_scripts": "true",
        "npm_config_audit": "false", "npm_config_fund": "false",
        "npm_config_update_notifier": "false", "npm_config_package_lock": "false",
    })


def install_verified(kind: str, root: Path) -> dict[str, Any]:
    config = validate_matrix()
    section = config["providers"]["typescript"] if kind == "typescript" else config["clients"][kind]
    downloads = root / "downloads"
    root_tarball, manifest = download_verified(section, downloads / f"{kind}-root.tgz")
    expected = EXPECTED_PINS[kind]
    if (manifest.get("name"), manifest.get("version")) != expected[:2]:
        raise RuntimeError(f"{kind} verified archive identity mismatch")
    dependencies = {expected[0]: "file:" + root_tarball.as_posix()}
    selected: dict[str, Any] | None = None
    if kind == "codex":
        machine = platform.machine().lower()
        arch = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64", "arm64": "arm64"}.get(machine)
        key = f"{sys.platform}-{arch}" if arch else ""
        selected = section["platforms"].get(key)
        if selected is None:
            raise RuntimeError(f"unsupported Codex runtime platform: {sys.platform}/{machine}")
        expected_optional = {
            values[0]: f"npm:{values[1]}@{values[2]}"
            for values in EXPECTED_CODEX_PLATFORMS.values()
        }
        if manifest.get("optionalDependencies") != expected_optional:
            raise RuntimeError("Codex root optional dependency mapping changed")
        platform_tarball, platform_manifest = download_verified(
            selected, downloads / f"codex-{key}.tgz"
        )
        if (platform_manifest.get("name"), platform_manifest.get("version"),
                platform_manifest.get("os"), platform_manifest.get("cpu")) != (
                    selected["package"], selected["version"], [selected["os"]], [selected["arch"]]
                ):
            raise RuntimeError("Codex platform archive identity does not match runtime")
        dependencies[selected["dependency"]] = "file:" + platform_tarball.as_posix()
    environment = npm_environment(root)
    prefix = root / "prefix"
    package = {"private": True, "name": "atlas-verified-tools", "version": "1.0.0",
               "dependencies": dependencies}
    (prefix / "package.json").write_text(json.dumps(package, separators=(",", ":")), encoding="utf-8")
    run([
        "npm", "install", "--prefix", str(prefix), "--omit=optional", "--ignore-scripts",
        "--no-audit", "--no-fund", "--no-package-lock", "--no-save",
    ], env=environment, cwd=prefix, timeout=INSTALL_TIMEOUT)
    installed = prefix / "node_modules" / Path(*expected[0].split("/")) / "package.json"
    installed_manifest = json.loads(installed.read_text(encoding="utf-8"))
    if (installed_manifest.get("name"), installed_manifest.get("version")) != expected[:2]:
        raise RuntimeError(f"{kind} installed identity mismatch")
    node = shutil.which("node", path=environment.get("PATH"))
    if node is None:
        raise RuntimeError("Node executable is unavailable")
    bin_value = installed_manifest.get("bin")
    if isinstance(bin_value, dict):
        bin_relative = next(iter(bin_value.values()))
    elif isinstance(bin_value, str):
        bin_relative = bin_value
    else:
        raise RuntimeError(f"{kind} installed package lacks a CLI")
    cli = installed.parent / bin_relative
    if not cli.is_file():
        raise RuntimeError(f"{kind} installed CLI is unavailable")
    return {
        "command": [node, str(cli)], "manifest": installed_manifest,
        "root_integrity": section["npm_integrity"], "platform": selected,
        "registry": section["registry"], "tarball": section["tarball"],
    }
def parse_json_output(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    value = json.loads(result.stdout)
    if not isinstance(value, dict):
        raise RuntimeError("client stdout is not a JSON object")
    return value




def _receive_socket_input(connection: socket.socket, destination: BinaryIO,
                          state: dict[str, object], complete: threading.Event) -> None:
    try:
        while True:
            try:
                chunk = connection.recv(65_536)
            except (ConnectionError, OSError) as error:
                code = getattr(error, "winerror", None) or getattr(error, "errno", None)
                state["failure"] = f"{type(error).__name__}:{code}"
                break
            if chunk == b"":
                if state.get("relay_failure") is None:
                    state["eof"] = True
                    state["eof_at"] = time.monotonic()
                else:
                    state["failure"] = state["relay_failure"]
                break
            try:
                destination.write(chunk)
                destination.flush()
            except (BrokenPipeError, OSError) as error:
                code = getattr(error, "winerror", None) or getattr(error, "errno", None)
                state["failure"] = f"child-stdin-{type(error).__name__}:{code}"
                break
    finally:
        try:
            destination.close()
        except (BrokenPipeError, OSError):
            pass
        complete.set()


def _duplicate_windows_pipe(process_id: int, source_value: int) -> BinaryIO:
    source_process = _kernel32.OpenProcess(0x00000040, False, process_id)
    if not source_process:
        raise ctypes.WinError(ctypes.get_last_error())
    duplicate = wintypes.HANDLE()
    try:
        if not _kernel32.DuplicateHandle(
            source_process, wintypes.HANDLE(source_value), _kernel32.GetCurrentProcess(),
            ctypes.byref(duplicate), 0, False, 0x00000002,
        ):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        _kernel32.CloseHandle(source_process)
    if _kernel32.GetFileType(duplicate) != 0x0003:
        _kernel32.CloseHandle(duplicate)
        raise RuntimeError("lifecycle stdin handle is not a pipe")
    descriptor = msvcrt.open_osfhandle(int(duplicate.value), os.O_RDONLY | os.O_BINARY)
    return os.fdopen(descriptor, "rb", buffering=0)


class LifecycleProxy:
    def __init__(self, atlas_mcp: Path, evidence: Path):
        if not atlas_mcp.is_file() or evidence.exists() or not evidence.parent.is_dir():
            raise RuntimeError("invalid lifecycle proxy path")
        self.atlas_mcp = atlas_mcp
        self.evidence = evidence
        self.token = uuid.uuid4().hex
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(3)
        self.listener.settimeout(20)
        self.port = self.listener.getsockname()[1]
        self.error: BaseException | None = None
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def command(self) -> list[str]:
        return [
            sys.executable, str(Path(__file__).resolve()), "--stdio-proxy",
            "--guardian-port", str(self.port), "--guardian-token", self.token,
        ]

    def join(self) -> None:
        self.thread.join(timeout=15)
        if self.thread.is_alive():
            raise RuntimeError("lifecycle guardian did not finish")
        if self.error is not None:
            raise RuntimeError(f"lifecycle guardian failed: {self.error}") from self.error

    def _accept(self) -> tuple[str, socket.socket, tuple[int, int] | None]:
        connection, address = self.listener.accept()
        if address[0] != "127.0.0.1":
            connection.close()
            raise RuntimeError("non-loopback lifecycle proxy connection rejected")
        connection.settimeout(5)
        handshake = bytearray()
        while not handshake.endswith(b"\n") and len(handshake) <= 256:
            chunk = connection.recv(1)
            if not chunk:
                break
            handshake.extend(chunk)
        connection.settimeout(None)
        if not handshake.endswith(b"\n") or len(handshake) > 256:
            connection.close()
            raise RuntimeError("incomplete or oversized lifecycle proxy handshake")
        fields = handshake.decode("ascii").strip().split()
        try:
            role, token = fields[:2]
        except ValueError as error:
            connection.close()
            raise RuntimeError("invalid lifecycle proxy handshake") from error
        if not hmac.compare_digest(token, self.token) or role not in {"stdin", "stdout", "stderr"}:
            connection.close()
            raise RuntimeError("unauthorized lifecycle proxy connection")
        metadata = None
        if os.name == "nt" and role == "stdin":
            if len(fields) != 4 or not fields[2].isdigit() or not fields[3].isdigit():
                connection.close()
                raise RuntimeError("Windows lifecycle stdin handle metadata is invalid")
            metadata = (int(fields[2]), int(fields[3]))
            if not 0 < metadata[0] <= 0xFFFFFFFF or not 0 < metadata[1] <= (1 << 64) - 1:
                connection.close()
                raise RuntimeError("Windows lifecycle stdin handle metadata is out of range")
        elif len(fields) != 2:
            connection.close()
            raise RuntimeError("unexpected lifecycle proxy handshake metadata")
        return role, connection, metadata

    def _serve(self) -> None:
        child: subprocess.Popen[bytes] | None = None
        owner: ProcessOwner | None = None
        connections: dict[str, socket.socket] = {}
        stdin_metadata: tuple[int, int] | None = None
        started = time.monotonic()
        try:
            for _ in range(3):
                role, connection, metadata = self._accept()
                if role in connections:
                    connection.close()
                    raise RuntimeError("duplicate lifecycle proxy channel")
                connections[role] = connection
                if role == "stdin":
                    stdin_metadata = metadata
            if set(connections) != {"stdin", "stdout", "stderr"}:
                raise RuntimeError("lifecycle proxy channels incomplete")
            child, owner = ProcessOwner.spawn(
                [str(self.atlas_mcp)], stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False,
            )
            input_complete = threading.Event()
            input_state: dict[str, object] = {
                "eof": False, "eof_at": None, "failure": None, "relay_failure": None,
            }
            input_connection = connections["stdin"]
            relay_thread = None
            translated_sockets: tuple[socket.socket, socket.socket] | None = None
            if os.name == "nt":
                if stdin_metadata is None:
                    raise RuntimeError("Windows lifecycle stdin metadata is missing")
                source = _duplicate_windows_pipe(*stdin_metadata)
                translated_sockets = socket.socketpair()

                def relay_windows_stdin() -> None:
                    try:
                        while True:
                            chunk = source.read(65_536)
                            if chunk == b"":
                                translated_sockets[1].shutdown(socket.SHUT_WR)
                                return
                            translated_sockets[1].sendall(chunk)
                    except (ConnectionError, OSError) as error:
                        code = getattr(error, "winerror", None) or getattr(error, "errno", None)
                        input_state["relay_failure"] = f"stdin-pipe-{type(error).__name__}:{code}"
                    finally:
                        source.close()
                        translated_sockets[1].close()

                input_connection = translated_sockets[0]
                relay_thread = threading.Thread(target=relay_windows_stdin, daemon=True)
                relay_thread.start()

            def send_output(source: BinaryIO, connection: socket.socket) -> None:
                try:
                    while chunk := source.read1(65_536):
                        connection.sendall(chunk)
                except (BrokenPipeError, ConnectionError, OSError):
                    pass

            threads = [
                threading.Thread(
                    target=_receive_socket_input,
                    args=(input_connection, child.stdin, input_state, input_complete), daemon=True,
                ),
                threading.Thread(target=send_output, args=(child.stdout, connections["stdout"]), daemon=True),
                threading.Thread(target=send_output, args=(child.stderr, connections["stderr"]), daemon=True),
            ]
            for thread in threads:
                thread.start()
            input_timed_out = not input_complete.wait(timeout=TIMEOUT)
            if input_timed_out:
                input_state["failure"] = "client-input-timeout"
                owner.terminate()
            elif not input_state["eof"]:
                owner.terminate()
            timed_out = False
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                timed_out = True
                terminate_process_tree(child, owner)
            root_exit_at = time.monotonic()
            active_after_root_exit = owner.active_process_count()
            if os.name == "nt" and active_after_root_exit != 0:
                owner.wait_empty(WINDOWS_JOB_SETTLE_SECONDS)
                active_after_root_exit = owner.active_process_count()
            if active_after_root_exit != 0:
                terminate_process_tree(child, owner)
            for thread in threads[1:]:
                thread.join(timeout=2)
            if relay_thread is not None:
                relay_thread.join(timeout=2)
            if translated_sockets is not None:
                translated_sockets[0].close()
            streams_closed = not any(thread.is_alive() for thread in threads[1:])
            active_after_cleanup = owner.active_process_count()
            if active_after_cleanup != 0:
                terminate_process_tree(child, owner)
                active_after_cleanup = owner.active_process_count()
            if active_after_cleanup != 0:
                raise RuntimeError("lifecycle-owned process tree did not settle")
            eof_at = input_state["eof_at"]
            eof_to_exit = (
                round(root_exit_at - float(eof_at), 6)
                if isinstance(eof_at, float) else None
            )
            record = {
                "schema": 1, "phase": "complete", "stdin_eof": input_state["eof"],
                "stdin_channel_failure": input_state["failure"],
                "eof_to_exit_seconds": eof_to_exit,
                "duration_seconds": round(time.monotonic() - started, 6),
                "child_exit_code": child.returncode, "child_exited": child.poll() is not None,
                "timed_out": timed_out, "streams_closed": streams_closed,
                "owned_descendants_remaining": active_after_root_exit != 0,
                "owned_processes_active_after_root_exit": active_after_root_exit,
                "owned_processes_active_after_cleanup": active_after_cleanup,
                "containment": owner.containment,
                "containment_measurement": owner.measurement,
                "kill_on_close": os.name == "nt",
            }
            owner.close()
            temporary = self.evidence.with_suffix(self.evidence.suffix + ".tmp")
            temporary.write_text(json.dumps(record, sort_keys=True, separators=(",", ":")), encoding="utf-8")
            temporary.replace(self.evidence)
        except BaseException as error:
            self.error = error
            if child is not None and owner is not None:
                try:
                    if owner.active_process_count() != 0:
                        terminate_process_tree(child, owner)
                    owner.close()
                except BaseException:
                    pass
        finally:
            for connection in connections.values():
                try:
                    connection.close()
                except OSError:
                    pass
            self.listener.close()


def proxy_stdio(port: int, token: str) -> int:
    if not 1 <= port <= 65535 or len(token) != 32 or not token.isascii():
        raise RuntimeError("invalid lifecycle guardian endpoint")
    inputs = socket.create_connection(("127.0.0.1", port), timeout=10)
    outputs = socket.create_connection(("127.0.0.1", port), timeout=10)
    errors = socket.create_connection(("127.0.0.1", port), timeout=10)
    if os.name == "nt":
        stdin_handle = msvcrt.get_osfhandle(sys.stdin.fileno())
        inputs.sendall(f"stdin {token} {os.getpid()} {stdin_handle}\n".encode("ascii"))
    else:
        inputs.sendall(f"stdin {token}\n".encode("ascii"))
    outputs.sendall(f"stdout {token}\n".encode("ascii"))
    errors.sendall(f"stderr {token}\n".encode("ascii"))
    inputs.settimeout(None)
    outputs.settimeout(None)
    errors.settimeout(None)

    def receive_output(connection: socket.socket, destination: BinaryIO) -> None:
        try:
            while chunk := connection.recv(65_536):
                destination.write(chunk)
                destination.flush()
        except (BrokenPipeError, ConnectionError, OSError):
            pass

    threads = [
        threading.Thread(target=receive_output, args=(outputs, sys.stdout.buffer), daemon=True),
        threading.Thread(target=receive_output, args=(errors, sys.stderr.buffer), daemon=True),
    ]
    for thread in threads:
        thread.start()
    input_closed = os.name == "nt"
    if os.name != "nt":
        try:
            while chunk := sys.stdin.buffer.read1(65_536):
                inputs.sendall(chunk)
            input_closed = True
        except (BrokenPipeError, ConnectionError, OSError):
            pass
        finally:
            try:
                inputs.shutdown(socket.SHUT_WR)
            except OSError:
                input_closed = False
    for thread in threads:
        thread.join()
    inputs.close()
    outputs.close()
    errors.close()
    return 0 if input_closed else 1




def validate_lifecycle(evidence: Path) -> dict[str, Any]:
    last_candidate: dict[str, Any] | None = None
    deadline = time.monotonic() + 12
    record: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        if evidence.is_file() and evidence.stat().st_size <= 4096:
            candidate = json.loads(evidence.read_text(encoding="utf-8"))
            last_candidate = candidate
            if candidate.get("phase") == "complete":
                record = candidate
                break
        time.sleep(0.02)
    if record is None:
        raise RuntimeError(
            f"stdio proxy lifecycle evidence is missing, incomplete, or oversized: {last_candidate}"
        )
    required = {
        "schema": 1, "phase": "complete", "stdin_eof": True,
        "stdin_channel_failure": None,
        "child_exit_code": 0, "child_exited": True,
        "timed_out": False, "streams_closed": True, "owned_descendants_remaining": False,
        "owned_processes_active_after_root_exit": 0,
        "owned_processes_active_after_cleanup": 0,
        "containment": "windows-job-object" if os.name == "nt" else "posix-process-group",
        "containment_measurement": (
            "QueryInformationJobObject(JobObjectBasicAccountingInformation).ActiveProcesses"
            if os.name == "nt" else "killpg(pgid, 0) existence probe"
        ),
        "kill_on_close": os.name == "nt",
    }
    if any(record.get(key) != value for key, value in required.items()):
        raise RuntimeError(f"Atlas MCP lifecycle was not clean: {record}")
    for key in ("owned_processes_active_after_root_exit", "owned_processes_active_after_cleanup"):
        if isinstance(record.get(key), bool) or not isinstance(record.get(key), int):
            raise RuntimeError(f"Atlas MCP lifecycle process count was not measured: {record}")
    if not 0 <= record.get("eof_to_exit_seconds", -1) <= 10:
        raise RuntimeError(f"Atlas MCP EOF exit duration was invalid: {record}")
    return record

def inspector_command(installed: list[str], proxy: list[str], *options: str) -> list[str]:
    return [*installed, "--cli", proxy[0], *proxy[1:], "--", *options]


def run_inspector(atlas_mcp: Path, workspace: Path, catalogue: Path,
                  temporary: Path) -> dict[str, Any]:
    installation = install_verified("inspector", temporary / "npm-inspector")
    installed = installation["command"]
    environment = clean_environment({
        "HOME": str(temporary / "client-home"), "USERPROFILE": str(temporary / "client-home"),
        "TEMP": str(temporary), "TMP": str(temporary), "MCP_AUTO_OPEN_ENABLED": "false",
        "MCP_STORAGE_DIR": str(temporary / "inspector-storage"),
    })
    Path(environment["HOME"]).mkdir()
    version_surface = run([*installed, "--version"], env=environment, expected=(0, 1, 2, 5))
    version_text = (version_surface.stdout + version_surface.stderr).strip()
    expected_version = installation["manifest"]["version"]
    if expected_version in version_text:
        observed_version = version_text
        version_evidence = "executed-cli-version"
    else:
        help_surface = run([*installed, "--help"], env=environment)
        if not (help_surface.stdout or help_surface.stderr).strip():
            raise RuntimeError("Inspector executed help surface was empty")
        observed_version = expected_version
        version_evidence = "executed-cli-help-plus-verified-installed-package-json"
    lifecycle_root = temporary / "lifecycle"
    lifecycle_root.mkdir()
    lifecycle_records: dict[str, Any] = {}

    def invoke(label: str, options: list[str], expected: tuple[int, ...] = (0,)) -> subprocess.CompletedProcess[str]:
        evidence = lifecycle_root / f"inspector-{label}-{uuid.uuid4().hex}.json"
        guardian = LifecycleProxy(atlas_mcp, evidence)
        try:
            result = run(
                inspector_command(installed, guardian.command(), *options),
                env=environment, expected=expected,
            )
        finally:
            guardian.join()
        lifecycle_records[label] = validate_lifecycle(evidence)
        return result

    initialize = parse_json_output(invoke("initialize", ["--method", "initialize", "--format", "json"]))
    listed = parse_json_output(invoke(
        "list", ["--method", "tools/list", "--strict", "--format", "json"]
    ))
    tools = listed.get("result", {}).get("tools", [])
    names = [tool.get("name") for tool in tools if isinstance(tool, dict)]
    if names != REQUIRED_TOOLS:
        raise RuntimeError(f"Inspector discovered unexpected tool registry: {names}")
    arguments = json.dumps({"workspace_root": str(workspace), "catalogue": str(catalogue)}, separators=(",", ":"))
    valid = parse_json_output(invoke(
        "valid", ["--method", "tools/call", "--tool-name", "atlas_status",
                  "--tool-args-json", arguments, "--format", "json"]
    ))
    if '"ok":true' not in json.dumps(valid).replace(" ", ""):
        raise RuntimeError("Inspector representative atlas_status call did not return ok=true")
    malformed_args = json.dumps({
        "workspace_root": str(workspace), "catalogue": str(catalogue), "unknown": True
    })
    malformed = invoke(
        "malformed", ["--method", "tools/call", "--tool-name", "atlas_status",
                      "--tool-args-json", malformed_args, "--format", "json"], (1, 5)
    )
    if "unknown" not in (malformed.stdout + malformed.stderr).lower():
        raise RuntimeError("Inspector malformed-argument call did not expose a closed-schema rejection")
    unknown = invoke(
        "unknown", ["--method", "tools/call", "--tool-name", "atlas_not_a_tool",
                    "--tool-args-json", "{}", "--format", "json"], (1, 5)
    )
    if "not" not in (unknown.stdout + unknown.stderr).lower():
        raise RuntimeError("Inspector unknown-tool call did not expose rejection")
    return {
        "version": observed_version, "version_evidence": version_evidence,
        "configured_version": expected_version,
        "protocol_version": initialize.get("result", {}).get("protocolVersion"),
        "tool_count": len(names), "ordered_tools": names, "valid_call": "ok",
        "malformed_arguments": "rejected", "unknown_tool": "rejected",
        "clean_eof": lifecycle_records, "install_scripts": "disabled",
        "root_integrity": installation["root_integrity"],
        "registry": installation["registry"], "tarball": installation["tarball"],
    }


class FakeResponses:
    def __init__(self, case: str, workspace: Path, catalogue: Path):
        self.case = case
        self.workspace = workspace
        self.catalogue = catalogue
        self.requests: list[dict[str, Any]] = []
        self.authorization_seen = False
        self.lock = threading.Lock()

    def handler(self):
        state = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, _format: str, *_args: object) -> None:
                return

            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("Content-Length", "0"))
                if length > MAX_OUTPUT:
                    self.send_error(413)
                    return
                raw = self.rfile.read(length)
                try:
                    request = json.loads(raw)
                except json.JSONDecodeError:
                    self.send_error(400)
                    return
                with state.lock:
                    if self.headers.get("Authorization"):
                        state.authorization_seen = True
                    state.requests.append(request)
                    number = len(state.requests)
                events = state.events(request, number)
                payload = "".join(f"event: {event['type']}\ndata: {json.dumps(event, separators=(',', ':'))}\n\n" for event in events)
                payload += "data: [DONE]\n\n"
                encoded = payload.encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(encoded)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(encoded)

        return Handler

    def events(self, request: dict[str, Any], number: int) -> list[dict[str, Any]]:
        if number == 1:
            tools = request.get("tools")
            if not isinstance(tools, list):
                raise RuntimeError("Codex request omitted tools")
            namespaces = [tool for tool in tools if isinstance(tool, dict) and tool.get("name") == "mcp__atlas"]
            nested = namespaces[0].get("tools", []) if len(namespaces) == 1 else []
            names = [tool.get("name") for tool in nested if isinstance(tool, dict)]
            status_name = "atlas_status" if "atlas_status" in names else None
            if status_name is None or sorted(names) != sorted(REQUIRED_TOOLS):
                raise RuntimeError(f"Codex did not discover the exact Atlas registry: {names}")
            if self.case == "unknown":
                status_name = "atlas_not_a_tool"
                arguments = "{}"
            elif self.case == "malformed":
                arguments = json.dumps({"workspace_root": str(self.workspace), "catalogue": str(self.catalogue), "unknown": True})
            else:
                arguments = json.dumps({"workspace_root": str(self.workspace), "catalogue": str(self.catalogue)})
            item = {"id": "fc_atlas_smoke", "type": "function_call", "status": "completed",
                    "arguments": arguments, "call_id": "call_atlas_smoke", "name": status_name,
                    "namespace": "mcp__atlas"}
            return function_events(item)
        return message_events("ATLAS_SMOKE_OK")


def base_response(response_id: str, status: str, output: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "id": response_id, "object": "response", "created_at": 1, "status": status,
        "background": False, "completed_at": 1 if status == "completed" else None,
        "error": None, "incomplete_details": None, "instructions": None,
        "max_output_tokens": None, "model": "atlas-smoke", "output": output,
        "parallel_tool_calls": False, "previous_response_id": None,
        "reasoning": {"effort": None, "summary": None}, "service_tier": "default",
        "store": False, "temperature": 0.0, "text": {"format": {"type": "text"}},
        "tool_choice": "auto", "tools": [], "top_p": 1.0, "truncation": "disabled",
        "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0},
                  "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2},
    }


def function_events(item: dict[str, Any]) -> list[dict[str, Any]]:
    rid = "resp_atlas_tool"
    pending = dict(item, status="in_progress", arguments="")
    return [
        {"type": "response.created", "sequence_number": 0, "response": base_response(rid, "in_progress", [])},
        {"type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": pending},
        {"type": "response.function_call_arguments.done", "sequence_number": 2, "response_id": rid,
         "item_id": item["id"], "output_index": 0, "arguments": item["arguments"]},
        {"type": "response.output_item.done", "sequence_number": 3, "output_index": 0, "item": item},
        {"type": "response.completed", "sequence_number": 4, "response": base_response(rid, "completed", [item])},
    ]


def message_events(text: str) -> list[dict[str, Any]]:
    rid = "resp_atlas_final"
    pending = {"id": "msg_atlas_smoke", "type": "message", "status": "in_progress", "role": "assistant", "content": []}
    part = {"type": "output_text", "annotations": [], "logprobs": [], "text": text}
    complete = dict(pending, status="completed", content=[part])
    return [
        {"type": "response.created", "sequence_number": 0, "response": base_response(rid, "in_progress", [])},
        {"type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": pending},
        {"type": "response.content_part.added", "sequence_number": 2, "response_id": rid,
         "item_id": pending["id"], "output_index": 0, "content_index": 0, "part": dict(part, text="")},
        {"type": "response.output_text.delta", "sequence_number": 3, "response_id": rid,
         "item_id": pending["id"], "output_index": 0, "content_index": 0, "delta": text, "logprobs": []},
        {"type": "response.output_text.done", "sequence_number": 4, "response_id": rid,
         "item_id": pending["id"], "output_index": 0, "content_index": 0, "text": text, "logprobs": []},
        {"type": "response.content_part.done", "sequence_number": 5, "response_id": rid,
         "item_id": pending["id"], "output_index": 0, "content_index": 0, "part": part},
        {"type": "response.output_item.done", "sequence_number": 6, "output_index": 0, "item": complete},
        {"type": "response.completed", "sequence_number": 7, "response": base_response(rid, "completed", [complete])},
    ]


def codex_config(port: int, proxy: list[str]) -> str:
    command = json.dumps(proxy[0])
    arguments = json.dumps(proxy[1:])
    return f'''model = "atlas-smoke"\nmodel_provider = "fixture"\napproval_policy = "never"\nsandbox_mode = "read-only"\ncheck_for_update_on_startup = false\nfeedback.enabled = false\nanalytics.enabled = false\n\n[model_providers.fixture]\nname = "Local deterministic fixture"\nbase_url = "http://127.0.0.1:{port}/v1"\nrequires_openai_auth = false\nwire_api = "responses"\nrequest_max_retries = 0\nstream_max_retries = 0\nstream_idle_timeout_ms = 10000\n\n[mcp_servers.atlas]\ncommand = {command}\nargs = {arguments}\nstartup_timeout_sec = 10\ntool_timeout_sec = 15\nenabled = true\nrequired = true\ndefault_tools_approval_mode = "prompt"\n\n[mcp_servers.atlas.tools.atlas_status]\napproval_mode = "approve"\n'''


def run_codex_case(codex: list[str], atlas_mcp: Path, workspace: Path, catalogue: Path,
                   temporary: Path, case: str) -> dict[str, Any]:
    state = FakeResponses(case, workspace, catalogue)
    server = ThreadingHTTPServer(("127.0.0.1", 0), state.handler())
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    home = temporary / f"codex-home-{case}"
    home.mkdir()
    lifecycle = temporary / "lifecycle" / f"codex-{case}-{uuid.uuid4().hex}.json"
    lifecycle.parent.mkdir(exist_ok=True)
    guardian = LifecycleProxy(atlas_mcp, lifecycle)
    proxy = guardian.command()
    (home / "config.toml").write_text(codex_config(server.server_port, proxy), encoding="utf-8")
    environment = clean_environment({
        "CODEX_HOME": str(home), "HOME": str(home), "USERPROFILE": str(home),
        "TEMP": str(temporary), "TMP": str(temporary), "NO_PROXY": "127.0.0.1,localhost",
    })
    try:
        result = run([
            *codex, "exec", "--json", "--ephemeral", "--skip-git-repo-check",
            "--ignore-rules", "--strict-config", "--sandbox", "read-only", "-C", str(workspace),
            "Use the Atlas MCP server. Call atlas_status exactly once, then answer ATLAS_SMOKE_OK.",
        ], env=environment, cwd=workspace, expected=(0,) if case != "unknown" else (0, 1), timeout=TIMEOUT)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        guardian.join()
    lifecycle_record = validate_lifecycle(lifecycle)
    if state.authorization_seen:
        raise RuntimeError("Codex sent an Authorization header to the credential-free fake provider")
    combined = result.stdout + result.stderr
    if len(state.requests) < 2:
        raise RuntimeError(f"Codex {case} case did not return tool output to the fake model")
    second_input = state.requests[1].get("input", [])
    tool_outputs = [
        item for item in second_input
        if isinstance(item, dict) and str(item.get("type", "")).endswith("call_output")
    ] if isinstance(second_input, list) else []
    returned_tool_output = json.dumps(tool_outputs, separators=(",", ":")).lower()
    if case == "valid":
        if "atlas_status" not in combined or "atlas_smoke_ok" not in combined.lower():
            raise RuntimeError("Codex valid case lacked observable MCP tool-call/final evidence")
        normalized_tool_output = returned_tool_output.replace(" ", "")
        if "\\\"ok\\\":true" not in normalized_tool_output and '"ok":true' not in normalized_tool_output:
            raise RuntimeError(f"Codex valid case did not return Atlas ok=true to the model: {redact(returned_tool_output)[:4000]}")
        observation = "real-discovery-valid-call"
    else:
        expected_words = ("unknown field", "invalid", "not found", "unknown tool", "unsupported call")
        if not any(word in returned_tool_output for word in expected_words):
            raise RuntimeError(f"Codex {case} case lacked an observable rejection in tool output: {redact(returned_tool_output)[:3000]}")
        observation = "real-client-returned-rejection-to-model"
    return {
        "case": case, "exit": result.returncode, "responses_requests": len(state.requests),
        "authorization_header": False, "observed": observation, "clean_eof": lifecycle_record,
    }


def run_codex(atlas_mcp: Path, workspace: Path, catalogue: Path,
              temporary: Path) -> dict[str, Any]:
    installation = install_verified("codex", temporary / "npm-codex")
    codex = installation["command"]
    version = run([*codex, "--version"], env=clean_environment()).stdout.strip()
    expected = installation["manifest"]["version"]
    if expected not in version:
        raise RuntimeError(f"Codex version mismatch: expected {expected}, observed {version}")
    cases = [run_codex_case(codex, atlas_mcp, workspace, catalogue, temporary, case)
             for case in ("valid", "malformed", "unknown")]
    selected = installation["platform"]
    return {
        "version": version, "configured_version": expected,
        "credential_mode": "isolated-empty-home",
        "provider": {"requires_openai_auth": False, "wire_api": "responses"},
        "root_integrity": installation["root_integrity"],
        "platform": {
            "os": selected["os"], "arch": selected["arch"],
            "dependency": selected["dependency"], "package": selected["package"],
            "version": selected["version"], "npm_integrity": selected["npm_integrity"],
            "registry": selected["registry"], "tarball": selected["tarball"],
        },
        "cases": cases,
    }


def initialize_workspace(atlas: Path, temporary: Path) -> tuple[Path, Path]:
    workspace = temporary / "workspace"
    workspace.mkdir()
    (workspace / "README.txt").write_text("deterministic Atlas MCP client smoke\n", encoding="utf-8")
    catalogue = temporary / "catalogue.sqlite"
    init = run([str(atlas), "init", str(workspace), "--catalogue", str(catalogue)])
    reconcile = run([str(atlas), "reconcile", str(workspace), "--catalogue", str(catalogue)])
    for output in (init.stdout, reconcile.stdout):
        if json.loads(output).get("ok") is not True:
            raise RuntimeError("Atlas fixture initialization failed")
    return workspace, catalogue


def self_test() -> dict[str, Any]:
    config = validate_matrix()
    assert config["modern_protocol"] == "2026-07-28"
    assert config["legacy_protocol"] == "2025-11-25"
    assert config["required_tool_count"] == len(REQUIRED_TOOLS)
    assert config["clients"]["codex"]["requires_openai_auth"] is False
    generated_config = codex_config(1, ["python", "proxy.py", "--stdio-proxy"])
    assert 'default_tools_approval_mode = "prompt"' in generated_config
    assert '[mcp_servers.atlas.tools.atlas_status]\napproval_mode = "approve"' in generated_config
    assert "default_tools_approval_mode = " + json.dumps("approve") not in generated_config
    environment = clean_environment({"CODEX_HOME": "isolated"})
    assert not any(any(marker in key.upper() for marker in SECRET_NAMES) for key in environment)
    permission_denial = _self_test_posix_permission_denial()
    with tempfile.TemporaryDirectory(prefix="atlas-client-containment-self-test-") as raw:
        temporary = Path(raw)
        started = time.monotonic()
        try:
            run([
                sys.executable, "-c",
                "import os,sys,time\nwhile True:\n os.write(sys.stdout.fileno(), b'x'*65536)\n time.sleep(.001)",
            ], output_limit=4096, timeout=10)
        except BoundedProcessError as error:
            assert "output exceeded" in str(error)
            assert len(error.result.stdout.encode()) <= 4096
            assert len(error.result.stderr.encode()) <= 4096
            assert time.monotonic() - started < 5
        else:
            raise AssertionError("noisy child did not trigger the streaming output cap")

        pid_file = temporary / "delayed-output.pid"
        descendant = (
            "import os,time\n"
            "time.sleep(.25)\n"
            "try:\n os.write(1,b'x'*131072)\n"
            "except OSError:\n pass\n"
            "time.sleep(30)\n"
        )
        parent = (
            "import pathlib,subprocess,sys\n"
            f"p=subprocess.Popen([sys.executable,'-c',{descendant!r}],"
            "stdin=subprocess.DEVNULL,stdout=sys.stdout,stderr=subprocess.DEVNULL)\n"
            f"pathlib.Path({str(pid_file)!r}).write_text(str(p.pid))\n"
        )
        try:
            run([sys.executable, "-c", parent], output_limit=4096, timeout=5)
        except BoundedProcessError as error:
            assert "output exceeded" in str(error)
        else:
            raise AssertionError("post-root descendant output did not trigger the cap")
        descendant_pid = int(pid_file.read_text(encoding="utf-8"))
        assert wait_process_stopped(descendant_pid)

        def connect(guardian: LifecycleProxy) -> tuple[
            socket.socket, socket.socket, socket.socket, int | None, int | None,
        ]:
            inputs = socket.create_connection(("127.0.0.1", guardian.port), timeout=5)
            outputs = socket.create_connection(("127.0.0.1", guardian.port), timeout=5)
            errors = socket.create_connection(("127.0.0.1", guardian.port), timeout=5)
            read_fd = write_fd = None
            if os.name == "nt":
                read_fd, write_fd = os.pipe()
                handle = msvcrt.get_osfhandle(read_fd)
                inputs.sendall(
                    f"stdin {guardian.token} {os.getpid()} {handle}\n".encode("ascii")
                )
            else:
                inputs.sendall(f"stdin {guardian.token}\n".encode("ascii"))
            outputs.sendall(f"stdout {guardian.token}\n".encode("ascii"))
            errors.sendall(f"stderr {guardian.token}\n".encode("ascii"))
            return inputs, outputs, errors, read_fd, write_fd

        def send_input(inputs: socket.socket, write_fd: int | None, content: bytes) -> None:
            if write_fd is not None:
                os.write(write_fd, content)
                os.close(write_fd)
            else:
                inputs.sendall(content)
                inputs.shutdown(socket.SHUT_WR)

        reset_listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        reset_listener.bind(("127.0.0.1", 0))
        reset_listener.listen(1)
        reset_sender = socket.create_connection(reset_listener.getsockname(), timeout=5)
        reset_receiver, _ = reset_listener.accept()
        reset_state: dict[str, object] = {
            "eof": False, "eof_at": None, "failure": None, "relay_failure": None,
        }
        reset_complete = threading.Event()
        reset_destination = tempfile.TemporaryFile()
        reset_thread = threading.Thread(
            target=_receive_socket_input,
            args=(reset_receiver, reset_destination, reset_state, reset_complete), daemon=True,
        )
        reset_thread.start()
        reset_sender.sendall(b"reset is not EOF")
        linger = struct.pack("hh", 1, 0) if os.name == "nt" else struct.pack("ii", 1, 0)
        reset_sender.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, linger)
        reset_sender.close()
        assert reset_complete.wait(timeout=5)
        reset_thread.join(timeout=1)
        reset_receiver.close()
        reset_listener.close()
        assert reset_state["eof"] is False
        assert reset_state["failure"]

        descendant_evidence = temporary / "descendant.json"
        lifecycle_pid_file = temporary / "lifecycle-descendant.pid"
        descendant_guardian = LifecycleProxy(Path(sys.executable), descendant_evidence)
        (descendant_main, descendant_outputs, descendant_errors,
         descendant_read_fd, descendant_write_fd) = connect(descendant_guardian)
        child_flags = (
            ",creationflags=subprocess.CREATE_NEW_PROCESS_GROUP|subprocess.DETACHED_PROCESS"
            if os.name == "nt" else ""
        )
        source = (
            "import pathlib,subprocess,sys\n"
            "p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'],"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL"
            f"{child_flags})\n"
            f"pathlib.Path({str(lifecycle_pid_file)!r}).write_text(str(p.pid))\n"
        )
        send_input(descendant_main, descendant_write_fd, source.encode("utf-8"))
        descendant_guardian.join()
        descendant_main.close()
        descendant_outputs.close()
        descendant_errors.close()
        if descendant_read_fd is not None:
            os.close(descendant_read_fd)
        lifecycle_pid = int(lifecycle_pid_file.read_text(encoding="utf-8"))
        descendant_record = json.loads(descendant_evidence.read_text(encoding="utf-8"))
        assert descendant_record["owned_processes_active_after_root_exit"] >= 1
        assert descendant_record["owned_processes_active_after_cleanup"] == 0
        assert wait_process_stopped(lifecycle_pid)
        try:
            validate_lifecycle(descendant_evidence)
        except RuntimeError:
            pass
        else:
            raise AssertionError("owned lifecycle descendant was accepted")
        if os.name == "nt":
            natural_evidence = temporary / "natural-descendant.json"
            natural_marker = temporary / "natural-descendant-complete.txt"
            natural_guardian = LifecycleProxy(Path(sys.executable), natural_evidence)
            (natural_main, natural_outputs, natural_errors,
             natural_read_fd, natural_write_fd) = connect(natural_guardian)
            natural_child = (
                "import pathlib,time;"
                "time.sleep(.2);"
                f"pathlib.Path({str(natural_marker)!r}).write_text('settled')"
            )
            natural_source = (
                "import subprocess,sys\n"
                f"subprocess.Popen([sys.executable,'-c',{natural_child!r}],"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL"
                f"{child_flags})\n"
            )
            send_input(natural_main, natural_write_fd, natural_source.encode("utf-8"))
            natural_guardian.join()
            natural_main.close()
            natural_outputs.close()
            natural_errors.close()
            if natural_read_fd is not None:
                os.close(natural_read_fd)
            natural_record = validate_lifecycle(natural_evidence)
            assert natural_marker.read_text(encoding="utf-8") == "settled"
            assert natural_record["owned_processes_active_after_root_exit"] == 0

        clean_evidence = temporary / "clean.json"
        clean_guardian = LifecycleProxy(Path(sys.executable), clean_evidence)
        clean_main, clean_outputs, clean_errors, clean_read_fd, clean_write_fd = connect(clean_guardian)
        send_input(clean_main, clean_write_fd, b"pass\n")
        clean_guardian.join()
        clean_main.close()
        clean_outputs.close()
        clean_errors.close()
        if clean_read_fd is not None:
            os.close(clean_read_fd)
        clean_record = validate_lifecycle(clean_evidence)
        assert clean_record["stdin_eof"] is True
        assert clean_record["stdin_channel_failure"] is None
        assert clean_record["child_exit_code"] == 0
        assert clean_record["streams_closed"] is True
        assert 0 <= clean_record["eof_to_exit_seconds"] <= 10
        assert clean_record["owned_processes_active_after_root_exit"] == 0
    return {
        "self_test": "passed", "tool_count": len(REQUIRED_TOOLS),
        "integrity_pins": len(EXPECTED_PINS) + len(EXPECTED_CODEX_PLATFORMS),
        "noisy_child": "cap-triggered-tree-terminated",
        "delayed_descendant": "post-root-cap-triggered-job-settled",
        "lifecycle_reset": "recorded-as-channel-failure",
        "lifecycle_descendant": "measured-rejected-and-terminated",
        "lifecycle_clean": "actual-eof-exit-streams-and-zero-owned-processes",
        "posix_permission_denial": permission_denial,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--stdio-proxy", action="store_true")
    parser.add_argument("--guardian-port", type=int)
    parser.add_argument("--guardian-token")
    parser.add_argument("--provision-typescript", type=Path)
    parser.add_argument("--client", choices=("inspector", "codex", "all"), default="all")
    parser.add_argument("--atlas", type=Path)
    parser.add_argument("--atlas-mcp", type=Path)
    args = parser.parse_args()
    if args.stdio_proxy:
        if args.guardian_port is None or args.guardian_token is None:
            parser.error("--stdio-proxy requires --guardian-port and --guardian-token")
        return proxy_stdio(args.guardian_port, args.guardian_token)
    if args.provision_typescript is not None:
        installation = install_verified("typescript", args.provision_typescript.resolve())
        print(json.dumps({
            "command": installation["command"],
            "version": installation["manifest"]["version"],
            "root_integrity": installation["root_integrity"],
            "registry": installation["registry"], "tarball": installation["tarball"],
        }, sort_keys=True, separators=(",", ":")))
        return 0
    if args.self_test:
        print(json.dumps(self_test(), sort_keys=True))
        return 0
    if args.atlas is None or not args.atlas.is_file() or args.atlas_mcp is None or not args.atlas_mcp.is_file():
        parser.error("--atlas and --atlas-mcp must name existing binaries")
    with tempfile.TemporaryDirectory(prefix="atlas-mcp-client-smoke-") as raw:
        temporary = Path(raw)
        workspace, catalogue = initialize_workspace(args.atlas.resolve(), temporary)
        output: dict[str, Any] = {
            "matrix": str(MATRIX_PATH.relative_to(ROOT)).replace("\\", "/"),
            "manual_boundary": ["claude_desktop", "claude_code"],
        }
        if args.client in {"inspector", "all"}:
            output["inspector"] = run_inspector(
                args.atlas_mcp.resolve(), workspace, catalogue, temporary
            )
        if args.client in {"codex", "all"}:
            output["codex"] = run_codex(
                args.atlas_mcp.resolve(), workspace, catalogue, temporary
            )
        print(json.dumps(output, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
