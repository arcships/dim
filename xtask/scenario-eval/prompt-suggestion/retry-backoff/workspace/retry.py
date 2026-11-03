"""Retry a callable with exponential backoff."""
import time


class AttemptsExceeded(RuntimeError):
    pass


def retry(function, max_tries=3, base=0.1, sleep=time.sleep):
    raise NotImplementedError("retry")
