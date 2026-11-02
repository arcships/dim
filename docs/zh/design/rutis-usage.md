# Dim 如何使用 rutis

[English](../../design/rutis-usage.md)

状态：已接受，见 [ADR 006](../adr/006-plugin-contract.md)（D2–D4）。各节说明契约。

插件机制归 rutis。本文说明 Dim 怎样使用它：部件如何装载，如何按名字提供和查找服务，服务如何跨语言，
以及 rutis 的生命周期和事件总线提供什么。它适用于每个部件，不论用哪种语言编写；附录是 Rust 与
TypeScript 的绑定。

本文不讲 Dim 的架构。实例的含义、Timeline 事实、所有权、Capability、发布与关闭，见[插件契约](plugin-contract.md)，
它建立在[时间总线](../../architecture/rust-agent/time-bus.md)与[设计规则](../../architecture/rust-agent/principles.md)之上。
本文的规则不决定任何业务问题，插件契约的规则也不依赖任何 rutis API。

## 1. Dim 从 rutis 取用什么

以下各项 Dim 直接使用，不做包装（[ADR 006 D2](../adr/006-plugin-contract.md#d2-rutis-拥有插件机制dim-在其上定义自己的领域)）：

- 插件生命周期：apply、effect、卸载、取消、期限；
- 依赖门控、Pending 与重载传播；
- 按类型和按名字的服务、实例化组，以及按实例解析名字；
- loader 行与配置；
- 作为 rutis session 远端的语言运行时、跨语言调用、事件总线与诊断。

本地运行时是宿主的一个连接行：它的插件用 rutis-bridge 的公开部件启动运行时进程，并在其上挂载 session
（附录 A.4）。这是 rutis 为宿主自己的连接写明的形式，不是包装。

Dim 不增加插件 trait、面向作者的作用域类型、解析器、manifest、role 字段、第二套服务目录、第二条总线，
也不增加按语言的中继或适配器。

## 2. 插件是一行；实例挂载它的部件

一个插件是一个 rutis 插件，恰好作为进程里的一行 loader 配置装载。它的 Session 部件和分支部件不是行：由插件登记，
由每个实例挂载（[ADR 006 D3](../adr/006-plugin-contract.md#d3-插件是一行每个实例挂载为它登记的部件)）。部件应放在
哪个作用域，由 Dim 决定（[插件契约 §3](plugin-contract.md#3-位置)）。

| 行字段 | 含义 |
| --- | --- |
| `id` | 配置中稳定的行标识 |
| `name` | 模块：内置名称；路径或 npm 名称，在 Node 运行时中运行；或 `<运行时行>:<模块>`，例如 Python 运行时的 `py:<module>` |
| `config` | 结构性配置，由插件的 schema 校验 |
| `inject` | 插件依赖的名称。外来插件自身的 `inject` 必须等于这个列表，或是它的子集 |
| `isolate`、`disabled` | rutis 隔离标签；该行是否关闭 |

**登记部件。** 部件登记处是一个进程服务。插件在自己的 `apply` 里为 `session` 或 `branch` 登记生成器。登记是调用方的
一个 effect，和 Cordis 把 `ctx.on` 绑定到调用方一样：插件的这一代运行多久，登记就保持多久；这一代无论怎样结束，
登记都会撤销。生成器的依赖在实例挂载它时按它的作用域检查，所以点名了作用域之外的组的部件在那时失败。

**插件在哪里运行。** 一个宿主只有一个 loader，它的 Agent 就建在这个 loader 里（`AgentBuilder::build(loader, anchor)`）。
App Server 把插件作为它 loader 里的进程行放置，并以进程名字 `partRegistry` 把登记处交给它们。测试、示例和 `xtask`
也是宿主，和 App Server 一样：最小宿主（`dim_agent_runtime::testing::MinimalHost`）用同样的方式装配一个根和它的
loader，把加到 Agent builder 上的插件（`AgentBuilder::plugin`）放成进程行。每个最小宿主有自己的登记处，所以同一个
builder 在两个宿主里建出的两个 Agent 不会挂上彼此的部件。

**挂载。** 装配在 Session 或分支的分组里只放一行：它的作用域。宿主可以往分组配置里追加行，这些行仍是可选的。
作用域提供完本实例的服务后，把实例的两个挂载器挂成自己的子节点，并把这些视图交给实例的创建流程；它自己不等任何一个：
等待只发生在创建里。

- 实例打开时，每个挂载器调用它负责的、为该作用域登记的生成器，用 rutis 的 `ctx.plugin` 把每个结果挂成自己的
  子节点；对这个实例什么都不返回的生成器就不挂。
  - 必需挂载器负责装配 profile 要求的部件。它把这些部件交给实例的创建流程，后者先等挂载器进入 Active，再等这些部件，
    实例这时才发布。
  - 可选挂载器负责其余部件。它把实例打开时挂上的副本交给实例的创建过程，实例像对待可选行一样，等它们加载结束再发布。
    可选部件失败或处于 Pending 时只影响它自己。
- 实例打开期间，挂载器跟随登记处：新登记的就挂上，撤销的就卸下。所有实例都卸载完各自那一份，撤销才算结束。
- 实例关闭时，挂上的部件随它一起关闭，由 rutis 对子节点的所有权保证。

**插件登记什么。**

- Rust 生成器拿到它所挂进的实例的名字（附录 A），返回一个插件。这个插件可以带着插件在 `apply` 里准备好的东西。
- 其他运行时的插件登记的不是函数，而是插件定义（R7）：用该运行时的 `definePlugin` 做出的一个值，可以闭包引用插件
  在 `apply` 里准备好的东西。每个实例用 rutis 的 `Definition::at` 挂载它的一份副本；副本在插件的运行时里运行，
  `inject` 在该实例里解析。登记它的那一代卸载时，rutis 先结束每一份副本，再结束这个定义（`Definition::ended`）；
  定义结束后，部件登记处撤掉这条登记，之后打开的实例不再挂载它。定义不能对某个
  实例什么都不返回：实例的业务配置关掉了这个功能时，副本不做任何工作。
- **外来行还要声明什么。** loader 的服务目录在运行时启动前就定下了。部件在自己的 Session 或分支里提供的名字，必须
  以那个组登记在目录里；否则 rutis 会把它当作整个进程的一个名字发布，另一个实例的副本就会因为重复提供而失败（§3.5）。
  Rust 插件在宿主的 builder 里登记这些名字。外来插件的部件要等它的运行时跑起来才存在，所以它唯一的那一行列出这些
  名字及各自的组，即 `instanceNames`，App Server 在运行时启动前登记它们。这是外来行关于自己部件唯一要声明的东西：
  部件的 `inject`、`provides` 和其余一切都随定义一起交出。

**每个插件和部件：**

- 工厂或模块顶层只读取自己的配置和标识，不做 I/O，也不启动工作；资源在 `apply` 中取得（§4）。
- 行只保存结构性配置。业务配置放在所有者的来源中（[ADR 002 D4](../adr/002-plugin-scope.md#d4-结构性配置与业务配置分离)）。
- 部件可以在自己的子树里创建子节点，比如 McpSession 为每个服务器各建一个。这些是部件自己的事。

*现状：*部件登记处和挂载器已经实现（附录 A），所有 Rust 部件都已按名称依赖（#391、#392、#396–#398、#400）。
- capabilities 各 crate 的每个功能、App Server 的每个 Session 部件，都是一个登记自己部件的插件：
  `Feature`（不持有进程状态的插件）用 `AgentBuilder::feature` 添加，自己写的插件用 `AgentBuilder::plugin` 添加。
  安装函数就是 profile，它按名字要求自己安装的部件（`require_feature`、`require_part`）。App Server 把 builder
  的插件放成进程行，并以 `partRegistry` 提供 `AgentBuilder::parts()`（`AgentBuilder::plugin_rows`）；最小宿主也这样
  做，用它自己的登记处。必需部件尚未有人登记时，实例的创建像等必需行一样等它。
  持有进程状态的功能从持有它的那一行登记部件：Mcp、Collaboration、Agents、SkillsCatalog，以及持有文件写协调器的
  WorkspaceTools。ConfiguredModels 与 ConfiguredModel、PermissionRules 与 CommandRules 仍是两个插件：由 profile
  选择哪个部件为 Session 绑定 `PrepareModelHost` 或 `CommandRulesHost`，与进程行是否运行无关。最小宿主在启动时
  启动它的插件。
- 工具、工具来源和提示词来源已经向 ToolRegistry 和 PromptRegistry 登记，Session 方法和管理方法的命名空间向 Session 的 `sessionMethods` 和进程的 `managementMethods` 登记，Rpc 每次调用时读取它们（R6，[#382](https://github.com/arcships/dim/issues/382)）。
- `partRegistry` 可以跨语言调用：TypeScript 或 Python 插件是进程里的一行，用 `registerDefinition` 登记自己的部件（附录 B，[#403](https://github.com/arcships/dim/issues/403)）；声明了组的行会被 App Server 拒绝。

## 3. 按名字的服务

### 3.1 标识

- 服务是恰好属于一个组的 camelCase 名称。
- 交给同一个所有者的一组贡献（工具、工具来源、提示词来源、Session 方法和管理方法的命名空间）不是一组名字。
  每个贡献向它的所有者登记，由所有者保管这张表（R6）。
- 对行的每个副本，名称解析到该名称所属组的最近外层实例。它从不回退到另一个组。一行若指向它
  不在其内的组，在组合时失败。
- 每个实例中每个名称只有一个提供者。同一实例中同名的第二个提供者会失败。
- 各语言的部件都依赖名称。Rust 类型 key 是 Rust 绑定存储名称的方式，不是第二个标识。

### 3.2 形状

| 要素 | 形式 |
| --- | --- |
| 方法 | 名称，`sync` 或 `async`。其他语言的调用方总是得到 Promise |
| 请求与回复 | serde 数据，以字段表或 JSON 结构描述 |
| 错误 | 数据，`{code, message}`，错误码由服务写明。转发的部件重新抛出它收到的错误，由 wire 边界映射（[插件契约](plugin-contract.md#9-向机制所有者提出的需求) R3） |
| 取消 | 一个协议动作：请求中的取消回调或 promise，或消费方丢弃该调用 |
| 回调 | 按引用，在 rutis 允许的范围内。回调不得在提交锁内被调用 |
| 流 | 回调或事件；rutis 没有流类型 |

### 3.3 跨语言

一个服务提供的每个方法都有 serde 参数和结果时，它就能跨语言。不能跨语言的方法，例如接受借用值、
通道、句柄或提交锁内同步回调的方法，仅限 Rust。该服务仍有名称和组，服务目录把该方法或整个服务
标为仅限 Rust。要让它跨语言，就改变它的形状。不存在手写的适配层。

### 3.4 服务目录

每个名称只声明一次。跨语言的服务在其 Rust 类型上用 `#[rutis::service]` 声明名称。仅限 Rust 的服务，
或由外来部件提供的名称，注册在宿主的 loader 服务目录中。
`xtask graph` 从真实装配写出权威的表，即[生成的服务目录](../../architecture/rust-agent/generated/catalog.md)，
表过期时 `xtask check` 失败（R4）。Dim 定义的每个名称的用途见[插件契约 §4](plugin-contract.md#4-dim-的服务)。

*现状：*所有跨语言的服务都标记了 `#[rutis::service]`，包括 Session 和分支级的服务；rutis 0.17 移除了
构建脚本中的服务清单（`Exports::service_in`），也就是原来的另一种形式。标记过的类型在某个实例中提供时，
会在那里*认领*它的名字，而每次新的认领都运行一次覆盖整个 loader 的 reconcile，这也会重建那些构建被拒绝的
副本：在某个 Session 的装配中失败的必需行可能在该 Session 创建期间被重建，每次创建 Session 或分支都会
reconcile 整个 loader。App Server 把这些名字按各自的组共享（`register_shared_instance`），一个测试检查每个
名字都在 Rust 绑定登记它的那个组中共享。`xtask types` 把它们的 TypeScript 声明写到
`npm/branch-services.d.ts`，该文件过期时 `xtask check` 失败。

### 3.5 各运行时如何解析名字

§3.1 的规则归框架（[ADR 006 D4](../adr/006-plugin-contract.md#d4-服务是某个组中的一个名字目录就是契约)）：
loader 为每个副本解析每个名字，各运行时落实结果。部件代码只写名字。

| 运行时 | 部件写的 | 由什么解析出拿到哪一份 |
| --- | --- | --- |
| Node（TypeScript、Cordis 4 插件） | `inject: [name]`、`ctx.use(name)` 或 `ctx.<name>`、`ctx.provide(name, value)` | loader 解析名字（`key_in`），并在 Node 进程中把它隔离在该实例的标签下（Cordis 的 `isolate`） |
| Python 和其他运行时 | `inject`、`use`、`provide` | 同上，通过该运行时的隔离表 |
| Rust | 在 `injects` 中列出名字的 key，在 `apply` 中用 `require_at` 读取；在同一个 key 上提供 `T` | 绑定按名字和副本的实例链从目录取得 key（附录 A） |

- **私有名字。** 部件用自己行上的 `isolate` 把一个服务留给自己或一组部件，运行时行和 Rust 行都一样。目录不共享
  这样的名字。其他语言的 Session 部件或分支部件是定义副本，没有自己的行，因此不带 `isolate`：它的名字就是所在实例的。
- **运行时行。** TypeScript 或 Python 插件的行经它所在运行时的 session（`session-node` 或 `py`，附录 A.4）解析。
  session 不在时，该行等它；session 结束时，它的行重新等待。Dim 不在运行时行上写 `isolate`。
- **session 导出。** 每个 rutis session 都把非平台插件提供的每个进程级共享名字导出给远端，并宣告每个监听名。
  语言运行时和 UI leaf 都同样能拿到这些名字；它们是受信任的本地代码（AGENTS 第 11 条）。
- **Rust 的 key。** 用 Rust 类型登记的名字位于该类型的 key；跨语言的名字位于它的分发器的 key（`host_key`）。
  两者都用组的实例限定。Rust 部件在分发器的 key 上读取任何语言的提供方。导出的 Rust 服务两者都有。
- **提供的名字。** 行没有声明、在运行时才宣告的服务，rutis 以其普通名字在整个进程内发布。在 Session 或分支内，
  只属于该实例的名字要以其组登记，这样每个实例各有一份；部件私有的名字由它的行隔离。否则其他实例也能看到该服务，
  第二个提供它的副本会因 rutis 的重复提供者错误而失败。
- **必需与可选。** 必需的依赖要注入，并对部件门控。可选的依赖在使用时读取，不注入：Cordis 的 `ctx.get(name)`、
  rutis 的 `ctx.get_at(&key)`。由配置选择的依赖，是否包含在副本的名字中由配置决定。
- **Cordis。** 未经修改的 rutis Cordis 4（`@deepseek-ai/cordis`）插件作为 Node 行运行，得到同样的解析。
  Cordis 3 插件不在目标内。

## 4. rutis 提供的生命周期

| 阶段 | 部件可以 | rutis 做什么 |
| --- | --- | --- |
| 构建 | 读取自己的配置和标识 | 构造插件；不做 I/O |
| `apply` | 取得资源、提供服务、注册订阅、登记清理 | 按名称解析依赖并以它们门控，每个代次运行一次 `apply` |
| 卸载 | — | 按相反顺序运行清理，把部件的监听器从总线移除，取消其正在运行的调用，并最多等待到 rutis 的期限 |

一个代次是部件从 `apply` 到卸载的一次装载。某个依赖被替换时，rutis 沿声明的依赖卸载并重新装载其下游。
Dim 在此之上增加发布与关闭两个阶段（[插件契约 §7](plugin-contract.md#7-部件的生命周期)）。

## 5. 事件总线

从 rutis 0.15.1 起，每个根只有一条平总线。

- **投递。** 某个名称的每个监听器都收到该名称的每个事件，无论监听器在哪里注册。监听器看不到发送方的实例。
  事件只到达发出时已经注册的监听器，不存储、不回放。不同名称的事件之间没有共同顺序。`emit` 不等待；
  `parallel` 等待并汇总失败。
- **顺序。** 同一名称的事件按发出顺序启动监听器：上一个事件的每个监听器都已启动后，下一个事件的监听器
  才启动；没有谁等待一个正在 await 的监听器。需要顺序的监听器在第一次 await 之前做完有序的部分，
  或者通过 `dispatcher` 消费事实（[插件契约 §5.2](plugin-contract.md#52-消费事实)）。
- **载荷是 serde。** 事件中不放句柄、通道或锁。
- **名称就是契约。** 属于某个实例的事件，在名称中带上该实例的标识，名称由其所有者导出的构造函数生成，
  因此调用方从不拼接名称。或者所有者改为提供 watch 或服务方法。
- **保留前缀：**`rutis/*`（rutis 的框架事件：只可监听）和 `internal/*`（各运行时自有）。

Dim 的部件如何消费总线事件，以及 Dim 定义了哪些公开总线事件，见[插件契约 §5.3](plugin-contract.md#53-总线事件)。

*现状：*Dim 已在 rutis 0.19.3 上，本节成立。总线跨越每个 rutis session：两端各自宣告自己的监听名，事件到达任一端
该名称的监听器，语言运行时的每次 emit 都经过宿主。

## 6. 向机制所有者提出的需求

这些需求关于 Dim 如何使用 rutis。它们遵循[原则 §4 第 8 条](../../architecture/rust-agent/principles.md)，
所属阶段见[插件契约的迁移](plugin-contract.md#10-迁移)。R1–R3 在[插件契约](plugin-contract.md#9-向机制所有者提出的需求)中。

### R4. 服务目录自动生成，自动标出 Capability

- **受阻：**本文 §3.4 与[插件契约 §8](plugin-contract.md#8-保证约定与审计)。服务目录曾分散在 `build.rs`、作者指南和设计文档中，也没有东西标出哪些部件是 Capability。
- **考虑过的现有机制：**`xtask graph` 已经从真实装配记录注入和提供的服务。
- **所需机制：**`xtask graph` 写出服务目录表（名称、组、提供者、是否跨语言、方法），并根据部件注入的名字
  （`dispatcher`、`timeline`）标出哪些部件是 Capability。
- **影响：**运行时没有影响。
- **所有者：**`xtask`。已交付（[#381](https://github.com/arcships/dim/issues/381)）：`xtask graph` 从 App Server
  产品配置及其中的一个 Session 写出[生成的服务目录](../../architecture/rust-agent/generated/catalog.md)，表过期时 `xtask check` 失败。

### R5. 外来插件登记自己的部件

- **受阻：**§2。一个有 Session 部件和分支部件的外来功能，每个部件都是一行配置，各自携带自己的组。R5 的最初形式
  （外来行声明自己的组，不再由 App Server 从 `inject` 推断）已在阶段 4 交付（[#376](https://github.com/arcships/dim/issues/376)），
  但插件仍然不是一行。
- **考虑过的现有机制：**运行时解析器已经能为一个模块所在的实例构造副本（`Resolved::scoped`），服务目录拒绝放错位置的名称。
- **所需机制：**外来插件是进程里的一行，在自己的代码里以插件定义（R7）向部件登记处登记部件。其他语言的
  Session 行和分支行，以及它们声明的组，都删除。不做包声明：定义自带 `inject` 和 `provides`，而 R6 已经去掉了宿主
  在运行时启动前知道名字的大部分需要。剩下的，即部件在自己实例里提供的名字，留在插件的那一行里（§2）。
- **影响：**对 Timeline 没有影响。
- **所有者：**App Server 和各 SDK，仓库内的插件一起改（[#403](https://github.com/arcships/dim/issues/403)）。已交付。

### R6. 贡献向所有者登记

- **R6 之前：**[ADR 002 D5](../adr/002-plugin-scope.md#d5-合同语言中立)
  （“注册：由插件声明……而非宿主侧的 `AgentBuilder` 调用”）曾受阻：宿主预先注册贡献名称
  （`session_tool`、`session_methods`、`session_prompt_source`、`session_tool_source`、`host_tools`），
  并把外来行的 `provides` 翻译成同样的调用。原生工具贡献和外来工具贡献有不同的 key，因此 ToolRegistry
  两者都读，并通过以同一字符串找到的 sidecar 令牌检测正在退役的外来代次（`tool_registry.rs:486-509`）；
  Rpc 对方法命名空间也有同样的拆分。提示词来源和工具来源不对外来部件开放。下面“所需机制”和“所有者”两条
  说明了取代它的做法。
- **考虑过的现有机制：**
  - 在实例中按名字查找贡献，靠 `rutis/service` 事件或目录列表。不采用：所有者要从框架内部（哪些行提供了哪些名字）
    得知贡献方在调用时本来就知道的事。
  - 向所有者登记已经存在：部件登记处（§2）和 `sessionLifecycle.hold`，后者由 bridge 绑定到发起调用的行
    （`caller_row`）。经调用方的 `ctx.effect` 做的登记在调用方卸载时撤下；bridge 能把其他语言的函数作为可调用的
    引用传过来（`RemoteFunction`）。
- **所需机制：**每个所有者为它的贡献提供 `register`：ToolRegistry 管工具和工具来源，PromptRegistry 管提示词来源，
  Rpc 管 Session 方法和管理方法的命名空间。工具和方法命名空间可以从任何语言登记；提示词来源和工具来源在有外来部件
  需要之前仍只限 Rust。
  - 登记属于调用方。在 Rust 里调用方传入自己的 ctx；从其他语言调用时，bridge 把登记绑定到发起调用的行，处理函数
    是调用方自己的函数，经 bridge 回调。调用方卸载时登记撤下。
  - 同一个名字向同一个所有者登记第二次会失败，与一个名字有第二个提供者时一样。
  - 撤下的处理函数不再被调用：经撤下的登记发出的调用被拒绝并说明没有执行，撤下时正在进行的调用报告它可能已经生效，
    与现在一样，只是不再需要令牌。
  - 没有挂载列表（`SessionConfig.tools`）的 Session 声明全部已登记的工具，因此插件的工具不需要修改配置
    （[#364](https://github.com/arcships/dim/issues/364)）；有挂载列表的 Session 声明其中列出的工具。创建 Session
    时不拒绝列出但没有任何部件登记的工具；ToolRegistry 在读取工具时报告它。
  - 预先注册、`toolContribution:*`、`sessionMethods:*` 和 `managementMethods:*` 这些名字、sidecar 令牌，以及
    App Server 对外来行 `provides` 的翻译都去掉。ExternalAgent 设计中的 `collaborationMembers:<kind>` 在实现时
    也同样向 Collaboration 登记。
- **影响：**对 Timeline 没有影响。工具采样保持 ToolBinding 现有的逐 turn 规则。
- **所有者：**Dim 运行时（ToolRegistry、PromptRegistry）和 App Server（Rpc），[#382](https://github.com/arcships/dim/issues/382)。
  已实现：工具、工具来源和提示词来源向 ToolRegistry 和 PromptRegistry 登记，方法命名空间向 Rpc 每次调用时读取的两张
  方法表登记，即 Session 服务 `sessionMethods` 和进程服务 `managementMethods`。每张表在命名空间登记时检查它，并记住
  已撤下命名空间的行，因此调用按该行的状态作答（[方法合同 A.1](contracts/session-boundary.md#a1-方法贡献)）。

### R7. 每个运行时的部件都是插件定义

- **受阻：**§2。外来插件只能交给宿主模块名和配置，因为宿主只能按名字加载外来插件。按插件自身状态准备的部件，必须经过
  共享模块或服务，Rust 部件则不需要。
- **考虑过的现有机制：**模块名和配置覆盖了现有的每个部件；它是过渡形式，现在去掉。
- **所需机制：**运行中的行把一个部件连同它的声明交给宿主，宿主在指定实例里挂载它的副本。每一份副本在生成者的运行时里
  运行，按实例的标签隔离，按自己的 `inject` 门控；它属于挂载它的宿主 fiber；生成者的这一代卸载时结束。
- **影响：**对 Timeline 没有影响。
- **所有者：**rutis，[arcships/rutis#642](https://github.com/arcships/rutis/issues/642)。已以插件定义的形式交付：
  Node 在 rutis 0.17.0，Python 在 0.17.1。类型为 `rutis::Definition` 的 `#[rutis::service]` 参数接收其他语言交来的
  定义；`Definition::at` 为 `ctx.plugin` 构造一份副本；定义属于交出它的行，它的副本在该行自己的清理之前结束。定义是
  一个值，不是生成器函数，所以 Rust 生成器靠返回空值做的按实例选择，移到副本里做（§2）。
  [#405](https://github.com/arcships/dim/issues/405) 记录这次升级。

## 附录 A. Rust 绑定

### A.1 旧形式及其替代

P3 删除了旧形式（[#375](https://github.com/arcships/dim/issues/375)）；此表列出每个旧形式的替代。

| 旧形式 | 替代 |
| --- | --- |
| `impl Capability for P { fn keys(&self) }` | 没有 Dim trait。部件是 rutis `Plugin`；依赖名字时是 `host::DynPlugin` |
| 传给工厂的 `SessionScope`、`BranchScope` | 工厂需要实例时用 `Names::instance(group)`；标识来自 `sessionIdentity` 或 `timelineIdentity` |
| `InSession<T>`、`InBranch<T>` | 部件保存的 key `names.key::<T>()`，列在 `injects` 中并用 `require_at` 读取：对 `T` 的名字的依赖，key 用这个副本实例链中该名字所属组的实例限定（§3.5） |
| `SessionGate<T>`、`BranchGate<T>` | 该名字的 key 列在 `injects` 中，但不读取 |
| 提供服务时的 `scope.key::<T>()` | `names.key::<T>()`，与该名字的消费方拿到的 key 相同 |
| `Contributions<T>` | 无：贡献向所有者登记（R6） |
| `When<D>` | 工厂构建副本时填或不填的 `Option<Key<T>>`；有值时列在 `injects` 中并读取 |
| `SessionService`、`BranchService` 标记 | 注册在服务目录中的组（`names::register`） |
| `AgentBuilder::session_plugin`/`timeline_plugin` | 功能向部件登记处登记自己的部件（`Feature` 用 `AgentBuilder::feature` 添加，自己写的插件用 `AgentBuilder::plugin` 添加；profile 用 `require_feature`）；App Server 的进程行用 `register_part` |
| `AgentBuilder::session_tool`、`session_methods`、`session_prompt_source`、`session_tool_source`、`host_tools`（已删除） | 所有者的 `register`，由提供贡献的部件调用：`toolRegistry`、`promptRegistry`、`sessionMethods`、`managementMethods`（R6） |
| `build.rs` `Exports::service_in(..)` | 类型上的 `#[rutis::service]`，包括 Session 和分支级服务（§3.4）；源码无法修改的类型包进 Dim 自己的、带标记的类型。`Exports` 只生成 TypeScript 声明，由 `xtask types` 调用 |
| `PublicService` | 不变（门面规则） |

### A.2 绑定

没有名字依赖的 Rust 部件就是普通的 rutis `Plugin`（`type Deps` 按类型）。依赖名字的部件是 rutis
`host::DynPlugin`，也就是 rutis 给出的按 key 依赖的写法：部件保存这个副本的 key（由工厂从 `Names` 取得），
在 `injects` 中列出，在 `apply` 中用 `require_at` 读取（只在存在时才用的依赖用 `get_at`，不列出）。
Dim 不另加 trait 或适配层。列出的 key 在严格读取时发现提供者已消失，会返回它自己的错误，rutis 把部件退回
Pending（A.3）。绑定在 Rust 中落实 §3.5：

- 按 `#[rutis::service]` 的规则给类型命名：`Timeline` 是 `timeline`，`dyn LoopState` 是 `loopState`
  （`names::name_of`）。一个 Rust 类型就是一个名字，所以在两个组提供的类型要拆成两个类型，或归到一个组。
- key 按名字和副本的实例链从服务目录取得。`names.key::<T>()` 用于以 Rust 类型 `T` 登记或由它导出的名字：
  在该名字所属组的实例中是 `Key::<T>::of().instance(id)`，进程级是 `Key::<T>::of()`。
  `names.named::<T>(name)` 在目录的 key 上读取任意名字；对跨语言的名字，`T` 是 `dyn HostDispatch`，
  任何语言的提供方都在那里。提供方在它的消费方拿到的 key 上提供。
- profile 部件的名称在组合装配时按它的作用域检查：工厂向 `Names` 取 key，部件依赖的名称属于某个组而它所在的实例不在该组内，就在此时
  失败，并指出该组；依赖未登记的服务也同样失败。登记的部件在实例挂载它时做同样的检查。

服务目录是 builder 的（`AgentBuilder::catalog`），App Server 的 loader 用的是同一个。
`names::register::<T>(catalog, group)` 声明一个 Rust 名称：运行时登记自己的名称，每个 crate 登记它提供的
服务的名称；同一名称在同一组再次登记不产生任何效果。导出的名称经生成的分发器提供给其他语言，一个测试
检查每个导出的名字和组与绑定按其类型推出的相同。

```rust
pub struct WaitBranch {
    dispatcher: Key<Dispatcher>,
    wait: Key<Wait>,
}
impl WaitBranch {
    fn new(names: &Names) -> rutis::Result<Self> {
        // "dispatcher" is a branch name; nothing here says so.
        Ok(Self { dispatcher: names.key()?, wait: names.key()? })
    }
}
impl DynPlugin for WaitBranch {
    fn name(&self) -> &str { "WaitBranch" }
    fn injects(&self) -> Vec<TypeKey> { vec![self.dispatcher.erase()] }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            /* dispatcher.follow(ctx, "Wait", keys, handler).await?, the Rust form of `consume`
               (plugin contract §5.2), then ctx.provide_at(self.wait.clone(), wait) */
            Ok(Effect::Done)
        })
    }
}

// A plugin that owns nothing for the process: it registers a part of every branch, and the profile
// requires it. `names` gives each copy's keys, for its dependencies and for `wait`.
builder.require_feature(
    Feature::new("Wait").branch("WaitBranch", |names| Ok(Box::new(WaitBranch::new(names)?))),
);
// A process row of the App Server's loader.
register_part(&mut builtins, builder.catalog(), |names| Ok(Box::new(Recorder::new(names)?)))?;

// From a process plugin's `apply`: a Session part of every Session that mounts `record` (one
// without a mount list mounts every tool), withdrawn when the plugin unloads. `builder.parts()` is the registry; `builder.require_part(name)` makes
// such a part required.
registry.register(ctx, PartScope::Session, "Recorder", |mount| {
    let mounted = mount.session.tools.as_ref();
    if !mounted.is_none_or(|tools| tools.iter().any(|tool| tool == "record")) {
        return Ok(None);
    }
    Ok(Some(Box::new(Recorder::new(&mount.names)?)))
})?;
```

必需挂载器挂载 profile 要求的部件（`AgentBuilder::require_part`）；创建先等挂载器、再等这些部件，实例这时才发布。
必需部件尚未有人登记时，创建在自己的时限内等它登记。builder 的登记处（`AgentBuilder::parts`）服务于宿主建出的
Agent（`AgentBuilder::build`），宿主以 `partRegistry` 把它提供给自己的进程插件（`AgentBuilder::plugin_rows`）；
最小宿主建一个自己的登记处。登记是调用方 `ctx` 的一个 effect。
挂载器在 rutis 自己的级联清理之前先卸下自己挂的副本，所以已经失败的副本在实例关闭时不会再报一次错；
`SessionLoader` 从所在实例的 Session scope 行创建实例，因为 loader 只能从行的上下文放置实例。

由配置启用的依赖是工厂只在启用时才填的 `Option<Key<T>>`：
`storage: enabled.then(|| names.key::<FileStorage>()).transpose()?`；有值时列在 `injects` 中，`apply` 也只在此时读取。

宿主为进程名称配置的值（例如脚本化的 `ModelHost`）用 `AgentBuilder::process_value`。Agent 组行
（`AgentBuilder::agent_row`）包含它的提供方，并按 Agent 实例隔离该名称，因此共享宿主的其他 Agent、宿主自己
对该名称的提供方都不会与它冲突。宿主把这一行放进自己的 loader，该 loader 的服务目录是 builder 的。`session_value` 保留给 Session 名称。

### A.3 已定的问题

来自 [#6](https://github.com/arcships/dim/issues/6)：

- **依赖消失。** 某个名称的提供者消失时，rutis 把依赖它的部件退回 Pending，提供者回来后再次加载。新的一代
  按能力重载协议从 Timeline 重新开始（[插件契约 §7.2](plugin-contract.md#72-代次重载与重建)），不再增加任何东西。
- **可选依赖与由配置选择的依赖。** 同 §3.5：由配置选择的依赖在被选中时门控（工厂填的 `Option<Key<T>>`），
  组合检查此时也覆盖它；只在存在时才用的依赖用 `get_at` 读取，不列在 `injects` 中。它的名称仍须登记。
- **启动进展。** `startup_timeout` 限制 loader 启动、reconcile、必需行就绪和打开重放期间连续没有进展的时间。本次打开中一行的首次激活、分支派发序号前进或一份打开回执完成会重置间隔；同一行反复重启不算新的进展。
  只计本次打开范围内的进展，其他 Session 的活动不算；不设固定总时限。
  Session、分支、进程启动和 restart 使用同一规则。分支等待父分支时跟随父分支自身受限的打开过程，
  不另加总时限。没有进展时列出未 Active 的行及缺少的依赖并失败；组合错误或必需提供者失败立即返回。
  回滚取消各行，rutis 从不中止 `apply`，而是等待它在 `ctx.cancelled()` 完成后返回。

### A.4 运行时与 session

App Server 的根提供 `rutis_bridge::LocalId("dim-daemon")`，它与每个 session 握手时都用这个 id。它不挂载
`Session::service()`，因为那个服务的名字 `sessions` 是 Dim 的 `AgentDirectory`。

每个本地运行时是一个进程级连接行，Node 是 `session-node`，Python 是 `py`。它的插件 `RuntimeConnection`
（`dim-app-server/src/session_typescript.rs`）：

- 注入一个携带启动命令的仅限 Rust 的进程服务（`nodeRuntime`、`pythonRuntime`），所以命令提供之后该行才启动。
  配置了运行时路径时由 App Server 在启动时提供；否则由 RuntimeManager 在取得运行时之后提供
  （[RuntimeManager](plugins/RuntimeManager.md)）；再次提供会重启该行；
- 用 `rutis_bridge::runtime::Launcher`（`--id <row>`）启动进程，记在自己的 `pipe::Processes` 中，等它的问候
  （`pipe::greeted`），再挂载 `Session::new(channel)`。卸载时杀掉尚未结束的进程并等它结束。标准错误逐行写到 `tracing`；
- 用 Dim 自己的值 `RuntimeStatus`（`starting`、`ready`，或带原因的 `down`）在 `RuntimeStatus::key(row)` 记录运行时的
  状态。运行时启动失败或进程结束时，该行仍是 Active，状态因此仍可读取；运行时的各行重新等待。

一个 `rutis_loader::runtime::SessionResolver::npm(catalog, "session-node")` 放在解析链末尾，解析两个运行时的行：
`py:<module>` 在 `py` 的 session 上，其他外来名字在 `session-node` 上，由 Node 运行时解析 npm 名称。它在 loader
挂载之前接到 loader 上（`attach`），loader 运行后跟随每个运行时行（`follow_row`）。Dim 的 `SnapshotResolver`
包住它，只做 rutis 不做的事：按 `provides` 快照检查每一行，并把各自的包交给使用 `workflowFiles` 的行。配置了运行时
路径时，准入等待每个运行时的行（`SessionRows::key(row)`）；RuntimeManager 取得的运行时是可选的，所以启动从不等下载。
`plugin.reload` 就是 `Loader::reload(row)`；没有解析器缓存。

rutis-loader 的 `LoaderPlugin` 还提供带标记的服务 `rutis/loader`。注入它的外来行可以 `reload` 一行、读取 `status`；
`create` 和 `remove` 会被拒绝，因为 Dim 不配置可编辑的层，所以 PluginManager 仍是行的唯一写入者。

## 附录 B. TypeScript 与 Python 绑定

部件使用 `definePlugin`（Python 中为 `define_plugin`，或一个带 `inject`、`provides` 和 `apply` 的模块），
带 `inject` 和 `provides`。

插件不论用哪种语言写，都是进程里的一行。它的 `apply` 用 `partRegistry.registerDefinition(scope, name, definition)`
登记每个 Session 部件和分支部件；`definition` 是一个 `definePlugin` 值（Python 中为 `define_plugin` 值；Node 中也可以是
未经修改的 Cordis 4 插件），可以闭包引用插件准备好的东西。它的行注入 `partRegistry`，并在 `instanceNames` 里列出部件在每个 Session 或分支里提供的名字（§2）。SDK 生成的
声明来自服务目录。

```json
{ "id": "goal", "name": "/abs/goal/index.ts", "inject": ["partRegistry"], "provides": {},
  "config": {} }
```

```ts
export default definePlugin({
  name: 'Goal',
  inject: ['partRegistry'],
  async apply(ctx) {
    const parts = ctx.use<Services['partRegistry']>('partRegistry')
    await parts.registerDefinition('session', 'GoalSession', session)
    await parts.registerDefinition('branch', 'GoalBranch', branch)
  },
})
```

Python 行的名字以运行时的行为前缀写出模块：`py:<module>`；配置了 Python 运行时时，App Server 在其中运行该行，从配置的
项目目录导入该模块。Python 运行时需要 Python 3.12 或更高版本。

```json
{ "id": "python", "name": "py:python_plugin", "provides": {}, "config": {} }
```

```python
from rutis import define_plugin

async def apply_branch(ctx, config):
    dispatcher = ctx.use("dispatcher")
    identity = await ctx.use("timelineIdentity").get()
    ...

branch = define_plugin(apply_branch, inject=["dispatcher", "timelineIdentity", "treeCommands"])

inject = ["partRegistry"]
provides = {}

async def apply(ctx, config):
    await ctx.use("partRegistry").registerDefinition("branch", "PythonBranchPart", branch)
```

```ts
// A Capability that rebuilds from history, then follows live events (plugin contract §5.2).
export default definePlugin({
  name: 'ExampleCounter',
  inject: ['dispatcher'],
  provides: { exampleCounter: { get: 'sync' } },
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    let count = 0
    const fold = (event: BranchEvent) => {
      if (event.type === 'input' && event.name === 'message') count++
    }
    await dispatcher.consume('example-counter', ['input/message'], delivery => {
      if ('live' in delivery) fold(delivery.live)
      else delivery.replay.forEach(fold)
    })
    ctx.provide('exampleCounter', { get: () => ({ count }) })
  },
})
```

手写形式仍然有效：先 `listen`，再取 `committed()`，在处理函数等待期间从 `local` 重建到该截止点，
并跳过重建已覆盖的实时事件。

仓库的 fixture 展示了两种语言和两种形式：`runtime/fixtures/branch-part.ts` 手写重建，
`runtime/fixtures/python/python_branch_part.py` 与 `runtime/plugins/goal/branch.ts` 使用 `consume`。
