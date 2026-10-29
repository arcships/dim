"""Results reader and baseline flow tests.

The fixtures mirror a real harbor job — the field names and nesting here were
taken from an actual `result.json` pair, not from the upstream docs — because
every failure this reader must not have is a misreading of that shape: an
errored trial counted as a failed one, an unscored trial counted as a failed
one, or a missing trial not counted at all.

Run: python3 -m unittest discover -s benchmarks/runner -p 'flow_test.py'
"""

from __future__ import annotations

import json
import random
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import baseline  # noqa: E402
import calibrate  # noqa: E402
import dim_events  # noqa: E402
import results  # noqa: E402


#: What run_l2 captures from the endpoint and writes beside the results. Every
#: baseline needs one: it is what a later run's identity guard holds the endpoint
#: to, so a reference without it cannot be compared against at all.
MODEL_IDENTITY = {
    "provider": "icecn",
    "model": "glm-5.3",
    "model_reported": "glm-5.3",
    "base_url": "https://example.invalid/v1",
    "captured_at": "2026-09-14T07:19:02+00:00",
}
#: A committed harness, as daily_run records it in run.json.
HARNESS = {"runner_sha": "82afef823071fd3ab233e8d92157ef12fa238590", "dirty": False}


def trial_record(trial_name: str, task_name: str, reward: float | None, *,
                 tokens: tuple[int, int] = (1000, 100),
                 metadata: dict | None = None) -> dict:
    record = {
        "trial_name": trial_name,
        "task_name": task_name,
        "task_checksum": "73d894b2d49e5f9f",
        "agent_info": {"name": "dim", "version": "0.5.1"},
        "agent_result": {
            "n_input_tokens": tokens[0],
            "n_output_tokens": tokens[1],
            "n_cache_tokens": 0,
            "metadata": metadata or {},
        },
        "verifier_result": None if reward is None else {"rewards": {"reward": reward}},
        "exception_info": None,
        "agent_execution": {
            "started_at": "2026-09-14T03:10:13.067022Z",
            "finished_at": "2026-09-14T03:18:42.779797Z",
        },
    }
    return record


class ResultsFlowTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.job = Path(self._tmp.name) / "job"
        self.job.mkdir()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write_job(self, trials: list[dict], exceptions: dict[str, list[str]],
                  *, declare_extra: list[str] = (), evals: int = 1) -> None:
        rewards: dict[str, list[str]] = {}
        for record in trials:
            verifier = record["verifier_result"]
            if verifier is None:
                continue
            key = str(verifier["rewards"]["reward"])
            rewards.setdefault(key, []).append(record["trial_name"])
        declared = [r["trial_name"] for r in trials] + list(declare_extra)

        eval_stats = {
            "n_trials": len(declared),
            "reward_stats": {"reward": rewards},
            "exception_stats": exceptions,
        }
        summary = {
            "id": "job-uuid",
            "n_total_trials": len(declared),
            "stats": {
                "n_completed_trials": len(declared),
                "evals": {f"dim__glm-5.3__tasks{n}": eval_stats
                          for n in range(evals)},
            },
        }
        # An unscored trial appears in no reward bucket, so it is declared here
        # the way harbor would: through the exception grouping or the count.
        for name in declare_extra:
            summary["stats"]["evals"][
                next(iter(summary["stats"]["evals"]))
            ]["exception_stats"].setdefault("AgentTimeoutError", []).append(name)

        (self.job / "result.json").write_text(json.dumps(summary))
        for record in trials:
            directory = self.job / record["trial_name"]
            directory.mkdir()
            (directory / "result.json").write_text(json.dumps(record))

    def test_errored_trial_that_passed_is_not_a_failure(self) -> None:
        """Observed in a real run: the agent's deadline passed after it had already
        finished the work, and the verifier scored it 1.0."""
        self.write_job(
            [
                trial_record("a__1", "tb/a", 1.0),
                trial_record("a__2", "tb/a", 1.0),
            ],
            {"AgentTimeoutError": ["a__2"]},
        )
        report = results.read_job(self.job)
        task = report.tasks[0]
        self.assertEqual((task.passes, task.k), (2, 2))
        self.assertEqual(task.pass_score, 1.0)
        self.assertEqual(report.problems(expected_k=2), ())

    def test_unscored_trial_makes_the_run_unscoreable_without_failing_the_task(self) -> None:
        self.write_job(
            [
                trial_record("b__1", "tb/b", 1.0),
                trial_record("b__2", "tb/b", None),
            ],
            {"NonZeroAgentExitCodeError": ["b__2"]},
        )
        report = results.read_job(self.job)
        task = report.tasks[0]
        self.assertEqual(task.passes, 1)
        self.assertFalse(task.fully_scored)
        problems = report.problems(expected_k=2)
        self.assertTrue(any("never scored" in p for p in problems), problems)

    def test_infra_failures_are_counted_apart_from_task_failures(self) -> None:
        """A provider error says nothing about the change; the rate is the guard
        that voids the run rather than scoring it."""
        self.write_job(
            [
                trial_record("c__1", "tb/c", 0.0),
                trial_record("c__2", "tb/c", 0.0),
            ],
            {"NetworkConnectionError": ["c__2"], "AgentTimeoutError": ["c__1"]},
        )
        report = results.read_job(self.job)
        self.assertEqual(report.infra_failures, 1)
        self.assertEqual(report.infra_rate, 0.5)
        self.assertTrue(any("infra failure rate" in p
                            for p in report.problems(expected_k=2)))

    def test_usage_limit_is_an_infra_failure_not_a_task_failure(self) -> None:
        self.write_job(
            [
                trial_record("q__1", "tb/q", 0.0),
                trial_record("q__2", "tb/q", 0.0),
            ],
            {"ApiUsageLimitError": ["q__1"]},
        )
        report = results.read_job(self.job)
        self.assertEqual(report.infra_failures, 1)
        self.assertTrue(any("infra failure rate" in p
                            for p in report.problems(expected_k=2)))

    def test_missing_trial_is_reported_rather_than_ignored(self) -> None:
        """A trial with no record on disk would otherwise shrink k silently and
        raise the task's pass score."""
        self.write_job([trial_record("d__1", "tb/d", 1.0)], {},
                       declare_extra=["d__2"])
        report = results.read_job(self.job)
        self.assertEqual(report.missing, ("d__2",))
        self.assertEqual(report.tasks[0].k, 1)
        problems = report.problems(expected_k=2)
        self.assertTrue(any("not on disk" in p for p in problems), problems)

    def test_repetitions_are_grouped_by_task_not_by_index(self) -> None:
        """Harbor numbers nothing: three trials of one task are three directories
        with random suffixes."""
        self.write_job(
            [
                trial_record("e__Ab1", "tb/e", 1.0),
                trial_record("e__Zq9", "tb/e", 0.0),
                trial_record("e__Kp3", "tb/e", 1.0),
            ],
            {},
        )
        report = results.read_job(self.job)
        self.assertEqual(len(report.tasks), 1)
        self.assertEqual(report.tasks[0].k, 3)
        self.assertAlmostEqual(report.tasks[0].pass_score, 2 / 3)
        self.assertEqual(report.problems(expected_k=3), ())

    def test_subagent_trial_is_excluded_from_usage_comparison(self) -> None:
        """Its cost and turns are a parent-only undercount, which would read as an
        improvement — the exact false signal the gate exists to catch."""
        self.write_job(
            [
                trial_record("f__1", "tb/f", 1.0,
                             metadata={"dim_turns": 12, "dim_usage_complete": True}),
                trial_record("f__2", "tb/f", 1.0,
                             metadata={"dim_turns": 4, "dim_subagent_runs": 2,
                                       "dim_usage_complete": False}),
            ],
            {},
        )
        report = results.read_job(self.job)
        comparable = [t for t in report.trials if t.comparable_usage]
        self.assertEqual([t.trial_name for t in comparable], ["f__1"])
        self.assertEqual(comparable[0].turns, 12)

    def test_a_verifier_that_gave_no_verdict_is_unscored_not_failed(self) -> None:
        """Harbor writes -1.0 there. Observed on a trial with no exception at all:
        the agent finished and the verifier simply never scored it."""
        self.write_job(
            [
                trial_record("i__1", "tb/i", 1.0),
                trial_record("i__2", "tb/i", -1.0),
            ],
            {},
        )
        report = results.read_job(self.job)
        task = report.tasks[0]
        self.assertEqual(task.passes, 1)
        self.assertFalse(task.fully_scored)
        self.assertFalse(any(t.passed for t in task.trials if t.reward is None))
        self.assertTrue(any("never scored" in p
                            for p in report.problems(expected_k=2)))

    def test_partial_credit_is_refused(self) -> None:
        self.write_job([trial_record("g__1", "tb/g", 0.5)], {})
        with self.assertRaises(results.ResultsError) as caught:
            results.read_job(self.job)
        self.assertIn("neither 0 nor 1", str(caught.exception))

    def test_two_evals_in_one_job_is_refused(self) -> None:
        """Two arms in one job would make every score ambiguous about which build
        produced it."""
        self.write_job([trial_record("h__1", "tb/h", 1.0)], {}, evals=2)
        with self.assertRaises(results.ResultsError):
            results.read_job(self.job)


