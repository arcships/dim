# 014: File access facts

Decision status: Proposed.

Extends ADR 010. It uses the grants of ADR 012 and the host-artifact ownership of ADR 013
(sibling PRs). This decision gives `Permission` one fixed set of facts for file reads and writes.
ADR 003's file is not edited; its D5 file-root sentence is superseded below.

## Context

Reads are path-independent today, writes ask outside the workspace, and the platform's system
roots are reachable for writes under `workspace-write`. A path alone cannot say whether a file is
the session's own artifact. The facts have to be named once, with an owner for each.

## Decision

### D1. Fixed facts

`Permission` decides a file operation from:

| Fact | Source |
| --- | --- |
| workspace | `SessionConfig` |
| system roots | HostPlatform's platform-roots list, independent of the sandbox switch |
| session-owned | ADR 013's ownership fact; `SessionFiles.owns` reads both ownership tables |
| user grant | ADR 012's grants |

The branch preset is a query argument, not a fact. The platform-roots list is HostPlatform state.
Linux uses `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64` and `/etc`; macOS uses `/System`, `/usr`,
`/bin`, `/sbin`, `/Library/Apple`, `/private/etc` (with the `/etc` alias) and `/var/select`.
HostPlatform computes it once when it mounts, and the facts service and the sandbox builder use
the same fixed list, so the fact is non-empty when the sandbox is off. The builder adds the
effective developer directory and git's global configuration separately, per process; those are
not facts.

The `FileAccessFacts` Session part registers the service `fileAccessFacts`. Its query is
`facts(session_id, preset, path, mode) -> FileFacts`, with `workspace`, `system`, `owned` and
`grant` fields; it calls `SessionFiles.owns` (both ownership tables, including command output)
and reads the Session's grants through `SessionGrants`. It does not import
SessionStore. `Permission` passes the branch preset and applies the table below. A query failure
denies. The service holds no state; it reads at use time, so a restart changes nothing.

### D2. Read and write

| Mode | Inside the facts | Outside the facts |
| --- | --- | --- |
| `read-only` | reads allowed | reads ask; writes denied |
| `workspace-write` | reads allowed; writes only in the workspace and write grants | reads and writes ask |
| `full-access` | file-tool writes allowed | reads and writes allowed without a prompt |

The table is about file reads and writes. Processes and network keep their existing rules: the
sandbox setting and the execute-process approval.

- Under `read-only`, the workspace is read-only and a write grant is inert. Under
  `workspace-write`, the workspace and write grants are writable.
- System roots and session-owned artifacts are read-only under `read-only` and `workspace-write`;
  a write to them is denied, not asked. Under `full-access`, file-tool writes are admitted
  anywhere, including them. The process mounts are owned by ADR 010 (workspace and platform
  roots), ADR 012 (grants) and ADR 013 (artifacts); this decision changes none. The trust
  consequence is stated in ADRs 011 and 012.
- A write grant also permits reads; a read grant never permits writes.

### D3. Matching

All file tools with a path input — `read`, `write`, `edit`, `grep` and `glob` — resolve the path
before admission, like `write` and `edit` do today: cwd, `~`, `..` and symlinks are settled, and
`Permission` compares the canonical path. A workspace
symlink to a path outside the workspace does not match the workspace fact. The fact paths are
compared resolved too, with the same longest-existing-prefix rule: a Session started through a
symlinked directory still matches its workspace.

The path check runs before the tool lists and `allowedTools`, so an allowed tool does not bypass
a path fact. ADR 012 owns the grant's directory rule and the grant mounts.

There are no path deny rules or glob patterns.

### D4. The visible changes

Reads outside the facts now ask under `read-only` and `workspace-write`, where today they never
ask. A non-interactive run turns the prompt into the ordinary denial with exit code 3. The same
release notes state the write tightening: a write to a system root or a session-owned artifact
is now denied under `workspace-write`, where it used to ask. Both changes are tracked in #691.

## Consequences

- `Permission.md`, `session-files.md` §11.1/§11.2, `Attachments.md`, the CLI permission and
  command tables, and their Chinese mirrors describe the fixed facts, the pre-admission read
  resolution and the read change. ADR 012's “outside the facts asks” is this decision's D2; 012
  cites it.
- HostPlatform owns the fixed platform-roots list used by both the facts service and the sandbox
  builder; `SessionFiles.owns` reads both ownership tables, including command output.
- AGENTS.md rule 11's “file-root authorization” clause is superseded: workspace, system-root,
  session-owned and grant facts now decide access.
- No Timeline event or wire field is added; the facts service is Session configuration.
- ADR 012, 013 and 014 land together; the mounts are owned by ADR 010 (sandbox) and ADR 013
  (artifacts).

## Non-goals

- A path-pattern engine or path deny rules.
- Changing the sandbox mounts; ADR 010 and ADR 013 decide them.
- Cross-session grants.

## Supersession

Supersedes ADR 010's “file access is otherwise unchanged”, ADR 009's “file-root authorization is
unchanged”, ADR 003 D5's file-root sentence, `session-files.md` §11.1's “Permission, Approval and
WorkspaceTools stay unchanged”, and AGENTS.md rule 11's exact sentence “File-root authorization,
tool permission presets and replay never confer authority.” §11.2's two properties are kept. It
extends ADR 010 and depends on ADRs 012 and 013. ADR 003's file is not edited.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `Permission` remains the branch Capability; the facts service is Session configuration; tool owners keep execution. |
| §4 rule 1 | The facts service is typed configuration; no command event. |
| §4 rule 2 | The admitted call is unchanged. |
| §4 rule 3 | Read and write resolution happen before admission; no `before` callback. |
| §4 rule 4 | No core mechanism change. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | `Permission` reads the facts service; no other plugin state. |
| §4 rule 8 | No core mechanism change: HostPlatform exposes the fixed platform-roots list as capability state, and the file tool owners resolve paths before admission. |
| §4 rule 9 | No new Timeline fact. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Facts are Session-scoped; `Permission` depends on them. |
| ADR 002 D4 | Business configuration, read at use time. |
| ADR 002 D5 | The facts service key `fileAccessFacts`, its `facts(session_id, preset, path, mode)` query and the `FileFacts` result are the language-neutral contract; grants and ownership carry paths/modes. |
