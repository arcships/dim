# DimOAuth

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019). Source: [`crates/dim-oauth`](../../../crates/dim-oauth/README.md).

DimOAuth is the account owner for the Dim relay. The account operations, errors and flows are [ManagedAccount](ManagedAccount.md)'s; this document records what DimOAuth adds and what it keeps in the owner's vocabulary.

## Use and scope: responsibility split

DimOAuth owns:

- **The account record.** `<product home>/auth.json` with `nextApiOauth` and `dimAccountState` schema 1 (`credentialEpoch`, `generation`, `credentialRevision`, `state`, and when authenticated `identity` and `tokenDigest`). Writes go through FileStorage's `update`; the record's revision is the business compare-and-set inside the change.
- **Login.** Browser PKCE and device flows, the directory discovery from the email or the host default, the localhost callback listener's browser opening, and the token exchange.
- **Tokens.** The 30-second freshness window, one serialized refresh under the account lock, rotation persisted before release, and the background renewal.
- **The relay catalog.** The provider ids and models the plan serves, projected as the account catalog the model owner reads.
- **Request leases.** One request's relay base URL, headers and bearer, with the lease id, capacity and expiry [ManagedAccount](ManagedAccount.md) defines.
- **Search and title.** The relay's `POST {relay}/web-search` and `POST {issuer}/v1/session/rename`.
- **The client namespace `account.*`.** Status, recover, login begin/browser/device, login status/cancel, logout, usage, referral, reset batches and reset apply.

Callers keep:

- Which provider uses the account: the record's credential authority.
- The search query and how results are shown: Web.
- The title prompt and the fallback when the relay is unavailable: SessionTitle.
- The login UI and when a sign-in starts: the client.

| Requirement (callers) | Compose from [ManagedAccount](ManagedAccount.md)'s operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Sign in, sign out, read usage and resets | No: the flow, the record and the plan are DimOAuth's | Client namespace `account.*` |
| Run one relay request with the account's credential, recovering once on a rejection | No: the grant and the lease table are DimOAuth's | `with_lease` |
| Search through the relay | No: the server-side model and the grant are DimOAuth's | `search` |
| Generate a session title through the relay | No: the server model, quota and prompt are the relay's | `title` |
| Learn whether the relay is signed in | No: state and its changes are DimOAuth's | `availability`, `subscribe` |

## Caller example

Web declares its search tool only while the relay account is signed in:

```rust
let AccountAvailability { signed_in, .. } = dim_account.availability(cancelled.clone()).await?;
if signed_in {
    let mut changes = dim_account.subscribe(SubscriptionRequest {}, cancelled.clone()).await?;
    // declare web_search; withdraw it when changes report signed_out
}
```

Web searches:

```rust
let SearchReply { results } = dim_account
    .search(SearchRequest { query, count }, cancelled.clone())
    .await?;
```

SessionTitle asks for a title when the Session's model belongs to the relay:

```rust
match dim_account.title(TitleRequest { provider: adopted.provider_id.clone(), text }, cancel.clone()).await {
    Ok(TitleReply { title }) => use_title(title),
    Err(TitleError::UnsupportedProvider | TitleError::Unavailable { .. }) => fallback_title(),
    Err(error) => return Err(error),
}
```

## Own contract

### Face and operations

Face `dimAccount`, marked on `DimOAuth`. The operations are [ManagedAccount](ManagedAccount.md)'s table, with these DimOAuth facts:

| Operation | DimOAuth's request and reply | Errors |
| --- | --- | --- |
| `availability`, `subscribe` | `{}` → `{signed_in, identity?}`; the identity is the grant's issuer and subject, never a token | `cancelled`, `closed`, `failed` |
| `catalog` | `{}` → `{account, models}`; the models are the relay's, each with its declared capabilities | `unavailable`, `cancelled`, `closed`, `failed` |
| `with_lease` | `{provider, model}` plus the attempt callback → `{recovered}`; `provider` must be one of the relay's; one serialized refresh on a rejection | `not_found`, `unavailable`, `cancelled`, `closed`, `rejected`, `failed` |
| `search` | `{query, count}` → `{results: [{title, url, snippet, publishedAt?}]}` | `unavailable`, `failed`, `cancelled`, `closed` |
| `title` | `{provider, text}` → `{title}`; `provider` must be the relay's | `unsupported_provider`, `unavailable`, `cancelled`, `closed`, `failed` |

