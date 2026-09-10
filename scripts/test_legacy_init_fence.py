#!/usr/bin/env python3
"""Focused process-boundary tests for the private legacy-init fence."""

from __future__ import annotations

import contextlib
import hashlib
import io
import importlib.util
import json
import pathlib
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True

REPOSITORY = pathlib.Path(__file__).resolve().parent.parent
FENCE = pathlib.Path(__file__).with_name("legacy-init-fence.py")
PRESERVED_BINARY = (
    REPOSITORY
    / "target/rp2/rp7-test-artifacts"
    / "atlas-max-migration-3-27a5d620ebaaec42df5ce6b526e1bdcaaacfd567.exe"
)
FIXTURES = REPOSITORY / "target/rp2/t17-post-rp10-review-fixtures/legacy-fence"
EXPECTED_BINARY_SHA256 = (
    "c61f71b9e17a46c06aabbc40383de4e549061e860da0bb74f7eaa5eea601fabf"
)

SPEC = importlib.util.spec_from_file_location("legacy_init_fence", FENCE)
assert SPEC is not None and SPEC.loader is not None
fence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = fence
SPEC.loader.exec_module(fence)


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def catalogue_state(path: pathlib.Path) -> tuple[str, int, int, str]:
    connection = sqlite3.connect(
        f"{path.resolve().as_uri()}?mode=ro&immutable=1", uri=True
    )
    try:
        return (
            sha256(path),
            connection.execute("PRAGMA user_version").fetchone()[0],
            connection.execute("SELECT MAX(version) FROM schema_migration").fetchone()[0],
            connection.execute("PRAGMA integrity_check").fetchone()[0],
        )
    finally:
        connection.close()


