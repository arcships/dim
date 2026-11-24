import os
import sys
import threading
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.retry import Attempts


class RetryTest(unittest.TestCase):
    def test_concurrent_attempts_are_not_lost(self):
        attempts = Attempts()

        def worker():
            for _ in range(250):
                attempts.record()

        threads = [threading.Thread(target=worker) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(attempts.count, 1000)


if __name__ == "__main__":
    unittest.main()
