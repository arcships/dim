"""Harbor agent adapter for dimcode.

The dimcode under test is mounted read-only into the task environment as a
prebuilt image (see ``benchmarks/README.md`` — 被测对象与评测工具分离).
``install()`` therefore never touches the network: every L2 task runs under a
``no-network`` baseline, and only the model API host is opened for the agent
phase through the trial-level ``extra_allowed_hosts``. The task definition is
never modified.

Metrics come from the Timeline export (``dimcode export``), dimcode's read path
for complete runs. Cost and turns are Primary gate metrics, so they are
extracted here rather than reconstructed later from trajectories.
"""

from __future__ import annotations

import json
import shlex
import sys
import tomllib
from pathlib import Path
from typing import ClassVar, override

from harbor.agents.installed.base import (
    ApiInternalServerError,
    ApiOverloadedError,
    ApiRateLimitError,
    ApiUsageLimitError,
    BaseInstalledAgent,
    ErrorPattern,
    NetworkConnectionError,
    UnknownApiError,
    with_prompt_template,
)
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

# Harbor loads this file by path, and a path-loaded module does not put its own
# directory on sys.path; the sibling import needs it to be there.
sys.path.insert(0, str(Path(__file__).resolve().parent))

import dim_events  # noqa: E402  (sibling module, stdlib-only)

#: Read-only mount point of the build under test, laid out like a release
#: payload: dimcode finds its default plugins one level above ``bin/``.
MOUNT_PATH = "/opt/dim-agent"
DIM_BIN = f"{MOUNT_PATH}/bin/dimcode"

#: Task images need not carry a CA bundle, and dimcode reads the system's roots
#: for every HTTPS client it builds, the model's included. The image ships the
#: eval machine's bundle; ``SSL_CERT_FILE`` points dimcode at it without
#: touching the task's own filesystem.
CA_BUNDLE = f"{MOUNT_PATH}/ca-certificates.crt"

#: Written inside the environment; synced back to ``logs_dir`` after the trial.
OUTPUT_FILENAME = "dim-output.jsonl"
TIMELINE_FILENAME = "dim-timeline.jsonl"

#: The mount is read-only, so dimcode's state lives on the writable layer.
DIMCODE_HOME = "/tmp/dimcode-home"
MODEL_CONFIG = "/tmp/dimcode-model.json"

#: Seconds added to Harbor's own agent deadline before the in-container guard
#: fires. Harbor must still be the one that declares the timeout — its
#: ``AgentTimeoutError`` is what the gate counts — so the guard is strictly the
#: later of the two and only cleans up what Harbor cannot reach.
TIMEOUT_GUARD_SLACK_SEC = 10

#: How much of the run's output to replay onto stderr for the error classifier.
#: Enough to carry the provider's error codes, short of dumping a whole trace.
FAILURE_REPLAY_CHARS = 2000


