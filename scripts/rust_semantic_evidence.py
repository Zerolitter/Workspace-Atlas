"""Inert validators for the fixed rust-analyzer 1.98.1 qualification profile.

All inputs are supplied by the caller. This module does not launch providers,
create captures, copy catalogues or establish execution/Truth/Serving results.
Snapshot inspection requires caller ownership and independently established
writer quiescence; path and link checks are not an OS access-control boundary.
"""

from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import stat


_HEX64 = re.compile(r"[0-9a-f]{64}")
_PROVIDER_VERSION = "1.98.1"


def _safe_path(path):
    try:
        path = Path(path)
    except (TypeError, ValueError) as error:
        raise ValueError("filesystem path required") from error
    if not path.is_absolute():
        raise ValueError("absolute path required")
    for ancestor in [*reversed(path.parents), path]:
        try:
            info = ancestor.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise ValueError("linked or reparse path")
    return path.resolve()


def _regular_file(path):
    path = _safe_path(path)
    try:
        info = path.lstat()
    except FileNotFoundError as error:
        raise ValueError("regular file required") from error
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("regular file required")
    return path, info


def _owned_regular_file(path):
    """Preserve the single-link guard for owned catalogue/source/capture inputs."""
    path, info = _regular_file(path)
    if info.st_nlink != 1:
        raise ValueError("owned input requires a single-link regular file")
    return path


def hash_retained_executable(path, *, expected_bytes, expected_sha256):
    """Check both byte pins without executing or changing a retained file.

    Regular retained executables may have hardlinks. This exception does not
    apply to owned catalogue/source/capture files. Matching bytes do not grant
    permission to execute, and callers must exclude concurrent modification.
    """
    if type(expected_bytes) is not int or expected_bytes < 0:
        raise ValueError("tool size binding differs")
    if not isinstance(expected_sha256, str) or not _HEX64.fullmatch(expected_sha256):
        raise ValueError("tool SHA256 binding differs")
    path, _ = _regular_file(path)
    data = path.read_bytes()
    if len(data) != expected_bytes:
        raise ValueError("tool size binding differs")
    observed = hashlib.sha256(data).hexdigest()
    if observed != expected_sha256:
        raise ValueError("tool SHA256 binding differs")
    return observed


def _json_hash(value):
    # Match ProviderDescriptor::seal's serde_json representation, not escaped
    # study JSON. Array order and the raw command string are part of the pin.
    data = json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False, allow_nan=False).encode("utf-8")
    return hashlib.sha256(data).hexdigest()


def rust_provider_identity(provider):
    """Return (key, descriptor SHA256, configuration SHA256) for this profile.

    The closed identity has name, version, executable_hash and command. The
    executable hash is an explicit pin; the command is kept verbatim without
    resolving a local executable. Other profiles require separate validation.
    """
    if not isinstance(provider, dict) or set(provider) != {
        "name", "version", "executable_hash", "command",
    }:
        raise ValueError("provider identity fields differ")
    if (
        provider["name"] != "rust-analyzer"
        or provider["version"] != _PROVIDER_VERSION
        or not isinstance(provider["executable_hash"], str)
        or not _HEX64.fullmatch(provider["executable_hash"])
        or not isinstance(provider["command"], str)
        or not provider["command"]
    ):
        raise ValueError("provider identity differs")
    configuration = {"qualification_condition": f"installed-provider-{_PROVIDER_VERSION}"}
    descriptor = {
        "protocol_version": "1.0.0", "name": provider["name"],
        "version": provider["version"], "tier": "semantic_index",
        "execution_kind": "external_process", "execution_scope": "project",
        "output_format": "scip", "deterministic": True,
        "languages": ["rust"], "artifact_classes": ["source", "test"],
        "project_markers": ["Cargo.toml"],
        "capabilities": ["symbols", "definitions", "references", "imports",
                         "implementations", "type_definitions", "coverage", "diagnostics"],
        "limitations": [
            "dynamic dispatch is not exhaustively resolved",
            "call-site classification requires structural composition",
            "exports are not distinguished from other definitions by SCIP's SymbolRole bitset",
        ],
        "invalidation": {"default_scope": "project", "provider_upgrade": "all_provider_output"},
        "executable": {"command": provider["command"],
                       "arguments": ["scip", ".", "--output", "{output_file}"],
                       "expected_output": "index.scip"},
    }
    fingerprint = _json_hash({
        "descriptor": descriptor, "executable_hash": provider["executable_hash"],
        "configuration": configuration, "protocol_version": "1.0.0",
        "output_format": "scip", "decoder_version": "scip-decoder-1.0.0",
        "mapping_policy_version": "rust-analyzer-scip-mapping-1.0.0",
    })
    return (f"{provider['name']}@{provider['version']}#{fingerprint[:16]}",
            fingerprint, _json_hash(configuration))


