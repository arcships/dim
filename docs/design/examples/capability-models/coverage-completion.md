# 已知缺项的第二轮覆盖

本页逐条接续 [原覆盖审计](coverage-audit.md) 和 [第一轮补充](coverage-fill.md)，不重新定义 Capability 的职责。
承诺来源仍是原表链接的所属设计和实现；本轮对照工作树基线 `da7e3614b`，下列相关实现与 `8a5fe0980` 相同。
[运行记录](coverage-completion-results.md)及其 [JSON](coverage-completion-results.json)记录实际检查、文件摘要和可达场景。

**原表列出的具体缺项现在都有对应的有限模型检查；这不等于所有真实需求、Rust 路径或模型组合都已覆盖。**
两项 Shell crash 性质仍有反例，继续由 #1633 跟踪。“有覆盖”不等于“性质通过”，更不等于“问题修复”。
未知需求不能从已有命题的证明中推出；未定位生产者的 `context/scope` 仍不能称为端到端覆盖。

## 缺项与检查对应

编号沿用原审计的 22 个部件。每个分号分开的场景都指向具体模型或已有实验；它们不是新增运行规则。
性质的精确定义、是否附加公平性，分别以 `.tla` 和 `.cfg` 为准。

| 部件 | 原缺项 → 检查 |
| --- | --- |
| 1 Permission | facts await 后策略变更、取消、读取失败 → 已有 `PermissionFacts`；preset、allowed/disallowed、文件 gate、command rule 与 advisor 组合 → [PermissionComposition](PermissionComposition.tla) 的 `DisallowedAlwaysDenies`、`ReadOnlyWriteDenies`、`ProtectedWriteGate`、`CompoundAllowDoesNotBypassAsk`、`AdvisorPriority`。advisor 收集中的撤销/取消仍由 `AdmissionAdvice` 检查 |
| 2 Approval | 同 branch 两个 pending 和跨 branch 隔离 → [ApprovalRequests](ApprovalRequests.tla) 两个配置的 `ConsumeOnlyOwnRequest`、`CancelOnlyOwnRequest`；查询和 Unknown/Closed/Cancelled → `QueryIsBranchScoped`、`OneResponsePerConsumedCommand`；自动/响应 terminal 写失败不能准入 → `FailedTerminalNeverAdmits`。不增加“失败也必定补终态”的活性承诺 |
| 3 ToolExecutor | 提交前 claim、未派发释放 → [ToolDispatch](ToolDispatch.tla) 的 `ClaimBeforeCommit`、`ReleasedDropsClaims`；resolve_write 后批准/执行同一参数 → `SameApprovedCallExecutes`；progress、completed、batch 身份/顺序 → `ProgressHasStartedIdentity`、`BatchAfterEachCompletion`；多 parent effects 及不重复补入 → 第一轮 `NestedSettlement` |
| 4 InputQueue | 实际到达 FIFO、跨 turn handover、handover 不算普通 pending → [InputOrdering](InputOrdering.tla)；notHeld 不替代、发布未知、多个 owed withdrawal 按命令序、snapshot 等发布 → [InputReceipts](InputReceipts.tla)。输入边界检查仍由 SubmitChecks 提供，不把通过检查当成预留成功 |
| 5 Loop | 每轮输入组成、停止后旧队列不能单独驱动 → [LoopInputs](LoopInputs.tla) 的 `OldQueueCannotWake`、`ClaimIncludesOrderedPending`；预算恢复复用旧 source 不再 claim 新输入 → `BudgetSourceClaimsNoNewInputs`；失败/输出上限/工具/新输入选择 → [LoopDecisions](LoopDecisions.tla)；未派发完整工具按开始顺序结算 → [UndispatchedTools](UndispatchedTools.tla)；failure chain 总额 → 第一轮 `RetryChain` |
| 6 WaitBranch | 资格、FIFO、Steer、保留不匹配通知、槽拒绝、timeout/interrupted/reload/drop、唤醒交接 → 第一轮 `WaitRuntime`；工具与 native timeout 边界、Loop 后果 → `WaitHandoff`；已退休 face 不进入旧代服务 → [ToolRetirement](ToolRetirement.tla) 的 `RetiredFaceCannotEnter`，对应 `local_execution.rs` 的旧 face 流程 |
| 7 ContextPreparation | 超限压缩/再次失败、最终请求重估 → 第一轮 `FinalRequest`；压缩期间 Steer 延后交接 → `LoopInputs.CompactionDefersRequests`；guide/model-only 采用沿用已采用 system source → [PromptProjection](PromptProjection.tla) 的 `GuideUsesAdoptedSource`、`LegacyKeepsAdoptedSystem` |
| 8 ModelProvider | continuation 继承总额、本请求局部额度重置、耗尽不再入场 → `RetryChain`；半截参数与完整但未执行 → [ModelStream](ModelStream.tla) 的 `ClosureKeepsIdentityAndMeaning`；所有开始的工具先结算、request-specific usage/terminal → `FailureClosesAllStarted`、`TerminalUnique`、`UsageAfterEndForbidden` |
| 9 Prompt | 成功/失败/superseded、缺失 source 撤销 → `PromptOutcomes`；opening 固定 snapshot、首次请求等 receipt、动态变更顺序 → [OpeningConfiguration](OpeningConfiguration.tla)；完整 source/scope 身份、最新 entry、inactive scope → `PromptProjection.FullIdentityLatest`、`InactiveScopeAbsent`；退役同批仍由 `PromptRetirement` 检查 |
| 10 ToolBinding | activation 前变更、opening、失败保留旧采用、多个 request 各自释放 pins → `ToolAdoption`；历史配置的来源版本已经不可取得时新 bind 失败；仍有 prefix/request pin 的已替换版本可继续绑定，旧 pin 不改绑新 handler → `ToolRetirement.UnavailableDoesNotBind`、`OldRequestNeverRunsReplacement`、`StillHeldVersionCanBind`。旧 handler 可成功也可报错，具体退出机制不是统一新增的语义 |
| 11 ContextUsage | 来源筛选、child render facts 排除 → [UsageSources](UsageSources.tla) 的 `ChildRenderDoesNotCount`、`OtherSourcesExcluded`；最终 request/projection 初值及 compaction 竞争 → `FinalRequest`；投递后才取得 basis → `CalibrationWaitsForDeliveryAndBasis`；历史 cut 缺 predecessor → `MissingHistoricalPrefixUnavailable` |
| 12 BackgroundExec | stop/close/自然完成、清理失败、迟到 stop 保留已观察结果 → [ExecStopHistory](ExecStopHistory.tla)；已完成历史 wait/stop 不重启、不撤 wake → `HistoricalCallsDoNotRestart`；close 后 end 再 opening 不补资格 → `ReopeningDoesNotRequalifyOldEnd`；实时预览与历史文件分页可用性 → [OutputAvailability](OutputAvailability.tla) |
| 13 ShellSession | 前台转后台唯一身份、完成/stop/shell exit 终态竞争、资源释放 → [ShellCommands](ShellCommands.tla) 的 `OneTaskIdentity`、`TerminalRequiresRelease`、`TerminalNeverReplaced`；旧 generation 与排队取消 → `OldGenerationCannotChoose`、`SkippedQueueDoesNotStart`。两项 crash 恢复反例保留 |
| 14 FileReads | 文件/分支隔离、read/write/edit 建绑定、edit/write 对压缩不同要求、固定 published cut → 第一轮 `FileBindingQueries`；不把绑定解释成锁、权限或文件 CAS |
| 15 TreeTimeline | 固定 ancestry cut、不同 parent、waiter 消失不撤已接纳义务、opening receipt/input/fence 前不可见、取消构建释放后失败、tool end 等 turn/end → [TreeOpening](TreeOpening.tla)；父子关闭顺序及 close 发起后义务 → 原 `TreeLifecycle`，调用方停止等待不增加撤销树关闭的动作 |
| 16 BranchLine | main/tool/host、opening/已通知 head、idle 排除 own-end/成功 Steer/pending input、roster 与外部 ended 同批、根回到可切换目标且不收状态说明、一次开线总占一个名额、最后一个名额的开线边界 → [BranchEligibility](BranchEligibility.tla)，边界另由 `BranchEligibility-CapBoundary.cfg` 检查；显式读可回退 cursor → `ExplicitReadCanReset`；根不收说明、根可移回 → `RootGetsNoNote`、`ReturnToRootAdmitted`，其中根的那个状态是可达状态而非初值；新子线占名额、边界只接受一次开线 → `ANewChildSpendsASlot`、`OneSlotIsSpent`；角色离开某个子树、其下的工具祖先重新计数 → `DefaultLeavesBelow`，两个切换场景各由 `BranchEligibility-MoveToChild.cfg`（计数由一变零，`TheCountGoesFromOneToZero`、`MoveFreesTheSlot`）和 `BranchEligibility-AtCapBelow.cfg`（切换被接受、停在上限之上、无开线，`TheSwitchIsAdmitted`、`TheSwitchLeavesTheSessionOverTheCap`、`TheSwitchLeavesNoRoom`）检查。读父历史的下界是 ancestry cut，通知下界是 opening；两个值不合并。整树的上限重算另由 [BranchCapSwitch](BranchCapSwitch.tla) 检查：两条嵌套工具线 A、B，计数口径就是 §6.1 的集合差（未结束工具线减去受保护路径），所以一次切换对每条线一次重算；`CreateStaysWithinCap`、`SwitchClosesNothing` 在全部状态上成立，`BranchCapSwitch-BackToRoot` 用评审给的上限二、`main → A → B` 一条都没多开的场景，检查切换被接受、两条嵌套线同时开始计数、名额刚好用尽、之后不能再开线；`BranchCapSwitch-OverCap` 多开一条线，检查同一次切换把 Session 留在上限之上 |
| 17 CollaborationTimeline | 同 member 重复请求、跨 branch、message/cancel/close 不同作用 → [CollaborationIdentity](CollaborationIdentity.tla) 两个配置；只取消指定 branch 当前请求、旧结果不变 → `CancelNamesCurrentBranchRequest`、`SettledResultStable`；取消不把已完成结果改写成 cancelled → `CancellationCannotRewriteCompleted`；多个审批独立撤回 → `EachApprovalRetires`；交付失败不撤销已采用输入 → `DeliveryFailureDoesNotUndoAdoption`。成员 idle 是来自所属 Session 的输入，单个 turn/end 不直接替代它 |
| 18 GoalBranch | edit、pending/tasks、普通输入重置、旧 evaluator 身份、两种 judge、fork/reload → 第一轮 `GoalProgress`；现行实现对应及 Draft 边界不变 |
| 19 HooksBranch | event/outcome 点选择、follow-up 第二条链 reset → `HookFollowups`；catalog 顺序、异步不参与 gate、onFailure、首个 block 停止后续合并、SessionStart 提交前重查、approval_id 取消隔离 → [HookDecisions](HookDecisions.tla)；不同 hook 点的相同退出结果及输出字段优先级 → [HookReplyMeaning](HookReplyMeaning.tla) |
| 20 EnvironmentClientsBranch | 开场 receipt 首发、初读前监听、读取期间变化合并、reload 清 last → 第一轮 `SnapshotRefresh-Clients`；append 失败仍需下次 wake，不声称 timer 重试 |
| 21 SettingsSnapshotBranch | 开场 receipt 首发并交接 last/pending、read/append 连续失败、读取中关闭、新代初始化、条件最终发布 → 第一轮 `SnapshotRefresh` |
| 22 SessionBroadcast | 原事件/branch 身份、分支内序、drain 后 Closed → [BroadcastRecords](BroadcastRecords.tla)；`NoBranchEventAfterClosed` 只排除后续 branch Event，不排除 racing Notice/Fence；状态 dump 确认后二者可在 Closed 后出现 |

