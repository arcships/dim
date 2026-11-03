import unittest

from expr import ExprError, evaluate


class EvaluateTest(unittest.TestCase):
    def test_addition(self):
        self.assertEqual(evaluate("1 + 2"), 3)

    def test_precedence(self):
        self.assertEqual(evaluate("2 + 3 * 4"), 14)
        self.assertEqual(evaluate("(2 + 3) * 4"), 20)

    def test_division_truncates(self):
        self.assertEqual(evaluate("7 / 2"), 3)

    def test_unary_minus(self):
        self.assertEqual(evaluate("-3 + 1"), -2)

    def test_whitespace(self):
        self.assertEqual(evaluate(" 4 * ( 2 + 1 ) "), 12)

    def test_errors(self):
        with self.assertRaises(ExprError):
            evaluate("1 +")
        with self.assertRaises(ExprError):
            evaluate("1 / 0")
