"""Record storage."""


def get_data(handle):
    with open(handle, encoding="utf-8") as file:
        return file.read()
