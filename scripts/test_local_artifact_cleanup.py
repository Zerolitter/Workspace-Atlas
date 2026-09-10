#!/usr/bin/env python3
"""Focused safety tests for local-artifact-cleanup.py."""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("local-artifact-cleanup.py")
SPEC = importlib.util.spec_from_file_location("local_artifact_cleanup", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
cleanup_module = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = cleanup_module
SPEC.loader.exec_module(cleanup_module)


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

    def test_retained_verdict_protects_generated_fixture_root_without_marker(self) -> None:
        verdict = self.workspace / "_generated_fixture" / "verdict.json"
        disposable = self.workspace / "_generated_fixture" / "temporary.bin"
        verdict.parent.mkdir(parents=True)
        disposable.write_bytes(b"delete")
        verdict.write_text(json.dumps({"accepted": True, "state": "accepted"}), encoding="utf-8")

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(report["proposed"], [])
        self.assertEqual([item["path"] for item in report["protected"]], [
            "_generated_fixture/temporary.bin",
            "_generated_fixture/verdict.json",
        ])
        self.assertEqual(report["deleted"], [])
        self.assertTrue(disposable.exists())
        self.assertTrue(verdict.exists())

    def test_unreadable_verdict_file_protects_generated_fixture_root(self) -> None:
        undecidable = self.workspace / "_generated_fixture" / "verdict.json"
        disposable = self.workspace / "_generated_fixture" / "temporary.bin"
        undecidable.parent.mkdir(parents=True)
        disposable.write_bytes(b"delete")
        undecidable.write_bytes(b"\xff\xfe not utf-8 \xc3\x28")

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(report["proposed"], [])
        self.assertEqual(sorted(item["path"] for item in report["protected"]), [
            "_generated_fixture/temporary.bin",
            "_generated_fixture/verdict.json",
        ])
        self.assertEqual(report["deleted"], [])
        self.assertTrue(disposable.exists())
        self.assertTrue(undecidable.exists())

    def test_unrecognized_decision_json_is_not_proposed_as_disposable(self) -> None:
        decision = self.workspace / "_generated_fixture" / "decision.json"
        decision.parent.mkdir(parents=True)
        decision.write_text(
            json.dumps({"accepted_outcome": {"accepted": True, "state": "accepted"}}),
            encoding="utf-8",
        )

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [item for item in report["proposed"] if item["path"] == "_generated_fixture/decision.json"],
            [],
        )
        self.assertTrue(
            any(
                item["path"] == "_generated_fixture/decision.json"
                and item.get("reason") == "unrecognized_generated_fixture_leaf"
                for item in report["unknown"]
            ),
            msg=repr(report),
        )
        self.assertEqual(report["deleted"], [])
        self.assertTrue(decision.exists())

    def test_unrecognized_provider_output_leaf_fails_closed(self) -> None:
        decision = self.workspace / "provider-output" / "decision.json"
        decision.parent.mkdir(parents=True)
        decision.write_text(
            json.dumps({"accepted_outcome": {"accepted": True, "state": "accepted"}}),
            encoding="utf-8",
        )

        result, report = self.run_cleanup("--apply")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [item for item in report["proposed"] if item["path"] == "provider-output/decision.json"],
            [],
        )
        self.assertTrue(
            any(
                item["path"] == "provider-output/decision.json"
                and item.get("reason") == "unrecognized_provider_output_leaf"
                for item in report["unknown"]
            ),
            msg=repr(report),
        )
        self.assertEqual(report["deleted"], [])
        self.assertTrue(decision.exists())

    def test_apply_refuses_to_unlink_directory_entry_changed_after_validation(self) -> None:
        disposable = self.workspace / "_generated_fixture" / "temporary.bin"
        disposable.parent.mkdir(parents=True)
        disposable.write_bytes(b"original")
        replacement = self.workspace / "replacement.bin"
        replacement.write_bytes(b"replacement")
        original_identity = cleanup_module._identity
        candidate_identity_calls = 0

        def replace_after_identity(metadata: object) -> tuple[int, int, int, int]:
            nonlocal candidate_identity_calls
            identity = original_identity(metadata)
            if identity[2] == len(b"original"):
                candidate_identity_calls += 1
                if candidate_identity_calls == 2:
                    disposable.unlink()
                    replacement.replace(disposable)
            return identity

        with mock.patch.object(
            cleanup_module,
            "_identity",
            side_effect=replace_after_identity,
        ):
            report = cleanup_module.audit(self.workspace, [], apply=True)

        self.assertEqual(report["deleted"], [])
        self.assertTrue(
            any(
                item["path"] == "_generated_fixture/temporary.bin"
                and "change" in str(item.get("reason", ""))
                for item in report["unknown"]
            ),
            msg=repr(report),
        )
        self.assertTrue(disposable.exists())
        self.assertIn(disposable.read_bytes(), (b"original", b"replacement"))

    def test_apply_does_not_report_entry_missing_at_delete_as_deleted(self) -> None:
        disposable = self.workspace / "_generated_fixture" / "temporary.bin"
        disposable.parent.mkdir(parents=True)
        disposable.write_bytes(b"temporary")

        with mock.patch.object(
            cleanup_module,
            "_safe_unlink_identity_bound",
            return_value=False,
        ):
            report = cleanup_module.audit(self.workspace, [], apply=True)

        self.assertEqual(report["deleted"], [])
        self.assertTrue(
            any(
                item["path"] == "_generated_fixture/temporary.bin"
                and item.get("reason") == "candidate_missing_before_deletion"
                for item in report["unknown"]
            ),
            msg=repr(report),
        )
        self.assertTrue(disposable.exists())


if __name__ == "__main__":
    unittest.main()
