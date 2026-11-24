import json
import os
import tempfile
import unittest

from report import render, write


class ReportTest(unittest.TestCase):
    def test_render(self):
        self.assertEqual(json.loads(render([{"name": "api"}])), [{"name": "api"}])

    def test_write(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "report.json")
            write(path, [{"name": "api"}])
            with open(path) as handle:
                self.assertEqual(json.load(handle), [{"name": "api"}])


if __name__ == "__main__":
    unittest.main()
