"""Daily pipeline flow tests.

What these protect is the window: which merges a run is responsible for, and
when that responsibility is allowed to move forward. Everything else in
daily_run is delegation to pieces with their own tests, but the window is the
one thing that, when wrong, loses coverage silently — the run still succeeds,
the report still looks normal, and a regression lands attributed to nothing.

The build and the remote L2 are stubbed. They need a Linux toolchain and the
eval machine; what is under test here is the decision to call them at all and
what happens to the state afterwards.

Run: python3 -m unittest discover -s benchmarks -p 'daily_flow_test.py'
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import daily_run  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent / "build"))
import daily_batch  # noqa: E402

#: Reads the diff off stdin, answers for every file it names. The paths come from
#: the diff itself because the reviewer contract is exactly that: one verdict per
#: changed file, no more and no fewer.
FAKE_REVIEWER = textwrap.dedent(
    """
    import json, sys
    mode = sys.argv[1]
    if mode == "crash":
        sys.stderr.write("judge unreachable")
        sys.exit(3)
    paths = []
    for line in sys.stdin.read().splitlines():
        if line.startswith("diff --git a/"):
            paths.append(line.split(" b/", 1)[1])
    for path in paths:
        record = {"file": path, "verdict": mode, "trace": "test"}
        if mode == "visible":
            record["axes"] = ["content"]
        print(json.dumps(record))
    """
).strip()


#: What run_l2 captures from the endpoint and leaves beside the results, and
#: what a baseline has to record: the identity guard holds every later run's
#: endpoint to it, so a reference frozen without one disarms the guard.
MODEL_IDENTITY = {
    "provider": "icecn",
    "model": "glm-5.3",
    "model_reported": "glm-5.3",
    "base_url": "https://example.invalid/v1",
    "captured_at": "2026-09-14T07:19:02+00:00",
}


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=True,
    ).stdout.strip()


def write_harbor_job(job: Path, k: int, unscored: int = 0) -> None:
    """A minimal, scoreable harbor job: one task run k times, all passing.

    `unscored` drops the verifier result from that many trials, which is what a
    run looks like when the agent finished and nothing graded it — the shape the
    real 2026-09-14 baseline came back in, and the one the window must not be
    advanced over.
    """
    trials = [
        {
            "trial_name": f"tb-a__{index}",
            "task_name": "tb/a",
            "task_checksum": "checksum-a",
            "agent_info": {"name": "dim", "version": "0.5.1"},
            "agent_result": {
                "n_input_tokens": 1000, "n_output_tokens": 100,
                "n_cache_tokens": 0,
                "metadata": {"dim_turns": 9, "dim_usage_complete": True},
            },
            "verifier_result": {"rewards": {"reward": 1.0}},
            "agent_execution": {
                "started_at": "2026-09-14T03:10:13.067022Z",
                "finished_at": "2026-09-14T03:18:42.779797Z",
            },
        }
        for index in range(k)
    ]
    job.mkdir(parents=True, exist_ok=True)
    (job / "result.json").write_text(json.dumps({
        "id": "job-uuid",
        "n_total_trials": len(trials),
        "stats": {"evals": {"dim__glm-5.3__tasks": {
            "reward_stats": {"reward": {"1.0": [t["trial_name"] for t in trials]}},
            "exception_stats": {},
        }}},
    }))
    for index, record in enumerate(trials):
        if index < unscored:
            record = {key: value for key, value in record.items()
                      if key != "verifier_result"}
        directory = job / record["trial_name"]
        directory.mkdir()
        (directory / "result.json").write_text(json.dumps(record))


class DailyFlowTest(unittest.TestCase):
    """End-to-end runs over a real git repository and a faked eval machine.

    Most of these expect `EXIT_NOT_COMPARABLE` from a run that settled, which
    reads oddly until you notice what the fixture does not do: it never freezes
    a baseline. A run with nothing to compare against reaches no conclusion and
    says so, and that is orthogonal to what these tests are about — whether the
    window moved, what the ledger holds, which image was built. Tests that care
    about the comparison take a baseline first, and then expect 0.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "t@example.com")
        git(self.repo, "config", "user.name", "t")
        self._commit("seed.txt", "seed\n", "seed")
        self.base = git(self.repo, "rev-parse", "HEAD")

        self.reviewer = self.root / "fake_reviewer.py"
        self.reviewer.write_text(FAKE_REVIEWER)
        self.prompt = self.root / "prompt.md"
        self.prompt.write_text("judge the diff")
        self.state = self.root / "state.json"
        self.out = self.root / "out"
        self.baselines = self.root / "baselines"
        self.ledger = self.root / "ledger"

        # The harness's own checkout, stubbed. `runner_dirty` is read off it, and
        # reading it off the real repository made every test in this file depend
        # on whether the developer happened to have saved a file — green on CI,
        # red on a laptop mid-edit, for reasons having nothing to do with the
        # behaviour under test.
        self.tooling = self.root / "tooling"
        (self.tooling / "benchmarks").mkdir(parents=True)
        git(self.tooling, "init", "-q", "-b", "main")
        git(self.tooling, "config", "user.email", "t@example.com")
        git(self.tooling, "config", "user.name", "t")
        (self.tooling / "benchmarks" / "daily_run.py").write_text("harness\n")
        git(self.tooling, "add", "-A")
        git(self.tooling, "commit", "-qm", "harness")

        self.built: list[str] = []
        self.ran: list[tuple[str, int]] = []
        self.concurrency: list[int] = []
        self.fetched: list[str] = []

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # --- helpers ---------------------------------------------------------

    def _commit(self, name: str, body: str, message: str) -> None:
        (self.repo / name).write_text(body)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", message)

    def _merge(self, branch: str, name: str, body: str) -> str:
        """Land a change the way an MR does: a branch merged with --no-ff."""
        git(self.repo, "checkout", "-q", "-b", branch)
        self._commit(name, body, f"{branch}: change {name}")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "-m", f"Merge branch '{branch}'", branch)
        return git(self.repo, "rev-parse", "HEAD")

    def _run(self, mode: str = "invisible", l2_exit: int = 0,
             run_type: str = "daily", extra: list[str] | None = None,
             unscored: int = 0, fetch_error: str | None = None,
             dirty: bool = False, no_model: bool = False) -> int:
        if dirty:
            (self.tooling / "benchmarks" / "scratch.py").write_text("edit\n")
        k = {"daily": 1, "checkpoint": 3, "baseline": 3}[run_type]
        argv = [
            "daily_run.py",
            "--repo", str(self.repo),
            "--run-type", run_type,
            "--head", "main",
            "--state", str(self.state),
            "--out", str(self.out),
            "--baselines-dir", str(self.baselines),
            "--ledger-dir", str(self.ledger),
            "--host", "eval@host", "--scratch", "/scratch",
            "--remote-python", "/venv/bin/python",
            "--prompt", str(self.prompt),
            "--cache-dir", str(self.root / "cache"),
            "--no-fetch",
            *(extra or []),
        ]
        command = (sys.executable, str(self.reviewer), mode)

        def fake_build(repo, head, staging, host, scratch, tag_prefix):
            self.built.append(head)
            return f"{tag_prefix}:{head[:9]}", {"commit": head}

        def fake_run_l2(host, remote, tag, run_id, k, timeout_sec,
                        baseline=None, python=None, *, concurrency):
            self.ran.append((tag, k))
            self.concurrency.append(concurrency)
            return l2_exit, f"{remote}/results"

        def fake_fetch(host, remote_results, run_id, out):
            """Stand in for the rsync by writing the job harbor would have left,
            so the reading and freezing that follow are exercised too.

            Mirrors `--delete`: the local directory ends up holding this run's
            job and nothing else. Returns None, the way a fetch that worked does.

            The model identity comes back beside the job, because that is what
            the real fetch does — run_l2 captures it from the endpoint and rsync
            brings it along. `no_model` is the case where it does not.
            """
            self.fetched.append(run_id)
            if fetch_error is not None:
                return fetch_error
            harbor = Path(out) / "harbor"
            if harbor.exists():
                shutil.rmtree(harbor)
            write_harbor_job(harbor, k, unscored=unscored)
            if not no_model:
                (Path(out) / f"{run_id}-model.json").write_text(
                    json.dumps(MODEL_IDENTITY))
            return None

        with mock.patch.object(sys, "argv", argv), \
             mock.patch.object(daily_run, "TOOLING_ROOT", self.tooling), \
             mock.patch.object(daily_batch, "REVIEWER_COMMAND", command), \
             mock.patch.object(daily_run, "build_subject", fake_build), \
             mock.patch.object(daily_run, "deliver_runner", lambda *a: "/remote/eval"), \
             mock.patch.object(daily_run, "run_l2_remote", fake_run_l2), \
             mock.patch.object(daily_run, "remote_harbor_version", lambda *a: "0.18.0"), \
             mock.patch.object(daily_run, "fetch_results", fake_fetch):
            return daily_run.main()

    def _state_head(self) -> str | None:
        if not self.state.exists():
            return None
        return json.loads(self.state.read_text())["last_l2_head"]

    def _manifest(self) -> dict:
        found = sorted(self.out.glob("*/run.json"))
        self.assertEqual(len(found), 1, f"expected one manifest, got {found}")
        return json.loads(found[0].read_text())

    # --- window ----------------------------------------------------------

    def test_first_run_without_a_base_refuses_to_guess(self) -> None:
        self._merge("feat", "a.txt", "a\n")
        with self.assertRaises(SystemExit) as caught:
            self._run()
        self.assertIn("no window base", str(caught.exception))

    def test_nothing_new_does_not_build(self) -> None:
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(), 0)
        self.assertEqual(self.built, [])
        self.assertEqual(self.ran, [])

    def test_rewritten_history_is_refused(self) -> None:
        """A base that is no longer an ancestor describes a range that never was."""
        self._merge("feat", "a.txt", "a\n")
        orphan = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "reset", "-q", "--hard", self.base)
        self._merge("other", "b.txt", "b\n")
        self.state.write_text(json.dumps({"last_l2_head": orphan}))
        with self.assertRaises(SystemExit) as caught:
            self._run()
        self.assertIn("not an ancestor", str(caught.exception))

    # --- advancing -------------------------------------------------------

    def test_invisible_batch_closes_the_window_without_running_l2(self) -> None:
        head = self._merge("docs", "notes.md", "notes\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="invisible"), 0)
        self.assertEqual(self.built, [])
        self.assertEqual(self.ran, [])
        self.assertEqual(self._state_head(), head)

    def test_visible_batch_builds_the_head_and_advances(self) -> None:
        self._merge("one", "a.txt", "a\n")
        head = self._merge("two", "b.txt", "b\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.built, [head])
        self.assertEqual(self.ran, [(f"dim-agent-under-test:{head[:9]}", 1)])
        self.assertEqual(self._state_head(), head)

        manifest = self._manifest()
        self.assertEqual(manifest["window"], {"base": self.base, "head": head})
        self.assertEqual(len(manifest["merges"]), 2)
        self.assertTrue(all(m["triggers_l2"] for m in manifest["merges"]))

    def test_failed_l2_holds_the_window_and_still_records_the_attempt(self) -> None:
        """The next run must still be responsible for these merges — and the
        ledger must say this one tried.

        It used to record nothing at all: the run record was written only when
        there was a summary to write it from. Months later the window then read
        as one L2 never looked at, which is the shape of an L1 false negative, so
        a crashed harbor was filed as L1 waving merges through. The exit code
        goes in with it, because that is the first thing anyone diagnosing it
        wants and the artifacts holding the detail expire in 30 days.
        """
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible", l2_exit=1), daily_run.EXIT_L2_FAILED)
        self.assertEqual(self._state_head(), self.base)

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.outcome, "invalid")
        self.assertEqual(recorded.invalid_reason, "l2_failed")
        self.assertEqual(recorded.l2_exit_code, 1)
        # Nothing was measured, so it covers nothing: the merges in this window
        # stay in the denominator L1 is held to account against.
        self.assertFalse(recorded.measured)

    def test_a_failed_l2_still_has_its_results_fetched(self) -> None:
        """A non-zero harbor is when the evidence matters most, and it was the
        one case that had none.

        The fetch used to be reached only on exit 0, so a failed run left an
        artifact holding an exit code and nothing to explain it — no partial
        trials, no trajectories. Exit 124 is the sharpest version: ssh gave up on
        a run still writing on the eval machine, and the recovery starts with
        looking at what it has written so far.

        Scoring is still gated on exit 0. The fetch has two uses and only one of
        them needs a whole job.
        """
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))

        self.assertEqual(self._run(mode="visible", l2_exit=124),
                         daily_run.EXIT_L2_FAILED)
        self.assertEqual(len(self.fetched), 1)
        self.assertTrue((self.out / self.fetched[0] / "harbor").is_dir())

        manifest = self._manifest()
        self.assertIsNone(manifest["fetch_error"])
        # Fetched, not scored: exit 124 says nothing about this job adding up.
        self.assertIsNone(manifest["l2"])
        self.assertEqual(manifest["l2_exit_code"], 124)

    def test_a_fetch_that_fails_is_recorded_not_raised(self) -> None:
        """The fetch reports instead of raising, and the reason is the manifest.

        A raise here would land before the manifest write and take down the
        record of a run that already spent its hours — the same loss
        `settle_error` was added to prevent, one step earlier. On a run that
        exited 0 there is now nothing to score and nothing to score it from, so
        it is the same situation from here on: recorded, window held, re-run.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        self.assertEqual(
            self._run(mode="visible", run_type="checkpoint",
                      fetch_error="fetch failed: rsync exited 23"),
            daily_run.EXIT_INVALID,
        )

        self.assertEqual(self._state_head(), self.base)
        manifest = self._manifest_for(head)
        self.assertIn("rsync exited 23", manifest["fetch_error"])
        self.assertIn("rsync exited 23", manifest["settle_error"])

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.invalid_reason, "settle_failed")
        self.assertFalse(recorded.measured)

    def test_a_judge_that_decided_nothing_stops_the_run(self) -> None:
        """Not one verdict came back, so L2 would spend hours measuring a window
        nothing judged. Reporting that as the ordinary undetermined warning is
        how a stalled pipeline stays green."""
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_JUDGE_DOWN)
        self.assertEqual(self.ran, [])
        self.assertEqual(self.built, [])
        self.assertEqual(self._state_head(), self.base)
        self.assertEqual(self._manifest()["merges"][0]["trigger"], "fail_open")

    def test_one_undecided_merge_still_runs_l2_and_holds_the_window(self) -> None:
        """A single merge the judge missed is not an outage. L2 runs to stay
        safe, and the window is held so that merge is judged again."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        # One merge decided by a path rule, one the judge crashes on.
        self._merge("bump", "Cargo.lock", "lock\n")
        head = self._merge("feat", "a.txt", "a\n")
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_UNDETERMINED)
        self.assertEqual(self.built, [head])
        self.assertEqual(self._state_head(), self.base)
        triggers = {m["trigger"] for m in self._manifest()["merges"]}
        self.assertEqual(triggers, {"path_rule", "fail_open"})

    def test_a_verdict_replaces_the_fail_open_record_it_was_waiting_for(self) -> None:
        """fail_open holds no verdict, so the one the judge reaches later is not
        a second opinion. Filing it as a dispute beside the kept placeholder left
        the merge undecided in the ledger for good, and attribution could never
        learn what L1 thought of it."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._merge("bump", "Cargo.lock", "lock\n")
        head = self._merge("feat", "a.txt", "a\n")
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_UNDETERMINED)

        import ledger_tools
        ledger = ledger_tools.Ledger(self.ledger)
        self.assertEqual(ledger.merge(head).l1_trigger, "fail_open")

        # Judged again because the window was held, and still no answer: the
        # placeholder stays and nothing is disputed.
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_UNDETERMINED)
        self.assertEqual(ledger.merge(head).l1_trigger, "fail_open")
        self.assertEqual(ledger.runs()[-1].disputed, ())

        # The judge is back.
        self.assertEqual(self._run(mode="invisible"), daily_run.EXIT_NOT_COMPARABLE)
        replaced = ledger.merge(head)
        self.assertEqual((replaced.l1_verdict, replaced.l1_trigger), ("invisible", "agent"))
        self.assertEqual(ledger.runs()[-1].disputed, ())
        self.assertEqual(self._state_head(), head)
        self.assertEqual(ledger_tools.audit(self.ledger), [])

        # A verdict, once recorded, is still never rewritten.
        with self.assertRaises(ledger_tools.LedgerError):
            ledger.replace_fail_open(replaced)

    def test_window_spans_merges_the_previous_run_could_not_settle(self) -> None:
        """Yesterday's L2 failed, so today answers for both days."""
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(mode="visible", l2_exit=1)

        head = self._merge("two", "b.txt", "b\n")
        self.built.clear()
        self.ran.clear()
        self.assertEqual(self._run(mode="visible"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.built, [head])
        self.assertEqual(len(self._manifest_for(head)["merges"]), 2)
        self.assertEqual(self._state_head(), head)

    def _manifest_for(self, head: str) -> dict:
        # The head is a component of the run id, not its tail: a unique token
        # follows it so that a re-run of the same commit gets its own directory.
        found = sorted(self.out.glob(f"*__{head[:9]}__*/run.json"))
        self.assertEqual(len(found), 1, f"expected one manifest, got {found}")
        return json.loads(found[0].read_text())

    def test_direct_push_runs_l2_and_still_closes_the_window(self) -> None:
        """A commit with no MR has nothing to judge, so it is evaluated rather than
        skipped — but it cannot hold the window, or the window never moves again."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._commit("hotfix.txt", "hotfix\n", "chore: straight to main")
        head = git(self.repo, "rev-parse", "HEAD")

        self.assertEqual(self._run(mode="invisible"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.built, [head])
        self.assertEqual(self._state_head(), head)

        manifest = self._manifest()
        self.assertEqual(manifest["merges"], [])
        self.assertEqual([c["sha"] for c in manifest["direct_pushes"]], [head])

    def test_the_ledger_records_every_merge_and_what_the_run_answered_for(self) -> None:
        head = self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible"), daily_run.EXIT_NOT_COMPARABLE)

        import sys as _sys
        _sys.path.insert(0, str(Path(daily_run.__file__).parent))
        import ledger_tools

        ledger = ledger_tools.Ledger(self.ledger)
        merges = ledger.merges()
        self.assertEqual([m.merge_sha for m in merges], [head])
        self.assertEqual(merges[0].l1_verdict, "visible")

        runs = ledger.runs()
        self.assertEqual(len(runs), 1)
        # k=1 daily: it ran, it did not collapse, and it is not a verdict.
        self.assertEqual(runs[0].outcome, "screen")
        self.assertFalse(runs[0].conclusive)
        self.assertEqual(runs[0].covers, (head,))
        self.assertEqual(audit_findings := ledger_tools.audit(self.ledger), [])
        del audit_findings

    def test_a_merge_judged_differently_the_second_time_is_disputed(self) -> None:
        """The recorded verdict stands and the disagreement goes on the run.

        The comparison used to be the verdict alone, so a merge could be
        re-judged under a different policy, prompt or model, land on the same
        verdict for entirely different reasons, and leave nothing saying the
        question had changed. `l1_identity` covers all of those, so it is what
        catches it.

        Recorded, not refused. The identity covers the policy file's digest and
        the judge's model, so one ordinary policy edit makes every merge still in
        the window disagree at once — raising would wedge the pipeline on a
        legitimate change, against an append-only store with no way to clear it.
        The disputed shas outlive the artifacts that hold the detail.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")
        self.assertEqual(self._run(mode="visible", l2_exit=1),
                         daily_run.EXIT_L2_FAILED)

        import ledger_tools
        ledger = ledger_tools.Ledger(self.ledger)
        first = ledger.merges()[0]
        self.assertEqual(first.l1_verdict, "visible")

        # The window is held, so the same merge is judged again — this time with
        # an edited prompt. The prompt is part of the decision identity, so the
        # verdict cache misses and the judge is really asked again; it answers
        # `visible` as before. Same answer, different question, which is exactly
        # the case a verdict-only comparison could not see. In the wild the edit
        # is to the policy file or the judge's model.
        self.prompt.write_text("judge the diff, but more carefully")
        self.assertEqual(self._run(mode="visible", l2_exit=1),
                         daily_run.EXIT_L2_FAILED)

        # The record is untouched: the store is write-once, and a silent
        # overwrite would erase the evidence that this happened.
        self.assertEqual(ledger.merges()[0].l1_verdict, "visible")
        self.assertEqual(ledger.merges()[0].l1_identity, first.l1_identity)

        second = ledger.runs()[-1]
        self.assertEqual(second.disputed, (head,))
        self.assertEqual(ledger.runs()[0].disputed, ())
        self.assertEqual(ledger_tools.audit(self.ledger), [])

    def test_a_direct_push_is_recorded_as_unjudged_not_as_covered(self) -> None:
        """It has no MR to judge, so it has no L1 record — but the run did answer
        for its code, and the ledger has to say so."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._commit("hotfix.txt", "hotfix\n", "chore: straight to main")
        head = git(self.repo, "rev-parse", "HEAD")
        self.assertEqual(self._run(mode="invisible"), daily_run.EXIT_NOT_COMPARABLE)

        import sys as _sys
        _sys.path.insert(0, str(Path(daily_run.__file__).parent))
        import ledger_tools

        ledger = ledger_tools.Ledger(self.ledger)
        self.assertEqual(ledger.merges(), [])
        run = ledger.runs()[0]
        self.assertEqual(run.covers, ())
        self.assertEqual(run.unjudged, (head,))
        self.assertEqual(ledger_tools.audit(self.ledger), [])

    def test_a_checkpoint_that_matches_its_baseline_is_recorded_as_pass(self) -> None:
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(run_type="baseline")
        head = self._merge("one", "a.txt", "a\n")
        self.assertEqual(self._run(mode="visible", run_type="checkpoint"), 0)

        import sys as _sys
        _sys.path.insert(0, str(Path(daily_run.__file__).parent))
        import ledger_tools

        runs = {r.run_type: r for r in ledger_tools.Ledger(self.ledger).runs()}
        self.assertEqual(runs["checkpoint"].outcome, "pass")
        self.assertTrue(runs["checkpoint"].conclusive)
        self.assertEqual(runs["checkpoint"].covers, (head,))

    def test_a_scoring_failure_still_leaves_the_evidence_behind(self) -> None:
        """Reading the job can fail in ways this harness does not enumerate — the
        files come from another machine and are compared against a stored
        reference. Every one of those used to propagate out of `main`, past the
        manifest write, so hours of machine time left nothing at all: no
        run.json, no ledger record, only a traceback.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        with mock.patch.object(daily_run, "settle",
                               side_effect=RuntimeError("harbor wrote nonsense")):
            exit_code = self._run(mode="visible", run_type="checkpoint")

        self.assertEqual(exit_code, daily_run.EXIT_INVALID)
        self.assertEqual(self._state_head(), self.base)   # 窗口不动
        self.assertEqual(self.built, [head])              # L2 确实跑过了

        manifest = self._manifest_for(head)
        self.assertIn("harbor wrote nonsense", manifest["settle_error"])
        self.assertIsNone(manifest["l2"])
        # 判定与窗口都在，只是没有分数——重跑要用的东西一样不少。
        self.assertEqual(manifest["merges"][0]["sha"], head)
        self.assertEqual(manifest["l2_exit_code"], 0)

        # And the ledger records the attempt, with what scoring raised. The
        # manifest is an artifact that expires; this is the part that does not.
        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.outcome, "invalid")
        self.assertEqual(recorded.invalid_reason, "settle_failed")
        self.assertIn("harbor wrote nonsense", recorded.settle_error)
        self.assertFalse(recorded.measured)

    def test_a_dirty_harness_is_refused_before_anything_is_spent(self) -> None:
        """The harness must come from main, or a change can alter the ruler that
        measures it.

        This used to be a warning, and a warning protected nothing: the run went
        on to judge the window, build the image, spend its hours, freeze a
        baseline and write the ledger — all measured by code that exists on one
        machine and in no commit. Recorded-as-dirty-but-used-anyway is not a
        trusted evaluator.

        Refused before the judge is paid, not after.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._merge("feat", "app.ts", "export const a = 1\n")

        with self.assertRaises(SystemExit) as caught:
            self._run(mode="visible", dirty=True)
        self.assertIn("--allow-dirty", str(caught.exception))
        self.assertEqual(self.built, [])
        self.assertEqual(self.ran, [])
        self.assertEqual(self._state_head(), self.base)

    def test_a_dirty_harness_may_never_freeze_a_baseline(self) -> None:
        """`--allow-dirty` covers experiments and does not reach this.

        A baseline is the reference every later comparison is read against, so
        defining it from uncommitted code makes all of them untraceable.
        Committing first costs nothing: taking a baseline is already a reviewed,
        deliberate act rather than something a schedule does.
        """
        with self.assertRaises(SystemExit) as caught:
            self._run(mode="visible", run_type="baseline", dirty=True,
                      extra=["--allow-dirty"])
        self.assertIn("Commit first", str(caught.exception))
        self.assertEqual(self.built, [])
        self.assertEqual(list(self.baselines.glob("*.json")), [])

    def test_an_allowed_dirty_run_says_so_in_the_ledger(self) -> None:
        """The flag lets the run happen; it does not launder it.

        The record is what someone reads months later to decide whether a number
        can be trusted, so it has to carry the one fact that says it cannot be
        reproduced from any commit.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        self.assertEqual(
            self._run(mode="visible", dirty=True, extra=["--allow-dirty"]),
            daily_run.EXIT_NOT_COMPARABLE,
        )
        self.assertEqual(self.built, [head])

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertTrue(recorded.runner_dirty)
        self.assertTrue(self._manifest()["harness"]["dirty"])

    def test_an_existing_baseline_is_refused_before_anything_is_built(self) -> None:
        """The answer does not depend on the run, so paying for the run to reach
        it was pure loss: the refusal used to come from inside `settle`, after
        the image was built and L2 had spent its 2-7 hours."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(run_type="baseline"), 0)
        built_once = list(self.built)

        with self.assertRaises(SystemExit) as caught:
            self._run(run_type="baseline")
        self.assertIn("already exists", str(caught.exception))
        self.assertIn("in its own commit", str(caught.exception))

        self.assertEqual(self.built, built_once)   # 没有再构建
        self.assertEqual(len(self.ran), 1)         # 也没有再跑 L2

    def test_a_run_nothing_could_score_holds_the_window_and_goes_red(self) -> None:
        """harbor exiting 0 says the job ran, not that it can be scored.

        This is not hypothetical: the real 2026-09-14 run came back with three
        trials the agent finished and nothing graded. It survived only because it
        was a `baseline` run, which judges no window — the same result on a daily
        or checkpoint run advanced the window past a batch nothing measured, wrote
        the ledger record `invalid`, and returned 0 over it.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        exit_code = self._run(mode="visible", run_type="checkpoint", unscored=1)

        self.assertEqual(exit_code, daily_run.EXIT_INVALID)
        # The batch is unmeasured, so it stays in the next run's range.
        self.assertEqual(self._state_head(), self.base)
        # L2 did run — this is not a failure to execute, it is a failure to score.
        self.assertEqual(self.built, [head])
        self.assertEqual(len(self.ran), 1)

        summary = self._manifest()["l2"]
        self.assertTrue(summary["unscoreable"])
        self.assertIn("never scored", summary["unscoreable"][0])

        # And the ledger says the same thing the exit code does.
        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.outcome, "invalid")

    def test_an_invalid_run_can_be_re_run_the_same_day(self) -> None:
        """EXIT_INVALID asks for a re-run, so the re-run has to be possible.

        A run id of (date, head) made it not be. Both runs would address the same
        remote results directory, and the fetch is an rsync without --delete, so
        the second read back its own trials plus the first's — declared 3, found
        6, invalid again for reasons inherited from the run it was replacing. The
        ledger's write-once would have refused its record on top of that. The
        window then could not move until the date rolled over.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        first = self._run(mode="visible", run_type="checkpoint", unscored=1)
        self.assertEqual(first, daily_run.EXIT_INVALID)
        self.assertEqual(self._state_head(), self.base)

        # Same day, same commit, this time the job comes back whole. The fixture
        # takes no baseline, so a run that settles reports NOT_COMPARABLE rather
        # than 0 — what matters here is that it is no longer INVALID and the
        # window moved.
        second = self._run(mode="visible", run_type="checkpoint", unscored=0)
        self.assertEqual(second, daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self._state_head(), head)

        import ledger_tools
        runs = ledger_tools.Ledger(self.ledger).runs()
        # Two records, not one refused write: the invalid attempt is part of the
        # history, and the run that settled the window is a different run.
        self.assertEqual(len(runs), 2)
        self.assertEqual([r.outcome for r in runs], ["invalid", "invalid"])
        self.assertEqual(len({r.run_id for r in runs}), 2)

    def test_an_unscoreable_screen_is_recorded_invalid_not_as_a_screen(self) -> None:
        """A screen that could not be scored did not screen anything.

        `screen` claims a measurement exists and reached no conclusion. A job
        nothing could score has no measurement in it, so at k=1 it is `invalid`
        for the same reason it is at k=3. It used to be recorded `screen`, because
        the schema barred every other outcome below k=3 — the pipeline printed
        "run recorded invalid" one line after writing a screen, and the ledger is
        write-once, so the wrong one was the permanent one.

        The window is held on the measurement, not on the outcome: a covered merge
        leaves the denominator L1's false-negative rate is measured against, and
        covering a batch nothing measured is the same lie at k=1 as at k=3.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._merge("feat", "app.ts", "export const a = 1\n")

        self.assertEqual(self._run(mode="visible", unscored=1),
                         daily_run.EXIT_INVALID)
        self.assertEqual(self._state_head(), self.base)

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.outcome, "invalid")
        self.assertEqual(recorded.invalid_reason, "unscoreable")
        self.assertFalse(recorded.measured)
        self.assertFalse(recorded.conclusive)

    def test_a_missing_baseline_does_not_hold_the_window_or_spoil_the_screen(self) -> None:
        """Not every entry in `problems` is a broken measurement.

        "No baseline to compare against" bars a conclusion the same way, but the
        run measured fine and running it again cannot produce a baseline — only a
        person can. Holding for it would stall the window until someone noticed,
        on a condition no re-run ever clears, so the two are kept apart.

        The same distinction decides the ledger outcome. This is k=1, and a
        screen never compares, so having nothing to compare against is the screen
        working. Keying the `invalid` exemption on all of `problems` rather than
        on `unscoreable` would have filed every night of the whole pre-baseline
        period as a failure.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feat", "app.ts", "export const a = 1\n")

        self.assertEqual(self._run(mode="visible"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self._state_head(), head)

        summary = self._manifest()["l2"]
        self.assertEqual(summary["unscoreable"], [])
        self.assertIn("no baseline", summary["problems"][0])

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.outcome, "screen")
        self.assertTrue(recorded.measured)

    def test_a_diff_too_large_to_judge_is_decided_not_held(self) -> None:
        """A generated one-line file makes a diff the judge cannot be shown at all.
        Untraceable resolves to visible — but as a decision, so the window still
        moves; holding it would stall the pipeline on every catalog update,
        forever, the same way a permanently unscorable task stalls a baseline."""
        import sys as _sys
        _sys.path.insert(0, str(Path(daily_run.__file__).parent / "build"))
        import daily_batch

        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        oversized = "x" * (daily_batch.MAX_JUDGED_DIFF_BYTES + 1)
        head = self._merge("generated", "catalog.json", oversized)

        # mode="crash" would fail-open if the judge were consulted at all.
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.built, [head])
        self.assertEqual(self._state_head(), head)

        merge = self._manifest()["merges"][0]
        self.assertEqual((merge["verdict"], merge["trigger"]), ("visible", "oversize"))

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).merges()[0]
        self.assertEqual(recorded.l1_trigger, "oversize")
        self.assertIn("over the", recorded.l1_reason)

    def test_an_unreadable_policy_fails_the_batch_open_not_over(self) -> None:
        """L1 may only add L2 runs, never withhold one — including when L1 itself
        is what broke. The batch path used to raise straight out of the run here,
        so a typo in the policy file took the whole pipeline down with a
        traceback. It now decides every merge `fail_open` — and because *every*
        merge came back undecided, the run stops before L2 rather than spending
        hours measuring a window nothing judged. The window is held either way,
        so the merges are judged again once the file is fixed.
        """
        broken = self.root / "broken-policy.toml"
        broken.write_text("this is not valid toml [[[")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        head = self._merge("feature", "app.ts", "export const a = 1\n")

        exit_code = self._run(mode="invisible", extra=["--policy", str(broken)])

        self.assertEqual(exit_code, daily_run.EXIT_JUDGE_DOWN)
        # Held, not advanced past merges nothing actually judged.
        self.assertEqual(self._state_head(), self.base)
        # And nothing was built or run: a broken policy file costs no machine time.
        self.assertEqual(self.built, [])
        self.assertEqual(self.ran, [])
        merge = self._manifest()["merges"][0]
        self.assertEqual((merge["verdict"], merge["trigger"]), ("visible", "fail_open"))
        self.assertEqual(merge["sha"], head)

    def test_a_merge_that_changed_nothing_costs_no_judge_call(self) -> None:
        """Re-merging a branch already in main produces a merge whose diff against
        its first parent is empty. There is nothing to judge, and asking anyway
        spends a call to be told so — the batch path used to do exactly that."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        git(self.repo, "checkout", "-q", "-b", "empty-branch")
        git(self.repo, "commit", "-q", "--allow-empty", "-m", "no file changes")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "-m", "Merge branch 'empty-branch'",
            "empty-branch")

        # mode="crash" fails open if the judge is consulted at all, so reaching
        # `invisible` proves it was not.
        self.assertEqual(self._run(mode="crash"), 0)
        merge = self._manifest()["merges"][0]
        self.assertEqual((merge["verdict"], merge["trigger"]), ("invisible", "path_rule"))
        self.assertEqual(self.ran, [])   # 没有 model-visible 改动，L2 不跑

    def test_a_path_rule_beats_the_size_guard(self) -> None:
        """A claimed path needs no judge and no measuring: the rule already
        states the answer, and reporting it as `oversize` would hide which of the
        two actually decided."""
        import sys as _sys
        _sys.path.insert(0, str(Path(daily_run.__file__).parent / "build"))
        import daily_batch

        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        oversized = "x" * (daily_batch.MAX_JUDGED_DIFF_BYTES + 1)
        self._merge("bump", "Cargo.lock", oversized)
        self.assertEqual(self._run(mode="crash"), daily_run.EXIT_NOT_COMPARABLE)

        merge = self._manifest()["merges"][0]
        self.assertEqual(merge["trigger"], "path_rule")
        self.assertEqual(merge["matched_rules"], ["lockfile"])

    # --- run types -------------------------------------------------------

    def test_daily_run_may_not_conclude(self) -> None:
        """k=1 is a screen. The manifest has to say so, because the ledger writer
        reads that field rather than re-deriving the rule."""
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible"), daily_run.EXIT_NOT_COMPARABLE)
        manifest = self._manifest()
        self.assertEqual(manifest["run_type"], "daily")
        self.assertEqual(manifest["k"], 1)
        self.assertFalse(manifest["conclusive"])

    def test_checkpoint_runs_k3_over_the_window(self) -> None:
        self._merge("one", "a.txt", "a\n")
        head = git(self.repo, "rev-parse", "HEAD")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible", run_type="checkpoint"), daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.ran, [(f"dim-agent-under-test:{head[:9]}", 3)])
        self.assertTrue(self._manifest()["conclusive"])
        self.assertEqual(self._state_head(), head)

    def test_baseline_measures_one_commit_and_leaves_the_window_alone(self) -> None:
        """A baseline is a reference measurement, not a coverage claim: it runs
        without a window and must not close anyone else's."""
        self._merge("one", "a.txt", "a\n")
        head = git(self.repo, "rev-parse", "HEAD")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(run_type="baseline"), 0)
        self.assertEqual(self.built, [head])
        self.assertEqual(self.ran, [(f"dim-agent-under-test:{head[:9]}", 3)])
        self.assertEqual(self._state_head(), self.base)

        manifest = self._manifest()
        self.assertEqual(manifest["run_type"], "baseline")
        self.assertIsNone(manifest["window"]["base"])
        self.assertEqual(manifest["merges"], [])
        self.assertEqual(
            manifest["corpus_pin"],
            json.loads((Path(daily_run.__file__).parent / "tasks" / "MANIFEST.json")
                       .read_text())["tree_digest"],
        )

    def test_baseline_at_k1_is_refused(self) -> None:
        """The one thing that must not happen quietly: a reference nothing in a
        later comparison would know it cannot support."""
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        with self.assertRaises(SystemExit) as caught:
            self._run(run_type="baseline", extra=["--k", "1"])
        self.assertIn("needs k>=3", str(caught.exception))

    def test_baseline_runs_even_with_nothing_new_on_main(self) -> None:
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(run_type="baseline"), 0)
        self.assertEqual(self.built, [self.base])

    def test_baseline_run_freezes_the_scores_it_measured(self) -> None:
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(run_type="baseline"), 0)

        pin = json.loads((Path(daily_run.__file__).parent / "tasks" /
                          "MANIFEST.json").read_text())["tree_digest"]
        frozen = json.loads((self.baselines / f"{pin}.json").read_text())
        # The version the CLI reported inside the trials, which is what says
        # which build these numbers describe.
        self.assertEqual(frozen["measured_versions"], ["0.5.1"])
        self.assertEqual(frozen["k"], 3)
        self.assertEqual(frozen["corpus_pin"], pin)
        self.assertEqual(frozen["tasks"]["tb/a"]["passes"], 3)
        self.assertEqual(self._manifest()["l2"]["baseline_written"],
                         str(self.baselines / f"{pin}.json"))
        # The identity that came back beside the results. Without it in here,
        # the guard in run_l2 has nothing to hold a later endpoint to.
        self.assertEqual(frozen["model"], MODEL_IDENTITY)

    def test_a_second_corpus_keeps_its_own_pin_and_concurrency(self) -> None:
        """OfficeQA Pro runs through the same entry point with its own tasks
        directory. Its baseline must land under its own pin — never beside or on
        top of the 30-task one — and the concurrency it was measured at must reach
        both the remote run and the reference it freezes."""
        tasks = self.root / "officeqa-tasks"
        tasks.mkdir()
        (tasks / "MANIFEST.json").write_text(json.dumps(
            {"tree_digest": "f" * 64, "tasks": {}}))
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(run_type="baseline", extra=[
            "--tasks-dir", str(tasks), "--concurrency", "10"]), 0)

        self.assertEqual(self.concurrency, [10])
        self.assertEqual([p.name for p in self.baselines.iterdir()], [f"{'f' * 64}.json"])
        frozen = json.loads((self.baselines / f"{'f' * 64}.json").read_text())
        self.assertEqual(frozen["corpus_pin"], "f" * 64)
        self.assertEqual(frozen["params"]["n_concurrent_trials"], 10)

    def test_a_baseline_is_not_frozen_when_the_model_identity_is_missing(self) -> None:
        """The reference must say which model produced it, or the guard that
        makes every later comparison mean anything has nothing to read.

        The identity is captured on the eval machine and rsync'd back beside the
        job. A transient failure of that one file, or DIM_EVAL_SKIP_MODEL_CHECK=1
        in the machine's environment, used to be enough: the fetch warned and
        carried on, `settle` got None, and the baseline was written with
        `"model": null`. `write` is write-once, so it then *was* the reference,
        and every gated run after it died inside the identity guard on a
        TypeError. Undoing it took a commit deleting the file.

        Refused at the freeze instead. Nothing is written, and the run is
        recorded invalid — which is what a re-run is for.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))

        self.assertEqual(self._run(run_type="baseline", no_model=True),
                         daily_run.EXIT_INVALID)
        self.assertEqual(list(self.baselines.glob("*.json")), [])

        manifest = self._manifest()
        self.assertIn("no model identity", manifest["settle_error"])
        # L2 did run: this is not a cheap refusal, it is the one place the fact
        # can be known. The evidence is kept so the re-run has it.
        self.assertEqual(len(self.ran), 1)
        self.assertIsNone(manifest["fetch_error"])

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[0]
        self.assertEqual(recorded.invalid_reason, "settle_failed")

    def test_a_later_run_compares_against_that_baseline(self) -> None:
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(run_type="baseline")

        self._merge("one", "a.txt", "a\n")
        self.assertEqual(self._run(mode="visible", run_type="checkpoint"), 0)
        comparison = self._manifest_for(git(self.repo, "rev-parse", "HEAD"))["l2"]["comparison"]
        self.assertIsNotNone(comparison)
        self.assertEqual(comparison["tasks_compared"], 1)
        self.assertEqual(comparison["full_flips"], [])
        self.assertEqual(comparison["blocking_breaches"], [])

    def test_the_daily_screen_does_not_warn_about_being_a_screen(self) -> None:
        """k=1 against a k=3 baseline is what the schedule does every night.

        The criteria cannot be evaluated across unequal k, and for a checkpoint
        that is a misconfiguration worth reporting. For the daily screen it is
        the design: it claims nothing either way. Reporting it made the pipeline
        warn every night about its own normal operation — and advise re-taking
        the baseline at k=1, which would replace the reference with one no
        checkpoint could ever use.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(run_type="baseline")
        head = self._merge("one", "a.txt", "a\n")

        self.assertEqual(self._run(mode="visible", run_type="daily"), 0)
        self.assertEqual(self._state_head(), head)

        summary = self._manifest_for(head)["l2"]
        self.assertEqual(summary["problems"], [])
        self.assertFalse(summary["comparison"]["comparable"])
        # The deltas are still computed and still reported; what is withheld is
        # a verdict, which a screen was never going to give.
        self.assertEqual(summary["comparison"]["tasks_compared"], 1)

    def test_a_baseline_at_another_k_is_invalid_not_a_pass(self) -> None:
        """Unequal k evaluates no criterion, and no criterion evaluated leaves no
        blocking breach — which is what `pass` is made of.

        `freeze --k` takes any integer, so a baseline can exist at a k the gate
        will never run at. Comparing against it printed "arms not comparable",
        recorded a conclusive pass, and put every merge in the window into the
        escape-rate denominator as one L2 had affirmatively cleared. Nothing had
        been compared at all.
        """
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(run_type="baseline")

        # Re-stamp the frozen baseline at a k this run will not match. It is what
        # `baseline.py freeze --k 2` leaves behind.
        frozen = next(self.baselines.glob("*.json"))
        data = json.loads(frozen.read_text())
        data["k"] = 2
        frozen.write_text(json.dumps(data))

        head = self._merge("one", "a.txt", "a\n")
        # Exit 0, like the missing-baseline case it shares a shape with: the
        # window is not held, because no re-run of this commit can change the
        # baseline's k. See the note in the test below on what that leaves open.
        self.assertEqual(self._run(mode="visible", run_type="checkpoint"), daily_run.EXIT_NOT_COMPARABLE)

        manifest = self._manifest_for(head)["l2"]
        self.assertFalse(manifest["comparison"]["comparable"])
        self.assertEqual(manifest["comparison"]["blocking_breaches"], [])
        self.assertIn("re-take the baseline", manifest["problems"][0])
        # Not a broken measurement: running this again produces the same
        # mismatch, so the window is not held on it.
        self.assertEqual(manifest["unscoreable"], [])

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[-1]
        self.assertEqual(recorded.outcome, "invalid")
        self.assertEqual(recorded.breaches, ())
        # Measured, so it still covers: the window advanced and no later run
        # will ever answer for these merges again.
        self.assertEqual(recorded.invalid_reason, "no_reference")
        self.assertTrue(recorded.measured)

    def test_a_baseline_from_a_dirty_harness_is_not_compared_against(self) -> None:
        """A reference measured by uncommitted code is refused, and the run that
        meets it lands where a missing baseline does: measured, window advanced,
        exit 7. Not `settle_failed`, which would hold the window and re-run the
        same batch every night until someone replaced the file."""
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self._run(run_type="baseline")

        # A baseline run writes a clean harness into the reference itself; this
        # re-stamps it the way the first, pre-check baseline was recorded.
        frozen = next(self.baselines.glob("*.json"))
        data = json.loads(frozen.read_text())
        self.assertIs(data["harness"]["dirty"], False)
        data["harness"]["dirty"] = True
        frozen.write_text(json.dumps(data))

        head = self._merge("one", "a.txt", "a\n")
        self.assertEqual(self._run(mode="visible", run_type="checkpoint"),
                         daily_run.EXIT_NOT_COMPARABLE)

        manifest = self._manifest_for(head)["l2"]
        self.assertIsNone(manifest["comparison"])
        self.assertIn("refused", manifest["problems"][0])
        self.assertEqual(manifest["unscoreable"], [])
        self.assertEqual(json.loads(self.state.read_text())["last_l2_head"], head)

        import ledger_tools
        recorded = ledger_tools.Ledger(self.ledger).runs()[-1]
        self.assertEqual(recorded.outcome, "invalid")
        self.assertEqual(recorded.invalid_reason, "no_reference")
        self.assertTrue(recorded.measured)

    def test_a_reused_image_must_match_the_commit_being_measured(self) -> None:
        """--head moves with main under a long session; an image built minutes ago
        would then have its results filed under a commit it never contained."""
        head = self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        with self.assertRaises(SystemExit) as caught:
            self._run(mode="visible", extra=["--image", "under-test:deadbeef"])
        self.assertIn("was built from deadbeef", str(caught.exception))

        self.assertEqual(
            self._run(mode="visible",
                      extra=["--image", f"under-test:{head[:9]}"]),
            daily_run.EXIT_NOT_COMPARABLE)
        self.assertEqual(self.built, [])

    # --- dry run ---------------------------------------------------------

    def test_dry_run_changes_nothing(self) -> None:
        self._merge("one", "a.txt", "a\n")
        self.state.write_text(json.dumps({"last_l2_head": self.base}))
        self.assertEqual(self._run(mode="visible", extra=["--dry-run"]), 0)
        self.assertEqual(self.built, [])
        self.assertEqual(self.ran, [])
        self.assertEqual(self._state_head(), self.base)


if __name__ == "__main__":
    unittest.main()


class DiscoveryTest(unittest.TestCase):
    """That every test file is reachable is itself a thing worth testing.

    `unittest discover` walks into a directory only when it is an importable
    package. `runner/` was not one, so discovery reported OK over the 63 tests it
    could reach while the 24 it could not never ran — the silent kind of gap,
    where the CI badge is green precisely because nothing looked.
    """

    def test_every_test_file_is_somewhere_discovery_can_reach(self) -> None:
        root = Path(__file__).resolve().parent
        for path in sorted(root.rglob("*flow_test.py")):
            for directory in path.relative_to(root).parents:
                if directory == Path("."):
                    continue
                self.assertTrue(
                    (root / directory / "__init__.py").exists(),
                    f"{path.relative_to(root)} will be skipped by "
                    f"`unittest discover`: {directory}/ has no __init__.py, so "
                    "discovery cannot import it. Add one — an empty package "
                    "marker is cheaper than a suite that silently stops running.",
                )
