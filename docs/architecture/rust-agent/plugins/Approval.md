# Approval

Owner: [approval.rs](../../../../crates/dim-agent-capabilities/src/approval.rs).
Turns permission decisions into call admission or durable approval requests and
rechecks policy before allowing an approved call to start.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:Permission` | Evaluate current policy and recheck an allowed response. |
| `B:Dispatcher` | Receive approval responses. |
| `B:Timeline` | Commit requests/cancellations and recover abandoned requests. |

## Events and lifetime

Produces `approval/request` and `approval/cancelled`; consumes `approval/respond`.
Caller cancellation settles its waiter; branch close/reload releases remaining
requests. Recovery cancels unmatched historical requests because their execution
did not survive. The service owns request state, not tool execution tasks.
Responses are not unconditional authority: cancelled batches and changed policy
can prevent execution. Approval does not sandbox an admitted tool.

## Client notifications

Rpc projects the three events at outbound encoding; none reaches `session.event`
or `session.history`. `approval/request` → `approval.request`, also replayed on
attach for pending requests; `approval/cancelled` → `approval.cancelled`;
`approval/respond` → `approval.resolved {sessionId, branchId, requestId, decision}`
(protocol v3 only: the v1 and v2 notification sets are frozen,
[#278](https://github.com/arcships/dim/issues/278)).
`approval/respond` is the command, so every committed response is projected.
Approval accepts the first one it consumes in sequence order: the first
`approval.resolved` per `requestId` carries the accepted decision, a later one
was answered `Unknown`, and an `approval.cancelled` after it means the call did
not start. The decision is the user's answer, not the tool's outcome. A client
that attaches after a response is committed but before Approval consumes it is
replayed the request without that response; its own `approval.respond` then
fails with `invalid_params`, which ends the prompt.

## Tests

[permission_flow.rs](../../../../crates/dim-agent-capabilities/tests/permission_flow.rs):
`an_allowed_approval_is_rechecked_against_the_current_policy`,
`a_response_that_finds_its_batch_stopped_ends_as_an_interrupt`,
`a_deny_rule_written_while_approval_is_pending_cancels_the_allowed_call`;
[local_product.rs](../../../../tests/integration/tests/local_product.rs).

<!-- generated:plugin Approval — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Permission`、`B:Timeline` |
| 提供 | `B:Approval` |
| 被注入于 | `ToolExecutor` |
<!-- /generated -->
