import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from audit.exporter import export


class ExporterTest(unittest.TestCase):
    def test_json_export(self):
        events = [{"kind": "login", "user": "ada"}]
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "audit.json")
            export(path, events)
            with open(path) as handle:
                self.assertEqual(json.load(handle), events)


if __name__ == "__main__":
    unittest.main()
