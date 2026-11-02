# SessionStore Session deletion, cleanup and fork

Status: Implemented. Roadmap item S5c ([#61](https://github.com/arcships/dim/issues/61),
[#97](https://github.com/arcships/dim/pull/97)), after S5a ([#59](https://github.com/arcships/dim/issues/59)) and its
design, [Session files](session-files.md), with [#127](https://github.com/arcships/dim/issues/127) (Collaboration).
§16 lists what the implementation changed.
Overview: [Appendix B.2](../overview.md#b2-app-server-process-plugins), the Rpc, Agents and SessionStore rows. Scope:
[ADR 002](../../adr/002-plugin-scope.md) process scope, inside the existing Agents and SessionStore plugins, with two
Rpc built-ins.
Consumers: clients through `session.delete` and `session.fork`, and the CLI's `dimcode session` commands (F6,
[#77](https://github.com/arcships/dim/issues/77)).

## 1. Summary

A client deletes a Session with `session.delete` and forks one with `session.fork`. Both are Rpc built-ins of
protocol v3. They call Agents, which owns loading, closing and creating Sessions; SessionStore performs the row and
file steps.

- **Delete.** Agents stops loading the Session and closes it if it is loaded, so its capabilities settle their work
  and release what they hold. SessionStore then removes the Session's command output directory, and in one
  transaction every row the store keeps for it, including its Session-file owner rows. S5a's sweep removes the files
  that lost their last owner. Deleting an absent Session succeeds.
- **Cleanup.** One delete removes everything the Session owns (§3.3). Cleanup by age or count, with a preview, is the
  CLI composing `session.summaries` and `session.delete`; there is no server method for it.
- **Crash safety.** The directory removal is durable before the row transaction commits, and Session files go by
  S5a's rows. A delete interrupted anywhere leaves either the whole Session or none of its rows, and a retry or the
  next sweep finishes it (§4).
- **Fork.** A fork copies the source's `main` through a cut into a new Session in one transaction, byte for byte, and
  shares the source's Session files. The cut is a rest point of `main`, and branches `main` created by then are
  copied without their events, so the fork starts no work until a client sends it input (§6.5). The fork's `main`
  opens with `branch/resume`: the fork is the source's conversation restored at the cut under a new id. Right after
  the copied facts the fork writes one fact of its own, `session/forked`, so an owner that folds a branch's own
  history knows where the copy ends (§6.5); v3 connections deliver it and v2 keeps its list.
  `SessionConfig` is copied whole, so `interactive` and the permission preset carry over and nothing is widened.
- **Format.** S5c changes no format. The new event row was added at v16 as a development-phase extension that daemons
  built before it cannot read, and format v17 keeps it as it is (§8). No provenance column and no tombstone.

One event kind, `session/forked` (empty data), and no change to the Timeline, Dispatcher, TimelineTree, Prompt, rutis
or the Session facade.

## 2. Structure

```
 Client -- session.delete {sessionId} ----------+
        -- session.fork {sessionId, at} --------+
                                                v
 Process scope (control node)          Rpc built-ins (v3): shape, protocol version
                                                |
                                                v
                         Agents (AgentIndex): per-Session admission gate,
                         close, creation from another Session's cut
                           |                                 |
                           | delete_session(S)               | prepared creation,
                           |                                 | prepare step = fork_session(S, F, at)
                           v                                 v
                         SessionStore ---- one transaction each ----> SqliteStorage
                           |      \
                           |       +-- S5a sweep (Session files)
                           | per call, by the Session's resource node (#38)
                           v
 Resource node (P1/P2: the control node)     FileStorage: remove(command-output/<S>)
```

Rpc checks the params and the Session's protocol version and calls Agents. Agents owns the Session lifecycle: the
admission gate `acquire` already holds while it restores a Session, `Session::close`, and prepared creation
(`create_prepared_session`). SessionStore owns the rows and the files under the database directory. Its new steps
are Rust methods over the native store, not services: only Agents calls them.

## 3. Deletion

### 3.1 Method

`session.delete {sessionId: string} -> {}`

| Item | Rule |
| --- | --- |
| Kind | Rpc built-in, next to `session.create` and `session.close`. It is not a `sessionMethods` entry, because a Session cannot unload itself through one of its own parts. It is not a management method either: it acts on one Session, and Rpc's Session checks apply |
| Protocol | v3. v1 and v2 get `unknown_method` ("Method requires protocol v3"). `hello.features` lists it under the conditions that list `session.create`. The protocol version stays 3: clients discover new built-ins through `hello.features`, as they discover `session.summaries` |
| Callers | Every connection that may call `session.close`, under the same profile rules: in a protected profile, local transports only. Paired devices (P3) are decided in RemoteGateway's compatibility table together with the other Session built-ins |
| Session version | A Session's recorded protocol version must equal the connection's (`session_protocol_version`), as for every Session built-in. A Session with no readable owner row has no recorded version, so the check passes |
| Damaged Sessions | Deleted by id. A delete reads none of the Session's records, so a Session that fails validation or has no available owner can be deleted; Rpc's unavailable-Session check does not apply to `session.delete` |
| Reply | `{}` once the Session's rows are gone. An unknown or already deleted id also replies `{}` |

### 3.2 Sequence

```
 Rpc          Agents                        Session S            SessionStore              FileStorage
  | delete(S) ->|                              |                     |                          |
  |             | 1 take S's admission gate    |                     |                          |
  |             | 2 loaded? close() ---------->| settle accepted work, write terminal facts,
  |             |                              | release processes, leases, member requests;
  |             |<-------------- closed -------|                      |                          |
  |             | delete_session(S) ------------------------------->|                          |
  |             |                              |  3 remove command-output/S, parent synced ---->|
  |             |                              |    absent counts as removed <------------------|
  |             |                              |  4 txn on the Timeline writer: owner rows,     |
  |             |                              |    Timeline rows, owner, record -> commit      |
  |             |                              |  5 schedule the S5a sweep                      |
  |             | 6 forget S; drop its slot                          |                          |
  |<---- {} ----|                              |                     |                          |
```

1. **Admission.** Agents checks the id (D3) and takes S's admission gate, the `SessionSlot` gate that `acquire` holds
   while it restores and a creation holds until the Session it builds is published (§7), so no request,
   subscription or fork can load S while the delete runs, and a delete of a Session still building waits for it. A
   request that already holds a use of S keeps it, and the close in step 2 ends its work.
2. **Close.** If S is loaded, Agents closes it as `session.close` does: a business close, not an unload. Its
   capabilities settle their accepted work, write their terminal facts and release processes, FileStorage leases and
   member requests. A close that reports cleanup errors is logged and the delete continues: the facts it could not
   write are removed with the rest. From the close on, S's slot is closed: S does not load again in this process,
   whether or not its rows go (step 6).
3. **Command output.** SessionStore removes the Session's output directories before any row changes
   (ADR 013 D4): its own `<database directory>/command-output/<S>/`, and the directory of every
   path it has a row for, so inherited `command-output/<parent>/` and
   `.dim/exec/<parent>/<branch>/` directories a fork copied are covered. A directory is removed
   only if no row of another Session in this database names a path under it. SessionStore
   serializes this removal with step 4's row deletion across Sessions with its ownership lock:
   two concurrent deletes could otherwise both see the other's rows, both keep the shared
   directory, and leave it with no owner after both commits. The fork step takes the same lock
   while it copies every current output row; the lock, not a file-existence check, keeps fork and
   delete consistent, so the row a running command registered before writing its file is copied
   and F owns the file the command creates. `command-output`
   directories go through the FileStorage of S's resource node, looked up per call as in S5a;
   HostPlatform removes the `.dim/exec` directories it owns. HostPlatform does not read ownership
   rows. An absent directory counts as removed. Each removal must be durable before step 4 (§4).
   A failure ends the delete before any row changes (§3.5). SessionStore removes its own
   `command-output/<S>/` only while its database has rows of S: `command-output/` is shared by
   every database in the directory, so the directory of an id without rows here may belong to
   another database's Session (§4).
4. **Rows.** One `IMMEDIATE` transaction removes every row of S: `session_file_owners`, `session_output_owners`, `timeline_snapshots`,
   `timeline_events`, `timeline_branches`, `timeline_content`, `session_grants`, `session_owners`, `session_catalog` and
   `agent_sessions`, in this order because of the foreign keys, plus S5b's title metadata (§14). `session_catalog` is
   the derived row of [#556's catalog projection](native-storage-format.md#556-v23-格式决定). This is the operation a
   creation rollback runs today ([timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs)
   `rollback_creation`), extended by S5a with the owner rows; delete and rollback share it. It runs on the Timeline
   writer, as a rollback does, so it follows every append already queued for S and clears S's recorded write failure.
5. **Sweep.** SessionStore schedules S5a's sweep ([Session files §5.4](session-files.md#54-sweep)). Files whose last
   owner was S are removed there; files a fork still owns stay.
6. **Forget.** Agents removes S's protocol version and unavailable-Session entry, marks S's slot closed and deleted,
   and drops the slot once nothing else holds it ([idle unload §6.4](idle-unload.md#64-admission-slots)). A caller
   still waiting at the gate finds the slot closed, and a later one finds S with no owner and no rows: `acquire`
   answers `session_closed` either way. A delete that fails before step 4 releases the gate instead. A Session it
   closed stays as `session.close` leaves it: its rows remain, `session.list` omits it, it does not load again in
   this process, and a retry deletes it; after a restart it loads. A Session it did not close loads on its next use
   (§16 D5).

### 3.3 What a delete removes

| State | Where | Removed by |
| --- | --- | --- |
| Session record, owner, branches, events, content, snapshots | Native store | Step 4, one transaction |
| Session-file ownership | `session_file_owners` (S5a) | Step 4. Files left without an owner go in S5a's sweep |
| Title metadata | Native store, S5b's `session_metadata` row ([title metadata](session-title-metadata.md#9-lifecycle)) | Step 4, same transaction |
| File grants | Native store, `session_grants` rows (v28, [ADR 012](../../adr/012-session-grants.md) D1) | Step 4, same transaction |
| Output ownership | Native store, `session_output_owners` rows (v29, [ADR 013](../../adr/013-command-output-ownership.md) D1) | Step 4, same transaction |
| Exec output, offloaded `read` results and persistent shell logs | `<database directory>/command-output/<S>/` and every directory named by S's output rows | Step 3. A directory another Session in this database still names is kept |
| Loaded instance, leases, subscriptions, in-flight uses | Process memory | Step 2 |
| Protocol version, admission slot, unavailable-Session entry | Agents, in memory | Step 6 |
| Collaboration relations that name S, the member Sessions S leads and the Agents S owns | Durable: `agent_sessions.leader_session_id` and `agents.owner_session_id` ([Collaboration §6](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)); Collaboration's relations are rebuilt from them | Agents deletes a leader with its Team: each member's own delete, in which SessionStore removes that Session's rows, then the Agent records Agents owns, then S. Collaboration drops a relation when it observes a member's close ([collaboration.rs](../../../crates/dim-app-server/src/collaboration.rs) `session_ended`) or the delete of an unloaded member, and rebuilds the rest when it loads ([idle unload §7](idle-unload.md#7-races-and-interactions)) |

A delete does not remove:

- **Other owners' records that name S**, such as Scheduler and Workflow run records in `pluginFiles`. When their
  owners use the id, they get `session_closed`, as for a closed Session.
- **Other Sessions.** A fork of S is a Session with its own rows. A deleted leader's members and the Agents it owns are
  its Team, and its delete ends them with it ([Collaboration §6](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)):
  SessionStore removes each member's rows and Agents removes the Agent records, each owning its own record.
- **Host configuration** (provider selections by workspace, MCP project enablement). It is not per Session.

### 3.4 Clients and subscriptions

- Connections attached to S get the existing `session.state` and `session.closed` notices when step 2 closes it, as
  for `session.close`. A notice carrying the reason "deleted" is not added: `session.closed` is sent before the row
  transaction commits, so it could announce a delete that then fails. The protocol has no list notification for a
  new Session either, so none is added for a removed one.
- After the reply, `session.attach`, Session methods and `session.fork` on S fail `session_closed`, and
  `session.list` and `session.summaries` omit it.
- The connection that sends the delete may itself be attached to S. It gets the notices, then the reply.

### 3.5 Idempotence and errors

Every step tolerates an absent target, so a delete can be retried after any failure or an uncertain reply.

| Result | When | State after |
| --- | --- | --- |
| `{}` | The rows were removed, or there were none | Session gone; its unowned files go in the sweep |
| `unknown_method` (-32601) | Protocol v1 or v2 | Unchanged |
| `invalid_params` (-32602) | `sessionId` missing or not a string | Unchanged |
| `session_protocol_version` | S belongs to another protocol version | Unchanged |
| `product_integration_pending` | The profile does not serve Session execution | Unchanged |
| `-32603` "Session operation failed" | Step 3 failed (`Locked`, `DirectoryNotEmpty`, `Unsynced`, I/O, node unavailable) or step 4 failed (SQLite) | If step 2 closed S, S is closed but not deleted, as after `session.close`: its rows remain, it is neither listed nor loaded again in this process, and the relations its close ended stay ended. After a restart it loads, possibly with part of its command output gone. A Session that was not loaded is unchanged. Retry |
| No reply | The connection or Rpc ended before the reply | Either state. Retry |

`Locked` and `DirectoryNotEmpty` in step 3 come only from writers outside the closed Session, such as a PowerShell
child that outlived its shell or another process holding a file open. The delete fails and is retried once they are
gone.

## 4. Crash convergence

```
            2 close            3 remove (synced)            4 commit              5 sweep
 [loaded] ----------> [closed] -------------------> [no dir] -----------> [no rows] -------> [gone]
    ^                    |                              |                     |
    |                    | crash: all rows,             | crash: all rows,    | crash: unowned file
    +--- next use -------+ part of the directory        | no directory        | rows (S5a)
         loads S           retry the delete               retry the delete      next sweep
```

| Interrupted | Store | Disk | Converges by |
| --- | --- | --- | --- |
| Before or during the close | All rows | The directory | Nothing lost; S loads on its next use; retry |
| During the removal | All rows | Part of the directory | Retry: removal continues and absent entries count as removed |
| After the removal, before the commit | All rows | No directory, durably | Retry. Meanwhile S loads, without its command output files |
| After the commit, before the reply | No rows | No directory; S's files unowned | The client's retry finds no rows, so it removes no directory, replies `{}` and schedules the sweep again; otherwise S5a's next trigger (another delete, or the first put of a later generation) removes the files |
| During the sweep | S5a's states | — | [Session files §6](session-files.md#6-deletion-crash-recovery-and-serialization) |

**Why the directory goes before the commit.** The Session's own `command-output/<S>/` directory has no row of its
own (only the files in it may). Once the rows are gone, an orphaned directory could only be found by listing
`command-output/`, and that directory is shared by every database in the database directory (two `--database` files
in one directory), so a listing cannot tell a deleted Session's directory from another store's. Removing it first
means no committed delete ever leaves one behind. A directory another Session in this database still names is kept
(ADR 013 D4): it is found again through that Session's rows, so the next delete removes it. S5a's files follow the
opposite order because their rows record them and the sweep can find them. FileStorage removal was not synced
([S6](https://github.com/arcships/dim/pull/87)): after a power loss a removed directory could reappear while the later
commit, made with `synchronous=FULL`, stays. `remove` now syncs the parent before it returns (§14), which closes that
gap; HostPlatform syncs the parent of a removed `.dim/exec` directory the same way.

**Only an id with rows here.** For the same reason a delete removes the directory only while its own database has
rows of the id. Two daemons on `a.sqlite` and `b.sqlite` in one directory share `command-output/`; a
`session.delete {S_b}` sent to the first, for example by a CLI given the wrong `--database`, replies `{}` and must not
remove the output of S_b, which is alive in the second. The check costs no convergence: the directory goes before the
rows, so a database with no rows of S has no directory of S left to remove, and Agents' admission gate keeps two
deletes of one id from interleaving. A retry after a commit therefore removes nothing, as it needs nothing removed.

Rejected alternatives:

| Alternative | Why not |
| --- | --- |
| Commit first, remove the directory after | A crash in between leaves a directory nothing names, and listing is unsafe in a shared database directory |
| A tombstone row for deleted ids awaiting cleanup | A format change (v16) for one directory, when the order above converges without it |
| Unload instead of close | An unload does not settle accepted work or stop member requests (AGENTS.md rule 8) |

## 5. Cleanup

### 5.1 Cleanup by age or count

The [feature list](../../feature-list.md) asks for cleanup "by age or retained count, with a preview". The CLI
composes it: `session.summaries` returns `createdAt`, `updatedAt` and a preview for every Session of the connection's
protocol version; the CLI selects, shows the selection, and calls `session.delete` for each id. A failed id is
reported and can be retried; the others are deleted.

Rejected: a server method such as `session.cleanup {olderThan, keep, dryRun}`. It repeats the selection and preview
the client already has, a retention policy would need a business owner and configuration that nothing defines, and a
partial failure would still need per-id results.

### 5.2 Orphans

| Kind | Handling |
| --- | --- |
| Session files | S5a's sweep: after each delete, retried ones included, and on the first put of each generation |
| Command output | None needed: removed before the commit (§4) |
| Rows of a deleted Session | None needed: one transaction |
| Rows left by earlier faults, such as Timeline rows with no record | A delete by that id removes them: every table is cleared by `session_id`, whether or not `agent_sessions` has the row |

## 6. Fork

### 6.1 Method

`session.fork {sessionId: string, at: u64} -> {sessionId: string, sequence: u64}`

| Item | Rule |
| --- | --- |
| Kind | Rpc built-in. It creates a Session, as `session.create` does, from another Session's cut (Agents) |
| Protocol and callers | As for `session.delete` (§3.1). The source must belong to the connection's protocol version, and the fork gets that version |
| `at` | The last `main` sequence the fork inherits. 0 inherits no event |
| Reply | The fork's id and its `main` head once it is published. The client then attaches |
| No other params | No `branchId` or configuration fields. The fork's configuration is the source's (§6.3). A client changes it afterwards with `session.permission.update` or `session.model.update`, as [#26](https://github.com/arcships/dim/issues/26) decided for the preset |

A fork is not idempotent: a retry after an uncertain reply can create a second fork, which the client finds in
`session.summaries`.

### 6.2 The cut

The cut is on `main` and follows `session.branch_create`'s rule
([tree.rs](../../../crates/dim-app-server/src/connection/tree.rs) `create_branch`): `at` is at most the published
`main` head and ends an atomic batch, and `history::validate_context_boundary` accepts the context at `at` (no tool
proposal without its result, no unfinished model output). Agents acquires the source to check it, restoring it if
needed, as `session.branch_create` does. Rows at or before `at` never change, so appends during the fork do not
affect the copy.

`at` must also be a **rest point** of `main`: no turn is open, every Queue or Steer input is consumed, and every
background task has ended, with its wake input, if it posts one, at or before `at` and consumed. A reload starts work
from the last two today: Loop starts a turn for an applied Queue or Steer input it has not consumed
([agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs) `rebuild`), InputQueue applies a Queue
input held behind a turn when that turn ends ([input_queue.rs](../../../crates/dim-agent-capabilities/src/input_queue.rs)),
and BackgroundExec ends a task it finds open as failed and wakes the branch with a Steer input
([background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) `rebuild`). In F those turns
would run in the source's `cwd` for work the source already did or is still doing. A turn open at the cut is only failed by the reload,
not run again; requiring none keeps F from opening with a failed turn the source never had. At a rest point F's reload
starts nothing. Append inputs may remain: they start no turn and join the next request a client's input starts in F. Agents checks it on `main`'s prefix through `at`,
right after `validate_context_boundary`, by reading those capabilities' facts (§14, §16 D1). The usual fork points, after a turn ends or before a user message, are rest
points unless a background task was running.

**Why `main` only.** A child branch's history is its ancestors' prefixes followed by its own events, and each branch
numbers its events from 1. Laying that out as one `main` would renumber events, and compaction `through`,
`branch/create.at`, `branch/start.createEvent`, batch ends and snapshot anchors refer to those numbers: the store
would rewrite facts other owners wrote. To continue a child branch's work, a client creates a branch from it inside
the Session.

Rejected: copying a child's ancestor chain under the same branch ids and continuing on the copy of the child. The
fork's conversation would then not be its `main`, which clients, `session.list` and SessionTitle read.

### 6.3 What a fork copies

```
 Source S                                                 Fork F (one transaction)
 main: 1 .. 7  8 branch/create(c1)  9 .. 12 | 13 ..       main: 1 .. 12, same ids     (at = 12, a rest point)
                      |                   cut                   then 13 session/forked; F's first load
 c1:   1 branch/start .. 30 (may still grow)                    writes 14 branch/resume, 15 branch/create-failed(c1)
 c2:   branch/create at main 15                          c1:   directory (stored info, head 0), no events
                                                         -     c2 not copied (created after the cut)
 timeline_content: rows the events reference             only the rows the copied events reference, with
                                                         the list nodes they reach
 timeline_snapshots: main, c1                            main's, anchored at or before 12
 session_file_owners: (S,f1) (S,f2)                      (F,f1) (F,f2), no byte copied
 agent_sessions.config: stored text                      the same text
 session_owners: (S, Agent A, v3)                        (F, Agent A, v3)
 session_catalog: derived row                            one row, as a backfill over F's events
 S5b title metadata                                      not copied
```

| Table | Rows F gets |
| --- | --- |
| `agent_sessions` | A new id (UUID v4), `created_at_ms` at the fork, and the source's `config` text unchanged. That is the whole `SessionConfig`, including `interactive` ([session access A.3](../contracts/session-access.md#a3-sessionconfig-fields)), `permissionPolicy`, `collaborationRole`, `cwd`, `model`, `systemPrompt` and `tools`. Copying the text instead of re-serializing it keeps `interactive`'s omission rule as written |
| `session_owners` | The source's Agent and protocol version |
| `session_catalog` | One row for F, with the values a backfill over F's events writes: `updated_at_ms` the `create_at` of the last event the fork transaction writes — the `session/forked` row at `at + 1` for a nonzero cut, F's creation time at a cut of `0` — and `preview` the first user input among the copied events under the same batch-complete window, NULL at a cut of `0` ([#556's catalog projection](native-storage-format.md#556-v23-格式决定)) |
| `timeline_branches` | `main` with head `at + 1` (its `session/forked`; `0` for a fork at `0`), and every branch whose `branch/create` is on `main` at or before `at`, with its stored `info` and head 0. Branches below those are not copied: their creations are in events F does not get |
| `timeline_events` | `main`'s rows through `at` (the cut rule means no row spans it), with kinds, bodies, sequences, batch ends, event ids and before traces as stored, then one new `session/forked` row at `at + 1` (none for a fork at `0`). No row of a child branch |
| `timeline_content` | The rows the copied events reference, by the references the codec writes for requests and their before traces, and every list node those references reach (format v17). The `session/forked` row references no content |
| `timeline_snapshots` | The rows anchored on copied `main` events. They are derived from the same facts, so F's first load uses them instead of replaying from the start |
| `session_file_owners` | One row for F per file S owns ([Session files §4](session-files.md#4-sharing-on-fork)) |
| `session_output_owners` | One row for F per output path S owns, including a row registered by a command still running at the fork (ADR 013 D4). No file is copied; F reads the current content and keeps it after S is deleted |
| `session_grants` | None: a fork is a new Session and does not inherit grants ([ADR 012](../../adr/012-session-grants.md) D1) |
| Title metadata (S5b) | None: a fork starts untitled ([SessionTitle](../plugins/SessionTitle.md) A7) |

**Why child branches come without their events.** A child's events are not ordered against `main`'s cut, so no prefix
of a child corresponds to `at`. Copied whole, a child holds what happened after the cut, and its pending work (an
unstarted first input, a Queue or Steer input, a background task's wake) would start turns in F for work the source
already ran or is running, in the same `cwd` (§6.5). The store still requires a directory for every `branch/create`
and checks it when it loads a Session ([timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs)
`validate_session`); a directory with head 0 passes. TimelineTree treats a child without its `branch/start` and first
Queue input as a creation that never started ([timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)
`restore`): at F's first load it writes `branch/create-failed` (`error: "branch creation failed"`) on F's `main` and
builds nothing, and later loads skip the child because of that fact. A crash between a creation's commit and the
child's start already takes this path. In F the fact means the branch is not part of F; the branch and its work stay
in S, where a client can continue them. The fact is not model context, so the prompt prefix at the cut is unchanged.

**Why only referenced content.** Content rows are per Session. Copying all of them would copy, for a fork from an
early point of a long Session, the requests of everything after the cut, while the copy holds the store's write lock.

Rejected alternatives:

| Alternative | Why not |
| --- | --- |
| Copy each child whole, as committed when the fork transaction runs (this page's first draft) | It carries the child's work after the cut into F, and the reload rules run its pending inputs and wakes again in the shared `cwd` (§6.5) |
| Copy a child only up to its creation | Its first Queue input becomes pending again, and F runs the delegated task a second time |
| Copy a child through its last rest point | It still carries work after the cut. A child on its first task has no rest point after its creation, and the check would read every copied child's facts while the fork transaction holds the write lock |
| Copy every content row of the source | Disk and lock time for requests after the cut, which no copied event references |

The copy is one write transaction; every Session's commits wait for it, as they wait for any write round, and its
length grows with `main`'s copied prefix.

### 6.4 What a fork does not copy

| Not copied | Why |
| --- | --- |
| `main` events after `at`, branches created after `at` | After the cut |
| Events of child branches, and the branches below them | Not ordered against the cut, and a reload would run their pending work again (§6.3). Their work stays in S |
| The loaded instance, leases, subscriptions, attached clients, in-flight requests | Runtime state, not stored; F builds its own |
| An input still being submitted | Not committed, so not a fact |
| The command output directory | The bytes are not copied; F's `session_output_owners` rows name the same files under `command-output/<S>/` or `.dim/exec/<S>/<branch>/` and keep them readable after S is deleted (ADR 013 D4, §6.6) |
| Title metadata | SessionTitle A7 |
| File grants | A grant is a per-Session approval; F is a new Session and does not inherit S's grants ([ADR 012](../../adr/012-session-grants.md) D1). The user answers for F separately |
| Collaboration relations and temporary Agents | Process memory of the source's team. F leads no member, and S's temporary Agents stay invisible to F |
| Host configuration | Never part of a Session |

### 6.5 Opening the fork

Agents creates F with its existing prepared creation: the prepare step is SessionStore's fork transaction instead of
`create_owned_session`. The runtime then validates F's rows and builds F. A failure rolls F back, its owner rows
included ([Session files §5.3](session-files.md#53-delete-s5c)); the source is untouched.

TimelineTree opens a root branch that already has events with `branch/resume`
([timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) `restore`). F's `main` therefore
continues with `branch/resume` after the copied facts. Before that, the fork transaction itself writes one fact of its
own after the copied facts: `session/forked` (empty data) at `at + 1`, so F never exists without it. It says that
every earlier event of F's `main` was copied. Owners that fold a branch's own history (Goal, A11) start
after it, and v3 clients read it the same way; every other owner keeps treating the copied facts as F's
history under its reload rule, and a v2 connection keeps its list and drops this fact. A fork at `0`
copies nothing, writes no `session/forked` and opens with `branch/start` like a new Session. F is the source's conversation restored at the cut under a new
id, and every owner applies its existing reload rule. The cut is a rest point (§6.2) and child branches come without
events (§6.3), so no reload rule starts work: **F runs no turn, model call or tool until a client sends it input.**
F and S share `cwd`, so this keeps a fork from repeating the source's side effects.

| At the cut | In F |
| --- | --- |
| Model configuration (`context/configured`), prompt, adopted tools | Kept. A resumed branch keeps its configuration, and Prompt delivers source changes as on any resume, so the prompt prefix equals S's at the cut |
| Permission preset (`permission/changed`) | Kept. Permission folds the copied facts (#26); the policy in `SessionConfig` is copied too |
| An open turn, a Queue or Steer input not consumed, a running background task | None: the cut is a rest point (§6.2) |
| An Append input not consumed | Stays pending, and joins the next request a client's input starts in F |
| A branch `main` created | Its directory without events. TimelineTree writes `branch/create-failed` on F's `main` at the first load (§6.3) |
| A Collaboration request delivered without a result | Collaboration writes its failed result (`capability reloading`). A result written by a reload posts no report input, so no turn starts. It does not stop the source's member: a reload stops only members whose relation names the reloading Session as their leader ([#127](https://github.com/arcships/dim/issues/127), §14) |
| A Collaboration result whose report input comes after the cut | F keeps the result without its report: a reload posts no report, as for the source after a crash in that window. Nothing starts; the model can still read the result with the `agent` tool |
| A live goal (Goal, P5) | Goal's A11 (§14) |

**Child branches after the cut.** A child runs on its own Timeline while `main` goes on, so the source can hold child
events from after the cut. F holds none of them:

```
 1. The child finishes after the cut
 S main: .. 8 branch/create(c1) .. 10 step/tool-result "Branch created: c1" .. 12 turn/end | 13 ..
                                                                                         cut
 S c1:   1 branch/start  2 input/message (the task)  3 turn/start .. 25 turn/end   (after the cut)
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   directory only

 2. The child is running when the fork commits
 S main: .. 8 branch/create(c1) .. 12 turn/end | ..
 S c1:   1 .. 5 turn/start  6 step/tool-call (exec)   <- the fork transaction commits here
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   directory only: no turn to fail, no input to apply, no task to wake

 3. The child gets input after the cut
 S main: .. 8 branch/create(c1) .. 12 turn/end | ..
 S c1:   1 .. 9 turn/end  10 input/message (steer, from a client)  11 turn/start ..
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   directory only
```

| Case | In the source | In F |
| --- | --- | --- |
| 1. The child finishes after the cut | Nothing reaches `main`. TimelineTree has no hand-back: the `branch` tool returns `Branch created: <id>` once the child has started, and a child's turns end in its own Timeline. `wait` wakes on background-task notices and steers, not on a child | F's `main` holds the creation and, for a `branch` tool call, that result, both before the cut, then `branch/create-failed`. It is handed no result, waits for nothing and records no other failure. The child's result stays readable in S. The one hand-back Dim has is Collaboration's, from a member Session: the Collaboration row above |
| 2. The child is running when the fork commits | Its turn, model call or tool batch is open | No child event is copied, so nothing is failed, applied, woken or run again in the shared `cwd`. F's `main` gets `branch/create-failed`, not a "delegation failed" input: no such input exists |
| 3. The child gets input after the cut | `main` cannot send input to a child. A child gets input from clients (`session.submit {branchId}`), from its own capabilities (exec wakes, Collaboration reports) and from TimelineTree's first input | F has none of the child's inputs, so it holds no input F never received |

Rejected: `branch/start` after the copied facts. Prompt reads a root `branch/start` as a new Session and adopts the
model and system prompt in `SessionConfig`, which would undo model changes the copied facts carry and change the
prompt prefix ([prompt.rs](../../../crates/dim-agent-capabilities/src/prompt.rs) `open`). Avoiding that would change
TimelineTree, to start a prepared root that has events, and Prompt, to tell a fork's start from a new Session's. A
crash between F's commit and that start would still leave F with `branch/resume`.

### 6.6 Identity, provenance and paths

- **Event ids.** Copied events keep their ids. An event is identified by its Session and its id: the store checks
  ids per Session and indexes them by `(session_id, event_id)`. New ids would mean rewriting every reference to them:
  `input_id`, `call_event_id`, compaction cursors, before traces, snapshot anchors and Collaboration operation ids.
- **Other ids.** Branch, turn, request, call, task and approval ids are kept; they are also scoped by Session. A
  copied task id names a task that ended by the cut (§6.2): BackgroundExec rebuilds it from the copied facts, and
  `session.exec.stop` returns its recorded result and stops nothing.
- **`submission_id`.** It is copied inside `input/message`, so `session.input_lookup` on F finds copied inputs, since
  it reads F's history. [#32](https://github.com/arcships/dim/issues/32) keeps the field in the event
  ([sessions service §6.3](sessions-service.md#where-the-submission-identity-lives)), so the fork needs no record of
  its own for it.
- **Provenance.** F records no source: no column, and `session/forked` carries no data. Nothing decides with it today, and session access A.3
  adds the Session's origin (`createdBy`) when something does, with its own format decision.
- **Paths.** Session files and command output are shared through owner rows and stay valid while any owner exists
  (S5a, ADR 013). F's copied tool results quote `command-output/<S>/…` and `.dim/exec/<S>/<branch>/…`; the output
  rows the fork copied let F read the current content after S is deleted, and the delete keeps the directories
  while F names a path under them. A command output log may grow after the fork; F reads its current content. A
  point-in-time copy is a separate concern.

Rejected for S5c: keeping S's command output until S's forks are deleted. ADR 013 reverses that decision: a fork
copies the source's output rows, and a directory is removed by the delete of its last owning Session in this database,
so no fork relation is needed.

### 6.7 Errors

| Result | When | State after |
| --- | --- | --- |
| `{sessionId, sequence}` | F is published | F exists |
| `unknown_method` | Protocol v1 or v2 | Nothing created |
| `invalid_params`, `session_invalid_input` | Param shape, a cut `session.branch_create` rejects at the same position, or a cut that is not a rest point (§6.2) | Nothing created |
| `session_closed` | The source is unknown or deleted, also when its delete commits before the fork transaction | Nothing created |
| `session_protocol_version` | The source belongs to another protocol version | Nothing created |
| `session_build_failed` | F's build fails | F rolled back |
| `-32603` "Session operation failed" | Storage failure, including F's copied rows failing validation | Nothing committed, or F rolled back |

## 7. Serialization and races

Deletes and forks decide at write transactions of the one native store connection, as puts do in
[Session files §6](session-files.md#6-deletion-crash-recovery-and-serialization). Agents' admission gate orders a
delete against loading.

| Race | Outcome |
| --- | --- |
| Fork commits, then the source's delete | F keeps its copy and the shared files |
| The source's delete commits, then the fork transaction | It finds no source: `session_closed`, nothing created |
| The delete's close during the fork's cut check | The check fails `session_closed` |
| Put against delete or fork | Session files §6 |
| Two deletes | The second waits for the gate, then finds no rows: `{}` |
| Delete of a Session still building: a `session.create`, a member's creation or a fork whose rows are committed and listed | The creation holds the new id's gate from before its rows commit until the Session is published and its protocol version recorded (D6). The delete waits, then closes the published Session and removes its rows. A creation that fails rolls its rows back, and the delete finds nothing |
| Delete against attach, submit or a Session method | They wait for the gate, then fail `session_closed`; requests that already hold a use end with the close |
| Delete against the Session's own appends | The close settles them; the row transaction follows every queued append on the Timeline writer |
| Fork against appends to the source | Rows through `at` never change, and the fork copies no row after them: child directories come without events |
| Delete of a Collaboration leader | Its close stops its member requests, and Agents then deletes the Team with it: the members it leads and the Agents it owns ([Collaboration §6](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)) |
| Delete of a member | The leader's requests to it settle as member closed, as today |
| Two forks of one source | Independent Sessions, each owning the shared files |

**S6 review notes.** "SessionStore must close the Session and release its leases first": step 2 comes before any
removal. "`attachment.put` racing a delete should expect `Locked` or `DirectoryNotEmpty`": a put's directory belongs
to no Session (S5a), the delete never removes it, and S5a's sweep removes only unowned, non-pending files, so neither
meets a put's lease. The command output removal meets `Locked` or `DirectoryNotEmpty` only from a writer outside the
closed Session; the delete then fails and is retried (§3.5). "Put and delete must be serialized": the store's write
transactions order them (S5a §6); no in-memory lock is added.

## 8. Storage format

**Decision: no format change of its own** (S5c itself added no change; #222 added one event row at v16 without a version
bump, and format v17 keeps it). Recorded in [native-storage-format.md](native-storage-format.md#s5c-格式决定).
[ADR 013](../../adr/013-command-output-ownership.md) adds `session_output_owners` at v29; this page's delete and fork
steps read and write those rows with the steps above.

- A delete removes rows from existing tables and, since [#556's catalog projection](native-storage-format.md#556-v23-格式决定),
  the derived `session_catalog` row.
- A fork inserts rows into existing tables and copies stored text byte for byte, then inserts one `session/forked` row
  (empty data) at `at + 1`, and writes F's `session_catalog` row in the same transaction from the rows it writes. A
  child directory with head 0 and no events is a state the store already accepts: a creation whose child never
  started.
- An event id is unique within its Session, and a fork repeats the source's ids in another Session. The store already
  checks and indexes ids per Session; S5c states this as an invariant, not as a change.
- No provenance column and no tombstone, for the reasons in §4 and §6.6.
- **Compatibility (development phase, decided with #222).** `EventKind` is a closed list, so a daemon built before
  `session/forked` existed cannot decode that row and cannot restore a Session forked by a newer daemon. This is
  accepted: the native format has no compatibility promise before its first release, so the number is not bumped and
  no fallback decoder or migration is added. A Session forked at `0`, and every Session that was never forked, has no
  such row. A deleted Session reads as one that never existed.
- **Format v17.** [Request lists](request-delta-storage.md#11-format-v17) move the format to v17 and keep this row as
  it is; it references no content. Every daemon built before `session/forked` refuses a v17 store with the existing
  "unsupported SQLite schema … preserved" error, so a daemon that opens a v17 store decodes the row. The gap above
  remains for v16 stores only.

## 9. Ownership and S2

| Operation | Owner | Called by |
| --- | --- | --- |
| `session.delete`, `session.fork` (shape, protocol version) | Rpc built-ins | Clients |
| Delete orchestration: gate, close, forget | Agents (`AgentIndex::delete`) | Rpc |
| Creation from another Session's cut | Agents (`AgentIndex::fork`), the Agents supplement in overview B.2 | Rpc |
| Command output removal, row removal, fork transaction | SessionStore, Rust methods | Agents |
| Session-file rows and sweep | SessionStore (S5a) | `sessionFiles.put`, `delete_session` |
| Output ownership rows | SessionStore (v29, ADR 013) | `sessionFiles.register`, `delete_session`, `session.fork` |

**S2 ([#62](https://github.com/arcships/dim/issues/62)).** S2's `sessions.launch` creates a Session from a profile
and a policy capped by its starter; it does not create from a cut. Agents keeps one creation path for both: prepared
creation, whose prepare step is `create_owned_session` for `session.create` and `launch`, and the fork transaction for
a fork. S2 adds no fork method to `sessions`. A later `sessions.fork` would call `AgentIndex::fork` and cap the copied
policy by its starter as `launch` does, because a process caller must not get a wider preset than its starter holds.

## 10. Authority (ADR 003)

- A fork copies the permission preset and never widens it. The preset at the cut is one the source held, and the
  caller could already change the source's preset with `session.permission.update`.
- Copied approval facts are decisions about calls that are already settled. Approval rechecks the current policy on
  every answer and itself records no standing grant, so no authority comes from them. A client-written Session
  grant is separate and is not copied by the fork (§6.4, [ADR 012](../../adr/012-session-grants.md) D1).
- Host security settings never enter the Timeline, so a fork neither restores nor widens them (ADR 003 D5).
- Owning the source's Session files adds no authority ([Session files §11](session-files.md#11-security)).
- Copied references to another Session's resources (member Session ids, task ids, paths) confer nothing: an owner
  settling a fork acts only on resources its own Session holds (§14, Collaboration).
- A delete changes no host state, and diagnostics carry ids and error kinds, never event content.

## 11. Resource nodes (P3)

S5c implements the control node only, as S5a does. There, step 3 fails while FileStorage is reloading or closed, and
the delete is retried. On a P3 resource node, step 3 runs on that node's FileStorage,
and an unavailable node fails the delete with node unavailable. Whether a delete may then commit and leave the
directory for the node to reconcile is decided in P3 with the resource-node lookup
([#38](https://github.com/arcships/dim/issues/38)): a resource node serves one control node, so a listing there,
unlike in a shared database directory, can attribute its entries.

## 12. Changes to other documents

| Document | Change in this PR |
| --- | --- |
| [Overview B.2](../overview.md#b2-app-server-process-plugins) | Rpc row: `session.delete` and `session.fork` built-ins. Agents and SessionStore rows link here |
| [Native storage format](native-storage-format.md#s5c-格式决定) | S5c format decision: no change |
| [AcpGateway](../plugins/AcpGateway.md), [RemoteGateway](../plugins/RemoteGateway.md) | Their text said the preset is reset on fork, which #26 replaced: the preset is copied with the facts |
| [Documentation index](../../INDEX.md) | Link to this page |
| [Session files](session-files.md) | §5.3 and §12 said a delete commits while the node is offline. That holds for its Session-file step; the delete as a whole removes the command output on the node first (§11). §5.2, §5.4 and §6 are unchanged and are the steps this page runs; §5.4 notes that removal is now synced |
| [FileStorage](../../architecture/rust-agent/plugins/FileStorage.md) | Removal syncs the parent of the removed entry (§14) |
| [Single-tree assembly](../../architecture/rust-agent/single-tree.md) | The `Agents` reload row: reload settlement stops only members the reloading Session leads (#127) |
| [Session access A.3](../contracts/session-access.md#a3-sessionconfig-fields) | Its status line: a fork copies the stored `SessionConfig` text, so `interactive`'s omission rule carries over |

## 13. Rule check

| Rule | Answer |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | Delete and fork change only Agents' and SessionStore's own state: Session records and rows. A fork copies stored bytes and writes one fact of its own, `session/forked`, and none for another owner; the facts F writes when it opens come from their owners (TimelineTree's `branch/resume` and `branch/create-failed`, Collaboration's failed results). Goal decides on `session/forked` itself (A11). The preset is copied, not reset (#26) |
| Principles §4 rule 2 (handover) | Not applicable: nothing is handed over |
| Principles §4 rule 3 (before callbacks only) | No before callback. Rpc checks shape and the Session's version; the cut check is `session.branch_create`'s existing one plus the rest point, both reads of the fixed prefix |
| Principles §4 rule 4 (no business meaning in core) | Timeline, Dispatcher, before and batches are untouched. TimelineTree's existing rules, that a root with events resumes and a child without its start is a failed creation, apply unchanged |
| Principles §4 rule 5 (batch) | The cut ends an atomic batch, so the copy holds whole batches; `session/forked` is a batch of its own after it |
| Principles §4 rule 6 (context from handed-over events) | F's context comes from the copied adopted inputs and tool results; nothing else enters it |
| Principles §4 rule 7 (public events and services only) | Agents uses the Session facade (`close`, `context`) and SessionStore's methods; SessionStore uses FileStorage's public `remove`. The Collaboration requirement keeps a fork from acting on the source's members |
| Principles §4 rule 8 (requirements to mechanism owners) | No change to the Timeline, Dispatcher, envelope, assembly, rutis or the Session facade, and nothing raised to rutis. One event kind is added, `session/forked`, owned by the fork; it is a format decision made with #209, because `EventKind` is a closed list until dynamic events (#2). The fork transaction gives it sequence `at + 1` in F's storage, as it gives the copied facts theirs: F's Timeline does not exist yet and publishes the stored history when F loads. The synced removal is a FileStorage supplement about the durability of its own operation; the rest-point check only reads the facts of the capabilities whose reload rules start work; the Collaboration check is Collaboration's own rule (§14) |
| Principles §4 rule 9 (only facts later decisions use) | One Timeline event, `session/forked`, because Goal decides with it: an owner that folds a branch's own history starts after it (time-bus §2: that is its consumption rule; no other owner must consume it). Its data is empty. It is internal: clients learn of the fork from the reply, and v1 excludes it. Deletion is a process-level operation on records and writes no event |
| Principles §1 (Capability and App Server boundary) | No new plugin; the Agents and SessionStore supplements are in overview B.2 |
| ADR 002 D1 (scope follows state) | Process scope: records and the database directory outlive any Session |
| ADR 002 D2 (no wiring modules) | Rpc built-ins call Agents; SessionStore's steps are internal methods, with no adapter or registry |
| ADR 002 D3 (dependencies follow scope) | Rpc, Agents and SessionStore depend on no optional plugin; FileStorage is looked up per call |
| ADR 002 D4 (structural vs business configuration) | No configuration |
| ADR 002 D5 (language-neutral contracts) | The two wire methods take and return data. SessionStore adds no service |
| ADR 003 (approval scope) | §10: a fork copies the preset and never widens it; copied facts and shared files confer no authority |

## 14. Requirements raised to mechanism owners

None to rutis. To the event format: the event kind `session/forked` (§13, rule 8). Supplements and requirements to other owners:

| To | Requirement | Status |
| --- | --- | --- |
| FileStorage | `remove` syncs the parent of the removed entry before it reports success, also when the entry was already absent, and returns `Unsynced` when that sync fails. Windows has no directory sync and relies on NTFS metadata journaling, as for `replace`. Needed so a delete's removal is durable before its commit (§4) | Implemented in S5c ([FileStorage](../../architecture/rust-agent/plugins/FileStorage.md#listing-and-removal)) |
| Loop, InputQueue, BackgroundExec | A read-only rest-point check over a fixed prefix: no open turn, no Queue or Steer input waiting to drive a turn, every background task ended with its wake posted (§6.2). A capability whose reload rule comes to start work from other facts adds them to the check, so a fork keeps starting nothing | Implemented in Agents as a fold over these capabilities' public facts, with no change to them (§16 D1). Their owners keep those facts and extend `rest_point` in [agents.rs](../../../crates/dim-app-server/src/agents.rs) when a reload rule comes to start work from other facts. Found on the way, for BackgroundExec's owner: its `rebuild` keeps `closing` from an earlier generation's branch close (a `session.close` or a graceful shutdown), so after a restore it does not repost a wake lost in a crash. `rest_point` scopes that flag to the generation, as the live wake does, so a cut between such an `exec/ended` and its wake is refused |
| Collaboration | When a branch rebuilds, a delivered request without a result is settled as failed without stopping its member unless the member's relation names this Session as its leader. In general, reload settlement acts only on resources its own Session holds. The rebuild stopped any loaded Session named in the fact ([collaboration.rs](../../../crates/dim-app-server/src/collaboration.rs) `rebuild`), so a fork of a leader would have stopped the source's member | Implemented ([#127](https://github.com/arcships/dim/issues/127)). Before #127 that reload stopped such a member if it was loaded. Now the member keeps whatever its own reload rules start, the leader has already settled the request as failed, and the member's `agent` messages to a fork of the leader are refused (`TeamMemberNotFound`) because no relation names the fork ([#97 review](https://github.com/arcships/dim/pull/97)). A restart keeps the Team: relations are rebuilt from the stored leaders ([Collaboration §6](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)). Acceptance 7 |
| Goal (P5, [#69](https://github.com/arcships/dim/issues/69)) | A copied goal must not drive a turn in F or be F's goal. The fork writes `session/forked` after the copied facts in its transaction (§6.5); Goal folds `main` from after that fact and clears a copied live goal once ([Goal](../plugins/Goal.md) A11) | Implemented with Goal's acceptance ([#209](https://github.com/arcships/dim/pull/209)) |
| S5b ([#60](https://github.com/arcships/dim/issues/60)) | Title metadata rows are removed in the delete transaction and not copied by a fork | Implemented in S5b ([#96](https://github.com/arcships/dim/pull/96), [title metadata §9](session-title-metadata.md#9-lifecycle)) |
| [#32](https://github.com/arcships/dim/issues/32) | When `submission_id` leaves `input/message`, the fork copies the idempotency records of inputs at or before the cut in its transaction, so `session.input_lookup` on a fork keeps answering for copied inputs | Not needed: #32 keeps the field in `input/message` ([sessions service §6.3](sessions-service.md#where-the-submission-identity-lives)), so the fork copies it with the event |
| F6 ([#77](https://github.com/arcships/dim/issues/77)) | `dimcode session delete` and cleanup by age or count with a preview, composed from `session.summaries` and `session.delete` (§5.1) | F6's design |

Collaboration's branch unload does not await a member's settlement: it interrupts the member, commits the request's
failed result and releases the request, because the member's Loop may be reloading in the same cascade. Awaiting the
stop would deadlock that reload, and delaying release would let a later member input land after the interrupt.

## 15. Acceptance (flow tests)

1. Delete a loaded Session with a running turn while a client is attached: the client gets `session.closed`, the reply
   is `{}`, `session.list` omits the Session and `session.attach` fails `session_closed`. No row with its id remains
   in any table, `command-output/<id>/` is gone, and its Session files are removed by the sweep. A second delete
   replies `{}`.
2. Delete an unloaded Session, and one whose rows fail validation: both reply `{}` and leave no row.
3. Fork at a rest point of `main`, with one child branch created before the cut and one after: F's `main` holds the
   source's events through `at` with the same ids, then `session/forked`, `branch/resume` and one `branch/create-failed` for the first
   child; F has the first child's directory without events and nothing of the second; F has only the content rows
   and snapshots of its `main` events; after a restart it reloads without a second `session/forked` or `branch/create-failed`. A fork at `0` opens with `branch/start` and has no `session/forked`.
4. The cases of §6.5: fork while a child finishes after the cut, while a child has an open turn with a tool batch and
   a background task, and while a client steers a child after the cut. In each, F's child has no events, F records
   no `turn/start`, model call, tool execution or `exec/started` until a client submits input to F, and the source's
   child goes on unaffected.
5. A fork's `SessionConfig` text equals the source's, including `"interactive": false`. A permission preset and a
   model set on the source before the cut are F's preset and model.
6. A fork is refused for `at` beyond the head, inside a batch, outside a context boundary or not at a rest point
   (inside a turn, with a Queue or Steer input not consumed, with a background task running or its wake not
   consumed), for an unknown source, and on a v2 connection, and creates nothing.
7. Fork a leader while its request to a member is open at the cut: F records a failed result, posts no report input
   and starts no turn, and the source's member keeps running ([#127](https://github.com/arcships/dim/issues/127)).
8. Fork, then delete the source: F loads and reads its Session files and the output paths it copied; deleting F
   removes them. A delete and a fork of the same source raced: either F exists with the files or the fork fails
   `session_closed`; never a fork without its files.
9. On disposable stores: a Session whose command output directory is gone but whose rows remain loads, and a delete
   then completes; unowned file rows left by a delete commit are swept on the next generation's first put.
10. A delete that fails after it closed a loaded leader, because its command output cannot be removed, replies
    -32603 and keeps the leader's rows; the leader is neither listed nor loaded again in this process, and its member
    stays a Session. After a restart the leader loads, and a retry deletes it (D5).
11. A delete of a Session still building, a `session.create` and a `session.fork` held in a capability's apply after
    their rows commit and are listed, waits for the build, then replies `{}`; no row and no loaded Session of the id
    remain (D6). An id that is not one path component is refused and removes nothing (D3).

Tests: 1 and 2 are `delete_closes_a_running_session_and_removes_what_it_stored` and
`unloaded_and_damaged_sessions_are_deleted_by_id` in
[deletion.rs](../../../crates/dim-app-server/tests/transport_flow/deletion.rs). 3 to 8 are in
[fork.rs](../../../crates/dim-app-server/tests/transport_flow/fork.rs):
`a_fork_copies_main_through_a_rest_point_and_starts_nothing` (3, 4 and the `interactive` of 5),
`a_fork_waits_for_background_work_and_keeps_preset_and_model` (the child with a background task of 4, the preset and
model of 5, and the background and batch cuts of 6, with the real BackgroundExec),
`a_fork_is_refused_unless_the_cut_is_a_rest_point` (6, and the inputs D1 lets rest at a cut: a withdrawn Queue input
and a Steer input stopped before it was consumed), `a_fork_of_a_leader_does_not_stop_the_source_member` (7) and
`a_fork_shares_session_files_and_outlives_its_source` (8). `output_ownership_survives_a_fork_and_a_source_delete` in
[storage_flow.rs](../../../crates/dim-app-server/tests/storage_flow.rs) checks the output rows, the shared directory
after the source's delete and both ownership tables (8, ADR 013).
`a_fork_copies_main_through_the_cut_with_its_content_and_snapshots` in
[storage_flow.rs](../../../crates/dim-app-server/tests/storage_flow.rs) checks the content rows and snapshots of 3 at
the store, including content that only a before trace references. 9 is
`an_interrupted_delete_converges_on_retry_and_the_next_sweep` in
[files/tests.rs](../../../crates/dim-app-server/src/storage/files/tests.rs), which stops a delete between its output
removal and its commit, together with the Session without output in `unloaded_and_damaged_sessions_are_deleted_by_id`.
`a_delete_leaves_another_databases_command_output` in deletion.rs checks §4's shared directory with two daemons, and
`a_failed_delete_leaves_the_session_closed_until_a_retry` there is 10, and `a_delete_waits_for_a_building_session` is
11, with the refused ids in `delete_closes_a_running_session_and_removes_what_it_stored`.

Verification: `./dev check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and
`cargo test --workspace --locked`. No Capability, assembly or crate dependency changes, so `xtask` is not needed
beyond its usual CI run.

## 16. Design changes in implementation

Changes the implementation made to this design ([roadmap §5.3](../roadmap.md#53-实现中发现的变更)). Each affects only
S5c's own owners.

| # | Change | Why |
| --- | --- | --- |
| D1 | The rest-point check is `rest_point` in Agents, a read-only fold over the public facts of Loop, InputQueue and BackgroundExec on the prefix Agents already reads for `validate_context_boundary`, instead of a check next to `validate_context_boundary` in the capabilities crate. Its input rule follows Loop's: a Queue or Steer input applied before the last `stop/interrupt` and not consumed is allowed, because a reload does not run it (it joins the next request, as an Append input does); an input InputQueue deleted on a withdrawal (`input/deleted`, S1) is never applied and does not count; any other input not yet applied is refused. A task's wake is required when the task ended outside a branch close of its own generation, as the live wake is posted | Every fact it needs is a public event, so it needs no API from those capabilities and no change to them. Requiring every such input to be consumed would refuse the cut right after a stopped turn, where nothing would run |
| D2 | `session.fork` needs the native store. A daemon without one (the mock profile without `--database`) does not list it in `hello.features` and answers `unknown_method` ("Method requires native storage"). `session.delete` is listed with `session.create` there too; without a store it closes the Session and forgets it | A fork copies stored rows; in-memory Timelines have no SessionStore to copy from |
| D3 | `session.delete` refuses a `sessionId` that is not a single path component with `session_invalid_input`. An empty `sessionId` is refused before it, at the boundary every Session method shares, with `invalid_params` ("sessionId must not be empty"); the path-component rule covers the other ids. Agents checks it before it takes any admission state, and a delete that found nothing loaded or stored under the id keeps no slot for it; `session.close` of an id Agents does not know keeps none either | The id names `command-output/<id>`. Session ids are UUIDs; an id such as `<S>/.` would otherwise remove S's command output while deleting no row. Without the last two rules, deleting or closing arbitrary absent ids would add a slot each, without bound |
| D4 | The fork transaction does not check the cut again: it copies `main`'s rows through `at`, and the runtime validates the fork's rows when it builds it. A copy that fails validation rolls back like any failed creation | Agents has checked the cut on the source (published head, batch end, context boundary, rest point), and rows through the cut never change, so a second check inside the transaction would repeat the validation every build already runs |
| D5 | A delete that fails after its close leaves S closed in this process, as `session.close` does, instead of releasing S's slot so that S loads on its next use (step 6, §3.5) | The close is a business close: S's capabilities settled its work and Collaboration ended the relations that name S, as it does for every closed Session. Loading S again would run a leader that cannot reach its members and, after #127, stops none of them on reload ([#97 review](https://github.com/arcships/dim/pull/97)). With a closed slot the failure leaves the state `session.close` already produces, which every owner handles; telling a delete's close apart from other closes would add delete state in Agents and a notice for one failure path. A retry needs no load, and a restart loads S as it loads any closed Session |
| D6 | A prepared creation (`session.create`, a member's creation, `session.fork`) holds the new id's admission gate from before its prepare step commits the rows until the Session is published and its protocol version recorded (`AgentIndex::admit`); a failed creation keeps no slot | A Session's rows are listed from their commit, while the runtime still builds it. A delete that skipped the building Session could commit after the build read its branches, and the build would publish a loaded Session without rows, whose appends violate the foreign keys; a fork would also record the deleted id's protocol version again. A creation is a load, so it takes the gate a restore takes, and the delete orders after it ([#97 review](https://github.com/arcships/dim/pull/97)) |
| D7 | The fork transaction writes `session/forked` (empty data) at `at + 1` after the copied facts (not at `0`), and owners that fold a branch's own history start after it. Earlier this design wrote no Timeline event and left Goal a fallback on its first load | Decided with Goal A11 (#209): a forked Session starts with no goal, and the boundary must be a fact its owner writes when the fork happens, so a crash between the commit and F's first opening cannot lose it and a consumer decides on it like any event |

## Appendix A. Rust binding

In dim-app-server:

```rust
impl AgentIndex {
    /// `session.delete`: id check, gate, close if loaded, `SessionStore::delete_session`, forget (§3.2).
    pub(crate) async fn delete(&self, id: &str) -> dim_agent_runtime::Result<()>;
    /// A prepared creation under the new id's admission gate until it is published (§7, D6).
    pub(crate) async fn admit(
        &self,
        id: &str,
        agent_id: &str,
        version: u32,
        creation: impl Future<Output = dim_agent_runtime::Result<Session>>,
    ) -> dim_agent_runtime::Result<Session>;
    /// `session.fork`: acquire the source, check the cut, prepared creation (§6.5).
    pub(crate) async fn fork(&self, source: &str, at: u64) -> dim_agent_runtime::Result<Session>;
}
/// The rest point of §6.2 over `main`'s prefix (D1).
fn rest_point(events: &[Arc<AgentEvent>]) -> Result<(), String>;
impl SessionStore {
    /// `ctx` gives each file step the node's FileStorage; `files_root` is the
    /// database directory's root.
    pub fn new(db: Arc<SqliteStorage>, ctx: Ctx, files_root: usize) -> Self;
    /// Removes every output directory the id names, except one another
    /// Session in this database still names (ADR 013 D4), then every row in
    /// one transaction; schedules the Session-file sweep. Returns whether
    /// this database had rows of the id.
    pub async fn delete_session(&self, id: &str) -> Result<bool>;
    /// Prepare step of a fork: copies the rows of §6.3 in one transaction.
    pub async fn fork_session(&self, source: &str, fork: &str, at: u64) -> Result<()>;
    /// `sessionFiles.register` (ADR 013 D2): records `path` as output owned
    /// by `session_id`; a missing Session fails.
    pub async fn register(&self, session_id: &str, path: PathBuf) -> Result<()>;
    /// `sessionFiles.owns` and `owned_paths` (ADR 013 D3).
    pub async fn owns(&self, session_id: &str, path: PathBuf) -> Result<bool>;
    pub async fn owned_paths(&self, session_id: &str) -> Result<Vec<PathBuf>>;
}
/// In `storage`: the output root's name below the database directory.
pub const COMMAND_OUTPUT_DIRECTORY: &str = "command-output";
```

`SessionStorePlugin` already takes the database directory's root index (S5a). SessionStore removes `command-output/<id>`
under that root, the directory the daemon gives WorkspaceTools as its output root
([main.rs](../../../crates/dim-app-server/src/main.rs)); `COMMAND_OUTPUT_DIRECTORY` is the one definition of its name. The creation rollback
path (`TimelineStore::rollback_session`) calls the same row removal as `delete_session`.
