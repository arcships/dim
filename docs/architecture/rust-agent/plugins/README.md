# Local-execution business plugins

This directory has one page per plugin of the **local-execution profile**, not every App Server
plugin. A plugin's page documents each part it registers and, for a plugin that owns process
state, the plugin itself: each has its own dependency table and generated block. The authoritative assembly is
[`defaults::install_local_execution`](../../../../crates/dim-agent-capabilities/src/defaults.rs)
plus Attachments (which `serve` adds where the host has native storage) and
Questions (which the product profiles add), with process services and scripted model hosts supplied by
[`xtask`](../../../../xtask/src/plugins.rs). Its recorded bindings are in
[`generated/local/services.json`](../generated/local/services.json).
The graph uses isolated host facts and one fixture Skill, not the developer's
home. The host fixture reuses HostPlatformPlugin's declared dependencies and keys.
SessionStore's `sessionFiles` is a graph-only fixture that stores nothing.

Infrastructure rows `BranchScope`, `SessionScope`, `SessionBroadcast`,
`SessionBroadcastEnd`, `SessionExtension`, `rutis-loader`, `session` and `branch`, the instances'
mounters (`SessionParts`, `SessionOptionalParts`, `BranchParts`, `BranchOptionalParts`), the row
providing `partRegistry`, and the scripted `PrepareModelHost` plugin are excluded. This list may hold only
runtime assembly and scripted model hosts: a newly assembled capability needs its own page instead of an
entry here. Excluded rows can still appear as providers or consumers in the generated blocks. A service
dependency is not necessarily a plugin with the same name.

## Index

| Plugin | Process | Session parts | Branch parts |
| --- | --- | --- | --- |
| [Skills](Skills.md) | SkillsCatalog | Skills, SkillContent (SkillContentInstances) | SkillsBranch |
| [PromptSources](PromptSources.md) | — | PromptRegistry, PromptSources | PromptSourcesBranch |
| [WorkspaceTools](WorkspaceTools.md) | — | WorkspaceTools | FileReads, BackgroundExec |
| [Wait](Wait.md) | — | Wait | WaitBranch |
| [Recall](Recall.md) | — | Recall | — |
| [TimelineTree](TimelineTree.md) | — | TimelineTree | TreeTimeline |
| [InputQueue](InputQueue.md) | — | InputMethods | InputQueue |
| [Permission](Permission.md) | — | — | Permission, Approval |
| [ToolExecution](ToolExecution.md) | — | — | ToolBinding, ToolExecutor |
| [ToolRegistry](ToolRegistry.md) | — | ToolRegistry | — |
| [SessionMethods](SessionMethods.md) | — | SessionMethods | — |
| [ShellInput](ShellInput.md) | — | ShellInput | — |
| [Attachments](Attachments.md) | — | Attachments | — |
| [Questions](Questions.md) | — | Questions | — |
| [BranchTool](BranchTool.md) | — | BranchTool, BranchLine | — |
| [ContextPreparation](ContextPreparation.md) | — | — | ContextPreparation |
| [Prompt](Prompt.md) | — | — | Prompt |
| [Loop](Loop.md) | — | — | Loop |
| [ModelProvider](ModelProvider.md) | — | — | ModelProvider |
| [HostPlatform](HostPlatform.md) | HostPlatform | — | — |
| [FileStorage](FileStorage.md) | FileStorage | — | — |
| [Hooks](Hooks.md) | — | HooksSession, HookSources (App Server) | HooksBranch |

App Server optional plugins are outside this local-execution profile. [Web](../../../design/plugins/Web.md) provides `web_search` and `web_fetch` through a Session part its `web` process row registers; it adds no service to the assembly recorded here.

Process infrastructure rows are also outside this local-execution index. `UiHost` owns UI leaves: per leaf, a `UiLeaf` plugin whose life is the rutis session with the interactive `dimcode` process. Its contract is [Tui design §3.6](../../../design/plugins/Tui.md#36-ui-registry-and-ui-leaves) and its current assembly is in the generated [App Server services](../generated/app-server/services.md). The leaf runs its own `Tui` in the CLI's client tree; it is not a local-execution capability. The language runtimes' connection rows, `session-node` and `py`, are process infrastructure too: each starts its runtime and mounts the rutis session its foreign rows run on ([How Dim uses rutis A.4](../../../design/rutis-usage.md#a4-runtimes-and-sessions)).

## Reading and maintaining these pages

Dependency tables describe the exact injected services in this profile:
`B:` means the calling branch (`main` in the graph), `S:` its Session, and `P:`
the process. Named contributions retain their `#name`. Conditional dependencies
are listed only when enabled in this assembly. A dependency used only as a
lifecycle gate is still a dependency. Per-call branch service lookups are
explained separately from injected services.

Each plugin is one process row that registers its Session and branch parts with the part registry
([ADR 006 D3](../../../adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it));
parts are plain rutis `TypedPlugin`s that depend on services by name (`Keyed<T>` and the other
forms of the [Rust binding](../../../design/rutis-usage.md#appendix-a-rust-binding)), each name in
the catalog group it is registered in; `Deps` and the keys the catalog gives each copy control
readiness and lifecycle, not a security sandbox. Generators and factories
capture configuration and stable identities, not live I/O. `apply` acquires
generation resources and `ctx.effect` registers cleanup. Capabilities own their
tasks, cancellation and terminal outcomes; Dispatcher handlers register work
and return rather than awaiting host completion. Opening receipts and dispatch
fences precede branch readiness. Cancellation requests settlement; it does not
prove a non-cooperative host has finished. Loader `Pending` is **not** proof of
successful business settlement or terminal commits.

Timeline facts are durable; state cells, watches and runtime handles are not.
Replay must not rerun uncertain external effects. Runtime context enters through
adopted raw inputs containing final content, or public tool results, not a
domain-specific second history projection. Completed compaction facts must
remain effective without a selected recovery snapshot. Snapshots are separate,
optional interpreter caches; compatibility is checked before decoding state.
`Timeline.context` reads full facts; `HistoryRenderer.context` selects a supported
snapshot, and `history::read_context` uses the default interpreter. These pages
describe current source, not the planned dynamic-event, loader-configuration or
cross-language APIs in the [foundation ADR](../../../adr/001-harness-foundation.md).

Run `cargo run --locked -p xtask -- graph` to refresh all generated blocks;
never edit their contents by hand. English prose surrounds the generator's
current table labels. `cargo run --locked -p xtask -- check` checks recorded-graph
coverage in both directions, block freshness and exact dependency tables. Missing
or empty documentation directories fail. Run `graph` after assembly changes so
new capabilities participate in that check; plugins outside this profile are not
covered by it. A page documents the names of the generated blocks it holds; each
name's dependency table is the `Dependencies` (or `依赖`) table between the
previous block and its own. Linked tests
are existing evidence to consult, not a claim that every lifetime edge is covered.
