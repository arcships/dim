"""比较语义化版本号。"""


def parse(text):
    """把 "1.2.3" 解析成 (1, 2, 3)。"""
    raise NotImplementedError("parse")


def compare(left, right):
    """返回 -1、0 或 1。"""
    raise NotImplementedError("compare")
