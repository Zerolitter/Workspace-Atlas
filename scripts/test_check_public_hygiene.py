#!/usr/bin/env python3
"""Focused regression tests for the public-package hygiene checker."""

from __future__ import annotations

import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

CHECKER = pathlib.Path(__file__).with_name("check-public-hygiene.py")
SPEC = importlib.util.spec_from_file_location("public_hygiene", CHECKER)
assert SPEC is not None and SPEC.loader is not None
hygiene = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = hygiene
SPEC.loader.exec_module(hygiene)


def _path_separator_runs() -> tuple[bytes, ...]:
    slash = bytes((92,))
    return tuple(
        unit * length
        for unit in (b"/", slash)
        for length in range(1, 5)
    ) + (
        slash + b"/",
        b"/" + slash,
        slash * 2 + b"/",
        slash + b"/" + slash,
        b"/" + slash * 2,
        b"/" + slash + b"/",
    )


class PathRulesTests(unittest.TestCase):
    def test_nested_internal_artifacts_and_root_reports_are_rejected(self) -> None:
        old_marker = "pre" + "work"
        rejected = [
            "docs/internal-evidence.md",
            f"tests/fixtures/{old_marker}-report.json",
            f"tests\\fixtures\\{old_marker} report.json",
            "ATLAS_CONTEXT_COMPILER_ARCHITECTURAL_DIRECTION.md",
            "docs/orchestration-reports/run.json",
        ]
        for path in rejected:
            with self.subTest(path=path):
                self.assertIsNotNone(hygiene.forbidden_path_reason(path))
                self.assertFalse(hygiene.package_path_allowed(path))

    def test_public_package_paths_remain_allowed(self) -> None:
        allowed = [
            "README.md",
            "docs/adr/020-caller-driven-reconciliation.md",
            "tests/fixtures/context_ir/context-ir.example.json",
            "schemas/scip/PROVENANCE.md",
            "src/lib.rs",
        ]
        for path in allowed:
            with self.subTest(path=path):
                self.assertIsNone(hygiene.forbidden_path_reason(path))
                self.assertTrue(hygiene.package_path_allowed(path))


