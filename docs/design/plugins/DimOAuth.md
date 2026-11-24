# DimOAuth

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019). Source: [`crates/dim-oauth`](../../../crates/dim-oauth/README.md).

DimOAuth is the account owner for the Dim relay. The account operations, errors and flows are [ManagedAccount](ManagedAccount.md)'s; this document records what DimOAuth adds and what it keeps in the owner's vocabulary.

## Use and scope: responsibility split

DimOAuth owns:

- **The account record.** `<product home>/auth.json` with `nextApiOauth` and `dimAccountState` schema 1 (`credentialEpoch`, `generation`, `credentialRevision`, `state`, and when authenticated `identity` and `tokenDigest`). Writes go through FileStorage's `update`; the record's revision is the business compare-and-set inside the change.
- **Login.** Browser PKCE and device flows, the directory discovery from the email or the host default, the localhost callback listener's browser opening, and the token exchange.
- **Tokens.** The 30-second freshness window, one serialized refresh under the account lock, rotation persisted before release, and the background renewal.
- **The relay catalog.** The provider ids and models the plan serves, projected as the account catalog the model owner reads.
- **Credentials.** The relay base URL, headers and bearer for one request, with the grant generation [ManagedAccount](ManagedAccount.md) defines.
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
| Get the relay credential for a request, and the decision after a rejection | No: the grant, its refresh window and its generation are DimOAuth's | `credential`, `rejected` |
| Search through the relay | No: the server-side model and the grant are DimOAuth's | `search` |
| Generate a session title through the relay | No: the server model, quota and prompt are the relay's | `title` |
| Learn whether the relay is signed in | No: state and its changes are DimOAuth's | `availability`, `subscribe` |

## Caller example

Web declares its search tool only while the relay account is signed in:

```rust
let AccountAvailability { signed_in, .. } = dim_account.availability(cancelled.clone()).await?;
if signed_in {
    // The owner calls `notify` once with the current value, then on every change;
    // the registration ends with this Session part's row or the owner's close.
    dim_account
        .subscribe(
            &self.ctx,
            Arc::new(|availability: AccountAvailability| {
                Box::pin(async move {
                    // Declare web_search from `availability.signed_in`.
                    Ok(())
                })
            }),
            cancelled.clone(),
        )
        .await?;
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
| `credential` | `{provider, model}` → `{protocol, base_url, headers, bearer, generation}`; `provider` must be one of the relay's; a read inside the 30-second window refreshes first | `not_found`, `account_required`, `unavailable`, `cancelled`, `closed`, `failed` |
| `rejected` | `{provider, generation}` → a credential; the current generation refreshes once under the account lock, a superseded generation answers the current credential; a rejected refresh or a rejected recovered generation follows the sign-out rule | `not_found`, `account_required`, `unavailable`, `cancelled`, `closed`, `failed` |
| `search` | `{query, count}` → `{results: [{title, url, snippet, publishedAt?}]}` | `unavailable`, `failed`, `cancelled`, `closed` |
| `title` | `{provider, text}` → `{title}`; `provider` must be the relay's | `unsupported_provider`, `unavailable`, `cancelled`, `closed`, `failed` |

The `account.*` namespace keeps its published methods; handlers call the same owner operations. A failed login attempt reports only `Login failed`; account-service error bodies are never returned.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `credential` | Read the grant atomically when fresh; inside the 30-second window take the lock and refresh; answer the facts and the generation | The wait ends; a started refresh continues under the owner | The call ends `cancelled` | Pending calls end `closed`; a persisted grant stays | DimOAuth's refresh task |
| `rejected` | Compare the generation; current → one serialized refresh, superseded → the current credential; a rejected refresh signs out the grant that holds the lock | The wait ends; the refresh continues | The call ends `cancelled` | Pending calls end `closed` | DimOAuth |
| `search` / `title` | Send the relay request with the current bearer; a 401 follows the same rule as `rejected` | The request is cancelled | The call ends `cancelled` | Pending requests end `closed` | DimOAuth |
| `account.*` | Run the login flow or the account read; write the record through FileStorage; publish the result | The attempt is cancelled | The attempt stays queryable until its deadline | Attempts end with the generation; a completed token rotation settles before unload | DimOAuth |

## Exceptions with reasons

- The bearer's boundary is [ManagedAccount](ManagedAccount.md#the-bearers-boundary); DimOAuth's replies and errors omit it.
- The account file is shared with the TypeScript subscriber under the published `auth.json.lock` marker protocol; FileStorage's contract records the marker exception, and DimOAuth keeps the file format.

## Client-visible changes

None. The `account.*` method names, fields, error codes and user-visible behavior are unchanged; handlers call these operations.

## Rule check

[Design rules §7](https://github.com/arcships/dim/pull/2029) is the rule source.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part; no Timeline facts, no Capability. |
| §2 scopes | Process: the grant and its generation outlive Sessions. |
| §3 orthogonality | No other owner's state is read; FileStorage provides the file operations. |
| §4 rules 1–6, 9 | No command events, handoffs, before callbacks, batches, context or Timeline state. |
| §4 rule 7 | Web, SessionTitle and the model owner call the face; none reads `auth.json`, the lock or the token. |
| §4 rule 8 | No infrastructure change; FileStorage's operations are the contract. |
| §7 ids, values, callbacks | The credential is a value with a generation; availability and its changes are a read-only view. |
| §7 no proxy | DimOAuth returns its own account data; no host binding stands between the caller and this owner. |
| §7 one change | The delivery part deletes the account forwarding methods and migrates Web, SessionTitle and the model owner together. |
| ADR 002 D1 | Process: the account is shared across Sessions. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Depends on FileStorage only; the model owner depends on this face. |
| ADR 002 D4 | The directory and account record are business state in the owner's store. |
| ADR 002 D5 | The face is language-neutral; the credential value is data. |

## Requirements raised to mechanism owners

- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `read`, `update` and `write` for `auth.json` and its backup; the `auth.json.lock` marker exception is already recorded there.
- **Settings, storage, session directory.** None.

## Supersedes

- `service-faces.md`: the `managedSearch` row and §4.6, whose `search` this owner answers.
- `crates/dim-app-server/README.md`: the managed provider contract's search paragraph; the account owner answers it directly.


## Binding appendix

`DimOAuth` is the owner type; `dimAccount` is the wire name. The request and reply types are the [ManagedAccount](ManagedAccount.md) types plus these:

```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest { pub query: String, pub count: u32 }

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchReply { pub results: Vec<SearchHit> }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", content = "details", rename_all = "snake_case")]
pub enum SearchError {
    #[error("{reason}")] Unavailable { reason: String },
    #[error("{0}")] Failed(String),
    #[error("cancelled")] Cancelled,
    #[error("the account owner is unloading")] Closed,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRequest { pub provider: String, pub text: String }

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleReply { pub title: String }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", content = "details", rename_all = "snake_case")]
pub enum TitleError {
    #[error("{0}")] UnsupportedProvider(String),
    #[error("{reason}")] Unavailable { reason: String },
    #[error("cancelled")] Cancelled,
    #[error("the account owner is unloading")] Closed,
    #[error("{0}")] Failed(String),
}

// The delivery maps the search types to the facade's `WebSearchRequest`/`WebSearchResponse` and the
// title types to `GenerateTitleRequest`/`GenerateTitleReply`; the operation shapes above are the face.

impl PublicService for DimOAuth {}

#[rutis::service(name = "dimAccount")]
impl DimOAuth {
    pub async fn availability(&self, cancelled: CancelledFn) -> Result<AccountAvailability, AccountError> { /* ... */ }
    pub async fn subscribe(&self, caller: &Ctx, notify: NotifyFn, cancelled: CancelledFn) -> Result<(), AccountError> { /* ... */ }
    pub async fn catalog(&self, cancelled: CancelledFn) -> Result<AccountCatalog, AccountError> { /* ... */ }
    pub async fn credential(&self, request: CredentialRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
    pub async fn rejected(&self, request: RejectedRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
    pub async fn search(&self, request: SearchRequest, cancelled: CancelledFn) -> Result<SearchReply, SearchError> { /* ... */ }
    pub async fn title(&self, request: TitleRequest, cancelled: CancelledFn) -> Result<TitleReply, TitleError> { /* ... */ }
}
```
