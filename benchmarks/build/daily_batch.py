"""Identify the MRs in a day's batch and run the L1 path rules over them.

L2 moved from blocking a single MR to checking a merged batch, so the unit of
attribution is a range of main rather than one MR. This resolves that range into
its merge commits, canonicalises each one's diff, and applies the path rules.

The path rules are only half of L1: files they do not claim go to the review
agent, which decides model visibility. That reviewer is a pluggable subprocess
(see l1/reviewer.py) and is not wired here — a batch whose files are all
unclaimed is reported as needing review, not as skippable.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCHMARKS = HERE.parent
sys.path.insert(0, str(BENCHMARKS))

from l1.agent_reviewer import DEFAULT_MODEL as JUDGE_MODEL  # noqa: E402
from l1.cache import VerdictCache  # noqa: E402
from l1.decision import L1Decision  # noqa: E402
from l1.diff import canonical_diff  # noqa: E402
from l1.policy import load_policy  # noqa: E402
from l1.reduce import MAX_JUDGED_DIFF_BYTES, decide  # noqa: E402,F401
from l1.reviewer import ReviewerConfig  # noqa: E402

DEFAULT_POLICY = BENCHMARKS / "policy" / "always-eval.toml"
DEFAULT_PROMPT = BENCHMARKS.parent / "docs" / "eval" / "l1-review-agent-prompt.md"

#: The judge runs as a subprocess so the model can be pinned and swapped without
#: touching L1. It is not `dim exec`: the judge must not be the agent under test.
#: Addressed by path rather than as `-m l1.agent_reviewer`, because the subprocess
#: inherits neither this process's cwd nor its sys.path.
REVIEWER_COMMAND = (sys.executable, str(BENCHMARKS / "l1" / "agent_reviewer.py"))


def judge_code_digest() -> str:
    """The judge's identity when it has no image: the L1 sources themselves.

    A cached verdict is reusable only while the code that produced and validated
    it is unchanged. With a constant here, an edit to how the judge calls the
    model or how its output is checked would silently inherit verdicts the old
    code gave. Tests are left out; nothing a verdict depends on lives in them.
    """
    digest = hashlib.sha256()
    for path in sorted((BENCHMARKS / "l1").iterdir()):
        if path.suffix in (".py", ".sh") and path.name != "flow_test.py":
            digest.update(path.name.encode() + b"\0" + path.read_bytes() + b"\0")
    return f"sha256:{digest.hexdigest()}"


@dataclass(frozen=True)
class MergeRecord:
    sha: str
    subject: str
    merged_at: str
    changed_paths: tuple[str, ...]
    matched_rules: tuple[str, ...]
    verdict: str = "unreviewed"
    trigger: str = "unreviewed"
    #: The decision's identity — what question was asked, not what was answered.
    #: The ledger records it so two records claiming different verdicts for the
    #: same question read as the contradiction they are.
    identity: str = ""
    visible_files: tuple[str, ...] = ()
    review_error: str | None = None
    cached: bool = False

    @property
    def triggers_l2(self) -> bool:
        return bool(self.matched_rules) or self.verdict == "visible"

    @property
    def undetermined(self) -> bool:
        """Fail-open is not a verdict. It triggers L2 to stay safe, but counting it
        as `visible` would hide a judge that is down: every MR would trigger, the
        batch would look busy, and nothing would say the judging never happened."""
        return self.trigger == "fail_open"


def git(repo: Path, *args: str) -> str:
    import subprocess
    result = subprocess.run(["git", "-C", str(repo), *args],
                            check=True, text=True, capture_output=True)
    return result.stdout.strip()


def merges_in_range(repo: Path, base: str, head: str) -> list[tuple[str, str, str]]:
    """Merge commits in (base, head], oldest first — the order they landed."""
    raw = git(repo, "log", "--merges", "--reverse", "--format=%H|%cI|%s", f"{base}..{head}")
    out = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        sha, when, subject = line.split("|", 2)
        out.append((sha, when, subject))
    return out


def _digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def _record(sha: str, subject: str, when: str,
            decision: L1Decision) -> MergeRecord:
    """A decision, as the ledger wants to read it back.

    The two shapes differ because they answer different questions: L1Decision
    says what was asked and what came back, MergeRecord says which merge it was
    about. Everything below is a rename or a projection — no verdict is decided
    here, which is the point of the split.
    """
    return MergeRecord(
        # Full sha: the ledger joins a batch to the commits it covers, and a
        # short sha is not a key. Display truncates; the record does not.
        sha=sha,
        subject=subject,
        merged_at=when,
        changed_paths=decision.changed_paths,
        matched_rules=decision.matched_rules,
        verdict=decision.verdict,
        trigger=decision.trigger,
        visible_files=decision.visible_files,
        # `reason` is always set; only the two failure triggers carry one worth
        # surfacing as an error, and flattening the rest into this field would
        # make every ordinary verdict look like it had a problem.
        review_error=(decision.reason[:200]
                      if decision.trigger in ("fail_open", "oversize") else None),
        cached=decision.cached,
        identity=decision.identity,
    )


def collect(repo: Path, base: str, head: str, policy_path: Path,
            review: bool = False, prompt_path: Path = DEFAULT_PROMPT,
            cache_dir: Path | None = None,
            command: tuple[str, ...] = REVIEWER_COMMAND) -> list[MergeRecord]:
    """Resolve a window into merges and decide each one.

    The deciding is `l1.decide`, not a second copy of it living here. It used to
    be a copy, and the two drifted: this one crashed the run on an unreadable
    policy where L1's contract is to fail open, skipped L1's empty-diff
    short-circuit and paid a judge call for a merge that changed no files, and
    digested the policy file as text where L1 digests it as bytes, so the two
    computed different cache keys for the same question.
    """
    config = ReviewerConfig(
        command=command,
        image_digest=judge_code_digest(),
        model_snapshot=os.environ.get("DIM_EVAL_JUDGE_MODEL", JUDGE_MODEL),
    )
    # A judge that cannot be reached leaves the batch half-decided. The cache is
    # what makes the retry cheap: only the MRs that never got a verdict are asked
    # again. It is keyed on the question — diff, policy, prompt, judge code and
    # model — so a change to any of them invalidates it on its own.
    cache = VerdictCache(cache_dir) if cache_dir else None
    records = []
    for sha, when, subject in merges_in_range(repo, base, head):
        if not review:
            # Inspection only: what would the path rules say, without spending a
            # judge call. It produces no verdict, and the ledger's schema refuses
            # `unreviewed`, so nothing from this branch can be recorded — which is
            # why it does not go through `decide`.
            diff = canonical_diff(str(repo), f"{sha}^1", sha)
            paths = tuple(f.path for f in diff.files)
            records.append(MergeRecord(
                sha=sha, subject=subject, merged_at=when, changed_paths=paths,
                matched_rules=tuple(load_policy(policy_path).match(paths)),
            ))
            continue
        # An MR's own change is its first parent to the merge result; diffing the
        # two parents would also pull in whatever main advanced by meanwhile.
        records.append(_record(sha, subject, when, decide(
            repo=str(repo), base_sha=f"{sha}^1", head_sha=sha,
            policy_path=policy_path, prompt_path=prompt_path,
            reviewer=config, cache=cache,
        )))
    return records


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=BENCHMARKS.parent)
    parser.add_argument("--base", required=True, help="last evaluated commit")
    parser.add_argument("--head", required=True, help="commit being built")
    parser.add_argument("--policy", type=Path, default=DEFAULT_POLICY)
    parser.add_argument("--json", action="store_true", help="emit the batch as JSON")
    parser.add_argument("--review", action="store_true",
                        help="ask the review agent about files no path rule claims")
    parser.add_argument("--prompt", type=Path, default=DEFAULT_PROMPT)
    parser.add_argument("--cache-dir", type=Path,
                        help="reuse verdicts across runs so a judge outage only "
                             "costs the MRs it actually failed on")
    parser.add_argument("--remote-judge", action="store_true",
                        help="run the judge on the eval machine instead of locally")
    args = parser.parse_args()

    command = ((str(BENCHMARKS / "l1" / "remote_reviewer.sh"),)
               if args.remote_judge else REVIEWER_COMMAND)
    records = collect(args.repo.resolve(), args.base, args.head, args.policy,
                      review=args.review, prompt_path=args.prompt,
                      cache_dir=args.cache_dir, command=command)
    triggering = [r for r in records if r.triggers_l2]

    if args.json:
        print(json.dumps({
            "base": args.base,
            "head": args.head,
            "merge_count": len(records),
            "path_rule_hits": len(triggering),
            "merges": [
                {"sha": r.sha, "subject": r.subject, "merged_at": r.merged_at,
                 "files": len(r.changed_paths), "matched_rules": list(r.matched_rules),
                 "verdict": r.verdict, "visible_files": list(r.visible_files),
                 "review_error": r.review_error}
                for r in records
            ],
        }, indent=2))
        return 0

    print(f"batch {args.base[:9]}..{args.head[:9]} — {len(records)} MR(s)\n")
    for r in records:
        if r.matched_rules:
            mark = f"PATH RULE: {', '.join(r.matched_rules)}"
        elif r.trigger == "oversize":
            mark = f"VISIBLE [oversize: {r.review_error}]"
        elif r.verdict == "unreviewed":
            mark = "needs review agent (pass --review)"
        elif r.undetermined:
            mark = f"UNDETERMINED -> L2 [judge failed: {r.review_error[:60]}]"
        else:
            mark = r.verdict.upper()
            if r.visible_files:
                mark += f" ({len(r.visible_files)} file(s))"
            if r.cached:
                mark += " [cached]"
        print(f"  {r.sha[:9]}  {len(r.changed_paths):4d} files  {mark}")
        print(f"             {r.subject[:78]}")
    rule_hits = sum(1 for r in records if r.matched_rules)
    undetermined = [r for r in records if r.undetermined]
    cached = sum(1 for r in records if r.cached)
    print(f"\ntriggering L2: {len(triggering)}/{len(records)}"
          f"  (path rules {rule_hits}, judged visible "
          f"{len(triggering) - rule_hits - len(undetermined)}, "
          f"undetermined {len(undetermined)})"
          + (f"  [{cached} from cache]" if cached else ""))
    if undetermined:
        # Exit non-zero so a scheduled run surfaces a judge outage instead of
        # reporting a batch where everything "triggered".
        print(f"\nWARNING: {len(undetermined)} MR(s) were never judged; they trigger "
              f"L2 to stay safe, but L1 did not decide. Re-run with the same "
              f"--cache-dir to retry only these.")
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
