# Model preferences and Agent configuration

Implementation contract for [F3 #74](https://github.com/arcships/dim/issues/74).
It follows the [design rules](../architecture/rust-agent/principles.md),
[plugin scope](../adr/002-plugin-scope.md) and existing
[Prompt](../architecture/rust-agent/plugins/Prompt.md) contracts.

## Scope and owners

`/effort` changes reasoning effort, `/fast` selects a supported service tier, and
`/agent` adopts an existing Agent profile's prompt and optional model on the current
Session's `main` branch. The profile is a configuration template for this operation.
The Session keeps its Agent owner, identity, workspace, tools, permissions,
capabilities and existing branches. `AgentProfile.tools` is not applied.

| Owner | Responsibility |
| --- | --- |
| Agents, process | Resolve immutable profiles by exact ID from the existing directory; no second mode catalog. |
| Provider, process | Validate advertised effort/tier support and produce typed model configuration. |
| Agents, Session entry | Serve profile adoption through `sessionMethods`; no adopted state or background reconciler. |
| Prompt, branch | Order configuration commands, supersede work, atomically publish facts and rebuild state. |
| PromptSources, Session | Construct the existing `dim/session` System entry from the selected text. |
| CLI | Parse commands, list choices and render replies; no preference authority or shadow configuration. |

The Session entry extends Agents' public entry surface. Its factory captures only
configuration and instance identity; services come from `Deps`. Provider remains a
per-call lookup. Rpc's generic Session-method dispatch does not implement modes.

`AgentsSession` injects process `AgentIndex` and Session `TreeCommands` and
`sessionMethods`, registers the namespace `session.agent` with `sessionMethods`
from its `apply`, and reads the addressed branch's public Prompt service per
call. The Session's method table owns admission, tasks and cleanup of the
registration; unload reports interruption and releases the waiter without
revoking committed Prompt commands. The entry stores no adopted configuration. Its implementation is
[Agents' configuration entry](../../crates/dim-app-server/src/agents/configuration.rs).

## Existing mechanisms

- `session.model.update` already validates `ModelPreferences` through Provider and
  configures a model. Its explicit target starts from catalog defaults, so omitted
  preferences do not preserve previous Session choices.
- `ConfigureUpdate` carries source targets and a model. Prompt owns the existing
  configure-request/result commands, pending records and serial adoption queue.
- `ConfigurationPublisher` publishes prompt/model changes atomically and retains
  the previous tool part.
- `Prompt::open` already preserves System entries and the adopted model on resume
  and child creation. No opening/recovery extension is needed.
- PromptSources' periodic checks already refresh project/user instructions,
  environment and date.
- `Branch::get` reads a capability's published StateCell. The existing
  `PromptSnapshot` contains prompt entries and revisions, but no adopted model
  configuration; F3 adds that derived value to this owner-held snapshot.

## Model parameters

Extend the existing request without adding fields:

```text
session.model.update {
  sessionId, branchId, providerId?, modelId?,
  modelPreferences?: {
    reasoningEffort?: string | {action:"clear"},
    serviceTier?:     string | {action:"clear"}
  }
} -> {model: {providerId, modelId}, outcome: "adopted" | "unchanged"}
```

A plain string sets the field. `{action:"clear"}` clears it and needs no
`value`; any other action, including `{action:"set"}`, any extra field, null and
an empty string are invalid. An omitted field keeps its adopted value.
[Clearing one preference](#clearing-one-preference) gives the clear semantics.

Without a target, at least one preference field is required; an empty update is invalid.
Both target fields are present or both absent; partial targets and explicit null
are invalid. Explicit-target requests keep their current semantics. Without a
target, read the current adopted configuration from Prompt's branch snapshot and apply
only supplied preferences to that captured base. Other configuration values remain
unchanged. Provider owns validation and native reasoning-off/tier mapping; the CLI
does not reconstruct complete preferences from provider-specific request fields.
Changing effort replaces mutually exclusive reasoning enable/budget controls;
unrelated sampling, token limits, service tier and metadata are preserved.

This extends the business request's choice of base configuration. `ConfigureUpdate`,
`ModelSelection`, Timeline events and Session facades remain unchanged. No change
returns `unchanged` without a new adopted configuration fact. Requests still support `main` only. An older
daemon rejects the new request form normally; the client does not retry with a
guessed complete selection.

`/effort VALUE`, alias `/reasoning VALUE`, sends `reasoningEffort` without a target.
The `/models` picker cycles a model's advertised efforts with ←/→. Enter saves a
cycled effort as that model's `reasoningEffort`, then adopts the model with its
saved preferences.
Values come from declared `effortOptions` and the existing reasoning-off declaration.
The CLI reads the adopted provider/model identity from the public
`context/configured` view, then reads choices from the existing `provider.catalog`
response. The same view carries `model_preferences` (`reasoningEffort`,
`serviceTier`): the effective values the adopted configuration sends, derived
from the event's stored config and the `providerProtocol` recorded with it when
the view is read. The field is absent when the configuration carries neither
value, and when the recorded protocol is missing or unreadable; no protocol is
guessed. Nothing new is stored and the storage format is unchanged. The
`session.create` reply names these preferences from its creation configuration;
the `session.attach` reply names the model and the preferences from the
Session's latest adopted configuration, falling back to the creation
configuration while the Session has not adopted one. A Session therefore shows
its effort before publishing its first `context/configured` fact, and a reopened
Session shows the current model and values after any update. The event data
spells the field `model_preferences` and the two replies spell it
`modelPreferences`; each keeps its layer's naming and the inner fields match.
The CLI draws the effort from that field and keeps no preference state of its
own. The view does not expose the rest of the internal model settings or
capability snapshots. Catalog choices are display data; Provider validates each
update.
`none` needs that declaration or an advertised native `none`. Unsupported values
fail without adoption; no value is guessed from a model name.

`/fast on` sends `serviceTier: "priority"`; `/fast off` sends
`serviceTier: "default"`. The shortcut requires both advertised values and a
protocol supporting this preference. Other tier names do not implicitly mean fast
mode. Effort, token limits and other settings from the captured base are retained.
There is no persisted `fast` flag.

The snapshot field uses the existing `ModelConfiguration` type.
`ModelConfiguration` keeps the fields it already had; the preferences are not
stored beside it, and a replayed fact derives the same values a live one does
from the same config and protocol. Prompt's
projection updates it from adopted `context/configured` facts, and initializes it
from the existing context/checkpoint reconstruction when a branch opens or
restores. `Branch::get::<PromptSnapshot>()` supplies the latest published value;
a missing or reloading state follows the existing unavailable/error behavior.
An available snapshot without adopted model configuration rejects an absent-target
request with the existing `invalid_params` error; the caller can supply an explicit
provider/model target. This is not a service reload.
Preference commands do not call `Branch::context`, replay history or fill missing
configuration with defaults. This adds no service key, runtime state, persisted
field or second adopted-configuration type.

[Provider management](appserver/provider-management.md#33-probe-preference-adoption-and-usage)
stores per-model `ModelPreferences`. Clients read them and explicitly pass chosen
preferences through the existing Session adoption methods. The server never
applies them automatically. Clients include the exact `{providerId, modelId}`
with saved preferences for that pair; an unavailable target fails without retrying
with an implicit selection. `/fast` changes only the current Session. A saved
`serviceTier` takes effect only when a client passes it; no persisted `fast`
boolean or additional F3 adoption path is introduced.

## Clearing one preference

A preference-only update may clear one field without reselecting the model and
without changing the other field. Without a target, the fields are independent:

| Request value | Meaning | Adopted configuration | Readback |
| --- | --- | --- | --- |
| field omitted | keep the adopted value | that field unchanged | the value it already carried |
| `"high"` | apply the value | the model sends `high` | `"high"` |
| `{"action":"clear"}` | remove the Session's value for the field | the model's declared configuration for that field, or no value when it declares none | the declared value, or absent |

Clear is not a value: `reasoningEffort:"none"` selects the model's reasoning-off
declaration and `serviceTier:"default"` selects an advertised tier; neither
clears anything. The sibling preference and every other adopted model setting
stay. A cleared field carries the value an explicit adoption of the same model
with no preference for that field would carry, so reselecting the model is no
longer needed to clear one field. The value comes from the model's declared
configuration, not from a per-model preference saved through
[Provider management](appserver/provider-management.md#33-probe-preference-adoption-and-usage);
a Session clear neither reads nor changes those saved values.

Clearing is meaningful only without a target. With `providerId` and `modelId`,
both fields start from the target's declared configuration as for any explicit
adoption, so a clear there is accepted and adds nothing to that adoption; the
adoption can still change the model. An omitted sibling is reset by that
adoption. A client that clears one field and keeps the other sends no target.
`session.create` keeps the set-only spelling: a new Session has no value to
clear. A clear follows the same `main`-only, admission, supersession and
cancellation behavior as a set.

`session.model.update` still requires at least one preference field and returns
its existing `{model, outcome}` reply. A clear that leaves the adopted
configuration unchanged returns `unchanged` and writes no fact. A clear that
changes it is a configure request like a set: Prompt adopts it and publishes one
`context/configured` fact in the existing atomic batch. Attached clients receive
that event, `session.history` replays it, and a later `session.attach` reply
names the same values. The update reply is not a readback.

The request patch is a distinct type from `ModelPreferences`: one is what a
client asks to change, the other the effective values a configuration sends.
One request patch type serves `session.model.update`; Provider management's
saved-per-model patch keeps its own type and target. `ModelPreferences` keeps
its readback spelling and the readback stays derived from the stored
configuration.

### What a client sees

| Step | Result |
| --- | --- |
| Sends | one field `{"action":"clear"}`, the sibling omitted |
| Reply | `{model, outcome:"adopted"}`, or `"unchanged"` when that field already carried the declared value |
| Live | one `context/configured` event whose `data.model_preferences` omits the cleared field, or names the declared value |
| Reopen | the same model and `model_preferences` from `session.history`; the `session.attach` reply names the same values |
| Failure | the error a failed set gives; no fact, the adopted configuration unchanged |

Absence in the readback has one meaning: the configuration sends no value for
that field, so the provider's default applies. That is the state after clearing
a field the model declares no value for, and also the state of a field no one
set. A field the model declares a value for reports that value after a clear; it
does not become absent.

An older daemon rejects the action object as invalid parameters; the client does
not retry with a guessed complete selection. Plain strings remain accepted in
both request fields, and the readback still spells set values as plain strings,
so a client can send back what it read.

## Agent configuration adoption

Add one mutating Session method, contributed by Agents in namespace `session.agent`:

```text
session.agent.update {sessionId, branchId, agentId}
  -> {outcome: "adopted" | "unchanged"}
```

It uses existing `sessionMethods` admission/cancellation and supports `main` only.
Agents resolves a currently available profile by exact ID. Its `systemPrompt`
replaces the `dim/session` entry. An absent profile model preserves the adopted
model and preferences. A specified model adopts the resolved target's defaults,
including when it names the current model. Names/descriptions remain UI data.
No tool list, permission or Session ownership record is written.

PromptSources constructs the entry with its current identity, main branch scope,
System delivery, order and content hash. Empty prompt text revokes the entry. Base
instructions, Skills, AGENTS.md and environment sources keep their own entries.

Prompt's existing branch service gains a resolved-source operation. Its data is an
optional `ModelSelection` and a list of `{target: PromptSourceTarget, entry:
PromptEntry | null}`; null revokes that target. The Rust binding reuses these types
and Prompt's current loaded-source representation, derives the configure targets
and freezes the values in the existing operation's pending record. Each non-null entry
must match its target source/scope; System entries require branch scope, matching
the ordinary source-loading contract. It writes the
existing configure command, enters the same Dispatcher/queue and publishes through
the existing result path. The caller never writes the adopted result directly.

The service keeps the existing branch `Prompt` key and branch instance identity.
`configureLoaded` is asynchronous and returns the existing configuration outcome;
dropping its waiter does not revoke a committed command. Prompt owns cancellation
on supersession/unload. This adds no second service key or named-service adapter;
cross-language exposure remains C4's work, not an assumed F3 prerequisite.

This is Prompt configuration behavior, not runtime infrastructure. Neither
`ConfigureControl` nor `ConfigureUpdate` changes. Source-name-only loading cannot
carry selected profile text without mutating a shared source behind another
request's back; frozen values avoid that staging state and additional locks.

```text
/agent ID -> existing sessionMethods dispatch
          -> Agents resolves profile; Provider resolves optional model
          -> Prompt registers frozen values and writes configure-request
          -> existing Dispatcher, adoption queue and atomic publication
          -> Prompt consumes facts; reply adopted/unchanged
          -> later requests use the configuration and existing tools
```

PromptSources' explicit refresh uses its existing dynamic `CHECKED` sources,
matching periodic refresh. It does not reload static base/Session sources and so
cannot restore the creation-time prompt after a switch. Opening already preserves
System entries. Restored Sessions retain adopted prompt/model facts; new branches
inherit their cut, and existing branches do not follow later main changes.

## Ordering and failure

Already prepared requests and their tool handlers are unchanged. Later preparations
use adopted facts. Resolution/validation failure leaves configuration intact.
Prompt retains its existing supersession, cleanup and unchanged outcomes;
supersession uses the existing configuration error behavior. A lost response is
not automatically retried because adoption may have committed. Closing a profile
after copying its values does not revoke adopted content. Provider credentials are
not copied into prompts or persisted configuration facts.

One CLI awaits each command before sending another command/input. Across clients,
a preference candidate can be prepared from a prefix preceding another client's
change; Prompt ordering can then adopt that candidate's older base model/settings.
This is a known limit, not a concurrent patch guarantee. Record it for consolidated
configuration design. A clear is a preference candidate too and has the same
limit; it never resets the sibling. Moving the read into Prompt's queue would also require
validating the delta against whichever model that queue then contains. The
current Provider operation asynchronously resolves catalog/protocol metadata in
the App Server and returns a complete typed selection; Prompt's queue accepts
that selection, not Provider's business preference type or a resolver callback.
Adding a Provider dependency or a new deferred validation/patch contract is not
part of this design. Keep the captured-candidate semantics without revisions,
CAS, retries, a global lock or another serialization layer.

Errors reuse existing invalid-parameter, Agent-unknown, model/provider,
configuration-superseded, reload/interruption and Session-closed contracts.

## Scrollback interaction

Commands run at the existing idle prompt. Bare `/effort` and `/fast` read the
adopted provider/model identity from public `context/configured` history/notifications
and show supported choices from the existing `provider.catalog` response, without
mutation or a new query method. Bare `/agent` uses existing `agent.list` IDs, names
and descriptions; selection uses an exact ID. Empty/unavailable choices are explicit.

```text
> /effort high
Reasoning effort configuration adopted: high
> /fast off
Service tier configuration adopted: off
> /agent
ID                 NAME              DESCRIPTION
<agent-id>         Reviewer          Review changes
> /agent <agent-id>
Agent prompt/model configuration adopted; Session tools unchanged.
> /fast on
Error: this model does not advertise priority/default service tiers.
> _
```

Enter submits a command; errors keep the prompt and Session. Ctrl+C/EOF keep the
existing behavior for turns. Ctrl+C during configuration waits for its accepted
operation's result before another line; the reply may describe an already adopted
change. Further interrupts keep waiting and are consumed before returning to the
editor rather than clearing or exiting the next draft. Lines wrap at terminal width and use existing escaping. No
overlay, fullscreen UI, input queue or typeahead change. Command text does not
become model input; headless prompt/stdin content keeps its literal meaning.

## Impact and verification

| Area | Necessary change |
| --- | --- |
| Provider / current model-update entry | Apply preferences to an adopted base when no target is supplied; apply the per-field set/clear patch, clearing one field from the model's declared configuration while keeping the sibling; preserve explicit-target behavior. |
| App Server `session.attach` | Derive the reply's `model` and `modelPreferences` from the latest adopted configuration, falling back to the creation configuration. |
| Agents Session entry | Resolve a profile and submit prompt/model through one contributed method. |
| Prompt | Accept frozen sources through its current command/adoption path; expose adopted ModelConfiguration in its existing published snapshot. |
| PromptSources | Reuse System-entry construction and refresh dynamic sources only. |
| CLI | Three commands, choice display and replies. |

New public content is the absent-target model-update semantics, the per-field
clear action and the attach reply's current source, the Agent Session method,
Prompt's resolved-source operation and its derived configuration snapshot
field. Each serves the operations above.
No core port, durable event, persistence format, loader option, lifecycle stage,
dependency manager or general configuration registry is added.

Reuse public flows: actual Provider HTTP requests check effort/off/tier mapping and
untouched settings; transport checks both model-update forms, the per-field
clear applied beside a set and a clear that changes nothing, and rejection
without adoption; Prompt lifecycle checks atomic adoption, supersession, failure,
unchanged and restoration; CLI binary/PTY flows check routing and recovery. Include absent
and present profile models, empty prompts, unchanged tools/permissions/owner,
refresh after switching, child cut inheritance and an already bound request.
Verify that preference reads use the published snapshot after adoption and
restore, without history replay on each command; concurrent clients retain the
documented complete-candidate ordering rather than a delta-merge guarantee.
Do not test private queue/helper calls. Run graph/check when implementing the
Session entry registration, not for this design document.

The clear flow runs through the existing transport product-protocol test: adopt
a model with both preferences, clear one and observe the sibling and the other
settings unchanged, clear the other and observe the field absent, repeat the
same clear and observe `unchanged` with no new `context/configured`, then
re-attach and read the same model and values from the reply and from history.
Switch to another model and re-attach to observe the reply's `model` follow the
adopted one. Each malformed action is rejected without adoption.

Ablation: existing profiles replace a mode store; facts replace a preferences cache;
existing model-update fields replace a separate parameter method; frozen values
replace mutable staging; existing Prompt ordering/opening replaces new lifecycle
logic. The Session entry serves the user operation and the resolved-source path
supplies immutable content. Neither owns another module's state.

## Rule check

| Rule | Application |
| --- | --- |
| §4.1 | Existing Prompt commands request changes; Prompt writes adopted facts. A clear is a configure request like a set. |
| §4.2 | Prepared requests/bindings retain handed-over configuration. |
| §4.3 | No before callback or entry-side adoption; entries check shape and scope. |
| §4.4 | No core business branch; Provider owns mapping and Prompt owns adoption. |
| §4.5 | ConfigurationPublisher commits prompt/model facts atomically. |
| §4.6 | Context follows existing adopted configuration and consumed inputs/results. |
| §4.7 | Public profiles, model resolution, published Prompt state and Prompt operations. |
| §4.8 | Existing infrastructure suffices; no mechanism-owner requirement. |
| §4.9 | Only adopted facts are durable; names, choices and replies remain UI data. A clear stores no state; the readback stays derived. |
| §1 / D1 | Process catalogs, Session entry and branch state keep their owners; App Server adds only the Agents entry. |
| D2 | No registry/manager wiring part; the entry serves profile adoption. |
| D3 | Dependencies point outward; Provider lookup per call does not gate core loading. |
| D4 | Business changes adopt data without loader edits/reloads. |
| D5 | Client methods and the resolved-source operation have data-only contracts; Rust uses existing types. No new cross-language framework contract is assumed. |

## Requirements raised to mechanism owners

None. Timeline, Dispatcher, envelopes, assembly/lifecycle and Session/branch
facades remain unchanged. The known concurrent-candidate limit is not a request
for new infrastructure.
