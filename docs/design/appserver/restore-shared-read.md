# Restore-scoped shared history read

Status: Implemented in [#235](https://github.com/arcships/dim/pull/235); §17 (TimelineTree's restore facts,
[#239](https://github.com/arcships/dim/issues/239)) extends it. Phase 3 of
[#193](https://github.com/arcships/dim/issues/193). Phase 1, a bounded content cache shared across the pages of a read,
merged as [#213](https://github.com/arcships/dim/pull/213); phase 2, request message lists (native format v17), merged
as [#224](https://github.com/arcships/dim/pull/224), and §15 of its [design](request-delta-storage.md) leaves this
phase to the runtime. The numbers in §2 to §8 were measured on #224's branch with a prototype; §8 ends with the
implementation's.

> **The opening copy is withdrawn.** [ADR 021](../adr/021-reads-follow-the-state-owner.md) D6 and
> [#1063](https://github.com/arcships/dim/issues/1063) removed the shared decoded copy of a branch's opening: no read
> fills or serves a copy while a branch opens, and `Agent::live_openings` is gone. §4, §5, §8, §14 and §17 describe it
> and are kept for reference only. The bounded content cache across the pages of one read and the request message
> lists of phases 1 and 2 are separate mechanisms and remain in force.

The change is to runtime infrastructure ([principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为)
rule 8), so it requires review by the runtime mechanism owner, the core mechanisms lane (eric8810,
[roadmap §3.5](../roadmap.md#35-按人按批次)). Author lane: Agents/storage (yyhhyyyyyy). Reviewers: eric8810, zerob13.
Scope: the committed-history read path of `Timeline`
([timeline.rs](../../../crates/dim-agent-runtime/src/core/timeline.rs)), which the branch scope starts sharing
([assembly.rs](../../../crates/dim-agent-runtime/src/assembly.rs)) and the branch's `BranchLifecycle` stops sharing when
the branch leaves `Loading` ([config.rs](../../../crates/dim-agent-runtime/src/config.rs)).

## 1. Summary

After v17 a Session restore still decodes each branch's history about a dozen times: storage validates it once,
TimelineTree scans it once, and every capability that rebuilds while the branch opens reads it again (§2). The reads
return the same immutable facts; only the decoding repeats.

- **Rule.** While a branch opens, its Timeline serves reads of the history the branch was built with from one decoded
  copy. The opening is the branch lifecycle's `Loading` phase: it starts when the branch scope builds the Timeline at
  the stored head `h`, and ends when the branch is activated (published), starts to close or unload, or its build is
  rolled back. The copy holds the branch's own events `1..=h` and, for a child branch, each ancestor prefix to its cut
  (§4).
- **Capabilities still read.** They fix `S = committed()` and read `local(..=S)`; nothing is pushed to them.
  The first read that needs the whole prefix decodes it with today's store read; readers that arrive meanwhile wait
  for it; each reader receives its own `Vec` of the same `Arc<AgentEvent>` values, as live dispatch hands every
  subscriber the same `Arc`. The copy keeps each event's batch end, so a range that ends inside an atomic batch fails
  with the store's `InvalidInput` (§4.3). Events past `h` (terminals written during the opening, `branch/resume`) are
  read exactly as today. Reads outside an opening do not change.
- **Bounded in time and size.** The copy is filled once and never grows: it is one `local(..=h)` result, which every
  rebuilding reader holds today, with each event's batch end. It is dropped when the opening ends, so nothing of it
  survives publication, and an idle branch holds what it holds today (§5).
- **Failures.** A failed fill is not kept: each reader either receives the copy or the outcome of its own read, error
  included (§6).
- **Effect.** Measured on #224's branch at about 10,000 events, restore with first idle goes from 469 to 126 ms
  (plain), 527 to 141 ms (tools) and 496 to 170 ms (product) with the prototype, and from 468 to 129, 523 to 141 and
  492 to 169 ms with the implementation against v17. Full-history decodes per restore go from 12 to 3 (validation,
  TimelineTree's scan and the fill), and to 2 once §17 reads TimelineTree's restore facts from the copy. The peak heap of a restore falls to about a third, and live bytes at the first
  idle do not change (§8).

Unchanged: the Timeline as the per-branch time bus, the Dispatcher and its hot window, the capability rebuild
contract, the signatures and results of `local`, `history`, `committed` and the checkpoint selection (errors included), event types
and frozen fixtures, capability and plugin code, storage and its format, and every protocol. No new event, service,
method, configuration or public API; the `testing` feature adds a count for tests (§9).

## 2. Problem and evidence

### 2.1 Restore cost after v17

#224 measured restore with first idle on v16 against v17 (its PR body; Apple M4, release, mean of two interleaved
rounds of seven): at about 10,000 events, plain 1,697 → 452 ms and tools 2,088 → 510 ms, with a full read of the
history taking 139 → 35 ms and 171 → 40 ms. The per-read cost fell four times; the number of reads did not change. At
10,000 plain events a v17 restore still spends 403 of its 469 ms in 11 full-history reads, 39 ms in validation and
27 ms elsewhere (§8).

### 2.2 What a restore is

A restore starts at `Agent::restore_session` and ends when the Session is published. What callers do afterwards is
outside it and unchanged: the first `idle` that `restore_cost` waits for makes no store call, and Agents' `acquire`
notifies its `SessionRestored` listeners (Collaboration) on the published Session. In between:

```text
Agent::restore_session(id)
  store.session_config(id)
  Agent::create(Restore), in its build task
    store.branches(id)                     storage validates every branch and row   ........ read A
    build_session: Session scope and Session capabilities (none of the measured ones reads branch history)
    TimelineTree::restore: one branch at a time, parents first
      for branch b at stored head h:
        store.read(b, 1..=h)               TimelineTree's own scan                   ........ read B
        child only: read_context(parent's Timeline, cut)  validates the cut          ........ read C
        TimelineFactory::build
          store.head(b)
          BranchScope: Timeline(b, ancestry, h); Dispatcher, not yet running       ─┐
          capabilities apply in dependency waves; each rebuilds from a read        │ opening of b
            (table below); some append settlement terminals                       │ (BranchLifecycle
          TimelineContext::start: dispatch begins                                  │  Loading)
        append branch/resume, wait until it is dispatched                          │
          opening handlers: ToolBinding reconciles, Prompt adopts its initial      │
          configuration; each reads the context                                   │
        finish_opening: opening receipts, then the committed fence                 │
        TimelineContext::activate: branch published                               ─┘
    Session::start, entries Ready, Session::activate: Session published
```

Reads of one branch's opening in `defaults::install`, the assembly `restore_cost` measures (product parts in the last
rows):

| Reader | Call | Range read | When |
| --- | --- | --- | --- |
| ContextPreparation | `local(..=S)` | own `1..=S` | apply, wave 1 |
| InputQueue | `local(..=S)` | own `1..=S` | apply, wave 1 |
| Permission | `history(Some(0))`, `local(..=S)` | ancestry, own `1..=S` | apply, wave 1 |
| ModelProvider | `local(..=S)` | own `1..=S` | apply, wave 2 (needs ContextPreparation) |
| Approval | `local(..=S)` | own `1..=S` | apply, wave 2 (needs Permission) |
| Prompt | `read_context(committed())` | ancestry and own `1..=S`, or from a checkpoint's `resume` | apply, wave 2 (needs ContextPreparation's `ConfigurationPublisher`) |
| ToolBinding | `read_context(sequence())` | as Prompt | apply, wave 3 (needs Prompt) |
| Loop | `local(..=S)` | own `1..=S` | apply, wave 4 (needs ToolBinding) |
| ToolExecutor | `local(..=S)` | own `1..=S` | apply, wave 4 (needs ToolBinding, Approval) |
| ToolBinding | `read_context(sequence())` | as Prompt, through `branch/resume` | opening receipt of `branch/resume` |
| Prompt, with an initial configuration (the product profile) | `read_context(event.sequence)` adopting, at the `branch/resume` event; `read_context(sequence())` publishing | as Prompt, through `branch/resume` | opening receipt of `branch/resume` |
| Wait, BackgroundExec, Collaboration (product assembly, not in `restore_cost`) | `local(..=S)` | own `1..=S` | apply |

The rows that name `read_context` are met by event-query pages since #942, not by the copy: `Timeline::query`, at the
published or the branch's publishing facts, reads the store's pages directly, and the context projection folds through them
(`history::read_context`, from a checkpoint's `resume` or from the chain's start). The copy serves the one read that
remains on the Timeline — the checkpoint's anchor (`Timeline::checkpoint`) — and nothing on `Branch` or `Session`,
which expose only the forwarding `query`, `batch_end` and `lookup`.

One traced restore of a plain Session at 10,002 events (`restore_cost` with `RESTORE_COST_SPANS`, every store call
with its range; readers inferred from their dependencies):

```text
ms from restore_session   store call                         reader
v17
    0-40                  branches                           validation, read A
   40-79                  read 1..=10002                     TimelineTree's scan, read B
   81-193                 read 1..=10002  x3, overlapping    ContextPreparation, InputQueue, Permission
  198-309                 read 1..=10002  x3, overlapping    ModelProvider, Approval, Prompt
  315-354                 read 1..=10002                     ToolBinding
  357-431                 read 1..=10002  x2, overlapping    Loop, ToolExecutor
  435-438                 append branch/resume
  438-476                 read 1..=10003                     ToolBinding, on branch/resume
  479                     Session published; the first idle follows at once, with no store call
prototype, same Session
    0-79                  branches, read 1..=10003           read A, read B
   82-120                 read 1..=10003                     the fill; every reader above is served from it
  122-127                 append branch/resume
  127                     read 10004..=10004                 ToolBinding's tail past h
  129                     Session published
```

- **Sequential or concurrent.** rutis applies a capability once its dependencies are active, so the rebuild reads
  run in four waves, one after another. The reads of a wave overlap in time but take turns on the store's connection
  segment by segment: three overlapping reads took about 111 ms each, against 38 ms for one alone. Branches open one
  after another: TimelineTree awaits each branch before reading the next one's events.
- **`S` is shared up to `h`.** The Timeline is built at `h`, the last stored sequence; `committed()` is `h` until
  something is appended. With unfinished work from before the restart, some capabilities append settlement terminals
  in `apply` (ToolExecutor, ContextPreparation, ModelProvider, InputQueue's owed decisions, BackgroundExec's wake
  inputs; [single-tree](../../architecture/rust-agent/single-tree.md#能力重载协议) row "失败判定"), so readers of a
  later wave fix a larger `S`. Not every reader reads through `committed()`: ToolBinding and Prompt's publication read
  through `sequence()`, the published sequence, which trails `committed()` while terminals are unpublished, and Prompt's
  adoption reads through the `branch/resume` event; the table names each. All of these are at least `h`, so every
  reader's range starts with the same `1..=h` and differs only in a short tail written during the opening.
- **Hot window or store.** At construction the hot window is empty and the published sequence is `h`, so `1..=h` comes
  from the store. Events appended during the opening enter the hot window (the Dispatcher does not run until
  `TimelineContext::start`), and once the writer commits them they are also in the store. The Dispatcher itself reads
  only the hot window.

### 2.3 The multiplier

Every reader of the table decodes `1..=h` on its own. With v17 each such read costs about a full read of the history,
so the opening of a branch with `n` readers costs `n` full decodes on top of validation and the tree's scan. In the
plain and tools profiles `n` is 10 (nine in apply, one on `branch/resume`), so a restore decodes the history 12 times;
each rebuilding capability a product assembly adds is one more. Decoding dominates: in #193's profile the folds
themselves took at most 21 ms at 10,000 events.

## 3. Constraints

Each row names a contract, where it is written, and the mechanism that keeps it.

| Contract | Source | How this design keeps it |
| --- | --- | --- |
| The Timeline is each branch's time bus; capabilities consume it through the Dispatcher and rebuild by reading it | [time bus](../../architecture/rust-agent/time-bus.md), [timeline-storage §1](../../architecture/rust-agent/timeline-storage.md#1-结构与所有权) | The copy lives behind the Timeline's read functions. The Dispatcher, its hot window and subscriptions are untouched |
| Rebuild: fix `S = Timeline::committed()` in the rebuild gate, read `local(..=S)` (own events, no ancestor prefix); the read covers terminals the old generation wrote and has not published, without waiting for persistence or the Dispatcher | [single-tree, capability reload protocol](../../architecture/rust-agent/single-tree.md#能力重载协议) row "重建" | Capabilities call the same function with the same `S` and receive the same events. The copy covers only `1..=h`, which was stored before the Timeline existed; everything after `h`, including unpublished terminals, is read from the hot window and the store as today |
| `query`, `batch_end` and the checkpoint selection: signatures and semantics | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#4-读取) | Unchanged, errors included. `through` runs as today. A slice is selected by sequence, so it holds exactly the events a store read of that range returns, and it is served only after the store's end check (next row) |
| A range may start inside an atomic batch; its end must be a batch boundary, or the read fails with `InvalidInput` | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) "分段读取" row; `TimelineStore::read` ([timeline_store.rs](../../../crates/dim-agent-runtime/src/timeline_store.rs)) | Today the stores check the end with `StoredEvent.batch_end`, and the restored hot window holds nothing for `1..=h`. The copy keeps each event's batch end from the fill and runs the same check on the end of every slice, own or ancestor, before serving it (§4.3) |
| Events are facts once sequenced; order is the sequence only | [time bus §1, §2](../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实) | The copy depends on it: events `1..=h` and their batch ends never change, so a slice taken later, with its end check, equals a read taken later |
| Only immutable facts are shared; no push replay; no capability snapshot; no shared capability state | [time bus §2](../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实) (each state has one owner, decided from the events it consumed); [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) (no business state for an optimization) | The copy holds the `Arc<AgentEvent>` values live dispatch hands every subscriber (`Committed(Arc<AgentEvent>)`), each with the batch end the store returned with it, and no capability state. Each reader calls the read itself and folds into its own state |
| Bounded reads: segments, connection released after each | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | The copy is filled by today's store read, with its segments and #213's bounded content cache |
| A checkpoint read does not scan request bodies before the snapshot's `resume` | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | A read that starts after sequence 1 never fills the copy: it slices an existing copy or reads alone from `resume` |
| Restore caches are bounded; validation's cache is released when validation ends | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) "恢复" row, [§7](../../architecture/rust-agent/timeline-storage.md#7-验收) "分页与恢复" | The copy's size is fixed when it is filled: one `local(..=h)` result (plus, for a child, one ancestry read), which each rebuilding reader already holds today, with 8 bytes per event for batch ends. It never grows and is dropped when the opening ends |
| The process keeps no dispatched events; how long a returned `Arc<AgentEvent>` lives is the caller's choice | [timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#使用方), last bullet | During its opening the Timeline holds one read result for its readers; after the opening it holds none (§15 added this sentence with the implementation) |
| An idle branch's live bytes do not grow with steps | [timeline-storage §7](../../architecture/rust-agent/timeline-storage.md#7-验收) | The copy does not outlive the opening; live bytes at the first idle equal v17's (§8) |
| A read failure fails the operation that read, with the capability's failure terminal; it does not stop the Session's writes | [timeline-storage §6](../../architecture/rust-agent/timeline-storage.md#6-故障) | A failed fill is not kept; each reader receives its own read's error (§6) |
| Side effects are not replayed; a new generation takes over from an explicit sequence | [time bus §5](../../architecture/rust-agent/time-bus.md#5-副作用不重放) | Unchanged: capabilities still take over at their own `S` |
| Event types and frozen fixtures; plugin code | [principles §6](../../architecture/rust-agent/principles.md#6-验证), AGENTS.md rule 6 | Untouched |

## 4. Mechanism

### 4.1 Scope: the branch opening

The copy belongs to one branch's `Timeline` and lives exactly while that branch is `Loading`:

| Point | Where | Effect |
| --- | --- | --- |
| The branch scope builds the Timeline at `h` (`BranchScopePlugin::apply`) | assembly.rs | The opening starts, with the branch's ancestry and `h`, and the branch's `BranchLifecycle` holds it; nothing is read yet. A branch with `h` = 0 and no ancestry has nothing to share and starts none |
| The branch leaves `Loading` (`BranchLifecycle` advances to `Active` or `Closing`) | config.rs | The lifecycle drops the opening. One signal covers every way out: `TimelineContext::activate` (published), the start of `TimelineContext::finish` (close or unload: failure, cancellation, Session close during the restore) and the rollback of a build that failed before a `TimelineContext` existed |

The Timeline keeps only a weak reference to the opening, so the opening ends with the phase change and no call site of
its own; a lifecycle dropped with its scope drops it too.

A restore opens branches one at a time, so at most one copy of the Session exists at a time during a restore, and the
last one is dropped before the Session is published. Branches created at runtime open independently: concurrent child
builds each hold their own copy of their ancestry (§5). Assembly follows the opening's scoped inactivity interval
([How Dim uses rutis](../rutis-usage.md#appendix-a-rust-binding)); first row activation, opening dispatch advancement and
receipt completion renew that interval. Storage reads have an I/O cancellation budget; replay has no fixed total deadline.
Failure or cancellation rolls back the opening and leaves `Loading`.

The opening is the runtime's own unit; a restore is a Session whose branches all open with `branch/resume`. The same
opening also serves a branch created at runtime (TimelineTree `branch`, `create_main`) and a prepared Session (fork):
a new child has no events of its own but rereads its ancestry several times while it opens (§7). The runtime does not
need to know which of these it is building.

### 4.2 What the copy holds

- **Own part.** The events `1..=h` of the branch in sequence order, each as an entry of the event and its batch end.
- **Ancestry part.** For a child, the entries `1..=cut` of each ancestor with a cut above 0, in ancestry order.
- **Events and batch ends.** An entry is one `StoredEvent` of the fill without its trace: the `Arc<AgentEvent>` and
  its `batch_end`, the last sequence of the atomic batch the event was committed in. The store's range check reads
  the batch end (§4.3), and the restored hot window has none for `1..=h`, so the copy keeps it, 8 bytes per event.
  Traces are not kept: the reads served from this path (`local`, `history`) return events only; `trace`
  keeps its own read. The `Arc`s are the store read's, so a reader that keeps an event keeps exactly what it would
  keep after its own read.
- **Fixed.** Each part is filled at most once and never extended; `h` and the cuts are fixed when the Timeline is
  built.

### 4.3 Serving a read

Every committed-history read of a Timeline that reads stored events in ranges goes through one internal path that
splits the request into its hot window part and stored parts; `local`, `history`, the checkpoint's anchor read and the
wrappers on `Dispatcher`, `Branch` and `Session` all reach it. The event query is not one of them: it reads the store's
pages directly. Only the stored parts change. Every stored-range read, own or ancestor, goes through one of two seams,
and both run the same end check:

```text
own stored part a..=b          (b <= published sequence, as today)
  no opening, or a > h ................................ store.read(a..=b)               as today
  a == 1 and b >= h ............ fill the own part once ─┐
  any other range, own part filled .....................─┴─> slice(own, a..=min(b, h))
                                                             then store.read(h+1..=b), if b > h
  any other range, own part not filled ................ store.read(a..=b)               as today

ancestor parts [(branch, a..=b), ...]
  reached from: the ancestor segments of history; history's single-segment
  shortcut (one ancestor segment, such as a new child's history(Some(0))); the anchor
  read of the checkpoint selection, for an anchor on an ancestor
  no opening, or a range outside the ancestry ........... store.read / read_segments    as today
  every ancestor, each 1..=cut ...... fill the ancestry once ─┐
  any other set, ancestry filled ............................─┴─> one slice per segment
  any other set, ancestry not filled ..................... store.read / read_segments  as today
```

- **Every stored read.** The own seam serves every read of the branch's own stored events: `local`, the own segment
  of `history`, and the anchor read of the checkpoint selection (`records(branch, snapshot.sequence,
  snapshot.sequence)`, or the segment read when the anchor row is inside an atomic batch) for an anchor on the branch
  itself. The ancestor seam serves every read of an ancestor's: the ancestor segments of `history`, the
  single-segment shortcut of `history_records` (one segment is read through the per-branch read instead of
  `read_segments`; a new child's `history(Some(0))` is such a read), and the anchor read for an anchor on an ancestor.
  No other path in the Timeline reads stored events.

- **Slices by sequence.** A slice takes the entries whose sequence lies in the range (a binary search on
  `event.sequence`), so its events are what the store returns for that range: one event per sequence, merged output
  rows already split into their events.
- **End check.** Before a slice `a..=e` is served (`e` is `min(b, h)` for the own part and the segment's end for an
  ancestor), it applies the store's rule for a range: the start may be inside an atomic batch, and the end must be a
  batch boundary. If the entry at `e` has `batch_end > e`, the read fails with
  `AgentError::InvalidInput("sequence {e} is inside an atomic batch")`, the error both stores return
  (`MemoryTimelineStore` and the SQLite store), and nothing is served. With a batch persisted at sequences 1 and 2 and
  `h >= 2`, `local(..=1)`, `history(Some(1))` and `context(1)` fail whether or not the copy is filled, while
  `local(..=2)` returns events 1 and 2 and `local(2..=2)` returns event 2. `h` is a batch boundary, so when `b > h` the
  cached part passes and today's read past `h` checks `b`. Every ancestor slice runs the same check, with the same
  function: an ancestor segment ends at its cut, which the fill's read already checked, so no current reader can make
  it fail, but no slice is served without it.
- **Past `h`.** The part after `h` is never served from the copy. It is read from the hot window and the store by
  today's code, so a reader with a larger `S` sees the terminals written during the opening exactly as today.
- **Sub-ranges.** `local(a..=b)` with `a > 1`, a checkpoint read from `resume`, and the checkpoint anchor read, own or
  ancestor, are slices once the copy exists, and plain reads before. They never fill it: a read that starts after
  sequence 1 does not cause the prefix before it to be decoded.
- **Different `S`.** Readers fix their own `S`; the copy only replaces the stored prefix `1..=h` that every such range
  starts with.

### 4.4 Single flight

Each part is filled at most once, by the first read that needs all of it; readers that need it while the fill runs
wait for that fill instead of starting their own. This is `tokio::sync::OnceCell::get_or_try_init`, whose semantics
are the ones this design needs:

- The filling reader runs the store read with no lock held; waiters wait on the cell, not on the Timeline's data lock,
  so appends and dispatch never wait for a fill.
- If the fill fails, the cell stays empty, the filling reader receives its error, and the next waiter runs its own
  read (§6). Waiters therefore retry one after another: on a store that keeps failing, `n` waiting readers cost `n`
  failed reads in sequence, each reader receiving its own error, where without the copy the same reads would fail
  side by side.
- If the filling reader is cancelled (its apply future is dropped), the next waiter fills.
- A fill waits only for the store, and no store call waits for a Timeline reader, so a waiter can never be waiting on
  itself, whatever rebuild gate it holds.

### 4.5 Release

The branch's `BranchLifecycle` holds the opening and drops it when the branch leaves `Loading` (§4.1); the Timeline
keeps only a weak reference. A reader that obtained the copy before that finishes slicing and drops its own reference,
so the copy's memory is freed when the last such reader returns. After the opening, reads go to the store as before.
Nothing else holds the copy: it is private to the Timeline and its lifecycle, never handed to a capability, and never
cached by key outside the Timeline.

### 4.6 Readers outside an opening

Unchanged, because no copy exists: capability reloads in an active branch, model steps (the context fold,
ContextPolicy's `local(through+1..)`), Prompt and configuration publication, TimelineTree's `branch` validation on a
parent, the App Server's history pages and summaries, collaboration and export.

## 5. Residency

Today each reader holds its own decoded history from its read until its fold returns, and readers of one wave overlap,
so a restore's peak holds several decoded histories at once, plus the opening handlers' context reads after the
waves. With the copy the opening holds one decoded history from the first fill until the branch leaves `Loading`, its
opening handlers included, bounded by the assembly and I/O policies in §4.1; each reader adds a vector of pointers (8 bytes per
event) while it folds.

Measured with a counting allocator in `restore_cost` (§8): the peak heap above the level before the restore, during
the restore and its first idle, in MB.

| Mix | Events | `main` (v16 + #213) | #224 (v17) | #224 + prototype |
| --- | --- | --- | --- | --- |
| plain | ~1k | 3.9 | 5.4 | 2.4 |
| plain | ~3k | 18.2 | 17.8 | 6.6 |
| plain | ~10k | 110.7 | 95.9 | 32.7 |
| tools | ~1k | 4.4 | 5.9 | 2.6 |
| tools | ~3k | 21.1 | 20.1 | 7.5 |
| tools | ~10k | 134.7 | 112.5 | 38.1 |
| product | ~1k | 4.9 | 6.8 | 3.0 |
| product | ~3k | 18.0 | 19.7 | 7.3 |
| product | ~10k | 65.3 | 69.2 | 23.7 |

At the first idle the heap is 0.30 to 0.33 MB above the level before the restore in every build, mix and size, and
0.10 MB after the Session unloads. The prototype counted no opening alive at the first idle in any restore.

- **No worse than today.** The copy is one of the results every reader already materializes, plus its batch ends;
  the readers' own results become pointer vectors. A reader that keeps an event keeps the same `Arc` it would keep
  today.
- **Batch ends.** The prototype in the table kept events only. The batch ends of §4.2 add at most 8 bytes per event
  to the one copy an opening holds, 0.08 MB at 10,000 events. Measured on one plain Session of about 10,000 events,
  in four interleaved runs of seven restores, without and with them: the peak was 32.69 MB and the heap at the first
  idle 0.30 MB above the level before the restore in every run.
- **Nothing survives publication.** The copy is dropped when the branch leaves `Loading`; live bytes at the first idle
  are v17's.
- **Long-session residency.** An idle branch holds no copy, so its live bytes do not grow with steps
  ([timeline-storage §7](../../architecture/rust-agent/timeline-storage.md#7-验收)).
- **Child branches.** A child's opening holds its own events and one ancestry read, with their batch ends: the size
  of one context fold of the child, which Prompt and ToolBinding each materialize today.
- **One copy per Session, during a restore only.** A restore opens one branch at a time. Branches created at runtime
  (concurrent `branch` calls, a prepared Session) open independently, so concurrent child builds each hold their own
  copy of their ancestry while they open, as each of their readers holds a decoded ancestry today.

## 6. Failure semantics

| Case | Behaviour |
| --- | --- |
| The fill's store read fails | The filling reader receives its error. The cell stays empty; each waiting reader then runs its own read, one after another, and receives its own result. With a store that keeps failing, `n` waiting readers make `n` failed reads one after another (§4.4). A capability whose read fails fails its `apply`, the branch build fails and is rolled back as today, and the rollback, leaving `Loading`, ends the opening |
| A suffix read past `h` fails | That reader receives the error, as today |
| The filling reader is cancelled | The next waiter fills; nobody receives a partial copy |
| Storage commit failure during the opening | Unchanged: the part past `h` follows today's hot window and store rules; `1..=h` was committed before the Timeline was built |
| The opening ends while a reader holds the copy | That reader finishes with the events it was served; later readers read the store |
| Corrupt rows | `TimelineStore::branches` validated every row before the build (read A); a fill that hits damage fails like any read |

**Same result per reader.** A reader served from the copy receives the events `store.read` returns for `1..=h` (the
fill was such a read), cut to its range by sequence after the store's end check on the batch ends the fill returned
(§4.3), followed by what today's code returns after `h`. The events `1..=h` and their batch ends are committed, and
storage never changes a committed event: it appends events, extends an output row only with later events, and its
upgrades rewrite no business row ([storage format](native-storage-format.md)). On an intact store, a read served from
the copy therefore returns what its own read would: the same events, or the same `InvalidInput` for an end inside an
atomic batch. The only difference is exposure to I/O failure: a reader served from a copy filled a moment earlier does
not repeat that I/O, so it cannot meet a transient storage error its own read might have met.

## 7. What it removes and what remains

Reads of `main` in one restore of a `main`-only Session, from the traces of §2.2 and §8:

| Read | Plain and tools today | Product today | With the copy |
| --- | --- | --- | --- |
| A. `TimelineStore::branches` (`validate_session`) | 1 full | 1 full | 1 full; storage's own pass, see below |
| B. TimelineTree's restore scan | 1 full | 1 full | 1 full; a slice since §17 |
| `local(..=S)` of ContextPreparation, InputQueue, Permission, ModelProvider, Approval, Loop, ToolExecutor | 7 full | 7 full | 1 full, the fill; the other readers get slices |
| Prompt and ToolBinding in apply (`read_context`) | 2 full | 2 from a checkpoint's `resume` | slices |
| Opening handlers on `branch/resume` | 1 full (ToolBinding) | 3 from `resume` (ToolBinding, Prompt twice) | slices, each with a one-event tail |
| Wait, BackgroundExec, Collaboration (product assembly, not measured) | 3 full | 3 full | slices |

A checkpoint read here is two store calls: the anchor read of the snapshot's sequence and the context from `resume`, 1
and 212 events at 10,000 product events.

A child branch adds, today, its own reads as above and five reads of its ancestry: TimelineTree's cut validation on the
parent (read C), Permission, Prompt and ToolBinding twice. With the copy it adds its own part once and its ancestry
twice (read C and one fill). Traced on v17 for a child cut at 3,002 events of `main`: its own events were read 11 times
and `main`'s prefix 5 times; with the prototype, twice each (own: scan and fill; prefix: read C and fill), plus a
one-event tail.

**Validation hand-over (read A) is out of scope.** Storage validates every branch before the runtime builds anything,
page by page, with a bounded cache released at the end of the pass
([timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描), "恢复" row). Handing its
decoded events to the runtime would hold every branch's history from validation until the last branch opens, change
what the `TimelineStore` port returns or keep state between its calls, and contradict the bounded-cache convention.
Making validation itself cheaper is storage's work and needs no runtime change
([restore validation reuse](restore-validation.md)); the first pass of a process still decodes every event, and a
later restore with an unchanged directory reuses it.

**TimelineTree's scan (read B) stays in this design**, which changes no capability code: it ran before the branch's
Timeline existed. §17 takes it up: the failed creates are read from the copy while the branch opens, and a child's
creation check reads only its first events.

## 8. Measured effect

Release builds on one Apple M4, disposable stores under an isolated home, no other build running. Three builds of
the same `restore_cost` harness: `main` at 2946f25d (format v16 with #213), #224 at 29d3450b (v17), and #224 with the
prototype of this design (uncommitted). The harness adds to #224's version a counting allocator, the store calls of a
restore with their ranges, an optional child branch and an optional pause before the create; v16 Sessions were grown with the `main` build and v17
Sessions with the #224 build, which the prototype restores too. Two rounds per mix, in the orders main, v17,
prototype and prototype, v17, main; seven restores per size in each; within a round the sizes are rotated across
repetitions so no size is always measured first or last, avoiding order-dependent cache bias. The tables show the mean
of the two medians. Round medians agreed within 3% at 10k. Nothing here is asserted in tests. The prototype measured here kept events
only; the batch ends added after review (§4.2) were measured separately on one plain Session at 10k (§5), where a
restore with first idle took 131 and 134 ms without them and 140 and 132 ms with them, in interleaved runs.

Restore with first idle, ms:

| Mix | Events | `main` | #224 | #224 + prototype | Prototype / #224 |
| --- | --- | --- | --- | --- | --- |
| plain | ~1k | 53.7 | 40.3 | 19.0 | 0.47 |
| plain | ~3k | 240.6 | 118.3 | 35.1 | 0.30 |
| plain | ~10k | 1,750 | 469 | 126 | 0.27 |
| tools | ~1k | 62.2 | 45.0 | 17.0 | 0.38 |
| tools | ~3k | 284.3 | 132.6 | 38.6 | 0.29 |
| tools | ~10k | 2,136 | 527 | 141 | 0.27 |
| product | ~1k | 65.8 | 62.0 | 20.5 | 0.33 |
| product | ~3k | 155.7 | 146.9 | 51.0 | 0.35 |
| product | ~10k | 524 | 496 | 170 | 0.34 |

Store reads per restore, besides validation (the same at every size for a mix): the number of read calls, and the
events they returned over the Session's events, that is, full-history reads:

| Mix | `main` and #224 | #224 + prototype |
| --- | --- | --- |
| plain, tools | 11 calls, 10.9 to 11.0 | 3 calls, 2.0 |
| product | 18 calls, 11.2 (1k), 8.7 (3k), 8.3 (10k) | 8 calls, 2.0 |

The prototype's calls are TimelineTree's scan, the fill and one-event tails past `h` (one in plain and tools, six in
product, where three opening handlers each read from a checkpoint twice). In every restore three readers arrived while
the fill was running and waited for it; 10 (plain, tools) and 17 (product) reads were served from the copy.

Where a plain restore at ~10k spends its time, ms (the store's spans; reads overlap, so their wall time is counted
once):

| | Validation | Reads | `branch/resume` append | Outside the store | Total |
| --- | --- | --- | --- | --- | --- |
| `main` | 147.8 | 1,577.8 | 4.2 | 20.7 | 1,750 |
| #224 | 39.1 | 403.0 | 4.3 | 22.6 | 469 |
| #224 + prototype | 39.0 | 75.0 | 4.3 | 7.4 | 126 |

What is left is about a third each: validation (read A), TimelineTree's scan (read B) and the fill. Bytes per event
are unchanged (411 against 410 at plain 10k), as the store is unchanged.

A child branch (one traced restore of a plain Session with a child cut at 3,002 events of `main`; counts only, see
§7): `main`'s prefix was read 5 times on #224 and twice with the prototype, the child's own events 11 times and twice.

**Session create.** In the same rounds a Session create took 9 ms (plain, tools) and 39 ms (product) on `main` and
#224, and 21 to 24 ms and 51 ms with the prototype, though a new `main` starts no opening (`h` = 0, no ancestry). The
cause is the store's writer, which then started its rounds at least 20 ms apart
([storage/timeline.rs](../../../crates/dim-app-server/src/storage/timeline.rs),
[timeline-storage §3](../../architecture/rust-agent/timeline-storage.md#3-写入与发布)): the prototype's
restore ends within 20 ms of its own `branch/resume` round, so the create's first round waits. With a 40 ms pause before
the create, the prototype's create took 9 ms (plain) and 36 ms (product). The same pacing lengthens the prototype's
`branch/resume` append in the small restores (8.5 ms against 4.1 at plain 1k), so the 1k ratios understate the gain.

**The implementation.** Measured the same way on #235: release builds of `main` before phase 2 (0fb1d2bf, v16 with
#213), v17 (ae1bbe72) and #235, each with the `restore_cost` harness #235 commits (the two baselines without its
openings count); v16 Sessions grown with the v16 build, v17 Sessions with the v17 build, which #235 restores too; two
interleaved rounds of seven restores per size, the mean of the two medians. Restore with first idle, ms, and the peak
heap of the restore, MB:

| Mix | Events | v16 | v17 | #235 | #235 / v17 | Peak heap, v17 → #235 |
| --- | --- | --- | --- | --- | --- | --- |
| plain | ~1k | 53.0 | 39.7 | 17.9 | 0.45 | 5.43 → 2.36 |
| plain | ~3k | 236.3 | 117.7 | 36.0 | 0.31 | 17.75 → 6.64 |
| plain | ~10k | 1,710 | 468 | 129 | 0.28 | 96.3 → 32.7 |
| tools | ~1k | | 44.4 | 16.7 | 0.38 | 5.90 → 2.61 |
| tools | ~3k | | 131.7 | 38.2 | 0.29 | 20.1 → 7.5 |
| tools | ~10k | | 523 | 141 | 0.27 | 112.5 → 38.1 |
| product | ~1k | | 61.2 | 19.5 | 0.32 | 6.49 → 3.00 |
| product | ~3k | | 144.8 | 50.1 | 0.35 | 19.7 → 7.3 |
| product | ~10k | | 492 | 169 | 0.34 | 69.4 → 23.7 |

Store reads per restore, besides validation: 3 calls and 2.0 full-history reads in plain and tools, 8 calls and 2.0 in
product (v17: 11 and 18 calls). At the first idle the heap was v17's within 0.003 MB in every mix and size, and no
opening was alive. Round medians agreed within 11% (within 8% at 10k).

**Session create after a restore, accepted.** In the same rounds a create took 21.1, 21.9 and 45.6 ms (plain, tools,
product) on #235 against 10.1, 8.9 and 35.3 ms on v17; with a 40 ms pause before it, 8.8 and 32.6 ms (plain,
product) against 8.9 and 32.5 ms. The difference is the writer's round pacing described above, not the create's own
work, and it appears only when a create starts within 20 ms of a restore's last write round. The pacing is a storage
convention outside this change, so the difference is accepted rather than worked around.
[#238](https://github.com/arcships/dim/issues/238) later changed that convention ([#243](https://github.com/arcships/dim/pull/243)):
a write round now starts as soon as the previous one has committed unless every pending batch is streamed output
([timeline-storage §3](../../architecture/rust-agent/timeline-storage.md#3-写入与发布)). Measured on that change, a
create after the restores takes 7.8 and 12.0 ms (plain, product) against 24.6 and 51.0 ms before it.

## 9. Impact surface

Following [principles §3](../../architecture/rust-agent/principles.md#3-正交): the target is zero changes outside the
runtime's Timeline and zero new public content.

| Module or content | Change | Reason |
| --- | --- | --- |
| `dim-agent-runtime` `core/timeline.rs` | The opening's copy in the committed-history read path: its two seams (own part, ancestry) and the function that begins it (crate-private) | The read cost is the Timeline's own |
| `dim-agent-runtime` `assembly.rs`, `config.rs` | `BranchScopePlugin` begins the opening; `BranchLifecycle` holds it and drops it when the branch leaves `Loading` | The branch lifecycle already delimits `Loading`; `TimelineContext` is unchanged |
| `testing` feature | `Agent::live_openings`: the Agent's branch openings that are alive, with or without their copy, for the release checks of §14, like #224's node-load counter; release builds count nothing | After a failed or cancelled build no public call can reach the Timeline, so its release is not observable otherwise; the read counts need no hook (a counting store) |
| `dim-app-server` `examples/restore_cost.rs` | Heap peak and live bytes, store calls with their ranges, an optional child and an optional pause before the create, the openings alive at the first idle | The measurements of §8 and §14 items 8 and 9; nothing is asserted |
| Dispatcher, hot window, `TimelineStore`, storage, event types, fixtures, capabilities, plugins, protocols | None | §3 |
| Public content added | None | No event, service, method, wire field or configuration; `Agent::live_openings` exists only with the `testing` feature |

## 10. Rule check

| Rule | Answer |
| --- | --- |
| [Principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 1 (command events for others' state) | Not applicable: no state of another module changes |
| §4 rule 2 (handover) | Not applicable: nothing is handed over; readers still read |
| §4 rule 3 (before callbacks) | Unchanged; the copy holds committed events, which before callbacks never see again |
| §4 rule 4 (no business meaning in core) | The copy is keyed by branch and sequence ranges only; it reads no event kind and has no special case for any capability |
| §4 rule 5 (atomic batches) | Unchanged: `h` is a batch boundary (the published sequence), and a slice is served only after the store's end check on the batch ends the copy keeps, so reads return the same events and the same errors (§4.3) |
| §4 rule 6 (model context) | Unchanged: the same events, so the same context |
| §4 rule 7 (public events and services) | Capabilities keep reading through the Timeline service; none reads another's state, and none is given another's results |
| §4 rule 8 (infrastructure) | A change to the Timeline, raised by the Timeline's own read cost and closed inside the runtime: §11 |
| §4 rule 9 (only decision facts in the Timeline) | No new fact; the copy is process memory |
| §1 (Capability and App Server boundary) | Inside the runtime; no capability or App Server change |
| §3 (orthogonality) | No other module changes; §9 lists the impact surface |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | Branch scope: the copy belongs to one branch's Timeline and lives with its opening |
| ADR 002 D2 (no wiring modules) | No new module or plugin |
| ADR 002 D3 (dependencies follow scope) | No new dependency |
| ADR 002 D4 (configuration) | None |
| ADR 002 D5 (language-neutral contracts) | No contract changes; a TypeScript part reading through the Timeline query is served the same way |

## 11. Requirements raised to mechanism owners

None to rutis. The change itself is to the runtime, so it answers rule 8's items here for the runtime mechanism owner:

| Item | Content |
| --- | --- |
| Blocked by | Nothing in a plugin. The requirement comes from the Timeline: during an opening, each reader of the same immutable prefix pays the store's decode, and the rebuild contract ([single-tree](../../architecture/rust-agent/single-tree.md#能力重载协议)) makes every rebuilding capability such a reader |
| Existing mechanisms considered | The hot window holds only undispatched events and is empty at a restore. Storage's caches are bounded per read call and cannot span calls by convention. Capability-side changes (snapshots, kind-filtered reads, pushing events) are rejected in §13 |
| Generic capability | A Timeline serves reads of the history it was built with from one decoded copy while its branch opens. No business meaning: branch, sequence ranges and the lifecycle phase only |
| Effect on sequence, batch, interrupt, reload | Sequence and batch: none; reads return the same events and errors, and an end inside an atomic batch is rejected from the copy's batch ends as the store rejects it (§4.3). Interrupt: none; an interrupt during an opening is an event past `h`. Reload: a capability reloaded while its branch is still opening reads the same copy; one reloaded later reads the store as today |
| Verification | Inside the runtime and its flows (§14) |
| Owner and issue | Runtime, core mechanisms lane (eric8810, [roadmap §3.5](../roadmap.md#35-按人按批次)); [#193](https://github.com/arcships/dim/issues/193) |

## 12. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | The copy lives with a branch's opening (`Loading`) | Scoped to a Session restore, released at Session publication | Branches open one at a time, so a branch scope holds one branch's history; a Session scope would hold every branch's until the last opens. The branch lifecycle already marks the opening in the runtime, and the runtime need not know whether a branch is restored, created or prepared |
| D2 | The seam is the Timeline's committed-history read path | A forwarding `TimelineStore` for the Session's restore; wrappers in `Dispatcher::local`, `Branch` and `Session` | A store wrapper would also serve TimelineTree's scan (read B), but it would intercept the storage port for the Session's whole life, keep state between its calls and change what TimelineTree's direct store read means. Facade wrappers would cover some readers and not others (`read_context` goes through the Timeline directly) |
| D3 | The copy holds each event with its batch end, not its trace | Events alone; the store's `StoredEvent`s with traces | A read whose end lies inside an atomic batch must fail as it does from the store, which checks the end's `batch_end`; the restored hot window has none for `1..=h`, so events alone would serve half a batch. The paths served return events only, so traces would add memory and deep copies for no reader |
| D4 | A part is filled only by a read that needs all of it | Fill on any read | A checkpoint or tail read must not decode the prefix before it (timeline-storage §4) |
| D5 | The copy is fixed at `h`; reads past `h` are today's reads | Append the tail as readers read it | The tail is a few events written during the opening; extending the copy would add a second fill rule and mutable shared state for no measured gain |
| D6 | A failed fill is not kept; waiters read for themselves | Share the fill's error with its waiters | Each reader's outcome stays the outcome of its own read (timeline-storage §6), and a transient failure does not fail readers that could succeed. The cost is on a store that keeps failing: waiters retry one after another, `n` waiters and `n` failed reads (§4.4) |
| D7 | A child's ancestry is shared like its own events | Own events only | A child's opening reads its ancestry about four times (Permission, Prompt, ToolBinding twice), at creation and at restore; the same rule covers it at no extra mechanism |

## 13. Rejected alternatives

- **Replaying history to capabilities** (the Dispatcher, or a restore step, hands each subscriber the events to fold).
  Capabilities would stop reading at their own `S`; the rebuild contract and the Dispatcher's live-only role change.
- **Capability snapshots for rebuild.** Business state stored for an optimization, in every plugin
  ([timeline-storage §4](../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)), and a second source of
  truth beside the events ([time bus §3](../../architecture/rust-agent/time-bus.md#3-总线只承载影响后续行为的结果)).
- **Sharing rebuilt state between capabilities** (for example one fold that several capabilities read). State has one
  owner, which decides only from the events it consumed ([time bus §2](../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实)).
- **A store-wide decoded-event cache.** Residency would grow with every Session loaded or read recently, against the
  idle-branch acceptance; phase 2 rejected it for the same reason ([request delta storage §17](request-delta-storage.md#17-rejected-alternatives)).
- **Validation handing its result to the runtime.** §7.
- **Kind-filtered reads** (each capability asks only for the kinds it folds). Changes the read API and every
  capability's rebuild, and the store still decodes each row to filter it.
- **Keeping the copy until the Session publishes, or for some time after.** More residency for no reader: after
  activation reads are rare and served as today.

## 14. Acceptance

The implementation PR passes all of these. Behaviour checks are deterministic and run in the existing flows
(`tests/integration`, the runtime's and the capabilities' flow tests); timing and size targets are measured with
`restore_cost` in release builds and reported in the implementation PR, not asserted in tests.

**Behaviour (CI):**

1. **One decode per opening.** Restoring a Session of `defaults::install` through a counting store, each branch's
   own history `1..=h` is read from the store at most once during the restore (the fill; since §17 TimelineTree
   reads its restore facts from the copy, and a child's creation check reads pages from its start that end at its
   first queued input), whatever the number of rebuilding capabilities; the reads past `h` cover only events written
   during the opening.
   With a child branch, its ancestry is read once by its opening (plus TimelineTree's cut validation on the parent).
2. **The same events per reader.** The existing restore, reload and branch flows pass unchanged, including restores
   with unfinished work (settlement terminals written during the opening, so readers fix different `S`), a
   compaction checkpoint (a read from `resume`), and a child branch.
3. **Atomic batch ends.** In an existing restore flow whose branch probe receives the Timeline while the branch opens
   (the restore case of `crates/dim-agent-capabilities/tests/branch_flow.rs`), with a batch persisted at sequences 1
   and 2 and `h >= 2`: once a rebuild read `local(..=S)` has filled the copy, `local(..=1)`, `history(Some(1))` and
   `context(1)` fail with `InvalidInput`, while `local(..=2)` (the whole batch) returns events 1 and 2 and
   `local(2..=2)` (starting inside it, ending at its boundary) returns event 2. Each result equals the same read after
   activation, which reads the store.
4. **Release.** After `restore_session` returns, no opening holds a copy (`Agent::live_openings`, `testing`), and a
   later `local(..)` reads the store. After a restore that fails (a branch whose rebuild read fails, an injected read
   failure), and after a restore cancelled by `Agent::close` or the Session's close, no opening holds a copy.
5. **Failure.** An injected failure of the fill read fails the restore with the same storage error as today; the next
   restore succeeds.
6. **Readers outside an opening.** A capability reloaded in an active branch reads the store.
7. **Frozen contracts.** `crates/dim-agent-types/tests/fixtures/` and the `*_contract.rs` tests are unchanged.

**Measured (reported in the implementation PR):**

8. Restore with first idle, `restore_cost` plain, tools and product at about 1k, 3k and 10k events, against v17 on the
   same machine: never slower than v17; at 10k, at most 35% of v17's in plain and tools and at most 40% in product
   (the prototype measured 27%, 27% and 34%).
9. The peak heap of a restore is at most v17's at every size and mix (the prototype measured about a third at 10k),
   and the heap at the first idle is v17's within 0.01 MB.

## 15. Changes to other documents

With this design: the [documentation index](../../INDEX.md) links this page.

With the implementation, each keeping its meaning:
[timeline-storage](../../architecture/rust-agent/timeline-storage.md) §4 "使用方" says that while a branch opens its
Timeline serves reads of the history it was built with from one shared copy, released when the branch leaves `Loading`
(published, closed or rolled back), and that the copy keeps each event's batch end so §4's range-end rule holds for its
reads. §5's "恢复" row says the capabilities' point-in-time reads share it; the rebuild row
of [single-tree](../../architecture/rust-agent/single-tree.md#能力重载协议) stays as written. §15 and §20 of
[request delta storage](request-delta-storage.md) link here.

## 16. Questions from review

1. **Child ancestry (D7).** Kept and implemented: a child's opening shares its ancestry as it shares its own events,
   which also speeds up branch creation at runtime.
2. **TimelineTree's restore scan (read B).** It reads each branch's whole history to find `branch/start`, the first
   queued input and `branch/create-failed`. Left to TimelineTree's owner as a follow-up; this change does not touch it.
   Taken up in [#239](https://github.com/arcships/dim/issues/239), §17.

## 17. TimelineTree's restore facts

Follow-up [#239](https://github.com/arcships/dim/issues/239) to §16 item 2. Owner: TimelineTree
([timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)), core mechanisms lane.

**Problem.** With §4, a restore still read each branch's own history `1..=h` from the store twice: TimelineTree's scan
(read B) before it built the branch, and the opening's fill. The scan looks for two facts. The first is the creates
the branch recorded as failed (`branch/create-failed`), which keep the matching children from starting. The second,
for a child only, is whether its creation completed: `branch/start` followed by a queued input. At 3k to 10k events
the scan was 27% to 30% of a restore (#239).

**Mechanism.** Inside TimelineTree:

- **Failed creates.** The restore no longer reads them before the build. A resumed branch's build task reads
  `local(..=h)` through the branch's Timeline after its capabilities have applied and before activation, while the
  opening is alive, so the read is a slice of the copy (§4.3). It hands the failed creates to the restore loop,
  which checks the branch's children against them as before. A child is restored only after its parent is
  published, so the facts are there when it needs them.
- **A child's creation.** The check reads `TimelineStore::read_page` pages of 32 events from the start until it finds
  `branch/start` followed by the first queued input, which a created child writes while it opens. A created child
  reads one page; an interrupted creation reads to its head, as before. The check still runs before the build, so an
  interrupted child is never built.

```text
TimelineTree::restore, branch b at stored head h
  child only: read_page(b) from the start until branch/start + queued input ... one page
  child only: read C, the cut validation on the parent (unchanged)
  launch b, Resume at h
    build: capabilities apply; the first rebuild read fills the copy   ─┐
    TimelineTree: local(..=h), a slice; collects branch/create-failed          │ opening of b
    append branch/resume ... activate                                         ─┘
  restore loop: records b's failed creates, then restores b's children
```

**Constraints kept.** Every decision is made from the same facts: failed creates from the same events `1..=h`, and a
child's creation from the same events up to the one that settles it. TimelineTree's read fails only when the fill
fails, and then fails the build, which rolls back as for any read failure during the opening (§6). The creation
check's pages are ordinary store reads under timeline-storage §4's range rules. `Timeline`, `TimelineStore`, event
types, fixtures and every capability other than TimelineTree are unchanged; no event, service, method, configuration
or dependency is added.

**Rule check.** As in §10, with one difference: the change is in TimelineTree, the capability that owns restore, not
in the runtime's infrastructure (principles §4 rule 8 does not apply). TimelineTree reads the Timeline it built through
the public read API, as it already reads a parent's history to validate a cut (rule 7).

**Acceptance.** Behaviour, in CI: §14 item 1 as amended, checked in the context flow's restore with a child
(`manual_compaction.rs`): `main`'s own history is read once from its start, the child's once, and the child's
creation check reads one page that holds less than its history. The existing restore flows, including interrupted
creates and failed directories (`branch_flow.rs`), pass unchanged. Measured locally and reported in the
implementation PR: restore with first idle against `main` on the same machine, and the peak heap unchanged.

**Measured effect.** Release builds of `restore_cost` on one Apple M4: `main` at 806099d3 and this change, on the same
disposable stores, in four interleaved rounds of 15 restores per size; mean of the round medians, which agreed within
5%. Restore with first idle, ms:

| Mix | Events | `main` | This change | Ratio |
| --- | --- | --- | --- | --- |
| plain | 1,739 | 18.8 | 18.5 | 0.98 |
| plain | 3,739 | 40.2 | 29.0 | 0.72 |
| plain | 10,739 | 132.6 | 90.0 | 0.68 |
| product | 1,385 | 23.0 | 19.3 | 0.84 |
| product | 3,383 | 54.1 | 38.9 | 0.72 |
| product | 10,393 | 174.3 | 120.0 | 0.69 |
| plain, child cut at `main`'s head | 3,018 | 62.1 | 52.7 | 0.85 |
| plain, child cut at `main`'s head | 10,018 | 221.3 | 181.3 | 0.82 |

Store reads per restore besides validation: plain 3 calls and 2.0 full-history reads to 2 and 1.0, product 8 and 2.0
to 7 and 1.0, with a child 8 and 4.0 to 7 and 3.0 (the child's ancestry is still read by read C and its fill). The
peak heap and the heap at the first idle were unchanged within 0.01 MB, and no opening was alive at the first idle.
At plain 1k the time saved goes to the storage writer's round pacing: the `branch/resume` append waits 7.9 ms instead
of 4.0 ([#238](https://github.com/arcships/dim/issues/238)).
