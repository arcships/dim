import unittest

from retry import attempts


class RetryTest(unittest.TestCase):
    def test_attempts(self):
        self.assertEqual(attempts(), 3)


if __name__ == "__main__":
    unittest.main()
