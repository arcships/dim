# ToolMethods

Owner: [tool_executor/read.rs](../../../../crates/dim-agent-capabilities/src/tool_executor/read.rs).
ToolExecution's Session part registers `session.tool.get`. The caller-visible contract is
[Tool origin read](../../../design/tool-origin-read.md).

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineTree` | Read the addressed branch’s own facts, including ended branches. |
| `S:SessionMethods` | Register and withdraw the admitted read method. |

It writes no event, holds no StateCell and starts no task. Each admitted request owns its
query and cancellation; its returned call data is released after serialization.

<!-- generated:plugin ToolMethods — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionMethods`、`S:TimelineTree` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->
