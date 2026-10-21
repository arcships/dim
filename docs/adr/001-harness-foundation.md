# 001: Extensible harness foundation

Superseded in part by [ADR 004](004-loader-plugin-management.md): MCP now uses
Session-owned instanced groups. Business documents retain source-local authority;
[ADR 002](002-plugin-scope.md) D4 reserves loader configuration for structural settings.

Decision status: Recovery design adopted for this branch. ADR 004 records the approved
configuration scope and its remaining integration work for issue #3. Dynamic events are decided
(issue #2, revised 2026-10-20): the model-context boundary, a structural write check (envelope and
reserved domains), Dispatcher filtering, descriptive event declarations and history of events whose
writer is gone. The
dependency-loss refinements remain Proposed for issue #6. This ADR does not supersede those issues'
decisions.

Implementation status: The recovery, configuration and common-assembly batch is implemented;
the broader foundation remains incomplete. The delivery table records implementation scope,
not maintainer approval or completion of the linked issues. Its acceptance column describes
the required behavior, not a claim that unimplemented units have passed verification.

## Context

Dim continues the extracted v4 harness as its own mainline. Its execution kernel already
has typed plugins, scoped services, committed Timeline facts, persistent Sessions and
generation-owned work. Extension events remain a closed Rust enum; general structural plugin
management and cross-language capabilities are incomplete. Business source authority is
separate under D4. Resolve each capability's contract and dependencies before extending it.

The goal is a complete extensible harness, not a replacement event bus or plugin framework.
Use rutis and its loader. The standalone Rust library remains usable without the App Server,
SQLite, Node, a TUI or a private account plugin.

## Decisions

### Events and ownership (issue #2)

Status: Decided. The model-context boundary and the history rule were decided on 2026-10-15. On
2026-10-20 the structural write check, Dispatcher filtering and descriptive event declarations
replaced the earlier proposal of runtime type registration, a registered-key write check and
declaration lifetimes.

Model-context boundary: events never enter model context directly. The model sees only adopted
inputs and tool results; a capability that needs content in context writes an adopted input with
the final content or returns it in a tool result.

Envelope: runtime mechanism facts, public capability protocol events and domain events share one
envelope and persistence path. The Rust `EventKind` enum is a typed view of the events the core
defines, not an admission list: an event whose type key it does not define is carried as opaque
data. Changes to shared protocol semantics require a decision record.

Write check: Timeline checks only the envelope structure and one structural key rule. It keeps
no type registry and does not validate the payload; the producer guarantees the payload against the
event type's contract ([time bus §2](../architecture/rust-agent/time-bus.md)).

Reserved domains: the domains (`type` values) that `EventKind` uses are reserved for the core's
typed events, and Timeline rejects an opaque append in a reserved domain. Restore decodes these
domains by their typed contract, so a core key with a foreign payload would make the branch's
history undecodable. Extension event types use their own domain. The core adds event types only
within its reserved domains; reserving a new domain requires a decision record, so a core release
never takes over a key an extension already writes.

Dispatch: a Dispatcher subscription is a delivery filter on type keys. It does not check whether a
key is declared or written by anyone; a filter that matches nothing receives nothing.

Event declarations: a capability's event declaration is a descriptive contract, the Timeline
counterpart of its service interface. It lists the event types the capability writes and the event
types its state and decisions depend on. Dependence covers every way the capability reads facts,
including history reads, so it is not derived from Dispatcher subscriptions. Declarations cover
public events ([principles §3](../architecture/rust-agent/principles.md)); private events need
none. The declaration lives with the capability's event contracts in its design page, not in rutis
or loader configuration. Assembly, dispatch and Timeline writes do not read it; review and flow
tests keep it consistent with behavior.

Keys: use stable type keys. Outside the reserved domains, namespaces express ownership
conventions, not authentication or a central approval process. Do not use loader-generated names or process-local instance IDs as type
keys. The capability that introduces an event type defines its data contract; any producer with the
appropriate public port and work ownership may write it.

History (Decided 2026-10-15): history keeps events whose writer is gone, unchanged. Reading or
importing history never installs code, grants permissions or resumes external work. Unknown
payloads remain readable as data and are never executed.

