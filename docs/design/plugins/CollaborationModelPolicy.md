# CollaborationModelPolicy

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature: [Subagent model policy](../subagent-model-policy.md).

CollaborationModelPolicy owns the `collaboration` document: which model a new Collaboration member runs. Collaboration asks this owner for the policy each time it creates a member. This document is the owner's target contract; it replaces the store read with the policy's own value read.

## Use and scope: responsibility split

CollaborationModelPolicy owns:

- The `collaboration` source: its file, its document type and its validation — a `fixed` policy needs both a provider id and a model id.
- The value `policy`: `agent` (the profile's model, or the existing default), `inherit` (the requesting branch's adopted model) or `fixed {model}`.
- The `collaboration` management namespace: `collaboration.model_policy` and `collaboration.set_model_policy` for a local client.

Callers keep:

- What each policy means when a member starts: Collaboration's business, including the startup override that always inherits.
- When the policy is read: at each member creation.
- What an absent policy means: `agent`, as if the user had chosen it.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Collaboration needs the policy for a new member | No: the file, the type and its validation belong to this owner | `policy` |
| Collaboration uses `agent` when nothing is stored | Yes: the read answers the default document | Business |
| A local client reads the policy with a draft base | No: the client reaches this owner's namespace only | `collaboration.model_policy` |
| A local client replaces the policy at a revision | No: a stale draft is reported | `collaboration.set_model_policy` |
| A consumer reacts to a changed policy | Yes: follow the `settings/collaboration` fact and read again | Business |

## Caller example

Collaboration reads the policy when it creates a member.

```rust
let policy = if startup {
    ModelPolicy::Inherit
} else {
    collaboration_policy.policy(cancelled.clone()).await?.policy
};
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `collaboration` source and `read`/`change` for its value.
- `managementMethods`: the two client methods.
- Collaboration: the App Server's `Collaboration` calls `policy` directly in-process; it is in the same crate as this owner, so no port type is needed. The policy type stays in this owner's module.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `policy` | The policy in force now | `{}` | `{policy}` | The caller's token; nothing changes | `failed`, `cancelled` |
| `collaboration.model_policy` | The policy with the revision a draft starts from | `{}` | `{revision, policy}` | The call is dropped | `collaboration_model_policy_failed`, `operation_interrupted`, reloading |
| `collaboration.set_model_policy` | Replace the policy at the revision read | `{expectedRevision, policy}` | `{revision}` | The call is dropped | invalid params, `collaboration_model_policy_conflict`, `collaboration_model_policy_failed`, `operation_interrupted`, reloading |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Use the default policy (`agent`) for this member; the member still starts |
| `cancelled` | The member creation is abandoned with the call |
| invalid params | Fix the request: an unknown mode, or a `fixed` policy without a provider and a model |
| `collaboration_model_policy_conflict` | Read again; the user's draft is stale |
| `collaboration_model_policy_failed` | Read again before retrying |
| `operation_interrupted` | The call was cancelled or the owner is unloading; read again before retrying |
| reloading | The daemon is restarting; retry after it is back |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `policy` | Read the source through [Settings](Settings.md), return the policy | The call ends `cancelled` | The call ends `failed`; the member uses the default policy | Settings |
| `collaboration.model_policy`, `collaboration.set_model_policy` | [Settings](Settings.md) `read_user`/`replace_user`, map the reply and the errors | The call is dropped; a write before the commit does not happen | In-flight calls finish or end `reloading` | Settings |

## Exceptions with reasons

- The policy document is a Rust type rather than a JSON Schema: the closed mode set with a typed model is the owner's validation, so the client reaches it through this owner's namespace.

## Supersedes

- [Subagent model policy](../subagent-model-policy.md)'s row that persists and edits the policy through `SettingsService::read/update`; the `collaboration.*` management methods keep their shapes.

## Rule check

Rules: [§7](https://github.com/arcships/dim/blob/2fc4f50b47529e17d51e92b1d7441734771226fc/docs/architecture/rust-agent/principles.md#7-所有者与操作), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part owning a document; not a Capability. |
| §2 scopes | Process: the policy applies to every new member of the process. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | Settings publishes the `settings/collaboration` fact; this owner writes none. |
| §4 rule 6 | The policy never enters model context. |
| §4 rule 7 | Collaboration calls `policy`; the client calls the namespace; no caller names the file or the type. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | Replies are values; no object or path crosses. |
| §7 no proxy | The policy belongs to this owner; Collaboration asks this owner, not the store. |
| §7 one face | One service value read and one namespace; no second read path. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | The file, the type and the validation stay with this owner. |
| §7 one change | The new value read, Collaboration's migration and the store read's deletion are one change. |
| ADR 002 D1 | Process scope. |
| ADR 002 D3 | Collaboration depends on this owner's value; this owner depends on Settings. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |
| ADR 027 | Not applicable: the consumer is a process part reading a process value. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::ModelId;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum ModelPolicy {
    Agent {},
    Inherit {},
    Fixed { model: ModelId },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub policy: ModelPolicy,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CollaborationPolicyError {
    #[error("{message}")]
    Failed { message: String },
    #[error("{message}")]
    Cancelled { message: String },
}

impl PublicService for CollaborationModelPolicy {}

#[rutis::service(name = "collaborationPolicy")]
impl CollaborationModelPolicy {
    pub async fn policy(&self, cancelled: CancelledFn) -> Result<Policy, CollaborationPolicyError> { /* ... */ }
}
```
