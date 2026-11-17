import unittest

from normalize import normalize


class NormalizeTest(unittest.TestCase):
    def test_keeps_plain_words(self):
        self.assertEqual(normalize("hello"), "hello")

    def test_keeps_lowercase_words(self):
        self.assertEqual(normalize("hello world"), "hello world")


if __name__ == "__main__":
    unittest.main()
