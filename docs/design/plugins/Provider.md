# Provider

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

Provider is the model owner of the product profile: it decides which model a request uses, resolves the credential that model needs, and runs the request. This document is the owner's target contract. The client protocol and the existing management behavior stay in [Provider management](../appserver/provider-management.md); this document replaces the object surfaces that document describes as the pre-migration state. The API-key profile has the same operations from [ConfiguredModels](ConfiguredModels.md).

## Use and scope: responsibility split

Provider owns:

- **Records.** Provider, model and selection records: static and imported records, descriptor overrides, user models, enables, per-model preferences, workspace and global selections, source generations and per-provider adoption. Records are read and written through the storage owner's operations; Provider keeps no SQL.
- **Selection.** The precedence among an explicit target, the workspace and global selections, the configured default and account-backed candidates; the availability rule execution applies; the adopted capability snapshot and configuration a selection carries.
- **Credentials.** Static credentials read from the record or an external secret store, managed-account leases, and temporary startup connections. A credential is used for one request and never leaves Provider except as the one-attempt lease a media call needs.
- **Execution.** One model request from the adopted selection through adaptation, transport, retries, credential recovery, streaming and settlement. Provider owns the driver that performs the request and its connection pool, request identity, stream set and tasks.
- **Temporary connections.** A startup connection admitted for one Session creation, the admission id that names it, and its settlement.
- **Client management.** The reads and writes the `provider` client namespace serves, with their transactions and versions.
- **The wire recorder.** The recorder its driver starts from ModelLog's `recording` value; the file, its redaction and its lifetime are Provider's.

Callers keep:

- Which model a Session adopts and when it changes; Provider answers the selection and the caller writes the Session's configuration.
- The request's messages, system prompt, tools and metadata; Provider adapts and sends what the caller built.
- What the output means: ModelProvider commits the stream to the Timeline and decides the turn's end; a one-shot caller collects the text.
- The summary budget and the retry display: the caller passes an admission callback and observes progress.
- Account login, logout and usage: the account owners' client namespaces, not Provider.
- Which search service a tool uses: Web's own settings, with the account owner's availability read.

| Requirement (callers) | Compose from the operations below without breaking Provider's invariants? | Handling |
| --- | --- | --- |
| Choose the model for a Session creation or a model change | No: the precedence and the availability rule are Provider's state and decisions | `resolve` |
| Apply a preference patch to an adopted selection | No: the patch is validated against the model's declarations | `preferences` |
| List the models that can run a use, and the current default | No: the list is the execution rule, one snapshot | `selectable` |
| Admit a temporary connection for a Session creation | No: the credential is a resource bound to the calling row; expiring or releasing it is Provider's | `prepare_startup`, `retain_startup`, `release_startup` |
| Run one model request with retries, recovery and streaming | No: credentials, retry budgets, failure chains and output adaptation stay with the owner | `invoke` |
| Learn a media model's route and where its credential comes from (the media executor) | No: the route is catalog state; the credential owner is the record's authority | `media_route` |
| Run one media exchange with a static model's credential (the media executor) | No: the credential is Provider's resource, and a caller holding it would release it on every error path | `with_lease` |
| Read or write a Provider's management record | No: records, versions and transactions are Provider's | The `provider` client namespace; handlers call the same operations |
| Show a retry countdown | Yes: the `invoke` progress callback | Business |
| Learn where the recorder writes | Yes: read ModelLog's `recording` value | Business |
| Decide the prompt, tools or output handling | Yes | Business |
| Decide whether a search is offered | Yes: read the account owner's availability | Business |

An operation exists only where the row says No.

## Caller example

The Rpc entry chooses a model while creating a Session:

```rust
let selection = provider
    .resolve(ResolveRequest { target: requested, cwd: Some(cwd) }, cancelled.clone())
    .await?;
session.configure(ConfigureUpdate { model: Some(selection), ..Default::default() }, &branch).await?;
```

The Rpc entry changes only the preferences of the model already adopted:

```rust
let next = provider
    .preferences(PreferencesRequest { selection: current, update: patch }, cancelled.clone())
    .await?;
session.configure(ConfigureUpdate { model: Some(next), ..Default::default() }, &branch).await?;
```

