"""Report formatting."""

import json


def render(rows):
    """Returns the rows as a JSON string."""
    return json.dumps(rows)


def write(path, rows):
    """Writes the rows to path."""
    with open(path, "w") as handle:
        handle.write(render(rows))
