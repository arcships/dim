#!/usr/bin/env python3
"""Pack a Timeline export for an evidence pack.

Content is omitted unless a rule or the submitter keeps it, identifiers in
what is kept are replaced, and no pack is written when a check fails. The
contract is docs/eval/evidence-pack.md. The tool reads the JSONL written by
`dimcode export`, runs on the submitter's machine and connects to nothing.

    pack.py EXPORT --out DIR --dimcode /absolute/path/to/dimcode [--decisions FILE] [--confirm DIGEST]

DIR/pack/ is the pack. Until confirmed, it is for local review only.
DIR/review.md and the decisions file always stay local.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import json
import math
import os
import re
import secrets
import sys
import tempfile
import subprocess
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from datetime import date
from pathlib import Path
from xml.sax.saxutils import unescape

RULES_VERSION = 9
PACK_ID = re.compile(r"[0-9a-f]{16}")
PROVIDER_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}")
LEVEL_ONE = {"user-input", "assistant-text"}
# A string seen in several places takes the first of these kinds that any of
# its occurrences has, so the kind does not depend on the order of the lines.
# The export puts every child branch before `main` (branch ids sort before
# "main"), and a child's requests carry the context it inherited: taking the
# first occurrence's kind would make the whole dialogue `other`. Once a string
# is kept anywhere it is in the pack, so a level-one kind always wins.
PRECEDENCE = ("user-input", "assistant-text", "thinking", "tool-args", "tool-result", "injected", "error",
              "system", "tool-def", "attachment", "other")
RANK = {kind: rank for rank, kind in enumerate(PRECEDENCE)}
OPAQUE = {"signature", "thought_signature", "thoughtSignature", "encryptedContent", "encrypted_content"}
REMOVED = "[removed]"
SESSION = "00000000-0000-4000-8000-000000000000"
STUB = re.compile(r"\[omitted u\d+ [a-z-]+ \d+B\]")
PSEUDONYM = re.compile(r"(item|call|tool)-\d+")
RUNS = {"thinking": "thinking", "text": "assistant-text"}
KEPT_TOOL_SOURCES = {"builtin", "extension"}
ASSIGNMENT = re.compile(r"(?i)\b\w*(?:api_?key|secret|token|password|passwd|credential)\w*\s*[=:]\s*\S{6,}")
HOME = re.compile(r"<home>([^<\n]+)</home>")
PASTED = re.compile(r"\[Pasted text: ([^\s\]]+)")
URL_HOST = re.compile(r"https?://([A-Za-z0-9.-]+\.[A-Za-z]{2,})")
HOSTED_OWNER = re.compile(r"(?:github|gitlab)\.com[/:]([A-Za-z0-9_.-]+)/|bitbucket\.org/([A-Za-z0-9_.-]+)/")
PUBLIC_HOSTS = {"github.com", "api.github.com", "raw.githubusercontent.com", "gitlab.com", "bitbucket.org",
                "example.com", "example.org", "json-schema.org"}


class Refused(Exception):
    """The pack is not written; the message says which check failed."""


# Field allowlists for the documented Timeline export and reviewed event payloads.

KEEP = ("keep",)
INTEGER = ("integer",)
NUMBER = ("number",)
BOOL = ("bool",)
PASS = ("pass",)
CALL = ("id", "call")
ITEM = ("id", "item")
TOOL = ("tool",)
SESSION_ID = ("session",)
USER_CONTENT = ("user-content",)

# crates/dim-agent-types/src/markup.rs: MARKERS and char::is_whitespace.
RUNTIME_MARKUP = re.compile(
    r"<dim-(?:agent-approval|agent-approval-ended|agent-message|agent-report|branch-ended|"
    r"branch-idle|branch-line|branch-lines|branch-message|branch-parent|digest|event|event-guide|"
    r"goal|hooks|notification|rule|scheduler|shell-input|skill|state|summary|workflow)"
    r"[>\t-\r \x85\xa0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]"
)


def text(kind):
    return ("text", kind)


def obj(kind):
    return ("object", kind)


@functools.cache
def content(kind):
    """A message or result body: one string, or a list of blocks."""
    return lambda value: [block(kind)] if isinstance(value, list) else text(kind)


@functools.cache
def block(kind):
    return {
        "type": KEEP, "text": text(kind), "thinking": text("thinking"),
        "id": CALL, "tool_use_id": CALL, "name": TOOL, "input": obj("tool-args"),
        "content": content("tool-result"), "mimeType": KEEP, "data": text("attachment"),
        "source": {"type": KEEP, "mediaType": KEEP, "fileId": text("attachment"),
                   "data": text("attachment"), "url": text("attachment")},
        "providerMetadata": metadata_spec,
        **dict.fromkeys(OPAQUE, KEEP),
    }


def opaque_field(container, key):
    return key in OPAQUE or (
        key == "data" and container.get("protocol") in ("bedrock-redacted", "anthropic-redacted"))


def metadata_spec(value):
    spec = {"protocol": KEEP, "itemId": ITEM, "summaryIndex": INTEGER, "phase": KEEP,
            **dict.fromkeys(OPAQUE, KEEP)}
    if isinstance(value, dict) and opaque_field(value, "data"):
        spec["data"] = KEEP
    return spec


def system(value):
    return [{"type": KEEP, "text": text("system")}] if isinstance(value, list) else text("system")


IDS = {"turn_id": KEEP, "request_id": KEEP}
REF = {"timeline": KEEP, "revision": KEEP}
TOOL_DEFINITION = {"name": TOOL, "source": KEEP, "risk": text("tool-def"),
                   "description": text("tool-def"), "inputSchema": obj("tool-def")}
CAPABILITIES = {
    "modalities": [KEEP], "toolCalling": BOOL, "streaming": BOOL, "structuredOutput": BOOL,
    "vision": BOOL, "audio": BOOL, "reasoning": BOOL, "contextWindow": INTEGER, "maxOutputTokens": INTEGER,
}
MODEL = {"providerId": KEEP, "modelId": KEEP,
         "capabilitySnapshot": {"target": {"providerId": KEEP, "modelId": KEEP},
                                "capabilities": CAPABILITIES, "reasoning": {"mode": KEEP},
                                "serviceTier": {"supported": BOOL, "default": KEEP, "options": [KEEP]}},
         "metadata": {"providerProtocol": KEEP}, "tools": [TOOL_DEFINITION],
         "config": {**dict.fromkeys(("temperature", "maxTokens", "topP", "topK"), NUMBER),
                    "stopSequences": [text("other")], "reasoning": obj("other"), "providerOptions": obj("other")}}
USAGE = {**dict.fromkeys(("promptTokens", "completionTokens", "totalTokens", "cacheReadTokens",
                          "cacheWriteTokens", "reasoningTokens", "cost"), NUMBER), "currency": KEEP}
# Messages inside a request are `other`: a request-only message has no known
# origin. The same string in an event field outranks it (PRECEDENCE).
REQUEST = {**MODEL, "messages": [{"role": KEEP, "content": content("other")}], "system": system}
CALLED = {"id": CALL, "name": TOOL, "input": obj("tool-args")}
RESULT = {"content": content("tool-result"), "structuredContent": obj("tool-result"), "isError": BOOL,
          "effects": [{"kind": KEEP, "command": text("tool-result"), "path": text("tool-result"),
                       "exitCode": INTEGER, "signal": text("tool-result")}]}
SENT = {**IDS, "request": REQUEST, "configuration": REF, "prompt": REF}
# `status` and `finish_reason` are enum tags; `reason` and `error` are free text.
OUTCOME = {"status": KEEP, "finish_reason": KEEP, "reason": text("error"), "error": text("error")}
# `BranchSource` carries a provider call id beside the internal ones.
SOURCE = {"kind": KEEP, "turn_id": KEEP, "request_id": KEEP, "call_id": CALL, "operation_id": KEEP}
TARGET = {"kind": KEEP, "agent_id": KEEP, "session_id": SESSION_ID, "profile": obj("tool-def")}
PEER = {"turn_id": KEEP, "request_id": KEEP, "call_id": CALL, "agent_id": KEEP, "session_id": SESSION_ID}
COMPACT = {**IDS, "command": INTEGER, "through": INTEGER, "trigger": KEEP, "strategy": KEEP}
COMPACTION = {**IDS, "command": INTEGER, "start": INTEGER, "through": INTEGER, "status": KEEP,
              "retain_from": KEEP, "strategy": KEEP, "reason": text("error"), "error": text("error"),
              "replacement": [{"role": KEEP, "content": content("injected")}], "diagnostics": obj("other"),
              "replacement_parts": {"strategy": INTEGER, "digest": INTEGER}, "pinned_inputs": [KEEP],
              "digest_entries": obj("other"),
              "context_baseline": [{"key": text("injected"), "sourceEvent": KEEP,
                                    "message": {"role": KEEP, "content": content("injected")}}]}

EVENTS = {
    "approval/cancelled": {"approval_id": KEEP, "reason": text("error")},
    "approval/request": {"approval_id": KEEP, "source": SOURCE, "reason": text("tool-args"),
                         "call": {"name": TOOL, "input": obj("tool-args")}},
    "approval/respond": {"approval_id": KEEP, "decision": KEEP},
    "branch/create": {"at": {"kind": KEEP, "sequence": INTEGER}, "source": SOURCE,
                      "branch": KEEP, "prompt": text("tool-args")},
    "branch/create-failed": {"status": KEEP, "branch": KEEP, "createEvent": INTEGER,
                             "error": text("error"), "reason": text("error")},
    "branch/end": {"source": SOURCE, "ancestor": KEEP},
    "branch/ended": {"branch": KEEP, "endEvent": INTEGER, "source": SOURCE, "ancestor": KEEP},
    "branch/default": {"branch": KEEP, "from": KEEP, "source": SOURCE},
    "branch/resume": {},
    "branch/start": {"parent": KEEP, "createEvent": INTEGER},
    "collaboration/cancel": PEER,
    "collaboration/close": PEER,
    "collaboration/delivered": {"op_event_id": KEEP, "status": KEEP, "agent_id": KEEP, "session_id": SESSION_ID,
                                "input_event_id": KEEP, "outcome": KEEP, "error": KEEP},
    "collaboration/message": {**PEER, "mode": KEEP, "content": text("tool-args")},
    "collaboration/request": {**PEER, "target": TARGET, "prompt": text("tool-args"), "title": text("tool-args")},
    "collaboration/result": {"request_event_id": KEEP, "agent_id": KEEP, "session_id": SESSION_ID,
                             "last_turn_id": KEEP, "text": text("tool-result"),
                             "status": KEEP, "reason": text("error")},
    "context/budget": {**IDS, "boundary": KEEP, "baselineSource": KEEP, "estimatedInputTokens": NUMBER,
                       "baselinePromptTokens": NUMBER, "deltaTokens": NUMBER, "contextWindow": NUMBER,
                       "outputTokens": NUMBER},
    "context/compact": {},
    "context/compaction": COMPACTION,
    "context/compaction-start": COMPACT,
    "context/configured": {**REF, "prompt": REF, "settings": MODEL, "system": system, "system_source": system},
    "context/prepare": {**SENT, "through": INTEGER},
    "context/scope": {"scope": {"kind": KEEP, "timeline": KEEP, "id": KEEP}, "status": KEEP},
    "exec/ended": {"task_id": KEEP, "status": KEEP, "result": RESULT},
    "exec/started": {**IDS, "call_id": CALL, "task_id": KEEP, "description": text("tool-args"),
                     "title": text("tool-args"), "output_path": text("tool-result")},
    "input/applied": {"input_id": KEEP, "mode": KEEP},
    "input/consumed": {"input_id": KEEP},
    "input/deleted": {"input_id": KEEP},
    "input/message": {"submission_id": KEEP, "mode": KEEP, "user": {"content": USER_CONTENT},
                      "blocks": [{"type": KEEP, "text": text("injected"), "key": text("injected")}]},
    "input/withdraw": {"input_id": KEEP},
    "model/call": SENT,
    "model/usage": {**IDS, "usage": USAGE},
    "permission/changed": {"policy": obj("injected")},
    "prompt/changed": REF,
    "prompt/configure-request": {"operation_id": KEEP},
    "prompt/configure-result": {"operation_id": KEEP},
    "session/forked": {},
    "step/model-content": {**IDS, "content": [block("assistant-text")]},
    "step/model-output-end": {**IDS, **OUTCOME},
    "step/model-start": IDS,
    "step/text": {**IDS, "delta": text("assistant-text")},
    "step/thinking": {**IDS, "delta": text("thinking")},
    "step/tool-call": {**IDS, "calls": [CALLED]},
    "step/tool-completed": {**IDS, "call_id": CALL, "result": RESULT, "call_event_id": KEEP},
    "step/tool-result": {**IDS, "call_event_id": KEEP, "results": [{"call_id": CALL, "result": RESULT}]},
    "step/tool-start": {**IDS, "call_id": CALL, "call_event_id": KEEP},
    "step/tool-use": {**IDS, "call": CALLED},
    "step/tool-use-start": {**IDS, "call_id": CALL, "name": TOOL},
    "stop/interrupt": {"reason": text("error"), "scope": KEEP},
    "turn/end": {"turn_id": KEEP, **OUTCOME},
    "turn/start": {"turn_id": KEEP},
}
LINES = {
    "header": {"kind": KEEP, "session_id": SESSION_ID, "export": INTEGER,
               "store_format": INTEGER, "sensitive": BOOL},
    "branch": {"kind": KEEP, "head": INTEGER,
               "info": {"id": KEEP, "parent": KEEP, "branch_point": INTEGER, "create_anchor": INTEGER}},
    "end": {"kind": KEEP, "branches": INTEGER, "events": INTEGER},
}


def line_spec(record, data=None):
    """The spec of one line; `data` overrides the spec of an event's data."""
    event = record.get("event")
    if record.get("kind") != "event" or not isinstance(event, dict):
        return LINES.get(record.get("kind"), {"kind": KEEP})
    if data is None:
        data = EVENTS.get(f"{event.get('type')}/{event.get('name')}", ("omit-data",))
    return {"kind": KEEP, "branch_id": KEEP, "batch_end": INTEGER, "trace": obj("other"),
            "event": {"id": KEEP, "sequence": INTEGER, "create_at": INTEGER, "parent_call_id": CALL,
                      "type": KEEP, "name": KEEP, "data": data}}


