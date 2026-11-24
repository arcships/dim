# PromptSettings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature design: [Prompt overrides](../appserver/prompt-overrides.md).

PromptSettings owns the `prompts` document and the three model-facing texts it carries: the base identity and rules, the compaction summary prompt, and the Skills instructions. This document is the owner's target contract; it replaces the three `*PromptSettingsHost` services with one service.

## Use and scope: responsibility split

PromptSettings owns:

- The `prompts` source: its file, its schema, and the rule that each of the three texts is non-empty and changed only by the user.
- The three values `base`, `summary` and `skills`: the text in force, or none when the user set no override.

Callers keep:

- The precedence: a factory configuration outranks the user's text, which outranks the built-in text. The consumer applies the precedence and keeps the built-in text on a missing value or a failed read.
- When the text is adopted: at the Session part's `apply`, fixed for that Session; a change applies to Sessions loaded after it.
- The text's meaning and how it is composed into the prompt.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A capability reads its own override text before a Session starts | No: the document belongs to this owner, and the consumer must not name another text's member | One read per text on the owner's service |
| A capability falls back when the user set nothing | Yes: the read answers none | Business |
| A user edits a text | Yes: the client edits the source through [Settings](Settings.md) and [SettingsMethods](SettingsMethods.md) | Business |
| A capability reacts to a changed text mid-Session | Yes: the value is fixed at `apply`; a changed text applies to the next Session | Business |

## Caller example

PromptSources reads the base text once, while its Session part applies.

```rust
let base = prompt_settings
    .base(cancelled.child_token())
    .await
    .ok()
    .and_then(|view| view.text);
let text = factory.base.or(base).unwrap_or_else(|| BUILT_IN.into());
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `prompts` source and `read` for its value. PromptSettings registers the source from its own process row and keeps the source id.
- The Rust port, for consumers outside the App Server: the three traits `BasePromptSettings`, `SkillsInstructionsSettings` and `SummaryPromptSettings` stay in `dim-agent-capabilities` ([`prompt_sources`](../../../crates/dim-agent-capabilities/src/prompt_sources.rs), [`skills`](../../../crates/dim-agent-capabilities/src/skills/mod.rs), [`summary_strategy`](../../../crates/dim-agent-capabilities/src/summary_strategy.rs)), each covering one text. The App Server's `PromptSettings` implements all three and serves the same reads on the wire; a consumer in `dim-agent-capabilities` depends on the trait, never on the App Server.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `base` | The user's replacement of `dim/base`, when set | `{}` | `{text}` or none | The caller's token; nothing changes | `failed` |
| `summary` | The user's replacement of the compaction summary prompt, when set | `{}` | `{text}` or none | The caller's token; nothing changes | `failed` |
| `skills` | The user's replacement of the Skills instructions, when set | `{}` | `{text}` or none | The caller's token; nothing changes | `failed` |

A read returns the member as stored after the schema check; an absent member is none, never an empty string.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Use the factory configuration or the built-in text; the Session opens. The message names the source, never the text. |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `base`, `summary`, `skills` | Read the source through [Settings](Settings.md), return the member or none | Nothing changes | The call ends `failed`; the consumer keeps its built-in text | Settings |

## Exceptions with reasons

- The three texts share one document and one registering row: a file has one owner, while each text belongs to a different capability; the schema keeps each member independent, and a user-only member cannot be changed by the owner.
- A read failure does not fail the Session: the text is policy with a safe default, and the consumer keeps the built-in text.

## Supersedes

- [Prompt overrides](../appserver/prompt-overrides.md) §3 and §3.1 (the three ports), §7 (the three-host binding appendix) and §4's `settings.sources` sentence: one `promptSettings` service and `{text}`, with each capability depending on its own port trait. The precedence rules above and the rest of §4 stay in force.

## Rule check

Rules: [§7](https://github.com/arcships/dim/blob/2fc4f50b47529e17d51e92b1d7441734771226fc/docs/architecture/rust-agent/principles.md#7-所有者与操作), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part that owns a document. It publishes facts through Settings and folds no state: not a Capability. |
| §2 scopes | Process: the document is shared by every Session; the values are read at a Session part's `apply`. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | Settings publishes the `settings/prompts` fact; this owner writes none. |
| §4 rule 6 | The text reaches the model through the existing adopted prompt sources; no context path is added. |
| §4 rule 7 | Consumers call the three reads; no consumer names the other members. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | Replies are values; no object or path crosses. |
| §7 no proxy | The texts belong to this owner; the consumers ask this owner, not the store. |
| §7 one face | One service with three reads, one per text; the three `*SettingsHost` names are deleted. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | Nothing of the store, the schema or the file name reaches the consumer. |
| §7 one change | The one service replaces the three names, and all three consumers migrate in the same change. |
| ADR 002 D1 | Process scope: the setting is shared configuration, not Session state. |
| ADR 002 D3 | The consumers' crates depend on the port traits, not on the App Server. |
| ADR 027 | Not applicable: the consumers are process or Session parts reading a process value. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None new. The consumers ([PromptSources](../../architecture/rust-agent/plugins/PromptSources.md), Skills and ContextCompaction) call one service name instead of three; each consumer's own contract records the fallback rule.

## Binding appendix

The three reads are one service on the owner type; the old `BasePromptSettingsHost`, `SkillsInstructionsSettingsHost` and `SummaryPromptSettingsHost` types and their traits are deleted.

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptText {
    pub text: Option<String>,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PromptSettingsError {
    #[error("{message}")]
    Failed { message: String },
}

impl PublicService for PromptSettings {}

#[rutis::service(name = "promptSettings")]
impl PromptSettings {
    pub async fn base(&self, cancelled: CancelledFn) -> Result<PromptText, PromptSettingsError> { /* ... */ }
    pub async fn summary(&self, cancelled: CancelledFn) -> Result<PromptText, PromptSettingsError> { /* ... */ }
    pub async fn skills(&self, cancelled: CancelledFn) -> Result<PromptText, PromptSettingsError> { /* ... */ }
}
```
