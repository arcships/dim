"""In-memory list endpoint."""

ITEMS = list(range(1, 101))


def list_items(limit=None):
    """Returns the items, at most `limit`."""
    if limit is None:
        return list(ITEMS)
    return ITEMS[:limit]
