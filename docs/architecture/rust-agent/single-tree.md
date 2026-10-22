# Rust Agent 与 App Server 单树装配

Status: single-tree assembly, capability reload settlement, native Timeline storage
and recovery are implemented. FileStorage, process Aimux, product Provider and
optional managed-provider bindings are connected. Native user MCP/Skill intent
uses source-owned business documents; MCP connections and Skill content instances belong
to Sessions. Project MCP entries start disabled until the user enables them. See
[ADR 004](../../adr/004-loader-plugin-management.md) for implemented configuration
scope. Rpc looks up [management methods](../../design/appserver/management-methods.md)
per call as named services of account, provider, MCP and Skills owners, without
depending on them, so an owner's reload affects only its namespace; business
configuration changes do not reload those plugins. No Methods or Management
plugin, registration port or separate management adapter row is required.
Product assembly preflight remains incomplete; proposals are recorded in
[ADR 001](../../adr/001-harness-foundation.md). Capability event declarations are
descriptive contracts that assembly does not check.
External protocols include v1/v2/v3; see [dim-app-server](../../../crates/dim-app-server/README.md).

本文统一按 rutis 0.12.1 设计，定义服务、事件与装配合同；产品 caps、依赖环预检与跨进程授权由项目实现。

## 结构

每个进程一个 rutis root。进程级服务、Session 与分支都是这棵树上的 Plugin；所有有状态服务、监听器和长期任务都由 Plugin 提供，依赖全部经 `TypedPlugin::Deps` 声明。`main` 只解析启动参数、建立 Tokio runtime、调用 `serve`、处理进程信号；纯协议解析、序列化与无状态转换是普通函数。

