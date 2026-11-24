"""Retry and small concurrency helpers."""

import time


class Attempts:
    """Counts attempts made by concurrent workers."""

    def __init__(self):
        self.count = 0

    def record(self):
        current = self.count
        time.sleep(0)
        self.count = current + 1