Modality lists the options for an operation:

```rust
let SelectableReply { models, default } = provider
    .selectable(SelectableRequest { use_: Use::Modality { operation: "image.generate".into() } }, cancelled.clone())
    .await?;
```

The media executor (the adapter that owns the protocol) resolves the route and then the credential owner it names; Modality passes the model target and holds no lease:

```rust
let route = provider
    .media_route(MediaRouteRequest { target, operation: "image.generate".into() }, cancelled.clone())
    .await?;
// `with_lease` resolves the face named by `route.credential` and runs the exchange inside it.
with_lease(&self.ctx, &route.credential, target, |lease| send_media(&lease), cancelled.clone()).await?;
```

ModelProvider runs a turn; `consume` is the branch part's own callback that writes `step/*` events and reports how the stream ended:

```rust
let outcome = provider
    .invoke(
        InvokeRequest {
            request,
            mode: Mode::Dialogue { chain: failure_chain },
            consume: consumer,
            admit: None,
        },
        cancelled.clone(),
    )
    .await?;
```

The summary strategy runs with its own budget:

```rust
provider
    .invoke(
        InvokeRequest { request, mode: Mode::Dialogue { chain: None }, consume, admit: Some(budget) },
        cancelled,
    )
    .await?;
```

SessionTitle and ImageUnderstanding run one request and keep the text:

```rust
let selection = provider.resolve(ResolveRequest { target: Some(model), cwd: None }, cancel.clone()).await?;
let reply = provider
    .invoke(
        InvokeRequest { request: ModelRequest::from(selection, messages), mode: Mode::Once, consume: collect_text, admit: None },
        cancel,
    )
    .await?;
```

The driver reads ModelLog's value when a request starts:

```rust
let destination = model_log.recording().await.ok().flatten();
// The recorder owns the file under this directory for the request it starts.
```

A Session creation with a temporary connection:

