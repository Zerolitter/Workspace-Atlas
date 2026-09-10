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
        task_manifest_sha256 = "6" * 64
        omp_version = "omp/18.1.11"
        executables = [
            {"bytes": 10, "file": "adapter.py", "name": "adapter", "sha256": "7" * 64, "version": None, "version_identity_sha256": None},
            {"bytes": 20, "file": "atlas-mcp.exe", "name": "atlas-mcp", "sha256": "8" * 64, "version": None, "version_identity_sha256": None},
            {"bytes": 30, "file": "omp.exe", "name": "omp", "sha256": "9" * 64, "version": omp_version, "version_identity_sha256": hashlib.sha256((json.dumps(omp_version, sort_keys=True, separators=(",", ":")) + "\n").encode()).hexdigest()},
        ]
        executable_identity = hashlib.sha256(
            (json.dumps(executables, sort_keys=True, separators=(",", ":")) + "\n").encode()
        ).hexdigest()
        identity = {
            "adapter_identity_sha256": "1" * 64,
            "command_identity_sha256": "3" * 64,
            "executable_identity_sha256": executable_identity,
            "model_identity_sha256": "4" * 64,
        }
        campaign_material = {
            **identity,
            "max_tasks": 1,
            "repetitions": 1,
            "task_manifest_sha256": task_manifest_sha256,
            "tasks": ["task-a"],
            "timeout_seconds": 5.0,
        }
        identity["campaign_identity_sha256"] = hashlib.sha256(
            (
                json.dumps(
                    campaign_material,
                    sort_keys=True,
                    separators=(",", ":"),
                    ensure_ascii=False,
                )
                + "\n"
            ).encode("utf-8")
        ).hexdigest()
        records = [
            {
                "schema_version": "1.0.0", "kind": "local-atlas-ab-observation",
                "task_id": "task-a", "task_identity_sha256": "5" * 64,
                "repetition": 1, "arm": "off", "arm_workspace": "arms/task-a/1/off",
                "model": "fixture-local", "timeout_seconds": 5.0, **identity,
                "exit_code": 7,
                "accepted_outcome": {"accepted": False, "state": "rejected"},
                "elapsed_ms": 12, "tokens": {"input": 3, "output": None, "total": None},
                "tool_calls": None, "files_read": 1, "source_bytes_read": None,
                "atlas_route": None, "atlas_runtime_ms": None,
                "context_expansion": {"records": None, "estimated_tokens": 9},
                "error": {"kind": "runner_exit", "detail": None},
            },
            {
                "schema_version": "1.0.0", "kind": "local-atlas-ab-observation",
                "task_id": "task-a", "task_identity_sha256": "5" * 64,
                "repetition": 1, "arm": "on", "arm_workspace": "arms/task-a/1/on",
                "model": "fixture-local", "timeout_seconds": 5.0, **identity,
                "exit_code": 0,
                "accepted_outcome": {"accepted": None, "state": "unavailable"},
                "elapsed_ms": None, "tokens": None, "tool_calls": 2,
                "files_read": None, "source_bytes_read": 17, "atlas_route": "LIGHT",
                "atlas_runtime_ms": 4, "context_expansion": None, "error": None,
            },
        ]
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        manifest = {
            "adapter": "fixture-json-stdio",
            "adapter_identity_sha256": identity["adapter_identity_sha256"],
            "campaign_identity_sha256": identity["campaign_identity_sha256"],
            "command_display": ["python", "runner.py"],
            "command_identity_sha256": identity["command_identity_sha256"],
            "executable_identity_sha256": identity["executable_identity_sha256"],
            "executables": executables,
            "expected_raw_count": len(records),
            "max_tasks": 1,
            "model": "fixture-local",
            "model_identity_sha256": identity["model_identity_sha256"],
            "raw_count": len(records),
            "raw_sha256": hashlib.sha256(payload.encode("utf-8")).hexdigest(),
            "repetitions": 1,
            "schema_version": "1.0.0",
            "state": "complete",
            "task_manifest": {"name": "tasks.json", "sha256": task_manifest_sha256},
            "tasks": ["task-a"],
            "timeout_seconds": 5.0,
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
        self.assertEqual(manifest["missing_provenance"], [])
        self.assertEqual(manifest["identity"]["kind"], "harness")
        for name in ("environment.json", "raw.json", "summary.json", "results.csv"):
            self.assertEqual(manifest["files"][name]["sha256"], hashlib.sha256((self.destination / name).read_bytes()).hexdigest())
        csv_rows = list(csv.DictReader(io.StringIO((self.destination / "results.csv").read_text(encoding="utf-8"))))
        self.assertEqual([(row["task_id"], row["arm"]) for row in csv_rows], [("task-a", "off"), ("task-a", "on")])
        self.assertEqual(csv_rows[0]["accepted"], "false")
        self.assertEqual(csv_rows[1]["accepted"], "<null>")
        for path in self.destination.iterdir():
            self.assertTrue(path.read_bytes().endswith(b"\n"))

    def test_incomplete_harness_sidecar_is_partial_with_explicit_missing_provenance(
        self,
    ) -> None:
        records = self.write_records()
        raw = self.source.read_bytes()
        (self.workspace / "harness-manifest.json").write_text(
            json.dumps({
                "schema_version": "1.0.0",
                "raw_sha256": hashlib.sha256(raw).hexdigest(),
                "raw_count": len(records),
                "expected_raw_count": len(records),
                "state": "complete",
            }),
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
            ["harness_identity", "harness_pairing"],
        )
        self.assertNotIn("identity", manifest)

    def test_missing_executable_provenance_cannot_be_complete(self) -> None:
        self.write_records()
        sidecar_path = self.workspace / "harness-manifest.json"
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        del sidecar["executables"]
        del sidecar["executable_identity_sha256"]
        sidecar_path.write_text(json.dumps(sidecar), encoding="utf-8")

        result = self.run_export()

        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(
            (self.destination / "manifest.json").read_text(encoding="utf-8")
        )
        self.assertEqual(manifest["state"], "partial")
        self.assertIn("harness_identity", manifest["missing_provenance"])
        self.assertNotIn("identity", manifest)

    def test_invalid_harness_pairing_is_partial_with_explicit_missing_provenance(
        self,
    ) -> None:
        records = self.write_records()
        records[1]["arm"] = "off"
        records[1]["arm_workspace"] = "arms/task-a/1/off"
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        sidecar_path = self.workspace / "harness-manifest.json"
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["raw_sha256"] = hashlib.sha256(payload.encode("utf-8")).hexdigest()
        sidecar_path.write_text(json.dumps(sidecar), encoding="utf-8")

        result = self.run_export()

        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(
            (self.destination / "manifest.json").read_text(encoding="utf-8")
        )
        self.assertEqual(manifest["state"], "partial")
        self.assertEqual(manifest["missing_provenance"], ["harness_pairing"])
        self.assertNotIn("identity", manifest)

    def test_rejects_consistent_but_recomputation_wrong_campaign_identity(
        self,
    ) -> None:
        records = self.write_records()
        wrong_campaign_identity = "f" * 64
        for record in records:
            record["campaign_identity_sha256"] = wrong_campaign_identity
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        sidecar_path = self.workspace / "harness-manifest.json"
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["campaign_identity_sha256"] = wrong_campaign_identity
        sidecar["raw_sha256"] = hashlib.sha256(payload.encode("utf-8")).hexdigest()
        sidecar_path.write_text(json.dumps(sidecar), encoding="utf-8")

        result = self.run_export()

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_rejects_executable_provenance_contradiction(self) -> None:
        self.write_records()
        sidecar_path = self.workspace / "harness-manifest.json"
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["executables"][0]["sha256"] = "a" * 64
        sidecar_path.write_text(json.dumps(sidecar), encoding="utf-8")

        result = self.run_export()

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_rejects_task_id_outside_harness_contract(self) -> None:
        records = self.write_records()
        for record in records:
            record["task_id"] = "../task-a"
            record["arm_workspace"] = (
                f"arms/../task-a/{record['repetition']}/{record['arm']}"
            )
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        sidecar_path = self.workspace / "harness-manifest.json"
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["tasks"] = ["../task-a"]
        campaign_material = {
            "adapter_identity_sha256": sidecar["adapter_identity_sha256"],
            "command_identity_sha256": sidecar["command_identity_sha256"],
            "executable_identity_sha256": sidecar["executable_identity_sha256"],
            "max_tasks": sidecar["max_tasks"],
            "model_identity_sha256": sidecar["model_identity_sha256"],
            "repetitions": sidecar["repetitions"],
            "task_manifest_sha256": sidecar["task_manifest"]["sha256"],
            "tasks": sidecar["tasks"],
            "timeout_seconds": sidecar["timeout_seconds"],
        }
        campaign_identity = hashlib.sha256(
            (
                json.dumps(
                    campaign_material,
                    sort_keys=True,
                    separators=(",", ":"),
                    ensure_ascii=False,
                )
                + "\n"
            ).encode("utf-8")
        ).hexdigest()
        sidecar["campaign_identity_sha256"] = campaign_identity
        for record in records:
            record["campaign_identity_sha256"] = campaign_identity
        payload = "".join(json.dumps(row) + "\n" for row in records)
        self.source.write_text(payload, encoding="utf-8", newline="\n")
        sidecar["raw_sha256"] = hashlib.sha256(payload.encode("utf-8")).hexdigest()
        sidecar_path.write_text(json.dumps(sidecar), encoding="utf-8")

        result = self.run_export()

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

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
        self.assertNotIn("identity", manifest)

    def test_generic_raw_ndjson_is_not_misclassified_as_harness_output(
        self,
    ) -> None:
        self.source.write_text(
            json.dumps(
                {
                    "schema_version": "custom-observation-v1",
                    "kind": "custom-observation",
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
        self.assertEqual(manifest["state"], "complete")
        self.assertEqual(manifest["missing_provenance"], [])

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
        self.assertEqual(rows[0]["error_kind"], "runner_exit")


if __name__ == "__main__":
    unittest.main()
