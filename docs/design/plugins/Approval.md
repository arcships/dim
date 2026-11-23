# Approval

Scope: branch. Issue: #2028.

Approval owns one call's admission and the approval questions it produces: it asks Permission for the policy decision, asks the user when the policy or an advisor requires it, waits for the answer, rechecks the policy before the call starts, and settles every request it left open when the generation ends. This document is the owner's target contract; the current implementation is described in the [Permission plugin page](../../architecture/rust-agent/plugins/Permission.md#approval).

## Use and scope: responsibility split

Approval owns:

- **Admission.** One call's admission: the policy decision, the advisor advice, the user's answer when required, and the final allow or refusal.
- **Pending requests.** The committed approval requests that no answer or cancellation has ended, in request order, as their `approval/request` payloads. A request exists from its commit until the answer, a policy settlement or a cancellation ends it.
- **The answer path.** An answer commits `approval/respond` and settles the request; the owner answers the caller once the branch dispatched the answer.
- **Settlement on behalf of policy and lifecycle.** A policy change rechecks a waiting call; an allow ends the question with `approval/cancelled` and its reason. The generation's close cancels every request it still holds; a reload cancels the historical requests whose execution did not survive.

Approval does not decide the policy (Permission does), run tools (ToolExecutor does), or notify clients: the client notifications are the App Server's projection of `approval/*` facts and its own attach ordering.

Callers keep as business:

- When a tool call is admitted, and what a refusal means for the turn.
- Which questions a client shows and when it retires them.
- The meaning of a "yes" beyond the call: a session grant is the grant owner's record.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Admit one call, waiting for the user when required | No: composing it in the executor would duplicate the recheck rule and hold the pending state across the wait | `admit` |
| Answer a pending request | No: the answer must be committed as a fact and accepted only while the request is pending | `decide` |
| List the pending requests of a line | No: the set is the owner's state | `pending` |
| Recheck the policy when it changes while a question is open | No: the owner subscribed to the published policy value; a caller cannot see every change it must react to | the owner does it; no caller operation |

An operation exists only where its row says No.

## Caller example

ToolExecutor admits one call with the batch token; dropping the call is the caller's cancellation:

```rust
approval.admit(AdmitRequest {
    source: BranchSource::Tool { turn_id, request_id, call_id },
    call: ApprovalCallData { name: call.name.clone(), input: call.input.clone() },
    risk: risk.clone(),
}, cancelled.clone()).await?; // Denied { reason }, Cancelled, Closed
```

A client answers one request; the answer is accepted only while the request is pending:

```rust
approval.decide(DecideRequest {
    approval_id: request.approval_id,
    decision: ApprovalDecision::Allow,
}).await?; // Unknown (answered, cancelled or never pending), Closed
```

Collaboration lists a member line's pending approvals and answers the one it was asked about:

```rust
for request in approval.pending().await?.requests {
    if request.approval_id == target {
        approval.decide(DecideRequest { approval_id: target, decision }).await?;
    }
}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Permission` | One policy decision for a call, with the call's source when it is an admission. |
| `B:Permission`'s published `PermissionState` | Observe a policy change while a question is open; the owner rechecks on a change. |
| `B:Timeline`, `B:Dispatcher` | Commit `approval/request`, `approval/respond` and `approval/cancelled`; recover abandoned requests. |

The policy value is a read-only view Permission publishes (§7); Approval takes it and its change subscription, never Permission's internals.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `admit` | Admit one call, asking the user when required | `{source, call, risk}` | `{}` | The caller drops the call: the pending request is cancelled and the call ends `cancelled` | `denied {reason}`, `cancelled`, `closed` |
| `pending` | The requests no answer or cancellation ended, in request order | `{}` | `{requests}` | None | `closed` |
| `decide` | Answer a pending request | `{approvalId, decision}` | `{}` | None: an accepted answer is committed | `unknown` (not pending: answered, cancelled or never existed), `closed` |

`risk` is the tool owner's declaration for the call, or absent. `source` identifies the caller and is passed to Permission for the admission decision. The reply to `decide` means the branch dispatched the answer, not that the tool started.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `denied` | `admit` | Do not run the call; the result carries the reason. |
| `cancelled` | `admit` | The batch token was dropped or the batch was interrupted; do not run the call. |
| `closed` | any | The generation is closing or reloading; stop. |
| `unknown` | `decide` | The request is no longer pending; retire the prompt and do not retry the answer. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `admit` — allowed without a question | Ask Permission; an allow returns | The token is checked before the call starts | Nothing held | Refused with `closed` | Approval and Permission |
| `admit` — a question is required | Commit `approval/request`, register the pending request, wait for the answer or a policy change; on an allow, recheck the policy and return; on a deny, return the refusal | The pending request is removed and `approval/cancelled` is written; the call ends `cancelled` | The pending request is cancelled with the same terminal | Every pending request of the generation is cancelled; an in-flight wait ends `closed` | Approval: the request, its terminal and the waiter |
| `admit` — policy changed while waiting | Recheck through Permission; an allow without an advisor question ends the question with `approval/cancelled` and its reason; a deny ends it with the refusal; an ask keeps waiting | As above | As above | As above | Approval |
| `decide` | Commit `approval/respond`, answer the waiter once the branch dispatched it; the admission rechecks before the call starts | — | A committed answer stays; the admission may end `closed` | A response to a request of a closed generation is `unknown` | Approval |
| `pending` | One consistent read under the owner's lock | — | — | Answered at the state observed | Approval |

A call that a policy change no longer requires is not restarted; an explicit advisor question stays pending until answered. The owner never fabricates a user answer.

## Exceptions with reasons

- `pending` is kept although the App Server's attach path no longer needs a lock-held replay: the Collaboration route in [runtime-owner-operations.md](../contracts/runtime-owner-operations.md) reads a member line's pending approvals, and one owner operation serves it. The App Server orders its own notifications; the owner promises only a point-in-time set.

## Supersedes

- `docs/design/service-faces.md` §3.4's `approval` row and its pointer to the implementation page.
- `docs/design/contracts/runtime-owner-operations.md`'s Collaboration-approval rows (`approval.pending/decide`).
- The current implementation page (`docs/architecture/rust-agent/plugins/Permission.md#approval`) keeps describing the implementation until this contract lands.

## Verification

Flow tests on the owner:

- An allow without a question returns without a request; an ask writes one request, and the call starts only after the answer and the recheck.
- A dropped admission removes its request and writes `approval/cancelled`; a policy change that allows the call ends it the same way and starts the call; a policy change that denies it returns the refusal.
- An advisor's question stays pending across a policy change; only an answer settles it.
- A second answer to the same request is `unknown`; an answer to a cancelled request is `unknown`.
- The generation's close cancels every pending request; a reload cancels the historical requests and rebuilds an empty pending set.
- The face and the Rust call answer the same requests and errors.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Approval is one branch part and a Capability: it injects `dispatcher` and `timeline` and folds `approval/respond`. |
| §2 scopes | Branch: a call's admission belongs to the line that runs it. |
| §3 orthogonality | Permission's policy and decision are separate owners; Approval coordinates through their operations and the published value. |
| §4 rule 1 | The answer is a command (`approval/respond`); the owner consumes it and writes the outcome. |
| §4 rule 2 | A request handed to a client is not taken back; only the answer or a terminal ends it. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | A terminal and the state it ends are one batch. |
| §4 rule 6 | Approval facts carry no model context; the tool result is the model's only outcome. |
| §4 rule 7 | Permission is asked through its operation; the policy value is a published read-only view. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | Requests and their terminals are facts later decisions use; client notifications stay broadcasts. |
| §7 resources and objects | Pending requests are the owner's state; no waiter handle or guard crosses. |
| §7 no proxy | The owner answers its own requests and returns its own state. |
| §7 one surface | `admit`, `pending`, `decide`; `respond` and `replay` are removed. |
| §7 cross-language | Every parameter and reply is data; cancellation is dropping the call. |
| §7 responsibility split | The caller keeps the turn's use of a refusal; the owner keeps the question, the recheck and the settlement. |
| ADR 002 D1 | Branch scope. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | Depends on Permission (branch) and the branch infrastructure. |
| ADR 002 D4 | No configuration. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **Permission (#2028).** `decide` with the call's source and the published `PermissionState` are this board's own contract; see [Permission](Permission.md).
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `approval`. `BranchSource`, `ApprovalCallData`, `ApprovalDecision` and `ApprovalRequestData` are `dim_agent_types` values.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::{ApprovalCallData, ApprovalDecision, ApprovalRequestData, BranchSource};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmitRequest {
    pub source: BranchSource,
    pub call: ApprovalCallData,
    pub risk: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReply {
    pub requests: Vec<ApprovalRequestData>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideRequest {
    pub approval_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalError {
    #[error("the call was denied: {reason}")]
    Denied { reason: String },
    #[error("the call was cancelled")]
    Cancelled,
    #[error("the approval service is closing")]
    Closed,
    #[error("the approval is absent or already resolved")]
    Unknown,
}

impl PublicService for Approval {}

#[rutis::service(name = "approval")]
impl Approval {
    pub async fn admit(&self, request: AdmitRequest, cancelled: CancelledFn) -> Result<(), ApprovalError> { /* ... */ }
    pub async fn pending(&self) -> Result<PendingReply, ApprovalError> { /* ... */ }
    pub async fn decide(&self, request: DecideRequest) -> Result<(), ApprovalError> { /* ... */ }
}
```
