# Web

Optional Session TypeScript plugin implementing the [Web contract](../../../docs/design/plugins/Web.md). It registers `web_search` and `web_fetch` with the Session's `toolRegistry`. Both declarations use `risk: "network"`; Permission and ToolExecutor retain admission, approval and terminal-fact ownership.

## Loading

Use the repository's pinned Node version, `npm ci --prefix runtime --ignore-scripts` and `npm ci --prefix runtime/plugins/web --ignore-scripts`. Web owns its SDK, HTML converter and IP parser in its own package and lockfile. They are installed only with this optional package; the shared runtime and account-enabled release do not acquire them.

Install Web into the embedding host's Node environment, for example `(cd runtime && npm install --no-save --ignore-scripts --install-links ./plugins/web)`. This explicit installation is separate from the shared runtime's locked dependencies and release payload. Its row file is [plugin.json](plugin.json), which the repository's default config file ([plugins.default.json](../../plugins.default.json)) loads; an embedder can pass the same declaration through `ServerConfig.session_typescript.rows`, with `anchor` pointing to that host's `package.json` (`runtime/package.json` for this example). The npm name `@dimcode/web` resolves through its declared export using the host's SDK/Cordis installation; loader 0.12.1 does not resolve relative file names. No row-field replacement is required. The module imports `inject` and `provides` from the same declaration (the row injects only `partRegistry` and provides no name); the row also owns the `web` Settings source. From its `apply` the row registers the Session part (the `part` export) with `partRegistry`, and each Session mounts a copy. A Session without a mount list declares `web_search` and `web_fetch` once its copy registers them; a Session with one declares those it names. The default CLI loads it from the default config file; `dimcode plugin disable web` turns it off.

The Session part's required named service is `settings`. Missing dependencies retain the loader's Pending behavior. The Session's copy registers its tools while its Session is created, so they are in the Session's first request; a reload registers them again. Configuration edits take effect on the next call without a reload.

## Configuration

Each call reads `settings.read({source: "web", scope: "user"})`. Settings validates and defaults the document. The source declares the user file `web.json`, no project file, and these fields:

| Field | Meaning |
| --- | --- |
| `searchService` | Optional `{kind, endpoint?, apiKey?}`; initial built-in adapter: `brave` |
| `allowPrivateHosts` | Exact normalized hostnames or CIDRs, default `[]`; invalid entries are ignored with a fixed diagnostic |
| `proxy` | Optional HTTP(S) proxy origin supporting CONNECT to numeric addresses; no authentication or direct fallback |

`searchService` and `allowPrivateHosts` are user-only writes. Local clients update the source through `settings.update` using the revision from `settings.read`; Web never writes configuration. User-only does not hide values from authorized local reads. Native storage protects the document with the product-home boundary described in the design.

