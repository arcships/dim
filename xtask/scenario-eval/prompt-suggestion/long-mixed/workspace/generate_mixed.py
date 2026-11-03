#!/usr/bin/env python3
"""Writes the long-prefix scenario's workspace: alternating English service
logs and Chinese review records (see README)."""
from pathlib import Path

TOPICS = ("发布记录", "故障复盘", "容量规划", "依赖升级", "监控告警")
STATUSES = ("ok", "retry", "timeout", "rejected", "ok")
DETAILS = (
    "the request reached the regional replica and the handler finished without waiting for the batch writer",
    "the session lookup found the cached entry and the worker continued with the next queued message",
    "the writer flushed its buffer to the store and the reader observed the new sequence without a gap",
    "the client retried once after the connection was reused and the second attempt completed normally",
)


def english(file_index, index):
    status = STATUSES[(file_index + index) % len(STATUSES)]
    latency = (file_index * 41 + index * 11) % 1200
    detail = DETAILS[(file_index + index) % len(DETAILS)]
    return (
        f"2026-11-03T09:{file_index:02d}:{index % 60:02d}.{index * 7 % 1000:03d}Z "
        f"INFO shard-{file_index:02d} record={index + 1:04d} status={status} "
        f"latency_ms={latency} detail={detail}"
    )


def chinese(file_index, index):
    topic = TOPICS[file_index % len(TOPICS)]
    group = (file_index + index) % 7 + 1
    return (
        f"第 {index + 1} 条（{topic}）：{topic}的第 {index + 1} 项记录，涉及已知风险，"
        f"评审人是第 {group} 组，跟踪编号 R-{file_index:02d}-{index + 1:03d}。"
    )


def main():
    root = Path(__file__).resolve().parent
    (root / "logs").mkdir(exist_ok=True)
    (root / "notes").mkdir(exist_ok=True)
    for file_index in range(1, 11):
        if file_index % 2 == 1:
            lines = [english(file_index, index) for index in range(150)]
            (root / "logs" / f"{file_index:02d}.log").write_text("\n".join(lines) + "\n", encoding="utf-8")
        else:
            lines = [f"记录汇编：{TOPICS[file_index % len(TOPICS)]}", ""]
            lines.extend(chinese(file_index, index) for index in range(110))
            (root / "notes" / f"{file_index:02d}.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
