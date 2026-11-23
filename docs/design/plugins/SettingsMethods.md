# SettingsMethods

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017).

SettingsMethods registers the local client's `settings.*` management methods and calls [Settings](Settings.md)' user operations. This document is the owner's target contract; the client surfaces are in [Settings service](../appserver/settings-service.md) §4.3 and §7.

## Use and scope: responsibility split

SettingsMethods owns:

- The `settings` management namespace: which methods exist, their request and reply data, and the mapping from Settings' errors to the client's error codes.
- The local-only rule: the methods are never served to paired devices or a remote entry.

Callers keep:

- The user's draft, its editing flow and what a stale draft means in the client.
- Which source the client edits; the client names sources it knows, and it never learns another owner's document.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A local client reads a document to edit it | No: the client reaches management methods only, and the draft base is [Settings](Settings.md) `read_user` | `settings.read` |
| A local client writes the draft it edited | No: the write must carry the draft's base revision, the client's business concurrency control between the user's read and write, and report a stale draft | `settings.update` |
| A client discovers every declared source and its schema | Yes, and no client asks: the CLI reads four sources by name | Not an operation; `settings.sources` is deleted |
| A client lists authorized projects to pass a project scope | Yes, and no client asks: no declared source declares a project file | Not an operation; `settings.projects` is deleted |

## Caller example

The CLI edits the operator's prompts with the revision it read. The revision is the draft's base, a business value the client holds; the daemon compares it and never exposes the owner's internal version.

```rust
let snapshot: PromptSetting = client
    .typed("settings.read", json!({"source": SOURCE}))
    .await?;
// The user edits one field; the write is the whole document at the revision read.
client
    .typed(
        "settings.update",
        json!({
            "source": SOURCE,
            "expectedRevision": snapshot.revision,
            "document": document,
        }),
    )
    .await?;
```

## Own contract

### Dependencies

- [Settings](Settings.md): `read_user` and `replace_user`. SettingsMethods adds no state and forwards nothing else.

### Operations

No service. Two management methods, registered with `managementMethods`:

| Method | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `settings.read` | The user's draft base for one document | `{source}` | `{revision, document}` | The call is dropped | invalid params, `settings_unknown_source`, `settings_failed`, reloading |
| `settings.update` | Replace a document with the draft at the revision read | `{source, expectedRevision, document}` | `{revision}` | The call is dropped | invalid params, `settings_unknown_source`, `settings_conflict`, `settings_unsynced`, `settings_failed`, reloading |

`settings_conflict` means the draft is stale: the client reads again and lets the user decide. `settings_unsynced` means the bytes are in place with unconfirmed durability: read again instead of repeating the write.

### Errors

| Kind | What the caller does next |
| --- | --- |
| invalid params | Fix the request: an unknown field, a missing revision, or a document that is not an object |
| `settings_unknown_source` | The source is not registered now; report that the feature is off and retry after its owner registers it |
| `settings_conflict` | Read again; the user's draft is stale |
| `settings_unsynced` | Read again; the write is in place |
| `settings_failed` | Read again before retrying; the store refused or failed without a code the client can act on |
| reloading | The daemon is restarting; retry after it is back |

### Flows

| Operation | Normal path | Caller cancels | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- |
| `settings.read` | Call [Settings](Settings.md) `read_user`, map the reply | Nothing changes | Pending calls end `reloading` | Settings |
| `settings.update` | Call [Settings](Settings.md) `replace_user`, map the reply | Before Settings writes: nothing changes, `settings_failed` or `reloading` | In-flight calls finish or end `reloading` | Settings |

## Exceptions with reasons

- `managementMethods` is a mechanism registration point, not a forwarding facade: the namespace belongs to this part because Rpc reaches Settings through Agents and may not reach a management owner ([ADR 004](../../adr/004-loader-plugin-management.md) D4), and the mapping from Settings' kinds to the client's codes is this part's own contract.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part, no Timeline facts, no folded state: not a Capability. |
| §2 scopes | Process; the methods are client surfaces, not Session state. |
| §3 orthogonality | No state of its own; one dependency, [Settings](Settings.md). |
| §4 rule 1 | No event. |
| §4 rule 7 | Clients use management methods; they learn no file name, path or lock. |
| §4 rule 8 | No infrastructure change; the existing `managementMethods` mechanism is used. |
| §7 objects | Request and reply are data; no object, handle or path crosses. |
| §7 no proxy | Settings answers the document; SettingsMethods maps the errors and owns the namespace. |
| §7 one face | The management namespace exists once; the service operations stay on [Settings](Settings.md). |
| §7 cross-language | Both methods are data. |
| §7 caller complexity | The revision reaches the client because the user's draft depends on it; nothing else of Settings' mechanism does. |
| §7 one change | `settings.sources` and `settings.projects` are deleted with this change; the CLI is the only caller migrated. |
| ADR 002 D2 | No wiring plugin: the namespace owner is a process part that depends on Settings, as PermissionRules owns `permission`. |
| ADR 002 D4 | Business configuration stays in Settings sources; the methods carry no structural item. |

## Requirements raised to mechanism owners

None.

## Binding appendix

No service and no Rust binding: the two methods are registered with `managementMethods`. The request and reply types are the client's JSON shapes in the operations table.
