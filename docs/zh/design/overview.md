# 总体设计：未设计功能的插件

状态：草案

本文从 [CLI 功能清单](../../feature-list.md)出发，找出现有插件未覆盖的功能，并把新插件放入现有系统。每个新插件在 `docs/design/plugins/<Name>.md` 中设计。对现有插件的补充列在[附录 B](#附录-b-现有插件的补充)，由所属插件设计。

> **作用域规则**（[ADR 002](../adr/002-plugin-scope.md)）：插件的每一部分位于拥有其状态与生命周期的作用域（进程、Session 或分支），各部分只通过运行时已有的机制集成。不引入新机制。每个合同都是语言中立的（D5）：一个带作用域、实例 key 和纯数据方法的具名服务，因此任何语言的插件都可以提供或使用它（[§3.1](#31-合同形式)）。两份合同描述对现有接口的补充：[方法与工具贡献](contracts/session-boundary.md)（Rpc、ToolRegistry）和 [Session 访问](contracts/session-access.md)（Agents、Session 门面、InputQueue）。

正文涵盖前提、作用域图与划分、集成、目录、关键流程、约束、事件、阶段和待对齐问题。附录涵盖功能归属、现有插件的补充、实现语言、CLI 层、文档模板和向机制所有者提出的需求。

## 1. 范围与前提

| 项目 | 约定 |
| --- | --- |
| 终端形态 | 只设计 scrollback。功能清单中标为“仅全屏 TUI”的条目不在范围内 |
| 多模态 | 覆盖全部类别（图像、视频、语音、音乐、音频、3D 资产、动作、实时、OCR）。“仅特定 Desktop 构建”是可用性约束（[Modality](plugins/Modality.md)） |
| 不计划 | Todo 状态与 `plan` Skill |
| 动态事件 | 以 [#2](https://github.com/arcships/dim/issues/2) 为前提：插件在运行时注册自己的事件类型，注册跟随插件生命周期，Timeline 写入时只检查事件类型已注册 |
| 模型上下文 | 依据 [ADR 001](../../adr/001-harness-foundation.md)：模型只看到被采纳的输入和工具结果，插件事件不投影。#2 采用同一边界（[§9](#9-待对齐问题)） |
| 配置 | [ADR 002](../adr/002-plugin-scope.md) D4：结构性插件配置（启用、运行时与包路径、节点绑定）位于 loader 行（#3 可编辑层），可能重载该行。业务配置位于所有者的具名 Settings 来源，使用时读取，永不重载；目前 Settings 只服务 Rust 所有者，语言中立的服务稍后提供（[§6.4](#64-约定)，[路线图](../../design/roadmap.md) S8a/S8b）。项目文件保持自己的权威（ADR 001）；目录中的项目文件，例如 Workflow 的 `.dimcode/workflows/*.yaml` 和 Hooks 的 `.dimcode/hooks.json`，不是 Settings 来源，由其所有者经 FileStorage 读取；这类配置运行的命令不需要批准（§6.4 批准范围）。项目目录为 `.dimcode/` |
| 现有设计 | [时间总线](../../architecture/rust-agent/time-bus.md)、[设计规则](../../architecture/rust-agent/principles.md)、[单树装配](../../architecture/rust-agent/single-tree.md)、[生命周期](../../architecture/rust-agent/lifecycle.md)、[事件驱动流程](../../architecture/rust-agent/event-driven-flows.md)、[工具贡献](../../architecture/rust-agent/tool-contributions.md)以及 ADR 001 信任合同均适用 |

## 2. 作用域划分

```mermaid
flowchart TB
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
  classDef changed stroke:#e8590c,stroke-width:3px

  subgraph CLIP["CLI 进程"]
    cli["dim-cli"]
    acp["AcpGateway"]:::new
  end
  subgraph PROC["App Server 进程"]
    entry["ServerControl, Transport, Connection, Rpc"]:::changed
    store["FileStorage, SqliteStorage, SessionStore"]:::changed
    cfg["Settings, rutis-loader"]:::changed
    models["Provider, ManagedProviders, Aimux ModelHost, ConfiguredModels"]:::changed
    cat["Mcp, SkillsCatalog, HostPlatform, Trace"]:::changed
    agents["Agents, Collaboration"]:::changed
    pnew["Scheduler 任务, Workflow 运行"]:::new
    pext["ExternalAgent, Modality, PluginManager, RemoteGateway"]:::new
  end
  subgraph SES["Session"]
    sexist["ToolRegistry, PromptRegistry, Skills, Wait, WorkspaceTools, TimelineTree, McpSession, Collaboration Session 部分, SessionBroadcastEnd"]
    snew1["Questions ask_user, Goal goal_update, Hooks 规则与 Session 启动"]:::new
    snew2["ShellInput, Attachments, Web, SessionTitle"]:::new
    snew3["Scheduler schedule 工具, Workflow workflow 工具"]:::new
  end
  subgraph BR["分支"]
    bexist["Loop, InputQueue, Approval, Permission, ModelProvider, Prompt, ContextPreparation, ToolBinding, ToolExecutor, BackgroundExec, CollaborationTimeline, SessionBroadcast"]:::changed
    bnew["Questions 待答, Goal 状态与续跑, Hooks before 回调、工具后与 turn 结束"]:::new
  end
  rn["资源节点：FileStorage, HostPlatform, WorkspaceTools 宿主侧"]

  cli -->|"本地 RPC"| entry
  acp -->|"本地 RPC"| entry
  pext -->|"RemoteGateway：已认证入口"| entry
  entry -->|"解析 Session"| agents
  agents -->|"装载"| SES
  SES -->|"TimelineTree 装载"| BR
  pnew -->|"launch, input, idle"| agents
  SES -.->|"按 resourceNode 逐次调用"| rn
```

绿色部分为新增，橙色边框部分为变更。每个新插件部分位于拥有其状态的作用域。跨两个作用域的功能是一个插件包，每个作用域一个部分，与 SkillsCatalog、Skills 和 SkillsBranch 的做法相同。可选的 Session 与分支部分只在插件启用时安装，从不作为占位。

| 插件 | 进程 | Session | 分支 | 理由 |
| --- | --- | --- | --- | --- |
| Questions | — | `ask_user` 工具；`session.question.*` 方法；非交互 Session 中 `ask_user` 回答当前没有可以回答的对象 | 待答问题、`question/*` 事实，沿用 Approval 的做法 | 问题属于某个分支的 turn |
| Goal | — | `goal_update` 工具与 `session.goal.*` 方法，只写一条带 `goal:objective` 和/或 `goal:status` 块的 `input/message` | 目标状态由已消费的目标变更块按序号折叠得出；Goal 经 Dispatcher 消费 `turn/end`、`stop/interrupt` 或 `input/deleted` 时自己决定续跑 | 一个目标只属于一个分支：折叠只用该分支自己的事件，因此子分支和分叉 Session 从没有目标开始；续跑按序跟随其事件 |
| Hooks | — | 来自业务来源的规则；Session 启动 Hook | `input/message`（`user_prompt_submit`）与 `step/tool-call`（`pre_tool_use`）上的 before 回调；来自 Dispatcher 的工具后与 turn 结束 Hook | 规则作用于 Session；执行发生在分支 |
| ShellInput | — | `shell.*` Session 方法；在 Session 的工作区中运行并写入其输入 | — | 命令属于 Session |
| Attachments | — | `attachment.put`；通过 SessionStore 管理 Session 文件 | — | 附件是 Session 数据 |
| Web | — | `web_search`、`web_fetch` 工具 | — | 没有跨 Session 共享的状态 |
| SessionTitle | — | 通过 Session 广播观察首条用户输入的采纳（`input/applied`）；`session.rename`；标题由 SessionStore 保存 | — | 标题属于 Session |
| **已撤回（Notifications 已撤回）。** Notifications | — | 观察 Session 广播；`notify` 工具 | — | 触发源是 Session 的事件 |
| Scheduler | 触发器、运行历史；任务位于 Settings 业务来源 `schedule` | `schedule` 工具 | — | 调度跨越多个 Session |
| Workflow | 运行；定义位于 Settings 业务来源 `workflow` 或项目文件 | `workflow` 工具 | — | 一次运行跨越多个 Session |
| ExternalAgent | 连接、ModelHost kind | — | — | 外部 Agent 连接是共享的 |
| Modality | 任务、实时连接 | — | — | 任务与 provider 连接是共享的 |
| PluginManager | 市场、包 | — | — | 安装以宿主为单位 |
| RemoteGateway | 远程入口、设备 | — | — | 一个外部入口 |
| AcpGateway | CLI 进程 | — | — | 一个客户端 |

管理不是插件。管理方法是关于宿主而不是某个 Session 的客户端 RPC 方法（[定义](contracts/session-boundary.md)）；只有进程插件提供，作为 `managementMethods` 服务收集到 Rpc 的方法表中。Session 方法没有级别。Rpc 为本地连接提供 Session 方法和管理方法，从不向带 `deviceId` 的连接（已配对设备）提供管理方法，只在导出时向导出的 rutis 链接提供管理方法。

## 3. 基于现有机制的集成

插件从不新增接线插件（ADR 002 D2）。每项需求都对应插件自身作用域已有的机制：

| 需求 | 机制 | 使用方 |
| --- | --- | --- |
| 向 Session 提供工具 | Session 级 `toolContribution`（[方法 A.5](contracts/session-boundary.md#a5-工具贡献)）或 `SessionToolSource`（[工具贡献](../../architecture/rust-agent/tool-contributions.md)） | Questions, Goal, Web, Scheduler, Workflow |
| 提供指令 | PromptRegistry 提示词来源 | 现有 |
| 观察 Session 的事件 | 现有的 Session 广播（`session/<id>` 通道），由 Session 级插件监听 | SessionTitle, Hooks Session 启动 |
| 按序观察并响应分支事件 | 分支 Dispatcher，由分支级插件订阅 | Goal, Questions, Hooks 工具后与 turn 结束 |
| 把内容放入模型上下文 | 分支输入（raw 块、带 key 的块）或工具结果（[Session 访问](contracts/session-access.md#a2-来源输入约定)） | Goal, Hooks, ShellInput, Scheduler, Workflow |
| 在写入提交前对其作出处理 | 由分支级插件注册的同步 Timeline before 回调（[single-tree](../../architecture/rust-agent/single-tree.md)）：在提交锁内变换草稿，不阻止写入，不等待外部工作，失败时提交原草稿（[原则 §4 第 3 条](../../architecture/rust-agent/principles.md)） | 暂无（Hooks 待设计，#21） |
| 关于某个 Session 的客户端方法 | `sessionMethods`，由 Rpc 经该 Session 分发（[A.1](contracts/session-boundary.md#a1-方法贡献)） | Questions, Goal, SessionTitle, Attachments, ShellInput, Hooks, InputQueue（`session.input.withdraw`） |
| 宿主管理方法 | 进程插件的 `managementMethods`，由 Rpc 按调用查找 | 现有管理方法, Settings（业务配置，`settings.read`/`settings.update`，S8b，尚未实现）, Scheduler, Workflow, ExternalAgent, Modality, PluginManager, RemoteGateway |
| 访问其他 Session | Agents 的 `sessions` 服务（`launch`、`input`、`history`、`idle` 等），与 Collaboration 的做法相同（[Session 访问](contracts/session-access.md#a1-sessions-服务)） | Scheduler, Workflow |
| 响应另一个进程插件的结果 | 事件通知，而非依赖 | Scheduler 运行结果 |

```mermaid
flowchart LR
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000

  subgraph PROC["进程"]
    Rpc["Rpc"]
    Agents["Agents"]
    SchedP["Scheduler, Workflow"]:::new
    MM["managementMethods 所有者"]
  end
  subgraph SES["Session"]
    TR["ToolRegistry"]
    BC["Session 广播"]
    SP["Session 部分"]:::new
  end
  subgraph BR["分支"]
    TL["Timeline"]
    DP["Dispatcher"]
    IQ["InputQueue"]
    BP["分支部分"]:::new
  end
  Rpc -.->|"按调用查找"| MM
  Rpc -->|"sessionMethods"| SP
  TR -.->|"Session 装载时接入 toolContribution"| SP
  SP -->|"监听"| BC
  BP -->|"before 回调"| TL
  BP -->|"按序订阅"| DP
  BP -->|"输入"| IQ
  SchedP -->|"sessions"| Agents
  SP -->|"分支服务"| BP
```

箭头是依赖边，指向相同或外层作用域（D3）。依赖方向从扩展指向核心（D3）：核心收集方从不依赖可选的贡献方。ToolRegistry 接入 Session 装载时已可用的可选插件的工具（之后起来的在下次重载时接入），Rpc 在每次调用时查找管理方法所有者（虚线箭头，[方法 §3.2](contracts/session-boundary.md#32-管理方法)），Session 装配不等可选部件，因此未激活或正在重载的提供方只影响它自己的功能。重载只跟随罕见的结构性操作（安装、升级、启用、禁用、崩溃）。其传播（包括使在途 turn 失败）是被接受并记录的代价，不增加任何阻止手段。业务配置变更永不重载（D4）。

### 3.1 合同形式

设计定义或使用的每个合同都是一个具名服务（ADR 002 D5）：服务名（camelCase）、作用域、实例 key，以及标注为 `sync` 或 `async` 的方法，其请求、回复和错误都是可序列化数据。取消是一个取消请求，或消费方丢弃该调用。插件在其 `provides` 和 loader 行中声明它提供的内容；消费方按服务名和作用域收集。rutis 跨语言调用此类服务（rutis-bridge 运行时、`host_key(name)`、`TypeKey::named`），因此 TypeScript 或 Python 部分无需手写 Rust 绑定即可满足合同。Rust trait、`TypeKey`、`Contributions<T>` 和 `AgentBuilder` 调用只出现在各文档的“Rust 绑定”附录中。

| 服务 | 作用域 | Key | 方法 | 定义于 |
| --- | --- | --- | --- | --- |
| `sessionMethods` | Session | 命名空间 | `describe(): async -> MethodDecl[]`；`call(MethodCall): async -> MethodReply` | [方法 A.1](contracts/session-boundary.md#a1-方法贡献) |
| `managementMethods` | 进程 | 命名空间 | 同上 | [方法 A.1](contracts/session-boundary.md#a1-方法贡献) |
| `toolContribution` | Session | 工具名 | `declaration(): sync -> ToolDeclaration`；`execute(ToolCall): async -> ToolResult` | [方法 A.5](contracts/session-boundary.md#a5-工具贡献) |
| `sessions` | 进程 | — | `launch`、`input`、`lookup`、`history`、`withdraw`、`stop`、`idle`、`delete` | [Session 访问 A.1](contracts/session-access.md#a1-sessions-服务) |
| `modelKind` | 进程 | 种类 | `invoke(ModelRequest, target, ModelCallScope): async -> 模型流` | [ExternalAgent](plugins/ExternalAgent.md) |
| `scheduleJobs`（提议） | 进程 | — | 供 `schedule` 工具使用的 Session 到进程的任务操作 | [Scheduler](plugins/Scheduler.md) |
| `workflowRuns`（提议） | 进程 | — | 供 `workflow` 工具使用的 Session 到进程的运行操作 | [Workflow](plugins/Workflow.md) |

运行时现有的类型化服务（Timeline、Dispatcher、ToolHost 等）保持原样。把它们按名称暴露给非 Rust 的 Session 与分支部分是 [#4](https://github.com/arcships/dim/issues/4) 的工作；rutis 本身已经支持跨语言调用。

## 4. 插件目录

插件设计文档索引：[plugins/README.md](plugins/README.md)。

| 插件 | 作用域 | 职责 | 阶段 | 语言 | 文档 |
| --- | --- | --- | --- | --- | --- |
| Questions | Session + 分支 | `ask_user`；待答问题与回答；`session.question.respond` | P1 | Rust | [Questions](plugins/Questions.md)（需要重新设计） |
| ShellInput | Session | `!command` / `!!command`，作为 Session 方法 `shell.run/stop/list` | P1 | Rust | [ShellInput](plugins/ShellInput.md) |
| Attachments | Session | 大段文本的 `attachment.put`；Session 文件 | P1 | Rust | [Attachments](plugins/Attachments.md) |
| SessionTitle | Session | 由首条被采纳的用户输入生成标题；`session.rename` | P1 | TypeScript 优先 | [SessionTitle](plugins/SessionTitle.md) |
| Goal | Session + 分支 | 目标状态由 `goal:objective` 与 `goal:status` 输入块折叠得出；续跑；`goal_update`；`session.goal.*` | P2 | TypeScript 优先 | [Goal](plugins/Goal.md) |
| Web | Session | `web_search`、`web_fetch` | P2 | TypeScript 优先 | [Web](plugins/Web.md) |
| Hooks | Session + 分支 | 命令 Hook：Session 启动、提示词提交与工具前 before 回调、工具后、turn 结束 | P2 | Rust | [Hooks](plugins/Hooks.md)（待设计） |
| Scheduler | 进程 + Session | 定时任务；`schedule` 工具；运行结果通知 | P2 | TypeScript 优先 | [Scheduler](plugins/Scheduler.md) |
| Workflow | 进程 + Session | 跨 Session 的多阶段运行；`workflow` 工具 | P2 | TypeScript 优先 | [Workflow](plugins/Workflow.md) |
| Notifications | Session | 投递到渠道；`notify` 工具 | P2 | TypeScript 优先 | [Notifications](plugins/Notifications.md)（已撤回） |
| ExternalAgent | 进程 | 外部 Agent 作为 ModelHost provider kind | P3 | TypeScript 优先 | [ExternalAgent](plugins/ExternalAgent.md)（需要重新设计） |
| AcpGateway | CLI 进程 | 基于本地 RPC 的 ACP 服务器 | P3 | TypeScript 优先 | [AcpGateway](plugins/AcpGateway.md) |
| RemoteGateway | 进程 | 远程入口、配对、设备 | P3 | Rust | [RemoteGateway](plugins/RemoteGateway.md) |
| Modality | 进程 | 多模态任务与实时连接 | P3 | TypeScript 优先 | [Modality](plugins/Modality.md) |
| PluginManager | 进程 | 市场、包、`PluginContents` | P3 | Rust | [PluginManager](plugins/PluginManager.md) |

| 合同 | 所有者 | 定义 |
| --- | --- | --- |
| [方法与工具贡献](contracts/session-boundary.md) | Rpc, ToolRegistry | `sessionMethods`、`managementMethods`（及管理方法的定义）、`callChannel`、各入口获得的服务范围、`unknown_method`/`service_reloading`、现有管理方法的迁移与兼容、`toolContribution` |
| [Session 访问](contracts/session-access.md) | Agents, InputQueue | `sessions` 服务；带受限策略的 `launch`、`SessionConfig.interactive`、来源 raw 块、固定前缀读取、撤回（经 `sessionMethods` 提供的 `session.input.withdraw`） |

## 5. 关键流程

### 5.1 使用 Session 工具与 Hooks before 回调的工具调用

> 已撤回：Hooks 待设计，异步 before 回调已撤回（[#21](https://github.com/arcships/dim/issues/21)）。

```mermaid
sequenceDiagram
  participant LP as Loop（分支）
  participant TL as Timeline
  participant HK as Hooks before 回调（分支）
  participant TE as ToolExecutor
  participant AP as Approval + Permission
  participant TB as ToolBinding
  participant W as Web toolContribution（Session）
  LP->>TL: 追加 step/tool-call [web_fetch]
  TL->>HK: 待定写入，回调在提交锁之外执行
  HK->>HK: 使用时读取 pre_tool_use 规则，Hooks 允许
  HK-->>TL: 草稿不变
  TL->>TL: 提交 step/tool-call
  TL-->>TE: 经 Dispatcher 送达 step/tool-call
  TE->>AP: admit（风险 network）
  AP->>AP: 策略决定
  AP-->>TE: 允许
  TE->>TB: 从请求快照取 handler
  TE->>W: invoke
  W-->>TE: ToolResult 内容
  TE->>TL: 批次结算后写 step/tool-result
```

被 Hook 拒绝的调用留在草稿中，标记为 `skip: {reason}`（`step/tool-call` 数据合同的一个通用字段，提议中，归 Loop 与 ToolExecutor 所有）。ToolExecutor 对被跳过的调用不询问 Approval 与 Permission，也不执行，并在其正常批次的 `step/tool-result` 中写入错误结果（`reason` 作为错误文本）；其他调用照常进行（[Hooks](plugins/Hooks.md)）。Hooks 从不写 `step/tool-result`。before 回调从不使写入失败或阻止写入：回调持续更新一份兜底草稿（这里是所有尚未允许的调用都标记为跳过，通过草稿做失败关闭），回调无法完成时（注册方卸载、分支关闭、回调故障、超时，或排队了 `cancel_on` 中某种事件的写入，例如 `stop/interrupt`），运行时提交最新的兜底草稿。Approval 不变。`skip` 的处理是 Loop 与 ToolExecutor 的变更（[附录 B.1](#b1-session-与分支-capability)）；异步 before 回调是对运行时的需求（[附录 F](#附录-f-向机制所有者提出的需求)）。

### 5.2 由分支 Dispatcher 驱动的 Goal 续跑

```mermaid
sequenceDiagram
  participant TL as 分支 Timeline
  participant D as Dispatcher
  participant G as Goal（分支）
  participant IQ as InputQueue
  TL-->>D: seq N 处的 turn/end
  D->>G: turn/end，按序
  G->>G: 折叠已消费的 goal:objective 与 goal:status 块
  G->>G: 目标有效？其后无 stop/interrupt？未达上限？
  G->>IQ: 带 dim-goal 标记的 Queue 输入（自持任务）
  IQ->>TL: input/message，然后 turn/start
```

每次目标变更（设置、编辑、暂停、恢复、清除，以及模型通过 `goal_update` 报告完成或受阻）都是一条 `input/message`，其 raw 块带 key `goal:objective`（设置、编辑）、`goal:status`（设置、暂停、恢复、完成、受阻、自动暂停），或退役两者（清除）；续跑不带 key，只被计数。压缩（[ContextCompaction](https://github.com/arcships/dim/blob/roadmap/f1-production-compaction-host-compact-and-u/docs/zh/design/plugins/ContextCompaction.md)，F1；PR #108 合并前为分支链接）原样保留从 `retain_from` 起的事件（最后几轮），并重放位于它之前的每个 key 最新的存活块，因此模型保有当前目标与状态；压缩是 Loop 的一个步骤，不写 `turn/*`，因此从不触发续跑决策。工具和 `session.goal.*` 方法只写这条事件，不读 Goal 的状态，也不做决定。Goal 的状态是已消费的块按序号折叠的结果，没有例外：每个 key 最新的存活块就是当前目标与状态，因此压缩重放的内容总与折叠结果一致；只有 `goal:objective` 存活时目标才存在。Goal 经 Dispatcher 消费 `turn/end`、`stop/interrupt` 或 `input/deleted` 时自己决定是否续跑，只用已消费的状态；把写入登记为自持任务后返回，写入前再用已消费的状态复核。被删除的输入不改变状态。一个目标只属于一个分支：折叠只用写在该分支上的事件，从不用继承的前缀，因此子分支和分叉 Session 的 `main` 从没有目标开始，在那里的变更只影响该分支；Goal 消费新分支的 `branch/start` 时，若继承的前缀中仍有存活的目标 key，就作为自持任务在那里写一次 `clear`，使新分支上的模型不把父分支的目标当作自己的。`stop/interrupt` 会暂停目标。如果中断落在续跑输入之后，最多多跑一个 turn，模型会看到已暂停的块。

### 5.3 通过 Session 广播的观察者（SessionTitle）

```mermaid
sequenceDiagram
  participant IQ as InputQueue（main）
  participant BC as Session 广播
  participant ST as SessionTitle（Session）
  participant PR as Provider AuxiliaryModel
  participant SS as SessionStore
  IQ-->>BC: main 上首条用户输入的 input/applied
  BC-->>ST: 事件
  ST->>ST: 仅限无标题的交互式 leader Session
  ST->>PR: complete(model, 已采纳文本)
  PR-->>ST: 标题
  ST->>SS: 类型化写操作：标题，source auto，修订检查
  SS-->>BC: 给客户端的标题通知
```

手动标题永不被覆盖。被删除的输入从不产生标题，只有已采纳的文本发给辅助模型。标题是 SessionStore 的进程级状态，不是 Timeline 事实：SessionTitle 通过 SessionStore 的类型化写操作写入，SessionStore 发出 Session 广播通知供客户端使用（约束 2）。

### 5.4 经 Session 分发的 Session 方法（Questions）

```mermaid
sequenceDiagram
  participant CL as 客户端
  participant Rpc as Rpc
  participant S as Session
  participant QS as Questions sessionMethods
  participant QB as Questions（分支）
  participant TL as Timeline
  CL->>Rpc: session.question.respond {sessionId, branchId, callId, answers}
  Rpc->>S: acquire，sessionMethods session.question
  Rpc->>QS: call
  QS->>QB: branchId 上的分支服务，respond
  QB->>TL: question/answered（若在覆盖它的 stop/interrupt 之后则拒绝）
  QS-->>CL: 结果
```

### 5.5 经 Agents 的定时运行

> 下面的 Notifications 监听与运行结果通知随 Notifications 一并撤回。运行结果的通知是 Scheduler 自己的功能，在其设计复核时细化。

```mermaid
sequenceDiagram
  participant SC as Scheduler（进程）
  participant AI as Agents sessions
  participant S as 任务 Session（非交互）
  participant NT as Notifications 监听方
  SC->>AI: launch {策略以启动方为上限}
  AI-->>SC: sessionId
  SC->>S: 带 dim-scheduler 标记的 Queue 输入
  SC->>S: idle(main)
  S-->>SC: 在 seq N 处空闲
  SC->>SC: 读取结果，记录历史
  SC-->>NT: 运行结果事件通知（无依赖）
```

`schedule.ended` 通知由运行所属 Session 的 Notifications 实例投递；通知发布时该 Session 未装载，则这条通知丢失。这一点被接受（[Notifications](plugins/Notifications.md)）。

### 5.6 Collaboration 委派给外部 Agent

> 已撤回：下面的审批路径随 ExternalAgent 重新设计一并撤回（[#28](https://github.com/arcships/dim/issues/28)）。

```mermaid
sequenceDiagram
  participant L as Leader 分支（agent 工具）
  participant C as Collaboration
  participant M as 成员分支（ModelProvider, Approval）
  participant X as ExternalAgent（ModelHost kind）
  participant EA as 外部 Agent 进程
  L->>C: agent 请求
  C->>M: collaboration/request，成员输入
  M->>X: 以 ModelCallScope 调用
  X->>EA: 经 ACP 或 CLI 发送 prompt
  EA-->>X: 权限请求
  X-->>M: approval_ask 流条目
  M->>M: ModelProvider 经 Approval 准入，approval/request
  M->>C: 成员中有待处理审批
  C-->>L: 转发给 leader
  L->>C: leader 模型通过 agent 工具决定
  C->>M: approval/respond
  M->>X: answerApproval(outcome)
  X->>EA: 允许一次或拒绝一次
  EA-->>X: 输出
  X-->>M: 模型输出
  M-->>C: collaboration/result
  C-->>L: 结果
```

权限请求以模型流中的 `approval_ask` 条目到达成员分支；ModelProvider 经该分支的 Approval 对其准入，并以 `answerApproval` 作答，因此它成为普通的 dim 审批（现有的 `approval/request` 与 `approval/respond` 事实）。Collaboration 成员的审批交给 leader 的模型，没有 leader 时由用户决定。从不选择 `always` 选项。

## 6. 通用约束

### 6.1 集成方式

[§3](#3-基于现有机制的集成) 中的表是穷尽的。只有当某作用域上的状态或行为没有现有插件拥有时，才新建插件部分；现有职责内的变化记录在[附录 B](#附录-b-现有插件的补充)。

### 6.2 约束

1. **最少事件**：只为影响后续行为、且无法从工具调用与结果、`input/*`、`turn/*` 或 `stop/interrupt` 推导出的结果新增事件（时间总线规则 3）。进程输出进入诊断。
2. **所有权**：声明事件类型即定义其数据合同。生产方使用所有者的公共服务。谁启动工作，谁写其开始与终态事实。决策只使用已消费的事实，从不使用其他插件的内存。没有 Timeline 的进程级状态（SessionStore 标题、Provider 连接条目、Agents profile、Settings 业务来源、插件存储）从不进入 Timeline：写入通过所有者的类型化公共写操作，需要在 UI 上显示时由所有者发出 Session 广播通知，供客户端使用。
3. **外部入口**：RPC 从不写事件。它调用所属服务，由该服务写入（先例 `approval.respond`）。Rpc 只做外部边界校验（参数格式、`sessionId` 授权）；图像嗅探、大小限制、模型能力等功能检查归各自所有者（上传方法的所有者、客户端，或构建请求时的 ModelProvider），不放在 Rpc、before 回调或 `session.submit` 中。竞争由 Timeline 序号裁决。
4. **重载与副作用**：已开始但未终止的工作结算为“未能在恢复后存活”，且从不重新执行。允许通过轮询已保存的 handle 恢复对外部任务的观察。
5. **作用域**：每个部分只依赖自身或外层作用域（D3）。Session 与分支部分读取所属 Session 的 `SessionConfig`。进程部分只通过 Agents 的 `sessions` 服务读取 Session。
6. **中断**：插件先取消在途工作并释放资源，再写终态事实（[中断](../../architecture/rust-agent/interrupt.md)）。
7. **准入**：新工具声明风险类别。不新增 Permission 类别：Permission 不认识的风险按未分类处理，ReadOnly 拒绝、WorkspaceWrite 询问、FullAccess 允许（`permission.rs`）；工具仍可声明 `persistent_configuration` 这类描述性的风险。由自动化启动的 Session 以启动方的策略为上限，Scheduler 任务和 Workflow 定义可以进一步收窄。由管理方法启动的运行除非另有声明，默认只读，并受宿主配置约束。`workflow` 使用 `agent` 工具的风险类别。
8. **依赖方向与重载代价**：核心部件从不依赖可选插件；可选插件重载或失败只影响它自己的功能（D3）。重载传到真正的依赖方时，可能使其在途 turn 失败；这一点被接受，并记录在每个插件的冲突表中，从不加以阻止。
9. **管理面**：管理方法（`account.*`、`provider.*`、`mcp.*`、`skills.*`、S8b 加入后的 `settings.read`/`settings.update`，以及新进程插件的方法）只由进程插件通过 `managementMethods` 提供。Session 方法没有级别；`shell.*` 是普通 Session 方法。本地 RPC 服务两类方法，RemoteGateway 只服务 Session 方法，rutis 链接只在显式导出时服务管理方法（宿主管理员权限）。不存在身份分级。`hello`、`daemon.*`、`agent.*` 和现有的 `session.*` 方法保持为 Rpc 内建方法；`daemon.shutdown` 是内建方法，从不提供给已配对设备（带 `deviceId` 的连接）。
10. **远程 Session 方法**：已配对设备可调用所有 Session 方法，包括 `session.permission.update` 和 `shell.*`（兼容性变更，[方法 A.4](contracts/session-boundary.md#a4-迁移与兼容性)）。
11. **在途工作**：没有共享注册表。每个所有者提供自己的查询与停止，由 CLI 汇总。
12. **撤回**：由 InputQueue 拥有的单一合同。`input/withdraw {input_id, reason?}` 是一个指令事件，可由任何合法生产方写入：客户端经 `session.input.withdraw`（InputQueue 通过 `sessionMethods` 提供的 Session 方法），进程调用方经 `sessions.withdraw`。InputQueue 按序消费它；只有当它仍持有该输入且尚未应用（无 `input/applied`）时，才写入结果事实 `input/deleted {input_id}`，且永不应用该输入。否则（未知、已应用或已删除）不写入任何内容，调用方收到的回复为 `notHeld`。已应用的输入无法撤回，Loop 不变。“撤回并作为 Steer 重新提交”是一个原子批次。每个待处理输入推导都排除已删除的输入，它们从不进入模型上下文（其带 key 的块从不生效）。
13. **提交前处理**：插件只能通过 Timeline before 回调，在另一个所有者的写入提交前对其作出处理，回调同步变换草稿，不阻止写入，不等待外部工作；该写入的消费方不做任何特定于插件的改动。
14. **低业务耦合**：raw 块标记与 key、配置 key 和方法命名空间都带所有者前缀。消费方为自己的任务选择的模型是它自己的配置，显式传给共享服务；共享服务不保存按消费方区分的策略。收集方从不点名贡献方。
15. **交互式 Session**：`interactive` 表示 Session 有可交互的对象，能回答它的提问和审批，可以是用户，也可以是另一个 Session 或分支的 agent（[Session 访问 A.3](contracts/session-access.md)）。`session.create` 设置 `interactive`（客户端只在其为 false 时发送）。Exec 和自动化创建非交互 Session；ACP Session 是交互的，因为编辑器回答它的审批。装配不依赖 `interactive`，因此 fork 前后工具集合与提示词前缀保持一致（#30）；这类 Session 中 `ask_user` 回答当前没有可以回答的对象。SessionTitle 只作用于交互式 leader Session。

### 6.3 多节点

rutis 节点通过链接相连，导出和导入服务，在对端运行插件（`peer:<node>/<plugin>`）并转发事件（[rutis 节点](https://github.com/arcships/rutis/blob/main/docs/guide/nodes.md)，[#5](https://github.com/arcships/dim/issues/5)）。

| 规则 | 内容 |
| --- | --- |
| 控制节点 | 承载 App Server：Timeline、Session、配置来源、Provider、调度 |
| 资源节点 | 宿主资源所在之处（文件、进程、shell、隔离、本地 CLI）。每个 Session 将绑定一个，记为 `SessionConfig.resourceNode`，创建时固定；这是 P3 新增的可选字段，缺省即控制节点；P1/P2 中每个 Session 都使用控制节点 |
| 宿主服务 | Session 资源节点上的 FileStorage、HostPlatform、隔离以及 WorkspaceTools 宿主侧，在每次调用时按 Session 的 `resourceNode` 查找（#38）。没有 Session 的进程插件（Modality OCR、ExternalAgent 桥接）在自身业务配置中指定节点，使用时读取；ExternalAgent 的桥接在其重新设计中改为 loader 行（#37） |
| 节点绑定数据 | 隔离配置和外部 Agent 目录以节点和路径标识 |
| 离线 | 对离线资源节点的调用以 `node_unavailable` 失败；依赖方不重载，也不进入 Pending。节点服务在每次调用时按 `resourceNode` 查找，因此节点离线只让这些调用以 `node_unavailable` 失败；不向 rutis 提需求（#38）。链接断开时，由资源节点自己的 HostPlatform 结束它启动的进程 |
| 认证 | 复用 rutis 链接 token；管理方法只在配置后才导出 |
| 未来 | 跨控制节点协作不在此设计 |

### 6.4 约定

| 主题 | 约定 |
| --- | --- |
| 上下文 | 模型上下文只经由工具结果和输入；常驻规则经由提示词来源或带 key 的 raw 块 |
| 配置 | 每项一个来源，只经其所有者的接口写入。结构性条目（例如 `enabled`）位于 loader 行，只由 PluginManager 的结构性操作（安装、启用、禁用、更新）在 loader 可编辑层（#3）上修改；插件不写自己的结构性字段。业务条目位于 Settings 来源：一个所有者有一个或多个具名来源，每个来源只保存一种文档类型（MCP 拥有 `mcp` 和 `mcp-projects`）；来源名使用 `[a-z0-9_-]`。Settings 不依赖任何插件。**S8a（Rust，已实现）：** 所有者为每个来源实现 `BusinessDocument`（名称、用户文件、可选的项目文件、`validate`、`Default`）；读取返回 `{revision, document}`，更新按 revision 做 CAS。Settings 没有自己的订阅接口：写入成功后在通道 `settings_changes(source)`（`settings/<source>`）上发出 rutis 事件 `settings.changed` `{source, scope, revision}`，用户和项目作用域都覆盖，从不带文档内容；新授权的项目在 `project_authorizations()` 上发出 `settings.project_authorized` `{root, cwd}`。使用方用 `ctx.events().on` 监听，监听器随其所在插件一起存亡；先读一次，再跟随事件。`SettingsChanged` 与 `settings_changes` 声明在 `dim-agent-capabilities` 中，任何 crate 中的所有者（MCP、Skills 目录）都监听同一个类型。校验和默认值只在 Settings 边界应用，即写入时和读取持久化文件时（文件缺失即默认文档）；使用方按交付的样子使用文档，不再校验，也不自带默认值。新的原生来源是 v13 存储中增量的 `user/<source>` 行。**以后（S8b，随 TypeScript 桥接 C4；尚未实现）：** 语言中立的具名 `settings` 服务，`read(source, scope) -> {revision, document}`、带 CAS 的 `update(source, scope, expectedRevision, document)`，以及同样的 `settings.changed` 事件，文档为 JSON；插件清单声明每个来源的 JSON Schema、默认值、允许的作用域和仅限用户的字段，`BusinessDocument` 仍是 Rust 绑定。写入走两条路径之一：客户端管理方法 `settings.read`/`settings.update`，或所有者的类型化写入（包括代其工具发起的写入）；所有者代其工具发起、改动仅限用户字段的写入会被拒绝。JSON Schema 无法表达的规则（例如 CIDR）是所有者在使用时给出的诊断。凭据是 `secretRef` 风格的 `*Ref` 字段，值为不透明字符串，写入时检查非空，由唯一的解析器 `credentials.resolve(owner, reference)` 解析；它与 Provider 的 `secretRef` 解析器统一，或者保留 Provider 的 AWS 结构化情形并记录原因。Scheduler 任务（来源 `schedule`）和保存的 Workflow 定义（来源 `workflow`，一个把名称映射到定义的文档，另含项目启用记录）是 Settings 文档。目录中的项目文件（Workflow 的 `.dimcode/workflows/*.yaml`、Hooks 的 `.dimcode/hooks.json`）不是 Settings 来源：其所有者在项目文件权威下经 FileStorage 读取。运行记录位于 `pluginFiles`（扩展后）。RemoteGateway 监听设置、Modality 的 OCR 组件路径和 ExternalAgent 桥接都是业务配置 |
| 批准范围 | Approval 与 Permission 只管 agent 决策引起的动作（模型的工具调用）。按用户或项目配置运行、而不是由 agent 决定的命令（连接时启动的 MCP stdio 服务、Hook 命令、工作流文件、插件）不属于 agent 体系，由该配置自己负责：启用即运行，不需要批准。功能开关（启用、禁用）是普通配置，与批准无关；来自项目来源的条目默认关闭，只能由用户本机配置启用。没有宿主本地批准（[ADR 003](../../adr/003-approval-scope.md)）；原有的 `mcp.approve`/`mcp.revoke` 及其批准记录已删除（[附录 B.2](#b2-app-server-进程插件)） |
| 安全设置 | 宿主安全设置从不进入 Timeline，回放和 fork 从不恢复或放宽它们。权限预设是 Timeline 事实（`permission/changed`），由子分支继承，fork 时随事实一起复制 |
| 持久化 | 进程存储使用 `pluginFiles`（扩展后）：每插件根目录、UTF-8 与二进制原子替换、锁、格式版本（[受信插件存储](../../../crates/dim-app-server/README.md#trusted-plugin-storage)）。Session 文件位于 SessionStore 之下 |
| 协议 | Rpc 内建方法优先于贡献的方法 |
| 国际化 | 用户可见文本属于 CLI；模型可见文本属于提示词来源 |

## 7. 新增 Timeline 事件

| 事件 | 写入方 | 必要性 |
| --- | --- | --- |
| `question/asked {call_id, turn_id, request_id, questions}` | Questions（分支） | 待答状态无法推导，因为结果只在整个批次完成后才发布（先例 Approval） |
| `question/answered {call_id, answers, declined}` | Questions（分支），经 `session.question.respond` | 通过序号与中断排序 |
| `question/cancelled {call_id, reason}` | Questions（分支） | 在中断、分支关闭或重载时结束待答问题 |
| `input/withdraw {input_id, reason?}` | 客户端（`session.input.withdraw`，由 InputQueue 提供）、进程调用方（`sessions.withdraw`） | 一个通过序号与采纳排序的指令；可选的 `reason`（通用文本，不带业务含义）是客户端为被删除输入显示的内容 |
| `input/deleted {input_id}` | InputQueue | 结果事实：被持有的输入永不被应用；队列状态必须可重建 |

三行 `question/*` 正在复审：Questions 需要重新设计（[Questions](plugins/Questions.md)），这些事件可能被删除。

| 不新增的事件 | 改用什么 | 原因 |
| --- | --- | --- |
| `goal/*` | 带 key 的 raw 块，包括终态块 | 目标状态是输入 |
| `shell/*` | `!command` 结果写为 Append 输入 | 不是分支在途工作 |
| **已撤回（Hooks 待设计，#21）。** `hook/*` | 工具拒绝表现为 `step/tool-call` 草稿中该调用上的 `skip: {reason}`（由 ToolExecutor 写错误结果），提示词拒绝表现为 `input/withdraw {input_id, reason}`（随后为 `input/deleted`），以及注入的输入、诊断；Hooks 不声明事件（[Hooks](plugins/Hooks.md)） | 不是核心事件 |
| `external_agent/*` | Collaboration 事实、成员事件、审批事实 | 外部 Agent 是模型连接 |
| `session/titled` | SessionStore 进程级状态，经其类型化操作写入，并发出 Session 广播通知 | 不影响分支行为 |
| `workflow/*`、定时运行结果 | 工具结果、报告输入、`pluginFiles` 存储、事件通知 | 进程级结果 |
| 排队消息转 Steer | `input/withdraw` + 新 Steer 输入，在一个批次中 | 无需单独事件 |

## 8. 阶段

前置条件总是落在早于其消费方的阶段。

| 阶段 | 内容 |
| --- | --- |
| P0 基础 | 由 `session.create` 设置的 `SessionConfig.interactive`；带 `launch` 的 `sessions` 服务；`sessionMethods` 分发；Rpc 表中的 `managementMethods` 及现有管理方法的迁移；ToolRegistry 中的 `toolContribution`；带变更通知的 Settings 业务来源（S8a）；语言中立的 `settings` 服务与凭据解析器（S8b，随 C4）；`pluginFiles`（扩展：二进制、格式版本）；Provider `AuxiliaryModel`（会话外单次模型调用）；SessionStore 元数据与 Session 文件；FileStorage 列举与删除；HostPlatform argv 辅助 |
| P1 核心交互 | Questions（需要重新设计）、ShellInput、Attachments、SessionTitle；InputQueue 撤回；Permission 规则；模型参数；SessionStore 删除、清理与 fork；HostPlatform 隔离；#4 为非 Rust 部分按名称暴露运行时的类型化服务；CLI 子命令、图像转换、`/export`、`exec --json` |
| P2 功能扩展 | Goal、Web、Scheduler、Workflow；ModelProvider `ModelCallScope`；Collaboration 成员审批作为输入送到 leader；Mcp OAuth；生产环境压缩宿主 |
| P3 外部集成 | ExternalAgent、AcpGateway、RemoteGateway 及 Transport `EntryHost` 与 `ConnectionEntry {name, deviceId}`、Modality；PluginManager；资源节点（按 `resourceNode` 在每次调用时查找服务） |

ADR 002 中记录的 HostPlatform exec shell 变更（作为业务配置在每次 spawn 时读取）是独立工作，不阻塞任何阶段。

## 9. 待对齐问题

| 问题 | 状态 |
| --- | --- |
| 事件如何进入上下文 | 已决定：只有被采纳的输入和工具结果（ADR 001）。#2 已于 2026-10-15 修订为同一口径 |
| MCP stdio 隔离 | 待定：MCP stdio 进程是否默认隔离 |
| 远程访问取舍 | 待定：认证、断连与 TLS 部署，与 [#5](https://github.com/arcships/dim/issues/5) 相关（[RemoteGateway](plugins/RemoteGateway.md)） |
| TypeScript Session 部分 | 待定：SessionTitle、Goal、Web 以及 Scheduler 和 Workflow 的 Session 部分按名称提供语言中立合同；其中还使用运行时类型化服务（Timeline、Dispatcher、InputQueue）的部分，需要 #4 按名称暴露这些服务，否则从 Rust 开始 |
| **已撤回（Notifications 已撤回）。** Notifications 获取定时任务结果 | 已决定：任务 Session 是非交互的，因此 Scheduler 发布运行结果通知，Notifications 监听它，二者无依赖；通知由运行所属 Session 的 Notifications 实例投递，该 Session 未装载时丢失（§5.5） |
| 定时任务存活 | 已决定：守护进程必须存活（[Scheduler](plugins/Scheduler.md)） |
| FullAccess 与新类别 | 已决定：自动允许（约束 7） |
| 外部 Agent 权限 | 重新设计（#28）：请求作为一条 `input/message` 写入主模型所在分支，主模型通过工具决定（[ExternalAgent](plugins/ExternalAgent.md)） |
| 外部会话延续 | 提议：从分支历史重新初始化（[ExternalAgent](plugins/ExternalAgent.md)） |
| fork 时的权限预设 | 已决定：不特殊处理；预设随事实一起复制，应用层如需不同的预设，在 fork 后自行设置（#26） |
| 重载传播 | 已决定：罕见结构性操作的可接受代价（[ADR 002](../adr/002-plugin-scope.md) D3） |
| **已撤回（Hooks 待设计，#21）。** 异步 before 与同步写入方 | 已决定：写入方的追加在被接受时返回，提交结果经 Dispatcher 报告，因此 Loop 等写入方不会阻塞。属于异步 before 需求（[附录 F](#附录-f-向机制所有者提出的需求)） |
| 由 Collaboration 成员运行 Workflow 阶段 | 未来功能：跨阶段复用成员的上下文。目前每个阶段都在启动的 Session 中运行（[Workflow](plugins/Workflow.md)） |

## 附录 A. 功能归属

“现有”表示由现有插件覆盖。“扩展”表示现有职责，列于附录 B。“新增”表示新插件（§4）。“CLI”指附录 D。

| 领域 | 功能 | 归属 |
| --- | --- | --- |
| Provider | OAuth / API-key Provider、`/connect`、`/models` | 现有：Provider、ManagedProviders、OAuth 插件、Aimux |
| Provider | `/effort`、`/fast` | 扩展：Prompt、ModelProvider |
| Provider | 子 Agent 与辅助模型 | 扩展：Provider（`AuxiliaryModel`） |
| Session | `/new`、`/resume`、Session 列表、`/history` | 现有：Agents、SessionStore |
| Session | `/timeline`、`/export` | CLI |
| Session | Session 标题 | 新增：SessionTitle |
| Session | fork、删除、清理 | 扩展：SessionStore、Agents |
| Session | 请求快照导出 | 扩展：Trace |
| Session | 文件与对话回滚 | 不在范围内（仅全屏 TUI） |
| 交互 | 发送、排队、自动发送、Steer、中断 | 现有：InputQueue、Loop |
| 交互 | `Ctrl+S`、`/queue`、召回、删除排队消息 | 扩展：InputQueue（撤回） |
| 交互 | 多行编辑、历史、补全、引用、粘贴、剪贴板、复制、链接 | CLI |
| 交互 | 大段粘贴、混合附件 | 新增：Attachments；CLI 把图像转换为图像块 |
| 交互 | `!command`、`!!command` | 新增：ShellInput |
| 交互 | 流式输出、运行状态、文件变更反馈 | 现有事件，由 CLI 渲染 |
| 交互 | 工具审批 | 现有：Approval、Permission |
| 交互 | 用户提问 | 新增：Questions |
| 交互 | Todo 状态 | 不计划 |
| 交互 | 查看与停止后台任务 | 各所有者（BackgroundExec、Collaboration、Workflow、ShellInput），由 CLI 汇总 |
| 系统 | `/exit`、`/theme`、`/keys`、升级 | CLI |
| 系统 | `/settings` | 每项属于各自插件；由 CLI 汇总 |
| 系统 | `/usage` | 扩展：ContextPreparation、ManagedProviders |
| 系统 | `/language` | CLI；扩展：PromptSources |
| 系统 | `/debug`、调试日志 | 现有：Trace 诊断 |
| 系统 | `/stats`、鼠标设置 | 不在范围内 |
| Agent 行为 | `/agent` | 扩展：Agents、Prompt |
| Agent 行为 | `/goal`、续跑、Goal 模式 | 新增：Goal（模式由目标状态覆盖） |
| Agent 行为 | `/approvals`、命令规则 | 扩展：Permission、Settings |
| Agent 行为 | 三个权限级别 | 现有：Permission |
| Agent 行为 | 沙箱 | 扩展：HostPlatform |
| Agent 行为 | `/compact`、自动压缩 | 扩展：ContextPreparation |
| Agent 行为 | `/scheduled`、定时任务 | 新增：Scheduler |
| 扩展 | `/plugins`、`/reload-plugins`、市场 | 新增：PluginManager |
| 扩展 | `/skills`、`/reload-skills`、Skill 来源 | 现有：SkillsCatalog、Skills；由 PluginManager 安装 |
| 扩展 | `plan` Skill | 不计划 |
| 扩展 | `AGENTS.md` | 现有：PromptSources |
| 扩展 | `/mcp`、MCP 连接 | 现有：Mcp、Settings；扩展：HTTP/SSE OAuth（[MCP OAuth](appserver/mcp-oauth.md)） |
| 扩展 | `/workflow`、`/workflow:save` | 新增：Workflow |
| 扩展 | `/external-agents` | 新增：ExternalAgent，经 Collaboration 委派 |
| 扩展 | 并行子 Agent | 现有：Collaboration；扩展：Provider 模型选择 |
| 扩展 | Hooks | 新增：Hooks |
| 扩展 | Web 搜索与抓取 | 新增：Web |
| 扩展 | Slack 与飞书 | 各平台各自的插件与工具；没有通用渠道，也没有 `notify` 工具 |
| 工作区工具 | `read`、`write`、`edit`、`glob`、`grep`、`exec`、`wait` | 现有：WorkspaceTools、BackgroundExec、Wait |
| 远程访问 | JSON-RPC 常驻服务 | 现有：Transport、Rpc |
| 远程访问 | ACP | 新增：AcpGateway |
| 远程访问 | `/web`、`/remote`、手机配对、远程 Session | 新增：RemoteGateway |
| 远程访问 | VS Code | CLI |
| CLI 命令 | 子命令、`exec --json`、`exec --verify` | CLI，调用各插件的 RPC |
| 多模态 | `image generate/edit/enhance`、`modality`、`modality task` | 新增：Modality |
| 多模态 | `image read` | 现有：视觉模型通过 `read` 读取图像 |
| 多模态 | `video`、`speech`、`music`、`audio`、`asset`、`motion`、`realtime` | 新增：Modality（由 provider 执行） |
| 多模态 | `ocr` | 新增：Modality（由宿主执行；OCR 运行时组件） |

## 附录 B. 现有插件的补充

**补充**是增加服务、方法或配置。**变更**是改变现有行为或合同，需要兼容性说明。通过 #2 声明现有事件类型属于 #2 的工作，`input/message` 不变。

### B.1 Session 与分支 Capability

| 插件 | 类型 | 内容 | 理由 |
| --- | --- | --- | --- |
| **已撤回（Hooks 待设计，#21）。** Timeline（运行时） | 补充 | `Timeline::before` 增加异步回调。当某分支写入匹配已注册的异步回调时，该写入保持其在序号顺序中的位置，只在回调完成后才提交；该分支上后续的写入排在它之后；等待期间不持有提交锁。回调收到草稿（其事件 id 在写入占位时即已固定），返回变换后的草稿和/或追加到同一原子批次的事件（只能是发给事件所有者的指令或注册方自己声明的事件，不能是别的模块的结果事件）；随后同步 `before` 处理器像现在一样在提交锁内应用，追加的事件不再被匹配。before 回调从不使写入失败或阻止写入：回调开始时返回一份兜底草稿，运行期间持续更新；回调无法完成时（超时、回调故障、注册方卸载、分支关闭，或排队了 `cancel_on` 中某种事件的写入），运行时提交最新的兜底草稿。注册选项全部由注册方决定：`timeout`；`cancel_on`（一组事件种类，其排队写入会取消待定回调）。写入方的追加在被接受时返回，提交结果经 Dispatcher 报告，因此 Loop 等同步写入方不会阻塞。回调从不等待其自身分支上的写入。通用：不含业务语义。五项说明见 [Hooks §9](plugins/Hooks.md#9-向机制所有者提出的需求) | Hooks |
| **已撤回（#28）。** Approval | 补充 | 在模型步骤打开期间，接受宿主来源的准入（由 ModelProvider 针对 `approval_ask` 流条目发起），写入普通的 `approval/request`、`approval/respond`、`approval/cancelled` 事实；通用，不了解 model kind | ExternalAgent（首个使用方） |
| InputQueue | 变更 | 按序消费 `input/withdraw {input_id, reason?}`（`reason`：提议的通用可选文本，最多 2 KiB，任何生产方都可写入，由客户端显示，例如 Hooks 拒绝提示词；InputQueue 做决定时忽略它；不带它的事件不变；设计规则 §4 第 9 条允许必需事件带描述性字段；设计见 [Hooks](plugins/Hooks.md)），只为它仍持有且未应用的输入写入 `input/deleted {input_id}`（否则不写入，`notHeld`）；经 `sessionMethods` 提供 `session.input.withdraw`（命名空间 `session.input`，提议中）；“撤回并作为 Steer 重新提交”作为一个批次；队列状态从事件重建（[Session 访问 A.4](contracts/session-access.md#a4-撤回)） | `Ctrl+S`、`/queue`；Hooks、AcpGateway、Scheduler |
| 运行时 Session 门面 | 补充 | 按命名空间查找 `sessionMethods`（固定前缀读取由现有门面组合实现，#50） | Rpc；进程调用方 |
| ToolRegistry | 补充 | 除现有注册外，接受具名的 `toolContribution` 服务（[方法 A.5](contracts/session-boundary.md#a5-工具贡献)） | 非 Rust 的 Session 工具 |
| Agent 装配 | 补充 | 收集每个已安装插件在 `provides` 中声明的具名服务；可选的 Session 与分支部分按已启用插件安装 | 所有 Session 插件 |
| **已撤回（Hooks 待设计，#21）。** Loop, ToolExecutor | 变更 | `step/tool-call` 的调用增加通用可选字段 `skip: {reason: string}`（提议的数据合同变更，不带业务含义）。ToolExecutor 对被跳过的调用不询问 Approval 与 Permission，也不执行，并在其正常批次的 `step/tool-result` 中写入错误结果（`reason` 作为错误文本）。设计见 [Hooks](plugins/Hooks.md) | Hooks |
| Permission | 补充 | 命令规则在使用时读取（不新增风险类别：未知风险按未分类处理，#45） | `/approvals` |
| ModelProvider | 补充 | 向 model kind 传递 `ModelCallScope`（session、branch、request 与 step id，`SessionConfig`）；构建请求时发送图像，模型不支持图像时改为文本占位；历史不变（从 Rpc 移出） | ExternalAgent；Attachments |
| Prompt, ModelProvider | 补充 | 模型参数（effort、fast 模式）；切换 Agent 配置作为一次采纳 | `/effort`、`/fast`、`/agent` |
| PromptSources | 补充 | 提示词语言来源 | `/language` |
| ContextPreparation | 补充 | 用量查询；手动压缩；实现 `ResolveContextModel` 的生产环境压缩宿主 | `/usage`、`/compact` |
| FileStorage | 补充 | 根目录下的目录列举与删除（S6）；目录创建（S5a，[Session 文件](appserver/session-files.md#7-filestorage-新增操作)） | Session 文件、清理 |
| HostPlatform | 补充 | 进程启动时的 OS 隔离（macOS seatbelt、Linux landlock 或 bubblewrap、Windows 明确不可用）、按节点的宿主本地配置；exec argv 辅助；每次 spawn 时读取 exec shell（ADR 002）；到控制节点的链接断开时，该节点的 HostPlatform 结束它启动的进程 | 沙箱；ShellInput、Hooks、ExternalAgent |

### B.2 App Server 进程插件

| 插件 | 类型 | 内容 | 理由 |
| --- | --- | --- | --- |
| Rpc | 变更 | 方法表：内建方法，然后是按调用查找、不依赖其所有者的 `managementMethods`（取代 `When<ProductServices>` 和 `When<ExecutionServices>`），然后是 Session 的 `sessionMethods`；供 provider 使用的 `callChannel`。从不向带 `deviceId` 的连接提供管理方法和 `daemon.shutdown`。`session.create {interactive}`（只在其为 false 时发送）。内建方法 `session.delete` 与 `session.fork`（S5c，已实现：[删除与分叉](appserver/session-deletion-and-fork.md)）。把 Session 广播通知记录原样转发给已订阅的客户端（SessionTitle）。Session 列表中的 `title`/`titleSource`。只做外部边界校验（参数格式、`sessionId` 授权）；`session.submit` 只检查格式，不再校验图像（[方法](contracts/session-boundary.md)） | 所有方法所有者；SessionTitle |
| Transport, Connection | 变更 | 带升级认证器的 `EntryHost`；`ConnectionEntry {name, deviceId}` | RemoteGateway |
| Agents | 补充 | `sessions` 服务（[sessions 服务](appserver/sessions-service.md)）；`SessionConfig.interactive`；策略以启动方为上限的 `launch`，`LaunchRequest.agent` 可选（缺省为 Agents 的默认 profile；请求的 `model` 优先于 profile 的模型）；从另一个 Session 的切点创建（[删除与分叉](appserver/session-deletion-and-fork.md#6-分叉)）；以外部 Agent 作为模型的 profile，经 Agents 的类型化公共写操作写入（[Session 访问](contracts/session-access.md)） | Scheduler、Workflow、fork、ExternalAgent |
| Collaboration | 变更 | 成员模型：profile 的模型，否则为默认选择；成员的审批请求作为一条 `input/message` 送到 leader 分支；leader 模型通过 Collaboration 的工具回答，这次工具调用经过 leader 的 Approval 与 Permission，Collaboration 通过 Approval 的公开 respond 操作写入回答（Approval 不变，#29） | ExternalAgent |
| SessionStore | 补充 | 带修订检查的标题元数据，只通过 `setTitle` 写入，变更时发出 Session 广播通知（进程级状态的所有者都发出这类通知）；删除与清理；fork（复制切点之前的事实）（S5c，已实现：[删除与分叉](appserver/session-deletion-and-fork.md)）；资源节点上的 Session 文件，fork 时共享，随最后一个所有者删除（S5a，已实现：`sessionFiles` 服务，[Session 文件](appserver/session-files.md)） | SessionTitle、Attachments、fork |
| Settings | 补充 | S8a（Rust，已实现）：每个所有者一个或多个具名业务来源，每个来源一种文档类型；没有自己的订阅接口：写入提交后在 `settings/<source>` 上发出 rutis 事件 `settings.changed` `{source, scope, revision}`，覆盖用户和项目作用域，不带文档内容；新授权的项目发出 `settings.project_authorized`；校验和默认值只在 Settings 边界应用；v13 中增量的 `user/<source>` 行（D4）。S8b（随 C4，尚未实现）：语言中立的 `settings` 服务，插件清单声明 JSON Schema、默认值、允许的作用域和仅限用户的字段；管理方法 `settings.read`/`settings.update`；凭据解析器 `credentials.resolve(owner, reference)`（§6.4） | 每个有业务配置的插件，包括用户编写的 Scheduler 任务（来源 `schedule`）和 Workflow 定义（来源 `workflow`） |
| Provider | 补充 | `AuxiliaryModel` 服务：`complete(model, request)`，在任何 Session 之外的一次非流式调用，模型由调用方从自己的配置中传入（[设计](appserver/auxiliary-model.md)）；按 Modality 类别的非对话连接；外部 Agent 连接类型；连接条目经 Provider 的类型化公共写操作写入 | SessionTitle、Modality、ExternalAgent |
| Aimux / ModelHost | 补充 | 来自其他插件的 provider kind；某 kind 没有 provider 时，其请求在受理前失败 | ExternalAgent |
| `pluginFiles`（`PluginFilesPlugin`） | 补充 | 二进制读写与格式版本（[受信插件存储](../../../crates/dim-app-server/README.md#trusted-plugin-storage)）；代码位置不变 | Scheduler、Workflow、ExternalAgent、Modality、RemoteGateway、PluginManager |
| Mcp, Provider, SkillsCatalog, ManagedProviders, OAuth 插件 | 变更 | 把各自现有的管理方法作为 `managementMethods` 提供（线上兼容）；Mcp 的 `mcp.approve`/`mcp.revoke` 已删除，服务启用即运行，项目条目默认关闭、只能由用户本机配置启用（§6.4 批准范围） | Rpc 迁移 |
| ManagedProviders | 补充 | 套餐用量查询 | `/usage` |
| Mcp | 补充 | HTTP/SSE OAuth（[MCP OAuth](appserver/mcp-oauth.md)）；通过自己的类型化操作从 PluginManager 接收插件包中的 MCP 配置（#36） | MCP；PluginManager |
| SkillsCatalog | 补充 | 通过自己的类型化操作从 PluginManager 接收插件包中的 Skill 根目录（#36） | PluginManager |
| Trace | 补充 | 模型请求快照导出 | 快照导出 |

## 附录 C. 实现语言

| TypeScript 优先 | Rust |
| --- | --- |
| 业务规则仍需验证 | 安全边界与准入：命令执行、审批、before 回调、隔离、远程入口 |
| 主要是外部 I/O 或协议 | 输入与 Session 方法边界 |
| 不在 turn 热路径上 | 在途工具调用的结算 |
| 失败只影响该功能 | loader 与插件管理；基础服务 |

TypeScript 进程插件如今以 loader 行运行（仅在配置时启动 Node，[App Server](../../../crates/dim-app-server/README.md)）。TypeScript 部分按名称提供和使用 [§3.1](#31-合同形式) 的语言中立合同；还使用运行时类型化服务的 Session 与分支部分，需要 [#4](https://github.com/arcships/dim/issues/4) 按名称暴露这些服务，并需要 #2（§9）。API-key 基线不依赖 Node：TypeScript 优先的功能随固定版本的 Node 发布，没有 Node 时明确不可用。插件从 TypeScript 迁到 Rust 时，其语言中立合同、事件类型、工具与方法名以及存储格式保持不变；迁移只替换 loader 行，流程测试原样通过，且同一时间只装载一个实现。现有插件的补充使用 Rust。

## 附录 D. CLI 层

以下内容只在 CLI 中实现：多行编辑、输入历史、命令补全、`$` 与 `@` 引用、粘贴与拖放路径、剪贴板、消息复制、打开链接、`/timeline`、`/export`、`/exit`、`/theme`、`/keys`、界面语言、升级、VS Code 集成、子命令体系、`exec --json`、`exec --verify` 修复循环、汇总的后台任务，以及附带不可用原因的 Modality 类别列表。

## 附录 E. 插件文档模板

插件与合同文档使用以下布局：

- 头部（状态、总览链接、阶段、语言）和摘要。
- 1 结构：作用域图，以及标明各部分作用域的部分表。
- 2 数据与状态。
- 3 流程：时序图。
- 4 时序与冲突：关键竞争（包括重载传播），用简短表格列出。
- 5 上下文映射。
- 6 接口（摘要）。
- 7 安全。
- 8 规则检查：一张表，[设计规则](../../architecture/rust-agent/principles.md) §4 第 1–9 条、§1（Capability 与 App Server 边界）和 [ADR 002](../adr/002-plugin-scope.md) D1–D5 各占一行，每行用一句话回答，或写“不适用：<原因>”。
- 9 向机制所有者提出的需求：只列对运行时或 rutis 的需求，每项写 §4 第 8 条的五项，没有写“无”。由最需要该需求的文档写出，其他文档链接过去。需要其他插件做的变更列在[附录 B](#附录-b-现有插件的补充)，文档链接过去。
- 附录：接口细节、完整冲突表（在推导输入处包含 `input/deleted` 行）、错误与恢复、验收、假设和待对齐问题。

## 附录 F. 向机制所有者提出的需求

对运行时或 rutis 的每项需求一行；对其他插件的变更见[附录 B](#附录-b-现有插件的补充)。链接的文档写出[设计规则](../../architecture/rust-agent/principles.md) §4 第 8 条的五项，其他文档链接到它。

| 需求 | 所有者 | Issue | 需要方 | 五项说明 |
| --- | --- | --- | --- | --- |
| **已撤回（Hooks 待设计，#21）。** 异步 Timeline before 回调（保持在顺序中的位置、由回调持续更新并在回调无法完成时提交的兜底草稿、`timeout`、`cancel_on`），写入方的追加在被接受时返回，因此 Loop 等同步写入方不会阻塞 | 运行时 | 待开 | Hooks | [Hooks §9](plugins/Hooks.md#9-向机制所有者提出的需求) |
| 运行时的类型化服务（Timeline、Dispatcher、InputQueue、Session 广播等）按名称暴露给非 Rust 部分 | 运行时 | [#4](https://github.com/arcships/dim/issues/4) | SessionTitle、Goal、Web、Scheduler、Workflow、ExternalAgent、PluginManager | [Session 访问 §9](contracts/session-access.md#9-向机制所有者提出的需求) |
| **已撤回（现有门面组合即可，#50）。** Session 门面上的固定前缀历史读取 | 运行时 | 待开 | 进程调用方、依据 Timeline 事实决定的分支部分 | [Session 访问 §9](contracts/session-access.md#9-向机制所有者提出的需求) |
| Agent 装配从 `provides` 收集具名服务，并按插件启用状态安装可选部分 | 运行时 | 待开 | 所有 Session 插件 | [Session 访问 §9](contracts/session-access.md#9-向机制所有者提出的需求) |
| 按名称查找带 key 的 Session 服务（按命名空间查 `sessionMethods`）及收集具名 `toolContribution` | 运行时 | 待开 | Rpc、ToolRegistry | [方法与工具贡献 §9](contracts/session-boundary.md#9-向机制所有者提出的需求) |
| **已撤回（rutis 是外部依赖）。** Session 级方法提供方卸载时等待其 `drain` 调用 | rutis 生命周期 | 待开 | Rpc | [方法与工具贡献 §9](contracts/session-boundary.md#9-向机制所有者提出的需求) |
| 插件在运行时注册自己的事件类型 | 运行时 | [#2](https://github.com/arcships/dim/issues/2) | 目前没有消费者（Questions 需要重新设计） | [Questions §9](plugins/Questions.md#9-向机制所有者提出的需求) |
| **已撤回（rutis 是外部依赖）。** 经 rutis 链接导入的服务在链接断开期间保持登记，每次调用以 `node_unavailable` 失败（该节点上的进程由该节点自己的 HostPlatform 结束） | rutis | [#5](https://github.com/arcships/dim/issues/5) | ExternalAgent、Modality、ShellInput、Hooks、Attachments、Workflow、Scheduler、PluginManager、RemoteGateway | [ExternalAgent §9](plugins/ExternalAgent.md#9-向机制所有者提出的需求) |
| **已撤回（Notifications 已撤回）。** 跨语言发出和监听具名进程事件 | rutis | 待开 | Scheduler、Notifications | [Scheduler §9](plugins/Scheduler.md#9-向机制所有者提出的需求) |
| **已撤回（rutis 是外部依赖）。** 提供方在一次调用内向调用方发请求 | rutis | 待开 | RemoteGateway | [RemoteGateway §9](plugins/RemoteGateway.md#91-提供方在调用内向调用方发请求) |
| 进程级状态的所有者在变更提交后发出的 Session 广播通知记录 `{source, kind, data}` | 运行时（Session 广播） | 待开 | SessionTitle | [SessionTitle §9](plugins/SessionTitle.md#9-向机制所有者提出的需求) |
| **已撤回（rutis 是外部依赖）。** loader 可编辑层上的类型化操作 | rutis-loader | [#3](https://github.com/arcships/dim/issues/3) | PluginManager | [PluginManager §9](plugins/PluginManager.md#9-向机制所有者提出的需求) |
