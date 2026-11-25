# 013: Command output ownership

Decision status: Proposed.

Extends ADR 010 and the session-file ownership model in
[`session-files.md`](../design/appserver/session-files.md) §11.1. This decision makes command
output, offloaded `read` results and background logs session-owned artifacts, so copied history
can read them after the source session is deleted. ADR 003 is unchanged.

## Context

Two producers hand host output paths to the model. WorkspaceTools writes under
`<database dir>/command-output/<session>/`: `exec` logs and offloaded `read` results. The
persistent shell writes under `<HostPlatform.home>/.dim/exec/<session>/<branch>/`. Neither has a
`session_files` row or id; a log can grow while a command runs; delete removes the directory. A
fork's copied history quotes those paths, so ownership has to follow a row, not the source
directory.

## Decision

### D1. Two tables, one fact

Attachments keep `session_file_owners (session_id, file_id)`. Output uses
`session_output_owners (session_id, path)`. Both feed the `session-owned` fact defined by ADR 014;
the fact reads both tables.

Two keys are deliberate. Output has no `session_files` row or id, is appended while running and
is removed as a directory. A single polymorphic table would need a nullable key and a
discriminator and would filter every sweep by kind. The second table exists so a fork can read
output after the source is deleted; without that requirement the current directory rule is
cheaper. An index on `path` supports the per-directory delete check; the primary key
`(session_id, path)` does not.

### D2. Registration

WorkspaceTools and the persistent shell both call `register(session_id, path)` before they return
the tool result that hands the path to the model. If registration fails, the call fails; a path
with no row is unowned. Each checks only its own root: `<database dir>/command-output/<session>/`
for WorkspaceTools, `<HostPlatform.home>/.dim/exec/<session>/<branch>/` for the shell. It guards
misuse among trusted parts, not isolation.

`exec` output, offloaded `read` results, a running background log and shell output all register.
A log may grow after registration; ownership is stable.

### D3. Queries and escapes

`owns(session_id, path)` resolves the path, then:

- an attachment requires the path to stay inside
  `<db>.session-files/<fileId>/` for an owned file id;
- output requires the exact registered path.

`..`, symlinks and an unresolvable path are denied. `owned_paths(session_id)` queries the rows
and returns the attachment path from its file-id directory and the output path from its row; it
is not one primary-key read. A query failure denies.

### D4. Fork, delete and growth

A fork copies every current row in both tables at the fork's commit, including a row registered
by a command that was still running at the fork point. The fork owns those files; a file the
source registers later is not owned.

Delete treats the deleting session's own `command-output/<S>/` as a candidate whenever this
database has rows for the session, plus every directory named by its rows, including inherited
`command-output/<parent>/` or `.dim/exec/<parent>/<branch>/` paths copied by a fork. A candidate
is removed only if no row of another session in this database names a path under it. The own
directory candidate covers output written before the upgrade and a file whose `register` failed;
neither has a row. When S is deleted, F's rows keep S's
directory; when F is deleted, F's copied rows name S's directory, so F's delete removes it. A
directory shared with another database is that database's responsibility; this guard is scoped
to this database and the cross-database case is accepted.

The session-files owner supplies the paths and the last-owner answer; HostPlatform removes the
`.dim/exec` directory it owns and makes the removal durable (the parent is synced) before the row
commit. HostPlatform does not read ownership rows. Keeping the removal before the row commit
preserves the crash argument in `session-deletion-and-fork.md`: a leftover directory in a shared
location cannot be found later.

The new table is the native format step after #671, shared with or following ADR 012's step; the
number is left to the format decision. Its upgrade creates the table empty and rolls back on
failure. There is no
backfill: output written before the upgrade is unowned, a resumed session may ask once, and a
fork made after the upgrade inherits no output rows.

Command output may grow after a fork. The fork owns the file and reads its current content; a
point-in-time copy is a separate concern.

### D5. Process start

A process start asks the ownership owner for the session's artifact paths. Workspace and platform
roots are ADR 010's mounts; grants are ADR 012's mounts. This decision owns the artifact mounts:
an owned path inside any writable root of that start — the workspace, a write grant, or the
persistent shell's own `.dim/exec/<S>/<branch>/` — is not mounted; `exec` passes no output root.
Every remaining owned file is one read-only bind, so a long session has one bind per file.
Inherited output outside the writable roots is mounted read-only. A running persistent shell
keeps the mounts it started with.

## Consequences

- A new `session_output_owners` table, a native format version and `register`, `owns`,
  `owned_paths`.
- `native-storage-format.md`, `session-deletion-and-fork.md`, `session-files.md` §11, the
  Attachments, WorkspaceTools and shell-session designs and the CLI tables are updated under
  their owners after acceptance. The table has an index on `path`; `.dim/exec` is removed by
  HostPlatform under the same last-owner rule.
- No Timeline event is added; the table is storage state. The decision reverses the S5c rejection
  of keeping a source's output for its forks, as recorded in `session-deletion-and-fork.md`.

## Supersession

Extends ADR 010 and uses ADR 014's `session-owned` fact; ADR 012 owns the grant mounts. It
reverses the S5c decision in `session-deletion-and-fork.md` that removed an output directory
unconditionally. ADR 003 remains as written.

## Non-goals

- A new file-access rule; output uses ADR 014's `session-owned` fact.
- Copying output at fork; growth is a separate concern.
- Registering paths outside the caller's output directory.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Ownership stays in the session-files owner; WorkspaceTools and the persistent shell keep producing output. |
| §4 rule 1 | Registration is a typed owner operation; no command event. |
| §4 rule 2 | The model reads the registered path; no call handover changes. |
| §4 rule 3 | No `before` callback. |
| §4 rule 4 | No core mechanism change. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | Both producers call the session-files owner; no other plugin state. |
| §4 rule 8 | No infrastructure change; the native table is a storage format version owned by SessionStore. |
| §4 rule 9 | No new Timeline fact. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Ownership is session-files state; file tools and process start read it. |
| ADR 002 D4 | Business state in the native store, read at use time. |
| ADR 002 D5 | `register`, `owns` and `owned_paths` are language-neutral contracts. |
