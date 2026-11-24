# CollaborationTimeline

Scope: branch. Issue: #2020. Process part: [Collaboration](Collaboration.md); Session part: [CollaborationSession](CollaborationSession.md).

CollaborationTimeline owns one line's delegations: the requests the line started, their deliveries and results, the approval routes of that line, and the settlement of unfinished work when the line is rebuilt or closed. This document is the owner's target contract; the current implementation is `CollaborationTimelinePlugin`, `BranchRecord`, `State` and `restore`/`unload` in `crates/dim-app-server/src/collaboration.rs`. The capability half is `CollaborationTimeline` (a branch Capability); the line's delegation state is folded and written here.

## Use and scope: responsibility split

CollaborationTimeline owns:

- **One line's delegations.** Which member a request targets, the request event, its delivery, the turn that serves it, and the result ([collaboration.rs:846-920](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L846-L920)).
- **The line's facts.** `collaboration/request`, `/cancel`, `/close`, `/message`, `/delivered` and `/result`, written on this line's Timeline in dispatch order ([collaboration.rs:972-1080](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L972-L1080)).
- **The line's approval routes.** Which approval ask of this line was forwarded to which delegation, and its retirement ([collaboration.rs:1834-2028](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L1834-L2028)).
- **The publication the Session part reads.** Which delegation this line holds open, its member, its id and its state, published as this line's read-only value.
- **Settlement.** On a rebuild, unfinished requests settle as `failed: reloading` and the member turn that served a delivered input is stopped on the line the delivery fact names; on a close, every request of the line settles as `cancelled: leader closed` and the member's stop is carried out stage by stage ([collaboration.rs:3856-4246](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L3856-L4246), [collaboration.rs:2725-2827](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L2725-L2827)).

Callers keep:

- The tool, its texts and the client methods: [CollaborationSession](CollaborationSession.md).
- Team relations and the address space: [Collaboration](Collaboration.md).
- A member's own turn, input queue and approval: the member line's owners.
- The leader's `close` action decision: the tool; this owner settles it.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The Session part needs to know which delegation a line holds | No: the delegation is folded from this line's facts, and only this owner sees them in order | The published line value |
| The tool's `request` action must record the request and deliver it | No: the request event, its identity and the delivery are one state transition of this line | `request` |
| The tool's `cancel`/`close`/`message` actions act on a delegation | No: the request's identity and suppression rules are this owner's | `cancel`, `close`, `message` |
| A member's approval ask must be forwarded and answered | No: the route's identity and retirement are this owner's | Approval route |
| A rebuild or a close must settle what is unfinished | No: the terminal facts are this line's | Settlement |
| The model reads a member's transcript | Yes: the member line's read by `(session, line)` id | Business composes |

An operation or parameter exists only where the row says No.

## Caller example

The Session part handling a tool call asks the line for its delegation state and then acts:

```rust
let state = line_owner::<CollaborationTimeline>(calling_line)?.state().await?;
// state: {open: [{member, delegationId, requestEvent}], closing}
```

The tool's `request` action after validation:

