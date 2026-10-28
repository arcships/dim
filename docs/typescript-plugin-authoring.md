# TypeScript plugin authoring

This guide is the current TypeScript binding of [How Dim uses rutis](design/rutis-usage.md) and the
[plugin contract](design/plugin-contract.md) ([ADR 006](adr/006-plugin-contract.md)). Those documents
state the rules in any language; this guide shows how to write them today. Dim is unreleased, so
shapes change with the phase that needs them, together with this guide.

Dim loads trusted JavaScript/TypeScript plugins through rutis. Match `@arcships/rutis`
to the host's pinned runtime: main uses 0.17.1.
Use type-only imports from `dimcode/plugin` for Dim's implemented named services.
These declarations add no runtime wrapper or service.
The source declaration is [npm/plugin.d.ts](../npm/plugin.d.ts). The types of the services Dim's
Rust types serve to other languages are generated into
[npm/branch-services.d.ts](../npm/branch-services.d.ts) and re-exported through the same entry
point as `Services`.
The pinned rutis 0.16 declarations pass `tsc --strict` without `skipLibCheck`.
This does not disable checking a plugin's calls against the service interfaces.

## Available services

The [generated catalog](architecture/rust-agent/generated/catalog.md) lists every name with its group,
provider and the methods it serves to other languages. This table says which to use for what.

