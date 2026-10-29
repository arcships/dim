"""L2 run entry point.

One entry point for both drivers: a developer over SSH and the nightly run. Everything that
varies between them comes from the environment, so a run reproduced by hand is the
same run the schedule performed. See benchmarks/README.md.

The job config emitted here encodes three requirements from
docs/eval/trigger-policy.md that are easy to lose:

* Infra failures are retried and counted apart from task failures. Harbor raises a
  typed exception per provider error class; those types are listed in ``retry`` so a
  rate limit costs a retry instead of scoring as a task loss.
* Task definitions are never edited. Tasks run ``no-network``; the model API is opened
  for the agent phase only, through the trial-level ``extra_allowed_hosts``, leaving
  the verifier phase isolated and the pinned task definition untouched.
* Containers are deleted. Harbor keeps them when told not to, and a run that leaks
  containers eventually wedges the machine it runs on.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse

HERE = Path(__file__).resolve().parent
BENCHMARKS = HERE.parent
TASKS_DIR = BENCHMARKS / "tasks"

@dataclass(frozen=True)
class ProviderSettings:
    """Where the model lives, read from the eval machine's dimcode model config.

    The config file (``DIM_EVAL_MODEL_CONFIG``, the format of
    ``crates/dim-agent-aimux/examples/openai.json``) is the single place the
    endpoint, model, sampling and reasoning settings are pinned. Its text goes
    into the job config verbatim, so a run records the exact settings it used.
    """

    provider: str
    model: str
    base_url: str
    api_key_env: str
    model_config: str

    @property
    def allowed_hosts(self) -> list[str]:
        host = urlparse(self.base_url).hostname
        if not host:
            raise RuntimeError(f"cannot derive a host from base url {self.base_url!r}")
        return [host]

    @property
    def probe_base_url(self) -> str:
        """Where a plain OpenAI-compatible request goes."""
        base = self.base_url.rstrip("/")
        return base if base.endswith("/v1") else f"{base}/v1"


def provider_settings() -> ProviderSettings:
    path = Path(_env("DIM_EVAL_MODEL_CONFIG"))
    text = path.read_text()
    config = json.loads(text)
    default = config["defaultModel"]
    # The config text is written into the job file and every trial record.
    # Credentials must therefore stay environment references.
    if any(p.get("headers") for p in config["providers"]):
        raise RuntimeError(f"{path}: provider headers are recorded with the run; "
                           "pass credentials through apiKeyEnv instead")
    matches = [p for p in config["providers"] if p["id"] == default["providerId"]]
    if not matches:
        raise RuntimeError(f"{path}: no provider {default['providerId']!r}")
    provider = matches[0]
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", provider.get("apiKeyEnv") or ""):
        raise RuntimeError(f"{path}: provider {provider['id']!r} needs an apiKeyEnv "
                           "that names an environment variable")
    return ProviderSettings(
        provider=provider["id"],
        model=default["modelId"],
        base_url=provider["baseUrl"],
        api_key_env=provider["apiKeyEnv"],
        model_config=text,
    )


#: Transport and provider-side failures. Retrying these is correct; retrying a task
#: failure would inflate the pass rate.
INFRA_EXCEPTIONS = [
    "ApiRateLimitError",
    "ApiOverloadedError",
    "ApiInternalServerError",
    "ApiConnectionClosedError",
    "NetworkConnectionError",
]


def _env(name: str, default: str | None = None) -> str:
    value = os.environ.get(name, default)
    if value is None:
        raise RuntimeError(f"{name} is required")
    return value


def build_job_config(settings: ProviderSettings) -> dict:
    cli_image = _env("DIM_EVAL_CLI_IMAGE")
    run_id = os.environ.get(
        "DIM_EVAL_RUN_ID",
        datetime.now(timezone.utc).strftime("%Y-%m-%d__%H-%M-%S"),
    )
    tasks_dir = Path(os.environ.get("DIM_EVAL_TASKS", str(TASKS_DIR)))
    task_paths = sorted(str(path.parent) for path in tasks_dir.glob("*/task.toml"))
    if not task_paths:
        raise RuntimeError(f"no tasks found under {tasks_dir}")

    return {
        "job_name": run_id,
        "jobs_dir": _env("DIM_EVAL_OUT", "results"),
        "n_attempts": int(_env("DIM_EVAL_K", "3")),
        "n_concurrent_trials": int(_env("DIM_EVAL_CONCURRENCY", "5")),
        # Timeouts come from each task.toml. Scaling them would make results
        # incomparable with the baseline, so the multiplier stays at 1.
        "timeout_multiplier": 1.0,
        # Harbor carries a *second* multiplier that applies to the agent phase
        # alone — an observed run had it at 2.0 while this one was 1.0. Left
        # unset it takes harbor's default, which is an experiment variable under
        # nobody's control and recorded nowhere. It is also required to equal
        # `timeout_multiplier`: the in-container guard in dim_agent derives the
        # agent deadline from that one, so a larger value here would let harbor
        # allow time the guard has already killed.
        "agent_timeout_multiplier": 1.0,
        "environment": {
            "type": "docker",
            # Deleting containers is not housekeeping; leaked containers from earlier
            # runs are what exhausts the machine.
            "delete": True,
            "force_build": False,
            "mounts": [
                {
                    "type": "image",
                    "source": cli_image,
                    "target": "/opt/dim-agent",
                    "read_only": True,
                }
            ],
        },
        "agents": [
            {
                "import_path": "dim_agent:DimAgent",
                "model_name": f"{settings.provider}/{settings.model}",
                "extra_allowed_hosts": settings.allowed_hosts,
                "env": {
                    # A reference, not the secret. Harbor resolves `${VAR}` from
                    # its own environment when it constructs the agent
                    # (harbor/agents/factory.py), and it already templatizes
                    # sensitive values in the trial records it writes — so the
                    # literal here was the only copy of the key on disk, sitting
                    # in a world-readable file on a shared machine, once per run,
                    # with nothing ever deleting it.
                    settings.api_key_env: "${%s}" % settings.api_key_env,
                    # Written into the task environment as dimcode's --model-config.
                    "DIM_EVAL_MODEL_CONFIG_JSON": settings.model_config,
                    "DIM_EVAL_API_KEY_ENV": settings.api_key_env,
                },
            }
        ],
        "retry": {
            "max_retries": 3,
            "include_exceptions": INFRA_EXCEPTIONS,
        },
        "tasks": [{"path": path} for path in task_paths],
    }


def check_model_identity(settings: ProviderSettings,
                        baseline_path: Path | None):
    """Refuse to run against a different model than the baseline recorded.

    Catches a changed model id, not a changed model behind an unchanged id — see
    model_identity for why that trade is the right one for a self-hosted endpoint.
    """
    sys.path.insert(0, str(HERE))
    import model_identity

    current = model_identity.capture(
        provider=settings.provider,
        model=settings.model,
        base_url=settings.probe_base_url,
        api_key_env=settings.api_key_env,
    )

    if baseline_path is None or not baseline_path.exists():
        print(f"[model] no baseline to compare; endpoint reports {current.model_reported!r}")
        return current

    baseline = json.loads(baseline_path.read_text())
    verdict, detail = model_identity.compare(
        model_identity.ModelIdentity.from_dict(baseline["model"]), current
    )
    print(f"[model] {verdict}: {detail}")
    if verdict == "changed":
        raise SystemExit(
            "the endpoint is serving a different model than the baseline. Re-take "
            "the baseline in its own MR before running the gate."
        )
    return current


def main() -> int:
    settings = provider_settings()
    config = build_job_config(settings)
    out_dir = Path(config["jobs_dir"])
    out_dir.mkdir(parents=True, exist_ok=True)
    config_path = out_dir / f"{config['job_name']}-job.json"
    config_path.write_text(json.dumps(config, indent=2))
    print(f"[run_l2] job config -> {config_path}")
    print(f"[run_l2] {len(config['tasks'])} tasks x k={config['n_attempts']}")
    print(f"[run_l2] model {settings.provider}/{settings.model}")

    if os.environ.get("DIM_EVAL_SKIP_MODEL_CHECK") != "1":
        baseline = os.environ.get("DIM_EVAL_BASELINE")
        identity = check_model_identity(settings, Path(baseline) if baseline else None)
        # Persisted because only this process can capture it: the credential and
        # the route to the endpoint live on the eval machine, and a baseline
        # written later, elsewhere, has no way to ask what answered.
        (out_dir / f"{config['job_name']}-model.json").write_text(
            json.dumps(identity.to_dict(), indent=2)
        )

    if os.environ.get("DIM_EVAL_DRY_RUN") == "1":
        print("[run_l2] dry run; not invoking harbor")
        return 0

    return subprocess.call(["harbor", "run", "--config", str(config_path)])


if __name__ == "__main__":
    raise SystemExit(main())
