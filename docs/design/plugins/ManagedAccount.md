# Managed account owners

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

This document is the contract of the account owner role. Its implementations are DimOAuth, OpenAI Codex, Grok and the ChatGPT account; an account plugin built outside this repository implements the same role. Each implementation is its own owner with its own face name; the operations and errors below are the same for all. Per-owner facts, login flows and extra operations are in [DimOAuth](DimOAuth.md), [ManagedOAuth](ManagedOAuth.md) and [ManagedProviders](ManagedProviders.md).

## Use and scope: responsibility split

An account owner owns:

- **The account record.** The persisted grant and account state under the product home, written through FileStorage's operations with the account's own revision and lease.
- **Login and tokens.** Login attempts, the pending exchange, token refresh, rotation and revocation.
- **Model availability.** Whether the account is signed in and which models its plan serves; the catalog the plan projects.
- **The grant's generation.** The credential it hands out carries the generation it belongs to, so a rejection names the generation the provider refused; the owner decides whether to refresh or to answer with the current one.
- **Its client namespace.** `account.*`, `account.codex.*`, `account.grok.*` or `account.chatgpt.*`, served to protected-local clients; handlers call the owner's operations.

Callers keep:

- Which account a provider uses: the Provider record's credential authority names the owner.
- The request itself and its retries: the model owner sends it, calls `rejected` on a refusal and retries within its own failure chain; the account owner performs one refresh at most, not the request.
- What a login flow shows and when it starts: the client protocol.
- The search query and its presentation: Web, through DimOAuth's search operation when its settings name the relay.
- The title prompt and its fallback: SessionTitle, through DimOAuth's title operation.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Learn whether the account can serve, and follow changes | No: sign-in state and its changes are the owner's state | `availability`, `subscribe` |
| Project the models the plan serves | No: the plan, the grant and the catalog are the owner's | `catalog` |
| Get the connection facts for one request | No: the grant, its refresh window and its generation are the owner's | `credential` |
| Find out what to do after the provider rejected a generation | No: whether the rejected generation is current, and one refresh at most, are the owner's decision | `rejected` |
| Search through the relay (Dim) | No: the grant and the server-side model are the owner's | `search` |
| Generate a title through the relay (Dim) | No: the server model and quota are the owner's | `title` |
| Show login, logout, usage and resets | No: the flows, records and plan are the owner's | The client namespace |
| Decide which account to sign into | Yes: the client chooses from `provider.management`'s account namespaces | Business |

An operation exists only where the row says No.

## Caller example

The model owner sends one request, recovers once from a rejection and keeps the retry count:

```rust
let Credential { protocol, base_url, headers, bearer, generation } =
    account.credential(CredentialRequest { provider: record.provider_id.clone(), model: model.clone() }, cancelled.clone()).await?;
// Send with these facts. On a credential rejection:
let next = account.rejected(RejectedRequest { provider: record.provider_id, generation }, cancelled.clone()).await?;
// Send once more with `next`; further retries are this caller's failure chain.
```

Web follows the Dim account's availability:

```rust
let AccountAvailability { signed_in, .. } = account.availability(cancelled.clone()).await?;
account
    .subscribe(
        &self.ctx,
        Arc::new(|availability: AccountAvailability| {
            Box::pin(async move {
                // Declare or withdraw web_search from `availability.signed_in`.
                Ok(())
            })
        }),
        cancelled.clone(),
    )
    .await?;
```

The client namespace is the same owner's operations behind the client protocol; no part calls it.

## Own contract

### Face names

| Owner | Face name | Provider ids | Login | Extra operations |
| --- | --- | --- | --- | --- |
| DimOAuth | `dimAccount` | the relay providers its catalog projects | browser PKCE, device | `search`, `title` |
| OpenAI Codex | `codexAccount` | `openai-codex` | browser PKCE | — |
| Grok | `grokAccount` | `grok-build` | device | — |
| ChatGPT account | `chatgptAccount` | `openai-chatgpt` | browser PKCE | — |
| External account plugin | configured by its row | whatever its descriptor lists | its own | its own |

### Operations

All operations are marked on the owner type and provided under the owner's face name. `cancelled` is the caller's cancellation argument.

