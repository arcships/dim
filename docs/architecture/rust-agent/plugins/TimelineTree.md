# TimelineTree

Session 的分支目录：拥有全部分支实例、分支身份预留、构建任务、就绪结果与关闭；创建 main、恢复已存储的分支、处理新分支的提交与构建，并提供模型的 `branch` 工具。不订阅分支事件（分支半边 [TreeTimeline](TreeTimeline.md) 把本分支的 `branch/create` 转交过来），不限制分支数量。创建流程见[事件驱动约定·分支创建](../event-driven-flows.md#4-分支创建)，装配见[单树装配](../single-tree.md#session-与分支)。

<!-- generated:plugin TimelineTree — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionIdentity`、`S:TimelineFactory`、`S:dyn TimelineStore` |
| 提供 | `S:TimelineTree`、`S:ToolContribution#branch`、`S:TreeCommands`、`S:dyn TimelineTreeControl` |
| 被注入于 | `ToolRegistry`、`TreeTimeline`、`Wait`、`WorkspaceTools` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:TimelineFactory` | 按 `(分支 id, 祖先链)` 构建分支（载入 BranchScope 与分支能力，返回 TimelineContext） |
| `S:dyn TimelineStore` | 恢复时读子分支自己的事件，判定创建是否完成 |
| `S:SessionIdentity` | 读存储时的 `session_id` |

## 服务接口

| 服务 | 方法 | 语义 | 错误 |
| --- | --- | --- | --- |
| `S:dyn TimelineTreeControl`（Session 持有） | `create_main()` | 构建 main，写 `branch/start`（无父）后发布 | 构建错误；`Closed` |
| | `restore(stored)` | 按父先子后恢复；空目录则创建 main。见下文“恢复” | `Storage`（目录缺父或成环、截点不合法） |
| | `branch(parent, at, options, admission)` | 见下文“创建”；`Session::branch` 经此调用 | `UnknownBranch`、`InvalidInput`（位置越界或切开因果）、`Closed`、`Cancelled`、构建错误 |
| | `get(id)`、`list()` | 只返回已发布的分支 | — |
| | `begin_close()`、`close()` | 停止准入，取消未启动的创建（写 `branch/create-failed`），等待在建任务，关闭全部分支 | 汇总清理错误 |
| `S:TreeCommands` | `get(id)`、`list()`、`branch(...)` | 同上，弱引用树；供分支能力与 Session 能力使用；`list` 只返回已发布的分支，`get` 在分支构建中、发布前即可取得句柄（初始化期间需要树服务） | 树已释放：`Closed`；`get` 找不到分支或尚无上下文：`UnknownBranch` |
| `S:TimelineTree` | — | 树本身；目前没有插件注入，只有流程测试按键读取 | — |
| `S:ToolContribution#branch` | `branch` 工具 | 参数 `prompt`（非空）、`at`（可选）；省略 `at` 时截点取本次请求 `step/model-start` 之前一位；结果文本 `Branch created: <id>`；risk `session`，经 [Approval](Approval.md) 准入 | 参数非法；创建错误映射为工具失败，准入令牌取消映射为 `Cancelled` |

## 运行时调用方

| 调用方 | 方式 |
| --- | --- |
| [Wait](Wait.md)、[WorkspaceTools](WorkspaceTools.md) | 注入 `S:TreeCommands`，按 `context.branch_id` 取分支句柄与其服务 |
| App Server Collaboration（`CollaborationCapability`） | 注入 `S:TreeCommands`，按分支 id 取分支句柄 |
| Session / Agent | `S:TimelineTreeControl`：启动、恢复、`Session::branch`、关闭 |

生成块的“被注入于”只来自 agent 默认装配，不含 App Server 装配的能力。

## 创建

`branch(parent, at, options, admission)`：

- 校验 `at` 不超过父尾部、截点合法；提交 `branch/create` 时按最终事件计算继承位置并预留分支身份，父分支关闭中或准入已取消则拒绝。
- 等待就绪结果；准入令牌在提交后取消时立即返回 `Cancelled`，构建由树继续完成。
- 构建、`branch/start`、首条输入（`first_input`）、发布与失败回滚的流程见[事件驱动约定·创建与生命周期](../event-driven-flows.md#42-创建与生命周期)。

## 事件

写出：

| 事件 | 写到 | 时机 |
| --- | --- | --- |
| `branch/create` | 父分支 | `branch(...)` 提交；`at` 等于父尾部写 `create`，否则写 `sequence` |
| `branch/start` | 子分支 / main | 新建分支构建完成；恢复时从未开始过的根分支（存储序号为 0） |
| `branch/resume` | 已存储的分支 | 恢复构建完成 |
| `input/message`（Queue） | 子分支 | `branch/start` 分发完后的首条输入：`branch` 工具创建的是 raw 块 `<dim-branch-task parent="…">`，宿主创建的是操作者原文 |
| `branch/create-failed` | 父分支 | 构建失败或取消（首条输入写出之前）；Session 关闭或父分支卸载时取消未启动的创建；恢复时判定创建未完成 |

本插件不订阅事件。

## 状态

| 状态 | 内容 | 重建 |
| --- | --- | --- |
| 节点表 | `分支 id → (TimelineInfo, TimelineContext?, 就绪结果, 是否已启动)`；已提交、未消费的创建同样占用节点 | 进程启动时由 `restore` 从分支目录重建 |
| `closing` | 关闭中，拒绝预留 | — |
| 清理错误 | 回滚与写失败事件时的错误，关闭时汇总 | — |

一个节点：

```text
预留（未启动）─ launch ─► 构建中 ─┬─ 发布 ─────────► 已发布（get/list 可见）
     │                            └─ 失败 / 关闭 ──► 移除（写 branch/create-failed，除非首输入已写）
     └─ 关闭 / 父分支卸载 ──► 移除（写 branch/create-failed{cancelled}）
```

## 生命周期

- 装载：提供服务；Session 随后调用 `create_main` 或 `restore`。
- 恢复：按父先子后，判定创建是否完成按[事件驱动约定·重启恢复](../event-driven-flows.md#42-创建与生命周期)；未完成的跳过它及其后代，父分支上已有 `branch/create-failed` 的创建不再恢复。恢复截点重新校验。
- 卸载：`close`，停止准入、取消未启动的创建、等待在建任务、关闭全部分支并汇总错误（[生命周期·stop 与 close](../lifecycle.md#5-stop-与-close)）。
- 不注入可重载的上游（[单树装配·重载](../single-tree.md#重载)）。

## 不变式

- 只有已发布的分支出现在 `TimelineTreeControl::get`/`list` 与 `TreeCommands::list`；失败候选从不公开。
- 每个 `branch/create` 最终要么有子分支的 `branch/start` 与首条 Queue 输入，要么在父分支上有一条 `branch/create-failed`。
- 继承范围由最终事件的 `at` 固定，不随消费时的父尾部扩展。

## 为什么

- 分成 Session 级目录与分支级订阅两半：目录持有所有分支、跨分支存活；`branch/create` 是父分支的事件，由父分支自己的订阅按序交给目录。首条输入与父分支上不写成功事实的理由见[事件驱动约定 §4.2](../event-driven-flows.md#42-创建与生命周期)。

## 代码与测试

- `crates/dim-agent-capabilities/src/timeline_tree.rs`（`TimelineTreePlugin`、`BranchTool`、`first_input`）
- `crates/dim-agent-capabilities/tests/branch_flow.rs`
- `tests/integration/tests/local_execution.rs`
