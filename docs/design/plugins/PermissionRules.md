# PermissionRules

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017). Feature: [Permission command rules](../appserver/permission-command-rules.md).

PermissionRules owns the `permission` document: the user's command rules. The permission decision that applies the rules is the `CommandRules` capability; this owner answers the rules. This document is the owner's target contract; it replaces the `commandRules` Session service and the Session part that read the rules from the store.

## Use and scope: responsibility split

PermissionRules owns:

- The `permission` source: its file, its document type (`command_rules`) and its validation — at least one word per rule, no shell separator.
- The value `rules`: the user's command rules as stored now.
- The `permission` management namespace: `permission.rules` and `permission.set_rules` for a local client.

Callers keep:

- The permission decision: whether a command is allowed, asked or denied, and how a rule matches a command.
- When the rules are read: at each decision.
- What a conflict means for the user's edit: the draft is stale; read again.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The permission decision needs the user's rules | No: the document, its type and its validation belong to this owner | `rules` |
| A local client reads the rules with a draft base | No: the client reaches this owner's namespace only | `permission.rules` |
| A local client replaces the rules at a revision | No: the whole list is replaced and a stale draft is reported | `permission.set_rules` |
| A consumer reacts to a changed rule set | Yes: follow the `settings/permission` fact and read again | Business |

## Caller example

The permission decision reads the rules once per call.

```rust
let rules = permission_rules.rules(cancelled.clone()).await?;
let decision = decide(&rules.rules, &command);
```

The CLI replaces the list with the revision it read.

```rust
client
    .typed(
        "permission.set_rules",
        json!({"expectedRevision": revision, "commandRules": rules}),
    )
    .await?;
```

## Own contract

### Dependencies

- [Settings](Settings.md): `register` for the `permission` source and `read`/`change` for its value.
- `managementMethods`: the two client methods.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `rules` | The user's command rules as stored now | `{}` | `{rules}` | The caller's token; nothing changes | `failed`, `cancelled` |
| `permission.rules` | The rules with the revision a draft starts from | `{}` | `{revision, commandRules}` | The call is dropped | `permission_rules_failed`, reloading |
| `permission.set_rules` | Replace the whole list at the revision read | `{expectedRevision, commandRules}` | `{revision}` | The call is dropped | invalid params, `permission_rules_conflict`, `permission_rules_failed`, reloading |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `failed` | Deny or ask for the command; a rules read that failed never allows it |
| `cancelled` | The decision is abandoned with the call |
| invalid params | Fix the request: a rule without a command word, or with a shell separator |
| `permission_rules_conflict` | Read again; the user's draft is stale |
| `permission_rules_failed` | Read again before retrying |
| reloading | The daemon is restarting; retry after it is back |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `rules` | Read the source through [Settings](Settings.md), return the list | The call ends `cancelled` | The call ends `failed`; the decision denies or asks | Settings |
| `permission.rules`, `permission.set_rules` | [Settings](Settings.md) `read_user`/`replace_user`, map the reply and the errors | The call is dropped; a write before the commit does not happen | In-flight calls finish or end `reloading` | Settings |

## Exceptions with reasons

- The rules' validation is Rust code rather than a JSON Schema: a shell separator and an empty command word are expressible only as code, so the source is a Rust document and the client reaches it through this owner's namespace instead of `settings.*`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part owning a document. The `CommandRules` capability owns the decision and folds no rule state: it reads this owner's value. |
| §2 scopes | Process: the rules apply to every Session. |
| §3 orthogonality | No other owner's state beyond the Settings source it registers. |
| §4 rule 1 | Settings publishes the `settings/permission` fact; this owner writes none. |
| §4 rule 6 | The rules never enter model context. |
| §4 rule 7 | The decision calls `rules`; the client calls the namespace; no caller names the file or the type. |
| §4 rule 8 | No infrastructure change. |
| §7 objects | Replies are values; no object or path crosses. |
| §7 no proxy | The rules belong to this owner; the decision asks this owner, not the store, and the `commandRules` Session service is deleted. |
| §7 one face | One service for the value and one namespace for the client; no second read path. |
| §7 cross-language | Request, reply and error are data. |
| §7 caller complexity | The file, the type and the validation stay with this owner. |
| §7 one change | The new value read, the decision's migration and the deletion of `commandRules` are one change. |
| ADR 002 D1 | Process scope. |
| ADR 002 D3 | The decision depends on this owner's value; this owner depends on Settings. |
| ADR 002 D4 | Business configuration in the owner's named source, read at use time. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::CommandRule;
use rutis::host::BoxFuture;

// `CancelledFn` is the binding's cancellation argument, not a request field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rules {
    pub rules: Vec<CommandRule>,
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionRulesError {
    #[error("the command rules could not be read: {0}")]
    Failed(String),
    #[error("the command rules read was cancelled")]
    Cancelled,
}

impl PublicService for PermissionRules {}

#[rutis::service(name = "permissionRules")]
impl PermissionRules {
    pub async fn rules(&self, cancelled: CancelledFn) -> Result<Rules, PermissionRulesError> { /* ... */ }
}
```

The client methods are registered with `managementMethods`; their request and reply types are the JSON shapes in the operations table.
