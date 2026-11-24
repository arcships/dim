# Provider

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

Provider is the model owner of the product profile: it decides which model a request uses, resolves the credential that model needs, and runs the request. This document is the owner's target contract. The client protocol and the existing management behavior stay in [Provider management](../appserver/provider-management.md); this document replaces the object surfaces that document describes as the pre-migration state. The API-key profile has the same operations from [ConfiguredModels](ConfiguredModels.md).

## Use and scope: responsibility split

Provider owns:

- **Records.** Provider, model and selection records: static and imported records, descriptor overrides, user models, enables, per-model preferences, workspace and global selections, source generations and per-provider adoption. The records are Provider's own; the storage engine holds the file, lease and transaction primitive as a private implementation, and the rows never cross a public face.
- **Selection.** The precedence among an explicit target, the workspace and global selections, the configured default and account-backed candidates; the availability rule execution applies; the adopted capability snapshot and configuration a selection carries.
- **Credentials.** Static credentials read from the record or an external secret store, the managed-account credentials Provider uses for a request, and temporary startup connections. A credential is used for one request; the only value that crosses is the credential a media executor reads for one call.
- **Execution.** One model request from the adopted selection through adaptation, transport, retries, credential recovery, streaming and settlement. Provider owns the driver that performs the request and its connection pool, request identity, stream set and tasks.
- **Temporary connections.** A startup connection admitted for one Session creation; it is bound to the calling row until a Session adopts it, then to that Session.
- **Client management.** The reads and writes the `provider` client namespace serves, with their transactions and versions.
- **The wire recorder.** The recorder its driver starts from ModelLog's `recording` value; the file, its redaction and its lifetime are Provider's.

Callers keep:

- Which model a Session adopts and when it changes; Provider answers the selection and the caller writes the Session's configuration.
- The request's messages, system prompt, tools and metadata; Provider adapts and sends what the caller built.
- What the output means: ModelProvider commits the stream to the Timeline and decides the turn's end; a one-shot caller collects the text.
- The summary budget and the retry display: the caller passes an `admit` callback and receives `Retrying` chunks.
- Account login, logout and usage: the account owners' client namespaces, not Provider.
- Which search service a tool uses: Web's own settings, with the account owner's availability read.

| Requirement (callers) | Compose from the operations below without breaking Provider's invariants? | Handling |
| --- | --- | --- |
| Choose the model for a Session creation or a model change | No: the precedence and the availability rule are Provider's state and decisions | `resolve` |
| Apply a preference patch to an adopted selection | No: the patch is validated against the model's declarations | `preferences` |
| List the models that can run a use, and the current default | No: the list is the execution rule, one snapshot | `selectable` |
| Admit a temporary connection for a Session creation | No: the credential is a resource bound to the calling row and then to the Session; expiring or releasing it is Provider's | `prepare_startup`, `retain_startup`, `release_startup` |
| Run one model request with retries, recovery and streaming | No: credentials, retry budgets, failure chains and output adaptation stay with the owner | `invoke` |
| Learn a media model's route and where its credential comes from (the media executor) | No: the route is catalog state; the credential owner is the record's authority | `media_route` |
| Read a static model's credential for one media exchange (the media executor) | No: the record, the route and the credential are Provider's | `credential` |
| Read or write a Provider's management record | No: records, versions and transactions are Provider's | The `provider` client namespace; handlers call the same operations |
| Show a retry countdown | Yes: the `Retrying` chunk | Business |
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
line_owner::<PromptAdoption>(line)?.configure(
    ConfigureRequest { sources: None, model: Some(selection), tools: None },
    cancelled.clone(),
).await?;
```

The Rpc entry changes only the preferences of the model already adopted:

```rust
let next = provider
    .preferences(PreferencesRequest { selection: current, update: patch })
    .await?;
line_owner::<PromptAdoption>(line)?.configure(
    ConfigureRequest { sources: None, model: Some(next), tools: None },
    cancelled.clone(),
).await?;
```

Modality lists the options for an operation:

```rust
let SelectableReply { models, default } = provider
    .selectable(SelectableRequest { use_: Use::Modality { operation: "image.generate".into() } }, cancelled.clone())
    .await?;
```

The media executor (the adapter that owns the protocol) resolves the route and then reads the credential from the owner it names; Modality passes the model target and holds nothing to release:

```rust
let route = provider
    .media_route(MediaRouteRequest { target, operation: "image.generate".into() }, cancelled.clone())
    .await?;
