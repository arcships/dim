"""Ini-style line parser."""


def parse(line):
    name, _, value = line.partition("=")
    return {name.strip(): value.strip()}
