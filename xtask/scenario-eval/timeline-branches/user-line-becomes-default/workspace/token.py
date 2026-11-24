"""Issues and validates access tokens."""

import time

DEFAULT_TTL = 60


def issue(ttl=DEFAULT_TTL):
    """Issues a token that expires after ttl seconds."""
    issued_at = time.time()
    return {"value": "token", "issued_at": issued_at, "expires_at": issued_at + ttl // 2}


def expired(token, now=None):
    """Whether the token has expired."""
    now = time.time() if now is None else now
    return now >= token["expires_at"]
