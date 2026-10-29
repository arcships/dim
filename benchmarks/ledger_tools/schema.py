"""Ledger record schemas.

Two records, because the two halves of a judgement answer for different units:
**L1 judges one merge, L2 answers for a range of main.** Collapsing them into one
record would either copy a single run's conclusion into N merge records with
nothing saying they are the same run, or leave the batch conclusion homeless.

Both are content-addressed by digest, so a silently edited history fails the
audit instead of rewriting the past.

What is deliberately absent, now that batch is the only mode: attempts, a head
pointer, and exemptions. A merge commit's sha never changes, so there is no
second attempt for the same key and no race to lose; and a regression found in a
batch is reverted rather than waived (see trigger-policy.md).
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Literal

L1Verdict = Literal["visible", "invisible"]

#: `fail_open` is not a verdict. The judge was unreachable or its output could
#: not be validated, so the merge triggers L2 to stay safe — but recording it as
#: `agent` would hide an outage: every merge would look judged, and nothing would
#: say the judging never happened.
#:
#: `oversize` is a verdict, reached without asking: the diff is larger than the
#: judge can read, and an untraceable change resolves to visible. Separate from
#: `fail_open` because that one clears when the judge comes back and this one
#: does not, and separate from `agent` because no model saw it.
L1Trigger = Literal["path_rule", "agent", "fail_open", "oversize"]

#: `invalid` is not a synonym for `fail`. A run that could not be scored — missing
#: trials, unscored trials, a corpus pin that does not match, infra over budget —
#: says nothing about the change, and counting it as a failure would punish the
#: author for the harness.
#:
#: `screen` is the daily k=1 outcome: it ran, it did not collapse, and it is not
#: a verdict. Note what it claims that `invalid` does not: that a measurement
#: exists. A screen whose job could not be scored is not a screen — it is a run
#: with nothing in it, and `invalid` is the only thing it may be recorded as.
RunOutcome = Literal["pass", "fail", "invalid", "screen"]

#: Why a run is `invalid`, recorded because the four are not interchangeable and
#: only one of them is about the measurement being absent.
#:
#: `l2_failed` — harbor exited non-zero. `settle_failed` — harbor finished and
#: the scoring of its output raised. `unscoreable` — the output was read and does
#: not add up: trials missing, trials nothing graded, a task not run k times,
#: infra over budget. None of the three leaves a usable measurement behind.
#:
#: `no_reference` is the one that does. That run measured fine and has nothing to
#: compare against — no baseline for the corpus pin, or one at a k the criteria
#: cannot be evaluated across. It is invalid as a *judgement* while still being
#: evidence that the window's code was exercised, and `measured` is where that
#: difference is read back out. Collapsing it into the other three would put
#: every merge from the whole pre-baseline period back into the denominator of
#: L1's false-negative rate, as merges L1 supposedly waved through untested.
InvalidReason = Literal["l2_failed", "settle_failed", "unscoreable", "no_reference"]

RunType = Literal["daily", "checkpoint", "baseline"]

#: Below this a run may not conclude anything. trigger-policy.md bars a single
#: k=1 round from being a ledger judgement: on this corpus the same build measured
#: 53.3% at k=1 and 45.6% at k=3, and k=1 once reported a failure mode eliminated
#: that k=3 showed was still there.
#:
#: This is the one definition, and `RunRecord` is the one thing that enforces it —
#: a k<3 `pass` or `fail` cannot be constructed. Note what that leaves out:
#: nothing stops a *baseline* being frozen below it, and comparing against such a
#: baseline reports "arms not comparable" and records `pass`. The rule lives here
#: because it is about judgements, not about measurements.
CONCLUSIVE_MIN_K = 3

_SHA = re.compile(r"^[0-9a-f]{40}$")


class SchemaError(ValueError):
    """A record violates the ledger contract."""


def _require_sha(value: str, label: str) -> None:
    if not _SHA.match(value or ""):
        raise SchemaError(f"{label} must be a full 40-character sha, got {value!r}")


@dataclass(frozen=True)
class MergeRecord:
    """What L1 decided about one merge commit.

    Keyed by the merge commit rather than by the MR, because that is what a run's
    `covers` names and what a bisect lands on. `mr_iid` is carried when the forge
    put it in the commit body and is metadata, not identity: a merge with no
    trailer still has to be recorded.
    """

    merge_sha: str
    project_id: str
    subject: str
    merged_at: str
    l1_verdict: L1Verdict
    l1_trigger: L1Trigger
    l1_reason: str
    #: The decision identity from l1/decision.py: diff, policy, prompt and judge
    #: model. Two records claiming different verdicts for the same identity is a
    #: contradiction the audit can see.
    l1_identity: str
    mr_iid: str | None = None
    visible_files: tuple[str, ...] = ()
    recorded_at: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )

    @property
    def key(self) -> str:
        return self.merge_sha

    @property
    def digest(self) -> str:
        return hashlib.sha256(
            json.dumps(self._digest_material(), sort_keys=True).encode()
        ).hexdigest()

    def _digest_material(self) -> dict:
        return {
            "merge_sha": self.merge_sha,
            "project_id": self.project_id,
            "mr_iid": self.mr_iid,
            "subject": self.subject,
            "merged_at": self.merged_at,
            "l1_verdict": self.l1_verdict,
            "l1_trigger": self.l1_trigger,
            "l1_reason": self.l1_reason,
            "l1_identity": self.l1_identity,
            "visible_files": list(self.visible_files),
            "recorded_at": self.recorded_at,
        }

    def validate(self) -> None:
        _require_sha(self.merge_sha, "merge_sha")
        for name in ("project_id", "l1_identity"):
            if not getattr(self, name):
                raise SchemaError(f"{name} is required")
        if self.l1_verdict not in ("visible", "invisible"):
            raise SchemaError(f"unknown verdict {self.l1_verdict!r}")
        if self.l1_trigger not in ("path_rule", "agent", "fail_open", "oversize"):
            raise SchemaError(f"unknown trigger {self.l1_trigger!r}")
        if self.l1_trigger == "fail_open" and self.l1_verdict != "visible":
            raise SchemaError(
                "a fail-open merge must be recorded visible: an undecided merge "
                "may not be skipped"
            )
        if self.l1_verdict == "invisible" and self.visible_files:
            raise SchemaError("invisible merge lists visible files")

    def to_dict(self) -> dict:
        return {**self._digest_material(), "digest": self.digest}

    @classmethod
    def from_dict(cls, data: dict) -> "MergeRecord":
        return cls(
            merge_sha=data["merge_sha"],
            project_id=data["project_id"],
            subject=data.get("subject", ""),
            merged_at=data.get("merged_at", ""),
            l1_verdict=data["l1_verdict"],
            l1_trigger=data["l1_trigger"],
            l1_reason=data.get("l1_reason", ""),
            l1_identity=data["l1_identity"],
            mr_iid=data.get("mr_iid"),
            visible_files=tuple(data.get("visible_files", ())),
            recorded_at=data.get("recorded_at", ""),
        )


@dataclass(frozen=True)
class RunRecord:
    """What one L2 run answered for.

    `covers` is every merge in the window, not only the ones that triggered the
    run: L2 runs against the window head, so an invisible merge's code is in the
    build being tested just as much as a visible one's. Recording only the
    triggering merges would later let a regression in tested code be blamed on L1
    for not flagging it.
    """

    run_id: str
    run_type: RunType
    k: int
    corpus_pin: str
    subject_commit: str
    window_head: str
    outcome: RunOutcome
    window_base: str | None = None
    covers: tuple[str, ...] = ()
    #: Commits the run answered for that have no L1 record at all — pushes
    #: straight to main. A merge the judge never decided is *not* here: it has
    #: its own record whose `l1_trigger` is `fail_open`, and its code really was
    #: in the build. Each fact is recorded once, where it belongs.
    unjudged: tuple[str, ...] = ()
    breaches: tuple[str, ...] = ()
    #: Required when `outcome` is `invalid`, absent otherwise. See `InvalidReason`.
    invalid_reason: InvalidReason | None = None
    #: harbor's exit code, recorded only when it is not 0 — the evidence behind
    #: `l2_failed`, and the first thing anyone diagnosing one will want.
    l2_exit_code: int | None = None
    #: The exception that scoring raised, `Type: message`. The evidence behind
    #: `settle_failed`, and also what a fetch that could not bring the job back
    #: leaves: from the ledger's side those are the same situation, a run whose
    #: results cannot be read.
    settle_error: str | None = None
    #: Merges this run re-judged and disagreed with the recorded L1 record about
    #: — a different verdict, trigger, file list, or decision identity.
    #:
    #: The window is re-judged every run, so most merges are already recorded and
    #: a repeat is skipped. A repeat that *disagrees* is a fact about the ledger's
    #: own reliability, and it has to survive somewhere the 30-day artifacts do
    #: not. Recorded rather than refused: the decision identity covers the policy
    #: file's digest and the judge's model, so one ordinary policy edit makes
    #: every merge still in the window drift at once. Refusing would wedge the
    #: pipeline on a legitimate change, with a write-once store and no way back.
    disputed: tuple[str, ...] = ()
    #: True when the harness that produced this run had uncommitted changes.
    #: Only reachable deliberately, through `--allow-dirty`, and recorded so the
    #: ledger says which of its records were measured with a ruler nobody can
    #: reconstruct. A baseline may not be one at all — see `validate`.
    runner_dirty: bool = False
    #: 0.0 on a run that never produced a report to compute it from. Read it
    #: against `measured`: on an unmeasured run it is a default, not an
    #: observation of a healthy infrastructure.
    infra_rate: float = 0.0
    recorded_at: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )

    @property
    def key(self) -> str:
        return self.run_id

    @property
    def conclusive(self) -> bool:
        """Derived, never supplied. A caller that could set this could set it
        wrong, and the whole point is that k decides."""
        return self.k >= CONCLUSIVE_MIN_K

    @property
    def measured(self) -> bool:
        """Whether this run exercised the window's code and got numbers out.

        Not the same question as `conclusive`, and not the same as `outcome !=
        "invalid"`. A k=1 screen is measured and inconclusive; a run with no
        baseline is measured and invalid. Only the three failure reasons leave
        nothing behind.

        This is what licenses a run to *cover* the merges in its window. Derived,
        never supplied, for the same reason `conclusive` is.
        """
        return self.outcome != "invalid" or self.invalid_reason == "no_reference"

    @property
    def digest(self) -> str:
        return hashlib.sha256(
            json.dumps(self._digest_material(), sort_keys=True).encode()
        ).hexdigest()

    def _digest_material(self) -> dict:
        material = {
            "run_id": self.run_id,
            "run_type": self.run_type,
            "k": self.k,
            "conclusive": self.conclusive,
            "corpus_pin": self.corpus_pin,
            "subject_commit": self.subject_commit,
            "window_base": self.window_base,
            "window_head": self.window_head,
            "covers": list(self.covers),
            "unjudged": list(self.unjudged),
            "outcome": self.outcome,
            "breaches": list(self.breaches),
            "infra_rate": self.infra_rate,
            "recorded_at": self.recorded_at,
        }
        # Only when set. An always-present key would change the digest of every
        # record already written — including the `pass` and `screen` records
        # these three fields say nothing about — and the audit re-derives the
        # digest to detect tampering, so each of them would start reading as
        # edited. The store is write-once, so there is no fixing that afterwards.
        #
        # `measured` is deliberately not here: it is derived from `outcome` and
        # `invalid_reason`, which are, so covering it adds no integrity.
        for name, value in (("invalid_reason", self.invalid_reason),
                            ("l2_exit_code", self.l2_exit_code),
                            ("settle_error", self.settle_error),
                            ("runner_dirty", self.runner_dirty or None),
                            ("disputed", list(self.disputed) or None)):
            if value is not None:
                material[name] = value
        return material

    def validate(self) -> None:
        if not self.run_id:
            raise SchemaError("run_id is required")
        if self.run_type not in ("daily", "checkpoint", "baseline"):
            raise SchemaError(f"unknown run type {self.run_type!r}")
        if self.k < 1:
            raise SchemaError("k is 1-based")
        if not self.corpus_pin:
            raise SchemaError("corpus_pin is required")
        _require_sha(self.subject_commit, "subject_commit")
        _require_sha(self.window_head, "window_head")
        if self.window_base is not None:
            _require_sha(self.window_base, "window_base")
        elif self.run_type != "baseline":
            raise SchemaError(
                f"a {self.run_type} run answers for a range and needs window_base"
            )
        for sha in (*self.covers, *self.unjudged):
            _require_sha(sha, "covered commit")
        for sha in self.disputed:
            _require_sha(sha, "disputed commit")
        # A merge this run never answered for cannot be one it disagreed about.
        if set(self.disputed) - set(self.covers):
            raise SchemaError("a disputed merge must be one this run covers")
        if set(self.covers) & set(self.unjudged):
            raise SchemaError("a commit cannot be both covered and unjudged")

        if self.outcome not in ("pass", "fail", "invalid", "screen"):
            raise SchemaError(f"unknown outcome {self.outcome!r}")
        # `invalid` is exempt, and the exemption is the point: the k rule is about
        # judgements, not about measurements. `invalid` decides nothing — it says
        # this run produced no measurement to decide from — so barring it below
        # k=3 does not protect the rule, it only leaves a run with nothing in it
        # nowhere to go but `screen`, which claims a measurement happened.
        if not self.conclusive and self.outcome not in ("screen", "invalid"):
            raise SchemaError(
                f"a k={self.k} run cannot conclude {self.outcome!r}: "
                f"trigger-policy bars a single round below k={CONCLUSIVE_MIN_K} "
                "from deciding anything"
            )
        if self.conclusive and self.outcome == "screen":
            raise SchemaError(
                f"a k={self.k} run is conclusive; `screen` would discard a result "
                "that was paid for"
            )
        if self.outcome == "fail" and not self.breaches:
            raise SchemaError("a failing run must name the criteria it breached")
        if self.outcome != "fail" and self.breaches:
            raise SchemaError(f"breaches recorded on a {self.outcome!r} run")

        # Required, not optional. `invalid` covers both "nothing was measured"
        # and "it was measured and there is nothing to compare it to", and
        # `measured` — which decides whether this run covers its merges — cannot
        # be derived without knowing which. A record that does not say is a
        # record nobody can read later, and the store is append-only.
        if self.outcome == "invalid":
            if self.invalid_reason is None:
                raise SchemaError("an invalid run must say why it is invalid")
            if self.invalid_reason not in (
                "l2_failed", "settle_failed", "unscoreable", "no_reference"
            ):
                raise SchemaError(f"unknown invalid_reason {self.invalid_reason!r}")
        elif self.invalid_reason is not None:
            raise SchemaError(
                f"invalid_reason {self.invalid_reason!r} on a {self.outcome!r} run"
            )
        if self.invalid_reason == "l2_failed" and not self.l2_exit_code:
            raise SchemaError("l2_failed must record the non-zero exit code")
        if self.invalid_reason == "settle_failed" and not self.settle_error:
            raise SchemaError("settle_failed must record what scoring raised")

        # A baseline is the reference every later comparison is read against.
        # Freezing one from a harness with uncommitted changes defines the ruler
        # with code that exists on one machine and in no commit, and nothing
        # downstream could ever be traced back to it. `--allow-dirty` covers
        # experiments; it does not reach this.
        if self.runner_dirty and self.run_type == "baseline":
            raise SchemaError(
                "a baseline cannot come from a harness with uncommitted changes"
            )

        if not 0.0 <= self.infra_rate <= 1.0:
            raise SchemaError(f"infra_rate {self.infra_rate} is not a rate")

    def to_dict(self) -> dict:
        return {**self._digest_material(), "digest": self.digest}

    @classmethod
    def from_dict(cls, data: dict) -> "RunRecord":
        return cls(
            run_id=data["run_id"],
            run_type=data["run_type"],
            k=int(data["k"]),
            corpus_pin=data["corpus_pin"],
            subject_commit=data["subject_commit"],
            window_head=data["window_head"],
            outcome=data["outcome"],
            window_base=data.get("window_base"),
            covers=tuple(data.get("covers", ())),
            unjudged=tuple(data.get("unjudged", ())),
            breaches=tuple(data.get("breaches", ())),
            invalid_reason=data.get("invalid_reason"),
            l2_exit_code=data.get("l2_exit_code"),
            settle_error=data.get("settle_error"),
            disputed=tuple(data.get("disputed", ())),
            runner_dirty=bool(data.get("runner_dirty", False)),
            infra_rate=float(data.get("infra_rate", 0.0)),
            recorded_at=data.get("recorded_at", ""),
        )