class ContentRulesTests(unittest.TestCase):
    def test_legacy_markers_are_rejected_across_separators(self) -> None:
        rejected = [
            b"pre" + b"work",
            b"pre" + b"-work",
            b"pre" + b"_work",
            b"pre" + b" work",
            b"launch" + b" pack",
        ]
        for data in rejected:
            with self.subTest(data=data):
                self.assertIsNotNone(hygiene.find_forbidden_content(data))

    def test_machine_paths_and_orchestration_ids_are_rejected(self) -> None:
        slash = bytes((92,))
        rejected = [
            b"D:" + slash + b"builds" + slash + b"atlas" + slash + b"report.txt",
            b"E:" + b"/builds/atlas/report.txt",
            b"D:" + slash + slash + b"builds" + slash + slash + b"atlas",
            slash + slash + b"server" + slash + b"Users" + slash + b"alice" + slash + b"work",
            b"/private" + b"/var/folders/ab/cache/item",
            b"/home" + b"/alice/workspace",
            b"/tmp" + b"/workspace-atlas/report.json",
            b"term_" + b"12345678-1234-1234-1234-123456789abc",
            b"ctx_" + b"8616fc51f21e",
            b"task_" + b"5c27cd1cb365",
            b"dcap_" + b"MiOFq_ziBaKnlieDgl3W_u0gnwjfUpg8THlM8UsanRU",
        ]
        for data in rejected:
            with self.subTest(data=data):
                self.assertIsNotNone(hygiene.find_forbidden_content(data))

    def test_extended_device_unc_user_paths_are_rejected(self) -> None:
        slash = bytes((92,))
        prefixes = [
            (slash * 2 + b"?" + slash + b"UNC" + slash, slash),
            (slash * 2 + b"." + slash + b"UNC" + slash, slash),
            (slash * 4 + b"?" + slash * 2 + b"UNC" + slash * 2, slash * 2),
            (slash * 4 + b"." + slash * 2 + b"UNC" + slash * 2, slash * 2),
        ]
        for prefix, separator in prefixes:
            for user_root in (b"Users", b"home"):
                user_path = (
                    prefix
                    + b"server"
                    + separator
                    + user_root
                    + separator
                    + b"alice"
                )
                for data in (user_path, user_path + separator + b"workspace"):
                    with self.subTest(data=data):
                        self.assertIsNotNone(hygiene.find_forbidden_content(data))

    def test_unix_placeholder_case_matrix_is_allowed(self) -> None:
        placeholders = (b"<user>", b"<USER>", b"<UsEr>")
        suffixes = (b"", b"\n", b"/workspace", b'")', b"`", b",")
        for user_root in (b"home", b"Users"):
            for placeholder in placeholders:
                root = b"/" + user_root + b"/" + placeholder
                for suffix in suffixes:
                    data = root + suffix
                    with self.subTest(data=data):
                        self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_real_unix_user_paths_are_rejected_across_delimiters(self) -> None:
        suffixes = (b"", b"\n", b"/workspace", b'")', b"`", b",")
        for user_root in (b"home", b"Users"):
            for user in (b"alice", b"ALICE", b"<userland>"):
                root = b"/" + user_root + b"/" + user
                for suffix in suffixes:
                    data = root + suffix
                    with self.subTest(data=data):
                        self.assertEqual(
                            hygiene.find_forbidden_content(data), "Unix user home"
                        )

    def test_terminal_private_roots_are_rejected(self) -> None:
        slash = bytes((92,))
        rejected = [
            b"/home" + b"/alice",
            b"/Users" + b"/alice" + b"\n",
            b".omp" + b"/agent",
            b".omp" + slash + b"session" + b" ",
            b".omp" + b"/sessions",
            b".omp" + b"/agent" + b"/state.json",
        ]
        for data in rejected:
            with self.subTest(data=data):
                self.assertIsNotNone(hygiene.find_forbidden_content(data))

    def test_omp_roots_are_rejected_across_path_boundaries(self) -> None:
        slash = bytes((92,))
        terminators = (
            b"",
            b" ",
            b"\t",
            b"\n",
            b"\r",
            b"\v",
            b"\f",
            b'"',
            b"'",
            b"`",
            b"(",
            b")",
            b"[",
            b"]",
            b"{",
            b"}",
            b"<",
            b">",
            b",",
            b";",
            b":",
            b"!",
            b"?",
            b".",
            b"=",
            b"|",
            b"&",
            b"#",
            b"$",
            b"%",
            b"@",
            b"+",
            b"~",
            b"^",
            b"\x00",
        )
        for separator in (b"/", slash):
            for segment in (
                b"agent",
                b"AGENT",
                b"session",
                b"sessions",
                b"SeSsIoNs",
            ):
                root = b".omp" + separator + segment
                for terminator in terminators:
                    data = root + terminator
                    with self.subTest(data=data):
                        self.assertEqual(
                            hygiene.find_forbidden_content(data), "OMP session path"
                        )
                for child_separator in (b"/", slash):
                    data = root + child_separator + b"state.json"
                    with self.subTest(data=data):
                        self.assertEqual(
                            hygiene.find_forbidden_content(data), "OMP session path"
                        )

    def test_omp_roots_are_rejected_across_escaped_separator_runs(self) -> None:
        slash = bytes((92,))
        prefixes = (
            b"",
            b'Path("',
            slash * 2 + b"server" + slash + b"share" + slash,
            slash * 4 + b"server" + slash * 2 + b"share" + slash * 2,
        )
        terminators = (b"", b" ", b"\n", b'"', b".", b")")
        for prefix in prefixes:
            for separator in _path_separator_runs():
                for segment in (
                    b"agent",
                    b"AGENT",
                    b"session",
                    b"sessions",
                    b"SeSsIoNs",
                ):
                    root = prefix + b".omp" + separator + segment
                    for terminator in terminators:
                        data = root + terminator
                        with self.subTest(data=data):
                            self.assertEqual(
                                hygiene.find_forbidden_content(data),
                                "OMP session path",
                            )
                    for child_separator in _path_separator_runs():
                        data = root + child_separator + b"state.json"
                        with self.subTest(data=data):
                            self.assertEqual(
                                hygiene.find_forbidden_content(data),
                                "OMP session path",
                            )

    def test_placeholders_relative_paths_and_binary_bytes_are_allowed(self) -> None:
        slash = bytes((92,))
        allowed = [
            b"C:" + slash + b"Users" + slash + b"<user>" + slash + b"AppData",
            b"C:" + slash + b"<workspace>" + slash + b"project",
            b"C:"
            + slash
            + slash
            + b"Users"
            + slash
            + slash
            + b"<user>"
            + slash
            + slash
            + b"AppData",
            b"C:/<workspace>/project",
            b"/Users/<user>/Library/Application Support",
            b"/home/<user>/.local/share",
            b"tests/fixtures/example.json",
            b"package evidence is verified",
            b"\x00\xff\x01binary",
        ]
        for data in allowed:
            with self.subTest(data=data):
                self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_extended_device_unc_placeholders_are_allowed(self) -> None:
        slash = bytes((92,))
        prefixes = [
            (slash * 2 + b"?" + slash + b"UNC" + slash, slash),
            (slash * 2 + b"." + slash + b"UNC" + slash, slash),
            (slash * 4 + b"?" + slash * 2 + b"UNC" + slash * 2, slash * 2),
            (slash * 4 + b"." + slash * 2 + b"UNC" + slash * 2, slash * 2),
        ]
        for prefix, separator in prefixes:
            for placeholder in (b"<user>", b"<USER>", b"<UsEr>"):
                user_placeholder = (
                    prefix
                    + b"server"
                    + separator
                    + b"Users"
                    + separator
                    + placeholder
                )
                placeholder_paths = [
                    prefix
                    + b"server"
                    + separator
                    + b"share"
                    + separator
                    + b"user",
                    user_placeholder,
                    user_placeholder + b"\n",
                    user_placeholder + separator + b"workspace",
                    b'OsStr::new("' + user_placeholder + b'")',
                ]
                for data in placeholder_paths:
                    with self.subTest(data=data):
                        self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_terminal_placeholders_and_similar_omp_names_are_allowed(self) -> None:
        allowed = [
            b"/home" + b"/<user>",
            b"/Users" + b"/<user>" + b"\n",
            b'PathBuf::from("' + b"/home" + b'/<user>")',
            b".omp" + b"/agency",
            b".omp" + b"/session-template",
        ]
        for data in allowed:
            with self.subTest(data=data):
                self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_longer_omp_segment_names_are_allowed(self) -> None:
        longer_segments = (
            b"agency",
            b"agentic",
            b"agents",
            b"agent2",
            b"agent-tools",
            b"agent_tools",
            b"agent.json",
            b"agent.-archive",
            b"agent._cache",
            b"agent\xc3\xa9",
            b"agent.\xc3\xa9",
            b"session-template",
            b"session_data",
            b"session.log",
            b"session2",
            b"sessions2",
            b"sessions-archive",
            b"sessions_backup",
            b"sessions.json",
            b"sessionss",
        )
        for separator in _path_separator_runs():
            for segment in longer_segments:
                data = b".omp" + separator + segment
                with self.subTest(data=data):
                    self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_escaped_separators_preserve_omp_nonpaths(self) -> None:
        slash = bytes((92,))
        near_misses = (
            b"<agent>",
            b"<sessions>",
            b"agent-placeholder",
            b"agent-tools",
            b"agent.json",
            b"session-template",
            b"session_data",
            b"sessions-archive",
            b"sessions.json",
        )
        for separator in _path_separator_runs():
            for segment in near_misses:
                data = b".omp" + separator + segment
                with self.subTest(data=data):
                    self.assertIsNone(hygiene.find_forbidden_content(data))

        malformed = (
            b".ompx" + slash + b"/agent",
            b".omp" + slash + b"u002fagent",
            b".omp" + slash + b"x2f" + slash + b"agent",
            b".omp%2fagent",
            b".omp&sol;agent",
            b".omp" + slash + b" " + b"/agent",
        )
        for data in malformed:
            with self.subTest(data=data):
                self.assertIsNone(hygiene.find_forbidden_content(data))

    def test_checker_source_does_not_match_its_own_rules(self) -> None:
        self.assertIsNone(hygiene.find_forbidden_content(CHECKER.read_bytes()))