| Need | Service and scope | Availability |
| --- | --- | --- |
| Persist a process plugin's own data | `pluginFiles`, process | Implemented when the host supplies an authorized product root and provisions parent directories; use its native transactions and lifecycle scope. |
| Start or drive another Session | `sessions`, process | Implemented by Agents; all eight methods are typed. |
| Read current branch permissions or host defaults | `sessionPermissions`, process | Generated from Agents: `permission({sessionId, branchId})` and `permissionDefaults({})`. |
| Read or set a Session's title | `sessionMetadata`, process | Implemented by SessionStore: `read` and the revision-checked `setTitle` ([title metadata](design/appserver/session-title-metadata.md)). SessionTitle is the title's one writer; other plugins only read. |
| Read/update business configuration | `settings`, process | Owner registration, read/write and Settings event delivery implemented; a row registers its sources with `register` in its `apply`. |
| Read/save Workflow definition files | `workflowFiles`, process | Generated `read/list/replace/createDirectory`; fixed project directories and the calling row's read-only packages ([contract](design/p6-p7-bindings.md#3-workflow-definition-files)). |
| Offer model tools | `toolRegistry`, Session | A Session part registers each tool with `register` ([Session tools](#session-tools-and-client-methods)). |
| Offer client methods | `sessionMethods`, Session | A Session part registers each method namespace with `register` ([Session tools](#session-tools-and-client-methods)). |
| Offer host management methods | `managementMethods`, process | A process row registers each namespace with `register`; protected-local v3 Rpc serves it ([management methods](#process-owners-and-management-methods)). |
| Register Session and branch parts | `partRegistry`, process | `registerDefinition(scope, name, definition)` ([One row per plugin](#one-row-per-plugin)). |
| Know this Session and its phases | `sessionIdentity`, `sessionConfig`, `sessionLifecycle`, Session | `get()` returns `{sessionId, restored}` or the creation-time configuration; `activated()`, `closing()`, `hold()` and `release(id)` ([Session observation](#session-observation)). |
| Observe this Session | the `session/{sessionId}` channel | Listen with `ctx.on` ([Session observation](#session-observation)). |
| Consume branch events in order or act on branches | `dispatcher`, `branchLifecycle`, `timelineIdentity`, branch; `treeCommands`, Session | Use `Services` from the SDK. |
| Write events of your own domain | `timeline`, branch | Implemented ([extension events](#extension-events)); subscribe and read them through `dispatcher` like any event. |

See [Session TypeScript bindings](design/session-typescript.md) and
[Settings](design/appserver/settings-service.md) for their contracts. Do not depend on an API merely because a design describes it. A process
plugin uses `sessions` for other Sessions; it does not use that service to imitate
a Session part or hold a Session's private state.

## One row per plugin

A plugin is one loader row in `ServerConfig.session_typescript.rows`, and that row runs once, in the
process. Its Session and branch parts are not rows: from `apply`, the plugin registers each part
with `partRegistry` as a plugin definition, a `definePlugin` value, for the `session` or `branch`
scope. Every Session, or every branch, mounts one copy of each registered definition, the open ones
at once and later ones as they open. A copy runs in the plugin's Node runtime, and its `inject`
resolves in the instance it is mounted in. The host does not look at what a part injects or
provides to place it; the scope it is registered for decides.

```ts
import { definePlugin } from '@arcships/rutis'
import type { Services } from 'dimcode/plugin'

// A Session part: one copy per Session. It closes over the row's config.
const greeter = (greeting: string) => definePlugin({
  name: 'ExampleGreeter',
  inject: ['sessionIdentity'],
  async apply(ctx) {
    const { sessionId } = await ctx.use<Services['sessionIdentity']>('sessionIdentity').get()
    console.warn(`${greeting} ${sessionId}`)
  },
})

// A branch part: one copy per branch, providing a name in its branch.
const counter = definePlugin({
  name: 'ExampleCounter',
  inject: ['dispatcher'],
  provides: { exampleCounter: { get: 'sync' } },
  async apply(ctx) {
    let count = 0
    await ctx.use<Services['dispatcher']>('dispatcher').consume('example-counter', ['input/message'], delivery => {
      count += 'live' in delivery ? 1 : delivery.replay.filter(event => event.name === 'message').length
    })
    ctx.provide('exampleCounter', { get: () => ({ count }) })
  },
})

export default definePlugin({
  name: 'Example',
  inject: ['partRegistry'],
  async apply(ctx, config: { greeting: string }) {
    const parts = ctx.use<Services['partRegistry']>('partRegistry')
    await parts.registerDefinition('session', 'ExampleGreeter', greeter(config.greeting))
    await parts.registerDefinition('branch', 'ExampleCounter', counter)
  },
})
```

```json
[
  { "id": "example", "name": "/absolute/example/index.ts", "inject": ["partRegistry"],
    "provides": {}, "instanceNames": { "exampleCounter": "branch" },
    "config": { "greeting": "hello" } }
]
```

The registration belongs to the row. When the row unloads, whether it is reloaded, disabled or
fails, rutis ends every copy first, and the registration goes with it; the next generation
registers again. A definition may close over anything the plugin prepared in `apply`, such as its
config or a process object. It cannot skip an instance: a part that one Session's business
configuration turns off still gets a copy there, which reads that configuration and does no work.

Each copy reads plain names, and the framework resolves every name to the nearest enclosing
instance of the name's group ([How Dim uses rutis §3.5](design/rutis-usage.md#35-how-each-runtime-resolves-a-name)):
`timelineIdentity` in a branch part is that branch's, `sessionIdentity` in a Session or branch part
is the enclosing Session's, and `settings` is the process's. A part that names a service of a group
it is not registered inside cannot be built there: the instance mounts no copy of it, and the
host's log names the reason, such as
`"dispatcher" is a service inside "branch" instances: put the row in the "branch" group`. The
message is the loader's, written for rows; for a part, register it for the scope it names
(`registerDefinition('branch', …)` here), since the plugin's own row has no group.

Which names a part declares:

- **Provided for its instance:** list the name in the part's `provides`, and in the row's
  `instanceNames` with its group, `"session"` or `"branch"`. The catalog is fixed before the
  runtime starts, so this is the one thing the row declares about its parts
  ([How Dim uses rutis §2](design/rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)):
  the host registers each name in its group, every Session or branch has its own copy, and the other
  parts of that instance, in any language, read it by name. A name is declared once: one that is
  already a service, or that two rows list, fails the configuration.
- **Private to the plugin:** keep it in the plugin's own code; its definitions close over it. It
  needs no service name.
- **Provided without declaring it** (an undeclared `ctx.provide`, as an unmodified Cordis plugin
  does): rutis publishes it under its plain name for the whole process. Inside a Session or branch
  that is one process-wide service: a second copy fails with rutis's duplicate-provider error, and a
  debug build of the host refuses the Session at once (`scoped service provided without an instance
  key`).

One provider per name per instance: of two parts that provide the same name in one Session or
branch, the second fails with rutis's duplicate-provider error.

The host adds no check of its own beyond the configuration's shape. The row's `provides` must equal
the module's declaration, and its `inject`, if given, lists the names the module injects. A row has
no `group`. Python plugins take the same form with `define_plugin`
([How Dim uses rutis, appendix B](design/rutis-usage.md#appendix-b-typescript-and-python-binding)).

## Plugin shape and lifetime

`definePlugin` declares `inject`, `provides`, structural configuration schema and
`apply`. Declare every dependency in `inject`; obtain it with `ctx.use`. Each
provided method has the same `sync` or `async` kind in `provides` and its implementation.
`ctx.provide` makes the named object available for this plugin's lifetime.
`ctx.effect` registers cleanup; register it immediately after acquiring a resource.

Keep module top-level code to imports, constant metadata and plugin declarations.
Do no business I/O or asynchronous business initialization there, including
top-level `await` on a network request. Node evaluates a module to discover its
declaration before the plugin has an owned context or effects. One load that
never finishes can keep the shared `RuntimeRows` gate unpublished and cause
host startup to time out, even when native rows are ready. Put resource
acquisition and work in the owned lifecycle below.

During `apply`, acquire owned resources and register services, subscriptions and
cleanup. Background I/O starts after publication, as required by
[the architecture rules](architecture/rust-agent/principles.md). Branch Dispatcher handlers
can receive opening facts before publication; they follow the ordered-consumption contract below.
An injected dependency becoming available is not Session publication. Do not
start a timer or task from `apply` and assume that the Session is ready. A Session part injects
`sessionLifecycle` and a branch part `branchLifecycle`: background work waits for `activated()`,
which is `false` when the instance is never published, and ends when `closing()` resolves.
The process has no lifecycle service; a process row follows host start and shutdown through its
own `apply` and cleanup. The
rutis TypeScript `Context` has `use`, `provide`, `effect` and the bus: `ctx.on(name, listener)`
listens to every event of that name on the host's one bus, from any process or language, and
`ctx.emit` / `parallel` / `serial` / `waterfall` / `bail` emit on it. `emit` returns at once and does
not wait for listeners; `parallel` waits for all. Neither listening nor emitting needs a
declaration.

Write diagnostics to standard error (`console.warn`, `console.error`). The host writes each line
of the runtime's standard error to its process log, the App Server's `daemon.log`
([ADR 005](adr/005-observability.md)); standard output is discarded. Keep secrets and private
business data out of both.

State shared by one Session belongs to its Session part; a branch's work and
cancellation belong to that branch. A process part holds only state/resources
shared across Sessions or outliving them. Read other plugins only through their
declared public services/events. See [ADR 002](adr/002-plugin-scope.md), including
the distinction between loader structural configuration and business sources.

## Process owners and management methods

A process owner is a plugin's row in `SessionTypeScriptConfig.rows`. All rows
share the configured Node runtime and declaration catalog. Process scope describes ownership and
lifetime, not a dedicated operating-system process. This embedding input does
not select additional named runtimes or replicate workers.

A process row injects `managementMethods` and registers each of its namespaces from
`apply` with `register(namespace, declarations, call)`. The registration lasts until
the row unloads, and the same owner retains accepted calls through cleanup.
For example:

```ts
import { definePlugin } from '@arcships/rutis';
import type { ManagementMethods } from 'dimcode/plugin';

export default definePlugin({
  name: 'ExampleProcess',
  inject: ['managementMethods'],
  provides: {},
  async apply(ctx) {
    await ctx.use<ManagementMethods>('managementMethods').register(
      'example',
      [{ name: 'example.status', mutating: false, interruption: 'report', progress: false }],
      async call => ({ kind: 'value', value: { method: call.method, params: call.params } }),
    );
  },
});
```

Pass the module's `inject` and `provides` with the ordinary process loader row.
`declarations` is data, and `call` serves each call with the cancellation, progress
and replies of [Session client methods](#session-tools-and-client-methods). `register`
refuses a namespace or method name outside the grammar, a method outside its namespace
or declared twice, a namespace registered already, a built-in method name and the
reserved roots `hello`, `daemon`, `agent`, `approval` and `session`; an `apply` that
awaits a refusal fails its row. Management params are the client's original JSON; the
host adds no Session id. Unsupported replies fail explicitly. A client disconnect does
not cancel accepted work.

Optional process rows do not determine host startup or restart readiness. A namespace
no row has registered is unknown. Once its registration is withdrawn, the row that
registered it answers: `service_reloading` while it reloads or waits for a dependency,
`service_unavailable` once it failed or cannot run, and unknown when it is disabled.
`hello.features` lists the methods registered at that call. Required host
infrastructure and the shared declaration gate retain the startup deadline.
See the [process binding](design/process-typescript.md) for validation and lifetime.

## Process storage

Use `pluginFiles` for durable process-owned state rather than constructing a path
under the application home or importing native SQLite internals. The host chooses
the storage root. Paths passed to a scope are relative; namespace them by your
plugin's stable identity.

The embedding host must supply an authorized product root and create the
`notebook/` parent directory before enabling this example. Without a product root,
no `pluginFiles` provider is supplied and its dependent rows stay Pending. With a
root but no parent directory, `withLock` returns `io` before calling the callback;
it does not create directories.

```ts
import { definePlugin } from '@arcships/rutis';
import type { PluginFiles } from 'dimcode/plugin';

export default definePlugin({
  name: 'Notebook',
  inject: ['pluginFiles'],
  provides: { notebook: { read: 'async', replace: 'async' } },
  async apply(ctx) {
    const files = await ctx.use<PluginFiles>('pluginFiles').open();
    ctx.effect(() => files.close());
    ctx.provide('notebook', {
      async read() {
        return files.withLock('notebook/state.json', file => file.read());
      },
      async replace(text: string) {
        return files.withLock('notebook/state.json', file => file.replace(text));
      },
    });
  },
});
```

`withLock` holds the native file lease through its complete async callback. The
outer `FileResult` is the transaction result; a callback returning a `FileResult`
produces a nested result. Check each result rather than treating failure as empty
state. `read()` returns `null` only when the file is absent. `replace` atomically
writes and flushes the host's v1 envelope around your opaque text; keep your own
payload schema/version inside that text. Invalid, unversioned or unknown storage
records return `invalid_data` and preserve the original bytes.

Do not retain a lease after its callback. `backup` uses a fresh alphanumeric/hyphen
suffix and never overwrites a backup; a retained partial backup is not proof of a
successful copy. Quiesce stops new acquisition; close joins admitted work. An
optional cancellation Promise cancels acquisition when it settles. Settle it on
ordinary completion too, so it cannot hold runtime drain open. rutis does
not encode an `AbortSignal` as this argument.

Native storage locking and durability do not make remote side effects atomic.
After an uncertain external outcome, use that system's reconciliation contract
before retrying. Full semantics and fixed storage error codes are in
[trusted plugin storage](../crates/dim-app-server/README.md#trusted-plugin-storage).

## Business configuration

Declare `inject: ['settings']` and obtain `ctx.use<Settings>('settings')`.
Settings owns business documents; loader rows hold structural configuration.
Register each source from the plugin's process row, in its `apply`, with
`settings.register(declaration)`; a Session or branch part does not register.
The registration lasts as long as the row is loaded and is withdrawn when it
unloads (turned off, removed or reloaded); the stored document stays in the
home. Rows carry no `settings` member.

```ts
import { definePlugin } from '@arcships/rutis'
import type { PartRegistry, Settings, SourceDeclaration } from 'dimcode/plugin'

const settings: SourceDeclaration = {
  source: 'web',
  userFile: 'web.json',
  schema: {
    type: 'object',
    properties: { maxResults: { type: 'integer', minimum: 1, default: 5 } },
    additionalProperties: false,
  },
}

const part = definePlugin({
  name: 'WebSession',
  inject: ['settings'],
  async apply() {},
})

export default definePlugin({
  name: 'Web',
  inject: ['settings', 'partRegistry'],
  async apply(ctx) {
    await ctx.use<Settings>('settings').register(settings)
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'Web', part)
  },
})
```

A declaration requires `source`, `userFile` and `schema`; optional `projectFile`
adds project scope and `userOnly` lists protected member JSON Pointers. Unknown
fields, a malformed declaration, and a source name or file registered already
(Rust sources included) reject `register`, which fails the row's `apply`. Rust
sources such as `mcp` are not served by this named API. See the
[source registration contract](design/appserver/settings-service.md#31-source-registration)
and the working [owner fixture](../runtime/fixtures/settings-owner.ts) and its
[Settings declaration](../runtime/fixtures/settings-owner.settings.json).

`read` and `update` are asynchronous and return `SettingsResult<SettingsSnapshot>`.
`projects({})` is synchronous and returns the same envelope around
`{projects: [{root, cwd}]}`. Scope is `'user'` or `{project: root}`; root numbers
belong to this process and must not be persisted as project identities.

For a row declaring the Web source above:

```ts
import type { Settings } from 'dimcode/plugin';

declare const settings: Settings;
const current = await settings.read({ source: 'web', scope: 'user' });
if (!current.ok) throw new Error(current.error.code);
const updated = await settings.update({
  source: 'web', scope: 'user', expectedRevision: current.value.revision,
  document: { ...current.value.document, maxResults: 10 },
});
if (!updated.ok) throw new Error(updated.error.code);
```

An update compares the opaque revision and validates the complete document after
defaults. Owner writes cannot change user-only fields; local client
`settings.update` is the separate path allowed to change them. `conflict` requires
reading the current snapshot before deciding what to write. `unsynced` means the
write is already installed and announced; read before retrying. Dropping a call
cancels its wait, but does not undo a committed write. Settings emits
its events on the bus after commit. Listen to `settings/{source}` with
`ctx.on('settings/example', listener)` for `{source, scope, revision}`, and to
`settings/projects` for a newly authorized project's `{root, cwd}`. Register
listeners during `apply` and read the current document when needed; event payloads
contain no document. Events of one name from one process start in emit order. Reading at use time remains valid
for owners that do not need notifications. See the
[event contract](design/appserver/settings-service.md#44-events); no separate
subscription service or polling loop is needed.

Dim provides no credential service. A secret such as an API key is your
plugin's business: keep it as an ordinary field of your source, or store and
encrypt it yourself. A field the user fills in belongs in `userFile` (not
`projectFile`) and in `userOnly`; mark a credential `writeOnly: true` in its schema, so the
agent's `plugin config` never shows its value or writes it; state its rules in the schema (for example
`minLength`). Settings writes the user file with mode 0600, and local clients
can read the value back through `settings.read`, which never serves remote or
paired clients. Keep values out of tool results, terminal replies, history and
diagnostics. See
[settings-service §1.2](design/appserver/settings-service.md#12-decision-secrets-belong-to-their-owner).

## Driving Sessions

Declare `inject: ['sessions']` and use `ctx.use<Sessions>('sessions')`. Agents
provides the process directory service and looks up the current index on each
call. `launch` creates a non-interactive Session with permissions capped by the
starter's current policy, or the host's defaults when no starter is supplied.
The caller does not own or close the Session by dropping a handle.

| Method | Use and result |
| --- | --- |
| `launch` | Create a Session; reply includes its stable id and effective policy. |
| `input` | Write marked model-facing blocks and optional user content; reply confirms publication. The branch accepts each `submissionId` once: a resubmission writes nothing and replies the first receipt. |
| `lookup` | Read the input with a `submissionId` without writing. |
| `history` | Page local events through a fixed published prefix; carry `through` across pages. |
| `withdraw` | Withdraw a held input; `notHeld` means nothing was deleted. |
| `stop` | Interrupt the branch and wait for settlement. |
| `idle` | Wait for the first idle point; pending approval can keep it waiting. |
| `delete` | Close and delete the Session through its existing owner. |

Methods return `SessionsResult<T>`. Branch calls take both `sessionId` and
`branchId`; there is no implicit current branch. Input must include the caller's
source marker as its first raw block and a nonempty `submissionId`, conventionally
`<source>:<uuid>`, generated once per logical submission and stored before the call.
After a lost reply, `reloading` or `publication_unknown`, including in a new generation,
submit again with the same id; you get the first input's receipt.
`retryable` alone is not authority to repeat a side effect. A lost launch reply
can leave an empty Session: launch has no deduplication key.

`history` returns at most a requested page before extending to a batch boundary;
`limit` is 1–256. Use either `through` or `atLeast`, not both. Event sequence and
timestamp fields are JSON numbers; event payloads belong to their declaring
owners. The SDK types their envelope and JSON data without asserting that one
event payload describes every domain. For complete publication, paging, failure
and cleanup semantics, use the [sessions contract](design/appserver/sessions-service.md).

For permission reads, declare `inject: ['sessionPermissions']` and use
`ctx.use<SessionPermissions>('sessionPermissions')`. `permission({sessionId, branchId})`
acquires/restores the target and returns its dispatched `{version, policy}`;
`permissionDefaults({})` reads the host bound without creating a Session. Both
use Agents' stable directory and look up the current index per call. There is
no policy cache or fallback when the branch's Permission is unavailable.
Versions retain `number | bigint`. These generated methods return values
directly and throw `SessionsErrorThrown` with the owner's code, message and serde
data; they do not return the legacy `sessions` envelope. Neither read grants
permissions or makes a later Settings write or input atomic with that read.

Process rows using `workflowFiles` declare it in `inject` and use
`ctx.use<WorkflowFiles>('workflowFiles')`. Their structural `config.packages`
contains `{id, root}` entries; every root must exactly name a canonical absolute
root already authorized by the host in `ServerConfig.file_roots`. Package ids
are unique per row. Omitting the list means no installed packages. Only
Settings-authorized project cwd selectors are accepted. Generated methods return
Uint8Array, typed directory entries or void and throw `WorkflowFilesErrorThrown`,
without native root indices or caller context arguments. Mutating methods decode
package selectors but refuse them as `invalid_path` before I/O. Project enablement
and definition semantics remain with Settings and the Workflow package.

## Session tools and client methods

A Session part registers its tools with the Session's `toolRegistry` and its client method
namespaces with the Session's `sessionMethods`. The host configures plugin rows through
`ServerConfig.session_typescript`; general CLI installation remains PluginManager work. Give the
row the same `inject` and `provides` the module declares. Neither tools nor method namespaces are
in `provides`: a part registers each from its code. The plugin registers this part for `session`
([One row per plugin](#one-row-per-plugin)). The public
[embedding example](../crates/dim-app-server/examples/session_typescript.rs)
shows the row input and actual Session calls.

```ts
import { definePlugin } from '@arcships/rutis';
import type { SessionMethods, ToolRegistry } from 'dimcode/plugin';

export const sessionExample = definePlugin({
  name: 'SessionExample',
  inject: ['toolRegistry', 'sessionMethods'],
  provides: {},
  async apply(ctx) {
    await ctx.use<ToolRegistry>('toolRegistry').register(
      {
        name: 'example_echo', description: 'Echo supplied text',
        source: 'extension', risk: 'session',
        inputSchema: {
          type: 'object', properties: { text: { type: 'string' } },
          required: ['text'], additionalProperties: false,
        },
      },
      async call => {
        if (typeof call.arguments.text !== 'string') {
          return { content: 'text must be a string', isError: true };
        }
        return { content: call.arguments.text };
      },
    );

    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.example',
      [{ name: 'session.example.echo', mutating: false, interruption: 'report', progress: true }],
      async (call, _cancelled, progress) => {
        progress({ phase: 'started' });
        return { kind: 'value', value: call.params };
      },
    );
  },
});
```

Copy the row module's `inject` and `provides` into the existing host row during your package's
build. The App Server does not discover them or install a plugin by itself.
Online module metadata must match the configured row snapshot. Disabled, Pending
and malformed rows have the outcomes in the
[binding contract](design/session-typescript.md#host-input-and-declaration-discovery).
ToolRegistry declares a tool the Session mounts while a part has it registered.
`SessionConfig.tools` is the Session's optional mount list, fixed at creation: a
Session without one mounts every registered tool, so your tool is declared with
no configuration edit; a Session with one mounts the tools it names, and `[]`
mounts none. The registration lasts until the part's copy unloads, and a part
that registers later, or its next generation after a reload, is declared from
the next request without a Session reload. A named tool that no part registers is not
declared and does not fail the Session. Permission (its preset, `allowedTools`
and `disallowedTools`) decides whether a declared tool's call runs; it never
changes the declarations the model sees, so the prompt prefix stays stable.

`register(declaration, execute)` refuses a declaration whose name is not a valid
tool name, is a host tool or is registered already, and one without a Permission
risk. Permission recognises
`read_filesystem`, `write_filesystem`, `execute_process`, `network` and `session`.
Any other string (including `read`) is unclassified: without a tool-specific policy
override it is denied under read-only and requires approval under workspace-write. Use the exact category for the tool's
actual effects. `execute` receives the neutral
`ToolCall`: `sessionId`, `branchId`, `turnId`, `callId`, `arguments` and `config`.
This is distinct from the native `{id, name, input}` call. Validate model-supplied
arguments at the tool's external-input boundary. `config` is the creation-time
Session snapshot; provider-owned nested configuration remains JSON in the SDK.
The native serializer omits `interactive` when true, so an absent field means
interactive and only `false` is written; it omits `tools` when the Session has no
mount list. It is not the latest adopted Prompt/model
configuration after a configuration command.

The second `execute` argument is a cancellation Promise. It settles when the call
ends, is cancelled or its part unloads. Cooperative work releases its resources
before settling. Ordinary provider failures return a valid `ToolResult` with
`isError: true`; thrown exceptions are binding failures. Put every handle or
reference the model needs into `content`; `structuredContent` is for persistence,
UI and internal consumers. The SDK preserves the native result fields without
another conversion layer.

`sessionMethods.register(namespace, declarations, call)` takes the declarations as data: each
method declares its full name inside the namespace, `mutating`, `interruption` and
`progress`. It refuses a namespace or method name outside the grammar, a method
outside its namespace or declared twice, a namespace the Session has registered
already, one that is, contains or lies inside a built-in method, and one under the
`hello`, `daemon`, `agent` or `approval` roots; an `apply` that awaits a refusal
fails its part. The registration lasts until the part unloads, and `session.methods`
lists the methods registered at that call. `call` is asynchronous and receives an
opaque runtime `callId`, the method and original params containing `sessionId` and
optional `branchId`; it also receives cancellation and a per-call progress callback.
Return `{kind: 'value', value}`, return a failure as data, `{code, message}`, or
rethrow the service error you received. An omitted value becomes JSON null. Rpc maps a failure to
the wire error: `code` becomes `appError`, with JSON-RPC code -32602 for `invalid_params` and
-32000 for the others, and a rethrown native error (such as an `AgentErrorThrown` from
`treeCommands.input`) maps as it does for built-in methods. Do not choose JSON-RPC codes yourself; a
throw without a `code` is a binding failure. Handoff and connection-bound notifications are not provided by
this binding. On unload, C1 stops admission and settles accepted calls before the
JavaScript part unloads: `report` replies with interruption, while `drain` preserves
the provider's reply. No business task in this example runs during `apply`.

## Session observation

A Session part learns about its Session from three Session services, read by name like any other:

| Service | Contract |
| --- | --- |
| `sessionIdentity` | `get()` returns `{sessionId, restored}`; `restored` is true when the Session was loaded from storage rather than created. |
| `sessionConfig` | `get()` returns the creation-time configuration, the same snapshot a `ToolCall` carries as `config`. |
| `sessionLifecycle` | `activated()` resolves when the Session is published (`false` if it never is); `closing()` resolves when its close starts. `hold()` keeps an idle unload from ending accepted work: it returns the id of a hold the calling part owns, or `null` once the Session is closing or its release is decided. `release(id)` ends that hold and returns whether it was still held; the part's unload ends the holds it still has. |

A part needs no tool or client method to observe. This one listens to its Session's channel from
`apply` and stops when the Session starts closing:

```ts
import { definePlugin } from '@arcships/rutis';
import type { SessionBroadcast, Services } from 'dimcode/plugin';

export const observer = definePlugin({
  name: 'SessionObserver',
  provides: {},
  inject: ['sessionIdentity', 'sessionConfig', 'sessionLifecycle'],
  async apply(ctx) {
    const { sessionId } = await ctx.use<Services['sessionIdentity']>('sessionIdentity').get();
    const config = await ctx.use<Services['sessionConfig']>('sessionConfig').get();
    const lifecycle = ctx.use<Services['sessionLifecycle']>('sessionLifecycle');
    let closed = false;
    void lifecycle.closing().then(() => { closed = true; });
    ctx.effect(() => { closed = true; });
    if (config.interactive === false) return;
    ctx.on(`session/${sessionId}`, (record: SessionBroadcast) => {
      if (closed || record.kind !== 'event') return;
      console.info(record.branchId, record.event.type, record.event.name);
    });
  },
});
```

Its plugin registers it for `session`, as in [One row per plugin](#one-row-per-plugin).

`session/{sessionId}` is the Session's channel on the bus; only that Session's
records reach the listener. `SessionBroadcast` is `{kind: 'event', branchId, event}`
with the existing `AgentEvent` envelope, or a `notice`, `fence` or `closed` record
that belongs to its owner; an observer skips those. The immutable configuration uses the same
serialized form as ToolCall: absent `interactive` means true; eligibility that needs an interactive
leader checks `config.interactive !== false && config.collaborationRole === 'leader'`.
It does not report later Prompt/model adoption.

The example only logs transient observations. Handlers that start async work
register it with their own part and return; when `closing()` resolves, stop admission,
cancel and release resources, and join accepted work in an effect of the part. `ctx.on` removes
the listener on unload. Captured handlers must not start work after closing or after the part
unloads.

A branch event reaches the listener after its branch dispatches it. Records of one
Session start in emit order; nothing orders Sessions against each other, and nothing
is replayed: a listener registered later sees only later records. An `async` listener
does not hold back the next record, and a slow one does not slow native delivery.
Unload waits for running listeners up to the host's shutdown deadline. The
[real fixture](../runtime/fixtures/session-observer.ts) shows these boundaries.
Observation supplies no branch write or context-input path. Use the branch's `dispatcher` for
critical ordered branch consumption.

Since rutis 0.11, integer values beyond ±(2^53−1) cross into TypeScript as `bigint`,
including integers nested in model-supplied tool arguments or event data. Ordinary
Dim sequence and millisecond timestamp values remain within the safe-number range.
The SDK's `JsonValue` describes JSON-only values and does not model this bridge
`bigint` case; a declared `JsonValue` is not proof that incoming tool or event data can be
JSON-encoded unchanged.
`JSON.stringify` throws on `bigint`; do not stringify an unchecked argument object
or coerce every integer to `number`. At the tool's external-input boundary, validate
against its actual schema and explicitly choose a lossless representation when
sending such a value to a JSON-only API. Preserve the native value when passing
it to a compatible owner.

## Ordered branch consumption

Dim serves its Rust branch services to other languages by name. Import their generated types from
`dimcode/plugin`; no per-service adapter or branch setup call is needed. A part registered for
`branch` runs in each branch of its Session and reads that branch's `dispatcher`, `branchLifecycle`,
`timeline` and `timelineIdentity`. Keep Session tools, client methods and observers in Session
parts.

| Service | Contract |
| --- | --- |
| `dispatcher` | `consume(name, keys, handler)` registers an ordered consumer on the calling part's context: the handler receives `{ replay }`, the branch's own committed history through a fixed cutoff, then `{ live }` for each matching event after it, and the call resolves once the replay is handled. `listen(name, keys, handler)` registers for live events only. A handler's throw/rejection follows the native branch failure path. `committed/local/history` read committed history. `notify/sequence/changed` expose native dispatch progress and wakeups, not an ordered event subscription. |
| `treeCommands` | Session directory `list/info`, validated `input` by branch id and `create`. These are the Rust data methods, not the process `sessions` request envelopes. |
| `branchLifecycle` | Background work waits for `activated()` and releases its resources when `closing()` resolves. Never wait for activation inside a Dispatcher callback. |
| `timelineIdentity` | `get()` returns the original Session and branch ids. |

This part reconstructs its own input count, then follows live input. The native subscription
belongs to the part, so no second event channel or manual unsubscription is needed.

```ts
import { definePlugin } from '@arcships/rutis'
import type { BranchEvent, Services } from 'dimcode/plugin'

const provides = { exampleCounter: { get: 'sync' } } as const

export const counter = definePlugin({
  name: 'ExampleCounter',
  provides,
  inject: ['dispatcher'],
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    let count = 0
    let through = 0n
    const fold = (event: BranchEvent) => {
      if (event.type === 'input' && event.name === 'message') count++
      through = BigInt(event.sequence)
    }
    await dispatcher.consume('example-counter', ['input/message'], delivery => {
      if ('live' in delivery) fold(delivery.live)
      else for (const event of delivery.replay) fold(event)
    })
    ctx.provide('exampleCounter', { get: () => ({ count, through }) })
  },
})
```

Its plugin registers it for `branch` and lists `"exampleCounter": "branch"` in its row's
`instanceNames` ([One row per plugin](#one-row-per-plugin)).

`consume` subscribes, fixes the committed cutoff and hands the handler the replay: this branch's
own events through the cutoff as `local` reads them, without the ancestor prefix and not filtered
by the keys, since a rebuild may need facts it does not consume live. Live events after the cutoff
follow through the same handler in sequence order; none is delivered twice. Live events wait until
the replay call returns, so do what the replay leaves to do, such as ending the operations a
previous generation left open, in that call. A rebuild repeats no external side effect. Publish a
derived service after `consume` resolves. This example's state belongs to the branch part and is
rebuilt on each generation.

The hand-written form stays valid: subscribe with `listen` before sampling `committed()`, rebuild
from `local` through that fixed cutoff while the handler waits, and skip live events at or below
it. The [real Node part](../runtime/fixtures/branch-part.ts) is written that way.

Callbacks can run on opening facts before publication. Register long work in the part's owned
tasks and return; do not wait for dispatch, activation or a child creation whose progress depends
on this callback returning. `treeCommands.input` commits validated input to the Timeline tail;
its reply does not prove dispatch or adoption. `treeCommands.create(parent, at, options, cancelled)` takes a by-reference callback whose
Promise completes when the caller cancels. Complete it during the part's cleanup; the native call owns
and ends the wait when creation completes. Cancellation before acceptance stops admission;
after the creation anchor commits, TimelineTree finishes the branch or records `branch/create-failed`,
while cancellation stops the caller waiting. See [branch cancellation](design/branch-typescript.md#cancellation).

`BranchEvent` reuses the existing event envelope and retains `number | bigint` sequences and
owner-defined payloads. Keep sequence values lossless. Errors are thrown `AgentErrorThrown` values
with native variant codes; the code alone does not authorize retrying a write. The generated types
also retain native serde field names such as `operation_id` and `branch_point`, rather than the
process `sessions` service's envelopes.

Parts in the same branch can inject a provided branch service. A Session part depends on no
per-branch service: it writes input by branch id, the branch owner decides, and clients read its
results as Timeline facts. See the
[branch binding](design/branch-typescript.md) and [real Node part](../runtime/fixtures/branch-part.ts).
The services and signatures come from the Rust types: process services are marked
`#[rutis::service]`, the Session and branch services included. Regenerate the tracked
`npm/branch-services.d.ts` with `cargo run --locked -p xtask -- types`, so the Rust services and
packed SDK share one contract. `xtask check` fails when the committed SDK declarations do not
match the generator and compiles this example from the actual packed archive.

### Extension events

A branch part writes events of its own domain with `timeline.append({type, name, data})`, the
branch Timeline native capabilities use, and gets the committed event back with its sequence.
Pick a domain of your own, such as your plugin id; `type` and `name` must be non-empty without `/`
or `*`. The payload is your contract: the host stores and returns it unchanged and never validates
it.

The core's domains (`input`, `turn`, `step`, `context` and the others its events use) decode only
through their typed contracts, so a malformed event there is rejected. Which events a part writes
is otherwise the events' contract, as for native capabilities: core facts belong to their owners,
so ask them through their commands, such as input through `treeCommands.input`, rather than
appending their events. The branch's Timeline is closed by the branch itself, so `timeline` has no
`close`.

Extension events behave like any other branch event. `dispatcher.consume` with a key such as
`myplugin/*` delivers them in order, `local` and `history` return them, they are persisted and
restored with the branch, and a restored generation rebuilds from them. They never reach the
model's context; write an adopted input or return a tool result when the model needs content
([ADR 001](adr/001-harness-foundation.md#events-and-ownership-issue-2)). Existing v1-v3 client
feeds do not deliver them. History keeps them after your plugin is uninstalled.

```ts
import { definePlugin } from '@arcships/rutis'
import type { Services } from 'dimcode/plugin'

export const checkpoints = definePlugin({
  name: 'ExampleCheckpoints',
  provides: {},
  inject: ['dispatcher', 'timeline'],
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    const timeline = ctx.use<Services['timeline']>('timeline')
    // Rebuild from your own events in history; act only on live events.
    let steps = 0
    await dispatcher.consume('example-checkpoints', ['myplugin/*', 'turn/end'], async delivery => {
      if ('replay' in delivery) {
        for (const event of delivery.replay) if (event.type === 'myplugin') steps++
      } else if (delivery.live.type === 'myplugin') {
        steps++
      } else {
        await timeline.append({ type: 'myplugin', name: 'checkpoint', data: { step: steps + 1 } })
      }
    })
  },
})
```

See the [real Node part](../runtime/fixtures/extension-events.ts).

## Using services that other-language plugins provide

A service is a name, whatever language provides it. Inject the name and call it as you would a
service of another TypeScript part: a Rust part, a Python part and a TypeScript part in the same
Session provide and read one another's services by name, and every method returns a Promise in
TypeScript. The name resolves the same way for every language (see [One row per plugin](#one-row-per-plugin)):
in a branch part, a Session service is the enclosing Session's copy and a branch service that
branch's.

- **Provider in Rust:** a Rust service that crosses languages is in the host's catalog with its
  group. Its methods take and return serde data, and its errors are thrown as `RustError` values
  with the type's name and the variant's `code`. Its TypeScript type is in `Services` when Dim
  defines it; a service another Rust part defines has its own declarations.
- **Provider in Python or TypeScript:** the host registers the name in its group from the
  provider's row: its `instanceNames` for a Session or branch name, or its `provides` for a process
  name. Its methods and their `sync` or `async` kinds are what the provider declares; a failure it
  reports is its own contract.
- **Not available yet:** an injected name gates the part or row; it loads once a provider is there and
  unloads when the provider goes, in any language.

Python rows are named `py:<module>` and run in the host's Python runtime when one is configured;
the Python binding is in [How Dim uses rutis, appendix B](design/rutis-usage.md#appendix-b-typescript-and-python-binding).
An unmodified plugin for rutis's Cordis 4 (`@deepseek-ai/cordis`) can be registered as a part:
pass the plugin module, or its object or function, to `registerDefinition`, and its `inject` and
`ctx.get(name)` resolve to its own instance's copies.


## Packaging and trust

Keep the `dimcode/plugin` import type-only. Use a plugin's own build dependencies
to resolve it and bundle the rutis SDK/business dependencies, or include them in
that plugin's package. Module resolution must not rely on the host repository's
development installation. Pin the rutis SDK to the version the host supports.

`pluginFiles` is a trusted-plugin convention, not a sandbox: Node code can still
use `fs` directly. Dependency declarations do not isolate trusted in-process
code. OS isolation of model commands does not sandbox plugin packages, hooks or
MCP startup. Credentials and other private business data must stay out of model
content, terminal RPC, history and diagnostics. See [ADR 003](adr/003-approval-scope.md).
