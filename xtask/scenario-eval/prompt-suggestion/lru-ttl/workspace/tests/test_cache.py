import unittest

from cache import LruCache


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


class LruCacheTest(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.cache = LruCache(2, 10, self.clock)

    def test_put_and_get(self):
        self.cache.put("a", 1)
        self.assertEqual(self.cache.get("a"), 1)

    def test_evicts_least_recently_used(self):
        self.cache.put("a", 1)
        self.cache.put("b", 2)
        self.cache.get("a")
        self.cache.put("c", 3)
        self.assertIsNone(self.cache.get("b"))
        self.assertEqual(self.cache.get("a"), 1)

    def test_expires_after_ttl(self):
        self.cache.put("a", 1)
        self.clock.now = 11
        self.assertIsNone(self.cache.get("a"))

    def test_refreshes_recency_on_get(self):
        self.cache.put("a", 1)
        self.cache.put("b", 2)
        self.cache.get("a")
        self.cache.put("c", 3)
        self.assertEqual(len(self.cache), 2)
