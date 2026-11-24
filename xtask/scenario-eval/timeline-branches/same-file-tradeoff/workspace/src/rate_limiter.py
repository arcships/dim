"""Fixed-window rate limiter."""

import time


class RateLimiter:
    """Allows `limit` requests in each wall-clock window."""

    def __init__(self, limit=10, window=1.0):
        self.limit = limit
        self.window = window
        self.start = time.monotonic()
        self.count = 0

    def allow(self):
        now = time.monotonic()
        if now - self.start >= self.window:
            self.start = now
            self.count = 0
        if self.count >= self.limit:
            return False
        self.count += 1
        return True
