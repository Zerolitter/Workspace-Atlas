#!/usr/bin/env python3
"""Focused safety tests for local-artifact-cleanup.py."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("local-artifact-cleanup.py")


class LocalArtifactCleanupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name) / "workspace"
        self.workspace.mkdir()

    def run_cleanup(self, *arguments: str) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--workspace", str(self.workspace), *arguments],
            capture_output=True, text=True, check=False,
        )
        return result, json.loads(result.stdout) if result.stdout else {}

    def test_default_dry_run_is_deterministic_and_does_not_delete(self) -> None:
        disposable = self.workspace / "target" / "debug" / "build.o"
        disposable.parent.mkdir(parents=True)
        disposable.write_bytes(b"1234")

        first, report = self.run_cleanup()
        second, repeated = self.run_cleanup()

        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(report, repeated)
        self.assertEqual(report["mode"], "dry_run")
        self.assertEqual(report["proposed"], [
            {"bytes": 4, "class": "cargo_build_output", "path": "target/debug/build.o"}
        ])
        self.assertEqual(report["deleted"], [])
        self.assertTrue(disposable.exists())

    def test_apply_deletes_only_disposable_files_and_is_idempotent(self) -> None:
        disposable = self.workspace / ".atlas-provider-tmp" / "provider.tmp"
        disposable.parent.mkdir()
        disposable.write_bytes(b"abc")

        applied, report = self.run_cleanup("--apply")
        repeated, empty = self.run_cleanup("--apply")

        self.assertEqual(applied.returncode, 0, applied.stderr)
        self.assertEqual(report["deleted"], [
            {"bytes": 3, "class": "provider_temp_output", "path": ".atlas-provider-tmp/provider.tmp"}
        ])
        self.assertEqual(report["proposed"], report["deleted"])
        self.assertFalse(disposable.exists())
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        self.assertEqual(empty["deleted"], [])
        self.assertEqual(empty["proposed"], [])

    def test_containment_escape_and_links_are_unknown_and_untouched(self) -> None:
        outside = Path(self.temporary.name) / "outside"
        outside.mkdir()
        payload = outside / "keep.txt"
        payload.write_text("keep", encoding="utf-8")
        link = self.workspace / "provider-output"
        try:
            link.symlink_to(outside, target_is_directory=True)
        except OSError:
            junction = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(link), str(outside)],
                capture_output=True,
                check=False,
            )
            if junction.returncode:
                self.skipTest("directory links and junctions are unavailable")

        result, report = self.run_cleanup(
            "--benchmark-scratch", "../outside", "--apply"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([item["path"] for item in report["unknown"]], [
            "../outside", "provider-output"
        ])
        self.assertTrue(payload.exists())
        self.assertTrue(link.exists())

    def test_candidate_ancestor_junction_is_unknown_and_never_traversed(self) -> None:
        outside = Path(self.temporary.name) / "outside-target"
        (outside / "debug").mkdir(parents=True)
        payload = outside / "debug" / "keep.bin"
        payload.write_bytes(b"outside")
        target = self.workspace / "target"
        try:
            target.symlink_to(outside, target_is_directory=True)
        except OSError:
            junction = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(target), str(outside)],
                capture_output=True,
                check=False,
            )
            if junction.returncode:
                self.skipTest("directory links and junctions are unavailable")

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(report["proposed"], [])
        self.assertEqual(
            [item["path"] for item in report["unknown"]], ["target"]
        )
        self.assertTrue(payload.exists())

    def test_explicit_scratch_name_cannot_reclassify_workspace_content(self) -> None:
        source = self.workspace / "scripts" / "keep.py"
        source.parent.mkdir()
        source.write_text("keep = True\n", encoding="utf-8")

        result, report = self.run_cleanup(
            "--benchmark-scratch", "scripts", "--apply"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [item["path"] for item in report["unknown"]], ["scripts"]
        )
        self.assertTrue(source.exists())

    def test_protected_evidence_and_unknown_target_content_are_never_deleted(self) -> None:
        evidence = self.workspace / "_generated_fixture" / "raw-observations.ndjson"
        preservation = self.workspace / "_generated_fixture" / "retained" / ".atlas-preserve"
        disposable = self.workspace / "_generated_fixture" / "temporary.bin"
        unknown = self.workspace / "target" / "custom-profile" / "keep.bin"
        preservation.parent.mkdir(parents=True)
        evidence.parent.mkdir(parents=True, exist_ok=True)
        unknown.parent.mkdir(parents=True)
        evidence.write_text('{"accepted":null}\n', encoding="utf-8")
        preservation.write_text("", encoding="utf-8")
        (preservation.parent / "accepted-outcome.json").write_text("{}\n", encoding="utf-8")
        disposable.write_bytes(b"delete")
        unknown.write_bytes(b"unknown")
        benchmark_summary = (
            self.workspace / "_generated_fixture" / "capacity-summary-v1.json"
        )
        run_attestation = (
            self.workspace / "_generated_fixture" / "run-attestation-42.json"
        )
        benchmark_result = (
            self.workspace / "_generated_fixture" / "benchmark-result.json"
        )
        benchmark_summary.write_text("{}\n", encoding="utf-8")
        run_attestation.write_text("{}\n", encoding="utf-8")
        benchmark_result.write_text("{}\n", encoding="utf-8")

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([item["path"] for item in report["deleted"]], [
            "_generated_fixture/temporary.bin"
        ])
        self.assertEqual([item["path"] for item in report["protected"]], [
            "_generated_fixture/benchmark-result.json",
            "_generated_fixture/capacity-summary-v1.json",
            "_generated_fixture/raw-observations.ndjson",
            "_generated_fixture/retained/.atlas-preserve",
            "_generated_fixture/retained/accepted-outcome.json",
            "_generated_fixture/run-attestation-42.json",
        ])
        self.assertEqual([item["path"] for item in report["unknown"]], [
            "target/custom-profile"
        ])
        self.assertTrue(evidence.exists())
        self.assertTrue(preservation.parent.exists())
        self.assertTrue(unknown.exists())
        self.assertTrue(benchmark_summary.exists())
        self.assertTrue(run_attestation.exists())
        self.assertTrue(benchmark_result.exists())


if __name__ == "__main__":
    unittest.main()
