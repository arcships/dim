"""Flow tests for the nightly issue reporter against an in-memory GitHub.

Run: python3 -m unittest discover -s benchmarks/report -p 'flow_test.py'
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

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
              log: Path | None = None, pushed: bool = True) -> list[str]:
        found = issues.problems(suite, "daily", exit_code, run, log, "arcships/dim",
                                ledger_push_failed=not pushed)
        return issues.report(self.github, suite, exit_code, run, found,
                             ledger_push_failed=not pushed)

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


if __name__ == "__main__":
    unittest.main()
