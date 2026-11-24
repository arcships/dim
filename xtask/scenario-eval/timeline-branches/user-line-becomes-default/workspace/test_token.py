import unittest

import token


class TokenTest(unittest.TestCase):
    def test_token_lasts_the_ttl(self):
        issued = token.issue(ttl=60)
        self.assertAlmostEqual(issued["expires_at"] - issued["issued_at"], 60, delta=1)


if __name__ == "__main__":
    unittest.main()
