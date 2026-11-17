# DSH 包

状态：草案（[#1753](https://github.com/arcships/dim/issues/1753)）。规则：[设计规则](../../../architecture/rust-agent/principles.md) 第 1、4 节、[ADR 002](../../adr/002-plugin-scope.md)、[插件契约](../plugin-contract.md)（[ADR 006](../../adr/006-plugin-contract.md)）。作用域：进程与 Session。语言：Dim 侧 Rust，桥接层 TypeScript。English: [English](../../../design/plugins/DshPackages.md)。

DSH 是本设计要运行的插件包所属的 harness。DSH 包是一个 Cordis 4 插件包，Dim 已经能把它作为一个 Node loader 行装载（[Dim 如何使用 rutis](../rutis-usage.md)）。它缺的不是装载，而是它编程所对的宿主 API：工具、系统提示词段落、斜杠命令、子 Session、本地管理动作，以及它要响应的 Session 生命周期通知。本设计只补齐这套宿主 API，其余一概不加，因此一个为 DSH 发布的包在只使用 [§2.2](#22-宿主-api) 所列宿主 API 时，可以在 Dim 中原样运行，自己的文件、依赖与持久化都保持原样。

首期由两个真实包界定范围：`dsh-mysql` 注册四个工具、贡献一段会静默变化的表结构提示上下文、向同进程伙伴包导出一个 `mysql` 服务、响应 Session 生命周期，并暴露七个本地连接动作；`dsh-side-session` 注册一个斜杠命令，并创建一个带父 Session、受父策略封顶、可回滚取消的子 Session。另外三个包 `dsh-memory-recall`、`dsh-trellis`、`dsh-auto-retry` 在装载时失败并指明缺失的契约（[§11](#11-本设计之外)）。

**包仍然是一个进程行。** 没有包装行、没有第二套业务逻辑、没有针对某个包的 Dim 代码：每个包一个受管入口、随运行时发布的一份桥接库，每个贡献都由包自己的行登记到它的 Dim 所有者。

## 1. 结构

```mermaid
flowchart LR
  subgraph DIM["Dim 进程"]
    PM["PluginManager：包、受管入口、行"]
    PR["partRegistry"]
    MM["managementMethods"]
    AG["Agents：sessions"]
    subgraph SESS["一个 Session"]
      TR["toolRegistry"]
      PSR["提示词来源"]
      PC["pluginCommands"]
      LC["sessionIdentity、sessionConfig、sessionLifecycle"]
    end
  end
  subgraph NODE["session-node"]
    HOOK["解析钩子：单一 Cordis 副本"]
    ROW["包行"]
    BR["桥接：宿主 API、账本、门面"]
    COPY["Session 副本，每个打开的 Session 一份"]
  end
  PM -->|"写入 entry.mjs、装载行"| ROW
  HOOK --> ROW
  ROW --> BR
  BR -->|"registerDefinition"| PR
  PR -->|"挂载一份副本"| COPY
  COPY -->|"按所有者登记的租约"| TR
  COPY -->|"按所有者登记的租约"| PSR
  COPY -->|"按所有者登记的租约"| PC
  COPY -->|"按所有者登记的租约"| LC
  BR -->|"dsh_typert 命名空间"| MM
  BR -->|"launch、input、delete"| AG
```

| 部件 | 作用域 | 持有 |
| --- | --- | --- |
| 包行 | 进程，每包一个 | 包自己的模块状态、它的伙伴服务，以及经桥接维护的贡献期望态 |
| 桥接 | 在该行内部，不是名字也不是行 | 它交给包的宿主 API 对象、贡献账本、Agent 门面、一份 Session 副本定义 |
| Session 副本 | Session，每个打开的 Session 一份 | 副本自己的 Cordis fiber，以及它调用各所有者所用的服务租约 |
| 受管入口 | 进程，每包一个文件 | 运行期不持有任何东西：它指明包的真实入口并交给桥接 |

**不是接线模块。** 桥接是包自己那一行导入的库：它不向进程提供名字，也不适配别的插件的接口。它把包自己的贡献登记到这些贡献所属的所有者，这正是任何贡献部件都要做的事（[ADR 002 D2](../../adr/002-plugin-scope.md#d2-不设接线模块)），且它不持有进程状态。

**Agent 不是 Dim 的 Session。** Dim 的 Session、它的内存实例与分支是 Rust 核心状态。包所称的 agent 是桥接在 `agent/created` 中交给它的对象（[§2.4](#24-agent-门面)），每个已挂载的 Session 一个。包的模型中没有任何东西是分支：包不贡献分支部件，在一个 Session 上开十条分支也不会多挂副本。

**运行时只解析一份 Cordis。** DSH 包在模块顶层 import 它的 harness 所用的 Cordis 包，而一个进程里两份 Cordis 无法共享上下文。Node 运行时以解析钩子启动（`node --import`，运行时锁定的 Node 24 支持）：把所有包的 Cordis import 指向运行时已有的那一份，并从桥接自己的依赖解析 harness 的纯辅助包（工具定义、schema 辅助）。启动命令归 RuntimeManager，因此该钩子作为需求提给它（[§10.9](#109-runtimemanagernode-启动携带桥接的解析钩子)）。运行时那一份是 Cordis 4.0.4（`runtime/package-lock.json`）；若包的 `peerDependencies` 对 Cordis 的区间不被该副本满足，装载时以 `cordis_range` 拒绝（[附录 B](#附录-b-错误与恢复)）。自己解析出另一份 Cordis 的包同样被拒，运行时已会对「入口解析到不同副本」的情况报错。

## 2. 入口与桥接

### 2.1 受管入口

DSH 包不是 Dim 插件：它不带 `dim-plugin.json`，其 `package.json` 是写给它自己的 harness 的。两个首期包唯一相关差别在于：数据库包以可选方式声明对 Cordis `~4.0.4` 的 peer 依赖，旁会话包完全不声明 Cordis 依赖；两者都声明了 `dsh` 成员，其 `bundle.patch` 指向 harness 自己的 bundle patch。标记一个包的是 `dsh` 成员，而那个 bundle patch 不被读取：本设计的受管入口取代它。

识别这样的包并写出它的入口归 PluginManager（[§10.8](#108-pluginmanager识别-dsh-包并写受管入口)）。本设计固定入口的位置与内容：位于插件文件根下 `.dsh/<插件 id>/entry.mjs`，三行，从运行时的单一副本导入桥接，再导出 `dshPackageRow(<包的真实入口>)`。包目录全程只读，包括从本地路径安装的情况，所以升级 Dim 只换桥接，不动任何包。

入口的模块 URL 携带包入口的修改时间与大小，这是运行时已有的行模块规则，因此重载该行时导入的是变化后的入口，而不是缓存中的模块（[PluginManager §3.4](PluginManager.md#34-重新加载)：宿主不监听文件，重载调用才是信号）。入口属于 PluginManager 对该插件的记录，每次解析行时重写；它不持有状态，除运行时外无人读取。

### 2.2 宿主 API

桥接在该行自己的隔离作用域内提供下列对象，两个包互相看不到。每一项就是包眼中的整个 Dim。

| API | 包用它做什么 | Dim 收到什么 |
| --- | --- | --- |
| `tools.register(definition, execute, options?)` | 用 schema、渲染与并发类声明一个工具 | 在 Session 的 `toolRegistry` 登记一个工具，处理器是包的函数 |
| `systemPrompt.section`、`.context`、`.variable`、`.suppressRuntimeContext` | 贡献系统文字、每次请求的状态块、插值变量 | 每个 Session 一个提示词来源，其取值由分支采纳（[§3.5](#35-请求准入顺序)） |
| `commands.register(name, handler)` | 登记带描述与参数提示的斜杠命令 | Session 的 `pluginCommands` 中的一项（[§6](#6-终端-ui)） |
| `subagents.startContinuable`、`.startActivation` | 创建带父 Session、策略与首条消息的子 Session | 带父的 `sessions.launch`，随后 `sessions.input`，再返回子 Session 的 id |
| `./typert` 动作（读自包的清单） | 把本地配置动作暴露给客户端 | 以包自己的命名空间登记到 `managementMethods` |
| `ctx.on('agent/created' \| 'agent/disposed')`、`agent.getHistory` 作用域、`agent.ctx` | 每个 Session 的建立与拆除 | 桥接自己的门面事件（[§2.4](#24-agent-门面)） |
| `ctx.provide(name, service)` | 向伙伴包提供服务 | Dim 不参与：它留在共享的 Node 进程里，由运行时解析 |

包注入的、本表未覆盖的任何名字，以及 harness 有而本设计不提供的任何拦截点（步前钩子、工具执行前守卫、请求错误处理器、HTTP 路由、同步全量历史读取），都会让该行在装载时失败并写明缺失项（[§3.4](#34-失败)）。包永远不会在宿主 API 缺一块的状态下被装载。

### 2.3 贡献账本

包的注册 API 是同步的（登记工具立即返回 disposer），而每个 Dim 所有者都要跨进程调用。桥接为每个包行维护一份账本填补这个差：每个贡献一项，带本地代数；每个 Session 副本一个串行协调器，把各所有者的表对齐到该期望态。

| 情况 | 协调器做什么 |
| --- | --- |
| 已登记、未发出、已撤回 | 什么都不发，账本项删除 |
| 已登记、在途、已撤回 | 等返回登记 id 的回复，再用该 id 撤回，不留孤儿项 |
| Session 正在关闭时撤回 | 什么都不发；副本卸载时一次撤回它登记过的全部项 |
| 同名重复登记 | 在包的调用中同步拒绝，与 harness 一致 |
| 某个登记失败 | 记入该 Session 副本的诊断，其余贡献照常 |

账本每行一份，绝不跨包共享。它是桥接为贡献保留的唯一状态，存在的原因是包的 API 不能等待、而所有者的表是异步的。

### 2.4 Agent 门面

包在 `agent/created` 中拿到的对象，是它作为 map 键使用的身份，也是它保存每个 Session 私有状态的载体。因此它在进程存活期内每个 Session 恒为同一对象，且它是一个薄壳：持有 Session id 和一个指向当前 Session 副本服务句柄的槽位。空闲卸载清空槽位，恢复时填入新副本的句柄。

```text
agent/created             session.close / Session 删除          daemon 重启
  agent = facade(id)        agent/disposed(agent)               首次装载该 Session
  包保存该对象              门面身份不变                           agent/created 一次，
  空闲卸载：槽位置空                                              Session 为恢复时
  恢复：    槽位重绑                                              source: 'resume'
```

桥接能发出这两类通知，只因为 Session 作用域告诉了它自己为何结束。无论业务关闭还是空闲卸载，Session 作用域都会结束（[插件契约 §2](../plugin-contract.md#2-实例树)；运行时对两者取消同一个 closing 令牌，`crates/dim-agent-runtime/src/agent.rs:804-808`，`finish(unloading)`）。而 `sessionLifecycle` 只提供 `activated`、`closing`、`hold` 与 `release`，没有任何一项能区分二者（[插件契约 §4](../plugin-contract.md#4-dim-的服务)，`crates/dim-agent-runtime/src/config.rs:403-408`）。在 `closing` 时发 `agent/disposed` 的副本，实现的正是本设计拒绝的空闲卸载通知。创建还是恢复这一半已经是数据：`sessionIdentity.get()` 给出 `{sessionId, restored}`（`crates/dim-agent-runtime/src/config.rs:234-249`），而 `restored` 恰为 `SessionInitialization::Restore`（`crates/dim-agent-runtime/src/assembly.rs:1749`，在 `crates/dim-agent-runtime/src/agent.rs:169-183` 选定）。业务删除则只在进程根上发 `agent/session-deleted`，那是宿主内部总线事件，不是行可读的服务面（[插件契约 §5.3](../plugin-contract.md#53-总线事件)）。

因此这两类通知依赖 Session 所有者的一项需求（[§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始)）：副本能得知自己作用域结束的原因（业务关闭、删除还是空闲卸载），以及本次装载是创建还是恢复。有了它，`agent/disposed` 跟随业务关闭与删除，绝不跟随空闲卸载；`agent/created` 跟随本进程内的首次挂载，`source` 指明是否为恢复。

门面身份永不变化，包以 agent 为键的 `WeakMap`、`WeakSet` 跨空闲卸载仍然正确，`session.close` 时的 `agent/disposed` 也命中了包当初登记状态的那个对象。槽位重绑前包通过旧句柄发起的调用会以服务不可用失败，不会打到另一个 Session 的副本。

### 2.5 登记状态

某个包的一个贡献，从包的调用到它在 Dim 中的生效。状态按 Session 区分：同一贡献在某个 Session 已发布、在另一个 Session 被拒，都是正常的。

| 状态 | 所有者的表 | 模型看到什么 | 何时结束 |
| --- | --- | --- | --- |
| 期望 | 不变 | 无 | 协调器取走，或 disposer 先执行 |
| 登记中 | 不变 | 无 | 所有者回复，或副本卸载 |
| 已登记 | 表中有该项 | 工具尚无；提示词取值在下一次采纳时 | 撤回，或副本卸载 |
| 已采纳 | 不变 | 工具进入下一次请求的快照；提示词取值作为 Append 块 | 下一次变化，或撤回 |
| 撤回中 | 正在移除 | 当前请求保留它已绑定的内容 | 所有者确认 |
| 已撤回 | 表中已无该项 | 无；提示词块由 Revoke 撤下 | 无：再次撤回是空操作，因为 id 已不匹配 |

变更表中已有名字的登记是一次撤回加一次登记，顺序固定，第二次不会与第一次抢成同名拒绝。同一名字的两代是两个独立项、两个独立 id：按旧 id 撤回不会删掉新项。

### 2.6 桥接不保留什么

不排队、不重试、不定时器，不建第二套注册表。它实际持有的状态只有：每行一份账本、每行见过的每个 Session 一个门面、每行一份副本定义。被所有者拒绝的登记报告给包的调用或记入副本诊断后不再保存。它没有 Session 历史缓存、没有 Timeline 的影子、没有上下文的副本。

## 3. 流程

### 3.1 装载一个包

```mermaid
flowchart TD
  A["package.json 声明 Cordis 或 DSH 依赖"] --> B["PluginManager 写入 .dsh/id/entry.mjs"]
  B --> C["行启动"]
  C --> D["入口导入桥接与包"]
  D --> E{"宿主 API 与注入名都受支持？"}
  E -->|否| F["行 Failed，写明缺失契约"]
  E -->|是| G["桥接登记一份 Session 副本定义"]
  G --> H["行 Active"]
  H --> I["每个已打开的 Session 挂载副本"]
  H --> J["之后每个新 Session 挂载副本"]
  I --> K["副本向本 Session 各所有者登记贡献"]
  J --> K
```

行只有在定义交出、服务宣告完成后才 `Active`，与其他 Node 行一致。已经打开的 Session 在定义登记时挂载副本；之后打开的 Session 在打开过程中、发布之前挂载副本。因此凡是先装载包、后使用 Session 的情形，包对第一次请求就已生效，不需要追赶步骤。

### 3.2 登记与发布

```mermaid
sequenceDiagram
  participant P as 包
  participant B as 桥接与账本
  participant C as Session 副本
  participant O as 所有者
  P->>B: tools.register(definition, execute)
  B->>B: 账本一项，本地代数
  B-->>P: disposer
  B->>C: 协调器对齐表
  C->>O: 用副本租约登记
  O-->>C: id
  C->>B: 该本地代数对应远端 id
  Note over O: 工具：下一次请求快照<br/>提示词：分支采纳取值
```

工具经 Session 的工具来源集到达请求，而请求在 ToolBinding 处取快照，因此请求运行期间的登记或撤回只影响后续请求。提示词取值经采纳进入请求（[§3.5](#35-请求准入顺序)），这也是包的文本进入模型上下文的唯一通道。

### 3.3 撤回单个贡献

disposer 在包自己的调用栈中运行：删掉账本项并唤醒协调器，协调器按所有者分配的 id 撤回，且只针对登记过它的那些 Session 副本。撤回对已绑定该贡献的请求不可见：该请求保留自己的快照，也不关心它离开了。提示词取值的撤回是一次 Revoke，被撤下的块在下次采纳时离开模型上下文，而不是继续留存。

除 disposer、Session 副本卸载、包行卸载之外，没有任何东西撤回贡献。Session 关闭通过卸载副本来撤回它的全部贡献，不需要为每项各调一次。

### 3.4 失败

| 失败 | 判定处 | 效果 |
| --- | --- | --- |
| 包注入了本设计不提供的名字 | 桥接，行变为 Active 之前 | 行 Failed，消息写明缺失的名字 |
| 包登记了 harness 有、Dim 不提供的钩子 | 桥接，该次登记调用中 | 行 Failed：半装载的包会在作者以为有守卫的情况下运行 |
| 包导入了自己的 Cordis 副本 | 运行时，模块解析时 | 行 Failed，写明入口与副本 |
| 包的 Cordis peer 区间排除运行时那份 4.0.4 | 桥接，行变为 Active 之前 | 行 Failed，`cordis_range`，写明该区间 |
| 包模块装载时抛错 | 运行时 | 行 Failed，带模块错误 |
| 某个 Session 副本构建失败 | 部件登记处，针对该 Session | 只有该副本标记 Failed，行与其他 Session 正常 |
| 某个贡献与宿主工具或其他插件同名 | 所有者，在登记回复中 | 该贡献被拒，副本记录后保留其余贡献 |
| 包的 `dispose` 抛错 | 运行时，卸载期间 | 记为该行的清理错误，其他行不受影响 |

被拒的贡献不会被静默重试：包在发起它的那次调用中看到拒绝，或在调用已返回后于副本诊断中看到。

### 3.5 请求准入顺序

包的动态上下文没有通知：`dsh-mysql` 在 harness 组装该步时从自己模块的闭包里读当前选中的库，harness 因此每一步都重读一次。Dim 的提示词来源登记一次、由分支在打开时采纳，若不重读，包变化后的取值要等到下次打开 Session 才到模型。重读需要提示词所有者提供两件事，本设计提出这两项需求（[§10](#10-向机制所有者提出的需求)）：分支在一步之前向来源索取当前取值；该步在为其写出的输入被 Loop 准入之前不发起模型请求。

```mermaid
sequenceDiagram
  participant P as 包
  participant H as 提示词来源宿主
  participant B as Prompt（分支）
  participant Q as InputQueue
  participant L as Loop
  participant M as ModelProvider
  P->>P: 状态变化，无通知
  L->>B: 步边界，需要核对来源
  B->>H: 索取当前取值，在所有锁之外
  H-->>B: 渲染后的条目
  alt 取值未变
    B-->>L: 无需投递
  else 取值已变
    B->>B: 发布配置与投递 id
    B->>Q: input/message，append 模式，与 prompt/changed 同一批次
    Q->>Q: input/applied
    Q->>L: input/applied
    L->>L: input/consumed
  end
  L->>M: 带已采纳上下文发起模型请求
```

| 步骤 | 谁执行 | 让该步继续的事实 | 此处的中断或重载 |
| --- | --- | --- | --- |
| 1 | 包改变自己的状态 | 无：取值在被索取时才读 | 无可取消项 |
| 2 | Loop 到达步边界 | 回合开始、工具结果批次、压缩结果或请求替换 | 中断使回合以取消结束 |
| 3 | Prompt 读取需要的取值 | 在所有锁之外，带分支取消 | 中断立即结束回合，不等读取返回 |
| 4 | Prompt 比较并发布 | 一个原子批次：`prompt/changed` 与 Append 输入 | 分支关闭或该步已取消时，批次在提交锁内自我拒绝 |
| 5 | InputQueue 准入 Append 输入 | 队列自己的顺序 | 若回合已先结束，输入留给下一回合 |
| 6 | Loop 准入该投递 | 发布者写入的输入 id | 该回合已不再等待的 id 被忽略 |
| 7 | Loop 发起请求 | 本步的投递全部准入 | 已发起的请求按被 steer 的请求替换 |

这条链依赖三个性质。读取在提交锁之外，包在自己的代码里阻塞只拖慢自己这一步，不会拖慢别的 Session。Append 输入不会开启回合：空闲 Session 上被采纳的取值等操作者的下一条消息，而不是自己开一轮。取值未变则不写任何东西，因此每步都核对一遍的包只花一次边界调用，不写 Timeline。

如果进程在步骤 4 与 7 之间停止，该回合有开始和已采纳配置却没有模型请求。现有重建规则会把这种回合结算为失败，并把 Append 输入留在队列里供下一回合消费，因此包的上下文既不会被重放，也不会丢。

### 3.6 工具调用与 stop

```mermaid
sequenceDiagram
  participant L as Loop
  participant T as ToolExecutor
  participant B as 桥接
  participant P as 包
  L->>T: 本批调用
  T->>B: 带上参数与取消 future
  B->>P: tool.execute，带 AbortSignal
  alt 包返回
    P-->>B: 结果
    B-->>T: 渲染后的内容与结构化内容
    T->>L: 工具结果
  else 操作者 stop
    T->>B: 取消
    B->>P: signal 置为已中止
    P-->>B: 包结算，或忽略
    B-->>T: 结果或拒绝
    T->>L: 工具结果，随后回合结束
  else 插件代次卸载
    T->>T: 立即截断等待
    T->>L: 工具结果，说明可能已生效
  end
```

stop 是协作式的，对所有外语工具都是如此：把取消传进包，等这次调用结算。只有卸载插件代次才截断等待，此时结果写明「可能已生效」，因为 Dim 无法观测外部写入是否落地。忽略信号的包会被一直等待，操作者通过重载或禁用该插件结束等待（[§6](#6-终端-ui)）。

桥接由那个取消 future 构造工具的 `AbortSignal`：future 结算时置为已中止。宿主传的是 future，不是标志（`crates/dim-agent-capabilities/src/tool_registry.rs:767-772`），因此包把信号理解为「触发即已中止」；本设计不承诺包第一次同步读时它已为 aborted。派发前就被取消的调用，包根本不会收到：执行器在它到达提供方之前就拒绝并回复 `Tool cancelled before execution`（`crates/dim-agent-capabilities/src/tool_executor.rs:408-415`、`:447-456`）。

### 3.7 命令与子 Session

```mermaid
flowchart TD
  A["操作者选择插件命令"] --> B["session.command.run，带分支与参数"]
  B --> C["pluginCommands 调用包的处理函数"]
  C --> D["包调用 subagents.startContinuable"]
  D --> E["sessions.launch，带 parentSessionId、标题与受父封顶的策略"]
  E --> F{"已取消或被拒？"}
  F -->|是| G["sessions.delete 子 Session，命令失败"]
  F -->|否| H["sessions.input 投递开场消息"]
  H --> I["输入回执"]
  I --> J["命令立即返回子 Session 的 id"]
  J --> K["子 Session 自己跑回合；操作者可接手"]
```

命令在子 Session 的首条消息入队时返回，而不是等它回答，这正是 harness 的可继续子 agent 契约对处理函数的承诺。子 Session 是普通 Session：持久保存、按父列出、空闲时与其他 Session 一样卸载，删除父时经已存的父子关系级联删除。父的 Timeline 不受影响：命令不向父写输入、也不写事实。

## 4. 寿命

一个 Session 在各种事件中的样子。包的每 Session 状态指的是包模块闭包里的 `Map`。

| 事件 | Dim Session | 内存实例 | 包在该 Session 的副本 | 包在行内的状态 | 外部工作 |
| --- | --- | --- | --- | --- | --- |
| 稳态 | 已存 | 已装载 | 已挂载，贡献已登记 | 存活 | 无 |
| 空闲卸载 | 已存，不变 | 卸载，不写任何事件 | 卸载；其贡献消失；租约归还 | 存活；门面槽位置空；不发 `agent/disposed` | 无：卸载从不取消已受理的工作 |
| 空闲卸载后恢复 | 已存，不变 | 重新装载，分支以 `branch/resume` 打开 | 重新挂载：包的贡献随副本的 `apply` 重新登记 | 存活；槽位重绑；不发 `agent/created` | 无 |
| `session.close` | 已存，本进程内为 closed | 卸载 | 卸载 | 存活；为该 Session 发 `agent/disposed` | 无：关闭会等待自己受理的工作 |
| Session 删除 | 记录删除 | 卸载 | 卸载 | 存活；为该 Session 发 `agent/disposed` | 无 |
| 插件重载或禁用 | 已存 | 已装载 | 随行卸载；新代再挂载 | 全新模块：包内存在所有 Session 上清空 | 在途调用被截断为「可能已生效」 |
| daemon 重启 | 已存 | 被 acquire 时装载 | 首次装载时挂载 | 全新进程：包内存清空；`agent/created` 一次，Session 为恢复时带 `source: 'resume'` | 旧进程的套接字随进程关闭；写入是否落地未知 |

第四、五行正是 Session 所有者必须变得可观测的部分：空闲卸载与业务关闭结束同一个作用域，单靠 `sessionLifecycle.closing` 副本无法区分二者（[§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始)）。最后一行的 `source` 来自 `sessionIdentity.restored`：恢复已存 Session 的装载为 `true`，新建的为 `false`（`crates/dim-agent-runtime/src/config.rs:234-249`、`crates/dim-agent-runtime/src/assembly.rs:1749`）。

被访问过但未关闭的 Session，其条目在包自己的每 Session `Map` 里保留到进程结束，因此这部分内存与「包见过且未关闭的 Session」成正比，而不是与「当前装载的 Session」成正比。daemon 重启会清空它，这与重启 harness 本身一致。

## 5. 上下文映射

| 位置 | 模型看到什么 | 形式 | 存续 |
| --- | --- | --- | --- |
| 该 Session 的任意分支 | 包声明的工具，含其声明中的描述与 schema | 请求快照中的工具声明 | 贡献它的副本挂载期间 |
| 该 Session 的任意分支 | 包渲染后的工具结果 | `ToolResult.content`；包的结构化返回值不进模型视野 | 与普通工具结果相同 |
| 该 Session 的任意分支 | 包的系统段（若声明） | 已采纳系统提示词的一部分 | 取值变化时重发 |
| 该 Session 的任意分支 | 包的动态上下文 | 一个 `<dim-state>` 块，与其他状态来源一样被采纳 | 撤回时由 Revoke 撤下 |
| 子 Session | 它自己的回合，从开场消息开始 | 普通 Session 上下文 | 它自己的 |
| 父 Session | 命令没有写入任何内容；操作者只看到命令回复的文字 | 不是模型上下文 | 不适用 |

包不写 Timeline 事实、不接触 `dispatcher`、不给请求追加文字。它对模型可见的每一个字节都经工具结果或经采纳到达。

## 6. 终端 UI

四处界面都在客户端。各类按键与布局归终端 UI 所有者（[cli-commands](../cli-commands.md)）；下面只说明本设计要求它们显示什么，`r` 键只是出路的一个示例，不是本设计设定的绑定。四处都在 [§10.6](#106-客户端stop-提示以及插件命令与动作所需的界面) 中提出。

**命令菜单。** 一个 Session 的插件命令列在 `/` 菜单中内置命令之后、Skills 之前，这是[命令菜单契约](../cli-commands.md#命令菜单)留给动态组的唯一位置。每行写明命令、描述与它来自哪个插件。

```text
+----------------------------------------------------------------------+
| > /side                                                              |
+----------------------------------------------------------------------+
|  /side-chat    开一个独立的旁会话，自己保留一整段对话                |
|                [plugin: dsh-side-session]                            |
+----------------------------------------------------------------------+
  Enter 执行 session.command.run("side-chat") · Tab 补全 · Esc 关闭
```

**子 Session。** 子 Session 与其他 Session 一样列出，带父与来源，操作者是接手它，而不是通过父驱动它。列表的父字段是 `SessionInfo.parentSessionId`，已存 Session 本就上报它，因此这一行唯一需要的改动是 [§10.4](#104-agentssessionslaunch-接受父-session) 让 launch 存下父关系。`/resume` 选择器就是该列表（[cli-commands](../cli-commands.md#resume-选择面板)）；按父筛选子会话的面在 [§10.6](#106-客户端stop-提示以及插件命令与动作所需的界面) 中提出。

```text
+--------------------- /sessions  (Tab: 全部 | 本会话的子会话 [1]) ---------------------+
| > 旁会话 14:32    [internal · parent 8f3a..c1]    "已就绪，请提出第一个问题"          |
|   本会话          [client   · 8f3a..c1]           "排查订单表慢查询"            2m 前 |
+--------------------------------------------------------------------------------------+
  Enter 接手子会话 · d 删除 · Esc 关闭
```

**插件动作。** 导出本地动作的包在 `/plugins` 中得到一个面板，对任何包通用：动作列表、某个动作的 JSON 参数（动作涉及 Session 时自动填入当前 Session id）、以及结构化回复。该面板与其动作在 [§10.6](#106-客户端stop-提示以及插件命令与动作所需的界面) 中提出。

```text
+------------------ /plugins -> dsh-mysql -> 动作（本地动作）---------------------------+
| 动作: [ selectConnection ]   listConnections | saveConnection | testConnection        |
| 参数（已填入当前 Session id）:                                                        |
|   { "sessionId": "8f3a9c10-…", "connectionId": "orders-ro" }                          |
| 结果: { "ok": true, "connectionId": "orders-ro", "connectionIds": ["orders-ro"] }     |
+--------------------------------------------------------------------------------------+
  Ctrl+Enter 执行 dsh_typert.dsh_mysql.selectConnection · Esc 返回
```

**等待中的 stop。** stop 在等某个插件工具时，运行中的回合带一行说明，写明插件与出路。这是客户端全局提示，不针对 DSH：所有尚未结算的外语工具都适用。

```text
  正在停止。dsh-mysql 尚未返回。
  r 重载该插件以结束等待；此后它影响每一个会话，且其内存状态被重置。
  Esc 让 stop 继续等待。
```

## 7. 接口

下表每一行要么是本设计使用的现有服务面，要么是本设计向其所有者提出的服务面。所有者文档才是契约；本表只说明本设计如何使用它。

| 名字 | 作用域 | 服务面 | 所有者契约 | 本设计 |
| --- | --- | --- | --- | --- |
| `partRegistry` | 进程 | data（`registerDefinition`） | [Dim 如何使用 rutis §2](../rutis-usage.md#2-插件是一行实例挂载它的部件)、[插件契约 §4](../plugin-contract.md#4-dim-的服务) | 每个包行登记一份 Session 副本定义 |
| `managementMethods` | 进程 | data（`register`） | [Session 边界 §3.2](../contracts/session-boundary.md#32-管理方法) | 每个包的本地动作，命名空间 `dsh_typert.<包>` |
| `toolRegistry` | Session | data（`register`） | [插件契约 §4](../plugin-contract.md#4-dim-的服务)、[ToolRegistry](../../../architecture/rust-agent/plugins/ToolRegistry.md) | 每个副本登记该包的工具 |
| `promptRegistry`、`promptHost`、`initialPromptConfiguration` | Session | 提示词数据面（[PromptSources](PromptSources.md)、[Prompt](Prompt.md)） | 同上 | 每个副本为包登记一个提示词来源，取值由分支采纳 |
| `pluginCommands` | Session | data，在 [§10.3](#103-插件命令session-命令表与固定方法命名空间) 中向所有者提出 | `dim-agent-capabilities`，拥有命令表的 Session 部件；菜单归[命令菜单](../cli-commands.md#命令菜单) | 每个副本登记该包的命令 |
| `session.command.list`、`session.command.run` | Session，位于 `sessionMethods` | data | [Session 边界 §3.1](../contracts/session-boundary.md#31-session-方法)、[cli-commands](../cli-commands.md#命令菜单) | 客户端读取并执行插件命令 |
| `sessions` | 进程 | data（`launch`、`input`、`delete`、`stop`） | [Session access A.1](../contracts/session-access.md#a1-sessions-服务) | 子 Session 及其开场输入 |
| `sessionIdentity`、`sessionConfig` | Session | data | [插件契约 §4](../plugin-contract.md#4-dim-的服务) | 副本读取自己 Session 的身份（含 `restored`）与配置 |
| `sessionLifecycle` | Session | data | [插件契约 §4](../plugin-contract.md#4-dim-的服务) | 副本等 `activated`，在 `closing` 时停手；作用域结束的原因在 [§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始) 中向 Session 所有者提出 |
| Session 的结束原因 | Session | data，在 [§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始) 中向 Session 所有者提出 | [插件契约 §4](../plugin-contract.md#4-dim-的服务) | 区分业务关闭、删除与空闲卸载，`agent/disposed` 依赖它 |
| `agent/session-deleted` | 进程，总线事件 | 宿主内部，不是本设计使用的服务面 | [插件契约 §5.3](../plugin-contract.md#53-总线事件) | 桥接不读它：删除的原因经 [§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始) 的 Session 面到达 |

本设计拥有的名字：受管入口路径 `<插件文件根>/.dsh/<插件 id>/entry.mjs`、管理命名空间 `dsh_typert.<包>`，以及桥接自己的包名。后者不是 Dim 的服务名，不进目录。

桥接对包作者提供的宿主 API 是契约，写在生成的 TypeScript 声明里，与 Dim 服务声明并列，因此包作者可以像对着 `npm/plugin.d.ts` 那样对它做类型检查。

## 8. 安全

- **包是受信任的宿主代码。** 已装载的行以用户权限运行在运行时进程内，与其他 Node 行相同，[ADR 006 第 11 条](../../adr/006-plugin-contract.md) 适用，本设计不对它做沙箱。包的工具调用仍受该 Session 策略下的 Approval 与 Permission 约束，与 Dim 自带的工具相同。
- **风险类来自包，且必须给出。** 工具声明写明风险类；Permission 把未知风险类视为未分类，预设可拒绝、询问或允许（[Permission](../../../architecture/rust-agent/plugins/Permission.md)）。缺少风险类的声明被桥接拒绝，而不是填一个宽松默认值。
- **硬依赖不受支持的名字即拒绝装载，而非降级。** 需要 HTTP 路由或步前钩子的包在装载时失败，因此模型不会运行在一个自以为有守卫、其实没有的包之下。
- **秘密留在包里。** Dim 没有 secret store（[Settings 服务 §1.2](../appserver/settings-service.md)），本设计也不新增：持久化凭据的包自己管文件，动作回复也由它自己决定。到达客户端的只是包返回的内容；返回密码是包自身的缺陷，面板照原样显示它返回的文档。
- **管理动作仅限本地。** `managementMethods` 只对受保护的本地连接提供，从不提供给配对设备，因此包的配置动作属于宿主管理员面。
- **Session id 不是授权。** `sessions`、`pluginCommands` 与提示词来源都把 Session id 当数据，各所有者文档已如此约定。包可以指名任意 Session；操作者的审批仍决定它的工具在该 Session 中能做什么。
- **入口不写入包内。** 受管入口位于插件文件根下，包目录只读；向自己目录写文件的包仍会在与 harness 中相同的位置失败。

## 9. 规则检查

| 规则 | 本设计如何满足 |
| --- | --- |
| §4 规则 1 改他处状态写指令事件 | 包不写其他所有者的状态。动态上下文变化由 `Prompt` 经既有发布器写出，子 Session 由 `sessions.launch` 创建，两者都在所有者自己的流程里 |
| §4 规则 2 交接后不可撤回 | 已采纳的 Append 输入属 InputQueue 与 Loop；包无法撤回它，之后的取值是一次新的采纳。工具结果是执行器的事实 |
| §4 规则 3 `before` 回调 | 不适用：桥接不注册任何 `before`，需要它的拦截点在装载时即被拒 |
| §4 规则 4 核心不带业务含义 | Timeline、Dispatcher、InputQueue、Loop 都没有包特有的处理。步前重读与准入屏障用所有者自己的术语表述，服务任何提示词来源 |
| §4 规则 5 原子批次、按序派发 | 配置与其投递输入同批；该步等到投递输入被准入，因此一个请求不会看到取值而没有对应输入 |
| §4 规则 6 上下文只来自已交接事件 | 模型可见的每个字节都以工具结果或采纳形式到达；不向请求注入任何内容，桥接也无法追加文字 |
| §4 规则 7 仅经公开事件与服务交互 | 包经 `partRegistry`、`toolRegistry`、提示词来源、`pluginCommands`、`managementMethods`、`sessions` 与 Session 生命周期面接触 Dim，本表已逐项注明所有者；它不读其他插件的状态，尤其不读宿主内部总线事件 |
| §4 规则 8 基础设施默认不变 | 桥接不改 rutis 与 loader；它按现有行、定义与租约契约使用它们。它对其他所有者的需求均在 [§10](#10-向机制所有者提出的需求) 中以所有者自己的术语提出：Prompt 与 Loop（10.1）、登记所有者（10.2）、Session 命令表（10.3）、Agents（10.4）、ToolExecution（10.5）、客户端（10.6）、Session 生命周期（10.7）、PluginManager（10.8）与 RuntimeManager（10.9） |
| §4 规则 9 只有后续决策需要的事实进 Timeline | 不新增 Timeline 事件。账本、门面、副本定义都是进程与 Session 内存，没有一项是后续决策要读的事实 |
| §1 Capability 与 App Server 边界 | 本设计没有部件是 Capability：没有部件折叠分支事实或写事实。桥接是行内的库，App Server 中不新增业务代码 |
| ADR 002 D1 作用域随所有者 | 包行与它的伙伴服务是跨 Session 的进程状态；贡献属于 Session，因此登记它们的副本在 Session 作用域；没有分支部件 |
| ADR 002 D2 不设接线模块 | 桥接不提供名字、不持有进程状态、不适配其他插件接口。它把包自己的贡献登记到所有者，这正是每个贡献部件都做的事 |
| ADR 002 D3 依赖随作用域 | 桥接依赖进程与 Session 所有者，并经 `sessions` 写其他 Session；核心不依赖它，某个包的失败只影响它自己的贡献 |
| ADR 002 D4 结构与业务配置分离 | 行的启用与包的安装归 PluginManager；包的业务配置留在它自己的文件与动作里，不新增 Settings 来源 |
| ADR 002 D5 契约语言中立 | 上表每个面都以操作、数据与错误码表述；桥接的 TypeScript API 是同一套操作的一种绑定 |
| ADR 008 D2、D3 不写状态、只写自己作用域 | 本文只写设计、它提出的需求与它使用的东西；每项需求都注明所有者并留给该所有者的文档去写改动，本页不描述其他模块的内部实现 |

## 10. 向机制所有者提出的需求

### 10.1 Prompt 与 Loop：一步之前重读动态来源，并等它们被准入

- **受阻功能与规则：** 包的上下文会静默变化。`initialPromptConfiguration` 只在分支打开时读取，来源取值也在那时采纳，因此之后变化的取值到不了任何请求（[PromptSources](PromptSources.md)、[Prompt §3.4](Prompt.md#34-在写入者装载时采用)）。发布器确认批次已发布，并不能告诉分支它写的输入已被 Loop 准入，只凭该确认发起请求，可能恰好漏掉刚采纳的取值。
- **已排查的现有机制：** 指令来源使用的周期性分支检查会把变化延迟到一个检查间隔，而且按计划写事件，无法服务「下一步模型必须看到」的取值；把包的文本登记为 `System` 来源会把这个每步变化的值放进可缓存前缀；由包自己写输入则是在写别的所有者的状态（规则 1）。
- **所需通用能力：** 用提示词所有者的术语：分支在一步之前可以判断自己的动态来源是否需要重读，在所有锁之外读取它们，用既有原子批次采纳变化，并把写出的投递 id 交给 Loop；Loop 在这些输入被准入之后才发起请求，等待期间收到中断则以取消结束回合。取值未变则不写、不等。
- **对序号、批次、中断、重载的影响：** 不新增事件类型，批次仍然原子；中断不等忽略取消的包即结束回合；批次与请求之间崩溃时，该回路由既有重建规则结算为失败，Append 输入留给下一回合。
- **实现归属与验证：** `dim-agent-capabilities`（`prompt`、`prompt_registry`、`agent_loop`），由其自己的文档（[Prompt](Prompt.md)、[PromptSources](PromptSources.md) 与 Loop 架构页）写明改动。用流测试验证：两轮之间取值变化的来源在第二次请求中以一个 `<dim-state>` 块到达；取值未变不写任何事件；读取期间中断则以取消结束回合且不发起请求。

### 10.2 运行期登记：按所有者校验的单条撤回

- **受阻功能与规则：** 包的 disposer 要在该行仍在运行时撤回单个贡献。登记表只在所属行卸载时移除条目，因此包想替换一个工具就得重载整行，而那会清空它在所有 Session 上的每 Session 状态。
- **已排查的现有机制：** 卸载并重载整行（正确，但代价是所有 Session 的包状态与其他贡献一起被清掉）；在桥接里再建一张隐藏已撤回项的表（影子注册表，规则 8 禁止，且所有者那张表仍是错的）。
- **所需通用能力：** 登记表记录每项的所属者与每个属主代次的一次性清理；登记回复带回该项 id；一次撤回只移除 id 与所属者都匹配的条目，不匹配时拒绝而不是删别的所有者的项。对 `toolRegistry`、提示词来源与 `pluginCommands` 一致适用。
- **对序号、批次、中断、重载的影响：** Timeline 无影响；撤回只改变下一次请求快照的内容，已绑定的请求不变。行卸载仍一次移除它登记的全部内容。
- **实现归属与验证：** `dim-agent-runtime`（登记表）与 `dim-agent-capabilities`（三个所有者的服务面）。用流测试验证：同一个工具登记加撤回五百次后所有者的清理项仍为一项；另一行按同名撤回被拒；撤回前已绑定的请求仍返回旧处理器结果，而下一请求不再声明该工具。

### 10.3 插件命令：Session 命令表与固定方法命名空间

- **受阻功能与规则：** 命令名含连字符（`/side-chat`），而 Session 方法与命名空间的每一段只能是 `[a-z][a-z0-9_]*`（`crates/dim-agent-capabilities/src/methods.rs:205-213`，[Session 边界 §3.1](../contracts/session-boundary.md#31-session-方法)）。按命令逐条建方法，对合法命令名根本不可行；一命令一方法也会让命令列表对未逐个订阅的客户端不可读。
- **已排查的现有机制：** 把命令名清洗成方法名（客户端与包对操作者所敲的名字会不一致）；用工具代替命令（斜杠命令不是模型动作，不该消耗一次请求）；在 `sessionMethods` 用共享命名空间加名字参数（这正是下面的形态，但需要后面那张表）。
- **所需通用能力：** 一个拥有命令表的 Session 部件 `pluginCommands`，其 `register`、`withdraw`、`list` 是数据操作，条目属于调用方副本；以及固定 Session 方法命名空间 `session.command`，含 `list` 与 `run {name, branchId?, args?}`。`run` 携带调用方取消信号，回复命令自己的文字与结果。命令名为 `[a-z][a-z0-9_-]*`，与内置斜杠命令同名的登记被拒。
- **对序号、批次、中断、重载的影响：** 不写 Timeline 事实；该表是 Session 内存，副本挂载时重建。包重载会中断已受理的 run，客户端看到的是中断。撤回后新调用返回 `unknown_command`，而不是 reloading 错误。
- **实现归属与验证：** 部件归 `dim-agent-capabilities`，菜单槽位归[命令菜单](../cli-commands.md#命令菜单)所有者（[§10.6](#106-客户端stop-提示以及插件命令与动作所需的界面)）。用流测试验证：含连字符的命令可登记，`session.command.list` 能列出，`session.command.run` 带调用方取消执行它，运行中重载会中断，撤回后再次运行为 `unknown_command`。

### 10.4 Agents：`sessions.launch` 接受父 Session

- **受阻功能与规则：** 包的子 Session 必须是父的子会话。`sessions.launch` 创建 Session 时不传 leader（`crates/dim-app-server/src/agents/sessions.rs:461-468`），因此没有父被存下，子会话不会被按父列出，删除父也不会级联到它：删除路径只删 `led_members` 返回的 Session，而该读取只选已存配置标为协作成员的 Session（`crates/dim-app-server/src/agent_records.rs:159-178`，调用处 `crates/dim-app-server/src/agents.rs:1531-1533`）。本设计创建的子会话不是协作成员，因此该级联不会到达它。
- **已排查的现有机制：** 包自己调 `session.create` 并在标题里写父（标题是文字不是关系，级联仍然漏掉它）；另开一个创建入口（重复了所有者那一个方法）。
- **所需通用能力：** launch 请求上一个可选 `parentSessionId`，经既有列存为该子会话的 leader；删除路径的级联扩展到所有 leader 为被删 Session 的已存 Session，而不只是协作成员。Collaboration 自己的读取保留成员过滤：`led_members`（`crates/dim-app-server/src/agent_records.rs:159-178`）与 `members`（`:215-236`，使用于 `crates/dim-app-server/src/collaboration.rs:510`）仍排除非成员子会话，因此包的子会话是带父的 Session，而不是委派成员。同一请求另带可选 `interactive`，使需要操作者审批的子会话能够提问，并按发起 Session 的策略封顶，与现有策略封顶一致。
- **对序号、批次、中断、重载的影响：** Timeline 不变；父的 Timeline 不受影响。首条输入之前被取消的 launch 由既有 `sessions.delete` 清理，不留半成品子会话。
- **实现归属与验证：** Agents（`crates/dim-app-server`），由 sessions 服务文档写明该字段。用流测试验证：创建的子会话记录父、按子会话列出、首条消息被执行、随父删除；被取消的 launch 不留下 Session。

### 10.5 工具执行：崩溃恢复不得声称调用未执行

- **受阻功能与规则：** 被崩溃或能力重载打断的工具调用在分支重建时结算。重建选出所有已提交、无结果、且没有子结果的工具调用批次，把其中每个调用结算为「Tool did not finish before Capability reload; an effect may have happened」，或在该批次完全没有子结果时结算为「Tool was not executed: capability reloading」（`crates/dim-agent-capabilities/src/tool_executor.rs:1020-1060,1146-1159`）。第二种措辞取决于批次，而不是某个调用是否被派发。一个已经到达包、随后崩溃的单个调用写工具批次因此被结算成「未执行」，模型据此认为写入没有发生，而这是假的。
- **已排查的现有机制：** 为每次调用写执行器 progress 事实，使批次级区分变成调用级（内置装配下是关闭的，而且它把每次工具调用都变成一次额外的 Timeline 写入）；让包自己上报进度（包正是结果未知的一方，它来源的 harness 也不要求它这样做）。
- **所需通用能力：** 对无法排除已派发的调用，恢复文案必须说「该调用未完成、可能已发生效果」，而不是「未执行」；能确定从未派发的批次保留未执行的措辞。外语工具卸载时已有的 may already have taken effect 措辞即是先例。
- **对序号、批次、中断、重载的影响：** 同一个结果事件，改一个字符串；重载仍使该回合失败。
- **实现归属与验证：** `dim-agent-capabilities`（`tool_executor`），由 [ToolExecution](../../../architecture/rust-agent/plugins/ToolExecution.md) 写明结算。用流测试打开一个含未配对工具调用的库，断言恢复出的结果说明效果可能已发生。

### 10.6 客户端：stop 提示，以及插件命令与动作所需的界面

- **受阻功能与规则：** 本设计需要四处不归它所有的客户端界面。stop 可能等在忽略取消的外语工具上，而操作者对此一无所知；`/` 菜单是固定策展顺序，没有给 Session 插件命令的位置（[cli-commands](../cli-commands.md#命令菜单)）；`/plugins` 面板是 PluginManager 方法之上的一个管理区，没有渲染或执行包自己的本地动作的途径（[management surfaces](../cli-commands.md#管理界面)）；Session 列表也没有按父筛选子会话的过滤，而每行已带 `parentSessionId`（[Session 目录](../appserver/session-catalog.md#31-sessioncatalog)、`crates/dim-app-server/src/protocol.rs:1306-1308`）。
- **所需通用能力：** 运行中的回合在 stop 等待外语工具时显示一行，写明插件与出路（[§6](#6-终端-ui)），对所有外语工具生效而不只是这些包；`/` 菜单中内置命令之后、Skills 之前的动态组，行来自 `session.command.list`，执行走 `session.command.run`；`/plugins` 中一个对任何提供 `dsh_typert.<包>` 命名空间的插件通用的动作面板，列出其动作，编辑某个动作的 JSON 参数（动作涉及 Session 时填入当前 Session id），并显示结构化回复；以及 Session 列表上按父筛选子会话的过滤，读列表已有的 `parentSessionId`。
- **对序号、批次、中断、重载的影响：** Timeline 无影响；四项都是读取路径加相应的一次既有方法调用。面板不读包自己的设置：它只渲染命名空间声明的动作与包返回的回复。
- **实现归属与验证：** 四项都归终端 UI 所有者，由命令菜单契约写明该组、由 management surfaces 契约写明面板的动作类。用 CLI 流测试验证：登记了命令的 Session 在菜单中列出并可执行；stop 等待插件工具时显示提示；包的命名空间在 `/plugins` 中渲染且能执行一个动作；子会话能经该过滤找到。

### 10.7 Agents 与 Session 生命周期：作用域为何结束，以及它如何开始

- **受阻功能与规则：** 桥接在业务关闭与 Session 删除时发 `agent/disposed`，空闲卸载时绝不发；daemon 重启后首次装载某 Session 时发 `agent/created` 并带 `source: 'resume'`。Session 旁边没有任何服务面告诉它这些。无论业务关闭还是空闲卸载，Session 作用域都会结束（[插件契约 §2](../plugin-contract.md#2-实例树)），而运行时对两者取消同一个 closing 令牌（`crates/dim-agent-runtime/src/agent.rs:804-808`，`finish(unloading)`，由 `:754-764` 的 `unload_if_idle` 与 `:801-803` 的 `close` 进入），所以 `sessionLifecycle.closing` 无法区分二者（`crates/dim-agent-runtime/src/config.rs:403-408`；[插件契约 §4](../plugin-contract.md#4-dim-的服务) 中该行只列 `activated`、`closing`、`hold`、`release`）。删除只以 `agent/session-deleted` 宣告，那是宿主内部总线事件（[插件契约 §5.3](../plugin-contract.md#53-总线事件)）。创建还是恢复这一半已经是数据：`sessionIdentity.get()` 是 `{sessionId, restored}`（`crates/dim-agent-runtime/src/config.rs:234-249`），`restored` 即 `SessionInitialization::Restore`（`crates/dim-agent-runtime/src/assembly.rs:1749`，在 `crates/dim-agent-runtime/src/agent.rs:169-183` 选定）。
- **已排查的现有机制：** 在桥接里直接监听 `agent/session-deleted`（宿主内部事件而非服务面，而且行分不清它与同一 Session 的空闲卸载）；在每个 `closing` 时都发 `agent/disposed`（这正是本设计拒绝的空闲卸载通知，会在包本应存活的卸载中丢弃它的每 Session 选择）；对包见过的每个 Session 各持一个 `hold`（让包碰过的每个 Session 都无法空闲卸载，这是本设计拒绝支付的代价）。
- **所需通用能力：** 由 Session 所有者提供一个公开数据面，用该所有者自己的术语，让 Session 作用域的部件得知两件事：自己的作用域为何结束，按所有者已经知道的区分，即业务关闭、删除与空闲卸载；以及本次装载是创建还是恢复。该面在副本首次 `apply` 返回前就可读，使 `agent/created` 首次挂载就能带对的 `source`。所有者没有含义的原因不编造：该面报告所有者自己已经决定的东西。
- **对序号、批次、中断、重载的影响：** 不新增 Timeline 事实，也不新增事件类型；结束原因是 Session 的运行期状态，空闲卸载对 Timeline 仍与今天一样不可见（`crates/dim-agent-runtime/src/agent.rs:754-764` 不写任何事件）。插件重载仍然什么都不发：副本随其行卸载，既不是关闭也不是删除。
- **实现归属与验证：** Agents 与运行时的 Session 生命周期（`crates/dim-agent-runtime`、`crates/dim-app-server`），由 [插件契约 §4](../plugin-contract.md#4-dim-的服务) 与 §2 的 Session 行写明该面。用流测试验证：同一 Session 的空闲卸载与业务关闭以不同原因报告，删除报告为删除，daemon 重启后的装载报告 `restored: true` 而新 Session 为 `false`，且包的每 Session 选择跨卸载与恢复保留（[附录 D](#附录-d-验收) 第 7 项）。

### 10.8 PluginManager：识别 DSH 包并写受管入口

- **受阻功能与规则：** DSH 包不带 `dim-plugin.json`，PluginManager 找不到它的候选，其行从不创建。它的 `package.json` 是写给它自己的 harness 的，其 `dsh` 成员是该 harness 的 bundle 声明，不是 Dim 清单（[§2.1](#21-受管入口)）。
- **已排查的现有机制：** 要求用户给每个包加一个 `dim-plugin.json`（包目录只读，需要改动的包也不再是原样）；在 PluginManager 之外另设安装流程（重复了所有者那一个的获取、记录与清理）。
- **所需通用能力：** 一条识别规则与一个受管入口。规则是 `package.json` 的 `dsh` 成员，两个首期包都有，其中 `dsh.bundle.patch` 指向 harness 的 bundle patch，`dsh.client` 在有客户端半边时指向它。Cordis 依赖不是规则：旁会话包完全不声明，数据库包只声明可选的 `peerDependencies` `~4.0.4`。harness 的 bundle patch 不读不应用；受管入口取代它。入口写在插件文件根下 `.dsh/<插件 id>/entry.mjs`，属于该插件的记录，每次解析行时重写，随插件移除。安装、启用、记录与文件系统规则仍归 PluginManager，与其他插件一致。
- **对序号、批次、中断、重载的影响：** Timeline 无影响。包自己的入口变化后，`plugin.reload` 导入的是变化后的文件，因为入口的模块 URL 携带包入口的修改时间与大小（[PluginManager §3.4](PluginManager.md#34-重新加载)）。
- **实现归属与验证：** PluginManager（`crates/dim-app-server/src/plugin_manager`），由 [PluginManager](PluginManager.md) §2.1 与 §3.2 写明识别规则、入口路径与移除。用流测试验证：两个首期包从目录安装、获得入口、作为行装载，且其目录全部文件与修改时间不变；禁用并移除插件后不残留入口。

### 10.9 RuntimeManager：Node 启动携带桥接的解析钩子

- **受阻功能与规则：** 包对 Cordis 的 import 若解析到第二份，就无法与运行时共享上下文，因此桥接必须把这些 import 指向运行时装载的那一份（[§1](#1-结构)）。Node 运行时的启动命令归 RuntimeManager，它是 `node --import tsx src/runner.mjs`（`docs/design/plugins/RuntimeManager.md:114`，构造于 `crates/dim-app-server/src/session_typescript.rs:393-406`），自身没有钩子。
- **已排查的现有机制：** 在 `runner.mjs` 外包一层脚本（在所有者那条启动路径旁再开一条，并改变 `--import` 的相对解析）；包侧 import map（包文件只读，需要它的包也不再是原样）；依赖 Node 自身解析去选运行时那份（运行时的拒绝只能抓到解析自己副本的包，而 harness 的辅助包不是 Cordis）。
- **所需通用能力：** Node 启动在 `tsx` 之前增加一个桥接解析钩子的 `--import`，使 `registerHooks` 覆盖每个包的模块图。Node 按给定顺序应用 `--import`，`registerHooks` 与其可组合；运行时锁定的 Node 24.21.0 接受重复的 `--import` 标志。钩子自身的规则、它的 Cordis 目标与它解析的辅助包归桥接，写在 [§1](#1-结构)。
- **对序号、批次、中断、重载的影响：** Timeline 无影响。钩子在该运行时进程内对所有行生效，不只这些包，因此它必须不改动任何其他模块的解析；若包的 Cordis 区间不被运行时副本满足，以 `cordis_range` 拒绝（[附录 B](#附录-b-错误与恢复)），自己解析副本的以 `cordis_duplicate` 拒绝。
- **实现归属与验证：** RuntimeManager（`crates/dim-app-server/src/runtime_manager.rs`、`session_typescript.rs`），由 [RuntimeManager](RuntimeManager.md) §2.3 写明命令。用流测试验证：在顶层 import Cordis 的包按运行时的单一副本装载；自己解析副本的包仍以 `cordis_duplicate` 失败；其他语言的行不受影响。

除上述之外不向其他所有者提需求：受管入口的路径与内容、桥接的宿主 API 与 `dsh_typert` 命名空间都是本设计自己的，插件的动作使用现有的 `managementMethods`（[§7](#7-接口)）。

## 11. 本设计之外

- **需要 harness 自己的 HTTP 路由、同步全量历史读取或拦截点的包。** `dsh-memory-recall` 注入 harness 的 web server 并同步读取全量历史；`dsh-trellis` 拦截模型请求前的步与工具执行前的调用；`dsh-auto-retry` 处理模型请求错误，会成为重试的第二个所有者。三者都在装载时失败并指明缺失的契约。装载其中一部分比拒绝更糟：一个被静默省略的工作流守卫，是模型自以为存在的守卫。
- **同步历史读取与单次请求上下文视图。** 两者都是通用能力，放到二期与另一套 harness 的扩展面一起设计。那项工作不承诺支持上面三个包，它们各自还需要一项自己的机制。
- **对包进程内遇到的依赖显示 Pending。** 当一个包注入另一个包在同一运行时提供的服务时，等待由运行时自己的上下文持有，Dim 的 loader 此时把消费者行显示为 `Active`。桥接可以把它观测到的等待报告出来，本设计用它做诊断。loader 侧的这类名字显示是上游改进，这里不需要，也不是首期任何内容的前置条件。
- **在其他运行时中运行包。** 只有 Node 运行时携带桥接；为其他语言运行时编写的包不在范围内。
- **按包划分 Session 或第二套 Session 存储。** 子 Session 就是带父的普通 Session。
- **为包自己的视图提供客户端 UI。** 为 harness 客户端提供网页 UI 代码的包，在本设计中不渲染任何东西；它支持的界面是命令、工具与动作。
- **包市场、安装流程或更新策略。** PluginManager 的安装流程已覆盖包目录；本设计只新增对包的识别与它运行的受管入口（[§10.8](#108-pluginmanager识别-dsh-包并写受管入口)）。

## 附录 A. 桥接的宿主 API

字段名为 camelCase，与 Dim 服务声明一致。桥接自身的声明与它们一起生成。

```text
tools.register({name, description, inputSchema, risk, metadata?}, execute, {concurrency?})
  -> () => void                       // disposer；只会被撤回一次
execute({arguments, signal, sessionId, callId, config}) -> {content, structuredContent?, isError?}

systemPrompt.section({name, order, text}) -> () => void
systemPrompt.context({name, order, text, required?}) -> () => void   // 每次请求的动态文字
systemPrompt.variable({name, value}) -> () => void                   // 跨包共享

commands.register({name, description, argumentHint?}, handle) -> () => void
handle({branchId, args, signal}) -> {kind: "success" | "error", text}

subagents.startContinuable({provider, label, request: {parent, prompt}, signal})
  -> {childId, messageId}
subagents.startActivation(…) -> {result, dispose}
```

三者共同成立。`signal` 是 `AbortSignal`，在调用取消触发时中止；派发前就被取消的调用不会传给包（`crates/dim-agent-capabilities/src/tool_executor.rs:408-415`、`:447-456`）。disposer 幂等。拒绝以携带所有者错误码的类型化错误抛出，绝不用字符串。

## 附录 B. 错误与恢复

| 错误码 | 出现处 | 含义 |
| --- | --- | --- |
| `unsupported_host_api` | 行装载 | 包注入了本设计不提供的名字，或登记了不提供的拦截；消息写明是哪一个 |
| `package_loading` | 行装载 | 入口不可读或模块抛错；消息即模块错误 |
| `cordis_duplicate` | 行装载 | 包解析出了自己的 Cordis 副本 |
| `cordis_range` | 行装载 | 包对 Cordis 的 `peerDependencies` 区间排除了运行时那份 4.0.4；消息写明该区间 |
| `invalid_tool` | `tools.register` | 字段越界、名字已被占用，或缺风险类 |
| `invalid_command` | `commands.register` | 名字不在 `[a-z][a-z0-9_-]*`、描述为空，或与内置命令同名 |
| `duplicate_name` | `tools.register`、`commands.register`、`systemPrompt.*` | 本 Session 的另一个贡献已占该名字 |
| `session_closed` | 任何登记、`subagents.*` | Session 正在关闭或已消失；未登记任何内容 |
| `reloading` | 任何登记 | 所有者正在重载；该副本失败并随行重新挂载 |
| `rejected` | 任何 launch 或 input | 请求被拒，带原因 |
| `unknown_command` | `session.command.run` | 本 Session 没有该命令，包括刚被撤回的 |
| `interrupted` | `session.command.run`、运行中卸载 | 包代次已卸载；调用可能已产生效果 |

恢复说明：

- **登记回复丢失**的结果是贡献未登记并报告给副本；副本下次挂载会按行内状态重新登记，无需操作者干预。
- **行装载失败**不影响任何 Session，`/plugins` 中该插件状态写明缺失的契约。升级 Dim 后重载它即为恢复。
- **已采纳但在崩溃前未用上的取值**连同其输入一起在 Timeline 中排队，下一回合消费。
- **开场输入失败的子 Session** 由桥接在命令返回错误前删除，不留空 Session。
- **始终不结算的 stop** 通过重载插件结束，调用被截断为「可能已生效」；操作者动手前已被告知。

## 附录 C. 静态成本

本节不给测量值，每一行都是代码路径的性质。

| 路径 | 成本 |
| --- | --- |
| 装载一个包行 | 每个包一次模块导入、一次受管入口读取、一次定义登记。在 Session 打开之前没有任何每 Session 工作 |
| 在包已装载时打开 Session | 构建并挂载一份定义副本、一次为该副本键控的 Node 行、副本注入的每个 Session 服务一份租约（每份租约是所有者上的一个作用域内登记），以及每个贡献一次登记。全部是既有运行时操作，且都不写 Timeline |
| 包未装载时打开 Session | 无：注册表没有对应条目 |
| 包已挂载且取值未变的请求 | 对该 Session 提示词来源的一次边界调用，逐字节比较回复；无 Timeline 写入、无输入、无额外任务 |
| 取值变化的请求 | 同一次调用，加一个含配置与 Append 输入的原子批次、一个 InputQueue 批次，以及等待该输入被准入。提交锁绝不跨越包调用 |
| 一次工具调用 | 一次跨进程调用并等待；参数与渲染结果各单向序列化一次。并发调用只在外部等待上重叠：包内同步计算会阻塞它所在运行时的事件循环 |
| 一次命令执行 | 一次边界调用、可能一次 Session 创建、一次输入 |
| 一次空闲卸载 | 卸载副本：一次行卸载、每个注入服务一次租约归还、一份账本丢弃 |
| 每个被访问过的 Session 的内存 | 一个门面对象、每个包 `Map` 中一项；Session 卸载后其余皆无。副本的 fiber 与租约只在 Session 装载期间存在 |
| 包重载 | 旧代副本卸载，新代定义交出；成本是每个打开的 Session 一次卸载加一次挂载，不是每条分支 |

## 附录 D. 验收

实现的流测试覆盖以下各项，对象是两个首期包以及它们旁边的 fixture：

1. 从目录安装的包原样装载：目录内容与修改时间不变，工具声明进入请求，调用返回包渲染后的内容。
2. 两轮之间状态变化的包把新的 `<dim-state>` 块恰好在第二次请求中投递一次；取值未变不写任何东西；读取期间中断则以取消结束回合且不发起请求。
3. 忽略取消的包工具在 stop 下保持回合运行，重载插件后以「可能已生效」结算；等待期间显示提示。
4. 含连字符的命令可登记，`session.command.list` 列出它，带操作者取消执行它，disposer 之后返回 `unknown_command`。
5. 子 Session 记录父、按子会话列出、收到开场消息，并随父删除；被取消的 launch 不留 Session。
6. 数据库包的七个本地动作可由受保护本地连接调用，配对远端被拒，回复是包自己的内容。
7. Session 的空闲卸载与恢复保留包的每 Session 选择：操作者清空的选择在卸载后仍为空，卸载不发 `agent/disposed`，`agent/created` 不重复发送；结束原因与 `restored` 的验证归 [§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始)。
8. `session.close` 与 Session 删除各发一次 `agent/disposed`，且正是 [§10.7](#107-agents-与-session-生命周期作用域为何结束以及它如何开始) 的 Session 面告诉副本它处于两者中的哪一种、哪一次卸载；daemon 重启后首次装载发送 `agent/created`，带 `source: 'resume'`，读自 `sessionIdentity.restored`。
9. 每个被排除的包都在装载时失败并写明缺失的契约，没有任何一部分处于生效状态。
10. 重载包影响它挂载过的每个 Session，每个 Session 的下一次请求携带新代的声明。
