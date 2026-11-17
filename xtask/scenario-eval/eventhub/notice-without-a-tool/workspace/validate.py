"""Port validation."""


def valid_port(text):
    try:
        port = int(text)
    except ValueError:
        return False
    return 0 <= port <= 70000
