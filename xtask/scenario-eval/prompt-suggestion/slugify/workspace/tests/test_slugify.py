import unittest

from slugify import slugify


class SlugifyTest(unittest.TestCase):
    def test_lowercases_and_joins(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_drops_punctuation(self):
        self.assertEqual(slugify("Rust: a guide!"), "rust-a-guide")

    def test_collapses_dashes(self):
        self.assertEqual(slugify("a -- b"), "a-b")

    def test_accents(self):
        self.assertEqual(slugify("Café au lait"), "cafe-au-lait")
