# Timeline 关联部件逐项分析

范围：仓库中清单列出的 22 个部件及 50 个核心事件 key、5 个生产扩展事实。
代码基线 `8a5fe0980`；分析与实验进度、缺陷跟踪见 [#1629](https://github.com/arcships/dim/issues/1629)。
这是对实现的有界分析，不是第二份设计规范，也不声称逐行证明整个程序或穷尽所有运行环境。

## 方法与共同前提

每项分别写使用场景、最小状态/转换、恢复行为、需要成立的性质以及代码/流程证据。
单个状态表用来解释业务边界，不要求代码改写成纯状态机。已有流程测试检查的是实际边界，
有限模型检查的是声明的转换；两者结合也不意味着已经证明题设完整。

`H≤c` 表示一个固定截止位置的可见历史及允许的祖先切点；`last(K,c)` 取其中最后一个 K。
`unmatched(A,B,k,c)` 表示 c 内有 A、无同关联键 k 的 B。这个记号是查询结果定义，
**不是恢复时顺序执行历史事件的算法**。实际查询的发布边界、祖先、分页规则由
[查询契约](../timeline-property-query.md)与 [Dispatcher](../plugin-contract.md#52-consuming-facts)提供。

下列分析依赖事件生产方遵守已有类型/身份契约，不假设只有清单中的插件能写或读事件。
副作用完成、取消请求、释放资源、终态提交和终态持久发布不是同一个时刻。
跨代恢复必须分别处理业务状态与原代工作，不能用恢复出状态替代结束原代工作。

## 分类：依赖扫描不等于业务判断

生成目录按 Timeline/Dispatcher 依赖列项；[Capability 契约](../plugin-contract.md#6-capability)
按折叠分支事实或写自身领域事实定义。下面 18 项有明确的业务状态/领域事实边界；另 4 项
分别是输入发布、资源绑定或通知投影，不能仅凭依赖强行套上业务状态机恢复要求。
这是本次分析的分类，不更改生成器、装配或所属设计。

| 分类 | 部件 |
| --- | --- |
| 业务状态/领域事实部件 | Approval、BackgroundExec、BranchLine、CollaborationTimeline、ContextPreparation、ContextUsage、GoalBranch、InputQueue、Loop、ModelProvider、Permission、Prompt、ToolBinding、ToolExecutor、TreeTimeline、WaitBranch、HooksBranch、ShellSession |
| 当前外部状态的输入发布 | EnvironmentClientsBranch、SettingsSnapshotBranch |
| I/O 绑定与查询辅助 | FileReads |
| 基础设施的无业务状态投影 | SessionBroadcast |

## 1. Permission

- **场景/状态**：工具准入读取当前策略；保留已登记版本 W、已派发版本 V、生效策略 P，不能把 W 与 V 合并。
- **转换**：expected=W 才登记更新；派发后 P/V 才改变并答复调用者。并发更新检查的是 W；关闭释放未完成等待者。
- **恢复**：P 为祖先链/本分支内最后策略，无记录则用 Session 初值；V 只取本分支版本，祖先策略继承时 V=0；不读取工具历史来恢复策略。
- **性质/论证**：提交锁内的版本比较使两个相同 expected 更新至多一个登记；按序派发维持 P 与 V 对应。授权算法还依赖调用时的文件事实、命令规则和 advisor，不能由版本论证推出所有授权正确。
- **对应**：[permission.rs](../../../crates/dim-agent-capabilities/src/permission.rs) 的 update/consume/rebuild；[审批交接分析](approval-handoff-analysis.md)；permission_flow 的版本、路径、规则和 advisor 流程。

## 2. Approval

- **场景/状态**：Ask 调用产生 pending；响应命令可以有多个；pending 的取走者只有一个。登记、持久发布、派发和准入返回分开。
- **转换**：响应消费、取消、自动策略结算竞争 pending；自动结算复核捕获位置，持久发布后再复核准入。advisor 的 Ask 不被普通 Allow 自动解除。
- **恢复**：`unmatched(approval/request,{respond,cancelled},approval_id,c)`，仅取 ID，补取消，不恢复旧提问或原工具执行。
- **性质/论证**：互斥 take 支持单个获胜响应；提交位置复核防止自动结算越过已提交响应。不能要求一个 ID 仅有一个“看起来像终态”的事件。关闭 pending 本身不是已经唤醒的原生调用全部结束的证明，调用者取消需单独覆盖。
- **对应**：[approval.rs](../../../crates/dim-agent-capabilities/src/approval.rs)；[Approval 模型和范围](approval-handoff-analysis.md)。实际审批顺序、持久发布测试与该模型检查不同层面。

## 3. ToolExecutor

- **场景/状态**：批次、绑定处理器、每调用准入/执行/结算状态、连续执行段剩余数和已结算前缀；子批次另带 parent_call_id。
- **转换**：所有调用先独立准入；相邻 Concurrent 合成一段，Exclusive 独占段；只有前面所有段结算后才能进入处理器。拒绝、panic、取消、future drop 都释放本调用的段计数；中断与重载取消并 join 工作。
- **恢复**：按 call_event_id 查询无 result 的批次，只取调用身份；先结算子批次，再把已确认子结果 effects 归入对应父调用。不得重跑历史参数或把取消当回滚。
- **性质/论证**：段剩余数等于尚未结算调用数，head 只能越过零计数前缀；后段提前拒绝不能越过仍运行的前段。`Settled::drop`、`Turn::new` 与[ToolSegments 模型](tool-model/ToolSegments.tla)对应。模型中结算与 head 推进可分开调度。
- **边界**：模型的启动指已观察取消标志后的调度决定，不是取消与外部系统副作用之间的原子屏障；不合作处理器受宿主截止时间约束。子批次五秒 join 宽限是现有边界，不能声称任意宿主都先于外层终态结束。准入与段后实际调用之间可能有时间间隔，不擅自增加二次授权要求。
- **对应**：[tool_executor.rs](../../../crates/dim-agent-capabilities/src/tool_executor.rs)、[ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md)；flows、local_execution 的批次顺序，interrupt、session_lifecycle 的取消/卸载流程。

## 4. InputQueue

- **场景/状态**：条目 ID、mode、released，handover 所属 turn，当前 turn/starting 与计数。普通队列条目与 handover 不可混为一类。
- **转换**：Queue 在已有 turn/starting 时等待；Steer/Append 立即交接，Append 本身不驱动新回合。普通输入 released 后不能撤回。接受 queued-input Steer 时，替代 input/message 与原 input/deleted 同批提交，替代在前。handover 由对应回合消费或结束清理。
- **恢复**：固定已发布截止点查询当前普通/handover 成员；按页读其首个 applied/withdraw 控制；仅 owed replacement 才读被选中候选正文。已提交删除/替代批次从成员集合中排除，不能重复替换。
- **性质/论证**：released 是普通输入交接的单向位，withdraw 只处理未 released 的条目；删除后原 ID 不再有 entry，重试不产生第二次替换。纯 Append 没有驱动位；stop 清 starting 不是授权释放旧队列。handover 的独立规则使“所有输入都必须先 applied”这个命题不成立。
- **对应**：[QueueState::step/withdraw/release](../../../crates/dim-agent-capabilities/src/input_queue.rs)、[recovery](../../../crates/dim-agent-capabilities/src/input_queue/recovery.rs)、[队列契约](../../architecture/rust-agent/plugins/InputQueue.md)；flows、branch_flow、interrupt_flow。

## 5. Loop

- **场景/状态**：一个 turn、一个当前 request/stage，已交接输入与工具身份，compaction 状态、ending 标志和 continuation 状态。
- **转换**：有 driving input 且无当前 turn/压缩才写 turn/start；step/model-start → context/prepare → model output → tool batch → 下一步骤/turn/end。中断抑制后续调用，reload-settled result 使回合失败；终态必须关联本 request/turn。
- **恢复**：读当前 input 成员、其采用事实、最后回合/中断边界、未结算压缩；仅开放回合读取请求控制字段，补终态而不构造旧模型请求/工具参数。
- **性质/论证**：`try_start` 的 turn/compaction guard 和 ending 位分别禁止并行回合与重复结束；消费方只处理当前身份，晚到旧结果不能成为新请求结果。仍依赖各工作所有者的结算与正确关联字段，不能只检查 Loop 的一个枚举。
- **发现**：当前 Loop 文档描述 `step/disposition`，该基线 EventKind 和生产代码没有这个 key；不能用这段文档作为已实现证明。代码的 deferred/continuation 行为必须按实际入口核对，文档差异单独跟踪。
- **对应**：[agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs) 的 try_start/end/rebuild/consume；flows、interrupt_flow、context_flow、session_lifecycle；[Loop 页](../../architecture/rust-agent/plugins/Loop.md)。

## 6. WaitBranch

- **场景/状态**：当前回合 open、待取通知、waiting 槽、generation token 和已接受调用 tracker；不是永久任务消息箱。
- **转换**：open 回合内的合格通知或 Steer 可结束等待；匹配 task ID 取对应通知；turn/end 清通知；reload 终止旧调用并拒绝保留原生句柄的新调用。
- **恢复**：`open=(last({turn/start,turn/end},c)=start)`；pending 通知和旧调用不恢复。工具结果归 ToolExecutor 写，不由 Wait 另写终态。
- **性质/论证**：open 的最后边界等价于对边界规则的归纳；接纳和关闭共用锁，tracker 登记发生在解锁前，关闭后 drain 不会漏过新接纳；释放槽早于释放 tracker。进度仍依赖调用 future 被 poll 或 drop。
- **对应**：[wait.rs](../../../crates/dim-agent-capabilities/src/wait.rs)、[Wait](../../architecture/rust-agent/plugins/Wait.md)、[#1619](https://github.com/arcships/dim/pull/1619)；复用 #1616 的有界恢复模型，当前 local_execution 的原生、face、完整工具 reload 流程验证新实现。

## 7. ContextPreparation

- **场景/状态**：一个当前 request/cancel、已采用配置、请求局部 projection 和压缩任务；handler 只登记工作，不等待外部模型或存储。
- **转换**：prepare 成功提交 model/call；失败/取消提交 model-output-end；压缩 start/result 成对关联。stop/steer 取消工作，自动压缩期间先等已有压缩结果再替换准备请求。
- **恢复**：查询没有 model/call 或 output-end 的 preparation 身份，以及无 result 的自动 compaction-start；只加载结算字段，失败旧工作，不重复摘要模型调用。
- **性质/论证**：single-flight guard 使当前准备只有一个；成功交接与失败终态互斥取决于工作结果及取消检查。内容读取固定内容 cut，重试控制有独立 cut，不能把后来的输入偷放进旧请求。配置批次的决定事实放最后，使逐条消费者不会先看到完成配置再缺少组成事实。
- **对应**：[context_preparation.rs](../../../crates/dim-agent-capabilities/src/context_preparation.rs) 的 rebuild/run/prepare 与 plugin cleanup；[契约](../../architecture/rust-agent/plugins/ContextPreparation.md)；context_flow、prompt_boundaries、interrupt_flow。

## 8. ModelProvider

- **场景/状态**：一个当前调用、取消与重试预算、输出块/工具参数的已发布进度；这些 I/O 控制状态不跨代恢复。
- **转换**：model/call 启动外部请求；流产生 thinking/text/tool-use/content/usage，最后 output-end。可重试错误只有在未跨不可逆输出边界时退避重试；stop/reload 取消并 join。
- **恢复**：未结束 model/call 的身份、provider/model 与失败链、工具调用参数完成标记；写失败终态，不读取历史 messages/tools/system 再调用模型。
- **性质/论证**：一个 current slot 避免双调用；输出身份与请求身份关联，终态后释放运行槽。是否允许重试由错误种类与已发布进度共同决定，不能只数失败次数。max_tokens 是 output_limit 路径，不等于压缩预算不足。
- **对应**：[model_provider.rs](../../../crates/dim-agent-capabilities/src/model_provider.rs) 的 rebuild/run 与 [output.rs](../../../crates/dim-agent-capabilities/src/output.rs)；[契约](../../architecture/rust-agent/plugins/ModelProvider.md)；flows、interrupt_flow、context_flow 的模型失败/恢复流程。

## 9. Prompt

- **场景/状态**：配置命令、加载中的操作、已采用 Snapshot、scope 与串行 adoption；加载成功与采用成功不同。
- **转换**：configure-request/result 进入所属消费；串行采用通过 ConfigurationPublisher 原子提交 prompt/changed、input/message、context/configured；过时加载不能覆盖较新的采用。
- **恢复**：恢复结果与必要未处理 source suffix 在固定 cut 上构造业务状态；不恢复旧加载 future。live 路径等 Dispatcher 达到 source cut，而非再次抓一遍正文；scope 退出可写 retire 输入。
- **性质/论证**：采用按 source 位置单调发布，不以完成时间排序；恢复结果与退役输入同批防止“状态已退役、输入仍有效”。drop 回复等待者不撤销已提交命令，动态插件加载也不能绕过同一采用路径。
- **发现/边界**：`context/scope` 有类型和消费逻辑，但该基线没有找到专门的生产写入路径；这代表路径覆盖未建立，不等于生产消费者本身错误。
- **对应**：[prompt.rs](../../../crates/dim-agent-capabilities/src/prompt.rs)、[prompt/recovery.rs](../../../crates/dim-agent-capabilities/src/prompt/recovery.rs)、[Prompt](../../architecture/rust-agent/plugins/Prompt.md)；prompt_lifecycle、prompt_boundaries。

## 10. ToolBinding

- **场景/状态**：当前注册目录、已采用工具配置、每 request 的处理器 pins；名字不是持久可执行身份。
- **转换**：registry watch 先注册再初读；opening 与后续变更经过 Prompt 采用；prepare 固定声明和 handler 快照，bind 只接受该快照支持的选择。相同名字的后来替换不能偷偷替换已准备的请求处理器。
- **恢复**：从已采用配置与当前 registry 重新协调；不把历史工具定义当代码执行。已退休处理器按其自身 stopping 合同失败，continuation 的空声明对应空绑定。
- **性质/论证**：声明与 pins 来自同一个保留快照，避免准备中途注册变动造成“模型看 A、实际调用 B”；采用失败保留旧 prefix/pins，不发布半配置。
- **对应**：[tool_binding.rs](../../../crates/dim-agent-capabilities/src/tool_binding.rs) 的 prepare/bind_current/插件任务；[ToolBinding](../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding)；tool_binding、prompt_lifecycle。

## 11. ContextUsage

- **场景/状态**：epoch、branch 顺序、当前累计贡献、saved/source/covered 位置、计数算法和当前请求数值关联。
- **转换**：可渲染来源改变数字后写 context-usage/recovered；确认无影响尾部才写 covered；provider measurement 只校准当前经数值验证的请求关联。unknown schema/算法失败而非默认为零。
- **恢复**：最新数字结果及对应 covered，再检查有无未覆盖相关来源；有缺口则 unavailable，等下一次真实请求的已有 projection 初始化，不能为恢复数字遍历历史 messages。
- **性质/论证**：covered 只能延长已确认无贡献尾部，不能冒充重新计数；来源位置与 epoch 联合定位避免不同分支/压缩段相加。请求对象或算法变化使旧校准失效。可用性下降不等于业务模型调用已失败。
- **对应**：[context_usage.rs](../../../crates/dim-agent-capabilities/src/context_usage.rs) 的 Machine/cover/恢复及初始化路径；[契约](../../architecture/rust-agent/plugins/ContextUsage.md)；context_flow 中数字、压缩、恢复与拒绝历史正文读取的流程。

## 12. BackgroundExec

- **场景/状态**：task ID → 进程/终态，Session hold、closing、跟踪任务和待发 wake；turn stop 与 branch close 不同。
- **转换**：先取得 Session hold 再 started；supervisor 先释放进程树再 ended；有效终态另写带 end-event ID 的 input/message。turn stop 保留后台任务；branch close 取消且不 wake；reload drain 原代进程。
- **恢复**：`unmatched(exec/started,exec/ended,task_id,c)` 补失败；再找符合分支开闭边界、无关联 wake 的 ended 事件补通知。fork 切点防止把复制来的旧任务当本 Session 工作。
- **性质/论证**：task 状态的取走/结算防止 stop 与自然完成双结算；wake 的幂等关联键是 ended 事件 ID，而不是模糊的描述或时间。结束事实与 wake 之间允许中断，必须由后者的缺失查询补齐，不能假设两者天然原子。
- **对应**：[background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) 的 consume/rebuild/unposted_filter/unload；[BackgroundExec](../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)；local_execution 的 stop/close/reload/unposted-wake 流程。

## 13. ShellSession

- **场景/状态**：最小执行 profile 的 PTY generation、前台队列、当前命令、后台 task map 与输出 marker；不是 BackgroundExec 的别名。
- **转换**：队列串行进入持续 shell，generation 区分旧 shell 输出；后台/占用命令写 exec/started，完成经 settle 写 ended 和 wake；正常卸载取消 lifecycle、终止 PTY、释放排队调用、join，再结算剩余后台任务。
- **恢复**：新实例创建空队列/PTY/task map。`ShellSessionPlugin::apply` 没有 Dispatcher::consume 或 Timeline 查询；注入 Dispatcher 不能作为已实现恢复的证据。
- **发现**：宿主进程崩溃、未运行正常卸载而留下 started 无 ended，或 ended 已落盘而 wake 未写时，新最小 profile 未找到补终态/补通知路径；该 profile 不装 BackgroundExec，不能借它的恢复保证。正常 close 测试不能排除这个崩溃窗口。本次作为静态源代码缺口记录于 [#1633](https://github.com/arcships/dim/issues/1633)，未运行真实进程崩溃重启实验，不把推断写成已复现缺陷。
- **对应**：[shell_session.rs](../../../crates/dim-agent-capabilities/src/shell_session.rs) 的 apply/settle/handle_shell_exit；[defaults::install_minimal_execution](../../../crates/dim-agent-capabilities/src/defaults.rs)；minimal_execution 的正常关闭、持续 shell、后台执行流程。该测试文件引用的 persistent-shell.md 在此基线缺失，也是文档导航缺口。

## 14. FileReads

- **场景/状态**：规范文件路径 → 编码/BOM/换行/原始 hash 和建立位置；属于本代 I/O 绑定，不是持久权限或文件锁。
- **转换**：读/写建立绑定；edit 仍核对目标 oldText；write 检查绑定是否被后续已完成压缩切出模型上下文。
- **恢复**：新实例空 map，重新读文件建立绑定；只查询建立位置后的 completed context/compaction 及 retain_from 位置，不重建历史文件正文。
- **性质/论证**：只有成功压缩且保留边界越过建立位置才让 write 依据失效；不把 hash 当跨进程 CAS。其状态可随 I/O 生命周期丢弃，不能为了“所有 cap 必须恢复”而新增持久快照。
- **对应**：[file_reads.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/file_reads.rs) 的 get/established/in_context；[FileReads](../../architecture/rust-agent/plugins/WorkspaceTools.md#filereads)；workspace_files、context_flow/compacted_bindings。

## 15. TreeTimeline

- **场景/状态**：branch part 接收本父分支的 create/end，真正的目录、reservation 与子树资源归 Session 的 TimelineTree。
- **转换**：create 登记异步构建；成功 opening 或 create-failed；end 按父子生命周期结算，工具发起的 end 可等回合结束再关闭。handler 不等待整项构建。
- **恢复**：TreeTimeline 本身不重放旧 create；目录、未完成 creation/end 的修复由 TimelineTree 恢复。不能在两边各运行一次相同恢复。
- **性质/论证**：reservation 先登记再构建，close 能找到已接受创建；父/Session 关闭允许取消尚未完成创建，不能许诺每个 create 一定存活。子树结束与结果关联 branch/source 身份。
- **对应**：[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 的 TreeTimelinePlugin 与 tree create/end/recovery；[TimelineTree](../../architecture/rust-agent/plugins/TimelineTree.md)；branch_flow 的 interrupted create/end、descendants-first、opening/closing 竞争。

## 16. BranchLine

- **场景/状态**：本分支已知子线 roster、parent read cursor、待发布 parent block 和当回合是否已通知；依赖目录的当前子线而非完整历史。
- **转换**：子线变化更新 keyed roster；父亲 turn/end 后基于 read cursor 发布 parent block；tool line 结束按原因/当前驾驶输入决定 idle/ended notice。失败或拒绝的 steer 不算已通知父线。
- **恢复**：从 Dispatcher committed cut 挂载；按当前子线创建位置、最后 roster/parent block 和目标 read cursor 精准查询。旧 turn 进度不恢复，恢复补写 turn/end 不触发新的 idle notice。
- **性质/论证**：parent block 只报告身份与位置而不复制父正文；通知依据 line 消费态而非一个原始父信号，消费/删除 held block 后可重算。不同目标 cursor 不混用。
- **对应**：[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 的 BranchLine 与 writer/read-cursor 查询；[BranchTool](../../architecture/rust-agent/plugins/BranchTool.md#branchline)；branch_flow 和 local_execution 的分支交接流程。文档 `tool/use`/`tool/result` 拼写应与实际 `step/tool-use`/`step/tool-result` 区分。

## 17. CollaborationTimeline

- **场景/状态**：每个参与分支的 open request、ops/chains、forwarded approval 与未交付 input；Session 协作服务负责关系和任务。
- **转换**：request/message 产生对方输入；delivered 记录交接；result 关闭关联请求；cancel/close 由实际所属请求/成员处理。成员多轮工作不能每个 turn/end 都当整项协作完成。
- **恢复**：查询无 delivered 的协作命令、无 result 的 request 和未交接审批输入；结合当前关系/成员是否存在决定终结与清理，不重发已经交付的历史工作。过期 held input 经 InputQueue 撤回，不直接改其状态。
- **性质/论证**：request_event_id / input_id 是交接身份；不同分支各自有序，不假定跨 Session 全局序号。恢复时成员缺失必须得到明确结算，不能恢复一个失去所有者的运行句柄。
- **对应**：[collaboration.rs](../../../crates/dim-app-server/src/collaboration.rs) 的 consume/restore/unload 及审批子模块；transport_flow/collaboration 的正常交接、取消、成员卸载与恢复流程。owner-independent Timeline 提案不是此实现已具有跨 Session 持久协调器的证据。

## 18. GoalBranch

- **场景/状态**：objective、业务状态/判断方式持久；当前 continuation/任务/评估关系、去重与写入工作仅本代内存。
- **转换**：set/resume 才开始一代执行；edit/pause/clear/completed/blocked 写完整 goal/state。普通输入、回合、任务完成推进本代判断，不因为每次消费都写恢复记录。
- **恢复**：选最后 goal/state；active 映射为内存 paused(interrupted_reload)，不补写 paused、不重新执行旧决定。只有后续显式 set/resume 重新查询当前未消费输入/未结束任务身份并启动。
- **性质/论证**：恢复函数 `R(active)=paused, R(other)=other` 丢弃本代集合，因此旧 turn/end/exec/ended 无法单凭历史再次启动；完整状态结果无需沿 previous 链重放。恢复与查询不要求目标文本丢失。
- **对应**：[branch.rs](../../../crates/dim-goal/src/branch.rs)、[read::load/pause_reloaded](../../../crates/dim-goal/src/read.rs)、[Goal 状态契约](../plugins/Goal-state-read.md)；dim-goal 的 flow/load/product/query-cost 流程。

## 19. HooksBranch

- **场景/状态**：已消费 view、排队 hook point、独立 PermissionRequest token、当前 Stop follow-up round；source 配置与命令资源归自身。
- **转换**：普通 hook point 顺序执行，每点 handlers 并发合并；PermissionRequest 独立取消、不挡队列。写出前重新确认触发事实没被中断或更新的已消费操作员输入越过；PreToolUse advice 不越过 Permission 的 Deny。
- **恢复**：从当前 committed cut 挂载，不回放历史脚本。SessionStart 先看 turn/start 存在性及当前 keyed startup contribution；旧 recovery-settled result 不触发脚本。
- **性质/论证**：失效边界用 input/consumed，而非“用户提交了输入”，否则排队输入会误删有效输出；审批已结束取消对应 hook；Stop 只有 blocking 输出才发 follow-up，连续上限三次由实际首请求输入确定。
- **对应**：[hooks/runner.rs](../../../crates/dim-agent-capabilities/src/hooks/runner.rs) 的 consume/validation/插件起始 cut 与 worker；[Hooks](../../architecture/rust-agent/plugins/Hooks.md)；hooks_flow 的输出失效、审批取消、启动与 follow-up 流程。

## 20. EnvironmentClientsBranch

- **场景/状态**：当前客户端 presence 的字符串快照和本代 last；输入只是声称的客户端环境，不授予信任或权限。
- **转换**：先注册 ClientsChanged listener，再在 activation 后读当前值；变化才 append 自己的 input/message，成功后更新 last；关闭/代取消结束任务。
- **恢复**：重新读 SessionClientsHost 当前状态，不恢复已断开的客户端；last 清空允许本代再次报告当前值。
- **性质/论证**：先监听再初读避免丢初值期间的改变，notify 合并后重读当前值而不是逐个回放连接通知。失败 append 不更新 last，但这里没有定时 retry，直到下次唤醒才再试；不能承诺无新信号时必然重试成功。
- **对应**：[environment_clients.rs](../../../crates/dim-agent-capabilities/src/environment_clients.rs)；environment_clients。它只写采用输入，按现有行为更接近输入发布部件，不为它增加恢复状态机。

## 21. SettingsSnapshotBranch

- **场景/状态**：执行设置读入口、本代 last、pending retry；设置本体归其 owner。
- **转换**：watch 变化唤醒读取，格式化后采用 input/message；读失败或 append 失败保留 pending，一秒后重试；成功才替换 last。
- **恢复**：激活后重读当前设置，不从旧输入推回用户配置。停止依赖 branch closing / generation token。
- **性质/论证**：last 只记成功采用值，pending 使临时失败不被当成“已经发布”；相同值不不断写相同输入。重试完成依赖服务和存储恢复，不是无条件活性保证。
- **对应**：[execution_settings/prompt.rs](../../../crates/dim-agent-capabilities/src/execution_settings/prompt.rs)；environment_settings。与客户端环境发布相似，但不能把它的 retry 保证错误归给上一项。

## 22. SessionBroadcast

- **场景/状态**：无业务折叠状态；把分支 Dispatcher 已交付的 event 包装为 Session channel 通知。
- **转换**：subscribe_all 收到事实 → 带 branch_id emit；Session 关闭通知由 SessionBroadcastEnd 在分支关闭后发出。emit 不等客户端消费确认。
- **恢复**：不重播旧通知；重连/历史补读归 Session/RPC 客户端协议，不能通过加载此部件再次运行旧事实。
- **性质/论证**：event 与 branch 身份直接搬运，不解释业务类型；每分支顺序由 Dispatcher 提供，但不同分支与无序号 SessionNotice 没有共同全序。此处不能承诺端到端 exactly-once。
- **对应**：[session_broadcast.rs](../../../crates/dim-agent-capabilities/src/session_broadcast.rs)；dispatch_flow、session_lifecycle、transport_flow 的交付与关闭测试。它是投影机制，不因为依赖 Dispatcher 就拥有业务状态。

## 工具执行段模型的范围

[ToolSegments.tla](tool-model/ToolSegments.tla) 与 [配置](tool-model/ToolSegments.cfg) 使用 4 个调用，
穷举 16 种 Concurrent/Exclusive 标记。每个调用可独立准入、运行、拒绝或结算；后段可先
拒绝，head 推进是独立动作。检查计数一致、head 仅越过已结算前缀、执行顺序、Exclusive
不与另一调用同时运行，以及已观察取消后不开始新运行。

Settle 抽象正常返回、拒绝、panic 和 future drop 的同一个 RAII 释放点；不模拟真实时长，
不证明不合作宿主会返回。没有公平性或最终完成断言，禁用 deadlock 检查；完成后的状态
可以无业务动作。不模拟任意多调用、子批次、OS 写入原子性或跨进程授权。

```sh
java -cp /absolute/path/tla2tools.jar tlc2.TLC -workers 1 \
  -metadir /tmp/dim-tool-segments-tlc \
  -config docs/design/examples/tool-model/ToolSegments.cfg \
  docs/design/examples/tool-model/ToolSegments.tla
```

各部件的承诺已拆分到[有限模型检查清单](capability-models/README.md)，其中逐项列出模型动作、
检查性质、实现入口及边界；[运行记录](capability-models/results.md)保留实际结果和模型摘要。
共享协议复用模型，不把共同记号变成新运行时抽象。当前未找到的路径、文档差异与宿主依赖
作为发现保留；分析和模型覆盖不等于这些问题已修复。

## 事件逐项检查索引

下面逐个对应本基线的 55 个具体 key。它是上面模型的检查入口，不另抄 payload 契约。
同一事件可能被新的插件使用；“检查关联”不等于封闭的写入/消费授权名单。
没有新增业务义务：每个检查点都针对现有生产/交接含义。Extension 载体本身不是固定 key。

| 事件 key | 本次检查关联 | 主要检查点 |
| --- | --- | --- |
| `input/message` | InputQueue / Prompt / 通知生产方 | 采用内容与候选内容分开；通知不得绕过输入采用 |
| `input/applied` | InputQueue / Loop | 普通输入交接后不再按原队列撤回 |
| `input/consumed` | Loop / HooksBranch / GoalBranch | 已消费才影响上下文及操作员输入失效判断 |
| `input/withdraw` | InputQueue | 首个有效撤回决定；命令本身不是删除事实 |
| `input/deleted` | InputQueue / Loop | 删除成员不再恢复；替代批次不能重复创建 |
| `stop/interrupt` | Loop / 各工作所有者 | turn 与 branch 范围不同；取消请求不等于副作用回滚 |
| `approval/request` | Approval | 登记待处理身份；恢复仅选择无关联响应/取消者 |
| `approval/respond` | Approval | 提交不等于接受；迟到答案可败给更早结算 |
| `approval/cancelled` | Approval | 结束问题，不总是取消执行；自动 Allow 等持久发布 |
| `permission/changed` | Permission | 登记版本和派发生效版本分开；恢复继承不冒用祖先版本 |
| `exec/started` | BackgroundExec / ShellSession | 运行身份与资源归属；异常恢复需要明确结算所有者 |
| `exec/ended` | BackgroundExec / ShellSession | 自然完成与 stop 竞争；与 wake 之间可有崩溃窗口 |
| `turn/start` | Loop / WaitBranch / Prompt | 单活动回合；Wait 只接受已打开回合中的通知 |
| `turn/end` | Loop / GoalBranch / BranchLine | 结算后通知；旧恢复终态不能再启动 Goal 或伪造新 idle notice |
| `prompt/changed` | Prompt | 已采用快照，不是刚加载完成的候选 |
| `prompt/configure-request` | Prompt | 提交命令不能因回复等待者 drop 而被撤销 |
| `prompt/configure-result` | Prompt | 结果关联原操作；晚完成不能越过更新的采用 |
| `context/configured` | Prompt / ToolBinding / ContextPreparation | 配置组成事实与最终决定同批；请求固定其采用版本 |
| `context/scope` | Prompt | 消费者有实现但未定位生产路径，不能宣称完整流程已覆盖 |
| `context/prepare` | Loop / ContextPreparation | request 身份与固定内容 cut；无结束者恢复失败而非重跑 |
| `context/compact` | Loop / ContextPreparation | 手动压缩与普通步骤互斥；中断仍须结算命令 |
| `context/compaction-start` | ContextPreparation | 压缩操作身份；与结果关联决定是否遗留 |
| `context/compaction` | ContextPreparation / ContextUsage / FileReads | 成功结果才改变有效历史与绑定边界；失败不是成功摘要 |
| `context/budget` | ContextPreparation / ContextUsage | 请求及 cutoff 的预算证据；非授权或压缩终态 |
| `model/call` | ContextPreparation / ModelProvider | 准备完成交接；恢复只取未终结调用身份 |
| `model/usage` | ModelProvider / ContextUsage | 只校准经验证的当前请求关联，不能复用旧正文 |
| `step/model-start` | Loop / 模型工作部件 | 当前 request 交接身份，不等于外部模型已经响应 |
| `step/thinking` | ModelProvider | 块首事件身份与增量归属；不可逆输出限制重试 |
| `step/text` | ModelProvider | 已发布文本不因重试或恢复重新执行产生 |
| `step/tool-use-start` | ModelProvider / Loop | 已开始但参数未完整的工具也要有对应错误结算 |
| `step/tool-use` | ModelProvider / Loop / ToolBinding | 模型提议不等于执行授权；按绑定快照处理 |
| `step/model-content` | ModelProvider | 内容与请求关联；不用它另造上下文输入通道 |
| `step/model-output-end` | 模型工作部件 / Loop | 准备失败、模型结束、恢复失败共享终态协议；按 request 区分 |
| `step/tool-call` | Loop / ToolExecutor | call_event_id 交接；child 的 parent_call_id 不能进入顶层执行 |
| `step/tool-start` | ToolExecutor / Loop | 执行流程开始，不推定外部副作用已经发生 |
| `step/tool-completed` | ToolExecutor / Loop | 调用完成与整个批次结果分开；按原身份结算 |
| `step/tool-result` | ToolExecutor / Loop / ContextUsage | 结果含已确认 effects；取消不抹掉已完成子调用效果 |
| `branch/create` | TreeTimeline / BranchLine / GoalBranch | reservation 所有者与 source 身份；提交后关闭可取消创建 |
| `branch/start` | TimelineTree / Prompt / ToolBinding | 新分支 opening 和 adopted 配置在消费边界协调 |
| `branch/resume` | TimelineTree / Prompt / ToolBinding | 新代重新绑定，不恢复旧进程或历史 handler |
| `branch/create-failed` | TreeTimeline / BranchLine | 创建失败结算 reservation，不能留下活动目录假象 |
| `branch/end` | TreeTimeline | 命令与结束结果分开；子树结束顺序由 owner 实现 |
| `branch/ended` | TreeTimeline / BranchLine / GoalBranch | 终结对应分支；通知不启动重载后暂停的 Goal |
| `collaboration/request` | CollaborationTimeline | 跨 Session request_event_id，不假设全局总序 |
| `collaboration/cancel` | CollaborationTimeline | 只操作当前所属成员工作；idle 拒绝不伪造工作结果 |
| `collaboration/close` | CollaborationTimeline | 关系关闭与 Session/任务结算边界不混淆 |
| `collaboration/message` | CollaborationTimeline | 对方采用输入与发送命令不是同一时刻 |
| `collaboration/delivered` | CollaborationTimeline | 已交接身份用于去重，恢复不重新发送已交付工作 |
| `collaboration/result` | CollaborationTimeline | 按原 request_event_id 结束整项协作，不按每个 turn/end 误结算 |
| `session/forked` | GoalBranch / BackgroundExec / Session 存储 | 复制历史不继承旧资源、执行与未发通知的本地所有权 |
| `goal/state` | GoalBranch | 完整业务状态；active 恢复为内存 paused，只有新 set/resume 启动 |
| `prompt-state/recovered` | Prompt | 恢复结果按 source 位置采用，不能越过较新结果 |
| `context-usage/recovered` | ContextUsage | 数字与 epoch/来源位置一致，不证明当前完整正文已重新计数 |
| `context-usage/covered` | ContextUsage | 只扩展已检查无贡献的尾部，不掩盖未处理相关来源 |
| `migration/imported` | 迁移导入边界 | 仅来源标记；未发现业务执行消费者，不应启动历史副作用 |

类型和关键字段以 [EventKind](../../../crates/dim-agent-types/src/events.rs) 及各部件已有契约为准。
导入标记的生产边界见 [migration/native.rs](../../../crates/dim-migration/src/native.rs)。
未知扩展事实继续作为不透明数据保留，不要求生产插件仍存在，也不触发自动安装或重执行。
