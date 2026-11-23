# Managed OAuth accounts (OpenAI Codex, Grok)

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

This document covers two owners that share one implementation: **OpenAI Codex** and **Grok**. The account operations, errors and flows are [ManagedAccount](ManagedAccount.md)'s; the two owners differ in provider id, issuer, login method and record namespace.

## Use and scope: responsibility split

Each owner holds:

- **The account record** for its provider under the product home, written through FileStorage's `update`.
- **Login.** Browser PKCE for Codex, device flow for Grok, the pending attempt and its deadline.
- **Tokens.** Refresh, rotation and the one-controlled-recovery rule.
- **The account's model availability.** The models the subscription serves, as the account catalog.
- **Credentials.** The connection facts for one request and the grant generation [ManagedAccount](ManagedAccount.md) defines.
- **The client namespace** `account.codex.*` or `account.grok.*`.

Callers keep the same responsibilities as [ManagedAccount](ManagedAccount.md): the model owner chooses the provider by its record, the client owns login UX, no caller reads the record.

| Requirement (callers) | Compose from [ManagedAccount](ManagedAccount.md)'s operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Sign in, cancel, sign out | No: the OAuth attempt and record are the owner's | Client namespace `account.codex.*` / `account.grok.*` |
| Read account status for the provider list | No: the sign-in state is the owner's | `availability` |
| Get the credential for a request, and the decision after a rejection | No: the grant, its refresh window and its generation are the owner's | `credential`, `rejected` |
| Project the plan's models | No: the plan and the grant are the owner's | `catalog` |

## Caller example

The model owner runs one Codex request inside the owner's operation:

```rust
codex_account
    .credential(CredentialRequest { provider: "openai-codex".into(), model: model.clone() }, cancelled.clone())
    .await?;
// Send; on a credential rejection call `rejected` with the generation and send once more.
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

Both faces implement [ManagedAccount](ManagedAccount.md)'s `availability`, `subscribe`, `catalog`, `credential` and `rejected`. The account record keeps its existing file, revision and `credentialEpoch`/`generation` fields; the catalog keeps the plan's models and their declared capabilities.

### Errors

[ManagedAccount](ManagedAccount.md)'s kinds. A failed login attempt reports only `Login failed`; a rejected refresh signs out only the grant that holds the lock.

### Flows

[ManagedAccount](ManagedAccount.md)'s flows. Codex additionally opens the browser and receives the localhost callback; Grok polls the device authorization. A completed token rotation remains the owner's work and settles before unload.

## Exceptions with reasons

- The bearer's boundary is [ManagedAccount](ManagedAccount.md#the-bearers-boundary); these replies and errors omit it.
- The legacy `openai-codex` record keeps its legacy fields; the hidden state shared with the ChatGPT account and the migration between the two accounts stay the owners' business and are described in the [Codex sign-in design](../../../crates/dim-oauth/docs/design.md).

## Client-visible changes

None. The `account.codex.*` and `account.grok.*` method names, fields, error codes and user-visible behavior are unchanged; handlers call these operations.

## Rule check

[Design rules §7](https://github.com/arcships/dim/pull/2029) is the rule source.

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Two process parts, one implementation; no Timeline facts, no Capability. |
| §2 scopes | Process: grants and generations are shared across Sessions. |
| §3 orthogonality | No other owner's state is read. |
| §4 rules 1–6, 9 | No events, handoffs, callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use the faces; none reads the record or the attempt table. |
| §4 rule 8 | No infrastructure change. |
| §7 ids, values, callbacks | The credential is a value with a generation; availability is a read-only view. |
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

## Supersedes

- `service-faces.md`: the `grokOAuthService` and `openAICodexOAuthService` rows.

## Binding appendix

```rust
impl PublicService for OpenAICodexOAuthService {}

#[rutis::service(name = "codexAccount")]
impl OpenAICodexOAuthService {
    pub async fn availability(&self, cancelled: CancelledFn) -> Result<AccountAvailability, AccountError> { /* ... */ }
    pub async fn subscribe(&self, cancelled: CancelledFn) -> Result<Subscription, AccountError> { /* ... */ }
    pub async fn catalog(&self, cancelled: CancelledFn) -> Result<AccountCatalog, AccountError> { /* ... */ }
    pub async fn credential(&self, request: CredentialRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
    pub async fn rejected(&self, request: RejectedRequest, cancelled: CancelledFn) -> Result<Credential, AccountError> { /* ... */ }
}

impl PublicService for GrokOAuthService {}

#[rutis::service(name = "grokAccount")]
impl GrokOAuthService { /* the same operations */ }
```
