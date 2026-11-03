# Permission

The Permission plugin registers two branch parts: Permission, which decides each call, and Approval, which admits it or asks the user.

## Permission

Owner: [permission.rs](../../../../crates/dim-agent-capabilities/src/permission.rs).
Evaluates tool risk, the fixed file-access facts, explicit allow/deny policy and
the user's command rules, and exposes versioned branch-local permission state.
The facts service is owned by the Session part in
[file_access.rs](../../../../crates/dim-agent-capabilities/src/file_access.rs)
([ADR 014 D1](../../../adr/014-file-access-facts.md#d1-fixed-facts)).

`PermissionState` is serializable for read-only generated host exports. Agents'
`sessionPermissions.permission` reads this same dispatched state through the
Session facade and holds its Session lease; it adds no Permission dependency,
cache or event.

### Policy contract

The policy is the preset and two tool-name lists:

| Field | Values |
| --- | --- |
| `preset` | `read-only`, `workspace-write`, `full-access` |
| `allowedTools` | tool names that run without the preset's question |
| `disallowedTools` | tool names that always deny; a deny wins over any allow |

`preset` answers both what may run and what is asked first, and it is the policy of record
([ADR 018](../../../adr/018-permission-is-the-preset.md)): `read-only` denies writes and
process/network/unclassified risks, `workspace-write` allows workspace writes and Session tools,
asking for the rest, and `full-access` runs every call without asking. Tool lists refine that,
one name at a time; they never lift an explicit deny, a file check or process isolation.

The type ignores keys it does not name. Policies written while ADR 017's `fileAccess` and
`toolApproval` existed still load, with those keys dropped: nothing compares or migrates them, and
a record that ran under a stricter override admits by its preset. Session configuration and
`permission/changed {policy}` carry the same type; versions, dispatch, ancestry and restoration
rules are unchanged. A client updating the preset sends the tool lists it read, unchanged.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionConfig` | Seed the initial permission policy. |
| `S:CommandRulesHost` | Read the user's command rules when deciding a command call. |
| `S:SessionIdentity` | Name the Session in the facts query. |
| `B:Dispatcher` | Replay local policies, apply committed policy changes and settle update waiters. |
| `B:Timeline` | Write changes and read ancestor policies. |

`S:FileAccessFacts` and `P:HostPlatform` are looked up when a file call is
decided, after the part is active, not while it loads; a composition without the
App Server facts part decides a write by the workspace alone and keeps a read's
file-scope fallback, and one without HostPlatform keeps the fallback for relative
targets. Neither is an injected dependency, so their absence does not gate the
part.

### Tool advice

A branch part may register an advisor with the branch's Permission while it
runs, with `Permission::register_advice(ctx, name, advise)`; the registration
is an effect of that part, so it is withdrawn when the part unloads, and a
second advisor of the same name fails. Hooks' `HooksBranch` registers `hooks`
for `PreToolUse` ([Hooks A.6.1](../../../design/plugins/Hooks.md#a61-tool-advice-in-permission)).

`Approval::admit` calls `Permission::admit_decision(call, source, risk, cancel)`,
and only the admission decision asks advisors: `decide` stays the plain
decision, so the recheck after an answer, and every other caller of `decide`,
asks none. `admit_decision` settles policy, command rules and file facts
first; when its own result is not a deny it asks the advisors registered at
that moment, in name order, each with the call, its source, its risk and its
own `allow` or `ask`. Advice combines as any deny, then any ask, then any
allow, and without advice the decision stands: advice never overrides a deny,
because it is not asked then. An advisor error counts as no advice.

Cancelling the caller's batch drops a pending advice, so the admission fails
as cancelled without waiting for the advisor to look at its token, as a
cancelled command-rules read does. An advisor withdrawn while its advice is
pending has that advice dropped and counted as none, so Permission decides
alone. ToolExecutor and the event formats are unchanged: a denied call's error
text is the advisor's, written by ToolExecutor in its existing batch result.

### Events and lifetime

Produces and consumes `permission/changed`. Updates become effective when
dispatched, not merely appended. Replay starts with Session policy, folds the
fixed ancestor prefix and then local changes. No background tasks. Cleanup
closes admission and drops pending waiters, which report Closed. Reload builds
a new state cell from history. Policy is admission logic, not OS isolation;
explicit disallow overrides allow.

### Model-facing texts

`Command denied by command rule …`: the refusal an admitted-and-denied call
returns, and it is protocol
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)).
The text names the rule and what it matched, which the model uses to change the
command rather than retry it; `Command rules could not be read: <error>` is
protocol too, because it says the rules were not applied and a retry may be
decided differently.

### File-access facts

[ADR 014](../../../adr/014-file-access-facts.md) fixes what a file operation is
decided from. The `FileAccessFacts` Session part registers `fileAccessFacts`
with `facts(session_id, fileAccess, path, mode, cancel) -> FileFacts`: `workspace` from
`SessionConfig` and the Session's confirmed worktree roots, `system` from HostPlatform's platform roots, `owned` from `SessionFiles.owns`, and `grant` from the Session's grants.
The facts part requires SessionConfig, SessionIdentity, SessionLifecycle, HostPlatform,
SessionFilesHost and SessionGrantsHost. Only external directory membership is cached;
permission decisions are read at use time.
The facts part consumes the association port and imports no SessionStore. Ownership or grant
query failure denies; discovery unavailability preserves approval with its reason.

For a file call, Permission resolves the call's path to the same canonical target
the tool owner admitted and calls the service with the effective file access and the
call's mode (`read` for `read`, `grep` and `glob`; `write` for `write` and
`edit`). It uses WorkspaceTools' `resolve_path` for both absolute and relative
targets, so cwd, `~`, `..` and symlinks are settled before the query; a
`grep` or `glob` without a path targets the Session cwd. A composition without
HostPlatform cannot expand `~` or join a relative target and keeps the fallback
rules below. The path check runs before tool allows, so an
allowed tool does not bypass a path fact; a resolution failure refuses the call.

File access follows the preset:
`read-only` → Read only, `workspace-write` → Workspace read/write, `full-access` → Unrestricted.
The fixed facts themselves do not depend on tool approval or sandbox state.

| File access | Read scope | Write scope |
| --- | --- | --- |
| Read only | Workspace, system roots, Session-owned artifacts and grants pass; others ask | Denied, even with an explicit tool allow or write grant |
| Workspace read/write | The same read facts pass; others ask | Workspace or matching write grant passes; system roots and Session-owned artifacts denied; other targets ask |
| Unrestricted | Passes without a facts query | Passes without a facts query |

Tool allows and command allows cannot answer a file-scope question or lift a file denial.
A read grant never permits a write.
Without the facts service a read has the legacy unrestricted read-scope fallback, then the tool
gate applies; workspace writes use the fallback below.

### On-demand worktree membership

[Worktree access](../../../design/worktree-access.md) extends the workspace fact with Session-local,
Git-verified roots. FileAccessFacts owns discovery, shared in-flight probes and root identities;
its optional process dependency `worktreeAssociations` persists a repository/root and Session subtree
atomically before publication. Without this port it keeps the original workspace boundary.
The service reports discovery unavailability separately from failed ownership/grant reads: the former
keeps the existing approval path with a reason, while the latter still denies. Cancellation is not denial.
Permission samples its dispatched policy again after file facts finish, so tightening during discovery
applies to the waiting call. Explicit denies and already decisive facts do not trigger discovery.

The Session service supplies the original workspace and confirmed roots for process preparation, recognizing the
resolved explicit cwd first. It verifies cached directory identities and fixed subtree prefixes before
returning roots. Closing cancels and joins probes; reload and restore start empty. Concurrent callers
share one probe for an existing directory, while cancellation only ends that caller's wait. No negative
candidate result or permission decision is cached. The worktree design owns Git and subtree semantics.

### Write targets without the facts

Decision for [#294](https://github.com/arcships/dim/issues/294): with Workspace read/write
file access, a `write_filesystem` call passes the file gate only when the
facts say its resolved `path` is inside the Session workspace. Every outside
write asks for approval for that call, including a model write to a host-owned
output directory. `allowedTools` does not bypass this question;
`disallowedTools` still denies. A write tool without a path, or with a relative
target in a composition without HostPlatform, asks because its workspace scope
cannot be established. This is the fallback rule when the facts part is absent.

The tool's owner resolves its target before admission. WorkspaceTools expands
home-relative paths using its own HostPlatform facts and applies its shared
path boundary: platform path handling, lexical components and canonicalization
of the longest existing prefix, including a new file's parent. ToolExecutor
passes that resolved call to both Approval and execution, so retargeting an
original symlink while approval is pending does not redirect the approved call.
The model's original tool-call fact stays unchanged. Permission applies the same
resolver to a call it receives unresolved, and the facts compare the workspace,
system roots, owned artifacts and grants against that canonical target. Compare
path components, not string prefixes. Resolution failures refuse the call; an
unreadable prefix, symlink loop or dangling symlink is not a missing directory
to skip. I/O runs outside the async executor. Read the dispatched policy after
this I/O so a policy change while it waits takes effect. Approval rechecks the
path and policy before an approved call starts, using its existing
call-specific request and response.

### Session grants

[ADR 012](../../../adr/012-session-grants.md) stores the user's
"Allow for this session" answers in `session_grants`, owned by the App Server
`SessionGrants` part. The facts service reads them and matches the resolved
target: a file grant matches the exact path, a directory grant matches by
component, and a write requires a write grant. Permission applies no grant by
itself, so there is one path resolver. A grant read failure denies; a cancelled
read propagates. No grant becomes a Timeline fact, preset or
output-directory exemption. A write grant also admits a read; a read grant
never admits a write. Session attachments are Session-owned facts, so reading
them needs no grant; ADR 013's output table is the second ownership table, so
`owns` also answers for command output and a read of it needs no approval under
`read-only` and `workspace-write`. Host-generated output and
configured commands retain their own execution contracts under ADR 003. This is
tool admission, not an OS filesystem sandbox against concurrent filesystem
mutation.

The CLI uses its existing approval prompt, with the resolved outside target in
the reason and a Session-scoped grant action for file tools:

```text
[approval] read: Reading outside the session workspace requires approval: /home/user/.ssh/config
  path: /home/user/.ssh/config
› Allow once   Allow for this session   Reject
```

### Rule check

- §1 and ADR 002 D1–D3: the existing branch Permission and Approval Capabilities
  own admission and approval facts. WorkspaceTools owns file execution; no
  plugin, wiring part, lifecycle or isolation dependency is added. The grant
  store and `session.grants.add/remove` belong to the App Server
  `SessionGrants` Session part (ADR 012), and the fixed facts belong to the
  `FileAccessFacts` Session part (ADR 014 D1); Permission only reads them.
- §4 rules 1–6 and 9: existing approval requests/responses and tool results
  carry the decision and outcome; handlers retain their existing tasks,
  cancellation and cleanup. No new event or context input path is introduced.
- §4 rule 7: Permission reads its declared Session configuration, command rules,
  Session identity and the optional facts service; WorkspaceTools uses its
  existing HostPlatform dependency. The bound handler's `resolve_write`
  prepares the actual call at its external path boundary before admission; it
  reads metadata and performs no file mutation or process execution.
- ADR 002 D4–D5: the language-neutral policy is a preset and tool lists under ADR 018;
  approval request/reply formats stay unchanged. No structural configuration changes.
- §4 rule 8: no new Timeline, Dispatcher, loader or rutis mechanism is required.

### Requirements raised to mechanism owners

None. Existing admission, approval facts and host process facts suffice.

Command rules are read on each decision of an `execute_process` call with a
string `command`, never cached, stored or replayed. A matching deny rule denies
under every preset; a matching allow rule only turns an approval question into
an allow, for a command without shell separators; a failed read denies the
command call. The policy is taken after the read, so a change dispatched while
it waits decides the call. See [permission command rules](../../../design/appserver/permission-command-rules.md).

### Tests

[permission_flow.rs](../../../../crates/dim-agent-capabilities/tests/permission_flow.rs):
`presets_decide_by_risk_and_denied_calls_do_not_run`,
`tool_lists_override_presets_and_disallowed_wins`,
`updates_are_versioned_per_branch_take_effect_when_dispatched_and_survive_restore`,
`allow_rules_answer_approval_only_for_simple_commands_within_the_preset`,
`deny_rules_match_any_segment_under_every_preset_and_are_read_at_use_time`,
`unreadable_rules_deny_command_calls_only`,
`a_policy_tightened_during_the_rules_read_decides_the_call`,
`a_composition_without_facts_builds_and_asks`,
`file_write_facts_decide_each_path_by_preset`,
`file_read_facts_decide_each_path_by_preset`,
`a_session_grant_matches_only_the_canonical_target_and_mode`,
`advisors_are_asked_in_name_order_and_an_ask_refines_an_allow`,
`an_allow_advice_admits_a_call_the_preset_would_ask_about`,
`a_deny_advice_wins_over_an_allow_and_an_ask`,
`a_second_registration_of_the_same_name_fails`,
`a_registration_is_withdrawn_when_its_part_unloads`,
`advisors_are_not_asked_when_the_policy_denies`,
`advisors_are_not_asked_when_a_command_rule_denies`,
`an_advisor_error_counts_as_none`,
`cancelling_the_batch_fails_the_admission_and_the_advisor_sees_it`,
`cancelling_the_batch_drops_an_advice_that_never_answers`,
`an_advisor_withdrawn_while_its_advice_is_pending_counts_as_none`.

[session_files_flow.rs](../../../../crates/dim-app-server/tests/session_files_flow.rs)
checks `sessionFiles.owns` against the attachment owner rows.

[session_grants.rs](../../../../crates/dim-app-server/tests/transport_flow/session_grants.rs)
covers the `session.grants.add/remove` methods, a write grant admitting the
next outside write without an approval request, its removal making the next
write ask, and an outside read that asks until a read grant admits it.

[local_product.rs](../../../../tests/integration/tests/local_product.rs) verifies
real workspace writes, per-call external write/edit approvals despite explicit
tool allows, outside reads, denied edits, host output targets, parent symlinks,
unresolvable targets and the unchanged read-only/full-access presets.

[cli_flow.rs](../../../../crates/dimcode/tests/cli_flow.rs) verifies that a
non-interactive run answers an outside-read approval with the ordinary denial
and exits 3.

<!-- generated:plugin Permission — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`S:CommandRulesHost`、`S:SessionConfig`、`S:SessionIdentity` |
| 提供 | `B:Permission`、`B:StateCell<PermissionState>` |
| 被注入于 | `Approval`、`HooksBranch` |
<!-- /generated -->

## Approval

Owner: [approval.rs](../../../../crates/dim-agent-capabilities/src/approval.rs).
Turns permission decisions into call admission or durable approval requests and
rechecks policy before allowing an approved call to start.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:Permission` | Evaluate current policy and recheck an allowed response. |
| `B:Dispatcher` | Replay requests to recover abandoned ones, then receive approval responses. |
| `B:Timeline` | Commit requests/cancellations. |

### Events and lifetime

Produces `approval/request` and `approval/cancelled`; consumes `approval/respond`.
Caller cancellation settles its waiter; branch close/reload releases remaining
requests. Recovery cancels unmatched historical requests because their execution
did not survive. The service owns request state, not tool execution tasks.
Responses are not unconditional authority: cancelled batches and changed policy
can prevent execution. Approval does not sandbox an admitted tool.

### Model-facing texts

`Tool execution denied by user` is the result of a call the user denied, and it
is protocol
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)):
the model recognises it as a denial and does not retry the call.

