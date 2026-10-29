"""Canonical MR diff extraction.

L1 judges a diff, so the diff is part of the decision's identity: the same
`(base, head)` must always produce the same bytes, or the verdict cache silently
keys on noise. Rename detection, context size and colouring are therefore pinned
here rather than inherited from the caller's git config.

Anything that cannot be parsed into a complete file list — truncated input, an
unknown status letter, a rename without its pair — raises `DiffError`. The reducer
turns that into `visible`: an incomplete diff means an unjudged file, and an
unjudged file is exactly the case that must not be skipped.
"""

from __future__ import annotations

import hashlib
import subprocess
from dataclasses import dataclass
from enum import Enum


class DiffError(RuntimeError):
    """The diff could not be read or parsed into a complete file list."""


class ChangeType(str, Enum):
    ADDED = "added"
    MODIFIED = "modified"
    DELETED = "deleted"
    RENAMED = "renamed"
    COPIED = "copied"
    TYPE_CHANGED = "type_changed"


#: git --name-status letters. 'U' (unmerged) is absent on purpose: a conflicted
#: tree is not a reviewable diff and must fail rather than be judged.
_STATUS_LETTERS = {
    "A": ChangeType.ADDED,
    "M": ChangeType.MODIFIED,
    "D": ChangeType.DELETED,
    "R": ChangeType.RENAMED,
    "C": ChangeType.COPIED,
    "T": ChangeType.TYPE_CHANGED,
}

#: Pinned so the same revisions always yield the same bytes.
_DIFF_FLAGS = [
    "--no-ext-diff",
    "--no-color",
    "--find-renames",
    "--unified=3",
    "--no-textconv",
]


@dataclass(frozen=True)
class ChangedFile:
    path: str
    change_type: ChangeType
    old_path: str | None = None
    is_binary: bool = False


@dataclass(frozen=True)
class CanonicalDiff:
    base_sha: str
    head_sha: str
    files: tuple[ChangedFile, ...]
    patch: str
    digest: str

    @property
    def paths(self) -> tuple[str, ...]:
        """Every path the change touches, including the source side of renames."""
        out: list[str] = []
        for file in self.files:
            out.append(file.path)
            if file.old_path is not None:
                out.append(file.old_path)
        return tuple(out)


def _run_git(repo: str, args: list[str]) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", repo, *args],
            capture_output=True,
            check=True,
        )
    except FileNotFoundError as error:
        raise DiffError("git is not available") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode("utf-8", "replace").strip()
        raise DiffError(f"git {' '.join(args)} failed: {detail}") from error
    # Paths are bytes on disk; decode leniently so an exotic filename cannot make
    # the whole diff unreadable, and let the reviewer see the replacement chars.
    return result.stdout.decode("utf-8", "replace")


def _parse_name_status(raw: str) -> tuple[ChangedFile, ...]:
    """Parse `--name-status -z` output.

    The NUL-separated form is used because a newline in a path would otherwise
    split one entry into two and quietly drop a file from the review.
    """
    fields = [field for field in raw.split("\0") if field != ""]
    files: list[ChangedFile] = []
    index = 0
    while index < len(fields):
        status = fields[index]
        letter = status[0]
        change_type = _STATUS_LETTERS.get(letter)
        if change_type is None:
            raise DiffError(f"unknown diff status {status!r}")

        if change_type in (ChangeType.RENAMED, ChangeType.COPIED):
            if index + 2 >= len(fields):
                raise DiffError(f"truncated diff: {status!r} without both paths")
            files.append(
                ChangedFile(
                    path=fields[index + 2],
                    change_type=change_type,
                    old_path=fields[index + 1],
                )
            )
            index += 3
            continue

        if index + 1 >= len(fields):
            raise DiffError(f"truncated diff: {status!r} without a path")
        files.append(ChangedFile(path=fields[index + 1], change_type=change_type))
        index += 2
    return tuple(files)


def _binary_paths(patch: str) -> frozenset[str]:
    """Paths git reported as binary.

    A binary file has no reviewable text, so the reviewer is told rather than
    handed an empty hunk it might read as "no change".
    """
    found: set[str] = set()
    for line in patch.splitlines():
        if line.startswith("Binary files ") and line.endswith(" differ"):
            body = line[len("Binary files ") : -len(" differ")]
            for side in body.split(" and "):
                if side.startswith(("a/", "b/")):
                    found.add(side[2:])
                elif side != "/dev/null":
                    found.add(side)
    return frozenset(found)


def canonical_diff(repo: str, base_sha: str, head_sha: str) -> CanonicalDiff:
    """Read the diff between two exact revisions.

    Both revisions are exact SHAs by contract: a branch name would let the diff
    move under a cached verdict.
    """
    if not base_sha or not head_sha:
        raise DiffError("both base_sha and head_sha are required")

    name_status = _run_git(
        repo, ["diff", *_DIFF_FLAGS, "--name-status", "-z", base_sha, head_sha]
    )
    patch = _run_git(repo, ["diff", *_DIFF_FLAGS, base_sha, head_sha])

    files = _parse_name_status(name_status)
    if not files and patch.strip():
        raise DiffError("diff has content but no file list; refusing to judge")

    binary = _binary_paths(patch)
    files = tuple(
        ChangedFile(
            path=file.path,
            change_type=file.change_type,
            old_path=file.old_path,
            is_binary=file.path in binary or (file.old_path or "") in binary,
        )
        for file in files
    )

    digest = hashlib.sha256(patch.encode("utf-8", "replace")).hexdigest()
    return CanonicalDiff(
        base_sha=base_sha,
        head_sha=head_sha,
        files=files,
        patch=patch,
        digest=digest,
    )