@contextmanager
def _completed_snapshot_connection(path, *, writer_quiescent):
    """Open only a caller-owned, completed self-contained snapshot.

    True is the caller's assertion, not writer detection. Immutable SQLite
    ignores WAL and cannot safely inspect a live catalogue. Nonempty WAL must
    be settled by a separately authorized WAL-aware workflow. No backup or
    repair occurs here; immutable mode avoids creating WAL/SHM sidecars.
    """
    if writer_quiescent is not True:
        raise ValueError("completed snapshot must be independently established as quiescent")
    path = _owned_regular_file(path)
    wal = _safe_path(Path(str(path) + "-wal"))
    if wal.exists():
        if not wal.is_file() or wal.stat().st_size != 0:
            raise ValueError("catalogue inspection requires a self-contained snapshot without nonempty WAL")
        _owned_regular_file(wal)
    connection = sqlite3.connect(path.as_uri() + "?mode=ro&immutable=1", uri=True)
    try:
        yield connection
    finally:
        connection.close()


def _one(connection, statement, parameters, label):
    rows = connection.execute(statement, parameters).fetchall()
    if len(rows) != 1:
        raise ValueError(f"{label} evidence must have exactly one row")
    return rows[0]


def inspect_provider_binding(catalogue, *, workspace_id, provider, writer_quiescent):
    """Validate selected descriptor/policy columns in a completed snapshot.

    Return (descriptor identity tuple, policy identity tuple), preserving the
    production SQL NULL executable_hash. The binary pin belongs to the sealed
    fingerprint, not that column. Require exactly one rust-analyzer descriptor
    in this narrow qualification snapshot and the selected workspace policy.
    This does not validate the entire schema, generation, execution, coverage,
    source facts or Serving. Ownership/quiescence remain caller preconditions.
    """
    if not isinstance(workspace_id, str) or not workspace_id:
        raise ValueError("workspace identity differs")
    provider_key, fingerprint, configuration_hash = rust_provider_identity(provider)
    try:
        with _completed_snapshot_connection(catalogue, writer_quiescent=writer_quiescent) as connection:
            descriptor = _one(
                connection,
                "SELECT provider_key,provider_version,executable_hash,descriptor_hash,"
                "executable_identity,execution_kind,execution_scope,output_format,provider_tier,protocol_version "
                "FROM provider_descriptor WHERE provider_name=?",
                (provider["name"],), "provider descriptor",
            )
            if descriptor != (provider_key, provider["version"], None, fingerprint,
                              provider["command"], "external_process", "project", "scip",
                              "semantic_index", "1.0.0"):
                raise ValueError("provider descriptor identity differs")
            policy = _one(
                connection,
                "SELECT enabled,required,configuration_hash,configuration_json "
                "FROM workspace_provider WHERE workspace_id=? AND provider_key=?",
                (workspace_id, provider_key), "provider policy",
            )
            try:
                configuration = json.loads(policy[3])
            except (TypeError, ValueError) as error:
                raise ValueError("provider policy configuration differs") from error
            if policy[:3] != (1, 1, configuration_hash) or configuration != {
                "qualification_condition": f"installed-provider-{_PROVIDER_VERSION}",
            }:
                raise ValueError("provider policy configuration differs")
            return descriptor[:4], policy[:3]
    except sqlite3.Error as error:
        raise ValueError("catalogue schema/query failure") from error
