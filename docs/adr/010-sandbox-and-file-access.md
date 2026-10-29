# 010: The sandbox is opt-in; file access uses session ownership

Decision status: Proposed.

Extends [ADR 009](009-permission-decides.md): the permission decision is final and HostPlatform
enforces it. This decision replaces the default-on sandbox policy of the
[F5 isolation design](../design/host-platform-isolation.md), its policy file and management
methods, and the `sandbox` argument. ADR 003 is unchanged.

## Context

The current defaults — sandbox on, network off, readable workspace and system directories — are
wrong for a local coding agent. A normal task needs the user's toolchains in the home directory,
the package managers' network and the files the user points at. A release without a policy file
gets those restrictive defaults and the agent cannot do ordinary work.

Main also carries `host.isolation.get/update/reset`, `isolation_policy.rs`,
`host-isolation.json` and the `/settings` Command isolation row (added after this branch was
cut). Those methods exist to manage the wrong policy file. This decision removes them rather
than replacing them with another node policy.

Attachments and command output are host-produced files outside the workspace. Their access is
owned by the session, not decided by their location. A session can be briefly interrupted and
resumed, so a grant made for it must survive the interruption.

## Decision

### D1. The sandbox is a client application setting, off by default

The client application owns two switches: `sandbox` and, when the sandbox is on, `network`. Both
default to off. They are part of the application configuration; the daemon reads them when it
starts a Session and when a process starts. A running process keeps its environment.

- Off: `exec` and the persistent shell run on the host with the user's environment, home and
  network.
- On: they run in the workspace baseline plus the platform's system roots; network follows the
  setting. The existing platform defaults remain, including the persistent shell output
  directory and, on macOS, each process tree's temporary directory and git's global
  configuration.

There is no node policy file, no `host-platform.json`, no management method and no revision.
Changing the setting applies to the next process or Session. An existing `host-isolation.json`
is not read. `host.isolation.get/update/reset`, `isolation_policy.rs`, `IsolationPolicy` and the
`/settings` Command isolation row are removed.

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
| system roots | HostPlatform's platform roots; read-only |
| session-owned | the session-files owner's ownership rows; read-only |
| user grant | the session's persisted grants; the grant's mode |
| preset | the branch's permission mode |

The facts are composed by a Session-scoped service. `Permission` reads the facts and does not
know about attachments, SessionStore or the sandbox. A new fact source is a design decision, not
a plugin hook.

### D4. Host artifacts are owned by the session

Attachments and command output use one ownership model:

- A host-produced file handed to the model is owned by the session. Attachments are owned when
  `sessionFiles.put` commits; command output is registered by its owner when the output is
  handed to the model.
- The owner exposes `register(session_id, path)`, which accepts only a path under the output
  directory the caller owns. It is not a general grant.
- Ownership is a row `(session_id, artifact)` in the native store, not a directory prefix. A
  path is readable when the session owns its artifact.
- A fork copies the source's ownership rows at the fork's commit. The fork owns those files; a
  file the source adds later is not owned by the fork. The file path stays stable.
- Deleting a session releases its rows; a file another session still owns remains.
- Command output may grow after a fork. The fork owns the file and reads its current content; a
  point-in-time copy is a separate concern.

The session-files owner exposes `owns(session_id, path)` for a single decision and
`owned_paths(session_id)` for a process start. Both are primary-key lookups; `owned_paths`
derives each path from the row plus the node's file directory. A query failure denies the
operation.

### D5. Read and write use one table

| Mode | Inside the facts | Outside the facts |
| --- | --- | --- |
| `read-only` | reads allowed | reads ask; writes, processes and network denied |
| `workspace-write` | reads allowed; writes only in the workspace and write grants | reads and writes ask |
| `full-access` | allowed | reads and writes allowed without a prompt |

- System roots and session-owned artifacts are read-only to the model. Only the workspace and a
  write grant are writable.
- A write grant also permits reads; a read grant never permits writes.
- `full-access` admits an outside operation without a grant. The file tools run in process; the
  process sandbox is unchanged. It is a trust decision, not a sandbox boundary: a full-access
  model can write the application setting, the grant table or a shell rc file, and the sandbox
  is not a defense against it. A user who wants a boundary does not use `full-access`.
- Command rules remain user-scoped and govern `exec` commands. There are no path deny rules or
  glob patterns.

### D6. Session grants are user actions

A grant is `{ session_id, path, mode }`, stored in the native store with the session. A path
may be exact or a directory prefix; approving a directory writes a prefix. It applies to that
session only, survives a restore, and is removed when the session is deleted. A fork is a new
session and does not inherit grants.

Grant writes come from a user client. Local and remote clients are treated the same; the
connection or identity layer decides whether that user may act on the session. A model is not a
user: an approval answered by a leader model for a member never writes a grant, and a member
session does not inherit the leader's grants. `ApprovalDecision` stays `Allow | Deny`; the client
writes the grant and then answers allow.

### D7. Process start uses the session's ownership

When the sandbox is on, a process start asks the ownership owner for the session's artifact
paths and mounts them read-only, together with the workspace, the platform roots and the
session's grants. A running persistent shell keeps the mounts it started with; a file handed
after it started is read through the file tools. A grant made after the shell started applies to
new processes, not to that shell.

### D8. `full-access` is not a sandbox setting

`full-access` means “no tool prompts”. It does not turn the sandbox off, does not add grants and
does not change the network setting. To run all model-started processes on the host, the user
turns the sandbox off in the client application setting.

## Consequences

- The release works without a policy file: the sandbox is off and the agent uses the user's
  environment. Isolation is opt-in.
- `host.isolation.get/update/reset`, `isolation_policy.rs`, `IsolationPolicy`,
  `host-isolation.json` and the `/settings` Command isolation row are removed. No migration is
  kept; an existing file is not read.
- `exec` loses `sandbox`; the node policy file, `escalation` and the tier field are removed.
- The session-files owner gains `owns`, `owned_paths` and `register`; command output joins the
  same ownership model. A Session-scoped facts service composes the fixed sources for
  `Permission`.
- Session grants gain a native table and a Session method. No Timeline event or approval wire
  field is added; `ApprovalDecision` is unchanged. The sandbox switches are application
  configuration, not a management RPC.
- `Permission.md`, `WorkspaceTools.md`, `HostPlatform.md`, `session-files.md` §11, the
  Attachments design, the `host.isolation` method document and the CLI permission/isolation
  tables are updated under their owners after acceptance. ADR 003 is not edited.
- AGENTS.md rule 11's “permission presets” clause is updated by ADR 009 and its “file-root
  authorization” clause by this decision.
- The path facts are exact/prefix for grants and ownership rows for host artifacts. The platform
  roots and workspace are directory roots.

## Non-goals

- A policy file, `host-platform.json`, `host-isolation.json` or any node configuration.
- Globs or a path-pattern engine.
- Cross-session or user-scoped grants.
- Agent-initiated resource requests or per-call sandbox/network expansion.
- A remote node's policy ceiling.
- Migration from the old session-file layout, the old isolation file or old grants.

## Supersession

Replaces the default-on sandbox policy, the policy file and the management methods in the
[F5 isolation design](../design/host-platform-isolation.md). It also supersedes that design's
trusted-local rule for session grants: a grant is a user action, the identity layer decides
which clients may act on a session, and a model never writes one. It extends
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
