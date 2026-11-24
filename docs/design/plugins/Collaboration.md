# Collaboration

Scope: process. Issue: #2020. Parts in other scopes: [CollaborationSession](CollaborationSession.md) (Session) and [CollaborationTimeline](CollaborationTimeline.md) (branch).

Collaboration owns a Team: the member Sessions one leader Session created, and the Agent each member runs. This document is the process owner's contract and the delegation's lifetime; the Session part's tool, texts, methods and approval routes are [CollaborationSession](CollaborationSession.md)'s, and the branch's facts and settlement are [CollaborationTimeline](CollaborationTimeline.md)'s. The current implementation is `Collaboration`, `CollaborationDirectory`, `CollaborationPlugin` and `CollaborationDirectoryPlugin` in `crates/dim-app-server/src/collaboration.rs`; the delegation mechanism's architecture is the [overview](../overview.md#56-collaboration-delegation-to-an-external-agent)'s.

## Use and scope: responsibility split

Collaboration owns:

- **Team relations.** Which member Session belongs to which leader Session and runs which Agent. A member belongs to exactly one leader; the relation is process memory rebuilt from stored member rows at load ([collaboration.rs:171-212](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L171-L212)).
- **Establishing a member.** Resolving the target Agent or creating an owned one, creating the member Session with the leader's cwd, sampled policy, inherited model and the leader's protocol version, and recording the relation ([collaboration.rs:270-448](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L270-L448)).
- **Ending a delegation.** `close` stops nothing by itself: the branch part settles the request and stops the member's turn, then this owner closes the member Session and removes the relation; the member's Agent record ends with its last member Session ([collaboration.rs:552-570](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L552-L570)).
- **Observation and recovery.** A member's business close or delete removes its relation; a delete of an unloaded member is observed; the first generation with a store removes unfinished delegations; a restart rebuilds Teams from the rows ([collaboration.rs:511-550](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L511-L550)).
- **The address space.** Which Agent a Team may address (public Agents, and owned Agents of the caller's own Team) and which member Session a leader may address ([collaboration.rs:440-460](https://github.com/arcships/dim/blob/5a279d6988c80692a77f3438fff4d26f7875e4c9/crates/dim-app-server/src/collaboration.rs#L440-L460)).

Callers keep:

- The model-facing texts, the `agent` tool's actions, the Session methods and the approval routes: [CollaborationSession](CollaborationSession.md).
- The delegation's facts, the request/result settlement and the branch's own state: [CollaborationTimeline](CollaborationTimeline.md).
- Agent and Session instance lifetime: Agents. Collaboration asks for a creation or a close by id and holds no instance.
- Records: Agents owns the Agent and member records; Collaboration reads the member links through `sessions.members` and leaves their writing to Agents' creation. The storage engine carries no face for them.
- Provider's temporary connections: Provider owns admissions; Collaboration asks Provider to retain the connection a member inherits.
- The subagent model policy document: CollaborationModelPolicy owns it; Collaboration reads `policy` once per member creation.

| Requirement (caller: this owner's Session part) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Turn a leader's `request` into a running member | No: resolving or creating the Agent, creating the Session, recording the relation and rolling back a failure are one establishment; a failed creation must leave no owned Agent | `establish` |
| End a member's delegation and close it in this process | No: the relation removal, the close and the owned-Agent release are one settlement | `close` |
| Learn which Agent a member Session runs | No: the relation is this owner's memory | `member_agent` |
| Learn whether a leader may address an Agent or a Session | No: the ownership and Team rules are this owner's | `visible`, `addresses` |
| List the caller's Team | No: the relation set and each member's running state are this owner's answer | `team` |
| Learn the leader of a member Session, for an approval forward | No: the relation is this owner's memory | `leader_of` |
| Read the member links for a rebuild | Yes: `sessions.members` is Agents' own record read | Business composes |

An operation or parameter exists only where the row says No.

## Caller example

The Session part establishing a member when the leader's branch commits a `request`:

```rust
let member = collaboration.establish(
    Establish {
        leader_session_id: leader.clone(),
        target: Target::Agent(agent_id),   // or Target::Profile(profile) for a temporary Agent
        adopted_model,
        policy,                            // sampled from the requesting line
    },
    cancelled,
).await?;
// member: {memberSessionId, memberAgentId}
```

Ending a delegation from the tool's `close` action after the branch settled its request:

```rust
collaboration.close(CloseMember { member_session_id: member }).await?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `sessions.agents`, `create_agent`, `close_agent`, `create_session`, `close_session`, `session`, `observe` | Agent and Session lifetime ([Agents](Agents.md)). |
| `sessions.members` | The member links Agents recorded, at load. |
| `providerService.retain_startup`, `release_startup` | The temporary connection a member inherits ([Provider](https://github.com/arcships/dim/pull/2096/files)); the retention requirement is below. |
| `collaborationModelPolicy.policy` | The model a new member runs ([CollaborationModelPolicy](https://github.com/arcships/dim/pull/2094/files)). |
| The member's line owners by `(session, line)` id | Stopping a member's turn and withdrawing its input ([Session access](#session-access)). |

Collaboration holds no Agent, Session or line object. Its Session part calls the operations below in-process; they have no catalog name and no other caller.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `establish` | Create a member and record the relation | `{leaderSessionId, target, adoptedModel?, policy}` where `target` is `{agentId}` or `{profile}` | `{memberSessionId, memberAgentId}` | Before the member Session is created; a cancelled establishment removes the Agent it created | `not_found` (Agent absent, not visible, or owned with no member), `busy` (the member already has an open request), `create` (Session creation failed), `policy` (the policy could not be read), `closed`, `cancelled` |
| `close` | End a delegation and close the member Session | `{memberSessionId}` | `{}` | Not cancelled once admitted | `not_found` (no relation for that member), `closed` |
| `member_agent` | The Agent a member Session runs, when the caller's Team addresses it | `{callerSessionId, memberSessionId}` | `{agentId}` or none | None | — |
| `visible` | Whether an Agent is public or owned by the caller's Team | `{callerSessionId, agentId}` | `{visible}` | None | — |
| `addresses` | Whether a caller's Team may address a member Session | `{callerSessionId, agentId, memberSessionId}` | `{addresses}` | None | — |
| `team` | The caller's Team with each member's running state | `{callerSessionId}` | `[{role, agentId, sessionId, name, status}]`; `status` is `working`, `idle` or `closed` | None | — |
| `leader_of` | The leader of a member Session | `{memberSessionId}` | `{leaderSessionId}` or none | None | — |

`establish` writes no fact: the leader's branch records the request and its delivery in [CollaborationTimeline](CollaborationTimeline.md); the relation is process state rebuilt from `sessions.members` on load.

`close` closes the member Session through Agents' `close_session` and removes the relation. It does not close the leader, delete rows, or stop a turn: the branch part settles the request and stops the member's turn first, so ordering a close against a live turn is the branch's concern.

`team` reads each member's running state from Agents' `session` value; a member that is not loaded is `closed`, and a loaded member with a running turn is `working`.

### Delegation lifetime

- **Durable facts.** An Agent's owner and a member's leader are Agents' records: Agents writes both in the same creation and answers them through `sessions.members`; the Session row keeps its creation origin and leader for `store.creationRecord`. The member Session and its branches are ordinary Session records ([native storage format](../appserver/native-storage-format.md)). Teams are rebuilt from Agents' records, so nothing becomes public by being unloaded, unread or reloaded.
- **Visibility.** An owned Agent is visible only within its owner's Team; a public Agent is visible to every Team; an absent relation is a refusal, never a public record. An owned Agent with no member Session is an incomplete delegation: a `request` names it as absent, and startup removes it.
- **Restart.** A restart restores owned Agents as owned and members with their leader. It also recovers requests that have no `collaboration/result` yet: each recovered request is stopped on the line that holds the member input its `collaboration/delivered` fact names, never on the member's current default line; without that evidence recovery stops nothing and logs the skip.
- **Close.** `close` ends the member Session for this process, keeps its records, and ends its relation. A restart loads the member again as an ordinary Session its leader may address; closed is process state and is not stored.
- **Delete.** Deleting a leader ends its Team through Agents' `delete_session`: each member's own delete runs first, then each owned Agent closes and its record goes, then the leader. There is no transaction across the Team: a failure midway leaves the leader with part of its Team gone, and a retry finishes.
- **Fork.** Forking a member is refused: the fork would carry the member's role and Agent but no leader.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `not_found` | Treat the target as absent: an Agent outside the Team, a member of another Team, or an owned Agent whose member Session is gone. The tool answers `agent_not_found` or `team_member_not_found`. |
| `busy` | The member already has an open request; end or wait for it. The tool answers `busy`. |
| `create` | The member could not be created; the Agent created for the request is removed. The tool answers `member_create_failed` with the cause. |
| `policy` | The subagent model policy could not be read; no member is created. The tool answers `member_create_failed`. |
| `closed` | This owner is unloading; the caller's branch rebuild settles its requests. |
| `cancelled` | The caller stopped before the member Session was created; nothing was created. |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `establish` | Resolve the Agent or create an owned one; read the policy; create the member Session through Agents; record the relation; watch the member's end | Before the member Session exists: the Agent created for the request is removed | The established member stays, and its relation stays until the member closes or is deleted | Admission stops; a reloaded generation rebuilds relations from `sessions.members` and re-arms an observation for each loaded member | The Agent and member records by Agents; the relation by Collaboration |
| `close` | Remove the relation, close the member Session, release the owned Agent when its last member Session's rows are gone | Not cancelled | The close runs as the owner's work; the reply may be lost and the member is closed all the same | A reloaded owner re-reads the rows; a closed member stays closed for this process | Agents |
| `team`, `member_agent`, `visible`, `addresses`, `leader_of` | Read the relations and Agents' `session` values | None | Nothing to release | Pending reads end with the generation; a reloaded owner answers from its rebuilt relations | Collaboration |

## Session access

A member's owners are reached by id: `(memberSessionId, line)` resolves any branch-group data face, not only the input write and the published-value read ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1); registration points and lifecycle services are excluded (D2). The mechanism resolves the id and the caller calls the owner's own operation. Collaboration receives no `Branch` and no `Session` and reads no member Timeline; the branch part performs these writes, and implementation is [#2093](https://github.com/arcships/dim/pull/2093).

## Exceptions with reasons

- Relations live in their own process part (`CollaborationDirectory`) rather than in `Collaboration`: a reload of the creation logic must not lose the watch on already loaded members, which produce no restore event. This is an arrangement inside one owner, not a second owner.
- The relation set is process memory rebuilt at load from `sessions.members`; it is not rebuilt per call, and a member whose records are gone is not a member.
- Deleting a leader ends its Team through Agents' `delete_session`, not here: the cascade needs the Session gate only Agents holds.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process owner (creation, relations, address space). It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | Process. A Team spans Sessions. The member's turn, input and history stay with the member's own owners. |
| §3 orthogonality | No other owner's state is read or changed; Agents, SessionStore, Provider and the policy owner are asked through their operations. |
| §4 rule 1 | No command event: the leader's branch writes the request and delivery facts; this owner records the relation. |
| §4 rule 2 | No hand-off: a member Session is created and closed by id. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business text or model policy in core. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: the member's transcript is read by its own owners. |
| §4 rule 7 | Callers use these operations; no caller reads the relation map or a member's facade. |
| §4 rule 8 | One requirement is raised: `(session, line)` addressing, shared with Agents and the branch part. |
| §4 rule 9 | The relation is process state, not a Timeline fact. |
| §7 ids and values | Ids and values cross; no Agent, Session or Branch object. |
| §7 one interface | One internal interface for the owner's Session part; no catalog name and no second face. |
| §7 one change | The implementation deletes `CollaborationDirectory`'s face, the temporary-Agent map and the unused injections together with the caller moves. |
| ADR 002 D1 | Process scope: Teams span Sessions. |
| ADR 002 D2 | No wiring plugin: creation and relations are one owner behind one Rust interface. |
| ADR 002 D3 | Dependencies point to Agents, SessionStore and Provider; nothing narrower depends on this owner. |
| ADR 002 D4 | No configuration member: the policy is its owner's document. |
| ADR 002 D5 | The interface is language-neutral; the Rust binding is the same operations. |

## Requirements raised to other owners

1. **Provider retains a startup connection by source id for a member.** A member inherits the leader's adopted model; when that model carries a startup connection, retention needs the connection the leader's creator admitted, and Provider's `retain_startup` takes an admission id only. Either `retain_startup` accepts `{source, session}` for a source another Session already admitted, or Provider states that an inherited startup connection is not retained for a member. Raised with [#2019](https://github.com/arcships/dim/issues/2019).
2. **`(session, line)` addressing** as stated in [CollaborationTimeline](CollaborationTimeline.md) and [Agents](Agents.md): [ADR 027](https://github.com/arcships/dim/pull/2029/files) decides it, the mechanism implements it, and callers write `line_owner::<T>(line)?.op(..)` for any branch-group data face except registration points and lifecycle services — no forwarding facade and no string-routed call. Implementation is [#2093](https://github.com/arcships/dim/pull/2093).

## Supersedes

- `docs/design/contracts/session-access.md` §1/§6 and `docs/design/appserver/sessions-service.md`: the `sessions` proxy this owner used to read members through is replaced by [ADR 027](https://github.com/arcships/dim/pull/2029/files).
- `docs/design/service-faces.md` §3.1: the `collaborationDirectory.relations` example is removed with that face.
- `docs/design/subagent-tool-approval.md` §3: the route table's owner is this page and [CollaborationSession](CollaborationSession.md); the approval-destination shape stays as that contract states.

## Binding appendix

No catalog face. The interface exists on the `Collaboration` type for this owner's Session part; requests and replies are serde data with `camelCase` fields.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Target { Agent { agent_id: String }, Profile { profile: crate::protocol::AgentProfile } }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Establish {
    pub leader_session_id: String,
    pub target: Target,
    #[serde(default)] pub adopted_model: Option<dim_agent_types::ModelSelection>,
    pub policy: dim_agent_types::PermissionPolicy,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CollaborationError { NotFound { message: String }, Busy { message: String }, Create { message: String }, Policy { message: String }, Closed { message: String }, Cancelled { message: String } }

impl Collaboration {
    pub async fn establish(&self, request: Establish, cancelled: CancelledFn) -> Result<Established, CollaborationError> { /* … */ }
    pub async fn close(&self, request: CloseMember) -> Result<(), CollaborationError> { /* … */ }
    pub fn member_agent(&self, request: MemberRef) -> Option<MemberAgent> { /* … */ }
    pub fn visible(&self, request: Visibility) -> Visible { /* … */ }
    pub fn addresses(&self, request: AddressCheck) -> Addresses { /* … */ }
    pub fn team(&self, request: CallerRef) -> Team { /* … */ }
    pub fn leader_of(&self, request: MemberRef) -> Option<Leader> { /* … */ }
}
```
