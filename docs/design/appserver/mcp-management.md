# MCP configuration management

Status: Draft.

Scope: process.
Issues: [#453](https://github.com/arcships/dim/issues/453), [#738](https://github.com/arcships/dim/issues/738), [#452](https://github.com/arcships/dim/issues/452), [#953](https://github.com/arcships/dim/issues/953), [#1146](https://github.com/arcships/dim/issues/1146).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

## 1. Scope

This document owns the configuration surface of the `mcp` management namespace: how a client
reads one entry in order to edit it, writes the result, and exercises a configuration before
saving one. It owns those methods' requests and replies, the values they redact, and how a source
revision orders an edit. It also owns the user↔project migration (`mcp.move`), the user-local
secret overlay that migration uses, and the import of a standard `mcpServers` document into one
source (`mcp.import.preview`, `mcp.import.apply`). It also owns the opt-in discovery of the known
MCP stores other local clients keep, and the `from` import input that reads one of them inside the
daemon (`mcp.import.sources`, D26).

It does not own:

| Surface | Owner |
| --- | --- |
| `mcp.auth.start`/`complete`/`cancel`/`logout`, the catalog's `auth` field, `session.mcp` | [MCP OAuth](mcp-oauth.md#9-interfaces) |
| `mcp.temporary.capabilities` | [TemporaryMcp](../plugins/TemporaryMcp.md) |
| Entry admission, dispatch, withdrawn-namespace answers, interruption settlement | [Management methods](management-methods.md) |
| Revisions, compare-and-set, project authorization | [Settings service](settings-service.md#33-persistence-and-revisions) |
| Which plugin registers which entry, and how | [PluginManager](../plugins/PluginManager.md#37-content-registration) |

An MCP Apps host contract — the document a tool declares, a view's tool call and its admission,
and what a client renders — is not part of this surface; [MCP Apps](mcp-apps.md) owns it
([#451](https://github.com/arcships/dim/issues/451)). The probe lists tools for a diagnostic and
exposes no resource, invocation or view.

Two terms are used below:

- The **declared** configuration is what an entry's source holds for it: `mcp.json` for a user
  entry, `.mcp.json` for a project entry, the plugin's registered document for a plugin entry.
  A project entry moved from user scope also has a **user-local secret overlay** (`mcp-secrets`);
  its declared configuration is the project file's public skeleton merged with that overlay. The
  overlay is not a second entry and is never returned to a client.
- The **effective** configuration is the declared one with the daemon's own resolution applied:
  a project entry's stdio `cwd` is resolved against the workspace and its enablement is taken
  from the user's record (ADR 003 D3), and a plugin entry's stdio `cwd` is resolved against the
  plugin directory. A stdio entry's child `PATH` is the entry's own `PATH` when it sets one,
  otherwise the daemon's, with the bin directories of the runtimes RuntimeManager installed
  appended after it (RuntimeManager §5); the command text stays what the user wrote.

## 2. Current behavior

`mcp.catalog` lists the merged entries with `id`, `source`, `enabled`, `transport` and the
optional `auth`, plus both source revisions and each source's application record. `mcp.save`
creates or replaces one source's entry, `mcp.update` applies a typed patch, `mcp.remove` removes
the entry and `mcp.set_enabled` writes enablement. `mcp.configuration` returns a redacted
projection of one entry, `mcp.inspect` returns that projection with the entry's connection
generations, and `mcp.test` probes a saved entry or a draft configuration, answering its tools or
an RPC error.

Two properties of that surface prevent a client from editing an entry:

1. **The read carries no revision.** `mcp.save`, `mcp.update`, `mcp.remove` and `mcp.set_enabled`
   all require the target source's revision as `expectedVersion`, and the read does not return
   it. A client pairs the read with a separate `mcp.catalog` call, so the entry it renders and
   the token it writes with come from two snapshots taken at different times.
2. **The read and the patch are not the same configuration.** Both go through the effective
   entry, so a value the read shows is not a value the client may write back. For a project stdio
   entry that declares no `cwd`, the read shows the resolved workspace path, and a patch that
   leaves `cwd` alone persists that absolute path into `.mcp.json`. For a project entry the patch
   also persists the user record's enablement as the file's `enabled` field, which the same
   design ignores when it runs the entry.

Nothing moves an entry between sources. The client-side workaround is `mcp.save` on the
destination and `mcp.remove` on the source, built from the redacted read: it loses every withheld
value, or writes a token into `.mcp.json`, does not check the destination id or revision, and
does not handle enablement. `mcp.move` is the owner operation; D10–D16 and §4.6 state its
contract.

The probe hides its own outcome. A client that asks whether a server works gets its tools or an
RPC error, so a timeout, a failed request and a refused schema arrive as error codes with no
latency, and a server that needs sign-in answers `mcp_unavailable` like any other failure.

## 3. Decisions

### D1. One edit read, with the source revision and the enablement

`mcp.configuration` is the read an edit is based on: the reply carries the source revision, the
enablement the entry runs with, and the configuration projection, all taken from one source
snapshot. A client never combines a projection from one read with a revision from another; this
is the rule [Provider management](provider-management.md#31-read-models) states for the sole
edit-view read.

Creation has no entry to read yet, so `mcp.save` takes its token from the source revisions
`mcp.catalog` returns: `userRevision` for a user entry, `projectRevision` for a project one. That
is those two fields' only remaining use; a client editing an existing entry takes the token from
the entry read.

`mcp.catalog` stays the list read: no entry configuration and no per-entry revision, because a
list of entries and a token for writing one answer different questions. `mcp.inspect` stays the
run view: it reports the effective configuration with the entry's connection generations, and a
client must not write its values back.

### D2. The management surface speaks the declared configuration

Every configuration a client reads for editing, patches, or submits in a draft is the declared
entry. The daemon applies its own resolution when it runs an entry or inspects a running one.
Echoing a read value back therefore changes nothing, and no host path enters a source through an
edit: a project entry's `cwd` is returned as the file declares it (`null` when the file declares
none), and a patch that does not mention `cwd` leaves the file's `cwd` as it was.

The one field whose value is not the source's is `enabled`, and it is returned beside the
configuration, never inside it: a user or plugin entry is enabled unless its own `enabled` field
is `false`, and a project entry is enabled exactly when the user's local record names it, since
project files cannot set it (ADR 003 D3). It is written only by `mcp.set_enabled`. Enablement
and credentials therefore change without a configuration write (`mcp.set_enabled`, `mcp.auth.*`);
a client never rewrites an entry to change either.

The publication follows the same rule: a project entry's generation is retired as soon as the
user's record stops naming its `(workspace, id)`, whatever the project file or the withheld
records read as. A disable is readable whenever it lands, so it never depends on a source it
cannot read; the source's own error is reported unchanged.

### D3. Redaction covers every value a client does not need

The projection returns the executable, the endpoint's origin, names, counts and presence flags.
Argument values, environment and header values, the bearer token, and the URL's userinfo, path,
query and fragment are withheld: a stdio command line and a request header commonly carry a
token, and a URL can carry one in its userinfo or query. This design adds no field that exposes a
stored value; an edit preserves every withheld field by omitting it (D4).

### D4. An edit is a patch; an omitted field keeps its stored value

`mcp.update` takes the source revision and a typed `patch`. Omission preserves, an explicit value
replaces, and a map changes only the keys it names. This is what lets a client edit one field
without holding another field's secret. `mcp.save` remains the complete-configuration write for
creating an entry, or replacing one whose withheld values the user re-enters; a client does not
use it to round-trip an existing entry, because a complete configuration would have to carry the
secrets the read withholds.

### D5. One revision per source, and a conflict is a reread

The revision is an opaque value the owner computes and the client only carries. For a user source
it is Settings' revision for that document
([Settings service](settings-service.md#33-persistence-and-revisions)); for a project source it is
a digest over that same revision and the workspace's withheld records, as `(id, generation)` pairs
(D23). A project entry is served from two documents, so one token names both: a record another
writer renewed makes it stale exactly as a source edit does. A generation is random and renewed on
every record write (D15), so the digest identifies a record version without deriving from its
values, and another workspace's records are not part of it.

`mcp.catalog` reports the value as `userRevision` or `projectRevision`, and the same one every
write takes as `expectedVersion`. All entries of one source share it, so a write to another entry
of that source makes the token stale and the next write answers `mcp_settings_conflict`. A client
then re-reads the entry it is editing and shows the conflict; it does not retry the same write. A
remove whose `expectedVersion` is stale when it arrives writes nothing. A source change that
lands while the remove runs, after that check, can still leave the entry withdrawn: it is then
off, which fails closed, and the reply is the conflict. A remove needs a readable source; a
disable still withdraws the record without one.

A plugin entry has no Settings source and no revision. The read returns no `revision` for it,
which is the client's signal that the entry is read-only: `mcp.save`, `mcp.update`, `mcp.remove`
and `mcp.set_enabled` refuse a plugin target. Sign-in, `mcp.test`, `mcp.inspect` and
`mcp.disconnect` still serve one, as they do today.

### D6. Writes keep their reply

`mcp.save`, `mcp.update`, `mcp.remove` and `mcp.set_enabled` keep replying `{}`. A client that
has written re-reads the entry and the catalog: the server normalizes what it stores (map merges,
URL and OAuth validation, a project entry's declared `cwd`), so the result is read back rather
than predicted from the request. Returning the new revision instead would not remove that read.

### D7. No server-side copy of an entry

Duplicating an entry is a client-side draft built from the redacted read. Secrets are not copied:
the credential record is keyed by server id and URL ([MCP OAuth](mcp-oauth.md#41-credential-record-persisted)),
so a copy either shares that credential or needs its own, and neither is decided here. The new
entry starts without the environment, header and bearer values of its original, and the user
enters them again.

### D8. A probe reports its outcome; a failed probe is data

`mcp.test` is a diagnostic of one configuration, not an operation: it opens one disposable
connection, initializes it, lists every tool page, closes it, and replies with what happened.
A timeout, a failed request, a refused credential or an answer the MCP owner cannot use is the
probe's result, so it is reported in the reply rather than as an error of the call.

Only a request the owner refuses before anything is contacted stays an error: an empty or unknown
`id`, an entry its source cannot probe, a plugin `patch`, a draft that is not user-supplied, a
field of the other transport, and a configuration the MCP owner cannot construct. Cancellation
and interruption keep the call's existing meaning: an interrupted probe is not an outcome.

A client shows the result of a test the user asked for. Answering through RPC errors would force
every client to rebuild the outcome from error codes, and would lose the latency of the attempt
that failed.

### D9. The probe and the run view answer different questions

- `mcp.inspect` is the run view: it reports the connection generations the Sessions hold now,
  their state, their last start or transport failure and their last complete discovery, and it
  contacts nothing.
- A transport close moves the connection to `disconnected`, keeps the last tools published and
  callable, and keeps the reason in both `mcp.inspect` and `session.mcp`. `session.mcp` reports
  such a server as `disconnected`, with the reason as its `message`. A failed start is `failed`
  only while its cooldown holds; the cooldown moves it to `disconnected`, and discovery starts
  the server again after each cooldown. The reason stays until a connection succeeds. A
  generation retries its own start: a discovery attempt during the cooldown, a
  `session.mcp.refresh` included, reuses the generation that recorded it and keeps its count,
  and only a successful connection clears them. A refresh replaces the entry's discovery, and
  the Session keeps the entry's lease across the replacement, so the generation outlives the
  refresh and its cooldown holds. Disabling or removing the entry, changing its
  configuration, `mcp.disconnect`, and the owning Session closing retire the generation; so
  does a discovery that ends on its own error, which releases its hold. The next one starts
  again at one second.
  Diagnostic text is bounded to 4 KiB on UTF-8 boundaries. A stdio failure may include
  up to 20 recent stderr lines within that bound; server-emitted text may contain
  secrets and is exposed only to management/status clients, never model requests or logs.
- A remote server that answers HTTP 404 to a request carrying its session id ended that session
  without processing the request: the connection starts a new session and the request is
  replayed once. A second 404 in a row fails the call. The replacement is a new connection
  generation, so a Session republishes its tool list.
- `mcp.test` is a one-shot diagnostic: it contacts the server for a saved entry or a draft and
  reports one terminal outcome. It attaches no Session, publishes no tool, registers no
  connection and writes no configuration, so it says nothing about what runs.
- A server no Session uses has no inspect connection; a probe leaves the inspect reply unchanged.
- What Dim holds stays the credential authority's: the catalog's `auth` field and `mcp.auth.*`
  report it. The probe reports what its attempt met, and shares the authority's behavior for one
  thing: an OAuth entry may refresh its credential, and a `403` records its challenge and scopes,
  as it does for any connection.

### D10. The move is between user and project sources

`mcp.move` moves one entry by id from `user` to `project` or the reverse. The request names the
source and destination; `cwd` is required for the project side and authorized as today. Plugin
entries are not movable (`invalid_params`). The move never changes another source's entry, and
source precedence (project over plugin over user) is unchanged: if a higher-precedence source
holds the same id, the moved entry is shadowed, and the client states that before the move. A
destination **source** that already holds the id is a collision, not a shadow (`mcp_move_conflict`).

### D11. The move is copy-before-delete and not atomic

Settings gives one revision and compare-and-set per source document
([Settings service](settings-service.md#33-persistence-and-revisions)); there is no cross-source
transaction. The move commits the destination side first and removes the source last, and
reports success only after the source is removed. A failure between those writes is
`mcp_move_partial`, not success: the committed state stays, and the client rereads both sources
and follows the direction's recovery in §4.6. §4.6 also fixes the step order and the partial
states.

### D12. Enablement belongs to the destination's owner

Into project, the destination starts disabled. The move clears the user's `mcp-projects` record
for the workspace and id before writing the project entry, so a stale enablement cannot grant
execution, and the source's `enabled` value is not carried into `.mcp.json`. Into user, the
destination user entry is the source's effective configuration: a project entry's resolved
workspace `cwd` becomes the user entry's declared `cwd`, and its current effective enablement
becomes the user `enabled` field; the project record is cleared. The move never enables a project
entry; only the user's `mcp.set_enabled` can.

### D13. Secrets stay in user scope

`.mcp.json` never receives a value the management read withholds. The move splits the source
entry: `.mcp.json` gets the public skeleton (type, command, declared `cwd`, URL origin and the
non-secret OAuth fields), and the user-local `mcp-secrets` source gets the withheld part (args,
environment and header values, the bearer token, the full URL when it carries userinfo, path,
query or fragment, hidden OAuth fields and unknown extension fields), keyed by canonical
workspace and id. Into project the source's declared `cwd` is preserved in the skeleton; the
project resolution applies at run time. Into user the source's effective configuration is
written, so a resolved workspace `cwd` survives the move. The declared configuration merges the
overlay before the effective resolution; the overlay is never returned to a client and is a
user-scope document under Settings' own storage and access rules. A moved project entry is a
Dim-managed skeleton, not a complete standalone configuration for another tool; the user keeps
the entry in user scope if that portability is needed.

### D14. Secret fields of a moved project entry are edited in user scope

While an entry has an overlay, `mcp.configuration` reports `secretsInUserScope: true`.
`mcp.update` refuses a patch that sets `args`, `env`, `headers`, `bearer`, `url`, or the hidden
OAuth fields with `mcp_credentials_are_user_scoped`; `mcp.save` on the entry is refused entirely.
`mcp.save` is a complete single-document replacement: splitting it
across `.mcp.json` and `mcp-secrets` would make a partial save possible, and the client does not
hold the hidden values it would have to submit. `mcp.update` remains the complete path for every
non-secret field, and a non-secret patch is applied to the merged entry but written back as the
public skeleton only; the overlay-held fields stay in `mcp-secrets`. To change a withheld field,
the one-step recovery is `mcp.move` with `{id, from: "project", to: "user", cwd,
expectedFromVersion: <project revision>, expectedToVersion: <user revision>}`; the entry is fully
editable in user scope, and moving it back re-applies the split. The error message names that
move and the Web page offers it as an action. The read still shows the merged names, counts and
`authConfigured`/`oauthConfigured`.

The same refusal covers OAuth as a whole: while the overlay holds `auth` or an `authorization`
header, `mcp.update` refuses every OAuth patch — `set` and `remove`, public and hidden fields —
with `mcp_credentials_are_user_scoped`, because the skeleton would carry OAuth while the overlay
keeps the static credential and the merged entry may not combine the two. Nothing else can
produce that combination: a user entry with both is rejected by the document validation. The
recovery is the same move to user scope, where the static credential is visible and removable;
the Web page hides the OAuth control while the overlay holds the static credential.

### D15. Overlay life cycle follows the project entry

A move into project writes the project skeleton first and the overlay second, then removes the
source. A move back to user writes the user entry first, clears the project enablement, deletes
the overlay and removes the project skeleton last. `mcp.remove` of a project entry removes the
skeleton first and deletes the overlay only once that write commits, and only while the record
still carries the generation it captured before the write. Every overlay write assigns a fresh
generation, so a replacement entry for the same id that a concurrent move committed keeps its
record even when its withheld values are identical. A remove that loses its revision to a
concurrent edit leaves the entry and its overlay intact, never a live entry without its withheld
values. A record whose project entry is already absent is still deleted, so the leftover cleanup
keeps working, subject to the same captured-generation check. `mcp.update` of an existing project
entry preserves its overlay; `mcp.save` is refused while the overlay exists (D14) and refuses a
record whose project entry is gone (D17), naming `mcp.remove` as the cleanup: the save never
deletes a record. A leftover record is not inert — the read and publication paths merge it into
any later entry of the same id — so the save refuses it instead of absorbing it until `mcp.remove`
deletes it.

### D16. OAuth identity is unchanged

The move never changes the server id or the effective URL, and it never reads, writes or deletes
the MCP OAuth credential record (`mcp-oauth.json`). That record is keyed by id and canonical URL
independent of the source ([MCP OAuth §4.1](mcp-oauth.md#41-credential-record-persisted)), so an
existing grant and client identity continue to apply. OAuth tokens stay in that record;
`.mcp.json` and `mcp-secrets` never hold them.

### D17. A record without a project entry refuses the save

A project entry can disappear through an external `.mcp.json` edit while its overlay record stays.
The record is keyed by workspace and id and every read merges it into the entry of that id, so a
later save of the same id would run the deleted entry's withheld values, or fail the transport
merge when the new entry is of another transport. `mcp.save` therefore reads the record for the
canonical workspace and id before it writes: with the entry present, the overlay refuses the save
(D14); with the entry absent it reads the entry again, because a concurrent move may have
committed the destination and its record between the two reads — an entry that now exists refuses
the save (D14), and only an entry still absent answers `mcp_stale_secret_record`, which names
`mcp.remove` as the cleanup. The save deletes nothing: a record it judges stale at guard time may
become the only copy of a concurrent move's withheld values before the write, and when that move
has changed `.mcp.json`, the save's own revision check fails instead. `mcp.remove` with the
project source and id deletes the leftover record (D15) and the caller saves again. An unreadable
overlay or project document fails the save instead of passing the guard.

### D18. Entries decode one by one, and a write keeps the entries it did not change

A `mcpServers` document is decoded entry by entry. An entry that does not decode is not a server:
it stays in the document exactly as written, every other entry loads, and the catalog reports it
in an `invalid` list beside `servers`, so a client that predates the list still reads every valid
row and shows nothing about the broken ones. A broken entry shadows nothing: when a
lower-precedence source holds a valid entry for the same id, the catalog lists that entry as the
server with its own source, beside the broken row. The entry's error names its id and field and
states the expected kind; it never repeats a value, because a wrongly typed header or argument can
hold a credential. `ws` and an entry with a `url` but no `type` say the transport must be `http` or
`sse`; `streamable-http` decodes as `http`. An optional member set to `null` is absent, as it was
before this rule; the required `command` and `url` are not optional and reject `null`. A non-JSON
file and a non-object `mcpServers` still fail the whole document, with the decoder's line and
column and no value. An entry id that is empty or only whitespace fails the whole document too,
because no management call can name it; the error says an entry id is empty and quotes no value.
Every broken entry is its own row in its own source; two sources that hold a broken entry for one
id list two rows and no server.

`$generation` is a reserved entry member. It names the secret overlay record's generation (D13),
and an entry that carried it would collide with that record when the entry is split, so an entry
that has it does not decode. This includes a stored entry written before the member was reserved:
on upgrade it reads as broken, only that entry, and its error names the field. A member dim does
not read, such as `transport`, is unknown instead: the entry loads, a read and an edit of another
member keep it as written, and nothing interprets it; `type` alone names the transport.

The write rule follows from the same value: each entry is held as read beside the entry decoded
from it, and a write serialises from typed values only the entry it changed. Every other entry,
decoded or broken, goes back as the file wrote it, including member order and unknown fields.
The file keeps the order of its ids: a new id is appended, and a removed id leaves its place.
Only the edited entry may change its spelling, so an untouched `streamable-http` entry keeps that
name and a saved one is written as `http`.

A broken entry takes no `mcp-secrets` overlay, and its overlay record is kept. `mcp.remove`
deletes it, `mcp.save` replaces it with a valid definition, and `mcp.update`, `mcp.set_enabled`,
`mcp.test`, `mcp.inspect` and `mcp.configuration` refuse it with its error
(`mcp_invalid_config`). The replacement ends the broken entry's record: a successful project save
deletes the record at the generation it read, so withheld values written for the old entry cannot
enter the new one; a record that changed after that read is kept. A move reads its source entry as
usual: a broken source id answers its error, and a broken destination id is `mcp_move_conflict`.
Plugin content keeps its existing contract: a plugin registers its MCP document as one unit, and
a document with a broken entry is refused for that plugin with the entry's error, because dim
does not manage a plugin entry on its own. The registration is stored in a wrapper document that
owns `dir` and the file's shape, so a plugin document's own top-level members are dropped at
registration rather than written beside it. The stored plugin document decodes entry by entry like
any other source, so one entry that no longer decodes is reported as invalid under its
`<plugin>:<server>` id while the plugin's other entries keep loading, and the plugin's next
registration replaces the whole document and heals it.

Rejected: refuse the whole document on one broken entry, the behavior this design replaces. One
typo then hides every server and makes every edit fail, and the client offers no way to correct
the file. Silently dropping or coercing the broken entry was rejected too: it would lose the
user's text and could hide a credential-bearing value the user meant to fix.

Stored documents decode the same way. A read leaves bytes, version and revision unchanged; a
write that changes one entry still writes every other entry as read.

### D19. Environment references expand when a server is built, and stored text keeps the reference

An entry may write `${NAME}` or `${NAME:-default}` in `command`, an argument, an `env` value,
`cwd`, a remote `url`, a header value or the bearer token, and the daemon expands it from its own
environment when it builds the effective configuration a connection runs. `NAME` matches
`[A-Za-z_][A-Za-z0-9_]*`; any other `$` text is literal, a variable set to the empty string counts
as set, and there is no escape or nesting, so a default ends at the first `}`. Header names,
environment names, `extra` and `oauth` are not expanded. A reference the daemon environment does
not set keeps its text: the entry decodes, loads and is listed with the name, and its connection
fails with its usual transport error rather than a configuration error. A remote `url` that still
names a reference is left to that connection: the publisher's application check does not judge it,
so the source stays applied with its revision, and a probe of the entry contacts nothing and
answers `failed`/`unavailable` with the unset names in the message (§4.5). The stored document
always keeps the text the user wrote; only the expanded copy a connection runs is transient, so a
read, save or move never writes a value back. The legacy import copies such an entry as written, so
an imported reference starts working when the entry connects.

The expansion is one pure function in the MCP configuration module. It runs in the publisher after
the `mcp-secrets` overlay merge and before `config::effective` joins a project or plugin `cwd` to
its workspace, and the expanded copy lives only in the publisher's current source. Every path that
builds a connection outside publication calls the same function first: the `mcp.test` probe, both
temporary-binding sites, and `mcp.auth.start`/`mcp.auth.logout` with the catalog's credential key.
`ConnectionConfig` carries the expanded value only, so nothing expands twice. A `cwd` is expanded
before it is joined. A plugin entry's `cwd` is not expanded: it is confined to the plugin directory
at registration, a reference there has no use, and the existing refusal stays. A temporary binding
is the client's own document, so both of its sites expand with `Origin::User`, where no field is
guarded and no credential names are read.

A document shared with other users or the repository is not trusted to read the daemon's own
credentials. A user entry expands every reference. A plugin entry is guarded in every guarded
field. A workspace entry is guarded where its text is the file's, and not where the member came
from the user-local overlay (`McpSecretEntry`'s `url`, the header keys in `headers` and `auth`),
which is user text. A probe patch's members are user text too: they are what the user is typing.
What is guarded is the remote `url`, each header value and the bearer token: a protected name there
expands to the empty string whether it is set or not, and a default is ignored. Stdio fields are
not guarded, because enabling a workspace stdio entry already runs its command with the user's
authority; the guard covers what a remote entry sends.

The protected names are a fixed list in `config.rs` plus the daemon's live credential variables.
The fixed list covers the model provider, cloud, proxy, code hosting and package registry
variables the daemon knows by name; its prefixes are `DIM_` and `DIMCODE_` plus the exact names
`DIM` and `DIMCODE`, so an unrelated name such as `DIMENSION` is not caught, and matching ignores
case. The live set arrives through the process data face `credentialVariablesHost` in the
capabilities host, provided by the assembly root in `serve`: `variables {} -> {variables: [string]}`,
the names and never a value ([service faces §4.8](../service-faces.md#48-旧主机端口credentialvariableshost-等六个)).
The owner binds its read into `CredentialVariablesHost`, so a Rust consumer calls the same operation
in process and no second public contract exists. The model-config profile serves `AimuxModels::api_key_envs()`; the product
profile reads no credential from an environment variable (`secretRef` is opaque), so it and the mock
profile serve an empty set. The port returns the current set on every call. MCP unions it with the
fixed list and never reads Provider or the model catalogue itself.

Clients see names, never values. `mcp.catalog`'s server rows and the configuration view of
`mcp.configuration` and `mcp.inspect` carry `referencedVariables` and `unsetVariables`: always
present, sorted, deduplicated, and computed from the merged effective configuration. A protected
name used in a guarded field is listed as referenced and never as unset, because the daemon will
not reveal whether it is set. There is no separate list of guarded names. Invalid entries and probe
results carry neither list. `mcp.auth.start`'s authorization URL carries the expanded endpoint as
`resource`, because the OAuth resource must match the endpoint the client connects to. The CLI
list, the `/mcp` panel and the Web settings page show the names; a new client defaults to empty
lists against a daemon that omits them, and an older client ignores them.

`config::public_url` is reference-aware, so a redacted endpoint does not break a reference it
contains: without a reference it keeps its behaviour; with a reference and a `://` it keeps the
scheme and authority as written, in the reference's own case, and drops userinfo, path, query and
fragment; and with a reference and no `://` it keeps the text before any `?` or `#`.

While a remote entry's `url` still names an unset reference, `mcp.auth.logout` cannot build that
entry's credential key and so cannot clear its stored sign-in; the key exists again once the
variable is set. A withheld URL keeps the literal scheme check the secret record always had: text
before the first reference that names a scheme other than `http` or `https` is refused when the
record is written.

### D20. A tool call waits as long as its entry allows

`tools/call` is the one MCP request whose duration is the user's work: a build, a test run, a
browser session, a long query. It takes an optional per-entry `timeout`, an integer number of
milliseconds from 1,000 to 86,400,000 (one day), stored under the same `timeout` key the MCP
configuration format already uses. Without one, a tool call waits up to ten minutes. Every other
request (`initialize`, `tools/list`, `ping` and the rest) keeps the sixty-second limit its
exchange is designed for, and starting a server and the SSE connect timeout keep their own
deadlines. The ten minutes and the sixty seconds are named constants beside the request loop, and
one function picks the limit from the request kind and the entry.

`timeout` is a typed member of a stdio or remote entry, not an unknown field. Any other value — a
number out of range, a fraction, a string, a negative number or `null` — is that entry's error
under D18: it names the entry's id and the field, states the allowed range and never repeats the
value, and every other entry keeps loading. The rule covers stored documents too: a stored
`timeout` of any shape was an ignored unknown member before this decision, and now a value outside
the range makes only its own entry an error, so that server does not load until the value is
corrected. `null` is an error here although D18 reads other optional members' `null` as absent: a
document states "no limit of its own" by leaving the member out, so a written `null` states
nothing and is refused rather than silently read as the default. A patch's `null` is different
(§4.3): it clears the member.

A timeout fails the call with an error that names the limit and where it came from, the entry's
`timeout` or the default, and keeps the existing `Timeout` failure class, so retry behaviour and
the error code are unchanged; only the message carries the limit. The limit is one deadline
measured from the call's start and shared by the session-expiry retry, so the call fails at its
limit whichever attempt is running; connecting counts against it, and a call whose limit has
passed before its request leaves fails with the same timeout and sends nothing. dim sends
`notifications/cancelled` for that request id with the same sentence as its reason, so a server
knows the work it is still doing is unwanted. A user's interrupt cancels at once, as before, and
sends the same notice; that path already sent it, and a test now pins it.

The `mcp-secrets` overlay (D13) carries credentials only. A `timeout` written there is an unknown
member of the overlay: it is not the entry's limit, and it neither shadows nor clears a value the
entry declares. An overlay name the decoder knows for the entry's transport is not an extension
field either: it is dropped when the overlay is merged, so a move to user scope writes only the
entry's own members.

The management read shows `timeout` when the entry states one, and `mcp.update` sets it or clears
it with `null` (§4.2, §4.3). The line and Web clients take it as a document member with no new
flag: `dimcode mcp add` passes the definition through, `mcp set` and `mcp edit` write it in the
patch, the `/mcp` editor seeds a stated value, and the Web settings form has one field for it. The
legacy import copies `timeout` as written, with no unit conversion, under the same per-entry rule;
an invalid value makes that entry an import error and the other entries still import (§10).

Rejected: one global limit, because the value a user already writes in `.mcp.json` is per entry
and a long-running server is exactly the entry that needs it; extending the limit on progress
notifications, because progress says work is happening, not how much is left, and deciding when
to extend the limit is a second contract; and a separate `mcp tool-call-timeout` method, because the
value belongs to the entry's own document where `mcp.save`, `mcp.update` and the legacy import
already carry it.

### D21. An import takes the document text a client read

The client locates and reads a file; it sends the text as `document` and the target source. The
owner never reads a path a client supplies, never scans a directory and, for `document`, names no
other client's file. It accepts only the standard `mcpServers` object, decodes it with the one
entry decoder, imports nothing outside it, and reports other top-level members as
`unknownTopLevel`. There is no second envelope, no format sniffing and no per-client parser:
converting a format another client writes in another syntax is the client's step when it sends
`document`.

D26 adds one narrow exception: a request may name one of the fixed known stores listed in §4.9,
and the owner reads that store itself. The text it reads still never leaves the daemon, so the
reasoning above holds for what a client can see and write; the exception is only about which
component reads the file.

### D22. Preview is a pure report of names, never values

`mcp.import.preview` is a function of the document text, the target source snapshot and, for a
project target, the workspace's `mcp-secrets` records. It writes nothing, executes nothing and
contacts no endpoint. It reports each entry's `id`, transport, status (`identical`, `changed`,
`conflict` or `invalid`), the changed field names with their JSON kinds, the members the decoder
keeps unused or drops, the entry's own decode error, the document's unknown top-level names and
the target revision. A project target is compared as it would serve the entry: the declared entry
with its withheld record merged (D25). Enablement is not part of the comparison: an entry whose
configuration matches is `identical` however it is enabled. Extension members, including those
inside `oauth`, are not part of it either: the decoder keeps them unused, they do not change what
runs, and only one side may hold one, so an entry whose declared members match is `identical`
whatever extension members either side carries. A `replace` writes the document's entry as it
stands, so an extension member only the target holds is not kept, and one only the document holds
is written, into the withheld record for a project entry. A user `identical` entry's `changes`
is empty except `enabled`, which it names when the entry is enabled, because a replace would write
`enabled: false`; a project entry's own `enabled` member is never reported, because the import
writes none. No value from the document or a record appears in the reply (D3), and an entry's
error names its field and expected kind but not its id, which `id` states. Every name the reply
carries comes from the document and is reported once: `changes` names declared members and
`unsupported` the rest, so a document at its 8 MiB cap yields a reply of about one document
length, well inside the 16 MiB transport frame. Each entry reports at most 32 names in `changes`
and 32 in `unsupported`, and `omitted` counts the distinct names those caps left out.

### D23. One compare-and-set covers the accepted selection

`mcp.import.apply` takes the document, the preview's revision and one resolution per entry
(`import`, `skip`, `replace` or `rename` with a new id), with `skip` as the default. It re-parses
the document and re-checks every resolution against the source it reads at `expectedVersion`; it
never trusts a preview result sent back. For a project target that source is the file and the
workspace's withheld records (D5), so the token names the pair the preview compared against: a
record another writer renewed in between answers `mcp_settings_conflict` before any write, exactly
as a changed file does. Every accepted entry enters that one snapshot and is
committed by one source update: one revision check, one source write, and no entry visible before
the commit. A project import withdraws the enablement record of every entry it writes before the
source update and writes its withheld records after it (D24, D25).
A refused resolution answers before either write, so the request changes nothing; a stale revision
answers `mcp_settings_conflict` and leaves the source as it was, though an entry the import would
have replaced may be left disabled (D24); a request that accepts no entry writes nothing. Entries
it does not touch keep their stored value, broken entries included (D18).

The record write is the last step and cannot join the source compare-and-set: it runs after the
source commits. It therefore replaces a record only while that record still carries the generation
the plan saw, and keeps one another writer renewed in between. The source entries are already
written at that point, so the reply is `mcp_import_partial` (D25, §4.8); replacing the entries
again writes both.

### D24. An import never enables anything

The document's `enabled` member is not carried as enablement. A user entry is written with
`enabled: false`; a project entry is written without the member and gets no `mcp-projects`
record, so only `mcp.set_enabled` can enable it (ADR 003 D3). An entry whose configuration is
identical is not rewritten, so an enabled user entry keeps its enablement. Every id a project
import writes, imported or replaced, has its `mcp-projects` record withdrawn before the source
write, so a replace starts disabled like an import and an imported configuration never runs
enabled; this includes a destination whose entry an external edit removed and whose record
stayed. Withdrawing first makes an import fail closed: one that answers `mcp_settings_conflict`,
or fails before or during the source write, writes no entry, but an entry it would have replaced
may be left disabled, and `mcp.set_enabled` enables it again. The withdrawn records are not
restored after a failed write, because two imports that replace one entry can interleave so that
the failed one re-enables the entry the successful one wrote. The record is inert while no entry
holds the id, so withdrawing one changes nothing else, and the same write is what `mcp.remove`
would do if the entry were still there.

### D25. A project import writes through the withheld split

A project import writes what a move into project writes (D13): `.mcp.json` receives the public
skeleton, and the user-scope `mcp-secrets` record for the canonical workspace and id receives
every withheld member — arguments, environment and header values, the bearer token, a URL with
userinfo, path, query or fragment, hidden OAuth fields and unknown members. None of those values
enters a file commonly committed, a `${NAME:-literal}` default among them included, and no
confirmation is asked: the preview never shows a value, so a user could not verify the one a
confirmation would name. The preview compares the configuration the target would serve, the
declared entry with its record merged, so an entry already stored as a skeleton and a record
reads `identical`.

The destination's record is written from the import rather than merged with what was there. An
entry with a withheld part replaces the record with a fresh generation (D15); an entry without
one removes it. A leftover record whose entry an external edit deleted therefore neither refuses
the import nor leaks into the imported entry: the import carries the withheld values itself, so
the hazard `mcp.save` refuses (D17) does not arise here. `replace` and `rename` write the same
pair as `import`.

The two stores cannot commit together. Every written id has its enablement record withdrawn
before the source write (D24), so the entry cannot run while it reads its new skeleton with its
old record, which it does until the `mcp-secrets` update commits; a withdrawal that fails answers
before the source write, leaving `.mcp.json` as it was. The source update commits next and the
withheld records last. A record write that fails after the source commit answers
`mcp_import_partial`: the entries are written with their skeletons, their withheld values are
missing or old, and replacing the entries again rewrites both. The reverse order is not used: it
would write a record whose source entry does not exist yet, and a failed source write would leave
the old entry running with the new import's credentials.

### D26. A known store is read by the owner, on request, and never leaves the daemon

An import may name one of a fixed set of known stores another local client keeps, through
`from`; the owner reads that store itself and converts it to the standard document. The owner
reads the §4.9 files only on request: `mcp.import.sources` reads each to report its state and
count, and `from` reads the one it names. It never reads a path a caller supplies, a profile or a
plugin server, never scans a directory and never watches a file. It never caches, writes or
executes anything in a store, never expands a `${NAME}` it contains, and never runs a
`headersHelper` or any command. The converted text is used in place of `document` and never
appears in a reply: `mcp.import.sources` reports only each store's identity, display path, state
and entry count, the preview repeats that row in `store`, and the preview and apply take the
store id in place of the text. The preview
answers `sourceRevision`, a digest of the converted document, and the apply requires it as
`expectedSourceRevision`, so what the apply writes is the text the preview read; a store that
changed in between answers `mcp_settings_conflict` before any write. That keeps preview and apply
on one document exactly as the `document` input does by construction.

## 4. Contract

Namespace `mcp`, process scope, served on local connections
([Management methods § Dispatch and lifecycle](management-methods.md#dispatch-and-lifecycle)).
`source` is `"user"`, `"project"` or `"plugin"`; `cwd` is required for a project source and is
the workspace whose `.mcp.json` is read, authorized as today.

These are the methods that read or write an entry's configuration, exercise it, or import a
document into a source:

| Method | Request | Reply | Writes |
| --- | --- | --- | --- |
| `mcp.configuration` | `{cwd?, id, source}` | `{revision?, enabled, secretsInUserScope, configuration: Configuration}` | no |
| `mcp.save` | `{cwd?, id, source, expectedVersion, configuration}` | `{}` | the source |
| `mcp.update` | `{cwd?, id, source, expectedVersion, patch}` | `{}` | the source |
| `mcp.move` | `{cwd?, id, from, to, expectedFromVersion, expectedToVersion}` | `{}` | the source and destination |
| `mcp.import.sources` | `{cwd?}` | `{sources: Source[]}` (§4.9) | no |
| `mcp.import.preview` | `{cwd?, source, document? \| from?}` | `ImportPreview` (§4.8) | no |
| `mcp.import.apply` | `{cwd?, source, document? \| from?, expectedVersion, expectedSourceRevision?, resolutions}` | `{revision}` | the source |
| `mcp.test` | `{cwd?, id, source, configuration?, patch?}` | `Probe` (§4.5) | no configuration; the credential authority's, as for any connection |

`mcp.save` takes a complete stdio, HTTP or SSE definition in the existing `mcpServers` format and
creates or replaces that source's entry, not a merged one. The namespace's other methods are
listed in §6 and keep their references: `mcp.catalog`, `mcp.inspect`, `mcp.remove`,
`mcp.set_enabled` and `mcp.disconnect` in the App Server's
[MCP configuration reference](../../../crates/dim-app-server/README.md#mcp-configuration-management),
`mcp.auth.*` in [MCP OAuth §9.1](mcp-oauth.md#91-management-methods-namespace-mcp-local-transports-only),
and `mcp.temporary.capabilities` in [TemporaryMcp](../plugins/TemporaryMcp.md). §3 D5 states the
revision every write takes.

`mcp.catalog` reports one revision and one application record per source:

| Field | Type | Meaning |
| --- | --- | --- |
| `userRevision`, `projectRevision` | string | The source's revision token (D5); `projectRevision` is absent without a project |
| `userApplication`, `pluginApplication`, `projectApplication` | `{appliedRevision?, error?, cleanupError?}` | The publisher's state for that source. `appliedRevision` is the source token of the configuration that is applied: `userRevision` for a user source and `projectRevision` for a project one, so a client comparing the two sees whether the configuration it read is the one applied. A plugin source has no catalog token; its `appliedRevision` is its own `mcp-plugins` document revision. It is absent when the source is not applied, and `error` names why |

`mcp.catalog` returns `invalid: [{id, source, error}]` beside `servers` (D18). `mcp.configuration`,
`mcp.inspect`, `mcp.test` and `mcp.set_enabled` answer a broken entry with its error and
`mcp_invalid_config`; `mcp.save` replaces it, and `mcp.remove` deletes it. Each server row also
carries the entry's `referencedVariables` and `unsetVariables` (D19). The import pair reads one
standard `mcpServers` document, supplied as `document` or read from a named known store through
`from`, and writes the entries a selection accepts (§4.8); against a broken entry it reports the
entry's own error and can replace it (§4.8). `mcp.import.sources` reports the known stores
without reading a document for a client (§4.9).

### 4.1 Entry read (`mcp.configuration`)

| Field | Type | Meaning |
| --- | --- | --- |
| `revision` | string, absent for a plugin entry | The source's revision, the token `expectedVersion` takes (D5): Settings' revision for a user entry, and for a project entry a digest of that revision and this workspace's withheld records |
| `enabled` | boolean | The enablement the entry runs with: for a user or plugin entry its own field, enabled when absent; for a project entry the user's record (D2) |
| `secretsInUserScope` | boolean | True when the project entry has an `mcp-secrets` overlay; its withheld fields are edited in user scope (D13, D14) |
| `configuration` | `Configuration` | The declared configuration projection below; `enabled` is reported above and is not part of it |

```text
client                                  Mcp (process)                     Settings
  | mcp.configuration {source,id,cwd?}     |                                 |
  |--------------------------------------->| read the entry's source         |
  |                                        |-------------------------------->|
  |                                        |<-- {revision, document} --------|
  |                                        | project entry: enablement record|
  |<-- {revision, enabled, secretsInUserScope, configuration} -|             |
  | user edits one field                   |                                 |
  | mcp.update {expectedVersion: revision, |                                 |
  |             patch}                     | patch the declared entry, then  |
  |--------------------------------------->| compare-and-set that revision   |
  |                                        |-------------------------------->|
  |<-- {} ---------------------------------|                                 |
  | re-read to show what is stored         |                                 |
```

### 4.2 `Configuration`

The declared entry's projection. A stdio entry:

| Field | Type | Meaning |
| --- | --- | --- |
| `type` | `"stdio"` | Transport |
| `command` | string | The executable as declared |
| `argsCount` | integer ≥ 0 | How many declared arguments are stored; the values are withheld |
| `cwd` | string or `null` | The declared working directory; a project or plugin entry's relative path, not a resolved one, and `null` when the source declares none |
| `envNames` | string[] | Declared environment names, ascending; values withheld |
| `timeout` | integer 1000 to 86400000, present when the entry states one | The entry's `tools/call` limit in milliseconds; absent means the ten-minute default (D20) |

A remote entry:

| Field | Type | Meaning |
| --- | --- | --- |
| `type` | `"http"` or `"sse"` | Transport |
| `endpoint` | string | `scheme://host[:port]` of the declared URL; userinfo, path, query and fragment withheld |
| `headerNames` | string[] | Declared header names, ascending; values withheld |
| `authConfigured` | boolean | A static bearer token is stored |
| `oauthConfigured` | boolean | An `oauth` object is stored |
| `oauth` | object, present when `oauthConfigured` | `clientId?` string, `callbackPort?` integer 1 to 65535, `scopes?` space-separated string, `metadataConfigured` boolean; the metadata URL itself is withheld |
| `timeout` | integer 1000 to 86400000, present when the entry states one | The entry's `tools/call` limit in milliseconds; absent means the ten-minute default (D20) |

Every configuration projection carries `referencedVariables` and `unsetVariables` (D19): the
sorted, deduplicated names the merged effective configuration references and the ones the daemon
environment does not set. Both are always present; `mcp.catalog` server rows carry the same two
fields, while an invalid entry and a probe result carry neither. A protected name used in a guarded
field is referenced and never unset. Values are never listed. For a declared URL that carries a
reference, `endpoint` keeps the scheme and authority as written, including a reference there, and
still withholds the rest.

Withheld from every reply and error message: environment and header values, the bearer token,
URL userinfo, path, query and fragment, the authorization server metadata URL, argument values,
unknown extension fields of an entry and of its `oauth` object, and every credential of
[MCP OAuth](mcp-oauth.md) (D3).

### 4.3 Edit (`mcp.update`)

The patch fields and what an edit does with them; `mcp.save` takes a complete server definition
in the `mcpServers` file format instead.

| Patch field | Applies to | Omitted | Present |
| --- | --- | --- | --- |
| `command` | stdio | kept | replaced |
| `args` | stdio | kept | replaces the whole list, including an empty list |
| `cwd` | stdio | kept | replaced; an empty string clears it |
| `env` | stdio | kept | per key: a string sets the value, `null` removes the key |
| `url` | remote | kept, with everything the projection withholds | replaced |
| `headers` | remote | kept | per key, as `env` |
| `bearer` | remote | kept | `{action:"set",token}` replaces, `{action:"remove"}` clears |
| `oauth` | remote | kept | `{action:"remove"}` clears the object; `{action:"set"}` changes only the fields it names, `null` clearing an optional one |
| `timeout` | stdio and remote | kept | the millisecond limit replaces it; `null` clears it and the ten-minute default applies (D20) |

A field of the other transport is `invalid_params` (`-32602`). The patched entry is validated as
a whole document before it is stored, so an edit cannot persist a configuration that `mcp.save`
would refuse, including one that sets `oauth` beside static credentials.

A project entry with `secretsInUserScope: true` (D13, D14) refuses a patch that sets `args`,
`env`, `headers`, `bearer`, `url`, or the hidden OAuth fields (`authServerMetadataUrl`, unknown
`oauth` extension fields) with `mcp_credentials_are_user_scoped`; `mcp.save` on that entry is
refused entirely. The message
names the one-step recovery: `mcp.move {id, from: "project", to: "user", cwd,
expectedFromVersion: <project revision>, expectedToVersion: <user revision>}`. After the move
the entry is fully editable; moving it back re-applies the overlay split. `command`, `cwd`,
`oauth.clientId`, `oauth.callbackPort` and `oauth.scopes` remain editable without a move. A
non-secret patch is applied to the merged entry and written back to `.mcp.json` as the public
skeleton only; the overlay-held fields stay in `mcp-secrets` and are never copied into the file.

### 4.4 Errors

| `appError` | When |
| --- | --- |
| `mcp_unknown_server` | The target source holds no entry with that id, for a read or a write; a move whose source lacks the id. A broken entry answers its own error instead |
| `mcp_invalid_config` | The patched or saved document, an import document that is not JSON or exceeds the size, entry or id limits, a named store that is `too-large` or `malformed`, an empty `id`, an unauthorized workspace, or an operation on a broken entry is refused; a broken entry's error names its id and field |
| `mcp_settings_conflict` | `expectedVersion` is not the source's current revision (D5), including a project entry's withheld record renewed since the token was read, an overlay/enablement compare-and-set lost, or an import's `expectedSourceRevision` differs from the store it re-read (D26) |
| `mcp_settings_read` | The source could not be read, or a named store is `absent` or `unreadable` (§4.9) |
| `mcp_move_conflict` | The destination source already holds an entry with that id; nothing is written |
| `mcp_move_partial` | A destination-side write committed and the source removal did not; the client rereads and follows the direction's recovery: into project remove the destination and retry, into user remove the project source (D11, §4.6) |
| `mcp_credentials_are_user_scoped` | A patch on a moved project entry sets a field held in `mcp-secrets`, or a save targets that entry; move the entry back to user scope (D14) |
| `mcp_stale_secret_record` | A project save finds a withheld record whose project entry is absent; call `mcp.remove` with that id and the project revision to delete it, then retry (D17) |
| `mcp_import_conflict` | An import resolution cannot apply to the source read at `expectedVersion`: `import` on an id the source holds with different content or as a broken entry, `replace` on an id it does not hold, a `rename` destination already taken, or a non-skip resolution on an invalid entry; no entry is written and no record changes (§4.8) |
| `mcp_import_partial` | A project import's source write committed and its withheld records did not: the update failed, or a record it planned was renewed by another writer since the plan and is kept. The source entries stay as written and their `mcp-secrets` records are missing or old, so the entries run without the document's withheld values until they are replaced again (D25, §4.8) |
| `invalid_params` (`-32602`) | A field of the wrong transport, a project target without `cwd`, a plugin source, the same `from` and `to`, an import that names both or neither of `document` and `from`, an unknown `from`, a project-scoped `from` without `cwd`, `expectedSourceRevision` with `document` or without `from`, an import resolution naming an entry the document does not contain, a `newId` on another action, or a write to a plugin entry, which answers `plugin MCP entries change through their plugin` |
| `local_transport_required` | The connection is not local |
| `service_reloading` / `service_unavailable` | Mcp or its row is not serving; from [management methods](management-methods.md#dispatch-and-lifecycle) |

A read serves a disabled entry: enablement is a field, not a condition for reading. An unset
environment reference is not a configuration error (D19): the entry loads and its connection
reports the transport's usual failure. A probe of a remote entry whose URL still names one answers
`failed` with the names (§4.5).

### 4.5 Probe (`mcp.test`)

`mcp.test` exercises one configuration: it opens one disposable connection, initializes it, lists
every tool page, closes it, and reports what happened. It attaches no Session, publishes no tool,
registers no connection and writes no configuration (D9). It expands environment references
exactly as a connection does, with a complete configuration's and a patch's members treated as
user text (D19); its reply carries no variable lists, though a failure message may name the
variables a URL still references.

The request is unchanged, and a complete `configuration` is a user-supplied draft without
`patch`. Otherwise the probe uses that source's declared entry with an optional unsaved `patch`,
so stored secrets are preserved by the patch rules in §4.3 (D4). A user entry is probed even when
it is disabled, as an explicit local action; a project or plugin entry must be enabled. The
30-second deadline and the cleanup after every outcome are unchanged.

The reply is a `Probe`:

| Field | Type | Meaning |
| --- | --- | --- |
| `status` | `"connected"` \| `"authRequired"` \| `"failed"` | The terminal outcome: the connection initialized and every tool page was listed; the server refused the attempt for lack of a usable credential; anything else (D8) |
| `latencyMs` | integer ≥ 0 | Daemon-measured time from opening the disposable connection to the terminal outcome, excluding cleanup |
| `tools` | `Tool[]` or `null` | Every discovered tool with `name`, `description` and `inputSchema`; `null` unless `connected`. The count is this list's length |
| `expired` | boolean, with `authRequired` | Dim held a credential and the authorization server rejected it, rather than Dim holding none |
| `missingScopes` | string[], with `authRequired` | The scopes a `403 insufficient_scope` named while discovering |
| `reason` | `"timeout"` \| `"unavailable"` \| `"response"` \| `"cleanup"`, with `failed` | The deadline passed; the connection, a request or the server failed; the server's answer was refused; the disposable connection did not close cleanly |
| `message` | string, with `failed` | The detail the same failure carried as an error before; it never carries a configuration value, a credential or a callback URL |

`connected` is the only outcome that lists tools; `expired` and `missingScopes` are present only
with `authRequired`, and `reason` and `message` only with `failed`.

An OAuth entry's probe uses the stored credential as any connection does, which can refresh it
and record a `403` challenge with its scopes; the catalog's `auth` report can therefore gain
`missingScopes` after a probe (D9). No other state changes: no source revision, no tool published
to a Session, no connection generation created or replaced.

Only a request refused before anything is contacted stays an RPC error: `invalid_params` for an
empty `id`, a plugin `patch`, a complete `configuration` for a non-user source or a field of the
other transport (§4.4); `mcp_unknown_server` for an unknown `id`; `mcp_unavailable` for a project
or plugin entry that is not enabled; and, unchanged, the error a configuration answers when the
MCP owner cannot construct it (D8). A remote URL that still names an unset reference is not such a
refusal: the probe contacts nothing and answers `failed`/`unavailable` with the names in its
message (D19). An interrupted probe answers `operation_interrupted`, and a probe while Mcp is not
serving answers `service_reloading`
([management methods](management-methods.md#dispatch-and-lifecycle)).

### 4.6 Move (`mcp.move`)

`mcp.move` moves one entry by id between the user and project scopes. It is the only operation
that reads the complete entry, including withheld values, and writes another source; no value it
moves is returned to the client.

| Field | Type | Meaning |
| --- | --- | --- |
| `cwd` | string, required for the project side | Workspace whose `.mcp.json` is read or written; authorized as today |
| `id` | string | Entry id; non-empty |
| `from`, `to` | `"user"` or `"project"` | Source and destination; must differ |
| `expectedFromVersion` | string | The source's revision token (D5): for a project side the same digest `mcp.catalog` reports as `projectRevision`, for a user side the document revision |
| `expectedToVersion` | string | The destination's revision token (D5), in the same form |

The owner reads both sources and the user-local `mcp-secrets` and `mcp-projects` records, checks
both revisions, and refuses before writing when the source lacks the id
(`mcp_unknown_server`), the destination already holds it (`mcp_move_conflict`), or the full entry
is invalid (`mcp_invalid_config`). Plugin entries are not movable, and the same `from` and `to`
is `invalid_params`. A user side's token is `missing` when that source has no document. A project
side's token is never that sentinel: it is the digest over the document revision — `missing`
itself when the workspace has no `.mcp.json` — and the workspace's withheld `(id, generation)`
records, exactly as `mcp.catalog` reports `projectRevision`. A document revision is therefore no
substitute for a project side's token, and sends a move to `mcp_settings_conflict`.

The complete entry (the overlay merged in, for a project source) is validated as one
`mcpServers` entry before any write. The move then writes in this order:

Into project:

1. clear `mcp-projects` for the workspace and id (CAS);
2. write the public skeleton to `.mcp.json` (CAS on `expectedToVersion`);
3. write the withheld part to `mcp-secrets` (CAS);
4. remove the entry from `mcp.json` (CAS on `expectedFromVersion`).

Into user:

1. write the source's effective entry, including the overlay, to `mcp.json` with `enabled` set to
   the project entry's current effective enablement and a project entry's resolved workspace
   `cwd` as the declared `cwd` (CAS on `expectedToVersion`);
2. clear `mcp-projects` for the workspace and id (CAS);
3. delete the overlay (CAS);
4. remove the entry from `.mcp.json` (CAS on `expectedFromVersion`).

The order is the recovery contract. Into project, the entry is disabled and its skeleton exists
before the overlay, so a failure leaves a disabled entry without secrets, never an enabled entry
without them. Into user, the complete user entry exists before the project side is withdrawn, so
a failure leaves both entries, with the project entry disabled and still effective; the user
entry is complete and becomes effective when the project entry is removed. A move reports `{}`
only after the source entry is gone. A failure before the enablement clear answers the ordinary
error and changes nothing; a failure after only the enablement clear leaves no destination entry
and can be retried. A failure after a destination-side write answers `mcp_move_partial`; the
client rereads both sources and recovers by direction:

| Direction | Safe recovery |
| --- | --- |
| into project | remove the destination with `mcp.remove` and retry the move; the source still holds the complete entry. Never remove the source while the destination may be incomplete |
| into user | remove the project source with `mcp.remove`; the user entry is complete, and removing the project source also deletes the overlay and skeleton |

The into-project recovery also covers the case where the overlay committed: removing the complete
destination and retrying is safe and avoids guessing whether the overlay is present. If no
destination entry exists, the client retries the move; the enablement clear alone is harmless.
The into-user recovery is safe at every phase after the user entry commit.

Retrying `mcp.move` unchanged is not idempotent after a destination-side write: the destination
source now holds the id and the retry answers `mcp_move_conflict`. The deterministic finish is:

- into project: remove the destination with `mcp.remove`, using the destination revision from
  `mcp.catalog`; then resend `mcp.move` with the unchanged source revision and the destination
  revision the catalog reports after the removal. Do not resend before the removal.
- into user: remove the project source with `mcp.remove`; the user entry is complete. Do not
  resend the move.
- A partial that only cleared enablement has no destination entry; resending the move with the
  current revisions is safe.

The `mcp_move_partial` message names the direction and the next operation, and the Web page
offers it as an action. There is no resume flag and no skip-a-step mode.

The source and destination revisions are checked once before the first write, and every write is
CAS. A concurrent write to either source, the overlay or the enablement record makes that write
fail (`mcp_settings_conflict`, or `mcp_move_partial` if a destination write already committed).
No lock spans a move: two calls through the same daemon may interleave, and a second daemon on
the same home is ordered only by the per-document CAS. The order in §4.6 makes every
interleaving recoverable — a destination write that loses its CAS commits nothing, a source
removal that loses its CAS leaves both entries with the source still effective, a removal of one
entry never deletes withheld values that a concurrent write has just committed (D15), and a save
refuses a record it cannot prove stale instead of deleting it (D17).

The overlay is a user-scope Settings document `mcp-secrets` (`mcp-secrets.json`), keyed by
canonical workspace and id. Its record is the withheld part of one project entry:

```json
{
  "entries": {
    "/canonical/workspace": {
      "github": {
        "$generation": "4d1c…",
        "args": ["--stdio"],
        "env": {"TOKEN": "…"},
        "headers": {"Authorization": "…"},
        "auth": {"type": "bearer", "token": "…"},
        "url": "https://user:pass@host/mcp?key=…",
        "oauth": {"authServerMetadataUrl": "…", "extra": {}},
        "extra": {}
      }
    }
  }
}
```

Only the members that exist on the source entry are present. When the withheld part is empty,
the move writes no overlay record and `secretsInUserScope` stays false. The project `.mcp.json`
skeleton contains the safe members: `type`, `command`, declared `cwd`, `url` reduced to
`scheme://host[:port]` when the full URL carries userinfo, path, query or fragment, or to its
scheme and authority as written when it carries an environment reference (D19), and
`oauth.clientId`/`callbackPort`/`scopes`. The owner merges the overlay into the declared
configuration before `config::effective`; the merge replaces `args` and `url`, merges `env`,
`headers`, `auth`, `extra` and the hidden `oauth` members, and is validated as a whole. A record
for a workspace and id with no project entry is not inert: every read merges it into the entry of
that id, so `mcp.save` refuses it with `mcp_stale_secret_record` (or with D14 when a concurrent
move has just created the entry) and names `mcp.remove`, which deletes the record as its leftover
cleanup (D15, D17). The move never creates an overlay without a destination entry.

The overlay is never read by a client method. `mcp.configuration`, `mcp.catalog`, `mcp.inspect`,
`mcp.test` and every error show only the redacted projection (D3); `secretsInUserScope` is the
only new fact. It is a user-scope Settings document under the storage and access rules of that
service.

`mcp.remove` of a project entry that has an overlay withdraws enablement, removes the project
entry, and deletes the overlay once that write commits and only while the record still carries the
generation captured before the removal. Every overlay write assigns a fresh generation, so a
replacement entry for the same id that a concurrent move committed keeps its own record even when
its withheld values are identical. A failure at the removal leaves the entry disabled with its
overlay intact; a failure at the overlay deletion leaves the entry gone and an overlay that a
repeated `mcp.remove` deletes (D15, D17). Neither leaves an enabled entry without its secrets.
`mcp.save` on a project entry with an overlay is refused with `mcp_credentials_are_user_scoped`;
use `mcp.update` for its non-secret fields or move it back to user scope. Moving back to user
deletes the overlay in the same operation.

### 4.7 The `mcp-secrets` document

`mcp-secrets` is a user-scope-only Settings source (`mcp-secrets.json`; no project file) owned by
Mcp. It is a `BusinessDocument` with one opaque revision for the whole document; every move,
remove or project import that changes it takes that revision as CAS. The client never supplies it
and no client method reads the document; as a Rust source, `settings.read` answers
`settings_unknown_source`, and the MCP methods return only redacted projections. Settings'
user-scope storage and access rules apply (0600 file on Unix or the native row), so the overlay
has the same protection as the other MCP documents.

The document is:

```json
{
  "entries": {
    "/canonical/workspace": {
      "github": {
        "$generation": "4d1c…",
        "args": ["--stdio"],
        "env": {"TOKEN": "…"},
        "headers": {"Authorization": "…"},
        "auth": {"type": "bearer", "token": "…"},
        "url": "https://user:pass@host/mcp?key=…",
        "oauth": {"authServerMetadataUrl": "…", "extra": {}},
        "extra": {}
      }
    }
  }
}
```

Validation at the Settings boundary rejects unknown fields at every level except the two
designated extension maps (`extra` on the record and on `oauth`), a non-absolute or empty
workspace, an empty id, empty environment/header names, an empty bearer token, and a `url` that
is not an absolute URL. A record may carry only the members of its entry's transport; a stdio
record with `url`/`headers`/`auth` or a remote record with `args`/`env` is invalid. The merged
entry is validated again as a whole before use.

Every write of a record assigns it a fresh opaque `generation`. It is the record's identity, not
a value: a removal deletes the record only while its generation still matches the one captured
before the removal, so a replacement with equal withheld values is a different record and is kept.
No client method returns it. The member is `$generation`, a reserved name: an entry's own members
never use it, because the record flattens those members into `extra` and a `$generation` there is
refused at the Settings boundary. A record written before the generation existed, or one whose
`generation` member is an ordinary entry extra, reads with an empty `$generation`, which a removal
then matches only as that empty value.

The key is the canonical workspace path at move time, the same key `mcp-projects` uses; the two
documents are independent (one holds enablement, the other hidden values) and both are written by
the move. A renamed or moved workspace therefore changes the key: the old record is inert, the
new path has no overlay and reports `secretsInUserScope: false`, and the user re-runs the move or
re-enters the values. Deleting a workspace leaves an inert record; it is never applied to another
workspace. There is no background scavenger; `mcp.remove` or a move back to user deletes the
record while the workspace is still reachable.

No format version is added: this is a new Settings document validated at its boundary, not a
binary store. A missing file reads as empty, and a future incompatible shape changes the source
name or adds a version rather than silently reusing this one.

### 4.8 Import (`mcp.import.preview`, `mcp.import.apply`)

The client reads a file and sends its text as `document`; or it names one known store of another
local client as `from`, and the owner reads that store itself and converts it (§4.9). Either way
the owner decodes the standard document and imports selected entries into one source (D21, D26).
Only the standard `mcpServers` object is read, with the one entry decoder (D18). A document
larger than 8 MiB or holding more than 1,000 entries is `mcp_invalid_config`; the size is
checked before parsing and the count after, and neither writes anything. A file that is not
JSON, a non-object `mcpServers` and an empty id fail the whole document; an entry's own error
makes that entry invalid and leaves the rest usable. The reply names only ids and field names
taken from the document. Each entry reports at most 32 names in `changes` and 32 in
`unsupported` and counts the rest in `omitted`, so a 1,000-entry document stays well under the
16 MiB transport frame.

`mcp.import.preview` takes:

| Field | Type | Meaning |
| --- | --- | --- |
| `cwd` | string, required for `source: "project"` and for a project-scoped `from` | Authorized workspace whose `.mcp.json` is the target, and the workspace a project store belongs to |
| `source` | `"user"` or `"project"` | Target source |
| `document` | string, exactly one of `document` and `from` | The file's text |
| `from` | string, exactly one of `document` and `from` | One id from §4.9; the owner reads that store and converts it |

It replies:

| Field | Type | Meaning |
| --- | --- | --- |
| `revision` | string | The target source's revision, the token `mcp.import.apply` takes as `expectedVersion` (D5); `missing` when a user source has no document, while a project source's token is a digest and never that sentinel |
| `sourceRevision` | string, present with `from` | The digest of the converted store document, the token the apply takes as `expectedSourceRevision` (D26); absent with `document`, which the apply carries itself |
| `store` | `{id, client, scope, path}`, present with `from` | The store the preview read, with the display values `mcp.import.sources` reports for it; absent with `document`. It names the store, never one of its entries or values (D26). The request's own target is `source`, so the row the preview read is `store` |
| `entries` | `ImportEntry[]` | One row per entry in the document's order |
| `unknownTopLevel` | string[] | Top-level members other than `mcpServers`, ascending; an import does not carry them |

A store that is not `ready` answers its state's code before any preview: `mcp_settings_read` for
`absent` or `unreadable`, `mcp_invalid_config` for `too-large` or `malformed`, with the message
`mcp.import.sources` shows for it. An unknown `from`, both inputs together and a project-scoped
`from` without `cwd` are `invalid_params`.

An `ImportEntry` is:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | The document's entry id |
| `transport` | `"stdio"` \| `"http"` \| `"sse"`, absent when the entry does not decode | The transport the entry would run |
| `status` | `"identical"` \| `"changed"` \| `"conflict"` \| `"invalid"` | What importing the entry would do. Extension members, including those inside `oauth`, are not configuration and never change it |
| `changes` | `[{field, kind}]` | The declared field names and the JSON kind each holds (`string`, `number`, `boolean`, `array`, `object`, `null`). Extension members, those inside `oauth` included, stay in `unsupported`, never here, and they never make an entry `conflict`. Never a value. `conflict` lists the fields that differ from the target entry; `changed` lists every field of the entry the write would add; `identical` is empty except `enabled` on a user entry that is enabled, which a replace would set to `false` |
| `unsupported` | `[{field, effect}]` | Members the decoder does not act on, by name. `effect` is `kept-unused` for an entry's or its `oauth` object's extension member, preserved in the document and unused, or `dropped` for an unknown `auth` member, which the decoder discards |
| `omitted` | number | How many distinct names the 32-name caps left out of `changes` and `unsupported`; the reported lists are then partial |
| `error` | string, optional | The entry's decode error, with `invalid`, or a project destination whose stored record does not merge into its declared entry, with `conflict`; it names the field and the expected kind and never a value, and it does not repeat the entry's id, which `id` states |

`status` compares the entry, normalized for the target scope (D24), with the configuration the
target source read at `revision` would serve, ignoring `enabled` on both sides because enablement
is the user's state: a project entry is the declared entry with its `mcp-secrets` record merged
(D25), so a skeleton and its record read as the document's entry. `identical` means the source
already serves the same configuration, `conflict` means it holds the id with different content, as
an entry that does not decode, or with a record that does not merge, and `changed` means the id is
free. Extension members, including those inside `oauth`, take no part in this comparison (D22), and
a `replace` writes the document's entry as it stands, so an extension member only the target holds
is not kept. A preview reads the target source and, for a project target, the workspace's records;
it writes nothing, executes nothing and contacts nothing (D22).

`mcp.import.apply` takes the same input — `document`, or `from` with the preview's
`sourceRevision` as `expectedSourceRevision` — the same `source`, the preview's `revision` as
`expectedVersion`, and `resolutions`. With `from` it re-reads and converts the store and compares
the digest before any write; a different digest answers `mcp_settings_conflict` with a message
saying the store changed since the preview, before any write, and the remedy is the same: preview
again. With `document`, `expectedSourceRevision` is `invalid_params`.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | The document entry the resolution applies to; at most one resolution per entry, and an entry without one is skipped |
| `action` | `"import"` \| `"skip"` \| `"replace"` \| `"rename"` | What to do with the entry |
| `newId` | string, with `rename` | The destination id; non-empty |

`import` accepts a free id and an `identical` entry; an id the source holds with different
content or as a broken entry is `mcp_import_conflict`. `import` checks
the id it writes and `rename` its `newId`, after the source's own ids have answered, so a taken
id stays `mcp_import_conflict`. `replace` accepts an id the source holds, decoded or broken,
whether or not a record is stored for it. `replace` writes the entry as an import writes it
(D24): a user entry is written with `enabled: false` and a project entry without the member, so
replacing an enabled user entry disables it; the preview's `changes` names `enabled` when a
replace would do that (D22). `rename` writes the entry under `newId`, which must be free in the
target source and not another resolution's destination. An `invalid` entry can only be skipped.
An `id` the document does not contain and a `newId` on another action are `invalid_params`.

The apply re-parses the document and re-checks every resolution against the source it reads at
`expectedVersion`; it uses no preview result (D23). Every accepted entry enters that one snapshot
and is committed by one source update, so one compare-and-set and one settings write cover the
whole selection. A `mcp_import_conflict` refusal answers before any write, so nothing changes. A
stale revision answers `mcp_settings_conflict` and leaves the source's entries as they were,
though an entry the import would have replaced may be left disabled (D24). Entries it does not
touch keep their stored value, broken entries included (D18).
A request that accepts no entry writes nothing and answers the source's current revision.

An import never enables anything (D24): the document's `enabled` member is not carried as
enablement, a user entry is written with `enabled: false`, and a project entry is written without
the member and gets no `mcp-projects` record. Every id a project import writes, replaced ones
included, has its enablement record withdrawn before the source write, so an imported
configuration never runs enabled and a failed compare-and-set writes no entry but leaves an entry
the import would have replaced disabled; `mcp.set_enabled` enables it again. A project import
writes through the withheld split (D25): each accepted entry enters the source as its public
skeleton, and the workspace's `mcp-secrets` record receives the withheld members, replacing or
removing the destination's record. The records are written in one update after the source commit;
if that write fails, the reply is `mcp_import_partial` and replacing the entries again rewrites
both stores.

### 4.9 Discovery (`mcp.import.sources`, and `from`)

`mcp.import.sources` reports the known stores, read-only, only when a request asks. It takes an
optional `cwd`; without one it reports the three user stores, and with an authorized `cwd` it also
reports `claude-code:local` and the two project stores, using the same authorization as every
other `mcp.*` method. It reads each distinct file once and writes nothing. A preview that reads a
store through `from` names the same row it read in its `store` field, so a client that previews
without listing the stores first still knows which file the entries came from; the row carries
identity, display path and scope, never an entry or a value.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | `claude-code:user`, `claude-code:local`, `codex:user`, `codex:project`, `omp:user` or `omp:project` |
| `client` | `"claude-code"` \| `"codex"` \| `"omp"` | The client that owns the store |
| `scope` | `"user"` \| `"local"` \| `"project"` | The scope as that client names it |
| `path` | string | Display path: `~/…` under the home directory, workspace-relative for a project row, absolute otherwise. For `claude-code:local`, the file plus the `projects` key it reads |
| `state` | `"ready"` \| `"absent"` \| `"unreadable"` \| `"too-large"` \| `"malformed"` | The store's state |
| `entries` | number, with `ready` | How many server entries the store holds; `ready` with 0 is an existing store that declares none, not `absent` |
| `error` | string, with `unreadable` or `malformed` | The owner's message; it names the problem and never a value |

| id | Client | Scope | File | What is read |
| --- | --- | --- | --- | --- |
| `claude-code:user` | Claude Code | user | `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json` | Top-level `mcpServers` |
| `claude-code:local` | Claude Code | local | The same file | `projects["<canonical cwd>"]`, else `projects["<request cwd>"]`, then its `mcpServers`; a missing key is `absent`, and no other project key is enumerated |
| `codex:user` | Codex | user | `$CODEX_HOME/config.toml`, else `~/.codex/config.toml` | `[mcp_servers.<id>]` tables |
| `codex:project` | Codex | project | `<workspace>/.codex/config.toml` | The same |
| `omp:user` | OMP | user | `~/.omp/agent/mcp.json` | `mcpServers` and the other top-level members |
| `omp:project` | OMP | project | `<workspace>/.omp/mcp.json` | The same |

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` are honoured only when absolute in the daemon's environment.
These files are never read: `<workspace>/.mcp.json` (Dim's own project source),
`<workspace>/mcp.json`, OMP and Codex profiles, and plugin servers.

Each store converts to the standard document. Claude Code and OMP copy their `mcpServers` object;
for OMP the other top-level members are carried too, so `unknownTopLevel` names `disabledServers`,
`enabledServers` and `$schema`. `~/.claude.json` carries nothing else: its other members are
application state, not a configuration document. A Codex `[mcp_servers.<id>]` table maps:

| Codex member | Standard member |
| --- | --- |
| `command`, `args`, `env`, `cwd`, `enabled` | The same member |
| `url` | `url` and `type: "http"` |
| `bearer_token_env_var = "NAME"` | `auth: {type: "bearer", token: "${NAME}"}` |
| `http_headers` | `headers` |
| `env_http_headers = {"X" = "NAME"}` | `headers: {"X": "${NAME}"}` |
| `tool_timeout_sec` (integer) × 1000 | `timeout`, in milliseconds |
| Any other member | Kept as written, so the preview reports it `kept-unused` |

An out-of-range value makes that entry `invalid` under D20; it is never clamped or dropped. No
`${NAME}` is expanded and no `headersHelper` or other command is run.

Bounds and files: each source file is read through one handle, must be a regular file, and is
refused as `too-large` above 32 MiB from its metadata before it is read; it is never truncated.
A file that grows past the cap after that check is still refused when the read reaches it. A user
store follows symlinks. A project store is canonicalized and must stay inside the authorized
workspace, or the row is `unreadable` and its target is never read. Reading and parsing run on a
blocking task.
`~/.claude.json` is parsed as a stream, so no call holds a copy of the file; a non-UTF-8 byte
inside a skipped member may go unnoticed, while every member the import uses is still decoded as
UTF-8. The converted document still passes the import's own 8 MiB and 1,000-entry caps, and a
store over either is that row's `too-large`.

## 5. What changes for a client

- The reply of `mcp.configuration` stops being the bare projection and becomes
  `{revision?, enabled, secretsInUserScope, configuration}`. That is a breaking change on
  protocol v3 for any client that decodes the old reply: the projection is now under
  `configuration` and the new boolean is additive. This repository's Web settings page changes
  in the same implementation; the native CLI adopts this read and patch: `dimcode mcp edit`/`set`
  and the `/mcp` panel read `mcp.configuration` and write `mcp.update`, and every other native
  verb keeps its reply; every other client sees the projection replaced by the wrapper.
- An empty `id` on the entry read answers `mcp_invalid_config`, the code `mcp.save` already
  returns, instead of `invalid_params`.
- The reply of `mcp.test` stops being `{tools}` and becomes the probe report in §4.5, and a probe
  that fails is a reply rather than an RPC error. That is also a breaking change on protocol v3:
  a client that read `{tools}` must read `tools` under the report and take the outcome from
  `status`. The Web settings page changes in the same implementation; the native CLI adopts the
  report in `dimcode mcp test`; every other client sees the wrapper replace the old reply.
- The edit flow is `mcp.configuration` then `mcp.update` with the returned `revision`; creating an
  entry takes the token from `mcp.catalog`'s source revision (D1).
- `mcp.catalog` gains an additive `invalid` list of entries that did not decode. A client shows
  them apart from the servers with their error, offers removal, and offers replacement through
  the add flow; it does not offer toggle, edit, test or inspect for them. A client that ignores
  the list still reads every valid `servers` row.
- `mcp.catalog` server rows and the configuration projections of `mcp.configuration` and
  `mcp.inspect` gain `referencedVariables` and `unsetVariables`: sorted, deduplicated names from
  the merged effective configuration (D19). A client shows them as names and never as values, and
  points at an unset name when a connection fails. A daemon that predates them omits both, so a
  new client defaults to empty lists; an older client ignores them. Invalid entries and probe
  replies carry neither.
- An entry's `command`, arguments, environment values, `cwd`, `url`, header values and bearer
  token may carry `${NAME}` or `${NAME:-default}`, which the daemon expands when it connects; the
  stored document keeps the reference text (D19). A client edits and saves that text as written,
  and a move keeps it, so a client that reads a name list must not substitute a value.
- `mcp.auth.start`'s `resource` is the entry's expanded endpoint, so a client does not compute it
  from the possibly referenced URL.
- The configuration projections of `mcp.configuration` and `mcp.inspect` gain an optional
  `timeout` and the patch gains a nullable one (D20). A client shows it when present, edits it in
  milliseconds, and clears it with `null`; a client that predates it ignores the field. The entry
  definition `mcp.save` takes carries it unchanged.
- A client renders `command`, `cwd` and `endpoint`, the name lists and the counts, and offers
  replace-or-keep for `args`, environment values, header values, the bearer token, the URL and
  the metadata URL. It shows `argsCount`, `envNames` and `headerNames` as the fields that a
  replacement would replace.
- When `secretsInUserScope` is true, the client shows that the withheld fields are stored in the
  user's local configuration and disables their editors; the owner refuses those patch fields.
  Moving the entry back to user scope restores full editing. When the read reports both
  `secretsInUserScope` and `authConfigured`, the static credential is withheld, so the client
  hides the OAuth control too (D14); a withheld `authorization` header is not visible to the
  client, and the owner refuses that OAuth patch with the move-to-user recovery.
- The move flow takes both revisions from `mcp.catalog`, asks for confirmation, and calls
  `mcp.move`. The client explains that into project the entry starts disabled and its withheld
  values stay in the user's local configuration; into user the entry keeps its current enabled
  state. On `mcp_move_conflict` or `mcp_settings_conflict` it keeps the form and rereads; on
  `mcp_move_partial` it follows the direction's recovery in §4.6: into project remove the
  destination and retry, into user remove the project source; it never removes the source while
  the destination may be incomplete. On success it reloads the catalog and opens the destination
  entry. The client never displays a moved withheld value.
- The import flow is: the client reads the file (for `document` the daemon reads no path it
  supplies), or it names a known store with `from` and the daemon reads it (§4.9); it calls
  `mcp.import.preview` with that input and the target source, and renders each entry's `status`,
  `changes`, `unsupported` and `error`. It shows field names and kinds, never values, and offers
  no per-entry confirmation: a project import writes through the withheld split (D25), so the
  page states that the workspace file receives the public skeleton and the withheld values go to
  the user-scope record. It then calls `mcp.import.apply` with the preview's `revision` and one
  resolution per entry; an entry without a resolution is skipped. On `mcp_settings_conflict` it
  previews again; on `mcp_import_conflict` it keeps the form and resends with the same revision,
  because the refusal wrote nothing. `mcp_import_partial` says the source was written and the
  records were not, so the client offers a replace of the same entries rather than a plain
  import. On success it reloads the catalog.
- The client states that an import starts every entry disabled: a user entry is written with
  `enabled: false`, and a project entry needs the user's local enablement. Locating and reading an
  arbitrary file, and converting a format another client writes in another syntax, remain the
  client's own steps when it sends `document`. `from` covers only the fixed stores in §4.9, whose
  text stays inside the daemon (D21, D26).
- Discovery is opt-in: a client calls `mcp.import.sources` when the user asks, once per request.
  It shows each row's id, client, scope, path, state and entry count or owner message, offers a
  preview only on a `ready` row, and never asks for, holds or renders a store's text. A preview
  opened from a row sends `from`; the apply sends the same `from` and the preview's
  `sourceRevision` as `expectedSourceRevision`. The client does not poll, watch or cache a store,
  and does not read the file itself (D26).
- A client must not write back `mcp.inspect` values (D1).
- A client shows a probe's `status`, `latencyMs` and tools, offers sign-in on `authRequired` (with
  `expired` and `missingScopes` when present), and shows `reason` and `message` on `failed`. It
  takes "no live connection" from `mcp.inspect`, not from a probe (D9).
- The Desktop client (arcships/dim-electron#20) replaces its `mcp.get` and `mcp.update`
  `capability_unavailable` answers with `mcp.configuration` and `mcp.update`, and its `mcp.test`
  answer with the probe report.

## 6. What does not change

- `mcp.catalog` keeps its `servers` rows and source revisions and gains the additive `invalid`
  list (D18); `mcp.inspect`, `mcp.save`, `mcp.remove`, `mcp.set_enabled` and
  `mcp.disconnect` keep their requests and replies, including `mcp.inspect`'s effective
  configuration and its connection generations. `mcp.configuration` gains the
  `secretsInUserScope` field (§4.1); `mcp.update` keeps its request and reply, but a moved
  project entry's withheld fields are refused as §4.6 states, and `mcp.save` refuses the
  leftover record §4.6 describes.
- `mcp.test`'s request, its 30-second deadline, its cleanup and its credential behavior are
  unchanged; only its reply changes (§4.5).
- The MCP request loop keeps one limit per request kind: `timeout` changes only a `tools/call`
  request's, so starting a server and every other request behave as before (D20). The
  cancellation notice a timeout sends is the one an interrupted request already sent.
- The namespace gains four methods, `mcp.move`, `mcp.import.sources`, `mcp.import.preview` and
  `mcp.import.apply`; `hello.features` advertises them and a client checks support before offering
  the control. The import pair gains the optional `from` input and the preview's optional
  `sourceRevision`; a client that sends `document` sees the same request and reply with the field
  absent, and a client that predates discovery keeps working.
- The probe adds no error code. The move, the overlay and the import add six:
  `mcp_move_conflict`, `mcp_move_partial`, `mcp_credentials_are_user_scoped`,
  `mcp_stale_secret_record`, `mcp_import_conflict` and `mcp_import_partial`. Existing codes
  keep their meanings.
- No new persisted document: an import writes the existing source document and, for a project,
  the existing `mcp-secrets` record of every entry it writes; it writes no enablement record and
  withdraws the one an entry already had (D24, D25).
- The `mcp` and `mcp-projects` business documents, their file names, source precedence, project
  enablement record, plugin registration and the `mcp-oauth.json` credential store keep their
  formats. A new user-scope `mcp-secrets` source holds the migration overlay; it has no prior
  format and a missing file reads as empty. No existing persisted data moves or changes meaning,
  so no format version changes.
- No new plugin, part, Timeline event or notification. D19 adds one capability port,
  `CredentialVariablesHost`, whose operation is the data face `credentialVariablesHost`, registered
  shared in the capabilities service catalog and provided by the assembly root in `serve`. Assembly, routing, the MCP Session
  part and its connections are otherwise untouched; the MCP row registers the new Settings source
  itself.

## 7. Events and state

None. Entry configuration is process configuration changed through its owner's typed writes, as
[design rules §4 rule 9](../../architecture/rust-agent/principles.md) requires; it never enters
the Timeline. Revisions come from the stored document, not from an event. Mcp registers the
user-scope `mcp-secrets` source and follows its changes like its other sources; the overlay is
business configuration, not an event.

## 8. Dependencies

| Dependency | Use |
| --- | --- |
| `managementMethods` ([management methods](management-methods.md)) | The MCP row registers the namespace and its methods during `apply` |
| `settings` ([Settings service](settings-service.md#42-service-settings-owner-path)) | Reading a source with its revision, compare-and-set writes, project authorization; the `mcp`, `mcp-projects` and `mcp-secrets` sources |
| PluginManager ([§3.7](../plugins/PluginManager.md#37-content-registration)) | A plugin entry is registered content; Mcp reads it as declared and writes nothing |
| `McpAuth` ([MCP OAuth](mcp-oauth.md)) | The `mcp.auth.*` methods of the same namespace; the credential record is untouched by a move |
| `credentialVariablesHost` (capabilities host, D19) | The names of the environment variables the daemon reads credentials from; `McpPublication` and `McpPlugin` declare it, and the assembly root in `serve` provides it |

## 9. Cross-module requirements

One requirement is raised to the capabilities host: the `CredentialVariablesHost` port with its
data face `credentialVariablesHost`, registered shared in the service catalog and provided by the
assembly root in `serve` (D19). None is raised to the runtime, rutis, Settings
or Rpc: the revision and the source snapshot already exist, and the change stays inside the MCP
owner and its documents and reply shapes.

Client-side follow-ups, which are not requirements on another owner: the Web settings page (this
repository) adds the move control and admits exactly `mcp.move` for it, as
[Web workspace §4.3](../web-workspace.md#43-settings-and-account-management-on-the-local-web-entry)
states; the Desktop client (arcships/dim-electron#20) adopts §5.

## 10. Tests

Flow level, reusing the existing MCP flows:

| File | What it protects |
| --- | --- |
| `crates/dim-app-server/tests/transport_flow/product_protocol.rs` | The wire contract on local v3: the read returns the revision the catalog reports for the same source; creating an entry with the catalog's source revision succeeds and a stale revision is `mcp_settings_conflict`; a plugin entry's read has no revision and a write to it is `invalid_params`; a project entry patched on one unrelated field leaves `.mcp.json` without a resolved `cwd` and without an `enabled` key (`project_entry_edits_keep_the_declared_configuration`); no withheld value appears in the read, including after an edit (`mcp_editor_preserves_redacts_and_removes_credentials_with_source_cas`); `mcp_scope_move_preserves_withheld_values_and_enablement` moves one entry in each direction, keeps its withheld values out of `.mcp.json`, refuses `mcp.save` and withheld patches with `mcp_credentials_are_user_scoped`, and leaves a removed entry saved clean; `mcp_save_refuses_a_leftover_overlay_and_remove_cleans_it` refuses a save over a withheld record whose entry an external edit deleted with `mcp_stale_secret_record`, proves `mcp.remove` deletes the record, and saves again into a working workspace catalog across transports; `mcp_update_refuses_oauth_over_an_overlay_static_credential` refuses both OAuth actions while the overlay holds the token and completes the move-to-user recovery; and a probe reports `connected` with a latency and every tool, or `failed` with `reason`/`message` and no tools, without changing the source revision or creating a Session connection |
| `crates/dim-app-server/tests/transport_flow/mcp_import.rs` | The import wire contract: a preview of a multi-entry document reports each status, the changed field names and kinds, the unsupported members and their effects, the unknown top-level names and the target revision, and writes nothing; apply imports, replaces, renames and skips entries with one revision, keeps a skipped target as written and a re-import of unchanged entries writing nothing with no duplicate; an enabled user target with the same configuration previews `identical` and an `import` leaves it enabled, while a `replace` over an enabled entry names `enabled` in its changes and writes it disabled; a user import writes `enabled: false`, while a project import writes each entry as its public skeleton and puts argument, environment and header values, the bearer token, a URL with userinfo or a query, hidden OAuth fields, unknown members and a reference carrying a literal `${NAME:-default}` in the workspace's `mcp-secrets` record, so `.mcp.json` carries none of them and the contract has no confirmation; the same document then previews `identical`, and an identical entry is replaceable and renamable; a leftover record whose entry an external edit deleted is replaced or removed by the import instead of refusing it, and a stale enablement record is withdrawn so the entry starts disabled; a replace of an enabled entry starts disabled; untouched broken entries survive; a stale revision is `mcp_settings_conflict` and an unknown resolution id is `invalid_params`; a malformed, oversize or over-count document is `mcp_invalid_config` |
| `crates/dim-app-server/tests/transport_flow/mcp_discovery.rs` | The discovery wire contract on a daemon with a disposable `HOME`: the six rows in order with their states and counts, `ready` with zero apart from `absent`, `unreadable`, `too-large` and `malformed` staying on their own row, project rows only with an authorized `cwd`, and another project's key never appearing; a workspace symlink pointing outside is `unreadable` and its target is not read; a preview with `from` gives the entries the same converted text gives, and an apply writes them disabled with a Codex timeout in milliseconds and its bearer token as `${NAME}`; no fixture secret appears in any `sources`, preview or apply reply; editing the store between preview and apply answers `mcp_settings_conflict` and writes nothing; both or neither of `document` and `from`, an unknown id, a project `from` without `cwd` and `expectedSourceRevision` with `document` are `invalid_params` |
| `crates/dim-app-server/tests/transport_flow/web_entry.rs` | The authenticated Web entry advertises and reaches the MCP namespace including `mcp.move`, `mcp.import.sources` and the import pair, and a bare connection is refused |
| `crates/dim-app-server/tests/mcp_flow.rs` | The service level: a source's read returns its declared entry with the source revision, and a project entry's enablement comes from the user's record, while the effective entry a Session runs resolves the workspace `cwd` (`editing_reads_the_declared_entry_while_running_resolves_it`); `scope_move_partial_is_recoverable_and_remove_cleans_the_overlay` and `scope_move_back_to_user_keeps_the_merged_entry_and_its_enablement` cover a move in each direction, the withheld part, the destination's enablement, the overlay deletion and the cleared record; `scope_move_checks_the_destination_id_and_both_revisions` covers `mcp_move_conflict` and a stale revision on either side; `a_conflicting_remove_keeps_the_project_entry_and_its_overlay` covers the same-revision race and the removal order; `remove_keeps_a_replacement_moves_overlay` captures a removal's record, commits the removal, lets a replacement move with the same id and identical withheld values write a new generation, and proves the earlier cleanup keeps the replacement's record while a later removal deletes it; `a_save_racing_a_move_keeps_the_moves_overlay` walks the reads a racing save makes — the id absent with no record, a move into the project, the record read returning the move's record, and the entry read returning the entry with `secretsInUserScope`, the state D17 requires `mcp_credentials_are_user_scoped` for rather than the stale refusal — and asserts the move's overlay and credential survive and the racing save's revision-checked write loses; the handler's re-read itself has no injection point in a flow test, and the scoped answer is asserted through `mcp.save` on an overlay entry in `mcp_scope_move_preserves_withheld_values_and_enablement`; `an_into_project_partial_is_finished_after_a_restart` restarts on the same home after an injected overlay write failure and finishes the move by the documented recovery without credential loss; `a_stored_plugin_document_decodes_entry_by_entry` keeps a broken stored plugin entry invalid on its own, answers its own error on a path that contacts the server, and heals it on re-registration; and `a_plugin_document_cannot_set_the_wrappers_members` registers a document with a top-level `dir` and another extension member, reads the stored file back with one `dir` and its entries intact, and registers a second plugin |
| `crates/dim-app-server/tests/mcp_oauth_flow.rs` | `scope_moves_keep_the_oauth_record_and_the_sign_in_state` moves a signed-in OAuth entry in each direction: the `mcp-oauth.json` record and the `authorized` state survive, no token appears in `.mcp.json`, and `mcp.configuration`, `mcp.inspect` and `mcp.update` keep their replies for the moved entry; a probe of an unauthorized entry reports `authRequired`, with `expired` where Dim's stored credential was rejected and `missingScopes` where the server answered `403 insufficient_scope` |
| `crates/dim-app-server/tests/mcp_http_flow.rs` | A tool call under an entry's `timeout` fails after about that long with an error naming the millisecond limit and the entry, and the fixture receives `notifications/cancelled` for that call's id with the same sentence; the same tool without a `timeout` answers after two seconds under the ten-minute default; a call whose limit passes while the connection is starting fails with the same error and sends neither the call nor a cancellation; an interrupt sends the same notice. |
| `crates/dim-app-server/tests/mcp_session_flow.rs` | The management read omits `timeout` until `mcp.update` sets it, then shows it, and removes it after a `null` patch; an out-of-range value is `mcp_invalid_config` naming the field and never the value. |
| `crates/dim-app-server/src/mcp/config.rs` (unit) | An entry that does not decode is held out of `mcp_servers` and reported with its field and expected kind while the rest load; an error never repeats a token-like value; an untouched entry, including `streamable-http` and unknown members, is written back in the order it was read, and only an edited entry is serialised from its typed value; only a non-JSON file or a non-object `mcpServers` fails the document; a `timeout` from 1000 to 86400000 loads and any other value, including `null`, is that entry's error; an overlay `timeout` is not the limit and is dropped from a write |
| `crates/dimcode/tests/cli_flow/mcp.rs` | `mcp add` passes a `timeout` member through unchanged, `mcp set` writes one and clears it with `null`, and `mcp inspect` shows a stated one in milliseconds; `mcp import --discover` prints the rows and no value, `--from ID` previews and applies with the id and the digest, and a path, `--from` and `--discover` are mutually exclusive. |
| `crates/dim-migration/tests/migration_flow.rs` | Legacy `mcpServers` import entry by entry: decodable entries are written as the legacy file had them, a broken entry is recorded `Unsupported` as `mcpServers.<id>` with its error and never reaches the native store, and a fully valid file imports exactly as before; a `timeout` imports as written and a fraction is that entry's import error |

Environment expansion (D19) has its own coverage. The `config.rs` unit tests pin the syntax, every
walked field, the guard rules by origin, the protected list including `DIMENSION_X` and `DIM_X`,
the reference-aware `public_url`, and the rule that a stored document keeps its text. The session
flow covers a daemon variable at connect, the probe with a complete configuration and with a
patch, a temporary binding, the guard of a configured credential variable, the overlay's unguarded
members, and an unset reference the catalog names while the entry still loads, including a URL
reference that keeps its source applied while the probe reports the name. The transport flow
asserts the catalog and configuration fields on the wire, including their absence on invalid
entries and probe results. `mcp_flow.rs` keeps a reference through reads, writes and moves and
proves a write never stores an expanded value; `mcp_oauth_flow.rs` discovers from the expanded URL;
the CLI flow prints the names in `mcp list` and the detail view; the Web settings test renders
them; and the migration flow pins an imported reference as written.

The discovery module's unit tests pin the Codex mapping table, a non-integral
`tool_timeout_sec` staying unmapped, Claude user and local extraction, OMP's top-level members,
absolute-only overrides, a project symlink outside the workspace, the 32 MiB source cap, and that
another project's record is skipped without being built. The Web settings test pins the Discover
rows, that only a `ready` row offers a preview, and that a discovered preview sends `from` and the
apply its digest.

The redaction assertions of
`mcp_editor_preserves_redacts_and_removes_credentials_with_source_cas` extend to the new reply
shape rather than moving to a new test: the same fixture writes argument, environment, header,
bearer and URL secrets and reads the entry back. The move tests assert the same fixture's
secrets never appear in the reply, `.mcp.json`, a Web page response or a log.

The failure test uses a disposable home and makes the overlay write fail (`mcp-secrets.json`
replaced by a directory), asserts the `mcp_move_partial` answer, and restarts the App Server on
the same home. It then asserts the partial state and finishes it by the direction's documented
recovery without credential loss or duplicate execution.

The probe is asserted through the management method, in the transport flow, because its outcome
is the reply rather than a service result the fixture can call; its auth outcomes need the OAuth
fixture. The 30-second deadline and the `cleanup` outcome are not exercised by a flow test:
reaching the deadline costs 30 seconds of real time for one outcome, and no fixture makes the
disposable view fail to close. The refusal paths that assert the spawned child has closed show
that a probe returns only after its cleanup ran; they reply `failed`/`unavailable` and
`failed`/`response`, not `failed`/`cleanup`.

The sixty-second limit on every request other than a tool call is not exercised by a flow test
for the same reason: reaching it costs sixty seconds of real time for one assertion.

## 11. Rule check

| Rule | Answer |
| --- | --- |
| §4 rule 1 (commands for another owner's state) | No event. Entry configuration is process configuration written through the MCP owner's typed methods; the MCP owner is the writer of its own state |
| §4 rule 2 (handover) | Not applicable: no object is handed between modules |
| §4 rule 3 (`before` callbacks) | None |
| §4 rule 4 (core without business meaning) | No core change; Rpc dispatches the namespace as it does today |
| §4 rule 5 (batch) | Not applicable: no events are written |
| §4 rule 6 (model context) | Not applicable: nothing enters the model context. MCP tool declarations are unchanged, and a probe's discovered tools are a reply to the client that asked, not tools published to a Session or a model |
| §4 rule 7 (public events and services only) | The MCP row uses Settings' public service, reads the entries PluginManager registered, and reaches the credential authority through its own part; it reads no other part's internal state. The `mcp-secrets` overlay is MCP's own user-scope document |
| §4 rule 8 (infrastructure unchanged) | No change to the Timeline, Dispatcher, event envelope, Session or branch facades. One process host value is added: the capabilities catalog registers `CredentialVariablesHost`, and the assembly root in `serve` provides it beside `ModelSelectionHost` (D19, §9) |
| §4 rule 9 (only facts later decisions use) | Configuration stays out of the Timeline; a Session broadcast notice is not added |
| §1 (Capability and the App Server boundary) | No new part and no new Capability. The MCP process row and its Session part keep their state; the change is in the process row's management methods |
| ADR 002 D1 (scope follows the owner of state) | Entry configuration is shared across Sessions, so it stays in the process owner |
| ADR 002 D2 (no wiring modules) | One port is added to the capabilities host (`CredentialVariablesHost`, D19), whose operation is the `credentialVariablesHost` data face, not a wiring module; `mcp.move` is one method on the existing MCP row |
| ADR 002 D3 (dependencies point to the core) | `McpPublication` and `McpPlugin` declare the capabilities host `CredentialVariablesHost`; it points to the core, and Settings is already declared |
| ADR 002 D4 (structural versus business configuration) | Business configuration stays in its source, read on use; the `mcp-secrets` overlay is user-scope business configuration and reloads nothing |
| ADR 002 D5 (language-neutral contracts) | The request, reply and error shapes are JSON data; the revision is an opaque string |
| ADR 003 D3 (project entries start disabled) | A move into project clears the user's enablement record before the project entry exists and never writes `enabled` into `.mcp.json`; project content cannot enable itself. A move into user preserves only the user's existing enablement choice. An import writes `enabled: false` into a user entry, no `enabled` member into a project entry, and withdraws the enablement record of every project id it writes (D24) |

Scope migration adds no new rule answer: it writes MCP-owned Settings documents through the
owner's typed writes, uses the public Settings and OAuth contracts, and starts a project
destination disabled. It is explicitly not a cross-source transaction; §4.6 is the recovery
contract. Environment expansion (D19) adds the two answers above: the capabilities port and the
dependency on it. Import adds no new rule answer: it is one method pair on the existing MCP row,
reads the source snapshot and writes that source's document and the workspace's `mcp-secrets`
record through the owner's typed Settings writes, and starts every project entry disabled
(D24, D25). Discovery adds none either: `mcp.import.sources` is one read method on the same row,
reads fixed local files through the process owner, writes nothing and returns no value; the
`toml` crate is a wire-boundary parser inside the App Server's MCP module, not a core dependency
(D26).

## 12. Alternatives considered

| Alternative | Why not |
| --- | --- |
| Add `mcp.get` beside `mcp.configuration` | Two reads of one entry; `mcp.configuration` already is the read. The Desktop client maps its own name onto it |
| Include each entry's configuration in `mcp.catalog` | The list would carry every entry's redacted configuration and cannot carry a per-entry token that means anything: the revision belongs to the source, and the list is not the entry snapshot a client writes with. It would also make every list render the widest possible projection |
| Return secrets to local clients (`settings.read` precedent) | A stored token would then travel to every local UI, be cached and be written back by an editor. Each owner protects its own secrets ([Settings service §1.2](settings-service.md#12-decision-secrets-belong-to-their-owner)); the read withholds them and the patch preserves them |
| Return `args` values | A stdio command line may carry a token in an argument, and the redaction flow test treats argument values as secret. A replacement remains possible, with the count shown |
| Return the new revision from the writes (`provider.update` precedent) | A client reads the entry back after a write anyway, because the server normalizes what it stores; the four writes keep one reply shape, and `mcp.remove` has no entry to return |
| Keep the read and the patch on the effective configuration | Then a client cannot echo a value it read: a project entry's resolved `cwd` and the user record's `enabled` would be persisted into `.mcp.json` by an edit of another field |
| A server-side `mcp.duplicate` | It would carry secrets between entries without deciding whether the credential record is shared or copied; the copy's enablement and source would need their own decision |
| Client-side save plus remove for a move | The redacted read loses every withheld value, and a complete save would have to carry them; the destination id, both revisions and enablement would be handled by the client, not the owner |
| Write the complete entry into `.mcp.json` | It writes env/header values, bearer tokens, URL credentials and argument values into a workspace file that is commonly committed. D13 keeps them in the user-local overlay |
| Refuse moves of an entry with withheld values | It makes the required both-direction move impossible for ordinary entries; the overlay preserves them without a readback |
| A cross-source transaction in Settings | Settings owns per-document revisions and CAS; a cross-document transaction is a new mechanism for one operation. The copy-before-delete order and `mcp_move_partial` give explicit recovery instead (D11) |
| Merge a project entry with the lower-precedence user entry that has the same id | It changes the source precedence and makes a hidden user entry affect a project entry implicitly. The explicit `mcp-secrets` overlay has one meaning and one writer |
| Reuse `mcp-oauth.json` for static secrets | That record is keyed by id and URL, not workspace, and its lifecycle is OAuth sign-in and refresh. Static env/header/bearer values have a different key and lifetime; a separate source is smaller than changing the OAuth format |
| Route a project entry's secret patch into `mcp-secrets` | It would make every edit of a moved entry a two-document, two-revision write, and the migration does not need it. Moving the entry back to user scope is the supported edit path (D14) |

| Keep a failed probe an RPC error | Every client would rebuild the outcome from error codes and lose the latency of the attempt that failed. A request the owner refuses before contacting anything stays an error, so nothing is hidden |
| Report only an outcome code, without the tool list | The probe already lists every tool page and a client asks for both in one action; the count is that list's length, so no field repeats it |
| Attach a Session to measure what runs | A diagnostic must not create, publish to or replace a Session, and `mcp.inspect` already reports the connections Sessions hold (D9) |
| Record nothing from the probe's connection | It would need a flag on the credential path, and the catalog would ignore a challenge the server just sent. A probe already refreshes a credential as any connection does, so recording the challenge with it is the same rule |
| One `mcp.probe` method beside `mcp.test` | Two methods that open a disposable connection and list tools; the Desktop client's name maps onto the existing method |
| Expand references where each connection is built | The catalog and the configuration views would keep raw text while a connection ran another value, and publication, the probe, a temporary binding and sign-in would each expand separately. One expansion in the publisher gives every path the same copy and the same name lists |
| Expand at decode time and store the values | A stored document would then hold expanded secrets and lose the text the user wrote; the expansion depends on the daemon environment, so a copied or moved entry would freeze one machine's values |
| Return expanded values to local clients | A value would travel to every local UI and be written back by an editor, the same reason the read redacts it. The name lists give a client what it can show without the value |
| A `guardedVariables` field beside the two lists | A guarded name is a referenced name whose value the source may not read; it is not a third kind of reference, and a client shows the same name either way |
| Expand a plugin entry's `cwd` | It is confined to the plugin directory at registration by refusing an absolute path, `..` or a symlink escape. Expanding it would let the plugin's document name a directory outside that confinement |
| A `$NAME` form without braces, or nesting | An ordinary `$` already occurs in command lines and defaults, so the braced form is the one users of npm and systemd expect; refusing nesting keeps one pass enough and makes a default end at the first `}` |
| Guard every field, including stdio | Enabling a workspace stdio entry already runs its command with the user's authority, so a stdio reference adds no exposure; guarding it would change commands that legitimately read a protected name. The guard covers what a remote entry sends |
| A client loop calling `mcp.save` per entry | It takes one revision check and one write per entry, so a failure leaves a partial import, and the client would reimplement the decode, same-id and project policy that the owner already enforces. The owner pair makes the selection one all-or-nothing write (D23) |
| Replace the whole source document with the imported one | It deletes entries the user did not select and drops broken entries that D18 keeps as written; import writes only the entries a resolution accepts |
| Write the document's literal values into `.mcp.json` behind a per-field confirmation | The preview names fields and never values (D3), so the confirmation asks a user to approve a value they never saw, and a `${NAME:-literal}` default passed the reference checks as a reference. The file is commonly committed; the split keeps every withheld value in user scope (D25) |
| Refuse every literal project value and accept only references | An ordinary stdio server's arguments are literal, so that rule would make most project imports impossible; the split accepts the entry and keeps the literal out of the file (D25) |
| Write the withheld records before the source document | A failed source write would leave the old entry running the new import's credentials, and the record would name an entry that does not exist yet. The source-first order leaves every written entry disabled until a replace rewrites both stores and answers `mcp_import_partial` (D25) |
| Import `enabled: true` from the document | It would let a file enable a project entry without the user's local record, contradicting ADR 003 D3; every import starts disabled (D24) |
| Return the converted document from a discovery read | Another client's environment, header, URL and OAuth values would then travel to the CLI and the Web page, be cached and be written back by an editor, which is the exposure D3 and D22 exist to prevent. The owner already holds the text after reading it, so naming the store with `from` on the existing preview and apply keeps every value where it was read (D26) |
| A second preview/apply pair for known stores | It would duplicate the decoder, the preview comparison and the one-write apply for the same document shape; only where the text comes from changes, so `from` replaces `document` on the existing pair (D26) |
| Scan the home directory, or a client's directory, for stores | It reads files the user did not name, grows with every client layout and makes a state list depend on what happens to exist; the fixed table is small and testable, and a request reads it only on demand (D26) |
