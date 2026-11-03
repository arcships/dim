# Permission command rules

Status: Implemented (corrections found during implementation are listed in
[§12](#12-implementation-notes)). Roadmap item F2 ([#73](https://github.com/arcships/dim/issues/73)).
Overview: [§3 command rules row](../overview.md#3-integration-by-existing-mechanisms) and
[Appendix B.1 Permission row](../overview.md#b1-session-and-branch-capabilities). Scope:
[ADR 002](../../adr/002-plugin-scope.md) (a supplement to the existing branch Permission, with its
storage owned in App Server) and [ADR 003](../../adr/003-approval-scope.md) (agent decisions only).

## 1. Summary

The user keeps a list of command rules in one Settings business source, `permission`, user scope
only. Each rule is a command, a match kind (`exact` or `prefix`, by words) and a decision (`allow`
or `deny`). Permission reads the list each time it decides a call of a tool that starts processes
(`risk: "execute_process"`) with a string `command` input. Nothing is cached, nothing reloads and
nothing subscribes.

- A matching `deny` rule denies the call, under every preset.
- A matching `allow` rule only answers the question Approval would otherwise ask: it turns `Ask`
  into `Allow`. It never turns a policy `Deny` into `Allow` or bypasses file checks.
- `allow` matches only a simple command (no shell separators, redirection or substitution).
  `deny` is checked against every separated segment of the command.

Rules are written in two ways, both through the same owner: the client management methods
`permission.rules` and `permission.set_rules` (CAS on the Settings revision), and therefore also an
"always allow" answer, which the client turns into a `set_rules` write followed by the ordinary
`approval.respond allow`. Approval's events and methods are unchanged.

Command rules add no Timeline event or policy field and change no Timeline, Dispatcher, rutis,
Settings or assembly mechanism. The policy is owned by the
[Permission contract](../../architecture/rust-agent/plugins/Permission.md#policy-contract)
([ADR 018](../../adr/018-permission-is-the-preset.md)), not this rules source.

### 1.1 What this does not do

- No project-scope rules, no per-workspace rules, no patterns or regular expressions, no shell
  parser, no `ask` decision. Each is deferred ([§10](#10-decisions)).
- No rules for other tools. `allowedTools`/`disallowedTools` in the Session policy already cover
  whole tools; allowing a tool does not bypass [workspace write admission](../../architecture/rust-agent/plugins/Permission.md#workspace-write-targets).
- No change to the execution shell setting. The feature list puts "execution Shell" next to
  command rules, but it is HostPlatform's business configuration (ADR 002, "Existing plugins").
- Not a sandbox. Like presets, rules are admission logic; isolation is HostPlatform's separate work.

## 2. Structure

```
 Process scope (App Server)
 +---------------------------------------------------------------+
 |  Settings (unchanged): source `permission`, user file          |
 |    permission.json / native row user/permission               |
 |  PermissionRules (process part, owner row `permission`)        |
 |    managementMethods: permission.rules, permission.set_rules  |
 |    --read/update(CAS)--> Settings                             |
 +---------------------------------------^-----------------------+
                                         | read (each call)
 Session scope                           |
 |  CommandRules (Session part, App Server) provides             |
 |    commandRules (Rust: CommandRulesHost) --read--> Settings    |
 |  (mock and test profiles: defaults bind a fixed empty host)    |
 +---------------------------------------^-----------------------+
                                         | Keyed<CommandRulesHost>
 Branch scope                            |
 |  ToolExecutor --admit--> Approval --decide(call, risk)-->      |
 |                                      Permission (supplement)   |
 +---------------------------------------------------------------+
```

Dependencies point to the core: Permission (core, dim-agent-capabilities) depends only on the
Session port `CommandRulesHost` that the capabilities crate defines. App Server implements the
port over its Settings source; capabilities never names App Server or Settings. No dependency is
optional: every profile that installs Permission binds exactly one rules host (§4.2).

## 3. Data and state

### 3.1 Settings source `permission`

One document, user scope only (no project file):

```json
{
  "commandRules": [
    { "command": "cargo test", "match": "prefix", "decision": "allow" },
    { "command": "git push",   "match": "prefix", "decision": "deny" }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `command` | The rule's words: the string split on whitespace. At least one word. Must not contain a separator character (§5.1); such a rule could never match |
| `match` | `exact`: the command's words equal the rule's words. `prefix`: the rule's words are the first words of the command (`git status` matches `git status -s`, not `git statusx`) |
| `decision` | `allow` or `deny` |

Validation happens at the Settings boundary (`BusinessDocument::validate`), on write and when the
persisted file is read; a missing file is the default document (no rules). Order in the list has
no meaning: deny wins over allow (§5.2). Duplicates are harmless and not rejected.

Storage: a file source in the plain profile and an additive native `user/permission` row under
the v13 `settings_documents` contract, as for every new native source (overview §6.4). No format change to existing rows.

### 3.2 Runtime state

None. Permission keeps its existing `PermissionState`; rules are not part of it, not in
`session.permission.get`, and never in the Timeline (overview §6.4 "Security settings"). Replay,
restore and fork neither restore nor widen them.

## 4. Interfaces

### 4.1 Language-neutral (ADR 002 D5)

**Session service `commandRules`**, Session scope, no instance key, provided by the profile.

| Method | Mode | Request | Reply |
| --- | --- | --- | --- |
| `read` | async | `{}` | `{rules: CommandRule[]}` |

`CommandRule = {command: string, match: "exact" | "prefix", decision: "allow" | "deny"}`. Errors
are data: `unavailable` (the source could not be read, including an invalid persisted file),
`cancelled` (the caller dropped the call or the provider is unloading).

**Management methods**, namespace `permission`, which the process part registers with
`managementMethods` (overview §6.2 rule 9; never served to paired devices).

| Method | Request | Reply | Errors |
| --- | --- | --- | --- |
| `permission.rules` | `{}` | `{revision, commandRules}` | `permission_rules_failed` |
| `permission.set_rules` | `{expectedRevision, commandRules}` | `{revision}` | invalid params (validation), `permission_rules_conflict` (stale revision), `permission_rules_failed` |

`set_rules` replaces the whole list. The reply's `revision` is the Settings revision. Settings
emits its existing `settings.changed` event for the write; F2 adds no listener.

**Permission decision** (branch service, existing): the decision now takes the call, not only
its name. Its outcomes are unchanged (`Allow`, `Ask {reason}`, `Deny {reason}`); a rules read that
was cancelled is reported as cancellation.

### 4.2 Rust binding

In `dim-agent-types::permission`, next to `PermissionPolicy` (one type, used by the port and by the
App Server document):

```rust
#[serde(rename_all = "camelCase")] pub enum CommandMatch { Exact, Prefix }
#[serde(rename_all = "camelCase")] pub enum CommandRuleDecision { Allow, Deny }
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRule { pub command: String,
                         #[serde(rename = "match")] pub matching: CommandMatch,
                         pub decision: CommandRuleDecision }
```

In `dim-agent-capabilities::permission`:

```rust
pub trait ReadCommandRules: Send + Sync + 'static {
    fn rules(&self, cancel: CancellationToken)
        -> HostFuture<'_, Vec<CommandRule>>;   // Result<_, HostError>: Failed | Cancelled
}
pub struct CommandRulesHost(pub Arc<dyn ReadCommandRules>);       // Session value
pub struct NoCommandRules;                                         // the fixed empty host

impl Permission {
    pub async fn decide(&self, call: &ApprovalCallData, risk: Option<&str>,
                        cancel: &CancellationToken) -> Result<Decision, HostError>;
}
```

`PermissionPlugin::Deps` gains `Keyed<CommandRulesHost>` (hard). `Approval::admit` passes the
call it already holds and propagates a cancelled read; its two existing `decide` sites (first
decision and the recheck after an allow response) are otherwise unchanged.

Bindings per profile (exactly one each):

| Profile | Binding |
| --- | --- |
| `defaults::install`, `install_with_prompt`, `install_with_process_model` (mock App Server profile and test hosts) | `install_profile` binds the fixed empty host `NoCommandRules` with `session_value`, next to the `EmptyPromptHost` it already binds. Existing tests are unchanged |
| `install_with_prompt_capability`, `install_local_execution`, `install_minimal_execution` | Bind nothing, like `PromptHost`: the host that installs them binds the port. Capability-only hosts (integration tests, the `xtask` local graph) bind `CommandRulesHost(Arc::new(NoCommandRules))` |
| App Server product profile (`install_local_execution` in `main.rs`, its tests and the `xtask` App Server graph) | The profile calls `permission_rules::install(&mut builder)`, which adds the feature `CommandRules` and requires its Session part `CommandRules` (`require_feature`, Deps `Keyed<SettingsService>`); it provides `CommandRulesHost` reading `SettingsService::read::<PermissionSettings>(User)` on each call. `serve` registers the process part `PermissionRulesPlugin` (`register_part`, Deps `Keyed<SettingsService>` and `Keyed<ManagementMethods>`, owner row `permission`) with local execution, next to `mcp` and `skills`; it provides the management methods. Both live in `dim-app-server/src/permission_rules.rs` with the `PermissionSettings` document type (`NAMESPACE = "permission"`, `USER_FILE = "permission.json"`, no `PROJECT_FILE`) |

## 5. Flows

### 5.1 Matching

The command is the call input's `command` string, and only for a tool whose definition declares
`risk: "execute_process"`. Today these are WorkspaceTools' `exec` and ShellTools' `execute`, which
both already take `command`. Permission names no tool. Other calls never read the rules.

- Words: split on whitespace; quotes are not interpreted.
- Separator characters: `;` `&` `|` `` ` `` `(` `)` `<` `>` `$` and newline / carriage return.
- **Simple command**: contains no separator character. Only a simple command can match an `allow`
  rule. `cargo test; rm -rf x` or `cargo test > out` never matches `allow cargo test` and falls back
  to the preset (normally an approval question).
- **Segments**: the command split at every separator character. A `deny` rule matches when it
  matches any segment, so `echo ok && git push` and `x=$(git push)` match `deny git push`.
  Splitting ignores quoting, so a separator inside quotes can create a false deny; that fails
  closed and the reason names the rule.

This is a check on characters, not a shell parser. It is meant for the model's ordinary command
forms. It does not see through `sh -c`, aliases, environment prefixes (`A=1 git push`), absolute
paths or scripts; those are denied only by presets or OS isolation.

### 5.2 Decision order

```
 decide(call, risk)
  1. call.name in disallowedTools                  -> Deny   (existing)
  2. command call? read rules (each time)
       read failed                                 -> Deny "Command rules could not be read: <error>"
       a deny rule matches a segment               -> Deny "Command denied by command rule `<command>` (<match>)"
  3. file-scope gate, for file calls               -> Deny | Ask, or continue
  4. call.name in allowedTools                     -> Allow
  5. preset risk rules  -> Allow | Ask | Deny
  6. tool result is Ask and an allow rule matches the
     simple command                                -> Allow
```

File-scope and tool questions are combined into one call-specific approval. A command allow
answers only the tool question; the file gate and isolation are not command-rule policy.

| Preset | `execute_process` | With a matching allow rule | With a matching deny rule |
| --- | --- | --- | --- |
| read-only | Deny | Deny | Deny |
| workspace-write | Ask | Allow | Deny |
| full-access | Allow | Allow | Deny |

An allow rule cannot lift a preset denial or file check. Deny rules apply under every preset.
Read failures deny because a deny rule may exist.

### 5.3 Admission

```
 ToolExecutor       Approval                     Permission           CommandRulesHost
    | admit(call) --->| decide(call, risk) ------->| rules() ----------->| read user source
    |                 |<-- Allow | Ask | Deny -----|<-- rules ----------|
    |<- Ok (runs)     |   Allow                    |
    |<- error result  |   Deny: content = reason (existing HostError::failed path)
    |                 |   Ask: approval/request ... approval/respond allow
    |                 | decide(call, risk) again --> reads the rules again
    |                 |   Deny now: approval/cancelled {reason}, error result
```

The recheck after an allow response already exists; with rules it also picks up a deny rule
written while the question was open.

### 5.4 "Always allow" (client flow)

Delivered by the CLI permission controls ([#257](https://github.com/arcships/dim/issues/257); [CLI design](../cli-commands.md#always-allow)).

```
 Client                         PermissionRules             Approval
   | approval.request {call: exec, command "cargo test -p x"} (shown)
   | user picks "always"
   | permission.rules ------------>| {revision r1, rules}
   | permission.set_rules(r1, rules + {command, exact, allow}) -> {revision r2}
   |   (conflict: read again and retry)
   | approval.respond allow ------------------------------------>| recheck, call runs
```

The client proposes the rule from `call.input.command` (default `exact`; the user may shorten it to
a prefix). Approval and `approval/respond` are unchanged, so the answer and the rule are two
separate writes: if the rule write fails, the client can still answer once. A command that is not
simple cannot become an allow rule (§3.1), so the client offers "always" only for simple commands.

### 5.5 What the model sees

Nothing new. Tool definitions and prompts do not mention rules. An allowed call runs and returns
its normal result. A denied call returns an error result whose `content` is the reason, as preset
denials do today, for example
`Command denied by command rule \`git push\` (prefix)`. Rules are not listed to the model.

## 6. Errors

| Case | Result |
| --- | --- |
| Rules source unreadable or persisted file invalid | The command call is denied with "Command rules could not be read: …"; other calls are unaffected. `permission.rules` returns `permission_rules_failed` |
| Management call interrupted (owner unloading) | `operation_interrupted`, as for MCP; a write may already have committed, so read before retrying |
| Read cancelled (batch cancelled, Session closing) | Admission ends as cancelled, as today |
| `set_rules` with an invalid rule (empty, separator character, unknown field) | Invalid params; nothing written |
| `set_rules` with a stale revision | `permission_rules_conflict`; read and retry |
| Settings closed or reloading | The management call reports reloading as other owners do; admission denies (read failed) |

## 7. Security

- Rules govern only agent tool calls (ADR 003 D1). User commands (`!command`, ShellInput) and
  commands run by configuration (MCP stdio, Hooks, workflows) are not affected.
- Only the user scope exists, so no project file can grant itself an allow rule. Project scope is
  deferred (§10).
- Rules are host security settings: never in the Timeline, never restored or widened by replay or
  fork, and not writable from a paired device (management methods are local only).
- Allow rules cannot lift a policy denial or file check; deny rules apply under every preset; read failures deny.
- Matching is deliberately conservative for `allow` and over-inclusive for `deny`; §5.1 states its
  limits. It is not isolation.

## 8. Relationship to existing parts

| Part | Relationship |
| --- | --- |
| Permission | Supplement: `decide` takes the call and reads rules for command calls; Session dependency `CommandRulesHost`. Rules add no policy fields and change no policy versions or methods; presets follow Permission's own contract |
| Approval | Passes the call to `decide` at both existing sites and propagates a cancelled read. Events, `approval.respond`, replay and recovery unchanged |
| ToolExecutor, WorkspaceTools, ShellTools | Unchanged; `exec` and `execute` already declare `execute_process` and take `command` |
| Settings | Unchanged; one new source `permission` (user file, native row). Its `settings.changed` event is emitted as for any source; F2 does not listen |
| Timeline, Dispatcher, rutis, assembly | Unchanged |
| CLI `/permissions` | The CLI owns the panel that lists, adds and removes rules through `permission.rules`/`permission.set_rules`, and an "always" choice on the approval prompt for simple `exec`/`execute` commands (§5.4). `/approvals` and `/approval` are compatibility aliases ([CLI design](../cli-commands.md#permission-controls)) |
| HostPlatform | The execution shell stays its own business configuration; not F2 |
| RemoteGateway | Paired devices answer approvals but cannot read or write rules |
| Collaboration, Scheduler, Workflow | Rules are host-wide and apply in every Session; since allow only answers `Ask`, an automation Session capped at read-only is not widened |
| Hooks (pre-tool) | Independent: Hooks advice is consulted after command rules and only when they do not deny ([Hooks A.6.1](../plugins/Hooks.md#a61-tool-advice-in-permission)) |

Public additions: the Session port `commandRules` (Rust `CommandRulesHost`), the `CommandRule`
types, the `permission` management namespace (two methods) and the `permission` Settings source.
Each is needed for a rule to be stored, written by a client, and read by Permission.

## 9. Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (one event bus) | No new subscription, watch channel or event; rules are read on use. Settings' existing `settings.changed` is untouched |
| Principles §3 (dependency direction) | Permission (core) depends on a port the capabilities crate defines; App Server implements it. No core part depends on an optional plugin |
| §4 rule 1 (command events) | Not used: rules are process configuration written through their owner's typed write (overview §6.2 rule 2) |
| §4 rule 2 (handover) | Unchanged; admission happens before the tool starts, as today |
| §4 rule 3 (before callback) | Not used |
| §4 rule 4 (core has no business meaning) | Timeline and Dispatcher unchanged; Permission matches by declared risk and the `command` field, naming no tool |
| §4 rule 5 (batches) | No new writes |
| §4 rule 6 (context) | The model sees only the existing denial result |
| §4 rule 7 (public interfaces only) | Permission reads rules through its declared port; Approval uses Permission's public `decide` |
| §4 rule 8 (infrastructure unchanged) | No change to Timeline, Dispatcher, event envelope, rutis, loader, Agent assembly or Session facade. No requirement raised |
| §4 rule 9 (only needed facts in Timeline) | No event; rules are not in the Timeline |
| ADR 002 D1 (scope) | Rules are host-wide: storage and management are process; the read port is Session; the decision stays in branch Permission |
| ADR 002 D2 (no wiring modules) | No registry or aggregation; the Session part is the rules owner's binding of Permission's port, as `DefaultPrepareModelPlugin` binds `PrepareModelHost` |
| ADR 002 D3 (dependencies) | All hard; each profile binds one host |
| ADR 002 D4 (configuration) | Business configuration in the owner's Settings source, read at use time, no reload |
| ADR 002 D5 (language-neutral) | §4.1 is the contract; §4.2 is one binding |
| ADR 003 | Applies only to agent tool calls; no project entry grants authority |

Requirements raised to mechanism owners: none.

## 10. Decisions

Decided by the maintainer:

1. **Project rules:** deferred, not needed now. User-scope rules only; no project file.
2. **Per-workspace user rules:** deferred, not needed now.
3. **Wildcards or a shell parser:** deferred, not needed now. Word-wise `exact`/`prefix` plus the
   separator check (§5.1).
4. **`ask` rules:** deferred, not needed now.
5. **Dedicated methods vs S8b `settings.read`/`settings.update`:** decided when S8b is designed;
   F2 keeps `permission.rules`/`permission.set_rules`.
6. **Binding:** the Session port `Keyed<CommandRulesHost>` per profile (§4.2), not a process
   service dependency; existing tests are unchanged.
7. **Scope:** F2 is server side only: rules source, Permission check, management methods and flow
   tests. The CLI `/permissions` panel and the "always" option belong to F6 (CLI) as a requirement it
   consumes (§5.4, §8); the CLI delivered them in [#257](https://github.com/arcships/dim/issues/257).

## 11. Acceptance (flow tests)

Permission flow (`crates/dim-agent-capabilities/tests/permission_flow.rs`, a test host whose rules
the test changes between calls):

1. Workspace-write, rule `allow cargo test (prefix)`: `exec` with `cargo test -p x` runs with no
   `approval/request`; `cargo test; rm x` and `cargo test > out` raise an approval request.
2. Rule `deny git push (prefix)`: `git push origin` is denied under full-access, with the rule in
   the error content; `echo ok && git push` is denied; the tool did not run.
3. Read at use time: two calls in the same Session, rules changed in between, no reload: the second
   call follows the new rules.
4. Read-only with an allow rule: the call is still denied.
5. Recheck: while an approval is pending a deny rule is added; the allow response ends with
   `approval/cancelled` carrying the rule's reason, and the tool did not run.
6. A read failure denies the command call; a non-command tool in the same turn is unaffected.

App Server flow (`crates/dim-app-server/tests/`):

7. `permission.set_rules` writes a rule; the next `exec` in an already open Session follows it.
   A stale revision returns `permission_rules_conflict`; a rule with `;` is rejected; a
   `.dimcode/permission.json` in the workspace has no effect.

Implemented by `crates/dim-agent-capabilities/tests/permission_flow.rs` (1-6) and
`crates/dim-app-server/tests/transport_flow/permission_rules.rs` (7).

## 12. Implementation notes

Corrections to the draft found against the code; the sections above already state them.

1. **Bindings (§4.2).** The draft listed only the `defaults` profiles and "App Server with
   Settings". `install_with_prompt_capability`, `install_local_execution` and
   `install_minimal_execution` bind no host, as they bind no `PromptHost`, so their callers bind
   one: the App Server product profile calls `permission_rules::install`, capability-only hosts
   bind `NoCommandRules`. `serve` cannot install the Session part itself, because App Server tests
   that pair a product home with `defaults::install` already bind the empty host. The process part
   is registered with local execution, where the rules are consumed, not for a product home alone.
2. **Reason texts (§5.2, §6).** Denials read "Command rules could not be read: <error>" and
   "Command denied by command rule `<command>` (<match>)", matching §5.5 and the capitalized
   preset reasons.
3. **Recheck cancellation (§5.3).** A rules read cancelled during the recheck after an allow
   response ends the request with `approval/cancelled` ("Tool execution cancelled") and the
   cancellation is returned, as for a batch stopped at that point.
4. **Port future type (§4.2).** `HostFuture<'_, T>` is the existing alias of
   `BoxFuture<'_, Result<T, HostError>>`.
5. **Policy snapshot (§5.2, review).** The rules read is the only wait in a decision; the policy
   (`disallowedTools`, `allowedTools`, preset) is taken after it, so a `permission/changed`
   dispatched while the read waits decides the call, at the first decision and at the recheck.
