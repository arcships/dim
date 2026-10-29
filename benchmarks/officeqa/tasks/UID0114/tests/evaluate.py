"""Score /app/answer.txt with OfficeQA's official `score_answer`.

`reward.py` beside this file is upstream's, byte for byte. `score_answer` only
ever returns 0.0 or 1.0, which is the pass/fail reward runner/results.py reads.

A missing or empty answer is the agent's result and scores 0. Anything wrong on
the verifier's side — unreadable config, a score outside {0, 1} — raises and
writes no reward, so the trial reads as unscored rather than as a failure.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from reward import score_answer  # noqa: E402

ANSWER = Path("/app/answer.txt")
CONFIG = Path(__file__).resolve().parent / "config.json"
REWARD = Path("/logs/verifier/reward.txt")


def main() -> None:
    config = json.loads(CONFIG.read_text(encoding="utf-8"))
    expected = config["expected_answer"]
    tolerance = float(config["tolerance"])

    predicted = ANSWER.read_text(encoding="utf-8").strip() if ANSWER.exists() else ""
    score = score_answer(expected, predicted, tolerance) if predicted else 0.0
    if score not in (0.0, 1.0):
        raise RuntimeError(f"score_answer returned {score!r}, not 0 or 1")

    print(f"expected: {expected!r}\npredicted: {predicted!r}\nscore: {score}")
    REWARD.parent.mkdir(parents=True, exist_ok=True)
    REWARD.write_text(f"{score:.0f}\n")


if __name__ == "__main__":
    main()
