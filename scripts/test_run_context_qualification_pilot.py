#!/usr/bin/env python3
"""Focused tests for run-context-qualification-pilot.py."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import warnings
from types import SimpleNamespace
from unittest import mock

SCRIPT = Path(__file__).with_name("run-context-qualification-pilot.py")
SPEC = importlib.util.spec_from_file_location("atlas_context_pilot_runner", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("executed pilot runner import unavailable")
runner = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = runner
SPEC.loader.exec_module(runner)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def cargo_output(names: list[str], failed: set[str]) -> str:
    passed = len(names) - len(failed)
    result = "FAILED" if failed else "ok"
    lines = [f"running {len(names)} tests"]
    for name in names:
        state = "FAILED" if name in failed else "ok"
        lines.append(f"test {name} ... {state}")
    if failed:
        lines.append("panicked at 'trusted runtime assertion'")
    lines.append(
        f"test result: {result}. {passed} passed; {len(failed)} failed; "
        "0 ignored; 0 measured; 0 filtered out"
    )
    return "\n".join(lines) + "\n"

def synthetic_process_record(
    argv: list[str],
    cwd: Path,
    *,
    stdout: str = "",
    stderr: str = "",
    exit_code: int | None = 0,
    transport_failure: str | None = None,
    name: str | None = None,
    checkout: str | None = None,
) -> dict[str, object]:
    record: dict[str, object] = {
        "argv": argv,
        "resolved_argv": list(argv),
        "cwd": str(cwd),
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "stdout_sha256": sha256(stdout.encode()),
        "stderr_sha256": sha256(stderr.encode()),
        "duration_us": 1,
        "transport_failure": transport_failure,
        "failure_kind": None if transport_failure is None else "bounded_transport",
        "containment_cleanup_completed": True,
        "stream_encoding": "UTF-8 decoded transport output re-encoded for retention",
    }
    if name is not None:
        record["name"] = name
    if checkout is not None:
        record["checkout"] = checkout
    return record


def synthetic_fence_records(
    checkout_name: str,
    checkout: Path,
    git_executable: str,
    *,
    modified: set[str],
    untracked: set[str],
) -> list[dict[str, object]]:
    outputs = {
        "head": runner.FROZEN_COMMIT + "\n",
        "branch": "",
        "porcelain": "".join(
            [f" M {path}\n" for path in sorted(modified)]
            + [f"?? {path}\n" for path in sorted(untracked)]
        ),
        "diff": "\n".join(sorted(modified)) + ("\n" if modified else ""),
        "cached": "",
        "untracked": "\n".join(sorted(untracked)) + ("\n" if untracked else ""),
        "ignored": "",
    }
    return [
        synthetic_process_record(
            [git_executable, *runner.GIT_FENCE_COMMANDS[name]],
            checkout,
            stdout=outputs[name],
            name=name,
            checkout=checkout_name,
        )
        for name in runner.GIT_FENCE_COMMANDS
    ]

def canonical_lockfile_bytes() -> bytes:
    physical = (SCRIPT.parent.parent / runner.LOCKFILE_PATH).read_bytes()
    canonical = physical.replace(b"\r\n", b"\n")
    if (
        b"\r" in canonical
        or len(canonical) != runner.LOCKFILE_CANONICAL_BYTES
        or sha256(canonical) != runner.LOCKFILE_CANONICAL_SHA256
    ):
        raise AssertionError("workspace Cargo.lock does not match frozen blob")
    return canonical


def install_protected_lockfiles(
    recipe: dict[str, object],
    checkouts: dict[str, Path],
) -> dict[str, dict[str, object]]:
    canonical = canonical_lockfile_bytes()
    result = {}
    for index, checkout_name in enumerate(runner.FROZEN_CHECKOUT_ROLES):
        checkout = checkouts[checkout_name]
        data = (
            canonical
            if index % 2 == 0
            else canonical.replace(b"\n", b"\r\n")
        )
        (checkout / runner.LOCKFILE_PATH).write_bytes(data)
        result[checkout_name] = runner._protected_lockfile_identity(
            checkout, recipe, checkout_name
        )
    return result



class ExecutedPilotRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_registry_is_exactly_two_tasks_with_frozen_fixture_hashes(self) -> None:
        registry = runner.load_registry()
        self.assertEqual(
            set(registry["tasks"]),
            {
                "single-file-canonical-id-whitespace",
                "configuration-project-marker-validation",
            },
        )
        self.assertEqual(
            registry["registry_sha256"],
            "2a406f15fedb0398f6ebf7b0e8c1d14e6dae346dbe86c2a146d6d40068088bc6",
        )
        expected = {
            "single-file-canonical-id-whitespace": (
                "5efd1e64f959da2fb4b3652dd5340685fcdcc5c837458073e49757cb77681cbc",
                "8fb34a5bd489d7e3bbfb74dd244d4daa5b9170d570493de5e90585cd74de38b1",
            ),
            "configuration-project-marker-validation": (
                "212cd7b95847b2bf8aed60d16f6f5b6f56e05c6e3449aa58b304588adff079be",
                "8a39b9465d87674d02cec34adb2100271034855710a049b7bd86723f10a7424b",
            ),
        }
        for task_id, (probe_hash, contract_hash) in expected.items():
            recipe = runner.trusted_recipe(registry, task_id)
            self.assertEqual(sha256(runner.decode_probe(recipe)), probe_hash)
            self.assertEqual(recipe["validator_contract_file_sha256"], contract_hash)
            self.assertEqual(len(recipe["acceptance_names"]), 13 if "whitespace" in task_id else 14)
            self.assertNotIn(
                runner.LOCKFILE_PATH, recipe["protected_file_sha256"]
            )
            self.assertEqual(
                recipe["protected_lockfile_contract"][
                    "canonical_blob_sha256"
                ],
                runner.LOCKFILE_CANONICAL_SHA256,
            )
        with self.assertRaisesRegex(runner.PilotError, "unsupported task"):
            runner.trusted_recipe(registry, "refactor-route-escalation-predicate")

    def test_registry_fixture_or_unknown_field_tampering_fails_closed(self) -> None:
        value = json.loads(runner.REGISTRY_PATH.read_text(encoding="utf-8"))
        value["unexpected"] = True
        with self.assertRaisesRegex(runner.PilotError, "registry fields"):
            runner.validate_registry(value, b"{}")

        registry = runner.load_registry()
        recipe = copy.deepcopy(registry["tasks"]["single-file-canonical-id-whitespace"])
        recipe["probe_sha256"] = "0" * 64
        with self.assertRaisesRegex(runner.PilotError, "trusted probe hash"):
            runner.decode_probe(recipe)

        self.assertEqual(
            runner._validate_declared_configuration(
                b'{"model":null}\n', "model"
            ),
            {"model": None},
        )
        for document in (
            b'{"model":null,"provider":"x"}\n',
            b'{"model":7}\n',
        ):
            with self.subTest(document=document):
                with self.assertRaises(runner.PilotError):
                    runner._validate_declared_configuration(document, "model")

    def test_private_text_preserves_runtime_absolute_paths(self) -> None:
        runtime_path = (self.root / "private-scratch").resolve()
        data = (
            f"Use the configured private scratch path {runtime_path} exactly.\n"
        ).encode()
        instruction = self.root / "private-instruction.txt"
        instruction.write_bytes(data)
        self.assertIsNotNone(
            runner.PUBLIC_HYGIENE.find_forbidden_content(data)
        )
        self.assertEqual(
            runner._bounded_private_text_file(
                instruction, "private instruction"
            ),
            data,
        )
        with self.assertRaisesRegex(runner.PilotError, "unsafe public"):
            runner._safe_public_text_file(
                instruction, "public fixture"
            )
        nul_text = self.root / "nul-instruction.txt"
        nul_text.write_bytes(data + b"\0")
        with self.assertRaisesRegex(runner.PilotError, "NUL"):
            runner._bounded_private_text_file(
                nul_text, "private instruction"
            )

    def test_lockfile_accepts_only_exact_lf_or_deterministic_crlf(self) -> None:
        recipe = runner.load_registry()["tasks"][
            "single-file-canonical-id-whitespace"
        ]
        canonical = canonical_lockfile_bytes()
        cases = {
            "canonical_lf": canonical,
            "deterministic_crlf": canonical.replace(b"\n", b"\r\n"),
        }
        for representation, data in cases.items():
            with self.subTest(representation=representation):
                checkout = self.root / representation
                checkout.mkdir()
                (checkout / runner.LOCKFILE_PATH).write_bytes(data)
                identity = runner._protected_lockfile_identity(
                    checkout, recipe, "baseline_checkout"
                )
                self.assertEqual(
                    identity["representation"], representation
                )
                self.assertEqual(identity["bytes"], len(data))
                self.assertEqual(identity["sha256"], sha256(data))
                self.assertEqual(
                    identity["canonical_blob_sha256"],
                    runner.LOCKFILE_CANONICAL_SHA256,
                )

        rejected = {
            "mixed": canonical.replace(b"\n", b"\r\n", 1),
            "changed": canonical + b"x",
        }
        for label, data in rejected.items():
            with self.subTest(rejected=label):
                checkout = self.root / f"rejected-{label}"
                checkout.mkdir()
                (checkout / runner.LOCKFILE_PATH).write_bytes(data)
                with self.assertRaisesRegex(
                    runner.PilotError, "representation is unsupported"
                ):
                    runner._protected_lockfile_identity(
                        checkout, recipe, "baseline_checkout"
                    )

    def test_freeze_manifest_retains_private_instruction_exactly(self) -> None:
        recipe = runner.load_registry()["tasks"][
            "single-file-canonical-id-whitespace"
        ]
        scratch = self.root / "freeze-scratch"
        scratch.mkdir()
        baseline = scratch / "baseline"
        model = scratch / "model"
        validation = scratch / "validation"
        acquisition = self.root / "freeze-acquisition"
        evidence = self.root / "freeze-evidence"
        target = self.root / "freeze-target"
        for path in (
            baseline, model, validation, acquisition, evidence, target,
        ):
            path.mkdir()
        frozen_lockfiles = install_protected_lockfiles(
            recipe,
            {
                "baseline_checkout": baseline,
                "model_checkout": model,
                "validation_checkout": validation,
                "acquisition_checkout": acquisition,
            },
        )
        runtime_path = (self.root / "configured-private-scratch").resolve()
        instruction_bytes = (
            "# Runtime operator instructions\n"
            f"Use private scratch {runtime_path} without rewriting it.\n"
        ).encode()
        instruction = self.root / "operator-instruction.txt"
        instruction.write_bytes(instruction_bytes)
        model_configuration = self.root / "model-configuration.json"
        model_configuration.write_bytes(b'{"model":null}\n')
        agent_configuration = self.root / "agent-configuration.json"
        agent_configuration.write_bytes(b'{"agent":null}\n')
        catalogue = self.root / "freeze-catalogue.sqlite"
        catalogue.write_bytes(b"synthetic catalogue")
        executable = Path(sys.executable).resolve()
        args = SimpleNamespace(
            task_id="single-file-canonical-id-whitespace",
            git_executable=executable,
            cargo_executable=executable,
            atlas_executable=executable,
            omp_executable=executable,
            instruction_file=instruction,
            instruction_sha256=sha256(instruction_bytes),
            model_configuration=model_configuration,
            agent_configuration=agent_configuration,
            acquisition_checkout=acquisition,
            catalogue=catalogue,
            baseline_checkout=baseline,
            model_checkout=model,
            validation_checkout=validation,
            scratch_root=scratch,
            target_root=target,
            evidence_root=evidence,
        )

        def capture_only_runner(
            argv: list[str], **kwargs: object
        ) -> subprocess.CompletedProcess[str]:
            del kwargs
            stdout = (
                runner.FROZEN_COMMIT + "\n"
                if "rev-parse" in argv
                else ""
            )
            return subprocess.CompletedProcess(argv, 0, stdout, "")

        with mock.patch.object(runner, "_hash_paths") as hash_paths:
            manifest, manifest_path = runner.freeze_manifest(
                args, process_runner=capture_only_runner
            )
        self.assertEqual(hash_paths.call_count, 9)
        self.assertEqual(
            (evidence / "frozen-instruction.txt").read_bytes(),
            instruction_bytes,
        )
        self.assertTrue(
            (evidence / "model-prompt.md").read_bytes().startswith(
                instruction_bytes
            )
        )
        self.assertEqual(
            manifest["configuration"]["instruction"]["sha256"],
            sha256(instruction_bytes),
        )
        self.assertEqual(
            manifest["protected_lockfiles"], frozen_lockfiles
        )
        self.assertEqual(
            runner._load_manifest(manifest_path)["manifest_sha256"],
            manifest["manifest_sha256"],
        )
        self.assertEqual(
            set(target.iterdir()),
            {
                target / "baseline",
                target / "validation",
            },
        )

    def test_layout_rejects_overlap_outside_scratch_and_links(self) -> None:
        scratch = self.root / "scratch"
        scratch.mkdir()
        baseline = scratch / "baseline"
        model = scratch / "model"
        validation = scratch / "validation"
        for path in (baseline, model, validation):
            path.mkdir()
        evidence = self.root / "evidence"
        target = self.root / "target"
        trusted = self.root / "trusted"
        acquisition = self.root / "acquisition"
        for path in (evidence, target, trusted, acquisition):
            path.mkdir()
        layout = {
            "scratch_root": scratch,
            "baseline_checkout": baseline,
            "model_checkout": model,
            "validation_checkout": validation,
            "evidence_root": evidence,
            "target_root": target,
            "trusted_root": trusted,
            "acquisition_checkout": acquisition,
        }
        runner.validate_path_layout(layout)

        overlapped = dict(layout)
        overlapped["model_checkout"] = baseline
        with self.assertRaisesRegex(runner.PilotError, "distinct"):
            runner.validate_path_layout(overlapped)

        outside = dict(layout)
        outside["validation_checkout"] = self.root / "outside"
        outside["validation_checkout"].mkdir()
        with self.assertRaisesRegex(runner.PilotError, "inside scratch"):
            runner.validate_path_layout(outside)

        link = scratch / "linked"
        if os.name == "nt":
            created = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(link), str(model)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(created.returncode, 0, created.stderr)
        else:
            os.symlink(model, link, target_is_directory=True)
        linked = dict(layout)
        linked["model_checkout"] = link
        with self.assertRaisesRegex(runner.PilotError, "link or reparse"):
            runner.validate_path_layout(linked)


        descendant_target = self.root / "descendant-target"
        descendant_target.mkdir()
        descendant_link = baseline / "tests"
        if os.name == "nt":
            created = subprocess.run(
                [
                    "cmd", "/c", "mklink", "/J",
                    str(descendant_link), str(descendant_target),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(created.returncode, 0, created.stderr)
        else:
            os.symlink(descendant_target, descendant_link)
        with self.assertRaisesRegex(runner.PilotError, "link or reparse"):
            runner._checkout_member(
                baseline,
                "tests/probe.rs",
                "synthetic probe",
                must_exist=False,
            )

        checkout_executable = baseline / "tool.exe"
        checkout_executable.write_bytes(b"tool")
        with self.assertRaisesRegex(runner.PilotError, "mutable checkout"):
            runner.reject_mutable_checkout_authorities(
                {"tool executable": checkout_executable},
                [baseline, model, validation],
            )
    def test_git_fence_rejects_staged_unexpected_and_ignored_build_inputs(self) -> None:
        runner.validate_git_fence(
            head=runner.FROZEN_COMMIT,
            branch="",
            porcelain="",
            diff_names=[],
            cached_names=[],
            untracked_names=[],
            ignored_porcelain="",
            allowed_modified=set(),
            allowed_untracked=set(),
        )
        cases = [
            ({"branch": "main"}, "detached"),
            ({"cached_names": ["src/config.rs"]}, "staged"),
            ({"diff_names": ["README.md"]}, "modified"),
            ({"untracked_names": ["extra.rs"]}, "untracked"),
            ({
                "porcelain": " M README.md\n",
                "diff_names": ["src/config.rs"],
                "allowed_modified": {"src/config.rs"},
            }, "porcelain facts"),
            ({"ignored_porcelain": "!! .cargo/config.toml\n"}, "build-affecting"),
            ({"ignored_porcelain": "!! .cargo/\n"}, "build-affecting"),
        ]
        base = {
            "head": runner.FROZEN_COMMIT,
            "branch": "",
            "porcelain": "",
            "diff_names": [],
            "cached_names": [],
            "untracked_names": [],
            "ignored_porcelain": "",
            "allowed_modified": set(),
            "allowed_untracked": set(),
        }
        for mutation, message in cases:
            with self.subTest(mutation=mutation):
                values = {**base, **mutation}
                with self.assertRaisesRegex(runner.PilotError, message):
                    runner.validate_git_fence(**values)

        protected = self.root / "protected"
        protected.mkdir()
        protected_file = protected / "Cargo.toml"
        protected_file.write_bytes(b"frozen")
        expected = sha256(b"frozen")
        runner._hash_paths(
            protected, {"Cargo.toml": expected}, "retry protected file"
        )
        protected_file.write_bytes(b"changed")
        with self.assertRaisesRegex(runner.PilotError, "hash mismatch"):
            runner._hash_paths(
                protected, {"Cargo.toml": expected}, "retry protected file"
            )

    def test_named_results_require_complete_exact_partition(self) -> None:
        names = ["alpha", "beta", "gamma"]
        observed = runner.parse_cargo_test_output(
            cargo_output(names, {"beta"}), "", names
        )
        self.assertEqual(observed["passed"], ["alpha", "gamma"])
        self.assertEqual(observed["failed"], ["beta"])
        self.assertTrue(observed["runtime_assertion_evidence"])

        malformed = [
            cargo_output(names[:-1], set()),
            cargo_output(names + ["alpha"], set()),
            cargo_output(names, set()).replace("0 ignored", "1 ignored"),
            cargo_output(names, set()).replace("0 filtered out", "1 filtered out"),
            "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
            "error: could not compile workspace_atlas\n",
        ]
        for output in malformed:
            with self.subTest(output=output[:40]):
                with self.assertRaises(runner.PilotError):
                    runner.parse_cargo_test_output(output, "", names)

    def test_negative_control_distinguishes_runtime_from_compile_failure(self) -> None:
        recipe = runner.load_registry()["tasks"]["configuration-project-marker-validation"]
        output = cargo_output(recipe["acceptance_names"], set(recipe["baseline_failed_names"]))
        observed = runner.parse_cargo_test_output(output, "", recipe["acceptance_names"])
        runner.validate_baseline_control(recipe, 101, observed, containment_verified=True)
        with self.assertRaisesRegex(runner.PilotError, "runtime assertion"):
            runner.validate_baseline_control(
                recipe,
                101,
                {**observed, "runtime_assertion_evidence": False},
                containment_verified=True,
            )
        with self.assertRaisesRegex(runner.PilotError, "named test output"):
            runner.parse_cargo_test_output(
                "error: compile failed", "", recipe["acceptance_names"]
            )

    def test_external_build_configuration_hierarchy_is_complete(self) -> None:
        cargo_home = self.root / "cargo-home"
        rustup_home = self.root / "rustup-home"
        baseline = self.root / "baseline-cargo"
        validation = self.root / "validation-cargo"
        for path in (cargo_home, rustup_home, baseline, validation):
            path.mkdir()
        environment = dict(os.environ)
        environment["CARGO_HOME"] = str(cargo_home)
        environment["RUSTUP_HOME"] = str(rustup_home)
        cargo_cwds = {
            "baseline_checkout": baseline,
            "validation_checkout": validation,
        }
        frozen = runner._external_build_configuration(
            environment, cargo_cwds
        )
        ancestor_config = self.root / ".cargo" / "config.toml"
        matching = [
            item
            for item in frozen
            if Path(item["path"]) == ancestor_config
        ]
        self.assertEqual(len(matching), 1)
        self.assertFalse(matching[0]["exists"])
        manifest = {
            "paths": {
                "baseline_checkout": str(baseline),
                "validation_checkout": str(validation),
            },
            "environment": {
                "qualified_inherited_toolchain":
                    runner._environment_snapshot(environment),
                "external_configuration": frozen,
                "mutated_by_runner": False,
            },
        }
        with mock.patch.dict(os.environ, environment, clear=True):
            runner._manifest_environment(manifest)
            ancestor_config.parent.mkdir()
            ancestor_config.write_text(
                "[build]\ntarget-dir='target'\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(
                runner.PilotError, "configuration set changed"
            ):
                runner._manifest_environment(manifest)
            added = runner._external_build_configuration(
                environment, cargo_cwds
            )
            manifest["environment"]["external_configuration"] = added
            ancestor_config.write_text(
                "[build]\ntarget-dir='other'\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(
                runner.PilotError, "configuration set changed"
            ):
                runner._manifest_environment(manifest)
            ancestor_config.write_text(
                "[build]\ntarget-dir='target'\n", encoding="utf-8"
            )
            ancestor_config.unlink()
            with self.assertRaisesRegex(
                runner.PilotError, "configuration set changed"
            ):
                runner._manifest_environment(manifest)
            ancestor_config.write_text(
                'include = ["shared.toml"]\n', encoding="utf-8"
            )
            with self.assertRaisesRegex(runner.PilotError, "include"):
                runner._external_build_configuration(
                    environment, cargo_cwds
                )
        with self.assertRaisesRegex(runner.PilotError, "overlap"):
            runner._external_build_configuration(
                environment,
                {
                    "baseline_checkout": baseline,
                    "validation_checkout": baseline,
                },
            )

    def test_finite_retry_requires_exact_results_and_owned_child_cleanup(self) -> None:
        names = ["alpha", "beta"]
        observed = runner.parse_cargo_test_output(cargo_output(names, set()), "", names)
        first = {
            "argv": ["cargo", "test"],
            "cwd": "checkout",
            "exit_code": 0,
            "transport_failure": "process owner retained 1 process(es) after root exit",
            "containment_cleanup_completed": True,
            "observed": observed,
        }
        second = {**first, "transport_failure": None}
        result = runner.evaluate_cargo_attempts(
            [first, second],
            expected_passed=set(names),
            expected_failed=set(),
            expected_exit=0,
        )
        self.assertEqual(len(result["attempts"]), 2)
        self.assertTrue(result["successful_transport"])

        for changed in (
            [{**first, "containment_cleanup_completed": False}, second],
            [{**first, "transport_failure": "timeout"}, second],
            [{**first, "argv": ["cargo", "other"]}, second],
            [first, second, second],
        ):
            with self.subTest(changed=changed):
                with self.assertRaises(runner.PilotError):
                    runner.evaluate_cargo_attempts(
                        changed,
                        expected_passed=set(names),
                        expected_failed=set(),
                        expected_exit=0,
                    )

    def test_return_code_policy_and_launch_failures_are_captured(self) -> None:
        high_code = 4_000_000_001

        def high_runner(*args: object, **kwargs: object) -> subprocess.CompletedProcess[str]:
            del args, kwargs
            return subprocess.CompletedProcess(["synthetic"], high_code, "out", "err")

        result, record = runner._process_record(
            ["synthetic"],
            self.root,
            environment=dict(os.environ),
            timeout=1,
            output_limit=1024,
            process_runner=high_runner,
        )
        self.assertEqual(result.returncode, high_code)
        self.assertEqual(record["exit_code"], high_code)
        self.assertIsNone(record["transport_failure"])

        def missing_runner(*args: object, **kwargs: object) -> object:
            del args, kwargs
            raise FileNotFoundError("synthetic executable missing")

        result, record = runner._process_record(
            ["missing"],
            self.root,
            environment=dict(os.environ),
            timeout=1,
            output_limit=1024,
            process_runner=missing_runner,
        )
        self.assertIsNone(result.returncode)
        self.assertEqual(record["failure_kind"], "launch_or_stream")
        self.assertIn("FileNotFoundError", record["transport_failure"])

    def test_usage_aggregation_preserves_nullable_and_partial_fields(self) -> None:
        messages = [
            {
                "provider": "provider-a",
                "model": "model-a",
                "usage": {"input": 10, "output": 2, "cacheRead": 3},
                "content": [{"type": "toolCall", "name": "read"}],
            },
            {
                "provider": "provider-a",
                "model": "model-a",
                "usage": {"input": 5, "output": None, "cacheRead": 4},
                "content": [{"type": "text", "text": "done"}],
            },
        ]
        summary = runner.aggregate_omp_messages(messages)
        self.assertEqual(summary["provider_labels"], ["provider-a"])
        self.assertEqual(summary["model_labels"], ["model-a"])
        self.assertEqual(summary["usage"]["input"]["value"], 15)
        self.assertIsNone(summary["usage"]["output"]["value"])
        self.assertEqual(summary["usage"]["output"]["observed_partial_sum"], 2)
        self.assertEqual(summary["tool_requests"], 1)
        self.assertIsNone(summary["exact_source_bytes"])
        self.assertIsNone(summary["reasoning_use"])
        for invalid in (-1, 1.5, True):
            with self.subTest(invalid_usage=invalid):
                changed = copy.deepcopy(messages)
                changed[0]["usage"]["input"] = invalid
                with self.assertRaisesRegex(
                    runner.PilotError, "nonnegative integer"
                ):
                    runner.aggregate_omp_messages(changed)

    def test_persistent_omp_messages_reconcile_with_stdout_completion(self) -> None:
        completed = {
            "responseId": "response-1",
            "provider": "provider-a",
            "model": "model-a",
            "role": "assistant",
            "completedAt": 123,
            "stopReason": "toolUse",
            "usage": {
                "input": 5,
                "output": 2,
                "totalTokens": 7,
                "reasoningTokens": 1,
                "cacheRead": 3,
                "cacheWrite": 0,
            },
            "content": [{
                "type": "toolCall",
                "id": "call-1",
                "name": "read",
            }],
        }
        session = b"".join([
            runner.canonical_document_bytes({
                "type": "message",
                "message": {"role": "user", "content": []},
            }),
            runner.canonical_document_bytes({
                "type": "message",
                "message": {
                    **completed,
                    "completedAt": None,
                    "stopReason": "aborted",
                },
            }),
            runner.canonical_document_bytes({
                "type": "message", "message": completed
            }),
        ])
        messages = runner._completed_assistant_messages(
            session,
            event_type="message",
            role="synthetic persistent session",
        )
        self.assertEqual(len(messages), 1)
        stdout = runner.canonical_document_bytes({
            "type": "message_end", "message": completed
        }).decode()
        runner.reconcile_omp_stdout(messages, stdout)
        changed = copy.deepcopy(completed)
        changed["usage"]["output"] = 3
        mismatched_stdout = runner.canonical_document_bytes({
            "type": "message_end", "message": changed
        }).decode()
        with self.assertRaisesRegex(runner.PilotError, "disagree"):
            runner.reconcile_omp_stdout(messages, mismatched_stdout)

    def test_direct_capture_rejects_route_payload_counter_and_status_contradictions(self) -> None:
        status = {
            "workspace_id": "ws",
            "config_hash": "a" * 64,
            "registered_config_hash": "a" * 64,
            "effective_config_hash": "a" * 64,
            "active_generation_id": "gen",
            "active_generation_state": "committed",
        }
        execution = {
            "negotiated_versions": {
                "capability_version": "context-capabilities-v2.0.0",
                "route_version": "context-route-v2.0.0",
                "decision_version": "context-route-v2.0.0",
                "execution_version": "context-execution-v2.0.0",
                "ir_version": "2.0.0",
                "operation_versions": ["query-v1.0.0", "source-reference-v1.0.0"],
            },
            "execution": {
                "state": "completed",
                "schema_version": "context-execution-v2.0.0",
                "decision": {
                    "schema_version": "context-route-v2.0.0",
                    "request": {
                        "schema_version": "context-route-v2.0.0",
                        "normalized_task_hash": sha256(b"task"),
                        "declared_kind": None,
                        "explicit_seeds": {"path_count": 1, "symbol_count": 0},
                        "atlas_intent": "none",
                        "route_floor": "DIRECT",
                        "route_ceiling": "DIRECT",
                        "deep_budget": None,
                    },
                    "starting_generation": {
                        "state": "observed",
                        "generation_id": "gen",
                    },
                },
                "attempts": [{
                    "route": "DIRECT",
                    "state": "completed",
                    "counters": {"atlas_calls": 0, "records": 0, "source_bytes": 0, "estimated_tokens": 0, "work_units": 0},
                }],
                "final_route": "DIRECT",
                "payload": {"payload_type": "direct_none"},
                "counters": {"atlas_calls": 0, "records": 0, "source_bytes": 0, "estimated_tokens": 0, "work_units": 0},
                "materialization": None,
            },
            "light_results": None,
            "deep_context_ir": None,
        }
        runner.validate_direct_acquisition(
            status,
            status,
            execution,
            expected_task_hash=sha256(b"task"),
            expected_path_count=1,
        )
        capabilities = {
            "schema_version": "context-capabilities-v2.0.0",
            "supported_route_policy_versions": ["context-route-v2.0.0"],
            "supported_decision_versions": ["context-route-v2.0.0"],
            "supported_execution_versions": ["context-execution-v2.0.0"],
            "supported_ir_versions": ["2.0.0"],
            "supported_operation_versions": [
                "query-v1.0.0",
                "source-reference-v1.0.0",
            ],
            "availability": {"route_decision": "available"},
        }
        runner.validate_capabilities(capabilities)
        changed_capabilities = copy.deepcopy(capabilities)
        changed_capabilities["supported_ir_versions"] = ["1.0.0"]
        with self.assertRaisesRegex(runner.PilotError, "supported V2"):
            runner.validate_capabilities(changed_capabilities)
        mutations = [
            lambda value: value["execution"].update(final_route="ATLAS_LIGHT"),
            lambda value: value["execution"].update(payload={"payload_type": "light_bundle"}),
            lambda value: value["execution"]["counters"].update(records=1),
            lambda value: value.update(light_results=[]),
            lambda value: value["execution"]["counters"].pop("records"),
            lambda value: value["execution"]["counters"].update(records=True),
        ]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                changed = copy.deepcopy(execution)
                mutate(changed)
                with self.assertRaises(runner.PilotError):
                    runner.validate_direct_acquisition(
                        status,
                        status,
                        changed,
                        expected_task_hash=sha256(b"task"),
                        expected_path_count=1,
                    )
        changed_status = {**status, "active_generation_id": "other"}
        with self.assertRaisesRegex(runner.PilotError, "status changed"):
            runner.validate_direct_acquisition(
                status,
                changed_status,
                execution,
                expected_task_hash=sha256(b"task"),
                expected_path_count=1,
            )

    def test_acquisition_commands_bind_frozen_request_path(self) -> None:
        checkout = self.root / "acquisition"
        catalogue = self.root / "catalogue.sqlite"
        request = self.root / "governor-request.json"
        manifest = {
            "paths": {
                "acquisition_checkout": str(checkout),
                "catalogue": str(catalogue),
            },
            "executables": {"atlas": {"path": "atlas-fixed"}},
            "acquisition_request": {"path": str(request)},
        }
        commands = runner.fixed_acquisition_commands(manifest)
        self.assertEqual(
            commands["governor"],
            [
                "atlas-fixed",
                "governor",
                "run",
                str(checkout),
                "--request",
                str(request),
                "--catalogue",
                str(catalogue),
            ],
        )
        self.assertEqual(
            list(commands),
            ["capabilities", "status-before", "governor", "status-after"],
        )

    def test_cargo_argv_isolates_checkout_role_targets(self) -> None:
        target = self.root / "target-container"
        target.mkdir()
        baseline_target = target / "baseline"
        validation_target = target / "validation"
        baseline_target.mkdir()
        validation_target.mkdir()
        manifest = {
            "paths": {"target_root": str(target)},
            "executables": {"cargo": {"path": "cargo-fixed"}},
            "cache": {
                "state": "fresh_empty",
                "sharing": "role_isolated",
                "cold_latency_claim": False,
                "role_targets": {
                    "baseline_checkout": str(baseline_target),
                    "validation_checkout": str(validation_target),
                },
            },
        }
        baseline = runner.fixed_cargo_argv(
            manifest, "baseline_checkout", "probe"
        )
        validation = runner.fixed_cargo_argv(
            manifest, "validation_checkout", "probe"
        )
        self.assertNotEqual(baseline, validation)
        self.assertEqual(
            baseline[baseline.index("--target-dir") + 1],
            str(baseline_target),
        )
        self.assertEqual(
            validation[validation.index("--target-dir") + 1],
            str(validation_target),
        )
        for role in ("model_checkout", "foreign"):
            with self.subTest(role=role):
                with self.assertRaisesRegex(
                    runner.PilotError, "Cargo target role"
                ):
                    runner.fixed_cargo_argv(manifest, role, "probe")
        invalid_bindings = {
            "crossover": str(validation_target),
            "container": str(target),
            "foreign": str(self.root / "foreign-target"),
        }
        for label, path in invalid_bindings.items():
            with self.subTest(binding=label):
                changed = copy.deepcopy(manifest)
                changed["cache"]["role_targets"][
                    "baseline_checkout"
                ] = path
                with self.assertRaisesRegex(
                    runner.PilotError, "role binding mismatch"
                ):
                    runner.fixed_cargo_argv(
                        changed, "baseline_checkout", "probe"
                    )

        dirty = self.root / "dirty-target-container"
        dirty.mkdir()
        (dirty / "foreign").write_bytes(b"occupied")
        with self.assertRaisesRegex(runner.PilotError, "fresh and empty"):
            runner._reserve_cargo_targets(dirty)

    def test_resealed_manifest_cannot_replace_recipe_or_delete_dependency(self) -> None:
        scratch = self.root / "manifest-scratch"
        scratch.mkdir()
        checkouts = {
            name: scratch / name for name in ("baseline", "model", "validation")
        }
        acquisition = self.root / "manifest-acquisition"
        evidence = self.root / "manifest-evidence"
        target = self.root / "manifest-target"
        for path in (*checkouts.values(), acquisition, evidence, target):
            path.mkdir()
        cargo_targets = runner._reserve_cargo_targets(target)
        catalogue = self.root / "catalogue.sqlite"
        catalogue.write_bytes(b"synthetic catalogue")
        retained_files = {
            "instruction": ("frozen-instruction.txt", b"Synthetic instruction.\n"),
            "model_configuration": (
                "frozen-model-configuration.json", b'{"model":null}\n'
            ),
            "agent_configuration": (
                "frozen-agent-configuration.json", b'{"agent":null}\n'
            ),
            "prompt": ("model-prompt.md", b"Synthetic prompt.\n"),
            "acquisition_request": (
                "governor-request.json", b'{"synthetic":true}\n'
            ),
        }
        retained = {}
        for role, (name, data) in retained_files.items():
            path = evidence / name
            path.write_bytes(data)
            retained[role] = {
                "path": str(path),
                "bytes": len(data),
                "sha256": sha256(data),
            }
        registry = runner.load_registry()
        recipe = registry["tasks"]["single-file-canonical-id-whitespace"]
        protected_lockfiles = install_protected_lockfiles(
            recipe,
            {
                "baseline_checkout": checkouts["baseline"],
                "model_checkout": checkouts["model"],
                "validation_checkout": checkouts["validation"],
                "acquisition_checkout": acquisition,
            },
        )
        executable = runner._executable_identity(
            Path(sys.executable), "synthetic executable"
        )
        executables = {
            name: executable for name in ("git", "cargo", "atlas", "omp")
        }
        external = runner._external_build_configuration(
            dict(os.environ),
            {
                "baseline_checkout": checkouts["baseline"],
                "validation_checkout": checkouts["validation"],
            },
        )
        dependencies = [
            runner._manifest_dependency(runner.REGISTRY_PATH, "registry"),
            runner._manifest_dependency(
                runner.FIXTURE_ROOT / recipe["validator_contract_fixture"],
                "validator_contract",
            ),
            runner._manifest_dependency(
                runner.FIXTURE_ROOT / recipe["probe_fixture"],
                "trusted_probe_fixture_encoded",
            ),
            runner._manifest_dependency(Path(runner.__file__), "runner"),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("provider-smoke.py"),
                "process_guard",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("qualify-context-evidence.py"),
                "qualification_helper",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("benchmark-evidence-export.py"),
                "exporter_helper",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("check-public-hygiene.py"),
                "hygiene_helper",
            ),
            runner._manifest_dependency(catalogue, "catalogue"),
        ]
        for role, identity in retained.items():
            dependencies.append({"role": role, **identity})
        for name, identity in executables.items():
            dependencies.append({"role": f"{name}_executable", **identity})
        preflight_records = [
            {
                "checkout": checkout,
                "name": name,
                "argv": [
                    str(Path(sys.executable).resolve()),
                    *runner.GIT_FENCE_COMMANDS[name],
                ],
                "resolved_argv": [
                    str(Path(sys.executable).resolve()),
                    *runner.GIT_FENCE_COMMANDS[name],
                ],
                "cwd": str(
                    checkouts.get(
                        checkout.removesuffix("_checkout"), acquisition
                    )
                ),
                "exit_code": 0,
                "stdout": (
                    runner.FROZEN_COMMIT + "\n" if name == "head" else ""
                ),
                "stderr": "",
                "stdout_sha256": sha256(
                    (
                        runner.FROZEN_COMMIT + "\n"
                        if name == "head"
                        else ""
                    ).encode()
                ),
                "stderr_sha256": sha256(b""),
                "duration_us": 1,
                "transport_failure": None,
                "failure_kind": None,
                "containment_cleanup_completed": True,
                "stream_encoding": "UTF-8 decoded transport output re-encoded for retention",
            }
            for checkout in (
                "baseline_checkout", "model_checkout",
                "validation_checkout", "acquisition_checkout",
            )
            for name in runner.GIT_FENCE_COMMANDS
        ]
        material = {
            "schema_version": runner.MANIFEST_VERSION,
            "kind": "atlas-context-pilot-run-manifest",
            "synthetic": True,
            "task_id": "single-file-canonical-id-whitespace",
            "frozen_commit": runner.FROZEN_COMMIT,
            "registry": {
                "path": str(runner.REGISTRY_PATH),
                "sha256": registry["registry_sha256"],
            },
            "recipe": recipe,
            "paths": {
                "scratch_root": str(scratch),
                "baseline_checkout": str(checkouts["baseline"]),
                "model_checkout": str(checkouts["model"]),
                "validation_checkout": str(checkouts["validation"]),
                "evidence_root": str(evidence),
                "target_root": str(target),
                "trusted_root": str(runner.FIXTURE_ROOT),
                "acquisition_checkout": str(acquisition),
                "catalogue": str(catalogue),
            },
            "executables": executables,
            "configuration": {
                "instruction": retained["instruction"],
                "model": retained["model_configuration"],
                "agent": retained["agent_configuration"],
                "weights_revision": None,
                "execution_binding": "declared_metadata_only",
            },
            "environment": {
                "qualified_inherited_toolchain": runner._environment_snapshot(
                    dict(os.environ)
                ),
                "external_configuration": external,
                "mutated_by_runner": False,
            },
            "cache": {
                "state": "fresh_empty",
                "sharing": "role_isolated",
                "cold_latency_claim": False,
                "role_targets": {
                    role: str(path)
                    for role, path in cargo_targets.items()
                },
            },
            "protected_lockfiles": protected_lockfiles,
            "prompt": retained["prompt"],
            "acquisition_request": retained["acquisition_request"],
            "preflight_records": preflight_records,
            "dependencies": dependencies,
        }
        manifest = runner.seal_document(material, "manifest_sha256")
        manifest_path = evidence / "run-manifest.json"
        manifest_path.write_bytes(runner.canonical_document_bytes(manifest))
        runner._load_manifest(manifest_path, allow_synthetic=True)
        forged_lockfile = copy.deepcopy(manifest)
        forged_lockfile["protected_lockfiles"]["baseline_checkout"][
            "sha256"
        ] = "0" * 64
        forged_lockfile = runner.seal_document(
            {
                key: value
                for key, value in forged_lockfile.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        forged_lockfile_path = evidence / "forged-lockfile.json"
        forged_lockfile_path.write_bytes(
            runner.canonical_document_bytes(forged_lockfile)
        )
        with self.assertRaisesRegex(
            runner.PilotError, "lockfile changed after freeze"
        ):
            runner._load_manifest(
                forged_lockfile_path, allow_synthetic=True
            )

        swapped_lockfiles = copy.deepcopy(manifest)
        swapped_lockfiles["protected_lockfiles"][
            "baseline_checkout"
        ], swapped_lockfiles["protected_lockfiles"]["model_checkout"] = (
            swapped_lockfiles["protected_lockfiles"]["model_checkout"],
            swapped_lockfiles["protected_lockfiles"]["baseline_checkout"],
        )
        swapped_lockfiles = runner.seal_document(
            {
                key: value
                for key, value in swapped_lockfiles.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        swapped_lockfiles_path = evidence / "swapped-lockfiles.json"
        swapped_lockfiles_path.write_bytes(
            runner.canonical_document_bytes(swapped_lockfiles)
        )
        with self.assertRaisesRegex(
            runner.PilotError, "lockfile changed after freeze"
        ):
            runner._load_manifest(
                swapped_lockfiles_path, allow_synthetic=True
            )

        baseline_lockfile = (
            checkouts["baseline"] / runner.LOCKFILE_PATH
        )
        original_lockfile = baseline_lockfile.read_bytes()
        baseline_lockfile.write_bytes(
            original_lockfile.replace(b"\n", b"\r\n")
        )
        try:
            with self.assertRaisesRegex(
                runner.PilotError, "lockfile changed after freeze"
            ):
                runner._load_manifest(
                    manifest_path, allow_synthetic=True
                )
        finally:
            baseline_lockfile.write_bytes(original_lockfile)


        drifted_environment = copy.deepcopy(manifest)
        drifted_environment["environment"]["qualified_inherited_toolchain"][
            "RUSTC_WRAPPER"
        ] = "changed-wrapper"
        with self.assertRaisesRegex(runner.PilotError, "environment drifted"):
            runner._manifest_environment(drifted_environment)

        changed_recipe = copy.deepcopy(manifest)
        changed_recipe["recipe"]["acceptance_target"] = "arbitrary_target"
        changed_recipe = runner.seal_document(
            {
                key: value
                for key, value in changed_recipe.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        changed_recipe_path = evidence / "changed-recipe.json"
        changed_recipe_path.write_bytes(
            runner.canonical_document_bytes(changed_recipe)
        )
        with self.assertRaisesRegex(runner.PilotError, "trusted registry"):
            runner._load_manifest(changed_recipe_path, allow_synthetic=True)

        crossed_target = copy.deepcopy(manifest)
        crossed_target["cache"]["role_targets"][
            "baseline_checkout"
        ] = crossed_target["cache"]["role_targets"][
            "validation_checkout"
        ]
        crossed_target = runner.seal_document(
            {
                key: value
                for key, value in crossed_target.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        crossed_target_path = evidence / "crossed-target.json"
        crossed_target_path.write_bytes(
            runner.canonical_document_bytes(crossed_target)
        )
        with self.assertRaisesRegex(
            runner.PilotError, "role binding mismatch"
        ):
            runner._load_manifest(
                crossed_target_path, allow_synthetic=True
            )

        reused_cache = copy.deepcopy(manifest)
        reused_cache["cache"]["state"] = "reused"
        reused_cache = runner.seal_document(
            {
                key: value
                for key, value in reused_cache.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        reused_cache_path = evidence / "reused-cache.json"
        reused_cache_path.write_bytes(
            runner.canonical_document_bytes(reused_cache)
        )
        with self.assertRaisesRegex(runner.PilotError, "cache contract"):
            runner._load_manifest(
                reused_cache_path, allow_synthetic=True
            )

        helper_alias = self.root / "qualification-helper-alias.py"
        helper_source = Path(runner.__file__).with_name(
            "qualify-context-evidence.py"
        )
        helper_alias.write_bytes(helper_source.read_bytes())
        redirected_helper = copy.deepcopy(manifest)
        for dependency in redirected_helper["dependencies"]:
            if dependency["role"] == "qualification_helper":
                dependency["path"] = str(helper_alias)
        redirected_helper = runner.seal_document(
            {
                key: value
                for key, value in redirected_helper.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        redirected_helper_path = evidence / "redirected-helper.json"
        redirected_helper_path.write_bytes(
            runner.canonical_document_bytes(redirected_helper)
        )
        with self.assertRaisesRegex(runner.PilotError, "path mismatch"):
            runner._load_manifest(
                redirected_helper_path, allow_synthetic=True
            )

        missing_dependency = copy.deepcopy(manifest)
        missing_dependency["dependencies"] = [
            item
            for item in missing_dependency["dependencies"]
            if item["role"] != "cargo_executable"
        ]
        missing_dependency = runner.seal_document(
            {
                key: value
                for key, value in missing_dependency.items()
                if key != "manifest_sha256"
            },
            "manifest_sha256",
        )
        missing_dependency_path = evidence / "missing-dependency.json"
        missing_dependency_path.write_bytes(
            runner.canonical_document_bytes(missing_dependency)
        )
        with self.assertRaisesRegex(
            runner.PilotError, "dependency roles are incomplete"
        ):
            runner._load_manifest(
                missing_dependency_path, allow_synthetic=True
            )

    def test_complete_synthetic_phase_chain_reconstructs_and_rejects_fabrication(self) -> None:
        evidence = self.root / "synthetic-evidence"
        evidence.mkdir()
        scratch = self.root / "synthetic-scratch"
        scratch.mkdir()
        baseline_checkout = scratch / "baseline"
        model_checkout = scratch / "model"
        validation_checkout = scratch / "validation"
        acquisition_checkout = self.root / "synthetic-acquisition"
        target_root = self.root / "synthetic-target"
        for path in (
            baseline_checkout, model_checkout, validation_checkout,
            acquisition_checkout, target_root,
        ):
            path.mkdir()
        cargo_targets = runner._reserve_cargo_targets(target_root)
        catalogue = self.root / "synthetic-catalogue.sqlite"
        catalogue.write_bytes(b"synthetic")
        registry = runner.load_registry()
        recipe = registry["tasks"]["single-file-canonical-id-whitespace"]
        protected_lockfiles = install_protected_lockfiles(
            recipe,
            {
                "baseline_checkout": baseline_checkout,
                "model_checkout": model_checkout,
                "validation_checkout": validation_checkout,
                "acquisition_checkout": acquisition_checkout,
            },
        )
        retained_files = {
            "instruction": ("frozen-instruction.txt", b"Synthetic instruction.\n"),
            "model_configuration": (
                "frozen-model-configuration.json", b'{"model":null}\n'
            ),
            "agent_configuration": (
                "frozen-agent-configuration.json", b'{"agent":null}\n'
            ),
            "prompt": ("model-prompt.md", b"Synthetic prompt.\n"),
            "acquisition_request": (
                "governor-request.json", b'{"synthetic":true}\n'
            ),
        }
        retained_identity = {}
        for role, (name, data) in retained_files.items():
            path = evidence / name
            path.write_bytes(data)
            retained_identity[role] = {
                "path": str(path),
                "bytes": len(data),
                "sha256": sha256(data),
            }
        prompt = evidence / "model-prompt.md"
        request_path = evidence / "governor-request.json"
        executable = runner._executable_identity(
            Path(sys.executable), "synthetic executable"
        )
        executables = {
            name: executable for name in ("git", "cargo", "atlas", "omp")
        }
        external = runner._external_build_configuration(
            dict(os.environ),
            {
                "baseline_checkout": baseline_checkout,
                "validation_checkout": validation_checkout,
            },
        )
        dependencies = [
            runner._manifest_dependency(runner.REGISTRY_PATH, "registry"),
            runner._manifest_dependency(
                runner.FIXTURE_ROOT / recipe["validator_contract_fixture"],
                "validator_contract",
            ),
            runner._manifest_dependency(
                runner.FIXTURE_ROOT / recipe["probe_fixture"],
                "trusted_probe_fixture_encoded",
            ),
            runner._manifest_dependency(Path(runner.__file__), "runner"),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("provider-smoke.py"),
                "process_guard",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("qualify-context-evidence.py"),
                "qualification_helper",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("benchmark-evidence-export.py"),
                "exporter_helper",
            ),
            runner._manifest_dependency(
                Path(runner.__file__).with_name("check-public-hygiene.py"),
                "hygiene_helper",
            ),
            runner._manifest_dependency(catalogue, "catalogue"),
        ]
        for role, identity in retained_identity.items():
            dependencies.append({"role": role, **identity})
        for name, identity in executables.items():
            dependencies.append({"role": f"{name}_executable", **identity})
        preflight_records = [
            {
                **synthetic_process_record(
                    [
                        executable["path"],
                        *runner.GIT_FENCE_COMMANDS[name],
                    ],
                    checkout,
                    stdout=(
                        runner.FROZEN_COMMIT + "\n"
                        if name == "head"
                        else ""
                    ),
                    name=name,
                    checkout=checkout_name,
                )
            }
            for checkout_name, checkout in (
                ("baseline_checkout", baseline_checkout),
                ("model_checkout", model_checkout),
                ("validation_checkout", validation_checkout),
                ("acquisition_checkout", acquisition_checkout),
            )
            for name in runner.GIT_FENCE_COMMANDS
        ]
        material = {
            "schema_version": runner.MANIFEST_VERSION,
            "kind": "atlas-context-pilot-run-manifest",
            "synthetic": True,
            "task_id": "single-file-canonical-id-whitespace",
            "frozen_commit": runner.FROZEN_COMMIT,
            "registry": {
                "path": str(runner.REGISTRY_PATH),
                "sha256": registry["registry_sha256"],
            },
            "recipe": recipe,
            "paths": {
                "scratch_root": str(scratch),
                "baseline_checkout": str(baseline_checkout),
                "model_checkout": str(model_checkout),
                "validation_checkout": str(validation_checkout),
                "evidence_root": str(evidence),
                "target_root": str(target_root),
                "trusted_root": str(runner.FIXTURE_ROOT),
                "acquisition_checkout": str(acquisition_checkout),
                "catalogue": str(catalogue),
            },
            "executables": executables,
            "configuration": {
                "instruction": retained_identity["instruction"],
                "model": retained_identity["model_configuration"],
                "agent": retained_identity["agent_configuration"],
                "weights_revision": None,
                "execution_binding": "declared_metadata_only",
            },
            "environment": {
                "qualified_inherited_toolchain": runner._environment_snapshot(
                    dict(os.environ)
                ),
                "external_configuration": external,
                "mutated_by_runner": False,
            },
            "cache": {
                "state": "fresh_empty",
                "sharing": "role_isolated",
                "cold_latency_claim": False,
                "role_targets": {
                    role: str(path)
                    for role, path in cargo_targets.items()
                },
            },
            "protected_lockfiles": protected_lockfiles,
            "prompt": retained_identity["prompt"],
            "acquisition_request": retained_identity["acquisition_request"],
            "preflight_records": preflight_records,
            "dependencies": dependencies,
        }
        manifest = runner.seal_document(material, "manifest_sha256")
        manifest_path = evidence / "run-manifest.json"
        manifest_path.write_bytes(runner.canonical_document_bytes(manifest))
        runner._load_manifest(manifest_path, allow_synthetic=True)

        def retained(path: str, role: str, data: bytes) -> dict[str, object]:
            target = evidence / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            return runner.file_dependency(evidence, target, role)

        status = {
            "workspace_id": "ws",
            "config_hash": "a" * 64,
            "registered_config_hash": "a" * 64,
            "effective_config_hash": "a" * 64,
            "active_generation_id": "gen",
            "active_generation_state": "committed",
        }
        counters = {
            "atlas_calls": 0,
            "records": 0,
            "source_bytes": 0,
            "estimated_tokens": 0,
            "work_units": 0,
        }
        envelope = {
            "negotiated_versions": {
                "capability_version": "context-capabilities-v2.0.0",
                "route_version": "context-route-v2.0.0",
                "decision_version": "context-route-v2.0.0",
                "execution_version": "context-execution-v2.0.0",
                "ir_version": "2.0.0",
                "operation_versions": [
                    "query-v1.0.0", "source-reference-v1.0.0"
                ],
            },
            "execution": {
                "state": "completed",
                "schema_version": "context-execution-v2.0.0",
                "decision": {
                    "schema_version": "context-route-v2.0.0",
                    "request": {
                        "schema_version": "context-route-v2.0.0",
                        "normalized_task_hash": sha256(
                            recipe["specification"].strip().lower().encode()
                        ),
                        "declared_kind": None,
                        "explicit_seeds": {"path_count": 1, "symbol_count": 0},
                        "atlas_intent": "none",
                        "route_floor": "DIRECT",
                        "route_ceiling": "DIRECT",
                        "deep_budget": None,
                    },
                    "starting_generation": {
                        "state": "observed",
                        "generation_id": "gen",
                    },
                },
                "attempts": [{
                    "route": "DIRECT",
                    "state": "completed",
                    "counters": counters,
                }],
                "final_route": "DIRECT",
                "payload": {"payload_type": "direct_none"},
                "counters": counters,
                "materialization": None,
            },
            "light_results": None,
            "deep_context_ir": None,
        }
        acquisition_dependencies = []
        capabilities = {
            "schema_version": "context-capabilities-v2.0.0",
            "supported_route_policy_versions": ["context-route-v2.0.0"],
            "supported_decision_versions": ["context-route-v2.0.0"],
            "supported_execution_versions": ["context-execution-v2.0.0"],
            "supported_ir_versions": ["2.0.0"],
            "supported_operation_versions": [
                "query-v1.0.0",
                "source-reference-v1.0.0",
            ],
            "availability": {"route_decision": "available"},
        }
        acquisition_values = {
            "capabilities": capabilities,
            "status-before": status,
            "governor": envelope,
            "status-after": status,
        }
        acquisition_commands = {
            "capabilities": [
                executable["path"], "governor", "capabilities",
                str(acquisition_checkout), "--catalogue", str(catalogue),
            ],
            "status-before": [
                executable["path"], "status", str(acquisition_checkout),
                "--catalogue", str(catalogue),
            ],
            "governor": [
                executable["path"], "governor", "run", str(acquisition_checkout),
                "--request", str(request_path), "--catalogue", str(catalogue),
            ],
            "status-after": [
                executable["path"], "status", str(acquisition_checkout),
                "--catalogue", str(catalogue),
            ],
        }
        for name, value in acquisition_values.items():
            stdout = runner.canonical_document_bytes(value).decode()
            record = synthetic_process_record(
                acquisition_commands[name],
                acquisition_checkout,
                stdout=stdout,
            )
            retained_record = {
                key: item
                for key, item in record.items()
                if key not in ("stdout", "stderr")
            }
            acquisition_dependencies.extend([
                retained(
                    f"acquisition-{name}.stdout.txt",
                    f"acquisition-{name}_stdout",
                    stdout.encode(),
                ),
                retained(
                    f"acquisition-{name}.stderr.txt",
                    f"acquisition-{name}_stderr",
                    b"",
                ),
                retained(
                    f"acquisition-{name}.execution.json",
                    f"acquisition-{name}_execution",
                    runner.canonical_document_bytes(retained_record),
                ),
            ])
        acquisition = {
            "schema_version": "atlas-context-pilot-acquisition-v1.0.0",
            "synthetic": True,
            "task_id": manifest["task_id"],
            "route": "DIRECT",
            "supplied_packet": None,
            "counters": counters,
            "status_before": status,
            "status_after": status,
            "literal_capabilities": capabilities,
            "literal_governor": envelope,
            "decision_hash_recomputed": False,
            "automatic_sufficiency": False,
            "atlas_benefit": None,
            "fence_records": (
                synthetic_fence_records(
                    "acquisition_checkout", acquisition_checkout, executable["path"],
                    modified=set(), untracked=set(),
                )
                + synthetic_fence_records(
                    "acquisition_checkout", acquisition_checkout, executable["path"],
                    modified=set(), untracked=set(),
                )
            ),
            "dependencies": acquisition_dependencies,
        }
        (evidence / "acquisition-result.json").write_bytes(
            runner.canonical_document_bytes(acquisition)
        )

        baseline_stdout = cargo_output(
            recipe["acceptance_names"],
            set(recipe["baseline_failed_names"]),
        )
        baseline_argv = runner.fixed_cargo_argv(
            manifest,
            "baseline_checkout",
            recipe["acceptance_target"],
        )
        baseline_record = synthetic_process_record(
            baseline_argv,
            baseline_checkout,
            stdout=baseline_stdout,
            exit_code=101,
        )
        baseline_retained = {
            key: value
            for key, value in baseline_record.items()
            if key not in ("stdout", "stderr")
        }
        baseline_dependencies = [
            retained(
                "baseline-control-attempt-1.stdout.txt",
                "baseline-control-attempt-1_stdout",
                baseline_stdout.encode(),
            ),
            retained(
                "baseline-control-attempt-1.stderr.txt",
                "baseline-control-attempt-1_stderr",
                b"",
            ),
            retained(
                "baseline-control-attempt-1.execution.json",
                "baseline-control-attempt-1_execution",
                runner.canonical_document_bytes(baseline_retained),
            ),
        ]
        baseline_observed = runner.parse_cargo_test_output(
            baseline_stdout, "", recipe["acceptance_names"]
        )
        baseline_attempt = {
            **baseline_retained,
            "observed": baseline_observed,
        }
        baseline = {
            "schema_version": "atlas-context-pilot-baseline-v1.0.0",
            "synthetic": True,
            "task_id": manifest["task_id"],
            "manifest_sha256": manifest["manifest_sha256"],
            "negative_control_satisfied": True,
            "acquisition_path": "acquisition-result.json",
            "cargo": {
                "attempts": [baseline_attempt],
                "observed": baseline_observed,
                "successful_transport": True,
                "dependencies": baseline_dependencies,
                "fence_records": synthetic_fence_records(
                    "baseline_checkout",
                    baseline_checkout,
                    executable["path"],
                    modified=set(),
                    untracked={recipe["probe_injection_path"]},
                ),
            },
            "start_fence_records": synthetic_fence_records(
                "baseline_checkout",
                baseline_checkout,
                executable["path"],
                modified=set(),
                untracked=set(),
            ),
            "end_fence_records": synthetic_fence_records(
                "baseline_checkout",
                baseline_checkout,
                executable["path"],
                modified=set(),
                untracked={recipe["probe_injection_path"]},
            ),
            "acceptance": None,
        }
        (evidence / "baseline-result.json").write_bytes(
            runner.canonical_document_bytes(baseline)
        )

        candidate_source = b"synthetic candidate source\n"
        candidate_patch = b"synthetic patch\n"
        retained("candidate-source.bin", "candidate_source", candidate_source)
        retained("candidate.patch", "candidate_patch", candidate_patch)
        model_argv = [
            executable["path"], "-p", "--session-dir", str(evidence / "model-sessions"),
            "--mode=json", "--no-title", "--no-extensions", "--no-lsp",
            "--no-pty", "--tools=read,grep,glob,edit,write",
            "--approval-mode=yolo", "--max-time=8m", f"@{prompt}",
        ]
        patch_argv = [
            executable["path"], "--no-optional-locks",
            "diff", "--binary", "--no-ext-diff", "--",
            recipe["candidate_source"],
        ]
        patch_record = synthetic_process_record(
            patch_argv,
            model_checkout,
            stdout=candidate_patch.decode(),
        )
        patch_retained = {
            key: value
            for key, value in patch_record.items()
            if key not in ("stdout", "stderr")
        }
        message = {
            "responseId": "response-synthetic-1",
            "provider": "synthetic-provider",
            "model": "synthetic-model",
            "role": "assistant",
            "completedAt": 2,
            "stopReason": "stop",
            "usage": {
                "input": 1, "output": 1, "totalTokens": 2,
                "reasoningTokens": 0, "cacheRead": 0, "cacheWrite": 0,
            },
            "content": [{
                "type": "toolCall",
                "id": "tool-synthetic-1",
                "name": "read",
            }],
        }
        session_data = (
            runner.canonical_document_bytes({
                "type": "message",
                "message": {
                    "role": "user",
                    "content": [{"type": "text"}],
                },
            })
            + runner.canonical_document_bytes({
                "type": "message",
                "message": {
                    **message,
                    "completedAt": None,
                    "stopReason": "aborted",
                },
            })
            + runner.canonical_document_bytes({
                "type": "message", "message": message
            })
        )
        model_stdout = (
            runner.canonical_document_bytes({
                "type": "message_update",
                "message": message,
            })
            + runner.canonical_document_bytes({
                "type": "message_end", "message": message
            })
        ).decode()
        model_record = synthetic_process_record(
            model_argv, model_checkout, stdout=model_stdout
        )
        model_retained = {
            key: value
            for key, value in model_record.items()
            if key not in ("stdout", "stderr")
        }
        model_dependencies = [
            retained("model.stdout.txt", "model_stdout", model_stdout.encode()),
            retained("model.stderr.txt", "model_stderr", b""),
            retained(
                "model.execution.json",
                "model_execution",
                runner.canonical_document_bytes(model_retained),
            ),
            retained(
                "candidate-patch.stdout.txt",
                "candidate-patch_stdout",
                candidate_patch,
            ),
            retained(
                "candidate-patch.stderr.txt",
                "candidate-patch_stderr",
                b"",
            ),
            retained(
                "candidate-patch.execution.json",
                "candidate-patch_execution",
                runner.canonical_document_bytes(patch_retained),
            ),
            runner.file_dependency(
                evidence, evidence / "candidate-source.bin", "candidate_source"
            ),
            runner.file_dependency(
                evidence, evidence / "candidate.patch", "candidate_patch"
            ),
            retained(
                "model-sessions/session.jsonl",
                "omp_session_jsonl",
                session_data,
            ),
        ]
        model = {
            "schema_version": "atlas-context-pilot-model-v1.0.0",
            "synthetic": True,
            "task_id": manifest["task_id"],
            "manifest_sha256": manifest["manifest_sha256"],
            "execution_successful": True,
            "execution": model_retained,
            "patch_capture": patch_retained,
            "fence_records": (
                synthetic_fence_records(
                    "model_checkout", model_checkout, executable["path"],
                    modified=set(), untracked=set(),
                )
                + synthetic_fence_records(
                    "model_checkout", model_checkout, executable["path"],
                    modified=set(recipe["candidate_source_allowlist"]),
                    untracked=set(),
                )
            ),
            "candidate_source_sha256": sha256(candidate_source),
            "candidate_patch_sha256": sha256(candidate_patch),
            "scope_verified": recipe["candidate_source_allowlist"],
            "omp_observations": runner.aggregate_omp_messages([message]),
            "acceptance": None,
            "dependencies": model_dependencies,
        }
        (evidence / "model-result.json").write_bytes(
            runner.canonical_document_bytes(model)
        )

        acquisition_result, baseline_result, model_result = (
            runner.validate_phase_chain(manifest, evidence)
        )
        self.assertTrue(acquisition_result["synthetic"])
        self.assertTrue(baseline_result["synthetic"])
        self.assertTrue(model_result["synthetic"])

        def accepted_cargo(
            prefix: str, target_name: str, names: list[str]
        ) -> dict[str, object]:
            stdout = cargo_output(names, set())
            argv = runner.fixed_cargo_argv(
                manifest, "validation_checkout", target_name
            )
            record = synthetic_process_record(
                argv, validation_checkout, stdout=stdout
            )
            retained_record = {
                key: value
                for key, value in record.items()
                if key not in ("stdout", "stderr")
            }
            observed = runner.parse_cargo_test_output(stdout, "", names)
            dependencies = [
                retained(
                    f"{prefix}-attempt-1.stdout.txt",
                    f"{prefix}-attempt-1_stdout",
                    stdout.encode(),
                ),
                retained(
                    f"{prefix}-attempt-1.stderr.txt",
                    f"{prefix}-attempt-1_stderr",
                    b"",
                ),
                retained(
                    f"{prefix}-attempt-1.execution.json",
                    f"{prefix}-attempt-1_execution",
                    runner.canonical_document_bytes(retained_record),
                ),
            ]
            return {
                "attempts": [{**retained_record, "observed": observed}],
                "observed": observed,
                "successful_transport": True,
                "dependencies": dependencies,
                "fence_records": synthetic_fence_records(
                    "validation_checkout",
                    validation_checkout,
                    executable["path"],
                    modified=set(recipe["candidate_source_allowlist"]),
                    untracked={recipe["probe_injection_path"]},
                ),
            }

        candidate_acceptance = accepted_cargo(
            "candidate-acceptance",
            recipe["acceptance_target"],
            recipe["acceptance_names"],
        )
        candidate_regression = accepted_cargo(
            "candidate-regression",
            recipe["regression_target"],
            recipe["regression_names"],
        )
        validation_fences = (
            synthetic_fence_records(
                "validation_checkout",
                validation_checkout,
                executable["path"],
                modified=set(),
                untracked=set(),
            )
            + synthetic_fence_records(
                "validation_checkout",
                validation_checkout,
                executable["path"],
                modified=set(recipe["candidate_source_allowlist"]),
                untracked={recipe["probe_injection_path"]},
            )
            + synthetic_fence_records(
                "validation_checkout",
                validation_checkout,
                executable["path"],
                modified=set(recipe["candidate_source_allowlist"]),
                untracked={recipe["probe_injection_path"]},
            )
        )
        execution = {
            "task_id": manifest["task_id"],
            "acquisition": "acquisition-result.json",
            "baseline": baseline,
            "model": model,
            "candidate_acceptance": candidate_acceptance,
            "focused_regression": candidate_regression,
            "named_acceptance_results": candidate_acceptance["observed"],
            "named_regression_results": candidate_regression["observed"],
            "final_fences": {
                "candidate_source_sha256": model["candidate_source_sha256"],
                "trusted_probe_sha256": recipe["probe_sha256"],
                "allowlisted_modified": recipe["candidate_source_allowlist"],
                "allowlisted_untracked": [recipe["probe_injection_path"]],
            },
            "fence_records": validation_fences,
            "validation_execution_verified": True,
            "atlas_benefit": None,
            "actual_useful_context_throughput": None,
            "reasoning_use": None,
        }
        receipt_material = {
            "schema_version": runner.RECEIPT_VERSION,
            "kind": "atlas-context-pilot-executed-receipt",
            "synthetic": True,
            "manifest_sha256": manifest["manifest_sha256"],
            "outcome": "accepted",
            "execution": execution,
            "dependencies": runner._receipt_dependencies(evidence),
        }
        receipt = runner.seal_document(
            receipt_material, "receipt_sha256"
        )
        receipt_path = evidence / "executed-receipt.json"
        receipt_path.write_bytes(runner.canonical_document_bytes(receipt))
        verified_receipt = runner.verify_executed_receipt(
            manifest_path,
            receipt_path,
            evidence,
            _allow_synthetic_test_fixture=True,
        )
        self.assertTrue(verified_receipt["synthetic"])
        self.assertEqual(verified_receipt["outcome"], "accepted")

        def replace_execution_dependency(
            result: dict[str, object],
            role: str,
            data: bytes,
        ) -> None:
            for dependency in result["dependencies"]:
                if dependency["role"] == role:
                    dependency["bytes"] = len(data)
                    dependency["sha256"] = sha256(data)
                    return
            self.fail(f"missing dependency role {role}")

        baseline_execution_path = (
            evidence / "baseline-control-attempt-1.execution.json"
        )
        original_baseline_execution = baseline_execution_path.read_bytes()
        crossed_baseline_argv = runner.fixed_cargo_argv(
            manifest,
            "validation_checkout",
            recipe["acceptance_target"],
        )
        crossed_baseline_record = json.loads(
            original_baseline_execution
        )
        crossed_baseline_record["argv"] = crossed_baseline_argv
        crossed_baseline_record["resolved_argv"] = crossed_baseline_argv
        crossed_baseline_bytes = runner.canonical_document_bytes(
            crossed_baseline_record
        )
        crossed_baseline = copy.deepcopy(baseline)
        crossed_baseline["cargo"]["attempts"][0]["argv"] = (
            crossed_baseline_argv
        )
        crossed_baseline["cargo"]["attempts"][0]["resolved_argv"] = (
            crossed_baseline_argv
        )
        replace_execution_dependency(
            crossed_baseline["cargo"],
            "baseline-control-attempt-1_execution",
            crossed_baseline_bytes,
        )
        baseline_execution_path.write_bytes(crossed_baseline_bytes)
        try:
            with self.assertRaises(runner.PilotError):
                runner.validate_baseline_artifact(
                    manifest, crossed_baseline
                )
        finally:
            baseline_execution_path.write_bytes(
                original_baseline_execution
            )

        candidate_execution_path = (
            evidence / "candidate-acceptance-attempt-1.execution.json"
        )
        original_candidate_execution = (
            candidate_execution_path.read_bytes()
        )
        expected_candidate_argv = runner.fixed_cargo_argv(
            manifest,
            "validation_checkout",
            recipe["acceptance_target"],
        )
        redirected_argvs = {
            "crossover": runner.fixed_cargo_argv(
                manifest,
                "baseline_checkout",
                recipe["acceptance_target"],
            ),
            "container": [
                (
                    str(target_root)
                    if item == str(cargo_targets["validation_checkout"])
                    else item
                )
                for item in expected_candidate_argv
            ],
            "foreign": [
                (
                    str(self.root / "foreign-target")
                    if item == str(cargo_targets["validation_checkout"])
                    else item
                )
                for item in expected_candidate_argv
            ],
        }
        for label, redirected_argv in redirected_argvs.items():
            with self.subTest(candidate_target=label):
                redirected_record = json.loads(
                    original_candidate_execution
                )
                redirected_record["argv"] = redirected_argv
                redirected_record["resolved_argv"] = redirected_argv
                redirected_bytes = runner.canonical_document_bytes(
                    redirected_record
                )
                redirected_result = copy.deepcopy(candidate_acceptance)
                redirected_result["attempts"][0]["argv"] = redirected_argv
                redirected_result["attempts"][0][
                    "resolved_argv"
                ] = redirected_argv
                replace_execution_dependency(
                    redirected_result,
                    "candidate-acceptance-attempt-1_execution",
                    redirected_bytes,
                )
                candidate_execution_path.write_bytes(redirected_bytes)
                try:
                    with self.assertRaises(runner.PilotError):
                        runner._validate_cargo_result(
                            redirected_result,
                            names=recipe["acceptance_names"],
                            expected_passed=set(recipe["acceptance_names"]),
                            expected_failed=set(),
                            expected_exit=0,
                            evidence_root=evidence,
                            dependency_prefix="candidate-acceptance",
                            expected_argv=expected_candidate_argv,
                            expected_cwd=validation_checkout,
                            manifest=manifest,
                            checkout_name="validation_checkout",
                            allowed_modified=set(
                                recipe["candidate_source_allowlist"]
                            ),
                            allowed_untracked={
                                recipe["probe_injection_path"]
                            },
                        )
                finally:
                    candidate_execution_path.write_bytes(
                        original_candidate_execution
                    )

        fabricated = copy.deepcopy(baseline)
        fabricated["cargo"]["observed"]["failed"] = []
        with self.assertRaises(runner.PilotError):
            runner.validate_baseline_artifact(manifest, fabricated)

        tampered_model = copy.deepcopy(model)
        tampered_model["candidate_source_sha256"] = "0" * 64
        with self.assertRaises(runner.PilotError):
            runner.validate_model_artifact(manifest, tampered_model)

        missing_capture = copy.deepcopy(acquisition)
        missing_capture["dependencies"] = missing_capture["dependencies"][:-1]
        with self.assertRaisesRegex(runner.PilotError, "omit required roles"):
            runner.validate_acquisition_artifact(manifest, missing_capture)

        missing_start_fence = copy.deepcopy(baseline)
        missing_start_fence["start_fence_records"] = []
        with self.assertRaisesRegex(runner.PilotError, "wrong capture count"):
            runner.validate_baseline_artifact(manifest, missing_start_fence)

        aliased_stdout = evidence / "alias-baseline.stdout.txt"
        aliased_stdout.write_bytes(
            (evidence / "baseline-control-attempt-1.stdout.txt").read_bytes()
        )
        redirected_capture = copy.deepcopy(baseline)
        redirected_capture["cargo"]["dependencies"][0]["path"] = (
            "alias-baseline.stdout.txt"
        )
        with self.assertRaisesRegex(runner.PilotError, "wrong paths"):
            runner.validate_baseline_artifact(manifest, redirected_capture)

        for field, value in (
            ("argv", ["cargo", "arbitrary"]),
            ("resolved_argv", [str(Path(sys.executable).resolve()), "arbitrary"]),
            ("cwd", "other-checkout"),
            ("exit_code", 0),
            ("stdout_sha256", "0" * 64),
            ("containment_cleanup_completed", False),
        ):
            with self.subTest(captured_field=field):
                contradictory = copy.deepcopy(baseline)
                contradictory["cargo"]["attempts"][0][field] = value
                with self.assertRaises(runner.PilotError):
                    runner.validate_baseline_artifact(manifest, contradictory)

        altered_acquisition = copy.deepcopy(acquisition)
        altered_acquisition["status_before"]["workspace_id"] = "fabricated"
        with self.assertRaisesRegex(
            runner.PilotError, "differs from retained captures"
        ):
            runner.validate_acquisition_artifact(manifest, altered_acquisition)

        altered_fence = copy.deepcopy(acquisition)
        altered_fence["fence_records"][0]["stdout"] = "other-head\n"
        altered_fence["fence_records"][0]["stdout_sha256"] = sha256(
            b"other-head\n"
        )
        with self.assertRaises(runner.PilotError):
            runner.validate_acquisition_artifact(manifest, altered_fence)

        altered_summary = copy.deepcopy(model)
        altered_summary["omp_observations"]["tool_requests"] = 99
        with self.assertRaisesRegex(
            runner.PilotError, "differ from retained OMP sessions"
        ):
            runner.validate_model_artifact(manifest, altered_summary)

        session_path = evidence / "model-sessions/session.jsonl"
        original_session = session_path.read_bytes()
        session_path.write_bytes(
            runner.canonical_document_bytes({
                "type": "message",
                "message": {
                    **message,
                    "content": [
                        {
                            "type": "toolCall",
                            "id": "tampered-write",
                            "name": "write",
                        },
                        {
                            "type": "toolCall",
                            "id": "tampered-read",
                            "name": "read",
                        },
                    ],
                },
            })
        )
        with self.assertRaisesRegex(
            runner.PilotError, "phase dependency digest mismatch"
        ):
            runner.validate_model_artifact(manifest, model)
        session_path.write_bytes(original_session)

    def test_receipt_dependency_tampering_and_legacy_import_reject(self) -> None:
        evidence = self.root / "evidence"
        evidence.mkdir()
        dependency = evidence / "attempt.stdout.txt"
        dependency.write_text("captured\n", encoding="utf-8")
        manifest = runner.seal_document({
            "schema_version": runner.MANIFEST_VERSION,
            "kind": "atlas-context-pilot-run-manifest",
            "synthetic": False,
            "task_id": "single-file-canonical-id-whitespace",
            "dependencies": [],
        }, "manifest_sha256")
        manifest_path = evidence / "run-manifest.json"
        manifest_path.write_bytes(runner.canonical_document_bytes(manifest))
        receipt = runner.seal_document({
            "schema_version": runner.RECEIPT_VERSION,
            "kind": "atlas-context-pilot-executed-receipt",
            "synthetic": False,
            "manifest_sha256": manifest["manifest_sha256"],
            "outcome": "rejected",
            "execution": {
                "task_id": "single-file-canonical-id-whitespace",
                "acquisition": None,
                "baseline": None,
                "model": None,
                "candidate_acceptance": None,
                "focused_regression": None,
                "named_acceptance_results": None,
                "named_regression_results": None,
                "final_fences": {},
                "fence_records": [],
                "validation_execution_verified": False,
                "atlas_benefit": None,
                "actual_useful_context_throughput": None,
                "reasoning_use": None,
            },
            "dependencies": [runner.file_dependency(evidence, dependency, "attempt_stdout")],
        }, "receipt_sha256")
        receipt_path = evidence / "receipt.json"
        receipt_path.write_bytes(runner.canonical_document_bytes(receipt))
        with self.assertRaisesRegex(runner.PilotError, "run manifest fields"):
            runner.verify_executed_receipt(manifest_path, receipt_path, evidence)
        runner._verify_retained_dependencies(
            receipt["dependencies"],
            evidence,
            required_roles={"attempt_stdout"},
        )

        with self.assertRaisesRegex(runner.PilotError, "nonempty"):
            runner._verify_retained_dependencies(
                [],
                evidence,
                required_roles={"attempt_stdout"},
            )

        dependency.write_text("tampered\n", encoding="utf-8")
        with self.assertRaisesRegex(runner.PilotError, "dependency digest"):
            runner._verify_retained_dependencies(
                receipt["dependencies"],
                evidence,
                required_roles={"attempt_stdout"},
            )

        qualification_spec = importlib.util.spec_from_file_location(
            "legacy_qualification_for_runner_test",
            SCRIPT.with_name("qualify-context-evidence.py"),
        )
        qualification = importlib.util.module_from_spec(qualification_spec)
        sys.modules[qualification_spec.name] = qualification
        qualification_spec.loader.exec_module(qualification)
        with self.assertRaisesRegex(qualification.QualificationError, "fields"):
            qualification._validate_receipt(
                receipt,
                {
                    "task_id": "x", "corpus_sha256": "0" * 64,
                    "task_spec_sha256": "0" * 64,
                    "validation_contract_sha256": "0" * 64,
                    "workspace_identity_sha256": "0" * 64,
                    "repository_commit_before": "0" * 40,
                    "atlas_generation_before": "g",
                    "source_snapshot_sha256_before": "0" * 64,
                    "policies": {},
                },
                "0" * 64,
                "0" * 64,
                {"outcome_artifact_sha256": "0" * 64},
                {"status": "sufficient"},
                None,
            )

    def test_guard_delegation_runs_harmless_bounded_subprocess(self) -> None:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", ResourceWarning)
            result = runner.PROCESS_GUARD.bounded_run(
                [sys.executable, "-c", "print('guard-smoke')"],
                environment=dict(os.environ),
                cwd=self.root,
                timeout=30,
                output_limit=4096,
            )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "guard-smoke")


if __name__ == "__main__":
    unittest.main()
