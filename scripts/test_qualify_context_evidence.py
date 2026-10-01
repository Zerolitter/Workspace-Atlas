#!/usr/bin/env python3
"""Focused contract tests for qualify-context-evidence.py."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("qualify-context-evidence.py")
CORPUS = SCRIPT.parents[1] / "tests/fixtures/context_qualification/corpus-v1.json"
SPEC = importlib.util.spec_from_file_location("atlas_qualify_context_evidence", SCRIPT)
FIXTURE_ROOT = SCRIPT.parents[1] / "tests/fixtures/context_qualification"
FIXTURE_A = FIXTURE_ROOT / "accepted-a-v1.json"
FIXTURE_B = FIXTURE_ROOT / "accepted-b-v1.json"
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("qualification harness import unavailable")
qualification = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = qualification
SPEC.loader.exec_module(qualification)


def sha(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


class QualificationHarnessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workspace = Path(self.temporary.name)
        self.corpus_path = self.workspace / "corpus.json"
        self.corpus_path.write_bytes(CORPUS.read_bytes())
        self.corpus = qualification.load_corpus(self.workspace, self.corpus_path)
        self.task = self.corpus["tasks_by_id"]["single-file-canonical-id-whitespace"]

    def evidence_ref(
        self,
        required: dict[str, object],
        *,
        provenance: str,
    ) -> dict[str, object]:
        identity = {
            "kind": required["kind"],
            "id": f"native:{required['id']}",
            "range": required["range"],
        }
        source_revision = required["source_revision_sha256"]
        mapping_hash = qualification.canonical_sha256({
            "benchmark_locator_id": required["id"],
            "identity": identity,
            "source_revision_sha256": source_revision,
        })
        return {
            "identity": identity,
            "source_revision_sha256": source_revision,
            "provenance_sha256": sha(provenance),
            "benchmark_locator": {
                "id": required["id"],
                "mapping_sha256": mapping_hash,
                "verified": True,
            },
        }

    def packet(self) -> dict[str, object]:
        return {
            "task_id": self.task["task_id"],
            "corpus_sha256": self.corpus["corpus_sha256"],
            "task_spec_sha256": self.task["task_spec_sha256"],
            "validation_contract_sha256": self.task["validation_contract_sha256"],
            "workspace_identity_sha256": sha("fixture-workspace"),
            "repository_commit_before": self.corpus["repository"]["commit"],
            "atlas_generation_before": "gen_fixture_001",
            "source_snapshot_sha256_before": sha("source-snapshot"),
            "policies": {
                "configuration_sha256": sha("configuration"),
                "provider_configuration_sha256": sha("provider-configuration"),
                "model_identity_sha256": sha("fixture-model"),
                "agent_identity_sha256": sha("fixture-agent"),
                "tool_configuration_sha256": sha("fixture-tools"),
                "planner": "planner-v2.0.0",
                "projection": "projection-v2.0.0",
                "context_ir": "2.0.0",
                "estimator": "context-estimator-v1.0.0",
                "metric": "context-qualification-v1.0.0",
            },
            "route": "ATLAS_DEEP",
            "budget": {
                "max_records": 20,
                "max_source_bytes": 2000,
                "max_estimated_tokens": 1000,
                "max_work_units": 100,
                "controlled_dimension": None,
            },
            "observation_coverage": {
                "atlas_cost": True,
                "context_cost": True,
                "agent_cost": True,
                "read_requests": True,
                "reported_use": True,
                "task_result": True,
                "validation": True,
            },
            "sample_case_set_sha256": sha("case-set"),
        }

    def document(
        self,
        *,
        receipt: bool = True,
        attestation_kind: str = "validator_reported",
    ) -> dict[str, object]:
        required = self.task["required_evidence"]
        first = self.evidence_ref(required[0], provenance="supplied-a")
        second = self.evidence_ref(required[1], provenance="supplied-b")
        expanded = {
            "identity": {
                "kind": "file",
                "id": "native:src/expanded.rs",
                "range": None,
            },
            "source_revision_sha256": sha("expanded-source"),
            "provenance_sha256": sha("expanded-provenance"),
            "benchmark_locator": None,
        }
        packet = self.packet()
        packet_sha256 = qualification.canonical_sha256(packet)
        outcome_hash = sha("accepted-outcome")
        observation = {
            "schema_version": qualification.OBSERVATION_VERSION,
            "kind": "atlas-context-qualification-observation",
            "observation_id": "observation-fixture-a",
            "packet": packet,
            "packet_sha256": packet_sha256,
            "after_binding": {
                "repository_commit_after": packet["repository_commit_before"],
                "atlas_generation_after": packet["atlas_generation_before"],
                "source_snapshot_sha256_after": packet["source_snapshot_sha256_before"],
            },
            "supplied": [
                {**first, "source_bytes": 100, "selected_span_bytes": 80},
                {**second, "source_bytes": 300, "selected_span_bytes": 150},
            ],
            "read_requests": [first, first, expanded],
            "reported_used": [second, expanded],
            "atlas_cost": {
                "cpu_ms": None,
                "io_read_bytes": 4096,
                "io_write_bytes": 0,
                "db_work_units": 7,
                "visited_edges": 4,
                "processed_files": 2,
                "provider_scopes": 1,
                "work_units": 10,
                "retrieval_duration_ms": 5.0,
                "retrieval_environment_sha256": sha("environment"),
            },
            "context_cost": {
                "records": 2,
                "supplied_source_bytes": 400,
                "selected_source_span_bytes": 230,
                "estimated_tokens": 120,
                "working_set": [first["identity"], second["identity"]],
            },
            "agent_cost": {
                "input_tokens": 800,
                "output_tokens": 200,
                "tool_calls": 3,
                "independent_reads": 2,
                "expansion": 1,
                "rediscovery": 2,
            },
            "acquisition": {
                "status": "sufficient",
                "coverage": "complete",
                "native": False,
            },
            "task_result": {
                "agent_reported_accepted": True,
                "tests_validation": ["synthetic focused context metric contract"],
                "correct_target": True,
                "correctness": True,
                "outcome_artifact_sha256": outcome_hash,
                "time_to_correct_target_ms": 12.0,
                "time_to_accepted_result_ms": 20.0,
            },
            "error": None,
        }
        validation_receipt = None
        if receipt:
            validation_receipt = {
                "schema_version": qualification.RECEIPT_VERSION,
                "kind": "atlas-context-qualification-validation-receipt",
                "receipt_id": "receipt-fixture-a",
                "task_id": packet["task_id"],
                "corpus_sha256": packet["corpus_sha256"],
                "task_spec_sha256": packet["task_spec_sha256"],
                "validation_contract_sha256": packet["validation_contract_sha256"],
                "packet_sha256": packet_sha256,
                "observation_sha256": qualification.canonical_sha256(observation),
                "workspace_identity_sha256": packet["workspace_identity_sha256"],
                "repository_commit": packet["repository_commit_before"],
                "atlas_generation": packet["atlas_generation_before"],
                "source_snapshot_sha256": packet["source_snapshot_sha256_before"],
                "policies_sha256": qualification.canonical_sha256(packet["policies"]),
                "outcome_artifact_sha256": outcome_hash,
                "validation_outcome": "passed",
                "attestation_kind": attestation_kind,
                "evidence_summary": "Synthetic fixture validator reports the focused acceptance procedure passed.",
                "validator_identity_sha256": sha("fixture-validator"),
            }
        return {
            "schema_version": qualification.INPUT_VERSION,
            "kind": "atlas-context-qualification-input",
            "observation": observation,
            "validation_receipt": validation_receipt,
        }

    def reseal(self, document: dict[str, object]) -> None:
        observation = document["observation"]
        packet = observation["packet"]
        packet_hash = qualification.canonical_sha256(packet)
        observation["packet_sha256"] = packet_hash
        receipt = document["validation_receipt"]
        if receipt is None:
            return
        receipt.update({
            "task_id": packet["task_id"],
            "corpus_sha256": packet["corpus_sha256"],
            "task_spec_sha256": packet["task_spec_sha256"],
            "validation_contract_sha256": packet["validation_contract_sha256"],
            "packet_sha256": packet_hash,
            "observation_sha256": qualification.canonical_sha256(observation),
            "workspace_identity_sha256": packet["workspace_identity_sha256"],
            "repository_commit": packet["repository_commit_before"],
            "atlas_generation": packet["atlas_generation_before"],
            "source_snapshot_sha256": packet["source_snapshot_sha256_before"],
            "policies_sha256": qualification.canonical_sha256(packet["policies"]),
            "outcome_artifact_sha256": observation["task_result"]["outcome_artifact_sha256"],
        })

    def report(self, document: dict[str, object] | None = None) -> dict[str, object]:
        return qualification.build_report(self.corpus, document or self.document())

    def assert_fraction(
        self,
        metric: dict[str, object],
        numerator: int,
        denominator: int,
    ) -> None:
        self.assertEqual(metric["state"], "available")
        self.assertEqual(metric["numerator"], numerator)
        self.assertEqual(metric["denominator"], denominator)
        self.assertEqual(metric["ratio"], numerator / denominator)

    def test_hand_computable_metrics_keep_four_layers_and_use_claims_separate(self) -> None:
        report = self.report()

        self.assertEqual(
            set(report["measurements"]),
            {"atlas_cost", "context_cost", "agent_cost", "task_result"},
        )
        metrics = report["metrics"]
        self.assert_fraction(metrics["observed_precision"], 1, 2)
        self.assert_fraction(metrics["reported_use_precision"], 1, 2)
        self.assert_fraction(metrics["required_evidence_recall"], 2, 2)
        self.assertEqual(
            metrics["required_evidence_recall"]["requirement_coverage"], "known"
        )
        self.assertEqual(
            metrics["expansion"],
            {"state": "available", "count": 1, "unit": "items"},
        )
        self.assert_fraction(metrics["rediscovery_rate"], 2, 3)
        self.assert_fraction(metrics["observed_source_efficiency"], 100, 400)
        self.assertEqual(metrics["working_set"]["identities"], 2)
        self.assertEqual(
            report["measurements"]["context_cost"]["selected_source_span_bytes"],
            230,
        )
        self.assertEqual(report["task_outcome"]["state"], "reported_accepted")
        self.assertEqual(
            report["task_outcome"]["attestation_kind"], "validator_reported"
        )
        self.assertFalse(report["task_outcome"]["validation_execution_verified"])
        self.assertFalse(report["task_outcome"]["qualified_for_optimization"])
        self.assertEqual(
            metrics["useful_context_throughput_per_work_unit"]["reason"],
            "independent_execution_unverified",
        )
        self.assert_fraction(
            metrics["reported_useful_items_per_attested_work_unit"], 1, 10
        )
        self.assert_fraction(
            metrics[
                "reported_environment_qualified_useful_items_per_attested_ms"
            ],
            1,
            5,
        )
        self.assertEqual(report["acquisition"]["status"], "sufficient")

    def test_missing_coverage_and_zero_denominators_are_typed_unavailable(self) -> None:
        document = self.document(receipt=False)
        observation = document["observation"]
        observation["supplied"] = []
        observation["read_requests"] = []
        observation["reported_used"] = []
        observation["context_cost"] = {
            "records": 0,
            "supplied_source_bytes": 0,
            "selected_source_span_bytes": 0,
            "estimated_tokens": 0,
            "working_set": [],
        }
        observation["atlas_cost"]["work_units"] = None
        observation["atlas_cost"]["retrieval_duration_ms"] = None
        observation["atlas_cost"]["retrieval_environment_sha256"] = None
        observation["packet"]["observation_coverage"]["reported_use"] = False
        observation["task_result"]["correct_target"] = None
        observation["task_result"]["correctness"] = None
        observation["packet_sha256"] = qualification.canonical_sha256(observation["packet"])

        report = self.report(document)

        self.assertEqual(report["task_outcome"]["state"], "unavailable")
        self.assertEqual(report["metrics"]["observed_precision"]["reason"], "zero_denominator")
        self.assertEqual(
            report["metrics"]["reported_use_precision"]["reason"],
            "capture_coverage_unavailable",
        )
        self.assertEqual(report["metrics"]["rediscovery_rate"]["reason"], "zero_denominator")
        self.assertEqual(report["metrics"]["observed_source_efficiency"]["reason"], "zero_denominator")
        self.assertEqual(
            report["metrics"]["useful_context_throughput_per_work_unit"]["reason"],
            "independent_execution_unverified",
        )
        self.assertEqual(
            report["metrics"]["reported_useful_items_per_attested_work_unit"]["reason"],
            "reported_acceptance_unavailable",
        )
        self.assertIsNone(report["task_outcome"]["correct_target"]["value"])
        self.assertEqual(report["task_outcome"]["correct_target"]["state"], "unavailable")

    def test_strict_schema_rejects_unknown_fields_and_bool_as_integer(self) -> None:
        unknown = self.document()
        unknown["observation"]["atlas_cost"]["surprise"] = 1
        with self.assertRaisesRegex(qualification.QualificationError, "atlas_cost fields"):
            self.report(unknown)

        for path in [
            ("observation", "atlas_cost", "work_units"),
            ("observation", "context_cost", "records"),
            ("observation", "agent_cost", "tool_calls"),
            ("observation", "packet", "budget", "max_records"),
        ]:
            with self.subTest(path=path):
                document = self.document()
                value = document
                for key in path[:-1]:
                    value = value[key]
                value[path[-1]] = True
                with self.assertRaisesRegex(qualification.QualificationError, "integer"):
                    self.report(document)

    def test_range_paths_require_canonical_portable_relative_spelling(self) -> None:
        drive = "".join(("C", ":"))
        slash = "/"
        backslash = chr(92)
        invalid = [
            f"{drive}{slash}repo{slash}file.rs",
            f"{drive}repo{slash}file.rs",
            slash.join(("", "rooted", "file.rs")),
            f".{slash}src{slash}file.rs",
            f"src{slash}{slash}file.rs",
            f"src{slash}file.rs{slash}",
            f"src{slash}..{slash}file.rs",
            f"src{backslash}file.rs",
            f"{backslash}rooted{backslash}file.rs",
        ]
        for path in invalid:
            with self.subTest(path=path):
                with self.assertRaisesRegex(
                    qualification.QualificationError,
                    "canonical forward-slash-separated relative spelling",
                ):
                    qualification._validate_range(
                        {"path": path, "start_line": 1, "end_line": 2},
                        "fixture.range",
                    )

        self.assertEqual(
            qualification._validate_range(
                {"path": "src/foo.rs", "start_line": 1, "end_line": 2},
                "fixture.range",
            )["path"],
            "src/foo.rs",
        )

    def test_strict_file_loading_rejects_duplicate_json_nonfinite_and_bounds(self) -> None:
        duplicate = self.workspace / "duplicate.json"
        duplicate.write_text('{"schema_version":"x","schema_version":"y"}\n', encoding="utf-8")
        with self.assertRaisesRegex(qualification.QualificationError, "duplicate JSON field"):
            qualification.load_json_document(self.workspace, duplicate, "input")

        nonfinite = self.workspace / "nonfinite.json"
        nonfinite.write_text('{"value":NaN}\n', encoding="utf-8")
        with self.assertRaisesRegex(qualification.QualificationError, "non-finite"):
            qualification.load_json_document(self.workspace, nonfinite, "input")

        oversized = self.workspace / "oversized.json"
        oversized.write_bytes(b" " * (qualification.MAX_DOCUMENT_BYTES + 1))
        with self.assertRaisesRegex(qualification.QualificationError, "byte bound"):
            qualification.load_json_document(self.workspace, oversized, "input")

    def test_evidence_bound_and_conflicting_costs_fail_closed(self) -> None:
        over_bound = self.document()
        reference = over_bound["observation"]["read_requests"][0]
        over_bound["observation"]["read_requests"] = [reference] * 4097
        with self.assertRaisesRegex(qualification.QualificationError, "4096"):
            self.report(over_bound)

        conflict = self.document()
        duplicate = copy.deepcopy(conflict["observation"]["supplied"][0])
        duplicate["source_bytes"] += 1
        conflict["observation"]["supplied"].append(duplicate)
        conflict["observation"]["context_cost"]["records"] = 3
        conflict["observation"]["context_cost"]["supplied_source_bytes"] += duplicate["source_bytes"]
        conflict["observation"]["context_cost"]["selected_source_span_bytes"] += duplicate["selected_span_bytes"]
        conflict["observation"]["context_cost"]["working_set"].append(duplicate["identity"])
        with self.assertRaisesRegex(qualification.QualificationError, "conflicting supplied costs"):
            self.report(conflict)

    def test_changed_generation_commit_or_source_binding_is_rejected(self) -> None:
        fields = [
            ("repository_commit_after", "0" * 40),
            ("atlas_generation_after", "gen_changed"),
            ("source_snapshot_sha256_after", "0" * 64),
        ]
        for field, value in fields:
            with self.subTest(field=field):
                document = self.document()
                document["observation"]["after_binding"][field] = value
                with self.assertRaisesRegex(qualification.QualificationError, "changed during observation"):
                    self.report(document)

    def test_validation_receipt_binds_full_observation_and_every_contract_identity(self) -> None:
        missing_document = self.document(receipt=False)
        missing_document["observation"]["task_result"]["outcome_artifact_sha256"] = None
        missing_document["observation"]["task_result"]["correct_target"] = None
        missing_document["observation"]["task_result"]["correctness"] = None
        missing = self.report(missing_document)
        self.assertEqual(missing["task_outcome"]["state"], "unavailable")
        self.assertIsNone(missing["task_outcome"]["outcome_artifact_sha256"])

        failed_document = self.document()
        failed_document["validation_receipt"]["validation_outcome"] = "failed"
        failed = self.report(failed_document)
        self.assertEqual(failed["task_outcome"]["state"], "reported_rejected")

        changed_cost = self.document()
        changed_cost["observation"]["atlas_cost"]["work_units"] = 1
        with self.assertRaisesRegex(
            qualification.QualificationError, "receipt binding mismatch: observation_sha256"
        ):
            self.report(changed_cost)

        for field, value in [
            ("task_id", "wrong-task"),
            ("corpus_sha256", "0" * 64),
            ("task_spec_sha256", "0" * 64),
            ("validation_contract_sha256", "0" * 64),
            ("packet_sha256", "0" * 64),
            ("observation_sha256", "0" * 64),
            ("workspace_identity_sha256", "0" * 64),
            ("repository_commit", "0" * 40),
            ("atlas_generation", "wrong-generation"),
            ("source_snapshot_sha256", "0" * 64),
            ("policies_sha256", "0" * 64),
            ("outcome_artifact_sha256", "0" * 64),
        ]:
            with self.subTest(field=field):
                document = self.document()
                document["validation_receipt"][field] = value
                with self.assertRaisesRegex(
                    qualification.QualificationError, "receipt binding"
                ):
                    self.report(document)

    def test_passed_receipt_rejects_errors_false_outcomes_and_blocked_acquisition(self) -> None:
        mutations = [
            (
                "error",
                lambda document: document["observation"].update(
                    error={"kind": "runner_failure", "detail": "synthetic fixture"}
                ),
            ),
            (
                "wrong_target",
                lambda document: document["observation"]["task_result"].update(
                    correct_target=False
                ),
            ),
            (
                "incorrect",
                lambda document: document["observation"]["task_result"].update(
                    correctness=False
                ),
            ),
            (
                "missing_target",
                lambda document: document["observation"]["task_result"].update(
                    correct_target=None
                ),
            ),
            (
                "missing_correctness",
                lambda document: document["observation"]["task_result"].update(
                    correctness=None
                ),
            ),
            (
                "blocked",
                lambda document: document["observation"]["acquisition"].update(
                    status="blocked", coverage="partial", native=True
                ),
            ),
        ]
        for name, mutate in mutations:
            with self.subTest(name=name):
                document = self.document()
                mutate(document)
                self.reseal(document)
                with self.assertRaisesRegex(
                    qualification.QualificationError,
                    "passed validation receipt contradicts",
                ):
                    self.report(document)

    def test_attested_acceptance_never_enables_actual_useful_context_throughput(self) -> None:
        report = self.report(
            self.document(attestation_kind="operator_reported")
        )
        self.assertEqual(report["task_outcome"]["state"], "reported_accepted")
        self.assertFalse(report["task_outcome"]["validation_execution_verified"])
        self.assertEqual(
            report["metrics"]["useful_context_throughput_per_work_unit"]["reason"],
            "independent_execution_unverified",
        )
        self.assert_fraction(
            report["metrics"]["reported_useful_items_per_attested_work_unit"],
            1,
            10,
        )

        unsupported = self.document(
            attestation_kind="independently_executed"
        )
        with self.assertRaisesRegex(
            qualification.QualificationError, "attestation kind"
        ):
            self.report(unsupported)

    def test_attested_task_with_missing_work_or_latency_has_typed_unavailable_reported_throughput(self) -> None:
        document = self.document()
        document["observation"]["atlas_cost"]["work_units"] = None
        document["observation"]["atlas_cost"]["retrieval_duration_ms"] = None
        document["observation"]["atlas_cost"]["retrieval_environment_sha256"] = None
        self.reseal(document)

        report = self.report(document)

        self.assertEqual(report["task_outcome"]["state"], "reported_accepted")
        self.assertEqual(
            report["metrics"][
                "reported_useful_items_per_attested_work_unit"
            ]["reason"],
            "denominator_unavailable",
        )
        self.assertEqual(
            report["metrics"][
                "reported_environment_qualified_useful_items_per_attested_ms"
            ]["reason"],
            "denominator_unavailable",
        )

    def test_incompatible_versions_nonfinite_overflow_and_failed_claims_reject(self) -> None:
        for path, replacement in [
            (("schema_version",), "atlas-context-qualification-input-v9.0.0"),
            (
                ("observation", "schema_version"),
                "atlas-context-qualification-observation-v9.0.0",
            ),
            (
                ("validation_receipt", "schema_version"),
                "atlas-context-qualification-validation-receipt-v9.0.0",
            ),
            (
                ("observation", "packet", "policies", "metric"),
                "context-qualification-v9.0.0",
            ),
            (("observation", "packet", "policies", "context_ir"), "999.0.0"),
        ]:
            with self.subTest(path=path):
                document = self.document()
                target = document
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = replacement
                if "packet" in path:
                    self.reseal(document)
                with self.assertRaisesRegex(
                    qualification.QualificationError, "incompatible"
                ):
                    self.report(document)

        for value, message in [
            (float("inf"), "finite"),
            (qualification.MAX_SIGNED_63 + 1, "bounded"),
        ]:
            with self.subTest(value=value):
                document = self.document()
                document["observation"]["atlas_cost"]["cpu_ms"] = value
                with self.assertRaisesRegex(
                    qualification.QualificationError, message
                ):
                    self.report(document)

        failed = self.document()
        failed["observation"]["task_result"]["agent_reported_accepted"] = False
        self.reseal(failed)
        with self.assertRaisesRegex(
            qualification.QualificationError, "contradicts"
        ):
            self.report(failed)

    def comparable_pair(self) -> tuple[dict[str, object], dict[str, object]]:
        left_document = self.document()
        right_document = self.document()
        right_document["observation"]["observation_id"] = "observation-fixture-b"
        right_document["validation_receipt"]["receipt_id"] = "receipt-fixture-b"
        self.reseal(right_document)
        return self.report(left_document), self.report(right_document)

    def test_comparison_accepts_only_declared_budget_dimension(self) -> None:
        left_document = self.document()
        right_document = self.document()
        for document, records in ((left_document, 20), (right_document, 40)):
            document["observation"]["packet"]["budget"]["max_records"] = records
            document["observation"]["packet"]["budget"][
                "controlled_dimension"
            ] = "max_records"
            self.reseal(document)
        comparison = qualification.compare_reports(
            self.report(left_document), self.report(right_document)
        )
        self.assertEqual(comparison["state"], "comparable_attested_inputs")
        self.assertFalse(comparison["qualified_for_optimization"])
        self.assertEqual(comparison["controlled_dimension"], "max_records")

        incompatible_left = self.document()
        incompatible_right = self.document()
        incompatible_right["observation"]["packet"]["policies"][
            "model_identity_sha256"
        ] = sha("other-model")
        self.reseal(incompatible_right)
        with self.assertRaisesRegex(
            qualification.QualificationError, "policies"
        ):
            qualification.compare_reports(
                self.report(incompatible_left),
                self.report(incompatible_right),
            )

    def test_comparison_rejects_unvalidated_outcomes_and_frozen_identity_drift(self) -> None:
        base = self.document()
        cases: list[tuple[str, object, str]] = []

        unvalidated = self.document(receipt=False)
        cases.append(("unvalidated", unvalidated, "reported-accepted"))

        outcome = self.document()
        outcome["observation"]["task_result"][
            "outcome_artifact_sha256"
        ] = sha("different-outcome")
        self.reseal(outcome)
        cases.append(("outcome", outcome, "outcome identity"))

        generation = self.document()
        generation["observation"]["packet"][
            "atlas_generation_before"
        ] = "gen_fixture_002"
        generation["observation"]["after_binding"][
            "atlas_generation_after"
        ] = "gen_fixture_002"
        self.reseal(generation)
        cases.append(("generation", generation, "generation"))

        source = self.document()
        source["observation"]["packet"][
            "source_snapshot_sha256_before"
        ] = sha("other-source")
        source["observation"]["after_binding"][
            "source_snapshot_sha256_after"
        ] = sha("other-source")
        self.reseal(source)
        cases.append(("source", source, "source"))

        coverage = self.document()
        coverage["observation"]["packet"]["observation_coverage"][
            "agent_cost"
        ] = False
        self.reseal(coverage)
        cases.append(("coverage", coverage, "coverage"))

        case_set = self.document()
        case_set["observation"]["packet"][
            "sample_case_set_sha256"
        ] = sha("other-case-set")
        self.reseal(case_set)
        cases.append(("case", case_set, "sample/case"))

        route = self.document()
        route["observation"]["packet"]["route"] = "ATLAS_LIGHT"
        self.reseal(route)
        cases.append(("route", route, "route"))

        for name, document, message in cases:
            with self.subTest(name=name):
                with self.assertRaisesRegex(
                    qualification.QualificationError, message
                ):
                    qualification.compare_reports(
                        self.report(base), self.report(document)
                    )

    def test_read_and_reported_source_revision_conflicts_fail_closed(self) -> None:
        for collection in ("read_requests", "reported_used"):
            with self.subTest(collection=collection):
                document = self.document()
                conflicting = copy.deepcopy(
                    document["observation"][collection][0]
                )
                conflicting["source_revision_sha256"] = sha("stale-revision")
                locator = conflicting["benchmark_locator"]
                if locator is not None:
                    locator["mapping_sha256"] = qualification.canonical_sha256({
                        "benchmark_locator_id": locator["id"],
                        "identity": conflicting["identity"],
                        "source_revision_sha256": conflicting[
                            "source_revision_sha256"
                        ],
                    })
                document["observation"][collection].append(conflicting)
                with self.assertRaisesRegex(
                    qualification.QualificationError,
                    "another source revision",
                ):
                    self.report(document)

    def test_every_metric_group_respects_capture_coverage(self) -> None:
        context_missing = self.document()
        context_missing["observation"]["packet"]["observation_coverage"][
            "context_cost"
        ] = False
        self.reseal(context_missing)
        context_report = self.report(context_missing)
        for metric in (
            "observed_precision",
            "reported_use_precision",
            "required_evidence_recall",
            "observed_source_efficiency",
        ):
            self.assertEqual(
                context_report["metrics"][metric]["reason"],
                "capture_coverage_unavailable",
            )
        self.assertEqual(
            context_report["metrics"]["working_set"]["reason"],
            "capture_coverage_unavailable",
        )

        atlas_missing = self.document()
        atlas_missing["observation"]["packet"]["observation_coverage"][
            "atlas_cost"
        ] = False
        self.reseal(atlas_missing)
        atlas_report = self.report(atlas_missing)
        self.assertEqual(
            atlas_report["metrics"][
                "reported_useful_items_per_attested_work_unit"
            ]["reason"],
            "capture_coverage_unavailable",
        )

        for field in ("task_result", "validation"):
            with self.subTest(outcome_coverage=field):
                outcome_missing = self.document()
                outcome_missing["observation"]["packet"]["observation_coverage"][
                    field
                ] = False
                self.reseal(outcome_missing)
                self.assertEqual(
                    self.report(outcome_missing)["task_outcome"]["state"],
                    "unavailable",
                )

    def test_native_null_partial_and_direct_zero_context_acquisition_are_honest(self) -> None:
        for status, coverage in ((None, "unavailable"), ("partial", "partial")):
            with self.subTest(status=status):
                document = self.document(receipt=False)
                document["observation"]["acquisition"] = {
                    "status": status,
                    "coverage": coverage,
                    "native": True,
                }
                report = self.report(document)
                self.assertEqual(report["acquisition"]["status"], status)
                self.assertEqual(report["task_outcome"]["state"], "unavailable")

        direct = self.document(receipt=False)
        observation = direct["observation"]
        observation["packet"]["route"] = "DIRECT"
        observation["supplied"] = []
        observation["read_requests"] = []
        observation["reported_used"] = []
        observation["context_cost"] = {
            "records": 0,
            "supplied_source_bytes": 0,
            "selected_source_span_bytes": 0,
            "estimated_tokens": 0,
            "working_set": [],
        }
        observation["acquisition"] = {
            "status": "sufficient",
            "coverage": "complete",
            "native": True,
        }
        self.reseal(direct)
        direct_report = self.report(direct)
        self.assertEqual(direct_report["acquisition"]["status"], "sufficient")
        self.assertEqual(direct_report["task_outcome"]["state"], "unavailable")
        self.assertEqual(
            direct_report["metrics"]["observed_precision"]["reason"],
            "zero_denominator",
        )

    def test_collection_sum_overflow_rejects_without_uncaught_exception(self) -> None:
        document = self.document()
        document["observation"]["supplied"][0]["source_bytes"] = (
            qualification.MAX_SIGNED_63
        )
        document["observation"]["context_cost"]["supplied_source_bytes"] = (
            qualification.MAX_SIGNED_63
        )
        with self.assertRaisesRegex(
            qualification.QualificationError, "sum"
        ):
            self.report(document)

    def test_timing_only_change_preserves_semantic_hash_but_changes_full_hash(self) -> None:
        first_document = self.document()
        second_document = self.document()
        second_document["observation"]["atlas_cost"]["cpu_ms"] = 99.0
        second_document["observation"]["atlas_cost"][
            "retrieval_duration_ms"
        ] = 55.0
        second_document["observation"]["atlas_cost"][
            "retrieval_environment_sha256"
        ] = sha("other-environment")
        second_document["observation"]["task_result"][
            "time_to_correct_target_ms"
        ] = 88.0
        second_document["observation"]["task_result"][
            "time_to_accepted_result_ms"
        ] = 144.0
        self.reseal(second_document)

        first = self.report(first_document)
        second = self.report(second_document)

        self.assertEqual(
            first["semantic_report_sha256"], second["semantic_report_sha256"]
        )
        self.assertNotEqual(first["identity"]["observation_sha256"], second["identity"]["observation_sha256"])
        self.assertNotEqual(first["report_sha256"], second["report_sha256"])

    def test_tampered_and_resealed_report_cannot_bypass_nested_validation(self) -> None:
        valid = self.report()
        fabricated = copy.deepcopy(valid)
        fabricated["metrics"] = {"fabricated": True}
        fabricated["report_sha256"] = qualification.canonical_sha256({
            key: value
            for key, value in fabricated.items()
            if key != "report_sha256"
        })
        with self.assertRaisesRegex(
            qualification.QualificationError, "does not rebuild exactly"
        ):
            qualification.compare_reports(valid, fabricated)

        malformed = {
            "schema_version": qualification.REPORT_VERSION,
            "kind": "atlas-context-qualification-report",
            "harness_version": qualification.HARNESS_VERSION,
            "corpus": self.corpus["document"],
            "normalized_input": self.document(),
            "identity": {"task_id": self.task["task_id"], "budget": {}},
            "measurements": {},
            "metrics": {"fabricated": True},
            "acquisition": {},
            "task_outcome": {
                "state": "reported_accepted",
                "outcome_artifact_sha256": None,
            },
            "semantic_report_sha256": "0" * 64,
            "report_sha256": "0" * 64,
        }
        malformed["report_sha256"] = qualification.canonical_sha256({
            key: value
            for key, value in malformed.items()
            if key != "report_sha256"
        })
        with self.assertRaisesRegex(
            qualification.QualificationError, "does not rebuild exactly"
        ):
            qualification.compare_reports(valid, malformed)

    def test_corpus_integrity_matches_frozen_git_objects_and_all_classes(self) -> None:
        corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
        self.assertEqual(len(corpus["tasks"]), 11)
        self.assertEqual(
            {task["task_class"] for task in corpus["tasks"]},
            qualification.TASK_CLASSES,
        )
        commit = corpus["repository"]["commit"]
        for task in corpus["tasks"]:
            for evidence in task["required_evidence"]:
                path = evidence["range"]["path"]
                contents = subprocess.check_output(
                    ["git", "show", f"{commit}:{path}"],
                    cwd=SCRIPT.parents[1],
                )
                self.assertEqual(
                    hashlib.sha256(contents).hexdigest(),
                    evidence["source_revision_sha256"],
                )
                lines = contents.decode("utf-8").splitlines()
                self.assertLessEqual(evidence["range"]["end_line"], len(lines))
                if evidence["kind"] == "range":
                    self.assertTrue(evidence["id"].startswith("corpus-region:"))

    def test_offline_cli_loads_files_hashes_reports_and_enforces_compare_gate(self) -> None:
        input_a = self.workspace / "accepted-a.json"
        input_b = self.workspace / "accepted-b.json"
        input_a.write_bytes(FIXTURE_A.read_bytes())
        input_b.write_bytes(FIXTURE_B.read_bytes())
        report_a = self.workspace / "report-a.json"
        report_b = self.workspace / "report-b.json"

        for source, output in ((input_a, report_a), (input_b, report_b)):
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "report",
                    "--workspace",
                    str(self.workspace),
                    "--corpus",
                    str(self.corpus_path),
                    "--input",
                    str(source),
                    "--output",
                    str(output),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            payload = json.loads(completed.stdout)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(payload["sha256"], report["report_sha256"])
            self.assertEqual(report["task_outcome"]["state"], "reported_accepted")
            self.assertTrue(output.read_bytes().endswith(b"\n"))
            qualification._validate_report(report, "CLI report")

        comparison_path = self.workspace / "comparison.json"
        compared = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "compare",
                "--workspace",
                str(self.workspace),
                "--left",
                str(report_a),
                "--right",
                str(report_b),
                "--output",
                str(comparison_path),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(compared.returncode, 0, compared.stderr)
        comparison = json.loads(comparison_path.read_text(encoding="utf-8"))
        self.assertEqual(comparison["state"], "comparable_attested_inputs")
        self.assertEqual(comparison["controlled_dimension"], "max_records")
        self.assertFalse(comparison["qualified_for_optimization"])
        self.assertEqual(
            json.loads(compared.stdout)["sha256"],
            comparison["comparison_sha256"],
        )

    def test_report_content_and_hashes_are_deterministic(self) -> None:
        first = self.report()
        second = self.report(copy.deepcopy(self.document()))
        self.assertEqual(first, second)
        self.assertEqual(
            first["report_sha256"],
            qualification.canonical_sha256({
                key: value for key, value in first.items() if key != "report_sha256"
            }),
        )
        self.assertRegex(first["semantic_report_sha256"], r"^[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
