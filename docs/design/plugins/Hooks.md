# Hooks

Status: Ready ([#143](https://github.com/arcships/dim/issues/143)); maintainer decisions of 2026-10-22 and 2026-10-31 are applied (§10). Overall design: [overview](../overview.md) (Hooks). Phase: M1 (user, project and plugin sources). Language: Rust. Scope: [ADR 002](../../adr/002-plugin-scope.md) (process + Session + branch).

## Target contract

Scope: process, Session and branch. Issue: #2022.

The owners are `HooksSettings` (process), `HooksSession` (Session) and `HooksBranch` (branch). `HooksRuntime` is a private Session service. The feature's event points, document shape and script protocol stay as this document describes; this section replaces the public-surface decisions.

### Use and scope: responsibility split

`HooksSettings` owns:

- **Sources.** The Settings documents `hooks` (user), `hooks-projects` and `hooks-plugins`; `.dimcode/hooks.json` is read through FileStorage under the project root Settings authorizes.
- **Content registration.** The hook document a plugin package carries, the holding plugin and its refusal.
- **One reader.** `sources(cwd)` resolves the user, project and plugin sources into the value a hook point reads. The management methods and the agent-crate port use this same reader.
- **Client operations.** `hooks.catalog`, `hooks.save` and `hooks.set_enabled`.

`HooksSession` owns:

- **Command execution.** The Session's `HooksRuntime`, the command processes, their stdin payloads, timeouts, cancellation and settlement.
- **Submit check.** The `hooks` check registered with `submitChecks` for operator input. The check writes no input; it returns the context or a block.
- **Source read.** It calls `hooksService.sources`; the data face is the only reader.

`HooksBranch` owns:

- **Hook decisions.** Folding the public events into the six hook points, the Permission advisor, the Approval answers and the branch input that carries hook context or diagnostics.
- **Observed state.** The branch-input writes are the only state it adds; the decision itself is the owner's.

Callers keep:

- The hook document's business meaning and the command scripts.
- The admission, approval and input operations: Permission, Approval and the operator-input entries own them; Hooks only registers a check or advice and answers through their operations.
- The context the model reads; `HooksSession`/`HooksBranch` contribute through the owners' inputs and tool results.

| Requirement (callers) | Compose from existing operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Register a package's hook document | No: the content registry and refusal are `HooksSettings`' state | `register`, `withdraw`, `plugins`, `refusal` |
| Client reads and edits hooks | No: the merged catalog and the revision write are the owner's state | `hooks.catalog`, `hooks.save`, `hooks.set_enabled` |
| A hook point reads the current sources | No: the project file read and the source merge are the owner's knowledge | `sources(cwd)` |
| The Session runs a hook command | No: the process, timeout and settlement are the Session's resource | `HooksSession`'s runner; the command is private |
| Operator input is checked before it is written | No: the check must run before the entry writes, and the entry owner provides the registration point | `submitChecks.register("hooks")`; the check returns a value |
| A tool call gets hook advice | No: Permission owns the admission; Hooks contributes through its advisor registration | `Permission.advise`; the advisor returns deny/ask/allow |
| Hooks answers an approval | No: Approval owns the question and the decision | `Approval.decide` |
| Hook context or diagnostics reaches the branch | No: branch input is the branch owner's entry | `Branch.input` with source `hooks` |

### Caller example

`PluginManager` registers a package's hook document:

```rust
hooks.register(ctx, plugin, dir, document).await?; // refusal is a value
```

The local client:

```rust
let catalog = hooks.catalog(project, cancelled.clone()).await?;
hooks.save(revision, document, cancelled).await?; // conflict -> re-read
```

`HooksSession` registers the submit check and runs a command:

```rust
submit_checks.register(ctx, "hooks", move |input, cancelled| {
    let sources = host.read(cwd, cancelled.clone());
    run_user_prompt_submit(sources, input, cancelled)
});
```

`HooksBranch` registers advice, answers an approval and writes an observation:

```rust
permission.advise(ctx, "hooks", move |call, cancelled| advise(hook_sources(), call, cancelled));
approval.decide(approval_id, decision);
branch.input(source, HookInput::observation(text));
```

### Own contract

#### Dependencies

`HooksSettings` uses Settings (`register`, `read`, `change`, `authorize_project`, `authorized_projects`, `declared_sources`) and FileStorage (`read` for `.dimcode/hooks.json`). `HooksSession` uses SessionScope (`config`, `identity`, `activated`, `closing`, `withHold`), `hooksService.sources` and `submitChecks`. HostPlatform (#2032) has no target contract; the command-execution dependency is recorded on #2032 and no interface is defined here. `HooksBranch` uses the branch's `HooksRuntime`, `Permission.advise`, `Approval.decide`, `Branch.input`, and the public events it folds. `HooksRuntime` is not a public service; the Session and branch parts are the same feature.

#### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `hooksService.register` | Register a package's hooks document | `{plugin, dir, document}` | `{}` or `{refusal}` | Accepted write is not cancelled | `invalid_document`, `settings_write`, `settings_conflict`, `closed` |
| `hooksService.withdraw` | Withdraw a package's document | `{plugin}` | `{}` | Accepted write is not cancelled | `closed` |
| `hooksService.plugins` | The packages holding hook content | `{}` | `{plugins}` | None | `closed` |
| `hooksService.refusal` | The refusal of a package | `{plugin}` | `{reason?}` | None | `closed` |
| `hooksService.sources` | The merged hook sources for a cwd | `{cwd}` | `{sources, project?}` | The read can be cancelled | `unreadable`, `closed`, `cancelled` |
| `hooks.catalog` | The client's hook catalog | `{project?}` | `{sources, entries}` | The read can be cancelled | `invalid`, `closed`, `cancelled` |
| `hooks.save` | Save the user document at a revision | `{revision, document}` | `{revision}` | Accepted write is not cancelled | `invalid`, `conflict`, `settings_write`, `closed` |
| `hooks.set_enabled` | Enable or disable a project source | `{project, enabled}` | `{}` | Accepted write is not cancelled | `invalid`, `conflict`, `closed` |
| `HooksSession` runner | Run one hook command with a payload | `{hook, payload}` | `{decision, output}` | The Session's cancel kills the process tree; the owner settles | `failed`, `timeout`, `block`, `cancelled`, `closed` |
| `HooksSession` submit check | Check operator input before it is written | `{input}` | `{context?}` or block | The entry's call cancel stops the check and its command | `block`, `warnings`, `proceed` |
| `HooksBranch` advice | Advise a tool admission | `{call, source}` | deny/ask/allow | The admission's cancel stops the advisor | `deny`, `ask`, `allow`; failures map to allow/ask |
| `HooksBranch` approval | Answer an approval question | `{approvalId, decision}` | `{}` | The live answer's cancel | `Approval`'s errors |
| `HooksBranch` observation | Write hook context or diagnostics to the branch | `{input}` | `{}` | The branch close cancels the write; the owner rechecks | none; only diagnostics |

The `HooksRuntime` runner is Session-private; `HooksSession` and `HooksBranch` use it directly. `hooksService.sources` is the only source read; the `SettingsHookSources` duplicate is deleted.

#### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_document`, `invalid`, `conflict` | Fix the document or re-read the revision; nothing was written |
| `unreadable` | The project file cannot be read; report the source error; the user document still applies |
| `settings_write`, `settings_conflict` | Retry after Settings is available, or report the conflict |
| `failed` | The hook command failed; apply `onFailure` |
| `timeout` | The handler exceeded its timeout; apply `onFailure` |
| `block` | The hook blocked the operation; report its reason |
| `cancelled` | The operation stopped; nothing was written |
| `closed` | The Session or owner is unloading; use the replacement generation |

#### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Content register/withdraw | Validate, write the Settings document, update the content registry | The accepted write completes | The plugin row unloads; its content is withdrawn | New registrations refused; the next generation reads the documents | Settings write by `HooksSettings`; registry by `HooksSettings` |
| `sources` | Resolve the user document, the project file under the authorized root, and plugin content; return a snapshot | The read stops; no partial source | The caller's row unload drops the read | The next call reads the new generation | Source read by `HooksSettings`; project file by FileStorage |
| Command | Build the payload, spawn through HostPlatform, write stdin, wait with the handler timeout, settle the process tree | The cancel kills the process tree and settles it | The Session's row unload cancels its commands | Pending commands end `closed`; every process is settled | Command process by `HooksSession`; the work's future by the calling row |
| Submit check | Read the sources, run the `UserPromptSubmit` handler, return context or block | The entry's cancel stops the check | The check registration is withdrawn with the Session row | The entry does not write after a closed check | Check by `HooksSession`; input write by the entry owner |
| Advice | Read the sources, run the `PreToolUse` handler, return deny/ask/allow | The admission's cancel stops the advisor | The advisor registration is withdrawn with the branch | Permission's admission sees the failure mapping | Advice by `HooksBranch`; admission by Permission |
| Approval answer | Run the `PermissionRequest` handler, call `Approval.decide` | The live answer's cancel ends the answer | The branch row unload drops the answer | Approval's own errors | Approval by Approval; answer by `HooksBranch` |
| Observation | Write `Branch.input` with source `hooks` | The branch close stops the write | The branch row unload ends writes | The branch's input entry decides | Input by the branch owner; the hook result by `HooksBranch` |

### Exceptions with reasons

- `HooksRuntime` is the Session's private runner. `HooksSession` and `HooksBranch` are one feature; the command processes and their settlement stay in the Session part; the runtime has no public name.
- Hook commands run because they are configured; they need no host-local approval (AGENTS rule 11). `PreToolUse` only advises Permission; the tool call's approval stays with Approval.
- `UserPromptSubmit` runs only at the operator-input entries, before the write; other inputs do not run it.
- `HooksBranch` writes observations through `Branch.input`; it does not write the Timeline directly.
- The `hooks.*` methods are management methods (#2095), not a second `hooksService` face.

### Client-visible changes

None. `hooks.*` and the six hook points keep their client-visible behavior; `hooksService.sources` is a plugin-facing owner operation, `HookSourcesHost` was agent-crate-private, and `Permission.advise`/`Approval.decide` are #2028's contract names.

### Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | `HooksSettings` process; `HooksSession` Session; `HooksBranch` branch. The branch part folds public events and writes branch input; it is a Capability. |
| §2 scopes | Configuration is process; command execution and the submit check are Session; decisions and observations are branch. |
| §3 orthogonality | The branch part uses Permission, Approval and Branch input operations; the Session part uses the Settings source through the port. |
| §4 rule 1 | Hook commands are configuration-run commands, not agent commands; no Timeline command event. |
| §4 rule 2 | No handoff: the runner and command processes stay in the Session; the branch uses the Session's service. |
| §4 rule 3 | Commands wait in Hooks' tasks, never in a Dispatcher handler or under the commit lock. |
| §4 rule 4 | The vocabulary is hook points, sources, handlers and decisions; no caller business appears. |
| §4 rule 5 | No batches. |
| §4 rule 6 | Hook context reaches the model only through adopted inputs or tool results. |
| §4 rule 7 | Callers use `hooksService`, `hooks.*`, the check, the advisor, Approval and Branch input; no caller holds `SettingsHookSources` or a command process. |
| §4 rule 8 | Requirements to #2026, #2027/#2028, #2032, #2094, #2080, #2093 are listed below. |
| §4 rule 9 | The branch input is written after the command's decision; the Timeline commit is the branch owner's. |
| §7 callbacks and ids | The submit check and the Permission advisor are callbacks by reference; no object or handle crosses. |
| §7 one change | The two readers are merged into one; the old `SettingsHookSources` duplicate is deleted with the caller migration. |
| ADR 002 D1 | Process `HooksSettings`; Session `HooksSession`; branch `HooksBranch`. |
| ADR 002 D2 | No wrapper: `hooksService` is on `HooksSettings`; the port is a host binding, not a facade. |
| ADR 002 D3 | Extensions depend on the Hooks owner; the agent crate uses the port. |
| ADR 002 D4 | The hook documents are Settings sources and a FileStorage project file; no assembly business setting. |
| ADR 002 D5 | Operations, values and callbacks are language-neutral; the Rust binding is the appendix. |

### Requirements raised to mechanism owners

- #2026: `submitChecks.register` must accept the `hooks` check with a value result; the operator-input entries must call it before writing.
- #2027/#2028: `Permission.advise` registers the advisor with the call source and the admission decision; `Approval.decide` accepts a decision; `Branch.input` must accept `hooks` observations.
- #2032: HostPlatform's target contract is pending; the requirement comment records the intent (command execution with stdin, handler timeout and tree cancellation); no interface is defined here.
- #2094: Settings `register`, `read`, `change`, `authorize_project`, `authorized_projects`, `declared_sources`; no new operation.
- #2080: FileStorage `read` for `.dimcode/hooks.json`; no new operation.
- #2093: SessionScope `config`, `identity`, `activated`, `closing`, `withHold`; no new operation.

### Binding appendix

`hooksService` is on `HooksSettings`; the `hooks.*` methods are registered through #2095. `HooksRuntime` is `pub(crate)`. A branch part may call any data face of the branch group with `line_owner::<T>(line)`, except registration and lifecycle services (ADR 027); `HooksBranch` calls Permission and Approval that way.

```rust
#[rutis::service(name = "hooksService")]
impl HooksSettings {
    pub async fn register(&self, caller: &Ctx, plugin: String, dir: String, document: HooksDocument, cancelled: CancelledFn) -> Result<Option<HooksRefusal>, HooksContentError>;
    pub async fn withdraw(&self, caller: &Ctx, plugin: String, cancelled: CancelledFn) -> Result<(), HooksContentError>;
    pub fn plugins(&self) -> Vec<String>;
    pub fn refusal(&self, plugin: &str) -> Option<HooksRefusal>;
    pub async fn sources(&self, caller: &Ctx, cwd: String, cancelled: CancelledFn) -> Result<HookSources, HookSourceError>;
}
```

`HooksDocument`, `HooksRefusal`, `HookSources` and `HookSourceError` are the types in `hooks_settings.rs` and `hooks/sources.rs`; `HooksContentError` and `HookSourceError` are `#[serde(rename_all = "snake_case")]` enums serialized as `{code, data}` with `message` in `data`; the wire `code` is the variant name and matches the error table. `From<CallError>` keeps the callback's own `code` and `message`. `CancelledFn` is the binding's cancellation argument, never a request field.

The Session and branch parts keep their internal types; `hooksService.sources`' request and reply are `{cwd}` and `HookSources`.


Hooks runs user-configured commands at fixed points of a Session's **business operations** and hands each command's decision to the owner of that operation. Dim reads only its own hooks configuration; it does not load Claude Code or Codex hook files. For easy migration, the document shape (event names, matcher groups, command handlers, `timeout`, `onFailure`) and the script protocol (stdin JSON, exit codes, decision and output fields) follow Claude Code and Codex, so existing scripts and pasted configuration work (Appendix G).

Hooks never hooks infrastructure. The withdrawn design ([#21](https://github.com/arcships/dim/issues/21)) made Timeline `before` callbacks asynchronous so a command could hold a write; that is not done, and Hooks registers no before callback. Timeline, Dispatcher, the event envelope and assembly are unchanged. Each hook point instead attaches to the business owner that already decides the operation, through that owner's public events or services:

| Hook point | Business operation | Owner that decides | How the hook's decision reaches the owner |
| --- | --- | --- | --- |
| `PreToolUse` | Admission of a tool call | Permission (called by Approval's `admit`) | During the admission decision, Permission calls the advisors registered with it; `HooksBranch` registers `hooks` through `Permission.advise` |
| `PermissionRequest` | An approval question | Approval | Hooks answers through Approval's public `respond`, like a client (existing) |
| `UserPromptSubmit` | Submission of operator input | The operator-input entries: Rpc `session.submit` and `session.branch_create`, and InputQueue's `session.input.withdraw {steer}` | Before it writes, the entry calls the checks registered with the Session's `submitChecks`; `HooksSession` registers `hooks` through `submitChecks` |
| `PostToolUse`, `PostToolUseFailure`, `Stop`, `SessionStart` | Context after a tool result, after a turn, at Session start | InputQueue (inputs) | Hooks writes branch input with source `hooks` (existing) |
| `Notification` | An approval waiting for the user | — (observation) | Nothing reaches an owner (existing) |

`UserPromptSubmit` is not a Timeline hook point: Session input is not necessarily the user's (Goal, Scheduler, Collaboration, ShellInput and Hooks itself write inputs), so the command runs only where operator input enters, before anything is written. Commands run through HostPlatform (the Session's resource node); a hook command waits only in Hooks' own tasks or in the caller's own task (Permission's admission decision runs in ToolExecutor's per-call admission task; a submit check runs in the Rpc call), never in a Dispatcher handler or under the commit lock. Contracts are given in neutral form ([ADR 002 D5](../../adr/002-plugin-scope.md#d5-contracts-are-language-neutral)); the Rust binding is Appendix H.


## Supersedes

Replaces the old Hooks.md Structure, Flows, Rule check, Requirements, Appendix A.2.1/A.6/A.7/H and Owner check (deleted); the `hooksService` rows in [service-faces.md](../service-faces.md); the Hooks rows in [resource-owner-operations.md](../contracts/resource-owner-operations.md), [runtime-owner-operations.md](../contracts/runtime-owner-operations.md) and [session-boundary.md](../contracts/session-boundary.md); the `HookSourcesHost` port and the two-reader design. Rules: [§7 所有者与操作](https://github.com/arcships/dim/pull/2029). Dependencies are defined by [FileStorage #2080](https://github.com/arcships/dim/pull/2080), [Settings #2094](https://github.com/arcships/dim/pull/2094), [SessionScope #2093](https://github.com/arcships/dim/pull/2093) and the branch contracts #2102/#2100; HostPlatform is pending #2032.

## 5. Context mapping

| Source | What the model sees | Placement | Compaction | Child branches |
| --- | --- | --- | --- | --- |
| `SessionStart` context | Append input, marker `<dim-hooks kind="session-start">`, keyed raw block `hooks:session-start/<n>` per hook | With the first driving input | Latest block per key replayed | Inherited while live at the cut |
| `UserPromptSubmit` context | Text blocks, marker `<dim-hooks kind="prompt-submit">`, one per hook | In the operator's input, after the operator's own content; not attached to a `session.branch_create` prompt | Summary only | As inputs |
| `UserPromptSubmit` block | Nothing: no input is written | — | — | — |
| `UserPromptSubmit` warning | Nothing: the failed run contributes no context, and the reply carries `Hook <source> did not finish: <reason>` for the caller | — | — | — |
| `PreToolUse` deny | Error text `Hook <source>: <reason>` in ToolExecutor's `step/tool-result` | Paired with the call | Summary only | If before the cut |
| `PreToolUse` allow, `PermissionRequest` | Nothing beyond the call's ordinary outcome | — | — | — |
| `PostToolUse(Failure)` context or block reason | Append input, marker `kind="post-tool"` with `call_id` and `tool` | Next model request of the turn | Summary only | As inputs |
| `Stop` block reason | Queue input, marker `kind="follow-up"` | Start of the turn it drives; a client draws the row there, after the turn it ended | Summary only | As inputs |
| `Notification`, `async` handlers | Nothing | — | — | — |

The model never sees hook configuration, commands, exit codes, diagnostics, a `UserPromptSubmit` block reason or a `UserPromptSubmit` warning.

## Appendix A. Interface details
### A.1 Hook document format

The user's document is the Settings source `hooks` (user scope only); the project document is `<workspace>/.dimcode/hooks.json`, which `HooksSettings` reads through FileStorage under the project root Settings authorizes and validates with the same document type. Both use the Claude Code / Codex shape: an object keyed by event name, each holding matcher groups with command handlers.

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash|mcp__fs__.*",
        "hooks": [ { "type": "command", "command": "$DIM_PROJECT_DIR/scripts/guard.sh", "timeout": 10, "onFailure": "block" } ] }
    ],
    "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "~/bin/prompt-check" } ] } ]
  },
  "disableAllHooks": false
}
```

| Field | Rule |
| --- | --- |
| `hooks.<Event>` | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `Notification`. Other event names are listed `unsupported` and never run |
| `matcher` | As in Claude: empty, absent or `*` matches all; a value of only letters, digits, `_`, `-`, space, `,` and `|` is a list of exact names split on `|` or `,`; anything else is an unanchored regular expression. Tool events match the Dim tool name or its migration alias (G.2). `SessionStart`: `startup`. `Notification`: `permission_prompt`. Ignored for `UserPromptSubmit` and `Stop`. An invalid expression makes the group `invalid` |
| `hooks[].type` | `command`. Other types (`http`, `mcp_tool`, `prompt`, `agent`) are `unsupported` |
| `hooks[].command` | Shell form, run by the node's execution shell with `SessionConfig.cwd` as working directory. `commandWindows`, when present, replaces it on a Windows node |
| `hooks[].timeout` | Positive integer seconds; default 600, and 30 for `UserPromptSubmit` |
| `hooks[].onFailure` | `continue` (default) or `block`; what a failure, timeout or cancellation means (A.5) |
| `hooks[].async` | `true`: runs, nothing waits for it and its output is ignored |
| `hooks[].args`, `hooks[].if` | The handler is `unsupported` and never runs: exec form and permission-rule filters are not implemented, and ignoring them would change what runs |
| `hooks[].statusMessage`, other unknown fields | Ignored |
| `disableAllHooks` | Honored only in the user document: no hook runs from any source. Ignored in the project file |

Project enablement is the user-local source `hooks-projects`: `{ "enabled": ["<workspace>", …] }`, keyed by the canonical project root Settings authorized (as MCP's `mcp-projects`, with one source per workspace instead of entry ids).

### A.2 Sources and order

| Source id | Document | Enabled |
| --- | --- | --- |
| `user` | Settings source `hooks`, user scope | As written |
| `project` | `<workspace>/.dimcode/hooks.json`, read through FileStorage | When `hooks-projects` lists the workspace |
| `plugin:<id>` | A plugin package's hook document, registered by PluginManager with `register({plugin, dir, document})` | While PluginManager keeps it registered; `withdraw({plugin})` removes it |

Plugin documents are kept in the Settings source `hooks-plugins`, one `{dir, hooks}` entry per plugin id, as Mcp keeps `mcp-plugins`; `plugins()` lists the ids Hooks holds, and Hooks restores them after its own reload. A registered document is validated like any other (A.1): a document Hooks refuses is reported to PluginManager under `refused`, leaves no entry, and none of its hooks run. A plugin source's commands get `DIM_PLUGIN_ROOT` set to `dir` (A.3), and its catalog entry's `path` is that `dir`, with its enabled state.

`hooksService.sources(cwd)` returns, per source, its id, path, enabled state and document or error. The workspace is the project root Settings authorizes for `cwd`, as `mcp.catalog` resolves it; a `cwd` with no project has no project source. Catalog order is the table order, then document order. Handlers at one hook point with the same `command` string and the same plugin directory run once; a plugin's commands are never merged with another plugin's, because `DIM_PLUGIN_ROOT` gives them different meanings (A.3). Documents are read at each hook point; a missing project file is empty, an unreadable or invalid one is listed `invalid` with a diagnostic and none of its hooks run.

### A.3 Command protocol

| Channel | Content |
| --- | --- |
| stdin | One UTF-8 JSON object, then EOF. Common: `hook_event_name`, `session_id`, `cwd`, `permission_mode` (Dim preset: `read-only`, `workspace-write`, `full-access`), `turn_id` when turn-scoped, plus Dim fields `branch_id`, `dim_source` (`user`, `project` or `plugin:<id>`) and `collaboration_role` (`leader` for every Session that is not a Collaboration member, `member` for a Collaboration member Session). No `transcript_path` |
| Environment | HostPlatform's base environment plus `DIM_PROJECT_DIR`, `DIM_HOOK_EVENT`, `DIM_SESSION_ID` and `DIMCODE_HOME`; `DIM_PLUGIN_ROOT` for plugin sources only |
| Exit 0 | stdout empty, one JSON object (A.4), or plain text — anything that is not one JSON object, other JSON values included — which is context for `SessionStart` and `UserPromptSubmit` and otherwise ignored |
| Exit 2 | Blocking result for the event (A.4); stderr (first 2 KiB) is the reason |
| Other exit, signal, timeout, spawn failure, stdout over 64 KiB, `node_unavailable` | Failure: diagnostic, then the handler's `onFailure` (A.5) |

Event fields:

| Event | stdin fields |
| --- | --- |
| `PreToolUse` | `tool_name`, `tool_input`, `tool_use_id`, `risk`, `permission_decision` (`allow` or `ask`: Permission's own) |
| `PermissionRequest` | `tool_name`, `tool_input`, `tool_use_id`, `approval_id`, `reason` |
| `PostToolUse`, `PostToolUseFailure` | `tool_name`, `tool_input`, `tool_use_id`, `tool_response: {content, is_error}` |
| `UserPromptSubmit` | `prompt` (text blocks joined by newlines; images not passed), `mode` |
| `Stop` | `stop_hook_active` (the turn was driven by a Hooks follow-up) |
| `SessionStart` | `source: "startup"` |
| `Notification` | `notification_type: "permission_prompt"`, `message` |

`tool_name` is the Dim tool name and `tool_input` is Dim's input; `tool_use_id` is the `call_id` of the call's `source` (`BranchSource::Tool`).

`DIMCODE_HOME` is the product home the App Server that runs the Session is configured with, not the daemon's inherited environment: a product daemon always has one and sets it, while a development profile (`--model-config`, `--mock-model`) has no product home and sets none, so a hook that archives there passes `dimcode export --database PATH` itself. `DIM_PLUGIN_ROOT` is the `dir` a plugin source was registered with (A.2); user and project sources have none.

**Archiving.** A hook that needs the Session's history reads the Session's Timeline with `dimcode export "$DIM_SESSION_ID"` ([Timeline export](../appserver/timeline-export.md)), which reads the home `DIMCODE_HOME` names ([ADR 005](../../adr/005-observability.md) D3). There is no `transcript_path` (decision 8).

### A.4 Outcomes

| Event | stdout JSON read | Exit 2 | Result |
| --- | --- | --- | --- |
| `PreToolUse` | `hookSpecificOutput.permissionDecision` (`deny`, `ask`, `allow`, `defer`) with `permissionDecisionReason`; legacy `decision: "block"` / `"approve"` with `reason` | deny | `deny` → advice deny; `ask` → advice ask; `allow`/`approve` → advice allow; `defer` → no opinion |
| `PermissionRequest` | `hookSpecificOutput.decision.behavior` (`allow`, `deny`) | ignored | `respond(allow)` / `respond(deny)`; none → nothing |
| `UserPromptSubmit` | `decision: "block"` with `reason`; `hookSpecificOutput.additionalContext` | block | Block → `input_blocked`; context → a text block after the operator's content |
| `PostToolUse(Failure)` | `decision: "block"` with `reason`; `hookSpecificOutput.additionalContext` | stderr as context | Append input |
| `Stop` | `decision: "block"` with `reason`; `hookSpecificOutput.additionalContext` | stderr as reason | Queue follow-up (reason, then context) |
| `SessionStart` | `hookSpecificOutput.additionalContext` | failure | Keyed Append block |
| `Notification` | — | failure | — |

A `Stop` block whose reason is empty, an exit 2 with no stderr or a `decision: "block"` without a `reason`, writes no follow-up; the runner logs a warning naming the handler's source and the event.

Context and reasons are each at most 16 KiB per hook. Fields Dim does not support (`updatedInput`, `updatedPermissions`, `updatedToolOutput`, `message`, `continue`, `stopReason`, `suppressOutput`, `systemMessage`, `initialUserMessage`, `sessionTitle`) are ignored with a diagnostic.

### A.5 Concurrency, combination, timeouts and failure

| Item | Rule |
| --- | --- |
| Concurrency | The matching handlers of one hook point run concurrently; results are combined in catalog order, so the outcome and the chosen reason do not depend on finishing order |
| `PreToolUse` | Any deny → deny; else any ask → ask; else any allow → allow; else no opinion. The reason is the first deny in catalog order: once a deny is known, handlers after it in catalog order are terminated, and handlers before it run to completion because their deny would take precedence |
| `PermissionRequest` | Any deny → deny; else any allow → allow; else nothing |
| `UserPromptSubmit` | Any block → block (first block in catalog order, by the same early-termination rule); else proceed with each context as one text block after the operator's content, in catalog order. An `async` handler is not part of the decision and is not terminated by a block: it ends at its own timeout, when the entry's call is interrupted, or when Hooks' parts unload |
| `PostToolUse(Failure)`, `Stop` | All outputs in catalog order in one input |
| Timeout | Per handler `timeout`; the process tree is terminated |
| Failure | Failure, timeout and cancellation are mapped by the handler's `onFailure`: `continue` = no opinion (`PreToolUse`), nothing (`PermissionRequest`), proceed without its context and report the failed run to the caller as a warning (`UserPromptSubmit`); `block` = deny, deny, block with `Hook <source> did not finish`. Observation hooks: diagnostic only |
| Cancellation | `PreToolUse`: Permission drops the advice call (batch cancelled, branch closing). `PermissionRequest`: the request is answered or `approval/cancelled` is consumed. `UserPromptSubmit`: the entry drops the check only when its own call is interrupted (the Rpc load or the entry's provider unloads, the Session closes); a client disconnect cancels nothing. Unload: the handlers still running are answered by their `onFailure` (§1); one that already finished keeps its own result. Processes always end through `ctx.effect` |

## Appendix G. Migrating from Claude Code and Codex

Dim does not read Claude or Codex files and has no import command. To migrate, copy the `hooks` object: from `~/.claude/settings.json` or `~/.codex/hooks.json` into the user document (`dimcode hooks edit`, `dimcode hooks set PATH`, or Ctrl+E in `/hooks`, which call `hooks.save`), and from a repository's `.claude/settings.json`, `.claude/settings.local.json` or `.codex/hooks.json` into `.dimcode/hooks.json`, then enable that workspace (`hooks.set_enabled`). Codex `[hooks]` tables in `config.toml` are rewritten as JSON. Scripts keep working when they use only the compatible parts below.

### G.1 Events

| Claude Code | Codex | Dim | Notes |
| --- | --- | --- | --- |
| `PreToolUse` | `PreToolUse` | `PreToolUse` → Permission advice | `allow` admits, `deny`/`ask` as in Claude; `defer` is no opinion; `updatedInput` ignored |
| `PermissionRequest` | `PermissionRequest` | `PermissionRequest` → Approval | `updatedInput`, `updatedPermissions`, `message` ignored |
| `PostToolUse` | `PostToolUse` | `PostToolUse` | Non-error results; `updatedToolOutput` ignored |
| `PostToolUseFailure` | — | `PostToolUseFailure` | Error results |
| `UserPromptSubmit` | `UserPromptSubmit` | `UserPromptSubmit` at the submit entries | Text only; a block is an `input_blocked` error |
| `Stop` | `Stop` | `Stop` → Queue follow-up | Bounded (A8) |
| `SessionStart` | `SessionStart` | `SessionStart` → keyed Append | `startup` only |
| `Notification` | — | `Notification` on `approval/request` | `permission_prompt` only |
| `PreCompact`, `PostCompact`, `SessionEnd`, `Subagent*`, others | `PreCompact`, `PostCompact`, `SessionEnd`, `Interrupt`, `Subagent*` | Unsupported | Listed `unsupported` in the catalog. `PreCompact`/`PostCompact`: blocking would need a compaction-admission port in ContextCompaction; an observation-only form needs no owner change and can be added later. Compaction removes nothing from the Timeline, and `Stop` runs after every completed turn, so archiving loses nothing. `SessionEnd`: Session close is lifecycle, including idle unload, and work must settle before services are removed (AGENTS rule 8); a command at close would delay close. `SubagentStart`/`SubagentStop`: a Collaboration member is its own Session, which gets its own `SessionStart` and `Stop`, with `collaboration_role: "member"` on their stdin (A.3) |

### G.2 Tool names

Matchers are tested against the Dim tool name and its alias, so pasted matchers keep selecting the same tools; `tool_name` on stdin is always the Dim name. The alias is a property of the tool's declaration, and `HooksBranch` resolves it the same way on every tool event: when the branch has adopted a tool part, the aliases come from that part's declarations, and the registry is read only when it has adopted none.

| Dim tool | Alias accepted by matchers |
| --- | --- |
| Any tool with declared risk `execute_process` (`exec`, the shell tool) | `Bash` |
| `read` | `Read` |
| `write` | `Write` |
| `edit` | `Edit` |
| `glob` | `Glob` |
| `grep` | `Grep` |
| MCP tools (Dim names them `mcp_<label>_<digest>`) | `mcp__<server id>__<original tool name>`, from the declaration's `metadata.mcpServerId` and `metadata.mcpOriginalToolName`, which `HooksBranch` reads from the branch's adopted `context/configured`; MCP tools are always declared through an adopted tool part ([tool contributions §7](../../architecture/rust-agent/tool-contributions.md#7-动态工具来源)) |

Codex `apply_patch` has no alias: its input is a patch, while Dim's `write` and `edit` take file arguments, so a script for it must be rewritten.

### G.3 Differences

| Area | Claude Code / Codex | Dim |
| --- | --- | --- |
| Configuration files | `settings.json` levels, `hooks.json`, `config.toml` `[hooks]`, managed, plugin, skill and subagent hooks | User document (Settings `hooks`) and `.dimcode/hooks.json` only |
| Project trust | Codex: hash-bound trust and `/hooks` review; Claude: an interactive session's workspace trust dialog lists the hooks a folder's settings would activate, and `claude -p` or SDK runs never show it | Disabled until the user enables the workspace; no hashes; later edits apply (ADR 003 D4) |
| `disableAllHooks` | Claude: highest-precedence file wins; a project `false` overrides a user `true` | User document only |
| Handler types and fields | `command`, `http`, `mcp_tool`, `prompt`, `agent`; `args`, `if` | `command` only; `args`/`if` handlers are not run |
| Install reporting | Installing a plugin is silent about its handlers | `dimcode plugin install` prints the installed plugin's handlers whose state is not `ok` — events Dim does not run (G.1) and handler kinds it does not support — read from the existing `hooks.catalog`, with one line pointing to this appendix; a daemon that does not advertise `hooks.catalog` is not asked, and a read that fails is reported without failing the install |
| Timeouts | Claude: 600 s (30 s for `UserPromptSubmit`); Codex: 600 s | Same as Claude |
| stdin | `transcript_path`; `permission_mode` (`default`, `plan`, `acceptEdits`, `bypassPermissions`, …); `tool_name` with the product's names | `transcript_path` is replaced by `dimcode export` reading the Session (A.3); `permission_mode` is `read-only`, `workspace-write` or `full-access`; `tool_name` and `tool_input` are Dim's (G.2), so an MCP tool's `tool_name` is its Dim name, not `mcp__<server>__<tool>` |
| Environment | `CLAUDE_PROJECT_DIR`, `CLAUDE_PLUGIN_ROOT` | `DIM_PROJECT_DIR`, `DIMCODE_HOME`; replace `$CLAUDE_PROJECT_DIR` in pasted commands. `CLAUDE_PLUGIN_ROOT` and `GROK_PLUGIN_ROOT` are not set, so a pasted command that references them does not resolve; a plugin ships its Dim form with `$DIM_PLUGIN_ROOT` |
| `PreToolUse` | `allow` skips the prompt; `updatedInput` rewrites input | `allow` skips the question but never overrides a Deny; input is never rewritten |
| `PermissionRequest` deny | Claude/Codex show `message` | Approval's generic text |
| `UserPromptSubmit` block | Prompt erased, reason shown | `session.submit` fails with `input_blocked`; ACP `refusal` |
| Output fields | `continue`, `stopReason`, `systemMessage`, `suppressOutput`, … | Ignored with a diagnostic (A.4) |
