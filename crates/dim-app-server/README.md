# App Server

The Rust CLI uses this server over a private Unix socket (NDJSON) or Windows named pipe. The server owns Agent v4 execution, approvals, model/provider selection and native storage. Product and configured profiles require protected local transports for execution and management; their WebSocket endpoint supports only `hello` and `ping`. The explicit mock profile permits WebSocket execution for protocol testing.

Commands below run from the repository root:

```sh
cargo run --locked -p dim-app-server -- \
  --product-home /absolute/product-home \
  --home /absolute/product-home/rust \
  --unix /absolute/product-home/rust/daemon.sock --no-ws
```

A configured model file uses the same public execution capabilities: workspace tools, approvals,
Skills, prompt sources, MCP, branches and collaboration. Provider selection does not select a
different harness. Account/provider management remains specific to the product provider.

```sh
cargo run --locked -p dim-app-server -- \
  --model-config /absolute/models.json \
  --home /absolute/development-home \
  --unix /absolute/development-home/daemon.sock --no-ws
```

Configured execution requires `--home` or `--database`; the CLI supplies `--home` automatically.
Its legacy user settings and shell preference live under the native home (or next to the
explicit database). The product profile uses product-home for these files. User MCP and Skill
settings are imported independently on each source's first successful read; original files
remain intact but cease to be authority after import. Invalid files or failed imports leave
that source unimported for repair and retry, without an empty fallback. Shell preferences remain files.
Workspace `.mcp.json` files remain independent sources. A missing account plugin does not disable
these public capabilities. Protocol v3 advertises their management methods on local connections.
Configured model changes validate against the supplied catalog and adopt that model's generation
settings; model preferences requiring capability declarations are rejected rather than guessed.

### Business settings

SettingsService reads and updates generic `BusinessDocument` values with per-source
CAS. Business owners define and validate their payloads: MCP types live in
`mcp/config.rs`, and the Skills owner supplies the existing `SkillSettingsHost`.
Settings owns source persistence, not approvals or business management methods.
There is no `configuration.*` RPC namespace.

Native v13 retains `settings_documents` with `user/mcp`, `user/skills`, `user/mcp-projects` and `user/permission` keys,
plain payload JSON (not `{kind, document}`), source-local versions and opaque
`native:<source>:<version>` revision tokens. Import provenance is preserved on
edits. Successful writes notify consumers after persistence; failed persistence
does not publish unsaved intent. Business edits do not reload plugins. Structural
loader configuration and package installation remain separate work. See
[ADR 004](../../docs/adr/004-loader-plugin-management.md).

### MCP configuration management

On protected local protocol-v3 connections, `mcp.catalog({cwd?})` returns the merged catalog,
`userRevision` and optional `projectRevision`, without commands, URLs, credentials or environment
values. Edits target `source: "user" | "project"`, an `id`, and that source's `expectedVersion`.
Project edits require an authorized workspace `cwd`; `mcp.save` and `mcp.remove` write `.mcp.json`,
not the user source.

- `mcp.save` additionally takes `configuration`: a complete stdio, HTTP or SSE server definition
  in the existing `mcpServers` format. It creates or replaces that source's entry, not a merged one.
- `mcp.remove` removes the entry from the selected source; it can reveal a lower-precedence entry.
- `mcp.set_enabled` additionally takes `enabled`. A user entry's `enabled` field changes and the
  rest of the entry is preserved. For a project entry, enabling checks `expectedVersion` against
  `projectRevision` and the entry's presence; the choice is stored only in the user's local record
  (below). Disabling a project entry always succeeds against that record, even if `.mcp.json` is
  unreadable or no longer defines the entry, and `mcp.remove` withdraws it before editing the file.
- `mcp.disconnect` takes `source`, `id` and optional `cwd`, without a revision; it does not edit
  persisted intent.

Approval and Permission govern only model tool calls ([ADR 003](../../docs/adr/003-approval-scope.md)).
A configured server runs when it is enabled; there are no grants, fingerprints or approval methods,
and `mcp.approve`/`mcp.revoke` return `unknown_method`. Project entries start disabled: a project
file cannot enable its own entries, including with its own `enabled` field, which is ignored.
The user enables a project entry with `mcp.set_enabled`. That choice is stored in the user's local
record `mcp-projects` (`user/mcp-projects` in native storage, `mcp-projects.json` in a standalone
home), keyed by canonical workspace and server id. It holds no configuration and never enters
project files or Session history. An unreadable record disables every project entry and is
reported in `projectApplication.error`; user entries stay available. A record outlives a deleted
workspace, so a different project later cloned at the same path with the same id is enabled.
After enabling, later edits to that entry's command or arguments take effect without another
decision (ADR 003 D4). FullAccess, file-root authorization and Session restoration enable nothing.
An old `mcp-approvals.json` is ignored, not migrated or removed.