The `account.*` namespace keeps its published methods; handlers call the same owner operations. A failed login attempt reports only `Login failed`; account-service error bodies are never returned.

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `with_lease` | Read the grant atomically when fresh; inside the 30-second window take the lock and refresh; record the lease id; call the attempt; on `rejected` one recovery under the account lock; release the lease | The token ends the attempt; the lease is released and the call ends `cancelled` | The callback is revoked; the lease is released | The attempt ends; leases and callbacks are released; a persisted grant stays | DimOAuth: grant, lease id, expiry and recovery; the calling row: its attempt |
| `search` / `title` | Send the relay request with the current bearer; a 401 follows the same one-recovery rule as `lease` | The request is cancelled | Pending requests end `closed` | DimOAuth |
| `account.*` | Run the login flow or the account read; write the record through FileStorage; publish the result | The attempt is cancelled | Attempts end with the generation; a completed token rotation settles before unload | DimOAuth |

## Exceptions with reasons

- `Lease` carries the bearer to the caller's attempt for one request; [ManagedAccount](ManagedAccount.md) states the rule and the prohibition on storing, logging or returning it.
- The account file is shared with the TypeScript subscriber under the published `auth.json.lock` marker protocol; FileStorage's contract records the marker exception, and DimOAuth keeps the file format.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part; no Timeline facts, no Capability. |
| §2 scopes | Process: the grant and leases outlive Sessions. |
| §3 orthogonality | No other owner's state is read; FileStorage provides the file operations. |
| §4 rules 1–6, 9 | No command events, handoffs, before callbacks, batches, context or Timeline state. |
| §4 rule 7 | Web, SessionTitle and the model owner call the face; none reads `auth.json`, the lock or the token. |
| §4 rule 8 | No infrastructure change; FileStorage's operations are the contract. |
| §7 ids, values, callbacks | The lease is a value with an id; availability and its changes are a read-only view. |
| §7 no proxy | DimOAuth returns its own account data; no host binding stands between the caller and this owner. |
| §7 one change | The delivery part deletes the account forwarding methods and migrates Web, SessionTitle and the model owner together. |
| ADR 002 D1 | Process: the account is shared across Sessions. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Depends on FileStorage only; the model owner depends on this face. |
| ADR 002 D4 | The directory and account record are business state in the owner's store. |
| ADR 002 D5 | The face is language-neutral; the lease value is data. |

## Requirements raised to mechanism owners

- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `read`, `update` and `write` for `auth.json` and its backup; the `auth.json.lock` marker exception is already recorded there.
- **Settings, storage, session directory.** None.

## Binding appendix

`DimOAuth` is the owner type; `dimAccount` is the wire name. The request and reply types are the [ManagedAccount](ManagedAccount.md) types plus the search and title types this crate already exports.

```rust
impl PublicService for DimOAuth {}

#[rutis::service(name = "dimAccount")]
impl DimOAuth {
    pub async fn availability(&self, cancelled: CancelledFn) -> Result<AccountAvailability, AccountError> { /* ... */ }
    pub async fn subscribe(&self, cancelled: CancelledFn) -> Result<Subscription, AccountError> { /* ... */ }
    pub async fn catalog(&self, cancelled: CancelledFn) -> Result<AccountCatalog, AccountError> { /* ... */ }
    pub async fn with_lease(&self, request: LeaseRequest, attempt: AttemptFn, cancelled: CancelledFn) -> Result<WithLeaseReply, AccountError> { /* ... */ }
    pub async fn search(&self, request: SearchRequest, cancelled: CancelledFn) -> Result<SearchReply, SearchError> { /* ... */ }
    pub async fn title(&self, request: TitleRequest, cancelled: CancelledFn) -> Result<TitleReply, TitleError> { /* ... */ }
}
```