可选的 managed provider 账号插件由外部单独提供：给出 `--plugin-dir`、`--js-runtime` 与 `--node` 时，rutis Node runtime 与该插件作为同一进程树中的独立 loader 行装载，`ManagedProviderBinding` 把它绑定到进程服务 `ManagedProviders`。公开宿主只暴露通用的 provider/账号操作与请求内凭据租约，不实现具体账号体系、token 交换或刷新策略；插件不可用时其 managed provider 请求明确失败，API-key 与原生订阅 provider 不受影响（合同见 [dim-app-server](../../../crates/dim-app-server/README.md#optional-managed-provider)）。

MCP connection generations are nested `McpConnections` group instances owned by
each Session in the same loader. `McpPublication` serializes source selection
and cross-Session retirement; it does not reconcile root layers.
Its plugin declares Settings as a dependency and owns the publication worker in
an asynchronous effect. Reload and disposal stop admission, settle the current
operation and retire retained connections before withdrawing the service. The
process shutdown coordinator neither starts nor joins MCP-specific tasks.
See [ADR 004](../../adr/004-loader-plugin-management.md).

`SettingsPlugin` 不作为 `SessionScopePlugin` 的 `Deps` 依赖：Session 创建入口传入已校验的配置快照，既有 Session 的设置变更由受影响能力按业务合同更新（如 McpSession 观察目录修订）。普通配置写入不通过 rutis 重载重建 Session。

当前 `--database PATH` 装载 `SqliteStoragePlugin` 与 `SessionStorePlugin`，共用一个原生数据库，保存 Agent profile、Session 身份与 Timeline；启动只读目录，Session 按使用装载，未指定数据库时使用内存存储。`--home PATH` 使用 `<home>/dimcode.sqlite` 并授权该文件根；FileStorage 默认授权 `session_defaults.cwd`。SQL/JSON 格式见[原生存储格式](../../design/appserver/native-storage-format.md)。

下图为进程归属树。进程层实际装配见[App Server 生成产物](generated/app-server/services.md)，Session 与分支见[生成产物](generated/services.md)。

```text
rutis root
└─ rutis-loader（根下唯一的插件；以下每项都是它的一行，模块名即插件名）
    ├─ ServerControl
    ├─ FileStorage、FileWriteCoordinator
    ├─ Aimux ──► ModelHost / request 对象                   （--model-config PATH 或产品配置）
    ├─ ConfiguredModels ──► 开发目录与 resolver              （--model-config PATH）
    ├─ SqliteStorage ──► SessionStore                       （--database PATH 或 --home PATH）
    ├─ ManagedProviders、OpenAICodexOAuth、GrokOAuth ──► Provider（--product-home PATH）
    ├─ ManagedProviderBinding、JS runtime 与账号插件行       （可选，单独提供的插件）
    ├─ Settings                                             （--product-home 或本地执行 Session）
    ├─ McpPublication、Mcp、HostPlatform、SkillsCatalog     （受保护 profile 且执行 Session）
    ├─ AgentDirectory（Agent 表、Session 协议版本与占用、serverInstanceId）
    ├─ Agents（AgentIndex：目录逻辑与 Agent 管理，经 loader 创建与关闭 Agent）
    ├─ CollaborationDirectory、Collaboration（Team 关系与临时 Agent 归属）
    ├─ Connection ◄── Rpc 注册入站处理器
    ├─ Rpc
    ├─ Transport
    └─ agent ⟨instanced⟩                          实例 Agent#1（默认）、Agent#N（agent.create）
        ├─ AgentScope（Agent 门面；子插件 Sessions 承载监听）
        └─ session ⟨instanced⟩                    实例 Session#1
            ├─ SessionScope、TimelineTree、ToolRegistry …
            ├─ McpConnections ⟨instanced⟩         Session-local generations
            │   └─ McpServer
            └─ branch ⟨instanced⟩                 实例 Branch（main 等）
                └─ BranchScope（Timeline / Dispatcher）、InputQueue、Loop、
                   ContextPreparation、ModelProvider、Permission、Approval、
                   ToolExecutor、TreeTimeline …
```

Groups annotate loader configuration rows; dependency order comes from `Deps`.
`serve` registers process factories with `register_capability` and owns the static
assembly rows. All profiles reconcile their full assembly directly and wait for
readiness. Native business documents import lazily per source; subsequent edits
use Settings source-local CAS, not loader reconciliation. `RunningServer::restart`
uses `Loader::restart(row_id)`.

`AgentBuilder::install` registers scope plugins, capabilities and Session extension
templates and returns the `session` row, nested under `agent` by App Server.
Standalone `AgentBuilder::build` owns a root and loader; `build_in(ctx)` owns an
isolated loader key under the host; both create an `AgentAnchor` instance.
`build_at(loader, ctx)` uses the host's existing loader.

登记返回 `Result`，工厂构造和元数据读取的 panic 均返回构建错误。插件名称冲突时分配未占用的 `#n` 后缀，同时避让内置 Agent 行及所有分组 ID；扩展分组不得占用内置名称。实例创建检查 loader 的逐行失败报告，按登记生成的真实 row ID 等待 AgentBuilder 注册的部件，包括作用域根和显式请求的扩展；这些必需行即使没有 fiber，失败也不能忽略。失败则回滚整个候选实例。

宿主通过现有 loader 行配置直接追加的可选部件不是 AgentBuilder 的必需注册：其 Pending、Failed 或运行时缺失保留在 loader 状态中，不阻塞必需部件和首条输入。注册路径决定等待范围，不按语言、展示名或当前状态推断，也不新增 required/optional 标记或注册 API。所有行仍由同一个作用域分组持有并一起清理；服务的 `Deps`、Pending、audit 和失效传播不变。必需部件仍会等待自己声明的全部依赖，不能通过可选行绕过。

因此，ShellInput、Attachments 等经 `AgentBuilder` 注册的第一方 Rust 部件也属于必需成员：它们不能 Active，Session 就不能发布。当前接受这一后果，因为这些部件的启动失败主要属于实现错误，其依赖是核心服务。若某个 Rust 功能开始依赖可能长期不可用的服务，应改由现有 loader 行安装；只有实际需求到来时才重新评估显式成员标记，现在不增加标记 API。

必需部件在 `apply` 中登记的子插件仍由该部件等待；返回意味着其服务和订阅已就绪。debug 发布校验检查必需行及其子树的 Active 状态，发现未等待的子插件就拒绝发布；仅可选行及其子树不参加此 Active 校验。两者仍参加依赖读取和实例键审计。release 保留原有的作者责任与按注册行等待，不增加全进程诊断扫描。

这修正装配自身的等待范围（[#132](https://github.com/arcships/dim/issues/132)，[ADR 002 D3](../../adr/002-plugin-scope.md#d3-dependencies-follow-scope-and-point-from-extensions-to-the-core)、[Session access §9.4](../../design/contracts/session-access.md#94-optional-parts-per-enabled-plugin)、[规则 §4.8](principles.md#4-通过时间线扩展行为)）。输入仍经原有分支初始化、ToolBinding 取样和 BranchOpening；不增加发布阶段或工具专用接线。顺序、批次、Timeline 及重载语义均不变。

loader 的 `create_instance` 仍先等待行 settled；异步 apply 一直不返回的行可能占用该等待。此限制保留在 #132 中，不因此增加后台创建管理器；部件仍应遵守 apply 只登记服务/订阅、发布后再开始业务工作的规则。

首次 loader 装配也属于 Agent 已登记的创建操作；调用方取消等待不取消装配拥有权，`Agent::close()` 等待在途创建及其回滚后才释放 loader。请求的 Session 扩展由 `SessionExtension` 行装载，该行显式注入 `SessionIdentity`，不等待普通 Session 能力全部就绪。扩展与普通能力一起受候选 Session 的启动期限、Active 检查和分组回滚约束；作用域及扩展装配入口只初始化一次，不作为可独立重启的业务能力。嵌套实例归 Session 分组所有，不归创建它的行所有。

### 产品装配约束（目标）

**所有运行能力进入 Plugin 树。** App Server 的进程 root 承载跨 Session 的存储、账号和管理能力；`SessionScopePlugin` 是一次会话的业务根，`BranchScopePlugin` 是一条 Timeline 的业务根。Timeline、Dispatcher 当前由 BranchScope 提供并随其清理，已经处于 Plugin 管理内；不是每个结构体都另建可重启实例。Session/Timeline 下不得自建另一棵 rutis root。纯函数和数据类型可放在内部模块；任何被其他 Plugin 消费的运行服务必须有提供者和明确依赖键，不能从构造参数、静态变量或全局单例偷渡。

**归属树与依赖图分开。** 每个实例只有一个父节点；一个 Plugin 可以注入多个上游服务。按实际作用域与服务键解析后的依赖图必须无环，包含只为初始化顺序声明的依赖。父子关系本身不是服务依赖；父 `apply` 不等待依赖父服务的子节点 Active。事件的请求/结果往返不自动形成依赖边，只有必须存在的上游服务进入 `Deps`。

依赖以类型声明：每个 Capability 实现 rutis `TypedPlugin`，`Deps` 列出全部依赖（`InSession<T>`、`InBranch<T>`、进程级 `Arc<T>`、只管顺序的 `SessionGate`/`BranchGate`/`Gate`、按配置取舍的 `When<D>`），`Capability::keys` 由作用域算出实例键，`apply` 收到已解析的依赖；声明与读取在编译期对齐，注册与依赖图从这份声明生成，不另维护手写依赖图。所提供的服务和事件订阅在 `apply` 中登记，不进入类型声明；能力写入与依赖哪些事件是描述契约，装配不检查。rutis 以 `EventKey<E>` 约束事件载荷；这些机制本身不检查产品依赖环或授权。

| 时机 | 必须检查与处理 |
| --- | --- |
| 发布 Session/分支或新增代码实例前 | 校验服务键唯一性、类型与作用域、必需提供者和依赖环；错误包含实例、服务键和闭环路径，不能等启动超时才把环报成缺依赖 |
| 替换实例前 | 对替换后的实际图预检；失败保留旧代。依赖集合变更建立新实例，不能绕过固定的 `Deps` 键 |
| 上游缺失或换代 | 停止受影响消费者、取消并等待旧任务、撤销旧监听/句柄；依赖缺失时保持 Pending，依赖重新合法且 Active 后再按拓扑装载 |
| 配置、类型或依赖环非法 | 拒绝新配置/装配并暴露原因；不通过反复重启尝试修复非法图 |
| 运行任务或 runtime 故障 | 由所属 Plugin 结算操作并报告失败；显式 restart 经同一依赖调度换代，不自建另一套 supervisor 或无限重试 |

重载传播覆盖实际依赖消费者，独立兄弟节点不受影响。Timeline/Session 根的关闭是该作用域的终结，不作为修复普通业务 Plugin 的重启手段；存储维护须按持久化合同先排空。动态来源通过已声明的管理服务取得，管理器负责所管理实例的合法性与可用代次，不能借稳定路由掩盖代码插件之间的必需依赖。

### 进程层

| Plugin | 拥有 | 提供 | `Deps` |
| --- | --- | --- | --- |
| `ServerControlPlugin` | 就绪门闩、正常/强制关闭意图、监听器故障 | `ServerControl` | — |
| `FileStoragePlugin` | 文件操作准入、锁租约、在途 I/O 和原子替换清理 | `FileStorage` | — |
| `SqliteStoragePlugin` | 共享数据库连接、事务、原生 schema 门禁、文件维护锁与关停 | `SqliteStorage` | —；启动参数固定数据库路径 |
| `SessionStorePlugin` | 原生会话目录、Timeline 落盘/恢复及写入任务 | `SessionStore`、`TimelineStore` 持久化端口 | `SqliteStorage` |
| `FileWriteCoordinatorPlugin` | 跨 Session 的逐路径写入 FIFO 准入域；跨代共享 | `FileWriteCoordinator` | — |
| `SettingsPlugin` | Generic BusinessDocument sources and per-source CAS; no domain interpretation, approvals, connections or discovery | `SettingsService` | `FileStorage`, `When<SqliteStorage>` |
| `OpenAICodexOAuthPlugin` / `GrokOAuthPlugin` | 各自账号字段、登录、刷新与终态 | `OpenAICodexOAuthService` / `GrokOAuthService` | `FileStorage` |
| `ManagedProvidersPlugin` | 可选 managed provider 的当前绑定代次与在途操作；未绑定时请求明确失败 | `ManagedProviders` | —；`ManagedProviderBinding` 另注入 `ManagedProviders` 与 JS 宿主分发 |
| `ProviderPlugin` | catalog、连接与选择管理、账号变更后的非秘密 binding | `ProviderService`、`ProviderConnectionResolver`、`ModelSelectionHost` | `SqliteStorage`、`ManagedProviders`、Codex/Grok 账号服务；可选秘密宿主 |
| `AimuxPlugin`（进程级） | 共享 HTTP 资源、请求/流转换及取消桥接 | `ModelHost`：接收最终请求、已解析连接与取消信号的 request 对象 | 无业务服务依赖；不注入 Provider、ToolRegistry 或 Session |
| `ConfiguredModelsPlugin` | `--model-config` 的开发模型目录与 resolver | 开发目录与 resolver | — |
| `McpPublication` | Source selection, including project enablement from the user's record, and retirement across Session-owned instances; no root reconciliation | `Publication` | `SettingsService` |
| `McpPlugin` | MCP 目录、连接租约与项目条目的用户启用记录；不拥有已配置行 | `McpService` | `SettingsService`、`Publication` |
| `HostPlatformPlugin` | 平台与 exec shell 设置 | `HostPlatform` | `When<FileStorage>` |
| `SkillsCatalogPlugin` | Skill discovery/catalog and business settings; privately supplies the reusable catalog's SkillSettingsHost | `SkillsCatalog`, Skills management contribution | `HostPlatform`, `SettingsService` |
| `AgentDirectoryPlugin` | `AgentDirectory`：Agent 表与实例、Session 协议版本与占用、不可用记录、默认 agentId、serverInstanceId；不可重启 | `AgentDirectory` | — |
| `rutis-loader` | 进程装配的配置行与 `agent` / `session` / `branch` instanced 分组的实例 | `Loader` | — |
| `AgentsPlugin` | `AgentIndex`：目录逻辑、Session 准入、经 loader 创建与关闭 Agent、默认及恢复 Agent 装载入口；进程 builder | `AgentIndex` | `AgentDirectory`、`Loader`；`--database` 另注入 `SqliteStorage`、`SessionStore`，产品 profile 另注入 `SettingsService`；受保护 profile 每次调用时读取 `ModelSelectionHost`，不依赖其提供方（方法合同 §3.2） |
| `AgentScope`（`agent` 实例中的行） | 一个 `Agent`（Session 建在同一 Agent 实例里）与承载监听的子插件 `Sessions`；`AgentHost`（agent、agentId、default、profile、承载监听的 ctx）；apply 登记进目录 | 经 `AgentDirectory` 登记 | `AgentDirectory`、`Loader`；`TimelineStore` 由已配置的 AgentBuilder 传入 |
| `CollaborationDirectoryPlugin` | Team 关系与临时 Agent 归属的目录 | `CollaborationDirectory` | — |
| `CollaborationPlugin` | 成员关系、临时 Agent 归属、参与 Session 的关闭观察任务 | `Collaboration` | `AgentIndex`、`CollaborationDirectory` |
| `ConnectionPlugin` | Connection index, admission limits and transport tasks | `ConnectionHub` | `ServerControl` |
| `RpcPlugin` | Built-in commands, per-call routing of contributed methods and reply drainage | Registers RpcService with ConnectionHub; unregisters and drains on unload | `AgentIndex`, Collaboration gate, `ConnectionHub`, `ServerControl`; reads `ManagementMethods` providers and, in the product profile, Provider per call without depending on them |
| Management contributions | Owner-specific declarations, wire validation, business calls and their accepted calls, provided by the business plugin itself | Named `ManagementMethods` services; an owner's unload interrupts its `Report` calls and waits for its `Drain` calls, without reloading Rpc | Each owner's existing business dependencies |
| `TransportPlugin` | Unix/WS 监听器、socket 文件、accept 任务 | `ListenAddresses` | `ConnectionHub`、`ServerControl`；原生数据库启用时另注入 `SqliteStorage`，确保数据库独占锁就绪后才绑定端点 |

`AgentsPlugin` 提供 `AgentIndex`（`agent` 分组行由 `serve` 写入）；默认 agentId 由首代固定在 `AgentDirectory`。默认 Agent 与运行时创建的 Agent 都由索引经 `create_instance("agent")` 创建，`AgentScope` 只依赖 `AgentDirectory` 与 loader，二者不重载，所以 `AgentScope` 只在其 Agent 被关闭或进程停止时卸载；`AgentsPlugin` 重载只替换目录逻辑，新一代从目录接管已有 Agent。`AgentScope` 先载入子插件 `Sessions`（监听宿主），再登记 effect：先从目录注销，再经 `Agent::close()` 完成各 Session 的业务结算。业务结算不依赖树的祖先关系：显式 `agent.close` 与进程关闭第一阶段先在普通任务上下文结算，再关闭 Agent 实例；effect 幂等地重复结算。多个 Agent 平等并存、互不串扰，hosting 不含 Agent 间关系。

开发 profile（`--mock-model`、`--model-config`）不注册产品 OAuth/Provider；产品 profile（`--product-home`）注册 Settings、Provider 及账号服务。`--database` 在 Agents 就绪前装载 SQLite 和 SessionStore，进程关闭先结算 Session，再排空 writer 并关闭连接；活跃 Session 存在时存储不可独立重载。生产工具宿主与 Prompt 来源按 Session 能力工厂接入，进程管理服务按自身资源归属接入。

### 配置发布与 MCP

Native v13 settings retain plain business payloads in `settings_documents` with
source-local CAS and lazy atomic import, as described by
[ADR 004](../../adr/004-loader-plugin-management.md). Generic SettingsService
persists BusinessDocument values; MCP owns its types in `mcp/config.rs`, and
Skills owns its SkillSettingsHost implementation. Successful persistence precedes
invalidation; no unsaved loader intent, configuration namespace or flush exists.
Project files and MCP's user-local project enablement record retain separate authority.
Business updates do not reload owners or Rpc. Structural editable configuration
remains separate issue #3 work.

| 项 | 约定 |
| --- | --- |
| Sources and writes | Native user MCP/Skill documents import independently on first successful source read; plain v13 payloads and import provenance commit atomically. SQLite then owns that source, with native:source:N CAS tokens. Originals remain intact. Project `.mcp.json` remains file-authoritative with content CAS |
| Publication | Settings notifications update Session discovery automatically. New bindings use new instances; a persistence acknowledgement does not guarantee discovery, adoption or cleanup |
| MCP generations | Connections are not shared across Sessions. The declared `SessionLoader` service creates nested instances using private immutable values, never secret-bearing loader options or mutable templates. Bindings keep a superseded generation while its entry stays enabled, until release or Session close; disabling or removing the entry retires its generations in every Session even with bindings; a generation no lease uses closes even while current. Disconnect drains every affected Session instance; cleanup failures are reported separately |
| 项目 MCP 启用 | 没有宿主本地批准（[ADR 003](../../adr/003-approval-scope.md)）：已配置的服务器启用即运行。项目条目默认禁用，项目文件（包括其 `enabled` 字段）不能启用自己；用户经 `mcp.set_enabled` 在本地记录 `mcp-projects` 中按规范工作区与服务器 ID 启用。未启用的项目条目不启动 stdio、不连接 HTTP/SSE，包括自动发现；停用或经 `mcp.remove` 删除时先撤回记录，不依赖项目文件，并在各 Session 中关闭其运行代次后才返回（项目文件不可读时也一样：协调者把当前记录应用到上次读到的条目）；记录不可读时所有项目条目按禁用处理，用户条目不受影响。文件根授权、FullAccess 工具权限与 Session 恢复都不启用任何条目 |

Session 与分支能力不向进程服务登记自己：RPC 经 `Session::service::<T>(branch)` 调用分支能力（如 [Approval](plugins/Approval.md)）。开发配置的 ModelProvider 已注入进程 resolver 与 `ModelHost`，每次请求解析当前连接，再调用 Aimux；Session 的 ConfiguredModelPlugin 只提供请求准备。产品 profile 的模型选择经 Provider 的 `ModelSelectionHost` 解析；managed provider 的凭据只以请求内租约交给模型请求。

### Session 与分支

| 对象 | 建立与释放 |
| --- | --- |
| `SessionScopePlugin` | `Agent` 创建 Session 时载入；`apply` 以本实例键提供身份、配置、故障报告、`TimelineFactory`、Session 生命周期 `SessionLifecycle`（Loading → Active → Closing → Closed：`Agent` 在 entries 锁内发布时 Active；关闭 token 取消即 Closing；关闭完成或装配失败时由运行时记录 Closed）与宿主值；Active 后 `Agent` 在其下载入 Session 能力；关闭 Session 即永久卸载该子树 |
| `BranchScopePlugin` | `TimelineTree` 创建分支时在 SessionScope 下载入；`apply` 提供 Timeline、Dispatcher、分支身份与分支生命周期 `BranchLifecycle`（Loading → Active → Closing → Closed，只由 TimelineContext 推进）；Active 后在其下载入分支能力；分支结束即永久卸载该子树 |
| `TimelineContext` | 分支能力全部 Active 后由 `TimelineFactory::build` 建立并启动分发，`TimelineTree` 持有；关闭时先结算、再卸载 BranchScope 子树、最后关闭 Timeline。不作为分支服务提供，能力不经它取依赖 |
| 能力 Plugin | 每个作用域实例一组；随作用域子树卸载 |
| 宿主值 | `AgentBuilder::session_value(Arc<T>)` 由每个 SessionScope 在 `apply` 中以 Session 键提供 |

成对能力（TimelineTree/TreeTimeline 等）的分支半边经 `Deps` 取得 Session 服务。

Session 与 Timeline 在结构图中分别由 `SessionScopePlugin` 和 `BranchScopePlugin` 表示；Timeline 是后者提供并管理的服务，不另建 `TimelinePlugin`。`TimelineTreePlugin` 管分支目录和创建，创建关系不等于 BranchScope 注入 TimelineTree。

`SessionConfig.collaboration_role: Leader | Member` 是 Session 创建时确定的协作角色，缺省 `Leader`，创建后不变。只有 `CollaborationPlugin` 创建成员 Session 时写入 `Member`；`session.create` 不接受该字段。

`SessionConfig.interactive: bool` 表示 Session 有可交互的对象，能回答它的提问和审批，在创建时由 `session.create` 的 `interactive` 参数确定（缺省 `true`），创建后不变。装配从不读取它；关心它的 Session 部件经 `InSession<SessionConfig>` 或 `Session::config()` 读取（[Session 访问 A.3](../../design/contracts/session-access.md#a3-sessionconfig-fields)）。

Session 能力向 `ToolRegistry` 贡献工具的方式见[工具贡献](tool-contributions.md)。

## 实例键与可见性

```text
TypeKey { type_id, qualifier: Option<Qualifier>, instance: Option<InstanceId> }   // Clone
InstanceId：rutis 在 fiber 创建时分配；作用域键使用所在 instanced 分组实例的 InstanceId（Build::instance）
TypeKey::of::<T>()             进程级服务
TypeKey::instance::<T>(id)     作用域服务
```

`dim-agent-runtime` 提供：

```text
SessionScope { instance }
BranchScope  { session: SessionScope, instance }
trait SessionService / BranchService               服务类型声明所属作用域
scope.key::<T>()                                   T: SessionService（SessionScope）或 BranchService（BranchScope）
scope.service::<T>(ctx)                            按同一键用 require_as 严格读取服务
session.named_key::<T>(name)                       同类型 Session 服务的限定键
session.named_service::<T>(ctx, name)              按限定键用 require_as 严格读取服务
session_plugin(|scope: &SessionScope| ...)
timeline_plugin(|scope: &BranchScope| ...)
```

插件实例在构造时按作用域生成 `Deps`。作用域服务在能力载入前已 Active，每个能力只装载一次。启动屏障覆盖作用域及经工厂载入的全部能力；能力在 `apply` 中登记的子插件由该能力负责等待。带实例的键只对该实例作用域子树内的节点可见；严格读取分别报告越界与服务不可用。祖先节点提供的服务对子树直接可见。

| 方向 | 方式 |
| --- | --- |
| 向上（祖先作用域、进程） | `Deps` 注入 |
| 向下（Session → 分支） | `TreeCommands` / `Branch` |
| 横向（兄弟分支、其他 Session） | 不可见；经共同祖先的业务接口 |

插件每次 `apply` 使用 `Ctx::require` / `require_as`，作用域的 `service` 和 `named_service` 共用严格读取。`ServiceReadError` 保留服务键、插件、实例、调用位置和原因，包括 `Undeclared`、`OutOfScope`、`Inactive`、`TypeMismatch`、`Unavailable` 及拦截失败；不统一降为“服务不存在”。该检查在 debug 与 release 都执行。

rutis 严格读取允许自身提供的服务，也允许可见范围相同的祖先声明；项目仍要求每个 Plugin 在自己的 `Deps` 列全必需外部依赖，以保证自身重载关系。插件外的门面查询和作用域发布使用 `get_as`，缺失按对应公开接口返回构建错误或 `service_reloading`；不能用它绕过插件内的依赖声明。debug 快照校验另检查插件内直接 `get_as` 的未声明外部访问、读取代次不在已解析依赖中和无实例键提供；失败时不发布 Session/分支。

## 事件

Dispatcher、before 与 Session 广播统一使用显式 `EventKey<E>`：

| 事件范围 | 键与调用 |
| --- | --- |
| 分支已提交事件 | `EventKey::<Committed>::of().instance(branch.instance)`；`on(ctx, &key, listener)` / `serial(ctx, &key, &event).await` |
| 分支 before | `EventKey::<BeforeCommit>::of().instance(branch.instance)`；内部事件实现 `SyncEvent`，用 `on_waterfall_sync` / `waterfall_sync` |
| Session 活事件 | `session_channel(session_id) -> EventKey<SessionBroadcastEvent>`；`emit(ctx, &key, Arc::new(event))` / `on(ctx, &key, listener)` |
| 进程通知 | 所属 Plugin 导出的 `EventKey<E>::of()`、静态 `named` 键或动态键构造函数 |

服务键使用 `TypeKey`，事件键不代替服务依赖。事件键的身份含载荷类型、可选名字与可选实例；静态与动态同名等值，默认类型通道不等于命名通道。内部事件键不作为 wire 字段或持久身份。Session 键由所属模块导出，调用方不拼接名字。Session 级能力不注册分支实例事件；需要结束记录的监听在比 Session 更晚卸载的宿主 Ctx 注册。`emit` 返回接纳结果，不证明监听器或出站写入完成，错误进入所属故障出口；Fence 输出屏障按[Session 广播](event-driven-flows.md#6-session-广播)执行。

Dispatcher 与 before 的精确名称、`*`、`领域/*` 只过滤同一分支通道内的业务 `EventKind`，匹配由 runtime 完成。rutis `EventPattern` 仅匹配同类型、显式命名、无实例的通道，不能覆盖分支实例；也不提供跨名字全序。

**唯一总线。** 同一宿主 root 的所有 Plugin 使用 `ctx.events()` 返回的同一 rutis EventBus；实例和命名通道是该总线的路由键。Timeline 是事实日志，Dispatcher 是读取有序日志并向该总线派发的分支任务，SessionBroadcast 是同一总线上的对外投影，三者不各自建立总线。插件内部的队列、状态 watch 和网络传输可保留，但不得成为跨插件业务事件的第二套发布/订阅系统。

**事件分发。** 按 [ADR 001](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)（issue #2 已决定）：Dispatcher 的 `&[&str]` 参数承载精确、全部与领域选择，只是投递过滤，不校验键是否被声明或有人写入；Timeline 只检查信封，并拒绝在 `EventKind` 保留的领域内写入不透明事件，不维护类型注册表；能力的事件声明是描述契约，监听注册与写入都不读取它。持久事件使用 `AgentEvent` 信封；核心未定义的类型键作为不透明数据承载，v1-v3 协议不投递不透明事件（未实现）。关键业务事件先提交 Timeline 再派发；普通进程通知在同一总线直接分发。JS/MCP/client 的传输在 Rust 边界解码、校验来源与授权后进入同一入口，JS 插件不能另建跨插件 EventBus。监听随注册 Plugin 卸载；重载时先注销旧代再登记新代，遵守分支顺序、停止结算及输出屏障。

### 同步 before

`Timeline::before(ctx, name, keys, handler)` 在分支实例通道注册 `on_waterfall_sync`，监听随注册 Plugin 卸载；提交锁内调用 `waterfall_sync`，不维护独立注册表或生命周期。`BeforeCommit` 是 runtime 内实现 `SyncEvent` 的调用载荷，不写入 Timeline，不启动 future 或任务。

- handler 签名为 `Fn(EventDraft) -> Result<EventDraft>`。包装层固定先调用 `next`，再变形下游结果；先注册者在外层，后注册者先变形，trace 按实际顺序保留 original 与 steps。handler 不能拦截下游。
- handler 报错、panic（`SyncEventPanicked`）或改变事件 kind 时整链回退，原样提交并清空轨迹。
- `AppendGuard` 阻止 before 和登记回调里的嵌套 append；rutis 的同步重入检测只覆盖同线程、同总线、同完整键。
- 分支作用域关闭中或 Ctx 失活时，before 不执行，原样提交且不留轨迹；写入准入由 Timeline 自身关闭状态决定，Timeline 已关闭后拒绝写入。清理终态提交与正常停止的分发、关闭顺序见[生命周期](lifecycle.md#5-stop-与-close)。
- rutis 卸载等待已接纳的同步调用栈退出，包括无监听器的终点；该计数不包含返回后的业务提交，Timeline 自己管理提交锁与关闭准入。
- 回调不执行 I/O，不等待包含自己的卸载，也不重新取得调用方持有的业务锁。异步执行前 Hook 由业务所有者管理，不能改成同步总线调用。

公开流程验证复用 [timeline_flow](../../../crates/dim-agent-runtime/tests/timeline_flow.rs)：多 handler 顺序与 trace、整链回退、嵌套 append 和作用域关闭期间原样提交。

## RPC 接线

- **Protocol handling**: envelope decoding, hello/version negotiation, v1/v2 error mapping and event filtering follow the App Server contract. Built-in calls use RpcService; a contributed management call is looked up per call among the active `ManagementMethods` providers, whose owners validate concrete DTOs ([management methods](../../design/appserver/management-methods.md)). The local v3 entry serves management; WebSocket does not. Management methods have no notification channel.
- **`session.create`**：App Server 解析工作目录、显式或默认模型与准入（产品 profile 经 `ProviderService`），再由 `Agent` 创建 Session。
- **无 Session 的管理请求**：使用进程 `ProviderService` 与账号服务（Codex/Grok OAuth、`ManagedProviders`），不创建隐藏 Session。
- **WS**：账号与 provider 管理只经本地传输（Unix socket / Windows named pipe）开放，WS 不开放账号与配置方法。
- **重载窗口**：`ConnectionHub` 未注册 `RpcService` 时，hello 与请求均返回 `-32000`，`data.appError = "service_reloading"`。请求在持有当前处理器期间完成准入并登记到其命令集合，已关闭或属于旧实例的连接不再受理；卸载的处理器排空它受理的全部命令。
- **实例换代**：连接记录 hello 时的 instanceId；注册 instanceId 不同的 `RpcService` 时关闭这些连接，client 重新 hello。

## 启动

1. `serve(builder, config)` 校验配置，建立 root 并注册全部进程 Plugin；Plugin 构造器只接收配置。
2. `TransportPlugin` 绑定监听器并登记清理；accept 任务等待 `ServerControl` 的就绪门闩。
3. 全部进程 Plugin 在 `startup_timeout` 内 Active 后，门闩打开，`serve` 返回 `RunningServer` 与真实监听地址。
4. 任一 Plugin 失败或超期：门闩不打开，root shutdown 并等待回滚，返回原始错误并汇总清理错误；不留下 socket、登录轮询或 Session。
5. `serve` 在交出 `RunningServer` 前被丢弃时，同样请求 root shutdown。

## 关闭

正常关闭由 `daemon.shutdown`、进程信号、`RunningServer::shutdown` 或 `RunningServer` 被丢弃触发，由 `serve` 建立的协调任务执行；等待方放弃不取消该任务：

1. `ServerControl` 停止 RPC 准入并等待已受理的 shutdown 处理器（含强制应答写出）；停止新连接、请求与 Session 创建，accept 任务退出并关闭监听器。发起非强制 `daemon.shutdown` 的连接只把最终应答的写端交给协调任务，自身离开插件图。
2. 全部 `Agent::close()`（并行，错误汇总）：等待已受理的构建、Session、分支及模型/工具操作结算；随后排空命令任务。
3. `ConnectionHub` 通知连接关闭并等待读写任务退出。
4. 协调任务执行 `Ctx::shutdown()`（含存储与插件释放）；`RunningServer::join` 返回该结果。此后才向非强制 `daemon.shutdown` 写出成功应答（v3 `hello.features` 含 `daemon.shutdown.completed`）；清理失败返回 `shutdown_failed`，不暴露释放错误细节；连接 EOF 不代表成功。

监听器异常退出进入同一路径，保留原始错误并汇总清理错误。`daemon.shutdown(force=true)`：应答先写出，进程可立即退出，结果报告 `Forced`，不声称 root 已完整清理；强制请求胜出时，等待中的非强制调用方得到 `shutdown_failed`。客户端断开不取消已受理的清理。合同见 [dim-app-server](../../../crates/dim-app-server/README.md#shutdown-completion)。

作用域子树经 `FiberView::shutdown()` 永久卸载。rutis 在 shutdown 结果中重复报告插件的装载结果（apply 错误在前、回滚错误在后）：卸载前已 Failed 的插件错误即构建原因，不计入清理失败；卸载时仍在装载的插件去掉其 apply 拒绝，保留回滚错误；卸载前 Active 的插件错误全部计入清理失败。

## rutis

设计基准为 crates.io [rutis 0.12.1](https://github.com/arcships/rutis/releases/tag/v0.12.1)。核心服务、事件与生命周期接口沿用 0.10.0 的合同；接口依据见 [0.12.1 源码](https://github.com/arcships/rutis/tree/v0.12.1/crates/rutis/src)。

0.5.0 → 0.9.1 的内核变化不改变本文合同：错误、诊断记录与 `EventOptions` 标为 `#[non_exhaustive]`；`RuntimeDiagnostics::event_backlogs` 报告同键 `emit` 积压；根关闭先取消子树再排空实例事件，根 `dispose()` 与 `shutdown()` 并发时共享关闭结果；新增 `TypedPlugin`、`Ctx::dispose_self`、`Ctx::view`、`ServiceChanged`。`apply` 显式返回 `InjectUnsatisfied` 且本代有失败的严格读取时回到 Pending；项目代码不返回该错误，`require` 失败仍是 `ServiceRead`。

0.9.1 → 0.10.0：`rutis` 与 `rutis-loader` 无不兼容变化；内核修复失败插件只在依赖变化时重试（rutis #327），loader 同时创建作用域插件与能力时，失败的能力不再被提供者激活时的重复通知再次装载。

0.10.0 → 0.11.0：核心与 loader 的合同不变。bridge 支持宿主声明固定事件名、通过 `Process::emit` 投递给 leaf 插件；TypeScript 的 `ctx.on` / `ctx.emit` 使用运行时进程内的同一事件总线。新增跨语言值类型使 `session::Value` 非穷尽；JSON 数据边界仍由现有消费者的 `json()` 校验，未增加产品值类型。C4 的具体广播绑定按 [#168](https://github.com/arcships/dim/issues/168) 评审和验收。
0.11.0 → 0.12.1：核心不变，loader 无破坏性变更。leaf 插件 `ctx.provide` 的服务即使未在 `provides` 中声明也会到达 rutis，同名时后提供者失败；`ServiceCatalog::new()` 默认按名共享未另行注册的名字；`rutis-bridge` 新增 `build::Exports` / `ServiceCatalog::export_all`，让外部 leaf 直接调用已有 Rust 服务的方法（C5 使用）。

| 能力 | 接口 |
| --- | --- |
| 实例键 | `Ctx::instance()`、`TypeKey::instance`/`with_instance`；provide、读取、门控与诊断按作用域子树校验，越界返回 `InstanceOutOfScope` |
| 事件键 | `EventKey<E>` 与统一的 `on` / `emit` / `serial` / `parallel`；实例键限于实例子树，`emit` 按完整键保序，`serial` 按监听注册顺序等待 |
| 同步事件 | `SyncEvent`、`on_sync` / `on_waterfall_sync`、`bail_sync` / `waterfall_sync`；等待已接纳调用栈退出，不包含返回后的业务提交 |
| 严格服务读取 | `Ctx::require` / `require_as` 返回带调用位置的 `ServiceReadError`；不取代 `injects` 门控或产品装配预检 |
| 子树永久卸载 | `FiberView::shutdown()`：子树内消费者先于提供者 dispose 且不进入 Pending，子树外消费者重载；回收父级、依赖索引与总线记录；结果共享、不可重启 |
| 就绪观察 | `FiberView::watch()` 观察各作用域子插件状态 |
| 诊断 | `Ctx::diagnostics()` 读取整棵树；`Session::diagnostics`、`branch_diagnostics` 截取对应作用域插件及其绑定 |

## 重载

| 项 | 规定 |
| --- | --- |
| 传播 | 上游重载时，rutis 沿 `Deps` 依赖驱逐并重新装载下游，可跨进程、Session、分支作用域 |
| 能力 | 随上游重载的能力按[能力重载协议](#能力重载协议)卸载与装载 |
| 核心状态 | Timeline、TimelineTree、Dispatcher、AgentIndex 内的 Agent 句柄不注入可重载的上游 |
| 门面 | `Branch` 经 TimelineContext 每次调用按分支键查找 `LoopState` 与公开服务，不持有能力实例；状态查询在 Loop 重载期间返回可重试错误。写入输入不查找任何能力端口，InputQueue 重载期间的输入由新一代从 Timeline 接收；stop、idle 只等待 `LoopState`，不拼装其他能力的状态：Loop 重载期间等新一代接手（重载即重启，见[总纲](time-bus.md)第 5 条），分支关闭后 Loop 不在时返回 `Closed`；关闭开始时 Loop 正在重载，则中断已提交、不再等待，不算关闭失败，未完成的 turn 按重载结算。分支仍存活时，`approval.respond` 因审批服务换代失败返回 `service_reloading`。`Session` 持有的 TimelineTree 不注入可重载的上游 |
| 拥有 | 作用域 Plugin 卸载即关闭该实例 |
| 代次结算 | 移除一代服务前先停止新工作、排空已受理工作并写完必需终态（AGENTS 规则 8）；连接断开不等于关闭 Session，运行时句柄不是持久身份。依赖丢失回到 Pending 时的业务结算语义仍为 Proposed（issue #6，见 ADR 001） |
| 触发 | 需要替换服务实现的配置变化、显式 restart、`check()` 变化；普通配置值更新由服务内部通知，不重载。故障不触发重载。各 Plugin 的设计列出自身触发来源 |
| 显式 restart | `RunningServer::restart(name)` 只接受 `Rpc`、`Connection`、`Transport`、`Agents`、`Mcp`，所有进程 Plugin 恢复 Active 后返回（`Agents` 只在默认 Agent 不存在时装载它）；卸载 `Rpc` 等待不依赖客户端输入的已受理命令结束，无期限 |
| 窗口期 | 下游 Pending 期间，依赖它的 RPC 与模型请求立即返回可重试错误，不等待重载完成；RPC 返回 `-32000`，`data.appError = "service_reloading"` |

### 能力重载协议

能力重载等同于该能力从 Timeline 重启：Timeline 是能力状态的唯一来源，重载期间 Dispatcher 分发的事件不送达该能力，新一代从 Timeline 取回；未完成的操作不再执行，判定为失败。原因统一为 `capability reloading`。

| 阶段 | 规定 |
| --- | --- |
| 卸载 | 取消自身在途操作并提交终态：工具调用返回错误结果并在 `structuredContent.settlement` 标记重载结算，模型请求以 `Cancelled` 结束；不代其他能力提交事件 |
| 重载结算 | Loop 收到带重载结算标记的工具结果、或原因为 `capability reloading` 的模型终态时，turn 以 `Failed` 结束，不再推进；新旧两代 Loop 相同 |
| 装载：订阅 | 取得依赖后先注册 Dispatcher 订阅 |
| 装载：重建 | 在能力的重建闸门内固定 `S = Timeline::committed()`，以 `local(..=S)` 读取本分支事件（不含祖先前缀）计算状态。旧代处理器和任务在其卸载完成前结束，读取覆盖旧代写出、尚未发布的终态，不等待持久化或 Dispatcher。重建与事件处理共用闸门，处理器跳过序号不大于 S 的事件；S 之后的事件只走实时消费 |
| 装载：失败判定 | 读取到的已提交而没有终态的操作，由所属能力提交失败终态，不执行；未结束的 turn 在其在途操作全部有终态后以 `Failed` 结束，期间不再发起模型请求或派发工具调用 |
| 历史投影 | turn 结束时仍未得到结果的工具提议，投影为未执行的错误结果 |
| 已知代价 | 未随重载的上下文准备跨过重载时，已判失败的 turn 仍可能完成一次模型调用 |

随上游重载的能力：

| 能力 | 作用域 | 重载来源 | 卸载 | 装载 |
| --- | --- | --- | --- | --- |
| `ToolRegistry` | Session | `ToolHost`；各 `ToolContribution` 贡献方 | — | 按 Session 配置重新注册宿主工具与贡献工具 |
| `PrepareModelHost` 提供者（当前开发接线：`DefaultPrepareModelPlugin`、`ConfiguredModelPlugin`） | Session | 当前 `ToolRegistry`，无 Provider resolver | — | 重新提供；产品请求准备归 Loop，不由底层 Aimux 提供 |
| `Loop` | 分支 | `PrepareModelHost` | — | 重建当前 turn、请求阶段、待收工具结果与中断状态；未结束的 turn 待在途操作有终态后提交 `turn/end`（`Failed`）；尚未交给上下文准备的请求由 Loop 提交 `Failed` 模型终态；没有未结束的 turn 时，已采用、未消费的驱动输入照常开始新 turn |
| `ToolExecutor` | 分支 | `ToolRegistry` | 执行中的批次提交错误结果 | 重建中断状态；已提交而未派发、无结果的 `step/tool-call` 批次提交错误结果 |
| `ContextPreparation` | 分支 | `PrepareContextHost`、`ContextModelHost` | 取消并等待准备任务，以 `capability reloading` 结算取消 | 重建中断状态；没有后继 `model/call` 或模型终态的 `context/prepare` 补写 `Failed`，不重放准备或压缩；被同一 turn 后续 `step/model-start` 取代的请求不补终态；已交接的请求归 ModelProvider |
| `ModelProvider` | 分支 | `ModelHost` | 进行中的请求以 `Cancelled` 结束 | 重建中断状态；已提交而无终态的 `model/call` 以 `Failed` 结束，被同一 turn 后续 `step/model-start` 取代的不补终态 |
| `Approval` | 分支 | `Permission` | 未决请求逐个写 `approval/cancelled` 后释放等待者 | 为上一代未结束的请求补写 `approval/cancelled` |

注入链上出现可重载服务的其他能力，同样遵守本协议。

进程 Aimux 的 `ModelHost` 换代会沿依赖重载已接线的 ModelProvider：消费者取消并等待自己的模型任务，结算由自身负责；Aimux 最后释放底层资源。开发目录/resolver 换代沿依赖传播到调用方，Aimux 不依赖该目录。Provider 换代沿依赖重载其实际下游；RunningServer.restart 的开放集合不含 Provider。

进程 Plugin 重载的结果：

| 重载 | 随之重载 | 结果 |
| --- | --- | --- |
| `ServerControl` | Connection、Rpc、Transport | 连接断开，监听器重绑；Session 与协议 instanceId 保留；不提供显式 restart |
| `Provider` | 其实际下游；不含 Rpc 与 Agents | 二者每次调用时读取 Provider 与 `ModelSelectionHost`：重载期间 `provider.*` 返回 `service_reloading`（Rpc 按 loader 行状态区分重载中与未启用），`session.create`、`session.model.update` 与成员创建得到可重试错误；连接保持；Session 历史保留；Aimux 不依赖 Provider |
| `Agents` | Rpc、Collaboration 及依赖协作服务的 Session 能力 | 只替换目录逻辑：Agent、Session、serverInstanceId 与连接保留；依赖协作服务的能力按能力重载协议重载，进行中的协作请求以 `capability reloading` 失败结算，对成员只发出停止、不等其结算；重载结算只作用于本 Session 持有的资源：只有成员关系仍以本 Session 为 leader 时才停止该成员，分叉复制来的请求不停止源 Session 的成员，守护进程重启后关系不存在，也不停止任何成员（[#127](https://github.com/arcships/dim/issues/127)）；窗口期请求得到 `service_reloading` |
| `Connection` | Rpc、Transport | 连接断开，监听器重绑；Session 保留，client 重新 attach |
| `Rpc` | — | 已受理命令排空后卸载，被打断的管理调用也等其操作结算；等待中的命令（`session.idle`）以 `service_reloading` 结束；未决审批属于各分支的 Approval，不随 Rpc 重载；窗口期请求（含 `hello` 与管理方法）得到 `service_reloading`。管理方法提供方的重载不重载 Rpc |

## 迁移

| 步骤 | 状态 |
| --- | --- |
| runtime：作用域 Plugin 与实例键、实例事件派发、`build_in` | 完成 |
| App Server：进程层 Plugin、`serve`/`RunningServer`、显式 restart（限定集合）与 `service_reloading` | 完成 |
| 能力重载协议与门面按调用查找 | 完成 |
| FileStorage、SQLite 资源边界、Aimux 请求对象及开发 resolver 调用方接线 | 完成 |
| 产品 Provider、Settings、Codex/Grok OAuth 进程 Plugin 与可选 managed provider 绑定 | 完成 |
| App Server 多 Agent：`AgentsPlugin`/`AgentIndex`/`AgentScopePlugin` 与协议 v3 agent 面 | 完成 |

## 验收

| 流程 | 结果 |
| --- | --- |
| 隔离 | 不同 Session、不同分支的同类服务与事件互不可见；越界读取不可解析；无实例键提供作用域服务时发布失败 |
| 启动与回滚 | 任一必需 Plugin 或监听器失败时不发布服务；已取得的 socket、文件、任务和 Session 全部清理，原始失败可见 |
| 无 Session 的管理请求 | Provider/OAuth 管理经进程服务执行；不创建隐藏 Session，后续 Session 使用同一状态 |
| 多连接与 Session | 断连不终止已受理工作；同一 Session 可重新 attach |
| 重载 | 进程服务重载后，各 Session 中依赖它的能力重新装载并使用新一代服务；历史与无关能力不变；各进程 Plugin 的重载结果符合上表 |
| 在途结算 | 重载时进行中的模型与工具调用留下终态事实；重新装载后状态与历史一致 |
| 窗口期 | 重载期间的 RPC 得到 `-32000` / `service_reloading`，模型请求得到可重试错误；重载完成后同一请求成功 |
| 正常关闭 | 停止准入、Agent 结算、命令排空、连接退出、root 释放均完成；重复关闭共享结果 |
| 正常关闭应答 | 非强制 `daemon.shutdown` 在 root 清理完成后才返回成功，清理失败返回 `shutdown_failed` |
| 强制关闭 | 响应先写出，进程及时退出；结果只标记 `Forced` |
| 关闭回收 | 关闭 Session/分支后子树 driver、注册、索引与父级清理记录归零；多轮创建与关闭不增长 |
| 规模 | 测 0/100/1000 空 Session、并发创建与关闭、多分支派发延迟；存活申请与创建耗时与原型基线（每空 Session 约 139.3 KiB、16 个任务）同一量级 |
| 兼容 | 事件序列、Unix/WS 上的 v1/v2 流程（以 [Session 广播](event-driven-flows.md#6-session-广播)修订后的合同为准）、审批与错误码不变；v1/v2 仅增加 `service_reloading`。`session.create` 不再接受 `trace`，结果不再含 `tracePath`（[ADR 005](../../adr/005-observability.md) D5） |
| 框架合同 | 严格读取在 release 同样拒绝漏声明（含命名工具贡献）；实例事件和 before 不越界；卸载等待同步回调退出；作用域清理期间终态原样提交，Timeline 关闭后拒绝写入；广播接纳失败不被误认为交付成功；生成图取自真实装配 |
