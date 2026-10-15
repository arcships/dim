# App Server

The Rust CLI uses this server over a private Unix socket (NDJSON) or Windows named pipe. The server owns Agent v4 execution, approvals, model/provider selection and native storage. Product and configured profiles require protected local transports for execution and management; their WebSocket endpoint supports only `hello` and `ping`. The explicit mock profile permits WebSocket execution for protocol testing.

The local web entry for the [Web workspace](../../docs/design/web-workspace.md) is opened at runtime with `web.open` from a local connection. It binds loopback only, serves `--web-assets PATH`, and admits browsers that exchanged the bootstrap token from `web.open`'s URL for a cookie. Its connections speak protocol v3 and get Session methods (including execution Session methods) plus read-only `provider.list`, `provider.catalog` and `provider.selection`; other management methods and `daemon.shutdown` are `unknown_method`. `web.close` revokes every token and detaches web connections.

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
settings are imported once into the native database on their first successful read; original
files remain intact but are no longer read or written after import. Invalid files or failed
imports retain file authority and can be repaired before retrying. Shell preferences remain files.
Workspace `.mcp.json` files remain independent sources. A missing account plugin does not disable
these public capabilities. Protocol v3 advertises their management methods on local connections.
Configured model changes validate against the supplied catalog and adopt that model's generation
settings; model preferences requiring capability declarations are rejected rather than guessed.

### MCP configuration management

On protected local protocol-v3 connections, `mcp.catalog({cwd?})` returns the merged catalog,
`userRevision` and optional `projectRevision`, without commands, URLs, credentials or environment
values. Edits target `source: "user" | "project"`, an `id`, and that source's `expectedVersion`.
Project edits require an authorized workspace `cwd`; they write `.mcp.json`, not the user source.

- `mcp.save` additionally takes `configuration`: a complete stdio, HTTP or SSE server definition
  in the existing `mcpServers` format. It creates or replaces that source's entry, not a merged one.
- `mcp.remove` removes the entry from the selected source; it can reveal a lower-precedence entry.
- `mcp.set_enabled` additionally takes `enabled` and preserves the rest of the selected entry.
- `mcp.disconnect` takes `source`, `id` and optional `cwd`, without a revision; it does not edit
  persisted intent.
- `mcp.approve` takes required `cwd`, `id` and `approvalToken` from the catalog. It grants only
  the reviewed project's exact effective configuration, not future changes or another workspace.
- `mcp.revoke` takes required `cwd` and `id`. It removes that project's grant and waits for
  generation cancellation and transport cleanup; it does not undo earlier external effects.

Project MCP requires explicit execution approval before discovery, for stdio, HTTP and SSE.
Catalog entries expose `approval` (`notRequired`, `required`, `approved`) and `approvalToken`;
the token is a digest, not a credential. `enabled` expresses configuration intent independently
of approval. Sessions report `approval_required` for enabled but unapproved servers. FullAccess,
file-root authorization, session restoration and same-name user entries never approve a project.
Approval may immediately start discovery in active sessions. Only protected-local v3 management
can grant or revoke; these operations are not model tools.

Grants use the host's `mcp-approvals.json` (version 1, private file permissions on Unix), outside
project configuration and Session history. Native and standalone Settings hosts share this format;
the SQLite format is unchanged. Corrupt or inaccessible grant storage fails closed. Approval
compares current configuration with the caller's digest before persistence; stale tokens return
`mcp_settings_conflict`. Commands, arguments, environment overrides, resolved cwd, URLs, headers
and authentication are bound by the digest, but changes to referenced scripts/binaries or inherited
environment are not. This is configuration approval, not an OS sandbox; see the
[trust contract](../../docs/adr/001-harness-foundation.md#trust-and-cross-language-calls).

Successful configuration mutations return `{}` after persistence, not after connection/tool
adoption. The catalog also returns `userApplication` and optional `projectApplication`, each with
`appliedRevision`, `error` and `cleanupError`. Application confirms that the published rows are
active or deliberately disabled; it does not confirm a network connection or tool adoption.
An application error leaves persisted intent intact. Cleanup errors remain visible for that
source during this publisher's lifetime, including after later successful application.
Read `session.mcp` for runtime state. A conflict requires rereading the source;
interruption may follow an already committed write, so never retry
blindly. Use these methods rather than editing imported user files. `skills.set_enabled` likewise
persists in SQLite; legacy `skills.json` is only an import source. Private server configuration
stays inside its owning service and is excluded from catalog replies, notifications and diagnostics.

The host publishes source changes to the existing root loader, preserving the assembly layer.
Catalog reads and connection operations also request refresh of external workspace-file edits;
there is no background filesystem watcher. A catalog reply is not a refresh-completion barrier.
Already bound user-source tools retain their connection generation during replacement. After the last old
binding is released, the publisher removes that generation's row and the loader disposes it.
Project generations instead close on configuration change, disablement or revoked approval,
even when a model request retains their tools. Each project operation rechecks current authority.
Explicit disconnect drains all generations of the selected source/server, without changing its
configuration; a later acquisition can reconnect. It reports cleanup failure rather than treating
loader reconciliation as proof of successful cleanup.

Standalone hosts using `McpPlugin` register `McpPublication` with their `Builtins`, include its
returned row in the immutable assembly, and run its publisher outside the plugin tree after
assembly activation. Join the publisher before root shutdown. Its typed port is a dependency of
`McpPlugin`; neither the directory nor connection leases spawn or dispose configured plugins.
No second loader, editable-layer target or SQLite dependency is required by this port.

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

`withLock` holds the native FileStorage lease through the complete callback, including asynchronous work. Reads require UTF-8; a missing file returns null. Replacement is atomic and flushed, with Unix mode 0600 and native Windows DACL handling. `backup` creates a flushed, exclusive sibling copy with an alphanumeric/hyphen suffix. A failed partial backup is retained; an existing backup is never overwritten. Storage errors expose only fixed codes, never file contents or exception text. An error after replacement does not imply rollback or authorize replaying remote side effects.

Use a fresh suffix for each backup attempt, such as `before-reset-<UUID>`. An `exists` result does not prove the earlier backup completed. After a failed backup, stop the destructive operation, address the storage failure, and create a successful backup with a new suffix before proceeding. Retained partial backups require explicit operator inspection and cleanup; the lease has no deletion API. Host worker failures return `internal`, distinct from invalid arguments or paths. Callback exceptions retain Rutis rejection semantics and still revoke the lease.

Quiesce rejects new transactions and cancels acquisition, but permits an acquired transaction to persist before close joins it. The optional Promise cancels acquisition when it settles; Node must settle it on ordinary completion too so a pending exported Promise cannot hold runtime drain open. Rutis 0.10 does not encode Node AbortSignal arguments; do not pass one. Escaped file callbacks are revoked before releasing the lease. Abandoning the caller does not abandon issued I/O. Runtime loss revokes the lease after admitted native I/O settles; it cannot guarantee persistence of an external side effect whose result died with the runtime.

Credential bytes, including refresh tokens, may pass through this trusted local bridge. They must never be forwarded to terminal RPC, model requests, history, diagnostics or logs. OAuth policy and file schema remain in the private package. Startup follows current loader row identities: Rutis may replace an offline JS row when it discovers its declared injections, so a disposed provisional fiber is not itself a startup failure. A row that remains disposed consumes the remaining startup deadline before reporting its terminal state; an explicit failed state still fails immediately.

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
