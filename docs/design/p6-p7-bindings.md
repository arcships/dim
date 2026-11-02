# Scheduler and Workflow native access

Status: Accepted in [#256](https://github.com/arcships/dim/pull/256).
Read-only permission access (§2), native Workflow file access (§3.1) and native
input settlement (§4) are implemented. Applies to
[Scheduler #70](https://github.com/arcships/dim/issues/70) and
[Workflow #71](https://github.com/arcships/dim/issues/71). Readiness follows each
owner's design document; neither business package is implemented. This document
specifies repository-local bindings and their acceptance.

The run-store API and its [public composition](process-typescript.md#52-public-pluginfiles-binding)
use the existing native provider with the authorized product root. Actual Node
flows cover public process and Session-only storage, missing roots/parents and
scope drain. Workflow owns its report-specific evidence and remaining package
acceptance in [§3.4 and Appendix D](plugins/Workflow.md). Readiness follows each
owner's design; the storage binding does not implement either business package.

## 1. Problem and existing mechanisms

| Consumer | Required behavior | Existing mechanism | Missing repository work |
| --- | --- | --- | --- |
| Scheduler authorizing saves and reuse | Read the dispatched branch policy and the host default bound | `Session::get<PermissionState>`, native `session.permission.get`, Agents' Session defaults | A generated export of Agents' read-only `SessionPermissions` type |
| Workflow definitions | Read user, project and installed-package definitions; save project definitions within file authority | Settings owner reads/CAS and project authorization; `FileStorage` read/list/replace/create-directory | A native, row-scoped file binding restricted to definition directories |
| Workflow report recovery | Retry a report whose reply was lost without writing it twice | S2 input with the branch accepting each `submissionId` once | Retain admitted native input work through settlement; verify resubmission across row and runtime replacement in actual Node flows |

Creation configuration is not live Permission state. Published history is not a
replacement for the owner's dispatched state. `pluginFiles` is a versioned
private text store, not a project/package reader. An abandoned JavaScript Promise
does not prove that its native write ended.

The locked rutis 0.16.0 release already exposes
[`caller_row` and `CallerRow`](https://github.com/arcships/rutis/blob/v0.16.0/crates/rutis-bridge/src/session/services.rs).
`sessions` stays hand-written and does not capture its caller (§4). New services use generated exports, which capture
the row context for a Rust `&Ctx` parameter. An effect on that context belongs
to its original loader row. No rutis
upgrade, new lifecycle event, cancellation protocol or host-epoch service is
part of this binding.

## 2. Read-only permission access

Agents' stable directory provides one plain Rust `SessionPermissions(Ctx)` value.
Its two public asynchronous methods look up the current `AgentIndex` per call,
so consumers do not reload with that index. Export the type as
`sessionPermissions` through the existing generator (§5), rather than adding
methods to the hand-written `sessions` dispatcher. Each method takes one serde
request object and returns `std::result::Result<_, SessionsError>`:

| Method | Request | Value |
| --- | --- | --- |
| `permission` | `{sessionId, branchId}` | `{version, policy}`, with the same fields and meanings as native `session.permission.get` |
| `permissionDefaults` | `{}` | The current index's `ServerConfig.session_defaults.permission_policy`, as the existing `PermissionPolicy` |

`permission` returns the existing `PermissionState` with serde serialization
added at its owner; do not introduce a second version/policy data type.
`permission_defaults` returns the existing `PermissionPolicy`. Preserve the
generator's integer representation for `version` (`number | bigint` in TS).

Generated calls return the value directly. An `Err(SessionsError)` rejects with
`RustError`: `name` is `SessionsError`, `code` is the existing snake-case error
code, `message` is its Display text and `data` is its serde value. There is no
`ok` envelope or new `retryable` field. A read failure is handled by its code and
leaves the Scheduler save unchanged; it does not start an automatic retry.

To preserve those codes, make the existing `SessionsError` a Display-capable,
serde-tagged enum with `code` as the tag and the existing message payload. The
locked generator would give the current non-serializable struct the constant
code `SessionsError`; simply deriving Serialize on its multi-field shape also
does not select its `code` field. The owner error representation must therefore
support the generator. The existing `sessions` wire envelope and codes stay
unchanged at its boundary; do not add a second permission-error hierarchy.

`permission` acquires/restores the target through Agents, retains its existing
per-call Session lease, and reads `PermissionState` from that branch. It reads
no private field, reconstructs no policy and adds no watcher or cache. A missing
Permission throws code `reloading`; missing/closed Sessions and branches retain S2's
existing errors. `permissionDefaults` needs no Session. Both look up the current
index per call; an unavailable index throws code `reloading`. Neither mutates policy
or grants authority.

Scheduler declares `sessionPermissions` in its process module's `inject` and uses
`permission` for the saving caller and, when reusing an existing Session, the
recorded tool starter and reused Session's `main` branch. A launch supplies its
recorded starter to S2, which already reads that current bound. Management
`schedule.save` and reuse without a starter read `permissionDefaults`. Keep the accepted origin/cap replacement, policy
comparison, Settings CAS and pre-effect rechecks in
[Scheduler §3.3 and A.5](plugins/Scheduler.md#33-permission-of-a-run): a failed required
read leaves the job unchanged; a read is not atomic with CAS or input; a stale
CAS is returned without retry. Launch still applies its own current bound.
There is no dummy Session, omitted starter or creation-config fallback.

Workflow A17 continues to use the first actual launch reply's policy. These
methods do not move its capture point or add a new pre-launch policy check.
Full Access selection, CLI permission management and independent model approval
are separate product work; none is implemented by these reads.

## 3. Workflow definition files

### 3.1 Generated Workflow file access

The existing native Settings provider supplies one plain Rust `WorkflowFiles`
value: it already owns the authorized project map and depends on FileStorage.
Export that narrow type as `workflowFiles` through the generator (§5), without a
hand-written `HostDispatch`. The type selects authorized paths; FileStorage keeps
I/O ownership and is not exported. Add no plugin, root registry or Node filesystem
implementation. FileStorage operations stay unchanged; its existing `EntryKind`
gains lowercase serde serialization so directory listings reuse the native kind
without a second enum.

`root` below is a selector, `{project: canonicalCwd}` or `{package: packageId}`,
not a FileStorage index or arbitrary absolute path. There is no `root` lookup
method. Resolve projects only through Settings' current project map, and packages
only through the calling process row's trusted `packages` declaration (§3.2).
Each exported method takes `&Ctx`; the generator captures the calling row's
context synchronously and supplies it, without a TS argument or manual
`caller_row()` lookup. Calls outside a runtime row fail at that existing generated
boundary. Native assembly associates validated structural package options with
the original row's context; the type refuses missing/unconfigured package scope.
Call arguments cannot supply a row identity, add a package or grant a root.

The native row factory validates package options once for process consumers of
`workflowFiles`, against the host’s canonical FileStorage roots. Its immutable
package-to-index map is a private service on the original row’s instance; the
generated caller `&Ctx` reads that scope. A retired row’s cancelled context cannot
adopt its replacement’s scope. This adds no global root/row registry or new lock.

An invalid package configuration is refused by the native factory. During the
loader's initial metadata refresh, that refusal can leave the optional namespace
Inactive and answering `service_reloading`; it does not grant file authority.
This retains the existing inactive-owner limitation tracked by
[#124](https://github.com/arcships/dim/issues/124), without changing loader/Rpc
classification. Correct the structural configuration before restarting the owner.

Every call resolves its selector before I/O. The only visible project directory
is `<cwd>/.dimcode/workflows`; a package exposes only `<packageRoot>/workflows`,
read-only. `path` is one UTF-8 `.yaml` basename, without separators, `.` or `..`.
The four methods are asynchronous:

| Method | Request | Value |
| --- | --- | --- |
| `read` | `{root, path}` | Bytes, received as `Uint8Array` by TypeScript |
| `list` | `{root}` | Definition-directory entries `[{name, kind}]`, where `kind` is `file`, `directory` or `other` |
| `replace` | `{root: {project: canonicalCwd}, path, bytes}` | `void`, after existing atomic replacement and durability check |
| `createDirectory` | `{root: {project: canonicalCwd}}` | `void`, after ensuring `.dimcode`, then `.dimcode/workflows`, through existing non-recursive creates |

The plain Rust methods take serde request types. `read` returns
`std::result::Result<rutis_bridge::session::Bytes, WorkflowFilesError>`;
`list` returns `std::result::Result<Vec<WorkflowFileEntry>, WorkflowFilesError>`
(UTF-8 `name` and the `kind` above); `replace` and `create_directory` return
`std::result::Result<(), WorkflowFilesError>`. Their generated TS methods use the
table's shapes; Bytes becomes `Uint8Array` and `()` becomes `void`. `FilePath`,
root indices and `CancellationToken` stay private. Dropping a call follows the
existing generated future cancellation; it does not undo a committed mutation.

The native binding constructs fixed prefixes after checking the selector; it
accepts no home/database root index, absolute filename or replacement prefix.
Package mutations are refused. It cannot access user `workflow.json`, pluginFiles'
store, project Settings documents or `dimcode.sqlite`, even if their parent is
also an authorized FileStorage root. Directory creation ensures only the two
fixed project ancestors, without creating siblings or replacing non-directories.

These are new binding authorization checks, not existing FileStorage guarantees.
FileStorage has `register_root` and `root_path`, not a path-to-index lookup. Use
the project indices Settings already owns and package indices from trusted
assembly; expose neither root lookup nor registration. Reserved resources,
symlinks, generation validity and mutation locking remain FileStorage's checks.
Root registration stays with the trusted host. Selectors and internal indices
are not persisted definition identities.

Methods return values directly; `Err(WorkflowFilesError)` throws the generated
`RustError` with `name: WorkflowFilesError`, its enum's snake-case `code`, Display
`message` and serde `data`. Define the error as a serde-tagged enum, retaining
the native distinctions `invalid_path`, `closed`, `cancelled`, `locked`,
`unsupported`, `unsynced` and `io`, plus `not_found` and `invalid_data` at this
file boundary. Unknown/unauthorized selectors, package mutations and invalid
filenames throw `invalid_path` before I/O. Malformed request data, including a
raw numeric root, is refused by generated decoding before the method runs.
A name that cannot be represented as UTF-8 is `invalid_data`, never a lossy
alias. `unsynced` means a mutation may already be visible: do not retry it as an
unchanged write. `replace` keeps native permission behavior, with no chmod API.
`read` and `replace` reject definition files above 8 MiB as `invalid_data`. Reads use FileStorage's bounded read: check the opened handle's length before allocating contents and enforce the cap on each chunk to cover concurrent growth. An over-limit read returns no partial bytes; an over-limit replacement leaves the existing file untouched. This ceiling leaves room for base64 bytes and the JSON envelope within the existing 16 MiB bridge frame. Workflow checks serialized project-save bytes before issuing a binary call. Workflow's own decoded-definition admission (1 MiB compact JSON) and run-store payload (8 MiB) bounds apply after this port returns bytes; committed run snapshots keep their existing decoder. This port adds no chunking or shared transport change.

No deletion, arbitrary root registration, lock API or S10 envelope is exported.

Workflow declares `workflowFiles` in its process module's `inject`. Its provider is
the Settings generation it already injects, so this adds no upstream reload source
or FileStorage-to-Settings dependency cycle. Optional tools and native
Session readiness retain the accepted [process binding](process-typescript.md#51-p6p7-access-through-declared-feature-dependencies).

### 3.2 Source selection and authority

| Source | Discovery and identity | Writes and enablement |
| --- | --- | --- |
| User | Source `workflow`, user file `workflow.json`; entry key is the definition name | Workflow's existing typed Settings CAS |
| Project | Authorized cwd from `settings.projects`; `.dimcode/workflows/<name>.yaml`; basename is the name | `workflow.save source:project` uses FileStorage; user-local enablement is required separately |
| Plugin | Explicit installed package roots on the Workflow process row; `workflows/<name>.yaml`; identity is package id plus basename | Read-only to Workflow; installation/enablement comes from trusted host configuration |

The Settings document is
`{definitions: {[name]: Definition}, projectEnabled: {[canonicalCwd]: {[name]: boolean}}, max_concurrent_runs: number, max_concurrent_stages: number}`.
Its source schema declares the following fields. Settings applies defaults at
its existing external-data boundary; Workflow consumes the typed result directly.

| Field | Schema and default | Use |
| --- | --- | --- |
| `definitions` | Name-to-Definition object, default `{}`; definition fields follow Workflow A.1 | User definitions |
| `projectEnabled` | Canonical cwd → definition name → boolean, default `{}` | Protected local enablement; missing entry is disabled |
| `max_concurrent_runs` | Integer ≥ 1, default 4 | Workflow A11's process-wide admission bound, read at each start |
| `max_concurrent_stages` | Integer ≥ 1, default 4 | Workflow A11's per-run bound, captured at that run's start |

Both concurrency limits are ordinary fields of this document, never loader
options. Definition saves preserve them. A later source change applies to later
starts; it neither cancels accepted runs nor changes an existing run's stage
limit. Reload/restart interrupts those runs under Workflow A.6; it does not
resume them with newly read limits.

Declare `userOnly: ["/projectEnabled"]`, with no `projectFile`. Settings performs
structural validation at its existing boundary. Definition semantics stay with
Workflow at save/list/start under A.1. Owner writes preserve `projectEnabled`;
only the protected local Settings client changes it. Missing enablement means
disabled. A project file cannot enable itself, and edits to an enabled project
definition affect only later runs.

This is the user-source document shape, not an implicit
conversion of an existing name map. A nonconforming persisted source is invalid
and its bytes stay unchanged; no source or run-store migration is included.

Installed packages are structural row options
`config.packages: [{id, root}]`, where ids are unique and roots are canonical absolute
paths explicitly present in the host's FileStorage roots. This is package
placement, not a definition manifest or executable business content in loader
rows. PluginManager may populate these existing row options later; Workflow
does not depend on an unimplemented PluginManager or `PluginContents` API.
Package document `name` must match `<name>.yaml`, as for project files.

Keep the existing `project`, then `user`, then `plugin` source precedence and
the existing `source` selector. Names must be unique within a source category:
if two installed packages supply one name, list both package identities as
invalid and reject that plugin definition with `invalid_definition`; never pick
a package by directory order. Invalid selected definitions do not fall through
to another source. Lists put the name, category, package id where applicable,
and diagnostics in model-visible `content`.

Read each source at use time; the accepted run stores the definition snapshot.
Decode UTF-8/YAML and validate external structure once at the Workflow boundary.
Do not parse Settings-certified documents again. An absent project workflow
directory means no project definitions; other source failures surface as
`definition_source_failed`, not an empty catalog. An explicitly installed
package missing its workflow directory is such a failure. A missing selected
definition keeps `definition_not_found`. Project save uses `createDirectory` to
ensure the two fixed ancestors, without supplying parent paths; it never enables the
definition or overwrites a non-directory entry. Native project-file replacement
is atomic, with the current last-writer behavior; it adds no project CAS.

## 4. Native report settlement

### 4.1 One report per submission id

Use the existing S2 `input` operation and Workflow's inner-v2 report record. No
new report service or operation id is introduced. The destination branch accepts
each `submissionId` once ([sessions §6.2](appserver/sessions-service.md#62-input)):
a resubmission writes nothing and replies the first input's receipt, also while
that input is still unpublished. A report retry therefore needs no proof that an
earlier writer ended.

The S2 contract is in [sessions §6.2](appserver/sessions-service.md#62-input)
and [§8.3–8.4](appserver/sessions-service.md#83-agents-index-reload), with matching
[session-access A.6 / Appendix B](contracts/session-access.md#a6-errors) and Chinese
copies. For every caller, typed Rust or named service, the admitted task retains
the actual append/publication work and lease until its own outcome; index reload
cannot substitute an early `reloading` reply, and dropping a result does not end
it. `sessions` captures its calling row only to refuse input from a row that stopped
before admission (`reloading`, nothing written); it does not hold the row: a row
reloads without waiting for the inputs it admitted, and its next generation
resubmits. Request, reply, input
event and publication contracts are unchanged.

### 4.2 Recovery order

One configured Workflow process row owns its run store and report tasks. The
next generation starts recovery after the ordinary row lifecycle allows its
business work to start. It resubmits each pending report with its stored
`submissionId`, as [Workflow §3.4](plugins/Workflow.md#34-report-ownership-and-recovery)
requires; the reply settles the record. `ambiguous` (several inputs written before
branches accepted each id once) settles it delivered with a violation log.

No host epoch or row key is stored. The persisted file stays inner v2; no field,
version reuse or migration changes. `attempt_open` is still written but no
longer decides recovery; removing it needs an explicit format decision.
Workflow still owns record writes and waits for their success/error before
closing storage.

## 5. Implementation ownership and acceptance

| Owner | Repository-local implementation |
| --- | --- |
| Agents / S2 | Provide `SessionPermissions`; make the existing owner error exportable with stable codes; preserve S2's envelope; implement admitted `input` lifetime |
| Settings / FileStorage | Provide plain `WorkflowFiles` with row-authorized definition directories, serde requests/errors and Bytes; keep FileStorage internal |
| TypeScript assembly / SDK | List the two types in existing `build.rs` exports; commit generated declarations and SDK re-exports |
| Workflow / Scheduler | Concrete Settings/source/package rows and business flows; actual recovery evidence |

Use the existing `rutis_bridge::build::Exports` list in
[`crates/dim-app-server/build.rs`](../../crates/dim-app-server/build.rs):
`.service("sessionPermissions", "crate::agents::SessionPermissions")` and
`.service("workflowFiles", "crate::settings::WorkflowFiles")`.
`catalog.export_all(exports::all())` already installs that route. The owner
plugins provide the typed values; no hand-written dispatch or additional wiring
plugin is introduced. New exports use `&Ctx` wherever the original caller's row
is needed. The existing `sessions` service keeps its hand-written dispatcher under §4,
rather than changing the S2 service's binding contract.

Implementation regenerates [`npm/branch-services.d.ts`](../../npm/branch-services.d.ts)
through the same Exports list and re-exports its generated service/data types
from `dimcode/plugin`. Do not hand-write TS interfaces or commit declarations
for methods not implemented. Spell exported fallible returns as
`std::result::Result<T, ErrorType>`, following the current generator's alias
limitation documented in [C5](branch-typescript.md#open). The proposal adds no
rutis marker mechanism or dependency on a future export facility.

Architecture review belongs to eric8810; Agents/storage review to yyhhyyyyyy;
binding and plugin delivery to zerob13. These are requested repository-owner
reviews, not requests for rutis to add an API.

Delivery order: accept this contract, implement the native bindings with their
Node fixtures, confirm each plugin's remaining Ready choices, then implement
the business package. The binding fixtures demonstrate reads, source authority
and native I/O ownership without implementing a Scheduler/Workflow engine.
Complete save/list/start/report behavior remains the plugin implementation's
acceptance, after Ready. #126 can proceed once either consumer is Ready and
must land before repeated-Session shipping.

Required flow evidence, with disposable homes and the real Node binding (the
business assertions are completed with the subsequent plugin implementation):

1. Use generated `sessionPermissions` from a real Node row. Narrow a branch after
   creation; `permission` reads dispatched state, not config. Missing index or
   Permission rejects with `RustError`/`SessionsError` code `reloading`, not an
   `ok:false` envelope. Check the other owner codes and serde error data. Cover
   index/Permission reload, host defaults, save failure/CAS
   preservation and reuse of a Session above the job's effective policy.
2. Exercise all three source locations through a declared Settings/source
   fixture. Protected client enablement works, owner enablement fails. Direct
   generated `workflowFiles` calls return Uint8Array/list/void and throw the
   specified `WorkflowFilesError` codes without envelopes. They cannot access
   `workflow.json`, pluginFiles' store, `dimcode.sqlite`,
   project Settings files or mutate package YAML, including guessed home/database
   root indices, absolute paths and traversal. Another row's package id and an
   absent caller are refused. Only the two fixed project directories can be
   created, without access to siblings. Complete
   Workflow save/list/start verifies project files start disabled and package-name
   collisions, invalid YAML/UTF-8, absent project directory, denied traversal,
   no S10 wrapping, and source edits preserving an accepted snapshot.
3. Hold report input before admission, after admission before append and during
   publication. Index stop before admission returns `reloading` with no write.
   After admission, typed Rust and named calls return their own result across
   index reload. Dropping a Rust result does not cancel admitted input. A row
   reload does not wait for its admitted input; the next generation's
   resubmission, during and after the held write, replies the same receipt.
4. Crash Node while the old native report write is held. The replacement row
   resubmits; once the write is released it is answered with that input, and the
   branch holds one report. Crash between the marker write and native invocation:
   recovery writes the report once. Repeat over host restart.
5. Hold a store replacement during unload, including a failed replacement of
   the reply state. Storage closes only after admitted writes settle; recovery
   resubmits and does not duplicate a report.

Compile the packed SDK fixtures against regenerated types, using both permission
methods and all four file methods. Verify the export list and methods are present;
a generator warning that omits a required method does not satisfy acceptance.
Reuse the existing public SDK and Node flows for this evidence.

The generated permission export is covered by
`node_session_permissions_reads_live_policy_and_survives_index_reload` and the
packed SDK fixture: live branch updates, host defaults, owner errors and serde
error data, missing Permission during index reload, and unchanged consumer
generation. Existing S2 flows protect the legacy error envelope. Scheduler's
save/CAS/reuse business acceptance remains with its package implementation.

The file binding has a real Node source/authority flow,
`node_workflow_files_enforces_project_and_original_row_package_authority`, and
packed SDK checks for all four generated methods. It covers raw Bytes, directory
kinds, fixed-prefix writes and preserved modes, package read-only/row isolation,
host-root validation, traversal/numeric-root/symlink refusal, protected Settings
enablement and dependency reload. The non-UTF-8 name case is Linux-only and is not
covered by local macOS verification. Definition parsing, precedence, collisions and
accepted snapshots belong to the business package’s acceptance.

Native input flows cover a row reload during Session restoration, index reload
during publication, row reload before append, input from a cleanup during unload,
and a real Node crash while native append is held. A resubmission during and after
the held write replies the first receipt, and the branch holds one input. Rust
named-service calls retain admitted work when their result is dropped; a native
full-server flow also directly drops the typed Rust future after admission and
verifies its input still publishes once.

The native input tests hold the public TimelineStore I/O port and existing Session
restore assembly, with no production hook. Workflow records its report-specific
evidence and remaining real-package acceptance in [§3.4 and Appendix D](plugins/Workflow.md);
#234/#237 alone establish process placement and dependency behavior. Readiness
follows each owner's design document. #126 remains a shipping prerequisite after
Ready; #248/#255 optional-tool work is independent of these source and authority reads.

## 6. Rule check and ablation

| Rule | Answer |
| --- | --- |
| §4.1 | Policy/file reads change no owner's state; report input goes through S2 and InputQueue. |
| §4.2 | Accepted input is never recalled; interruption uses existing withdrawal/stop semantics. |
| §4.3 | No before callback, asynchronous commit hook or publication phase. |
| §4.4 | Native services know no Scheduler job or Workflow run/report marker; they expose reads and own admitted I/O. |
| §4.5 | No new event/batch; Workflow's existing file transaction precedes each guarded effect. |
| §4.6 | Definition text enters context only in existing adopted stage inputs or tool results. |
| §4.7 | PermissionState and S2 are read through their owners; Settings restricts file access to definition directories without exposing another owner's files. |
| §4.8 | All changes close in repository-owned bindings; the requirement table below gives owners and alternatives. |
| §4.9 | No new Timeline fact, host epoch, task log or persisted root/row handle. |
| §1 | No App Server scheduler/workflow engine; its changes are access and assembly. |
| ADR 002 D1 | Run state stays process-owned; optional tools remain Session parts. |
| ADR 002 D2 | Existing Agents and Settings providers supply ordinary typed services; rutis generates their exports, without a wiring/adapter plugin. |
| ADR 002 D3 | Native readiness depends on no optional package; the file binding shares the already-injected Settings owner. |
| ADR 002 D4 | Definitions/enablement remain business sources; installed package paths alone are structural options. |
| ADR 002 D5 | Rust serde types and public methods generate the named dispatch and TS contract; failures are generated RustError, Bytes is Uint8Array; no hand-written new binding. |

Ablation: keep two distinct policy reads to avoid an implicit fallback from a
missing branch to host authority. Four file methods serve read/list/save and
fixed-directory creation. Selectors replace raw root indices and an unnecessary
root lookup method; native checks enforce directory and write authority.
Do not export root registration, removal, chmod or generic leases. Two plain Rust
types reuse the existing export generator and its caller-context/error handling;
there is no separate permission/file wire envelope or dispatch implementation.
One immutable package scope on the original row is required for per-row file
authority; it is not a global root registry. Reuse the native EntryKind instead
of defining a second wire enum. Root equality with canonical host roots already
guarantees absolute paths, so no second absolute-path check is needed.
The admitted input task keeps native I/O alive across a dropped bridge result or
Rust future. Other S2 methods keep their existing cancellation behavior. The
branch accepting each `submissionId` once replaces a row drain, pending-call
registry, host epoch and report queue. Keep the existing transactions; the
attempt marker stays in the agreed format until a format decision removes it.

## 7. Requirements raised to mechanism owners

| Item | Problem, existing alternatives and requested change | Ordering / owner |
| --- | --- | --- |
| Public run-store binding | Process-typescript §5.2 supplies the existing provider at the authorized product root, preserving the API and FileStorage authority. Public Node storage acceptance is implemented; report-specific evidence and package acceptance follow Workflow §3.4/Appendix D. | App Server binding delivered; business acceptance remains under #70/#71 |
| Permission reads | Config/history do not provide dispatched Permission; launch-only capture does not meet Scheduler save/reuse. Agents supplies `SessionPermissions` through generated exports (§2). | Binding delivered by #259; Scheduler save/reuse acceptance remains under #70 |
| Workflow file export | Private `pluginFiles` cannot read raw project/package YAML; exposing native roots allows writes to other owners' state. Settings supplies generated `WorkflowFiles` scoped to project definitions and the calling row's read-only packages (§3). | Before P7 source acceptance; Settings/FileStorage and TS binding owners, #71 |
| Native input settlement | An early index-unload reply or dropped bridge result is not a writer-termination proof. Retain admitted native input; the destination branch accepts each `submissionId` once, so a retry resubmits; verify §5. | Before P7 report acceptance; Agents and TS binding owners, #71 |

No external capability request is established. If an actual acceptance flow
cannot be implemented with the locked APIs, record the reproduction, the
existing API that fails, required generic behavior, ordering effect and owner
in a Dim issue before proposing a rutis dependency. Do not turn untested
behavior into a request for a new protocol.
