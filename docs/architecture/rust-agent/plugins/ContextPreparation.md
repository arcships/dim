# ContextPreparation

分支的上下文准备：消费 Loop 写的 `context/prepare`，经宿主准备请求、按上下文策略记预算与压缩，写 `model/call` 交给 [ModelProvider](ModelProvider.md)，失败时写模型终态；另提供本分支配置的原子发布者。不构造请求（[Loop](Loop.md)）、不调用模型（ModelProvider）、不加载提示词（Prompt，见[提示词生命周期](../prompt.md)）。

<!-- generated:plugin ContextPreparation — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Diagnostics`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:ContextModelHost`、`S:HistoryRenderer`、`S:PrepareContextHost` |
| 提供 | `B:ConfigurationPublisher`、`B:ContextPreparation` |
| 被注入于 | `ModelProvider`、`Prompt` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Timeline` | 读请求前缀与压缩、预算事实；写 `context/budget`、`context/compaction`、`model/call`、`step/model-output-end`；发布者原子批量写配置 |
| `B:Dispatcher` | 订阅 `context/prepare`、`stop/interrupt`、`step/model-start`、`input/applied`、`turn/start`、`turn/end`；准备任务出错时报告分支故障 |
| `S:PrepareContextHost` | 宿主准备请求；只能在历史前缀之后追加消息 |
| `S:ContextModelHost` | 可选：有解析器时启用上下文策略（预算、压缩、检查点、窗口校验），否则原样透传 |
| `S:HistoryRenderer` | 把固定前缀渲染为模型消息，用于前缀校验与策略投影 |
| `B:Diagnostics` | `context.prepare` 诊断 span，记录结果（returned / interrupted / failure / defect；被 Steer 取代记为 interrupted） |
| `B:TimelineIdentity` | 发布者写入事件的分支身份、校验提示词作用域可见性 |

## 服务接口

| 服务 | 方法 | 语义 | 调用方 |
| --- | --- | --- | --- |
| `B:ContextPreparation` | `validate(request, cancel)` | 有上下文策略时估算输入，达到上下文窗口即返回带预算事实的失败；无策略直接通过 | [ModelProvider](ModelProvider.md)，每次模型调用前 |
| `B:ConfigurationPublisher` | `publish(changes, model, tools, inputs, register)` | 把 `prompt/changed`、每项一条的 Append 输入、`context/configured` 作为一个批次原子提交并等到发布；模型部分与工具部分分别替换，另一部分沿用；没有变化不写 | Prompt |

`publish` 的错误：作用域不在本分支祖先链内（`InvalidInput`）、作用域已结束（`Cancelled`）、system 条目不是分支作用域、只采用工具部分却没有模型配置、system 变化而没有配置。`B:ConfigurationPublisher` 在订阅与重建之前提供。

## 事件

写出：

| 事件 | 时机 |
| --- | --- |
| `context/budget` | 有策略时，每次准备估算后 |
| `context/compaction` | 有策略且估算达到阈值（本 turn 同一边界未检查过）或预算恢复时；宿主已完成的压缩即使随后被取消也提交 |
| `model/call` | 准备成功，携带最终请求、配置与提示词版本 |
| `step/model-output-end` | 准备失败（`Failed`）、被中断（`Cancelled`，原因取中断原因）、被 Steer 取代（`Cancelled`，`replaced by steer`）、卸载（`Cancelled`，`capability reloading`）；装载时为上一代未交接的准备补写（`Failed`，`capability reloading`） |
| `prompt/changed`、`input/message`（Append）、`context/configured` | 经 `ConfigurationPublisher::publish`，由 Prompt 驱动 |

消费（提交后）：

| 事件 | 作用 |
| --- | --- |
| `context/prepare` | 依次判断：已卸载：报 `Closed`；已有当前请求：报 `concurrent preparation` 故障；中断标记存在：直接写 `Cancelled` 终态（中断原因），不执行；该请求的 `step/model-start` 之后已采用过驱动输入（Steer）：请求在 Loop 手中已被取代，直接写 `Cancelled`（`replaced by steer`），不执行；否则登记为当前请求并启动准备任务 |
| `step/model-start` | 记下最新请求，清除“其后已有 Steer”标记 |
| `input/applied` | Steer（驱动输入，Append 除外）：给最新请求记下“其后已有 Steer”；当前请求就是它、且没有中断标记时即被取代：取消准备，写 `Cancelled`（`replaced by steer`），不交出 `model/call`；取消前宿主已完成的压缩仍会提交，并按压缩结果生效。已写出 `model/call` 的请求归 ModelProvider，本能力不再处理 |
| `stop/interrupt` | 记下中断标记；取消当前准备，以中断原因写 `Cancelled` |
| `turn/start`、`turn/end` | 清除中断标记 |

中断流程见[中断](../interrupt.md#1-流程与归属)。

## 状态

| 状态 | 内容 | 重建 |
| --- | --- | --- |
| 当前请求 | `(request id, 取消令牌)`；结束时按 Steer 标记判断是否已被取代，终态或 `model/call` 提交后才释放 | 不跨重载 |
| 最新请求 | 最后一次 `step/model-start` 的 request id，及其后是否采用过 Steer | 回放本分支事件 |
| 中断标记 | `stop/interrupt` 之后、`turn/start` 或 `turn/end` 之前 | 同上 |
| 准备缓存 | 同一 turn、同一 `through` 的宿主准备结果，预算恢复重试时复用 | 不跨重载 |

一次准备：

```text
context/prepare ─┬─ 已卸载 ───────────────────────► Closed
                 ├─ 已有当前请求 ─────────────────► concurrent preparation 故障
                 ├─ 中断标记 ─────────────────────► model-output-end{Cancelled}
                 ├─ model-start 之后已有 Steer ───► model-output-end{Cancelled: replaced by steer}
                 └─ 准备中 ─┬─ 成功 ──────────────► model/call
                            ├─ 失败 ──────────────► model-output-end{Failed}
                            ├─ stop/interrupt ────► model-output-end{Cancelled}
                            ├─ Steer ─────────────► model-output-end{Cancelled: replaced by steer}
                            └─ 卸载 ──────────────► model-output-end{Cancelled: capability reloading}
