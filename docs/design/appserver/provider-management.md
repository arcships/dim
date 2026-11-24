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
| Stored selection | One side-effect-free projection of an admitted static record and an explicit model ID; a data-boundary consumer reuses Provider's route, descriptor and selectability instead of rebuilding a catalog (§3.7) |
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
| Provider module boundary `stored_selection` (Rust) | Native data-boundary consumers such as Migration; §3.7 adds one pure function over an admitted record, not an RPC, host port or management method |
| Provider host port `SelectableModelsHost` (core `dim-agent-capabilities`), provided by the profile's model owner (Provider, or the configured model file) | In-process owners that route by model capability (Modality) and the `agent` tool's `models` action; §3.6 adds two reads, not a management method or RPC method |
| Provider host port `ModalityConnectionHost` (core `dim-agent-capabilities`), provided by the profile's model owner | Modality's process adapters for image generation, editing and enhancement; §3.6 defines one per-call resolution, not a management method or RPC method |
| Account namespaces | Account owners provide login, usage and logout. The [ChatGPT plan provider](chatgpt-plan.md) owns `account.chatgpt`. |
| `ProviderConnectionResolver`, `ModelSelectionHost` | Provider implements catalog/route decisions through existing types; no new core port |
| `auxiliaryModel.complete` | Existing caller-selected S4 contract, unchanged |
| `provider` Settings source, user scope (`provider.json`) | The Provider part registers it; it holds `userAgent`, the User-Agent override (§2, Model request identity), so a user's choice is a declared, validated document rather than a request-local input |
| Native CLI Provider flows | Migrate discovery, create, update and disconnect together with the new owner methods |

Business logic stays in Provider, not `connection.rs`, `protocol.rs` or a wiring
plugin. No Session Capability, notification stream, second catalog, credential
service or registry is added. The [managed-provider boundary](../../architecture/development.md#managed-provider-boundary)
continues to exclude private account implementations and credentials from this repository.

### Model request identity

Every chat model request dim builds through
[`build_model`](../../../crates/dim-agent-aimux/src/driver.rs) carries dim's own
identity in its Provider layer: the header layer whose User-Agent the library's
own `ai-sdk-<package>/<version>` and `ai-sdk-provider-utils/<version>` parts
follow. Media requests do not carry it yet: the media adapters build their
providers directly
([`dim-modality-aimux`](../../../crates/dim-modality-aimux/src/media.rs)) and
send none of the three headers. The default is
`User-Agent: dim/<version> DimAgent/<version>`, `X-Title: DimCode` and
`HTTP-Referer: https://dimagent.com/`. `dim` comes first because a gateway that
identifies its client by the first token names the entry Dim publishes;
`DimAgent` is the older relay identity, kept for those still matching it. The
version is the workspace version, read with `env!` rather than written out.

The identity is a provider-layer default, so a per-call header still wins:
a Provider's own `User-Agent`, `X-Title` or `HTTP-Referer` (in `record.headers`,
or in `DIM_CUSTOM_HEADERS` for a startup connection) replaces that header for
that Provider alone. Precedence, highest first: an impersonating path, a
Provider's own header, the override below, the built-in default, the library's
parts.

Two paths keep their own headers instead of this default identity:

- The managed `openai-codex` Provider sends the Codex CLI's own per-call
  `User-Agent`, `originator` and `OAI-Product-Sku` instead of this identity. Its
  token request sends the Codex CLI's `User-Agent` and `originator`. Being
  overridden is not enough for this one: `X-Title` and `HTTP-Referer` would
  still reach the wire from the provider layer, so its model is built without
  the identity headers at all.
  Its version is the Codex release the request must appear to come from; the
  numbers in `provider.rs` and `managed_oauth.rs` change together.
- The Dim OAuth relay keeps its own `DimCode/<version>`, `X-Title` and
  `HTTP-Referer` headers instead of the default identity
  ([`dim-oauth`](../../../crates/dim-oauth/src/http.rs)); its wire does not
  change.

The managed Grok Provider (`grok-build`) does not impersonate: it sends the
default identity like any other Provider.

| Override | Where | Notes |
| --- | --- | --- |
| `provider` settings source, `userAgent` | `provider.json`, user scope, owned by the Provider part | Replaces the whole User-Agent string; 1–512 printable ASCII characters, refused by the source's schema otherwise. The part serving one App Server holds it, so two Servers in one process keep theirs apart |
| `DIM_USER_AGENT` | Environment of the process that hosts models | The same validation; wins over the setting. Read once by the App Server command line, beside the other startup inputs, and passed to each Server it builds, so a value a request cannot carry fails that process's startup |

`DIM_USER_AGENT` wins over the setting because it is set for one run of the
process that hosts models, as environment overrides do elsewhere. A shared
daemon keeps the environment it was started with, so a new value takes effect
when the daemon restarts.

Both go in the provider layer too, so the library's parts still follow them.
The settings row serializes its read and its publish, so an older document
cannot replace a newer one.
There is no short mode: a value the request cannot carry is refused rather than
trimmed.

## 3. Management contract

### Session-local startup connections

[Model startup](../model-startup.md) adds a separate admission to the existing
protected-local `session.create`, `session.model.update` and
`session.model.preview` entries, not a Provider-management write. `startup`
contains optional `connection`,
`contextWindow`, `maxOutputTokens` and `autoCompactWindow`. Connection fields are
`apiFormat`, `baseUrl`, optional `apiKey` or `authToken`, and optional `headers`
(a name/value map). A compaction window is a positive integer or `"auto"`.
Remote entries reject this admission. Ordinary creation stays unchanged.

Provider admits the connection and model together. It uses the existing protocol
driver and bundled descriptor projection; a configured target keeps its catalog
descriptor. A direct connection prefers an exact model declaration in its
protocol's native catalog before generic descriptor matching, so a generic
gateway entry cannot hide native effort declarations. Explicit capacity
declarations apply to this adoption only and may
complete missing catalog limits without making a disabled or otherwise
unavailable model selectable. Effort and tier still require model declarations.
Startup execution and preference-only updates use the validated adopted
capacities, not the persisted catalog's missing limits. Ordinary selections
still require catalog declarations. Current availability, disablement
and route checks still apply; no capacity is written back to the catalog.
A bare model ID first uses the effective Provider if it offers that exact ID;
otherwise exactly one available Provider must offer it. Ambiguity is an error.
An individual catalog failure does not exclude healthy Providers from that
search; cancellation still stops it. An explicitly named Provider retains its
own error rather than falling back to another Provider. If the search finds no
match, its unavailable-model diagnostic includes the first catalog error, if any.

Provider retains credentials and all header values only in memory, under an
opaque source UUID. The adopted non-secret declaration contains that UUID,
protocol, normalized base URL, authentication kind and sorted header names.
URL user info, query and fragment are rejected. Source lookup requires the whole
declaration to match and never falls back to another Provider or account.
Header names use the HTTP case-insensitive grammar. Header values trim leading
and trailing space and tab before HTTP validation; internal whitespace and
colons are preserved. Duplicate canonical names and generated-auth conflicts
are rejected, matching the CLI's text-to-map admission.
Failed admission/creation releases unclaimed bindings. Sessions sharing a source
retain it independently; final business close releases it after requests settle.
Idle unload retains ownership; restore reinstates its close observer, and deletion
of an unloaded Session releases its ownership. Provider unload drops all bindings.
The product assembly declares the Session-directory dependency used for these
lifecycle notifications; standalone Provider assemblies do not require it.
Forks retain sources from their adopted branch snapshots before returning,
including a model adopted after the source Session's creation.

After process restart, a recorded source is unbound until the caller explicitly
supplies a connection. A matching protocol, normalized URL, auth kind and header
name set rebinds missing sources in that Session, including parent-cut branches.
Credential/header values may rotate, but a live source is never overwritten:
already running branches and members retain their captured connection. A changed
destination adopts a new source on main only. No Provider record, saved
preference, workspace selection, account lease or secret file is created.

### Connection-scoped Provider routes

