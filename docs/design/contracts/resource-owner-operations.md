# Resource owner operations

Rust and foreign callers use the same owner operation. The native binding calls the owner
directly. The rutis binding dispatches its data arguments and returns its result.

## Interfaces already specified

| Dependency | Caller and contract |
| --- | --- |
| `hostPlatform` | The process service exposes its own data methods: `info`, `execShell`, `persistentShell`, `sandboxBackend`, `execRoot`, and the task operations `spawn`/`wait`/`kill`/`settle`/`output`/`retainOutput`/`tail`/`shell`/`write`/`resize`/`spawnWithStdin`/`spawnConfined`/`removeOutputDirectory`/`childPath`. `spawn` returns a task id; the process and output file stay with the owner. `spawn_tree`, `spawn_with_stdin_process`, `spawn_pty`, `build_process_spec` and `child_path_process` stay `#[service(skip)]`. Process handles remain with the service that owns them. |
| `fileStorage`, `mentionIndex` | File tools and workspace completion use [fsHost §4](../resource-faces.md#4-fshost), including mentions. Storage clients use the owner's file and lease operations. An opened reader uses openRead/readAt/closeRead. |
| `sessionStore` | Session repository, metadata, attachments, Timeline and worktree associations use store, store.readMetadata/setTitle, sessionFiles, timelineStore and store.associateWorktree respectively. Session construction binds the existing Session-scoped timelineStore. Path import and fileId reads use the [Session files contract](../appserver/session-files.md). |
| `pluginFiles` | Scheduler uses pluginFiles.open(). EventHub retains a caller-managed scope through settlement of admitted publishes across row restarts. Both use scope.withLock and lease.read/replace for the same atomic text update. Scope.list/remove and lease.readBytes/replaceBytes have their existing contracts. |
| `settingsService` | Declared sources use settings.register/read/update/projects. PluginManager uses the owner-write form for CAS edits and applies its existing writeOnly redaction. SettingsMethods retains the local client-write path. [Settings contract](../appserver/settings-service.md#42-service-settings-owner-path). |
| Permission, PromptSources, Skills catalog/management and execution/shell Settings → resource owners | Use the node facts and file operations in the [resource caller matrix](../resource-faces.md#6-provider-binding-and-the-owners-that-use-each-operation), including `hostPlatform.info` and fsHost resolution and reads. |

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

## Persistent Agents → sqliteStorage

| Caller in Agents | Repository API; IDs and records are data |
| --- | --- |
| Load the directory and Session ownership | agents({}) → AgentRecord[]; owners({}) → SessionOwner[]. Keep ordered results, invalid-profile rows, ownership errors and validation of the unique default. |
| Install/forget a persisted Agent | save({id,profile,default,owner?}) and delete({id}) → void. |
| Look up delegation and decide if removal is permitted | members({}) → ordered Session/leader pairs; ledMembers({leader}), ownedAgents({owner}), memberSessions({agent}) → ordered IDs; agentOwner({agent}) → owning Session ID or null. |
| Startup repair | unfinishedOwned({}) → {incomplete,orphaned}. Preserve the existing single query and the two distinct cleanup paths. |

## Native storage

SessionStore, persistent Agents, Provider and native Settings call the same process face, `sqliteStorage`.
The SQLite plugin owns reads, transactions and writer shutdown. Each consumer owns its
own unload. Session rows use `store`; volatile Agents and file-profile Settings use their existing backends.

### SessionStore → sqliteStorage

SessionStore publishes `store`, `sessionFiles` and the Session-scoped `timelineStore`.
Its native backend calls the existing process face `sqliteStorage` with data. SQLite
owns SQL, read connections, database transactions, writer shutdown and the database lock.
SessionStore owns Session admission, the ownership lock, file I/O, change notices and
draining its admitted work before unloading.

| Caller → owner | API | Transaction owner and boundary |
| --- | --- | --- |
| Agents, Session catalog, grants and worktree clients → store → sqliteStorage | Reuse the store request/reply shapes for `createOwnedSession, fork, owner, creationRecord, hasRecords, sessionConfig, sessionConfigText, catalog, addGrant, removeGrant, grants, setSessionLanguage, associateWorktree, worktreeDirectories, worktreeUsages, sessionsUnderWorkspace, removeWorktreeDirectory`. | SQLite creates the Session, main branch, ownership and optional worktree association in one transaction. Fork uses bounded copy transactions on its own connection; only the final transaction inserts the Session row and makes the fork visible. Association commits the directory and Session link together. |
| Session deletion and worktree removal → store | `store.delete` uses `sqliteStorage.hasRecords, removableOutputDirectories, touchWorktreeDirectories, rollbackSession`. `store.removeWorktree` uses the existing association/workspace checks and directory deletion; `store.removeUnregisteredDirectory` uses the existing directory-record and workspace checks and the caller's deletion. | SessionStore holds its ownership lock across the checks and the registration of the resolved path in an in-memory set, then releases it for the caller's deletion, including a recursive `--force` deletion with no time limit. Until the same removal task clears the registration, Session creation, fork, worktree association, directory-record deletion or another removal at, under or above that path are refused instead of waiting for it; dropping the caller stops neither the deletion nor the registration. SQLite owns each database step. File deletion and Git removal run outside SQL transactions and outside the ownership lock. |
| SessionTitle and Session rename → store → sqliteStorage | `readMetadata(sessionId)`, `setTitle(existing title request)`; existing metadata replies. FileAccess → `store.associateWorktree` with the existing association record. | SQLite checks the title revision and commits the write. SessionStore serializes title writes and publishes the committed notice before admitting the next one. |
| Timeline clients → timelineStore; SessionStore → sqliteStorage | Bind existing `createSession, rollbackSession, releaseSession, sessionConfig, branches, head, batchEnd, batchEndAt, readPage, readSegments, query, trace, saveSnapshot, latestSnapshot` requests at the native boundary. `commitTimelineRound({batches: Append[]}) → {sessions: [{sessionId, result}]}` accepts ordered batches grouped by Session. | SessionStore admits appends synchronously, groups the existing writer round, and serves commit subscriptions and release/drain. SQLite owns encoding, the grouped transaction, per-Session fallback on failure and writer memo settlement. One accepted round produces the existing ordered batch outcomes after commit. Release or unload drops generation-local reader/writer state after drain. |
| Session file put/import and read → sessionFiles → sqliteStorage | `reserveFile(sessionId,fileId,generation)`, `commitFile(sessionId,fileId,generation)`, `abandonFile(fileId,generation)`; ownership and path reads via `ownsFile, ownsOutput, ownedPaths`; `registerOutput(sessionId,path)`. | SessionStore reserves, writes or imports bytes through the file owner, then finalizes. SQLite verifies the Session and writer generation, clears the reservation and inserts ownership atomically. Bytes, path validation and fileId transfer stay with the file owner. |
| Session file sweep → sqliteStorage | `releaseStaleFileWriters(generation), unownedFiles(), deleteIfUnowned(fileId), recordedFiles(names)`. | SessionStore removes files outside SQLite. SQLite rechecks ownership when deleting the row. Failed cleanup remains retryable. |
| StorageMethods → sqliteStorage; native Transport → SQLite readiness | `storage.report` calls `sqliteStorage.storageReport({})`. Transport gates endpoint binding on the storage face. | SQLite owns the report and holds the database lock before readiness. Transport performs no storage read. |

Each batch reuses the timelineStore append fields `{sessionId,branchId,batchId,events: StoredEvent[]}`.
Return one outcome per distinct input Session. The result uses the existing completion
result type, `Result<(), CommitFailure {code,message}>`. SQLite preserves batch order within each Session, including when retrying
Sessions separately. SessionStore keeps the publication callbacks locally and settles each
batch exactly once with its Session's result, original key, batchId and final sequence.
An admitted round is drained on shutdown. Losing the reply does not authorize replay of the write.

### Settings → sqliteStorage

| Operation | Contract |
| --- | --- |
| readUser({source}) | Return null or {version,document}. The document is the stored text. Settings decodes it using the registered format. |
| importUserIfAbsent({source,document,importedRevision}) | In one transaction insert version 1 and provenance if absent, read the winning row and return it. Settings reads and validates the legacy file only after a miss. |
| replaceUser({source,expectedVersion,decision}) | Check the expected version first. For an accepted document, write it, increment the version and return the committed version. For a rejected protected-field change, return the existing rejection without writing. Missing row, conflict, overflow and storage failure keep their existing mappings. |

Settings decodes and validates the document. The writer checks the expected version before
applying the decision and enqueues the change notice in commit order on a successful replace.
First import emits no notice. File and project profiles retain their lease and notification paths.

### Provider → sqliteStorage

| Operation family | Current call sites and proposed inputs/results |
| --- | --- |
| readSelection(scope); writeSelection(scope,expectedVersion,selection) | selection/select. Return the stored selection and version; the write checks the scope version and commits the selected model atomically. |
| readPreferences(providerId,modelId) | saved_preferences. Read the one model preference field and keep the existing absent-value behavior in Provider. |
| readRecord({providerId,projection}) | catalog, catalog_for, catalog_target, resolve_connection, probe, discovery and edit preparation. Select header/version, one target model, managed catalog or full editable record as required. Return missing or the matching persisted projection. A projection containing an inline credential returns its stored value. |
| findCandidates(criteria); listProviders(filter); queryModels(page); managementPage(page) | Candidate IDs/ordered models, provider list, filtered model pages and management windows use their existing indexed selectors, limits, ordering and row-error behavior. Pass bundle identity, required catalog inputs and account facts as data. Keep transaction-local adoption and page selection together wherever the existing query does both. |
| activateSource(revision,baseFacts); adoptProvider(id,revision,baseFacts) | Existing source activation and per-provider adoption transactions. Supply only the bundle facts required by the existing operation. |
| commitEdit({providerId,edit}) | Closed edit variants: create, delete, connection/header patch, model create/edit/reset/delete, preferences, single/bulk enablement and discovery refresh. Preserve each existing transaction's touched columns, indexes, write precondition and projection. Create checks absence; versioned edits check the expected version. Refresh and connection changes that compare the prior record keep that exact comparison. Return the committed version and requested existing projection. |
| importLegacy(batch) | The existing legacy import commits its related records together and returns the existing item results. Cancellation and rollback keep the original transaction boundary. |

Provider validates edits and runs probe/discovery HTTP requests outside SQLite. A conditional
commit returns the original conflict on a stale snapshot.

**Record reads.** `readRecord({providerId,projection}) → PersistedProviderRecord | null`.
Use the existing persisted managed/static variants and the five SQLite model projections:
`header`, `all`, `mutable`, `one{modelId}`, `selected{modelIds}`. Header and target reads
retain their original query bounds. A version-only read uses the existing header version lookup.

**Conditional edits.** `commitEdit({providerId,edit})` selects one transaction by the edit tag.
Each variant carries its own precondition; callers cannot choose a weaker one.

| Edit | Input and precondition | Reply to Provider |
| --- | --- | --- |
| create / delete | Admitted create record and absence / expected version. Built-in deletion disables the record. | Created record / void |
| patchHeader | Expected version, existing ProviderPatch header fields, UpdateReplyKind (full or summary) | Committed projection for the requested reply |
| replaceConnection / refreshStatic | Prior complete static record and admitted replacement. Compare the exact prior record in the transaction. | Committed record |
| refreshManaged | Previous optional version, discovered account catalog and current account facts. Preserve account identity and optional-version checks. | Committed managed record |
| createModel / deleteModel / editModel | Expected version and ModelEntry / modelId / {modelId, edit: ModelEdit or null}. Null resets the edit. | Committed catalog projection |
| preferences / enableModel / enableModels | Expected version and {modelId, patch: PreferencesPatch} / {modelId, enabled} / {modelIds}. Retain bundled and account facts used by the existing mutation. | Management projection / version / catalog projection |

Discovery and credential admission run in Provider before the conditional write. SQLite executes
the existing mutation, including its index updates, in one transaction. Provider constructs the
existing business reply. `importLegacy` remains its existing separate bulk transaction.

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
