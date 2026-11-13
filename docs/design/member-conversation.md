# Read-only member conversations in the CLI

Base detail contract: accepted in [#300](https://github.com/arcships/dim/pull/300),
implemented in [#324](https://github.com/arcships/dim/pull/324).
Sections 1–6 describe that single selected Session-member view. Their base-only
observation-count and selection-lifetime constraints are identified below;
§7 replaces that lifetime.

Sections 7–9 define the transcript-card slice of
[#263](https://github.com/arcships/dim/issues/263), accepted in
[#333](https://github.com/arcships/dim/pull/333) and implemented in the CLI;
§8 records what the flows verify. Model-policy configuration is separate
business scope in #263.

## 1. Existing mechanisms

For a Session member, `collaboration/delivered` binds a delegation's request
event to its actual Agent and Session. The task panel retains that identity.
The following methods apply only to this kind of member.

The provider-member boundary proposed in [ExternalAgent #254](https://github.com/arcships/dim/pull/254)
distinguishes a delivered `agent_id` containing `:` (`<kind>:<rest>`) from the
native Agents' IDs, which never contain `:`. For a provider member, the field
named `session_id` carries an opaque member ID, not a dim Session. Never pass
it to `session.attach`, `session.history`, `session.stop` or `session.detach`,
even if its text resembles a Session ID. This compatibility boundary is part
of the §7 extension; #254's provider implementation/notice availability
is not delivered by this document.

| Existing interface | Use |
| --- | --- |
| `session.attach {sessionId}` | Observe the selected member while keeping the leader attached |
| `session.history {sessionId,branchId,after,limit,through?}` | Read the member's main conversation through a fixed watermark |
| `session.event`, `session.model.progress`, `session.state`, `session.closed` | Update the member view through the existing Session subscription |
| `session.detach {sessionId}` | Release this connection's member observation |
| `session.stop {sessionId,branchId,reason}` | The existing explicit member interruption control |

The native [connection subscription](../../crates/dim-app-server/src/connection/subscription.rs)
stores attachments by Session ID; attaching a member does not replace the leader's
attachment. [History](../../crates/dim-app-server/src/connection/tree.rs) reads the
named Session and branch through the existing authorization and dispatch boundary.
Attaching can restore an unloaded member through the existing acquire path and
keeps it pinned while attached. This does not invoke the CLI's editable resume
path or apply `--permission` to that member.
No new server method, notice, Permission preset or core/rutis capability is needed.

The ordinary CLI resume path is unsuitable for this view:
`Worker::enter` can apply `--permission` to a resumed Session and makes it the
editable conversation. Viewing a member must not select that path, change its
policy or expose a composer for it.

## 2. Entry and layout

In `/tasks`, Enter opens output for a command or a conversation for a delivered
Session member. With an empty list filter, `x` invokes the existing stop control
for either kind; in a nonempty filter it remains ordinary filter text. The detail has no
filter, so its `x` control remains available. A starting member
has no Session ID: Enter or `x` explains that its target is not available yet and
sends no request. Do not derive an ID from an agent name or prompt.
Provider members have no Session conversation/stop action; §7 limits their
cards to leader facts and any leader-side activity the provider publishes.

```text
+-- Background tasks --------------------------------------------+
| > Survey the crates                                agent       |
| cargo build                                      command       |
| Enter details   x stop (empty filter)   Esc close              |
+----------------------------------------------------------------+

+-- Survey the crates - read-only -------------------------------+
| Member Session: <delivered Session ID>                         |
| user       Survey the crates                                   |
| assistant  I am checking the dependency boundaries.            |
| tool       read ...                                            |
| ...                                                            |
| x interrupt member   Esc back                                  |
+----------------------------------------------------------------+
```

The detail is a modal read-only view using the existing conversation row
renderers, wrapping, scrolling, theme and localization. It opens at the tail;
Up/Down, PageUp/PageDown and Home/End navigate the conversation. The existing
minimum-size screen applies. Sanitize externally supplied text before styling;
keep the identity, state and key hints within the overlay's width.

Enter and typed text in the detail never submit input. Esc is consumed by the
detail and returns to the task list; it must not fall through to the leader's
interrupt shortcut. The leader remains the active conversation, with its draft,
selected model, permission display and transcript position preserved. Its
notifications continue to be processed. In the base view, leader approval/question
prompts take precedence and close the selected observation through the same
cleanup path. Under §7, closing detail releases its selection while unfinished
cards retain the observations they still need.
The detail does not expose member approval, question or configuration actions. Member tool
approvals follow [Collaboration's leader route](subagent-tool-approval.md); any user review uses
the [leader approval prompt](cli-commands.md#subagent-tool-approval), not controls in this detail.

The displayed history belongs to the delivered Session's `main` branch, not to a
synthetic transcript assembled from the leader's tool result. A reused member can
contain earlier or later turns; its Session ID stays visible. The leader's real
collaboration result determines the delegation outcome. A member being idle does
not establish completion of a particular delegation.

## 3. Read and observation lifetime

1. Keep the leader attached. Attach the selected delivered member on the same
   connection, then read its main history from `after:0`, using the existing page
   limit and cursor validation. The first page fixes `through`; every following
   page uses that watermark and the returned `nextAfter` until `done`.
2. Route the selected member's main-branch events, state and progress by Session
   and branch ID into a temporary instance of the existing conversation projection.
   Route `session.closed` by Session ID before handling a leader close; a member
   close marks only its detail closed and must not end the leader worker.
   Other branches do not supply this detail's transcript or running state. Fold the
   initial history before draining the member notices accumulated during it,
   discarding already folded sequences. Reuse existing progress offsets; progress
   is display data and does not create a history fact.
3. Send history requests without blocking the Session worker's handling of leader
   notifications and controls. At most one member-history page is outstanding;
   no periodic history polling or second connection is needed. **Base only:**
   keep one selected member observation. The §7 lifetime instead shares
   one observation per needed Session across cards and the one selected detail.
4. **Base only:** leaving the detail stops further page requests and releases only
   its member attachment with `session.detach`. An admitted attach/page request settles;
   if the view closed while attach was pending, detach after that attach settles.
   Do not let an obsolete reply reopen or replace the current detail. Process a
   subsequent selection after the previous observation's cleanup; connection
   shutdown retains the server's existing attachment cleanup.

A failed member history read releases its buffered notices and retains only the
validated conversation prefix. A rejected detach is a member-local notice; it
does not close the leader's connection or claim that the server released the
attachment. The local observation ends, and connection teardown retains the
server's attachment cleanup. No detach retry is added.

This temporary projection is client view state. It owns no task, member, durable
result or agent configuration. Detach does not stop or close either Session and
does not withdraw input. Removing a completed delegation from the running list
does not close an already-open detail; it remains bound to the same member until
the user leaves or another prompt replaces it.

## 4. Visible states and controls

| Condition | Display and behavior |
| --- | --- |
| Attach/history pending | `Loading conversation...`; Esc remains available |
| Successful complete history with no rows | `No conversation yet`; continue observing |
| Live member events/progress | Update the existing read-only rows, without changing leader state |
| Attach or history error | Show the error and any rows already read; stop loading/observing and detach the owned observation; reopening makes a fresh attempt |
| Member `session.closed` | Retain the rendered conversation with `Session closed`; no invented delegation result or automatic reopening of that closed Session |
| Explicit `x` | Existing `session.stop(member, main)` with the user-facing reason; an error uses the existing notice path |
| Escape or leader prompt replaces base detail | Detach only that base observation and return to the leader's existing UI; §7 defines shared-card references |

A stop reply does not fabricate a collaboration result or remove another member.
Stopping still targets the member's current turn: it cannot retract adopted input
and may race a later turn. This view adds no delegation-specific cancellation or
delivery fence. A member detach RPC error is a local notice, without a
Session-close request or automatic re-attach loop. Transport failure retains
the existing connection failure path.

## 5. Implementation and verification

The affected CLI surfaces are the existing task panel/key hints, Session worker,
conversation projection/rendering and localization. Keep the leader projection
and the selected read-only projection distinct; reuse their canonical event/row
types. The server and Collaboration retain their current contracts.

Extend the existing CLI PTY flows to verify:

- The delivered ID selects member attach/history/stop, while the leader stays
  attached and neither Session receives input or configuration writes from the
  detail. Include `--permission` to prove viewing does not apply it to the member.
- History paging keeps its watermark and handles an event arriving during the
  read exactly once. Live text/state belongs to the selected member; simultaneous
  leader updates remain available after Esc.
- Starting and failed delivery, loading/empty/error/closed states, close during
  attach/history, an obsolete reply, a real delegation result while viewing,
  reopening, narrow wrapping and terminal-control text.
- Esc causes neither stop nor Session close, restores the leader draft and allows
  continued leader input. `x` keeps the existing current-turn semantics.

Reuse a real App Server Collaboration flow for member history plus two simultaneous
attachments and member-only detach; verify the leader still accepts input.

### Verified boundaries

The CLI PTY flows cover the delivered identity, fixed-watermark paging with
buffered live events and duplicate sequences, main-branch routing, ignored
detail input, retained leader draft, `--permission` without member policy writes,
explicit interruption, real delegation-result display and member-only cleanup.
They also cover closing during attach or a page, obsolete replies before another
selection, refusal and failed/malformed pages, empty/closed states, fresh reopen,
narrow scrolling and terminal-control text. A leader approval replaces the detail
and cleans up its admitted page; member approval controls are not exposed.
Member approval cancellation cannot remove a leader request with the same request
ID; both approval notices are scoped to their Session before changing the prompt.

The real App Server flow
`collaboration::member_work_absorbs_messages_and_spans_turns` verifies both
attachments on one connection, the member's native request history, member-only
detach while its work continues, and the leader's subsequent subscribed input
event. Native Collaboration delivers its request as a contextual raw block;
the flow checks that public representation rather than assuming an ordinary user
message. The CLI keeps the existing conversation projection's rendering rules.

The implementation remains part of #324's acceptance work. Its task-inventory
attachment replacement and real restored-branch/background-wake/report checks
remain separate requirements; these observations do not establish them.

## 6. Rule check and ablation

| Rule | Application |
| --- | --- |
| §1 / ADR 002 D1 | Existing client process owns transient view state; native Session/branch owners keep their work |
| §4.1 commands/results | Read and observe through public methods; explicit stop keeps the existing owner path and results |
| §4.2 handover | No adopted input or accepted work is revoked by leaving a view |
| §4.3 before | No before callback |
| §4.4 core neutrality | No server or core behavior changes |
| §4.5 batches | No new Timeline writes or batch rule |
| §4.6 context | Client display adds no model-context path |
| §4.7 public access | Public client methods/notices only; no plugin internals or daemon file reads |
| §4.8 infrastructure | Existing attachment, paging, request settlement and connection cleanup; no extension |
| §4.9 necessary facts | Existing history and live progress suffice; no new persistent event |
| ADR 002 D2 | No adapter plugin, event bus, task registry or generic observation framework |
| ADR 002 D3 | Client depends on public server contracts; server never depends on the UI |
| ADR 002 D4 | No loader or business configuration is introduced |
| ADR 002 D5 | Existing JSON RPC/notice contracts; no language-specific service contract |

**Requirements raised to mechanism owners:** none. Existing Session observation
and history cover this requirement. Limitations of turn-scoped interruption remain
recorded in #263 instead of extending a core or cancellation protocol.

Ablation for the accepted base: retain one selected temporary view projection
because replacing the leader's editable projection would lose its view state
or risk writing to the member.
Reuse the existing connection, attachment table, pagination and row rendering;
no extra output stream, polling timer, persistent copy, capability or lock.

## 7. Transcript member cards

The leader's `agent` request block shows member identity/name, the requested task,
observed member model/activity where exposed, delegation state, elapsed time and the
real result summary. `agent` list/inspect/read/message operations keep their
ordinary tool presentation; a tool-call success is not a delegation completion.
This extension neither changes delegation execution nor adds a model policy.

### Sources and identity

Reuse the branch-local delegation identity and task records from the
[task list](background-task-list.md). Pair a request with its originating tool
call by leader branch plus `request_id` and `call_id`; identify the delegation
itself by its request event ID. Reused tool call IDs, request batches and member
Sessions must not merge separate delegations: a call whose ID an earlier batch
already used gets its own transcript row identity.

| Existing source | Card data |
| --- | --- |
| `collaboration/request` | Task prompt, request timestamp, target Agent/profile and originating call |
| `collaboration/delivered` | Actual Agent and member identity: a Session target or a provider's opaque ID; or the real delivery error |
| `collaboration/result` | Completed/failed/cancelled outcome, text and terminal timestamp |
| `agent.list {}` | Native Agent IDs/names; use the delivered Agent identity, not a guessed alias |
| Session-member `session.history` and `session.event` | Existing decoded `context/configured` supplies the adopted member model; conversation facts supply activity |
| Session-member `session.model.progress`, `session.state`, `session.closed` | Live observed activity or explicit observation unavailability |
| Provider `externalAgent.activity` on the leader, when #254 supplies it | Received member activity only; no provider history replay or Session read |

Temporary profile names already exist on the leader's request. For an existing
native Agent, read the public Agent list after identities are known and share that
result across cards; it is read again only when a card names a native Agent the
last read did not cover. A missing entry/read error shows the Agent ID rather than
an invented name; the delivered Session ID remains available in detail. Do not
read Agent records or private Collaboration state. A provider card uses its
leader-side target/delivered Agent identity; `agent.list` is not assumed to
expose that provider's configuration or model.

Apply §1's member-kind boundary before building any observation or control
target, including `/tasks` and its stop-all capture. A provider member's opaque
ID is not a delivered Session stop target. Its card has no conversation-open or
direct-stop action; existing Session stops exclude it and report that this
member kind has no such user control. Do not guess its kind from `session_id`,
probe it with a Session call, or add a client route to the provider's service.

Provider cards retain the leader's request/delivered/result facts. If
`externalAgent.activity` arrives on that leader connection, route it by the
notice's `agentId` and `extSessionId` as member activity. The proposed notice has
no delegation request ID and is not replayed; it cannot reconstruct a task's
historical activity or be attributed to a particular reused-member request.
Show absence as activity not observed on this attachment. No dim model is
exposed by that notice, so do not display the leader's model as the provider's.

`session.summaries` is not the current-model source: its `model` comes from the
creation-time Session configuration, including in stored records. Requested
`profile.model` is a requested target, not a replacement for the member's
adopted configuration. For Session members, decode the existing configuration
facts once at the wire boundary and show the latest observed provider/model.
Before any such fact is observed, show `No model observed`. Later read failures
retain the actual model with a last-observed/error label; never copy the leader's
model or infer one from a model name. Model updates remain member observations,
not writes or configuration adoption by the CLI.

The model/activity is explicitly **member activity**, not a historical claim
about which model executed an old delegation. A reused member can run later
turns or receive Steer messages. Each delegation's outcome and elapsed end still
come only from its own leader-side facts. Member idle/close, a stop reply or a
later turn cannot complete or reopen that delegation.

### Observation lifetime

The base-only selected-observation constraint in §3/§6 is replaced by this
lifetime. The worker owns one observation per distinct
delivered Session that has an unfinished leader delegation or is selected for detail.
Multiple delegations and the selected detail share that member observation;
they do not issue competing attaches/detaches. There is still one selected
read-only detail and one editable leader. Observations are keyed by Session:
a new observation of the same member starts only after the released one has
settled, while another member's observation does not wait for it.

Each automatic Session attachment takes the existing [usage lease](appserver/idle-unload.md#4-what-holds-a-session) and can
restore an unloaded member. Live unfinished cards therefore keep those member
Sessions loaded even when detail is closed. If a terminal collaboration fact
never arrives, the lease can remain for the leader attachment's lifetime;
restoring that leader discovers the unfinished fact and can restore/pin the
member again. This is a cost of automatic live cards, not passive observation
or a new unload exemption. Switching/detaching the leader releases the owned
observations; stopping a member or closing only detail does not by itself
release an unfinished card's reference. Provider cards take no member lease.

Reuse the existing attach/main-history/live-notice/detach flow, decoder,
conversation projection and progress offsets. Serialize member history pages
through one worker-owned member-page slot and fair pending queue, including
across leader attachment replacement. Attach/response handling is asynchronous;
a waiting member cannot block leader input, approval handling or other notices.
The task-branch history reader retains its separate existing scope.

History is folded through its fixed watermark before buffered live facts,
with the existing sequence and progress deduplication. Route by actual member
Session and main branch before touching a card/detail. Leader approval/request
and cancellation handling remains filtered by the leader Session. The CLI
neither answers member prompts nor registers another editable UI for a member.

Once the leader records a delegation's terminal result, release that member
observation if no unfinished delegation or selected detail still needs it.
Keep each ended card's last observed metadata with the existing transcript
projection; another delegation's later member activity does not overwrite that ended card's
cached observation. Explicitly opening its detail can supply a new member
observation, labeled as member state rather than the old task's execution model.
Do not persist another catalog. Restoring unfinished delegations rediscovers
their actual delivered Session and reads it again. Ended cards are not attached
just to fill their model label: after a fresh CLI restore they show `No model
observed` unless a selected detail supplies an actual observation. Opening an
ended Session member's card reuses the base finite history view; an unavailable
old Session is an explicit local error, not a missing delegation result.

Closing only detail releases its selection, not an observation still needed by
an unfinished card. Ending the last reference settles admitted attach/page
requests and detaches only the worker-owned observation. Leader replacement or
connection teardown invalidates all owned member observations with the existing
attachment identity before old replies can change another view. This applies
when returning to the same leader or selecting the same member again.

A failed initial read retains validated data, marks activity/model unavailable
or last observed, and is retried by explicitly opening/reopening the member view,
as in the base contract. An unfinished-card reference does not clear that error
or immediately reattach in a loop. Do not add polling, retry loops, a second
connection, server reference counts, a subscription service or a cancellation
protocol. Session closure or an unsettled request does not produce a fabricated
terminal delegation result.

### Layout and entry points

Card headings show the delegation's task title followed by its Agent name,
using the same title and identity as the
[background-task presentation](background-task-list.md#task-titles-and-composer-rows).
The prompt remains the expanded task detail, not the heading. Historical
requests without a title retain their first prompt line as the label.

```text
+-- Check dependencies · Agent: inspect imports -----------------+
| Task: Check crate dependencies                                 |
| Member model: provider / model     Running          00:42       |
| Activity: reading Cargo.toml                                   |
| [Open member conversation]                                     |
+---------------------------------------------------------------+

+-- Review coverage · Agent: inspect tests ----------------------+
| Task: Review the integration coverage                          |
| Member model: No model observed                 Completed 01:12 |
| Result: Found two missing restart cases ...                    |
| [Open member conversation]                                     |
+---------------------------------------------------------------+

+-- Check imports · Agent: default ------------------------------+
| Task: Check the imports                                        |
| Not started · 2s                                               |
| Reason: agent_not_found: no visible Agent has this agentId;    |
|         call list and use an exact agents[].agentId, or give   |
|         a profile to create a temporary Agent                  |
+---------------------------------------------------------------+

+-- Inspect repository · Provider member: external-agent:… -------+
| Task: Inspect the repository                                   |
| State: Running                                       00:42     |
| Activity: not observed on this attachment                      |
| Controlled by the leader                                      |
+---------------------------------------------------------------+
```

A card is still part of its original leader transcript, not a synthetic model
message. A delivery failure shows its real error and no actionable member
Session. A request the server refused before a member Session existed is a
rejected attempt, not an Agent the leader worked with: its card shows the
target the model asked for, `Not started` in place of a member state, the
refusal's own text as its reason and no model line, since there was no member
to observe. The group's header counts started delegations and rejected
attempts apart — `Worked with 2 agents · 1 rejected`, or `3 delegations
rejected` when none started. A member that started and then failed keeps
`Failed`, its result and its model line, including `No model observed` when
none was observed. The initial request shows Starting until actual delivery;
delivered unfinished work shows Running, independently of whether the member
is between turns. `collaboration/result` supplies Completed, Failed or
Cancelled. Use the request/result timestamps and existing UI tick; no
stop/idle heuristic creates an end timestamp. Stored result detail remains
available without observing a live member.

Use the existing canonical transcript renderer below the App layer. Collapsed
cards show the task title and Agent name, model/state and elapsed time. Expanded
cards show up to three wrapped task lines, the latest member activity line and
up to six wrapped result lines; indicate omitted text and keep the full real
result in detail. Do not copy the App renderer into the panel or introduce a
dependency from a lower renderer on the TUI application. Names, activity, model
labels and result text are sanitized before styling, wrapped at narrow widths
and kept out of the leader draft. Model/activity loading/error is distinct from
delegation failure.
An ended card can show a last-observed Session model only when this client
actually observed one. A provider card has no dim model/conversation button;
its result summary is the leader's actual collaboration result. Later member
activity does not turn an ended card's metadata into a claim about that task.

In fullscreen mode, add Session-member cards to the existing transcript keyboard
focus order alongside user messages; a provider card or a failed delivery has
no action to stop on. Tab/Up selects older items, Shift+Tab/Down newer;
Esc returns to the composer without modifying its draft. Enter on a focused
Session-member card opens the base read-only detail; a provider card has no such
action. Editing a selected user message with `e` keeps its current behavior,
and `q` appends it to the draft as a Markdown quote; a member card cannot be
copied into the composer by either action. `q` does nothing in a shell draft
(`!`, `!!`), where the quote would run as part of the command, and says so.

The Web workspace offers the same quote as an action on a user message, shown
while its composer is. Both clients write one format, pinned by the cases in
`web/src/features/chat/quote-cases.json`, and submit the draft as it stands:

- CRLF becomes LF and one trailing newline is dropped; a message that is then
  empty leaves the draft unchanged.
- Each line becomes `> line`, an empty line `>`, each followed by a newline.
- The block follows the draft after a separator that makes one blank line: none
  for an empty draft or one ending in two newlines, one newline for a draft
  ending in one, two otherwise. A newline follows the block, so typing starts
  on a fresh line after a blank one. Ordinary text
ends focus and returns to existing
composer input rather than controlling a member.

A fullscreen click on a Session card's conversation action opens the same detail,
using its stable delegation identity. Drag selection/copy and existing tool
group header expansion retain their behavior; a changing row height cannot
retarget an action to another member. Inline scrollback renders the same card
data as of the frame that commits it and uses the existing `/tasks` keyboard
path to reach member detail; it gains no mouse or transcript-focus mode.
Fullscreen redraws a committed card from the snapshot until its delegation
ends, including its running time. Provider cards expose no Session
open/stop through either path. Historical scrollback rewrites
keep the existing terminal rewrite limits.

Both paths preserve the leader draft, viewport anchor and current configuration.
Esc from detail returns to the originating list/card focus without falling
through to leader interrupt. Session-member `x` interruption remains in the existing
detail/list; stop-all remains in the task list, not a new global card shortcut.
Leader approvals/questions close detail and release its selection through the
shared cleanup path. Read-only observations still needed by unfinished cards
remain owned; they never expose member approval controls.
When a Session card no longer has a delivered Session, navigation reports that condition
instead of opening an editable resume or another member.

### Model-policy boundary

The existing `agent` request accepts a temporary `profile.model`, and named
Agents carry their own profile model. Collaboration resolves that profile
through its existing model selection host. These do not constitute an exposed
user setting for inherit/fixed/agent-choice behavior, and `agent.list` returns
Agent summaries rather than a policy configuration.

This presentation displays what the member actually adopted. User configuration
is covered separately by the [subagent model policy](subagent-model-policy.md),
owned by Collaboration: inherit/fixed applies to every newly created native member,
including named Agents, while existing members retain their configuration.
Provider's client preferences do not supply this policy; no core, Prompt,
Provider or rutis capability is requested.

## 8. Transcript extension acceptance

Extend the existing PTY and actual two-attachment member flows. Cover two active
Session members, a reused member and a live model change, with distinct leader and member
models. Actual member `context/configured` must determine the displayed model;
the creation model and leader's configuration must not overwrite it.

- Pair repeated tool call IDs in different request batches and distinct requests
  sharing a member without merging their outcomes. Non-request `agent` actions
  retain ordinary tool output. Real result and elapsed time update the correct
  card even while detail is closed.
- Verify one attach per needed member, shared detail ownership, one member-page
  slot, history/live ordering, fair reads, failure/reopen, closed/removed members,
  leader replacement away/back and cleanup while requests are still admitted.
  No detached old reply or member approval cancellation can affect the leader.
- Keep an idle Session member attached while its delegation has no terminal
  fact: its observation holds the existing usage lease, including after leader
  resume. Closing only detail keeps an unfinished card's lease; switching or
  detaching the leader releases it. A known failed read does not auto-reattach
  merely because the card is unfinished.
- Restore an ended card in a fresh client with no member observation. Show
  `No model observed`; do not copy the leader/creation model or auto-attach an
  ended member for this label. A selected detail can supply actual member data.
- Include a provider member whose delivered `agent_id` contains `:` and whose
  opaque member ID resembles a Session ID. Card focus, Enter, click, `x` and
  stop-all must send no Session attach/history/stop/detach for it. Its leader
  facts remain visible. When #254 supplies activity, show only received
  leader-side notices as member activity; reconnect does not invent replay,
  request correlation or a dim model. No provider call/approval control is added.
- For Session cards, keyboard focus/Enter and fullscreen click open the correct
  read-only Session.
  Preserve user-message edit/navigation, selection/copy, leader draft, scroll
  anchor and approval priority. Inline `/tasks` still reaches that member.
  Include multiple cards, growing progress, sanitization and 48-column layout.
- Only actual delegation facts set its end. Include cancelled reports that start
  another leader turn, later member work, a stop before delivery and unavailable
  observation. Keep the known case where no terminal collaboration fact arrives
  visible as unfinished/unavailable rather than inventing completion.

### Verified boundaries

The CLI PTY flows `tui_member_cards_share_observations_and_show_the_members_actual_model`
and `tui_member_cards_attach_only_unfinished_members_and_keep_a_failed_read_unavailable`
cover: a named Agent named by one shared `agent.list`, a temporary profile and a
call ID reused across batches as separate cards; one attach per unfinished
native member and none for an ended card or a provider member whose opaque ID
resembles a Session ID; one member-history page at a time; the member's own
`context/configured` and a live model change as the displayed model, with
`No model observed` otherwise; fullscreen Tab/Up and Enter opening the correct
read-only member through the shared observation; a real result ending its card
and releasing the observation while detail is closed; and a failed read staying
unavailable without reattaching. The base detail, stop and stop-all flows now
use ended delegations or a refused card observation where they assert the base
selection lifetime, and the native App Server stop-all flow passes with card
observations. Provider activity notices are not delivered (#254), so provider
cards show only leader facts.

## 9. Extension rule check and ablation

### Requirements raised to mechanism owners

None proposed. Public native Agent metadata, Session-member attachments/history
and addressed stops supply the native view. Its existing observation lifetime
changes as described, including the usage-lease cost. Provider compatibility
follows #254 when available and uses only leader facts/notices, without a
provider Session read/control or a requirement for another backend interface.

| Rule | Application |
| --- | --- |
| §4.1 commands/results | Existing owner facts determine outcomes; existing stop handles control |
| §4.2 handover | Observation changes do not revoke accepted work or adopted inputs |
| §4.3 before | No callback or Timeline write |
| §4.4 core neutrality | No business case added to core, Rpc or Session facade |
| §4.5 batches | Per-branch order only; no cross-Session atomic snapshot or time correlation claimed |
| §4.6 context | Card metadata/progress never enters model context |
| §4.7 public access | Session-only RPCs after the member-kind check, leader facts/notices and native Agent summaries |
| §4.8 infrastructure | Existing connection/response ownership and private attachment identity; no new mechanism |
| §4.9 facts | No event, persistent card index or copied configuration |
| §1 / ADR 002 D1 | Existing CLI owns derived display/I/O; Collaboration and model owners keep execution |
| ADR 002 D2 | Reuse the member observer and canonical renderer; no registry or wiring plugin |
| ADR 002 D3 | A member RPC/read error affects its display; transport failure retains the existing connection failure path |
| ADR 002 D4 | No loader option or new business setting; model policy remains its owner's scope |
| ADR 002 D5 | Existing JSON methods/facts; no Rust-only public interface |

Ablation: one member observation is shared by every card/detail needing the same
Session. The Session-keyed ownership set exists to prevent duplicate attach and
premature detach, not to create another task registry. A single selected detail
and the existing delegation projection remain. No progress-forwarding service,
periodic read, separate client tree, lock, generation protocol or server field
is needed. Actual model facts are read instead of inventing a default.
Provider cards need no member observation or usage lease. The base has one
selected observer; the §7 cards need per-Session observations to show live
cards while detail is closed, with pinning stated rather than hidden behind a
new passive-observer mechanism.
