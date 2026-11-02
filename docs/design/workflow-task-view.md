# Workflows in the CLI task view

Status: **Accepted ([#332](https://github.com/arcships/dim/pull/332)); implemented
in the native CLI**, with the delivery notes in §6. This is the Workflow slice of
[#264](https://github.com/arcships/dim/issues/264). It extends the accepted
[command/member task list](background-task-list.md), implemented in
[#324](https://github.com/arcships/dim/pull/324). The optional
[Workflow package](../../runtime/plugins/workflow/README.md) is delivered by
[#306](https://github.com/arcships/dim/pull/306).
This does not complete #263's other transcript/model-policy work.

## 1. Scope and existing interfaces

The same `/tasks` (`/background`) panel lists associated Workflow runs, opens
stored stage results/logs and stops individual runs or a finite captured batch.
The existing status row includes their observed running/stopping count. This
view neither edits definitions nor starts runs, installs packages or changes
Session configuration.

Use the package's delivered [management contract](plugins/Workflow.md#a3-management-methods):

| Existing interface | Use |
| --- | --- |
| `hello.features` | Discover the actual `workflow.status` and `workflow.stop` methods |
| Existing `workflow` tool results and `dim-workflow kind="report"` inputs | Discover exact run IDs from the displayed Session's existing branch histories and notices |
| `workflow.status {run_id}` | Read one known run, including its association, state, stages and logs |
| `workflow.status {}` | Explicit panel-open discovery of retained runs, including management-started runs addressed to this Session |
| `workflow.stop {run_id}` | Ask Workflow to settle its run; the reply contains the resulting stored run |

The implementation is in the package's [management contribution](../../runtime/plugins/workflow/index.ts)
and [run owner](../../runtime/plugins/workflow/engine.ts). `status` without an ID
returns the retained run array; it is not a definition list, paged history or
server task registry. `workflow.list` lists definitions and is not used here.

Management is local and process-global. Select runs whose tool
`started_by: {kind:"tool", session_id, ...}` or `report_to.session_id` identifies
the displayed Session.
A management-started run with no matching report recipient is outside this view.
Match Session identity, not cwd, workflow name or branch; the same rule includes
runs from another branch of that Session. This filter defines presentation,
not a new authorization boundary or a claim that management methods are scoped.
The native CLI connects as `Entry::Local`, which may call these management
methods. Web and paired entries do not receive `workflow.*`; their unavailable
method behavior is not a new permission or fallback route.

Workflow stores its own process records. Tool results and report markers supply
identities and observed outcomes, not every stage transition. Use those existing
facts for discovery and the current management read for details. Existing Session
subscriptions continue to serve commands and members; no Workflow event,
subscription, Timeline write or host interface is introduced.

## 2. Read lifetime and projection

Reuse the command/member worker's existing all-branch history/live path to find
Workflow run IDs. A successful `workflow` start result supplies `run_id`; status
and stop results can supply the owner's stored run. UI code may use the existing
`structuredContent` for these fields; the model-visible ID remains in `content`.
The existing report input carries `run` and `status` in its `dim-workflow`
marker. Decode these producer-defined fields at the wire boundary, not by
searching free-form result prose. Tool errors, list results and stage-input
markers do not invent a run ID or an associated run.

There is no process-wide discovery read when a Session attaches or while its
panel is closed. When `/tasks` is explicitly opened, queue one `status {}` read
to find retained associated runs, including management-started runs with a
matching report recipient. Reopening explicitly requests another discovery;
there is no recurring full-array request. A management run started while the
panel is closed is not discovered until that read or its report arrives.

Read `status {run_id}` for known associated runs whose observed owner state is
unfinished. After each reply, the existing worker refresh deadline may admit
the next known unfinished ID, at least one second later, rotating across those
IDs. This refresh may continue with the panel closed so the observed count can
settle; an empty or fully terminal set has no scheduled status read. A terminal
report for an observed unfinished run permits one final read for its stored
outcome and end timestamp, not a terminal polling loop. Restoring old terminal
reports does not start one read per historic report. Opening terminal detail
can request one current read; it does not create ongoing refresh.

The count covers observed associated runs and can lag. It is not a complete
closed-panel inventory, a cross-owner snapshot or a real-time guarantee.

Keep at most one Workflow status read outstanding in the existing Session
worker, including across attachment replacements. An old read retains its slot
until reply or connection teardown; a replacement's first queued read waits for that
slot. A slow reply does not start another read. Explicit discovery, known-run
refresh and detail share this slot and projection, without another attachment.
The command/member history reader and ordinary leader controls remain independent.
Use the worker's existing response loop and private attachment identity.

Status reads and stop requests use independent pending requests. Neither waits
for a stop's full settlement before continuing; status may show `stopping`
while the stop is pending. A valid stop reply updates its run. An older
overlapping status request cannot later replace that reply: mark only that
Workflow request stale, keeping its slot until reply or teardown. Do not
invalidate the leader projection. If it was an explicit discovery still needed by the
current panel, fulfill that same discovery once more after its old slot clears.
This adds no public revision, epoch or result fence. Leader input, ordinary
controls and other owners' notices remain independent.

A per-ID call reduces the returned records, not the owner's storage cost:
`Workflow.status` still uses `RunStore.read`, takes the file transaction and
parses the whole stored document. Refresh ceases at a terminal/unavailable run;
it is not described as an indexed or constant-cost read. The native branch
history reader keeps #301's event-driven discovery and no polling timer.

Decode the external reply once at the RPC boundary into the fields needed by
the view. Do not duplicate Workflow's persisted-file validator, definition
validation, authority checks or recovery logic. Reuse the existing timestamp
conversion for the owner's UTC timestamps. Adopt a valid complete reply in one
projection update; a malformed/error reply adopts no partial rows.

The current Session's existing projection owns the derived rows and stage/log
details. Use kind plus `run_id` as the Workflow row identity; do not use name,
stage Session or tool-call identity. Merge associated runs into the same task
collection, preserving command/member keys. Retain every observed unfinished
item and the latest 50 actual terminal outcomes across kinds, ordered by the
owner's end timestamp with the existing stable identity tie-break. Workflow's
own 100-run persistence retention is unchanged.

| Owner data | View meaning |
| --- | --- |
| `name`, `source`, `run_id` | Label, source and exact control/detail identity |
| `created_at`, `ended_at` | Elapsed time and actual terminal ordering; not RPC receipt time |
| `state: running / stopping` | Unfinished work; keep both in the background count |
| `state: completed / failed / stopped / interrupted` | Show the actual terminal state; no invented exit code |
| `stages[]` | Ordered stage IDs, step indexes, states, optional Session IDs and stored results/reasons |
| Stage states | `waiting`, `launching`, `running`, `completed`, `failed`, `stopped`, `skipped`, `lost` |
| `log[]`, `report.state` | Stored diagnostics and actual report settlement, separate from run outcome |

Definition snapshots, parameter values and permission caps are not displayed or
copied into a second client store. A pending report does not reopen a terminal
run. No event is written merely because a run was read or displayed.

A successful complete explicit array read reconciles the retained inventory;
a per-ID read updates only its named run. Both apply the association filter.
An unavailable/pruned run stops its automatic reads without being relabeled
completed. If selected detail is no longer available, show that explicitly and
return to the current list. Use the owner's stored `ended_at` for terminal
ordering; a report's arrival time is not a substitute.

## 3. Loading, errors and attachment changes

If `workflow.status` is absent, the panel continues its existing command/member
behavior without fake Workflow rows. A read-only integration may show runs when
status exists but stop does not; the action is unavailable in that case.

When status is advertised but still loading or failed, include Workflow in the
panel's existing incomplete-inventory guard for Ctrl+X. Known single-item stops
remain available. An absent optional method does not block command/member
stop-all. The next connection reevaluates its advertised methods; there is no
new hot-registration contract. Features are captured by `hello` for the
connection: a Workflow reload during hello can hide these methods until the
next connection, even after its provider is active again.

Before the first reply, show Workflow loading separately from branch-history
loading. A successful empty associated set is empty, not an error. On an RPC,
service-reloading or decode failure, retain the last validated rows as **last
observed** and show the error; do not present their count as a fresh total or
clear them to zero. Stop the affected automatic reads after failure. Reopening
`/tasks` retries explicit discovery once and successful known unfinished reads
can resume. If no snapshot ever
succeeded, show unavailable rather than an empty Workflow inventory. Existing
transport response limits still apply. Workflow bounds the records it admits and
the store it reads ([Workflow A.18](plugins/Workflow.md)), so an explicit array
and a per-ID record stay within one client frame for runs admitted under those
bounds; a record committed before them keeps the transport limit stated here. A
frame-limit failure follows the existing client path: it disables submission and
preserves the current UI draft, not a Workflow-local read error. Do not claim
recovery by silently increasing the limit, truncating records in the client,
reading daemon files or inventing a summary endpoint.

Session replacement, disconnect or worker shutdown invalidates this reader with
the existing attachment identity and cancels its next scheduled read. An admitted
old read settles through the existing response loop and cannot update the new
projection, even after returning to the same Session ID. A new attachment
discovers IDs through its own existing history/live path; it does not start a
global status request. Closing only the panel stops neither known unfinished
refresh nor an admitted run/control.

Errors, leader approvals/questions and other overlays use the existing modal
priority. Leader notifications, draft, selected model, permission display and
conversation position continue to belong to the leader. A view neither submits
input nor applies permission/model configuration to any stage Session.

## 4. Layout and interaction

```text
+-- Background tasks ---------------------------------------------+
| > nightly-check       workflow   Running      02:13              |
|   cargo build         command    Running      00:18              |
|   inspect imports     agent      Completed    00:42              |
| Workflow: last observed; refresh failed                          |
| Enter details  x stop selected  Ctrl+X stop all  Esc close        |
+-----------------------------------------------------------------+

+-- nightly-check - workflow -------------------------------------+
| Run: <run_id>              Source: user             Running      |
| Step  Stage        State         Session                         |
| 1     analyze      completed     <session_id>                    |
| 2     tests        running       <session_id>                    |
| 2     lint         waiting       -                               |
| analyze result: <stored result text>                             |
| Log: <stored log entries>                                        |
| Report: none                                                    |
| x stop workflow  Up/Down/PgUp/PgDn scroll  Esc back               |
+-----------------------------------------------------------------+
```

Enter on a Workflow opens its read-only stage/result/log detail, including ended
runs. Stage Session IDs remain labels, not editable Session navigation. The
existing scrolling, wrapping, terminal sanitization, theme and localization
apply. Follow new snapshots without resetting the selected run or scroll on
unrelated updates. Do not auto-scroll someone reading older content.

Use the existing panel width and minimum-screen behavior. At 48 columns, put
kind/state and identity on wrapped lines rather than losing the stop target;
body content scrolls above persistent key hints. Loading, empty and error text
must remain legible. Externally supplied names, results and logs cannot inject
terminal control sequences.

| Key | List | Workflow detail |
| --- | --- | --- |
| Enter | Open selected detail | No submission or stage navigation |
| x | Stop selected `running` run when the list filter is empty | Stop this `running` run |
| Ctrl+X | Existing finite stop-all action | Ignored, as in existing detail views; Esc returns to list |
| Esc | Close panel, preserving leader draft | Return to list; no stop or leader interrupt |
| Navigation keys | Existing list selection/filter behavior | Scroll stored stages/results/logs |

## 5. Stops and settlement

Single-run stop uses its exact `run_id` with `workflow.stop`. Workflow owns stage
withdrawal, interruption, joining and final state. Never replace that call with
stops of the listed stage Sessions: those would bypass the run owner. A terminal
row has no stop action. An unavailable method or reply is an explicit error,
not permission to fall back to another control. `stopping` is unfinished work,
but has no repeat stop action. An admitted stop also disables repeat `x` for
that run while its reply is pending, without inventing an owner state.

Extend the existing finite stop-all batch with the known `running` Workflow
run IDs when it is admitted. Keep commands addressed by branch/task and members
by delivered Session, with existing deduplication. Each Workflow run is included
once. Send captured commands and Session-member stops first, then Workflow
stops, preserving serial settlement within that finite batch. Already-stopping
Workflows are not stopped again. New runs/stages are not
added to an admitted batch. Starting stages do not require a Session identity
because the run owner is already addressable.

The delivered `Workflow.stop` persists `stopping` and awaits its owned drive
task before replying. Stage withdrawal, interruption and joining have no fixed
completion bound. A pending Workflow stop can therefore delay later captured
Workflow targets indefinitely; it must not sit ahead of command/member targets
or gate status reads, leader input or notices. Do not report a completed batch
until its admitted requests actually settle.

A Session switch before admission refuses stale controls. After admission, the
batch keeps its original targets; it never retargets a run to the replacement
Session. Panel closure does not cancel accepted mutations. Track reply/error
settlement through the existing worker so a pending Workflow stop does not block
leader input, notices or independent reads. Do not retry an uncertain mutation
on disconnect; the next valid status read resolves what the owner stored.

Only the owner's returned/polled state determines the row outcome. A stop racing
completion may return a completed stage within a stopped run. A failed or missing
reply does not fabricate stopped. Preserve the package's accepted later-turn
stop and retained-input limits, and the task panel's existing statement that a
finite batch is not a fence against later reports, input or new work.

## 6. Implementation acceptance

Reuse the existing CLI PTY flows and the real optional-package App Server flow;
no new test harness, backend hook or mock call-order suite is needed.

- With the actual package installed, tool-started and management-started runs
  exercise both association fields. Include an unrelated run with the same cwd,
  a matching non-main branch and an unassociated management run. Restore the
  client, discover known IDs from existing history, then explicitly open the
  panel to obtain the retained associated inventory, including management runs.
- An attachment with no known Workflow sends no Workflow status request. A
  known unfinished ID uses per-ID reads; a terminal result/report ends its
  recurring reads after any final settlement read. Duplicated historic terminal reports do not start a query
  per report. With the panel closed, there is no full-array discovery. Verify
  tool-structured IDs and escaped report-marker IDs without parsing result prose.
- Show sequential/parallel stage state and actual terminal results/logs. Verify
  elapsed timestamps, combined terminal retention/order, empty/loading/error,
  unavailable methods, refresh failure/reopen, sanitization and 48-column layout.
- Hold one status reply while the leader accepts input and notifications, then
  switch away and back to the same Session. The old reply cannot populate or
  retarget the new view; there is no second pending read from detail or a timer.
- Single stop and a mixed finite stop-all use the real run ID and existing other
  targets. Include completion during stop, a failed mutation, a run appearing
  after capture and a Session switch before/after admission. Confirm actual
  Workflow settlement; no direct stage stop is used by the CLI. Hold a Workflow
  stop's join and verify command/member targets have already received their
  stops, leader input and status reads continue, and later Workflow targets stay
  queued. A late status read cannot overwrite the settled stop result. A
  `stopping` row or pending stop cannot issue a second `x` request. Loading/error
  blocks all-task capture without disabling known single targets.
- Include `launching`, `skipped` and `lost` stages, a provider unavailable during
  hello and per-ID not-found/pruning. A reply from a record committed before
  Workflow's bounds can still be oversized; that frame-limit failure follows the
  existing disconnected-UI/draft behavior, no count is replaced by zero and no
  local-read recovery is claimed.
- Preserve the leader draft/model/permission and approval priority. Exit/reload
  releases only owned client resources; it neither cancels unrelated runs nor
  changes optional package installation, persisted format or native ownership.

### Delivery

The CLI implements this contract in its existing Session worker
(`session_host/workflow.rs`), task projection and panel. Public PTY flows in
`crates/dimcode/tests/cli_flow/tasks.rs` drive a mock App Server that advertises
`workflow.status` and `workflow.stop`:

- A restored terminal report starts no read; a live tool result's structured
  `run_id` is read alone, with no full array while the panel is closed. Opening
  the panel discovers tool-started and report-addressed management runs and
  omits another Session's and an unaddressed management run. Detail shows
  stages, stored results, log and report; `x` sends one `workflow.stop`, a
  repeated `x` while it is pending sends nothing, and the terminal run is not
  refreshed again.
- A failed discovery shows the error, blocks Ctrl+X and stops automatic reads
  until reopening. Stop-all sends the command stop before the Workflow stop; the
  held Workflow stop gates neither leader input nor a status read, and that read,
  answered after the stop, cannot replace the stop's stored result.

Not covered by these flows: the real optional-package App Server flow, the
48-column layout, `launching`/`lost` stages, per-ID not-found/pruning and a
record committed before Workflow's frame bounds. Opening terminal detail does
not request an extra read; detail shows the last stored record. These remain
open #264 verification items.

## 7. Impact, rule check and ablation

Implementation is confined to the CLI's existing Session worker, projection/view
model, task panel, localization, related README and public flow tests. The
Workflow package, App Server, Rpc, core, rutis, storage formats and assembly gain
no interface or behavior. No new Capability or wiring plugin is needed.

### Requirements raised to mechanism owners

None proposed. Existing process management reads and the run owner's stop
operation supply this flow, with their whole-store parse cost and unbounded stop
join. Workflow's own payload bounds ([A.18](plugins/Workflow.md)) keep the
records it admits within one client frame; a record committed before them keeps
the stated transport limit. Session facts supply discovery; conditional per-ID
reads supply current stored detail.

| Rule | Application |
| --- | --- |
| §4.1 commands/results | Existing typed owner method performs the stop and supplies its result; the client writes no result fact |
| §4.2 handover | Accepted run/input work is not revoked by closing the view or changing attachment |
| §4.3 before | No callback or Timeline write |
| §4.4 core neutrality | No Workflow case in Rpc, Transport, Timeline or Dispatcher |
| §4.5 batches | No cross-owner atomic snapshot or Timeline batch is claimed |
| §4.6 context | The UI never injects read data into model context |
| §4.7 public access | Existing tool/report facts, advertised management methods and returned owner records |
| §4.8 infrastructure | No mechanism requirement; existing client I/O and attachment identity suffice |
| §4.9 necessary facts | Derived display only; no event, index, persistence or subscription |
| §1 / ADR 002 D1 | Existing CLI owns the view; Workflow retains process-run state and work |
| ADR 002 D2 | No management adapter, registry, second connection or polling framework |
| ADR 002 D3 | Optional method discovery; absence affects only Workflow display/control |
| ADR 002 D4 | No setting or loader row; installation stays explicit |
| ADR 002 D5 | Existing named methods/JSON/errors/cancellation; no Rust-only public contract |

Ablation: one existing task collection and one worker-owned read slot cover
discovery, known-run refresh and detail. The next-read deadline exists only for
known unfinished runs; terminal/empty sets need no read loop. Full discovery is
an explicit panel action. The existing finite stop batch keeps command/member
targets before potentially unbounded Workflow joins. No run registry, detail
attachment, second persisted store, public event, lock or extra server method
serves this view.