class BaselineFlowTest(unittest.TestCase):
    """The comparison, which is where a wrong answer is most expensive: it is the
    number someone decides to ship or revert on."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def report(self, trials: list[dict], exceptions: dict | None = None):
        job = self.root / f"job{len(list(self.root.iterdir()))}"
        job.mkdir()
        helper = ResultsFlowTest()
        helper.job = job
        helper.write_job(trials, exceptions or {})
        return results.read_job(job)

    def frozen(self, trials: list[dict], k: int = 2, **kwargs) -> dict:
        kwargs.setdefault("model", MODEL_IDENTITY)
        kwargs.setdefault("harness", HARNESS)
        return baseline.build(
            self.report(trials), corpus_pin="pin", run_id="r", k=k, subject=None,
            params={"reasoning_effort": "high"}, **kwargs,
        )

    def test_a_baseline_without_a_model_identity_is_refused(self) -> None:
        """The reference has to say which model produced it.

        Not for provenance: `run_l2` refuses to run when the endpoint no longer
        serves what the baseline recorded, and it reads exactly these fields. A
        baseline with no identity does not lose the guard's evidence, it disarms
        the guard — the check reached `ModelIdentity.from_dict(None)` and died of
        a TypeError on the eval machine, long after the reference had become
        write-once and could only be removed by a commit.

        The identity is captured remotely and fetched beside the results, so a
        transient rsync failure or DIM_EVAL_SKIP_MODEL_CHECK=1 was enough.
        """
        trials = [trial_record("a__1", "tb/a", 1.0), trial_record("a__2", "tb/a", 1.0)]
        with self.assertRaises(baseline.BaselineError) as caught:
            self.frozen(trials, model=None)
        self.assertIn("no model identity", str(caught.exception))

        # A partial one is refused for the same reason: the guard reads all three.
        for absent in ("provider", "model", "model_reported"):
            partial = {k: v for k, v in MODEL_IDENTITY.items() if k != absent}
            with self.assertRaises(baseline.BaselineError) as caught:
                self.frozen(trials, model=partial)
            self.assertIn(absent, str(caught.exception))

    def test_a_baseline_with_no_identity_is_refused_on_the_way_in_too(self) -> None:
        """One frozen before the check existed, or hand-edited since, must not be
        compared against either — and must say why rather than raising a
        TypeError from inside the guard."""
        path = Path(self._tmp.name) / "stale.json"
        frozen = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        frozen["model"] = None
        path.write_text(json.dumps(frozen))
        with self.assertRaises(baseline.BaselineError):
            baseline.load(path)

    def test_a_baseline_the_committed_harness_did_not_produce_is_refused(self) -> None:
        """The reference has to name a committed harness.

        The first baseline was measured and scored by code that existed only in
        a working tree, and was committed a week later; its record said
        `dirty: true` and nothing stopped it being compared against. A harness
        the record cannot name is refused too, since it traces back to nothing.
        """
        trials = [trial_record("a__1", "tb/a", 1.0), trial_record("a__2", "tb/a", 1.0)]
        with self.assertRaises(baseline.BaselineError) as caught:
            self.frozen(trials, harness={**HARNESS, "dirty": True})
        self.assertIn("uncommitted changes", str(caught.exception))
        for unnamed in (None, {"dirty": False}, {"runner_sha": HARNESS["runner_sha"]}):
            with self.assertRaises(baseline.BaselineError) as caught:
                self.frozen(trials, harness=unnamed)
            self.assertIn("cannot say which harness", str(caught.exception))

        # One that already exists, frozen before the check, is refused on the way
        # in, so no comparison reads against it.
        path = Path(self._tmp.name) / "dirty.json"
        stale = self.frozen(trials)
        stale["harness"] = {**stale["harness"], "dirty": True}
        path.write_text(json.dumps(stale))
        with self.assertRaises(baseline.BaselineError):
            baseline.load(path)

    def test_unscoreable_run_cannot_become_a_baseline(self) -> None:
        """It would be the reference for months with nothing recording that its
        trials were short."""
        with self.assertRaises(baseline.BaselineError) as caught:
            self.frozen([trial_record("a__1", "tb/a", 1.0)], k=2)
        self.assertIn("unscoreable", str(caught.exception))

    def test_existing_baseline_is_not_overwritten(self) -> None:
        data = self.frozen([trial_record("a__1", "tb/a", 1.0),
                            trial_record("a__2", "tb/a", 1.0)])
        path = self.root / "pin.json"
        baseline.write(path, data)
        with self.assertRaises(baseline.BaselineError):
            baseline.write(path, data)

    def test_full_flip_is_detected_and_reported_but_does_not_block(self) -> None:
        """It reads like the one call needing no statistics, and it is not: a
        task at k/k in the baseline can reach 0/k on noise alone, and nothing has
        measured how often. Detection is exact; the threshold is a guess."""
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 0.0),
                             trial_record("a__8", "tb/a", 0.0)])
        comparison = baseline.compare(before, after, 2)
        self.assertEqual(comparison.full_flips, ("tb/a",))
        flip = next(c for c in comparison.criteria() if c.name == "full flip")
        self.assertFalse(flip.ok)
        self.assertFalse(flip.blocking)
        self.assertEqual(comparison.blocking_breaches, ())

    def test_net_degraded_is_reported_but_never_blocks(self) -> None:
        """Its threshold was falsified on this corpus: it false-blocks unchanged
        code roughly a third of the time."""
        # Two tasks half-degrading is net 2, past the threshold of 1.
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0),
                              trial_record("b__1", "tb/b", 1.0),
                              trial_record("b__2", "tb/b", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 1.0),
                             trial_record("a__8", "tb/a", 0.0),
                             trial_record("b__9", "tb/b", 1.0),
                             trial_record("b__8", "tb/b", 0.0)])
        comparison = baseline.compare(before, after, 2)
        self.assertEqual(comparison.net_degraded, 2)
        criterion = next(c for c in comparison.criteria() if c.name == "net degraded")
        self.assertFalse(criterion.ok)
        self.assertFalse(criterion.blocking)
        self.assertNotIn(criterion, comparison.blocking_breaches)

    def test_cost_and_turns_compare_only_jointly_successful_tasks(self) -> None:
        usage = {"dim_turns": 10, "dim_usage_complete": True}
        before = self.frozen([
            trial_record("a__1", "tb/a", 1.0, tokens=(1000, 100), metadata=usage),
            trial_record("a__2", "tb/a", 1.0, tokens=(1000, 100), metadata=usage),
        ])
        after = self.report([
            trial_record("a__9", "tb/a", 1.0, tokens=(1300, 100),
                         metadata={"dim_turns": 13, "dim_usage_complete": True}),
            trial_record("a__8", "tb/a", 1.0, tokens=(1300, 100),
                         metadata={"dim_turns": 13, "dim_usage_complete": True}),
        ])
        comparison = baseline.compare(before, after, 2)
        # 1100 -> 1400 token cost, 10 -> 13 turns: both past the 15% threshold.
        self.assertAlmostEqual(comparison.token_change, 300 / 1100, places=6)
        self.assertAlmostEqual(comparison.turns_change, 0.3, places=6)
        names = {c.name for c in comparison.criteria() if not c.ok}
        self.assertIn("token cost median", names)
        self.assertIn("turns median", names)
        # Reported, not blocked: neither threshold has been calibrated here.
        self.assertEqual(comparison.blocking_breaches, ())

    def test_no_jointly_successful_task_is_not_zero_change(self) -> None:
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 0.0),
                             trial_record("a__8", "tb/a", 0.0)])
        comparison = baseline.compare(before, after, 2)
        self.assertIsNone(comparison.token_change)
        criterion = next(c for c in comparison.criteria()
                         if c.name == "token cost median")
        self.assertFalse(criterion.ok)
        self.assertIn("not computable", criterion.detail)

    def test_unequal_k_reports_deltas_but_evaluates_no_criterion(self) -> None:
        """Ten of this corpus's tasks are 3/3. A 3/3 task scoring 0/1 reads as a
        full flip, so a k=1 screen against a k=3 baseline would print a blocking
        FAIL on most days with no code change at all."""
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 0.0)])
        comparison = baseline.compare(before, after, 1)

        self.assertFalse(comparison.comparable)
        self.assertEqual(comparison.full_flips, ("tb/a",))   # 差值照常算出
        names = [c.name for c in comparison.criteria()]
        self.assertEqual(names, ["arms not comparable"])     # 但不出判据
        self.assertEqual(comparison.blocking_breaches, ())
        self.assertIn("criteria not evaluated", baseline.render(comparison))

    def test_no_criterion_blocks_while_the_thresholds_await_recalibration(self) -> None:
        """trigger-policy.md disarms the L2 table as a whole, not one row of it.

        Locked as a test because the failure is silent: re-arming a criterion
        that false-blocks unchanged code does not produce an error, it produces
        `fail` records that look like findings.
        """
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 0.0),
                             trial_record("a__8", "tb/a", 0.0)])
        comparison = baseline.compare(before, after, 2)

        criteria = comparison.criteria()
        self.assertTrue(any(not c.ok for c in criteria))  # 确实有判据不满足
        self.assertEqual([c.name for c in criteria if c.blocking], [])
        self.assertEqual(comparison.blocking_breaches, ())

        rendered = baseline.render(comparison)
        self.assertNotIn("[FAIL]", rendered)
        self.assertIn("no criterion blocks", rendered)

    def test_a_breach_still_becomes_a_verdict_once_a_criterion_is_armed(self) -> None:
        """The disarming is a statement about today's thresholds, not the removal
        of the mechanism. Without this, `blocking_breaches` is unreachable code
        and re-arming a calibrated criterion would silently do nothing."""
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        after = self.report([trial_record("a__9", "tb/a", 0.0),
                             trial_record("a__8", "tb/a", 0.0)])
        comparison = baseline.compare(before, after, 2)
        self.assertEqual(comparison.blocking_breaches, ())

        armed = baseline.Criterion("calibrated thing", "detail",
                                   ok=False, blocking=True)
        passing = baseline.Criterion("other thing", "detail",
                                     ok=True, blocking=True)
        with mock.patch.object(type(comparison), "criteria",
                               lambda self: (armed, passing)):
            self.assertEqual(comparison.blocking_breaches, (armed,))
        # And the render path calls it a FAIL rather than a warn.
        with mock.patch.object(type(comparison), "criteria",
                               lambda self: (armed,)):
            self.assertIn("[FAIL] calibrated thing", baseline.render(comparison))

    def test_calibration_measures_the_gate_it_is_calibrating(self) -> None:
        """Both arms are the same build, so every criterion that trips is a false
        block. The point of the test is the wiring: `calibrate` must read the
        criteria off `Comparison`, not off a second copy of the thresholds, or it
        would certify a gate that no longer exists."""
        frozen = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 0.0),
                              trial_record("b__1", "tb/b", 1.0),
                              trial_record("b__2", "tb/b", 1.0)])
        for method in ("resample", "beta"):
            fired, widths = calibrate.calibrate(
                frozen, k=2, rounds=10, method=method, seed=1)
            names = set(fired)
            self.assertIn("pass rate CI", names)
            self.assertIn("full flip", names)
            self.assertEqual(len(widths), 10)
            # A count, not a rate, and never more rounds than were run.
            self.assertTrue(all(0 <= n <= 10 for n in fired.values()))

    def test_calibration_never_invents_a_trial_name(self) -> None:
        """Duplicated trial names would make the synthetic run unreadable as a
        job, and the resample path draws with replacement."""
        frozen = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 0.0)])
        report = calibrate._candidate(frozen, 2, "resample", random.Random(3))
        names = [t.trial_name for t in report.trials]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(report.declared_trials, len(names))

    def test_a_changed_task_definition_refuses_comparison(self) -> None:
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 1.0)])
        before["tasks"]["tb/a"]["task_checksum"] = "deadbeefdeadbeef"
        after = self.report([trial_record("a__9", "tb/a", 1.0),
                             trial_record("a__8", "tb/a", 1.0)])
        with self.assertRaises(baseline.BaselineError) as caught:
            baseline.compare(before, after, 2)
        self.assertIn("task definition changed", str(caught.exception))

    def test_a_backfill_supersedes_one_task_and_says_where_it_came_from(self) -> None:
        """Correcting one task's definition does not invalidate the others'
        measurements, so the baseline is assembled from both runs — and records
        which run each task came from, because the mixture is a real difference."""
        original = self.report([
            trial_record("a__1", "tb/a", 1.0), trial_record("a__2", "tb/a", 1.0),
            trial_record("b__1", "tb/b", 0.0), trial_record("b__2", "tb/b", None),
        ])
        backfill = self.report([
            trial_record("b__9", "tb/b", 0.0), trial_record("b__8", "tb/b", 1.0),
        ])
        frozen = baseline.build(
            [("run-1", original), ("backfill", backfill)],
            corpus_pin="pin", run_id="backfill", k=2,
            model=MODEL_IDENTITY, subject=None, harness=HARNESS, params={},
            notes="tb/b re-run after its image was corrected",
        )
        self.assertEqual(frozen["tasks"]["tb/a"]["source_run_id"], "run-1")
        self.assertEqual(frozen["tasks"]["tb/b"]["source_run_id"], "backfill")
        # The superseded, unscored trial is gone rather than merged alongside.
        self.assertEqual(frozen["tasks"]["tb/b"]["passes"], 1)
        self.assertEqual([s["run_id"] for s in frozen["sources"]],
                         ["run-1", "backfill"])
        self.assertIn("image was corrected", frozen["notes"])

    def test_a_backfill_that_does_not_fix_the_task_is_still_refused(self) -> None:
        original = self.report([
            trial_record("a__1", "tb/a", 1.0), trial_record("a__2", "tb/a", 1.0),
            trial_record("b__1", "tb/b", None), trial_record("b__2", "tb/b", None),
        ])
        backfill = self.report([
            trial_record("b__9", "tb/b", None), trial_record("b__8", "tb/b", 0.0),
        ])
        with self.assertRaises(baseline.BaselineError) as caught:
            baseline.build([("run-1", original), ("backfill", backfill)],
                           corpus_pin="pin", run_id="backfill", k=2,
                           model=MODEL_IDENTITY, subject=None, harness=HARNESS,
                           params={})
        # Named by the run that actually produced the bad data, not the first one.
        self.assertIn("from backfill", str(caught.exception))

    def test_the_interval_is_reproducible(self) -> None:
        """A gate whose number moves between two reads of the same data stops
        being believed."""
        before = self.frozen([trial_record("a__1", "tb/a", 1.0),
                              trial_record("a__2", "tb/a", 0.0)])
        after = self.report([trial_record("a__9", "tb/a", 1.0),
                             trial_record("a__8", "tb/a", 1.0)])
        first = baseline.compare(before, after, 2).pass_rate_ci
        second = baseline.compare(before, after, 2).pass_rate_ci
        self.assertEqual(first, second)


class DimEventTotalsTest(unittest.TestCase):
    """When a trial's Timeline export may be read as a full accounting.

    A trial whose in-container guard fired at the agent deadline is killed
    mid-run and reports no usage at all. Read as complete, its zero entered the
    cost comparison as a measurement: the frozen baseline then carried that zero
    as the task's cost, and every later run of a task that actually spends
    tokens read as a cost explosion.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def export(self, branches: dict[str, list[tuple[str, dict]]],
               finished: bool = True) -> Path:
        """A `dimcode export` file: branches, their events, and the end line."""
        lines = [{"kind": "header", "export": 1, "session_id": "s",
                  "store_format": 20, "sensitive": True}]
        for branch in branches:
            info = {"id": branch} if branch == "main" else {"id": branch, "parent": "main"}
            lines.append({"kind": "branch", "info": info, "head": len(branches[branch])})
        count = 0
        for branch, events in branches.items():
            for name, data in events:
                count += 1
                kind, _, event = name.partition("/")
                lines.append({"kind": "event", "branch_id": branch, "trace": None,
                              "event": {"type": kind, "name": event, "data": data}})
        if finished:
            lines.append({"kind": "end", "branches": len(branches), "events": count})
        path = self.root / f"export{len(list(self.root.iterdir()))}.jsonl"
        path.write_text("".join(json.dumps(line) + "\n" for line in lines))
        return path

    @staticmethod
    def usage(prompt: int, cached: int, completion: int) -> tuple[str, dict]:
        return ("model/usage", {"usage": {"promptTokens": float(prompt),
                                          "cacheReadTokens": float(cached),
                                          "completionTokens": float(completion),
                                          "totalTokens": float(prompt + completion)}})

    def test_a_settled_run_is_complete(self) -> None:
        totals = dim_events.read_totals(self.export({"main": [
            self.usage(600, 200, 10),
            ("step/tool-call", {"calls": [{"id": "a", "name": "read", "input": {}},
                                          {"id": "b", "name": "read", "input": {}}]}),
            self.usage(400, 0, 20),
            ("turn/end", {"status": "completed", "reason": "end_turn"}),
        ]}))
        self.assertTrue(totals.usage_complete)
        self.assertEqual(totals.n_input_tokens, 1000)
        self.assertEqual(totals.uncached_input_tokens, 800)
        self.assertEqual(totals.n_output_tokens, 30)
        self.assertEqual(totals.turns, 2)
        self.assertEqual(totals.tool_calls, 2)
        self.assertEqual(totals.end_reason, "end_turn")

    def test_a_killed_run_is_not_complete(self) -> None:
        """No `turn/end` means the run never settled: its usage is unknown."""
        totals = dim_events.read_totals(self.export({"main": [self.usage(500, 0, 5)]}))
        self.assertFalse(totals.usage_complete)
        self.assertIsNone(totals.end_reason)

    def test_a_truncated_export_is_not_complete(self) -> None:
        totals = dim_events.read_totals(self.export(
            {"main": [self.usage(500, 0, 5), ("turn/end", {"reason": "end_turn"})]},
            finished=False))
        self.assertFalse(totals.usage_complete)

    def test_a_second_turn_left_running_is_not_complete(self) -> None:
        totals = dim_events.read_totals(self.export({"main": [
            ("turn/start", {"turn_id": "t1"}), self.usage(500, 0, 5),
            ("turn/end", {"turn_id": "t1", "reason": "end_turn"}),
            ("turn/start", {"turn_id": "t2"}), self.usage(300, 0, 5),
        ]}))
        self.assertFalse(totals.usage_complete)

    def test_subagent_usage_is_counted_with_the_parent(self) -> None:
        """Child branches are in the export, so their calls are in the totals."""
        totals = dim_events.read_totals(self.export({
            "main": [self.usage(1000, 0, 10), ("turn/end", {"reason": "end_turn"})],
            "child": [self.usage(300, 0, 7)],
        }))
        self.assertTrue(totals.usage_complete)
        self.assertEqual(totals.subagent_runs, 1)
        self.assertEqual(totals.n_input_tokens, 1300)
        self.assertEqual(totals.turns, 1)

    def test_no_stream_reads_as_nothing(self) -> None:
        self.assertIsNone(dim_events.read_totals(self.root / "absent.jsonl"))


if __name__ == "__main__":
    unittest.main()
