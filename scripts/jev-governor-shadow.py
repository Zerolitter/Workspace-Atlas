#!/usr/bin/env python3
"""Research-only JEV shadow adapter for Workspace Atlas Governor evaluation.

This script is deliberately outside the Atlas runtime path. It cannot change a
Governor decision, catalogue state, Context IR, Serving state, or workspace
source. Network execution is explicit through the `call` subcommand.

The default request uses OpenRouter's documented OpenAI-compatible chat
endpoint. `--request-json-schema` additionally asks for JSON Schema structured
output; keep this optional until per-model support is proven in the experiment.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

SCHEMA_VERSION = "jev-governor-shadow-v0.1.0"
DEFAULT_MODEL = "typesafe/jev-1.13"
DEFAULT_ENDPOINT = "https://openrouter.ai/api/v1/chat/completions"

ROUTES = ("DIRECT", "ATLAS_LIGHT", "ATLAS_DEEP")
TASK_KINDS = (
    "bug_fix",
    "behavior_change",
    "api_change",
    "refactor",
    "configuration_change",
    "audit",
    "unknown",
)
RISKS = ("minimal", "bounded", "broad", "uncertain")
REASON_CODES = (
    "explicit_target_sufficient",
    "bounded_relationships_needed",
    "cross_module_scope",
    "public_api_surface",
    "unresolved_edges",
    "conflict_state",
    "temporal_requirement",
    "validation_requirement",
    "high_context_cost",
    "unknown_task_kind",
    "insufficient_metadata",
)

# Exact key matches only. Fields such as estimated_source_tokens are allowed.
FORBIDDEN_KEYS = {
    "source",
    "source_text",
    "raw_source",
    "file_contents",
    "contents",
    "snippet",
    "diff",
    "patch",
    "secret",
    "secrets",
    "api_key",
    "authorization",
    "environment",
    "env",
}


class ContractError(ValueError):
    pass


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def load_json(path: str) -> dict[str, Any]:
    if path == "-":
        raw = sys.stdin.read()
    else:
        raw = Path(path).read_text(encoding="utf-8")
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ContractError(f"input is not valid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ContractError("input root must be a JSON object")
    return value


def find_forbidden_key(value: Any, trail: tuple[str, ...] = ()) -> str | None:
    if isinstance(value, dict):
        for key, child in value.items():
            lowered = str(key).strip().lower()
            if lowered in FORBIDDEN_KEYS:
                return ".".join((*trail, str(key)))
            found = find_forbidden_key(child, (*trail, str(key)))
            if found:
                return found
    elif isinstance(value, list):
        for index, child in enumerate(value):
            found = find_forbidden_key(child, (*trail, str(index)))
            if found:
                return found
    return None


def validate_input(value: dict[str, Any]) -> None:
    task = value.get("task")
    atlas = value.get("atlas")
    if not isinstance(task, str) or not task.strip():
        raise ContractError("input.task must be a non-empty string")
    if len(task) > 12000:
        raise ContractError("input.task exceeds the 12,000-character research bound")
    if not isinstance(atlas, dict):
        raise ContractError("input.atlas must be a JSON object")

    forbidden = find_forbidden_key(value)
    if forbidden:
        raise ContractError(
            f"input contains forbidden raw/sensitive field '{forbidden}'; "
            "the first JEV experiment accepts task text plus bounded metadata only"
        )


def decision_json_schema() -> dict[str, Any]:
    return {
        "type": "object",
        "properties": {
            "route": {"type": "string", "enum": list(ROUTES)},
            "task_kind": {"type": "string", "enum": list(TASK_KINDS)},
            "requires_history": {"type": "boolean"},
            "requires_validation_plan": {"type": "boolean"},
            "risk": {"type": "string", "enum": list(RISKS)},
            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
            "reasons": {
                "type": "array",
                "items": {"type": "string", "enum": list(REASON_CODES)},
                "uniqueItems": True,
                "maxItems": 8,
            },
        },
        "required": [
            "route",
            "task_kind",
            "requires_history",
            "requires_validation_plan",
            "risk",
            "confidence",
            "reasons",
        ],
        "additionalProperties": False,
    }


def build_request(
    experiment_input: dict[str, Any],
    model: str,
    request_json_schema: bool,
) -> dict[str, Any]:
    system = (
        "You are a research-only shadow policy evaluator for Workspace Atlas. "
        "Predict the minimum useful context-acquisition depth for the supplied "
        "task and bounded Atlas metadata. You do not decide repository truth and "
        "you do not authorize edits. Return ONLY one JSON object matching the "
        "closed output contract. Do not include prose, markdown, hidden reasoning, "
        "or chain-of-thought."
    )

    user_payload = {
        "question": "What is the minimum useful Atlas route for this task?",
        "route_vocabulary": list(ROUTES),
        "task_kind_vocabulary": list(TASK_KINDS),
        "risk_vocabulary": list(RISKS),
        "reason_code_vocabulary": list(REASON_CODES),
        "output_contract": decision_json_schema(),
        "input": experiment_input,
    }

    body: dict[str, Any] = {
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": canonical_json(user_payload)},
        ],
    }

    if request_json_schema:
        body["response_format"] = {
            "type": "json_schema",
            "json_schema": {
                "name": "atlas_governor_shadow_decision",
                "strict": True,
                "schema": decision_json_schema(),
            },
        }

    return body


def strip_code_fence(text: str) -> str:
    stripped = text.strip()
    if stripped.startswith("```"):
        lines = stripped.splitlines()
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        return "\n".join(lines).strip()
    return stripped


def extract_decision(provider_response: dict[str, Any]) -> dict[str, Any]:
    choices = provider_response.get("choices")
    if not isinstance(choices, list) or not choices:
        raise ContractError("provider response has no choices")

    first = choices[0]
    if not isinstance(first, dict):
        raise ContractError("provider response choice is not an object")

    message = first.get("message")
    if not isinstance(message, dict):
        raise ContractError("provider response choice has no message object")

    parsed = message.get("parsed")
    if isinstance(parsed, dict):
        decision = parsed
    else:
        content = message.get("content")
        if isinstance(content, str):
            raw = strip_code_fence(content)
        elif isinstance(content, list):
            text_parts = []
            for part in content:
                if isinstance(part, dict) and isinstance(part.get("text"), str):
                    text_parts.append(part["text"])
            raw = strip_code_fence("".join(text_parts))
        else:
            raise ContractError("provider response message has no parseable content")

        try:
            decision = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise ContractError(f"JEV response is not valid JSON: {exc}") from exc

    if not isinstance(decision, dict):
        raise ContractError("JEV decision must be a JSON object")

    validate_decision(decision)
    return decision


def validate_decision(decision: dict[str, Any]) -> None:
    expected = {
        "route",
        "task_kind",
        "requires_history",
        "requires_validation_plan",
        "risk",
        "confidence",
        "reasons",
    }
    actual = set(decision)
    if actual != expected:
        raise ContractError(
            "JEV decision keys must exactly match the shadow contract; "
            f"expected {sorted(expected)}, got {sorted(actual)}"
        )

    if decision["route"] not in ROUTES:
        raise ContractError(f"invalid route: {decision['route']!r}")
    if decision["task_kind"] not in TASK_KINDS:
        raise ContractError(f"invalid task_kind: {decision['task_kind']!r}")
    if decision["risk"] not in RISKS:
        raise ContractError(f"invalid risk: {decision['risk']!r}")
    if not isinstance(decision["requires_history"], bool):
        raise ContractError("requires_history must be boolean")
    if not isinstance(decision["requires_validation_plan"], bool):
        raise ContractError("requires_validation_plan must be boolean")

    confidence = decision["confidence"]
    if isinstance(confidence, bool) or not isinstance(confidence, (int, float)):
        raise ContractError("confidence must be numeric")
    if not 0.0 <= float(confidence) <= 1.0:
        raise ContractError("confidence must be between 0 and 1")

    reasons = decision["reasons"]
    if not isinstance(reasons, list) or len(reasons) > 8:
        raise ContractError("reasons must be an array with at most 8 values")
    if len(set(reasons)) != len(reasons):
        raise ContractError("reasons must be unique")
    invalid = [reason for reason in reasons if reason not in REASON_CODES]
    if invalid:
        raise ContractError(f"invalid reason code(s): {invalid}")


def call_openrouter(
    body: dict[str, Any],
    endpoint: str,
    api_key: str,
    timeout_seconds: float,
) -> dict[str, Any]:
    request = urllib.request.Request(
        endpoint,
        data=canonical_json(body).encode("utf-8"),
        method="POST",
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "X-Title": "Workspace Atlas JEV Governor Shadow",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            raw = response.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        body_text = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(
            f"OpenRouter HTTP {exc.code}; response={body_text[:2000]}"
        ) from exc
    except urllib.error.URLError as exc:
        raise RuntimeError(f"OpenRouter request failed: {exc.reason}") from exc

    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise RuntimeError("OpenRouter returned non-JSON content") from exc
    if not isinstance(value, dict):
        raise RuntimeError("OpenRouter response root is not an object")
    return value


def envelope(
    experiment_input: dict[str, Any],
    model: str,
    decision: dict[str, Any],
    provider_response: dict[str, Any] | None,
    include_task_text: bool,
) -> dict[str, Any]:
    input_hash = sha256_text(canonical_json(experiment_input))
    task_hash = sha256_text(experiment_input["task"])
    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "mode": "shadow",
        "authoritative": False,
        "runtime_effect": "none",
        "truth_plane_effect": "none",
        "model": model,
        "input_hash": input_hash,
        "task_hash": task_hash,
        "decision": decision,
    }
    if include_task_text:
        result["task"] = experiment_input["task"]

    if provider_response is not None:
        response_model = provider_response.get("model")
        if isinstance(response_model, str):
            result["provider_response_model"] = response_model
        usage = provider_response.get("usage")
        if isinstance(usage, dict):
            # Usage contains accounting metadata, not prompt/source content.
            result["usage"] = usage

    return result


def command_dry_run(args: argparse.Namespace) -> int:
    value = load_json(args.input)
    validate_input(value)
    body = build_request(value, args.model, args.request_json_schema)
    output = {
        "schema_version": SCHEMA_VERSION,
        "mode": "dry_run",
        "authoritative": False,
        "warning": (
            "The task text and bounded Atlas metadata shown in request_body "
            "would be sent to the configured external endpoint."
        ),
        "endpoint": args.endpoint,
        "request_body": body,
    }
    print(json.dumps(output, indent=2, ensure_ascii=False, sort_keys=True))
    return 0


def command_call(args: argparse.Namespace) -> int:
    value = load_json(args.input)
    validate_input(value)

    api_key = os.environ.get("OPENROUTER_API_KEY")
    if not api_key:
        raise ContractError(
            "OPENROUTER_API_KEY is required for 'call'; use dry-run without credentials"
        )

    body = build_request(value, args.model, args.request_json_schema)
    provider_response = call_openrouter(
        body=body,
        endpoint=args.endpoint,
        api_key=api_key,
        timeout_seconds=args.timeout_seconds,
    )
    decision = extract_decision(provider_response)
    result = envelope(
        experiment_input=value,
        model=args.model,
        decision=decision,
        provider_response=provider_response,
        include_task_text=args.include_task_text,
    )
    print(json.dumps(result, indent=2, ensure_ascii=False, sort_keys=True))
    return 0


def command_self_test(_: argparse.Namespace) -> int:
    fixture = {
        "task": "Fix reconnect timeout without changing login semantics.",
        "atlas": {
            "observed_route": "ATLAS_LIGHT",
            "task_kind": "bug_fix",
            "explicit_target": True,
            "estimated_source_tokens": 940,
            "direct_callers": 7,
            "direct_callees": 3,
            "tests": 4,
            "packages_spanned": 3,
            "conflicts": 0,
            "unresolved_edges": 2,
            "history_available": True,
            "public_api": True,
        },
    }
    validate_input(fixture)
    body = build_request(fixture, DEFAULT_MODEL, request_json_schema=True)
    assert body["model"] == DEFAULT_MODEL
    assert body["response_format"]["json_schema"]["schema"]["additionalProperties"] is False

    fake_provider_response = {
        "model": DEFAULT_MODEL,
        "choices": [
            {
                "message": {
                    "content": json.dumps(
                        {
                            "route": "ATLAS_DEEP",
                            "task_kind": "behavior_change",
                            "requires_history": False,
                            "requires_validation_plan": True,
                            "risk": "broad",
                            "confidence": 0.84,
                            "reasons": [
                                "cross_module_scope",
                                "validation_requirement",
                            ],
                        }
                    )
                }
            }
        ],
        "usage": {"prompt_tokens": 100, "completion_tokens": 0},
    }
    decision = extract_decision(fake_provider_response)
    result = envelope(
        fixture,
        DEFAULT_MODEL,
        decision,
        fake_provider_response,
        include_task_text=False,
    )
    assert result["authoritative"] is False
    assert result["runtime_effect"] == "none"
    assert "task" not in result
    assert result["decision"]["route"] == "ATLAS_DEEP"

    try:
        validate_input(
            {
                "task": "bad fixture",
                "atlas": {"source_text": "do not transmit source"},
            }
        )
    except ContractError:
        pass
    else:
        raise AssertionError("raw-source guard did not reject source_text")

    print(
        json.dumps(
            {
                "ok": True,
                "schema_version": SCHEMA_VERSION,
                "tests": [
                    "input_contract",
                    "request_shape",
                    "decision_validation",
                    "redacted_output",
                    "raw_source_rejection",
                ],
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


def add_common_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--input", required=True, help="JSON input file, or '-' for stdin")
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--endpoint", default=DEFAULT_ENDPOINT)
    parser.add_argument(
        "--request-json-schema",
        action="store_true",
        help=(
            "Request OpenRouter JSON Schema structured output. Optional because "
            "per-model parameter support must be proven by the experiment."
        ),
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Research-only JEV shadow adapter. It predicts Atlas Governor policy "
            "but has no runtime or Truth Plane authority."
        )
    )
    sub = parser.add_subparsers(dest="command", required=True)

    dry = sub.add_parser("dry-run", help="validate input and print outbound request")
    add_common_arguments(dry)
    dry.set_defaults(func=command_dry_run)

    call = sub.add_parser("call", help="explicitly call OpenRouter/JEV")
    add_common_arguments(call)
    call.add_argument("--timeout-seconds", type=float, default=30.0)
    call.add_argument(
        "--include-task-text",
        action="store_true",
        help="include raw task text in local output; default output stores only hashes",
    )
    call.set_defaults(func=command_call)

    self_test = sub.add_parser("self-test", help="run contract/parser checks without network")
    self_test.set_defaults(func=command_self_test)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        return int(args.func(args))
    except (ContractError, RuntimeError, OSError) as exc:
        print(
            json.dumps(
                {
                    "ok": False,
                    "schema_version": SCHEMA_VERSION,
                    "error": str(exc),
                },
                ensure_ascii=False,
                sort_keys=True,
            ),
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
