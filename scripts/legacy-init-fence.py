#!/usr/bin/env python3
"""Private, package-excluded pre-open fence for the preserved schema-3 Atlas init.

Direct invocation of the preserved executable's `init` command is unsupported.
This wrapper is its only supported operational entry point: it validates the
exact executable and a stopped, sidecar-free catalogue before constructing the
fixed legacy `init` invocation. Backup creation, retention, and restore remain
caller-owned.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import sqlite3
import stat
import subprocess
import sys
from collections.abc import Sequence

PRESERVED_BINARY_SHA256 = (
    "c61f71b9e17a46c06aabbc40383de4e549061e860da0bb74f7eaa5eea601fabf"
)
LEGACY_MAXIMUM_SCHEMA = 3
FENCE_REFUSAL_EXIT = 78
EXPECTED_MIGRATIONS = (
    "workspace_atlas_foundation",
    "0002_semantic_provider_foundation",
    "0003_context_intelligence_foundation",
)
EXPECTED_INTEGRITY = (
    (
        "0001_foundation",
        "80230d67861ae584dfd5438dd2292a7173387cb40bb1265945f1632b0d276754",
    ),
    (
        "0002_semantic_provider_foundation",
        "00913e5c736686034bad98130fc280bd34e2655c4e29db485f1c1ff53367e1bb",
    ),
    (
        "0003_context_intelligence_foundation",
        "10b53b1063187555448bca434ba968c5fafbaf6529c506f7aeeea9207f0da80f",
    ),
)
SIDECAR_SUFFIXES = ("-wal", "-shm", "-journal")
EXPECTED_LEGACY_IDENTITY_INDEX = re.compile(
    r"CREATE\s+UNIQUE\s+INDEX\s+idx_file_revision_identity\s+"
    r"ON\s+file_revision\s*\(\s*file_id\s*,\s*content_hash\s*,\s*"
    r"artifact_class\s*,\s*ifnull\s*\(\s*language\s*,\s*''\s*\)\s*\)\s*",
    re.IGNORECASE,
)


class FenceRefusal(RuntimeError):
    """The legacy process must not be launched for this input state."""

    def __init__(self, message: str, *, catalogue_user_version: int | None = None):
        super().__init__(message)
        self.catalogue_user_version = catalogue_user_version


@dataclasses.dataclass(frozen=True)
class CatalogueSnapshot:
    user_version: int
    sha256: str
    size: int
    modified_ns: int
    device: int
    inode: int
    registered_roots: tuple[str, ...]


def _sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _regular_nonsymlink(path: pathlib.Path, role: str) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise FenceRefusal(f"cannot inspect {role} {path}: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise FenceRefusal(f"{role} {path} must be a regular, non-symlink file")
    return metadata


def validate_preserved_binary(path: pathlib.Path) -> None:
    _regular_nonsymlink(path, "preserved legacy executable")
    try:
        actual = _sha256(path)
    except OSError as error:
        raise FenceRefusal(
            f"cannot read preserved legacy executable {path}: {error}"
        ) from error
    if actual != PRESERVED_BINARY_SHA256:
        raise FenceRefusal(
            "preserved legacy executable hash mismatch; refusing unsupported binary "
            f"{path} (expected {PRESERVED_BINARY_SHA256}, got {actual})"
        )


def _sidecars(path: pathlib.Path) -> list[pathlib.Path]:
    return [pathlib.Path(f"{path}{suffix}") for suffix in SIDECAR_SUFFIXES]


def _existing_sidecars(path: pathlib.Path) -> list[pathlib.Path]:
    present = []
    for sidecar in _sidecars(path):
        try:
            sidecar.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise FenceRefusal(f"cannot inspect catalogue sidecar {sidecar}: {error}") from error
        present.append(sidecar)
    return present


def _legacy_identity_index_is_exact(connection: sqlite3.Connection) -> bool:
    schema_rows = list(
        connection.execute(
            "SELECT tbl_name, sql FROM sqlite_schema "
            "WHERE type = 'index' AND name = 'idx_file_revision_identity'"
        )
    )
    if len(schema_rows) != 1:
        return False
    table_name, definition = schema_rows[0]
    if table_name != "file_revision" or not isinstance(definition, str):
        return False

    index_entries = [
        row
        for row in connection.execute("PRAGMA index_list('file_revision')")
        if row[1] == "idx_file_revision_identity"
    ]
    if len(index_entries) != 1:
        return False
    _, _, unique, origin, partial = index_entries[0]
    if unique != 1 or origin != "c" or partial != 0:
        return False

    column_ids = {
        str(name): int(column_id)
        for column_id, name, *_ in connection.execute("PRAGMA table_info('file_revision')")
    }
    expected_keys = [
        (0, column_ids.get("file_id"), "file_id", 0, "BINARY", 1),
        (1, column_ids.get("content_hash"), "content_hash", 0, "BINARY", 1),
        (2, column_ids.get("artifact_class"), "artifact_class", 0, "BINARY", 1),
        (3, -2, None, 0, "BINARY", 1),
    ]
    actual_keys = [
        tuple(row)
        for row in connection.execute(
            "PRAGMA index_xinfo('idx_file_revision_identity')"
        )
        if row[5] == 1
    ]
    if actual_keys != expected_keys or any(key[1] is None for key in expected_keys[:3]):
        return False

    return EXPECTED_LEGACY_IDENTITY_INDEX.fullmatch(definition) is not None


def _read_catalogue_state(
    path: pathlib.Path,
) -> tuple[
    int,
    list[tuple[int, str]],
    list[tuple[int, str, str]],
    bool,
    str,
    list[tuple[object, ...]],
    tuple[str, ...],
]:
    try:
        connection = sqlite3.connect(
            f"{path.resolve().as_uri()}?mode=ro&immutable=1", uri=True
        )
    except (OSError, sqlite3.Error) as error:
        raise FenceRefusal(
            f"cannot open catalogue read-only for inspection {path}: {error}"
        ) from error
    try:
        connection.execute("PRAGMA query_only = ON")
        user_version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        if user_version > LEGACY_MAXIMUM_SCHEMA:
            raise FenceRefusal(
                f"catalogue schema {user_version} is newer than preserved legacy maximum "
                f"{LEGACY_MAXIMUM_SCHEMA}; refusing before launching legacy init",
                catalogue_user_version=user_version,
            )
        if user_version < 1:
            raise FenceRefusal(
                f"catalogue user_version {user_version} is not an initialized supported catalogue",
                catalogue_user_version=user_version,
            )
        if user_version < LEGACY_MAXIMUM_SCHEMA:
            raise FenceRefusal(
                f"existing catalogue schema {user_version} requires an explicit caller-owned "
                "backup and migration procedure; legacy init will not be launched",
                catalogue_user_version=user_version,
            )
        migrations = [
            (int(version), str(name))
            for version, name in connection.execute(
                "SELECT version, name FROM schema_migration ORDER BY version"
            )
        ]
        integrity_rows = [
            (int(version), str(name), str(checksum))
            for version, name, checksum in connection.execute(
                "SELECT version, migration_name, sha256 "
                "FROM migration_integrity ORDER BY version"
            )
        ]
        identity_index_is_exact = _legacy_identity_index_is_exact(connection)
        integrity = str(connection.execute("PRAGMA integrity_check").fetchone()[0])
        foreign_key_failures = list(connection.execute("PRAGMA foreign_key_check"))
        registered_roots = tuple(
            str(row[0])
            for row in connection.execute(
                "SELECT canonical_root FROM workspace ORDER BY workspace_id"
            )
        )
        return (
            user_version,
            migrations,
            integrity_rows,
            identity_index_is_exact,
            integrity,
            foreign_key_failures,
            registered_roots,
        )
    except FenceRefusal:
        raise
    except (TypeError, ValueError, sqlite3.Error) as error:
        raise FenceRefusal(
            f"catalogue state is unreadable or ambiguous at {path}: {error}"
        ) from error
    finally:
        connection.close()


def inspect_catalogue(path: pathlib.Path) -> CatalogueSnapshot | None:
    """Validate a stopped catalogue without allowing SQLite writes or recovery."""
    try:
        before = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise FenceRefusal(f"cannot inspect catalogue {path}: {error}") from error
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise FenceRefusal(f"catalogue {path} must be a regular, non-symlink file")

    present_sidecars = _existing_sidecars(path)
    if present_sidecars:
        names = ", ".join(str(sidecar) for sidecar in present_sidecars)
        raise FenceRefusal(
            f"catalogue has sidecar state ({names}); stop writers and checkpoint it before legacy init"
        )
    try:
        with path.open("rb") as source:
            header = source.read(100)
    except OSError as error:
        raise FenceRefusal(f"cannot read catalogue header {path}: {error}") from error
    if len(header) != 100 or header[:16] != b"SQLite format 3\x00":
        raise FenceRefusal(
            f"catalogue header is unreadable or not SQLite format 3 at {path}"
        )

    (
        user_version,
        migrations,
        integrity_rows,
        identity_index_is_exact,
        integrity,
        foreign_key_failures,
        registered_roots,
    ) = _read_catalogue_state(path)
    expected_migrations = [
        (version, EXPECTED_MIGRATIONS[version - 1])
        for version in range(1, user_version + 1)
    ]
    if migrations != expected_migrations:
        raise FenceRefusal(
            f"catalogue user_version {user_version} disagrees with ordered migration rows {migrations}",
            catalogue_user_version=user_version,
        )
    expected_integrity = [
        (version, *EXPECTED_INTEGRITY[version - 1])
        for version in range(1, user_version + 1)
    ]
    if integrity_rows != expected_integrity:
        raise FenceRefusal(
            f"catalogue user_version {user_version} disagrees with migration integrity rows "
            f"{integrity_rows}",
            catalogue_user_version=user_version,
        )
    if not identity_index_is_exact:
        raise FenceRefusal(
            f"catalogue identity index disagrees with legacy schema {user_version}",
            catalogue_user_version=user_version,
        )
    if integrity != "ok":
        raise FenceRefusal(
            f"catalogue integrity check did not return ok: {integrity}",
            catalogue_user_version=user_version,
        )
    if foreign_key_failures:
        raise FenceRefusal(
            f"catalogue foreign-key check failed: {foreign_key_failures}",
            catalogue_user_version=user_version,
        )

    try:
        digest = _sha256(path)
    except OSError as error:
        raise FenceRefusal(f"cannot hash inspected catalogue {path}: {error}") from error
    after = _regular_nonsymlink(path, "catalogue")
    present_after = _existing_sidecars(path)
    if present_after:
        names = ", ".join(str(sidecar) for sidecar in present_after)
        raise FenceRefusal(f"catalogue sidecar state appeared during inspection ({names})")
    before_identity = (before.st_size, before.st_mtime_ns, before.st_dev, before.st_ino)
    after_identity = (after.st_size, after.st_mtime_ns, after.st_dev, after.st_ino)
    if before_identity != after_identity:
        raise FenceRefusal(f"catalogue changed during pre-open inspection: {path}")
    return CatalogueSnapshot(
        user_version=user_version,
        sha256=digest,
        size=after.st_size,
        modified_ns=after.st_mtime_ns,
        device=after.st_dev,
        inode=after.st_ino,
        registered_roots=registered_roots,
    )


def parse_args(arguments: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fence and invoke the exact preserved maximum-schema-3 Atlas init"
    )
    parser.add_argument("--legacy-binary", type=pathlib.Path, required=True)
    parser.add_argument("--workspace-root", type=pathlib.Path, required=True)
    parser.add_argument("--catalogue", type=pathlib.Path, required=True)
    parser.add_argument("--config", type=pathlib.Path)
    parser.add_argument("--display-name")
    parser.add_argument("--human", action="store_true")
    return parser.parse_args(arguments)


def _validate_inputs(args: argparse.Namespace) -> None:
    validate_preserved_binary(args.legacy_binary)
    if not args.workspace_root.is_dir():
        raise FenceRefusal(f"workspace root is not an existing directory: {args.workspace_root}")
    if args.config is not None:
        _regular_nonsymlink(args.config, "configuration")


def _paths_identical(left: pathlib.Path, right: pathlib.Path) -> bool:
    try:
        return os.path.samefile(left, right)
    except OSError:
        return left.resolve() == right.resolve()


def _command(args: argparse.Namespace) -> list[str]:
    command = [str(args.legacy_binary), "init", str(args.workspace_root)]
    if args.display_name is not None:
        command.extend(("--display-name", args.display_name))
    if args.config is not None:
        command.extend(("--config", str(args.config)))
    command.extend(("--catalogue", str(args.catalogue)))
    if args.human:
        command.append("--human")
    return command


def main(arguments: Sequence[str] | None = None) -> int:
    args = parse_args(arguments)
    try:
        _validate_inputs(args)
        inspected = inspect_catalogue(args.catalogue)
        if inspect_catalogue(args.catalogue) != inspected:
            raise FenceRefusal(
                f"catalogue state changed between validation and legacy process launch: {args.catalogue}"
            )
        if inspected is not None:
            if not any(
                _paths_identical(args.workspace_root, pathlib.Path(root))
                for root in inspected.registered_roots
            ):
                raise FenceRefusal(
                    f"existing catalogue is not registered for workspace root {args.workspace_root}",
                    catalogue_user_version=inspected.user_version,
                )
            print(
                json.dumps(
                    {
                        "ok": True,
                        "kind": "legacy_init_already_initialized",
                        "catalogue_user_version": inspected.user_version,
                        "legacy_process_launched": False,
                        "backup_authority": "caller_owned",
                    },
                    indent=2,
                )
            )
            return 0
        validate_preserved_binary(args.legacy_binary)
        return subprocess.run(_command(args), check=False).returncode
    except FenceRefusal as error:
        payload: dict[str, object] = {
            "ok": False,
            "kind": "legacy_init_fence",
            "error": str(error),
        }
        if error.catalogue_user_version is not None:
            payload["catalogue_user_version"] = error.catalogue_user_version
        print(json.dumps(payload, indent=2), file=sys.stderr)
        return FENCE_REFUSAL_EXIT


if __name__ == "__main__":
    raise SystemExit(main())
