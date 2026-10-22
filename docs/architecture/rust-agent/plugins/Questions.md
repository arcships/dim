# Questions

Owner: [dim-agent-questions](../../../../crates/dim-agent-questions/src/lib.rs).
Provides the `ask_user` tool (`toolContribution#ask_user`) and the Session
method `session.question.respond` (`sessionMethods["session.question"]`). The
model asks the user 1 to 4 multiple-choice questions and waits; a client
answers or declines. The question is the pending tool call and the answer is
its `ToolResult`, so Questions writes no events. Design:
[Questions](../../../design/plugins/Questions.md).

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionConfig` | `interactive`, read on each call: a non-interactive Session gets an error result at once. |

## Tool

`ask_user {questions: [{id?, header?, question, options: [{label, description?}], multiSelect?, allowCustom?}]}`,
risk `session`, source `extension`. Unknown fields are refused.

| Outcome | `isError` | Text the model reads |
| --- | --- | --- |
| Answered | absent | `User answered:` then `<n>. <question>` and `   -> <label>, ..., (custom) <text>` per question |
| Declined | absent | `User declined to answer. Proceed without these answers.` |
| `interactive` is false | true | `No one is available to answer questions in this session. Proceed without asking.` |
| Invalid input, duplicate `(branchId, requestId, callId)` | true | `Invalid ask_user input: <reason>` |
| Batch token fired while waiting | true | `The question was cancelled before the user answered.` |
| This part unloaded | true | ToolExecutor's reload settlement, `Tool execution cancelled: capability reloading` |

Input rules: 1 to 4 questions; ids unique, defaulting to the position (`"1"`,
`"2"`, ...); a header of at most 40 characters; non-empty question text; 2 to
6 options with non-empty, unique labels. `multiSelect` defaults to false and
`allowCustom` to true. An answer's `structuredContent` is
`{answers: [{questionId, selected, custom?}], declined}`, answers in question
order.

## Method

`session.question.respond {sessionId, branchId, requestId, callId, answers? | declined: true}`
(mutating, `report`, no progress) replies `{}` when the waiting call takes the
answer. `requestId` is the `request_id` of the call's `step/tool-call`; with
the branch and the call id it names the call the client showed.

```text
params ──> wrong shape, both or neither of answers/declined ──> invalid_params (-32602)
   │
   v  lock the waiter table
(branchId, requestId, callId) waiting? ── no ───────────────> question_not_pending (-32000)
   │ yes
   v
declined, or answers valid for the call's questions? ── no ─> invalid_params; the call keeps waiting
   │ yes
   v
remove the entry, hand the answer over under the lock ─────> {}
```

Answers must name every question exactly once by `questionId`, choose only
offered labels, each at most once, and give at least one choice. A question
without `multiSelect` takes exactly one label or one custom text. `custom`
needs `allowCustom` and non-blank text. Rpc checks only the shape it owns and
the `sessionId`; the answer is checked here, against the call it answers.

## Events and lifetime

No Timeline events, no `StateCell`, no Dispatcher subscription. The Session
part keeps one table, `(branchId, requestId, callId) -> {questions, sender}`,
under a mutex. An entry lives exactly as long as its handler waits, and whoever
removes it under the lock decides the call's output:

```text
                         ask_user handler
input valid, interactive ──> insert entry ──> wait ─┬─ respond removes it ───> answer or decline
                                                    ├─ batch token fires ────> handler removes it:
                                                    │                          "question was cancelled"
                                                    └─ part unloads ─────────> table drained, senders
                                                                               dropped: reloading
