# FileStorage

Owner: [file_storage.rs](../../../../crates/dim-agent-capabilities/src/file_storage.rs)
and [windows.rs](../../../../crates/dim-agent-capabilities/src/file_storage/windows.rs).
Provides authorized-root-relative file operations and leases with secure path
traversal and serialized mutation, plus directory creation, listing and removal.

## Dependencies

No injected services (`Deps = ()`).

## Events and lifetime

No Timeline events. Each `apply` opens fresh root-directory handles; blocking
operations and leases are tracked by that generation. Close stops admission,
drains operations/leases and releases resources. Retained old service handles
reject new operations. Clone the plugin to preserve append-only authorized-root
indices across reload; rebuilding from the original list could reuse indices.
Cancellation is checked before commit, not treated as rollback after installation.
Roots reject symlinked traversal; this is a trusted host storage boundary, not
isolation from the host or same-user processes. Do not automatically retry an
uncertain committed mutation.

## Leases and lock keys

A lease locks a key file with the target's name in the reserved
`.dim-file-storage-locks` directory beside the target, so the filesystem's own
case and Unicode aliases of one name share one key. Releasing a lease keeps its
key; only directory removal unlinks keys, one at a time while holding each. A
lock counts only if its key is still linked once held. A key linked when locked
stays linked while held, because only its holder unlinks it. A key unlinked
before it was held is stale: the current key is resolved once more, and a
second stale key returns `Locked`. Without that check, a lock racing a failed
removal admitted a second holder:

```text
B: lock(d/x)              remove(d)                    C: lock(d/x)
open key x (inode 1)
                          lock, unlink every key,
                          rmdir the lock directory
                                                       recreate the lock directory,
                                                       create and lock key x (inode 2)
                          rmdir d fails, release keys
lock inode 1              -> before: B and C both hold d/x
                          -> now: inode 1 is unlinked, so B reopens key x,
                             finds inode 2 held and returns Locked
```

A replacement writes its temporary file in
`.dim-file-storage-locks/.dim-file-storage-temps` under its key's name, so only
that key's holder writes or removes it. A leftover from an interrupted
replacement is discarded by the next replacement of that name.

`auth.json` also takes the TypeScript-compatible `auth.json.lock` marker, which
is reclaimed only while holding the `auth.json` key. A marker is stale once it
is older than 30 seconds and names a process that is no longer running; a
marker naming this process counts as live. The exception is a marker this
process failed to release: it keeps the owner record, whose nonce only that
marker carries, and reclaims the marker at once, so removal and locking need
not wait for the process to exit.

## Directory creation

`create_directory` makes a path below an authorized root an existing business
directory, one level at a time: its parent must exist and is reached as every
operation reaches it, without following links. A missing directory is created
relative to the held parent handle, with mode `0700` on Unix (`mkdirat`) and
the parent's inherited DACL on Windows (`NtCreateFile` with `FILE_CREATE`). An
existing business directory counts as created, so a retry is safe; any other
existing entry (a file, link, reparse point or special file) fails with an I/O
error and is left untouched.

Aliases are checked the way `lock` checks them, before anything is created. The
parent's reserved directory is ensured first, so a name the filesystem equates
with it is recognized by identity. Then the key for `auth.json.lock` is ensured
and the key for the name is opened if it exists; if they are the same file, the
name is an alias of the account marker. Both fail `InvalidPath`. The name's key
is never created, so making a directory leaves no key behind in a parent that
may never be removed, such as SessionStore's Session-files directory.

```text
create_directory(d/x):
  open d (no links, never the reserved directory)       missing -> NotFound
  ensure d/.dim-file-storage-locks
  x is the reserved directory under another spelling?    -> InvalidPath
  ensure key auth.json.lock; key x exists and is it?     -> InvalidPath
  cancelled?                                             -> Cancelled
  mkdirat(d, x, 0700); an existing entry is kept
  x is not a business directory?                         -> Io, untouched
  fsync d                                                fails -> Unsynced
```

In both the created and the existing case the parent is synced before the call
returns, so a retry after `Unsynced` confirms durability. Windows has no
directory sync, as for replacement. Cancellation is checked up to the creation.

## Listing and removal

`list` returns the entries of a directory below an authorized root, sorted by
name, each a `File`, `Directory` or `Other` (a link, reparse point or special
file, never followed). FileStorage's lock, temporary and account-marker
resources are omitted, including names the filesystem equates with them.

`EntryKind` serializes as `file`, `directory` or `other` for generated exports.
Native entry names remain `OsString`; the exporting boundary chooses its UTF-8 policy.

`remove` makes an entry absent. Files, including those beneath a directory,
are removed under the lock leases use, so removing a leased file, or a directory
with a lease held anywhere beneath it, returns `Locked`. A link is removed
itself. A directory is emptied depth first through directory handles, without
following links or entering a filesystem mounted at or inside it (a Linux bind
mount of the same filesystem is not detected). An explicit stack replaces
recursion, so depth costs no thread stack. On Unix, removal holds one directory
descriptor at a time: it returns to each parent through `..` and fails if that
is no longer the directory it descended from, or starts again from the target if
a concurrent removal deleted it. Windows holds a handle per level, since it
cannot reopen a parent from a handle without a path.

Before emptying a directory, removal clears the leftovers that would stop
every file in it from being locked: a non-directory in place of the lock
directory or of the temporary directory inside it, and in the lock directory
any link, or any directory other than the temporary one, such as a directory
in place of a key. Keys are regular files and no `FilePath` reaches inside the lock
directory, so no lease holds these. The one exception is a regular file in
place of the temporary directory, which can be a key, so it goes under its
lock. Trees there are removed without locks, entering directories with
reserved names as well, but never another filesystem. The temporary
directory, however it is spelled, is never taken for such a tree: that is
checked on the handle the removal walks, so a temporary directory created
meanwhile is not either.

