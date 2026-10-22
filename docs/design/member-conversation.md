# Read-only member conversations in the CLI

Status: Proposed; not implemented. Scope: the conversation-detail slice of
[#263](https://github.com/arcships/dim/issues/263). After this contract is accepted,
implementation can use #299 as its declared parent; delivery to `rust-tui` follows
the order #287, #299, then this view. This does not complete transcript-block entry, model policy,
concurrency controls or stop-all.

## 1. Existing mechanisms

`collaboration/delivered` binds a delegation's request event to the actual member
Session. The task panel already retains that identity in #287. A member is a
Session, so the existing client methods can read it:

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

The ordinary resume path is unsuitable for this view: on `rust-tui`,
`Worker::enter` can apply `--permission` to a resumed Session and makes it the
editable conversation. Viewing a member must not select that path, change its
policy or expose a composer for it.

## 2. Entry and layout

In `/tasks`, Enter opens output for a command or a conversation for a delivered
member. With an empty list filter, `x` invokes the existing stop control for either
kind; in a nonempty filter it remains ordinary filter text. The detail has no
filter, so its `x` control remains available. A starting member
has no Session ID: Enter or `x` explains that its target is not available yet and
sends no request. Do not derive an ID from an agent name or prompt.

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
notifications continue to be processed. Existing leader approval/question
prompts take precedence and close this observation through the same cleanup path.
The detail does not expose member approval, question or configuration actions.

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
   no periodic history polling or second connection is needed. Keep a single
   selected member view, not a catalog or another task tracker.
4. Leaving the detail stops further page requests and releases only its member
   attachment with `session.detach`. An admitted attach/page request settles;
   if the view closed while attach was pending, detach after that attach settles.
   Do not let an obsolete reply reopen or replace the current detail. Process a
   subsequent selection after the previous observation's cleanup; connection
   shutdown retains the server's existing attachment cleanup.

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
| Escape or leader prompt replaces detail | Detach only the member observation and return to the leader's existing UI |

A stop reply does not fabricate a collaboration result or remove another member.
Stopping still targets the member's current turn: it cannot retract adopted input
and may race a later turn. This view adds no delegation-specific cancellation or
delivery fence. Detach errors use the existing connection error path; they do not
turn into a Session-close request or an automatic re-attach loop.

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
attachments and member-only detach; verify the leader still accepts input. These
are implementation acceptance requirements, not evidence that the proposed UI
already works. A documentation-only change runs no runtime test suite.

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

Ablation: retain one temporary view projection because replacing the leader's
editable projection would lose its view state or risk writing to the member.
Reuse the existing connection, attachment table, pagination and row rendering;
no extra output stream, polling timer, persistent copy, capability or lock.
