# 010: The sandbox is opt-in; file access uses session ownership

Decision status: Proposed.

Extends [ADR 009](009-permission-decides.md): the permission decision is final and HostPlatform
enforces it. This decision replaces the default-on sandbox policy of the
[F5 isolation design](../design/host-platform-isolation.md), the `sandbox` argument and the node
policy file. ADR 003 is unchanged.

## Context

The current defaults — sandbox on, network off, readable workspace and system directories — are
wrong for a local coding agent. A normal task needs the user's toolchains in the home directory,
the package managers' network and the files the user points at. A release without a policy file
gets those restrictive defaults and the agent cannot do ordinary work. The policy file was the
only escape, and users are not expected to create one.

Attachments and command output are host-produced files outside the workspace. Their access is
owned by the session, not decided by their location. A session can also be briefly interrupted
and resumed, so a user grant made for it must survive the interruption.

## Decision

### D1. The sandbox is an application setting, off by default

The application setting has two switches: sandbox on/off and, when on, network on/off. Both are
read when a process starts; a running process keeps its environment.

- Off: `exec` and the persistent shell run on the host with the user's environment, home and
  network.
- On: they run in the workspace baseline plus the platform's system roots; network follows the
  setting.

The defaults live in code. There is no policy file and no `host-platform.json`; a release
without any setting runs unsandboxed. The application setting is optional.

### D2. `exec` does not choose the environment or declare resources

`exec` has no `sandbox`, `paths` or `network` input. It runs with the application setting and
the session's access facts (D3). The model cannot widen the sandbox by calling a tool. A command
that hits a missing path fails as an ordinary command error; the user grants the path or turns
the sandbox off, then the model may retry.

### D3. File access facts come from fixed sources

`Permission` decides a file operation from one set of facts:

| Fact | Source |
| --- | --- |
| workspace | `SessionConfig` |
| system root | HostPlatform's platform roots |
| session-owned | the session-files owner's ownership rows |
| user grant | the session's persisted grants |
| preset | the branch's permission mode |

The facts are composed by a Session-scoped service. `Permission` reads the facts and does not
know about attachments, SessionStore or the sandbox. A new fact source is a design decision, not
a plugin hook.

### D4. Host artifacts are owned by the session

Attachments and command output use one ownership model:

- A host-produced file handed to the model is owned by the session. Attachments are owned when
  `sessionFiles.put` commits; command output is registered by its owner when the output is
  handed to the model.
- Ownership is a row `(session_id, artifact)` in the native store, not a directory prefix. A
  path is readable when the session owns its artifact.
- A fork copies the source's ownership rows at the fork's commit. The fork owns those files; a
  file the source adds later is not owned by the fork. The file path stays stable.
- Deleting a session releases its rows; a file another session still owns remains.
- Command output may grow after a fork. The fork owns the file and reads its current content; a
  point-in-time copy is a separate concern.

The session-files owner exposes `owns(session_id, path)` for a single decision and
`owned_paths(session_id)` for a process start. Both are primary-key lookups. The query failure
denies the operation.

### D5. Read and write use one table

| Mode | Inside the facts | Outside the facts |
| --- | --- | --- |
| `read-only` | reads allowed | reads ask; writes, processes and network denied |
| `workspace-write` | reads and writes allowed | reads and writes ask |
| `full-access` | allowed | reads and writes allowed without a prompt |

- A write grant also permits reads; a read grant never permits writes.
- Session-owned artifacts are read-only to the model.
- `full-access` admits an outside operation without adding a grant. The file tools run in
  process; the process sandbox is unchanged.
- Command rules remain user-scoped and govern `exec` commands. There are no path deny rules or
  glob patterns.

### D6. Session grants are persisted

A grant is `{ session_id, path, mode }`, stored in the native store with the session. It applies
to that session only, survives a restore, and is removed when the session is deleted. A fork is
a new session and does not inherit grants. The client writes the grant through a Session method
and then answers the approval allow, as it does for a command rule. `ApprovalDecision` stays
`Allow | Deny`.

### D7. Process start uses the session's ownership

When the sandbox is on, a process start asks the ownership owner for the session's artifact
paths and mounts them read-only, together with the workspace, the platform roots and the
session's grants. A running persistent shell keeps the mounts it started with; a file handed
after it started is read through the file tools.

### D8. `full-access` is not a sandbox setting

`full-access` means “no tool prompts”. It does not turn the sandbox off, does not add grants and
does not change the network setting. To run all model-started processes on the host, the user
turns the sandbox off in the application setting.

## Consequences

- The release works without a policy file: the sandbox is off and the agent uses the user's
  environment. Isolation is opt-in.
- `exec` loses `sandbox`; the node policy file, `escalation` and the tier field are removed. No
  migration is kept.
- The session-files owner gains `owns` and `owned_paths`; command output joins the same ownership
  model. A Session-scoped facts service composes the fixed sources for `Permission`.
- Session grants gain a native table and a Session method. No Timeline event or wire field is
  added; `ApprovalDecision` is unchanged.
- `Permission.md`, `WorkspaceTools.md`, `HostPlatform.md`, `session-files.md` §11, the
  Attachments design and the CLI permission/isolation tables are updated under their owners
  after acceptance. ADR 003 is not edited.
- The path facts are exact/prefix for grants and ownership rows for host artifacts. The platform
  roots and workspace are directory roots.

## Non-goals

- A policy file, `host-platform.json`, or any required node configuration.
- Globs or a path-pattern engine.
- Cross-session or user-scoped grants.
- Agent-initiated resource requests or per-call sandbox/network expansion.
- A remote node's policy ceiling.
- Migration from the old session-file layout, the old isolation file or old grants.

## Supersession

Replaces the default-on sandbox policy and the tier and escalation fields in the
[F5 isolation design](../design/host-platform-isolation.md). It extends
[ADR 009](009-permission-decides.md); ADR 003 remains as written.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `Permission` and `Approval` remain branch Capabilities; the facts service and session grants are Session configuration; `ToolExecutor` and `ToolBinding` remain branch parts; tool owners keep execution; `HostPlatform` remains a process part. |
| §4 rule 1 | Grants are configuration with typed owner operations; the session-owned fact is read from its owner. No command event is introduced. |
| §4 rule 2 | The admitted call is handed to execution once; a grant applies to the next call. |
| §4 rule 3 | No `before` callback is added; write resolution keeps its existing pre-admission, metadata-only contract. |
| §4 rule 4 | No core mechanism changes. |
| §4 rule 5 | No new Timeline batch or ordering. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | Parts interact through existing services and the call input. |
| §4 rule 8 | No infrastructure change; no requirement is raised to the runtime. |
| §4 rule 9 | No new Timeline fact; grants and facts stay outside the Timeline. |
| ADR 002 D2 | No wiring plugin or adapter. |
| ADR 002 D3 | The facts service is Session-scoped; `Permission` and the tool owners depend on it; `HostPlatform` receives the process portion from the caller. |
| ADR 002 D4 | The application setting and session grants are business configuration; facts are read from their owners at use time. |
| ADR 002 D5 | The facts and grant Session methods are language-neutral contracts; they carry paths and modes, not event fields. |