Brave uses its [Web Search API](https://api-dashboard.search.brave.com/app/documentation/web-search/get-started), default endpoint `https://api.search.brave.com/res/v1/web/search`, GET parameters `q` and `count`, and header `X-Subscription-Token`. Configure `searchService.kind` as `brave` and `searchService.apiKey` locally. The optional endpoint uses the same API shape, including `type: "search"` and optional `web.results`. A missing `web` section means zero web results; a result without a description has an empty snippet. There is no backend registry. An absent or unusable configuration returns `search_unavailable`.

The API key is used only for the current call. Results, errors and diagnostics redact its raw and URL-encoded forms; invalid key-bearing search URLs fail without returning them. Requests carry no ambient cookies, credentials or referrer. Fetch does not send the search token.

## Tools

| Tool | Parameters | Result |
| --- | --- | --- |
| `web_search` | `query`: 1–500 Unicode scalars after trimming; `max_results`: 1–10, default 5; mutually exclusive `allowed_domains` / `blocked_domains`: up to ten hostnames | Numbered plain titles, URLs and snippets of at most 500 scalars; every result is post-filtered, including subdomains. Zero results is successful. |
| `web_fetch` | HTTP(S) `url`: at most 2048 scalars, no userinfo; `start_char`: non-negative safe integer, default 0; `max_chars`: 1000–50000, default 20000 | URL, title, media type, character range and total, followed by text. Truncation and download-limit notices are model-visible. Continue at the returned final URL. |

HTML/XHTML becomes Markdown, with absolute links, navigation and executable/control elements removed, preferring `main`, then `article`. Header charset precedes HTML meta charset and UTF-8 replacement. Plain text, Markdown, CSV, JSON and XML stay text; valid JSON is indented without losing numeric literals. Binary formats are rejected. Error responses never expose their bodies.

Each request resolves and checks every target address, then tries the checked IPs in DNS order until a connection succeeds or the shared connect deadline expires. Failed attempts are closed before retrying; an HTTP request is sent only after connection and TLS verification succeed. Private, loopback, link-local, shared, multicast, reserved and equivalent IPv6 addresses are blocked unless covered by a user exception. A proxy receives `CONNECT <validated IP>:<port>` for both HTTP and HTTPS. Host and TLS identity retain the original hostname. DNS lookup, proxy establishment and TLS negotiation share the 10-second connect deadline. Search has a 20-second total deadline; fetch has 30 seconds. Responses stop at 5 MiB after decompression and close the connection before returning the prefix.

Search titles and snippets, and fetched-page titles, are independently limited to 500 Unicode scalar values. Fetch `max_chars` limits the body slice; a remote title cannot bypass it through unbounded metadata.

At most five redirects are followed within the normalized hostname group (lowercase, leading `www.` removed), checking addresses on each hop. Cross-host redirects return a notice and require another tool call. Fetch cache entries use the final URL, expire after 15 minutes and share a 50 MiB UTF-8 payload budget per Session; oldest entries are evicted first. Concurrent fetches are independent and the first completed conversion supplies the cached page. Reload and restart discard the cache and do not replay network operations.

The existing by-reference cancellation Promise aborts owned sockets and timers. Copy cleanup cancels and drains calls before releasing the cache. Web writes no Timeline event or durable state. Non-fatal diagnostics are in `structuredContent`; pagination handles and actionable errors remain in `content`.

## Fake-IP DNS

Fake-IP DNS can return synthetic benchmark addresses such as `198.18.0.0/15` for public names. Web rejects them by default, including the Brave endpoint, before opening a direct connection or proxy tunnel. An explicit proxy does not bypass local DNS validation, and Web cannot infer the real destination behind a synthetic address.

Configure the DNS/TUN service to return real target addresses for Web traffic. For Mihomo, use its [Fake-IP filter](https://wiki.metacubex.one/en/config/dns/#fake-ip-filter) for the search endpoint and fetched hosts, or use `redir-host` when arbitrary web browsing needs real answers. A real answer is still checked against the address policy and pinned for connection. The proxy must support CONNECT to that IP.

For a fixed host you explicitly trust, the existing user-only `allowPrivateHosts` setting can authorize that exact hostname, for example `"allowPrivateHosts": ["api.search.brave.com"]`. A custom search endpoint needs its own exact hostname; allowing the search endpoint does not allow fetched result hosts. The exception trusts that hostname's destinations, including private ones, and only works if the local TUN/proxy can route its synthetic IP. Prefer real-address DNS when possible. Do not allow the entire Fake-IP CIDR as a workaround: that grants every model-selected hostname resolving into it the same exception. No automatic benchmark-range exception, remote-DNS proxy mode or implicit public DNS service is enabled.

## Verification

```sh
node runtime/node_modules/typescript/bin/tsc -p runtime/plugins/web/tsconfig.json
node --test runtime/plugins/web/web.test.mjs
DIM_TEST_NODE="$(command -v node)" \
  DIM_TEST_JS_RUNTIME="$PWD/runtime/node_modules/@arcships/rutis-runtime" \
  cargo test --locked -p dim-app-server --test transport_flow node_web_ -- --ignored
```

The Node flows use the public rutis test loader and real loopback HTTP, TLS and CONNECT servers. They cover tool output, configuration changes, address pinning, TLS identity, proxy failure, redirects, bounded conversion, concurrent cache fills, cancellation, unload and deadlines. TLS fixtures generate temporary certificates with OpenSSL. No live Brave request or real API key is required.

The native transport flow uses real Settings, C4, Permission, ToolExecutor and SQLite in a disposable home. It covers hot configuration, denied/approved/cancelled approval, slow-response interruption, a single terminal result, cache pagination and restore without repeating completed requests. Crash settlement and cancellation before dispatch remain ToolExecutor's existing flow coverage; this test does not simulate a process crash during a Web request.
