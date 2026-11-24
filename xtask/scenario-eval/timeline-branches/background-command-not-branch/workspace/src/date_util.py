"""Calendar helpers."""


def is_leap(year):
    """Returns whether the year has 366 days."""
    return year % 4 == 0
