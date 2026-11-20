"""A Timeline export leaves the machine only as the pack the submitter read.

The export here is built by the test: no real Session, no model. Its shape is
the one `dimcode export` writes (docs/design/appserver/timeline-export.md §5):
event data are the Rust serializations, and `test_deltas_have_the_fields_rust_writes`
pins the one whose shape the packer depends on.

Run with DIM_EVIDENCE_DIMCODE set to a trusted absolute path to the built CLI:
python3 -m unittest discover -s benchmarks/usercases -p 'pack_flow_test.py'
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import unittest
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from datetime import date, timedelta
from pathlib import Path
from threading import Event
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pack  # noqa: E402

SESSION = "5e0b1c1e-1111-4222-8333-444455556666"
EXEC_CALL = "call_00_abcdefghijklmnop"
LOOKUP_CALL = "call_01_qrstuvwxyzabcdef"
LOOKUP = "mcp_zebra_lookup"
LISTING = "-rw-r--r--  1 alice  staff  12 MARK-RESULT notes.txt"
THINKING = ["I should look at MARK-TH", "INKING in /Users/al", "ice/work first."]
REPLY = ["It is owned by alice (", "/Users/al", "ice/work). Also \\/Users\\/alice and %2FUsers%2Falice. ",
         "-rw-r--r--  1 alice  staff"]
ASK = "List /Users/alice/work and say who owns it. See https://wiki.internal.example.net/owners"
ENVIRONMENT = "<dim-state>\n<cwd>/Users/alice/work</cwd>\n<home>/Users/alice</home>\n</dim-state>"
MARKS = ["MARK-RESULT", "MARK-THINKING", "MARK-OPAQUE", "MARK-UNKNOWN", "MARK-NESTED", "MARK-ARGS", "MARK-KEY",
         "MARK-LOOKUP", "MARK-REQUEST-ONLY", "MARK-TRACE", "MARK-REASON", "MARK-SYSTEM", "MARK-TOOLDEF", "zebra"]


def export_lines(home="/Users/alice", turn="turn-0001", end=True):
    """One short Session: a question, a thought, two tool calls, an answer."""
    user = home.rsplit("/", 1)[1]

    def own(value):
        return (value.replace(urllib.parse.quote("/Users/alice", safe=""), urllib.parse.quote(home, safe=""))
                    .replace("/Users/alice", home)
                    .replace("alice", user))

    tools = [
        {"name": "exec", "source": "builtin", "description": "Run a command. MARK-TOOLDEF",
         "inputSchema": {"type": "object", "properties": {"command": {"type": "string"}}}},
        {"name": "web_fetch", "source": "extension", "description": "Fetch a page.", "inputSchema": {}},
        {"name": LOOKUP, "source": "dynamic", "description": "Look things up.", "inputSchema": {}},
    ]
    model = {"providerId": "test", "modelId": "model-1", "capabilitySnapshot": {"reasoning": {"mode": "effort"}},
             "metadata": {"providerProtocol": "openai-compatible"}, "config": {"maxTokens": 100.0}}
    ids = {"turn_id": turn, "request_id": "request-0001"}
    exec_input = {"command": own("ls -l /Users/alice/work MARK-ARGS")}
    lookup_input = {"MARK-KEY": "MARK-LOOKUP arguments"}
    thinking = {"type": "thinking", "thinking": own("".join(THINKING)), "signature": "MARK-OPAQUE",
                "providerMetadata": {"protocol": "openai-responses", "itemId": "rs_0123456789abcdef"}}
    calls = [{"type": "tool_use", "id": EXEC_CALL, "name": "exec", "input": exec_input},
             {"type": "tool_use", "id": LOOKUP_CALL, "name": LOOKUP, "input": lookup_input}]
    asked = [{"role": "user", "content": [{"type": "text", "text": own(ENVIRONMENT)}]},
             {"role": "user", "content": own(ASK)},
             {"role": "user", "content": "MARK-REQUEST-ONLY reminder"}]
    answered = asked + [
        {"role": "assistant", "content": [thinking] + calls},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": EXEC_CALL, "content": own(LISTING)},
            {"type": "tool_result", "tool_use_id": LOOKUP_CALL, "content": "MARK-LOOKUP result"}]},
    ]
    events = [
        ("branch", "start", {}),
        ("context", "configured", {"timeline": "main", "revision": "rev-1",
                                   "settings": {**model, "tools": tools}, "system": "You are DimAgent. MARK-SYSTEM"}),
        ("input", "message", {"mode": "append", "blocks": [
            {"type": "raw", "key": "state:dim/environment/context", "text": own(ENVIRONMENT)}]}),
        ("input", "message", {"submission_id": "submission-1",
                              "user": {"content": [{"type": "text", "text": own(ASK)}]}}),
        ("turn", "start", {"turn_id": turn}),
        ("model", "call", {**ids, "request": {**model, "messages": asked, "tools": tools,
                                              "system": "You are DimAgent. MARK-SYSTEM"}}),
        ("step", "model-start", ids),
        *[("step", "thinking", {**ids, "delta": own(part)}) for part in THINKING],
        ("step", "tool-use-start", {**ids, "call_id": EXEC_CALL, "name": "exec"}),
        ("step", "tool-use", {**ids, "call": {"id": EXEC_CALL, "name": "exec", "input": exec_input}}),
        ("step", "tool-use", {**ids, "call": {"id": LOOKUP_CALL, "name": LOOKUP, "input": lookup_input}}),
        ("step", "model-content", {**ids, "content": [thinking] + calls}),
        ("step", "model-output-end", {**ids, "status": "completed", "finish_reason": "tool_use"}),
        ("step", "tool-result", {**ids, "call_event_id": "event-call", "results": [
            {"call_id": EXEC_CALL, "result": {
                "content": own(LISTING),
                "structuredContent": {"command": exec_input["command"], "cwd": own("/Users/alice/work"), "exitCode": 0},
                "effects": [{"kind": "exec", "command": exec_input["command"], "exitCode": 0}]}},
            {"call_id": LOOKUP_CALL, "result": {"content": "MARK-LOOKUP result", "isError": False}}]}),
        ("acme", "custom", {"note": "MARK-UNKNOWN", "nested": {"deep": "MARK-NESTED"}, "count": 3}),
        ("model", "call", {**ids, "request_id": "request-0002",
                           "request": {**model, "messages": answered, "tools": tools,
                                       "system": "You are DimAgent. MARK-SYSTEM"}}),
        *[("step", "text", {**ids, "request_id": "request-0002", "delta": own(part)}) for part in REPLY],
        ("step", "model-content", {**ids, "request_id": "request-0002", "content": [
            {"type": "text", "text": own("".join(REPLY)),
             "providerMetadata": {"protocol": "openai-responses", "itemId": "msg_0123456789abcdef"}}]}),
        ("turn", "end", {"turn_id": turn, "status": "completed", "reason": "MARK-REASON end_turn"}),
        ("model", "usage", {**ids, "usage": {"promptTokens": 10.0, "completionTokens": 2.0}}),
        ("exec", "started", {**ids, "call_id": EXEC_CALL, "task_id": "exec-1", "description": "MARK-ARGS in background",
                             "output_path": own("/Users/alice/.dimcode/output.txt")}),
    ]
    lines = [{"kind": "header", "export": 1, "session_id": SESSION, "store_format": 50, "sensitive": True},
             {"kind": "branch", "info": {"id": "main"}, "head": len(events)}]
    for sequence, (kind, name, data) in enumerate(events, 1):
        trace = {"event_id": f"event-{sequence}", "original": {"note": "MARK-TRACE"}, "steps": []}
        lines.append({"kind": "event", "branch_id": "main",
                      "event": {"id": f"event-{sequence}", "sequence": sequence, "create_at": 1000 + sequence,
                                "type": kind, "name": name, "data": data},
                      "trace": trace if name == "tool-result" else None, "batch_end": sequence})
    if end:
        lines.append({"kind": "end", "branches": 1, "events": len(events)})
    return lines


CHILD = "4f1e2d3c-0000-4000-8000-00000000c41d"
CHILD_ASK = "Now do the same for /Users/alice/play."
CHILD_REPLY = "Done: /Users/alice/play is owned by alice too."
DECOY = "fn prompt() { format!(\"<home>/home/fixture</home>\") }"


def branched_export(main_first=False):
    """`export_lines` with a child branch forked from `main` after its first turn.

    `dimcode export` writes branches in directory order, and every branch id
    sorts before "main", so the child comes first. Its first request carries
    the context it inherited from `main`; its own events start after the fork.
    """
    lines = export_lines()
    header, main_branch, main_events, end = lines[0], lines[1], lines[2:-1], lines[-1]
    events = {line["event"]["sequence"]: line["event"]["data"] for line in main_events}
    request = events[18]["request"]
    inherited = [message for message in request["messages"][:2]] + [
        {"role": "assistant", "content": [{"type": "text", "text": events[23]["content"][0]["text"]}]},
        {"role": "user", "content": [{"type": "text", "text": CHILD_ASK}]},
    ]
    ids = {"turn_id": "turn-child", "request_id": "request-child"}
    child = [
        ("branch", "start", {}),
        # A tool result that quotes the home tag, read before any of Dim's own
        # environment blocks: it must not be taken for the home directory.
        ("exec", "ended", {"task_id": "exec-0", "status": "completed", "result": {"content": DECOY}}),
        ("input", "message", {"submission_id": "submission-child",
                              "user": {"content": [{"type": "text", "text": CHILD_ASK}]}}),
        ("turn", "start", {"turn_id": "turn-child"}),
        ("model", "call", {**ids, "request": {**request, "messages": inherited}}),
        ("step", "model-start", ids),
        ("step", "model-content", {**ids, "content": [{"type": "text", "text": CHILD_REPLY}]}),
        ("step", "model-output-end", {**ids, "status": "completed", "finish_reason": "stop"}),
        ("turn", "end", {"turn_id": "turn-child", "status": "completed", "reason": "end_turn"}),
    ]
    child_branch = {"kind": "branch", "info": {"id": CHILD, "parent": "main", "branch_point": 25,
                                               "create_anchor": 25}, "head": len(child)}
    child_events = [{"kind": "event", "branch_id": CHILD,
                     "event": {"id": f"child-{n}", "sequence": n, "create_at": 5000 + n, "type": kind,
                               "name": name, "data": data}, "trace": None, "batch_end": n}
                    for n, (kind, name, data) in enumerate(child, 1)]
    end = {**end, "branches": 2, "events": end["events"] + len(child)}
    if main_first:
        return [header, main_branch, child_branch, *main_events, *child_events, end]
    return [header, child_branch, main_branch, *child_events, *main_events, end]


SPLIT = ["我看完了 /Users/jin", "yaxuan/Desktop 的文件，jin", "yaxuan 说先别动"]


def split_export(interrupt):
    """The second reply streamed as SPLIT, the account name cut across deltas.

    With `interrupt`, another event is committed between each two deltas, as a
    provider may do mid-block; the reader still sees one sentence.
    """
    lines, out, done = export_lines(home="/Users/jinyaxuan"), [], False
    for line in lines:
        event = line.get("event") or {}
        if event.get("name") == "text":
            if not done:
                for n, part in enumerate(SPLIT):
                    if n and interrupt:
                        out.append({**line, "event": {**event, "type": "model", "name": "usage", "data": {
                            "turn_id": "turn-0001", "request_id": "request-0002", "usage": {"totalTokens": 1.0}}}})
                    out.append({**line, "event": {**event, "data": {**event["data"], "delta": part}}})
                done = True
            continue
        if event.get("name") == "model-content" and event["data"]["request_id"] == "request-0002":
            event["data"]["content"][0]["text"] = "".join(SPLIT)
        out.append(line)
    events = [line for line in out if line["kind"] == "event"]
    for sequence, line in enumerate(events, 1):
        line["event"] = {**line["event"], "sequence": sequence}
        line["batch_end"] = sequence
    out[1]["head"] = out[-1]["events"] = len(events)
    return out


def boundary_export():
    """A denial, an interrupt, a compaction and a delegation recorded after the turn."""
    extra = [
        ("approval", "request", {"approval_id": "approval-1",
                                  "source": {"kind": "tool", "turn_id": "turn-0001",
                                             "request_id": "request-0001", "call_id": EXEC_CALL},
                                  "call": {"name": "exec", "input": {"command": "MARK-APPROVAL rm -rf /Users/alice"}},
                                  "reason": "MARK-APPROVAL-REASON to clean the workspace"}),
        ("approval", "respond", {"approval_id": "approval-1", "decision": "deny"}),
        ("approval", "cancelled", {"approval_id": "approval-2", "reason": "MARK-CANCEL superseded"}),
        ("permission", "changed", {"policy": {"version": 1, "rules": "MARK-POLICY ask first"}}),
        ("stop", "interrupt", {"reason": "MARK-STOP the plan changed", "scope": "turn"}),
        ("context", "compaction-start", {"turn_id": "turn-0001", "request_id": "request-0001",
                                          "command": 5, "through": 12, "trigger": "threshold",
                                          "strategy": "latest-messages"}),
        ("context", "compaction", {"turn_id": "turn-0001", "request_id": "request-0001", "command": 5,
                                   "start": 6, "through": 12, "status": "completed", "retain_from": "event-5",
                                   "replacement": [{"role": "user", "content": "MARK-SUMMARY the workspace is clean"}],
                                   "strategy": "latest-messages", "diagnostics": {"tokens": 100}}),
        ("collaboration", "request", {"turn_id": "turn-0001", "request_id": "request-0001", "call_id": EXEC_CALL,
                                       "target": {"kind": "member", "agent_id": "agent-1",
                                                  "session_id": "session-9"},
                                       "prompt": "MARK-COLLAB clean the tests", "title": "MARK-TITLE cleanup"}),
        ("collaboration", "result", {"request_event_id": "event-1", "agent_id": "agent-1",
                                      "session_id": "session-9",
                                      "text": "MARK-COLLAB-RESULT done", "status": "completed"}),
    ]
    lines = export_lines()
    end = lines.pop()
    last = lines[-1]["event"]["sequence"]
    for offset, (kind, name, data) in enumerate(extra, 1):
        sequence = last + offset
        lines.append({"kind": "event", "branch_id": "main",
                      "event": {"id": f"event-x{sequence}", "sequence": sequence, "create_at": 2000 + sequence,
                                "type": kind, "name": name, "data": data},
                      "trace": None, "batch_end": sequence})
    lines[1]["head"] = end["events"] = last + len(extra)
    lines.append(end)
    return lines


class PackFlowTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.export = self.write(export_lines())
        self.out = self.root / "out"

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write(self, lines, name="export.jsonl") -> Path:
        path = self.root / name
        path.write_text("".join(json.dumps(line, ensure_ascii=False) + "\n" for line in lines), encoding="utf-8")
        return path

    def decide(self, **decisions) -> None:
        path = self.out / "decisions.json"
        current = json.loads(path.read_text()) if path.exists() else {"pack_id": "feedfacefeedface"}
        self.out.mkdir(exist_ok=True)
        path.write_text(json.dumps({**current, **decisions}))

    def packed(self) -> str:
        return (self.out / "pack" / "timeline.jsonl").read_text(encoding="utf-8")

    def events(self) -> dict:
        lines = [json.loads(line) for line in self.packed().splitlines()]
        return {line["event"]["sequence"]: line["event"]["data"] for line in lines if line["kind"] == "event"}

    def unit_at(self, placeholder: str) -> str:
        self.assertRegex(placeholder, pack.STUB)
        return placeholder.split()[1]

    def test_redactor_requires_an_explicit_trusted_absolute_path(self) -> None:
        with patch.dict(os.environ, {"PATH": str(self.root)}, clear=True):
            for path in (None, "dimcode", "./dimcode"):
                with self.subTest(path=path), self.assertRaisesRegex(pack.Refused, "absolute dimcode path"):
                    pack.build(self.export, self.out, dimcode=path)
                self.assertFalse((self.out / "pack").exists())
                self.assertFalse((self.out / "pack.tmp").exists())

    def test_helper_failure_removes_stale_pack_and_review(self) -> None:
        helper = self.root / "dimcode"
        helper.write_text("#!/bin/sh\nexit 1\n")
        helper.chmod(0o700)
        pack.build(self.export, self.out)
        with self.assertRaisesRegex(pack.Refused, "evidence-redact"):
            pack.build(self.export, self.out, dimcode=helper)
        for name in ("pack", "pack.tmp", "review.md", ".pack.lock"):
            self.assertFalse((self.out / name).exists())

    def test_unowned_reserved_paths_are_left_untouched(self) -> None:
        for relative in ("pack/notes.txt", "pack.tmp/notes.txt", "review.md", ".pack-receipt"):
            with self.subTest(relative=relative):
                out = self.root / relative.replace("/", "-") / "out"
                existing = out / relative
                existing.parent.mkdir(parents=True)
                existing.write_bytes(b"my notes")
                with self.assertRaisesRegex(pack.Refused, "fresh --out"):
                    pack.build(self.export, out)
                self.assertEqual(existing.read_bytes(), b"my notes")
                self.assertFalse((out / ".pack.lock").exists())

    def test_a_receipt_does_not_authorize_unknown_or_modified_files(self) -> None:
        for action in ("extra", "edit", "link", "missing receipt", "damaged receipt"):
            with self.subTest(action=action):
                out = self.root / action
                pack.build(self.export, out)
                timeline = out / "pack" / "timeline.jsonl"
                original = timeline.read_bytes()
                review = out / "review.md"
                notes = self.root / "notes.txt"
                notes.write_bytes(b"my notes")
                if action == "extra":
                    (timeline.parent / "notes.txt").write_bytes(notes.read_bytes())
                elif action == "edit":
                    review.write_bytes(notes.read_bytes())
                elif action == "link":
                    review.unlink()
                    review.symlink_to(notes)
                elif action == "missing receipt":
                    (out / ".pack-receipt").unlink()
                else:
                    (out / ".pack-receipt").write_bytes(b"unrelated")
                with self.assertRaisesRegex(pack.Refused, "fresh --out"):
                    pack.build(self.export, out)
                self.assertEqual(timeline.read_bytes(), original)
                if action == "extra":
                    self.assertEqual((timeline.parent / "notes.txt").read_bytes(), notes.read_bytes())
                if action in ("edit", "link"):
                    self.assertEqual(review.read_bytes(), b"my notes")
                if action == "link":
                    self.assertTrue(review.is_symlink())

    # ---- level one ------------------------------------------------------------

    def test_level_one_carries_the_dialogue_and_nothing_else(self) -> None:
        pack.build(self.export, self.out)
        packed = self.packed()
        for mark in MARKS:
            self.assertNotIn(mark, packed, f"{mark} left the machine without being kept")
        self.assertNotIn("alice", packed.lower(), "the account name survived in some spelling")
        self.assertNotIn(SESSION, packed)
        self.assertNotIn("abcdefghijklmnop", packed, "a provider identifier survived")
        events = self.events()
        self.assertEqual(events[4]["user"]["content"][0]["text"],
                         "List /Users/useru/work and say who owns it. See https://wiki.internal.example.net/owners")
        reply = events[23]["content"][0]["text"]
        self.assertEqual(reply, "It is owned by useru (/Users/useru/work). Also /Users/useru and /Users/useru. "
                                "-rw-r--r--  1 useru  staff", "an escaped or encoded spelling was missed")
        self.assertRegex(events[16]["results"][0]["result"]["content"], pack.STUB)
        self.assertEqual(events[13]["call"]["name"], "tool-3", "a tool that is not Dim's own kept its name")
        self.assertEqual(events[12]["call"]["name"], "exec")
        self.assertEqual(events[14]["content"][0]["signature"], pack.REMOVED)
        self.assertEqual(events[17], {}, "an unlisted event's body left the machine")
        self.assertRegex(events[24]["reason"], pack.STUB, "free text of an outcome was treated as an enum")
        self.assertEqual(events[24]["status"], "completed")
        self.assertEqual(events[25]["usage"], {"promptTokens": 10.0, "completionTokens": 2.0})
        self.assertEqual(sorted(p.name for p in (self.out / "pack").iterdir()), ["pack.json", "timeline.jsonl"])
        manifest = (self.out / "pack" / "pack.json").read_text()
        self.assertNotIn("alice", manifest)
        self.assertEqual(json.loads(manifest)["level"], 1)

    def test_submit_check_context_requires_an_explicit_keep(self) -> None:
        lines = export_lines()
        operator = [
            '<dim-hooks kind="prompt-submit">Operator pasted this</dim-hooks>',
            "Continue with the second part of my request.",
            "This sentence mentions <dim-hooks>.",
            "<dim-hooks-extra>Not a runtime marker</dim-hooks-extra>",
            " <dim-hooks>Not at the start of the block</dim-hooks>",
            "<dim-hooks\x1cNot Rust whitespace",
            "<dim-hooks/>",
        ]
        hook = '<dim-hooks kind="prompt-submit">PRIVATE-HOOK /Users/alice <home>/home/decoy</home></dim-hooks>'
        source = Path(__file__).resolve().parents[2] / "crates" / "dim-agent-types" / "src" / "markup.rs"
        markers = re.search(r"const MARKERS: .*?= &\[(.*?)\n\];", source.read_text(), re.DOTALL)
        self.assertIsNotNone(markers, "MARKERS moved; point this fixture at its definition")
        runtime = [hook] + [f"<{name}{boundary}" for name in re.findall(r'"(dim-[\w-]+)"', markers[1])
                            for boundary in (">context", "\u00a0source=\"test\">context")]
        blocks = [{"type": "text", "text": body} for body in operator + runtime]
        lines[5]["event"]["data"]["user"]["content"] = blocks
        for index in (7, 19):
            lines[index]["event"]["data"]["request"]["messages"][1]["content"] = blocks
        export = self.write(lines)

        def copies(events):
            return [events[4]["user"]["content"]] + [
                events[seq]["request"]["messages"][1]["content"] for seq in (6, 18)]

        pack.build(export, self.out)
        submitted, *requests = copies(self.events())
        self.assertEqual([b["text"] for b in submitted[:len(operator)]], operator)
        self.assertEqual(len(submitted), len(blocks))
        self.assertEqual(requests, [submitted, submitted])
        for block in submitted[len(operator):]:
            self.assertRegex(block["text"], r"^\[omitted u\d+ injected \d+B\]$")
        self.assertNotIn("PRIVATE-HOOK", self.packed())
        self.assertNotIn("/home/decoy", (self.out / "review.md").read_text())

        def restore(packed):
            events = {line["event"]["sequence"]: line["event"]["data"] for line in packed if line["kind"] == "event"}
            for copy in copies(events):
                copy[len(operator)]["text"] = hook.replace("alice", "useru")

        findings = self.audit_of(restore, export)
        for seq in (4, 6, 18):
            self.assertTrue(any(f.startswith(f"content: main#{seq} ") for f in findings), findings)

        self.decide(keep=[self.unit_at(submitted[len(operator)]["text"])])
        pack.build(export, self.out)
        for copy in copies(self.events()):
            self.assertEqual(copy[len(operator)]["text"], hook.replace("alice", "useru"))
            self.assertEqual(copy[len(operator) + 1:], submitted[len(operator) + 1:])
        manifest = json.loads((self.out / "pack" / "pack.json").read_text())
        self.assertEqual(manifest["level"], 2)

    def test_an_image_counts_as_the_first_operator_block(self) -> None:
        lines = export_lines()
        image = {"type": "image", "source": {"type": "base64", "mediaType": "image/png", "data": "IMAGE-BYTES"}}
        hook = {"type": "text", "text": "<dim-hooks>PRIVATE-HOOK</dim-hooks>"}
        lines[5]["event"]["data"]["user"]["content"] = [image, hook]
        pack.build(self.write(lines), self.out)
        first, second = self.events()[4]["user"]["content"]
        self.assertEqual(first["type"], "image")
        self.assertRegex(first["source"]["data"], r" attachment ")
        self.assertRegex(second["text"], r" injected ")
        self.assertNotIn("PRIVATE-HOOK", self.packed())

    def test_one_original_has_one_stand_in_everywhere(self) -> None:
        pack.build(self.export, self.out)
        events = self.events()
        call = events[12]["call"]["id"]
        self.assertRegex(call, r"^call-\d+$")
        self.assertEqual(events[11]["call_id"], call)
        self.assertEqual(events[14]["content"][1]["id"], call)
        self.assertEqual(events[16]["results"][0]["call_id"], call)
        answered = events[18]["request"]["messages"]
        self.assertEqual(answered[3]["content"][1]["id"], call)
        self.assertEqual(answered[4]["content"][0]["tool_use_id"], call)
        self.assertNotEqual(events[13]["call"]["id"], call)
        self.assertEqual(answered[1]["content"], events[4]["user"]["content"][0]["text"],
                         "the question differs between the event and the request that carried it")
        self.assertEqual(json.loads(self.packed().splitlines()[0])["session_id"], pack.SESSION)

    def test_the_pack_reads_as_the_export_it_came_from(self) -> None:
        pack.build(self.export, self.out)
        before = [json.loads(line) for line in self.export.read_text().splitlines()]
        after = [json.loads(line) for line in self.packed().splitlines()]
        self.assertEqual(len(before), len(after))
        for old, new in zip(before, after):
            self.assertEqual(old["kind"], new["kind"])
            if old["kind"] == "event":
                for field in ("id", "sequence", "create_at", "type", "name"):
                    self.assertEqual(old["event"][field], new["event"][field])
                self.assertEqual(old["batch_end"], new["batch_end"])
        self.assertTrue(after[0]["sensitive"], "a pack is still sensitive material")
        self.assertEqual(after[-1], before[-1])

    def test_deltas_have_the_fields_rust_writes(self) -> None:
        source = Path(__file__).resolve().parents[2] / "crates" / "dim-agent-types" / "src" / "events.rs"
        found = re.search(r"pub struct DeltaData \{(.*?)\n\}", source.read_text(), re.DOTALL)
        self.assertIsNotNone(found, "DeltaData moved; point this test at its definition")
        fields = set(re.findall(r"pub (\w+):", found.group(1)))
        for line in export_lines():
            if line["kind"] == "event" and line["event"]["name"] in ("thinking", "text"):
                self.assertEqual(set(line["event"]["data"]), fields, "the fixture's deltas are not DeltaData")

    def test_an_unlisted_field_of_a_delta_is_removed(self) -> None:
        lines = export_lines()
        at = next(i for i, line in enumerate(lines)
                  if line.get("event", {}).get("name") == "text")
        lines[at]["event"]["data"]["annotation"] = "MARK-EXTRA-FIELD"
        pack.build(self.write(lines, "extra-field.jsonl"), self.out)
        events = self.events()
        self.assertNotIn("annotation", events[19])
        self.assertEqual(events[19]["turn_id"], "turn-0001")
        self.assertEqual(events[19]["request_id"], "request-0002")
        self.assertNotIn("MARK-EXTRA-FIELD", self.packed())
        self.assertEqual("".join(events[i]["delta"] for i in (19, 20, 21, 22)),
                         events[23]["content"][0]["text"], "the stream lost its deltas")

    def test_a_text_field_cannot_become_an_unlisted_object(self) -> None:
        lines = export_lines()
        input_message = lines[5]["event"]["data"]["user"]
        input_message["content"] = {"private": "MARK-OBJECT-CONTENT"}
        export = self.write(lines, "object-in-text.jsonl")
        with self.assertRaisesRegex(pack.Refused, "unexpected JSON type"):
            pack.build(export, self.out)
        self.assertTrue(export.exists(), "a rejected input must remain available")
        self.assertFalse((self.out / "pack").exists())

    def test_unknown_fields_never_enter_the_pack(self) -> None:
        lines = export_lines()
        lines[0]["private-header-key"] = True
        lines[1]["info"]["private-branch-key"] = 197463
        lines[7]["event"]["private-event-key"] = {"number": 17}
        model = lines[7]["event"]["data"]["request"]
        model["capabilitySnapshot"]["reasoning"]["private-model-key"] = [True, 42]
        pack.build(self.write(lines), self.out)
        for key in ("private-header-key", "private-branch-key", "private-event-key", "private-model-key"):
            self.assertNotIn(key, self.packed())
        unknown = next(line for line in lines if line.get("event", {}).get("type") == "acme")
        self.assertEqual(self.events()[unknown["event"]["sequence"]], {})

    def test_a_kept_trace_still_cannot_carry_provider_blobs(self) -> None:
        lines = export_lines()
        trace = lines[17]["trace"]
        trace["original"]["parts"] = [
            {"protocol": protocol, "data": "PRIVATE-OPAQUE-PAYLOAD"}
            for protocol in ("anthropic-redacted", "bedrock-redacted")
        ] + [{"encrypted_content": "PRIVATE-ENCRYPTED-PAYLOAD"}]
        export = self.write(lines)
        pack.build(export, self.out)
        first = [json.loads(line) for line in self.packed().splitlines()]
        self.decide(keep=[self.unit_at(first[17]["trace"]["omitted"])])

        for index, part in enumerate(trace["original"]["parts"]):
            key = "data" if "protocol" in part else "encrypted_content"
            with self.subTest(field=part.get("protocol", key)):
                def restore_blob(packed):
                    packed[17]["trace"]["original"]["parts"][index][key] = part[key]

                found = self.audit_of(restore_blob, export)
                self.assertTrue(any(f.startswith("opaque: main#16") for f in found), found)
                self.assertNotIn(part[key], self.packed())
                self.assertNotIn(part[key], (self.out / "review.md").read_text(encoding="utf-8"))
        self.assertIn("MARK-TRACE", self.packed(), "the trace unit was not actually kept")

    # ---- approvals, stops, compactions, delegations ----------------------------

    def named(self) -> dict:
        kinds = {"approval/request", "approval/respond", "approval/cancelled", "permission/changed",
                 "stop/interrupt", "context/compaction-start", "context/compaction",
                 "collaboration/request", "collaboration/result"}
        found = {}
        for line in self.packed().splitlines():
            record = json.loads(line)
            event = record.get("event") or {}
            key = f"{event.get('type')}/{event.get('name')}"
            if record.get("kind") == "event" and key in kinds:
                found[key] = event["data"]
        return found

    def test_approvals_stops_and_delegations_answer_triage(self) -> None:
        pack.build(self.write(boundary_export(), "boundary.jsonl"), self.out)
        named = self.named()
        self.assertEqual(named["approval/respond"]["decision"], "deny", "the decision was hidden")
        self.assertEqual(named["approval/respond"]["approval_id"], "approval-1")
        self.assertEqual(named["stop/interrupt"]["scope"], "turn", "the interrupt scope was hidden")
        self.assertEqual(named["context/compaction-start"]["trigger"], "threshold")
        self.assertEqual(named["context/compaction"]["status"], "completed",
                         "the compaction outcome was hidden")
        self.assertEqual(named["collaboration/result"]["status"], "completed")
        self.assertEqual(named["collaboration/result"]["agent_id"], "agent-1")
        target = named["collaboration/request"]["target"]
        self.assertEqual((target["kind"], target["agent_id"]), ("member", "agent-1"))
        self.assertEqual(target["session_id"], named["collaboration/result"]["session_id"])
        self.assertNotIn("session-9", self.packed())
        self.assertEqual(named["approval/request"]["call"]["name"], "exec")
        self.assertEqual(named["approval/request"]["source"]["call_id"], self.events()[12]["call"]["id"],
                         "the approval lost its call")
        self.assertRegex(named["approval/request"]["reason"], pack.STUB, "the reason was kept unreviewed")
        self.assertRegex(named["permission/changed"]["policy"]["omitted"], pack.STUB,
                         "the policy was kept unreviewed")
        self.assertRegex(named["context/compaction"]["replacement"][0]["content"], pack.STUB,
                         "the compaction replacement was kept unreviewed")
        self.assertRegex(named["collaboration/request"]["prompt"], pack.STUB,
                         "the delegation prompt was kept unreviewed")
        for mark in ("MARK-APPROVAL", "MARK-APPROVAL-REASON", "MARK-CANCEL", "MARK-POLICY", "MARK-STOP",
                     "MARK-SUMMARY", "MARK-COLLAB", "MARK-COLLAB-RESULT", "MARK-TITLE"):
            self.assertNotIn(mark, self.packed(), f"{mark} left the machine without being kept")

    def test_collaborators_keep_distinct_session_stand_ins_in_fields_and_text(self) -> None:
        lines = boundary_export()
        first = lines[-3]["event"]["data"]["target"]["session_id"]
        second = "sid-b"
        delivery = lines[-2]
        event = delivery["event"]
        lines.insert(-1, {**delivery, "batch_end": delivery["batch_end"] + 1, "event": {
            **event, "id": "member-delivered", "sequence": event["sequence"] + 1, "name": "delivered",
            "data": {"op_event_id": "member-request", "status": "ok", "agent_id": "agent-2", "session_id": second}}})
        lines[1]["head"] += 1
        lines[-1]["events"] += 1
        lines[5]["event"]["data"]["user"]["content"][0]["text"] = f"{first} / {second} / {SESSION}"
        export = self.write(lines)
        pack.build(export, self.out)
        records = list(pack.read(self.out / "pack" / "timeline.jsonl"))
        a = records[-4]["event"]["data"]["target"]["session_id"]
        b = records[-2]["event"]["data"]["session_id"]
        primary = records[0]["session_id"]
        self.assertEqual(len({a, b, primary}), 3)
        self.assertEqual(records[-3]["event"]["data"]["session_id"], a)
        self.assertEqual(self.events()[4]["user"]["content"][0]["text"], f"{a} / {b} / {primary}")
        for raw in (first, second, SESSION):
            self.assertNotIn(raw, self.packed())
            self.assertNotIn(raw, (self.out / "pack" / "pack.json").read_text())
        for wrong in (first, b):
            def misattribute(records):
                records[-3]["event"]["data"]["session_id"] = wrong
            self.assertTrue(any(f.startswith("identifier:") for f in self.audit_of(misattribute, export)))

    def test_the_roster_covers_every_core_event_kind(self) -> None:
        source = Path(__file__).resolve().parents[2] / "crates" / "dim-agent-types" / "src" / "events.rs"
        text = source.read_text()
        block = re.search(r"RESERVED_DOMAINS: \[&str; \d+\] = \[(.*?)\];", text, re.DOTALL).group(1)
        missing = []
        for domain in re.findall(r'"(\w+)"', block):
            variant = re.search(r'"%s" => Self::(\w+)\(' % domain, text).group(1)
            body = re.search(r"enum %sEvent \{(.*?)\n\}" % variant, text, re.DOTALL).group(1)
            for name in re.findall(r'rename = "([^"]+)"', body):
                if f"{domain}/{name}" not in pack.EVENTS:
                    missing.append(f"{domain}/{name}")
        self.assertEqual(missing, [], "event kinds the source defines but the roster does not carry")

    def test_a_streamed_block_is_one_unit(self) -> None:
        pack.build(self.export, self.out)
        events = self.events()
        self.assertEqual((events[8]["turn_id"], events[8]["request_id"]), ("turn-0001", "request-0001"),
                         "a delta's ids were taken for content")
        self.assertRegex(events[8]["delta"], pack.STUB)
        self.assertEqual([events[9]["delta"], events[10]["delta"]], ["", ""])
        self.assertEqual("".join(events[i]["delta"] for i in (19, 20, 21, 22)), events[23]["content"][0]["text"])

    # ---- level two ------------------------------------------------------------

    def test_a_named_unit_is_kept_wherever_it_occurs(self) -> None:
        pack.build(self.export, self.out)
        events = self.events()
        result = self.unit_at(events[16]["results"][0]["result"]["content"])
        thought = self.unit_at(events[8]["delta"])
        self.decide(keep=[result, thought])
        pack.build(self.export, self.out)
        events = self.events()
        listing = "-rw-r--r--  1 useru  staff  12 MARK-RESULT notes.txt"
        self.assertEqual(events[16]["results"][0]["result"]["content"], listing)
        self.assertEqual(events[18]["request"]["messages"][4]["content"][0]["content"], listing)
        thinking = events[14]["content"][0]
        self.assertEqual("".join(events[i]["delta"] for i in (8, 9, 10)), thinking["thinking"])
        self.assertEqual(thinking["thinking"], "I should look at MARK-THINKING in /Users/useru/work first.")
        self.assertEqual(thinking["signature"], pack.REMOVED, "keeping the thought kept the provider's blob")
        self.assertEqual(events[18]["request"]["messages"][3]["content"][0]["thinking"], thinking["thinking"])
        packed = self.packed()
        self.assertNotIn("alice", packed.lower())
        for mark in set(MARKS) - {"MARK-RESULT", "MARK-THINKING"}:
            self.assertNotIn(mark, packed)
        self.assertEqual(json.loads((self.out / "pack" / "pack.json").read_text())["level"], 2)

    def test_a_kept_subtree_has_its_identifiers_replaced(self) -> None:
        pack.build(self.export, self.out)
        arguments = self.unit_at(self.events()[12]["call"]["input"]["omitted"])
        self.decide(keep=[arguments])
        pack.build(self.export, self.out)
        events = self.events()
        self.assertEqual(events[12]["call"]["input"], {"command": "ls -l /Users/useru/work MARK-ARGS"})
        self.assertEqual(events[14]["content"][1]["input"], events[12]["call"]["input"])
        self.assertEqual(set(events[13]["call"]["input"]), {"omitted"})

    def test_a_default_unit_can_be_left_out(self) -> None:
        pack.build(self.export, self.out)
        review = (self.out / "review.md").read_text()
        question = next(line.split(":")[0].removeprefix("### ") for line in review.splitlines()
                        if line.startswith("### ") and "user-input" in line)
        self.decide(omit=[question])
        pack.build(self.export, self.out)
        self.assertRegex(self.events()[4]["user"]["content"][0]["text"], pack.STUB)
        self.assertNotIn("wiki.internal", self.packed())

    def test_the_submitters_own_terms_are_replaced(self) -> None:
        self.decide(terms=["wiki.internal.example.net", "owners"])
        pack.build(self.export, self.out)
        self.assertIn("See https://[term-1]/[term-2]", self.events()[4]["user"]["content"][0]["text"])

    # ---- review and confirmation ----------------------------------------------

    def test_the_review_shows_what_leaves_and_what_might_identify(self) -> None:
        self.assertIsNone(pack.build(self.export, self.out))
        review = (self.out / "review.md").read_text()
        self.assertIn("List /Users/useru/work and say who owns it.", review)
        self.assertIn("| `/Users/alice` | `/Users/useru` |", review)
        self.assertIn("- `wiki.internal.example.net`", review, "an unlisted host is a candidate, not a replacement")
        self.assertIn("- `web_fetch`", review)
        self.assertIn("pack/timeline.jsonl", review)
        self.assertIn('"timeline_sha256"', review)
        self.assertNotIn("MARK-RESULT", review, "the review printed content that was not kept")
        self.assertIn("- `/Users/alice/.dimcode/output.txt`", review, "a file outside the export went unmentioned")
        self.assertEqual(json.loads((self.out / "pack" / "pack.json").read_text())["files_not_packed"], 1)

    def test_a_folder_under_a_generic_home_is_suggested_not_automatically_replaced(self) -> None:
        lines = export_lines(home="/home/ubuntu")
        lines[5]["event"]["data"]["user"]["content"][0]["text"] = "Read /home/ubuntu/acme-customer/src"
        export = self.write(lines)
        pack.build(export, self.out)
        self.assertIn("- `acme-customer`", (self.out / "review.md").read_text().splitlines())
        self.assertIn("/home/ubuntu/acme-customer/src", self.packed())
        self.decide(terms=["acme-customer"])
        pack.build(export, self.out)
        self.assertNotIn("acme-customer", self.packed())
        self.assertIn("/home/ubuntu/[term-1]/src", self.packed())

    def test_confirming_binds_the_digest_that_was_read(self) -> None:
        consent = {"diagnosis": True, "regression": True, "providers": ["test"], "expires": date.max.isoformat()}
        self.decide(consent=consent)
        digest = pack.build(self.export, self.out)
        result = self.unit_at(self.events()[16]["results"][0]["result"]["content"])
        for changes in ({"consent": {**consent, "providers": ["other"]}}, {"keep": [result]},
                        {"pack_id": "0000000000000000"}):
            with self.subTest(changes=changes):
                self.decide(consent=consent, keep=[], pack_id="feedfacefeedface")
                self.decide(**changes)
                with self.assertRaisesRegex(pack.Refused, "confirm"):
                    pack.build(self.export, self.out, confirm=digest)
                self.assertFalse((self.out / "pack").exists())

    def test_a_second_build_cannot_replace_a_pack_being_confirmed(self) -> None:
        self.decide(consent={"diagnosis": True, "regression": False,
                             "providers": [], "expires": date.max.isoformat()})
        digest = pack.build(self.export, self.out)
        other = self.write(export_lines(turn="another-turn"), "other.jsonl")
        entered, resume = Event(), Event()
        write = pack.private

        def pause_at_manifest(path, body):
            if path.name == "pack.json":
                entered.set()
                if not resume.wait(5):
                    raise TimeoutError("test did not resume the first build")
            write(path, body)

        with patch.object(pack, "private", side_effect=pause_at_manifest), ThreadPoolExecutor(1) as runner:
            first = runner.submit(pack.build, self.export, self.out, None, digest)
            try:
                self.assertTrue(entered.wait(5), "first build never reached the manifest write")
                with self.assertRaisesRegex(pack.Refused, "another build owns"):
                    pack.build(other, self.out)
            finally:
                resume.set()
            self.assertEqual(first.result(), digest)
        final = self.out / "pack"
        manifest = json.loads((final / "pack.json").read_bytes())
        self.assertEqual(manifest["timeline_sha256"], hashlib.sha256((final / "timeline.jsonl").read_bytes()).hexdigest())
        self.assertNotIn("another-turn", self.packed())
        self.assertFalse((self.out / ".pack.lock").exists())

    def test_changing_replacements_requires_a_full_review_again(self) -> None:
        self.decide(terms=["owners"], consent={"diagnosis": True, "regression": False,
                                              "providers": [], "expires": date.max.isoformat()})
        digest = pack.build(self.export, self.out)
        self.assertNotIn("/owners", self.packed())
        self.decide(terms=[])
        with self.assertRaisesRegex(pack.Refused, "confirm"):
            pack.build(self.export, self.out, confirm=digest)
        pack.build(self.export, self.out)
        review = (self.out / "review.md").read_text()
        self.assertIn("/owners", review)
        self.assertIn("Read every unit on each pass", review)

    def test_a_confirmed_pack_carries_its_consent(self) -> None:
        consent = {"diagnosis": True, "regression": False, "providers": [], "expires": date.max.isoformat()}
        self.decide(consent={**consent, "given": "2000-01-01", "submitter": "PRIVATE-IDENTITY"})
        digest = pack.build(self.export, self.out)
        manifest_path = self.out / "pack" / "pack.json"
        self.assertNotIn("consent", json.loads(manifest_path.read_text()),
                         "an unconfirmed pack claims an authorization")
        review = (self.out / "review.md").read_text()
        self.assertIn(f"--confirm {digest}", review)
        pack.build(self.export, self.out, confirm=digest)
        manifest = json.loads(manifest_path.read_text())
        self.assertEqual(manifest["consent"], {**consent, "given": date.today().isoformat()})
        self.assertEqual(manifest["timeline_sha256"], hashlib.sha256(manifest_path.with_name("timeline.jsonl").read_bytes()).hexdigest())
        self.assertEqual(digest, hashlib.sha256(manifest_path.read_bytes()).hexdigest())
        self.assertIn(manifest_path.read_text().rstrip("\n"), review)
        pack.build(self.export, self.out)
        self.assertNotIn("consent", json.loads(manifest_path.read_text()),
                         "a preview inherited an earlier confirmation")

    def test_consent_can_be_confirmed_on_its_expiry_date_but_not_the_next_day(self) -> None:
        expiry = date(2026, 11, 18)
        self.decide(consent={"diagnosis": True, "regression": False,
                             "providers": [], "expires": expiry.isoformat()})
        with patch.object(pack, "date", wraps=date) as clock:
            clock.today.return_value = expiry
            digest = pack.build(self.export, self.out)
            self.assertEqual(pack.build(self.export, self.out, confirm=digest), digest)
            manifest = json.loads((self.out / "pack" / "pack.json").read_text())
            self.assertEqual(manifest["consent"]["given"], expiry.isoformat())
            clock.today.return_value = expiry + timedelta(days=1)
            with self.assertRaisesRegex(pack.Refused, "^consent: .*already passed"):
                pack.build(self.export, self.out, confirm=digest)
        self.assertFalse((self.out / "pack").exists())
        self.assertFalse((self.out / "pack.tmp").exists())

    def test_invalid_consent_does_not_leave_a_pack(self) -> None:
        valid = {"diagnosis": True, "regression": True, "providers": ["test"], "expires": date.max.isoformat()}
        for label, consent in (
            ("missing", None),
            ("non-boolean diagnosis", {**valid, "diagnosis": "true"}),
            ("no regression providers", {**valid, "providers": []}),
            ("provider is not an ID", {**valid, "providers": [42]}),
            ("provider has free text", {**valid, "providers": ["private customer name"]}),
            ("credential in providers", {**valid, "providers": ["sk-" + "x" * 24]}),
            ("missing expiry", {k: v for k, v in valid.items() if k != "expires"}),
            ("wrong date format", {**valid, "expires": "16-05-2027"}),
            ("impossible date", {**valid, "expires": "2027-02-30"}),
        ):
            with self.subTest(label=label):
                self.decide(consent=valid)
                digest = pack.build(self.export, self.out)
                self.decide(consent=consent)
                with self.assertRaisesRegex(pack.Refused, "^consent:"):
                    pack.build(self.export, self.out, confirm=digest)
                self.refused("^consent:")

    def test_the_same_input_gives_the_same_bytes(self) -> None:
        self.decide()
        pack.build(self.export, self.out)
        first = {p.name: p.read_bytes() for p in (self.out / "pack").iterdir()}
        other = self.root / "again"
        other.mkdir()
        shutil.copy(self.out / "decisions.json", other / "decisions.json")
        pack.build(self.export, other)
        self.assertEqual(first, {p.name: p.read_bytes() for p in (other / "pack").iterdir()})

    # ---- account names --------------------------------------------------------

    def test_a_short_account_name_does_not_rewrite_the_conversation(self) -> None:
        export = self.write(export_lines(home="/Users/me"), "short.jsonl")
        pack.build(export, self.out)
        question = self.events()[4]["user"]["content"][0]["text"]
        self.assertTrue(question.startswith("List /Users/us/work and say who owns it."), question)
        self.assertIn("- `me`", (self.out / "review.md").read_text())

    def test_a_short_windows_account_is_removed_from_both_separator_styles(self) -> None:
        lines = export_lines()
        home = r"C:\Users\xy"
        lines[4]["event"]["data"]["blocks"][0]["text"] = f"<home>{home}</home>"
        paths = (home, "c:/users/XY", json.dumps(home)[1:-1], urllib.parse.quote(home, safe=""))
        lines[5]["event"]["data"]["user"]["content"][0]["text"] = " | ".join(paths)
        pack.build(self.write(lines, "windows.jsonl"), self.out)
        text = self.events()[4]["user"]["content"][0]["text"]
        for path in paths:
            self.assertNotIn(path.lower(), text.lower())
        self.assertIn(r"C:\Users\us", text)
        self.assertIn("C:/Users/us", text)

    def test_case_folded_reserved_path_cannot_delete_the_export(self) -> None:
        self.out.mkdir()
        actual = self.out / "pack" / "local-input.jsonl"
        actual.parent.mkdir()
        actual.write_text(self.export.read_text(encoding="utf-8"), encoding="utf-8")
        alias = self.out / "PACK" / "local-input.jsonl"
        with self.assertRaisesRegex(pack.Refused, "generated files"):
            pack.build(alias, self.out)
        self.assertTrue(actual.exists(), "input inside a reserved path was deleted")

    def test_input_overlap_is_checked_after_resolving_directory_links(self) -> None:
        child = self.root / "child"
        child.mkdir()
        alias = self.root / "aliases" / "child"
        alias.parent.mkdir()
        alias.symlink_to(child, target_is_directory=True)
        decisions = alias / ".." / self.export.name
        self.assertTrue(decisions.samefile(self.export))
        before = self.export.read_bytes()
        with self.assertRaisesRegex(pack.Refused, "separate from each other"):
            pack.build(self.export, self.out, decisions)
        self.assertEqual(self.export.read_bytes(), before)
        self.assertFalse(self.out.exists())

    def test_an_account_everyone_has_is_left_alone(self) -> None:
        export = self.write(export_lines(home="/home/ubuntu"), "generic.jsonl")
        pack.build(export, self.out)
        self.assertIn("It is owned by ubuntu (", self.events()[23]["content"][0]["text"])

    # ---- a name cut across deltas ----------------------------------------------

    def streamed(self) -> list[str]:
        lines = [json.loads(line) for line in self.packed().splitlines()]
        return [line["event"]["data"]["delta"] for line in lines
                if line["kind"] == "event" and line["event"]["name"] == "text"]

    def test_a_name_cut_across_deltas_is_replaced(self) -> None:
        for interrupt in (False, True):
            with self.subTest(interrupt=interrupt):
                pack.build(self.write(split_export(interrupt), "split.jsonl"), self.out)
                deltas = self.streamed()
                self.assertEqual(len(deltas), len(SPLIT), "the pack has a different number of deltas")
                self.assertEqual("".join(deltas), "我看完了 /Users/useruseru/Desktop 的文件，useruseru 说先别动")
                for delta in deltas:
                    self.assertNotIn("jin", delta, "half of the name survived in one delta")
                    self.assertNotIn("yaxuan", delta, "half of the name survived in one delta")

    def test_halves_of_a_name_put_back_are_found(self) -> None:
        def change(lines):
            deltas = [line for line in lines if line["kind"] == "event" and line["event"]["name"] == "text"]
            for line, part in zip(deltas, SPLIT):
                line["event"]["data"]["delta"] = part
        found = self.audit_of(change, self.write(split_export(True), "split.jsonl"))
        self.assertTrue(any(f.startswith("content: the stream starting at") for f in found), found)
        self.assertTrue(any(f.startswith("replacement: the stream starting at") for f in found), found)

    # ---- several branches -------------------------------------------------------

    def test_level_one_keeps_the_dialogue_when_a_child_branch_comes_first(self) -> None:
        pack.build(self.write(branched_export(), "branched.jsonl"), self.out)
        lines = [json.loads(line) for line in self.packed().splitlines()]
        self.assertEqual(lines[1]["info"]["id"], CHILD, "the fixture no longer puts the child first")
        main = {line["event"]["sequence"]: line["event"]["data"] for line in lines
                if line["kind"] == "event" and line["branch_id"] == "main"}
        child = {line["event"]["sequence"]: line["event"]["data"] for line in lines
                 if line["kind"] == "event" and line["branch_id"] == CHILD}
        question = main[4]["user"]["content"][0]["text"]
        reply = main[23]["content"][0]["text"]
        self.assertTrue(question.startswith("List /Users/useru/work"), "main's question became `other`")
        self.assertTrue(reply.startswith("It is owned by useru"), "main's reply became `other`")
        inherited = child[5]["request"]["messages"]
        self.assertEqual(inherited[1]["content"], question)
        self.assertEqual(inherited[2]["content"][0]["text"], reply)
        self.assertEqual(child[3]["user"]["content"][0]["text"], "Now do the same for /Users/useru/play.")
        self.assertEqual(child[7]["content"][0]["text"], "Done: /Users/useru/play is owned by useru too.")
        self.assertRegex(inherited[0]["content"][0]["text"], pack.STUB, "the environment block was kept")
        review = (self.out / "review.md").read_text()
        self.assertNotIn("other, first", review, "a dialogue unit was reviewed as `other`")
        self.assertIn("user-input, first at main#4", review, "the kind points at the request, not the event")

    def test_home_comes_from_dims_environment_block_only(self) -> None:
        pack.build(self.write(branched_export(), "branched.jsonl"), self.out)
        self.assertNotIn("alice", self.packed().lower())
        review = (self.out / "review.md").read_text()
        self.assertIn("| `/Users/alice` | `/Users/useru` |", review)
        self.assertNotIn("fixture", review.split("## 3.")[0], "a quoted home tag was taken for the home")

    def test_xml_escaped_home_is_replaced_in_both_forms(self) -> None:
        home = "/Users/R&DTeam"
        lines = export_lines(home=home)
        escaped = home.replace("&", "&amp;")
        environment = lines[4]["event"]["data"]["blocks"][0]
        environment["text"] = environment["text"].replace(home, escaped)
        for index in (7, 19):
            lines[index]["event"]["data"]["request"]["messages"][0]["content"][0]["text"] = environment["text"]
        export = self.write(lines)
        pack.build(export, self.out)
        self.assertNotIn("R&DTeam", self.packed())
        self.assertNotIn("R&amp;DTeam", self.packed())
        self.decide(keep=[self.unit_at(self.events()[3]["blocks"][0]["text"])])
        pack.build(export, self.out)
        self.assertIn("<home>/Users/useruse</home>", self.events()[3]["blocks"][0]["text"])
        self.assertNotIn("R&DTeam", self.packed())
        self.assertNotIn("R&amp;DTeam", self.packed())

    def test_the_order_of_branches_does_not_change_what_is_kept(self) -> None:
        manifests = []
        for name, main_first in (("child-first.jsonl", False), ("main-first.jsonl", True)):
            out = self.root / name.removesuffix(".jsonl")
            pack.build(self.write(branched_export(main_first), name), out)
            manifests.append(json.loads((out / "pack" / "pack.json").read_text()))
        self.assertEqual(manifests[0]["units"], manifests[1]["units"])
        self.assertEqual(manifests[0]["models"], ["test/model-1"])

    # ---- refusals ---------------------------------------------------------------

    def refused(self, pattern, export=None) -> None:
        with self.assertRaisesRegex(pack.Refused, pattern):
            pack.build(export or self.export, self.out)
        self.assert_no_output()

    def assert_no_output(self) -> None:
        for name in ("pack", "pack.tmp", "review.md", ".pack.lock"):
            self.assertFalse((self.out / name).exists(), f"a failed run left {name} behind")

    def test_a_non_object_timeline_line_is_refused(self) -> None:
        pack.build(self.export, self.out)
        lines = export_lines()
        lines.insert(2, ["not a Timeline record"])
        self.refused("each Timeline line must be a JSON object", self.write(lines))

    def test_a_term_in_manifest_consent_is_refused(self) -> None:
        self.decide(terms=["private-customer"], consent={"diagnosis": False, "regression": True,
                    "providers": ["private-customer"], "expires": date.max.isoformat()})
        self.refused("^manifest: .* identifier marked for replacement")

    def test_invalid_pack_id_is_refused(self) -> None:
        pack.build(self.export, self.out)
        self.decide(pack_id="private-customer")
        self.refused("^decisions: `pack_id`")

    def test_write_failure_leaves_no_pack_or_review(self) -> None:
        pack.build(self.export, self.out)
        decisions = self.out / "decisions.json"
        original = decisions.read_bytes()
        replace = Path.replace

        def fail_to_save(path, target):
            if target == decisions:
                raise OSError("injected decisions write failure")
            return replace(path, target)

        with patch.object(Path, "replace", fail_to_save):
            with self.assertRaisesRegex(OSError, "injected decisions write failure"):
                pack.build(self.export, self.out)
        self.assert_no_output()
        self.assertEqual(decisions.read_bytes(), original)
        self.assertEqual(list(self.out.iterdir()), [decisions])

    def test_an_incomplete_export_is_refused(self) -> None:
        pack.build(self.export, self.out)
        self.refused("incomplete", self.write(export_lines(end=False), "cut.jsonl"))
        lines = export_lines()
        lines[-1]["events"] += 1
        self.refused("incomplete", self.write(lines, "miscounted.jsonl"))

    def test_an_unknown_unit_is_refused(self) -> None:
        self.decide(keep=["u9999"])
        self.refused("no unit is named u9999")

    def test_a_term_that_survives_in_structure_is_refused(self) -> None:
        self.decide(terms=["main"])
        self.refused("replacement: main#")

    def test_a_credential_outside_content_is_refused(self) -> None:
        self.refused("secret: main#5", self.write(export_lines(turn="sk-" + "a" * 24), "secret.jsonl"))

    def test_a_credential_in_kept_content_is_replaced(self) -> None:
        lines = export_lines()
        token = "ghp_" + "b" * 36
        lines[5]["event"]["data"]["user"]["content"][0]["text"] += f" token {token}"
        lines[7]["event"]["data"]["request"]["messages"][1]["content"] += f" token {token}"
        pack.build(self.write(lines, "token.jsonl"), self.out)
        self.assertTrue(self.events()[4]["user"]["content"][0]["text"].endswith(" token [secret-1]"))
        self.assertNotIn(token, self.packed())

    def audit_of(self, change, export=None) -> list[str]:
        """What the checks say about a pack that `change` edited after it was written."""
        export = export or self.export
        pack.build(export, self.out)
        with pack.prepare(export, pack.load_decisions(self.out / "decisions.json")) as (scan, replacements):
            lines = [json.loads(line) for line in self.packed().splitlines()]
            change(lines)
            return pack.audit(export, self.write(lines, "edited.jsonl"), scan, replacements)

    def test_the_written_pack_passes_its_own_checks(self) -> None:
        self.assertEqual(self.audit_of(lambda lines: None), [])

    def test_content_put_back_is_found(self) -> None:
        def change(lines):
            lines[17]["event"]["data"]["results"][0]["result"]["content"] = "something that was never reviewed"
        self.assertTrue(any(f.startswith("content: main#16") for f in self.audit_of(change)))

    def test_a_provider_blob_put_back_is_found(self) -> None:
        def change(lines):
            lines[15]["event"]["data"]["content"][0]["signature"] = "opaque bytes"
        self.assertTrue(any(f.startswith("opaque: main#14") for f in self.audit_of(change)))

    def test_a_provider_identifier_put_back_is_found(self) -> None:
        def change(lines):
            lines[13]["event"]["data"]["call"]["id"] = "call_from_the_provider"
        self.assertTrue(any(f.startswith("identifier: main#12") for f in self.audit_of(change)))

    def test_a_changed_line_is_found(self) -> None:
        def reorder(lines):
            lines[6]["event"]["sequence"] = 99

        def drop(lines):
            del lines[6]
        self.assertTrue(any(f.startswith("structure: main#5") for f in self.audit_of(reorder)))
        self.assertTrue(any(f.startswith("structure:") for f in self.audit_of(drop)))


if __name__ == "__main__":
    unittest.main()
