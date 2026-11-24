"""Access tokens."""

import time


def issue(ttl=60):
    """Issues a token that is valid for ttl seconds."""
    return {"issued": time.time(), "ttl": ttl}


def is_valid(token, now=None):
    """Returns whether the token is still valid."""
    now = time.time() if now is None else now
    return now < token["issued"] + token["ttl"] - 1
