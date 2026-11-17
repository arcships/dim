import unittest

from validate import valid_port


class ValidateTest(unittest.TestCase):
    def test_accepts_a_normal_port(self):
        self.assertTrue(valid_port("8080"))

    def test_rejects_zero(self):
        self.assertFalse(valid_port("0"))

    def test_rejects_above_the_range(self):
        self.assertFalse(valid_port("65536"))


if __name__ == "__main__":
    unittest.main()
