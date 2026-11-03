# Plugin designs

Designs of the new plugins and proposed supplements to existing plugins in the [overall design](../overview.md). Local-execution plugins and their capabilities are documented in [architecture/rust-agent/plugins](../../architecture/rust-agent/plugins/README.md). Rules: [design rules](../../architecture/rust-agent/principles.md) §1 and §4, [ADR 002](../../adr/002-plugin-scope.md), [plugin contract](../plugin-contract.md) ([ADR 006](../../adr/006-plugin-contract.md)). Shared contracts: [session-access](../contracts/session-access.md), [session-boundary](../contracts/session-boundary.md).

A plugin is split by the owner of its state into parts, each placed in its owner's scope: the process, a Session or a branch. Which parts are Capabilities ([design rules §1](../../architecture/rust-agent/principles.md)) is stated in each design's Rule check. The columns below are scopes; they do not say which parts are Capabilities. Each document records its design; delivery status lives in the issue. The Phase / issue column is the plan and its tracking issue, not a delivery record.

## Index

| Plugin | Process part | Session part | Branch part | Phase / issue |
| --- | --- | --- | --- | --- |
| [Migration](Migration.md) | on-demand import tasks, source mappings, progress and retry records; unloaded after settlement | — | — | [Delivery plan](../../plan/analysis/legacy-migration.md) |
| [Provider management](../appserver/provider-management.md) | existing Provider: configuration, catalogs, client preferences and management methods | — | — | #329 |
| [Questions](Questions.md) | — | `ask_user` tool, `session.question.respond`, waiters of running `ask_user` calls; no events | — | M2; #262 |
| [ShellInput](ShellInput.md) | — | `shell.*` methods, command runs | — | P1 |
| [Attachments](Attachments.md) | — | `attachment.put`, Session files | — | P1 |
| [SessionTitle](SessionTitle.md) | — | observes adoption of the first user input, `session.rename` | — | P1 |
| [PromptSuggestion](PromptSuggestion.md) | `PromptSuggestionSettings` in App Server: the `prompt-suggestion` Settings source | observes the completed turn's events, `session.suggestion.request` and `session.suggestion.respond`, the offer in flight and the shown offer; no events | — | P1; [#943](https://github.com/arcships/dim/issues/943) |
| [Goal](Goal.md) | — | `goal_update` tool, `session.goal.*` | goal state folded from `goal:objective` and `goal:status` blocks, continuation | P2 |
| [Pursuit](Pursuit.md) (evaluator judge mode of Goal) | — | `judge` on `session.goal.set` | completion requests decided by evaluator branches; conclusion inputs and Goal's outcomes | M2; #422 |
| [Web](Web.md) | — | `web_search`, `web_fetch` tools | — | P2; optional package |
| [ContextCompaction](ContextCompaction.md) (supplements to Loop and ContextPreparation) | — | `ContextModelHost`, summary `CompactionStrategyHost`, `session.context.*` | `context/compact` command and compaction step in Loop; K-turn cut and usage query in ContextPreparation | P2 |
| [Prompt](Prompt.md) | — | `guideEntries`: the input guide's frame, the entries in force and their composition | `Prompt`: the System its publisher adopts carries the composition over the entries in force, and a host-prepared request carries the same composition, placed by Loop | #548 |
| [PromptSources](PromptSources.md) | — | the named sources, their policy texts at the factory configuration, and the state and rule entries | — | #548 |
| [Recall](Recall.md) | — | `recall` tool (tells the model its branch's history: turns, a turn, one thing in full, search) | — | M2; #129 |
| [Hooks](Hooks.md) | `HooksSettings` in App Server: sources `hooks` (user), `hooks-projects` and `hooks-plugins`, the project file `.dimcode/hooks.json` read through FileStorage, `hooks.*` methods and plugin registration | hook catalog, runner, a submit check registered with `submitChecks` for the user-input entries (`UserPromptSubmit`) | tool advice, permission requests, post-tool (`PostToolUse`, `PostToolUseFailure`), Stop and Session-start hooks | M1; #755, #756, #757, #758 |
| [Notifications](Notifications.md) | — | Session observer, `notify` tool | — | Withdrawn |
| [Scheduler](Scheduler.md) | triggers, run history (jobs in Settings) | `schedule` tool | — | P2 |
| [Workflow](Workflow.md) | runs (definitions in Settings or project files) | `workflow` tool | — | P2 |
| [Collaboration](Collaboration.md) | member relations and temporary agents | `agent` tool, reports and messages; the two markers' guide entries | the delegation's facts, wake inputs and settlement | #551 |
| [ExternalAgent](ExternalAgent.md) | external conversations, Collaboration member kind `external-agent`; bridge row per node | — (delegation and decisions are Collaboration's) | — | P3 |
| [AcpGateway](AcpGateway.md) | client-host plugin: ACP/RPC connections, requests and cleanup; outside the daemon | — | — | #436; PR #441; [acceptance](../acp-gateway-acceptance.md) |
| [TemporaryMcp](TemporaryMcp.md) | registered by the existing MCP feature | temporary bindings, MCP server children and tools; no settings writes | — | #436 |
| [Tui](Tui.md) | — (client tree in the CLI process: `DaemonLink`, `Tui`, `WebHold`; not daemon plugins) | — | — | P1 |
| [RemoteGateway](RemoteGateway.md) | remote entry, devices | — | — | P3 |
| [Modality](Modality.md) | tasks, realtime connections | — | — | P3 |
| [Skills](Skills.md) (installation) | existing SkillsCatalog: preview of a local or Git source, selective installation into the user root, update and confirmed removal of loose Skills | — (discovery, enablement and the `skill` tool already belong to the Skills capability) | — | #740 |
| [PluginManager](PluginManager.md) | plugin configuration (`plugins.json`), install, loader rows of plugin code, content registration | `PluginTools` (`plugin` tool) | — | M1 |
| [RuntimeManager](RuntimeManager.md) | the Node and Python runtimes of plugin code: pinned download, verification, start; `runtime.*` | — | — | M1 (#476) |

## By scope

| Scope | Parts |
| --- | --- |
| Process parts | Provider (management supplement), Scheduler, Workflow, ExternalAgent, Modality, PluginManager, RuntimeManager, RemoteGateway, Skills (installation), PromptSuggestion (the Settings source) in the daemon; AcpGateway in its client host process |
| Session parts | Questions, ShellInput, Attachments, SessionTitle, PromptSuggestion, Goal, Web, Recall, Hooks, Scheduler, Workflow, PromptSources, Prompt (the entries table) |
| Branch parts | Goal (with Pursuit), Hooks, Prompt |
