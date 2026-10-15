# Session 启动就绪

状态：Proposed，待维护者确认；接口与流程尚未实现。跟踪：[#46](https://github.com/arcships/dim/issues/46)，实现 PR [#82](https://github.com/arcships/dim/pull/82)。机制所有者：`dim-agent-runtime`，沿 roadmap 的 core 机制负责线评审和交付。

## 1. 目标与启动顺序

运行时对成功创建、准备后创建和恢复 Session 的保证应当相同：返回的 Session 已完成首次输入所需的初始化。服务和订阅已装配，不等于依赖完整服务表的初始状态已经准备好。

当前 [`build_session`](../../../crates/dim-agent-runtime/src/assembly.rs) 先等待必需 Session 部件 Active，再创建/恢复分支、等待 `BranchOpening`，最后由 [`Agent::create`](../../../crates/dim-agent-runtime/src/agent.rs) 发布 Session 并触发 `SessionLifecycle::activated()`。C3 如果等发布后才接入工具，会与第一个输入竞争；如果让 BranchOpening 等这项接入，则形成“Session 发布等待分支打开，分支打开等待 Session 发布”的环。

本提案在服务装配完成与分支打开之间，增加一次**同步准备**：

```text
Session parts apply: provide services, subscribe, register preparation
    |
existing required-part Active wait and assembly audit
    |
SessionOpening: run synchronous preparation once
    |                 (SessionLifecycle remains Loading)
    |
create/restore branches
    |
existing branch/start or branch/resume
    |
existing BranchOpening receipts + committed dispatch fence
    |
Agent publishes Entry::Ready + SessionLifecycle::Active
    |
return Session -> accept the first input
```

需要确认 [A.5](session-boundary.md#a5-tool-contribution) 的时点调整：初次工具取样由 `activated()` 后移到上述准备阶段。提供者在取样后才 Active，仍等下次 Session 重载，不自动热接入。异步的分支初始化和 Prompt 采用继续由现有 BranchOpening 负责。

`SessionLifecycle` 保留 `Loading -> Active -> Closing -> Closed`。Skills、MCP 等后台任务仍等 `activated()` 才开始。Agent 的创建结果、`get_session` 和 `sessions` 继续只暴露 `Entry::Ready`；不公开“已发布但不能输入”的 Session，也不在每个 Session/Branch 输入或模型请求上新增等待。

## 2. 一个同步初始化扩展点

`SessionScopePlugin` 提供运行时自有的 `SessionOpening`。它只封装 Session 内的一次准备调用，不是新 Capability，也不建立后台任务集合或另一套生命周期管理器。

Rust 绑定的目标接口：

```rust
SessionOpening::register(
    &self,
    ctx: &Ctx,
    prepare: impl Fn() -> Result<()> + Send + Sync + 'static,
) -> Result<()>;
```

| 项目 | 合同 |
| --- | --- |
| 登记 | 已由现有候选装配等待的部件，在自己的 `apply` 中通过 `Deps` 取得 SessionOpening 并登记；不收集可选提供者的启动回执，不增加 required/optional 配置开关 |
| 时点 | 全部必需部件 Active 且审计完成后，运行时封存登记并调用一次；此时尚未打开分支 |
| 回调 | 只查询已装配的公开服务、完成本部件的有限内存准备并发布本部件状态；不做网络/文件 I/O、不等待、不 spawn、不打开分支、不替换子实例，也不新增需要异步释放的资源 |
| 依赖 | 必需服务仍在 `Deps` 声明并于 `apply` 取得。公开合同允许的可选按名查找发生在自身已 Active 的回调中；不增加可选依赖边、`Maybe` 或审计豁免 |
| 顺序 | 部件之间没有准备顺序合同；回调不依赖其他准备回调的结果。不增加第二张依赖图或重试机制 |
| 结果 | 每个已登记的必需部件都须成功；无登记则直接继续。调用方不能短路其他回调，错误使本次候选构建失败 |
| 迟到登记 | 封存后拒绝登记，不静默漏掉初始化。装配期间部件若换代，候选失败并回滚，不替新代自动重放回调 |
| 正常重载 | Session 已 Active 时不重开准备阶段；单独部件重载使用其原有初始化方式。整个 Session 再次装载时创建新阶段 |

实现复用 rutis 0.10 已有的同一 EventBus：Session 实例键下的私有同步初始化通知，以 `on_sync` 登记，由 runtime 调用 `bail_sync`；包装器把成功统一映射为 `None`，从而调用全部监听器，错误原样返回。内部事件键不向业务插件暴露；插件只通过 SessionOpening 登记。它不是 Timeline 事实，不进入 Dispatcher、历史或 wire，也不能用于传递运行中的业务指令或业务数据。

rutis 已提供同步调用的 panic 捕获、实例隔离、代次所属订阅和卸载等待；本机制直接复用，不再造回调任务、join 集合或完成回执。登记随所属代次的 effect 清理，已进入的同步回调返回前，所属代次不能释放它正在使用的资源。callback 不跨异步边界，所需资源已在 `apply` 取得并即时登记清理。

准备阶段只需封存状态，订阅仍由 rutis 保存。运行时在既有必需部件全部 Active 后保存 `FiberView::watch()` 的代次快照，在准备前后及最终发布前核对候选有效性；观察到其换代或失活则拒绝候选，不能因监听器已被卸载而静默漏调。该检查不把可选提供者纳入必需集合，也不修改 Pending 等待规则。实现不增加业务数据锁，固定工具查找仍是不可变读取。runtime 不持 Agent entries 锁执行回调。

语言中立的合同是 Session 作用域的 `sessionPrepare` 初始化钩子：由部件装载时登记，运行时在上述阶段调用；模式为 sync，输入 `{}`、成功输出 `{}`，失败为 `{code, message}`，所属代次停止即不再接纳调用，已接纳同步调用返回后才释放。Rust 闭包和 AgentError 是本地绑定。非 Rust 绑定由 #4 沿该合同交付，本次不造桥接协议，不要求贡献者使用尚不存在的 API。可信进程内插件的登记不是授权或隔离边界。

## 3. 失败、关闭与代次

| 情况 | 行为 |
| --- | --- |
| 回调返回错误或 panic | 保留原始错误和部件身份，候选失败，走既有子树回滚，不打开分支、不发布 Session |
| 回调与所属代次卸载竞争 | 复用 rutis 同步调用准入与卸载等待；登记代次失效则候选失败，不把旧结果交给新代 |
| 关闭或启动期限到达 | 复用现有候选取消与启动期限；同步调用不能强制抢占，准备返回后检查取消和期限，失效则不再打开分支，回滚并等待真实清理 |
| 同步回调不返回 | 不能声称启动期限能中止它或回滚已完成；这是违反有限同步回调合同的部件缺陷，机制不增加隔离线程或强杀策略 |
| 调用方放弃创建等待 | 已登记构建继续由 Agent 持有；不会丢弃回滚责任 |
| 准备后创建失败 | 沿用现有新建/准备后创建的 `rollback_session` 责任；恢复失败只释放本次加载资源，不删除已存历史 |
| 准备完成后、发布前必需部件换代 | 检查现有必需部件的代次与关闭状态，观察到失效则拒绝发布；发布后的结构重载沿用原有结算规则 |

整个装配、准备和分支初始化共用原有启动期限，不分别增加超时。准备状态只存在于本次加载的内存中，不改变持久化格式、错误协议、Timeline 序号、原子批次或 stop/interrupt 语义。

## 4. C3 接入与首次输入

1. ToolRegistry 的 `apply` 校验名称和配置、提供注册表，并为初次 Session 装载登记同步准备；对 SessionOpening 声明真实依赖，不注入可选 ToolContribution。
2. 回调按 `SessionConfig.tools` 顺序，读取当时 Active 的已登记提供者，构建并一次发布完整固定表。缺失、Pending、Failed 或重载中的提供者被省略并记录诊断，不等待它们。查询只读服务声明，不调用提供者的外部业务。
3. 之后才构建分支。Prompt 初始准备和 ToolBinding 的 `branch/start` / `branch/resume` 处理读取已准备的表；已有 `Prompt::adopt_tools`、BranchOpening 回执与派发 fence 保证采用先于首次输入。没有模型配置时沿用 PrepareModelHost / bind_current 读取同一张表。
4. 正常运行中单独重载 ToolRegistry，沿用既有装载取样、changed 通知与请求快照规则，不重开 Session 准备阶段。旧代处理器的停止与已接纳调用的结算仍归提供者。

首个请求、恢复后首个请求和以后新建的子分支，都复用既有分支打开合同。runtime 不认识 ToolRegistry、工具名、Prompt 或 ToolBinding，不替分支写 `context/configured`。

## 5. 现有机制与消减检查

| 方案 | 判断 |
| --- | --- |
| 真实依赖、gates、Contributions | 保持原有规则；拿来等待可选提供者会恢复 C3 要消除的硬依赖 |
| activated 后 changed 通知 | 适合后台更新，不能保证立即提交的首个请求已采用工具 |
| BranchOpening 等待发布后接入 | 当前启动顺序形成等待环 |
| 发布后新设 ready 状态并逐入口拦截 | 需要覆盖 Session、Branch、恢复、内部首输入，以及分支采用汇总；发布前准备可复用现有分支打开，省去这些状态和入口修改 |
| 提前触发 activated | 会让候选仍可能失败时就开始后台副作用，破坏发布语义 |
| apply 中读取可选提供者 | 不能保证其他部件已完成 apply；不放宽必需依赖声明和审计 |
| 异步准备集合 | 当前缺口只是同步服务取样与内存发布；异步采用已有 BranchOpening，不新增另一套任务、取消和 join 管理 |
| 给工具表加锁、轮询或延时 | 不能建立“初始化完成先于输入”的关系 |

保留的只有一个同步生命周期扩展点：现有服务装配完成后、分支打开前，runtime 调用部件的有限初始化，并把失败归入候选回滚。不增加异步 readiness 能力、公开 Session phase、每输入检查、可选依赖语义或 rutis 改动。

## 6. 影响面与交付

| 归属 | 变更与验收责任 |
| --- | --- |
| 共享设计 | A.5、session-access §9.4、roadmap C3、Web 的引用及中文镜像统一取样时点；Session methods、interactive 和 Rpc 错误合同不变 |
| dim-agent-runtime | SessionScope 提供 SessionOpening，build_session 在打开分支前调用；复用 EventBus、现有期限、代次观察、在途构建和回滚 |
| dim-agent-capabilities | ToolRegistry 初次装载从发布后任务改为同步准备；ToolBinding 和 Prompt 保持自己的既有采用路径 |
| 架构文档与生成输出 | 实现时同步 lifecycle、single-tree、ToolRegistry、tool-contributions；装配图由 xtask 生成 |
| App Server / CLI | 无新增门面、wire 字段或交互状态；回归首次输入、恢复和协作工具 |

同批工作已核对：A2 #43/#79 明确了首次输入要求，C1/C2 #47/#48 处理方法分发和配置，R1 #116/#117 处理管理服务不可用错误，均未提供这一阶段。#114 的[机制负责人决议](https://github.com/arcships/dim/issues/114#issuecomment-6190712696)拒绝可选依赖语义，本提案不重开该能力。C4 #63 沿获批生命周期补语言绑定，不提供另一套启动流程。

先确认并合入共享合同，再由 runtime 机制线实现、独立验证同步准备，C3 随后接入验收。runtime 的交付不得依赖 ToolRegistry 才能证明正确。确认前不改 core，C3 #82 保持未完成；验收后转 Ready for review。

## 7. 验证计划

复用现有最小公开装配，用普通 Session 部件登记准备，不依赖工具贡献或模型行为：

- 创建、准备后创建、恢复返回后，立即经公开服务读取准备结果；准备中的候选尚不在 Agent 可用列表中，尚无本次加载的分支打开事实，activated 尚未触发。
- 覆盖无登记、多部件准备和 Session 隔离；断言公开可用性、初始化错误与资源释放，避免私有 helper 或 mock 调用顺序断言。
- 覆盖回调错误/panic、关闭和启动期限、放弃创建等待、准备中代次停止及迟到登记；验证不发布候选、真实清理完成、原始错误和清理错误保留，恢复历史不删除。同步回调与卸载竞争复用并补足已有 rutis 生命周期公开流程。
- 保留必需 Deps 的 Pending 与未声明 apply 读取拒绝，证明没有新增审计例外。

C3 复用工具/Prompt 流程：create/restore 返回后立即输入，首个模型请求已含完整声明，执行使用该请求的处理器快照；保留提供者不可用、晚到不接入、原代停止和下次装载取样。不得靠 sleep、等待 changed 或手动重载掩盖首次输入竞争。

实现后运行受影响的公开流程和 xtask check / graph，再执行仓库要求的提交检查。本文仅改设计，已有 C3 绿色 CI 不证明新机制已实现，不重复运行代码测试。

## 8. Rule check

| Rule | Contract |
| --- | --- |
| §4 rule 1 | 只完成本部件初始化，不写其他能力的业务状态 |
| §4 rule 2 | 准备发生在公开交接前，已接纳工作及请求快照不改绑 |
| §4 rule 3 | 不增加 before 等待或输入内容检查，首次可用属于实例初始化 |
| §4 rule 4 | runtime 只处理准备、失败、关闭与回滚，不认识业务插件 |
| §4 rule 5 | 序号、批次与派发不变，分支沿用自己的提交 fence |
| §4 rule 6 | 工具声明仍由 Prompt 的已采用配置进入上下文 |
| §4 rule 7 | 依赖声明及公开作用域服务不变，不读提供者私有状态 |
| §4 rule 8 | 需求用候选实例初始化表述，runtime 独立实现验证，见下一节 |
| §4 rule 9 | 准备不增加 Timeline 事实，内部同步通知只用于装配 |
| §1 | 部件拥有状态与资源，runtime 拥有候选和回滚，不加接线插件 |
| ADR 002 D1 | 一个准备阶段属于一个 Session 实例 |
| ADR 002 D2 | 复用 SessionScope、build_session 和既有 EventBus |
| ADR 002 D3 | 不等待可选提供者，不给它建立硬依赖 |
| ADR 002 D4 | 装载准备不把业务配置编辑变成结构重载 |
| ADR 002 D5 | 同步钩子的作用域、数据、错误和代次语义与语言无关，非 Rust 绑定随 #4 交付 |

## 9. Requirements raised to mechanism owners

- **Blocked consumer and rule:** C3 #46 的首次输入要求与现有“分支打开后才发布、后台接入等发布”的顺序不能由现有 BranchOpening 串接；A.5 按本提案调整取样时点。
- **Existing mechanisms considered:** 第 5 节列出依赖门控、activated 通知、BranchOpening、逐入口门控、提前激活、apply 读取、异步集合和锁/延时的限制。
- **Generic capability:** 必需 Session 部件装配后、分支打开前，执行其登记的有限同步初始化，出错由同一候选构建回滚，不带工具含义。
- **Ordering and lifecycle effects:** 不分配序号，不改批次、中断、格式和 activated 的发布语义；只改变首次创建/恢复时的准备顺序，正常部件重载不重开该阶段。
- **Owner and issue:** Dim runtime 机制负责线；设计确认跟踪 #46/#82，实施前按 roadmap §5.3 确定机制线的独立交付单元。不向 rutis 提需求，不归入已被拒绝的 #6 可选依赖能力。
