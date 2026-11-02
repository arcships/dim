# Restore validation reuse

Status: Draft for review ([#601](https://github.com/arcships/dim/issues/601)).
Scope: the restore validation pass of the native Session store
([`SqliteTimelineStore`](../../../crates/dim-app-server/src/storage/timeline.rs)) and the process
memory that decides whether to run it. It changes no event, service, capability, protocol,
configuration or storage format.
Related: the storage and IO audit [#588](https://github.com/arcships/dim/issues/588); the restore read
paths [restore-scoped shared history read](restore-shared-read.md) and
[TimelineTree's restore facts](restore-shared-read.md#17-timelinetrees-restore-facts); idle unload
[idle-unload.md](idle-unload.md); cold first attach [#713](https://github.com/arcships/dim/issues/713);
the latest-turn window [#712](https://github.com/arcships/dim/issues/712).

## 1. Summary

Every restore of an unloaded Session validates its whole stored Timeline. Idle unload closes a Session
after 60 s without a lease, so a Session used now and then pays the pass on every use, although this
process wrote nothing but its own atomic batches in between.

- **Rule.** A successful validation pass leaves, in process memory, the Session directory it checked.
  A later restore of the same Session in the same store reuses that result and skips the event pass
  while the stored directory is exactly the recorded one.
- **What the record is.** Every `timeline_branches` row — `branch_id`, `info`, `sequence` — in
  branch-id order. Equality of this directory is the condition; a changed branch head, a new or
  removed branch, or changed branch info is a miss. The Session config is read and parsed on every
  call, on both paths, but it is not stored: no supported writer changes it after create or fork.
- **When it is written.** After a successful validation pass, and again when a load releases the
  Session, after the writer drained that load's accepted batches. A failed pass never writes it; a
  failed refresh removes it.
- **What is still checked on a reuse.** The Session config parses; every branch row parses and matches
  its id; `main` exists and is the root; every child has a parent, a create anchor within that
  parent's head, a branch point, and no cycle.
- **What is skipped.** The orphan scan, every event decode with its kind, identity-index, trace,
  sequence, batch-boundary and head checks, each child's create event, and each completed
  compaction's `through` boundary. They were checked by the pass that wrote the record, and the
  process is the only writer while it holds the store.
- **First open.** The first restore of a Session in a process still validates fully, and a new store
  (restart, Agents reload) starts empty. Damage introduced while the process lives is detected by
  the next full pass — the next process, or a directory change — not by every restore.
- **Effect.** The validation pass of a re-acquisition is removed: the issue measured it at about
  14 µs per event on `product`, 30 of 69 ms at 5,008 events. Removing that pass is this change's
  acceptance; the rest of the restore — branch opening, capability rebuild, the `branch/resume`
  append, MCP and skill startup — is unchanged and belongs to [#713](https://github.com/arcships/dim/issues/713)
  and [#588](https://github.com/arcships/dim/issues/588).
- **Unchanged.** Storage format 30, schema, rows, frozen fixtures, protocol, services, the
  `TimelineStore` signatures, capability rebuild, export (which keeps its full pass), and the
  in-memory store.

## 2. Behavior today

### 2.1 The restore and its validation pass

A restore starts at `Agent::restore_session`
([agent.rs:120](../../../crates/dim-agent-runtime/src/agent.rs#L120)): it reads and parses the Session
config, then `Agent::create` receives `SessionStart::Restore` and calls `store.branches(id)`
([agent.rs:174](../../../crates/dim-agent-runtime/src/agent.rs#L174)) before it builds anything. For
the native store that is `validate_session`
([timeline.rs:3329](../../../crates/dim-app-server/src/storage/timeline.rs#L3329)):

```text
store.branches(id)  — one connection hold: the directory
  agent_sessions.config            must exist and parse as SessionConfig
  orphan scan                      every timeline_events row has a timeline_branches row
  timeline_branches                branch_id, info, sequence, ordered by branch_id
then, per branch, pages of 64 physical rows:
  decode every event and its request content (one bounded reader across the pass)
  check the row kind, the event-id index, the before trace's event id and kinds
  check sequence continuity, atomic-batch boundaries, and that the head is the final batch end
  collect every event id in one set (duplicates fail), and every completed compaction
then:
  every branch/create event names a directory row
  per child: read the create anchor's event and match parent and cut
  per completed compaction: read its through event, which must end a batch
  directory checks: main is the root; every child has parent, anchor and cut; anchors do not
  exceed the parent head; parent chains exist and have no cycle
```

The cost is the pass. The issue measured it with `restore_cost product` in a release build:
about 14 µs per event, 30 ms of a 69 ms restore at 5,008 events. Validation is one of the three
remaining parts of a restore after [#235](https://github.com/arcships/dim/pull/235) made the reads
share one decoded copy and [#239](https://github.com/arcships/dim/pull/239) took TimelineTree's scan
out of the pre-build path. It is also the part that repeats unchanged: the `branch/resume` append,
the capability rebuild and the rest of the restore happen after it and are outside this design.

Who calls `branches`:

| Caller | Session | Pass today |
| --- | --- | --- |
| `Agent::restore_session` (attach, `sessions` call, Collaboration, Scheduler) | unloaded stored Session | full |
| `Agent::create_prepared_session` (fork) | new Session copied from a source | full (its own rows) |
| Timeline export | any Session, read-only store, one shot | full, always ([export.rs:82](../../../crates/dim-app-server/src/storage/export.rs#L82)) |

`TimelineStore::branches` itself has no documented validation contract
([timeline_store.rs:138](../../../crates/dim-agent-runtime/src/timeline_store.rs#L138)); the full pass
is the SQLite implementation's boundary check, and the in-memory store computes the directory from its
own map.

Idle unload is what makes the repetition common: Agents closes a Session after 60 s without a lease
([idle-unload.md §2, §6.1](idle-unload.md#61-the-pass)), and the next `acquire`
([agents.rs:758](../../../crates/dim-app-server/src/agents.rs#L758)) restores it, validates it, opens
every branch and appends `branch/resume`. A Collaboration member asked again later, a Desktop thread
reopened after a minute, or a `sessions` call after the grace pays this every time.

### 2.2 Who can change a stored Session

The argument for reuse rests on the store's writer and on the database lease:

| Fact | Where |
| --- | --- |
| One process owns the database for writing: `SqliteStorage::open` takes a file lease, and a second open fails with "another process owns the SQLite database" | [sqlite.rs:446](../../../crates/dim-app-server/src/storage/sqlite.rs#L446) |
| The export opens read-only without a lease and only reads | [sqlite.rs:591](../../../crates/dim-app-server/src/storage/sqlite.rs#L591) |
| Appends go through the store's one writer thread, which commits each round in one SQLite transaction and only then publishes; a failed round is rolled back or fails the Session | [timeline.rs:227](../../../crates/dim-app-server/src/storage/timeline.rs#L227), [timeline.rs:446](../../../crates/dim-app-server/src/storage/timeline.rs#L446), [timeline-storage §3](../../architecture/rust-agent/timeline-storage.md#3-写入与发布), [§6](../../architecture/rust-agent/timeline-storage.md#6-故障) |
| A Session id has at most one live load per store, and callers serialize closing and restoring it | [timeline_store.rs:108](../../../crates/dim-agent-runtime/src/timeline_store.rs#L108) |
| `create_session` writes the Session row and the `main` directory in one transaction; `branch/create` writes the event and the child directory in the same batch | [timeline.rs:1932](../../../crates/dim-app-server/src/storage/timeline.rs#L1932), [timeline.rs:2784](../../../crates/dim-app-server/src/storage/timeline.rs#L2784), [timeline.rs:2193](../../../crates/dim-app-server/src/storage/timeline.rs#L2193) |
| `rollback_session` sends the delete; `rollback_creation` removes every row of a newly created Session, and `session.delete` uses it | [timeline.rs:2808](../../../crates/dim-app-server/src/storage/timeline.rs#L2808), [timeline.rs:900](../../../crates/dim-app-server/src/storage/timeline.rs#L900), [timeline.rs:1682](../../../crates/dim-app-server/src/storage/timeline.rs#L1682) |
| `release_session` drains the Session's prior appends and clears its failure state without deleting history | [timeline.rs:2832](../../../crates/dim-app-server/src/storage/timeline.rs#L2832) |
| `save_snapshot` writes `timeline_snapshots` outside the writer, but restore validation does not read snapshot state; `latest_snapshot` checks its anchor when it is read | [native-storage-format.md §可丢弃恢复快照](native-storage-format.md#可丢弃恢复快照) |
| `agent_sessions.config` is written only with the Session (create, fork copy); no runtime call updates it | [timeline.rs:1932](../../../crates/dim-app-server/src/storage/timeline.rs#L1932), [timeline.rs:1702](../../../crates/dim-app-server/src/storage/timeline.rs#L1702) |

So, within one store generation, every mutation of a Session's Timeline is a transaction this process
wrote, or a whole-Session create/rollback/delete. The directory is a complete summary of those
mutations: an append changes a branch head; a new branch changes its parent's head and adds a row;
delete removes the rows. While a Session is not loaded, this process has no writer for it at all.

### 2.3 What the pass protects against

The pass is the storage read boundary for a store written by earlier processes and for any damage
outside the supported writers — a partially copied file, a manual edit, a legacy format. That is why
the first open in a process must keep it. Repeating it for rows this process committed and has not
touched since does not protect against anything the lease and the writer do not already rule out.

## 3. Problem

A restore revalidates facts that cannot have changed since the pass that this process already ran:

- the rows are immutable once committed ([time bus §2](../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实), [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描));
- the only writer is this store, and it writes atomically;
- the Session is unloaded between the pass and the next restore, so no append can be in flight;
- the directory it validates is small and can be compared exactly.

The process therefore needs one piece of state, not a second store: the directory it last validated
for each Session, refreshed when a load closes. Re-acquiring a 5,000-event Session then costs the
restore without the pass.

## 4. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | The condition is exact equality of the branch directory: every `(branch_id, info, sequence)` row equals the recorded one; the config is parsed on every call and is not stored | Compare only `main`'s head, a timestamp, an event count, or the config text | A child branch created from a non-main parent advances only that parent's head; a removed or added row need not change `main`; info is part of what validation returns. The config cannot change through a supported writer and is re-parsed on both paths. The directory read is one indexed query and already exists in the pass |
| D2 | The record lives in `SqliteTimelineStore`, in process memory, keyed by Session id | A table, a file, a `SessionSlot` field, or a cache outside the store | It must outlive a load and die with the storage part: the store is the owner of validation and of the writer, and an Agents reload creates a new store, so the first open after a reload validates. A persisted record would need a format decision, invalidation and cleanup; a slot is removed when nothing holds it and is not available after an unload |
| D3 | The record is written after a successful pass, and refreshed at `release_session` only when one exists, after the writer drained that load's batches | Refresh at every commit; write it on create; refresh unconditionally at release | A commit is not a validation, and a created Session has not been validated; refreshing only an existing record means a failed pass can never seed one. Release is the point where the load's last accepted batch is committed and no writer remains. Refreshing at every commit would put directory state in the write path for no gain |
| D4 | A reuse still reads the directory and runs its checks; it skips the event pass and the orphan scan | Skip validation wholesale, including the directory read | The caller needs the parsed branches anyway, and the cheap checks keep the method's return contract: main is the root, parents and anchors are consistent, no cycle. The orphan scan and the per-event checks need the events, whose immutability is the premise of the reuse |
| D5 | A mismatch or any failure runs the full pass; a failed pass or a failed refresh removes the record | Keep the old record on failure | A stale record can only cause a later full pass, never a wrong skip; removing it makes the recovery rule one sentence. A failed refresh must not fail a clean close, so the refresh is best-effort and only costs the next pass |
| D6 | The map keeps one entry per Session that passed a pass in this process and removes it on create, rollback and delete; there is no eviction or size bound in this change | An LRU or a count bound | Each entry is the Session directory (the config text is not kept; branch rows are typically one and are small). It is far below a loaded Session, and a bound would only turn a reuse into a full pass — a later change can add one without changing the condition or the contract |
| D7 | Timeline export keeps its full pass | Let export reuse a daemon's record | Export opens its own read-only store, has no writer, and reports damage in the file it is exporting; a record from another store generation would be exactly the state it must not trust |
| D8 | The first open of a Session in a process validates fully; cold first attach stays with [#713](https://github.com/arcships/dim/issues/713) | Lazy, background or sampled validation on the first attach | The issue keeps the first open; a lazy pass would change when damage is detected and what `session.attach` may reply. #713 profiles the cold open and decides whether it needs a bound. This design removes only the repeated pass |

## 5. The condition, what is checked, what can go stale

### 5.1 The algorithm

`D(id)` is the branch directory read: the ordered `(branch_id, info, sequence)` rows. The same read
parses `agent_sessions.config`, on both paths, but the config is not part of `D`. `R(id)` is the
process record, absent or equal to a `D` read earlier.

```text
store.branches(id)
  D = read_directory(id)                      one hold: config parses; rows ordered by branch_id
  if R(id) exists and R(id) == D:
      return D.parse()                        branch rows parse; directory checks; no event read
  branches = validate_session(D)              the orphan scan and every event check, as today
  R(id) = D                                   after the pass succeeds
  return branches                             on failure: R(id) removed, the pass's error returned

release_session(id)
  the writer has committed every accepted batch of this load
  if R(id) exists:
      R(id) = D(id)                           best effort; a read failure removes R(id)
  release admission state, as today

create_session(id), rollback_session(id)      remove R(id) after the commit
store close / drop                            the map dies with the store
```

The map lock is held only around a lookup or an insert, never across a database call; the per-Session
gate and the store's one-load-per-id contract order `branches` and `release` for the same id, so the
two never race on one record.

### 5.2 Checked on a reuse

| Check | Source |
| --- | --- |
| The Session config exists and parses | `agent_sessions.config` in the directory read |
| Every branch row parses as `BranchRecord`, and `info.id` equals `branch_id` | the directory read |
| `main` exists, with no parent, branch point or create anchor | the directory |
| Every child has a parent and a create anchor within the parent's head, and a branch point | the directory |
| Parent chains exist and have no cycle | the directory |
| The directory is exactly the one the last pass validated | `R(id) == D` |

### 5.3 Skipped, and why that is safe

| Skipped check | Why it cannot have changed |
| --- | --- |
| The orphan scan (every event has a branch row) | Appends target an existing branch; `branch/create` and its directory row are one transaction; delete removes the whole Session. No supported writer creates an event without its directory |
| Event decode, row kind, event-id index, before trace | Committed rows are immutable; this process wrote them in atomic transactions |
| Sequence continuity, batch boundaries, head equals the final batch end | The writer commits each round in one transaction and refuses a non-contiguous sequence; the recorded head is the directory after its last round |
| Every `branch/create` event names a directory row; the child's anchor event matches parent and cut | Written in the same batch as the directory row; the directory equality covers all later branches |
| Completed compaction's `through` boundary | Part of the same committed events; immutable |

What can go stale is therefore the event rows and everything derived from them. The reuse accepts
that a change to them with an unchanged directory — an external edit to the database file while the
daemon holds it — is not detected until the next full pass. That is outside the supported mutations:
the lease admits one writer process, export is read-only, and the only in-process writer is the
store. The first open in the next process still detects all of it, which is the guarantee the issue
keeps.

### 5.4 Where the first open remains

| Situation | Pass |
| --- | --- |
| First restore of a Session after a daemon start or an Agents reload | full: the store is new and the map is empty |
| A Session created in this process, then restored after an unload | full: creation is not validation, so no record exists |
| A fork's prepared build | full: its own rows have never been validated; that pass records them, so a later restore reuses |
| Restore after the first successful pass, directory unchanged | reuse |
| Restore after this process appended to the Session (through a load) | reuse: release refreshed the record after the last commit |
| Directory changed while the Session was unloaded (a direct append to the store, a future unloaded writer) | full |
| Delete, then a new Session with the same id | full: delete removed the record and creation writes no new one |

## 6. Failure and recovery

| Case | Behaviour |
| --- | --- |
| The directory read fails at `branches` | The restore fails with that storage error, as today; the record is removed, so the next attempt validates fully |
| The record differs | The full pass runs. On success the record becomes the new directory; on failure the record is removed and the restore fails with the pass's error |
| The full pass fails | As today: the restore fails, the Session stays stored and bytes are preserved, and the next attempt validates again |
| The release refresh's read fails | The record is removed and the release still succeeds; the next restore validates. The refresh is not part of the release contract (admission state and the writer memo), and a clean close must not fail because an optimization read failed |
| The writer or store fails while the Session is loaded | The release refreshes to the committed directory; if the release cannot run (store closing), the record stays stale, and any later restore validates on the mismatch. A failed Session's rejected batches were never committed |
| The process crashes or the store closes | The memory is gone; the next process's first open validates |
| The Session is deleted, or a failed creation is rolled back | The record is removed with the rows; a reused id starts with no record |
| `branches` and `release` for one id overlap | The Session's admission gate and the store's one-live-load-per-id contract serialize them; the map lock is never held across I/O |
| An external edit with the daemon still running | Not detected unless it changes the directory; the next process's first open detects it. Accepted, and stated in §5.3 |

No error type or message is added. A reuse can produce the directory errors above; a miss produces
exactly today's validation errors.

## 7. Persistence, format and compatibility

None of the following changes:

- the format version (30), the schema, any table, column, index or row, and the frozen fixtures
  ([native-storage-format.md](native-storage-format.md));
- the protocol, a method, a reply field, an event kind, a service or a configuration value;
- `TimelineStore`'s signatures and its other implementations; the in-memory store keeps computing its
  directory from its own map and is not required to reuse anything;
- the runtime's restore, branch opening, capability rebuild, `branch/resume`, idle unload, attach,
  export and deletion flows.

The only contract text that changes is one sentence on `TimelineStore::branches`: an implementation
*may* reuse a successful pass while the stored directory and heads are unchanged. It is permission,
not a requirement, so the memory store and test doubles stay valid.

Compatibility is observable in one place: a Session damaged after its first successful open in a
process is not rejected by validation until the process restarts or its directory changes; damage the
build itself reads differently still fails the build. The issue accepts this by
keeping full validation on the first open; §5.3 states it. Nothing reads the record across processes,
so a downgrade or an older binary sees the database exactly as today.

## 8. Impact surface

| Module or content | Change | Reason |
| --- | --- | --- |
| `dim-app-server` `storage/timeline.rs` | `ValidatedDirectory`; `read_directory`; the map on `SqliteTimelineStore`; `branches` compares then reuses or validates; `validate_session` takes the directory and returns the branches; release refreshes, create/rollback remove | Validation is storage's own work |
| `dim-agent-runtime` `timeline_store.rs` | One doc sentence on `branches` allowing reuse | The port's callers must know the pass may have run earlier in the same store |
| `dim-app-server` `examples/restore_cost.rs` | Split the first restore of each Session from the re-acquisitions in the reported numbers | The measurement of §12; nothing is asserted |
| `dim-app-server` `tests/storage_flow.rs` | One flow for reuse and the head-change miss | The behaviour of §12 |
| `dim-agent-runtime`, capabilities, plugins, protocol, schema, fixtures, configuration, dependencies | None | §7 |

## 9. Rule check

This is not a plugin, capability or event design; it is the App Server's storage read boundary.
The applicable rules:

| Rule | Answer |
| --- | --- |
| [Principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 1 (command events) | Not applicable: no other module's state changes |
| §4 rule 2 (handover) | Not applicable: no handover |
| §4 rule 3 (`before`) | Not applicable: validation reads committed events, and `before` callbacks are unchanged |
| §4 rule 4 (no business meaning in core) | The record is keyed by Session and holds directory rows only; it reads no event kind |
| §4 rule 5 (atomic batches) | The condition includes every branch head, and the record is taken only at a validation pass or after the writer drained a load's batches; a batch is committed whole, so a head never exposes half a batch |
| §4 rule 6 (model context) | Unchanged: no context input, and the same events reach the rebuild |
| §4 rule 7 (public events and services) | No service or event; capabilities keep reading through the Timeline |
| §4 rule 8 (infrastructure) | Not a Timeline, Dispatcher or facade change. The one cross-owner item is the port doc sentence in §10 |
| §4 rule 9 (facts in the Timeline) | No new fact; the record is process memory of the storage, not history |
| §1 (Capability and App Server boundary) | Inside the App Server's storage part; no capability change |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | Process scope: the store owns the record for as long as it lives |
| ADR 002 D2 (no wiring modules) | No new module or plugin |
| ADR 002 D3 (dependencies follow scope) | No new dependency |
| ADR 002 D4 (configuration) | None |
| ADR 002 D5 (language-neutral contracts) | No public contract; the Rust port's doc gains permission, and no TypeScript part sees it |
| [ADR 008](../../adr/008-documentation-ownership.md) D3 (one owner per document) | This design owns the storage mechanism; it links the restore read path and idle unload instead of restating them |

## 10. Requirement raised to the runtime port owner

| Item | Content |
| --- | --- |
| Blocked by | Nothing. The requirement comes from the SQLite store's validation cost; the port already lets an implementation validate in `branches` |
| Existing mechanisms considered | The port's `branches` has no stated validation contract; the in-memory store validates nothing; a store wrapper or a runtime cache would place storage state outside the store and outlive its generation |
| Generic capability | A `TimelineStore` implementation may answer `branches` from a successful pass it ran earlier for the same Session while the stored directory and every branch head are unchanged; callers see the same branches and errors |
| Effect on sequence, batch, interrupt, reload | None. Signatures, errors and the returned rows are unchanged; a reload or restart starts with no reuse |
| Verification | The storage flow of §12; the runtime's own flows use the in-memory store and are unchanged |
| Owner and issue | Runtime port wording: core mechanisms lane ([roadmap §3.5](../roadmap.md#35-按人按批次)); [#601](https://github.com/arcships/dim/issues/601) |

## 11. Rejected alternatives

- **Persisting the validated head or a signature.** A table or side file needs a format version, an
  invalidation rule for every writer, cleanup on delete, and a downgrade story — for a value that is
  only usable while this process lives. It would also outlive the generation that could vouch for it.
- **Comparing only `main`.** A child branch created from another child advances only that parent's
  head; a removed branch need not touch `main`. The directory read is already required to return the
  branches.
- **Keeping decoded events across an unload.** That is the residency the idle-unload design removes
  ([restore-shared-read §13](restore-shared-read.md#13-rejected-alternatives)); the validation record
  holds directory rows, not events.
- **Recording at every commit.** It puts directory maintenance in the write path and still needs the
  validation boundary to seed the record; release already observes the settled directory.
- **Recording on create.** A created Session has not been validated; the first restore should keep
  the pass, as the issue states. It would also require an argument for every future create path.
- **A store-wide or App-Server-wide cache keyed by Session id.** It would outlive the storage
  generation and duplicate the owner of the data.
- **Lazy or background validation on first attach.** It changes when corruption is reported and what
  `session.attach` may answer; [#713](https://github.com/arcships/dim/issues/713) owns the cold-open
  profile and any bound.
- **A `testing` counter for the pass.** The flow test proves the reuse by placing damage that only the
  event pass detects between two opens; no test-only API is needed (§12).

## 12. Acceptance

**Behaviour (CI).**

1. **Reuse and miss.** In `tests/storage_flow.rs`, on a disposable database: create a Session, append
   events, call `branches` (full pass), then `release_session`; damage an event row in a way only the
   event pass checks (a batch boundary in the body, as the existing
   [`recovery_rejects_corrupt_records_and_preserves_them`](../../../crates/dim-app-server/tests/storage_flow.rs#L2413)
   does, or the event-id index); `branches` returns the same branches and heads — the pass did not
   run. Then append a valid event through the store (the head changes) and `branches` fails with the
   existing validation error — the pass ran.
2. **First open.** The existing corruption tests keep their result: a fresh `SqliteStorage` on the
   same file rejects the damage, because a new store starts without a record. A new Agents generation
   creates a new `SqliteTimelineStore` over the same database, so its first restore validates too;
   the agents flows that restart a server and restore a stored Session exercise that path.
3. **Unchanged flows.** Restore, fork, idle unload and re-attach, delete, export and the format
   upgrade tests pass unchanged.

**Measured (reported in the implementation PR, not asserted).**

4. `restore_cost` plain, tools and product at about 1k, 3k and 10k events, against main on the same
   machine: `firstRestoreMs`/`firstBranchesMs` for the first restore of each Session (full pass),
   and `reacquireMedianMs`/`reacquireBranchesMedianMs` for the rest (reuse). Acceptance: on the
   issue's 5,008-event `product` sample the re-acquisition does not run the pass (about 30 ms of
   the 69 ms). The first restore stays within noise of main. The remaining restore time — branch
   opening, capability rebuild, `branch/resume`, product startup — is [#713](https://github.com/arcships/dim/issues/713)
   and [#588](https://github.com/arcships/dim/issues/588), not a target here.
5. The record's size per Session (directory rows, not events) and the map after a run over N
   Sessions, from the release build; reported, not asserted.

Measured on this change (release build, one machine, a disposable store, five repetitions per size;
`restore_cost` reports `firstRestoreMs`, `firstBranchesMs`, `reacquireMedianMs` and
`reacquireBranchesMedianMs`):

| Mix | Events | First restore | First `branches` | Re-acquisition median | Re-acquisition `branches` |
| --- | ---: | ---: | ---: | ---: | ---: |
| product | 5,010 | 106.3 ms | 57.8 ms | 50.7 ms | 0.04 ms |
| plain | 3,016 | 47.0 ms | 22.7 ms | 25.9 ms | 0.04 ms |

The pass is the difference between a first restore and a re-acquisition; the rest is the restore itself
and is not part of this change. The absolute numbers differ from the issue's machine and sample (30 of
69 ms at 5,008 product events there); the structural result is the same: `branches` falls to the
directory read on a reuse.

**Not in this PR.** The cold first `session.attach` after a daemon start (the issue's comment,
[#713](https://github.com/arcships/dim/issues/713)) keeps its full pass and its measurement; the
first open's cost is unchanged here, and the rest of a re-acquisition after the pass is #713/#588.

## 13. Review decisions

1. **Acceptance is the removed pass, not "a few ms".** On the issue's 5,008-event `product` sample the
   pass is about 30 of 69 ms; the rest of the restore stays with [#713](https://github.com/arcships/dim/issues/713)
   and [#588](https://github.com/arcships/dim/issues/588). The implementation PR states this split.
2. **Creation is not validation.** A Session created in this process has no record; its first restore
   validates fully. No record is written on create.
3. **Cleanup only.** Delete and rollback remove the entry; there is no eviction or size bound. An
   entry is one directory row per branch, and a miss only costs one full pass.
4. **External edits.** An edit to the database while the daemon holds it is detected by the next full
   pass, not by every restore; the lease admits one writer and export is read-only. This is the
   boundary of "corruption is still detected on the first open".

## 14. Changes to other documents

With this design: [the documentation index](../../INDEX.md) links this page, and
[restore-scoped shared history read §7](restore-shared-read.md#7-what-it-removes-and-what-remains)
links here where it assigns validation to storage.

With the implementation:

- [timeline-storage](../../architecture/rust-agent/timeline-storage.md) §4 "恢复" and §5 "恢复": the
  first restore of a Session in a process validates fully; a later restore reuses the result while
  the directory and every branch head are unchanged, and the record is process memory of the store,
  dropped with it.
- [native-storage-format.md](native-storage-format.md) "读取校验": the failure statement is limited to
  the full pass — a restore whose directory is unchanged does not re-read events or content — and links
  here; the invariants it states are unchanged, only when they run is narrower.
- `TimelineStore::branches`: the permission sentence of §10.

## 15. Related issues

| Issue | Relation |
| --- | --- |
| [#601](https://github.com/arcships/dim/issues/601) | This design: skip the validation pass on a re-acquisition |
| [#588](https://github.com/arcships/dim/issues/588) | The audit that measured validation at ~14 µs per event; this removes the repeated pass, not the first |
| [#712](https://github.com/arcships/dim/issues/712) | Independent and complementary. The history window's anchors come from indexes, not from state rebuilt during validation; its D6 already assumes a re-acquired Session skips validation |
| [#713](https://github.com/arcships/dim/issues/713) | Cold first attach. This design keeps the first open fully validated; #713 profiles and bounds it separately |
| [#193](https://github.com/arcships/dim/issues/193), [#235](https://github.com/arcships/dim/pull/235), [#239](https://github.com/arcships/dim/pull/239) | The restore read path this pass precedes; their measurements are the baseline for §12 |
| [#331](https://github.com/arcships/dim/issues/331) / [idle unload](idle-unload.md) | The 60 s unload that makes repeated restores common |