class DimAgent(BaseInstalledAgent):
    """Runs ``dimcode exec`` inside a Harbor environment."""

    SUPPORTS_ATIF = False
    SUPPORTS_WINDOWS = False

    # Infra failures must be counted separately from task failures, and rate limits
    # must be retryable rather than scored as a task loss. Harbor's defaults are
    # written against other CLIs' wording, so dim's and DeepSeek's are added here.
    # Order matters: the generic patterns in the base class match last.
    ERROR_PATTERNS: ClassVar[list[ErrorPattern]] = [
        ErrorPattern(r"429 Too Many Requests", ApiRateLimitError),
        ErrorPattern(r"insufficient_quota|Insufficient Balance", ApiUsageLimitError),
        ErrorPattern(r"50[023] (Internal|Bad Gateway|Service Unavailable)", ApiInternalServerError),
        ErrorPattern(r"server_overloaded|model is overloaded", ApiOverloadedError),
        ErrorPattern(r"error sending request|connection (reset|refused|closed)|"
                     r"dns error|operation timed out", NetworkConnectionError),
        ErrorPattern(r"provider .* request failed", UnknownApiError),
        *BaseInstalledAgent.ERROR_PATTERNS,
    ]

    @staticmethod
    @override
    def name() -> str:
        return "dim"

    @override
    def get_version_command(self) -> str | None:
        return f"{DIM_BIN} --version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().splitlines()[-1].strip()

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        """Verify the mounted build and put it on PATH.

        Fails fast when the mount is missing: a silently absent dimcode would
        surface as every task failing, which reads like a catastrophic regression.
        """
        await self.exec_as_agent(
            environment,
            command=(
                "set -eu; "
                f"test -x {DIM_BIN} "
                f'|| {{ echo "dimcode not mounted at {MOUNT_PATH}" >&2; exit 1; }}; '
                f"mkdir -p $HOME/.local/bin && ln -sf {DIM_BIN} $HOME/.local/bin/dimcode; "
                f"{DIM_BIN} --version"
            ),
        )

    @with_prompt_template
    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        env = {
            "DIMCODE_HOME": DIMCODE_HOME,
            "DIMCODE_UPDATE_CHECK": "0",
            "SSL_CERT_FILE": CA_BUNDLE,
            **self.extra_env,
        }
        key_var = env["DIM_EVAL_API_KEY_ENV"]

        # The model config travels as an env value and is written by the shell,
        # so no quoting of its JSON is needed. Missing credentials otherwise
        # surface as every task failing at once, which reads like a catastrophic
        # regression rather than a setup mistake, so check before running.
        await self.exec_as_agent(
            environment,
            command=(
                "set -eu; "
                f'test -n "${{{key_var}:-}}" '
                f'|| {{ echo "{key_var} is empty in the task environment" >&2; exit 1; }}; '
                f"mkdir -p /logs/agent; "
                f'printf %s "$DIM_EVAL_MODEL_CONFIG_JSON" > {MODEL_CONFIG}'
            ),
            env=env,
        )

        # Harbor enforces its agent deadline with asyncio.wait_for, which cancels
        # the awaiting coroutine but never reaches the process `docker exec`
        # started. Without a guard the agent keeps running after the trial is
        # declared timed out, holding a live container and spending tokens.
        phase_timeout = self._phase_timeout_sec()
        guard = (
            f"timeout -s KILL {int(phase_timeout) + TIMEOUT_GUARD_SLACK_SEC} "
            if phase_timeout
            else ""
        )
        home = f"--home {DIMCODE_HOME}"

        # No pipe: Harbor runs commands through `sh -c`, where the exit status of
        # a pipeline is the last command's — a crashed run would report success.
        # Redirecting straight to files keeps `$?` dimcode's own status.
        #
        # Harbor classifies failures by matching patterns against the command's
        # output, so stderr and, on failure, the tail of the JSON stream (where
        # provider errors arrive as notifications) are replayed to stderr.
        #
        # The export and daemon stop run whatever the exit code: a failed run's
        # Timeline is the evidence for why it failed.
        await self.exec_as_agent(
            environment,
            command=(
                f"{guard}{DIM_BIN} exec {home} --model-config {MODEL_CONFIG} "
                f"--permission full-access --json -p {shlex.quote(instruction)} "
                f">/logs/agent/{OUTPUT_FILENAME} 2>/logs/agent/dim-stderr.log; "
                f"rc=$?; cat /logs/agent/dim-stderr.log >&2; "
                f"[ $rc -ne 0 ] && tail -c {FAILURE_REPLAY_CHARS} "
                f"/logs/agent/{OUTPUT_FILENAME} >&2; "
                "sid=$(sed -n 's/^session: //p' /logs/agent/dim-stderr.log | head -n 1); "
                f'[ -n "$sid" ] && {DIM_BIN} export "$sid" {home} '
                f"--output /logs/agent/{TIMELINE_FILENAME} >/dev/null; "
                f"cp {DIMCODE_HOME}/rust/daemon.log /logs/agent/daemon.log 2>/dev/null; "
                f"timeout 60 {DIM_BIN} daemon stop {home} >/dev/null 2>&1; "
                "exit $rc"
            ),
            env=env,
        )

    def _phase_timeout_sec(self) -> float | None:
        """The deadline Harbor will apply to this trial's agent phase.

        Harbor hands ``agent_timeout_sec`` only to the oracle agent, so it is
        derived from the trial lock instead: the lock names the task directory
        and the multiplier in force, and the task's own ``task.toml`` carries the
        base value. Per-task values differ (300s to 5400s across this corpus), so
        a single constant would either fire early or never.

        Returns ``None`` when anything is missing. An unguarded run is the
        behaviour that existed before this guard, so falling back to it cannot
        make a trial worse than it already was.
        """
        try:
            lock = json.loads((self.logs_dir.parent / "lock.json").read_text())
            task_dir = Path(lock["task"]["path"])
            multiplier = float(lock.get("timeout_multiplier") or 1.0)
            with (task_dir / "task.toml").open("rb") as handle:
                base = float(tomllib.load(handle)["agent"]["timeout_sec"])
        except (OSError, KeyError, TypeError, ValueError, tomllib.TOMLDecodeError):
            return None
        return base * multiplier if base > 0 else None

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        """Extract Primary metrics from the trial's Timeline export.

        The parsing itself lives in `dim_events`, which carries the rules for
        when the totals may be compared.
        """
        totals = dim_events.read_totals(self.logs_dir / TIMELINE_FILENAME)
        if totals is None:
            return

        context.n_input_tokens = totals.n_input_tokens
        context.n_output_tokens = totals.n_output_tokens
        context.n_cache_tokens = totals.n_cache_tokens

        # Not provider-priced here: cost is computed against the pinned price table
        # in the baseline so a provider price change cannot silently move the gate.
        context.metadata = {
            **(getattr(context, "metadata", None) or {}),
            "dim_turns": totals.turns,
            "dim_tool_calls": totals.tool_calls,
            "dim_uncached_input_tokens": totals.uncached_input_tokens,
            "dim_end_reason": totals.end_reason,
            "dim_subagent_runs": totals.subagent_runs,
            "dim_usage_complete": totals.usage_complete,
        }

        if not totals.usage_complete:
            self.logger.warning(
                "this trial's Timeline never settled (killed at the agent "
                "deadline?): its usage is unknown, not what the partial Timeline "
                "sums to, and must not be compared against the baseline"
            )
