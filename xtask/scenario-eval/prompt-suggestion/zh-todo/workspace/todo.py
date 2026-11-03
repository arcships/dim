"""一个 JSON 文件存储的待办事项工具。"""
import json
import sys


def load(path):
    raise NotImplementedError("load")


def save(path, items):
    raise NotImplementedError("save")


def add(items, text):
    raise NotImplementedError("add")


def done(items, index):
    raise NotImplementedError("done")


def main(argv):
    print("todo: not implemented", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
