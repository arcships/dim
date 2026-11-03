import unittest

from rename import plan


class PlanTest(unittest.TestCase):
    def test_replaces_the_extension(self):
        self.assertEqual(
            plan(["a.md", "b.txt"], {".md": ".markdown"}),
            [("a.md", "a.markdown")],
        )

    def test_dry_run_lists_all(self):
        self.assertEqual(len(plan(["a.md", "b.md"], {".md": ".txt"})), 2)

    def test_conflict_raises(self):
        with self.assertRaises(ValueError):
            plan(["a.md", "a.txt"], {".md": ".txt"})
