# Resource owner operations

Rust and foreign callers use the same owner operation. The native binding calls the owner
directly. The rutis binding dispatches its data arguments and returns its result.

## Interfaces already specified

| Dependency | Caller and contract |
| --- | --- |
| `hostPlatform` | Exec and Shell use the task and PTY operations in [execHost §3](../resource-faces.md#3-exechost). Hooks calls configured(request); OCR calls confined(request). Both return the result of that call. Process handles remain with the platform. |
| `fileStorage`, `mentionIndex` | File tools and workspace completion use [fsHost §4](../resource-faces.md#4-fshost), including mentions. Storage clients use the owner's file and lease operations. An opened reader uses openRead/readAt/closeRead. |
| `sessionStore` | Session repository, metadata, attachments, Timeline and worktree associations use store, store.readMetadata/setTitle, sessionFiles, timelineStore and store.associateWorktree respectively. Session construction binds the existing Session-scoped timelineStore. Path import and fileId reads use the [Session files contract](../appserver/session-files.md). |
| `pluginFiles` | Scheduler uses pluginFiles.open(). EventHub retains a caller-managed scope through settlement of admitted publishes across row restarts. Both use scope.withLock and lease.read/replace for the same atomic text update. Scope.list/remove and lease.readBytes/replaceBytes have their existing contracts. |
| `settingsService` | Declared sources use settings.register/read/update/projects. PluginManager uses the owner-write form for CAS edits and applies its existing writeOnly redaction. SettingsMethods retains the local client-write path. [Settings contract](../appserver/settings-service.md#42-service-settings-owner-path). |
| Permission, PromptSources, Skills catalog/management and execution/shell Settings → resource owners | Use the node facts and file operations in the [resource caller matrix](../resource-faces.md#6-provider-binding-and-the-owners-that-use-each-operation), including execHost.info and fsHost resolution and reads. |

## Additional public bindings

| Target and signature | Existing operation and consumer |
| --- | --- |
| FileScope.directory(name) → FileResult of node-absolute path | The file scope validates the scoped name, creates parents and returns the directory. Modality passes the node-local input filenames and work directory directly to its native adapter. OCR uses its current work/tmp/cache paths and grants. Existing exports use the recorded result filename. |
| FileScope.openRead(name) → FileResult of {readId,size} or null | The file scope opens a pinned file by relative name and returns None if missing. Bind that reader to the existing fsHost reader identity for the caller on the same node. Use fsHost.readAt/closeRead and the existing caller-unload cleanup. A scoped name never requires a caller-side root lookup. |
| FileScope.writeStream(name, next) → FileResult of null | The file scope consumes a reader to EOF under the destination lease. A rutis callback supplies bytes or EOF for the native reader parameter. Preserve atomic replacement and cleanup on failure or caller shutdown. The existing bounded whole-file replaceBytes remains available for small files. |
| settings.declaredBy({instanceId}) → SourceDeclaration[] | SettingsService.declared_by already selects live declarations by registering instance. PluginManager passes the loader's current instance ID. settings.sources lists declarations across owners and omits that identity. |

The OS path, pinned reader and input reader above use a path value, an existing reader ID
and a callback respectively. Existing local media submission/import, legacy node paths,
result settlement and result paging keep the [Modality contract](../plugins/Modality.md).

## Persistent Agents and native storage

Superseded by the owner contracts. The Agent records belong to [Agents and Collaboration](https://github.com/arcships/dim/pull/2098), the Settings document operations to [Settings](https://github.com/arcships/dim/pull/2094), the Provider record operations to [Provider](https://github.com/arcships/dim/pull/2096), and the Session repository operations to [SessionStore](../plugins/SessionStore.md). The engine's public face is the capacity read and the offline operations ([SqliteStorage](../plugins/SqliteStorage.md)); its transactions are the data owners' crate-private implementation, and no operation of those owners is published under `sqliteStorage`.

### Credential values in replies

Any trusted plugin in the same process-service scope can call these storage methods.
`sqliteStorage.readRecord` returns the stored fields selected by its projection,
including inline credentials when selected. A stored secret reference is returned as a
reference. Provider resolves it through its configured secret host and obtains managed
OAuth credentials from their existing owner.

`sqliteStorage.readUser` and `importUserIfAbsent` return the complete stored document,
including original values in `mcp-secrets`. `replaceUser` returns the committed version.
MCP reads its credential document through Settings and uses it to establish connections.
Provider and MCP business replies omit credentials. The business API `settings.read`
serves declared sources and excludes the native `mcp-secrets` document.

## Settings

| Caller → Settings | Current operation and contract |
| --- | --- |
| PluginManager; declared-source consumers | Filter declarations by live loader instance using declaredBy above. Reuse settings.read/update/projects for owner edits and reads. SettingsMethods uses its client path and lists only declared sources. |
| Agents create/restore, Hooks and MCP management | authorize_project(cwd,cancelled) → root registers the authorized root and publishes the existing notice. It is a write performed at trusted admission. project_root/path/sources are reads covered by settings.projects. |
| PermissionRules and CommandRules; CollaborationModelPolicy and optional Collaboration runtime | User permission and collaboration policy reads/writes. Expose the existing commandRules.read projection to rule consumers; use the policy owner's read returning {revision,policy} for Collaboration. Preserve absent-profile fallback. |
| HooksSettings and HookSources | hooks, hooks-projects and hooks-plugins. The runtime consumes sources(cwd,cancelled) → HookSourcesSnapshot. Source resolution, revisions and enablement belong to Hooks. |
| Mcp and McpPublication | mcp, mcp-projects, mcp-plugins and mcp-secrets. Publication reads effective server configuration and constructs live connections. Keep partial decoding of individual server entries and credential use at the MCP owner. |
| SkillsCatalog, Scheduler, Modality | Each registers and reads/updates its own native source. Keep the existing SkillDocument, ScheduleDocument and ModalitySettings behavior, including duplicate-job checks and validated model defaults. Their public management methods run at the feature owner. |
| Settings → sqliteStorage in the native user profile | Read the existing row; on first use import validated content and provenance atomically; replace at expected revision and publish after commit. File-backed and project settings keep their existing file path and lease behavior. |
| Sources using a declared schema → Settings | `settings.register/read/update`. |
| Native non-secret sources with custom decoding → Settings | `settings.registerDocument({source,userFile,projectFile?},codec) → {read,update}`. The source-bound operations are `read(scope) → {revision,document}` and `update(scope,expectedRevision,document) → {revision,document}`. Registration ends on owner unload. |

The source codec has two operations. `decode(bytes or null) → document` uses the source's
existing decoder for stored bytes, its default for absence, and validates the result.
`validate(document) → void` applies BusinessDocument write validation before admitting a
replacement. Settings performs serialization, version checks and persistence. Both calls
propagate the original Settings error and end with the registration generation. Foreign codec
calls occur outside SQLite transactions. Declared-schema sources reuse their existing codec.
