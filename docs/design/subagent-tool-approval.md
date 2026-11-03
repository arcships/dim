# Subagent tool approval and permission inheritance

Scope: Collaboration's native member Sessions and the leader branch that delegates to them.
Business owner: Collaboration. Related requirements: #19, #29 and #257.

This design specifies Collaboration's member-approval route and the permission inheritance
required by [CLI permission controls](cli-commands.md#branch-semantics). It does not imply that
these interfaces are available in the current binary. Delivery and acceptance evidence belong
in the issues. The [CLI interaction contract](cli-commands.md#subagent-tool-approval) owns user
presentation; [Approval](../architecture/rust-agent/plugins/Permission.md#approval) owns admission.

## 1. Scope and decisions

A native member is a different Session, not a child Timeline branch. The [branch tool](timeline-branches.md#4-tree-and-roles)
continues to create leaves from `main` only; this route grants no ability to create nested tool branches. Its own Permission and
Approval admit its tools. Collaboration forwards a pending approval to the **branch that
requested the current delegation**, not unconditionally to the leader's `main` branch.
The leader model decides through Collaboration tools; allowing is itself a tool call subject
to the leader branch's current Permission and Approval. Only when that call asks does the user
receive an approval prompt. The member's raw approval is never a second prompt in the leader UI. Approving an `agent`
delegation call does not approve the member's later individual tool calls.

New native members copy the requesting branch's dispatched permission policy once. Existing
members keep their policy, including when reused. There is no live policy synchronization,
permission intersection, new permission preset, inherited per-call approval or remembered grant.
Native members cannot delegate. External member permission inheritance is outside this design;
provider asks use the same leader-side route subject to [ExternalAgent §9.3](plugins/ExternalAgent.md#93-member-approval-route-accepts-provider-asks).
Questions from `ask_user` are a separate contract, not tool approvals.

## 2. Permission inheritance

When the leader branch consumes the committed `collaboration/request` for a **new** native
member, Collaboration reads that branch's public Permission state before starting asynchronous
creation. That read is the sampling point. It copies the complete `policy` (preset,
`allowedTools`, `disallowedTools`) into the member's creation configuration under
[Permission's policy contract](../architecture/rust-agent/plugins/Permission.md#policy-contract). The member starts
its own policy version; the leader's version number is not copied. The operation retains this
snapshot across profile/model selection and Session creation waits. A later leader update affects
subsequent requests, not a creation already sampled.

Use the requesting branch, including a child branch with a policy different from `main`.
Neither the leader Session's creation configuration, the member Agent profile nor the host default
replaces this snapshot. The normal branch ancestry contract remains Permission's responsibility.
User command rules remain host-owned and read at use time; they are not copied into the policy.
A request for an existing `agentId + sessionId` neither samples nor updates its policy.

If the read fails because Permission is reloading, the branch is closing or the state cannot be
read, creation fails through the existing `member_create_failed` delivery with the cause in text.
Do not fall back to a creation policy or create a partially configured member. Cancellation before
creation follows Collaboration's existing suppressed-request cleanup. A policy read does not reserve
a member or confer authority on another Session.

Consequences visible to users:

- Changing the leader's permission preset or tool lists affects members created by later requests,
  without changing process isolation.
- Existing members retain their policy. Reusing a member does not refresh it; create a new member
  to obtain the current leader policy. Permission-switch confirmation states this explicitly.
- The leader's current policy still decides every later member-allow tool call. A member's own
  latest policy is rechecked by its Approval before the original tool starts.

## 3. Ownership and dependencies

No new plugin or wiring part is introduced. Collaboration's process directory owns team identity;
its Session part owns registered tools/methods; each member branch consumes its own Approval facts
and owns the corresponding submission resources. The leader branch creates and retires routes by
consuming its own committed source inputs. A member never calls a leader operation that mutates
that leader's route table. Branch business state belongs to the branch part, not a new process-wide
or Session-wide route cache. All resources register cleanup before use and start consumption after publication.

| Existing public dependency | Use |
| --- | --- |
| Agents' Session access and usage holds | Reach native members, retain an accepted delegation, observe public history/state and call their public services |
| Requesting branch's public Permission state | Sample a new member's creation policy |
| Member branch Dispatcher | Consume the member's own `approval/request`, `approval/respond` and `approval/cancelled` facts in order; no initial pending replay or Session-wide observation loop |
| Member Approval's respond operation | Answer the original request at decision time; a stale identity answers `Unknown`/`Cancelled`; no direct modification of Approval state |
| Branch Timeline and Dispatcher | Consume delegation, submitted-input and decision-tool facts in order |
| Session/branch input and input withdrawal | Deliver final request content and retire unapplied requests |
| ToolRegistry, PromptRegistry guide entries and Session method registry | Publish decision tools, the producer-owned `dim-agent-approval` input guide and the detail query below |
| Session and branch lifecycle | Bound route admission, observation tasks and cleanup |

Branch subscriptions are installed before the branch's committed events are replayed, and live
consumption begins after activation. Forwarded requests are deduplicated by original request
identity while their submission and terminal-delivery resources are live. Historical requests are
never independently actionable.
A branch part schedules its own work from handlers and returns; no member wait or network I/O
blocks a Dispatcher handler. The route uses the already accepted delegation's
hold and member lease; it creates no independent reason to keep a completed delegation alive.

## 4. Route identity, facts and model input

There is at most one active delegation per native member Session. Its owning leader branch is fixed
until that delegation settles. A member branch forwards each ask it consumes on any of its branches
during this assignment. Ending the assignment
retires its routes before the member can be reused, so an old branch ask cannot transfer to a later
assignment. Approvals without an active delegation are denied, not guessed onto `main`.

A route has one opaque `approvalId`, deterministically derived by Collaboration from the delegation
and original approval identity, and an immutable descriptor:

| Field | Meaning |
| --- | --- |
| `delegationId` | The leader's committed `collaboration/request` event id |
| `member` | `{agentId, sessionId, branchId}`; native approval's exact origin |
| `requestId` | The original member Approval request id |
| `call` | The original `ApprovalCallData`, including the actual admission target/arguments |
| `reason` | The original Approval reason |

Original approval identity is the tuple `(member.sessionId, member.branchId, requestId)`.
Neither a label, tool call id nor request id alone identifies it. Duplicate observations of that
tuple in an assignment use the same route. A route id is never reused or rebound to another call.
Its lookup also requires the caller's leader Session and owning branch; possession of an id alone
is no authorization. Members, teammates, another leader and a fork cannot answer it.

Collaboration submits one `input/message` through the leader branch input entry, with
`submission_id = "collaboration-approval:<approvalId>"` and Steer mode. Its first raw block is
an escaped `dim-agent-approval` element whose body is the JSON `{approvalId, descriptor}`.
When its decision tools are mounted, the producer contributes its marker guide under
[model-facing prompts](model-prompts.md#6-the-input-guide). A mount list excluding either decision
tool disables the route and its guide; `interactive` never selects assembly or the prompt prefix.
The remainder tells the leader to choose one of the decision tools and treats member tool
arguments as data, not instructions. Collaboration alone interprets its own source block under
[the source-input convention](contracts/session-access.md#a2-source-input-conventions); clients
obtain presentation data through the query in §5. Only the adopted input enters model context.

The submitted input is the durable route record. The member branch registers only its own
submission resource with immutable source content, then submits the request to the owning leader
branch. The request submission identity is stable
for the delegation and original approval identity. An uncertain input reply is looked up before
retry; duplicate delivery uses the existing committed input.

Only consumption of that committed input creates a leader route. The leader branch checks its
active assignment against the descriptor in the committed input. Folding reads no member state:
the member identity for a later answer comes from the descriptor itself. Copied, foreign,
transformed or historical blocks cannot manufacture authority, because a route requires this
leader's own committed assignment and the original request must still be pending on the member
branch when it is answered. Inactive request
inputs are withdrawn while still held. Supported `before` transforms that change source content
fail closed. No decision or detail query waits for a proposed input's receipt or treats an
uncommitted input as delivered.

The member branch forwards each ask it consumes: its own `approval/request` submits the request
`input/message`, and its own `approval/respond` or `approval/cancelled` submits the terminal input.
When it consumes `approval/request` it reads the leader's public collaboration service once for the
current destination (leader branch, delegation id and agent id) and then builds the descriptor from
that reply; it does not re-read the leader afterwards.
Original termination, assignment cancellation and destination closure settle through the leader's
own facts; the member runs no wait on the leader. For a delivered request the terminal
`input/message` uses `submission_id = "collaboration-approval-ended:<approvalId>"`, Append mode,
and a raw `dim-agent-approval-ended` element containing the same `{approvalId, descriptor}`.
One member-owned task submits the request before its terminal input, even if the member's ask
ends during request submission. If request submission fails, it sends no terminal input.
The owning leader consumes this committed terminal input to retire its route. Terminal inputs
carry no authority and do not start a model turn. A request input the leader withdraws while
still unconsumed is denied on the member branch from that same fold. Existing decision-tool
results, turn-end, interrupt and delegation settlement facts also retire routes in the leader's
own sequence.

The forwarding resource is the member branch's own termination signal; its task owns the source
and leader branch id and ends on the member's own terminal fact or branch unload.
No Approval generation is pinned; the decision tool resolves the member branch's public Approval
at decision time, and a reloaded generation answers `Unknown` for an old request id. Removing a
route under the owning branch's lock is its retirement: cleanup (terminal notice, denial and
withdrawal of a still-held input) runs once because no second caller can take the removed route.
Consumption turn and retirement are ordinary state under the owning branch's lock; no precommit binding
channel, shared completion or atomic flags are needed. InputQueue decides whether withdrawal still
applies, so cleanup does not duplicate its adoption check.

Descriptors and source identities remain in history, but a new generation retires rather than
resumes old operations. No new persistence format, reserved event key or database change is
introduced. Detail/input size limits remain those of their existing boundaries. If the complete
action cannot be delivered or rendered within them, report unavailable and deny; never approve
a truncated substitute.

Collaboration consumes existing delegation/cancel/close/result and branch turn/interrupt facts,
its request/terminal `input/message`, `input/consumed` / `input/deleted`, decision `step/tool-call` /
`step/tool-result`, and, on a member branch, its own `approval/request`, `approval/respond` and
`approval/cancelled`. Original Approval and existing tool results record the answer. The leader's
detail query reads its own committed route state. Original Approval.respond is the
answer authority and maps an already-ended request to `not_pending`; no separate foreign pending
read immediately before responding adds another answer guarantee.

## 5. Decision tools and detail query

The Session part registers the following tools for leader Sessions. An explicit tool mount list
must include them to use automatic member-approval forwarding; otherwise asks are denied with a
clear unavailable reason. Role controls declarations; `interactive` does not alter assembly.

| Tool | Input | Risk | Effect |
| --- | --- | --- | --- |
| `member_approval_allow` | `{approvalId: string}` | Unclassified (no new risk category) | Answer the bound member request with allow once after ordinary leader admission |
| `member_approval_reject` | `{approvalId: string}` | `session` | Answer that request with deny |

With standard policies and no explicit tool overrides, read-only denies allow; workspace-write
asks the user; full-access allows it automatically. Reject is available under each preset.
`allowedTools`/`disallowedTools` keep their existing precedence: an explicit allow can authorize the
allow tool, and an explicit deny can refuse either tool. There is no command string on the wrapper
for command-rule matching, and no permanent-allow operation. Member Permission and command rules
still apply to the original call and its final recheck.

Only `approvalId` is model supplied. The handler resolves its immutable descriptor from its own
branch state, checks the assignment's admission, then uses the member branch's public Approval
respond operation. That operation decides whether the original request is still pending. The model cannot replace the member, command, path or reason.
The member Approval chooses the first response in its own sequence order, including responses
from a separately attached trusted client. A tool result says the answer was accepted, not that
the original action ran. Every result writes its route id and outcome into `content`, with the target only for an
authorized route; foreign ids reveal no target.

| Result code | Meaning |
| --- | --- |
| `answered` | Original Approval accepted this answer; include `decision: allow|deny` |
| `not_pending` | Unknown/foreign/wrong-branch route, ended assignment, or request already answered/cancelled; no answer made and no foreign details disclosed |
| `unavailable` | Required service unavailable/reloading; no automatic approval or substitution |

Malformed tool input is rejected at the tool boundary. Unknown fields, empty ids and an id of
the wrong type cannot create a route or change its target. The read method applies the same
boundary validation to its identity fields.

A leader admission denial or a cancelled allow call must not leave the member suspended. Once the
owning Collaboration branch consumes that terminal tool result, it attempts deny on the original
pending request and retires the route. This cleanup is the owner's safe settlement, not a new model
action. A failed respond with an uncertain outcome is reconciled against live pending state before
any retry; never infer an accepted allow from a transport error.

The same owner registers read-only Session method `session.collaboration.approval.get`:

| Element | Contract |
| --- | --- |
| Request | `{sessionId, branchId, requestId}` naming the **leader Approval request**, not the member request |
| Reply | `{state: "pending", approvalId, descriptor}` or `{state: "ended"}` |
| Admission | Attached leader Session's normal read access; unknown/foreign requests return `ended` |
| Binding | Read the live leader Approval request, require its call to be `member_approval_allow`, resolve its `approvalId` on this branch, and require its committed route and assignment to remain active; original termination is folded from the terminal source input |
| Errors | Normal Session errors and retryable `service_reloading`; no fallback descriptor from model arguments |
| Mutation/interruption | Non-mutating; report interruption; no state change on read or disconnect |

This query supplies trusted presentation data without modifying Approval's request payload or making
Approval depend on Collaboration. It can serve any client attached to the leader, including Web/ACP;
client interaction requirements are linked in §1. A reply is a display snapshot, never a grant.
Approval and the decision handler recheck their own state at execution.

When a delivered route retires, Collaboration emits the display-only Session notice
`{source: "Collaboration", kind: "memberApprovalEnded", data: {branchId, approvalId}}` on the
leader Session, after closing that route's admission. Clients use it to retire any matching
wrapper prompt and deny that leader request if still pending. This notice carries no grant,
original arguments or execution outcome. It is not replayed; reconnect reads live status through
the query. A notice racing a detail response wins for that prompt, so an older pending reply
cannot reopen it. It uses the [existing notice contract](appserver/session-notices.md).

## 6. Cancellation, races and recovery

| Situation | Required settlement |
| --- | --- |
| Several members or parallel calls ask | Independent routes and original identities; leader can reject/allow independently; client queues leader Approval requests without overwriting one prompt with another |
| Member answers/cancels before leader decision | Retire route and withdraw its input if still held. A later decision returns `not_pending` |
| Original request ends while user prompt is open | The terminal notice retires the client prompt; detail lookup also returns `ended`; the client denies the still-pending leader wrapper. A late allow cannot target a different request |
| Input was already applied | Do not delete adopted context or stop unrelated leader work. The old input remains history; decision tools check live status and return `not_pending` |
| Leader rejects or declines to decide | Reject tool answers deny. If the leader turn that consumed the request ends with that ask unanswered, deny it; do not wait indefinitely for a model decision |
| Leader turn ends before request input is adopted | A still-pending driving input follows normal InputQueue scheduling; it can start the next turn. Applying an input just before the previous turn ends does not bind it to that turn; only `input/consumed` binds its decision turn. An interrupted branch instead withdraws it and denies the request |
| Leader stop/branch end or delegation cancel/close/result | A turn stop closes admission and retires routes only for the requests it covers: the interrupted turn's undelivered operations. Delivered delegations that keep running retain route admission. Branch end, cancel, close and settlement close admission for their own requests, withdraw unapplied request inputs and deny outstanding original asks before releasing assignment resources. Do not cancel unrelated members or branches |
| Member stop/close or Approval reload | The member branch consumes its own `approval/respond` or `approval/cancelled` and submits terminal input; the leader consumes it, retiring routes and inputs; no synthetic successful answer |
| Collaboration branch reload | The old generation retires its routes, denies pending original asks, withdraws held inputs and joins tasks before withdrawing services. The new generation starts with no routes and withdraws unapplied request inputs; a terminal input retires only a route created in that generation, never replaying an allow |
| Restart/fork | Old executions and route ids confer no authority. Native Approval performs its own abandoned-request recovery; copied leader facts do not recreate a team or pending decision |
| Leader detaches / client disconnects | Detach is not Session close. Pending requests remain answerable on another attached client; attach replays leader pending Approval requests and clients query details again |
| Leader closes while detached member work continues | Existing detached-work contract remains. Pending asks are denied; a member branch with no live assignment denies later asks instead of forwarding to a dead leader |
| Leader cannot decide (`interactive: false` or decision tools not mounted) | Before denying, send an ordinary leader message to the original member branch explaining the specific cause; create no driving leader approval input |
| Headless `dimcode exec` needs leader approval | Existing client denies it, stops/settles and exits 3; cancellation settles the member ask. No unattended allow fallback |

A definitively failed input submission denies the original ask; an uncertain reply is looked up
by submission identity before retry or settlement. If denial cannot be delivered because a service
is reloading, retain the cleanup task and lease until the request is demonstrably terminal or the Session has closed; do not report cleanup success while
it is still pending. Permanent cleanup/storage failures surface through the existing lifecycle
fault path, not a successful delegation result. No timeout, client absence or retry grants allow.

## 7. Impact and requirements raised to mechanism owners

Public additions are two Collaboration tools, one Collaboration-owned read method and one
display-only Session notice. Their consumers are the leader model and clients rendering its
approval. The source input is interpreted only by Collaboration. Permission, Approval notifications, command rules and public
Session input shapes are unchanged. Implementation updates Collaboration's owning architecture
page and generated assembly/catalog only if the actual dependencies change.

| Owner | Requirement and existing mechanism |
| --- | --- |
| CLI clients | Present trusted member details and only once/reject controls for the wrapper; defined in [CLI contract](cli-commands.md#subagent-tool-approval). Generic raw tool-input display lacks the member action, hence the read method |
| Web and ACP clients | Use the same detail query before offering a member-allow answer; [client contract](clients.md#member-tool-approval) owns their projection |
| ExternalAgent | Supply/retire provider asks through this route and answer only once, using its [existing requirement](plugins/ExternalAgent.md#93-member-approval-route-accepts-provider-asks); no native policy inheritance is imposed on an external process |

No infrastructure change is required. Existing idempotent branch input, ordered dispatch, public
Permission state, Approval respond, input withdrawal and named method registration suffice.
A decision calls the member branch's public Approval.respond at answer time; that is an existing
public service call, not a new cross-branch dependency or ownership. No `before` hook, generic
approval router or new host port is needed.

## 8. Rule check

| Rule | Application |
| --- | --- |
| Principles §1; ADR 002 D1 | Existing branch Collaboration is the Capability consuming delegation and route-input facts; Session registration and process team directory retain their own scope; member branch submission resources do not mutate leader branch state. No new business logic in Rpc |
| §4.1 | Member branches submit request/end inputs; only the owning leader's ordered consumption changes its routes. Answers and input withdrawal use their owners' public commands |
| §4.2 | Applied inputs remain adopted; the decision answers the original Approval, whose own pending check rejects a stale identity |
| §4.3 | No new `before` behavior; producers retain final committed identities |
| §4.4 | Timeline and Dispatcher do not interpret member approvals |
| §4.5 | One input commits its identity and descriptor together; no cross-Timeline transaction is claimed |
| §4.6 | Only adopted source inputs and decision tool results enter model context |
| §4.7 | Public Session state, history, broadcast and Approval operations only |
| §4.8 | Mechanisms unchanged; cross-module UI requirements are at their owning layer |
| §4.9 | Existing request/end input shapes preserve facts needed by routing and withdrawal; no new event family or UI-only durable state |
| ADR 002 D2 | No wiring plugin or second approval service |
| ADR 002 D3 | Dependencies point to existing core owners; core Approval/Permission never depend on Collaboration |
| ADR 002 D4 | No new configuration source or reload on policy update |
| ADR 002 D5 | Tools, identities, method payloads and errors are language-neutral |

## 9. Acceptance and ablation review

Observable flow verification accompanying implementation must cover:

1. New named and temporary members inherit all policy fields from the requesting branch; different
   `main`/child policies, an update before sampling, one after sampling during creation, policy-read
   failure, existing-member reuse and explicit tool overrides have the specified outcomes.
2. A member's main and child-branch asks reach only their assignment's leader branch; multiple
   members/calls, reused request ids across Sessions, duplicate replay/live observations, stale ids,
   a fork and a different leader cannot misroute or approve work.
3. Standard read-only/Ask/Full Access leader policies deny/ask/allow the wrapper respectively;
   leader/member policy tightening and command-rule changes while waiting are rechecked. The
   displayed command/path is the original request's server-owned content, never model arguments.
4. Line, scrollback and fullscreen clients display member and both branch identities, original
   action and reason; detail loading/error blocks allow, only once/reject are offered, drafts survive,
   concurrent prompts queue and cancellation removes only its own prompt. Reattach and two-client
   first-answer races close prompts correctly. Web/ACP preserve the same action information.
5. Reject, leader admission denial, no model decision, interrupted/cancelled allow, unapplied versus
   applied input, member/delegation end, detached work, reload/restart/fork and non-interactive/headless
   flows terminate the original wait. Cleanup failure is reported rather than hidden.
6. Route inputs survive storage roundtrip using the existing event format; uncertain submissions
   deduplicate by submission identity, a decision before request consumption returns `not_pending` without waiting, and no replay
   executes an allow. Copied or forged source blocks create no live authority.

Ablation: reuse the original Approval waiter and the delegation hold; remove any second answer
queue, resolved-event family, policy cache or per-member settings source. Existing idempotent source inputs record the descriptor, request receipt and original termination;
no new public event family or precommit receipt waiter is required.
The read method is retained because tool arguments contain only an opaque route id and cannot safely
supply the action the user must review. Separate allow/reject tools express different risks without
weakening reject or requiring a new risk mechanism. The terminal notice retires an open user
prompt without polling or modifying Approval. No further abstraction is required.
