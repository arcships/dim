import unittest

from duration import parse_duration


class DurationTest(unittest.TestCase):
    def test_minutes(self):
        self.assertEqual(parse_duration("5 m"), 300)

    def test_hours(self):
        self.assertEqual(parse_duration("2 h"), 7200)

    def test_unknown_unit_raises_value_error(self):
        with self.assertRaises(ValueError):
            parse_duration("5 x")


if __name__ == "__main__":
    unittest.main()
