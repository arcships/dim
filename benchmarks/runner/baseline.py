#!/usr/bin/env python3
"""Baselines and the paired comparison against them.

A baseline is one run's per-task scores frozen under the corpus pin it was taken
on, plus everything needed to tell later whether a second run may be compared to
it at all. The comparison is paired per task, never per repetition: harbor
numbers no repetitions, and task difficulty — which dwarfs the effect being
measured — cancels only when the same task is compared to itself.

Two things this file deliberately does not do.

**It does not decide whether to block.** It reports each criterion from
trigger-policy.md with the threshold the document gives and whether the document
treats it as blocking. The `net degraded` criterion is reported as measured and
marked non-blocking, because its initial threshold was falsified on this corpus:
a 28% (k=1) to 32% (k=3) false-block rate on unchanged code. Raising k makes it
worse, not better, so nothing here can fix it — only re-calibration can.

**It does not price tokens.** The cost metric is a token count
(trigger-policy.md, Metrics). A dollar figure would need a price table pinned
per epoch, and the one number harbor reports carries `cost_source:
reported_by_dimcode` — the subject of the experiment pricing itself.
"""

from __future__ import annotations

import hashlib
import json
import random
import statistics
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

from results import JobReport, TaskScore, Trial  # noqa: E402

SCHEMA_VERSION = 1

#: Pinned so two comparisons of the same data give the same interval. The epoch
#: includes this version and seed for exactly that reason.
BOOTSTRAP_ALGORITHM = "paired-task-delta-v1"
BOOTSTRAP_SEED = 20260914
BOOTSTRAP_RESAMPLES = 10_000

#: trigger-policy.md, L2 门禁.
MAX_MEDIAN_INCREASE = 0.15
MIN_PASS_RATE_CI_LOWER = -0.05
MAX_TURN_EXHAUSTED_INCREASE = 1
MAX_NET_DEGRADED = 1

#: The CLI's own wording for hitting the turn limit is not verified here, so the
#: full end-reason distribution is reported alongside the count: a constant that
#: never matches anything shows up as a distribution with some other reason
#: rising instead of silently reporting zero.
TURN_EXHAUSTED_REASON = "turn_exhausted"


class BaselineError(RuntimeError):
    """A baseline could not be built, read, or compared."""


def _task_entry(task: TaskScore) -> dict:
    return {
        "task_checksum": task.trials[0].task_checksum if task.trials else "",
        "k": task.k,
        "passes": task.passes,
        "pass_score": task.pass_score,
        "fully_scored": task.fully_scored,
        "trials": [
            {
                "trial_name": trial.trial_name,
                "reward": trial.reward,
                "exception": trial.exception,
                "input_tokens": trial.input_tokens,
                "output_tokens": trial.output_tokens,
                "cache_tokens": trial.cache_tokens,
                "uncached_input_tokens": trial.uncached_input_tokens,
                "token_cost": trial.token_cost,
                "turns": trial.turns,
                "usage_complete": trial.usage_complete,
                "end_reason": trial.end_reason,
                "agent_seconds": trial.agent_seconds,
            }
            for trial in task.trials
        ],
    }


#: The fields `model_identity.ModelIdentity.from_dict` reads. Named here because
#: a baseline that lacks any of them is one the identity guard cannot use, and
#: the place to find that out is when the reference is frozen.
_IDENTITY_FIELDS = ("provider", "model", "model_reported")


def _require_model_identity(model: dict | None) -> None:
    """Refuse a reference that cannot say which model produced it.

    A baseline is what every later comparison is read against, and the one thing
    that makes those comparisons mean anything is that the same model answered.
    `run_l2` enforces that before it runs, by comparing the endpoint against this
    record — so a baseline with no identity does not merely lose provenance, it
    disarms the guard: the check reached `ModelIdentity.from_dict(None)` and died
    of a TypeError, on the eval machine, months after the fact.

    It could get there because the identity is captured remotely and rsync'd back
    beside the results. A transient fetch failure, or `DIM_EVAL_SKIP_MODEL_CHECK=1`
    in the eval machine's environment, left `model` as None — and the baseline was
    written anyway, then refused to be overwritten, because `write` is write-once.
    Recovering meant a commit deleting the reference.
    """
    if not isinstance(model, dict):
        raise BaselineError(
            "refusing to freeze a baseline with no model identity: the reference "
            "could not say which model produced it, and every later run's "
            "identity guard would have nothing to compare the endpoint against. "
            "The identity is captured by run_l2 on the eval machine and fetched "
            "beside the results; it is absent when DIM_EVAL_SKIP_MODEL_CHECK=1, "
            "or when <run-id>-model.json did not come back. Fix that and re-run "
            "— do not freeze this one."
        )
    absent = [name for name in _IDENTITY_FIELDS if not model.get(name)]
    if absent:
        raise BaselineError(
            f"model identity is missing {', '.join(absent)}; the identity guard "
            f"reads exactly {', '.join(_IDENTITY_FIELDS)}, so a reference without "
            "them cannot be compared against"
        )


