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
from urllib.parse import urlparse, urlsplit

HERE = Path(__file__).resolve().parent
BENCHMARKS = HERE.parent
#: On the eval machine daily_run delivers the corpus beside runner/; this is
#: that copy, not the manifest-only directory in this repository.
TASKS_DIR = BENCHMARKS / "tasks"

#: Shared cache mounted into the task environment as /opt/eval-cache. Verifiers
#: install uv and, through uvx, CPython at scoring time; without a cache every
#: trial repeats those downloads and a flaky route turns a good agent run into a
#: zero. The directory is prepared once on the eval machine (see
#: benchmarks/host/README.md).
EVAL_CACHE = Path(os.environ.get(
    "DIM_EVAL_EVAL_CACHE", str(Path.home() / "dim-eval" / "eval-cache")))

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
    "ApiUsageLimitError",
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


#: Loopback always bypasses the proxy: verifiers connect to servers the agent
#: started in the same container. ``host.docker.internal`` covers a verifier
#: running outside that container. Additional service names belong in
#: ``DIM_EVAL_VERIFIER_NO_PROXY`` and are recorded with the run condition.
DEFAULT_VERIFIER_NO_PROXY = "localhost,127.0.0.1,::1,host.docker.internal"


def verifier_proxy_url() -> str | None:
    """The validated verifier proxy URL, or None when no proxy is configured.

    Credentials are refused rather than written down. The URL goes into
    harbor's job config and may be copied into trial records, so a
    ``user:password@host`` proxy would put a secret in a file this harness does
    not fetch or delete. Authenticate the eval machine to the proxy by network,
    not by embedding a password in a URL. Error messages deliberately do not
    repeat the value: a rejected URL may itself contain the secret.
    """
    raw = os.environ.get("DIM_EVAL_VERIFIER_PROXY")
    if not raw:
        return None
    try:
        parsed = urlsplit(raw)
        scheme = parsed.scheme.lower()
        host = parsed.hostname
        _ = parsed.port  # validate the port, if present
    except ValueError as error:
        raise RuntimeError(
            "DIM_EVAL_VERIFIER_PROXY is not a valid HTTP(S) proxy URL"
        ) from error
    if scheme not in {"http", "https"}:
        raise RuntimeError(
            "DIM_EVAL_VERIFIER_PROXY must use http:// or https://"
        )
    if not host:
        raise RuntimeError(
            "DIM_EVAL_VERIFIER_PROXY must name a proxy host"
        )
    if parsed.username is not None or parsed.password is not None:
        raise RuntimeError(
            "DIM_EVAL_VERIFIER_PROXY must not contain credentials; "
            "authenticate the eval machine to the proxy by network policy"
        )
    if parsed.path not in ("", "/") or parsed.query or parsed.fragment:
        raise RuntimeError(
            "DIM_EVAL_VERIFIER_PROXY must be only scheme, host, and optional port"
        )
    return raw


def verifier_no_proxy() -> str | None:
    """The effective verifier ``NO_PROXY`` list, or None when no proxy is used.

    Loopback (and ``host.docker.internal``) are always present. Docker Compose
    service names are site-specific, so they are added through
    ``DIM_EVAL_VERIFIER_NO_PROXY`` rather than guessed here. The exact string is
    recorded with the run because changing what bypasses the proxy changes the
    experiment just as much as turning the proxy on or off.
    """
    if verifier_proxy_url() is None:
        return None
    extra = os.environ.get("DIM_EVAL_VERIFIER_NO_PROXY", "")
    entries = [entry for entry in re.split(r"[,\s]+", extra) if entry]
    for entry in entries:
        if "://" in entry or "/" in entry or "@" in entry:
            raise RuntimeError(
                "DIM_EVAL_VERIFIER_NO_PROXY entries must be hostnames or "
                "addresses, not URLs or credentials"
            )
    ordered = [DEFAULT_VERIFIER_NO_PROXY, *entries]
    # Deduplicate without changing the documented order.
    return ",".join(dict.fromkeys(
        item for entry in ordered for item in entry.split(",") if item
    ))


def verifier_proxy_enabled() -> bool:
    """Whether the verifier phase will run behind ``DIM_EVAL_VERIFIER_PROXY``.

    This is the run-level fact daily_run records in run.json and in a baseline's
    params. It is kept beside ``verifier_env`` so the flag and the environment
    are derived from the same variable: recording one condition while running
    another is exactly what makes two arms silently incomparable.
    """
    return verifier_proxy_url() is not None


def _cached_python(cache: Path) -> Path | None:
    """The cached CPython directory, or None when the interpreter is missing."""
    python = cache / "python"
    return python if any(python.glob("cpython-3.13.*/bin/python3.13")) else None


