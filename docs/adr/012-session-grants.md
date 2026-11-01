# 012: Session grants

Decision status: Proposed.

Extends the file-access facts of ADR 014 (sibling PR). ADR 010 leaves file access unchanged; this
decision adds the persisted “for this session” answer that ADR 014's ask needs. ADR 003 is
unchanged.

## Context

ADR 014 makes a file operation outside the facts ask. A session can be interrupted and resumed,
and the user should not answer the same path again. A grant is a user action; a model answering a
member's approval is not a user.

## Decision

### D1. The grant and its storage

The `SessionGrants` Session part owns the service and registers `session.grants.add` and `remove`
in the session method namespace. SessionStore owns the native
`session_grants` table, keyed `(session_id, path)`. A row is `{session_id, path, kind, mode}`:
`path` is canonical, `kind` is `file` or `dir`, `mode` is `read` or `write`, and a write grant
also permits reads.

`add(session_id, path, kind, mode)` upserts. A `read` add never lowers an existing `write`; a
`write` add raises it. `remove(session_id, path)` drops both modes. The method owner resolves `~`,
relative paths, `..` and symlinks, and rejects an unresolvable path. A file that does not exist
yet is resolved by its parent and final name and stored with `kind: file`; the caller passes the
kind it is approving.

A grant applies only to its session, survives a restore (it is stored configuration read at use
time), and is removed with the session in the same delete transaction. A fork is a new session
and does not inherit grants.

The table is part of a native storage format version after the one #671 takes; the number is
claimed in #596 by the implementation. Its upgrade creates the table empty, and a failed upgrade
rolls back. `session-deletion-and-fork.md` lists it in the delete transaction so a
damaged or unloaded session is still cleaned.

### D2. Permission applies the grant

The grant is a `Permission` fact, not an `Approval` decision. `Permission` reads the Session's
grants through `SessionGrants` and applies a matching grant only at the `Ask` step; it never
turns a `Deny` into `Allow`, and a write grant is inert under `read-only`. A grant read failure
denies. `Approval` remains the one admission point and re-checks through `Permission` after the
user answers. At process start, a write grant is mounted writable and a read grant read-only.

The client calls `add(session_id, path, kind, mode)` and then answers allow; the re-check sees
the grant. If `add` fails, the client answers allow-once. `ApprovalDecision` stays `Allow | Deny`.

### D3. Who may write

Any client that may call the session's methods may write a grant, including a paired remote
client. This decision does not add an identity tier; a deployment that needs one adds it at the
connection layer.

A leader model answering a member's approval never writes a grant, and a member session does not
inherit the leader's grants. No tool exposes `add` or `remove`; the approval-answer path has no
grant field.

### D4. Matching

A `file` grant matches only its exact path; a `dir` grant matches the path and its children by
component. A directory operation is decided on the directory path; `grep`/`glob` over a
directory use that directory. Approving a directory writes `kind: dir`. The grant is on the
canonical path, not the inode: a replacement file at the same path inherits it.
Read paths are resolved before admission by ADR 014, so `read`, `grep` and `glob` match the same
canonical target as `write`.

### D5. Full access

The trust consequence is stated in ADR 011: a model that can write the store can edit this table
too. It is not a property of this decision.

## Consequences

- A native `session_grants` table, the `SessionGrants` part, `add`/`remove` and a
  native format upgrade.
- `native-storage-format.md`, `session-deletion-and-fork.md`, `session-boundary.md`,
  `session-files.md`, `overview.md` B.1/B.2, `Permission.md` and the CLI tables are updated under
  their owners after acceptance.
- No Timeline event or approval wire field is added; `ApprovalDecision` is unchanged.

## Non-goals

- Cross-session or user-scoped grants.
- Model-written grants.
- An identity tier; that belongs to the connection layer.
- A path-pattern engine.

## Supersession

Extends ADR 014's facts and adds the grant store. It supersedes ADR 009 D3's “No new … stored
grant” and its Consequences bullet “No … persisted grant is added”. ADR 010's “file access is
unchanged” is qualified by ADR 014, not this decision. ADR 012, 013 and 014 land together; ADR 003
remains as written.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `Permission` owns the grant fact; `Approval` remains the branch Capability; `SessionGrants` is the Session part and SessionStore owns the table. |
| §4 rule 1 | Grants are configuration with typed owner operations in `session.grants`; no command event. |
| §4 rule 2 | A grant changes the next decision; this rule does not apply to stored configuration. |
| §4 rule 3 | No `before` callback. |
| §4 rule 4 | No core mechanism change. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | `Permission` reads the `SessionGrants` part; `Approval` does not write the table. |
| §4 rule 8 | No new infrastructure; the native table is a storage format change owned by SessionStore. |
| §4 rule 9 | No new Timeline fact; grants stay outside the Timeline. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Grants are Session-scoped; `Permission` depends on the service. |
| ADR 002 D4 | Business configuration, read at use time. |
| ADR 002 D5 | `session.grants.add/remove` are language-neutral contracts carrying paths, kinds and modes. |
