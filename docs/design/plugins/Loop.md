# Loop

Scope: branch. Issue: #2027.

Loop owns one line's turns: when a turn starts, which request runs, how a tool batch settles, and when the turn ends. ContextPreparation owns the request's content and its compaction, ModelProvider owns one model call, and ToolExecutor owns tool execution; the current implementation is described in the [Loop plugin page](../../architecture/rust-agent/plugins/Loop.md).

## Use and scope: responsibility split

Loop owns:

- **Turn and step order.** Which input opens a turn, the one request a step runs, when the request's calls settle, and the turn's single end.
- **Input adoption.** Which applied inputs drive a turn, which join the next request, and which a turn consumed.
- **The compaction boundary.** No request starts while ContextPreparation's compaction step runs; the step, its command and its outcome belong to ContextPreparation.
- **Turn and step facts.** `turn/start`, `turn/end`, `input/consumed`, `step/model-start`, `context/prepare`, `step/tool-call`, the recovery terminals, and the failure chain a continuation carries.
- **Recovery.** After a reload, unsettled requests, open turns, interrupts and undispatched calls settle without repeating an external effect.
- **The run state.** The phase and the open turn, as a value other owners and clients read.

Callers keep as business:

- What a turn is for, and which input the operator or a plugin sends.
- The request's messages, system prompt and tools: ContextPreparation, Prompt and ToolBinding supply them.
- Which model runs and what a failure means: ModelProvider decides the request's settlement, and Provider runs it.
- When a manual compaction is asked for: the client calls ContextPreparation.
- What a tool call does: ToolExecutor and the tool's owner.

| Requirement (caller) | Can callers compose it from the operations below without breaking Loop's invariants? | Handling |
| --- | --- | --- |
| A collaborator stops a member line's turn only while that turn is the open one | No: the open-turn check and the interrupt commit must happen under the lock that opens and closes turns, or a stale interrupt voids the next turn's queued inputs | `stopTurn` |
| A client or UI reads the running state and follows changes | No: the phase is Loop's folded state, and a poll loop in every caller duplicates the fold | `state`, `observe` |
| A Session method answers a turn read: identity, outcome, active output | Yes: `timelineTree.read` at the line's head answers identity and outcome, and ModelProvider's `progress` answers the active request | Business composition; `session.turn.get` is migrated in the delivery |
| A client asks for a manual compaction | Not this owner: the command, the policy and the outcome are ContextPreparation's | ContextPreparation's `compact` |
| A caller learns whether a turn is running before writing an input | Yes: `state` | Business |

An operation exists only where the row says No.

## Caller example

Collaboration stops the member's turn that consumed the request's input; the answer says whether an interrupt was written:

```rust
let Some(_) = agent_loop
    .stop_turn(StopTurn { turn_id: turn, reason: INTERRUPTED.into() })?
else {
    return Ok(()); // the turn already ended; nothing was written
};
```

A client reads the phase, then follows changes for the rest of its connection:

```rust
let RunState { phase, turn_id } = agent_loop.state()?;
agent_loop
    .observe(&ctx, Arc::new(move |state| show(&state)))
    .await?;
```

