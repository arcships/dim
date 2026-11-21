# CLI current activity

Design for [#1659](https://github.com/arcships/dim/issues/1659). Runtime
implementation requires separate authorization. [Timeline UI](timeline-ui.md)
owns the display layout and [CLI commands](cli-commands.md#command-menu)
owns the command menu.

## Display with shipped facts

Use the existing composer status and task strip in both modes. Foreground
activity describes the selected branch. The task strip and `/tasks` retain
their scope, labels, controls and terminal fade. Permission and question
interactions take precedence, retry keeps its countdown, and a disconnected
or synchronizing view says “Status pending sync” before asserting activity.

| Confirmed activity | Composer text |
| --- | --- |
| Open model request before visible output | Waiting for model |
| Current request with text or thinking progress | Generating or Thinking, following the existing streaming distinction |
| Open dispatched tool batch | Tool batch pending |
| Open turn with insufficient phase evidence | Working |
| Background tasks alone on a branch confirmed idle | Background running |

“Tool batch pending” covers the interval from dispatch through batch settlement,
including scheduling and admission. If the count is useful, show “Tool batch
pending (N calls)” using the total dispatched count. It represents the batch
size, including siblings that may already have settled or still be queued.
An open turn with uncertain activity can show “Working” alongside the task
strip. The strip by itself establishes no foreground wait. An idle line
keeps the existing idle treatment when there is no confirmed background
work. `LineState::Waiting` means an open line with no running turn.

Both modes use the same composer layout: the run status, status slot
(`esc interrupt`, notices, `permission required` / `answer required`,
disconnect), background task count, goal and context usage share the left side
of the composer’s bottom border. The place and unread badge continue to share
the right side of the bottom border. The model label sits on the right side of
the top border, while shell mode keeps the top-border left title. The footer
line below the composer keeps only the permission mode in both modes. While a
prompt replaces the composer, the composer is not drawn, so a notice or the
disconnect state is drawn once, on the rows above the footer. Keep the existing
notice slots and
[notification and clipping priorities](timeline-ui.md#unread-replies).
Shorten the status within its existing space by terminal cell width. Preserve
the phase and any known object ahead of optional elapsed text.

The examples use `o` for the existing task marker and brackets for a history
placeholder. The outer frame in the fullscreen example denotes the viewport.

Scrollback during a dispatched batch at the default tool detail setting:

```text
> Fix the issue and verify
  [committed tool history]

  o CI checks                                     2m13s
+----------------------------------- provider / model -+
| >                                                    |
+-- Tool batch pending ---------- Unread (1) · /lines -+
```

Scrollback while the model generates and a server runs in the background:

```text
  o Dev server                                    8m02s
+----------------------------------- provider / model -+
| >                                                    |
+-- Generating · this turn 24s --------------- ~/repo -+
```

The same generating state in narrow scrollback, with both elapsed labels
omitted, the model label dropped to fit, and the task still visible:

```text
  o Dev server
+------------------------------+
| >                            |
+-- Generating ------- ~/repo -+
```

The task row’s elapsed time comes from the task’s own start. Show “this turn”
elapsed only with the authoritative turn start. Omit unavailable durations.
Dispatch time measures neither tool execution nor wait admission and supplies
no wait clock. A successful task row means the command reported success.
Task failure or cancellation follows the task’s own terminal fact.

## Evidence and recovery

Source reviewed against `077892025020a2875a559b432dfa3256878422ff`.

- [Client facts](clients.md) feed the existing worker and projection.
  Use current turn and request identity with ModelProvider progress. The
  output identity and counts in [session.turn.get](input-state-read.md#turn-identity-read)
  describe the latest open model output at the fixed cutoff.
- Loop writes the top-level dispatch (`step/tool-call`). ToolExecutor
  writes the dispatched batch result (`step/tool-result`). Correlate them
  by Session, branch, turn, request and dispatch event ID (the result’s
  `call_event_id`). An undispatched `step/tool-result` has no
  `call_event_id` and retires no dispatched batch. Protocol v3 and v4 expose
  it. The v1 event view omits it.
  Track the top-level batch for the composer. Nested batches have their own
  identities and do not replace it. Model-streamed call arguments are separate
  from dispatch. [Loop](../architecture/rust-agent/plugins/Loop.md#the-continuations-tool-boundary),
  [ToolExecution](../architecture/rust-agent/plugins/ToolExecution.md#events-and-lifetime-1),
  [v3/v4 event visibility](../../crates/dim-app-server/src/tree_protocol.rs),
  [v1 event view](../../crates/dim-agent-types/src/v1.rs)
- The public schema also has `step/tool-start` and `step/tool-completed`.
  The default assembly constructs `ToolExecutorPlugin::new`, which sets
  `emit_progress` to false. Both per-call appends require that flag. The CLI
  design uses the default batch facts. In `[exec, wait]`, the exclusive
  `exec` occupies the earlier step. The concurrent `wait` is dispatched in
  the same batch, and its handler can run only after that step settles and
  its own admission finishes. The shipped facts give neither the active
  call set nor individual completion before the batch result.
  [ADR 005 D1](../adr/005-observability.md#d1-two-observation-layers),
  [ToolExecution scheduling](../architecture/rust-agent/plugins/ToolExecution.md#scheduling),
  [WaitTool](../../crates/dim-agent-capabilities/src/wait.rs)
- [WorkspaceTools](../architecture/rust-agent/plugins/WorkspaceTools.md)
  publishes background handoff as `exec/started`, settlement as
  `exec/ended`, and pending tasks via `session.exec.pending`. Use the
  existing task projection’s title/description fallback. Handoff and tool
  settlement can leave a task running independently. A task title or
  origin call ID conveys no foreground wait relationship.
- Reuse the bounded transcript and [inventory handoff](../../crates/dim-cli/src/session_host/inventory.rs)
  at the existing owner cutoff, then apply later live facts. Key claims to
  the attachment and branch, deduplicate by branch sequence within that
  attachment and reject stale replies. A batch result or turn end retires
  the corresponding foreground claim. Task settlement wins over a stale
  pending task reply. On branch changes and disconnect, invalidate claims
  from the old view. Restore a pending batch label only with its dispatch
  and coverage through the cutoff and live handoff sufficient to rule out
  settlement. A truncated window, unsuccessful read or gap can leave the
  phase unknown. Pending task inventory and current-turn identity recover
  their own state. Even a complete default batch trace cannot confirm an
  active per-call wait.

## Owner requirement for named waits

To show “Waiting for task: CI checks” in #1659, ToolExecution needs an agreed
public per-call activity contract with [Wait](../architecture/rust-agent/plugins/Wait.md).
ToolExecution owns tool lifecycle and call identity. WaitBranch owns admission
and settlement through the existing branch-local `wait` face. Tool handler start
alone cannot establish that WaitBranch accepted a wait or supply its clock.

The owner decision must cover:

- A scoped identity for the admitted wait: Session, branch, turn, request,
  dispatch event ID and call ID, with an optional task ID. Publish a positive
  admission signal and the authoritative admission time if elapsed time is
  supported. Correlate the target by exact task identity in the same Session
  and branch. Use the task projection’s existing title fallback.
- Matching settlement for notices, steer, timeout, interruption and reload,
  ordered against batch and turn completion. Handle rejected calls and
  immediate outcomes without leaving a live wait claim. A timeout ends the
  wait independently of the target’s outcome.
- Evidence for any claim that the foreground is waiting on one object.
  A concurrent wait can share a step with calls such as `read`. Use the
  neutral batch status while a sibling is active or its state is unknown.
  A confirmed untargeted wait can say “Waiting for a background task or
  new input”. A confirmed targeted wait with an unavailable task can say
  “Waiting for task: details unavailable”.
- If exact reconnect restoration is required, a bounded owner read at a
  fixed cutoff and an ordered handoff to live updates. A gap leaves the
  relationship unknown. WaitBranch reload settles accepted calls and
  rebuilds open-turn status, so a reload does not reconstruct an old wait.

The owners decide whether existing progress facts can contribute and what
additional admission or settlement evidence is needed. The design with
shipped facts adds no service face, Timeline event, stored format or flag
change. The conditional status below depends on that owner decision and
an admitted wait with an authoritative start.

Conditional scrollback and fullscreen examples:

```text
  o CI checks                                            2m13s
+----------------------------------------- provider / model -+
| >                                                          |
+-- Waiting for task: CI checks · wait 1m40s ------- ~/repo -+

+----------------------------------------------------------------+
| [tool history at current detail setting]                       |
|                                                                |
| o CI checks                                              2m13s |
| +----------------------------------------- provider / model -+ |
| | >                                                          | |
| +-- Waiting for task: CI checks · wait 1m40s ------- ~/repo -+ |
+----------------------------------------------------------------+
```

## Cost and terminal signaling

Keep the [terminal signaling contract](plugins/Tui.md#terminal-channels)
for OSC 7501, OSC 9;4, title, notifications and cleanup. Permission and
question remain the user-blocked states. Composer text does not select
OSC states. Update from existing notifications and reuse the local repaint
cadence, changing elapsed text at most once per second. UI teardown, a slow
read or a failed frame leaves timeout, cancellation and execution with their
owners. Display adds zero model requests, tokens, progress tool calls or
adopted inputs. Repainting adds zero RPCs, history scans or remote CI polls.

## Verification plan for implementation

- Reuse the [scripted tool and wait flows](../../tests/integration/tests/local_execution.rs)
  with the default assembly. Follow model output, exec dispatch, handoff,
  wait dispatch, batch settlement and the next request. Exercise success,
  failure, cancellation, wait timeout, steer and stop. In an `[exec, wait]`
  batch, delay exec and approval and verify the display never claims a
  named wait, running call count or wait clock. Check the task row and
  foreground status settle independently. Compare model inputs, requests,
  tool calls and task outcomes. Count RPCs during a quiet repaint interval
  and delay an owner reply while checking input and tool settlement.
- Use projection and frame coverage for a background server during
  generation, multiple dispatched calls, approval, question and retry,
  missing titles and turn origins, attachment/branch switches, truncated
  windows, duplicate events, stale replies and terminal task facts arriving
  before pending replies. Verify an undispatched result does not retire a
  dispatched batch and restored tasks never imply a wait. Check
  OSC bytes independently through the existing terminal status checks.
- With a disposable `DIMCODE_HOME` and deterministic provider, capture
  both modes at 100, 60 and 32 columns using a synthetic “CI checks” task.
  Check the unified composer borders (top-right model label and bottom-left
  status dock) in both modes, the place/unread label and notifications, default
  visibility without `/tasks`, history scroll, detail toggles, branch changes,
  disconnect, stop and resume. This scenario can join the local tmux suite under
  [#1803](https://github.com/arcships/dim/issues/1803) when it lands.
  Until then, use the existing isolated local terminal harness.
- After an owner contract is approved, separately verify the named status
  only appears after admission and clears at the matching settlement.
  Cover untargeted and unknown-target waits, colliding task IDs across
  scopes, immediate outcomes, active or unknown concurrent siblings and
  missing clock origins.
  If exact reconnect becomes part of that contract, test owner cutoff,
  live handoff, stale settlement and wait generation reload.

## Ablation review

The existing composer, task strip and bounded projection suffice for
batch-level display. Keep batch and task identities separate because
their lifetimes diverge at handoff. A per-call tracker, an additional
store or a timer read cannot establish admission from the default facts.
Named waits depend on the owner contract. Exact restoration is a
separate requirement for that contract to settle.
