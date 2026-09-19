#!/usr/bin/env python3
"""Research-only TypeSafe/JEV shadow evaluator for Workspace Atlas."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

SCHEMA_VERSION = "jev-governor-shadow-v0.2.0"
DEFAULT_MODEL = "jev-latest"
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
FORBIDDEN_KEYS = {
    "source", "source_text", "raw_source", "file_contents", "contents",
    "snippet", "diff", "patch", "secret", "secrets", "environment", "env",
}


class ContractError(ValueError):
    pass


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def load_json(path: str) -> dict[str, Any]:
    raw = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise ContractError("input root must be a JSON object")
    return value


def find_forbidden_key(value: Any, trail: tuple[str, ...] = ()) -> str | None:
    if isinstance(value, dict):
        for key, child in value.items():
            if str(key).strip().lower() in FORBIDDEN_KEYS:
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
    if not isinstance(value.get("task"), str) or not value["task"].strip():
        raise ContractError("input.task must be a non-empty string")
    if not isinstance(value.get("atlas"), dict):
        raise ContractError("input.atlas must be a JSON object")
    forbidden = find_forbidden_key(value)
    if forbidden:
        raise ContractError(
            f"input contains forbidden raw/sensitive field '{forbidden}'; "
            "the first experiment accepts task text plus bounded metadata only"
        )


def state_for(value: dict[str, Any]) -> dict[str, Any]:
    return {
        "task": value["task"],
        "atlas": value["atlas"],
        "experiment": {
            "purpose": "shadow prediction only",
            "authoritative": False,
            "route_order": list(ROUTES),
        },
    }


def questions() -> dict[str, dict[str, Any]]:
    return {
        "route": {
            "type": "choice",
            "instructions": (
                "Choose the minimum Workspace Atlas context-acquisition route likely "
                "sufficient for `task` given `atlas`. Prefer less context when sufficient. "
                "Choose deeper context when relationships, uncertainty, temporal evidence, "
                "or validation needs make a shallower route insufficient."
            ),
            "criteria": {
                "DIRECT": "Direct/caller-provided context is likely sufficient.",
                "ATLAS_LIGHT": "Bounded target/source/relationship/coverage evidence is likely needed.",
                "ATLAS_DEEP": "Full bounded task-specific Context IR is likely needed.",
            },
        },
        "task_kind": {
            "type": "choice",
            "instructions": "Classify `task` by the requested repository work.",
            "criteria": {
                "bug_fix": "Correct behavior described as wrong, broken, or failing.",
                "behavior_change": "Intentionally change runtime behavior or semantics.",
                "api_change": "Change a public or consumed interface or contract.",
                "refactor": "Restructure implementation while intending to preserve behavior.",
                "configuration_change": "Primarily change configuration, defaults, or wiring.",
                "audit": "Inspect, review, verify, assess impact, or explain.",
                "unknown": "No category is sufficiently supported by the bounded state.",
            },
        },
        "needs_escalation": {
            "type": "noul",
            "instructions": (
                "Given `atlas.observed_route` when present and the rest of `atlas`, "
                "is a deeper Atlas route likely required for sufficient project evidence?"
            ),
            "criteria": {
                "true": "A deeper route is likely needed.",
                "false": "The current/observed route is likely sufficient.",
            },
        },
        "requires_history": {
            "type": "noul",
            "instructions": "Is temporal/history evidence likely necessary to complete `task` safely?",
            "criteria": {
                "true": "History is likely necessary.",
                "false": "Current project state is likely sufficient.",
            },
        },
        "requires_validation_plan": {
            "type": "noul",
            "instructions": (
                "Does `task` likely require a nontrivial validation plan across tests, "
                "effects, contracts, or affected areas?"
            ),
            "criteria": {
                "true": "A nontrivial validation plan is likely needed.",
                "false": "Validation is likely local/simple.",
            },
        },
        "risk": {
            "type": "score",
            "instructions": (
                "Rate project-context risk: the need for broader project evidence, "
                "not the inherent difficulty of writing code."
            ),
            "criteria": [
                "Minimal: explicit/local target and narrow blast radius.",
                "Bounded: some relationships/tests matter but scope is contained.",
                "Broad: cross-module/public-contract/validation concerns need broader evidence.",
                "Uncertain: unresolved/conflicting/missing evidence makes scope unclear.",
            ],
        },
    }


def request_shape(value: dict[str, Any], model: str) -> dict[str, Any]:
    return {"model": model, "state": state_for(value), "questions": questions()}


def validate_choice(answer: dict[str, Any], allowed: tuple[str, ...], name: str) -> None:
    if answer.get("type") != "choice" or answer.get("choice") not in allowed:
        raise ContractError(f"invalid {name} Choice answer")
    if set(answer.get("probabilities", {})) != set(allowed):
        raise ContractError(f"{name} probabilities must match the closed vocabulary")
    confidence = answer.get("confidence")
    if isinstance(confidence, bool) or not isinstance(confidence, (int, float)) or not 0 <= confidence <= 1:
        raise ContractError(f"invalid {name} confidence")


def validate_noul(answer: dict[str, Any], name: str) -> None:
    probability = answer.get("noul")
    if answer.get("type") != "noul" or isinstance(probability, bool) or not isinstance(probability, (int, float)) or not 0 <= probability <= 1:
        raise ContractError(f"invalid {name} Noul answer")


def validate_score(answer: dict[str, Any]) -> None:
    score = answer.get("score")
    confidence = answer.get("confidence")
    if answer.get("type") != "score":
        raise ContractError("invalid risk Score answer")
    if isinstance(score, bool) or not isinstance(score, (int, float)) or not 0 <= score <= 3:
        raise ContractError("invalid risk score")
    if isinstance(confidence, bool) or not isinstance(confidence, (int, float)) or not 0 <= confidence <= 1:
        raise ContractError("invalid risk confidence")


def validate_judgments(value: dict[str, Any]) -> None:
    expected = {
        "route", "task_kind", "needs_escalation",
        "requires_history", "requires_validation_plan", "risk",
    }
    if set(value) != expected:
        raise ContractError("judgment set does not match the experiment contract")
    validate_choice(value["route"], ROUTES, "route")
    validate_choice(value["task_kind"], TASK_KINDS, "task_kind")
    validate_noul(value["needs_escalation"], "needs_escalation")
    validate_noul(value["requires_history"], "requires_history")
    validate_noul(value["requires_validation_plan"], "requires_validation_plan")
    validate_score(value["risk"])


def normalize_response(response: Any) -> dict[str, Any]:
    route = response.choices["route"]
    task_kind = response.choices["task_kind"]
    escalation = response.nouls["needs_escalation"]
    history = response.nouls["requires_history"]
    validation = response.nouls["requires_validation_plan"]
    risk = response.scores["risk"]
    result = {
        "route": {
            "type": "choice", "choice": route.choice,
            "confidence": route.confidence, "probabilities": dict(route.probabilities),
        },
        "task_kind": {
            "type": "choice", "choice": task_kind.choice,
            "confidence": task_kind.confidence, "probabilities": dict(task_kind.probabilities),
        },
        "needs_escalation": {"type": "noul", "noul": escalation.noul},
        "requires_history": {"type": "noul", "noul": history.noul},
        "requires_validation_plan": {"type": "noul", "noul": validation.noul},
        "risk": {
            "type": "score", "score": risk.score, "confidence": risk.confidence,
            "legend": {str(k): v for k, v in risk.legend.items()},
            "probabilities": {str(k): v for k, v in risk.probabilities.items()},
        },
    }
    validate_judgments(result)
    return result


def envelope(value: dict[str, Any], requested_model: str, response: Any, judgments: dict[str, Any], include_task: bool) -> dict[str, Any]:
    result = {
        "schema_version": SCHEMA_VERSION,
        "mode": "shadow",
        "authoritative": False,
        "runtime_effect": "none",
        "truth_plane_effect": "none",
        "requested_model": requested_model,
        "response_model": response.model,
        "input_hash": sha256_text(canonical_json(value)),
        "task_hash": sha256_text(value["task"]),
        "judgments": judgments,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
        },
    }
    if include_task:
        result["task"] = value["task"]
    return result


def dry_run(args: argparse.Namespace) -> int:
    value = load_json(args.input)
    validate_input(value)
    print(json.dumps({
        "schema_version": SCHEMA_VERSION,
        "mode": "dry_run",
        "authoritative": False,
        "request": request_shape(value, args.model),
    }, indent=2, sort_keys=True))
    return 0


def call(args: argparse.Namespace) -> int:
    value = load_json(args.input)
    validate_input(value)
    try:
        from typesafe_sdk import TypeSafeClient
    except ImportError as exc:
        raise ContractError(
            "The official TypeSafe Python SDK is required for the live call."
        ) from exc

    request = request_shape(value, args.model)
    with TypeSafeClient(model=args.model, timeout=args.timeout_seconds) as client:
        response = client.system_one(
            state=request["state"],
            questions=request["questions"],
        )
    result = envelope(value, args.model, response, normalize_response(response), args.include_task_text)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def self_test(_: argparse.Namespace) -> int:
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
    req = request_shape(fixture, DEFAULT_MODEL)
    assert req["questions"]["route"]["type"] == "choice"
    assert req["questions"]["needs_escalation"]["type"] == "noul"
    assert req["questions"]["risk"]["type"] == "score"

    fake = {
        "route": {
            "type": "choice", "choice": "ATLAS_DEEP", "confidence": 0.84,
            "probabilities": {"DIRECT": 0.04, "ATLAS_LIGHT": 0.12, "ATLAS_DEEP": 0.84},
        },
        "task_kind": {
            "type": "choice", "choice": "behavior_change", "confidence": 0.72,
            "probabilities": {
                "bug_fix": 0.12, "behavior_change": 0.72, "api_change": 0.04,
                "refactor": 0.03, "configuration_change": 0.03, "audit": 0.01,
                "unknown": 0.05,
            },
        },
        "needs_escalation": {"type": "noul", "noul": 0.88},
        "requires_history": {"type": "noul", "noul": 0.18},
        "requires_validation_plan": {"type": "noul", "noul": 0.91},
        "risk": {
            "type": "score", "score": 2.1, "confidence": 0.74,
            "legend": {"0": "Minimal", "1": "Bounded", "2": "Broad", "3": "Uncertain"},
            "probabilities": {"0": 0.04, "1": 0.16, "2": 0.55, "3": 0.25},
        },
    }
    validate_judgments(fake)
    assert "confidence" not in fake["needs_escalation"]

    try:
        validate_input({"task": "bad", "atlas": {"source_text": "blocked"}})
    except ContractError:
        pass
    else:
        raise AssertionError("raw-source guard failed")

    print(json.dumps({
        "ok": True,
        "schema_version": SCHEMA_VERSION,
        "tests": [
            "input_contract", "typed_question_shapes", "choice_probabilities",
            "noul_probability_semantics", "score_semantics", "raw_source_rejection",
        ],
    }, indent=2, sort_keys=True))
    return 0


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="TypeSafe/JEV shadow evaluator; no Atlas runtime authority.")
    sub = p.add_subparsers(dest="command", required=True)

    d = sub.add_parser("dry-run")
    d.add_argument("--input", required=True)
    d.add_argument("--model", default=DEFAULT_MODEL)
    d.set_defaults(func=dry_run)

    c = sub.add_parser("call")
    c.add_argument("--input", required=True)
    c.add_argument("--model", default=DEFAULT_MODEL)
    c.add_argument("--timeout-seconds", type=float, default=30.0)
    c.add_argument("--include-task-text", action="store_true")
    c.set_defaults(func=call)

    s = sub.add_parser("self-test")
    s.set_defaults(func=self_test)
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        return int(args.func(args))
    except (ContractError, OSError, json.JSONDecodeError) as exc:
        print(json.dumps({"ok": False, "schema_version": SCHEMA_VERSION, "error": str(exc)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