## 取值、代码入口和抽象

这些模型用于补原表的具体场景，不要求插件按模型的字段或状态数量实现。共享某一事件的生产者和消费者可以动态增加。
没有对“全部模型的并发组合”作检查；一张表中引用两个模型，也不表示已经证明二者组合的全部行为。

| 模型组 | 实际入口 | 有限取值及外部输入 |
| --- | --- | --- |
| ApprovalRequests | `approval.rs::respond/consume/admit/pending` | 两个 request，分别检查同 branch 和不同 branch；最多两条响应命令；commit/publish/consume 分开；close/取消/自动结算可交错 |
| InputOrdering / InputReceipts | `input_queue.rs` 的 `QueueState::input/end_turn/withdraw`、publication 和恢复 owed 结果 | 三条输入、两个 turn；receipt 模型两条输入、两条撤回；关联 ID 不复用；替代输入已在 Session 边界检查 |
| ToolDispatch / UndispatchedTools / ModelStream | `agent_loop.rs::start_request/end` 及 Dispatcher 消费回调、`tool_executor.rs::admit`、`model_provider.rs::complete`、`output.rs::closure` | 两个 request、三个 request/call 组合；未派发模型两调用；两个 usage 记录。ModelStream 的 End 是 `model-output-end`，不是上游 message-stop，未禁止 message-stop 后、终态前的 usage |
| LoopInputs / LoopDecisions | `agent_loop.rs::driven/start_request/next_model/consume` | 三输入、两请求、一次压缩；失败选择枚举会影响决定的布尔条件，不解释模型回答质量。预算 source 的实际内容及 chain 数值由对应模型另查 |
| PermissionComposition | `permission.rs::decide/admit_decision` | 三 preset、五风险、八种文件判定结果、四种命令规则结果、所有 advisor 意见集合；file access 从 preset 派生。路径/命令匹配算法的结果是边界输入，匹配结果如何影响准入保留在模型内 |
| PromptProjection / OpeningConfiguration | `prompt.rs` 消费 opening 并送入 adoption 队列、`prompt/recovery.rs`、`context_preparation.rs::rerendered` | 三个完整 source/scope key、三次 entry 更新；scope 身份不复用。来源 key 抽象完整 kind/id 身份，不能只按显示名称关联。两次配置采用；每次成功发布后才处理下一次；老格式无 system_source 时保留旧 System |
| ToolRetirement | `tool_binding.rs::bind/handler/release`、`tool_registry.rs::source`；`tests/integration/tests/local_execution.rs` 的 retired face 场景 | 两代 registry、两请求、初始配置有/无 prefix pin；Weak 保留的旧版本只有在最后一个强引用消失后才不可用，不把撤销注册等同于来源立即消失；旧 handler 的成功/失败由自身合同输入，不能假定卸载自动回滚副作用；face 新调用不可进入与已进入调用如何退出是两件事 |
| UsageSources | `context_usage/sources.rs::source_filter`、`context_usage/sources.rs::calibrate`、`context_usage/numeric.rs::prefix`、`AgentEvent::context_flags` | 两事件、三个 cut；configured/usage 是 filter 的显式例外，不虚构“所有 child 类型一律排除”的结论；historical predecessor 查询结果是输入，SQL 与祖先合并实现不在模型里 |
| ExecStopHistory / OutputAvailability / ShellCommands | `workspace_tools/background.rs::stop/ended/page/output`、`exec.rs::finish_background`、`shell_session.rs::process_queue/occupy_current/advance_occupying/handle_shell_exit` | 两任务/两命令、两代，旧代任务结算后才重新 opening。观察进程结果与资源清理/写终态分开，清理失败可把结果变为 failed。close 观察点抽象为通知判定点，不承诺物理时间同时发生时的唯一赢家；页字节解码不展开 |
| TreeOpening / BranchEligibility | `timeline_tree.rs` 的 launch/reservation、BranchLine idle/roster/read/parent_from | 两候选子分支、两父、head 至 3；opening 通知位置 1、ancestry cut 0，刻意保留差别；构建与发布分开；上限取 2，Session 其余计数线作为 `peers` 输入，新子线一律计入。没有任意深树的归纳证明 |
| CollaborationIdentity | `collaboration.rs` 的请求/交付/取消入口及 `settle`、`collaboration/approvals.rs::retire_member_approvals` | 两请求，同 member 和不同 member 两配置，各两个审批；目标 input 已采用与源 delivered 分开；idle/成员关闭是所属 Session 的输入，idle 的 completed/failed/cancelled 作为 Session 输入；取消只给取消结果命名，不把 completed 改成 cancelled；重载未结算请求为 failed；结果正文计算不展开；撤回成功、公平调度下检查审批最终退出 |
| HookDecisions / HookReplyMeaning | `hooks/runner.rs::run_ordered/write/prompt_decision/advice_of/permission_decision` | 两 handler、两审批，任意完成顺序；文本解析后的字段为输入，决定优先级在模型内。PermissionRequest 的 exit 2 是无意见，PreToolUse 是 Deny；不能共用一个笼统 Block 规则 |
| BroadcastRecords | `session_broadcast.rs` | 两 branch，每 branch 两事件，一个 Notice、一个 Fence；保留本 branch 序号，不增加跨 branch 全序或客户端确认语义 |

