# Timeline 承诺的有限模型检查

这是 [22 项逐项分析](../timeline-capability-analysis.md) 的实验附件，不增加插件规范，也不要求插件实现成这些小状态机。
题设是各部件**对 Timeline 的交接、状态和恢复承诺**；文件解析、网络、PTY、模型服务等内部实现仍由其所属模块负责。
代码对应基线为 `8a5fe0980`。实验的配置、模型摘要和结果见 [运行记录](results.md)。

[承诺覆盖复查](coverage-audit.md)保留首次复查发现的 22 个部件的业务缺项。
[业务覆盖补充实验](coverage-fill.md)列出本轮补入的场景、源码入口和仍未覆盖的部分。
[第二轮缺项对应表](coverage-completion.md)及[运行记录](coverage-completion-results.md)接续第一轮，逐条对应原审计的具体场景。
下表是最初的检查清单；WaitRuntime、NestedSettlement 已扩展，以补充实验和新运行记录为准。

## 如何读结论

每项先确定使用场景和承诺，再确定模型动作与代码入口，最后检查性质。下表里的“检查”指 `.cfg` 声明的
性质在指定有限状态空间中通过，或得到反例；不是 Rust 实现已经获得数学证明。

- **安全性**：所有可达状态或转换都满足条件，例如旧句柄不能在新代登记工作。
- **条件活性**：明确假设查询/写入成功、指定动作获得弱公平调度后，最终完成某事。没有这些前提就不作承诺。
- **边界**：模型未展开的外部算法或宿主行为，必须单独核对；不能计作 TLC 已验证。
- **反例**：模型允许违背承诺的执行。先核对抽象是否对应代码，再决定是否修实现；不能为了全绿删掉合法执行。

这里没有“题目已穷尽实际需求”的命题，也没有各模型同时组合后的全系统证明。新的插件可以按同一事件契约加入，
模型中的有限身份是实验取值，不是封闭的生产者/消费者名单。未知事件在不相关模型里没有状态效果，不表示应从存储删除。

## 逐项承诺与检查

编号对应逐项分析的章节。每行拆分现有承诺；性质的精确公式在所链接模型及同名 `.cfg` 中。
“恢复选择”统一使用 [RecoverySelection](RecoverySelection.tla)，其适用条件见下一节。

