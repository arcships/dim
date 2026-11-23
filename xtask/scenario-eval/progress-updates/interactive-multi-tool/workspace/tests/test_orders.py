import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from orders.labels import label
from orders.pricing import discount, line_total


class PricingTest(unittest.TestCase):
    def test_line_total_multiplies(self):
        self.assertEqual(line_total(3, 4), 12)

    def test_discount_takes_a_percentage(self):
        self.assertEqual(discount(200, 10), 180)


class LabelsTest(unittest.TestCase):
    def test_label_names_the_customer_first(self):
        self.assertEqual(label("Ada", "A-17"), "Ada (A-17)")


if __name__ == "__main__":
    unittest.main()
