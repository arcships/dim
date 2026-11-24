# Session access by id

Status: Draft. The `sessions` supplement this document used to define is replaced by [ADR 027](../../adr/027-cross-scope-access-by-id.md) and the [Agents contract](../plugins/Agents.md). What remains here are the input-source, configuration, withdrawal and fixed-prefix conventions; each is owned by the plugin listed below.

> **Offline resource nodes.** Rows about an offline resource node describe P3 multi-node behaviour. #5 is withdrawn (rutis is an external dependency, and it withdraws imported services when a link drops), so these rows are redesigned in P3 following overview §6.3: node services are looked up on each call by the Session's resource node, which P3 adds to `SessionConfig` ([A.3](#a3-sessionconfig-fields)). In P1/P2 every Session uses the control node.

## 1. Cross-scope access by id

A process-scope caller reaches any Session-group data face with `sessionId` and any branch-group data face with `(sessionId, branchId)`; a Session-scope caller reaches its own line's owners with `branchId`. The address is not limited to input writes or published-value reads. The mechanism resolves the instance key to the target scope's data-face dispatch and the caller calls the owner's own operation ([ADR 027](../../adr/027-cross-scope-access-by-id.md) D1). Nothing forwards by operation name, wraps a request or reply, or restates the owner's error. An unknown, unloaded or closing target ends resolution with the mechanism's own error.

Resolution reaches the target scope's shared data faces only. Registration points (`sessionMethods`, `toolRegistry`) and lifecycle services are not reachable this way (ADR 027 D2). The path is available to Rust parts, language runtimes and leaves through rutis sessions; clients keep using Rpc's method tables and Session authorization. Which links reach it is the deployment's ([service faces §2.1](../service-faces.md#21-目标)).

Keeping a target loaded is Agents' operation: a process caller that needs a Session to stay loaded across calls takes a lease from [Agents](../plugins/Agents.md), bound to the caller's row and settled on release or row unload (ADR 027 D4). A lease decides whether the Session is loaded; it grants no access.

| Block | Owner |
| --- | --- |
| Session creation, close, delete, fork, admission values and leases | [Agents](../plugins/Agents.md) |
| Input submission, its receipt, pending state and withdrawal | [InputQueue](../plugins/InputQueue.md) |
| Stopping a line's turn | [Loop](../plugins/Loop.md) |
| Reading a line's events and its default line | [TimelineTree](../plugins/TimelineTree.md) |
| A line's current permission state | [Permission](../plugins/Permission.md) |
| Input source conventions, `SessionConfig` fields, withdrawal and the fixed-prefix read | this document, Appendix A |

## 2. Attach expectations

