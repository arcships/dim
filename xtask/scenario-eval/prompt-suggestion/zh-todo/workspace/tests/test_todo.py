import unittest

from todo import add, done


class TodoTest(unittest.TestCase):
    def test_add_appends(self):
        items = []
        add(items, "写测试")
        self.assertEqual(items, [{"text": "写测试", "done": False}])

    def test_done_marks_by_index(self):
        items = [{"text": "a", "done": False}, {"text": "b", "done": False}]
        done(items, 1)
        self.assertEqual(items[0]["done"], False)
        self.assertEqual(items[1]["done"], True)
