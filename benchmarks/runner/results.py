#!/usr/bin/env python3
"""Read a harbor job directory into per-task scores.

Harbor writes one directory per trial and one summary for the job. Neither is a
result the gate can use directly: the summary aggregates across tasks, and a
trial record says what happened without saying whether it may be compared. This
turns both into per-task scores plus the reasons, if any, that the run cannot be
scored at all.

What the shape of harbor's output forces:

* **There is no repetition index.** A trial is `<task-id>__<random suffix>`, and
  nothing in the record numbers it. So repetitions are exchangeable and k is
  simply how many trials a task has. That is enough for a paired comparison,
  which pairs per task and never per repetition.
* **Errored is not failed.** A trial that raised can still carry a verifier
  reward — an observed `AgentTimeoutError` scored 1.0, because the agent had
  already done the work when its deadline passed. Counting exceptions as
  failures would have lost that.
* **Unscored is not failed either.** A trial with no reward says nothing about
  the change. It makes the run incomplete, which is `invalid`, and
  trigger-policy.md is explicit that `invalid` may not be read as `fail`.
* **Cost is not read from `cost_usd`.** In a run whose adapter reports it, that
  field's `cost_source` is `reported_by_dimcode` — the subject of the experiment
  pricing itself. Tokens are recorded here and priced later against the table
  pinned in the baseline, so neither a provider price change nor a change in how
  the CLI reports cost can move the gate on its own.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from run_l2 import INFRA_EXCEPTIONS  # noqa: E402

#: Above this the whole run is void rather than merely worse: it is measuring the
#: infrastructure. trigger-policy.md, Experiment parameters.
MAX_INFRA_RATE = 0.05

#: What harbor writes when the verifier produced no reward. Distinct from 0.0,
#: which is a verdict.
UNSCORED_REWARD = -1.0

#: Harbor's own file, written both for the job and for each trial. Same name,
#: different schema; the job's is the one with `stats`.
RESULT_FILENAME = "result.json"


class ResultsError(RuntimeError):
    """The job's output could not be read into scores."""


@dataclass(frozen=True)
class Trial:
    trial_name: str
    task_name: str
    task_checksum: str
    reward: float | None
    #: Exception class, from the job summary rather than the trial record: the
    #: summary's grouping is the classification harbor itself made, and the
    #: trial's `exception_info` internals are not relied on here.
    exception: str | None
    input_tokens: int | None
    output_tokens: int | None
    cache_tokens: int | None
    turns: int | None
    #: False when the trial spawned subagents, whose provider calls are missing
    #: from the parent's usage. Such a trial's cost and turns are an undercount
    #: and must not be compared. None when the adapter did not say.
    usage_complete: bool | None
    agent_seconds: float | None
    agent_version: str | None
    #: What the adapter reported the run ended as, when it reported anything.
    #: Used for the failure-mode distribution.
    end_reason: str | None = None
    #: Our adapter's own count of uncached input; absent under another adapter.
    _uncached: int | None = None

    @property
    def scored(self) -> bool:
        return self.reward is not None

    @property
    def passed(self) -> bool:
        return self.reward == 1.0

    @property
    def infra_failure(self) -> bool:
        return self.exception in INFRA_EXCEPTIONS

    @property
    def uncached_input_tokens(self) -> int | None:
        """Input tokens that were not served from cache.

        Our adapter computes this; a run driven by another one is derived from
        the two raw counts instead.
        """
        if self._uncached is not None:
            return self._uncached
        if self.input_tokens is None:
            return None
        return max(self.input_tokens - (self.cache_tokens or 0), 0)

    @property
    def token_cost(self) -> int | None:
        """The scalar the cost metric compares.

        Uncached input plus output: the two components every provider bills at
        full rate, and the two a harness change actually moves. Cached reads are
        left out because their price is a fraction of the others' and including
        them at equal weight would let context growth dominate a metric meant to
        catch burn. Replacing this with a real price table is a change to this
        one expression.
        """
        uncached = self.uncached_input_tokens
        if uncached is None or self.output_tokens is None:
            return None
        return uncached + self.output_tokens

    @property
    def comparable_usage(self) -> bool:
        """Whether this trial's cost and turns may enter a comparison."""
        return (self.usage_complete is not False
                and self.input_tokens is not None
                and self.output_tokens is not None)


