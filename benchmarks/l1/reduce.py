"""The reducer: turns a diff into one decision.

Order matters and is not an optimisation. Path rules run before the reviewer
because a lockfile bump has nothing to trace; asking a model to judge a version
number produces variance, not information.

Every exception below resolves to `visible`. That is the layer's contract: L1 can
only add L2 runs, never withhold one, so its own failures must cost compute rather
than coverage.
"""

from __future__ import annotations

from pathlib import Path

from .cache import VerdictCache
from .decision import L1Decision, ReviewerIdentity
from .diff import DiffError, canonical_diff
from .policy import PolicyError, load_policy
from .reviewer import ReviewerConfig, ReviewerError, run_reviewer

#: Largest diff worth sending to the judge.
#:
#: Sized for the first judge's endpoint, which refused a prompt over 880,000
#: tokens ("prompt too long") and whose estimator counted 2 bytes per token on
#: adversarial input, so 1.4 MB is ~700k tokens. The current judge's window is
#: 1,048,576 tokens shared with up to 64,000 of output, which still leaves room
#: for the prompt and the reply at that density.
#:
#: Measured over 40 merges on main: the median diff is 27 KiB and the 90th
#: percentile 1.0 MiB, so this cap is not a routine constraint — it catches
#: generated files, where a one-line 3.9 MB JSON turns a one-field edit into a
#: 7.8 MB diff that is 99.99% noise.
#:
#: Raising it does not help: the ceiling belongs to the model deployment, not to
#: any setting here. Reading such a diff properly needs a judge that fetches what
#: it needs instead of being handed everything — see the Open items.
#:
#: Counted in characters rather than bytes despite the name, which understates a
#: non-ASCII diff by up to 3x. Left as it was when this moved here, so that the
#: move changes no verdicts; the end result is a diff that overruns the endpoint
#: and fails open, which is the safe direction but not the intended one.
MAX_JUDGED_DIFF_BYTES = 1_400_000

#: Used when the diff or policy could not be read at all, so their real digests are
#: unknown. It keeps the decision serialisable while making the gap obvious, and —
#: because identity feeds the cache — keeps unreadable inputs from ever being
#: cached as a reusable answer.
UNKNOWN_DIGEST = "unavailable"


def decide(
    *,
    repo: str,
    base_sha: str,
    head_sha: str,
    policy_path: Path,
    prompt_path: Path,
    reviewer: ReviewerConfig,
    cache: VerdictCache | None = None,
    max_diff_bytes: int = MAX_JUDGED_DIFF_BYTES,
) -> L1Decision:
    prompt_digest = UNKNOWN_DIGEST
    policy_digest = UNKNOWN_DIGEST
    diff_digest = UNKNOWN_DIGEST

    def fail_open(reason: str) -> L1Decision:
        return L1Decision(
            base_sha=base_sha,
            head_sha=head_sha,
            verdict="visible",
            trigger="fail_open",
            reason=reason,
            diff_digest=diff_digest,
            policy_digest=policy_digest,
            reviewer=ReviewerIdentity(
                image_digest=reviewer.image_digest,
                model_snapshot=reviewer.model_snapshot,
                prompt_digest=prompt_digest,
            ),
        )

    try:
        prompt = prompt_path.read_text()
    except OSError as error:
        return fail_open(f"reviewer prompt unreadable: {error}")
    prompt_digest = _digest(prompt)

    try:
        policy = load_policy(policy_path)
    except PolicyError as error:
        return fail_open(f"path policy unusable: {error}")
    policy_digest = policy.digest

    try:
        diff = canonical_diff(repo, base_sha, head_sha)
    except DiffError as error:
        return fail_open(f"diff unreadable: {error}")
    diff_digest = diff.digest

    identity = ReviewerIdentity(
        image_digest=reviewer.image_digest,
        model_snapshot=reviewer.model_snapshot,
        prompt_digest=prompt_digest,
    )

    def build(**overrides) -> L1Decision:
        return L1Decision(
            base_sha=base_sha,
            head_sha=head_sha,
            diff_digest=diff_digest,
            policy_digest=policy_digest,
            reviewer=identity,
            changed_paths=diff.paths,
            **overrides,
        )

    if not diff.files:
        return build(
            verdict="invisible",
            trigger="path_rule",
            reason="empty diff: no files changed",
        )

    matched = policy.match(diff.paths)
    if matched:
        return build(
            verdict="visible",
            trigger="path_rule",
            reason=f"path rule(s) matched: {', '.join(matched)}",
            matched_rules=matched,
            visible_files=tuple(
                path
                for path in diff.paths
                if policy.match((path,))
            ),
        )

    if len(diff.patch) > max_diff_bytes:
        # Untraceable resolves to visible, and this is untraceable: the judge
        # cannot be shown the change at all. Its own trigger, so it does not read
        # as a judged verdict and so the visible rate can show how much of L1 is
        # being decided by size rather than by reasoning.
        #
        # Not cached, and not for the reason fail_open is not cached. This one is
        # a deterministic function of the diff: re-deriving it costs a `len`, and
        # storing it would only add an entry that can never differ.
        return build(
            verdict="visible",
            trigger="oversize",
            reason=(f"diff is {len(diff.patch) / 1e6:.1f} MB, over the "
                    f"{max_diff_bytes / 1e6:.1f} MB the judge can read"),
        )

    # Identity is known now, so a cached answer to this exact question can be
    # reused. Only the reviewer path is cached: the path-rule branch is already
    # deterministic and cheap.
    probe = build(verdict="visible", trigger="agent", reason="probe")
    if cache is not None:
        cached = cache.get(probe.identity)
        if cached is not None:
            return cached

    reviewable = tuple(file.path for file in diff.files)
    try:
        verdicts = run_reviewer(reviewer, prompt, diff.patch, reviewable)
    except ReviewerError as error:
        return fail_open(f"reviewer output rejected: {error}")

    visible_files = tuple(v.path for v in verdicts if v.verdict == "visible")
    decision = build(
        verdict="visible" if visible_files else "invisible",
        trigger="agent",
        reason=(
            f"{len(visible_files)} of {len(verdicts)} file(s) judged visible"
            if visible_files
            else f"all {len(verdicts)} file(s) judged invisible"
        ),
        visible_files=visible_files,
        file_verdicts=verdicts,
    )
    if cache is not None:
        cache.put(decision)
    return decision


def _digest(text: str) -> str:
    import hashlib

    return hashlib.sha256(text.encode()).hexdigest()
