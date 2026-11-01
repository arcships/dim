# SessionStore Session files

Status: Implemented. Roadmap item S5a ([#59](https://github.com/arcships/dim/issues/59), [#95](https://github.com/arcships/dim/pull/95)), after S6 ([#51](https://github.com/arcships/dim/issues/51)). The fork and delete
methods that use it are S5c's (§13); §17 lists what the implementation changed.
Overview: [Appendix B.2](../overview.md#b2-app-server-process-plugins), SessionStore and FileStorage rows. Scope:
[ADR 002](../../adr/002-plugin-scope.md) process scope, inside the existing SessionStore plugin.
Consumers: [Attachments](../plugins/Attachments.md) (P2) and Session fork and deletion (S5c).

## 1. Summary

SessionStore stores immutable files for Sessions. A caller hands it a file name and bytes for a
Session; SessionStore writes them once, read-only, on the Session's resource node and returns an
absolute path the model can `read`. The path is fixed for the file's whole life, because Session
history quotes it and history never changes.

- **Placement.** A file lives at `<db>.session-files/<fileId>/<name>`, next to the native
  database `<db>` (by default `dimcode.sqlite`). The directory belongs to that database, not to
  any single Session.
- **Ownership.** Ownership is a set of rows in the native store on the control node. A fork adds
  rows instead of copying bytes. Deleting a Session removes its rows.
- **Removal.** A background sweep removes files whose last owner row is gone, and anything in the
  directory that no row records.
- **FileStorage.** S5a adds one operation, `create_directory`. Sharing needs no link operation.
- **Format.** The native store moves to version 15, with two new tables. A v13 store is upgraded
  in place at startup.
- **Interface.** Attachments reaches the store through the process service `sessionFiles`.
- **Reading.** The model reads a file with `read`, like any file: ownership satisfies file
  scope without a grant, but does not bypass Tool approval Ask (§11).

No Timeline event, no new plugin, no change to the runtime, rutis or the Session facade.

## 2. Structure

```
 Process scope (control node)
 +--------------------------------------------------------------------------+
 |  SessionStore (existing plugin)                                          |
 |    sessionFiles.put ---------------------------+                         |
 |    S5c fork and delete steps (Rust, internal)  |                         |
 |    sweep (background)                          |                         |
 |         |                                      |                         |
 |         | rows                                 | per call, by the        |
 |         v                                      | Session's resource node |
 |  SqliteStorage: session_files,                 | (#38)                   |
 |                 session_file_owners            |                         |
 +------------------------------------------------|-------------------------+
                                                  v
 Resource node (P1/P2: the control node)    FileStorage: create_directory,
                                            replace, list, remove
 Session scope
   Attachments (Deps: SessionIdentity, sessionFiles) -- put(own sessionId, name, text)
```

SessionStore keeps its single dependency, SqliteStorage. It does not depend on FileStorage: it
reads the FileStorage of the Session's resource node on each call, as overview
[§6.3](../overview.md#63-multiple-nodes) requires for node services and as Rpc reads management
providers ([methods.rs](../../../crates/dim-app-server/src/methods.rs) `ctx.get_as`). A FileStorage
reload therefore never reloads SessionStore or the Agents and Sessions above it. Attachments
depends on SessionStore, the core part; SessionStore names no consumer.

## 3. Layout and ownership

### 3.1 On disk

```
<database directory>/               FileStorage root (product profile: ~/.dimcode/rust)
  dimcode.sqlite                    the native store, with session_files and session_file_owners
  dimcode.sqlite.session-files/     SessionStore's directory for this database, 0700
    .dim-file-storage-locks/        FileStorage's own resources, hidden from listings
    3f2a…-uuid/                     one Session file; fileId = random UUID v4, 0700
      .dim-file-storage-locks/
      paste.txt                     the bytes, written once, read-only (0400)
    9c41…-uuid/
      notes.md
```

- The directory sits next to the database whose rows own it and is named after the database
  file, as SQLite's `-wal` and FileStorage's `.lock` companions are. Each database therefore has
  its own directory, even when two databases share a directory (two `--database` files). Copying
  a database file does not copy its Session files.
- On the control node, the database directory is the `--home` directory, or the directory of
  `--database`. In the product profile that is `~/.dimcode/rust`, not the product home
  `~/.dimcode` that `native_home` names ([lib.rs](../../../crates/dim-app-server/src/lib.rs)).
  `serve` registers the database directory as a FileStorage root when it is not one already
  (with `--home` it already is) and passes its index to `SessionStorePlugin`. SessionStore
  provides `sessionFiles` wherever it is loaded; a host without native storage has no
  SessionStore and no Session files.
- The returned path is `root_path(root)/<db>.session-files/<fileId>/<name>`. Roots are canonical
  absolute paths, so `read` resolves the path to the same file.
- On a P3 resource node, the directory has the same name under the home root that node's
  FileStorage registers. The per-node lookup is P3's (#38). A resource node serves one control
  node, since overview §6.3 does not design cross-control-node use. S5a implements the control
  node only, because `SessionConfig.resourceNode` does not exist before P3. It still records the
  node on every file, so P3 needs no format change.

The layout Attachments assumed, `attachments/<attachmentId>/<name>` under a per-Session root,
becomes `<db>.session-files/<fileId>/<name>`, with `attachmentId = fileId`.

The directory is not per Session. History quotes the path, so the path has to stay valid while
any Session still owns the file. A forked Session keeps the source's paths after the source is
deleted. A per-Session directory would therefore outlive its Session, and "is this directory
garbage?" would depend on rows anyway. With a Session-independent directory the rows are the
only authority.

### 3.2 Ownership states

A file is a `session_files` row plus its directory. Its owners are its `session_file_owners`
rows.

```
                txn 1                        txn 2 (commit)
   (no row) ----------> pending -----------------------------> owned (>= 1 owner)
                        writer = G                               |   ^
                          |                                      |   | fork: copy the
                          | abandon: failure, cancel,            |   | source's owner rows
                          | Session gone (writer := NULL)        |   |
                          v                                      v   |
     pending, writer    unowned <---- the last owner row is removed (delete)
     of a dead      ---->  |
     generation            | sweep: FileStorage.remove(<db>.session-files/<fileId>) succeeds
     (sweep claims)        v
                        (row deleted)
```

- **Pending:** `writer` holds the generation id G of the SessionStore instance writing the file.
  It has no owners, so no fork can share it and no Session can see its path.
- **Owned:** `writer` is NULL and at least one owner row exists.
- **Unowned:** `writer` is NULL and no owner row exists. This state is terminal: owners are
  added only by the put that created the file or by copying another Session's owner rows, so an
  unowned file never gains an owner again.

G is a random UUID created by each SessionStore `apply`. Unloading a generation cancels its
admitted puts and waits for each to commit or abandon. A pending row whose writer is not the
live generation therefore belongs to a put that can no longer finish. The design does not rely
on that wait alone: the sweep first claims such a row by clearing `writer` only if it still
holds that old value, and txn 2 commits only while its own row still holds G. A put of an
older generation can therefore never commit a file the sweep is removing.

## 4. Sharing on fork

**Decision: reference rows in the native store.** A fork owns every file its source owns at
the fork's commit. S5c's fork transaction copies the owner rows:

```sql
INSERT INTO session_file_owners (session_id, file_id)
SELECT :fork, file_id FROM session_file_owners WHERE session_id = :source
```

No byte is copied and no file is touched, so a fork succeeds while the resource node is
offline. The fork shares every file the source owns, not only those its inherited prefix
quotes. Telling them apart would mean scanning history text for paths, and a shared file the
fork never quotes costs only disk space until the fork is deleted. A pending file is never
shared. Its path has not been returned yet, so no inherited history can quote it.

The design takes its mechanism from containerd's content store: references live in a metadata
database, a lease protects an in-progress write from collection, and garbage collection
removes content nothing references. Here the owner rows are the references, `writer` is the
lease and the sweep is the collector.

Rejected alternatives:

| Alternative | Why not |
| --- | --- |
| Hard links, one per owner (`sessions/<id>/attachments/...`) | History quotes one path, so it breaks: after the source is deleted, the fork's inherited path no longer exists, whatever other links remain. Links also fail across volumes, on FAT and exFAT and on some network filesystems, and are a second reference count that can disagree with the rows after a crash. They need a new FileStorage link operation for no gain. |
| Content-addressed store (`sha256/<hash>`) with owner rows | Attachments decided against deduplication (A7) and returns an id per put. Identical pastes would share one path and one id, the file name would leave the path, and a put racing the sweep of identical content would need extra coordination. The owner rows are needed anyway. |
| Ownership on the resource node (marker files or a JSON index next to the files) | Fork and delete would need the node online and could not be atomic with the Session rows they belong to. A crash between the two stores leaks files or loses a live fork's files. |
| Ownership in a separate SQLite file | It keeps v13 but loses atomicity with fork and delete: in WAL mode a transaction over attached databases is not atomic across them. |
| A JSON column in `agent_sessions.config` | `SessionConfig` is fixed at creation and belongs to Agents ([session-access A.3](../contracts/session-access.md#a3-sessionconfig-fields)). |

## 5. Flows

`<sf>` below is the Session-files directory, `<db>.session-files`.

### 5.1 Put

```
 Caller        SessionStore                  native store          FileStorage (Session's node)
   | put(S, name, content)                        |                          |
   |----------->| check name, size                |                          |
   |            | read S's resource node -------->| missing S: not_found     |
   |            | look up that node's FileStorage ---------------------------> missing: node_unavailable
   |            | txn 1: S exists? insert row     |                          |
   |            |   (file_id, node, writer = G) ->|                          |
   |            | create_directory(<sf>) ------------------------------------->| parent synced
   |            | create_directory(<sf>/<id>) -------------------------------->| parent synced
   |            | replace(<sf>/<id>/<name>, content, read-only) -------------->| file and parent synced
   |            | cancelled? -> abandon           |                          |
   |            | txn 2: S exists, row still G?   |                          |
   |            |   insert owner (S, id);         |                          |
   |            |   writer = NULL --------------->|                          |
   |<-----------| {fileId, path, bytes}           |                          |
```

The reply is sent only after the file and its directory entries are synced and txn 2 has
committed (`synchronous=FULL`). Windows has no directory sync. As for `replace`, durability
there rests on the file sync and NTFS metadata journaling.

**Abandon.** Txn 1 is followed by a failure, a cancellation (checked right before txn 2), or
txn 2 finding the Session gone or its row no longer pending under G. SessionStore then runs
`UPDATE session_files SET writer = NULL WHERE file_id = ? AND writer = G`. The file becomes
unowned and a sweep is scheduled. If that update fails too, the row stays pending under G, and
the next generation sweeps it.

The put runs as SessionStore's own task: a caller that stops waiting cancels it, and the task
still ends by committing or abandoning, so no row stays pending under a live generation. Until
P3 adds `resourceNode`, the put reads no node: it looks up the control node's FileStorage, and
txn 1 answers `not_found` (D2).

### 5.2 Fork (S5c)

```
 S5c fork(S -> F)             native store (one transaction)
   |-- copy facts before the cut, create F (S5c) ---->|
   |-- copy owner rows of S to F (§4) --------------->|
   |-- commit ----------------------------------------| F owns the same files; no file touched
```

If the source was deleted first, S5c's fork transaction finds no source and the fork fails.
Rows are copied only inside S5c's own transaction, never as a separate call, so a fork is never
left without its files.

### 5.3 Delete (S5c)

```
 S5c delete(S)       Agents          native store (one transaction)        sweep
   | close S ------->| parts unload; Attachments stops calling
   | delete owner rows of S, then S's Session rows --->| commit
   | schedule sweep ----------------------------------------------------------->|
                                                     files left with no owner are removed
```

The owner rows go before `agent_sessions`, because `session_file_owners` references it. A
Session creation rollback (`rollback_creation`) deletes owner rows the same way. When a fork's
creation is rolled back, its files keep the source as owner, unless the source was deleted
meanwhile; they are then unowned and swept. After the commit, files whose last owner was S are
unowned, and the sweep removes them. This step needs no node: its rows commit while the node is
offline, and the files wait as unowned rows until a sweep reaches the node. The delete as a whole
also removes the Session's command output on the node first
([deletion and fork §11](session-deletion-and-fork.md#11-resource-nodes-p3)).

### 5.4 Sweep

```
 sweep(N):
   1 claim:  for each row on node N with writer IS NOT NULL and writer != G:
               txn: SET writer = NULL WHERE file_id = ? AND writer = <that old value>   (now unowned)
   2 remove: for each row on node N with writer IS NULL and no owner:
               FileStorage(N).remove(<sf>/<file_id>)          absent counts as removed
                 ok                -> txn: delete the row if it is still unowned
                 Locked, I/O error -> keep the row, record a diagnostic; the next sweep retries
                 node unavailable  -> stop; the next sweep retries
   3 list:   FileStorage(N).list(<sf>), then read the rows for those names
               every entry with no row -> FileStorage(N).remove(<sf>/<name>)
```

| Trigger | Why |
| --- | --- |
| A put abandons a file | Its directory may exist |
| A commit removes owner rows (S5c delete, creation rollback) | Files may have lost their last owner |
| Every put on a node until a sweep of the current SessionStore generation has completed there (D3) | Pending rows of a crashed or reloaded generation, unowned rows a failed removal left, and directories whose removal a power loss undid |
| S5c cleanup | Explicit reclamation |

Step 3 is safe because the directory belongs to one database (§3.1), and a put
commits its row before it creates its directory. Any directory the listing returns either has
a row by the time the rows are read (a pending or owned file, which the pass skips) or never
had one. It is needed because a directory can lack a row: after a power loss a removed directory
can reappear while its row deletion, committed with `synchronous=FULL`, stays deleted. FileStorage
removal was not synced before S5c ([S6](https://github.com/arcships/dim/pull/87)); it now syncs
the parent before it returns, which closes that window on hosts that can sync a directory. Entries
created outside SessionStore are removed the same way: the directory is SessionStore's.

At most one sweep runs per node. A trigger during a sweep schedules one more pass. Sweeps are
SessionStore's own background work. They are cancelled and drained when it unloads, which is
safe because FileStorage removal can be retried.

## 6. Deletion, crash recovery and serialization

**Order against Session close and leases.** S5c closes the Session first, then commits its
delete transaction, then SessionStore sweeps. No FileStorage lease outlives a put: `replace`
holds its lease only while it writes. The sweep never removes a pending file of the live
generation. A deletion therefore never meets `Locked` or `DirectoryNotEmpty` because of a put.
If it meets them because of something outside SessionStore, the row stays and the next sweep
retries.

**Serializing put against delete and fork.** All three decide at a write transaction of the one
native store connection. Txn 2 checks that the Session still exists. S5c's delete removes the
owner rows and Session rows together, and its fork copies owner rows in the transaction that
creates the fork. SQLite orders those transactions, so no in-memory lock is needed:

| Race | First commit | Outcome |
| --- | --- | --- |
| Put and delete of its Session | Delete | Txn 2 finds no Session: `not_found`; the file is abandoned and swept |
| Put and delete of its Session | Put | The delete releases the file; it is swept unless a fork owns it |
| Put and fork of its Session | Put | The fork shares the file |
| Put and fork of its Session | Fork | Only the source owns it; the fork's history cannot quote a path returned after the fork |
| Fork and delete of the source | Fork | The fork keeps the files |
| Fork and delete of the source | Delete | The fork fails: no source |

**Crash recovery.**

| Crash point | State left | Recovery |
| --- | --- | --- |
| After txn 1, before or during the write | Pending row of a dead generation; possibly a directory and a FileStorage temporary file | Sweep claims the row, then removes the directory (with FileStorage's resources) and the row |
| After the write, before txn 2 | Same | Same; the caller got no path |
| After txn 2, before the reply | Owned file; the client got no reply | The file stays with the Session (no deduplication, Attachments A7) |
| After a delete commit, before or during the sweep | Unowned rows; partial directories | Next sweep; removal is retry-safe |
| During a sweep, after removal, before the row delete | Unowned row, no directory | Next sweep: absent counts as removed |
| Power loss after a sweep removed a directory and deleted its row | The directory may reappear with no row | Step 3 of the next sweep |

## 7. FileStorage additions

**`create_directory` is the only addition.** Sharing is by rows (§4), so FileStorage needs no
link or reference-count operation. Removal, listing and durable writes already exist: S6's
`remove` and `list`, and `replace` with its permissions parameter. The S6 review agreed to add
`create_directory` in S5a.

`create_directory(path, cancel) -> ()`: makes `path`, below an authorized root, an existing
business directory.

| Rule | Content |
| --- | --- |
| Parent | Every component but the last is opened as `open_directory` opens it: an existing directory reached without following links, never the reserved directory. A missing parent fails `Io` `NotFound`; there is no recursive creation, and callers create each level in order |
| Reserved names | `path` already rejects the reserved prefix `.dim-file-storage-` and `auth.json.lock` by name (`InvalidPath`) |
| Aliases | Aliases are checked the way `lock` checks them, before anything is created. The parent's reserved directory is ensured first, so any spelling the filesystem equates with it already exists. Then the key for `auth.json.lock` is ensured in that reserved directory and the key for the last component is opened if it exists; if they are the same file, the name is an alias of the account marker and the call fails `InvalidPath` |
| Create | An absent last component is created relative to the held parent handle: Unix `mkdirat` with mode `0700`; Windows `NtCreateFile` with `FILE_CREATE` and `FILE_DIRECTORY_FILE`, inheriting the parent's DACL |
| Present | An existing directory is opened as `open_directory` opens it, so an alias of the reserved directory fails `InvalidPath`. An existing business directory counts as created, just as S6's `remove` counts an absent entry as removed. Any other existing entry (file, link, reparse point, special file) fails `Io` and is left untouched; links are never followed |
| Durability | In both the created and the present case the parent is synced before the call returns, so a retry after `Unsynced` confirms durability. A sync failure returns `Unsynced` (the directory exists and is visible, durability unconfirmed); its message is generalized from "file replaced" to cover directories. Windows has no directory sync, as for `replace` |
| Roots | A root itself cannot be named: `path` requires a relative name |
| Cancellation | Checked up to the creation, which is the commit boundary. Once created, the sync result is reported |
| Errors | `InvalidPath`, `Io` (`NotFound` parent, an existing non-directory entry, other I/O), `Unsynced`, `Closed`, `Cancelled`, `Worker` |

The alias check never creates a key for the new directory's name, so a put leaves no key behind
in `<db>.session-files/.dim-file-storage-locks`, which is never removed (Design changes, D1). The
operation lives inside FileStorage's own module, beside `list` and `remove`, and holds the same
generation admission. FileStorage's plugin page documents it.

## 8. Storage format

**Decision: version 15, two additive tables, an in-place upgrade from exactly v13.** Recorded in
[native-storage-format.md](native-storage-format.md#s5a-v15-格式决定).

```sql
CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 15))
CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)
CREATE TABLE session_file_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id),
  file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))
CREATE INDEX session_file_owners_by_file ON session_file_owners(file_id)
```

| Column | Meaning |
| --- | --- |
| `session_files.file_id` | Lowercase hyphenated UUID v4; the directory name and the `fileId` the caller gets |
| `session_files.node` | The Session's `resourceNode` at put time; NULL means the control node, as an absent `resourceNode` does. It survives the owners' deletion, so the sweep knows where to remove |
| `session_files.writer` | Generation id G while pending; NULL once committed or abandoned |
| `session_file_owners` | One row per owning Session. The primary key serves fork and delete by Session; the index serves the "no owner left" check |

The rows carry no name, size or timestamp: nothing decides with them, and the file on disk has
them.

**Why not stay on v13.** Startup requires the exact SQL schema
([sqlite.rs](../../../crates/dim-app-server/src/storage/sqlite.rs)), so a v13 daemon refuses a
store with extra tables. The format document keeps a version only when old readers interpret
the new data, old writers do not drop it and the invariants hold; an old daemon that cannot
open the store fails the first condition. C2 and S8a stayed on v13 because they added JSON
fields and rows inside existing tables. S5a needs relations that are atomic with the Session
rows (§4), and no existing table owns them.

**Why 15, not 14.** A branch-only v14 (`user_configuration`) was committed in e41ceaf and
removed in fb3748e. Development stores carrying marker 14 may exist, and the format document
says they are never interpreted as v13. Giving 14 to a different schema would reuse a version
for an incompatible committed format, which AGENTS.md forbids.

**Upgrade path.** The upgrade runs in the existing startup admission transaction (`IMMEDIATE`,
before writable PRAGMAs):

| Store found | Action |
| --- | --- |
| Empty | Create the v15 schema with marker 15 |
| Exact v13 schema, marker 13 | In the same transaction: drop and recreate `agent_store_format` with `CHECK (version = 15)`, insert 15, create the two tables and the index. Only the marker row is replaced; no business row is read or rewritten. A failure rolls back to the untouched v13 store |
| Exact v15 schema, marker 15 | Open |
| Anything else (older formats, the branch-only v14, unknown schemas) | Refuse and preserve, as today |

The upgrade is one-way. A v13 daemon refuses an upgraded store with the existing "unsupported
SQLite schema … preserved" error. This is accepted because the change is additive and the
alternative, refusing every v13 store, loses users' Sessions. Later formats append their own step
from their predecessor. Tests build disposable v13 stores from a frozen copy of the v13 schema and
never open a user's store (AGENTS.md).

**Upgrade log line.** Opening the store tells its caller that it upgraded, and the daemon then
writes one line to stderr: `Upgraded native store <path> from format v13 to v15; earlier dimcode
versions cannot open it.` The line is written after the upgrade commits, so it never claims an
upgrade that rolled back, and only by the start that upgrades. It goes to stderr directly, not
as a `tracing` event, because the daemon's default log filter keeps only errors
([main.rs](../../../crates/dim-app-server/src/main.rs)). A CLI-started daemon's stderr is the
private `daemon.log` in the database directory, so a user who rolls back finds the reason next
to the old daemon's refusal.

**S5c needs no further format change for Session files.** Its fork step only inserts
`session_file_owners` rows (§4), its delete step only removes them (§5.3), and the sweep reads
`session_files`; v15 already holds everything those steps read and write. A v16 could only come
from a change to S5c's own Session records, not from Session files.

**Host output is a second table at v29.** [ADR 013](../../adr/013-command-output-ownership.md)
adds `session_output_owners(session_id, path)` and its index for command output, offloaded `read`
results and shell logs. It is not a Session-file table: those paths have no `session_files` row or
id, and a directory is removed by the delete of its last owning Session. A fork copies its rows
like `session_file_owners`, so output stays readable after the source is deleted.

## 9. Interface

### 9.1 Language-neutral (ADR 002 D5)

`sessionFiles`, process scope, no instance key, provided by SessionStore.

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `put` | async | `{sessionId: string, name: string, content: string}`, where `content` is the bytes in base64 | `{fileId: string, path: string, bytes: u64}` |
| `register` | async | `{sessionId: string, path: string}` (ADR 013) | `{}`; a missing Session is `not_found` |
| `owns` | async | `{sessionId: string, path: string}` | `{owned: bool}` |
| `owned_paths` | async | `{sessionId: string}` | `{paths: string[]}`, a Session file's directory and each output path |

- `path` is absolute on the Session's resource node, the way `read` resolves it for the Session.
  `bytes` is the stored length.
- `register`, `owns` and `owned_paths` are ADR 013's ownership interface. `register` resolves the
  path and records it before the caller hands it to the model; `owns` answers from both ownership
  tables, and ADR 014 reads it as the session-owned fact for writes.
- Cancellation is a protocol action: the caller drops the call or sends the bridge's cancel
  request. It takes effect up to txn 2. After that the file is committed and the reply stands.
- Errors are data, `{code, message}` (§10).
- SessionStore offers no operation that changes, renames or removes one file. Sharing and
  release happen only inside S5c's fork and delete transactions, which are SessionStore's own
  Rust steps, not service methods.

### 9.2 How Attachments uses it

Attachments ([A.2](../plugins/Attachments.md#a2-attachmentput)) declares `S:SessionIdentity` and
the process service `sessionFiles` in its `Deps`: an enclosing-scope dependency on a core part
(D3). A SessionStore reload reloads Attachments, which its rule check already accepts. For
`attachment.put` it calls `put(sessionId = its own Session, name, content = the text's UTF-8
bytes)`. It replies `{attachmentId: fileId, path, bytes}`. Attachments names no SessionStore
type; the Rust binding is Appendix A.

| `sessionFiles` error | `attachment.put` result |
| --- | --- |
| `not_found` | `session_closed` |
| `invalid` | `attachment_invalid` |
| `too_large` | `attachment_too_large` (Attachments' 10 MiB limit is below SessionStore's, so this does not occur) |
| `node_unavailable` | `node_unavailable` |
| `cancelled` | `service_reloading`: nothing was stored, and the call is `drain`, so it replies with this result |
| `storage` | `storage_failed`, as SessionTitle maps the same failure (§15) |

## 10. Errors, limits and immutability

| Code | When | Kept |
| --- | --- | --- |
| `not_found` | No Session record with that id, or it was deleted before txn 2. The same code as SessionStore's metadata service ([SessionTitle](../plugins/SessionTitle.md)) | Nothing owned; an abandoned directory is swept |
| `invalid` | `name` breaks the rule below | Nothing |
| `too_large` | Content over 16 MiB | Nothing |
| `node_unavailable` | The FileStorage of the Session's resource node is not reachable. On the control node this means FileStorage is reloading or closed. Retryable | Nothing owned; an abandoned directory is swept |
| `cancelled` | Cancelled before txn 2, including SessionStore unloading and a later generation's sweep claiming the pending row | Nothing owned; swept |
| `storage` | SQLite or FileStorage failure, including `Unsynced` (durability unconfirmed) | Nothing owned; swept |

Limits, and who owns them:

| Limit | Owner | Reason |
| --- | --- | --- |
| One file at most 16 MiB | SessionStore | A put is one message, and `replace` holds the content in memory; the bound caps one call's memory |
| `name`: `[A-Za-z0-9._-]{1,64}`, no leading dot, not `auth.json` or `auth.json.lock` in any letter case, and accepted by the node's FileStorage as one path component (Windows device names) | SessionStore | The name becomes a path component under SessionStore's directory. `auth.json` would switch `replace` to FileStorage's account-marker protocol |
| Content rules (non-empty text, 10 MiB, default `paste.txt`) | Each consumer (Attachments A.2) | Business rules of the feature |
| No per-Session count or total quota | None | Disk use ends with Session deletion and cleanup (S5c); a quota would need a business owner, and none exists |

**Immutability.** A path names the same bytes for the file's whole life, which ends only when
the sweep removes an unowned file. The file is written once, read-only: Unix mode `0400`,
Windows read-only attribute, through `replace`'s permissions parameter. WorkspaceTools' `write`
and `edit` therefore fail with `permission_denied`: their commit step opens the target for
writing, and on Windows also checks the read-only attribute, before replacing it. This is a
guard, not isolation. A daemon running as root on Unix is not stopped by the mode, a shell
command can still change the file, and FileStorage is a trusted host storage boundary, not
isolation from same-user processes. Moving the database directory breaks the absolute paths in
history, as moving a workspace breaks paths into it.

## 11. Security

- Files are written only under `<db>.session-files/<fileId>/`, never at a caller-chosen
  location; `name` is one validated component. Directories are `0700` and files `0400` on Unix;
  Windows inherits the parent's DACL.
- The directory belongs to one database, so one store's sweep never removes another store's
  files, even when two databases share a directory.
- Contents are user data. They appear in no diagnostic, row or event; diagnostics carry the
  file id, node and error only.

### 11.1 Reading a Session file

**Decision: the model reads a Session file the way it reads any file. A Session-owned artifact
is one of the fixed facts, so its read passes file scope without recording a grant. Tool approval
is independent: explicit Ask can still ask for that read.** The
[Permission contract](../../architecture/rust-agent/plugins/Permission.md#policy-contract) owns
the two gates and legacy behavior; ownership is not an explicit tool allow.
[ADR 014](../../adr/014-file-access-facts.md) decides file reads and writes from the workspace,
system roots, Session-owned artifacts and grants; [ADR 013](../../adr/013-command-output-ownership.md)
adds the second ownership table, so `owns` also answers for command output and a read of it is
allowed under `read-only` and `workspace-write` without an approval.

```
 model: read {path: "<database directory>/<db>.session-files/<fileId>/paste.txt"}
   |
   v
 ToolExecutor -> Approval.admit -> Permission.decide(tool "read", risk "read_filesystem", path)
                                   resolve (~, cwd, .., symlinks), then fileAccessFacts(read)
   "read" in disallowedTools ---------------------------> Deny, for every path
   Read only or Workspace read/write, inside facts -----> pass file scope
   Read only or Workspace read/write, outside facts ----> scope Ask
   Unrestricted ----------------------------------------> pass file scope
   tool gate -------------------------------------------> Ask unless allowed or bypassed;
                                                         legacy risk behavior if override absent
   scope Ask + tool Ask --------------------------------> one approval (exec denies and exits 3)
   |
   v
 WorkspaceTools read: the same canonical target, refuses binary files
```

The admission contract, owned by [permission.rs](../../../crates/dim-agent-capabilities/src/permission.rs)
`decide`, [file_access.rs](../../../crates/dim-agent-capabilities/src/file_access.rs) and
[approval.rs](../../../crates/dim-agent-capabilities/src/approval.rs) `admit`:

- **An owned artifact needs no scope prompt.** `SessionFiles.owns` reports the Session's attachment
  rows and ADR 013's output rows, so the facts' `owned` field is true for a path a `put`
  returned or a producer registered. Under `read-only` and `workspace-read-write` file access
  it is inside the facts. Reading it records no grant; explicit Tool approval Ask still asks
  unless the tool is allowed. §11.2's two properties hold for the rows `owns` reads.
- **Reads outside the facts ask.** A read whose resolved path is not the workspace, a system
  root, a Session-owned artifact or a grant asks under Read only and Workspace read/write file access
  ([ADR 014 D2/D4](../../adr/014-file-access-facts.md#d2-read-and-write)). The CLI offers "Allow
  for this session", which writes a read grant for the resolved path; `exec` answers with the
  ordinary denial, stops the turn and exits 3. Unrestricted passes read scope, not the tool gate. The
  path check runs before the tool lists and `allowedTools`, so an allowed tool does not bypass a
  path fact. A `grep` or `glob` without a `path` searches the Session cwd, which is the
  workspace.
- **A grant.** A read grant or write grant satisfies read scope for the resolved path; it cannot
  answer explicit Tool approval Ask or turn a read grant into write authority. A policy change
  (`session.permission.update`, written as `permission/changed`) holds policy values, never a
  path. The FileStorage root and remembered decisions confer no read authority.
- **The FileStorage root confers nothing.** Registering the database directory as a FileStorage
  root (§3.1) lets SessionStore write there through FileStorage. `read` consults no FileStorage
  root; the Session-owned fact is `sessionFiles.owns`, not file-root authorization.
- **Not new.** WorkspaceTools already writes oversized `read` results and `exec` output to
  `<database directory>/command-output/<sessionId>/` and gives the model that path
  ([read.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/files/read.rs)
  `offload`). In the product profile both directories sit next to `dimcode.sqlite`. ADR 013
  registers those paths, and the persistent shell's `.dim/exec/<sessionId>/<branchId>/` logs,
  in `session_output_owners` before the result hands them to the model. Registration is a guard
  among trusted parts, not a read rule: `read` still asks nothing for these paths.

```
 ~/.dimcode/                            product home: auth.json and others
   rust/                                database directory, a FileStorage root
     dimcode.sqlite                     native store; read refuses .sqlite as binary
     command-output/<sessionId>/        WorkspaceTools output; owned by this Session
     dimcode.sqlite.session-files/
       <fileId S owns>/paste.txt        path returned by a put for S
       <fileId T owns>/notes.md         Session T's file, unowned here
```

**What Permission does not do.** The facts decide tool admission, not isolation
([Permission](../../architecture/rust-agent/plugins/Permission.md), ADR 001 "Trust and
cross-language calls"). They do not scope process mounts (ADR 010 and ADR 012 own them) or
prevent a concurrent filesystem change after a path is resolved. S's model can still reach T's
file above by asking to read it and having the user approve, by a grant the user writes, under
`full-access`, or through a process whose mounts allow it. `read` refuses `dimcode.sqlite` only
because its binary guard rejects the `.sqlite` extension, not because of these facts. S5a does
not widen this boundary. It does not narrow it with an exception for its own directory either,
since the generic ownership fact covers the paths a `put` returns. What S5a guarantees by
construction:

- Every path `put` returns is `<db>.session-files/<fileId>/<name>`, with `fileId` a random UUID
  v4 and `name` one validated component. No returned path names the database, its companions
  or any other entry of the database directory.
- SessionStore returns a path only to a `put` for the Session that then owns the file, and a
  fork owns every file its source owned, so the paths its inherited history got from `put` stay
  owned (§4).

### 11.2 Requirement for path-scoped file access

Whichever mechanism confines in-process file access by path, for tools or for processes, must
keep the two properties below. [ADR 014](../../adr/014-file-access-facts.md) implements them for
file tools: it resolves `read`, `grep` and `glob` before admission and consults
`SessionFiles.owns`. F2 ([#73](https://github.com/arcships/dim/issues/73)) covers command rules,
and F5 ([#76](https://github.com/arcships/dim/issues/76)) isolates processes at start. They are a
generic file-access need, not a Session-files exception: WorkspaceTools' command output has the
same shape, data a Session's own capabilities hand to its model by path, outside the workspace.

1. **Owned files need no scope approval.** Ownership satisfies read scope without a recorded
   grant; it does not answer an independent tool-approval question (§11.1). No grant is derived
   from such a read.
2. **Ownership, not location, decides.** Session S may read `<db>.session-files/<fileId>/` for
   each file S owns in SessionStore's rows when access is decided, or when an isolated process
   starts. The set never covers `<db>.session-files/` as a whole, the database directory or the
   product home, and it includes another Session's file only if S owns it (a fork owns its
   source's files).

SessionStore answers "which files does S own" from both ownership tables (ADR 013): a Session
file from `session_file_owners`, whose primary key `(session_id, file_id)` serves that lookup,
and registered command output from `session_output_owners`. `SessionFiles.owns(session_id, path)`
is that query; [ADR 014](../../adr/014-file-access-facts.md) reads it as the session-owned fact
for reads and writes. `owned_paths(session_id)` lists the owned Session-file directories and
output paths.

Rejected alternatives:

| Alternative | Why not |
| --- | --- |
| Register the Session-files directory as a read-only root that `read` accepts | `read` consults no roots, so nothing would change today, and file-root authorization may confer no tool authority (ADR 003 D5, AGENTS.md rule 11). A root also covers the whole directory, every Session's files |
| Attachments puts the text in context, or a dedicated tool reads by `fileId` | Inlining puts up to 10 MiB into context at submit, which Attachments exists to avoid. A second read tool duplicates `read` (offsets, encodings, offload) while the path stays readable by `read` |
| A Permission rule "a Session may read its own files" now | A Session-files case inside a path-agnostic Permission, which would exempt nothing else. Its generic form is §11.2's ownership fact, implemented by [ADR 014](../../adr/014-file-access-facts.md) |
| `read` refuses the database directory except Session files | An exception in WorkspaceTools for one App Server directory. It covers `dimcode.sqlite`, already refused as binary, but not `auth.json` or the rest of the product home |

## 12. Timing and conflicts

| Scenario | Outcome | Decided by |
| --- | --- | --- |
| Put, fork and delete races | §6 table | Native store write order |
| Put on a closed, not deleted, Session | Stored: the record exists. Attachments itself unloads with the Session | — |
| Two puts on one Session | Two independent files | — |
| Node offline at put | `node_unavailable`; nothing stored; nothing reloads | SessionStore |
| Node offline at fork | Unaffected: no file is touched | Native store |
| Node offline at delete | The Session-file step commits without the node; unowned rows wait for a sweep. The delete as a whole: [deletion and fork §11](session-deletion-and-fork.md#11-resource-nodes-p3) | Sweep |
| FileStorage reloads during a put | `node_unavailable` or `storage`; abandoned and swept | SessionStore |
| SessionStore reloads during a put | Cancelled before txn 2 (abandoned), or already committed; Attachments reloads with it | SessionStore unload |
| Process restart | Leftovers are swept on the first put or after the next delete | Sweep |
| Something outside SessionStore writes into the Session-files directory | A top-level entry is removed by step 3 of the sweep; an entry inside a file's directory goes with that directory when the file is unowned. `Locked` leaves it for the next sweep | FileStorage, sweep |
| `read` after the last owner is deleted | File not found; no surviving Session quotes the path | — |

## 13. Changes to other documents

| Document | Change in this PR |
| --- | --- |
| [Attachments](../plugins/Attachments.md) | Layout `<db>.session-files/<fileId>/<name>` with `attachmentId = fileId`; A1 refined to a directory no single Session owns; fork and delete ordering rows point here; the offline rows no longer depend on the withdrawn #5, because SessionStore looks FileStorage up per call (#38). The client contract of `attachment.put` is unchanged here; its `storage_failed` code lands in P2's design review (§15). §7 points to §11 for how reads are admitted, instead of saying a fork reads only the files its history references |
| [Overview B.2](../overview.md#b2-app-server-process-plugins) | FileStorage row gains directory creation |
| [Native storage format](native-storage-format.md) | Planned v15 decision |

S5c builds `session.fork` and `session.delete` on §5.2, §5.3 and §5.4 and covers them through
RPC. S5a implements those steps and tests them at SessionStore's Rust API.

## 14. Rule check

| Rule | Answer |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | Session files are SessionStore's process-level state without a Timeline. Callers change them only through its typed `put`, and fork and delete are SessionStore's own steps. SessionStore changes no other module's state; it uses FileStorage's public operations. |
| Principles §4 rule 2 (handover) | Not applicable: nothing is handed over. A path reaches a branch only inside user input a client writes. |
| Principles §4 rule 3 (before callbacks only) | No before callback. Name and size checks are the owner's own feature checks at its service, not entry checks; Rpc is unchanged. |
| Principles §4 rule 4 (no business meaning in core) | Timeline, Dispatcher, before and batches are untouched. |
| Principles §4 rule 5 (batch) | Not applicable: no event. |
| Principles §4 rule 6 (context from handed-over events) | The model sees a path only in adopted user input, and file content only in a `read` result. |
| Principles §4 rule 7 (public events and services only) | Attachments uses `sessionFiles`; SessionStore uses FileStorage's public operations and reads no plugin's internals. |
| Principles §4 rule 8 (requirements to mechanism owners) | No change to the Timeline, Dispatcher, event format, rutis, assembly or the Session facade, and nothing raised to rutis. The format change belongs to SessionStore's own store and is recorded as a format decision. `create_directory` is a FileStorage supplement inside its own module, agreed in the S6 review. Permission admits file-tool reads and writes through the fixed facts (§11.1); §11.2 is the generic requirement it implements, not a Session-files exception. |
| Principles §4 rule 9 (only facts later decisions use) | No Timeline event. Ownership is process-level state in SessionStore's rows. |
| Principles §1 (Capability and App Server boundary) | Session files are SessionStore data (overview B.2); no new plugin. The paste feature stays in Attachments' own crate. |
| ADR 002 D1 (scope follows state) | Process scope: a file outlives the Session that put it and is shared across Sessions. |
| ADR 002 D2 (no wiring modules) | `sessionFiles` is SessionStore's own service; no adapter or registry. |
| ADR 002 D3 (dependencies follow scope) | Attachments (optional, Session) depends on SessionStore (core, process); SessionStore depends on no optional plugin and looks FileStorage up per call by resource node, so a FileStorage reload reloads nothing. |
| ADR 002 D4 (structural vs business configuration) | No configuration: the location and limits are fixed. |
| ADR 002 D5 (language-neutral contracts) | §9.1 is the contract; Appendix A is one binding. |
| ADR 003 (approval scope) | Reading a Session file is a tool call, admitted by Approval and Permission like any `read` (§11.1). The FileStorage root SessionStore registers confers no authority (D5). |

## 15. Requirements raised to mechanism owners

None to the runtime or rutis. Supplements and requirements to other owners:

| To | Requirement | Status |
| --- | --- | --- |
| FileStorage | `create_directory` (§7) | Added in S5a's implementation, agreed in the S6 review |
| App Server assembly (`serve` in lib.rs) | Register the database directory as a FileStorage root when it is not one, and pass its index to `SessionStorePlugin` (§3.1) | A few lines in S5a's implementation, appended after the assembly lane's work like other registrations |
| Attachments (P2 design refinement) | Map `sessionFiles` `storage` to an `attachment.put` code. Attachments lists none today; SessionTitle maps SessionStore `storage` to `storage_failed` | Decided: `storage_failed`, recorded in [#66](https://github.com/arcships/dim/issues/66); applied in P2's design review |
| S5c | Delete owner rows inside the delete transaction, after closing the Session and before `agent_sessions`; copy owner rows inside the fork transaction; schedule a sweep after a delete | Contract for S5c (§5.2, §5.3) |
| The item that first scopes file access by path (F5 [#76](https://github.com/arcships/dim/issues/76) for processes; [ADR 014](../../adr/014-file-access-facts.md) for in-process file tools) | §11.2: a Session's owned files need no scope approval; ownership decides, never the Session-files directory as a whole, the database directory or the product home. Tool approval remains independent | S5a needed none. ADR 014 reads `sessionFiles.owns` for reads and writes; §11.1 admits a file-tool read by path |

## 16. Acceptance (flow tests)

1. `put` returns an absolute path `<database directory>/<db>.session-files/<fileId>/<name>`. The
   bytes read back equal the content, the file is read-only and its directory is `0700` on Unix.
2. A second Session shares the files through the fork step. Releasing the source keeps the file
   (same file identity, no bytes copied); releasing the second removes it after the sweep.
3. `put` on an unknown or deleted Session: `not_found`; no owner row; the directory, if created,
   is swept.
4. A pending row of another generation, with its directory, is swept on the first put of a new
   generation, and so is a directory with no row. The Session-files directory of a second
   database in the same directory is untouched.
5. FileStorage not active: `node_unavailable`; nothing written; SessionStore and Attachments
   stay loaded.
6. Cancellation before txn 2: nothing owned; the directory is swept. An invalid name (including
   `AUTH.JSON`) or content over 16 MiB: `invalid` or `too_large`; nothing written.
7. An empty store is created at v15. A disposable v13 store upgrades in place to the exact v15
   schema with every business row unchanged, then reopens as v15. The upgrading open reports
   the upgrade, which the §8 log line comes from; the reopen does not. The branch-only v14 and unknown
   schemas are refused and preserved.
8. FileStorage flow: `create_directory` creates `0700` and syncs the parent; an existing
   directory succeeds; a missing parent fails `NotFound`. Reserved names, a case alias of the
   reserved directory, and a case alias of `auth.json.lock` (on a case-insensitive filesystem)
   fail `InvalidPath` and create nothing. An existing file or link is refused and untouched.
   Cancellation before creation creates nothing.
9. Read admission, through a Session's tool path: after `put`, the Session's `read` of the
   returned path, under the product default `workspace-write` and under `read-only`, returns
   the content; the branch writes no `approval/request`, and its permission state (version and
   policy) is unchanged. Denying another Session's file or the database directory is acceptance
   of §11.1's outside-facts read ask: neither path is in the Session's facts.

Tests: items 1, 3 (unknown Session), 5, 6 (names, size, a cancelled call), 4 and 9 are flow
tests through `sessionFiles` in
[session_files_flow.rs](../../../crates/dim-app-server/tests/session_files_flow.rs); 2, and 3 and
6 between the write and txn 2, run S5c's fork step and a hook at that point in
[files/tests.rs](../../../crates/dim-app-server/src/storage/files/tests.rs); 7 is
`a_v13_store_upgrades_in_place_once`, `the_daemon_reports_a_v13_upgrade_on_stderr_once` and the
refusal cases in [storage_flow.rs](../../../crates/dim-app-server/tests/storage_flow.rs); 8 is
`directory_creation_stays_inside_the_authorized_root` in
[file_storage_flow.rs](../../../crates/dim-agent-capabilities/tests/file_storage_flow.rs).

Verification: `./dev check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, and `cargo run --locked -p xtask -- graph` then `check`,
because SessionStore provides a new service.

## 17. Design changes in implementation

Changes the implementation made to this design ([roadmap §5.3](../roadmap.md#53-实现中发现的变更)). Each affects
only S5a.

| # | Change | Why |
| --- | --- | --- |
| D1 | `create_directory` ensures the key for `auth.json.lock` and only opens the key for the new name, instead of opening both keys with creation (§7) | Opening a key with creation leaves a key file named after the directory, and FileStorage unlinks keys only when it removes their parent. `<db>.session-files` is never removed, so every put would have left one key there for good. An existing alias of the marker still resolves to the marker's key, so the check gives the same answer |
| D2 | Before P3, a put reads no resource node. The Session's existence is decided by txn 1, after the FileStorage lookup, so an unknown Session gets `node_unavailable` while FileStorage is not active (§5.1) | `SessionConfig.resourceNode` does not exist yet, so a read before txn 1 would only repeat txn 1's check. P3 adds the node read before the lookup |
| D3 | Every put schedules a sweep until one sweep of the generation has completed on its node, instead of only the first put (§5.4) | A first sweep stopped by an unavailable node would otherwise wait for an abandon or a release to retry; a completed sweep ends the extra requests |

## Appendix A. Rust binding

In `dim-agent-capabilities::host`, next to the other host ports, so Attachments depends on no
App Server type:

```rust
pub struct StoredSessionFile { pub file_id: String, pub path: PathBuf, pub bytes: u64 }
pub enum SessionFileErrorCode { NotFound, Invalid, TooLarge, NodeUnavailable, Cancelled, Storage }
pub struct SessionFileError { pub code: SessionFileErrorCode, pub message: String }
pub trait SessionFilesService: Send + Sync + 'static {
    fn put(&self, session_id: String, name: String, content: Vec<u8>, cancel: CancellationToken)
        -> Pin<Box<dyn Future<Output = Result<StoredSessionFile, SessionFileError>> + Send + '_>>;
    fn register(&self, session_id: String, path: PathBuf)
        -> Pin<Box<dyn Future<Output = Result<(), SessionFileError>> + Send + '_>>;
    fn owns(&self, session_id: String, path: PathBuf)
        -> Pin<Box<dyn Future<Output = Result<bool, SessionFileError>> + Send + '_>>;
    fn owned_paths(&self, session_id: String)
        -> Pin<Box<dyn Future<Output = Result<Vec<PathBuf>, SessionFileError>> + Send + '_>>;
}
pub struct SessionFilesHost(pub Arc<dyn SessionFilesService>);
```

`register` stores a resolved output path; the producer has checked it against its own root.
`owns` takes the resolved absolute path and reads both ownership tables: an exact
`session_output_owners` row, or the `session_file_owners` row of an attachment whose path lies
in `<db>.session-files/<fileId>/`. A query failure is an error that denies access.
`owned_paths` lists the owned file-id directories and output paths.

`SessionStorePlugin` takes the database directory's root index, provides `SessionFilesHost`, and
reads `FileStorage` with `ctx.get_as` on each call. The fork, release and sweep steps are
SessionStore methods over the native store connection, used by S5c.

FileStorage:

```rust
impl FileStorage {
    pub async fn create_directory(self: &Arc<Self>, path: &FilePath, cancel: &CancellationToken)
        -> Result<()>;
}
```
