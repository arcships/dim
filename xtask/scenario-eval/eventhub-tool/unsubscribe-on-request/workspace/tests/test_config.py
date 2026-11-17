import unittest

from config import PORT


class ConfigTest(unittest.TestCase):
    def test_port(self):
        self.assertEqual(PORT, 8080)


if __name__ == "__main__":
    unittest.main()
