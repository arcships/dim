# Managed account owners

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

This document is the contract of the account owner role. Its implementations are DimOAuth, OpenAI Codex, Grok and the ChatGPT account; an account plugin built outside this repository implements the same role. Each implementation is its own owner with its own face name; the operations and errors below are the same for all. Per-owner facts, login flows and extra operations are in [DimOAuth](DimOAuth.md), [ManagedOAuth](ManagedOAuth.md) and [ManagedProviders](ManagedProviders.md).

## Use and scope: responsibility split

An account owner owns:

- **The account record.** The persisted grant and account state under the product home, written through FileStorage's operations with the account's own revision and lease.
- **Login and tokens.** Login attempts, the pending exchange, token refresh, rotation and revocation.
- **Model availability.** Whether the account is signed in and which models its plan serves; the catalog the plan projects.
- **Request leases.** One request's connection facts, their expiry and capacity, recovery after a rejection, and the final settlement.
- **Its client namespace.** `account.*`, `account.codex.*`, `account.grok.*` or `account.chatgpt.*`, served to protected-local clients; handlers call the owner's operations.

Callers keep:

- Which account a provider uses: the Provider record's credential authority names the owner.
- The request itself: the model owner or the media executor performs the attempt with the lease the owner hands to its callback, and the owner settles the lease after the callback.
- What a login flow shows and when it starts: the client protocol.
- The search query and its presentation: Web, through DimOAuth's search operation when its settings name the relay.
- The title prompt and its fallback: SessionTitle, through DimOAuth's title operation.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Learn whether the account can serve, and follow changes | No: sign-in state and its changes are the owner's state | `availability`, `subscribe` |
| Project the models the plan serves | No: the plan, the grant and the catalog are the owner's | `catalog` |
| Run one request with the account's credential, recovering once when the provider rejects it | No: the grant, the refresh and the lease's capacity are the owner's; a caller holding a lease id would have to release it on every error, cancel and unload path | `with_lease` |
| Search through the relay (Dim) | No: the grant and the server-side model are the owner's | `search` |
| Generate a title through the relay (Dim) | No: the server model and quota are the owner's | `title` |
| Show login, logout, usage and resets | No: the flows, records and plan are the owner's | The client namespace |
| Decide which account to sign into | Yes: the client chooses from `provider.management`'s account namespaces | Business |

An operation exists only where the row says No.

## Caller example

The model owner runs one attempt inside the owner's operation; the account owner hands over the lease, recovers once when the attempt reports a rejection, and settles the lease afterwards. No lease id appears here:

```rust
let WithLeaseReply { recovered } = account
    .with_lease(
        LeaseRequest { provider: record.provider_id.clone(), model: model.clone() },
        Arc::new(move |lease: Lease, cancelled: CancellationToken| {
            Box::pin(async move {
                // One exchange with `lease`. `Rejected` only when the provider rejected the credential.
                match send_once(&lease, cancelled).await {
                    Err(CredentialRejected) => AttemptReply { rejected: true },
                    _ => AttemptReply { rejected: false },
                }
            })
        }),
        cancelled.clone(),
    )
    .await?;
```

Web follows the Dim account's availability:

