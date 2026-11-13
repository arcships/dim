# Loop

Owner: [agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs).
Coordinates turns, request preparation and model/tool steps from adopted inputs;
exposes loop state without itself executing model or tool effects.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:ContextPreparation` | Transfer the request-local projection without another history read. |
| `S:HistoryRenderer` | Render supported context history for requests. |
| `S:ContextModelHost` | Window facts; with the strategy, context recovery and manual compaction are available. |
| `S:CompactionStrategyHost` | The compaction strategy its manual step calls through ContextPolicy. |
| `S:PrepareModelHost` | Prepare the request before context/model execution. |
| `B:ToolBinding` | Pin and release request tool handlers. |
| `B:Dispatcher` | Hand off restored state to live step outcomes, inputs and interrupts. |
| `B:Timeline` | Commit turn/step transitions. |
| `B:TimelineIdentity` | This branch's id selects the guide entries in force. |
| `S:GuideEntries` | Compose the System it places on a host-prepared request. |

## Events and lifetime

Consumes `input/applied`, `input/consumed`, the `input/message` and
`input/deleted` of handover inputs, `stop/interrupt`, turn boundaries,
`step/model-start`, tool-use start/use, model-output-end, `step/disposition`,
tool-start/completed/result, `context/compact` and `context/compaction`.
Produces turn start/end, input consumption, model-start, `context/prepare`,
`step/disposition`, tool-call and recovery terminal facts. `AgentLoop::compact` writes
`context/compact`; the Loop serves it as a compaction step at its next step
boundary, or at once with no turn, and starts no request or turn while the step
runs (ContextCompaction §5.4). An interrupt cancels a running step and settles a
pending command; reload settles both as `cancelled`. Preparation work is tracked; handlers
schedule it without waiting for host I/O. Cleanup cancels and joins work.
Recovery queries current ordinary/handover input members, then applied facts for only those IDs, unsettled manual compaction
commands/starts, the last interrupt and the last turn boundary at one committed
cut. Only an open turn needs a paged read of its request control fields. Input
selection keeps the entire branch range, including applications before that
turn. Recovery holds identities, stages and owed settlements; it does not
reconstruct request bodies or tool arguments. Undispatched completed calls are
closed in the turn-end batch, in their original start order. Orphaned requests
are settled without repeating external effects. Reload-settled tool results
fail the turn. An idle append alone starts no turn. Domain-specific history must
not become a second input-adoption path.

`AgentLoop::stop_turn` is the branch-local way to stop one turn: under the lock
that opens and closes turns it checks that the named turn is the open one and
not already ending, commits the `stop/interrupt` only then, and writes nothing
for any other id.

## A failed model step and its disposer

The Loop owns one port, the disposer of a failed model step. The port is a
Rust-only operation, like the Permission advice precedent. Any Rust branch
part may register one disposer under a name, as an effect of that part,
following the
[Permission advice](Permission.md#tool-advice) precedent: the registration lasts
while the part runs, unloading withdraws it, a withdrawal while the Loop waits
counts as no answer, and cancelling drops the pending answer. At most one
disposer is registered on a branch, and a second registration fails. The port
has one operation, `dispose(handover, cancel)`: `handover` is `{turn_id,
request_id, failure_sequence}`, where `failure_sequence` is the sequence of the
failed `step/model-output-end`; the answer is `continue {input_id}` or
`finish`. The disposer answers promptly and observes `cancel`: it reads
committed facts, writes at most one handover input, and does no other waiting.
One that blocks keeps the handover open until stop, steer or branch close, each
of which always ends it. The port names no plugin and no provider category, and
the Loop never interprets the failure.

### Opening a handover

The Loop considers a handover when it consumes `step/model-output-end` with
`status: failed`. All of these must hold; otherwise the step settles without a
handover: budget recovery when it applies, `turn/end` Failed otherwise.

- the terminal carries a structured `failure`; a context-preparation failure
  has none;
- the Loop's own budget recovery does not apply;
- the turn is open, not interrupted, not ending and not a turn the rebuild
  failed;
- no driving input (steer or queue) is pending;
- every tool batch of the turn is settled;
- a disposer is registered.

Opening writes no fact. The Loop keeps the handover in its turn state and runs
one disposition task, tracked like its other step tasks.

### The disposition task

The task waits for the first of four outcomes:

- the disposer answers; the task appends `step/disposition` with that answer;
- the disposer errors; it appends `finish`;
- the disposer is withdrawn; it appends `finish`;
- stop, steer or branch close cancels it; it appends nothing.

The task never ends the turn itself, so stop and steer are ordered against the
decision by Timeline sequence.

`step/disposition {turn_id, request_id, failure_sequence}` is a new core kind in
the `step` domain. Its `decision` is `continue {input_id}` with `input_id`
required, or `finish` without one; a record of any other shape fails to decode.
The Loop's disposition task is its only writer, and its only reader is the
Loop's in-sequence consumption: it orders the disposer's answer against stop
and steer, which is why it is a Timeline fact (§4.9).

### Consuming `step/disposition`

- **Stale.** The turn is closed, the handover was dropped, or the identity does
  not match the open handover; the Loop ignores it.
- **`finish`.** The Loop writes `turn/end` Failed with the original failure's
  error.
- **`continue`.** The named input must be a handover input of this handover
  ([InputQueue](InputQueue.md#handover-inputs)) that is neither deleted nor
  consumed; otherwise the Loop treats the answer as `finish`. Then one batch,
  in this order:
  - `input/consumed` for every pending Append input;
  - `input/consumed` for the handover input, so its text is the last model
    message;
  - `step/model-start` for a new request id in the same turn.

The continuation request is prepared fresh from committed history, so the
committed partial output and the real tool results are context. It does not
reuse the failed request's prepared source, as budget recovery does. It
declares no tools and carries `failure_chain`; a call it emits is refused at
the stream boundary and by the executor (ToolExecution).

### While a handover is open

- **Stop.** `stop/interrupt` cancels the task, and the Loop ends the turn
  Cancelled itself; it does not leave a turn with no request for the next
  `next_model`.
- **Steer.** A steer drops the handover and cancels the task. The steer is
  consumed by a normal request in the same turn, with tools, under the
  existing steer contract.
- **Append.** Append inputs wait for the next request.
- **Queue.** Queue inputs are held to turn end.
- **Manual compaction.** An open handover is not a step boundary, and starting
  the continuation is not one. A `context/compact` consumed while a handover is
  open waits for the next boundary after the continuation request ends, which
  for a text-only continuation is the turn's end; on `finish` it runs once the
  turn has ended, as with no turn. A compaction before the continuation could
  summarize away the committed partial output the continuation must continue
  from. The `ContextCompact` handler starts a step
  only when the turn has no request, no handover is open and no continuation
  batch is being written; the continuation batch order is unchanged.
- **Reload.** The rebuild fails the open turn
  ([agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs)),
  and a late `step/disposition` is stale.
- **Fork and replay.** No model or tool effect is replayed. A fork's rebuild
  fails an open turn, and its late decisions are stale.

### The bound

The wait ends on the disposer's answer, its withdrawal, which unloading always
causes, stop, steer or branch close. There is no wall-clock timer: a
disposition does no waiting of its own, and any backoff belongs to
ModelProvider inside the continuation request.

## Tests

[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs),
[interrupt_flow.rs](../../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs),
[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs)
cover turn progression, interruption, pending-input restore and the manual
compaction step.

<!-- generated:plugin Loop — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding`、`S:CompactionStrategyHost`、`S:ContextModelHost`、`S:GuideEntries`、`S:HistoryRenderer`、`S:PrepareModelHost` |
| 提供 | `B:AgentLoop`、`B:StateCell<LoopSnapshot>`、`B:dyn LoopState` |
| 被注入于 | — |
<!-- /generated -->
