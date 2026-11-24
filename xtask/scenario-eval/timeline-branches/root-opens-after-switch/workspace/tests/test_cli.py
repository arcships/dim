import unittest

from cli import build_parser


class CliTest(unittest.TestCase):
    def test_path_is_required(self):
        with self.assertRaises(SystemExit):
            build_parser().parse_args([])

    def test_help_names_the_path(self):
        self.assertIn("the file to read", build_parser().format_help())


if __name__ == "__main__":
    unittest.main()
