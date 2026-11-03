#!/usr/bin/env python3
"""Daily eval pipeline: judge what landed on main, build it, run L2 against it.

This is the entry point the scheduler calls. The pieces it drives already exist
and each is usable on its own; what was missing is the thing that runs them in
order and, more importantly, decides what the *next* run is allowed to skip.

    window          base..head on main                     (state file)
      -> L1         build/daily_batch.collect              (per merge verdict)
      -> build      build/build_image                      (image tagged by sha)
      -> L2         runner/run_l2.py over ssh              (harbor, k=1 daily)
      -> artifacts  run.json + harbor results, pulled back

Three decisions are load-bearing and are the reason this file exists rather than
a shell script:

* **The window starts at the last commit L2 actually ran on**, not at the last
  commit L1 looked at. A day whose merges are all invisible still advances it —
  nothing model-visible is left unevaluated — but a day whose L2 could not run
  does not, so the next run's window still covers those merges.
* **State advances only when the batch is settled.** A failed L2, or an MR the
  judge never decided, leaves the base where it was. Re-judging is nearly free
  (the verdict cache is keyed on the diff), and re-running L2 is what the next
  day would do anyway, so resumption costs nothing and losing coverage costs a
  regression nobody attributes.
* **The ledger gets a record, never a k=1 verdict.** Every merge judged and
  every L2 attempted is written down — including the attempts that failed,
  because a window with no record reads later as one L2 never looked at, which
  is the shape of an L1 false negative. What a k=1 run may not write is a
  conclusion: trigger-policy.md forbids a single k=1 round from being a
  judgement, and `RunRecord` enforces it, so such a run records `screen`, or
  `invalid` when it could not be scored at all. See the note above
  `write_manifest` for what `run.json` holds alongside.

Not solved: a dropped ssh connection kills the remote L2 mid-run. Keepalives
make that unlikely, not impossible; when it happens the run fails, the state
does not advance, and the next run repeats it. A detached remote session with a
poll loop is the fix, and it is not in this file.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shlex
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
#: The checkout this script lives in. It is the harness's own revision, which
#: is not necessarily the tree being evaluated: `--repo` may point at a mirror.
TOOLING_ROOT = HERE.parent
sys.path.insert(0, str(HERE / "build"))
sys.path.insert(0, str(HERE / "runner"))

import baseline as baselines  # noqa: E402
import build_image  # noqa: E402
import ledger_tools  # noqa: E402
import corpus  # noqa: E402
import daily_batch  # noqa: E402
import results  # noqa: E402
import run_l2  # noqa: E402

#: Machine-specific settings come from the eval machine's environment file
#: (benchmarks/host/README.md), never from this public repository.
DEFAULT_HOST = os.environ.get("DIM_EVAL_HOST")
DEFAULT_SCRATCH = os.environ.get("DIM_EVAL_SCRATCH")

#: The only fact that must outlive a run. It sits beside the ledger rather than
#: with the artifacts because the results directory is pruned after 30 days,
#: and a lost window base is not recoverable by guessing.
DEFAULT_STATE = HERE / "ledger" / "daily-state.json"

#: `benchmarks/results/` is already ignored by git.
DEFAULT_OUT = HERE / "results"

#: Imported rather than restated. ledger_tools.schema is where the threshold is
#: enforced, and a second copy here is a second thing to forget to change.
CONCLUSIVE_MIN_K = ledger_tools.CONCLUSIVE_MIN_K


@dataclass(frozen=True)
class RunType:
    """What a run is for, which decides what it is allowed to claim.

    `conclusive` is the field the ledger writer reads. Splitting it from `k` keeps
    the rule in one place: raising k on a daily run does not quietly turn a screen
    into a verdict, and lowering it on a baseline is refused rather than recorded
    as a weaker reference.
    """

    name: str
    k: int
    conclusive: bool
    #: A daily or checkpoint run answers for a range of main, so it judges that
    #: range and moves the window. A baseline answers for one commit: it is a
    #: measurement, not a coverage claim, and must not close anyone's window.
    judges_window: bool


RUN_TYPES = {
    # Sensitivity grows with sqrt(k), cost with k. The daily job catches a
    # collapse; settling a question is what the k=3 types are for.
    "daily": RunType("daily", k=1, conclusive=False, judges_window=True),
    "checkpoint": RunType("checkpoint", k=3, conclusive=True, judges_window=True),
    "baseline": RunType("baseline", k=3, conclusive=True, judges_window=False),
}

#: Harbor lives in this virtualenv on the eval machine and is not on the PATH a
#: non-interactive ssh session gets. Both the interpreter and its bin directory
#: are needed: run_l2 shells out to `harbor`, and the adapter is imported into
#: harbor's own process.
DEFAULT_REMOTE_PYTHON = os.environ.get("DIM_EVAL_REMOTE_PYTHON")

SSH_KEEPALIVE = ("-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=20")

EXIT_UNDETERMINED = 3
EXIT_L2_FAILED = 4
#: Every merge in the window went unjudged. Distinct from EXIT_UNDETERMINED
#: because that one heals on the next run: a judge that decided nothing at all
#: is an outage, and letting it pass as a warning produces a pipeline that is
#: green every day while the window never advances.
EXIT_JUDGE_DOWN = 5
#: The run finished but cannot be scored: trials missing from disk, trials the
#: agent finished but nothing graded, a task that did not run exactly k times, or
#: an infra failure rate over the limit. `record_run` already calls this
#: `invalid`, and this is the exit code that makes the rest of the pipeline agree
#: with it — without this the run advanced the window and returned 0, so a batch
#: nothing could score left the window anyway and nobody was told.
#:
#: Red, not a warning. trigger-policy.md requires an unscoreable run to be redone,
#: and a warning is how "redone" turns into "never".
EXIT_INVALID = 6
#: The run measured fine and there is nothing to compare it against: no baseline
#: for this corpus pin, or one whose k the criteria cannot be evaluated across.
#:
#: A warning rather than a failure, and the distinction from EXIT_INVALID is what
#: a re-run would do. Re-running fixes an unscoreable run; it cannot produce a
#: baseline or change an existing one's k. Only a person can, and until they do,
#: failing the pipeline every night would train them to ignore it. The window
#: advances for the same reason — holding it would stall on a condition no run
#: clears.
#:
#: Not silent, though. The nightly reporter keeps an issue open for it, so the
#: warning shows every night instead of passing quietly over a run that reached
#: no conclusion.
EXIT_NOT_COMPARABLE = 7


@dataclass(frozen=True)
class Window:
    """The range of main this run is responsible for."""

    base: str
    head: str

    @property
    def label(self) -> str:
        return f"{self.base[:9]}..{self.head[:9]}"


def corpus_pin(tasks_dir: Path) -> str:
    """The task corpus's content pin, which names its baseline file.

    Read from the corpus actually being run, not from the canonical one: a
    reduced corpus used to exercise the pipeline must not label its results with
    the full corpus's pin, where they would later be compared as if they were
    the same measurement. The tree must also be what its manifest says, digest
    for digest, or a task edited without regenerating the manifest would be
    measured, and frozen into a baseline, under a pin it does not have. A corpus
    with no manifest has no pin and is refused — generate one with
    runner/assemble_tasks.py.
    """
    manifest = tasks_dir / "MANIFEST.json"
    try:
        pin = json.loads(manifest.read_text())["tree_digest"]
    except (OSError, json.JSONDecodeError, KeyError) as error:
        raise SystemExit(f"cannot read the corpus pin from {manifest}: {error}")
    problems = corpus.verify(manifest, tasks_dir)
    if problems:
        raise SystemExit(f"{tasks_dir} is not the corpus its manifest pins: "
                         + "; ".join(problems[:5]))
    return pin


DEFAULT_BASELINES = HERE / "baselines"
DEFAULT_LEDGER = HERE / "ledger"

#: Namespaces merge records: the GitHub repository this repo lives in, so a
#: local run and a scheduled run write the same project into the ledger.
DEFAULT_PROJECT = "arcships/dim"


def baseline_for(pin: str, directory: Path) -> Path | None:
    """The baseline taken on this exact corpus, if one exists yet."""
    path = directory / f"{pin}.json"
    return path if path.exists() else None


def run(cmd: list[str], *, capture: bool = False, timeout: float | None = None) -> str:
    printable = " ".join(cmd)
    print(f"[daily] $ {printable}", flush=True)
    result = subprocess.run(
        cmd,
        check=False,
        text=True,
        timeout=timeout,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
    )
    if result.returncode != 0:
        if capture and result.stdout:
            sys.stderr.write(result.stdout)
        raise SystemExit(f"command failed ({result.returncode}): {printable}")
    return (result.stdout or "").strip()


def git(repo: Path, *args: str) -> str:
    return run(["git", "-C", str(repo), *args], capture=True)


# --- window ---------------------------------------------------------------


def read_state(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as error:
        # Not recoverable by guessing: a corrupt state file that silently became
        # "start from HEAD" would skip every merge since the last good run.
        raise SystemExit(f"state file {path} is unreadable: {error}")


def resolve_window(repo: Path, state: dict | None, base_arg: str | None,
                   head_ref: str) -> Window | None:
    """Resolve base..head, or None when nothing new landed.

    A missing base is a hard error rather than a default. "Everything since
    yesterday" and "everything since the last release" are both plausible
    guesses and both produce a batch whose attribution is wrong.
    """
    head = git(repo, "rev-parse", head_ref)
    base = base_arg or (state or {}).get("last_l2_head")
    if not base:
        raise SystemExit(
            "no window base: pass --base <sha> for the first run, or point "
            "--state at the file a previous run wrote"
        )
    base = git(repo, "rev-parse", base)

    if base == head:
        return None
    if subprocess.run(
        ["git", "-C", str(repo), "merge-base", "--is-ancestor", base, head],
        check=False, capture_output=True,
    ).returncode != 0:
        raise SystemExit(
            f"{base[:9]} is not an ancestor of {head[:9]}: the branch was "
            "rewritten or the wrong ref was passed; the window cannot be trusted"
        )
    return Window(base=base, head=head)


def write_state(path: Path, head: str, run_id: str) -> None:
    """Replace the state file atomically: a truncated one stops every later run."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("w") as stream:
        stream.write(json.dumps({
            "last_l2_head": head,
            "run_id": run_id,
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }, indent=2) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    print(f"[daily] state -> {path} (last_l2_head {head[:9]})")


# --- stages ---------------------------------------------------------------


def direct_pushes(repo: Path, window: Window) -> tuple[tuple[str, str], ...]:
    """First-parent commits in the window that are not merges.

    L1 judges MRs, and an MR is a merge commit — so a commit pushed straight to
    main is not judged by anything. It must still be evaluated: a window that
    contains only direct pushes would otherwise report "no merges", skip L2 and
    close, which is the one outcome this pipeline may never reach silently.

    They do not hold the window. Unlike a judge outage there is nothing a later
    run could decide differently, so holding would stall the window forever.
    """
    raw = git(repo, "log", "--first-parent", "--no-merges", "--reverse",
              "--format=%H|%s", f"{window.base}..{window.head}")
    return tuple(
        (line.split("|", 1)[0], line.split("|", 1)[1])
        for line in raw.splitlines() if line.strip()
    )


def deliver_judge(host: str, scratch: str, runner_sha: str) -> str:
    """Ship the judge to the eval machine and return its remote path.

    Without this the judge is whatever file was last copied there by hand —
    outside the pinning the harness claims for itself, and with a failure mode
    that hides: a missing or stale script makes every merge fail open, which
    triggers L2, burns the machine time, and never advances the window. The
    pipeline stays green while measuring nothing.
    """
    remote = f"{scratch}/eval-{runner_sha[:9]}"
    run(["ssh", *SSH_KEEPALIVE, host, f"mkdir -p {shlex.quote(remote)}/l1"])
    for name in ("agent_reviewer.py", "remote_reviewer.sh"):
        run(["rsync", "-az", f"{HERE / 'l1' / name}", f"{host}:{remote}/l1/{name}"])
    print(f"[daily] judge -> {host}:{remote}/l1/agent_reviewer.py")
    return f"{remote}/l1/agent_reviewer.py"


def judge(repo: Path, window: Window, policy: Path, prompt: Path,
          cache_dir: Path, remote_judge: bool) -> list[daily_batch.MergeRecord]:
    """L1 over every merge in the window.

    The whole window is re-judged on every run, not just the merges new since
    the last one. The cache makes a repeat verdict free, and re-deriving the
    window from scratch is what keeps the batch's attribution complete when an
    earlier run left merges undecided.
    """
    command = ((str(HERE / "l1" / "remote_reviewer.sh"),)
               if remote_judge else daily_batch.REVIEWER_COMMAND)
    return daily_batch.collect(
        repo, window.base, window.head, policy,
        review=True, prompt_path=prompt, cache_dir=cache_dir, command=command,
    )


def build_subject(repo: Path, head: str, staging: Path, host: str,
                  scratch: str, tag_prefix: str) -> tuple[str, dict]:
    """Build the CLI at `head` and deliver it as an image.

    The build runs in a detached worktree, for two reasons. The commit under
    test must be exactly the head that L1 judged — building whatever the
    driving checkout happens to have is how a result gets attributed to the
    wrong code — and a daily job must not disturb the tree someone works in.

    `:local` is deliberately not moved. It is what a running job resolves its
    mount from, and moving it mid-run swaps the binary under way.
    """
    worktree = staging / "src"
    if worktree.exists():
        run(["git", "-C", str(repo), "worktree", "remove", "--force", str(worktree)])
    run(["git", "-C", str(repo), "worktree", "add", "--detach", str(worktree), head])
    try:
        identity = build_image.collect_identity(worktree)
        if identity["commit"] != head:
            raise SystemExit(
                f"worktree is at {identity['commit'][:9]}, expected {head[:9]}"
            )
        binary, plugins = build_image.build_cli(worktree, staging)
        image_dir = staging / "image"
        identity = build_image.assemble(binary, plugins, image_dir, identity)
        tag = build_image.deliver(
            image_dir, host, f"{scratch}/dim-image-{identity['commit_short']}",
            tag_prefix, identity, tag_latest=False,
        )
    finally:
        run(["git", "-C", str(repo), "worktree", "remove", "--force", str(worktree)])
    return tag, identity


def deliver_runner(host: str, scratch: str, runner_sha: str,
                   baselines_dir: Path, tasks_dir: Path) -> str:
    """Ship the harness to the eval machine and return its remote directory.

    The harness is delivered from this checkout rather than left to whatever was
    scp'd onto the machine last: a result is reproducible only when both the
    subject and the tooling that scored it are pinned. `tasks/` travels with it
    because run_l2 resolves the corpus as a sibling of `runner/`.
    """
    remote = f"{scratch}/eval-{runner_sha[:9]}"
    run(["ssh", *SSH_KEEPALIVE, host, f"mkdir -p {shlex.quote(remote)}"])
    # `baselines/` travels with them: the model-identity guard reads it on the eval
    # machine, and a guard that silently finds no baseline is a guard that never
    # fires.
    sources = {"runner": HERE / "runner", "tasks": tasks_dir,
               "baselines": baselines_dir}
    for name, source in sources.items():
        run(["rsync", "-az", "--delete", "--exclude", "__pycache__",
             f"{source}/", f"{host}:{remote}/{name}/"])
    print(f"[daily] harness -> {host}:{remote}")
    return remote


def remote_harbor_version(host: str, python: str) -> str:
    """Harbor's version on the eval machine.

    Part of the corpus's triple pin (tasks/UPSTREAM.md): the task bodies live in
    images and the verifiers run under harbor, so two runs under different harbor
    versions are not the same experiment. Recorded rather than assumed — the box
    has several installations and they are not the same version.
    """
    result = subprocess.run(
        ["ssh", *SSH_KEEPALIVE, host,
         f"{shlex.quote(python)} -c 'import harbor, importlib.metadata as m; "
         "print(m.version(\"harbor\"))'"],
        check=False, capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"cannot read harbor's version on {host} via {python}: "
            f"{result.stderr.strip()[:200]}"
        )
    return result.stdout.strip()


def run_l2_remote(host: str, remote: str, tag: str, run_id: str, k: int,
                  timeout_sec: float,
                  baseline: str | None = None,
                  python: str = DEFAULT_REMOTE_PYTHON, *,
                  concurrency: int) -> tuple[int, str]:
    """Run L2 on the eval machine. Returns (exit code, remote results dir).

    The credential lives in `~/.dim-eval-env` on that machine and is sourced
    into the remote shell; it never reaches this process. What the run writes to
    disk carries a `${VAR}` reference rather than the value — harbor resolves it
    from its own environment, and templatizes it again in every record it writes.
    """
    runner = f"{remote}/runner"
    results = f"{remote}/results"
    env = {
        "PYTHONPATH": runner,
        "DIM_EVAL_CLI_IMAGE": tag,
        "DIM_EVAL_RUN_ID": run_id,
        "DIM_EVAL_OUT": results,
        "DIM_EVAL_K": str(k),
        "DIM_EVAL_CONCURRENCY": str(concurrency),
    }
    # Always override the values the remote shell sources from ~/.dim-eval-env.
    # daily_run records the condition from this process's environment; if the
    # remote file could supply a different proxy or bypass list, the job would
    # run under a condition the manifest never saw.
    env["DIM_EVAL_VERIFIER_PROXY"] = os.environ.get(
        "DIM_EVAL_VERIFIER_PROXY", ""
    )
    env["DIM_EVAL_VERIFIER_NO_PROXY"] = os.environ.get(
        "DIM_EVAL_VERIFIER_NO_PROXY", ""
    )
    if baseline:
        # run_l2 refuses to run when the endpoint no longer serves the model the
        # baseline recorded. Without this the check runs and reports it has
        # nothing to compare against.
        env["DIM_EVAL_BASELINE"] = baseline
    assignments = " ".join(f"{key}={shlex.quote(value)}" for key, value in env.items())
    bindir = str(Path(python).parent)
    command = (
        "set -eu; set -a; . ~/.dim-eval-env; set +a; "
        f"cd {shlex.quote(runner)}; PATH={shlex.quote(bindir)}:$PATH "
        # -u so progress reaches the tail of a long run instead of a buffer.
        f"{assignments} {shlex.quote(python)} -u run_l2.py"
    )
    print(f"[daily] $ ssh {host} run_l2.py ({run_id}, k={k}, "
          f"concurrency={concurrency})", flush=True)
    try:
        completed = subprocess.run(
            ["ssh", *SSH_KEEPALIVE, host, command],
            check=False, text=True, timeout=timeout_sec,
        )
    except subprocess.TimeoutExpired:
        # Losing the ssh client does not stop the run: harbor is orphaned, not
        # killed, and keeps writing into the same results directory. Say so
        # rather than raising, because the recovery is to let it finish and
        # re-read, not to start again on top of it.
        print(f"[daily] ssh gave up after {timeout_sec / 3600:.1f}h. The remote "
              f"run is orphaned, not stopped: check `pgrep -af '[h]arbor run'` on "
              f"{host} before relaunching, or results will interleave.")
        return 124, results
    return completed.returncode, results


def fetch_results(host: str, remote_results: str, run_id: str, out: Path) -> str | None:
    """Bring the job back. Returns why it could not be, or None.

    Reported rather than raised, for both of the reasons it is called. On the
    scoring path a raise would take the manifest down with it, losing the record
    of a run that already spent its hours — the same failure `settle_error`
    exists to prevent, one step earlier. On the diagnostic path there may be
    nothing to fetch at all: harbor can fail before it writes a results
    directory, and "no evidence" is an answer, not an error.
    """
    out.mkdir(parents=True, exist_ok=True)
    # --delete because the local directory must be what the remote job holds and
    # nothing else. run_id is unique per run so there should be nothing here to
    # delete; this is the second line of defence, and the failure it guards is
    # silent — a leftover trial reads as a real one and lands in the score.
    command = ["rsync", "-az", "--delete", f"{host}:{remote_results}/{run_id}/",
               f"{out}/harbor/"]
    print(f"[daily] $ {' '.join(command)}", flush=True)
    fetched = subprocess.run(
        command, check=False, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    # Exit 23 is rsync's partial transfer: files the harness user cannot read,
    # namely the agent logs task containers write as root. The job summary is
    # what scoring reads, so as long as it arrived the run is scorable and the
    # unreadable files stay on the eval machine. Treating 23 as failure made
    # every run with a root-owned timeline unscorable after it had already spent
    # its hours.
    if fetched.returncode == 23 and (out / "harbor" / "result.json").is_file():
        print("[daily] some files were unreadable (rsync exit 23); "
              "continuing with the readable job", flush=True)
    elif fetched.returncode != 0:
        if fetched.stdout:
            sys.stderr.write(fetched.stdout)
        return f"fetch failed: command failed ({fetched.returncode}): {' '.join(command)}"
    # Written beside the job by run_l2, which is the only process that can ask
    # the endpoint what it served. Absent when the identity check was skipped, so
    # a failure here is reported rather than fatal.
    identity = subprocess.run(
        ["rsync", "-az", f"{host}:{remote_results}/{run_id}-model.json", f"{out}/"],
        check=False, capture_output=True, text=True,
    )
    if identity.returncode != 0:
        # Not fatal here: a gated run does not need it, because run_l2 already
        # held the endpoint to the baseline before it ran. A baseline run does,
        # and `baselines.build` refuses one without it rather than freezing a
        # reference that cannot say what produced it.
        print("[daily] no model identity alongside the results; a baseline run "
              "will refuse to freeze from this")
    print(f"[daily] results -> {out / 'harbor'}")
    return None


def _reason(record: daily_batch.MergeRecord) -> str:
    """Why this merge got its verdict, in one line.

    The per-file traces the judge produced stay in the verdict cache, keyed by
    the same identity this record carries; duplicating them here would put the
    same text in two places that can drift.
    """
    if record.matched_rules:
        return f"path rule: {', '.join(record.matched_rules)}"
    if record.trigger == "oversize":
        return f"not judged: {record.review_error}"
    if record.review_error:
        return f"judge failed: {record.review_error}"
    if record.visible_files:
        return f"{len(record.visible_files)} model-visible file(s)"
    return "no model-visible file"


def _disagreements(existing: ledger_tools.MergeRecord,
                   record: daily_batch.MergeRecord) -> list[str]:
    """Every way a re-judgement differs from what the ledger holds.

    The verdict alone used to be the whole comparison, which meant a merge could
    be re-judged under a different policy, prompt or judge model, reach the same
    verdict for different reasons, and leave nothing saying the question had
    changed. `l1_identity` is what catches that: it covers the diff, the policy
    file's digest, the prompt and the model. The other two are the parts of the
    recorded answer that would now be stale.
    """
    differences = []
    if existing.l1_verdict != record.verdict:
        differences.append(f"verdict {existing.l1_verdict} -> {record.verdict}")
    if existing.l1_trigger != record.trigger:
        differences.append(f"trigger {existing.l1_trigger} -> {record.trigger}")
    if existing.l1_identity != record.identity:
        differences.append(f"identity {existing.l1_identity[:12]} -> "
                           f"{record.identity[:12]}")
    if tuple(existing.visible_files) != tuple(record.visible_files):
        differences.append(f"{len(existing.visible_files)} -> "
                           f"{len(record.visible_files)} visible file(s)")
    return differences


def record_merges(ledger: ledger_tools.Ledger, records: list[daily_batch.MergeRecord],
                  project_id: str) -> tuple[list[str], list[str]]:
    """Write one L1 record per merge. Returns (shas the ledger holds, disputed).

    The window is re-judged every run, so most merges are already recorded. A
    repeat is skipped rather than refused — but a repeat that *disagrees* is
    reported, because two verdicts for the same question is the one thing the
    verdict cache exists to make impossible, and a silent overwrite would erase
    the evidence that it happened.

    Reported, not refused, and the reason is in what `l1_identity` covers: the
    policy file's digest and the judge's model are part of it, so one ordinary
    policy edit makes every merge still in the window disagree at once. Raising
    would wedge the pipeline on a legitimate change, against an append-only store
    with no way to clear it. The disputed shas go on the run record instead,
    which outlives the artifacts that hold the detail.

    A `fail_open` record is the exception. It holds no verdict, so there is
    nothing to disagree with: the first real verdict replaces it, and another
    fail_open leaves it as it is (trigger-policy.md, Merge records).
    """
    written: list[str] = []
    disputed: list[str] = []
    for record in records:
        entry = ledger_tools.MergeRecord(
            merge_sha=record.sha,
            project_id=project_id,
            subject=record.subject,
            merged_at=record.merged_at,
            l1_verdict=record.verdict,
            l1_trigger=record.trigger,
            l1_reason=_reason(record),
            l1_identity=record.identity,
            visible_files=record.visible_files,
        )
        existing = ledger.merge(record.sha)
        if existing is None:
            ledger.append_merge(entry)
        elif existing.l1_trigger == "fail_open":
            if not record.undetermined:
                ledger.replace_fail_open(entry)
                print(f"[daily] {record.sha[:9]} was recorded fail_open; replacing "
                      f"it with this run's verdict ({record.verdict}, {record.trigger})")
        else:
            differences = _disagreements(existing, record)
            if differences:
                disputed.append(record.sha)
                print(f"[daily] WARNING: {record.sha[:9]} was judged differently "
                      f"this time ({'; '.join(differences)}); keeping the "
                      "recorded verdict, and noting the disagreement on this run")
        written.append(record.sha)
    return written, disputed


def record_run(ledger: ledger_tools.Ledger, *, run_id: str, run_type: RunType,
               k: int, pin: str, subject_commit: str, window: Window | None,
               head: str, covers: list[str], unjudged: tuple[str, ...],
               summary: dict | None, l2_exit: int, settle_error: str | None,
               runner_dirty: bool, disputed: list[str]) -> None:
    """Write what this run answered for — including that it answered nothing.

    Every L2 attempt gets a record. It used to be only the ones that produced a
    summary, so a run harbor failed or scoring crashed on left the ledger with
    nothing at all, and the merges in its window then read as merges no L2 ever
    covered — which is the shape of an L1 false negative. A crashed harness was
    being written down as L1 waving things through.

    The outcome comes from `conclusive`, not from the numbers: a k=1 screen has
    no verdict to record however good its numbers look, and `RunRecord` refuses
    to store one. What it does not refuse is `invalid` at k=1, because that is
    not a verdict either — it is the absence of one, and a screen that could not
    be scored has to be able to say so.
    """
    comparison = (summary or {}).get("comparison")
    # Order is chronological, and the three failures come before k because none
    # of them produced a measurement: `screen` would claim one exists. `k` then
    # separates the runs that did measure into the ones allowed to conclude and
    # the ones that are not.
    #
    # `no_reference` is last and is the one that stays `measured`. The rest of
    # `problems` — no baseline, a baseline at another k — describes a good
    # measurement with nothing to read it against, which for a k=1 screen is not
    # news: it never compares. That is why it sits below the k check and files as
    # `screen` there; a conclusive run has no such excuse.
    reason: ledger_tools.InvalidReason | None = None
    if l2_exit != 0:
        outcome, breaches, reason = "invalid", (), "l2_failed"
    elif settle_error is not None:
        outcome, breaches, reason = "invalid", (), "settle_failed"
    elif summary is None or summary.get("unscoreable"):
        outcome, breaches, reason = "invalid", (), "unscoreable"
    elif k < ledger_tools.CONCLUSIVE_MIN_K:
        outcome, breaches = "screen", ()
    elif summary.get("problems"):
        outcome, breaches, reason = "invalid", (), "no_reference"
    elif comparison and comparison["blocking_breaches"]:
        outcome, breaches = "fail", tuple(comparison["blocking_breaches"])
    else:
        outcome, breaches = "pass", ()

    ledger.append_run(ledger_tools.RunRecord(
        run_id=run_id,
        run_type=run_type.name,
        k=k,
        corpus_pin=pin,
        subject_commit=subject_commit,
        window_base=window.base if window else None,
        window_head=head,
        covers=tuple(covers),
        unjudged=unjudged,
        outcome=outcome,
        breaches=breaches,
        invalid_reason=reason,
        # Only the non-zero one. A 0 here would be noise on every healthy record
        # and would move their digests for nothing.
        l2_exit_code=l2_exit or None,
        settle_error=settle_error,
        runner_dirty=runner_dirty,
        disputed=tuple(disputed),
        infra_rate=float((summary or {}).get("infra_rate", 0.0)),
    ))
    print(f"[daily] ledger run {run_id}: {outcome}"
          + (f" ({reason})" if reason else "")
          + (f" ({', '.join(breaches)})" if breaches else ""))


def settle(out: Path, run_id: str, run_type: RunType, k: int, pin: str,
           existing: Path | None, subject: dict | None, runner_sha: str,
           params: dict, baselines_dir: Path) -> dict:
    """Turn the fetched job into scores, and into a baseline or a comparison.

    Returns what the manifest records. A run that cannot be scored still returns
    its reasons: `invalid` is an outcome, and losing it here would leave the
    manifest saying a run happened without saying it said nothing.
    """
    report = results.read_job(out / "harbor")
    problems = report.problems(expected_k=k)
    passes = sum(task.passes for task in report.tasks)
    trials = sum(task.k for task in report.tasks)
    print(f"\n[daily] L2: {passes}/{trials} trial(s) passed across "
          f"{len(report.tasks)} task(s), infra {report.infra_rate:.1%}")
    for problem in problems:
        print(f"[daily]   not scoreable: {problem}")

    summary = {
        "tasks": len(report.tasks),
        "trials": len(report.trials),
        "trial_passes": passes,
        "infra_failures": report.infra_failures,
        "infra_rate": report.infra_rate,
        # Two questions share this list, and only one of them is about the
        # measurement. `unscoreable` is the report's own verdict — trials missing,
        # trials nothing graded, a task not run k times, infra over the limit —
        # and every one of those is fixed by running again. "No baseline" lands in
        # `problems` too, below, because it equally bars a conclusion; but it is
        # not a broken measurement and re-running cannot clear it, so the window
        # decision keys on `unscoreable` rather than on the union.
        "problems": list(problems),
        "unscoreable": list(problems),
        "baseline_written": None,
        "comparison": None,
    }

    model_path = out / f"{run_id}-model.json"
    model = json.loads(model_path.read_text()) if model_path.exists() else None

    if not run_type.judges_window:
        if problems:
            print("[daily] not freezing a baseline from an unscoreable run")
            return summary
        path = baselines_dir / f"{pin}.json"
        # `dirty` is False by construction: a baseline run with uncommitted
        # harness changes is refused before the judge or the build. It is written
        # anyway because the reference has to say so itself, in the same shape
        # run.json uses, for `load` to check.
        baselines.write(path, baselines.build(
            report, corpus_pin=pin, run_id=run_id, k=k, model=model,
            subject=subject, harness={"runner_sha": runner_sha, "dirty": False},
            params=params,
        ))
        print(f"[daily] baseline -> {path}")
        summary["baseline_written"] = str(path)
        return summary

    if existing is None:
        # Nothing to compare against is not the same as nothing wrong. Falling
        # through would record a conclusive `pass` on the strength of no
        # comparison at all, and every merge in the window would then enter the
        # escape-rate denominator as one L2 affirmatively cleared.
        summary["problems"].append(f"no baseline for corpus pin {pin[:12]}")
        return summary

    try:
        reference = baselines.load(existing)
    except baselines.BaselineError as error:
        # A baseline that exists but is refused, measured by a dirty harness or
        # missing what the guards read, leaves this run where a missing one does:
        # a good measurement with nothing trustworthy to read it against. It is
        # not a scoring failure. Re-running cannot clear it, and holding the
        # window on it would re-run the same batch every night until someone
        # replaced the file.
        summary["problems"].append(f"baseline {existing.name[:12]} refused: {error}")
        return summary
    comparison = baselines.compare(reference, report, k, candidate_params=params)
    print("\n" + baselines.render(comparison))
    summary["comparison"] = {
        "baseline": str(existing),
        "tasks_compared": len(comparison.tasks_compared),
        "full_flips": list(comparison.full_flips),
        "net_degraded": comparison.net_degraded,
        "pass_rate_delta": comparison.pass_rate_delta,
        "pass_rate_ci": list(comparison.pass_rate_ci),
        "token_change": comparison.token_change,
        "turns_change": comparison.turns_change,
        "end_reason_deltas": comparison.end_reasons,
        "comparable": comparison.comparable,
        "baseline_verifier_proxy": comparison.baseline_verifier_proxy,
        "candidate_verifier_proxy": comparison.candidate_verifier_proxy,
        "baseline_verifier_no_proxy": comparison.baseline_verifier_no_proxy,
        "candidate_verifier_no_proxy": comparison.candidate_verifier_no_proxy,
        "verifier_proxy_comparable": comparison.verifier_proxy_comparable,
        "blocking_breaches": [c.name for c in comparison.blocking_breaches],
    }
    if not comparison.comparable and run_type.conclusive:
        # Unequal k evaluates no criterion, so `blocking_breaches` is empty — and
        # an empty breach list is what `pass` is made of. Without this the run
        # recorded a conclusive pass having compared nothing, and every merge in
        # the window entered the escape-rate denominator as one L2 affirmatively
        # cleared. That is the same hole `existing is None` guards above, one
        # level down: there the baseline is absent, here it is present and not
        # commensurable.
        #
        # `problems`, not `unscoreable`: this run measured fine. Running it again
        # produces the same mismatch, because what has to change is the baseline.
        #
        # Only for a run that could otherwise have concluded. The daily screen is
        # k=1 against a k=3 baseline by design — every night, forever — and it
        # claims nothing either way, so reporting that no criterion applied tells
        # it nothing it did not already know. Recording it as a problem made the
        # pipeline warn every single night about its own normal operation, and
        # told the reader to re-take the baseline at k=1, which would destroy the
        # reference. A k=1 screen is not a misconfiguration; a k=3 checkpoint
        # against a k=2 baseline is.
        if comparison.baseline_k != comparison.candidate_k:
            summary["problems"].append(
                f"baseline k={comparison.baseline_k} but this run is k="
                f"{comparison.candidate_k}; re-take the baseline at k={k}"
            )
        else:
            summary["problems"].append(
                f"baseline is not comparable: {comparison.incomparable_detail}; "
                "re-take the baseline under the same verifier proxy condition"
            )
    return summary


# --- artifacts ------------------------------------------------------------


def write_manifest(path: Path, *, run_id: str, run_type: RunType, k: int,
                   conclusive: bool, head: str, base: str | None, corpus_pin: str,
                   baseline: Path | None, records: list[daily_batch.MergeRecord],
                   direct: tuple[tuple[str, str], ...], image: dict | None,
                   runner_sha: str, runner_dirty: bool, verifier_proxy: bool,
                   verifier_no_proxy: str | None, l2_exit: int | None,
                   l2: dict | None = None, settle_error: str | None = None,
                   fetch_error: str | None = None) -> None:
    """The run's evidence, in one file.

    This is not itself a ledger record, and `conclusive` is the field that says
    whether one may be derived from it. A daily run is k=1 and may not decide
    anything (trigger-policy.md, Experiment parameters); a checkpoint or baseline
    run at k=3 may. Everything else here is what a ledger writer needs to attach
    that decision to something: the range answered for, the per-merge L1 verdicts,
    the corpus the numbers came from, and what was built and run.

    ``verifier_proxy`` is recorded even on a run that never reached L2: it is a
    condition of the measurement, and the baseline comparison refuses two arms
    whose conditions differ. A run whose manifest says False cannot have used the
    proxy configured in the job config, because both are derived from the same
    environment variable in run_l2. ``verifier_no_proxy`` is the effective
    bypass list when the proxy is on; changing it changes which services the
    verifier reaches directly, so it is part of the same comparison.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({
        "run_id": run_id,
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "run_type": run_type.name,
        "k": k,
        "conclusive": conclusive,
        # A baseline answers for one commit, so it has a head and no base.
        "window": {"base": base, "head": head},
        "corpus_pin": corpus_pin,
        "baseline": str(baseline) if baseline else None,
        "image": image,
        "harness": {"runner_sha": runner_sha, "dirty": runner_dirty},
        "verifier_proxy": verifier_proxy,
        "verifier_no_proxy": verifier_no_proxy,
        "l2_exit_code": l2_exit,
        "l2": l2,
        # Set when the job was fetched but could not be turned into scores. The
        # trials are still on disk beside this file; what is missing is the
        # reading of them, which is why this is recorded rather than raised.
        "settle_error": settle_error,
        # Set when the results could not be brought back at all. The fetch is
        # attempted whatever harbor exited with, because a failed run's partial
        # trials and trajectories are the diagnosis — but on a run that failed
        # early there may be no remote directory to fetch, and that is an answer
        # rather than an error.
        "fetch_error": fetch_error,
        # Commits no MR carried, and therefore no L1 verdict covers. Recorded
        # so the ledger does not later read their absence as "nothing landed".
        "direct_pushes": [{"sha": sha, "subject": subject} for sha, subject in direct],
        "merges": [
            {
                "sha": record.sha,
                "subject": record.subject,
                "merged_at": record.merged_at,
                "changed_files": len(record.changed_paths),
                "matched_rules": list(record.matched_rules),
                "verdict": record.verdict,
                "trigger": record.trigger,
                "visible_files": list(record.visible_files),
                "review_error": record.review_error,
                "triggers_l2": record.triggers_l2,
            }
            for record in records
        ],
    }, indent=2) + "\n")
    print(f"[daily] manifest -> {path}")


def summarise(records: list[daily_batch.MergeRecord]) -> None:
    for record in records:
        mark = record.verdict.upper()
        if record.matched_rules:
            mark = f"VISIBLE [path rule: {', '.join(record.matched_rules)}]"
        elif record.trigger == "oversize":
            mark = f"VISIBLE [oversize: {record.review_error}]"
        elif record.undetermined:
            mark = f"UNDETERMINED [judge failed: {(record.review_error or '')[:60]}]"
        elif record.cached:
            mark += " [cached]"
        print(f"  {record.sha[:9]}  {len(record.changed_paths):4d} files  {mark}")
        print(f"             {record.subject[:78]}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=HERE.parent)
    parser.add_argument("--run-type", choices=sorted(RUN_TYPES), default="daily",
                        help="daily: k=1 screen over the window. checkpoint: k=3 over "
                             "the window, may conclude. baseline: k=3 reference "
                             "measurement at one commit, no window")
    parser.add_argument("--head", default="origin/main",
                        help="commit to evaluate (default: origin/main)")
    parser.add_argument("--base", help="window start; required on the first run")
    parser.add_argument("--state", type=Path, default=DEFAULT_STATE)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--baselines-dir", type=Path, default=DEFAULT_BASELINES)
    parser.add_argument("--ledger-dir", type=Path, default=DEFAULT_LEDGER)
    parser.add_argument("--project-id", default=DEFAULT_PROJECT)
    parser.add_argument("--no-ledger", action="store_true",
                        help="do not write ledger records (for rehearsals)")
    parser.add_argument("--allow-dirty", action="store_true",
                        help="run with uncommitted changes in benchmarks/. For "
                             "deliberate experiments: the run record says the "
                             "harness was dirty, and a baseline is still refused")
    parser.add_argument("--tasks-dir", type=Path, required=True,
                        help="corpus to run: frontier/ or officeqa-pro/ in a checkout "
                             "of arcships/dim-eval-data, or any tree with its own "
                             "MANIFEST.json")
    parser.add_argument("--policy", type=Path, default=daily_batch.DEFAULT_POLICY)
    parser.add_argument("--prompt", type=Path, default=daily_batch.DEFAULT_PROMPT)
    parser.add_argument("--cache-dir", type=Path,
                        help="L1 verdict cache (default: <out>/l1-cache)")
    parser.add_argument("--remote-judge", action="store_true",
                        help="run the L1 judge on the eval machine")
    parser.add_argument("--host", default=DEFAULT_HOST,
                        help="eval machine, as ssh names it (default: $DIM_EVAL_HOST)")
    parser.add_argument("--remote-python", default=DEFAULT_REMOTE_PYTHON,
                        help="interpreter on the eval machine whose venv has harbor "
                             "(default: $DIM_EVAL_REMOTE_PYTHON)")
    parser.add_argument("--scratch", default=DEFAULT_SCRATCH,
                        help="working directory on the eval machine "
                             "(default: $DIM_EVAL_SCRATCH)")
    parser.add_argument("--tag-prefix", default="dim-agent-under-test")
    parser.add_argument("--image", help="skip the build and run against this image tag")
    parser.add_argument("--k", type=int, help="override the run type's repetitions")
    # Measured, not estimated: three k=3 runs on this corpus took 2.16h, 5.49h
    # and 6.92h. Wall time tracks the number of timed-out trials (each burns its
    # full agent deadline), so a bad run is the long one — exactly when a tight
    # limit would cut it off and throw the whole run away.
    parser.add_argument("--l2-timeout-hours", type=float, default=12.0)
    # A corpus property as much as the timeout is: wall time is roughly
    # tasks x k x agent deadline / concurrency in the bad case, and the bound on
    # it is the eval machine's memory (each trial reserves its task's memory_mb).
    parser.add_argument("--concurrency", type=int, default=5,
                        help="harbor n_concurrent_trials (default: 5, what the "
                             "30-task baselines were measured at)")
    parser.add_argument("--no-fetch", action="store_true",
                        help="do not fetch before resolving the head")
    parser.add_argument("--dry-run", action="store_true",
                        help="judge the window and print the plan; build nothing")
    args = parser.parse_args()
    if not args.dry_run or args.remote_judge:
        missing = [flag for flag, value in (("--host", args.host),
                                            ("--scratch", args.scratch),
                                            ("--remote-python", args.remote_python))
                   if not value]
        if missing:
            parser.error(f"{', '.join(missing)} required (or set the DIM_EVAL_* "
                         "variables, see benchmarks/host/README.md)")

    run_type = RUN_TYPES[args.run_type]
    k = args.k if args.k is not None else run_type.k
    if run_type.conclusive and k < CONCLUSIVE_MIN_K:
        # Refused rather than downgraded. A `baseline` file written from k=1 would
        # be read later as a reference like any other, and nothing in the
        # comparison against it would show that it could not support one.
        raise SystemExit(
            f"--run-type {run_type.name} exists to conclude something and needs "
            f"k>={CONCLUSIVE_MIN_K}; got k={k}. Use --run-type daily for a screen."
        )
    conclusive = run_type.conclusive and k >= CONCLUSIVE_MIN_K

    repo = args.repo.resolve()
    if not args.no_fetch:
        run(["git", "-C", str(repo), "fetch", "--quiet", "origin"])

    pin = corpus_pin(args.tasks_dir)
    baseline = baseline_for(pin, args.baselines_dir)

    if not run_type.judges_window and baseline is not None:
        # Checked here rather than in `settle`, which is where the write happens
        # and therefore where the refusal used to land — after the image was
        # built and L2 had run. The answer does not depend on any of that, so
        # paying 2-7 hours to reach it was pure loss.
        raise SystemExit(
            f"a baseline for corpus pin {pin[:12]} already exists at {baseline}. "
            "Replacing the reference every earlier comparison was made against "
            "is a deliberate change: remove that file in its own commit first."
        )

    ledger = ledger_tools.Ledger(args.ledger_dir)
    recorded: list[str] = []
    disputed: list[str] = []

    window = None
    if run_type.judges_window:
        window = resolve_window(repo, read_state(args.state), args.base, args.head)
        if window is None:
            print("[daily] no new commits on main; nothing to evaluate")
            return 0
        head, base = window.head, window.base
    else:
        head, base = git(repo, "rev-parse", args.head), None

    # Unique per run, and unique by construction rather than by clock.
    #
    # Two runs sharing an id share the remote results directory, and the fetch is
    # an rsync into it, so the second reads back its own trials plus the first's:
    # the job declares 3, the directory holds 6, and it scores as `undeclared`
    # and `not run exactly k times`. The re-run is then invalid for reasons
    # imported from the run it was replacing, and the ledger's write-once refuses
    # its record on top of that. That is the recovery path EXIT_INVALID asks for,
    # so an id of (day, head) made the fix for one bug unreachable.
    #
    # A timestamp alone does not fix it: the cheap failures return in well under
    # a second, so a re-run can land inside the same tick — which is what this
    # did when it was first written to seconds. The timestamp is kept because it
    # sorts, and because it tells a human which of two directories on the eval
    # machine is newer; the token is what makes the id an id.
    run_id = (f"{datetime.now(timezone.utc):%Y-%m-%dT%H%M}"
              f"__{run_type.name}__{head[:9]}__{secrets.token_hex(2)}")
    out = (args.out / run_id).resolve()
    cache_dir = args.cache_dir or (args.out / "l1-cache")

    # Both are needed before the window is judged: the sha addresses the remote
    # harness directory the judge is delivered into, and every early return from
    # here on writes a manifest that records them.
    runner_sha = git(TOOLING_ROOT, "rev-parse", "HEAD")
    runner_dirty = git(TOOLING_ROOT, "status", "--porcelain", "--", HERE.name) != ""
    # Read once and carry them through the manifest, the baseline params and the
    # comparison. The remote run reads the same variables when it builds the job,
    # so the record and the condition cannot drift.
    verifier_proxy = run_l2.verifier_proxy_enabled()
    verifier_no_proxy = run_l2.verifier_no_proxy()

    if runner_dirty:
        # Refused here, before the judge is paid and the image is built. It used
        # to be a warning, and a warning does not protect anything: the run went
        # on to judge the window, build, spend its hours, freeze a baseline and
        # write the ledger, all measured by a harness that exists on one machine
        # and in no commit. "Recorded as dirty but used anyway" is not a trusted
        # evaluator, it is an untrusted one with a note attached.
        #
        # A baseline is refused outright. It is the reference every later
        # comparison is read against, so defining it from uncommitted code makes
        # every one of those untraceable — and committing first costs nothing,
        # because taking a baseline is already a reviewed, deliberate act.
        if run_type.name == "baseline":
            raise SystemExit(
                f"{HERE.name}/ has uncommitted changes, so this harness is not "
                "the one on main. A baseline taken with it would be a reference "
                "nobody can reconstruct. Commit first."
            )
        if not args.allow_dirty:
            raise SystemExit(
                f"{HERE.name}/ has uncommitted changes, so this run would not be "
                "scored by the committed harness. Commit them, or pass "
                "--allow-dirty for a deliberate experiment — its ledger record "
                "will say the ruler was dirty."
            )
        print(f"[daily] WARNING: {HERE.name}/ has uncommitted changes; --allow-dirty "
              "was given, so this run is recorded as measured by a dirty harness")

    records: list[daily_batch.MergeRecord] = []
    direct: tuple[tuple[str, str], ...] = ()
    triggering: list[daily_batch.MergeRecord] = []
    undetermined: list[daily_batch.MergeRecord] = []

    if window is None:
        print(f"[daily] run {run_id}  {run_type.name} k={k}  head {head[:9]}")
        print("[daily] reference measurement: no window judged, no state advanced")
    else:
        print(f"[daily] run {run_id}  {run_type.name} k={k}  window {window.label}")
        if args.remote_judge:
            # Delivered before judging, not with the rest of the harness: the
            # harness ships just before L2, and by then every verdict is already
            # decided.
            os.environ["DIM_EVAL_JUDGE_SCRIPT"] = deliver_judge(
                args.host, args.scratch, runner_sha
            )
            os.environ.setdefault("DIM_EVAL_JUDGE_HOST", args.host)
        records = judge(repo, window, args.policy, args.prompt, cache_dir,
                        args.remote_judge)
        summarise(records)

        direct = direct_pushes(repo, window)
        if direct:
            print(f"[daily] WARNING: {len(direct)} commit(s) went straight to main "
                  "and carry no MR to judge; L2 runs on their account")
            for sha, subject in direct:
                print(f"  {sha[:9]}  UNJUDGED  {subject[:60]}")

        if not args.no_ledger:
            # Written before the build and the run, because the L1 verdict is
            # already final here: a build that fails must not cost the ledger
            # the judgements this run paid a judge to produce.
            recorded, disputed = record_merges(ledger, records, args.project_id)

        triggering = [r for r in records if r.triggers_l2]
        undetermined = [r for r in records if r.undetermined]
        print(f"\n[daily] {len(records)} merge(s), {len(triggering)} triggering L2"
              + (f", {len(undetermined)} undetermined" if undetermined else "")
              + (f", {len(direct)} direct push(es)" if direct else ""))

        if records and len(undetermined) == len(records):
            # Not one verdict came back. Running L2 on this would cost hours to
            # measure a window nothing judged, and reporting it as a warning is
            # how a stalled pipeline stays green.
            print("[daily] the judge decided nothing at all; not running L2. "
                  "Check the judge on the eval machine, then re-run.")
            write_manifest(out / "run.json", image=None, l2_exit=None,
                           run_id=run_id, run_type=run_type, k=k,
                           conclusive=conclusive, head=head, base=base,
                           corpus_pin=pin, baseline=baseline, records=records,
                           direct=direct, runner_sha=runner_sha,
                           runner_dirty=runner_dirty,
                           verifier_proxy=verifier_proxy,
                           verifier_no_proxy=verifier_no_proxy)
            return EXIT_JUDGE_DOWN

    if baseline is None:
        # Not fatal: the first run on a new corpus pin is how a baseline gets
        # taken. It is said out loud because every later comparison depends on it,
        # and because run_l2's model-identity guard cannot fire without one.
        print(f"[daily] no baseline for corpus pin {pin[:12]}; nothing to compare "
              "against and the model-identity guard cannot fire")

    evidence = dict(
        run_id=run_id, run_type=run_type, k=k, conclusive=conclusive,
        head=head, base=base, corpus_pin=pin, baseline=baseline,
        records=records, direct=direct,
        runner_sha=runner_sha, runner_dirty=runner_dirty,
        verifier_proxy=verifier_proxy,
        verifier_no_proxy=verifier_no_proxy,
    )

    if args.dry_run:
        write_manifest(out / "run.json", image=None, l2_exit=None, **evidence)
        print("[daily] dry run; nothing built, nothing run, state unchanged")
        return EXIT_UNDETERMINED if undetermined else 0

    if window is not None and not triggering and not direct:
        # Nothing model-visible landed, so there is nothing for L2 to say. The
        # window still closes: leaving it open would attribute these merges to a
        # later run that had no reason to suspect them.
        write_manifest(out / "run.json", image=None, l2_exit=None, **evidence)
        if undetermined:
            print("[daily] holding the window: the judge never decided "
                  f"{len(undetermined)} merge(s)")
            return EXIT_UNDETERMINED
        write_state(args.state, window.head, run_id)
        print("[daily] no model-visible change; L2 skipped")
        return 0

    if args.image:
        # The tag carries the commit's short sha, so a reused image still says
        # which build it holds. A baseline recording no subject at all would be a
        # reference nobody could later trace to code.
        tag, identity = args.image, {"image_tag": args.image}
        # And it must be the commit this run says it is measuring. `--head`
        # defaults to whatever main points at now, which moves under a long
        # session: reusing an image built minutes ago then labels its results
        # with a commit it never contained. Refused rather than reconciled —
        # both values are plausible and only the operator knows which was meant.
        suffix = tag.rsplit(":", 1)[-1]
        if suffix and suffix != "local" and not head.startswith(suffix):
            raise SystemExit(
                f"--image {tag} was built from {suffix}, but this run resolved "
                f"head {head[:9]}. Pass --head {suffix} to measure the image you "
                f"have, or drop --image to build {head[:9]}."
            )
        print(f"[daily] reusing image {tag}")
    else:
        tag, identity = build_subject(repo, head, out / "build", args.host,
                                      args.scratch, args.tag_prefix)

    remote = deliver_runner(args.host, args.scratch, runner_sha,
                            args.baselines_dir, args.tasks_dir)
    # A baseline defines the reference; it does not check itself against the one it
    # is replacing. Every other run type does.
    remote_baseline = (f"{remote}/baselines/{pin}.json"
                       if baseline and run_type.judges_window else None)
    harbor_version = remote_harbor_version(args.host, args.remote_python)
    print(f"[daily] harbor {harbor_version} on {args.host}")
    l2_exit, remote_results = run_l2_remote(
        args.host, remote, tag, run_id, k,
        args.l2_timeout_hours * 3600, remote_baseline, args.remote_python,
        concurrency=args.concurrency,
    )
    l2_summary = None
    settle_error = None
    # Fetched whatever the exit code, because the two uses of the fetch are not
    # the same use. Scoring needs a whole job and only runs on exit 0; a person
    # diagnosing a failure needs whatever is there, and a failed harbor still
    # leaves trajectories and partial trials behind. Exit 124 is the case that
    # made this obvious: ssh gave up on a run that is still going, and that was
    # the one where the artifact held an exit code and nothing else.
    fetch_error = fetch_results(args.host, remote_results, run_id, out)
    if l2_exit == 0:
        if fetch_error is not None:
            # Nothing to score and nothing to score it from. Recorded the same
            # way a scoring failure is, because from here on it is the same
            # situation: the run happened, its results are unreadable, and the
            # window must not move over it.
            settle_error = fetch_error
        else:
            try:
                l2_summary = settle(
                    out, run_id, run_type, k, pin, baseline, identity, runner_sha,
                    params={"timeout_multiplier": 1.0,
                            "agent_timeout_multiplier": 1.0,
                            "n_concurrent_trials": args.concurrency,
                            "harbor_version": harbor_version,
                            "verifier_proxy": verifier_proxy,
                            "verifier_no_proxy": verifier_no_proxy},
                    baselines_dir=args.baselines_dir,
                )
            except Exception as error:  # noqa: BLE001 - see below
                # Deliberately every exception. Scoring reads files a remote
                # machine wrote and compares them against a stored reference, so
                # it can fail in ways this file does not enumerate — and each of
                # those used to propagate out of `main`, past the manifest write
                # below, destroying the only record of a run that had already
                # cost its hours. Losing the evidence is worse than any of the
                # failures that produce it.
                settle_error = f"{type(error).__name__}: {error}"
                print(f"[daily] could not score the job: {settle_error}")

    # The manifest goes first. It is the only record that survives a refused
    # ledger write, and a refusal is not hypothetical: the store is write-once,
    # so re-running the same head on the same day hits it.
    write_manifest(out / "run.json", image=identity, l2_exit=l2_exit,
                   l2=l2_summary, settle_error=settle_error,
                   fetch_error=fetch_error, **evidence)

    if not args.no_ledger:
        # Unconditional, including the paths where there is no summary. The run
        # record is the only long-lived statement that L2 was attempted on this
        # window; without one a failed harbor is indistinguishable, months later,
        # from a window L2 never looked at. The artifacts that hold the detail
        # expire in 30 days, the ledger does not.
        try:
            record_run(
                ledger, run_id=run_id, run_type=run_type, k=k, pin=pin,
                subject_commit=head, window=window, head=head, covers=recorded,
                unjudged=tuple(sha for sha, _ in direct), summary=l2_summary,
                l2_exit=l2_exit, settle_error=settle_error,
                runner_dirty=runner_dirty, disputed=disputed,
            )
        except ledger_tools.LedgerError as error:
            # Write-once must refuse the write, not destroy the evidence for it.
            print(f"[daily] WARNING: ledger refused the run record: {error}")

    if settle_error is not None:
        # The harbor output is fetched and sits beside the manifest; it is the
        # reading of it that failed, so re-running is what fixes this and the
        # window must not move past a batch no score exists for.
        if window is not None:
            print(f"[daily] window held at {window.base[:9]}: the job was run "
                  "but never scored")
        print(f"[daily] results kept at {out}/harbor for inspection")
        return EXIT_INVALID

    if l2_exit != 0:
        print(f"[daily] L2 exited {l2_exit}")
        if fetch_error is None:
            print(f"[daily] what the failed run left is at {out}/harbor")
        if window is not None:
            # The window stays open. Advancing it here would drop these merges out
            # of every future run's range, and a regression they carry would then
            # be attributed to whatever ran next.
            print(f"[daily] window held at {window.base[:9]}")
        return EXIT_L2_FAILED
    if undetermined:
        print(f"[daily] L2 completed, but {len(undetermined)} merge(s) were never "
              "judged; window held so they are judged again")
        return EXIT_UNDETERMINED

    unscoreable = (l2_summary or {}).get("unscoreable")
    if unscoreable:
        # harbor exiting 0 says the job ran, not that it can be scored. Those are
        # different claims, and only the second one licenses moving the window:
        # advancing here drops this batch out of every future run's range, so a
        # regression it carries gets attributed to whatever runs next — and the
        # ledger already recorded the run `invalid`, which is the pipeline
        # contradicting itself one line apart.
        #
        # It applies to a baseline run too, where there is no window to hold. That
        # run wrote no baseline, and returning 0 for it is the same green-over-
        # nothing this file objects to everywhere else.
        for problem in unscoreable:
            print(f"[daily]   not scoreable: {problem}")
        if window is not None:
            print(f"[daily] window held at {window.base[:9]}: "
                  "this batch has not been measured yet")
        print("[daily] run recorded invalid; it must be re-run, not re-judged")
        return EXIT_INVALID

    if window is not None:
        write_state(args.state, window.head, run_id)

    # Anything still here is a comparison problem: the unscoreable ones returned
    # above. The measurement stands and the window has moved; what is missing is
    # a reference to read it against.
    problems = (l2_summary or {}).get("problems")
    if problems:
        for problem in problems:
            print(f"[daily]   {problem}")
        print("[daily] measured, but with nothing to compare it against. No "
              "re-run changes that — take a baseline (--run-type baseline, "
              "reviewed, not from the schedule) or run at the baseline's k.")
        return EXIT_NOT_COMPARABLE

    print(f"[daily] done: {out}"
          + ("" if conclusive else "  (k<%d: screening only, not a verdict)" % CONCLUSIVE_MIN_K))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
