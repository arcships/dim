# Loop

> 部分文档：只写本地执行相关的工具声明与绑定。回合、输入与中断见[事件驱动约定](../event-driven-flows.md)与[中断](../interrupt.md)：回合的执行状态由 Loop 持有，分支没有暂停态。

分支的执行循环：取输入、构造模型请求、推进回合。

<!-- generated:plugin Loop — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline`、`B:ToolBinding`、`S:ContextModelHost`、`S:HistoryRenderer`、`S:PrepareModelHost` |
| 提供 | `B:AgentLoop`、`B:StateCell<LoopSnapshot>`、`B:dyn LoopState` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:ToolBinding` | 构造请求时固定本请求的工具；预算恢复时转移；调用结算或回合结束时释放 |
| `S:PrepareModelHost` | 没有已采用配置时准备模型设置 |
| `S:HistoryRenderer` | 把上下文前缀渲染为模型消息 |
| `S:ContextModelHost` | 是否启用恢复路径 |
| `B:Timeline` | 读上下文前缀；写回合事件 |
| `B:Dispatcher` | 订阅 `input/applied`、`input/consumed`、中断、回合、模型与工具事件 |

## 输入的消费

Loop 不读 InputQueue：待消费集合由它自己消费的 `input/applied`（带模式）与 `input/consumed` 算出，重建时回放。何时采用由 InputQueue 决定，何时消费由 Loop 按模式决定：

| 模式 | 消费 |
| --- | --- |
| Steer | 驱动：空闲时开 turn；turn 进行中按插入规则插队，请求还没有模型输出时由其持有者以 `Cancelled` 结束、Loop 再发下一个请求（见下节） |
| Queue | 驱动：采用时没有 turn 在跑，开 turn |
| Append | 不驱动：下一次构建请求时一并消费 |

中断之前采用的输入不再驱动 turn，只随下一个 turn 一并消费。

## Steer 与请求取代

被取代的请求先终止，Loop 看到它的终态再开新请求。是否取代由 Steer 被消费时持有该请求的状态机决定，各自只看自己消费到的事件和自己的工作：

| 持有者 | 持有时段 | Steer 到达时 |
| --- | --- | --- |
| Loop | 写 `step/model-start` 起，到准备任务写出 `context/prepare` 为止 | `step/model-start` 尚未分发：分发时不再准备，直接写 `Cancelled`（`replaced by steer`）；准备任务在读上下文：取消它，任务在 turn 锁内改写 `Cancelled`，不写 `context/prepare`。任务已写出 `context/prepare` 后，Loop 不再处理 |
| [ContextPreparation](ContextPreparation.md) | `context/prepare` 到 `model/call` | 取消准备，写 `Cancelled` |
| [ModelProvider](ModelProvider.md) | `model/call` 到终态 | 还没收到任何可见输出：取消调用，写 `Cancelled`；已收到：忽略，输出照常结束 |

交接时序由序号决定：Steer 排在请求的 `step/model-start` 之后、却在下一持有者接手之前被消费时，交出方可能已经交出；接手方收到交接事件时，若该请求的 `step/model-start` 之后已采用过 Steer，直接写 `Cancelled`，不再执行。排在 `step/model-start` 之后的 Steer 不在该请求里（请求消费的输入都在它的 `step/model-start` 之前采用），所以这样判断不会误取消。

Loop 等当前请求的终态：

| 终态 | 处理 |
| --- | --- |
| `Cancelled`，turn 未被中断，原因不是 `capability reloading`，有待消费的驱动输入 | 释放本请求的绑定，在同一 turn 消费 Steer 发新请求 |
| `Cancelled`，turn 已被中断 | 以中断原因结束 turn |
| `Cancelled`，`capability reloading` | turn 失败并结束 |
| `Completed` | 按插入规则：无工具调用时在同一 turn 消费 Steer 发新请求；有调用时随这批结果进入下一次请求 |

Loop 不读 ContextPreparation、ModelProvider 的内存状态，也不订阅 `step/text`、`step/thinking`。

## 闭合未分发的调用

输出以 `Completed` 结束、其调用却不会分发（turn 已被中断，或因重载失败）时，Loop 写 `turn/end` 时在同一原子批次、排在其前写一个不带 `call_event_id` 的 `step/tool-result`，按 `step/tool-use-start` 的顺序为每个调用给出错误结果 `Tool was not executed: <turn 结束原因>`。不带 `call_event_id` 的结果不对应任何分发，Loop 收到后忽略（包括 ModelProvider 在非 `Completed` 终态前写的闭合）。

## 工具声明与绑定

构造模型请求时，本请求的工具声明与实现由 [ToolBinding](ToolBinding.md) 一起固定：

```text
已采用配置有工具部分 ──> bind(request, through, settings.tools, settings.tool_sources)，声明 settings.tools
已采用配置无工具部分 ──> bind_current(request, None)，声明注册表当前的工具
没有已采用配置       ──> PrepareModelHost ──> bind_current(request, 它给出的声明)
绑定失败 ──> step/model-output-end(Failed)
```

释放：

| 时机 | 调用 |
| --- | --- |
| 分发 `step/tool-result`（本请求的调用全部结算） | `release(request)` |
| 请求被 Steer 取代后以 `Cancelled` 结束，或输出没有工具调用而 turn 带着 Steer 继续 | `release(request)` |
| 预算恢复重试同一请求 | `transfer(原请求, 新请求)`，不重新选择工具 |
| 分发 `turn/end` | `retain_only(None)`：回合结束时的请求一律释放 |
| Loop 单独重载后重建 | `retain_only(当前请求)` |

## 为什么

- 绑定放在请求准备里：只有 Loop 知道请求何时开始、何时重试、何时结束。
- 回合结束统一释放，而不是在每种结束分支里各释放一次：结束的路径多（完成、失败、取消、重载），一处兜底不会漏。

## 代码与测试

- `crates/dim-agent-capabilities/src/agent_loop.rs`
- `crates/dim-agent-capabilities/src/steer.rs`（ContextPreparation 与 ModelProvider 共用的 Steer 标记）
- `tests/integration/tests/local_product.rs`（声明恰好 8 个工具）
