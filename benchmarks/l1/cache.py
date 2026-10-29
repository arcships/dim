"""Verdict cache keyed by decision identity.

Two runs over the same MR must not disagree — a gate that flips on a re-run stops
being believed. Temperature 0 does not give that; the cache does.

A cache failure is never allowed to decide anything. A miss, a corrupt entry or an
unwritable directory all mean "recompute", never "invisible".
"""

from __future__ import annotations

import json
from dataclasses import replace
from pathlib import Path

from .decision import Axis, FileVerdict, L1Decision, ReviewerIdentity


class VerdictCache:
    def __init__(self, root: Path) -> None:
        self._root = root

    def _path(self, identity: str) -> Path:
        # Sharded: one flat directory grows into thousands of entries over a year
        # of MRs and becomes slow to list on some filesystems.
        return self._root / identity[:2] / f"{identity}.json"

    def get(self, identity: str) -> L1Decision | None:
        try:
            raw = self._path(identity).read_text()
            data = json.loads(raw)
            decision = _from_dict(data)
        except (OSError, json.JSONDecodeError, KeyError, TypeError, ValueError):
            return None
        # A stored entry whose identity does not re-derive is not this question's
        # answer, whatever the filename says.
        if decision.identity != identity:
            return None
        # Marked here rather than by the caller: this is the only place that knows
        # the verdict was read rather than reached.
        return replace(decision, cached=True)

    def put(self, decision: L1Decision) -> None:
        path = self._path(decision.identity)
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            temporary = path.with_suffix(".json.tmp")
            temporary.write_text(json.dumps(decision.to_dict(), indent=2))
            temporary.replace(path)
        except OSError:
            # Losing the cache costs one reviewer run. Failing the decision because
            # the cache is unwritable would cost correctness.
            return


def _from_dict(data: dict) -> L1Decision:
    reviewer = data["reviewer"]
    return L1Decision(
        base_sha=data["base_sha"],
        head_sha=data["head_sha"],
        verdict=data["verdict"],
        trigger=data["trigger"],
        reason=data["reason"],
        diff_digest=data["diff_digest"],
        policy_digest=data["policy_digest"],
        reviewer=ReviewerIdentity(
            image_digest=reviewer["image_digest"],
            model_snapshot=reviewer["model_snapshot"],
            prompt_digest=reviewer["prompt_digest"],
        ),
        visible_files=tuple(data.get("visible_files", ())),
        matched_rules=tuple(data.get("matched_rules", ())),
        changed_paths=tuple(data.get("changed_paths", ())),
        file_verdicts=tuple(
            FileVerdict(
                path=v["path"],
                verdict=v["verdict"],
                axes=tuple(Axis(a) for a in v.get("axes", ())),
                trace=v["trace"],
            )
            for v in data.get("file_verdicts", ())
        ),
        decided_at=data.get("decided_at", ""),
    )
