"""Private, loopback-only feedback intake for internal dogfood use."""

import argparse
from contextlib import closing, contextmanager
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import re
import sqlite3
import stat
import sys
import time
import uuid


MAX_BODY = 64 * 1024
RETENTION = 30 * 24 * 60 * 60
PURGE_INTERVAL = 1
FIELDS = {"schemaVersion", "caseId", "allowEval", "note", "sample"}
BEARER = re.compile(rb"[A-Za-z0-9._~+/\-]+=*")


class UnsafeStore(Exception):
    pass


def check_file(directory, name):
    try:
        info = os.stat(name, dir_fd=directory, follow_symlinks=False)
    except FileNotFoundError:
        return
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1):
        raise UnsafeStore


@contextmanager
def private_database_path(path):
    # SQLite opens by pathname. Walk from / without following symlinks and
    # only allow ancestors that other UIDs cannot replace entries under us in.
    if not os.path.isabs(path):
        path = os.path.join(os.getcwd(), path)
    parts = [part for part in path.split("/") if part not in {"", "."}]
    if not parts or ".." in parts:
        raise UnsafeStore
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    uid = os.geteuid()
    directory = os.open("/", flags)
    try:
        for index, part in enumerate(parts):
            leaf = index == len(parts) - 1
            if leaf:
                try:
                    os.mkdir(part, 0o700, dir_fd=directory)
                except FileExistsError:
                    pass
                else:
                    os.fsync(directory)  # Persist the new store's directory entry.
            child = os.open(part, flags, dir_fd=directory)
            os.close(directory)
            directory = child
            info = os.fstat(directory)
            mode = stat.S_IMODE(info.st_mode)
            if leaf:
                if info.st_uid != uid or mode != 0o700:
                    raise UnsafeStore
            elif info.st_uid not in {0, uid} or (mode & 0o022 and not mode & stat.S_ISVTX):
                raise UnsafeStore

        for name in ("feedback.sqlite3", "feedback.sqlite3-journal", "feedback.sqlite3-wal",
                     "feedback.sqlite3-shm"):
            check_file(directory, name)
        try:
            fd = os.open("feedback.sqlite3", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory)
        except FileExistsError:
            check_file(directory, "feedback.sqlite3")
        else:
            os.close(fd)
        yield "/" + "/".join(parts) + "/feedback.sqlite3"
    finally:
        os.close(directory)


@contextmanager
def transaction(db):
    with db:
        db.execute("BEGIN IMMEDIATE")
        yield


class Store:
    def __init__(self, db):
        self.db = db
        if db.execute("PRAGMA journal_mode=DELETE").fetchone()[0] != "delete":
            raise UnsafeStore
        db.execute("PRAGMA synchronous=EXTRA")
        db.execute("PRAGMA fullfsync=ON")  # Stronger sync on macOS; no-op elsewhere.
        db.execute("PRAGMA secure_delete=ON")
        with transaction(db):
            db.execute("""CREATE TABLE IF NOT EXISTS cases (
                id TEXT PRIMARY KEY NOT NULL,
                body BLOB,
                allow_eval INTEGER,
                expires REAL
            ) WITHOUT ROWID""")
            db.execute("CREATE INDEX IF NOT EXISTS expiry ON cases(expires) WHERE expires IS NOT NULL")
        self.purge()

    def purge(self):
        now = time.time()
        if self.db.execute("SELECT 1 FROM cases WHERE expires <= ? LIMIT 1", (now,)).fetchone():
            with transaction(self.db):
                self.db.execute("UPDATE cases SET body=NULL, allow_eval=NULL, expires=NULL WHERE expires <= ?",
                                (now,))

    def put(self, case_id, body, allow_eval):
        with transaction(self.db):
            now = time.time()
            row = self.db.execute("SELECT body, expires FROM cases WHERE id=?", (case_id,)).fetchone()
            if row is not None:
                previous, expires = row
                if expires is None:
                    return 409
                if expires <= now:
                    self.delete_row(case_id)
                    return 409
                return 200 if previous == body else 409
            self.db.execute("INSERT INTO cases VALUES (?, ?, ?, ?)",
                            (case_id, body, allow_eval, now + RETENTION))
            return 201

    def get(self, case_id, for_eval):
        row = self.db.execute("SELECT body, allow_eval, expires FROM cases WHERE id=?",
                              (case_id,)).fetchone()
        if row is None or row[2] is None or row[2] <= time.time():
            return None, "case not found or expired"
        if for_eval and not row[1]:
            return None, "case has not consented to evaluation"
        return row[0], None

    def delete_row(self, case_id):
        self.db.execute("""INSERT INTO cases(id) VALUES (?) ON CONFLICT(id)
            DO UPDATE SET body=NULL, allow_eval=NULL, expires=NULL""", (case_id,))

    def delete(self, case_id):
        with transaction(self.db):
            self.delete_row(case_id)


def valid_case_id(value):
    if not isinstance(value, str) or len(value) != 36:
        return False
    try:
        parsed = uuid.UUID(value)
    except ValueError:
        return False
    return parsed.version == 4 and str(parsed) == value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError
        result[key] = value
    return result


def reject_constant(_):
    raise ValueError


def is_integer_one(text):
    # Only schemaVersion's numeric value is used. Avoid converting arbitrarily
    # long sample integers; the parsed object is never stored or reserialized.
    return 1 if text == "1" else 0


