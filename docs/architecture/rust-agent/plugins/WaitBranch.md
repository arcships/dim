# WaitBranch

分支的等待状态：记录本分支上到达的后台完成通知与回合中的插话，供 `wait` 工具取用。不写事件、不唤醒 Loop（唤醒由 [BackgroundExec](BackgroundExec.md) 投递输入完成）。

<!-- generated:plugin WaitBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:Wait` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Dispatcher` | 订阅 `input/message`、`turn/start`、`turn/end` |
| `B:Timeline` | 装载时从历史恢复回合是否在进行 |

## 服务接口

`B:Wait`

| 方法 | 语义 | 错误 |
| --- | --- | --- |
| `wait(task_id, timeout, cancel) -> WaitResult` | 先取任一插话，再取与 `task_id` 匹配（未给则任意）的完成通知；都没有则等下一次到达、超时或取消 | `AlreadyWaiting` |

取用规则：插话优先，完成通知按 `task_id` 过滤。

## 运行时调用方

| 调用方 | 方式 |
| --- | --- |
| [Wait](Wait.md) 工具 | `TreeCommands.get(context.branch_id)?.service::<Wait>()` |

## 事件

消费（均为提交后）：

| 事件 | 作用 |
| --- | --- |
| `turn/start` | 回合进行中 |
| `input/message` 带唤醒标签 `exec-wake-task:{任务 ID}` 且回合进行中 | 记一次完成到达（任务 ID） |
| `input/message` 为 Steer 且回合进行中 | 记一次插话到达；开启回合的输入在 `turn/start` 之前，不算插话 |
| `turn/end` | 回合结束，清空未取用的到达 |

## 状态

| 状态 | 位置 | 重建 |
| --- | --- | --- |
| 回合是否在进行 | 内部，只由自己消费的 `turn/start`、`turn/end` 决定，不读 Loop 状态 | 先订阅，再在闸门内读历史：最后一个回合事件是 `turn/start` 即在进行 |
| 未取用的到达（按到达顺序） | 内部 | 不重建 |
| 是否有 `wait` 在进行 | 内部 | — |

到达只在回合进行中登记、在本回合内有效：`turn/end` 清空。空闲（含停止之后）时到达的通知不登记：它开启或进入下一回合，由模型在输入中看到，不会结束之后的 `wait`。

## 生命周期

装载时订阅并提供服务；卸载时丢弃状态，进行中的 `wait` 随调用方令牌结束。

## 不变式

- 同一分支同时至多一个 `wait`。
- 每个到达至多被一次 `wait` 取用。

## 为什么

- 从输入事件观察到达，而不是由 BackgroundExec 直接通知：完成与插话本来就以输入进入分支，同一来源保证顺序一致，也不需要反向注册。
- `turn/end` 清空：上一回合遗留的到达不应让下一回合的 `wait` 立即返回。

## 代码与测试

- `crates/dim-agent-capabilities/src/wait.rs`
- `tests/integration/tests/local_execution.rs`（`wait_*`）
