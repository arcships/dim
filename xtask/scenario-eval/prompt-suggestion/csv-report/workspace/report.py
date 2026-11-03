"""Totals from sales.csv by region."""
import csv
import sys


def summarize(rows):
    """Return {region: total} for the rows of a sales CSV."""
    raise NotImplementedError("summarize")


def main(argv):
    path = argv[1] if len(argv) > 1 else "sales.csv"
    with open(path, newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    for region, total in sorted(summarize(rows).items()):
        print(f"{region}: {total}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