Successful configuration mutations return `{}` after persistence, not after connection/tool
adoption; disabling or removing an entry also waits until its running generations have closed in
every Session. The catalog also returns `userApplication` and optional `projectApplication`, each with
`appliedRevision`, `error` and `cleanupError`. Application describes source eligibility
and instance creation failures; it does not confirm a network connection or tool adoption.
An application error leaves persisted intent intact. Cleanup errors remain visible for that
source during this publisher's lifetime, including after later successful application.
Read `session.mcp` for runtime state. A conflict requires rereading the source;
interruption may follow an already committed write, so never retry
blindly. Use these methods rather than editing imported user files. `skills.set_enabled` likewise
persists in SQLite; legacy `skills.json` is only an import source. Private server configuration
stays inside its owning service and is excluded from catalog replies, notifications and diagnostics.

### MCP OAuth

HTTP and SSE servers that answer `401` use OAuth ([design](../../docs/design/appserver/mcp-oauth.md)).
It applies to a remote entry without `auth` or an `Authorization` header whose URL is HTTPS or
loopback HTTP; an entry's optional `oauth` object (`clientId`, `callbackPort`,
`authServerMetadataUrl`, `scopes`) uses Claude Code's field names and carries no secret.

- `mcp.auth.start({source, id, cwd?, clientSecret?})` needs an enabled entry. It discovers the
  authorization server through Protected Resource Metadata (or `oauth.authServerMetadataUrl` when
  the server publishes none), registers a client when needed, and returns
  `{attemptId, authorizationUrl, redirectUri, resource, issuer, expiresAt}`. The redirect URI is
  `http://localhost:{callbackPort}/callback` (default 1456); the daemon runs no listener.
- `mcp.auth.complete({attemptId, callbackUrl})` takes the full callback URL, captured by the
  client's loopback listener or pasted by the user, validates it and exchanges the code. A wrong
  redirect, `state` or `iss` leaves the attempt pending.
- `mcp.auth.logout({source, id, cwd?})` revokes the tokens (RFC 7009) and deletes the record,
  replying `{revoked}`. It is allowed for disabled entries and for entries that no longer use
  OAuth.

Credentials are keyed by server id and canonical URL, shared by every source and Session, and
stored in `mcp-oauth.json` under the home root (version 1, mode 0600, atomic replace). Every
request takes a bearer; tokens are refreshed 30 s before expiry and once after a `401`, one
refresh per key across Sessions and daemons sharing the home. `mcp.catalog` entries gain
`auth: {status: "required" | "authorized" | "expired", missingScopes?}`, and `session.mcp`
reports `auth_required` until the key's credential changes, when the Session reconnects by
itself. Tokens, codes and secrets never reach replies, errors, tool results or logs.

Native storage remains v13 with lazy source-local import and durable source CAS.
Existing v13 document bytes remain compatible. Unsupported databases, including
the removed branch-only v14 representation, are rejected and preserved; there
is no migration command, save queue or flush operation. See the
[storage contract](../../docs/design/appserver/native-storage-format.md).

Each Session owns its MCP connections as nested instances in the existing loader.
The host coordinates source selection and retirement without rewriting root layers.
Catalog reads and connection operations also request refresh of external workspace-file edits;
there is no background filesystem watcher. A catalog reply is not a refresh-completion barrier.
Already bound tools retain their connection generation while a replacement takes over, so a
request in flight finishes on the configuration it started with. After the last old binding is
released, the coordinator closes that instance through the loader's fiber. Disabling an entry
closes its generations in every Session, including bound ones, even while its source is
unreadable: the coordinator applies the user's current choice, or an unreadable record, to the
entries it last read. Removing an entry does so once its source is read; an unreadable source
closes nothing else. `session.mcp` reports a withdrawn entry as `disabled`, not `failed`. Sessions
re-read their selection when the coordinator's applied state changes, so an external edit that a
catalog read or connection operation picks up reaches every Session. A generation that no lease uses closes even while current, such
as a user server that a project entry now shadows. Session closure drains all its instances, including those
retained by old bindings, without affecting another Session using the same server configuration.
Explicit disconnect drains all generations of the selected source/server, without changing its
configuration; a later acquisition can reconnect. It reports cleanup failure rather than treating
loader reconciliation as proof of successful cleanup.

