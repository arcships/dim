"""The three quarterly health metrics.

Each one measures a different layer's failure, and they only mean anything read
together. A low L1 false-negative rate achieved by judging everything visible is
not health — it is L1 doing nothing, which the visible rate is there to expose.

`regressed_commits` comes from outside the ledger: commits that L3 bisect or a
diffuse-regression backfill confirmed introduced a Primary regression. Without it
the two escape rates are undefined, not zero — a distinction the return type keeps.
"""

from __future__ import annotations

from dataclasses import dataclass

from .schema import MergeRecord, RunRecord


@dataclass(frozen=True)
class Metric:
    name: str
    numerator: int
    denominator: int

    @property
    def rate(self) -> float | None:
        """`None` when there is nothing to measure, never 0.0.

        Reporting an empty quarter as a perfect 0% would read as evidence of health
        where there is no evidence at all.
        """
        return None if self.denominator == 0 else self.numerator / self.denominator


@dataclass(frozen=True)
class HealthReport:
    l1_false_negative: Metric
    l1_visible_rate: Metric
    l2_escape: Metric

    def as_rows(self) -> list[tuple[str, str]]:
        rows = []
        for metric in (self.l1_false_negative, self.l1_visible_rate, self.l2_escape):
            rate = metric.rate
            shown = "n/a" if rate is None else f"{rate * 100:.1f}%"
            rows.append((metric.name, f"{shown} ({metric.numerator}/{metric.denominator})"))
        return rows


def health(
    merges: list[MergeRecord],
    runs: list[RunRecord],
    regressed_commits: set[str],
) -> HealthReport:
    """Join the two record types into the quarterly metrics.

    The join direction is run -> merge. Under batch triggering no merge record
    can say whether L2 passed for it: L2 answered for a window, and the run is
    the only thing that knows which merges that window held.
    """
    # Only a conclusive run can clear a merge. A daily k=1 screen that came back
    # clean is not evidence of anything, so a merge covered only by screens is
    # still waiting to be judged — counting it as passed is exactly the mistake
    # the k rule exists to prevent.
    cleared: set[str] = set()
    covered: set[str] = set()
    for run in runs:
        # `measured`, not `outcome != "invalid"`. A run harbor failed, scoring
        # crashed on, or that came back unscorable exercised nothing anyone can
        # read, so counting its merges as covered credits L1 for coverage that
        # does not exist. A run with no baseline is the opposite case and has to
        # keep covering: it measured fine, its window advanced, and no later run
        # will ever cover those merges again — dropping them would move the whole
        # pre-baseline period into the false-negative denominator as merges L1
        # supposedly waved through untested.
        if run.measured:
            covered.update(run.covers)
        if run.conclusive and run.outcome == "pass":
            cleared.update(run.covers)

    # A merge L1 judged invisible is not thereby untested: L2 runs against the
    # window head, so its code is in the build whenever anything else in the same
    # window triggered the run. Only a merge no run ever covered was actually
    # skipped on L1's word, and only those can hold L1 to account.
    skipped = [
        record for record in merges
        if record.l1_verdict == "invisible" and record.merge_sha not in covered
    ]
    skipped_regressions = [r for r in skipped if r.merge_sha in regressed_commits]

    passed = [r for r in merges if r.merge_sha in cleared]
    passed_regressions = [r for r in passed if r.merge_sha in regressed_commits]

    visible = [r for r in merges if r.l1_verdict == "visible"]

    return HealthReport(
        l1_false_negative=Metric(
            "L1 false negative", len(skipped_regressions), len(skipped)
        ),
        l1_visible_rate=Metric("L1 visible rate", len(visible), len(merges)),
        l2_escape=Metric("L2 escape", len(passed_regressions), len(passed)),
    )
