"""Nightly report runbook."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from report.export import build_report, export_csv


def load_rows():
    return [
        {"name": "api", "amount": 12},
        {"name": "web", "amount": 30},
    ]


def main():
    rows = load_rows()
    report = build_report(rows)
    export_csv("daily.csv", report["rows"])


if __name__ == "__main__":
    main()
