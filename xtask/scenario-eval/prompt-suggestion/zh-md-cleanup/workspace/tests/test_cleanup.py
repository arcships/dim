import unittest

from cleanup import cleanup


class CleanupTest(unittest.TestCase):
    def test_strips_trailing_spaces(self):
        self.assertEqual(cleanup("第一行  \n第二行"), "第一行\n第二行")

    def test_collapses_blank_lines(self):
        self.assertEqual(cleanup("甲\n\n\n\n乙"), "甲\n\n乙")

    def test_keeps_one_blank_line_before_heading(self):
        self.assertEqual(cleanup("正文\n## 小节"), "正文\n\n## 小节")

    def test_empty(self):
        self.assertEqual(cleanup(""), "")