@dataclass(frozen=True)
class TaskScore:
    """One task's trials in one run — the unit the gate compares."""

    task_name: str
    trials: tuple[Trial, ...]

    @property
    def k(self) -> int:
        return len(self.trials)

    @property
    def passes(self) -> int:
        return sum(1 for trial in self.trials if trial.passed)

    @property
    def pass_score(self) -> float:
        """`passes / k`, and only meaningful when every trial was scored.

        Callers check `fully_scored` first; dividing by a k that includes
        unscored trials would quietly report a lower score for a task whose
        harness failed, which is the difference between `invalid` and `fail`.
        """
        return self.passes / self.k if self.k else 0.0

    @property
    def fully_scored(self) -> bool:
        return all(trial.scored for trial in self.trials)


@dataclass(frozen=True)
class JobReport:
    job_id: str
    trials: tuple[Trial, ...]
    tasks: tuple[TaskScore, ...]
    #: Trials the job summary names but whose record is not on disk, and records
    #: on disk the summary never named. Either way the run is not complete, and
    #: neither shows up as a failure anywhere else.
    missing: tuple[str, ...]
    undeclared: tuple[str, ...]
    declared_trials: int

    @property
    def infra_failures(self) -> int:
        return sum(1 for trial in self.trials if trial.infra_failure)

    @property
    def infra_rate(self) -> float:
        return self.infra_failures / len(self.trials) if self.trials else 0.0

    def problems(self, expected_k: int) -> tuple[str, ...]:
        """Why this run may not be scored. Empty means it may.

        Returned rather than raised: a run that cannot be scored is a real
        outcome with its own name (`invalid`), and the caller records it as that
        instead of treating it as a crash.
        """
        out: list[str] = []
        if self.missing:
            out.append(f"{len(self.missing)} trial(s) declared but not on disk: "
                       f"{', '.join(self.missing[:3])}")
        if self.undeclared:
            out.append(f"{len(self.undeclared)} trial(s) on disk the job never "
                       f"declared: {', '.join(self.undeclared[:3])}")
        if len(self.trials) != self.declared_trials:
            out.append(f"job declares {self.declared_trials} trial(s), read "
                       f"{len(self.trials)}")
        unscored = [t.trial_name for t in self.trials if not t.scored]
        if unscored:
            out.append(f"{len(unscored)} trial(s) never scored: "
                       f"{', '.join(unscored[:3])}")
        wrong_k = [t.task_name for t in self.tasks if t.k != expected_k]
        if wrong_k:
            out.append(f"{len(wrong_k)} task(s) not run exactly {expected_k} "
                       f"time(s): {', '.join(wrong_k[:3])}")
        if self.infra_rate > MAX_INFRA_RATE:
            out.append(f"infra failure rate {self.infra_rate:.1%} over the "
                       f"{MAX_INFRA_RATE:.0%} limit")
        return tuple(out)


def _seconds(phase: dict | None) -> float | None:
    if not phase or not phase.get("started_at") or not phase.get("finished_at"):
        return None
    start = datetime.fromisoformat(phase["started_at"])
    end = datetime.fromisoformat(phase["finished_at"])
    return (end - start).total_seconds()


def _reward(record: dict, trial_name: str) -> float | None:
    verifier = record.get("verifier_result")
    if not verifier:
        return None
    rewards = verifier.get("rewards") or {}
    if "reward" not in rewards:
        return None
    value = float(rewards["reward"])
    if value == UNSCORED_REWARD:
        # Harbor's sentinel for a verifier that returned no verdict. Observed on a
        # trial with no exception at all: the agent finished and the verifier
        # simply did not score it. Not a failure — an absence of one.
        return None
    if value not in (0.0, 1.0):
        # Partial credit would make "passed" a judgement call, and this corpus's
        # verifiers are deterministic pass/fail. Refuse rather than round.
        raise ResultsError(
            f"{trial_name}: reward {value} is neither 0 nor 1; this reader scores "
            "pass/fail and cannot collapse partial credit"
        )
    return value


