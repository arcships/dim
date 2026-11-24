# SessionScope

Scope: session. Issue: #2025.

SessionScope is the runtime's Session owner: the creation record and identity, the phase, the activity leases and the failure record that accepted work and callers share, and the Session subtree's content instances. This document is the owner's target contract. The branch tree (#2024) and the Session store (#2018) are separate owners; their calls appear here only as dependencies or requirements.

## Use and scope: responsibility split

SessionScope owns:

- **The creation record.** The Session's mount list, working directory, model selection, system prompt, permission preset, collaboration role, interactive flag, language seed and initial title, fixed when the Session is built. Every member is fixed except `language`, which the native store updates in place; a caller reads the live reply language from `sessionLanguage`, never from this value.
- **Identity.** The Session id and whether the Session was restored from the store.
- **The phase.** Loading, Active, Closing, Closed, monotonic and never replaced by an earlier phase. `activated` and `closing` are the waits on the two transitions.
- **Activity leases.** An accepted piece of work holds a lease so an idle release does not end the Session first. Every lease is bound to the calling row and the owner settles it at the row's unload, so no caller frees anything on an error path. `hold` answers an id for accepted work; the id is bound to the calling row and the owner settles it at the row's unload. A lease reports nothing and cancels nothing.
- **The failure record.** The first fault that stopped the Session; a storage failure supersedes an earlier non-storage fault and the first storage failure wins. `reportFault` writes, `failure` reads, `observeFault` subscribes.
- **Content instances.** Session-owned content mounted from a definition — a Skill's content, an MCP connection — inside this Session's subtree, addressed by instance id. The owner admits only groups whose configured parent is the Session group.
- **The branch's runtime context.** One construction per branch id, handed to the branch tree, which owns and settles the context (see Exceptions with reasons).

Callers keep as business:

- When their background work may start and must stop: BranchLine, Skills, ShellInput and FileAccess wait on the phase.
- Which work counts as accepted: the extent of ShellInput's command, the lifetime of BackgroundExec's task, Report's draft and Collaboration's member.
- What a fault means for their work; the failure record says only that the Session stopped.
- Which definitions to mount and when to retire them.
- Which branches exist, their ids and their ancestors: TimelineTree owns the branch directory (#2024).
- Acting on a line's owners: the mechanism resolves a branch-group service by line id (`line_owner::<T>(line)`), and the caller calls that owner's typed operation. Any branch-group data face is reachable this way except registration points and lifecycle services. The route answers `SessionClosing` when this Session is closing, which ends the part's work, and `NoLine` when the line is not live, which is final for that line. `NoSession` never reaches a Session part ([#2127](https://github.com/arcships/dim/issues/2127)). SessionScope publishes no operation, request or reply for that path.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read the creation record: WorkspaceTools' root and role, ToolRegistry's declaration, Permission's preset, ShellInput's cwd, PromptSources' system prompt and language seed | No: the record is the owner's state and has no other source | `config` |
| Read identity: Attachments' ownership check, Permission and tool-source attribution, the store key | No | `identity` |
| Wait for publication or for close to start: BranchLine's writer, Skills' replacement, ShellInput's refusal, FileAccess' gate | No: polling a value makes every caller write the wait | `activated`, `closing` |
| Read the phase now: Goal's state read, MCP's admission, ShellInput's refusal | No | `phase` |
| Keep accepted work from being ended by an idle release: ShellInput's command, BackgroundExec's task, Report's draft, Collaboration's member; each run outlives the call that started it | No: the idle-release decision is the owner's, and a callback cannot cover work that continues after the call returns | `hold`, settled at the row's unload |
| End a hold early, after the work's last fact, so an idle release may happen sooner | No: the id is bound to the calling row and only the owner releases it | `release` |
| Record a fault: Prompt on a prompt or recovery failure | No | `reportFault` |
| Read the current fault | No | `failure` |
| Subscribe to later faults: a part in another language | No: a subscription is the owner's, not a poll loop in the caller | `observeFault` |
| Act on one line's owners by line id: any branch-group data face — `inputQueue.submit` or `withdraw`, `contextUsage.read`, `prompt.adoptedConfiguration`, `approval.pending` — except registration points and lifecycle services ([ADR 027 D1 and D3](https://github.com/arcships/dim/pull/2029)) | No: a part cannot address a line's instance itself, and a forwarding operation here would be the facade §7 forbids | Not this owner: the runtime routes by line id ([#2127](https://github.com/arcships/dim/issues/2127)), and the caller calls that owner's typed operation |
| Mount and retire Session content: Skills' content instances, MCP's connections | No: the Session's anchor and the group admission are the owner's knowledge | `mount`, `unmount` |
| Open a branch's runtime context: TimelineTree | No: only the runtime constructs a branch's context, in its own subtree | `openBranch` |

An operation exists only where the row says No.

## Caller example

Skills replaces content after publication, and stops when the Session closes:

```rust
if !scope.activated().await.active {
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

ShellInput's run outlives the `shell.run` call: a cancelled call returns at once and the
run keeps its hold until it leaves ShellInput's registry. BackgroundExec and Report keep the
same shape. The id belongs to the part's row; the owner settles it at the row's unload, and
the part releases it when the work ends only so an idle release may happen sooner.

```rust
// ShellInput: the run's hold lives in the part's registry, not in the call.
if !matches!(scope.phase().phase, Phase::Loading | Phase::Active) {
    return Err(RpcError::closed());
}
let lease = scope.hold(ctx).await?.lease_id;
self.runs.lock().unwrap().insert(run_id, lease);
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
// `openBranch` is the one Rust-only method; TimelineTree is the only caller.
let branch = scope
    .open_branch(ctx, OpenBranchRequest { branch_id: id, ancestors }, cancelled)
    .await?;
// The tree owns `branch` from here: it hands it to the branch's parts and closes it
// before the Session scope unloads.
self.nodes.insert(id, branch);
```

## Own contract

### Dependencies

SessionScope injects nothing. The assembly gives the owner its creation record and the Session anchor; the trusted host installs the content definitions the Session may mount. SessionScope no longer publishes the Agent's configuration, the Skills or Collaboration factory texts, or the names of the five host ports (`historyRenderer`, `contextModelHost`, `prepareModelHost`, `prepareContextHost`, `compactionStrategyHost`); those names leave the catalog (delivery).

The Session store constructs the Timeline engine and its Session-scope binding; SessionScope installs that binding in the Session instance and never hands it to a process caller. The operations are the engine's (#2018): the Session face publishes the per-line reads and the completion subscriptions, while `query` and `sessionConfig` stay on the process face `store` because an unloaded Session has no Session instance to host them. One engine, one operation set, one spelling per operation.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `config` | The Session's creation record | `{}` | `SessionConfig` | None; the owner completes the read | None |
| `identity` | Who this Session is | `{}` | `{sessionId, restored}` | None | None |
| `phase` | The phase now | `{}` | `{phase}`: `loading`, `active`, `closing`, `closed` | None | None |
| `activated` | Wait until the Session is published or starts closing | `{}` | `{active}`: true only when published and not closing | The caller drops the call; the owner's close ends the wait with `active: false` | None |
| `closing` | Wait until the Session starts closing | `{}` | `{}` | The caller drops the call | None |
| `hold` | Keep an idle release from ending this Session while accepted work runs | `{}` | `{leaseId}` | None; the call does not wait | `closed` |
| `observeFault` | Receive the current fault and later reports until released or the row unloads | `{listener}` | `{leaseId}` | None; the call does not wait | `closed` |
| `release` | End a lease or a subscription before the row unloads | `{leaseId}` | `{released}`: false when the id was already released | None | `invalid` when the id belongs to another row; `closed` |
| `reportFault` | Record the fault that stopped the Session | `{fault}` | `{}` | None | None |
| `failure` | The fault in force, if any | `{}` | `{fault?}` | None | None |
| `mount` | Create a Session-owned content instance from a definition | `{group, definitionId, instanceData}` | `{instanceId}` | The caller drops the call; an instance it no longer waits for is shut down before the reply | `invalid`, `not_ready`, `cleanup`, `closed` |
| `unmount` | Retire a content instance this Session owns | `{instanceId}` | `{}` | None; the owner completes the release | `unknown` when no live instance has the id; `cleanup`; `closed` |
| `openBranch` (Rust-only, `#[rutis::service(skip)]`) | Construct a branch's runtime context for the branch tree | `{branchId, ancestors}` | the branch's runtime context (see Exceptions) | The caller drops the call; a construction the caller abandoned is shut down before the branch becomes visible | `closed`, `build` |

`listener` is a callback by reference: it runs outside the owner's locks and outside the Timeline's commit lock. A lease id is an owner-issued token bound to the calling row; it carries no authority, the owner settles it at the row's unload, and a caller never has to free it on an error path.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `closed` | `hold`, `observeFault`, `release`, `mount`, `unmount`, `openBranch` | The Session is closing or unloading; stop starting work; a later generation serves the next Session |
| `invalid` | `release`, `mount` | Fix the id or the definition: an id of another row, a group outside this Session's subtree, an unknown definition |
| `unknown` | `unmount` | The id names no live instance of this Session; nothing to retire |
| `not_ready` | `mount` | Act on the reported row and state: the definition's rows did not activate; the instance was shut down and nothing is left to release |
| `cleanup` | `mount`, `unmount` | The instance is unreachable but its shutdown failed; the Session will report it as a fault; treat the content as unavailable |
| `build` | `openBranch` | The branch did not open: `message` says why, `cleanup` is a shutdown that also failed; the tree records the branch as failed and settles it |

`reportFault`, `failure`, `config`, `identity`, `phase`, `activated` and `closing` have no error kinds: nothing in the owner refuses them.

### Flows

| Operation | Normal path | Caller cancels or unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `hold` | The owner checks the phase, increments the hold count and registers the lease under the calling row | The row's unload releases every lease the row still holds | No new lease is issued once an idle release is decided; an existing lease never delays a close | SessionScope |
| `observeFault` | The owner registers a listener under the calling row, calls it once with the fault in force (none included), then on every later report | The row's unload stops the listener and drops the registration | The listener stays until the generation unloads; a report after close is ignored | SessionScope; the listener's work by the calling row |
| `release` | The owner removes the lease or subscription and disposes it | A dropped call leaves the lease in place; the row's unload settles it | An id of an unloaded generation is `closed` | SessionScope |
| `mount` | The owner creates the instance in the Session subtree, waits for its required rows, then answers the id | A dropped call shuts the half-made instance down; a definition that never activates is shut down and answers `not_ready` | New mounts are refused; in-flight mounts settle, and every instance is shut down when the Session scope unloads | SessionScope |
| `unmount` | The owner shuts the instance down, propagates a shutdown failure as `cleanup`, and forgets the id | A dropped call does not cancel the shutdown | The Session's unload shuts down every remaining instance | SessionScope |
| `openBranch` | The owner admits the id, waits for the branch scope and its required parts, and hands the context to the tree | A dropped call settles the half-made branch before it is visible | `closed` once the Session is closing; a Construction in flight settles or ends `closed` | SessionScope creates; TimelineTree holds and closes (Exceptions) |

### A branch's runtime context: the handover

`openBranch` hands one object between two owners. Both cite [lifecycle.md §1](../../architecture/rust-agent/lifecycle.md) for the scope tree, its close rights and the unload order, and [single-tree §Close](../../architecture/rust-agent/single-tree.md#关闭) for what a failed construction settles before its cause is reported.

| Question | Answer |
| --- | --- |
| Who creates | SessionScope, inside the Session subtree, after the branch scope and its required parts are Active and the branch's loop state exists. |
| Who holds | TimelineTree: the context is inserted under the line id and handed to that line's parts; SessionScope keeps no reference after the reply. |
| Handover order | The line is admitted first, in the tree's directory; the construction answers only when the context is runnable; a failed construction settles its cleanup before the reply, and the line never becomes visible. |
| Who closes | TimelineTree, when the line ends and when the tree closes, before the Session scope unloads. |
| TimelineTree closes first | Normal Session close: every line's context closes, then the Session scope unloads. |
| SessionScope closes first | `openBranch` refuses new lines with `closed`; a construction in flight settles or ends `closed`; a context already handed over is closed by the tree in the Session's close. |
| Reload | The Session scope never reloads, so a context never outlives its generation. |

## Exceptions with reasons

- `openBranch` hands out the branch's runtime context: an id would put branch runtime state behind a second set of operations owned by SessionScope, while ADR 002 D1 gives the directory and the close to TimelineTree, and the branch's whole capability machine runs behind that context. Ids, values and callbacks cannot carry it. §7 allows the object only with a lifecycle design both sides cite, and [lifecycle.md §1](../../architecture/rust-agent/lifecycle.md) is that design: the handover table above states who creates, holds and closes it and what either side's close means.
- The Session-scoped `timelineStore` binding is assembly infrastructure: the store owns the engine and the operations, SessionScope installs the binding and states the spelling, and a process caller uses `store` (§2018's requirement).
- `config` is the persisted record's serde form: `interactive` is omitted when true because only the native store writes that form. A caller that decodes the value must treat absence as true; the live reply language is `sessionLanguage`, not this record.
- The failure record keeps only the first fault, and a storage failure supersedes a non-storage one: work that fails after the Session already stopped learns the original cause, not its own.
- A fault listener that returns an error stops receiving: the owner drops the registration and the lease, and the fault record is unchanged.
- `identity` carries no version: the id and the restored flag are fixed at creation.
- A lease is a value, not a handle: it names nothing the caller can use except in `release`, and the owner releases every lease at the row's unload, so a caller that loses the id leaks nothing. `release` only lets an idle release happen sooner.
- Content instance ids are the Session's, not the caller's: the Session's close shuts down every instance it mounted, so a part that never unmounts leaves obsolete content published until the Session ends, without breaking the owner.

## Verification

- `hold` answers `closed` once an idle release is decided; a lease taken before it keeps the Session loaded until released, and never refuses a close; a lost id is settled at the row's unload; another row's id is `invalid` on release.
- `observeFault` delivers the fault in force and later reports; `release` answers `released: true` then `false`; another row's id is `invalid`; the row's unload stops delivery.
- `reportFault` keeps the first fault; a storage fault after a non-storage one wins; a non-storage fault after a storage one loses.
- `mount` answers `invalid` for a group outside the Session's subtree, `not_ready` for a definition whose row failed (and leaves no instance), and shuts down an abandoned mount; `unmount` answers `cleanup` when the shutdown fails and `unknown` for an id this Session never issued.
- `activated` answers false when the Session closes before publishing; `closing` returns when the close starts.
- `openBranch` answers `closed` during the Session's close; a failed construction settles the branch scope before the reply; the tree closes every branch before the Session scope unloads.
- `identity` is fixed; `phase` only moves forward through the four phases; `config` is fixed except `config.language`, which the native store updates in place.

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
| §7 values, ids and objects | The record, identity, phase and fault are values; a lease id and a content instance id are the owner's, bound to the calling row and settled at its unload; the one object is `openBranch`, with the handover design above. |
| §7 no proxy lookup | The owner returns only its own state; the Agent's configuration and the Skills and Collaboration factory texts leave this face. |
| §7 one interface | One name, `sessionScope`, on the owner type; the wire calls every method except `openBranch`, which is marked `#[rutis::service(skip)]`; no facade or wrapper type. |
| §7 cross-language | Every operation is data, a callback by reference or an id, except `openBranch`: it hands TimelineTree the branch's runtime context, which has no serde form, and the tree is a Rust part. That is the only Rust-only method, and it is marked skip. |
| §7 one change | The implementation task deletes the old names and migrates every caller in one change. |
| ADR 002 D1 | Session scope: the record, phase, leases, faults and mounts live and die with the Session. |
| ADR 002 D2 | No wiring plugin: the operations are on the owner type; no adapter exists beside it. |
| ADR 002 D3 | No dependency: SessionScope injects nothing. |
| ADR 002 D4 | The mount list and the record are structural; no business setting is read here. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

- [#2127](https://github.com/arcships/dim/issues/2127), owned by the dim runtime, and [#2024](https://github.com/arcships/dim/issues/2024): route by line id so a Session part resolves a branch-group data face with `line_owner::<T>(line)?` and calls that owner's typed operation, per [ADR 027 D1–D3](https://github.com/arcships/dim/pull/2029). The instance key never leaves the instance, the caller keeps no resolution between calls, and this owner adds no forwarding operation, operation-name routing or reply envelope. TimelineTree supplies the line id and the line's state, never the address.
- The runtime's `Session` facade stops forwarding line operations: `end_branch`, `set_default_branch`, `branch`, `branches`, `directory`, `default_and_directory`, `ended_head`, `sequence` and `batch_end` are TimelineTree's own operations, and the Session access surface must not restate them. History reads of a line go through `timelineTree.read`, never a second `query`.
- [#2018](https://github.com/arcships/dim/issues/2018): the Session store owns the Timeline engine and constructs the Session-scope binding; SessionScope installs it. The Session face publishes the per-line reads and the completion subscriptions; `query` and `sessionConfig` exist only on the process `store`; `createSession`, `releaseSession`, `rollbackSession` and `append` are not on the Session face.

## Supersedes

This contract replaces the Session-scope paragraphs and rows that state these names; the delivery in #2025 carries the migration.

- `docs/design/contracts/runtime-owner-operations.md`: the `sessionLifecycle` rows of "Retained operations" and "New rutis APIs on existing owners", the `sessionLoader` row, the Report rows and the Report paragraph of "Calls from a Session to a selected branch" (now pointers to TimelineTree's `read` and `directory`), and the "Calls from a Session to a selected branch" section — a branch-group data face is resolved by line id, and `timelineTree.call` is removed rather than kept. `line_owner::<T>(line)` reaches any branch-group data face except registration points and lifecycle services ([ADR 027 D1 and D3](https://github.com/arcships/dim/pull/2029); the routing mechanism is [#2127](https://github.com/arcships/dim/issues/2127)).
- `docs/design/service-faces.md`: §3.6's `timelineFactory` row, §3.2's `agentConfig`, `skillsConfig` and `collaborationConfig` rows, and §3.4's `timelineStore` row, which is now a pointer to this contract.
- `docs/design/resource-faces.md` §5: the `timelineStore` rows and the Session-scope sentence, now pointers to this contract; the engine and its process face remain #2018's.
- `docs/design/plugin-contract.md`: the `sessionIdentity`, `sessionLifecycle`, `sessionConfig` and `agentConfig` rows of the named-services table, the `session` row of the scope table, and R2's needed names.

Rule D: no client wire protocol or user-visible behavior changes; every change here is plugin-facing.

## Binding appendix

The service is marked on the owner type under one name; the existing names `sessionConfig`, `sessionIdentity`, `sessionLifecycle`, `sessionLoader` and `timelineFactory` stop existing.

```rust
use dim_agent_runtime::{PublicService, SessionConfig, SessionFault, TimelineContext};
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
    pub phase: Phase,
}

/// The wire form of the four phases; the owner maps its internal phase onto it.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Loading,
    Active,
    Closing,
    Closed,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivatedReply {
    pub active: bool,
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

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SessionScopeError {
    #[error("{message}")]
    Closed { message: String },
    #[error("{message}")]
    Invalid { message: String },
    #[error("{message}")]
    NotReady { message: String },
    #[error("{message}")]
    Cleanup { message: String },
    #[error("{message}")]
    Build {
        message: String,
        cleanup: Option<String>,
    },
    #[error("{message}")]
    Unknown { message: String },
}

impl PublicService for SessionScope {}

#[rutis::service(name = "sessionScope")]
impl SessionScope {
    pub fn config(&self) -> SessionConfig { /* ... */ }
    pub fn identity(&self) -> Identity { /* ... */ }
    pub fn phase(&self) -> PhaseReply { /* ... */ }
    pub async fn activated(&self) -> ActivatedReply { /* ... */ }
    pub async fn closing(&self) { /* ... */ }
    pub async fn hold(&self, caller: &Ctx) -> Result<LeaseReply, SessionScopeError> { /* ... */ }
    pub async fn observe_fault(
        &self,
        caller: &Ctx,
        listener: FaultListener,
    ) -> Result<LeaseReply, SessionScopeError> { /* ... */ }
    pub async fn report_fault(&self, fault: SessionFault) { /* ... */ }
    pub fn failure(&self) -> Option<SessionFault> { /* ... */ }
    pub async fn release(
        &self,
        caller: &Ctx,
        request: LeaseRequest,
    ) -> Result<ReleaseReply, SessionScopeError> { /* ... */ }
    pub async fn mount(&self, definition: MountDefinition) -> Result<MountedInstance, SessionScopeError> { /* ... */ }
    pub async fn unmount(&self, request: UnmountRequest) -> Result<(), SessionScopeError> { /* ... */ }
    /// The one Rust-only method: the branch's runtime context has no serde form,
    /// and TimelineTree, which holds and closes it, is a Rust part.
    #[rutis::service(skip)]
    pub async fn open_branch(
        &self,
        caller: &Ctx,
        request: OpenBranchRequest,
        cancelled: dim_agent_capabilities::host::CancelledFn,
    ) -> Result<Arc<TimelineContext>, SessionScopeError> { /* ... */ }
}
```

`caller: &Ctx` and `cancelled: CancelledFn` are the binding's caller-row and cancellation arguments, not request fields; the dispatch fills them and no caller sends or names a row. `LeaseId`, `InstanceId` and the ancestor pairs cross as strings and numbers. Errors derive `serde` with `#[serde(tag = "code", rename_all = "snake_case")]`, so the wire `code` is the variant name; `build` also carries `cleanup`. `openBranch` has no serde reply and is marked skip.
