#!/usr/bin/env python3
"""Deterministic local-runner tests for local-atlas-ab.py."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPT = Path(__file__).with_name("local-atlas-ab.py")
EXPORTER = Path(__file__).with_name("benchmark-evidence-export.py")
EXAMPLE = Path(__file__).with_name("local-ab-example-tasks.json")


class LocalAtlasAbTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name) / "workspace"
        self.workspace.mkdir()
        self.destination = self.workspace / "campaign"
        self.fake = self.workspace / "fake_runner.py"
        self.fake.write_text(textwrap.dedent("""\
            import json, os, sys, time
            request = json.load(sys.stdin)
            if request["task_id"] == "slow":
                time.sleep(0.2)
            if request["task_id"] == "malformed":
                print("not json")
                raise SystemExit(0)
            if request["task_id"] == "no-acceptance":
                print(json.dumps({"elapsed_ms": 3}))
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
            ],
        }), encoding="utf-8")
        self.adapter = self.workspace / "adapter.json"
        self.adapter.write_text(json.dumps({
            "schema_version": "1.0.0", "adapter": "fixture-json-stdio", "model": "fixture-local",
            "command": [sys.executable, str(self.fake), "--access-token", "sensitive-fixture-value"],
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

    def test_runs_strict_paired_off_then_on_and_captures_structured_metrics(self) -> None:
        result = self.run_harness("alpha")

        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in (self.destination / "raw.ndjson").read_text(encoding="utf-8").splitlines()]
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
        self.assertEqual(manifest["state"], "complete")
        self.assertEqual(manifest["tasks"], ["alpha"])
        serialized = json.dumps(manifest)
        self.assertNotIn("sensitive-fixture-value", serialized)
        self.assertNotIn(str(self.workspace), serialized)
        self.assertEqual(manifest["command_display"][-2:], ["--access-token", "<redacted>"])
        self.assertEqual(len(manifest["command_identity_sha256"]), 64)

    def test_success_without_acceptance_stays_unavailable_and_malformed_is_failure(self) -> None:
        result = self.run_harness("no-acceptance", "malformed", extra=("--repetitions", "1"))

        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in (self.destination / "raw.ndjson").read_text(encoding="utf-8").splitlines()]
        unavailable = [row for row in records if row["task_id"] == "no-acceptance"]
        self.assertTrue(all(row["exit_code"] == 0 for row in unavailable))
        self.assertTrue(all(row["accepted_outcome"] == {"accepted": None, "state": "unavailable"} for row in unavailable))
        malformed = [row for row in records if row["task_id"] == "malformed"]
        self.assertTrue(all(row["accepted_outcome"]["accepted"] is None for row in malformed))
        self.assertTrue(all(row["error"] == {"kind": "malformed_runner_output"} for row in malformed))

    def test_timeout_is_captured_as_unavailable_without_promoting_acceptance(self) -> None:
        result = self.run_harness(
            "slow", extra=("--repetitions", "1", "--timeout", "0.01")
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        records = [
            json.loads(line)
            for line in (self.destination / "raw.ndjson")
            .read_text(encoding="utf-8")
            .splitlines()
        ]
        self.assertEqual(len(records), 2)
        self.assertTrue(all(row["exit_code"] is None for row in records))
        self.assertTrue(
            all(
                row["accepted_outcome"]
                == {"accepted": None, "state": "unavailable"}
                for row in records
            )
        )
        self.assertTrue(all(row["error"] == {"kind": "timeout"} for row in records))

    def test_bounds_explicit_selection_existing_destination_and_escape_fail_closed(self) -> None:
        cases = [
            ("no-task", [] , ()),
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

    def test_example_manifest_is_tiny_and_valid(self) -> None:
        example = json.loads(EXAMPLE.read_text(encoding="utf-8"))
        self.assertEqual(example["schema_version"], "1.0.0")
        self.assertEqual([task["id"] for task in example["tasks"]], ["inspect-route"])


if __name__ == "__main__":
    unittest.main()