```rust
let delivered = line_owner::<CollaborationTimeline>(calling_line)?
    .request(Request { member, agent_id, prompt, title, turn_id, request_id, call_id }, cancelled)
    .await?;
// delivered: ok {memberSessionId} or refused {code, detail}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `timeline`, `dispatcher`, `timelineIdentity`, `branchLifecycle` (branch infrastructure) | Write this line's facts, consume them in order, know the line and wait for activation. |
| The member's data faces by `(session, line)` id | Any branch-group data face except registration points and lifecycle services ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1, D2): write the request input, stop the serving turn, withdraw an unqueued input, read the member's published state. |
| The member line's Approval owner by id | Answer an approval ask. |
| `sessions.session`, `sessions.observe` | A member's running state and end ([Agents](Agents.md)). |
| `Collaboration` (process, this owner's part) | Establish a member Session for a request ([Collaboration](Collaboration.md)). |

No member `Branch`, `Session`, `Approval` or `InputQueue` object is held: a member is a `(session, line)` id, and its owners are reached by id.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `state` | This line's delegations as a value | `{}` | `{open: [{member, delegationId, requestEvent, delivered}], closing}` | None | `closed` |
| `request` | Record a request and deliver it to its member | `{member, agentId, prompt, title, turnId, requestId, callId}` | `{memberSessionId}` or a refusal `{code, detail}` | The tool call's cancellation abandons the wait for the delivery receipt; the admitted write settles by its facts | `invalid` (argument), `refused` (the process owner's refusal kind, carried as data), `closed` |
| `cancel`, `close`, `message` | Act on an open delegation | `{member, agentId, content?, mode?, turnId, requestId, callId}` | `{status}` or a refusal | As `request` | `invalid`, `refused`, `closed` |
| Approval route | Forward one ask and answer it | the member's ask identity | `accepted` or `denied` | The route retires when the ask settles; nothing else is cancelled | — |

`request`'s refusal carries the process owner's kind as data (`agent_not_found`, `team_member_not_found`, `member_create_failed`, `busy`, `member_closed`, `interrupted`, `closed`) so the tool can print its protocol line without translating.

### Published value

The line publishes `{open, closing}`: one entry per delegation whose request has no result, each with the member Session id, the delegation id, the request event and the delivery status. The Session part reads the value to answer by member id; the value is a projection of this line's own facts, and this owner is its publisher.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `request` | Validate; establish the member if new; write `collaboration/request`; write the member input; write `collaboration/delivered`; the member's turn serves it; its `collaboration/result` settles | The wait ends `cancelled`; the request settles by its own facts, and a delivery already written stands | The request stays on the line and settles by its facts | A rebuild settles unfinished requests as `failed: reloading` and stops the turn on the line the delivery fact names; the line never falls back to the member's default line | The member Session by Agents; the request and result facts by this owner |
| `cancel`, `close`, `message` | Record the operation, carry out its stop or message, settle | The stop continues on the owner's process task, so it outlives the line | As `cancel` | A closing line settles every request as `cancelled: leader closed` and carries each stop out stage by stage | The member line's input and turn owners; the facts by this owner |
| Approval route | The member's ask becomes a leader-line input; the leader's decision answers the member's Approval | The route retires with the ask | The route retires on the member's end | A rebuild retires routes whose delegation settled | The member line's Approval owner |
| `state` | Read the folded value | None | Nothing to release | A rebuild re-folds from the line's facts | This owner |

### Session access

This owner resolves a member's owners by `(session, line)` id and calls them directly; any branch-group data face is addressable, except registration points and lifecycle services ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1, D2), and the mechanism implements the resolution ([#2093](https://github.com/arcships/dim/pull/2093)). It never receives a member `Branch`, never reads a member's Timeline to re-derive another owner's state, and never addresses the member's default line: the delivery fact names the line the input was written to.

## Exceptions with reasons

- The carry-out of a cancel or an interrupt runs on the process part's task tracker, not on the line's: the stop must outlive the line and the request it was decided for, and the line's teardown must not wait for a member ([collaboration.rs:2488-2502](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L2488-L2502)).
- A rebuild writes terminal facts (`delivered: error closed`, `result: failed reloading`) before the line is visible: the settlement must be durable so a restart does not re-open a request whose line is gone.
- The line reads its own `step/tool-call` and `step/tool-result` facts to answer `result`: the tool result is this line's, and no other owner folds it.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch Capability: it folds its own facts and writes `collaboration/*`. |
| §2 scopes | Branch. The delegation is this line's work; the member's state stays with the member's line. |
| §3 orthogonality | No other owner's state is read or changed; members are addressed by id. |
| §4 rule 1 | An operation writes a request fact; the member's line decides what the input becomes. |
| §4 rule 2 | No hand-off: a delivered request is settled by this line's facts. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The facts are this owner's; core carries no delegation meaning. |
| §4 rule 5 | The rebuild reads the whole committed range before it settles. |
| §4 rule 6 | Only a tool result reaches the model from this owner. |
| §4 rule 7 | The Session part reads a published value; it never reads this line's Timeline. |
| §4 rule 8 | One requirement is raised: `(session, line)` addressing. |
| §4 rule 9 | Requests, deliveries and results are facts later decisions use; the published value is a projection. |
| §7 ids and values | Members are ids; the Session part reads a value. |
| §7 one interface | One branch face; the Session part has no second path into the line. |
| §7 one change | The implementation moves the per-line state out of the Session part into this owner in one change. |
| ADR 002 D1 | Branch scope: the delegation belongs to the line that started it. |
| ADR 002 D2 | No wiring plugin: the Capability is registered by the feature. |
| ADR 002 D3 | Depends on branch infrastructure and the Session's values; nothing depends on this owner except its Session part. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Facts and values are language-neutral. |

## Requirements raised to other owners

1. **`(session, line)` addressing.** [ADR 027](https://github.com/arcships/dim/pull/2029/files) decides the path: any branch-group data face is addressable by id except registration points and lifecycle services, and callers write `line_owner::<T>(line)?.op(..)`; no forwarding facade and no string-routed call. A line-owning part reaches another line's input, turn, Approval and published values that way. Implementation is [#2093](https://github.com/arcships/dim/pull/2093).
2. **A member's end observation** from Agents' `observe`, including the difference between a business close and an unload, so a request settles for the right reason. The operation is in [Agents](Agents.md); no further requirement.

## Supersedes

- `docs/design/contracts/session-access.md` A.1/§6: the `sessions` methods this line used (`input`, `withdraw`, `stop`, `history`) are replaced by [ADR 027](https://github.com/arcships/dim/pull/2029/files) and the line owners' operations.
- `docs/design/plugins/Collaboration.md` §6: the delegation's per-line facts and settlement are owned here; that page keeps the process history and lifetime.

## Binding appendix

The Capability is registered per branch by the feature; it provides its operations on its own type and publishes `{open, closing}` as a read-only branch value.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestInput {
    pub member: String,
    pub agent_id: String,
    pub prompt: String,
    #[serde(default)] pub title: Option<String>,
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenDelegation {
    pub member: String,
    pub delegation_id: String,
    pub request_event: String,
    pub delivered: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationState { pub open: Vec<OpenDelegation>, pub closing: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Delivered { Ok { member_session_id: String }, Refused { code: String, detail: String } }

impl CollaborationTimeline {
    pub fn state(&self) -> DelegationState { /* … */ }
    pub async fn request(&self, input: RequestInput, cancelled: CancelledFn) -> Result<Delivered, LineError> { /* … */ }
}
```
