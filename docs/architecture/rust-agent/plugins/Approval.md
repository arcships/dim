# Approval

分支的执行准入闸门：对每次工具调用问 [Permission](Permission.md)，需要询问时写 `approval/request` 并等本分支分发的决定。不负责策略本身、不执行工具、不知道客户端连接。

<!-- generated:plugin Approval — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Permission`、`B:Timeline` |
| 提供 | `B:Approval` |
| 被注入于 | `ToolExecutor` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Permission` | `admit` 时裁决；放行后复核 |
| `B:Timeline` | 写 `approval/*`；装载时读本分支历史 |
| `B:Dispatcher` | 订阅本分支 `approval/respond` |

## 服务接口

`B:Approval`

| 方法 | 语义 | 错误 |
| --- | --- | --- |
| `admit(call, risk, cancel)` | `Ok` 表示调用可以开始；`cancel` 是调用方的批次令牌，开始前最后一次取消检查在这里 | `HostError::Cancelled`（取消、分支关闭、重载），`HostError::Failed`（拒绝，含原因） |
| `respond(approval_id, decision)` | 写 `approval/respond`，等它被分发并被接受后返回；返回不代表工具已开始 | `Unknown`（不存在或已结束）、`Cancelled`（请求已被取消）、`Closed` |
| `replay(notify)` | 在 Approval 锁内按请求顺序回放未决请求 | — |

调用方：`admit` 由 [ToolExecutor](ToolExecutor.md) 注入调用。`respond`、`replay` 由 app-server 经 `Session::service::<Approval>(branch)` 调用：`approval.respond`（v2/v3 必带 `branchId`，v1 固定 main）与 attach 时逐分支回放。

## 运行时调用方

| 调用方 | 方式 |
| --- | --- |
| app-server `approval.respond` | `Session::service::<Approval>(branchId)` |
| app-server attach 回放 | 遍历 `session.branches()`，逐个 `service::<Approval>` |

## 事件

写出：

| 事件 | 时机 | 字段 |
| --- | --- | --- |
| `approval/request` | `admit` 裁决为询问且未取消 | `approvalId`（新 UUID）、`source`、`call`、`reason` |
| `approval/respond` | `respond` 找到未决请求 | `approvalId`、`decision` |
| `approval/cancelled` | 请求在被接受的 `approval/respond` 之外结束：调用方取消、放行后复核拒绝、放行后在开始前被取消、卸载、上一代遗留 | `approvalId`、`reason` |

消费：

| 事件 | 时机 | 作用 |
| --- | --- | --- |
| `approval/request` | 写入时（登记回调） | 加入未决列表；此后的 `replay` 包含它 |
| `approval/respond` | 写入时（登记回调） | 登记 `respond` 的等待者，保证先于分发 |
| `approval/respond` | 提交后 | 取出未决请求：已取消的回 `Cancelled`、通知 `admit` 按取消结束（不是重载），并补 `approval/cancelled`；否则把决定交给 `admit`；不存在的回 `Unknown` |
| `approval/request`、`approval/respond`、`approval/cancelled` | 装载时 | 找出本分支未结束的请求，逐个补写 `approval/cancelled`（`execution did not survive recovery`） |

## 状态

| 状态 | 位置 | 重建 |
| --- | --- | --- |
| 未决请求（请求、取消令牌、决定通道） | 内部，按请求顺序 | 不重建：上一代未结束的请求在装载时补写结束 |
| `respond` 等待者 | 内部，按事件 ID | 不重建 |

一个请求：

```text
              admit: Ask
                  │ approval/request（登记进未决）
                  ▼
              ┌────────┐  approval/respond{deny} ─────────────► 结束：拒绝
              │ 未决   │  调用方取消 ─ approval/cancelled ─────► 结束：取消
              └────────┘  卸载/重载 ─ approval/cancelled ──────► 结束：重载
                  │ approval/respond{allow}
                  ▼
             复核 Permission
              ├─ 拒绝 ─ approval/cancelled(reason) ──────────► 结束：拒绝
              ├─ 已取消 ─ approval/cancelled ─────────────────► 结束：取消
              └─ 其他（放行或再次询问）─────────────────────────► 开始执行
```

每个已提交的 `approval/request` 以一个被接受的 `approval/respond` 或一个 `approval/cancelled` 结束；被接受的 allow 若复核拒绝或在开始前取消，另写一个 `approval/cancelled`。

## 生命周期

- 装载：先订阅，闸门内扫描本分支历史、补写遗留请求的结束，记 `rebuilt`，再提供服务。
- 卸载：标记关闭、丢弃 `respond` 等待者、取出全部未决请求，逐个写 `approval/cancelled` 后释放其 `admit`（`admit` 返回重载取消）。
- 调用方取消：`admit` 在锁内取出请求，放锁后写 `approval/cancelled`；若决定先到，请求已被取出，不再重复结束。

## 不变式

- 结束请求的每条路径都在 Approval 锁内把它移出未决，放锁后再写结束事件；`replay` 在同一把锁内读：被回放的请求的结束事件一定在回放之后，已移出的请求不会被回放。
- 写事件只在锁外进行：Timeline 登记回调会在提交锁内取 Approval 锁。
- 放行不写事件；许可事实只有被分发的 `approval/respond{allow}`。
- 放行后复核时再次得到询问，视为已批准。

## 为什么

- 决定经事件分发而非直接回调：同一决定对重连客户端、重放与审计可见，且与分支其他事件同序。
- 取消走调用方的批次令牌：中断、停止的语义由 Loop 与 ToolExecutor 决定，Approval 只观察。
- 按分支一个实例，由调用方经分支门面 `Branch` 查找，不维护进程级索引：没有反向注册，也不需要清理。

## 代码与测试

- `crates/dim-agent-capabilities/src/approval.rs`
- `crates/dim-agent-capabilities/tests/permission_flow.rs`
- `crates/dim-app-server/src/approval.rs`（RPC 层）
- `crates/dim-app-server/tests/transport_flow.rs`、`transport_flow/approval.rs`（断线、竞争、停止、重载、attach 回放）