def walk(value, spec, visit):
    """Projects declared fields and passes reviewable values to `visit`."""
    if callable(spec):
        spec = spec(value)
    if spec == ("omit-data",):
        return {}
    if value is None:
        return None
    if spec == USER_CONTENT:
        if not isinstance(value, list):
            return walk(value, text("user-input"), visit)
        projected = []
        for index, item in enumerate(value):
            body = item.get("text") if isinstance(item, dict) and item.get("type") == "text" else None
            kind = "injected" if index and isinstance(body, str) and RUNTIME_MARKUP.match(body) else "user-input"
            projected.append(walk(item, block(kind), visit))
        return projected
    if isinstance(spec, dict) and isinstance(value, dict):
        return {k: visit("opaque", None, v) if opaque_field(value, k) else walk(v, spec[k], visit)
                for k, v in value.items() if k in spec}
    if isinstance(spec, list) and isinstance(value, list):
        return [walk(v, spec[0], visit) for v in value]
    if ((spec == INTEGER and type(value) is int)
            or (spec == BOOL and type(value) is bool)
            or (spec == NUMBER and (type(value) is int or type(value) is float and math.isfinite(value)))):
        return value
    if isinstance(spec, tuple):
        op, *args = spec
        if isinstance(value, str) and op in ("keep", "session", "id", "tool", "pass"):
            return visit(op, args[0] if args else None, value) if value else value
        if op == "text" and isinstance(value, str):
            return visit("text", args[0], value) if value else value
        if op == "object" and isinstance(value, (dict, list)):
            return visit("object", args[0], value) if value else value
    raise Refused("input: a listed Timeline field has an unexpected JSON type")


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def read(path):
    with open(path, encoding="utf-8") as lines:
        for line in lines:
            if line.strip():
                record = json.loads(line)
                if not isinstance(record, dict):
                    raise Refused("input: each Timeline line must be a JSON object")
                if not isinstance(record.get("kind"), str):
                    raise Refused("input: each Timeline line must have a string `kind`")
                yield record


