#!/usr/bin/env python3
"""Focused tests for normalize-native-context-evidence.py."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("normalize-native-context-evidence.py")
REPOSITORY = SCRIPT.parents[1]
CORPUS = REPOSITORY / "tests/fixtures/context_qualification/corpus-v1.json"
FROZEN_CONFIG = REPOSITORY / "config/workspace-atlas-v1.1.config.example.toml"
SPEC = importlib.util.spec_from_file_location("atlas_native_acquisition_adapter", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("native acquisition adapter import unavailable")
adapter = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = adapter
SPEC.loader.exec_module(adapter)


def canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def native_context_hash(document: dict[str, object]) -> str:
    identity = {key: value for key, value in document.items() if key != "context_hash"}
    return sha256(canonical_bytes(identity))


class NativeFixture:
    def __init__(
        self,
        root: Path,
        *,
        blocked: bool = False,
        source_bytes: bytes | None = None,
        spans: list[tuple[int, int]] | None = None,
    ) -> None:
        self.root = root
        self.evidence = root / "evidence"
        self.source_root = root / "source"
        self.evidence.mkdir(parents=True)
        self.source_root.mkdir()
        self.source_path = "config/workspace-atlas-v1.1.config.example.toml"
        (self.source_root / "config").mkdir()
        if blocked:
            source_bytes = None
            spans = []
        elif source_bytes is None:
            source_bytes = b"x" * 3391
            spans = []
            offset = 0
            for index in range(40):
                length = 24 if index == 39 else 13
                spans.append((offset, offset + length))
                offset += length
        assert spans is not None
        self.source_bytes = source_bytes
        self.spans = spans
        if source_bytes is not None:
            (self.source_root / self.source_path).write_bytes(source_bytes)

        self.workspace_id = "ws_fixture"
        self.generation_id = "gen_fixture"
        self.task_session_id = "task_fixture"
        self.context_id = "ctx_fixture"
        self.request_hash = sha256(b"request")
        self.configuration_hash = sha256(b"configuration")
        self.root_fingerprint = sha256(b"root")
        self.provider_set_hash = "p_fixture"
        self.repository_commit = "f" * 40
        self.source_tree_hash = sha256(b"indexed-generation-tree")
        self.stream_path = self.evidence / "context.json"
        self.status_before_path = self.evidence / "status-before.json"
        self.status_after_path = self.evidence / "status-after.json"
        self.catalogue_path = self.evidence / "catalogue.sqlite"
        self.capture_path = self.evidence / "capture.json"
        self.model_path = self.evidence / "no-model.json"
        self.agent_path = self.evidence / "no-agent.json"
        self.descriptor_path = self.evidence / "descriptor.json"
        self.output_path = self.evidence / "normalized.json"
        self.model_path.write_bytes(b'{"model":null}\n')
        self.agent_path.write_bytes(b'{"agent":null}\n')

        self.items = self._items()
        self.ir = self._ir(blocked)
        self.stream = {
            "ok": True,
            "task_session_id": self.task_session_id,
            "task_kind": "configuration_change" if not blocked else "bug_fix",
            "task_kind_source": "declared",
            "kind_rule_id": None,
            "context_ir": self.ir,
        }
        self.status = {
            "ok": True,
            "workspace_id": self.workspace_id,
            "canonical_root": str(self.source_root.resolve()),
            "config_hash": self.configuration_hash,
            "registered_config_hash": self.configuration_hash,
            "effective_config_hash": self.configuration_hash,
            "schema_version": "1.3.0",
            "migration_version": 4,
            "active_generation_id": self.generation_id,
            "active_generation_sequence": 1,
            "active_generation_state": "committed",
            "committed_generation_count": 1,
            "failed_generation_count": 0,
            "abandoned_generation_count": 0,
            "candidate_generation_count": 0,
            "integrity_ok": True,
            "writer_lease_holder": None,
            "writer_lease_expires_at": None,
        }
        self.capture = {
            "arguments": ["context-ir", "<source-root>"],
            "environment_qualified_wall_us": 3210,
            "exit_code": 0,
            "stderr_sha256": sha256(b""),
            "stdout_sha256": "",
            "task_acceptance": None,
        }
        self._write_stream()
        self.status_before_path.write_bytes(canonical_bytes(self.status) + b"\n")
        self.status_after_path.write_bytes(canonical_bytes(self.status) + b"\n")
        self.capture["stdout_sha256"] = sha256(self.stream_path.read_bytes())
        self.capture_path.write_bytes(canonical_bytes(self.capture) + b"\n")
        self._create_catalogue()
        self.descriptor = self._descriptor()
        self.write_descriptor()

    def _items(self) -> list[dict[str, object]]:
        if self.source_bytes is None:
            return []
        items = []
        source_hash = sha256(self.source_bytes)
        for index, (start, end) in enumerate(self.spans):
            tokens = 6 if len(self.spans) == 40 and index == 39 else 3
            items.append({
                "item_id": f"item_{index}",
                "entity_kind": "symbol",
                "entity_id": f"toml-key:{self.source_path}#key_{index}@{start}",
                "role": "CONFIGURATION_INPUT",
                "selection_reason": "EXACT_PATH_MATCH",
                "origin_id": None,
                "distance": 0,
                "rank": index,
                "evidence": {
                    "state": "supported",
                    "confidence": 0.85,
                    "provider_fingerprint": None,
                    "source_revision_hash": None,
                    "generation_id": self.generation_id,
                    "preferred": True,
                    "alternative_count": 0,
                },
                "cost": {
                    "metadata_bytes": 96,
                    "source_bytes": end - start,
                    "estimated_tokens": tokens,
                },
                "source": {
                    "path": self.source_path,
                    "start_byte": start,
                    "end_byte": end,
                    "indexed_hash": source_hash,
                    "observed_hash": None,
                    "verification_status": "not_requested",
                },
            })
        return items

    def _ir(self, blocked: bool) -> dict[str, object]:
        selected_bytes = sum(end - start for start, end in self.spans)
        selected_tokens = sum(item["cost"]["estimated_tokens"] for item in self.items)
        selected = len(self.items)
        status = "blocked" if blocked else "partial"
        document: dict[str, object] = {
            "schema_version": "1.0.0",
            "context_id": self.context_id,
            "workspace": {
                "workspace_id": self.workspace_id,
                "generation_id": self.generation_id,
                "generation_sequence": 1,
                "source_tree_hash": None,
                "configuration_hash": self.configuration_hash,
                "provider_set_hash": self.provider_set_hash,
                "serving_generation_id": None,
            },
            "task": {
                "task_session_id": self.task_session_id,
                "task_hash": sha256(b"task"),
                "normalized_goal_hash": sha256(b"goal"),
                "task_kind": "bug_fix" if blocked else "configuration_change",
                "kind_source": "declared",
                "kind_rule_id": None,
                "seed_paths": [] if blocked else [self.source_path],
                "seed_symbols": [],
            },
            "policy": {
                "planner_policy_version": "planner-v1.1.0",
                "projection_policy_version": "projection-v1.0.0",
                "budget_profile": "standard",
                "deterministic": True,
            },
            "status": {
                "working_set_status": status,
                "reasons": ["serving_plane_unavailable_used_direct_truth"],
                "serving_fallback": True,
            },
            "working_set": self.items,
            "relationships": [],
            "effects": [],
            "uncertainty": [] if not blocked else [{
                "code": "explicit_path_seed_unresolved",
                "severity": "error",
                "message": "synthetic unresolved path",
                "entity_ids": [f"path:{self.source_path}"],
            }],
            "coverage": {
                "eligible": 104 if not blocked else 0,
                "complete": selected,
                "partial": 0,
                "unsupported": 0,
                "excluded": 0,
                "failed": 0,
                "claim_strength": "bounded" if not blocked else "none",
                "details": [],
            },
            "omissions": {
                "candidates_considered": 104 if not blocked else 0,
                "selected": selected,
                "omitted": 104 - selected if not blocked else 0,
                "truncated": not blocked,
                "by_reason": {"path_seed_record_budget": 104 - selected} if not blocked else {},
            },
            "validation_plan": [],
            "cost": {
                "max_records": 40,
                "selected_records": selected,
                "max_source_bytes": 32768,
                "selected_source_bytes": selected_bytes,
                "max_estimated_tokens": 8000,
                "selected_estimated_tokens": selected_tokens,
                "soft_latency_ms": 500,
                "hard_latency_ms": 5000,
                "elapsed_ms": 0,
            },
            "context_hash": "",
        }
        document["context_hash"] = native_context_hash(document)
        return document

    def _write_stream(self) -> None:
        self.stream_path.write_bytes(canonical_bytes(self.stream) + b"\n")

    def _create_catalogue(self) -> None:
        connection = sqlite3.connect(self.catalogue_path)
        connection.executescript("""
            CREATE TABLE workspace(
                workspace_id TEXT PRIMARY KEY, canonical_root TEXT NOT NULL,
                root_fingerprint TEXT NOT NULL, configuration_hash TEXT NOT NULL,
                active_generation_id TEXT
            );
            CREATE TABLE index_generation(
                generation_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
                sequence_no INTEGER NOT NULL, state TEXT NOT NULL,
                source_tree_hash TEXT, provider_set_hash TEXT NOT NULL
            );
            CREATE TABLE task_session(
                task_session_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
                start_generation_id TEXT NOT NULL, task_hash TEXT NOT NULL,
                normalized_goal_hash TEXT NOT NULL, task_kind TEXT NOT NULL,
                task_kind_source TEXT NOT NULL, task_kind_rule_id TEXT,
                planner_policy_version TEXT NOT NULL, context_ir_version TEXT NOT NULL,
                state TEXT NOT NULL, accepted INTEGER, tests_passed INTEGER,
                outcome_code TEXT
            );
            CREATE TABLE context_ir(
                context_id TEXT PRIMARY KEY, task_session_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL, generation_id TEXT NOT NULL,
                context_ir_version TEXT NOT NULL, planner_policy_version TEXT NOT NULL,
                projection_policy_version TEXT NOT NULL, budget_profile TEXT NOT NULL,
                request_hash TEXT NOT NULL, working_set_status TEXT NOT NULL,
                serving_fallback INTEGER NOT NULL, max_records INTEGER NOT NULL,
                selected_records INTEGER NOT NULL, max_source_bytes INTEGER NOT NULL,
                selected_source_bytes INTEGER NOT NULL, max_estimated_tokens INTEGER NOT NULL,
                selected_estimated_tokens INTEGER NOT NULL, soft_latency_ms INTEGER NOT NULL,
                hard_latency_ms INTEGER NOT NULL, elapsed_ms INTEGER NOT NULL,
                canonical_json TEXT NOT NULL, context_hash TEXT NOT NULL
            );
            CREATE TABLE query_stage_metric(
                metric_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
                generation_id TEXT NOT NULL, task_session_id TEXT,
                context_id TEXT, request_id TEXT NOT NULL, operation TEXT NOT NULL,
                serving_fallback INTEGER NOT NULL, seed_resolution_us INTEGER NOT NULL,
                serving_lookup_us INTEGER NOT NULL, graph_expansion_us INTEGER NOT NULL,
                ranking_us INTEGER NOT NULL, source_verify_us INTEGER NOT NULL,
                source_read_us INTEGER NOT NULL, packing_us INTEGER NOT NULL,
                candidate_count INTEGER NOT NULL, expanded_edge_count INTEGER NOT NULL,
                source_bytes_read INTEGER NOT NULL, returned_records INTEGER NOT NULL,
                returned_estimated_tokens INTEGER NOT NULL, cache_hits INTEGER NOT NULL,
                cache_misses INTEGER NOT NULL, truncated INTEGER NOT NULL
            );
        """)
        connection.execute(
            "INSERT INTO workspace VALUES (?,?,?,?,?)",
            (self.workspace_id, str(self.source_root.resolve()), self.root_fingerprint,
             self.configuration_hash, self.generation_id),
        )
        connection.execute(
            "INSERT INTO index_generation VALUES (?,?,?,?,?,?)",
            (self.generation_id, self.workspace_id, 1, "committed",
             self.source_tree_hash, self.provider_set_hash),
        )
        task = self.ir["task"]
        policy = self.ir["policy"]
        connection.execute(
            "INSERT INTO task_session VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (self.task_session_id, self.workspace_id, self.generation_id,
             task["task_hash"], task["normalized_goal_hash"], task["task_kind"],
             task["kind_source"], task["kind_rule_id"], policy["planner_policy_version"],
             "1.0.0", "context_compiled", None, None, None),
        )
        cost = self.ir["cost"]
        connection.execute(
            "INSERT INTO context_ir VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (self.context_id, self.task_session_id, self.workspace_id, self.generation_id,
             "1.0.0", policy["planner_policy_version"], policy["projection_policy_version"],
             policy["budget_profile"], self.request_hash,
             self.ir["status"]["working_set_status"], 1,
             cost["max_records"], cost["selected_records"], cost["max_source_bytes"],
             cost["selected_source_bytes"], cost["max_estimated_tokens"],
             cost["selected_estimated_tokens"], cost["soft_latency_ms"],
             cost["hard_latency_ms"], cost["elapsed_ms"],
             canonical_bytes(self.ir).decode("utf-8"), self.ir["context_hash"]),
        )
        connection.execute(
            "INSERT INTO query_stage_metric VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            ("metric_fixture", self.workspace_id, self.generation_id,
             self.task_session_id, None, self.request_hash, "context_ir_compile", 1,
             14, 125, 8371 if self.items else 0, 22, 0, 0, 2320,
             104 if self.items else 0, 0, 0, len(self.items),
             self.ir["cost"]["selected_estimated_tokens"], 0, 1,
             1 if self.items else 0),
        )
        connection.commit()
        connection.close()

    def _file_ref(self, path: Path) -> dict[str, object]:
        return {
            "path": path.relative_to(self.evidence).as_posix(),
            "sha256": sha256(path.read_bytes()),
        }

    def _descriptor(self) -> dict[str, object]:
        source_files = []
        if self.source_bytes is not None:
            source_files.append({
                "path": self.source_path,
                "sha256": sha256(self.source_bytes),
                "bytes": len(self.source_bytes),
                "encoding": "utf-8",
            })
        return {
            "schema_version": adapter.DESCRIPTOR_VERSION,
            "kind": "atlas-native-acquisition-descriptor",
            "synthetic": True,
            "repository_commit": self.repository_commit,
            "native_operation": "context-ir",
            "files": {
                "context_stream": self._file_ref(self.stream_path),
                "status_before": self._file_ref(self.status_before_path),
                "status_after": self._file_ref(self.status_after_path),
                "catalogue_snapshot": self._file_ref(self.catalogue_path),
                "model_configuration": self._file_ref(self.model_path),
                "agent_configuration": self._file_ref(self.agent_path),
                "capture_metadata": self._file_ref(self.capture_path),
                "benchmark_corpus": None,
            },
            "expected": {
                "context_id": self.context_id,
                "context_hash": self.ir["context_hash"],
                "workspace_id": self.workspace_id,
                "generation_id": self.generation_id,
                "task_session_id": self.task_session_id,
                "request_hash": self.request_hash,
                "configuration_hash": self.configuration_hash,
                "workspace_root_fingerprint": self.root_fingerprint,
            },
            "source_files": source_files,
            "benchmark_locator_candidates": [],
        }

    def write_descriptor(self) -> None:
        self.descriptor_path.write_bytes(canonical_bytes(self.descriptor) + b"\n")

    def reseal_context(self, *, update_database: bool = False) -> None:
        self.ir["context_hash"] = native_context_hash(self.ir)
        self.stream["context_ir"] = self.ir
        self._write_stream()
        self.descriptor["files"]["context_stream"] = self._file_ref(self.stream_path)
        self.descriptor["expected"]["context_hash"] = self.ir["context_hash"]
        self.capture["stdout_sha256"] = sha256(self.stream_path.read_bytes())
        self.capture_path.write_bytes(canonical_bytes(self.capture) + b"\n")
        self.descriptor["files"]["capture_metadata"] = self._file_ref(self.capture_path)
        if update_database:
            connection = sqlite3.connect(self.catalogue_path)
            connection.execute(
                "UPDATE context_ir SET canonical_json=?, context_hash=?, context_ir_version=?, max_records=?, selected_records=?, max_source_bytes=?, selected_source_bytes=?, max_estimated_tokens=?, selected_estimated_tokens=?, working_set_status=? WHERE context_id=?",
                (
                    canonical_bytes(self.ir).decode("utf-8"),
                    self.ir["context_hash"],
                    self.ir["schema_version"],
                    self.ir["cost"]["max_records"],
                    self.ir["cost"]["selected_records"],
                    self.ir["cost"]["max_source_bytes"],
                    self.ir["cost"]["selected_source_bytes"],
                    self.ir["cost"]["max_estimated_tokens"],
                    self.ir["cost"]["selected_estimated_tokens"],
                    self.ir["status"]["working_set_status"],
                    self.context_id,
                ),
            )
            connection.execute(
                "UPDATE query_stage_metric SET returned_records=?, returned_estimated_tokens=?, truncated=? WHERE request_id=?",
                (
                    self.ir["cost"]["selected_records"],
                    self.ir["cost"]["selected_estimated_tokens"],
                    int(self.ir["omissions"]["truncated"]),
                    self.request_hash,
                ),
            )
            connection.commit()
            connection.close()
            self.reseal_catalogue()
        self.write_descriptor()

    def reseal_catalogue(self) -> None:
        self.descriptor["files"]["catalogue_snapshot"] = self._file_ref(self.catalogue_path)
        self.write_descriptor()

    def normalize(self) -> dict[str, object]:
        return adapter.normalize(
            self.evidence,
            self.source_root,
            Path("descriptor.json"),
        )


class NativeAcquisitionAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_partial_40_record_and_empty_blocked_contracts_are_acquisition_only(self) -> None:
        partial = NativeFixture(self.root / "partial")
        result = partial.normalize()
        self.assertEqual(result["schema_version"], adapter.OUTPUT_VERSION)
        self.assertEqual(result["native_operation"], "context-ir")
        self.assertIsNone(result["governor_route"])
        self.assertIsNone(result["task_execution"])
        self.assertIsNone(result["acceptance"])
        self.assertEqual(
            result["bindings"]["repository_commit"],
            {
                "value": partial.repository_commit,
                "verification": "declared_only",
                "source_root_git_proof": None,
                "benchmark_corpus_consistency": "not_available",
            },
        )
        self.assertEqual(result["acquisition"]["status"], "partial")
        self.assertEqual(len(result["evidence"]), 40)
        self.assertEqual(result["cost"]["selected_records"], 40)
        self.assertEqual(result["cost"]["selected_span_bytes"], 531)
        self.assertEqual(result["cost"]["selected_estimated_tokens"], 123)
        self.assertEqual(result["cost"]["delivered_source_bytes"], 0)
        self.assertIsNone(result["cost"]["legacy_elapsed_ms"])
        self.assertEqual(
            result["cost"]["legacy_elapsed_reason"],
            "hardcoded_zero_not_timing_evidence",
        )
        for field in ("cpu_ms", "io_read_bytes", "io_write_bytes", "database_steps",
                      "provider_scopes", "work_units"):
            self.assertIsNone(result["cost"][field])
        self.assertEqual(result["evidence"][0]["identity"]["range"], None)
        self.assertEqual(result["evidence"][0]["source"]["whole_file_bytes"], 3391)
        self.assertEqual(result["evidence"][0]["source"]["verification_status"], "not_requested")
        self.assertEqual(result["evidence"][0]["benchmark_mapping_reason"], "not_requested")
        self.assertEqual(
            result["normalization_sha256"],
            sha256(canonical_bytes({
                key: value
                for key, value in result.items()
                if key != "normalization_sha256"
            }) + b"\n"),
        )

        blocked = NativeFixture(self.root / "blocked", blocked=True)
        blocked_result = blocked.normalize()
        self.assertEqual(blocked_result["acquisition"]["status"], "blocked")
        self.assertEqual(blocked_result["evidence"], [])
        self.assertEqual(blocked_result["cost"]["selected_records"], 0)
        self.assertEqual(blocked_result["cost"]["delivered_source_bytes"], 0)
        self.assertIsNone(blocked_result["acceptance"])

    def test_changed_stream_and_source_digests_fail_closed(self) -> None:
        stream = NativeFixture(self.root / "stream")
        stream.stream_path.write_bytes(stream.stream_path.read_bytes() + b" ")
        with self.assertRaisesRegex(adapter.AdapterError, "context_stream digest mismatch"):
            stream.normalize()

        source = NativeFixture(self.root / "source")
        source_file = source.source_root / source.source_path
        source_file.write_bytes(source_file.read_bytes() + b"x")
        with self.assertRaisesRegex(adapter.AdapterError, "source file (digest|byte count) mismatch"):
            source.normalize()

    def test_wrong_context_hash_and_unsupported_versions_fail_closed(self) -> None:
        wrong = NativeFixture(self.root / "wrong")
        wrong.ir["context_hash"] = "0" * 64
        wrong.stream["context_ir"] = wrong.ir
        wrong._write_stream()
        wrong.descriptor["files"]["context_stream"] = wrong._file_ref(wrong.stream_path)
        wrong.descriptor["expected"]["context_hash"] = "0" * 64
        wrong.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "native Context IR hash mismatch"):
            wrong.normalize()

        for version, message in (("2.0.0", "adapter does not support"), ("9.0.0", "unknown Context IR")):
            with self.subTest(version=version):
                fixture = NativeFixture(self.root / f"version-{version.replace('.', '-')}")
                fixture.ir["schema_version"] = version
                fixture.reseal_context(update_database=True)
                with self.assertRaisesRegex(adapter.AdapterError, message):
                    fixture.normalize()

    def test_duplicate_nonfinite_unknown_and_oversized_json_reject(self) -> None:
        duplicate = NativeFixture(self.root / "duplicate")
        duplicate.stream_path.write_text('{"ok":true,"ok":false}\n', encoding="utf-8")
        duplicate.descriptor["files"]["context_stream"] = duplicate._file_ref(duplicate.stream_path)
        duplicate.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "duplicate JSON field"):
            duplicate.normalize()

        nonfinite = NativeFixture(self.root / "nonfinite")
        nonfinite.stream_path.write_text('{"value":NaN}\n', encoding="utf-8")
        nonfinite.descriptor["files"]["context_stream"] = nonfinite._file_ref(nonfinite.stream_path)
        nonfinite.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "non-finite"):
            nonfinite.normalize()

        unknown = NativeFixture(self.root / "unknown")
        unknown.stream["unexpected"] = True
        unknown._write_stream()
        unknown.descriptor["files"]["context_stream"] = unknown._file_ref(unknown.stream_path)
        unknown.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "native stream fields"):
            unknown.normalize()

        oversized = NativeFixture(self.root / "oversized")
        oversized.descriptor_path.write_bytes(b" " * (adapter.MAX_DESCRIPTOR_BYTES + 1))
        with self.assertRaisesRegex(adapter.AdapterError, "byte bound"):
            oversized.normalize()

        unknown_enum = NativeFixture(self.root / "unknown-enum")
        unknown_enum.ir["working_set"][0]["entity_kind"] = "future_kind"
        unknown_enum.reseal_context(update_database=True)
        with self.assertRaisesRegex(adapter.AdapterError, "entity_kind is unsupported"):
            unknown_enum.normalize()

    def test_traversal_absolute_and_link_paths_reject(self) -> None:
        bad_paths = [
            "../context.json",
            "/absolute/context.json",
            "nested\\context.json",
            "C:relative/context.json",
            "./context.json",
            "nested//context.json",
        ]
        for index, path in enumerate(bad_paths):
            with self.subTest(path=path):
                fixture = NativeFixture(self.root / f"path-{index}")
                fixture.descriptor["files"]["context_stream"]["path"] = path
                fixture.write_descriptor()
                with self.assertRaisesRegex(adapter.AdapterError, "canonical relative path"):
                    fixture.normalize()

        linked = NativeFixture(self.root / "link")
        if os.name == "nt":
            target = self.root / "link-target"
            target.mkdir()
            (target / "context.json").write_bytes(linked.stream_path.read_bytes())
            link = linked.evidence / "linked"
            created = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(link), str(target)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(created.returncode, 0, created.stderr)
            linked_path = "linked/context.json"
        else:
            link = linked.evidence / "linked-context.json"
            os.symlink(linked.stream_path, link)
            linked_path = "linked-context.json"
        linked.descriptor["files"]["context_stream"] = {
            "path": linked_path,
            "sha256": sha256(linked.stream_path.read_bytes()),
        }
        linked.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "link or reparse point"):
            linked.normalize()

    def test_offsets_and_crlf_unicode_line_mapping_are_exact(self) -> None:
        data = "alpha\r\nβeta\r\nomega\n".encode("utf-8")
        start = data.index("β".encode("utf-8"))
        end = start + len("βeta".encode("utf-8"))
        fixture = NativeFixture(
            self.root / "unicode", source_bytes=data, spans=[(start, end)]
        )
        result = fixture.normalize()
        self.assertEqual(
            result["evidence"][0]["source"]["line_range"],
            {
                "start_line": 2,
                "start_column": 1,
                "end_line": 2,
                "end_column": 5,
            },
        )

        for name, start_byte, end_byte in (
            ("past_end", start, len(data) + 1),
            ("reversed", end, start),
            ("unicode_split", start + 1, end),
            ("crlf_split", data.index(b"\r") + 1, end),
        ):
            with self.subTest(name=name):
                broken = NativeFixture(
                    self.root / f"offset-{name}", source_bytes=data, spans=[(start, end)]
                )
                broken.ir["working_set"][0]["source"]["start_byte"] = start_byte
                broken.ir["working_set"][0]["source"]["end_byte"] = end_byte
                broken.ir["working_set"][0]["cost"]["source_bytes"] = max(0, end_byte - start_byte)
                broken.ir["cost"]["selected_source_bytes"] = max(0, end_byte - start_byte)
                broken.reseal_context(update_database=True)
                with self.assertRaisesRegex(adapter.AdapterError, "source byte range"):
                    broken.normalize()

    def test_identity_drift_and_ambiguous_metric_rows_reject(self) -> None:
        fields = [
            ("workspace_id", "ws_other", "expected identity mismatch"),
            ("generation_id", "gen_other", "expected identity mismatch"),
            ("configuration_hash", "0" * 64, "expected identity mismatch"),
            ("task_session_id", "task_other", "expected identity mismatch"),
            ("request_hash", "1" * 64, "query metric row binding"),
        ]
        for field, value, message in fields:
            with self.subTest(field=field):
                fixture = NativeFixture(self.root / f"identity-{field}")
                fixture.descriptor["expected"][field] = value
                fixture.write_descriptor()
                with self.assertRaisesRegex(adapter.AdapterError, message):
                    fixture.normalize()

        ambiguous = NativeFixture(self.root / "ambiguous")
        connection = sqlite3.connect(ambiguous.catalogue_path)
        connection.execute(
            "INSERT INTO query_stage_metric SELECT 'metric_duplicate', workspace_id, generation_id, task_session_id, context_id, request_id, operation, serving_fallback, seed_resolution_us, serving_lookup_us, graph_expansion_us, ranking_us, source_verify_us, source_read_us, packing_us, candidate_count, expanded_edge_count, source_bytes_read, returned_records, returned_estimated_tokens, cache_hits, cache_misses, truncated FROM query_stage_metric"
        )
        connection.commit()
        connection.close()
        ambiguous.reseal_catalogue()
        with self.assertRaisesRegex(adapter.AdapterError, "ambiguous query metric"):
            ambiguous.normalize()

    def test_status_change_and_catalogue_wal_ambiguity_reject(self) -> None:
        changed = NativeFixture(self.root / "status-change")
        changed_status = copy.deepcopy(changed.status)
        changed_status["active_generation_sequence"] = 2
        changed.status_after_path.write_bytes(canonical_bytes(changed_status) + b"\n")
        changed.descriptor["files"]["status_after"] = changed._file_ref(
            changed.status_after_path
        )
        changed.write_descriptor()
        with self.assertRaisesRegex(adapter.AdapterError, "before/after status changed"):
            changed.normalize()

        ambiguous = NativeFixture(self.root / "wal-ambiguity")
        Path(str(ambiguous.catalogue_path) + "-wal").write_bytes(b"retained")
        with self.assertRaisesRegex(adapter.AdapterError, "WAL or journal ambiguity"):
            ambiguous.normalize()

    def test_commit_is_declared_only_and_must_match_optional_corpus(self) -> None:
        fixture = NativeFixture(self.root / "commit-mismatch")
        corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
        corpus["repository"]["commit"] = "e" * 40
        corpus_path = fixture.evidence / "corpus.json"
        corpus_path.write_bytes(canonical_bytes(corpus) + b"\n")
        fixture.descriptor["files"]["benchmark_corpus"] = fixture._file_ref(
            corpus_path
        )
        fixture.write_descriptor()

        with self.assertRaisesRegex(
            adapter.AdapterError, "descriptor repository commit disagrees"
        ):
            fixture.normalize()

    def test_unrelated_and_oversized_aggregate_sources_reject_before_reads(self) -> None:
        unrelated = NativeFixture(self.root / "unrelated-source")
        unrelated.descriptor["source_files"].append({
            "path": "config/unrelated-large.toml",
            "sha256": "0" * 64,
            "bytes": adapter.MAX_SOURCE_BYTES,
            "encoding": "utf-8",
        })
        unrelated.write_descriptor()
        with self.assertRaisesRegex(
            adapter.AdapterError, "unrelated descriptor source paths"
        ):
            unrelated.normalize()

        aggregate = NativeFixture(self.root / "aggregate-source")
        for item, path in zip(
            aggregate.ir["working_set"][:2],
            ("config/extra-a.toml", "config/extra-b.toml"),
            strict=True,
        ):
            item["source"]["path"] = path
            item["source"]["indexed_hash"] = "0" * 64
        aggregate.reseal_context(update_database=True)
        for path in ("config/extra-a.toml", "config/extra-b.toml"):
            aggregate.descriptor["source_files"].append({
                "path": path,
                "sha256": "0" * 64,
                "bytes": adapter.MAX_SOURCE_BYTES,
                "encoding": "utf-8",
            })
        aggregate.write_descriptor()
        with self.assertRaisesRegex(
            adapter.AdapterError, "aggregate source byte bound"
        ):
            aggregate.normalize()

    def test_partial_or_different_native_identity_never_maps_to_corpus_locator(self) -> None:
        source = FROZEN_CONFIG.read_bytes()
        corpus_commit = json.loads(CORPUS.read_text(encoding="utf-8"))[
            "repository"
        ]["commit"]
        partial = NativeFixture(
            self.root / "mapping-partial", source_bytes=source, spans=[(957, 969)]
        )
        partial.items[0]["entity_kind"] = "config"
        partial.items[0]["entity_id"] = partial.source_path
        partial.ir["working_set"] = partial.items
        partial.reseal_context(update_database=True)
        corpus_copy = partial.evidence / "corpus.json"
        corpus_copy.write_bytes(CORPUS.read_bytes())
        partial.descriptor["files"]["benchmark_corpus"] = partial._file_ref(corpus_copy)
        partial.descriptor["repository_commit"] = corpus_commit
        partial.descriptor["benchmark_locator_candidates"] = [{
            "item_id": "item_0",
            "task_id": "configuration-project-marker-validation",
            "locator_id": "config/workspace-atlas-v1.1.config.example.toml",
        }]
        partial.write_descriptor()
        result = partial.normalize()
        self.assertIsNone(result["evidence"][0]["benchmark_mapping"])
        self.assertEqual(
            result["evidence"][0]["benchmark_mapping_reason"],
            "required_range_not_fully_covered",
        )

        different = NativeFixture(
            self.root / "mapping-different", source_bytes=source, spans=[(0, len(source))]
        )
        corpus_copy = different.evidence / "corpus.json"
        corpus_copy.write_bytes(CORPUS.read_bytes())
        different.descriptor["files"]["benchmark_corpus"] = different._file_ref(corpus_copy)
        different.descriptor["repository_commit"] = corpus_commit
        different.descriptor["benchmark_locator_candidates"] = [{
            "item_id": "item_0",
            "task_id": "configuration-project-marker-validation",
            "locator_id": "src/config.rs::Config::validate",
        }]
        different.write_descriptor()
        result = different.normalize()
        self.assertIsNone(result["evidence"][0]["benchmark_mapping"])
        self.assertEqual(
            result["evidence"][0]["benchmark_mapping_reason"],
            "identity_mismatch",
        )

    def test_sourceless_costs_budget_overruns_and_tree_mismatch_reject(self) -> None:
        sourceless = NativeFixture(self.root / "sourceless")
        sourceless.ir["working_set"][0]["source"] = None
        sourceless.reseal_context(update_database=True)
        with self.assertRaisesRegex(
            adapter.AdapterError, "source-less item has positive source cost"
        ):
            sourceless.normalize()

        budget_cases = [
            ("max_records", "selected_records"),
            ("max_source_bytes", "selected_source_bytes"),
            ("max_estimated_tokens", "selected_estimated_tokens"),
        ]
        for maximum, selected in budget_cases:
            with self.subTest(maximum=maximum):
                fixture = NativeFixture(self.root / f"budget-{maximum}")
                fixture.ir["cost"][maximum] = fixture.ir["cost"][selected] - 1
                fixture.reseal_context(update_database=True)
                with self.assertRaisesRegex(
                    adapter.AdapterError, "selected .* exceeds native budget"
                ):
                    fixture.normalize()

        matched = NativeFixture(self.root / "tree-matched")
        matched.ir["workspace"]["source_tree_hash"] = matched.source_tree_hash
        matched.reseal_context(update_database=True)
        matched_result = matched.normalize()
        self.assertEqual(
            matched_result["bindings"]["ir_source_tree_hash"],
            matched.source_tree_hash,
        )
        self.assertEqual(
            matched_result["bindings"]["indexed_generation_source_tree_hash"],
            matched.source_tree_hash,
        )

        mismatched = NativeFixture(self.root / "tree-mismatched")
        mismatched.ir["workspace"]["source_tree_hash"] = sha256(b"other-tree")
        mismatched.reseal_context(update_database=True)
        with self.assertRaisesRegex(
            adapter.AdapterError, "IR source tree hash disagrees"
        ):
            mismatched.normalize()

    def test_cli_descriptor_and_output_paths_are_lexically_confined(self) -> None:
        fixture = NativeFixture(self.root / "cli-paths")
        outside_descriptor = fixture.root / "outside-descriptor.json"
        outside_descriptor.write_bytes(fixture.descriptor_path.read_bytes())
        for descriptor in (
            Path("..") / "outside-descriptor.json",
            outside_descriptor,
        ):
            with self.subTest(descriptor=str(descriptor)):
                with self.assertRaisesRegex(
                    adapter.AdapterError, "canonical relative path"
                ):
                    adapter.normalize(
                        fixture.evidence,
                        fixture.source_root,
                        descriptor,
                    )

        nested = fixture.evidence / "nested"
        nested.mkdir()
        nested_descriptor = nested / "descriptor.json"
        nested_descriptor.write_bytes(fixture.descriptor_path.read_bytes())
        normalized = adapter.normalize(
            fixture.evidence,
            fixture.source_root,
            Path("nested/descriptor.json"),
        )
        nested_output = fixture.evidence / "nested-output"
        nested_output.mkdir()
        adapter._write_output(
            fixture.evidence,
            Path("nested-output/result.json"),
            normalized,
        )
        self.assertTrue((nested_output / "result.json").is_file())

        outside_output = fixture.root / "outside-output.json"
        for output in (Path("..") / "outside-output.json", outside_output):
            with self.subTest(output=str(output)):
                with self.assertRaisesRegex(
                    adapter.AdapterError, "canonical relative path"
                ):
                    adapter._write_output(
                        fixture.evidence,
                        output,
                        normalized,
                    )
        self.assertFalse(outside_output.exists())

        target = fixture.root / "output-target"
        target.mkdir()
        link = fixture.evidence / "output-link"
        if os.name == "nt":
            created = subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(link), str(target)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(created.returncode, 0, created.stderr)
        else:
            os.symlink(target, link)
        with self.assertRaisesRegex(
            adapter.AdapterError, "link or reparse point"
        ):
            adapter._write_output(
                fixture.evidence,
                Path("output-link/result.json"),
                normalized,
            )

    def test_selected_bytes_cannot_be_relabelled_as_delivered(self) -> None:
        fixture = NativeFixture(self.root / "delivered")
        connection = sqlite3.connect(fixture.catalogue_path)
        connection.execute(
            "UPDATE query_stage_metric SET source_bytes_read=?",
            (fixture.ir["cost"]["selected_source_bytes"],),
        )
        connection.commit()
        connection.close()
        fixture.reseal_catalogue()
        with self.assertRaisesRegex(adapter.AdapterError, "delivered source bytes contradict"):
            fixture.normalize()

    def test_cli_roundtrip_is_bounded_and_output_is_exclusive(self) -> None:
        fixture = NativeFixture(self.root / "cli")
        command = [
            sys.executable,
            str(SCRIPT),
            "--evidence-workspace",
            str(fixture.evidence),
            "--source-root",
            str(fixture.source_root),
            "--descriptor",
            "descriptor.json",
            "--output",
            "normalized.json",
        ]
        completed = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        output = json.loads(fixture.output_path.read_text(encoding="utf-8"))
        self.assertEqual(json.loads(completed.stdout)["sha256"], output["normalization_sha256"])
        self.assertTrue(fixture.output_path.read_bytes().endswith(b"\n"))
        repeated = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertNotEqual(repeated.returncode, 0)
        self.assertIn("output already exists", repeated.stderr)

        with self.assertRaisesRegex(adapter.AdapterError, "output byte bound"):
            adapter._write_output(
                fixture.evidence,
                Path("oversized-output.json"),
                {"payload": "x" * adapter.MAX_OUTPUT_BYTES},
            )


if __name__ == "__main__":
    unittest.main()
