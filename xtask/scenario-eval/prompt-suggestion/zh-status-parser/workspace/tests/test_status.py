import unittest

from status import parse


class ParseTest(unittest.TestCase):
    def test_modified(self):
        self.assertEqual(parse(" M README.md"), [{"status": " M", "path": "README.md"}])

    def test_added(self):
        self.assertEqual(parse("A  docs/guide.md"), [{"status": "A ", "path": "docs/guide.md"}])

    def test_untracked_directory(self):
        self.assertEqual(parse("?? notes/"), [{"status": "??", "path": "notes/"}])

    def test_rename(self):
        self.assertEqual(
            parse("R  old.py -> new.py"),
            [{"status": "R ", "path": "old.py", "to": "new.py"}],
        )
