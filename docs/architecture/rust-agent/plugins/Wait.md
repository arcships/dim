# Wait

Session 贡献 `wait` 工具：让模型等到后台命令完成、用户插话或超时。定义、参数与返回文案见[服务接口](#服务接口)。等待的状态属于分支，见 [WaitBranch](WaitBranch.md)；本 Plugin 只提供工具并把调用转给调用方分支。

<!-- generated:plugin Wait — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:TreeCommands` |
| 提供 | `S:ToolContribution#wait` |
| 被注入于 | `ToolRegistry` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:TreeCommands` | 每次调用按 `context.branch_id` 取调用方分支的 `B:Wait` |

## 服务接口

`S:ToolContribution#wait`：`risk: session`，来源 `extension`。

| 参数 | 规则 |
| --- | --- |
| `taskId` | 可选字符串；给出时只接受该任务的完成 |
| `timeoutMs` | 可选正整数，默认且最大 300,000 |

返回为工具结果（不写其他事件），`content` 与 `structuredContent.reason`：

| 结果 | `content` | `reason` |
| --- | --- | --- |
| 后台命令完成 | `wait ended: notification (<taskId>)` | `notification`，带 `taskIds` |
| 用户插话 | `wait ended: user steer` | `user_steer` |
| 超时 | `wait ended: timeout` | `timeout` |
| 被中断 | `wait ended: interrupted` | `interrupted` |

都带 `waitedMs`。同一分支已有 `wait` 在进行时返回错误 `wait is already running in this session.`；参数不合法返回对应错误文案。

命令结果本身不在 `wait` 的返回里：它由 [BackgroundExec](BackgroundExec.md) 作为通知输入写入分支，模型从那条输入读到结果。

## 生命周期

装载时提供工具贡献；分支的 `B:Wait` 重载中时，调用以重载取消结算。

## 为什么

- 工具在 Session、状态在分支：工具集合是 Session 固定的，等待与到达只对调用方分支有意义。
- 用 `wait` 而不是专门的任务查询工具：模型只需要「等到有结果」，结果本身经输入送达。

## 代码与测试

- `crates/dim-agent-capabilities/src/wait.rs`
- `tests/integration/tests/local_execution.rs`（`wait_*`）