Standalone hosts using `McpPlugin::new(home_root)` register `McpPublication` and its
`standalone_template` with their `Builtins` and include both rows in the immutable
assembly. The publication plugin owns its coordinator worker: it starts after
apply and is settled, with its cleanup result, when the plugin unloads. Pass an
explicit `McpOwner::new(ctx, loader)` when acquiring connections. Agent hosts
instead use `mcp::install` and the declared Session loader. Its typed port is a
dependency of `McpPlugin`; connection leases never become a second lifecycle engine. No second
loader, editable-layer target or SQLite dependency is required by this port.

### Permission command rules

With local execution, `permission.rules({})` returns `{revision, commandRules}` and
`permission.set_rules({expectedRevision, commandRules})` replaces the whole list and returns the new
`revision` (`permission_rules_conflict` for a stale revision, invalid params for a rule that is
empty, contains a shell separator or has an unknown field). Each rule is
`{command, match: "exact" | "prefix", decision: "allow" | "deny"}`, compared word by word. The
list is the user source `permission` (`user/permission` in native storage, `permission.json` in a
standalone home); there is no project source, so a workspace file grants nothing. Permission reads
it on each `exec`/`execute` decision through the Session part that `permission_rules::install`
adds to the product profile: a deny rule denies under every preset, an allow rule only answers an
approval question, and an unreadable source denies command calls. Rules never enter the Timeline
and paired devices cannot read or write them. See
[permission command rules](../../docs/design/appserver/permission-command-rules.md).

`--mock-model` selects the explicit deterministic test profile. It uses mock model/tool hosts. Product, configured and mock daemons identify themselves in `hello.daemonProfile`:

```json
{"kind":"product"}
{"kind":"configured","modelConfigSha256":"<64 lowercase hexadecimal characters>"}
{"kind":"mock"}
```

The configured digest is SHA-256 of the same original bytes parsed and validated for that server launch. It excludes neither whitespace nor environment-variable reference names, and never prints file contents or resolved environment values. A client must refuse another profile or configuration rather than reuse, delete or terminate its daemon.

`--home` and `--database` are mutually exclusive. SQLite format v13 includes user settings, normalized managed-provider records and optional recovery snapshots separate from Timeline facts. Unsupported formats, including v12, are rejected and preserved; there is no automatic database migration. [Native storage format](../../docs/design/appserver/native-storage-format.md) defines the SQL and JSON contract.

Unix endpoint directories must be private (0700), sockets use 0600, and cleanup removes only the inode acquired by this listener. Named pipe ownership and local identity checks remain in the transport. Ordinary client disconnect detaches; it does not delete a session. See the [public architecture](../../docs/architecture/README.md).

### Shutdown completion

For authorized transports, v3 `hello.features` includes `daemon.shutdown.completed`. A successful `daemon.shutdown` response with `force: false` (the default) certifies that Agent work, trace writers, connections and the Rutis process tree have settled, including storage and plugin disposal. Cleanup failure returns `shutdown_failed` without exposing disposer error details. EOF alone is not success. `force: true` retains its early acknowledgement and does not promise cleanup.

The requesting connection hands only its final-response writer to the existing process coordinator before leaving the plugin graph. The coordinator sends successful completion after `root.shutdown()`; no graph-owned RPC task waits on its own disposal. Shutdown first stops RPC admission and joins admitted shutdown handlers, including force acknowledgement writes. If an admitted force request wins, the coordinator joins connection handoffs and returns `shutdown_failed` to pending graceful callers without waiting for business cleanup. Reply writes are bounded by the transport write deadline. Client disconnect does not cancel accepted cleanup. Clients must check the feature before relying on this completion contract from an older daemon.

## Optional managed provider

The public server runs without account plugins. API-key and supported native subscription providers do not require Node. An explicitly supplied plugin adds managed providers through rutis 0.10.0:

```sh
dim-app-server --product-home /absolute/product-home \
  --home /absolute/product-home/rust --unix /absolute/product-home/rust/daemon.sock --no-ws \
  --plugin-dir /absolute/plugin-package \
  --node /absolute/node-executable \
  --js-runtime /absolute/runtime/node_modules/@arcships/rutis-runtime
```

All three plugin flags are required together. `--js-runtime` names the installed package directory, not a bootstrap script. Node is started by absolute executable path. The runtime receives no terminal stdin, stdout or stderr; business output is returned through the server protocol. The runtime and plugin are separate rutis loader rows in the existing process tree.

