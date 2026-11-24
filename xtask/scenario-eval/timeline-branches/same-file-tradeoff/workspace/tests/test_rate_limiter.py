import os
import sys
import time
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src"))

from rate_limiter import RateLimiter


class RateLimiterTest(unittest.TestCase):
    def test_sustained_rate(self):
        limiter = RateLimiter(limit=10, window=1.0)
        allowed = sum(limiter.allow() for _ in range(20))
        self.assertEqual(allowed, 10)

    def test_burst_after_idle(self):
        limiter = RateLimiter(limit=20, window=1.0)
        time.sleep(1.05)
        allowed = sum(limiter.allow() for _ in range(20))
        self.assertEqual(allowed, 20)


if __name__ == "__main__":
    unittest.main()
