# Provider management

Status: **Implemented**. Process-owned Provider management for
[#329](https://github.com/arcships/dim/issues/329).
[中文](../../zh/design/appserver/provider-management.md).
The [S4 auxiliaryModel](auxiliary-model.md) contract remains unchanged.

## 1. Scope

Desktop and CLI use one App Server contract for
[Providers and models](../../feature-list.md#providers-and-models).

| Flow | Contract |
| --- | --- |
| List/edit | Safe management snapshots include connection fields, record versions and complete model descriptors |
| Connection | UUID creation and partial updates preserve secrets the client cannot read |
| Disconnect/delete | Disconnect clears credentials and disables atomically; custom deletion removes owned selections |
| Discovery | Probe tests a draft without storing a Provider, catalog, credential or selection |
| Models | Distinct defaults and user models, persistent descriptor overrides and explicit New API routes; each limit a model declares names its source, and a model that declares none is listed with the reason and cannot be chosen (§3.5) |
| Preferences | Clients read per-model preferences and pass them with an exact target at explicit adoption |
| Accounts/usage | Existing login/logout/usage owners and errors; no static-provider usage settings |
| Multiple clients | Record CAS and rereads on open or after own writes; no pushed notices |
| Legacy import | Explicit read-only import with origin mapping, per-entry results and unchanged source bytes |
| Process owners | One read of the models Provider would serve, with model and provider IDs and declared capabilities; a routing owner uses it instead of duplicating availability (§3.6) |

Source contracts: [Provider owner](../../../crates/dim-app-server/src/provider.rs),
[management methods](../../../crates/dim-app-server/src/provider/management.rs),
[model descriptors](../../../crates/dim-app-server/src/model_catalog.rs),
[legacy import](../../../crates/dim-app-server/src/provider/legacy.rs),
[account methods and Usage](../../../crates/dim-app-server/README.md), and
[F3](../model-configuration.md).
Media execution belongs to [Modality](../plugins/Modality.md); consumer-specific
choices, such as SessionTitle and sub-agent models, remain with their owners.

## 2. Ownership and affected surfaces

Provider owns its process configuration, model catalog, overrides, preferences,
validation, safe projections, writes and network tasks. Account owners retain login,
tokens and credential leases. SqliteStorage owns transactions and format admission.
Clients own unsaved drafts and presentation. Adopted Session configuration remains
with its existing owner; management never writes another owner's state.

| Surface | Consumers and change |
| --- | --- |
| `managementMethods`, process, namespace `provider` | Local v3 Desktop/CLI and trusted process callers; §3 extends this existing namespace |
| Provider host port `SelectableModelsHost` (core `dim-agent-capabilities`), provided by Provider | In-process owners that route by model capability (Modality); §3.6 adds one read, not a management method or RPC method |
| Existing `account`, `account.codex`, `account.grok` namespaces | Existing account operations; no new account method, event or managed host capability |
| `ProviderConnectionResolver`, `ModelSelectionHost` | Provider implements catalog/route decisions through existing types; no new core port |
| `auxiliaryModel.complete` | Existing caller-selected S4 contract, unchanged |
| Native CLI Provider flows | Migrate discovery, create, update and disconnect together with the new owner methods |

Business logic stays in Provider, not `connection.rs`, `protocol.rs` or a wiring
plugin. No Session Capability, notification stream, second catalog, credential
service or registry is added. The [managed-provider boundary](../../architecture/development.md#managed-provider-boundary)
continues to exclude private account implementations and credentials from this repository.

## 3. Management contract

### 3.1 Read models

| Method | Request | Reply | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.management` | `{providerId?}` | `{drivers: ManagementDriver[], providers: ManagementProvider[], errors: ProviderRecordError[]}` | false | `Drain` |
| `provider.list` | `{}` | Existing `{providers, errors}` summary; each provider adds `remoteModelList`, whether `provider.refresh` can list its models, which clients use to offer or withhold that command in a row naming an empty catalog | false | `Drain` |
| `provider.selection` | `{cwd?}` | Existing `{selection}` and its separate scope/version | false | `Drain` |
| `provider.catalog` | `{providerId}` | Existing `{catalog}` with `ProviderModel`; **unchanged** | false | `Drain` |

Without an ID, management returns all known entries, including signed-out accounts.
With an ID, it returns that entry or a typed error. It is the sole edit-view read:
its record version, model descriptors and annotations come from the same Provider record
snapshot. Clients do not combine `provider.catalog` from another version with its
`expectedVersion`. Selection and read-only web clients keep using the unchanged
catalog. Aggregation does not promise an atomic snapshot across account owners.

| `ManagementProvider` field | Type and meaning |
| --- | --- |
| `providerId`, `displayName` | Strings; stable identity and user-visible name |
| `source` | `builtin` / `custom` / `managed`; record origin, not login state |
| `credentialAuthority` | `static` / `openai_codex` / `grok` / `account`; the credential owner |
| `version` | Record version or `null` only when an account entry has no local catalog record; never a fabricated zero |
| `enabled` | Boolean for a static record; `null` when the account-owned record has no Provider enable flag |
| `account` | Optional `{namespace, loginMethods, state}`; namespace is `account`, `account.codex` or `account.grok`, login methods are `browser`/`device`, and state is that namespace's existing public status reply; absent for static authority |
| `actions` | Available operation names: `update`, `delete`, `refresh`, `models.create`, `models.edit`, `models.reset`, `models.delete`, `models.set_enabled`, `preferences.update`, `login`, `logout`, `usage`; no per-action reason codes |
| `connection` | Safe static-record connection below; `null` when connection configuration exists only in the optional account package |
| `models` | `{model: ModelEntry, source: "default" \| "override" \| "user", enabled: boolean, endpoint: ModelProtocol \| null, unavailableReason: string \| null, missingLimits: ("contextWindow" \| "maxOutputTokens")[], actions: string[], endpointOverride?: NewApiEndpoint}[]`; endpoint overrides use the executable subset in §6. `null` only for an account entry with no local catalog record. Bad catalog data is an error. `missingLimits` names the limits no source declared (§3.5) and is empty otherwise. Model actions are `set_enabled`, `edit`, `reset`, `delete`, restricted by origin/authority |
| `preferences` | Map from model ID to the existing `ModelPreferences` shape |
| `modelsListed` | Absent from the reads and from an update that did not change `baseUrl`, `credential` or `headers`; on the other updates it says what the write did with the record's models: `listed`, `bundled`, `unsupported` or `disabled` |

`ManagementDriver` fields are `driverKind: string`, `authTypes: StaticAuth["type"][]`
and `remoteModelList: boolean`, derived from executable drivers. `StaticAuth` is the
existing tagged type, not another auth schema. Only drivers allowed for custom
creation appear. There is no dynamic form-schema or second driver registry.

| Safe `connection` field | Type and meaning |
| --- | --- |
| `driverKind`, `auth`, `baseUrl` | Driver string, existing `StaticAuth`, validated URL |
| `credentialConfigured` | Boolean; neither the credential nor secret reference is returned |
| `headerNames` | String array; header values are never returned |
| `proxyUrl` | String without user info, or `null`; display only, never automatically written back |
| `proxyAuthConfigured` | Boolean; proxy authentication remains secret |
| `timeoutMs` | Positive integer or `null` for the driver default |

List errors use existing `ProviderRecordError {providerId, message}`; healthy
entries remain available. A builtin without `remoteModels` whose stored fingerprint
differs from its bundled definition gets a derived reseed diagnostic (§6), while
its admitted entry and deletion action remain usable. Name any colliding model
IDs; after their deletion, keep a restart-required diagnostic until startup applies
the definition. Seeding and this read share one Provider-owned candidate builder,
including remote-model substitution, so they cannot disagree about the candidate.
Do not persist another status or disable the entry.
Actions describe current availability, not authorization or a promise that a later
write succeeds. Model origin is derived from the distinct persisted lists in §6.
An account's existing catalog retains its version and descriptors while signed out
or changed; its models are unavailable and model writes wait for a current refresh.
A disappeared default's override cannot become a user-added model.
`source:"override"` means a default model with a stored descriptor override;
an edited user model remains `source:"user"`.

### 3.2 Writes and field operations

`ManagementProvider` below means the same edit snapshot as §3.1. Existing method
request/reply shapes remain except the explicit `ModelEdit.endpoint` addition and
typed errors in §3.4. Existing writes retain `mutating:true`, `interruption:Drain`.

| Method | Request | Reply / behavior | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.create`  | `{providerId, displayName, connection, models: ModelEntry[], enabled}` | `ManagementProvider`; create-only, caller-generated fresh UUID, existing ID conflicts | true | `Drain` |
| `provider.update`  | `{providerId, expectedVersion, patch}` | `ManagementProvider`; validate and commit one partial update transaction; a patch that changes `baseUrl`, `credential` or `headers` also replaces the record's own models with the new endpoint's listing in that same compare-and-set write, and `modelsListed` says what it did; a `network`-only change saves alone | true | `Drain` |
| `provider.probe`  | `{providerId, connection}` | `{models: ModelEntry[]}`; §3.3, no record writes or version check | false | `Drain` |
| `provider.models.create`  | `{providerId, expectedVersion, model: ModelEntry}` | Existing catalog; separate user-added list; duplicate default or user ID rejected; add enabled and remove any stale `modelConfigOverrides`, `disabledModelIds`, `modelPreferences` and `endpointOverrides` entries for that ID in the same transaction | true | `Drain` |
| `provider.models.delete`  | `{providerId, expectedVersion, modelId}` | Existing catalog; only user-added items; remove that ID from `disabledModelIds` and clean owned preferences/routes/selections in the same transaction | true | `Drain` |
| `provider.preferences.update`  | `{providerId, expectedVersion, modelId, patch}` | `ManagementProvider`; effort/tier independently set/clear, validated against the model | true | `Drain` |

The client generates and retains the UUID before sending. `create` accepts only
a canonical lowercase hyphenated UUID; another value returns
`provider_invalid_config`. It never derives identity from the name. Existing
bundled and imported non-UUID IDs remain valid for every other method.
After an uncertain reply, reread that exact ID. A repeat
create with the same ID returns `provider_conflict`, never overwrites or duplicates
the record. A new creation after deletion uses a fresh UUID. No identity tombstone,
cross-deletion version clock or new idempotency service is introduced.

Creation `connection` contains required `driverKind`, `auth`, `baseUrl`, optional
`network: ModelNetwork`, `headers: Record<string,string>` and `credential` using
existing keep/clear/set/set_aws/set_ref operations. Absent headers/network mean no
overrides for a new record; absent credential means none. Enabled keyed records
require a valid credential. `create.models` carries only probe results, stored as
defaults; it is empty when discovery is unsupported or no models were discovered.
Refresh may replace these defaults, and `models.delete` cannot remove them.
After creation, add manual entries through `provider.models.create` so they live
in `userModels`, survive refresh and can be deleted. Template entries use that
same method: copy the complete public `ModelEntry` identified by
`{providerId, modelId}`; persist neither a template reference nor a client-side
bundled catalog. Clients never reconstruct descriptors from `ProviderModel`.

An update that changes `baseUrl`, `credential` or `headers` — the endpoint or
the account the record reaches — lists the models of the new endpoint when the
record's models are its own — the bundled catalog does not define the provider,
or `remoteModels` is already true — and commits the listing with the connection
in one compare-and-set write. The listing is taken outside the write and keeps
`refresh`'s user-model conflict rule. An invalid patch is rejected before
anything is listed, as `provider_invalid_config` with its reason. A failed
listing fails the update and writes nothing, so re-pointing never leaves the old
endpoint's models on the new one. A disabled record whose models are its own
drops them in the same write, without a request: they are the old endpoint's,
and the empty, listable catalog is what the picker's refill fills after the
record is enabled. A `network`-only change — a proxy or a timeout
— saves alone and lists nothing: it changes how the endpoint is reached, not
what it serves, and requiring a listing would make those settings unsavable
whenever listing fails. A driver that cannot list saves the connection alone. A
record whose models are the bundled catalog's keeps them through any connection
change, and later bundled updates still reach it;
`dimcode models PROVIDER --refresh` stays the explicit way to replace them. A
stored listing sets `remoteModels` as `refresh` does. The reply's `modelsListed`
says which of these happened.

| `patch` field | Operation | Writable records |
| --- | --- | --- |
| `displayName` | Nonempty string | Custom only |
| `baseUrl` | Validated URL string | Static-authority builtin/custom |
| `enabled` | Boolean | Local builtin/custom records |
| `credential` | Existing keep/clear/set/set_aws/set_ref; `keep` changes nothing, exactly as an omitted credential does, so a keep-only patch is the empty patch | Static authority only |
| `headers` | `{name, action:"set", value}` / `{name, action:"remove"}` array; omitted names keep | Static-authority builtin/custom |
| `network.proxy` | `{action:"set", value:URL}` / `{action:"clear"}` | Local builtin/custom; existing authority validation applies |
| `network.timeoutMs` | `{action:"set", value:positive integer}` / `{action:"clear"}` | Local builtin/custom |

Omitted fields keep their value; null, unsupported fields, duplicate/conflicting
header names and invalid operations fail at the input boundary. Header names are
case-insensitive there. An empty `provider.update` patch returns
`provider_invalid_config` without changing the record or version.
Proxy and timeout update independently; replacing a proxy
URL explicitly replaces its authentication. Builtin identity/name/driver/auth and
default descriptors remain readonly. Custom `driverKind` and `auth` are fixed at
creation too; changing them requires a new Provider and explicit selection.
Account-owned credentials and connection fields use their owner, not this patch.
The preference patch is
`{reasoningEffort?: {action:"set",value:string} | {action:"clear"}, serviceTier?: {action:"set",value:string} | {action:"clear"}}`.
Reject an empty patch or a value the model does not support with
`provider_invalid_config`. Unknown fields and malformed operations fail at the
input boundary (§3.4). Clear means the catalog default at subsequent
explicit adoption; each field changes independently. Validate only values set by
this patch, not retained values in the other field. Saved preferences are user
choices (including imported user configuration); discovery supplies supported
options, not those choices. Descriptor edits, route changes and refresh never
automatically delete unsupported saved fields. Explicit adoption reports the
rejected saved preferences; the user can set or clear each field explicitly.

Existing `delete` keeps its builtin-disable/custom-delete behavior. `models.edit`
keeps `ModelEdit` and adds `endpoint: {action:"set",value:NewApiEndpoint} |
{action:"clear"}` for static New API models only, restricted to §6's executable subset.
Override wins over discovery; clear returns to discovery without deleting metadata.
Unknown upstream endpoint strings remain descriptive, not executable routes.
An edit that sets or clears only `endpoint` writes only `endpointOverrides`; it
does not create or change a descriptor override. `models.reset` removes only the
descriptor override and keeps the endpoint override; `endpoint: clear` removes
only the route override. `models.reset` restores an existing default; user entries
use delete. `refresh` updates default models while preserving user models,
overrides, preferences and explicit disables, with its existing
admitted-configuration conflict check. A managed refresh for a different account
clears preferences together with descriptor overrides and disables; a same-account
refresh preserves them.
Descriptor edits to a user model replace that model's `userModels` entry; they
never write the user ID into `modelConfigOverrides`. `models.set_enabled` accepts IDs
from both defaults and user models. These operations update the effective catalog
and routes from the same record; endpoint overrides remain their separate map.
Managed account catalogs do not gain user-added routes/models through these methods.

Deleting a custom Provider clears its Provider-owned selections in the same transaction.
Builtin delete disables its record and retains its scoped choice; unavailable choices
are skipped during subsequent default resolution.
Deleting a user model likewise clears its owned selection/preferences/route and
its `disabledModelIds` entry. Creating a user model starts enabled, with no
descriptor override, preferences or endpoint override, and drops stale entries
for the same ID, so it cannot inherit a disappeared model's settings.
Agents profiles, SessionTitle choices and adopted Sessions retain their exact
IDs: their later use reports not found/unavailable. Provider neither rewrites other
owners' records nor silently substitutes a model. A new Provider uses a fresh ID
and does not rebind those references.

### 3.3 Probe, preference adoption and usage

For a new connection, probe carries the client's fresh canonical UUID and the
complete creation `connection`. The UUID supplies the existing secret host's
Provider context without creating a record. The complete connection shape selects
the new-draft probe even when its ID already exists; `credential:keep` then has no
stored credential to keep. For an existing connection, probe
carries its ID with `connection` as the §3.2 patch. Provider reads the current
record once for that patch, merges it in memory and obtains credentials through the original
authority. Probe has no version check; it writes nothing and its result does not
establish a version for a later write. Apply the same
URL/header/network validation as create/update **before** any request. A changed
base URL sends the stored credential to that new host even without saving; the
UI states this when testing the edit. Local management already holds that authority.
No temporary record, catalog, selection or credential is persisted. Unsupported
listing returns `provider_unsupported` and still permits manual models. Results and
errors disclose no secrets or raw response body. A stale draft probe continues to
its request timeout; the client discards its reply. Disconnect/navigation cannot
cancel it. Owner unload uses the existing cancellation token and drains the call.

Provider persists the existing `ModelPreferences` for clients only. The
[F3 client](../model-configuration.md) sends
the exact `{providerId, modelId}` with preferences read for that pair to
`session.create` or `session.model.update`. An unavailable explicit target fails;
report it and do not retry without the target or fall back to another model.
The server never applies saved preferences implicitly. Omitted preferences with
an explicit target still start at catalog defaults; untargeted F3 updates still
change only supplied fields on the current adopted model. `/fast` remains
Session-only; saved `serviceTier` is a persisted fast/tier choice applied only by
an explicit client request. Consumer-specific models do not inherit chat preferences.

Usage reuses implemented `account.usage {providerId} -> Usage` in the `account`
namespace, forwarding one call to the optional package's existing `usage` method.
The [public Usage DTO](../../../crates/dim-app-server/README.md) and account errors
remain unchanged. Provider adds no usage settings, probe, CAS, cache or states for
static providers, and no account generation recheck or managed host-API extension.
Missing metrics remain absent; reported unlimited buckets retain the DTO's explicit
`unlimited` convention.

`hello.features` advertises `account.usage` whenever the namespace is active; it
does **not** prove the loaded package implements usage. For an entry returned by
`account.providers`, the client may offer a query when that method is advertised.
An absent package/method can return `account_provider_unavailable`; display that
safe error without relabeling it "unsupported" or affecting model/login behavior.
`unknown_method` describes an older daemon, not package capability discovery.
Any more precise discovery/error contract remains with the account owner.

### 3.4 Errors, admission and compatibility

Management-facing Provider operations return a Provider-local error enum in
`dim-app-server`; `provider/management.rs` maps those variants directly to the
codes below. The shared `HostErrorKind` is not extended, and generic host-error
strings are not parsed to recover business types.

An update containing only empty `headers:[]` and/or `network:{}` is an empty
patch and fails without advancing the record version.

Provider business errors use JSON-RPC code `-32000`, a safe message and
`data.appError`: `provider_not_found`, `provider_conflict`,
`provider_invalid_config`, `provider_unsupported`, `provider_auth_required`,
`provider_auth_failed`, `provider_transport_failed`, `provider_cancelled`,
`provider_failed`. Classify typed owner errors, not their message text. This also
maps not-found, CAS/admitted-refresh conflict and validation errors from retained
`delete`, `models.edit`, `models.reset`, `models.set_enabled`, `select` and
`refresh`; their requests/replies and JSON-RPC code do not change. Unknown methods
and malformed RPC parameters retain their existing generic boundary errors.
Never include tokens, secret references, headers, proxy auth or raw upstream bodies.

New methods, endpoint editing and typed owner errors ship as the discoverable
`provider.management` interface unit. `provider.save` is replaced by create/update
and removed with its CLI callers; there is no fallback save/direct-database path.
Rpc retains `service_reloading`/`service_unavailable` and ordinary admission.
All accepted calls remain owner-tracked and drain on unload. Disconnect only loses
the reply; it never rolls back committed writes. Reread uncertain writes rather
than replaying them. Provider updates do not reload the loader row.

The native service and RPC share the same create, partial update and model operations.
New custom Providers use canonical UUIDs. Existing bundled and explicitly imported
non-UUID identities remain editable through their accepted partial-write paths.

### 3.5 Declared model limits

A model's context window and maximum output are facts a source declares. Provider
never assumes either. Per limit, the declaring source is the one the catalog
already records:

| Source | Recorded by | Scope |
| --- | --- | --- |
| Provider descriptor | `capabilityOverrideFields` on the stored descriptor; a non-positive descriptor value reads as absent | The model that descriptor describes |
| Bundled catalog | `metadataSource` naming the matched bundled provider: an entry declares a limit only where its `metadata.raw` does, and the bundle drops every other limit when it is loaded; that provider's `contextWindowOverride` and `contextWindowFallback` metadata stay declarations | The models the lookup matches, or every model of the provider |
| User | The same `capabilityOverrideFields` field on the descriptor override or user model: `models.edit` writes it for each limit it edits, and either legacy importer for an imported value it keeps | The model the override names |

Absence is a value of the type, not a number. The catalog carries
`CatalogCapabilities`, whose two limits are optional and absent when no source
declares them; the stored descriptor, its overrides, user models, the projected
entry and the managed catalog's model all use it. The Session-facing
`ModelCapabilities` keeps both limits required and positive and has no default
value, so no projection, fixture or fallback can invent a window. One conversion
turns a catalog entry into `ModelCapabilities`, and it refuses while either limit
is absent; that conversion is the adoption path, so an undeclared model cannot be
adopted.

Removing the default is a breaking change for an extension crate outside this
workspace that built a value through `ModelCapabilities::default()` or
`..Default::default()`: those no longer compile. The replacement is
`ModelCapabilities::with_limits` (`crates/dim-agent-types/src/model.rs`); a value
that also sets another field keeps the struct literal and takes that constructor
as its base.

On the wire, `ProviderModel.capabilities` in `provider.catalog` and
`models[].model.capabilities` in `provider.management` carry `contextWindow` and
`maxOutputTokens` as optional numbers: an absent key is the undeclared value, and
`missingLimits` names it. Every in-tree client reads both as optional: `dim-cli`
decodes catalog entries as `CatalogCapabilities`
(`crates/dim-cli/src/configure.rs`, `crates/dim-cli/src/session_host.rs`),
`dim-acp` lists models by ID, name and unavailable reason
(`crates/dim-acp/src/config.rs`), the Web workspace reads the same three fields
(`web/src/rpc/read.ts`), and `dim-client` carries the reply untyped.
`PROTOCOL_VERSION` stays 3 because the clients ship with the daemon, so the build
that writes such a catalog is the build that reads it, and the Web entry refuses
assets built for another protocol version
(`crates/dim-app-server/src/web_entry.rs`, `web/src/rpc/client.ts`). A protocol-v3
client built before this change that requires both numbers cannot decode a
catalog holding an undeclared model; no `hello.features` entry negotiates the
shape.

The reads report what is missing: `ProviderModel` and `ManagementModel` carry
`missingLimits`, and `unavailableReason` is `limits_undeclared` when it is the
first reason that applies. A model missing either limit is never selectable.
Every refusal uses the existing unavailable path: `select`, `configuration` (an
explicit target, default resolution and an Agents profile), `apply_preferences`,
`preferences.update` and the per-request availability check before credentials.
The request-path error names the missing limits. Enabling or disabling such a
model changes nothing about its selectability.

The effective catalog supplies an absent limit from what a source declares: the
bundled match for a default or a user model, the same bundled fallback for a
managed catalog model, and the base entry for a descriptor override that does not
declare the limit itself. Probe, refresh and creation store only declared values,
and `models.reset` returns a model to its discovery result; when that result
declares nothing, the model is undeclared again.

Format v24 stored what a client supplied without a mark, and its probe filled
each missing field with the assumed pair (128000 context, 16384 output). The
format upgrade and both legacy importers therefore decide each field of a user
model, a static record's descriptor override or a custom provider's own model
alone, through one function: a limit `capabilityOverrideFields` marks or
`metadata.raw` declares is a declaration; an unmarked limit equal to the
assumed default is removed, because it may be the probe's fill; any other value
can only have been typed or listed, so it is kept and marked. The rule prefers
asking again over keeping a possible assumption: a user who typed exactly
128000 or 16384 sees that field listed as undeclared after the upgrade and
declares it again with `--set-missing-limits` or `/models`.

These writers set the mark: `models.edit` for each limit it edits, and both
legacy importers for each imported value they keep, descriptor overrides
included. `provider.create`, `provider.models.create` and the earlier probe set
none. A static record's descriptor overrides take the per-field rule because
the importers copy the legacy `modelConfigOverrides` as they are, without a
mark, and those values are user input; a managed record's overrides take the
strict rule because only `models.edit` writes them and it marks every limit it
edits.

The same split applies in both legacy importers: the `models` of a `bundled` or
`remote` legacy row are listing output and take the strict rule, while user
models and the row's descriptor overrides take the per-field rule. The `models`
of a `user` row is not the row's input but the seed the record starts from — the
previous record's models or, without a target, the bundled definition's — so the
import leaves it exactly as that seed states it, with no mark and nothing
dropped; a bundled definition's seed is already strict where the bundle loads.

A Session keeps its adopted capability snapshot. One whose model becomes
undeclared resumes and stays readable; its next request fails in the resolver with
the missing limits named, before credentials or network I/O. After both values are
declared, selecting the model adopts them. Until that selection `/usage` reports
the adopted snapshot, as for any descriptor edit (§6).

The managed account catalog follows the same rule. A model that neither the relay
nor the bundled catalog declares a limit for is projected with that limit absent
and keeps its `unavailableReason` by the
[managed catalog projection](../../../crates/dim-oauth/src/models.rs); its own
family rule still declares a window where it applies. The host admits an absent
limit only on a model that carries a reason. The
[model-file profile](../../../crates/dim-cli/README.md#api-key-configuration) is
unchanged: it already refuses a model entry that lacks either value, naming the
entry ([#517](https://github.com/arcships/dim/issues/517)).

A stored record carries the absence as an absent key. The
[#522 storage decision](native-storage-format.md#522-v26-格式决定) is the format
decision that introduced it, and §6 states what the upgrade does to records
written before it: a listed model keeps the item it came from in
`metadata.raw`, so a limit that item declares stays a declaration even when the
stored entry does not mark it, and only a limit no source declares is deleted.

### 3.6 Selectable models for process owners

A process owner that routes work by model capability cannot keep its own copy of
Provider's availability rules. Provider owns one side-effect-free read of the
models it would serve, with the identifiers and declared capabilities a routing
decision needs. This read is a process host port, not a management method: it is
not on the `provider` namespace, is not served over RPC, and does not change
`provider.catalog`.

**Why a port rather than a method on `ProviderService`.** A consumer plugin lives
in its own crate, and `xtask check` rejects a dependency on `dim-app-server`, the
composition layer, so the consumer cannot name `ProviderService`. The port and its
data type live in `dim-agent-capabilities`, which Provider and the consumer both
depend on; registering it as a process name lets the consumer declare
`Keyed<SelectableModelsHost>` and the catalog resolve it. The dependency points
from the extension to the core, as [ADR 002 D3](../../adr/002-plugin-scope.md)
requires, and Provider keeps the implementation, the catalog and the rules.

**Contract (language-neutral).** A process host port, no instance key, provided
by Provider: operation `selectableModels`, asynchronous, request `{}`, reply
`{models: ModelCapabilitySnapshot[]}`. The element is the existing
`ModelCapabilitySnapshot` — `{target: {providerId, modelId}, capabilities:
ModelCapabilities, reasoning?, serviceTier?}` — not a new model type: it already
carries the model and provider identifiers and the declared capabilities, and it
is the descriptor execution adopts. A selectable model always declares both
limits (§3.5), so the existing `capability_snapshot` conversion always succeeds.
The caller passes its cancellation token; cancelling it cancels the read and
returns `Cancelled`.

**Rules it applies.** The candidates are the providers default resolution
considers: every account provider the managed package advertises and every static
provider `provider.list` returns. Each model comes from the same per-provider
catalog read as `provider.catalog` (`catalog_for`), so bundled substitution,
descriptor overrides, user models, routes, disables and account state are the ones
execution sees. A model is included only when the existing
`ProviderCatalog::selectable` accepts it: `unavailableReason` is absent and its ID
is not in `disabledModelIds`. That reason already covers every exclusion: a model
with `ModelStatus::Unavailable`, a model that declares no `text` modality, a
non-chat model on a driver other than `new-api-router`, a disabled Provider
record, a model with no route, a signed-out account, a missing static credential,
and an undeclared limit (`limits_undeclared`). The read adds no second predicate.
A model appears only if `select`, `configuration`, `apply_preferences`, the
per-request check and `auxiliaryModel` accept it at the same read. Execution
remains authoritative: a later write can make a model unselectable, and the
consumer passes the ID back to Provider, which re-checks.

This read is not [#560](https://github.com/arcships/dim/issues/560)'s validation
contract. That contract takes a persisted native record and an explicit model ID
and answers whether that selection can execute; this read takes no input and
enumerates what is available now. Both are defined on the same catalog and
selectability predicate, so they cannot disagree about a model.

**Snapshot and consistency.** One call reads each provider once. No transaction
spans providers, there is no version and no cache: a concurrent `provider.*` write
may be visible for some providers and not others, and the next call returns the
new state. It makes no live request: it never refreshes or lists a provider
endpoint, and it writes no record, selection or preference. It uses the same
account-status read as `provider.catalog`. The reply is derived; a consumer must
not persist it or treat it as an adopted snapshot. Adoption stays with model
selection, which resolves a configuration once.

**Errors.** The read has no error code of its own; it returns Provider's existing
errors: `Cancelled` when the caller cancels the token it passed, `Failed` with
`HostErrorKind::OwnerClosed` while Provider unloads, and `Failed` when Provider
storage cannot be read. A provider whose own record or account status cannot be
read is omitted, as default resolution omits it; it never hides the others and the
read returns no per-provider error list. A caller that needs a provider's
diagnostic uses `provider.list` or `provider.management`.

**Consumers.** Modality computes each operation's options and its automatic
default from this read: it intersects these IDs with the operations its collected
adapters declare and reports "no adapter for the configured model" when the
configured model is absent. It does not call `provider.catalog` and keeps no
availability predicate of its own. The reply carries IDs and capabilities only,
not a display name: a client that renders an option joins the ID with the model
list of `provider.catalog` or `provider.management`, and Modality copies no name.
Provider gains no per-consumer key, setting or filter: the read has no input, so a
caller that wants one provider filters the reply.

**What does not change.** `provider.catalog`, `provider.management`,
`provider.select`, model selection, adoption, preferences and their errors; the
client protocol and `PROTOCOL_VERSION`; persistence and its format; Timeline,
Dispatcher, callbacks, Session and branch parts; and the driver, route and
non-chat rules. The read adds no state, cache, notification, crate or module.

**Rust binding.** The port is defined in `crates/dim-agent-capabilities/src/host.rs`
beside `ModelSelectionHost`. Provider provides it in `ProviderPlugin::apply`, and
the App Server catalog registers it as the Rust-only name `selectableModelsHost`
(no cross-language binding before
[#4](https://github.com/arcships/dim/issues/4)):

```rust
pub trait SelectableModels: Send + Sync + 'static {
    fn selectable(
        &self,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Vec<ModelCapabilitySnapshot>>;
}
pub struct SelectableModelsHost(pub Arc<dyn SelectableModels>);
```

The owner implementation is `ProviderService::selectable_models`; `ProductResolver`
delegates to it. A consumer declares `Keyed<SelectableModelsHost>` in its `Deps`;
no consumer reads `ProviderService` directly. A non-Rust consumer reaches the port
by name when the runtime-services-by-name requirement
([#4](https://github.com/arcships/dim/issues/4)) is delivered; that requirement is
raised once in [Modality §9](../plugins/Modality.md#9-requirements-raised-to-mechanism-owners)
and is not raised again here.

## 4. Client integration

```text
Desktop / CLI              Local App Server             Provider / account
    | hello: actual methods       |                            |
    | management / selection ---->|---- safe snapshot --------->|
    | edit unsaved draft          |                            |
    | probe --------------------->|---- validated I/O --------->|
    | create(UUID) / CAS patch --->|---- atomic owner write ---->|
    | reread after own write ---->|                            |
    | explicit target + saved preferences -> existing Session adoption
    | reopen / reconnect: reread  |                            |
```

Use the local socket/pipe for the same product home; paired devices gain no
management authority. A CAS conflict preserves the draft and requires reread and
user confirmation, not automatic overwrite with a fresh version. One bad record
does not empty the list. Replies from an obsolete view/draft do not replace current
edits. Other clients' changes appear at the next read; no watch/replay is assumed.
`provider.select` changes a scoped default, not an existing Session; Session
adoption is a separate explicit action with its own outcome and exact target.

Native CLI migration includes `PROVIDER_METHODS` in
`crates/dim-cli/src/session_host.rs`, all save callers, and actual-daemon flows:
Add probes the unsaved connection **before** create, retains its generated UUID
for recovery, and never saves then refreshes as a test. Connection edits probe a
draft when requested, then update the changed fields; the update lists and
stores the new endpoint's models when the record's models are its own, and a
proxy or timeout edit saves alone. Disconnect sends credential
clear **and** `enabled:false` atomically, fixing the enabled-key-required rejection
of clear alone. Enable/disable use update; account login/logout retain their owners.
When listing is unsupported, connection edits still save and explicitly report
that the key could not be tested. Failed writes retain drafts; the subsequent
reread refreshes the record version and safe status, and the user confirms before
retrying. A repeated create whose UUID already exists opens that existing record
with its current version and the retained draft; it requires Apply instead of
reporting the draft as saved. `Ctrl+R` rereads a form after a failed list request
without discarding it.

The `/models` picker clears both saved preferences for its selected model with
`Ctrl+X`, using the displayed catalog version for CAS. It keeps the picker open;
Enter separately adopts catalog defaults. On conflict it refreshes the list and
the user retries. The refreshed list reaches the picker before the operation's
result notice, so a retry after that notice uses the refreshed catalog version.
The native `dimcode models PROVIDER --clear-preferences MODEL`
command rereads the management version and performs the same partial update.
Neither action changes an adopted Session model. Discovery and adoption errors
keep historical preferences until this explicit user action.
`←` and `→` cycle the selected model's advertised reasoning efforts. Enter
adopts that model; a cycled effort is saved as its `reasoningEffort` and sent
with the model's other saved preferences.

Models whose catalog entry has `zdr: true` show a `ZDR` tag before the
reasoning effort, including in the fixed subagent model picker. Searching
`zdr` includes these models. Missing or false values show no tag; replacing
the catalog list replaces the tag with the latest server value.

The picker also fills a static provider whose catalog holds no models through
`provider.refresh`, once per version of its catalog in a connection, when
`provider.list` says its models can be listed, and shows what the refresh
stored. A successful refresh records the version it wrote, so a listing that
comes back empty is not asked for again; a write to the record, such as an
enable or a re-point, asks once more. A failed read or a failed refresh keeps
the provider's row, and a reply that arrives after the picker closed only
records the fill: the next open reads it.

An undeclared model is not a picker row. Every provider with at least one such
model keeps one muted row after its model rows: it counts them and names the
command that declares them. When the provider offers nothing selectable, that
row replaces the empty-provider row instead of joining it.
`dimcode models PROVIDER --set-limits MODEL --context-window N
--max-output-tokens N` declares both limits of one model through `models.edit`,
and `--set-missing-limits --context-window N --max-output-tokens N` declares them
for every model the list marks `undeclared`, an enabled model whose missing limits
are its only blocker; a disabled model or one unavailable for another reason is
not written. Both named
values are required with either form. Neither form combines with `--refresh` or
`--clear-preferences`. Both read the record version first, chain the version each
`models.edit` reply returns, and then print the catalog as `models PROVIDER`
does. The list marks a model whose limits are undeclared as `undeclared` and
names the declaring command below its rows.

```text
Select a model
> Provider / model          ZDR  ‹ high ›
Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close
```

```text
Providers                         Provider detail
[ Search... ] [+ Add]             Name / source / connection status
  Builtin A       Connected       [Edit] [Disable] [Disconnect]
  Custom B        Disabled        Credential: Stored [Keep | Replace | Clear]
  Account C       Signed out      Models [Search...] [Add] [Refresh]
  Broken D        Error           Model / Enabled / [Edit] [Reset/Delete]
                                  Account usage: result or actual query error
                                  Draft [Test connection] [Cancel] [Save]
                                  Conflict: reread / retain draft
```

Reuse existing layouts/components. CLI provides equivalent actions without
copying Desktop layout or inferring capabilities from hardcoded Provider names.

## 5. Requirements raised to mechanism owners

None. Existing named management calls, CAS, Provider storage, metadata and owner
lifetimes suffice. No core, Rpc, rutis, assembly or account host-API expansion is
requested. S4 remains implemented. Client notifications, if wanted, require a
separate decision covering Settings too, not a prerequisite here.

## 6. Persistence, existing Sessions and migration

**Format decision: v19, unchanged SQL schema.** These optional top-level fields
carry user configuration; missing maps/lists mean no corresponding user data:

| Struct / JSON field | Stored value and use |
| --- | --- |
| `StaticProviderRecord.user_models` / `userModels` | `ModelEntry[]` for user-added models; `models` remains defaults and `modelConfigOverrides` remains overrides only |
| `StaticProviderRecord.model_preferences` / `modelPreferences` | Map of model ID to existing `ModelPreferences` |
| `ProviderRecord.model_preferences` / `modelPreferences` | Same preference map for managed catalogs |
| `StaticProviderRecord.endpoint_overrides` / `endpointOverrides` | Map of model ID to the existing `NewApiEndpoint`; only `openai`, `openai-response`, `anthropic` and `gemini` are executable overrides here; separate from discovered `endpointTypes` |

The two model limits carry their own, later format decision: the
[#522 storage decision](native-storage-format.md#522-v26-格式决定) makes an
undeclared limit an absent key, which a reader of the earlier format cannot
parse either way, and upgrades stored records in place instead of reusing a
version. The decision states the format version this change introduces, the
rewrite the startup transaction performs, and the old binary's startup refusal.
§3.5 states how the effective catalog then supplies what a source declares.

The [storage contract](native-storage-format.md#329-格式决定) records the v19
decision and its downgrade loss. No fields are added to `ModelMetadata`,
`ModelNetwork`, `StaticCredential` or the closed endpoint enum: old v19 readers
reject unknown nested fields there. `ModelPreferences` also denies unknown fields,
and `NewApiEndpoint` stays closed. Binaries supporting these new top-level maps
therefore reject later unknown preference fields or endpoint values inside them;
any such addition requires another explicit format decision. Model
source needs no extra tag: persisted membership in defaults/overrides/userModels
defines it. Build the effective static catalog **and** driver routes from defaults
plus userModels, apply descriptor overrides to their owned defaults and endpoint
overrides before deriving routes. Preserve these fields on refresh, writes and
bundled-definition reseeding; managed preferences are preserved only for a
same-account refresh (§3.2). User IDs must not collide with default/override IDs;
a refresh introducing a collision fails without replacing the admitted catalog,
and an update that lists the new endpoint follows the same rule: it fails, names
the colliding user models, and writes nothing. Delete the colliding user model
through `provider.models.delete`, then retry the refresh or the update.

Bundled reseeding must not fail seeding or Provider activation on that collision.
When a changed fingerprint's candidate would collide with a user model, leave
that record's admitted definition, fingerprint and version untouched, and continue
seeding the other records in the existing transaction. This applies to the actual
replacement candidate: a record with `remoteModels` retains its remote defaults.
Expose the collision through §3.1's existing per-entry error list, keeping the
Provider and `provider.models.delete` usable. After explicit deletion, the next
startup can apply the bundled definition. Other validation failures retain their
existing failure behavior. A replacement user model needs an ID distinct from
the incoming bundled defaults; no
automatic rename, overwrite, persistent warning or new migration service.

Old v19 binaries can read these records but drop new top-level fields on rewrite,
including automatic `seed_bundled` rewrites when fingerprints change. That downgrade
loss is accepted explicitly, not limited to user edits. Disposable-database
acceptance must run an old v19 reader and its rewrite path on a new record.
No storage-owner gate, deletion history or format bump is added. Unknown database
formats retain their existing rejection and byte-preservation rules.

| Edit | Effect on already-adopted Sessions |
| --- | --- |
| Credential, base URL, headers, proxy, timeout | Current connection is resolved on the next request; already-issued I/O uses its admitted connection |
| Descriptor/capability edits or saved preferences | Adopted snapshot stays unchanged; changes apply on the next explicit adoption, preferences only when passed |
| Disable/delete Provider or model | Next request/adoption fails unavailable/not found; no forced switch or cross-owner reference rewrite |
| A limit declared, or its source removed | An adopted Session keeps its snapshot. With a limit undeclared, the next request fails before credentials or network I/O and names the missing limits; after both are declared, selecting the model adopts them (§3.5) |
| Effective route change from refresh or an endpoint override | A different adopted route stamp fails with a diagnostic to select the model again. An unstamped Session uses the current route on its next request; either edit can break protocol-bound history or lose effort/tier options mapped for the old protocol |

For static New API selection, Provider stamps the chosen `ModelProtocol` string
in existing `ModelSelection.metadata.extensions["providerProtocol"]`. Prompt and
request preparation already carry metadata through adoption to `ModelRequest`.
When the stamp is present, Provider compares it with the current effective route
before resolving credentials/network I/O. A missing stamp means no check: Sessions
adopted before the upgrade keep working while their route is unchanged. Refresh
and endpoint overrides retain the unstamped route-change risk described above.
Untargeted F3 preference updates preserve the stamp or its absence, checking a
present stamp before mapping options. Every new selection resolved by
`ProviderService::configuration` creates the stamp: an explicit target, an Agents
profile model or default model resolution. Untargeted F3 `apply_preferences` and
Collaboration inherit preserve the prior stamp or its absence. Inherit copies an
already adopted selection without creation-time resolution. A mismatched present
stamp fails before credentials/network I/O at the member's first request; legacy
absence retains the unchecked-route risk above. Only Provider-owned selection
metadata is copied; the member's request producer supplies its own Session/run/trace/turn
identities. This is Provider-owned data in the existing metadata extension, not a
new core field, epoch or registry. Managed route/lease checks remain unchanged.

Legacy TypeScript import stays explicit, read-only and per-entry:
`imported`/`already_imported`/`rejected`, and Provider owns the frozen mapping
below as one pure conversion of a legacy row: the row's fields, the existing
target record and the bundled catalog in, the native record and the omitted
source fields out, or a rejection reason. `provider.import_legacy` and the
Migration provider step both call it; Migration keeps its snapshot, receipts,
ledger and report. Map
connections, defaults, user overrides, user models, disables, preferences and
scoped selections explicitly. `activeModelId` is only a hint and cannot
overwrite an existing scoped choice. A field with no native counterpart is
dropped and reported as omitted, and the entry imports: nonempty static usage
settings, `retryConfig` and `capabilityDefaults`, and metadata keys outside the
mapped ones. Unknown descriptor fields and conflicting enable/disable lists
change what a model is and still reject the entry. Never overwrite an existing
target, copy OAuth grants or resurrect deletion on repeated import. Drafts,
templates, usage replies and resolved credentials are not persisted. Register
cleanup at acquisition, close admission, cancel owned I/O, release leases and
settle accepted calls before removing services.

The frozen legacy schema uses `modelsSource` to distinguish model origins:

| Legacy field | Native record |
| --- | --- |
| `modelsSource: bundled/remote`, `models` | Default descriptors; only `remote` sets `remoteModels` |
| `modelsSource: user`, `models` | `userModels`; an existing builtin keeps its native bundled defaults, and collisions reject the entry |
| `modelConfigOverrides` | Matching user descriptors replace that user entry; remaining items stay descriptor overrides, including disappeared defaults, and never become user models; every limit one states keeps its value and gains the mark (§3.5) |
| `metadata.enabledModelIds/disabledModelIds` | Explicit disables over defaults and user models; conflicting lists reject the entry |
| `metadata.modelPreferences` | The strict existing `ModelPreferences` map |
| `activeModelId` | A per-Provider hint; no authority to create or overwrite a scoped selection |
| `provider_selections` | Explicit scoped choices, subject to existing target and import-ledger checks |

A `builtin` row maps onto the builtin of its id. Without a target the import
starts from that builtin's bundled definition, exactly as an import over a
seeded target does, so a `builtin` row whose id has no bundled definition is
rejected; over an existing target the target must be the pristine seeded
record. A `custom` row whose id belongs to the bundled catalog is rejected.
Both entries parse and validate an existing target record with the same
parser, and a target that does not validate rejects the row. Migration
classifies a non-pristine target as `Conflict` with no reason before the
conversion runs; otherwise both entries report the conversion's reason.

`provider_selections` share one conversion as well: a workspace scope is the
canonical path of an existing directory, and the chosen model must be available
in Provider's catalog for the imported record. A selection whose directory no
longer exists is rejected with that reason; it could never be matched. A
provider row an earlier import recorded stays usable for its selections: both
entries read that record from the import ledger, and a stored record that
cannot be read rejects the selection with the read error. The
`provider.import_legacy` reply adds `omitted` to imported items, the source
field names the conversion dropped, and imports selections without it; the
field is additive and protocol v3 is unchanged.

Null or empty objects/arrays in those settings carry no configuration and are
not reported. Source bytes remain unchanged.

## 7. Acceptance and delivery

| Gate | Required observable evidence |
| --- | --- |
| Contract | Typed DTOs, client migration, explicit preference targets, route handling and frozen legacy mapping |
| Secrets/patches | Header edit preserves others, timeout edit preserves proxy auth, keep/set/clear/ref and authority boundaries, no secret in RPC/log/errors |
| Models/routes | Real calls to user-added builtin/custom models; create with probe defaults, then add/edit/enable/delete manual and template entries that survive refresh without becoming default overrides; an endpoint-only edit of an unmodified default keeps `source:"default"` and its descriptor follows refresh; any existing descriptor override is unchanged; reset keeps the route override and clear keeps the descriptor override; explicit/profile/default selections get stamps, untargeted F3 and Collaboration inherit preserve absence; inherit copies without creation-time resolution and checks a present stamp at the first request; changed routes from refresh and endpoint overrides reject mismatched stamps, reselection restores effort/tier; unstamped Sessions keep working while their route is unchanged, and both route-change risks are exercised; resolve refresh collisions by user-model deletion |
| Probe | Unsaved discovery, current-record draft merge without a version check, validated new host with stored credential, failure/timeout/unload without writes, stale reply discarded, no client-cancel claim |
| Preferences | Independent set/clear and restart; empty/unsupported values use typed errors while malformed patches use boundary errors; exact target with preferences; unavailable target fails without fallback; untargeted F3 and consumer choices unchanged |
| Model limits | A probed model the descriptor leaves without a limit is listed with `missingLimits`, refused by selection, adoption and preferences, and becomes selectable with its declared window in the adopted snapshot after the CLI declares both; a bundled match stays declared; a managed model without limits keeps its reason instead of a substitute window; the model-file profile still refuses an entry that lacks either value |
| Selectable read | The read equals the set `select`/`configuration` accept at the same snapshot for the same providers; a disabled model, a disabled Provider, an unavailable model, a model with no `text` modality, a non-chat model on another driver, a route-less model and a signed-out account are absent while their Provider still answers; each returned `capabilities.modalities` equals the catalog model's declared set; a model with an undeclared limit is absent rather than making the read fail; managed and static providers both appear; a mock endpoint receives no request; a write changes the next read and not an earlier reply; cancelling the token and Provider close use the existing errors |
| Accounts | Existing login/logout/usage; managed same-account refresh preserves preferences, descriptor overrides and disables, while a different-account refresh clears all three; actual advertised-method versus missing-package-method behavior, unchanged Usage DTO, no static usage config |
| Concurrency/recovery | Two-client typed CAS conflicts on new and retained writes; lost create reply recovered by canonical UUID; noncanonical create IDs fail while existing non-UUID IDs still work with other methods; repeated create conflicts; read errors preserve healthy entries/drafts |
| Storage/import | New record read and rewritten by old v19 code including reseed; bundled upgrade collides with a user-added builtin model: Provider activates, that record/definition/fingerprint/version remain unchanged, other records update and management reports the collision; explicit deletion keeps a restart-required diagnostic until startup applies the new definition; deleting a disabled user model clears its stale disable so the new bundled model is enabled; a user model created with a disappeared default's ID starts enabled without that default's descriptor override, preferences or endpoint override; valid/invalid/repeated imports and source preservation in disposable databases |
| Client flow | Actual source-built App Server and Desktop/CLI: draft → probe → create → model/select → call → edit → call → restart → disconnect/delete; no fake-daemon-only evidence |

Provider, transport, managed-account and source-built native CLI flows exercise
these contracts with isolated homes and actual HTTP requests. The native CLI flow
covers saved preference adoption, credential replacement, daemon restart and
disconnect/delete; the RPC flow covers CAS and recovery after a lost create reply.
Desktop UI acceptance remains with that client. Old-v19 read, write and automatic
reseed behavior is verified with a binary built from the accepted baseline and a
disposable database; see [the storage contract](native-storage-format.md#329-格式决定).

## 8. Rule check

| Rule | Application |
| --- | --- |
| §4.1 | Provider writes its own process state; Session adoption uses existing owner commands |
| §4.2 | Disconnect does not retract accepted work or committed writes |
| §4.3 | No before hook; boundary validation and owner business validation; no network wait under commit locks |
| §4.4 | No Provider branch in core/Rpc/Dispatcher |
| §4.5 | Owned related selections/models/preferences/routes commit together; existing adoption remains atomic |
| §4.6 | Management data does not enter model context; only existing adoption paths change configuration |
| §4.7 | Accounts use public methods; Modality reads selectable models through §3.6's core host port; other owners' references are not rewritten |
| §4.8 | §5 raises no mechanism requirement; existing metadata and v19 top-level JSON suffice; §3.6 adds no requirement, and a non-Rust consumer uses [#4](https://github.com/arcships/dim/issues/4) already raised by Modality |
| §4.9 | No new Timeline event or pushed process notification |
| §1 | Extend the existing process Provider; no Capability or wiring plugin |
| ADR 002 D1 | Process records belong to Provider; adopted branch state to its owner; drafts to clients |
| ADR 002 D2 | Reuse namespaces/resolver; no management plugin or second registry |
| ADR 002 D3 | Optional account failure affects its own operations; no core dependency |
| ADR 002 D4 | Business configuration is owner state; updates do not reload |
| ADR 002 D5 | §3 names fields, methods, mutability, interruption and errors independently of Rust bindings; §3.6 states the process read in neutral form, and its core host port is the binding a consumer declares |

## 9. Ablation review

The single management read supplies one edit snapshot; the unchanged catalog serves
selection. Create, update and probe respectively handle recoverable identity, secret-
preserving writes and unsaved I/O. Disconnect reuses update. Separate user-model
storage is required to distinguish it from an orphaned default override and make
it executable. The route stamp preserves the adopted protocol meaning using
existing metadata. A declared limit needs no new state: the marker and match the
catalog already records say whether a value exists, an absent value carries its
absence in the catalog type, and the reason is derived at the projection, so no
parallel model list, no second catalog and no `limits` entity is added. The
selectable-model read adds no state, cache, event or model type: it applies the
existing `ProviderCatalog::selectable` to the same per-provider catalog and returns
the existing `ModelCapabilitySnapshot`, so there is no second availability
predicate or parallel catalog. No notification, version-history service, template
entity, credential abstraction or second client configuration store is needed.