The plugin package contains `package.json`, its ESM entry and `dim-plugin.json`:

```json
{
  "schemaVersion": 1,
  "id": "my-managed-provider",
  "version": "1.0.0",
  "hostApiVersion": 2,
  "rutisVersion": "0.10.0",
  "entry": "dist/index.js",
  "files": {"dist/index.js": "<sha256>"}
}
```

The host checks compatibility and confines the canonical entry path to the plugin directory. Release staging verifies download and file digests before loading or packaging code. An explicitly configured invalid or incompatible plugin fails startup. Without plugin configuration, no Node process starts.

The ESM entry is a `definePlugin` export from `@arcships/rutis` 0.10.0. It declares `managedProvider` in `provides`, with every method `async`, and receives `{home: absoluteProductHome}` as its configuration. Bundle its SDK/business runtime dependencies or supply them inside its own package. Plugin module resolution must not rely on this repository's development installation.

```ts
import { definePlugin } from '@arcships/rutis'
export default definePlugin({
  name: 'ManagedProvider',
  provides: { managedProvider: { providers: 'async', status: 'async' } },
  apply(ctx, config) {
    ctx.provide('managedProvider', accountService)
    ctx.effect(() => accountService.close())
  },
})
```

This fragment shows declaration and ownership; a usable account plugin implements the full service contract below. It owns account schema, authentication, token refresh and catalog normalization. The Rust provider host does not interpret supplier-specific responses. Host API 2 requires `quiesce` and offers native file transactions; API 1 packages are rejected rather than silently losing shutdown guarantees.

### Trusted plugin storage

A plugin declares `inject: ['pluginFiles']` and opens a fresh scope during each `apply`. Register `scope.close()` with `ctx.effect` immediately after opening, then register business cleanup. The host chooses the authorized product root; the plugin supplies relative file paths only. This is a trusted-plugin API, not a sandbox for untrusted Node code.

```ts
type FileResult<T> = { ok: true; value: T } |
  { ok: false; error: 'closed' | 'exists' | 'invalid_data' | 'invalid_path' | 'internal' | 'io' }
interface FileLease {
  read(): Promise<FileResult<string | null>>
  replace(text: string): Promise<FileResult<null>>
  backup(suffix: string): Promise<FileResult<null>>
}
interface FileScope {
  withLock<T>(path: string, work: (file: FileLease) => Promise<T>, cancelled?: Promise<void>): Promise<FileResult<T>>
  quiesce(): Promise<void>
  close(): Promise<void>
}
interface PluginFiles { open(): Promise<FileScope> }
```

`withLock` holds the native FileStorage lease through the complete callback, including asynchronous work. Reads validate the storage record below and return its UTF-8 text; a missing file returns null. Replacement is atomic and flushed, with Unix mode 0600 and native Windows DACL handling. `backup` creates a flushed, exclusive sibling copy with an alphanumeric/hyphen suffix. A failed partial backup is retained; an existing backup is never overwritten. Storage errors expose only fixed codes, never file contents or exception text. An error after replacement does not imply rollback or authorize replaying remote side effects.

Use a fresh suffix for each backup attempt, such as `before-reset-<UUID>`. An `exists` result does not prove the earlier backup completed. After a failed backup, stop the destructive operation, address the storage failure, and create a successful backup with a new suffix before proceeding. Retained partial backups require explicit operator inspection and cleanup; the lease has no deletion API. Host worker failures return `internal`, distinct from invalid arguments or paths. Callback exceptions retain Rutis rejection semantics and still revoke the lease.

Quiesce rejects new transactions and cancels acquisition, but permits an acquired transaction to persist before close joins it. The optional Promise cancels acquisition when it settles; Node must settle it on ordinary completion too so a pending exported Promise cannot hold runtime drain open. Rutis 0.10 does not encode Node AbortSignal arguments; do not pass one. Escaped file callbacks are revoked before releasing the lease. Abandoning the caller does not abandon issued I/O. Runtime loss revokes the lease after admitted native I/O settles; it cannot guarantee persistence of an external side effect whose result died with the runtime.

Credential bytes, including refresh tokens, may pass through this trusted local bridge. They must never be forwarded to terminal RPC, model requests, history, diagnostics or logs. OAuth policy and payload schema remain in the private package. Startup follows current loader row identities: Rutis may replace an offline JS row when it discovers its declared injections, so a disposed provisional fiber is not itself a startup failure. A row that remains disposed consumes the remaining startup deadline before reporting its terminal state; an explicit failed state still fails immediately.

