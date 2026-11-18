"""Offline public user flows for executable layout training and acceptance."""

from contextlib import ExitStack
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import socket
import struct
import subprocess
import termios
import threading
import time


def environment(home, probe=None):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("DIM_", "DIMCODE_")) and
           key.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "LD_PRELOAD")}
    env.update(DIMCODE_HOME=str(home), NO_PROXY="127.0.0.1,localhost",
               TERM="xterm-256color", COLORTERM="truecolor", LANG="C.UTF-8")
    if probe:
        env["LD_PRELOAD"] = str(probe)
    return env


def stopped(process):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        pid, status = os.waitpid(process.pid, os.WNOHANG | os.WUNTRACED)
        if pid:
            if os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGSTOP:
                return
            process.returncode = os.waitstatus_to_exitcode(status)
            raise RuntimeError(f"training process exited early: {process.returncode}")
        time.sleep(.01)
    raise TimeoutError("training process did not stop")


def begin(process):
    stopped(process)
    Path(f"/proc/{process.pid}/clear_refs").write_text("1\n")
    process.send_signal(signal.SIGCONT)


def finish(process, profile=None):
    if profile:
        stopped(process)
        profile.write_text(Path(f"/proc/{process.pid}/smaps").read_text())
        process.send_signal(signal.SIGCONT)
    if process.wait(timeout=30) != 0:
        raise RuntimeError(f"training process failed: {process.returncode}")


def terminate(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


class Server:
    def __init__(self, binary, home, stack, probe=None):
        self.home, self.binary = home, binary
        home.mkdir(parents=True, mode=0o700)
        native = home / "rust"
        native.mkdir(mode=0o700)
        self.socket_path = native / "daemon.sock"
        error = stack.enter_context((home / "server.stderr").open("w"))
        self.process = subprocess.Popen(
            [str(binary), "app-server", "--product-home", str(home), "--home", str(native),
             "--unix", str(self.socket_path), "--no-ws"], cwd=home, env=environment(home, probe),
            stdout=subprocess.PIPE, stderr=error, start_new_session=True)
        stack.callback(self.process.stdout.close)
        stack.callback(terminate, self.process)
        if probe:
            begin(self.process)
        pending = bytearray()
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if select.select([self.process.stdout], [], [], .1)[0]:
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError(f"server exited before ready: {(home / 'server.stderr').read_text()[-4000:]}")
                pending.extend(chunk)
                if b'"unixPath"' in pending and b"\n" in pending:
                    break
        else:
            raise TimeoutError(f"server not ready: {(home / 'server.stderr').read_text()[-4000:]}")
        self.stack = stack
        self.wire = None
        self.sequence = 0

    def rpc(self, method, params):
        if self.wire is None:
            connection = self.stack.enter_context(socket.socket(socket.AF_UNIX))
            connection.settimeout(30)
            connection.connect(str(self.socket_path))
            self.wire = self.stack.enter_context(connection.makefile("rwb"))
            self.rpc("hello", {"protocolVersion": 4, "clientKind": "other"})
        self.sequence += 1
        self.wire.write(json.dumps({"jsonrpc": "2.0", "id": self.sequence,
                                    "method": method, "params": params}).encode() + b"\n")
        self.wire.flush()
        while True:
            line = self.wire.readline()
            if not line:
                raise RuntimeError(f"connection closed during {method}")
            reply = json.loads(line)
            if reply.get("id") == self.sequence:
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply["result"]

    def close(self, profile=None):
        self.process.send_signal(signal.SIGTERM)
        finish(self.process, profile)


class LocalModel:
    def __init__(self, stack):
        self.tool_seen = threading.Event()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                self.connection.settimeout(30)
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                has_result = any(m.get("role") == "tool" and "layout-probe-marker" in
                                 json.dumps(m.get("content")) for m in request.get("messages", []))
                if has_result:
                    owner.tool_seen.set()

                def frame(delta, finish_reason):
                    return "data: " + json.dumps({"id": "layout", "choices": [
                        {"index": 0, "delta": delta, "finish_reason": finish_reason}]}) + "\n\n"

                if not has_result and any(t.get("function", {}).get("name") == "read" for t in request.get("tools", [])):
                    payload = frame({"tool_calls": [{"index": 0, "id": "layout-read", "type": "function",
                        "function": {"name": "read", "arguments": '{"path":"marker.txt"}'}}]}, None)
                    payload += frame({}, "tool_calls")
                else:
                    payload = frame({"content": "layout-probe-complete" if has_result else "Offline fixture"}, "stop")
                data = (payload + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)

        self.http = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        thread.start()
        stack.callback(thread.join)
        stack.callback(self.http.server_close)
        stack.callback(self.http.shutdown)


def configure(server, port):
    provider = "018ec73f-0000-7000-8000-000000000007"
    server.rpc("provider.create", {"providerId": provider, "displayName": "Layout fixture",
        "connection": {"driverKind": "openai-compatible", "baseUrl": f"http://127.0.0.1:{port}/v1",
                       "auth": {"type": "anonymous"}}, "enabled": True,
        "models": [{"modelId": "fixture", "displayName": "Fixture", "status": "active",
                    "capabilities": {"modalities": ["text"], "toolCalling": True, "streaming": True,
                        "structuredOutput": False, "vision": False, "audio": False, "reasoning": False,
                        "contextWindow": 128000, "maxOutputTokens": 4096}}]})
    server.rpc("provider.select", {"providerId": provider, "modelId": "fixture"})


def tui(binary, home, stack, probe, profile):
    master, slave = pty.openpty()
    stack.callback(os.close, master)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))

    def terminal():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    try:
        env = environment(home, probe)
        if probe:
            env["DIM_LAYOUT_NO_EXIT_STOP"] = "1"
        client = subprocess.Popen([str(binary), "tui", "--home", str(home), "--cwd", str(home), "--fullscreen"],
            cwd=home, env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=terminal)
    finally:
        os.close(slave)
    stack.callback(terminate, client)
    if probe:
        begin(client)
    capture = bytearray()

    def drain(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([master], [], [], .02)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                capture.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
                if b"\x1b]11;?" in chunk:
                    os.write(master, b"\x1b]11;rgb:0000/0000/0000\x1b\\")

    drain(2)
    for _ in range(3):
        start = len(capture)
        os.write(master, b"/models\r")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            drain(.05)
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(capture[start:]))
            if b"Models" in plain and b"Enter select" in plain:
                break
        else:
            raise TimeoutError(f"TUI model picker did not render: {plain[-2000:]!r}")
        os.write(master, b"\x1b")
        drain(.2)
    if probe:
        client.send_signal(signal.SIGSTOP)
        stopped(client)
        profile.write_text(Path(f"/proc/{client.pid}/smaps").read_text())
        client.send_signal(signal.SIGCONT)
    os.write(master, b"/exit\r")
    drain(.5)
    finish(client)


