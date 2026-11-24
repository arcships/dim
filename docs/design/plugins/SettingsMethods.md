# SettingsMethods

Scope: process. Issue: [#2017](https://github.com/arcships/dim/issues/2017).

SettingsMethods registers the local client's `settings.*` management methods and calls [Settings](Settings.md)' user operations. The namespace serves **declared sources only**: a Rust source such as `mcp` is `settings_unknown_source`, and its owner's own namespace is the only way to edit it.

## Use and scope: responsibility split

SettingsMethods owns:

- The `settings` management namespace: which methods exist, their request and reply data, and the mapping from Settings' errors to the client's error codes.
- The declared-only rule and the source-name resolution: the namespace maps the client's `source` name to the registered declared source's id, and refuses a name whose owner is a Rust document.
- Admission: paired devices are never served the namespace; the authenticated loopback Web entry is served `settings.read` and `settings.update` for its allowlisted sources (`WEB_SETTINGS_SOURCES` in `crates/dim-app-server/src/protocol.rs`: `model-log` and `execution`).

Callers keep:

- The user's draft, its editing flow and what a stale draft means in the client.
- Which declared source the client edits; the client never learns another owner's document, and a Rust source is not reachable here.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client reads a declared document to edit it | No: the client reaches management methods only, and the draft base is [Settings](Settings.md) `read_user` | `settings.read` |
| A client writes the draft it edited | No: the write must carry the draft's base revision, the client's business concurrency control between the user's read and write, and report a stale draft | `settings.update` |
| A client lists every declared source and its schema | Yes, and no client asks: the CLI reads five sources by name; the Web pages read their two allowlisted sources by name | Not an operation; `settings.sources` is deleted |
| A client lists authorized projects to pass a project scope | Yes, and no client asks: no declared source declares a project file | Not an operation; `settings.projects` is deleted |
| A client edits a Rust-typed source | Yes, through that owner's own namespace (`permission.*`, `collaboration.*`, and so on) | Business |

Every caller of the namespace: the CLI reads `prompts` (`crates/dim-cli/src/prompt_command.rs`, `crates/dim-cli/src/session_host.rs`), `prompt-suggestion` and `execution` (`session_host.rs`), `model-log` (`crates/dim-cli/src/recording.rs`) and `workflow` (`crates/dim-cli/src/management/workflow.rs`); the Web `DiagnosticsSettings` and `PermissionsSettings` pages (`web/src/features/settings/`) read `model-log` and `execution`; the Workflow TypeScript plugin (`runtime/plugins/workflow/catalog.ts`) reads the plugin-face `settings` service, which is not this namespace.

## Caller example

The CLI edits the operator's prompts with the revision it read.

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadRequest<'a> {
    source: &'a str,
    scope: Scope,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest<'a> {
    source: &'a str,
    scope: Scope,
    expected_revision: &'a str,
    document: &'a JsonObject,
}

let snapshot: SettingsDraft = client
    .typed(
        "settings.read",
        serde_json::to_value(ReadRequest { source: SOURCE, scope: Scope::User })?,
    )
    .await?;
// The user edits one field; the write is the whole document at the revision read.
client
    .typed(
        "settings.update",
        serde_json::to_value(UpdateRequest {
            source: SOURCE,
            scope: Scope::User,
            expected_revision: &snapshot.revision,
            document: &document,
        })?,
    )
    .await?;