```rust
let Started { selection, admission } = provider.prepare_startup(request, cancelled.clone()).await?;
let session = sessions.create(/* with selection */).await?;
provider.retain_startup(RetainRequest { session: session.id().into(), model: selection }, cancelled).await?;
// On any failure before retention: provider.release_startup(admission, cancelled).await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| Storage owner ([#2018](https://github.com/arcships/dim/issues/2018)) | record reads, selection reads and writes, source activation and adoption, edit commits, candidate and page queries, legacy import | Provider records; the storage owner's contract defines the shapes |
| ProviderSettings ([#2017](https://github.com/arcships/dim/issues/2017)) | `user_agent` | The operator's User-Agent replacement |
| ModelLog ([#2017](https://github.com/arcships/dim/issues/2017)) | `recording` | The recorder's directory |
| FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)) | `root_path` | The directory the wire recorder writes under |
| Storage owner ([#2018](https://github.com/arcships/dim/issues/2018)) | `store.creationRecord` | The ChatGPT client-session rule (the Session's origin) |
| Session directory ([#2020](https://github.com/arcships/dim/issues/2020)) | session close observation | Releasing a retained startup admission |
| Account owners ([ManagedAccount](ManagedAccount.md)) | `availability`, `catalog`, `with_lease` | Account-backed models; the record's authority names the owner |

Provider reads an account owner by the name its record's credential authority carries; the face names are in [ManagedAccount](ManagedAccount.md).

### Operations

All operations are marked on `Provider` and provided under `providerService`; requests, replies and errors are data. `cancelled` is the caller's cancellation argument, not a request field.

| Operation | Intent | Request | Reply | Cancellation, timeout | Errors |
| --- | --- | --- | --- | --- | --- |
| `resolve` | The model one request resolves to | `{target?, cwd?}` | `Selection` | The caller cancels; no timeout | `invalid`, `not_found`, `unavailable`, `auth_required`, `cancelled`, `closed`, `failed` |
| `preferences` | Apply a patch to an adopted selection | `{selection, update}` | `Selection` | None; completes immediately | `invalid` |
| `selectable` | The models that can run a use, and the default | `{use: "chat" \| {modality}}` | `{models, default?}` | The caller cancels; no timeout | `cancelled`, `closed`, `failed` |
| `prepare_startup` | Admit a temporary connection and select its model | `{target?, cwd?, preferences, connection?, limits?}` | `{selection, admission}` | The caller cancels before admission | `invalid`, `invalid_connection`, `not_found`, `unavailable`, `auth_required`, `cancelled`, `closed`, `failed` |
| `retain_startup` | Bind the temporary connection the model carries to a Session | `{session, model}` | `{}` | None; completes immediately | `not_found` (the model carries no source, or the source is not bound), `closed` |
| `release_startup` | Release an admission before or after retention | `{admission}` | `{}` | None; completes immediately | `not_found`, `closed` |
| `invoke` | Run one model request | `{request, mode, consume, admit?}` | `{outcome}` | The caller cancels; the request stops and the consumer is released. No owner timeout; a request may wait on the transport through its configured HTTP timeout | `invalid`, `unavailable`, `cancelled`, `closed`, `provider_failed {retryable}` |
| `media_route` | A media model's route and the owner of its credential | `{target, operation}` | `{protocol, base_url, headers, realtime?, credential: Owner}` | The caller cancels; no timeout | `invalid`, `not_found`, `unavailable`, `cancelled`, `closed`, `failed` |
| `with_lease` | Run the caller's attempt with a static model's credential | `{provider, model}` plus `attempt(lease) -> {rejected}` | `{}` | The caller cancels; the attempt stops | `not_found`, `unavailable`, `cancelled`, `closed` |

`admission` is the caller's release handle before retention; `retain_startup` names the Session and the model it adopts, so a member Session that inherits the leader's model retains the same source by value and no caller holds the source. A connection never crosses. `Selection` is the adopted provider, model, configuration, capability snapshot and metadata. `Lease` is [ManagedAccount](ManagedAccount.md)'s one-attempt value; `with_lease` hands it to the caller's attempt and releases it when the attempt ends, so no lease id crosses.

`invoke`'s `mode` is `Dialogue { chain? }` or `Once`. `Dialogue` continues the failure chain when given one and reports retry waits through the optional `admit`/progress callbacks; `Once` makes exactly one request with no retry and no model substitution, which is what a caller sending one paid request needs (Report's model generation). `consume(next)` receives chunks, ends and typed failures and returns how the call ended and whether output was accepted; output accepted before a failure prevents replay. `admit` runs before each attempt and returns `admitted` or `rejected {message}`; only a caller with its own budget passes it.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the input: an unknown use, an explicit target missing its model, a malformed preference patch |
| `invalid_connection` | Fix the startup connection: a rejected URL, user info, query, fragment, both an API key and a bearer, or an unsupported auth |
| `not_found` | The provider, model or admission does not exist; re-read or choose another |
| `unavailable` | The model cannot run now; the message names the reason; choose another model or fix the record |
| `auth_required` | Sign in through the account owner named by the record, then retry |
| `cancelled` | Nothing changed; the call stopped where it was cancelled |
| `closed` | This generation is unloading; call the next generation |
| `failed` | Provider's storage or a catalog read failed; retry the read |
| `provider_failed {retryable}` | The request failed after the owner's retries; `retryable` says whether a new `invoke` can succeed |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `resolve`, `selectable` | Read records and account state, apply the precedence and availability rule, return the value | The read stops; nothing changes | The call ends `cancelled` | Pending calls end `closed`; the next generation answers from current records | Provider's read tasks |
| `invoke` | Resolve or adopt the request's selection, adapt images, resolve the credential (account lease or static), send, adapt chunks, hand them to `consume`, retry or recover per the failure chain, settle the consumer | The transport is cancelled, the stream is dropped, the consumer is released; accepted output stays committed | The consumer callback is revoked; the call ends `cancelled`; admitted work is joined | Admission stops; in-flight requests are cancelled and joined; account leases are settled | Provider: request tasks, stream and lease. ModelProvider: the Timeline writes already committed |
| `prepare_startup` | Validate and admit the connection under an admission id bound to the calling row, select its model | Before admission; nothing is retained | The row's admissions are released | Admissions are dropped; a retained Session's connection is restored by its next generation | Provider: the admission; the calling row: its lifetime |
| `retain_startup` / `release_startup` | Read the model's source, add the Session to its owners, watch the Session; release an admission not yet retained | — | An unretained admission is released with the row | Retained sources survive the owner's reload as Session state is re-read | Provider: the source and its owners; the Session directory reports close |
| `media_route`, `with_lease` | Read the catalog and the credential owner, or hand a static credential to the attempt | The read stops; the attempt ends and the credential is dropped | The call ends `cancelled` | Pending calls end `closed` | Provider |

## Exceptions with reasons

- `with_lease` and the lease inside `invoke` hand the bearer to the caller's attempt for one request: the executor is the model owner or the media adapter and the account owner holds the grant, so the value crosses once and is never persisted, logged, quoted in an error or returned to a client.
- The `provider` client namespace is a client protocol, not a second service face: it is the mechanism's registration point for protected-local v3 clients, and every handler calls the operations above or the management operations this owner also owns.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part. Provider writes no Timeline event; it is not a Capability. ModelProvider, the branch Capability, consumes `invoke`. |
| §2 scopes | Process: records, catalogs and credentials outlive Sessions. Session and branch parts call the operations; no branch state is held. |
| §3 orthogonality | No other owner's state is read or changed. Account state comes from the account owner's operations; records from the storage owner's. |
| §4 rule 1 | No command event: model calls reach Provider as operations, not Timeline facts. |
| §4 rule 2 | No handoff: the request and its output stay with the caller and the Timeline. |
| §4 rule 3 | No before callback: `consume` writes through ModelProvider's own Timeline service, outside the commit lock. |
| §4 rule 4 | Core and assembly carry no business meaning; no prompt, tool or output policy appears here. |
| §4 rule 5 | No batches: Provider writes no Timeline fact. |
| §4 rule 6 | Context enters only through the request the caller built from adopted inputs and tool results. |
| §4 rule 7 | Callers use these operations; no caller reads a record, a connection or a catalog internal. |
| §4 rule 8 | Requirements are listed below; no runtime mechanism is added. |
| §4 rule 9 | No Timeline fact: selections and records are process state with owner writes. |
| §7 objects, ids, values, callbacks | Values: selections, lists, leases, routes. Callbacks: `consume`, `admit`. Ids: `admission`. No object crosses. |
| §7 no proxy | Account data comes from the account owner; the storage operations from the storage owner. Provider returns only its own records and decisions. |
| §7 one change | The delivery part migrates every caller and deletes every pre-migration name in one change. |
| ADR 002 D1 | Process: records, catalogs and credentials are shared across Sessions. |
| ADR 002 D2 | No wiring plugin: the operations are on the owner type. |
| ADR 002 D3 | Dependencies point from extensions to the core; Provider is a process service callers may depend on. |
| ADR 002 D4 | Structural configuration (the bundled catalog, the assembly) and business configuration (the `provider` source's user agent) are separate. |
| ADR 002 D5 | The face is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Storage owner ([#2018](https://github.com/arcships/dim/issues/2018)).** Provider owns no SQL and no connection. It needs operations for: a record read by projection (header, all, one model, selected models, version); selection read and versioned write; per-model preferences; candidate lookup by criteria; provider list, model pages and management pages with their ordering and limits; source activation and per-provider adoption from bundle facts; one commit operation per edit family (create, delete, header patch, connection replacement, refresh, model create/edit/delete, enablement, preferences) with the existing preconditions and returned projections; legacy import as one batch; and `store.creationRecord` for a Session's origin. The storage owner's document defines the names and shapes.
- **ProviderSettings and ModelLog ([#2017](https://github.com/arcships/dim/issues/2017)).** Their owner contracts define `user_agent` and `recording`; Provider reads both at request time. No requirement remains.
- **Session directory ([#2020](https://github.com/arcships/dim/issues/2020)).** Provider needs a close observation for a retained startup admission: when the Session closes without unload, the admission is released; an unload keeps it for restore.
- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `root_path` only; the recorder owns its JSONL file.
- **Account owners ([ManagedAccount](ManagedAccount.md)).** `availability`, `catalog`, `with_lease`; the face name comes from the record's credential authority.
- **Media executor (Modality's adapter contract).** The adapter resolves `media_route` and calls `with_lease` on the credential owner for the model target; Modality passes the target and settles the task, so no business caller holds a lease id. The adapter's service shape is Modality's contract.

## Binding appendix

`Provider` is the owner type; `providerService` is the wire name. `CancelledFn` is the binding's cancellation argument. The request and reply types are serde data; only `consume` and `admit` cross by reference.

```rust
use dim_agent_capabilities::host::CancelledFn;
use dim_agent_types::{ModelId, ModelRequest, ModelSelection, ModelCapabilitySnapshot, ModalityModelSnapshot};

