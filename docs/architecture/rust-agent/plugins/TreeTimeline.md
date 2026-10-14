# TreeTimeline

[TimelineTree](TimelineTree.md) 的分支半边：订阅本分支的 `branch/create`，交给 Session 的分支目录启动构建；本分支卸载时取消以它为父、尚未启动的创建。不构建分支、不持有分支实例。创建流程见[事件驱动约定·分支创建](../event-driven-flows.md#42-创建与生命周期)。

<!-- generated:plugin TreeTimeline — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Timeline` | 构建子分支时取本分支的祖先链与固定前缀 |
| `B:Dispatcher` | 订阅 `branch/create` |
| `S:TreeCommands` | 弱引用分支目录：转交创建、卸载时取消未启动的创建 |
| `B:TimelineIdentity` | 本分支 id，作为子分支的父 |

## 事件

消费：

| 事件 | 时机 | 作用 |
| --- | --- | --- |
| `branch/create` | 提交后 | 目录中该分支身份已预留且未启动时启动构建（`branch/start` 带 `parent` 与 `createEvent`）；目录关闭中、身份未预留（上一代卸载时已结算）或已启动则忽略。处理器只登记构建任务，不等待构建 |

不写事件；`branch/create-failed` 由 TimelineTree 写入。

## 生命周期

- 装载：订阅 `branch/create`。不重建状态：未消费的创建在目录里以未启动节点存在，由下一次分发启动。
- 卸载：取消以本分支为父、尚未启动的创建，向本分支写 `branch/create-failed{cancelled}`，结束等待方。已启动的构建归 TimelineTree。

## 为什么

- 创建事件经父分支自己的 Dispatcher 按序消费：子分支的继承位置由已提交的最终事件决定，构建不早于事件可见。

## 代码与测试

- `crates/dim-agent-capabilities/src/timeline_tree.rs`（`TreeTimelinePlugin`）
- `crates/dim-agent-capabilities/tests/branch_flow.rs`
