# 承诺覆盖复查

复查对象：`c885c8eab` 的 34 个 TLC 配置及其承诺清单；运行时代码对应 `8a5fe0980`。
本次问题是“这些承诺涵盖了部件的哪些职责”，不是重新统计通过数或要求更多漏洞。
原有[运行结果](results.md)仍有效，但不能把局部性质通过等同于部件的完整业务承诺已覆盖。

本页保留 `5a78b2ed2` 时的审计基线；“缺口/当前模型”描述当时的模型。
后续已补场景及边界见 [第一轮补充](coverage-fill.md)和[第二轮逐项对应](coverage-completion.md)，不要把本页的旧反例当作新模型运行结果。

## 结论

原清单对恢复、关闭、身份、原子交接覆盖较集中；对正常使用中的选择、拒绝、返回内容、
连续多轮以及跨部件携带的状态覆盖较薄。有些性质存在于文字中，却没有进入模型状态或检查公式。
有些模型把“满足资格的输入”当作前提，却没有把资格的业务判断留在本部件的验证范围内。

这不要求证明插件内部全部代码。判断某个细节是否应保留，可以问：
**省掉它以后，能否仍判断调用返回什么、写哪个事实、后续消费者该做什么？**
如果不能，它就是当前命题所需的业务状态；如果能，例如 PTY 字节解码和 tokenizer 实现，仍可放在外部契约。

下面按实际入口和所属合同反查模型，而不是从已有模型反向定义“完整职责”。
列出的缺口是待覆盖的具体场景，不宣称已经枚举完无限运行环境，也不要求每格各建一个模型。

## 22 项对应检查

“已有”只指原配置实际检查的部分；“缺口”是没有表达或只作为假设处理的责任。
边界列防止为了补覆盖而扩大到别的 owner 的全部实现。

