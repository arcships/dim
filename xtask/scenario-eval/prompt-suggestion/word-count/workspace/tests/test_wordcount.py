import unittest

from wordcount import count


class CountTest(unittest.TestCase):
    def test_counts_words(self):
        self.assertEqual(
            count("the cat and the hat"),
            {"the": 2, "cat": 1, "and": 1, "hat": 1},
        )

    def test_ignores_case(self):
        self.assertEqual(count("The the"), {"the": 2})

    def test_strips_punctuation(self):
        self.assertEqual(count("dog, dog."), {"dog": 2})

    def test_empty(self):
        self.assertEqual(count(""), {})
