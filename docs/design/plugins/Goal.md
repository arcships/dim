# Goal

Status: Draft.
Scope: 既有 Goal Session/branch 部件的目标、命令、续行、模型输入和终止。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。

[业务状态与重载](Goal-state-read.md)唯一规定完整状态数据、内存与持久化边界、加载映射、原子提交、成本和验收。[读取与输入删除](Goal-property-queries.md)规定旧历史不恢复及输入删除不回滚的边界。本文规定 Goal 的入口、执行规则和模型协议；[Pursuit](Pursuit.md)规定 evaluator 判定方式。当前代码和生成装配图不因设计更新而宣称已实现目标。

## 1. Structure

Goal 属于一条分支。Session 部件注册命令和工具，branch 部件拥有目标、当前执行和异步任务。依赖及作用域遵守 [ADR 002](../../adr/002-plugin-scope.md)、[插件合同](../plugin-contract.md)和 [rutis](../rutis-usage.md)。

| 部件 | 依赖和职责 |
| --- | --- |
| GoalSession | TreeCommands、ToolRegistry、SessionMethods、GuideEntries、SessionIdentity；注册 goal_update、session.goal 和模型 guide。入口按 Session 与 branch 身份路由本插件拥有的 branch 处理句柄，不持有分支业务状态。 |
| GoalBranch | Dispatcher、Timeline、TimelineIdentity、BranchLifecycle、SessionLifecycle、TreeCommands、SettingsService；拥有状态、决定、业务事件和自己的任务。 |

分支内写业务状态用声明的 timeline；普通 continuation 和评估分支操作沿用原输入/TreeCommands 服务。没有新公共分支服务、Store 依赖或恢复协调模块。部件在 apply 注册清理及服务，实际消费和任务遵守 activated/closing；关闭完成必要终态后才移除服务。

maxContinuations 仍是 Settings 的 goal 源中的可选业务字段，范围 1..=1000，缺失表示无限额。每次决定通过 Settings 读取；不增加默认限额、缓存或重载配置机制。

## 2. Data and state

Goal 有两个不同的对象：

- 持久业务状态：一个完整目标、判定方式、active/paused/completed/blocked/none 及原因或说明。每次真实业务变更写一个 goal/state。
- 本实例执行：次数、轮次、当前输入/任务、评估配对、待执行决定和写入协调；全部在内存。它们不因每个事件到达而保存。

