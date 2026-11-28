# Idle Session unload

Status: Implemented in [#331](https://github.com/arcships/dim/pull/331). The implementation corrected one statement in
place: under the admission gate the pass checks the acquire count again, not `uses` (§6.1, step 4). It also added two
cases to §7: a Session of an owned Agent closing while another is unloaded, and the delete of an unloaded Session, which
Agents announces to Collaboration with a process event. An owned Agent's record follows its member Session rows
([Collaboration §6](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)), so a close keeps it and a delete
removes it with the last member. §2 ends with its measurement. [#345](https://github.com/arcships/dim/issues/345)
bounds the admission slots: a mark holds its Session's slot, and each pass ends by removing the slots nothing needs
(§6.1, §6.4, D12). Roadmap:
Agents lane ([#126](https://github.com/arcships/dim/issues/126)), the target of
[S2 §8.6](sessions-service.md#86-target-idle-unload-follow-up). It reopens S3's Session activity handle
([#57](https://github.com/arcships/dim/issues/57), [ShellInput A.5](../plugins/ShellInput.md#a5-changes-to-other-plugins))
as a runtime hold. Required before Scheduler's `create`-mode jobs ([P6](../plugins/Scheduler.md),
[#70](https://github.com/arcships/dim/issues/70)) and Workflow's repeated stage creation ([P7](../plugins/Workflow.md),
[#71](https://github.com/arcships/dim/issues/71)) ship. Scope: [ADR 002](../../adr/002-plugin-scope.md) process scope,
inside the existing Agents plugin, with one runtime requirement and three owner adoptions (§11).

## 1. Summary

The App Server never unloads an idle Session: it stays loaded until it is closed or the process exits
([lifecycle §5](../../architecture/rust-agent/lifecycle.md#5-stop-与-close)). Scheduler and Workflow launch a Session
per run or stage, so loaded Sessions grow without bound (§2). This design makes Agents unload them.

- **Rule.** Agents unloads a loaded Session once no lease (`SessionUse`) has held it for at least the grace period
  `G`, and `Session::unload_if_idle()` releases it. The next `acquire` restores it. Unloading is not closing: the
  unload writes no event (the restore opens the branches with `branch/resume`, as after a restart), and every user of
  the Session finds it as it was.
- **What holds a Session.** Only leases (§4): an attached client, an Rpc or `sessions` call in flight, and a
  Collaboration operation or open request on a member. Agents interprets nothing else.
- **Detached work** (open question 1). A running `!` command, BackgroundExec task or open Collaboration request blocks
  the unload. Its owner takes a runtime **activity hold** while the work runs, and `unload_if_idle()` refuses while
  any hold exists (§5, requirement R1 to the runtime, adopted by R2 to R4). An automatic unload never cancels
  accepted work.
- **When** (open question 2). A periodic pass every `G` (§6). It is level-triggered: it covers both a Session whose
  last lease dropped and a lease-free Session that becomes idle later, and it loses no edge. `G` defaults to 60 s and
  is host configuration, not a user setting.
- **Races.** The pass unloads under the Session's admission gate, the gate `acquire`, creation and delete already
  use, so an `acquire` during an unload waits and then restores (§7).
- **Scope.** Every Session of a host with a native store: client, launched, member, fork. Without a native store the
  runtime cannot restore a Session, so nothing is unloaded.

No new Rpc method, Timeline event or fact, storage format or client-visible API. Inside the process, Agents tells
Collaboration that an unloaded Session was deleted, as it tells it of a restore (§7). Nothing is raised to rutis.

## 2. Problem and evidence

S2's spike measured a loaded idle Session ([S2 §2](sessions-service.md#2-spike-findings), finding 6; `xtask measure`,
release build, mock model, Apple M4): about 300 KB of Rust allocations and 43 Tokio tasks, and creation slows as they
pile up.

| Sessions loaded | Mean creation time |
| --- | --- |
| 10 | 1.5 ms |
| 100 | 4.0 ms |
| 1000 | 57 ms |

One Scheduler job every five minutes leaves about 288 loaded Sessions a day: roughly 86 MB and 12,000 tasks. S2 D14
bounds storage and the Session list by deleting run Sessions when their records expire, not memory: the retained runs'
Sessions stay loaded.

What exists today:

| Piece | State |
| --- | --- |
| `Session::unload_if_idle()` ([agent.rs](../../../crates/dim-agent-runtime/src/agent.rs)) | Takes each branch's commit lock in id order, checks Loop idle, nothing unpublished and dispatch at the tail, then freezes every branch at that tail and releases the instance with the close's write window. No production caller; refuses for an in-memory store, a fault or a closing Session |
| Restore ([agents.rs](../../../crates/dim-app-server/src/agents.rs) `acquire`) | Under the Session's admission gate: authorizes the project, restores the Session from the native store, emits `SessionRestored` |
| Leases | `SessionUse` counts uses per Session (`SessionSlot.uses`); the count is maintained but never read |
| Detached work on unload | Not checked by the runtime ("first-release limit" in BackgroundExec's test). BackgroundExec writes `exec/ended {cancelled}`, which never wakes; ShellInput terminates the run and writes nothing; Collaboration settles an open request `failed` and interrupts the member |

A restore is not free either. It rebuilds the Session scope and every branch, replays their history into the
capabilities that fold it, and opens each branch with `branch/resume`, as after a restart. A throwaway measurement
for this design (release build at `c044140`, `SqliteTimelineStore` on a disposable database, `defaults::mock_hosts`
and `defaults::install`, Apple M4; one Session, unloaded and restored seven times per size; creation on the same
store for comparison; nothing committed):

| Events on `main` | Restore and first `idle`, median | Create | Unload |
| --- | --- | --- | --- |
| 1 | 22 ms | 23 ms | 1.0 ms |
| 101 | 22 ms | 23 ms | 0.5 ms |
| 1,001 | 51 ms | 22 ms | 0.2 ms |
| 10,001 | 2.27 s | 28 ms | 0.2 ms |

In the product a restore also authorizes the project, binds the Session's TypeScript parts and reacquires MCP
connections, which this measurement leaves out. Two consequences for the design: a restore per call would cost tens of
milliseconds to seconds and add a `branch/resume` each time, which is what the grace period avoids (§6.2); and the
first use after an unload pays one restore, as the first use after a restart does today. Restore time grows faster
than history (ten times the events, about 45 times the time between the last two rows). That is a restore performance
question for its owners, not a prerequisite of this design.

Measured with the implementation ([#331](https://github.com/arcships/dim/pull/331); release build, mock model, native
store, Apple M4; runs launched through `sessions` and dropped): a loaded run Session costs 51 Tokio tasks and about
342 KB. Once unloaded, all 51 tasks are gone and about 100 KB per load stays allocated. A close leaves the same, and so
does the bare runtime (`xtask measure`, `allocationBytesAfterClose`), so that remainder is runtime memory kept per
Session load, a follow-up for the runtime outside this design.

## 3. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | Only leases hold a Session; Agents reads nothing but its own lease count, the runtime's `unload_if_idle()` and, from R1, the holds it checks | Agents checks each holder's own state (subscriptions, relations, runs) | One mechanism every holder already uses (§4). Reading plugin state from Agents is plugin-specific handling in a core part (principles §4 rules 4 and 7, ADR 002 D3) |
| D2 | A Collaboration relation alone does not hold its member; an open request does (it already holds a lease on the member) | Relations pin members | Members of long-lived Teams would stay loaded until the leader closes: the leak this design removes. Relations are process memory, survive an unload, and the next request restores the member |
| D3 | Accepted detached work blocks an automatic unload | Unload anyway, as `unload_if_idle()` does today and ShellInput §4 accepts | A user who disconnects would lose a `!` run or background command, a BackgroundExec end would be written `cancelled` and never wake the model, and a Collaboration request would fail with its member interrupted. Detaching a client is not closing a Session (AGENTS.md rule 8), and an unload must not behave like one |
| D4 | Detached work is recognised by a runtime activity hold its owner takes; `unload_if_idle()` refuses while any is held, in the same step as its branch checks (R1) | (a) Agents reads owners' facts, such as `exec/started` without `exec/ended`; (b) S3's original handle, provided by Agents; (c) owners hold a `SessionUse`; (d) owners keep Loop busy | (a) ShellInput writes no fact for a run, and the host would interpret plugin events. (b), (c) dim-agent-capabilities cannot depend on App Server types, other hosts that call `unload_if_idle()` would stay unprotected, and the check would sit outside the runtime's commit-lock step. (d) changes `idle`, which excludes detached work by contract (S2 D10) |
| D5 | A level-triggered pass every `G` decides; it unloads a Session lease-free since the previous pass whose `unload_if_idle()` succeeds | (a) Edge-triggered: a timer on the last lease drop plus a runtime "became quiescent" watch; (b) unload at the last lease drop | (a) needs a new facade notification and one watcher task per lease-free Session, and a lost edge leaves a Session loaded forever; it still needs the grace timer. (b) misses a Session that becomes idle after its last lease dropped (submit and leave, `timelineContext.input` without `idle`) and thrashes on call sequences. [Orleans' activation collection](https://learn.microsoft.com/en-us/dotnet/orleans/host/configuration-guide/activation-collection) works the same way: an idle age, checked by a periodic scan, with calls as activity and `DelayDeactivation` as a hold |
| D6 | Grace `G` = 60 s, `ServerConfig.idle_unload_after` | No grace; a count bound (keep the N most recent lease-free Sessions); a user setting | Without a grace an S2 run restores its Session for nearly every call (§6.2). A count bound keeps N Sessions loaded forever and picks N as arbitrarily as a time. No user need is known for a setting (ADR 002 D4); tests need a short value, so it is host configuration beside `startup_timeout` |
| D7 | The unload runs under the Session's admission gate; the pass never waits for a gate | A separate unload lock, or an "unloading" flag `acquire` checks | The gate already orders `acquire`'s restore, prepared creation and delete (S5c §7). A second lock invites lock-order bugs with them, and a flag needs its own wait |
| D8 | Metadata reads that do not take a lease (`session.summaries`) fall back to the store when the loaded Session they found unloads under them; a Collaboration peer history read acquires its peer under [read ownership](../timeline-read-ownership.md) | Take a lease for each listed Session | A lease would restore and pin every listed Session; a peer history read targets one authorized Session |
| D9 | Withdrawn by [ADR 005](../../adr/005-observability.md), which removed the trace archive: no Session holds a lease for observation | — | A run is read afterwards through the Timeline and process logs, which an unload and restore do not interrupt |
| D10 | SessionTitle's generation and an S5b `setTitle` take no hold | Holding until the title is written | SessionTitle's design accepts that an unload cancels generation before the store handover and retries on the next adopted input ([SessionTitle §4](../plugins/SessionTitle.md#4-timing-and-conflicts)). Its 30 s budget is below `G`, so a submit-and-leave Session finishes its title before an unload can start. An admitted `setTitle` is SessionStore's process-scope operation and settles whether or not the Session is loaded |
| D11 | No client API and no metric; a debug log line per unload and a warning per failed one | A `session.unload` method, an unload event or counter | No consumer. `agent.list` already reports each Agent's loaded Session count |
| D12 | Agents removes a Session's admission slot once nothing needs it: nothing holds it, its Session is not loaded, and it records no close the process must keep (§6.4). Each pass, each delete and each failed creation ends by removing such slots | (a) Keep every slot, as before [#345](https://github.com/arcships/dim/issues/345); (b) keep the closed fact of a Session whose rows remain in a smaller record than its slot; (c) remove a slot when its last lease drops, or from a task of its own | (a) The map grows by one slot per Session id the process has seen, and Scheduler and Workflow create Sessions continually. (b) It would save under a hundred bytes per closed Session, but every reader of the flag, `acquire` under the gate included, would read a second structure that any slot made again must agree with. These slots grow only with the closed Sessions whose rows remain. (c) A slot cannot go while its Session is loaded, and only the pass unloads; a check at each lease drop would lock the index for nothing, and `SessionUse` would carry the directory. A task of its own would repeat the pass's period |

## 4. What holds a Session

A holder takes a lease with `AgentIndex::acquire`, which restores the Session if needed, and drops it when done.

| Holder | How it holds today | Holds? |
| --- | --- | --- |
| Attached client (`session.attach`, local or through RemoteGateway and AcpGateway) | The subscription keeps the attach call's lease until detach, replacement, `Closed` or connection close | Yes |
| Rpc call on a Session: `session.submit`, `history`, `input_lookup`, `stop`, `idle`, `approval.respond`, branch create, the execution methods, `session.methods` and every `sessionMethods` call (`shell.*`, `session.rename`), `session.close` of an available Session | A lease per call | Yes, while the call runs |
| `sessions` call (`input`, `lookup`, `history`, `withdraw`, `stop`, `idle`; `launch`'s starter read) | A lease per call (S2 §6) | Yes, while the call runs |
| Collaboration operation in flight (`Peer` from `lookup`) | A lease on the peer | Yes, while it runs |
| Collaboration request open on a member (delivered, no result yet) | `Request` holds a lease on the member | Yes, on the member. The leader's side is detached work: R4 |
| Collaboration relation, no open request | Process memory; survives an unload | No (D2) |
| `session.fork` source | A lease while the fork is created | Yes, while it runs |
| `session.delete`, `sessions.delete` | The admission gate | Serialized with the unload (§7) |
| Open turn: model or tool call running, pending approval or question, input unpublished or undispatched | Runtime state | Blocks `unload_if_idle()` |
| Running `!` run, BackgroundExec task, the leader side of an open Collaboration request | Nothing today | Blocks through a hold (§5) |
| C4 leaf observers (rows listening to `session/{sessionId}`) | The rows unload with the Session; the restored Session's rows listen again ([C4 design](../session-typescript.md#native-session-broadcast)) | No: they observe |
| C6 notice listeners | Attached clients only | Through the attachment |
| `session.summaries`, `agent.list`, Collaboration's Team status | Non-restoring reads | No: they tolerate an unload (D8) |
| Collaboration `agent` peer history read | An acquired peer Session lease | Yes, while the read runs; acquisition restores an unloaded peer |
| Scheduler and Workflow run records | Their own records; per-call leases | No: S2 D12, and they must not add an unload loop |
| SessionTitle generation, S5b `setTitle` | Session part task; SessionStore operation | No (D10) |
| Processes a `!cmd &` left in the background | Outside ShellInput after the run ends ([ShellInput A.2](../plugins/ShellInput.md#a2-execution)) | No: nothing in Dim tracks them, and no unload reaches them |

S2 §8.6 listed "a Collaboration member relation" as a holder. D2 narrows it to an open request, which already holds.

## 5. Detached work

### 5.1 Activity hold

The runtime adds a generic hold to `SessionLifecycle`, the Session service that Session and branch parts
reach as `Keyed<SessionLifecycle>` (R1):

```rust
impl SessionLifecycle {
    /// None once the Session is closing or being released.
    pub fn hold(&self) -> Option<SessionHold>;
}
/// Dropping it releases the hold.
pub struct SessionHold { /* private */ }
```

An owner takes one hold for each piece of accepted detached work, before the open turn or lease that admitted the work
ends, and drops it once the work's last fact is committed. The hold carries no identity, result or cancellation; the
owner keeps its task, as lifecycle §4 requires. `unload_if_idle()` refuses while any hold exists. Its hold check and
its decision are one step: a hold taken before the decision makes it refuse, and a hold requested after it returns
`None`, as on a closing Session. `Session::close()` ignores holds and ends the work as it does today. `Session::idle()`,
`session.idle` and `timelineContext.idle` are unchanged: they still exclude detached work (S2 D10).

| Owner | Takes the hold | Drops it | After the drop |
| --- | --- | --- | --- |
| BackgroundExec (R2) | In `start`, before `exec/started` is appended (a tool call, so a turn is open) | When the supervising task has committed `exec/ended`, or the start was refused | `exec/ended` is undispatched until its handler has appended the wake input, so the branch is not idle until the woken turn ends |
| ShellInput (R3) | At the second admission, with the spawn and registration under the registry lock (the Rpc call holds a lease then) | When the run task ends: after the recorded input's append returns, at exit in `private` mode, or on termination | The recorded Append input is applied at dispatch and starts no turn |
| Collaboration, leader side (R4) | With the `Request` that holds the member's lease, created while the leader's tool call waits for its delivery | When `collaboration/result` is committed and the `Request` removed | The result's dispatch posts the result input, which wakes the leader |

Every hold is therefore taken while a lease or an open turn already keeps the Session loaded, and released only after
a fact that keeps the branch busy until dispatch has absorbed it. No gap opens in which an unload could cancel the
work or lose its wake.

```text
 client      Rpc lease   ShellInput run           pass (every G)               Session S
   |-- shell.run -->| 1  | admit+spawn: hold 1    |                            |
   |-- disconnect   | 0  | running                | lease-free: mark           |
   |                |    |                        | unload_if_idle() -> false  | (hold)
   |                |    | exits; append X        |                            | input/message X (append)
   |                |    | hold 0                 |                            | X applied: idle
   |                |    |                        | unload_if_idle() -> true   | released, no event
```

### 5.2 What does not change

- A reload of the owner, a Session close and App Server shutdown still end the work as their designs state (ShellInput
  §3.2, BackgroundExec, Collaboration `unload`). A hold only stops the automatic, idle release.
- An explicit `unload_if_idle()` by another host gets the same protection. No caller can unload a Session with
  accepted detached work; one that needs it gone closes it.
- Work that holds no hold is not protected. A future owner of detached work takes a hold the same way, or states why an unload may end its work. A TypeScript owner would need C4 to
  expose the hold; none needs it today.

## 6. When to unload

### 6.1 The pass

The Agents index generation runs one pass every `G` while it is active and has a native store. Each pass visits every
loaded Session of every live Agent:

1. The slot is closed (closed or deleted Session): skip.
2. `uses > 0`: forget the Session's mark.
3. `uses == 0` and no mark, or a mark that differs from the slot's acquire count: mark it with the count.
4. `uses == 0` and the mark equals the acquire count: no lease was taken since the previous pass, so the Session has
   been lease-free for at least `G`. Try the admission gate without waiting; a taken gate means a restore, creation or
   delete is in progress, so skip. Under the gate, check the count again (every lease starts with an `acquire` under
   the gate, so an unchanged count also means `uses == 0`), then call `unload_if_idle()` and keep the gate until it
   returns.
   - `true`: unloaded. Forget the mark.
   - `false`: busy (turn, publication, dispatch, a hold, closing, a fault). Keep the mark; the next pass tries again.
   - An error: the runtime has released the instance and reports cleanup errors. Log a warning and forget the mark.
     The next `acquire` restores the Session, and the restore settles what the release could not write.
5. After visiting every loaded Session, replace the marks with the ones this pass set, then remove every admission
   slot that can go (§6.4): those of the Sessions this pass unloaded, and those other callers released since the
   previous pass.

The slot gains one counter, incremented by every `acquire`. Marks are memory of the pass, rebuilt after an index
reload. A mark holds the slot whose count it records, so that slot stays until the next pass compares the count: a
slot made anew in between would restart its count, and an unchanged count would no longer prove that no lease was
taken. A pass unloads one Session after another, so a slow release delays the pass and that Session's next
`acquire`, never a request on another Session.

```text
                          acquire
          +-----------------------------------------------+
          v                                               |
   +--------------+   last lease drops   +--------------------------+
   |     Held     | -------------------> |        Lease-free        |
   |   uses > 0   | <------------------- | uses == 0, acquire count |
   +--------------+       acquire        +--------------------------+
                                                    | a pass finds the count unchanged
                                                    | since the previous pass
                                                    v
                                     +------------------------------+
                                     | try the admission gate       |-- taken: skip
                                     | count still unchanged?       |-- no: skip
                                     +------------------------------+
                                                    | yes, gate held
                                                    v
                                     +------------------------------+  false: busy (turn,
                                     |  Session::unload_if_idle()   |  hold, publication,
                                     +------------------------------+  fault); next pass
                                                    | true
                                                    v
                                     +------------------------------+
                                     | Unloaded: stored only; gate  |
                                     | released; next acquire       |
                                     | restores                     |
                                     +------------------------------+
```

Inside `unload_if_idle()` with R1:

```text
 holds > 0 ?                                            yes -> false (no lock taken)
 skip each branch whose own close has finished: it commits nothing more
 take each other branch's commit lock in branch-id order
 each branch: not closing, Loop Idle, nothing unpublished,
              dispatched == tail ?                                   no  -> unlock, false
 holds == 0, and refuse new holds (one step with the check) ?        no  -> unlock, false
 freeze every branch at its tail, start the release (closing, no stop events)
 settle as an idle release (finalizers may still write), remove the instance
 -> true
```

### 6.2 Grace period

Without a grace, an S2 run unloads and restores its Session between almost every pair of calls: `idle` replies at
the first idle point and drops its lease, and each `history` page that follows restores the Session, replaying its
history and writing a `branch/resume`, only for the pass to release it again. Each such restore costs 22 ms to
seconds (§2). `G` must outlast such gaps and stay small enough to bound memory.

| Bound | Gap or cost | Effect on `G` |
| --- | --- | --- |
| Lower | Consecutive calls of one consumer (S2 `input`, `idle`, `history` pages; a client reading without attaching): milliseconds | Any `G` covers them |
| Lower | A client reconnecting (CLI restart, network drop) re-attaches within seconds | A few seconds |
| Lower | A Collaboration leader's next request to the same member follows a model turn: seconds to tens of seconds | Tens of seconds |
| Lower | SessionTitle's generation budget is 30 s (D10) | Above 30 s |
| Upper | Released Sessions stay loaded between `G` and `2G`: at most the Sessions released in the last `2G`, about 300 KB each (§2) | At one job every five minutes, any `G` up to 2.5 minutes keeps at most one released run Session per job loaded; a Workflow fan-out of 20 stages holds about 6 MB for at most `2G` |

Any value from about 30 s to a few minutes meets both bounds; 60 s is the shortest whole minute above the title
budget, and it covers a leader's turn. It is `ServerConfig.idle_unload_after` (a positive `Duration`, default 60 s), set by
the host like `startup_timeout`; flow tests set it to a few hundred milliseconds. Users have no setting. For
comparison, Akka Cluster Sharding passivates an entity idle for 120 s by default
([akka#26787](https://github.com/akka/akka-core/pull/26787)), and Orleans collects activations idle for 15 minutes,
scanning every minute.

### 6.3 A Scheduler-style run

```text
 Scheduler          sessions / AgentIndex           slot S (uses, count)     pass            Session S
   |-- launch ------->| create S (prepared, gate)     | 0, 0                 |               | loaded
   |-- input -------->| acquire                       | 1, 1                 |               | turn starts
   |<- receipt -------| drop lease                    | 0, 1                 | mark 1        |
   |-- idle --------->| acquire, wait                 | 1, 2                 | uses 1: skip  |
   |<- {C} -----------| drop lease                    | 0, 2                 |               | idle
   |-- history x3 --->| acquire / drop, no restore    | 0, 5                 | mark 5        |
   |                  |                               |                      | 5 unchanged:  |
   |                  |                               |                      | gate, unload->| released
   |-- delete (retention, later) -> gate; S not loaded: rows removed, nothing restored
```

A `reuse`-mode job whose period is shorter than `G` keeps its Session loaded; a longer period restores it once per
run.

### 6.4 Admission slots

Agents keeps a Session's gate, lease count, acquire count and closed flag in one admission slot per Session id
(`SessionSlot`). A slot stays while any of these holds:

| The slot stays while | Why |
| --- | --- |
| Something holds it: a caller at its gate (`acquire`, a creation, a delete, an unload), a lease, or a pass's mark | Callers of one Session must wait on one gate, and the pass must see every lease. A slot made again for the id would have a gate of its own and a fresh count (§6.1) |
| Its Session is loaded | The pass reads a loaded Session's leases and acquire count from its slot |
| Its Session was closed in this process and its rows remain | The closed flag keeps the Session from loading again in this process ([S5c D5](session-deletion-and-fork.md#16-design-changes-in-implementation)) |

Every other slot goes: the slot of a Session the pass unloaded or that was released some other way, and the slot of
a deleted Session, whose rows and owner are gone, so `acquire` answers `session_closed` without it. The map holds
the slots of loaded Sessions, of closed Sessions whose rows remain, and of what callers hold, plus, until the next
pass, the slots released since the previous one.

- **Creation.** Only `acquire`, `session.close`, a creation (`admit`) and a delete create a slot. `acquire` and
  `session.close` look up the Session's owner and create the slot in one step under the index lock, so once a delete
  has forgotten a Session, nothing gives it a new, open slot. Reads that only check the closed flag (`session.summaries`,
  `session.summaries`, Rpc's close check and Collaboration's Team status) look the
  slot up and create none.
- **Removal.** Only under the index lock, and only when the map holds the slot's sole reference, its Session is not
  loaded, and it records no close to keep. Every other reference is taken under that lock or cloned from one a holder
  already has, so the count cannot rise during the check, and the slot any holder has is the one in the map.
- **Deletion.** A delete that removed the Session's rows marks the slot deleted as well as closed: a caller still at
  the gate finds it closed, and nothing keeps the slot once the callers are gone. A `session.close` marks it closed
  only, and that slot stays until a delete removes the rows.
- **When.** Each pass removes every slot that can go at its end (§6.1), and so does each delete and each failed
  creation, so their own slot goes then unless another caller still holds it. A slot left that way goes, at the
  latest, with the first pass after it is released. No task is added.

Without a native store no pass runs and nothing is restored: `session.close` drops its Session's slot, and each
creation drops those of Sessions no longer loaded, as before.

## 7. Races and interactions

| Scenario | Order | Outcome | Owner |
| --- | --- | --- | --- |
| `acquire` during an unload | Pass holds S's gate | `acquire` waits for the gate, finds S not loaded, restores it and proceeds; the call never sees a half-released instance | Agents |
| Unload attempted during a restore, creation or delete | Gate taken | The pass skips S | Agents |
| Lease taken and dropped between two passes | Acquire count changed | No unload at the next pass; the grace restarts | Agents |
| A pass while `acquire` restores S or waits for its gate | The `acquire` holds S's slot | The slot stays; the lease the restore ends with is on the slot the pass reads, so it holds S (§6.4) | Agents |
| S released outside the pass after its mark (a fault, another caller of `unload_if_idle()`), then restored within the grace | The mark holds S's slot | The slot and its count survive every removal; the restore raises the count, so the next pass does not unload S | Agents |
| `acquire` or `session.close` of S while a delete forgets S | Owner lookup and slot creation are one step under the index lock | Either it takes the slot before the delete forgets S, and `acquire` then finds it closed at the gate, or it finds no owner. A deleted S gets no new slot and is never restored: `acquire` answers `session_closed` | Agents |
| A lease held across the delete of S | The delete cannot remove the slot | The slot stays, closed and deleted, until the lease ends; the next pass removes it at the latest | Agents |
| `session.delete` / `sessions.delete` during an unload | Delete waits for the gate | Finds S not loaded and removes its rows without restoring it; S then ends as for the delete of an unloaded Session below | Agents (S5c) |
| `session.close` during an unload | Close acquires an available S, so it waits for the gate | Restores S and closes it with its close facts | Rpc, Agents |
| Agent close or process shutdown during an unload | Not gated | Joins the release already under way, as concurrent closes share one flow (lifecycle §5) | Runtime |
| Agents index reload during a pass | Old generation stopping | The pass stops; a release under way finishes before the old generation's cleanup returns; the next generation marks again | Agents |
| `session.summaries` while S unloads | S found loaded, then released | The loaded read falls back to stored metadata (D8), retaining S in the list | Agents, Rpc |
| Collaboration `agent` peer history read while S unloads | Acquisition waits for the admission gate | Restores S and holds its lease through the read; an acquisition failure refuses the read | Collaboration, Agents |
| Collaboration Team status of an unloaded member | Any | `idle`, as for any available Session | Collaboration |
| Collaboration request to an unloaded member | Leader tool call | `lookup` restores the member; `SessionRestored` lets Collaboration watch it again; relations kept | Collaboration |
| Last Session of an owned Agent unloaded | Any | The Agent stays: unload keeps the Session's rows, so its record keeps the Agent, and its instance stays loaded | Agents |
| A Session of an owned Agent closes while another is unloaded | Business close | The Agent stays: the closed member keeps its rows, so the record keeps the Agent. The leader's next request restores the unloaded member | Agents, Collaboration |
| `session.delete` / `sessions.delete` of an unloaded Session | Delete under the gate; S neither loaded nor closed in this process | Agents removes S's rows and emits `UnloadedSessionDeleted` with the Agent and Session ids, as it emits `SessionRestored` for a restore. Deleting S's last member Session also closes the Agent and removes its record. Collaboration drops S's relation when it observes the event, and an owned Agent left with no member is removed at startup | Agents, Collaboration |
| The delete of an unloaded Session ends while Agents reloads | `sessions.delete` runs to its own outcome across the reload, which reloads Collaboration | Agents emits `UnloadedSessionDeleted` on the process root, so a delete that ends after the next Collaboration subscribed reaches it. One that ends while Collaboration reloads reaches no listener. Collaboration therefore reconciles once it has subscribed, before it provides its service: it rebuilds the relations from the stored leaders, so a member whose rows are gone is no longer one | Agents, Collaboration |
| C4 leaf observers | Any | The rows stop admitting work when their `closing` settles and unload with the Session; the restored Session's rows listen again | C4 |
| C6 notice while unloaded | No listener | Dropped, as for any Session without an attached client; a client that attaches reads current state through `session.summaries` ([C6](https://github.com/arcships/dim/pull/85)) | C6 |
| S5b `setTitle` admitted before an unload | Any | Settled by SessionStore regardless; its notice is dropped if S is unloaded by then | SessionStore |
| Pending approval in a launched Session nobody watches | Turn open | Never unloaded; Scheduler or Workflow ends the run with `stop` | Runtime |
| Queue inputs left held by S2 §6.4 item 2 (Loop reload between `input/applied` and `stop/interrupt`) | Loop idle | The unload may proceed; the restore rebuilds InputQueue and Loop from history, which decides the held inputs as after a restart | InputQueue, Loop |
| Fork of an unloaded source | `fork` acquires it | The source is restored; source and fork are each unloaded after `G` if unused | Agents (S5c) |
| Restore checks | Any | As after a restart: project authorization, stored Agent available. A Session that would not load after a restart does not load after an unload | Agents |
| Per-generation memory of Session parts (ShellInput's used `runId`s, SessionTitle's attempt count, MCP connections, TypeScript part state) | Any | Reset, as after a restart; each owner's design already covers a reload | Owners |
| Faulted or closing Session | Any | `unload_if_idle()` refuses; the supervisor or the close ends it | Runtime |
| A branch closed on its own (`Branch::close()`) while its Session stays | Any | `unload_if_idle()` refuses while the branch is closing and skips it once closed; the restore opens it again with `branch/resume`, as after a restart | Runtime |
| Host without a native store | Any | No pass runs; the runtime cannot restore an in-memory Session | Agents |

## 8. Agents lane changes

| Part | File | Change |
| --- | --- | --- |
| `SessionSlot` | [agents.rs](../../../crates/dim-app-server/src/agents.rs) | An acquire counter beside `uses`, incremented by `acquire`. From [#345](https://github.com/arcships/dim/issues/345), a deleted flag, and removal once nothing needs the slot (§6.4) |
| Unload pass | agents.rs | Started by `AgentsPlugin` for its index generation when a native store is present; stopped and drained by the generation's cleanup. Its marks hold their slots, and it ends by removing the slots that can go (§6.1, §6.4) |
| Closed-flag reads | agents.rs `session_catalog`, `session_available` | Look the slot up instead of creating it (§6.4) |
| Lease-free reads | agents.rs `session_catalog`, `session_summary` | Fall back to the store when the loaded Session is released under them (D8) |
| Delete of an unloaded Session | agents.rs `delete`, `UnloadedSessionDeleted` | Once the rows of a Session that was neither loaded nor closed are gone, announces its Agent and Session ids to the owners of process state. It emits on the process root, because a delete can outlive its index generation. Collaboration listens as it does for `SessionRestored`, ends the Session as after a close, and reconciles at load what a reload made it miss (§7) |
| `ServerConfig.idle_unload_after` | App Server `lib.rs` | One field with its default, in the assembly lane (zerob13), as S2 added one line there |

## 9. Observability

The release emits the runtime's existing `session.close` span in the process log
([ADR 005](../../adr/005-observability.md) D4). Agents logs each unload at debug level and each failed one as a warning,
with the Session id. `agent.list` already reports each Agent's loaded Session count, which is what the acceptance
measures. Nothing new reaches clients.

## 10. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (command events) | No event. The unload changes no other module's state; holders release their own leases and holds |
| §4 rule 2 (handover) | Nothing is taken back. Accepted detached work stays with its owner until its last fact |
| §4 rule 3 (before callbacks) | None |
| §4 rule 4 (no business meaning in core) | Agents counts leases; the runtime counts holds. Neither knows what a holder does |
| §4 rule 5 (batch) | No batch; the release freezes each branch at a committed tail |
| §4 rule 6 (model context) | Unchanged; an unload writes nothing the model reads |
| §4 rule 7 (public events and services) | Agents uses the Session facade (`unload_if_idle`) and its own leases; owners use the public `SessionLifecycle`. Collaboration learns of a restore and of an unloaded Session's delete from Agents' process events (`SessionRestored`, `UnloadedSessionDeleted`) |
| §4 rule 8 (infrastructure unchanged) | One lifecycle requirement (R1, §11.1), which closes a question lifecycle §5 and event-driven flows §3 left open; nothing to rutis |
| §4 rule 9 (Timeline facts) | Leases, holds and marks are process memory, never Timeline facts |
| §1 (App Server boundary) | Unloading is a host decision in Agents; capabilities only declare their work |
| ADR 002 D1 | Process scope: the pass spans Sessions and lives in Agents |
| ADR 002 D2 | No new plugin; the existing Agents index runs the pass |
| ADR 002 D3 | No core part depends on an optional plugin; the hold is a runtime service any part may use |
| ADR 002 D4 | No business configuration; `G` is host configuration (D6) |
| ADR 002 D5 | No new contract for clients or other languages; the hold is a Rust runtime service, and a C4 binding is added when a TypeScript owner needs one |
| ADR 003 | Not applicable: no agent action |

## 11. Requirements raised to mechanism owners

### 11.1 R1. Runtime: Session activity hold

Status: implemented. `SessionLifecycle::take_hold()` and `SessionHold` are in
[config.rs](../../../crates/dim-agent-runtime/src/config.rs), `Session::unload_if_idle()` checks them in the order §6.1
shows, and [session_lifecycle.rs](../../../tests/integration/tests/session_lifecycle.rs) carries the verification. No
owner takes a hold until R2 to R4.

| Item | Content |
| --- | --- |
| Blocked by | [Lifecycle §5](../../architecture/rust-agent/lifecycle.md#5-stop-与-close) defers whether in-flight detached work blocks an idle release and how it is recognised; `unload_if_idle()` does not check it, and owners cancel the work when they unload (§2). [Event-driven flows §3](../../architecture/rust-agent/event-driven-flows.md#3-后台任务停止与自动唤醒) leaves in-flight work recognition undesigned until a need arises; automatic idle unload is that need. D3 requires that an idle release never cancels accepted work |
| Existing mechanisms considered | `Session::idle` and Loop idle: exclude detached work by contract (S2 D10). Owners' public facts: ShellInput writes none, and a host reading them would interpret plugin events (rule 4). Agents' leases: an App Server type that dim-agent-capabilities cannot reach, checked outside the commit-lock step. `SessionLifecycle::closing()`: tells owners when to stop, not the runtime when not to release |
| Generic capability | `SessionLifecycle::take_hold() -> Option<SessionHold>`, reachable from Session and branch parts through `Keyed<SessionLifecycle>`. `None` once the Session is closing or being released. Dropping the hold releases it. It carries no identity or business meaning. `Session::unload_if_idle()` returns `false` while any hold exists; the hold check and the release decision are one step (a hold taken before it refuses the release; one requested after it gets `None`). A refusal caused by a hold takes no commit lock. `close()`, `idle()` and the facade's other calls are unchanged. Lifecycle §4's "no shared in-flight count" stays true for execution: a hold reports nothing and cancels nothing; it only vetoes an idle release. The foreign form, `hold(ctx)`/`release(id)`, registers the hold's cleanup and inserts its record under one lock: a row unloading at that moment either sees the record and drops it or takes no hold. Narrowing that lock lets the cleanup miss the record, and no later unload removes it, so every `unload_if_idle()` of the Session refuses |
| Effect on sequence, batch, interrupt, reload | None on sequence or batch: no event. Interrupt: unchanged; a branch close cancels the branch's detached work, and the owner drops its holds as the work ends. Reload: a reloading owner's work ends as today and its holds drop with it. Close: never delayed or refused by holds |
| Verification | A runtime test with a test capability: `unload_if_idle()` is `false` while it holds, `true` after the drop; a hold requested after a release has started is `None`; `close()` succeeds while a hold is held. The lifecycle §4/§5 and event-driven flows §3 updates come with it; the BackgroundExec page changes with R2 (§14) |
| Owner and issue | Runtime lifecycle, core mechanisms lane (eric8810, [roadmap §3.5](../roadmap.md#35-按人按批次)); tracked in [#126](https://github.com/arcships/dim/issues/126) until split |

### 11.2 R2 to R4. Owners take holds

Each adoption needs R1 and nothing else, and changes only its owner's crate. Until the unload pass ships, a hold has
no observable effect except that an explicit `unload_if_idle()` refuses.

| # | Owner | Change | Test and documents |
| --- | --- | --- | --- |
| R2 | BackgroundExec ([background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs)) | Add `Keyed<SessionLifecycle>` to its dependencies. One hold per task, taken in `start` before `exec/started` and dropped after the supervising task commits `exec/ended` or the start is refused; no hold (`None`) refuses the start as a closing branch does | `idle_unload_stops_tasks_and_a_restart_wakes_for_every_unreported_end`: the unload refuses while the task runs and succeeds after its end and woken turn; the recovery half is unchanged. The BackgroundExec page's unload sentence then covers reload and close only. `xtask check` |
| R3 | ShellInput ([shell_input.rs](../../../crates/dim-agent-capabilities/src/shell_input.rs)) | One hold per run, taken in the second admission with the spawn and registration and dropped when the run task ends; `None` refuses the run `session_closed` | A flow test: a `!` run continues after its client disconnects and is recorded while the host's unload is refused. ShellInput §3.2, §4.1, Appendix B and A.5 lose "a host's idle unload" as a way a run ends |
| R4 | Collaboration ([collaboration.rs](../../../crates/dim-app-server/src/collaboration.rs)) | The leader's `CollaborationSession` injects `Keyed<SessionLifecycle>`; each `Request` holds a hold on the leader beside its lease on the member, dropped when the request is settled and removed. `None` happens only while the leader is closing, which Collaboration already settles | A flow test: a leader with an open request and no client stays loaded until the member's result, then both unload after `G` |

R2 implemented ([#201](https://github.com/arcships/dim/pull/201)).
[background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) takes the hold in `start`
before `exec/started`, keeps it in the task's supervising task and drops it once `exec/ended` is appended, or with a
refused start; [local_execution.rs](../../../tests/integration/tests/local_execution.rs) carries the verification
(`idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end`, renamed from
`idle_unload_stops_tasks_and_a_restart_wakes_for_every_unreported_end`).

R3 implemented ([#200](https://github.com/arcships/dim/pull/200)).
[shell_input.rs](../../../crates/dim-agent-capabilities/src/shell_input.rs) takes the hold in the
second admission and keeps it in the run's registry entry, which the run task clears after the run's last fact;
[transport_flow/shell_input.rs](../../../crates/dim-app-server/tests/transport_flow/shell_input.rs) carries the
verification (`a_run_outlives_its_caller_and_holds_the_session_until_recorded`).

R4 implemented ([#202](https://github.com/arcships/dim/pull/202)).
[collaboration.rs](../../../crates/dim-app-server/src/collaboration.rs) takes the leader's hold with each `Request`, beside
its lease on the member, and drops it when the `Request` is removed after its `collaboration/result` is appended; a
closing leader gives no hold, and the request is refused `closed` before the member receives it.
[transport_flow/collaboration.rs](../../../crates/dim-app-server/tests/transport_flow/collaboration.rs) carries the
verification with explicit `unload_if_idle()` calls (`open_requests_hold_their_leader_until_the_last_result`,
`every_settlement_of_a_request_releases_its_leader`, `a_closing_leader_gives_no_hold_and_refuses_the_request`); the
test row's unload after `G` belongs to the pass's acceptance (§13, item 6).

C4, SessionTitle, Scheduler and Workflow need no change. Nothing is raised to rutis.

## 12. Dependency order

```text
 R1 runtime hold --+--> R2 BackgroundExec hold --+
                   +--> R3 ShellInput hold ------+--> Agents unload pass (this design) --> P6 create-mode jobs
                   +--> R4 Collaboration hold ---+                                     --> P7 repeated stages
```

Each step merges on its own. R1 alone changes only explicit `unload_if_idle()` callers, which are tests. R2 to R4
alone add holds that nothing acts on yet. The pass ships last, with D8 and the configuration field, and only after
R2 to R4, because before them an automatic unload would cancel detached work. The work starts once P6 or P7 is Ready
(roadmap §2, rule 5).

## 13. Acceptance

Flow tests of the implementation PR, on a disposable native store with `idle_unload_after` of a few hundred
milliseconds. Each waits on an observed transition (`Session::closed()` of the released instance, the loaded count in
`agent.list`, a call's reply) with a timeout, never on a sleep alone.

1. **Measured cost (#126).** A test plugin launches 50 Sessions through `sessions`, runs one input, `idle` and the
   `history` pages in each, then drops them. After the grace, `session.summaries` lists all 50 with their stored heads
   and `agent.list` reports none loaded. A later `timelineContext.input` restores one Session and its turn runs. Each
   `sessions.history` page acquires its Session, restoring it if unloaded. Paging through each run's original cutoff
   returns the same events as before unload. The implementation PR also reports `xtask measure` numbers, loaded count
   and tasks, before and after.
2. **No restore inside a run.** When the gaps between `input`, `idle` and three `history` pages stay within the
   grace period, each call acquires the same loaded Session and `main` gains no `branch/resume`.
3. **Acquire during an unload.** A test Session part holds its own unload until the test releases it, so a release
   is under way when `sessions.history` and an Rpc `session.history` arrive. Both wait, then succeed on the restored
   Session; neither replies `session_closed`.
4. **Unload is not close.** The unload leaves the stored head of every branch unchanged and writes no
   `stop/interrupt`; the restore adds only `branch/resume`. The Session stays in `session.summaries`, a Collaboration
   relation still routes the leader's next request, and a C4 observer receives the restored Session's events.
5. **Held subscription.** An attached client keeps the Session loaded past `2G`. After detach, or connection close,
   it unloads; a re-attach restores it, and events stream again.
6. **Detached work.** A BackgroundExec task, a `!` run and an open Collaboration request each keep their Session
   loaded past `2G` with no lease. When the work ends, its result is recorded (not `cancelled`), the woken turn runs,
   and the Session unloads after the grace.
7. **Pending approval.** A launched non-interactive Session with a pending approval and no lease stays loaded until
   `stop`.
8. **Reads during unloads.** `session.summaries` retains stored Sessions while they unload. A leader's
   `agent` read acquires and holds the member, waits for an unload and restores it before reading,
   and refuses the read if acquisition fails.
9. **Index reload.** An Agents index reload while a release is held under way as in 3: it completes, no Session is
   lost, the next generation unloads the remaining lease-free Sessions, and an `acquire` after the reload restores.
10. **Delete and fork.** `sessions.delete` of an unloaded Session removes its rows without restoring it. A fork of an
    unloaded source restores the source; both are unloaded after the grace.
11. **No native store.** With an in-memory store nothing is unloaded.
12. **Admission slots ([#345](https://github.com/arcships/dim/issues/345)).** Sessions are created, listed, unloaded,
    closed and deleted through Rpc. After each step Agents keeps only the slots of loaded Sessions, of closed Sessions
    whose rows remain, and of what a caller holds; listing creates none. A closed Session stays closed through the
    passes, and a lease held across a delete keeps its slot until the first pass after the lease ends. Two races:
    the mark of a Session released outside the pass keeps its slot, so a restore within the grace is not unloaded;
    and a pass during a restore keeps the slot the restore holds, whose lease then holds the Session. No client sees
    slots, so these tests ([slots.rs](../../../crates/dim-app-server/src/agents/tests/slots.rs)) count the map inside
    the crate and run each pass themselves.

R1 to R4 carry their own tests (§11).

## 14. Changes to other documents

With this design:

- [S2 §8.6](sessions-service.md#86-target-idle-unload-follow-up) (en/zh) points here and notes D2.
- [ShellInput A.5](../plugins/ShellInput.md#a5-changes-to-other-plugins) and Q5 (en/zh): the activity handle is
  reopened as a proposal (R3); the decision stays with ShellInput's owner.
- [Documentation index](../../INDEX.md) links this page.

With the implementation, by each owner: lifecycle §4 and §5 and event-driven flows §3 (R1); the BackgroundExec page
(R2); ShellInput §3.2, §4.1 and Appendix B (R3); S2 §1, §8.2, §8.6 and §15 (loaded Sessions are unloaded); this page's
status; the roadmap, which zerob13 keeps in sync.
