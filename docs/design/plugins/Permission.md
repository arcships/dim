# Permission

Scope: branch. Issue: #2028.

Permission owns the branch's permission policy and one call's decision: the preset and tool lists the branch adopted, the version those changes are counted in, the file-access facts a call's target is judged by, the user's command rules, and the branch's tool advisors. This document is the owner's target contract; the current implementation is described in the [Permission plugin page](../../architecture/rust-agent/plugins/Permission.md#permission).

## Use and scope: responsibility split

Permission owns:

- **The policy value.** The preset and the two tool lists, folded from the Session's creation record and the branch's `permission/changed` facts, published as a read-only value with the version of the last local change. A client's update is a versioned write.
- **The decision.** One call's outcome — allow, ask with a reason, or deny with a reason — from the risk the tool owner declares, the policy, the file-access facts of the call's target, the command rules for an `execute_process` call, and the branch's advisors when the decision is an admission.
- **Tool advisors.** A branch part registers an advisor under a name while it runs; an admission asks the advisors registered at that moment, in name order. Advice is never asked on a recheck and never overrides a deny.
- **The decision's inputs' freshness.** The owner reads the facts and the rules at decision time; a policy change while waiting makes the decision start over.

Permission does not ask the user, run tools, write approval facts or decide the model's declarations. Approval owns the question and the answer; ToolExecutor owns execution; ToolRegistry owns declarations.

Callers keep as business:

- What an ask means and when it is worth asking: Approval's admission and its recheck.
- What a deny means for the turn, and the model-facing text of a refusal.
- The user's command rules and their file: the rules owner reads them; Permission applies them.
- The workspace, system, owned-file and grant facts: the facts owner resolves and answers them.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Decide one call | No: the policy, the facts, the rules and the advisors are one decision, and composing them in the caller duplicates the policy's precedence rules | `decide` |
| Decide an admission, asking the branch's advisors | No: only the owner knows the advisors registered at that moment and how their advice combines | `decide` with the call's source |
| Recheck after an answer or a policy change | No: the recheck must see the current policy, not the snapshot the first decision used | `decide` without a source |
| Read the current policy and version | Yes: the published `PermissionState` | Business composition |
| Update the policy | No: the version check and the fact are one write | `update` |
| Register tool advice | No: the registration is bound to the registering part's lifetime and asked by name order | `advise` |

An operation exists only where its row says No.

## Caller example

Approval asks for an admission and, after the answer, for a plain recheck:

```rust
let admission = permission.decide(DecideRequest {
    call: call.clone(),
    risk: risk.cloned(),
    source: Some(source.clone()),
}, cancelled.clone()).await?; // Cancelled, Closed
if admission.outcome == Outcome::Ask && !admission.advisor_question {
    // ask the user; later, after the answer:
    let recheck = permission.decide(DecideRequest { call, risk, source: None }, cancelled).await?;
}
```

Hooks registers advice for the lifetime of its branch part; the registration ends with the part:

```rust
permission.advise(AdviseRequest {
    name: "hooks".into(),
    advise: Arc::new(HookAdvice { branch: self.clone() }),
}).await?; // Duplicate, Closed
```

A client updates the policy with the version it read:

```rust
let next = permission.update(UpdateRequest {
    expected_version: read.version,
    policy: next_policy,
}).await?; // Conflict { current }, Closed
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionScope` (Session, [#2093](https://github.com/arcships/dim/pull/2093)) | The creation record's permission preset and working directory. |
| `S:PermissionRules` (Session, [#2094](https://github.com/arcships/dim/pull/2094)) | `permissionRules.rules` answers the user's command rules at decision time. |
| `S:FileAccessFacts` (Session owner, [#2025](https://github.com/arcships/dim/issues/2025)) | `check({access, mode, path})` resolves the call's target and answers the workspace, system, owned and grant facts. |
| `B:Timeline`, `B:Dispatcher` | Fold `permission/changed`; commit an update. |

The file-access facts operation that resolves the target is not defined yet; it is stated in Requirements.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `decide` | One call's outcome | `{call, risk?, source?}` | `{outcome: allow\|ask\|deny, reason?, advisorQuestion}` | The caller's token stops the facts or advice read; the decision is not made | `cancelled`, `closed` |
| `update` | Versioned policy write | `{expectedVersion, policy}` | `{version}` | None; one commit | `conflict {current}`, `closed` |
| `advise` | Register tool advice for the calling row's lifetime | `{name, advise}` | `{}` | None | `duplicate`, `closed` |
| `PermissionState` (published value) | Read the policy and its version | — | `{version, policy}` | None | — |

`risk` is the tool owner's declaration (`read_filesystem`, `write_filesystem`, `execute_process`, `network`, `session`, or absent). `source` is present exactly for an admission; the owner asks the advisors only then, and `advisorQuestion` says whether an advisor required an explicit answer, so a policy change can end a policy question but never an advisor's.

The policy is the preset and `allowedTools`/`disallowedTools`. A deny from the tool list, a file fact or a command rule wins over an allow; a command allow turns an ask into an allow for a command without shell separators; an absent `permissions` for a file call means the preset's access.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `cancelled` | `decide` | The call's batch was cancelled; do not run the call. |
| `closed` | any | The generation is closing or reloading; stop; a later generation serves. |
| `conflict` | `update` | Re-read the version from `PermissionState` and decide whether to retry the update with the new base. |
| `duplicate` | `advise` | The name is taken; pick another or withdraw the first registration. |

An unreadable fact or rule is not an error: the owner denies the call with its reason, because the caller's next step is the same as for any refusal.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `decide` | Read the policy, resolve the target and read the facts, read the rules for a command, ask the advisors for an admission, combine and answer | The facts or advice read stops; `cancelled`; nothing changes | The decision's future is the caller's; no resource is held | Refused with `closed`; an in-flight decision ends `closed` | Permission; the facts read by the facts owner |
| `update` | Commit `permission/changed` when the expected version matches, answer the new version once dispatched | None | The write is admitted or not; nothing to release | Refused with `closed` | Permission and the Timeline |
| `advise` | Register the callback under the name against the calling row | None | The row's unload withdraws the registration | Every registration of the generation is withdrawn | Permission |

## Exceptions with reasons

- A branch part registers a callback (`advise`): §7 allows a callback by reference; the owner never calls it inside the Timeline's commit lock, and the calling row's unload withdraws it.
- The policy value is published as a read-only view: Approval subscribes to it to recheck a waiting call, and the App Server reads it for the client; neither reads Permission's internals.

## Verification

Flow tests on the owner:

- Presets decide by risk; `allowedTools` and `disallowedTools` refine the preset and a deny wins; an explicit disallow, a deny rule and a file denial are not lifted by an allow.
- A file call asks when its resolved target is outside the workspace; a read or write fact denial refuses the call; a command rule denies under every preset and an allow rule only answers an ask for a simple command.
- An admission asks the advisors in name order; an ask refines an allow; a deny wins over an ask and an allow; a recheck asks no advisor; a registration is withdrawn when its part unloads; a second registration of the same name is `duplicate`.
- An update with a stale version is `conflict` with the current version; a dispatched change is visible to the next decision and survives a reload.
- The published value's version is the last local change; the face and the Rust call answer the same decision.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Permission is one branch part and a Capability: it injects `dispatcher` and `timeline` and folds `permission/changed`. |
| §2 scopes | Branch: a line's policy and decisions; the creation record and the facts are Session state it reads. |
| §3 orthogonality | Approval is a separate owner; the facts, rules and Session record are other owners' operations and values. |
| §4 rule 1 | A policy update is a command event; the owner writes the outcome fact. |
| §4 rule 2 | No handoff. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in core or assembly. |
| §4 rule 5 | A policy change and its version are one batch. |
| §4 rule 6 | No model context input. |
| §4 rule 7 | The facts, rules and record are read through their owners' operations and values; the advisors are the branch's registrations. |
| §4 rule 8 | No infrastructure change; the facts operation is an owner operation. |
| §4 rule 9 | Policy changes are facts later decisions use; the version is theirs. |
| §7 resources and objects | No resource; no object crosses. Advice is a callback by reference. |
| §7 no proxy | The owner answers its own decision and publishes its own state. |
| §7 one surface | `decide`, `update`, `advise`, and the published value; `admit_decision`, `check` and the separate `decide` are removed. |
| §7 cross-language | Every parameter and reply is data; the decision is a value, not an error. |
| §7 responsibility split | The caller keeps the question and the turn; the owner keeps the policy, the facts' use and the advisors. |
| ADR 002 D1 | Branch scope. |
| ADR 002 D2 | No wiring part. |
| ADR 002 D3 | Depends on Session capabilities; nothing depends on Permission's internals. |
| ADR 002 D4 | The preset seed comes from the Session's creation record; no separate configuration. |
| ADR 002 D5 | Language-neutral operations and data. |

## Requirements raised to mechanism owners

- **File access facts ([#2025](https://github.com/arcships/dim/issues/2025)).** The Session owner publishes `check({access, mode, path}, cancelled) → {target, workspace, system, owned, grant, discoveryError?}` ([#2093](https://github.com/arcships/dim/pull/2093)), which resolves the call's path (`~`, cwd, `..`, links) and answers the facts under the effective access; `mode` is the grant mode the call asks for. Permission resolves the facts service at the Session instance; the Session id is not a request field. Errors are `invalid`, `unavailable`, `cancelled` and `closed`; a `discoveryError` is a field, not an error. Permission currently reads the owner's Rust object and a host `home` value; both disappear with this operation.
- **SessionScope ([#2093](https://github.com/arcships/dim/pull/2093)).** `config` is the target's operation; Permission's creation-record read (the preset seed and the working directory) uses it. `identity` is no longer needed: the facts service is Session-bound.
- **Permission rules ([#2017](https://github.com/arcships/dim/issues/2017)).** `permissionRules.rules` ([#2094](https://github.com/arcships/dim/pull/2094)) is the command-rules operation Permission reads; the `commandRules` name is removed.
- **None other.** No Timeline, Dispatcher, loader or runtime change.

## Binding appendix

The service is marked on the owner type; the name stays `permission`.

```rust
use crate::host::CancelledFn;
use dim_agent_runtime::PublicService;
use dim_agent_types::{ApprovalCallData, BranchSource, CommandRule, PermissionPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideRequest {
    pub call: ApprovalCallData,
    pub risk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<BranchSource>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionReply {
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub advisor_question: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    pub expected_version: u64,
    pub policy: PermissionPolicy,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdviseRequest {
    pub name: String,
    pub advise: AdviseFn,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub version: u64,
    pub policy: PermissionPolicy,
}

/// One advisor's answer: none, allow, ask with a reason, or deny with a reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Advice {
    None,
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

pub type AdviseFn = std::sync::Arc<
    dyn Fn(AdviceRequest) -> rutis::host::BoxFuture<'static, Result<Advice, rutis::host::CallError>>
        + Send
        + Sync,
>;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("the decision was cancelled")]
    Cancelled,
    #[error("the permission service is closing")]
    Closed,
    #[error("permission version conflict; current version is {current}")]
    Conflict { current: u64 },
    #[error("an advisor named {name} is already registered")]
    Duplicate { name: String },
}

impl PublicService for Permission {}

#[rutis::service(name = "permission")]
impl Permission {
    pub async fn decide(&self, request: DecideRequest, cancelled: CancelledFn) -> Result<DecisionReply, PermissionError> { /* ... */ }
    pub async fn update(&self, request: UpdateRequest) -> Result<u64, PermissionError> { /* ... */ }
    pub async fn advise(&self, request: AdviseRequest) -> Result<(), PermissionError> { /* ... */ }
}
```
