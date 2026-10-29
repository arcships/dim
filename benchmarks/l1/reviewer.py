"""Reviewer invocation and strict output validation.

The reviewer is a model, so its output is untrusted in two directions. It can be
malformed — invalid JSON, an unknown axis, a file judged twice or not at all — and
the diff it reads can contain text addressed to it. Both are handled the same way:
the output is validated against the file list computed from git, and anything that
does not line up exactly is rejected.

Rejection is not an error path that falls back to a guess. `ReviewerError` reduces
to `visible`, because an output that cannot be checked is an output that cannot be
trusted to say `invisible`.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass

from .agent_reviewer import DEADLINE_SEC
from .decision import Axis, FileVerdict

#: A reviewer that runs longer than this has stopped making progress. Taken from
#: the default judge rather than set here: a separate number drifted below the
#: judge's own retries and killed it while it was still working.
DEFAULT_TIMEOUT_SEC = DEADLINE_SEC


class ReviewerError(RuntimeError):
    """The reviewer failed, or produced output that could not be validated."""


@dataclass(frozen=True)
class ReviewerConfig:
    """How to run the reviewer.

    `command` receives the prompt and diff on stdin and writes one JSON object per
    changed file on stdout. Keeping it a command rather than an in-process client
    is what lets the judge model be pinned and swapped without touching L1.
    """

    command: tuple[str, ...]
    image_digest: str
    model_snapshot: str
    timeout_sec: float = DEFAULT_TIMEOUT_SEC


def build_input(prompt: str, patch: str) -> str:
    """Frame the diff as data.

    The diff is quoted material, not instruction. The prompt already tells the
    reviewer to ignore directives found inside it; the fence makes the boundary
    explicit so an injected line cannot read as part of the prompt itself.
    """
    return f"{prompt}\n\n<diff>\n{patch}\n</diff>\n"


def parse_output(raw: str, expected_paths: tuple[str, ...]) -> tuple[FileVerdict, ...]:
    """Validate reviewer output against the file list git produced.

    Coverage is checked both ways. A missing file means something went unjudged; a
    duplicate means two verdicts disagree with no way to choose; an unexpected path
    means the reviewer invented a file, which puts the rest of its output in doubt.
    """
    verdicts: dict[str, FileVerdict] = {}
    for number, line in enumerate(raw.splitlines(), start=1):
        # The trailing comma is a JSON array written one element per line, which
        # the judge sometimes emits instead of one object per line; the bracket
        # lines around it fall through as prose.
        line = line.strip().removesuffix(",")
        if not line or line.startswith("```"):
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            # Summary lines and prose are expected around the per-file records;
            # only structurally valid objects carrying a path are considered.
            continue
        if not isinstance(entry, dict) or "file" not in entry:
            continue

        path = str(entry["file"])
        if path in verdicts:
            raise ReviewerError(f"line {number}: duplicate verdict for {path!r}")

        verdict = entry.get("verdict")
        if verdict not in ("visible", "invisible"):
            raise ReviewerError(f"line {number}: unknown verdict {verdict!r} for {path!r}")

        trace = str(entry.get("trace", "")).strip()
        if not trace:
            # A verdict with no traceable reason cannot be checked by a human, and
            # an unverifiable `invisible` is the failure this whole layer exists to
            # avoid.
            raise ReviewerError(f"line {number}: empty trace for {path!r}")

        axes = _parse_axes(entry.get("axes", entry.get("axis")), verdict, path, number)
        verdicts[path] = FileVerdict(
            path=path, verdict=verdict, axes=axes, trace=trace
        )

    expected = set(expected_paths)
    seen = set(verdicts)
    missing = sorted(expected - seen)
    unexpected = sorted(seen - expected)
    if missing:
        raise ReviewerError(f"no verdict for changed file(s): {missing}")
    if unexpected:
        raise ReviewerError(f"verdict for file(s) not in the diff: {unexpected}")

    return tuple(verdicts[path] for path in expected_paths if path in verdicts)


def _parse_axes(
    value: object, verdict: str, path: str, line_number: int
) -> tuple[Axis, ...]:
    """Read `axes`, tolerating the older singular `axis` spelling."""
    if value is None or value == []:
        if verdict == "visible":
            raise ReviewerError(f"line {line_number}: visible {path!r} declares no axis")
        return ()
    raw = value if isinstance(value, list) else [value]
    axes: list[Axis] = []
    for item in raw:
        try:
            axis = Axis(str(item))
        except ValueError as error:
            raise ReviewerError(
                f"line {line_number}: unknown axis {item!r} for {path!r}"
            ) from error
        if axis not in axes:
            axes.append(axis)
    if verdict == "invisible" and axes:
        raise ReviewerError(
            f"line {line_number}: invisible {path!r} declares axes {axes}"
        )
    return tuple(axes)


def run_reviewer(
    config: ReviewerConfig, prompt: str, patch: str, expected_paths: tuple[str, ...]
) -> tuple[FileVerdict, ...]:
    try:
        result = subprocess.run(
            list(config.command),
            input=build_input(prompt, patch),
            capture_output=True,
            text=True,
            timeout=config.timeout_sec,
        )
    except subprocess.TimeoutExpired as error:
        raise ReviewerError(f"reviewer timed out after {config.timeout_sec}s") from error
    except OSError as error:
        raise ReviewerError(f"reviewer could not be started: {error}") from error

    if result.returncode != 0:
        raise ReviewerError(
            f"reviewer exited {result.returncode}: {result.stderr.strip()[:400]}"
        )
    return parse_output(result.stdout, expected_paths)
