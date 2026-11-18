# Owner 交接：消费前提与跨代结算实验

对应 #1732 的 D，代码基线 `c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`。
这是实验和源码对应，不新增插件规则。A/B/C 分别在 PR #1734/#1737/#1739；本批独立基于 main，
不把尚未合并的模型当运行时依赖。运行范围、哈希和可达窗口见[记录](capability-models/owner-handoff-results.md)。

## 1. 从合同到两个交接

| 现行约定 | 实现与模型 | 前提的提供者及范围 |
| --- | --- | --- |
| [时间总线 §2、§4](../../architecture/rust-agent/time-bus.md)：生产方只按自身已消费状态复核；消费者拒绝中断后的迟到请求 | `AgentLoop::on_event` 在模型终态上写 tool-call；ToolExecutor 的 handler 记录 interrupt、拒绝批次；`InterruptHandoff.Deliver` | Dispatcher 单事实串行交付；允许在较早事实尚在派发时追加 interrupt。两个消费者进度分开，不引入全局取消位 |
| ToolExecutor 在途工作先释放资源再写结果；Loop 等结果再结束 turn | [ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md) 的任务 join 与 result；`Invoke/Finish`、`TurnEndWaitsForResult` | 本模型将 host 结束作为动作机会；不证明 host 尊重取消或最终返回 |
| [重载协议](../../architecture/rust-agent/single-tree.md#能力重载协议)：旧工作不重执行，各 owner 写自己的终态 | `WaitBranch` cleanup、ToolExecutor cleanup/rebuild、Loop rebuild/on_event；`GenerationHandoff` | 一个已持久化的 turn 和顶层 batch；一个调用、各 owner 最多一次正常换代，另允许一次进程崩溃 |
| [Wait](../../architecture/rust-agent/plugins/Wait.md) 重载拒绝旧调用，并把 reload 交给工具结算 | `ReplyReload/Settle/ConsumeLoop`，`ReloadDoesNotContinue` | `Reloading` → HostError → result settlement 标记的转换来自代码；普通完成另有 `NormalReply`，不会预先假定所有结果都是重载失败 |
| 新代读完整固定 cut 后再消费后缀 | ToolExecutor `rebuild`、Loop `rebuild`；`CaptureTool/CaptureLoop` 与后续处理 | C 的暂存/串行算法作为既有接口前提；本模型不复制队列，而让 Loop 在重建前后读到终态 |
| 失败不能伪装成功接管 | `RebuildTool(FALSE)/RebuildLoop(FALSE)` 保留 failed；`Settle(FALSE)`、`StorageFailure` 保留写入故障 | 查询失败与存储故障分开；required apply 失败的装配回滚不在模型中，见第 4 节。不能据此宣称分支继续健康运行 |

源码入口：
[Loop](../../../crates/dim-agent-capabilities/src/agent_loop.rs)、
[ToolExecutor](../../../crates/dim-agent-capabilities/src/tool_executor.rs)、
[Wait](../../../crates/dim-agent-capabilities/src/wait.rs)、
[Dispatcher](../../../crates/dim-agent-runtime/src/core/dispatcher.rs)、
[Timeline](../../../crates/dim-agent-runtime/src/core/timeline.rs)。

## 2. C 的 cut 前提，哪些 owner 实际提供了

源码检索范围为 `crates/` 的生产 `Dispatcher.consume` 调用，共 13 处；另核对三个外语 fixture。
这不是所有读历史的插件清单，也不把仅 `subscribe/listen` 的 live-only 接口混进来。

令注册时已派发位置为 d，之后回调读取的 allocated head 为 h。
`Dispatcher.consume` 先 subscribe 再调用 rebuild；`Timeline.committed()` 在锁内返回 allocated，
`Dispatcher.committed()` 委托同一读。因此在同一条仍存在的 Timeline 上有 d ≤ published ≤ h。
下表各回调在注册之后取 h，而非使用装载前保存的旧 cut。
**这闭合的是 cut 不过旧；查询选择是否重建了正确业务状态，是另一项义务。**

| Owner / 源码 | cut、查询与返回 | 本轮核对结论及业务边界 |
| --- | --- | --- |
| [Loop](../../../crates/dim-agent-capabilities/src/agent_loop.rs) `rebuild` | 开头 committed；branch 固定 through、include_publishing；分页不改 through；返回原 through | 包含旧代尚未发布终态；未结束 turn 标记 failed，等待所属 owner 的终态；未重新证明全部请求恢复选择 |
| [ToolExecutor](../../../crates/dim-agent-capabilities/src/tool_executor.rs) `rebuild` | 同上；先选最新控制，再选无关联结果的 batch | 只补失败结果；读子结果效果的附加 query 可取更新 head，以包含刚补写的 child terminal；不把这个附加读取当消费 cut |
| [ContextPreparation](../../../crates/dim-agent-capabilities/src/context_preparation.rs) `rebuild` | committed、固定 through、include_publishing、返回原 through | 未交给 model/call 的准备归本 owner；完整替换/压缩选择仍复用原局部模型和流程 |
| [ModelProvider](../../../crates/dim-agent-capabilities/src/model_provider.rs) `rebuild` | committed、固定 through、include_publishing、返回原 through | model/call 的终态归本 owner；不把 retry 的本代控制状态迁移给新请求 |
| [Approval](../../../crates/dim-agent-capabilities/src/approval.rs) `rebuild` | committed、固定 through、include_publishing、分页、返回原 through | 补取消未结算审批；历史依赖多于 live 的 approval/respond |
| [Permission](../../../crates/dim-agent-capabilities/src/permission.rs) `rebuild` | committed、chain 含 publishing，取最后 policy，返回 reply.through | 祖先策略与本分支版本分开；没有把所有历史事件当实时重放 |
| [Wait](../../../crates/dim-agent-capabilities/src/wait.rs) `rebuild` | committed、branch 含 publishing，取最后 turn 边界，返回 reply.through | 只重建 open，不恢复旧通知或旧 wait future |
| [InputQueue](../../../crates/dim-agent-capabilities/src/input_queue/recovery.rs) `rebuild` | 先 committed，再 await_publication；固定 through、不含 publishing；返回原 through | 等的是捕获的 h，不用后来移动的 head；等待失败返回错误 |
| [BackgroundExec](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) `rebuild` | committed、固定 through、query 含 publishing、返回原 through | orphan 结算和缺失 wake 选择各有合同；repost 的错误处理不由本轮 cut 检查证明 |
| [Goal](../../../crates/dim-goal/src/branch.rs) `rebuild` | committed、publication(h)、load 固定 h；返回 h | [read](../../../crates/dim-goal/src/read.rs) 读取最后 goal/state；[query](../../../crates/dim-goal/src/branch/recovery.rs) 不含 publishing；pause_reloaded，不恢复旧任务 |
| [CollaborationTimeline](../../../crates/dim-app-server/src/collaboration.rs) `restore` | 回调内 committed、固定 through、include_publishing；返回原 through | 整理旧操作；部分撤回工作另等 activated；这不等于所有协作副作用已经结束 |
| [HooksBranch](../../../crates/dim-agent-capabilities/src/hooks/runner.rs) apply 回调 | 回调内 committed，设置 view.last，返回 h；不读历史 | [Hooks 合同](../../architecture/rust-agent/plugins/Hooks.md) 明确不重跑旧 hook；不是遗漏了通用恢复扫描 |
| [BranchLine](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) apply 回调 | 回调内 committed，返回 h；随后 want Roster/Parent | [BranchTool 合同](../../architecture/rust-agent/plugins/BranchTool.md) 从当前目录和按需历史重建；跳过旧触发不等于恢复整个业务视图 |

[TS branch](../../../runtime/fixtures/branch-part.ts)、[TS extension](../../../runtime/fixtures/extension-events.ts)、
[Python branch](../../../runtime/fixtures/python/python_branch_part.py) 均在 rebuild 回调里调用
`dispatcher.committed()`，以同一 cut 查询 include_publishing 后返回。它们是示例证据，不能替第三方插件作保证。
新插件仍必须自己提供这个前提；无需枚举封闭消费者名单。

## 3. 中断与迟到请求

[InterruptHandoff](capability-models/InterruptHandoff.tla) 保存 log、published、共享串行派发位置，
以及 Loop/ToolExecutor 各自已消费位置和中断状态。一个事实可先送达任一 owner，但必须都处理完才开始下一事实。
后台发布、外部 interrupt、工具调用的开始/完成可以夹在两次处理之间。

- `Model` 是该 turn 的模型输出完成，不是再次发起模型；`Deliver("loop")` 根据 Loop 自己的已消费中断决定写请求或结束。
- interrupt 可以先提交，Loop 仍在处理更早的完成事件，因而写出排在 interrupt 后面的请求；`late` 记录这个窗口，**不阻止它发生**。
- ToolExecutor 消费 interrupt 后取消当前工作；读到迟到请求时写未执行结果。已经开始的工作可晚于 interrupt 完成。
- `Invoke` 代表取消检查之后进入工具；`Finish` 代表其任务退出和结果提交，外部资源释放是 host 的前提。
- 本模型限定一个 turn 一项顶层 batch，不覆盖同一 turn 两个顶层 batch 之间的中断；生产实现可在工具结果后继续同 turn 的下一请求。第二配置允许下一 turn，检查 turn 边界清除中断后新工作仍能运行。下一 turn 在前一 end 派发完后开始，省略同一 end 回调内提前追加 start 的窗口。

检查：迟到请求不启动、被拒请求已有结果、已交出 batch 的 turn 不先结束、消费位置保持前缀；
动作性质检查生产方使用本地消费状态、工具不在本地已取消后启动。没有假设调度公平或声称最终终止。

## 4. 旧工作、终态与新代

[GenerationHandoff](capability-models/GenerationHandoff.tla) 分开 Wait、ToolExecutor、Loop 的阶段，
并区分 append、durable、published。状态名 old/new 是模型代号，不是产品的新增生命周期。

- `CloseWait → ReplyReload → RemoveWait`：旧调用释放后才结束 Wait 换代；ToolExecutor 可以稍后才写结果。
- `CloseTool → CancelToolCall → Settle → RemoveTool`：旧工具任务完成结算尝试后才退出；写失败仍可能完成卸载，但保留 cleanupError，绝不将“卸载完成”算作“终态成功”。
- `CaptureTool → RebuildTool`：读本 owner 固定头，无结果才补失败；不重新 Invoke。
- `ReloadLoop → CaptureLoop → RebuildLoop`：若已看到结果就结算旧 turn；否则等待其后实时结果。Loop 与 ToolExecutor 的读取顺序不限，旧终态可以仍未发布。
- `NormalReply` 是正常 Wait 返回（如超时）的对照；旧 Loop 可决定继续。带 reload 标记的结果、或恢复出来的未结束 turn 则失败，不再继续旧 turn。
- `Crash` 只保留 durable 前缀，丢失句柄与旧代调用；没有执行正常 unload。新 ToolExecutor 对持久化未结束 batch 补终态。已经发生的抽象 host effect 不因崩溃回滚。
- 模型的 query/rebuild 失败只置对应 owner failed；`Settle(FALSE)` 表示终态 append 失败，`StorageFailure` 表示尾部落盘失败。这只表示查询失败没有直接设置存储故障；不表示实际装配继续健康运行。
  required capability 的 apply 失败会使 branch build 失败并触发回滚（[实例创建合同](../../architecture/rust-agent/single-tree.md)；插件合同 §7.1 规定发布后的 activated）；
  本模型没有装配/回滚动作，不能用另一个 owner 仍可转换来证明真实分支会继续运行。

边界：只含一个已持久化顶层调用，不覆盖未持久化 call 的丢失、嵌套工具效果合并、任意多代、所有 host 的取消实现。
`effect` 是普通工具可能产生的外部影响标志，不表示 Wait 自己修改外部系统，也不证明外部系统的行为。
`DecideAfterResult` 的正常分支记录 continue 决定和 continued 阶段，不是 turn/end。
正常工具结果通常令 Loop 发起下一个模型请求；本模型不展开该请求及最终正常 turn/end。
`EndAfterTerminal` 因而只覆盖失败/恢复结束；该决定之后不再模拟一次正常 Loop 换代。
仅在本次历史上检查结果不重复，**不承诺跨崩溃外部 exactly-once**。

Timeline 已有存储故障时，纯查询仍可能成功；模型允许不需补写的 ToolExecutor 重建返回 new，
但故障仍阻止发布/消费。new 只是该回调返回成功，不意味着分支恢复健康。
这和“一个失败的重建不能返回成功”是两个不同命题。

## 5. 实现证据、修正与消融

本轮读取已有公开流程断言，未重跑未修改的 Rust 行为测试；不能把下面的存在当作本轮测试通过：

| 流程 | 对应关系 |
| --- | --- |
| [interrupt_flow](../../../crates/dim-agent-capabilities/tests/interrupt_flow.rs) `a_tool_call_already_committed_when_the_interrupt_arrives_is_not_executed` | 真实门闩令 call.sequence > interrupt.sequence，验证执行次数为零、未执行结果与 Cancelled turn；对应 late 窗口 |
| 同文件 `interrupt_reaches_every_capability_before_native_cleanup_and_stop_waits_for_the_turn` | 中断传播与本地清理相分离 |
| [local_execution](../../../tests/integration/tests/local_execution.rs) `wait_reload_settles_the_tool_call_without_a_user_stop` | 真实 WaitBranch restart；检查 settlement 标记、Failed turn、模型调用不增加；新代仍可接受新 wait |
| 同文件 `wait_service_reload_ends_native_and_face_calls_and_refuses_old_handles` | native/face 及旧句柄边界；模型不展开 wire |
| [context_flow](../../../crates/dim-agent-capabilities/tests/context_flow.rs) `restore_preserves_each_parent_tools_own_confirmed_child_effects`、`restoring_each_preparation_handoff_settles_without_replaying_work` | 既有嵌套效果/准备交接证据，本批未扩展为组合证明 |

发现并更正的文档问题：ToolExecution、Questions 的设计/架构页及中文翻译、ToolBinding 的 rustdoc 把“重载恢复的 batch”
写成会重新执行，与当前 rebuild 和重载协议不符；同时把 Dispatcher 依赖的 “Replay batches” 改为恢复/live 交接。
只修正文档和注释，不改变执行、事件或持久化格式。

消融：不复制 A 的写入队列、B 的查询表达式、C 的订阅缓冲；不增加全局取消位。
删除冗余的 startedAfterRestart 观察量，以 Invoke 的动作性质及重建不调用 host 的性质表达边界。
保留各 owner 阶段、各自 cut、持久化/发布位置和清理失败，因为它们分别区分真实交接窗口。
所有结果都是有限模型检查；D 的两组交接不等于所有 cap 的任意组合已证明。
剩余工作包括其他 owner 的业务选择/终态义务组合、A/B/C 已列的模型边界及 facade 故障观察（#1718/#1719）。

## 6. Loop → Preparation → Model 的恢复结算责任

[RequestRecovery](capability-models/RequestRecovery.tla)补充另一个明确交接，
不是把 Wait/Tool 的 GenerationHandoff 结果推广到所有请求。
模型范围是**旧任务已经停止后的恢复**：持久历史固定在一个交接阶段，三个 owner
可以独立捕获 committed cut、延后完成查询/补写，恢复产生的终态可以尚未发布。
没有旧任务在查询期间继续交接、新请求或同一 owner 的两代同时写入；动态热重载中仍有旧任务运行的情况不由此模型证明。

| owner | 对同一 `(turn_id, request_id)` 的恢复选择 | 前提提供者 / 实现 |
| --- | --- | --- |
| Loop | 有 model-start，无 context/prepare、无 output-end | Loop 产生 request 身份和 context/prepare；`agent_loop.rs::rebuild` 定位最新未结束 turn/request 后按序构造 handed_over / request。模型将这个投影作为已选当前 request，不证明其完整查询算法 |
| ContextPreparation | 有 context/prepare，无 model/call、无 output-end | Loop 交接，Preparation 成功写 model/call 或失败写 output-end；`context_preparation.rs::rebuild` 用相关存在查询排除已交接/结束身份；[owner 合同](../../architecture/rust-agent/plugins/ContextPreparation.md#events-and-lifetime) |
| ModelProvider | 有 model/call，无 output-end | Preparation 交接，ModelProvider 写输出终态；`model_provider.rs::rebuild` 按 turn/request 匹配 output-end；[owner 合同](../../architecture/rust-agent/plugins/ModelProvider.md#events-and-lifetime) |

模型初态包含四个阶段（start、prepare、call、已结束），另有一个已结束身份作为干扰：
可以同 turn 不同 request，或同 request 不同 turn。匹配必须同时使用两项；这描述契约，不是证明 SQL 实现了关联。
它不生成非法的重复 start、倒序交接或多个未结束请求；这些有效历史前提由生产方和 Loop 单活动请求合同提供。
查询取完整 committed 前缀，不能把只读 published 当作同一题设。

推导：记 S/P/C/E 为该身份已有 start/prepare/call/end。合法交接满足 C ⇒ P ⇒ S；
S 成立且 E 不成立时，`¬P`、`P ∧ ¬C`、`C` 恰有一个成立，分别对应三个 owner。
恢复期间只新增 E、不新增 P/C，所以不同 cut 只会使原责任方看见“已结束”，不会把责任转给另一方。
每个 owner 的本次恢复最多结算一次，因而不重复；若全部成功完成，原责任方补 E 或已经读到 E，因而不遗漏。
这段论证依赖上文的有效历史、停止旧任务、完整 committed 查询和单次恢复前提；不扩展为任意热重载证明。

`Capture` 保存各自选择，`Complete` 根据该选择补写，**没有在补写处加入全局去重检查**。
`AtMostOneTerminal` 因而检查三个选择与交错是否允许重复，而非靠写入守卫直接保证答案。
`CorrectSettlementOwner` 用初始交接阶段的独立责任表检查写入方；
`AllSuccessfulMeansSettled` 检查三方恢复均完成时确有一个终态。不是“最终一定完成”的活性声明。
`PublicationBound` 仅是模型自洽检查，不作为额外业务证明。

失败配置保留查询失败和终态写入失败；失败不算成功恢复，没有承诺失败后一定有终态。
它不模拟 required part 失败后的装配回滚，也不把其他 owner 还能转换解释为分支仍健康。
模型不重新调用外部模型/准备工作；这是范围边界，不包装成一次独立的不重执行证明。

实现对应复用 `context_flow::restoring_each_preparation_handoff_settles_without_replaying_work`：
原九种持久化边界已有 ModelOutputEnd 总数唯一断言（不仅是前面的存在性检查）；
本批保留该断言、“不重新准备/调用模型”和工具结算检查，仅增加当前 turn/request 的
ModelOutputEnd 数量断言以确认终态身份：未创建 request 时为零，其他边界恰好一条，包含原本已完成的历史终态。
该流程没有穷举模型调度，也没有覆盖任意业务 owner 的结算责任。

模型配置、可达窗口和执行结果见[恢复结算运行记录](capability-models/request-recovery-results.md)。
消融：不复制 A 的存储队列、B 的查询表达式或 C 的暂存算法；三个独立 cut/选择用于保留真实交错，
初始阶段用于独立责任判定，干扰身份用于明确关联边界。未增加运行时抽象、规则或接口。

## 7. Owner 范围收口表

本表核对的是上文 **13 个 consume owner 的恢复/交接责任**，不是所有 Capability 的完整业务规格，
也不涵盖只使用 subscribe/listen、门面、存储等其他入口。代码基线仍为 `c8e4edd5b`，
实验补充见本 PR；原有模型证据按各自记录版本复用，未默认为当前 main 全量通过。
“断言已读”表示本轮读取具体断言；“入口已定位”不表示已完成该流程的全部断言审计。本轮不重跑未修改的测试。

| Owner | 自己负责的恢复/交接；依赖谁 | 现有证据及本轮核对程度 | 处置 / 剩余范围 |
| --- | --- | --- | --- |
| Loop | 重建当前 turn/request；未交接的 request 自己结算，已交接者等待 owner 终态；依赖 InputQueue、Preparation、ModelProvider、ToolExecutor 的事实 | §6 RequestRecovery + §3/4 组合；context_flow 九边界断言及终态总数已核对 | 停止旧任务的请求恢复已作有限组合；仍运行的上游/下游不能直接套用 |
| ContextPreparation | prepare 无 call/end 则补失败；自动 compaction 无结果则补取消；依赖 Loop 的身份和 ModelProvider 的接管事实 | §6 + Preparation；rebuild 查询与九边界断言已读 | 请求责任已对应；compaction、steer 与新代的组合仍沿原局部证据，不称此次组合已证明 |
| ModelProvider | call 无匹配 end 则补失败，不重新调用模型；依赖 Preparation 的 call 和本 owner 的输出结算 | §6 + 既有 ModelOutput/RetryChain；rebuild、cleanup、injects 已读 | **单独重启时上游 Preparation 可继续运行**，见下节；已由 §8 的独立重启模型和真实流程补齐 |
| ToolExecutor | batch 无 result 则补失败，子结果效果归各自父调用；依赖 Loop 的 batch、工具 owner 的返回和释放 | §3/4 + NestedSettlement；原 local_execution/interrupt_flow 对应已在 §5 记录 | 不重复建同一模型；嵌套效果、不同 host 释放语义沿各 owner 证据，未推成外部 exactly-once |
| Approval | request 无 respond/cancelled 则取消；依赖审批身份和调用方停止旧执行 | [Permission owner 合同](../../architecture/rust-agent/plugins/Permission.md)；ApprovalRequests；[permission_flow](../../../crates/dim-agent-capabilities/tests/permission_flow.rs) 的 `restore_cancels_only_unsettled_approvals_across_pages_without_running_tools` 断言已读：259 项、排除已响应项、取消总数与身份集合一致、工具零调用 | 复用既有选择/分页与执行证据；不再单独造一个“恢复取消”模型。不同 owner 仍在提交新审批时的交接不由这条测试穷举 |
| Permission | 取 chain 最新 policy；仅本分支事实更新 version；依赖 Timeline 的祖先 cut | PermissionState/PermissionFacts；同文件 `updates_are_versioned_per_branch_take_effect_when_dispatched_and_survive_restore` 断言已读：版本冲突、并发更新一胜、子分支 version=0、恢复等值 | 归已有状态模型和真实接口流程；不增加跨效果结算模型，权限探测/host 执行仍有自己的范围 |
| Wait | 重建 turn 是否 open；旧调用在卸载时结束，不恢复旧 future/通知 | WaitRuntime/WaitHandoff + §4；本 PR 原 local_execution 两个 Wait 重载流程 | 复用已交付证据；不把恢复 open 写成恢复调用。其他工具 owner 不自动获得 Wait 组合的证明 |
| InputQueue | 固定发布 cut 重建未消费/删除身份、applied/withdraw 和 turn 状态；依赖 Loop 的 consumed 和终态 | [InputQueue 合同](../../architecture/rust-agent/plugins/InputQueue.md)；InputQueue/InputReceipts/PromptAdoption；recovery 查询及 context_flow 的 queued/handover 断言已读：撤回恰好一次、不再 applied、两 queued input 分属两 turn、二次恢复不再写删除 | 保留既有模型与流程；pending 外部可见性仍由 #1721 的独立屏障承担，不能用恢复表替代 |
| BackgroundExec | orphan started 补 ended；已结束未通知者补 wake；依赖进程任务释放、input 身份关联 | ExecLifecycle/ExecStopHistory；[local_execution](../../../tests/integration/tests/local_execution.rs) 的 `idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end` 断言已读：任务阻止 idle unload、orphan 失败、丢失通知恢复 | 正常成功路径复用；§8 补齐 append 同步拒绝、异步失败的故障观察及下次恢复补通知，不承诺任意失败下最终成功 |
| Goal | 固定 cut 读取业务状态，pause_reloaded，不重启旧 continuation/evaluator；依赖 input/turn 和子分支目录 | [Goal 合同](../../design/plugins/Goal.md)；GoalLifecycle/GoalProgress；[goal_cases_flow](../../../crates/dim-goal/tests/goal_cases_flow.rs) 的 `d5_a_restored_line_repeats_no_decision` 断言已读：恢复暂停、旧 continuation 不重启、显式 resume 才继续 | 复用已有恢复语义；evaluator 子分支清理/输入撤回组合仍是业务范围，不因 cut 核对判定全覆盖 |
| CollaborationTimeline | 未投递操作写关闭错误，已投递 delegation 恢复关联，后续撤回等待 publication；依赖 Session collaboration owner 和 input/tree | [Collaboration 合同](../../design/plugins/Collaboration.md)；CollaborationHandoff/Identity；restore 选择及 transport_flow 的 restart/private 与 orphan 清理断言已读：重启保留 delegation 与隔离、移除 orphan/incomplete 目录且保留合法 leader | 目录/投递/撤回并非一个 terminal；保留逐项核对，不用“delegation 可恢复”代表全部副作用已结算 |
| HooksBranch | 从当前 cut 开始接收新触发，不重跑旧 hook；旧运行 handler 由自己卸载结算 | [Hooks 合同](../../architecture/rust-agent/plugins/Hooks.md)；HookScheduling/HookFollowups 等；`a_live_reload_resolves_the_tool_that_started_before_hooks_mounted` 断言已读：工具阻塞时重启 Hooks，释放后仍得到正确工具名/参数 | 真实跨 owner 热重载已有流程，直接复用；不要求补全量历史回放，不把旧请求身份查询误作重执行 |
| BranchLine | 不回放旧触发；从当前目录、last roster/parent block 和目标 cursor 按需构造视图；依赖 TimelineTree/目录及 publication | [BranchTool 合同](../../architecture/rust-agent/plugins/BranchTool.md)；BranchLine；branch_flow 的 `a_restore_writes_a_due_parent_block_once_and_none_without_movement`、`a_read_cursor_survives_a_restore` 断言已读：只补应有 parent block、无新进展不补、cursor 后续读严格前进 | 归按需视图恢复，不能强加“所有历史事件消费一次”；目录重建/通知组合仍按对应 flow 进一步核对 |

### 7.1 生命周期不是全局停机屏障

[rutis 生命周期合同](../rutis-usage.md#4-lifecycle-rutis-provides)与固定依赖 rutis 0.19.3 的
`src/fiber.rs` 相符：`Intent::Restart` await `unload(Pending)` 后才 refresh dependencies/load；
unload 关闭 admission、取消代次、等待在途调用和 drain_effects。
**它串行化同一 fiber 的代次，不保证所有 owner 同时静止，也不证明未登记或未协作的外部任务已经结束。**
清理错误走 error sink 后仍可进入 Pending，不能把新代启动解释为旧代结算成功。

Dim 的 Loop、Preparation、ModelProvider cleanup 都关闭 task tracker 并等待自己的任务；
Preparation/ModelProvider 还标记 closed、取消 current，错误按其清理路径汇总。
实际依赖区别也必须保留：Loop 和 ModelProvider 都 inject ContextPreparation，
Preparation 服务撤下会影响这些依赖方；ModelProvider 自己只提供 progress，
Loop/Preparation 不依赖它，因此单独重启 ModelProvider **不能假设 Preparation 被同时停止**。

本次验证的合法窗口：Preparation 可能在 ModelProvider 注册前、恢复捕获 cut 前后写入 model/call。
cut 内的未结束 call 走恢复结算；cut 外的 call 应经 C 的 held/live 交接进入新代处理。
要验证的是这两条路径的责任分界、错误和终态唯一性，而不是人为让同一 owner 的旧新代并行运行。
§8 给出该窗口的 TLC 与真实流程证据；不把现有 Session host 级联重启测试算作单独重启证据。

### 7.2 收口结论

13 个 consume owner 均有责任与证据去向；这不是 13 个 owner 的全部业务承诺审计完成。
上节 ModelProvider 热重载窗口已在 §8 补齐；Collaboration/BranchLine 的上述恢复断言可复用，但不扩展为全部操作组合已验证。
BackgroundExec 通知失败与下一次恢复见 §8。InputQueue 对外屏障和故障保留继续由 #1721/#1719 负责。
不为已存在的 Approval/Permission/Goal/Hooks 恢复流程重复建模，也不把本轮读取的旧证据记作新运行通过。


## 8. 上游仍运行的 ModelProvider 重启与 BackgroundExec 补通知失败

| 合同与动作 | 模型 | 真实实现与流程 |
| --- | --- | --- |
| ModelProvider 读 committed cut 内未结束 call，补失败终态；cut 外经 Dispatcher 接收并执行 | ModelReloadHandoff 的 Capture / ReadOK / Deliver / Finish | `model_provider.rs` 的 rebuild / consume；`context_flow::context_reads::model_only_reload_hands_off_a_running_preparation_at_its_recovery_cut` 直接 restart ModelProvider fiber，断言 Preparation 和 Loop 代次不变 |
| 恢复查询失败不能执行暂存 call；后续重新恢复重新取 cut | ReadFail / Retry | 同一流程阻塞真实存储查询并注入错误，确认该代 Failed、模型调用零次；再 restart，将已有 call 作为历史补一个终态 |
| 已结束且无持久 wake 的执行仍欠通知；故障后在下一次恢复补发 | ExecWakeRecovery 的 Capture / Post / Fail / Reload | `workspace_tools/background.rs` 的 rebuild / repost 与 Timeline commit/publish 故障路径；`local_execution::a_failed_recovery_wake_faults_the_session_and_is_posted_once_on_retry` |

ModelProvider 流程覆盖三个窗口：call 在 cut 内但尚未发布、在 cut 外且查询尚未完成、
查询失败后再次重启。都检查同 turn/request 只有一个终态；仅第二种调用模型一次并正常结束，
其余恢复补失败且不调用模型。旧的 `model_reload_recovers_the_old_terminal_before_its_publication`
重启 Session context hosts，会级联影响 Preparation/Loop/ModelProvider，与这里的独立重启不同。
本次一并重跑，保留原有发布延迟路径。

模型只含一项 prepare→call、一次恢复查询失败和一次后续重试。WriteCall 可以在 Capture 前后发生；
注册与 cut 间没有另造一个业务状态，Dispatcher 暂存/实时交接复用 C 的合同。
ReadOK 将选中历史 call 的补终态视作成功原子动作，**不覆盖该补写本身失败**；
旧代的任务释放、多个请求、流式输出、重试链沿各 owner 既有证据，不由本模型重新证明。
活性依赖准备完成、查询/发布/执行最终推进，以及失败后确实重试并成功，不保证永久故障下结束。

BackgroundExec 流程分别注入 store.append 同步拒绝与接受后异步 publish(error)。
调用者在 restore 或 Session.idle 得到存储错误，持久历史没有假 wake；下一次恢复补发一次，
第三次恢复不重复；原 exec/ended 保留且不再生成。repost 自身只记录 warning，
但底层 Timeline 已记录共享故障，因此不能据此说错误被静默吞掉，也不需要新增原地重试。
现有 `idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end` 也重跑通过。

ExecWakeRecovery 用两个已满足通知条件的 ended 身份，初态允许其中一个已经通知。
故障使未持久尾部不能继续提交，后续恢复仅取持久前缀；第二代假设存储成功并公平推进。
同步拒绝和异步失败在模型中抽象为未持久尾部失败，由真实流程分别对应；
候选 ended 的选择沿 ExecLifecycle/ExecStopHistory 和实现查询，不在这里重证查询语义。
OnlyEligibleWakes 与 FailureStopsPersistence 主要检查模型前提自洽；主要结论是已提交通知不重复、
在上述成功重试条件下缺失通知最终补齐，不涵盖任意进程副作用或永久存储故障。

[运行记录](capability-models/live-handoff-results.md)保存配置、哈希、可达窗口和测试结果。
没有发现新的运行时缺陷，没有改运行行为或设计合同。消融：复用现有存储 fixture，只增加
查询门闩与两类 append 故障注入；保留直接重启、cut 两侧和二次恢复，分别区分真正的交接责任。
不复制 Dispatcher 或 SQL，不新增运行时抽象和通用验证框架。