Event declarations add no lifetime of their own. Writes across a capability reload follow the
existing generation handover ([time bus §5](../architecture/rust-agent/time-bus.md)) for every
event alike.

Preserve the existing v1-v3 protocol contracts: they keep their allowlists and do not deliver
opaque events. Only a new dynamic protocol carries them, after host visibility filtering, without
requiring a server-side adapter for every extension. Visibility, authorization and sensitive-data
filtering remain host responsibilities, independent of compatibility conversion.
Do not require a universal event version counter: storage, snapshot interpretation and business
semantics have separate compatibility boundaries. Incompatible semantics require an explicit contract.

### Model context and recovery

The proposed event extension boundary preserves the existing adopted-input contract: domain events
do not independently project into model context. Capabilities deliver final runtime
content through adopted inputs or public tool results. The host interprets public protocol
facts, including assistant output, input consumption, tool results, adopted configuration and
compression. Optional UI rendering does not change what the model sees.

Historical interpretation needs supported host semantics and complete referenced content,
not execution of the original plugin. This does not guarantee that missing tools or Providers
can continue running. End persistent rules with explicit retirement facts, not historical edits.

Compression changes only the model-context view. It does not delete Timeline facts or replace
the replay input of other capabilities. A completed compression records its source prefix,
inclusive cut, summary and baseline. Preserve the suffix, changes after the source prefix,
and branch ancestry limits. A baseline cannot overwrite later rule changes or retirement.

Separate compression facts from optional recovery snapshots. A snapshot identifies committed
history and an interpreter compatibility contract readable before decoding its state. Incompatible
or absent snapshots can be bypassed; valid compression facts cannot. Replay reconstructs the
same context without generating another model summary. Unsupported facts or damaged required
content fail explicitly and remain intact. Snapshot candidates are supplied by storage; the
context interpreter decides compatibility. Exported facts and references must suffice without
snapshots. Independently verify full replay and accelerated replay agree at a fixed legal prefix.

The default interpreter v2 owns a paired encoder/decoder and an integrity envelope binding the
exact checkpoint payload to its anchor identity and interpreter. Reject damaged envelopes before
using their state; bypass unprotected v1 snapshots without migrating the database or rewriting
facts. This detects corruption after encoding, not malicious writers or semantic bugs in trusted
checkpoint builders. Builder equivalence is verified against full replay in tests; a known builder
defect requires a corrected compatibility version. Valid snapshots retain suffix-only reads.

### Configuration and lifecycle

Business configuration follows [ADR 002](002-plugin-scope.md) D4: MCP and Skill documents belong to
business sources, not the structural editable layer in issue #3. General scope
and acknowledgement requirements extend beyond these implemented paths; see
the delivery table and issue alignment below.

The loader owns structural plugin enablement and runtime assembly configuration.
Management services retain business routing, not independent generation ownership.
Distinguish user, workspace and Session scopes, source provenance and temporary overrides.
Each source has an explicit persistence authority and one write path; management edits name their
target source rather than writing back a merged view. Preserve shared workspace files as sources.
Import legacy product-home configuration before switching authority, retain originals on failure,
and do not automatically delete them. Scope row identities and configuration mappings so one
workspace cannot change another's instances. The loader owns structural runtime intent; its
single editable layer does not itself define multi-scope persistence. Providers are a selection
dimension, not different harness feature sets.

SQLite is the App Server's default configuration persistence implementation, with revision checks.
It is not a runtime dependency. Report accepted intent, persisted revision, applied revision,
runtime state and business settlement separately. Loader edits are not atomic transactions with
external effects; runtime convergence can precede a failed save. Notifications trigger state
refresh, not an assumption of durable success. Stable MCP identity survives process restarts;
connections and leases remain generation-owned.

The App Server validates and CAS-writes the explicitly named business source
through generic SettingsService BusinessDocument operations, then notifies
consumers. Failed persistence cannot change runtime intent; failed application
retains persisted intent. A mutation reply certifies persistence, not discovery,
model adoption or business settlement. MCP configuration types belong to MCP;
Skills implements its existing SkillSettingsHost. Full process assembly uses one
reconcile, without an editable-user bootstrap or configuration management namespace.

