import os
import tempfile
import unittest

from app import load_config


class AppTest(unittest.TestCase):
    def test_load_config_reads_the_file(self):
        with tempfile.NamedTemporaryFile(
            "w", suffix=".txt", delete=False, encoding="utf-8"
        ) as file:
            file.write("port=8080")
            path = file.name
        try:
            self.assertEqual(load_config(path), "port=8080")
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
