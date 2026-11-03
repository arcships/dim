# Agents `sessions` service

Status: Implemented in S2 ([#98](https://github.com/arcships/dim/pull/98)); `sessions.delete` (§6.8) was added once S5c
([#97](https://github.com/arcships/dim/pull/97)) had merged. Informed by a spike (§2); the changes made during
implementation are in §17. Roadmap item S2 ([#62](https://github.com/arcships/dim/issues/62)), after C2
([#48](https://github.com/arcships/dim/issues/48), merged) and S1 ([#58](https://github.com/arcships/dim/issues/58),
merged in [#94](https://github.com/arcships/dim/pull/94)). Contract:
[session access A.1](../contracts/session-access.md#a1-the-sessions-service). Overview:
[Appendix B.2](../overview.md#b2-app-server-process-plugins), Agents row. Scope: [ADR 002](../../adr/002-plugin-scope.md)
process scope, inside the existing Agents plugin. Consumers: [Scheduler](../plugins/Scheduler.md) (P6) and
[Workflow](../plugins/Workflow.md) (P7), both TypeScript.

Agents also provides the generated read-only `sessionPermissions` service from
its stable directory: `permission({sessionId, branchId})` returns live
`PermissionState`, and `permissionDefaults({})` returns the current host bound.
Those calls return values or throw generated `RustError`, while `sessions`
retains its existing envelope. See [native access §2](../p6-p7-bindings.md#2-read-only-permission-access).

Input ownership in §5, §6.2 and §8.3–8.4 follows the accepted
[native access contract](../p6-p7-bindings.md#4-native-report-settlement): admitted
input retains its own append/publication outcome, and the branch accepts each
`submissionId` once, so a caller that lost a reply resubmits instead of waiting
for the old writer.

## 1. Summary

Agents exposes the process service `sessions`, so a process plugin in any language can start and drive Sessions as
Collaboration does. Its methods are `launch`, `input`, `lookup`, `history`, `withdraw`, `stop`, `idle` and `delete`.
Each one maps onto Agents' existing Session creation and admission (`AgentIndex`) and the public Session facade.
`withdraw` forwards to the InputQueue withdrawal service that S1 added, and `delete` to S5c's `AgentIndex::delete`.

- **Shape.** One named process service, `sessions`, with no instance key. It is a `dyn HostDispatch` at
  `host_key("sessions")`, shared across languages through the service catalog like `pluginFiles`. Agents' directory
  part, which never reloads, provides it and looks up the Agents index on each call. Consumers therefore never reload
  with the index (#35).
- **Launch.** `launch` creates a non-interactive Session. Its permission policy is the request capped by the
  starter branch's current policy, which is the `PermissionState` folded from `permission/changed` facts. Without a
  starter, the cap is the host's Session defaults. Agents applies the cap before creation, through
  `SessionConfig.permission_policy`.
- **Input.** `input` writes `input/message` through `Session::input` and replies once the event is published. The
  branch accepts each `submissionId` once: a resubmission writes nothing and replies the first input's receipt.
  `lookup` by `submissionId` reads the same index without writing.
- **Idle.** `idle` is the facade's `Session::idle`: Loop has no open turn, nothing waits for publication, and
  dispatch has reached the tail. A pending approval or question keeps a turn open, so a caller bounds its wait with
  `stop`.
- **Withdraw.** `withdraw` forwards to S1's branch withdrawal service. S1 owns the semantics; a deletion replies once
  `input/deleted` is published, as `input` does.
- **Lifecycle.** Agents owns launched Sessions like any other Session; `sessions` neither closes nor unloads them.
  Loaded Sessions accumulated (§2, finding 6) until idle unload ([#126](https://github.com/arcships/dim/issues/126)):
  Agents now unloads a Session that no lease has held for the grace period, and the next call restores it (§8.6).
  Stored Sessions accumulate too, so a consumer deletes each run's Session when it drops the run's record, through
  `sessions.delete`, which forwards to S5c's delete (§6.8, §8.7).

No new plugin, no Timeline event, no storage format change, no runtime or facade change, and no requirement on
rutis.

## 2. Spike findings

S2 is an L-sized item, so it was spiked first ([roadmap §5.4](../roadmap.md#54-容量与重排)) on `main` at `3cca153`.
The throwaway tests and measurements were deleted; nothing from them is committed.

| # | Question | How it was checked | Finding | Effect on the design |
| --- | --- | --- | --- | --- |
| 1 | Where does the starter's current policy live? | Test: a starter created with full access, then narrowed by `Permission::update` | `Session::config().permission_policy` stays full access: it is the creation-time seed. `Session::get::<PermissionState>(branch)` reads the dispatched workspace-write policy. A Session created with a capped `SessionConfig.permission_policy` reads it back as its `PermissionState` at version 0 | The bound is read from `PermissionState`; the cap is applied at creation; no runtime change |
| 2 | Can two policies be capped? | `PermissionPreset` has no order and nothing intersects two policies. Throwaway exhaustive check of the rule in §6.1 against Permission's own `decide`: 3 × 3 presets, 16 list memberships, 2 tool names and 7 risks, 2016 cases in all | The capped policy is never more permissive than either input. It equals the pointwise minimum in 2006 cases. In the other 10, a tool appears in `allowedTools` only on the side with the lower preset, and the cap denies it where the minimum would ask or allow | The cap rule in §6.1. It is conservative on purpose |
| 3 | Can `idle` return before a just-written input runs? | Code: `TimelineContext::idle` waits for Loop `Idle`, no pending publication and dispatch at the tail. InputQueue applies an input, and Loop starts a turn, while the preceding event is being dispatched. Test, 40 runs: three Queue inputs, then `idle` | No. Every input is consumed and followed by `turn/end` at or before the cutoff. `idle` on an idle branch returns the same cutoff at once. An Append input is applied but not consumed at the cutoff | `idle` is the facade call, raced only against close and reload (§6.4) |
| 4 | What does a pending approval do to `idle`? | Test: a non-interactive workspace-write Session whose model calls `exec` | `approval/request` is written and `idle` has not returned after 500 ms. `stop` ends the turn and `idle` returns; `exec` never ran. Nothing reads `interactive` for approvals | Approvals keep `idle` open; callers bound a run with `stop` (§6.4) |
| 5 | How does a TypeScript consumer reach a Rust service? | The `pluginFiles` precedent and the rutis 0.10 sources | A `dyn HostDispatch` at `host_key(name)` plus `ServiceCatalog::register_shared(name)` is visible to JavaScript rows. `HostDispatch::invoke(method, args)` carries no caller identity. The JavaScript plugin API has only `inject`, a hard dependency with no per-use lookup. rutis 0.10 does not carry a JavaScript `AbortSignal` into a Rust call ([App Server README](../../../crates/dim-app-server/README.md#trusted-plugin-storage)) | `sessions` comes from the directory part (§5). "Source fixed by Agents" leaves A.1. Cancellation from JavaScript is raised to C4 (§14) |
| 6 | What does a loaded Session cost? | `xtask measure`, release build, mock model, Apple M4 | About 300 KB of Rust allocations and 43 Tokio tasks per loaded idle Session. Mean creation time is 1.5 ms with 10 Sessions loaded, 4.0 ms with 100 and 57 ms with 1000. The App Server never unloads idle Sessions ([#57](https://github.com/arcships/dim/issues/57)) | Launched Sessions accumulate until restart; idle unload is raised as a follow-up (§8.6) |
| 7 | When can readers see an input? | Code: `Session::input` returns at commit, before publication; protocol v3 `session.submit` waits for publication | A caller that reads right after `Session::input` can miss its own input | `input` replies after publication, and `lookup` waits for pending publication first (§6.2, §6.3) |
| 8 | Where does lookup by `submissionId` live? | Code | Only in Rpc: `session.input_lookup` scans the published pages, O(n) | Agents owns one lookup and Rpc calls it (§6.3) |

The spike changed three things: where the service is provided (finding 5), the lifecycle follow-up (finding 6), and
the contract refinements in §12.

## 3. Decisions

| # | Decision | Rejected alternative | Why |
| --- | --- | --- | --- |
| D1 | Agents' directory part, which never reloads, provides `sessions` and looks up the Agents index on each call | Providing it from the index plugin, or waiting for a per-use lookup in C4 | The index reloads whenever SessionStore, Settings or the SQLite storage does, and every consumer that injects the service would reload with it, against #35. rutis 0.10's JavaScript API cannot look up per use, and the directory part gets the same effect from mechanisms that exist today |
| D2 | One implementation on `AgentIndex`; the named service only decodes requests and encodes replies | A separate `SessionsPlugin` | That would be a wiring module (ADR 002 D2) |
| D3 | The cap takes the lower preset, the union of `disallowedTools`, and keeps an `allowedTools` entry only when both sides allow that tool unconditionally | The lower preset alone; rejecting requests wider than the bound | The preset alone lets a requested `allowedTools` entry exceed the bound, which is unsound. The contract caps a wider request rather than rejecting it |
| D4 | Without a starter, the bound is the host's Session defaults (`ServerConfig.session_defaults.permission_policy`) | A new host setting for automation | It is the host's existing answer to what a Session may do when nobody chose its policy. It adds no configuration, and a separate setting can be added later without breaking anything |
| D5 | A starter that cannot be read fails the launch: `session_closed` or `branch_missing` when it is gone, `failed` when it cannot be restored, `reloading` while its Permission reloads | Falling back to the host bound | The launch fails closed: the host bound may be wider than the starter's policy |
| D6 | `launch` replies `{sessionId, policy}` with the effective policy | `{sessionId}` only | The caller can record the authority a run got, and see that a cap took effect, without another read |
| D7 | `LaunchRequest.model` is `{providerId, modelId}` | A bare model id string | A model id is unique only within its provider. `{providerId, modelId}` is `ModelId`, the type of Agent profiles and of `session.create`'s explicit model |
| D8 | `input` replies after publication | Replying at commit | `history {atLeast: sequence}` right after the reply must succeed (finding 7) |
| D9 | `lookup` waits for pending publication, then scans; Rpc's `session.input_lookup` uses the same code | Scanning at once; a second scanner | An input that is committed but not yet published would read `unknown`, and Scheduler would mark a running input `lost` |
| D10 | `idle` is the facade's `Session::idle`, and S2 keeps today's behaviour: while an approval is pending, `idle` keeps waiting, and the caller ends the wait with `stop`. It takes no timeout | A "blocked" status, deciding in S2 what happens to asks in non-interactive Sessions (for example denying them), or `idle {timeoutMs}` | How asks in a non-interactive Session are handled is Approval's decision, tied to the Questions redesign. It is not designed yet, and S2 presumes no outcome: `idle` follows the turn however Approval settles it. Callers already own their deadlines and end runs with `stop` |
| D11 | `history` pages the branch's own events with `after`, `limit` and a fixed `through`, as `AgentEvent` records | One reply with the whole history, ancestors included | That reply has no size bound across the bridge. Each branch numbers its own sequences, so one cursor cannot page across ancestors. Launched Sessions are read on `main`, which has no ancestors |
| D12 | `sessions` has no `close` or `release`; Agents owns launched Sessions | Closing a run's Session when the caller is done; counting releases | Closing ends the Session for every user of it (design rules §4 rule 8: detaching is not closing). Unloading is a host decision, and it must cover client Sessions too (§8.6) |
| D13 | No `sessions.fork` | Adding one with S2 | It has no consumer. S5c owns creation from a cut (`AgentIndex::fork`, [#97](https://github.com/arcships/dim/pull/97)) |
| D14 | The consumer that launched a Session deletes it when it drops the last record that names it, through `sessions.delete`, which forwards to S5c's `AgentIndex::delete` (§8.7) | The user cleans up by hand; deleting the Session when its run ends | One job every five minutes adds about 288 Sessions a day to every client's list, and once a run record expires nothing ties its Session to the job. A run must stay readable while its record exists. The consumer's run retention (Scheduler A8, Workflow A9) already decides how long that is, so Sessions are bounded by it and none is left that no record names |

## 4. Structure

The call path from a consumer to a Session (a call path, not an assembly graph):

```
 Scheduler / Workflow process part              Rust process plugin
 (TypeScript row)                                       |
     | inject 'sessions'                                | ctx.get::<AgentIndex>() on each use
     v                                                  |
 +----------------------------------------------+       |
 | Agents directory part (AgentDirectoryPlugin) |       |
 |   provides dyn HostDispatch at               |       |
 |   host_key("sessions"); per call:            |       |
 |   ctx.get::<AgentIndex>(), absent: reloading |       |
 +----------------------+-----------------------+       |
                        v                               v
 +-----------------------------------------------------------------+
 | AgentIndex: launch -> host(agent), bound, cap, create_session   |
 |             delete -> delete(sessionId): S5c's gate and close   |
 |             others -> acquire(sessionId): a lease for the call  |
 +----------------------------+------------------------------------+
                              v
 Session facade: input, publication, idle, stop, sequence, local_page,
                 get::<PermissionState>(branch), service::<withdrawal>(branch) (S1)
```

| Part | Scope | Change |
| --- | --- | --- |
| `AgentDirectoryPlugin` ([agents.rs](../../../crates/dim-app-server/src/agents.rs)) | Process; no dependencies, never reloads | Provides `sessions` |
| `AgentIndex` ([agents/sessions.rs](../../../crates/dim-app-server/src/agents/sessions.rs)) | Process; reloads with SessionStore, Settings and storage | Implements the methods (Appendix A), `delete` being S5c's `AgentIndex::delete`, and the input lookup Rpc shares |
| Service catalog (App Server [lib.rs](../../../crates/dim-app-server/src/lib.rs)) | Process | `register_shared("sessions")` beside `pluginFiles`; one line in the assembly lane |
| Rpc ([connection.rs](../../../crates/dim-app-server/src/connection.rs), [connection/tree.rs](../../../crates/dim-app-server/src/connection/tree.rs)) | Process | `session.create` shares the cwd rule with `launch`; `session.input_lookup` calls the Agents lookup |
| Session facade, Permission, Loop | Session, branch | Unchanged; read through public calls |
| InputQueue withdrawal service | Branch | Added by S1 |

## 5. Service shape

| Item | Content |
| --- | --- |
| Identity | Name `sessions`, process scope, control node only, no instance key |
| Provider | Agents' directory part. Each call looks up the current `AgentIndex`; while there is none, the call fails `reloading`. Index reload ends cancellable calls, preserves own-outcome calls (§8.3), and never reloads a consumer (D1) |
| Calls | All methods are async. Each takes one request object, carried as `[request]` by rutis, and returns one envelope: `{ok: true, value}` or `{ok: false, error: SessionsError}`. The error carries `retryable` beside its `code` and `message`. An unknown method, a missing or extra argument, an unknown field or a wrong type replies `rejected` |
| Errors | `SessionsError {code, message, retryable}` (§9) |
| Cancellation | The consumer drops the call. Dropping a Rust future ends cancellable work and releases its lease. A dropped `launch` still finishes creation (§6.1); admitted `input` retains its task and lease (§6.2) |
| Rust consumers | Look up `AgentIndex` on each use, as Rpc looks up Provider, and call the typed methods in Appendix A. They may also use the named service |
| Core independence | Agents, Rpc and Session assembly never depend on a consumer of `sessions`. Rpc does not use `sessions` |
| Exposure | Only plugins in the control node's process. `sessions` is not an Rpc method and is never served to a client or exported over a rutis link |

**Input cancellation:** after §6.2 admission,
dropping any caller's result does not stop the owned input task or release its
lease early. The caller's lifetime is not tied to that task: a runtime row
unloads without waiting for it, and its next generation resubmits (§8.4).

## 6. Methods

Every method except `launch` and `delete` acquires its Session (restoring it if needed) and holds the lease until it
replies. Shapes
are the contract's ([A.1](../contracts/session-access.md#a1-the-sessions-service)), as refined in §12.

### 6.1 launch

```text
// LaunchRequest
{ "agent?": "string",                                  // Agent id; absent: the default Agent
  "cwd": "string",                                     // absolute
  "model?": { "providerId": "string", "modelId": "string" },
  "policy?": { /* PermissionPolicy */ },
  "starter?": { "sessionId": "string", "branchId": "string" } }
// reply
{ "sessionId": "string", "policy": { /* the effective PermissionPolicy */ } }
```

**Policy.** The starter's current policy is the `PermissionState` of the starter's branch: Permission's dispatched
state, folded from the Session's configured policy and the `permission/changed` facts on that branch and its
ancestors. Agents reads it once, while it holds a lease on the starter.

The policy shape belongs to the
[Permission contract](../../architecture/rust-agent/plugins/Permission.md#policy-contract).
Under [ADR 018](../../adr/018-permission-is-the-preset.md), cap compares presets and tool lists;
removed override keys are ignored when the policy object is read.

```
                         starter given?
                  yes /                  \ no
 bound = PermissionState of               bound = host Session defaults
   starter.branchId (read at launch)        (ServerConfig.session_defaults.permission_policy)
 requested = policy, else bound           requested = policy, else read-only
                  \                      /
             effective = cap(requested, bound)
                preset      the lower one: read-only < workspace-write < full-access
                disallowed  the union of both lists
                allowed     the names both sides allow unconditionally (listed in their
                            allowedTools, or that side is full-access), minus disallowed
```

The host bound today: workspace-write in the product and API-key profiles; read-only with `echo` and `wait`
allowed on the development host ([main.rs](../../../crates/dim-app-server/src/main.rs)). The cap is never wider
than either side (finding 2). A tool listed in `allowedTools` on only one side is decided by the lower preset. When
the listing side has the lower preset, that is stricter than the pointwise minimum, which would follow the other
side's preset.

**Creation.** Launch composes Agents' existing creation steps, as Collaboration does for members:

| Field of the new Session | Value |
| --- | --- |
| Agent | `agent`, else the default Agent. An unknown id fails `agent_unknown` |
| System prompt, tools | The Agent profile over the host Session defaults (`session_config`), as `session.create` without overrides |
| `cwd` | Absolute, UTF-8 and canonicalized: the rule `session.create` applies, shared as one check |
| `model` | The request's `model`, else the profile's. With a model-selection host (the product profile), it is resolved through the host Agents looks up on each call (`AgentIndex::model_selection`), as for Collaboration members; with none active, `reloading`. Otherwise the ids replace the defaults |
| `permissionPolicy` | The effective policy |
| `interactive` | `false` |
| `collaborationRole` | The default (leader) |
| `origin` | `internal`: the launch created the Session for its caller's work, so a picker leaves it out of the conversation list ([session classification](session-classification.md#32-which-creation-entry-writes-which-value)) |
| Protocol version | `PROTOCOL_VERSION` (3): written to the record, and registered with `insert_version` after creation, as `session.create` and Collaboration do. Rpc serves a loaded Session only once its version is registered, and current clients list it. Creation and registration run in one task, so they finish together even if the caller stops waiting |
| Record | Prepared creation (`AgentIndex::create_session`): project authorization, then the owned record, as for `session.create` |

Errors: `rejected` (shape, `cwd`, empty tool names in `policy`), `agent_unknown`, `session_closed` or
`branch_missing` (the starter), `reloading` (also while the starter's Permission reloads), `failed` (model
resolution, project authorization, storage, an unavailable stored Agent, a starter that cannot be restored, Session
build).

A launch is not idempotent. If the reply is lost, the result can be an empty Session that no caller knows about. It
is non-interactive and listed like any other Session. No launch lookup is added for this harmless case.

### 6.2 input

| `InputWrite` | `input/message` (`InputMessageData`) |
| --- | --- |
| `submissionId` | `submission_id`. A non-empty string; by convention `<source>:<uuid>` ([A.2](../contracts/session-access.md#a2-source-input-conventions)) |
| `mode` | `mode` |
| `rawBlocks[i]` `{text, key?}` | `blocks[i]` = `Raw {text, key: key.name, retire: key.use == "retire"}` |
| `content` | `user.content` when not empty; otherwise the input has no user part |

Agents takes the branch's publication watch and calls `Session::input`, which validates the input as for any caller
(user content or blocks, no all-empty text). It then waits until the published sequence reaches the input, and replies
`{inputId, sequence}` with the event's id and sequence.

| Outcome | Reply |
| --- | --- |
| Published | `{inputId, sequence}` |
| Not valid | `rejected`, nothing written |
| Closed Session or unknown branch | `session_closed`, `branch_missing`, nothing written |
| Committed, but the branch closed or failed before publication | `publication_unknown` |
| The index stops before admission | `reloading`; nothing written |
| The index unloads after admission | The admitted task's own outcome |

**One input per `submissionId`.** The branch accepts each `submissionId` once (A.2). `Session::input` hands the
input to the branch's Timeline, which checks the id and appends under its commit lock, so of concurrent inputs with one
id exactly one is written. The check reads an index of the branch's own inputs by `submission_id`: the branch's build
fills it from its restored history (the same copy of its own events the opening reads for its capabilities), and every
later commit adds to it. A resubmission writes nothing; `Session::input` returns the committed input, and `input`
replies its `{inputId, sequence}` once it is published, even while the first write is still unpublished. The id stays
taken after the input is consumed or withdrawn. An id that several inputs share, written before branches accepted each
id once, fails `ambiguous` and writes nothing. After `publication_unknown`, `reloading` or a lost reply, the caller
resubmits with the same `submissionId`, or reads with `lookup`.

**Input ownership.** Preparation validates the request,
acquires the Session lease and obtains the publication watch. Admission is the
synchronous handoff of that prepared input and lease to an owned task that calls
`Session::input`. At the final admission check immediately before handoff, check
index stopping and, for a call from a runtime row, that row's generation token
(captured with `caller_row()` when the call is invoked), without an intervening await. Cancellation after that decision
cannot recall an admitted task.

| Reload outcome | Reply / ownership |
| --- | --- |
| No active index, or index stop observed before admission | `reloading` (`retryable:true`); nothing written |
| The calling runtime row stopped before admission (including a call from its own cleanup) | `reloading`; nothing written |
| Index unload after admission | Await the task's own append/publication outcome; do not substitute `reloading` |
| Caller discards its result after admission, or its row unloads | Task and lease remain until settlement; the reply is lost, and a resubmission replies the same receipt |

This own-outcome lifetime applies to typed Rust and named-service callers alike. A native Rust caller has no row
and no row check.

### 6.3 lookup

`{sessionId, branchId, submissionId}` → `{status: "accepted", inputId, sequence}` or `{status: "unknown"}`; error
`ambiguous`.

`Session::lookup` reads the branch's index of `submission_id`s (§6.2). For an input it holds, it waits until that
input is published and replies it; a branch that closes or fails first ends the call with that error. Otherwise it
replies `unknown`. Every input committed before the call is in the index, so `unknown` is final for every write that
returned before the lookup began, without waiting for other pending events or scanning pages. After a restart it is
also correct: the unpublished tail was lost with the process, and the index is rebuilt from the restored history.
`ambiguous` answers only an id with several inputs written before branches accepted each id once. Rpc's
`session.input_lookup` calls the same method, and its boundary keeps parsing its own parameters. The identity is a
field of the input itself; no other record holds it ([below](#where-the-submission-identity-lives)).

#### Where the submission identity lives

Status: decided for [#32](https://github.com/arcships/dim/issues/32): `submission_id` stays a field of
`input/message`, and nothing else records it. Checked against main at `bb4028d`. The native format stays v18
([format decision](native-storage-format.md#32-格式决定)).

#32 described the field as client-protocol data that no runtime capability uses. It asked for an idempotency record
owned by Rpc or SessionStore and written atomically with the input. Since then, S2 has made the field required on
every `sessions.input`, Goal writes it through `Branch::input`, and Workflow, with Scheduler in draft, decides its
recovery from it:

| Owner | Writes | Decides from it |
| --- | --- | --- |
| Rpc `session.submit`, protocol v3 ([connection/tree.rs](../../../crates/dim-app-server/src/connection/tree.rs)) | An optional UUID | — |
| Agents `sessions.input` (§6.2) | A required, non-empty id, `<source>:<uuid>` by convention | — |
| The branch Timeline ([core/timeline.rs](../../../crates/dim-agent-runtime/src/core/timeline.rs)), through `Session::input` and `Session::lookup` (`sessions.input`, `sessions.lookup`, Rpc's `session.submit` and `session.input_lookup`) | — | Accepts each id once per branch; `accepted`, `unknown` or `ambiguous` for lookup |
| Goal branch parts ([goal.ts](../../../runtime/plugins/goal/goal.ts)) | `goal:<uuid>`, through `Branch::input` | Nothing |
| Workflow ([engine.ts](../../../runtime/plugins/workflow/engine.ts)) | `workflow:<uuid>` for each stage input and report, stored in its run file before the write | A retry, by the same or a replacement generation, resubmits the same id and settles from the reply |
| Scheduler ([A.5](../plugins/Scheduler.md#a5-timeline-and-capability-mechanics); runtime in draft [#317](https://github.com/arcships/dim/pull/317)) | `scheduler:<run_id>` and `scheduler-report:<run_id>` | `submitted` or `lost`, never a second write; a report is never written twice |
| `dimcode` and the Rust TUI ([#312](https://github.com/arcships/dim/pull/312)) | A new UUID for each submit | Nothing yet: an uncertain submit is reported to the user, not looked up |
| S5c fork ([§6.6](session-deletion-and-fork.md#66-identity-provenance-and-paths)) | Copies it with the event | — (lookup on the fork finds copied inputs) |
| InputQueue, Loop, the history projection, Goal's fold, the model | — | Never read it |

AcpGateway, RemoteGateway, Attachments and Hooks are designed to look up with `session.input_lookup` and retry with
the same id. The field is therefore no longer client-protocol data.
It belongs to the input contract ([session access A.2](../contracts/session-access.md#a2-source-input-conventions)):
an opaque correlation key that the writer chooses, scoped to the branch. The branch accepts each id once (§6.2); an
input without one is never deduplicated.

Keeping it in the event meets the constraint #32 set, and needs no new mechanism:

- **Atomic by construction.** The identity is part of the event that the append transaction commits, so it is
  durable exactly when the input is. Publication follows the commit, and lookup waits for the input it finds.
  After a restart, the restored history is the committed set and rebuilds the branch's index, so `input` and `lookup`
  decide with certainty. Flow tests: `product_submit_acknowledges_only_a_committed_input` (held native writer, Rpc restart),
  `input_replies_after_publication_and_lookup_waits_for_committed_inputs`, and the Node-backed
  `node_workflow_reports_recover_after_complete_host_restart` and
  `node_workflow_reports_reconcile_after_host_process_restart`.
- **No lifecycle rules of its own.** A fork copies the event, a delete removes it, the
  [Timeline export](timeline-export.md) carries it in the event data, lookup reads only the branch's own events, and
  compaction removes no event. A separate record would need a rule for each of these, and for every later storage
  change.
- **Design rules.** [§4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 3: when a draft
  cannot express what a writer needs, the writer asks the event owner for a generic field. `submission_id` is that
  field of InputQueue's `input/message`, opaque to InputQueue. Rule 9 limits new events, and a required event may
  carry such a field; this one is also used by later decisions, the recoveries above. Rule 4: no core part interprets
  it. Rule 8: keeping it changes no infrastructure. Moving it would change the `input/message`
  format, the facade's input signature, the Timeline append (a record that commits with a batch but is not an event)
  and every TimelineStore. The requirement for that comes from callers, and the existing contract already meets it, so
  rule 8 does not allow the change.

| Option | What it takes | Why not |
| --- | --- | --- |
| A store-owned record in the append transaction (#32's proposal) | `input/message` loses the field. `Session::input` and `Branch::input` take the id separately, and the Timeline append carries it as a record that is not an event. Each store writes it in the batch transaction. v19 fills the table from existing rows and either rewrites their bodies and before traces or keeps decoding the old field. Fork and delete gain rows to copy and remove. The export loses the ids or adds a field. `session.history`, `session.event`, `sessions.history` and the event in `session.input_lookup`'s reply stop carrying `data.submission_id`. The SDK's `InputMessageData` loses it, and Goal changes | It changes infrastructure without a requirement of its own (rule 8), changes what clients see, and is the first migration that rewrites event rows, all to get what the event already provides |
| A record kept by Agents or Rpc after the commit | A process table written after `Session::input` returns | Not atomic. After a crash between the commit and the record, lookup answers `unknown` for a written input, and the retry writes it twice |
| A separate event in the input's batch | A new event that carries the id | It is still in the Timeline, and adds an event where a field is enough (rule 9) |
| The id in the input's raw blocks | The writer puts the id in its marker block, and `input/message` loses the field | Lookup would have to parse raw blocks, which [session access A.2](../contracts/session-access.md#a2-source-input-conventions) reserves to the producing plugin. Rpc input has no marker block, and the event format still changes |
| The caller chooses the event id | The Timeline accepts a caller's id for `input/message` | The Timeline allocates ids. Ids are unique within a Session, so a repeated id would be rejected instead of replying the first input's receipt |
| An index derived from the event body | An expression index on `json_extract(body, '$.record.event.data.submission_id')` over `input/message` rows, as `timeline_tool_use_starts` does. The SQL changes, so it needs its own format decision | Not needed now: the branch builds its in-memory index from the history it already reads when it opens. It is the reopen path if that read becomes costly, and the field stays in the event |

Revisit when any of these becomes true:

1. Opening a long branch costs too much, as measured locally, once it no longer reads its whole own history. Add the
   derived index above; the field stays where it is.
2. Committed events can be removed (retention or truncation). `unknown` would then no longer mean "never written", and
   a removed id could be accepted again, so that design must say what survives the removal.

### 6.4 idle

`{sessionId, branchId}` → `{sequence}`, the branch's dispatched sequence at the first idle point after the call
starts. It is `Session::idle`:

```
              input/applied -> turn/start
   +--------+ ----------------------------> +---------+ <--+ approval or question
   |  Idle  |                               | Running |    | pending: the turn
   +--------+ <---------------------------- +---------+ ---+ stays open
       ^            turn/end dispatched          |
       |                                         | stop/interrupt
       |                                         v
       |                                    +----------+
       +------------ turn closed ---------- | Stopping |
                                            +----------+

 idle returns C = dispatched sequence when, at once:
   Loop phase == Idle  AND  nothing committed is unpublished  AND  dispatched >= published
```

What a caller can rely on at the cutoff `C`:

1. Every Queue or Steer input whose `input` reply came before the call has been applied and consumed, and its turn has
   ended at or before `C`. So has every Queue input held behind it, because InputQueue applies the next input, and
   Loop starts its turn, while the previous event is dispatched (finding 3). A `stop/interrupt` is the exception
   (items 2 and 3).
2. InputQueue normally holds no input written before `C`, so each has `input/applied` or `input/deleted` by then. It
   holds a Queue input only while a turn is open or about to start, or behind another held Queue input, and each
   `turn/end` releases the next one; an interrupted turn still ends with `turn/end`. One narrow exception: if Loop
   reloads between an input's `input/applied` and a later `stop/interrupt`, its rebuild folds the interrupt and does
   not start that input's turn, so no `turn/end` releases the Queue inputs behind it until a later turn ends
   ([input_queue.rs](../../../crates/dim-agent-capabilities/src/input_queue.rs) `starting`,
   [agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs)). A caller that finds its input with
   neither `input/applied` nor `input/deleted` at `C` withdraws it or writes again; calling `idle` again returns at
   once.
3. Some inputs can be applied but still pending at `C`. An Append input never starts a turn; an input applied before a
   `stop/interrupt` waits for the next turn.
4. A pending approval or question keeps its turn open, so `idle` waits until someone answers or the turn is stopped
   (finding 4).
5. Detached work, such as a BackgroundExec task or a `!` run, does not count.
6. `idle` covers inputs written before the call. If the branch is idle when the call starts, it returns at once, with a
   cutoff before any input written during the wait.

The facade waits on the Dispatcher, publication and Loop watch channels; `sessions` adds no polling. The call holds a
lease. Agents races the facade wait against the Session's close and the index's stopping token, because a close
settles the turn and the facade could then return a cutoff. A wait that ends while the Session is closing (close,
delete, Agent close) replies `session_closed`; a Session fault replies `failed`; an unknown branch, `branch_missing`;
an index unload, `reloading`; any other failure of the wait is a dispatch failure of the branch and replies `failed`.
There is no timeout: a caller that stops waiting drops the call, or ends the run with `stop` or `withdraw`, which also
ends the wait.

### 6.5 stop

`{sessionId, branchId, reason?}` → `{sequence}`. `Session::stop(branch, reason)` writes `stop/interrupt` with turn
scope and replies once the stopped turn has settled, with the dispatched sequence. The default reason is
`"Stopped by automation"`. Inputs that were applied but not consumed stay pending for the next turn
([Appendix B](../contracts/session-access.md#appendix-b-full-conflict-table)). A stop joins one already running for
the same turn. Errors: `session_closed`, `branch_missing`, `reloading`, and `failed` for a dispatch failure.

### 6.6 withdraw

`{sessionId, branchId, inputId, reason?}` → `{status: "deleted", sequence}` or `{status: "notHeld"}`.

Agents gets S1's InputQueue with `Session::service::<InputQueue>` on that branch, calls
`withdraw(InputWithdrawData { input_id, reason }, None)` and returns its decision. S1 replies once it has written
`input/deleted`, which is then committed but not necessarily published, so for `deleted` Agents waits for that
publication before replying, as `input` does (D8); a branch that closes or fails first replies `publication_unknown`.
S2 adds no semantics: no ownership check, since a caller withdraws by the `input_id` it received (contract §9.3), no
retry and no event of its own. S1 owns the command, the decision, the result fact and the format decision
([A.4](../contracts/session-access.md#a4-withdrawal)); §14.1 records what it delivered.

Errors: `rejected` (S1's validation: an empty `inputId`, a `reason` over 2048 bytes), `session_closed`,
`branch_missing`, `reloading`, `publication_unknown`. A `reloading` reply after the command was written leaves the
outcome to InputQueue's successor. A close that starts after the command was written also replies `reloading`, not
`session_closed`: InputQueue ends with the close before it decides, and S1 reports that as `Reloading`. That is the
accurate code, because `session_closed` promises that nothing was written, while the command is committed and is
decided from history when the Session next loads. A retry then fails `session_closed`. Once dispatch has passed the
command, which every later `idle` cutoff has, history holds exactly one of `input/applied` and `input/deleted` for an
input the caller wrote.

### 6.7 history

`{sessionId, branchId, after?, through?, atLeast?, limit?}` → `{through, events}`.

| Item | Rule |
| --- | --- |
| `through` | With `atLeast`: the published head (`Session::sequence`); below `atLeast`, `future_sequence`. Otherwise the requested `through`, which must not exceed the head (else `future_sequence`), or the head |
| `events` | The branch's own events in `(after, through]`, read with `Session::local_page`. `after` (default 0) must end a batch. A page takes `limit` events (1 to 256, default 256) when that many remain, then runs to the end of that batch, so it can exceed `limit`; it never passes `through`. With a published head or an `idle` cutoff as `through`, every batch is whole. The caller pages with `after` set to the last returned sequence until it reaches `through` |
| Form | Every event of the branch as an `AgentEvent` record, in the serde form `session.history` uses for the events it shows. Nothing is filtered, so the last sequence of a page always ends a batch |
| Ancestors | Not included. Launched Sessions are read on `main`, which has none. A way to read a child branch's prefix is added when a consumer needs one |
| Errors | `rejected` (`after` > `through`, `after` inside a batch, `limit` outside 1 to 256, `atLeast` together with `through`), `future_sequence`, `session_closed`, `branch_missing`, `reloading` |

### 6.8 delete

`{sessionId}` → `{}`. Agents runs S5c's `AgentIndex::delete`
([S5c §3.2](session-deletion-and-fork.md#32-sequence)): it checks the id, takes the Session's admission gate, closes
the Session if it is loaded, has SessionStore remove its command output and then all its rows in one transaction, and
forgets it. Calls in flight on the Session end `session_closed`, `idle` included (§6.4), and so do later calls. An
absent or already deleted Session replies `{}`. Like every `sessions` method, the call carries no caller identity, so
it does not check that the caller launched the Session (§11).

Once started, a delete runs to its own outcome, as Rpc's `session.delete` does: an index reload does not cut it short,
because the Sessions and their store outlive the index (D1), and a delete stopped halfway would leave the Session
closed with its rows until a retry. A Rust caller that drops the call can leave it partly done, as a lost Rpc reply
does (S5c §3.5); deleting again finishes it.

Errors: `rejected` (an id that is not one path component, S5c D3), `reloading` (no Agents index is active; nothing was
done) and `failed` (the command output or the rows could not be removed,
[S5c §3.5](session-deletion-and-fork.md#35-idempotence-and-errors)). After `failed`, a Session the delete closed stays
closed in this process with its rows kept (S5c D5): calls on it fail `session_closed` until a delete succeeds. A
delete is idempotent, so the caller deletes again after any error or a lost reply (§8.7).

## 7. Sequence

A run with one input, a second input withdrawn while it is held, and the wait:

```
 Caller              sessions / AgentIndex           Session S (main)            InputQueue, Loop
   | launch{starter,policy} ->|                            |                           |
   |                    lease starter, read PermissionState, cap, create S (interactive=false)
   |<- {S, policy} ----------|                            |                           |
   | input{S,main,queue,A} ->| Session::input ----------->| input/message A @a        |
   |                         | wait until published >= a  | -- dispatch ------------->| applied A, turn/start
   |<- {A, a} ---------------|                            |                           |
   | input{S,main,queue,B} ->| ... ---------------------->| input/message B @b        | B held (turn open)
   |<- {B, b} ---------------|                            |                           |
   | withdraw{S,main,B} ---->| S1 withdrawal service ---->| input/withdraw B @w       |
   |                         |                            | -- dispatch ------------->| B held: input/deleted @d
   |<- {deleted, d} ---------|                            |                           |
   | idle{S,main} ---------->| Session::idle (watches)    |                           | ... turn/end of A
   |<- {C} ------------------|                            |                           | Loop idle, dispatched >= tail
   | history{S,main,through:C,after:0} -> pages up to C: consumed A, turn/end, deleted B
```

## 8. Lifecycle

### 8.1 Ownership

Agents owns a launched Session's instance and record like any other Session. Current clients list it, and a user can
attach to it, answer its approvals, stop, close or delete it. The caller owns only its own records that name the
Session (run records) and the inputs it wrote, and deletes the Session when it drops the last such record (§8.7).
Closing or deleting the starter does not touch a launched Session, and a user closing or deleting a launched Session
only makes the caller's later calls fail `session_closed`.

### 8.2 Loading, closing and unloading

Like every Session, a launched Session stays loaded while a lease holds it or its work runs. With a native store,
Agents unloads it once no lease has held it for the grace period `idle_unload_after` (60 s by default) and it is idle
([idle unload](idle-unload.md), [lifecycle §5](../../architecture/rust-agent/lifecycle.md#5-stop-与-close)). After an
unload or a restart, the next `sessions` call restores it. A call holds a lease only while it runs, and `launch` keeps
none after its reply. `sessions` has no `close` or `release` (D12).

### 8.3 Agents index reload

AgentScope and the Sessions keep running, and the `sessions` service stays.
Cancellable calls end `reloading` when the old index stops; new calls fail
`reloading` until the next index is active. Consumers do not reload. A `withdraw`
whose reply was `reloading` is resolved with `history`. A `launch` that
has started creating its Session is not cut short: it replies that creation's outcome, because a `reloading` reply
would invite a retry while the first Session is still created (§6.1). Nor is a `delete`: it replies its own outcome
(§6.8).

Admitted `input` also replies its own outcome. All callers retain the admitted task and Session lease
through actual append/publication settlement (§6.2). Index stop observed at the
final admission check returns `reloading` without a write.
Index reload still does not reload consumers.

### 8.4 Consumer reload during a call

A consumer's old generation unloads without waiting for its `sessions` calls. An `idle` wait can continue to its own
end (§6.4), holding its lease; a lost reply does not close its Session. A `launch` or admitted `input` completes even
if its reply is lost. The new generation resubmits an input with the same `submissionId` and gets the first input's
receipt, written or still in flight (§6.2); it reads other state again with `lookup`, `idle` and `history`. A stale
`idle` ends at the Session's next idle point, the same moment a new one would. A part that awaits its own call while it
unloads, such as a C1 `drain` method or a cleanup that joins its work, waits for that call's reply; that is the part's
own work, not a `sessions` rule.

### 8.5 Delete (S5c)

A delete ([#97](https://github.com/arcships/dim/pull/97)), by a client through `session.delete` or by a process plugin
through `sessions.delete` (§6.8), takes the Session's admission gate and closes it. Calls in flight on that Session,
`idle` included, end `session_closed`, and so do later calls. A launch whose starter was
deleted fails `session_closed`. A launched Session whose starter is deleted keeps running.

### 8.6 Target: idle unload (follow-up)

Finding 6 shows that launched Sessions accumulate. A job every five minutes leaves about 288 loaded Sessions a day,
roughly 86 MB of allocations and 12,000 tasks, and each later creation is slower. The target:

- **Unload.** Agents unloads a loaded Session when every branch is idle and no `SessionUse` holds it: no attached
  client or subscription, no Collaboration member relation, no Rpc or `sessions` call in flight.
  `Session::unload_if_idle` checks the branches under their commit locks. The next `acquire` restores the Session.
- **Scope.** It applies to every Session, not only launched ones.
- **Open questions it owns.** Those #57 left open: whether detached work blocks the unload, and when to check (when
  the last lease drops, and when a Session without leases becomes idle).
- **Dependency order.** S2, then idle unload (Agents lane, [#126](https://github.com/arcships/dim/issues/126); it
  reopens S3/#57 with this measurement), then Scheduler's `create`-mode jobs at volume (P6).

S2 needs no change for it. Every call holds a lease, `sessions` keeps no state in a loaded Session, and an `idle` wait
ends at the first idle point, so a stale wait never pins a Session past it.

The design is [Idle Session unload](idle-unload.md), implemented in [#331](https://github.com/arcships/dim/pull/331).
It narrows the relation bullet above: an open Collaboration request holds its member, a relation alone does not (its
D2).

### 8.7 Deleting run Sessions

Idle unload bounds memory, but every launched Session also stays in storage and in every client's Session list. One
job every five minutes adds about 288 a day. So the consumers delete them (D14):

- **Who and when.** The consumer that launched a Session deletes it when it drops the last record that names it.
  Scheduler does so when a run falls out of its run history (A8, the last 100 runs per job) or when the job is
  deleted; a `reuse` Session is named by its job, so it goes with the job. Workflow does so when a run record falls
  out of its retention (A9, the last 100 runs); the record names every stage Session of the run.
- **Finished runs only.** A delete closes the Session and so stops its turn. A consumer deletes a run's Session only
  once the run is terminal; a run that outlives its job's deletion (Scheduler A14) has its Session deleted when it
  ends.
- **Order.** The consumer deletes the Session first and drops the record after. A failed delete keeps the record, so
  the next pass retries; a delete of a Session that is already gone succeeds, so a crash between the two steps is
  harmless.
- **Keeping a run.** A user who wants to keep a run's conversation forks it (S5c's `session.fork`). No record names
  the fork, so it stays until the user deletes it.
- **Left to the user.** A Session left by a lost `launch` reply (§6.1) is named by no record. The user removes it
  with the CLI's cleanup by age or count, composed from `session.summaries` and `session.delete` (S5c §5.1; F6,
  [#77](https://github.com/arcships/dim/issues/77)).
- **Method.** `sessions.delete {sessionId}` → `{}` forwards to S5c's `AgentIndex::delete`
  ([#97](https://github.com/arcships/dim/pull/97)): admission gate, close, then the Session's rows and files (§6.8).
  S5c's `session.delete` is an Rpc built-in, which a process plugin cannot call. Like every `sessions` method it
  carries no caller identity, so it does not check that the caller launched the Session (§11).
- **Dependency order.** S5c's `AgentIndex::delete`, then `sessions.delete` in the Agents lane (§6.8), added with its
  A.1 row once S5c and S2 had both merged (nothing was stubbed, roadmap §3.6). The consumers' deletes come next (P6
  [#70](https://github.com/arcships/dim/issues/70), P7 [#71](https://github.com/arcships/dim/issues/71)).

## 9. Errors

| Code | Methods | Meaning | `retryable` |
| --- | --- | --- | --- |
| `rejected` | All | Request shape or a boundary rule; nothing written | false |
| `agent_unknown` | `launch` | No loaded Agent has that id | false |
| `session_closed` | All but `delete` (for `launch`, the starter) | The Session is closed, deleted or unknown; nothing written | false |
| `branch_missing` | All but `delete` (for `launch`, the starter) | No such branch; nothing written | false |
| `reloading` | All | The Agents index, the model-selection host, the starter's Permission or S1's withdrawal service is not active (also when the Session closes after a `withdraw` wrote its command, §6.6), or the index unloaded during the call. For `input`, resubmit with the same `submissionId`; for `withdraw` the outcome is uncertain: `history` first. A `delete` fails `reloading` only before it starts (§6.8) | true |
| `future_sequence` | `history` | `atLeast` or `through` beyond the published head | false |
| `ambiguous` | `input`, `lookup` | More than one input has that `submissionId`, written before branches accepted each id once; nothing written | false |
| `publication_unknown` | `input`, `withdraw` | Committed, publication not confirmed; resubmit, `lookup` or `history` after recovery | true |
| `failed` | All | Session fault, storage, model resolution, project authorization, Session build; for `delete`, the Session's command output or rows could not be removed (§6.8) | false |

## 10. Timing and conflicts

| Scenario | Order | Outcome | Owner |
| --- | --- | --- | --- |
| The starter's policy changes during a launch | `permission/changed` against the launch's read | The launch uses the policy dispatched when it reads; later changes never affect the launched Session | Agents |
| Starter closed or deleted before the launch | — | `session_closed`; nothing created | Agents |
| Starter deleted after the launch | — | The launched Session keeps running | Agents |
| `idle` after an `input` reply | input published < call | Covers the input's turn (Queue, Steer) | Loop, InputQueue |
| `idle` called on an idle branch | call < input | Returns at once, before the input | Caller |
| Approval pending | turn open | `idle` waits; `stop` ends it | Approval, Loop |
| `withdraw`, then `idle` | withdraw < cutoff | History holds `input/deleted` or `input/applied` for the input | InputQueue |
| Loop reloads between an input's `input/applied` and a `stop/interrupt` | applied < interrupt, no `turn/start` | Queue inputs behind it are still held at the `idle` cutoff, until a later turn ends; the caller withdraws or writes again | InputQueue, Loop |
| Agents index reload during a call | — | `reloading`, except an admitted `launch` and a `delete`, which reply their own outcome; the consumer stays loaded | Agents |
| Consumer reload during `idle` | — | The old wait runs to its end, its reply discarded; the new generation reads again | Agents, consumer |
| Session closed or deleted during a call | — | `session_closed` | Agents |
| Two callers write one Session | Timeline order | Applied by mode in sequence order | InputQueue |
| Restart after a commit, reply lost | — | `lookup`: `accepted` if published, `unknown` if lost with the unpublished tail | Caller |

## 11. Security and authority

- **Trust.** `sessions` serves only trusted plugins in the control node's process. `HostDispatch` carries no caller
  identity, so `sessions` does not authorize callers. A trusted in-process plugin is not sandboxed by its dependency
  declarations (AGENTS.md, Agent v4 rule 11).
- **Policy cap.** The cap (§6.1) keeps a launched Session within its starter's current policy, or within the host
  defaults when the request names no starter. It is a convention for well-behaved consumers, not a security boundary:
  the caller fills in `starter` itself, and without one the cap is the host defaults, which can be wider than the
  starter's policy. Callers are trusted in-process plugins (above), so `sessions` cannot tell a run with no starter
  from one that left it out. A user can still change a launched Session's policy through `session.permission.update`,
  as for any Session.
- **Approval.** Tool calls in a launched Session go through Approval and Permission under its own policy. A run started
  by configuration (a job, a definition) needs no host-local approval
  ([ADR 003 D2](../../adr/003-approval-scope.md#d2-no-host-local-approval-for-configuration)).
- **Content.** Agents never parses markers or `submissionId`s. Inputs are replayed into model context, so callers must
  not put credentials in them ([contract §7](../contracts/session-access.md#7-security)).

## 12. Changes to other documents

[Session access](../contracts/session-access.md) (en/zh), the `sessions` part only:

- **Intro, §4, Appendix B, §8 (D3).** An Agents reload fails the calls in flight with `reloading`; callers do not
  reload (D1, #35).
- **§3.1.** The `launch` arrow loses `source` and the reply gains `policy`.
- **§7.** The policy cap is a convention for trusted callers, not a security boundary (§11).
- **A.1.**
  - The sentence "A caller's `source` is fixed by Agents …" is removed: rutis passes no caller identity, and §9.3
    already withdrew the requirement.
  - `launch` replies `{sessionId, policy}` and gains the errors `agent_unknown`, `session_closed` and
    `branch_missing`.
  - `model` becomes `{providerId, modelId}`.
  - `stop` gains `reason?`.
  - `HistoryRequest` gains `limit?`, and `HistoryPage.events` are `AgentEvent` records of the branch.
  - Every method can also fail `failed`.
  - The page links to this document for binding and lifecycle.
- **A.5.** History is the branch's own events, paged.
- **A.6.** Adds `agent_unknown`, and widens `reloading`, `rejected` and `failed` to the cases §9 lists.
- **Appendix E.** `launch` returns `LaunchReply`, not a lease; keys map to `InputBlock::Raw {key, retire}`; `lookup` is
  the shared Agents lookup; the methods are typed `AgentIndex` methods (Appendix A).

Elsewhere:

- [Overview](../overview.md) (en/zh): B.2 Agents row links this page; §5.5's `launch` arrow loses `source`.
- [Documentation index](../../INDEX.md) links this page.

During implementation (§17): A.1's `withdraw` gains `publication_unknown`, `history` rejects `atLeast` together with
`through`, A.6's `publication_unknown` covers `withdraw`, and Appendix E's `withdraw` row follows S1's merged shape.
With `sessions.delete`, the intro, §6 and A.1 list `delete` and Appendix E maps it; the Agents-reload rows of §4 and
Appendix B name the admitted `launch` and `delete`, which reply their own outcome; overview B.2's `sessions` row lists
`delete` too.

S1's withdrawal contract (A.4), the input conventions (A.2) and `interactive` (A.3) are unchanged, so no separate
contract proposal is needed.

## 13. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (command events) | `sessions` writes `input/message` through the facade, `stop/interrupt` through `Session::stop`, and reaches `input/withdraw` only through S1's service. InputQueue and Loop write the results. A withdraw with no effect writes nothing and replies `notHeld` |
| §4 rule 2 (handover) | Nothing is taken back: a withdraw is decided by InputQueue before `input/applied`, and after it only `stop` applies |
| §4 rule 3 (before callbacks) | None. The entry checks only shape; the cap is Agents' own rule for its own creation |
| §4 rule 4 (no business meaning in core) | Agents never interprets markers, `submissionId`s or `reason`; the cap is generic |
| §4 rule 5 (batch) | Each `input` is one event; `sessions` writes no batch of its own |
| §4 rule 6 (model context) | No new context path; only consumed inputs reach the model |
| §4 rule 7 (public events and services) | Uses only the Session facade, including `Session::get::<PermissionState>`, Permission's public read-only state, S1's public service and, for `delete`, Agents' own `AgentIndex::delete` (S5c) |
| §4 rule 8 (infrastructure unchanged) | No change to the Timeline, Dispatcher, event format, rutis, assembly or the Session facade. The needs are raised to S1 (a capability) and C4 (the #4 bridge) in §14; none to rutis |
| §4 rule 9 (only decision facts in the Timeline) | No new event. Leases, caller progress and launch provenance stay out of the Timeline. `submissionId` is a field of the required `input/message`; the branch accepts each id once, and callers recover by resubmitting it or through `lookup` (§6.2, §6.3, #32) |
| §1 (App Server boundary) | Agents only exposes access; scheduling and workflow logic stay in their plugins |
| ADR 002 D1 | Process scope: the service spans Sessions; it lives in Agents |
| ADR 002 D2 | No new plugin; Agents' existing directory part provides the service |
| ADR 002 D3 | Consumers depend on Agents; no core part depends on a consumer. The per-call index lookup keeps consumers out of index reloads (#35) |
| ADR 002 D4 | No configuration added; the host bound is the host's existing Session defaults |
| ADR 002 D5 | Neutral contract in §6 and A.1; Rust binding in Appendix A; errors as data; cancellation by dropping the call |
| ADR 003 | The launch cap keeps a well-behaved consumer within its starter's policy, a convention rather than a security boundary (§11); tool calls in launched Sessions are admitted by Approval and Permission under the launched Session's own policy |

## 14. Requirements raised to mechanism owners

### 14.1 S1, InputQueue withdrawal (eric8810, [#58](https://github.com/arcships/dim/issues/58))

Delivered in [#94](https://github.com/arcships/dim/pull/94) as `InputQueue::withdraw(InputWithdrawData { input_id,
reason }, steer: Option<InputMessageData>) -> Result<(Withdrawal, Option<Arc<AgentEvent>>)>`, with
`Withdrawal::{Deleted { sequence }, NotHeld}`. It replies once its decision is written, and fails `InvalidInput` (its own
checks) or `Reloading` (InputQueue unloaded after the append). `sessions` passes no `steer`. The table is the request as
it was raised; §17 lists what S2 adapted.

| Item | Content |
| --- | --- |
| Blocked by | `sessions.withdraw` must forward to InputQueue's decision; A.4 and Appendix E name the service but it does not exist (InputQueue has no public service today) |
| Existing mechanisms considered | Writing `input/withdraw` from Agents: Agents would have to wait for and interpret InputQueue's result (§4 rules 1 and 7). `session.input.withdraw` through `sessionMethods`: a client method, not reachable from a process plugin without a connection |
| Generic capability | A branch-scoped public service (a `PublicService` whose name is registered in the branch group) reachable from outside the branch through `Session::service`, with `withdraw(input_id, reason: Option<String>) -> Result<WithdrawOutcome, AgentError>`. It appends `input/withdraw` and resolves with InputQueue's decision for that command only: `Deleted { sequence }` once `input/deleted` is dispatched, or `NotHeld` with nothing written. It fails with `Closed`, `UnknownBranch`, `Reloading` (InputQueue reloading after the append; the successor decides from history) or `InvalidInput` (its own checks, including the `reason` limit, defined once with the event). It decides correctly on a restored Session, from the held set rebuilt from history |
| Effect on sequence, batch, interrupt, reload | None beyond A.4: one command event, decided in sequence order |
| Owner and issue | S1, [#58](https://github.com/arcships/dim/issues/58). The `input/withdraw`/`input/deleted` format decision is S1's; S2 needs none |

### 14.2 C4, TypeScript bridge (zerob13, [#63](https://github.com/arcships/dim/issues/63))

| Item | Content |
| --- | --- |
| Blocked by | ADR 002 D5 defines cancellation as the consumer dropping the call. rutis 0.10 does not carry a JavaScript `AbortSignal` into a Rust call, so a TypeScript caller cannot abandon a pending `sessions.idle` (§8.4) |
| Existing mechanisms considered | `pluginFiles`' extra cancellation Promise argument: a convention of one service, not a binding rule. `stop` or `withdraw`: they end the run as well as the wait |
| Generic capability | In the neutral binding, a TypeScript caller that abandons a call to a Rust-provided shared service drops the Rust future. The binding also keeps the App Server envelope (§5) for process services, so `SessionsError` arrives as data |
| Effect on sequence, batch, interrupt, reload | None: dropping a wait writes nothing |
| Owner and issue | C4, [#63](https://github.com/arcships/dim/issues/63) / [#4](https://github.com/arcships/dim/issues/4). Not blocking S2: without it, waits end as in §8.4 |

### 14.3 Agents lane: idle unload (follow-up)

| Item | Content |
| --- | --- |
| Blocked by | Launched Sessions are never unloaded (#57), and each costs about 300 KB and 43 tasks while it stays loaded (finding 6) |
| Existing mechanisms considered | Closing runs: ends the Session for every user of it (D12). `Session::unload_if_idle`: exists, but has no caller |
| Generic capability | §8.6: the host unloads a loaded Session when it is idle and no lease holds it; `acquire` restores it |
| Effect on sequence, batch, interrupt, reload | None on the Timeline: idle release records no events, and the next use restores the Session |
| Owner and issue | Agents lane (yyhhyyyyyy), [#126](https://github.com/arcships/dim/issues/126), reopening S3 ([#57](https://github.com/arcships/dim/issues/57)); due before Scheduler's create-mode jobs ship |

Nothing is raised to rutis.

## 15. Notes for the consumer designs

For the Scheduler and Workflow design reviews (zerob13); S2 does not edit those documents:

- **Rust binding.** Their Appendix G's `AgentIndex::launch -> SessionUse` and `service::<InputQueue>` become the named
  service (§5); `SessionUse` is internal to Agents.
- **Model.** `model` is `{providerId, modelId}`.
- **Starter.** The cap holds only for a consumer that passes `starter` whenever a Session started the run (§11): the
  Session that created a Scheduler job (A15), or a Workflow run's starter branch.
- **Policy reads.** `sessions` has no policy read. A launch caps the policy and replies with the effective one (D6). A
  cap captured at save (Scheduler A15) or Workflow's start-time check needs the starter's policy from the calling
  Session's own part, or a read method proposed in their review. The same holds for Scheduler's reuse check.
- **History.** `sessions.history` pages with `after` and `limit`.
- **Reloads.** `inject: ['sessions']` does not reload with the Agents index (D1), so a run no longer has to end
  `interrupted` on an Agents reload; an in-flight call fails `reloading`, except an admitted `launch` and a `delete`,
  which reply their own outcome (§8.3).
- **Loaded Sessions.** Agents unloads a run Session that no lease has held for the grace period, and the next call
  restores it (§8.6, [#126](https://github.com/arcships/dim/issues/126)). `launch` is not idempotent (§6.1).
- **Deleting run Sessions.** A requirement on both designs (§8.7): Scheduler and Workflow delete each Session they
  launched when they drop the last record that names it (Scheduler: a run leaving the last 100 of its job, or the job
  deleted; Workflow: a run leaving the last 100), once the run is terminal, Session first and record after, through
  `sessions.delete`. Users keep a run by forking it, and remove the Sessions of lost `launch` replies with the CLI's
  cleanup.
- **Waiting again.** "Not consumed yet, wait again" must not loop on `idle`: on an idle branch it returns at once.
  An input with neither result at the cutoff is held (§6.4, item 2).

## 16. Implementation and acceptance

Implemented on [#98](https://github.com/arcships/dim/pull/98) after S1 merged, and `sessions.delete` after S5c; nothing
was stubbed (roadmap §3.6).

1. Done. Types and the seven `AgentIndex` methods in the Agents lane (Appendix A).
2. Done. The directory part provides `sessions`; App Server `lib.rs` registers `register_shared("sessions")` (one line
   in the assembly lane).
3. Done. Lookup is the branch's (`Session::lookup`, §6.3); `sessions.lookup` and Rpc's `session.input_lookup` both
   call it.
4. Done. `xtask graph` records the new module and the `sessions` binding; `xtask check` passes.
5. Done. `sessions.delete` forwards to S5c's `AgentIndex::delete` ([#97](https://github.com/arcships/dim/pull/97))
   and has its row in A.1 (§6.8).

The flow tests are in
[transport_flow/sessions.rs](../../../crates/dim-app-server/tests/transport_flow/sessions.rs). `serve` takes no extra
process rows, so a test Session part injects `sessions` by name (`Keyed<dyn HostDispatch>`, as `plugin_files_flow`
does) and the test calls it, exercising the neutral path. The plan's cases map to them as follows:

1. **Issue #62's done criterion** (`a_consumer_launches_writes_waits_and_withdraws_by_input_id`). The plugin launches a
   Session, writes input, waits for idle and withdraws a held input by `input_id` (`deleted`); a withdraw after
   `input/applied` replies `notHeld`.
2. **Launch** (`launch_caps_the_policy_by_the_starters_current_policy_or_the_host_defaults`). With a starter narrowed
   by `permission/changed`, the launched Session is non-interactive and its
   `PermissionState` equals the cap, which the reply also returns. Without a starter: a full-access request is capped
   by the host defaults, and no policy means read-only. An unknown Agent fails `agent_unknown`, a bad `cwd` fails
   `rejected`, and a closed starter fails `session_closed`.
3. **Input and lookup** (`input_replies_after_publication_and_lookup_waits_for_committed_inputs`). The marker comes
   first and the user content after it. `history {atLeast: sequence}` succeeds right after the reply. `lookup` returns
   `accepted` for the written id and `unknown` for another. A resubmission with the same id replies the first receipt
   and writes nothing; another id is written. With publication held by a
   locked native store, the `input` reply and a `lookup` started after the commit both wait, and the lookup then reads
   `accepted`.
4. **Idle** (`idle_covers_written_turns_and_waits_on_a_pending_approval_until_stop`). The cutoff covers each input's
   consumption and its `turn/end`. With an approval pending, `idle` stays open until `stop`.
5. **Reload and close** (`idle_ends_reloading_on_an_index_reload_and_session_closed_on_close`). An Agents index reload
   during `idle` ends it with `reloading`; the test part stays loaded and its next call succeeds. A close during `idle`
   ends it with `session_closed`. A reload while a `launch` waits on its locked native store
   (`launch_admitted_before_an_index_reload_replies_the_created_session`) replies the created Session.
6. **History** (`history_pages_the_branch_through_a_fixed_sequence_on_batch_boundaries`). Pages with `limit` 1 chain to
   the fixed head and equal the branch's events, a page finishes a two-event batch, and a cursor inside that batch,
   `future_sequence` and the other boundary errors are checked.
7. **Delete** (`a_consumer_deletes_its_launched_session_and_retries_a_failed_delete`, on a disposable native store).
   The plugin deletes a loaded launched Session while `idle` waits on its pending approval: the wait ends
   `session_closed`, the Session and its rows are gone, and later calls on it, a launch with it as starter included,
   fail `session_closed`. Deleting it again, or an id never created, replies `{}`; an id that is not one path component
   and a request with an extra field are `rejected`. With the native writer held, an Agents index reload during a
   delete does not end it: it replies `{}` once the writer is free, and the rows are gone. A delete that cannot remove
   the Session's command output replies `failed`; the Session stays closed with its rows, and the retry deletes it.
8. **Review follow-ups of #98.**
   - `a_launch_fails_reloading_while_the_starters_permission_reloads` (D5): the test binds Permission's command rules
     through a Session part that reloads with the Agents index and holds its reload. While the starter's
     `PermissionState` is absent, a launch with that starter fails `reloading` instead of using the host bound; once
     Permission is back, the same launch succeeds.
   - `a_launch_resolves_its_model_through_the_model_selection_host` (§6.1, `model`): with configured models, the
     request's model, else the catalog default, is resolved through the host and carries the catalog's model defaults;
     a model the host cannot resolve fails `failed` and creates nothing. The host's `reloading` case is not reachable
     from a flow test: `serve` restarts neither model-selection provider.
   - `a_dropped_launch_still_registers_its_session` (§17 #6): a launch dropped while its creation waits on the held
     native writer still creates the Session, and Rpc attaches it, which needs its registered protocol version.
   - `input_and_withdraw_reply_publication_unknown_when_the_store_fails_the_write` (§17 #1): the native store fails the
     write of an input, then of a withdraw's `input/deleted`. Both reply `publication_unknown`, so neither replies
     before its event is published.

## 17. Design changes during implementation

Each change stays within S2 (roadmap §5.3); the contract's `sessions` part is updated to match.

| # | Change | Why |
| --- | --- | --- |
| 1 | `withdraw` waits for the publication of `input/deleted` before replying `deleted`, and can reply `publication_unknown` | S1 replies once the deletion is committed, not once it is dispatched as §14.1 asked. Without the wait, `history {atLeast: sequence}` right after the reply could fail |
| 2 | The binding uses S1's merged names: `Withdrawal` and `InputQueue::withdraw(InputWithdrawData, None)`, not `WithdrawOutcome` and `withdraw(input_id, reason)` | The shape S1 merged (§14.1) |
| 3 | Each typed method takes the request type of its wire shape (`LookupRequest`, `WithdrawRequest`, `StopRequest`) instead of positional arguments | One type per shape; the named service only decodes and encodes |
| 4 | `history` rejects `atLeast` together with `through`, and returns every event unfiltered | The two set `through` in conflicting ways. A filtered page could end inside a batch, and the next `after` would be rejected |
| 5 | A failed `stop` or `idle` wait that is not a closed Session, a missing branch or a reload replies `failed` | Those failures are the branch's dispatch failures. Mapping a handler's `InvalidInput` to `rejected` would blame the caller |
| 6 | `launch` creates and registers the Session in one task that outlives the call, and once that task starts, the reply waits for it rather than the index reload | A dropped call must not leave a loaded Session that clients cannot see, and a `reloading` reply must not invite a duplicate launch |
| 7 | The code is the submodule [agents/sessions.rs](../../../crates/dim-app-server/src/agents/sessions.rs) | `agents.rs` keeps the directory and the index |
| 8 | The flow tests reach `sessions` from a Session part | `serve` takes no extra process rows; the part injects the service by name, as a process plugin does |
| 9 | `history` rejects a `limit` above 256, its default | Review of #98: D11 pages to bound a reply across the bridge, and an unbounded `limit` returned the whole branch in one page. Collaboration's `agent` read also rejects a `limit` above its default |

### 17.1 Stored ownership before index publication

Status: Implemented in [#327](https://github.com/arcships/dim/pull/327), correcting
[#318](https://github.com/arcships/dim/issues/318). Before it, the implementation
published `AgentIndex` before `serve` or `restart` called `install_default`. That
interval let a process owner query a healthy stored Session before its owning Agent
and protocol-version mapping were loaded. `acquire` then returned a non-retryable
storage failure, which could incorrectly settle Scheduler recovery as `lost`.

Use the existing initialization routine and publication boundary:

1. `AgentsPlugin.apply` constructs the index and registers its existing cleanup.
2. Preserve the existing `config.session_execution` condition. When execution is
   enabled, await `index.install_default()` before `ctx.provide_as::<AgentIndex>`:
   load stored Agents, record unavailable entries and install ownership mappings
   before publishing the index. When execution is disabled, skip this
   initialization and keep Session admission disabled until the product execution
   profile is installed.
3. Remove the post-publication `install_default` calls from `serve` and
   `RunningServer::restart`, and the stale comment that an Agent scope injects
   the index. Startup, restart and dependency-driven Agents reloads use the same
   owner initialization path.

The current `AgentScopePlugin` depends on `AgentDirectory` and `Loader`; its
`Sessions` child has no dependencies. Neither declares `AgentIndex`, so the
proposal does not introduce a dependency from the index to its own published
service. The pinned rutis-loader 0.16.0 `Loader::create_instance` supports calls
from a plugin's `apply`; `AgentIndex::load` already uses that operation. Verify
this bootstrap path during implementation rather than adding a
readiness service, gate, flag, event or consumer retry policy.

Until publication, the stable `sessions` service uses its existing `reloading`
reply. Callers retain that reply's existing retry behavior; Scheduler recovery
retries it without marking a healthy stored Session lost. A returned
initialization error follows the existing required-provider failure path.
Per-record unavailability is recorded and allows other stored owners to load,
subject to the process inactivity policy below; failures reading the stored Agent and
owner records remain errors.
An index reload retains the Agents and Sessions in `AgentDirectory`.

During `serve` and `RunningServer::restart`, process assembly follows the scoped
inactivity policy in [How Dim uses rutis](../rutis-usage.md#appendix-a-rust-binding).
Continuous row activation can keep initialization alive beyond one interval.
A stalled process opening fails and cleans up; no partially initialized index
is published. An individual stored Agent retains its existing creation timeout:
when it expires while process initialization can still proceed, the Agent is
recorded unavailable and other owners continue loading. Dependency-driven Agents
reload uses the same per-Agent policy. Returned initialization errors remain
errors. No fixed total startup or restart budget is added.

Acceptance covers execution-disabled hosts without default or stored-Agent
loading, execution-enabled empty and populated stores, unavailable records beside
healthy owners, stalled-process failure cleanup, and a stored Agent's creation
timeout during progressing startup. Restart and dependency-driven reload must
record a not-running stored Agent's creation timeout as unavailable and continue.
Index reload must preserve loaded Sessions.
Query during initialization must return
`reloading` until publication and must not permanently fail a healthy stored
owner. The strict full-host Scheduler recovery regression in #318 must pass.
No Session input, permission, record format, bridge or rutis contract
changes are included; Scheduler and Workflow keep the existing S2 interface.

## Appendix A. Rust binding

The types live next to `AgentIndex` in [agents/sessions.rs](../../../crates/dim-app-server/src/agents/sessions.rs). A Rust
plugin in another crate uses the named service.

```rust
pub struct SessionRef { pub session_id: String, pub branch_id: String }
pub struct LaunchRequest {
    pub agent: Option<String>,
    pub cwd: String,
    pub model: Option<ModelId>,
    pub policy: Option<PermissionPolicy>,
    pub starter: Option<SessionRef>,
}
pub struct LaunchReply { pub session_id: String, pub policy: PermissionPolicy }
pub struct InputWrite {
    pub session_id: String, pub branch_id: String,
    pub mode: InputMode, pub submission_id: String,
    pub raw_blocks: Vec<RawBlock>, pub content: Vec<ContentBlock>,
}
pub struct RawBlock { pub text: String, pub key: Option<BlockKey> }
pub struct BlockKey { pub r#use: KeyUse, pub name: String }   // KeyUse: Live | Retire
pub struct InputReceipt { pub input_id: String, pub sequence: u64 }
pub struct LookupRequest { pub session_id: String, pub branch_id: String, pub submission_id: String }
pub enum LookupReply { Accepted { input_id: String, sequence: u64 }, Unknown }
pub struct HistoryRequest {
    pub session_id: String, pub branch_id: String,
    pub after: Option<u64>, pub through: Option<u64>,
    pub at_least: Option<u64>, pub limit: Option<usize>,
}
pub struct HistoryPage { pub through: u64, pub events: Vec<Arc<AgentEvent>> }
pub struct WithdrawRequest {
    pub session_id: String, pub branch_id: String,
    pub input_id: String, pub reason: Option<String>,
}
pub struct StopRequest { pub session_id: String, pub branch_id: String, pub reason: Option<String> }
pub struct SessionsError { pub code: SessionsErrorCode, pub message: String }  // retryable() follows the code

impl AgentIndex {
    pub async fn launch(self: &Arc<Self>, request: LaunchRequest) -> Result<LaunchReply, SessionsError>;
    pub async fn input(&self, write: InputWrite) -> Result<InputReceipt, SessionsError>;
    pub async fn lookup(&self, request: LookupRequest) -> Result<LookupReply, SessionsError>;
    pub async fn history(&self, request: HistoryRequest) -> Result<HistoryPage, SessionsError>;
    pub async fn withdraw(&self, request: WithdrawRequest) -> Result<Withdrawal, SessionsError>; // Withdrawal from S1
    pub async fn stop(&self, request: StopRequest) -> Result<u64, SessionsError>;
    pub async fn idle(&self, at: SessionRef) -> Result<u64, SessionsError>;
    /// S5c's delete, served as `sessions.delete {sessionId}` (§6.8).
    pub(crate) async fn delete(&self, id: &str) -> dim_agent_runtime::Result<()>;
}
```

Request types derive `Deserialize` with camelCase fields and `deny_unknown_fields`, and reply types derive `Serialize`.
The named service decodes each request object (A.1) into its request type, calls the method and encodes the envelope;
`withdraw`, `stop` and `idle` replies are encoded there as `{status, sequence?}` and `{sequence}`. For `delete` it
decodes `{sessionId}` into Rpc's `protocol::SessionId`, maps the `AgentError` as `From<AgentError>` does (§9) and
replies `{}`. The cap is a function
of two `PermissionPolicy` values next to `launch`. Each method's lease is a private `SessionUse` from `acquire`. `input`
and `lookup` call the facade's `Session::input` and `Session::lookup`, which Rpc's `session.submit` and
`session.input_lookup` also call.
