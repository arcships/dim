# Session access: Agents and Session public-interface supplement

Status: Draft. Overall design: [overview §3 integration](../overview.md#3-integration-by-existing-mechanisms), [Appendix B Agents and InputQueue](../overview.md#appendix-b-supplements-to-existing-plugins); rules: [ADR 002](../../adr/002-plugin-scope.md) D1–D3, D5. Phase: P0. Language: neutral contract; Rust binding in [Appendix E](#appendix-e-rust-binding).

> **Offline resource nodes.** Rows about an offline resource node describe P3 multi-node behaviour. #5 is withdrawn (rutis is an external dependency, and it withdraws imported services when a link drops), so these rows are redesigned in P3 following overview §6.3: node services are looked up on each call by the Session's resource node, which P3 adds to `SessionConfig` ([A.3](#a3-sessionconfig-fields)). In P1/P2 every Session uses the control node.

Process plugins reach Sessions the way Collaboration already does: through Agents and the public Session interface. This document defines that access as the language-neutral process service `sessions` (ADR 002 D5), provided by Agents, so a TypeScript or Python process plugin (Scheduler, Workflow) uses it by name. Its methods are `launch`, `input`, `lookup`, `history`, `withdraw`, `stop` and `idle`, with request and reply as data ([A.1](#a1-the-sessions-service)). In Rust it is `AgentIndex` plus the `Session` facade ([Appendix E](#appendix-e-rust-binding)). It adds no host or facade plugin, and a caller that depends on Agents reloads with it (D3).

| Supplement | Owner | Used by |
| --- | --- | --- |
| `sessions` service: named, data-only access to Sessions | Agents | Scheduler, Workflow process parts |
| `launch` with a policy capped by the starter | Agents | Scheduler, Workflow |
| `SessionConfig` field `interactive` | Agents | Plugins that read it from the Session's `SessionConfig` (Questions, SessionTitle) |
| Input with source raw blocks (marker, keys, `submissionId`) | InputQueue data contract, runtime facade | Scheduler, Workflow; Goal branch parts write the same form, and Hooks branch parts (to be designed) would too |
| Fixed-prefix read | Runtime facade (requirement, §9) | Process callers and branch parts deciding from Timeline facts |
| Withdrawal (command `input/withdraw` with optional `reason`, proposed in [overview Appendix B.1](../overview.md#b1-session-and-branch-capabilities), result `input/deleted`, `session.input.withdraw` through InputQueue's `sessionMethods`) | InputQueue | Clients, Hooks (to be designed), AcpGateway, Scheduler, Workflow |

Session-scoped and branch-scoped plugins do not use this interface to reach their own Session. They use their Session's and branch's services, the branch Dispatcher and the Session broadcast directly.

## 1. Structure

```mermaid
flowchart LR
  subgraph PROC["Process"]
    Agents["Agents: sessions (launch, input, history, ...)"]
    Sched["Scheduler process part"]
    Wf["Workflow process part"]
    Rpc["Rpc"]
  end
  subgraph SESS["Session"]
    SC["SessionConfig"]
    Fac["Session facade: input, history, idle, stop, service"]
  end
  subgraph BR["Branch"]
    IQ["InputQueue: input/applied, input/deleted"]
    LP["Loop: input/consumed"]
    TL["Branch Timeline"]
  end
  Sched --> Agents
  Wf --> Agents
  Rpc --> Agents
  Agents -->|"per call: acquire Session"| Fac
  Fac --> SC
  Fac -->|"input/message"| TL
  Fac -->|"service InputQueue: withdraw (writes input/withdraw)"| IQ
  TL --> IQ
  TL --> LP
```

| Part | Scope | Status |
| --- | --- | --- |
| `sessions` (Agents) | Process | Supplement; maps to the existing `acquire` and facade, plus `launch` |
| `SessionConfig` field `interactive` | Session record | Supplement |
| Session facade ([agent.rs](../../../crates/dim-agent-runtime/src/agent.rs), [branch.rs](../../../crates/dim-agent-runtime/src/branch.rs)) | Session, branch | Existing; fixed-prefix read (§9.2) is a requirement |
| InputQueue | Branch | Change: consumes `input/withdraw`, writes `input/deleted`; withdrawal service |

Contracts defined here are the `sessions` service with `LaunchRequest`, `InputWrite`, `RawBlock`, `HistoryRequest`, `HistoryPage`, `LookupReply`, `WithdrawRequest`, `WithdrawReply` and `SessionsError`; the `SessionConfig` field `interactive` and the `interactive` parameter of `session.create`; the source marker, key and `submissionId` conventions; and the withdrawal contract. Contracts used are the runtime facade, Agents loading, the `input/message` data and consumption contract, the existing context-block element format and the Session input path.

## 2. Data and state

| State | Owner | Rebuild |
| --- | --- | --- |
| `SessionConfig` with `interactive` | Agents, Session record | From the record on restore |
| Inputs and their applied, consumed and deleted facts; InputQueue's held set | InputQueue, Loop | From local history on reload: an input is held while it has neither `input/applied` nor `input/deleted` |
| Leases held during a `sessions` call | Agents | Runtime only |
| Keyed block effect | History projection ([history.rs](../../../crates/dim-agent-capabilities/src/history.rs)) | From the fixed prefix and compaction facts |
| Caller progress (runs, pending `submissionId`s) | Caller's process storage | Caller's document |

An input is pending from its `input/message` until either `input/applied` followed by `input/consumed`, or `input/deleted`, references its event id. `input/withdraw` alone never changes whether an input is pending; only InputQueue's result fact does. Every "pending input" derivation (Goal, AcpGateway, Scheduler, Workflow) uses exactly this rule.

## 3. Flows

### 3.1 Launch, write, wait

```mermaid
sequenceDiagram
  participant C as Caller (Scheduler)
  participant AI as Agents sessions
  participant S as Session (non-interactive)
  participant TL as Branch Timeline
  participant L as InputQueue, Loop
  C->>AI: launch {source scheduler, policy, starter}
  AI->>AI: cap policy by the starter's current policy
  AI-->>C: {sessionId}
  C->>AI: input {sessionId, main, queue, marker dim-scheduler, submissionId}
  AI->>S: input
  S->>TL: input/message
  TL-->>L: input/applied, turn/start, input/consumed
  C->>AI: idle {sessionId, main}
  AI-->>C: {sequence N}
  C->>AI: history {sessionId, main, atLeast N}
  C->>C: record outcome in process storage
```

The caller holds no lock. Facts committed after its read are ordered before its write and are resolved by the input owners. A caller that may retry after an uncertain outcome calls `lookup` with its `submissionId` first.

### 3.2 Withdrawal

```mermaid
sequenceDiagram
  participant C as Client or process caller
  participant W as InputQueue withdrawal service
  participant TL as Branch Timeline
  participant IQ as InputQueue (dispatch)
  C->>W: withdraw(inputId, reason?)
  W->>TL: append input/withdraw {input_id, reason?} at w
  TL-->>IQ: dispatch w in sequence order
  alt input held: no input/applied, no input/deleted
    IQ->>TL: input/deleted {input_id} at d
    W-->>C: deleted {sequence d}
  else unknown, applied or already deleted
    IQ->>IQ: no effect, nothing written
    W-->>C: notHeld
  end
```

`input/withdraw { input_id, reason? }` is a command event, and `input/deleted { input_id }` is InputQueue's result fact. Legitimate producers write the command: clients through `session.input.withdraw`, which InputQueue provides through `sessionMethods` ([session-boundary](session-boundary.md#a1-method-contributions)), and process callers through `sessions.withdraw`. InputQueue consumes commands in sequence order and decides from its own decision state only ([time bus §4](../../architecture/rust-agent/time-bus.md#4-写入前的复核归各状态机)): the command takes effect only if InputQueue still holds that `input/message` unapplied. Once InputQueue has written `input/applied` for an input, the input belongs to Loop and cannot be withdrawn; Loop needs no change. For one input, at most one of `input/applied` and `input/deleted` exists.

## 4. Timing and conflicts

| Scenario | Order | Outcome | Owner |
| --- | --- | --- | --- |
| Input before a turn `stop/interrupt`, not consumed | applied < interrupt | Stays pending; consumed by the next turn | Loop |
| Input after `stop/interrupt` | interrupt < input | Queue/Steer start a turn, Append waits | InputQueue, Loop |
| Withdraw while the input is held | input < `input/withdraw`, no `input/applied` | `input/deleted`; never applied or consumed; not pending | InputQueue |
| Withdraw after the input is applied | `input/applied` < `input/withdraw` | No effect, nothing written; reply `notHeld`; the input is consumed normally | InputQueue |
| Agents reload during a `sessions` call | — | Caller reloads with Agents (D3); committed inputs stay; caller calls `lookup` before rewriting | rutis lifecycle, caller |
| Session's resource node offline | — | Input committed; tool calls on that node fail `node_unavailable`; nothing goes Pending | rutis link (#5) |
| Launch with a policy wider than the starter's | — | Capped to the starter's current policy | Agents |

Full table: Appendix B.

## 5. Context mapping

| Input form | What the model sees | Placement |
| --- | --- | --- |
| Marker block | One user-role message with the verbatim `<dim-<source> kind="...">` element | First, at the input's consumed position |
| Other raw blocks | One user-role message each, verbatim, in order | After the marker |
| User content (text, images) | One user message | After all blocks of the same input |
| Keyed block (`Live`) | As a raw block; the latest block per key stays in effect | Consumed position |
| Retire block | Delivered once; the key leaves effect | Consumed position |
| Deleted input (never applied) | Nothing: its user content and raw blocks never enter context, its keyed blocks never take effect, and compaction never sees it. Clients may show it as deleted from `input/deleted` | — |

Queue inputs are consumed when Loop starts the turn they drive. Steer inputs are consumed at the next boundary of the running turn and start a turn when the branch is idle. Append inputs are consumed with the next model request and never start a turn. Through compaction, consumed inputs survive only in the summary, except that the latest block of each live key is replayed verbatim. A child branch sees the ancestor prefix up to its cut, including the keys in effect at the cut, and never adopts the parent's pending inputs. The model never sees `submissionId`, mode, `SessionConfig` fields or leases.

## 6. Interfaces (summary)

| Interface | Mode | Purpose |
| --- | --- | --- |
| `sessions.launch(LaunchRequest) -> {sessionId}` | async | Create a non-interactive Session for automation with a capped policy |
| `sessions.input(InputWrite) -> {inputId, sequence}` | async | Write input with source raw blocks |
| `sessions.lookup({sessionId, branchId, submissionId}) -> LookupReply` | async | Find a committed input after an uncertain outcome |
| `sessions.history(HistoryRequest) -> HistoryPage` | async | Read history, optionally as a fixed prefix bounded by the published head |
| `session.input.withdraw {sessionId, branchId, inputId, reason?}` | RPC | Client withdrawal; InputQueue's `sessionMethods` (namespace `session.input`) |
| `sessions.withdraw(WithdrawRequest) -> WithdrawReply` | async | Write `input/withdraw` for an input the caller wrote, by its `input_id`, and return InputQueue's decision |
| `sessions.stop({sessionId, branchId}) -> {sequence}` | async | Interrupt the branch's turn |
| `sessions.idle({sessionId, branchId}) -> {sequence}` | async | Wait until the branch is idle; the returned sequence is the last committed one, a whole-batch boundary |
| `SessionConfig.interactive` | data | Creation-time configuration |
| `session.create {interactive}` | RPC | Non-interactive creation |

## 7. Security

Only plugins in the control node's process hold `sessions`, and it is never exported over RPC entries or rutis links. Process callers withdraw an input by the `input_id` they received when writing it, and clients withdraw through Rpc's Session authorization. Markers are model-facing text: shared components (InputQueue, Agents, the runtime) never parse them; the plugin that wrote a marker may interpret its own content. Writing input confers no authority, because the resulting tool calls are admitted by Approval and Permission under the Session's policy. Inputs are replayed into model context, so callers must not put credentials or host security settings into them.

## 8. Rule check

| Rule | How this document complies |
| --- | --- |
| §4 rule 1 (command events) | Withdrawal is the command `input/withdraw`; only InputQueue writes the result `input/deleted`, and a withdraw with no effect writes nothing and replies `notHeld`. |
| §4 rule 2 (handover) | `input/applied` hands an input from InputQueue to Loop; after it the input cannot be withdrawn, only interrupted with `stop/interrupt`. |
| §4 rule 3 (before callbacks only) | Entries check only param shape and authorization; no before callback is used. |
| §4 rule 4 (no business meaning in core) | `reason`, markers, keys and `submissionId` are generic data; InputQueue and Agents never interpret a source. |
| §4 rule 5 (batch) | InputQueue handles `input/message` and `input/withdraw` in sequence order; "withdraw and re-submit as Steer" writes the withdraw first in one batch. |
| §4 rule 6 (model context) | A deleted input never enters context; consumed inputs are placed at their consumed position (§5). |
| §4 rule 7 (public events and services) | Process callers reach Sessions only through `sessions`, and withdraw only through InputQueue's public withdrawal service. |
| §4 rule 8 (requirements) | #4, fixed-prefix read and optional-part assembly are raised in §9. |
| §4 rule 9 (only decision facts in the Timeline) | `SessionConfig`, leases and caller progress stay out of the Timeline; only inputs, withdrawal commands and results, and `stop/interrupt` from `stop` are events; the descriptive `reason` on the required `input/withdraw` is allowed by rule 9. |
| §1 (Capability and App Server boundary) | Agents only exposes access; callers keep their business logic in their own plugins. |
| ADR 002 D1 | `sessions` is a process service of Agents; InputQueue's withdrawal stays in its branch part, served to clients by its Session part. |
| ADR 002 D2 | No access host or facade plugin; callers use Agents as Collaboration does. |
| ADR 002 D3 | A caller depending on Agents reloads with it, and resolves uncertain writes with `lookup`. |
| ADR 002 D4 | `SessionConfig.interactive` is creation data of the Session record, not plugin configuration. |
| ADR 002 D5 | `sessions` is named with data-only methods and errors; Rust types appear only in Appendix E. |

## 9. Requirements raised to mechanism owners

### 9.1 Runtime services exposed by name (#4)

| Item | Content |
| --- | --- |
| Blocked by | ADR 002 D5: non-Rust parts (SessionTitle, Goal, Web, the Scheduler and Workflow Session parts, ExternalAgent, PluginManager) must use the runtime's typed services (Timeline, Dispatcher, InputQueue, Session broadcast) without a hand-written Rust binding. App Server services they also use (SessionStore metadata, Settings, `AuxiliaryModel`) are named by their owners' supplements ([overview Appendix B.2](../overview.md#b2-app-server-process-plugins)). |
| Existing mechanisms considered | rutis named services (`TypeKey::named`, `host_key`): work only for services registered by name, and the runtime registers these by Rust type. Hand-written bindings per plugin: rejected by D5. Starting in Rust: the fallback until this lands. |
| Generic capability | The runtime registers each public typed service also under a stable service name with a data-only method shape, at the same scope and key. |
| Effect on sequence, batch, interrupt, reload | None: calls by name keep the typed service's ordering, batch and interrupt rules; a named service reloads with its owner. |
| Owner and issue | Runtime; [#4](https://github.com/arcships/dim/issues/4). |

### 9.2 Fixed-prefix read

| Item | Content |
| --- | --- |
| Blocked by | §4 rule 5 and time bus §4: a process caller deciding from history must read through a known published sequence, never the uncommitted or unconsumed tail. |
| Existing mechanisms considered | `Session::history`: no lower bound on the head, so a read may miss the event that triggered it. Subscribing to the Dispatcher: process callers have no branch part. |
| Generic capability | Read a branch's published history through at least a given sequence, failing `future_sequence` if the head is below it (A.5). |
| Effect on sequence, batch, interrupt, reload | Reads only; the returned head is a whole-batch boundary. A reload makes the read fail `reloading`. |
| Owner and issue | Runtime (Session facade, `Session::prefix`); issue to be opened. |

### 9.3 Caller source fixed from plugin identity

Withdrawn ([#24](https://github.com/arcships/dim/issues/24)): a caller withdraws by the `input_id` it received when writing the input, so no caller source is needed.

### 9.4 Optional parts per enabled plugin

| Item | Content |
| --- | --- |
| Blocked by | ADR 002 consequences: optional Session and branch parts are installed only when enabled. |
| Existing mechanisms considered | Installing every part and checking inside it: placeholders the ADR rejects. Host-side `AgentBuilder` calls per plugin: not language-neutral (D5). |
| Generic capability | Agent assembly collects the named services each installed plugin declares in `provides`, and installs a part only when its plugin is enabled, and does not wait for an optional part to become active: the Session is built without it and only that plugin's features are unavailable. Whether a part is attached is decided during the Session preparation described in session-boundary A.5, before branches open; a part that becomes active later is attached on the next Session reload, so the tool set stays the same within one load (ADR 002 D3, #34). Installation never depends on `SessionConfig` fields such as `interactive`, so the tool set and prompt prefix stay the same across a fork (#30). |
| Effect on sequence, batch, interrupt, reload | Decided during Session preparation before branches open, and a Session reload decides again; enabling a plugin applies to Sessions loaded afterwards. |
| Owner and issue | Runtime (Agent assembly); issue to be opened. |

#5, used by the offline rows, in [ExternalAgent §9](../plugins/ExternalAgent.md#9-requirements-raised-to-mechanism-owners). The optional `reason` on `input/withdraw` and the `LaunchRequest.agent` default are changes of InputQueue and Agents, listed in [overview Appendix B](../overview.md#appendix-b-supplements-to-existing-plugins).

## Appendix A. Interface details

### A.1 The sessions service

`sessions` is a process service provided by Agents, without an instance key. Shapes are JSON with camelCase fields; a `?` after a field name marks it optional and `|` separates alternatives. Each call acquires the Session (restoring it if needed) and holds it loaded until the reply. A caller's `source` is fixed by Agents from the calling plugin's declared source name.

| Method | Mode | Request | Reply | Errors |
| --- | --- | --- | --- | --- |
| `launch` | async | `LaunchRequest` | `{sessionId}` | `rejected`, `reloading`, `failed` |
| `input` | async | `InputWrite` | `{inputId, sequence}` | `rejected`, `session_closed`, `branch_missing`, `reloading`, `publication_unknown`, `failed` |
| `lookup` | async | `{sessionId, branchId, submissionId}` | `LookupReply` | `ambiguous`, `session_closed`, `branch_missing`, `reloading` |
| `history` | async | `HistoryRequest` | `HistoryPage` | `future_sequence`, `session_closed`, `branch_missing`, `reloading` |
| `withdraw` | async | `WithdrawRequest` | `WithdrawReply` | `rejected`, `session_closed`, `branch_missing`, `reloading` |
| `stop` | async | `{sessionId, branchId}` | `{sequence}` | `session_closed`, `branch_missing`, `reloading` |
| `idle` | async | `{sessionId, branchId}` | `{sequence}` | `session_closed`, `branch_missing`, `reloading` |

```text
// LaunchRequest
{ "agent?": "string",                // Agent profile id; absent: Agents' default profile
  "cwd": "string",
  "model?": "string",                // model id; takes precedence over the profile's model
  "policy?": { /* PermissionPolicy data */ },  // narrowed request; absent: starter's or read-only default
  "starter?": { "sessionId": "string", "branchId": "string" } } // upper bound; absent: host configuration bound
// InputWrite
{ "sessionId": "string", "branchId": "string",
  "mode": "queue" | "steer" | "append",
  "submissionId": "<source>:<uuid>",
  "rawBlocks": [ RawBlock ],         // first block is the caller's marker (A.2)
  "content": [ { "type": "text", "text": "..." } | { "type": "image", "...": "..." } ] }
// RawBlock
{ "text": "string", "key?": { "use": "live" | "retire", "name": "string" } }
// LookupReply
{ "status": "accepted", "inputId": "string", "sequence": 0 } | { "status": "unknown" }
// HistoryRequest
{ "sessionId": "string", "branchId": "string",
  "after?": 0,                        // exclusive lower bound; absent: from the start of the branch
  "through?": 0,                      // read up to this sequence
  "atLeast?": 0 }                     // fixed-prefix read (A.5)
// HistoryPage
{ "through": 0, "events": [ { "sequence": 0, "branchId": "string", "type": "turn/end", "data": {} } ] }
// WithdrawRequest
{ "sessionId": "string", "branchId": "string", "inputId": "string",
  "reason?": "string" }                 // copied into input/withdraw; shown by clients
// WithdrawReply
{ "status": "deleted", "sequence": 0 }   // sequence of input/deleted
| { "status": "notHeld" }                  // unknown, already applied or already deleted; InputQueue wrote nothing
// SessionsError
{ "code": "rejected" | ..., "message": "string", "retryable": false }
```

Cancellation: the caller dropping an `idle` or `history` call ends it with no effect. Dropping an `input` before the reply leaves the outcome uncertain; the caller resolves it with `lookup`.

`launch` creates a Session with `interactive = false`. The effective policy is the requested policy capped by the starter's current policy, which is a Timeline fact read at launch. Without a starter, a run started by a management method defaults to read-only unless the request declares a policy, and it is always capped by host configuration.

### A.2 Source input conventions

| Item | Rule |
| --- | --- |
| Marker | The first raw block is the element `<dim-<source> kind="..." ...>body</dim-<source>>` in the existing context-block format. It is model-facing text written by the producing plugin; shared components never parse it, and only the producing plugin may interpret its own content (for example Goal folding its own blocks) |
| Keys | `RawBlock.key` with `use: live` or `retire`, per branch Timeline. The writer chooses the full key; by convention it starts with the writer's own prefix (overview constraint 14), and nothing checks it |
| Submission identity | `<source>:<uuid>`, generated per logical submission. Writes do not deduplicate; `sessions.lookup` (same semantics as `session.input_lookup`) finds a committed input (`accepted`, `unknown`, error `ambiguous`) |
| Never possible | Writing events other than `input/message`, `input/withdraw` through `withdraw` and `stop/interrupt` through `stop`; writing `input/deleted`; editing history |

### A.3 SessionConfig fields

```text
// SessionConfig (new field; existing: permissionPolicy, cwd, model, systemPrompt, tools, collaborationRole)
{ "interactive": true }             // false: exec, ACP, automation; absent: true
```

`interactive` means the Session has an interaction counterpart that can answer its questions and approvals: the user through a client, or another Session's or branch's agent (for example a Collaboration leader). It does not require a human. `interactive` is fixed at creation. Assembly never reads it (ADR 002 D3, #30), so the tool set stays the same and a fork keeps the prompt prefix and its KV cache. Plugins that care read it from the Session's config directly: Questions (`ask_user` stays declared and answers that no one is available to answer) and SessionTitle (titles interactive leader Sessions only).

`session.create` gains `interactive?: bool` (default `true`), checked at the boundary only. `session.create` rejects unknown fields, so an older daemon fails a request that carries `interactive` with invalid params; clients therefore send it only when it is `false`.

A fork copies the source Session's whole `SessionConfig`; the permission preset follows with the copied `permission/changed` facts (#26).

Format decision: the native store stays on version 13. `interactive` is an additive optional field of the stored config JSON, and a stored record without it means `true`, because every Session created before the field existed was a client Session. An older daemon ignores the field and reads a non-interactive record as interactive; this is accepted. No version 14.

Not in this contract: `createdBy` (the Session's origin) and the `client` parameter of `session.create` are added when something decides with them. The resource node is a P3 addition (overview §6.3): an additive optional `SessionConfig` field, where a missing value means the control node.

### A.4 Withdrawal

| Item | Content |
| --- | --- |
| Command | `input/withdraw { input_id, reason? }`, a branch command event. `reason` is generic optional text with no business meaning, which clients show for the deleted input; events written without it are unchanged. Producers: the withdrawal service (clients, process callers). It is never a fact about the input by itself |
| Result fact | `input/deleted { input_id }`, written only by InputQueue, only when it consumes an `input/withdraw` while it holds that input unapplied |
| Held | InputQueue holds an input from its `input/message` until it writes `input/applied` or `input/deleted` for it. |
| No effect | Unknown id, input already applied, or already deleted: InputQueue writes nothing and no failure event exists; the caller's reply is `notHeld` |
| Client path | `session.input.withdraw {sessionId, branchId, inputId, reason?}`, provided by InputQueue through `sessionMethods` (namespace `session.input`; proposed Session-scoped InputQueue part that calls the branch's withdrawal service), so Rpc names no owning service → `{status: "deleted", sequence}` or `{status: "notHeld"}` |
| Process and branch-part path | `sessions.withdraw` for process callers, InputQueue's branch withdrawal service for branch parts: by the `input_id` returned when the caller wrote the input |
| Write path | The service appends `input/withdraw`, then replies with InputQueue's decision for that command. InputQueue's recheck uses only its own decision state (held set), never a lock on another owner's facts |
| Withdraw and re-submit as Steer | One atomic batch: `input/withdraw` for the old input plus the new Steer `input/message`. The withdraw takes effect only if the old input is still held; the new input is written either way |
| After apply | An applied input cannot be withdrawn; Loop consumes it as usual and needs no change |

### A.5 Fixed-prefix read

`sessions.history` with `atLeast` fixes `through` as the published head at call time. It fails with `future_sequence` if that head is below `atLeast`, and otherwise returns the history through that head, including ancestors. Every event delivered by the branch Dispatcher or the Session broadcast is published, so `atLeast = event.sequence` succeeds while the branch exists.

### A.6 Errors

| Code | Meaning |
| --- | --- |
| `session_closed`, `branch_missing` | Nothing written |
| `reloading` | Agents or the Session's capabilities are reloading; retryable after `lookup` |
| `rejected` | Param shape (external-boundary checks only); nothing written |
| `future_sequence` | Fixed-prefix read beyond the published head |
| `ambiguous` | `lookup` matched more than one input |
| `publication_unknown` | Committed, publication failed; `lookup` after recovery |
| `node_unavailable` | Not returned by `sessions`: calls on an offline resource node fail inside the Session's tools |
| `failed` | Session fault |

## Appendix B. Full conflict table

| Scenario | Sequence order | Outcome | Owner of the decision |
| --- | --- | --- | --- |
| Input before `stop/interrupt`, not consumed | applied < interrupt | Consumed with the next turn | Loop |
| Input after `stop/interrupt` | interrupt < input | Queue/Steer start a turn; Append waits | InputQueue, Loop |
| Input consumed by the interrupted turn | consumed < interrupt | Cancelled with that turn; stays in history | Loop |
| Branch close | close < write | `SessionClosed`/`BranchMissing` | Runtime facade |
| Concurrent writers | Timeline order | Applied by mode in sequence order | InputQueue |
| Tool batch not settled | input < `step/tool-result` | Steer and Append after the batch; Queue after `turn/end` | Loop |
| `input/withdraw` while held | input < withdraw, no `input/applied` | `input/deleted`; never applied or consumed | InputQueue |
| `input/withdraw` after apply | `input/applied` < withdraw | No effect; `notHeld`; consumed normally | InputQueue, Loop |
| Two withdraws for one input | withdraw₁ < withdraw₂ | First deletes (if held); second `notHeld` | InputQueue |
| Withdraw of an unknown id | — | No effect; `notHeld` | InputQueue |
| Withdraw and re-submit as Steer, old input already applied | applied < batch | Old input consumed normally; new Steer input applied as usual | InputQueue |
| Compaction between write and consumption | input < compaction < consumed | Placed after the summary; a deleted input is never seen by compaction | ContextPreparation |
| Child branch cut between write and consumption | input < cut < consumed | Child does not adopt it | InputQueue (child) |
| Two writers, same key | Timeline order | Later supersedes; owner prefixes avoid collisions by convention | History projection |
| Decision read, newer fact, then write | through < fact < write | Write ordered after the fact | Input owners |
| Agents reload | — | Callers reload with it (D3); committed inputs stay | rutis lifecycle |
| Restart after commit, reply lost | — | `lookup` returns `accepted`; no duplicate | Caller |
| Session not loaded | — | Agents restores it before the call | Agents |
| Resource node offline | — | Input committed; the turn runs and calls on that node fail `node_unavailable`; no dependent reloads or goes Pending (requirement on #5) | rutis link |

## Appendix C. Acceptance

1. `sessions.launch` creates a Session with `interactive = false` whose policy is capped by the starter, and a TypeScript process plugin can call it by name.
2. `session.create {interactive: false}` produces `interactive = false` in the Session's `SessionConfig`; `ask_user` is declared and answers that no one is available to answer. A stored record without the field loads as interactive.
3. Queue, Steer and Append written during a tool batch are consumed in the documented order, with the marker before the user content.
4. A keyed block survives compaction, a retired key does not, and a child branch inherits keys independently.
5. `input/withdraw` while InputQueue holds the input yields `input/deleted` and the input is never applied; after `input/applied` it writes nothing and replies `notHeld`; for one input at most one of `input/applied` and `input/deleted` exists. A deleted input's raw and keyed blocks never reach the model.
6. `history` with `atLeast` after a dispatched `turn/end` returns `through` ≥ that sequence.

## Appendix D. Assumptions

| # | Assumption | Basis | Effect if rejected |
| --- | --- | --- | --- |
| A1 | Every source input starts with one marker block | Existing context-block format | Each plugin defines recognition |
| A2 | Moved to the format decision in [A.3](#a3-sessionconfig-fields) | — | — |
| A3 | Process callers may call `idle` and `stop` on Sessions they started | Collaboration precedent | Another stop path |
| A4 | "Withdraw and re-submit as Steer" is written as one batch by InputQueue's `session.input.withdraw` through a further optional parameter (proposed; its shape is not defined here) | Judgment | A separate built-in |

## Appendix E. Rust binding

| Neutral | Rust |
| --- | --- |
| `sessions` | `AgentIndex` (Agents) plus the `Session` facade ([agent.rs](../../../crates/dim-agent-runtime/src/agent.rs), [branch.rs](../../../crates/dim-agent-runtime/src/branch.rs)); a generated binding exposes it by name |
| `launch` | `AgentIndex::launch(LaunchRequest) -> Result<SessionUse, AgentError>` |
| per-call acquire | `AgentIndex::acquire(id)` (existing) returning a `SessionUse` lease |
| `input` | `Session::input(InputMessageData, branch)` (existing), keys as `KeyUse::Live(name)` / `Retire(name)` |
| `lookup` | `session.input_lookup` semantics on the facade |
| `history` with `atLeast` | `Session::prefix(branch, at_least) -> Result<Prefix, AgentError>` (supplement); otherwise `Session::history`, or `Session::local_page(branch, after, limit, through)` when `after` is set |
| `withdraw` | `Session::service::<InputQueue>(branch)` then `InputQueue::withdraw(input_id, reason: Option<String>) -> Result<WithdrawOutcome, AgentError>`: appends `input/withdraw`, awaits InputQueue's decision for it; `WithdrawOutcome::Deleted { sequence }` or `WithdrawOutcome::NotHeld` |
| `stop`, `idle` | `Session::stop`, `Session::idle` (existing) |
| `SessionConfig` | Rust struct below |
| Errors | `AgentError`: `SessionClosed`, `BranchMissing`, `Reloading`, `Rejected(reason)` (including `FutureSequence`), `PublicationUnknown`, `Failed` |

```rust
pub struct LaunchRequest {
    pub agent: Option<String>,            // absent: Agents' default profile
    pub cwd: PathBuf,
    pub model: Option<String>,            // takes precedence over the profile's model
    // no source field: Agents fixes it from the calling plugin's identity (§9.3)
    pub policy: Option<PermissionPolicy>,
    pub starter: Option<SessionRef>,
}
impl AgentIndex {
    pub async fn launch(&self, request: LaunchRequest) -> Result<SessionUse, AgentError>;
}
pub struct SessionConfig {
    // existing: permission_policy, cwd, model, system_prompt, tools, collaboration_role
    #[serde(default = "default_true")]   // absent in stored records: true
    pub interactive: bool,
}
pub enum WithdrawOutcome {
    Deleted { sequence: u64 },            // sequence of input/deleted
    NotHeld,                              // unknown, applied or already deleted
}
```

Session-scoped and branch-scoped Rust plugins read the configuration as `S:SessionConfig` or `Session::config()`.
