# Plugin designs

Designs of the new plugins planned in the [overall design](../overview.md). Implemented plugins and their capabilities are documented in [architecture/rust-agent/plugins](../../architecture/rust-agent/plugins/README.md). Rules: [design rules](../../architecture/rust-agent/principles.md) §1 and §4, [ADR 002](../../adr/002-plugin-scope.md). Shared contracts: [session-access](../contracts/session-access.md), [session-boundary](../contracts/session-boundary.md).

A plugin is split by the owner of its state. Process parts are process plugins; Session and branch parts are Capabilities. Each document records its design and delivery status. Ready permits implementation; it does not mean the package is implemented.

## Index

| Plugin | Process plugin | Session Capability | Branch Capability | Phase |
| --- | --- | --- | --- | --- |
| [Questions](Questions.md) | — | `ask_user` tool, `session.question.respond`, waiters of running `ask_user` calls; no events | — | M2 (Ready) |
| [ShellInput](ShellInput.md) | — | `shell.*` methods, command runs | — | P1 (implemented) |
| [Attachments](Attachments.md) | — | `attachment.put`, Session files | — | P1 (implemented) |
| [SessionTitle](SessionTitle.md) | — | observes adoption of the first user input, `session.rename` | — | P1 |
| [Goal](Goal.md) | — | `goal_update` tool, `session.goal.*` | goal state folded from `goal:objective` and `goal:status` blocks, continuation | P2 |
| [Pursuit](Pursuit.md) (evaluator judge mode of Goal) | — | `judge` on `session.goal.set` | claims judged by evaluator branches, verdict writes | M2 |
| [Web](Web.md) | — | `web_search`, `web_fetch` tools | — | P2 (implemented, optional package) |
| [ContextCompaction](ContextCompaction.md) (supplements to Loop and ContextPreparation) | — | `ContextModelHost`, summary `CompactionStrategyHost`, `session.context.*` | `context/compact` command and compaction step in Loop; K-turn cut and usage query in ContextPreparation | P2 |
| [Recall](Recall.md) | — | `recall` tool (tells the model its branch's history: turns, a turn, one thing in full, search) | — | M2 (#129, Ready) |
| [Hooks](Hooks.md) | — | hook documents, runner, `hooks.catalog` | tool advice, permission requests, prompt holds, post-tool, Stop and Session-start hooks | M2 |
| [Notifications](Notifications.md) | — | Session observer, `notify` tool | — | Withdrawn |
| [Scheduler](Scheduler.md) | triggers, run history (jobs in Settings) | `schedule` tool | — | P2 |
| [Workflow](Workflow.md) | runs (definitions in Settings or project files) | `workflow` tool | — | P2 |
| [ExternalAgent](ExternalAgent.md) | connections, `modelKind` `external-agent`, asks; bridge row per node | ask decision tools, `external_agent` methods | — | P3 |
| [AcpGateway](AcpGateway.md) | — (a client in the CLI process, not a daemon plugin) | — | — | P3 |
| [RemoteGateway](RemoteGateway.md) | remote entry, devices | — | — | P3 |
| [Modality](Modality.md) | tasks, realtime connections | — | — | P3 |
| [PluginManager](PluginManager.md) | marketplaces, packages | — | — | P3 |

## By scope

| Scope | Parts |
| --- | --- |
| Process plugins | Scheduler, Workflow, ExternalAgent, Modality, PluginManager, RemoteGateway; AcpGateway is not a process plugin: it is a client in the CLI process |
| Session Capabilities | Questions, ShellInput, Attachments, SessionTitle, Goal, Web, Recall, Hooks, Scheduler, Workflow, ExternalAgent |
| Branch Capabilities | Goal (with Pursuit), Hooks |