def _require_clean_harness(harness: dict | None) -> None:
    """Refuse a reference the committed harness did not produce.

    Every later comparison is read against the baseline, so the code that ran
    and scored it has to be one someone can check out again. The first baseline
    was taken from a working tree whose entry point and scoring were committed a
    week later: its numbers happen to re-derive from the committed code, but
    nothing in the record could have said so.

    A harness the record cannot name is refused for the same reason as a dirty
    one: it gives no more to trace back to.
    """
    if (not isinstance(harness, dict) or not harness.get("runner_sha")
            or not isinstance(harness.get("dirty"), bool)):
        raise BaselineError(
            "refusing a baseline that cannot say which harness produced it: it "
            "needs harness.runner_sha and harness.dirty, which daily_run records "
            "in the run's run.json (pass it to freeze with --manifest)"
        )
    if harness["dirty"]:
        raise BaselineError(
            f"refusing a baseline measured by a harness with uncommitted changes "
            f"(runner_sha {harness['runner_sha'][:9]}, dirty): nobody can "
            "reconstruct the code that produced it. Take a new one from a "
            "committed harness with --run-type baseline."
        )


def build(reports: JobReport | Sequence[tuple[str, JobReport]], *,
          corpus_pin: str, run_id: str, k: int, model: dict | None,
          subject: dict | None, harness: dict | None, params: dict,
          notes: str | None = None) -> dict:
    """Freeze one or more runs as a baseline.

    More than one run is accepted because a corpus does not always change whole.
    When a single task's definition is corrected, the other tasks' measurements
    are still measurements of exactly what they measure: harbor's per-task
    `task_checksum` is what says whether a task's data is still current, and it
    is unchanged for every task the correction did not touch. Re-running them to
    satisfy a corpus-wide label would spend hours to reproduce numbers already
    in hand.

    Later runs win per task, so a backfill supersedes what it replaces. Every
    task records the run it came from: a baseline assembled from more than one
    run is a real difference from one taken in a single sitting — different
    time, possibly different machine load — and the next person reading it has
    to be able to see that rather than infer it.

    Refuses a set that could not be scored. A reference taken from an incomplete
    run would be compared against for months, and nothing in a later comparison
    would show that its k was short or its trials unjudged.
    """
    _require_model_identity(model)
    _require_clean_harness(harness)

    if isinstance(reports, JobReport):
        reports = [(run_id, reports)]

    merged: dict[str, tuple[str, TaskScore]] = {}
    for source, report in reports:
        for task in report.tasks:
            merged[task.task_name] = (source, task)

    problems: list[str] = []
    for name, (source, task) in sorted(merged.items()):
        if task.k != k:
            problems.append(f"{name}: {task.k} trial(s), expected {k} (from {source})")
        unscored = [t.trial_name for t in task.trials if not t.scored]
        if unscored:
            problems.append(f"{name}: never scored: {', '.join(unscored)} (from {source})")
    # The infra guard stays per run: a rate is only meaningful over the run that
    # produced it, and averaging two runs' rates would hide a bad one.
    for source, report in reports:
        if report.infra_rate > 0.05:
            problems.append(f"{source}: infra failure rate {report.infra_rate:.1%}")
    if problems:
        raise BaselineError(
            "refusing to freeze an unscoreable run as a baseline: "
            + "; ".join(problems)
        )

    report = reports[-1][1]
    return {
        "schema_version": SCHEMA_VERSION,
        "corpus_pin": corpus_pin,
        "taken_at": datetime.now(timezone.utc).isoformat(),
        "run_id": run_id,
        "job_id": report.job_id,
        "k": k,
        # Read by run_l2's identity guard, which refuses to run when the endpoint
        # no longer serves what this baseline measured.
        "model": model,
        "subject": subject,
        "harness": harness,
        "params": params,
        "bootstrap": {
            "algorithm": BOOTSTRAP_ALGORITHM,
            "seed": BOOTSTRAP_SEED,
            "resamples": BOOTSTRAP_RESAMPLES,
        },
        # What the CLI itself reported inside the trials. More than one value
        # means the mounted image changed while the run was in flight, which
        # makes the whole baseline a mixture of two builds.
        "measured_versions": sorted(
            {t.agent_version for _, r in reports for t in r.trials if t.agent_version}
        ),
        "infra": {
            "failures": sum(r.infra_failures for _, r in reports),
            "rate": max(r.infra_rate for _, r in reports),
        },
        "sources": [
            {"run_id": source, "job_id": r.job_id,
             "tasks": sorted(t.task_name for t in r.tasks),
             "infra_rate": r.infra_rate}
            for source, r in reports
        ],
        "notes": notes,
        "tasks": {
            name: {**_task_entry(task), "source_run_id": source}
            for name, (source, task) in sorted(merged.items())
        },
    }


