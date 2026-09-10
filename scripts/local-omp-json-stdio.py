#!/usr/bin/env python3
"""Translate one local A/B harness request into an isolated OMP model run."""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Any

SCHEMA_VERSION = "1.0.0"
MODEL_RESULT_KEYS = {"accepted", "state", "result"}
TASK_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
MAX_REQUEST_BYTES = 1024 * 1024
MAX_OMP_OUTPUT_BYTES = 16 * 1024 * 1024
SYSTEM_PROMPT = """You are the model worker in a bounded local evidence run.
No model tools are available. Do not claim to have inspected files or Atlas results,
and do not infer missing measurements. Perform the supplied task using only its text
and the stated Atlas arm. Finish with exactly one JSON object and no prose:
{"accepted":true|false|null,"state":"accepted|rejected|unavailable","result":"brief evidence-grounded result"}
Set accepted true only when the requested result was actually produced from the
provided input, false only when that input rejects it, and null when the requested
evidence or capability is unavailable. Never emit a tool-call object.
"""
class AdapterError(RuntimeError):
    """The adapter cannot produce a grounded model observation."""


def _canonical(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def _unavailable() -> dict[str, Any]:
    return {
        "accepted_outcome": {"accepted": None, "state": "unavailable"},
        "elapsed_ms": None,
        "tokens": None,
        "tool_calls": None,
        "files_read": None,
        "source_bytes_read": None,
        "atlas_route": None,
        "atlas_runtime_ms": None,
        "context_expansion": None,
    }


def _request() -> dict[str, Any]:
    data = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    if len(data) > MAX_REQUEST_BYTES:
        raise AdapterError("request exceeds byte bound")
    try:
        value = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AdapterError("request is malformed JSON") from error
    if not isinstance(value, dict) or set(value) != {
        "arm", "prompt", "repetition", "schema_version", "task_id"
    }:
        raise AdapterError("request contract is malformed")
    if value["schema_version"] != SCHEMA_VERSION:
        raise AdapterError("request schema is unsupported")
    if value["arm"] not in ("off", "on"):
        raise AdapterError("request arm is malformed")
    if (
        not isinstance(value["task_id"], str)
        or TASK_ID.fullmatch(value["task_id"]) is None
        or not isinstance(value["prompt"], str)
        or not value["prompt"]
        or len(value["prompt"].encode("utf-8")) > 256 * 1024
        or isinstance(value["repetition"], bool)
        or not isinstance(value["repetition"], int)
        or value["repetition"] < 1
    ):
        raise AdapterError("request fields are malformed")
    expected = "1" if value["arm"] == "on" else "0"
    if os.environ.get("ATLAS_ENABLED") != expected or os.environ.get("ATLAS_AB_ARM") != value["arm"]:
        raise AdapterError("request arm disagrees with harness environment")
    return value


def _prepare_state(cwd: Path, atlas_mcp: Path, arm: str) -> Path:
    state = cwd / "omp-state"
    state.mkdir()
    (state / "models.yml").write_text(
        "providers:\n"
        "  ollama:\n"
        "    baseUrl: http://127.0.0.1:11434\n"
        "    api: openai-responses\n"
        "    auth: none\n"
        "    discovery:\n"
        "      type: ollama\n"
        "      timeoutMs: 30000\n",
        encoding="utf-8",
        newline="\n",
    )
    (cwd / "omp-config.yml").write_text("retry:\n  enabled: false\n", encoding="utf-8", newline="\n")
    if arm == "on":
        metadata = atlas_mcp.lstat()
        if not stat.S_ISREG(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            raise AdapterError("Atlas MCP command is not a regular file")
        (cwd / ".mcp.json").write_bytes(_canonical({
            "mcpServers": {
                "workspace-atlas": {
                    "type": "stdio",
                    "command": str(atlas_mcp),
                    "args": [],
                }
            }
        }))
    return state


def _content_text(message: dict[str, Any]) -> str | None:
    content = message.get("content")
    if not isinstance(content, list):
        return None
    parts = [part.get("text") for part in content if isinstance(part, dict) and part.get("type") == "text"]
    if not parts or not all(isinstance(part, str) for part in parts):
        return None
    return "".join(parts)


def _model_result(text: str | None) -> dict[str, Any] | None:
    if text is None:
        return None
    stripped = text.strip()
    if stripped.startswith("```json") and stripped.endswith("```"):
        stripped = stripped[7:-3].strip()
    try:
        value = json.loads(stripped)
    except json.JSONDecodeError:
        return None
    if not isinstance(value, dict) or set(value) != MODEL_RESULT_KEYS:
        return None
    accepted, state, result = value["accepted"], value["state"], value["result"]
    if (
        (accepted is not None and not isinstance(accepted, bool))
        or state not in ("accepted", "rejected", "unavailable")
        or (accepted is True and state != "accepted")
        or (accepted is False and state != "rejected")
        or (accepted is None and state != "unavailable")
        or not isinstance(result, str)
        or len(result.encode("utf-8")) > 64 * 1024
    ):
        return None
    return value


def _number(value: Any) -> int | float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if value < 0 or (isinstance(value, float) and not math.isfinite(value)):
        return None
    return value


def _parse_omp_output(path: Path) -> tuple[dict[str, Any] | None, dict[str, Any]]:
    if path.stat().st_size > MAX_OMP_OUTPUT_BYTES:
        raise AdapterError("OMP output exceeds byte bound")
    events: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise AdapterError("OMP output is malformed JSONL") from error
        if not isinstance(event, dict):
            raise AdapterError("OMP output event is not an object")
        events.append(event)
    messages = [
        event["message"] for event in events
        if event.get("type") in ("message_end", "turn_end")
        and isinstance(event.get("message"), dict)
        and event["message"].get("role") == "assistant"
    ]
    message = messages[-1] if messages else {}
    result = _model_result(_content_text(message))
    usage = message.get("usage") if isinstance(message.get("usage"), dict) else {}
    token_values = {
        "input": _number(usage.get("input")),
        "output": _number(usage.get("output")),
        "total": _number(usage.get("totalTokens")),
    }
    tokens = token_values if any(value is not None for value in token_values.values()) else None
    turns = [event for event in events if event.get("type") == "turn_end"]
    tool_results = turns[-1].get("toolResults") if turns and isinstance(turns[-1].get("toolResults"), list) else None
    tool_calls = len(tool_results) if tool_results is not None else None
    files_read = None
    if tool_results is not None:
        files_read = sum(
            1 for item in tool_results
            if isinstance(item, dict) and item.get("toolName") == "read"
        )
    return result, {
        "elapsed_ms": _number(message.get("duration")),
        "tokens": tokens,
        "tool_calls": tool_calls,
        "files_read": files_read,
        "source_bytes_read": None,
        "atlas_route": None,
        "atlas_runtime_ms": None,
        "context_expansion": None,
    }


def run(arguments: argparse.Namespace) -> tuple[int, dict[str, Any]]:
    request = _request()
    cwd = Path.cwd()
    repository = Path(__file__).resolve().parent.parent
    atlas_mcp = Path(arguments.atlas_mcp).absolute()
    state = _prepare_state(cwd, atlas_mcp, request["arm"])
    (cwd / "request.json").write_bytes(_canonical(request))
    prompt = (
        f"Task ID: {request['task_id']}\nRepetition: {request['repetition']}\n"
        f"Atlas arm: {request['arm']} (ATLAS_ENABLED={os.environ['ATLAS_ENABLED']})\n\n"
        f"Task:\n{request['prompt']}"
    )
    (cwd / "prompt.txt").write_text(prompt + "\n", encoding="utf-8", newline="\n")
    command = [
        *arguments.omp_command,
        "-p", "--model", arguments.model, "--mode", "json", "--no-session",
        "--no-tools", "--no-skills", "--no-rules", "--no-lsp", "--no-pty",
        "--config", str(cwd / "omp-config.yml"), "--max-time", str(arguments.max_time),
        "--system-prompt", SYSTEM_PROMPT, "--add-dir", str(repository), prompt,
    ]
    environment = os.environ.copy()
    environment["PI_CODING_AGENT_DIR"] = str(state)
    environment["OMP_WORKTREE_DIR"] = str(cwd / "omp-worktrees")
    discovery = [*arguments.omp_command, "models", "ollama", "--json"]
    with (cwd / "omp-models.json").open("xb") as stdout, (
        cwd / "omp-models.stderr.txt"
    ).open("xb") as stderr:
        discovered = subprocess.run(
            discovery, cwd=cwd, env=environment, stdout=stdout, stderr=stderr,
            check=False,
        )
    if discovered.returncode != 0:
        return discovered.returncode, _unavailable()
    with (cwd / "omp-output.ndjson").open("xb") as stdout, (cwd / "omp-stderr.txt").open("xb") as stderr:
        completed = subprocess.run(command, cwd=cwd, env=environment, stdout=stdout, stderr=stderr, check=False)
    if completed.returncode != 0:
        return completed.returncode, _unavailable()
    model_result, metrics = _parse_omp_output(cwd / "omp-output.ndjson")
    if model_result is None:
        return 0, {**_unavailable(), **metrics}
    (cwd / "model-result.json").write_bytes(_canonical(model_result))
    return 0, {
        "accepted_outcome": {
            "accepted": model_result["accepted"],
            "state": model_result["state"],
        },
        **metrics,
    }


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True)
    parser.add_argument("--atlas-mcp", required=True)
    parser.add_argument("--max-time", type=int, default=240)
    parser.add_argument("--omp-command", nargs="+", default=["omp"])
    parsed = parser.parse_args(arguments)
    try:
        exit_code, observation = run(parsed)
    except (AdapterError, OSError, ValueError):
        exit_code, observation = 2, _unavailable()
    sys.stdout.write(json.dumps(observation, sort_keys=True, separators=(",", ":")) + "\n")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
