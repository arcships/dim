# Mcp

Scope: process and Session. Issue: #2022.

`Mcp` owns the process's MCP configuration, content, connection definitions and OAuth credentials. `McpSession` owns one Session's MCP connection generations and tool source. `TemporaryMcp` owns the client-supplied temporary bindings of one Session. `McpPublication` and `McpAuth` are private parts of `Mcp`; neither has a public surface. The current implementation is described in [mcp-management](../appserver/mcp-management.md), [mcp-apps](../appserver/mcp-apps.md), [mcp-oauth](../appserver/mcp-oauth.md) and [TemporaryMcp](TemporaryMcp.md).

## Supersedes

## Target contract

Replaces the public-surface sections of [mcp-management.md](../appserver/mcp-management.md) §8 (`mcpService` content face and the `mcp.*` connection/lease decisions), [mcp-apps.md](../appserver/mcp-apps.md) §4.4 (`mcpSession`), [mcp-oauth.md](../appserver/mcp-oauth.md) (credential operations) and [TemporaryMcp.md](TemporaryMcp.md) (temporary bindings); the MCP rows in [service-faces.md](../service-faces.md), [resource-owner-operations.md](../contracts/resource-owner-operations.md) and [session-boundary.md](../contracts/session-boundary.md); and the old `McpServiceBinding`, `McpConnection`, `McpOwner`, `McpPublication` connection table and `mcpSession.subscribe`. Rules: [§7 所有者与操作](https://github.com/arcships/dim/pull/2029). Dependencies are defined by [FileStorage #2080](https://github.com/arcships/dim/pull/2080), [Settings #2094](https://github.com/arcships/dim/pull/2094), [SessionScope #2093](https://github.com/arcships/dim/pull/2093), [Connection/ManagementMethods #2095](https://github.com/arcships/dim/pull/2095) and the Session registries #2103/#2102/#2100; HostPlatform, SessionFiles and SessionStore are pending #2032/#2018.

## Use and scope: responsibility split

`Mcp` owns:

- **Configuration documents.** The Settings sources `mcp`, `mcp-projects`, `mcp-plugins` and `mcp-secrets`; `.mcp.json` is the project file of `mcp-projects` under a project Settings authorizes.
- **Content registration.** The MCP document a plugin package carries, the holding plugin, and the refusal reason.
- **Connection definitions and credentials.** The resolved server definitions, the OAuth records and refresh, and the disposable probe connection.
- **Client operations.** `mcp.catalog`, `mcp.configuration`, `mcp.update`, `mcp.save`, `mcp.remove`, `mcp.set_enabled`, `mcp.move`, `mcp.import.sources`, `mcp.import.preview`, `mcp.import.apply`, `mcp.test`, `mcp.inspect`, `mcp.disconnect`, `mcp.temporary.capabilities` and `mcp.auth.*`.
- **Session-facing values and commands.** The selection, definition and credential reference a Session reads; the connection-state registry that Sessions report into; the disconnect command a Session observes.

`McpSession` owns:

- **Connection generations.** The child `McpServer` rows it mounts with `SessionScope.mount`, the connection id it binds to its own row, and the tools, resources and views read from that generation.
- **Session view.** `state`, `refresh`, `widget` and `call`; the tool source it registers with `toolRegistry`; the result files it writes through `sessionFiles`.
- **Lifecycle.** It mounts generations after `sessionLifecycle.activated`, closes them on `closing`, and settles every generation on Session close or its own row unload.

`TemporaryMcp` owns:

- **Temporary bindings.** The client's server definitions, the binding id, the 30 s lease, the child `McpServer` instances and their tool source. A binding ends on detach, lease expiry, Session close or client disconnect.

Callers keep:

- The MCP protocol payloads and the model-visible tool results; `McpSession` carries the result content.
- Which entry a client edits, the client's own confirmation, and the UI around `mcp.catalog` and `mcpSession.state`.
- The plugin package's manifest; `PluginManager` passes the MCP document to `Mcp.register`.
- The MCP Apps view render; the client calls `mcpSession.widget` and renders the returned value.
- The approval of an MCP tool call: `Permission`/`Approval` decide it; MCP does not.

| Requirement (callers) | Compose from existing operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Register or withdraw a plugin package's MCP content | No: the content registry and its refusal are `Mcp`'s state | `register`, `withdraw`, `plugins`, `refusal` |
| Read and edit MCP entries, enablement, order and imports | No: the merged catalog and the atomic write are `Mcp`'s state | `mcp.catalog`, `mcp.configuration`, `mcp.save`, `mcp.update`, `mcp.remove`, `mcp.set_enabled`, `mcp.move`, `mcp.import.*` |
| Test a server definition before saving it | No: the probe connection is a resource `Mcp` owns and settles | `mcp.test` |
| Read the MCP view of one Session | No: the view is the Session's owned generation state | `mcpSession.state`, `mcpSession.refresh`, `mcpSession.widget`, `mcpSession.call` |
| Run one MCP tool from the Session | No: the tool handler needs the connection generation and its settlement | `mcpSession`'s tool source; the handler calls the owner's internal `call(connectionId, tool, args)` |
| Mount a temporary client binding | No: the child rows and the lease belong to the Session | `session.mcp_temporary.attach`, `renew`, `detach` |
| Inspect every Session's connection state | No: the process registry is the value `Mcp` owns; a Session cannot read another Session's rows | `mcp.inspect` reads the registry; `McpSession` reports its own state |
| Disconnect a source or server everywhere | No: `Mcp` owns the command value; each `McpSession` owns its connection | `mcp.disconnect` writes the command; `McpSession` observes and closes |
| Sign in to an OAuth server | No: credentials and refresh are `Mcp`'s state | `mcp.auth.start`, `complete`, `cancel`, `logout` |

## Caller example

`PluginManager` registers a package's MCP content:

```rust
let refusal = mcp.register(ctx, plugin, dir, document).await?;
if let Some(reason) = refusal {
    record_refusal(plugin, reason); // the package is installed; MCP content is refused
}
```

The local client edits one entry:

```rust
let catalog = mcp.catalog(project, cancelled.clone()).await?;
let revision = catalog.entry(id).revision;
mcp.save(id, revision, entry, cancelled).await?; // conflict -> re-read the catalog
```

`McpSession` mounts one generation and registers its tool source:

```rust
let definition = mcp.definition(entry_id, cancelled.clone()).await?;
let instance = session_scope.mount("mcp-server", definition.id, definition.data, cancelled).await?;
let connection = connection_id.new(); // bound to this McpSession row
let tools = server_tools(instance).await?;
tool_registry.register(ctx, "mcp", tools, handler(connection));
```

The tool handler calls the owner's internal operation; the handler holds only the id:

```rust
let result = session.call_connection(ctx, connection, tool, args, cancelled).await?;
```

`McpSession` reports its state to the process registry, and observes the disconnect command:

```rust
mcp.report_connections(session_id, entries).await?;
if mcp.disconnect_version() > seen {
    for entry in entries { close(entry.connection); }
}
```

`TemporaryMcp` attaches a binding:

```rust
let binding = temporary.attach(ctx, server, cancelled).await?;
// binding.id; renew before the 30 s lease; detach removes the child rows and tools
```

## Own contract

### Dependencies

`Mcp` uses Settings (`register`, `read`, `change`, `authorize_project`, `authorized_projects`, `declared_sources`), FileStorage (`read` for `.mcp.json` through the project root), and the management-methods and connection mechanisms of #2095. `McpSession` uses SessionScope (`mount`, `unmount`, `withHold`, `activated`, `closing`, `config`, `identity`), `toolRegistry` and `sessionFiles` (#2026/#2018), and `Mcp`'s values and registry operations. The `McpServer` child's process uses HostPlatform (#2032), whose target contract is pending; the requirement is recorded on #2032 and no interface is defined here. `TemporaryMcp` uses SessionScope, `sessionMethods` (#2026), `toolRegistry` and the `Mcp` definition values. `McpAuth` is a private process service of `Mcp`; the child rows that need a credential receive the service name and the entry id, not the credential value.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `mcpService.register` | Register a package's MCP document | `{plugin, dir, document}` | `{}` or `{refusal}` | Accepted write is not cancelled | `invalid_document`, `settings_conflict`, `settings_write`, `closed` |
| `mcpService.withdraw` | Withdraw a package's MCP document | `{plugin}` | `{}` | Accepted write is not cancelled | `closed` |
| `mcpService.plugins` | List the packages holding MCP content | `{}` | `{plugins}` | None | `closed` |
| `mcpService.refusal` | The refusal reason of a package | `{plugin}` | `{reason?}` | None | `closed` |
| `mcp.catalog` | The merged servers and their revisions | `{cwd?}` | `{servers, userRevision, projectRevision, userApplication, pluginApplication, projectApplication, invalid}` | Read can be cancelled | `mcp_invalid_config`, `service_reloading`, `operation_interrupted`, `closed`, `cancelled` |
| `mcp.configuration` | One entry's declared and effective view | `{id, project?}` | `{entry}` | Read can be cancelled | `missing`, `closed`, `cancelled` |
| `mcp.save` | Create or replace one entry at a revision | `{cwd?, id, source, expectedVersion, configuration}` | `{}` | Accepted write is not cancelled | `mcp_invalid_config`, `mcp_settings_read`, `service_reloading`, `operation_interrupted`, `closed` |
| `mcp.update` | Change selected fields at a revision | `{id, revision, patch}` | `{entry, revision}` | Accepted write is not cancelled | `invalid`, `missing`, `conflict`, `closed` |
| `mcp.remove` | Remove an entry | `{id, revision}` | `{}` | Accepted write is not cancelled | `missing`, `conflict`, `closed` |
| `mcp.set_enabled` | Enable or disable an entry | `{id, enabled, project?}` | `{}` | Accepted write is not cancelled | `invalid`, `missing`, `closed` |
| `mcp.move` | Move an entry in its source | `{id, before?}` | `{}` | Accepted write is not cancelled | `invalid`, `missing`, `conflict`, `closed` |
| `mcp.import.preview` | Parse an import payload without writing | `{document, project?}` | `{entries, invalid}` | Read can be cancelled | `invalid`, `closed`, `cancelled` |
| `mcp.import.apply` | Write the selected imported entries | `{document, selection, project?}` | `{applied, invalid}` | Accepted write is not cancelled | `invalid`, `conflict`, `closed` |
| `mcp.import.sources` | Discover import sources | `{cwd?}` | `{sources}` | Read can be cancelled | `invalid`, `closed`, `cancelled` |
| `mcp.temporary.capabilities` | The temporary binding capabilities ACP reads | `{}` | `{transports, leaseSeconds}` | None | `closed` |
| `mcp.test` | Probe one server definition with a disposable connection | `{config, timeout?}` | `{status, latency, tools, reason}` | Cancel or timeout stops the probe; the owner settles it | `invalid`, `timeout`, `refused`, `cancelled`, `closed` |
| `mcp.inspect` | One entry's enabled state, configuration and live connections, read from the report registry | `{cwd?, id, source}` | `{enabled, configuration, connections}` | None | `mcp_invalid_config`, `closed` |
| `mcp.disconnect` | Ask every Session to close one source or server | `{source?, server?}` | `{}` | None; the command is a value | `closed` |
| `mcp.auth.start` | Start an OAuth flow | `{id, project?}` | `{authUrl?}` | Flow can be cancelled | `missing`, `unsupported`, `refused`, `closed`, `cancelled` |
| `mcp.auth.complete` | Finish an OAuth flow with the callback | `{id, callback}` | `{credentialState}` | Flow can be cancelled | `missing`, `invalid`, `refused`, `cancelled`, `closed` |
| `mcp.auth.cancel` | Cancel an OAuth flow | `{id}` | `{}` | None | `missing`, `closed` |
| `mcp.auth.logout` | Remove an entry's OAuth records | `{id, project?}` | `{}` | Accepted write is not cancelled | `missing`, `user_scope`, `closed` |
| `mcpSession.state` | The Session's MCP view | `{}` | `{servers, catalogError, closed}` | None; the value is read | `closed` |
| `mcpSession.refresh` | Ask the owner to rediscover the enabled entries | `{}` | `{}` | None; the signal is a value | `closed` |
| `mcpSession.widget` | The view for one published tool | `{publishedTool}` | `{view}` or absent | The client dropping the call cancels the read | `absent`, `unsupported`, `not_callable`, `unknown_server`, `unavailable`, `refused`, `unknown`, `closed` |
| `mcpSession.call` | One MCP Apps call | `{server, tool, args}` | `{result}` | The client dropping the call cancels the resource read | `unknown_server`, `unavailable`, `refused`, `unknown`, `cancelled`, `closed` |
| `session.mcp_temporary.attach` | Mount a client temporary binding | `{sessionId, bindingId, servers}` | `{}` | The caller dropping the call shuts the half-made binding | `invalid_params`, `mcp_binding_failed`, `mcp_binding_conflict`, `mcp_binding_missing`, `cancelled`, `closed` |
| `session.mcp_temporary.renew` | Extend the binding lease | `{binding}` | `{}` | None | `invalid`, `missing`, `closed` |
| `session.mcp_temporary.detach` | Remove the binding and its children | `{binding}` | `{}` | None; the owner completes the removal | `missing`, `closed` |

The internal `McpSession` connection operations (`open`, `tools`, `observe`, `close`) are on the owner type and are called only by its own tool source and view handlers; they carry `connectionId`. `Mcp`'s `definition`, `selection`, `report_connections`, `disconnect_version` and `config_version` are internal operations or values used by `McpSession`; they are not public names.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid`, `invalid_document` | Fix the document or the entry; the write did not happen |
| `missing` | Re-read the catalog; the entry or binding is gone |
| `conflict` | Re-read the revision and decide again; the write did not happen |
| `user_scope` | The credential belongs to the user scope; ask the user or use the user project |
| `settings_write`, `settings_conflict` | Retry after the Settings row is available, or report the conflict |
| `refused`, `unsupported`, `not_callable`, `unknown_server`, `unavailable`, `unknown`, `absent` | Report the MCP view or tool error; the generation is unchanged |
| `timeout` | The probe did not finish; the owner settled it; report the result |
| `mount_failed` | The child rows did not activate; the owner shut them down; report the binding failure |
| `cancelled` | Nothing was committed; the read or the flow stopped |
| `mcp_invalid_config` | Fix the MCP document or entry; the client sees the current invalid-config code |
| `mcp_settings_read` | Retry after Settings is readable, or report the failure |
| `service_reloading`, `operation_interrupted` | Retry after the row is active, or re-read the current state |
| `closed` | The owner is unloading; use the replacement generation |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `McpSession` generation | Read the selection and definition, mount the `McpServer` child after `activated`, bind a connection id to the row, register the tool source | A dropped `open` unmounts the half-made child | The row's generations close and unmount; the tool source is withdrawn | New generations are refused; every child is unmounted; the state reports unavailable/closed | Child by `McpSession` through `SessionScope.unmount`; connection id by the row |
| Tool call | The handler resolves the connection id, calls the child, writes the result through `sessionFiles` | The cancel stops the request and settles the child's process | The handler's row unload stops its calls | Pending calls end `closed`; the child settles | Connection by `McpSession`; result file by `McpSession`/`sessionFiles` |
| `mcp.inspect` | `Mcp` reads the registry of values reported by Sessions | None; the value is read | The Session's last report remains until its row unloads | The registry is a process generation value | Registry by `Mcp`; entries by each `McpSession` |
| `mcp.disconnect` | `Mcp` writes a command version; each `McpSession` observes and closes the matching connection | None; the command is a value | The Session's connections close with the row | New Sessions read the current command version; old generations do not | Command by `Mcp`; connections by each `McpSession` |
| `mcp.test` | `Mcp` opens a disposable connection, probes, lists tools, closes it | The timeout or cancel closes the probe | The probe is closed with the owner | The owner closes every probe | Probe by `Mcp` |
| `mcp.auth.start/complete` | `Mcp` starts the flow, stores the pending state, completes and writes the credential | The flow cancels; the pending state is removed | The flow ends with the owner | Pending flows are dropped on reload; credentials stay | Flow and credential by `Mcp`/`McpAuth` |
| `TemporaryMcp.attach` | The Session mounts the child, registers tools, returns the binding id and lease | A dropped attach unmounts the child | The binding's children close; tools are withdrawn | New binds refused; every child unmounts | Child by `TemporaryMcp`; lease by the binding |

## Client-visible changes

None. `mcp.inspect` keeps `{cwd?, id, source}` and `{enabled, configuration, connections}`, now re-sourced from the report registry; `mcp.catalog`, `mcpSession.state`, `session.mcp_temporary.attach`, `mcp.import.sources` and `mcp.temporary.capabilities` keep their current requests, replies and codes. The removed `mcpSession.subscribe` is Rust-only and never crossed the client protocol.

## Exceptions with reasons

- `McpAuth` is a private process service. Its instance key and unload are part of the MCP plugin assembly; the Session child receives the service name and an entry id, never a credential value. The MCP plugin's assembly is the shared lifecycle design for the service and its child rows.
- `McpServer` is a private child row of the MCP feature. `McpSession` and `TemporaryMcp` mount it through `SessionScope`; its tools are published through the Session's `toolRegistry`. The child has no public name.
- `mcpSession.call` and `mcpSession.widget` express cancellation by the client dropping the call; the cross-language face has no cancel parameter, and the owner stops the resource read when the call is dropped.
- `mcp.inspect` returns values reported by Sessions. `Mcp` owns the registry, each `McpSession` owns and reports its entries; `Mcp` does not read a Session's loader or child rows.
- `mcp.disconnect` is a command value. `Mcp` does not close a Session's connection; each `McpSession` observes the command and closes its own generation.
- `mcp.test` owns a disposable probe connection. It is not a Session generation and does not appear in `mcp.inspect`.
- The `mcp.*` methods are management methods (#2095), not a second `mcpService` face. The owner's operations are the same operations; the client binding is the management-methods mechanism.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | `Mcp` is one process part; `McpSession` and `TemporaryMcp` are Session parts. None folds Timeline state; they are not Capabilities. |
| §2 scopes | Process configuration and credentials; Session connection generations and views; branch tool calls through the Session's tool source. |
| §3 orthogonality | Each owner uses the other's operations: `McpSession` reads `Mcp`'s values and reports its state; no Session reads another Session. |
| §4 rule 1 | No MCP command Timeline event. |
| §4 rule 2 | No handoff: the connection id stays with `McpSession`; `Mcp` gets values. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The owner's vocabulary is MCP entries, servers, connections and views; no client or plugin business appears. |
| §4 rule 5 | No batches. |
| §4 rule 6 | Tool results reach the model through the Session's tool source; `McpSession` writes the result. |
| §4 rule 7 | Callers use `mcpService`, `mcp.*`, `mcpSession` and `session.mcp_temporary`; no caller holds `McpConnection`, `McpOwner`, `McpPublication` or `McpAuth`. |
| §4 rule 8 | Requirements to #2018, #2026, #2093, #2095 are listed below. |
| §4 rule 9 | No Timeline fact. |
| §7 callbacks and ids | Connection ids and Session ids cross; credentials and child instances stay with the owner. Drop-cancel is the data-face cancellation for view calls. |
| §7 one change | The old `McpServiceBinding`, `McpConnection`, `McpOwner`, `McpPublication` table and `mcpSession.subscribe` are removed in the same change that migrates every caller. |
| ADR 002 D1 | Process `Mcp`; Session `McpSession`/`TemporaryMcp`; the connection is per Session. |
| ADR 002 D2 | No wrapper: `mcpService` is on `Mcp`, `mcpSession` on `McpSession`; no binding type. |
| ADR 002 D3 | Extensions depend on the process MCP owner; `McpSession` is the Session child of the same feature. |
| ADR 002 D4 | The configuration documents are Settings sources; no assembly business setting is added. |
| ADR 002 D5 | Operations, values and ids are language-neutral; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- #2018: `sessionFiles` needs `write`, `read` and `remove` for MCP tool results; the Session owner writes result files.
- #2026: `toolRegistry.register` must accept a Session-owned tool source with a handler that holds a connection id; `sessionMethods.register` must accept `session.mcp_temporary.*`.
- #2093: `SessionScope.mount`/`unmount` must accept the `mcp-server` group and a definition value; `withHold`, `activated` and `closing` are used as defined.
- #2095: `ManagementMethods.register` must host the `mcp.*` namespace; `Connection` calls `mcpSession.state/refresh/widget/call`.
- #2032: HostPlatform's target contract is pending; the `McpServer` child uses its process operations; the requirement comment records the intent and no interface is defined here.
- #2080: FileStorage `read` for `.mcp.json` under the project root; no new operation.
- #2094: Settings `register`, `read`, `change`, `authorize_project`, `authorized_projects`, `declared_sources`; no new operation.

## Binding appendix

`mcpService` is on `Mcp`; `mcpSession` on `McpSession`; `session.mcp_temporary` on `TemporaryMcp`. `McpServiceBinding`, `McpConnection`, `McpOwner` and `McpPublication` do not exist. The management methods are registered through #2095.

```rust
#[rutis::service(name = "mcpSession")]
impl McpSession {
    pub fn state(&self) -> McpSessionState;
    pub fn refresh(&self) -> Result<(), McpSessionError>;
    pub async fn widget(&self, caller: &Ctx, published_tool: PublishedToolRef, cancelled: CancelledFn) -> Result<Option<McpView>, McpAppError>;
    pub async fn call(&self, caller: &Ctx, server: String, tool: String, args: serde_json::Value, cancelled: CancelledFn) -> Result<serde_json::Value, McpAppError>;
}
```

`session.mcp_temporary.attach/renew/detach` are `SessionMethods` registrations; `attach` takes `caller: &Ctx` and the binding id is bound to the caller's Session row. Current types: `McpPluginDocument`, `McpPluginRefusal`, `McpWidgetResult` and the `mcp/management.rs`/`mcp/session.rs` request and reply structs; `PublishedTool` is private, so the target exposes a public `PublishedToolRef` value instead. Target error enums use `#[derive(Debug, serde::Serialize, thiserror::Error)]` and `#[serde(tag = "code", rename_all = "snake_case")]`; one variant per error table code; a callback failure is `Rejected { reason, message }` with wire code `rejected`. The current `McpAppError` is a non-serializable `thiserror` enum and is marked target. `From<rutis::host::CallError>` is implemented where a callback failure must propagate. `CancelledFn` is the binding's cancellation argument, never a request field. The temporary-crate check with rustc 1.100.0 passed; every target error enum carries `#[error("...")]` for `Display`. Wire-code mapping (internal variant -> published code): mcp_invalid_config, mcp_settings_read, service_reloading, operation_interrupted.

```rust
#[rutis::service(name = "mcpService")]
impl Mcp {
    pub async fn register(&self, caller: &Ctx, plugin: String, dir: String, document: McpDocument, cancelled: CancelledFn) -> Result<Option<McpRefusal>, McpContentError>;
    pub async fn withdraw(&self, caller: &Ctx, plugin: String, cancelled: CancelledFn) -> Result<(), McpContentError>;
    pub fn plugins(&self) -> Vec<String>;
    pub fn refusal(&self, plugin: &str) -> Option<McpRefusal>;
}
```

`TemporaryMcp`'s methods are registered with `sessionMethods`; the request, reply and error types are the typed structures in `mcp/temporary.rs` and the management module. `McpServiceBinding` and the `McpConnection` methods are deleted.
