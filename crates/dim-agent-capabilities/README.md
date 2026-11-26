# 默认 Agent 能力

此包实现业务能力；`dim-agent-runtime` 不依赖此包。Each capability uses rutis `TypedPlugin` and the shared `Capability` contract, declares dependencies once, reads scoped keys and registers owned resources with `ctx.effect`.业务事件只经过 Timeline 和有序 Dispatcher。

```rust,no_run
use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig, CreateSessionOptions, SessionConfig};
use dim_agent_types::InputMode;

# async fn example() -> dim_agent_runtime::Result<()> {
let mut builder = AgentBuilder::new(AgentConfig::default());
defaults::mock_hosts(&mut builder);
defaults::install(&mut builder);
let agent = builder.build();
let session = agent.create_session(SessionConfig::default(), CreateSessionOptions::default()).await?;
session.submit("hello".into(), InputMode::Queue, "main").await?;
session.idle("main").await?;
agent.close().await?;
# Ok(())
# }
```

## 模块

| 模块 | 责任与状态归属 |
| --- | --- |
| `input_queue` | 决定何时采用：消费 `input/message` 与 turn 边界、`stop/interrupt`，按模式写带 `mode` 的 `input/applied`（Steer、Append 立即，Queue 在没有 turn 运行或将要开始、也没有更早的 Queue 被压住时立即，否则 turn 结束后一个 turn 一条）；发布已提交、尚未消费的输入数 `InputSnapshot`；不拦截写入 |
| `agent_loop` | 执行状态（`LoopSnapshot`：phase、turn）；由 `input/applied`、`input/consumed` 自建待消费集合，决定何时消费并写 `input/consumed`；turn / request 推进与一次预算恢复 |
| `context_preparation` / `context_policy` | 固定来源准备、历史前缀、usage 基线、压缩与最终请求预算校验 |
| `model_provider` / `output` | 最终 `model/call` 调用、模型流校验；协议错误后取消并排空宿主流 |
| `tool_executor` | 有界并发调用、逐项真实结果、原调用顺序结算 |
| `tool_registry` | 普通工具表；branch 与宿主工具使用同一接口 |
| `timeline_tree` | 分支构建登记、独立根、固定祖先前缀与关闭；`TreeTimelinePlugin` 消费最终 branch/create 后启动树拥有的构建任务 |
| `history` | 固定事件切片的纯转换；不保存另一份累计消息历史 |
| `host` | 可替换的模型、工具、准备、token 计数与压缩接口 |
| `defaults` / `adapters` | 默认安装与确定性测试宿主 |

默认安装同时用于 main 和所有新分支，包含 `TreeTimelinePlugin`。Session 服务通过运行时显式桥接到 Timeline 根。`InputSnapshot`、`LoopSnapshot` 使用同一 `StateCell` 提供 `Session::get/on`。

## 宿主与取消契约

- `defaults::install` 只安装能力。`mock_hosts` 另行安装全部测试宿主；App Server 可使用 `mock_model_hosts`；Permission 与分支级 Approval 随 `defaults::install` 安装。
- `PrepareModel` 返回不含消息的 `ModelSettings`，Loop 填入固定历史。可选准备允许附加消息；预算策略启用时必须保留原始历史前缀。
- `HistoryRenderer` 是可替换的纯函数端口，接收固定事件切片并返回 `HistoryEntry`。Loop 和上下文预算/压缩使用同一函数和同一 `through` 来源；函数必须确定性、保持条目标识稳定，不持有缓存或另一份事实源。`mock_model_hosts_with_history` 可显式传入跨包实现；完全自定义宿主需绑定此端口。
- `ModelDriver::invoke` 返回流。取消后宿主必须完成真实清理并结束流；出现协议错误时能力发送取消信号，继续读至 EOF 后才结算。宿主不得通过丢弃内部任务句柄假称清理完成。
- `ToolDriver::execute` 返回真实原生结果。成功先于取消完成时保留成功；工具异常转换为对应工具结果。构造或轮询 panic 触发运行时故障，但其他已开始的工具仍完成收尾。
- 模型流或工具不响应合作取消时，关闭持续等待其真实结束；此包不强行丢弃原生操作来生成完成状态。

## 中断

`stop/interrupt` 与业务事件共用 Timeline 和 Dispatcher。ContextPreparation、ModelProvider、ToolExecutor 各自订阅，立即取消自己的当前操作，记下“当前 turn 已中断”后返回；不在 handler 中等待宿主清理，宿主收尾后才写取消终态。之后收到的仍属于被中断 turn 的请求（Loop 尚未处理到中断时写出）直接写入取消终态，不创建任务；被中断 turn 的 `turn/end` 或下一个 `turn/start` 清除该记录。Loop 把当前 turn 标记为中断，不再发出步骤请求，已有请求结算后以 cancelled 结束轮次，因此 turn 闭合即本轮收尾。分支没有暂停状态：Loop 记下最后一次中断的序号，序号更早的已采用输入不再驱动 turn，由下一个 turn 一并消费；被 InputQueue 压住的 Queue 在被中断的 turn 结束后照常采用并开下一个 turn。同一 turn 出现新的 `step/model-start` 时，ContextPreparation 与 ModelProvider 静默停下被取代的旧请求，不为它写终态。

发起脱离 turn 的异步工作的能力自行负责执行、取消、结果与唤醒，以自身事件记录结果，不设共享的起止事件；分支在途工作的识别暂不设计；turn 范围中断不取消它，分支关闭时取消。唤醒以 steer 输入提交，与其他输入一样处理。

## 准入与资源

外部输入经分支门面 `Branch::input` 在边界校验后写入，能力产生的输入由产生方保证合法后直接追加；InputQueue 只消费 `input/*`，不拦截写入；是否开始 turn 由 Loop 分发输入时决定；输入不设容量额度；单批工具调用全部并发执行，每 Session 的分支数不设上限。

能力不申请共享执行或工具许可，模型、上下文准备、提示词加载和工具批次直接调用宿主。各能力直接持有当前操作的取消句柄与 Tokio TaskTracker，在 rutis 登记自己的取消和等待。

## 测试宿主

普通输入得到 `Echo: ...`。以下输入经过同一模型、工具和结果事件流程：

```text
tool:echo {"text":"hello"}
tool:wait {"millis":1000,"text":"done"}
tool:branch {"prompt":"child"}
```

工具列表由 `SessionConfig.tools` 决定。默认 mock 工具只有无文件副作用的 echo / wait，branch 为已实现的内置普通处理器。真实模型、文件工具、持久化和认证不在此包原型范围。

`adapters::MAX_MOCK_OUTPUT_BYTES` 将模型文本（包括 `Echo: ` / `Tool result: ` 前缀）、序列化工具调用以及 echo / wait 的结果文本分别限制为 1 MiB。超限返回明确宿主错误；wait 在等待前检查。此上限不包含外层事件、文本 JSON 转义的开销，也不替代运行时日志预算；不宣称约束未来真实模型 SDK 的任意输出。

```sh
cargo test -p dim-agent-capabilities
cargo clippy -p dim-agent-capabilities --all-targets -- -D warnings
```

压缩准备使用最终提交的 `context/compaction` 更新当前请求和恢复结果；检查点与当前请求采用相同内容。默认宿主不启用压缩。跨模块流程见[架构约定](../../docs/architecture/README.md)。

测试保护公开业务流程、跨能力顺序、before/预算、合作取消和实际资源收尾；独立生命周期复核用例保留在 `tests/review_flows.rs`。