def verifier_env() -> dict[str, str]:
    """Environment for the verifier phase.

    Terminal-Bench verifiers install uv from astral.sh and run ``uvx -p 3.13``,
    which downloads CPython. The CPython download is cached under
    ``EVAL_CACHE`` and mounted read-only at ``/opt/eval-cache``; the uv install
    still uses the network, because the verifier scripts run their own
    installers and the cache cannot make those idempotent without changing the
    pinned task bodies. ``CURL_HOME`` forces HTTP/1.1 for curl-based downloads
    when the prep step wrote a ``.curlrc``; uv itself is reqwest and does not
    read it. Both settings are only exported when the cache actually contains
    the artifact, so an unprepared cache falls back to the network instead of
    failing every verifier.

    Proxy settings still come from ``DIM_EVAL_VERIFIER_PROXY``. The agent phase
    is untouched: it reaches only the model host, and dimcode would otherwise
    route the model through the proxy too. Local addresses bypass it, because
    verifiers also connect to servers the agent started inside the container.
    """
    cache = "/opt/eval-cache"
    env = {
        "UV_CACHE_DIR": "/tmp/uv-cache",
        "UV_HTTP_TIMEOUT": "300",
        "UV_CONCURRENT_DOWNLOADS": "1",
    }
    if _cached_python(EVAL_CACHE) is not None:
        env["UV_PYTHON_INSTALL_DIR"] = f"{cache}/python"
    if (EVAL_CACHE / "curl-home" / ".curlrc").is_file():
        env["CURL_HOME"] = f"{cache}/curl-home"
    proxy = verifier_proxy_url()
    if proxy is not None:
        bypass = verifier_no_proxy()
        assert bypass is not None
        env.update({"HTTP_PROXY": proxy, "HTTPS_PROXY": proxy,
                    "http_proxy": proxy, "https_proxy": proxy,
                    "NO_PROXY": bypass, "no_proxy": bypass})
    return env


def build_job_config(settings: ProviderSettings) -> dict:
    cli_image = _env("DIM_EVAL_CLI_IMAGE")
    EVAL_CACHE.mkdir(parents=True, exist_ok=True)
    if _cached_python(EVAL_CACHE) is None:
        print(f"[run_l2] WARNING: no cached CPython 3.13 under {EVAL_CACHE}/python; "
              "uvx will download it at scoring time", flush=True)
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
                },
                {
                    "type": "bind",
                    "source": str(EVAL_CACHE),
                    "target": "/opt/eval-cache",
                    "read_only": True,
                },
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
                    # Harbor's templatize_sensitive_env redacts the value of
                    # any key whose name matches
                    # KEY|SECRET|TOKEN|PASSWORD|CREDENTIAL|AUTH. This value is
                    # itself the name of the real key variable, so the variable
                    # name must not look sensitive or resume loses it.
                    "DIM_EVAL_API_VAR": settings.api_key_env,
                },
            }
        ],
        "verifier": {"env": verifier_env()},
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


def _write_private_json(path: Path, payload: dict) -> None:
    """Write JSON readable only by the eval-machine user.

    The job config contains the model config and the verifier proxy. The proxy
    URL must be credential-free, but it can still name an internal host; 0600 is
    the same boundary the model-identity file gets. ``O_TRUNC`` on an existing
    file does not repair its mode, so fchmod runs before the bytes are written.
    """
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as handle:
            fd = -1
            json.dump(payload, handle, indent=2)
    finally:
        if fd >= 0:
            os.close(fd)


def write_job_config(config: dict) -> Path:
    """Write a job config beside the results, private to the eval-machine user."""
    out_dir = Path(config["jobs_dir"])
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{config['job_name']}-job.json"
    _write_private_json(path, config)
    return path


def main() -> int:
    settings = provider_settings()
    config = build_job_config(settings)
    out_dir = Path(config["jobs_dir"])
    config_path = write_job_config(config)
    print(f"[run_l2] job config -> {config_path}")
    print(f"[run_l2] {len(config['tasks'])} tasks x k={config['n_attempts']}")
    print(f"[run_l2] model {settings.provider}/{settings.model}")

    if os.environ.get("DIM_EVAL_SKIP_MODEL_CHECK") != "1":
        baseline = os.environ.get("DIM_EVAL_BASELINE")
        identity = check_model_identity(settings, Path(baseline) if baseline else None)
        # Persisted because only this process can capture it: the credential and
        # the route to the endpoint live on the eval machine, and a baseline
        # written later, elsewhere, has no way to ask what answered.
        _write_private_json(
            out_dir / f"{config['job_name']}-model.json", identity.to_dict()
        )

    if os.environ.get("DIM_EVAL_DRY_RUN") == "1":
        print("[run_l2] dry run; not invoking harbor")
        return 0

    return subprocess.call(["harbor", "run", "--config", str(config_path)])


if __name__ == "__main__":
    raise SystemExit(main())
