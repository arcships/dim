import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src"))

from date_util import is_leap


class DateUtilTest(unittest.TestCase):
    def test_ordinary_leap_year(self):
        self.assertTrue(is_leap(2024))

    def test_non_leap_year(self):
        self.assertFalse(is_leap(2023))


if __name__ == "__main__":
    unittest.main()
