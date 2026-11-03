import unittest

from retry import AttemptsExceeded, retry


class RetryTest(unittest.TestCase):
    def setUp(self):
        self.sleeps = []

    def sleep(self, seconds):
        self.sleeps.append(seconds)

    def test_returns_the_value(self):
        self.assertEqual(retry(lambda: 7, sleep=self.sleep), 7)

    def test_retries_until_success(self):
        calls = []

        def flaky():
            calls.append(1)
            if len(calls) < 3:
                raise OSError("not yet")
            return "ok"

        self.assertEqual(retry(flaky, sleep=self.sleep), "ok")
        self.assertEqual(len(calls), 3)

    def test_gives_up(self):
        with self.assertRaises(AttemptsExceeded):
            retry(lambda: 1 / 0, max_tries=2, sleep=self.sleep)
        self.assertEqual(self.sleeps, [0.1])

    def test_backoff_doubles(self):
        def always():
            raise OSError("no")

        with self.assertRaises(AttemptsExceeded):
            retry(always, max_tries=4, base=0.1, sleep=self.sleep)
        self.assertEqual(self.sleeps, [0.1, 0.2, 0.4])