`observe` calls the listener once with the current value, then on every change, until the calling row unloads or the owner closes.

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| RequestSettings (#2025) | `settings` | The Session's default model settings before a configuration is adopted. |
| ContextPreparation | `prepare` | Build the request at the source's cutoff from the projection and publish the handoff facts. |
| ToolBinding (#2028) | `bind`, `adopted`, `claim`, `transfer`; `release` and `retain_only` | Pin the request's tool generation, read its declarations, register the calls' claims and move the record to a retry. Settlement otherwise follows the line's facts; the Loop calls `retain_only` after its own rebuild, because ToolBinding outlives a Loop reload and only the new Loop knows the request it is on, and may call `release` when it knows a request ended sooner. |
| GuideEntries (#2026) | `compose` | The System the request carries. |
| Provider (#2019) | — | Not called by Loop; ModelProvider calls `invoke`. |
| BranchScope | `timeline`, `dispatcher`, `timelineIdentity`, `branchLifecycle` | The line's facts, their order, its identity and its phase. |

The exact names above are the siblings' contracts; Loop calls those operations, not their old objects.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `stopTurn` | Stop one turn if it is still the open one | `{turnId, reason}` | `{sequence?}`: absent when the turn already ended or the id is not the open turn | None; the check and the commit are one lock section | `closed` |
| `state` | The phase and the open turn now | `{}` | `{phase: idle\|running\|stopping, turnId?}` | None | `closed` |
| `observe` | The current state, then every change, until the row unloads | the listener argument | `{}` | The row's unload ends the subscription; nothing is released by the caller | `closed` |

`listener` is a callback by reference: it runs outside Loop's turn lock and outside the Timeline's commit lock. A lease id is bound to the calling row; the owner drops it at the row's unload, so a caller that loses the id leaks nothing.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `closed` | `stopTurn`, `state`, `observe` | The line is closing; stop starting work on the line; the next generation serves a reloaded line |

`stopTurn` has no other refusal: a turn that already ended is an absent reply, not an error, because the caller's next step is the same.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `stopTurn` | Take the turn lock, check the id is the open turn and not ending, append `stop/interrupt` under the same lock, answer its sequence | No wait to cancel; a dropped call leaves the committed interrupt | An interrupt before close settles the open turn as cancelled; a turn without one is settled by the close | Loop |
| `state` | Read the folded phase and open turn | Nothing to release | `closed` | Loop |
| `observe` | Register the listener under the calling row, call it once, then on each change | The row's unload drops the listener | The generation stops calling and releases every listener | Loop; the listener's own work by the calling row |
| internal turn pipeline | A driving input opens `turn/start`; the step reads the adopted configuration or `requestSettings`, `guideEntries.compose`, binds the tool generation and reads its declarations, writes `context/prepare`, calls `contextPreparation.prepare` with the source and projection, then follows `model/call`, the model facts and `step/tool-call` to the turn's single end | A stop cancels the step's task; the turn ends cancelled after the settlements Loop owes | A reload settles open turns, in-flight requests and undispatched calls as cancelled; no external effect is repeated | Loop; the request's external effect by Provider |

The request pipeline's own tasks and cancellation are Loop's: handlers register tasks and return, and cleanup cancels and joins them before the generation unloads.

## Exceptions with reasons

None. Every cross-owner call lands on an operation above or on a sibling's operation named in Dependencies.

## Supersedes

- `docs/design/contracts/runtime-owner-operations.md`: the `agentLoop` rows (`compact`, `stopTurn`, `state`, `observe`); the compaction row now points at ContextPreparation.
- `docs/design/service-faces.md` section 3.5: the `agentLoop` and `loopState` rows.
- [ContextCompaction](ContextCompaction.md) section 1 items 4-5, section 1.1 and section 2.1: the manual compaction command and step are ContextPreparation's, and the Loop only starts no request while a step runs.

## Verification

- `stopTurn` on the open turn writes one turn-scoped interrupt; on an ended turn it writes nothing; a stale id after an interrupt writes nothing.
- `state` reports `running` for an open turn, `stopping` after an interrupt, `idle` otherwise, and the open turn's id only while it is open.
- `observe` delivers the current value, then one call per change; the row's unload and the owner's close stop delivery.
- A reload settles an open turn as cancelled, an in-flight request as cancelled, and an undispatched call with an error result, without calling a model or a tool again.
- The compaction step's facts are ContextPreparation's; Loop starts no request while `context/compaction-start` has no matching `context/compaction`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part and a Capability: it folds its own facts and writes its own turn facts. |
| §2 scopes | Branch: turns and steps belong to one line and end with it. The Session entry `session.turn.get` reads values and facts, not Loop's state. |
| §3 orthogonality | No other owner's state is read or changed; the request's content is the siblings' operations. |
| §4 rule 1 | A stop is the `stop/interrupt` command; the turn's end is Loop's fact. |
| §4 rule 2 | A turn's input handover is `input/applied`; after it only Loop's commands apply. |
| §4 rule 3 | Handlers register tasks; the step's host work runs in those tasks. |
| §4 rule 4 | No business meaning: what a turn is for stays with the caller. |
| §4 rule 5 | A turn end is one batch; the settlements and the end commit together. |
| §4 rule 6 | Context enters only through adopted inputs and tool results. |
| §4 rule 7 | Callers use the three operations; Loop uses the siblings' operations. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Every turn and step terminal fact is published after its batch commits. |
| §7 values, ids and objects | The state is a value; `observe` takes a callback; no object or handle crosses. |
| §7 one interface | One name, `agentLoop`, on the `Loop` type; the old `compact`/`stop_turn` methods and `dyn LoopState` trait are removed. |
| §7 cross-language | Every operation is data or a callback; no method exists only for Rust. |
| ADR 002 D1 | Branch: a turn is work bound to one line. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |

## Requirements raised to mechanism owners

- #2028 (ToolBinding): Loop needs `bind`, `adopted`, `claim`, `transfer`, `release` and `retain_only` as data operations. Settlement otherwise follows `step/tool-result` and `turn/end`; `retain_only` is the Loop's own rebuild step, since ToolBinding keeps its records across a Loop reload, and `release` lets the Loop settle a request it knows has ended.
- #2026 (GuideEntries): Loop needs `compose` as a data operation returning the System text for one line and a base System; the current `compose_system` takes Rust values.
- #2025 (Session access): the Session entry `session.turn.get` reaches `line_owner::<Loop>(line)?.state()` and `timelineTree.read` by line id; ADR 027's id-to-instance step resolves the branch group's data face, except registration points and lifecycle services.

## Binding appendix

The service is marked on the `Loop` type; the wire name stays `agentLoop`.

```rust
use rutis::Ctx;
use std::sync::Arc;

pub type RunStateListener = Arc<dyn Fn(RunState) -> rutis::host::BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoopPhase {
    Idle,
    Running,
    Stopping,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunState {
    pub phase: LoopPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopTurn {
    pub turn_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stopped {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LoopError {
    #[error("closed")]
    Closed,
}

#[rutis::service]
impl Loop {
    pub fn stop_turn(&self, request: StopTurn) -> Result<Stopped, LoopError> { /* .. */ }
    pub fn state(&self) -> Result<RunState, LoopError> { /* .. */ }
    pub async fn observe(&self, caller: &Ctx, listener: RunStateListener) -> Result<(), LoopError> { /* .. */ }
}
```