| 部件 / 依据 | 已有模型覆盖 | 应补的业务场景 | 范围边界 |
| --- | --- | --- | --- |
| [Permission](../../../architecture/rust-agent/plugins/Permission.md) | 策略登记/消费版本、advisor 合并 | 文件 facts 等待期间策略收紧后重新决定；自身策略、file gate、command rule 与 advisor 的实际判定组合；取消/读取失败返回而不准入。AdmissionAdvice 把 own decision 当输入，PermissionState 没有 facts 等待，两者通过不能覆盖这条路径 | 路径解析算法可外置，但 facts 结果与策略版本如何共同决定准入是 Permission 的职责 |
| [Approval](../../../architecture/rust-agent/plugins/Permission.md#approval) | 单个 pending 的多响应、自动结算、发布栅栏 | 多个问题并存时按 request/branch 隔离；pending 查询与 respond 的 Unknown/Closed/Cancelled 结果；terminal 写失败后不得准入且不可保证补终态。原模型的单 pending、成功写入没有表达这些场景 | RPC 编码及客户端显示另属协议 owner；服务业务结果不能仅以“响应已提交”代替 |
| [ToolExecutor](../../../architecture/rust-agent/plugins/ToolExecution.md#toolexecutor) | 执行段、排他、取消、单父调用的子效果 | claim 在提交前登记、未派发批次释放；resolve_write 后批准和执行同一个调用；两个父调用各收自己的子 effects；已有父返回效果不重复补入；progress/completed/batch-result 身份和顺序 | 不证明宿主副作用能回滚；也不新增排队后的二次授权。NestedSettlement 只有一个 parent，不能检查跨 parent 混入 |
| [InputQueue](../../../architecture/rust-agent/plugins/InputQueue.md#inputqueue) | held/released、替代与删除同批、单输入首个控制 | 多条 Queue 的实际到达顺序；不同 turn 的 handover；普通 pending 排除 handover；notHeld 不产生替代；已接纳但发布未知的服务结果；多个 owed withdrawal 按命令序结算；observable snapshot 等结果提交后更新 | SubmitChecks 是 Session 入口的依赖，不重做检查；“检查通过不预留条目”仍须与条件接纳一起核对 |
| [Loop](../../../architecture/rust-agent/plugins/Loop.md) | 单 turn/request、工具后下一步、reload 失败、局部 continuation | 已采用输入如何组成每轮请求；停止后旧未应用队列不被误启动；未执行工具身份逐项结算；预算恢复、输出上限、工具步骤、用户输入共同决定结束还是续行；failure chain 数值跨请求不重置 | 不检查模型回答质量；输入和失败链影响下一步选择，不能仅作为无结构布尔值丢掉 |
| [WaitBranch](../../../architecture/rust-agent/plugins/Wait.md) | 槽/tracker、关闭 drain、task 匹配、最后回合边界恢复 | 通知资格、顺序、Steer 优先；不匹配通知保留；AlreadyWaiting/Reloading/Interrupted/Timeout 的可观察区别；已有通知立即取走与注册唤醒的交接；工具与服务的 timeout 边界不同。详见下一节 | 不负责后台任务执行完成；超时也不停止任务。无需模拟真实毫秒，但要有“到期”输入 |
| [ContextPreparation](../../../architecture/rust-agent/plugins/ContextPreparation.md) | prepare 结果互斥、内容 cut、compaction 返回后交接 | 预算超限先压缩、仍不适配则终止且不无限再恢复；最终请求被 host 改变后重新验证，不能沿用旧估计；Steer 在自动压缩中到达时延后替换；配置/guide 更新对同一 source 的采用 | 压缩文本质量及 codec 算法可外置；是否允许 model/call、是否重用估计是本次请求的状态责任 |
| [ModelProvider](../../../architecture/rust-agent/plugins/ModelProvider.md#failure-chain) | 单请求输出终态、重试上限/等待预算 | continuation 继承 attempts/waited，预算耗尽时根本不调用；request-local backoff/malformed 额度重置与 chain 总额不重置；ToolStart 与完整 ToolUse 的不同错误结算；按 request 关联 usage/terminal | 流解析算法可外置，但“工具参数未完成”与“已完整但未执行”必须保留为不同交接状态 |
| [Prompt](../../../architecture/rust-agent/plugins/Prompt.md) | source 单调采用、配置同批、退役记录同批 | configure 成功/失败/被 supersede 的结果；撤销缺失 source；首次 opening receipt 与后续动态变更的顺序；按完整 source/scope 身份恢复有效 entry。现有 Retirement 模型每 scope 一个集合成员，未覆盖多 source 的替换关系 | 未定位生产者的 context/scope 不当作已完成端到端路径；无需重写 PromptHost 内部加载 |
| [ToolBinding](../../../architecture/rust-agent/plugins/ToolExecution.md#toolbinding) | 单请求快照、同名替换不换 handler、空绑定 | activation 前变更留待首次采用；首次请求等待 opening 采用完成；失败采用保留旧 prefix/pins，后来变化再试；多个旧请求各自释放 pins；注册来源卸载后失败的业务路径 | retired handler 的具体停止机制归 handler；是否把新请求绑定到正确已采用集合归 ToolBinding |
| [ContextUsage](../../../architecture/rust-agent/plugins/ContextUsage.md) | 数字前缀、covered、epoch/请求关联 | 仅 context.render 来源计数、排除 child facts；真实最终请求与 projection 相同才保存初值；projection 之后先发生压缩则延迟初始化；usage 已投递后再取 basis；历史 cut 缺 predecessor 返回 unavailable | tokenizer 可外置；“哪些事实贡献数字”是本部件职责，不能先给模型正确贡献数字就算已覆盖来源选择 |
| [BackgroundExec](../../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec) | hold、释放后终态、turn stop、崩溃补终态/wake | 显式 task stop 与 branch close、自然完成的不同终态/通知资格；stop 已完成任务保留其结果和 wake；历史 wait/stop 立即返回事实而非重启；branch close 后 end 与再 opening 的通知选择 | outputPage 字节解码不是 Timeline 核心，但输出可用/不可用、任务终态和 wake 资格是服务承诺 |
| ShellSession / [源码](../../../../crates/dim-agent-capabilities/src/shell_session.rs) | 借 ExecLifecycle 检查 start/end/wake 边界及 crash 缺口 | 前台命令转后台只有一个 task 身份；stop/自然完成/shell 退出竞争哪个终态；旧 generation 完成信号不结算新命令；命令排队取消是否发生 started、是否欠终态 | PTY marker 解析可留插件内部，不因此排除其输出所决定的 Timeline 身份与终态；crash 缺口仍单独跟踪 #1633 |
| [FileReads](../../../architecture/rust-agent/plugins/WorkspaceTools.md#filereads) | 单文件绑定被压缩切出、reload 清绑定 | 分文件/分支绑定隔离；read/write/edit 都可建立绑定；edit 与 write 对压缩的要求不同；同一 published cut 的查询判断。原模型只有 WriteCheck，不能解释 edit 合法而 write 被拒绝的场景 | 编码/hash/外部文件竞争归文件操作契约；不强行增加持久状态 |
| [TreeTimeline / TimelineTree](../../../architecture/rust-agent/plugins/TimelineTree.md) | reservation、父关闭、两层子树结束 | 固定 ancestry cut 不随等待者消失改变；candidate 在 opening receipt 和初始 input 接纳前不可见；tool end 等 turn/end；中断 create/close 的已接纳义务如何结算；不同 parent/source 身份不混淆 | 目录恢复及资源归 Session tree，不让 branch 重做；TreeLifecycle 的 Build(ok) 把 opening 的核心条件压成了外部布尔值 |
| [BranchLine](../../../architecture/rust-agent/plugins/BranchTool.md#branchline) | 两个 cursor、held block、成功 steer、恢复不发 idle | main/tool/host 角色；opening cut 与最后已通知 head；idle notice 排除 own-end 和 pending driving input；外部结束合并 roster 与 ended notice；显式重新读取可回退 cursor。现有 Read 只能推进到 held block，未覆盖真实读页 | 不复制父正文、不接管树。资格判断属于 BranchLine，不能全部排除在模型输入前提中 |
| [CollaborationTimeline](../../../../crates/dim-app-server/src/collaboration.rs) | input/delivered 分开、未交付恢复、成员缺失结算 | operation/request/member 是不同身份：同一 member 的多次请求、message/cancel/close 不等价；当前请求被取消而已结束请求保持；多个审批转发分别清理；交付失败的结果与对方已采用输入分开 | Team 目录/Agent 删除由对应 owner 管；[owner-independent 提案](../../collaboration-timeline.md)不当现行实现。现有 Ids 同时代表成员和请求，无法检查多请求共用成员 |
| [GoalBranch](../../plugins/Goal.md) / [状态合同](../../plugins/Goal-state-read.md) | 显式启动、active 重载内存暂停 | edit 保留状态；pending input/后台工作抑制续行；正常输入重置本轮计数；旧 evaluator 报告不能结束新执行；model/evaluator judge 分别处理 completed；子分支/fork 不继承目标。GoalLifecycle 的 Signal 不具备这些判定状态 | Goal.md 是 Draft，逐项与 branch/fold 实现及既有 flow 对照，不将提案自动认作实现保证；不证明 evaluator 的判断质量 |
| [HooksBranch](../../../architecture/rust-agent/plugins/Hooks.md#hooksbranch) | consumed/interrupt 使输出失效、普通点与审批并行 | 按 event kind/outcome 选择 hook point；handler catalog 顺序合并及 onFailure；每条 follow-up chain 的上限和新正常回合 reset；SessionStart 输出提交前重新检查 turn/start；不同 approval_id 的独立取消 | shell 命令实现/输出文本解析可外置；会产生何种输入、是否启动后续 turn 属于 Hooks 的业务语义 |
| [EnvironmentClientsBranch](../../../../crates/dim-agent-capabilities/src/environment_clients.rs) | 开场 receipt 首发、激活后读取后成功提交 last、失败待下次 wake | 初读前的 listener 建立；开场封口或登记被拒时退回激活后首轮；读取期间多次变化合并后重读；reload 清本代 last 并重新报告。CurrentInput 覆盖激活后的循环，开场那一发是模型窗口之前的前提 | 客户端快照编码/展示及信任判定无需加入 Timeline 状态机；保留无定时 retry 的实际边界 |
| [SettingsSnapshotBranch](../../../../crates/dim-agent-capabilities/src/execution_settings/prompt.rs) | 开场 receipt 首发并交接 last/pending；当前值发布、单次失败后 retry、条件最终一致 | host read 失败与 append 失败分别保留 pending；连续失败后恢复；读取进行中关闭；reload 新代初始化。CurrentInput.Fail 只从 read 后进入，且 failed 标志限制只失败一次 | 不证明底层设置存储，亦不要求真实秒级调度；需要检查失败后是否仍有下一次重试机会 |
| [SessionBroadcast](../../../../crates/dim-agent-capabilities/src/session_broadcast.rs) | 分支事件顺序及 branch drain 后关闭 | 原事件和 branch 身份不被替换；Event 与 Notice/Fence 的范围明确区分。代码允许 Closed 后出现 racing notices/fences，模型的 NoPostCloseNotice 只约束它命名为 notices 的 **branch event** 数组 | Notice/Fence 由各 owner/host 产生，可列为外部行为；绝不能把当前性质扩读成关闭后整个 Session channel 再无任何记录 |

## Wait：覆盖缺口的可执行检查

原 `WaitRuntime` 用集合 `arrivals`，`Take(i)` 可选其中任意匹配 task；实际 `Wait::take`
先找队列中第一个 Steer，再找第一个匹配 task。无 taskId 时两个不同 task 都匹配。

两段历史 `task A → task B` 与 `task B → task A` 在原模型中得到相同集合 `{A,B}`；
但调用 `wait(null, ...)` 在代码中应分别取 A 和 B。因此原抽象不足以检查这个选择承诺。
这不影响原模型对 tracker/drain 的检查，却限制了它能代表的 Wait 行为范围。
这里依据的是方法注释和实现的现行选择行为，不另立新的公开 FIFO 规范；集合抽象对原先的生命周期命题仍可适用。

[WaitSelectionCoverage](WaitSelectionCoverage.tla) **直接复用原 Init/Next**，仅增加记录到达顺序的观察变量，
不修改运行时代码、不删旧模型动作、不加入调度约束。它检查：没有 Steer、未指定 taskId 时，
返回第一个尚未消费的通知。因为检查的是有限前缀安全性，此探针不附加原模型关闭进度的公平性。

TLC 实际返回 13，得到以下前缀（完整轨迹见 [coverage-counterexample.md](coverage-counterexample.md)）：

| 状态 | 模型动作后的事实 |
| --- | --- |
| 1 | 初始状态 |
| 2 | turn 打开 |
| 3 | task 1 到达 |
| 4 | task 2 到达；观察顺序 `[1,2]`，原集合 `{1,2}` |
| 5 | 接纳不指定 taskId 的 wait |
| 6 | 原 Take 允许取 task 2，task 1 留下 |

这是**模型相对实际选择规则的覆盖反例**，不是 Wait 实现 bug，也不推翻原 34 个配置的结果。
它说明“taskId 匹配检查通过”与“Wait 的通知消费语义覆盖完整”是不同结论。
源码锚点是 `wait.rs::consume/take/arrival`；现有 local_execution 的匹配/不匹配通知测试只覆盖其一部分，
不能用通过这些测试来补出模型没有记录的顺序。

Wait 补覆盖时应先保留如下可观察区别，再决定是否需要增加模型状态：

| 使用场景 | 当前模型的缺少项 | 需要检查的业务结果 |
| --- | --- | --- |
| 输入打开 turn，随后 wait | 普通 input 与 tagged notice 的分类 | 开始回合的输入不能被误当完成通知 |
| A/B 已到，等待 A | 到达队列 | 返回 A，保留 B；下一次仍可取 B |
| task 通知与 Steer 已在队列 | Steer 类型及优先级 | 先返回 user_steer，task 通知仍待取 |
| 第二个调用到来 | 被拒绝调用的结果 | AlreadyWaiting，不能占 tracker/取通知 |
| timeout / caller cancel / native drop | EndCall 无原因 | timeout/interrupted 有区分；drop 无业务返回；都释放本次槽 |
| reload 与旧调用完成交错 | 原生/face/工具的结果映射 | Reloading、retired face unavailable、工具 reload 标记；不能误作普通 interrupted 续跑 |
| tool 与 native/face 服务调用 | timeout 输入边界分层 | 工具的 1..300000 上限不强加给原生服务 |
| notice 到达与开始等待交错 | 注册通知与复查队列 | 不能漏掉已到的匹配通知；也不凭空消费未到通知 |

## 最值得补的交接场景

1. **输入 → Loop → Wait → 工具结果 → Loop**：不同结束原因究竟恢复模型工作、结束等待还是失败整个回合。
2. **Loop → Preparation → ModelProvider → continuation**：同一个 chain 的 attempts/waited 跨请求保存，耗尽后零调用；局部 backoff/malformed 预算可以重置。现有一个模型从 attempt=1 开始，另一个只保存不透明 chain，分别通过不能排除组合时重置总预算。
3. **ToolExecutor → 子批次 → 父结果**：两个 parent 各有孩子，已有父 effects 与补查效果不会重复或串到另一父调用。无需先证明任意深树。
4. **Hooks → follow-up 输入 → 下一回合**：连续第三次后停止；插入普通新回合后可开始新链。当前模型 commands<3 且 round 不 reset，不具备检验第二条链的状态空间。
5. **Prompt/ToolBinding → opening → 首次请求**：初次采用失败/被替换/动态注册时，首次请求究竟看到哪个配置，以及旧请求什么时候释放自己的 pins。
6. **ContextPreparation/Usage → 最终请求**：估计属于哪个实际 request；host 改写或压缩越过 projection 后不能沿用不匹配数字。这是状态身份问题，不是要求形式化 tokenizer。

顺序以对用户流程的影响和已有缺口为依据。恢复模型保留，优先补这些正常业务闭环；不为所有外部实现再建一套证明。

## 本次验证及消融

仅新增一个复用现有 Wait 模型的观察探针，不改现有 34 个配置和运行时。原模型/config 哈希已核对，原实验记录保持原义。
覆盖检查依据为各部件现有页面、相关实现入口及模型状态/转换；它不是新增运行时缺陷清单。

探针使用同一 Java 17 / TLA+ tools 1.7.4，676 个生成状态、427 个 distinct、深度 6；发现反例后提前停止，
队列还剩 275，**不能标成搜索耗尽或验证通过**。重跑：

```sh
java -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC -workers 1 \
  -metadir /tmp/dim-wait-coverage-states \
  -config docs/design/examples/capability-models/WaitSelectionCoverage.cfg \
  docs/design/examples/capability-models/WaitSelectionCoverage.tla
```

没有增加“每个 cap 都必须填满所有格”的规范。逐项复查保留一张表；探针复用原模型而不复制代码；
业务覆盖反例与 Shell crash 模型反例分开，避免把两类问题累计成所谓运行时 bug 数量。
