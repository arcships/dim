"""Count words in a text file."""
import sys


def count(text):
    """Return {word: count} for `text`."""
    raise NotImplementedError("count")


def main(argv):
    if len(argv) != 2:
        print("usage: wordcount.py FILE", file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        counts = count(handle.read())
    for word in sorted(counts):
        print(f"{word} {counts[word]}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