def train(binary, root, probe=None):
    root.mkdir()
    profiles = [root / f"{name}.smaps" for name in ("startup-shutdown", "tui", "conversation")]
    print(f"Running {'training' if probe else 'acceptance'} flows: {binary}", flush=True)
    with ExitStack() as stack:
        server = Server(binary, root / "startup", stack, probe)
        server.close(profiles[0] if probe else None)
    with ExitStack() as stack:
        model = LocalModel(stack)
        server = Server(binary, root / "interactive", stack)
        configure(server, model.http.server_port)
        tui(binary, server.home, stack, probe, profiles[1])
        server.close()
    with ExitStack() as stack:
        model = LocalModel(stack)
        server = Server(binary, root / "conversation", stack, probe)
        configure(server, model.http.server_port)
        (server.home / "marker.txt").write_text("layout-probe-marker\n")
        result = subprocess.run([str(binary), "--home", str(server.home), "--cwd", str(server.home),
                                 "exec", "--prompt", "Read marker.txt and report completion."],
                                cwd=server.home, env=environment(server.home),
                                capture_output=True, text=True, timeout=60)
        if result.returncode or "layout-probe-complete" not in result.stdout or not model.tool_seen.is_set():
            raise RuntimeError(f"tool conversation failed: {result.stdout}\n{result.stderr}")
        server.close(profiles[2] if probe else None)
    for arguments in (["--version"], ["--help"], ["app-server", "--help"]):
        subprocess.run([str(binary), *arguments], env=environment(root),
                       stdout=subprocess.DEVNULL, check=True, timeout=10)
    return profiles