## 实际检查与消融

本轮新增 24 个配置，均执行完成；既有 46 个配置按文件和依赖摘要复用证据。通过/反例及状态数见运行记录。
额外读取五个模型的完整可达状态 dump，确认模型确实允许：Closed 后 Notice/Fence、旧输入加入新输入驱动的请求、
显式向前页回读、半截/完整工具的不同失败结算、来源替换后旧 pin 仍执行旧 handler。
这些是正向可达场景，不通过故意加一个错误断言制造“发现”。

删除了 Collaboration 中只记录 message/close 曾发生、却不参与承诺判断的历史集合；保留这些操作对当前请求的作用。
用两个配置分别检查同 member 复用与不同 member 隔离，避免第三个请求与无关观察集合造成状态空间乘积；两请求和每请求两审批仍保留。
Prompt 去掉了不参与查询判断的 restored 标记；Loop 去掉了不改变交接行为的 deferred 布尔副本，直接检查压缩期间请求不变、压缩后按当时 pending 取输入。模型仍保留会改变结果的请求身份、cut、发布边界与失败状态。
初稿语法错误、被中止的大配置不计入完成结果；最终文件与完成运行一一对应。没有新增平台、CI 门槛或必填模板。

运行时代码未变；此前已记录的 660 项流程验证作为实现侧证据复用。它们与模型检查各自说明什么保持分开，
没有把通过测试标记成 Rust refinement proof。下一次新增承诺或改变生命周期，应更新其对应命题及代码检查，不能仅沿用本页“已知缺项”的结论。