#### Storage record format

PluginFiles owns a UTF-8 JSON storage envelope, independent of the plugin's
payload schema:

```json
{"version":1,"text":"opaque plugin-owned UTF-8 text"}
```

Both fields are required; `version` is the integer `1`, `text` is a string,
and additional fields are invalid. The host does not parse or validate `text`.
`replace(text)` atomically writes this envelope through the existing FileStorage
lease, preserving the existing permissions, durability and cancellation rules.
`read()` decodes and validates the envelope at this external storage boundary
and returns only `text`; a missing file still returns `null`. The named service,
method arguments and successful reply shapes do not change.

An unversioned file, malformed record, unknown version or invalid UTF-8 returns
the existing fixed `invalid_data` error, without file contents or parser details.
Read failure never rewrites or deletes the original. There is no automatic
migration, fallback to empty state or interpretation of an unknown format.
No distributed Rust App Server build used the unversioned PluginFiles format.
Files from before v1 are development data: the owning plugin and data owner must
explicitly decide whether to recreate that state or preserve it for manual
recovery. The host never resets, converts or deletes these files on read. This
baseline is separate from native SQLite storage and changes no database format.

`backup(suffix)` remains a byte-for-byte copy of the stored file, including its
envelope or any rejected legacy bytes. A backup must not decode, re-encode or
upgrade its source. An explicit `replace` writes version 1; the caller still owns
the decision to replace existing data and to make a successful backup first.
Binary payload support is outside S10 and remains P3 work.

##### Rule check

§1 and ADR 002 D1–D3 keep storage in the existing process plugin and
FileStorage dependency, without a wiring layer or new lock. §4 rules 1–6 and 9
are unchanged: there are no Timeline events, handovers, before callbacks or
context changes. §4 rule 7 keeps storage validation at its owner. ADR 002 D4
leaves plugin business settings and payload schemas with their owners; D5 keeps
the existing language-neutral service and error data unchanged.

##### Requirements raised to mechanism owners

None (§4 rule 8). Existing atomic
replacement and backup primitives cover the format. Acceptance uses disposable
plugin roots to verify round-trip text, stored version, reopen, version rejection
with byte preservation and unchanged transaction/backup/lifecycle behavior.

## Service contract

Every call returns one JSON envelope:

```ts
type Result<T> =
  | { ok: true; value: T }
  | { ok: false; error: { code: string; message: string } }

type Provider = { providerId: string; displayName: string; loginMethods: Array<'browser' | 'device'> }
type Account = {
  credentialEpoch: string
  generation: number
  account?: { issuer: string; subject: string; grantedScopes: string[] }
}
```

`providers()` and `quiesce()` have no arguments. Every other method receives one DTO, carried as `[dto]` by rutis. `providerId` is always explicit. There is no default managed supplier.

| Service method | DTO | Successful value |
| --- | --- | --- |
| `providers` | None | `Provider[]` |
| `quiesce` | None | `{}` |
| `status`, `recover`, `logout` | `{providerId}` | `Account` |
| `beginBrowser` | `{providerId,directory,email?,redirectUri}` | `{attemptId,authorizationUrl,expiresAt}` |
| `beginDevice` | `{providerId,directory,email?}` | `{attemptId,verificationUri,verificationUriComplete?,userCode,expiresAt}` |
| `completeBrowser` | `{providerId,attemptId,callbackUrl}` | `Account` |
| `attemptStatus` | `{providerId,attemptId}` | `{status,account?,error?}` |
| `cancelAttempt` | `{providerId,attemptId}` | `{}` |
| `catalog` | `{providerId}` | `{account,models:CatalogModel[]}` |
| `credential` | `{providerId,modelId}` | `CredentialLease` |
| `recoverRejected`, `finishRejected`, `release` | `{providerId,leaseId}` | Lease, `{}`, `{}` respectively |

Attempt statuses are `pending`, `completed`, `failed`, `cancelled` and `expired`. A completed attempt includes its non-secret account snapshot. Account and login operations are exposed as `account.providers`, `account.status`, `account.recover`, `account.logout`, `account.login.begin`, `account.login.browser.begin`, `account.login.browser.complete`, `account.login.status` and `account.login.cancel`. Existing `account.codex.*` and `account.grok.*` retain their native contracts.

Internal model operations use these normalized types:

