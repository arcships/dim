# Local-execution business plugins

This directory covers the **33 business plugins and content groups in the local-execution
profile**, not every App Server plugin. The authoritative assembly is
[`defaults::install_local_execution`](../../../../crates/dim-agent-capabilities/src/defaults.rs)
plus Attachments (which `serve` adds where the host has native storage) and
Questions (which the product profiles add), with process services and scripted model hosts supplied by
[`xtask`](../../../../xtask/src/plugins.rs). Its recorded bindings are in
[`generated/local/services.json`](../generated/local/services.json).
The graph uses isolated host facts and one fixture Skill, not the developer's
home. The host fixture reuses HostPlatformPlugin's declared dependencies and keys.
SessionStore's `sessionFiles` is a graph-only fixture that stores nothing.

Infrastructure rows `BranchScope`, `SessionScope`, `SessionBroadcast`,
`SessionBroadcastEnd`, `SessionExtension`, `rutis-loader`, `session` and `branch`, and the scripted
`PrepareModelHost` plugin are excluded. They can still appear as providers or
consumers in the generated blocks. A service dependency is not necessarily a
plugin with the same name.

## Index

| Scope | Plugins |
| --- | --- |
| Process | [FileWriteCoordinator](FileWriteCoordinator.md), [SkillsCatalog](SkillsCatalog.md), [HostPlatform](HostPlatform.md), [FileStorage](FileStorage.md) |
| Session | [Skills](Skills.md), [SkillContributions](SkillContributions.md), [SkillContentInstances](SkillContentInstances.md), [SkillContent](SkillContent.md), [PromptSources](PromptSources.md), [PromptRegistry](PromptRegistry.md), [Wait](Wait.md), [ShellInput](ShellInput.md), [WorkspaceTools](WorkspaceTools.md), [ToolRegistry](ToolRegistry.md), [TimelineTree](TimelineTree.md), [InputMethods](InputMethods.md), [Attachments](Attachments.md), [Questions](Questions.md) |
| Branch | [SkillsBranch](SkillsBranch.md), [PromptSourcesBranch](PromptSourcesBranch.md), [WaitBranch](WaitBranch.md), [BackgroundExec](BackgroundExec.md), [FileReads](FileReads.md), [ToolExecutor](ToolExecutor.md), [ToolBinding](ToolBinding.md), [Approval](Approval.md), [Permission](Permission.md), [ModelProvider](ModelProvider.md), [Loop](Loop.md), [InputQueue](InputQueue.md), [Prompt](Prompt.md), [ContextPreparation](ContextPreparation.md), [TreeTimeline](TreeTimeline.md) |

Optional TypeScript packages are outside this native profile. [Web](../../../design/plugins/Web.md) provides `web_search` and `web_fetch` through existing Session rows; its [package documentation](../../../../runtime/plugins/web/README.md) describes installation and verification. It adds no service to the assembly recorded here.

## Reading and maintaining these pages

Dependency tables describe the exact injected services in this profile:
`B:` means the calling branch (`main` in the graph), `S:` its Session, and `P:`
the process. Named contributions retain their `#name`. Conditional dependencies
are listed only when enabled in this assembly. A dependency used only as a
lifecycle gate is still a dependency. Per-call branch service lookups are
explained separately from injected services.

Plugins implement rutis `TypedPlugin` plus runtime `Capability`; `Deps` and
scoped keys control readiness and lifecycle, not a security sandbox. Factories
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
covered by it. Both `## Dependencies` and `## 依赖` are supported. Linked tests
are existing evidence to consult, not a claim that every lifetime edge is covered.