业务事件结构和加载映射见 [状态设计 §2](Goal-state-read.md#2-保存对象)与[§4](Goal-state-read.md#4-get-与冷恢复)。active 是持久记录中的状态，running 是当前实例实际在执行；重载可使最后记录仍为 active、当前状态已经 paused。

| 命令或结果 | 本次有效业务变化 |
| --- | --- |
| set | 新目标与 judge，active；开始新执行，次数归零。 |
| edit | 替换目标，保留有效状态和 judge；不启动暂停中的目标。 |
| pause | paused，reason=user；停止后续自动推进。 |
| resume | active，开始新的执行；不复活旧轮次、计数、评估或决定。 |
| clear | objective=null，state=none；终止当前 Goal。 |
| goal_update completed / blocked | model judge 下完成/阻塞；evaluator judge 下 completed 只是本次执行的评估请求，状态保持 active。 |
| evaluator 的有效 done / blocked | 完成/阻塞当前目标。 |
| 中断、失败、取消、限额、无进展、评估失败、可控卸载 | 结束正在执行的 Goal，写 paused 和原因。 |
| continuation、普通输入、任务和轮次变化 | 本实例内存变化，不写 Goal 状态事件。 |

Goal 命令的模型状态输入与 trigger 仍紧邻，状态输入在前；结构化业务事件同批置后。状态只在提交成功且按序消费后可见；后一个命令等待先前本插件写入已消费，不能对旧目标组成下一条状态。设置和恢复的 Queue 输入仍驱动第一轮；evaluator 完成申请不产生假的目标状态变更。

## 3. Flows

### 3.1 Set and continue

~~~mermaid
sequenceDiagram
    actor U as 用户
    participant S as Goal Session 入口
    participant G as Goal branch
    participant T as Timeline
    participant L as InputQueue / Loop
    U->>S: session.goal.set
    S->>G: 校验并路由到本 Session 的 branch
    G->>T: append_batch：模型状态输入、trigger、goal/state
    T-->>G: 提交成功并按序派发
    G->>G: running，初始化本次执行
    T-->>L: 采用输入并执行
    L->>T: turn/end
    T-->>G: 当前轮次完成
    G->>G: 检查本实例状态、Settings、进展
    G->>T: 原 continuation 输入
    Note right of G: 次数变化只在内存，不写恢复事件
~~~

### 3.2 Completion through goal_update

工具先校验外部参数，再由 Goal branch 组成目标变化。model judge 下，模型状态输入、completed trigger 和完整 completed 业务事件原子提交。evaluator judge 下只登记本次执行的完成申请；创建评估、等待报告不写恢复快照。只有有效评估结果才决定 completed/blocked，见 [Pursuit](Pursuit.md)。

### 3.3 Interrupt pauses the goal

turn-scope stop/interrupt、执行失败或取消使正在运行的 Goal 停止新的自动决定。Goal 的 owned task 取消并结算自己发起的工作，写 paused 与必要模型状态输入。已经交接的输入和已经开始的模型/工具工作按各自所有者规则结算；Goal 不直接撤回它们。

### 3.4 A new branch clears the inherited goal

子分支和 fork 从本地 branch/start 或 session/forked 开始，不能继承父分支/复制前缀的 Goal 执行或目标。继承 key 仍存在时，沿用原 start clear 退休这些模型可见 key。清理在 publication 后作为 Goal 自己的任务完成；它不是恢复父分支执行。

## 4. Timing and conflicts

### 4.1 Interrupt committed before the continuation (at most one extra turn)

决定只使用已消费事实，写入前重查同一轮次和执行。中断与已经提交的 continuation 交错时，不抹去已提交事实；后续暂停阻止新续行。已经交接的工作允许按原交接规则完成结算，不为了保证零额外调用而改变 InputQueue/Loop。

### 4.2 Reload

加载行为唯一见 [Goal 状态设计](Goal-state-read.md#4-get-与冷恢复)：最后 active 映射为内存 paused，不写回、不读取旧轮次和次数、不补答旧决定、不恢复旧评估。set/resume 才开始新执行。

Session/branch 部件的可控关闭会结束其正在运行的 Goal 并写 paused；进程直接崩溃或存储不可写时无法保证终态提交，由加载映射覆盖这一情况。客户端 detach、重连和查询不关闭原实例。

### 4.3 Other key rows

| 场景 | 结果 |
| --- | --- |
| 普通操作员输入 | 仅在本实例内重置 count/idle，使旧延后评估失效；不写归零事件。 |
| 待消费 Queue/Steer 输入 | 暂不自动续行；不将其列表写进 Goal 状态。 |
| 有未结束后台任务 | 在本实例等待，取消结束可唤起一次仍有效的决定；重载后不重建这项等待。 |
| Goal 命令输入被删除 | 仅移除本实例待输入 ID；已提交目标不变，零查询、零补偿写入。 |
| 重载后到达旧评估报告 | 不在当前执行的配对表中，不能完成或续行目标。 |
| paused 后 edit | 仍 paused；必须显式 set/resume 才运行。 |
| clear 后加载 | 没有目标，不读取更早目标复活它。 |
| 未知或损坏新状态 | 返回错误，不冒用更旧状态。 |

## 5. Context mapping

goal/state 不进入模型上下文。Goal 仍通过采用输入提供模型文本，不能通过请求准备注入内存状态。

| 对象 | 模型含义 |
| --- | --- |
| goal:state | 完整目标及这次业务变更时的状态。历史 active 只证明当时开始过执行。 |
| 新 set/resume trigger | 当前输入中的明确开始/恢复请求。 |
| 新 continuation | 当前执行驱动下一轮的请求。 |
| completed/blocked/paused 记录 | 当时已经确认的结束/暂停状态。 |
| 子分支的 start clear | 继承目标不属于本线，不在本线继续或完成它。 |

guide 和 active 文本不能把历史 active 解释为当前实例的运行授权。当前状态来自 Goal owner；历史页和模型历史保留当时事实。重载只在内存暂停，不补造一条模型上下文输入。

## 6. Interfaces (summary)

公开入口仍为 session.goal.set/edit/pause/resume/clear/get 和工具 goal_update；参数、GoalRead 和命令 sequence 返回保持原接口。[Appendix A](#appendix-a-interface-details)列出合同。无 Goal 数据库或专用查询机制。

## 7. Security

所有入口按 Session 身份及分支校验路由，不能通过参数切换到其他 Session。Goal 不授予执行权限，实际模型/工具仍遵守 Session 的 Permission/Approval。普通用户不能提交任意 raw Goal 块；生产者转义目标和说明。诊断不记录目标正文或凭据。

## 8. Rule check

| 规则 | 本设计 |
| --- | --- |
| principles §1 | Goal branch 是 Capability；Session 部件是入口，不把所有部件统称 Capability。 |
| §4.1、§4.2 | 不写别的所有者的结果，不回收已交接输入；删除按 InputQueue 的事实。 |
| §4.3、§4.4 | 无 before I/O；核心不解释 Goal。 |
| §4.5 | 业务状态和必要输入同批提交，Dispatcher 按序消费，任务写前重查。 |
| §4.6 | 模型内容只经采用输入，不注入内存状态。 |
| §4.7、§4.8 | 已声明的现有服务，无核心机制新增。 |
| §4.9 | 业务状态及实际驱动输入持久化，执行进度不落盘。 |
| ADR 002 D1 | Session 注册工具/方法；目标、执行和任务归 branch。 |
| ADR 002 D2 | 不增加接线模块或全局隐式资源。 |
| ADR 002 D3 | 扩展依赖核心，Session 不持有分支业务状态。 |
| ADR 002 D4 | goal Settings 是业务配置，不触发部件重载。 |
| ADR 002 D5 | 事件、状态、入口和错误为语言中立合同；Rust 名字在绑定附录。 |

## 9. Requirements raised to mechanism owners

无。Goal branch 使用已有 append_batch、Dispatcher、生命周期及属性查询。完整业务事件是扩展数据，不新增核心事件类型、查询字段或 native 格式版本。

## 10. Implementation

修改对象和验收见[状态设计 §6–§7](Goal-state-read.md#6-成本和修改落点)。保留既有 GoalSession/GoalBranch 装配、Settings、参数校验、文本编码及产品流程；移除运行对象序列化、逐事件 checkpoint、恢复节点、源后缀重放和每输入删除边界。当前实现状态由 issue 和实际代码记录，本文不把旧测试通过等同于新状态机完成。

## Appendix A. Interface details

### A.1 Tool goal_update

Session 的 ToolRegistry 注册 goal_update，risk=session；参数为 status=completed|blocked、summary，summary 长度 1..=2000。调用方分支由 ToolCall 身份确定。返回 ToolResult.content 为 goal_update recorded: completed. 或 goal_update recorded: blocked.；确认提交，不把评估请求误报为目标已完成。取消发生在提交前不写入，提交后事实保留。

### A.2 Session methods

namespace 为 session.goal，mutating 方法 interruption=report、progress=false；get 的 mutating=false。branchId 默认 main。

| 方法 | 参数 | 结果 / 行为 |
| --- | --- | --- |
| set | sessionId、branchId?、objective、judge? | {sequence}；judge=model（默认）或 evaluator，目标 1..=4000 字符。 |
| edit | sessionId、branchId?、objective | {sequence}；保留当前有效状态及 judge，不启动暂停目标。 |
| pause | sessionId、branchId? | {sequence}；用户暂停，Append。 |
| resume | sessionId、branchId? | {sequence}；Queue，新的执行、次数归零。 |
| clear | sessionId、branchId? | {sequence}；清除并终止当前执行。 |
| get | sessionId、branchId? | 原 GoalRead，见 A.3。 |

Session 入口校验参数和 Session 身份，Goal branch 决定状态并提交原子批次。sequence 仍是命令 trigger 的位置；结构化业务事件不改变它。没有目标时保持原命令的空目标结果，不凭空创建目标；没有生效变化不写伪造的业务状态。

### A.3 Reading the goal

返回 {branchId, through, goal}；goal 为 null 或 {objective, status, evaluator}。status 为 {kind:active|completed|blocked|none} 或 {kind:paused,reason}，保持 GoalRead 的既有外部类型。

活分支等待本次入口固定位置已消费后返回内存；不因 source_through 或新的持久事件等待。重载后的 active 记录返回有效 paused，reason=interrupted_reload；不能把持久历史页当作当前状态。无活 owner 时读取最后本地业务状态并应用加载映射。普通路径读取一个 Goal 状态对象；没有 goal/state 就没有当前 Goal，删除输入不改变已提交状态。

缺失分支、存储失败和未知 schema 仍是错误；取消读取返回 operation_interrupted，不取消已经接受的命令。详细范围见[状态读取](Goal-state-read.md#4-get-与冷恢复)。

### A.4 Continuation decision

只在本实例 running 时对新事实做决定；计数、轮次、去重和等待仅存在内存。

| 条件 | 不满足时 |
| --- | --- |
| C1：当前有效目标存在且在运行 | 不自动工作。 |
| C2：本次执行未被中断 | 终止并写 paused(interrupted)。 |
| C3：本轮正常完成 | 失败/取消时终止并写 paused(turn_failed / turn_cancelled)。 |
| C4：这是本实例当前待决定轮次，尚无决定且无新轮次打开 | 不重复决定。 |
| C5：无待消费驱动输入，且无未结束后台任务 | 在本实例等待，不写恢复状态。 |
| C6：未达到可选 maxContinuations | 暂停，reason=limit。 |
| C7：本轮非自动续行驱动，或调用过工具，或连续无进展轮数仍小于 3 | 暂停，reason=no_progress。 |

满足条件时只写原 continuation 输入，更新内存 count/idle。count 在 set、resume 和操作员输入时在内存归零；操作员输入是有 user 内容且首 raw 块不带来源标记的输入。其它插件自动输入不重置。

C7 沿用一次本轮 step/tool-call 存在性查询；仅自动续行驱动且没有未结束任务时需要。没有工具调用则 idle+1，有工具调用或非自动续行驱动则为零。Pursuit 的 continue 已由评估判断，idle=0，不额外执行 C7。

handler 只登记自己的任务后返回；网络、Settings 和查询等待在 owned task。写前重查当前执行/轮次。取消后台任务后本实例仍 running 时可以处理延后决定；部件重载不再恢复它。

### A.5 Model-visible texts

沿用 dim-goal 标记、goal:state key、命令和 continuation 的原编码。状态输入 Append，set/resume/continuation 的驱动输入 Queue，其余命令 Append。命令状态输入仍在 trigger 前且原始序号紧邻，goal/state 置于两者之后；完成申请不改变状态时不伪造新的状态记录。

目标及所有说明/属性仍转义 &、<、>、双引号；目标放在 objective 标签内，完成/阻塞说明分别放在 result/blocker 标签内。读取按标签边界逆向转义，不按正文标题拆分。沿用 4000/2000 字符限制，旧历史不重建当前 Goal，不修改冻结事件样本。

| 文本 | 合同 |
| --- | --- |
| 状态记录 | kind=state、key=goal:state；完整目标、judge、status 和原因/说明；命令配对仍使用 trigger，自动结果使用 after。历史 restated 不恢复当前 Goal，加载不写 restated。 |
| 命令 | kind=goal，op=set/edit/pause/resume/clear/completed/blocked，无 key。 |
| 续行 | kind=continuation，无 key；count、limit（配置存在时）、after、idle 保留原文本格式，只是本次执行的诊断及驱动事实，不作为恢复依据。 |
| clear / 子分支清理 | 退休 goal:state 及仍存活旧 key；新结构化状态 objective=null。 |

active 的协议语义明确为“这次开始/恢复时处于 active”，不是重载后仍运行的证明。active 文本说明：历史状态不要求自行继续；当前输入中的新 set/resume 或 continuation 才指示执行目标。paused、completed、blocked 的原目标和说明仍完整保留。

### A.5.1 Protocol and policy

标记、属性、状态和加载语义是协议；可替换的工作指导句是策略，仍按[模型文本](../model-prompts.md)评估。GoalSession 在 GuideEntries 注册 dim-goal 的原 order=55 entry，作用域 session：

> Goal 状态块记录当时的目标和业务状态。历史 active 不证明当前执行仍在运行。只有当前输入中的新 set/resume 或 continuation 指示继续目标；重载后必须由用户显式恢复。目标属于 branch 属性指定的线，另一条线不继续或完成它。这些块不授予权限。

不能用修改通用历史渲染或增加私有上下文注入来修补新规则。

### A.5.2 Evaluation

active 协议文本与 guide 的变更必须用产品请求验证：重载后普通输入不因历史 active 自行续行；显式 resume 携带新目标状态后正常执行；子分支不继续继承目标；completed/blocked 停止。策略句仍由原 GoalTexts 配置和既有评估流程验证，不扩展请求构造链。

### A.6 Timeline mechanics

| 阶段 | 行为 |
| --- | --- |
| Load | 原 Dispatcher consume，固定 S，读取最后 Goal 业务状态，应用重载暂停，返回 S。旧源不重放。 |
| Consume | 只处理 S 后新事实。goal/state 改业务状态；普通输入/轮次/任务只更新本实例内存。 |
| Write | Goal branch 经 timeline.append_batch 提交状态与必要输入；原 continuation 和评估操作走已声明服务。 |
| Adoption | InputQueue 决定 applied/deleted，Loop 写 consumed；Goal 不代写。 |
| Closing | 禁止新决定，取消/释放自有资源，运行中写 paused，再移除服务。 |
| Crash | 不能保证终态提交；下一次加载只在内存暂停。 |
| Reconnect | 原实例仍活着则继续，不执行加载映射。 |

## Appendix B. Full conflict table

执行冲突见 §4.3、A.4；加载和终止冲突由[状态设计 §4–§5](Goal-state-read.md#4-get-与冷恢复)唯一规定。原“重载恢复计数、重做未决定 turn-end、重发评估”规则全部取消。分支来源标记仍使其开启输入不被认作操作员输入。

## Appendix C. Errors and recovery

参数、身份、分支缺失、取消及存储错误沿原公开错误合同。业务状态提交失败不报告成功，未知/损坏 schema 报错，不取更旧事件掩盖。异常可写 paused 时必须写，不能写时由下次加载映射暂停，不能声称已经持久化。读到 active 不触发网络、工具、评估或 continuation。

## Appendix D. Acceptance

[状态设计 §7](Goal-state-read.md#7-requirements-raised-to-mechanism-owners验收与-rule-check)定义新状态机验收。既有命令、参数、Session 隔离、原子配对、模型转义、Settings 和正常续行产品测试保留；旧的重载继续执行断言替换为 paused、零快照写入、显式 resume 后重新执行。

## Appendix E. Assumptions

| 约定 | 决定 |
| --- | --- |
| A1 / A2 | Queue/Append 和模型标记沿 A.5，结构化业务状态不成为另一条上下文输入路径。 |
| A3 / A11 | 目标只属于本地分支，子分支和 fork 不继承执行。 |
| A4 / A5 | 可选限额、三次无进展、操作员输入在内存归零；重载不恢复计数。 |
| A6 / A7 | 执行异常终止写 paused；用户 pause 阻止后续自动推进，不直接撤回已经交接的工作。 |
| A8 | 当前状态通过 session.goal.get 读内存；历史块不代替重载后的有效 paused。 |
| A9 / A10 | goal_update 的 session 风险与 4000/2000 字符限制不变。 |
| A12 / A13 | 显式 set/resume 开始新执行，branchId 默认 main。 |
| A14 | 命令批次保留配对顺序；删除命令输入不撤销已经生效的目标。 |
| A15 / A16 | completed 在 model judge 下完成，在 evaluator judge 下是申请；当前状态还受重载暂停规则约束。 |
| A17 | 本实例等待当前后台任务，不在新实例恢复等待。 |

## Appendix F. Open questions

本方案没有留给实现选择的恢复模式：旧执行终止，重载暂停，显式 set/resume 开始新执行。

## Appendix G. Rust binding

沿用 GoalSession/GoalBranch、原 Changes 身份路由、TypedPlugin、ctx.effect、TaskTracker、CancellationToken、SettingsService、Dispatcher::consume 和现有 TimelineQuery。一个 Goal 业务状态定义用于生产与读取；text.rs 负责唯一模型文本转换。append_batch 构造的内部输入在 Goal 边界校验并赋予 submission ID，不新增通用写入包装或数据库能力。具体文件拆分不构成新增需求。
