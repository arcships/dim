# 002：插件作用域划分与配置归属

决策状态：Proposed。适用于 `docs/design/` 中的插件设计及未来的插件。

## 背景

Dim 在三个作用域装配插件：进程、Session 和分支
（[single-tree](../../architecture/rust-agent/single-tree.md)）。插件的作用域决定它能依赖哪些服务、
能观察哪些事件、能写哪些事实，以及生命周期多长。

插件设计的早期版本为了减少 Capability 数量，把大部分新功能放到了进程作用域。这些功能仍需向
Session 提供工具、观察 Session 的事件并写入分支的输入，于是设计中加入了回连 Session 的接线：
扩展注册表、注册端口、Session 访问宿主、进程级 Session 事件订阅、进程级工具贡献、准入记录和
管理聚合插件。每一部分都只是为了连接放错作用域的插件而存在。

插件重载很少发生：只在结构性操作之后，例如安装、升级、启用或禁用插件，或崩溃。修改业务设置
不得触发任何重载。

## 决策

### D1. 作用域跟随状态及其生命周期的所有者

| 作用域 | 承载 | 现有示例 |
| --- | --- | --- |
| 进程 | 跨 Session 共享或比 Session 存活更久的资源与状态：连接、目录、存储、外部入口、跨 Session 的工作 | Provider, Mcp, SkillsCatalog, SessionStore, Transport |
| Session | Session 各分支共享的配置与 Capability：向 Session 提供的工具和提示词来源、对 Session 事件的观察、Session 自身的数据 | ToolRegistry, Skills, WorkspaceTools, TimelineTree |
| 分支 | 写入分支 Timeline 的事实、按序消费分支事件、绑定到某个 turn 的工作 | Loop, InputQueue, Approval, BackgroundExec |

只有当一个功能的各部分确实有不同所有者时，才把它拆到多个作用域，且每一部分恰好属于一个作用域。
没有跨 Session 共享状态的功能不是进程插件。

### D2. 不设接线模块

不存在只用于连接、聚合或适配其他插件的插件（注册表、端口、访问宿主、聚合或适配插件）。插件在
自身作用域通过现有机制集成：

| 需求 | 机制 |
| --- | --- |
| 向 Session 提供工具 | Session 级 `toolContribution`（具名工具服务）或 `SessionToolSource` |
| 提供指令 | PromptRegistry 提示词来源 |
| 观察 Session 的事件 | 现有的 Session 广播，由 Session 级插件监听 |
| 按序观察并响应分支事件 | 分支 Dispatcher，由分支级插件订阅 |
| 把内容放入模型上下文 | Session 或分支输入（被采纳的输入）、工具结果 |
| 在写入提交前对其作出处理 | 由分支级插件注册的 Timeline before 回调：在提交锁内同步变换草稿，不阻止写入，不等待外部工作，失败时提交原草稿（[原则 §4 第 3 条](../../architecture/rust-agent/principles.md)） |
| 在输入被应用前撤回它 | `input/withdraw {input_id, reason?}` 指令事件（由客户端或进程内调用方发出）；InputQueue 按序消费它，且仅在它持有该输入、尚未应用时写入 `input/deleted {input_id}` |
| 暴露关于某个 Session 的客户端方法 | Session 级 `sessionMethods` 服务，由 Rpc 经该 Session 分发 |
| 暴露宿主管理方法 | 进程插件在 Rpc 方法表中的 `managementMethods` 服务；已配对设备的连接不会获得管理方法 |
| 访问其他 Session | Agents 的 `sessions` 服务（AgentIndex 和 Session 公共接口），与 Collaboration 的做法相同 |
| 响应另一个进程插件的结果 | 事件通知，而非依赖 |

### D3. 依赖跟随作用域，并从扩展指向核心

插件依赖自身作用域或外层作用域的插件，依赖方向从扩展指向核心。核心部件（例如 Session 装配、
ToolRegistry、Rpc，以及 Mcp、SkillsCatalog 这类内容所有者）从不硬依赖可选插件：它在每次调用时
查找可选的提供方，或在自己装载时接入当时可用的提供方（之后起来的在下次重载时接入）。因此可选插件缺失、Pending、Failed 或正在重载时，只影响它自己
的功能（#34）。在这条规则之内，由于重载罕见且属结构性，重载传播到真正的依赖方（并使其在途 turn
失败）是可接受的代价；不为阻止它增加任何机制。运行时崩溃属于基础设施故障，与 App Server 崩溃
相同，不为它做专门设计。

### D4. 结构性配置与业务配置分离

