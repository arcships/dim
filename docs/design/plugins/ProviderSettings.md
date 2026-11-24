# ProviderSettings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature: [Provider management](../appserver/provider-management.md) §2.

ProviderSettings owns the `provider` document: the User-Agent dim puts on its model requests when the operator replaces the built-in one. The document holds no Provider record; a record's own `headers` stay that record's, and a User-Agent there still wins for that Provider alone. This document is the owner's target contract; it replaces the push into the process identity with a value read.

## Use and scope: responsibility split

ProviderSettings owns:

- The `provider` source: its file, its schema, the `userAgent` member and the rule that only the user changes it.
- The value `userAgent`: the operator's replacement, or none when unset. The environment variable `DIM_USER_AGENT` still wins when set.

Callers keep:

- Which request carries which User-Agent: Provider's model request, including a record's own `headers`.
- When the value is read: at request time, or after following the `settings/provider` fact.
- Failing or not failing a request on a read error: a read error keeps the built-in User-Agent.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A model request needs the operator's User-Agent | No: the file and the user-only rule belong to this owner | `user_agent` |
| A model request keeps the built-in User-Agent when unset | Yes: the read answers none | Business |
| A user replaces the User-Agent | Yes: the user edits the source through [Settings](Settings.md) and [SettingsMethods](SettingsMethods.md) | Business |
| The request path learns of a change | Yes: follow the `settings/provider` fact and read again | Business |

## Caller example

Provider reads the value when it builds a request.

```rust
let configured = provider_settings
    .user_agent()
    .await
    .ok()
    .and_then(|view| view.user_agent);
let user_agent = configured.unwrap_or_else(|| dim_user_agent());
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `provider` source and `read` for its value.
- The Rust port, for the consumer outside the App Server: the trait `ProviderUserAgent` lives in `dim-agent-capabilities` where the model request is built, and the App Server's `ProviderSettings` implements it and serves the same read on the wire.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `user_agent` | The operator's User-Agent replacement, when set | `{}` | `{userAgent}` or none | The caller's token; nothing changes | `failed` |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Use the built-in User-Agent; never fail the model request for this setting |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `user_agent` | Read the source through [Settings](Settings.md), return the member or none | Nothing changes; the caller uses the built-in value | The call ends `failed`; the caller uses the built-in value | Settings |

## Exceptions with reasons

- The value is read per request instead of being published into the process identity: the identity belongs to Provider, and a value read keeps the write inside its owner.

## Supersedes

- [Provider management](../appserver/provider-management.md) §2's sentences that say the Provider part holds the value and pushes it into the model identity.

## Rule check

Rules: [§7](https://github.com/arcships/dim/pull/2029), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part owning a document; not a Capability. |
| §2 scopes | Process: the value applies to every request of this process. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | Settings publishes the `settings/provider` fact; this owner writes none. |
| §4 rule 6 | The value never enters model context; it is a request header. |
| §4 rule 7 | Provider calls the read; no caller names the file or the schema. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | The reply is a value; no object or path crosses. |
| §7 no proxy | The value belongs to this owner; Provider asks this owner, not the store. |
| §7 one face | One service with one read; the old registration-and-push shape is deleted. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | The file, the user-only rule and the fallback ordering stay with their owners; the caller sees a value or none. |
| §7 one change | The new read, Provider's migration and the deletion of the identity push are one change. |
| ADR 002 D1 | Process scope. |
| ADR 002 D3 | Provider depends on the value; ProviderSettings depends only on Settings. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| ADR 027 | Not applicable: the consumer is a process part reading a process value. |

## Requirements raised to mechanism owners

- **Provider, [#2019](https://github.com/arcships/dim/issues/2019):** the model request reads this value instead of receiving a pushed process identity; the request keeps its own `headers` precedence and its built-in fallback.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserAgent {
    pub user_agent: Option<String>,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ProviderSettingsError {
    #[error("{message}")]
    Failed { message: String },
}

impl PublicService for ProviderSettings {}

#[rutis::service(name = "providerSettings")]
impl ProviderSettings {
    pub async fn user_agent(&self, cancelled: CancelledFn) -> Result<UserAgent, ProviderSettingsError> { /* ... */ }
}
```