MCP generations are private immutable instances within each Session's loader
subtree. Bindings pin a superseded generation while its entry stays enabled;
disabling or removing the entry retires its generations even with bindings, and a
generation no lease uses closes. Disconnect drains every selected source/server generation
across affected Sessions and reports cleanup errors independently of persistence.
Ordinary Session closure stops only its own connections. No business document or
secret enters loader row options; there is no second lifecycle manager.

User MCP and Skill documents have one SQLite authority after a successful lazy import. Store
the document and import marker atomically, leave originals intact, and permit retry after failed
import. Workspace sources keep file authority and file CAS. The standalone Settings plugin can
still use its file backend without SQLite. The native database format follows the existing strict
compatibility policy; importing legacy settings files does not authorize migrating old databases.

The dependency-loss refinement is Proposed for issue #6: distinguish loader state, cleanup
completion and successful business settlement. Pending can occur before a plugin ever becomes
active, and rutis can publish Pending after reporting cleanup errors. Capabilities must own and
join their background work; retained service handles need generation checks where stale operations
would be unsafe. Do not infer successful terminal commits from loader state or retry unknown
external outcomes. Use existing rutis lifecycle hooks and the harness fault path.

### Trust and cross-language calls

> Superseded in part by [ADR 003](003-approval-scope.md): project MCP execution approval
> (grants, fingerprints, `mcp.approve`/`mcp.revoke`) is removed; project entries start disabled
> and the user enables them. The rest of this section stands. The approval paragraphs below
> record the superseded design; the code no longer implements them (see ADR 004).

Project MCP execution approval is implemented at the host boundary. It is not a complete
workspace sandbox or a cross-language Capability implementation. Authorizing a FileStorage root,
selecting FullAccess tool permissions or restoring a Session never grants project MCP execution.
Reading configuration is inert: unapproved project entries shadow user entries but cannot start
stdio processes or connect HTTP/SSE transports, including during automatic tool discovery.

MCP owns its host-local grants in `mcp-approvals.json`, format version 1, using
FileStorage leases and atomic replacement (0600 on Unix). Native and standalone hosts use the
same file contract; no native database version change or Session migration is required. The file
contains only hashed workspace/server identities and configuration fingerprints. Project files,
Timeline history and portable Session data cannot carry grants. Missing grants deny execution;
unreadable or corrupt stores fail closed without silently resetting existing records.

A grant binds a canonical workspace path, server ID and effective typed configuration. The
versioned fingerprint includes transport, command, arguments, environment overrides, resolved cwd,
URL, headers and authentication; it excludes enablement. Relative project stdio cwd resolves from
the workspace, never the daemon cwd. A disabled server remains disabled after approval. Changes to
executable configuration require a matching grant; identical configuration can reuse its grant.
User configuration remains a separate trusted local source and cannot confer approval on a
project entry with the same ID.

Protected-local protocol v3 management exposes `mcp.approve {cwd,id,approvalToken}` and
`mcp.revoke {cwd,id}`. `mcp.catalog` exposes `approval` (`notRequired`, `required`, `approved`) and
`approvalToken`, not executable configuration or credentials. The token is a configuration digest,
not a bearer credential or evidence of human consent. Approval compares the supplied token with
current intent, persists first, and re-reads before publication. An active Session may discover an
approved server immediately; callers must review configuration before approving. A stale token
returns `mcp_settings_conflict`. WebSocket callers cannot use these management methods.

The existing publication actor serializes host approval and generation changes; the loader still
owns disposal. Every project connection operation rechecks current source and grants before
admission. Revocation removes the persisted grant, rejects new calls through retained bindings,
cancels and joins accepted generation work, and closes transports before successful acknowledgement.
Cleanup failure is reported separately from persisted revocation; callers must inspect state after
interruption rather than assuming rollback. External file edits are observed on refresh or the
next connection operation, not by a filesystem watcher. Revocation settles this host's connections;
other independent hosts sharing the file observe it on their next read. Prior remote side effects
are not undone.

