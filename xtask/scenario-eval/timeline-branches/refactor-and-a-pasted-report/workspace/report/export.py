"""Report export."""

import csv


def build_report(rows):
    """Returns the rows and their total."""
    total = 0
    for row in rows:
        total += row["total"]
    return {"rows": rows, "total": total}


def export_csv(path, rows):
    """Writes the rows to path as CSV."""
    with open(path, "w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["name", "total"])
        for row in rows:
            writer.writerow([row["name"], row["total"]])
