"""批量重命名文件。"""
import os


def plan(names, mapping):
    """返回 [(旧名, 新名)]。"""
    raise NotImplementedError("plan")


def apply(directory, pairs):
    """按 pairs 重命名 directory 下的文件。"""
    raise NotImplementedError("apply")
