#!/usr/bin/env python3
"""File the night's eval results as GitHub issues.

Run by benchmarks/host/nightly.sh after daily_run.py, the ledger push and
report/analyze.py. Two kinds of issue:

* The nightly summary (``eval-nightly``): every failed trial with its analyzed
  cause and suspected PRs. Failures with the cause already reported on an
  earlier night fold into one line each. Each night's summary supersedes and
  closes the previous one, so only the latest is open.
* Pipeline problems: each problem kind of each suite has at most one open
  issue, found by a marker in its body: a repeat adds a comment instead of a
  second issue, and a later scored run without that problem closes it. The exit codes are daily_run's, plus nightly's
own 8 (ledger push failed); benchmarks/README.md lists them.

Exits 2 when GitHub could not be reached, so nightly.sh can surface that the
alerting itself is down; the run's own record is already in the ledger.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path

LABEL = "eval-pipeline"
LABEL_COLOR = "d93f0b"
NIGHTLY_LABEL = "eval-nightly"
NIGHTLY_COLOR = "1d76db"
API = "https://api.github.com"
LOG_TAIL_LINES = 80

#: Exit codes whose run can have measured something. A problem absent from
#: such a run is gone only if L2 actually ran (see `closes`).
SCORED_EXITS = (0, 7)

EXIT_PROBLEMS = {
    3: ("unjudged", "L1 left merges unjudged",
        "The judge did not decide every merge in the window. They ran L2 to stay "
        "safe and the window was held, so the next run judges them again."),
    4: ("l2-failed", "L2 failed",
        "The L2 run did not complete. The window was held; the next run repeats it."),
    5: ("judge-down", "L1 judge decided nothing",
        "The judge returned no verdict for any merge. Check the judge endpoint "
        "and its credential on the eval machine."),
    6: ("unscoreable", "L2 run could not be scored",
        "L2 finished, but trials are missing, ungraded, not run k times, or the "
        "infra failure rate is over the limit. The run is recorded invalid and "
        "the window was held: it must be re-run."),
    7: ("no-baseline", "No comparable baseline",
        "The measurement is good, but there is no baseline for this corpus pin "
        "(or none at this k). Re-running does not change that: take one with "
        "`daily_run.py --run-type baseline` in a reviewed change."),
}

LEDGER_PUSH = ("ledger-push", "Ledger push failed",
               "The night's records were not pushed to the ledger branch. They stay "
               "committed in the eval machine's ledger clone and are pushed with the "
               "next run's records; check the deploy key's write access and the "
               "branch rules.")
ANALYSIS_FAILED = ("analysis-failed", "Failure analysis failed",
                   "The run finished, but report/analyze.py could not explain its "
                   "failed trials. The previous nightly summary was kept open; "
                   "check the analysis endpoint and its credential on the eval "
                   "machine, then re-run.")


@dataclass(frozen=True)
class Problem:
    key: str
    title: str
    body: str


def marker(key: str) -> str:
    return f"<!-- dim-eval-key: {key} -->"


def redact(text: str) -> str:
    """Keep the eval machine, its endpoints and anything token-shaped out of
    the issue tracker. ``DIM_EVAL_REDACT`` adds strings, such as the model
    endpoint's host, that only the eval machine's config knows."""
    values = [os.environ.get(name, "") for name in
              ("DIM_EVAL_HOST", "DIM_EVAL_SCRATCH", "DIM_EVAL_REMOTE_PYTHON", "HOME")]
    host = os.environ.get("DIM_EVAL_HOST", "")
    values += [host.rpartition("@")[2], *os.environ.get("DIM_EVAL_REDACT", "").split()]
    for value in sorted(filter(lambda v: len(v) > 1, values), key=len, reverse=True):
        text = text.replace(value, "<redacted>")
    text = re.sub(r"\b\d{1,3}(?:\.\d{1,3}){3}\b", "<ip>", text)
    return re.sub(r"\b(sk-|ghp_|gho_|github_pat_)[A-Za-z0-9_-]{8,}", "<token>", text)


def log_tail(path: Path | None) -> str:
    if path is None or not path.exists():
        return ""
    lines = path.read_text(errors="replace").splitlines()[-LOG_TAIL_LINES:]
    return ("\n<details><summary>Log tail</summary>\n\n```text\n"
            + "\n".join(lines) + "\n```\n</details>\n")


def describe(run: dict | None, repo: str) -> str:
    """What the run covered, so the issue says which merges are in question."""
    if not run:
        return "No `run.json` was written; the run stopped before its manifest.\n"
    window = run.get("window") or {}
    lines = [f"- Run: `{run['run_id']}` ({run['run_type']}, k={run['k']})"]
    if window.get("head"):
        head = window["head"]
        lines.append(f"- Head: [`{head[:9]}`](https://github.com/{repo}/commit/{head})")
    if window.get("base"):
        lines.append(f"- Window: `{window['base'][:9]}..{window['head'][:9]}`")
    l2 = run.get("l2") or {}
    if l2:
        lines.append(f"- Trials: {l2.get('trial_passes')}/{l2.get('trials')} passed, "
                     f"infra failures {l2.get('infra_failures')}")
    for problem in l2.get("problems") or []:
        lines.append(f"- Problem: {problem}")
    for error in ("settle_error", "fetch_error"):
        if run.get(error):
            lines.append(f"- {error}: `{str(run[error])[:300]}`")
    triggering = [m for m in run.get("merges") or []
                  if m.get("verdict") == "visible" or m.get("matched_rules")]
    if triggering:
        lines.append("\nMerges in the window that reach the model:\n")
        for merge in triggering:
            number = re.match(r"Merge pull request #(\d+)", merge["subject"])
            name = (f"#{number.group(1)}" if number
                    else f"[`{merge['sha'][:9]}`](https://github.com/{repo}/commit/{merge['sha']})")
            lines.append(f"- {name} {merge['subject'][:100]}")
    for push in run.get("direct_pushes") or []:
        lines.append(f"- direct push `{push['sha'][:9]}` {push['subject'][:100]}")
    return "\n".join(lines) + "\n"


def problems(suite: str, run_type: str, exit_code: int, run: dict | None,
             log: Path | None, repo: str, ledger_push_failed: bool = False,
             analysis_failed: bool = False) -> list[Problem]:
    context = describe(run, repo) + log_tail(log)
    found = []
    if ledger_push_failed:
        kind, title, explanation = LEDGER_PUSH
        found.append(Problem(f"{suite}/{kind}", f"[eval] {suite} {run_type}: {title}",
                             f"{explanation}\n\n{context}"))
    if analysis_failed:
        kind, title, explanation = ANALYSIS_FAILED
        found.append(Problem(f"{suite}/{kind}", f"[eval] {suite} {run_type}: {title}",
                             f"{explanation}\n\n{context}"))
    if exit_code in EXIT_PROBLEMS:
        kind, title, explanation = EXIT_PROBLEMS[exit_code]
        found.append(Problem(f"{suite}/{kind}",
                             f"[eval] {suite} {run_type}: {title} (exit {exit_code})",
                             f"{explanation}\n\n{context}"))
    elif exit_code == 8 and ledger_push_failed:
        # nightly.sh's own code. The ledger-push issue above is the whole
        # report; a second "unexpected exit code" issue would be noise.
        pass
    elif exit_code not in SCORED_EXITS:
        found.append(Problem(f"{suite}/pipeline-error",
                             f"[eval] {suite} {run_type}: pipeline error (exit {exit_code})",
                             f"The nightly run stopped with an unexpected exit code.\n\n{context}"))

    comparison = ((run or {}).get("l2") or {}).get("comparison") or {}
    breaches = comparison.get("blocking_breaches") or []
    if breaches:
        flips = comparison.get("full_flips") or []
        details = [f"- Breached: {', '.join(breaches)}",
                   f"- Pass rate delta: {comparison.get('pass_rate_delta')} "
                   f"(CI {comparison.get('pass_rate_ci')})",
                   f"- Token change: {comparison.get('token_change')}, "
                   f"turns change: {comparison.get('turns_change')}"]
        if flips:
            details.append(f"- Tasks that flipped: {', '.join(flips)}")
        found.append(Problem(f"{suite}/regression",
                             f"[eval] {suite} {run_type}: regression against the baseline",
                             "The paired comparison against the baseline breached a "
                             "blocking criterion (docs/eval/trigger-policy.md, L2 门禁).\n\n"
                             + "\n".join(details) + "\n\n" + context))
    return [Problem(p.key, redact(p.title), redact(p.body)) for p in found]


def nightly(suite: str, run_type: str, run: dict, analysis: dict, repo: str) -> Problem | None:
    """The night's summary of failed trials, or None when every trial passed."""
    failures = analysis["failures"]
    if not failures:
        return None
    l2 = run.get("l2") or {}
    head = (run.get("window") or {}).get("head") or ""
    fresh = [f for f in failures if not f["known"] and not f["error"]]
    known = [f for f in failures if f["known"]]
    unanalyzed = [f for f in failures if f["error"]]
    suspects = sorted({s["pr"] for f in fresh for s in f["suspects"]})

    def link(pr: str) -> str:
        return pr if pr.startswith("#") else f"[`{pr}`](https://github.com/{repo}/commit/{pr})"

    lines = [f"**{suite} {run_type}** · run `{run['run_id']}` · head "
             f"[`{head[:9]}`](https://github.com/{repo}/commit/{head})",
             "",
             f"{l2.get('trial_passes')}/{l2.get('trials')} trials passed. "
             f"{len(failures)} failed: {len(fresh)} new or with a new cause, "
             f"{len(known)} failing for a known reason, {len(unanalyzed)} not analyzed."]
    if fresh:
        lines += ["", "### New or changed failures"]
        for f in fresh:
            lines += ["", f"#### `{f['task']}` — {f['category']}",
                      f"**Cause:** {f['cause']}", f"**Evidence:** {f['evidence']}"]
            if f["suspects"]:
                lines.append("**Suspected PRs:** " + "; ".join(
                    f"{link(s['pr'])} ({s['confidence']}): {s['reason']}" for s in f["suspects"]))
            else:
                lines.append("**Suspected PRs:** none")
    if known:
        lines += ["", "### Still failing for a known reason", "",
                  "| Task | Category | Failing since | Nights | Cause |", "|---|---|---|---|---|"]
        lines += [f"| `{f['task']}` | {f['category']} | {f['first_seen']} | {f['nights']} | "
                  f"{f['cause'].replace('|', '/')} |" for f in known]
    if unanalyzed:
        lines += ["", "### Not analyzed", ""]
        lines += [f"- `{f['task']}`: {f['error']}" for f in unanalyzed]
    window = describe(run, repo).split("\nMerges in the window that reach the model:\n")
    if len(window) == 2:
        lines += ["", "### Model-visible changes in this window", window[1].rstrip()]
    lines += ["", f"<sub>Causes and suspects are judgements by `{analysis['model']}` from "
              f"one run of each task (k={run['k']}), not verdicts.</sub>"]
    title = (f"[eval] {suite} {run_type} {run['run_id'][:10]}: {len(failures)} failed "
             f"({len(fresh)} new)" + (f" · suspects {' '.join(suspects)}" if suspects else ""))
    return Problem(f"{suite}/nightly", title, "\n".join(lines))


