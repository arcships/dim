# Permission

按工具名与风险类别裁决一次工具调用：放行、询问或拒绝。不负责询问本身（[Approval](Approval.md)）、不执行工具、不知道具体工具的语义。

<!-- generated:plugin Permission — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`S:SessionConfig` |
| 提供 | `B:Permission`、`B:StateCell<PermissionState>` |
| 被注入于 | `Approval` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:SessionConfig` | `permission_policy` 是策略的初值 |
| `B:Timeline` | 写 `permission/changed`；装载时读历史重建 |
| `B:Dispatcher` | 订阅本分支 `permission/changed` |

## 服务接口

`B:Permission`

| 方法 | 语义 | 错误 |
| --- | --- | --- |
| `decide(name, risk) -> Decision` | 用当前已分发的策略裁决；同步、无副作用 | — |
| `update(expected, policy) -> version` | `expected` 等于最后写入的版本时写 `permission/changed`，等它被分发后返回新版本 | `Conflict { current }`、`Closed`、`Failed` |

`B:StateCell<PermissionState>`：`{ version, policy }`，只在分发时变化，供界面与协议读取。

调用方：`decide` 由 Approval 注入调用；`update` 由协议层经 `Session::service::<Permission>(branch)` 调用（`session.permission.update`）。

## 裁决规则

依次判断，先命中者生效：

1. `disallowed_tools` 含工具名 → 拒绝。
2. `allowed_tools` 含工具名 → 放行。
3. 按 preset 与 `ToolDefinition.risk`：

| risk | read-only | workspace-write | full-access |
| --- | --- | --- | --- |
| `read_filesystem` | 放行 | 放行 | 放行 |
| `write_filesystem` | 拒绝 | 放行 | 放行 |
| `execute_process` | 拒绝 | 询问 | 放行 |
| `network` | 拒绝 | 询问 | 放行 |
| `session` | 放行 | 放行 | 放行 |
| 未声明或未知 | 拒绝 | 询问 | 放行 |

`reason` 为英文句子，进入 `approval/request.reason` 或拒绝的工具结果。

## 事件

写出：

| 事件 | 时机 | 字段 |
| --- | --- | --- |
| `permission/changed` | `update` 通过版本检查 | `policy`：完整的新策略 |

消费：

| 事件 | 时机 | 作用 |
| --- | --- | --- |
| `permission/changed` | 提交后 | 更新 `StateCell`，`version` 取事件的本地序号；释放对应 `update` 的等待 |
| `permission/changed` | 装载时 | 重建：祖先前缀（`history(Some(0))`）只取策略，本分支 `local` 同时取策略与版本 |

## 状态

| 状态 | 位置 | 重建 |
| --- | --- | --- |
| 当前策略与版本 | `StateCell<PermissionState>` | `SessionConfig` 初值，折叠祖先前缀与本分支的 `permission/changed` |
| 最后写入的版本 `written` | 内部 | 装载时等于重建出的版本 |
| `update` 等待者 | 内部，按事件 ID | 不重建；卸载时丢弃，调用方得到 `Closed` |

`written` 在提交锁内推进，`version` 在分发时推进：两次 `update` 以写入顺序检查版本，`decide` 只看已分发的策略。

## 生命周期

- 装载：先订阅，闸门内重建，记 `rebuilt`，再提供服务。
- 卸载：标记关闭，丢弃等待者；之后的 `update` 返回 `Closed`。
- 子分支继承祖先前缀中的策略，但版本从 0 开始：版本只描述本分支自己的写入。

## 不变式

- `decide` 从不写事件，也不等待。
- 一个 `permission/changed` 至多释放一个 `update`。
- 提交失败时 `written` 回退到 `expected`，不留下未写入的版本。

## 为什么

- 策略折叠自本分支事件，而不是由 Session 广播：分支的权限是分支历史的一部分，重建与分支继承都不需要额外机制。
- 版本用本地序号而非计数器：历史即版本来源，不需要另存。
- `session` 风险在三个 preset 下都放行：它只改变 Agent 自身的会话状态（等待、分支、Skill），不触及工作区或外部。

## 代码与测试

- `crates/dim-agent-capabilities/src/permission.rs`
- `crates/dim-agent-capabilities/tests/permission_flow.rs`