def validate(body):
    obj = json.loads(body.decode("utf-8"), object_pairs_hook=unique_object,
                     parse_constant=reject_constant, parse_int=is_integer_one, parse_float=lambda _: None)
    if (not isinstance(obj, dict) or obj.keys() != FIELDS
            or type(obj["schemaVersion"]) is not int or obj["schemaVersion"] != 1
            or not valid_case_id(obj["caseId"]) or type(obj["allowEval"]) is not bool
            or not isinstance(obj["note"], str) or not isinstance(obj["sample"], dict)
            or not obj["sample"]):
        raise ValueError
    return obj["caseId"], obj["allowEval"]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def reply(self, status, payload=None):
        self.close_connection = True
        data = json.dumps(payload or {"error": status}, separators=(",", ":")).encode("ascii")
        self.send_response_only(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        if getattr(self, "command", None) != "HEAD":
            self.wfile.write(data)

    def send_error(self, code, *_args, **_kwargs):
        # BaseHTTPRequestHandler otherwise reflects malformed request text.
        self.reply(code)

    def handle_expect_100(self):
        self.reply(417)
        return False

    def do_POST(self):
        # The stdlib normalizes multiple leading slashes; require the literal target.
        if self.requestline.split()[1] != "/v1/feedback":
            self.reply(404)
            return
        auth = self.headers.get_all("Authorization", [])
        parts = auth[0].encode("latin-1").split(b" ", 1) if len(auth) == 1 else []
        token = parts[1] if len(parts) == 2 and parts[0].lower() == b"bearer" else b""
        if not hmac.compare_digest(hashlib.sha256(token).digest(), self.server.token_digest):
            self.reply(401)
            return
        if self.headers.get_all("Expect"):
            self.reply(417)
            return
        if self.headers.get_all("Transfer-Encoding") or self.headers.get_all("Content-Encoding"):
            self.reply(400)
            return
        media = self.headers.get_all("Content-Type", [])
        if len(media) != 1 or media[0].split(";", 1)[0].strip().lower() != "application/json":
            self.reply(415)
            return
        lengths = self.headers.get_all("Content-Length", [])
        if not lengths:
            self.reply(411)
            return
        if len(lengths) != 1 or not re.fullmatch(r"[0-9]+", lengths[0]):
            self.reply(400)
            return
        digits = lengths[0].lstrip("0") or "0"
        if len(digits) > len(str(MAX_BODY)) or int(digits) > MAX_BODY:
            self.reply(413)
            return
        size = int(digits)
        body = self.rfile.read(size)
        try:
            if len(body) != size:
                raise ValueError
            case_id, allow_eval = validate(body)
        except (ValueError, RecursionError):
            self.reply(400)
            return
        try:
            status = self.server.store.put(case_id, body, allow_eval)
        except sqlite3.Error:
            print("feedback intake: storage write failed", file=sys.stderr)
            self.reply(503)
            return
        self.reply(status, {"caseId": case_id} if status in {200, 201} else None)


class Server(HTTPServer):  # HTTPServer uses AF_INET; there is no bind-address flag.
    def __init__(self, port, store, token):
        self.store = store
        self.token_digest = hashlib.sha256(token).digest()
        self.next_purge = time.monotonic() + PURGE_INTERVAL
        super().__init__(("127.0.0.1", port), Handler)

    def get_request(self):
        sock, addr = super().get_request()
        sock.settimeout(5)
        return sock, addr

    def handle_error(self, _request, _address):
        # No traceback containing request-controlled text (including malformed headers).
        pass

    def service_actions(self):
        if time.monotonic() >= self.next_purge:
            self.next_purge = time.monotonic() + PURGE_INTERVAL
            try:
                self.store.purge()
            except sqlite3.Error:
                print("feedback intake: retention purge failed", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--store", required=True, metavar="DIR")
    commands = parser.add_subparsers(dest="command", required=True)
    serve = commands.add_parser("serve")
    serve.add_argument("--port", type=int, required=True)
    get = commands.add_parser("get")
    get.add_argument("id")
    get.add_argument("--for-eval", action="store_true")
    commands.add_parser("delete").add_argument("id")
    args = parser.parse_args()
    if args.command == "serve":
        token = os.environb.get(b"DIM_FEEDBACK_TOKEN", b"")
        if len(token) < 32 or not BEARER.fullmatch(token):
            parser.error("DIM_FEEDBACK_TOKEN must be a bearer token of at least 32 bytes")
        if not 0 <= args.port <= 65535:
            parser.error("port must be between 0 and 65535")
    elif not valid_case_id(args.id):
        parser.error("ID must be a canonical UUID v4")

    os.umask(0o077)
    try:
        with private_database_path(args.store) as path, closing(sqlite3.connect(path)) as db:
            store = Store(db)
            if args.command == "serve":
                with Server(args.port, store, token) as server:
                    print(f"listening on 127.0.0.1:{server.server_port}", flush=True)
                    server.serve_forever()
            elif args.command == "delete":
                store.delete(args.id)
            else:
                body, error = store.get(args.id, args.for_eval)
                if error:
                    print(error, file=sys.stderr)
                    return 1
                sys.stdout.buffer.write(body)
    except KeyboardInterrupt:
        return 130
    except (OSError, sqlite3.Error, UnsafeStore):
        print("feedback intake: could not open/use private store or listening socket", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