// `credential` resolves the face named by `route.credential` and reads the value for this call.
let credential = credential_from(&self.ctx, &route.credential, target, cancelled.clone()).await?;
send_media(&credential).await?;
```

ModelProvider runs a turn; `consume` is the branch part's own callback that writes `step/*` events and reports how the stream ended:

```rust
let reply = provider
    .invoke(request, Mode::Dialogue { chain: failure_chain }, consumer, admit_all, cancelled.clone())
    .await?;
```

The summary strategy runs with its own budget:

```rust
provider
    .invoke(request, Mode::Dialogue { chain: None }, consume, budget, cancelled)
    .await?;
```

SessionTitle and ImageUnderstanding run one request and keep the text:

```rust
let selection = provider.resolve(ResolveRequest { target: Some(model), cwd: None }, cancel.clone()).await?;
let reply = provider
    .invoke(ModelRequest::from(selection, messages), Mode::Once, collect_text, admit_all, cancel)
    .await?;
```

The driver reads ModelLog's value when a request starts:

```rust
let destination = model_log.recording().await.ok().flatten();
// The recorder owns the file under this directory for the request it starts.
```

A Session creation with a temporary connection:

```rust
let selection = provider.prepare_startup(&self.ctx, request, cancelled.clone()).await?;
let session = sessions.create(/* with selection */).await?;
provider.retain_startup(RetainRequest { session: session.id().into(), model: selection }).await?;
// On a creation that fails before retention, the caller may release early:
provider.release_startup(&self.ctx, ReleaseRequest { model: selection }).await?;
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| ProviderSettings ([#2017](https://github.com/arcships/dim/issues/2017)) | `user_agent` | The operator's User-Agent replacement |
| ModelLog ([#2017](https://github.com/arcships/dim/issues/2017)) | `recording` | The recorder's directory |
| Storage engine ([#2018](https://github.com/arcships/dim/issues/2018)) | private transaction primitive; the public face keeps `storageReport` and the offline operations | Provider's records, selections, candidates, pages, source activation, adoption, edit commits and legacy import |
| SessionStore ([#2091](https://github.com/arcships/dim/pull/2091)) | `creationRecord({id})` | The ChatGPT client-session rule |
| Session directory ([#2020](https://github.com/arcships/dim/issues/2020)) | session close observation | Releasing a retained startup source |
| Account owners ([ManagedAccount](ManagedAccount.md)) | `availability`, `catalog`, `credential`, `rejected` | Account-backed models; the record's authority names the owner |

Provider reads an account owner by the name its record's credential authority carries; the face names are in [ManagedAccount](ManagedAccount.md).

### Operations

All operations are marked on `Provider` and provided under `providerService`; requests, replies and errors are data. `cancelled` is the caller's cancellation argument, not a request field.

| Operation | Intent | Request | Reply | Cancellation, timeout | Errors |
| --- | --- | --- | --- | --- | --- |
| `resolve` | The model one request resolves to | `{target?, cwd?}` | `Selection` | The caller cancels; no timeout | `invalid`, `not_found`, `unavailable`, `auth_required`, `cancelled`, `closed`, `failed` |
| `preferences` | Apply a patch to an adopted selection | `{selection, update}` | `Selection` | None; completes immediately | `invalid` |
| `selectable` | The models that can run a use, and the default | `{use: "chat" \| {modality}}` | `{models, default?}` | The caller cancels; no timeout | `invalid`, `cancelled`, `closed`, `failed` |
| `prepare_startup` | Admit a temporary connection under the calling row and select its model | `{target?, cwd?, preferences, connection?, limits?}` | `{selection}` | The caller cancels before admission | `invalid`, `invalid_connection`, `not_found`, `unavailable`, `auth_required`, `cancelled`, `closed`, `failed` |
| `retain_startup` | Bind the temporary connection the model carries to a Session | `{session, model}` | `{}` | None; completes immediately | `not_found` (the model carries no source, or the source is not bound), `closed` |
| `release_startup` | Release a source the calling row owns | `caller`, `{model}` | `{}` | None; completes immediately | `not_found` (already retained by a Session, or another row's), `closed` |
| `invoke` | Run one model request | `request`, `mode`, and the `consume` / `admit` callbacks as direct parameters | `{accepted}` | The caller cancels; the request stops and the consumer is released. No owner timeout; a request may wait on the transport through its configured HTTP timeout | `invalid`, `not_found`, `unavailable`, `auth_required`, `cancelled`, `closed`, `failed`, `provider_failed {retryable}` |
| `media_route` | A media model's route and the owner of its credential | `{target, operation}` | `{protocol, base_url, headers, realtime?, credential: Owner}` | The caller cancels; no timeout | `invalid`, `not_found`, `unavailable`, `cancelled`, `closed`, `failed` |
| `credential` | A static model's connection facts for one request | `{provider, model}` | `{protocol, base_url, headers, bearer, generation: none}` | The caller cancels; no timeout | `not_found`, `unavailable`, `cancelled`, `closed`, `failed` |

`prepare_startup` admits the source under the calling row; the row's unload releases it, so a missed `retain_startup` or `release_startup` settles with the row. `retain_startup({session, model})` moves the source to the Session named by the caller and the model's metadata; the Session's close releases it, so a missed failure-path release settles with the Session. `release_startup({model})` is the optional early release for a creation that failed; it answers `not_found` once a Session retains the source. A member Session that inherits the leader's model retains the same source by value. A connection never crosses. `Selection` is the adopted provider, model, configuration, capability snapshot and metadata. `Credential` is [ManagedAccount](ManagedAccount.md)'s value: static credentials carry no generation; an account owner's carry the grant's. `invoke` calls the account owner's `credential`, sends, and on a refusal calls `rejected` with the refused generation before retrying within its own failure chain.

`invoke`'s `mode` is `Dialogue { chain? }` or `Once`. `Dialogue` continues the failure chain when given one and reports retry waits as `Retrying` chunks; `Once` makes exactly one request with no retry and no model substitution, which is what a caller sending one paid request needs (Report's model generation). `consume(chunk)` reports `Continue`, `End`, `CredentialRejected` (the provider refused the credential, so `invoke` calls the account owner's `rejected` once and retries) or `Failed { retryable }`; output accepted before a failure prevents replay, and the reply's `accepted` says whether any output was accepted. `admit` runs before each attempt and answers `Admitted` or `Rejected { message }`; a caller without its own budget passes `admit_all`, the binding's default that admits every attempt. Both callbacks are direct parameters, so the request data stays serde-only. A request for `openai-chatgpt` requires a client-origin Session: Provider reads the Session's creation record (`creationRecord`) and answers `invalid` for a non-client Session.

### Errors

| Kind | Retry | What the caller does next |
| --- | --- | --- |
| `invalid` | No | Fix the input: an unknown use, an explicit target missing its model, a malformed preference patch, or a non-client Session for the ChatGPT provider |
| `invalid_connection` | No | Fix the startup connection: a rejected URL, user info, query, fragment, both an API key and a bearer, or an unsupported auth |
| `not_found` | No | The provider, model or source does not exist; re-read the record or choose another |
| `unavailable` | After the model or account changes | The model cannot run now; the message names the reason; choose another model or fix the record |
| `auth_required` | After sign-in | Sign in through the account owner named by the record, then call again |
| `cancelled` | Yes, by calling again | Nothing changed; the call stopped where it was cancelled |
| `closed` | Against the next generation | This generation is unloading; call the replacement generation |
| `failed` | Yes, after the store recovers | Provider's storage or a catalog read failed; retry the read |
| `provider_failed {retryable}` | When `retryable` | The request failed after the owner's retries; `retryable` says whether a new `invoke` can succeed |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `resolve`, `selectable` | Read records and account state, apply the precedence and availability rule, return the value | The read stops; nothing changes | The call ends `cancelled` | Pending calls end `closed`; the next generation answers from current records | Provider's read tasks |
| `invoke` | Resolve or adopt the request's selection, adapt images, read the credential (account owner or static), send, on a refusal call `rejected` and retry within the failure chain, adapt chunks, hand them to `consume`, settle the consumer | The transport is cancelled, the stream is dropped, the consumer is released; accepted output stays committed | The consumer callback is revoked; the call ends `cancelled`; admitted work is joined | Admission stops; in-flight requests are cancelled and joined; a started refresh finishes under the account owner | Provider: request tasks and stream. ModelProvider: the Timeline writes already committed |
| `prepare_startup` | Validate and admit the connection under the calling row, select its model | Before admission; nothing is retained | The row's unretained sources are released | Unretained sources are dropped; a retained Session's source is restored by its next generation | Provider: the source; the calling row: its lifetime |
| `retain_startup` / `release_startup` | Read the model's source, add the Session to its owners, watch the Session; release a source not yet retained | — | An unretained source is released with the row | Retained sources survive the owner's reload as Session state is re-read | Provider: the source and its owners; the Session directory reports close |
| `media_route`, `credential` | Read the catalog and the credential owner, or read a static credential | The read stops; the credential is dropped | The call ends `cancelled` | Pending calls end `closed` | Provider |

## Exceptions with reasons

- The bearer's boundary is [ManagedAccount](ManagedAccount.md#the-bearers-boundary): Provider never writes the account bearer it receives from `credential` or `rejected` to a client reply, the UI, a log, an error text, a persisted record, the model context or the Timeline. Static credentials in Provider's own records are the record's data, not this value.
- The `provider` client namespace is a client protocol, not a second service face: it is the mechanism's registration point for protected-local v3 clients, and every handler calls the operations above or the management operations this owner also owns.

## Client-visible changes

None. The `provider.*` client method names, request and reply fields, error codes and user-visible behavior are unchanged; handlers call the operations above.

## Rule check

[Design rules §7](https://github.com/arcships/dim/pull/2029) is the rule source.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part. Provider writes no Timeline event; it is not a Capability. ModelProvider, the branch Capability, consumes `invoke`. |
| §2 scopes | Process: records, catalogs and credentials outlive Sessions. Session and branch parts call the operations; no branch state is held. |
| §3 orthogonality | No other owner's state is read or changed. Account state comes from the account owner's operations; records are Provider's own. |
| §4 rule 1 | No command event: model calls reach Provider as operations, not Timeline facts. |
| §4 rule 2 | No handoff: the request and its output stay with the caller and the Timeline. |
| §4 rule 3 | No before callback: `consume` writes through ModelProvider's own Timeline service, outside the commit lock. |
| §4 rule 4 | Core and assembly carry no business meaning; no prompt, tool or output policy appears here. |
| §4 rule 5 | No batches: Provider writes no Timeline fact. |
| §4 rule 6 | Context enters only through the request the caller built from adopted inputs and tool results. |
| §4 rule 7 | Callers use these operations; no caller reads a record, a connection or a catalog internal. |
| §4 rule 8 | Requirements are listed below; no runtime mechanism is added. |
| §4 rule 9 | No Timeline fact: selections and records are process state with owner writes. |
| §7 objects, ids, values, callbacks | Values: selections, lists, credentials with a generation, routes. Callbacks: `consume`, `admit`. Ids: the calling row and the Session. No object crosses. |
| §7 no proxy | Account data comes from the account owner; records are Provider's own. The storage engine is not a face. |
| §7 one change | The delivery part migrates every caller and deletes every pre-migration name in one change. |
| ADR 002 D1 | Process: records, catalogs and credentials are shared across Sessions. |
| ADR 002 D2 | No wiring plugin: the operations are on the owner type. |
| ADR 002 D3 | Dependencies point from extensions to the core; Provider is a process service callers may depend on. |
| ADR 002 D4 | Structural configuration (the bundled catalog, the assembly) and business configuration (the `provider` source's user agent) are separate. |
| ADR 002 D5 | The face is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Storage engine ([#2018](https://github.com/arcships/dim/issues/2018)).** The engine's public face keeps only `storageReport` and the offline operations. Provider owns its records, selection, preferences, candidate and page queries, source activation, adoption, edit commits and legacy import; the engine supplies the private transaction primitive inside this crate, which is not a public operation.
- **SessionStore ([#2091](https://github.com/arcships/dim/pull/2091)).** `creationRecord({id})` for the ChatGPT client-session rule; SessionStore owns the record.
- **ProviderSettings and ModelLog ([#2017](https://github.com/arcships/dim/issues/2017)).** Their owner contracts define `user_agent` and `recording`; Provider reads both at request time. No requirement remains.
- **Session directory ([#2020](https://github.com/arcships/dim/issues/2020)).** Provider needs a close observation for a retained startup source: when the Session closes without unload, the source is released; an unload keeps it for restore.
- **Account owners ([ManagedAccount](ManagedAccount.md)).** `availability`, `catalog`, `credential`, `rejected`; the face name comes from the record's credential authority.
- **Media executor (Modality's adapter contract).** The adapter resolves `media_route` and reads the credential from the owner it names; Modality passes the target and settles the task, so no business caller holds a credential. The adapter's service shape is Modality's contract.
- **Id routing ([#2127](https://github.com/arcships/dim/issues/2127)).** The line-owner calls in the caller examples use the runtime's id routing; this contract raises no new mechanism requirement.

## Binding appendix

The owner type is `ProviderService` today (`crates/dim-app-server/src/provider.rs`; the delivery renames it to `Provider`). `providerService` is the wire name. `CancelledFn` is the binding's cancellation argument. The request and reply types are serde data; `consume` and `admit` are direct call parameters, never request fields.

```rust
use dim_agent_capabilities::host::{
    CancelledFn, ModelProtocol, ModelStopReason, RealtimeDeclaration,
};
use dim_agent_runtime::PublicService;
use dim_agent_types::FailureChain;
use dim_agent_types::{
    MessagePart, ModalityModelSnapshot, ModelCapabilitySnapshot, ModelId, ModelPreferences,
    ModelRequest, ModelSelection, ToolCall, Usage,
};
use rutis::host::{BoxFuture, CallError};
use rutis::Ctx;
use std::collections::BTreeMap;
use std::sync::Arc;
// `Credential` is the shared account value defined in ManagedAccount.md; the delivery places it in
// the account module both owners use.
use crate::account::Credential;

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
#[serde(tag = "kind", content = "models", rename_all = "camelCase")]
pub enum SelectableModels { Chat(Vec<ModelCapabilitySnapshot>), Modality(Vec<ModalityModelSnapshot>) }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupConnection {
    pub api_format: ModelProtocol,
    pub base_url: String,
    pub api_key: Option<String>,
    pub auth_token: Option<String>,
    pub headers: BTreeMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupLimits { pub context_window: Option<u64>, pub max_output_tokens: Option<u64> }

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
#[serde(rename_all = "camelCase")]
pub struct ReleaseRequest { pub model: ModelSelection }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Mode { Dialogue { chain: Option<FailureChain> }, Once }

/// One streamed fact for the consumer, serialized to the wire; the delivery maps each variant to
/// its `step/*` event.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderChunk {
    Text { delta: String },
    Thinking { delta: String },
    ToolStart { id: String, name: String },
    ToolArguments { id: String, delta: String },
    ToolUse { call: ToolCall },
    Content { parts: Vec<MessagePart> },
    Stop { reason: ModelStopReason },
    Usage { usage: Usage },
    Retrying { attempt: u32, delay_ms: u64 },
}

/// What the consumer reports for one chunk; `CredentialRejected` asks the owner to recover once.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Consumed { Continue, End, CredentialRejected, Failed { retryable: bool } }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Admitted { Admitted, Rejected { message: String } }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeReply { pub accepted: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRouteRequest { pub target: ModelId, pub operation: String }

/// Who the media executor asks for the credential: this owner or the account owner named by the record.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CredentialOwner { Provider, Named { face: String } }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRouteReply {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub realtime: Option<RealtimeDeclaration>,
    pub credential: CredentialOwner,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticCredentialRequest { pub provider: String, pub model: String }

/// The wire code is the variant's snake_case name; `details` carries its data.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", content = "details", rename_all = "snake_case")]
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
}

impl PublicService for Provider {}

#[rutis::service(name = "providerService")]
impl Provider {
    pub async fn resolve(&self, request: ResolveRequest, cancelled: CancelledFn) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn preferences(&self, request: PreferencesRequest) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn selectable(&self, request: SelectableRequest, cancelled: CancelledFn) -> Result<SelectableReply, ProviderError> { /* ... */ }
    pub async fn prepare_startup(&self, caller: &Ctx, request: StartupRequest, cancelled: CancelledFn) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn retain_startup(&self, request: RetainRequest) -> Result<(), ProviderError> { /* ... */ }
    pub async fn release_startup(&self, caller: &Ctx, request: ReleaseRequest) -> Result<(), ProviderError> { /* ... */ }
    pub async fn invoke(
        &self,
        request: ModelRequest,
        mode: Mode,
        consume: impl Fn(ProviderChunk) -> BoxFuture<'static, Result<Consumed, CallError>> + Send + Sync + 'static,
        admit: impl Fn() -> BoxFuture<'static, Result<Admitted, CallError>> + Send + Sync + 'static,
        cancelled: CancelledFn,
    ) -> Result<InvokeReply, ProviderError> { /* ... */ }
    pub async fn media_route(&self, request: MediaRouteRequest, cancelled: CancelledFn) -> Result<MediaRouteReply, ProviderError> { /* ... */ }
    pub async fn credential(&self, request: StaticCredentialRequest, cancelled: CancelledFn) -> Result<Credential, ProviderError> { /* ... */ }
}
```

The `provider` client namespace keeps its published method names and request shapes; handlers call the operations above and the management operations. No management operation is also published on `providerService`.
