# Managed OAuth accounts (OpenAI Codex, Grok)

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

This document covers two owners that share one implementation: **OpenAI Codex** and **Grok**. The account operations, errors and flows are [ManagedAccount](ManagedAccount.md)'s; the two owners differ in provider id, issuer, login method and record namespace.

## Use and scope: responsibility split

Each owner holds:

- **The account record** for its provider under the product home, written through FileStorage's `update`.
- **Login.** Browser PKCE for Codex, device flow for Grok, the pending attempt and its deadline.
- **Tokens.** Refresh, rotation and the one-controlled-recovery rule.
- **The account's model availability.** The models the subscription serves, as the account catalog.
- **Request leases.** The lease ids, capacity and expiry [ManagedAccount](ManagedAccount.md) defines.
- **The client namespace** `account.codex.*` or `account.grok.*`.

Callers keep the same responsibilities as [ManagedAccount](ManagedAccount.md): the model owner chooses the provider by its record, the client owns login UX, no caller reads the record.

| Requirement (callers) | Compose from [ManagedAccount](ManagedAccount.md)'s operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Sign in, cancel, sign out | No: the OAuth attempt and record are the owner's | Client namespace `account.codex.*` / `account.grok.*` |
| Read account status for the provider list | No: the sign-in state is the owner's | `availability` |
| Run one request with the account's credential, recovering once on a rejection | No: the grant and the lease table are the owner's | `with_lease` |
| Project the plan's models | No: the plan and the grant are the owner's | `catalog` |

## Caller example

The model owner runs one Codex request inside the owner's operation:

```rust
codex_account
    .with_lease(
        LeaseRequest { provider: "openai-codex".into(), model: model.clone() },
        attempt, // one exchange with the lease; rejected on the provider's 401
        cancelled,
    )
    .await?;
```

The client reads the provider list's account state through the client namespace, not the face:

```text
account.codex.status {} -> { authenticated, identity? }
```

## Own contract

### Faces

| Owner | Face | Provider id | Login | Namespace |
| --- | --- | --- | --- | --- |
| OpenAI Codex | `codexAccount` | `openai-codex` | browser PKCE, callback at `http://localhost:1455/auth/callback` | `account.codex.*` |
| Grok | `grokAccount` | `grok-build` | device | `account.grok.*` |

Both faces implement [ManagedAccount](ManagedAccount.md)'s `availability`, `subscribe`, `catalog` and `with_lease`. The account record keeps its existing file, revision and `credentialEpoch`/`generation` fields; the catalog keeps the plan's models and their declared capabilities.

### Errors

[ManagedAccount](ManagedAccount.md)'s kinds. A failed login attempt reports only `Login failed`; a rejected refresh signs out only the grant that holds the lock.

### Flows

[ManagedAccount](ManagedAccount.md)'s flows. Codex additionally opens the browser and receives the localhost callback; Grok polls the device authorization. A completed token rotation remains the owner's work and settles before unload.

## Exceptions with reasons

- `Lease` carries the bearer to the caller's attempt for one request; [ManagedAccount](ManagedAccount.md) states the rule.
- The legacy `openai-codex` record keeps its legacy fields; the hidden state shared with the ChatGPT account and the migration between the two accounts stay the owners' business and are described in the [Codex sign-in design](../../../crates/dim-oauth/docs/design.md).

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Two process parts, one implementation; no Timeline facts, no Capability. |
| §2 scopes | Process: grants and leases are shared across Sessions. |
| §3 orthogonality | No other owner's state is read. |
| §4 rules 1–6, 9 | No events, handoffs, callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use the faces; none reads the record or the attempt table. |
| §4 rule 8 | No infrastructure change. |
| §7 ids, values, callbacks | The lease is a value with an id; availability is a read-only view. |
| §7 no proxy | Each owner returns its own account data. |
| §7 one change | The delivery part deletes the forwarding shell and the object port together. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Depends on FileStorage; the model owner depends on the faces. |
| ADR 002 D4 | The issuer and client configuration are structural; the record is the owner's business state. |
| ADR 002 D5 | The faces are language-neutral. |

## Requirements raised to mechanism owners

- **FileStorage ([#2080](https://github.com/arcships/dim/pull/2080)).** `read` and `update` for the account record.
- **Others.** None.

## Binding appendix

```rust
impl PublicService for OpenAICodexOAuthService {}

#[rutis::service(name = "codexAccount")]
impl OpenAICodexOAuthService {
    pub async fn availability(&self, cancelled: CancelledFn) -> Result<AccountAvailability, AccountError> { /* ... */ }
    pub async fn subscribe(&self, cancelled: CancelledFn) -> Result<Subscription, AccountError> { /* ... */ }
    pub async fn catalog(&self, cancelled: CancelledFn) -> Result<AccountCatalog, AccountError> { /* ... */ }
    pub async fn with_lease(&self, request: LeaseRequest, attempt: AttemptFn, cancelled: CancelledFn) -> Result<WithLeaseReply, AccountError> { /* ... */ }
}

impl PublicService for GrokOAuthService {}

#[rutis::service(name = "grokAccount")]
impl GrokOAuthService { /* the same operations */ }
```
