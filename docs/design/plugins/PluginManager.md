# PluginManager

Status: Draft. Requirements: [plugin management](../plugin-management.md) ([#432](https://github.com/arcships/dim/issues/432)). Issues: [#3](https://github.com/arcships/dim/issues/3), [#384](https://github.com/arcships/dim/issues/384). Scope: process and Session (the `plugin` tool); installation is per host. Language: Rust.

## Target contract

Scope: process, with a Session tool part. Issue: #2022.

The owners are `PluginManager` (process) and `PluginTools` (Session). `Owners` and the in-memory content refusals are private parts of `PluginManager`. The loader's rows are rutis-loader's state and are outside this contract; `UiExtensions` owns the UI content and is outside this contract. The feature's manifest, `plugins.json` format, default configuration and install/update/remove behavior stay as this document describes.

### Use and scope: responsibility split

`PluginManager` owns:

- **Package records.** `plugins.json`, the default configuration, the plugin directories, the installed name/version/source and the enabled/disabled state.
- **Package operations.** `plugin.list`, `plugin.install`, `plugin.enable`, `plugin.disable`, `plugin.reload`, `plugin.update`, `plugin.remove`.
- **Content registration.** The manifest's MCP, Hooks and Skills content is given to those owners; the UI content is given to `UiExtensions`. The refusal is a value from the content owner and is kept for the package.
- **Code rows.** The plugin code rows it creates, turns on, replaces and removes through rutis-loader; the loader is a separate owner and its data face is not part of this contract.
- **Runtime acquisition.** The Node and Python runtimes a plugin declares are requested from `RuntimeManager`; the runtime operations are pending #2019's contract; the package record does not own the runtime.

`PluginTools` owns:

- **The `plugin` tool.** It maps the model's command to one process operation and returns the tool result. It holds the process owner's operation or callback, not the `Manager` object.

Callers keep:

- The plugin package's manifest, files and lock; `PluginManager` installs and records them.
- The content owners' documents and refusal rules; `PluginManager` passes the manifest content and reports the refusal.
- The model's choice to use the `plugin` tool and the client's choice to install or enable a package.
- The loader's row state and reloads; rutis-loader owns them.

| Requirement (callers) | Compose from existing operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| List, install, update, remove, enable, disable or reload a package | No: the records, directory, lock and content registration are `PluginManager`'s state | `plugin.*` operations |
| Install a package's code and content atomically | No: the loader rows, package record and content registration must be settled together | the install/update/remove operations; the owner rolls back the package record when the content is refused |
| Read or change a package's content | No: the content belongs to the MCP, Hooks, Skills or UI owner | the content owners' operations; `PluginManager` only registers and withdraws |
| Run a plugin command from the model | No: the tool needs the process owner's operation, not a package record | the `plugin` tool; `PluginTools` calls the operation |
| Start the runtime a package declares | No: the runtime is another owner's resource | `RuntimeManager`; `PluginManager` records the requirement |

### Caller example

The local client installs and enables a package:

```rust
let listed = plugin.list().await?;
let installed = plugin.install(source, cancelled).await?; // progress is an owner value
plugin.enable(installed.name, cancelled).await?;
```

The model's tool:

```rust
let result = plugin_tool.call("enable", args, cancelled).await?;
// the owner records the change; the tool returns the owner's value
```

`PluginManager` registers content during install:

```rust
let refusal = mcp.register(ctx, plugin, dir, document).await?;
if let Some(reason) = refusal {
    rollback_package_record(plugin, reason);
}
```

### Own contract

#### Dependencies

`PluginManager` uses FileStorage's defined operations for a record document (#2080); `PluginFiles` (#2016), `RuntimeManager` (#2019) and HostPlatform (#2032) have no target contracts. Their operations are to be defined by their contracts; the #2022 requirement comments record the intent only; no interface is invented here. rutis-loader owns the code rows (out of scope); MCP/Hooks/Skills/UiExtensions own content; #2095 owns management methods. `PluginTools` uses `ToolRegistry` (#2026) and the process owner's operation or callback.

#### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `plugin.list` | The installed packages and their state | `{}` | bare array of package status maps `{id, name, enabled, locked, skipped, refused, restartRequired}` | None | `plugin_not_found`, `closed` |
| `plugin.install` | Install a local path or Git source | `{source, name?, lock?}` | status map `{id, name, enabled, locked, skipped, refused, restartRequired}` | Accepted install settles or rolls back | `plugin_exists`, `invalid_params`, `invalid_id`, `path_outside_plugins`, `load_failed`, `content_refused`, `restart_required`, `closed` |
| `plugin.enable` | Make one installed package the name's active package | `{name, version?}` | status map `{id, name, enabled, locked, skipped, refused, restartRequired}` | Accepted operation settles | `plugin_not_found`, `plugin_locked`, `load_failed`, `content_refused`, `restart_required`, `closed` |
| `plugin.disable` | Turn a package name off | `{name}` | status map `{id, name, enabled, locked, skipped, refused, restartRequired}` | Accepted operation settles | `plugin_not_found`, `plugin_locked`, `restart_required`, `closed` |
| `plugin.reload` | Reload a package's rows | `{name}` | status map `{id, name, enabled, locked, skipped, refused, restartRequired}` | Accepted operation settles | `plugin_not_found`, `plugin_locked`, `load_failed`, `content_refused`, `closed` |
| `plugin.update` | Update a package to a source revision | `{name, source?, lock?}` | status map `{id, name, enabled, locked, skipped, refused, restartRequired}` | Accepted update settles or rolls back | `plugin_not_found`, `plugin_locked`, `invalid_params`, `load_failed`, `content_refused`, `closed` |
| `plugin.remove` | Remove a package | `{name, version?}` | `{}` | Accepted remove settles or restores | `plugin_not_found`, `plugin_locked`, `load_failed`, `closed` |
| Startup reconcile | Remove directories and records that are not installed; restore the default configuration | internal | — | None | `record_read`, `record_write`, `cleanup` |
| Content registration | Register or withdraw a package's MCP, Hooks, Skills and UI content | internal | `refusal` | The owner's content call can be cancelled | the content owners' refusals are values |
| `plugin` tool | Map one model command to a process operation | `{command, args}` | tool result content | The tool call's cancel stops the operation | `usage`, the owner's errors |

#### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_source`, `invalid_manifest` | Fix the source or manifest; nothing was installed |
| `exists`, `missing` | Re-read the list; choose another name/version or install first |
| `load_failed`, `restart_required` | The package record is written but the rows did not load; report the required restart/reload |
| `content_refused` | The content owner refused; the package record is rolled back or the content is left out as the owner states |
| `write_failed` | The record or files could not settle; report the partial state and its recovery |
| `usage` | Fix the tool command |
| `plugin_not_found` | Re-read the list; the package is gone |
| `plugin_exists` | Choose another name/version |
| `plugin_locked` | Use another name or unlock the package |
| `invalid_params` | Fix the install/update parameters |
| `invalid_id`, `path_outside_plugins` | Fix the id or path; the path check failed |
| `closed` | The owner is unloading; use the replacement generation |

#### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Install/update | Fetch, validate, write the package directory, register content, write the record, load the rows | Accepted work settles or rolls back; no half record | The caller's row unload does not undo an accepted package | In-flight work settles; the next generation reads the records | Files and records by `PluginManager`; rows by rutis-loader; content by the owners |
| Enable/disable/reload | Write the record, replace or reload the loader rows, withdraw/register content | Accepted operation settles | The caller's row unload leaves the package state | New calls refused; the next generation reads the records | Record by `PluginManager`; rows by rutis-loader |
| Remove | Withdraw content, remove the rows, remove the directory, remove the record | Accepted remove settles or restores | The caller's row unload leaves the package state | In-flight removes settle | Content by the owners; rows by rutis-loader; files/record by `PluginManager` |
| `plugin` tool | Dispatch the command to the process operation and return its value | The tool call's cancel stops the operation | The Session row unload stops its tool | Pending calls end `closed` | Operation by `PluginManager`; tool result by `PluginTools` |

### Exceptions with reasons

- The loader's rows are rutis-loader's state; this contract does not define its data face. The loader is outside the plugin board's scope.
- `UiExtensions` owns the UI content; `PluginManager` only gives it the manifest content. It is outside this contract.
- `Owners` is a private object of `PluginManager`; its content refusals are values kept by the owner.
- `PluginTools` holds the process owner's operation or callback, not the `Manager` object. The Session part is registered by the process row and withdrawn with it; the operation is the shared lifecycle boundary.
- The `plugin.*` methods are management methods (#2095), not a second `pluginService` face.
- The package's code rows are loaded by rutis-loader; `PluginManager` does not hold a row handle. The loader returns row values/status.

### Client-visible changes

None. `plugin.*` and the `plugin` tool keep their commands, results and errors; the `Manager` handoff was a Session-part implementation detail.

### Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | `PluginManager` is one process part; `PluginTools` is a Session part. Neither folds Timeline state; they are not Capabilities. |
| §2 scopes | Package records and operations are process; the tool is Session. |
| §3 orthogonality | The content owners decide content; the loader decides rows; `PluginManager` composes their operations and owns the package record. |
| §4 rule 1 | No plugin command Timeline event. |
| §4 rule 2 | No handoff: the `Manager` object does not reach the Session; the content and row values are data. |
| §4 rule 3 | Installs run in the owner's tasks; the model tool runs in the Session's tool task. |
| §4 rule 4 | The vocabulary is packages, records, content refusals and rows; no client or model business appears. |
| §4 rule 5 | No batches. |
| §4 rule 6 | The tool result is the only text the model reads; the owner's install output is a value. |
| §4 rule 7 | Callers use `plugin.*`, the `plugin` tool and the content owners; no caller holds `Manager` or `Owners`. |
| §4 rule 8 | Requirements to #2016, #2019, #2026, #2027, #2032, #2095 are listed below. |
| §4 rule 9 | No Timeline fact. |
| §7 callbacks and ids | Package names/versions and progress values cross; the `Manager` object does not. The tool gets an operation/callback. |
| §7 one change | The `Manager` handoff and the public `PluginTools` field are removed in the same change that migrates the tool. |
| ADR 002 D1 | Process `PluginManager`; Session `PluginTools`. |
| ADR 002 D2 | No wrapper: `plugin.*` are management methods of the owner; no facade. |
| ADR 002 D3 | Extensions depend on the plugin manager; the loader and content owners are separate. |
| ADR 002 D4 | `plugins.json` and the default configuration are the owner's structural configuration. |
| ADR 002 D5 | Operations, values and ids are language-neutral; the Rust binding is the appendix. |

### Requirements raised to mechanism owners

- #2016: `PluginFiles` has no target contract; the requirement comment records the intent (package files and `plugins.json` record), not an interface; the interface is to be defined by #2016.
- #2019: `RuntimeManager` has no target contract; the requirement comment records the intent (runtime install/start/settle), not an interface.
- #2026: `ToolRegistry.register` must accept the `plugin` tool source with a process operation/callback.
- #2027: the `plugin` tool's call is an ordinary tool call; Permission decides it; no plugin-specific approval is added.
- #2032: HostPlatform has no target contract; the requirement comment records the intent (git source fetch with cancellation), not an interface; npm is RuntimeManager's.
- #2095: `ManagementMethods.register` must host the `plugin.*` namespace; the local client and the `dimcode plugin` commands use it.

### Binding appendix

`plugin.*` are management methods (#2095) on `PluginManager`; `PluginTools` registers the `plugin` tool through #2026. The `Manager` is `pub(crate)`; the Session tool holds an operation/callback.

```rust
tool_registry.register(
    ctx,
    "plugin",
    plugin_tool_spec(),
    move |call, cancelled| manager.call(call.command, call.args, cancelled),
);
```

`manager` is the process owner's operation/callback, not the `Manager` object. The current `PluginError` is a struct; the target replaces it with `#[derive(Debug, serde::Serialize, thiserror::Error)]` and `#[serde(tag = "code", rename_all = "snake_case")]`, one variant per error table code; a callback failure is `Rejected { reason, message }` with wire code `rejected`. `From<rutis::host::CallError>` is implemented where a callback failure must propagate. `CancelledFn` is the binding's cancellation argument, never a request field. The `PluginTools` `manager` field is deleted; the install/update/remove result values remain. The temporary-crate check with rustc 1.100.0 passed; every target error enum carries `#[error("...")]` for `Display`.


## Supersedes

Replaces the old PluginManager.md §0/§1/§3-§5/§8/§9/Appendix, the `PluginTools` `Manager` handoff and the content-registration interfaces; the `plugin.*` rows in [service-faces.md](../service-faces.md); the PluginManager rows in [resource-owner-operations.md](../contracts/resource-owner-operations.md) and [session-boundary.md](../contracts/session-boundary.md). Keeps the manifest, `plugins.json`, default configuration and command protocol. Rules: [§7 所有者与操作](https://github.com/arcships/dim/pull/2029). FileStorage is #2080; ManagementMethods is #2095; the Session tool registry is #2103; PluginFiles, RuntimeManager and HostPlatform are pending #2016/#2019/#2032.

## 2. Data
### 2.1 The plugin directory and its manifest

A plugin is a directory with `dim-plugin.json` at its root. The keys present say which kinds the plugin carries.

```json
{
  "name": "my-tool",
  "description": "Adds the echo tool",
  "code": {
    "entry": "index.ts",
    "inject": ["partRegistry", "settings"],
    "provides": {},
    "instanceNames": {},
    "config": {}
  },
  "skills": "skills",
  "mcp": "mcp.json",
  "hooks": "hooks.json"
}
```

| Key | Kind | Meaning |
| --- | --- | --- |
| `name` | — | The plugin's name. Plugins with the same name replace each other. |
| `version` | — | The plugin's own version, three dot-separated numbers, each one or more digits. Optional, and required for an optional plugin (§2.3), where it names the install directory. |
| `code` | code | The rutis row fields ([rutis usage §2](../rutis-usage.md#2-plugins-are-rows-instances-mount-their-parts)) except `id` and `name`: `entry` is a path inside the directory; `inject`, `provides` and `instanceNames` are copied into the row; `config` is the row's configuration, with the host's patch for that plugin name deep-merged over it (below). |
| `dependencies` | code | The package manager whose dependencies the plugin declares. The only value today is `"npm"`: the directory carries `package.json` and `package-lock.json`, and the locked dependencies are installed into the plugin's own directory at acquisition (§7 and the target install flow). A manifest declaring it without either file is refused `invalid_manifest`. |
| `skills` | skills | A directory inside the plugin of Skill documents, registered with SkillsCatalog. |
| `mcp` | MCP | A file inside the plugin holding MCP server entries, registered with Mcp. |
| `hooks` | hooks | A file inside the plugin holding a hooks document, registered with Hooks. |
| `ui` | UI | Optional client content declaration, registered with UiExtensions ([client UI contract](../client-ui-extensions.md)). |

Code, skills, MCP and hooks paths are relative, resolved inside the directory after following symbolic links; a path that leaves the directory is refused at install and at reload (`invalid_manifest`). UI validation follows its content contract below. The manifest is parsed at this boundary; owners receive documents already read. A plugin that declares `dependencies` runs its entry from its own directory, with its dependencies resolved there, as the [TypeScript authoring guide](../../typescript-plugin-authoring.md) requires.

**Row configuration.** A row's `config` is the manifest's `code.config` with the host's patch for that plugin name deep-merged over it: objects merge member by member, and any other value, arrays included, replaces the manifest's. A host sets a patch before startup; the product sets none, so a shipped plugin reads its manifest's value. A host that runs a candidate text puts the text at the member the plugin's author exposed, as the evaluation tooling does ([model-prompts §8.1](../model-prompts.md#81-the-rule)), so both arms run the same plugin code.

**Data directory.** The plugin files root is the root an embedding host names, else the product home, else the execution home the host was started with (`--home`). A host started with only a database file has none of them, so it has no plugin files root and runs no PluginManager; the root is never the database's directory or the working directory. When a plugin with code is turned on, PluginManager creates `<pluginFiles root>/<id>/` if it does not exist: `<id>` is the installed id, or a default plugin's name, both checked by the id rule (§7). It never deletes the directory, on turning off or on remove, as Settings documents stay in the home. An existing entry is used only if it is a directory, not a symbolic link, that resolves to itself inside the root; otherwise nothing is created and the plugin's file operations fail as they would without it. A plugin writes only under its own id directory through `pluginFiles` ([process TypeScript §5.2](../process-typescript.md#52-public-pluginfiles-binding)).

A plugin's business configuration (its Settings sources) is not in the manifest: the plugin's code registers its sources with Settings when it starts (the target requirements). The `settings` member of today's TypeScript rows goes away.

The repository's own TypeScript plugins (`runtime/plugins/*`) get a `dim-plugin.json` in place of their row files (`web.json`, `process.json`, `settings.json`). SessionTitle is a built-in plugin, not a TypeScript package.

`ui` is an optional manifest member owned by
[UiExtensions](../client-ui-extensions.md#附录-a-manifest-绑定). A UI-only package
has kind `ui` and needs no code row. PluginManager forwards the validated
content declaration and stable plugin name to UiExtensions; unsupported versions
and invalid UI declarations appear as content refusals without stopping the
package's backend code or other content. When UiExtensions is absent, `ui` is
reported as skipped. Withdrawals use the installed id, as for other content.

This behavior requires a Dim manifest reader that recognizes `ui`. Older readers
reject that unknown top-level member with `invalid_manifest`, so the whole
package is unreadable: its code and other content do not remain enabled. A
backend that must support older Dim needs a package version without `ui`, or a
separate UI-only package. Restoring a v1 installation record does not make a
new manifest readable by an old binary.


### 2.2 `plugins.json`

`<home>/plugins.json`, read and written only by PluginManager. It holds configuration only.

```json
{
  "version": 2,
  "installed": [
    {
      "id": "my-tool",
      "name": "my-tool",
      "source": { "path": "/Users/me/src/my-tool" },
      "dir": "/Users/me/src/my-tool",
      "kinds": ["code", "skills"]
    },
    {
      "id": "hooks-plus",
      "name": "Hooks",
      "source": { "git": "https://example.com/hooks-plus.git", "ref": "main", "commit": "4f1c…" },
      "dir": "<home>/plugins/hooks-plus/4f1c…+a1b2c3d4e5f60718",
      "kinds": ["code", "hooks"]
    }
  ],
  "names": {
    "my-tool": { "use": "my-tool" },
    "Hooks": { "use": "hooks-plus" },
    "web": { "off": true }
  }
}
```

| Field | Meaning |
| --- | --- |
| `installed[].id` | Unique among installed plugins; the key of the record and the name of the clone directory. Matches `[a-z0-9_-]{1,64}` (lowercase, so two ids never name one directory on a case-insensitive file system) and is not `builtin` or `default` (§7). When the caller gives none, the id is the manifest name lowercased (`Hooks` → `hooks`); a collision after lowercasing is `plugin_exists`, and the caller then gives another id. The example's `hooks-plus` was given by the caller (`--id hooks-plus`). |
| `installed[].name` | The manifest name at install or last reload. |
| `installed[].source` | `{path}` for a local directory, used in place and never copied; `{git, ref?, commit}` for Git; `{optional}` for a plugin the default configuration file ships optionally (§2.3), copied from its shipped directory. |
| `installed[].dir` | The directory that is loaded: the local path itself, the clone under `<home>/plugins/<id>/<commit>+<suffix>`, or the copy under `<home>/plugins/<id>/<version>+<suffix>` (§7 and the target install flow). A record written before suffixes existed names its directory without one; nothing is migrated. |
| `installed[].kinds` | The kinds the manifest carried at install or last reload. |
| `installed[].version` | The installed version of an `{optional}` source, from its manifest; absent for other sources. |
| `names.<name>.use` | Which plugin the name uses: an installed id, `"builtin"` or `"default"`. Absent: the built-in plugin of that name if there is one, else the default plugin of that name. |
| `names.<name>.off` | `true` when the user turned the name off. |

A name with no entry is in its shipped state: a built-in or default plugin on. Writing goes to a temporary file in the same directory, flushed on its own writable handle (`sync_all`, which also works on Windows), then renamed over `plugins.json`. A file that does not parse fails startup with its path; PluginManager never overwrites a file it could not read. The format is versioned; an unknown version fails startup.

A persisted `use: "default"` selects the same-name built-in when the distribution no longer
ships that default package. An existing default package remains the explicit selection while
it is shipped. This source transition does not rewrite the record; `off` and installed source
selections retain their meaning.

`plugins.json` is version 3 exactly when it holds an `{optional}` source or an
installed version; otherwise it is written as version 2, so removing the last
optional entry writes a record older binaries read. The version written follows
the content on every write. Reading accepts versions 1, 2 and 3: version 1
preserves every installation, choice and `off` flag, and a version 1 or 2 record
that holds an `{optional}` source or a `version` field is `record_unreadable`
("optional sources require record version 3"). Over a version 1 file the first
version 2 write keeps the exact version 1 bytes in `plugins.json.v1.backup`,
once: a record never returns to version 1, so an existing identical backup is
reused and a different or unreadable one refuses the write. Over a version 2
file every version 3 write replaces `plugins.json.v2.backup` with the exact
current version 2 bytes, through a temporary file synced on its own handle and
renamed over the backup, before the record itself is written: the record does
return to version 2 when the last optional entry is removed, so the backup
always holds the last version 2 record, the one a rollback to an older binary
wants, and a failed backup write refuses the record write.
Backup or record write failure leaves the original record in place. Unknown
versions fail without overwriting data. Older readers reject version 2 and
version 3; rollback requires restoring the saved record.

### 2.3 The default configuration file

Shipped with Dim, read-only, given to the App Server with `--defaults PATH` (the default CLI passes the file in its release; `./dev` stages this file and the plugins it lists into the checkout's ignored `staging/dev-plugins` and passes that copy, so the plugins resolve the runtime's single copy of `@deepseek-ai/cordis`).

```json
{
  "plugins": {
    "plugin-authoring": "plugins/plugin-authoring"
  },
  "optional": {
    "light-ocr": "plugins/light-ocr"
  },
  "locked": ["PluginManager", "Rpc", "Transport"]
}
```

- `plugins` maps each default plugin's name to its directory, relative to this file. A default plugin is a plugin directory like any other (§2.1).
- `optional` maps an optional plugin's name to its directory, relative to this file. An optional plugin is shipped but is not installed and not turned on: nothing reads it until the user installs it, and `plugin.list` shows it so a client can offer it (§3.6). `plugin.install {source: {optional: "<name>"}}` copies the directory — never `node_modules` — into `<home>/plugins/<id>/<version>+<suffix>` and installs its declared dependencies there (§7 and the target install flow); the shipped directory stays untouched and read-only. A newer release ships a new `version`: at the next start an installed copy is re-acquired in the background (§3.1), the old copy stays in use and on disk until that succeeds, and the next start after the switch deletes it. `enable` defaults to true, `plugin.enable` and `plugin.disable` then work as for any installed plugin, and an optional plugin's name records no `off` state until it is installed: `plugin.list` reports it off (§3.6).
- Built-in SessionTitle, Scheduler and Goal are installed by the native product profile and have no entry in this file.
- A name cannot appear in both `plugins` and `optional`, and an optional plugin's name cannot be a built-in plugin's: each is a startup error naming both. An optional plugin's item has no candidates (§3.6), so it would hide the plugin shipped under that name.
- `locked` lists names that cannot be turned off and whose plugin cannot be switched. Other plugins of a locked name may still be installed. Locking is product configuration; PluginManager applies it.


## Command protocol

- CLI: `dimcode plugin list | install <path|git-url> [--ref R] [--id ID] [--no-enable] | install --optional <name> [--id ID] [--no-enable] | enable <name> [--use ID|builtin|default] | disable <name> | reload <name> | update <id> | remove <id>`.
- Agent tool from `PluginTools`: one tool, `plugin`, with one string parameter `command`, written like the CLI's arguments: whitespace separates words, single or double quotes group one (a path with spaces is quoted), and a backslash is an ordinary character, so a Windows path is written as is:

  | Command | Does |
  | --- | --- |
  | `list`, `install …`, `enable …`, `disable <name>`, `reload <name>`, `update <id>`, `remove <id>` | The `plugin.*` method of the same name, with the CLI's arguments; a relative path is resolved against the Session's working directory. Same result and errors. |
  | `config <name>` | The declared (JSON Schema) Settings sources the plugin that `<name>` uses registered (R4); Rust typed sources, such as Mcp's, are not served here and are not listed: for each, its user document, revision, schema and user-only fields. A member the schema marks `writeOnly` (a secret, [settings-service §1.2](../appserver/settings-service.md#12-decision-secrets-belong-to-their-owner)) is left out of the document and listed in `writeOnlySet` when it holds a value. A plugin that is not running has registered none. Its client-side counterpart is `settings.sources` ([settings-service §4.3](../appserver/settings-service.md#43-management-methods-settings-client-path)), which lists the declarations only. |
  | `config <name> set <key> <value>` | Replaces one top-level member of a source through Settings: reads the user document, sets `<key>` (`<source>.<key>` when the plugin has several sources) to `<value>` (JSON, else a string) and updates with the revision it read. Settings checks the revision and schema; the write is an owner write, so a user-only field is refused, and a member that is or contains a `writeOnly` member is refused too. The owner reads its source when it needs it, so the value takes effect without a restart. Structural row configuration is not edited here. |
  | `help [command]` | The command list, or one command's usage and details. |

  The tool's description carries only the one-line command list; `help` gives the details, and a malformed command's error result carries that command's usage. The tool is a tool call: Permission decides each one ([requirement 5](../plugin-management.md), [ADR 003](../../adr/003-approval-scope.md)). Its declared risk is `persistent_configuration`; `list` and `help` are admitted as `read_filesystem` (R3); `config`, whose documents may hold secrets and user content, is admitted at the declared risk, as the sources' own tools are.
## 7. Security

Installing and turning on are host configuration done through management methods or tool calls; enabled plugin code runs without host-local approval ([ADR 003](../../adr/003-approval-scope.md)). PluginManager adds no scope, approval or trust step ([requirements](../plugin-management.md)). What it guards is the acquisition itself:

- Git sources are `https://` or `ssh://` URLs only; other schemes (`file://`, `ext::`, scp-style paths) and arguments starting with `-` are refused before Git runs. Clones run with submodules off (`--no-recurse-submodules`), hooks off (`core.hooksPath` set to an empty directory), `protocol.file.allow=never` and no terminal prompt.
- **Paths.** One rule covers every operation that touches the file system for a plugin: install (staging, clone, rename), update, remove and the startup cleanup. Before any file system change, PluginManager checks that the id, whether the caller gave it or it came from the manifest name, matches `[a-z0-9_-]{1,64}` and is not `builtin` or `default`, and that every directory it is about to create, rename or delete resolves to exactly one of `<home>/plugins/.staging/<random>`, `<home>/plugins/.python`, `<home>/plugins/<id>`, `<home>/plugins/<id>/<key>` or `<home>/plugins/<id>/<key>+<16 lowercase hex characters>` (`.staging` and `.python` cannot be ids, since an id has no dot). A `<key>` without a suffix is accepted only as a directory an existing record names and for the cleanup. The Python links in `.python` are replaced or removed as links; what they point to is never touched. A failed check refuses the operation with `invalid_id` or `path_outside_plugins` and changes nothing: no directory, no `plugins.json` write, no loader command. A local-path plugin's own directory is never created, renamed or deleted, so the rule does not apply to it.
- **No copy is deleted while the process runs.** Install, update, the background re-acquisition and remove leave every replaced directory in place; only the startup cleanup deletes a directory no installed entry refers to, and it runs before any acquisition in that start (§3.1). A task that read a description therefore names a directory that exists until the process exits, even when the plugin was updated or removed after it started. The cost is disk: replaced copies stay until the next start. A lease from Modality to PluginManager was rejected: Modality would have to address PluginManager's state, which [principles §4 rule 7](../../architecture/rust-agent/principles.md) forbids, and a crash would still leave the copy to the startup cleanup.
- A clone goes to `<home>/plugins/.staging/<random>` and is renamed into place only after its manifest and paths are checked. A failure deletes the staging directory; startup deletes leftovers and clones or copies no entry refers to.
- A plugin's dependencies come only from its own `package-lock.json`, installed by RuntimeManager in the staging directory with `npm ci --omit=dev --ignore-scripts --no-audit --no-fund --workspaces=false`, so no package lifecycle script runs and no dependency outside the lock is resolved. The registry is RuntimeManager's rule (`DIMCODE_NPM_REGISTRY`, then `npmRegistries`, the target requirements). The shipped optional directory is never installed in place and is never modified.
- No manifest path may leave the plugin directory after following symbolic links; a symbolic link inside a Git clone that points outside it is refused.
- `plugins.json` is written atomically (§2.2).

The acquisition and link-check code behind these rules is one crate-internal implementation, `crates/dim-app-server/src/plugin_manager/acquire.rs`, shared with the Skills installation ([Skills](Skills.md) §4), which reads a local directory or clones a repository for its own destination, staging directory and record. The rules above are stated once because there is one implementation.

## Superseded anchors

These headings are kept so inbound links resolve; the content is replaced by the target contract above.

### 2.4 State PluginManager does not keep
See the target contract's responsibility split.

### 2.5 Runtimes
See RuntimeManager (target, pending #2019).

### 3. Flows
See the target contract's flows.

### 3.1 Startup
See the target contract's startup reconcile.

### 3.2 Install
See `plugin.install` in the target contract.

## 4. Timing and conflicts
See the target contract's flows.

## 6. Interfaces (summary)
See the target contract's operations.

## 9. Requirements raised to mechanism owners
See the target contract's requirements.

### A.1 Methods (managementMethods, namespace plugin)
See `plugin.*` in the target contract.

## Appendix C. Acceptance (M1)
See the delivery comment's acceptance.
