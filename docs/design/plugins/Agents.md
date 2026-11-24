# Agents

Scope: process. Issue: #2020.

Agents owns the process's Agent instances and the Session instances under them. This document is the owner's target contract; the current implementation is `AgentDirectory`, `AgentIndex`, `AgentHost`, `SessionUse` and the `sessions`/`sessionPermissions` services in `crates/dim-app-server/src/agents.rs` and `agents/`. The catalog entries `agentIndex` and `sessionPermissions` and the `AgentDirectory`/`AgentIndex` split are replaced by one face, `sessions`, marked on `Agents`.

## Use and scope: responsibility split

Agents owns:

- **Agent instances.** The loader row, the runtime `Agent`, its profile and its owner Session. Which Agent is the process default is Agents' choice ([agents.rs:90-101](https://github.com/arcships/dim/blob/5582af4747c42ea6f437f62f10513b9ce17c6afd/crates/dim-app-server/src/agents.rs#L90-L101)).
- **Session instances under those Agents.** Creation from a profile and a request, restoration from stored rows, the idle release, the business close, the delete and the fork. The runtime `Session` facade stays inside Agents; no other owner receives it.
- **Admission.** Each Session's slot: the gate that serializes restore, close and creation, the use count the idle release reads, and the marks that keep a closed Session from loading again ([agents.rs:169-183](https://github.com/arcships/dim/blob/5582af4747c42ea6f437f62f10513b9ce17c6afd/crates/dim-app-server/src/agents.rs#L169-L183)).
- **The protocol binding of a Session.** The version recorded when the Session was created; a Session belongs to the version that created it.
- **A Session's end.** A business close ends it for this process and keeps its rows; an unload keeps it for restore; a delete removes its rows after its close. Only Agents sees the difference ([agents.rs:670-704](https://github.com/arcships/dim/blob/5582af4747c42ea6f437f62f10513b9ce17c6afd/crates/dim-app-server/src/agents.rs#L670-L704)).
- **Agent and member records.** Which Agent record exists, which owner it names, and which member Sessions a leader created. Both are this owner's records, persisted through the storage engine's private transactions; no storage face carries them.
- **Agent record lifetime and the Team cascade.** Deleting a leader ends its members and its owned Agents first; an owned Agent without a member is removed ([agents.rs:1641-1740](https://github.com/arcships/dim/blob/5582af4747c42ea6f437f62f10513b9ce17c6afd/crates/dim-app-server/src/agents.rs#L1641-L1740)).
- **The idle shutdown decision for Sessions.** Whether every Session is quiescent, and the pause that keeps it so until this generation unloads ([agents.rs:738-744](https://github.com/arcships/dim/blob/5582af4747c42ea6f437f62f10513b9ce17c6afd/crates/dim-app-server/src/agents.rs#L738-L744)).

Callers keep:

- Client protocol rules: which Agents the public directory lists, how a version mismatch is reported, what `session.close` and `session.delete` answer.
- Model choice: Provider resolves a model and owns the connection; Agents takes a resolved selection as data, or asks Provider's `resolve` when the caller supplies none.
- Worktrees: ManagedWorktrees owns the directory; Agents asks for creation during a creation and discards on failure.
- Stored rows, titles and creation records: SessionStore owns them; Agents writes one creation through SessionStore's `create` and reads a title through `title`.
- Permission policy: Permission owns the policy value, and a caller reads a line's current policy from Permission by line id. Agents applies `PermissionPolicy::cap`, a method on the value type, to the requested policy and the starter line's published value.
- Narrative, delegation and tool policy: Collaboration.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Rpc creates a client Session: rows, instance, version, policy bound, worktree, title | No: rows, admission, version and worktree rollback are one creation the owner settles | `create_session` |
| Collaboration creates a member Session for an Agent, with the leader's version and cwd | No: the same creation plus the leader link | `create_session` with `leader_session_id` |
| A caller ends a Session and keeps its rows, with no reload in this process | No: the close must stop a restore, close the instance and record that the Session does not load again | `close_session` |
| A caller deletes a Session and refuses one in use | No: the gate, the Team cascade and the in-use answer are the owner's | `delete_session` |
| A caller forks a Session at a cut | No: the cut, the rows and the new instance are the owner's | `fork_session` |
| A caller runs work that spans calls on one Session | No: without a pin an idle release ends the instance between two calls | `acquire`, `release` |
| Provider releases a startup admission when a Session closes but not when it unloads | No: only Agents sees the difference | `observe` |
| A client reads the public Agent directory and who owns an Agent | No: the loaded set and the ownership rule are the owner's | `agents` |
| Collaboration creates and closes a member Agent | No: the instance and its record are the owner's | `create_agent`, `close_agent` |
| Rpc decides an idle shutdown, keeps Sessions quiescent until exit, then settles the process | No: the lease counts, the pause, the unload and the settlement are the owner's | `quiesce`, `settle` |
| Rpc authorizes an id, renders a summary and answers `daemon.ping` | No: version, closed, running and the loaded set are admission state | `session`, `sessions`, `instance`, `defaults` |
| A caller needs the host's policy for a new Session | No: it is a creation default this owner holds | `permissionDefaults` |
| A caller reads a line's current permission state | Yes: Permission publishes the value; the caller reads it by line id | Business composes over the line owner |
| Collaboration rebuilds Teams from the member links | No: the links are this owner's records | `members` |
| EventHub removes a deleted Session's subscriptions, loaded or not | No: only the owner knows a delete removed the rows | the `agent/session-deleted` notice |
| Scheduler and Workflow deliver an input, verify it landed and stop a run | No: the write and its receipt belong to the line's input owner | The line's input owner by `(session, line)` id (§ Session access) |
| A client lists a Session's preview and title | Yes: `sessions` and `session` give the admission values; SessionStore's `catalog` and `query` give rows and previews | Business composes |
| A caller keeps the startup connection of a Session it creates | Yes: Provider's `prepare_startup` before, `retain_startup` after, `release_startup` on failure; a failed retention deletes the Session it created | Business composes |

An operation or parameter exists only where the row says No.

## Caller example

Rpc creating a client Session from a request naming an Agent, a cwd and a policy:

```rust
let policy = request.policy.unwrap_or_else(|| defaults.permission_policy.clone()); // business default
let created = agents.create_session(
    CreateSession {
        agent_id: request.agent_id,          // none: the default Agent
        cwd: request.cwd,
        policy,
        starter: request.starter,             // the owner caps against the starter's published policy
        model: None,                          // the owner asks Provider for the profile's model
        title: request.title,
        origin: SessionOrigin::Client,
        leader_session_id: None,
        protocol_version: Some(connection.version()),
        worktree: request.worktree,
    },
    cancelled,
).await?;
// created: {sessionId, policy, model, worktree?}
```

Collaboration creating a member from a temporary Agent profile:

```rust
let agent = collaboration.create_agent(CreateAgent { profile, owner_session_id: Some(leader.clone()) }, cancelled).await?;
let member = collaboration.create_session(
    CreateSession {
        agent_id: agent.agent_id,
        cwd: leader_cwd,
        policy: sampled_policy,
        starter: None,
        model: inherited_model,
        title: None,
        origin: SessionOrigin::Internal,
        leader_session_id: Some(leader),
        protocol_version: None,               // the leader's recorded version
        worktree: None,
    },
    cancelled,
).await?;
```

Provider observing a Session's end for a retained startup admission:

```rust
let watched = directory.observe(
    caller,
    ObserveRequest { session_id: session.id().to_string(), generation: Some(generation) },
    Arc::new(move |end| Box::pin(async move {
        // end.reason: Closed releases the admission; Unloaded keeps it for restore.
        provider.on_session_end(end);
        Ok(())
    })),
)?;
```

A caller reading a line's current policy (Rpc's `session.permission.get`):

```rust
let state = line_owner::<Permission>(line)?.published()?;
// state: PermissionState; the caller maps its own protocol reply.
```

Rpc deciding an idle shutdown:

```rust
match agents.quiesce(caller)? {
    Quiesce::Paused => control.begin_shutdown(),
    Quiesce::Busy { running } => return Err(RpcError::daemon_busy(running)),
}
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| The storage engine's private transactions | This owner's Agent and member records: write, delete and the startup sweep of owned Agents. The engine executes the transaction; it answers no business read or write for these rows on any face. |
| `store.create`, `delete`, `fork`, `title`, `hasRecords`, `creationRecord` | Session rows, titles, creation records and the row write of a creation, delete or fork. |
| `managedWorktrees.create`, `discard` | The worktree a creation asks for, and its rollback ([ManagedWorktrees](https://github.com/arcships/dim/pull/2091/files)). |
| `providerService.resolve` | The model a Session starts with when the caller supplies none ([Provider](https://github.com/arcships/dim/pull/2096/files)). |
| `timelineStore` (Session instance, bound by the assembly) | The stored Session's timeline rows behind a creation, a restore and a close. |
| `processActivity` | The process admission hold a creation, a restore and an Agent load take. |

Agents holds no Session-scope or branch-scope service. The `Session` facade, the `Agent` facade, loader instances and admission slots never leave the owner.

### Operations

Face `sessions`, marked on `Agents`. Every operation answers one reply object; an operation that takes a request takes one request object. The value reads `agents`, `sessions`, `defaults`, `instance` and `permissionDefaults` take none.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `agents` | The Agent directory as a value | — | `{agents: [{agentId, default, profile, ownerSessionId?, unavailable?}]}` | None | — |
| `create_agent` | Create an Agent instance and its record | `{profile, ownerSessionId?}` | `{agentId}` | Before the instance is published; a cancelled creation leaves no instance and no record | `invalid`, `storage`, `cancelled`, `closed` |
| `close_agent` | Close one Agent and settle its Sessions | `{agentId}` | `{}` | Not cancelled once admitted | `unknown`, `default`, `closed` |
| `create_session` | Create a Session for an Agent | `{agentId?, cwd, policy, starter?, model?, title?, origin, leaderSessionId?, protocolVersion?, worktree?}` | `{sessionId, policy, model, worktree?}` | Before the rows commit; a cancelled or failed creation removes the rows it wrote and discards its worktree | `invalid`, `unknown_agent`, `unavailable`, `starter`, `model`, `worktree`, `storage`, `create`, `cancelled`, `closed` |
| `close_session` | End a Session for this process, keeping its rows | `{sessionId}` | `{}` | Not cancelled once admitted | `closed` |
| `delete_session` | Close and remove a Session, its members and its owned Agents | `{sessionId, requireInactive}` | `{removed: true}` or `{removed: false, inUse: reason}` | Not cancelled once admitted | `storage`, `closed` |
| `fork_session` | Copy a Session at a cut | `{sourceId, at}` | `{sessionId, sequence}` | Before the copy commits | `invalid`, `storage`, `cancelled`, `closed` |
| `acquire` | Pin a loaded Session for the calling row | `{sessionId, agentId?}` | `{leaseId, generation}` | None; admission only | `unknown`, `session_closed`, `mismatch`, `unavailable`, `closed` |
| `release` | Release a pin before the row unloads | `{leaseId}` | `{released}` | None | `invalid`, `closed` |
| `observe` | One end notification for one Session generation | `{sessionId, generation?}` + `onEnd` | `{agentId, generation}` or `null` | None; the observation ends with the row, the end or a new generation | `unknown`, `generation`, `closed` |
| `session` | One Session's admission value | `{sessionId}` | `{agentId?, generation?, version?, state, running, stored, unavailable?}`; `state` is `loaded`, `closed`, `deleted` or `unknown`; an id this process never saw answers `unknown` | None | `closed` |
| `sessions` | The loaded Sessions' value | — | `{sessions: [{sessionId, agentId, generation, closed, running}]}` | None | — |
| `defaults` | The process Session defaults | — | `SessionConfig` | None | — |
| `instance` | The protocol instance of this generation | — | `{instanceId}` | None | — |
| `members` | The member links this owner recorded | `{leaderSessionId?}` | `[{memberSessionId, leaderSessionId, memberAgentId}]` | None | `closed` |
| `permissionDefaults` | The host's policy for a new Session | — | `PermissionPolicy` | None | — |
| `quiesce` | Pause every Session until this generation unloads | `{}` | `{paused: true}` or `{paused: false, running}` | None | `closed` |
| `settle` | Close every Agent and Session for process shutdown and answer when settled | `{}` | `{}` | Not cancelled | `closed` |

`create_session.starter` is a `{sessionId, branchId}` pair: the owner reads that line's published `PermissionState` and caps. When the starter's value cannot be read the call answers `starter` and creates nothing; the caller retries when the line is available or asks for a wider bound explicitly.

`create_session.model` is a resolved `ModelSelection`. Absent, the owner asks Provider's `resolve` for the Agent profile's model and cwd. The reply carries the policy and model the Session was created with, so a caller records the authority a run got without a second read.

`delete_session` answers an in-use Session as a normal reply, not an error: `{removed: false, inUse: reason}`, which Rpc maps back to its `session_active` error; a delete that removed the Session answers `{removed: true}`.

`observe` is one shot per `(sessionId, generation)`. The callback runs on the owner's task, never inside a Timeline commit lock; a second registration for the same generation answers the same reply and replaces nothing. A generation mismatch answers `generation`, so a caller that kept an old id cannot attach to a newer instance.

`quiesce` pauses every loaded Session after checking that none has accepted work; the pause lives in Agents until this generation unloads, so no guard crosses to the shutdown owner. A Session that accepts work between the check and the pause answers `running`. `settle` is the shutdown itself: it closes every Agent and Session, settles their accepted work and answers once they have ended; a process shutdown calls it after `quiesce`.

`permissionDefaults` reads the host's creation default without creating or loading a Session; it moves here from `sessionPermissions` per [process owner operations](../contracts/process-owner-operations.md#new-rutis-bindings). A line's current policy is Permission's published value, read by line id; this face does not return it. `members` is this owner's record read: a member Session, its leader and its Agent, used by Collaboration to rebuild Teams and by the protocol's client rules.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: an unknown profile shape, a relative or missing cwd, a policy with an empty tool name, a cut outside the source's batches. |
| `unknown_agent` | Read `agents`; the id is gone, or it never existed. |
| `starter` | The requested bound could not be read from the starter line; retry when that line is available, or create without a starter. |
| `model` | Provider could not resolve a model for the profile; act on Provider's own kind. |
| `worktree` | The worktree was not created; the creation is unchanged and the caller reports the worktree failure. |
| `storage` | A row write or read failed; the creation or delete did not complete and the caller reports it; a retry is safe. |
| `create` | The Session instance failed to build after its rows were written; the rows are removed and the caller reports the failure. |
| `mismatch` | The id names a Session of another Agent; fix the caller's own record. |
| `unknown` | No loaded Session has the id; read `session` for its state. |
| `session_closed` | The Session was closed in this process; it will not load again until a restart. `acquire` answers this for a Session that is closing or already closed. |
| `generation` | The generation the caller kept is not the loaded one; read `session` and observe again. |
| `unavailable` | The Session is unusable, or the requested Agent exists but cannot run now (its reason is in the message). Report it and stop retrying until the named reason changes. |
| `default` | The default Agent cannot be closed. |
| `cancelled` | The caller stopped the call before the rows committed; nothing was created. |
| `closed` | This generation is unloading; the next generation serves the call. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `create_session` | Take the process hold, resolve the model, cap the policy, create the worktree, write the rows through SessionStore, build the instance, record the version, answer the ids | Before the rows commit: rows removed, worktree discarded, nothing published | The creation runs as the owner's admitted work and reaches its end; the reply may be lost, and the caller finds the Session by the id it chose or by `sessions` | Admission stops; an admitted creation finishes or is rolled back; a reloaded directory logic serves the next call | Rows by SessionStore; worktree by ManagedWorktrees; instance, slot and version by Agents |
| `close_session` | Mark the slot closing, wait for a restore at the gate, close the instance, record that the Session does not load again | Not cancelled | The close runs as the owner's task; the reply may be lost and the Session is closed all the same | A closing generation refuses new calls; the admitted close finishes | Agents |
| `delete_session` | Take the gate, delete members, close and delete owned Agents, close the Session, remove its rows, announce | Not cancelled | The admitted delete completes; the reply may be lost | Admission stops; the admitted delete finishes | Rows by SessionStore, files by SessionFiles, instances by Agents. A delete that stops midway leaves the leader present with part of its Team gone, and no rows of a member it did not finish; a retry finishes it, and deleting an already removed member or Agent succeeds |
| `fork_session` | Validate the cut, copy rows through SessionStore, build the target instance, announce | Before the copy commits: no target rows | The admitted copy completes; the reply may be lost | Admission stops; the admitted fork finishes | Rows by SessionStore, instance by Agents |
| `acquire`, `release` | Increment the use count and answer the generation; release decrements | Nothing to release on a dropped reply; the row's unload releases every use it holds | The row's uses are released | A use of an unloaded generation answers `closed`; a reloaded Agents keeps no use | Agents |
| `observe` | Register the observation under the calling row; deliver once on close or unload | The observation stays until the row unloads | The row's observations are dropped | Pending observations of a closing generation are dropped; a restored Session is a new generation | Agents |
| `quiesce` | Check running work, pause every loaded Session, answer `paused` | None | Not applicable | The pause ends with this generation | Agents |
| `permissionDefaults`, `members` | Read the creation default or the recorded member links, without loading a Session | None | Nothing to release | Pending reads end `closed` | Agents |
| `settle` | Stop admission, close every Agent and Session, wait for their accepted work, answer | Not cancelled | Not applicable | This generation ends after settlement | Agents |

Every resource is settled by the owner: rows and titles by SessionStore, worktrees by ManagedWorktrees, instances, slots and uses by Agents. A caller's only release is `release` for its own pins and its row's unload.

### Session directory calls and their targets

The calls `sessions` serves today and where each goes; a row that names a line owner is satisfied by the Session access surface below, not by Agents.

| Today's call | Target |
| --- | --- |
| `sessions.launch` | `create_session` |
| `sessions.input` | [InputQueue](https://github.com/arcships/dim/pull/2102/files)'s submit: `{input} → {inputId, sequence}` by `(session, line)` id |
| `sessions.lookup` | [InputQueue](https://github.com/arcships/dim/pull/2102/files)'s submit re-sent with the caller's `submissionId`: a duplicate answers the existing `{inputId, sequence}` (requirement below); no separate lookup exists |
| `sessions.stop` | [Loop](https://github.com/arcships/dim/pull/2102/files)'s `stop_turn({turnId, reason})` by `(session, line)` id, after reading its published running state for the current turn id; a turn that already ended writes nothing |
| `sessions.withdraw` | [InputQueue](https://github.com/arcships/dim/pull/2102/files)'s `withdraw({inputId, reason})` by `(session, line)` id |
| `sessions.pending` | [InputQueue](https://github.com/arcships/dim/pull/2102/files)'s `pending({inputId, publishedCutoff?}) → {pending}` by `(session, line)` id; the owner's published head applies when the caller names no `publishedCutoff` |
| `sessions.history` | [TimelineTree](https://github.com/arcships/dim/pull/2092/files)'s `read(line, from, through, limit)` by `(session, line)` id |
| `sessions.defaultBranchId` | [TimelineTree](https://github.com/arcships/dim/pull/2092/files)'s `directory` for the Session; the default line is in the reply |
| `sessions.exists` | `session.state`: `deleted` and `unknown` answer false, `closed` and `loaded` answer true |
| `sessions.delete` | `delete_session` |
| `sessions.idle` | Deleted: no product caller; a run ends with the stop operation and its own records |

### Execution notices

Three process events stay, because their consumers see state `observe` cannot report: `agent/session-restored {agentId, sessionId, generation}` when a Session is restored for this process, `agent/unloaded-session-deleted {agentId, sessionId}` when a delete removed an unloaded Session's rows, and `agent/session-deleted {agentId, sessionId}` after any other delete removed the rows. Provider reattaches a retained startup connection on the restore event and releases it on the unloaded-delete event; Collaboration re-arms its member watch on the restore event and ends the relation on the unloaded-delete event; EventHub removes the Session's subscriptions on either delete event. They are rutis process events, not Timeline facts, and carry no state a caller folds (`observe` remains the per-generation end notification for callers that need the close/unload distinction).

### Session access

No `Session`, `Agent` or lease object crosses this face. A caller that must run an operation on a Session's own owners resolves the target's data face by id and calls that owner's operation: `sessionId` for any Session-group data face, `(sessionId, line)` for any branch-group data face — not only input writes and published-value reads; registration points and lifecycle services are excluded ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1, D3). `acquire`'s lease keeps the addressed generation loaded for the caller's row; the lease decides loading and grants no access ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D5). Rpc's Session methods, Provider's adoption read, Scheduler's pending and default-line reads, Workflow's history read, EventHub's input, receipt and delete cleanup all use this path. A Session-scope owner reached across Sessions (the leader's `collaborationSession` value from a member) is reached by `sessionId` the same way.

## Exceptions with reasons

- `observe` takes a callback: the end of a Session is an event with no value to poll, the callback is bound to the calling row, and it is never called inside a commit lock.
- The protocol instance id is a process-lifetime value that survives a reload of the directory logic: the face is served by a stable row while creation and restore logic reloads behind it (this contract). This keeps consumers from reloading with storage; it is not a second face.
- Agents reads the starter line's published `PermissionState` to cap a creation: the value belongs to Permission and never leaves Agents except as the capped policy the Session was created with.
- `close_session` keeps the rows: a business close is not a delete, and the design decision is [session deletion and fork §16 D5](../appserver/session-deletion-and-fork.md#16-design-changes-in-implementation).

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Agents is one process owner. It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | Process. Agent and Session instances, admission slots and versions outlive Sessions and are shared across them. Branch and Session state stay with their owners. |
| §3 orthogonality | No other owner's state is read or changed. Stored rows, worktrees, models and policies are asked through their owners' operations. |
| §4 rule 1 | No command event: a close, a delete and a creation write through SessionStore and the Session's own owners; Agents writes no Timeline fact. |
| §4 rule 2 | No hand-off: a Session facade never leaves Agents. |
| §4 rule 3 | No before callback: `observe`'s callback is a notification, not a commit-lock transform. |
| §4 rule 4 | Core and assembly carry no business meaning; no protocol rule, model choice or permission decision appears in this contract. |
| §4 rule 5 | No batches. |
| §4 rule 6 | Context is unaffected: Agents returns no model-facing text. |
| §4 rule 7 | Callers use these operations; no caller reads a slot, a generation list or the facade. |
| §4 rule 8 | One requirement is raised: addressing a Session's owners by `(session, line)` id. |
| §4 rule 9 | No Timeline fact. Version, closed and running are process state and stay values; the delete notice is a process event, not a fact. |
| §7 ids and values | Ids, values and one row-scoped callback cross; no object or handle does. |
| §7 one interface | One face, `sessions`; `agentIndex`, `sessionPermissions` and the `AgentDirectory`/`AgentIndex` split are deleted. Agent and member records are this owner's, and the storage engine carries no face for them. |
| §7 one change | The implementation task deletes the old surface, adds these operations and migrates every caller in one change. |
| ADR 002 D1 | Process scope: Agents and their Sessions span Sessions and outlive one Session's lifetime. |
| ADR 002 D2 | No wiring plugin: the face is on the owner type; the stable row serving it is an assembly arrangement, not a second owner. |
| ADR 002 D3 | Dependencies point from extensions to the core; Agents depends on storage, worktrees and models, and is a process service they may depend on. |
| ADR 002 D4 | No configuration member is added: defaults, titles and policies arrive as request data or from their owners. |
| ADR 002 D5 | The contract is language-neutral: ids, values, one callback and error kinds; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

1. **Route by id; the instance key never goes to the caller.** The mechanism must route a call to the Session group's data faces by `sessionId` and to the branch group's data faces by `(sessionId, line)`, so a caller reaches the owner's own operation without a facade, a forwarding object or an instance key ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D2, D4; [#2127](https://github.com/arcships/dim/issues/2127)). Registration points (`sessionMethods`, `toolRegistry`) and lifecycle services are excluded (D3). The path must be available through rutis sessions to language runtimes and leaves, as other data faces are (D3); clients keep using Rpc. This is the same requirement TimelineTree records ([TimelineTree](https://github.com/arcships/dim/pull/2092/files)); Rpc, Provider, Collaboration, Scheduler, Workflow and EventHub depend on it.
2. **The input owner answers a duplicate submission with the existing receipt, and Loop's stop takes a turn id.** A caller that lost a reply verifies a write by re-sending the same `submissionId`; `inputQueue.submit` must answer the existing `{inputId, sequence}` instead of a bare duplicate error, so no separate lookup exists on any face. `inputQueue.pending` must answer `{pending}` for `{inputId, publishedCutoff?}`: the input id, and the owner's published head when the caller names no `publishedCutoff`. Loop's published running state must carry the current turn id and `stop_turn` must take it. Raised with [#2027](https://github.com/arcships/dim/issues/2027).
3. **SessionStore's `query` answers for a loaded Session.** Rpc's `session.summaries` reads a preview and activity for loaded Sessions; SessionStore's contract describes `query` as a read for an unloaded Session. The data owner is the store, so the store's operation must serve both with one cutoff. Raised with [#2018](https://github.com/arcships/dim/issues/2018).

## Supersedes

- `docs/design/appserver/sessions-service.md`: the `sessions` service, its methods and its decisions D1–D14; the page keeps only the lifecycle, submission-identity and authority facts.
- `docs/design/contracts/session-access.md`: the `sessions` method table (A.1), its errors (A.6), the `sessions` rows of §1 and §6, and the Rust binding (Appendix E).
- `docs/design/service-faces.md` §3.3: the `agentIndex` → `sessions` migration row is replaced by this contract and ADR 027.
- `docs/design/contracts/resource-owner-operations.md` §Persistent Agents: the Agent records move to this owner; the storage engine carries no face for them.
- `docs/design/appserver/session-title-metadata.md` and `docs/design/plugins/SessionTitle.md`: the `sessions.launch` title path becomes `create_session`'s `title`.
- `docs/design/resource-faces.md` §"repository steps": the `sessions.delete` name becomes `delete_session`.
- `docs/design/contracts/process-owner-operations.md`: the `sessions.permission` row and the `sessions` rows naming `useId`/`observeCurrent`/`startupSources` are replaced by this contract and ADR 027.
- `docs/design/service-faces.md` §3.1: the `collaborationDirectory.relations` example is removed with that face.
- Follow-ups recorded, not changed here because their owners rewrite them: `docs/design/plugins/Scheduler.md` (its `sessions.launch/input/stop` rows move with [#2097](https://github.com/arcships/dim/pull/2097/files)), `docs/design/plugins/Workflow.md` (same, with its own issue), `docs/design/worktree-management.md` §3 and `docs/design/p6-p7-bindings.md` (their `sessions.launch` references).

## Client-visible changes

None. The client protocol, its error codes and the `agent` tool's texts and results are unchanged; the removed `sessions` methods are plugin-facing only.

## Binding appendix

Face name `sessions`, marked on `Agents`. `Agents` replaces `AgentIndex`, `AgentDirectory`, `AgentHost` and `SessionUse`; the `Sessions`/`SessionPermissions` service types are removed. Every operation that binds to the calling row — `create_session`, `close_session`, `delete_session`, `fork_session`, `acquire`, `release`, `observe`, `quiesce`, `settle` — takes `caller: &Ctx`; the dispatch fills it and no caller sends or names a row. `CancelledFn` is the binding's cancellation argument, not a request field. An admitted write returns a future that only awaits the owner's task: dropping it detaches the reply, never the write.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_runtime::{PublicService, SessionConfig};
use dim_agent_types::{AgentProfile, PermissionPolicy, ModelSelection};
use rutis::Ctx;
use rutis::host::BoxFuture;
use std::future::Future;
use std::sync::Arc;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEntry {
    pub agent_id: String,
    pub default: bool,
    pub profile: AgentProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentsReply { pub agents: Vec<AgentEntry> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAgent {
    pub profile: AgentProfile,
    #[serde(default)]
    pub owner_session_id: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCreated { pub agent_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRef { pub agent_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Starter { pub session_id: String, pub branch_id: String }

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSession {
    #[serde(default)] pub agent_id: Option<String>,
    pub cwd: String,
    pub policy: PermissionPolicy,
    #[serde(default)] pub starter: Option<Starter>,
    #[serde(default)] pub model: Option<ModelSelection>,
    #[serde(default)] pub title: Option<String>,
    pub origin: crate::protocol::SessionOrigin,
    #[serde(default)] pub leader_session_id: Option<String>,
    #[serde(default)] pub protocol_version: Option<u32>,
    #[serde(default)] pub worktree: Option<crate::protocol::CreateWorktree>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatedSession {
    pub session_id: String,
    pub policy: PermissionPolicy,
    pub model: ModelSelection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<serde_json::Value>, // the worktree owner's reply, carried as data
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRef { pub session_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSession { pub session_id: String, pub require_inactive: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteReply { pub removed: bool, #[serde(skip_serializing_if = "Option::is_none")] pub in_use: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkSession { pub source_id: String, pub at: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Forked { pub session_id: String, pub sequence: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseRequest { pub session_id: String, #[serde(default)] pub agent_id: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseReply { pub lease_id: String, pub generation: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseRequest { pub lease_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseReply { pub released: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObserveRequest { pub session_id: String, #[serde(default)] pub generation: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observed { pub agent_id: String, pub generation: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndNotice {
    pub session_id: String,
    pub agent_id: String,
    pub generation: String,
    pub reason: SessionEnd,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionEnd {
    /// A business close: the Session is ended for this process and will not load again.
    Closed,
    /// An idle release: a later call restores the Session as a new generation.
    Unloaded,
}

pub type EndListener =
    Arc<dyn Fn(EndNotice) -> BoxFuture<'static, Result<(), rutis::Error>> + Send + Sync>;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionLifecycle { Loaded, Closed, Deleted, Unknown }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionState {
    #[serde(skip_serializing_if = "Option::is_none")] pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub version: Option<u32>,
    pub state: SessionLifecycle,
    pub running: bool,
    pub stored: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub unavailable: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionsReply { pub sessions: Vec<LoadedSession> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadedSession {
    pub session_id: String,
    pub agent_id: String,
    pub generation: String,
    pub closed: bool,
    pub running: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembersRequest { #[serde(default)] pub leader_session_id: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberLink {
    pub member_session_id: String,
    pub leader_session_id: String,
    pub member_agent_id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembersReply { pub members: Vec<MemberLink> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceReply { pub instance_id: String }

fn is_zero(value: &u32) -> bool { *value == 0 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuiesceReply {
    pub paused: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub running: u32,
}

/// Kinds by what the caller can do next; `code` is the wire name in the error table above.
#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AgentsError {
    #[error("{message}")] Invalid { message: String },
    #[error("{message}")] UnknownAgent { message: String },
    #[error("{message}")] Starter { message: String },
    #[error("{message}")] Model { message: String },
    #[error("{message}")] Worktree { message: String },
    #[error("{message}")] Storage { message: String },
    #[error("{message}")] Create { message: String },
    #[error("{message}")] Mismatch { message: String },
    #[error("{message}")] Unknown { message: String },
    #[error("{message}")] ClosedSession { message: String },
    #[error("{message}")] Generation { message: String },
    #[error("{message}")] Unavailable { message: String },
    #[error("{message}")] Default { message: String },
    #[error("{message}")] Cancelled { message: String },
    #[error("{message}")] Closed { message: String },
}

impl PublicService for Agents {}

#[rutis::service(name = "sessions")]
impl Agents {
    pub fn agents(&self) -> AgentsReply { /* … */ }

    pub fn create_agent(
        &self,
        caller: &Ctx,
        request: CreateAgent,
        cancelled: CancelledFn,
    ) -> impl Future<Output = Result<AgentCreated, AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn close_agent(
        &self,
        caller: &Ctx,
        request: AgentRef,
    ) -> impl Future<Output = Result<(), AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn create_session(
        &self,
        caller: &Ctx,
        request: CreateSession,
        cancelled: CancelledFn,
    ) -> impl Future<Output = Result<CreatedSession, AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn close_session(
        &self,
        caller: &Ctx,
        request: SessionRef,
    ) -> impl Future<Output = Result<(), AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn delete_session(
        &self,
        caller: &Ctx,
        request: DeleteSession,
    ) -> impl Future<Output = Result<DeleteReply, AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn fork_session(
        &self,
        caller: &Ctx,
        request: ForkSession,
        cancelled: CancelledFn,
    ) -> impl Future<Output = Result<Forked, AgentsError>> + Send + 'static { /* admit, then await */ }

    pub fn acquire(&self, caller: &Ctx, request: LeaseRequest) -> Result<LeaseReply, AgentsError> { /* … */ }

    pub fn release(&self, caller: &Ctx, request: ReleaseRequest) -> Result<ReleaseReply, AgentsError> { /* … */ }

    pub fn observe(
        &self,
        caller: &Ctx,
        request: ObserveRequest,
        on_end: EndListener,
    ) -> Result<Option<Observed>, AgentsError> { /* register under the calling row */ }

    pub fn session(&self, request: SessionRef) -> Result<SessionState, AgentsError> { /* … */ }

    pub fn sessions(&self) -> SessionsReply { /* … */ }

    pub fn defaults(&self) -> SessionConfig { /* … */ }

    pub fn instance(&self) -> InstanceReply { /* … */ }

    pub async fn members(
        &self,
        request: MembersRequest,
    ) -> Result<MembersReply, AgentsError> { /* … */ }

    pub fn permission_defaults(&self) -> PermissionPolicy { /* … */ }

    pub fn quiesce(&self, caller: &Ctx) -> Result<QuiesceReply, AgentsError> { /* … */ }

    pub fn settle(
        &self,
        caller: &Ctx,
    ) -> impl Future<Output = Result<(), AgentsError>> + Send + 'static { /* admit, then await */ }
}
```
