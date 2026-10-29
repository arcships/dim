"""Assemble the 30 runnable L2 tasks from their three upstreams.

frontier-harness-eval ships a *selection*, not runnable tasks: each of its task
directories carries only ``task.toml`` and ``instruction.md``. The verifier, the
environment Dockerfile and the reference solution live in the upstream datasets.

What the selection may override depends on which upstream a task came from, and the
reason is empirical rather than editorial.

For the Terminal-Bench tasks the selection's config cannot be executed. It sets
``allow_internet = false`` on all 21 — uniformly, alongside a uniformly injected
``PATH``, which is the signature of a generated report artifact rather than
per-task curation. The pinned images tell the same story: they are byte-identical
to the ones upstream names, and they contain neither the test toolchain those
verifiers download at scoring time (no curl, no uv) nor the offline assets the
selection's own rewritten instructions reference. A first baseline run under that
config scored zero on all 21 tasks: 60 of 90 trials never reached a verdict because
the verifier could not install itself, seven tasks were unsolvable because their
reference solutions install packages, and every timeout fell on one of those seven.
So Terminal-Bench takes upstream wholesale and the selection contributes only
membership.

For the DeepSWE tasks the selection's only config change is a tighter agent
timeout, 10800s down to 5400s. That is a budget decision, it executes, and the same
run scored those tasks normally — so it stands.

The Terminal-Bench registry resolves ``@latest`` and exposes no immutable ref, so the
download cannot be pinned at fetch time. The pin is the vendored output plus the
digests in MANIFEST.json — which is why the assembled tree is committed rather than
rebuilt per run. See benchmarks/tasks/UPSTREAM.md.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tomllib
from pathlib import Path

#: Files the selection owns, per upstream. An empty tuple means the selection only
#: decides membership and upstream is taken as-is.
OVERLAY_FILES: dict[str, tuple[str, ...]] = {
    "terminal-bench": (),
    "datacurve": ("task.toml", "instruction.md"),
}

#: Without this a task directory is metadata, not something harbor can run.
REQUIRED_AFTER_ASSEMBLY = ("task.toml", "instruction.md", "tests/test.sh")


def task_source(selection_task: Path) -> str:
    """Return 'terminal-bench' or 'datacurve' from the selection's task name."""
    config = tomllib.loads((selection_task / "task.toml").read_text())
    name = config.get("task", {}).get("name", "")
    prefix = name.split("/", 1)[0]
    if prefix not in {"terminal-bench", "datacurve"}:
        raise RuntimeError(
            f"{selection_task.name}: unrecognised task name {name!r}; refusing to "
            "guess which upstream it belongs to"
        )
    return prefix


def digest_dir(path: Path) -> str:
    """Content digest of a task directory: stable across filesystems and reruns."""
    digest = hashlib.sha256()
    for file in sorted(p for p in path.rglob("*") if p.is_file()):
        digest.update(str(file.relative_to(path)).encode())
        digest.update(file.read_bytes())
    return digest.hexdigest()


def assemble(selection: Path, tb: Path, deepswe: Path, out: Path) -> dict:
    tasks = sorted(p for p in selection.iterdir() if (p / "task.toml").exists())
    if not tasks:
        raise RuntimeError(f"no selection tasks under {selection}")

    out.mkdir(parents=True, exist_ok=True)
    manifest: dict[str, dict] = {}

    for task in tasks:
        source = task_source(task)
        upstream = (tb if source == "terminal-bench" else deepswe) / task.name
        if not upstream.is_dir():
            raise RuntimeError(
                f"{task.name}: not found in {source} upstream at {upstream}. The "
                "selection and the upstream have diverged; re-pin both together."
            )

        target = out / task.name
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(upstream, target)

        for name in OVERLAY_FILES[source]:
            source_file = task / name
            if not source_file.exists():
                raise RuntimeError(f"{task.name}: selection is missing {name}")
            shutil.copy2(source_file, target / name)

        for required in REQUIRED_AFTER_ASSEMBLY:
            if not (target / required).exists():
                raise RuntimeError(
                    f"{task.name}: assembled task is missing {required}; harbor will "
                    "reject it at load time"
                )

        manifest[task.name] = {
            "upstream": source,
            "overlaid": list(OVERLAY_FILES[source]),
            "digest": digest_dir(target),
        }

    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--tb", type=Path, required=True)
    parser.add_argument("--deepswe", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    manifest = assemble(args.selection, args.tb, args.deepswe, args.out)
    counts: dict[str, int] = {}
    for entry in manifest.values():
        counts[entry["upstream"]] = counts.get(entry["upstream"], 0) + 1

    tree_digest = hashlib.sha256(
        json.dumps(manifest, sort_keys=True).encode()
    ).hexdigest()
    (args.out / "MANIFEST.json").write_text(
        json.dumps({"tree_digest": tree_digest, "tasks": manifest}, indent=2, sort_keys=True)
    )

    print(f"assembled {len(manifest)} tasks -> {args.out}")
    for source, count in sorted(counts.items()):
        print(f"  {source}: {count}")
    print(f"tree_digest: {tree_digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
