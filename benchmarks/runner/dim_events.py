"""Totals from a dimcode Timeline export (``dimcode export``).

The export is dimcode's read path for complete runs
(docs/design/appserver/timeline-export.md): one Session's whole Timeline,
every branch included. The parsing lives in its own module because the adapter
that drives dimcode is imported into harbor's process, and harbor only exists on
the eval machine. This module has no harbor dependency, which is what lets the
runner's flow tests assert when a trial's totals may be compared — the one
property the cost gate depends on — without an eval machine.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

MAIN_BRANCH = "main"


@dataclass(frozen=True)
class Totals:
    """What one trial's Timeline said about its usage and activity."""

    n_input_tokens: int
    n_output_tokens: int
    n_cache_tokens: int
    turns: int
    tool_calls: int
    uncached_input_tokens: int
    end_reason: str | None
    subagent_runs: int
    usage_complete: bool


def read_totals(export_file: Path) -> Totals | None:
    """Read ``export_file``, or None when the trial left no export to read.

    Usage is summed over every ``model/usage`` event of every branch, so work a
    subagent did in a child branch is counted with the parent's. Turns are the
    main branch's model responses and tool calls the calls it made; the gate
    treats both as first-class metrics.

    ``usage_complete`` is False unless the export ran to its ``end`` line with
    the counts it wrote, and every turn the main branch started reached
    ``turn/end``. A run killed at the agent
    deadline settles nothing, and its usage is unknown, not what the partial
    Timeline sums to. Marked complete, that partial sum would enter the cost
    comparison as a measurement.
    """
    if not export_file.exists():
        return None

    prompt_tokens = completion_tokens = cache_read_tokens = 0.0
    turns = 0
    tool_calls = 0
    subagent_runs = 0
    end_reason: str | None = None
    open_turns: set[str] = set()
    turn_ended = False
    export_ended = False
    branches = events = 0

    with export_file.open(errors="ignore") as handle:
        for line in handle:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            kind = record.get("kind")
            if kind == "branch":
                branches += 1
                subagent_runs += "parent" in record["info"]
            elif kind == "end":
                export_ended = (record["branches"], record["events"]) == (branches, events)
            elif kind == "event":
                events += 1
                event = record["event"]
                name = f"{event['type']}/{event['name']}"
                main = record["branch_id"] == MAIN_BRANCH
                if name == "model/usage":
                    usage = event["data"]["usage"]
                    prompt_tokens += usage["promptTokens"]
                    completion_tokens += usage["completionTokens"]
                    cache_read_tokens += usage.get("cacheReadTokens", 0)
                    turns += main
                elif name == "step/tool-call" and main:
                    tool_calls += len(event["data"]["calls"])
                elif name == "turn/start" and main:
                    open_turns.add(event["data"]["turn_id"])
                elif name == "turn/end" and main:
                    open_turns.discard(event["data"].get("turn_id"))
                    turn_ended = True
                    end_reason = event["data"].get("reason") or event["data"].get("status")

    return Totals(
        n_input_tokens=int(prompt_tokens),
        n_output_tokens=int(completion_tokens),
        n_cache_tokens=int(cache_read_tokens),
        turns=turns,
        tool_calls=tool_calls,
        uncached_input_tokens=int(max(prompt_tokens - cache_read_tokens, 0)),
        end_reason=end_reason,
        subagent_runs=subagent_runs,
        usage_complete=export_ended and turn_ended and not open_turns,
    )