| Operation | Intent | Request | Reply | Cancellation, timeout | Errors |
| --- | --- | --- | --- | --- | --- |
| `availability` | Whether the account is signed in | `{}` | `{signed_in, identity?}` | The caller cancels; no timeout | `cancelled`, `closed`, `failed` |
| `subscribe` | Follow availability changes | `caller`, a `notify` callback | `{}` | The registration ends with the calling row or the owner's close; a call cancelled before registration changes nothing | `cancelled`, `closed` |
| `catalog` | The models the plan serves | `{}` | `{account, models}` | The caller cancels; a catalog read may wait on the account's own refresh | `unavailable`, `cancelled`, `closed`, `failed` |
| `credential` | The connection facts for one request | `{provider, model}` | `{protocol, base_url, headers, bearer, generation}`; a read inside the refresh window refreshes first | The caller cancels; a refresh already started finishes under the owner | `not_found`, `account_required`, `unavailable`, `cancelled`, `closed`, `failed` |
| `rejected` | The generation the provider refused; answer the credential to retry with | `{provider, generation}` | A credential; the current generation refreshes once, a superseded generation answers the current credential | The caller cancels; a refresh already started finishes under the owner | `not_found`, `account_required`, `unavailable`, `cancelled`, `closed`, `failed` |
| `search` (Dim) | Search through the relay | `{query, count}` | `{results}` | The caller cancels; the provider request is cancelled | `unavailable`, `failed`, `cancelled`, `closed` |
| `title` (Dim) | Generate a title through the relay | `{provider, text}` | `{title}` | The caller cancels; an HTTP exchange is bounded at 10 s | `unsupported_provider`, `unavailable`, `cancelled`, `closed`, `failed` |

`rejected` compares the generation with the current one: the current generation is refreshed once and the new credential returned; a generation already replaced answers the current credential without refreshing; a refresh the account rejects, or a recovered generation the provider rejects again, follows the sign-out rule and answers `account_required`. The caller never holds a lease, an id to release or an expiry.

### The bearer's boundary

`bearer` is a trusted in-process value, passed between parts of the same process; process-local plugins are not isolated from each other (AGENTS.md rule 11). What keeps it safe is where it never goes: a client reply, the UI, a log, an error text, a persisted record, the model context, or the Timeline. The account's own grant file is the token's store, not a copy of this reply. Every reply and error of this contract omits the bearer except `credential` and `rejected`, and errors quote at most the provider's status and code. [Provider](Provider.md), [ConfiguredModels](ConfiguredModels.md), [DimOAuth](DimOAuth.md), [ManagedOAuth](ManagedOAuth.md) and [ManagedProviders](ManagedProviders.md) link this paragraph instead of restating it.

### Errors

| Kind | Retry | What the caller does next |
| --- | --- | --- |
| `not_found` | No | The provider is not this owner's; re-read the record and use the owner it names |
| `account_required` | After sign-in | The account is signed out, or the refresh was rejected; the client signs in, then the caller calls again |
| `unavailable` | After the account changes | The plan or the relay cannot serve the model now; the message names the reason |
| `unsupported_provider` | No | This owner does not serve that provider; use the owner the record names |
| `cancelled` | Yes, by calling again | Nothing changed; the call stopped where it was cancelled |
| `closed` | Against the next generation | This generation is unloading; call the replacement generation |
| `failed` | Yes, after the store or provider recovers | The account store or the provider request failed; retry the business step |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `availability`, `subscribe` | Read the account state; publish the value and its changes | The read stops; a subscription ends | The subscription ends | The watch is rebuilt from the store; subscribers end | The owner |
| `catalog` | Read the grant, refresh if inside the window, project the plan's models | The wait ends; the owner's refresh continues | The call ends `cancelled` | Pending calls end `closed`; the next generation re-reads | The owner's refresh task |
| `credential` | Read the grant, refresh if inside the window, answer the facts and the generation | The wait ends; a started refresh continues | The call ends `cancelled` | Pending calls end `closed`; a persisted grant stays | The owner's refresh task |
| `subscribe` | Register the callback under the calling row and publish the current value once | Before registration; nothing registered | The registration ends with the row | The registration ends; no further values | The owner: the registration; the row: its lifetime |
| `rejected` | Compare the generation, refresh once or answer the current credential, or apply the sign-out rule | The wait ends; a started refresh continues | The call ends `cancelled` | Pending calls end `closed` | The owner |
| `search` / `title` | Send the relay request with the grant's bearer | The request is cancelled | The call ends `cancelled` | Pending requests end `closed` | The owner |
| Client namespace | Run the login flow, write the record through FileStorage and publish the result | The attempt is cancelled | The attempt stays queryable until its deadline | Attempts end with the generation | The owner |

## Exceptions with reasons

