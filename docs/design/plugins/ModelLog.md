# ModelLog

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature: [Model request log](../appserver/model-log.md).

ModelLog owns the `model-log` document: whether the process records its model wire exchanges. The recorder itself is aimux's and writes the JSONL file under the home root. This document is the owner's target contract; it replaces the push into the recorder with a value read.

## Use and scope: responsibility split

ModelLog owns:

- The `model-log` source: its file, its schema and the `enabled` switch, defaulting to `false`.
- The value `recording`: the directory the recorder writes to when the switch is on, or none when off.

Callers keep:

- The recording format, the file name and the write path: aimux's recorder.
- When the value is read: at each request, or after following the `settings/model-log` fact.
- Failing or not failing a request on a read error: a read error records nothing.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The request recorder needs its destination | No: the file and the switch belong to this owner | `recording` |
| The recorder is off when unset | Yes: the read answers none | Business |
| A user turns recording on or off | Yes: the user edits the source through [Settings](Settings.md) and [SettingsMethods](SettingsMethods.md) | Business |

## Caller example

The recorder reads the destination while it starts a request.

```rust
let destination = model_log.recording().await.ok().flatten();
if let Some(directory) = destination.directory {
    // aimux owns the file under this directory.
}
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `model-log` source and `read` for its value.
- FileStorage: `root_path` for the home root, once, to build the destination directory.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `recording` | Where the request recorder writes now | `{}` | `{directory}` or none | The caller's token; nothing changes | `failed` |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Record nothing for this request; the model request itself proceeds |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `recording` | Read the source through [Settings](Settings.md), answer the directory or none | Nothing changes; the caller records nothing | The call ends `failed`; the caller records nothing | Settings |

## Exceptions with reasons

- The directory is computed from the home root at each read instead of being pushed into the recorder: the recorder belongs to aimux, and a value read keeps the write inside its owner.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part owning a document; not a Capability. |
| §2 scopes | Process: the switch applies to the whole process. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers and the home root FileStorage answers. |
| §4 rule 1 | Settings publishes the `settings/model-log` fact; this owner writes none. |
| §4 rule 6 | The switch never enters model context. |
| §4 rule 7 | The recorder calls the read; no caller names the file or the schema. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | The reply is a value; no path crosses as a resource. |
| §7 no proxy | The switch and the destination belong to this owner. |
| §7 one face | One service with one read; the old push into the process recorder is deleted. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | The file, the switch and the home root stay with their owners. |
| §7 one change | The new read, aimux's migration and the deletion of the setter are one change. |
| ADR 002 D1 | Process scope. |
| ADR 002 D3 | The recorder depends on the value; ModelLog depends on Settings and FileStorage. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Provider, [#2019](https://github.com/arcships/dim/issues/2019):** aimux's request recorder reads this value instead of receiving a process-global destination; a read error records nothing and never fails a request.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recording {
    pub directory: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelLogError {
    #[error("the model-log setting could not be read: {0}")]
    Failed(String),
}

impl PublicService for ModelLog {}

#[rutis::service(name = "modelLog")]
impl ModelLog {
    pub async fn recording(&self, cancelled: CancelledFn) -> Result<Recording, ModelLogError> { /* ... */ }
}
```
