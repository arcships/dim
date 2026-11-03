#!/usr/bin/env python3
"""生成 notes/01.txt 到 notes/09.txt：长前缀评测场景的确定性中文记录。"""
from pathlib import Path

TOPICS = ("发布记录", "故障复盘", "容量规划", "依赖升级", "监控告警", "性能评审", "接口变更", "数据迁移", "安全加固")


def record(file_index, index):
    topic = TOPICS[file_index - 1]
    group = (file_index + index) % 9 + 1
    return (
        f"第 {index + 1} 条（{topic}）：{topic}的第 {index + 1} 项记录，涉及已知问题，"
        f"评审人是第 {group} 组，跟踪编号 R-{file_index:02d}-{index + 1:03d}，"
        f"需要在本周内完成复核并登记处理结果。"
    )


def main():
    notes = Path(__file__).resolve().parent / "notes"
    notes.mkdir(exist_ok=True)
    for file_index in range(1, 10):
        topic = TOPICS[file_index - 1]
        lines = [f"记录汇编：{topic}", ""]
        lines.extend(record(file_index, index) for index in range(100))
        (notes / f"{file_index:02d}.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