class IndexBlobTests(unittest.TestCase):
    def test_reads_staged_binary_blob_after_worktree_deletion(self) -> None:
        with tempfile.TemporaryDirectory() as raw_dir:
            repo = pathlib.Path(raw_dir)
            subprocess.run(["git", "init", "--quiet"], cwd=repo, check=True)
            fixture = repo / "fixture.bin"
            staged = b"index\x00bytes\xff"
            fixture.write_bytes(staged)
            subprocess.run(["git", "add", "fixture.bin"], cwd=repo, check=True)
            fixture.unlink()

            entries = hygiene.tracked_entries(repo)
            self.assertEqual([(entry.path, entry.stage) for entry in entries], [("fixture.bin", 0)])
            blobs = dict(hygiene.read_index_blobs(entries, repo))
            self.assertEqual(blobs, {"fixture.bin": staged})
            self.assertTrue(hygiene.tracked_tree_failures(repo))

    def test_staged_boundary_adversarial_matrix_uses_index_blobs(self) -> None:
        slash = bytes((92,))
        staged_cases = {
            "unix-placeholder-upper.txt": (b"/home/<USER>/workspace", None),
            "unix-placeholder-mixed.txt": (b"/Users/<UsEr>", None),
            "unix-real.txt": (b"/home" + b"/alice)", "Unix user home"),
            "omp-quoted.txt": (
                b'Path("' + b".omp" + b"/agent" + b'")',
                "OMP session path",
            ),
            "omp-descendant.txt": (
                b".omp" + slash + b"sessions" + b"/state.json",
                "OMP session path",
            ),
            "omp-near-miss.txt": (b".omp" + b"/agent-tools", None),
            "omp-source-escaped.txt": (
                b".omp" + slash * 4 + b"agent",
                "OMP session path",
            ),
            "omp-json-slash.txt": (
                b".omp" + slash + b"/sessions",
                "OMP session path",
            ),
            "omp-mixed-descendant.txt": (
                b".omp"
                + slash
                + b"/sessions"
                + b"/"
                + slash
                + b"state.json",
                "OMP session path",
            ),
            "omp-unc-escaped.txt": (
                slash * 4
                + b"server"
                + slash * 2
                + b"share"
                + slash * 2
                + b".omp"
                + slash * 4
                + b"agent",
                "OMP session path",
            ),
            "omp-near-miss-escaped.txt": (
                b".omp" + slash + b"/agent-tools",
                None,
            ),
            "omp-placeholder-escaped.txt": (
                b".omp" + slash * 4 + b"<agent>",
                None,
            ),
            "omp-malformed-escape.txt": (
                b".omp" + slash + b"u002fagent",
                None,
            ),
            "unc-real.txt": (
                slash * 2
                + b"server"
                + slash
                + b"Users"
                + slash
                + b"alice",
                "UNC user path",
            ),
            "unc-placeholder.txt": (
                slash * 2
                + b"server"
                + slash
                + b"Users"
                + slash
                + b"<UsEr>",
                None,
            ),
        }
        with tempfile.TemporaryDirectory() as raw_dir:
            repo = pathlib.Path(raw_dir)
            subprocess.run(["git", "init", "--quiet"], cwd=repo, check=True)
            for name, (data, _) in staged_cases.items():
                (repo / name).write_bytes(data)
            subprocess.run(["git", "add", *staged_cases], cwd=repo, check=True)
            for name in staged_cases:
                (repo / name).unlink()

            entries = hygiene.tracked_entries(repo)
            blobs = dict(hygiene.read_index_blobs(entries, repo))
            reasons = {
                name: hygiene.find_forbidden_content(data) for name, data in blobs.items()
            }
            self.assertEqual(
                reasons,
                {name: expected for name, (_, expected) in staged_cases.items()},
            )

    def test_unmerged_index_stage_is_reported(self) -> None:
        oid = "1" * 40
        entries = hygiene.parse_index_entries(
            f"100644 {oid} 2\tconflicted file.txt\0".encode("ascii")
        )
        self.assertEqual(entries[0].stage, 2)
        self.assertTrue(hygiene.index_entry_failures(entries))


if __name__ == "__main__":
    unittest.main()
