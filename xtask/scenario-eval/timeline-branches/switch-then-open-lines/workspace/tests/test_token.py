import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from auth.token import is_valid, issue


class TokenTest(unittest.TestCase):
    def test_token_is_valid_inside_ttl(self):
        token = issue(ttl=60)
        self.assertTrue(is_valid(token, now=token["issued"] + 59.5))

    def test_token_expires_after_ttl(self):
        token = issue(ttl=60)
        self.assertFalse(is_valid(token, now=token["issued"] + 61))


if __name__ == "__main__":
    unittest.main()
