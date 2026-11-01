# MCP Apps host contract

Status: Draft.

Scope: Session, with one process decision (§3 D8).
Issue: [#451](https://github.com/arcships/dim/issues/451).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md), [ADR 003](../../adr/003-approval-scope.md).

## 1. Scope and non-goals

An MCP server may ship an interactive view with a tool: the tool's `_meta.ui.resourceUri` names a
`ui://` resource whose content is an HTML document
([MCP Apps, SEP-1865](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx)).
The client that shows it is the **host**: it sandboxes the document, enforces the declared CSP, runs
the `ui/*` handshake, and forwards what the view asks its server for.

Two different `tools/call` requests reach an MCP server of a Session, and they are not the same kind
of act:

| | Agent tool loop | A view's call |
| --- | --- | --- |
| Who decided | The model, in a turn | The user, interacting with the rendered view |
| Who admits it | The branch's `Permission`, with `Approval` when the decision asks | The host: an enabled entry, the Session's own connection, the same entry |
| Where the result goes | A `tool/result` fact, and from there the model context | The view that asked |
| Timeline | `tool/call`, `tool/start`, `tool/result` | Nothing |
| Owner of the call | The branch's tool executor | The Session's MCP connection, requested by a client |

This document owns the second column: what a client may ask the daemon for on behalf of one rendered
view, and what it never may. The daemon is the MCP client — connections, credentials, OAuth state,
tool discovery and tool publication belong to the Session that adopted the entry — so a host cannot
answer a view by opening its own connection without duplicating daemon-owned configuration,
credentials and OAuth state.

Two words are used precisely below. An entry's **discovered** tools are the ones its server declares in
`tools/list`; they are what a view's call is looked up in. The **published** tools are the subset the
Session offers the model, under the namespaced name `mcp_<label>_<hash>` a `tool/call` fact carries.

It does not own:

| Surface | Owner |
| --- | --- |
| Sandboxing, CSP construction, `ui/initialize` and host context, display modes, theming, `ui/open-link`, `ui/resource-teardown`, `ui/notifications/*` | The client (Desktop, `arcships/dim-electron`) |
| MCP entry configuration, the `mcp` management namespace, the `mcp.test` probe | [MCP configuration management](mcp-management.md) |
| `session.mcp`, `mcp.auth.*`, the catalog's `auth` field, the credential store | [MCP OAuth](mcp-oauth.md#9-interfaces) |
| The agent's tool loop: admission, the `tool/*` facts, model context | [ADR 003](../../adr/003-approval-scope.md), [Session boundary](../contracts/session-boundary.md) |
| How user input is validated and adopted | [Session access](../contracts/session-access.md), [Session boundary](../contracts/session-boundary.md) |
| Which clients exist and which Session methods they take | [Clients](../clients.md) |

## 2. Current behavior

`McpServer::tools` keeps a discovered tool's `name`, `description` and `inputSchema` and drops
`_meta`, so the owner discards the widget association a server declares. Nothing in the daemon issues
`resources/read`; `McpServer` knows `tools/list` and `tools/call` only. A tool result keeps `content`
and `structuredContent` (both in the Timeline's `ToolResult`) and drops the result's `_meta`.

The client-facing surface stops one step short of a view:

- `session.mcp` reports per-entry state, version and rejection, and `mcp.inspect` reports the
  connections a Session holds. Neither carries tools or resources.
- `mcp.test` lists tools, but it opens a disposable connection with no Session, publishes nothing and
  reports no resource ([MCP configuration management §4.5](mcp-management.md#45-probe-mcptest)).
- The only path that issues `tools/call` is the agent's tool loop: the branch's executor admits it and
  writes the exchange into the Timeline ([Session boundary](../contracts/session-boundary.md)). A
  view's call is not that, and must not become it.
- The only path that adds text to the conversation is `session.submit`, a validated user input.

So a Desktop client cannot render a view, and cannot implement one itself without duplicating the
daemon's MCP client.

## 3. Decisions

### D1. The daemon stays the MCP client; the host contract forwards the view's requests

Rendering, sandboxing and the view's own message loop stay in the client. Everything the view asks of
its server — the document, a tool call — is a request on the Session's own connection, which the
daemon issues and the client never opens. This is the boundary the probe and `mcp.inspect` already
respect: the daemon owns the connection, a client reads what it needs through a method.

No second MCP client, no new connection, no new plugin, no new part: the operations use the existing
connection and the tool publication the Session already holds. Credentials stay where they are, in
the connection; nothing on this surface returns a header, a token or a callback URL to a client, and
certainly not to a view.

### D2. A view's operations are Session methods

Each operation acts on one Session's live connection and on the tools that connection discovered. That
is Session state, so the operations are Session methods dispatched through `sessionId`, beside
`session.mcp` and `session.mcp.refresh`
([Management methods § Dispatch and lifecycle](management-methods.md#dispatch-and-lifecycle)). A
management method would have to reach into a Session's connection from the process, which is the
wiring this repository removes.

The methods carry no branch: a view's call is not a turn's call, so no branch decides anything about
it (D5).

### D3. A view is identified by its Session, its entry and its tool; there is no handle

There is no view session, no lease, no token and no daemon-side registry. A view's **call** names the
Session it belongs to, the entry the Session adopted, and the tool; the daemon resolves them against
what the Session holds at that moment. The **mount read** names only the Session and the published tool
name, because a client reading a `tool/call` fact has nothing else: resolving that name to its entry is
what the mount read does (D4). Two consequences follow.

- Retiring a connection generation needs no invalidation: the next request uses the generation that is
  current then, and an entry that stayed the same keeps the same answer.
- The client's cache key is `session.mcp`'s per-entry `version`, which changes when the entry's
  discovered tools change **or when its connection is re-established** — a reconnect, a restore or a
  replaced generation starts a new discovery. A view re-reads its document when that version changes or
  when the entry is no longer `ready`, so a rebuilt connection cannot serve a document cached under the
  old one.

A runtime handle would add state to create, expire and clean up, and runtime handles are not durable
identities ([design rules §4 rule 8](../../architecture/rust-agent/principles.md)).

### D4. The mount read starts from the tool name the client already has

A `tool/call` fact carries the published tool name (`mcp_<label>_<hash>`), not the entry and not the
server's own tool name. So the mount read takes the published name, and its reply names the entry and
the server's tool, which the view's later calls need. It returns the document and everything the host
must apply to render it: the MIME type, the content, and `_meta.ui` (CSP domains, sandbox
permissions, dedicated domain, border preference). It also returns the tool's description and input
schema, which the host needs for `hostContext.toolInfo`.

The reply is a snapshot of the current generation. A tool the entry no longer declares, or one that
declares no resource, is `mcp_app_absent`; the daemon does not fall back to another tool or another
entry.

### D5. A view's call is the host's request to an entry it enabled, not an agent decision

A view's `tools/call` is not a tool call the model decided. It belongs to a view of an MCP entry the
user enabled — a click, a refresh, or the view loading — so it takes the shape the rest of the
daemon's MCP surface has: one request on that entry's own connection, forwarded as the MCP protocol
call it is.

[ADR 003](../../adr/003-approval-scope.md) D1–D3 therefore leave it outside `Permission` and
`Approval`: those govern what the agent decides, and a view's call is not the agent deciding. The
enabled entry is what authorizes it (ADR 003 D2/D3), exactly as it authorizes the server's startup,
its tool list and its credentials.

What the daemon does enforce is host policy, because that is what it can see:

- the `server` names an entry of this Session, and the entry is `ready` — not disabled, not failing,
  not waiting for sign-in;
- the tool exists in that entry's current discovery, and the connection is the generation the Session
  holds now;
- the call runs on that entry's connection and no other: no cross-entry and no cross-server call
  exists in this contract (`mcp_unknown_server`, `mcp_app_absent`).

Whether a view's call needs the user's consent is the host's decision, not the daemon's. The
specification puts that lever in the host ("the Host MAY decide to block some messages or subject them
to further user approval"), and only the client knows what the user is looking at. A client that wants
to ask before forwarding a call does so before it sends the request.

The call does not go through the branch's tool executor: it has no turn, it writes no `tool/*` fact,
and its result must not enter the model context (D6). The daemon adds no deadline either: a view's call
ends when the server answers, when the client's request is interrupted, or when the Session or the
entry ends. A model tool call on the same connection has no deadline, and a per-surface one would be a
second contract for the same connection.

### D6. The result belongs to the view

The reply is the tool's result, returned to the client that asked. It is not written to the Timeline,
so it never enters the model context: the model context holds adopted inputs and the tool results of
turns ([design rules §4 rule 6](../../architecture/rust-agent/principles.md)). The view and its server
own what happened; the conversation already shows the tool call that produced the view, which the
agent recorded.

A view's call therefore leaves no fact later decisions use. Its only trace is the process log (D10).

### D7. Only tools the server declares callable by an app can be called from an app

A tool's `visibility` decides who may call it; the default is both. The daemon therefore:

- keeps `visibility` from `_meta.ui` with the discovered tool, and refuses a view's call to a tool
  whose visibility excludes `"app"` (`mcp_app_not_callable`);
- does not publish a tool whose visibility excludes `"model"` to the Session's tool list, so the model
  never sees it. Such a tool is callable only by a view, which is what the specification's `"app"`
  scope is for.

The daemon cannot tell which view a host is forwarding for, so it enforces what it can see: the tool
exists in the entry the request names, and that entry is one of the Session's. The host's side of the
same rule is that a view's call goes to the entry whose document it mounted, because that is what
makes `"app"` mean "callable from this server's views". A different entry is the host breaking its own
contract, not something the daemon can detect.

A view may call any tool its server declares an app may call, model-visible or app-only, including the
tool that rendered it. The call is the user's action on an entry they enabled, and a view that can
refresh its own tool's data is the ordinary case; requiring `visibility: ["app"]` for view-driven tools
would be stricter than the specification's default and would refuse that refresh on servers that do not
annotate their tools. Restricting it later is a policy change to this decision, not a mechanism one.

### D8. The daemon advertises the extension on every MCP connection

A server registers widget tools only when its client declares support, so the daemon's MCP client
declares `io.modelcontextprotocol/ui` with `text/html;profile=mcp-app` in `initialize`. The
declaration is process-wide, not per Session: a connection exists before and independently of who
views the Session, and a Session can gain a widget-capable viewer later.

The cost is that a server may now attach a `resourceUri` to tools of a Session no client renders views
for. Nothing else changes for that Session: a widget tool keeps meaningful text content, which is each
client's fallback, and a client that does not render views ignores the descriptor.

### D9. The view's message is a user input; model-context updates are not supported

`ui/message` is a message to the conversation on the user's behalf, which is `session.submit`: the
Session input boundary validates it like any other user input, and the daemon records it as an ordinary
adopted input. It therefore enters the model context exactly as a typed message does, which is intended
and is the point of the request: the user sent it. The host decides whether to send it; nothing records
which view produced it.

`ui/update-model-context` has no conforming path and stays unsupported: the specification lets the view
replace its model context on every update, while an adopted input is handed over once and is final
([design rules §4 rule 2](../../architecture/rust-agent/principles.md)), and the model context holds
only adopted inputs and tool results (§4 rule 6). The host answers the view's request with an error. A
view that needs the model to see something sends `ui/message`.

### D10. Only the document a tool declares is readable, and the call is logged

The specification lets a view issue `resources/read` for any resource of its server. The daemon serves
none of those as a resource read: the only document it fetches is the one §4.1 mounts, and it is
fetched because a tool of that entry declares it. A view asking for another URI is answered from the
document it already mounted, and anything else is a refusal the host produces — there is no method that
takes a resource URI, and so no allowlist to keep in step with a server's `resources/list`. A server
that needs more can put it in the tool result's `structuredContent`, which the host already has.

A view's call is written to the daemon's process log with the Session id, the entry id, the tool name,
the outcome and the duration — never the arguments or the content, which can carry secrets and user
data. It writes no Timeline fact (D6). A product that later wants a record of UI actions in the
conversation decides that on the client side; it is not part of this contract.

### D11. One size limit, on the document

The widget document is limited to 4 MiB (the text, or the decoded blob), because it is a document the
daemon hands to a sandbox and the host caches; a larger one answers `mcp_app_unsupported`. A view's
tool result has no new limit: it is the same connection and the same MCP path a model tool call already
uses, so a limit that applied to one path only would be a second, unexplained contract for the same
connection.

### D12. Refusal, unknown outcome and the tool's own failure are three different answers

A host decides what to do next from what the daemon can guarantee, so the three outcomes are separate:

- **Refused before the request was handed to the connection.** Nothing reached the server, so nothing
  ran, and the view may retry once the cause is gone. A determinate refusal with a name of its own uses
  that name (`mcp_unknown_server` for a call, `mcp_app_absent`, `mcp_app_not_callable`,
  `mcp_app_unsupported`, `mcp_unavailable`); one without such a name — the request was interrupted while
  the connection was still being opened — is `mcp_app_refused`.
- **Handed over and no outcome received.** The request reached the connection and the daemon did not
  get the answer: the client's request was interrupted after the hand-off, or the Session, the entry or
  the generation ended while the call ran. That is `mcp_app_unknown`, and the view must treat the call
  as possibly executed. It must not retry blindly: only the server can say whether the tool ran, so the
  view shows the uncertainty and the user decides.
- **Ran.** The reply is the tool's result, including one with `isError: true`, which is the server's
  answer and not a failure of the call.

`mcp_app_refused` and `mcp_app_unknown` replace `operation_interrupted` for these methods, because the
client's next step depends on which side of the hand-off the request was on; a single code that warns
"may already have committed" would make every retry an unbounded risk. The hand-off is the moment the
daemon asks the connection to send `tools/call`; everything before it — resolving the entry, finding
the tool, checking visibility, opening the connection — is a refusal, and everything after it is
unknown if the answer does not arrive.

## 4. Contract

Session methods named `session.mcp.app.widget` and `session.mcp.app.call`. Both are **exact built-in
Session methods**, registered as `session.mcp` is: Rpc decodes them itself, and a Session namespace that
equals, contains or lies inside either name is refused when it registers
([Session boundary A.1](../contracts/session-boundary.md#a1-method-contributions), Reserved names). Their
names are advertised in `hello.features` with the methods that require local execution.

Admission is `session.mcp`'s — execution-session methods, so the bare WebSocket answers
`local_transport_required` and the owner-only local transport and the authenticated loopback web entry
are served ([Session boundary A.3](../contracts/session-boundary.md#a3-entries-and-export)) — with one
narrower rule:

**A connection carrying a `deviceId` is refused.** A paired device gets `local_transport_required` for
both names, which is narrower than the execution Session methods
[Session boundary A.4](../contracts/session-boundary.md#a4-migration-and-compatibility) E3 makes available
to paired devices. Rendering a third-party document on another device and forwarding its calls through
one of the user's enabled entries has no basis yet: the document's sandbox and CSP would be that
device's, the daemon cannot see which device shows the view, and the call would run on the user's
entries from a device the user may not be holding. Serving them there is a decision with its own
boundary, taken when RemoteGateway lands, not an extension of this one.

`sessionId` is required everywhere; `server` is the entry id `session.mcp` reports, and only
`session.mcp.app.call` takes it.

| Method | Request | Reply |
| --- | --- | --- |
| `session.mcp.app.widget` | `{sessionId, tool}` | `Widget` (§4.1) |
| `session.mcp.app.call` | `{sessionId, server, tool, arguments}` | `CallResult` (§4.2) |

Neither method carries a branch, and neither changes process or Session state beyond opening the
entry's connection as any request on it would.

### 4.1 Mount read (`session.mcp.app.widget`)

The request carries no entry: the published tool name is all a client has when it reads a `tool/call`
fact, and resolving that name to its entry is this method's job (D3, D4).

| Field | Type | Meaning |
| --- | --- | --- |
| `tool` | string | The published tool name from the `tool/call` fact |
| `server` | string | The entry the tool belongs to: the id `session.mcp` reports, and what `session.mcp.app.call` takes |
| `name` | string | The server's own tool name: what the view's `tools/call` sends |
| `description` | string or absent | The tool's description, as declared |
| `inputSchema` | object | The tool's input schema, for `hostContext.toolInfo` |
| `uri` | string | The declared `ui://` resource URI |
| `mimeType` | string | `text/html;profile=mcp-app` |
| `text` | string or absent | The document, when the server returned it as text |
| `blob` | string or absent | The document, base64, when the server returned it as a blob; exactly one of `text` and `blob` is present |
| `app` | object, absent when the resource declares no `_meta.ui` | `csp?: {connectDomains?, resourceDomains?, frameDomains?, baseUriDomains?}`, `permissions?: {camera?, microphone?, geolocation?, clipboardWrite?}`, `domain?: string`, `prefersBorder?: boolean`, each as declared |

```text
client (host)                     Session                       MCP entry
  | session.mcp.app.widget {tool}    |                              |
  |--------------------------------->| published tool name -> entry |
  |                                  |   and the server's tool name |
  |                                  | resources/read {uri}          |
  |                                  |----------------------------->|
  |                                  |<-- {contents:[...], _meta} ---|
  |<-- Widget {server, name, uri,    |                              |
  |            text|blob, app, ...}  |                              |
  | cache it under (server, version from session.mcp)              |
```

### 4.2 View tool call (`session.mcp.app.call`)

| Field | Type | Meaning |
| --- | --- | --- |
| `server` | string | The entry, from §4.1 |
| `tool` | string | The server's own tool name |
| `arguments` | object | The call's arguments, from the view |

The reply is the result the connection returned, in the shape the Timeline uses for a tool result:

| Field | Type | Meaning |
| --- | --- | --- |
| `content` | string or block array | The result's content, as `ToolResult.content` carries it |
| `structuredContent` | object or absent | The result's structured content; it is for the view, not the model |
| `isError` | boolean or absent | The server reported the call as failed; the call still happened |
| `meta` | object or absent | The result's `_meta`, which only this reply carries (§7) |

```text
client (host)                Session                       MCP entry
  | session.mcp.app.call {...}  |                              |
  |--------------------------->| entry of this Session? ready? |
  |                            | tool in its discovery with    |
  |                            |   "app" in visibility?        |
  |                            | tools/call {name, arguments}  |
  |                            |-------------------------------->|
  |                            |<-- {content, structuredContent, isError, _meta}
  |<-- CallResult -------------|                              |
  | log (session, entry, tool, outcome, duration)              |
```

The call is tracked by the connection's reply. An interruption before the request is handed to the
connection answers `mcp_app_refused` and nothing ran; after the hand-off, an interruption, a Session
close, a disable or a replaced generation answers `mcp_app_unknown`, because the server may have run it
(D12). There is no per-call deadline (D5).

### 4.3 Errors

The two methods answer different sets: the mount read has no entry in its request, so
`mcp_unknown_server` belongs to the call only. Both share the connection, Session and admission codes.

**`session.mcp.app.widget`**

| `appError` | When |
| --- | --- |
| `mcp_app_absent` | The published tool name is not one the Session holds, or resolves to a tool that declares no widget resource |
| `mcp_app_unsupported` | The declared resource is not a `ui://` `text/html;profile=mcp-app` document, carries neither text nor blob, or is larger than the limit (D11) |
| `mcp_unavailable` | The tool's entry has no usable connection: disabled, connecting, failed, authorization required |
| `mcp_app_refused` | Nothing was read: the request was interrupted before the read was handed to the connection. No effect; retryable |
| `mcp_app_unknown` | The read was handed to the connection and its answer did not arrive. A read has no side effect, so retrying is safe here |
| `invalid_params` (`-32602`) | A missing or empty field, or a field of the wrong type |
| `local_transport_required` | The connection is the bare WebSocket, or carries a `deviceId` |
| `session_closed` / `service_reloading` / `service_unavailable` | From the Session and the row that serves it ([management methods](management-methods.md#dispatch-and-lifecycle)) |

**`session.mcp.app.call`**

| `appError` | When |
| --- | --- |
| `mcp_unknown_server` | `server` is not an entry of this Session's MCP selection. Nothing was sent |
| `mcp_app_absent` | The tool is not declared by that entry's current discovery. Nothing was sent |
| `mcp_app_not_callable` | The tool's `visibility` excludes `"app"`. Nothing was sent |
| `mcp_unavailable` | The entry has no usable connection: disabled, connecting, failed, authorization required. Nothing was sent |
| `mcp_app_refused` | The request was interrupted before the daemon handed it to the connection, for a reason no code above names. Nothing ran; retryable |
| `mcp_app_unknown` | The request was handed to the connection and the outcome was not received: the request was interrupted, or the Session, the entry or the generation ended while the call ran. It may have run; the view must not retry blindly (D12) |
| `invalid_params` (`-32602`) | A missing or empty field, or a field of the wrong type |
| `local_transport_required` | The connection is the bare WebSocket, or carries a `deviceId` |
| `session_closed` / `service_reloading` / `service_unavailable` | From the Session and the row that serves it ([management methods](management-methods.md#dispatch-and-lifecycle)) |

No code is borrowed from admission: the daemon never refuses a view's call because of a permission mode
or an approval, so it has nothing to report about one. `mcp_app_refused` and `mcp_app_unknown` replace
`operation_interrupted` for these methods (D12).

## 5. Security boundary

A view is third-party HTML running in a sandbox the daemon does not control. Everything it may reach
through this contract is fixed by one binding, and nothing else is reachable.

- **One entry, one generation.** A view is mounted from a published tool name and belongs to that
  tool's entry. Its calls name that entry and run on the connection the Session holds for it now (D5),
  and no cross-entry and no cross-server call exists in this contract. The daemon cannot see which view
  a host forwards for, so same-entry forwarding is a host obligation (D7); what the daemon can see, it
  enforces.
- **No relay to another device.** A connection carrying a `deviceId` is refused at admission (§4), so
  neither a view's document nor a view's call crosses to a paired device. Rendering a third-party
  document elsewhere and forwarding its calls through the user's enabled entries would run on this
  daemon's connections from a device the daemon cannot identify, so it is refused by default until
  RemoteGateway decides otherwise.
- **Fail closed.** An entry that is not enabled, not `ready`, or whose connection is gone answers
  `mcp_unavailable` and nothing was sent. Nothing falls back to another entry, another tool or another
  generation, and nothing is retried against a server the view did not come from. A connection that is
  replaced while a call is in flight is the other case: the call answers `mcp_app_unknown` and the view
  treats it as possibly executed (D12).
- **Credentials stay in the connection.** The daemon is the MCP client: headers, bearer tokens, OAuth
  tokens and endpoints live in the connection and are never returned by this surface — not to a host
  and not to a view. A view's HTTP calls to the outside are governed by the document's declared CSP,
  which the host enforces; the daemon is not a proxy for them.
- **Bounded payloads.** The document is capped (D11) because the daemon hands it to a sandbox; a call's
  result is the same MCP payload a model tool call already accepts, with the same behavior, and the
  `_meta` a result carries is returned to the view only.
- **The view is not trusted with the daemon's authority.** It cannot name a resource outside its
  document (D10), cannot make the daemon read files, cannot see another Session's entries, and cannot
  ask for a tool its server did not declare callable by an app (D7).
- **Authority is the enabled entry.** A view's call is the user's action on an MCP server they enabled,
  not an agent decision, so `Permission` and `Approval` do not gate it (D5; ADR 003 D1–D3). Enabling
  the entry is what authorizes the server's tools, exactly as it authorizes the server's startup.
- **The host shows it.** Because the conversation does not record a view's call (§8), the client must
  show in its own UI that the app issued the action and which entry it belongs to, next to the view.
  That is a host obligation of this contract, not a client nicety: without it the user cannot tell an
  app's action from the agent's.

## 6. What changes for a client

- A host mounts a view from a `tool/call` fact: it reads `session.mcp.app.widget` with the tool name it
  has, and caches the reply under the entry's `version` from `session.mcp`. It re-reads when that
  version changes — the discovered tools changed **or the connection was re-established** (D3) — or when
  the entry stops being `ready`.
- It renders the document under the `app` metadata it received, sandboxed as the specification
  requires. Building the CSP, the `allow` list and the host context is the client's work and is not
  part of this contract.
- `tools/call` from the view becomes `session.mcp.app.call`, addressed to the entry of the document it
  mounted and never to another entry. Whether the host asks the user first is the host's own policy;
  showing that the app issued the action is its obligation (§5).
- A host treats `mcp_app_refused` as "nothing ran" and `mcp_app_unknown` as "it may have run": the first
  may be retried once the cause is gone, the second is shown to the user as an unknown outcome (D12).
- `resources/read` from the view is answered from the mounted document; any other URI is a refusal the
  client produces itself (D10). `ui/update-model-context` is refused by the client (D9).
- `ui/message` becomes `session.submit` and is validated as any user input is. Whether the user is
  asked to confirm is the client's decision.
- A connection with a `deviceId` cannot mount a document or call one (§4, §5): a remote client renders
  no views until RemoteGateway decides its own boundary.
- The Desktop client (`arcships/dim-electron#20`) replaces its local MCP Apps host's document read and
  tool call with §4 and keeps its sandbox, bridge and rendering.
- The native CLI and the Web workspace render no views. Their MCP tool results keep their text content,
  so a session they view is unaffected; the web entry is served the methods but its phase-1 UI mounts
  nothing.

## 7. What does not change

- `session.mcp`, `session.mcp.refresh`, `mcp.*`, `mcp.auth.*` and the probe keep their requests and
  replies. A view's state is read from `session.mcp`; this design adds no per-entry field there. The one
  meaning that narrows is the per-entry `version`: it also changes when the connection is
  re-established (D3), which a client already treats as a new snapshot.
- The two names are added to the built-in method list, so `hello.features` gains them under the same
  conditions as `session.mcp` and a Session namespace cannot shadow them (§4); no existing name moves.
- The agent's tool loop is untouched: the same `tool/*` facts, the same admission, the same model
  context. A view's call cannot be confused with one, because it is a different method.
- No method is added to a management namespace, so no client needs a new capability check beyond the
  names in `hello.features`.
- The MCP business documents, entry sources, credential store and tool publication keep their formats;
  the tool name a Session publishes is unchanged, so no client's naming breaks.
- No Timeline event is added or changed. In particular the `tool/call` and `tool/result` facts, and the
  shape of a tool result, stay as they are: the tool result's `_meta` is not persisted.
- No new plugin, part, service, port or notification; the MCP Session part, its connections and the
  assembly are untouched.

## 8. Events and state

None in the Timeline. A view's call writes no fact and changes no persisted state: the only record is
the process log entry of D10, which carries identifiers, the outcome and the duration. The discovered
tools a Session keeps — including the ones not published to the model — are memory state rebuilt from
`tools/list` on every connection, so a reload or a restart loses nothing. The client owns everything
else: the mounted document, the sandbox, the view's own state.

## 9. Dependencies

| Dependency | Use |
| --- | --- |
| `McpSession` ([MCP OAuth](mcp-oauth.md), [MCP configuration management](mcp-management.md)) | The Session's entries, their current connection generation and their discovered tools |
| `session.submit` ([Session boundary](../contracts/session-boundary.md)) | The view's message as a validated user input (D9) |
| MCP client (`rmcp`) | `resources/read` beside the existing `tools/list` and `tools/call` |
| The App Server's process log (`tracing`) | The audit entry of D10 |

## 10. Cross-module requirements

None. Every operation is served by the owner that already holds the state: the MCP Session part for the
connection, and the existing input path for a message. No requirement is raised to the runtime, the
Timeline, the Dispatcher, the input mechanism, `Permission`, `Approval` or rutis.

The two capabilities this design does not provide are decisions, not gaps in another owner:
`ui/update-model-context` conflicts with the finality of an adopted input (D9), and a resource read
beyond the mounted document is not served at all (D10).

## 11. Tests

Flow level, in `crates/dim-app-server/tests`, reusing the Node MCP fixtures under
`crates/dim-app-server/tests/fixtures/` (they already run the official SDK):

| File | What it protects |
| --- | --- |
| `mcp_session_flow.rs` | A widget tool's document is mounted from its published tool name and names its entry and the server's tool; a tool that declares no resource is `mcp_app_absent`; a document that is not a `ui://` `text/html;profile=mcp-app` or is larger than the limit is `mcp_app_unsupported`; a tool whose visibility is `["app"]` is absent from the Session's tool list but answers `session.mcp.app.call`, and one whose visibility is `["model"]` answers `mcp_app_not_callable`; a call naming an entry the Session does not hold answers `mcp_unknown_server`; a call on a disabled or unauthorized entry answers `mcp_unavailable`; a call interrupted after the fixture recorded the invocation answers `mcp_app_unknown` while one interrupted before the connection was handed the request answers `mcp_app_refused` with no invocation recorded; neither reply carries a header value, a bearer token, an OAuth token or an endpoint URL; a new connection generation changes the entry's `version`, so the mounted document is re-read |
| `transport_flow/product_protocol.rs` | The wire contract: both methods are advertised in `hello.features`, refused with `local_transport_required` on a bare entry and on a connection carrying a `deviceId`, and answer the error codes of §4.3; a Session namespace equal to `session.mcp.app.widget`, `session.mcp.app.call`, or containing or lying inside either, is refused at registration because both are exact built-ins; a view's call leaves no `tool/call`, `tool/start` or `tool/result` fact and does not enter the next turn's context; a `read-only` Session's view call is not refused by the daemon |

## 12. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (commands for another owner's state) | No command is written: the call is a request on the caller's own connection and the MCP owner answers it; nothing is instructed to another owner |
| §4 rule 2 (handover) | Not applicable: no object is handed between modules; `ui/message` uses the existing input handover |
| §4 rule 3 (`before` callbacks) | None |
| §4 rule 4 (core without business meaning) | No core change; the runtime, Timeline and Dispatcher gain no MCP Apps handling |
| §4 rule 5 (batch) | Not applicable: no batch is written |
| §4 rule 6 (model context) | A view's tool result does not enter the model context: it is a reply to the client that asked, and no `tool/result` fact is written |
| §4 rule 7 (public events and services only) | The MCP Session part exposes the entry, its tools and its connection through its public service; the App Server reads nothing of another part's internal state |
| §4 rule 8 (infrastructure unchanged) | No change to the Timeline, Dispatcher, event envelope, assembly, lifecycle or the Session and branch facades; no requirement is raised |
| §4 rule 9 (only facts later decisions use) | No new fact at all; the record is the process log, not the Timeline |
| §1 (Capability and the App Server boundary) | No new part and no new Capability: the MCP owner keeps the connection and the discovery, and the client keeps rendering |
| ADR 002 D1 (scope follows the owner of state) | The connection and its discovery are Session state; the document and the sandbox are the client's |
| ADR 002 D2 (no wiring modules) | No new plugin, port or adapter; the two methods are added to the existing Session dispatch and the existing MCP owner |
| ADR 002 D3 (dependencies point to the core) | No dependency changes; `rmcp` and the Session facade are already used |
| ADR 002 D4 (structural versus business configuration) | No configuration is added; the daemon's extension declaration (D8) is process behavior, not a stored setting |
| ADR 002 D5 (language-neutral contracts) | The requests and replies are JSON data; the resource URI, MIME type and `_meta.ui` fields keep the specification's spelling |
| ADR 003 D1 (approval covers agent decisions) | Applied as written: a view's call is the user acting on an enabled entry, not the agent deciding, so `Permission` and `Approval` do not apply; the agent's own tool calls are unchanged |
| ADR 003 D3 (project entries start disabled) | Unchanged: a view of a disabled or unauthorized entry has no connection, so both methods answer `mcp_unavailable` |

## 13. Alternatives considered

| Alternative | Why not |
| --- | --- |
| Route a view's call through the agent's tool loop | It would need a turn and a request batch, write `tool/*` facts and put the result into the model context; the view's interaction is not the model's decision |
| Give a view's call the branch's `Permission` and `Approval` | It treats the user's action inside an enabled entry as an agent decision, and makes a view unusable in a `read-only` session for no safety gain: the view calls the tools the entry already exposes |
| Restrict a view to `visibility: ["app"]` tools | Stricter than the specification's default, and it refuses the ordinary case of a view refreshing the data of the tool that rendered it, for every server that does not annotate its tools. It is a policy that can tighten D7 later without changing the contract |
| A management namespace `mcp.apps` | The connection and its discovery are Session state; a process-scoped method reaching into a Session is the wiring this repository removes |
| A widget handle: `app.open` returns a token, later calls carry it | State to create, replace and expire, and cleanup on several owners' lifetimes; the entry id and the tool name already identify the view, and the client's cache key is the version `session.mcp` reports |
| Let the client open its own MCP connection for the view | Duplicates daemon-owned configuration, credentials and OAuth state, and lets one entry run two connections with different authorization |
| Hand the view the entry's credentials or endpoint | A view is untrusted HTML from a third-party server; the daemon is the client, and nothing on this surface returns a header, a token or a URL |
| Advertise the extension only while a widget-capable client is attached | Connections exist before and independently of the viewers; the declaration changes only what a server offers, and every widget tool keeps text content |
| Carry widget metadata in `session.mcp` | That state is subscribed to as a notification; a per-entry tool list belongs to a read on demand, and the existing `version` already tells a client when to re-read |
| Persist the widget descriptor and the result's `_meta` in the `tool/call` and `tool/result` facts | A frozen event format ([design rules §6](../../architecture/rust-agent/principles.md)) and a stored value that only presentation uses; the mount read answers from the current generation instead |
| Record every view call as a Timeline fact | No later decision reads it, and it would either enter the model context or need an event kind of its own; the process log is the audit, and a UI record is the client's product decision |
| Support `ui/update-model-context` as an adopted input | An adopted input is handed over once and is final, while the specification replaces the view's context on each update; changing the input mechanism for this would weaken the context rule |
| Expose `resources/read` by URI for a view | A view could read anything its server exposes, with no relation to what the host chose to render; the document a tool declares is the only thing a host needs |
| Prefetch every declared document on discovery | One request per tool for documents a client may never render; the client caches per version instead |
| Return the probe's tool list with descriptors | That reply belongs to [MCP configuration management](mcp-management.md); this design reads the descriptor in the Session, where a view lives |

## 14. Open questions

1. **Should the extension declaration be per daemon profile?** D8 declares it on every connection. A
   profile whose clients never render views could stop declaring it; that would need a setting, which
   is why this design does not add one.
2. **Do views render in the Web workspace?** Phase 1 renders none; the methods are served to its
   loopback connection already, so a later phase is a client change, not a contract change.
