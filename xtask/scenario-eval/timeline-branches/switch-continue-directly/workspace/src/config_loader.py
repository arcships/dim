"""Configuration loading."""

import os

DEFAULTS = {
    "timeout": 30,
    "retries": 3,
}


def _read_schema(path):
    values = {}
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or line.startswith("["):
                continue
            key, _, value = line.partition("=")
            value = value.strip()
            values[key.strip()] = int(value) if value.isdigit() else value.strip('"')
    return values


def load(path="schema.toml"):
    """Returns the defaults merged with the schema file."""
    merged = dict(DEFAULTS)
    merged.update(_read_schema(path))
    return merged


def from_environment(prefix="APP_"):
    """Returns the environment overrides as a flat mapping."""
    overrides = {}
    for name, value in os.environ.items():
        if name.startswith(prefix):
            overrides[name[len(prefix):].lower()] = value
    return overrides
