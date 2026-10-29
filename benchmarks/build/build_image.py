"""Build the dimcode under test and assemble it into the carrier image.

The eval machine runs a prebuilt image mounted read-only at /opt/dim-agent; this
script is what produces it.

Two things this script exists to get right:

* **Build identity.** The workspace version moves on release cadence and spans
  hundreds of main commits, so it cannot identify a daily build. The commit sha
  is written to ``eval-build.json`` beside it.
* **Layout.** The image mirrors a release payload (``scripts/stage-release.mjs``):
  ``bin/dimcode`` with ``default-plugins.json`` and ``plugins/`` one level up,
  which is where dimcode looks for its default plugins. Flattening the layout
  silently drops them and changes what the agent can do.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCHMARKS = HERE.parent
REPO_ROOT = BENCHMARKS.parent

#: glibc 2.28 as in scripts/build-all.mjs. Linking against the build host's
#: newer glibc would make the binary fail to start in older task images, which
#: reads as every task in those images failing.
RUST_TARGET = "x86_64-unknown-linux-gnu"
GLIBC = "2.28"

IDENTITY_FILENAME = "eval-build.json"

#: Shipped beside dimcode because task images need not carry a CA bundle and
#: dimcode cannot start without system roots (see runner/dim_agent.CA_BUNDLE).
HOST_CA_BUNDLE = Path("/etc/ssl/certs/ca-certificates.crt")

DOCKERFILE = """\
# Carrier image for the dimcode under test. Nothing runs from it: harbor mounts
# its root read-only at /opt/dim-agent, laid out like a release payload.
FROM scratch
COPY bin /bin
COPY plugins /plugins
COPY default-plugins.json /default-plugins.json
COPY eval-build.json /eval-build.json
COPY ca-certificates.crt /ca-certificates.crt
"""


def run(cmd: list[str], cwd: Path | None = None, capture: bool = False,
        env: dict[str, str] | None = None) -> str:
    """Run a command, failing loudly. Silent failures here produce an image that
    looks fine and tests the wrong binary."""
    printable = " ".join(cmd)
    where = f" (in {cwd})" if cwd else ""
    print(f"[build] $ {printable}{where}", flush=True)
    result = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        check=False,
        text=True,
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


def collect_identity(repo: Path) -> dict:
    """Everything needed to tie a run back to the code that produced it.

    ``runner_sha`` is the eval tooling's own revision: the harness is required to
    come from main while the dimcode under test comes from the target commit, and
    a result is only reproducible when both are pinned.
    """
    return {
        "commit": git(repo, "rev-parse", "HEAD"),
        "commit_short": git(repo, "rev-parse", "--short", "HEAD"),
        "branch": git(repo, "rev-parse", "--abbrev-ref", "HEAD"),
        "commit_time": git(repo, "log", "-1", "--format=%cI"),
        "subject": git(repo, "log", "-1", "--format=%s"),
        "dirty": git(repo, "status", "--porcelain") != "",
        "runner_sha": git(REPO_ROOT, "rev-parse", "HEAD"),
        "built_at": datetime.now(timezone.utc).isoformat(),
        "rust_target": f"{RUST_TARGET}.{GLIBC}",
    }


def toolchain_env() -> dict[str, str]:
    """The private toolchain from benchmarks/host/setup-toolchain.sh.

    Put on PATH for the build alone, so the machine's default Rust, Zig and
    Node stay whatever other projects on it expect.
    """
    home = Path(os.environ.get("DIM_EVAL_HOME", Path.home() / "dim-eval"))
    root = Path(os.environ.get("DIM_EVAL_TOOLCHAIN", home / "toolchain"))
    if not (root / "cargo" / "bin" / "cargo-zigbuild").exists():
        raise SystemExit(f"no toolchain at {root}; run benchmarks/host/setup-toolchain.sh")
    env = {key: value for key, value in os.environ.items() if key != "RUSTC_WRAPPER"}
    env.update({
        "RUSTUP_HOME": str(root / "rustup"),
        "CARGO_HOME": str(root / "cargo"),
        "CARGO_INCREMENTAL": "0",
        "PATH": os.pathsep.join([str(root / "node" / "bin"), str(root / "zig" / "bin"),
                                 str(root / "cargo" / "bin"), env.get("PATH", "")]),
    })
    return env


def build_cli(repo: Path, staging: Path) -> tuple[Path, Path]:
    """Produce the linux binary and the default plugins. Returns both paths.

    The plugins are staged by the release's own script, so the tested payload
    carries the same plugin set and dependency lockfiles a user installs.
    """
    env = toolchain_env()
    run(["cargo", "zigbuild", "--release", "--locked",
         "--target", f"{RUST_TARGET}.{GLIBC}", "-p", "dimcode"], cwd=repo, env=env)
    target_dir = Path(env.get("CARGO_TARGET_DIR", repo / "target"))
    plugins = staging / "plugins"
    if plugins.exists():
        shutil.rmtree(plugins)
    staging.mkdir(parents=True, exist_ok=True)
    run(["node", "scripts/stage-plugins.mjs", str(plugins)], cwd=repo, env=env)
    return target_dir / RUST_TARGET / "release" / "dimcode", plugins


def assemble(binary: Path, plugins: Path, out: Path, identity: dict) -> dict:
    """Lay the build out the way the mount expects. Returns the identity written."""
    for required in (binary, plugins / "default-plugins.json", HOST_CA_BUNDLE):
        if not required.exists():
            raise SystemExit(f"build output missing: {required}")

    if out.exists():
        shutil.rmtree(out)
    (out / "bin").mkdir(parents=True)
    shutil.copy2(binary, out / "bin" / "dimcode")
    (out / "bin" / "dimcode").chmod(0o755)
    shutil.copy2(plugins / "default-plugins.json", out / "default-plugins.json")
    shutil.copytree(plugins / "plugins", out / "plugins")
    shutil.copy2(HOST_CA_BUNDLE, out / "ca-certificates.crt")

    # The release version is not this build's identity, but a baseline still has
    # to be able to say which release it measured.
    version = run([str(out / "bin" / "dimcode"), "--version"], capture=True).split()[-1]
    identity = {**identity, "version": version}

    (out / IDENTITY_FILENAME).write_text(json.dumps(identity, indent=2) + "\n")
    (out / "Dockerfile").write_text(DOCKERFILE)

    print(f"[build] assembled {out} (version {version}, commit {identity['commit_short']})")
    return identity


def deliver(out: Path, host: str, remote_dir: str, tag_prefix: str, identity: dict,
            tag_latest: bool) -> str:
    """Ship the assembled directory and build the image under a sha-derived tag.

    The tag is the commit, not the release version: consecutive daily builds
    share a version and would otherwise overwrite each other.

    ``tag_latest`` is off by default because ``:local`` is what a running job
    resolves its mount from. Moving it mid-run swaps the binary under way and
    silently produces a result set built from two different commits. The staging
    directory is likewise sha-scoped so a delivery cannot disturb an in-flight
    job's sources.
    """
    tag = f"{tag_prefix}:{identity['commit_short']}"
    tags = ["-t", tag]
    if tag_latest:
        tags += ["-t", f"{tag_prefix}:local"]
    run(["ssh", host, f"mkdir -p {remote_dir}"])
    run(["rsync", "-az", "--delete", f"{out}/", f"{host}:{remote_dir}/"])
    run(["ssh", host, f"cd {remote_dir} && docker build -q {' '.join(tags)} ."])
    moved = f", also moved {tag_prefix}:local" if tag_latest else ""
    print(f"[build] image ready: {tag}{moved}")
    return tag


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=REPO_ROOT,
                        help="source tree to build (default: this repo)")
    parser.add_argument("--out", type=Path, required=True,
                        help="directory to assemble the image contents into")
    parser.add_argument("--host", help="eval machine to deliver the image to")
    parser.add_argument("--remote-dir",
                        help="staging dir on the host; keep it sha-scoped so a "
                             "delivery cannot disturb an in-flight job")
    parser.add_argument("--tag-prefix", default="dim-agent-under-test")
    parser.add_argument("--tag-latest", action="store_true",
                        help="also move <prefix>:local — refuse this while a job is "
                             "running, it swaps the binary mid-run")
    args = parser.parse_args()
    if args.host and not args.remote_dir:
        parser.error("--host needs --remote-dir")

    repo = args.repo.resolve()
    identity = collect_identity(repo)
    if identity["dirty"]:
        print("[build] WARNING: source tree has uncommitted changes; "
              "the recorded commit does not fully describe this build")

    out = args.out.resolve()
    binary, plugins = build_cli(repo, out.parent / f"{out.name}-staging")
    identity = assemble(binary, plugins, out, identity)

    if args.host:
        tag = deliver(out, args.host, args.remote_dir, args.tag_prefix, identity,
                      args.tag_latest)
        print(f"\nRun the eval against it with:\n  DIM_EVAL_CLI_IMAGE={tag}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