```ts
type Protocol = 'openai-compatible' | 'openai-responses' | 'anthropic'
type CatalogModel = {
  entry: ModelEntry
  protocol: Protocol
  unavailableReason: string | null
  zdr: boolean
}
type CredentialLease = {
  leaseId: string
  account: Account
  protocol: Protocol
  baseUrl: string
  headers: Record<string, string>
  bearer: string
}
```

`ModelEntry`, capabilities and metadata are defined in [`model_catalog.rs`](src/model_catalog.rs) and `dim-agent-types`. A plugin supplies final public model descriptions and public diagnostics. An unavailable model keeps a reason and remains unselectable. The host validates duplicate IDs, supported protocols and safe positive model limits once at the cross-language boundary. It does not reproject normalized plugin models through bundled provider rules.

`baseUrl` is the final protocol base URL; the existing model adapter adds its protocol endpoint. Headers and bearer are request-local secrets. `leaseId` is an opaque short-lived handle bound to the plugin runtime, account and credential revision. A plugin rejects expired/old handles and bounds their count and lifetime. `recoverRejected` performs one controlled recovery, and `finishRejected` settles rejection of the recovered credential. Host recovery is limited to HTTP 401; unrelated failures retain the account.

Credential methods are internal to the model resolver and never appear in terminal RPC, history, Timeline, configuration replies or logs. Refresh tokens stay inside the plugin. Rust releases a lease when its last request/recovery owner drops it. A withdrawn binding rejects new calls, closes its generation's task tracker and drains already admitted promises. A caller disappearing does not drop an accepted token rotation. Runtime loss invalidates old handles and is not automatically replayed or restarted; API-key providers keep working.

Before draining Agent/RPC work, the host calls the idempotent `quiesce()` on the retained provider dispatch. Binding unload does the same before waiting for its generation. Quiesce stops admission and cancels preflight/lock waiters without awaiting those callers; it must not abandon an issued refresh or revoke delivered credentials. `release` remains available until final disposal. The plugin's unload disposer then joins accepted work. Rutis consumer-first disposal cannot substitute for this pre-drain phase.

Plugin error messages are not forwarded. The host maps canonical `invalid_store`, `unsupported_schema`, `identity_rejected`, `invalid_input`, `invalid_callback`, `config_error`, `attempt_not_found`, `attempt_closed`, `token_missing`, `refresh_rejected`, `account_changed`, `account_recovery_required`, `account_reset_blocked` and `service_reloading` into fixed public diagnostics. Malformed responses fail explicitly. Account corruption and unavailable plugins are not represented as a signed-out account.

## Catalog and storage consistency

The provider host stores normalized catalog caches, user model overrides, disabled model IDs and their revision in the existing `providers` table. Managed records have `kind: managed` and an account snapshot; token data is absent. Static records retain their own schema.

A catalog refresh captures the current SQL record version before its plugin request. Its commit uses that version as a compare-and-swap guard, so a late request cannot overwrite a newer catalog. Account generation is checked before and after commit. An account change fails the operation rather than returning successful stale data. Cache lookup and request credential resolution require the same account epoch/generation; a previously stored snapshot can never authorize the next account.

There is no atomic transaction spanning the plugin's account file and SQLite. A generation change during an admitted SQL write can leave an older, unusable cache record; the next explicit refresh replaces it. The host does not hold an account lock across IPC or retry state-changing operations implicitly.

## Verification

Public API-key, provider, native subscription and transport flows do not need Node:

```sh
cargo test --locked -p dim-app-server --test provider_flow --test managed_oauth_flow --test model_catalog_flow --test transport_flow
```

One separately enabled test loads a neutral SDK plugin through the actual rutis Node runtime. It covers normalized catalog selection, HTTP 401 recovery, lease release, secret-free replies/history and a runtime crash followed by an API-key request. It does not require a private account package:

```sh
npm ci --prefix runtime --ignore-scripts
DIM_TEST_NODE="$(command -v node)" \
DIM_TEST_JS_RUNTIME="$PWD/runtime/node_modules/@arcships/rutis-runtime" \
DIM_TEST_RUTIS_SDK="$PWD/runtime/node_modules/@arcships/rutis/src/index.mjs" \
  cargo test --locked -p dim-app-server --test transport_flow \
  node_managed_provider_recovers_credentials_and_failure_is_local -- --ignored
```

The [C4 Session TypeScript spike](../../docs/design/session-typescript-spike.md)
has a separate executable probe for per-Session loader rows, named service and
broadcast isolation, cancellation, unload and cross-process call measurements.
It installs no production capability bridge or Session RPC methods.
