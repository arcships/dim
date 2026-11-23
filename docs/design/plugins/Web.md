# Web

Scope: Session. Issue: #2033 (this contract). The tool parameters, result formats, address policy, error codes and acceptance list are Appendix A–E below and are unchanged; this part states the owner's shape and the calls it makes. The current shape is the one #2033's scan report describes.

Web owns two network call intents — read one public page, search once — the policy they run under, the converted text one Session keeps between page calls, and the rule that decides whether `web_search` is declared. Web provides no service: its public surface is the two tool contributions.

## Use and scope: responsibility split

Web owns:

- **The two calls.** A page read under the address and content policy (Appendix A.3, A.4) and a search under the query rules (A.2), each with its own time limit and cancellation.
- **The declaration.** `web_fetch` is always declared while the part is Active; `web_search` is offered only while a search service is usable, composed from the `web` Settings source and the account owner's availability subscription (A.1, A.3 of the tool contributions page).
- **The converted-text cache.** One Session's converted pages, keyed by final URL, bounded by age and bytes, so a later page of the same text needs no second request (A9).
- **The result text.** Headers, truncation notices, redirect notices and error codes the model acts on (Appendix A.2, A.3, C).

Callers keep as business:

- Whether a call may run: Permission and Approval decide, and Web never admits anything itself.
- What a result means for the conversation; Web returns tool results only.
- Which tools a Session mounts: `SessionConfig.tools` is SessionScope's record and ToolRegistry's rule.
- The search service and its key: the operator writes them through `settings.*`; Web reads and never writes them.

| Requirement (caller) | Can callers compose it from the owner's operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Read one public page as bounded text | No: the address policy, the conversion and the byte limits are the owner's | `web_fetch` tool contribution |
| Search once, with domain filters | No: the query rules, the service selection and the account fallback are the owner's | `web_search` tool contribution |
| Continue a long page without fetching it again | No: only the owner keeps the converted text across calls | the owner's cache; `start_char` on the next call |
| Declare `web_search` only while a service is usable | No: the availability subscription is the account owner's and the declaration is this owner's | the part's tool source, re-evaluated on a committed `web` write and on an availability change |
| Read the current configuration | No: the source and its defaults are Settings' | Settings' `read` at use |
| Decide whether a call may run | Yes: Permission and Approval decide | business, in ToolExecutor |
| Show or judge a result | Yes: the result is text on the call's fact | business |

An operation or parameter exists only where its row says No.

## Caller example

A tool call is one contribution; the handler receives data and a cancellation and returns the model-visible result. No other owner's object appears.

```rust
// Registered from the part's apply: a fixed tool.
tools.register(ctx, ToolContribution {
    definition: fetch_definition(),          // name, description, schema, risk network
    handler: Arc::new(WebFetch { settings: settings_id.clone(), cache: cache.clone() }),
})?;
// And the dynamic source: one set holding web_search while a search service is usable.
tools.register_source(ctx, "web", Arc::new(SearchSource { /* ... */ }))?;
```

```rust
// One fetch: read the source, apply the policy, convert, answer a slice.
let document: WebDocument = settings.read(settings_id.clone(), Scope::User, cancelled.clone()).await?.document;
let page = self.cache.page_or_fetch(&request.url, &document, cancelled).await?;
Ok(page.slice(request.start_char, request.max_chars))
```

