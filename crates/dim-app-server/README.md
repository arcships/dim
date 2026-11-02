# App Server

The Rust CLI uses this server over a private Unix socket (NDJSON) or Windows named pipe. The server owns Agent v4 execution, approvals, model/provider selection and native storage. Product and configured profiles require protected local transports for execution and management; their WebSocket endpoint supports only `hello` and `ping`. The explicit mock profile permits WebSocket execution for protocol testing.

The local web entry for the [Web workspace](../../docs/design/web-workspace.md) is opened at runtime by `web.open` (a holder) or `web.toggle` (the daemon-owned switch); `dimcode web --off` calls `web.disable`. It binds loopback only, serves the frontend from the directory given with `--web-assets DIR` (the separate `dimcode-web` artifact, which the CLI finds; without it, `<product home>/web`; the directory is looked at on each open; a directory whose `dimcode-web.json` names another protocol version is refused with `web_assets_incompatible`) or, in a build with the `embedded-web` feature, the one `build.rs` built from `web/` and embedded, and admits browsers that exchanged the bootstrap token from the returned URL for a cookie. Its connections speak protocol v3 and get Session methods (including execution Session methods) plus the read-only model discovery methods in [Web workspace §2.5](../../docs/design/web-workspace.md#25-protocol-on-the-entry); other management methods and `daemon.shutdown` are `unknown_method`. The entry stays open while the switch is on or any holder is connected; when both are gone, every token is revoked and web connections detach.

The UI registry ([Tui design §3.6](../../docs/design/plugins/Tui.md#36-ui-registry-and-ui-leaves)) is the daemon's record of the interfaces viewing its Sessions, any number per Session. `UiHostPlugin` is a process `Capability` providing `UiHost`. A terminal interface is a UI leaf: the interactive `dimcode` process is a rutis kernel waiting on a private address, and `ui.open` makes the daemon's `UiLeaf` dial it, greet from a context where the daemon's session catalog is isolated, check the leaf's id and mount the session. The leaf runs its own `Tui`. The daemon starts no process and runs no UI control protocol; a leaf lives as long as its session. The leaf's `Tui` keeps the ordinary protocol v3 business connection and registers it with `ui.attach {sessionId, kind, uiId, token}`, the token coming from `ui.open`'s reply.

`ui.open {sessionId, kind, peer, address, replace?, expectedGeneration?}` opens a session with the leaf at `address` (its id `peer` becomes the `uiId`) and returns `{uiId, generation, token}` once the session is mounted; a link or handshake failure is `ui_leaf_failed` and registers nothing. A leaf that does not attach within 20 s of the reply is closed. `ui.close` sends the advisory `ui.shutdown` and disposes the leaf, closing its session (the leaf restores its terminal and exits). `ui.status` is a read for management clients: a leaf is `starting` until its `Tui` attaches, then `online` (or `replacing` while it closes the one it replaces); a leaf whose session ends is removed. There is no heartbeat. `ui.replace` replaces a connection registration; `ui.open {replace}` claims the observed generation before the dial, closes the old interface only once the new leaf attaches, and leaves it untouched when the start fails or the new leaf ends or does not attach in time. Disposing `UiHostPlugin` waits for every leaf disposal. The methods are local-only and advertised in `hello`, with `ui.leaf.2`.

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
Workspace `.mcp.json` files remain independent sources. A missing managed provider does not disable
these public capabilities. Protocol v3 advertises their management methods on local connections.
Configured model changes validate against the supplied catalog and adopt that model's generation
settings; model preferences requiring capability declarations are rejected rather than guessed.

### Session submission errors

For protocol v3 `session.submit`, `error.data.appError` values `invalid_params`,
`session_invalid_input`, `session_closed`, `session_build_failed`,
`service_reloading`, `unknown_method` and `input_blocked` guarantee that this
request did not commit an input. They identify parameter/admission, Session
acquisition or pre-commit input rejection; the server must not return this set
after accepting the input. `input_blocked` is a submit check's block (Hooks
A.6.2): its message is the check's own, nothing was written, and a retry runs
the checks again. This guarantee is specific to `session.submit`, not a retry
policy for other methods or an assertion that a rejected request will succeed
unchanged.

After commit, failure to confirm publication remains uncertain even on a live
connection: `submission_unknown`, an opaque `-32603` publication/storage error,
and every error outside the guaranteed set do not prove rejection. Transport
loss is uncertain too. Clients retain the Session handle and must not blindly
resubmit. The native CLI exits with its uncertain-outcome guidance; this does
not add automatic retry or recovery. See the [CLI submission contract](../../docs/design/cli-attachment-input.md#submission-rejection-and-cancellation).

### Business settings

SettingsService reads and updates generic `BusinessDocument` values with per-source
CAS. Business owners define and validate their payloads: MCP types live in
`mcp/config.rs`, and the Skills owner supplies the existing `SkillSettingsHost`.
Each owner registers its sources in its `apply` (`SettingsService::register`, or
`register` on the named `settings` service); a source is served only while its
owner is loaded, and its stored document stays when the owner unloads.
Settings owns source persistence, not approvals or business management methods.
There is no `configuration.*` RPC namespace.

The native store keeps the v13 settings data contract unchanged in the current
format: `settings_documents` rows keyed `user/mcp`, `user/skills`, `user/mcp-projects`, `user/mcp-plugins` and `user/permission`,
plain payload JSON (not `{kind, document}`), source-local versions and opaque
`native:<source>:<version>` revision tokens. Import provenance is preserved on
edits. Successful writes notify consumers after persistence; failed persistence
does not publish unsaved intent. Business edits do not reload plugins. Structural
loader configuration and package installation remain separate work. See
[ADR 004](../../docs/adr/004-loader-plugin-management.md).

### Provider management

Local protocol-v3 clients read `provider.management {providerId?}` for one safe
editing snapshot containing the record version, connection fields, full model
descriptors, available actions and saved preferences. Credentials, secret
references, header values and proxy authentication are never returned.
`provider.list`, `provider.catalog` and `provider.selection` retain their read shapes;
`provider.list` additionally reports `remoteModelList` per provider, whether
`provider.refresh` can list that provider's models.

`provider.create` takes a fresh canonical UUID; `provider.update` applies only the
supplied fields with record CAS. `provider.probe` tests a complete new connection
with that UUID or an existing connection patch, without writes. Disconnect clears
the credential and sets `enabled:false` in one update. `provider.save` is absent.
User models survive refresh separately from defaults; descriptor resets and New API
endpoint overrides have independent effects. Clients pass saved preferences with
their exact model target when adopting it; the server never applies them implicitly.
`provider.import_legacy` imports one legacy SQLite source explicitly and read-only;
each imported provider item adds `omitted`, the source field names the frozen
mapping dropped, and imported selections carry no such field. The field is
additive and protocol v3 is unchanged.
The [Provider contract](../../docs/design/appserver/provider-management.md) defines
the methods, typed errors, account ownership and accepted old-v19 rewrite loss. v18 binaries reject the v19 database.

### Skills catalog

On protected local protocol-v3 connections, `skills.catalog({cwd?})` returns the catalog's
`version`, its `entries` and its `issues`. An entry is `{id, source, name, description,
filePath, allowedTools, enabled, shadowedBy}`: `source` is `user`, `project` or
`plugin:<plugin id>`, the root the Skill was found in; `shadowedBy` names the Skill whose
same name takes precedence. Clients group entries by `source` and never parse `id`.
`skills.set_enabled({cwd?, id, enabled})` writes the user's disabled set and returns the new
`version`; `skills.refresh({cwd?})` rescans the user and project roots and returns the
`version` and the `issues` of the rescan. The three persist in SQLite under native storage;
legacy `skills.json` is only an import source.

### MCP configuration management

On protected local protocol-v3 connections, `mcp.catalog({cwd?})` returns the merged catalog,
`userRevision` and optional `projectRevision`, without commands, URLs, credentials or environment
values. Edits target `source: "user" | "project"`, an `id`, and that source's `expectedVersion`.
Project edits require an authorized workspace `cwd`; `mcp.save` and `mcp.remove` write `.mcp.json`,
not the user source.

- `mcp.configuration`, `mcp.save` and `mcp.update` read, create or replace and patch one entry;
  the [MCP configuration management](../../docs/design/appserver/mcp-management.md) contract
  defines the entry read with its source revision, the declared configuration they read and
  write, the patch semantics and the values they redact.
- `mcp.inspect({source,id,cwd?})` returns `{enabled,configuration,connections}`. Configuration
  is the effective entry a Session runs, with a project or plugin entry's stdio `cwd` resolved.
  Each connection contains `state: {status,expired?}` and
  `tools: [{name,description,inputSchema}] | null`: its last complete successful discovery,
  or null before discovery. Status is `disconnected`, `connecting`, `connected`, `failed`,
  `closed` or `auth_required` (with `expired`). These are existing connection generations,
  not a host-global connection state. Inspection does not connect or contact the server.
- `mcp.test` probes one configuration without attaching a Session; the
  [MCP configuration management](../../docs/design/appserver/mcp-management.md#45-probe-mcptest)
  contract defines its outcome, latency, tools and auth report.
- `mcp.remove` removes the entry from the selected source; it can reveal a lower-precedence entry.
- `mcp.set_enabled` additionally takes `enabled`. A user entry's `enabled` field changes and the
  rest of the entry is preserved. For a project entry, enabling checks `expectedVersion` against
  `projectRevision` and the entry's presence; the choice is stored only in the user's local record
  (below). Disabling a project entry always succeeds against that record, even if `.mcp.json` is
  unreadable or no longer defines the entry, and `mcp.remove` withdraws it before editing the file.
- `mcp.disconnect` takes `source`, `id` and optional `cwd`, without a revision; it does not edit
  persisted intent.

Edits are source-local. No atomic user/project scope migration is offered; the Web editor fixes
the selected entry's source rather than copying secrets into another scope. OAuth sign-in,
confidential-client input and attempt cancellation follow the [OAuth contract](../../docs/design/appserver/mcp-oauth.md#9-interfaces).

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

Plugins add entries through PluginManager, which registers each plugin's MCP document with Mcp
under the plugin id ([PluginManager §3.7](../../docs/design/plugins/PluginManager.md#37-content-registration)).
Mcp keeps them in `mcp-plugins` and lists them with `source: "plugin"` as `<plugin id>:<server>`;
a stdio entry runs in the plugin directory, or in the directory inside it that its `cwd` names;
a `cwd` that is absolute, uses `..` or leaves the plugin through a symbolic link refuses the document.
They run while the plugin is on and change only through the plugin: `mcp.save`, `mcp.remove`
and `mcp.set_enabled` refuse them, while `mcp.disconnect` and sign-in work as for other entries.

Successful configuration mutations return `{}` after persistence, not after connection/tool
adoption; disabling or removing an entry also waits until its running generations have closed in
every Session. The catalog also returns `userApplication`, `pluginApplication` and optional `projectApplication`, each with
`appliedRevision`, `error` and `cleanupError`. Application describes source eligibility
and instance creation failures; it does not confirm a network connection or tool adoption.
An application error leaves persisted intent intact. Cleanup errors remain visible for that
source during this publisher's lifetime, including after later successful application.
Read `session.mcp` for runtime state. A conflict requires rereading the source;
interruption may follow an already committed write, so never retry
blindly. Use these methods rather than editing imported user files. `skills.set_enabled` likewise
persists in SQLite; legacy `skills.json` is only an import source. Private server configuration
stays inside its owning service and is excluded from catalog replies, notifications and diagnostics.

A stdio server's stderr is discarded: arbitrary server output may contain secrets, so it never
reaches tracing or the daemon log.

### MCP tool declarations

A server's tools are its author's. Mcp registers each as it was discovered: the
declaration carries the server's `description` and `inputSchema` unchanged, and a
call's `content` reaches the model as the server returned it. The server's author
marks the declaration's protocol and policy parts ([model-facing prompts §4](../../docs/design/model-prompts.md#4-protocol-and-policy));
Dim carries them and owns none of their words.

Two refusals are Dim's, and each is mixed, marked sentence by sentence
([model-facing prompts §4](../../docs/design/model-prompts.md#4-protocol-and-policy)):
in "The MCP server \"<id>\" requires the user to sign in. Ask the user to sign in
to it with /mcp." and the insufficient-scope form's "needs additional permissions
for this tool …", the first sentence is protocol — it names the server and the
condition, and the scopes in the second form. The closing "Ask the user to sign
in to it with /mcp." ("again" in the second form) is policy: it is guidance, and
the runtime does not depend on its words. A server's tools write no marker, so
they contribute no guide entry.

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

Native MCP settings use lazy source-local import and durable source CAS. Their
v13 document bytes remain valid in the current store format. Unsupported databases, including
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
`standalone_template` with their `Builtins` and the catalog their loader uses
(`dim_app_server::catalog()` holds the App Server's names), and include both rows in the
immutable assembly. The publication plugin owns its coordinator worker: it starts after
apply and is settled, with its cleanup result, when the plugin unloads. Pass an
explicit `McpOwner::new(ctx, loader)` when acquiring connections. Agent hosts
instead use `mcp::install` and the declared Session loader. Its typed port is a
dependency of `McpPlugin`; connection leases never become a second lifecycle engine. No second
loader, editable-layer target or SQLite dependency is required by this port.

### Subagent model policy

With local execution, `collaboration.model_policy({})` returns `{revision, policy}` and
`collaboration.set_model_policy({expectedRevision, policy})` replaces it and returns the new
`revision` (`collaboration_model_policy_conflict` for a stale revision, invalid params for an
unknown field or mode, an empty ID or a model with another mode). `policy` is
`{"mode":"agent"}` (default), `{"mode":"inherit"}` or
`{"mode":"fixed","model":{"providerId","modelId"}}`, stored as the user source `collaboration`.
Collaboration reads it once for each new member: `inherit` copies the requesting branch's adopted
model selection (captured when the request is consumed) and `fixed` resolves its model as a
profile model would; both override the requested profile's model. A failed read or resolution
refuses the request with `member_create_failed`. See
[subagent model policy](../../docs/design/subagent-model-policy.md).

### Execution shell preference

`host.exec_shell.get({})` returns `{revision, executablePath, shells, configError}`:
the optional explicit selection, detected `{executablePath,name}` choices and a parse
error if the current version-1 file is malformed. `host.exec_shell.update` takes
`{expectedRevision, executablePath?}` and returns the new snapshot. Omitting the path
selects automatic detection; null is invalid. The HostPlatform owner validates the
selection and atomically updates its configured `exec-shell.json` through FileStorage.
Stale writes return `exec_shell_conflict`; other host/storage failures are
`exec_shell_failed`. New commands reread the preference; running shells are unchanged.

### Sandbox backend availability

`host.execution.backend({})` returns `{available: true}` or `{available: false, reason}` for the
host that would start the next process. macOS uses the seatbelt backend and answers
`{available: true}`; Linux answers `{available: true}` when preparation would find bubblewrap on
the captured `PATH`, and `{available: false, reason}` otherwise; Windows always answers
`{available: false, reason}`. The App Server's `ExecutionBackend` part reads `HostPlatform`, which
resolves the same bubblewrap executable preparation uses. The reason is the text the next process
start would use. The Execution row in `/settings` and the Web Permissions page read this method and
display the reason; neither infers the platform from its own environment. The switches stay on
`settings.read`/`settings.update`, and a sandboxed process on a host without a backend still fails
as `isolation_unavailable`.

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

`--home` and `--database` are mutually exclusive. The SQLite store uses native format v29. It holds user settings in the unchanged v13 settings data contract, normalized managed-provider records, Timeline facts with optional recovery snapshots kept separate, Session file rows (v15), Session title metadata (v16), request messages stored as content-addressed node lists (v17), extension event rows (v18), Session configurations and Agent profiles that may omit their mount list (v19), the branch end events (v20) and the tool-effect and digest fields, which need no schema change either (v21), tool schemas that may omit `type` (v22), the Session catalog projection (v23) and a configuration's System source, which needs no schema change either (v24). Format v25 removes the unused timeline end-sequence index, format v26 stores a model limit only when a declaration supplies it, which needs no schema change either, and format v27 adds an Agent owner and a member Session leader, two nullable columns with an index each. Format v28 adds Session file grants, and format v29 adds command output ownership.

Startup opens a store with the exact v29 schema and a single marker of 29. A store with the exact v28, v27, v26, v25, v24, v23, v22, v21, v20, v19, v18, v17, v16, v15 or v13 schema and its own single marker is upgraded in place to v29 inside the startup admission transaction, before any writable PRAGMA. The transaction replaces the marker table once, then applies each step from that version on:

- v13 → v15 adds `session_files`, `session_file_owners` and the `session_file_owners_by_file` index.
- v15 → v16 adds `session_metadata`.
- v16 → v17, v17 → v18, v18 → v19, v19 → v20, v20 → v21 and v21 → v22 change only the marker.
- v22 → v23 adds the Session catalog table and indexes and backfills it once from committed main history.
- v23 → v24 changes only the marker.
- v24 → v25 removes `timeline_events_by_end` and replaces the marker; facts and other indexes are unchanged.
- v25 → v26 rewrites each stored Provider model limit, dropping the ones no source declares, and replaces the marker; facts and all other rows are unchanged.
- v26 → v27 alters `agents` and `agent_sessions` to add the owner and leader columns, then creates their indexes and replaces the marker; facts and all other rows are unchanged and both columns stay NULL.
- v27 → v28 adds the empty `session_grants` table and replaces the marker; existing rows and delegation ownership remain unchanged.
- v28 → v29 adds the empty `session_output_owners` table and its `path` index and replaces the marker; existing rows and output written before the upgrade remain unowned.

Existing facts, v13 settings bytes and v16 request rows are preserved, and a failure rolls back to the untouched store. The upgrade is one-way: the daemon writes `Upgraded native store <path> from format v<N> to v29; earlier dimcode versions cannot open it.` to stderr once, and earlier daemons refuse the upgraded store. Any other database is rejected and preserved: an older format such as v12, the removed branch-only v14, a mismatched marker, a partly upgraded schema or an unknown one. There is no migration command or startup option. [Native storage format](../../docs/design/appserver/native-storage-format.md) defines the SQL and JSON contract.

### Timeline export

`dim-app-server --export-timeline SESSION_ID --database PATH` writes one Session's whole Timeline to stdout as versioned JSONL and exits; `dimcode export` runs it. It opens the database read-only without the lease, upgrade or schema statements, and reads one SQLite snapshot, so it can read a database a running daemon owns. It reads the current format and the formats this build upgrades from as they are, rejects any other with `unsupported Timeline storage format`, and reads only that Session's Timeline rows. The [Timeline export design](../../docs/design/appserver/timeline-export.md) defines the lines.

Unix endpoint directories must be private (0700), sockets use 0600, and cleanup removes only the inode acquired by this listener. Named pipe ownership and local identity checks remain in the transport. Ordinary client disconnect detaches; it does not delete a session. See the [public architecture](../../docs/architecture/README.md).

### Shutdown completion

For authorized transports, v3 `hello.features` includes `daemon.shutdown.completed`. A successful `daemon.shutdown` response with `force: false` (the default) certifies that Agent work, connections and the Rutis process tree have settled, including storage and plugin disposal. Cleanup failure returns `shutdown_failed` without exposing disposer error details. EOF alone is not success. `force: true` retains its early acknowledgement and does not promise cleanup.

The requesting connection hands only its final-response writer to the existing process coordinator before leaving the plugin graph. The coordinator sends successful completion after `root.shutdown()`; no graph-owned RPC task waits on its own disposal. Shutdown first stops RPC admission and joins admitted shutdown handlers, including force acknowledgement writes. If an admitted force request wins, the coordinator joins connection handoffs and returns `shutdown_failed` to pending graceful callers without waiting for business cleanup. Reply writes are bounded by the transport write deadline. Client disconnect does not cancel accepted cleanup. Clients must check the feature before relying on this completion contract from an older daemon.

### Process logs

The server writes `tracing` output to stderr, filtered by `RUST_LOG` (errors only by default); a daemon started by the CLI writes it to the private `daemon.log` in its native home. The filter is read when the process starts; nothing changes it while the server runs, so a different filter needs a new process. The runtime operations `prompt.load`, `context.prepare`, `model.invoke`, `tool.attach`, `tool.execute`, `shell.run`, `timeline.close` and `session.close` are spans carrying the Session and branch ids and the operation's own ids. Closing one logs its duration as `time.busy` plus `time.idle`, so `RUST_LOG=info` shows operation timing. Span names and fields are log output, not a compatibility contract. Committed facts are read from the Timeline, not from logs ([ADR 005](../../docs/adr/005-observability.md)).

## Temporary MCP bindings

The MCP feature also registers the independent `TemporaryMcp` Session part for
client-supplied servers. It reuses ordinary `McpServer` children and the tool
registry; configured MCP sources and RPC dispatch are unchanged. Its public
methods, volatile configuration, lease and disposal contract are in
[Temporary MCP](../../docs/design/plugins/TemporaryMcp.md).

## Managed provider plugins

The public server runs without a managed provider. API-key and supported native subscription providers need none, and no Node process is started for one. A managed provider is a Rust rutis plugin built outside this repository that provides `Arc<dyn ManagedProvider>` (the [contract](#managed-provider-contract) below). Its own executable installs it, mirroring the public [`dimcode` entry](../dimcode/src/main.rs):

```rust
// `dimcode app-server …`, run by the CLI as its daemon from this same executable.
dim_app_server::cli::process_entry(args, Some(ManagedProviderRow(Arc::new(|builtins, root| {
    dim_agent_runtime::register_part(builtins, &dim_app_server::catalog(), move |names| {
        Ok(Box::new(MyProviderPlugin::new(names, root)?))
    }).expect("managed provider dependencies")
}))))
```

`ManagedProviderRow` registers the plugin's process row given the product root's FileStorage index and returns the row name; `ServerConfig.managed_provider` carries it for embedders. It is installed only with a product home, beside `ManagedProviderBinding`, which hands the provider to `ManagedProviders`. The plugin owns its account schema, authentication, token refresh and catalog normalization; the host does not interpret supplier-specific responses. Files it keeps under the product root go through FileStorage; `auth.json` is plain JSON shared with the native subscription providers, each owning its own root fields.

`process_entry` is the process entry: on Unix it closes every descriptor above standard error that the process inherited, then calls `run_with`, so a process `main` calls it before it opens anything. Linux closes the range in one `close_range` call; where that call is unavailable, and on the other Unix systems, the daemon lists `/proc/self/fd` (Linux) or `/dev/fd` and closes every number the listing names; when no listing can be read it closes 3 up to the process's descriptor limit, which is the soft limit, else the hard limit, else `sysconf(_SC_OPEN_MAX)`, else 65,536, so no inherited descriptor stays open. `run_with` and `run` do not touch the descriptors the process has open and are the entries for an embedder that keeps its own.

The Dim account (`dimcode-api-oauth`) is such a plugin, in [`crates/dim-oauth`](../dim-oauth/README.md); `dimcode` built with the account's client IDs is the account build. See the [development contract](../../docs/architecture/development.md#managed-provider-boundary) and [release packaging](../../docs/release/README.md#local-packaging).

### Trusted plugin storage

The native provider is installed once whenever the host supplies an authorized
product root. Public process, Session and branch rows use the same provider; a
Session-only configuration needs no process row. Without
that root, no provider or working-directory fallback is supplied. See the
[public binding](../../docs/design/process-typescript.md#52-public-pluginfiles-binding).
The embedding host must provision each consumer's parent directories before
enabling it: `withLock` returns `io` for a missing parent and does not create it.

A plugin declares `inject: ['pluginFiles']` and opens a fresh scope during each `apply`. Register `scope.close()` with `ctx.effect` immediately after opening, then register business cleanup. The host chooses the authorized product root; the plugin supplies relative file paths only. This is a trusted-plugin API, not a sandbox for untrusted Node code.

Import `PluginFiles`, `FileScope`, `FileLease` and `FileResult` with
`import type { PluginFiles } from 'dimcode/plugin'`. The
[SDK declarations](../../npm/plugin.d.ts) define the method types; the
[plugin author's guide](../../docs/typescript-plugin-authoring.md) shows their use.
`quiesce()` synchronously returns null; `close()` waits for admitted work and
resolves to null. Awaiting either remains valid.

`withLock` holds the native FileStorage lease through the complete callback, including asynchronous work. Reads validate the storage record below and return its UTF-8 text; a missing file returns null. Replacement is atomic and flushed, with Unix mode 0600 and native Windows DACL handling. `backup` creates a flushed, exclusive sibling copy with an alphanumeric/hyphen suffix. A failed partial backup is retained; an existing backup is never overwritten. Storage errors expose only fixed codes, never file contents or exception text. An error after replacement does not imply rollback or authorize replaying remote side effects.

Use a fresh suffix for each backup attempt, such as `before-reset-<UUID>`. An `exists` result does not prove the earlier backup completed. After a failed backup, stop the destructive operation, address the storage failure, and create a successful backup with a new suffix before proceeding. Retained partial backups require explicit operator inspection and cleanup; the lease has no deletion API. Host worker failures return `internal`, distinct from invalid arguments or paths. Callback exceptions retain Rutis rejection semantics and still revoke the lease.

Quiesce rejects new transactions and cancels acquisition, but permits an acquired transaction to persist before close joins it. The optional Promise cancels acquisition when it settles; Node must settle it on ordinary completion too so a pending exported Promise cannot hold runtime drain open. Rutis 0.10 does not encode Node AbortSignal arguments; do not pass one. Escaped file callbacks are revoked before releasing the lease. Abandoning the caller does not abandon issued I/O. Runtime loss revokes the lease after admitted native I/O settles; it cannot guarantee persistence of an external side effect whose result died with the runtime.

Bytes a plugin stores pass through this trusted local bridge. They must never be forwarded to terminal RPC, model requests, history, diagnostics or logs. Startup follows current loader row identities: Rutis may replace an offline JS row when it discovers its declared injections, so a disposed provisional fiber is not itself a startup failure. A row that remains disposed consumes the remaining startup deadline before reporting its terminal state; an explicit failed state still fails immediately.

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

## Managed provider contract

`ManagedProvider` ([`src/managed_provider.rs`](src/managed_provider.rs)) is the typed service a managed provider plugin publishes. `ManagedProviderBinding` hands it to the process-owned `ManagedProviders`, which the account RPC, the provider service and `webSearch` (exported via `#[rutis::service]` for the `Web` plugin) use. `ManagedProviders` is assembled on every server; `ManagedProviderMethods`, also a process row, registers the `account` namespace with `managementMethods` and is assembled with a product home. Without a binding the process lists no managed providers and `webSearch` fails with `unavailable`.

| Method | Input | Value |
| --- | --- | --- |
| `providers` | — | `ProviderDescriptor[]` (`providerId`, `displayName`, `loginMethods`, `defaultDirectory?`) |
| `status`, `recover`, `logout` | provider ID | `AccountSnapshot` |
| `usage` | provider ID | `Usage` |
| `referral` | provider ID | `Referral` |
| `reset_batches` | provider ID | `ResetBatches` |
| `apply_reset` | `{providerId,subscriptionId,resetType,idempotencyKey}` | `ResetApplied` |
| `web_search` | `{query,count}` | `{results: Array<{title,url,snippet,publishedAt?}>}` |
| `begin_browser` | `{providerId,directory,email?,redirectUri}` | `{attemptId,authorizationUrl,expiresAt}` |
| `begin_device` | `{providerId,directory,email?}` | `{attemptId,verificationUri,verificationUriComplete?,userCode,expiresAt}` |
| `complete_browser` | `{providerId,attemptId,callbackUrl}` | `AccountSnapshot` |
| `attempt_status` | `{providerId,attemptId}` | `pending`, `completed` with its account, `failed`, `cancelled` or `expired` |
| `cancel_attempt` | `{providerId,attemptId}` | — |
| `catalog` | provider ID | `{account, models: ManagedModel[]}` |
| `credential` | provider ID, model ID | `CredentialLease` |
| `recover_rejected`, `finish_rejected` | `CredentialLease` | `CredentialLease`, — |
| `quiesce` | — | — |

```ts
type AccountSnapshot = {
  credentialEpoch: string
  generation: number
  account?: { issuer: string; subject: string; email?: string; name?: string; grantedScopes: string[] }
}
// The plan and consumption of the signed-in account, as the provider projects
// its account service's usage report. Parts the service does not report are
// absent; an unlimited bucket reports its totals as 0 with `unlimited: true`.
type Usage = {
  subscriptionUrl?: string
  pricingUrl?: string
  subscriptionId?: number
  resets?: ResetSummary
  plan?: { name?: string; status?: string; priceAmount?: number; priceCurrency?: string; priceInterval?: string; expiresAt?: string; cancelAtPeriodEnd?: boolean; currentTermStart?: string; lastTermStatus?: string; lastTermStart?: string; lastTermEnd?: string }
  credits?: { usedUnits?: number; totalUnits?: number; remainingUnits?: number; unlimited?: boolean; resetsAt?: string }
  windows: Array<{ durationHours: number; tokenUsed: number; tokenCap: number; startedAt?: string; expiresAt?: string }>
  features: Array<{ key: string; used?: number; allowance?: number; remaining?: number; unit?: string; unlimited?: boolean; periodEnd?: string }>
}
type Referral = {
  enabled: boolean; eligible: boolean; affCode?: string; inviteUrl?: string
  invitedCount: number; paidCount: number; rewardGrantedUnits?: number
  inviterRebateBp: number; inviteeRebateBp: number
}
type ResetType = "window" | "monthly_full"
type ResetKind = { availableCount: number; nearestExpiresAt?: string }
type ResetSummary = { window: ResetKind; monthlyFull: ResetKind }
type ResetBatch = {
  grantId: number; remaining: number; expiresAt: string
  scopeType: "account" | "subscription"; subscriptionId?: number
  sourceType: "admin_compensation" | "campaign" | "incident_compensation"
}
type ResetBatches = { window: ResetBatch[]; monthlyFull: ResetBatch[] }
type ResetApplied = {
  credits: NonNullable<Usage["credits"]>; windows: Usage["windows"]
  resets: ResetSummary; replayed: boolean; appliedAt: string
}
```

Account and login operations are exposed as `account.providers`, `account.status`, `account.recover`, `account.logout`, `account.usage`, `account.login.begin`, `account.login.browser.begin`, `account.login.browser.complete`, `account.login.status` and `account.login.cancel`; a failed attempt reports only `Login failed`. Existing `account.codex.*` and `account.grok.*` retain their native contracts. Attempt secrets are memory-only and cleared on completion, cancellation or expiry; a terminal attempt stays queryable until its deadline.

`account.referral` and `account.reset_batches` take `{providerId}`. `account.apply_reset` takes the typed `apply_reset` input above: the client confirms an action and generates one nonblank idempotency key (at most 128 bytes), retaining it for retries after an uncertain reply. The subscription ID is a positive safe integer from `account.usage`; the account service authorizes it. Batch selection and consumption order belong to the account service, not the client. These three methods are optional provider operations; the trait defaults reject them with `account_invalid_input` for providers without this account functionality. The Dim provider implements all three. The issuer, tokens, request headers and refresh retry stay in the provider; none is a client parameter. Account-service error bodies are never returned to clients.

`defaultDirectory` is non-secret login metadata for a signed-out client, not an OAuth client ID. The Dim provider selects it from the host entry (`DIMCODE_NPM_PACKAGE`, otherwise the executable stem): `dimagent` selects `https://dimagent.com`, other entries select `https://dimagent.cn`. An enterprise email still goes through the provider's directory discovery. Subscription, pricing and referral URLs derive from the authenticated grant's issuer.

`ManagedModel` is `{entry: ModelEntry, protocol, unavailableReason, zdr}`. `ModelEntry`, capabilities and metadata are defined in [`model_catalog.rs`](src/model_catalog.rs) and `dim-agent-types`. A provider supplies final public model descriptions; `BundledCatalog::fallback` offers the provider service's bundled metadata (refreshed before each release) for what its source omits. An unavailable model keeps a reason and is unselectable.

`CredentialLease` carries the account snapshot, protocol, final protocol base URL (the model adapter adds its endpoint), request headers and bearer. It has no `Debug` or serialization; credential methods are internal to the model resolver and never appear in terminal RPC, history, Timeline, configuration replies or logs. Refresh tokens stay inside the provider. A lease is released when its last holder drops it, including a result whose caller went away. Leases are bound to the provider instance, account and grant revision, expire after 30 minutes and are capped at 1024. `ManagedProviders` stamps each lease with the binding that issued it: recovery and final rejection of a lease whose binding was replaced fail with `service_reloading` before any provider sees them, even when the replacement reports an equal account and reuses its lease IDs. `recover_rejected` performs one controlled recovery: it adopts a newer persisted grant or performs one serialized refresh. `finish_rejected` signs out only if the rejected grant still owns the account. Host recovery is limited to HTTP 401; unrelated failures retain the account.

`ManagedProviders` checks provider output once where it enters the host, and rejects it as `account_invalid_response`: provider descriptors (non-empty unique IDs, a display name, `browser`/`device` login methods; an invalid set also fails the binding), account snapshots (an epoch, a generation within the safe integer range, a non-empty issuer and subject), catalogs (a signed-in account, unique non-empty model IDs, display names, supported protocols, limits from 1 to the safe maximum) and leases (an ID, a bearer, a signed-in account, a supported protocol, an `http`/`https` base URL without user information, query or fragment, and header names and values without CR or LF). A refused lease is dropped, which releases it.

The provider owns every accepted operation: a caller dropping its future never abandons a token rotation or a persisted login, and disposal joins that work. Before draining Agent/RPC work, the host calls the idempotent `quiesce()`; binding unload does the same. Quiesce stops admission and cancels lock waits and login attempts without awaiting them; it does not abandon a refresh that holds the account lock or revoke delivered leases.

Errors are the canonical `ManagedError` kinds; the host maps them to fixed public diagnostics (`account_required`, `account_changed`, `account_invalid_store`, `account_unsupported_schema`, `account_recovery_required`, `account_reset_blocked`, `account_reset_denied`, `account_identity_rejected`, `account_invalid_input`, `login_attempt_unknown`, `login_attempt_closed`, `account_invalid_response`, `service_reloading`, otherwise `account_unavailable`). A reset-apply HTTP 403 maps to `account_reset_denied`, not a login failure. Account corruption and an unavailable provider are never represented as a signed-out account.

## Catalog and storage consistency

The provider host stores normalized catalog caches, user model overrides, disabled model IDs and their revision in the existing `providers` table. Managed records have `kind: managed` and an account snapshot; token data is absent. Static records retain their own schema.

A catalog refresh captures the current SQL record version before its provider request. Its commit uses that version as a compare-and-swap guard, so a late request cannot overwrite a newer catalog. Account generation is checked before and after commit. An account change fails the operation rather than returning successful stale data. Cache lookup and request credential resolution require the same account epoch/generation; a previously stored snapshot can never authorize the next account.

There is no atomic transaction spanning the account file and SQLite. A generation change during an admitted SQL write can leave an older, unusable cache record; the next explicit refresh replaces it. The host does not hold the account lock across provider calls or retry state-changing operations implicitly.

## Verification

Public API-key, provider, native subscription, managed provider and transport flows do not need Node:

```sh
cargo test --locked -p dim-app-server --test provider_flow --test managed_oauth_flow --test managed_provider_flow --test model_catalog_flow --test transport_flow
```

The separately enabled Node flows load public SDK fixtures: plugins through the actual rutis runtime, and MCP servers built on the official MCP SDK. They cover C4 Session tool/method binding, isolation, restore, cancellation and invalid boundaries; Python rows and an unmodified Cordis 4 plugin in their groups; and stdio and legacy SSE MCP discovery, calls, cancellation and cleanup. They do not require a private account package. The Python rows need Python 3.12 or later with the pinned `rutis` package:

```sh
npm ci --prefix runtime --ignore-scripts
python3 -m venv runtime/.venv
runtime/.venv/bin/python -m pip install --require-hashes --no-deps -r runtime/python-requirements.txt
DIM_TEST_NODE="$(command -v node)" \
DIM_TEST_JS_RUNTIME="$PWD/runtime/node_modules/@arcships/rutis-runtime" \
DIM_TEST_RUTIS_SDK="$PWD/runtime/node_modules/@arcships/rutis/src/index.mjs" \
DIM_TEST_PYTHON="$PWD/runtime/.venv/bin/python" \
  cargo test --locked -p dim-app-server --test transport_flow --test mcp_http_flow \
  node_ -- --ignored
```

CI selects these flows by the `node_` prefix, so a `node_*` flow may depend only on Node, Python and the public npm and PyPI packages the repository pins, never on credentials or remote services. `context7_live_through_rust_plugins` calls the hosted Context7 MCP server with `CONTEXT7_API_KEY`, so it is a manual live check that CI does not run:

```sh
cargo test --locked -p dim-app-server --test mcp_http_flow \
  context7_live_through_rust_plugins -- --ignored
```

The [C4 Session TypeScript spike](../../docs/design/session-typescript-spike.md)
has a separate executable probe for per-Session loader rows, named service and
broadcast isolation, cancellation, unload and cross-process call measurements.
It installs no production capability bridge or Session RPC methods.

## Session TypeScript embedding

`ServerConfig.session_typescript` accepts `SessionTypeScriptConfig` with absolute Node executable,
runtime package and anchor paths, an optional Python runtime (`python`: an interpreter with the
`rutis` package and the directory its `py:<module>` rows are imported from), plus ordinary loader
rows. Each row is one plugin, placed in the process. From its `apply` the plugin registers its
Session and branch parts with `partRegistry` as plugin definitions, and every Session or branch
mounts a copy; the row lists in `instanceNames` the names those parts provide in their instances.
A row has no `group`. The runtimes' standard error reaches the process log; their standard output
is discarded.
A Session part registers its tools
and Session method namespaces from its own `apply`, with the `toolRegistry` and `sessionMethods`
services it injects; the row's `RowSchema.provides` lists only the process services it provides by name. See the [binding contract](../../docs/design/session-typescript.md)
and [typed authoring example](../../docs/typescript-plugin-authoring.md#session-tools-and-client-methods).

The `dim-app-server` binary builds this configuration from `--session-rows FILE` with the
existing `--node` and `--js-runtime` flags, and `--python PATH` for `py:` rows. `FILE` is a JSON
array of ordinary loader rows, one per plugin; it is also the anchor, so package names in the
rows resolve from its location and `py:` modules are imported from its directory. Rows are
validated as they are for an embedding host; nothing is installed by default. Optional packages such as
[SessionTitle](../../runtime/plugins/session-title/README.md) and Goal are enabled this way:

```sh
dim-app-server --product-home /absolute/product-home --home /absolute/product-home/rust \
  --node /absolute/node-executable \
  --js-runtime /absolute/runtime/node_modules/@arcships/rutis-runtime \
  --session-rows /absolute/runtime/session-rows.json
```

## Plugins

With `--product-home` or `--home`, PluginManager keeps the host's plugin configuration in
`<home>/plugins.json` and serves the `plugin.*` management methods and the agent's `plugin_*`
tools ([design](../../docs/design/plugins/PluginManager.md)). `--defaults FILE` names the default
configuration file: the default plugins by name and directory, and the locked names
(development uses `runtime/default-plugins.json`). Plugin code runs in the runtimes given by
`--node` and `--js-runtime` (and `--python`); with `--defaults` and no `--session-rows`, packages
resolve from the default configuration file's location. A plugin whose runtime is not given shows
its row's status in `plugin.list`.

Client RPC replies omit Git source userinfo, query and fragment, preserving the
repository path, ref and commit. The complete source stays in the owner's record
so update continues to work. RPC failures preserve their stable application code
and an actionable description, but omit raw Git, manifest, loader and content-error
messages, which can contain credentials. Runtime and content failures in successful
replies use a generic error message. Projection touches only catalog candidate sources,
top-level status errors and direct content-refusal errors, including mutation results;
it does not traverse plugin-owned settings or add an alternate lifecycle.

```sh
dim-app-server --product-home /absolute/home --home /absolute/home/rust \
  --defaults /absolute/runtime/default-plugins.json \
  --node /absolute/node-executable \
  --js-runtime /absolute/runtime/node_modules/@arcships/rutis-runtime
```

Process rows inject `managementMethods` and register their namespaces from their own
`apply`, with native management admission, progress and cancellation; Rpc reads the
management table per call.
Process owners survive Session closure. Optional Pending/Failed rows affect only
their own namespaces, while required host infrastructure and the shared runtime
declaration gate retain the startup deadline. Module top levels must do no business
I/O: a hung declaration load can still block that shared gate. See the
[process contract](../../docs/design/process-typescript.md) and
[authoring example](../../docs/typescript-plugin-authoring.md#process-owners-and-management-methods).

The [public embedding example](examples/session_typescript.rs) starts the real server with offline
mock model/tools, a protected Unix socket and a fresh temporary SQLite home, then calls it
through JSON-RPC. It removes that home after shutdown and never uses the product home:

```sh
npm ci --prefix runtime --ignore-scripts
cargo run --locked -p dim-app-server --example session_typescript -- \
  "$(command -v node)" "$PWD/runtime/node_modules/@arcships/rutis-runtime"
```

After process reconciliation, existing startup readiness waits for the configured runtime's
`RuntimeRows` catalog publication. The SDK refreshes its legal offline placeholder with the
online declaration before publishing that catalog; the online `provides` must match the host
snapshot. Each Session row then remains Loading until all its declared tool/method services are
projected and its C1 methods are bound. Tool declarations are still read only at the first
ToolRegistry read. Ordinary create/restore flows exercise the immediate first request, including
a tool-only module without a test-only required dependency.

Rows remain optional: a row genuinely Pending on a missing dependency does not delay native
Session use; its methods may report `service_reloading` and its tools stay omitted until reload
if absent at the first registry read. Successful submit/idle replies certify native processing,
not execution of an omitted tool; the example prints durable history to show the actual result.
A Session row reads its Session through `sessionIdentity` (`{sessionId, restored}`),
`sessionConfig` (the creation configuration) and `sessionLifecycle` (`activated`, `closing`,
`hold`). It registers ``ctx.on(`session/${sessionId}`, ...)`` in its `apply` with that Session id. Its records are
`SessionBroadcast`: `{kind: 'event', branchId, event}`, with the existing native AgentEvent
serialization, or a `notice`, `fence` or `closed` record that an observer skips. Records of one
Session start in emit order, and a slow listener does not delay native client notifications
or fences. Closing and row effects own cancellation/cleanup; reload and restore never
replay completed observations. Apply only registers resources; background work waits for
`activated()`, and critical ordered branch actions remain outside this observation contract.
