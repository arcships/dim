import unittest

from capitalize import capitalize_words


class CapitalizeTest(unittest.TestCase):
    def test_capitalizes_each_word(self):
        self.assertEqual(capitalize_words("hello world"), "Hello World")

    def test_leaves_the_rest_unchanged(self):
        self.assertEqual(capitalize_words("hELLO wORLD"), "Hello World")


if __name__ == "__main__":
    unittest.main()
