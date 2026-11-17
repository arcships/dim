# Timeline 跨模块交接验证

关联工作：[#1667](https://github.com/arcships/dim/issues/1667)。检查基线为 `8b1e48000`。
本页是具体实验与证据的对应表，不增加开发规范，也不把局部模型合并成全系统证明。
已有有限模型及边界见[逐项覆盖](capability-models/coverage-completion.md)。

## 选中的实际流程

后台 Shell 命令已经产生文件效果并写入 `exec/ended`，通知写入失败；宿主退出后重新打开同一 SQLite，
恢复缺失通知，再处理新的用户输入。再次打开不能重执行命令或重复通知。

约定来自 [Shell 恢复](../shell-exec-recovery.md)、[BackgroundExec](../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)、
[输入交接](../../architecture/rust-agent/plugins/InputQueue.md)及 [Dispatcher 恢复交接](../plugin-contract.md#52-consuming-facts)。
这条流程不要求外部副作用与数据库提交原子化，也不推断未知任务已经成功。

| 边界 | 实验或已有证据 | 检查什么；不能推出什么 |
| --- | --- | --- |
| 命令完成 → 终态提交 → 通知提交 | 新增 `committed_shell_completion_recovers_after_notice_failure_and_host_loss` | 真实命令在独立宿主子进程执行；SQLite trigger 拒绝通知提交。终止宿主后核对终态已持久化、其后无输入提交。不是手写一条成功终态代替命令执行 |
| 数据库重开 → 恢复查询 → 通知交接 → 新输入 | 同一实验，两次重开 | 终态仍为 Completed；通知关联同一终态且 ID 不变；通知出现 `input/consumed`；新用户输入产生模型输出；副作用文件仅一行 |
| 未完成请求或恢复写入失败 | `storage_flow::failed_output_commit_closes_only_its_session_and_recovery_settles_the_orphan` | 真实 SQLite 的输出提交失败、恢复结算再次失败与随后成功，其他 Session 不受影响。不是所有操作的错误分支穷举 |
| 原子批次提交与发布 | `timeline_batch` | 发布一个完整快照单位；拒绝空批次。没有新增一套事务模拟器 |
| 固定截止点与发布中的尾部交接 | `timeline_tail::the_handover_folds_every_fact_once_when_the_tail_is_unpublished` 及对应无尾部场景 | 在重建与实时消费交接期间，每条事实按序恰好折叠一次；这是消费者折叠次数，不是外部动作 exactly-once |

新实验复用 `transport_flow/worktrees.rs` 中已有的 local-execution + ShellSession 产品宿主装配、隔离目录和 mock 模型。
真实部分是 Shell、SQLite、宿主进程终止与重开；没有请求外部模型服务。只在 macOS 的现有 Shell 流程配置运行。
故障先使该 Session 进入失败关闭，再终止仍运行的宿主；它**没有**覆盖正在执行命令时的任意崩溃、磁盘掉电、
SQLite 提交中断或操作系统遗留进程清理。没有把这些边界写成通过。

## 候选覆盖的处置

| 候选 | 核对后的处置与证据 |
| --- | --- |
| 持久化、恢复、继续消费 | 补上上述真实 SQLite + 进程终止流程；复用已有提交失败、批次和 tail 测试。限定于列出的窗口，不宣称覆盖所有退出位置 |
| Input → Loop → tool → notice → Wait | 新实验检查 notice → Input → Loop；`local_execution` 已检查 turn stop 不取消后台任务且通知驱动新 turn、stop/完成只结算一次、Wait 匹配通知、Steer/stop 退出、重载后旧 native/face 调用退出且旧句柄拒绝调用。已有 WaitHandoff 模型另查调用结果对 Loop 的影响；没有把这些不同运行当成一个完整组合证明 |
| 插件装载、卸载、换代 | `tool_binding::optional_contributors_do_not_gate_the_loop_and_follow_new_generations` 检查动态可选贡献者；`session_lifecycle` 检查关闭/激活/hold；Shell 的重载回归检查依赖先收尾；`storage_flow::extension_events_keep_their_own_rows_and_restore_unchanged` 在没有生产插件的装配中仍能读取原扩展事实。新消费者的历史选择由其自己声明，Dispatcher 的固定 cut 交接复用 tail 测试；不能据此规定新插件一律回读全部历史 |
| 查询实现与成本 | 已有 SQLite/memory 对照覆盖 relation、group、array、keyset page；祖先截断与发布 tail 由 `input_identity_relations_keep_the_full_ancestor_range_across_pages_and_tails` 检查；`input_anti_joins_seek_the_full_identity_index_as_history_grows` 比较 16 倍历史增长的 VM 工作量，另有选中正文及页大小的成本断言。复用这些测试和既有阈值，不另加相同性质的随机测试平台或墙钟门槛 |

实际入口：[transport_flow](../../../crates/dim-app-server/tests/transport_flow/worktrees.rs)、
[storage_flow](../../../crates/dim-app-server/tests/storage_flow.rs)、
[查询与成本](../../../crates/dim-app-server/tests/storage_flow/property_queries.rs)、
[tail](../../../tests/integration/tests/timeline_tail.rs)、
[本地执行](../../../tests/integration/tests/local_execution.rs)、
[动态贡献者](../../../tests/integration/tests/tool_binding.rs)、
[Session 生命周期](../../../tests/integration/tests/session_lifecycle.rs)。

## 换代测试发现的时序假设

复跑 `optional_contributors_do_not_gate_the_loop_and_follow_new_generations` 时，第三代调用曾返回
`Tool was not executed: capability reloading`，而测试期望 `generation 3`。原测试在重载 registry 后只等待
Loop 与 provider 的新代 Active，没有等待同样依赖 ToolBinding 的 ToolExecutor。Loop 可用不等于执行链所有参与者都换代完毕。
该稳态调用测试现在也等待 ToolExecutor 的新代 Active；保留原来的第三代结果断言，不重试失败调用或延长睡眠。
没有因此新增“结构重载期间提交的所有调用都必须成功”的运行时保证。

## 旧发现及独立负责的工作

- BranchLine 文档中的 `tool/use`、`tool/result` 已按实际事件 key 修正为 `step/tool-use`、`step/tool-result`，不改变事件格式。
- 基线 Loop 文档把不存在于 EventKind 和实现中的 disposer / `step/disposition` 描述为现有机制。现行说明按实际重试、有限续接与预算恢复对齐；插件式续接保留在 [ModelRecovery 提案](../plugins/ModelRecovery.md)，不作为可用接口。决定与交付见 [#1669](https://github.com/arcships/dim/issues/1669)，不因此新增运行时机制。
- `context/scope`：当前 Rust 生产代码找到类型、序列化和消费/恢复路径；写入构造位于 `prompt_boundaries` 测试，未找到实际生产调用方；TypeScript 运行时代码也未定位写入路径。保留“端到端生产路径未建立”的边界，不凭消费者存在宣称产品流程已验证，也不新增生产者。
- [#1661](https://github.com/arcships/dim/issues/1661) 的恢复错误传播与已发布迁移入口由原负责人处理，本次不放宽其恢复 guard；[服务面迁移 #1292](https://github.com/arcships/dim/issues/1292) 和 [存储后端 #1298](https://github.com/arcships/dim/issues/1298) 也保持独立验收。

## 复现与结果

使用仓库 build 文档指定的工具链与独立 target、home。新增测试自行建立临时产品目录，子进程日志也在该目录，
失败/退出时清理；不打开用户的数据库。定向命令：

```sh
cargo test --locked -p dim-app-server --test transport_flow worktrees::committed_shell_completion_recovers_after_notice_failure_and_host_loss
cargo test --locked -p dim-app-server --test storage_flow property_queries::
cargo test --locked -p dim-agent-integration-tests --test timeline_tail --test timeline_batch --test session_lifecycle --test tool_binding --test local_execution
```

实际运行结果记录在关联 PR 与 issue；不能把本页的测试清单当成运行通过记录。
模型没有变化，没有新增或重跑 TLC 配置：新问题是实际持久化和宿主行为，需要由实现流程检查。

消融：复用既有宿主、SQLite、Shell、mock 模型与流程测试。只新增故障窗口和观察断言，不增加生产 hook、恢复缓存、
新事件或通用故障框架。子进程用于验证非优雅退出，SQLite trigger 用于确定性保留“终态已提交、通知未提交”的窗口；
二者分别承担不同目的。其余候选复用现有证据，不按 Cap 数量新增文件。