```rust
let AccountAvailability { signed_in, .. } = account.availability(cancelled.clone()).await?;
let mut changes = account.subscribe(SubscriptionRequest {}, cancelled.clone()).await?;
while let Some(available) = changes.next().await { /* declare or withdraw web_search */ }
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
| `subscribe` | Follow availability changes | `{}` | `{subscription}` | The subscription ends with the caller's row | `closed` |
| `catalog` | The models the plan serves | `{}` | `{account, models}` | The caller cancels; a catalog read may wait on the account's own refresh | `unavailable`, `cancelled`, `closed`, `failed` |
| `with_lease` | Run the caller's attempt with one request's connection facts, recovering once when the attempt reports a rejection | `{provider, model}` plus `attempt(lease) -> {rejected}` | `{recovered}` | The caller cancels; the attempt stops and the owner settles the lease | `not_found`, `unavailable`, `cancelled`, `closed`, `rejected`, `failed` |
| `search` (Dim) | Search through the relay | `{query, count}` | `{results}` | The caller cancels; the provider request is cancelled | `unavailable`, `failed`, `cancelled`, `closed` |
| `title` (Dim) | Generate a title through the relay | `{provider, text}` | `{title}` | The caller cancels; an HTTP exchange is bounded at 10 s | `unsupported_provider`, `unavailable`, `cancelled`, `closed`, `failed` |

`Lease` is a value handed to the attempt callback: `{lease_id, expires_at, protocol, base_url, headers, bearer}`. The owner records the id for capacity (1024) and expiry (30 minutes) and settles it when the callback returns, the caller drops the call, the lease expires, or the owner closes or reloads; `lease_id` lets a caller release early, and settling is otherwise the owner's. One `with_lease` call performs at most one controlled recovery: it adopts a newer persisted grant or performs one serialized refresh, then calls the callback once more with the new lease. A second rejection settles the lease and applies the sign-out rule only when the rejected grant still owns the account.

`attempt` runs inside the account owner's call, not inside a lock: the owner holds no grant or file lock while the callback runs, and the callback must not call back into the same account owner. The callback may perform the network exchange for the request; `cancelled` is the call's token, which ends the attempt when the caller drops the call or the owner closes.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `not_found` | The provider is not this owner's; re-read the record |
| `unavailable` | Sign in, renew the plan or wait for the account; the message names the reason |
| `unsupported_provider` | This owner does not serve that provider; use the owner the record names |
| `cancelled` | Nothing changed; the attempt stopped and the lease is settled |
| `closed` | This generation is unloading; call the next generation |
| `rejected` | Both attempts were rejected; the owner settled the lease and applied the sign-out rule; the caller reports the failure |
| `failed` | The account store or the provider request failed; retry the business step |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `availability`, `subscribe` | Read the account state; publish the value and its changes | The read stops; a subscription ends | The subscription ends | The watch is rebuilt from the store; subscribers end | The owner |
| `catalog` | Read the grant, refresh if inside the window, project the plan's models | The wait ends; the owner's refresh continues | The call ends `cancelled` | Pending calls end `closed`; the next generation re-reads | The owner's refresh task |
| `with_lease` | Read the grant, refresh if needed, record the lease id, call `attempt`; on `rejected`, one controlled recovery, then `attempt` again; release the lease | The token ends the attempt; the lease is released and the call ends `cancelled` | The callback is revoked; the call ends `cancelled`; the lease is released | The attempt ends; every lease id and the callback are released; a persisted grant stays | The owner: lease id, expiry, capacity and recovery; the calling row: its attempt state |
| `search` / `title` | Send the relay request with the grant's bearer | The request is cancelled | The call ends `cancelled` | Pending requests end `closed` | The owner |
| Client namespace | Run the login flow, write the record through FileStorage and publish the result | The attempt is cancelled | The attempt stays queryable until its deadline | Attempts end with the generation | The owner |

## Exceptions with reasons

- `Lease` carries the bearer into the callback: the model owner or the media executor performs the exchange, the account owner holds the grant, and the value lives only for one attempt; the contract forbids storing, logging or returning the bearer to a client. The account owner cannot perform the exchange itself: the model protocol, transport, streaming, adaptation and the per-profile driver belong to the executor, and duplicating them in every account owner would make each account own a transport. A secret value in a reply is forbidden by AGENTS.md and an object or handle by §7. The boundary is raised for maintainer confirmation in [#2096](https://github.com/arcships/dim/pull/2096).
- The external account plugin's face name is configured by its row because the plugin is built outside this repository; the host records the name in the Provider record's authority.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part per account owner; no Timeline facts, no Capability. |
| §2 scopes | Process: grants, tokens and leases outlive Sessions. |
| §3 orthogonality | The account owner reads no other owner's state; FileStorage provides file operations, Settings the directory list. |
| §4 rules 1–6, 9 | No command events, handoffs, before callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use these operations; no caller reads the grant, the token or the attempt table. |
| §4 rule 8 | FileStorage's operations are the only infrastructure dependency; no requirement beyond [#2080](https://github.com/arcships/dim/pull/2080). |
| §7 ids, values, callbacks | The lease is a value inside the owner's callback; the availability subscription is a read-only view; no object or lease id crosses. |
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
use dim_agent_capabilities::host::CancelledFn;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAvailability { pub signed_in: bool, pub identity: Option<AccountIdentity> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRequest { pub provider: String, pub model: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub lease_id: String,
    pub expires_at: u64,
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub bearer: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptReply { pub rejected: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithLeaseReply { pub recovered: bool }

/// The caller's attempt; `cancelled` ends it when the call or the owner ends.
pub type AttemptFn =
    Arc<dyn Fn(Lease, CancellationToken) -> BoxFuture<'static, AttemptReply> + Send + Sync>;

#[derive(thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AccountError {
    #[error("{0}")] NotFound(String),
    #[error("{reason}")] Unavailable { reason: String },
    #[error("{0}")] UnsupportedProvider(String),
    #[error("cancelled")] Cancelled,
    #[error("the account owner is unloading")] Closed,
    #[error("the provider rejected the recovered credential")] Rejected,
    #[error("{0}")] Failed(String),
}
```

`Subscription` is the owner's replayable watch over availability; it ends when the calling row unloads or the owner closes.
