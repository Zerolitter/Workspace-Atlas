#!/usr/bin/env python3
"""Focused contract tests for benchmark-evidence-export.py."""
from __future__ import annotations

import csv
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("benchmark-evidence-export.py")


class BenchmarkEvidenceExportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name) / "workspace"
        self.workspace.mkdir()
        self.source = self.workspace / "raw.ndjson"
        self.destination = self.workspace / "bundle"

    def run_export(self, source: Path | None = None, destination: Path | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--workspace", str(self.workspace),
             "--input", str(source or self.source), "--destination", str(destination or self.destination)],
            capture_output=True, text=True, check=False,
        )

    def write_records(self) -> list[dict[str, object]]:
        records = [
            {
                "schema_version": "1.0.0", "kind": "local-atlas-ab-observation",
                "task_id": "task-b", "repetition": 1, "arm": "on", "exit_code": 0,
                "accepted_outcome": {"accepted": None, "state": "unavailable"},
                "elapsed_ms": None, "tokens": None, "tool_calls": 2,
                "files_read": None, "source_bytes_read": 17, "atlas_route": "LIGHT",
                "atlas_runtime_ms": 4, "context_expansion": None, "error": None,
            },
            {
                "schema_version": "1.0.0", "kind": "local-atlas-ab-observation",
                "task_id": "task-a", "repetition": 1, "arm": "off", "exit_code": 7,
                "accepted_outcome": {"accepted": False, "state": "rejected"},
                "elapsed_ms": 12, "tokens": {"input": 3, "output": None, "total": None},
                "tool_calls": None, "files_read": 1, "source_bytes_read": None,
                "atlas_route": None, "atlas_runtime_ms": None,
                "context_expansion": {"records": None, "estimated_tokens": 9},
                "error": {"kind": "runner_exit", "detail": None},
            },
        ]
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        manifest = {
            "schema_version": "1.0.0",
            "raw_sha256": hashlib.sha256(payload.encode("utf-8")).hexdigest(),
            "raw_count": len(records),
            "expected_raw_count": len(records),
            "state": "complete",
        }
        (self.workspace / "harness-manifest.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )
        return records

    def test_exports_stable_bound_bundle_without_mutating_input(self) -> None:
        records = self.write_records()
        before = self.source.read_bytes()

        result = self.run_export()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.source.read_bytes(), before)
        self.assertEqual(sorted(path.name for path in self.destination.iterdir()), [
            "environment.json", "manifest.json", "raw.json", "results.csv", "summary.json"
        ])
        raw = json.loads((self.destination / "raw.json").read_text(encoding="utf-8"))
        self.assertEqual(raw["records"], records)
        summary = json.loads((self.destination / "summary.json").read_text(encoding="utf-8"))
        self.assertEqual(summary["counts"], {
            "accepted": 0, "failed": 1, "records": 2, "rejected": 1, "unavailable": 1
        })
        manifest = json.loads((self.destination / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["input"]["name"], "raw.ndjson")
        self.assertEqual(manifest["input"]["sha256"], hashlib.sha256(before).hexdigest())
        self.assertEqual(manifest["state"], "complete")
        self.assertEqual(manifest["counts"], summary["counts"])
        for name in ("environment.json", "raw.json", "summary.json", "results.csv"):
            self.assertEqual(manifest["files"][name]["sha256"], hashlib.sha256((self.destination / name).read_bytes()).hexdigest())
        csv_rows = list(csv.DictReader(io.StringIO((self.destination / "results.csv").read_text(encoding="utf-8"))))
        self.assertEqual([(row["task_id"], row["arm"]) for row in csv_rows], [("task-a", "off"), ("task-b", "on")])
        self.assertEqual(csv_rows[0]["accepted"], "false")
        self.assertEqual(csv_rows[1]["accepted"], "<null>")
        for path in self.destination.iterdir():
            self.assertTrue(path.read_bytes().endswith(b"\n"))

    def test_harness_identity_sidecar_mismatch_is_rejected_before_destination(self) -> None:
        records = self.write_records()
        (self.workspace / "harness-manifest.json").write_text(
            json.dumps(
                {
                    "schema_version": "1.0.0",
                    "state": "complete",
                    "raw_count": len(records),
                    "expected_raw_count": len(records),
                    "raw_sha256": "0" * 64,
                }
            ),
            encoding="utf-8",
        )

        result = self.run_export()

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_real_capacity_raw_and_summary_drive_counts_identity_and_state(self) -> None:
        source = self.workspace / "capacity-raw-observations-v1.ndjson"
        record = {
            "schema_version": "1.0.0",
            "kind": "capacity-raw-observation",
            "outcome": "required_provider_failure",
            "correctness": False,
            "accepted_outcome": None,
            "preparation_failure": "required provider failed",
            "repetition": 1,
        }
        raw = (json.dumps(record, sort_keys=True) + "\n").encode()
        source.write_bytes(raw)
        summary_source = self.workspace / "capacity-summary-v1.json"
        summary_source.write_text(
            json.dumps(
                {
                    "schema_version": "1.0.0",
                    "kind": "capacity-evidence-summary",
                    "raw_sha256": hashlib.sha256(raw).hexdigest(),
                    "attempted_records": 1,
                    "missing_records": 0,
                    "duplicate_records": 0,
                }
            ),
            encoding="utf-8",
        )

        result = self.run_export(source)

        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(
            (self.destination / "manifest.json").read_text(encoding="utf-8")
        )
        self.assertEqual(manifest["state"], "complete")
        self.assertEqual(manifest["counts"], {
            "accepted": 0,
            "failed": 1,
            "records": 1,
            "rejected": 1,
            "unavailable": 0,
        })
        self.assertEqual(
            manifest["identity"]["name"], "capacity-summary-v1.json"
        )
        self.assertEqual(
            manifest["identity"]["sha256"],
            hashlib.sha256(summary_source.read_bytes()).hexdigest(),
        )

    def test_json_wrapper_binds_partial_state_and_sanitizes_environment(self) -> None:
        wrapper = {
            "schema_version": "1.0.0", "state": "partial", "raw_count": 1,
            "records": [{"schema_version": "atlas-capacity-raw-v2", "accepted_outcome": None,
                         "failure": "provider_failed", "private": None}],
        }
        source = self.workspace / "atlas.json"
        source.write_text(json.dumps(wrapper), encoding="utf-8")
        environment = self.workspace / "environment-input.json"
        environment.write_text(json.dumps({
            "atlas_version": "2.0.0", "architecture": "x86_64",
            "HOME": "C:/Users/<user>/private", "UNRELATED_VALUE": "must-not-export",
        }), encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--workspace", str(self.workspace), "--input", str(source),
             "--destination", str(self.destination), "--environment", str(environment)],
            capture_output=True, text=True, check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        exported = (self.destination / "environment.json").read_text(encoding="utf-8")
        self.assertIn("2.0.0", exported)
        self.assertNotIn("must-not-export", exported)
        self.assertNotIn("Users", exported)
        manifest = json.loads((self.destination / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["state"], "partial")
        self.assertEqual(manifest["source_schema_versions"], ["atlas-capacity-raw-v2"])

    def test_rejects_malformed_mismatched_existing_and_escaping_paths(self) -> None:
        cases: list[tuple[str, Path, Path]] = []
        malformed = self.workspace / "malformed.ndjson"
        malformed.write_text('{"ok":true}\nnot-json\n', encoding="utf-8")
        cases.append(("malformed", malformed, self.workspace / "bad-one"))
        nonfinite = self.workspace / "nonfinite.ndjson"
        nonfinite.write_text('{"metric":NaN}\n', encoding="utf-8")
        cases.append(("nonfinite", nonfinite, self.workspace / "bad-nonfinite"))
        mismatch = self.workspace / "mismatch.json"
        mismatch.write_text(json.dumps({"state": "complete", "raw_count": 2, "records": [{}]}), encoding="utf-8")
        cases.append(("mismatch", mismatch, self.workspace / "bad-two"))
        self.source.write_text("{}\n", encoding="utf-8")
        self.destination.mkdir()
        cases.append(("existing", self.source, self.destination))
        cases.append(("escape", self.source, Path(self.temporary.name) / "outside"))

        for label, source, destination in cases:
            with self.subTest(label=label):
                result = self.run_export(source, destination)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(destination.exists()) if label not in ("existing",) else None

    def test_rejects_link_input_without_creating_destination(self) -> None:
        real = self.workspace / "real.ndjson"
        real.write_text("{}\n", encoding="utf-8")
        source = self.workspace / "linked.ndjson"
        try:
            source.symlink_to(real)
        except OSError:
            outside = Path(self.temporary.name) / "outside"
            outside.mkdir()
            (outside / "raw.ndjson").write_text("{}\n", encoding="utf-8")
            linked_directory = self.workspace / "linked"
            junction = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(linked_directory), str(outside)],
                capture_output=True,
                check=False,
            )
            if junction.returncode:
                self.skipTest("file links and directory junctions are unavailable")
            source = linked_directory / "raw.ndjson"

        result = self.run_export(source)

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_environment_omits_obvious_secret_bearing_allowlisted_values(self) -> None:
        self.source.write_text("{}\n", encoding="utf-8")
        environment = self.workspace / "environment-input.json"
        environment.write_text(
            json.dumps(
                {
                    "atlas_version": "password=hunter2",
                    "toolchain": "stable",
                }
            ),
            encoding="utf-8",
        )

        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--workspace",
                str(self.workspace),
                "--input",
                str(self.source),
                "--destination",
                str(self.destination),
                "--environment",
                str(environment),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        exported = (self.destination / "environment.json").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("password=hunter2", exported)
        self.assertEqual(
            json.loads(exported)["allowlisted"],
            {"toolchain": "stable"},
        )

    def test_harness_shaped_raw_without_manifest_is_partial_with_missing_provenance(
        self,
    ) -> None:
        self.source.write_text(
            json.dumps(
                {
                    "schema_version": "1.0.0",
                    "kind": "local-atlas-ab-observation",
                    "task_id": "task-a",
                    "repetition": 1,
                    "arm": "off",
                }
            )
            + "\n",
            encoding="utf-8",
        )

        result = self.run_export()

        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(
            (self.destination / "manifest.json").read_text(encoding="utf-8")
        )
        self.assertEqual(manifest["state"], "partial")
        self.assertEqual(
            manifest["missing_provenance"],
            sorted([
                "harness_identity",
                "harness_expected_count",
                "harness_pairing",
            ]),
        )

    def test_csv_record_json_preserves_failure_unknown_and_null_vs_missing(
        self,
    ) -> None:
        records = [
            {
                "schema_version": "1.0.0",
                "kind": "local-atlas-ab-observation",
                "task_id": "task-a",
                "repetition": 1,
                "arm": "off",
                "error": {
                    "kind": "runner_exit",
                    "detail": {"message": "specific failure", "code": 17},
                },
                "known_null": None,
                "unknown_extension": {"nested": [1, None, {"x": False}]},
            },
            {
                "schema_version": "1.0.0",
                "kind": "local-atlas-ab-observation",
                "task_id": "task-a",
                "repetition": 1,
                "arm": "on",
            },
        ]
        self.source.write_text(
            "".join(
                json.dumps(record, sort_keys=True) + "\n" for record in records
            ),
            encoding="utf-8",
        )

        result = self.run_export()

        self.assertEqual(result.returncode, 0, result.stderr)
        rows = list(
            csv.DictReader(
                io.StringIO(
                    (self.destination / "results.csv").read_text(
                        encoding="utf-8"
                    )
                )
            )
        )
        self.assertEqual(
            [json.loads(row["record_json"]) for row in rows],
            records,
        )
        self.assertIsNone(json.loads(rows[0]["record_json"])["known_null"])
        self.assertNotIn("known_null", json.loads(rows[1]["record_json"]))


if __name__ == "__main__":
    unittest.main()
