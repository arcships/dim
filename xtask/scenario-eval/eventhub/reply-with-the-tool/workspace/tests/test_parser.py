import unittest

from parser import parse


class ParseTest(unittest.TestCase):
    def test_key_and_value(self):
        self.assertEqual(parse("port = 8080"), {"port": "8080"})

    def test_empty_line_is_ignored(self):
        self.assertEqual(parse("   "), {})

    def test_comment_is_ignored(self):
        self.assertEqual(parse("# a note"), {})


if __name__ == "__main__":
    unittest.main()