| 类别 | 示例 | 位置 | 变更时 |
| --- | --- | --- | --- |
| 结构性插件配置 | 启用或禁用、运行时与包路径、节点绑定 | loader 行（issue #3 可编辑层） | 可能重载该行 |
| 业务配置 | 搜索服务、通知渠道、Hook 规则、定时任务、工作流定义、各使用者的辅助模型选择、MCP 服务器与 Skill 文档、外部 Agent 条目与桥接、网关监听设置、OCR 组件路径、exec shell | 所属插件的业务来源（Settings 来源或插件自有存储） | 数据更新并通知；使用时读取；不重载 |

### D5. 合同语言中立

rutis 按名称跨语言调用插件与服务（rutis-bridge 运行时、配合 `dyn HostDispatch` 的 `host_key(name)`、
`TypeKey::named` 与 `TypeKey::dynamic`），且 Rust 绑定可以从具名合同生成。只写成以
`TypeKey::of<T>()` 为 key 的 Rust trait 的合同，若没有手写的 Rust 绑定，TypeScript 或 Python
插件就无法满足它。因此，插件设计定义或使用的每个合同都以语言中立的形式规定：

| 要素 | 形式 |
| --- | --- |
| 标识 | 服务名（camelCase 字符串）、作用域（进程、Session 或分支）和实例 key（命名空间、工具名、种类） |
| 方法 | 名称、`sync` 或 `async`、请求与回复为可序列化数据（字段表或 JSON 结构） |
| 错误 | 数据：`{code, message}`，错误码有文档说明 |
| 取消 | 一个协议动作：取消请求，或消费方丢弃该调用 |
| 注册 | 由插件声明（其 `provides` 与 loader 行），而非宿主侧的 `AgentBuilder` 调用；消费方按服务名和作用域收集 |
| Rust 细节 | trait 签名、`TypeKey`、`Contributions<T>` 与 `AgentBuilder` 调用属于一种绑定，列在“Rust 绑定”附录中 |

规范服务：`sessionMethods`（Session，按命名空间）、`managementMethods`（进程，按命名空间）、
`toolContribution`（Session，按工具名）、`sessions`（进程）和 `modelKind`（进程，按种类）。它们
定义在[合同](../design/contracts/session-boundary.md)和所属插件文档中。运行时现有的类型化服务
（Timeline、Dispatcher、ToolHost 等）保持不变；把它们按名称暴露给非 Rust 的 Capability 部分是
issue #4 的工作。

## 新插件的作用域划分

| 插件 | 作用域 | 理由 |
| --- | --- | --- |
| Questions | Session（`ask_user` 工具、`session.question` 方法和运行中 `ask_user` 调用的等待者；没有事件） | 等待者就是 Session 级工具处理函数自己的调用；分支生命周期经 ToolExecutor 的取消令牌到达它 |
| Goal | Session（`goal_update` 工具）+ 分支（目标状态、续跑） | 目标属于某个分支，续跑按序跟随其事件 |
| ShellInput | Session | 命令在 Session 的工作区中运行并写入其输入 |
| Attachments | Session | 附件是 Session 数据 |
| Web | Session | 没有跨 Session 共享的状态；只提供工具 |
| SessionTitle | Session | 观察 main 上首条用户输入的采纳（`input/applied`）；标题由 SessionStore 保存 |
| Notifications | Session | 观察 Session 的事件并提供 `notify` |
| Hooks | Session（Hook 文档、执行器）+ 分支（向 Permission 提供工具建议、经 Approval 回答、与 InputQueue 的提示词暂扣；工具后、Stop 与 Session 启动 Hook） | 规则作用于 Session；执行发生在分支 |
| Scheduler | 进程（任务、触发器）+ Session（`schedule` 工具） | 调度跨越多个 Session |
| Workflow | 进程（运行）+ Session（`workflow` 工具） | 一次运行跨越多个 Session |
| ExternalAgent | 进程 | 外部 Agent 连接是共享的 |
| Modality | 进程 | 任务与 provider 连接是共享的 |
| PluginManager | 进程 | 安装以宿主为单位 |
| RemoteGateway | 进程 | 一个外部入口 |
| AcpGateway | CLI 进程 | 一个客户端 |

管理不是插件：管理方法存在于 Rpc 的方法表中，已配对设备的连接不会获得这些方法。

## 撤回

撤回以下内容：扩展注册表合同、注册端口及其宿主插件、Session 访问宿主、进程级 Session 事件订阅、
进程级工具贡献、准入记录（`RequiredCheck`）以及 Management 插件。

## 现有插件

现有插件遵循 D1。HostPlatform 通过每次执行命令时调用 `exec_shell()` 读取 exec shell 的业务
配置来遵循 D4；修改它不会重载插件。

## 影响

- 设计文档按此划分及语言中立的合同形式（D5）修订：总览、合同（仅保留描述现有插件补充的合同）以及每个插件文档。
- 可选的 Session 与分支插件只在启用时安装；它们不是每次装配中的占位。
- 罕见的结构性操作可能使在途 turn 失败；这一点被记录，而不被阻止。
