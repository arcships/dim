import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from api.list import list_page


class ListPageTest(unittest.TestCase):
    def test_first_page(self):
        page = list_page(cursor=None, limit=10)
        self.assertEqual(page["items"], list(range(1, 11)))
        self.assertEqual(page["nextCursor"], "10")
        self.assertFalse(page["done"])

    def test_last_page(self):
        page = list_page(cursor="95", limit=10)
        self.assertEqual(page["items"], list(range(96, 101)))
        self.assertIsNone(page["nextCursor"])
        self.assertTrue(page["done"])

    def test_limit_is_capped(self):
        page = list_page(cursor=None, limit=500)
        self.assertEqual(len(page["items"]), 100)
        self.assertTrue(page["done"])


if __name__ == "__main__":
    unittest.main()
