#!/usr/bin/env python3
"""Deterministic local-runner tests for local-atlas-ab.py."""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("local-atlas-ab.py")
EXPORTER = Path(__file__).with_name("benchmark-evidence-export.py")
EXAMPLE = Path(__file__).with_name("local-ab-example-tasks.json")
OMP_ADAPTER = Path(__file__).with_name("local-omp-json-stdio.py")

DOCS = Path(__file__).parent.parent / "docs" / "local-testing.md"

SPEC = importlib.util.spec_from_file_location("local_atlas_ab", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
harness = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = harness
SPEC.loader.exec_module(harness)


class LocalAtlasAbTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name) / "workspace"
        self.workspace.mkdir()
        self.destination = self.workspace / "campaign"
        self.fake_atlas = self.workspace / "fake-atlas-mcp"
        self.fake_atlas.write_bytes(b"fixture atlas mcp")
        self.fake = self.workspace / "fake_runner.py"
        self.fake.write_text(textwrap.dedent("""\
            import json, subprocess, sys, time
            request = json.load(sys.stdin)
            if request["task_id"] == "slow":
                time.sleep(0.2)
            if request["task_id"] == "oversized":
                sys.stdout.write("x" * (1024 * 1024 + 1024))
                raise SystemExit(0)
            if request["task_id"] == "lingering-child":
                subprocess.Popen([
                    sys.executable,
                    "-c",
                    "import pathlib,time; time.sleep(0.3); pathlib.Path('descendant-survived').write_text('bad')",
                ])
            if request["task_id"] == "malformed":
                print("not json")
                raise SystemExit(0)
            if request["task_id"] == "no-acceptance":
                print(json.dumps({"elapsed_ms": 3}))
                raise SystemExit(0)
            if request["task_id"] == "nonfinite":
                print('{"elapsed_ms":NaN}')
                raise SystemExit(0)
            on = request["arm"] == "on"
            print(json.dumps({
                "accepted_outcome": {"accepted": on, "state": "accepted" if on else "rejected"},
                "elapsed_ms": 11 if on else 13,
                "tokens": {"input": 10, "output": None, "total": None},
                "tool_calls": 2 if on else 0,
                "files_read": 4,
                "source_bytes_read": 120,
                "atlas_route": "LIGHT" if on else None,
                "atlas_runtime_ms": 5 if on else None,
                "context_expansion": {"records": 3, "estimated_tokens": None} if on else None,
            }, sort_keys=True))
        """), encoding="utf-8", newline="\n")
        self.tasks = self.workspace / "tasks.json"
        self.tasks.write_text(json.dumps({
            "schema_version": "1.0.0",
            "tasks": [
                {"id": "alpha", "prompt": "Inspect the alpha fixture."},
                {"id": "no-acceptance", "prompt": "Return no accepted outcome."},
                {"id": "malformed", "prompt": "Return malformed output."},
                {"id": "slow", "prompt": "Exercise the runner timeout."},
                {"id": "oversized", "prompt": "Exercise the output byte bound."},
                {"id": "lingering-child", "prompt": "Exercise descendant containment."},
                {"id": "nonfinite", "prompt": "Return a non-finite metric."},
            ],
        }), encoding="utf-8")
        self.adapter = self.workspace / "adapter.json"
        self.adapter.write_text(json.dumps({
            "schema_version": "1.0.0", "adapter": "fixture-json-stdio", "model": "fixture-local",
            "command": [
                sys.executable, str(self.fake), "--access-token",
                "sensitive-fixture-value", "second-sensitive-fixture-value",
            ],
            "executables": [
                {"name": "adapter", "path": str(self.fake), "version_args": []},
                {"name": "atlas-mcp", "path": str(self.fake_atlas), "version_args": []},
                {"name": "omp", "path": sys.executable, "version_args": ["--version"]},
            ],
        }), encoding="utf-8")

    def run_harness(self, *tasks: str, destination: Path | None = None, extra: tuple[str, ...] = ()) -> subprocess.CompletedProcess[str]:
        arguments = [
            sys.executable, str(SCRIPT), "--workspace", str(self.workspace),
            "--tasks", str(self.tasks), "--adapter", str(self.adapter),
            "--destination", str(destination or self.destination),
            "--repetitions", "2", "--timeout", "5",
        ]
        for task in tasks:
            arguments.extend(("--task", task))
        arguments.extend(extra)
        return subprocess.run(arguments, capture_output=True, text=True, check=False)

    def records(self) -> list[dict[str, object]]:
        return [
            json.loads(line)
            for line in (self.destination / "raw.ndjson")
            .read_text(encoding="utf-8")
            .splitlines()
        ]

    def test_runs_strict_paired_off_then_on_and_captures_structured_metrics(self) -> None:
        result = self.run_harness("alpha")
        self.assertEqual(result.returncode, 0, result.stderr)
        records = self.records()
        self.assertTrue(
            all(record["schema_version"] == "1.0.0" for record in records)
        )
        self.assertEqual([(row["repetition"], row["arm"]) for row in records], [
            (1, "off"), (1, "on"), (2, "off"), (2, "on")
        ])
        self.assertEqual(records[0]["accepted_outcome"], {"accepted": False, "state": "rejected"})
        self.assertEqual(records[1]["accepted_outcome"], {"accepted": True, "state": "accepted"})
        self.assertIsNone(records[1]["tokens"]["output"])
        self.assertEqual(records[1]["tool_calls"], 2)
        self.assertEqual(records[1]["files_read"], 4)
        self.assertEqual(records[1]["source_bytes_read"], 120)
        self.assertEqual(records[1]["atlas_route"], "LIGHT")
        self.assertEqual(records[1]["atlas_runtime_ms"], 5)
        self.assertEqual(records[1]["context_expansion"], {"estimated_tokens": None, "records": 3})
        self.assertNotEqual(records[0]["arm_workspace"], records[1]["arm_workspace"])
        self.assertTrue((self.destination / records[0]["arm_workspace"] / "result.json").is_file())

        manifest = json.loads((self.destination / "harness-manifest.json").read_text(encoding="utf-8"))
        raw_bytes = (self.destination / "raw.ndjson").read_bytes()
        self.assertEqual(manifest["raw_sha256"], hashlib.sha256(raw_bytes).hexdigest())
        self.assertEqual(manifest["raw_count"], 4)
        self.assertEqual(manifest["expected_raw_count"], 4)
        self.assertEqual(manifest["state"], "complete")
        self.assertEqual(manifest["tasks"], ["alpha"])
        serialized = json.dumps(manifest)
        self.assertNotIn("sensitive-fixture-value", serialized)
        self.assertNotIn("second-sensitive-fixture-value", serialized)
        self.assertNotIn(str(self.workspace), serialized)
        self.assertEqual(
            manifest["command_display"][-3:],
            ["--access-token", "<redacted>", "<redacted>"],
        )
        self.assertEqual(len(manifest["command_identity_sha256"]), 64)
        self.assertEqual(len(manifest["model_identity_sha256"]), 64)
        self.assertEqual(len(manifest["executable_identity_sha256"]), 64)
        self.assertEqual(
            [entry["name"] for entry in manifest["executables"]],
            ["adapter", "atlas-mcp", "omp"],
        )

    def test_success_without_acceptance_stays_unavailable_and_malformed_is_failure(self) -> None:
        result = self.run_harness(
            "no-acceptance", "malformed", "nonfinite",
            extra=("--repetitions", "1"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        records = self.records()
        unavailable = [row for row in records if row["task_id"] == "no-acceptance"]
        self.assertTrue(all(row["exit_code"] == 0 for row in unavailable))
        self.assertTrue(all(row["accepted_outcome"] == {"accepted": None, "state": "unavailable"} for row in unavailable))
        malformed = [row for row in records if row["task_id"] == "malformed"]
        self.assertTrue(all(row["accepted_outcome"]["accepted"] is None for row in malformed))
        self.assertTrue(all(row["error"] == {"kind": "malformed_runner_output"} for row in malformed))
        nonfinite = [row for row in records if row["task_id"] == "nonfinite"]
        self.assertTrue(all(row["error"] == {"kind": "malformed_runner_output"} for row in nonfinite))

    def test_timeout_is_captured_as_unavailable_without_promoting_acceptance(self) -> None:
        result = self.run_harness("slow", extra=("--repetitions", "1", "--timeout", "0.01"))
        self.assertEqual(result.returncode, 0, result.stderr)
        records = self.records()
        self.assertEqual(len(records), 2)
        self.assertTrue(all(row["exit_code"] is None for row in records))
        self.assertTrue(all(row["accepted_outcome"] == {"accepted": None, "state": "unavailable"} for row in records))
        self.assertTrue(all(row["error"] == {"kind": "timeout"} for row in records))

    def test_oversized_runner_output_is_bounded_and_unavailable(self) -> None:
        result = self.run_harness("oversized", extra=("--repetitions", "1"))
        self.assertEqual(result.returncode, 0, result.stderr)
        records = self.records()
        self.assertEqual(len(records), 2)
        self.assertTrue(all(row["error"] == {"kind": "runner_output_too_large"} for row in records))
        self.assertTrue(all(row["accepted_outcome"]["accepted"] is None for row in records))

    def test_successful_runner_cannot_leave_a_descendant_in_an_arm(self) -> None:
        result = self.run_harness("lingering-child", extra=("--repetitions", "1"))
        self.assertEqual(result.returncode, 0, result.stderr)
        time.sleep(0.5)
        self.assertFalse(any(path.name == "descendant-survived" for path in self.destination.rglob("*")))

    def test_unexpected_interruption_retains_bound_partial_exportable_evidence(self) -> None:
        task_document = json.loads(self.tasks.read_text(encoding="utf-8"))
        task_document["tasks"] = [task_document["tasks"][0]]
        self.tasks.write_text(json.dumps(task_document), encoding="utf-8")
        successful = {
            "accepted_outcome": {"accepted": False, "state": "rejected"},
            "elapsed_ms": 1, "tokens": None, "tool_calls": 0,
            "files_read": 0, "source_bytes_read": 0, "atlas_route": None,
            "atlas_runtime_ms": None, "context_expansion": None,
        }
        with mock.patch.object(
            harness, "_run",
            side_effect=[(0, harness._capture(successful, 0), 12), RuntimeError("simulated interruption")],
        ):
            with self.assertRaisesRegex(RuntimeError, "simulated interruption"):
                harness.campaign(
                    self.workspace, self.tasks, self.adapter, self.destination,
                    ["alpha"], 1, 1, 5,
                )

        raw = (self.destination / "raw.ndjson").read_bytes()
        manifest = json.loads((self.destination / "harness-manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["state"], "partial")
        self.assertEqual(manifest["raw_count"], 1)
        self.assertEqual(manifest["expected_raw_count"], 2)
        self.assertEqual(manifest["raw_sha256"], hashlib.sha256(raw).hexdigest())
        bundle = self.workspace / "partial-bundle"
        exported = subprocess.run([
            sys.executable, str(EXPORTER), "--workspace", str(self.workspace),
            "--input", str(self.destination / "raw.ndjson"),
            "--destination", str(bundle),
        ], capture_output=True, text=True, check=False)
        self.assertEqual(exported.returncode, 0, exported.stderr)
        summary = json.loads((bundle / "summary.json").read_text(encoding="utf-8"))
        self.assertEqual(summary["state"], "partial")
        self.assertEqual(summary["counts"]["records"], 1)

    def test_bounds_explicit_selection_existing_destination_and_escape_fail_closed(self) -> None:
        cases = [
            ("no-task", [], ()),
            ("unknown-task", ["absent"], ()),
            ("repetitions", ["alpha"], ("--repetitions", "11")),
            ("max-tasks", ["alpha", "no-acceptance"], ("--max-tasks", "1")),
        ]
        for label, tasks, extra in cases:
            with self.subTest(label=label):
                destination = self.workspace / f"bad-{label}"
                result = self.run_harness(*tasks, destination=destination, extra=extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(destination.exists())
        self.destination.mkdir()
        self.assertNotEqual(self.run_harness("alpha").returncode, 0)
        outside = Path(self.temporary.name) / "outside"
        self.assertNotEqual(self.run_harness("alpha", destination=outside).returncode, 0)
        self.assertFalse(outside.exists())

    def test_harness_output_feeds_exporter(self) -> None:
        campaign = self.run_harness("alpha", extra=("--repetitions", "1"))
        self.assertEqual(campaign.returncode, 0, campaign.stderr)
        bundle = self.workspace / "bundle"
        exported = subprocess.run([
            sys.executable, str(EXPORTER), "--workspace", str(self.workspace),
            "--input", str(self.destination / "raw.ndjson"), "--destination", str(bundle),
        ], capture_output=True, text=True, check=False)
        self.assertEqual(exported.returncode, 0, exported.stderr)
        summary = json.loads((bundle / "summary.json").read_text(encoding="utf-8"))
        self.assertEqual(summary["counts"]["records"], 2)
        self.assertEqual(summary["counts"]["accepted"], 1)
        self.assertEqual(summary["counts"]["rejected"], 1)

    def test_timeout_and_adapter_identity_are_material_campaign_parameters(self) -> None:
        first = self.run_harness(
            "alpha",
            destination=self.workspace / "campaign-five-seconds",
            extra=("--repetitions", "1", "--timeout", "5"),
        )
        second = self.run_harness(
            "alpha",
            destination=self.workspace / "campaign-six-seconds",
            extra=("--repetitions", "1", "--timeout", "6"),
        )
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        first_manifest = json.loads(first.stdout)
        second_manifest = json.loads(second.stdout)
        self.assertEqual(first_manifest["timeout_seconds"], 5.0)
        self.assertEqual(second_manifest["timeout_seconds"], 6.0)
        self.assertEqual(
            first_manifest["adapter_identity_sha256"],
            second_manifest["adapter_identity_sha256"],
        )
        self.assertEqual(len(first_manifest["adapter_identity_sha256"]), 64)
        self.assertNotEqual(
            first_manifest["campaign_identity_sha256"],
            second_manifest["campaign_identity_sha256"],
        )

    def test_executable_contents_are_material_campaign_identity(self) -> None:
        first = self.run_harness(
            "alpha", destination=self.workspace / "campaign-before",
            extra=("--repetitions", "1"),
        )
        self.fake.write_text(
            self.fake.read_text(encoding="utf-8") + "\n# content identity change\n",
            encoding="utf-8",
        )
        second = self.run_harness(
            "alpha", destination=self.workspace / "campaign-after",
            extra=("--repetitions", "1"),
        )
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        before, after = json.loads(first.stdout), json.loads(second.stdout)
        self.assertNotEqual(
            before["executable_identity_sha256"],
            after["executable_identity_sha256"],
        )
        self.assertNotEqual(
            before["campaign_identity_sha256"],
            after["campaign_identity_sha256"],
        )

    def test_required_executable_provenance_roles_fail_closed(self) -> None:
        original = json.loads(self.adapter.read_text(encoding="utf-8"))
        cases = []
        for role in ("adapter", "atlas-mcp", "omp"):
            cases.append((
                f"missing-{role}",
                [entry for entry in original["executables"] if entry["name"] != role],
            ))
        substituted = [dict(entry) for entry in original["executables"]]
        substituted[0]["name"] = "other-adapter"
        cases.append(("substituted", substituted))
        duplicate = [dict(entry) for entry in original["executables"]]
        duplicate.append(dict(duplicate[0]))
        cases.append(("duplicate", duplicate))
        for name, executables in cases:
            with self.subTest(name=name):
                document = {**original, "executables": executables}
                self.adapter.write_text(json.dumps(document), encoding="utf-8")
                result = self.run_harness(
                    "alpha", destination=self.workspace / f"campaign-{name}",
                    extra=("--repetitions", "1"),
                )
                self.assertNotEqual(result.returncode, 0)

    def test_runner_wall_time_is_independent_and_adapter_timing_is_preserved(
        self,
    ) -> None:
        successful = self.run_harness(
            "alpha", extra=("--repetitions", "1", "--timeout", "5")
        )
        self.assertEqual(successful.returncode, 0, successful.stderr)
        alpha = self.records()
        self.assertEqual(
            [record["adapter_elapsed_ms"] for record in alpha],
            [13, 11],
        )

        timeout_destination = self.workspace / "timeout-campaign"
        timed = self.run_harness(
            "slow",
            destination=timeout_destination,
            extra=("--repetitions", "1", "--timeout", "0.05"),
        )
        self.assertEqual(timed.returncode, 0, timed.stderr)
        timed_out = [
            json.loads(line)
            for line in (timeout_destination / "raw.ndjson")
            .read_text(encoding="utf-8")
            .splitlines()
        ]
        records = alpha + timed_out
        self.assertTrue(
            all(
                isinstance(record["runner_wall_ms"], (int, float))
                and record["runner_wall_ms"] >= 0
                for record in records
            )
        )
        self.assertTrue(
            all(record["error"] == {"kind": "timeout"} for record in timed_out)
        )
        self.assertTrue(all(record["runner_wall_ms"] >= 40 for record in timed_out))

    def test_example_manifest_has_three_representative_non_guaranteed_tasks(
        self,
    ) -> None:
        example = json.loads(EXAMPLE.read_text(encoding="utf-8"))
        self.assertEqual(example["schema_version"], "1.0.0")
        self.assertEqual(
            [task["id"] for task in example["tasks"]],
            [
                "inspect-route-source",
                "plan-bounded-change",
                "handle-stale-error",
            ],
        )
        prompts = " ".join(task["prompt"].lower() for task in example["tasks"])
        self.assertNotIn("guarantee", prompts)
        self.assertNotIn("will succeed", prompts)

    def test_documented_windows_commands_are_directly_copyable_one_liners(
        self,
    ) -> None:
        documentation = DOCS.read_text(encoding="utf-8")
        self.assertIn(
            "py -3 scripts/local-artifact-cleanup.py --workspace .",
            documentation,
        )
        self.assertIn(
            "py -3 scripts/local-atlas-ab.py --workspace . --tasks "
            "scripts/local-ab-example-tasks.json --adapter "
            "config/local-adapter.json --destination "
            ".local-atlas-runs/campaign-001 --task inspect-route-source "
            "--repetitions 1 --timeout 300",
            documentation,
        )
        self.assertIn(
            "py -3 scripts/benchmark-evidence-export.py --workspace . "
            "--input .local-atlas-runs/campaign-001/raw.ndjson "
            "--destination .local-atlas-runs/campaign-001-bundle",
            documentation,
        )
        self.assertNotIn("environment.json contains no secrets", documentation)


class LocalOmpJsonStdioTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.arm = Path(self.temporary.name) / "arm"
        self.arm.mkdir()
        self.atlas_mcp = Path(self.temporary.name) / "atlas-mcp.exe"
        self.atlas_mcp.write_bytes(b"fixture")
        self.fake_omp = Path(self.temporary.name) / "fake-omp.py"
        self.catalogue = Path(self.temporary.name) / "atlas.sqlite"
        self.catalogue.write_bytes(b"fixture catalogue")
        self.fake_omp.write_text(textwrap.dedent("""\
            import json, pathlib, sys
            configured = (pathlib.Path.cwd() / ".mcp.json").is_file()
            disabling = {"--no-tools", "--no-extensions"}
            if configured and any(argument in disabling or argument.startswith("--tools") for argument in sys.argv):
                raise SystemExit(9)
            joined = " ".join(sys.argv)
            if "Return no accepted result." in joined:
                result = {"result": "model omitted its acceptance decision"}
            else:
                result = {
                    "accepted": configured,
                    "state": "accepted" if configured else "rejected",
                    "result": (
                        "Atlas status confirmed: workspace_id=ws_fixture; "
                        "schema_version=1.3.0; integrity_ok=true."
                        if configured else "Atlas unavailable"
                    ),
                }
            emit = configured and "models" not in sys.argv and "No MCP event." not in joined
            if emit:
                tool_name = (
                    "mcp__workspace_atlas_atlas_search"
                    if "Unrelated Atlas result." in joined
                    else "mcp__workspace_atlas_atlas_status"
                )
                payload = {
                    "ok": True, "workspace_id": "ws_fixture",
                    "schema_version": "1.3.0", "integrity_ok": True,
                    "atlas_route": "LIGHT", "atlas_runtime_ms": 4,
                    "context_expansion": {"records": 3},
                }
                start_id = "call-1"
                end_id = "call-2" if "Mismatched pair." in joined else start_id
                if "Missing start." not in joined:
                    start = {"type": "tool_execution_start", "toolCallId": start_id, "toolName": tool_name, "args": {}}
                    print(json.dumps(start))
                    if "Duplicate start." in joined:
                        print(json.dumps(start))
                if "Missing end." not in joined:
                    failed = "Error result." in joined
                    text = "not json" if "Invalid payload." in joined else json.dumps(payload)
                    print(json.dumps({
                        "type": "tool_execution_end", "toolCallId": end_id,
                        "toolName": tool_name,
                        "result": {"content": [{"type": "text", "text": text}], "isError": failed},
                        "isError": failed,
                    }))
            message = {
                "role": "assistant",
                "content": [{"type": "text", "text": json.dumps(result)}],
                "duration": 12.5,
                "usage": {
                    "input": 7, "output": 5, "cacheRead": 0,
                    "cacheWrite": 0, "totalTokens": 12,
                },
            }
            print(json.dumps({"type": "message_end", "message": message}))
            print(json.dumps({"type": "turn_end", "message": message, "toolResults": []}))
            print("diagnostic stays off stdout", file=sys.stderr)
        """), encoding="utf-8", newline="\n")

    def run_adapter(
        self,
        arm: str,
        enabled: str,
        prompt: str = "Inspect one bounded fixture.",
        arm_workspace: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        request = {
            "schema_version": "1.0.0",
            "task_id": "fixture-task",
            "prompt": prompt,
            "repetition": 1,
            "arm": arm,
        }
        cwd = arm_workspace or self.arm
        cwd.mkdir(parents=True, exist_ok=True)
        return subprocess.run(
            [
                sys.executable, str(OMP_ADAPTER),
                "--model", "ollama/qwen2.5-coder:14b",
                "--workspace-root", str(self.arm),
                "--catalogue", str(self.catalogue),
                "--atlas-mcp", str(self.atlas_mcp),
                "--omp-command", sys.executable, str(self.fake_omp),
            ],
            cwd=cwd,
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env={
                "PATH": os.environ["PATH"],
                "ATLAS_ENABLED": enabled,
                "ATLAS_AB_ARM": arm,
            },
            check=False,
        )

    def test_translates_model_result_and_enables_atlas_only_for_on_arm(self) -> None:
        result = self.run_adapter(
            "on", "1", "Inspect Atlas status for one bounded fixture.",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.count("\n"), 1)
        observation = json.loads(result.stdout)
        self.assertEqual(
            observation["accepted_outcome"],
            {"accepted": True, "state": "accepted"},
        )
        self.assertEqual(observation["tool_calls"], 1)
        self.assertEqual(observation["files_read"], 0)
        self.assertEqual(observation["source_bytes_read"], 0)
        self.assertEqual(observation["atlas_route"], "LIGHT")
        self.assertEqual(observation["atlas_runtime_ms"], 4)
        self.assertEqual(observation["context_expansion"], {"records": 3})
        self.assertTrue((self.arm / "omp-output.ndjson").is_file())
        self.assertTrue((self.arm / "atlas-tool-events.json").is_file())
        self.assertEqual(
            json.loads((self.arm / "model-result.json").read_text(encoding="utf-8"))["result"],
            "Atlas status confirmed: workspace_id=ws_fixture; schema_version=1.3.0; integrity_ok=true.",
        )

    def test_configured_on_without_mcp_events_stays_unavailable(self) -> None:
        result = self.run_adapter("on", "1", "No MCP event.")
        self.assertEqual(result.returncode, 0, result.stderr)
        observation = json.loads(result.stdout)
        self.assertEqual(observation["accepted_outcome"], {"accepted": None, "state": "unavailable"})
        self.assertEqual(observation["tool_calls"], 0)
        self.assertEqual(observation["files_read"], 0)
        self.assertEqual(observation["source_bytes_read"], 0)
        self.assertIsNone(observation["atlas_route"])
        self.assertIsNone(observation["atlas_runtime_ms"])
        self.assertIsNone(observation["context_expansion"])
        self.assertFalse((self.arm / "model-result.json").exists())

    def test_malformed_or_unpaired_tool_events_are_rejected(self) -> None:
        cases = (
            "Missing start.", "Missing end.", "Mismatched pair.",
            "Duplicate start.", "Error result.", "Invalid payload.",
        )
        for index, prompt in enumerate(cases):
            with self.subTest(prompt=prompt):
                result = self.run_adapter(
                    "on", "1", prompt, self.arm / f"case-{index}",
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(
                    json.loads(result.stdout)["accepted_outcome"],
                    {"accepted": None, "state": "unavailable"},
                )

    def test_unrelated_atlas_result_cannot_ground_acceptance(self) -> None:
        result = self.run_adapter("on", "1", "Unrelated Atlas result.")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["accepted_outcome"],
            {"accepted": None, "state": "unavailable"},
        )

    def test_process_success_without_model_acceptance_stays_unavailable(self) -> None:
        result = self.run_adapter("off", "0", "Return no accepted result.")
        self.assertEqual(result.returncode, 0, result.stderr)
        observation = json.loads(result.stdout)
        self.assertEqual(
            observation["accepted_outcome"],
            {"accepted": None, "state": "unavailable"},
        )
        self.assertFalse((self.arm / ".mcp.json").exists())
        self.assertFalse((self.arm / "model-result.json").exists())

    def test_rejects_arm_environment_mismatch_without_running_model(self) -> None:
        result = self.run_adapter("off", "1")
        self.assertNotEqual(result.returncode, 0)
        observation = json.loads(result.stdout)
        self.assertEqual(
            observation["accepted_outcome"],
            {"accepted": None, "state": "unavailable"},
        )
        self.assertFalse((self.arm / "omp-output.ndjson").exists())


if __name__ == "__main__":
    unittest.main()
