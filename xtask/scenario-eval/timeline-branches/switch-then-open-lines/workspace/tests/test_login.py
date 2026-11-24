import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cli.login import build_parser, login


class LoginTest(unittest.TestCase):
    def test_login_issues_a_token(self):
        self.assertEqual(login("ada")["ttl"], 60)

    def test_help_names_the_user(self):
        self.assertIn("the user to log in", build_parser().format_help())


if __name__ == "__main__":
    unittest.main()
