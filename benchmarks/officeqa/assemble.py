"""Generate the 133 OfficeQA Pro tasks from the gated dataset and pin them.

Unlike benchmarks/tasks, there is no upstream task tree to copy: the dataset is a
CSV of questions and answers, and the corpus is a directory of parsed bulletins.
This script is the adapter. It writes one harbor task per row of
``officeqa_pro.csv`` from ``template/``, vendors upstream's ``reward.py`` into every
verifier unchanged, and writes MANIFEST.json, whose ``tree_digest`` is the corpus
pin baselines are keyed on.

  python3 benchmarks/officeqa/assemble.py \\
      --dataset <hf snapshot of databricks/officeqa> --dataset-revision <hf sha> \\
      --officeqa-repo <git checkout of databricks/officeqa> --reward-commit <sha> \\
      --corpus-image <registry>/officeqa-corpus@sha256:<digest> \\
      --out benchmarks/officeqa/tasks

Everything that changes what a task measures — question, answer, tolerance,
reward.py, corpus image — lands in a task file, so it moves the task's digest and
therefore the pin. The dataset and reward revisions are recorded as provenance;
they are how to regenerate the tree, not part of what it is.

The corpus itself is not here: it is ~460 MB and lives in the image that
corpus-image/build.sh builds. See benchmarks/officeqa/README.md.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "runner"))

from assemble_tasks import REQUIRED_AFTER_ASSEMBLY, digest_dir  # noqa: E402

TEMPLATE = HERE / "template"
DATASET_FILE = "officeqa_pro.csv"
#: The size upstream publishes for OfficeQA Pro. A different count means a
#: different file (OfficeQA Full is 246 rows) or a moved dataset, not a new Pro.
EXPECTED_ROWS = 133
COLUMNS = ("uid", "question", "answer", "difficulty")
#: A tag can be re-pushed under the same name; a digest cannot.
IMAGE_BY_DIGEST = re.compile(r"^[^\s@\"]+@sha256:[0-9a-f]{64}$")
REVISION = re.compile(r"^[0-9a-f]{40}$")
UID = re.compile(r"^[A-Za-z0-9_-]+$")


class AssemblyError(RuntimeError):
    pass


def read_rows(csv_path: Path) -> list[dict[str, str]]:
    with csv_path.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        missing = [c for c in COLUMNS if c not in (reader.fieldnames or [])]
        if missing:
            raise AssemblyError(f"{csv_path}: missing columns {missing}")
        rows = list(reader)

    if len(rows) != EXPECTED_ROWS:
        raise AssemblyError(
            f"{csv_path}: {len(rows)} rows, OfficeQA Pro has {EXPECTED_ROWS}. "
            "Refusing to pin a corpus that is not the published benchmark."
        )
    seen: set[str] = set()
    for row in rows:
        uid = row["uid"].strip()
        if not UID.match(uid):
            raise AssemblyError(f"uid {uid!r} is not usable as a task directory")
        if uid in seen:
            raise AssemblyError(f"uid {uid} appears twice")
        seen.add(uid)
        if row["difficulty"].strip() != "hard":
            raise AssemblyError(
                f"{uid}: difficulty {row['difficulty']!r}; every Pro question is "
                "hard, so this is not officeqa_pro.csv"
            )
        for column in ("question", "answer"):
            if not row[column].strip():
                raise AssemblyError(f"{uid}: empty {column}")
        if "OFFICEQA_ANSWER_END" in row["answer"]:
            raise AssemblyError(f"{uid}: answer collides with solve.sh's delimiter")
    return rows


def render(text: str, values: dict[str, str]) -> str:
    for key, value in values.items():
        text = text.replace("{{" + key + "}}", value)
    if "{{" in text:
        raise AssemblyError(f"unrendered placeholder in template: {text!r}")
    return text


def write_task(row: dict[str, str], target: Path, *, reward: Path,
               corpus_image: str, tolerance: float) -> None:
    uid = row["uid"].strip()
    answer = row["answer"].strip()
    shutil.copytree(TEMPLATE, target)

    # The question goes in last-rendered: it is dataset text and may itself
    # contain braces, so nothing is substituted after it. Its line endings are
    # normalized because some rows use CRLF, and git transports that rewrite CR
    # (format-patch/am, autocrlf) would otherwise move the task digest.
    question = "\n".join(row["question"].strip().splitlines())
    values = {
        "uid": uid,
        "corpus_image": corpus_image,
        "tolerance_percent": f"{tolerance * 100:g}%",
        "answer": answer,
    }
    for name in ("task.toml", "solution/solve.sh"):
        path = target / name
        path.write_text(render(path.read_text(), values))
    instruction = target / "instruction.md"
    head, tail = instruction.read_text().split("{{question}}", 1)
    instruction.write_text(head + question + render(tail, values))

    shutil.copy2(reward, target / "tests" / "reward.py")
    (target / "tests" / "config.json").write_text(json.dumps(
        {"expected_answer": answer, "tolerance": tolerance},
        indent=2, sort_keys=True,
    ) + "\n")

    for required in REQUIRED_AFTER_ASSEMBLY:
        if not (target / required).exists():
            raise AssemblyError(f"{uid}: assembled task is missing {required}")


def assemble(*, dataset: Path, dataset_revision: str, officeqa_repo: Path,
             reward_commit: str, corpus_image: str, tolerance: float,
             out: Path) -> dict:
    if not IMAGE_BY_DIGEST.match(corpus_image):
        raise AssemblyError(
            f"--corpus-image {corpus_image!r} is not pinned by digest "
            "(<name>@sha256:<64 hex>); corpus-image/build.sh prints one"
        )
    for flag, value in (("--dataset-revision", dataset_revision),
                        ("--reward-commit", reward_commit)):
        if not REVISION.match(value):
            raise AssemblyError(f"{flag} {value!r} is not a full 40-hex revision")
    if not 0.0 <= tolerance < 1.0:
        raise AssemblyError(f"--tolerance {tolerance} is not a relative tolerance")
    reward = officeqa_repo / "reward.py"
    if not reward.is_file():
        raise AssemblyError(f"no reward.py in {officeqa_repo}")
    rows = read_rows(dataset / DATASET_FILE)

    # The output is wholly generated. Clearing it is what keeps a uid dropped
    # upstream from surviving as a stale task that the manifest no longer names.
    if out.exists():
        if not (out / "MANIFEST.json").exists():
            raise AssemblyError(f"{out} exists and is not an assembled OfficeQA tree")
        shutil.rmtree(out)
    out.mkdir(parents=True)

    tasks: dict[str, dict] = {}
    for row in rows:
        uid = row["uid"].strip()
        target = out / uid
        write_task(row, target, reward=reward, corpus_image=corpus_image,
                   tolerance=tolerance)
        tasks[uid] = {"upstream": "officeqa-pro", "overlaid": [],
                      "digest": digest_dir(target)}

    tree_digest = hashlib.sha256(json.dumps(tasks, sort_keys=True).encode()).hexdigest()
    manifest = {
        "tree_digest": tree_digest,
        "tasks": tasks,
        "source": {
            "dataset": "databricks/officeqa",
            "dataset_file": DATASET_FILE,
            "dataset_revision": dataset_revision,
            "reward_repo": "https://github.com/databricks/officeqa",
            "reward_commit": reward_commit,
            "corpus_image": corpus_image,
            "tolerance": tolerance,
        },
    }
    (out / "MANIFEST.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dataset", type=Path, required=True,
                        help=f"HF snapshot of databricks/officeqa holding {DATASET_FILE}")
    parser.add_argument("--dataset-revision", required=True,
                        help="the HF commit that snapshot was taken at")
    parser.add_argument("--officeqa-repo", type=Path, required=True,
                        help="git checkout of github.com/databricks/officeqa")
    parser.add_argument("--reward-commit", required=True,
                        help="the commit that checkout is at")
    parser.add_argument("--corpus-image", required=True,
                        help="<name>@sha256:<digest>, as printed by corpus-image/build.sh")
    # Upstream's own default. Harbor's adapter uses 0.01; scores under the two
    # are not comparable, which is why the value lands in every task's config.
    parser.add_argument("--tolerance", type=float, default=0.0)
    parser.add_argument("--out", type=Path, default=HERE / "tasks")
    args = parser.parse_args()

    try:
        manifest = assemble(
            dataset=args.dataset, dataset_revision=args.dataset_revision,
            officeqa_repo=args.officeqa_repo, reward_commit=args.reward_commit,
            corpus_image=args.corpus_image, tolerance=args.tolerance, out=args.out,
        )
    except AssemblyError as error:
        raise SystemExit(f"officeqa: {error}")
    print(f"assembled {len(manifest['tasks'])} OfficeQA Pro tasks -> {args.out}")
    print(f"tree_digest: {manifest['tree_digest']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