pub type Admission = String;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRequest { pub target: Option<ModelId>, pub cwd: Option<String> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesRequest { pub selection: ModelSelection, pub update: ModelPreferences }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Use { Chat, Modality { operation: String } }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectableRequest { pub use_: Use }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectableReply {
    pub models: SelectableModels,
    pub default: Option<ModelId>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SelectableModels { Chat(Vec<ModelCapabilitySnapshot>), Modality(Vec<ModalityModelSnapshot>) }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupRequest {
    pub target: Option<ModelId>,
    pub cwd: Option<String>,
    pub preferences: ModelPreferences,
    pub connection: Option<StartupConnection>,
    pub limits: Option<StartupLimits>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainRequest { pub session: String, pub model: ModelSelection }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Mode { Dialogue { chain: Option<FailureChain> }, Once }

pub type ConsumeFn = Arc<dyn Fn(ModelChunk) -> BoxFuture<'static, Consumed> + Send + Sync>;
pub type AdmitFn = Arc<dyn Fn() -> BoxFuture<'static, Admitted> + Send + Sync>;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeRequest {
    pub request: ModelRequest,
    pub mode: Mode,
    pub consume: ConsumeFn,
    pub admit: Option<AdmitFn>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeReply { pub outcome: Outcome }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRouteRequest { pub target: ModelId, pub operation: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRouteReply {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub realtime: Option<RealtimeDeclaration>,
    /// The credential owner the media executor calls `with_lease` on: this owner, or an account owner.
    pub credential: CredentialOwner,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRequest { pub provider: String, pub model: String }

#[derive(thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderError {
    #[error("{0}")] Invalid(String),
    #[error("{0}")] InvalidConnection(String),
    #[error("{0}")] NotFound(String),
    #[error("{reason}")] Unavailable { reason: String },
    #[error("authentication is required")] AuthRequired,
    #[error("cancelled")] Cancelled,
    #[error("provider is unloading")] Closed,
    #[error("{0}")] Failed(String),
    #[error("{message}")] ProviderFailed { retryable: bool, message: String },
    #[error("{0}")] InvalidPath(String),
}

impl PublicService for Provider {}

#[rutis::service(name = "providerService")]
impl Provider {
    pub async fn resolve(&self, request: ResolveRequest, cancelled: CancelledFn) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn preferences(&self, request: PreferencesRequest) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn selectable(&self, request: SelectableRequest, cancelled: CancelledFn) -> Result<SelectableReply, ProviderError> { /* ... */ }
    pub async fn prepare_startup(&self, request: StartupRequest, cancelled: CancelledFn) -> Result<Started, ProviderError> { /* ... */ }
    pub async fn retain_startup(&self, request: RetainRequest) -> Result<(), ProviderError> { /* ... */ }
    pub async fn release_startup(&self, admission: Admission) -> Result<(), ProviderError> { /* ... */ }
    pub async fn invoke(&self, request: InvokeRequest, cancelled: CancelledFn) -> Result<InvokeReply, ProviderError> { /* ... */ }
    pub async fn media_route(&self, request: MediaRouteRequest, cancelled: CancelledFn) -> Result<MediaRouteReply, ProviderError> { /* ... */ }
    pub async fn with_lease(&self, request: LeaseRequest, attempt: AttemptFn, cancelled: CancelledFn) -> Result<(), ProviderError> { /* ... */ }
}
```

The `provider` client namespace keeps its published method names and request shapes; handlers call the operations above and the management operations. No management operation is also published on `providerService`.
