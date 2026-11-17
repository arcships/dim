import unittest

from utils import normalize


class UtilsTest(unittest.TestCase):
    def test_normalize_trims(self):
        self.assertEqual(normalize(" Hello "), "Hello")


if __name__ == "__main__":
    unittest.main()
