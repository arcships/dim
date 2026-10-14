# ModelProvider

分支的模型调用：消费 [ContextPreparation](ContextPreparation.md) 写的 `model/call`，经宿主 `ModelHost` 调用模型，把输出流写成 `step/*` 输出事件与 `model/usage`，最后写 `step/model-output-end`。不准备上下文、不决定 turn 是否继续（[Loop](Loop.md)）、不执行工具。

<!-- generated:plugin ModelProvider — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:ContextPreparation`、`B:Diagnostics`、`B:Dispatcher`、`B:Timeline`、`S:ModelHost` |
| 提供 | `B:ModelProvider`、`B:StateCell<ModelProgress>` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Timeline` | 读本分支历史重建；写输出事件与模型终态 |
| `B:Dispatcher` | 订阅 `model/call`、`stop/interrupt`、`step/model-start`、`input/applied`、`turn/start`、`turn/end`；调用任务出错时报告分支故障 |
| `B:ContextPreparation` | 调用前 `validate`：输入达到上下文窗口即失败，带预算事实供 Loop 恢复 |
| `S:ModelHost` | 宿主模型驱动：`invoke(request, connection, cancel)` 返回输出流 |
| `B:Diagnostics` | `model.invoke` 诊断 span，记录结果 |

上表是本地执行装配（`ModelProviderPlugin::new`）。网络装配用 `ModelProviderPlugin::process`：改为注入进程级 `ModelHost` 与 `ProviderConnectionResolver`，每次请求解析当前连接；连接带恢复时，401 且输出仍可重放则恢复连接重试一次，再次被拒时通知恢复方（[单树装配·进程层](../single-tree.md#进程层)）。

暂时性失败的有限重试也由当前调用任务负责，与 401 恢复共用请求次数预算；等待可取消，仅在输出仍可安全重放时重发。最终失败时调用连接的产品回调生成可选展示信息，随终态持久化。一次调用最多发送三次请求（含凭据恢复后的回放），凭据恢复最多一次；有 Retry-After 时不提前重发，无提示时按 0.5 秒、1 秒基准加抖动退避，累计等待不超过 60 秒；业务永久错误不重试。本能力不维护供应商错误映射表。

## 服务接口

`B:ModelProvider` 不对外提供方法；只为能力实例可经作用域键查找。

## 事件

写出（只在输出成为事实时写入，不逐个增量写）：

| 事件 | 时机 |
| --- | --- |
| `step/text` | 文本块结束（`TextEnd`）时写一个，带块内已攒的文字；块内累计达 4 KiB、或最早未写的文字已攒 1 秒（收到新片段时检查）时先写出已攒部分；写任何其他输出事件之前、`MessageStop` 时、输出被打断或失败时也先写出已攒部分 |
| `step/thinking` | 思考块结束（`ThinkingEnd`）时写一个，带整块内容；思考增量不写 |
| `step/tool-use-start` | 宿主报告调用开始（`ToolStart`）时；参数增量不写 |
| `step/tool-use` | 调用完整时；必须先有同一调用的 `step/tool-use-start`，否则按协议错误失败 |
| `step/model-content` | 流给出完整内容时；须与已提交的文字、思考与调用一致，否则失败 |
| `model/usage` | 流给出用量时；计费事实，被 Steer 取代的请求也写 |
| `step/tool-result`（无 `call_event_id`） | 输出未以 `Completed` 结束（stop、失败、卸载、装载结算）且本请求有已开始的调用时，与终态同一原子批次、排在其前；按开始顺序逐个列出错误结果：只开始的 `Tool was not executed: the model output ended before its arguments were complete`，已完整的 `Tool was not executed: the model output ended` |
| `step/model-output-end` | 流结束（`Completed`，带 finish reason）、失败（`Failed`，可带预算事实）、被中断（`Cancelled`，原因取中断原因）、被 Steer 取代（`Cancelled`，`replaced by steer`，没有输出也没有闭合）、卸载（`Cancelled`，`capability reloading`）；装载时为上一代未结束的调用补写（`Failed`，`capability reloading`） |

闭合与终态用 `append_batch_registered` 作为一个原子批次提交。

消费（提交后）：

| 事件 | 作用 |
| --- | --- |
| `model/call` | 依次判断：已卸载：报 `Closed`；已有当前请求：报 `concurrent model calls` 故障；中断标记存在：直接写 `Cancelled` 终态（中断原因），不调用；该请求的 `step/model-start` 之后已采用过驱动输入（Steer）：请求在上一持有者手中已被取代，直接写 `Cancelled`（`replaced by steer`），不调用；否则登记为当前请求并启动调用任务 |
| `step/model-start` | 记下最新请求，清除“其后已有 Steer”标记 |
| `input/applied` | Steer（驱动输入，Append 除外）：给最新请求记下“其后已有 Steer”；当前请求就是它、且没有中断标记时，当前请求若尚未收到任何可见输出（思考、文字、调用片段，含未写出的），即被取代：取消调用，宿主结束流后写 `Cancelled`（`replaced by steer`）；已收到输出则忽略，输出照常结束 |
| `stop/interrupt` | 记下中断标记；取消当前调用，以中断原因写 `Cancelled` |
| `turn/start`、`turn/end` | 清除中断标记 |

中断流程见[中断](../interrupt.md#1-流程与归属)。

## 状态

| 状态 | 内容 | 重建 |
| --- | --- | --- |
| 当前请求 | request id、取消令牌、输出闸门（未收到输出 / 已收到输出 / 已被取代，先到者定）；终态提交后才释放 | 不跨重载 |
| 最新请求 | 最后一次 `step/model-start` 的 request id，及其后是否采用过 Steer | 回放本分支事件 |
| 中断标记 | `stop/interrupt` 之后、`turn/start` 或 `turn/end` 之前 | 同上 |
| 已开始的调用 | 当前请求已提交 `step/tool-use-start` 的调用及是否已完整，按开始顺序 | 装载结算时从未结束调用的事件回放 |
| `B:StateCell<ModelProgress>` | 当前请求尚未发布的内容：已写入未发布的文字与思考块，之后是已攒未写的文字、进行中的思考块（来自思考增量；块结束时换成写入的整块）、已开始未完整的调用及其参数文本；`text_offset`、`thinking_offset` 是本请求 `step/text`、`step/thinking` 增量按序拼接后排在 `text`、`thinking` 之前的 Unicode 标量数，请求开始时为 0，已发布的部分移出时加上其标量数。写入的部分等对应事件发布后才移除（读流时同时等待发布水位）；请求写完终态后等其事件全部发布再清空，卸载时直接清空 | 不重建（I/O 状态） |

`ModelProgress` 每个片段更新一次（`watch` 原地修改并通知）；订阅方每次读取都会复制整个快照，长思考块的复制量随块长增长，需要节流的由订阅方处理。

一次调用：

```text
model/call ─┬─ 已卸载 ───────────────────► Closed
            ├─ 已有当前请求 ─────────────► concurrent model calls 故障
            ├─ 中断标记 ─────────────────► model-output-end{Cancelled}
            ├─ model-start 之后已有 Steer ─► model-output-end{Cancelled: replaced by steer}
            └─ 调用中（validate → invoke → 输出事件）
                 ├─ 流结束 ──────────────► model-output-end{Completed}
                 ├─ 失败 ────────────────► [tool-result 闭合] + model-output-end{Failed}
                 ├─ stop/interrupt ──────► [tool-result 闭合] + model-output-end{Cancelled}
                 ├─ Steer，尚未收到输出 ──► model-output-end{Cancelled: replaced by steer}
                 └─ 卸载 ────────────────► [tool-result 闭合] + model-output-end{Cancelled: capability reloading}
装载时: model/call 无终态 ──► [tool-result 闭合] + model-output-end{Failed: capability reloading}
```

## 生命周期

- 装载：先订阅，在闸门内重建中断标记与最新请求，为上一代已提交而无终态的 `model/call` 补写闭合与 `Failed`；记下 `rebuilt`，再提供服务（[能力重载协议](../single-tree.md#能力重载协议)）。
- 重载来源：`ModelHost`；进程 Aimux 的 `ModelHost` 换代沿依赖重载本能力，由本能力结算自己的调用（[单树装配·重载](../single-tree.md#重载)）。
- 卸载：标记关闭，取消当前调用并等待任务结束；宿主须以结束输出流确认取消。

## 不变式

- 每个 `model/call` 恰有一个 `step/model-output-end`。
- 调用任务的最终复核（是否已被取代、是否卸载）、终态提交（闭合与 `step/model-output-end`）与释放当前请求在状态锁内一步完成，Steer 的处理器也取这把锁：Steer 要么在这一步之前被消费、取代尚未收到输出的请求，要么在之后被消费、找不到当前请求，不会在中间被消费却不影响终态（[总纲](../time-bus.md)第 4 条）。提交在锁内同步完成，锁内不等待任何异步操作；提交前钩子不取本能力的状态。
- 被 Steer 取代的请求没有输出事件：取代只在请求尚未收到任何可见输出时成立，之后宿主仍送出的块不再写入；它的终态之后才有同一 turn 的下一个 `step/model-start`。
- 每个 `step/tool-use` 之前有同一调用的 `step/tool-use-start`；输出未以 `Completed` 结束时，每个已开始的调用都在终态的批次里闭合。
- `step/model-content` 与同一请求已提交的输出一致。

## 为什么

- 被取代的请求由持有者写终态，Loop 看到终态再开新请求，而不是由 Loop 直接开下一个请求。判断只依据本能力消费到的事件和自己收到的输出，不读 Loop 的状态。
- 调用前校验窗口放在 ContextPreparation：预算估算与压缩同属上下文策略，失败结果带预算事实，由 Loop 决定是否以恢复请求重试。

## 代码与测试

- `crates/dim-agent-capabilities/src/model_provider.rs`
- `crates/dim-agent-capabilities/src/output.rs`（输出流到事件、进度与闭合）
- `crates/dim-agent-capabilities/tests/context_flow.rs`
- `crates/dim-agent-capabilities/tests/host_flows.rs`（连接解析与恢复；按块写入与进度）
- `crates/dim-agent-capabilities/tests/interrupt_flow.rs`（stop 时的闭合；被 Steer 取代的请求与宿主的迟到输出；首个输出之后才消费的 Steer；调用失败与终态之间不消费 Steer）
