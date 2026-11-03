# SessionMethods

Owner: [methods.rs](../../../../crates/dim-agent-capabilities/src/methods.rs).
Keeps the Session's method table, `sessionMethods`: the method namespaces the
Session's parts register with it, which Rpc reads on each call to dispatch a
Session method through the Session. Contract:
[methods contract A.1](../../../design/contracts/session-boundary.md#a1-method-contributions).

## Dependencies

None in this graph. When the assembly names the process value
`ReservedMethods`, the table depends on it. App Server names and provides it
with the protocol's roots and built-ins, and the table then refuses a namespace
that lies in one of those roots or equals, contains or lies inside a built-in.
A host that names none reserves nothing: every namespace in the name grammar is
accepted.

## Events and lifetime

No Timeline events, worker tasks or I/O. A part depends on `sessionMethods` and
calls `register(ctx, namespace, declarations, handler)` from its own `apply`;
the registration is withdrawn when that part unloads, and a namespace registered
already fails
([rutis usage R6](../../../design/rutis-usage.md#r6-contributions-register-with-their-owner)).
A refused registration fails the part. A part of another language calls
`sessionMethods.register(namespace, declarations, call)` through the bridge;
the registration belongs to the calling row, and `call` is the row's function,
called back through the bridge. The table provides that entry itself, at its
dispatcher key, with the native service.

A name belongs to the longest namespace that holds it. When a namespace is
withdrawn, the table keeps the row that registered it, so Rpc answers a call
into it from that row's status (`service_reloading` or `service_unavailable`)
rather than `unknown_method`. Each registration owns the calls it admitted: its
part's unload interrupts `report` calls and waits for every admitted call.
`session.methods` lists the methods of the namespaces registered now that no
management namespace holds.

## Tests

[session_methods.rs](../../../../crates/dim-app-server/tests/transport_flow/session_methods.rs)
registers namespaces from test parts and covers the reserved-name refusals, a
namespace inside a management namespace, dispatch through the Session, a
provider reload answering `service_reloading`, and `session.methods`;
[session_typescript.rs](../../../../crates/dim-app-server/tests/transport_flow/session_typescript.rs)
registers from TypeScript rows, including a namespace registered already.

<!-- generated:plugin SessionMethods — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | — |
| 提供 | `S:SessionMethods`、`S:dyn HostDispatch#sessionMethods` |
| 被注入于 | `Attachments`、`InputMethods`、`PromptSuggestion`、`Questions`、`ShellInput`、`WorkspaceTools` |
<!-- /generated -->
