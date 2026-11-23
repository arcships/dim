# SessionScope

Scope: session. Issue: #2025.

SessionScope is the runtime's Session owner: the creation record and identity, the phase, the activity leases and the failure record that accepted work and callers share, and the Session subtree's content instances. This document is the owner's target contract. The branch tree (#2024) and the Session store (#2018) are separate owners; their calls appear here only as dependencies or requirements.

## Use and scope: responsibility split

SessionScope owns:

- **The creation record.** The Session's mount list, working directory, model selection, system prompt, permission preset, collaboration role, interactive flag, language seed and initial title, fixed when the Session is built. The record never changes while the Session runs.
- **Identity.** The Session id and whether the Session was restored from the store.
- **The phase.** Loading, Active, Closing, Closed, monotonic and never replaced by an earlier phase. `activated` and `closing` are the waits on the two transitions.
- **Activity leases.** An accepted piece of work holds a lease so an idle release does not end the Session first. Every lease is bound to the calling row and the owner settles it at the row's unload, so no caller frees anything on an error path. `withHold` holds one for the extent of the caller's callback; `hold` answers an id for work that outlives the call. A lease reports nothing and cancels nothing.
- **The failure record.** The first fault that stopped the Session; a storage failure supersedes an earlier non-storage fault and the first storage failure wins. `reportFault` writes, `failure` reads, `observeFault` subscribes.
- **Content instances.** Session-owned content mounted from a definition — a Skill's content, an MCP connection — inside this Session's subtree, addressed by instance id. The owner admits only groups whose configured parent is the Session group.
- **Line-addressed calls.** A Session part acts on one line by line id: it names a branch-scope operation from the catalog and this owner runs it at the instance the runtime built for that line, answering that owner's reply and errors unchanged. This owner adds no operation of its own to the line; the two families that use the call are writing an input to the line's input owner and reading the value a line owner publishes.
- **The branch's runtime context.** One construction per branch id, handed to the branch tree, which owns and settles the context (see Exceptions with reasons).

Callers keep as business:

