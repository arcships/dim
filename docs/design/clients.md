# Clients

Status: Draft. This document is the overall view of Dim's clients: which clients exist, what they share, which capabilities each covers, and which gaps have no design yet. It does not restate lower-level contracts; each row links to the document that owns it. Overall design: [overview](overview.md); plan: [roadmap](roadmap.md) §3.7 (client lane).

Client work has so far been designed per feature. This document collects those designs, records where they disagree with each other or with the code, and lists the decisions needed before the client lane can be planned (§6).

## 1. Clients

Every client is a protocol v4 client of the App Server daemon. The daemon is headless and shared: one per product home, one executor with many viewers ([Tui §3.6](plugins/Tui.md#36-ui-registry-and-ui-leaves)). A client owns presentation, protocol adaptation and local interaction; the App Server owns execution, host configuration and persisted Sessions ([feature list](../feature-list.md)).

```mermaid
flowchart TB
  subgraph C["Clients"]
    CLI["Native CLI dimcode<br/>line, scrollback, fullscreen"]
    WEB["Web workspace<br/>phase 1, loopback entry"]
    DESK["Desktop<br/>own repository"]:::outside
    ACP["ACP<br/>implemented subset"]
    REM["Remote devices<br/>P3 draft"]:::planned
  end
  P["Protocol v3: the contract every client shares (§2)"]
  subgraph D["App Server daemon: one per product home, one executor, many viewers"]
    SM["Session methods<br/>dispatched through the Session"]
    MM["Management methods<br/>process scope, served per entry"]
    N["Broadcast notices<br/>not Timeline facts"]
    UI["UI registry<br/>terminal UIs are UI leaves"]
  end
  CLI --> P
  WEB --> P
  DESK --> P
  ACP --> P
  REM -.-> P
  P --> D
  classDef outside stroke-dasharray:4 3
  classDef planned stroke-dasharray:4 3,color:#888
```

Dashed boxes are outside this repository (Desktop) or not implemented (remote devices); dashed arrows are connections that do not exist yet.

| Client | Where | Form | Status | Design |
| --- | --- | --- | --- | --- |
| Native CLI `dimcode` | `crates/dim-cli`, `crates/dimcode` | Three interfaces in one executable: the line client (`--no-tui` or no terminal), the scrollback terminal UI (default) and the fullscreen terminal UI; `/tui` switches the two terminal UIs in-process | Implemented | [Tui](plugins/Tui.md), [CLI commands](cli-commands.md), [native CLI contract](../../crates/dim-cli/README.md) |
| Web workspace | `web/` (Vue 3), entry in `crates/dim-app-server/src/web_entry.rs` | Browser frontend over a loopback-only web entry, opened by `dimcode web` or `/web` | Phase 1 implemented | [Web workspace](web-workspace.md) |
| Desktop | Its own repository | Desktop application | Outside this repository | This repository owns only the App Server contract it uses (§5) |
| ACP | `crates/dim-acp` | reusable client-host rutis plugin; `dimcode acp` is the stdio entry, public RPC v3 to the shared daemon | Supported subset implemented, unreleased (#436, PR #441); acpx basic chat verified; Lody MCP GUI verification awaits login | [AcpGateway](plugins/AcpGateway.md), [acceptance](acp-gateway-acceptance.md) |
| Remote devices | — | Paired devices through RemoteGateway; absorbs the phase-1 web entry | Draft, P3, not implemented | [RemoteGateway](plugins/RemoteGateway.md) |

## 2. What clients share

| Contract | Owner document |
| --- | --- |
| Wire protocol v4: framing, `hello` and feature advertisement, notifications | `crates/dim-app-server/src/protocol.rs` (no prose reference exists, §4 item 2); [App Server README](../../crates/dim-app-server/README.md) |
| Session methods versus management methods, and which entry is served which | [Session boundary](contracts/session-boundary.md) (A.3 entries), [management methods](appserver/management-methods.md) |
| Reading a Session: `session.attach`, then `session.history` up to a fixed head, then live `session.event`, `session.state`, `session.model.progress`; clients fold these themselves. The attach reply carries that Session's `SessionInfo`, whose `model` follows the Session's latest adopted configuration, so a client that opens a known Session takes the model from it and reads no catalog; `session.summaries` items still report the creation model until [#795](https://github.com/arcships/dim/issues/795) lands. A reload leaves the title fields absent and `running` false, and the notices that follow correct them. The `session.create` and `session.attach` replies also carry `modelPreferences`, the preferences the Session's adopted configuration sends, derived from it and absent when it carries none; `session.summaries` items do not carry it | [Session access](contracts/session-access.md), [CLI commands](cli-commands.md); triage of [#276](https://github.com/arcships/dim/issues/276) |
| Broadcast notices that are not Timeline facts (`session.notice`, e.g. titles) | [Session notices](appserver/session-notices.md) |
| Approvals and questions: `approval.request` / `cancelled` / `resolved`; a question is a pending `ask_user` call answered by `session.question.respond` | [ADR 003](../adr/003-approval-scope.md), [Questions](plugins/Questions.md) |
| MCP App views: the document a tool declares (`session.mcp.app.widget`), a view's tool call (`session.mcp.app.call`), a view's message through `session.submit` | [MCP Apps](appserver/mcp-apps.md) |
| Interfaces viewing a Session: the UI registry, terminal UIs as UI leaves (`ui.open`, `ui.attach`, `ui.status`) | [Tui §3.6](plugins/Tui.md#36-ui-registry-and-ui-leaves) |
| Provider management for every client | [Provider management](appserver/provider-management.md) |
| Child tool batches: v4 envelope, v3 visibility and client grouping | [Child tool batches](appserver/child-tool-batches.md) |

The CLI and web client require protocol v4; the ACP gateway remains on v3 until its projection can group child batches. The CLI asks a daemon for its own version and accepts the highest one below it, down to v3, only to replace the daemon this executable runs or to stop the home's daemon; it never runs a Session below its own version. The client requires the `session.create` and `session.history` features; the web client and its assets require an exact protocol match ([web workspace §2.4](web-workspace.md)). The child-batch contract is specified in [child tool batches](appserver/child-tool-batches.md).

### Member tool approval

[Collaboration](subagent-tool-approval.md) owns member approval routing and its detail query.
Clients receive the leader's ordinary approval request for `member_approval_allow`, fetch the
server-owned original action with `session.collaboration.approval.get`, then answer that leader
request. No client merges member pending approvals into the leader's raw approval queue.

Every supporting client shows both the requesting leader branch and original member identity,
member branch, tool, complete arguments/target and original reason. It offers allow once or reject;
no remembered decision or policy escalation. Detail loading, failure or an unsupported query must
not enable allow. Collaboration's `memberApprovalEnded` Session notice retires the matching route prompt (and
clients query live status again after reconnect); the final server checks remain
authoritative. [CLI commands](cli-commands.md#subagent-tool-approval) owns terminal rendering.

Web applies this contract to the leader's normal approval card, even when a member detail is open;
it retains the full original arguments in an accessible detail view. ACP maps the trusted original
action into its permission request information and exposes only allow-once/reject-once options;
if the detail read fails, it shows the error with Reject only and waits for the user before
answering. An error during the final recheck also shows this rejection prompt. Ended requests
close without an allow. If its client cannot convey a permission prompt, it rejects rather than
silently approving an opaque id.
These are requirements for member-route support, separate from existing ordinary approvals.
Older clients can continue ordinary tools; rich member-approval review requires the new query and
corresponding client support. No frozen v1/v2 notification set or Approval envelope is extended.

### Branch directory and selected-line details

Clients discover identities and lifecycle state through the
[branch directory](timeline-branches.md#14-clients), without history reads.
CLI requests `session.branch.get` when a line is selected. Other lines remain
listed by identity until their details are known; a failed detail read leaves
the directory usable and reports the failure for that selection.

### Timeline block identity

- A text/thinking block is identified by the `id` of its **first raw event**. Following same-kind events belong to that block until a durable event of the other kind (thinking for text, text for thinking) or a tool event starts the next block. Provider `TextEnd`/`ThinkingEnd` are not durable and do not split a block. Replay, history and storage preserve the event ids.
- Tools use `call_id` within their model request; user inputs use the `input/message` event id.
- `session.model.progress` carries `textPartId` / `thinkingPartId` for the open blocks. They equal the first-event id that will publish that block.
- The open thinking block also carries `thinkingStartedAt`, the Unix-millisecond time of the event that opened it. The block's first durable `step/thinking` event has the same value as its `create_at`, which is the one event whose `create_at` is its fact's start rather than its commit time (#1330). A block ends at the next durable `step/text`, `step/tool-use-start`, `step/model-output-end`, or the turn end (`step/tool-use` also ends a block that had no start event): clients render the settled duration as that event's `create_at` minus the start, and can render the elapsed value while it runs from the same start, so live, history, replay and fork agree without a render-time clock.
- Clients must key timeline blocks/groups by that first-event id (or the request-scoped `call_id` for tools), not by local ordinals, request ids or synthetic `:progress` ids. UI-only group/expansion keys may derive from those stable ids.

### Opening a conversation with a bounded window

CLI main, line mode, branch side panels, member conversations and ACP load/resume
prefer `session.history.display {tail: true, limit: 64}` when the daemon advertises
`session.history.display.tail`. Older daemons keep the existing bounded `latestTurn` window.
The display tail has no whole-turn or atomic-batch expansion requirement. They show the first page
containing transcript rows, keeping that wire page and its fixed `through`.
If a page has no visible transcript rows, they continue backwards at the same head,
up to four pages total. They do not fetch older messages to fill a 100-message quota.
The capability name is `session.history.latestTurn`; it is not a separate RPC method.
The native TUI publishes the completed transcript window immediately. Owner inventories
continue at that same head; their loading, errors and guarded actions keep their own readiness.
A transcript ready for layout does not declare all controls ready.
An explicit earlier read uses 256 positions per page and stops at 100 transcript rows
or four pages. Web keeps its existing 256-position, 100-row, four-page window.
A transcript row is a user input, an assistant
response or a tool call that the client actually renders, not a text delta or a
hidden runtime input. Counting uses the projection's rules and stable identities.
After reading stops, the view applies a separate retention budget of 300 rows by
dropping older whole wire pages. It always keeps the newest page and, when any
transcript rows were found, the newest page containing them. These required pages
remain whole even when they exceed 300 rows. The retention budget does not cause
more pages to be read to reach 300 rows. The display tail may start or stop inside a committed batch; it never exposes uncommitted facts.
Legacy raw history and export retain their own atomic-batch contract.

The pages progress backwards; events within each page remain ascending. Clients
prepend each older page, then project the collected window once in event order. Live events received
during the read are held until the window is projected, then consumed above its
fixed head. An empty visible page does not end the read while its backward cursor
advances. `done` from `latestTurn` ends that turn window, not the branch's history.
JSON exec emits live wire notifications and has no historical transcript; it reads
one window page to pin the cursor without decoding historical event bodies.
Earlier transcript is loaded only on user request. Export keeps its full fixed
prefix. ACP reconnect replaces its internal projection with a fresh bounded window and
reloads current owner inventories; it does not accumulate the missed suffix. It emits
`_meta["dim/history"]` with `replace: true` on a configuration update so supporting
clients can replace their loaded window. Standard ACP has no history-reset operation;
the client's previously displayed messages remain client-owned. Web replaces each opened
transcript with a fresh bounded window. A lost submit acknowledgement is resolved by
submission id; reconnecting does not resubmit an input or recreate old execution state.
The line CLI's live idle/configuration settlement reads events after its current cursor;
this is execution-time catch-up, not cold history restoration.

```text
attach -> latest 64-position page -> draw -> editable
          | no visible row: at most 4 pages total
       -> owner inventories -> enable their own controls
/earlier -> 256-position page(s), at most 4 -> prepend older content
```

The client retains only the initial window, its oldest `nextBefore` and its fixed
head. It does not keep a second list of raw events after projection. A missing
prefix is displayed as “earlier content not loaded”; it does not turn a partial
reply into a complete reply. Tool results keep their call identity even when the
start is outside the window. A missing origin renders as a tool result with that identity;
[ToolExecution’s origin read](tool-origin-read.md) supplies complete arguments only when the user
requests tool details. Native committed tool blocks retain their existing three rendered
formats so previously displayed blocks can still switch detail after their source rows
leave the current window; this change does not reduce that rendering or retained-block cost.
Neither initial display nor owner-state readiness waits for origins. Neither an unresolved identity nor a
single very long turn removes the four-page initial-read bound.

A transcript window is not an inventory of running work. Before enabling controls,
clients must obtain the following state through public reads; they must not infer
absence from a missing start event in the window:

| State | Required read and merge |
| --- | --- |
| Adopted model and preferences | The attach reply; a `context/configured` in the window is historical display, not a substitute for the current adopted value. |
| Permission | `session.permission.get`; later policy notices update it. |
| Active turn and output | Current running/turn/request state and output block identities; a window without `turn/start` cannot prove the branch idle. |
| Pending questions | `session.question.pending` ([Questions §3.5](plugins/Questions.md#35-pending-question-read)); join later tool results by call identity. |
| Background execution | Task identities, statuses and display fields required by the task panel; a completed task in the transcript is historical, an open task remains controllable even if its start is older. |
| Delegations and branches | Current member/branch identities and unfinished delegations; read a selected member's transcript with its own window. |
| Pending inputs | Unconsumed Queue/Steer input identities, modes and display content, excluding deleted inputs. |

Ended branches remain directory entries but have no actionable inventories; clients read their
transcript and details only when selected. Configuration updates use the attach configuration
position independently of the transcript cursor, so a buffered configuration between attach and
the window head is still adopted.

The active-turn row and the last four rows raise requirements to InputQueue, Questions, BackgroundExec
and Collaboration, plus Loop for current execution state, through the App Server. Each read must identify the branch and
the committed cutoff its result describes. Clients fold held events above that
cutoff into that state separately from the transcript cutoff; an older transcript
event cannot overwrite a newer state. Lists must have a fixed cutoff and keyset
continuation, and are paged independently of the transcript. Questions require
`call_id`, source request/branch and complete question arguments; pending inputs
require input id, mode and user-visible content. Task/delegation results reuse the
owner's task/member identities and display-state types. A failed state read leaves
its controls unavailable and retryable; it is not an empty list.

These are requirements on the owning public contracts, not permission to read
private state or an assertion that the interfaces already exist. Their owner
must define the wire fields and register the public entry;
clients cannot access owner-private memory or add a second persistent ledger. A
window change is not complete until these cross-window cases pass. A server without
the window capability reports that limitation rather than silently triggering a
full-history initial read.


The changes apply to every entry, not just the main TUI:

| Consumer | Modification |
| --- | --- |
| CLI `session.rs` | Initial line-mode load uses the window. The existing forward reader remains for export and cursor-based catch-up. |
| CLI `session_host/tasks.rs` | Main and branch panel history use the same bounded initial-read rules; discovery of running tasks does not depend on those pages. |
| CLI `session_host/member.rs` | Each selected member pins its own head and loads its own window. Switching member releases the previous read's raw pages. |
| CLI `question/line.rs` | Questions on other branches come from the unanswered-question read, not a complete branch replay. |
| ACP `gateway.rs::read_history` | Load/resume returns only the window; model/configuration comes from attach and public current-state reads. A `forkAtTurn` lookup must resolve the requested terminal turn outside the window; full-history/export APIs keep their explicit full-prefix semantics. |

Acceptance opens histories differing by at least ten times with the same visible
tail. Native initial reading stops at the first page with visible transcript rows, after
four empty pages, or at the branch start. Explicit earlier reads and the Web window
retain the 100-row/four-page budget. After that stop, the
300-row retention budget drops only older whole pages, subject to the required
newest-page and visible-page exceptions above. It never causes another read.
Include a turn longer than the page budget, an empty visible page,
a question/task/delegation/input started before the window, a terminal event while
state is loading, member switching, reconnect and a transport-oversized batch.
Also verify that `/lines` lists a branch created before the window, line-mode
`/effort` uses the attach model, and an old ACP `forkAtTurn` anchor still resolves.
`/goal` reads `session.goal.get` before choosing a command and after its write; a successful Goal read must not
be discarded because the transcript failed, nor may a partial transcript decide
whether a goal exists. Retrying a transcript never changes other branches into
full-history readers. Tests exercise the window protocol itself, without a
legacy-history switch that silently restores the old production flow.

The separate state reads must also report their query costs; moving a full scan
behind an endpoint is not a performance acceptance result.

## 3. Capability coverage

Rows follow the sections of the [feature list](../feature-list.md), which describes target capabilities of `dimcode`; this table adds the other clients. Legend: **done** implemented; **partial** part implemented; **designed** design exists, not implemented; **none** neither designed nor implemented; **out** out of scope by decision; **—** not applicable or not planned for that client. Desktop is not tracked here (§5).

### 3.1 Providers and models

| Capability | CLI | Web | Source |
| --- | --- | --- | --- |
| Sign in, sign out (OAuth, every Provider the daemon describes) | done | — | [Provider accounts](cli-commands.md#provider-accounts) |
| API-key Providers, add/edit/test/enable/disable/switch | done; removal: none in the CLI (server supports it); model-list editing: `/models` → `Edit models` and `dimcode models PROVIDER` | done, including the model list and preferences (`provider.models.*`, `provider.preferences.update`) | [Provider management](appserver/provider-management.md) |
| Switch model | done | done | `session.model.update` |
| Reasoning effort, fast mode | done (`/effort`, `/fast`, and ←/→ in `/models`) | — | [Model configuration](model-configuration.md) |
| Subagent model policy | done (`/models` → Subagent models) | — | [Subagent model policy](subagent-model-policy.md), [#263](https://github.com/arcships/dim/issues/263) |
| Auxiliary models | designed server-side per owner (e.g. SessionTitle); no client UI | — | [AuxiliaryModel](appserver/auxiliary-model.md) |

### 3.2 Session management

| Capability | CLI | Web | Source |
| --- | --- | --- | --- |
| New, resume, Session list | done; `/resume` opens on the local scope — the launch workspace, or its repository's worktrees once git answers — and `Ctrl+S` toggles it with every workspace; two states, never three. It labels each row by creation origin and hides ACP Sessions unless the preference says otherwise; a Scheduler launch is a fourth origin class, listable through `session.catalog`'s `withAutomationSessions`, which `/resume` does not send yet | done (grouped by directory; it hides the internal and automation classes and has no ACP preference yet) | [Session classification](appserver/session-classification.md), [CLI commands](cli-commands.md#the-resume-picker) |
| Export conversation | done in the line client, both terminal UIs and `dimcode session export` | — | [CLI commands](cli-commands.md) |
| `/timeline` and message actions | done in the TUI; it also replaces `/history`'s paging | — | [Timeline UI §4.6](timeline-ui.md#46-timeline-jumping-to-a-message) |
| Titles | automatic: done (optional SessionTitle package); manual rename: designed | — | [SessionTitle](plugins/SessionTitle.md) |
| Fork | designed in the CLI (server `session.fork` done) | later phase | [Session deletion and fork](appserver/session-deletion-and-fork.md) |
| Deletion and cleanup | done: `dimcode session delete`/`cleanup`, and one row at a time in `/resume` | later phase | [Session deletion and fork](appserver/session-deletion-and-fork.md) |
| File rollback, file and conversation rollback | out | out | [overview](overview.md) App. A |
| Request snapshot export | done (`dimcode export`); filters: designed | — | [Timeline export](appserver/timeline-export.md) |

### 3.3 In-session interaction

| Capability | CLI | Web | Source |
| --- | --- | --- | --- |
| Send, queue (`Enter` while running), automatic sending, steer (`Ctrl+S`, or `Enter` on an empty draft), interrupt | done | send and stop: done; queue: later phase | InputQueue (S1) |
| Lines: tree, switch, create, send, stop, approvals, end | done | done (right-side Lines panel) | [Timeline branches](timeline-branches.md) §14, [Web workspace](web-workspace.md#41-lines-and-branch) |
| `/queue` panel and queue deletion | done in both terminal UIs | — | [CLI commands](cli-commands.md#queued-messages) |
| Retrieve a queued message for editing | done; images and multi-block messages remain queued with a notice | — | [CLI commands](cli-commands.md#queued-messages) |
| Multiline, paste, large paste, images, mixed attachments, `@` files, `!` / `!!` | done | attachments: later phase | [CLI attachment input](cli-attachment-input.md), [ShellInput](plugins/ShellInput.md) |
| Skill references: the `/` palette lists them, and `$skill: <name>` loads one with the message | done | none | [Skills](plugins/Skills.md#9-references-load-with-the-operators-input) |
| Clipboard text, images, files; selection copy; OSC 52 | done | browser | |
| Message copying through message actions | none | — | |
| Open links, tool groups, `/tools` | done | done (rendering) | |
| Streaming output, run status, terminal status and progress | done | done | |
| File change feedback | done | done | |
| Tool approvals | done (`/permissions` selects the preset and manages command rules; `/approvals` and `/approval` are aliases) | done | [ADR 003](../adr/003-approval-scope.md) |
| Member tool-approval forwarding and original-action review | done | done | [Member approval](subagent-tool-approval.md), [shared client requirements](#member-tool-approval) |
| User questions (`ask_user`) | done (line client and terminal UIs) | none | [Questions](plugins/Questions.md) (implemented in #269, #336, #351) |
| Todo status | out | out | [overview](overview.md) §1 |
| Background tasks: commands and members (detail, stop one, stop all) | done | — | [Background-task list](background-task-list.md), [output](background-task-output.md), [member conversation](member-conversation.md) |
| Background tasks: Workflow runs | done | later phase | [Workflow task view](workflow-task-view.md), [#264](https://github.com/arcships/dim/issues/264) |
| Member transcript cards | done | — | [Member conversation](member-conversation.md) §7–9, [#263](https://github.com/arcships/dim/issues/263) |

### 3.4 System management

| Capability | CLI | Web | Source |
| --- | --- | --- | --- |
| `/settings`: theme, language, startup form, mouse, notifications, approvals | done | later phase (needs a decision on management methods, §6) | Model selection uses `/model` (`/models`), not `/settings`. |
| `/settings`: subagent model, auxiliary model, default multimodal model, execution shell, sandbox, prompt language | designed elsewhere, no client UI; the reply language is a Session setting (`session.language.get/update`), not a `/settings` item | — | overview B.1, [Modality](plugins/Modality.md), [PromptSources §3](plugins/PromptSources.md#3-the-agent-prompt-language-state-source) |
| `/settings`: VS Code focus, resource monitoring, plan usage display | none | — | |
| `/settings`: ACP Session display | done: a saved "Show ACP sessions" switch, off by default; `/resume` labels rows by creation origin | — | [AcpGateway](plugins/AcpGateway.md#a5-client-labels), [Session classification](appserver/session-classification.md) |
| `/usage` | context usage and, for active Dim OAuth models, account usage | — | [CLI usage report](cli-commands.md#usage-report) |
| `/stats` | out | — | [overview](overview.md) App. A |
| `/debug`, debug-log configuration | none (only the daemon's `daemon.log`) | — | [Web workspace §4.3](web-workspace.md#diagnostics-the-legacy-logging-controls) |
| `/tui` mode switch | done | — | [Tui](plugins/Tui.md) |
| Plugin runtimes: see, install, switch source, turn on by configuration | `/runtime` panel and typed forms (designed, [#872](https://github.com/arcships/dim/issues/872)); `dimcode runtime` | — | [CLI commands](cli-commands.md#plugin-runtimes), [RuntimeManager](plugins/RuntimeManager.md) |
| Version and upgrade | done (upgrade dialog); `dimcode upgrade` subcommand: none | — | |
| Web access (`/web`, `dimcode web`) | done | — | [Web workspace](web-workspace.md) |

## 4. Gaps and conflicts

Undesigned capabilities from §3, in feature-list order:

1. Message copying (`/timeline` and its other message actions: [Timeline UI §4.6](timeline-ui.md#46-timeline-jumping-to-a-message)).
2. A prose reference for protocol v4: the wire is defined only in `protocol.rs`, and its description is spread over the App Server README and per-feature designs.
3. Questions in the Web workspace: it handles approvals but not `ask_user`, and web-workspace §4 does not list it as deferred.
4. Session list project/all switch, `/debug`, the remaining `/settings` items, `dimcode upgrade`.

Documents that disagree with the code or with each other:

| Statement | Where | Fact |
| --- | --- | --- |
| Only scrollback is designed; fullscreen-only items (rollback, mouse, `/stats`) are out of scope | [overview](overview.md) line 15, App. A | The fullscreen terminal UI, its mouse mode and `/tui` are implemented and documented in the [native CLI contract](../../crates/dim-cli/README.md); rollback and `/stats` are not |
| F6 does not implement a fullscreen interface | [CLI commands](cli-commands.md) | Same as above |
| `/web` is new work for RemoteGateway | [overview](overview.md) App. A | Delivered by the phase-1 web entry |
| Remote entries never get management methods; no web entry in the entries table | [Session boundary](contracts/session-boundary.md) A.3, [RemoteGateway](plugins/RemoteGateway.md) | The web entry serves the explicit read-only model and UI discovery methods defined in [web workspace §2.5](web-workspace.md#25-protocol-on-the-entry), including `ui.extensions.list` and reliable `ui.extensions.changed` notices |
| RemoteGateway does not reference the web entry that implements part of it | [RemoteGateway](plugins/RemoteGateway.md) | [Web workspace](web-workspace.md) §2.6 lists the differences |

## 5. This repository and Desktop

Desktop lives in its own repository. This repository owns the App Server contract that Desktop uses and nothing of its UI. The rule from the Desktop triage ([#276](https://github.com/arcships/dim/issues/276)–[#281](https://github.com/arcships/dim/issues/281)) is that the App Server changes only where existing facts and mechanisms cannot back a capability; the triage answered each gap without a protocol change:

| Desktop need | Answer |
| --- | --- |
| Current turn, branch head, live Session list | Derived from history and paging `session.catalog` ([#276](https://github.com/arcships/dim/issues/276)) |
| Rename and goal on the stock daemon | Today `--session-rows` installs SessionTitle and Goal ([#277](https://github.com/arcships/dim/issues/277), #290); with [PluginManager](plugins/PluginManager.md) they are built-in plugins of the default CLI |
| Approval outcome, pending question | `approval.resolved` (#289); a pending question is read from history ([#278](https://github.com/arcships/dim/issues/278)) |
| Attachment read, archive, file history | Attachment path from `attachment.put`; archive is client-local; rewind is `session.fork` ([#279](https://github.com/arcships/dim/issues/279)) |
| Branch metadata, rename, per-branch model | From `branch/*` facts; rename client-local; per-branch model not built ([#280](https://github.com/arcships/dim/issues/280)) |
| Local tools, edit/retry, usage, subagent relations | Client-run MCP server; `session.fork` + `session.submit`; client-side usage; `collaboration/*` facts ([#281](https://github.com/arcships/dim/issues/281)) |
| MCP App views (widget document, a view's tool call, its message) | The Session's own MCP connection through `session.mcp.app.*`; rendering and sandboxing stay in the client ([MCP Apps](appserver/mcp-apps.md), [#451](https://github.com/arcships/dim/issues/451)) |
| UI extension assets and Session media without a byte-stream entry | Protocol page reads `ui.extensions.read` and `session.ui.resources.read` ([client UI extensions](client-ui-extensions.md), [#1170](https://github.com/arcships/dim/issues/1170)) |
| Assistant model (the default for new conversations) | Provider's scoped selection: read `provider.selection {cwd?}`, write `provider.select {cwd?, providerId, modelId, expectedVersion?}` ([provider management](appserver/provider-management.md#3-management-contract)). `version` is the CAS token and a stale one is `provider_conflict`; `cwd` absent is the global default, and a workspace selection wins over it at `session.create` |
| Custom instructions (global) | The user's `~/AGENTS.md`, which PromptSources reads at branch load and re-checks every 60 s ([PromptSources](plugins/PromptSources.md#13-content)); a local client may open or edit that file, and no App Server method or setting holds it. A per-conversation instruction is `session.create {systemPrompt?, appendSystemPrompt?}` |
| Subagent max concurrency | Declined: Dim has no member-concurrency setting ([Collaboration](plugins/Collaboration.md)), so a client renders the control unavailable and never saves it locally |
| External agents (enabled state, executable path) | `external_agents.list`, `external_agents.set_enabled` and `external_agents.configure`, with the document revision they CAS ([ExternalAgent A.1](plugins/ExternalAgent.md#a1-management-methods)); a client offers these controls only while `hello.features` lists their methods |

A setting with no owner here is rendered unavailable by the client, never saved locally as if it applied.

Desktop's own feature coverage and UI acceptance are tracked in its repository. A capability that this repository's contracts cannot back becomes an issue here, under the same rule.

## 6. Decisions for the maintainer

```mermaid
flowchart LR
  G["§4 gaps<br/>capabilities with no design"] --> D["§6 decisions<br/>the six items below"]
  X["§4 conflicts<br/>documents versus code"] --> D
  D --> F["Designs and document fixes"]
  D --> R["Roadmap §3.7<br/>owner and milestones"]
```

1. **Fullscreen terminal UI.** The code ships it; the overview says it is out of scope. Either accept it in scope (update the overview and CLI commands, and decide whether rollback and `/stats` stay out) or freeze it.
2. **Web phase 2 scope**, including whether the loopback web entry may carry management methods for settings pages (web-workspace Q2), and whether Questions is required in the Web workspace.
3. **Which §4 gaps get designs**, and in which milestone: message copying and the remaining `/timeline` actions.
4. **A protocol v4 reference** (§4 item 2): write one, or keep the code as the reference.
5. **Milestones for the open client issues**: [#263](https://github.com/arcships/dim/issues/263) (member transcript cards, subagent model policy runtime), [#264](https://github.com/arcships/dim/issues/264) (Workflow view), [#386](https://github.com/arcships/dim/issues/386) (UI events toward the agent).
6. **Owner of the client lane** in the roadmap.