@unittest.skipUnless(PRESERVED_BINARY.is_file(), "preserved RP7 executable is unavailable")
class LegacyInitFenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        self.workspace = REPOSITORY / "target/rp2/rp9-test-artifacts/workspace"
        self.config = REPOSITORY / "target/rp2/rp9-test-artifacts/atlas.toml"
        self.assertEqual(sha256(PRESERVED_BINARY), EXPECTED_BINARY_SHA256)

    def run_fence(self, catalogue: pathlib.Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(FENCE),
                "--legacy-binary",
                str(PRESERVED_BINARY),
                "--workspace-root",
                str(self.workspace),
                "--config",
                str(self.config),
                "--catalogue",
                str(catalogue),
            ],
            cwd=REPOSITORY,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_schema4_refuses_before_legacy_launch_without_filesystem_or_database_change(self) -> None:
        catalogue = self.directory / "schema4.sqlite"
        shutil.copy2(FIXTURES / "schema4-base.sqlite", catalogue)
        before = catalogue_state(catalogue)
        before_entries = sorted(path.name for path in self.directory.iterdir())

        result = self.run_fence(catalogue)

        self.assertEqual(result.returncode, fence.FENCE_REFUSAL_EXIT)
        refusal = json.loads(result.stderr)
        self.assertEqual(refusal["kind"], "legacy_init_fence")
        self.assertEqual(refusal["catalogue_user_version"], 4)
        self.assertIn("before launching", refusal["error"])
        self.assertEqual(catalogue_state(catalogue), before)
        self.assertEqual(
            sorted(path.name for path in self.directory.iterdir()), before_entries
        )
        self.assertFalse(
            any(".pre-migration-" in path.name for path in self.directory.iterdir())
        )

    def test_schema3_is_permitted_and_explicit_backup_restore_stays_caller_owned(self) -> None:
        source = FIXTURES / "schema3.sqlite"
        external_backup = self.directory / "caller-owned-schema3-backup.sqlite"
        catalogue = self.directory / "schema3.sqlite"
        shutil.copy2(source, external_backup)
        shutil.copy2(external_backup, catalogue)
        backup_hash = sha256(external_backup)

        result = self.run_fence(catalogue)

        self.assertEqual(result.returncode, 0, result.stderr)
        permitted = json.loads(result.stdout)
        self.assertEqual(permitted["kind"], "legacy_init_already_initialized")
        self.assertFalse(permitted["legacy_process_launched"])
        self.assertEqual(permitted["backup_authority"], "caller_owned")
        self.assertEqual(catalogue_state(catalogue)[1:], (3, 3, "ok"))
        self.assertEqual(sha256(external_backup), backup_hash)
        self.assertFalse(
            any(".pre-migration-" in path.name for path in self.directory.iterdir())
        )
        restored = self.directory / "restored.sqlite"
        shutil.copy2(external_backup, restored)
        self.assertEqual(sha256(restored), backup_hash)
        self.assertEqual(catalogue_state(restored)[1:], (3, 3, "ok"))

    def test_partial_schema3_identity_index_refuses_without_launch_or_change(self) -> None:
        catalogue = self.directory / "partial-index.sqlite"
        shutil.copy2(FIXTURES / "schema3.sqlite", catalogue)
        connection = sqlite3.connect(catalogue)
        try:
            connection.execute("DROP INDEX idx_file_revision_identity")
            connection.execute(
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "ifnull(language, '')) WHERE is_test = 0"
            )
            connection.commit()
        finally:
            connection.close()
        before = catalogue_state(catalogue)
        before_entries = sorted(path.name for path in self.directory.iterdir())

        result = self.run_fence(catalogue)

        self.assertEqual(result.returncode, fence.FENCE_REFUSAL_EXIT)
        refusal = json.loads(result.stderr)
        self.assertEqual(refusal["kind"], "legacy_init_fence")
        self.assertEqual(refusal["catalogue_user_version"], 3)
        self.assertEqual(result.stdout, "")

        arguments = [
            "--legacy-binary",
            str(PRESERVED_BINARY),
            "--workspace-root",
            str(self.workspace),
            "--config",
            str(self.config),
            "--catalogue",
            str(catalogue),
        ]
        with mock.patch.object(fence.subprocess, "run") as legacy_run:
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(fence.main(arguments), fence.FENCE_REFUSAL_EXIT)
        legacy_run.assert_not_called()
        self.assertEqual(catalogue_state(catalogue), before)
        self.assertEqual(
            sorted(path.name for path in self.directory.iterdir()), before_entries
        )
        self.assertFalse(
            any(".pre-migration-" in path.name for path in self.directory.iterdir())
        )

    def test_schema3_identity_index_requires_exact_structural_definition(self) -> None:
        definitions: dict[str, str | None] = {
            "absent": None,
            "nonunique": (
                "CREATE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "ifnull(language, ''))"
            ),
            "extra-key": (
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "ifnull(language, ''), is_test)"
            ),
            "expression": (
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "coalesce(language, ''))"
            ),
            "expression-literal": (
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "ifnull(language, ' '))"
            ),
            "collation": (
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id, content_hash, artifact_class, "
                "ifnull(language, '') COLLATE NOCASE)"
            ),
            "order": (
                "CREATE UNIQUE INDEX idx_file_revision_identity "
                "ON file_revision(file_id DESC, content_hash, artifact_class, "
                "ifnull(language, ''))"
            ),
        }
        for label, definition in definitions.items():
            with self.subTest(label=label):
                catalogue = self.directory / f"{label}-index.sqlite"
                shutil.copy2(FIXTURES / "schema3.sqlite", catalogue)
                connection = sqlite3.connect(catalogue)
                try:
                    connection.execute("DROP INDEX idx_file_revision_identity")
                    if definition is not None:
                        connection.execute(definition)
                    connection.commit()
                finally:
                    connection.close()
                before = catalogue_state(catalogue)
                before_entries = sorted(path.name for path in self.directory.iterdir())

                with self.assertRaises(fence.FenceRefusal):
                    fence.inspect_catalogue(catalogue)

                self.assertEqual(catalogue_state(catalogue), before)
                self.assertEqual(
                    sorted(path.name for path in self.directory.iterdir()),
                    before_entries,
                )
                self.assertFalse(
                    any(
                        ".pre-migration-" in path.name
                        for path in self.directory.iterdir()
                    )
                )


    def test_absent_catalogue_launches_fixed_legacy_init_without_a_sibling(self) -> None:
        catalogue = self.directory / "new-schema3.sqlite"

        result = self.run_fence(catalogue)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(catalogue.is_file())
        self.assertEqual(catalogue_state(catalogue)[1:], (3, 3, "ok"))
        self.assertFalse(
            any(".pre-migration-" in path.name for path in self.directory.iterdir())
        )

    def test_unreadable_ambiguous_and_sidecar_states_fail_closed(self) -> None:
        unreadable = self.directory / "unreadable.sqlite"
        unreadable.write_bytes(b"not a sqlite catalogue")
        with self.assertRaises(fence.FenceRefusal):
            fence.inspect_catalogue(unreadable)

        ambiguous = self.directory / "ambiguous.sqlite"
        shutil.copy2(FIXTURES / "schema3.sqlite", ambiguous)
        connection = sqlite3.connect(ambiguous)
        connection.execute("PRAGMA journal_mode = DELETE")
        connection.execute("PRAGMA user_version = 2")
        connection.close()
        with self.assertRaises(fence.FenceRefusal):
            fence.inspect_catalogue(ambiguous)

        sidecar = self.directory / "sidecar.sqlite"
        shutil.copy2(FIXTURES / "schema3.sqlite", sidecar)
        pathlib.Path(f"{sidecar}-wal").write_bytes(b"uncheckpointed state")
        with self.assertRaises(fence.FenceRefusal):
            fence.inspect_catalogue(sidecar)

    def test_closed_parser_cannot_override_checked_catalogue_or_init_command(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                fence.parse_args(
                    [
                        "--legacy-binary",
                        str(PRESERVED_BINARY),
                        "--workspace-root",
                        str(self.workspace),
                        "--catalogue",
                        str(self.directory / "checked.sqlite"),
                        "--",
                        "--catalogue",
                        str(self.directory / "bypass.sqlite"),
                    ]
                )


if __name__ == "__main__":
    unittest.main()
