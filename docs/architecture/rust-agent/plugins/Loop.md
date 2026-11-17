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
`step/model-start`, tool-use start/use, model-output-end,
tool-start/completed/result, `context/compact` and `context/compaction`.
Produces turn start/end, input consumption, model-start, `context/prepare`,
tool-call and recovery terminal facts. `AgentLoop::compact` writes
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

## Failure handling boundaries

Request-local retries belong to [ModelProvider](ModelProvider.md), under the
[retry policy](../../../design/model-failures.md#4-retry). Loop handles the
settled failure: context-budget recovery when applicable, the bounded
continuation below, output-limit completion, or turn failure. These paths do
not require a registered recovery plugin.

Plugin-decided continuation after partial output is a separate
[ModelRecovery proposal](../../../design/plugins/ModelRecovery.md), not a Loop
service or event available to callers.

## A failed step whose calls were announced and never executed

The Loop checks the failed output for calls it announced
and never ran. A failed output executes nothing: a top-level tool batch is
written only from a `ModelOutputOutcome::Completed` output, and ToolExecutor
consumes `step/tool-call` alone, so `step/tool-use-start` without a batch is the
whole evidence. ModelProvider has already closed every such call with an error
result in the same batch as the terminal.

When the request announced at least one such call, no driving input is pending,
the turn is not interrupted, the turn's continuation is unspent, and the failure
is one a new request can settle, the Loop releases the failed request's tool
binding and starts one new request in the same turn. The order is fixed:

1. the context-budget fact applies: budget recovery runs, exactly as before,
   and it neither reads nor spends the turn's continuation;
2. a call was announced, the continuation is unspent, the provider did not veto
   retrying the failure, and its kind is one a new request can settle: one
   continuation;
3. the failure is an output-limit stop: `turn/end` Completed with reason
   `output_limit`, whether or not the slot is spent;
4. otherwise `turn/end` Failed.

A kind a new request can settle is `network`, `rate_limited`, `server_error`,
`malformed_response`, `output_limit` or `unknown`. The kinds that need the user
or another owner to act first never continue: `auth_rejected`,
`quota_exhausted`, `not_found`, `invalid_request`, `content_filtered` and
`context_too_long`. The same context fails the same way, an unrecoverable
rejection is not repeated behind the user's back, and a context that is too
long is budget recovery's case when its fact applies. The provider's veto is
`ModelFailure.retryForbidden`, written by ModelProvider's failure policy; the
model-failures contract says the prohibition covers a continuation
([model-failures](../../../design/model-failures.md)). A failed terminal with no
structured failure does not continue. The gate matches
`ModelFailureKind` exhaustively, so a new kind forces a decision here.

The continuation is a plain request with tools, as after a settled batch, so
the model can re-issue the call it withdrew. It reads the committed error
results and carries no injected instruction. Its `context/prepare` carries the
failed request's `failure_chain`, its attempts and waited time, so both
requests spend one 8-call, 300 s budget instead of the continuation taking a
second one. A compaction step that runs at the boundary before the
continuation keeps the chain across the step and passes it to the request the
step's resumption starts. The turn accepts this automatic continuation at most
once. An output-limit stop is not a failure of the turn:
it ends Completed, and its `step/model-output-end` stays for diagnostics.

### The continuation's tool boundary

This continuation declares tools because the failed request ran none of the
calls it announced:

- only a Completed output produces `step/tool-call`
  ([agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs));
- the batch's handler claims are made immediately before that append and held
  with the request's tool snapshot, so a call that is never dispatched — here,
  because the turn ends before the batch — releases whatever it claimed when the
  snapshot is dropped (ToolExecution, [Questions §2.1](../../../design/plugins/Questions.md#21-the-claim-before-the-fact));
- ToolExecutor consumes `step/tool-call` and nothing else
  ([tool_executor.rs](../../../../crates/dim-agent-capabilities/src/tool_executor.rs)),
  so no call of a failed output reaches a tool;
- ModelProvider closes the failed output's started calls as error results in
  the terminal's batch
  ([model_provider.rs](../../../../crates/dim-agent-capabilities/src/model_provider.rs)).

No eager or streaming path bypasses these boundaries, so the continuation stands
where a normal step stands after a settled batch of error results. A call it
makes is a new attempt under the same approval, permission and non-idempotence
rules, not a replay of an executed operation.

## Tests

[host_flows.rs](../../../../crates/dim-agent-capabilities/tests/host_flows.rs),
[flows.rs](../../../../crates/dim-agent-capabilities/tests/flows.rs),
[interrupt_flow.rs](../../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs),
[context_flow.rs](../../../../crates/dim-agent-capabilities/tests/context_flow.rs)
cover the failed-step settlements this page states, the continuation for calls
that were never executed, the retry chain it shares, turn progression,
interruption, pending-input restore and the manual compaction step.

<!-- generated:plugin Loop — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding`、`S:CompactionStrategyHost`、`S:ContextModelHost`、`S:GuideEntries`、`S:HistoryRenderer`、`S:PrepareModelHost` |
| 提供 | `B:AgentLoop`、`B:StateCell<LoopSnapshot>`、`B:dyn LoopState` |
| 被注入于 | — |
<!-- /generated -->

## Request Session identity

When Loop constructs `ContextPrepareData.request`, it writes the owning Timeline's
Session ID into request metadata. This identity is fixed before context preparation
and persisted with the model call. Final model execution uses it for Session-scoped
file reads; model configuration cannot select another Session's files.
