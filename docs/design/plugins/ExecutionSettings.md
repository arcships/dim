# ExecutionSettings

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Decision: [ADR 011](../../adr/011-execution-settings.md).

ExecutionSettings owns the `execution` document and the process execution switches it carries. The plugin also registers the `settings` tool ([AgentSettings](AgentSettings.md)) and the execution snapshot branch ([EnvironmentSettings](EnvironmentSettings.md)); those documents own the tool and the snapshot contract. This document is the owner's target contract; it replaces the `executionSettingsHost` name with `executionSettings`.

## Use and scope: responsibility split

ExecutionSettings owns:

- The `execution` source: its file, its schema, and the two switches `sandbox` and `network`, both defaulting to `false` and declared as user-only members. This contract changes their authority: today the source declares no user-only member (`crates/dim-app-server/src/execution_settings.rs`), so an owner or agent write could change the switches; under this contract such a write is `user_only`, and the delivery lists the change.
- The value `{sandbox, network}`: the switches in force now, read at each use.

Callers keep:

- When the switches take effect: at process preparation; a running process, including the persistent shell, keeps the boundary it started with.
- What a switch means for a command: the sandbox and network properties of a spawn are HostPlatform's business.
- When to show the value to the model: the snapshot branch and the tool are their own designs.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A process part reads the boundary at process preparation | No: the file, its schema and the user-only rule belong to this owner | `read` |
| The model-facing tool lists and reads the switches | Yes: the tool calls the same read | AgentSettings |
| The branch shows a snapshot on open and on a change | Yes: the branch calls the same read and follows the fact | EnvironmentSettings |
| A user changes the switches | Yes: the user edits the source through [Settings](Settings.md) and [SettingsMethods](SettingsMethods.md) | Business |

## Caller example

HostPlatform reads the switches when it prepares a process.

```rust
let switches = execution_settings.read(cancelled.clone()).await.map_err(|error| {
    HostError::failed(format!("execution settings: {error}"))
})?;
// A missing or unreadable source fails preparation; it never runs unsandboxed.
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `execution` source and `read` for its value.
- The Rust port, for consumers outside the App Server: the trait `ExecutionSettingsRead` and the reply type `Switches` live in `dim-agent-capabilities` ([`execution_settings`](../../../crates/dim-agent-capabilities/src/execution_settings.rs)). HostPlatform and the `settings` tool depend on the trait; the App Server's `ExecutionSettings` implements it and serves the same read on the wire.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `read` | The switches in force now | `{}` | `{sandbox, network}` | The caller's token; nothing changes | `failed` |

`failed` covers a read that could not produce the document: an unreadable source, a cancelled read, or an absent registration. The reply is the document itself, so no member is invented.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Fail process preparation; never prepare a process without knowing the boundary |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `read` | Read the source through [Settings](Settings.md), return the two switches | The call ends `failed` | The call ends `failed`; the caller fails preparation | Settings |

## Exceptions with reasons

- A missing value fails preparation instead of defaulting to unsandboxed: the built-in default is already "off", and silently running unsandboxed is the one failure the feature exists to prevent.

## Supersedes

- [ADR 011](../../adr/011-execution-settings.md) D1's type `ExecutionSettingsHost` and its service name `executionSettingsHost`: the decision is unchanged. The type is replaced by the owner type `ExecutionSettings`, which implements the port trait `ExecutionSettingsRead`; the wire service is `executionSettings`.
- [Service faces](../service-faces.md) §3.2's and §4.10's `executionSettingsHost` entries.

## Rule check

Rules: [§7](https://github.com/arcships/dim/pull/2029), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | The process part owns the document. The snapshot branch writes one input at branch open and on a change, so that part is a Capability ([EnvironmentSettings](EnvironmentSettings.md)); the tool part is a Session tool registration, not a Capability. |
| §2 scopes | The document and the value are process state; the branch part reads the value and appends an input to its branch. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | The snapshot is an input message, not a command event. |
| §4 rule 6 | The value reaches the model as an appended input; no Prompt source and no second context path. |
| §4 rule 7 | HostPlatform calls the read; no caller names the file or the schema. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | Replies are values; no object or path crosses. |
| §7 no proxy | The switches belong to this owner; consumers ask this owner. |
| §7 one face | One service with one read; the `executionSettingsHost` name is deleted. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | Nothing of the source, its file or the user-only rule reaches the caller. |
| §7 one change | The name change and its callers migrate in the same change. |
| ADR 002 D1 | Process scope: the switch applies to the process, not a Session. |
| ADR 002 D3 | HostPlatform depends on the port; the port has no Settings or file dependency. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| ADR 027 | Not applicable: the consumers are process or Session parts reading a process value. |

## Requirements raised to mechanism owners

None new. HostPlatform's read at process preparation is unchanged in shape; only the service name changes.

## Binding appendix

```rust
use dim_agent_capabilities::host::{CancelledFn, HostFuture};
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switches {
    pub sandbox: bool,
    pub network: bool,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ExecutionSettingsError {
    #[error("{message}")]
    Failed { message: String },
}

// The Rust port, in dim-agent-capabilities; the App Server's ExecutionSettings implements it.
pub trait ExecutionSettingsRead: Send + Sync + 'static {
    fn read(&self, cancelled: CancelledFn) -> HostFuture<'_, Result<Switches, ExecutionSettingsError>>;
}

impl PublicService for ExecutionSettings {}

#[rutis::service(name = "executionSettings")]
impl ExecutionSettings {
    pub async fn read(&self, cancelled: CancelledFn) -> Result<Switches, ExecutionSettingsError> { /* ... */ }
}
```
