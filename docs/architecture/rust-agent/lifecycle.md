# 生命周期

状态：已实现（事件驱动中断与单树装配）。执行状态归各能力自己持有，不设统一后台任务层。对象与能力分工见[架构](../README.md)，框架接口依据见[单树装配](single-tree.md#rutis)。

## 1. 作用域与拥有关系

```mermaid
flowchart TB
    App[应用 / 一个 Tokio runtime / 一个 rutis root] --> Connections[Connection 读写与等待]
    App --> Agents[AgentHost / Agent 的 Session 注册表]
    Agents --> Session[SessionScope 子树]
    Session --> Shared[Session 共享能力与端口]
    Session --> Tree[TimelineTree]
    Session --> Main[main / BranchScope 子树]
    Session --> Branch[branch / BranchScope 子树]
    Tree -. 关闭权 .-> Main
    Tree -. 关闭权 .-> Branch
    Main --> Caps[长期能力与订阅]
    Branch --> OtherCaps[该分支能力与订阅]
    Caps --> Ops[该能力启动的异步工作与取消句柄，含脱离工作]
```

作用域是 rutis 同一棵树上的 Plugin 子树，不是新 Tokio runtime 或线程池。Session 注册表、TimelineTree 持有子树的关闭权；它们已有的业务注册表同时记录实例存活，不再维护第二棵容器树。装配细节见[单树装配](single-tree.md)。

| 对象 | 建立与释放 |
| --- | --- |
| Session 子树 | 创建 Session 时载入 SessionScope；所属分支全部关闭后永久卸载 |
| 分支子树 | 创建分支时载入 BranchScope；执行结束并释放能力后永久卸载 |
| 能力插件 | 每个作用域载入时建立一次；随子树卸载；不随 turn 重建 |
| 前台操作集合 | 各能力自己拥有；stop/interrupt 取消分发时在途的操作，分支没有暂停状态；工作结束后移除任务 |
| 脱离工作 | 发起它的能力自己拥有；不随普通 turn 结束，branch 范围中断或所有者显式停止时结束，见[脱离工作](event-driven-flows.md#3-后台任务停止与自动唤醒) |
| 连接任务 | App Server 管理；不拥有 Session |
| Session 服务 | 分支能力按 Session 实例键注入；子树卸载时由提供者按依赖顺序释放 |

所有受管任务都必须有可等待的结束结果。`CancellationToken` 只发出停止请求，`Drop` 不表示异步清理完成。

## 2. rutis 能力

生命周期按 crates.io rutis 0.10.0 设计；接口见[单树装配](single-tree.md#rutis)。

| 项目 | 使用方式 |
| --- | --- |
| 初始化 | injects 门控、validate、apply；各作用域插件 `FiberView::watch` 全部 Active 后发布实例；Pending 不视为成功 |
| 资源登记 | `ctx.effect`、同步/异步 disposer、逆序清理；每取得资源立即登记 |
| 幂等清理 | dispose/shutdown 并发调用共享结果；等待方放弃后清理继续 |
| 子树永久卸载 | `FiberView::shutdown()`：卸载准入同步关闭，子树内消费者先于提供者释放，回收父级、依赖索引与总线记录；root 仍用 `Ctx::shutdown()` |
| 初始化回滚错误 | 失败插件的错误为 apply 原因在前、回滚错误在后；runtime 据此区分构建原因与清理失败，见[单树关闭](single-tree.md#关闭) |
| 实例隔离 | 实例键与实例事件按作用域子树校验 |
| 严格读取 | 插件必需外部服务用 `require` / `require_as`；普通及命名作用域服务都检查声明，release 同样执行，失败保留 `ServiceReadError` |
| 同步调用 | 卸载、重启、关闭和装载回滚等待已接纳同步调用退出；不包含调用返回后的业务提交，不能代替 Timeline 与 Loop 状态的结算屏障 |
| 诊断 | `Ctx::diagnostics()` 只读快照；debug 构建检查直接 `get_as` 绕过严格读取的未声明访问、读取代次与无实例键提供 |

插件调用的 panic 由 rutis 在注册元数据、apply 构造与 poll 边界捕获并转为失败；平台捕获自身工厂构造。业务 Dispatcher 和受管操作回调的构造/poll 由对应组件的故障边界处理。

同步 before 的执行、失败与重入约定见[单树事件合同](single-tree.md#同步-before)。`TimelineContext::close` 先完成作用域清理，再关闭 Timeline，保留清理任务提交终态的窗口。

## 3. 初始化与失败回滚

| 阶段 | 规则 |
| --- | --- |
| 受理 | Agent 或 TimelineTree 先登记构建操作；RPC 断开只停止等待，不能丢弃构建拥有权 |
| 绑定 | 在父节点下载入 Session/分支作用域 Plugin；Session 服务按实例键注入 |
| 装配 | 实例工厂建立插件；rutis 按依赖状态调用 apply |
| 取得资源 | 在下一次可取消 await 前登记清理；部分初始化失败同样释放 |
| 等待 | 观察插件状态、取消和统一初始化期限；不以 settle 代替 Active |
| 发布 | 全部 Active、订阅齐全、Loop 状态端口可解析后由 `TimelineContext::start` 启动 Dispatcher；子分支还须完成 branch/start 和首输入接纳，才出现在公共分支列表与操作入口。发布时 TimelineTree 调用 `TimelineContext::activate`，分支生命周期 `BranchLifecycle` 进入 Active；能力在 apply 中派生的后台业务等它才开始，分支装配失败时它直接进入 Closed，后台业务从不开始。分支进入 Closing 时 `BranchLifecycle::closing()` 返回，后台业务随即结束，不等所属能力卸载。初始化内部可持有句柄，不等于公开发布；发布与关闭准入在树锁内裁决，不持锁运行 before 或异步构建 |
| 回滚 | 禁止继续装配，取消初始化，等待已开始的 apply 和所有清理；永久卸载该作用域子树 |
| 返回错误 | 包含初始化原因、未就绪插件/服务与清理错误；不发布半成品句柄 |

构建结果与清理结果分别保存：初始化拒绝或取消但回滚成功，不使 Session close 失败；普通候选失败不关闭仍可用的 Session。真实回滚失败必须进入最终关闭结果，不能按 `InactiveEffect` 等错误名称过滤。rutis 以 apply 原因在前、回滚错误在后的结构保留失败插件的装载结果，子树 shutdown 会再次报告它；runtime 按卸载前状态从 shutdown 结果中去掉已作为构建原因报告的部分，构建入口合并两者向创建调用方报告。

同一缓存清理结果可能经创建返回值、树关闭及子树卸载被多次观察。runtime 在既有错误聚合入口展开聚合与框架回传包装，按共享错误对象的身份只保留一次；不同对象即使类型和文案相同也全部保留。不改变 rutis 原有诊断、清理执行次数或并发关闭的缓存结果。

初始化期限到达后进入回滚；期限不会让不合作的初始化自动终止。若第三方初始化不响应取消，返回的状态必须保留“仍在关闭”，应用继续持有资源；禁止把等待超时记录成回滚完成。

## 4. 受管执行

各能力直接持有实际工作所需的任务、取消句柄与状态，并在 rutis 登记自己的清理。上下文准备管理准备操作，模型能力管理请求与流，工具能力管理批次及各调用，发起脱离工作的能力管理自己的脱离工作；不建立统一任务对象或在途计数，也不设跨能力共享的执行许可。正常停止由 stop/interrupt 经 Dispatcher 分发，详见[中断设计](interrupt.md)。

```mermaid
flowchart TB
    Request[收到工作请求] --> Inflight{所属 turn 已被中断}
    Inflight -->|是| Direct[直接提交取消终态]
    Inflight -->|否| Admit[检查关闭]
    Admit --> Record[登记操作与任务]
    Record --> Started{启动前被取消}
    Started -->|是| Cancelled[产生取消结果]
    Started -->|否| Work[执行宿主工作]
    Work --> Join[等待宿主结果及真实清理]
    Cancelled --> Settle[发起能力结算并提交事件]
    Join --> Settle
    Settle --> Consumed[Dispatcher 消费终态]
    Direct --> Consumed
    Consumed --> Done[Loop 写 turn/end 后停止等待者可完成]
```

| 规则 | 内容 |
| --- | --- |
| 准入 | 关闭检查和操作登记在同一临界步骤完成；登记之后才能 spawn |
| 任务集合 | 能力自有 TaskTracker 跟踪任务至真正退出，脱离工作同样由发起能力跟踪 |
| 取消 | 向目标操作发信号，继续等待；不能通过丢弃 JoinHandle 或 abort 代替正常停止。在途任务同时观察 rutis 代 token（`ctx.cancelled()`）：插件卸载、驱逐或重载在清理前由 rutis 预取消，Disposer 只负责等待退出与结算 |
| 结算 | 不受普通 stop 取消；模型与工具能力提交自身的终态。取消路径先释放资源再写终态，turn 闭合即隐含本轮工作已收尾 |
| 启动前取消 | 已接纳操作仍有结算责任；不调用宿主，也不能直接丢弃整个执行 Future |
| 成功与取消竞争 | 保留真实已完成的成功结果；不因稍晚观察到 token 改写为取消 |
| 工具批次 | 单个调用独立收尾；全部收齐后按原 calls 顺序提交结果 |
| 失败监督 | 观察 Result 和 JoinError；未处理 panic/异常退出进入故障路径，不伪装正常取消 |
| 长期任务 | 能力自己拥有的监测任务也要登记取消与 join；裸 `ctx.handle().spawn` 不提供这项保证 |
| 释放次序 | 等真实任务结束后释放其资源；能力关闭时才移除长期订阅 |

各能力通过 rutis Effect 登记自己的取消与等待逻辑；单个操作不增加长期 EffectRecord。收到中断时先发出取消并返回，异步工作完成后提交终态；Dispatcher 不等待宿主 join。卸载整个作用域子树前先等待正常业务结算，故障路径仍由能力 finalizer 取消并等待。

## 5. stop 与 close

| 操作 | 结束范围 | 保留范围 |
| --- | --- | --- |
| 连接断开 | 连接读写、游标和等待者 | Session、模型/工具、审批 |
| 普通 turn 结束 | 本轮前台任务与资源 | 能力、订阅、历史、脱离工作 |
| stop(branch) | 目标分支在途的前台操作与当前 turn | 脱离工作、兄弟分支、历史、读者和长期能力 |
| Timeline close | 本分支执行、脱离工作、Dispatcher、能力与分支子树 | 其他分支和 Session 共享服务 |
| Session close | 全部分支、脱离工作、审批与 Session 子树 | 其他 Session |
| Agent close | 所属 Session | 其他 Agent |
| App Server shutdown | 所有 Session、共享设施、连接和监听器 | 关闭协调任务最后结束 |

```mermaid
sequenceDiagram
    participant S as Session 关闭流程
    participant T as Timeline
    participant C as 各分支能力
    participant H as 模型与工具宿主
    participant D as Dispatcher
    participant R as rutis 作用域子树
    S->>S: 关闭新输入和新分支准入
    S->>T: 写入 branch 范围 stop/interrupt
    T-->>D: 读取已提交中断
    D->>C: 分发中断，能力各自取消在途操作
    C->>H: 请求停止
    H-->>C: 实际收尾完成
    C->>D: 经 Timeline 提交终态
    D-->>S: 可正常分发的终态全部处理完成
    S->>D: 停止并等待 Dispatcher
    S->>R: 永久卸载各分支子树
    R->>T: 清理期间原样提交终态，无 before 轨迹
    R-->>S: 释放插件资源及驱动任务
    S->>T: 清理完成后关闭本分支 Timeline，等待写入落盘
    S->>R: 永久卸载 Session 子树
    R-->>S: 共享资源释放结果
    S->>S: 保存关闭结果并移除活动注册
```

业务关闭由 Session/TimelineTree 写入中断事件并等待；门面不直接调用能力的业务取消接口。不能直接调用 rutis dispose 跳过中断分发和终态等待。正常业务结算阶段，宿主端口、Timeline 和订阅必须保持可用；随后停止 Dispatcher、卸载作用域，存储正常时，清理回调仍可写入 Timeline，事件仅持久化，不再分发。全部清理完成后关闭 Timeline，等待所有已接纳写入提交并发布，再完成 Session 关闭；读者在发布完成后收到分支 EOF。空闲卸载同样保留此写入窗口。

App Server 不自动卸载空闲 Session：Session 保持载入直到关闭或进程退出，需要节省内存的宿主自行调用 `Session::unload_if_idle()`。空闲卸载的前置条件为各分支 Loop idle。`Session::unload_if_idle()` 按分支 id 顺序持有各分支提交锁，在锁内逐一检查 Loop idle、无待发布事件、分发已追到尾部；全部通过才把各分支冻结在该尾部，任一不通过则释放锁并放弃，检查期间的并发写入只会短暂等锁，不会被拒绝。在途脱离工作是否阻止卸载随其识别机制后续设计，当前不检查。冻结持续到该分支分发停止，随后解冻供能力 finalizer 写入终态。卸载与关闭时的资源释放由各能力登记的 rutis effect 保证。存储提交失败的 Session 丢弃未发布尾部并拒绝清理写入，未完成操作留待恢复结算。

| 并发或故障情形 | 约定 |
| --- | --- |
| 多次 close | 共享同一关闭流程和结果；上层关闭已关闭分支时加入其缓存结果 |
| 调用者放弃等待 | 已受理的 stop、close 与构建继续由实例拥有；等待者退出不取消收尾 |
| 关闭等待超时 | 保持 Closing，报告未结束的工作；不声称 Closed |
| Closing 后新增 | 拒绝输入、分支、任务和新能力注册 |
| Dispatcher 已故障 | 不等待不可能推进的消费游标；记录未分发范围，取消工作，直接进入资源清理 |
| 释放失败 | 继续释放其他资源，返回汇总错误 |
| 外部跟随读取 | Timeline 关闭唤醒读者并结束该分支跟随；Session.closed 等待全部清理结果及活动注册移除，不能以 main EOF 代替。连接通知排在该订阅的事件之后；慢连接和断线不拥有或阻塞清理 |
| 关闭后历史 | 已读取的事件由调用方持有；持久历史经 TimelineStore 读取，不保留活动子树；默认内存 Session 关闭后释放历史，显式共享存储由所有者决定保留期限 |
| 存储提交失败 | 未发布事件丢弃，该 Session 拒绝后续写入并清理；不再写清理终态，恢复时结算孤儿操作；其他 Session 保持可用 |
| 强制退出 | 单独报告强制结果；不归入正常释放保证 |

## 6. 错误归属

| 错误 | 产生方 | 首版处理 |
| --- | --- | --- |
| 参数/状态拒绝 | 外部边界、Session 准入 | 返回调用错误；未接纳操作不产生假终态 |
| 控制事件无法提交 | Session / Branch（stop） | 保留原始错误和已提交输入；报告 Dispatcher 故障，进入 Session 资源清理 |
| 模型失败、工具失败、业务取消 | 对应能力 | 转换为兼容业务结果与事件 |
| 缺依赖、插件初始化失败 | rutis + 实例构建入口 | 构建失败并回滚；不发布实例 |
| Dispatcher handler 缺陷、关键能力异常退出 | 对应组件 + 监督入口 | 通知宿主，关闭故障 Session；保留原始错误和日志 |
| 脱离工作的预期失败 | 拥有该工作的能力 | 以该能力自己的结果事件记录失败；不自动关闭全部 Session |
| 资源释放失败 | rutis 与宿主关闭接口 | 汇总到 CloseError，仍继续清理 |

“关键运行缺陷关闭 Session”是 Rust 首版策略，用于保持 v1 的故障通知与生命周期可解释；不是宣称原设计 D2/D3 已经最终定案。业务错误保留类型化枚举；跨包基础设施错误携带组件、实例、操作身份和原始错误，不解析错误字符串决定行为。

rutis error sink 进入诊断与监督，不能代替业务终态，也不能作为唯一的关闭成功判断。

## 7. 禁止形成的等待环

| 场景 | 约定 |
| --- | --- |
| 任务等待包含自己的集合 | 任务只请求停止；集合拥有者负责 join |
| Dispatcher handler 等待终态被自己消费 | handler 返回后由外部停止流程等待游标 |
| 持锁等待子任务 | join 前释放所有子任务收尾需要的锁 |
| 依赖先被卸载 | 先结算业务，再停止 Dispatcher，最后关闭依赖资源 |
| 共享服务引用循环 | Timeline 只持弱化或无拥有权的树命令句柄；不让 Timeline 与持有它的树相互强持有 |

## 8. 验收

验证跨 crate 能力依赖与异步回滚；初始化开始前和执行中取消；宿主清理屏障；stop 后再提交；脱离工作跨 turn；关闭调用者放弃等待；并发 close；工厂、元数据和 Future 构造/poll panic；子插件清理失败；反复创建/关闭后 driver 和插件回收。公开流程测试见 `crates/*/tests`（如 `crates/dim-agent-runtime/tests/lifecycle_review.rs`）与 `tests/integration`。