### Client notifications

Rpc projects the three events at outbound encoding; none reaches `session.event`
or `session.history`. `approval/request` → `approval.request`, also replayed on
attach for pending requests; `approval/cancelled` → `approval.cancelled`;
`approval/respond` → `approval.resolved {sessionId, branchId, requestId, decision}`
(protocol v3 only: the v1 and v2 notification sets are frozen,
[#278](https://github.com/arcships/dim/issues/278)).
`approval/respond` is the command, so every committed response is projected.
Approval accepts the first one it consumes in sequence order: the first
`approval.resolved` per `requestId` carries the accepted decision, a later one
was answered `Unknown`, and an `approval.cancelled` after it means the call did
not start. The decision is the user's answer, not the tool's outcome. A client
that attaches after a response is committed but before Approval consumes it is
replayed the request without that response; its own `approval.respond` then
fails with `invalid_params`, which ends the prompt.

### Tests

[permission_flow.rs](../../../../crates/dim-agent-capabilities/tests/permission_flow.rs):
`an_allowed_approval_is_rechecked_against_the_current_policy`,
`a_response_that_finds_its_batch_stopped_ends_as_an_interrupt`,
`a_deny_rule_written_while_approval_is_pending_cancels_the_allowed_call`,
`advisors_are_not_asked_on_the_recheck_after_an_answer`;
[local_product.rs](../../../../tests/integration/tests/local_product.rs).

<!-- generated:plugin Approval — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Permission`、`B:Timeline` |
| 提供 | `B:Approval` |
| 被注入于 | `HooksBranch`、`ToolExecutor` |
<!-- /generated -->