- The external account plugin's face name is configured by its row because the plugin is built outside this repository; the host records the name in the Provider record's authority.

## Client-visible changes

None. The `account.*`, `account.codex.*`, `account.grok.*` and `account.chatgpt.*` method names, fields, error codes and user-visible behavior are unchanged; handlers call these operations.

## Rule check

[Design rules §7](https://github.com/arcships/dim/pull/2029) is the rule source.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part per account owner; no Timeline facts, no Capability. |
| §2 scopes | Process: grants and generations outlive Sessions. |
| §3 orthogonality | The account owner reads no other owner's state; FileStorage provides file operations, Settings the directory list. |
| §4 rules 1–6, 9 | No command events, handoffs, before callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use these operations; no caller reads the grant, the token or the attempt table. |
| §4 rule 8 | FileStorage's operations are the only infrastructure dependency; no requirement beyond [#2080](https://github.com/arcships/dim/pull/2080). |
| §7 ids, values, callbacks | The credential is a value with a generation; availability is a read-only view; no object or id to release crosses. |
| §7 no proxy | The account owner returns its own account data; the model owner calls the owner the record names. |
| §7 one change | The delivery part migrates Provider, Web and the client namespace together. |
| ADR 002 D1 | Process: the account and its tokens are shared across Sessions. |
| ADR 002 D2 | No wiring plugin: the operations are on each owner type. |
| ADR 002 D3 | Account owners depend on the core FileStorage service; Provider depends on the account faces. |
| ADR 002 D4 | The account record and the directory are business configuration in the owner's store; the face name is structural. |
| ADR 002 D5 | The face is language-neutral; the Rust binding is each owner's appendix. |

## Requirements raised to mechanism owners

- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `read`, `update` and `write` for the account record; `write` for the pre-reset backup. The account record keeps its file name and revision semantics.
- **Settings ([#2017](https://github.com/arcships/dim/issues/2017)).** The account directory default is read from the host entry, not a Settings document; no requirement.
- **Storage owner ([#2018](https://github.com/arcships/dim/issues/2018)).** None: accounts persist through FileStorage, not the Session store.

## Binding appendix

Shared request, reply and error types; each owner's `#[rutis::service]` block is in its own document. `CancelledFn` is the binding's cancellation argument.

```rust
use dim_agent_capabilities::host::{CancelledFn, ModelProtocol};
use dim_agent_types::ModelCapabilitySnapshot;
use rutis::host::{BoxFuture, CallError};
use rutis::Ctx;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountIdentity { pub issuer: String, pub subject: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAvailability { pub signed_in: bool, pub identity: Option<AccountIdentity> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSnapshot { pub generation: u64, pub identity: AccountIdentity }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCatalog {
    pub account: AccountSnapshot,
    pub models: Vec<ModelCapabilitySnapshot>,
}

/// The owner calls this with each availability change while the calling row lives.
pub type NotifyFn =
    Arc<dyn Fn(AccountAvailability) -> BoxFuture<'static, Result<(), CallError>> + Send + Sync>;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRequest { pub provider: String, pub model: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedRequest { pub provider: String, pub generation: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub bearer: String,
    /// The grant generation; `None` for a static credential a profile owner answers.
    pub generation: Option<u64>,
}

/// The wire code is the variant's snake_case name; `details` carries its data.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", content = "details", rename_all = "snake_case")]
pub enum AccountError {
    #[error("{0}")] NotFound(String),
    #[error("sign in to the account")] AccountRequired,
    #[error("{reason}")] Unavailable { reason: String },
    #[error("{0}")] UnsupportedProvider(String),
    #[error("cancelled")] Cancelled,
    #[error("the account owner is unloading")] Closed,
    #[error("{0}")] Failed(String),
}
```

The `notify` callback is invoked once with the current value when the registration starts, then on every change until the calling row unloads or the owner closes; it is not called inside a commit lock.

## Supersedes

- `service-faces.md`: the `grokOAuthService` and `openAICodexOAuthService` rows; the `managedSearch` row and §4.6, which the account owners' `search` replaces.
- `appserver/provider-management.md`: the account rows in §2 and §3.1 that describe the account catalog and status as reported by a host binding.
- `crates/dim-app-server/README.md`: the managed provider contract's account and lease paragraphs; the account owners answer them directly.
- `resource-faces.md` §6: the managed OAuth and OAuth token store row, whose `lock`/`unlock` shape the account owner's FileStorage `read`/`update`/`write` replaces.
