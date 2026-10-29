"""Append-only ledger store.

Two flat stores, one per record type. Both are write-once: a merge commit's sha
never changes and a run id is unique, so there is no second write for the same
key to arbitrate and no head to go stale. The races the previous per-MR store
guarded against — a pipeline finishing after a newer one, two pipelines for the
same head — cannot arise when the key is immutable.

Write-once is still enforced, with `O_EXCL`, because the failure it catches is
different: two processes judging the same window at once, or a rerun quietly
rewriting what an earlier run concluded.

The one exception is a `fail_open` merge record, which a later verdict replaces
(`replace_fail_open`). It recorded that the judge gave no answer, so there is no
conclusion to rewrite.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

from .schema import MergeRecord, RunRecord, SchemaError

MERGES = "merges"
RUNS = "runs"


class LedgerError(RuntimeError):
    """The write was refused."""


class Ledger:
    def __init__(self, root: Path) -> None:
        self._root = root

    # --- layout ----------------------------------------------------------

    def merge_path(self, merge_sha: str) -> Path:
        # Sharded: a year of merges in one flat directory is slow to list on some
        # filesystems, and the ledger is never pruned.
        return self._root / MERGES / merge_sha[:2] / f"{merge_sha}.json"

    def run_path(self, run_id: str) -> Path:
        return self._root / RUNS / f"{run_id}.json"

    # --- reads -----------------------------------------------------------

    def merges(self) -> list[MergeRecord]:
        root = self._root / MERGES
        out = [
            MergeRecord.from_dict(json.loads(path.read_text()))
            for path in sorted(root.rglob("*.json"))
        ] if root.is_dir() else []
        return sorted(out, key=lambda r: (r.merged_at, r.merge_sha))

    def runs(self) -> list[RunRecord]:
        root = self._root / RUNS
        out = [
            RunRecord.from_dict(json.loads(path.read_text()))
            for path in sorted(root.glob("*.json"))
        ] if root.is_dir() else []
        return sorted(out, key=lambda r: (r.recorded_at, r.run_id))

    def merge(self, merge_sha: str) -> MergeRecord | None:
        path = self.merge_path(merge_sha)
        if not path.is_file():
            return None
        return MergeRecord.from_dict(json.loads(path.read_text()))

    # --- writes ----------------------------------------------------------

    def _write_once(self, path: Path, payload: dict, label: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            handle = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except FileExistsError as error:
            raise LedgerError(
                f"{label} already recorded; the ledger is append-only"
            ) from error
        with os.fdopen(handle, "w") as stream:
            json.dump(payload, stream, indent=2, sort_keys=True)

    def append_merge(self, record: MergeRecord) -> None:
        try:
            record.validate()
        except SchemaError as error:
            raise LedgerError(f"refusing to append an invalid record: {error}") from error
        self._write_once(
            self.merge_path(record.merge_sha), record.to_dict(),
            f"merge {record.merge_sha[:12]}",
        )

    def replace_fail_open(self, record: MergeRecord) -> None:
        """Replace a `fail_open` record with the verdict the judge reached later.

        Only a `fail_open` record can be replaced. Keeping the placeholder and
        filing the verdict as a dispute would leave the merge undecided in the
        ledger for good, so attribution could never learn what L1 thought of
        it. A recorded verdict is still never rewritten, and the replaced record
        stays in the ledger branch's history.
        """
        try:
            record.validate()
        except SchemaError as error:
            raise LedgerError(f"refusing to write an invalid record: {error}") from error
        existing = self.merge(record.merge_sha)
        if existing is None or existing.l1_trigger != "fail_open":
            raise LedgerError(
                f"merge {record.merge_sha[:12]} has no fail_open record to replace; "
                "a recorded verdict is never rewritten"
            )
        with self.merge_path(record.merge_sha).open("w") as stream:
            json.dump(record.to_dict(), stream, indent=2, sort_keys=True)

    def append_run(self, record: RunRecord) -> None:
        """Write a run, refusing one that claims merges the ledger never judged.

        A run may legitimately answer for commits with no L1 record at all —
        pushes straight to main — and those go in `unjudged`. A merge the judge
        failed on is a different case: it has a record, marked `fail_open`, and
        stays in `covers` because the run did test its code. Accepting an
        unrecorded commit in `covers` would let a gap read as coverage.
        """
        try:
            record.validate()
        except SchemaError as error:
            raise LedgerError(f"refusing to append an invalid record: {error}") from error

        unknown = [sha for sha in record.covers if self.merge(sha) is None]
        if unknown:
            raise LedgerError(
                f"run {record.run_id} covers {len(unknown)} merge(s) with no L1 "
                f"record: {', '.join(s[:12] for s in unknown[:3])}. Record the "
                "merges first, or list them as unjudged."
            )
        self._write_once(
            self.run_path(record.run_id), record.to_dict(), f"run {record.run_id}"
        )

    # --- joins -----------------------------------------------------------

    def covering_runs(self) -> dict[str, list[RunRecord]]:
        """Merge sha -> the runs that answered for it, oldest first."""
        out: dict[str, list[RunRecord]] = {}
        for run in self.runs():
            for sha in run.covers:
                out.setdefault(sha, []).append(run)
        return out