This approves configuration, not immutable executable bytes: modifying a referenced script or
binary, inherited environment, DNS or remote server behavior does not change the fingerprint.
Trusted native plugins, explicitly permitted shell tools and same-user processes can access host
files and are not sandboxed by this gate. Constrained plugin execution and OS isolation remain
separate work. Host safety settings do not belong to replayable business facts: replay must never
restore authority from a Session that once had it.

Configuration precedence never overrides host safety limits or workspace trust. Generic credential
references and resolution remain future work; exclude resolved secrets from management replies,
diagnostics, declarations and event history. Trust, authorization and OS isolation are distinct.
Native trusted code is not isolated by `Deps`; constrained callers require host checks of caller,
scope and work ownership. Shared language runtimes are not security sandboxes.

Rust and other languages use the same public business semantics. Cross-process operations carry
serializable data, explicit ownership and acknowledgement levels: accepted, committed, completed.
Establish correlation before publishing results. Caller abandonment is not rollback; cancellation
of unstarted work differs from settlement of accepted work and unknown external outcomes. Do not
automatically retry an operation whose side effects may already have happened.

Do not await cross-process code inside synchronous Timeline commit critical sections. External
preparation or review has an explicit operation lifetime. Validate the public event path with a
real Node capability as soon as registration and append are available, before hardening a Rust-only
API. Validate runtime loss, reload, cancellation and commit-before-reply disconnection.

## Delivery and acceptance