```rust
// One search: the account owner's search when the source names no adapter.
let reply = account.search(SearchRequest { query, count }, cancelled).await?;
Ok(numbered(&reply.results, &request))
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| Settings ([#2017](https://github.com/arcships/dim/issues/2017)) | `register`, `read` | The `web` source: search service, private-host exceptions, proxy, proxy DNS; read at each call |
| `dimAccount` (DimOAuth, [#2019](https://github.com/arcships/dim/issues/2019), optional) | `availability`, `subscribe`, `search` | The relay's sign-in state, its changes, and the relay search when the source names no adapter |
| Session registries ([#2026](https://github.com/arcships/dim/issues/2026)) | `toolRegistry.register`, `toolRegistry.register_source` | The fixed `web_fetch` contribution and the dynamic `web_search` source |

The account owner is read by its face name when the source names no search adapter. A profile without an account owner declares `web_fetch` only; a profile with one declares `web_search` while that owner reports a usable account.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `web_fetch` | Read one public `http(s)` page as bounded text | `{url, start_char?, max_chars?}` | `ToolResult.content`: the header block and the text slice; `structuredContent` for clients (A.3) | The call stops: the request is aborted and the socket released before the result is written | error codes as `ToolResult.isError` (Appendix C) |
| `web_search` | Query the configured service | `{query, max_results?, allowed_domains?, blocked_domains?}` | `ToolResult.content`: the numbered result list; `structuredContent` for clients (A.2) | As `web_fetch` | error codes as `ToolResult.isError` (Appendix C) |
| declaration | Offer `web_search` while a service is usable | — | the tool source's current set and its version | — | — |

The tool error codes are business results the model reads and acts on: `invalid_input`, `blocked_address`, `dns_failed`, `connect_failed`, `timeout`, `http_status`, `too_many_redirects`, `unsupported_content_type`, `search_unavailable`, `search_failed`, `settings_unavailable`, `cancelled` (Appendix C). A failed call changes nothing else and affects no other call.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `web_fetch`, `web_search` | Read the source, run the call under the policy, convert, answer one result | The request is aborted, the socket released, the call answers `cancelled`; a response fully converted before the abort is returned as it is | The part's tracker stops and joins the in-flight call before the part unloads | The part's close aborts in-flight calls; ToolExecutor settles them as reloaded; the next generation registers the tools again | the calling row: the converted text and the socket; ToolExecutor: the call's terminal fact |
| Declaration | The source offers `web_search` while the account reports a usable service; a committed `web` write or an availability change re-evaluates it | — | The registration goes with the part | Reload withdraws the registrations and the next generation offers the current set | Web: the offer; ToolBinding: which declaration a request adopts (A.1) |

### Errors

The tool result codes are listed above and detailed in Appendix C. Web has no operation-level errors: it provides no service, and every failure is a result the model reads.

## Exceptions with reasons

- Web reads the account owner's availability and follows its changes instead of failing the model's call: the declaration must follow a sign-in another process wrote, and a subscription is the account owner's own value.
- The converted-text cache is Web's own projection with its own freshness bound (15 minutes, 50 MiB, final URL): pagination is the only reader, and no other owner can answer for a page Web converted.
- A tool registration is a callback by reference on the mechanism's registration point, which §7 names as not a forwarding facade; Web keeps no second interface beside it.
- `web_fetch` is declared even when no search service is usable, and stays declared while the page path works: the two tools have independent availability.

## Verification

The acceptance list is Appendix D. It covers the declaration following a service change without a reload, the result formats and domain filters, pagination and the download limit, redirects, the address policy, permission, interrupt settlement, restart settlement, non-interactive Sessions and secret redaction.

## Supersedes

The Chinese mirror `docs/zh/design/plugins/Web.md` describes the pre-contract shape; its target is this document.

None outside this document: the tool parameters, result formats, address policy, error codes and acceptance in Appendix A–E stay in force, and the previous version of this file's structure sections are replaced by the sections above.

## Client-visible changes

None. Both tools keep their names, schemas, descriptions, results and error codes; the search service still follows `settings.read` and the account's availability, and the declaration timing is the one [#1977](https://github.com/arcships/dim/pull/1977) merged.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part. It writes no Timeline event and folds no state, so it is not a Capability. |
| §2 scopes | Session: the cache and the registration live with the Session; nothing is shared across Sessions. |
| §3 orthogonality | Web reads the source, the account answer and the call's arguments; it returns only its own results. |
| §4 rule 1 (commands) | No command event: results are written by ToolExecutor as the call's fact. |
| §4 rule 2 (handover) | The call is ToolExecutor's; Web returns one result per call. |
| §4 rule 3 (before callbacks) | None registered. |
| §4 rule 4 (no business meaning in core) | Tool names, schemas and result text are Web's. |
| §4 rule 5 (batches) | No batches. |
| §4 rule 6 (context) | Results reach the model only as the call's `ToolResult`. |
| §4 rule 7 (public events and services) | Settings, the optional account face and the tool registry; no subscription of its own besides the account's availability. |
| §4 rule 8 (requirements) | None; the account's availability, subscription and search are defined by the account owner's contract. |
| §4 rule 9 (facts) | Web writes no event; the cache is memory only. |
| §7 values, ids and callbacks | Arguments and results are data; the tool handler is a callback by reference on the registration point; no object crosses. |
| §7 no proxy lookup | Web returns its own results; the account's state and search come from the account owner. |
| §7 one interface | One tool contribution per tool, one dynamic source; no service, no adapter. |
| §7 cross-language | Both tools are declared with data schemas and their results are data. |
| §7 one change | Replacing the search face with the account owner's operations happens in the same change that migrates the declaration and the call. |
| ADR 002 D1 | Session scope: no state is shared across Sessions. |
| ADR 027 D2 | Web resolves no service by id: Settings and the account owner are process dependencies declared at load, and no registration point is reached by id. |
| ADR 002 D2 | No wiring plugin: the tools are on the part; the source is the registry's registration point. |
| ADR 002 D3 | Dependencies point from Web to Settings, the account owner and the registry. |
| ADR 002 D4 | The `web` document is business configuration read at use; the plugin's on/off state is structural. |
| ADR 002 D5 | The contract is language-neutral; the Rust binding is the appendix below. |

## Requirements raised to mechanism owners

None. The account owner's `availability`, `subscribe` and `search` and the tool registry's registration points cover the calls above.

## Binding appendix

The part registers one fixed contribution and one dynamic source on `ToolRegistry`; there is no service name of its own. Appendices A–E are the operation details and stay in force.

```rust
/// `SettingsId` is the id `settings.register` answered for the `web` source.
#[derive(Clone)]
struct WebFetch { settings: String, cache: Arc<PageCache> }