def write(path: Path, baseline: dict) -> None:
    """Write a baseline, refusing to replace one that already exists.

    Overwriting in place would rewrite the reference every earlier comparison was
    made against, and nothing would record that the numbers moved.
    """
    if path.exists():
        raise BaselineError(
            f"{path} already exists; a baseline for this corpus pin was already "
            "taken. Remove it deliberately, in its own commit, if it is to be "
            "replaced."
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n")


def load(path: Path) -> dict:
    try:
        baseline = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise BaselineError(f"cannot read baseline {path}: {error}") from error
    if baseline.get("schema_version") != SCHEMA_VERSION:
        raise BaselineError(
            f"{path} is schema version {baseline.get('schema_version')!r}, this "
            f"reader speaks {SCHEMA_VERSION}"
        )
    # Checked on the way in as well as on the way out. `build` is where a bad
    # reference is stopped from being written; this is where one that exists
    # anyway — hand-edited, or frozen before the check existed — is stopped from
    # being compared against, with a sentence saying what is wrong rather than a
    # TypeError from inside the identity guard.
    _require_model_identity(baseline.get("model"))
    _require_clean_harness(baseline.get("harness"))
    return baseline


# --- comparison -----------------------------------------------------------


@dataclass(frozen=True)
class Criterion:
    name: str
    detail: str
    ok: bool
    #: Whether trigger-policy.md treats a breach as grounds to block. `False`
    #: here is a statement about the criterion, not about this run.
    blocking: bool


@dataclass(frozen=True)
class Comparison:
    tasks_compared: tuple[str, ...]
    baseline_only: tuple[str, ...]
    candidate_only: tuple[str, ...]
    full_flips: tuple[str, ...]
    degraded: tuple[str, ...]
    improved: tuple[str, ...]
    pass_rate_delta: float
    pass_rate_ci: tuple[float, float]
    token_change: float | None
    turns_change: float | None
    end_reasons: dict[str, int]
    turn_exhausted_delta: int
    infra_rate: float
    #: Whether both arms ran the same number of repetitions. They must, for the
    #: criteria to mean anything: a per-task score out of 1 and one out of 3 are
    #: not the same quantity, and comparing them manufactures flips.
    comparable: bool = True
    baseline_k: int = 0
    candidate_k: int = 0

    @property
    def net_degraded(self) -> int:
        return len(self.degraded) - len(self.improved)

    def criteria(self) -> tuple[Criterion, ...]:
        """Every criterion, evaluated and reported. None of them blocks.

        trigger-policy.md states it for the whole table — "阈值状态：初值已被本
        语料上的 bootstrap 证伪，重标前 L2 不得 blocking" — and the thresholds it
        refers to are the ones in this file. Only `net degraded` had been
        disarmed to match; the reason is recorded per criterion below, and it is
        not the same reason for all of them.

        Re-arming one is not a matter of flipping its flag back. It needs an A/A
        measurement on this corpus — same build both arms — showing that
        criterion's false-block rate under 5%, recorded beside the threshold it
        justifies. `runner/calibrate.py` is what produces that number; every
        criterion here has now been through it and the five statistical ones all
        false-block unchanged code at 6% or worse.
        Blocking on a criterion that fires on unchanged code does not make the
        gate strict; it makes `fail` mean nothing, and every such record has to
        be discounted by hand afterwards.
        """
        if not self.comparable:
            # Not "everything passed" and not "everything failed" — the
            # thresholds are undefined across unequal k. Ten of this corpus's
            # tasks are 3/3, and a 3/3 task that scores 0/1 reads as a full flip;
            # at k=1 against a k=3 baseline that fires on most days with no code
            # change at all. Reporting per-task deltas is still useful. Reporting
            # verdicts is not.
            return (Criterion(
                "arms not comparable",
                f"baseline k={self.baseline_k}, this run k={self.candidate_k}; "
                "per-task deltas below are indicative only, no criterion applies",
                ok=True, blocking=False,
            ),)
        out = [
            # FALSIFIED: 0% to 25% false-block (calibrate.py, resample vs beta).
            # It reads like the one call needing no statistics, and it is not.
            # The 0% is the estimator talking, not the gate: resampling assigns
            # a task recorded at k/k a pass probability of 1, so it can never
            # flip. Give those tasks the failure rate their Beta posterior says
            # they have — three clean trials is weak evidence of never failing —
            # and a quarter of unchanged runs trip this.
            Criterion(
                "full flip",
                f"{len(self.full_flips)} task(s) went k/k -> 0/k"
                + (f": {', '.join(self.full_flips)}" if self.full_flips else "")
                + " — FALSIFIED (0-25% false-block), does not block",
                ok=not self.full_flips,
                blocking=False,
            ),
            # FALSIFIED: 28% (k=1) to 32% (k=3) false-block rate on unchanged
            # code. Note the direction — raising k makes it worse, because a
            # per-task score gains intermediate values to disagree on. Loosening
            # to "net > 4" buys a 3.9% rate and a gate that only catches a
            # regression killing five tasks outright, so neither end is usable
            # and the count of degraded tasks is the wrong quantity to gate on.
            Criterion(
                "net degraded",
                f"{self.net_degraded} (degraded {len(self.degraded)}, improved "
                f"{len(self.improved)}), threshold <= {MAX_NET_DEGRADED} "
                "— FALSIFIED (28-32% false-block), does not block",
                ok=self.net_degraded <= MAX_NET_DEGRADED,
                blocking=False,
            ),
            # FALSIFIED: 55% to 92% false-block rate on unchanged code, the
            # range spanning how the frozen baseline's own noise is modelled.
            # The cause is a threshold finer than the instrument: at 30 tasks
            # x k=3 the interval is about +/-5.6pp wide, so a -5pp floor sits
            # inside the noise band. Staying quiet requires the run to score
            # *better* than the baseline by +0.6pp, which unchanged code manages
            # about half the time by chance. Widening k does not fix this
            # cheaply — the interval narrows with sqrt(trials), and the wider
            # estimator puts the half-width at 11.6pp, not 5.6pp.
            Criterion(
                "pass rate CI",
                f"delta {self.pass_rate_delta:+.1%}, 95% CI "
                f"[{self.pass_rate_ci[0]:+.1%}, {self.pass_rate_ci[1]:+.1%}], "
                f"lower bound must be >= {MIN_PASS_RATE_CI_LOWER:+.0%} "
                "— FALSIFIED (55-92% false-block), does not block",
                ok=self.pass_rate_ci[0] >= MIN_PASS_RATE_CI_LOWER,
                blocking=False,
            ),
            # VACUOUS, and for a different reason than the rest: the CLI's
            # RunEndReason is end_turn | stop | user_interrupt | provider_error |
            # tool_error, with no turn-exhausted value, so this delta is
            # structurally always 0. It was not a gate set too tight, it was a
            # gate wired to nothing — blocking on it asserted a check that never
            # ran. Re-arming waits on the CLI reporting the reason at all.
            Criterion(
                "turn exhausted",
                f"{self.turn_exhausted_delta:+d}, threshold "
                f"<= +{MAX_TURN_EXHAUSTED_INCREASE} "
                "— VACUOUS (no such end reason exists), does not block",
                ok=self.turn_exhausted_delta <= MAX_TURN_EXHAUSTED_INCREASE,
                blocking=False,
            ),
            # REDUNDANT: an over-limit rate is already a `JobReport.problems`
            # entry, and the caller reads problems before breaches, so such a run
            # is `invalid` before this is consulted. Blocking here could only
            # ever have relabelled an unscoreable run as a failed one — which is
            # the one distinction trigger-policy.md is most insistent about.
            Criterion(
                "infra rate",
                f"{self.infra_rate:.1%}, threshold <= 5% "
                "— REDUNDANT (an over-limit run is already invalid)",
                ok=self.infra_rate <= 0.05,
                blocking=False,
            ),
        ]
        # FALSIFIED too, and this was the surprise: a median over per-task
        # medians reads as a robust statistic, and it is not robust to a task
        # crossing between the passing set and the failing set. Only jointly
        # successful tasks are compared, so which tasks are in the median shifts
        # from run to run, and the median moves with them. calibrate.py puts
        # token cost at 6-10% and turns at 27-30% on unchanged code.
        for name, change in (("token cost median", self.token_change),
                             ("turns median", self.turns_change)):
            if change is None:
                # No jointly successful task is `invalid`, not zero: there is
                # nothing to compare, which is not the same as no change.
                out.append(Criterion(
                    name, "no task succeeded in both arms; not computable "
                    "— does not block",
                    ok=False, blocking=False,
                ))
            else:
                out.append(Criterion(
                    name,
                    f"{change:+.1%}, threshold <= +{MAX_MEDIAN_INCREASE:.0%} "
                    "— FALSIFIED (6-30% false-block), does not block",
                    ok=change <= MAX_MEDIAN_INCREASE,
                    blocking=False,
                ))
        return tuple(out)

    @property
    def blocking_breaches(self) -> tuple[Criterion, ...]:
        """The criteria a run may be failed on. Empty while the table is disarmed.

        Kept as a derivation rather than a literal `()` so that re-arming one
        criterion is a one-line change here and nowhere else — and so the
        caller's `fail` path stays exercised by the tests that construct a
        blocking criterion directly.
        """
        return tuple(c for c in self.criteria() if c.blocking and not c.ok)


def _median_over_tasks(
    scores: dict[str, TaskScore], tasks: tuple[str, ...],
    value: str,
) -> dict[str, float]:
    """Per-task median of `value` over that task's comparable successful trials."""
    out: dict[str, float] = {}
    for name in tasks:
        samples = [
            getattr(trial, value)
            for trial in scores[name].trials
            if trial.passed and trial.comparable_usage
            and getattr(trial, value) is not None
        ]
        if samples:
            out[name] = statistics.median(samples)
    return out


def _paired_ci(deltas: list[float]) -> tuple[float, float]:
    """95% CI of the mean per-task delta, by paired bootstrap over tasks."""
    if not deltas:
        return (0.0, 0.0)
    rng = random.Random(BOOTSTRAP_SEED)
    size = len(deltas)
    means = sorted(
        sum(rng.choices(deltas, k=size)) / size
        for _ in range(BOOTSTRAP_RESAMPLES)
    )
    low = means[int(0.025 * BOOTSTRAP_RESAMPLES)]
    high = means[int(0.975 * BOOTSTRAP_RESAMPLES) - 1]
    return (low, high)


def _end_reasons(trials: tuple[Trial, ...]) -> dict[str, int]:
    out: dict[str, int] = {}
    for trial in trials:
        if trial.end_reason:
            out[trial.end_reason] = out.get(trial.end_reason, 0) + 1
    return out


def compare(baseline: dict, candidate: JobReport, candidate_k: int) -> Comparison:
    """Compare a run against a baseline, per task.

    Refuses a corpus mismatch outright: two runs over different task definitions
    produce numbers that look comparable and are not. Unequal k is the same
    problem one level down — the tasks match but the quantities do not — and is
    reported rather than refused, because the deltas still carry a signal even
    when no threshold applies to them.
    """
    base_tasks = baseline.get("tasks") or {}
    cand_tasks = {task.task_name: task for task in candidate.tasks}
    shared = tuple(sorted(set(base_tasks) & set(cand_tasks)))

    for name in shared:
        base_checksum = base_tasks[name].get("task_checksum")
        cand_checksum = cand_tasks[name].trials[0].task_checksum
        if base_checksum and cand_checksum and base_checksum != cand_checksum:
            raise BaselineError(
                f"{name}: task definition changed since the baseline "
                f"({base_checksum[:12]} -> {cand_checksum[:12]}); re-take the "
                "baseline rather than comparing across corpora"
            )

    deltas: list[float] = []
    full_flips: list[str] = []
    degraded: list[str] = []
    improved: list[str] = []
    for name in shared:
        before = float(base_tasks[name]["pass_score"])
        after = cand_tasks[name].pass_score
        deltas.append(after - before)
        if before == 1.0 and after == 0.0:
            full_flips.append(name)
        if after < before:
            degraded.append(name)
        elif after > before:
            improved.append(name)

    base_trials = tuple(
        Trial(
            trial_name=t["trial_name"], task_name=name, task_checksum="",
            reward=t["reward"], exception=t["exception"],
            input_tokens=t["input_tokens"], output_tokens=t["output_tokens"],
            cache_tokens=t["cache_tokens"], turns=t["turns"],
            usage_complete=t["usage_complete"], agent_seconds=t["agent_seconds"],
            agent_version=None, end_reason=t.get("end_reason"),
            _uncached=t.get("uncached_input_tokens"),
        )
        for name, entry in base_tasks.items() for t in entry["trials"]
    )
    base_scores = {
        name: TaskScore(name, tuple(t for t in base_trials if t.task_name == name))
        for name in base_tasks
    }

    changes: dict[str, float | None] = {}
    for metric in ("token_cost", "turns"):
        before = _median_over_tasks(base_scores, shared, metric)
        after = _median_over_tasks(cand_tasks, shared, metric)
        joint = sorted(set(before) & set(after))
        if not joint:
            changes[metric] = None
            continue
        before_median = statistics.median(before[name] for name in joint)
        after_median = statistics.median(after[name] for name in joint)
        changes[metric] = (
            (after_median - before_median) / before_median if before_median else None
        )

    base_reasons = _end_reasons(base_trials)
    cand_reasons = _end_reasons(candidate.trials)
    reason_deltas = {
        reason: cand_reasons.get(reason, 0) - base_reasons.get(reason, 0)
        for reason in sorted(set(base_reasons) | set(cand_reasons))
    }

    baseline_k = int(baseline.get("k") or 0)
    return Comparison(
        comparable=baseline_k == candidate_k,
        baseline_k=baseline_k,
        candidate_k=candidate_k,
        tasks_compared=shared,
        baseline_only=tuple(sorted(set(base_tasks) - set(cand_tasks))),
        candidate_only=tuple(sorted(set(cand_tasks) - set(base_tasks))),
        full_flips=tuple(full_flips),
        degraded=tuple(degraded),
        improved=tuple(improved),
        pass_rate_delta=sum(deltas) / len(deltas) if deltas else 0.0,
        pass_rate_ci=_paired_ci(deltas),
        token_change=changes["token_cost"],
        turns_change=changes["turns"],
        end_reasons=reason_deltas,
        turn_exhausted_delta=reason_deltas.get(TURN_EXHAUSTED_REASON, 0),
        infra_rate=candidate.infra_rate,
    )


def render(comparison: Comparison) -> str:
    lines = [
        f"compared {len(comparison.tasks_compared)} task(s)"
        + (f", {len(comparison.baseline_only)} only in baseline"
           if comparison.baseline_only else "")
        + (f", {len(comparison.candidate_only)} only in this run"
           if comparison.candidate_only else ""),
        "",
    ]
    if not comparison.comparable:
        lines.append(f"  baseline k={comparison.baseline_k} vs this run "
                     f"k={comparison.candidate_k}: criteria not evaluated")
        lines.append("")
    for criterion in comparison.criteria():
        mark = "ok  " if criterion.ok else ("FAIL" if criterion.blocking else "warn")
        lines.append(f"  [{mark}] {criterion.name}: {criterion.detail}")
    if comparison.comparable and not any(c.blocking for c in comparison.criteria()):
        # Said outright, because the alternative reading of an all-`warn` report
        # is "the gate looked and was satisfied". It did not look: every
        # threshold here is awaiting re-calibration, so this run cannot be
        # failed on the numbers above no matter what they say.
        lines.append("\n  no criterion blocks: thresholds await re-calibration "
                     "(trigger-policy.md, L2 门禁). A `warn` above is a reading, "
                     "not a verdict.")
    if comparison.end_reasons:
        changed = {r: d for r, d in comparison.end_reasons.items() if d}
        lines.append(f"\n  end reasons (change): {changed or 'unchanged'}")
    return "\n".join(lines)


# --- freezing ------------------------------------------------------------


def _sidecars(job_dir: Path) -> tuple[dict | None, dict | None]:
    """The job config and model identity harbor's own run left beside the job.

    Read rather than passed in: these are what the run actually used. A
    hand-entered reasoning effort or k would be an assertion about the run,
    and the whole point of recording them is that they are not assertions.
    """
    config_path = job_dir.parent / f"{job_dir.name}-job.json"
    model_path = job_dir.parent / f"{job_dir.name}-model.json"
    config = json.loads(config_path.read_text()) if config_path.exists() else None
    model = json.loads(model_path.read_text()) if model_path.exists() else None
    return config, model


def params_from_config(config: dict | None) -> dict:
    if not config:
        return {}
    agent = (config.get("agents") or [{}])[0]
    return {
        "k": config.get("n_attempts"),
        "timeout_multiplier": config.get("timeout_multiplier"),
        "agent_timeout_multiplier": config.get("agent_timeout_multiplier"),
        "n_concurrent_trials": config.get("n_concurrent_trials"),
        "model_name": agent.get("model_name"),
        "model_config_sha256": (
            hashlib.sha256(model_config.encode()).hexdigest()
            if (model_config := (agent.get("env") or {}).get("DIM_EVAL_MODEL_CONFIG_JSON"))
            else None
        ),
    }


def freeze(job_dirs: list[Path], *, pin: str, k: int, out: Path,
           notes: str | None, subject: dict | None,
           harness: dict | None) -> dict:
    """Assemble a baseline from one or more job directories, last wins per task."""
    from results import read_job

    reports = [(path.name, read_job(path)) for path in job_dirs]
    configs = [_sidecars(path) for path in job_dirs]

    # The newest run's identity describes the endpoint the baseline was closed
    # against; the older runs' configs are kept so a divergence is visible.
    model = next((m for _, m in reversed(configs) if m), None)
    params = params_from_config(configs[-1][0])
    per_run = {
        path.name: params_from_config(config)
        for path, (config, _) in zip(job_dirs, configs)
    }
    if len({json.dumps(p, sort_keys=True) for p in per_run.values()}) > 1:
        # Not fatal — a backfill legitimately runs alone, so concurrency differs —
        # but it is recorded per run rather than flattened into one claim.
        params = {**params, "per_run": per_run}

    baseline = build(
        reports, corpus_pin=pin, run_id=job_dirs[-1].name, k=k, model=model,
        subject=subject, harness=harness, params=params, notes=notes,
    )
    write(out, baseline)
    return baseline


def _freeze_cli(argv: list[str]) -> int:
    import argparse

    parser = argparse.ArgumentParser(prog="baseline.py freeze")
    parser.add_argument("jobs", type=Path, nargs="+",
                        help="harbor job directories; later ones win per task")
    parser.add_argument("--pin", help="corpus pin (default: tasks/MANIFEST.json)")
    parser.add_argument("--k", type=int, default=3)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--notes")
    parser.add_argument("--manifest", type=Path, required=True,
                        help="the daily_run run.json of the run, for the subject it "
                             "built and the harness that measured it")
    args = parser.parse_args(argv)

    benchmarks = HERE.parent
    pin = args.pin
    if not pin:
        pin = json.loads((benchmarks / "tasks" / "MANIFEST.json").read_text())["tree_digest"]

    manifest = json.loads(args.manifest.read_text())
    subject, harness = manifest.get("image"), manifest.get("harness")

    out = args.out or (benchmarks / "baselines" / f"{pin}.json")
    frozen = freeze(args.jobs, pin=pin, k=args.k, out=out, notes=args.notes,
                    subject=subject, harness=harness)
    print(f"baseline -> {out}")
    print(f"  corpus pin {pin[:12]}  k={frozen['k']}  {len(frozen['tasks'])} task(s)")
    for source in frozen["sources"]:
        print(f"  {source['run_id']}: {len(source['tasks'])} task(s)")
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "freeze":
        raise SystemExit(_freeze_cli(sys.argv[2:]))
    raise SystemExit("usage: baseline.py freeze <job dir>... [--notes ...]")