# Delta text is dealt with at stream scope; its other fields use the same allowlist as ordinary events.
DELTA = {**IDS, "delta": PASS}


def is_delta(record):
    """Whether a line is one delta of a stream, whatever other fields its data has.

    `dimcode export` writes `DeltaData` (`turn_id`, `request_id`, `delta`); a
    copy read from the store's batched rows may carry the delta alone.
    """
    event = record.get("event")
    return (record.get("kind") == "event" and isinstance(event, dict) and event.get("type") == "step"
            and event.get("name") in RUNS and isinstance(event.get("data"), dict)
            and isinstance(event["data"].get("delta"), str))


def streams(records):
    """Pairs each line with the stream its delta belongs to, or with None.

    Streamed output is committed as many delta events, each as short as one
    character, and other events can be committed between two of them; an
    identifier can straddle any two. A stream is every delta of one kind in
    one model request of one branch, from its `step/model-start` to its
    `step/model-output-end`, whatever lies between: it is replaced, kept and
    checked as one string.
    """
    segment, branch = 0, None
    for record in records:
        event = record.get("event")
        if record.get("kind") == "event" and isinstance(event, dict):
            if record.get("branch_id") != branch or (
                    event.get("type") == "step" and event.get("name") in ("model-start", "model-output-end")):
                segment += 1
                branch = record.get("branch_id")
        yield record, ((branch, segment, event["name"]) if is_delta(record) else None)


