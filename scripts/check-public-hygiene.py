#!/usr/bin/env python3
"""Fail when repository or Cargo package hygiene drifts from the public allowlist."""

from __future__ import annotations

import dataclasses
import io
import pathlib
import re
import subprocess
import sys
from collections.abc import Iterable, Iterator

ROOT = pathlib.Path(__file__).resolve().parents[1]

FORBIDDEN_PATH_RULES = (
    (
        "private workstream directory",
        re.compile(r"(^|/)(?:research|tasks)(?:/|$)", re.IGNORECASE),
    ),
    (
        "legacy planning marker",
        re.compile(r"pre[-_ ]*work", re.IGNORECASE),
    ),
    (
        "launch bundle marker",
        re.compile(r"launch[-_ ]+pack", re.IGNORECASE),
    ),
    (
        "nested internal evidence or report",
        re.compile(
            r"(^|/)[^/]*(?:internal[-_ ]*(?:evidence|report)|"
            r"(?:evidence|report)[-_ ]*internal)[^/]*(?:/|$)",
            re.IGNORECASE,
        ),
    ),
    (
        "orchestration report directory",
        re.compile(r"(^|/)(?:\.orca-reports|orchestration-reports)(?:/|$)", re.IGNORECASE),
    ),
    (
        "root internal report",
        re.compile(
            r"^[^/]*(?:ATLAS_|architectural[-_ ]*direction|internal|evidence|"
            r"report|ledger|pre[-_ ]*baseline|bench[-_ ]*run)"
            r"[^/]*\.(?:md|json|txt)$",
            re.IGNORECASE,
        ),
    ),
    (
        "internal evidence artifact",
        re.compile(
            r"(^|/)(?:ATLAS_.*(?:EVIDENCE|REPORT)|.*REQUIREMENT_LEDGER.*|"
            r".*PRE_BASELINE.*|.*BENCH_RUN.*)$",
            re.IGNORECASE,
        ),
    ),
)

PERSONAL_MARKERS = (("Pad" + "dy").encode(),)
PATH_TOKEN_SEPARATOR_RUN = rb"[\\/]+"
FORBIDDEN_CONTENT_RULES = (
    (
        "legacy planning marker",
        re.compile(rb"\bpre[\s_-]*work\b", re.IGNORECASE),
    ),
    (
        "launch bundle marker",
        re.compile(rb"\blaunch[\s_-]+pack\b", re.IGNORECASE),
    ),
    (
        "absolute Windows machine path",
        re.compile(
            rb"(?<![A-Za-z0-9+.-])[A-Z]:(?>\\{1,2}|/)"
            rb"(?!(?:Users(?>\\{1,2}|/)<user>|<[^>\r\n]+>)"
            rb"(?:(?>\\{1,2}|/)|$))",
            re.IGNORECASE,
        ),
    ),
    (
        "UNC user path",
        re.compile(
            rb"(?>\\{2,4})(?:[?.](?>\\{1,2})UNC(?>\\{1,2}))?"
            rb"[^\\/\r\n]+(?>\\{1,2}|/)(?:Users|home)(?>\\{1,2}|/)"
            rb"(?!<user>)[^\\/\s]+",
            re.IGNORECASE,
        ),
    ),
    (
        "Unix user home",
        re.compile(
            rb"/(?:home|Users)/(?!(?i:<user>))[^/\s]+(?:/|(?=\s|$))",
        ),
    ),
    (
        "macOS machine temp path",
        re.compile(rb"/private/var/folders(?:[\\/]|$)", re.IGNORECASE),
    ),
    (
        "local temporary path",
        re.compile(rb"(?<![A-Za-z0-9])/(?:tmp|var/tmp|private/tmp)(?:[\\/]|$)", re.IGNORECASE),
    ),
    (
        "OMP session path",
        re.compile(
            rb"\.omp" + PATH_TOKEN_SEPARATOR_RUN + rb"(?:agent|sessions?)"
            rb"(?="
            + PATH_TOKEN_SEPARATOR_RUN
            + rb"|[^\w./\x80-\xff-]|\.(?=[^\w\x80-\xff-]|$)|$)",
            re.IGNORECASE,
        ),
    ),
    (
        "Orca orchestration identifier",
        re.compile(
            rb"\b(?:term_[0-9a-f]{8}-[0-9a-f-]{20,}|"
            rb"(?:ctx|task|run|msg|delivery)_[0-9a-f]{12,}|"
            rb"dcap_[A-Za-z0-9_-]{20,})\b",
            re.IGNORECASE,
        ),
    ),
)

PACKAGE_EXACT = {
    ".cargo_vcs_info.json",
    "Cargo.lock",
    "Cargo.toml",
    "CONTRIBUTING.md",
    "Cargo.toml.orig",
    "LICENSE",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "OPERATIONS.md",
    "README.md",
    "SECURITY.md",
    "rust-toolchain.toml",
}
PACKAGE_PREFIXES = ("config/", "docs/", "migrations/", "schemas/", "src/", "tests/")
PACKAGE_BINS = {"bin/atlas.rs", "bin/atlas-bench.rs", "bin/atlas-mcp.rs"}


@dataclasses.dataclass(frozen=True)
class IndexEntry:
    mode: str
    oid: str
    stage: int
    path: str


class CommandError(RuntimeError):
    def __init__(self, args: tuple[str, ...], returncode: int, stderr: bytes):
        command = " ".join(args)
        detail = stderr.decode("utf-8", "replace").strip()
        super().__init__(f"{command} failed with exit code {returncode}: {detail}")
        self.returncode = returncode


