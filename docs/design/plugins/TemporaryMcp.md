# Temporary MCP

Scope: Session. Owner: the MCP feature. A separate `TemporaryMcp` part accepts
client-supplied server configurations and mounts ordinary `McpServer` plugins as
its disposable children. It is not a Capability: it owns I/O and tool
contributions, not Timeline business state. Agent core, RPC dispatch and the
built-in Session protocol do not change.

```text
AcpGateway (client process)
  session new/load/resume/fork
       | public contributed Session methods
       v
TemporaryMcp (native Session part)
  temporary binding plugin
    +-- existing McpServer plugin, one per supplied server
    +-- existing ToolRegistry source and MCP tool execution
    +-- cleanup: withdraw tools, cancel/settle calls, close transports
```

## Inputs and wiring

The existing MCP process plugin registers `TemporaryMcp` with the part registry;
the execution profile requires it alongside `McpSession`. It declares Session
methods, lifecycle and workspace configuration. Each binding declares the tool
registry and Session lifecycle. Its server children declare the existing MCP
authentication dependency. Keys come from the Session's names. No independent
daemon, transport implementation, Session store or settings source is added.

`mcp.temporary.capabilities` is a read-only management method advertising
`transports: ["stdio", "http", "sse"]` and `leaseSeconds: 30`. Older daemons
lacking this method retain the Gateway's ignore-client-MCP behavior and do not
advertise the new transport capabilities.

The Session part contributes the namespace `session.mcp_temporary` (the existing
`session.mcp` name belongs to the built-in runtime-state read):

| Method | Input | Completion |
| --- | --- | --- |
| `attach` | `sessionId`, UUID `bindingId`, `servers` map from nonempty name to the existing MCP server configuration | Validates every entry before mounting; returns `{}` after initial tools have been registered. Same binding and identical configuration is idempotent; different configuration conflicts. Empty maps need no binding. |
| `renew` | `sessionId`, `bindingId` | Extends the existing binding's deadline by 30 seconds; an absent binding returns `mcp_binding_missing`. Does not create resources. |
| `detach` | `sessionId`, `bindingId` | Idempotently withdraws the binding and waits for its children and accepted calls to settle. |

Invalid configuration returns `invalid_params`; an unavailable transport returns
`mcp_binding_failed` without configuration, URL, headers or environment values.
No input is written to `mcp.json`, `.mcp.json`, SQLite settings, account stores or
Timeline. Supplied stdio commands run when the binding is enabled, following the
existing configured-command boundary; native Permission/Approval still governs
model tool calls.

Bindings add namespaced tool sources; they never replace user/project MCP
entries or another client's binding. Server/tool identities include the binding
id to avoid collisions. Tool definitions, calls, result conversion, cancellation
and list-change observation reuse existing MCP implementation. Schemas follow
the shared [tool declaration contract](../contracts/session-boundary.md#a5-tool-contribution),
including composition nodes without a `type`. Static headers
and stdio environment are connection inputs; this surface does not start OAuth
login or persist credentials.

A server setup or connection failure disposes its whole temporary binding.
Other bindings and configured MCP sources keep their own lifetimes.

## Gateway and lifetime

The Gateway converts ACP stdio/HTTP/SSE configuration at the protocol boundary,
preserves headers/environment, rejects duplicate server or header/environment
names, and uses the native Session cwd for stdio. New/fork bind only after a
native Session id exists; load/resume bind after checking its real cwd. Initial
attachment completes before the ACP establishment response and before prompt
admission. Unsupported ACP-over-MCP transport remains unsupported.

One Gateway/Session has one binding id. Repeated establishment with the same
servers reuses it; conflicting configuration does not silently replace running
resources. Empty lists leave this connection's existing binding alone. The
Gateway renews bindings every ten seconds while it owns them. It retains their
volatile configurations for native reconnect and can idempotently reattach the
same binding. An expired binding is reattached before another prompt; mutations
and tool calls themselves are never replayed.

ACP EOF/signal/unload explicitly detaches owned bindings while RPC is available,
then detaches the viewer. It does not close the Session or stop its model turn.
Explicit Session close/delete and native part unload dispose bindings as part
of their owner. Process death or an unrecoverable native connection loss stops
renewal; at expiry the native owner unloads the binding. A closing Session stops
discovery and calls immediately without waiting for unload. Detaching first
withdraws future tool availability; already bound calls are cancelled and
settled before server resources disappear. An expired runtime handle cannot
publish tools into a replacement binding.

## Rule check

| Rule | Application |
| --- | --- |
| §1 Capability | Session I/O/tool owner, no branch state machine |
| §4.1 commands/results | Typed methods change the MCP owner's transient resources; tool execution uses existing native owners |
| §4.2 handover | Accepted tool handlers retain their connection; disposal cancels and settles them |
| §4.3 before | Not used |
| §4.4 infrastructure | No runtime, RPC or built-in Session protocol changes |
| §4.5 batches | No new persistent facts or batches |
| §4.6 context | Only normal native tool definitions/results enter model requests |
| §4.7 ownership | Public registries and owned child handles only; persisted MCP configuration is unchanged |
| §4.8 mechanisms | Existing part registry, rutis children and Session methods suffice |
| §4.9 facts | No new Timeline events; configuration and connections are volatile |
| ADR 002 D1–D2 | Session state; each part owns actual resources, no wiring-only plugin |
| ADR 002 D3 | Dependencies declared; core knows nothing about temporary MCP |
| ADR 002 D4 | Server configuration is method input, never loader JSON or a shared settings patch |
| ADR 002 D5 | Methods have language-neutral JSON contracts; Rust keys are binding details |

## Requirements raised to mechanism owners

None. Changes are restricted to MCP business parts, Gateway/CLI composition,
tests and documentation. No RPC disconnect hook, core event, reverse-call
channel or durable lease mechanism is required.

## Verification

Public flows cover stdio/HTTP/SSE discovery and real tool calls, Session/client
isolation, no configuration writes, idempotent attachment, explicit disposal,
partial startup failure, lease expiry, list changes and cancellation while a
tool call is running. Gateway flows cover nonempty new/load/resume/fork inputs,
old-daemon compatibility, reconnect and cleanup. Local Lody verification must
exercise an injected MCP tool, not merely a text-only model response.
