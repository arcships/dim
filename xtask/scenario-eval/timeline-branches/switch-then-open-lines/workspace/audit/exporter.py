"""Audit export."""

import json


def export(path, events):
    """Writes the events as JSON."""
    with open(path, "w") as handle:
        json.dump(events, handle)
