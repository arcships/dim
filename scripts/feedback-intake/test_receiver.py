"""Integration tests: real loopback HTTP, CLI subprocesses, and private SQLite."""

from contextlib import closing
import http.client
import json
import os
from pathlib import Path
import secrets
import selectors
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


RECEIVER = Path(__file__).with_name("receiver.py")
MISSING = object()


class ReceiverTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.parent = Path(temporary.name).resolve()
        self.root = self.parent / "store"
        self.db_path = self.root / "feedback.sqlite3"
        self.token = secrets.token_urlsafe(32)  # Ephemeral synthetic test credential.
        self.env = dict(os.environ, DIM_FEEDBACK_TOKEN=self.token)
        self.case_id = str(uuid.uuid4())
        self.payload = dict(schemaVersion=1, caseId=self.case_id, allowEval=False,
                            note="synthetic-private-note", sample={"marker": "synthetic-private-sample"})
        self.body = json.dumps(self.payload).encode()

    def launch(self, *args):
        return [sys.executable, str(RECEIVER), "--store", str(self.root), *args]

    def cli(self, *args, env=None):
        return subprocess.run(self.launch(*args), env=self.env if env is None else env,
                              capture_output=True, timeout=10)

    def start(self):
        self.server = subprocess.Popen(self.launch("serve", "--port", "0"), env=self.env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(self.stop, self.server)
        with selectors.DefaultSelector() as sel:
            sel.register(self.server.stdout, selectors.EVENT_READ)
            self.assertTrue(sel.select(5), "server did not report readiness")
        ready = self.server.stdout.readline()
        self.assertRegex(ready, rb"^listening on 127\.0\.0\.1:[0-9]+\n$")
        self.port = int(ready.rsplit(b":", 1)[1])

    def stop(self, server=None):
        server = self.server if server is None else server
        if server.poll() is None:
            server.terminate()
        if not server.stdout.closed:
            self.logs = b"".join(server.communicate(timeout=10))

    def send(self, body=None, *, auth=MISSING, method="POST", path="/v1/feedback", **headers):
        if body is None:
            body = self.body
        if auth is MISSING:
            auth = "Bearer " + self.token
        request_headers = {"Content-Type": "application/json"}
        if auth is not None:
            request_headers["Authorization"] = auth
        request_headers.update(headers)
        with closing(http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)) as client:
            client.request(method, path, body, request_headers)
            response = client.getresponse()
            return response.status, json.loads(response.read())

    def raw_status(self, headers):
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as client:
            client.sendall(b"POST /v1/feedback HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer "
                           + self.token.encode() + b"\r\nContent-Type: application/json\r\n"
                           + headers + b"\r\n")
            with client.makefile("rb") as stream:
                return int(stream.readline().split()[1])

    def row(self, case_id=None):
        with closing(sqlite3.connect(self.db_path)) as db:
            return db.execute("SELECT body, allow_eval, expires FROM cases WHERE id=?",
                              (case_id or self.case_id,)).fetchone()

    def expire(self):
        with closing(sqlite3.connect(self.db_path)) as db, db:
            db.execute("UPDATE cases SET expires=? WHERE id=?", (time.time() - 1, self.case_id))

    def test_startup_rejects_missing_or_unsafe_token(self):
        for token in (None, "x" * 31, "x" * 32 + "\nprivate"):
            with self.subTest(token_present=token is not None):
                env = self.env.copy()
                if token is None:
                    env.pop("DIM_FEEDBACK_TOKEN")
                else:
                    env["DIM_FEEDBACK_TOKEN"] = token
                result = self.cli("serve", "--port", "0", env=env)
                self.assertEqual(result.returncode, 2)
                self.assertIn(b"DIM_FEEDBACK_TOKEN", result.stderr)
                if token:
                    self.assertNotIn(token.encode(), result.stdout + result.stderr)
                self.assertFalse(self.root.exists())

    def test_authorization_and_no_http_read(self):
        self.start()
        for credential in (None, "Basic " + self.token, "Bearer wrong"):
            self.assertEqual(self.send(auth=credential), (401, {"error": 401}))
        self.assertIsNone(self.row())
        self.assertEqual(self.send()[0], 201)
        self.assertEqual(self.send(method="GET", path=f"/v1/feedback/{self.case_id}"),
                         (501, {"error": 501}))
        self.assertEqual(self.send(path="/v1/feedback?x=1")[0], 404)
        self.assertEqual(self.send(path="//v1/feedback")[0], 404)

    def test_identical_replay_collision_and_durability(self):
        self.start()
        before = time.time()
        self.assertEqual(self.send(), (201, {"caseId": self.case_id}))
        expires = self.row()[2]
        self.assertGreaterEqual(expires, before + 30 * 86400)
        self.assertLessEqual(expires, time.time() + 30 * 86400)
        self.assertEqual(self.send(), (200, {"caseId": self.case_id}))
        self.assertEqual(self.row()[2], expires)
        self.assertEqual(self.send(self.body + b" "), (409, {"error": 409}))
        self.stop()
        self.assertEqual(self.cli("get", self.case_id).stdout, self.body)
        self.start()
        self.assertEqual(self.send(), (200, {"caseId": self.case_id}))

    def test_validation_and_length(self):
        self.start()
        for fields in (dict(extra=None), dict(schemaVersion=True), dict(schemaVersion=1.0),
                       dict(caseId=self.case_id.upper()), dict(caseId=str(uuid.uuid1())),
                       dict(allowEval=1), dict(note=None), dict(sample={}), dict(sample=[])):
            with self.subTest(fields=fields):
                self.assertEqual(self.send(json.dumps(self.payload | fields).encode())[0], 400)
        for body in (b"[]", b"{", b"\xff", self.body[:-1] + b',"note":"duplicate"}',
                     self.body.replace(b'"synthetic-private-sample"', b"NaN"),
                     json.dumps({k: v for k, v in self.payload.items() if k != "note"}).encode()):
            self.assertEqual(self.send(body)[0], 400)
        self.assertEqual(self.send(**{"Content-Type": "text/plain"})[0], 415)
        self.assertEqual(self.raw_status(b""), 411)
        self.assertEqual(self.raw_status(b"Content-Length: -1\r\n"), 400)
        self.assertEqual(self.raw_status(b"Content-Length: 1\r\nContent-Length: 1\r\n"), 400)
        self.assertEqual(self.raw_status(b"Transfer-Encoding: chunked\r\nContent-Length: 1\r\n"), 400)
        self.assertEqual(self.raw_status(b"Content-Length: 65537\r\n"), 413)
        self.assertEqual(self.send(self.body.ljust(65536, b" "))[0], 201)

    def test_consent_withdrawal_and_delete_before_delayed_arrival(self):
        self.start()
        self.assertEqual(self.send()[0], 201)
        result = self.cli("get", self.case_id, "--for-eval")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"has not consented", result.stderr)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(self.cli("get", self.case_id).stdout, self.body)
        self.assertEqual(self.cli("delete", self.case_id).returncode, 0)
        self.assertEqual(self.cli("delete", self.case_id).returncode, 0)
        self.assertEqual(self.row(), (None, None, None))
        self.assertNotEqual(self.cli("get", self.case_id).returncode, 0)
        self.assertEqual(self.send()[0], 409)
        unseen = str(uuid.uuid4())
        self.assertEqual(self.cli("delete", unseen).returncode, 0)
        self.assertEqual(self.send(json.dumps(self.payload | {"caseId": unseen}).encode())[0], 409)
        allowed = str(uuid.uuid4())
        body = json.dumps(self.payload | {"caseId": allowed, "allowEval": True}).encode()
        self.assertEqual(self.send(body)[0], 201)
        result = self.cli("get", allowed, "--for-eval")
        self.assertEqual((result.returncode, result.stdout), (0, body))
        self.stop()
        self.start()
        self.assertEqual(self.send()[0], 409)

    def test_idle_purge_and_expired_get(self):
        self.start()
        body = json.dumps(self.payload | {"allowEval": True}).encode()
        self.assertEqual(self.send(body)[0], 201)
        self.expire()
        deadline = time.monotonic() + 5
        while self.row() != (None, None, None):
            if time.monotonic() >= deadline:
                self.fail("idle server did not remove expired raw data")
            time.sleep(0.05)
        self.assertEqual(self.send(body)[0], 409)
        other = str(uuid.uuid4())
        body = json.dumps(self.payload | {"caseId": other, "allowEval": True}).encode()
        self.assertEqual(self.send(body)[0], 201)
        self.stop()
        self.case_id = other
        self.expire()
        result = self.cli("get", other, "--for-eval")
        self.assertEqual((result.returncode, result.stdout), (1, b""))
        self.assertEqual(self.row(), (None, None, None))

    def test_modes_and_no_request_data_in_logs(self):
        self.start()
        self.assertEqual(stat.S_IMODE(self.root.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.db_path.stat().st_mode), 0o600)
        self.assertEqual(self.send()[0], 201)
        self.assertEqual(self.send(self.body + b"!")[0], 400)
        self.assertEqual(self.send(path="/synthetic-private-path", auth="Bearer synthetic-private-auth")[0], 404)
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as client:
            client.sendall(b"synthetic-private-path invalid request line\r\n")
            response = client.recv(4096)
            self.assertNotIn(b"synthetic-private-path", response)
        self.stop()
        for secret in (self.token.encode(), self.case_id.encode(), b"synthetic-private"):
            self.assertNotIn(secret, self.logs)
        self.assertEqual(self.logs, b"")

    def test_unsafe_store_rejected(self):
        target = self.parent / "target"
        target.mkdir(mode=0o700)
        self.root.symlink_to(target, target_is_directory=True)
        self.assertNotEqual(self.cli("delete", self.case_id).returncode, 0)
        self.assertFalse(list(target.iterdir()))
        self.root.unlink()
        self.root.mkdir(mode=0o700)
        self.root.chmod(0o755)
        self.assertNotEqual(self.cli("delete", self.case_id).returncode, 0)
        self.root.chmod(0o700)
        decoy = target / "decoy"
        decoy.touch(mode=0o600)
        for name in (self.db_path.name, self.db_path.name + "-journal", self.db_path.name + "-wal",
                     self.db_path.name + "-shm"):
            path = self.root / name
            path.symlink_to(decoy)
            self.assertNotEqual(self.cli("delete", self.case_id).returncode, 0)
            path.unlink()
        os.link(decoy, self.db_path)
        self.assertNotEqual(self.cli("delete", self.case_id).returncode, 0)
        self.db_path.unlink()
        self.db_path.touch()
        self.db_path.chmod(0o644)
        self.assertNotEqual(self.cli("delete", self.case_id).returncode, 0)
        self.assertEqual(decoy.read_bytes(), b"")


if __name__ == "__main__":
    unittest.main()