def with_delta(record, delta):
    event = record["event"]
    return {**record, "event": {**event, "data": {**event["data"], "delta": delta}}}


# ---- Pass one: what the export holds -----------------------------------------


@dataclass
class Unit:
    number: int
    kind: str
    size: int
    where: str
    seen: int = 1
    kept: bool = False

    @property
    def name(self):
        return f"u{self.number}"

    @property
    def stub(self):
        return f"[omitted {self.name} {self.kind} {self.size}B]"


class Scan:
    def __init__(self, export):
        self.units = {}
        self.ids = {"call": {}, "item": {}}
        self.tools = {}
        self.sources = {}
        self.sessions = {}
        self.homes = set()
        self.models = set()
        self.external = set()
        self.streams = {}  # stream -> (where it starts, its deltas)
        self.where = ""
        header = last = None
        lines = events = branches = 0
        for record, stream in streams(read(export)):
            header = header or record
            last = record
            lines += 1
            events += record.get("kind") == "event"
            branches += record.get("kind") == "branch"
            self.where = self.position(record)
            if stream is None:
                self.note(record)
                walk(record, line_spec(record), self.visit)
                continue
            self.streams.setdefault(stream, (self.where, []))[1].append(record["event"]["data"]["delta"])
            walk(record, line_spec(record, DELTA), self.visit)
        for (_, _, name), (where, parts) in self.streams.items():
            if any(parts):
                self.where = where
                self.unit("".join(parts), RUNS[name])
        if not header or header.get("kind") != "header" or not header.get("session_id"):
            raise Refused("input: the export does not start with a header line containing a Session ID")
        if type(header.get("export")) is not int or header["export"] != 1 or type(header.get("store_format")) is not int:
            raise Refused("input: expected a v1 Timeline export and integer storage version")
        if header.get("sensitive") is not True:
            raise Refused("input: expected the Timeline export to be marked sensitive")
        if not last or last.get("kind") != "end" or last.get("events") != events or last.get("branches") != branches:
            raise Refused("input: the export is incomplete; it must end with an `end` line whose counts match")
        self.lines = lines
        self.export = {"export": header.get("export"), "store_format": header.get("store_format")}

    @staticmethod
    def position(record):
        event = record.get("event")
        if isinstance(event, dict):
            return f"{record.get('branch_id')}#{event.get('sequence')}"
        return str(record.get("kind"))

    def note(self, record):
        """Facts read outside the walk: environment, tool sources, the model, and external files."""
        event = record.get("event")
        data = event.get("data") if isinstance(event, dict) else None
        if not isinstance(data, dict):
            return
        event_kind = f"{event.get('type')}/{event.get('name')}"
        if event_kind == "input/message" and isinstance(data.get("blocks"), list):
            for item in data["blocks"]:
                if not isinstance(item, dict) or item.get("type") != "raw":
                    continue
                key, body = item.get("key"), item.get("text")
                if (isinstance(key, str) and key.split(":", 2)[:2] == ["state", "dim/environment/context"]
                        and isinstance(body, str)):
                    self.homes.update(unescape(value) for value in HOME.findall(body))
        model_field = {"model/call": "request", "context/prepare": "request",
                       "context/configured": "settings"}.get(event_kind)
        holder = data.get(model_field)
        if isinstance(holder, dict):
            provider, model = holder.get("providerId"), holder.get("modelId")
            if isinstance(provider, str) and isinstance(model, str):
                self.models.add(f"{provider}/{model}")
            tools = holder.get("tools")
            for tool in tools if isinstance(tools, list) else ():
                if isinstance(tool, dict) and isinstance(tool.get("name"), str):
                    source = tool.get("source")
                    self.sources.setdefault(tool["name"], source if isinstance(source, str) else None)
        stack = [data]
        while stack:
            value = stack.pop()
            if isinstance(value, dict):
                for key, child in value.items():
                    if key in ("output_path", "outputPath") and isinstance(child, str) and child:
                        self.external.add(child)
                    stack.append(child)
            elif isinstance(value, list):
                stack.extend(value)

    def unit(self, key, kind):
        unit = self.units.get(key)
        if unit is None:
            self.units[key] = Unit(len(self.units) + 1, kind, len(key.encode()), self.where)
            return
        unit.seen += 1
        if RANK[kind] < RANK[unit.kind]:
            unit.kind, unit.where = kind, self.where

    def visit(self, op, arg, value):
        if op == "text":
            self.unit(value, arg)
            if arg == "user-input":
                self.external.update(PASTED.findall(value))
        elif op == "object":
            self.unit("\0" + canonical(value), arg)
        elif op == "id":
            self.ids[arg].setdefault(value, len(self.ids[arg]) + 1)
        elif op == "tool":
            self.tools.setdefault(value, len(self.tools) + 1)
        elif op == "session":
            if value not in self.sessions:
                self.sessions[value] = f"00000000-0000-4000-8000-{len(self.sessions):012x}"
        return value


# ---- Decisions and replacements ------------------------------------------------


def load_decisions(path):
    if path.exists():
        decisions = json.loads(path.read_text(encoding="utf-8"))
    else:
        # The pack id is random: it is derived from no account, Session or content.
        decisions = {"pack_id": secrets.token_hex(8)}
    if not isinstance(decisions, dict):
        raise Refused("decisions: expected a JSON object")
    pack_id = decisions.get("pack_id")
    if not isinstance(pack_id, str) or not PACK_ID.fullmatch(pack_id):
        raise Refused("decisions: `pack_id` must be the generated 16-digit lowercase hex identifier")
    for name in ("keep", "omit", "terms", "number_tools"):
        decisions.setdefault(name, [])
        if not isinstance(decisions[name], list) or not all(isinstance(v, str) and v for v in decisions[name]):
            raise Refused(f"decisions: `{name}` must be a list of non-empty strings")
    return decisions


