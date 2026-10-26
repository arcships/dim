# 插件设计

[总体设计](../overview.md)中规划的新插件的设计文档。已实现的插件及其 Capability 见 [architecture/rust-agent/plugins](../../../architecture/rust-agent/plugins/README.md)。规则：[设计规则](../../../architecture/rust-agent/principles.md) 第 1、4 节，[ADR 002](../../adr/002-plugin-scope.md)，[插件契约](../plugin-contract.md)（[ADR 006](../../adr/006-plugin-contract.md)）。共享合同：[session-access](../contracts/session-access.md)、[session-boundary](../contracts/session-boundary.md)。

插件按状态归属拆成部件，每个部件放在其所有者的作用域：进程、Session 或分支。哪些部件是 Capability（定义见[设计规则](../../../architecture/rust-agent/principles.md)第 1 节），写在设计文档的规则检查里。下表各列是作用域，不表示哪些部件是 Capability。各文档分别记录设计与交付状态。Ready 表示可以实施，不表示包已实现。

## 索引

| 插件 | 进程部件 | Session 部件 | 分支部件 | 阶段 |
| --- | --- | --- | --- | --- |
| [Provider 管理](../appserver/provider-management.md) | 现有 Provider：配置、目录、客户端偏好与管理方法 | — | — | 补充提案（#329），无机制前置；S4 已实现 |
| [Questions](Questions.md) | — | `ask_user` 工具、`session.question.respond`、运行中 `ask_user` 调用的等待者；没有事件 | — | M2（已实现，#262） |
| [ShellInput](ShellInput.md) | — | `shell.*` 方法、命令执行 | — | P1（已实现） |
| [Attachments](Attachments.md) | — | `attachment.put`、Session 文件 | — | P1（已实现） |
| [SessionTitle](SessionTitle.md) | — | 观察首条用户输入的采纳、`session.rename` | — | P1 |
| [Goal](Goal.md) | — | `goal_update` 工具、`session.goal.*` | 由 `goal:objective` 与 `goal:status` 块折叠出的目标状态、续跑 | P2 |
| [Pursuit](Pursuit.md)（Goal 的评审裁判模式） | — | `session.goal.set` 的 `judge` | 由评审分支决定的完成请求；结论输入与 Goal 的结局 | M2（已实现，#422） |
| [Web](Web.md) | — | `web_search`、`web_fetch` 工具 | — | P2（已实现，可选包） |
| [ContextCompaction](ContextCompaction.md)（Loop 与 ContextPreparation 的补充） | — | `ContextModelHost`、摘要 `CompactionStrategyHost`、`session.context.*` | Loop 中的 `context/compact` 指令与压缩步骤；ContextPreparation 中的 K 轮切点与用量查询 | P2 |
| [Recall](Recall.md) | — | `recall` 工具（把分支历史讲给模型：轮次、一轮、一件事的全文、检索） | — | M2（#129，Ready） |
| [Hooks](Hooks.md) | App Server 中的 `HooksSettings`：来源 `hooks`（用户）与 `hooks-projects`，经 FileStorage 读取的项目文件 `.dimcode/hooks.json`，`hooks.*` 方法 | Hook 目录、执行器、向 `submitChecks` 登记供用户输入入口使用的提交检查（`UserPromptSubmit`） | 工具建议、权限请求、工具后（`PostToolUse`、`PostToolUseFailure`）、Stop 与 Session 启动 Hook | M2 |
| [Notifications](Notifications.md) | — | Session 观察、`notify` 工具 | — | 已撤回 |
| [Scheduler](Scheduler.md) | 触发、运行历史（任务在 Settings 中） | `schedule` 工具 | — | P2 |
| [Workflow](Workflow.md) | 运行（定义在 Settings 或项目文件中） | `workflow` 工具 | — | P2 |
| [ExternalAgent](ExternalAgent.md) | 外部会话、Collaboration 成员类型 `external-agent`；每节点一个桥接行 | —（委派与决定归 Collaboration） | — | P3 |
| [AcpGateway](AcpGateway.md) | —（CLI 进程中的客户端，不是守护进程插件） | — | — | P3 |
| [RemoteGateway](RemoteGateway.md) | 远程入口、设备 | — | — | P3 |
| [Modality](Modality.md) | 任务、实时连接 | — | — | P3 |
| [PluginManager](PluginManager.md) | 安装、启用、reload、移除插件；loader 的可编辑层 | `plugin_*` 工具 | — | M1（设计已对齐） |

## 按作用域

| 作用域 | 部分 |
| --- | --- |
| 进程部件 | Provider（管理补充提案）、Scheduler、Workflow、ExternalAgent、Modality、PluginManager、RemoteGateway；AcpGateway 不是进程插件，而是 CLI 进程中的客户端 |
| Session 部件 | Questions、ShellInput、Attachments、SessionTitle、Goal、Web、Recall、Hooks、Scheduler、Workflow、PluginManager（`plugin_*` 工具） |
| 分支部件 | Goal（含 Pursuit）、Hooks |
