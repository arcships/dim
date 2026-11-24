import csv
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from report.export import build_report, export_csv


class ExportTest(unittest.TestCase):
    def test_build_report(self):
        rows = [{"name": "api", "total": 12}, {"name": "web", "total": 30}]
        self.assertEqual(build_report(rows)["total"], 42)

    def test_export_matches_the_golden_file(self):
        rows = [{"name": "api", "total": 12}, {"name": "web", "total": 30}]
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "daily.csv")
            export_csv(path, rows)
            with open(path, newline="") as written, open(
                os.path.join("tests", "golden", "daily.csv"), newline=""
            ) as golden:
                self.assertEqual(list(csv.reader(written)), list(csv.reader(golden)))


if __name__ == "__main__":
    unittest.main()
