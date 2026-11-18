# Timeline 合同、实现与模型对应

这是 #1732 的实验范围表，不增加开发规则。源码基线为
`c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`。第一批只新增写入/发布模型；
其余行用于明确既有证据、模型前提及后续缺口，不表示已经逐一重跑所有 Cap。

## 范围从哪里来

按[时间总线总纲](../../architecture/rust-agent/time-bus.md)、
[设计原则](../../architecture/rust-agent/principles.md)、
[插件合同](../plugin-contract.md)、[存储合同](../../architecture/rust-agent/timeline-storage.md)和
[查询合同](../timeline-property-query.md)抽取公共承诺，再由所有者页面解释业务含义。
[22 项分析](timeline-capability-analysis.md)及其[逐项补充](capability-models/coverage-completion.md)
保留自己的基线和有限范围，不把它们的通过记录当成本次 main 的重新验证。

“本批模型”是指定有限配置检查；“已有模型”是链接的局部证据；“源码/流程”表示该实现约束没有由本批 TLC 证明；
“B/C/D”对应 #1732 后续查询、消费交接和组合工作。没有用未展开的 SQL、宿主或插件内部算法填充模型数量。

## 公共条款对应表

下表省略路径的基准如下：运行时 `core/*` 与 `timeline_store.rs` 位于
[`crates/dim-agent-runtime/src/`](../../../crates/dim-agent-runtime/src/)；SQLite 实现位于
[`crates/dim-app-server/src/storage/`](../../../crates/dim-app-server/src/storage/)；能力实现（如 `wait.rs`）位于
[`crates/dim-agent-capabilities/src/`](../../../crates/dim-agent-capabilities/src/)；`plugins/*.md` 位于
[`docs/architecture/rust-agent/`](../../architecture/rust-agent/)。模型名均在 [capability-models](capability-models/README.md) 下，除特别注明。
测试入口：`timeline_batch`、`timeline_tail`、`local_execution`、`session_lifecycle` 位于
[`tests/integration/tests/`](../../../tests/integration/tests/)；`dispatch_flow` 位于
[`crates/dim-agent-capabilities/tests/`](../../../crates/dim-agent-capabilities/tests/)；
`storage_flow/property_queries` 位于 [`crates/dim-app-server/tests/`](../../../crates/dim-app-server/tests/)。
B 的后续查询范围证据见 [#1737](https://github.com/arcships/dim/pull/1737) 的
`docs/design/examples/query-range-verification.md`；与 C 一样，不追溯计入本批模型的覆盖。

| 合同出处与承诺 | 实现入口 | 模型/命题或已有证据 | 前提提供者及缺口处置 |
| --- | --- | --- | --- |
| 总纲 §1、§2：分支序号确定顺序，允许多个写入者 | `core/timeline.rs`：`append_batch_registered_with_create_at`、`commit` | 本批 `SequenceAndBatch`；`timeline_batch` | 提交锁串行化整个分配/登记/入队过程；不证明锁库、调度公平或跨分支全序 |
| principles §4.3：before 同步变换，失败采用原稿 | `Timeline::commit/before`、`AppendGuard` | 本批 `TransformOutcome`；`timeline_batch` 的成功变换路径 | 模型输入是整条 waterfall 的返回结果，不证明用户回调内部算法；no-wait、重入拒绝和中间变换丢弃由源码核对 |
| 存储 §3：登记在入队前；登记失败不追加另由实现/流程确认 | `Timeline::commit` 的 `register` 与 allocated 更新 | 源码/流程：`batch_is_registered_and_published_as_one_snapshot_unit` | 模型只保留 Timeline 结果，不证明生产者登记回调自身的内存回滚；该流程同时覆盖登记错误和 panic |
| principles §4.5：批次一起提交、逐事件消费 | `Timeline::commit/publish`、`Dispatcher::run` | 本批 `AtomicVisibility`、`DeliveryIsOrdered`；`timeline_batch` | TimelineStore 保证原子批次和回调顺序；“决定事实最后”是生产者义务，不由通用总线判断 |
| 存储 §3：分配、落盘、发布不同；只发布已落盘事实 | `Timeline::sequence/committed/publish`、`SqliteTimelineStore::commit_prepared` | 本批 `TypeOK`、`AtomicVisibility`、`ExternalPrefix` | SQLite 事务/fsync 正确性为存储依赖；MemoryTimelineStore 不是磁盘持久化证据 |
| TimelineStore：接受的回调恰一次、按分支序号回调；失败 Session 拒绝后续批次 | `timeline_store.rs` trait；SQLite `Pending::publish`、`commit_round/commit_prepared/append` | 本批 `Resolve/Callback` 的环境约束；既有 storage 流程 | **是模型前提，不是本模型证明所得。** 源码由队列、事务、FnOnce callback 支持；跨 Session 故障隔离不在单分支模型中 |
| 存储故障不推进失败尾部，晚到回调不恢复发布 | `Timeline::publish` 的 failure guard | 本批 `FailureFreezesPublication`、`ClosedMeaning`；`storage_flow` | 允许较早批次已落盘、其回调晚于后一次同步拒绝；不假设已发布位置等于磁盘 head |
| Timeline close 停止追加，排空发布或明确失败 | `Timeline::close`、`publish` | 本批 `NoAdmissionAfterClose`、`ClosingSettles` | 活性依赖 Store/Callback 弱公平；不是整个 Session 关闭或 OS 进程退出的证明 |
| Dispatcher 仅按序交付发布事实 | `EventReader::next`、`Dispatcher::run` | 本批 `Fetch/Deliver`、`TypeOK/DeliveryIsOrdered` | 本批 handler 成功；允许已取出的事件与后来的存储故障交错。回调失败、stop、动态订阅归 C |
| 插件 §5.2：key 过滤、注册后暂存、恢复后消费 cut 后缀 | `Dispatcher::subscribe/consume` | `RecoveryBoundary` 仅给定 cut/后缀；本批基线的 `timeline_tail` 仅作查询/尾部证据，手写暂存而未调用 consume | 注册/暂存/排空/失败及多消费者归 C；#1739 已另交付真实 consume 流程，不将后续证据算作本批基线覆盖 |
| 插件 §5.4、查询 §2–3：固定祖先链和 cut，完整范围关联后分页 | `Timeline::query`、`TimelineReadScope::query_binding`、store query | 已有 `RecoverySelection`；`storage_flow/property_queries`、`timeline_tail` | B：祖先切点、落盘中的尾部、游标绑定与关联域组合；本批 ReadExternal 只有成功读取的发布上界 |
| 查询错误不是空集合；字段、missing/null、group/pick 语义 | `query.validate`、memory query、SQLite query | 既有 property-query 对照流程 | B：列出实际恢复使用的逻辑子集及剩余语义；SQL 编码/解析由实现测试承担，不能假设整个查询引擎已证明 |
| 总纲 §2、§4：决定依赖自身已消费状态；消费者处理迟到请求 | Loop、ModelProvider、ToolExecutor 的 consume/任务入口 | 已有 `LoopProtocol`、`UndispatchedTools`、tool-model/ToolSegments | D：生产者与消费者各自消费位置及中断交错；不以全局取消位代替 |
| principles §4.1–2：命令由 owner 决定，无效果不写结果，交接后不撤回 | InputQueue `step/withdraw`、Loop `consume`、Approval 响应处理 | 已有 `InputOrdering/InputRecovery/InputReceipts`、Approval 模型 | D：实际交接事件与上下游前提逐条衔接；模型 bookkeeping 结果不自动代表持久化结果事件 |
| 总纲 §3：跨代业务状态恢复后对未来决定等价 | 各 owner 的 rebuild；查询 §5 的恢复选择表 | 已有各恢复模型，见下一表 | 不是全部内存快照相等；外部状态重读、业务映射分别核对；B/C/D 尚未组合证明 |
| 总纲 §5、插件 §7：旧工作结算、新代明确接管、不重执行副作用 | single-tree 重载协议、各 owner cleanup/rebuild | 已有 `WaitRuntime/ExecLifecycle/GoalLifecycle`；`local_execution/session_lifecycle` | D：不同代进度、终态写入失败及重启；不承诺重载期间调用必成功 |
| 插件 §5.1：开放事件，多写入方、消费者动态加入，历史不随生产插件消失 | EventKind extension 解码、Timeline append、store 读写 | 既有 `extension_events_keep_their_own_rows_and_restore_unchanged` | C：注册退出与历史独立；包卸载/外语 bridge 由实际装配流程承担 |
| principles §4.6：只有采用、交接的内容进入上下文 | InputQueue、Prompt、history、ContextPreparation | 已有 `PromptAdoption/PromptRetirement/FinalRequest/RequestCuts` | 内容 cut 与后续控制 cut 分开；由所有者保证输入资格，A 不作业务过滤；跨模型衔接留 D |
| 插件 §7.1：激活才启动后台/输入工作，closing 停止准入并结算 | runtime lifecycle、各 part `activated/closing` | 既有 `session_lifecycle/TreeOpening/WaitRuntime` | A 的 Timeline close 不等于 part closing；逐 owner 启动/清理覆盖不能由 A 推出 |
| 总纲 §6、principles §2：跨总线读取及交接归业务 owner | BranchLine、Collaboration、TimelineTree | 已有 `BranchLine/CollaborationHandoff/TreeLifecycle` | 跨 Session 故障、动态成员和任意深树非本批范围；沿现有 owner 分析继续闭合前提 |
| principles §1、§3、§4.4/7/8/9：作用域、依赖、通用机制、只存必要事实 | 装配、命名服务、生成 catalog、owner 的事件声明 | xtask、设计评审及现有行为模型 | 架构/设计约束，不能伪装成事件序列不变量；无新 Capability、服务面或机制需求 |
| 对外投影可见性、消费故障原样传播 | AgentIndex pending、TimelineContext idle、Dispatcher failure | #1720/#1721、#1718/#1719 的独立修复 | A 允许 published > consumed；不能凭外部读上界推出 pending 已更新。以原 owner 的批准合同/合并状态为准，C/D 跟进 |

## 所有者恢复合同不能统一成一种算法

此表是公共合同到现有业务分析的索引，不是新增一轮所有者完整审计。具体命题和有限边界沿用
[模型清单](capability-models/README.md)，本批未重新执行那些配置。

| 所有者 | 设计/实现入口 | 已有模型与尚待闭合的前提 |
| --- | --- | --- |
| Wait | `plugins/Wait.md`；`wait.rs::rebuild` | `RecoveryBoundary/WaitRuntime`：只恢复 open-turn，丢弃通知/旧等待；固定 cut 与动态 consume 由 B/C 提供 |
| InputQueue | `plugins/InputQueue.md`；`input_queue/recovery.rs::rebuild` | `InputRecovery/InputOrdering`：成员、首个控制决定、handover/turn；先等 committed cut 发布再查询，不能只做 start-minus-end |
| Approval、ToolExecutor、ModelProvider、ContextPreparation、Loop | [逐项分析 §2–8](timeline-capability-analysis.md#2-approval) 中链接的 owner 页与 rebuild/恢复查询 | `RecoverySelection` 仅适用于对应未结算身份子问题；effects、控制事件、旧 request/turn 及结算次序仍依赖各业务模型，D 不可只复用集合公式 |
| Prompt、ToolBinding、ContextUsage | [逐项分析 §9–11](timeline-capability-analysis.md#9-prompt) 中链接的采用、projection/checkpoint、usage 入口 | `PromptRetirement/ToolRetirement/ContextUsage/UsageIdentity`：持久业务身份与当前资源身份不同；B 的选择与 D 的采用交接待闭合 |
| BackgroundExec、ShellSession、FileReads | [WorkspaceTools](../../architecture/rust-agent/plugins/WorkspaceTools.md) 与 [Shell 恢复](../shell-exec-recovery.md)；exec 恢复、FileReads 绑定 | `ExecLifecycle/ExecStopHistory/FileBindingQueries`：历史任务目录、遗漏通知与不恢复 I/O 绑定不同；保留 Shell 修复证据，不重执行命令 |
| Goal、Hooks | [Goal §18](timeline-capability-analysis.md#18-goalbranch)、[Hooks §19](timeline-capability-analysis.md#19-hooksbranch) 的 load/pause/consume | `GoalLifecycle/HookScheduling`：Goal 恢复 paused；Hooks 不历史重跑；不能统一要求恢复原运行状态 |
| TimelineTree、BranchLine、Collaboration | [逐项分析 §15–17](timeline-capability-analysis.md#15-treetimeline) 的目录、cursor、请求恢复入口 | `TreeLifecycle/BranchLine/CollaborationHandoff`：读取目录/外部成员、跨分支 cut 由各 owner 定义；B/D 跟进 |
| EnvironmentClients、SettingsSnapshot、SessionBroadcast | [逐项分析 §20–22](timeline-capability-analysis.md#20-environmentclientsbranch) 的 apply/发布任务 | `CurrentInput/SnapshotRefresh/SessionBroadcast`：当前外部状态、重试策略和投影不同；不是历史未完成请求恢复 |

## 本批写入/发布模型的解释

[TimelinePublication](capability-models/TimelinePublication.tla) 用两个可反复写入的生产者，
分别检查“两批、每批 1–2 个事件”和“三批、每批一个事件”。身份是有限实验取值，不限制生产者名单。
不改变已有模型，也不引入生产代码。

| 动作 | 对应代码及抽象 |
| --- | --- |
| `CommitBatch` | 提交锁内的 `commit`：before 返回结果、登记成功、连续编号、热窗口及 store 入队。`accept=FALSE` 表示 **allocated 已增加后** store 同步拒绝，Timeline 随即故障；不是登记失败。对应 store closing 时的同步拒绝；已记录的 Session 存储故障通常在分配前拒绝，不属于这个窗口 |
| `Resolve(last, ok)` | 存储轮完成若干连续批次，整个轮成功或失败；允许轮中多个批次一起落盘。其他 Session、SQLite rollback/retry 与编码抽象为端口结果 |
| `Callback` | 按批次顺序调用 `Timeline::publish`；成功可在 durable 之后推进 published，已有 Timeline failure 则忽略。store failure 后剩余接受批次仅能失败 |
| `Fetch/Deliver` | reader 先取一条已发布事实，成功 handler 后才推进 Dispatcher 位置。两个动作保留取出后发生存储故障的窗口；不覆盖 handler 错误或卸载 |
| `ReadExternal` | 一次成功外部范围读取的上界与前缀（也允许故障后的成功历史读）；不展开 query I/O、失败、分页或祖先。返回旧快照合法，不把它当成最新投影 |
| `Close` | 获取同一提交锁后设置 closing，并等待已分配尾部发布或 failure。不会删除已落盘历史 |

`SequenceAndBatch`、`AtomicVisibility`、`TransformOutcome`、`ExternalPrefix`、`DeliveryIsOrdered`
由动作定义直接维持，是模型定义的自洽检查（self-consistent by definition），不作为独立的实现验证成果。
它们不证明 Rust 的锁、waterfall 或存储实现。
本批主要检验的是这些约定组合后，在轮提交、逐批回调、读取消费、关闭及同步/异步失败之间的交错。
`Resolve` 要求上一轮回调全部完成（`resolved = callbacks`）才完成下一轮；
不覆盖上一轮尚有回调时下一轮已经完成的流水交错，不能将本结果扩展到任意存储轮并行。

`ClosingSettles` 只在存储完成和回调获得弱公平调度时成立：环境最终提供成功或失败，而不是永远悬挂。
不要求写入一定成功，不假设 handler 最终完成，也不证明整个 Session 关闭。
没有状态约束剪掉越界失败路径；有限性来自写入批数和批大小，`CHECK_DEADLOCK FALSE` 允许完成态/等待态停留。

`ClosedMeaning` 只用于一次 close，模型没有 idle thaw；实际 thaw 可清除 closing，
而 closed 要到后续 publish 才改变，因此不能把该式外推为整个运行期的不变量。

本模型从空分支开始，不检查重启恢复，也不把内存热窗口清理、fsync、事件 ID 随机唯一性或 SQL 实现作为已证明结论。
`consumed <= published <= durable <= allocated` 是本模型边界；真实错误会使旧已分配尾部不再可用，不能据此承诺错误后内部查询仍返回它。
尚未读取的发布事实可以因故障而停止交付，已从 reader 取出的一个事实则可能完成交付。

## 复现、证据与消融

命令、哈希、可达场景和实际结果见[本批运行记录](capability-models/publication-results.md)。
本批只运行新增配置和文档检查；表中 Rust 流程是既有证据入口，不是本批重新运行记录。

消融删除了只记录“登记拒绝发生过”的布尔量及其动作：它对 Timeline 是 stuttering，
不影响发布命题，登记失败的真实流程证据已存在。删除后重跑两个配置。保留一个共享模型与两个配置：第二配置覆盖三个批次的回调/故障位置，不复制模型。
未引入锁调度器、真实数据库模拟、插件框架或全体 Cap 状态。`durable/published`、`Fetch/Deliver` 不能合并，
因为分别承担落盘未发布和取出后故障的实际窗口；read 快照承担外部可见性，批次边界承担原子发布，
其他字段用于相应命题或场景识别。A 完成不关闭 B/C/D，也不宣称需求全集已经证明完整。
