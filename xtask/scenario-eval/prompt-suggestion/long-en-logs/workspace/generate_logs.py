#!/usr/bin/env python3
"""Writes logs/01.log through logs/10.log: deterministic service logs for the
long-prefix evaluation scenario (see README)."""
from pathlib import Path

LEVELS = ("INFO", "WARN", "ERROR", "DEBUG")
ACTIONS = (
    "accept-connection",
    "decode-request",
    "lookup-session",
    "append-event",
    "flush-batch",
    "refresh-token",
    "resolve-route",
    "close-connection",
)
STATUSES = ("ok", "ok", "ok", "retry", "timeout", "rejected")
DETAILS = (
    "the request reached the regional replica and the handler finished without waiting for the batch writer",
    "the session lookup found the cached entry and the worker continued with the next queued message",
    "the writer flushed its buffer to the store and the reader observed the new sequence without a gap",
    "the client retried once after the connection was reused and the second attempt completed normally",
    "the route table was refreshed from the control plane and the worker picked up the new endpoint",
    "the request was rejected before it reached the handler because the token had already expired",
    "the worker waited for the batch window to close and then acknowledged every queued record at once",
    "the replica fell behind briefly and the coordinator asked another worker to take the next segment",
)


def record(file_index, index):
    level = LEVELS[(file_index + index) % len(LEVELS)]
    action = ACTIONS[(file_index * 3 + index) % len(ACTIONS)]
    status = STATUSES[(file_index + index * 5) % len(STATUSES)]
    worker = (file_index * 17 + index) % 64
    latency = (file_index * 31 + index * 7) % 900
    detail = DETAILS[(file_index * 5 + index) % len(DETAILS)]
    return (
        f"2026-11-03T12:{file_index:02d}:{index % 60:02d}.{index * 13 % 1000:03d}Z "
        f"{level} worker-{worker:02d} step={index + 1:04d} action={action} "
        f"status={status} latency_ms={latency} detail={detail}"
    )


def main():
    logs = Path(__file__).resolve().parent / "logs"
    logs.mkdir(exist_ok=True)
    for file_index in range(1, 11):
        lines = [record(file_index, index) for index in range(130)]
        (logs / f"{file_index:02d}.log").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