装载时: context/prepare 无后继 model/call 或终态 ──► model-output-end{Failed: capability reloading}
```

## 生命周期

- 装载：提供发布者，先订阅，在闸门内重建中断标记与最新请求，为上一代留下的准备补写 `Failed`（不重放准备或压缩）；记下 `rebuilt`，再提供 `B:ContextPreparation`（[能力重载协议](../single-tree.md#能力重载协议)）。
- 重载来源：`PrepareContextHost`、`ContextModelHost`（[单树装配·重载](../single-tree.md#重载)）。
- 卸载：标记关闭，取消当前准备并等待任务结束；被取消的准备写 `Cancelled{capability reloading}`。已写出 `model/call` 的请求归 ModelProvider。

## 不变式

- 每个 `context/prepare` 恰有一个后继：`model/call` 或模型终态。
- 准备任务按 Steer 标记的最终复核、后继提交（`model/call` 或终态）与释放当前请求在状态锁内一步完成，Steer 的处理器也取这把锁：Steer 要么在这一步之前被消费、使请求以 `replaced by steer` 结束，要么在之后被消费、找不到当前请求（[总纲](../time-bus.md)第 4 条）。提交在锁内同步完成。
- 被 Steer 取代的请求以 `Cancelled` 结束、不交出 `model/call`；取消前宿主已完成的压缩仍提交，并按压缩结果生效。
- 宿主准备的结果保留固定历史前缀，只能追加消息。

## 为什么

- 被取代的请求由持有者写终态，Loop 看到终态再开新请求，而不是由 Loop 直接开下一个请求：被取代的请求总有终态，重建与投影不必为“无终态的请求”保留特例。
- 配置发布放在这里：配置与上下文准备同属请求前缀的事实。Prompt 与 ToolBinding（经 `Prompt::adopt_tools`）共用这一个发布者，各自的发布是一个原子批次。

## 代码与测试

- `crates/dim-agent-capabilities/src/context_preparation.rs`
- `crates/dim-agent-capabilities/src/context_policy.rs`
- `crates/dim-agent-capabilities/tests/context_flow.rs`（Steer 取代未输出的请求）
- `crates/dim-agent-capabilities/tests/interrupt_flow.rs`（准备中的 Steer；准备失败与终态之间不消费 Steer）
