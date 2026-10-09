"""Provider-free checks of the fixed Rust evidence validator profile."""

import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import rust_semantic_evidence as evidence


PROVIDER = {
    "name": "rust-analyzer",
    "version": "1.98.1",
    "command": r"C:\Users\<user>\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rust-analyzer.exe",
    "executable_hash": "175d90eec94f4d34ed9401fadae7f0faf61560c539f9d5f09f930523036aac27",
}
KEY = "rust-analyzer@1.98.1#14b604e49bbb9667"
FINGERPRINT = "14b604e49bbb966707b0be5d16d022bde1aa7e88f88724b75b2d51b69c8f0b08"
CONFIGURATION_HASH = "15de231acb99e769a199159062823c8c39fa9a29c1a69bcc815e7f67070cc625"
CONFIGURATION = '{"qualification_condition":"installed-provider-1.98.1"}'


class RustEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="atlas-validator-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()

    def catalogue(self, name="catalogue.sqlite", *, wal=False):
        """Synthetic rows in the two actual production table definitions."""
        path = self.root / name
        migration = (Path(__file__).resolve().parents[1] / "migrations" /
                     "0002_semantic_provider_foundation.sql").read_text(encoding="utf-8")
        statements = re.findall(
            r"CREATE TABLE IF NOT EXISTS (?:provider_descriptor|workspace_provider) \(.*?\n\);",
            migration, re.DOTALL,
        )
        self.assertEqual(len(statements), 2)
        connection = sqlite3.connect(path)
        try:
            if wal:
                self.assertEqual(connection.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))
            connection.executescript("\n".join(statements))
            connection.execute(
                "INSERT INTO provider_descriptor (provider_key,provider_name,provider_version,"
                "provider_tier,execution_kind,execution_scope,protocol_version,output_format,"
                "executable_identity,executable_hash,descriptor_hash,deterministic,"
                "capabilities_json,limitations_json,registered_at) "
                "VALUES (?,?,?,'semantic_index','external_process','project','1.0.0','scip',"
                "?,NULL,?,1,'[]','[]','synthetic')",
                (KEY, "rust-analyzer", "1.98.1", PROVIDER["command"], FINGERPRINT),
            )
            connection.execute(
                "INSERT INTO workspace_provider (workspace_id,provider_key,enabled,required,"
                "priority,configuration_json,configuration_hash,timeout_ms,max_stdout_bytes,"
                "max_stderr_bytes,max_output_bytes,network_isolation_policy,updated_at) "
                "VALUES ('workspace',?,1,1,0,?,?,1,0,0,1,'not_required','synthetic')",
                (KEY, CONFIGURATION, CONFIGURATION_HASH),
            )
            connection.commit()
        finally:
            connection.close()
        return path

    def inspect(self, path, **overrides):
        arguments = {"workspace_id": "workspace", "provider": PROVIDER,
                     "writer_quiescent": True}
        arguments.update(overrides)
        return evidence.inspect_provider_binding(path, **arguments)

    @staticmethod
    def sql(path, statement, parameters=()):
        connection = sqlite3.connect(path)
        try:
            connection.execute(statement, parameters)
            connection.commit()
        finally:
            connection.close()

    def test_identity_matches_accepted_synthetic_golden_vector(self):
        self.assertEqual(evidence.rust_provider_identity(PROVIDER),
                         (KEY, FINGERPRINT, CONFIGURATION_HASH))

    def test_unicode_command_uses_utf8_and_preserves_raw_command(self):
        provider = dict(PROVIDER, command="工具/rust-analyzer")
        # A second fixed vector exercises serde_json's unescaped UTF-8 form.
        self.assertEqual(evidence.rust_provider_identity(provider), (
            "rust-analyzer@1.98.1#043d3dbd425b9a42",
            "043d3dbd425b9a429c2ac8053e93a8685ccc2e0d9ab7e264845beec5a3b34ce9", CONFIGURATION_HASH,
        ))
        self.assertNotEqual(evidence.rust_provider_identity(provider),
                            evidence.rust_provider_identity(dict(provider, command="./工具/rust-analyzer")))

    def test_identity_rejects_missing_extra_and_wrong_typed_fields(self):
        invalid = [None, [], {key: value for key, value in PROVIDER.items() if key != "command"},
                   dict(PROVIDER, extra="unbound")]
        invalid.extend(dict(PROVIDER, **{key: value}) for key, value in (
            ("name", "other"), ("version", "1.94.1"), ("executable_hash", "x" * 64),
            ("executable_hash", "A" * 64), ("executable_hash", None),
            ("command", ""), ("command", 1),
        ))
        for provider in invalid:
            with self.subTest(provider=provider), self.assertRaises(ValueError):
                evidence.rust_provider_identity(provider)

    def test_production_null_hash_and_required_policy_are_accepted(self):
        self.assertEqual(self.inspect(self.catalogue()),
                         ((KEY, "1.98.1", None, FINGERPRINT), (1, 1, CONFIGURATION_HASH)))

    def test_every_selected_descriptor_field_is_bound(self):
        changes = {
            "provider_key": "other-key", "provider_version": "other-version",
            "executable_hash": PROVIDER["executable_hash"], "descriptor_hash": "0" * 64,
            "executable_identity": "rust-analyzer", "execution_kind": "builtin",
            "execution_scope": "file", "output_format": "normalized_json",
            "provider_tier": "structural", "protocol_version": "other-protocol",
        }
        for column, value in changes.items():
            with self.subTest(column=column):
                path = self.catalogue(column + ".sqlite")
                self.sql(path, f"UPDATE provider_descriptor SET {column}=?", (value,))
                with self.assertRaisesRegex(ValueError, "descriptor identity"):
                    self.inspect(path)

    def test_provider_policy_and_configuration_are_bound(self):
        changes = [("enabled", 0), ("required", 0), ("configuration_hash", "0" * 64),
                   ("configuration_json", "{}"), ("configuration_json", "invalid"),
                   ("configuration_json", "null"),
                   ("configuration_json", '{"qualification_condition":"other"}'),
                   ("configuration_json", '{"qualification_condition":"installed-provider-1.98.1","extra":1}')]
        for index, (column, value) in enumerate(changes):
            with self.subTest(column=column, value=value):
                path = self.catalogue(f"policy-{index}.sqlite")
                self.sql(path, f"UPDATE workspace_provider SET {column}=?", (value,))
                with self.assertRaisesRegex(ValueError, "policy configuration"):
                    self.inspect(path)

    def test_configuration_json_whitespace_does_not_change_identity(self):
        path = self.catalogue()
        self.sql(path, "UPDATE workspace_provider SET configuration_json=?",
                 (json.dumps(json.loads(CONFIGURATION), indent=2),))
        self.assertEqual(self.inspect(path)[1], (1, 1, CONFIGURATION_HASH))

    def test_missing_descriptor_or_policy_is_rejected(self):
        for table in ("provider_descriptor", "workspace_provider"):
            with self.subTest(table=table):
                path = self.catalogue(table + ".sqlite")
                self.sql(path, f"DELETE FROM {table}")
                with self.assertRaisesRegex(ValueError, "exactly one row"):
                    self.inspect(path)

    def test_second_rust_descriptor_is_rejected(self):
        path = self.catalogue()
        self.sql(path, "INSERT INTO provider_descriptor (provider_key,provider_name,provider_version,"
                 "provider_tier,execution_kind,execution_scope,protocol_version,output_format,"
                 "executable_identity,executable_hash,descriptor_hash,deterministic,"
                 "capabilities_json,limitations_json,registered_at) SELECT 'other-key',provider_name,"
                 "'other-version',provider_tier,execution_kind,execution_scope,protocol_version,"
                 "output_format,executable_identity,executable_hash,descriptor_hash,"
                 "deterministic,capabilities_json,limitations_json,registered_at FROM provider_descriptor")
        with self.assertRaisesRegex(ValueError, "exactly one row"):
            self.inspect(path)

    def test_workspace_selector_is_bound(self):
        path = self.catalogue()
        for value in ("other-workspace", "' OR 1=1 --", "", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.inspect(path, workspace_id=value)

    def test_missing_schema_column_and_invalid_database_fail_closed(self):
        missing = self.catalogue("missing.sqlite")
        self.sql(missing, "DROP TABLE workspace_provider")
        column = self.catalogue("column.sqlite")
        self.sql(column, "ALTER TABLE provider_descriptor RENAME COLUMN executable_identity TO other")
        invalid = self.root / "invalid.sqlite"
        invalid.write_bytes(b"not a database")
        for path in (missing, column, invalid):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "schema/query"):
                self.inspect(path)

    def test_completed_wal_mode_snapshot_preserves_bytes_and_membership(self):
        path = self.catalogue(wal=True)
        before = {item.name: item.read_bytes() for item in self.root.iterdir()}
        self.inspect(path)
        self.assertEqual({item.name: item.read_bytes() for item in self.root.iterdir()}, before)

    def test_empty_wal_is_accepted_and_preserved(self):
        path = self.catalogue()
        wal = Path(str(path) + "-wal")
        wal.write_bytes(b"")
        before = {item.name: item.read_bytes() for item in self.root.iterdir()}
        self.inspect(path)
        self.assertEqual({item.name: item.read_bytes() for item in self.root.iterdir()}, before)

    def test_nonempty_or_nonregular_wal_is_refused_before_sqlite_open(self):
        for directory in (False, True):
            with self.subTest(directory=directory):
                path = self.catalogue(f"wal-{directory}.sqlite")
                wal = Path(str(path) + "-wal")
                if directory:
                    wal.mkdir()
                else:
                    wal.write_bytes(b"unsettled WAL")
                with mock.patch.object(evidence.sqlite3, "connect") as connect:
                    with self.assertRaisesRegex(ValueError, "self-contained snapshot"):
                        self.inspect(path)
                    connect.assert_not_called()

    def test_quiescence_is_a_required_caller_precondition(self):
        path = self.catalogue()
        for value in (False, None, 1, "true"):
            with self.subTest(value=value), mock.patch.object(evidence.sqlite3, "connect") as connect:
                with self.assertRaisesRegex(ValueError, "quiescent"):
                    self.inspect(path, writer_quiescent=value)
                connect.assert_not_called()

    def test_snapshot_connection_is_read_only_and_closed(self):
        path = self.catalogue()
        with evidence._completed_snapshot_connection(path, writer_quiescent=True) as connection:
            with self.assertRaises(sqlite3.OperationalError):
                connection.execute("DELETE FROM provider_descriptor")
        with self.assertRaises(sqlite3.ProgrammingError):
            connection.execute("SELECT 1")

    def test_snapshot_connection_closes_on_validation_failure(self):
        path = self.catalogue()
        self.sql(path, "DELETE FROM workspace_provider")
        connection = sqlite3.connect(path.as_uri() + "?mode=ro&immutable=1", uri=True)
        with mock.patch.object(evidence.sqlite3, "connect", return_value=connection):
            with self.assertRaises(ValueError):
                self.inspect(path)
        with self.assertRaises(sqlite3.ProgrammingError):
            connection.execute("SELECT 1")

    def executable(self):
        path = self.root / "retained.exe"
        data = b"synthetic retained executable"
        path.write_bytes(data)
        return path, {"expected_bytes": len(data), "expected_sha256": hashlib.sha256(data).hexdigest()}

    def test_retained_executable_accepts_two_real_hardlinks(self):
        path, pins = self.executable()
        alias = self.root / "retained-alias.exe"
        os.link(path, alias)
        self.assertEqual(path.stat().st_nlink, 2)
        self.assertEqual(evidence.hash_retained_executable(path, **pins), pins["expected_sha256"])
        self.assertEqual(alias.read_bytes(), path.read_bytes())

    def test_size_hash_and_same_size_alias_drift_are_rejected(self):
        path, pins = self.executable()
        alias = self.root / "retained-alias.exe"
        os.link(path, alias)
        for changed in (dict(pins, expected_bytes=pins["expected_bytes"] + 1),
                        dict(pins, expected_sha256="0" * 64)):
            with self.subTest(pins=changed), self.assertRaises(ValueError):
                evidence.hash_retained_executable(path, **changed)
        alias.write_bytes(b"x" * pins["expected_bytes"])
        with self.assertRaisesRegex(ValueError, "SHA256 binding"):
            evidence.hash_retained_executable(path, **pins)

    def test_malformed_executable_pins_are_rejected(self):
        path, pins = self.executable()
        for field, values in (("expected_bytes", (True, -1, None, "1")),
                              ("expected_sha256", (None, "x" * 64, "A" * 64, "short"))):
            for value in values:
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    evidence.hash_retained_executable(path, **dict(pins, **{field: value}))

    def test_missing_relative_and_nonregular_files_are_rejected(self):
        path, pins = self.executable()
        for value in (self.root / "missing", Path("relative.exe"), self.root):
            with self.subTest(value=value), self.assertRaises(ValueError):
                evidence.hash_retained_executable(value, **pins)
            with self.assertRaises(ValueError):
                self.inspect(value)

    def test_owned_catalogue_source_and_capture_files_still_reject_hardlinks(self):
        catalogue = self.catalogue()
        source = self.root / "source.rs"
        capture = self.root / "capture.json"
        source.write_bytes(b"synthetic source")
        capture.write_bytes(b"{}")
        for path in (catalogue, source, capture):
            with self.subTest(path=path):
                os.link(path, self.root / (path.name + ".alias"))
                with self.assertRaisesRegex(ValueError, "single-link"):
                    evidence._owned_regular_file(path)
        with self.assertRaisesRegex(ValueError, "single-link"):
            self.inspect(catalogue)

    def test_hardlinked_wal_is_not_treated_as_owned_snapshot_input(self):
        path = self.catalogue()
        wal = Path(str(path) + "-wal")
        wal.write_bytes(b"")
        os.link(wal, self.root / "wal-alias")
        with self.assertRaises(ValueError):
            self.inspect(path)

    def test_reparse_and_symlink_metadata_on_file_or_ancestor_is_rejected(self):
        path, pins = self.executable()
        original = Path.lstat
        for target in (path, self.root):
            for symbolic in (False, True):
                def guarded(candidate):
                    if candidate == target:
                        return SimpleNamespace(st_mode=stat.S_IFLNK if symbolic else stat.S_IFREG,
                                               st_file_attributes=0 if symbolic else 0x400)
                    return original(candidate)
                with self.subTest(target=target, symbolic=symbolic):
                    with mock.patch.object(Path, "lstat", autospec=True, side_effect=guarded):
                        with self.assertRaisesRegex(ValueError, "linked or reparse"):
                            evidence.hash_retained_executable(path, **pins)
                        with self.assertRaisesRegex(ValueError, "linked or reparse"):
                            evidence._owned_regular_file(path)

    @unittest.skipIf(os.name == "nt", "real symlink creation needs Windows privilege; metadata guard covered")
    def test_real_symlink_file_and_ancestor_are_rejected(self):
        path, pins = self.executable()
        link = self.root / "linked.exe"
        link.symlink_to(path)
        linked_root = self.root / "linked-root"
        linked_root.symlink_to(self.root, target_is_directory=True)
        for candidate in (link, linked_root / path.name):
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                evidence.hash_retained_executable(candidate, **pins)


if __name__ == "__main__":
    unittest.main()
