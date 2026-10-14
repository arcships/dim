# InputQueue

分支输入的采用时机：消费 `input/message`，按模式决定何时写 `input/applied`（带模式），并发布本分支已提交、未消费的输入数。不校验、不写入外部输入（`Session::input` / `Branch::input` 在边界校验后直接写 Timeline），不决定消费（[Loop](Loop.md)）。

<!-- generated:plugin InputQueue — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:InputQueue`、`B:StateCell<InputSnapshot>` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Timeline` | 写 `input/applied`；装载时读本分支历史 |
| `B:Dispatcher` | 订阅 `input/message`、`input/consumed`、`turn/start`、`turn/end`、`stop/interrupt` |

## 服务接口

| 服务 | 内容 |
| --- | --- |
| `B:StateCell<InputSnapshot>` | `queued`：已提交、尚未被 `input/consumed` 的输入数，**含已采用、未消费的**。外部经 `Session::get::<InputSnapshot>(branch)` / `on` 只读 |
| `B:InputQueue` | 不对外提供方法；只为能力实例可经作用域键查找 |

## 事件

写出：

| 事件 | 时机 | 字段 |
| --- | --- | --- |
| `input/applied` | 采用一条输入时（见下表）；装载时补写上一代已决定、未写出的 | `inputId`、`mode`（输入的实际模式） |

消费（提交后，按分支内顺序）：

| 事件 | 作用 |
| --- | --- |
| `input/message` | 加入条目；按模式决定是否立即采用 |
| `turn/start` | `turn_open = true`，清除 `starting` |
| `turn/end`（任何结果，含 cancelled） | `turn_open = false`；采用最早一条被压住的 Queue |
| `stop/interrupt` | 清除 `starting`：中断前采用的输入不再开 turn，不再等它的 `turn/start` |
| `input/consumed` | 移除条目 |

采用规则：

| 模式 | 采用 |
| --- | --- |
| Steer、Append | 分发到 `input/message` 即采用 |
| Queue | 没有 turn 在跑（`turn_open`）、没有 turn 正要开始（`starting`）、也没有更早的 Queue 被压住时立即采用；否则压住，`turn/end` 分发后采用最早一条，一个 turn 一条 |

## 状态

| 状态 | 内容 | 重建 |
| --- | --- | --- |
| 条目 | `[(input id, mode, 是否已采用)]`，从 `input/message` 到 `input/consumed` | 装载时回放本分支事件 |
| `turn_open` | 处于已分发的 `turn/start` 与其 `turn/end` 之间 | 同上 |
| `starting` | 没有 turn 时采用过一条 Steer 或 Queue，对应 `turn/start` 尚未分发；`stop/interrupt` 清除 | 同上 |

一条输入：

```text
input/message ─┬─ Steer / Append ───────────────────────────► 已采用（写 input/applied）
               └─ Queue ─┬─ 空闲且无 starting、无更早 Queue ──► 已采用
                         └─ 否则 ─► 压住 ─ turn/end（轮到它）─► 已采用
已采用 ─ input/consumed（Loop 写）─► 移除
```

## 生命周期

- 装载：先订阅，在闸门内固定 `S = committed()`，用 `local(..=S)` 重放同一组决定；上一代已决定、未写出 `input/applied` 且仍未消费的输入，由新一代补写；记下 `rebuilt` 序号，处理器跳过不大于它的事件，然后提供服务（[能力重载协议](../single-tree.md#能力重载协议)）。
- 重载期间写入的输入不经任何能力端口，留在 Timeline 上由新一代接收（[单树装配·重载](../single-tree.md#重载)）。
- 没有在途操作，卸载不写事件。

## 不变式

- 采用决定只依据自己消费的事件，不读 Loop。
- 每条输入至多一条 `input/applied`；Queue 之间按提交顺序采用，一个 turn 一条。
- stop 不撤销被压住的 Queue：被中断的 turn 结束后照常采用（[中断](../interrupt.md#1-流程与归属)）。

## 为什么

- 采用归 InputQueue、消费归 Loop：Loop 的决定不依赖另一个状态机的内存与订阅顺序。
- `input/applied` 带模式：Loop 只看采用与消费两个事件即可决定，不订阅 `input/message`。
- Append 不压住：turn 进行中写入的状态（如规则变化）要赶上同一 turn 后面的请求。

## 代码与测试

- `crates/dim-agent-capabilities/src/input_queue.rs`
- `crates/dim-agent-capabilities/tests/flows.rs`（stop 后被压住的 Queue 照常运行）
- `crates/dim-agent-capabilities/tests/host_flows.rs`（turn 进行中 Steer、Append 采用而 Queue 等待）
- `crates/dim-agent-capabilities/tests/context_flow.rs`（恢复时补写采用，每条 Queue 各占一个 turn）
- `crates/dim-agent-capabilities/tests/interrupt_flow.rs`（中断前采用的输入不驱动 turn）
