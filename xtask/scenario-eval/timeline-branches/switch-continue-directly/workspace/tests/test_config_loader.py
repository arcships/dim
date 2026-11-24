import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.config_loader import load


class ConfigLoaderTest(unittest.TestCase):
    def test_defaults_are_used(self):
        config = load()
        self.assertEqual(config["timeout"], 30)
        self.assertEqual(config["retries"], 3)

    def test_schema_values_win(self):
        config = load()
        self.assertEqual(config["host"], "localhost")
        self.assertEqual(config["port"], 8080)


if __name__ == "__main__":
    unittest.main()
