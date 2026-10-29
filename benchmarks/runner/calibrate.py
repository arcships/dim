#!/usr/bin/env python3
"""Measure what each L2 criterion does to an unchanged build.

A criterion that fires on code nobody touched is not a strict gate, it is a
coin flip that writes `fail` into the ledger. trigger-policy.md therefore lets a
criterion block only once an A/A measurement puts its false-block rate under 5%,
and this is the thing that produces that number.

    python3 benchmarks/runner/calibrate.py benchmarks/baselines/<pin>.json

The candidate arm is synthesised from the baseline's own recorded trials, so the
two arms are the same build by construction and every criterion that trips is a
false positive by definition. Two ways of standing in for "the same build run
again", because the answer differs between them and the difference is the honest
uncertainty:

* ``resample`` draws the k trials with replacement from that task's recorded
  trials. It is the plug-in estimator, and it understates: a task recorded at
  k/k is assigned p=1 and can never fail again, which is how the full-flip
  criterion measures 0%. That 0 is a property of the estimator, not of the gate.
* ``beta`` draws the task's pass probability from its Beta(passes+1, fails+1)
  posterior and then draws k trials from it, so a task observed at k/k keeps a
  real chance of failing. It is the wider and the more believable of the two.

Neither is a true A/A — that needs the same image run twice on the machine, and
it is what should eventually replace this. What this does give is a floor: a
criterion that already false-blocks under the *understating* estimator cannot be
rescued by measuring it more carefully.

The criteria come from `Comparison.criteria()`, not from a copy of the
thresholds kept here. A calibration that drifts from the gate it calibrates is
worse than none.
"""

from __future__ import annotations

import argparse
import json
import random
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import baseline as baselines  # noqa: E402
import results  # noqa: E402


def _trial(record: dict, task: str) -> results.Trial:
    return results.Trial(
        trial_name=record["trial_name"], task_name=task, task_checksum="",
        reward=record["reward"], exception=record["exception"],
        input_tokens=record["input_tokens"], output_tokens=record["output_tokens"],
        cache_tokens=record["cache_tokens"], turns=record["turns"],
        usage_complete=record["usage_complete"],
        agent_seconds=record["agent_seconds"], agent_version=None,
        end_reason=record.get("end_reason"),
        _uncached=record.get("uncached_input_tokens"),
    )


def _candidate(frozen: dict, k: int, method: str, rng: random.Random) -> results.JobReport:
    """One synthetic re-run of the very build the baseline was taken from."""
    tasks = []
    for name, entry in frozen["tasks"].items():
        recorded = entry["trials"]
        if method == "resample":
            drawn = [dict(rng.choice(recorded)) for _ in range(k)]
        else:
            passes = sum(1 for t in recorded if (t["reward"] or 0) > 0)
            p = rng.betavariate(passes + 1, len(recorded) - passes + 1)
            # Keep each draw's usage attached to a real trial, so the cost and
            # turns criteria see plausible numbers rather than invented ones.
            winners = [t for t in recorded if (t["reward"] or 0) > 0] or recorded
            losers = [t for t in recorded if not (t["reward"] or 0) > 0] or recorded
            drawn = []
            for _ in range(k):
                won = rng.random() < p
                source = dict(rng.choice(winners if won else losers))
                source["reward"] = 1.0 if won else 0.0
                drawn.append(source)
        for index, record in enumerate(drawn):
            # Distinct names: a JobReport keyed on duplicates is not a run.
            record["trial_name"] = f"{name}__sim{index}"
        tasks.append(results.TaskScore(name, tuple(_trial(r, name) for r in drawn)))
    trials = tuple(t for task in tasks for t in task.trials)
    return results.JobReport(
        job_id="calibration", tasks=tuple(tasks), trials=trials,
        missing=(), undeclared=(), declared_trials=len(trials),
    )


def calibrate(frozen: dict, *, k: int, rounds: int, method: str,
              seed: int) -> tuple[dict[str, int], list[float]]:
    rng = random.Random(seed)
    fired: dict[str, int] = {}
    widths: list[float] = []
    for _ in range(rounds):
        comparison = baselines.compare(frozen, _candidate(frozen, k, method, rng), k)
        low, high = comparison.pass_rate_ci
        widths.append(high - low)
        for criterion in comparison.criteria():
            fired.setdefault(criterion.name, 0)
            if not criterion.ok:
                fired[criterion.name] += 1
    return fired, widths


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("--rounds", type=int, default=400)
    parser.add_argument("--k", type=int, default=None,
                        help="defaults to the baseline's own k")
    parser.add_argument("--method", choices=("resample", "beta"), default=None,
                        help="default: run both")
    parser.add_argument("--seed", type=int, default=20260914)
    args = parser.parse_args()

    frozen = json.loads(args.baseline.read_text())
    k = args.k or int(frozen["k"])
    methods = [args.method] if args.method else ["resample", "beta"]

    print(f"baseline {args.baseline.name[:12]}  {len(frozen['tasks'])} task(s)  "
          f"k={k}  {args.rounds} round(s) per method\n")
    for method in methods:
        fired, widths = calibrate(frozen, k=k, rounds=args.rounds,
                                  method=method, seed=args.seed)
        half = statistics.mean(widths) / 2
        print(f"[{method}] pass-rate CI half-width {half:+.1%} "
              f"(the gate's floor is {baselines.MIN_PASS_RATE_CI_LOWER:+.0%})")
        for name, count in fired.items():
            rate = count / args.rounds
            mark = "  OVER 5%" if rate > 0.05 else ""
            print(f"    {name:22s} false-block {rate:6.1%}{mark}")
        print()
    print("A criterion over 5% must not block. See trigger-policy.md, L2 门禁.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