| 编号 / 部件 | 承诺与具体场景 | 检查及代码对应 |
| --- | --- | --- |
| 1a Permission | 两个更新带同一个 expected；登记版本先于消费版本，只有消费后答复 | [PermissionState](PermissionState.tla)：`Write/Publish/Consume` 对应 update/consume；`Versions/NoLostUpdate/DispatchedPolicy/ReplyAfterDispatch` |
| 1b Permission | 继承祖先策略不继承本分支版本；恢复取最后策略 | 同模型 `Ancestor/Restore`，`InheritedVersion`；祖先查询返回正确策略是查询契约前提 |
| 1c Permission | 自身 Deny 不咨询 advisor；已收集 Deny 优先，Ask 保留，取消停止收集 | [AdmissionAdvice](AdmissionAdvice.tla)：`Advice/Withdraw/Finish` 对应 admit_decision；检查 `OwnDenyWins/AcceptedDenyWins/AskPreserved/NoAdviceAfterCancel`。错误答复归 none，已完成答复不因后来撤销注册倒退 |
| 2a Approval | 多个响应可以提交，只能一个取走 pending；自动结算不得越过已提交响应；持久发布前不返回准入 | [ApprovalOrdering](../approval-model/ApprovalOrdering.tla)，六个不变量；对应 pending take、捕获位置复核与发布等待 |
| 2b Approval | 重载只选择未结束请求，补取消，不重新提问或执行工具 | 恢复选择，关联 approval_id；旧 native future 的退出仍依赖调用方取消，不由 pending 清空推出 |
| 3a ToolExecutor | 准入可并行；运行必须等前段全部结算；Exclusive 不和别的调用同时运行 | [ToolSegments](../tool-model/ToolSegments.tla)：`Settle/Advance` 对应 Settled::drop/执行段推进；计数、前缀、顺序、排他及观察取消后的启动检查 |
| 3b ToolExecutor | 取消保留已确认子结果 effects；父结果收集对应子结果，截止时间是明确例外 | [NestedSettlement](NestedSettlement.tla)：`ChildResult/ParentResult/Expire`；`ConfirmedEffectsRetained/ChildBeforeParentUnlessDeadline/CancellationIsNotRollback`。当前扩展为两个父调用、三个孩子，见补充实验；不验证任意嵌套树 |
| 3c ToolExecutor | 恢复仅结算无 result 的 call_event_id，不重跑参数 | 恢复选择；嵌套结算模型检查确认效果的边界，不模拟五秒墙钟或不合作宿主的强制退出 |
| 4a InputQueue | Queue 可 held，Steer/Append 可交接；普通输入交接后不能撤回，Append 不驱动回合；handover 不要求普通 applied | [InputQueue](InputQueue.tla)：`Arrive/Take/Withdraw/End` 对应 QueueState；检查交接、删除、消费集合及驱动位 |
| 4b InputQueue | 首个控制决定生效；替代在删除之前且同批；恢复排除已交接/删除成员，不重复替代 | [InputRecovery](InputRecovery.tla)：`Apply/Withdraw/Replace/Restore`；检查 `AtomicReplacement/DecisionLast/NoRepeatReplacement/RestoreOnlyMembers`。replacement 正文是边界数据，不在模型内渲染 |
| 5a Loop | 一个活动 turn；压缩排斥新 turn；当前 request 才能推进，旧结果不串入新步骤 | [LoopProtocol](LoopProtocol.tla)：`Start/Prepared/Output/ToolResult/Compact` 对应 try_start/consume；最多两个回合、四个 request，含工具后的下一步 |
| 5b Loop | 中断不继续发模型调用；reload-settled 结果使旧回合失败；结束不重复启动旧回合 | 同模型 `Interrupt/Reload/End`；`NoRestart/NoRepeatModel/ReloadFailsTurn`。实际 deferred 输入选择和 continuation 内容不由该控制模型证明 |
| 5c Loop | 自动 continuation 每 turn 最多一次；budget recovery 独立；compaction 延后不重置 failure chain；driving input/interrupt 阻止自动续跑 | [LoopContinuation](LoopContinuation.tla)：`Failure/CompactionResult` 对应 next_model 与 ModelOutputEnd；`OneAutomaticContinuation/DeferredKeepsChain/NoAutomaticOnDrivenTurn/NoAutomaticAfterInterrupt`。错误分类作为输入，等待预算另由 RetryBudget 检查 |
| 5d Loop | 恢复根据未完成身份结算，不重新构造旧模型请求 | 恢复选择 + Loop 身份状态。`step/disposition` 属于 [ModelRecovery 提案](../../plugins/ModelRecovery.md)，不能作为已实现动作 |
| 6a WaitBranch | 一个 waiting 槽；登记 tracker 后才释放接纳锁；先放槽后清 tracker 仍能正确 drain | [WaitRuntime](WaitRuntime.tla)：`Admit/EndCall/ReleaseTracking/Close/Restore` 对应 Wait 的锁、槽与 tracker；`SlotTracked/DrainBeforeRestore`。在调用 future 被 poll/drop、取消与 tracker 释放获公平调度的前提下，`CloseDrains` 检查关闭最终排空 |
| 6b WaitBranch | 关闭后拒绝接纳；旧句柄不跨代接纳；按 task 取通知；turn/end、重载清通知 | 同模型 `NoAdmissionAfterClose/OldHandleRejected/SelectionMatches/ClosedTurnEmpty/RestoredNoNotifications`；EndCall 抽象通知、Steer、超时、取消或 drop 的释放点，不规定同时就绪时唯一赢家 |
| 6c WaitBranch | 恢复 open 只看固定 cut 的最后 start/end，然后连续消费后缀 | [RecoveryBoundary](RecoveryBoundary.tla)：`LatestEqualsFold/LivePrefixMatches/NoSkippedSuffix`；`Capture/Restore/Consume` 对应 rebuild 与 Dispatcher 交接。归纳折叠只是数学参照，不是恢复算法 |
| 7a ContextPreparation | 同一准备结果只能交接 model/call 或失败终态；准备函数返回后不再启动其压缩 | [Preparation](Preparation.tla)：`Accept/BeginCompaction/Release/Finish` 对应 current、prepare 返回和 complete；`HandoffExclusive/CompactionBeforeRelease` |
| 7b ContextPreparation | 内容 cut 固定，控制读取可以推进；发布配置的决定事实最后 | [RequestCuts](RequestCuts.tla) 的 `NoFutureContent/CompleteContentCut`；[PromptAdoption](PromptAdoption.tla) 的 `DecisionLast` |
| 7c ContextPreparation | 未结算 prepare、compaction 恢复失败，不重跑摘要工作 | 恢复选择，分别按 request 身份、compaction start 关联；Preparation 的 cancelled 表示已观察到的失败结果，不是“任意时刻 token 变真就禁止已完成的成功交接” |
| 8a ModelProvider | 不可逆输出之后不能重试；终态前释放工作；失败结算已开始的工具身份 | [ModelOutput](ModelOutput.tla)：`Output/Retry/Release/Terminal` 对应 run/consume_output；`NoReplayOutput/OneTerminal/ReleaseBeforeEnd/ToolsSettled` |
| 8b ModelProvider | 最多八次尝试、累计等待不超 300 秒、malformed 最多重试一次、credential recovery 最多一次；永久错误/output_limit 不重试 | [RetryBudget](RetryBudget.tla)：Failure 对应 RecoveryState::decide；检查 attempt、wait、malformed、replay-safe 与取消边界。模型只取无 hint、150 秒和超预算 hint，backoff 取无抖动上界；不证明浮点抖动算法 |
| 8c ModelProvider | 未结束 call 恢复失败，旧 messages/tools 不重新调用 | 恢复选择 + ModelOutput 终态协议；工具参数流解析/UTF-8 与 provider 请求协议属于外部边界，不在这个状态模型里 |
| 9a Prompt | 加载完成不等于采用；采用按 source 位置单调；回复等待者 drop 不撤销命令 | [PromptAdoption](PromptAdoption.tla)：`Request/Load/Select/Commit/DropReply`；`MonotonicAdoption/LoadIsNotAdoption`；对应串行 adoption 和 ConfigurationPublisher |
| 9b Prompt | 配置组成事实和决定同批，逐项消费不能先看决定再缺组成；重载丢弃旧加载任务 | 同模型 `BatchComplete/DecisionLast/Restore`。scope 退出及 retire 输入的具体查询/正文组合仍由 prompt_lifecycle/prompt_boundaries 覆盖；模型不宣称覆盖尚未定位生产者的 context/scope 全流程 |
| 9c Prompt | System entry 可直接生效，block entry 必须已交付；退役输入与恢复结果同批且结果最后，重载不复活已退役 entry | [PromptRetirement](PromptRetirement.tla)：`Change/ScopeEnd/Checkpoint/Restore` 对应 Projection::apply/checkpoint；`NoUndeliveredEntry/RetiredNotRestored/ResultLast/SourceNeverRewinds`。两个不复用的 scope 身份，正文与 before 校验由边界提供 |
| 10a ToolBinding | 声明与 handler 同快照；同名替换不改变旧请求绑定；空声明无可执行绑定 | [ToolBinding](ToolBinding.tla)：`Prepare/Bind/Replace/Run` 对应 prepare/bind_current；`PinnedIdentity/NoReplacementExecution/EmptyMeansNoTool/NoUndeclaredBinding` |
| 10b ToolBinding | registry 变化经采用路径；采用失败不发布半配置；历史定义不变成新代码身份 | PromptAdoption 的原子采用 + ToolBinding 的当前 registry。watch 先于初读是入口前提，与 CurrentInput 的初始 wake 相同；不把历史定义放进模型的注册来源 |
| 11a ContextUsage | 数字结果对应自己的 source；covered 只能覆盖无贡献尾部；缺口恢复不可用 | [ContextUsage](ContextUsage.tla)：`Process/Cover/Recover/InitializeFromActualRequest` 对应 Machine/恢复/真实请求初始化；`SavedPrefix/CorrectLivePrefix/CoveredNoMissingContribution` |
| 11b ContextUsage | schema/算法/epoch 不匹配不能当零；校准只属于仍存活的当前请求 | [UsageIdentity](UsageIdentity.tla)：`Recover/Measure/DropRequest/NewEpoch`；`CompatibleResult/CalibrationHasLiveIdentity/NoCalibrationRestored`。tokenizer 的数值正确性不是此模型的结论 |
| 12a BackgroundExec | started 有 Session hold；先释放进程后 ended；turn stop 不关闭后台任务；wake 有明确 ended 身份 | [ExecLifecycle](ExecLifecycle.tla)：`Start/Release/End/Wake/StopTurn` 对应 start/supervisor/consume；检查 hold、因果、释放及 stop 边界 |
| 12b BackgroundExec | 宿主崩溃留下未结束任务或未发通知，新代可补齐；fork 不认领继承资源 | 同模型 `RecoverHistory=TRUE`，`Crash/Restore/Recover/Fork` 对应 rebuild/unposted_filter；条件活性 `RecoveredSettled/RecoveredWoken`。仅一次 crash；恢复写入成功和任务公平执行是前提 |
| 13a ShellSession | 正常关闭先释放资源再结算；新 PTY 实例不能当作原工作仍运行 | 同一 ExecLifecycle 的 live 路径；PTY marker/generation、命令队列串行是插件内部协议，保留 minimal_execution 流程证据，不由 exec 事实模型证明 |
| 13b ShellSession | 宿主 crash 后未结束任务与未发 wake 是否补齐 | `ExecLifecycle-Shell.cfg`、`ExecLifecycle-ShellWake.cfg` 把 RecoverHistory 设为 FALSE，分别得到反例。依据是该 profile 没有相应恢复入口，不是人为删除已有实现；见 [#1633](https://github.com/arcships/dim/issues/1633)；[修复配置及验证](shell-recovery-results.md)单独记录 |
| 14 FileReads | 重载不恢复 I/O 绑定；只有后续成功 compaction 且 retain_from 越过建立位置才使依据失效 | [FileReads](FileReads.tla)：`Read/Compact/WriteCheck/Reload` 对应 established/in_context；`NoRestoredBinding/NoCompactedWrite/CompactionNeedsSuccessfulCut`。绑定不是权限、锁或文件 CAS |
| 15a TreeTimeline | reservation 先登记；父关闭能取消构建；结果必须关联已接纳创建 | [TreeLifecycle](TreeLifecycle.tla)：`Reserve/Build/CloseRoot` 对应 TimelineTree 的 reservation/create/close；`ResultRequiresReservation/PublicationRequiresResult/NoPublicationAfterClose` |
| 15b TreeTimeline | 子节点先结束；恢复由 Session owner 处理，不在 branch 再执行一次创建 | 同模型 `End/RestoreOwner`；`NoOrphanOpenChild/ChildrenEndFirst`。两个节点的父子链不是任意深树的归纳证明，end 工具等待回合结束由宿主生命周期前提提供 |
| 16a BranchLine | parent block 使用目标自己的 cursor，只有前进内容；held 消费或撤回后可重算 | [BranchLine](BranchLine.tla)：`Publish/Read/Withdraw` 对应 parent block/read-cursor；`ForwardBlocks/HeldAhead/CursorIsolation`；两个不同目标 cursor 可独立推进 |
| 16b BranchLine | 成功 steer 才记已通知；恢复旧 turn/end 不发新 idle notice；roster 从当前目录读 | 同模型 `Steer/End/Restore/Refresh`；`RestoreNoIdleNotice`。模型输入已经是满足业务 eligibility 的 line/parent 事件；不重新解释其他插件的工具结果 |
| 17a CollaborationTimeline | 目标 input 与源 delivered 分开提交；恢复不重发旧工作；turn/end 不是整个 request 的结束 | [CollaborationHandoff](CollaborationHandoff.tla)：`PublishInput/Deliver/Restore/Recover/TurnEnd` 对应 deliver/restore；`InputsHaveRequest/NoRecoveryResend/TurnIsNotRequestEnd`。特意保留 input 已提交、delivered 尚未提交的窗口 |
| 17b CollaborationTimeline | 成员缺失时明确结算；失效的 held approval 经撤回接口清理 | 同模型 `MemberGone/Recover/WithdrawApproval`；条件活性 `MissingOwnerSettles/ApprovalEventuallyWithdrawn`。WithdrawApproval 抽象 InputQueue 的请求/完成，不授权直接修改其内部集合 |
| 18a GoalBranch | 只有显式 set/resume 启动；旧信号不能重启重载后的 active | [GoalLifecycle](GoalLifecycle.tla)：`SetOrResume/Signal/Reload` 对应 Goal 状态处理及 pause_reloaded；`ExplicitStartOnly/ReloadCannotRun/RunningMeansActive` |
| 18b GoalBranch | active 恢复为内存 paused；不补写 paused，不恢复本代执行集合 | 同模型 `NoRuntimeRecovery/NoSyntheticPauseWrite`；文本及完整业务记录当不透明值，由 load/query-cost 流程覆盖其存取 |
| 19a HooksBranch | 已消费的 operator input 或 interrupt 才使旧输出失效；startup 不重复；blocking follow-up 最多三次 | [HooksValidity](HooksValidity.tla)：`Event/Finish/Startup/Reload` 对应 consume/validation；`ValidOutput/StartupAtMostOnce/BoundedFollowup/NoReplayOnReload` |
| 19b HooksBranch | 普通点串行、点内 handlers 可并发；PermissionRequest 独立取消；结束审批后不写旧输出 | [HookScheduling](HookScheduling.tla)：`Start/Handler/Finish/ApprovalEnded`；检查合并完整性、独立性、取消及重载不重跑；advice 的 Deny 优先另见 AdmissionAdvice |
| 20 EnvironmentClientsBranch | 激活后读取当前值；成功写入才更新 last；相同值不连续发布；不从旧历史恢复连接 | [CurrentInput](CurrentInput.tla)，`CurrentInput-Clients.cfg`：`Activate/Read/CommitInput/Fail` 对应发布任务；三个不变量与观察 closing 后的停止检查。无定时 retry，所以**不检查最终成功性质** |
| 21 SettingsSnapshotBranch | 失败保留 pending，重试成功才更新 last；当前值稳定后最终发布或关闭 | 同模型 `Retry=TRUE`，在服务/写入恢复及公平调度前提下检查 `EventuallyCurrent`；一秒 timer 抽象成重试机会，不证明真实时钟准时性 |
| 22 SessionBroadcast | 只投影已交付事件，保留 branch 身份和分支内顺序；Session end 在分支 drain 后 | [SessionBroadcast](SessionBroadcast.tla)：`Deliver/Close/EndSession` 对应 subscribe_all/SessionBroadcastEnd；`PublishedSourcesOnly/PerBranchOrder/CloseAfterBranchDrain/NoPostCloseNotice`。不提供客户端确认、重连重播或跨分支全序 |

## 恢复选择的共同实验

`RecoverySelection` 有两种相互独立的计算：按固定 cut 查无关联终态的身份；按同一 cut 的合法事实计算参照集合。
查询分两项一页，related 搜索仍覆盖整个 cut，不缩成当前页；恢复过程中还允许追加后缀。
`QueryEqualsReference` 检查两者相同，`CursorAfterCut` 检查后续消费从 cut 接续。
参照折叠只用于数学比较，**不是建议恢复代码重放历史**。

适用题设是身份不复用、终态关联已有开始、一个固定可见域。源端“同一个对象的多条终态命令”允许存在，
不会被错误地约束成每种终态只能一条。这个实验不实现 SQL 查询引擎，也没有模拟祖先链合并算法。

| 使用者 | 开始与结束的关联 | 必须从模型外核对的边界 |
| --- | --- | --- |
| Approval | request → respond/cancelled，approval_id | 字段投影、cut 与可见祖先规则 |
| ToolExecutor | tool-call → tool-result，call_event_id | child/parent 的查询排序和 effects 归并 |
| Loop / ContextPreparation | 准备/压缩身份 → 对应终态 | 当前 turn 控制字段、独立请求身份与两个查询域 |
| ModelProvider | model/call → model-output-end，request 身份 | 工具参数完成标记、failure chain 投影 |
| BackgroundExec | started → ended，task_id；ended → wake，end-event ID | fork/branch 开闭边界、通知资格的实际 filter |
| CollaborationTimeline | operation → delivered；request → result | 跨 Session 身份、成员存在性与拒绝结算 |

InputQueue 的成员/替代用独立模型检查；Wait 的最后边界用 RecoveryBoundary；Goal 的恢复映射、
Prompt 的采用和 ContextUsage 的数字/epoch 分别使用自己的模型。没有把不同恢复语义都简化成“找未结束工作”。

## 模型与实现的距离

抽象动作是代码的一个可观察交接点，不等于一行 Rust，也不等于一个整个 async 函数。
例如 Collaboration 的写 input 和写 delivered 是两个动作；Wait 的释放 waiting 槽和释放 tracker 也是两个动作。
反之，ConfigurationPublisher 的同批提交是一个动作，逐项发布是另外的动作。

观察到取消之后的决定、宿主已释放资源之后的结算，是模型边界；这不能推出取消与 OS 副作用之间有原子屏障。
Preparation 的 Release 表示 prepare 返回、准备局部工作不再展开，允许随后才观察到的关闭与已取得成功结果竞争。
模型没有将任意迟到取消强行改成失败。

以下结论**没有**由这些 TLC 实验给出，不能在 PR 中合并成“所有承诺已证明”：

1. 模型题设已经覆盖所有实际使用需求，或所有模型组合后仍满足全部性质。
2. 任意数量事件、任意深子树/嵌套批次的普遍证明；这里都是 `.cfg` 与模块定义限定的有限实例。
3. SQL、分页/祖先实现与公式逐行等价，以及 Rust 中所有 await/drop/错误分支都精化模型动作；这仍需源码对应和流程测试。
4. 外部算法正确性，例如路径/symlink/command 匹配、tokenizer、PTY marker、工具权限宿主和网络协议。
5. 未找到生产路径的 context/scope 端到端运行流程，以及 ShellSession 宿主 crash 的真实复现。

前面的 22 项有分项分析和局部边界模型实验；原审计列出的场景已在[逐项对应及边界](coverage-completion.md)中承接，不能因此认定实际需求或代码路径已穷尽。
Shell 两个恢复反例保留为旧配置的历史证据；新增的[修复配置验证](shell-recovery-results.md)对应单独的运行时修复，不改写原记录。

## 重跑

使用 TLA+ tools release 1.7.4（其输出版本为 TLC 2.19）和 Java 17。以仓库根目录为工作目录，
Java 和 jar 是仓库之外的前置依赖；仓库本身不能离线提供它们。
从 [1.7.4 release artifact](https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar) 获取 jar，
设置 `TLA_JAR` 为它的绝对路径，并先核对所有运行记录中的 `jar_sha256`：

```sh
printf '%s  %s\n' '936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88' "$TLA_JAR" | shasum -a 256 -c -
```

校验成功后再运行。TLC 状态和日志放在临时目录，不使用产品 home。

```sh
for config in docs/design/examples/capability-models/*.cfg \
              docs/design/examples/approval-model/*.cfg \
              docs/design/examples/tool-model/*.cfg; do
  name=$(basename "$config" .cfg)
  module=${name%%-*}
  java -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC \
    -workers 1 -metadir "/tmp/dim-tlc-$name" \
    -config "$config" "$(dirname "$config")/$module.tla" \
    > "/tmp/dim-tlc-$name.log" 2>&1
  printf '%s exit=%s\n' "$name" "$?"
done
```

原 34 个配置中，两个 `ExecLifecycle-Shell*` 预期返回 13（恢复活性反例），其余预期返回 0。
补覆盖后的 `WaitSelectionCoverage` 预期返回 0；旧模型的 exit 13 反例保留为历史证据。
新增配置均预期返回 0，精确范围见[第一轮记录](coverage-fill-results.md)、[第二轮记录](coverage-completion-results.md)与[Shell 修复重跑记录](shell-recovery-results.md)。
任何解析错误、超时或新的反例都不是通过。禁用 deadlock 检查是为了允许已完成/等待环境的状态 stutter；
只有声明了时序性质和相应公平性的模型才检查进度，不把安全性通过误报为“永不挂起”。

消融复查：共享的查询选择和当前输入发布各保留一个模型，不为每个调用者复制；删除了恒为零的“恢复读取正文次数”
及按有限 ID 集合自动成立的“压缩次数上限”，因为它们不能提供实现证据。保留分开的协议模型，是为了不把
一个巨型模型的状态爆炸或一个小模型的通过误当成整个插件已验证。未增加运行时抽象、CI 门槛或证明平台。

## 动态消费交接补充实验

#1732 C 的[合同与实现对应](../consumer-handoff-verification.md)及
[运行记录](consumer-handoff-results.md)展开真实 consume 的注册、暂存、排空、失败和退出。