Then the directory's other leftovers go first: entries with a reserved name,
or with the identity of the lock directory or the account marker but another
type. Examples are a resource from an earlier version and a marker that is a
directory or a link. They follow the business rules, since on a
case-insensitive volume an alias of a reserved name can be leased. Going first
means a stray marker cannot block locking `auth.json` beside it. A file whose
key the filesystem equates with the temporary directory, such as a file named
`.dim-file-storage-temps`, or `.DIM-FILE-STORAGE-TEMPS` on a case-insensitive
volume, cannot be locked, so no lease holds it; it is unlinked without a lock
unless it is the account marker under another name. The directory's FileStorage resources go last and hold one
key at a time, however many names the directory has used:

```text
remove(d), per directory, before its entries:
  a non-directory in place of .dim-file-storage-locks  -> unlink it
  in .dim-file-storage-locks:
    a file in place of temps                           -> lock it as a key, unlink
    any link                                           -> unlink it
    any directory but temps                            -> remove it as a tree
remove(d), once d's other entries are gone:
  auth.json.lock: holding key auth.json, reclaim it if stale; live -> Locked
  for each key k in .dim-file-storage-locks:
    lock k                                             held -> Locked
    k unlinked by a concurrent removal?                -> skip it
    remove temps/k (a directory as a tree), unlink k, release k
  for each temps/t still left (written under a key taken since,
  or named at random by an earlier version): the same, under key t,
  or without a key if key t would be temps itself
  rmdir temps, rmdir .dim-file-storage-locks, rmdir d
                    an entry or key created meanwhile -> DirectoryNotEmpty
remove(p), once p is absent (also when it already was):
  fsync p's parent                                     fails -> Unsynced
```

A link or special file is unlinked only while it still is one. If a key or
file has replaced it since it was listed, removal fails instead. Windows
checks the type through the handle that deletes the entry; Unix checks it
just before unlinking, which narrows the race without closing it.

Locking and replacement never delete leftovers. A non-directory in place of
the lock or temporary directory, or a directory in place of a key or a
temporary file, makes `lock` or `replace` fail with an I/O error, without
following or writing through it, until the directory is removed.

Removal therefore never unlinks a key or temporary file that a holder has
locked. An absent entry counts as removed. Removal is not atomic: an error or
cancellation can leave part of a tree, retrying is safe, and an owner that must
not leak files after a crash reconciles by listing. Once the entry is absent,
removal syncs its parent before it returns, also when the entry was already
absent, so a completed removal survives a crash and a retry after `Unsynced`
confirms durability; an owner can order a commit after it, as SessionStore's
Session delete does ([S5c](../../../design/appserver/session-deletion-and-fork.md#4-crash-convergence)).
Windows has no directory sync, as for replacement. The
owner orders removal after the directory's writers; entries or leases created
meanwhile make it fail. A replacement racing the removal of its directory still
commits: if removal deleted the empty temporary directory first, the
replacement creates it again. Roots, the reserved directory and the account
marker cannot be listed or removed.

On Windows, every reparse point is listed as `Other` and never entered. Removal
deletes a junction or symbolic link itself, but refuses a volume mount point
(a mount-point reparse point whose target is a `\??\Volume{…}` path) with
`CrossesDevices`, as Unix refuses a directory on another filesystem; Unix lists
such a directory as `Directory`. Listing and removal need a volume that reports
file IDs during enumeration, such as NTFS or ReFS. Elsewhere, such as on FAT,
exFAT or some network shares, they fail with an `Unsupported` I/O error rather
than list entries whose aliases of reserved names could not be recognized.

A directory deleted while an operation runs inside it has the same effect on
both platforms. Unix fails an operation in a deleted directory, or on a deleted
entry, with not found. Windows fails it with access denied, as it does a real
denial (`STATUS_DELETE_PENDING` or `STATUS_FILE_DELETED`). FileStorage then
checks whether the handle's directory or entry is pending deletion and, if
so, reports not found. A real denial in a live directory stays an error. A
file deleted while a lock resolves its stored name counts as missing, rather
than taking the name NTFS gives deleted files, and a key that fails to lock
once a removal unlinked it is stale on either platform. A lock racing the
removal of its directory therefore resolves its key again or returns
`Locked`, or not found once the directory is gone. A removal racing
another removal of the same tree treats what the other deleted as removed,
and starts again from its target if a directory it returns to was deleted.

## Tests

[file_storage_flow.rs](../../../../crates/dim-agent-capabilities/tests/file_storage_flow.rs):
`late_roots_preserve_indices_and_generation_boundaries_on_reload`,
`file_storage_flow` (path, lock, permission and platform checks),
`directory_creation_stays_inside_the_authorized_root` (mode, an existing
directory, a missing parent, aliases of the reserved directory and the account
marker, an existing file or link, cancellation),
`directory_listing_and_removal_stay_inside_the_authorized_root` (Session-file
layout, reserved resources and aliases, leases held during partial removal,
links, cancellation, leftovers with reserved names or of the wrong type, a
leased alias of a reserved name, stray entries in the lock and temporary
directories, a marker whose release failed) and
`removal_scales_with_directory_size` (a directory that used 300 names and a
1,000-level tree, removed under a 64-descriptor limit on a 256 KiB worker
stack, where recursion overflowed at a few hundred levels). The unit
test `lock_keys_stay_exclusive_across_concurrent_directory_removal` in the
owner module drives the lock, removal and temporary-file interleavings that the
public API cannot produce reliably.

<!-- generated:plugin FileStorage — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | — |
| 提供 | `P:FileStorage` |
| 被注入于 | `HostPlatform` |
<!-- /generated -->
