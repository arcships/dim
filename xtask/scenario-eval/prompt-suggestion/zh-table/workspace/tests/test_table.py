import unittest

from table import render


class RenderTest(unittest.TestCase):
    def test_header_and_rows(self):
        text = render([["a", "b"], ["1", "2"]])
        self.assertEqual(text, "| a | b |\n| --- | --- |\n| 1 | 2 |\n")

    def test_empty_cell_kept(self):
        text = render([["a", "b"], ["1", ""]])
        self.assertIn("| 1 |  |", text)