The protected local Provider management contribution exposes `provider.routes.list`, `provider.routes.set` and `provider.routes.disable`. It identifies the caller through the existing [call channel](../contracts/session-boundary.md#a2-call-channel). List returns safe routing information and the caller's current overrides. Set takes `{providerId,connection}`, using the URL, protocol and full headers grammar of [startup connections](#session-local-startup-connections). Disable takes `{providerId}` and prevents adoption of that Provider by new Sessions on the caller's connection. It returns a boolean indicating whether the caller’s routing state changed. An unknown Provider and an already disabled override return `false`. The operations use the Provider management face and keep shared records, saved credentials and model preferences untouched.

Disable replaces the caller's existing override even while the shared Provider is unavailable for temporary routing. If the shared Provider becomes available again, the caller's disabled setting still applies.

A Session on that connection adopts a snapshot through Provider's existing startup binding. Adopting a temporary route on an existing Session requires exclusive admission and idle branches. Each branch must have published and dispatched its committed events, and have no running turn. First attachment checks the caller's configuration expectation before adoption. If first attachment or model update cannot retain the scoped binding, it restores the previously adopted model under the same admission. Fork preserves the route at its inherited cut. If the current connection disables that logical Provider, or its current override differs from the inherited route, the fork is refused before creating a child. Adopt the desired route before choosing a cut that inherits it. Scoped models record the logical catalog Provider in the optional non-secret `scopedRouteProvider` extension of the open request metadata map. The existing startup source carries the binding identity and non-secret destination. Credentials and header values live in Provider memory. The storage schema and format version stay the same.

Connection close releases both its route settings and its adopted route bindings. An exchange that already holds its resolved connection can settle. Model requests requiring an expired binding fail, and another connection cannot attach or fork a Session that needs the scoped source. After expiration, an explicit catalog model selection through `session.model.update` can replace the expired selection with its shared connection. A live scoped binding remains exclusive to its owner.

### 3.1 Read models

| Method | Request | Reply | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.management` | `{providerId, modelId?, page?: {after?, limit?, query?}}` | `{drivers: ManagementDriver[], providers: ManagementProvider[], errors: ProviderRecordError[], next: string|null}`; each connection adds `gluedPrefix`, the prefix its record states, and each model adds `alias`, the catalog id the listed id stands for, and `catalogName`, the name `provider.catalog` publishes for the row; each is omitted when unset | false | `Drain` |
| `provider.list` | `{}` | Existing `{providers, errors}` summary; each provider adds `remoteModelList`, whether `provider.refresh` can list its models, which clients use to offer or withhold that command in a row naming an empty catalog, and `gluedPrefix`, the prefix its record states, omitted when unset | false | `Drain` |
| `provider.selection` | `{cwd?}` | Existing `{selection}` and its separate scope/version. A `cwd` reads only that scope's row and answers `null` when the folder has none; the workspace-over-global precedence is a `configuration` rule, not this read's | false | `Drain` |
| `provider.model.set_enabled` | `{providerId, expectedVersion, modelId, enabled}` | `{version}` | true | `Drain` |
| `provider.catalog` | `{providerId, modelId?}` | Existing `{catalog}` with `ProviderModel`; each model's `name` is unique within its provider (§3.1 below) | false | `Drain` |

`provider.catalog` publishes a `name` unique within its provider. Duplicates are
found among all of that catalog's rows, a disabled or an unavailable one included,
so switching a row on or off renames no other and every client reads the same
names. Names compare after trimming. A row whose name is unique keeps it as
stated. A row that shares its name publishes its label candidate, `<its own name>
· <the origin its listing states for it>` — the text after the last ` · ` of the
label it carries, `routing group` for a group row among them — while no other row
reaches the same candidate and no row states that name itself. Every other row of
the group appends its id instead, so a label two rows carry apart names neither of
them; a row whose listing states no origin has no candidate and appends its id.
Every computed name is built from trimmed parts, the appended id included, so two
ids that differ only in space reach one name. An appended id whose name no other
row reaches and no row states is kept as it stands, and every name so kept —
label or id — is reserved before any counter is assigned, so a counter never takes
a name another row publishes itself. A name still taken after that gains a
counter, the rows left taking theirs in one canonical order: by their id name,
then their id, then their stated name. That counter is the only part a listing
order can decide, and with that order only rows sharing an id and a stated name
reach it, so reordering the same rows names each of them the same. The name is
computed when the reply is built and never stored: every descriptor and every
persisted field keeps the name the provider or the user stated, so saving an edit
never writes a suffix back and a suffix disappears with its duplicate. This holds
for every provider, a custom one whose two rows state one name included.

Management requires a Provider ID and returns that entry or a typed error.
An optional model ID restricts the reply to that model and its saved preferences;
an absent model is a typed not-found error. It is the sole edit-view read:
its record version, model descriptors and annotations come from the same Provider record
snapshot. Clients do not combine `provider.catalog` from another version with its
`expectedVersion`. Known-target capability reads pass the provider and model ID;
model pickers use `provider.models.query`. Aggregation does not promise an atomic
snapshot across account owners.

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
| `models` | `{model: ModelEntry, source: "default" \| "override" \| "user", enabled: boolean, endpoint: ModelProtocol \| null, unavailableReason: string \| null, missingLimits: ("contextWindow" \| "maxOutputTokens")[], actions: string[], endpointOverride?: NewApiEndpoint}[]`; endpoint overrides use the executable subset in §6. `null` only for an account entry with no local catalog record. Bad catalog data is an error. `missingLimits` names the limits no source declared (§3.5) and is empty otherwise. `catalogName` is the name `provider.catalog` publishes for that row, present only when it differs from `model.displayName`; an absent field means the display name is already the one to show. It is computed, so `model.displayName` stays the name the provider or the user stated and an edit never writes the computed name back. Model actions are `set_enabled`, `edit`, `reset`, `delete`, restricted by origin/authority, and `set_enabled` only on an entry without an `unavailableReason`, because the set keeps an unavailable entry's enablement |
| `preferences` | Map from model ID to the existing `ModelPreferences` shape |
| `modelsListed` | Absent from reads. On a connection update it says what happened to the model list: `listed`, `bundled`, `unsupported` or `disabled`. It is also `listed` when enabling fills an empty catalog under §3.2; other updates omit it |

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
IDs; after their deletion, keep a restart-required diagnostic until the next
activation permits adoption on access. Adoption and this read share one
Provider-owned candidate builder, including remote-model substitution, so they
cannot disagree about the candidate. The pending attempt belongs to source
adoption (§3.1 below); it does not disable the entry.
Actions describe current availability, not authorization or a promise that a later
write succeeds. Model origin is derived from the distinct persisted lists in §6.
An account's existing catalog retains its version and descriptors while signed out
or changed; its models are unavailable and model writes wait for a current refresh.
A disappeared default's override cannot become a user-added model.
`source:"override"` means a default model with a stored descriptor override;
an edited user model remains `source:"user"`.

### Startup, selection and memory

Provider reads are predicates over persisted state, not client-side catalog scans.
Explicit selection reads the named provider and model. Default selection tries the
workspace selection, then the global selection, returning immediately on the first
usable target. Only after both fail does it query enabled, configured candidates
in the existing account-first and provider-ID order. Account availability is read
from its owner; persisted credentials and flags do not assert a live login.
The execution predicate remains authoritative for routes, capabilities and limits.
Business selection has no page cursor. A model-only startup queries the exact model
ID, prefers the effective provider, and rejects more than one usable provider.
See [Model startup](../model-startup.md) for input precedence.

The shipped catalog is compiled into independently readable records and lookup
keys. Provider does not materialize the complete catalog at activation or retain parsed
catalogs, identification results or query results. Model identification reads only
matching descriptors and preserves namespace, alias, canonical-name and ambiguity
rules. Building the shipped lookup data happens before distribution.

Source activation publishes the available generation without expanding providers.
Per-provider adoption, retirement and pending conflicts follow
[Indexed source and sparse adoption](#indexed-source-and-sparse-adoption).

Ordinary connection and model edits preserve the admitted model base, including
historical rows without a known source generation. A matching definition
fingerprint alone does not turn existing historical rows into implicit defaults.
A base is established by the first configuration of an untouched builtin or by
the explicit source-adoption flow below.

The shared default HTTP client for remote model listings is created by the first
listing request that uses the default network settings, including one triggered
by a Provider update (§3.2). Local reads and selection do not create it.
Construction failure fails that operation with the existing Provider failure
instead of failing process assembly. Per-request network overrides still build
their own client with the requested proxy and timeout.

Display lists use bounded pages and server-side search. Opening a conversation
reads its adopted model; it does not enumerate candidate catalogs. Web and CLI
model pickers query when opened, discard their results when closed, and refresh
only the affected query after a write. Agent and modality selection consume the
Provider-owned predicates through the existing service faces, not a separately
maintained availability table in the client.

Provider retains no parsed catalog or completed query results while idle.
Retained memory should approach the cost of its I/O handles and lifecycle state.
Verification reports retained and peak allocated bytes
separately, plus shipped read-only catalog bytes; it must not substitute whole
process RSS or omit a retained catalog index. Display and update flows must also
report their peak and bound the amount of data admitted per request. Measurements
use disposable homes and include repeated activation and reads, so released
results cannot conceal an accumulating cache.

### Model display queries

`provider.models.query` is a read-only operation of the process `managementMethods`
face. It accepts `{providerId?, modelId?, query?, after?, limit?}`. The limit defaults
to 32 and accepts 1–128. Search matches model ID, display name and provider name
independently; `zdr` also matches the account-owned ZDR flag.
`after` and reply `next` are exclusive ordered `{providerId, modelId}` cursors;
they are traversal positions, not selectable targets. An empty model ID represents
an enabled provider without a model row. Clients pass the returned cursor unchanged.

The reply is `{models, empty, next}`. Each model row contains `providerId`,
`providerName`, `model` (the published `ProviderModel`), `version`, `preferences`
and `disabled`. Disabled and unavailable models remain visible for diagnostics.
`empty` entries contain `providerId`, `providerName` and a tagged `reason`:
`{kind:"no_models", listable}` or `{kind:"unreadable"}`. Each page admits at most
`limit` model or empty-provider candidates, so a page containing errors can have
fewer models. `next:null` ends traversal. A candidate deleted before its target read
is skipped; a later unreadable row is reported in `empty`. A stored managed catalog
without a matching signed-in account does not contribute model candidates. If its
account descriptor is absent, it contributes one unreadable provider entry instead.
Invalid limits fail with `provider_invalid_config`; cancellation and storage
errors follow the Provider operation contract. Reads perform no remote catalog
refresh. Refresh is an explicit management operation.

`provider.catalog` accepts an optional `modelId` for the adopted model's capability
read. It returns the same catalog envelope with only that model and preserves the
name computed against the provider's naming fields. Known-target consumers use
this form, including CLI session opening, model changes and adoption diagnostics.

### On-demand management reads

`provider.management.list` is the process `managementMethods` namespace's read-only
management overview. Its request is `{providerId?, after?, limit?, accountsOnly?, query?}`.
`limit` defaults to 32 and admits 1–128. `providerId` and `after` are mutually exclusive.
`after` is an exclusive Provider ID cursor. `query` matches the ID or display name,
case-insensitively. `accountsOnly` restricts enumeration to account authorities.
The reply is `{drivers, providers, errors, next}`. `next` is the last visited ID
when more entries remain, otherwise null. Failed records consume a page position
and appear in `errors`, so paging progresses past them. Pages are independent reads;
a write between pages can change subsequent results.

Each overview entry contains the identity, source, authority, version, enablement,
account, actions and safe connection fields defined above. It has no model or
preference fields. Model absence in a detail still means an account has no stored
catalog; it is never used to mean that a list omitted its models.

Provider reads only the requested record or page, projects each detail once and
releases the result after the call. A scoped read uses an indexed equality lookup.
Bundled reseed diagnostics inspect only the requested builtin definition. Reads do
not retain snapshots, seed-record maps or computed model results between calls.
The Provider version remains the CAS unit. A model detail reads its header, target
state and descriptor plus indexed naming dependencies in one local snapshot; it
does not expand the owning provider's other model payloads.

Web requests a page when its provider list opens, follows `next` on Load more and
queries again when search changes. Connection configuration uses that summary;
opening Models reads the full edit snapshot. Account discovery uses the account-only overview. CLI list commands consume
successive overview pages for the list the user requested; named account operations
read that Provider directly. Model-specific operations pass the model ID.

```text
Providers                     Provider detail
[Search providers]            [Connection] [Models]
  Provider A                  Connection -> summary
  Provider B                  Models -> full edit snapshot
                              model edit -> Provider A + model ID
[Load more]
```

The Provider owner requests shared, cancellable catalog work from the managed
account owner; its contract is in the [App Server managed-provider contract](../../../crates/dim-app-server/README.md#managed-provider-contract).
The [Dim OAuth owner](../../../crates/dim-oauth/docs/runtime-contract.md) defines
in-flight sharing and result lifetime. The local and wire consumers use the same
owner methods through `managementMethods`.

### Indexed source and sparse adoption

The bundled source is an immutable build-time index: provider summaries and
fingerprints of each serialized provider declaration, original model order,
sorted target keys, matching keys, effective naming inputs and individually readable descriptors. Offsets address data; opening
Provider does not decode model payloads, build a runtime catalog index or insert
builtin model rows. Mutable storage contains admitted user/source state. Full edit
snapshots remain output-proportional.

An available source generation is distinct from a provider's adopted base. An
untouched builtin reads the current source at version 1 without a mutable row.
First configuration admits its source identity and commits version 2 by CAS.
Targeted management, catalog, configuration and model-page reads reconcile a
changed source atomically when admitting that provider; successful
adoption advances its version once before applying a caller's expected version.
The legacy all-provider `provider.list` returns admitted summaries without
forcing adoption of every provider. An unchanged provider declaration does not
advance a version, including when only the catalog index representation changes.
When the catalog generation changes, source-dependent naming inputs of persisted
mutable rows are reconciled on provider access; this derived-only update leaves the version and
model payloads unchanged. Its work can scale with that provider's mutable input,
including only the base targets needed by those overrides or aliases; unrelated
immutable defaults stay indexed and are not expanded for this step.
A retired admitted builtin becomes disabled. Remote listings remain authoritative
when `remoteModels` is true.

A collision retains the complete admitted base, version and user state. Its pending
attempt records the activation that attempted it. Deleting the conflicting user
model does not adopt immediately: adoption may retry on access after the next
activation. No background sweep expands unused providers. Source generations needed
by admitted bases remain readable without the old executable and travel with native
backup/restore. Migration cannot infer user intent from today's bundle: an old base
that cannot be proved identical is pinned as authoritative admitted history.

Exact, scoped and unscoped reads locate keys before loading descriptors. Each page
uses one local read snapshot, bounded candidate batches and the same identification
and naming rule as a full read. Naming compares the effective descriptor, including
ID-name enrichment and overrides, using Unicode trim. An index locates the target's
collision dependency component, including reserved labels, ID fallbacks and counters;
no published name is stored. Adversarial components may include a whole provider.
Substring search can examine necessary index candidates beyond the returned limit;
it does not parse unrelated model payloads. Overview filtering reads summaries only.

### 3.2 Writes and field operations

`ManagementProvider` below means the same edit snapshot as §3.1. Ordinary CLI/Web
configuration reads `provider.management.list` and requests `reply:"summary"` on update.
The connection flow loads model details only when requested:

```text
Provider overview -> Connection -> Save (summary + committed version)
                         |
                         +-> Models -> Full editable model snapshot
```

This selection changes only the reply, not CAS, secret preservation, cancellation
or discovery. For enablement, credential and network-only updates that keep
inheriting an unchanged nonempty bundled catalog, the write itself reads and
writes no model payload. A full reply still constructs the requested complete
edit snapshot. Existing method
request/reply shapes remain except the additive `provider.update.reply` selection,
the explicit `ModelEdit.endpoint` addition and
typed errors in §3.4. Existing writes retain `mutating:true`, `interruption:Drain`.

| Method | Request | Reply / behavior | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.create`  | `{providerId, displayName, connection, models: ModelEntry[], enabled, gluedPrefix?}` | `ManagementProvider`; create-only, caller-generated fresh UUID, existing ID conflicts; `gluedPrefix` takes the patch's rule: trimmed, and an empty one states none | true | `Drain` |
| `provider.update`  | `{providerId, expectedVersion, patch, reply?: "full" \| "summary"}` | `ManagementProvider` by default; `reply:"summary"` returns the committed safe `ManagementInfo` plus optional `modelsListed`. Connection changes and enablement use the listing rules below | true | `Drain` |
| `provider.probe`  | `{providerId, connection}` | `{models: ModelEntry[]}`; §3.3, no record writes or version check | false | `Drain` |
| `provider.models.create`  | `{providerId, expectedVersion, model: ModelEntry, reply?: "full" \| "summary"}` | Existing catalog by default, `{version}` for `reply:"summary"`; separate user-added list; duplicate default or user ID rejected; add enabled and remove any stale `modelConfigOverrides`, `disabledModelIds`, `modelPreferences` and `endpointOverrides` entries for that ID in the same transaction | true | `Drain` |
| `provider.models.delete`  | `{providerId, expectedVersion, modelId, reply?: "full" \| "summary"}` | Existing catalog by default, `{version}` for `reply:"summary"`; only user-added items; remove that ID from `disabledModelIds` and clean owned preferences/routes/selections in the same transaction | true | `Drain` |
| `provider.preferences.update`  | `{providerId, expectedVersion, modelId, patch, reply?: "full" \| "summary"}` | `ManagementProvider` by default, `{version}` for `reply:"summary"`; effort/tier independently set/clear, validated against the model | true | `Drain` |

`provider.models.edit` and `provider.models.reset` take the same additive `reply`
selection and answer the existing catalog or `{version}`. A client that rereads
only the edited row requests the summary; the committed version is the CAS token
for its next write. Absent `reply` keeps the full reply for existing callers.

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

An update that changes `baseUrl`, `credential` or `headers` changes the endpoint
or account the record reaches. If the Provider supports remote listing and the
update leaves it enabled, the owner lists the endpoint's models unless the
record inherits a nonempty bundled catalog. That catalog is kept, and later
bundled updates still reach it. A connection change that leaves the record
disabled makes no request and drops its own endpoint models, if any. Enabling a
listable built-in Provider defined with an empty bundle (as with Magpie) fills
its inherited catalog or an empty saved endpoint listing, including one cleared
by a disabled connection change. A nonempty saved listing is kept on enablement
without a request. User-added models do not count as endpoint models. An
enable-only update does not fill a cleared endpoint listing for a custom Provider
or a built-in defined with a nonempty bundle; use explicit refresh. A `network`-only
change saves alone: it changes how the endpoint is reached, not what it serves.
A driver that cannot list saves the connection alone.

An invalid patch is rejected before discovery, as `provider_invalid_config`
with its reason. The request runs outside the write and keeps `refresh`'s
user-model conflict rule. On success, the owner stores the connection and the
listing in the same compare-and-set write and sets `remoteModels` as `refresh`
does. A failed listing leaves the record unchanged. The Models area's `Ctrl+R`
and `dimcode models PROVIDER --refresh` remain available for an explicit
refresh, including for a record that was already enabled with no models.

| `patch` field | Operation | Writable records |
| --- | --- | --- |
| `displayName` | Nonempty string | Custom only |
| `baseUrl` | Validated URL string | Static-authority builtin/custom |
| `enabled` | Boolean | Local builtin/custom records |
| `credential` | Existing keep/clear/set/set_aws/set_ref; `keep` changes nothing, exactly as an omitted credential does, so a keep-only patch is the empty patch | Static authority only |
| `headers` | `{name, action:"set", value}` / `{name, action:"remove"}` array; omitted names keep | Static-authority builtin/custom |
| `network.proxy` | `{action:"set", value:URL}` / `{action:"clear"}` | Local builtin/custom; existing authority validation applies |
| `network.timeoutMs` | `{action:"set", value:positive integer}` / `{action:"clear"}` | Local builtin/custom |
| `gluedPrefix` | a string sets the prefix this Provider's listed ids carry; `null` clears it | Static-authority builtin/custom |

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
`models.edit` also adds `alias: {action:"set",value:string} | {action:"clear"}`,
which is stored on the record's alias map rather than as a descriptor override: an
alias is the Provider's statement about its ids, and an edit that only names one
writes no override for the reset path to keep.
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
For a static Provider with a saved credential or anonymous authentication,
explicit refresh is available whenever its driver and definition support remote
listing. A disabled Provider stays disabled after refresh, and its models remain
unavailable for execution.
Descriptor edits to a user model replace that model's `userModels` entry; they
never write the user ID into `modelConfigOverrides`. `models.set_enabled` accepts IDs
from both defaults and user models and names the complete set the account can use:
an ID that names no entry is rejected, and an entry that is unavailable keeps the
enablement it already has. These operations update the effective catalog
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
the exact `{providerId, modelId}` with preferences for that pair to
`session.create` or `session.model.update`. An unavailable explicit target fails;
report it and do not retry without the target or fall back to another model.
The server applies the model's saved preferences when the request states none:
for each field the client leaves unstated, `session.create`,
`session.model.preview` and a targeted `session.model.update` take the value saved
for that `{providerId, modelId}`, then the model's declared default. A stated
field wins. A saved value the model no longer accepts is dropped for that field,
the declared default stands, and the server logs one warning; a stated value it
rejects still fails the adoption. Untargeted F3 updates still change only
supplied fields on the current adopted model. Which catalog declarations are
applied, and why the others only describe the model, is
[the model-configuration rule](../model-configuration.md#model-parameters).
`/fast` remains
Session-only; a saved `serviceTier` is a persisted fast/tier choice the server
applies like a saved effort. `sessions.launch` — how Scheduler, Workflow and
other plugins start work — is not a client Session: it keeps catalog defaults and
does not inherit chat preferences, and neither does any other consumer-specific
model.

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
`provider_failed`, `provider_catalog_stale`, `provider_account_changed`.
Classify typed owner errors, not their message text. This also
maps not-found, CAS/admitted-refresh conflict and validation errors from retained
`delete`, `models.edit`, `models.reset`, `models.set_enabled`, `select` and
`refresh`; their requests/replies and JSON-RPC code do not change. Unknown methods
and malformed RPC parameters retain their existing generic boundary errors.
Never include tokens, secret references, headers, proxy auth or raw upstream bodies.

Managed `models.set_enabled`, `models.edit`, `models.reset` and
`preferences.update` check the record version first. A mismatch remains
`provider_conflict`, even if the account also changed; reread before deciding
whether to submit another write. A matching version with a signed-out account or
a catalog from another account epoch/generation returns `provider_catalog_stale`
without writing. A normal credential refresh that only advances the credential
revision does not invalidate the catalog.

`provider_catalog_stale` adds these fields beside `data.appError`:

| Field | Type and meaning |
| --- | --- |
| `providerId` | Managed Provider ID |
| `expectedVersion`, `currentVersion` | Requested and stored record versions at the rejected write; equal because the version check takes precedence |
| `issuer`, `subject` | Current public account identity, or both `null` when signed out; no profile, scopes, credential epoch or generation |
| `requiredAction` | `provider.refresh` when signed in, including a changed identity; `account.login` when signed out |

For `provider.refresh`, clients refresh that Provider and may retry the intended
write once using the returned version. For `account.login`, clients complete the
account's advertised login flow and refresh before a new write. Clients do not
parse messages, compare account generations or refresh on `provider_conflict`.
An account change reported by the account owner or detected during an in-flight
refresh returns `provider_account_changed`; reread account status before deciding
the next action. It does not represent a record version conflict.

New methods, endpoint editing and typed owner errors ship as the discoverable
`provider.management` interface unit. `provider.save` is replaced by create/update
and removed with its CLI callers; there is no fallback save/direct-database path.
Rpc retains `service_reloading`/`service_unavailable` and ordinary admission.
All accepted calls remain owner-tracked and drain on unload. Disconnect only loses
the reply; it never rolls back committed writes. Reread uncertain writes rather
than replaying them. Provider updates do not reload the loader row.

The native service and RPC share the same create, partial update and model operations.
Offline native migration uses Provider's `read_stored_static` / `write_stored_static`
boundary in its caller-owned SQLite transaction. That boundary assembles or splits
the native state and admitted source; migration does not interpret sparse rows.
These are storage-owner functions, not additional runtime services.
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

The bundled `openai-codex` chat rows declare a default `contextWindow` and a
read-only `metadata.raw.max_context_window` for user configuration. These are the
nominal token counts from the
[Codex catalog](https://github.com/openai/codex/blob/8653ca9e99b2c805107bde7b7d6f57a634c643d0/codex-rs/models-manager/models.json),
its [older catalog](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/models-manager/models.json)
for GPT-5.4 and GPT-5.4 Mini, and the
[published Spark window](https://openai.com/index/introducing-gpt-5-3-codex-spark/).
Dim uses its own input and compaction budgets. For example, GPT-6.2 Sol,
GPT-6.1 Sol and GPT-6.1 Terra default to 500,000 tokens and allow up to 872,000.
Edit `contextWindow` with `dimcode models openai-codex edit gpt-6.2-sol` or
`provider.models.edit`. Reset restores the bundled default. Source adoption
preserves explicit overrides and caps their effective value at the model's
current ceiling. New edits above the ceiling return `provider_invalid_config`.
The [Session startup override](../model-startup.md) is subject to the same
ceiling when setting a new value.

A listing declares a model's reasoning efforts the same way, and the first shape
it states is the model's own list: the `values` of its `reasoning_options` entry
of type `effort`, else the `effort` of each `supported_reasoning_levels` item,
else the flat `reasoning_levels` list. `reasoning_options` is read where it
already was, at the item's top level or beside the `raw` a stored entry keeps;
the other two are read the same way. Each value is trimmed, a non-string or an
empty one is dropped and the first of a repeated value is kept, so the order is
the listing's. A stated shape whose values all drop declares an empty list: the
model offers no effort choice, the identified row does not fill it, and the
stored descriptor keeps `effortOptions: []` so a later read still declares it. A
listing that states no shape and a model whose `reasoning` is `false` both keep
what they had, so an identified row still fills an absent list and never a
declared one. `reasoning` itself is unchanged: a listing that states no
`reasoning` still declares `supported: true` from its levels alone, as it does
from `reasoning_options` today.

The model is identified first, and the numbers are read from the entry that
identification named. Identification runs over the catalog, and the first step
that leaves one model wins: the provider's declarations by exact id; then the
whole qualified id, when the namespace it carries names the provider whose row
carries that id, so an aggregator's own house model is never answered by the
unrelated row its final token happens to name; then a vendor entry whose own
tokens are the head of the id, its tail no more than a date, `latest`, a tag
suffix or a bracketed context modifier; then the vendor's upstream canonical id
and display name read together, where the display name narrows several
canonical hits and a remaining disagreement is an ambiguity rather than a
guess; then the `custom-provider` rows by those same two rules; then the id a
provider says it stands for, its alias; and last, for a model that no vendor
entry, no `custom-provider` row and no alias names, a merge of the aggregator
rows whose own id names the same model with the same tag, taken in the safe
direction: the smallest window, the smallest output cap and the intersection of
the effort choices. A provider default is not a declaration, so a row carrying
the pair its provider writes across many of its rows declares no limit there.

A declaration at that first step is the provider's own statement about an id,
never something its listing produced. A builtin provider's declarations are its
bundled rows and the models the user added to it; a custom provider declares
nothing there. A row a listing wrote is that listing's output, so it answers for
neither itself nor a sibling id: a listed id is identified against the catalog
on every discovery and every read, not against the row an earlier listing left.
What the user stated keeps applying on either kind of provider, through
`capabilityOverrideFields` and the other user-owned state.

A vendor entry is a row of a provider whose role is `vendor`, when the row's
canonical id names that provider's own namespace or the row states no canonical
id. A row of a vendor provider whose canonical id names another vendor is a
resale: the steps that read vendor entries pass it over, and the merge reads it
as it reads an aggregator row. The first step still reads it for its own
provider, since a bundled row is that provider's declaration whichever model it
resells. A provider's own namespace is `metadata.vendorNamespace`, which the
sync writes from the provider's policy beside its role, and every vendor
provider whose rows carry canonical ids states one. A plan row that resells
another vendor's model, where upstream gives it no canonical id, has that id
stated in the committed catalog, and the sync keeps it.

A row joins that merge only when its kind is the id's kind. A row that states
no kind, because its raw `type` is absent or `model`, joins any id. An id that
states `chat` takes the rows stating `chat`, and an id stating any other kind
takes the rows stating exactly that kind, with no synonym. An id that states no
kind takes the rows stating `chat` or none when any row of its name states
`chat`, and otherwise every row of its name: it keeps the media rows it merged
before and carries their `type`, which is what keeps a media model out of
conversations. An id left with no joining row stays unidentified.

A provider's record states a prefix its listed ids carry, and that prefix is
stripped once, at the head of the id and case-insensitively, before the id is
broken into the tokens every token and name step compares. The first step and
the qualified step read the whole id, so an exact row and a qualified id are
untouched by the statement, and the qualified step answers before the prefix
is stripped: it is the whole id it reads, so a prefix that would leave it no
tokens cannot take it away. A name step sees the stripped id, because the
relay prefixed the id rather than the model. The prefix is a token head, so
`auto` and `auto-` are one statement, and a prefix that consumes the whole id
leaves no name, which identifies nothing rather than matching an empty one.
Discovery writes the ids the provider reports and reads the bundle without the
prefix: a prefix is the user's statement about the ids they see, not something
a listing may infer. `provider.create` states the prefix on the same terms as
`provider.update`: trimmed, and an empty one is the same statement as none.

A provider's record also maps a listed id to the id of the model it stands for.
That alias is the escape hatch for an id the rules cannot read, so it is
consulted after every rule and before the merge: it never overrides an id the
rules can read. Its target goes through the same steps with the alias step
excluded, which is why an alias answers with the target row's rule, score and
numbers rather than a word of its own, and why an alias never chains to
another one. A target that is itself ambiguous stays ambiguous, with the
target's own candidates, because an alias names one model and may not choose a
side of a disagreement; a target that names nothing leaves the id with the
refusal or the merge it had without the alias, so a stale statement never makes
an id worse than it is. The alias is keyed by the exact id the provider spells,
case-sensitively, because that spelling is the provider's own, while the target
goes through the steps where case folding already lives.

The namespace of a qualified id is what makes it an identity, not a hint. When
it names a bundled provider, a token, canonical-id or display-name step that
finds only rows of other providers answers nothing and refuses the id with
those rows as the candidates: a final token alone may not hand a qualified id
to a vendor the id did not name. An id whose namespace names no bundled
provider is a plain name for a model several sellers list, and those steps read
it as one.

The projection records the outcome in `metadata.identification`: `rule` is one
of `provider-exact`, `qualified`, `token`, `canonical`, `custom-provider`,
`merge` or `ambiguous`; `narrowedByDisplayName` says the display name settled
several canonical hits; `candidates` names the rows an ambiguity refused; and
`filled` names the entry, rule and value of each limit the row did not declare.
A merge adopts one minimum per limit from one joining row each, so its `filled`
names the row that declared each adopted value, with that merge's own `rule`. A
limit the row declares itself has no `filled` record, and `provider.models.edit`
drops the record of each limit it declares. A refusal names the provider and
the model id, the limits that are missing, the candidates that still disagree,
and `provider.models.edit` as the write that declares them.

`metadataSource` and `matchedProviderId` name the row the projected entry is
copied from, which for a merge is the first joining row: the entry's display
name, pricing, capabilities and any limit that row declares come from it. They
do not name the supplier of a merged limit; `filled` does, one limit at a time.

The identification is a fact beside the entry's availability, not part of its
reason. No reason value names it: a model that identified, one several rows
disagree on and one that identified nothing all keep `limits_undeclared` while
they declare no limit, and the outcome is what says which of the three they
are. A client that lists a model reads it from the reply it already fetches.
The terminal's model list adds the outcome after the row's effort, and the
limits row that names the write for a provider's undeclared models names the
candidates too. The web client's reader keeps `identification` on the entry and
its model list renders the outcome as a suffix, after the reason or the limits
the row already states, so nothing the row said is replaced. Both say which
model an id was taken to be, that the numbers came from several rows, the rows
that still disagree, or that nothing named the id. A client that does not read
the field keeps working and shows nothing new.

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

The two declared limits are facts, not a request allowance. Provider derives the
adopted output allowance as `max(1, min(declared maxOutputTokens, 131,072,
contextWindow / 2))` (`dim-agent-capabilities::context_model::output_allowance`),
so a declaration at or above the window is accepted and still leaves at least half
the window as input budget. Every adoption writes that value to the configuration's
`maxTokens`: the catalog read, static and managed selections, startup overrides and
the model-file profile; the auxiliary target reads the same value.
`capabilities.maxOutputTokens` keeps the declared fact. A stored
`context/configured` configuration whose `maxTokens` is absent or above the rule is
repaired when a request is built from it; the stored fact is not rewritten, so a
Session adopted before this rule resumes and compacts normally.

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

The managed account catalog follows the same rule, and identifies its models the
same way any provider does: the record's own prefix and aliases are the
projection identification reads, so an account model the catalog cannot name
gains the limits of the row its alias states, and an alias-only edit is not a
descriptor override. An account model's entry carries what any provider's entry
carries: `metadata.identification` with the rule that named its row, the
rows a refusal names, or nothing when no step named one; the matched
provider, model, strategy and score; and `filled` naming the row that supplied
each limit the entry does not state. The record is derived on every read, so a
cleared alias or a refreshed listing recomputes it, and a statement that names
nothing leaves neither a rule nor a stale row behind. What the account's listing
and any override state itself stays theirs: a limit marked in
`capabilityOverrideFields` is never filled or listed, and modalities, reasoning,
pricing and `metadataSource` keep the account's own descriptor. The relay's
listing fills what it can on its own, before the record exists, so it projects a
model with what the bundle alone says. A
model that neither the relay nor the bundled catalog declares a limit for is
projected with that limit absent and keeps its `unavailableReason` by the
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

#### Route-specific HTTP body limits

For requests carrying an image, Provider supplies the [48 MiB HTTP body
limit](https://api-docs.deepseek.com/guides/vision/#limits) on a static connection
for `deepseek-flash` via OpenAI-compatible Chat Completions at
`https://api.deepseek.com`, without a custom `Host` header. Provider uses the
resolved route, not the catalog's `vision` flag or a vendor name in a model ID.
Text and image references do not trigger this rule. The limit travels only
with the resolved connection and is not a catalog, settings or storage field.
Aimux applies it at its [HTTP send boundary](../../../crates/dim-agent-aimux/README.md).

### 3.6 Selectable models for process owners

The [target process binding](../contracts/process-owner-operations.md) moves `modelSelectionHost.resolve`, both `selectableModelsHost` reads and `auxiliaryModel.complete` to the data face `providerService` on the configured profile. Consumers switch their injection and retain the operation arguments, results and credential ownership. The separate binding names below describe the pre-migration catalog.

A process owner that routes work by model capability cannot keep its own copy of
the availability rules of the models it routes to. The port below has one provider
per profile — Provider in the product profile, the configured model profile
(`dim-agent-aimux`) from its model file — and each provider owns
side-effect-free reads of the models it would serve, with the identifiers and
declared capabilities a routing decision needs. This section defines the port and
Provider's rules; the configured profile's rule is that page's. The read is a
process host port, not a management method: it is not on the `provider` namespace,
is not served over RPC, and does not change `provider.catalog`.

**Why a port rather than a method on `ProviderService`.** A consumer plugin lives
in its own crate, and `xtask check` rejects a dependency on `dim-app-server`, the
composition layer, so the consumer cannot name `ProviderService`. The port and its
data type live in `dim-agent-capabilities`, which Provider and the consumer both
depend on; registering it as a process name lets a consumer name the port and
the catalog resolve it, and the dependency points from the extension to the core,
as [ADR 002 D3](../../adr/002-plugin-scope.md) requires. Each provider keeps its
implementation, its catalog and its rules.

**Contract (language-neutral).** A process host port, no instance key, provided
by Provider in the product profile and by the configured model profile from its
model file: operation `selectableModels`, asynchronous, request `{}`, reply
`{models: ModelCapabilitySnapshot[]}`. The element is the existing
`ModelCapabilitySnapshot` — `{target: {providerId, modelId}, capabilities:
ModelCapabilities, reasoning?, serviceTier?}` — not a new model type: it already
carries the model and provider identifiers and the declared capabilities, and it
is the descriptor execution adopts. A selectable model always declares both
limits (§3.5), so the existing `capability_snapshot` conversion always succeeds.
Abandoning the call cancels its owner read; dropping the reply is not a successful
read. Errors use the public codes stated below.

**Second operation: `selectableForModality`.** The same port carries one more
process read, for non-conversational uses (Modality §9.2): operation
`selectableForModality`, asynchronous, request `{}`, reply
`{models: ModalityModelSnapshot[]}` with each element
`{target: {providerId, modelId}, operations: string[]}`. `operations` holds the
`<category>.<operation>` ids the model's catalog entry declares, in that entry's
order; a model that declares none is omitted. An entry declares its operations
in `metadata.raw.modalityCapabilities`, and an explicit list, an empty one
included, is the whole answer. An entry with no list is recognized as an image
model, because relay and account catalogs list such models without media
metadata: its id is `arcship-image`, a known ID in the `gpt-image-2` or
`grok-imagine-image` family, or names `seedream`, or its `type` is
`image`/`image-generation` with image or no stated output, or `imageGeneration`
is true, or its only output is `image`. Such a model declares
`image.generate`, and `image.edit` too when it also reads images or is a known
model; a `type` of `image-edit` declares `image.edit` alone, and
`imageGeneration: false` recognizes nothing. The connection rule below reads
the same declaration. The read applies the same provider, account, model,
route, credential and status rules as `selectableModels`, without its
text-modality, chat-kind and declared-text-limit requirements: a model without
a `text` modality, a non-chat model and a model with `limits_undeclared`
remain candidates when they declare a modality operation, while an unavailable
status, a disabled Provider or model, a missing route, a signed-out account and
a missing static credential still exclude them. It carries no reason field: the
consumer derives availability from the options and explains a persisted
default that is no longer selectable from its own snapshot.

**Rules Provider applies.** Candidates are persisted model targets queried by
provider enablement, credential configuration and model status. Each candidate
is read through the exact-target catalog read, with account availability checked
by its owner. Bundled substitution, descriptor overrides, user models, routes
and disables are projected for that target using the execution rules. A model is
included only when the existing
`ProviderCatalog::selectable` accepts it: `unavailableReason` is absent and its ID
is not in `disabledModelIds`. That reason already covers every exclusion: a model
with `ModelStatus::Unavailable`, a model that declares no `text` modality, a
non-chat model on a driver other than `new-api-router`, a disabled Provider
record, a model with no route, a signed-out account, a missing static credential,
and an undeclared limit (`limits_undeclared`). The read adds no second predicate:
it reads the one selection already applies. The modality read is that same rule
minus the three requirements above, and both catalog builders derive an entry's
conversational and modality reasons from one classification of the entry, so a
new exclusion reason states its class once and the two reads cannot drift apart.
A model appears only if `select`, `configuration`, `apply_preferences`, the
per-request check and `auxiliaryModel` accept it at the same read. Execution
remains authoritative: a later write can make a model unselectable, and the
consumer passes the ID back to Provider, which re-checks.

**Rules the configured profile applies.** Its candidates are every provider and
model entry of the model file, ordered by provider and then model id. The file
requires both limits, so each entry is returned with them; it declares no other
capability, so entries carry the text defaults. That profile has no account,
route, disable or preference input, and the read adds no such test: the file's
validation and its rule are its own page's
([aimux](../../../crates/dim-agent-aimux/README.md)).

This read is not [#560](https://github.com/arcships/dim/issues/560)'s validation
contract. That contract (§3.7) takes an admitted persisted static record and an explicit
model ID and answers whether that selection can execute; this read takes no input and
enumerates what is available now. Both are defined on the same catalog and selectability
predicate, so they cannot disagree about a model.

**Snapshot and consistency.** One call enumerates persisted target IDs, then
reads each target separately. Each target record read has its own committed
snapshot. A concurrent `provider.*` write can be visible for some targets and not
others, including targets of the same provider; a deleted target is omitted.
The reply has no aggregate version or retained cache. The next call reads the
current state. It makes no live request: it never refreshes or lists a provider
endpoint, and it writes no record, selection or preference. It uses the same
account-status read as `provider.catalog`. The reply is derived; a consumer must
not persist it or treat it as an adopted snapshot. Adoption stays with model
selection, which resolves a configuration once.

**Errors.** The public face and its Rust binding return `cancelled` when the caller
cancels, `owner_closed` while Provider unloads, and `failed` when Provider storage
cannot be read. A provider whose own record or account status cannot be
read is omitted, as default resolution omits it; it never hides the others and the
read returns no per-provider error list. A caller that needs a provider's
diagnostic uses `provider.list` or `provider.management`.

**Consumers.** Modality computes each operation's options and its automatic
default from these reads. For a category an adapter serves, the candidates are
both reads deduplicated, the execution-selectable models first and the modality
read's after, each in its read's order, and an operation keeps those a collected
adapter serves for it; a media model execution rejects therefore stays an option
where an adapter serves it, and a selectable model that declares no operation
stays one too. An operation no adapter serves takes the modality read's models
whose entry declares that operation id, and `image.understand` takes the
selectable models with `vision`. It reports "no adapter for the configured
model" when the configured model is absent. It does not call `provider.catalog`
and keeps no availability predicate of its own. The reply carries IDs and
capabilities only, not a display name: a client that renders an option joins the
ID with the model list of `provider.catalog` or `provider.management`, and
Modality copies no name.
Provider gains no per-consumer key, setting or filter: the read has no input, so a
caller that wants one provider filters the reply. Collaboration's `agent` tool
reads the same reply for its `models` action and hands the IDs to the leader's
model, and keeps no rule of its own either
([Collaboration](../plugins/Collaboration.md#7-the-models-a-member-can-run)).

**What does not change.** `provider.catalog`, `provider.management`,
`provider.select`, model selection, adoption, preferences and their errors; the
client protocol and `PROTOCOL_VERSION`; persistence and its format; Timeline,
Dispatcher, callbacks, Session and branch parts; and the driver, route and
non-chat rules. The read adds no state, cache, notification, crate or module.

**Rust binding.** The binding calls the typed operation directly, without JSON
or a bridge task. It takes a `CancellationToken`; cancelling it returns
`ModelPortError::Cancelled`. Each call owns a child token that is cancelled when
the call completes or is abandoned, without cancelling the caller's token. The
operation maps the owner's `HostError` into the public `ModelPortError` codes;
the dispatcher invokes that same operation and encodes its reply or error.

The port is `SelectableModelsHost` in
`crates/dim-agent-capabilities/src/host.rs` beside `ModelSelectionHost`. Its two
reads are the operations §3.6 states: the port type holds the owner's closures,
its dispatcher writes the wire shape, and a Rust reader calls the same
operations through the binding. Provider provides it with
`provide_selectable_models` in `ProviderPlugin::apply`, and the configured model
profile (`dim-agent-aimux`) provides it from the entries of its model file, so
both deployments answer the same reads; that profile declares no modality
operation, so its `selectableForModality` read answers none
([aimux](../../../crates/dim-agent-aimux/README.md)). The catalog registers the
name shared ([service faces §4.10](../service-faces.md#410-宿主读取端口与-provider-选择)).

The owner implementations are `ProviderService::selectable_models` and
`ProviderService::selectable_modality_models`; `ProductResolver` delegates to
both. A consumer that needs the read declares
`Keyed<SelectableModelsHost>` in its `Deps` and gates on it (Modality). A consumer
that must load without Provider reads the name when it is provided and gates
nothing (Collaboration's `models` action); it takes a snapshot at load, so a
Provider reload does not reload it. No consumer reads `ProviderService` directly.
A non-Rust consumer calls `selectableModelsHost` by name, with the operations §3.6
states; the face delegates to the same owner read, so a consumer of either language
gets the models execution would.

**Connection contract.** Provider resolves one non-conversational connection for a trusted
in-process adapter. This is a native port beside `ProviderConnectionResolver`, not a
management method or a named data face. It accepts a model identity and operation and
returns one connection with its protocol, model and realtime declaration. The resolved
connection includes credential material and remains in process for that adapter call.

Both native connection ports use the same owner resolution: static or managed record
read, catalog selection and credential lease. One modality call resolves once; its route
facts and credential come from that resolution. The chat path's temporary startup
connection and adopted capability snapshot do not apply. The owner does not add an HTTP
executor to this port; media protocol and transport behavior remain with the existing
adapters. A cross-language operation contract is tracked in [#1613](https://github.com/arcships/dim/issues/1613).

**Rules.** It applies exactly the rules of the `selectableForModality` read —
provider, account, model, route, credential and status — and additionally
requires the model's catalog entry to declare `operation`, as the read above
derives it. A model that fails any of these is refused
before any network call: the availability cases with the reason execution reports
for that model, and an undeclared operation as an invalid request that names it.
The read and the connection cannot disagree, because they share the catalog and
its selection predicate. The managed account's credential lease reads the same
rule: it refuses only a reason that excludes every use, so a media model the
catalog marks as non-chat still leases for this call. That boundary rule reads
the stored reason together with the entry's status: an `unavailable` status
excludes every use whatever reason was stored, so a record an older projection
wrote for such a model is listed by neither read and leases nothing.

**Realtime declaration.** An entry that serves a realtime operation declares the
transport it speaks in `metadata.raw.realtime: {protocol, path}`. Provider parses
it into `RealtimeDeclaration` and carries it on the connection reply as
`realtime`; an entry that declares none replies `null`, and the configured model
profile replies `null` for every target, because a model file has no catalog
entry. The operation is still declared in `metadata.raw.modalityCapabilities`
with every other one, so a model that lists `realtime.voice` without a transport
resolves a connection whose `realtime` is `null`: Modality refuses that model's
`open` before any network call instead of guessing a URL. `protocol` is the
adapter's name for the wire protocol it accepts, and `path` is appended to the
route's base URL by that adapter.

**Credential rule and lifetime.** The connection lives for that one call. Provider
never persists it, never returns it to a client and never writes it to a task
record, result, diagnostic or error. Its `Debug` prints the model, the route, the
base URL, header names and whether a credential is present, and never a header
value, bearer or key; `ModelConnection` implements no `Debug`, so no other
connection formats one. A managed connection retains its recovery handle for
trusted in-process use,
but the in-tree media adapters do not invoke managed credential recovery. A media rejection is returned to the caller; automatic
refresh and retry are not provided by this port. The chat recovery path is
separate and does not establish a retry guarantee for non-conversational calls.

**Rust binding.** `ModalityConnectionHost` holds `Arc<dyn ResolveModalityConnection>`.
`resolve_for_modality(target, cancel)` returns `ModalityConnection { resolved, protocol,
model, realtime }`; `resolved` is `ResolvedModelConnection`, including its recovery
handle where applicable. `ProviderPlugin::apply` provides the typed port. The configured
model profile provides the same trait and refuses every operation because its entries
have no modality declaration. The catalog name has no dispatcher. A caller passes its
`CancellationToken` through the existing native resolution.

### 3.7 Offline validation of a stored selection

A consumer that reads Provider's persisted native records must decide whether an explicit
provider/model selection can execute without starting Provider or making a live request.
Migration's provider step is the first such consumer. Provider owns that judgment for an
admitted static record, and it exposes it as one side-effect-free projection rather than
letting the consumer rebuild routes, descriptors or availability rules. The contract
receives the native record; it imports no legacy row, migration receipt or task state, and
it reads no store, account status or network.

**Contract.** A synchronous Provider module function with no I/O and no cancellation:

| Part | Value |
| --- | --- |
| Input | `providerId`; the admitted `StaticProviderRecord` returned by `parse_static` or `convert_legacy_provider`; the explicit `modelId`; and the bundled catalog the caller loaded |
| Reply | `{protocol: ModelProtocol, snapshot: ModelCapabilitySnapshot}` |
| Errors | `account_owned` when the record's credential authority is not `static`; `not_found` when the effective catalog has no such model; `unavailable {reason: string \| null, disabled: boolean, missingLimits: string[]}` when the existing `ProviderCatalog::selectable` refuses a present model |

The reply is the effective descriptor execution adopts: `snapshot` is the existing
`ModelCapabilitySnapshot`, whose `target` carries the provider and model identity, and
`protocol` the effective route. A consumer that needs only a yes/no answer ignores the
fields. The contract does not apply saved `ModelPreferences`, choose a default model or
create the route stamp; those belong to adoption, which still resolves through
`configuration`. The record's precondition is the admitted type's: its
routes were derived by Provider's parser under the current driver and New API rules, so
this function does not re-parse the record, re-validate its fields or re-derive its routes.
It adds no second admission path.

**Rules it applies.** It builds the same per-provider catalog as `provider.catalog` and
`catalog_for` (`static_catalog`) and accepts a model only through the existing
`ProviderCatalog::selectable`. The effective descriptor — bundled substitution, descriptor
overrides, user models and declared limits — is the same one that catalog projects, and
the existing reason covers every exclusion: `ModelStatus::Unavailable`, no `text`
modality, a non-chat model on a driver other than `new-api-router`, a disabled Provider
record, no route (unsupported, missing or ambiguous New API endpoint), an unsupported
driver, a missing static credential and an undeclared limit. Disabled model IDs are the
catalog's `disabledModelIds`, not another list. The route is the catalog entry's
`endpoint`, derived by `drivers::routes`/`new_api::routes` at admission; no second route
matcher is added. `capability_snapshot` produces the declared descriptor, so the
projection and execution cannot disagree about limits, reasoning or service tier. The
refusal detail is read from that same catalog entry after `selectable` refuses; it does not
decide acceptance.

The contract rejects an `account_owned` record before building its catalog. A managed
record's current catalog, credentials and routes belong to its account owner and are not
contained in the persisted static record, so executing it requires that owner's read, not
this offline judgment. Static records are the contract's input; the account owner's
catalog remains authoritative for managed models.

**Relation to §3.6.** The selectable-model read is a process host port with no input. It
enumerates what Provider would serve now, including managed account catalogs, and returns
capability snapshots for routing. This contract takes one persisted static record and one
explicit model, reads no account or store, and returns the route with the snapshot. They
share `static_catalog`, `ProviderCatalog::selectable` and `capability_snapshot`; for a
stored static-authority record both can see, a target appears in the §3.6 reply exactly
when this contract accepts it. The two differ in scope, not in rules: §3.6 answers "what
is available now", this contract answers "can this stored selection execute". Neither is
a management method or an RPC.

**Consumer and migration.** Migration reads the target record with Provider's parser and
converts legacy selections through Provider's frozen selection conversion. That conversion
is the only offline judgment and currently uses Provider's private catalog predicate; this
contract is its public form, and the conversion calls it. Migration keeps its legacy
decoding, conversion, writes and receipts, and owns no route matcher, availability
predicate or catalog construction. The contract therefore serves any native record consumer
without a migration-specific owner API, callback or RPC.

**Compatibility.** The contract is a Rust module boundary over an already-admitted type,
not a rutis service, RPC or management method. It adds no service name, generated graph
entry, crate dependency, persistence or `PROTOCOL_VERSION` change. Legacy import outcomes
and rejection text are unchanged because the selection conversion maps any refusal to its
existing reason, and an accepted selection writes the same provider and model IDs. The
reply carries no credential, header, proxy or secret reference; the record input is the
same one Provider's parser already holds.

**Rust binding.** The only binding is the Provider module function, exported from
`dim_app_server::provider`. There is no cross-language consumer: the input is the admitted
record type, which carries the credential reference, so it is not exposed as a named
service.

```rust
pub struct StoredSelection {
    pub protocol: ModelProtocol,
    pub snapshot: ModelCapabilitySnapshot,
}

pub enum StoredSelectionError {
    AccountOwned,
    NotFound,
    Unavailable {
        reason: Option<String>,
        disabled: bool,
        missing_limits: Vec<String>,
    },
}

pub fn stored_selection(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
    model_id: &str,
) -> Result<StoredSelection, StoredSelectionError>;
```

### 3.8 Media execution for adapters

Provider resolves a model's connection for one non-conversational call, and Modality passes it to
its adapter for that call ([Modality A.2](../plugins/Modality.md#a2-adapters-and-ocr)). That
connection is not a public contract and is not exposed as a data face.

**Why the connection cannot be a face.** Its reply carries credential material for one call, and
`ResolvedModelConnection` is resolved for one final request and never serialized
(`dim-agent-capabilities` `host.rs`).
[Service faces §2.2](../service-faces.md#22-migration-marker) keeps secret values with the owner
and exposes operations that use them, so the only shape that could cross is an operation — the
request executed on the adapter's behalf — not the connection.

**Decision: no such operation now.** The connection is Provider's private typed binding, not a
public service: what it returns is credential material for one call, so it neither becomes a data
face nor stays a catalog name. [Service faces §2.6](../service-faces.md#26-migration-rule) already
says a name whose role is a typed binding leaves the catalog. Media adapters execute in-process
(`dim-modality-aimux`) through this native path. No media operation, signing,
retry, redirect, upload, download or streaming behavior changes.

**Why a `modalityRequest` executor is a different contract.** It would be a new HTTP executor, not
an existing operation serialized, and it cannot preserve what the native path does today
([#1613](https://github.com/arcships/dim/issues/1613)):

| Existing behavior | What an operation must match |
| --- | --- |
| One resolution per adapter call | Resolve once per call and reuse that one resolution for the call's retries; a cache that answers a later call would report a configuration the adapter never resolved, so later calls resolve again |
| Streaming downloads to files with a 2 GiB limit (`dim-modality-aimux`) | Stream to a file; never buffer a whole reply into base64 |
| WebSocket realtime open/send/receive/close | A connection-scoped protocol, not one request and reply |
| Vendor route and credential routing, including the fal image routes and query parameters | The same route selection and credential handling the catalog declares |
| Redirects and per-vendor error mapping | The same rules the connection query applies now |
| Cancellation and release at every step | Cancels owner work; dropping the call settles it and keeps the caller's token |

The `modalityRequest` operation proposed in an earlier revision of [#1399](https://github.com/arcships/dim/pull/1399) also accepted
Bedrock routes without SigV4 signing, while every bundled media adapter refuses a Bedrock
connection. Media support on Bedrock would be a new feature, not a serialization of an existing
one.

**Acceptance, if the operation is built later.** The maintainer confirms the operation's shape,
ownership and coverage before implementation; existing media operations and their limits are
preserved or a scope change is stated; one resolution per call lifetime with no mixed snapshots;
Rust and an actual foreign-runtime flow exercise the same request, reply, cancellation and error
contract; tests cover the affected flow boundaries, including credential confinement, vendor
routes, query parameters, redirects, download streaming and realtime cleanup; file transfer
agrees with [#1564](https://github.com/arcships/dim/issues/1564) and its active changes rather than
adding a second upload implementation.

**Consumer consequence.** Provider exposes no media-execution operation, so a foreign-language
adapter serves no media operation. Modality states that in its own document;
this page defines nothing about how an adapter declares or runs its work.

## 4. Client integration

`provider.management` is scoped to one Provider; the former no-argument
aggregate read is removed. A client that needs every Provider's version,
connection and model descriptors pages `provider.management.list` for the
overview and requests `provider.models.query` for the models it displays or
edits; the exact `provider.management {providerId, modelId?}` snapshot serves
one editing form. These reads are bounded pages or one explicit target, and the
server retains neither their results nor a parsed catalog. Migrating from
`provider.management {}` maps the provider-list and save-version reads to
`provider.management.list`, and the model capability and limit reads to
`provider.models.query` or the exact management snapshot.

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
draft when requested, then update the changed fields under §3.2's listing rules.
Disconnect sends credential
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
adopts that model and writes the default the next Session in the launch
workspace reads: the workspace scope when that folder has one, otherwise the
global scope, with `expectedVersion` from the same scope. A workspace write's
notice names that folder, and a global write's notice names no folder. A
cycled effort is saved as its `reasoningEffort` and the adoption then sends only
the target, so the server applies the value just saved together with the model's
other saved preferences. Editing a Provider's model list is the
models management area ([CLI commands § The Models area](../cli-commands.md#the-models-area)):
`Manage models` in `/models` and `dimcode models PROVIDER add|edit|set|remove|inspect`
read `provider.management` and write through `provider.models.*`; the picker
actions above are unchanged.

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

An undeclared model is not a picker row. The picker's `Model management`
section, outside the scrolling candidate list, keeps the compact diagnostics:
how many models have missing limits, how many are unavailable or disabled, how
many Providers have no models, and how many catalogs the owner could not read
([CLI commands § The Models area](../cli-commands.md#the-models-area)). Enter
opens a Provider list whose rows name each Provider and its state; a Provider
with at least one model missing limits says how many, and its footer names the
command that declares them (`dimcode models PROVIDER --set-missing-limits ...`).
A Provider that offers nothing selectable stays reachable in that list with its
reason, including `no models` and `catalog unreadable`. Choosing a Provider
opens the existing models management area, so the diagnostics never become
model candidates. A daemon that does not advertise `provider.management` shows
the same reasons and their recovery commands as read-only lines, with no
executable management action.
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
Models
  Conversation model
  > Provider / model          ZDR  ‹ high ›
  Selected: Provider / model
Enter select + default · ←/→ effort · Tab section · Esc clear/close
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

The logical record fields introduced by the [v19 decision](native-storage-format.md#329-格式决定)
carry user configuration; missing maps/lists mean no corresponding user data.
Their native representation now follows [v52](native-storage-format.md#v52-sparse-provider-state):

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

Source adoption follows §3.1's indexed-source contract. Collisions appear in the
existing per-entry errors and preserve the admitted base. The explicit
[native v52 format](native-storage-format.md#v52-sparse-provider-state) supersedes
v51's eager projection storage; it does not redefine earlier committed formats.

The v19 decision accepted loss of its then-new top-level fields when an old v19
binary rewrote a record, including automatic `seed_bundled` rewrites. That historical
compatibility decision does not permit opening a v52 store with an older daemon.
Unknown database formats retain their existing rejection and byte-preservation rules.

| Edit | Effect on already-adopted Sessions |
| --- | --- |
| Credential, base URL, headers, proxy, timeout | Current connection is resolved on the next request; already-issued I/O uses its admitted connection |
| Descriptor/capability edits or saved preferences | Adopted snapshot stays unchanged; changes apply on the next explicit adoption, saved preferences then applied by the server unless the request states them |
| Disable/delete Provider or model | Next request/adoption fails unavailable/not found; no forced switch or cross-owner reference rewrite |
| A limit declared, or its source removed | An adopted Session keeps its snapshot. With a limit undeclared, the next request fails before credentials or network I/O and names the missing limits; after both are declared, selecting the model adopts them (§3.5). The request output allowance is re-derived from the kept snapshot at every request, never from a stored value above the rule |
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
record. The converted record states the definition's name, not the source row's:
a builtin's name is catalog data the user does not set, so a source row written
before a catalog rename still imports and carries the current name. Its driver
and auth must match, because those decide what the record reaches. A `custom`
row whose id belongs to the bundled catalog is rejected.
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
| Model limits | A probed model the descriptor leaves without a limit is listed with `missingLimits`, refused by selection, adoption and preferences, and becomes selectable with its declared window in the adopted snapshot after the CLI declares both; a bundled match stays declared; a managed model without limits keeps its reason instead of a substitute window; the model-file profile still refuses an entry that lacks either value; an output declared at or above the window is accepted, the adopted configuration carries the derived allowance, and the snapshot keeps the declaration |
| Selectable read | The read equals the set `select`/`configuration` accept at the same snapshot for the same providers; a disabled model, a disabled Provider, an unavailable model, a model with no `text` modality, a non-chat model on another driver, a route-less model and a signed-out account are absent while their Provider still answers; each returned `capabilities.modalities` equals the catalog model's declared set; a model with an undeclared limit is absent rather than making the read fail; managed and static providers both appear; a mock endpoint receives no request; a write changes the next read and not an earlier reply; cancelling the token and Provider close use the existing errors |
| Stored selection | For one admitted static record and bundled catalog, `stored_selection` accepts exactly the model IDs `select`/`configuration` accept and returns the same protocol and adopted capabilities; fixed drivers and New API explicit, inferred, unsupported and ambiguous routes; missing, disabled, unavailable, non-text, disabled-Provider, missing-credential and undeclared-limit models are refused; an account-owned record is refused; a mock endpoint receives no request; the Migration provider/selection flow and `provider.import_legacy` report the same outcomes through the shared contract |
| Accounts | Existing login/logout/usage; managed same-account refresh preserves preferences, descriptor overrides and disables, while a different-account refresh clears all three; actual advertised-method versus missing-package-method behavior, unchanged Usage DTO, no static usage config |
| Concurrency/recovery | Two-client typed CAS conflicts on new and retained writes; lost create reply recovered by canonical UUID; noncanonical create IDs fail while existing non-UUID IDs still work with other methods; repeated create conflicts; read errors preserve healthy entries/drafts |
| Storage/import | New record read and rewritten by old v19 code including reseed; bundled upgrade collides with a user-added builtin model: Provider activates, that record/definition/fingerprint/version remain unchanged, independently admitted records update and management reports the collision; explicit deletion keeps a restart-required diagnostic until access after the next activation adopts the new definition; deleting a disabled user model clears its stale disable so the new bundled model is enabled; a user model created with a disappeared default's ID starts enabled without that default's descriptor override, preferences or endpoint override; valid/invalid/repeated imports and source preservation in disposable databases |
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
| §4.7 | Accounts use public methods; Modality reads selectable models through §3.6's core host port; Migration uses §3.7's Provider-owned judgment; other owners' references are not rewritten |
| §4.8 | The error-data requirement below uses the method owner's generic error fields; §5 uses existing metadata and v19 top-level JSON; §3.6's read is a data face ([service faces §4.10](../service-faces.md#410-宿主读取端口与-provider-选择)), so a non-Rust consumer calls it by name; §3.7 adds no requirement |
| §4.9 | No new Timeline event or pushed process notification |
| §1 | Extend the existing process Provider; no Capability or wiring plugin |
| ADR 002 D1 | Process records belong to Provider; adopted branch state to its owner; drafts to clients |
| ADR 002 D2 | Reuse namespaces/resolver; no management plugin or second registry |
| ADR 002 D3 | Optional account failure affects its own operations; no core dependency; §3.7 is a Provider module function |
| ADR 002 D4 | Business configuration is owner state; updates do not reload |
| ADR 002 D5 | §3 names fields, methods, mutability, interruption and errors independently of Rust bindings; §3.6 states the process read in neutral form, and its core host port is the binding a consumer declares; §3.7 is a Rust-only module boundary because its input is Provider's admitted record with a credential reference and it has no cross-language consumer |

### Requirements raised to mechanism owners

- **Problem:** §3.4 needs structured recovery data in a method error; a fixed
  application code and safe message cannot carry a version and public identity.
- **Existing mechanisms:** message parsing loses the typed contract, and another
  management read cannot classify the state observed by the rejected write.
- **Generic capability:** native methods supply safe owner-defined error fields,
  preserved by the [method error contract](../contracts/session-boundary.md#appendix-c-errors-and-recovery).
  Rpc does not interpret Provider fields.
- **Ordering impact:** none on sequence, batches, interruption or reload; rejected
  writes leave the record unchanged.
- **Owner/issue:** method errors in `dim-agent-capabilities` and App Server;
  [#1495](https://github.com/arcships/dim/issues/1495).

## Scoped downstream reads

`provider.management` accepts `page: {after?, limit?, query?}` for a single
Provider's model editor. `modelId` and `page` are mutually exclusive. Limits are
1–128, default 32; `after` is the last returned model ID and `next:null` ends
traversal. IDs sort lexically. Search matches model ID or display name separately,
case-insensitively. Disabled and unavailable models remain editable and visible.
A page's model rows, preferences and Provider version come from one committed
snapshot; later pages may observe later versions. Page reads decode only the
selected payloads and the naming information required for their labels.

The Web model editor requests its first page on opening, sends searches to the
owner, and loads further pages on demand. A single-model write is followed by an
exact `modelId` read and replaces that displayed row. Opening an existing model
for editing also reads its current row, preferences and version together. Deletion removes only that
row. With an active search, a write instead rereads the first bounded page of
that query so the owner determines whether changed rows still match. Returning
to the Models tab starts a new unfiltered first page, matching its empty search
field. A record-version conflict requires rereading before another write.

`provider.model.set_enabled` accepts `{providerId, expectedVersion, modelId,
enabled}` and returns `{version}` through `managementMethods`. It checks the
Provider version, managed-account identity and target availability, then updates
only that model's disabled state in the same transaction as the version. Unknown
targets return `provider_not_found`; unavailable targets cannot be toggled.
This operation does not infer other models' enablement from a displayed page.

`provider.models.query` accepts `selectableOnly: true` to return only targets
accepted by the owner's existing execution predicate. The cursor still advances
across examined candidates; a filtered page can be empty and carry `next`.
ACP consumes these bounded pages to build its protocol's complete available-model
response. Model IDs shared by multiple providers remain ambiguous and are omitted.
CLI limit declarations naming one model pass `modelId` both for the version read and the
post-write display. Bulk limit declarations enumerate the requested missing-limit
set.

## 9. Ablation review

The scoped management read supplies one edit snapshot. Known-target capability
reads specify the provider and model ID; model pickers use `provider.models.query`.
Create, update and probe respectively handle recoverable identity, secret-
preserving writes and unsaved I/O. Disconnect reuses update. Separate user-model
storage is required to distinguish it from an orphaned default override and make
it executable. The route stamp preserves the adopted protocol meaning using
existing metadata. A declared limit needs no new state: the marker and match the
catalog already records say whether a value exists, an absent value carries its
absence in the catalog type, and the reason is derived at the projection, so no
parallel model list, no second catalog and no `limits` entity is added. The
selectable-model read adds no state, cache, event or model type: it applies the
existing `ProviderCatalog::selectable` to each exact-target catalog and returns
the existing `ModelCapabilitySnapshot`, so there is no second availability
predicate or parallel catalog. The stored-selection contract likewise adds no
state, store or account read, cache or predicate: it builds the existing catalog,
applies the existing predicate, and returns the existing adopted snapshot with the
route execution already derives. A named reply type is required because the route
and snapshot are the public shape; returning the catalog or a client DTO would
expose more and still need the same judgment. No migration-side predicate or
catalog construction is added, so the selection conversion calls this contract
instead of a legacy-only private judgment. No notification, version-history
service, template entity, credential abstraction or second client configuration
store is needed.

The sparse representation needs no duplicate header projection, model-rewrite
trigger, retained decoded catalog or second resolver. Source archives keep an
admitted base usable without the old executable; keyed mutable rows preserve user
state independently of it. Naming dependency indexes contain the inputs required
by the existing collision rule, rather than another published-name store. Summary
replies reuse the existing management face and version contract. These structures
serve the required persistence and read boundaries; no further layer is needed.