def capture(
    *args: str,
    cwd: pathlib.Path = ROOT,
    input_data: bytes | None = None,
) -> bytes:
    result = subprocess.run(
        args,
        cwd=cwd,
        input=input_data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode:
        raise CommandError(args, result.returncode, result.stderr)
    return result.stdout


def normalize_path(path: str) -> str:
    return path.replace("\\", "/")


def forbidden_path_reason(path: str) -> str | None:
    normalized = normalize_path(path)
    for reason, pattern in FORBIDDEN_PATH_RULES:
        if pattern.search(normalized):
            return reason
    return None


def find_forbidden_content(data: bytes) -> str | None:
    for reason, pattern in FORBIDDEN_CONTENT_RULES:
        if pattern.search(data):
            return reason
    lowered = data.lower()
    if any(marker.lower() in lowered for marker in PERSONAL_MARKERS):
        return "personal username marker"
    return None


def parse_index_entries(raw: bytes) -> list[IndexEntry]:
    entries: list[IndexEntry] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        try:
            metadata, path_bytes = record.split(b"\t", 1)
            mode_bytes, oid_bytes, stage_bytes = metadata.split(b" ", 2)
            entry = IndexEntry(
                mode=mode_bytes.decode("ascii"),
                oid=oid_bytes.decode("ascii"),
                stage=int(stage_bytes),
                path=path_bytes.decode("utf-8", "surrogateescape"),
            )
        except (UnicodeDecodeError, ValueError) as error:
            raise ValueError(f"malformed git index entry: {record!r}") from error
        entries.append(entry)
    return entries


def tracked_entries(root: pathlib.Path = ROOT) -> list[IndexEntry]:
    return parse_index_entries(capture("git", "ls-files", "--stage", "-z", cwd=root))


def index_entry_failures(entries: Iterable[IndexEntry]) -> list[str]:
    return [
        f"unmerged index entry at stage {entry.stage}: {entry.path}"
        for entry in entries
        if entry.stage != 0
    ]


def read_index_blobs(
    entries: Iterable[IndexEntry],
    root: pathlib.Path = ROOT,
) -> Iterator[tuple[str, bytes]]:
    materialized = [entry for entry in entries if entry.stage == 0]
    requests = b"".join(entry.oid.encode("ascii") + b"\n" for entry in materialized)
    responses = io.BytesIO(capture("git", "cat-file", "--batch", cwd=root, input_data=requests))

    for entry in materialized:
        header = responses.readline().rstrip(b"\n")
        fields = header.split(b" ")
        if len(fields) != 3:
            raise ValueError(f"unexpected git cat-file response for {entry.path}: {header!r}")
        response_oid, object_type, size_bytes = fields
        if response_oid.decode("ascii") != entry.oid or object_type != b"blob":
            raise ValueError(
                f"index object for {entry.path} is not the expected blob {entry.oid}: "
                f"{header.decode('ascii', 'replace')}"
            )
        try:
            size = int(size_bytes)
        except ValueError as error:
            raise ValueError(f"invalid blob size for {entry.path}: {size_bytes!r}") from error
        data = responses.read(size)
        if len(data) != size or responses.read(1) != b"\n":
            raise ValueError(f"truncated git cat-file response for {entry.path}")
        yield entry.path, data

    if responses.read(1):
        raise ValueError("unexpected trailing git cat-file output")


def _git_differs(root: pathlib.Path, *args: str) -> bool:
    result = subprocess.run(
        ("git", *args),
        cwd=root,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    if result.returncode == 0:
        return False
    if result.returncode == 1:
        return True
    raise CommandError(("git", *args), result.returncode, result.stderr)


def tracked_tree_failures(root: pathlib.Path = ROOT) -> list[str]:
    failures: list[str] = []
    if _git_differs(root, "diff", "--quiet", "--"):
        failures.append("tracked worktree differs from the Git index")
    if _git_differs(root, "diff", "--cached", "--quiet", "--"):
        failures.append("Git index differs from HEAD")
    return failures


def package_path_allowed(path: str) -> bool:
    normalized = normalize_path(path)
    if forbidden_path_reason(normalized) is not None:
        return False
    return (
        normalized in PACKAGE_EXACT
        or normalized in PACKAGE_BINS
        or normalized.startswith(PACKAGE_PREFIXES)
    )


def main() -> int:
    failures: list[str] = []
    try:
        entries = tracked_entries()
        failures.extend(index_entry_failures(entries))

        for path, data in read_index_blobs(entries):
            reason = forbidden_path_reason(path)
            if reason is not None:
                failures.append(f"forbidden tracked path ({reason}): {path}")
            reason = find_forbidden_content(data)
            if reason is not None:
                failures.append(f"forbidden tracked content ({reason}): {path}")

        tree_failures = tracked_tree_failures()
        failures.extend(tree_failures)

        package_paths: list[str] = []
        if not tree_failures and not index_entry_failures(entries):
            package_paths = [
                normalize_path(path)
                for path in capture("cargo", "package", "--list").decode("utf-8").splitlines()
            ]
            for path in package_paths:
                reason = forbidden_path_reason(path)
                if reason is not None:
                    failures.append(f"forbidden Cargo package path ({reason}): {path}")
                elif not package_path_allowed(path):
                    failures.append(f"unexpected Cargo package path: {path}")
    except (CommandError, UnicodeDecodeError, ValueError) as error:
        failures.append(str(error))
        entries = []
        package_paths = []

    if failures:
        print("public hygiene check failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1

    print(f"public hygiene check passed: {len(entries)} tracked, {len(package_paths)} packaged")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
