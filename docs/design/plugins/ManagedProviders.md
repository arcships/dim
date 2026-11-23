# ManagedProviders (ChatGPT account; host binding retired)

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

This document covers the plugin at its target shape. The host binding and the `managedSearch` face are removed; the in-tree ChatGPT account remains as the `chatgptAccount` owner and implements [ManagedAccount](ManagedAccount.md). An account plugin built outside this repository provides its own face under the name its row configures.

## Use and scope: responsibility split

The remaining owner, the ChatGPT account, holds:

- **The account record** for `openai-chatgpt`, written through FileStorage's `update` with the record's revision.
- **Login.** Browser PKCE, the discovery document, the profile read and the pending attempt.
- **Tokens.** The access token's 30-second freshness window, one serialized refresh, rotation and the sign-out rule.
- **The plan catalog.** The models the ChatGPT plan serves, as the account catalog.
- **Credentials.** The connection facts for one request and the grant generation [ManagedAccount](ManagedAccount.md) defines.
- **The client namespace** `account.chatgpt.*`.

Removed with the host binding:

- The `dyn ManagedProvider` object port and the binding slot, generation and validation around it. An external account plugin is a normal process row: it provides its account face and owns its lifecycle, including quiesce before RPC settlement.
- The account data forwarding: `providers`, `status`, `recover`, `begin_browser`, `complete_browser`, `begin_device`, `attempt_status`, `cancel_attempt`, `logout`, `usage`, `referral`, `reset_batches`, `apply_reset`, `catalog` and `credential`; the old credential-recovery methods are replaced by the account face's `credential` and `rejected`. Callers request the account owner the Provider record names.
- The `managedSearch` face and its `search`, `usable`, `subscribe_usable` and `refresh_usable` methods. Web reads the Dim account's `availability` and calls its `search`; no host layer returns account data.

Callers keep the same responsibilities as [ManagedAccount](ManagedAccount.md): the model owner selects the account by the record's credential authority, Web owns the search tool and its declaration, and the client owns the login UI.

| Requirement (callers) | Compose from the account owners' operations without breaking their invariants? | Handling |
| --- | --- | --- |
| Read the ChatGPT account's status, catalog and credential | No: the grant, plan and its generation are this owner's | `availability`, `catalog`, `credential`, `rejected` |
| Search through the Dim relay | No: the Dim account's grant is DimOAuth's | DimOAuth's `availability`, `subscribe`, `search` |
| List the accounts a user can sign into | Yes: `provider.management` reports each record's account namespace | Business |
| Sign in, cancel, sign out | No: the attempt and record are this owner's | `account.chatgpt.*` |

## Caller example

Web reads the Dim account, not a host layer:

```rust
let AccountAvailability { signed_in, .. } = dim_account.availability(cancelled.clone()).await?;
let SearchReply { results } = dim_account.search(SearchRequest { query, count }, cancelled).await?;
```

The model owner runs a ChatGPT request inside the owner's operation:

```rust
chatgpt_account
    .credential(CredentialRequest { provider: "openai-chatgpt".into(), model: model.clone() }, cancelled)
    .await?;
// The request itself is Provider's; on a rejection it calls `rejected` with the generation.
```

A client signs in through the namespace; no part calls it:

```text
account.chatgpt.login.browser.begin {} -> { attemptId, authorizationUrl, expiresAt }
account.chatgpt.login.status { attemptId } -> { state }
```

## Own contract

### Face

`chatgptAccount`, marked on `ChatGptAccount`, implements [ManagedAccount](ManagedAccount.md)'s `availability`, `subscribe`, `catalog`, `credential` and `rejected`. The provider id is `openai-chatgpt`; login is browser PKCE; the record keeps its existing file and revision fields; the catalog keeps the plan's models and capabilities.

The client namespace `account.chatgpt.*` keeps its published methods. The existing rule that a ChatGPT plan request needs an interactive client Session stays in Provider: Provider reads the Session's creation record and refuses a non-client Session with `invalid`.

### Errors and flows

[ManagedAccount](ManagedAccount.md)'s kinds and flows. The account record's write, the one-recovery rule and the sign-out condition are unchanged.

## Exceptions with reasons

- The external account plugin's face name comes from its row: the plugin is built outside this repository, so the assembly supplies the name the Provider record's authority uses.
- The bearer's boundary is [ManagedAccount](ManagedAccount.md#the-bearers-boundary); these replies and errors omit it.

## Client-visible changes

None. The `account.chatgpt.*` method names, fields, error codes and user-visible behavior are unchanged; handlers call these operations.

## Rule check

[Design rules §7](https://github.com/arcships/dim/pull/2029) is the rule source.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part; ChatGPT is an account owner, not a Capability. |
| §2 scopes | Process: the grant and its generation are shared across Sessions. |
| §3 orthogonality | No other owner's state is read; the host binding and its account forwarding are gone. |
| §4 rules 1–6, 9 | No events, handoffs, callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use the account faces; no caller reads the binding or the account record. |
| §4 rule 8 | No infrastructure change. |
| §7 no proxy | Account data is returned by its owner; the host binding that returned it for every account is deleted. |
| §7 objects | The `dyn ManagedProvider` object port and the binding slot are deleted; the external plugin provides a face. |
| §7 one change | The delivery part deletes the binding, the forwarding methods and `managedSearch` and migrates Web, Provider and the client together. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin; the binding was one and is removed. |
| ADR 002 D3 | Depends on FileStorage; the model owner depends on the account faces. |
| ADR 002 D4 | The plan and the record are business state; the face name is structural for an external plugin. |
| ADR 002 D5 | The account face is language-neutral. |

## Requirements raised to mechanism owners

- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `read`, `update` and `write` for the ChatGPT account record.
- **Storage owner ([#2018](https://github.com/arcships/dim/issues/2018)).** Provider reads a Session's creation record through `store.creationRecord` to require an interactive client Session for this provider; the requirement is stated in [Provider](Provider.md)'s requirements.

## Supersedes

- `service-faces.md`: the `managedSearch` row and §4.6.
- `crates/dim-app-server/README.md`: the managed provider contract, including the binding, its validation and `ManagedSearch`.

## Binding appendix

```rust
impl PublicService for ChatGptAccount {}

#[rutis::service(name = "chatgptAccount")]
impl ChatGptAccount {
    pub async fn availability(&self, cancelled: CancelledFn) -> Result<AccountAvailability, AccountError> { /* ... */ }
    pub async fn subscribe(&self, cancelled: CancelledFn) -> Result<Subscription, AccountError> { /* ... */ }
    pub async fn catalog(&self, cancelled: CancelledFn) -> Result<AccountCatalog, AccountError> { /* ... */ }
    pub async fn credential(&self, request: CredentialRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
    pub async fn rejected(&self, request: RejectedRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
}
```
