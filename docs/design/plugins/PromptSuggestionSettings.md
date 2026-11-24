# PromptSuggestionSettings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature: [PromptSuggestion](../../architecture/rust-agent/plugins/PromptSuggestion.md).

PromptSuggestionSettings owns the `prompt-suggestion` document and the value the Session part reads at each decision. This document is the owner's target contract; it replaces the `promptSuggestionSettingsHost` name with `promptSuggestionSettings`.

## Use and scope: responsibility split

PromptSuggestionSettings owns:

- The `prompt-suggestion` source: its file, its schema, the switch `enabled` and the length limit `maxChars` in `[10, 200]`, defaulting to `false` and `80`.
- The value `{enabled, maxChars}` as stored now.

Callers keep:

- When the feature offers a suggestion and how the reply is validated.
- That a missing port or a failed read turns the feature off.
- The text the model sees; the limit bounds the reply, not the instruction.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The Session part reads the switch and the limit at each decision | No: the file, the schema and the range belong to this owner | `read` |
| The Session part treats an absent port as "off" | Yes: the read is optional at assembly; a failed read is its own error | Business |
| A user changes the switch or the limit | Yes: the user edits the source through [Settings](Settings.md) and [SettingsMethods](SettingsMethods.md) | Business |

## Caller example

PromptSuggestion reads the value before it decides.

```rust
let settings = prompt_suggestion_settings.read(token.clone()).await?;
if !settings.enabled {
    return Ok(Decision::Refused("disabled"));
}
let max_chars = settings.max_chars; // the owner's schema already bounded it
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `prompt-suggestion` source and `read` for its value.
- The Rust port, for consumers outside the App Server: the trait `PromptSuggestionSettingsRead` and the reply type `SuggestionSettings` live in `dim-agent-capabilities` ([`prompt_suggestion`](../../../crates/dim-agent-capabilities/src/prompt_suggestion.rs)). The PromptSuggestion Session part depends on the trait; the App Server's `PromptSuggestionSettings` implements it and serves the same read on the wire.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `read` | The switch and the limit in force now | `{}` | `{enabled, maxChars}` | The caller's token; nothing changes | `failed` |

The reply carries both members because the schema gives both a default; a document that misses one is invalid, not a value with a missing field.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Turn the feature off for this decision; the message names the source |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `read` | Read the source through [Settings](Settings.md), return both members | The call ends `failed`; the feature stays off for this decision | The call ends `failed`; the feature stays off | Settings |

## Exceptions with reasons

- The assembler may leave the port absent: an evaluation or a host without Settings then keeps the feature off, so the read is optional at assembly.

## Supersedes

- [Service faces](../service-faces.md) §3.2's and §4.10's `promptSuggestionSettingsHost` entries.

## Rule check

Rules: [§7](https://github.com/arcships/dim/blob/2fc4f50b47529e17d51e92b1d7441734771226fc/docs/architecture/rust-agent/principles.md#7-所有者与操作), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part owning a document; the Session part that reads it is a Capability of the PromptSuggestion feature, designed in that plugin's page. |
| §2 scopes | The document and the value are process state; the Session part reads the value. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | No event from this owner. |
| §4 rule 6 | The value reaches the model through the feature's own request; no context path is added. |
| §4 rule 7 | The Session part calls the read; no caller names the file or the schema. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | Request and reply are data; no object or path crosses. |
| §7 no proxy | The value belongs to this owner. |
| §7 one face | One service with one read; `promptSuggestionSettingsHost` is deleted. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | The range and the defaults stay in this owner's schema; the consumer does not clamp or fill them. |
| §7 one change | The name change and its caller migrate in the same change. |
| ADR 002 D1 | Process scope. |
| ADR 002 D3 | The Session part depends on the port type, not on the App Server. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| ADR 027 | Not applicable: the consumer is a Session part reading a process value. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuggestionSettings {
    pub enabled: bool,
    pub max_chars: u32,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PromptSuggestionSettingsError {
    #[error("{message}")]
    Failed { message: String },
}

impl PublicService for PromptSuggestionSettings {}

#[rutis::service(name = "promptSuggestionSettings")]
impl PromptSuggestionSettings {
    pub async fn read(&self, cancelled: CancelledFn) -> Result<SuggestionSettings, PromptSuggestionSettingsError> { /* ... */ }
}
```
