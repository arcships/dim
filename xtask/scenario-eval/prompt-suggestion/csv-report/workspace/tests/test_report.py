import unittest

from report import summarize


class SummarizeTest(unittest.TestCase):
    def test_totals_by_region(self):
        rows = [
            {"region": "north", "rep": "amy", "amount": "120"},
            {"region": "south", "rep": "bob", "amount": "80"},
            {"region": "north", "rep": "cat", "amount": "45"},
        ]
        self.assertEqual(summarize(rows), {"north": 165, "south": 80})

    def test_empty(self):
        self.assertEqual(summarize([]), {})