```

The request is one typed struct, serialized at the client boundary; no request is assembled as an inline JSON literal. The reply keeps `{revision, document}`, the shape today's CLI and Web pages parse.

## Own contract

### Dependencies

- [Settings](Settings.md): `declared_sources` (no instance) to resolve the client's source name to a declared source id, then `read_user` and `replace_user` with that id. SettingsMethods adds no state of its own beyond the admitted-write task it owns.

### Operations

No service. Two management methods, registered with `managementMethods`:

| Method | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `settings.read` | The user's draft base for one declared document | `{source, scope}` | `{revision, document}` | A cancelled call has no reply and changes nothing | invalid params, `settings_unknown_source`, `settings_failed`, `operation_interrupted`, reloading |
| `settings.update` | Replace a declared document with the draft at the revision read | `{source, scope, expectedRevision, document}` | `{revision, document}` | A cancelled wait changes nothing before admission; an admitted write commits or ends `settings_unsynced` | invalid params, `settings_unknown_source`, `settings_conflict`, `settings_unsynced`, `settings_failed`, `operation_interrupted`, reloading |

Invalid params covers a malformed request, an unknown field, a missing revision, a document that is not an object, and a document the source's schema refuses; for a schema refusal the message is the validator's, and the client shows it as an input error.

`settings_conflict` means the draft is stale: the client reads again and lets the user decide. `settings_unsynced` means the bytes are in place with unconfirmed durability: read again instead of repeating the write. `operation_interrupted` means this part unloaded while the call was pending; a caller-cancelled call has no reply. The client re-reads before retrying. `reloading` means the daemon is restarting.

### Errors

| Kind | What the caller does next |
| --- | --- |
| invalid params | Fix the request or the draft: an unknown field, a missing revision, a document that is not an object, a schema violation (the message is the validator's) |
| `settings_unknown_source` | The source is not registered now, or it is a Rust source this namespace does not serve; report that the feature is off, or use that owner's namespace |
| `settings_conflict` | Read again; the user's draft is stale |
| `settings_unsynced` | Read again; the write is in place |
| `settings_failed` | Read again before retrying; the store failed without a code the client can act on |
| `operation_interrupted` | The part unloaded while the call was pending; read again before retrying |
| reloading | The daemon is restarting; retry after it is back |

### Flows

| Operation | Normal path | Caller cancels | Caller closes or the row unloads | Settled by |
| --- | --- | --- | --- | --- |
| `settings.read` | Resolve the declared source id, call [Settings](Settings.md) `read_user`, map the reply | Nothing changes; no reply | Pending calls end `operation_interrupted` | Settings |
| `settings.update` | Resolve the declared source id, admit the write into this part's task, call [Settings](Settings.md) `replace_user`, map the reply | An admitted write still commits or ends `settings_unsynced`; dropping the wait does not undo it | Admitted writes settle before the part closes | Settings owns the write; SettingsMethods holds the admitted task |

## Supersedes

- [Settings service](../appserver/settings-service.md) §4.3.
- [Management methods](../appserver/management-methods.md)'s list of four `settings.*` methods.

## Client-visible changes

- The management methods `settings.sources` and `settings.projects` are removed. No repository client calls them: the CLI reads five sources by name, the Web `DiagnosticsSettings` and `PermissionsSettings` pages read their two allowlisted sources by name, and no declared source has a project file. An out-of-repo client that listed sources or projects reads the sources it knows by name instead; its `settings.read` and `settings.update` requests and replies keep their shapes, including `scope` and the `{revision, document}` update reply.
- `settings.read` and `settings.update` keep the project scope field; the number is Settings' own process-local project number, which is what the request already carried.
- No error code, field name or notification name changes.

## Exceptions with reasons

- `managementMethods` is a mechanism registration point, not a forwarding facade: the namespace belongs to this part because Rpc reaches Settings through Agents and may not reach a management owner ([ADR 004](../../adr/004-loader-plugin-management.md) D4), and the declared-only rule and the client's error codes are this part's own contract.
- The client's request and reply shapes stay as today's protocol: existing CLI and Web callers send `{source, scope, expectedRevision, document}` and parse `{revision, document}`, and the redesign changes plugin-facing surfaces, not the client protocol.
- `settings.sources` and `settings.projects` are deleted, not moved: no client calls them (the CLI reads five sources by name, the Web pages two, and no declared source has a project file).

## Rule check

Rules: [§7](https://github.com/arcships/dim/pull/2029), [ADR 002](../../adr/002-plugin-scope.md).

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part, no Timeline facts, no folded state: not a Capability. |
| §2 scopes | Process; the methods are client surfaces, not Session state. |
| §3 orthogonality | No state of its own; one dependency, [Settings](Settings.md), plus the admitted-write task it owns. |
| §4 rule 1 | No event. |
| §4 rule 7 | Clients use management methods; they learn no file name, path, id or lock. |
| §4 rule 8 | No infrastructure change; the existing `managementMethods` mechanism is used. |
| §7 objects | Request and reply are data; no object, handle or path crosses. |
| §7 no proxy | Settings answers the document; SettingsMethods owns the namespace, the declared-only rule and the error mapping. |
| §7 one face | The management namespace exists once; the service operations stay on [Settings](Settings.md). |
| §7 cross-language | Both methods are data. |
| §7 caller complexity | The revision reaches the client because the user's draft depends on it; the declared-only rule and the error mapping stay here. |
| §7 one change | `settings.sources` and `settings.projects` are deleted with this change; the CLI and the Web pages are migrated in the same change. |
| ADR 002 D2 | No wiring plugin: the namespace owner is a process part that depends on Settings, as PermissionRules owns `permission`. |
| ADR 002 D4 | Business configuration stays in Settings sources; the methods carry no structural item. |
| ADR 027 | Not applicable: the callers are clients of a process-scope service. |

## Requirements raised to mechanism owners

None.

## Binding appendix

No service and no Rust binding: the two methods are registered with `managementMethods`. The request and reply types are the client's JSON shapes in the operations table; the CLI's `ReadRequest` and `UpdateRequest` above are the Rust spelling of the same data.
