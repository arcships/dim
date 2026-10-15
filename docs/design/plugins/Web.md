# Web

Status: Draft. Overall design: [overview](../overview.md) (Web). Phase: P2. Language: TypeScript first. Scope: [ADR 002](../../adr/002-plugin-scope.md) (Session).

Web is a Session plugin that gives the model `web_search` and `web_fetch` as two Session-scoped `toolContribution` services keyed by tool name ([tool contributions](../../architecture/rust-agent/tool-contributions.md); ToolRegistry accepts named tool services as a supplement, [ADR 002 D5](../../adr/002-plugin-scope.md#d5-contracts-are-language-neutral)). Search goes to a configured built-in search adapter; fetch downloads one public page and returns bounded text. Web holds no state shared across Sessions, writes no Timeline event (every call and result is already a tool call fact) and reads its business configuration from its Settings source `web` at use time, so editing it never reloads anything ([ADR 002 D4](../../adr/002-plugin-scope.md#d4-structural-configuration-and-business-configuration-are-separate)).

## 1. Structure

```mermaid
flowchart LR
  subgraph PROC["Process"]
    Settings["Settings: web business source, credential-reference resolver"]
  end
  subgraph SESS["Session"]
    Web["Web: toolContribution web_search, web_fetch"]:::new
    ToolRegistry["ToolRegistry"]
  end
  subgraph BR["Branch"]
    Exec["ToolBinding, ToolExecutor, Approval, Permission"]
  end
  Web -->|"read source web at use, resolve secretRef"| Settings
  ToolRegistry -.->|"attaches toolContribution during Session preparation"| Web
  Exec --> ToolRegistry
  Exec -->|"execute(ToolCall), cancel request"| Web
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
```

| Part | Scope | Depends on | Provides |
| --- | --- | --- | --- |
| `Web` | Session (installed only when enabled) | Settings source `web` and credential-reference resolver; diagnostics channel | `toolContribution` keys `web_search` and `web_fetch`, declared in Web's `provides` |

**Contracts depended on:** `toolContribution` (Appendix A.1); Settings business source and credential-reference resolver; `ToolDefinition.risk = "network"` read by Permission (existing, [permission.rs](../../../crates/dim-agent-capabilities/src/permission.rs)). **Contracts defined here:** tool names, parameters and result formats; the `web` business document. The diagnostics channel is an existing typed service. Settings sources exist today only through the Rust `BusinessDocument` binding; the language-neutral `settings` service and the credential-reference resolver are Settings supplements that are not implemented yet (S8b, [overview §6.4](../overview.md#64-conventions) and [Appendix B.2](../overview.md#b2-app-server-process-plugins)). Exposing them by name to the TypeScript implementation is issue [#4](https://github.com/arcships/dim/issues/4)'s work, stated in [session-access §9](../contracts/session-access.md#9-requirements-raised-to-mechanism-owners).

**Reload.** Web depends only on an enclosing scope (Settings). Enabling, disabling or upgrading Web, or a Settings reload, reloads Web only: ToolRegistry attaches Web's tools during Session preparation and does not depend on Web (session-boundary A.5, C3), so the Session keeps running and Web's tools are absent until its next reload; an in-flight call is settled by ToolExecutor as reloaded. This is the accepted cost of a rare structural operation ([ADR 002 D3](../../adr/002-plugin-scope.md#d3-dependencies-follow-scope-rare-restarts-are-accepted)). Configuration edits never reload.

**Node.** Requests leave from the App Server process that hosts the Session, not from the Session's resource node (A2).

## 2. Data and state

```mermaid
flowchart TB
  subgraph CFG["Business configuration (Settings source web)"]
    WebCfg["search_service, allow_private_hosts, proxy"]
    Secret["secretRef (reference only)"]
  end
  subgraph MEM["Web memory per Session (not persisted)"]
    Cache["Fetch cache, bounded (A9)"]
    Inflight["In-flight calls: abort handle each"]
  end
  subgraph TL["Branch Timeline (written by others)"]
    Calls["step/tool-call ... step/tool-result, approval/*"]
  end
  WebCfg -->|"read at call start"| Inflight
  Secret -->|"resolved per call"| Inflight
  Inflight -->|"one ToolResult per call, written by ToolExecutor"| Calls
```

Web owns no Timeline state and no `StateCell`. The cache starts empty when Web is applied; nothing is rebuilt from the Timeline.

## 3. Flows

### 3.1 A `web_fetch` call

```mermaid
sequenceDiagram
  participant L as Loop
  participant TL as Timeline
  participant X as ToolExecutor
  participant A as Approval and Permission
  participant W as Web handler
  participant N as Remote site
  L->>TL: step/tool-call
  TL-->>X: Dispatcher delivers, X registers batch task and returns
  X->>TL: append step/tool-start
  X->>A: admit risk network
  A-->>X: allow (or ask, deny)
  X->>W: execute(ToolCall)
  W->>W: validate URL, resolve host, check every address
  W->>N: GET pinned to the validated IP (direct, or CONNECT IP:port through proxy)
  N-->>W: body up to 5 MiB
  W-->>X: ToolResult header plus text slice
  X->>TL: step/tool-completed, then one step/tool-result for the batch
```

The handler runs on ToolExecutor's task and decides only from its arguments and the configuration read at call start. Longer pages are read with further calls using `start_char`; a cross-host redirect is returned as a notice so the new host gets its own admission (A10).

### 3.2 A `web_search` call

```mermaid
sequenceDiagram
  participant X as ToolExecutor
  participant W as Web handler
  participant S as Settings
  participant Q as Search service
  X->>W: execute(ToolCall: query, filters)
  W->>S: read search_service
  alt no usable service
    W-->>X: error search_unavailable
  else configured adapter
    W->>S: resolve secretRef (per call, never stored)
    W->>Q: query over HTTPS
    Q-->>W: hits
    W-->>X: numbered result list, domain filters applied by Web
  end
```

`web_search` is always declared when Web is enabled; it fails with `search_unavailable` until a usable service is configured (A3). Changing the service takes effect at the next call.

## 4. Timing and conflicts

```mermaid
sequenceDiagram
  participant TL as Timeline
  participant X as ToolExecutor
  participant W as Web
  Note over TL,W: Turn interrupt during a request
  TL->>X: step/tool-start
  X->>W: execute
  TL-->>X: stop/interrupt (seq after tool-start)
  X->>W: cancel request
  W-->>X: error result cancelled (socket closed)
  X->>TL: step/tool-result for the batch
```

| Scenario | Outcome | Owner |
| --- | --- | --- |
| Configuration edit during a call | Call continues with values read at its start; next call uses the new values | Web |
| Service removed between calls | Next `web_search` returns `search_unavailable` | Web |
| Web or Settings reload mid-call | The call is settled as reloaded; ToolRegistry and Loop keep running and Web's tools are absent until the Session's next reload (D3, C3) | ToolExecutor |
| Process restart mid-call | ToolExecutor settles the committed call as not surviving recovery; Web never repeats a request | ToolExecutor |
| Compaction after a result | Fetched text survives only through the summary | ContextPreparation |

Full table in Appendix B.

## 5. Context mapping

| The model sees | Form | Position |
| --- | --- | --- |
| `web_search`, `web_fetch` name, description, input schema | Tool declarations | Adopted `context/configured.tools` when listed in `SessionConfig.tools` |
| Search results, fetched text, errors, redirect notices | `ToolResult.content` (text only) | Tool result position of the call's batch |

- Compaction: declarations survive as configuration; results only through the summary.
- Child branches inherit results in their ancestor prefix.
- Non-interactive Sessions see the same tools and can use them; the tool set stays the same across Sessions (#30).
- Never visible: `structuredContent`, the service name, configuration, credential references, private-host exceptions, diagnostics.
- Fetched page text is untrusted third-party content placed in a tool result without instruction framing (A14).

## 6. Interfaces (summary)

| Interface | Kind | Purpose |
| --- | --- | --- |
| `web_search` | `toolContribution` (Session, key `web_search`), risk `network` | Query the configured service; numbered result list |
| `web_fetch` | `toolContribution` (Session, key `web_fetch`), risk `network` | Fetch one public `http(s)` URL; bounded text slice with pagination |
| `web` source | Business configuration in Web's Settings source, read at use | Search service, private-host exceptions, proxy |

No Session methods, no `managementMethods` and no CLI subcommand: configuration goes through Settings' client methods `settings.read`/`settings.update` (S8b, not implemented) and `/settings`. Details in Appendix A.

## 7. Security

- Admission: both tools declare `network`; every call passes Approval and Permission (read-only denies, workspace-write asks, FullAccess allows).
- SSRF: Web resolves names itself, checks every address against the blocked ranges and connects only to the checked address, with or without `proxy`; cross-host redirects need a new call and a new admission (A.4, A10).
- No ambient authority: `GET` only; no cookies, stored site credentials or client certificates; nothing from the Session except the URL the model wrote (A8).
- Credentials: references resolved per call; never in definitions, results, `structuredContent`, diagnostics or errors.
- Exfiltration through URLs is the inherent risk of the `network` category and is controlled by admission.
- With `proxy`, Web asks the proxy for an HTTP `CONNECT` to the validated IP and port, never to the hostname, so the proxy does not resolve the target name; TLS SNI and the `Host` header carry the original hostname. Pinning and private-address protection are the same as for a direct connection (A.4).

## 8. Rule check

| Rule | How this document complies |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | Web changes no other module's state; tool results are written by ToolExecutor, their owner. |
| Principles §4 rule 2 (handover) | Not applicable: Web takes over no object; it only runs calls ToolExecutor hands it. |
| Principles §4 rule 3 (before callbacks only) | No intervention before a write; argument checks are inside the tool handler. |
| Principles §4 rule 4 (no business meaning in core) | No core change; the tools use the generic `toolContribution` contract. |
| Principles §4 rule 5 (batch) | Not applicable: Web decides from call arguments and configuration, not from events. |
| Principles §4 rule 6 (context from handed-over events) | Results reach the model only as `step/tool-result` facts written by ToolExecutor. |
| Principles §4 rule 7 (public events and services only) | Web uses only `toolContribution`, the Settings business source and the credential resolver. |
| Principles §4 rule 8 (requirements to mechanism owners) | No runtime or rutis requirement of its own; the Settings supplements it uses are listed in [overview Appendix B.2](../overview.md#b2-app-server-process-plugins); #4 is stated in [session-access §9](../contracts/session-access.md#9-requirements-raised-to-mechanism-owners). |
| Principles §4 rule 9 (only facts later decisions use) | Web writes no event; its fetch cache is memory only. |
| Principles §1 (Capability and App Server boundary) | Web is a Session Capability in its own plugin package; App Server holds none of its code. |
| ADR 002 D1 (scope follows state) | Session scope: Web holds no state shared across Sessions. |
| ADR 002 D2 (no wiring modules) | No wiring module; search adapters are built in (A1). |
| ADR 002 D3 (dependencies follow scope) | Its only dependency, Settings, is in the enclosing scope; a Settings reload reloads Web, which is accepted. |
| ADR 002 D4 (structural vs business configuration) | Enabling is structural; the `web` document is business configuration in Web's own Settings source, read at each call. |
| ADR 002 D5 (language-neutral contracts) | Contracts are neutral; the Rust form is Appendix G. |

## 9. Requirements raised to mechanism owners

None. #4 (typed services by name for non-Rust parts) is stated in [session-access §9](../contracts/session-access.md#9-requirements-raised-to-mechanism-owners); the diagnostics channel is one of the public typed services its generic capability covers. The Settings business source and credential-reference resolver are Settings supplements listed in [overview Appendix B.2](../overview.md#b2-app-server-process-plugins).

## Appendix A. Interface details

### A.1 Tools

Each tool is one `toolContribution` service (Session scope, key = tool name):

| Method | Kind | Request | Reply |
| --- | --- | --- | --- |
| `declaration` | sync | — | `ToolDeclaration {name, description, inputSchema, risk: "network"}` |
| `execute` | async | `ToolCall {sessionId, branchId, turnId, callId, arguments, config}` | `ToolResult {content: [{type: "text", text}], isError, structuredContent?}` |

Cancellation is a cancel request for the call or the consumer dropping it; Web aborts the request and replies `cancelled` if it still replies. Errors are `ToolResult`s with `isError: true` (Appendix C), never call failures. Parameters are validated by the handler; a violation returns an error result without network access.

### A.2 `web_search`

| Parameter | Type | Rule |
| --- | --- | --- |
| `query` | string | Required; 1 to 500 characters after trimming (A4) |
| `max_results` | integer | Optional; 1 to 10; default 5 (A4) |
| `allowed_domains` | string[] | Optional; at most 10 hostnames; results restricted to these hosts and subdomains |
| `blocked_domains` | string[] | Optional; at most 10; mutually exclusive with `allowed_domains` |

Domain filters are passed to the service when supported and applied by Web to the returned list in every case. `ToolResult.content`:

```
Search results for "<query>" (<n> results)

[1] <title>
<url>
<snippet>

[2] ...
```

Titles and snippets are plain text, one line each, snippet at most 500 characters. Zero results yield `No results for "<query>".` (not an error). The service name is not shown. `structuredContent` carries `{ results: [{ title, url, snippet, published_at? }] }` for clients. Total time limit 20 s (A5).

### A.3 `web_fetch`

| Parameter | Type | Rule |
| --- | --- | --- |
| `url` | string | Required; absolute `http` or `https`; at most 2048 characters; no userinfo; fragment removed (A6) |
| `start_char` | integer | Optional; default 0; offset into the converted text |
| `max_chars` | integer | Optional; 1000 to 50000; default 20000 (A7) |

`ToolResult.content`:

```
URL: <final url>
Title: <title, if any>
Content-Type: <media type>
Characters: <start>-<end> of <total>
[Truncated: call web_fetch again with start_char=<end> to continue.]
[Download limit reached: the page exceeds <limit> and later content is missing.]

<converted text slice>
```

Bracketed lines appear only when they apply. `structuredContent`: `{ url, final_url, status, content_type, title?, start, end, total, truncated, download_limited }`.

| Item | Rule |
| --- | --- |
| Method and headers | `GET` only; `User-Agent: dim-web/<version>`; `Accept` preferring HTML and text; no cookies, credentials or referrer (A8) |
| Limits | Connect 10 s; whole call 30 s; at most 5 MiB decoded body, then the connection is closed and `download_limited` set (A5) |
| Status | 2xx converted; 3xx redirect rules; 4xx/5xx error `http_status` with code and reason phrase, no body |
| Redirects | Up to 5 hops, each revalidated. A redirect to a different host (lowercased, leading `www.` removed) is not followed: non-error notice `Redirected to <url>; call web_fetch with that URL to follow it.` (A10) |
| `text/html`, `application/xhtml+xml` | Charset from header, then `<meta>`, else UTF-8 with replacement; `script`, `style`, `noscript`, `template`, `svg`, `iframe`, navigation and form controls removed; main content preferred; Markdown with absolute links; whitespace normalized |
| `text/plain`, `text/markdown`, `text/csv`, `application/json`, `application/xml`, `text/xml`, `*+json`, `*+xml` | Decoded text as is (valid JSON pretty-printed) |
| Other types | Error `unsupported_content_type` naming the type (A11) |
| Empty conversion | Non-error result with header and `(no text content)` |
| Slicing | By Unicode scalar values `[start_char, start_char + max_chars)`; `start_char >= total` returns an empty slice with the header |
| Cache | Converted text keyed by final URL, per Session, 15 minutes, at most 50 MiB, so pagination does not refetch (A9) |

### A.4 Address policy

| Rule | Content |
| --- | --- |
| Resolution | Web resolves the hostname, validates every resolved address and connects only to a validated address (pinned), directly or through `proxy`, so DNS rebinding between check and connect is impossible |
| Blocked by default | Loopback (`127.0.0.0/8`, `::1`), unspecified, RFC 1918, link-local (`169.254.0.0/16`, `fe80::/10`, including cloud metadata), CGNAT `100.64.0.0/10`, unique local `fc00::/7`, multicast, broadcast, reserved ranges, IPv4-mapped and IPv4-compatible IPv6 forms of these; hostnames `localhost` and `*.localhost` |
| Literal IPs | Allowed only when public by the same rules |
| Ports | Any port on a public address (A12) |
| Exceptions | `allow_private_hosts`: exact hostnames or CIDRs; empty by default. A user-only field: written through the client method `settings.update`, never by a Web write (S8b). Settings checks only that it is an array of strings; Web parses each entry at use, ignores one that is neither a hostname nor a CIDR and reports it as a diagnostic |
| Proxy | With `proxy`, Web resolves and validates as above, then sends an HTTP `CONNECT <validated IP>:<port>` to the proxy for both `http` and `https`; TLS SNI and the `Host` header use the original hostname; the proxy never receives the hostname as the connect target. A proxy that refuses the `CONNECT` fails the call with `connect_failed` (A15) |

### A.5 Business configuration (Settings source `web`)

| Field | Type | Default |
| --- | --- | --- |
| `search_service` | `{ kind: "<built-in adapter id>", endpoint?: string, secretRef?: string }` or absent | absent (`web_search` returns `search_unavailable`) |
| `allow_private_hosts` | string[] | `[]` |
| `proxy` | string (URL) | absent |

Read at each call and used as delivered: Settings validates the document against the schema Web declares and applies its defaults on write and when reading a persisted file, and Web does not validate it again. Web has no tool that writes configuration, so the document is written only through the client methods (S8b). A change is a data update with notification, never a reload. Only Web's enablement is structural configuration. Built-in adapter ids are an implementation choice listed in the plugin README (A13). Credentials appear only as `secretRef`, an opaque non-empty reference resolved per call through the credential resolver.

### A.6 Timeline and capability mechanics

| Step | Path |
| --- | --- |
| Provide | Web's `provides` declares `toolContribution` `web_search` and `web_fetch`; ToolRegistry registers those listed in `SessionConfig.tools`; ToolBinding adopts them. Web writes nothing |
| Call | ToolExecutor consumes `step/tool-call` on the Dispatcher, registers the batch task and returns; per call `step/tool-start`, admission, handler on the executor's task, `step/tool-completed`, then one `step/tool-result` per batch |
| Dispatcher | Web has no subscription |
| State and recheck | No `StateCell`; no consumed facts; decisions use arguments and configuration read at call start |
| Terminal facts | Written by ToolExecutor; Web returns exactly one `ToolResult` per call |
| Reload | Unload aborts in-flight requests; ToolExecutor settles them as reloaded ([tool contributions §4](../../architecture/rust-agent/tool-contributions.md)) |
| Process restart | Committed calls without results are settled by ToolExecutor's rebuild; Web never repeats a request |

## Appendix B. Full conflict table

| Scenario | Sequence order | Outcome | Owner |
| --- | --- | --- | --- |
| Turn `stop/interrupt` before dispatch | interrupt < `step/tool-call` consumption | Not executed | ToolExecutor |
| Turn `stop/interrupt` during approval | `approval/request` < interrupt | `approval/cancelled`; no request sent | Approval |
| Turn `stop/interrupt` during the request | `step/tool-start` < interrupt | Call cancelled; socket aborted; error `cancelled`; a response fully converted before the abort is returned as is | ToolExecutor, Web |
| Branch-scoped interrupt or branch close | branch interrupt | As turn interrupt | ToolExecutor |
| Batch with another long call | Web returned, other call running | Result held until the batch's `step/tool-result` | ToolExecutor |
| Concurrent Steer/Queue/Append | any | No effect on the call; Steer consumed after the batch result | Loop |
| `input/withdraw` / `input/deleted` of a held input | any | No effect; Web consumes no inputs | InputQueue |
| Compaction after the result | `step/tool-result` < compaction | Text survives only via the summary | ContextPreparation |
| Child branch cut | cut after `step/tool-result` | Child inherits the result | — |
| Configuration edit while a call runs | — | Call continues with values read at its start | Web |
| `web_search` with no usable service | — | Error `search_unavailable` | Web |
| Web or Settings reload mid-call | no result yet | Call settled as reloaded; dependents reload (D3) | ToolExecutor |
| Process restart mid-call | committed, no result | Settled as not surviving recovery; not repeated | ToolExecutor |
| Session unload and restore | `branch/resume` | Web applied again; cache empty | Lifecycle |
| Session resource node offline | — | No effect; Web uses no node services | — |
| Late handler call after cancellation | cancel request already received | Returns `cancelled` without network access | Web |
| Two branches fetch the same URL | — | Independent requests; cache filled by the first to finish | Web |
| Client detach | — | No effect | — |

## Appendix C. Errors and recovery

Every error is an error `ToolResult` (`isError: true`) with content `web_fetch failed: <code>: <message>` or `web_search failed: ...`. No error affects other calls or the Session.

| Code | Meaning |
| --- | --- |
| `invalid_input` | Parameter violates Appendix A |
| `blocked_address` | Host or a redirect hop resolves to a blocked address; the message names the rule only |
| `dns_failed`, `connect_failed`, `tls_failed` | Network errors; `connect_failed` includes a proxy refusing the `CONNECT` |
| `timeout` | Connect or whole-call limit reached |
| `http_status` | Non-2xx final status |
| `too_many_redirects` | More than 5 same-host hops |
| `unsupported_content_type` | Type not in A.3 |
| `search_unavailable` | No service configured, adapter unusable, or credential unresolvable |
| `search_failed` | Service error or invalid data; message shortened and stripped of secrets |
| `cancelled` | Call cancelled |

Recovery: Web holds no durable work; reload and restart lose only the cache.

## Appendix D. Acceptance

Flow tests through public interfaces: tool calls in a test Session, a local HTTP server reachable only through a test `allow_private_hosts` entry, a fake search adapter.

1. Without a service `web_search` returns `search_unavailable`; configuring one makes the next call succeed with no reload; removing it fails the next call.
2. `web_search` format, domain filters, zero results, service error.
3. `web_fetch` HTML header and Markdown; `start_char` pagination to the end; `max_chars`; download limit notice.
4. Plain text, JSON, unsupported type, 404, empty page.
5. Same-host redirects up to 5; sixth fails; cross-host redirect returns the notice.
6. Loopback, private, link-local, IPv4-mapped IPv6, `localhost` and a private-resolving hostname blocked; DNS rebinding test not connected, both directly and with `proxy` (the test proxy receives only `CONNECT <validated IP>:<port>`, with SNI and `Host` set to the hostname).
7. Permission: read-only denies, workspace-write asks, FullAccess allows.
8. Interrupt before dispatch, during approval and during a slow response (socket closed, one result).
9. Restart settlement repeats no request.
10. Non-interactive Session declares both tools.
11. Diagnostics and results never contain the resolved credential.

## Appendix E. Assumptions

| # | Assumption | Basis | Effect if rejected |
| --- | --- | --- | --- |
| A1 | Search services, including account-backed ones, are built-in adapters; no backend registry | ADR 002 D2 | A search-backend contract owned by Web |
| A2 | Requests leave from the App Server process hosting the Session | Judgment | Network access through the resource node's services |
| A3 | `web_search` is always declared and fails when no service is usable | ADR 002 D4 (configuration never reloads) | Declaration would follow configuration |
| A4 | Query length 500; `max_results` 1 to 10, default 5 | Judgment | Limits change |
| A5 | 10 s connect, 30 s fetch, 20 s search; 5 MiB download | Judgment | Limits change |
| A6 | Only `http`/`https`, at most 2048 characters, no userinfo | Judgment | Wider input surface |
| A7 | `max_chars` 1000 to 50000, default 20000, `start_char` pagination | Judgment | Different bounding of results |
| A8 | `GET` only, fixed user agent, no cookies or credentials | Judgment | Authenticated fetching needs its own design |
| A9 | Cache per Session, 15 minutes, at most 50 MiB, keyed by final URL | Judgment | Each slice refetches; slices may differ |
| A10 | Cross-host redirects reported, not followed | Judgment | Redirects reach hosts never admitted |
| A11 | PDFs and images not converted | Judgment | Add extraction or image content blocks |
| A12 | Any port on public addresses | Judgment | Restrict to 80/443 |
| A13 | Built-in adapter list is an implementation choice | Overview names no service | A service is fixed by the overview |
| A14 | No framing text around fetched content | Judgment | Model-visible framing to define |
| A15 | A configured proxy supports HTTP `CONNECT` to an IP address for both `http` and `https` targets | Maintainer decision: one pinning guarantee with and without a proxy | Proxies that only accept hostnames cannot be used; private-address protection would depend on the proxy |

## Appendix F. Open questions

| Question | Description |
| --- | --- |
| Settings by name | The TypeScript implementation needs the Settings source, credential-reference resolver and diagnostics channel exposed by name (the Settings parts are S8b; [#4](https://github.com/arcships/dim/issues/4), stated in [session-access §9](../contracts/session-access.md#9-requirements-raised-to-mechanism-owners)); `toolContribution` itself is neutral |

## Appendix G. Rust binding

Used when Web moves to Rust; the tool names, data shapes and loader row stay unchanged.

| Neutral | Rust |
| --- | --- |
| `toolContribution` keys `web_search`, `web_fetch` | Session `ToolContribution` registered with `AgentBuilder::session_tool("web_search")` and `("web_fetch")`; ToolRegistry collects `Contributions<ToolContribution>` |
| `declaration` / `execute` | `ToolDefinition` / handler with `CancellationToken`; the disposer aborts in-flight requests ([tool contributions §4](../../architecture/rust-agent/tool-contributions.md)) |