def closes(key: str, run: dict | None, analysis_ok: bool = False) -> bool:
    """Whether a scored run that did not show ``key`` shows it is gone.

    Only a run whose L2 ran can say a problem is gone, and only a comparable
    comparison can say a regression is. A nightly summary is different: its
    absence can mean either "no failures" or "analysis never ran", and only the
    former may close the previous summary. A failed analysis also must not close
    its own ``analysis-failed`` issue until a later analysis succeeds.
    """
    l2 = (run or {}).get("l2")
    if not l2:
        return False
    if key.endswith("/nightly"):
        trials = l2.get("trials")
        passes = l2.get("trial_passes")
        if isinstance(trials, int) and trials > 0 and passes == trials:
            return True
        return analysis_ok
    if key.endswith("/analysis-failed"):
        return analysis_ok
    if key.endswith("/regression"):
        return bool((l2.get("comparison") or {}).get("comparable"))
    return True


class GitHub:
    """The four calls the reporter makes, over the REST API."""

    def __init__(self, repo: str, token: str) -> None:
        self.repo = repo
        self.token = token

    def request(self, method: str, path: str, body: dict | None = None):
        request = urllib.request.Request(
            f"{API}/repos/{self.repo}{path}",
            method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": f"Bearer {self.token}",
                     "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28",
                     "User-Agent": "dim-eval"},
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or b"null")

    def open_issues(self) -> list[dict]:
        return self.request("GET", f"/issues?state=open&labels={LABEL}&per_page=100")

    def ensure_label(self) -> None:
        try:
            self.request("POST", "/labels", {"name": LABEL, "color": LABEL_COLOR,
                                             "description": "Filed by the nightly eval"})
        except urllib.error.HTTPError as error:
            if error.code != 422:
                raise

    def ensure_nightly_label(self) -> None:
        try:
            self.request("POST", "/labels", {"name": NIGHTLY_LABEL, "color": NIGHTLY_COLOR,
                                             "description": "Nightly eval failures"})
        except urllib.error.HTTPError as error:
            if error.code != 422:
                raise

    def create(self, title: str, body: str, labels: tuple[str, ...] = (LABEL,)) -> dict:
        return self.request("POST", "/issues", {"title": title, "body": body,
                                                "labels": list(labels)})

    def comment(self, number: int, body: str) -> None:
        self.request("POST", f"/issues/{number}/comments", {"body": body})

    def close(self, number: int) -> None:
        self.request("PATCH", f"/issues/{number}", {"state": "closed",
                                                    "state_reason": "completed"})


