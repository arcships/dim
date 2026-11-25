# SessionMethods

Owner: [methods.rs](../../../../crates/dim-agent-capabilities/src/methods.rs).
Keeps the Session's method table, `sessionMethods`: the method namespaces the
Session's parts register with it, which Rpc reads on each call to dispatch a
Session method through the Session. Contract:
[SessionMethods](../../../design/plugins/SessionMethods.md).

## Dependencies

None in this graph. When the assembly names the process value
`ReservedMethods`, the table depends on it. App Server names and provides it
with the protocol's roots and built-ins, and the table then refuses a namespace
that lies in one of those roots or equals, contains or lies inside a built-in.
A host that names none reserves nothing: every namespace in the name grammar is
accepted. The value is also the data face `reservedMethods`: a row of another
language asks `check {namespace, declarations}` the same question
([service faces §4.9](../../../design/service-faces.md#49-旧主机端口credentialvariableshost-等六个)).

The contract also answers a withdrawn namespace from the row that registered it.
Rpc reads that row today (`session_provider` in
[dim-app-server/src/methods.rs](../../../../crates/dim-app-server/src/methods.rs));
the table itself has no readable face for it, and the read is raised in the
contract ([SessionMethods §4 rule 8](../../../design/plugins/SessionMethods.md#requirements-raised-to-mechanism-owners)).

## Events and lifetime

No Timeline events or I/O. Each registration tracks the calls it accepted, so its unload can interrupt them and wait. A part depends on `sessionMethods` and
calls `register(ctx, namespace, declarations, call)` from its own `apply`, with the part's own function as `call`;
the registration is withdrawn when that part unloads, and a namespace registered
already fails
([rutis usage R6](../../../design/rutis-usage.md#r6-contributions-register-with-their-owner)).
A refused registration fails the part. A part of another language calls
`sessionMethods.register(namespace, declarations, call)` through the bridge;
the registration belongs to the calling row, and `call` is the row's function,
called back through the bridge. The table provides that entry itself, at its
dispatcher key, with the native service.

A name belongs to the longest namespace that holds it. When a namespace is
withdrawn, the table keeps the row that registered it, so a call into it answers
`service_reloading` or `service_unavailable` from that row's status. Today a
withdrawn namespace whose row is absent from the loader answers
`service_reloading`; the target contract answers `unknown_method` there, as it
already does for a disabled row and for a name no namespace ever held. Each registration owns the calls it admitted: its
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
| 被注入于 | `Attachments`、`InputMethods`、`LoopMethods`、`PromptSuggestion`、`Questions`、`ShellInput`、`ToolMethods`、`WorkspaceTools` |
<!-- /generated -->
