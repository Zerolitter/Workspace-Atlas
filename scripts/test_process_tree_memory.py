#!/usr/bin/env python3
"""Contract, fixture, and available-host tests for process-tree-memory.py."""
from __future__ import annotations

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("process-tree-memory.py")
SPEC = importlib.util.spec_from_file_location("process_tree_memory", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
memory = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = memory
SPEC.loader.exec_module(memory)


LABELS = {
    "phase": "no-change-reconcile",
    "filesystem_cache_state": "atlas-warm",
    "serving_state": "absent",
    "provider_cache_install_state": "typescript-only:installed",
    "build_state": "prebuilt",
    "catalogue_state": "prepared-baseline",
}

def progress_state(ordinal: int, phase: str = "capability-discovery",
                   state: str = "active", total: int = 135_000) -> dict[str, object]:
    return {
        **LABELS,
        "phase": phase,
        "progress_state": state,
        "progress_ordinal": ordinal,
        "progress_total": total,
    }


def observed(pid: int, identity: str, resident: int, private: int | None = None,
             high_water: int | None = None) -> dict[str, object]:
    return {
        "status": "observed",
        "pid": pid,
        "identity": identity,
        "resident_bytes": resident,
        "private_bytes": private,
        "high_water_bytes": high_water,
        "method": "fixture",
        "error": None,
    }

def complete_mutated_evidence(records: list[dict[str, object]]) -> tuple[bytes, dict[str, object]]:
    valid_records = memory.fixture_evidence_records(LABELS)
    valid_raw = memory.encode_raw_records(valid_records)
    summary = memory.build_summary(
        valid_records, valid_raw, memory.platform_metadata("win32"), exit_code=0,
        cadence_ns=memory.CADENCE_NS, idle_duration_ns=memory.IDLE_DURATION_NS,
        maximum_raw_samples=1_000,
    )
    raw = memory.encode_raw_records(records)
    idle = [record for record in records if record["control"] == "idle"]
    campaign = [record for record in records if record["control"] == "campaign"]
    summary["raw_sha256"] = hashlib.sha256(raw).hexdigest()
    summary["raw_record_count"] = len(records)
    summary["baseline"] = memory._baseline_summary(idle)
    summary["campaign"] = memory._campaign_summary(campaign)
    summary["events"] = memory._event_counts(records)
    return raw, summary

def write_segmented_fixture(
    directory: Path,
    *,
    maximum_segment_samples: int,
    maximum_segment_bytes: int,
) -> tuple[list[dict[str, object]], dict[str, object], Path]:
    records = memory.fixture_evidence_records(LABELS)
    prefix = "fixture-memory-raw-v2"
    writer = memory.SegmentedRawWriter(
        directory,
        prefix,
        maximum_segment_samples=maximum_segment_samples,
        maximum_segment_bytes=maximum_segment_bytes,
    )
    retained = copy.deepcopy(records)
    for record in retained:
        writer.write(record)
    segments = writer.finish()
    summary = memory.build_segmented_summary(
        retained,
        segments,
        memory.platform_metadata("win32"),
        segment_name_prefix=prefix,
        exit_code=0,
        cadence_ns=memory.CADENCE_NS,
        idle_duration_ns=memory.IDLE_DURATION_NS,
        maximum_segment_samples=maximum_segment_samples,
        maximum_segment_bytes=maximum_segment_bytes,
        raw_sha256=writer.digest.hexdigest(),
        raw_byte_length=writer.bytes_written,
    )
    summary_path = directory / "fixture-memory-summary-v2.json"
    summary_path.write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    return retained, summary, summary_path




class ParserFixtures(unittest.TestCase):
    def test_linux_status_parses_rss_and_high_water_bytes(self) -> None:
        rss, hwm = memory.parse_linux_status(
            "Name:\tatlas\nVmHWM:\t4097 kB\nVmRSS:\t3073 kB\n"
        )
        self.assertEqual((rss, hwm), (3_146_752, 4_195_328))

    def test_linux_status_rejects_missing_negative_and_overflow_values(self) -> None:
        for text, expected in [
            ("Name:\tatlas\n", "VmRSS"),
            ("VmRSS:\t-1 kB\n", "non-negative"),
            (f"VmRSS:\t{memory.MAX_BYTES // 1024 + 1} kB\n", "overflow"),
        ]:
            with self.subTest(text=text):
                with self.assertRaisesRegex(memory.EvidenceError, expected):
                    memory.parse_linux_status(text)

    def test_linux_stat_handles_spaces_and_parentheses_in_process_name(self) -> None:
        fields = ["S", "77", "88"] + ["0"] * 16 + ["123456"] + ["0"] * 4
        ppid, process_group, identity = memory.parse_linux_stat(
            "42 (provider worker (one)) " + " ".join(fields)
        )
        self.assertEqual((ppid, process_group), (77, 88))
        self.assertEqual(identity, "linux-starttime:123456")

        linux_tree = [
            memory.ProcessNode(42, 1, identity, 1024, high_water_bytes=2048),
            memory.ProcessNode(43, 42, "linux-starttime:123457", 512, high_water_bytes=1024),
            memory.ProcessNode(44, 43, "linux-starttime:123458", 256, high_water_bytes=512),
        ]
        self.assertEqual(memory.descendant_pids(linux_tree, 42, 8), [43, 44])

    def test_macos_ps_parses_tree_fixture_and_marks_identity_limitation(self) -> None:
        rows = memory.parse_macos_ps(
            "  10     1    10  2048 Mon Sep  6 12:00:00 2026\n"
            "  11    10    10   512 Mon Sep  6 12:00:01 2026\n"
        )
        self.assertEqual(
            [(row.pid, row.parent_pid, row.process_group_id) for row in rows],
            [(10, 1, 10), (11, 10, 10)],
        )
        self.assertEqual(rows[0].resident_bytes, 2_097_152)
        self.assertTrue(rows[0].identity.startswith("macos-lstart:"))
        self.assertEqual(memory.descendant_pids(rows, 10, 8), [11])

    def test_macos_ps_rejects_malformed_negative_and_overflow_rows(self) -> None:
        fixtures = [
            "garbage\n",
            "10 1 10 -1 Mon Sep  6 12:00:00 2026\n",
            f"10 1 10 {memory.MAX_BYTES // 1024 + 1} Mon Sep  6 12:00:00 2026\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture):
                with self.assertRaises(memory.EvidenceError):
                    memory.parse_macos_ps(fixture)

    def test_windows_process_tree_fixture_is_transitive_bounded_and_cycle_safe(self) -> None:
        rows = [
            memory.ProcessNode(1, 0, "w:1", 1),
            memory.ProcessNode(2, 1, "w:2", 1),
            memory.ProcessNode(3, 2, "w:3", 1),
            memory.ProcessNode(4, 99, "w:4", 1),
            memory.ProcessNode(5, 6, "w:5", 1),
            memory.ProcessNode(6, 5, "w:6", 1),
        ]
        self.assertEqual(memory.descendant_pids(rows, 1, 8), [2, 3])
        with self.assertRaisesRegex(memory.EvidenceError, "process bound"):
            memory.descendant_pids(rows, 1, 1)

    def test_posix_group_fixtures_retain_reparented_and_new_post_root_descendants(self) -> None:
        class LinuxFixture(memory.LinuxObserver):
            def __init__(self, nodes: list[object]):
                self.nodes = nodes
                self.direct_gaps: dict[int, dict[str, object]] = {}

            def _nodes(self) -> tuple[list[object], list[str]]:
                return self.nodes, []

            def observe_process(self, pid: int) -> dict[str, object]:
                for node in self.nodes:
                    if node.pid == pid:
                        return memory.observed_measurement(node, self.method)
                return self.direct_gaps.get(
                    pid, memory.measurement_gap(pid, "exited", "fixture absent", self.method)
                )

        class MacOSFixture(memory.MacOSObserver):
            def __init__(self, nodes: list[object]):
                self.nodes = nodes
                self.direct_gaps: dict[int, dict[str, object]] = {}

            def _nodes(self) -> list[object]:
                return self.nodes

            def observe_process(self, pid: int) -> dict[str, object]:
                for node in self.nodes:
                    if node.pid == pid:
                        return memory.observed_measurement(node, self.method)
                return self.direct_gaps.get(
                    pid, memory.measurement_gap(pid, "exited", "fixture absent", self.method)
                )

        for observer_type, prefix in (
            (LinuxFixture, "linux-starttime:"),
            (MacOSFixture, "macos-lstart:"),
        ):
            with self.subTest(platform=observer_type.__name__):
                old_identity = prefix + "one"
                nodes = [
                    memory.ProcessNode(
                        101, 1, old_identity, 300, process_group_id=100
                    ),
                    memory.ProcessNode(
                        102, 101, prefix + "two", 400, process_group_id=100
                    ),
                ]
                observer = observer_type(nodes)
                snapshot = observer.observe_tree(
                    100, retained={101: old_identity}, contained_group_id=100
                )
                self.assertEqual(snapshot.atlas["status"], "gap")
                self.assertEqual(
                    [provider["pid"] for provider in snapshot.providers], [101, 102]
                )
                aggregate = memory.concurrent_totals(snapshot.atlas, snapshot.providers)
                self.assertEqual(aggregate["provider_resident_bytes"], 700)
                self.assertIsNone(aggregate["resident_bytes"])
                first = memory.concurrent_totals(
                    observed(100, prefix + "root", 100),
                    [observed(101, old_identity, 50)],
                )
                summary = memory.summarize_campaign([first, aggregate])
                self.assertEqual(
                    summary["provider_only"]["resident_bytes"]["peak"], 700
                )
                self.assertEqual(
                    summary["concurrent_aggregate"]["resident_bytes"]["peak"], 150
                )

                observer.nodes = [
                    memory.ProcessNode(
                        101, 1, prefix + "reused", 999, process_group_id=100
                    )
                ]
                reused = observer.observe_tree(
                    100, retained={101: old_identity}, contained_group_id=100
                ).providers
                self.assertEqual(reused[0]["status"], "gap")
                self.assertEqual(reused[0]["error"]["kind"], "pid_reuse")

                observer.nodes = []
                observer.direct_gaps[101] = memory.measurement_gap(
                    101, "permission_denied", "fixture denied", observer.method
                )
                denied = memory.tracked_process_measurements(
                    [], [101], {101: old_identity}, observer.method,
                    observer.observe_process, atlas_pid=100,
                )
                self.assertEqual(denied[0]["identity"], old_identity)
                self.assertEqual(
                    memory.retained_descendant_identities({101: old_identity}, denied),
                    {101: old_identity},
                )
                observer.direct_gaps.clear()
                exited = observer.observe_tree(
                    100, retained={101: old_identity}, contained_group_id=100
                ).providers
                self.assertEqual(exited[0]["error"]["kind"], "exited")
                self.assertEqual(exited[0]["identity"], old_identity)
                self.assertEqual(
                    memory.retained_descendant_identities({101: old_identity}, exited),
                    {},
                )


class ArithmeticAndRecordTests(unittest.TestCase):
    def test_nearest_rank_percentiles_and_empty_values(self) -> None:
        self.assertEqual(memory.statistics([5, 1, 4, 2, 3]), {
            "count": 5, "p50": 3, "p95": 5, "peak": 5,
        })
        self.assertEqual(memory.statistics([]), {
            "count": 0, "p50": None, "p95": None, "peak": None,
        })

    def test_concurrent_aggregate_uses_only_measures_in_one_sample(self) -> None:
        first = memory.concurrent_totals(
            observed(10, "a", 100, 70), [observed(11, "p1", 10, 7)]
        )
        second = memory.concurrent_totals(
            observed(10, "a", 20, 15), [observed(11, "p1", 200, 150)]
        )
        self.assertEqual(first["resident_bytes"], 110)
        self.assertEqual(second["resident_bytes"], 220)
        summary = memory.summarize_campaign([first, second])
        self.assertEqual(summary["atlas_only"]["resident_bytes"]["peak"], 100)
        self.assertEqual(summary["provider_only"]["resident_bytes"]["peak"], 200)
        self.assertEqual(summary["concurrent_aggregate"]["resident_bytes"]["peak"], 220)
        self.assertNotEqual(300, summary["concurrent_aggregate"]["resident_bytes"]["peak"])

    def test_provider_only_statistics_survive_atlas_gap(self) -> None:
        atlas_gap = memory.measurement_gap(
            10, "exited", "owned root handle reported exit", "fixture", identity="atlas"
        )
        aggregate = memory.concurrent_totals(
            atlas_gap, [observed(11, "provider", 300, 200)]
        )
        self.assertEqual(aggregate["status"], "gap")
        self.assertIsNone(aggregate["atlas_resident_bytes"])
        self.assertEqual(aggregate["provider_resident_bytes"], 300)
        self.assertIsNone(aggregate["resident_bytes"])
        summary = memory.summarize_campaign([aggregate])
        self.assertEqual(summary["atlas_only"]["resident_bytes"]["count"], 0)
        self.assertEqual(summary["provider_only"]["resident_bytes"]["peak"], 300)
        self.assertEqual(summary["concurrent_aggregate"]["resident_bytes"]["count"], 0)

    def test_unavailable_measure_never_becomes_zero_or_success(self) -> None:
        gap = memory.measurement_gap(44, "permission_denied", "access denied", "fixture")
        aggregate = memory.concurrent_totals(gap, [])
        self.assertEqual(gap["status"], "gap")
        self.assertIsNone(gap["resident_bytes"])
        self.assertEqual(aggregate["status"], "gap")
        with self.assertRaisesRegex(memory.EvidenceError, "observed"):
            memory.validate_measurement({**gap, "status": "observed", "resident_bytes": 0})

    def test_sampling_gap_reports_missed_intervals(self) -> None:
        self.assertEqual(memory.sampling_gap(1_000, 1_049, 50)["status"], "on_schedule")
        gap = memory.sampling_gap(1_000, 1_151, 50)
        self.assertEqual(gap["status"], "missed_deadline")
        self.assertEqual(gap["missed_intervals"], 3)
        self.assertEqual(gap["lateness_ns"], 151)

    def test_descendant_churn_exit_and_pid_reuse_are_explicit(self) -> None:
        previous = {21: "birth-a", 22: "birth-b"}
        current = [
            observed(21, "birth-c", 10),
            memory.measurement_gap(23, "exited", "exited during observation", "fixture",
                                   identity="birth-d"),
        ]
        events = memory.descendant_events(previous, current)
        self.assertIn({"type": "pid_reuse", "pid": 21,
                       "previous_identity": "birth-a", "current_identity": "birth-c"}, events)
        self.assertIn({"type": "descendant_exited", "pid": 22, "identity": "birth-b"}, events)
        self.assertTrue(any(event["type"] == "descendant_measurement_gap" and event["pid"] == 23
                            for event in events))

    def test_labels_are_closed_nonempty_and_injection_is_data(self) -> None:
        memory.validate_labels(LABELS)
        for mutation in [
            {**LABELS, "phase": ""},
            {**LABELS, "extra": "forbidden"},
            {key: value for key, value in LABELS.items() if key != "build_state"},
        ]:
            with self.subTest(mutation=mutation):
                with self.assertRaises(memory.EvidenceError):
                    memory.validate_labels(mutation)
        command = memory.build_atlas_command(
            Path("atlas bench;not-a-shell"), Path("scenario ; still-one-arg.json"),
            Path("manifests & literal"), Path("target/evidence"), Path("target/labels.json"),
        )
        self.assertEqual(command[0], "atlas bench;not-a-shell")
        self.assertIn("scenario ; still-one-arg.json", command)
        self.assertNotIn("shell", memory.subprocess_invocation_contract())

    def test_progress_is_monotonic_bounded_rate_limited_and_redacted(self) -> None:
        emitted: list[str] = []
        reporter = memory.ProgressReporter(emitted.append)
        for state in [
            progress_state(0, "harness-startup", "starting"),
            progress_state(1),
            progress_state(2),
            progress_state(1_000),
            progress_state(1_001),
            progress_state(1_002, "fixed-incremental-reconcile"),
            progress_state(135_000, "progressive-deep-route-compiler", "complete"),
        ]:
            reporter.observe(state)
        self.assertEqual(
            emitted,
            [
                "ATLAS_CAPACITY_PROGRESS state=starting ordinal=0 total=135000 phase=harness-startup",
                "ATLAS_CAPACITY_PROGRESS state=active ordinal=1 total=135000 phase=capability-discovery",
                "ATLAS_CAPACITY_PROGRESS state=active ordinal=1001 total=135000 phase=capability-discovery",
                "ATLAS_CAPACITY_PROGRESS state=active ordinal=1002 total=135000 phase=fixed-incremental-reconcile",
                "ATLAS_CAPACITY_PROGRESS state=complete ordinal=135000 total=135000 phase=progressive-deep-route-compiler",
            ],
        )

        regression_output: list[str] = []
        regression = memory.ProgressReporter(regression_output.append)
        regression.observe(progress_state(42))
        with self.assertRaisesRegex(memory.EvidenceError, "regressed"):
            regression.observe(progress_state(41))
        self.assertEqual(len(regression_output), 1)

        for invalid in [
            progress_state(1, "private/path"),
            progress_state(135_001),
            progress_state(1, total=134_999),
        ]:
            private_output: list[str] = []
            with self.subTest(invalid=invalid):
                with self.assertRaises(memory.EvidenceError):
                    memory.ProgressReporter(private_output.append).observe(invalid)
                self.assertEqual(private_output, [])

    def test_identical_starting_progress_is_emitted_once(self) -> None:
        starting = progress_state(0, "harness-startup", "starting")
        emitted: list[str] = []
        reporter = memory.ProgressReporter(emitted.append)

        reporter.observe(starting)
        reporter.observe(dict(starting))

        self.assertEqual(
            emitted,
            [
                "ATLAS_CAPACITY_PROGRESS state=starting ordinal=0 "
                "total=135000 phase=harness-startup"
            ],
        )

    def test_complete_progress_is_idempotent_only_for_identical_final_state(self) -> None:
        completed = progress_state(
            33_750,
            "progressive-deep-route-compiler",
            "complete",
            total=33_750,
        )
        completed_line = (
            "ATLAS_CAPACITY_PROGRESS state=complete ordinal=33750 "
            "total=33750 phase=progressive-deep-route-compiler"
        )
        emitted: list[str] = []
        reporter = memory.ProgressReporter(emitted.append)
        reporter.observe(completed)
        reporter.observe(dict(completed))
        self.assertEqual(emitted, [completed_line])
        mutations = {
            "state": {**completed, "progress_state": "active"},
            "ordinal": {**completed, "progress_ordinal": 33_749},
            "total": {**completed, "progress_total": 135_000},
            "phase": {**completed, "phase": "fixed-incremental-reconcile"},
        }
        for field, mutation in mutations.items():
            with self.subTest(field=field):
                rejecting_reporter = memory.ProgressReporter(lambda _: None)
                rejecting_reporter.observe(completed)
                with self.assertRaises(memory.EvidenceError) as raised:
                    rejecting_reporter.observe(mutation)
                self.assertIn(
                    str(raised.exception),
                    (
                        "capacity progress changed after its final state",
                        "capacity progress state is malformed or outside its fixed bound",
                    ),
                )

        rejecting_reporter = memory.ProgressReporter(lambda _: None)
        rejecting_reporter.observe(completed)
        invalid = {**completed, "private_detail": "must-not-be-echoed"}
        with self.assertRaises(memory.EvidenceError) as raised:
            rejecting_reporter.observe(invalid)
        self.assertNotIn("must-not-be-echoed", str(raised.exception))

    def test_post_completion_labels_only_state_fails_closed_without_mutation(self) -> None:
        completed = progress_state(
            33_750,
            "progressive-deep-route-compiler",
            "complete",
            total=33_750,
        )
        completed_line = (
            "ATLAS_CAPACITY_PROGRESS state=complete ordinal=33750 "
            "total=33750 phase=progressive-deep-route-compiler"
        )
        labels_only = {
            **LABELS,
            "filesystem_cache_state": "private-downgrade-sentinel",
        }
        compatibility_output: list[str] = []
        compatibility_reporter = memory.ProgressReporter(compatibility_output.append)
        compatibility_state = (
            compatibility_reporter.last_observed_ordinal,
            compatibility_reporter.last_emitted_ordinal,
            compatibility_reporter.last_phase,
            compatibility_reporter.last_state,
            compatibility_reporter.progress_total,
        )
        compatibility_reporter.observe(labels_only)
        self.assertEqual(
            (
                compatibility_reporter.last_observed_ordinal,
                compatibility_reporter.last_emitted_ordinal,
                compatibility_reporter.last_phase,
                compatibility_reporter.last_state,
                compatibility_reporter.progress_total,
            ),
            compatibility_state,
        )
        self.assertEqual(compatibility_output, [])
        emitted: list[str] = []
        reporter = memory.ProgressReporter(emitted.append)
        reporter.observe(completed)
        state_before = (
            reporter.last_observed_ordinal,
            reporter.last_emitted_ordinal,
            reporter.last_phase,
            reporter.last_state,
            reporter.progress_total,
        )

        with self.assertRaises(memory.EvidenceError) as raised:
            reporter.observe(labels_only)

        self.assertEqual(
            str(raised.exception),
            "capacity progress changed after its final state",
        )
        self.assertEqual(
            (
                reporter.last_observed_ordinal,
                reporter.last_emitted_ordinal,
                reporter.last_phase,
                reporter.last_state,
                reporter.progress_total,
            ),
            state_before,
        )
        self.assertEqual(emitted, [completed_line])
        self.assertNotIn("private-downgrade-sentinel", str(raised.exception))

    def test_progress_early_exit_reports_last_active_without_accepting_completion(self) -> None:
        emitted: list[str] = []
        reporter = memory.ProgressReporter(emitted.append)
        reporter.observe(progress_state(0, "harness-startup", "starting"))
        reporter.observe(progress_state(42))
        reporter.finish(23)
        self.assertEqual(
            emitted[-1],
            "ATLAS_CAPACITY_PROGRESS state=stopped ordinal=42 total=135000 phase=capability-discovery",
        )
        self.assertFalse(any("secret" in line or "\\" in line or "/" in line for line in emitted))
        with self.assertRaises(memory.EvidenceError):
            reporter.observe(progress_state(43))

    def test_progress_envelope_does_not_change_raw_or_summary_evidence(self) -> None:
        labels, progress = memory._split_label_state(
            progress_state(42, LABELS["phase"])
        )
        self.assertEqual(labels, LABELS)
        self.assertEqual(progress["progress_ordinal"], 42)
        self.assertTrue(memory.PROGRESS_KEYS.isdisjoint(labels))

        baseline_records = memory.fixture_evidence_records(LABELS)
        progress_records = memory.fixture_evidence_records(labels)
        baseline_raw = memory.encode_raw_records(baseline_records)
        progress_raw = memory.encode_raw_records(progress_records)
        self.assertEqual(progress_raw, baseline_raw)
        baseline_summary = memory.build_summary(
            baseline_records, baseline_raw, memory.platform_metadata("win32"),
            exit_code=0, cadence_ns=memory.CADENCE_NS,
            idle_duration_ns=memory.IDLE_DURATION_NS, maximum_raw_samples=1_000,
        )
        progress_summary = memory.build_summary(
            progress_records, progress_raw, memory.platform_metadata("win32"),
            exit_code=0, cadence_ns=memory.CADENCE_NS,
            idle_duration_ns=memory.IDLE_DURATION_NS, maximum_raw_samples=1_000,
        )
        self.assertEqual(progress_summary, baseline_summary)

class BaselineAndEvidenceTests(unittest.TestCase):
    def test_five_second_idle_control_uses_same_50ms_cadence(self) -> None:
        clock = memory.FakeClock()
        observer = memory.FixtureObserver(observed(900, "observer", 4096, 2048))
        records = memory.collect_idle_control(
            observer, clock.monotonic_ns, clock.sleep, sampler_pid=900,
            duration_ns=5_000_000_000, cadence_ns=50_000_000, max_samples=102,
        )
        self.assertEqual(len(records), 101)
        self.assertEqual(records[0]["scheduled_monotonic_ns"], 0)
        self.assertEqual(records[-1]["scheduled_monotonic_ns"], 5_000_000_000)
        self.assertTrue(all(record["control"] == "idle" for record in records))
        self.assertTrue(all(record["atlas_pid"] is None for record in records))
        self.assertTrue(all(record["sampling_gap"]["status"] == "on_schedule"
                            for record in records))

    def test_early_sleep_returns_cannot_precede_idle_or_campaign_schedule(self) -> None:
        clock = memory.FakeClock()
        wake_count = 0

        def early_sleep(seconds: float) -> None:
            nonlocal wake_count
            wake_count += 1
            clock.now += max(1, round(seconds * 500_000_000))

        observer_measurement = observed(900, "observer", 4096, 2048)
        observer = memory.FixtureObserver(observer_measurement)
        idle = memory.collect_idle_control(
            observer, clock.monotonic_ns, early_sleep, sampler_pid=900,
            duration_ns=5_000_000_000, cadence_ns=50_000_000, max_samples=102,
        )
        self.assertGreater(wake_count, 100)
        self.assertTrue(all(
            record["monotonic_ns"] >= record["scheduled_monotonic_ns"]
            for record in idle
        ))
        self.assertTrue(all(
            later["scheduled_monotonic_ns"] - earlier["scheduled_monotonic_ns"]
            == memory.CADENCE_NS
            for earlier, later in zip(idle, idle[1:])
        ))
        self.assertTrue(all(
            record["sampling_gap"] == {
                "status": "on_schedule", "lateness_ns": 0, "missed_intervals": 0,
            }
            for record in idle
        ))

        campaign_start = clock.monotonic_ns() + memory.CADENCE_NS
        campaign = []
        snapshot = memory.TreeSnapshot(observer_measurement, [])
        for index in range(3):
            scheduled = campaign_start + index * memory.CADENCE_NS
            actual = memory.wait_until_scheduled(
                scheduled, clock.monotonic_ns, early_sleep
            )
            campaign.append(memory._campaign_record(
                index, scheduled, actual, 900, LABELS,
                observer_measurement, snapshot, {},
            ))
        self.assertTrue(all(
            record["monotonic_ns"] >= record["scheduled_monotonic_ns"]
            for record in campaign
        ))
        self.assertTrue(all(
            later["scheduled_monotonic_ns"] - earlier["scheduled_monotonic_ns"]
            == memory.CADENCE_NS
            for earlier, later in zip(campaign, campaign[1:])
        ))
        self.assertTrue(all(
            record["sampling_gap"] == {
                "status": "on_schedule", "lateness_ns": 0, "missed_intervals": 0,
            }
            for record in campaign
        ))

    def test_idle_control_fails_closed_when_bound_cannot_hold_five_seconds(self) -> None:
        clock = memory.FakeClock()
        observer = memory.FixtureObserver(observed(900, "observer", 4096))
        with self.assertRaisesRegex(memory.EvidenceError, "sample bound"):
            memory.collect_idle_control(
                observer, clock.monotonic_ns, clock.sleep, sampler_pid=900,
                duration_ns=5_000_000_000, cadence_ns=50_000_000, max_samples=100,
            )

    def test_writer_rolls_over_instead_of_ending_a_valid_logical_run(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            writer = memory.SegmentedRawWriter(
                Path(directory), "small-bound-raw-v2", maximum_segment_samples=2,
                maximum_segment_bytes=1_000,
            )
            for sequence in range(3):
                writer.write({"sequence": sequence, "value": sequence})
            segments = writer.finish()

            self.assertEqual([segment["record_count"] for segment in segments], [2, 1])
            self.assertEqual(
                [segment["first_sequence"] for segment in segments], [0, 2]
            )
    def test_segmented_evidence_round_trips_count_and_byte_rollovers(self) -> None:
        records = memory.fixture_evidence_records(LABELS)
        encoded = memory.encode_raw_records(records).splitlines(keepends=True)
        byte_bound = max(map(len, encoded))
        for name, sample_bound, maximum_bytes in [
            ("count", 60, memory.MAX_RAW_BYTES),
            ("byte", memory.MAX_RAW_SAMPLES, byte_bound),
        ]:
            with self.subTest(rollover=name):
                with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
                    retained, summary, summary_path = write_segmented_fixture(
                        Path(directory),
                        maximum_segment_samples=sample_bound,
                        maximum_segment_bytes=maximum_bytes,
                    )
                    memory.validate_segmented_evidence(summary_path)
                    self.assertGreater(len(summary["segments"]), 1)
                    self.assertEqual(
                        [record["sequence"] for record in retained],
                        list(range(len(retained))),
                    )
                    self.assertEqual(
                        sum(segment["record_count"] for segment in summary["segments"]),
                        len(retained),
                    )
                    self.assertEqual(
                        [segment["order"] for segment in summary["segments"]],
                        list(range(len(summary["segments"]))),
                    )

    def test_segmented_manifest_rejects_every_binding_and_sequence_mutation(self) -> None:
        for mutation in [
            "missing", "reordered", "duplicate", "extra", "tampered", "truncated",
            "path_escape", "discontinuous", "whole_digest",
        ]:
            with self.subTest(mutation=mutation):
                with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
                    root = Path(directory)
                    _, summary, summary_path = write_segmented_fixture(
                        root,
                        maximum_segment_samples=60,
                        maximum_segment_bytes=memory.MAX_RAW_BYTES,
                    )
                    first = root / summary["segments"][0]["path"]
                    second = root / summary["segments"][1]["path"]
                    if mutation == "missing":
                        second.unlink()
                    elif mutation == "reordered":
                        summary["segments"].reverse()
                    elif mutation == "duplicate":
                        summary["segments"][1] = copy.deepcopy(summary["segments"][0])
                    elif mutation == "extra":
                        (root / "fixture-memory-raw-v2-999999.ndjson").write_bytes(b"extra\n")
                    elif mutation == "tampered":
                        raw = first.read_bytes()
                        first.write_bytes(bytes([raw[0] ^ 1]) + raw[1:])
                    elif mutation == "truncated":
                        second.write_bytes(second.read_bytes()[:-1])
                    elif mutation == "path_escape":
                        summary["segments"][0]["path"] = "../escape.ndjson"
                    elif mutation == "discontinuous":
                        summary["segments"][1]["first_sequence"] += 1
                    elif mutation == "whole_digest":
                        summary["raw_sha256"] = "0" * 64
                    summary_path.write_text(
                        json.dumps(summary, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8",
                        newline="\n",
                    )
                    with self.assertRaises((memory.EvidenceError, OSError)):
                        memory.validate_segmented_evidence(summary_path)

    def test_segmented_evidence_rejects_linked_segment(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            _, summary, summary_path = write_segmented_fixture(
                root,
                maximum_segment_samples=60,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            first = root / summary["segments"][0]["path"]
            second = root / summary["segments"][1]["path"]
            second.unlink()
            try:
                second.symlink_to(first)
            except OSError as error:
                self.skipTest(f"host cannot create a file symlink: {error}")
            with self.assertRaisesRegex(memory.EvidenceError, "linked"):
                memory.validate_segmented_evidence(summary_path)
    def test_segmented_evidence_rejects_external_hard_link(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            evidence = root / "evidence"
            evidence.mkdir()
            _, summary, summary_path = write_segmented_fixture(
                evidence,
                maximum_segment_samples=60,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            first = evidence / summary["segments"][0]["path"]
            external = root / "external-segment.ndjson"
            external.write_bytes(first.read_bytes())
            first.unlink()
            try:
                os.link(external, first)
            except OSError as error:
                self.skipTest(f"host cannot create a hard link: {error}")
            self.assertEqual(first.stat().st_nlink, 2)
            with self.assertRaisesRegex(memory.EvidenceError, "linked"):
                memory.validate_segmented_evidence(summary_path)

    def test_segmented_validation_binds_consumption_to_open_handle(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            _, summary, summary_path = write_segmented_fixture(
                root,
                maximum_segment_samples=60,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            first = root / summary["segments"][0]["path"]
            replacement = root / "replacement.ndjson"
            replacement.write_bytes(b'{"kind":"replacement"}\n')

            def mutate_after_open(path: Path, order: int) -> None:
                if order == 0:
                    path.write_bytes(replacement.read_bytes())

            with self.assertRaises(memory.EvidenceError):
                memory.validate_segmented_evidence(
                    summary_path, before_segment_read=mutate_after_open
                )

    def test_memory_generations_and_undeclared_v2_outputs_cannot_mix(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            records, _, summary_path = write_segmented_fixture(
                root,
                maximum_segment_samples=60,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            (root / memory.LEGACY_RAW_NAME).write_bytes(b"legacy\n")
            (root / memory.LEGACY_SUMMARY_NAME).write_bytes(b"{}\n")
            with self.assertRaisesRegex(memory.EvidenceError, "mixed"):
                memory.validate_segmented_evidence(summary_path)

            (root / memory.LEGACY_RAW_NAME).unlink()
            (root / memory.LEGACY_SUMMARY_NAME).unlink()
            raw = memory.encode_raw_records(records)
            legacy = memory.build_summary(
                records,
                raw,
                memory.platform_metadata("win32"),
                exit_code=0,
                cadence_ns=memory.CADENCE_NS,
                idle_duration_ns=memory.IDLE_DURATION_NS,
                maximum_raw_samples=1_000,
            )
            with self.assertRaisesRegex(memory.EvidenceError, "mixed"):
                memory.validate_evidence(raw, legacy, evidence_directory=root)

            (root / "capacity-memory-raw-v2-999999.ndjson").write_bytes(b"extra\n")
            with self.assertRaisesRegex(memory.EvidenceError, "extras"):
                memory.validate_segmented_evidence(summary_path)
            (root / "capacity-memory-raw-v2-999999.ndjson").unlink()

            extra_summary = root / "other-memory-summary-v2.json"
            extra_summary.write_bytes(b"{}\n")
            with self.assertRaisesRegex(memory.EvidenceError, "extras|mixed"):
                memory.validate_segmented_evidence(summary_path)
            extra_summary.unlink()

            near_match = root / "capacity-memory-summary-v20.json"
            near_match.write_bytes(b"not a memory artifact\n")
            memory.validate_segmented_evidence(summary_path)
            ignored_names = [
                "notes-memory-summary-v2.md",
                "guide-memory-raw-v2-format.txt",
                "metrics-memory-raw-v1.csv",
                "capacity-memory-summary-v2.json.bak",
                "capacity-memory-raw-v1.ndjson.tmp",
                "capacity-memory-raw-v2-123456.ndjson.bak",
                "capacity-memory-raw-v2.ndjson",
                "capacity-memory-raw-v2-12345.ndjson",
                "capacity-memory-raw-v2-1234567.ndjson",
                "capacity-memory-raw-v2-12x456.ndjson",
                "-memory-summary-v1.json",
                "-memory-summary-v2.json",
                "-memory-raw-v1.ndjson",
                "-memory-raw-v2-123456.ndjson",
                "directory/capacity-memory-summary-v2.json",
            ]
            for name in ignored_names:
                with self.subTest(name=name):
                    self.assertIsNone(memory.classify_memory_artifact_name(name))
                    if "/" in name or "\\" in name:
                        continue
                    candidate = root / Path(name).name
                    candidate.write_bytes(b"not a memory artifact\n")
                    try:
                        memory.validate_segmented_evidence(summary_path)
                    finally:
                        candidate.unlink()

            classified_names = {
                "alternate-memory-summary-v1.json": ("summary", 1),
                "alternate-memory-summary-v2.json": ("summary", 2),
                "alternate-memory-raw-v1.ndjson": ("raw", 1),
                "alternate-memory-raw-v2-123456.ndjson": ("raw", 2),
            }
            for name, classification in classified_names.items():
                with self.subTest(name=name):
                    self.assertEqual(
                        memory.classify_memory_artifact_name(name), classification
                    )
                    candidate = root / name
                    candidate.write_bytes(b"undeclared memory artifact\n")
                    try:
                        with self.assertRaisesRegex(
                            memory.EvidenceError, "extras|mixed"
                        ):
                            memory.validate_segmented_evidence(summary_path)
                    finally:
                        candidate.unlink()

    def test_segmented_namespace_rejects_broken_memory_artifact_link(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            _, _, summary_path = write_segmented_fixture(
                root,
                maximum_segment_samples=60,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            broken = root / "other-memory-summary-v2.json"
            try:
                broken.symlink_to(root / "missing-summary.json")
            except OSError as error:
                self.skipTest(f"host cannot create a file symlink: {error}")
            with self.assertRaisesRegex(memory.EvidenceError, "extras|mixed"):
                memory.validate_segmented_evidence(summary_path)

    def test_segment_count_bound_fails_before_extra_file_and_bounds_descriptors(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            root = Path(directory)
            writer = memory.SegmentedRawWriter(
                root,
                "bounded-memory-raw-v2",
                maximum_segment_samples=1,
                maximum_segment_bytes=1_000,
                maximum_segments=2,
            )
            writer.write({"value": 0})
            writer.write({"value": 1})
            with self.assertRaisesRegex(memory.EvidenceError, "segment count"):
                writer.write({"value": 2})
            writer.close_partial()
            self.assertLessEqual(len(writer.segments), 2)
            self.assertEqual(
                sorted(path.name for path in root.iterdir()),
                [
                    "bounded-memory-raw-v2-000000.ndjson",
                    "bounded-memory-raw-v2-000001.ndjson",
                ],
            )
            self.assertFalse(
                (root / "bounded-memory-raw-v2-000002.ndjson").exists()
            )

    def test_short_run_supports_segmented_and_legacy_single_file_validation(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            _, summary, summary_path = write_segmented_fixture(
                Path(directory),
                maximum_segment_samples=1_000,
                maximum_segment_bytes=memory.MAX_RAW_BYTES,
            )
            memory.validate_segmented_evidence(summary_path)
            self.assertEqual(len(summary["segments"]), 1)

        records = memory.fixture_evidence_records(LABELS)
        raw = memory.encode_raw_records(records)
        legacy = memory.build_summary(
            records,
            raw,
            memory.platform_metadata("win32"),
            exit_code=0,
            cadence_ns=memory.CADENCE_NS,
            idle_duration_ns=memory.IDLE_DURATION_NS,
            maximum_raw_samples=1_000,
        )
        memory.validate_evidence(raw, legacy)



    def test_evidence_paths_and_outputs_fail_closed_before_sampling(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            evidence = Path(directory)
            existing = evidence / f"{memory.RAW_NAME}-000000.ndjson"
            existing.write_bytes(b"preserve")
            with self.assertRaisesRegex(memory.EvidenceError, "already exists"):
                memory.run_measurement(
                    [sys.executable, "-c", "raise SystemExit(99)"],
                    evidence,
                    lambda: dict(LABELS),
                )
            self.assertEqual(existing.read_bytes(), b"preserve")
            self.assertFalse((evidence / memory.SUMMARY_NAME).exists())
        with tempfile.TemporaryDirectory() as outside:
            with self.assertRaisesRegex(memory.EvidenceError, "package-excluded target"):
                memory._safe_evidence_directory(Path(outside))

    def test_summary_is_digest_bound_and_exactly_validated(self) -> None:
        records = memory.fixture_evidence_records(LABELS)
        raw = memory.encode_raw_records(records)
        summary = memory.build_summary(
            records, raw, memory.platform_metadata("win32"), exit_code=0,
            cadence_ns=50_000_000, idle_duration_ns=5_000_000_000,
            maximum_raw_samples=1_000,
        )
        memory.validate_evidence(raw, summary)
        self.assertEqual(summary["raw_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertEqual(summary["campaign"]["concurrent_aggregate"]["resident_bytes"]["peak"], 150)
        for mutate, expected in [
            (lambda value: value.__setitem__("raw_sha256", "0" * 64), "digest"),
            (lambda value: value.__setitem__("raw_record_count", 99), "count"),
            (lambda value: value["campaign"]["concurrent_aggregate"]["resident_bytes"].__setitem__("peak", 999), "summary"),
        ]:
            changed = copy.deepcopy(summary)
            mutate(changed)
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(memory.EvidenceError, expected):
                    memory.validate_evidence(raw, changed)

    def test_complete_evidence_rejects_campaign_cadence_and_timestamp_mutations(self) -> None:
        mutations: list[tuple[str, list[dict[str, object]]]] = []

        zero_delta = memory.fixture_evidence_records(LABELS)
        zero_delta[-1]["scheduled_monotonic_ns"] = zero_delta[-2]["scheduled_monotonic_ns"]
        zero_delta[-1]["monotonic_ns"] = zero_delta[-2]["monotonic_ns"]
        mutations.append(("zero campaign delta", zero_delta))

        hundred_delta = memory.fixture_evidence_records(LABELS)
        hundred_delta[-1]["scheduled_monotonic_ns"] += memory.CADENCE_NS
        hundred_delta[-1]["monotonic_ns"] += memory.CADENCE_NS
        mutations.append(("hundred millisecond campaign delta", hundred_delta))

        actual_before_scheduled = memory.fixture_evidence_records(LABELS)
        actual_before_scheduled[-1]["monotonic_ns"] = (
            actual_before_scheduled[-1]["scheduled_monotonic_ns"] - 1
        )
        mutations.append(("actual before scheduled", actual_before_scheduled))

        reversed_actual = memory.fixture_evidence_records(LABELS)
        reversed_actual[-2]["monotonic_ns"] = reversed_actual[-1]["monotonic_ns"] + 1
        mutations.append(("reversed actual timestamps", reversed_actual))

        boundary_overlap = memory.fixture_evidence_records(LABELS)
        last_idle = boundary_overlap[-3]["scheduled_monotonic_ns"]
        boundary_overlap[-2]["scheduled_monotonic_ns"] = last_idle
        boundary_overlap[-2]["monotonic_ns"] = last_idle
        boundary_overlap[-1]["scheduled_monotonic_ns"] = last_idle + memory.CADENCE_NS
        boundary_overlap[-1]["monotonic_ns"] = last_idle + memory.CADENCE_NS
        mutations.append(("idle campaign overlap", boundary_overlap))

        for name, records in mutations:
            for record in records:
                record["sampling_gap"] = memory.sampling_gap(
                    record["scheduled_monotonic_ns"],
                    record["monotonic_ns"],
                    memory.CADENCE_NS,
                )
            raw, summary = complete_mutated_evidence(records)
            with self.subTest(name=name):
                with self.assertRaises(memory.EvidenceError):
                    memory.validate_evidence(raw, summary)

    def test_linux_high_water_values_remain_in_summary_after_exit(self) -> None:
        records = memory.fixture_evidence_records(LABELS)
        records[-2]["atlas"]["high_water_bytes"] = 900
        records[-2]["providers"][0]["high_water_bytes"] = 700
        raw = memory.encode_raw_records(records)
        summary = memory.build_summary(
            records, raw, memory.platform_metadata("linux"), exit_code=0,
            cadence_ns=50_000_000, idle_duration_ns=5_000_000_000,
            maximum_raw_samples=1_000,
        )
        memory.validate_evidence(raw, summary)
        self.assertEqual(
            summary["campaign"]["atlas_only"]["high_water_bytes"]["peak"], 900
        )
        self.assertEqual(
            summary["campaign"]["provider_only"]["high_water_bytes"]["peak"], 700
        )

    @unittest.skipUnless(sys.platform == "win32", "Windows sharing contract")
    def test_label_reader_shares_delete_access(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "labels.json"
            replacement = Path(directory) / "replacement.json"
            path.write_text(json.dumps(LABELS), encoding="utf-8")
            replacement.write_text(json.dumps(LABELS), encoding="utf-8")

            delete_file = memory.ctypes.windll.kernel32.DeleteFileW
            delete_file.argtypes = (memory.wintypes.LPCWSTR,)
            delete_file.restype = memory.wintypes.BOOL
            with memory._open_label_reader(path) as reader:
                self.assertEqual(json.load(reader), LABELS)
                if not delete_file(str(path)):
                    raise memory.ctypes.WinError()
            os.replace(replacement, path)

            self.assertEqual(memory.read_labels(path), LABELS)

    @unittest.skipUnless(sys.platform == "win32", "Windows sharing contract")
    def test_label_reader_retries_only_replacefile_transition_errors(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "labels.json"
            path.write_text(json.dumps(LABELS), encoding="utf-8")
            original = memory._open_label_reader
            attempts = 0

            def transient_reader(candidate: Path):
                nonlocal attempts
                attempts += 1
                if attempts <= 3:
                    raise memory.ctypes.WinError((2, 5, 32)[attempts - 1])
                return original(candidate)

            memory._open_label_reader = transient_reader
            try:
                self.assertEqual(memory.read_labels(path), LABELS)
            finally:
                memory._open_label_reader = original
            self.assertEqual(attempts, 4)

            attempts = 0

            def unrelated_error(_: Path):
                nonlocal attempts
                attempts += 1
                raise memory.ctypes.WinError(123)

            memory._open_label_reader = unrelated_error
            try:
                with self.assertRaises(memory.EvidenceError):
                    memory.read_labels(path)
            finally:
                memory._open_label_reader = original
            self.assertEqual(attempts, 1)

            attempts = 0

            def persistent_transition(_: Path):
                nonlocal attempts
                attempts += 1
                raise memory.ctypes.WinError(32)

            memory._open_label_reader = persistent_transition
            try:
                with self.assertRaises(memory.EvidenceError):
                    memory.read_labels(path)
            finally:
                memory._open_label_reader = original
            self.assertEqual(attempts, 65)

    @unittest.skipUnless(sys.platform == "win32", "Windows sharing contract")
    def test_real_label_reader_survives_1127_concurrent_replacefile_publications(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "labels.json"
            replacement = Path(directory) / "replacement.json"
            path.write_text(json.dumps(LABELS), encoding="utf-8")
            replace_file = memory.ctypes.windll.kernel32.ReplaceFileW
            replace_file.argtypes = (
                memory.wintypes.LPCWSTR,
                memory.wintypes.LPCWSTR,
                memory.wintypes.LPCWSTR,
                memory.wintypes.DWORD,
                memory.wintypes.LPVOID,
                memory.wintypes.LPVOID,
            )
            replace_file.restype = memory.wintypes.BOOL
            stop = threading.Event()
            failures: list[BaseException] = []
            reads = 0

            def read_loop() -> None:
                nonlocal reads
                try:
                    while not stop.is_set():
                        self.assertEqual(memory.read_labels(path), LABELS)
                        reads += 1
                except BaseException as error:
                    failures.append(error)
                    stop.set()

            reader = threading.Thread(target=read_loop)
            reader.start()
            completed = 0
            try:
                for ordinal in range(1, 1_128):
                    replacement.write_text(json.dumps(LABELS), encoding="utf-8")
                    for retry in range(65):
                        if replace_file(str(path), str(replacement), None, 0, None, None):
                            break
                        error = memory.ctypes.WinError()
                        if error.winerror != 1175 or retry == 64:
                            raise error
                        memory.time.sleep(0.001)
                    self.assertEqual(memory.read_labels(path), LABELS)
                    completed = ordinal
                    if failures:
                        break
                deadline = memory.time.monotonic() + 5
                while reads <= 1_125 and not failures and memory.time.monotonic() < deadline:
                    memory.time.sleep(0.001)
            finally:
                stop.set()
                reader.join()
            self.assertEqual(completed, 1_127)
            self.assertGreater(reads, 1_125)
            self.assertEqual(failures, [])


    def test_duplicate_json_fields_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "labels.json"
            path.write_text(
                '{\"phase\":\"one\",\"phase\":\"two\",'
                '\"filesystem_cache_state\":\"warm\",\"serving_state\":\"absent\",'
                '\"provider_cache_install_state\":\"required\",\"build_state\":\"prebuilt\",'
                '\"catalogue_state\":\"ready\"}',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(memory.EvidenceError, "duplicate field phase"):
                memory.read_labels(path)

    def test_raw_validation_rejects_cross_time_negative_overflow_identity_and_labels(self) -> None:
        records = memory.fixture_evidence_records(LABELS)
        mutations = []
        wrong_aggregate = copy.deepcopy(records)
        wrong_aggregate[-2]["aggregate"]["resident_bytes"] += 1
        mutations.append(wrong_aggregate)
        negative = copy.deepcopy(records)
        negative[-2]["atlas"]["resident_bytes"] = -1
        mutations.append(negative)
        overflow = copy.deepcopy(records)
        overflow[-2]["atlas"]["resident_bytes"] = memory.MAX_BYTES + 1
        mutations.append(overflow)
        identity = copy.deepcopy(records)
        identity[-2]["atlas"]["identity"] = None
        mutations.append(identity)
        labels = copy.deepcopy(records)
        labels[-1]["labels"]["extra"] = "bad"
        mutations.append(labels)
        for changed in mutations:
            with self.subTest(change=changed[-1]):
                with self.assertRaises(memory.EvidenceError):
                    memory.validate_raw_records(changed, maximum_raw_samples=1000)

    def test_platform_metadata_is_exact_and_unsupported_fails(self) -> None:
        expected = {
            "win32": {
                "os": "windows",
                "method": "Job Object active-process list + GetProcessMemoryInfo WorkingSet64 and PrivateMemorySize64",
                "advisory": False,
                "limitations": [
                    "working-set-includes-shared-pages",
                    "private-bytes-omit-shared-and-mapped-pressure",
                    "sub-50ms-descendants-may-be-missed",
                    "observation-calls-within-one-sample-are-sequential",
                ],
            },
            "linux": {
                "os": "linux",
                "method": "/proc/<pid>/status VmRSS and VmHWM with /proc/<pid>/stat process-group/starttime",
                "advisory": False,
                "limitations": [
                    "rss-includes-shared-pages",
                    "VmHWM-is-retained-last-observed-at-exit",
                    "sub-50ms-descendants-may-be-missed",
                    "proc-reads-can-race-process-exit",
                    "one-raw-timestamp-labels-a-bounded-non-atomic-sequential-observation-window",
                    "process-group-containment-excludes-members-that-create-a-new-session-or-group",
                ],
            },
            "darwin": {
                "os": "macos",
                "method": "/bin/ps -axo pid=,ppid=,pgid=,rss=,lstart= resident bytes",
                "advisory": True,
                "limitations": [
                    "runner-default",
                    "second-resolution",
                    "runner-default-values-are-advisory-until-pinned",
                    "rss-includes-shared-pages",
                    "ps-snapshot-has-no-private-bytes-or-high-water-mark",
                    "sub-50ms-descendants-may-be-missed",
                    "one-raw-timestamp-labels-a-bounded-non-atomic-sequential-observation-window",
                    "process-group-containment-excludes-members-that-create-a-new-session-or-group",
                ],
            },
        }
        for system, exact in expected.items():
            with self.subTest(system=system):
                metadata = memory.platform_metadata(system)
                self.assertEqual(
                    {key: metadata[key] for key in exact},
                    exact,
                )
        with self.assertRaisesRegex(memory.UnsupportedPlatformError, "unsupported"):
            memory.platform_metadata("plan9")

    def test_run_measurement_keeps_summary_after_persistent_completion(self) -> None:
        reads = 0
        completed_reads = 0

        def labels() -> dict[str, object]:
            nonlocal reads, completed_reads
            reads += 1
            if reads == 1:
                return progress_state(
                    0, "harness-startup", "starting", total=33_750
                )
            if reads == 2:
                return progress_state(1, total=33_750)
            completed_reads += 1
            return progress_state(
                33_750,
                "progressive-deep-route-compiler",
                "complete",
                total=33_750,
            )

        output = io.StringIO()
        accumulators: list[object] = []
        original_accumulator = memory.StreamingSummaryAccumulator

        class InspectingAccumulator(original_accumulator):
            def __init__(self, directory: Path):
                super().__init__(directory)
                accumulators.append(self)

        memory.StreamingSummaryAccumulator = InspectingAccumulator
        try:
            with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
                with contextlib.redirect_stdout(output):
                    raw_path, summary_path, exit_code = memory.run_measurement(
                        [sys.executable, "-c", "import time; time.sleep(1)"],
                        Path(directory),
                        labels,
                        raw_name="persistent-complete-memory-raw-v2",
                        summary_name="persistent-complete-memory-summary-v2.json",
                        maximum_samples=60,
                    )
                self.assertEqual(
                    raw_path.name,
                    "persistent-complete-memory-raw-v2-000000.ndjson",
                )
                summary = memory._strict_json(
                    summary_path.read_bytes(), "persistent-complete summary"
                )
                raw = b"".join(
                    (Path(directory) / segment["path"]).read_bytes()
                    for segment in summary["segments"]
                )
                memory.validate_segmented_evidence(summary_path)
                self.assertGreaterEqual(len(summary["segments"]), 2)
        finally:
            memory.StreamingSummaryAccumulator = original_accumulator
        self.assertGreater(accumulators[0].record_count, 60)
        self.assertNotIn("records", vars(accumulators[0]))
        self.assertFalse(any(
            isinstance(value, list)
            and len(value) == accumulators[0].record_count
            for value in vars(accumulators[0]).values()
        ))

        completed_line = (
            "ATLAS_CAPACITY_PROGRESS state=complete ordinal=33750 "
            "total=33750 phase=progressive-deep-route-compiler"
        )
        self.assertEqual(exit_code, 0)
        self.assertGreaterEqual(completed_reads, 2)
        self.assertEqual(output.getvalue().splitlines().count(completed_line), 1)
        self.assertEqual(summary["raw_sha256"], hashlib.sha256(raw).hexdigest())

    def test_run_measurement_rejects_post_completion_labels_only_state(self) -> None:
        reads = 0

        def labels() -> dict[str, object]:
            nonlocal reads
            reads += 1
            if reads == 1:
                return progress_state(
                    0, "harness-startup", "starting", total=33_750
                )
            if reads == 2:
                return progress_state(
                    33_750,
                    "progressive-deep-route-compiler",
                    "complete",
                    total=33_750,
                )
            return {
                **LABELS,
                "filesystem_cache_state": "private-downgrade-sentinel",
            }

        output = io.StringIO()
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            evidence = Path(directory)
            raw_prefix = "downgrade-memory-raw-v2"
            raw_path = evidence / f"{raw_prefix}-000000.ndjson"
            summary_path = evidence / "downgrade-memory-summary-v2.json"
            with contextlib.redirect_stdout(output):
                with self.assertRaises(memory.EvidenceError) as raised:
                    memory.run_measurement(
                        [sys.executable, "-c", "import time; time.sleep(0.25)"],
                        evidence,
                        labels,
                        raw_name=raw_prefix,
                        summary_name=summary_path.name,
                        maximum_samples=256,
                    )
            raw = raw_path.read_bytes()
            records = [
                memory._strict_json(line, "downgrade partial raw")
                for line in raw.splitlines()
            ]
            self.assertFalse(summary_path.exists())

        campaign = [record for record in records if record["control"] == "campaign"]
        self.assertEqual(str(raised.exception), "capacity progress changed after its final state")
        self.assertEqual(reads, 3)
        self.assertEqual(len(campaign), 1)
        self.assertEqual(
            campaign[0]["labels"]["phase"],
            "progressive-deep-route-compiler",
        )
        self.assertTrue(memory.PROGRESS_KEYS.isdisjoint(campaign[0]["labels"]))
        self.assertNotIn("private-downgrade-sentinel", raw.decode("utf-8"))
        self.assertEqual(
            output.getvalue().splitlines(),
            [
                "ATLAS_CAPACITY_PROGRESS state=starting ordinal=0 "
                "total=33750 phase=harness-startup",
                "ATLAS_CAPACITY_PROGRESS state=complete ordinal=33750 "
                "total=33750 phase=progressive-deep-route-compiler",
            ],
        )


@unittest.skipUnless(os.name == "nt", "available Windows host smoke")
class WindowsHostSmoke(unittest.TestCase):
    def test_observer_measures_current_process_and_descendant(self) -> None:
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(2)"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            shell=False,
        )
        try:
            snapshot = memory.WindowsObserver().observe_tree(os.getpid())
            self.assertEqual(snapshot.atlas["status"], "observed")
            self.assertGreater(snapshot.atlas["resident_bytes"], 0)
            self.assertGreater(snapshot.atlas["private_bytes"], 0)
            self.assertIn(child.pid, [item["pid"] for item in snapshot.providers])
        finally:
            child.terminate()
            child.wait(timeout=5)

    def test_root_exit_immediately_gaps_atlas_and_keeps_provider_peak(self) -> None:
        with tempfile.TemporaryDirectory(dir=memory.ROOT / "target") as directory:
            raw_path, summary_path, exit_code = memory.run_measurement(
                memory._host_smoke_command(),
                Path(directory),
                lambda: {
                    "phase": "windows-root-exit-regression",
                    "filesystem_cache_state": "uncontrolled",
                    "serving_state": "not-applicable",
                    "provider_cache_install_state": "not-applicable",
                    "build_state": "python-prebuilt",
                    "catalogue_state": "not-applicable",
                },
                raw_name="root-exit-memory-raw-v2",
                summary_name="root-exit-memory-summary-v2.json",
                maximum_samples=256,
            )
            raw = raw_path.read_bytes()
            summary = memory._strict_json(summary_path.read_bytes(), "root-exit summary")
            memory.validate_segmented_evidence(summary_path)
            self.assertEqual(exit_code, 0)
            records = [
                memory._strict_json(line, "root-exit raw") for line in raw.splitlines()
            ]
            campaign = [record for record in records if record["control"] == "campaign"]
            root_exited_with_provider = [
                record for record in campaign
                if record["atlas"]["status"] == "gap"
                and record["atlas"]["error"]["kind"] == "exited"
                and any(provider["status"] == "observed" for provider in record["providers"])
            ]
            self.assertTrue(root_exited_with_provider)
            self.assertTrue(all(
                record["aggregate"]["atlas_resident_bytes"] is None
                and record["aggregate"]["provider_resident_bytes"] is not None
                and record["aggregate"]["resident_bytes"] is None
                for record in root_exited_with_provider
            ))
            provider_values = [
                provider["resident_bytes"] for record in campaign
                for provider in record["providers"] if provider["status"] == "observed"
            ]
            self.assertEqual(
                summary["campaign"]["provider_only"]["resident_bytes"]["peak"],
                max(provider_values),
            )
            self.assertTrue(any(
                event["type"] == "descendant_exited"
                for record in campaign for event in record["child_events"]
            ))


if __name__ == "__main__":
    unittest.main()
