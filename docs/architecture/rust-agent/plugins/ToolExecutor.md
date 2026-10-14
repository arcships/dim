# ToolExecutor

> 部分文档：只写本地执行相关的执行准入。批次执行、中断与重载结算见[事件驱动约定](../event-driven-flows.md)与[工具贡献](../tool-contributions.md)。

分支上执行模型发出的工具调用批次。每个调用先经 [Approval](Approval.md) 准入，通过后才调用处理器。

<!-- generated:plugin ToolExecutor — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Approval`、`B:Diagnostics`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`B:ToolBinding` |
| 提供 | `B:ToolExecutor` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Approval` | 每个调用开始前的准入 |
| `B:ToolBinding` | 按本请求的快照取定义（含 `risk`）与处理器 |
| `B:Timeline` | 写 `step/tool-result`；装载时补结上一代未完成的批次，并回放出当前 turn 是否已中断 |
| `B:Dispatcher` | 订阅 `step/tool-call`、`stop/interrupt`、`turn/start`、`turn/end` |
| `B:TimelineIdentity` | 构造工具执行上下文中的分支 ID |
| `B:Diagnostics` | 记录处理器 panic 与执行错误 |

## 执行准入

```text
step/tool-call 批次 ──► 每个 call:
    binding.handler(request, name) ── 无快照/无此工具 ──► 错误结果（"Unknown tool: <name>" 等）
        │
    approval.admit(source=Tool{turn, request, call}, call{name,input}, risk, 批次令牌)
        ├─ Cancelled ──► 结果为取消（中断/停止/重载）
        ├─ Failed ─────► 错误结果，内容为拒绝原因
        └─ Ok ─────────► handler.execute(call, context, 批次令牌)
```

- 准入与执行共用批次令牌：中断在等待审批时到达，Approval 结束请求并返回取消。
- 放行的调用不写审批事件；需要询问的调用在批次结果之前出现 `approval/request` 及其结束。

## 本次改动

- 注入 `B:Approval`，在处理器之前调用 `admit`；删除经 `ToolDriver::authorize` 的授权路径。
- 处理器取自 [ToolBinding](ToolBinding.md) 的请求快照，不读注册表当前表：来源在调用途中被替换或撤销，不影响本请求。
- MockTools 的 `echo`、`wait` 与 `branch` 工具声明 `risk: session`，在任何 preset 下放行。

## 代码与测试

- `crates/dim-agent-capabilities/src/tool_executor.rs`
- `crates/dim-agent-capabilities/tests/permission_flow.rs`
