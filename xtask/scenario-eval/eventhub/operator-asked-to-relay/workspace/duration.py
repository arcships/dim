"""Duration parsing."""

UNITS = {"s": 1, "m": 60, "h": 3600}


def parse_duration(text):
    value, _, unit = text.partition(" ")
    return float(value) * UNITS[unit]
