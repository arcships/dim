# 已知 Capability 与 Timeline 事件

这是一份实现导航表，不新增契约、事件注册限制或证明要求。核对基线：`cd422bf8f`。
范围是仓库内生产实现，以及记录的产品/本地装配；不含测试插件和外部动态安装包。
后续代码变化应以链接的类型、装配与所属契约为准，而不是把本表当成第二份规范。

## Capability 候选与 Timeline 关联部件（22 项）

候选来自依赖 Timeline / Dispatcher 的机械扫描，而 [Capability 契约](../../design/plugin-contract.md#6-capability)
按折叠状态或写自身领域事实的行为定义。依赖存在不等于行为分类已经核实；只写输入或提供服务
的部件不能仅凭依赖被认定为 Capability。产品的 20 个标记项取自[生成目录](generated/catalog.md#capabilities)；
另补[本地装配](generated/local/services.json)的 HooksBranch 和源码中的可选 ShellSession。
这 22 个候选都是 **branch** 作用域；Session 中的 Wait 工具等同名业务部件不因此自动算 Capability。
这里只列主要事件关联，不把订阅列表当成全部读取依赖。

| 候选部件 | 主要职责 | 主要事件关联 | 装配范围 | 实现 / 所属说明 |
| --- | --- | --- | --- | --- |
| Approval | 审批请求、响应与未完成请求结算 | approval/*；读取权限状态 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/approval.rs) / [说明](plugins/Permission.md) |
| BackgroundExec | 后台执行任务及完成通知 | exec/*、input/message；中断与回合边界 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) / [说明](plugins/WorkspaceTools.md) |
| BranchLine | 父子分支工作的跟踪、输入交接与结果通知 | branch/*、input/*、turn/*、step/tool-* | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) / [说明](plugins/BranchTool.md) |
| CollaborationTimeline | 跨 Session 协作交接与结算 | collaboration/*、input/*、turn/*、step/* | 产品生成目录 | [实现](../../../crates/dim-app-server/src/collaboration.rs) |
| ContextPreparation | 准备模型请求与上下文压缩工作 | context/prepare、compaction-*、model/call；失败输出终态 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/context_preparation.rs) / [说明](plugins/ContextPreparation.md) |
| ContextUsage | 上下文使用量计算与派生结果恢复 | context/*、model/*、input/consumed、step/*、context-usage/* | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/context_usage.rs) / [说明](plugins/ContextUsage.md) |
| EnvironmentClientsBranch | 把连接客户端环境作为上下文输入发布 | input/message（环境块） | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/environment_clients.rs) |
| FileReads | 文件读写绑定及其是否仍在模型上下文内 | 查询 context/compaction 与切点位置；不直接写事实 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/workspace_tools/file_reads.rs) / [说明](plugins/WorkspaceTools.md) |
| GoalBranch | 目标状态、调度判断与恢复 | goal/state；input、turn、exec、branch 相关事实 | 产品生成目录 | [实现](../../../crates/dim-goal/src/branch.rs) |
| InputQueue | 输入排队、采用、撤回和删除 | input/*、turn/*、stop/interrupt | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/input_queue.rs) / [说明](plugins/InputQueue.md) |
| Loop | 回合、模型步骤、工具交接和终态协调 | turn/*、input/consumed、step/*、context/prepare、context/compact | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/agent_loop.rs) / [说明](plugins/Loop.md) |
| ModelProvider | 模型调用、流式输出和失败结算 | model/call、model/usage、step/*；中断与回合边界 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/model_provider.rs) / [说明](plugins/ModelProvider.md) |
| Permission | 分支权限策略更新与读取 | permission/changed | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/permission.rs) / [说明](plugins/Permission.md) |
| Prompt | 提示词配置采用、作用域与业务状态恢复 | prompt/*、context/configured、context/scope、input/message、prompt-state/recovered | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/prompt.rs) / [说明](plugins/Prompt.md) |
| SessionBroadcast | 把已派发事实转成 Session 对外通知 | Dispatcher 通知；不直接写 Timeline 事实 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/session_broadcast.rs) |
| SettingsSnapshotBranch | 把执行设置快照作为上下文输入发布 | input/message（设置块） | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/execution_settings/prompt.rs) |
| ToolBinding | 采用当前工具绑定 | branch/start、branch/resume；查询配置事实 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/tool_binding.rs) / [说明](plugins/ToolExecution.md) |
| ToolExecutor | 工具调用、资源清理和执行终态 | step/tool-call → tool-start/completed/result；中断与回合边界 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/tool_executor.rs) / [说明](plugins/ToolExecution.md) |
| TreeTimeline | 执行分支创建、结束命令及结算 | branch/create、branch/end、turn/end；branch 结果事实 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) / [说明](plugins/TimelineTree.md) |
| WaitBranch | 当前回合内的等待通知与单次等待 | input/message、turn/start、turn/end；不直接写事实 | 产品生成目录 | [实现](../../../crates/dim-agent-capabilities/src/wait.rs) / [说明](plugins/Wait.md) |
| HooksBranch | 按已消费事实运行 Hooks，采用有效输出 | input/message；读取 input、turn、approval、tool 等触发事实 | 本地装配 | [实现](../../../crates/dim-agent-capabilities/src/hooks/runner.rs) / [说明](plugins/Hooks.md) |
| ShellSession | 持续 shell 执行、后台任务和完成通知 | exec/*、input/message；读取中断与回合事实 | 可选 shell 插件 | [实现](../../../crates/dim-agent-capabilities/src/shell_session.rs) |

## 核心 Timeline 事件（50 个具体 key）

逐项来自 [EventKind 及 key 映射](../../../crates/dim-agent-types/src/events.rs)。下面的写入方与读取方
是已知的主要路径，**不是排他名单，也不声称穷举所有查询和客户端**。读取包括实时消费、
恢复查询与历史投影；所有事实还会经过 Timeline 存储和 SessionBroadcast 等通用路径。
同一类型可由多个符合契约的生产方写入，新增消费者不需要修改这张表才能生效。
类型定义决定字段与兼容性，本表不再抄一套 payload。

| 事件 key | 含义 | 已知写入路径 | 主要消费 / 读取路径 |
| --- | --- | --- | --- |
| `input/message` | 采用的用户或运行时输入 | 输入入口、Prompt、通知生产方等 | InputQueue、WaitBranch、Prompt/历史上下文读取 |
| `input/applied` | 队列将输入交给执行流程 | InputQueue | Loop、ModelProvider/ContextPreparation 的 steer 处理 |
| `input/withdraw` | 请求撤回尚未消费的输入 | InputQueue 撤回入口 | InputQueue |
| `input/deleted` | 输入已删除，不再采用 | InputQueue | 输入恢复、GoalBranch、BranchLine、协作 |
| `input/consumed` | Loop 已将输入纳入当前回合 | Loop | InputQueue、GoalBranch、HooksBranch、上下文读取 |
| `stop/interrupt` | 中断指定执行范围 | 分支停止入口及所属工作协调方 | Loop、ModelProvider、ContextPreparation、ToolExecutor 等工作所有者 |
| `approval/request` | 等待批准的调用 | Approval | Approval、HooksBranch、协作审批、客户端通知 |
| `approval/respond` | 对审批请求的决定 | Approval 响应入口 | Approval、HooksBranch、协作审批 |
| `approval/cancelled` | 审批请求取消结算 | Approval | Approval 恢复、HooksBranch、协作审批 |
| `permission/changed` | 分支权限策略变更 | Permission | Permission |
| `exec/started` | 后台执行已开始 | BackgroundExec、ShellSession | 各执行所有者恢复、GoalBranch、执行历史 |
| `exec/ended` | 后台执行终态 | BackgroundExec、ShellSession | 各执行所有者、GoalBranch；另由生产方发布唤醒输入 |
| `turn/start` | 回合开始 | Loop | WaitBranch、Prompt、InputQueue、执行与协作部件 |
| `turn/end` | 回合结束及结果 | Loop | WaitBranch、InputQueue、GoalBranch、HooksBranch、执行与协作部件 |
| `prompt/changed` | 提示词快照已采用 | Prompt 经 ConfigurationPublisher | Prompt、ContextUsage/配置恢复 |
| `prompt/configure-request` | 请求配置提示词 | Prompt 配置入口 | Prompt |
| `prompt/configure-result` | 提示词配置操作结果 | Prompt | Prompt |
| `context/configured` | 上下文配置已采用 | ConfigurationPublisher（ContextPreparation） | Prompt、ToolBinding、ContextUsage |
| `context/scope` | 上下文作用域状态 | 本基线未找到专门的生产写入路径 | Prompt |
| `context/prepare` | 请求准备一次模型上下文 | Loop | ContextPreparation、ContextUsage |
| `context/compact` | 请求压缩上下文 | Loop 的 compact 入口 | Loop、ContextPreparation、ContextUsage |
| `context/compaction-start` | 压缩工作开始 | ContextPreparation | ContextPreparation 恢复、上下文策略 |
| `context/compaction` | 压缩结果与终态 | ContextPreparation | Loop、ContextUsage、FileReads、上下文历史/策略 |
| `context/budget` | 上下文预算决定 | 上下文策略 context_policy | 上下文策略/历史投影 |
| `model/call` | 准备完毕、交给模型的请求 | ContextPreparation | ModelProvider、ContextUsage |
| `model/usage` | 模型返回的用量 | ModelProvider 的输出处理 | ContextUsage |
| `step/model-start` | 模型步骤开始与交接身份 | Loop | ModelProvider、ContextPreparation、协作 |
| `step/thinking` | 模型思考内容片段 | ModelProvider 的输出处理 | 历史/客户端投影、PromptSuggestion |
| `step/text` | 模型文本片段 | ModelProvider 的输出处理 | 历史/客户端投影、PromptSuggestion、协作 |
| `step/tool-use-start` | 模型开始生成一个工具调用 | ModelProvider 的输出处理 | Loop、协作、历史投影 |
| `step/tool-use` | 模型给出工具调用参数 | ModelProvider 的输出处理 | Loop、BranchLine、协作、历史投影 |
| `step/model-content` | 模型结构化内容 | ModelProvider 的输出处理 | 历史/客户端投影、PromptSuggestion |
| `step/model-output-end` | 模型步骤输出终态 | ModelProvider、ContextPreparation；Loop 恢复结算 | Loop、ContextUsage、协作/历史查询 |
| `step/tool-call` | 已交接给执行器的工具调用 | Loop | ToolExecutor、协作审批 |
| `step/tool-start` | 工具执行开始 | ToolExecutor | Loop、恢复/诊断查询 |
| `step/tool-completed` | 工具执行已完成，待交接结果 | ToolExecutor | Loop |
| `step/tool-result` | 模型可读取的工具结果 | ToolExecutor；Loop/ModelProvider 的恢复结算 | Loop、ContextUsage、HooksBranch、协作/历史 |
| `branch/create` | 请求创建子分支 | TimelineTree/分支工具入口 | TreeTimeline、BranchLine、GoalBranch |
| `branch/start` | 子分支开始 | TimelineTree | Prompt、ToolBinding、GoalBranch |
| `branch/resume` | 分支重新装载 | TimelineTree | Prompt、ToolBinding |
| `branch/create-failed` | 创建子分支失败 | TimelineTree | BranchLine、GoalBranch |
| `branch/end` | 请求结束分支 | TimelineTree/分支工具入口 | TreeTimeline、BranchLine、GoalBranch |
| `branch/ended` | 分支已结束 | TimelineTree | BranchLine、GoalBranch |
| `collaboration/request` | 发起跨 Session 协作 | 协作服务 | CollaborationTimeline |
| `collaboration/cancel` | 取消协作工作 | 协作服务 | CollaborationTimeline、协作审批 |
| `collaboration/close` | 关闭协作关系 | 协作服务 | CollaborationTimeline、协作审批 |
| `collaboration/message` | 协作消息 | 协作服务 | CollaborationTimeline、历史读取 |
| `collaboration/delivered` | 协作消息已交接 | 协作服务 | CollaborationTimeline |
| `collaboration/result` | 协作工作结果 | 协作服务 | CollaborationTimeline、历史读取 |
| `session/forked` | Session 分叉标记 | App Server Timeline 存储的 fork 路径 | GoalBranch、历史读取 |

事件组的实现与契约入口：

| 事件组 | 定义以外的入口 |
| --- | --- |
| input、stop、turn | [InputQueue](plugins/InputQueue.md)、[Loop](plugins/Loop.md)、[分支输入](../../../crates/dim-agent-capabilities/src/branch_input.rs) |
| approval、permission | [Permission / Approval](plugins/Permission.md) |
| exec | [WorkspaceTools](plugins/WorkspaceTools.md)、[ShellSession](../../../crates/dim-agent-capabilities/src/shell_session.rs) |
| prompt、context | [Prompt](plugins/Prompt.md)、[ContextPreparation](plugins/ContextPreparation.md)、[ContextUsage](plugins/ContextUsage.md)、[上下文策略](../../../crates/dim-agent-capabilities/src/context_policy.rs) |
| model、step | [ModelProvider](plugins/ModelProvider.md)、[输出提交](../../../crates/dim-agent-capabilities/src/output.rs)、[ToolExecution](plugins/ToolExecution.md)、[Loop](plugins/Loop.md) |
| branch | [TimelineTree](plugins/TimelineTree.md)、[BranchLine](plugins/BranchTool.md) |
| collaboration | [协作实现](../../../crates/dim-app-server/src/collaboration.rs) |
| session/forked | [Timeline 存储](../../../crates/dim-app-server/src/storage/timeline.rs) |

## 仓库内写入的扩展事实（5 个具体 key）

这些事实通过 `EventKind::Extension` 携带，不在核心枚举里逐项定义。Extension 是开放载体，
不是第 51 个固定事件 key。表中不含测试/性能探针制造的扩展事实，也不枚举用户插件的自定义域。

| 事件 key | 用途 | 已知写入 / 读取路径 | 实现 |
| --- | --- | --- | --- |
| `goal/state` | 目标业务状态结果 | GoalBranch 写入与恢复读取 | [Goal recovery](../../../crates/dim-goal/src/branch/recovery.rs) |
| `prompt-state/recovered` | 提示词恢复结果 | Prompt 写入与恢复读取 | [Prompt recovery](../../../crates/dim-agent-capabilities/src/prompt/recovery.rs) |
| `context-usage/recovered` | 上下文用量派生结果 | ContextUsage 写入与恢复读取 | [ContextUsage](../../../crates/dim-agent-capabilities/src/context_usage.rs) |
| `context-usage/covered` | 派生结果已覆盖的来源位置 | ContextUsage 写入与恢复读取 | [ContextUsage](../../../crates/dim-agent-capabilities/src/context_usage.rs) |
| `migration/imported` | 导入历史的来源标记，不恢复历史执行 | migration 导入器写入；未找到专门的业务消费者 | [导入实现](../../../crates/dim-migration/src/native.rs) |

## 边界与待核对项

- EventHub 的外部通知、rutis 生命周期通知、Session/UI 广播不等同于 Timeline 事件。
  EventHub 消息进入分支时走采用输入路径；不把外部通知类型重复算成核心事件。
- WaitBranch 读取 `input/message` 中的通知，不是直接消费 `exec/ended`；执行方另外生成唤醒输入。
- 当前 Loop 文档出现 `step/disposition`，但本基线的 EventKind/key 映射和生产代码中未找到它。
  本表不把它记成已实现事件；需要在相应设计/实现变更中核对。
- 以上是清单覆盖，不是遵守性审计通过。没有据此要求每项补状态机、数学证明或 TLC 模型。
