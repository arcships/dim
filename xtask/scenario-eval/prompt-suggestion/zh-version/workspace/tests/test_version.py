import unittest

from version import compare, parse


class VersionTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(parse("1.2.3"), (1, 2, 3))

    def test_compare(self):
        self.assertEqual(compare("1.2.3", "1.2.4"), -1)
        self.assertEqual(compare("2.0.0", "1.9.9"), 1)
        self.assertEqual(compare("1.0", "1.0.0"), 0)