| Unit | Status | Acceptance |
| --- | --- | --- |
| Public contracts and contributor rules | Documented; proposed contracts still require issue alignment | Current and planned behavior remain distinct; no private source content |
| Recovery facts and snapshots | Implemented: immutable completion facts and separately persisted optional interpreter snapshots, introduced in native storage v12; current database format is v18 (S5a Session-file rows, S5b title metadata, #193 request message lists and #2 extension event rows, upgraded in place from v17, v16, v15 and v13) and default snapshot interpreter is v2 | Multi-compression, rule replacement/retirement, branch cuts, incompatible/missing snapshots; no new model calls |
| Production compression host | Not implemented: the real executable binds `ContextModelHost(None)` | Real model compaction uses the existing host contract and records replayable completion facts |
| Dynamic events | Implemented (#245): `EventKind::Extension`, reserved-domain rejection on decode, and the branch `Timeline` exported to TypeScript parts, whose `append` writes them. A client dynamic protocol is not implemented | Events whose type keys the core does not define, written by an external Rust capability, commit, persist, dispatch through key filters and survive restore; an opaque append in a reserved domain is rejected; v1-v3 do not deliver opaque events, and only a new dynamic protocol carries them after host visibility filtering; no registry or declaration is consulted |
| Cross-language public capability | Not implemented | Node append, subscription, cancellation, reload and process-loss flows |
| Business settings persistence | Original v13 plain settings_documents payloads, source-local CAS, lazy atomic import/provenance. See ADR 004. Structural editable plugin configuration, Session overrides and credential references are not implemented | Source isolation, save failure, protected-local business management, secret-free rows and diagnostics |
| Common execution assembly | Implemented: API-key and product profiles share public capabilities and protected-local management | Binary API-key flow, persistent settings across restart, model adoption, account isolation and transport restrictions |
| Loader-owned MCP instances | Implemented: Session-owned nested instanced groups with private immutable configuration, automatic replacement, explicit disconnect and cleanup receipts. Session closure stops only its connections; disconnect drains all affected Sessions | Bound tools survive replacement; disablement stops them; project entries run only after the user enables them; all generations drain; logical identities survive restart and root reordering |
| Project MCP enablement | Implemented per [ADR 003](003-approval-scope.md): no grants or approval methods; project entries start disabled and the user's local record enables them by workspace and id. General workspace sandboxing and constrained plugins are not implemented | Opening a workspace runs none of its servers; project files cannot enable entries; the record survives restart and fails closed when unreadable |

Keep this implementation batch on one integration branch with reviewable commits. The persistence
format, runtime assembly and lifecycle have one coordinated owner. Whole-plugin documentation
coverage and generated checks are not evidence of runtime correctness. Further work includes
compression hosts, broader workspace isolation, extensions to existing collaboration and permissions,
queue controls, questions, scoped rules, file history, external protocols, plugin management
and long-running/remote capabilities. These are separate delivery scopes, not additions to the
completed batch. TUI implementation and company services remain outside this work. Security
boundaries apply before exposing untrusted execution.

Each delivered unit must be usable on its declared parent and pass its relevant public-flow tests. Integrated
boundaries run workspace, lint, graph and packaging-script checks. Review P0 through P3 issues and remove
unnecessary machinery before committing. Storage compatibility follows actual format changes,
not a preallocated version shared across incompatible branch commits. Use disposable databases;
never overwrite a user's existing store to make a test pass.

## Existing issue ownership

- [#2](https://github.com/arcships/dim/issues/2) tracks dynamic events; its contracts are decided
  above. Committing the opaque envelope and append path as a public format still needs an explicit
  persistence-format decision.
- [#3](https://github.com/arcships/dim/issues/3) tracks loader configuration and MCP ownership;
  [#4](https://github.com/arcships/dim/issues/4) tracks cross-language capabilities. Validate the
  latter immediately after the public dynamic-event path is usable.
- [#5](https://github.com/arcships/dim/issues/5) tracks remote nodes;
  [#6](https://github.com/arcships/dim/issues/6) tracks typed-dependency questions and startup tests;
  [#7](https://github.com/arcships/dim/issues/7) tracks contributor documentation and graph checks.

### Maintainer alignment required

| Issue | Existing decision or open question | Branch position and merge requirement |
| --- | --- | --- |
| #2 | Events enter model context only through adopted inputs and tool results; Timeline checks only the envelope and rejects opaque appends in the domains `EventKind` reserves, with no type registry; Dispatcher subscriptions are key filters without validity checks; a capability's event declaration is a descriptive contract that no runtime path reads; history keeps events whose writer is gone (revised 2026-10-20, replacing runtime registration, the registered-key write check and declaration lifetimes) | Decided. Implementation carries event types the core does not define as opaque data |
| #3 | Structural plugin configuration uses the persisted editable layer and loader operations; ADR 002 D4 separates business sources | ADR 004 implements Session instanced groups and source-owned MCP/Skill documents with existing protected-local business methods. Generic structural plugin management remains unimplemented. Project files and the user's local MCP enablement record remain separate authorities |
| #6 | Dependency loss returning a plugin to Pending needs settlement semantics; optional dependencies remain open; the process startup deadline lacks an injection test | Proposed: separate loader state, cleanup completion and committed business settlement. This batch does not settle the optional-dependency question or add the missing startup test |

Source-owned business documents preserve shared workspace files and explicit edit
targets without using loader edits as persistence transactions. ADR 004 preserves
the original v13 plain payloads and uses private immutable Session instance
configuration. Issue #3's editable layer remains structural configuration work;
business persistence does not create applied-unsaved loader intent.

Recovery facts/snapshots and common API-key assembly are additional work, not completion of those
issues. The configuration implementation is available for review without settling dynamic
events, but does not complete #3. Plugin documentation and coverage checks address part of #7;
they do not resolve all design-document migration and issue-link work. Do not equate implemented
behavior with maintainer approval of the proposed issue changes.

## Alternatives and consequences

Requiring every extension event to be a variant of the closed enum would keep extension ownership
in the core. A universal executable projection language would create a second context-adoption
mechanism and make history depend on plugin interpreters. This proposal rejects both; issue #2
adopts the same model-context boundary (revised 2026-10-15).

Runtime type registration with a registered-key write check and persisted declarations was rejected
(2026-10-20). Readers must handle unknown keys anyway; an event type may have several legitimate
writers, so registration cannot check ownership; live registration cannot detect collisions with
history; and its lifecycle and storage would add a second management mechanism without changing
any capability's decisions. The core's existing `EventKind` events keep their wire form and need no
migration.

Stable public contracts require cross-crate work and explicit storage compatibility. Snapshots add
storage maintenance but remain disposable; immutable facts and reference closure remain authoritative.
The foundation must be verified through public flows, not inferred from a successful compile or
agreement between design documents.