`session.attach.expectedConfig` advertises an optional `expectedConfig` on `session.attach`. After authorization and acquisition, attach checks the specified fields against the Session's current default line, at the published sequence the reply's own `defaultBranch` cursor carries, before registering or replacing the subscription. An expectation can name `modelId` (an exact adopted ID), `permissionPreset`, and `reasoningEffort` (an exact adopted value, or `{action:"clear"}` to expect the model’s supported declared default at the cutoff, represented by absence when the declaration has no supported default). Model and effort read what that line adopted; the preset reads the last `permission/changed` on its ancestor chain, so a line that inherited a change and wrote none of its own answers the inherited policy. Omitted fields impose no condition. A mismatch returns `session_config_mismatch` with code `-32602`. The check makes no configuration write. Changes published after that sequence remain ordinary live changes. Fork uses [the same expectation](../appserver/session-deletion-and-fork.md#61-method) at its inherited cut, which stays on `main`: a fork copies the root's configuration, not the default line's.

## 3. Security

Input withdrawal is by the `input_id` the caller received when it wrote the input; a process caller reaches InputQueue by `(sessionId, branchId)`, a client through Rpc's Session authorization and InputQueue's `session.input.withdraw`. Markers are model-facing text: shared components (InputQueue, Agents, the runtime) never parse them; the plugin that wrote a marker may interpret its own content. Writing input confers no authority, because the resulting tool calls are admitted by Approval and Permission under the Session's policy. The policy bound of a creation is a convention for well-behaved callers, not a security boundary: the caller supplies `starter` itself, and without one the bound is the host's Session defaults ([Agents: `create_session`](../plugins/Agents.md#operations)). Inputs are replayed into model context, so callers must not put credentials or host security settings into them.

The path ADR 027 defines is available to language runtimes and leaves through rutis sessions; they are host-run trusted local code and id is not authorization (ADR 027 D2, AGENTS.md rule 11). Clients do not use it: a client reaches a Session only through Rpc's method tables and Session authorization.

## 4. Rule check

| Rule | How this document complies |
| --- | --- |
| §4 rule 1 (command events) | Withdrawal is the command `input/withdraw`; only InputQueue writes the result `input/deleted`, and a withdraw with no effect writes nothing and replies `notHeld`. |
| §4 rule 2 (handover) | `input/applied` hands an input from InputQueue to Loop; after it the input cannot be withdrawn, only interrupted with `stop/interrupt`. |
| §4 rule 3 (before callbacks only) | Entries check only param shape and authorization; no before callback is used. |
| §4 rule 4 (no business meaning in core) | `reason`, markers, keys and `submissionId` are generic data; InputQueue and Agents never interpret a source. |
| §4 rule 5 (batch) | InputQueue handles `input/message` and `input/withdraw` in sequence order; an accepted queued-input Steer commits its replacement and deletion together, with the deletion decision last (A.4). |
| §4 rule 6 (model context) | A deleted input never enters context; consumed inputs are placed at their consumed position. |
| §4 rule 7 (public events and services) | A caller reaches a Session's owners by id (ADR 027); InputQueue's withdrawal is its own public operation. |
| §4 rule 8 (requirements) | None here: the runtime-services-by-name requirement (#4) is answered by ADR 027. |
| §4 rule 9 (only decision facts in the Timeline) | `SessionConfig` and leases stay out of the Timeline; only inputs, withdrawal commands and results, and `stop/interrupt` are events; the descriptive `reason` on the required `input/withdraw` is allowed. `submissionId` is a field of the required `input/message` that callers' recovery decisions read by resubmitting. |
| §1 (Capability and App Server boundary) | InputQueue owns the withdrawal decision; callers keep their business logic in their own plugins. |
| ADR 002 D1 | InputQueue's withdrawal stays in its branch part, served to clients by its Session part. |
| ADR 002 D2 | Superseded for cross-Session access by [ADR 027](../../adr/027-cross-scope-access-by-id.md); no access host or facade plugin exists. |
| ADR 002 D3 | Callers do not declare inner services as dependencies; a call resolves the target by id and does not keep the resolution. |
| ADR 002 D4 | `SessionConfig.interactive` is creation data of the Session record, not plugin configuration. |
| ADR 002 D5 | The conventions here are language-neutral; Rust types appear only where an owner's binding appendix defines them. |

## 5. Requirements raised to mechanism owners

The runtime-services-by-name requirement (#4) is answered by [ADR 027](../../adr/027-cross-scope-access-by-id.md): the instance-key resolution of Session and branch data faces, reachable from language runtimes and leaves, without registration points or lifecycle services. No separate requirement remains here.

## Appendix A. Kept conventions

### A.2 Source input conventions

| Item | Rule |
| --- | --- |
| Marker | The first raw block is the element `<dim-<source> kind="..." ...>body</dim-<source>>` in the existing context-block format. It is model-facing text written by the producing plugin; shared components never parse it, and only the producing plugin may interpret its own content (for example Goal folding its own blocks) |
| Keys | `RawBlock.key` with `use: live` or `retire`, per branch Timeline. The writer chooses the full key; by convention it starts with the writer's own prefix (overview constraint 14), and nothing checks it |
| Submission identity | `<source>:<uuid>`, generated per logical submission. A branch accepts each identity once: a resubmission writes nothing and replies the first input's receipt, also after the input is consumed or withdrawn. A caller that lost a reply resubmits the same identity to [InputQueue](../plugins/InputQueue.md)'s submit and receives the existing receipt; error `ambiguous` means several inputs share the identity, written before branches accepted each once. The identity is the `submission_id` field of the committed `input/message`, so it commits, forks and is deleted with the input; no separate record exists ([sessions service §6.3](../appserver/sessions-service.md#where-the-submission-identity-lives), #32) |
| Never possible | Writing events other than `input/message`, `input/withdraw` through the withdrawal command and `stop/interrupt` through the line's stop; writing `input/deleted`; editing history |

### A.3 SessionConfig fields

```text
// SessionConfig (new field; existing: permissionPolicy, cwd, model, systemPrompt, tools?, collaborationRole)
{ "interactive": true }             // false: exec, automation; absent: true
{ "language": "zh-CN" }             // absent: no selection, the base rule stands
```

`interactive` means the Session has an interaction counterpart that can answer its questions and approvals: the user through a client, or another Session's or branch's agent (for example a Collaboration leader). It does not require a human. An ACP Session is interactive: the editor answers its approvals. `interactive` is fixed at creation. Assembly never reads it (ADR 002 D3, #30), so the tool set stays the same and a fork keeps the prompt prefix and its KV cache. Plugins that care read it from the Session's config directly: Questions (`ask_user` stays declared and answers that no one is available to answer) and SessionTitle (titles interactive leader Sessions only).

`session.create` gains `interactive?: bool` (default `true`), checked at the boundary only. `session.create` rejects unknown fields, so an older daemon fails a request that carries `interactive` with invalid params; clients therefore send it only when it is `false`.

A fork copies the source Session's whole `SessionConfig`; the permission preset follows with the copied `permission/changed` facts (#26).

Format decision: the native store stays on version 13. `interactive` is an additive optional field of the stored config JSON, and a stored record without it means `true`, because every Session created before the field existed was a client Session. An older daemon ignores the field and reads a non-interactive record as interactive; this is accepted. No version 14.

Implemented (C2): `session.create` sets the field, and the native store writes it only when it is `false`, so an interactive record keeps the form records had before the field existed. The omission is part of `SessionConfig`'s serde form, which only the native store serializes; exposing `SessionConfig` anywhere else (for example in a protocol reply) inherits it and needs its own decision. Its readers are Questions and SessionTitle, which read it from the Session's config. Session fork (S5c) copies the stored `config` text byte for byte, so the omission carries over to the fork.

| Stored `config` | Daemon with C2 | Older daemon |
| --- | --- | --- |
| No `interactive` (every record before C2, interactive records after it) | Interactive | Interactive |
| `"interactive": false` | Non-interactive | Interactive (field ignored; accepted) |

`language` is the operator's reply-language choice for the Session: one of `en`, `zh-CN`, `zh-Hant`, `ja`, `ko`, `id`, `de`, `fr`, `es`, `pt-BR` and `pt-PT`, the names [PromptSources §3](../plugins/PromptSources.md#3-the-agent-prompt-language-state-source) states in English. `session.create` accepts `language?`; `session.language.get` and `session.language.update` read and change it, the update taking a code or `{action: "clear"}`. Clients send the create member only when `hello.features` names `session.language`, because `session.create` rejects unknown fields on older daemons. A record without the member means no selection and produces no state block. The native store keeps version 13 and updates the member in place while the Session runs; an older daemon ignores it. A fork copies the value with the rest of the configuration. An update adopts the value on `main` and, before it replies, on every loaded branch, so a branch's next request carries it without a wake; a branch loaded later reads the current value, and each branch's periodic check is the fallback.

An update writes the record, replaces the live value and then has Prompt adopt the source. A failure before the record is written leaves the value unchanged. A failure after it, or an `interrupted` reply, leaves the new value stored, and each branch's periodic check delivers it to that branch; a client that receives an error reads the value back with `session.language.get`.

`tools` is the Session's optional mount list, fixed at creation. `session.create {tools?}` sets it; without the parameter the Session takes its Agent profile's optional `tools`, and a profile without one gives none (the product default). A profile's or member's list limits what that agent sees; a Collaboration member with a list always mounts `agent`. Without a mount list the Session declares every host tool, in the host's order, and every tool a part registers with `toolRegistry`, by name. With one it declares exactly the host and registered tools the list names, in its order; `[]` declares none of them. Other sources' tools (MCP) follow in both cases and are governed by their own configuration. A named tool that nothing has registered is not declared and is reported once; it is declared once something registers it. Duplicate names fail Session creation. Permission decides whether a declared tool's call runs and never changes the declarations ([session-boundary A.5](session-boundary.md#a5-tool-contribution)). The native store writes `tools` only when the Session has a mount list.

Not in this contract: `createdBy` (the Session's origin) and the `client` parameter of `session.create` are added when something decides with them. The resource node is a P3 addition (overview §6.3): an additive optional `SessionConfig` field, where a missing value means the control node.

### A.4 Withdrawal

| Item | Content |
| --- | --- |
| Command | `input/withdraw { input_id, reason?, steer? }`, a branch command event. `reason` is generic optional text with no business meaning, which clients show for the deleted input; events written without it are unchanged. `steer` is the checked candidate input, durably carried by the command, not an adopted input or model context. Producers: the withdrawal service (clients, process callers). It is never a fact about the input by itself |
| Result fact | `input/deleted { input_id }`, written only by InputQueue, only when it consumes an `input/withdraw` while it holds that input unapplied |
| Held | InputQueue holds an input from its `input/message` until it writes `input/applied` or `input/deleted` for it. |
| No effect | Unknown id, input already applied, or already deleted: InputQueue writes no result fact or replacement input and no failure event exists; the caller's reply is `notHeld` |
| Client path | `session.input.withdraw {sessionId, branchId, inputId, reason?, steer?}`, provided by InputQueue through `sessionMethods` (namespace `session.input`; Session-scoped part `InputMethods`, implemented in S1, that calls the branch's withdrawal service), so Rpc names no owning service → `{status: "deleted", sequence}` or `{status: "notHeld"}`. With `steer` (a `UserInput`), only a `deleted` reply also carries `steer: {inputId, sequence}` for the committed replacement, and with it `warnings?: [{check, message}]` for the submit checks' work that did not happen (Hooks A.6.2), absent when there is none. The warnings describe the replacement this call wrote, so they appear only with its receipt: a reply that wrote no replacement carries neither |
| Process and branch-part path | [InputQueue](../plugins/InputQueue.md)'s withdrawal operation, reached with `(sessionId, branchId)` by process callers and with `branchId` by branch parts: by the `input_id` returned when the caller wrote the input |
| Write path | The service appends `input/withdraw`, then replies with InputQueue's decision for that command. InputQueue's recheck uses only its own decision state (held set), never a lock on another owner's facts |
| Withdraw and re-submit as Steer | InputQueue accepts the replacement only when it accepts withdrawal of the old held input. The replacement Steer `input/message` and the old input's `input/deleted` commit in one atomic result batch, with `input/deleted` last. A `notHeld` decision creates no replacement |
| After apply | An applied input cannot be withdrawn; Loop consumes it as usual and needs no change |
| Refused `steer` | The steer is validated as the operator sent it before any submit check and before the withdrawal is decided, so a steer without operator content fails the call with `session_invalid_input`; an invalid steer for an input the queue does not hold still answers that error rather than `notHeld`, and neither the withdrawal nor the replacement is written |

#### Conditional Steer ordering

The operation is scoped to the original input id in the addressed branch, not
to its text, a client connection or a time window. For one original input,
competing withdrawals with or without `steer` share InputQueue's ordered withdrawal
decision. At most one can delete that input, and only an accepted Steer can
create a replacement. If deletion or recall wins, a later Steer cannot resurrect
the message. If application wins, the original is consumed normally and no
replacement is created. Other held inputs and explicit new submissions, including
identical text, remain independent.

InputMethods runs the existing validation and submission checks before admitting
a Steer command. A blocked check writes neither a command nor a replacement and
leaves a still-held input retryable. Passing the checks is not a reservation:
InputQueue must still decide when it consumes the command. Two callers may both
pass their checks, but cannot both replace the same original input.

The command and its result are distinct. A committed command may remain when
the decision is `notHeld`; no result or replacement is written for that decision.
The service must not publish a candidate as `input/message` before acceptance,
or delete the original and submit its replacement in separate result commits.
Replies follow publication of the accepted result batch; `sequence` identifies
the deletion and `steer.sequence` identifies the preceding replacement.

InputQueue remains the branch Capability that owns the decision; InputMethods
remains a Session method provider, not another Capability. The client-facing
entry remains a data-only RPC through `sessionMethods`; request fields,
authorization and Session method interruption/disconnect rules are unchanged.
Timeline and Dispatcher provide their existing ordering, atomic commit and
publication mechanisms, not a special Steer transaction or a new service face.
A client snapshot, debounce timer, per-connection lock or in-memory deduplication
set cannot substitute for the owner's ordered decision.

#### Recovery and compatibility

A committed command awaiting a result must retain enough durable information to
settle the checked replacement after recovery; the candidate must not become
model context merely because it is recorded. Recovery must distinguish this
case from an already committed result batch, settle the former once and never
recreate the latter. It must not rerun external submission checks to reconstruct
the previously checked content. Reload or a lost reply cannot authorize another
replacement of the deleted original; a retry returns `notHeld` without a Steer
receipt, rather than resending the accepted replacement.

This changes the meaning of the existing optional `steer` parameter: callers
must no longer expect a new receipt when withdrawal returns `notHeld`. Plain
withdrawal keeps its existing replies. Historical replacement inputs remain
ordinary committed inputs; recovery must not rewrite or remove them to enforce
the new rule retroactively. The guarantee is per branch, not a global claim
across independently forked histories.

The durable command representation is an InputQueue event-format decision, not
a new Timeline primitive. The optional command member `steer` holds the complete
checked input, including submit-check context, with mode `steer`. It is omitted
for plain withdrawals and old commands. Only the accepted result's
`input/message` adopts that content. External validation rejects an invalid
candidate before admission, regardless of whether the original is held.

The native store writes **v43** and upgrades the formats accepted by v42 without
rewriting historical events. v43 reads commands with or without `steer`; v42 and
earlier readers reject the v43 store at startup because their strict command
decoder cannot read the new member. The format gate, not a per-command fallback,
prevents an older writer from opening the upgraded store. See the
[storage owner's format decision](../appserver/native-storage-format.md#v43-conditional-queued-input-steer).
Frozen event fixtures remain unchanged.

Wire compatibility is independent of the store gate: v3 delivers the native
`input/withdraw` event, including its optional `steer` member. A client built
with the old strict command type rejects an event carrying that member; the
store upgrade cannot protect an already connected old client. No protocol
version, field-stripping adapter or fallback replacement is added. This follows
the paired client/daemon policy in
[Session boundary A.6](session-boundary.md#a6-tool-effects): clients decoding
native events must be upgraded with the daemon. Plain withdrawals keep their
previous wire shape, and the frozen v1 projection remains unchanged.

### A.5 Fixed-prefix read

The fixed-prefix read fixes `through` as the line's published head at call time. A read with `atLeast` fails `future_sequence` if that head is below `atLeast`, and otherwise returns the line's own events through that head, paged by `after` and `limit`. [TimelineTree](../plugins/TimelineTree.md)'s read provides it for every caller: a live line pages through its query; an ended line pages at its fixed head and batch boundaries. This entry has no store-read fallback for a Session the process does not hold. A live line pages through its query; an ended line pages through `TimelineTree` at its fixed head and batch boundaries. This entry has no store-read fallback for a Session the process does not hold. A branch part that needs the ancestor prefix reads the chain with the same query (§9.2). Every event delivered by the branch Dispatcher or the Session broadcast is published, so `atLeast = event.sequence` succeeds while the branch exists.

### A.6 Errors

Each owner defines its own kinds; see [Agents](../plugins/Agents.md#errors), [InputQueue](../plugins/InputQueue.md#errors) and [TimelineTree](../plugins/TimelineTree.md#errors).
