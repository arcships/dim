# LoopMethods

Loop's Session part registers nonmutating `session.turn.get`.
The [read contract](../../../design/input-state-read.md#turn-identity-read) defines its fields.
It declares TimelineTree to query the selected branch and SessionMethods to register the entry.
It owns no persistent state, background worker, events or services. Reads end with their request.

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:TimelineTree` | Query the requested branch at its published cutoff. |
| `S:SessionMethods` | Register the read entry. |

<!-- generated:plugin LoopMethods — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionMethods`、`S:TimelineTree` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->
