#!/usr/bin/env python3
"""Audit and explicitly remove only known disposable development artifacts."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePath
import re
import stat
import sys

if os.name == "nt":
    import ctypes
    import msvcrt
    from ctypes import wintypes

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _kernel32.CreateFileW.argtypes = (
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    )
    _kernel32.CreateFileW.restype = wintypes.HANDLE
    _kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    _kernel32.CloseHandle.restype = wintypes.BOOL
    _kernel32.GetFinalPathNameByHandleW.argtypes = (
        wintypes.HANDLE,
        wintypes.LPWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
    )
    _kernel32.GetFinalPathNameByHandleW.restype = wintypes.DWORD
    _kernel32.SetFileInformationByHandle.argtypes = (
        wintypes.HANDLE,
        ctypes.c_int,
        wintypes.LPVOID,
        wintypes.DWORD,
    )
    _kernel32.SetFileInformationByHandle.restype = wintypes.BOOL

    class _FileDispositionInfo(ctypes.Structure):
        _fields_ = [("DeleteFile", wintypes.BOOL)]

SCHEMA_VERSION = "1.0.0"
FIXED_CANDIDATES = (
    ("target/debug", "cargo_build_output"),
    ("target/release", "cargo_build_output"),
    ("target/doc", "cargo_build_output"),
    ("target/.rustc_info.json", "cargo_build_output"),
    ("_generated_fixture", "generated_fixture"),
    ("atlas_fixture", "generated_fixture"),
    (".atlas-provider-tmp", "provider_temp_output"),
    ("provider-output", "provider_temp_output"),
)
KNOWN_TARGET_NAMES = frozenset({"debug", "release", "doc", ".rustc_info.json", "CACHEDIR.TAG"})
PROTECTED_MARKERS = (
    "manifest", "raw", "observation", "accepted-outcome", "accepted_outcome",
    "evidence", "portable-bundle", "portable_bundle", "results.csv", "summary",
    "attestation", "preflight", "result", "report", "execution-plan",
    "label-state", "label_state", "metric", "measurement", "outcome", "sample",
    "provenance",
)
RETAINED_VERDICT_STEM = re.compile(r"(?i)verdict(?:[-_](?:file|outcome))?\Z")
PRESERVE_FILE = ".atlas-preserve"
SCRATCH_NAME = re.compile(
    r"(?:\.local-atlas-scratch|\.atlas-benchmark-scratch|"
    r"atlas-benchmark-scratch(?:-[A-Za-z0-9._-]+)?)\Z"
)


class AuditError(RuntimeError):
    """The requested workspace boundary cannot be inspected safely."""


def _relative_text(path: Path, workspace: Path) -> str:
    try:
        return path.relative_to(workspace).as_posix()
    except ValueError:
        return os.path.relpath(path, workspace).replace("\\", "/")


def _is_reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & 0x400)


def _identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def _metadata(path: Path) -> os.stat_result:
    return path.lstat()


def _is_link_or_reparse(metadata: os.stat_result) -> bool:
    return stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata)


def _tree_bytes(path: Path) -> int:
    try:
        metadata = _metadata(path)
    except OSError:
        return 0
    if _is_link_or_reparse(metadata):
        return 0
    if stat.S_ISREG(metadata.st_mode):
        return metadata.st_size
    if not stat.S_ISDIR(metadata.st_mode):
        return 0
    total = 0
    try:
        children = sorted(path.iterdir(), key=lambda item: item.name)
    except OSError:
        return 0
    for child in children:
        total += _tree_bytes(child)
    return total


def _item(path: str, artifact_class: str, byte_count: int, reason: str | None = None) -> dict[str, object]:
    value: dict[str, object] = {"bytes": byte_count, "class": artifact_class, "path": path}
    if reason is not None:
        value["reason"] = reason
    return value


def _protected_name(name: str) -> bool:
    lowered = name.lower()
    return name == PRESERVE_FILE or any(marker in lowered for marker in PROTECTED_MARKERS)



def _contains_preserve_marker(directory: Path) -> bool:
    marker = directory / PRESERVE_FILE
    try:
        _metadata(marker)
    except FileNotFoundError:
        return False
    except OSError:
        return True
    return True


def _is_retained_verdict_name(name: str) -> bool:
    lowered = name.lower()
    if not lowered.endswith(".json"):
        return False
    stem = lowered[:-len(".json")]
    return bool(RETAINED_VERDICT_STEM.fullmatch(stem))


def _contains_retained_verdict(directory: Path) -> bool:
    try:
        children = sorted(directory.iterdir(), key=lambda child: child.name)
    except OSError:
        return True
    for child in children:
        try:
            metadata = _metadata(child)
        except OSError:
            return True
        if _is_link_or_reparse(metadata):
            return True
        if (
            stat.S_ISREG(metadata.st_mode)
            and _is_retained_verdict_name(child.name)
        ):
            return True
        if (
            stat.S_ISDIR(metadata.st_mode)
            and _contains_retained_verdict(child)
        ):
            return True
    return False


def _collect_candidate(
    path: Path,
    workspace: Path,
    artifact_class: str,
    proposed: list[dict[str, object]],
    protected: list[dict[str, object]],
    unknown: list[dict[str, object]],
    identities: dict[str, tuple[int, int, int, int]],
    protected_ancestor: bool = False,
) -> None:
    relative = _relative_text(path, workspace)
    try:
        metadata = _metadata(path)
    except FileNotFoundError:
        return
    except OSError:
        unknown.append(_item(relative, "unknown", 0, "inspection_failed"))
        return
    if _is_link_or_reparse(metadata):
        unknown.append(_item(relative, "unknown", 0, "link_or_reparse_point"))
        return
    if stat.S_ISREG(metadata.st_mode):
        is_protected = (
            protected_ancestor
            or _protected_name(path.name)
            or (
                artifact_class == "benchmark_scratch"
                and path.suffix.lower() in {".json", ".ndjson", ".csv"}
            )
        )
        target = protected if is_protected else proposed
        target.append(_item(relative, artifact_class, metadata.st_size))
        if not is_protected:
            identities[relative] = _identity(metadata)
        return
    if not stat.S_ISDIR(metadata.st_mode):
        unknown.append(_item(relative, "unknown", 0, "unsupported_file_type"))
        return
    retained_root = artifact_class in {"generated_fixture", "benchmark_scratch"}
    preserved = (
        protected_ancestor
        or _contains_preserve_marker(path)
        or (retained_root and _contains_retained_verdict(path))
    )
    try:
        children = sorted(path.iterdir(), key=lambda child: child.name)
    except OSError:
        unknown.append(_item(relative, "unknown", 0, "inspection_failed"))
        return
    for child in children:
        _collect_candidate(
            child, workspace, artifact_class, proposed, protected, unknown,
            identities, preserved
        )


def _safe_scratch(raw: str, workspace: Path) -> tuple[Path | None, dict[str, object] | None]:
    supplied = Path(raw)
    if (
        supplied.is_absolute()
        or len(PurePath(raw).parts) != 1
        or not SCRATCH_NAME.fullmatch(supplied.name)
    ):
        return None, _item(
            raw.replace("\\", "/"), "unknown", 0, "unsafe_scratch_identity"
        )
    return workspace / supplied, None


def _ambiguous_ancestor(path: Path, workspace: Path) -> Path | None:
    current = workspace
    for part in path.relative_to(workspace).parts[:-1]:
        current /= part
        try:
            metadata = _metadata(current)
        except FileNotFoundError:
            return None
        except OSError:
            return current
        if _is_link_or_reparse(metadata) or not stat.S_ISDIR(metadata.st_mode):
            return current
    return None


def _check_identity_chain(
    path: Path,
    workspace: Path,
) -> None:
    try:
        relative = path.relative_to(workspace)
    except ValueError as error:
        raise AuditError("candidate escapes workspace") from error
    current = workspace
    for part in relative.parts[:-1]:
        current /= part
        try:
            metadata = _metadata(current)
        except FileNotFoundError:
            raise AuditError("candidate ancestry was removed before deletion")
        except OSError as error:
            raise AuditError("candidate ancestry became ambiguous before deletion") from error
        if _is_link_or_reparse(metadata) or not stat.S_ISDIR(metadata.st_mode):
            raise AuditError("candidate ancestry became a link before deletion")


def _safe_unlink_identity_bound(
    path: Path,
    workspace: Path,
    expected: tuple[int, int, int, int],
) -> None:
    _check_identity_chain(path, workspace)
    if os.name != "nt":
        raise AuditError("identity-bound deletion is unavailable on this platform")
    handle = _kernel32.CreateFileW(
        str(path),
        0x00010000 | 0x00000080,
        0x00000001 | 0x00000002,
        None,
        3,
        0x00200000,
        None,
    )
    if handle == wintypes.HANDLE(-1).value:
        error = ctypes.get_last_error()
        if error in (2, 3):
            return
        raise AuditError("candidate could not be opened for identity-bound deletion")
    fd = -1
    try:
        fd = msvcrt.open_osfhandle(handle, os.O_RDONLY)
        handle = None
        try:
            metadata = os.fstat(fd)
            actual = _identity(metadata)
        except OSError as error:
            raise AuditError(
                "candidate identity changed before deletion"
            ) from error
        if (
            _is_link_or_reparse(metadata)
            or not stat.S_ISREG(metadata.st_mode)
            or actual != expected
        ):
            raise AuditError("candidate identity changed before deletion")
        native_handle = msvcrt.get_osfhandle(fd)
        required = _kernel32.GetFinalPathNameByHandleW(
            native_handle, None, 0, 0
        )
        if not required:
            raise AuditError("candidate physical identity is unavailable")
        buffer = ctypes.create_unicode_buffer(required + 1)
        written = _kernel32.GetFinalPathNameByHandleW(
            native_handle, buffer, len(buffer), 0
        )
        if not written or written >= len(buffer):
            raise AuditError("candidate physical identity is unavailable")
        final_path = buffer.value
        if final_path.startswith("\\\\?\\"):
            final_path = final_path[4:]
        workspace_path = os.path.normcase(str(workspace.resolve(strict=True)))
        opened_path = os.path.normcase(final_path)
        try:
            contained = os.path.commonpath((workspace_path, opened_path))
        except ValueError as error:
            raise AuditError("candidate physical path escapes workspace") from error
        if contained != workspace_path:
            raise AuditError("candidate physical path escapes workspace")
        disposition = _FileDispositionInfo(True)
        if not _kernel32.SetFileInformationByHandle(
            native_handle,
            4,
            ctypes.byref(disposition),
            ctypes.sizeof(disposition),
        ):
            raise AuditError("candidate could not be identity-bound deleted")
    finally:
        if fd >= 0:
            os.close(fd)
        elif handle:
            _kernel32.CloseHandle(handle)


def audit(workspace: Path, scratches: list[str], apply: bool) -> dict[str, object]:
    workspace = workspace.absolute()
    try:
        root_metadata = _metadata(workspace)
    except OSError as error:
        raise AuditError("workspace must be an existing inspectable directory") from error
    if not stat.S_ISDIR(root_metadata.st_mode) or _is_link_or_reparse(root_metadata):
        raise AuditError("workspace must be a non-link, non-reparse directory")

    candidates = [(workspace / relative, artifact_class) for relative, artifact_class in FIXED_CANDIDATES]
    unknown: list[dict[str, object]] = []
    for raw in scratches:
        candidate, refusal = _safe_scratch(raw, workspace)
        if refusal is not None:
            unknown.append(refusal)
        elif candidate is not None:
            candidates.append((candidate, "benchmark_scratch"))

    target = workspace / "target"
    try:
        target_metadata = _metadata(target)
    except FileNotFoundError:
        target_metadata = None
    except OSError:
        unknown.append(_item("target", "unknown", 0, "inspection_failed"))
        target_metadata = None
    if target_metadata is not None:
        if _is_link_or_reparse(target_metadata) or not stat.S_ISDIR(target_metadata.st_mode):
            unknown.append(_item("target", "unknown", 0, "link_or_reparse_point"))
        else:
            try:
                for child in sorted(target.iterdir(), key=lambda value: value.name):
                    if child.name not in KNOWN_TARGET_NAMES:
                        unknown.append(_item(_relative_text(child, workspace), "unknown", _tree_bytes(child), "unclassified_target_content"))
            except OSError:
                unknown.append(_item("target", "unknown", 0, "inspection_failed"))
    proposed: list[dict[str, object]] = []
    protected: list[dict[str, object]] = []
    identities: dict[str, tuple[int, int, int, int]] = {}
    for path, artifact_class in candidates:
        ambiguous = _ambiguous_ancestor(path, workspace)
        if ambiguous is not None:
            unknown.append(_item(
                _relative_text(ambiguous, workspace), "unknown", 0,
                "link_or_reparse_point"
            ))
            continue
        _collect_candidate(
            path, workspace, artifact_class, proposed, protected, unknown,
            identities
        )

    key = lambda item: str(item["path"])
    proposed.sort(key=key)
    protected.sort(key=key)
    unique_unknown = {
        (str(item["path"]), str(item.get("reason", ""))): item for item in unknown
    }
    unknown = sorted(unique_unknown.values(), key=key)
    deleted: list[dict[str, object]] = []
    if apply:
        for item in proposed:
            path = workspace / str(item["path"])
            try:
                _safe_unlink_identity_bound(
                    path, workspace, identities[str(item["path"])]
                )
                deleted.append(item)
            except FileNotFoundError:
                continue
            except (OSError, AuditError) as error:
                reason = "candidate_changed_before_deletion" if "candidate" in str(error) else "delete_failed"
                unknown.append(_item(
                    str(item["path"]), "unknown", 0, reason,
                ))
        unknown.sort(key=key)

    return {
        "deleted": deleted,
        "mode": "apply" if apply else "dry_run",
        "proposed": proposed,
        "protected": protected,
        "schema_version": SCHEMA_VERSION,
        "unknown": unknown,
    }


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--benchmark-scratch", action="append", default=[])
    parser.add_argument("--apply", action="store_true", help="delete proposed artifacts")
    options = parser.parse_args(arguments)
    try:
        report = audit(options.workspace, options.benchmark_scratch, options.apply)
    except AuditError as error:
        print(json.dumps({"error": str(error), "kind": "local_artifact_cleanup"}, sort_keys=True), file=sys.stderr)
        return 2
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
