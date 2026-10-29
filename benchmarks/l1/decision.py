"""The L1 decision record and its identity.

L1 never blocks. Its only output is whether L2 runs, so every failure mode here —
unreadable diff, broken policy, crashed reviewer, cache miss — resolves to
`visible`. Judging wrongly `visible` costs one eval run; judging wrongly
`invisible` ships a regression. The costs are not symmetric, so the code leans one
way on purpose.

Identity is what makes a cached verdict safe to reuse. It covers everything that
could change the answer: the diff, the prompt, the path policy, and the reviewer's
image and model snapshot. Leave one out and an edited prompt silently inherits the
old verdict.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Literal

Verdict = Literal["visible", "invisible"]
#: `oversize` is a decision, not an outage: a diff too large for the judge to
#: read cannot be traced, and trigger-policy resolves anything untraceable to
#: `visible`. It is kept apart from `fail_open` because that one is worth
#: retrying and this one never will be.
Trigger = Literal["path_rule", "agent", "fail_open", "oversize"]


class Axis(str, Enum):
    """The three ways a change reaches the model. `axes` is a list: one change can
    sit on more than one, and collapsing them loses the reason it triggered."""

    CONTENT = "content"
    TIMING = "timing"
    EFFECT = "effect"


@dataclass(frozen=True)
class FileVerdict:
    path: str
    verdict: Verdict
    axes: tuple[Axis, ...]
    trace: str


@dataclass(frozen=True)
class ReviewerIdentity:
    """What produced the verdict, pinned so a cached answer stays attributable."""

    image_digest: str
    model_snapshot: str
    prompt_digest: str


@dataclass(frozen=True)
class L1Decision:
    base_sha: str
    head_sha: str
    verdict: Verdict
    trigger: Trigger
    reason: str
    diff_digest: str
    policy_digest: str
    reviewer: ReviewerIdentity
    visible_files: tuple[str, ...] = ()
    matched_rules: tuple[str, ...] = ()
    file_verdicts: tuple[FileVerdict, ...] = ()
    #: Every path the diff touched, not only the visible ones. The ledger records
    #: it so a verdict can be read back against what was actually changed, and a
    #: batch view can show the size of a merge without re-running git.
    changed_paths: tuple[str, ...] = ()
    decided_at: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )
    #: Whether *this* lookup came from the cache. It describes the retrieval, not
    #: the decision, so it is deliberately absent from `identity` and `to_dict`:
    #: the same verdict is fresh when computed and cached when read back, and
    #: storing that distinction would make a decision's own record depend on how
    #: it happened to be fetched.
    cached: bool = False

    @property
    def identity(self) -> str:
        """Cache key. Excludes `decided_at` and the verdict itself — it identifies
        the question, not the answer."""
        material = json.dumps(
            {
                "base_sha": self.base_sha,
                "head_sha": self.head_sha,
                "diff_digest": self.diff_digest,
                "policy_digest": self.policy_digest,
                "prompt_digest": self.reviewer.prompt_digest,
                "reviewer_image": self.reviewer.image_digest,
                "reviewer_model": self.reviewer.model_snapshot,
            },
            sort_keys=True,
        )
        return hashlib.sha256(material.encode()).hexdigest()

    def to_dict(self) -> dict:
        return {
            "identity": self.identity,
            "base_sha": self.base_sha,
            "head_sha": self.head_sha,
            "verdict": self.verdict,
            "trigger": self.trigger,
            "reason": self.reason,
            "diff_digest": self.diff_digest,
            "policy_digest": self.policy_digest,
            "reviewer": {
                "image_digest": self.reviewer.image_digest,
                "model_snapshot": self.reviewer.model_snapshot,
                "prompt_digest": self.reviewer.prompt_digest,
            },
            "visible_files": list(self.visible_files),
            "matched_rules": list(self.matched_rules),
            "changed_paths": list(self.changed_paths),
            "file_verdicts": [
                {
                    "path": v.path,
                    "verdict": v.verdict,
                    "axes": [a.value for a in v.axes],
                    "trace": v.trace,
                }
                for v in self.file_verdicts
            ],
            "decided_at": self.decided_at,
        }
