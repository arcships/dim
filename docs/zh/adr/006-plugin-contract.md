# 006：插件契约是概念与约定，而不是类型

[English](../../adr/006-plugin-contract.md)

决策状态：已由仓库所有者接受（2026-10-24）；同日修订 D4，写明各运行时如何解析名字、Rust 的 key 从哪来、
部件可以在实例内提供哪些名字，以及如何找到带前缀服务的提供者；同日改写 D3：插件是一行，每个 Session 和分支
挂载插件为它登记的部件；2026-10-25 修订 D3 和 D4：插件在哪里运行、贡献向所有者登记、外来插件等待 rutis#642。
2026-10-26 修订背景、D4 与两处说明：背景标明它描述迁移之前的状态，并写明其中哪些机制已被迁移移除；
D4 把移除写成已完成；按层 API 与 `sessionLifecycle` 的两处说明改为现状。同日修订 D3：外来插件以 rutis
插件定义登记部件，rutis 0.17 以此交付了 rutis#642。
实现：P1、P2、P3、R6 与 P4 已完成；R5 的第二种形式随 #403 完成。各阶段见
[插件契约设计](../design/plugin-contract.md#10-迁移)。

与其他决策的关系：

- D3 第一点由 [ADR 007](007-plugin-configuration.md) 纠正：插件配置归 PluginManager，插件带的内容不是行。
- 扩展 [ADR 002](002-plugin-scope.md)。其 D1–D4 不变（D4 中结构性配置的存放位置后来由 ADR 007 纠正）。本 ADR 补全 D5：
  Dim 自己的运行时服务成为契约中的命名条目，Rust 成为契约的一种绑定，而不是契约的定义。
- 保留 [ADR 001](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)（信封、保留
  领域、描述性的事件声明）和 [ADR 003](003-approval-scope.md)（受信任的进程内插件不靠声明隔离）。
- 写在两份分开的设计里：[Dim 如何使用 rutis](../design/rutis-usage.md)（D2–D4）与[插件契约](../design/plugin-contract.md)（D5–D8）。
- 取代 [rutis 0.15 升级设计](../design/rutis-0.15-upgrade.md)的事件作用域模型
  （§4、D2–D4）。由 [rutis 0.16 升级设计](../design/rutis-0.16-upgrade.md)取而代之。
- 取代按层划分类型的编写 API 作为目标：`Capability` trait 及其 `keys`、
  `SessionScope`/`BranchScope`、`InSession`/`InBranch`/`SessionGate`/`BranchGate`、
  `SessionService`/`BranchService`，以及 `AgentBuilder::session_plugin`/`timeline_plugin`。
  迁移已移除它；D4 列出了移除的内容。
- 替代未合并的实例挂载草案（#377）以及 #370 的决策。

## 背景

本节记录作出决策的理由，因此它描述迁移之前的 `main`（dd87f6af）。此后实现了什么见开头的决策状态；
今天成立的内容见 [Dim 如何使用 rutis](../design/rutis-usage.md)。

Dim 的架构建立在一套小理论之上。[时间总线](../../architecture/rust-agent/time-bus.md)
按序号确定顺序，让每份状态只有一个所有者，把变更作为指令交给所有者，让交接显式化，把 `before`
限定为变换草稿，从历史重建状态，且从不重放副作用。ADR 002 D1 加了一条位置规则：部件放在其状态
所有者所在的位置。这些都不依赖某种编程语言。

插件 API 及其相关指引用 Rust 类型表达这套理论：

| 概念 | 当前表达 | 在 `main`（dd87f6af）上的规模 |
| --- | --- | --- |
| 部件放在哪里 | `session_plugin(\|scope: &SessionScope\| ..)`、`timeline_plugin(\|scope: &BranchScope\| ..)`、`Capability::keys` | 199 个 `Capability` 实现 |
| 依赖来自哪个实例 | `InSession<T>`、`InBranch<T>`、`SessionGate<T>`、`BranchGate<T>`、`SessionService`/`BranchService` 标记 | 221 处使用 |
| 事件流的隔离 | `EventKey::of().instance(id)` | 9 处，rutis 0.13 已移除的 API |
| 给作者的规则 | AGENTS.md 第 1 条、[原则 §1](../../architecture/rust-agent/principles.md)、[single-tree](../../architecture/rust-agent/single-tree.md#实例键与可见性)、插件页面 README | 27 篇英文文档提到这些类型 |

由此产生四个后果。

1. **非 Rust 部件需要按层的绑定，重新推导 Rust 类型已编码的内容。**
   - App Server 根据 TypeScript 行的 `inject` 是否点名了分支服务来推断该行的组
     （`crates/dim-app-server/src/session_typescript.rs:373`），并硬编码每个组可以提供哪些前缀的服务
     （`:400`、`:457-492`）。
   - 一个服务的组被声明了三次，彼此没有交叉校验：`SessionService`/`BranchService`
     标记、`build.rs` 中的 `service_in(.., "branch")`，以及 `BRANCH_SERVICES`。
   - 一个概念有两个标识。原生的工具贡献是 `TypeKey::keyed_dynamic::<ToolContribution>`，
     外来的是 `host_key_in("toolContribution:..")`。ToolRegistry 两种都读，并通过一个按同一字符串
     找到的旁挂取消令牌来发现正在退役的外来代次（`tool_registry.rs:486-509`）。
   - 宿主预先注册提供者名称（`session_tool`、`session_methods`、`session_prompt_source`、
     `session_tool_source`、`host_tools`），并把外来行的 `provides` 翻译成这些调用。
     ADR 002 D5 规定注册由插件声明，而不是由宿主侧的 `AgentBuilder` 调用完成。
   - Session 与 Settings 的总线事件只能通过 `Process::emit` 上的中继到达外来行
     （`:166`、`:251`、`:515`），而这是 rutis 0.13 已移除的 API。
   - TypeScript Session 行没有发布（`activated`）服务，而 Rust Session 部件有
     （[作者指南](../../typescript-plugin-authoring.md#plugin-shape-and-lifetime)）。
2. **每个消费方都在每种语言中重新实现同样的基础设施协议。** 每个有序消费方都要订阅、固定已提交的
   截点、折叠本地历史并跳过已覆盖的事件：`crates/dim-agent-capabilities/src/wait.rs:361-380`、
   `runtime/plugins/goal/branch.ts:120-128` 和作者指南的示例都这样做。一个 TypeScript 部件
   自己把原生错误变体翻译成 JSON-RPC `appError` 错误码（`runtime/plugins/goal/session.ts:15`），
   这是在插件内部做的边界转换。
3. **指引混杂了四类陈述**：理论、约定、当前的 Rust 写法，以及交付状态或历史。贡献者分不清哪句
   是规则、哪句在描述今天的代码。因此设计文档的“规则检查”小节只是复述这份清单，而不是回答它。
4. **rutis 在底层变了，从 0.13 到 0.16：**
   - 每个根一条平的事件总线。一个名字的每个监听器都会收到该事件，无论它在哪里注册。监听器看不到
     发送方的实例，事件载荷必须可 serde。
   - 按名称提供的实例服务。目录中的一个名字属于一个实例化组，并对每个行副本解析到包围它的、
     最近的该组实例。放在该组之外的行会在组合时失败，无论它是 Rust 还是其他任何运行时
     （`ServiceCatalog::register_instance`、`register_shared_instance`、`key_in`、`Build::instance`）。
   - `#[rutis::service]` 在提供 Rust 服务的实例中按名称提供该服务。
   - rutis 写明的设计：“依赖声明只包含服务名”。模型只在 Rust 内核中实现一次，其他语言是带一个
     小型 SDK 的叶子。rutis 自己的 agent 插件使用普通插件、领域服务 trait 和命名事件，没有自己的
     插件 trait。

0.15 升级设计（#361）围绕按作用域投递来规划，而 rutis 0.15.1 撤回了按作用域投递。#377 草案向
rutis 模型靠拢，但保留了本 ADR 拒绝的两样东西。它在名字之外保留了 Rust 的 `Service<T>` 标识。
它还提出由 loader 强制的 Capability 角色，这是一种访问检查，而 [ADR 003](003-approval-scope.md)
指出受信任的进程内插件没有这种检查。

第 1 个后果列出的机制此后都已被移除：行自行声明所属组，App Server 不再推断；每个名字及其组只声明
一次（[Dim 如何使用 rutis §3.4](../design/rutis-usage.md#34-服务目录)）；工具贡献只有一个标识，
向所有者登记；贡献由提供它的部件登记，因此宿主不预注册任何贡献，也不翻译 `provides`；其他语言的行
通过 `sessionLifecycle` 等命名服务取得发布与关闭，并直接监听总线，而不是经过中继。仍保留的是 R5 的
第二种形式：外来功能仍是一个部件一行、各自声明所属组，直到 #403 将其移除。

## 决策

### D1. 分层，每层一个权威来源

| 层 | 内容 | 权威来源 | 变更途径 |
| --- | --- | --- | --- |
| Dim 架构：理论 | 序号、所有权、指令、交接、`before`、重建、副作用、按所有者放置 | [时间总线](../../architecture/rust-agent/time-bus.md)、[原则](../../architecture/rust-agent/principles.md)、[ADR 002](002-plugin-scope.md) | 决策记录 |
| Dim 架构：插件契约 | 实例树的含义、位置、Dim 的服务与公开总线事件、Timeline 事实及其消费、总线事件如何消费、Capability、发布与关闭 | [插件契约](../design/plugin-contract.md) | 本 ADR 之下的设计变更 |
| 使用 rutis | 部件即 loader 行、按名字与组的服务、服务目录机制、跨语言、rutis 生命周期与事件总线 | [Dim 如何使用 rutis](../design/rutis-usage.md) | 本 ADR 之下的设计变更 |
| 绑定 | 一种语言如何写出两者：Rust trait 与宏、TypeScript/Python `definePlugin`、生成的声明 | 《Dim 如何使用 rutis》的附录、SDK 源码、生成的文件 | 实现变更 |

中间两层分开写。D2–D4 决定 Dim 如何使用 rutis；D5–D8 决定建立在其上的 Dim 架构。Dim 的规则只依据
rutis 保证的东西来写，从不依据 rutis 的 API；关于使用 rutis 的规则也不决定任何业务问题。

绑定不增加语义。当某个绑定需要一条任何一层都没有写明的规则时，先改那一层。Rust 是一种绑定：它是
最完整的一种，而不是定义。面向贡献者的规则（AGENTS.md、原则、模板）写在各自的层，具体写法链接到
某个绑定。

### D2. rutis 拥有插件机制；Dim 在其上定义自己的领域

rutis 提供以下内容，Dim 直接使用：

- 插件生命周期（apply、effects、unload、取消、截止时间）；
- 依赖门控、Pending 与重载传播；
- 按类型和按名称的服务、实例化组，以及按实例的名称解析；
- loader 行与配置；
- 语言运行时、跨语言调用、事件总线与诊断。

Dim 定义：

- 实例树、其中的实例何时打开和关闭（`agent`、`session` 和 `branch` 组），以及每个实例借以挂载插件为它登记的部件的部件登记处（D3）；
- Timeline 机制（追加、序号、批次、发布、`before`、Dispatcher）；
- 其命名服务与公开总线事件的目录；
- 事件契约与保留领域；
- 部件约定（D6）、装配 profile 与审计（D9）。

Dim 不增加以下任何一项：插件 trait、面向作者的作用域类型、解析器、清单（manifest）、角色字段、
第二个服务目录、第二条总线，或按语言划分的中继与适配器。如果某个需求看起来需要其中之一，先查
rutis。真正的缺口在 Dim 自己的机制内解决，遵循[原则 §4 第 8 条](../../architecture/rust-agent/principles.md)；
rutis 仍是外部依赖。

### D3. 插件是一行；每个实例挂载为它登记的部件

- **插件是进程里的一行。** 每个插件都是一个 rutis 插件，在进程里挂载为一行 loader 行，无论原生还是外来，
  内置还是可选。它的配置、启用和重载都是这一行的。插件为进程持有的东西，由它自己持有。
- **部件是登记的，不是配置的。** 对 `session` 和 `branch` 作用域，插件在运行时向 Dim 的部件登记处登记部件
  生成器。登记属于插件的这一代：插件卸载时撤销，下一代重新登记。
- **由实例挂载。** Session 或分支打开时，调用为它的作用域登记的所有生成器，把每个结果挂成自己的子节点。挂上的
  部件就在这个实例里：它的名字在这里解析（D4），并随实例关闭。插件从不伸进实例，实例也只往自己里面挂部件。
- **登记跟随打开着的实例。** 有实例打开时新登记的生成器，会挂进其中每一个；撤销的生成器从每一个中卸下，
  这些部件都卸载完，撤销才算结束。
- **插件登记什么。** 在 Rust 中登记生成器：它返回一个按实例的 key（D4）构造的插件，也可以对某个实例什么都不返回。
  它看得到实例的标识和配置，配置里关掉了某个功能的 Session，就完全不挂这个功能的部件。其他运行时的插件登记一个
  rutis 插件定义，用该运行时的 `definePlugin` 做出；每个实例挂载它的一份副本，副本在插件的运行时里运行。定义不能
  什么都不返回，所以实例的配置关掉了这个功能时，副本不做任何工作。按实例的选择走 `SessionConfig` 这类业务配置，
  不走配置行。
- **发布。** 宿主的装配 profile 决定 Session 或分支在发布前要等待哪些部件
  （[single-tree](../../architecture/rust-agent/single-tree.md#结构)）。必需的部件要处于 Active，而不只是已加载。
  实例打开时已登记的其他部件，像实例的可选行一样，在发布前先加载结束，所以实例发布时它们登记的东西（工具、
  方法）已经就位；其中处于 Pending 或 Failed 的，只影响它自己的功能。发布后才挂上的部件不会让实例再发布一次。
  插件不声明自己是必需的。
- **部件内部。** 部件仍可以在自己的子树里创建子节点，比如 McpSession 为每个服务器、Skills 为每份内容各建一个。
  这些是部件自己的事。
- **插件在哪里运行。** App Server 把插件作为它 loader 里的进程行放置。库宿主或测试把插件加到 Agent builder 上，
  自己放置的 Agent 把它们挂进自己的 loader。登记处属于一次放置，所以自己放置的 Agent 只挂它自己的插件登记的部件。

位置照旧遵循 ADR 002 D1：插件为它持有状态的每个作用域登记一个部件。位置不由任何其他东西编码：没有插件 trait，
没有工厂签名，不从注入的名字推断，配置里也没有 Session 行或分支行。`agent` 组承载宿主的门面，而不是功能部件。
细节见 [Dim 如何使用 rutis §2](../design/rutis-usage.md#2-插件是一行实例挂载它的部件)。

### D4. 服务是某个组中的一个名字，目录就是契约

- **标识。** 一个部件提供给另一个部件的每个服务都有一个 camelCase 名字，且恰好属于一个组：
  进程、`session` 或 `branch`。名字从不回退到另一个组（rutis 规则）。交给同一个所有者的一组贡献（如一个
  Session 的工具）不是一组名字：每个贡献向所有者登记（见下文）。
- **一个目录。** 每个 Dim 名字只声明一次。跨语言的服务在其 Rust 类型上用 `#[rutis::service]`
  声明名字；仅限 Rust 的服务，或由外来部件提供的名字，在宿主的 loader 目录中注册。目录只共享
  已注册且已标记的名字（`share_registered_only`），因此跨语言的恰好就是这份目录。`xtask`
  从真实装配把它输出为一张生成的表：名字、组、提供者、方法（同步或异步，serde 请求与回复形状）、
  错误，以及该服务是跨语言还是仅限 Rust。
- **在每种语言中都按名称依赖。** 部件写明它注入和提供的服务名字；部件里没有任何东西决定它拿到哪一份。
  对每个副本，rutis 把名字解析到该名字所属组最近的外层实例，不回退，并拒绝点名了其所在实例之外的组的部件。
  这是 rutis 对“每个实例一份”的服务的模型：每个实例的 key 归框架，不归部件。各运行时用各自的方式落实这条规则：
  - 在 Node 和其他外来运行时中，rutis 把每个这样的名字隔离在该实例的标签下（Node 中即 Cordis 的 `isolate`）。
    部件读普通的名字，拿到的就是自己实例的那一份；未经修改的 rutis Cordis 4 插件也这样工作。
  - 在 Rust 中，绑定按名字和副本的实例链从目录取得每个 key，因此不论用哪种语言读，提供方都在同一个 key 上。
    用 Rust 类型登记的名字，key 是该类型的 key，用组的实例限定；绑定按 `#[rutis::service]` 的规则给这样的
    类型命名，因此一个 Rust 类型就是一个名字。跨语言的名字位于它的分发器的 key（`host_key`，同样用实例限定），
    Rust 部件在那里读取任何语言的提供方。导出的 Rust 服务同时也在它的原生 key 上，导出从那里提供给其他语言，
    同类型的 Rust 部件也在那里读取。
- **隔离归框架。** 部件看到哪一份由它的位置和目录决定。部件不隔离它共享的服务。行上的 `isolate`
  保留给只属于该部件或一组部件的私有名字，与 Cordis 的用法相同；目录不共享这些名字。
- **只属于一个实例的名字要登记。** 行没有声明、在运行时才宣告的服务（Cordis 插件就是这样），rutis 以其普通名字
  在整个进程内发布。在 Session 或分支内，这就成了一个进程级的服务：其他实例也能看到它，第二个提供它的副本
  会因 rutis 的重复提供者错误而失败。因此部件为自己实例提供的名字要以其组登记在目录中，部件私有的名字由它的行
  隔离。Dim 不另加检查。
- **仅限 Rust 的服务仍在目录中。** 方法无法跨语言的服务（在提交锁内执行的回调、句柄、通道）
  仍然有名字和组，目录将其标记为仅限 Rust。要让它跨语言，就给它 serde 形状的方法；不存在手写的
  适配层。
- **错误是数据**，`{code, message}`，错误码有文档说明（ADR 002 D5）。服务错误由 wire 边界
  （Rpc）映射为 wire 错误，而不是由转发它的部件映射。

- **贡献向所有者登记。** 工具、工具来源、提示词来源、Session 方法命名空间和管理方法命名空间各有一个
  所有者保管它们的表：ToolRegistry、PromptRegistry 或 Rpc。提供贡献的部件在自己的代码里调用所有者的
  `register`，像任何依赖一样按名字拿到所有者。登记属于调用方，调用方卸载时撤下。宿主不预先注册贡献，
  所有者不去查找贡献，一个贡献在每种语言里只有一个标识：它的登记。
  - 在 Rust 里调用方传入自己的 ctx。从其他语言调用时，bridge 把登记绑定到发起调用的行上，与
    `sessionLifecycle.hold` 相同；处理函数是调用方自己的函数，经 bridge 回调。
  - 登记撤下时，这个贡献的这一代就结束；没有旁路来标记正在退役的贡献。
  - 配置了却没有任何部件登记的名字，由所有者在用到时报告，与它报告任何缺失的贡献一样；创建 Session 时不拒绝它。

实现已移除：`Capability` trait 及其 key、`SessionScope`/`BranchScope`、
`InSession`/`InBranch`/`SessionGate`/`BranchGate`、`SessionService`/`BranchService`、
`session_plugin`/`timeline_plugin`、宿主侧的名称预注册、`toolContribution:*`、`sessionMethods:*` 和
`managementMethods:*` 这些名字及其原生/外来之分、Session 行和分支行（包括其他语言的，随 #403）、App Server 对行所属组的推断，
以及目录中的组已经表达的按前缀位置检查。
`PublicService` 保留。它限制 Session 与分支门面向普通调用方暴露哪些分支服务，这是门面规则，不是插件规则。

细节见 [Dim 如何使用 rutis §3](../design/rutis-usage.md#3-按名字的服务)；Dim 定义的名称列在
[插件契约 §4](../design/plugin-contract.md#4-dim-的服务)。

### D5. Timeline 事实与总线事件是两回事

| | Timeline 事实 | 总线事件 |
| --- | --- | --- |
| 是什么 | 一个分支的持久、有序号的事实，采用 `AgentEvent` 信封 | 根的平总线上一个短暂的 rutis 事件 |
| 写入方 | 分支的 `timeline` 服务；由生产者保证载荷 | 任何部件，通过 rutis `emit`/`parallel` |
| 投递 | 分支的 `dispatcher`：按序号顺序，本代次注册的每个部件都收到每一条事实 | 只投给发出时已注册的监听器；不存储、不回放；不同名字之间没有顺序；`emit` 不等待 |
| 消费方如何恢复 | 从 Timeline 重建 | 按[时间总线 §3](../../architecture/rust-agent/time-bus.md) 对每个状态机的要求：从 Timeline 恢复，或重新读取所有者的状态。漏掉一条通知只会推迟决定，不会改变它 |
| 标识 | 领域与名字；核心领域是保留的 | 目录中的公开名字；serde 载荷 |
| 隔离 | 按分支，由构造保证 | 按名字。属于某个实例的总线事件在名字中带上该实例的标识，由所有者导出的构造函数生成；或者由所有者改为提供 watch 或服务方法 |

实例界定生命周期与服务可见性。它们不界定事件投递。Dispatcher 与 Timeline 内部使用的总线名字
归它们所有。部件通过 `dispatcher` 消费，通过 `timeline.before` 注册变换，从不使用那些名字。

### D6. Capability 是约定：分支 Timeline 上的状态机

Capability 是按序号从分支的事实折叠状态（经 `dispatcher` 消费），或写入自己领域的事实（经 `timeline`）
的部件。它只在[原则 §1](../../architecture/rust-agent/principles.md) 中定义一次。其他部件不另起名字，就是部件，
现有规则对它们像对每个部件一样适用。Timeline、Dispatcher、TimelineTree、作用域根与门面是各有契约的机制，
不是这个意义上的部件。

这改变了这个词的含义。此前，原则 §1 把每个 Session 或分支部件都叫作 Capability。现在
Capability 是分支 Timeline 上的状态机。只提供工具或方法、或只写输入的 Session 部件不是
Capability。能力重载协议（[single-tree](../../architecture/rust-agent/single-tree.md#能力重载协议)）
适用于 Capability。

是否为 Capability 不声明，也不强制：没有角色字段，没有 trait，也没有 loader 检查。受信任的进程内部件
不靠声明隔离（ADR 003、AGENTS 第 11 条），而在平总线上，loader 检查能覆盖注入，却覆盖不了监听。这样的
检查看起来像边界，实际上不是。一个部件是否为 Capability 体现在它注入的名字中；审计（D9）从真实装配推导出来。

### D7. 机制保证什么，不保证什么

机制保证：

| 方面 | 保证 | 所有者 |
| --- | --- | --- |
| 可见性 | 一个组的名字只在该组的实例内解析；位置错误的行在组合时失败 | rutis |
| 暴露 | 只有目录中的名字跨语言 | rutis 目录、Dim 注册 |
| Timeline 完整性 | 信封检查，保留的核心领域按其类型化契约解码，原子批次，序号，提交后发布，关闭后拒绝 | Timeline（ADR 001） |
| 顺序 | 按分支、按序号顺序投递；重建截点的语义 | Dispatcher |
| 生命周期 | apply、effects、unload、取消与截止时间；只有在 profile 要求的部件都 Active 之后才发布 | rutis，Session 与分支装配 |
| `before` | 进程内、同步、在提交锁内。任何外来运行时都不注册它（[ADR 001](../../adr/001-harness-foundation.md#trust-and-cross-language-calls)：提交关键区内没有跨进程代码） | Timeline |

其余的一切，即时间总线、原则 §4 和 ADR 002 的规则，都是约定，由评审、审计（D9）和流程测试检查。

### D8. 每个消费方都要重复的协议归机制所有者

- **带重建的有序消费成为一个 Dispatcher 操作。** 一次注册先投递直到一个固定截点的本地历史，
  再投递截点之后的实时事件，在每种语言中都如此。手写形式仍然有效。
- **Session 作用域和分支作用域各有一个生命周期服务，面向每种语言：** `session` 中的 `sessionLifecycle`
  （`activated`、`closing`、`hold`）和 `branch` 中的 `branchLifecycle`（`activated`、`closing`）。进程作用域没有；
  进程部件经 rutis 生命周期跟随宿主的启动与关闭。这两个服务都在各自组的服务目录中，因此每种语言都能读取
  （R2，已在阶段 4 交付）。
- **Rpc 把错误映射到 wire。** 贡献的方法的错误在 Rpc 处映射。部件重新抛出它收到的原生错误数据。

这些以需求 R1–R3 的形式写在契约的[需求一节](../design/plugin-contract.md#9-向机制所有者提出的需求)，
采用原则 §4 第 8 条的格式；由 Dim 运行时与 App Server 负责。SDK 可以在契约之上加语法糖，但不能
加语义（D1）。

### D9. 指引分层，审计自动生成

- 每条规则只在一处陈述。AGENTS.md 与原则 §1 陈述规则时不带 Rust 写法，Rust 写法放在 Rust 绑定中。
  契约链接到规则，而不复述规则。
- 设计文档的“规则检查”写明哪些部件是 Capability，并照旧回答原有清单：原则 §4 第 1–9 条、§1 和
  ADR 002 D1–D5。拉取请求模板不变。
- 交付状态与历史放在 ADR 的交付表与设计文档中，不放在规则中。
- `xtask` 从真实装配推导哪些部件是 Capability（它们注入 `dispatcher` 或 `timeline`），连同服务目录表
  一起写入生成的文档。

### D10. 工作顺序

1. rutis 0.16 升级，按[其自身的设计](../design/rutis-0.16-upgrade.md)进行。中继随之移除：各行直接监听公开总线事件。
2. 目录与 Rust 名称绑定。
3. 部件登记处与实例的挂载（D3）。每个插件变成一行并登记自己的 Session 部件和分支部件，每个部件按名称依赖，
   贡献向所有者登记，删除层类型与宿主侧的预注册。旧类型只存在到这一步迁完所有部件为止。
4. 跨语言对齐：Session 作用域和分支作用域在每种语言中获得自己的生命周期服务，Rust 服务通过目录导出（已在 #376 完成）；
   外来插件以插件定义登记部件（#403），rutis 0.17 为 rutis#642 提供了它。
5. D8 中的机制需求（R2 和 R3 已在 #376 完成，R1 在 #380）。
6. 指引与生成的文档随每一步更新。

Dim 尚未发布，所以任何一步都不保留兼容路径：不为仓库外的任何人留别名、垫片或过渡形态。TypeScript SDK、
仓库内的插件、私有的托管 provider 插件和文档，随需要它的那一步一起修改。本设计不改变任何持久化格式或 wire 格式。

## 后果

- 一个服务在各语言之间只有一个标识，因此同一实例中的 Rust 与 TypeScript 部件按名称使用彼此的
  服务。
- Dim 特有的类型更少。插件页面与设计文档不再复述 Rust 写法。
- Capability 一词的含义收窄。把它用于每个 Session 部件的文档在下次改动时修订；原则 §1 现在修订。
- 是否为 Capability 不强制，因此有缺陷的受信任部件仍可能写入另一个所有者的领域，或在任何地方监听。和现在一样，
  由评审、审计与流程测试防范这一点。
- 私有的托管 provider 插件跟进每一次 SDK 变更。
- 兼容 Cordis 指的是 rutis 的 Cordis 4（`@deepseek-ai/cordis`）。Cordis 3（Koishi 一代）的插件不在目标内：
  它们的可选注入和拦截选项不参与门控。
- 在每种语言中，一个功能就是一个插件：一行、一份配置、一次重载。它的进程状态和它登记的部件都在一个模块或包里。
- D3 的机制都是 Cordis 的：登记归调用方所有，和 `ctx.on` 一样；挂上的部件归挂载它的实例所有；按实例的标签隔离。
  组合方式（实例拉取为它的作用域登记的生成器）在 Cordis 和 Koishi 中没有先例，那里的常规做法是一个插件加按条件
  过滤的上下文。
- Rust 功能已随 #402 变成一行，TypeScript 与 Python 功能随 #403。外来部件不能像 Rust 生成器那样跳过某个实例；它的副本照样挂上，只是在那里不做任何工作。
- `docs/architecture/rust-agent/plugins/` 下的 33 个插件页面在第 3 步按插件重新归组。

## 考虑过的替代方案

| 替代方案 | 不采用的原因 |
| --- | --- |
| 保留层类型并加 Rust 语法糖（`Service<T>`，#370/#371） | 在名字之外增加第二个标识；外来部件仍需宿主侧推断位置 |
| 由 loader 强制的 Capability 角色（#377） | 看起来像隔离，实际上不是（平总线、受信任插件）；还要维护一份特权服务清单 |
| 保留按层的跨语言绑定（C4、C5、进程） | 每个都要重新推导位置、生命周期与投递，而且它们已经出现分歧（C4 中没有 Session 发布） |
| 把约定放进 SDK 辅助函数 | 在每种语言中复制一遍框架，违背 rutis 的设计与 D1 |
| 按作用域的事件投递（0.15 设计） | 已被 rutis 0.15.1 撤回 |
| 用组行上的 `isolate`（Cordis 的惯用法）而不是实例 key 实现“每个实例一份”的服务 | rutis 把“每个实例一份”的服务建立在实例 key 上：内核的可见性、组合检查、导出到其他语言以及标记服务的认领都以它为前提，导出也看不到被隔离的原生服务。组的隔离到不了外来行，每个被隔离的 key 都要事先列出（状态单元、命名的服务族和运行时生成的名字都是开放集合），放错位置的行只会一直等待而不是失败。Node 已经把实例名字转换为 Cordis 的隔离 |
| 把 Session 部件和分支部件作为配置行（D3 的最初形式） | 把一个功能拆成多行，作者和每一份外来配置都要逐行放置。插件只是约定，它的进程部件还登记在别处 |
| 只有进程插件，每个插件自己保存按 Session、按分支的状态 | 放弃了实例给部件的东西：实例的生命周期和它自己的服务、发布前的就绪，以及只影响自身的失败 |
| 插件在 Session 创建的钩子里自己把部件挂进实例 | 实例不再拥有自己的子树：就绪和失败不再由它决定，插件还要持有实例的上下文 |
| 运行时往 loader 的实例化组里登记模板 | 要改 loader，又没有额外的好处：实例用 rutis 的 `ctx.plugin` 就能挂载自己的部件 |