def read_trial(path: Path, exception: str | None) -> Trial:
    try:
        record = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ResultsError(f"cannot read trial record {path}: {error}") from error

    agent = record.get("agent_result") or {}
    # Our adapter puts its metrics on the agent context, which lands here. A run
    # driven by a different adapter simply has none of them.
    metadata = agent.get("metadata") or {}
    info = record.get("agent_info") or {}

    return Trial(
        trial_name=record["trial_name"],
        task_name=record["task_name"],
        task_checksum=record.get("task_checksum", ""),
        reward=_reward(record, record["trial_name"]),
        exception=exception,
        input_tokens=agent.get("n_input_tokens"),
        output_tokens=agent.get("n_output_tokens"),
        cache_tokens=agent.get("n_cache_tokens"),
        turns=metadata.get("dim_turns"),
        usage_complete=metadata.get("dim_usage_complete"),
        agent_seconds=_seconds(record.get("agent_execution")),
        agent_version=info.get("version"),
        end_reason=metadata.get("dim_end_reason"),
        _uncached=metadata.get("dim_uncached_input_tokens"),
    )


def read_job(job_dir: Path) -> JobReport:
    """Read one harbor job directory."""
    summary_path = job_dir / RESULT_FILENAME
    try:
        summary = json.loads(summary_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ResultsError(f"cannot read job summary {summary_path}: {error}") from error

    stats = summary.get("stats") or {}
    evals = stats.get("evals") or {}
    if len(evals) != 1:
        # One agent per run is the whole design: two arms in one job would make
        # every per-task score ambiguous about which build produced it.
        raise ResultsError(
            f"expected exactly one eval in {summary_path}, found {sorted(evals)}"
        )
    eval_stats = next(iter(evals.values()))

    exception_by_trial: dict[str, str] = {}
    for exception, names in (eval_stats.get("exception_stats") or {}).items():
        for name in names:
            exception_by_trial[name] = exception

    declared: set[str] = set(exception_by_trial)
    for names in (eval_stats.get("reward_stats", {}).get("reward") or {}).values():
        declared.update(names)

    on_disk = {
        child.name: child / RESULT_FILENAME
        for child in sorted(job_dir.iterdir())
        if child.is_dir() and (child / RESULT_FILENAME).is_file()
    }

    trials = tuple(
        read_trial(path, exception_by_trial.get(name))
        for name, path in on_disk.items()
    )

    by_task: dict[str, list[Trial]] = {}
    for trial in trials:
        by_task.setdefault(trial.task_name, []).append(trial)
    tasks = tuple(
        TaskScore(task_name=name, trials=tuple(group))
        for name, group in sorted(by_task.items())
    )

    return JobReport(
        job_id=summary.get("id", ""),
        trials=trials,
        tasks=tasks,
        missing=tuple(sorted(declared - set(on_disk))),
        undeclared=tuple(sorted(set(on_disk) - declared)),
        declared_trials=int(summary.get("n_total_trials", len(declared))),
    )


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser(description="Summarise a harbor job directory.")
    parser.add_argument("job_dir", type=Path)
    parser.add_argument("--k", type=int, default=1, help="repetitions each task should have")
    args = parser.parse_args()

    report = read_job(args.job_dir.resolve())
    print(f"job {report.job_id}  {len(report.trials)} trial(s), "
          f"{len(report.tasks)} task(s)\n")
    for task in report.tasks:
        scored = "" if task.fully_scored else "  UNSCORED TRIALS"
        exceptions = sorted({t.exception for t in task.trials if t.exception})
        print(f"  {task.passes}/{task.k}  {task.task_name:<48}"
              f"{(' ' + ','.join(exceptions)) if exceptions else ''}{scored}")

    passed = sum(t.passes for t in report.tasks)
    total = sum(t.k for t in report.tasks)
    print(f"\n  aggregate {passed}/{total}"
          f"  infra {report.infra_failures}/{len(report.trials)} "
          f"({report.infra_rate:.1%})")

    problems = report.problems(args.k)
    if problems:
        print("\nnot scoreable:")
        for problem in problems:
            print(f"  - {problem}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
