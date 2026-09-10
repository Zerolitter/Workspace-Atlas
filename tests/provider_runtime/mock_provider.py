#!/usr/bin/env python3
"""Mock external provider for Workspace Atlas runtime tests.

This is a runtime-boundary fixture, not semantic evidence.
"""

from __future__ import annotations
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import time

def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--request", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument(
        "--mode",
        choices=[
            "success",
            "partial",
            "timeout",
            "invalid",
            "oversized",
            "failure",
            "stderr",
            "descendant-launcher",
            "descendant-child",
        ],
        default="success",
    )
    p.add_argument("--sleep-seconds", type=float, default=30.0)
    p.add_argument("--pid-file", type=Path)
    args = p.parse_args()

    req = json.loads(args.request.read_text(encoding="utf-8"))
    if args.mode == "descendant-child":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.GetCurrentProcess.restype = ctypes.c_void_p
        kernel32.IsProcessInJob.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_int),
        ]
        kernel32.IsProcessInJob.restype = ctypes.c_int
        in_job = ctypes.c_int()
        if not kernel32.IsProcessInJob(
            kernel32.GetCurrentProcess(), None, ctypes.byref(in_job)
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        args.pid_file.write_text(
            json.dumps({"pid": os.getpid(), "in_job": bool(in_job.value)}),
            encoding="utf-8",
        )
        time.sleep(args.sleep_seconds)
        return 0

    if args.mode == "descendant-launcher":
        child_pid_file = args.pid_file.with_suffix(".child.json")
        child = subprocess.Popen(
            [
                sys.executable,
                __file__,
                "--request",
                str(args.request),
                "--output",
                str(args.output),
                "--mode",
                "descendant-child",
                "--sleep-seconds",
                str(args.sleep_seconds),
                "--pid-file",
                str(child_pid_file),
            ]
        )
        marker_deadline = time.monotonic() + 5.0
        while not child_pid_file.exists():
            if time.monotonic() >= marker_deadline:
                raise RuntimeError("descendant did not publish its containment marker")
            time.sleep(0.01)
        descendant = json.loads(child_pid_file.read_text(encoding="utf-8"))
        args.pid_file.write_text(
            json.dumps(
                {
                    "launcher_pid": os.getpid(),
                    "descendant_pid": descendant["pid"],
                    "descendant_in_job": descendant["in_job"],
                }
            ),
            encoding="utf-8",
        )
        time.sleep(args.sleep_seconds)
        return 0

    if args.mode == "timeout":
        time.sleep(args.sleep_seconds)
        return 0
    if args.mode == "failure":
        print("mock provider failed", file=sys.stderr)
        return 7
    if args.mode == "stderr":
        print("bounded diagnostic: token=[REDACT_ME]", file=sys.stderr)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.mode == "invalid":
        args.output.write_bytes(b"not-json-or-scip")
        return 0
    if args.mode == "oversized":
        with args.output.open("wb") as f:
            f.write(b"x" * (16 * 1024 * 1024))
        return 0

    payload = {
        "mock_schema_version": "1.0.0",
        "execution_id": req["execution_id"],
        "status": "partial" if args.mode == "partial" else "complete",
        "documents": [
            {
                "path": "src/controller/connection.ts",
                "symbols": ["ConnectionController"],
                "relationships": []
            }
        ],
        "diagnostics": (
            [{"severity": "warning", "code": "mock_partial"}]
            if args.mode == "partial" else []
        )
    }
    args.output.write_text(json.dumps(payload), encoding="utf-8")
    print(json.dumps({"wrote": str(args.output), "mode": args.mode}))
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
