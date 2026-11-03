# Subagent model policy

Status: **Implemented** (Collaboration creation path, `collaboration` Settings source and
`CollaborationModelPolicyPlugin` management owner, `/models` "Subagent models" action).
Business owner: Collaboration.
Scope: the model-policy part of [#263](https://github.com/arcships/dim/issues/263)
and [the feature list](../feature-list.md#providers-and-models).

The confirmed scope applies inherit/fixed policy to **every newly created native
Collaboration member Session**, including a named Agent such as `reviewer`. Workflow
stages and Scheduler runs keep their `sessions.launch` model precedence; leader
`/agent` profile adoption is unchanged. Collaboration members cannot delegate, so
there is no nested-member policy. Existing member Sessions retain their
configuration. The configuration and interfaces below were approved in
[#352](https://github.com/arcships/dim/pull/352).

## 1. Behavior

An adopted selection with startup metadata ([model startup](model-startup.md)) takes
priority over this Settings policy for every newly created native member. Its
presence forces inheritance of the complete requesting-branch selection, even under
`agent` or `fixed` and a conflicting named Agent model. It neither reads nor writes
the saved policy. Without startup metadata the table below is unchanged.

| Policy | Model for a new native member |
| --- | --- |
| `agent` (default) | Existing behavior: use the temporary/named Agent's profile model, or its existing host/workspace default when the profile has none |
| `inherit` | The model selection currently adopted by the leader's **requesting branch**, including its model options and Provider-owned selection metadata |
| `fixed` | Resolve the configured `{providerId, modelId}` through the same model-selection path as an explicit Agent profile model |

Inherit/fixed takes precedence over both a temporary `profile.model` and a named
Agent's stored model. For example, a leader using A with an inherited policy
creates a `reviewer` member using A even when that Agent's profile names B.
The stored Agent profile is unchanged; the choice belongs to the new Session.
The requesting branch is a deliberate refinement of the feature list's "main
session" wording: a child keeps its adopted cut even if main later adopts another
model. Later `/agent` adoption affects only subsequent samples on that branch.
The leader model is not given the effective override in request/inspect replies
in this scope. Tool help states that user policy may override the requested profile;
Agent inspection still describes the profile, not a member's adopted model. No
callback or wait for member creation is added just to expose that choice.

A request addressed to an existing `agentId + sessionId` does not create a member
and does not apply this policy. Changing policy or the leader's model never
reconfigures existing members. Provider-owned/external members retain their own
contract; this policy does not interpret their opaque identities or set their models.

The default preserves current model selection. It introduces no additional model
decision by an LLM. `agent` means the existing request/profile selection behavior.

## 2. Use existing mechanisms

| Need | Existing mechanism |
| --- | --- |
| Own the decision | `Collaboration::establish`, reached for both named and temporary new members |
| Identify the requesting branch | `CollaborationSession::request` already receives `branch_id`; pass it within Collaboration, without changing an event or tool request |
| Read the adopted leader model | `Session::get<PromptSnapshot>(branch_id)` while consuming `collaboration/request`, before starting its delivery task |
| Resolve an explicit model | The existing `AgentIndex` model-selection host and configured/non-catalog profile paths |
| Persist and edit policy | Native `BusinessDocument`, `SettingsService::read/update`, source CAS and owner management methods, as used by `permission_rules.rs` |
| Start/restore and retain members | Existing Agents/Collaboration Session creation, usage holds and cleanup, including idle unloading |

Do not use `leader.config().model` or `session.summaries` as the adopted leader model:
they describe creation configuration rather than a subsequent F3 adoption.
The inherited selection consists only of the existing `ModelSelection` fields:
provider/model IDs, capability snapshot, model config and Provider-owned selection
metadata, including startup policy and its durable connection source. Do not copy request/session identity fields (`session_id`, `run_id`,
`trace_id`, `turn_id`) from `RequestMetadata`; the member's request producer supplies
its own identity.
The member keeps its own prompts, tools and existing permission/workspace behavior.
No leader tool table or prompt content is copied.

Inheritance preserves existing model options and metadata, including any Provider
route stamp or its absence. It does not resolve a different default model or read
Provider's saved client preferences. Fixed/agent selection also retains its current
profile-resolution semantics; saved client preferences are not implicitly applied.
Actual execution still checks availability through the existing model owner.
Inheritance performs no creation-time resolution: a disabled/deleted model or a
mismatched present route stamp therefore fails the member's first request and
produces the existing failed `collaboration/result`, rather than
`member_create_failed`. An absent legacy stamp retains its existing unchecked-route
risk. The Provider contract in §7 explicitly covers this adopted-selection copy.
For a startup connection, successful native Session creation is immediately followed
by retaining that source for the member through Provider, before publishing the
Collaboration relation. This checks the existing binding, not a new model selection
or connection binding. An unavailable Provider or source fails creation with
`member_create_failed` and removes the newly created Session and unused owned Agent.
Provider owns source lifetime, including idle unload and final-owner release; a
leader's close must not revoke a source still owned by a member. Collaboration uses
the target host's optional in-process Provider service, without a hard dependency.

## 3. User configuration and management

Register one native Settings source, namespace `collaboration`, with
`USER_FILE = "collaboration.json"` and no project file. Its document is the policy
itself, not another configuration wrapper:

```json
{"mode":"agent"}
```

```json
{"mode":"inherit"}
```

```json
{"mode":"fixed","model":{"providerId":"provider-id","modelId":"model-id"}}
```

This is one tagged type. Reject unknown fields/modes, a missing fixed model,
empty IDs, and a model supplied with another mode at the external boundary.
Fixed/agent mode starts with the existing resolver's catalog defaults. Dedicated
subagent effort/tier preferences, a `/settings` entry and `dimcode model` are deferred;
this design covers only the model-policy slice of those feature-list entries.
Inheritance keeps the effort/tier already adopted by its requesting branch.
Extending this closed source with preferences requires an explicit compatible
format decision or version/migration contract. Older binaries reject unsupported
source fields and preserve the document; they never silently drop them or default.

A missing source uses `agent`; malformed persisted data fails the read and is
not rewritten as a default. Existing Settings storage chooses native persistence
or its file-backed path; there is no new database table, store or migration.

Project files and tool arguments cannot set the policy. Use the local management
boundary for this user setting, not the generic `settings.*` methods, which do not
serve Rust `BusinessDocument` sources.

A dedicated `CollaborationModelPolicyPlugin` process owner row provides these
methods by registering namespace `collaboration` with the process
`ManagementMethods` table. It owns the native document's read/validation/CAS
operations and depends on `Keyed<SettingsService>` and `Keyed<ManagementMethods>`.
It is not the Collaboration plugin that Rpc already reaches through a Gate: the
management owner must reload independently without unloading Rpc, per the methods
contract §3.2.

| Method | Request | Reply | Mutating | Interruption |
| --- | --- | --- | --- | --- |
| `collaboration.model_policy` | `{}` | `{revision: string, policy: Policy}` | false | `Report` |
| `collaboration.set_model_policy` | `{expectedRevision: string, policy: Policy}` | `{revision: string}` | true | `Report` |

`Policy` is exactly the tagged source type above. Use the existing local v3
management authorization; a paired device has no access. Invalid request shape
or policy is invalid params (`-32602`, `invalid_params`); source CAS conflict is
`collaboration_model_policy_conflict`; source closure is `service_reloading`;
cancellation is `operation_interrupted`; other read/persistence errors are
`collaboration_model_policy_failed`. Follow the existing Settings/Permission
owner error handling. A write error can leave its outcome uncertain: reread
before another CAS, without automatically replacing the user's selection.

App Server configurations without `local_execution()` continue
using existing `agent` behavior and expose no policy management methods.
Collaboration must not gain an unconditional Settings dependency: declare the
dependency as `When<Keyed<SettingsService>>`, with keys from
`names.when::<Keyed<SettingsService>>(local_execution())`, the same predicate Agents
uses. Obtain it from `Deps`. In those hosts Collaboration already
reaches Settings through AgentIndex; account-only roots do not add a wait. The
composition root reserves the native source and registers its dedicated management
owner only under that predicate. The owner's `apply` registers its methods with
`methods.register(ctx, "collaboration", ...)`.
This needs no new user enable flag, optional lookup loop or wiring plugin.
When configured, a Pending Settings provider follows normal dependency waiting;
it is not treated as an absent source or a reason to skip policy.

## 4. Creation and lifetime

While the Dispatcher consumes `collaboration/request` in a configured host, capture
the requesting branch's `PromptSnapshot.configuration` through the existing public state read,
before spawning delivery (also in hosts without the Settings policy source, because
startup inheritance belongs to the captured selection). Serial dispatch has applied earlier events and cannot
apply a later `context/configured` until this handler returns. Pass this captured
selection within Collaboration to the existing creation task; do not sample it
after an asynchronous Settings read. A missing capture only fails `inherit`;
`agent`/`fixed` do not acquire a new Prompt dependency or wait for an unused snapshot.

For each new Collaboration member without startup metadata, read one Settings snapshot in that existing
owned request task. Settings is outside the Timeline and has no sequence-atomic
snapshot with Prompt. Do not cache policy or subscribe to changes. Resolve
`agent`/`fixed` through the existing profile paths; `inherit` uses the selection
already captured at request consumption.

If the configured source cannot be read, inherit has no captured selection, or
agent/fixed resolution fails, use the existing `member_create_failed` result and
temporary-Agent cleanup. Inherit execution failures follow §2. Do not silently use another model. Keep existing visibility checks,
Session admission holds, cancellation, terminal facts and Agent cleanup.

The captured selection and policy snapshot govern that creation even if policy or
the leader's model changes before Session publication. A later creation samples again; two concurrent
creations may therefore use different snapshots. There is no cross-owner atomic
check, retry fence or guarantee that an already sampled creation uses a later
edit. Each member's existing `context/configured` records what it actually adopted.
Read-only detail/cards display that observed model, not a guess from current policy.

## 5. Native CLI interaction

Add one action to the existing `/models` picker. It edits policy, not the leader's
current model. The fixed option reuses the model picker with a policy-selection
purpose; choosing it sends neither `session.model.update` nor `provider.select`.
It writes only `collaboration.set_model_policy`, preserving both the leader's
adopted model and Provider's scoped default.

```text
Select a model                  Subagent models
  Current provider / model        Agent choice (default)
  Other provider / model          Inherit requesting branch
  Edit models                     Fixed model > select from existing picker
  Subagent models: Agent choice

Applies to new members, including named Agents.
Existing members keep their models.
```

Read the source revision on opening and after a successful own write. On conflict,
keep the selection visible and require reread before another save. Cancel performs
no write. A failed/empty fixed-model listing leaves the existing policy intact.
Hosts without the advertised management methods omit this action; no legacy or
in-memory write fallback is added. No independent settings screen or second
command route is required for this TUI scope.

The picker's `Edit models` row opens the models management area for the Provider
its row names, and is absent from a daemon that does not advertise
`provider.management` ([CLI commands](cli-commands.md#the-models-area)).

## 6. Implementation and acceptance

The implementation touches Collaboration's process plugin and existing creation
path, its native Settings registration/management owner, and the CLI model picker.
Prompt, Agents' public contracts, Rpc dispatch, event types and rutis remain
unchanged. The Provider route-copy contract is stated in §7 and the EN/ZH Provider
documents; it adds no model-selection port. Register acquired effects
immediately and settle existing owned work before unload. Regenerate architecture output when dependency/assembly
registration changes, using `xtask graph`; do not hand-edit generated artifacts.

Extend existing Collaboration/native CLI public flows to cover:

- Named and temporary new members under all three modes; inherit/fixed overrides
  a conflicting profile model, while `agent` preserves current defaults.
- A requesting child branch and a later F3 adoption: inheritance uses that
  branch's captured model/options/Provider metadata. An adoption committed after
  the request event cannot change it. Request/session IDs remain the member's own.
  It preserves member prompts/tools and does not reconfigure an existing member on reuse or after a policy edit.
- Settings persistence/restart, two-client CAS, invalid source preservation,
  missing fixed models and source reload failure without model fallback or orphan
  temporary Agents. Unrooted hosts retain current behavior and assembly.
- Actual CLI selection, cancellation and conflict, confirming that policy editing
  leaves the leader's adopted model, Provider scoped default and draft intact.
- Independent management-owner reload keeps Rpc/connections active and returns
  the existing owner-unavailable error. Only local-execution hosts acquire the
  Settings dependency and expose the native policy namespace.
- Inherit from disabled/deleted/stale-route/unstamped legacy selections retains
  the copy semantics and first-request outcomes stated in §2.

The Collaboration and native CLI flow tests cover named and temporary members under
all three modes, inheritance of an adoption made after Session creation without
request identities, two-client CAS, invalid policies, a missing fixed model without an
orphan temporary Agent, unrooted hosts exposing no namespace, and CLI selection,
cancellation and conflict writing only `collaboration.set_model_policy`. Restart,
invalid persisted sources, owner reload, child-branch requests and stale-route
inheritance rely on the existing Settings, reload, Prompt and Provider flows and have no
dedicated policy test.
The public Collaboration flow additionally covers startup taking priority over a
missing fixed model and a temporary Agent's conflicting model, preserving full
options and metadata without request identities or policy writes. Its child branch
retains the parent-cut selection after main adopts another model; no branch-copy
mechanism change is required.
Native transcript cards still follow their separate integration dependency on
[#324](https://github.com/arcships/dim/pull/324).

## 7. Requirements raised to mechanism owners

The public Prompt snapshot, model-selection host, native Settings CAS and
management binding provide the required operations. No core, Prompt, Settings,
Rpc or rutis mechanism extension is requested.

[Provider management §6](appserver/provider-management.md) and its ZH mirror
explicitly permit Collaboration inherit to copy an already adopted selection,
including its Provider route stamp or absence, without creation-time resolution.
A mismatched present stamp fails before credentials/network I/O at the member's
first request; legacy absence keeps the existing unchecked-route risk. Every
fresh selection resolved by Provider still receives a stamp. Only Provider-owned
selection metadata is copied; member request identities are produced independently.

The same contract applies to #349. Fixed-mode effort/tier extension is outside
this cut and raises no resolver-port requirement.

## 8. Rule check

| Rule | Application |
| --- | --- |
| §4.1 | Collaboration chooses a model only for the Session it is creating; other owners' state is unchanged |
| §4.2 | Policy edits affect later creation samples; accepted work and existing Sessions are not revoked |
| §4.3 | No before callback; validate policy at its external boundary, perform I/O in the owned request task |
| §4.4 | No business branch in core, Dispatcher, Rpc or loader |
| §4.5 | Settings owns the policy CAS; member configuration uses existing Session creation without a cross-owner transaction |
| §4.6 | Only existing adopted model configuration reaches context; management replies are not injected |
| §4.7 | Read Prompt through public Session state and resolve models through the existing host; no private state access |
| §4.8 | Existing mechanisms suffice; no new port or speculative concurrency guarantee |
| §4.9 | No policy Timeline event, notice stream or duplicate model history; existing configuration facts identify the adopted model |
| §1 | Collaboration consumes the policy; the dedicated native management owner is required by the independent-reload contract; no new Capability |
| ADR 002 D1 | User policy belongs to Collaboration; Prompt/model execution and clients retain their own state |
| ADR 002 D2 | A business document owner provides read/validate/CAS methods; no adapter or wiring module |
| ADR 002 D3 | Settings is declared with the existing local_execution predicate; account-only/unrooted hosts and normal Pending behavior are preserved |
| ADR 002 D4 | Business policy is a native user source, never loader-row configuration |
| ADR 002 D5 | Tagged JSON, method fields, errors and interruption behavior are language-neutral |

## 9. Ablation review

One tagged document is necessary to persist the three user choices; no containing
configuration type is needed. Two owner methods provide the read and CAS write
that generic Settings methods intentionally do not expose for native sources.
One dedicated process row is required because Rpc already gates Collaboration
and cannot depend on its management owner; the row owns document operations rather
than merely wiring two services. One picker action provides the requested TUI
control. The existing creation path
and Prompt snapshot supply the behavior without a cache, listener, per-Agent
override map, new Timeline fact, model-policy service or additional lock.
