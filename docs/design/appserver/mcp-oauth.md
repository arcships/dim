# MCP OAuth

Status: Implemented on the daemon side (maintainer review on [#111](https://github.com/arcships/dim/pull/111)) and in the native CLI (§13). Roadmap item F4
([#75](https://github.com/arcships/dim/issues/75)). Overview:
[Appendix B.2, Mcp row](../overview.md#b2-app-server-process-plugins). Phase: P2. Language: Rust.
Scope: inside the existing Mcp process plugin ([ADR 002](../../adr/002-plugin-scope.md)). Basis:
MCP specification revision **2026-07-28**, the current revision
([Authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)).

## 1. Summary

An HTTP or SSE MCP server that answers `401` gets OAuth instead of a dead connection. Mcp
discovers the authorization server, registers a client, and hands the user an authorization
URL. It completes the code exchange with PKCE and the resource indicator, then stores the tokens
and refreshes them on use. Sessions that were waiting for the server reconnect on their own.

Every part lives in Mcp, the owner of MCP configuration and connections:

- a process-scope credential authority with its own 0600 store;
- the existing Session discovery and connection instances, which attach the bearer token;
- three new management methods, `mcp.auth.start`, `mcp.auth.complete` and `mcp.auth.logout`.

F4 adds no plugin and no Timeline event, changes no infrastructure and no shared contract, and
does not depend on S8b. Done when an OAuth-protected MCP server connects and refreshes its tokens
(Appendix B).

The OAuth protocol steps come from the official Rust SDK already in the workspace: rmcp 3.5.0
with its `auth` feature. Mcp owns everything around them: orchestration, storage, the user
flow, and the gaps rmcp leaves, three of them MUSTs of the specification (Appendix A).

## 2. Basis

| Source | What it settles | What Dim takes |
| --- | --- | --- |
| MCP 2026-07-28 [Authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization), [Authorization Server Discovery](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery), [Client Registration](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration), [Security Considerations](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/security-considerations) | OAuth 2.1 with PKCE `S256`. Protected Resource Metadata (RFC 9728), from `WWW-Authenticate` `resource_metadata` or the well-known URIs (path form first, then root). Authorization server metadata through RFC 8414 or OIDC discovery in a fixed order, with `issuer` checked against the URL used. Registration priority: pre-registered, then Client ID Metadata Document (CIMD), then Dynamic Client Registration (RFC 7591, deprecated), then user-entered. `resource` (RFC 8707) on authorization and token requests. `iss` validation (RFC 9207). A client MUST refuse when `code_challenge_methods_supported` is absent. Authorization server endpoints MUST use HTTPS, and redirect URIs MUST be `localhost` or HTTPS. Bearer token on every request, never in the query. A `403 insufficient_scope` calls for step-up with a union of scopes. Authorization servers rotate public clients' refresh tokens | Everything normative in §§5 to 10 |
| rmcp 3.5.0, feature `auth` (`src/transport/auth.rs`) | The `auth` module covers metadata discovery, DCR and CIMD client selection, PKCE and `state`, code exchange with `resource` and `iss`, and refresh with rotation. The `CredentialStore` and `StateStore` traits are pluggable. It shares the reqwest 0.13 already used by the transport | The protocol engine; gaps in Appendix A |
| Claude Code (`/mcp`, `claude mcp login`, [docs](https://code.claude.com/docs/en/mcp)) | The `oauth` object in `.mcp.json`: `clientId`, `callbackPort`, `authServerMetadataUrl`, `scopes`. Redirect `http://localhost:PORT/callback`; a switch to `127.0.0.1` broke servers that match the registered URI exactly and was reverted. Paste the callback URL when the redirect fails. The client secret is kept out of config. Refresh, then retry once, on `401` | The `oauth` field names and types, the redirect form, the paste fallback, the secret outside config |
| MCP TypeScript and Python SDKs (`OAuthClientProvider`, `TokenStorage`) | Storage and redirect handling are injected by the host. Credentials carry an issuer stamp and are discarded on mismatch. `invalid_client` clears the registration and `invalid_grant` clears the tokens | Owner-supplied storage, issuer binding, clearing rules |
| Dim account login (`account.login.browser.begin/complete`, `managed_oauth.rs`, `dim-cli/src/login.rs`) | The daemon returns `{attemptId, authorizationUrl, expiresAt}`. The client sends back the pasted callback URL, read with hidden input. Tokens live in `auth.json` under the home root, through a FileStorage lease with atomic 0600 replace | The attempt shape, paste input, the storage pattern |

## 3. Structure

| Part | Scope | Change | Depends on |
| --- | --- | --- | --- |
| `McpAuth`, the credential authority inside `McpPlugin` | Process | New, Mcp-internal | `FileStorage` (home root, file `mcp-oauth.json`), rmcp `AuthorizationManager` |
| Mcp management (`mcp.*`) | Process | Adds `mcp.auth.start/complete/logout`; `mcp.catalog` entries gain `auth` | `McpAuth`, Settings (existing) |
| `McpSession` | Session | Reports `auth_required`; re-runs discovery of such entries when their credential changes | `McpService` (existing), `McpAuth` |
| `McpServer` connection instance | Session loader instance | Attaches the bearer per request; reports `401` and `403` to `McpAuth`; may reconnect after `auth_required` | `McpAuth` replaces the ordering-only `Gate<McpService>` (same plugin, so the same reload propagation) |

- **Reload.** Nothing outside Mcp depends on these parts. `McpPlugin` takes the home root in its
  factory, as `OpenAICodexOAuthPlugin::new(home_root)` does, and adds `Arc<FileStorage>` to its
  `Deps`, a core-to-core dependency.
- **Standalone hosts.** Hosts that register `McpPlugin` supply the same root.
- **Lifecycle.** A reload of Mcp drops pending attempts, which live in memory. The store
  survives.
- **Assembly.** Mcp is assembled only with local execution (`execution_root`), so OAuth exists
  exactly where MCP does.

## 4. Data and state

### 4.1 Credential record (persisted)

There is one record per credential key: the server `id` plus the canonical server URL, the
configured `url` serialized by the URL parser (lowercase scheme and host, default port elided)
with a lone `/` path dropped. The key is process-wide. It does not depend on the configuration
source or the Session (§10 D8).

```text
// mcp-oauth.json, under the home root, mode 0600, FileStorage atomic replace
{ "version": 1,
  "credentials": [
    { "serverId": "github", "url": "https://api.example.com/mcp",
      "resource": "https://api.example.com/mcp",   // as the authorization request sent it
      "issuer": "https://auth.example.com",        // recorded authorization server
      "tokenEndpoint": "https://auth.example.com/token",
      "revocationEndpoint?": "https://auth.example.com/revoke",
      "client?": { "clientId": "…", "clientSecret?": "…",
                   "registration": "preregistered" | "dynamic",
                   "redirectUri": "http://localhost:1456/callback",
                   "authMethod": "none" | "client_secret_basic" | "client_secret_post" },
      "tokens?": { "accessToken": "…", "refreshToken?": "…",
                   "expiresAt?": 0, "scopes": ["…"] },   // expiresAt: Unix ms
      "requestedScopes": ["…"] } ] }
```

- **One writer.** Mcp is the only writer.
- **Self-contained.** The record carries the token and revocation endpoints and the client's
  authentication method, so refresh and sign-out need no new discovery. A record is written when
  an attempt completes, with its client and tokens; an abandoned attempt leaves nothing.
- **Write path.** A write takes the store's FileStorage lock, re-reads the file, replaces only its
  own key's record and atomically replaces the file with mode 0600. On Windows the file keeps the
  FileStorage DACL handling. A replace that is in place but whose sync failed counts as written:
  readers already see it, and a crash that loses it ends like the kill in §7.
- **Token requests hold the lock.** A code exchange, refresh or revocation holds that lock from
  the re-read through the request to the replace, as the managed OAuth host does. Two daemons
  can share a home (for example `--product-home` with different `--database` paths), and the
  lock stops both from redeeming one rotating refresh token. Token requests for different servers
  therefore wait on each other, each bounded by its 30 s timeout; they are rare. Discovery and
  registration run outside the lock.
- **Settled once sent.** A token request that has been sent is received and persisted on
  `McpAuth`'s own task, whoever is waiting for it, so neither the caller's departure nor an unload
  loses a rotated refresh token or a redeemed code. Mcp's unload waits for it. A code exchange
  whose attempt was superseded meanwhile, or whose record cannot be written, is still received,
  but its tokens are revoked and discarded instead (§7).
- **Unknown version.** OAuth is unavailable for every server (`mcp_auth_unsupported`) and the
  file is preserved, never overwritten. Connections then send requests without a bearer, so
  servers that need no sign-in keep working. A later incompatible format gets a new version and
  an explicit format decision.
- **Expired.** A record without `tokens` is expired. A rejected refresh removes `tokens`, and an
  `invalid_client` answer also removes `client`, so the state survives a restart.

### 4.2 Process memory (not persisted)

| State | Content | Lifetime |
| --- | --- | --- |
| Challenge | Per key: the last `WWW-Authenticate` (`resource_metadata`, `scope`, `error`) seen by any connection | Until authorization succeeds, sign-out or Mcp reload |
| Attempt | `attemptId`, key, PKCE verifier, `state`, expected `issuer`, metadata, client, redirect URI, scopes, `expiresAt` | At most one per key; ends on complete, expiry (10 min), sign-out or a new start for the same key (§7) |
| Attempts lock | One in-process async lock over every key's attempt and completed sign-outs: the single order of attempt state (§7). Its holders never wait for the store lock or a network request | Mcp generation |
| Store lock and change watch | One in-process async lock in front of the FileStorage lock, which admits or fails at once, so a lock held by another process is retried, as the managed OAuth host does; plus a watch per key, notified on every change to the key's record with whether it holds tokens: Sessions waiting in `auth_required` retry on any change, and ready Sessions react when the tokens are gone | Mcp generation |

The PKCE verifier and `state` are never persisted. A daemon restart abandons pending attempts.
There is no record cache: each request reads the file without the lock, which is safe because
writes are atomic replaces, and sees a refresh made by another daemon on the same home.

### 4.3 Configuration (`mcp` business source)

A remote entry may carry an `oauth` object whose field names and types match Claude Code's
`.mcp.json`, so files shared with it validate in Dim:

| Field | Type | Meaning |
| --- | --- | --- |
| `clientId` | string, non-empty | Pre-registered public or confidential client (registration priority 1) |
| `callbackPort` | integer 1 to 65535 | Port of the redirect `http://localhost:{port}/callback`; default 1456 |
| `authServerMetadataUrl` | `https` URL, or loopback `http` | Authorization server metadata used when the server publishes no valid PRM (§5.2) |
| `scopes` | string, space-separated (RFC 6749 §3.3) | Pins the requested scopes (§5.5) |

- **Unknown fields.** Unknown `oauth` fields are preserved and ignored, like entry-level
  extension fields. `oauth` is already a reserved key in `McpSettings::validate`, so no stored
  document changes meaning.
- **Static credentials exclude OAuth.** An entry with `auth` (static bearer) or an
  `Authorization` header never uses OAuth. Setting `oauth` together with either is
  `mcp_invalid_config`.
- **No secret field.** `oauth` carries no secret. A confidential client's secret is entered at
  sign-in (§5.3) and kept in the record.

## 5. Flows

### 5.1 Discovery and authorization

```text
 CLI (local transport)      Daemon: Mcp                       MCP server            Authorization server
  |                          | Session connects                  |                        |
  |                          |-- POST initialize --------------->|                        |
  |                          |<- 401 WWW-Authenticate: Bearer resource_metadata=…, scope=…|
  |                          | keep challenge; state auth_required; no tools published    |
  | mcp.auth.start {id}      |                                   |                        |
  |------------------------->|-- GET resource_metadata --------->|                        |
  |                          |<-- PRM (resource, AS list) -------|                        |
  |                          |-- GET /.well-known/oauth-authorization-server ------------>|
  |                          |<- metadata: issuer, endpoints, S256 -----------------------|
  |                          |   (checks: issuer, PKCE S256, HTTPS)                       |
  |                          |-- POST registration (only DCR) --------------------------->|
  |                          |<- client_id -----------------------------------------------|
  |                          | attempt: verifier, state, issuer (memory only)             |
  |<- {attemptId, authorizationUrl, redirectUri, resource, issuer, expiresAt}             |
  | shows resource + issuer; opens the browser ------------------------------------------>|
  |                          user signs in and consents at the authorization server       |
  |<- browser -> http://localhost:1456/callback?code&state&iss (CLI listener, or paste)   |
  | mcp.auth.complete {attemptId, callbackUrl}                                            |
  |------------------------->| check redirect URI, state, iss    |                        |
  |                          |-- POST token: code, code_verifier, resource -------------->|
  |                          |<- access_token, refresh_token?, expires_in ----------------|
  |                          | persist record (0600); bump key revision                   |
  |<- {}                     | Sessions in auth_required reconnect                        |
  |                          |-- initialize, Authorization: Bearer … --->|                |
```

Budgets: Mcp gives rmcp an HTTP client with a 30 s timeout per request, the same value as
rmcp's own default, and uses it for its own token requests; revocation requests get 10 s; an
attempt lives 10 min. Mcp's unload cancels discovery and registration in flight, but a token
request already sent is settled first (§4.1). A caller that leaves cancels nothing (management
rule).

### 5.2 Discovery rules

Every `mcp.auth.start` runs discovery again; nothing is cached across attempts. Mcp uses rmcp's
discovery, which follows the specification's order with the additions listed in Appendix A.

1. **Protected Resource Metadata.** Use the `resource_metadata` URL from the recorded challenge.
   Without a challenge, for example after a restart, try
   `/.well-known/oauth-protected-resource{path}` and then the root form. The metadata URL must be
   same-origin with the server, and `resource` must be the server URL or a parent path of it on
   the same origin (§10 D16). A mismatch is `mcp_auth_discovery_failed`.
2. **Authorization server metadata.** Try the entries of `authorization_servers` in order, each
   with the specification's well-known order for issuers with and without a path; `issuer` must
   equal the issuer used to build the URL. An issuer mismatch fails discovery at once instead of
   moving to the next entry (rmcp's behaviour, which fails closed).
3. **No valid PRM.** The specification says "MCP clients MUST use OAuth 2.0 Protected Resource
   Metadata for authorization server discovery", and its discovery flow ends in "abort or use
   pre-configured values" when none is found.
   - If `oauth.authServerMetadataUrl` is set, that document is used. A well-known URL is
     validated like a discovered one; any other URL is the configured trust anchor, and its
     `issuer` is recorded. The resource is then the server URL, as rmcp's authorization request
     serializes it; the record repeats exactly that value.
   - Otherwise the result is `mcp_auth_discovery_failed`.
   - Mcp never guesses an authorization server at the server's origin, and never uses rmcp's
     synthesized `/authorize` and `/token` endpoints.
   - A valid PRM always wins over the override, so a project file cannot send the user to
     another authorization server for a server that publishes PRM.
4. **Refusals.** Metadata without `code_challenge_methods_supported`, or whose list lacks
   `S256`, is refused. So is an authorization, token, registration or revocation endpoint that is
   not `https`, unless it is loopback `http`. Both are `mcp_auth_discovery_failed`. A server URL
   that is plain `http` on a non-loopback host is `mcp_auth_unsupported`: tokens never travel in
   cleartext.
5. **Issuer binding.** The record binds its client and tokens to the recorded `issuer`, as the
   specification requires for client credentials. When `start` finds that PRM now names another
   authorization server, nothing recorded for the old one is used. A DCR or CIMD client is
   registered again. A configured `oauth.clientId` is refused with `mcp_auth_client_required`,
   since the specification asks for an error rather than mismatched credentials; signing out
   deletes the record, and the next `start` binds the configured client to the new issuer. While a
   server stays signed in, a switch of authorization server shows up as `401` after refresh, so
   the record becomes `expired` and the next `start` finds the change.

### 5.3 Client registration

The specification's priority, first match wins:

1. **`oauth.clientId`.** For a confidential client the user supplies the secret, and the CLI
   sends it once in `mcp.auth.start {clientSecret}`. Mcp keeps it in the record, reuses it while
   both the configured `clientId` and the recorded issuer match, and never returns it.
2. **CIMD.** Used when the build carries a client metadata URL and the authorization server
   advertises `client_id_metadata_document_supported`. The client ID is the HTTPS URL of Dim's
   published document: `client_name` `Dim`, `redirect_uris`
   `["http://localhost:1456/callback"]`, `token_endpoint_auth_method` `none`. Maintainer
   decision (#111): Dim publishes no client metadata document for now and uses DCR; the question
   is decided again when a server requires CIMD. Until then F4 ships no URL and skips this step
   (Appendix C). Once enabled, it is also skipped when `callbackPort` is not 1456, because the
   document lists only that redirect URI.
3. **DCR.** Used when the authorization server has a `registration_endpoint`. The request
   carries `client_name` `Dim` (never taken from configuration), the redirect URI, grant types
   `authorization_code` and `refresh_token`, `token_endpoint_auth_method` `none` and
   `application_type` `native`. A secret the server issues anyway is kept in the record. A stored
   DCR client whose redirect URI differs from the current one, after a `callbackPort` change, is
   registered again.
4. **No route.** Otherwise the result is `mcp_auth_client_required`: register a client at the
   server's portal and set `oauth.clientId`.

An `invalid_client` answer clears the stored registration and its tokens. During sign-in the
attempt ends with `mcp_auth_failed`, because the code was issued to that client; during refresh
the record becomes `expired`. The next `mcp.auth.start` registers again by DCR or CIMD, or uses
the configured `clientId` again.

### 5.4 Callback validation (`mcp.auth.complete`)

Checked in order. A failed check in steps 1 to 3 leaves the attempt pending, so a wrong paste can
be retried:

1. **Origin and path.** Scheme, host, port and path must equal the attempt's redirect URI
   exactly, compared without normalization. Otherwise `mcp_auth_callback_invalid`.
2. **State.** `state` must equal the attempt's value, compared in constant time. Otherwise
   `mcp_auth_callback_invalid`.
3. **Issuer.** `iss` follows the specification's RFC 9207 table. If present, it must equal the
   recorded issuer by simple string comparison. If absent, it is rejected only when the metadata
   advertises `authorization_response_iss_parameter_supported`. On mismatch, no `error` field of
   the response is acted on or shown (`mcp_auth_callback_invalid`).
4. **Authorization server error.** An `error` parameter ends the attempt with `mcp_auth_failed`,
   carrying the RFC 6749 error code only, for example `access_denied`. A callback with neither
   `error` nor `code` is `mcp_auth_callback_invalid` and leaves the attempt pending.
5. **Exchange.** While the attempt is the key's current one (§7), Mcp sends the token request
   itself: `code`, `code_verifier`, `redirect_uri`, `client_id`, `resource` and the record's
   client authentication. It does not call rmcp's exchange function, which logs the code
   (Appendix A). Success ends the attempt and persists the record before replying. A token error
   ends the attempt with `mcp_auth_failed`. A transport error keeps it pending
   (`mcp_auth_unavailable`); if the server had already redeemed the code, the retry fails with
   `mcp_auth_failed` and the user starts again.

### 5.5 Scopes

- **Pinned.** `oauth.scopes`, when set, is requested as is.
- **Otherwise:**
  - for the first authorization, the challenge `scope`, else PRM `scopes_supported`, else no
    `scope` parameter;
  - for a re-authorization, the union of `requestedScopes`, the latest challenge `scope` and
    every scope a `403` asked for (step-up). `requestedScopes` counts only while the record's
    issuer is the current one.
- **`offline_access`.** Added when the authorization server lists it in `scopes_supported`, so
  that a refresh token is issued (the specification's MAY).

### 5.6 Using and refreshing tokens

```text
 conn A (Session 1)     conn B (Session 2)     McpAuth, key K            Authorization server
   |                      |                      |                              |
   |-- lease(K) -------------------------------->|                              |
   |                      |                      | 30 s or more left:           |
   |                      |                      | reply; else lock store,      |
   |                      |                      | re-check, refresh            |
   |                      |-- lease(K) --------->|                              |
   |                      |                      | (waits for the lock)         |
   |                      |                      |-- refresh, resource -------->|
   |                      |                      |<-- access, rotated refresh --|
   |                      |                      | persist, unlock              |
   |<-- lease r2 --------------------------------|                              |
   |                      |<-- lease r2 ---------|                              |
   | MCP: 401 on r2       |                      |                              |
   |-- rejected(r2) ---------------------------->|                              |
   |                      |                      | still r2: refresh;           |
   |                      |                      | else newer lease             |
   |<-- lease r3 --------------------------------|                              |
   | retry once with r3; another 401: record expired, connections auth_required
```

- **Per request.** Every HTTP request asks `McpAuth` for a lease and sends
  `Authorization: Bearer`; the specification requires authorization on every request. This
  covers streamable HTTP POST, GET and DELETE, and SSE GET and POST. A refreshed token is
  therefore used without reconnecting.
- **Before expiry.** A token with less than 30 s left is refreshed before use, the same margin as
  the managed OAuth host and rmcp. Refresh happens only on use; there is no background timer.
- **On `401`.** A `401` counts with or without `WWW-Authenticate`. When a request sent with
  token `rN` gets one, the connection reports `rN` as rejected. `McpAuth` refreshes only if the
  record still holds `rN`; otherwise another Session has already refreshed. The request is retried once. A rejected request was not processed by the
  server, so retrying it repeats no effect.
- **Single flight.** One refresh runs per key at a time across all Sessions: a waiter that gets
  the lock re-reads the record and finds the new token.
- **Rotation.** A new refresh token replaces the old one. If none is returned, the old one is
  kept. The new record is persisted before any lease uses the new access token.
- **Reauthorization.** `invalid_grant`, `invalid_client`, a token without a refresh token that
  expired or was refused, or a `401` after a refresh sets the record `expired`. Every Session on the key then
  reports `auth_required` without waiting for a request of its own. Any other refusal of a
  refresh, or a response that is not an OAuth error, keeps the record: the access token serves
  while it is valid, and after that the request fails until the authorization server answers.
- **Store errors.** A store that cannot be read, or is in an unknown version, sends requests
  without a bearer, so a server that needs no sign-in keeps working; only a `401` decides.

### 5.7 Step-up (`403 insufficient_scope`)

A `403` whose `WWW-Authenticate` has `error="insufficient_scope"` fails only that request; the
connection stays `ready`. Mcp checks the `error` parameter itself, because rmcp maps any `403`
that carries the header to `InsufficientScope`.

- The tool result is the fixed text of §8.
- A `403` while the Session discovers the server (the handshake or `tools/list`) leaves no tools
  to publish, so the Session reports `auth_required` until the credential changes.
- `McpAuth` records the challenged scopes, and the catalog shows them as `missingScopes`.
- The next `mcp.auth.start` requests the union of §5.5. Step-up is user-initiated, so it cannot
  loop.
- With pinned `oauth.scopes`, the pin is requested unchanged. The user adds the scope to the pin,
  as with the same field in Claude Code.

### 5.8 Sign-out

`mcp.auth.logout` revokes the refresh token, then the access token, through the authorization
server's `revocation_endpoint` (RFC 7009) when one is advertised. It then deletes the record,
including the registration and any client secret, whatever the revocation outcome. The reply
says whether revocation succeeded. Every Session on the key then reports `auth_required`.
From the moment it is accepted, sign-out closes the key's pending attempt, and it closes any
attempt started before it completes, so a sign-in already under way in the same daemon cannot
undo it (§7).
Sign-out is also allowed once an entry no longer uses OAuth (static credentials added, or a
cleartext URL), so a stored credential can always be removed.

`mcp.remove` does not touch credentials, because an entry of the same `id` and URL in another
source may still use them; the CLI offers sign-out first (§13). An orphaned record is inert: its
tokens go only to its own URL, and only for an entry with that key.

## 6. Token lifecycle

Each node is the catalog's `auth.status` for one key. `refreshing` is internal and not shown.

```text
 absent --401--> required --start+complete--> authorized <--success-- refreshing
   ^                                                  |                  ^    |
   |                                                  +-- near expiry ---+    |
   |                                                      or 401              |
   |                                                                          |
   |                expired <--- invalid_grant, invalid_client, no refresh ---+
   |                   |         token, or 401 after refresh
   |                   +--start+complete--> authorized
   |
   +---- logout, from required, authorized or expired
```

`absent`: Dim holds no record for the key and no recorded challenge. Sign-out deletes the record
and clears the challenge. The catalog then has no `auth` field.

## 7. Timing and conflicts

**Attempt currency.** A key has at most one current attempt, identified by its `attemptId`. Only
the current attempt may redeem its code, store tokens or wake Sessions. Attempts live in one
daemon's memory (§4.2), so this rule orders operations within a daemon. A sign-in that completes
in another daemon sharing the home after a sign-out is a later sign-in.

**One order.** The attempts lock (§4.2) is the single linearization point of attempt state.
`start` holds it to make its attempt current, `logout` to close attempts when it is accepted and
when it completes, `complete` to validate a callback, and the exchange for both currency checks.
The exchange holds it from its final check until its outcome is published. Its holders never
wait for the store lock or a network request, so waiting for it takes at most one local store
write, never a token request or a refresh. Every `start` and `logout` is therefore ordered either
before an exchange's final check, which then finds its attempt superseded, or after its
publication: a newer attempt that completes later replaces the stored credential, and a
sign-out deletes and revokes it. A superseded attempt never writes the store or wakes Sessions
after a newer `start` has returned.

- **`start`** makes its attempt current when it returns, replacing the previous one. A
  publication under way holds the attempts lock, so the new attempt becomes current after it. If
  a sign-out completed after `start` read the record, `start` answers `mcp_auth_attempt_closed`
  instead, because its attempt may reuse a client that the sign-out deleted.
- **`logout`** closes the current attempt when it is accepted, before it waits for the store lock.
  It closes the attempt again once it has deleted the record, which ends an attempt started in
  the meantime.
- **`complete`** ends the attempt when its exchange settles, except after a transport error
  (§5.4). An authorization server error in the callback, or expiry, ends it too.
- **The exchange** checks that its attempt is still current twice: after it takes the store lock,
  before it sends the token request, and when the response is in, before it writes the store.
  From the second check it holds the attempts lock until its outcome is published: the record
  written, the challenge and missing scopes cleared and the Sessions woken; after
  `invalid_client`, the registration cleared; after a transport error, the attempt pending again.
  - Superseded before the request: the code is never sent.
  - Superseded while the request was out: the request is still settled (§4.1). Tokens it
    obtained are revoked (RFC 7009, best effort, the refresh token first) and discarded. The
    revocation runs while the store lock is still held, so it precedes any later token request.
    Nothing is written to the store, whatever the response, and no Session is woken.
  - Either way `complete` answers `mcp_auth_attempt_closed`.
  - Current at the final check: the outcome is published before any later `start` or `logout`
    takes effect. The code is spent, so the attempt ends even if the record cannot be written;
    the tokens are then revoked once the attempts lock is released.
- **A refresh** needs no such check. It re-reads the record under the store lock, so it sees any
  sign-in or sign-out that held the lock before it, and one that holds the lock after it
  replaces or deletes its result.

| Race | Outcome |
| --- | --- |
| Two `start` calls for one key | The later to return replaces the earlier; completing the earlier is `mcp_auth_attempt_closed` |
| `start` while a `complete` is under way | Until the old exchange's final check, the new attempt supersedes it: a code not yet sent is never sent, tokens already obtained are revoked and discarded, and `complete` answers `mcp_auth_attempt_closed`. After that check, `start` becomes current only once the old exchange has written its record and woken the Sessions; completing the new attempt replaces that record |
| Two `complete` calls for one attempt | One exchange at a time: the second answers `mcp_auth_attempt_closed` at once. After a transport error the attempt is pending again |
| `logout` while an attempt is pending or under way | The attempt closes when `logout` is accepted. An exchange still waiting for the store lock sends nothing; one whose request is out discards its tokens; one past its final check publishes first, and `logout` is accepted after it. `logout` then deletes the record and revokes its tokens |
| `start` while `logout` runs | If the sign-out completes before `start` returns, `start` answers `mcp_auth_attempt_closed`; an attempt `start` made current before then closes when the sign-out completes |
| `complete` while a refresh runs | Both hold the store lock in turn: a refresh after `complete` finds fresh tokens and does nothing; `complete` after a refresh replaces the tokens |
| `logout` while a refresh runs | `logout` takes the store lock after it; the refreshed tokens are revoked and deleted. A refresh after `logout` finds no record |
| Two Sessions get `401` together | One refresh (§5.6); both retry with the new lease |
| Entry URL edited | New key, so the new generation starts `absent` and becomes `required` on its first `401`; the old record is inert |
| Entry disabled or removed during an attempt | The attempt may still complete and persist; nothing connects until the entry is enabled |
| Mcp reloads | Attempts are dropped (`complete` then answers `mcp_auth_attempt_closed`); calls in flight answer `operation_interrupted`; the store is kept |
| Process killed after the server rotated the refresh token but before persistence | The old refresh token is invalid: `expired`, sign in again. A graceful stop settles the request first (§4.1), so only a kill hits this window |
| Caller leaves mid-attempt (Ctrl-C, disconnect) | Nothing to undo: a pending attempt has no effect until `complete`, and it expires after 10 min or is replaced by the next `start` |
| Clock change | `expiresAt` is computed from `expires_in` on receipt; a wrong clock only moves the refresh earlier or later, and `401` handling covers it |

## 8. Context mapping

The model sees none of this.

- **No tools while unauthorized.** A server in `auth_required` publishes no tools, so its tools
  are absent rather than failing.
- **Failed calls.** A call that fails for authorization returns the fixed text below as
  `ToolResult.content`, with `isError` set.
- **No events.** Authorization writes no Timeline event (principles §4 rule 9).
- **No agent trigger.** Sign-in is a user action through management methods. It is never a tool.

| Case | `ToolResult.content` |
| --- | --- |
| Authorization required or expired | `The MCP server "<id>" requires the user to sign in. Ask the user to sign in to it with /mcp.` |
| `403 insufficient_scope` | `The MCP server "<id>" needs additional permissions for this tool (<scopes>). Ask the user to sign in to it again with /mcp.` |

`<id>` is the configured server id and `<scopes>` the challenged scope names; the parenthesis is
left out when the challenge names none. Neither is a
secret, and the text has no URL, token, code or issuer.

## 9. Interfaces

### 9.1 Management methods (namespace `mcp`, local transports only)

All three are `report` like the rest of `mcp.*`, which keeps the
[migration table](../contracts/session-boundary.md#a4-migration-and-compatibility) true. After
`operation_interrupted`, read `mcp.catalog` to see the outcome. All three are mutating. Targets use
the existing `{source, id, cwd?}` form. `source` is `"user"`, `"project"` or `"plugin"`; a plugin
entry's `id` is `<plugin id>:<server>`, as `mcp.catalog` lists it
([PluginManager §3.7](../plugins/PluginManager.md#37-content-registration)).

| Method | Params | Reply | Notes |
| --- | --- | --- | --- |
| `mcp.auth.start` | `{source: "user" \| "project" \| "plugin", id, cwd?, clientSecret?}` | `{attemptId, authorizationUrl, redirectUri, resource, issuer, expiresAt}` | The entry must be a remote, enabled entry without static credentials. `clientSecret` is accepted only with `oauth.clientId`. `expiresAt` is Unix ms, 10 min ahead. Replaces a pending attempt of the same key |
| `mcp.auth.complete` | `{attemptId, callbackUrl}` | `{}` | §5.4. Returns after the record is persisted |
| `mcp.auth.logout` | `{source, id, cwd?}` | `{revoked: boolean}` | Allowed for disabled entries and for remote entries that no longer use OAuth. No record: `{revoked: false}` |

Per-call progress (C1, PR [#83](https://github.com/arcships/dim/pull/83)) is not needed:
`start` returns the URL directly, and nothing completes an attempt in the background.

### 9.2 Additions to existing replies

- **`mcp.catalog`.** Each entry gains `auth?: {status: "required" | "authorized" | "expired",
  missingScopes?: string[]}`. It is absent for stdio entries, static-credential entries and the
  `absent` state. URLs, the issuer, the client and tokens stay out, as the catalog already
  excludes URLs and credentials.
- **`session.mcp`.** The server `state` vocabulary, owned by Mcp and copied by the protocol
  projection, gains `auth_required`, with `message` `authorization required` or
  `authorization expired`. Unlike `failed`, it clears by itself when the key's credential changes.

### 9.3 Errors (`appError`, code `-32000`)

| `appError` | When | Client action |
| --- | --- | --- |
| `mcp_auth_unsupported` | stdio entry; `auth` or `Authorization` header configured; plain `http` server off loopback; a store in an unknown format version | Stop: fix the entry, or run the Dim version that wrote the store |
| `mcp_auth_discovery_failed` | No PRM and no `authServerMetadataUrl`; `resource` or `issuer` mismatch; no `S256`; non-HTTPS endpoint | Fix the server or set `authServerMetadataUrl` |
| `mcp_auth_client_required` | No client ID, no usable CIMD, no `registration_endpoint`; or a configured `clientId` recorded with another issuer | Set `oauth.clientId`; after an issuer change, sign out first |
| `mcp_auth_attempt_closed` | Unknown, expired, completed or replaced attempt, or one closed by sign-out; a `start` overtaken by a sign-out | Start again |
| `mcp_auth_callback_invalid` | Redirect URI, `state` or `iss` mismatch; no `code` | Paste the right URL; the attempt stays pending |
| `mcp_auth_failed` | The authorization server refused: `error` in the callback, a token error, a registration rejected | Start again after fixing the cause |
| `mcp_auth_unavailable` | Network failure, timeout or 5xx at the authorization server; store I/O | Retry |

Existing codes are unchanged: `mcp_unknown_server`, `mcp_invalid_config`, and
`mcp_unavailable` (also for a disabled entry, as today). Messages never carry secrets,
callback URLs or authorization server `error_description` text from an unvalidated response.

## 10. Decisions and rejected alternatives

| # | Decision | Rejected alternative and why |
| --- | --- | --- |
| D1 | OAuth engine: rmcp 3.5.0 `auth` (`AuthorizationManager`) runs discovery, dynamic registration and the authorization request (PKCE `S256`, `state`, `resource`), driven by `McpAuth` through its own HTTP client and a `StateStore` that hands the verifier and `state` to the attempt. Mcp sends every token request itself (code exchange, refresh, revocation) and closes the gaps of Appendix A | A Dim-written OAuth client would re-track every specification revision that the official SDK already follows. rmcp's high-level `OAuthState` and `AuthClient` hold one manager per HTTP client with in-memory stores by default, fall back silently to guessed endpoints, and cannot share one refresh across Sessions. rmcp's `refresh_token` through a `CredentialStore`: after a restart it sends the base URL instead of the recorded `resource`, reports `invalid_client` only as text, and needs an adapter around the record for a request Mcp already sends for the code exchange |
| D2 | All parts inside Mcp: a process authority, plus the existing Session discovery, connection instances and management | A separate OAuth plugin would only connect Mcp to its own tokens, a wiring module (ADR 002 D2); the managed OAuth host serves fixed providers and knows no MCP discovery. Per-Session credentials would mean one sign-in per Session |
| D3 | Discovery strictly per the specification; `authServerMetadataUrl` applies only when there is no valid PRM; refuse missing PKCE metadata and non-HTTPS endpoints | rmcp's base-URL discovery and synthesized `/authorize`, `/token`, `/register`: the specification requires PRM for discovery and otherwise ends in abort or pre-configured values, and a guessed endpoint can send the user to an unintended page. An override that beats a valid PRM (Claude Code's semantics) would let a project file send the user to another authorization server for a server that names its own |
| D4 | Registration: `clientId`, then CIMD (product URL, once published), then DCR, then `mcp_auth_client_required`. A confidential client's secret is entered at sign-in and kept in the record | A secret in configuration (`clientSecretRef`): `.mcp.json` is shared with the project, and a project entry could point the secret at its own token endpoint; it would also wait for S8b |
| D5 | Redirect URI `http://localhost:{callbackPort}/callback`, default port 1456 (1455 is the account login's) | `127.0.0.1` breaks registrations made for Claude Code's `localhost` form (its v2.1.229 regression); a random port per attempt breaks stable DCR registrations and a fixed CIMD document |
| D6 | The callback always returns through `mcp.auth.complete {attemptId, callbackUrl}`; the CLI captures it with a loopback listener, or the user pastes it | A loopback listener in the daemon is a new external entry inside a business owner and is unreachable over SSH anyway; one long call with progress would hold an RPC open for minutes and still need a listener; the device flow is not in the MCP specification |
| D7 | Three methods, all `report`; no cancel and no attempt-status polling | `drain` would hold Mcp's unload behind authorization-server calls and break the uniform `mcp.*` rule. A cancel method would undo nothing, since a pending attempt has no effect and expires; polling has nothing to poll, because no background party completes an attempt |
| D8 | Credential key = server `id` + canonical URL, process-wide; the record binds `resource` and `issuer` | A URL-only key allows one account per server; `(source, id)` lets a token follow a name to a new URL; per-Session keys contradict process ownership (ADR 002 D1) |
| D9 | Store `mcp-oauth.json` under the home root through FileStorage, atomic 0600 replace, versioned; token requests hold the store lock and settle once sent. S8b adds no credential service, so nothing migrates: tokens are owner state that each refresh rewrites, and every owner handles its own secrets ([settings-service §1.2](settings-service.md#12-decision-secrets-belong-to-their-owner)); every other OAuth holder follows the same rule | The Settings rows in SQLite hold business documents, not secrets; `pluginFiles` is FileStorage's bridge API for non-Rust plugins, while a Rust owner uses FileStorage directly, as the managed OAuth host does; an OS keychain needs a host-wide secret-store decision that covers every OAuth owner; waiting for S8b blocks on an API that cannot hold rotating secrets |
| D10 | Refresh on use with a 30 s margin, plus refresh and one retry on `401`; single flight per key; persist rotation before use | Background refresh timers wake the daemon for idle servers and rotate tokens nobody uses |
| D11 | Step-up is user-initiated: the call fails with fixed text, and the next sign-in requests the union | Automatic re-authorization from a tool call needs a browser in the middle of a turn |
| D12 | Sign-out revokes (RFC 7009, best effort) then deletes the whole record; `mcp.remove` leaves credentials | Deleting on `mcp.remove` cannot know whether another source's entry with the same key still uses them |
| D13 | `session.mcp` gains `auth_required`, which clears by itself; the catalog gains `auth` | Reusing `failed` would require a manual refresh after sign-in and gives the CLI nothing to act on |
| D14 | `start` requires an enabled entry; sign-out is always allowed; one key is shared across sources | Sign-in on a disabled project entry would contact a URL from project files before the user enabled it (ADR 003 D3) |
| D15 | An entry with static credentials never uses OAuth; OAuth needs `https` or loopback `http` | Falling back to OAuth after a static credential fails mixes two credential sources; a bearer token over cleartext can be stolen |
| D16 | Keep rmcp's PRM acceptance: the `resource_metadata` URL must be same-origin, and `resource` may be the server URL or a parent path on the same origin | RFC 9728 does not require a same-origin metadata URL; accepting another origin would let any `401` point discovery at another host. RFC 9728 asks for an identical `resource`; requiring it would mean replacing rmcp's validation, while the parent-path rule keeps the token audience inside the server's origin |
| D17 | Attempt currency (§7): the `attemptId` identifies an attempt, and only the key's current attempt redeems its code, checked after the store lock and again after the token response. One attempts lock orders every change of attempt state; the exchange holds it from the second check until its outcome is published, so a later `start` or sign-out takes effect after the publication. Sign-out closes attempts when it is accepted and when it completes; a `start` prepared before a completed sign-out is refused. A superseded request already sent is settled, and its tokens are revoked best effort under the store lock and discarded. Holding the store lock orders the revocation before a newer attempt's exchange: a server that revokes a whole session then refuses the newer code at sign-in, instead of leaving a stored credential that is already dead | Checking only before the request lets a response still in flight overwrite a newer attempt. Ending the attempt before the store write and publishing after it lets a newer `start` return while the write is pending, so a superseded attempt stores and wakes after the newer one is current. Writing first and rolling back when superseded lets lock-free readers use the tokens in between, and a failed rollback leaves them stored. Cancelling a request in flight breaks "settled once sent" (§4.1): a redeemed code would leave live tokens nobody knows about. Storing them and leaving cleanup to a later sign-out undoes a sign-out the user has already completed. Discarding without revocation leaves the tokens valid at the authorization server until they expire. Taking the store lock in `start` would queue each sign-in behind refreshes of up to 30 s. A generation counter per attempt would duplicate the `attemptId` |

## 11. Security

- **PKCE and `state`.** PKCE is always `S256`, and Mcp refuses servers that do not advertise it.
  `state` is random and single-use, compared in constant time. The verifier and `state` exist
  only in memory, for one attempt.
- **Redirect URI.** The redirect URI is always `http://localhost:{port}/callback`, a form the
  specification allows. The callback must match it exactly. A malicious redirect target gains nothing: a code is useless without the verifier,
  which never leaves the daemon.
- **Mix-up.** Checked through `iss` per RFC 9207 and the recorded issuer. Residual risk: an
  authorization server that neither sends nor advertises `iss` leaves mix-up open, as for every
  conforming client. It closes when the specification makes `iss` mandatory, as it says it
  expects to.
- **TLS.** TLS is required for authorization server endpoints and for the MCP server when a token
  is attached; loopback `http` is the only exception, for local development, and an authorization
  server endpoint may use it only when the MCP server is on loopback too. Every discovery and
  registration request goes through Mcp's HTTP client, which refuses other cleartext URLs and
  never follows redirects. For a remote MCP server, no OAuth URL may name a loopback, private,
  link-local or cloud metadata host, over HTTPS either (rmcp's rule for authorization server
  metadata URLs, applied to every endpoint). Tokens go only to
  the configured URL's origin; the legacy SSE transport already refuses a cross-origin endpoint.
- **Token audience.** `resource` is sent on authorization, token and refresh requests. A token
  is never sent to another server, and `start` never reuses a client or tokens recorded for
  another issuer (§5.2).
- **SSE endpoint.** The SSE POST route is fixed after discovery. A later `endpoint` event ends
  the stream instead of retargeting requests, so the bearer cannot be sent to an unvalidated
  path or origin.
- **Storage.** The store is mode 0600 with atomic replace through FileStorage. No secret goes
  to the Timeline, `ToolResult.content`, the catalog, `session.mcp`, error messages or
  diagnostics. Mcp emits one `tracing` event per auth operation, target `mcp.auth`, with server
  id, issuer host, outcome code and duration.
- **rmcp's debug log.** rmcp 3.5.0 logs the authorization code at `debug` in
  `exchange_code_for_token_with_issuer`, so Mcp sends the code exchange itself (§5.4). rmcp's
  other `auth` logs print tokens only through `oauth2`'s redacting secret types. Nothing depends
  on a host's log filter.
- **Phishing.** Before the browser opens, the CLI shows the server's `resource` and the
  authorization server `issuer`. The DCR `client_name` is fixed to `Dim`, and configuration
  cannot set it.
- **Project entries.**
  - They run only once enabled by the user (ADR 003 D3), and sign-in requires that.
  - Their `oauth` fields carry no secret, so a project file cannot route a user secret anywhere.
  - Sharing a key with a user entry sends the token only to the same URL; enabling a project
    entry is accepted as trusting it (ADR 003 D4).
- **Authorization is not approval.** Signing in is the user's consent at the authorization
  server. Dim keeps no grant, and Approval and Permission still admit each MCP tool call
  (ADR 003 D1).

## 12. Rule check

| Rule | How this design complies |
| --- | --- |
| Principles §4 rule 1 (commands for others' state) | Mcp changes only its own state: its store, its attempts and its connection states. |
| Principles §4 rule 2 (handover) | Not applicable: no object is handed between modules. |
| Principles §4 rule 3 (before callbacks only) | No before callback. Entries check shape and authorization; OAuth checks belong to Mcp. |
| Principles §4 rule 4 (no business meaning in core) | No core change; the Timeline, Dispatcher and Rpc dispatch are untouched. |
| Principles §4 rule 5 (batch) | Not applicable: no events are written. |
| Principles §4 rule 6 (context from handed-over events) | Only tool results that ToolExecutor writes reach the model, with fixed text on authorization failure (§8). |
| Principles §4 rule 7 (public events and services only) | Mcp uses FileStorage, Settings and rmcp; its new service is consumed only by Mcp's own parts. |
| Principles §4 rule 8 (infrastructure unchanged) | No change to the Timeline, Dispatcher, event format, assembly, rutis or the facades. No requirement on rutis, an external dependency. |
| Principles §4 rule 9 (only facts later decisions use) | No Timeline event; credentials are process state changed through Mcp's typed management methods. |
| Principles §1 (Capability and App Server boundary) | A supplement to the existing Mcp process plugin, as Appendix B.2 records; no new App Server business module. |
| ADR 002 D1 (scope follows state) | Credentials are shared across Sessions, so they are process state; connection use stays in the Session-owned instances. |
| ADR 002 D2 (no wiring modules) | No new plugin, port or adapter. |
| ADR 002 D3 (dependencies point to the core) | Mcp gains a dependency on FileStorage, core to core; nothing new depends on an optional plugin. |
| ADR 002 D4 (structural vs business configuration) | `oauth` is business configuration in the `mcp` source, read at use; the token store is the owner's own store. Editing reloads nothing. |
| ADR 002 D5 (language-neutral contracts) | Methods, replies and errors are JSON data; cancellation is the caller leaving, and a pending attempt expires. |

## 13. Requirements raised to mechanism owners

None. F4 needs nothing from the runtime and raises nothing to rutis.

Requirements on the CLI owner (zerob13), stated as behaviour; the CLI's internals are its own:

1. **Sign in.** A `dimcode mcp` sign-in command and the `/mcp` action call `mcp.auth.start` for
   the entry `mcp list` shows, as `mcp enable ID` does.
   - Before opening the browser, show `resource` and the authorization server host from `issuer`.
   - Open the browser best effort.
   - Offer a way to enter a client secret with hidden input. The CLI never echoes, logs or
     stores it.
2. **Return the callback.** When `redirectUri` is loopback and its port is free on both
   `127.0.0.1` and `::1`, capture one matching callback with a short-lived local listener. Forward the full URL with `mcp.auth.complete`, then answer the browser with a static
   page.
   - Otherwise, for example when the port is busy, under SSH, or when the user asks not to open
     a browser, ask for the full callback URL with hidden input, as account login does.
   - On `mcp_auth_callback_invalid`, ask again.
3. **Stop waiting.** On Ctrl-C or at `expiresAt`, stop and print how to start again; the
   attempt needs no cleanup.
4. **Sign out.** A `dimcode mcp` sign-out command and the `/mcp` action call `mcp.auth.logout` and
   report `revoked`. Removing an entry whose `auth.status` is `authorized` offers to sign out first.
5. **Status.** `mcp list` and `/mcp` show the catalog's `auth.status` (needing sign-in, signed
   in, sign-in expired) and `missingScopes`. A Session view shows
   `auth_required` with a sign-in hint.
6. **Non-interactive.** Explicit sign-in requires a terminal and otherwise prints the command to run.
   `--prompt`, exec and piped prompt content remain literal model input, including slash text;
   the CLI never interprets them as a sign-in action.
7. **Errors.** Map the §9.3 codes to the listed actions, and never print a callback URL back.

The native CLI exposes `mcp login ID [--no-browser] [--client-secret]`, `mcp logout ID`,
idle `/mcp`, `/mcp login [--no-browser] [--client-secret] ID` and `/mcp logout ID`.
It uses the public catalog's selected source, including project shadowing. The `--client-secret`
flag requests hidden input, never an argument value. Loopback listeners bind both `127.0.0.1`
and `::1`. Requests must use the configured callback path; unrelated paths receive 404 without
consuming the listeners. Each connection has a two-second header deadline and bounded input;
invalid connections leave capture available. The first matching callback releases the listeners,
preserves its path/query against the known redirect origin and receives a static response after
`complete` settles. Hidden paste remains available while waiting for the browser; the first
callback or pasted line wins. The daemon remains
the only validator of redirect, state, issuer and authorization results.

Ctrl+C or expiry releases the listeners and hidden-input guard. Cancellation exits 130; expiry
exits 1 with an expiry message. A submitted operation that completes successfully exits 0 even
when its waiter was interrupted. Unknown-server errors do not echo unverified command text. An already-submitted RPC is
drained before the interactive prompt returns, preserving the existing response identity.
No attempt cancellation method, token state, background poller or daemon endpoint is added.
The CLI's current configuration surface enables and disables entries; entry removal through
the configuration API retains the sign-out-first requirement above. The CLI interaction is
illustrated in [its README](../../../crates/dim-cli/README.md#mcp-servers).

Other follow-up, not a requirement:

- **CIMD.** Maintainer decision (#111): the project publishes no client metadata document for
  now, and DCR covers servers without pre-registration. It is decided again when a server
  requires CIMD (Appendix C).

## Appendix A. rmcp 3.5.0 `auth`: used and closed gaps

Enabling `auth` adds `oauth2` 5 without default features, `async-trait` and `url`. It reuses
reqwest 0.13, the version `transport-streamable-http-client-reqwest` already uses, so there is
no second HTTP stack. The `xtask check` dependency rules are re-run with the change.

| Used from rmcp | Gap in rmcp 3.5.0 | Closed by Mcp |
| --- | --- | --- |
| PRM discovery (header, well-known, same-origin, `resource` check), authorization server discovery order and `issuer` check, a string-only SSRF filter on the authorization server URLs that PRM names | Falls back to the server origin and to synthesized endpoints, reported only in `AuthorizationMetadataResolution.source` | Uses only PRM-sourced metadata or the configured `authServerMetadataUrl`; refuses the fallbacks (§5.2) |
| Same | Additions to the specification's order: a `/{path}/.well-known/oauth-protected-resource` candidate between the path and root forms; the singular `authorization_server` field before `authorization_servers`; a root `oauth-authorization-server` candidate after the three for an issuer with a path. An `issuer` mismatch aborts discovery instead of trying the next authorization server | Accepted: the extra well-known candidates stay on the origin being discovered and are still checked for `issuer`; the singular field is an older name for the list; the abort fails closed |
| Nothing | Tolerates a missing `code_challenge_methods_supported` with a warning | Refuses it (specification MUST) |
| Nothing | No scheme check on the endpoints inside authorization server metadata | Requires HTTPS except on loopback |
| `configure_client` and `register_client` (DCR); CIMD is not used (§5.3) | Does not persist `client_secret` | The record keeps it and re-applies it per operation |
| `get_authorization_url` (PKCE `S256`, `state`, `resource`; verifier, `state` and expected issuer kept in Mcp's in-memory `StateStore`) | Its own selection unions previously requested, challenged and PRM scopes, and falls back to the authorization server's `scopes_supported` when that is empty | Passes the scopes chosen per §5.5 |
| Nothing | `AuthorizationCallback::from_redirect_url` ignores `error` and does not compare the redirect URI; `exchange_code_for_token_with_issuer` logs the code at `debug` | Mcp parses and validates the callback, `iss` included, and sends the code exchange itself (§5.4) |
| Nothing (`refresh_token` is not used, D1) | `refresh_token` takes `resource` from discovery state that a restarted daemon lacks, has no re-check after locking, and reports `invalid_client` only as text | Mcp's own refresh request under the store lock, with re-check and rotation (§5.6) |
| `StreamableHttpClientTransport` | Its worker waits for the first message without observing cancellation, so closing a transport that never sent one does not finish | The handshake is polled once before cancellation is honored |
| `StreamableHttpError::AuthRequired` and `InsufficientScope` | Any `403` that carries `WWW-Authenticate` is `InsufficientScope` | Mcp reads `error="insufficient_scope"` |
| Nothing | No RFC 7009 revocation | One request per token in `mcp.auth.logout` |
| Nothing | No SSE client transport (removed in rmcp 0.11) | Dim's own legacy SSE transport attaches the lease and reports `401` |

## Appendix B. Acceptance (flow tests)

`crates/dim-app-server/tests/mcp_oauth_flow.rs` drives items 1 to 10 and 13 through `mcp.auth.*`,
`mcp.catalog` and `session.mcp` on a daemon with an isolated home, for streamable HTTP and legacy
SSE. Item 11 is covered through a `403` while discovering; the fixed tool-call text has no flow
test, because the test model makes no tool calls. Item 12, an issuer change, has no flow test yet.

Against a loopback mock MCP server and mock authorization server, in `dim-app-server` flow
tests, using an isolated home:

1. **Unauthorized.** An unauthorized connection reaches `session.mcp` `auth_required` and catalog
   `auth.status` `required`, and no tools are published.
2. **Sign-in.** `start`, then `complete`, discovers through `resource_metadata` and registers
   by DCR. The authorization URL carries `S256`, `state` and `resource`, and the token request
   carries `code_verifier` and `resource`. The waiting Session reconnects without a refresh call,
   publishes the tools, and every request carries the bearer.
3. **Refresh.** An access token near expiry is refreshed before use. The rotated refresh token
   is persisted, and the old one is never sent again. A refresh in flight when Mcp unloads is
   still persisted.
4. **Shared refresh.** Two Sessions get `401` at the same time: the authorization server sees
   exactly one refresh, and both requests succeed on retry.
5. **Reauthorization.** A `401` after refresh, or `invalid_grant`, gives `expired` and
   `auth_required`. A new sign-in recovers both Sessions.
6. **Sign-out.** `logout` sends revocation, deletes the record and replies `revoked: true`. The
   next request ends `auth_required`.
7. **Refusals.** None of these sends a token request:
   - a wrong `state`, redirect path or `iss` (the attempt stays pending);
   - metadata without `code_challenge_methods_supported`;
   - plain `http` on a non-loopback host;
   - a server without PRM or a configured override.
8. **Secrets.** The store has mode 0600. No token, code, verifier or client secret appears in
   the Timeline, `ToolResult.content`, replies, errors or captured `tracing` output, with
   `RUST_LOG=debug`.
9. **Project entries.** On a disabled project entry, `start` is refused. Once the user enables
   it, it uses the record of the same key.
10. **Restart.** A daemon restart keeps tokens and drops attempts, so `complete` answers
    `mcp_auth_attempt_closed`.
11. **Step-up.** A `403 insufficient_scope` fails only that call, with the fixed text. The catalog
    shows `missingScopes`, and the next `start` requests the union.
12. **Issuer change.** When PRM names a new authorization server, `start` registers a DCR client
    again, and refuses a configured `clientId` with `mcp_auth_client_required` until sign-out.
13. **Superseded attempts.** Each race is held open at the mock authorization server, or at the
    store write through the `testing` feature's hook, because FileStorage cannot be paused from
    outside:
    - While a refresh holds the store lock, a sign-out closes the pending attempt and its
      callback is refused. The code is never sent, nothing is stored, and the Sessions require
      sign-in.
    - A newer `start` supersedes an attempt whose exchange waits for the lock; its code is never
      sent. A second callback for that attempt is refused while it waits.
    - A newer `start` supersedes an attempt whose token request is out; the tokens are revoked
      and never stored or used.
    - A newer `start` that finishes discovery while an exchange past its final check is writing
      its record returns only after that publication. Completing the newer attempt then replaces
      the record.
    - A sign-out that completes during a `start` refuses that start, and the next `start`
      registers a new client.

## Appendix C. Deferred

| Item | Why it does not block F4 | Owner |
| --- | --- | --- |
| Publishing Dim's client metadata document (CIMD) | Maintainer decision (#111): not published for now; DCR and `clientId` cover servers without it. Decided again when a server requires CIMD; turning it on then needs a stable project HTTPS URL | Maintainer (release) |