impl ToolHandler for WebFetch {
    fn execute(&self, call: ToolCall, _context: ToolExecutionContext, cancelled: CancellationToken)
        -> HostFuture<'_, ToolResult>;
}

/// The dynamic source: `sets()` answers one set with `web_search` while a
/// search service is usable and no set otherwise.
impl ToolProvider for SearchSource {
    fn sets(&self) -> Vec<ToolSet>;
}
```

---

## Appendix A. Interface details

### A.1 Tools

Web registers `web_fetch` once, from the part's apply, and offers `web_search` through a dynamic tool source it registers there, both with the Session's `toolRegistry` ([session-boundary A.5](../contracts/session-boundary.md#a5-tool-contribution); [tool contributions §7](../../architecture/rust-agent/tool-contributions.md#7-动态工具来源)). The source holds one set containing `web_search` while a search service is usable and no set otherwise; a new set carries a new version.

| Method | Kind | Request | Reply |
| --- | --- | --- | --- |
| `toolRegistry.register` | async | `ToolDeclaration {name, description, inputSchema, risk: "network"}`, `execute` | Nothing once registered |
| `toolRegistry.registerSource` | — | source name `web`, `sets()` | one set while a search service is usable, none otherwise |
| `execute` (Web's function) | async | `ToolCall {sessionId, branchId, turnId, callId, arguments, config}, cancelled: Promise<void>` | `ToolResult {content: [{type: "text", text}], isError, structuredContent?}` |

C4 supplies the existing by-reference `cancelled` Promise as the second argument. When it settles, Web aborts requests and releases sockets/timers before settling its call; row effects likewise release owned resources. ToolExecutor owns interruption terminal facts: Web writes no second cancellation fact and never retries an unknown outcome. Business errors are `ToolResult`s with `isError: true` (Appendix C); runtime/protocol binding failures retain C4 call-failure behavior rather than being masked as success or business data. Parameters are validated by the handler; a violation returns an error result without network access.

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

Titles and snippets are plain text, one line each, each at most 500 Unicode scalar values. Zero results yield `No results for "<query>".` (not an error). The service name is not shown. `structuredContent` carries `{ results: [{ title, url, snippet, published_at? }], diagnostics?: [{code, message}] }` for clients. Total time limit 20 s (A5).

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

The title is independently limited to 500 Unicode scalar values in both result fields. `max_chars` applies to the body slice. Bracketed lines appear only when they apply. `structuredContent`: `{ url, final_url, status, content_type, title?, start, end, total, truncated, download_limited, diagnostics?: [{code, message}] }`.

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
| Resolution | With `blockPrivateAddresses: true`, Web resolves the hostname once and checks every answer before the request. Without a proxy it also pins the first permitted address; a proxy resolves the target itself, so there the check does not constrain the connection. With it off, nothing resolves |
| Policy default | Off: `web_fetch` reaches whatever the host can route to, so a local proxy, a TUN device or a Fake-IP answer needs no exception. `blockPrivateAddresses: true` turns the policy below back on |
| Blocked with `blockPrivateAddresses: true` | Loopback (`127.0.0.0/8`, `::1`), unspecified, RFC 1918, link-local (`169.254.0.0/16`, `fe80::/10`, including cloud metadata), CGNAT `100.64.0.0/10`, unique local `fc00::/7`, multicast, broadcast, reserved ranges including the benchmark range `198.18.0.0/15`, and the IPv4-mapped and IPv4-compatible IPv6 forms of these |
| Literal IPs | Checked by the same rules when the policy is on; a literal IP is used as given |
| Ports | Any port (A12) |
| Exceptions | `allowPrivateHosts`: exact hostnames or CIDRs; empty by default. A user-only field: written through the client method `settings.update`, never by a Web write (S8b). Settings checks only that it is an array of strings; Web parses each entry at use, ignores one that is neither a hostname nor a CIDR and reports it as a diagnostic |
| Proxy | The target is `proxy` when set, otherwise the session's own proxy: `HTTPS_PROXY`, `ALL_PROXY` or `HTTP_PROXY` from the environment, then the desktop setting (GNOME `org.gnome.system.proxy` in `manual` mode). `DIM_WEB_SYSTEM_PROXY=off` disables both fallbacks. An `https` target goes through `CONNECT <host>:<port>`; an `http` target is sent to the proxy in absolute form, so the proxy sees the full URL |
| Proxy DNS | A proxy always receives the hostname: `CONNECT <host>:<port>` for `https`, an absolute-form request line for `http`. With `proxyDns: true` (default) that is the only lookup; with `false`, Web resolves and checks first and then sends the same hostname. A refused tunnel fails with `connect_failed` (A15); a proxy that answers the request itself is reported with its status. Without any proxy the flag has no effect |
| Proxy DNS trust | With `proxyDns`, the proxy alone resolves hostname targets, so a hostname may reach private or metadata addresses if the proxy permits it. Web still rejects `localhost` and `*.localhost` when the policy is on, unless explicitly allowed by hostname. CIDR exceptions apply only to addresses Web checks |

Fake-IP DNS answers are ordinary addresses. With a proxy and `proxyDns: true` they never reach Web at all, because the proxy resolves; with the policy on and no proxy they are rejected like any other reserved range.

### A.5 Business configuration (Settings source `web`)

| Field | Type | Default |
| --- | --- | --- |
| `searchService` | `{ kind: "<built-in adapter id>", endpoint?: string, apiKey?: string }` or absent; user-only | absent (uses `dim-oauth` through the account owner's `search` when signed in; otherwise `web_search` is not declared) |
| `allowPrivateHosts` | string[]; user-only | `[]` |
| `blockPrivateAddresses` | boolean; user-only | `false` |
| `proxy` | string (URL); write-only; absent means the session's own proxy (A.4) | absent |
| `proxyDns` | boolean; user-only | `true` |

Read at each call and used as delivered: Settings validates the document against the schema Web declares and applies its defaults on write and when reading a persisted file, and Web does not validate it again. Web has no tool that writes configuration, so the document is written only through the client methods (S8b). A change is a data update with notification, never a reload. The row's structural on/off state belongs to PluginManager (PluginManager). Built-in adapter ids are an implementation choice (A13). The API key is `searchService.apiKey` (A.5).

### A.6 Timeline and capability mechanics

| Step | Path |
| --- | --- |
| Provide | Web registers `web_fetch` and the `web_search` source with `toolRegistry` until its row unloads; the source offers `web_search` only while a search service is usable. ToolRegistry declares the host and registered tools the Session mounts (`web_fetch` without a mount list, or when its list names it) and every source's tools; because a mount list does not filter sources, Web itself withholds `web_search` when the list omits it. ToolBinding adopts the tools. Web writes nothing |
| Call | ToolExecutor consumes `step/tool-call` on the Dispatcher, registers the batch task and returns; per call `step/tool-start`, admission, handler on the executor's task, `step/tool-completed`, then one `step/tool-result` per batch |
| Dispatcher | No Dispatcher subscription; only the `web` source's changes and the managed account's availability |
| State and recheck | No `StateCell`; no consumed facts; decisions use arguments and configuration read at call start |
| Terminal facts | Written by ToolExecutor; Web returns exactly one `ToolResult` per call |
| Reload | Unload withdraws Web's registrations and aborts in-flight requests; ToolExecutor settles them as reloaded ([tool contributions §4](../../architecture/rust-agent/tool-contributions.md)); the next generation registers again |
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
| `web_search` with no usable service | — | Not declared; a request bound before the change returns `search_unavailable` | Web |
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
| `blocked_address` | Web rejects the target of a request or redirect hop under A.4. The message names the rule only |
| `dns_failed`, `connect_failed` | Network errors; `connect_failed` includes an unparsable proxy, a refused tunnel, or a TLS failure |
| `timeout` | Connect or whole-call limit reached |
| `http_status` | Non-2xx final status |
| `too_many_redirects` | More than 5 same-host hops |
| `unsupported_content_type` | Type not in A.3 |
| `search_unavailable` | No usable service or signed-in account, endpoint/proxy unparsable, adapter unusable, or a required API key missing |
| `search_failed` | Service error or invalid data; message shortened and stripped of secrets |
| `settings_unavailable` | The `web` source could not be read, or the stored document does not match the web document |
| `cancelled` | Call cancelled |

Recovery: Web holds no durable work; reload and restart lose only the cache.

## Appendix D. Acceptance

Flow tests through public interfaces: tool calls in a test Session, a local HTTP server reachable only through a test `allowPrivateHosts` entry, a fake search adapter.

1. Without a usable service a request declares `web_fetch` and not `web_search`; configuring a service makes the first request built after the branch adopts it declare it and its next call succeed, with no reload. Removing the service or signing the account out: a request built before that adoption keeps the declaration it was built with, so it may still declare `web_search`, and a call from it returns `search_unavailable`, while the first request built after the adoption declares `web_fetch` alone. Adding a service or signing in: a request built before that adoption does not see `web_search`, and the first request built after it declares it and its call succeeds. A request's declaration is fixed when the request is built, and a branch that has not adopted a tool part yet declares the registry's current set. A call-time failure of a usable service (network, service error, invalid response) keeps the declaration and fails the call. A Session whose mount list omits `web_search` never declares it. Schema-valid but unparsable endpoint/proxy values produce the errors in §6.1 without opening a connection or falling back to a direct connection.
2. `web_search` format, domain filters, zero results, service error.
3. `web_fetch` HTML header and Markdown; `start_char` pagination to the end; `max_chars`; download limit notice.
4. Plain text, JSON, unsupported type, 404, empty page.
5. Same-host redirects up to 5; sixth fails; cross-host redirect returns the notice.
6. With `blockPrivateAddresses: true`, loopback, private, link-local, IPv4-mapped IPv6, `localhost` and a private-resolving hostname fail with `blocked_address` and open no connection, directly and through `proxy`. With the policy off the same targets connect.
7. Permission: read-only denies, workspace-write asks, FullAccess allows.
8. Interrupt before dispatch, during approval and during a slow response (socket closed, one result).
9. Restart settlement repeats no request.
10. Non-interactive Sessions declare `web_fetch` and, with a usable search service, `web_search`.
11. Diagnostics and results never contain the API key.
12. Enabling proxy DNS through `settings.update` makes the next uncached fetch and direct search-adapter request resolve at the proxy: `CONNECT <hostname>:<port>` for `https`, an absolute-form request line for `http`, with no local target DNS even when the name has a Fake-IP answer or cannot resolve locally, and without a reload. The URL host remains the HTTP `Host` and the certificate/SNI identity. With no `allowPrivateHosts` exception, fetch a hostname that the fixture proxy resolves to a loopback test server and get its response. Disabling proxy DNS restores the local check before the request; the proxy still resolves the target. Proxy refusal and cancellation settle without a direct retry. These tests use a disposable home and a fixture proxy, never system DNS or proxy configuration.

## Appendix E. Assumptions

| # | Assumption | Basis | Effect if rejected |
| --- | --- | --- | --- |
| A1 | Search services, including account-backed ones, are built-in adapters; no backend registry | ADR 002 D2 | A search-backend contract owned by Web |
| A2 | Requests leave from the App Server process hosting the Session | Judgment | Network access through the resource node's services |
| A3 | `web_search` is declared only while the configuration or the managed account offers a usable search service; committed writes of that source and account changes change the offered set, a branch adopts it as its tool part, and a request's declaration is fixed when the request is built, so the first request built after the adoption declares the new set. After a removal or a sign-out a request built before the adoption may still declare `web_search` and its call returns `search_unavailable`; after an addition or a sign-in such a request does not see it. A branch that has not adopted a tool part yet declares the registry's current set. Call-time failures never withdraw the tool | ADR 002 D4 (configuration never reloads) | An always-declared `web_search` failing at call time |
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
| A15 | A configured proxy accepts both forms a target takes: `CONNECT <host>:<port>` for `https`, absolute-form requests for `http` | [ADR 024](../../adr/024-web-fetch-transport.md) | A proxy that accepts only one form cannot serve both schemes |
| A16 | Initial source is user file `web.json`, read at user scope; no implicit project merge | Maintainer decision | Define project source/merge semantics before using existing projectFile |
