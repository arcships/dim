"""Completeness and tamper audit.

The ledger is only evidence if it is complete. A missing record makes a regression
unattributable, and an edited one makes the attribution wrong — which is worse,
because it looks authoritative. Both are detected here rather than trusted.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from .schema import MergeRecord, RunRecord, SchemaError
from .store import MERGES, RUNS


@dataclass(frozen=True)
class Finding:
    kind: str
    location: str
    detail: str


def _load(path: Path, root: Path, kind, expected_name) -> tuple[object | None, list[Finding]]:
    location = str(path.relative_to(root))
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        return None, [Finding("unreadable", location, str(error))]

    findings: list[Finding] = []
    stored_digest = data.get("digest")
    try:
        record = kind.from_dict(data)
    except (KeyError, TypeError, ValueError) as error:
        return None, [Finding("malformed", location, str(error))]

    if stored_digest != record.digest:
        findings.append(
            Finding("tampered", location,
                    f"stored digest {stored_digest!r} does not match content")
        )
    try:
        record.validate()
    except SchemaError as error:
        findings.append(Finding("invalid", location, str(error)))

    expected = expected_name(record)
    if path.name != expected:
        findings.append(
            Finding("misfiled", location, f"content says it should be {expected}")
        )
    return record, findings


def audit(root: Path) -> list[Finding]:
    """Audit both stores.

    The two subtrees are read with their own schemas rather than one guessed per
    file: a run record read as a merge record is `malformed`, which would bury a
    real finding under noise every time the ledger is audited.
    """
    findings: list[Finding] = []

    merges: dict[str, MergeRecord] = {}
    merge_root = root / MERGES
    if merge_root.is_dir():
        for path in sorted(merge_root.rglob("*.json")):
            record, found = _load(path, root, MergeRecord, lambda r: f"{r.merge_sha}.json")
            findings.extend(found)
            if record is None:
                continue
            shard = path.parent.name
            if shard != record.merge_sha[:2]:
                findings.append(Finding(
                    "misfiled", str(path.relative_to(root)),
                    f"shard {shard!r} does not match {record.merge_sha[:2]!r}",
                ))
            merges[record.merge_sha] = record

    run_root = root / RUNS
    if run_root.is_dir():
        for path in sorted(run_root.glob("*.json")):
            record, found = _load(path, root, RunRecord, lambda r: f"{r.run_id}.json")
            findings.extend(found)
            if record is None:
                continue
            # A run that answers for a merge the ledger never judged is the
            # ledger's own incompleteness, not the run's: without the L1 record
            # there is no way to tell a false negative from an escape later.
            unknown = [sha for sha in record.covers if sha not in merges]
            if unknown:
                findings.append(Finding(
                    "uncovered", f"{RUNS}/{record.run_id}",
                    f"{len(unknown)} covered merge(s) have no L1 record: "
                    + ", ".join(s[:12] for s in unknown[:3]),
                ))

    return findings