- When their background work may start and must stop: BranchLine, Skills, ShellInput and FileAccess wait on the phase.
- Which work counts as accepted: the extent of ShellInput's command, the lifetime of BackgroundExec's task, Report's draft and Collaboration's member.
- What a fault means for their work; the failure record says only that the Session stopped.
- Which definitions to mount and when to retire them.
- Which branches exist, their ids and their ancestors: TimelineTree owns the branch directory (#2024).

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read the creation record: WorkspaceTools' root and role, ToolRegistry's declaration, Permission's preset, ShellInput's cwd, PromptSources' system prompt and language seed | No: the record is the owner's state and has no other source | `config` |
| Read identity: Attachments' ownership check, Permission and tool-source attribution, the store key | No | `identity` |
| Wait for publication or for close to start: BranchLine's writer, Skills' replacement, ShellInput's refusal, FileAccess' gate | No: polling a value makes every caller write the wait | `activated`, `closing` |
| Read the phase now: Goal's state read, MCP's admission, ShellInput's refusal | No | `phase` |
| Run accepted work under a hold for the extent of the call: ShellInput's command | No: the idle-release decision is the owner's, and composing it makes the caller hold a counter or a token across every error path | `withHold` |
| Hold across work that outlives the call: BackgroundExec's task, Report's draft, Collaboration's member | No: a callback would have to stay pending for the whole task, and a tool that answers a task id returns before the command ends | `hold`, settled at the row's unload |
| End a hold early, after the work's last fact, so an idle release may happen sooner | No: the id is bound to the calling row and only the owner releases it | `release` |
| Record a fault: Prompt on a prompt or recovery failure | No | `reportFault` |
| Read the current fault | No | `failure` |
| Subscribe to later faults: a part in another language | No: a subscription is the owner's, not a poll loop in the caller | `observeFault` |
| Act on one line's owner by line id: write an input (`inputQueue.submit`), withdraw one (`inputQueue.withdraw`), read usage (`contextUsage.read`), read the adopted configuration (`prompt.adoptedConfiguration`), read pending approvals (`approval.pending`) | No: a part of another language cannot address a line's instance, and making every part carry the loader's instance table would put the mechanism's index in every owner | `lineOwner.call` with the branch-scope operation's catalog name and request; that owner answers with its own reply and errors |
| Mount and retire Session content: Skills' content instances, MCP's connections | No: the Session's anchor and the group admission are the owner's knowledge | `mount`, `unmount` |
| Open a branch's runtime context: TimelineTree | No: only the runtime constructs a branch's context, in its own subtree | `openBranch` |

An operation exists only where the row says No.

## Caller example

Skills replaces content after publication, and stops when the Session closes:

```rust
if !scope.activated().await?.active {
    return Ok(()); // the Session never published
}
let mounted = scope
    .mount(MountDefinition {
        group: CONTENT_GROUP.into(),
        definition_id: skill.id.clone(),
        instance_data: value,
    })
    .await?;
// The instance is the Session's: its close shuts the content down. Retiring it earlier
// keeps an obsolete definition from staying published.
scope.unmount(UnmountRequest { instance_id: mounted.instance_id }).await?;
```

ShellInput refuses after closing starts and holds the Session while a command runs:

```rust
// ShellInput: the hold covers the command; the owner releases it when the call returns,
// and the row's unload releases it if the caller never returns.
scope.phase()?; // closing or closed -> RpcError::closed()
scope
    .with_hold(
        WithHoldRequest { work: Arc::new(move || Box::pin(run_command(cancel.clone()))) },
        cancel.clone(),
    )
    .await?;
```

BackgroundExec's task outlives the call, so the part takes an id; the owner settles it at
the row's unload, and the part releases it when the task ends only so an idle release may
happen sooner:

```rust
// The id belongs to this part's row; a lost id is settled by the owner.
let lease = scope.hold().await?.lease_id;
self.leases.lock().unwrap().insert(task_id, lease);
```

A part in another language subscribes to the fault record:

```ts
// The subscription belongs to this row: its unload releases it, and release ends it earlier.
const { leaseId } = await sessionScope.observeFault(async fault => {
  if (fault?.storage) record(fault.storage)
})
```

TimelineTree opens a branch after admitting it:

```rust
let branch = scope
    .open_branch(OpenBranchRequest { branch_id: id, ancestors }, cancelled)
    .await?;
// The tree owns `branch` from here: it hands it to the branch's parts and closes it
// before the Session scope unloads.
self.nodes.insert(id, branch);
```

## Own contract

### Dependencies

SessionScope injects nothing. The assembly gives the owner its creation record and the Session anchor; the trusted host installs the content definitions the Session may mount. SessionScope no longer provides the Agent's configuration, the Skills or Collaboration factory texts, or the five host ports; those names leave the catalog (delivery).

The Session store constructs the Timeline engine and its Session-scope binding; SessionScope installs that binding in the Session instance and never hands it to a process caller. The operations are the engine's (#2018): the Session face publishes the per-line reads and the completion subscriptions, while `query` and `sessionConfig` stay on the process face `store` because an unloaded Session has no Session instance to host them. One engine, one operation set, one spelling per operation.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `config` | The Session's creation record | `{}` | `SessionConfig` | None; the owner completes the read | None |
| `identity` | Who this Session is | `{}` | `{sessionId, restored}` | None | None |
| `phase` | The phase now | `{}` | `{phase}`: `loading`, `active`, `closing`, `closed` | None | None |
| `lineOwner.call` | Run one catalog branch-scope operation at a line's instance | `{line, operation, request}` | the operation's own reply, unchanged | The caller's cancel is passed to the operation; a dropped call ends the wait | `ended`, `unknown`, `closed`, and the called operation's own errors unchanged |
| `activated` | Wait until the Session is published or starts closing | `{}` | `{active}`: true only when published and not closing | The caller drops the call; the owner's close ends the wait with `active: false` | None |
| `closing` | Wait until the Session starts closing | `{}` | `{}` | The caller drops the call | None |
| `withHold` | Run the caller's work under a lease for the extent of the call | `{work}` | `{}` | The caller's cancel drops the work; the owner releases the lease and answers `cancelled` | `cancelled`, `closed` |
| `hold` | Keep an idle release from ending this Session for work that outlives the call | `{}` | `{leaseId}` | None; the call does not wait | `closed` |
| `observeFault` | Receive the current fault and later reports until released or the row unloads | `{listener}` | `{leaseId}` | None; the call does not wait | `closed` |
| `release` | End a lease or a subscription before the row unloads | `{leaseId}` | `{released}`: false when the id was already released | None | `invalid` when the id belongs to another row; `closed` |
| `reportFault` | Record the fault that stopped the Session | `{fault}` | `{}` | None | None |
| `failure` | The fault in force, if any | `{}` | `{fault?}` | None | None |
| `mount` | Create a Session-owned content instance from a definition | `{group, definitionId, instanceData}` | `{instanceId}` | The caller drops the call; an instance it no longer waits for is shut down before the reply | `invalid`, `not_ready`, `cleanup`, `closed` |
| `unmount` | Retire a content instance this Session owns | `{instanceId}` | `{}` | None; the owner completes the release | `invalid` when the id belongs to another Session; `cleanup`; `closed` |
| `openBranch` | Construct a branch's runtime context for the branch tree | `{branchId, ancestors}` | the branch's runtime context (see Exceptions) | The caller drops the call; a construction the caller abandoned is shut down before the branch becomes visible | `closed`, `build` |

`listener` and `work` are callbacks by reference: they run outside the owner's locks and outside the Timeline's commit lock. `work` returns nothing; its failure and its cleanup stay with the caller. A lease id is an owner-issued token bound to the calling row; it carries no authority, the owner settles it at the row's unload, and a caller never has to free it on an error path.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `closed` | `lineOwner.call`, `withHold`, `hold`, `observeFault`, `release`, `mount`, `unmount`, `openBranch` | The Session is closing or unloading; stop starting work; a later generation serves the next Session |
| `cancelled` | `withHold` | The caller's cancel stopped the work before it finished; the lease is released and nothing else changed |
| `ended` | `lineOwner.call` | The line has ended; stop acting on it and read the directory for the line in force |
| `unknown` | `lineOwner.call` | No live line of this Session has the id; read the directory and act on the right line |
| `invalid` | `release`, `unmount`, `mount` | Fix the id or the definition: an id of another row or Session, a group outside this Session's subtree, an unknown definition |
| `not_ready` | `mount` | Act on the reported row and state: the definition's rows did not activate; the instance was shut down and nothing is left to release |
| `cleanup` | `mount`, `unmount` | The instance is unreachable but its shutdown failed; the Session will report it as a fault; treat the content as unavailable |
| `build` | `openBranch` | The branch did not open: `cause` is why, `cleanup` is a shutdown that also failed; the tree records the branch as failed and settles it |

`reportFault`, `failure`, `config`, `identity`, `phase`, `activated` and `closing` have no error kinds: nothing in the owner refuses them.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `lineOwner.call` | The owner resolves the line to the instance the runtime built, runs the named operation there, and answers that operation's reply and errors | A dropped call ends the wait; the operation's own cancellation applies to its work | A closing Session answers `closed`; a line whose context closed answers `ended` | SessionScope for the addressing; the called owner for the operation |
| `withHold` | The owner takes a lease, calls the work, and releases the lease when the work returns | The dropped call stops the work; the owner releases the lease and answers `cancelled` | An in-flight call ends `closed`; a lease never delays a close | SessionScope; the work's own cleanup by the calling row |
| `hold` | The owner checks the phase, increments the hold count and registers the lease under the calling row | The row's unload releases every lease the row still holds | No new lease is issued once an idle release is decided; an existing lease never delays a close | SessionScope |
| `observeFault` | The owner registers a listener under the calling row, calls it once with the fault in force (none included), then on every later report | The row's unload stops the listener and drops the registration | The listener stays until the generation unloads; a report after close is ignored | SessionScope; the listener's work by the calling row |
| `release` | The owner removes the lease or subscription and disposes it | A dropped call leaves the lease in place; the row's unload settles it | An id of an unloaded generation is `closed` | SessionScope |
| `mount` | The owner creates the instance in the Session subtree, waits for its required rows, then answers the id | A dropped call shuts the half-made instance down; a definition that never activates is shut down and answers `not_ready` | New mounts are refused; in-flight mounts settle, and every instance is shut down when the Session scope unloads | SessionScope |
| `unmount` | The owner shuts the instance down, propagates a shutdown failure as `cleanup`, and forgets the id | A dropped call does not cancel the shutdown | The Session's unload shuts down every remaining instance | SessionScope |
| `openBranch` | The owner admits the id, waits for the branch scope and its required parts, and hands the context to the tree | A dropped call settles the half-made branch before it is visible | `closed` once the Session is closing; a Construction in flight settles or ends `closed` | SessionScope creates; TimelineTree holds and closes (Exceptions) |

## Exceptions with reasons

- `openBranch` hands out the branch's runtime context: an id would put branch runtime state behind a second set of operations owned by SessionScope, while ADR 002 D1 gives the branch's directory and close to TimelineTree, and the branch's whole capability machine runs behind that context. The joint design is [lifecycle.md §1](../../architecture/rust-agent/lifecycle.md): the owner creates the context inside the Session subtree after the branch scope loads; TimelineTree holds it, hands it to the branch's parts and closes it; the Session unloads only after its branches close; a failed construction reports `cause` before `cleanup` and settles both before the branch is visible; a Session scope never reloads, so the context never outlives its generation.
- `lineOwner.call` runs another owner's operation and is not a proxy with business logic: `operation` is a branch-scope catalog name, the reply and the errors are that owner's unchanged, and this owner keeps only the line index the runtime builds when it constructs a context. An instance id cannot be the reply because a part of another language cannot address an instance by id; letting each part hold the index instead would put the loader's table in every owner.
- The Session-scoped `timelineStore` binding is assembly infrastructure: the store owns the engine and the operations, SessionScope installs the binding and states the spelling, and a process caller uses `store` (§2018's requirement).
- `config` is the persisted record's serde form: `interactive` is omitted when true because only the native store writes that form. A caller that decodes the value must treat absence as true; the live reply language is `sessionLanguage`, not this record.
- The failure record keeps only the first fault, and a storage failure supersedes a non-storage one: work that fails after the Session already stopped learns the original cause, not its own.
- `identity` carries no version: the id and the restored flag are fixed at creation.
- `withHold` cannot replace `hold`: a tool that answers a task id returns before the command ends, and a member, a draft or a background task outlives many calls, so a callback kept pending for the whole work would make the tool wait for its own task. Long-running commands are fine with the callback: `withHold` awaits the work, and a client in another language passes the callback by reference while the row stays the settlement owner.
- A lease is a value, not a handle: it names nothing the caller can use except in `release`, and the owner releases every lease at the row's unload, so a caller that loses the id leaks nothing. `release` only lets an idle release happen sooner.
- Content instance ids are the Session's, not the caller's: the Session's close shuts down every instance it mounted, so a part that never unmounts leaves obsolete content published until the Session ends, without breaking the owner.

## Verification

- `withHold` releases its lease when the work returns, and answers `cancelled` with the lease released when the caller cancels; the work's error path needs no release.
- `hold` answers `closed` once an idle release is decided; a lease taken before it keeps the Session loaded until released, and never refuses a close; a lost id is settled at the row's unload.
- `observeFault` delivers the fault in force and later reports; `release` answers `released: true` then `false`; another row's id is `invalid`; the row's unload stops delivery.
- `reportFault` keeps the first fault; a storage fault after a non-storage one wins; a non-storage fault after a storage one loses.
- `mount` answers `invalid` for a group outside the Session's subtree, `not_ready` for a definition whose row failed (and leaves no instance), and shuts down an abandoned mount; `unmount` answers `cleanup` when the shutdown fails and `invalid` for another Session's id.
- `activated` answers false when the Session closes before publishing; `closing` returns when the close starts.
- `openBranch` answers `closed` during the Session's close; a failed construction settles the branch scope before the reply; the tree closes every branch before the Session scope unloads.
- `config`, `identity` and `phase` do not change while the Session runs.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part, the scope itself. It writes no Timeline event and folds no state, so it is not a Capability; it provides the Session's own state and the mounts. |
| §2 scopes | Session. Branch state and the branch directory stay with TimelineTree; the store's operations stay with the Session store. |
| §3 orthogonality | No other owner's state is read or changed. A caller that needs a new operation gets a general one here or composes it. |
| §4 rule 1 | No command event: the owner writes no Timeline fact. |
| §4 rule 2 | No handover of business objects; the phase and the fault record are the owner's own state. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | Core and assembly carry no business meaning: no content format, no prompt, no branch policy. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: content instances are mounted for their own owners; the owner adds no model-visible text. |
| §4 rule 7 | Callers use these operations; no caller reads the hold count, the fault slot or an instance's fiber. |
| §4 rule 8 | No infrastructure change. SessionScope is the runtime's Session part and its shape is the runtime's decision; the operations above are the data face those decisions already require. |
| §4 rule 9 | No Timeline fact: the phase and the fault record are runtime state, read live and never persisted as events. |
| §7 values, ids and objects | The record, identity, phase and fault are values; `withHold` takes the caller's work as a callback; a lease id and a content instance id are the owner's, bound to the calling row and settled at its unload; the one object is `openBranch`, with the joint design above. |
| §7 no proxy lookup | The owner returns only its own state; the Agent's configuration and the Skills and Collaboration factory texts leave this face. |
| §7 one interface | One name, `sessionScope`, on the owner type; the Rust binding and the wire call the same methods; no facade or wrapper type. |
| §7 cross-language | Every operation is data, a callback by reference or an id; no method exists only for Rust. |
| §7 one change | The implementation task deletes the old names and migrates every caller in one change. |
| ADR 002 D1 | Session scope: the record, phase, leases, faults and mounts live and die with the Session. |
| ADR 002 D2 | No wiring plugin: the operations are on the owner type; no adapter exists beside it. |
| ADR 002 D3 | No dependency: SessionScope injects nothing. |
| ADR 002 D4 | The mount list and the record are structural; no business setting is read here. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

- [#2024](https://github.com/arcships/dim/issues/2024): the two contracts agree on `lineOwner.call` and its resolution; nothing further is required from TimelineTree. The line index is built by this owner when it constructs a line's context and dropped when that context closes.
- The runtime's `Session` facade stops forwarding line operations: `end_branch`, `set_default_branch`, `branch`, `branches`, `directory`, `default_and_directory`, `ended_head`, `sequence`, `batch_end` and `query` are TimelineTree's, and the Session access surface must not restate them.
- [#2018](https://github.com/arcships/dim/issues/2018): the Session store owns the Timeline engine and constructs the Session-scope binding; SessionScope installs it. The Session face publishes the per-line reads and the completion subscriptions; `query` and `sessionConfig` stay on `store`; `createSession`, `releaseSession`, `rollbackSession` and `append` are not on the Session face.

## Binding appendix

The service is marked on the owner type under one name; the existing names `sessionConfig`, `sessionIdentity`, `sessionLifecycle`, `sessionLoader` and `timelineFactory` stop existing.

```rust
use dim_agent_runtime::{PublicService, SessionConfig, SessionFault, SessionPhase};
use rutis::Ctx;
use std::sync::Arc;

pub type LeaseId = String;
pub type InstanceId = u64;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub session_id: String,
    pub restored: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseReply {
    pub phase: SessionPhase,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivatedReply {
    pub active: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineOwnerCall {
    pub line: String,
    /// A branch-scope operation's catalog name, for example `inputQueue.submit`.
    pub operation: String,
    pub request: serde_json::Value,
}

/// The called owner's error, unchanged, or the addressing's own refusal.
#[derive(Debug, thiserror::Error)]
pub enum LineOwnerError {
    #[error("the line has ended")]
    Ended,
    #[error("no live line has this id")]
    Unknown,
    #[error("the Session is closing")]
    Closed,
    #[error("the line owner answered an error")]
    Owner(serde_json::Value),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRequest {
    pub lease_id: LeaseId,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseReply {
    pub lease_id: LeaseId,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseReply {
    pub released: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MountDefinition {
    pub group: String,
    pub definition_id: String,
    pub instance_data: serde_json::Value,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountedInstance {
    pub instance_id: InstanceId,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnmountRequest {
    pub instance_id: InstanceId,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenBranchRequest {
    pub branch_id: String,
    pub ancestors: Vec<(String, u64)>,
}

pub type FaultListener =
    Arc<dyn Fn(Option<SessionFault>) -> rutis::host::BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync>;

/// The caller's accepted work; its error and cleanup stay with the caller.
pub type WorkFn =
    Arc<dyn Fn() -> rutis::host::BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync>;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithHoldRequest {
    pub work: WorkFn,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionScopeError {
    #[error("{0}")]
    Closed(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotReady(String),
    #[error("{0}")]
    Cleanup(String),
    #[error("{0}")]
    Build(String),
    #[error("{0}")]
    Unknown(String),
    #[error("{0}")]
    Ended(String),
}

impl PublicService for SessionScope {}

#[rutis::service(name = "sessionScope")]
impl SessionScope {
    pub fn config(&self) -> SessionConfig { /* ... */ }
    pub fn identity(&self) -> Identity { /* ... */ }
    pub fn phase(&self) -> PhaseReply { /* ... */ }
    pub async fn activated(&self) -> ActivatedReply { /* ... */ }
    pub async fn closing(&self) { /* ... */ }
    pub async fn with_hold(
        &self,
        request: WithHoldRequest,
        cancelled: dim_agent_capabilities::host::CancelledFn,
    ) -> Result<(), SessionScopeError> { /* ... */ }
    pub async fn hold(&self) -> Result<LeaseReply, SessionScopeError> { /* ... */ }
    pub async fn observe_fault(&self, listener: FaultListener) -> Result<LeaseReply, SessionScopeError> { /* ... */ }
    pub async fn report_fault(&self, fault: SessionFault) { /* ... */ }
    pub fn failure(&self) -> Option<SessionFault> { /* ... */ }
    pub async fn release(&self, request: LeaseRequest) -> Result<ReleaseReply, SessionScopeError> { /* ... */ }
    pub async fn mount(&self, definition: MountDefinition) -> Result<MountedInstance, SessionScopeError> { /* ... */ }
    pub async fn unmount(&self, request: UnmountRequest) -> Result<(), SessionScopeError> { /* ... */ }
    pub async fn open_branch(
        &self,
        caller: &Ctx,
        request: OpenBranchRequest,
        cancelled: dim_agent_capabilities::host::CancelledFn,
    ) -> Result<Arc<TimelineContext>, SessionScopeError> { /* ... */ }
}
```

```rust
impl PublicService for LineOwner {}

/// The same owner's second name: the Session access surface's line-addressed call.
#[rutis::service(name = "lineOwner")]
impl LineOwner {
    pub async fn call(
        &self,
        request: LineOwnerCall,
        cancelled: dim_agent_capabilities::host::CancelledFn,
    ) -> Result<serde_json::Value, LineOwnerError> { /* ... */ }
}
```

`caller: &Ctx` and `cancelled: CancelledFn` are the binding's caller row and cancellation argument; the dispatch fills them and no caller sends or names a row. `LeaseId`, `InstanceId` and the ancestor pairs cross as strings and numbers.
