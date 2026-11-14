# Commands and members in the background-task list

Implementation in progress: [#324](https://github.com/arcships/dim/pull/324). Scope: command/member inventory, finished
rows and stop-all in [#264](https://github.com/arcships/dim/issues/264), also used
by [#263](https://github.com/arcships/dim/issues/263), on main including #299 and
#312. Member conversation uses the
[member conversation contract](member-conversation.md). Workflow presentation
follows its package's delivered run/control contract; this document defines no
Workflow API or placeholder rows.

## 1. Existing sources

Use the existing Session attachment and history, not a server task registry:

| Existing source | Meaning for the list |
| --- | --- |
| `session.history {sessionId,branchId,after,limit,through?}` | Branch-local, fixed-watermark history; read through the existing authorization and dispatch boundary |
| `branch/create`, `branch/start`, `branch/create-failed` | Discover branch identities and creation outcomes from their owner's facts |
| `session.state {sessionId,branchId,...}` | Discover published branches through the attachment's initial state watches and subsequent updates |
| `session.branches {sessionId}` | The complete line directory — open, ending or ended — with each line's state and head ([timeline branches §14](timeline-branches.md#14-clients)) |
| `session.event {sessionId,branchId,event}` | Live facts, with a separate sequence cursor for each branch |
| `exec/started`, `exec/ended` | Command identity, description and actual terminal result |
| `collaboration/request`, `collaboration/delivered`, `collaboration/result` | Delegation identity, actual member Session and outcome |
| `session.exec.stop`, `session.stop` | Existing addressed command stop and member current-turn interruption |

The [subscription](../../crates/dim-app-server/src/connection/subscription.rs)
initializes state watches from `Session::branches()` and follows new branches
signaled by broadcast events. On restore,
[TimelineTree](../../crates/dim-agent-capabilities/src/timeline_tree.rs) reads the
stored branch directory and publishes successfully restored branches; failed or
unfinished child creations are not published. Thus an existing child can be
discovered by its state notice without finding its creation in main's local history. A
reloading branch's state can be delayed until its Loop returns. Clients consume
these public notices; `session.branches` answers the complete directory, so a
client enumerates with one read and uses the notices for lines that open or end
after it rather than waiting for an enumeration notice. The public
[history handler](../../crates/dim-app-server/src/connection/tree.rs) reads
the named branch's own records; it does not
copy ancestor records into that branch's task list. The v3
[event filter](../../crates/dim-app-server/src/tree_protocol.rs) already exposes
the branch and task facts above.

## 2. Discover and fold tasks

Keep the existing task projection, canonical task identities and per-branch
cursors from #299. Seed main and enqueue every branch ID observed in
`session.state`, independently of the parent or its history cursor. Branch
identities in local `branch/create`/`branch/start` or live task facts supplement
discovery; read each discovered branch's own history and follow further identities
there. Main's local history is not a complete branch directory; `session.branches`
is ([timeline branches §14](timeline-branches.md#14-clients)). An explicit
create-failed fact settles that creation attempt;
an unavailable history read does not mean the branch is empty or finished.
Key discovery by branch ID within the current Session projection: repeated
history facts or state notices do not queue another initial read for a branch
already queued, loading, loaded or failed. Reopening the tasks panel retries a
known failed read as specified below; the first open of a line's transcript can
also restart its history through the same queue.

For each discovered branch, read from `after:0`, fix the first page's `through`,
then use `nextAfter` as the next `after` and that same fixed watermark until
`done`. Validate the complete response before folding its page or adopting its
cursor/watermark. An RPC error, malformed reply or cursor-invalid page adopts
nothing. History reads commit no server-side cursor; a repeated read uses the
same validated window. Fold that history before the live facts accumulated for
its initial read, deduplicating by `(branchId, sequence)`. Do not advance its cursor
past unread start facts merely because a later live event arrived first.

The worker gives each new attachment/projection a fresh local identity, even for
the same Session ID. Detach, replacement, a Session switch or connection teardown
invalidates the old identity before further controls or replies can use it; a
failed replacement does not reactivate it. Closing and reopening only the panel
preserves that projection and identity, including cursors and pending work, and
does not start another subscription. This identity is private client state.

Only one task-history page is outstanding per Session worker, across all branches
and projection replacements. Other RPCs or attachments do not share this
task-history limit. Other branch-history reads wait in the discovery queue;
a slow reply delays them until
it settles or the connection is torn down. The worker continues processing branch
discovery, leader notifications and controls through its asynchronous response
path; this does not promise concurrent history reads. Associate
each outstanding page request ID with its Session, branch and whether its originating projection is
still current. A Session switch or replacement attachment clears that current
flag and releases queued pages and buffered facts. Selecting the same Session
again does not reactivate old requests. Keep the outstanding slot until its
reply or connection teardown. A missing reply is not a failed page and cannot
trigger another request. Connection loss invalidates the projection; a new
attachment starts its branch reads at `after:0`, without assuming the unobserved
read changed a cursor. Only a reply for the still-current
projection can apply a page or queue its next one. Queued panel controls carry
the same originating projection identity; no server generation field is needed.
No polling timer, persisted index or generic observation framework is added.

A failed main-history read reports a connection failure and disables submission;
the unsent draft remains in the current UI, and recovery requires exit/resume. The following local-error rules
apply to child branches. Loading or failed branch reads are visible. Individual known targets remain
usable, but stop-all is disabled while any discovered branch's history is
loading or failed, including the transcript reread triggered by its first open.
A reply releases the global request slot. A failed page in the current projection keeps its last
validated cursor and watermark. Reopening the panel queues each failed page once,
after any outstanding read: resume from the last valid `nextAfter` (or `after:0`
if no page succeeded), retaining the fixed `through` if known. A first successful
page fixes `through` when it was not yet known. Reopening does not duplicate queued
or outstanding work. A queued retry carries the originating projection identity
and is discarded if that projection is invalid before send; an already-sent
retry uses the same stale-reply and slot-lifetime rules above. State notices and
live facts do not automatically retry a failed read; an unseen branch still
queues its first read. There is no retry timer or automatic retry loop.
For the [first open of a line's transcript](timeline-branches.md#14-clients),
reuse an outstanding initial read from `after:0` if it has consumed no page.
Otherwise restart that branch's history from `after:0` with a fresh watermark,
including after a failed discovery read, so the transcript receives the complete
history rather than only the task inventory's unread suffix. Previously folded
tasks remain deduplicated. Keep any superseded outstanding page in the global
slot until its reply, mark it noncurrent, and queue the fresh read behind it.
Later opens of the retained transcript do not restart its history.
On a child read failure, discard buffered event payloads and stop accumulating
live payloads for that failed read. After an explicit reopen finishes the
retained fixed window, enqueue one catch-up read from its validated `nextAfter`
with a fresh watermark, then fold buffered live facts. A failure before the
first successful page needs no extra catch-up: its reopened read already takes
a fresh watermark. The same single outstanding-page limit applies throughout.
This recovers facts received during the failed interval without retaining them
indefinitely or skipping the original window's unread facts. An outstanding
page still buffers live events until its reply; this is not a timeout or a hard
memory limit for a stalled read.
This is a client loading state, not a Session/assembly gate. No Pending provider
is skipped and no runtime readiness rule changes.

A transcript window can hold a terminal fact without the start it settles.
The current-state reads after a branch's window therefore include each task
owner's settled tasks at the window's cutoff, read before its unfinished ones:

| Method | Owner | Reply |
| --- | --- | --- |
| `session.exec.finished {sessionId, branchId, through?}` | WorkspaceTools | `branchId`, fixed `through`, `tasks`: up to 50 `FinishedExec` items `{start, endedAt, end}`, newest end first. `start` is the item `session.exec.pending` returns; `end` is the `exec/ended` data |
| `session.collaboration.finished {sessionId, branchId, through?}` | CollaborationSession | `branchId`, fixed `through`, `requests`: up to 50 `FinishedDelegation` items `{start, endedAt, result}`, newest end first. `start` is the item `session.collaboration.pending` returns, with its latest delivery; `result` is the `collaboration/result` data, or null when a failed delivery ended it |

Both are nonmutating and unpaged: 50 matches the task list's finished-row
retention, not a complete history. Older results remain in the Timeline and
their original conversation; the inventory does not restore every past task.
They read only
the terminal facts and the starts those facts name. The client folds a reported
task once, by branch and start event, whether or not the window also held it;
a task already finished in the projection is kept as folded. Each is its own method entry in the owner's Session method table and is
gated like the others: a daemon that does not list it is skipped. No new event, ledger or retention state is
added.

The result covers discovered branch histories and later facts. Neither the
attach reply nor finishing main's history establishes that every branch has
arrived. A delayed initial state can reveal existing work after stop-all admission,
just as new work can arrive later; stop-all covers the observed rows captured at
admission. No cross-branch sequence, transaction or execution fence is introduced.

## 3. Running and finished rows

Extend the same task records with their observed start/terminal facts; do not
maintain a second active/finished tracker. A command uses its originating branch
and task ID. A delegation uses its originating branch and the enclosing
`collaboration/request` event's `id`, not the model request or call ID:

| Fact | Fold into that delegation row |
| --- | --- |
| `collaboration/request` | Create a `starting` row keyed by its branch and event `id` |
| `collaboration/delivered` | Match `data.op_event_id` to that request event; `status:"ok"` supplies the actual `agent_id`/`session_id`, while `status:"error"` ends it as failed with `data.error` and no member Session |
| `collaboration/result` | Match `data.request_event_id` to the request event; retain its `text` and typed `status`/`reason` as the terminal result |

A delivered fact for a message, cancel or close operation does not create a
delegation row or settle a different request. Undelivered requests stay
`starting` until the corresponding owner's fact arrives.

Proposed first-release presentation:

- Keep every unfinished row and the **50 most recently finished rows** of the
  current Session. This fixed view limit has no setting, timer or persistent
  retention effect. "Most recently finished" is ordered by the
  enclosing terminal `AgentEvent.create_at` (Unix milliseconds), then branch ID
  and start/request event ID. Use the `exec/ended`,
  `collaboration/result` or failed `collaboration/delivered` event's timestamp;
  the delivery error is itself a Timeline fact. Transport, history and stop RPC
  errors are notices, not finished rows, and get no fallback terminal timestamp.
  The Timeline and original conversation retain their facts/results.
- Rebuild the retained rows on resume from the transcript window and the
  owners' inventory reads. A settled task whose start lies before the window
  comes from `session.exec.finished` or `session.collaboration.finished`
  (§2); a daemon that lists neither restores only the tasks the window holds.
- Order rows by dispatch, and never reorder them when a task finishes. Commands and
  delegations group by the model request that issued them; a request takes
  the time of its earliest retained task, and requests are ordered by that time, then branch ID
  and request ID. Within one request, tasks follow their call's position in the
  model response (`step/tool-call` `calls`) when that response was read or
  the owner supplies its position in inventory,
  then their start/request event's sequence: the requests of one parallel
  batch commit in no fixed order. Workflow runs follow every command and
  delegation, by creation time. The order is the same live, on reopening the
  panel and after resume.
  Timestamps and elapsed time are display data only; they never determine task completion,
  cancellation or storage cleanup.
- Show kind, label, observed state, elapsed time and command exit code when its
  existing result supplies `structuredContent.exitCode`. Missing exit status is
  not zero. The status-row count includes unfinished work only.
- Only `exec/ended`, a delivery error or `collaboration/result` supplies the
  corresponding terminal outcome. A stop reply, read error, idle member or
  vanished live capture cannot fabricate it.

### Task titles and composer rows

A task title identifies this command or delegation, not an Agent's profile
description, its detailed instructions or its member conversation title. The
title is supplied by the owner fact and retained by the pending inventory:
[Collaboration](plugins/Collaboration.md) owns delegation titles, and
[WorkspaceTools](../architecture/rust-agent/plugins/WorkspaceTools.md) owns
command titles. The CLI uses the title when present. For historical tasks without
one, retain the existing command/description or first prompt line as the label;
do not invent or persist a replacement title.

Command details retain the original command independently of the title and show
it before the output in the scrollable body, including silent, finished and
restored commands from another branch. Use the matching tool input when available,
otherwise the owner fact or pending inventory's description.

```text
+-- Wait for build ---------------------------------------------+
| Tail preview - last 2,000 lines / 50 KiB                        |
| sleep 60                                                      |
|                                                               |
| No output yet                                                 |
+----------------------------- x stop command · Esc back --------+
```

Member tasks carry their Agent name independently of the title. A temporary
profile supplies its name immediately; an existing Agent uses its id until
`agent.list` supplies its name. The task list, member card and composer rows
use the same projected task title and Agent identity, including after resume
and when the originating branch is not the displayed line.

Above the composer, each task occupies its own row; never concatenate several
task labels horizontally. Show the task's status mark, title, member Agent name
where applicable, and elapsed time. Truncate the title and then the Agent name
to fit a narrow terminal, preserving the elapsed time when it fits. Running
rows use an activity mark; terminal rows retain their actual outcome mark for
five seconds and then disappear. With no visible task there is no task strip.

Render every visible task as its own row, without a fixed row cap or a
hidden-task count replacing task rows. The existing terminal viewport still
preserves the composer when the rows exceed the available screen height.
The existing empty-composer Enter shortcut opens `/tasks` while the strip
shows, subject to the earlier shell/queue actions in
[CLI run keys](cli-commands.md#run-keys-and-queue).

In fullscreen mode, clicking a task row opens that task's existing detail
directly; Esc returns to the composer with its draft preserved. Command rows
open command output, and member rows use the existing
[member conversation contract](member-conversation.md). A click requires press
and release on the same cell with the same task identity still at that row.
Dragging, resizing or replacing the task at that row cancels the click;
resize cancellation is immediate even while transcript reflow is debounced.
Elapsed-time updates do not cancel a click. Rows clipped out of the viewport
are not clickable, and an open prompt or overlay blocks task-row clicks.
Scrollback mode retains its keyboard-only entry points.

```text
  ◐ Fix esbuild bundling · Builder                         27m57s
  ◐ Review the runtime · Reviewer                          25m27s
  ◐ Extend integration coverage · Test Explorer             13m01s
  ◐ Check generated types · Type Checker                     8m42s
  ◐ Audit cancellation · Safety Reviewer                     3m18s

  ╭────────────────────────────────────────────────────────────╮
  │ › Ask anything...                                          │
  ╰────────────────────────────────────────────────────────────╯

  Fullscreen: task row --click--> task detail --Esc--> kept draft
```

```text
+-- Background tasks --------------------------------------------+
| > cargo build                 command   running    12s         |
|   Survey the crates           agent     starting               |
|   Check imports               agent     completed   8s         |
|   cargo test                  command   failed      4s  101    |
| Enter details  x stop selected  Ctrl+X stop all  Esc close     |
+----------------------------------------------------------------+
```

For an active command, Enter keeps the accepted
[live-output behavior](background-task-output.md). For a finished command, show
its actual final result directly, without reading an old output path or asking
for a live capture. A member with a delivered ID uses the [read-only member view](member-conversation.md);
keep a finished delegation's result clearly labeled separately from the member
conversation, including when that Session can no longer be read. A failed
delivery has only its request error to show. Finished rows have no stop action.
Reuse existing text sanitization, theme, localization, wrapping and scrolling;
the existing minimum-size screen still applies.

Branch-read feedback keeps stop-all disabled until discovery succeeds:

```text
+-- Background tasks --------------------------------------------+
| Loading branch history...  Stop-all unavailable                |
| Branch history failed: <error>  Stop-all unavailable           |
| Reopen to retry failed reads.  Esc close                       |
+----------------------------------------------------------------+
```

## 4. Stop-all requests

At admission of the tasks list's `Ctrl+X` command, the Session worker takes a
finite snapshot from its existing projection for the Session displayed by the
panel: unfinished command targets and successfully delivered member Session IDs.
The command carries its displayed Session and originating projection identity
from §2. In one worker turn, compare both with the current projection and capture
the targets without an intervening await or command dispatch. If either no longer
matches, refuse the action, including a switch away and back to the same Session
ID. Keep the admitted Session and targets fixed across later switches. Preserve
command branch/task identities; coalesce repeated member Session IDs so the same
member is not interrupted twice for two
delegation rows. No other Session is selected.

| Captured target | Existing request |
| --- | --- |
| Command | `session.exec.stop {sessionId: <captured leader>, branchId: <originating branch>, taskId: <task>}` |
| Delivered member | `session.stop {sessionId: <delivered member>, branchId: "main", reason: "stopped by the user"}`; not for a provider member (an `agentId` of the form `<kind>:<rest>`, such as an [external agent](plugins/ExternalAgent.md#6-interfaces-summary)), whose `extSessionId` is not a Session and which has no stop row |

The member request interrupts that member's current main turn independently of
the leader; it does not stop the leader or infer a member branch from the
delegation's originating branch. Arbitrary member branches are outside this scope.

The existing worker owns one finite operation: the captured Session/targets,
next target, outstanding request ID, and attempted/error/unavailable counts.
Use the existing stop methods, one request at a time. Each response or send
failure clears that outstanding slot and advances to the next target; do not
wait for `exec/ended` or `collaboration/result` to advance. Replies continue
through the existing asynchronous client response path so the worker still handles live
events and user controls. Repeated Ctrl+X while this batch is in progress does
not start another batch. Esc may close the panel; it does not cancel accepted
stop calls or add new targets to this snapshot. Connection shutdown uses the
existing request/connection cleanup.

Skip finished rows and starting members with no delivered Session ID, and report
the number whose target is not available yet. Continue after one target fails;
show its error through the existing notice path. Report requests attempted,
errors and unavailable starting targets, not a fabricated count of cancelled
tasks. An already-ended command's successful stop reply is not a cancellation;
`exec_task_unknown`, closed/unavailable and reloading errors are reported once,
then the batch continues without retry. Actual terminal facts update the rows
independently.

This is a group of the existing addressed stops. A command may have already
completed; a member interrupt can affect a later turn; adopted input cannot be
withdrawn. Starting members and work arriving after the snapshot can continue.
If a member's execution profile supplies background commands, those commands
are outside this displayed Session's inventory and stop batch and keep running.
A non-cancelled `exec/ended` wake can start another member turn. A teammate's
Steer `message` can also start another member turn.
The built-in [WorkspaceTools](../../crates/dim-agent-capabilities/src/workspace_tools/exec.rs)
and [ShellSession](../../crates/dim-agent-capabilities/src/shell_session.rs)
keep member commands in the foreground, including `runInBackground` requests.
Interrupting that foreground command follows its existing cancellation path;
it is not a background command left running by stop-all.
Stop-all does not interrupt the leader. Unless Collaboration already suppresses
delivery, each stopped member's `collaboration/result`, including a cancelled
result, becomes the existing Steer `dim-agent-report` input on its originating
leader branch. Existing suppression covers a closing leader branch and a leader
turn stop that covered the request before delivery. An unsuppressed report can
start or steer a leader turn that creates new work during or after the batch;
that work is outside the captured snapshot.
These limits stay explicit in the panel's operation feedback. There is no new
Session close, conditional cancellation, task-creation freeze or automatic retry.
The existing stop-before-delivery ordering limitation in #263 remains recorded
instead of changing Collaboration or core to force a result.

```text
+-- Stop-all requests -------------------------------------------+
| Requests attempted: 3   Errors: 1   Unavailable: 1             |
| <target>: <stop error>                                         |
| Starting members and new work may continue.                    |
| Profile-supplied member background commands keep running.      |
| Teammate Steer messages may wake members.                      |
| A non-cancelled end of such a command may also wake a member.  |
| Unsuppressed reports may wake or steer the leader.             |
| The leader may start new work during or after stop-all.        |
| Rows wait for real terminal facts.  Esc close                  |
+----------------------------------------------------------------+
```

## 5. Implementation and acceptance

Affected owners are the CLI wire decoder, existing Session worker/projection,
tasks panel and localization. Decode the existing public branch/task types once
at the wire boundary; no consumer alias, normalization wrapper or server adapter.
The worker owns finite history/stop requests; the task projection owns derived
rows. No new Capability, shared protocol, configuration or persistence format.

Extend the existing PTY task flows to cover a resumed Session with work on main,
a child and a nested branch, including discovery by state when main's local page
contains no matching create fact and a delayed initial state after a stop batch.
Verify that panel reopen retains the projection, while detach/replacement creates
a fresh one. Cover fixed-watermark paging plus a live event during
initial load; failed creation and unavailable history; deduplication and Session
switch cleanup, including an old page after switching away and back to the same
Session and duplicate discovery notices. Verify the finished-row limit/order
from terminal envelopes, including failed delivery and equal timestamps,
original terminal text and exit status, and that finished command details do not invoke a live read.
Hold one history reply while discovering another branch: its read must wait,
while leader notices and controls continue. Fail a later history page, reopen the
panel and verify one retry with the same validated cursor/watermark; repeated
state notices must not retry it. Replace the projection before a queued retry
is sent and assert that it is discarded.

Exercise Ctrl+X with two command branches, repeated delegation rows for one
member, a starting member, a finished row and one failing stop. Assert exact
targets (member branch `main`, command branch from its row), one outstanding
stop advancing on its reply while terminal facts are withheld, continued leader
notifications, no duplicate batch, preserved actual outcomes and no Session close or input withdrawal.
A Session switch before admission refuses the action; one after admission does
not retarget it. New arrivals are not added to the batch. Include
loading/error/narrow layouts.

With the product local-execution profile (WorkspaceTools), request
`runInBackground` on a member and verify that its branch writes no `exec/started`
while the command runs. Admit a Ctrl+X stop-all batch containing that member and
interrupt its current turn through that batch's addressed member stop: its tool
result reports cancellation, its `turn/end` is cancelled, and the leader receives one cancelled
`collaboration/result`. Then send a later Steer `message` to the member and verify
that it can execute again without reopening the cancelled delegation. A stop
reply does not fabricate a terminal result. If a profile supplies member background
tasks, additionally hold such a command at stop admission, complete it without
cancellation and verify that its actual `exec/ended` wake starts another turn.
Do not bypass the built-in foreground rule or fabricate background facts to
exercise that conditional check. The command stays outside the leader's inventory.
Verify that an unsuppressed stopped member's cancelled `collaboration/result`
reaches its originating leader branch as a Steer report input, which can start or
steer a leader turn and create work outside the batch. Keep that delegation row's real
cancelled result; a later Steer `message` to the member, or a background-command
wake, must neither reopen it nor create another delegation row without a new request.

Reuse a real App Server flow to confirm that initial state watches discover
restored published children and nested branches, local history reconstructs their
command/member targets, and named stops leave the leader usable. These are required implementation checks; this
documentation supplies no passing UI/runtime evidence. Workflow runs and their
controls follow the [Workflow task view](workflow-task-view.md).

## 6. Rule check and ablation

| Rule | Application |
| --- | --- |
| §1 / ADR 002 D1 | Existing client owns derived rows and finite I/O; native owners keep task execution/results |
| §4.1 commands/results | Existing stop interfaces; only owner facts change displayed terminal state |
| §4.2 handover | No adopted input or accepted work is revoked by a view change |
| §4.3 before | No before callback |
| §4.4 core neutrality | No task-list or stop-all special case in core/Rpc |
| §4.5 batches | No Timeline writes, atomic snapshot claim or cross-branch sequence |
| §4.6 context | Display/retention adds no model-context path |
| §4.7 public access | Existing public histories, notices and stop calls only |
| §4.8 infrastructure | No new service, branch-list endpoint, readiness/cancellation protocol or rutis requirement |
| §4.9 necessary facts | Reuse existing facts; no new event, store or process notice |
| ADR 002 D2 | Same task projection and connection; no registry, adapter plugin or event bus |
| ADR 002 D3 | Client depends on public contracts; server does not depend on the UI |
| ADR 002 D4 | No loader field or business configuration; finished-row limit is fixed presentation |
| ADR 002 D5 | Existing JSON contracts; internal typed views are not new public services |

**Requirements raised to mechanism owners:** none. Observation and addressed
stops already exist. The proposed behavior records their ordering limits and
does not extend core to eliminate them.

Ablation: one existing task collection, branch-local cursors and finite pending
history/stop work suffice. Finished rows require their original result facts;
retain them in that same projection. Do not add a server registry, TTL, task
database, global cancellation token, subscription layer or client generation API.
