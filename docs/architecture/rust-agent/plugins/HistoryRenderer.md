# HistoryRenderer

The Session part serves the [history rendering operations](../../../design/contracts/runtime-owner-operations.md) used by Loop and ContextPreparation.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineTree` | Resolve the requested branch timeline. |

<!-- generated:plugin HistoryRenderer — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:TimelineTree` |
| 提供 | `S:HistoryRenderer<Arc<TimelineTree>>`、`S:dyn HostDispatch#historyRenderer` |
| 被注入于 | `ContextPreparation`、`Loop` |
<!-- /generated -->