def report(github: GitHub, suite: str, exit_code: int, run: dict | None,
           found: list[Problem], ledger_push_failed: bool = False,
           analysis_ok: bool = False) -> list[str]:
    """Open, update and close issues. Returns what was done, one line each."""
    open_by_key = {}
    for issue in github.open_issues():
        match = re.search(r"<!-- dim-eval-key: (\S+) -->", issue.get("body") or "")
        if match and "pull_request" not in issue:
            open_by_key[match.group(1)] = issue

    done = []
    if found:
        github.ensure_label()
    for problem in found:
        existing = open_by_key.get(problem.key)
        if problem.key.endswith("/nightly"):
            github.ensure_nightly_label()
            created = github.create(problem.title, f"{marker(problem.key)}\n{problem.body}",
                                    (LABEL, NIGHTLY_LABEL))
            done.append(f"opened #{created['number']} ({problem.key})")
            if existing:
                github.comment(existing["number"], f"Superseded by #{created['number']}.")
                github.close(existing["number"])
                done.append(f"closed #{existing['number']} ({problem.key})")
        elif existing:
            github.comment(existing["number"], f"Seen again.\n\n{problem.body}")
            done.append(f"commented on #{existing['number']} ({problem.key})")
        else:
            created = github.create(problem.title, f"{marker(problem.key)}\n{problem.body}")
            done.append(f"opened #{created['number']} ({problem.key})")

    if exit_code in SCORED_EXITS and not ledger_push_failed:
        current = {problem.key for problem in found}
        run_id = (run or {}).get("run_id", "the latest run")
        for key, issue in open_by_key.items():
            if key.startswith(f"{suite}/") and key not in current and closes(key, run, analysis_ok):
                github.comment(issue["number"], f"Not seen in `{run_id}`; closing.")
                github.close(issue["number"])
                done.append(f"closed #{issue['number']} ({key})")
    return done


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="owner/name")
    parser.add_argument("--suite", required=True)
    parser.add_argument("--run-type", required=True)
    parser.add_argument("--exit-code", type=int, required=True)
    parser.add_argument("--run-json", type=Path, help="this run's run.json, if written")
    parser.add_argument("--log", type=Path, help="the night's log")
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--ledger-push-failed", action="store_true")
    parser.add_argument("--analysis", type=Path, help="report/analyze.py's output, if written")
    parser.add_argument("--analysis-failed", action="store_true",
                        help="analysis was attempted but did not complete")
    parser.add_argument("--dry-run", action="store_true", help="print, file nothing")
    args = parser.parse_args()

    run = None
    if args.run_json and args.run_json.exists():
        try:
            run = json.loads(args.run_json.read_text())
            describe(run, args.repo)
        except (ValueError, KeyError, TypeError) as error:
            print(f"[issues] unreadable {args.run_json}: {error}", file=sys.stderr)
            run = None

    analysis = None
    analysis_failed = args.analysis_failed
    if args.analysis:
        try:
            loaded = json.loads(args.analysis.read_text())
            if not isinstance(loaded, dict) or not isinstance(loaded.get("failures"), list):
                raise ValueError("analysis has no failures list")
            analysis = loaded
        except (OSError, ValueError, TypeError) as error:
            print(f"[issues] unreadable {args.analysis}: {error}", file=sys.stderr)
            analysis_failed = True
    analysis_ok = analysis is not None

    found = problems(args.suite, args.run_type, args.exit_code, run, args.log, args.repo,
                     args.ledger_push_failed, analysis_failed)
    if run and analysis is not None:
        try:
            summary = nightly(args.suite, args.run_type, run, analysis, args.repo)
        except (KeyError, TypeError, ValueError) as error:
            print(f"[issues] unusable analysis {args.analysis}: {error}", file=sys.stderr)
            analysis_ok = False
            found = problems(args.suite, args.run_type, args.exit_code, run, args.log,
                             args.repo, args.ledger_push_failed, True)
        else:
            if summary:
                found.insert(0, Problem(summary.key, redact(summary.title), redact(summary.body)))
    if args.dry_run:
        for problem in found:
            print(f"[issues] would file {problem.key}: {problem.title}\n{problem.body}")
        return 0
    try:
        token = args.token_file.read_text().strip()
        for line in report(GitHub(args.repo, token), args.suite, args.exit_code, run, found,
                           args.ledger_push_failed, analysis_ok):
            print(f"[issues] {line}")
    except (OSError, urllib.error.URLError, ValueError) as error:
        print(f"[issues] could not report to GitHub: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
