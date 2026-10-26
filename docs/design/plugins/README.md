# Plugin designs

Designs of the new plugins and proposed supplements to existing plugins in the [overall design](../overview.md). Implemented local-execution plugins and their capabilities are documented in [architecture/rust-agent/plugins](../../architecture/rust-agent/plugins/README.md). Rules: [design rules](../../architecture/rust-agent/principles.md) §1 and §4, [ADR 002](../../adr/002-plugin-scope.md), [plugin contract](../plugin-contract.md) ([ADR 006](../../adr/006-plugin-contract.md)). Shared contracts: [session-access](../contracts/session-access.md), [session-boundary](../contracts/session-boundary.md).

A plugin is split by the owner of its state into parts, each placed in its owner's scope: the process, a Session or a branch. Which parts are Capabilities ([design rules §1](../../architecture/rust-agent/principles.md)) is stated in each design's Rule check. The columns below are scopes; they do not say which parts are Capabilities. Each document records its design and delivery status. Ready permits implementation; it does not mean the package is implemented.

## Index

| Plugin | Process part | Session part | Branch part | Phase |
| --- | --- | --- | --- | --- |
| [Provider management](../appserver/provider-management.md) | existing Provider: configuration, catalogs, client preferences and management methods | — | — | Proposed supplement (#329); no mechanism prerequisite; S4 remains implemented |
| [Questions](Questions.md) | — | `ask_user` tool, `session.question.respond`, waiters of running `ask_user` calls; no events | — | M2 (implemented, #262) |
| [ShellInput](ShellInput.md) | — | `shell.*` methods, command runs | — | P1 (implemented) |
| [Attachments](Attachments.md) | — | `attachment.put`, Session files | — | P1 (implemented) |
| [SessionTitle](SessionTitle.md) | — | observes adoption of the first user input, `session.rename` | — | P1 |
| [Goal](Goal.md) | — | `goal_update` tool, `session.goal.*` | goal state folded from `goal:objective` and `goal:status` blocks, continuation | P2 |
| [Pursuit](Pursuit.md) (evaluator judge mode of Goal) | — | `judge` on `session.goal.set` | completion requests decided by evaluator branches; conclusion inputs and Goal's outcomes | M2 (implemented, #422) |
| [Web](Web.md) | — | `web_search`, `web_fetch` tools | — | P2 (implemented, optional package) |
| [ContextCompaction](ContextCompaction.md) (supplements to Loop and ContextPreparation) | — | `ContextModelHost`, summary `CompactionStrategyHost`, `session.context.*` | `context/compact` command and compaction step in Loop; K-turn cut and usage query in ContextPreparation | P2 |
| [Recall](Recall.md) | — | `recall` tool (tells the model its branch's history: turns, a turn, one thing in full, search) | — | M2 (#129, Ready) |
| [Hooks](Hooks.md) | `HooksSettings` in App Server: sources `hooks` (user) and `hooks-projects`, the project file `.dimcode/hooks.json` read through FileStorage, `hooks.*` methods | hook catalog, runner, a submit check registered with `submitChecks` for the user-input entries (`UserPromptSubmit`) | tool advice, permission requests, post-tool (`PostToolUse`, `PostToolUseFailure`), Stop and Session-start hooks | M2 |
| [Notifications](Notifications.md) | — | Session observer, `notify` tool | — | Withdrawn |
| [Scheduler](Scheduler.md) | triggers, run history (jobs in Settings) | `schedule` tool | — | P2 |
| [Workflow](Workflow.md) | runs (definitions in Settings or project files) | `workflow` tool | — | P2 |
| [ExternalAgent](ExternalAgent.md) | external conversations, Collaboration member kind `external-agent`; bridge row per node | — (delegation and decisions are Collaboration's) | — | P3 |
| [AcpGateway](AcpGateway.md) | — (a client in the CLI process, not a daemon plugin) | — | — | P3 |
| [Tui](Tui.md) | — (client tree in the CLI process: `DaemonLink`, `Tui`, `WebHold`; not daemon plugins) | — | — | P1 (implemented) |
| [RemoteGateway](RemoteGateway.md) | remote entry, devices | — | — | P3 |
| [Modality](Modality.md) | tasks, realtime connections | — | — | P3 |
| [PluginManager](PluginManager.md) | install, enable, reload and remove plugins; the loader's editable layer | `plugin_*` tools | — | M1 (design aligned) |

## By scope

| Scope | Parts |
| --- | --- |
| Process parts | Provider (management supplement proposed), Scheduler, Workflow, ExternalAgent, Modality, PluginManager, RemoteGateway; AcpGateway is not a process plugin: it is a client in the CLI process |
| Session parts | Questions, ShellInput, Attachments, SessionTitle, Goal, Web, Recall, Hooks, Scheduler, Workflow, PluginManager (`plugin_*` tools) |
| Branch parts | Goal (with Pursuit), Hooks |