def save_decisions(path, decisions):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    handle, temporary = tempfile.mkstemp(dir=path.parent)
    temporary = Path(temporary)
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as output:
            json.dump(decisions, output, ensure_ascii=False, indent=2, allow_nan=False)
            output.write("\n")
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def consent_of(decisions, replacements):
    consent = decisions.get("consent")
    if not isinstance(consent, dict):
        raise Refused("consent: name `diagnosis`, `regression`, `providers` and `expires`"
                      " in the decisions file before confirming")
    for name in ("diagnosis", "regression"):
        if not isinstance(consent.get(name), bool):
            raise Refused(f"consent: `{name}` must be true or false")
    providers = consent.get("providers")
    if not isinstance(providers, list) or not all(
            isinstance(v, str) and PROVIDER_ID.fullmatch(v) and not replacements.inspect(v)["secret"] for v in providers):
        raise Refused("consent: `providers` must contain only provider IDs (letters, digits, `.`, `_`, `-`), not credentials")
    if consent["regression"] and not providers:
        raise Refused("consent: a pack kept for the corpus must name at least one provider")
    expires = consent.get("expires")
    if not isinstance(expires, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", expires):
        raise Refused("consent: `expires` must be an ISO date, YYYY-MM-DD")
    try:
        expiry = date.fromisoformat(expires)
    except ValueError:
        raise Refused("consent: `expires` is not a valid calendar date") from None
    given = date.today()
    if expiry < given:
        raise Refused("consent: `expires` has already passed; confirm with a current expiry date")
    return {"diagnosis": consent["diagnosis"], "regression": consent["regression"],
            "providers": providers, "expires": expires, "given": given.isoformat()}


class Redactor:
    def __init__(self, process, scan, decisions, kept):
        self.process = process
        self.memo = {}
        self.counted = set()
        self.calls = {v: f"call-{n}" for v, n in scan.ids["call"].items()}
        self.items = {v: f"item-{n}" for v, n in scan.ids["item"].items()}
        self.kept_tools = {name for name in scan.tools
                           if scan.sources.get(name) in KEPT_TOOL_SOURCES and name not in decisions["number_tools"]}
        self.tools = {name: name if name in self.kept_tools else f"tool-{n}" for name, n in scan.tools.items()}
        identifiers = {**self.calls, **self.items, **self.tools}
        self.sessions = scan.sessions
        primary, *members = self.sessions
        if {s.casefold() for s in self.sessions} & set(self.sessions.values()):
            raise Refused("input: a Session ID collides with a pack stand-in")
        init = self.request({"facts": {"homes": sorted(scan.homes), "session": primary,
                                       "member_sessions": [(s, self.sessions[s]) for s in members],
                                       "identifiers": list(identifiers.items()), "terms": decisions["terms"]},
                             "kept": kept})
        self.labels = init["labels"]
        self.hits = dict.fromkeys(self.labels, 0)
        self.homes = init["homes"]
        self.unreplaced_accounts = set(init["unreplaced_accounts"])
        if init["session"] != SESSION or any(self.labels.get(s) != label for s, label in self.sessions.items()):
            raise Refused("evidence-redact: expected a distinct stand-in for every Session ID; use an up-to-date dimcode")
        if any(self.inspect(s)["text"] != label for s, label in self.sessions.items()):
            raise Refused("evidence-redact: Session IDs do not have unambiguous stand-ins")

    def request(self, value):
        try:
            self.process.stdin.write(json.dumps(value) + "\n")
            self.process.stdin.flush()
            return json.loads(self.process.stdout.readline())
        except (OSError, ValueError) as error:
            raise Refused("evidence-redact: could not exchange JSONL with the trusted dimcode") from error

    def inspect(self, value):
        if value not in self.memo:
            self.memo[value] = self.request(value)
        return self.memo[value]

    def text(self, value):
        reply = self.inspect(value)
        if value not in self.counted:
            self.counted.add(value)
            for _, _, original, _ in reply["spans"]:
                self.hits[original] += 1
        return reply["text"]

    def tree(self, value):
        if isinstance(value, str):
            return REMOVED if value == REMOVED else self.text(value)
        if isinstance(value, dict):
            return {self.text(k): REMOVED if opaque_field(value, k) else self.tree(v) for k, v in value.items()}
        if isinstance(value, list):
            return [self.tree(v) for v in value]
        return value

    def cut(self, parts):
        """Replaces in the joined text and cuts it again at the original boundaries."""
        full = "".join(parts)
        new = self.text(full)
        moved = []  # (old end, new end) of each replaced span
        shift = 0
        for start, end, _, label in self.inspect(full)["spans"]:
            shift += len(label) - (end - start)
            moved.append((start, end, end + shift))
        out, at, previous = [], 0, 0
        for part in parts:
            at += len(part)
            cut = at
            for start, end, new_end in moved:
                if at <= start:
                    break
                # A boundary inside a replaced span moves to the span's end.
                cut = new_end if at < end else at + (new_end - end)
            out.append(new[previous:cut])
            previous = cut
        return out


# ---- Pass two: the pack ----------------------------------------------------------


class Packer:
    def __init__(self, scan, replacements):
        self.scan = scan
        self.replace = replacements

    def visit(self, op, arg, value):
        if op in ("keep", "pass"):
            return value
        if op == "opaque":
            return REMOVED
        if op == "session":
            return self.replace.sessions[value]
        if op == "id":
            return (self.replace.calls if arg == "call" else self.replace.items)[value]
        if op == "tool":
            return self.replace.tools[value]
        if op == "text":
            unit = self.scan.units[value]
            return self.replace.text(value) if unit.kept else unit.stub
        unit = self.scan.units["\0" + canonical(value)]
        if unit.kept:
            return self.replace.tree(value)
        return {"omitted": unit.stub} if isinstance(value, dict) else [unit.stub]

    def lines(self, export):
        parts = {}
        for record, stream in streams(read(export)):
            if stream is None:
                yield walk(record, line_spec(record), self.visit)
                continue
            if stream not in parts:
                original = self.scan.streams[stream][1]
                unit = self.scan.units.get("".join(original))
                if unit is None:
                    new = original
                elif unit.kept:
                    new = self.replace.cut(original)
                else:
                    new = [unit.stub] + [""] * (len(original) - 1)
                parts[stream] = iter(new)
            yield walk(with_delta(record, next(parts[stream])), line_spec(record, DELTA), self.visit)


# ---- Checks: read the pack again -------------------------------------------------


def same_shape(before, after):
    """Whether two values have the same JSON types; an omitted subtree matches any container."""
    if isinstance(after, dict) and set(after) == {"omitted"} and isinstance(before, dict):
        return True
    if isinstance(after, list) and len(after) == 1 and isinstance(after[0], str) and STUB.fullmatch(after[0]):
        return isinstance(before, list)
    if isinstance(before, dict) and isinstance(after, dict):
        return len(before) == len(after) and all(same_shape(a, b) for a, b in zip(before.values(), after.values()))
    if isinstance(before, list) and isinstance(after, list):
        return len(before) == len(after) and all(same_shape(a, b) for a, b in zip(before, after))
    return type(before) is type(after)


def strings(value):
    """Every string and every key of a JSON value."""
    if isinstance(value, str):
        yield None, value
    elif isinstance(value, dict):
        for key, child in value.items():
            yield None, key
            if opaque_field(value, key) and child != REMOVED:
                yield key, ""
            else:
                yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


def audit(export, packed, scan, replacements):
    """Reads the written pack and returns what is wrong with it, one line per finding.

    Nothing here trusts what the packer computed while writing: the pack is
    parsed and classified again, and compared with the export line by line.
    """
    kept_text = {replacements.text(key) for key, unit in scan.units.items() if unit.kept and key[:1] != "\0"}
    kept_trees = {canonical(replacements.tree(json.loads(key[1:])))
                  for key, unit in scan.units.items() if unit.kept and key[:1] == "\0"}
    tools = set(replacements.kept_tools)
    found = []
    clean = set()  # placeholders repeat in every request; each distinct string is searched once

    def content_ok(value):
        return not value or bool(STUB.fullmatch(value)) or value in kept_text

    def source_field(op, arg, value):
        if op == "session":
            expected_sessions.append(replacements.sessions[value])
        return value

    def visit(op, arg, value):
        if op == "session":
            written_sessions.append(value)
        elif op == "text" and not content_ok(value):
            found.append(f"content: {where} holds a string that is neither a placeholder nor a kept unit")
        elif op == "object":
            stub = value.get("omitted") if isinstance(value, dict) and set(value) == {"omitted"} else (
                value[0] if isinstance(value, list) and len(value) == 1 else None)
            if not (isinstance(stub, str) and STUB.fullmatch(stub)) and canonical(value) not in kept_trees:
                found.append(f"content: {where} holds a subtree that is neither a placeholder nor a kept unit")
        elif op == "id" and not PSEUDONYM.fullmatch(value):
            found.append(f"identifier: {where} holds a provider identifier")
        elif op == "tool" and value not in tools and not PSEUDONYM.fullmatch(value):
            found.append(f"tool name: {where} holds a name that is neither kept nor numbered")
        elif op == "opaque" and value != REMOVED:
            found.append(f"opaque: {where} still holds a provider blob")
        return value

    def residue(where, value):
        reply = replacements.inspect(value)
        if reply["spans"]:
            found.append(f"replacement: {where} still holds a replaced identifier")
        elif reply["secret"]:
            found.append(f"secret: {where} holds a string shaped like a credential")
        else:
            return False
        return True

    before = read(export)
    pending = {}  # stream -> (where it starts, its deltas in the pack)
    count = 0
    for new, stream in streams(read(packed)):
        old = next(before, None)
        if old is None:
            found.append("structure: the pack has more lines than the export")
            break
        where = Scan.position(new)
        count += 1
        spec = line_spec(old, DELTA if is_delta(old) else None)
        expected_sessions, written_sessions = [], []
        projected = walk(old, spec, source_field)
        if not same_shape(projected, new) or any(
                old.get(k) != new.get(k) for k in ("kind", "branch_id", "batch_end")) or any(
                (old.get("event") or {}).get(k) != (new.get("event") or {}).get(k)
                for k in ("id", "sequence", "create_at", "type", "name") if isinstance(old.get("event"), dict)):
            found.append(f"structure: {Scan.position(old)} differs from the allowed export structure")
        try:
            if walk(new, spec, visit) != new:
                found.append(f"structure: {where} holds an unlisted field")
            if expected_sessions != written_sessions:
                found.append(f"identifier: {where} does not reference the expected Session stand-in")
        except Refused:
            found.append(f"structure: {where} holds an unexpected JSON type")
        for key, value in strings(new):
            if key and value != REMOVED:
                found.append(f"opaque: {where} still holds a provider blob")
            elif value not in clean and not residue(where, value):
                clean.add(value)
        if stream is not None:
            pending.setdefault(stream, (where, []))[1].append(new["event"]["data"]["delta"])
    # A reader joins the deltas, so the check reads them joined: two halves
    # of a name pass every check one at a time.
    for start, parts in pending.values():
        joined = "".join(parts)
        if not content_ok(joined):
            found.append(f"content: the stream starting at {start} is neither a placeholder nor a kept unit")
        residue(f"the stream starting at {start}", joined)
    if next(before, None) is not None or count != scan.lines:
        found.append("structure: the pack and the export have different line counts")
    for labels in (replacements.calls, replacements.items):
        if len(set(labels.values())) != len(labels):
            found.append("structure: two identifiers share one stand-in")
    return found


# ---- Output -----------------------------------------------------------------------


def private(path, body):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    handle = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as out:
            out.write(body)
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def fence(body):
    ticks = "`" * max(3, 1 + max((len(run) for run in re.findall(r"`+", body)), default=0))
    return f"{ticks}text\n{body}\n{ticks}"


def review(scan, replacements, candidate, confirmation):
    kept = [(key, unit) for key, unit in scan.units.items() if unit.kept]
    omitted = [unit for unit in scan.units.values() if not unit.kept]
    lines = [
        "# Evidence pack review", "",
        "This file stays on your machine. Do not upload it or commit it.", "",
        "Read the full [Timeline](pack/timeline.jsonl) and the exact manifest in section 5."
        " The units below are a reading aid, not a substitute for reviewing both files.", "",
        "## 1. Kept content", "",
        "Read every unit on each pass, even when its id is unchanged. To drop a unit, remove it from"
        " `keep` if listed there, and add its id to `omit` if it is kept by default. Then run again.", "",
    ]
    hosts, owners, folders = set(), set(), set()
    homes = {h.rstrip("/") for h in scan.homes.union(replacements.homes) if h.rstrip("/")}
    for key, unit in kept:
        body = key[1:] if key[:1] == "\0" else key
        shown = canonical(replacements.tree(json.loads(body))) if key[:1] == "\0" else replacements.text(body)
        lines += [f"### {unit.name}: {unit.kind}, first at {unit.where}, {unit.seen} occurrences", ""]
        hints = sorted({m.group(0)[:60] for m in ASSIGNMENT.finditer(shown)})
        if hints:
            lines += ["Looks like an assignment of a secret; check it: " + ", ".join(f"`{h}`" for h in hints), ""]
        lines += [fence(shown), ""]
        hosts.update(h.lower() for h in URL_HOST.findall(shown))
        owners.update(a or b for a, b in HOSTED_OWNER.findall(shown))
        for home in homes:
            folders.update(re.findall(re.escape(home) + r"/([^/\s\"'<>]+)", shown))
    lines += ["## 2. Replacements", "", "| Original | Stands in | Times |", "|---|---|---:|"]
    lines += [f"| `{original}` | `{replacements.labels[original]}` | {times} |"
              for original, times in replacements.hits.items() if times]
    candidates = sorted((hosts - PUBLIC_HOSTS) | owners | folders | replacements.unreplaced_accounts)
    if not scan.homes:
        lines += ["", "The export records no home directory, so no account name was replaced. Add yours to"
                  " `terms`."]
    lines += ["", "Not replaced, and possibly identifying. Add any of these to `terms` to replace it:", ""]
    lines += [f"- `{c}`" for c in candidates] or ["- none found"]
    extension = sorted(n for n in replacements.kept_tools if scan.sources.get(n) != "builtin")
    lines += ["", "Tool names kept as they are. Add one to `number_tools` to number it instead:", ""]
    lines += [f"- `{n}`" for n in extension] or ["- none besides the built-in tools"]
    lines += ["", "## 3. Omitted units", "",
              "Each is a placeholder in the pack. To keep one, add its id to `keep` and run again.", "",
              "| Unit | Kind | First at | Bytes | Occurrences |", "|---|---|---|---:|---:|"]
    lines += [f"| {u.name} | {u.kind} | {u.where} | {u.size} | {u.seen} |" for u in omitted]
    lines += ["", "## 4. Files the Timeline refers to", "", "They are not in the export and are not packed.", ""]
    lines += [f"- `{p}`" for p in sorted(scan.external)] or ["- none"]
    lines += ["", "## 5. Manifest and confirmation", "", fence(candidate.rstrip("\n")), ""]
    if confirmation is None:
        lines += ["Add `diagnosis`, `regression`, `providers` and `expires` under `consent` and rerun without"
                  " `--confirm` to preview the complete manifest.", ""]
    else:
        lines += [f"After reviewing both files in full, run again with `--confirm {confirmation}`.",
                  "If either file or the authorization changes, rerun the preview and review both again.", ""]
    return "\n".join(lines)


def manifest(scan, replacements, decisions, digest):
    kinds = {}
    for unit in scan.units.values():
        entry = kinds.setdefault(unit.kind, {"kept": 0, "kept_bytes": 0, "omitted": 0, "omitted_bytes": 0})
        state = "kept" if unit.kept else "omitted"
        entry[state] += 1
        entry[f"{state}_bytes"] += unit.size
    stand_ins = sorted({replacements.labels[o] for o, times in replacements.hits.items() if times})
    manifest = {
        "pack_id": decisions["pack_id"], "rules": RULES_VERSION, "level": 2 if decisions["keep"] else 1,
        "source": scan.export, "models": sorted(scan.models), "units": dict(sorted(kinds.items())),
        "stand_ins": stand_ins, "files_not_packed": len(scan.external), "timeline_sha256": digest,
    }
    return manifest


@contextmanager
def prepare(export, decisions, dimcode=None):
    """Reads the export and applies the decisions: which units are kept, what is replaced."""
    scan = Scan(export)
    names = {unit.name for unit in scan.units.values()}
    unknown = sorted(set(decisions["keep"] + decisions["omit"]) - names)
    if unknown:
        raise Refused(f"decisions: no unit is named {', '.join(unknown)}")
    for unit in scan.units.values():
        unit.kept = unit.name in decisions["keep"] or (
            unit.kind in LEVEL_ONE and unit.name not in decisions["omit"])
    kept = [key for key, unit in scan.units.items() if unit.kept]
    executable = dimcode if dimcode is not None else os.environ.get("DIM_EVIDENCE_DIMCODE")
    if not executable or not Path(executable).is_absolute():
        raise Refused("evidence-redact: supply a trusted absolute dimcode path via --dimcode or DIM_EVIDENCE_DIMCODE")
    with subprocess.Popen([executable, "evidence-redact"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, text=True, encoding="utf-8") as process:
        try:
            yield scan, Redactor(process, scan, decisions, kept)
            process.stdin.close()
            if process.wait():
                raise Refused("evidence-redact: the trusted dimcode failed")
        finally:
            if process.poll() is None:
                process.kill()


def filesystem_path(path):
    """Return a conservative, case-folded physical path for an existing or future path."""
    return os.path.realpath(path).casefold()


def under(path, directory):
    path, directory = filesystem_path(path), filesystem_path(directory)
    return path == directory or path.startswith(directory + os.sep)


def build(export, out, decisions_path=None, confirm=None, *, dimcode=None):
    """Write the local review and pack; return a confirmation digest if consent was supplied."""
    export, out = Path(export), Path(out)
    decisions_path = Path(decisions_path) if decisions_path else out / "decisions.json"
    out = out.resolve()
    lock = out / ".pack.lock"
    reserved = (out / "pack", out / "pack.tmp", out / "review.md", out / ".pack-receipt", lock)
    input_paths = (export, decisions_path)
    if any(under(path, target) for path in input_paths for target in reserved):
        raise Refused("input: the export and decisions must be separate from each other and the generated files")
    if filesystem_path(export) == filesystem_path(decisions_path):
        raise Refused("input: the export and decisions must be separate from each other and the generated files")
    if not export.is_file():
        raise Refused("input: the export must be an existing file")
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        lock.mkdir(mode=0o700)
    except FileExistsError:
        raise Refused("output: another build owns this directory; see .pack.lock in the packing instructions") from None
    try:
        return build_locked(export, out, decisions_path, confirm, dimcode)
    finally:
        lock.rmdir()


PACK_FILES = ("timeline.jsonl", "pack.json")


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def receipt(pack_dir, review_path):
    hashes = [file_digest(pack_dir / name) for name in PACK_FILES]
    return "Dim evidence pack v1\n" + "\n".join([*hashes, file_digest(review_path)]) + "\n"


def clear_previous(pack_dir, review_path, staging, marker):
    conflict = Refused("output: existing files cannot be verified as this tool's output; "
                       "nothing was rebuilt. Choose a fresh --out directory")
    if os.path.lexists(staging):
        raise conflict
    if not any(os.path.lexists(path) for path in (pack_dir, review_path, marker)):
        return
    if (pack_dir.is_symlink() or not pack_dir.is_dir() or {p.name for p in pack_dir.iterdir()} != set(PACK_FILES)
            or any(p.is_symlink() or not p.is_file() for p in [*(pack_dir / n for n in PACK_FILES), review_path, marker])):
        raise conflict
    expected = receipt(pack_dir, review_path).encode("ascii")
    with marker.open("rb") as stream:
        if stream.read(len(expected) + 1) != expected:
            raise conflict
    for name in PACK_FILES:
        (pack_dir / name).unlink()
    pack_dir.rmdir()
    review_path.unlink()
    marker.unlink()


def build_locked(export, out, decisions_path, confirm, dimcode):
    pack_dir, review_path, staging, marker = (out / name for name in ("pack", "review.md", "pack.tmp", ".pack-receipt"))
    clear_previous(pack_dir, review_path, staging, marker)
    decisions = load_decisions(decisions_path)
    staging.mkdir(mode=0o700)
    try:
        with prepare(export, decisions, dimcode) as (scan, replacements):
            confirmation, review_text = write_preview(export, staging, scan, replacements, decisions, confirm)
        decisions.pop("confirmed", None)
        save_decisions(decisions_path, decisions)
        with ExitStack() as rollback:
            private(review_path, review_text)
            rollback.callback(review_path.unlink)
            private(marker, receipt(staging, review_path))
            rollback.callback(marker.unlink)
            staging.rename(pack_dir)
            rollback.pop_all()
        return confirmation
    finally:
        if staging.exists():
            for name in PACK_FILES:
                (staging / name).unlink(missing_ok=True)
            staging.rmdir()


def write_preview(export, staging, scan, replacements, decisions, confirm):
    consent = consent_of(decisions, replacements) if confirm is not None or "consent" in decisions else None
    timeline = staging / "timeline.jsonl"
    handle = os.open(timeline, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(handle, "wb") as written:
        for line in Packer(scan, replacements).lines(export):
            data = json.dumps(line, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
            written.write((data + "\n").encode("utf-8"))
    found = audit(export, timeline, scan, replacements)
    if found:
        shown = "\n  ".join(sorted(set(found))[:20])
        raise Refused(f"the pack failed {len(set(found))} checks and was not written:\n  {shown}")
    preview = manifest(scan, replacements, decisions, file_digest(timeline))
    candidate = {**preview, "consent": consent} if consent is not None else preview
    candidate_text = json.dumps(candidate, indent=2, allow_nan=False) + "\n"
    for _, value in strings(candidate):
        reply = replacements.inspect(value)
        if reply["secret"] or reply["spans"]:
            raise Refused("manifest: a field contains a credential or an identifier marked for replacement")
    confirmation = hashlib.sha256(candidate_text.encode("utf-8")).hexdigest() if consent is not None else None
    if confirm is not None and confirm != confirmation:
        raise Refused("confirm: the Timeline or manifest changed; rerun without --confirm and review both files")
    manifest_text = candidate_text if confirm is not None else json.dumps(preview, indent=2) + "\n"
    private(staging / "pack.json", manifest_text)
    if (staging / "pack.json").read_bytes() != manifest_text.encode("utf-8"):
        raise Refused("manifest: the written file does not match the audited manifest")
    return confirmation, review(scan, replacements, candidate_text, confirmation)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("export", help="the file written by `dimcode export`")
    parser.add_argument("--out", required=True, help="directory for pack/, review.md and the decisions file")
    parser.add_argument("--decisions", help="decisions file (default: OUT/decisions.json)")
    parser.add_argument("--confirm", metavar="DIGEST", help="confirm both reviewed files and record consent")
    parser.add_argument("--dimcode", metavar="PATH", help="trusted absolute dimcode path (or set DIM_EVIDENCE_DIMCODE)")
    args = parser.parse_args(argv)
    try:
        digest = build(args.export, args.out, args.decisions, args.confirm, dimcode=args.dimcode)
    except Refused as refusal:
        print(f"refused: {refusal}", file=sys.stderr)
        return 1
    except (OSError, json.JSONDecodeError) as failure:
        print(f"error: {failure}", file=sys.stderr)
        return 2
    print(f"pack: {Path(args.out) / 'pack'}")
    print(f"review (local only): {Path(args.out) / 'review.md'}")
    if digest is None:
        print("local preview only: add consent and rerun to obtain a confirmation digest")
    else:
        print(f"confirmation digest: {digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
