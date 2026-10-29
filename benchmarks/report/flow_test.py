"""Flow tests for the nightly issue reporter against an in-memory GitHub.

Run: python3 -m unittest discover -s benchmarks/report -p 'flow_test.py'
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import analyze  # noqa: E402
import issues  # noqa: E402


class FakeGitHub(issues.GitHub):
    def __init__(self) -> None:
        super().__init__("arcships/dim", "token")
        self.issues: dict[int, dict] = {}
        self.comments: dict[int, list[str]] = {}
        self.labels: set[str] = set()

    def request(self, method: str, path: str, body: dict | None = None):
        if method == "GET" and path.startswith("/issues?"):
            return [issue for issue in self.issues.values()
                    if issue["state"] == "open" and issues.LABEL in issue["labels"]]
        if method == "POST" and path == "/labels":
            self.labels.add(body["name"])
            return body
        if method == "POST" and path == "/issues":
            number = len(self.issues) + 1
            self.issues[number] = {"number": number, "state": "open", **body}
            return self.issues[number]
        number = int(path.split("/")[2])
        if method == "POST" and path.endswith("/comments"):
            self.comments.setdefault(number, []).append(body["body"])
            return body
        if method == "PATCH":
            self.issues[number].update(body)
            return self.issues[number]
        raise AssertionError(f"unexpected {method} {path}")


MEASURED = {"trials": 30, "trial_passes": 20, "infra_failures": 0}


def run_json(**l2) -> dict:
    return {
        "run_id": "2026-10-29T0300__daily__abc__0001", "run_type": "daily", "k": 1,
        "window": {"base": "a" * 40, "head": "b" * 40},
        "merges": [
            {"sha": "c" * 40, "subject": "Merge pull request #42 from arcships/x",
             "verdict": "visible", "matched_rules": []},
            {"sha": "d" * 40, "subject": "Merge pull request #43 from arcships/y",
             "verdict": "invisible", "matched_rules": []},
        ],
        "l2": l2 or None,
    }


class IssueReporterFlowTest(unittest.TestCase):
    def setUp(self) -> None:
        self.github = FakeGitHub()

    def night(self, exit_code: int, run: dict | None = None, suite: str = "frontier",
              log: Path | None = None, pushed: bool = True,
              analysis_failed: bool = False, analysis_ok: bool = True) -> list[str]:
        found = issues.problems(suite, "daily", exit_code, run, log, "arcships/dim",
                                ledger_push_failed=not pushed,
                                analysis_failed=analysis_failed)
        return issues.report(self.github, suite, exit_code, run, found,
                             ledger_push_failed=not pushed, analysis_ok=analysis_ok)

    def open_issues(self) -> list[dict]:
        return [issue for issue in self.github.issues.values() if issue["state"] == "open"]

    def test_a_failure_is_filed_once_and_followed_up(self) -> None:
        self.assertEqual(self.night(4, run_json()), ["opened #1 (frontier/l2-failed)"])
        self.assertEqual(self.night(4, run_json()), ["commented on #1 (frontier/l2-failed)"])
        [issue] = self.open_issues()
        self.assertIn("L2 failed (exit 4)", issue["title"])
        self.assertIn("#42", issue["body"])
        self.assertNotIn("#43", issue["body"])
        self.assertEqual(self.github.labels, {issues.LABEL})

    def test_a_measured_night_closes_what_it_no_longer_sees(self) -> None:
        self.night(4, run_json())
        self.assertEqual(self.night(0, run_json(**MEASURED)), ["closed #1 (frontier/l2-failed)"])
        self.assertEqual(self.open_issues(), [])

    def test_a_night_without_l2_closes_nothing(self) -> None:
        """A dry run, or a window with no model-visible change, measured nothing."""
        self.night(4, run_json())
        self.assertEqual(self.night(0, run_json()), [])

    def test_a_failed_ledger_push_is_its_own_issue_and_closes_nothing(self) -> None:
        self.night(4, run_json())
        self.assertEqual(self.night(7, run_json(**MEASURED), pushed=False),
                         ["opened #2 (frontier/ledger-push)", "opened #3 (frontier/no-baseline)"])
        self.assertEqual(len(self.open_issues()), 3)

    def test_a_night_that_stopped_early_closes_nothing(self) -> None:
        self.night(6, run_json())
        self.assertEqual(self.night(1, None), ["opened #2 (frontier/pipeline-error)"])
        self.assertEqual(len(self.open_issues()), 2)

    def test_suites_keep_their_own_issues(self) -> None:
        self.night(4, run_json(), suite="officeqa-pro")
        self.night(0, run_json(**MEASURED), suite="frontier")
        self.assertEqual(len(self.open_issues()), 1)

    def test_a_missing_baseline_stays_open_while_other_problems_close(self) -> None:
        self.night(4, run_json())
        done = self.night(7, run_json(**MEASURED))
        self.assertEqual(done, ["opened #2 (frontier/no-baseline)",
                                "closed #1 (frontier/l2-failed)"])

    def test_a_blocking_breach_is_a_regression_issue(self) -> None:
        run = run_json(trials=30, trial_passes=20, infra_failures=0, comparison={
            "blocking_breaches": ["pass_rate_ci"], "full_flips": ["tb/regex-log"],
            "pass_rate_delta": -0.2, "pass_rate_ci": [-0.3, -0.1],
            "token_change": 0.1, "turns_change": 0.0,
        })
        self.assertEqual(self.night(0, run), ["opened #1 (frontier/regression)"])
        self.assertIn("tb/regex-log", self.open_issues()[0]["body"])
        self.assertEqual(self.night(0, run_json(**MEASURED, comparison={"comparable": False})), [])
        self.assertEqual(self.night(0, run_json(**MEASURED, comparison={"comparable": True})),
                         ["closed #1 (frontier/regression)"])

    def test_an_unanalysed_failure_keeps_the_previous_nightly_open(self) -> None:
        """A missing analysis must not be read as "no failures"."""
        previous = issues.Problem("frontier/nightly", "previous", "previous")
        issues.report(self.github, "frontier", 0, run_json(**MEASURED), [previous],
                      analysis_ok=True)
        done = self.night(0, run_json(**MEASURED), analysis_failed=True, analysis_ok=False)
        self.assertEqual(done, ["opened #2 (frontier/analysis-failed)"])
        self.assertEqual(self.github.issues[1]["state"], "open")

    def test_a_clean_night_closes_the_previous_nightly_without_analysis(self) -> None:
        """All trials passed, so the old failure summary is resolved even if the
        analyzer was not needed (or failed)."""
        previous = issues.Problem("frontier/nightly", "previous", "previous")
        issues.report(self.github, "frontier", 0, run_json(trials=30, trial_passes=30), [previous],
                      analysis_ok=True)
        done = self.night(0, run_json(trials=30, trial_passes=30),
                          analysis_failed=True, analysis_ok=False)
        self.assertEqual(done, ["opened #2 (frontier/analysis-failed)",
                                "closed #1 (frontier/nightly)"])

    def test_exit_8_with_a_ledger_push_failure_is_one_issue(self) -> None:
        self.assertEqual(self.night(8, run_json(), pushed=False),
                         ["opened #1 (frontier/ledger-push)"])

    def test_exit_8_without_the_ledger_flag_is_a_pipeline_error(self) -> None:
        self.assertEqual(self.night(8, run_json()),
                         ["opened #1 (frontier/pipeline-error)"])

    def test_the_log_tail_does_not_leak_the_eval_machine(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "night.log"
            log.write_text("[daily] $ ssh eval@evalbox run_l2.py\n"
                           "ssh: connect to host evalbox port 22: refused\n"
                           "[daily] results at /srv/eval-scratch/x, peer 10.1.2.3\n"
                           "[run_l2] model p/m @ https://models.internal/v1\n"
                           "auth sk-abcdefghijklmnop rejected\n")
            with mock.patch.dict(os.environ, {"DIM_EVAL_HOST": "eval@evalbox",
                                              "DIM_EVAL_SCRATCH": "/srv/eval-scratch",
                                              "DIM_EVAL_REDACT": "models.internal"}):
                self.night(4, run_json(), log=log)
        body = self.open_issues()[0]["body"]
        for secret in ("evalbox", "10.1.2.3", "/srv/eval-scratch", "models.internal",
                       "sk-abcdefghijklmnop"):
            self.assertNotIn(secret, body)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, text=True,
                          capture_output=True).stdout.strip()


class NightlyAnalysisFlowTest(unittest.TestCase):
    """A night's failed trials are analyzed, known causes fold, and the summary
    issue supersedes the previous night's."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
            "--allow-empty", "-m", "base")
        git(self.repo, "checkout", "-q", "-b", "grep")
        (self.repo / "grep.rs").write_text("const LIMIT: usize = 100;\n")
        git(self.repo, "add", ".")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "limit")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-ff",
            "grep", "-m", "Merge pull request #548 from arcships/grep")
        self.merge = git(self.repo, "rev-parse", "HEAD")
        self.github = FakeGitHub()
        self.prompts: list[str] = []

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def night(self, outcomes: dict[str, float], replies: dict[str, dict], known: dict) -> tuple:
        run_dir = self.root / f"run{len(list(self.root.iterdir()))}"
        harbor = run_dir / "harbor"
        harbor.mkdir(parents=True)
        rewards: dict[str, list[str]] = {}
        for task, reward in outcomes.items():
            trial = harbor / f"{task}__x"
            (trial / "agent").mkdir(parents=True)
            (trial / "verifier").mkdir()
            task_dir = self.root / "tasks" / task
            task_dir.mkdir(parents=True, exist_ok=True)
            (task_dir / "instruction.md").write_text(f"Solve {task}.")
            (trial / "lock.json").write_text(json.dumps({"task": {"path": str(task_dir)}}))
            (trial / "verifier" / "test-stdout.txt").write_text(f"FAILED test_{task}\n")
            (trial / "agent" / "dim-timeline.jsonl").write_text("\n".join(json.dumps(line) for line in [
                {"kind": "event", "branch_id": "main", "event": {"type": "step", "name": "tool-call",
                 "data": {"calls": [{"name": "grep", "input": {"pattern": task}}]}}},
                {"kind": "event", "branch_id": "main", "event": {"type": "step", "name": "tool-result",
                 "data": {"results": [{"result": {"content": "output truncated", "isError": False}}]}}},
            ]) + "\n")
            (trial / "result.json").write_text(json.dumps({
                "trial_name": trial.name, "task_name": f"tb/{task}", "task_checksum": "c",
                "agent_result": {}, "verifier_result": {"rewards": {"reward": reward}},
                "exception_info": None}))
            rewards.setdefault(str(reward), []).append(trial.name)
        (harbor / "result.json").write_text(json.dumps({
            "id": "job", "n_total_trials": len(outcomes),
            "stats": {"evals": {"dim": {"reward_stats": {"reward": rewards}, "exception_stats": {}}}}}))
        run = {"run_id": "2026-10-30T0300__daily__abc__0001", "run_type": "daily", "k": 1,
               "window": {"base": "a" * 40, "head": self.merge},
               "merges": [{"sha": self.merge, "subject": "Merge pull request #548 from arcships/grep",
                           "verdict": "visible", "matched_rules": [], "visible_files": ["grep.rs"]}],
               "l2": {"trials": len(outcomes), "trial_passes": sum(r == 1.0 for r in outcomes.values())}}
        (run_dir / "run.json").write_text(json.dumps(run))

        def ask(prompt: str) -> str:
            self.prompts.append(prompt)
            task = next(t for t in replies if f"Solve {t}." in prompt)
            return "Here you go:\n" + json.dumps(replies[task])

        failures, updated = analyze.analyze(run_dir, self.repo, known, ask, "2026-10-30")
        analysis = {"model": "fake", "failures": [analyze.asdict(f) for f in failures]}
        summary = issues.nightly("frontier", "daily", run, analysis, "arcships/dim")
        done = issues.report(self.github, "frontier", 0, run, [summary] if summary else [],
                             analysis_ok=True)
        return failures, updated, summary, done

    def test_failures_are_explained_and_known_causes_fold(self) -> None:
        regression = {"category": "agent", "cause": "The grep output was cut off.",
                      "evidence": "result: output truncated", "same_as_known": False,
                      "suspects": [{"pr": "#548", "confidence": "medium", "reason": "Lowers LIMIT."},
                                   {"pr": "#999", "confidence": "high", "reason": "Not in the window."}]}
        hard = {"category": "agent", "cause": "Never finds the key.", "evidence": "step 3",
                "same_as_known": True, "suspects": []}
        known = {"tb/hard": {"category": "agent", "cause": "Never finds the key.",
                             "first_seen": "2026-10-20", "nights": 4},
                 "tb/fixed": {"category": "agent", "cause": "Old.", "first_seen": "2026-10-01",
                              "nights": 9}}
        failures, updated, summary, done = self.night(
            {"regex": 0.0, "hard": 0.0, "fixed": 1.0}, {"regex": regression, "hard": hard}, known)

        by_task = {f.task: f for f in failures}
        self.assertEqual(set(by_task), {"tb/regex", "tb/hard"})
        self.assertEqual([s["pr"] for s in by_task["tb/regex"].suspects], ["#548"])
        self.assertTrue(by_task["tb/hard"].known)
        self.assertEqual(updated["tb/hard"]["nights"], 5)
        self.assertEqual(updated["tb/regex"]["first_seen"], "2026-10-30")
        self.assertNotIn("tb/fixed", updated)
        self.assertTrue(any("LIMIT: usize = 100" in prompt for prompt in self.prompts))
        self.assertTrue(any("Never finds the key." in prompt for prompt in self.prompts))

        self.assertIn("2 failed (1 new) · suspects #548", summary.title)
        self.assertIn("#### `tb/regex` — agent", summary.body)
        self.assertIn("#548 (medium): Lowers LIMIT.", summary.body)
        self.assertIn("| `tb/hard` | agent | 2026-10-20 | 5 |", summary.body)
        self.assertNotIn("#### `tb/hard`", summary.body)
        self.assertEqual(done, ["opened #1 (frontier/nightly)"])
        self.assertIn(issues.NIGHTLY_LABEL, self.github.issues[1]["labels"])

    def test_each_night_supersedes_the_last_and_a_clean_night_closes_it(self) -> None:
        reply = {"category": "environment", "cause": "uv download failed.", "evidence": "curl: (28)",
                 "same_as_known": False, "suspects": []}
        self.night({"regex": 0.0}, {"regex": reply}, {})
        *_, done = self.night({"regex": 0.0}, {"regex": reply}, {})
        self.assertEqual(done, ["opened #2 (frontier/nightly)", "closed #1 (frontier/nightly)"])
        *_, done = self.night({"regex": 1.0}, {}, {})
        self.assertEqual(done, ["closed #2 (frontier/nightly)"])

    def test_a_reply_that_cannot_be_read_is_listed_not_lost(self) -> None:
        failures, updated, summary, _ = self.night(
            {"regex": 0.0}, {"regex": {"category": "vibes"}}, {})
        self.assertIn("unknown category", failures[0].error)
        self.assertEqual(updated, {})
        self.assertIn("### Not analyzed", summary.body)


if __name__ == "__main__":
    unittest.main()