```

`respond` sends under the lock, so a cancel that finds no entry receives the
answer and returns it; ToolExecutor keeps a returned result and the turn still
ends cancelled. ToolExecutor owns the batch token, the terminal
`step/tool-result` and the turn outcome:

| Event | What ends the wait | Result |
| --- | --- | --- |
| `stop` or interrupt of the branch | batch token | The cancelled text; the turn is cancelled |
| `session.close`, branch close | the close's interrupt of the branch, then the batch token | The cancelled text; the turn is cancelled |
| ToolExecutor reload | batch token | Reload-settled; the turn fails |
| Questions reload | the unload drains the table | Reload-settled; the turn fails |
| Process restart | nothing is waiting | ToolExecutor's rebuild settles the open batch as not executed |

The table is generation memory and is never rebuilt: nothing is asked again.
After this part reloads, ToolRegistry offers the new generation from the next
request, without a Session reload
([session-boundary A.5](../../../design/contracts/session-boundary.md#a5-tool-contribution)).
A call that still reaches the unloaded generation, from a request built before
the reload, finds its table closed and is reload-settled instead of waiting on
nothing. Call ids are unique, by convention, only within one
batch: a provider may use one again in a later request or on another branch.
The key therefore holds the branch and the `request_id` of the call's batch
(`ToolExecutionContext.request_id`). A late answer or decline to a settled call
finds nothing (`question_not_pending`) and never reaches a later call that
reuses its id.

## Installation

`dim_agent_questions::install` records the tool name and the namespace and
installs the Session part. A Session declares `ask_user` when its
`SessionConfig.tools` lists it. The product and configured App Server profiles
install it and list `ask_user` in their default Session tools; the mock
profile does not. Installation never reads `interactive`, so interactive and
non-interactive Sessions declare the same tools. This graph installs it as the
product profiles do.

## Tests

[questions_flow.rs](../../../../crates/dim-agent-questions/tests/questions_flow.rs)
drives the Agent runtime with a scripted model: answers and a decline as the
results the model reads, rejected answers, two concurrent answers, invalid
input, a non-interactive Session, a duplicate call id and an already fired
token through the tool contract, stop before and after the answer, Questions and ToolExecutor
reloads, a call to the unloaded generation and the next request asking through
the new one, `session.close` with a restore, the same call id on two branches,
and a late answer and decline to a settled call whose id a later request
reuses, which leave the later call waiting for its own answer.
[transport_flow/questions.rs](../../../../crates/dim-app-server/tests/transport_flow/questions.rs)
covers the method through Rpc and `session.methods`, the `step/tool-result` a
client reads, a non-interactive Session, the fork refusal at a pending
question, and a second client's late answer and decline to a call id the model
reuses in the next turn, with the answer the model then reads. The configured
binary flow in
[transport_flow.rs](../../../../crates/dim-app-server/tests/transport_flow.rs)
checks that the product tools include `ask_user`.

## Rule check

Rules: [principles](../principles.md) §1 and §4,
[ADR 002](../../../adr/002-plugin-scope.md) D1 to D5 and
[ADR 003](../../../adr/003-approval-scope.md).

| Rule | Answer |
| --- | --- |
| §1 Bounded Capability | One Session Capability in its own crate. `Deps` is `(InSession<SessionConfig>,)` and `keys()` is `scoped::<Deps>`; `apply` registers the unload disposer with `ctx.effect` before it provides the tool and the method, and starts no background work. Rpc and ToolExecutor hold no Questions logic. |
| §4.1 Command events for others' state | Not applicable: Questions changes no other module's state. The answer is the output of its own tool; ToolExecutor writes the call's terminal fact. A `respond` that finds nothing writes nothing; its reply says why. |
| §4.2 No recall after handoff | The answer is handed to the waiting handler once, under the lock, and is never withdrawn; a later cancel does not replace it. |
| §4.3 Before callbacks | Not applicable: no before callback. Rpc checks shape and `sessionId`; Questions checks the answer as the feature owner. |
| §4.4 No business meaning in core | Timeline, Dispatcher, ToolExecutor, ToolRegistry and Rpc are unchanged and have no Questions case. |
| §4.5 Atomic batches | Not applicable: Questions writes and consumes no events. The answer commits with its batch in ToolExecutor's `step/tool-result`. |
| §4.6 Context from handed-off events | The model reads the answer only as the call's tool result. |
| §4.7 Public events and services | Questions reads `SessionConfig` and the cancel token its tool contract passes; it reads no other plugin's state and calls no other plugin. |
| §4.8 Infrastructure unchanged | No change to Timeline, Dispatcher, envelope, assembly or facades; no requirement raised. |
| §4.9 Only facts later decisions use | No event added. Other clients close their prompt at the batch's `step/tool-result`; no earlier notice is sent. |
| ADR 002 D1 Scope | Tool, method and waiters belong to the Session part: a waiter is the handler's own call, which the Session's tool contribution runs. Branch lifecycle reaches it through ToolExecutor's batch token. The ADR's assignment table lists a branch part; the design removed it, and this follows the design. |
| ADR 002 D2 No wiring modules | ToolRegistry and Rpc collect the two services by name; there is no adapter. |
| ADR 002 D3 Dependencies toward the core | Questions depends only on `SessionConfig`; nothing depends on Questions. Its reload fails the turns that were waiting, an accepted cost. |
| ADR 002 D4 Configuration | Not applicable: Questions has no configuration. `interactive` is Session creation data read per call. |
| ADR 002 D5 Language-neutral contracts | The tool and the method are JSON shapes with documented error codes; this crate and `install` are the Rust binding. |
| ADR 003 Approval scope | `ask_user` is a model tool call that Permission admits under risk `session`; Questions adds no approval and runs nothing from configuration. |

<!-- generated:plugin Questions — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionConfig` |
| 提供 | `S:SessionMethods#session.question`、`S:ToolContribution#ask_user` |
| 被注入于 | — |
<!-- /generated -->
