"""The OfficeQA Pro corpus from dataset to reward, with the dataset stubbed.

The real dataset is gated, so the CSV here is synthetic; everything downstream of
it — the generated tasks, their manifest and the verifier they carry — is the real
code path. reward.py is stubbed too: upstream's is vendored into each task
unchanged, and what matters here is that it is called and its result recorded.
"""

from __future__ import annotations

import csv
import importlib.util
import json
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import assemble  # noqa: E402
import daily_run  # noqa: E402

IMAGE = "registry.example/officeqa-corpus@sha256:" + "a" * 64
REVISION = "b" * 40
STUB_REWARD = '''
def score_answer(ground_truth, predicted, tolerance=0.0):
    return 1.0 if ground_truth.strip() == predicted.strip() else 0.0
'''


class OfficeQAFlowTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.dataset = self.root / "dataset"
        self.dataset.mkdir()
        self.repo = self.root / "officeqa"
        self.repo.mkdir()
        (self.repo / "reward.py").write_text(STUB_REWARD)
        self.out = self.root / "tasks"
        self.rows = [
            {"uid": f"UID{i:04d}", "question": f"What was receipt {i}?",
             "answer": str(1000 + i), "difficulty": "hard",
             "source_docs": "", "source_files": f"treasury_bulletin_1950_{i % 12 + 1:02d}.txt"}
            for i in range(1, 134)
        ]
        self.rows[0]["question"] = "In {{year}} and {fiscal} terms,\r\nwhat was it?"
        self._write_csv(self.rows)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _write_csv(self, rows: list[dict[str, str]]) -> None:
        with (self.dataset / assemble.DATASET_FILE).open("w", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)

    def _assemble(self, **overrides) -> dict:
        kwargs = dict(dataset=self.dataset, dataset_revision=REVISION,
                      officeqa_repo=self.repo, reward_commit=REVISION,
                      corpus_image=IMAGE, tolerance=0.0, out=self.out)
        kwargs.update(overrides)
        return assemble.assemble(**kwargs)

    def _verifier(self, uid: str, answer: str | None):
        """Load a generated task's evaluate.py with /app and /logs redirected."""
        tests = self.out / uid / "tests"
        spec = importlib.util.spec_from_file_location(f"evaluate_{uid}", tests / "evaluate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        sys.modules.pop("reward", None)
        module.ANSWER = self.root / "answer.txt"
        module.REWARD = self.root / "logs" / "reward.txt"
        module.ANSWER.unlink(missing_ok=True)
        module.REWARD.unlink(missing_ok=True)
        if answer is not None:
            module.ANSWER.write_text(answer)
        return module

    def test_the_pro_set_becomes_133_pinned_runnable_tasks(self) -> None:
        manifest = self._assemble()

        uids = sorted(p.name for p in self.out.iterdir() if p.is_dir())
        self.assertEqual(uids, [row["uid"] for row in self.rows])
        self.assertEqual(daily_run.corpus_pin(self.out), manifest["tree_digest"])
        self.assertEqual(manifest["source"]["corpus_image"], IMAGE)

        task = self.out / "UID0001"
        config = tomllib.loads((task / "task.toml").read_text())
        self.assertEqual(config["environment"]["docker_image"], IMAGE)
        self.assertFalse(config["environment"]["allow_internet"])
        self.assertEqual(config["metadata"]["source_id"], "UID0001")
        instruction = (task / "instruction.md").read_bytes().decode()
        # Dataset text goes in with braces intact and LF line endings, and the
        # answer does not.
        self.assertTrue(instruction.startswith("In {{year}} and {fiscal} terms,\nwhat was it?"))
        self.assertNotIn("1001", instruction)
        self.assertIn("relative tolerance of 0%", " ".join(instruction.split()))
        self.assertEqual((task / "tests" / "reward.py").read_text(), STUB_REWARD)
        self.assertIn("\n1001\n", (task / "solution" / "solve.sh").read_text())

    def test_the_verifier_writes_what_score_answer_returns(self) -> None:
        self._assemble()
        for answer, reward in (("1002\n", "1"), ("999", "0"), ("", "0"), (None, "0")):
            with self.subTest(answer=answer):
                verifier = self._verifier("UID0002", answer)
                verifier.main()
                self.assertEqual(verifier.REWARD.read_text().strip(), reward)

    def test_a_broken_verifier_leaves_the_trial_unscored(self) -> None:
        """A 0 written for a verifier that never graded would read as the agent
        failing; no reward reads as unscored, which results.py reports."""
        self._assemble()
        (self.out / "UID0003" / "tests" / "config.json").write_text("{")
        verifier = self._verifier("UID0003", "1003")
        with self.assertRaises(json.JSONDecodeError):
            verifier.main()
        self.assertFalse(verifier.REWARD.exists())

    def test_the_pin_moves_with_anything_that_changes_the_score(self) -> None:
        pin = self._assemble()["tree_digest"]
        self.assertEqual(self._assemble()["tree_digest"], pin)
        other_image = IMAGE.replace("a" * 64, "c" * 64)
        self.assertNotEqual(self._assemble(corpus_image=other_image)["tree_digest"], pin)
        self.assertNotEqual(self._assemble(tolerance=0.01)["tree_digest"], pin)
        (self.repo / "reward.py").write_text(STUB_REWARD + "\n# upstream moved\n")
        self.assertNotEqual(self._assemble()["tree_digest"], pin)

    def test_what_is_not_officeqa_pro_is_refused(self) -> None:
        full = self.rows + [dict(self.rows[0], uid=f"EASY{i}", difficulty="easy")
                            for i in range(113)]
        self._write_csv(full)
        with self.assertRaisesRegex(assemble.AssemblyError, "246 rows"):
            self._assemble()

        self._write_csv([dict(self.rows[0], difficulty="easy")] + self.rows[1:])
        with self.assertRaisesRegex(assemble.AssemblyError, "not officeqa_pro.csv"):
            self._assemble()

        self._write_csv(self.rows)
        with self.assertRaisesRegex(assemble.AssemblyError, "not pinned by digest"):
            self._assemble(corpus_image="registry.example/officeqa-corpus:latest")

        self.out.mkdir()
        (self.out / "keep.txt").write_text("not generated\n")
        with self.assertRaisesRegex(assemble.AssemblyError, "not an assembled"):
            self._assemble()
        self.assertTrue((self.out / "keep.txt").exists())


if __name__ == "__main__":
    unittest.main()
