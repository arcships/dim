# 服务面：跨语言形状的策略、盘点与第一批改造

状态：Draft。对应 [#1289](https://github.com/arcships/dim/issues/1289)（策略）、[#1300](https://github.com/arcships/dim/issues/1300)（盘点）、[#1292](https://github.com/arcships/dim/issues/1292)（第一批）。父 issue [#1288](https://github.com/arcships/dim/issues/1288)。研究背景见 `docs/design/research/remote-plugins.md`（由 [#1299](https://github.com/arcships/dim/pull/1299) 引入）。

本文是改动设计，不是实现记录。所有 API 都是目标形状；现有代码位置用于说明改哪里。

## 1. 规则

1. **数据面是默认**。公开服务的方法是 serde 数据：参数、返回值、错误都是普通类型；不出现句柄、`Arc<dyn Trait>`、`&mut`、迭代器、通道、`PathBuf`/`OsString`。
2. **身份是值**。Session、branch、task、file、root、subscription 都是 id；id 不是授权。
3. **操作代替对象**。进程、PTY、文件、store 都是一组操作加 id；对象和句柄留在所有者内部。
4. **流是事件或订阅**。大输出分页；回调可以按引用跨语言，但不在 commit 锁里同步调用。
5. **错误是数据**。`{code, message}`；转发方原样抛。
6. **取消是调用**。显式 cancel，或调用方丢弃。

目标目录里没有 `internal`：每个公开服务都是 data face。迁移期间，尚未暴露的名字可以带一个 `internal(reason)` 作为 TODO；原因只描述还需要做什么（状态机、锁内回调、OS 对象、secret、热路径），不是永久分类，也不是“这个面永远不能跨”。功能可以整体作为 leaf 运行，但对外仍然是数据服务。

## 2. 策略（#1289）

### 2.1 目标

**Every public service is a data face.** A data face is a name shared in the catalog **and** a
`HostDispatch` at `host_key(name)`. The target catalog has no `internal` service class.

A Rust typed client is a binding over the same face, not a second public API. The owner trait is the
implementation behind the face; it is not a parallel contract.

### 2.2 Migration marker

During the migration only, a name that is not yet exposed may carry `internal(reason)` as a TODO.
It is not a target and not a contract category:

- `object`, `hot-path`, `lock-callback`, `secret`, `control`, `host-resource`, `legacy-no-consumer`
  and `owner-api` describe the work needed to expose the name, not permission to keep it internal.
- A new service must not be marked internal. It is written as a data face from the start.
- `legacy-no-consumer` means the migration has not happened; no consumer is required to expose a
  face.
- A secret **value** stays in the owner; the service still has a data face of operations. The value
  is a capability, not a serialization exception.

### 2.3 Checks

`xtask graph` and `check` enforce:

- shared + dispatcher → `data`;
- shared without dispatcher → failure, including `loaderService` and `pluginMounter`; the target has
  no `shared-no-dispatcher` exception;
- not shared + migration reason → `internal(reason)`, which must disappear as the batches land;
- not shared without a reason → failure;
- shared and internal at once → failure.

The target state is zero `internal(...)` rows and zero `shared-no-dispatcher` rows.

### 2.4 Leaf readiness

A leaf part's every `inject` and every `provides` name must be `data`. During migration an
`internal(reason)` inject fails at composition with `internal_service:` and the reason; after the
migration there is no internal face to reject.

### 2.5 One contract, typed binding

- The face is the only public contract.
- A typed client is a binding: it calls the face's operations, directly in-process instead of
  through JSON, and owns no second business logic.
- Do not leave an owner trait and a face as two public APIs. Tests prove the typed binding and the
  wire return the same result.
- `SessionFilesHost` + `sessionFiles` is a temporary migration state, not the target.

### 2.6 Migration rule

There is no permanent internal set. Every catalog name gets a data face in some batch; a face may
have no consumer yet. Performance and usability are the plugin author's concern; the platform
provides the mechanism. Split a handle into operations; keep a secret value in the owner and
expose operations.

## 3. 盘点（#1300）

catalog 现在 118 个名字：25 data、91 internal、2 shared-no-dispatcher；这是迁移起点，不是目标。§3 表覆盖这 91 个待迁移名字；`scheduleJobs`、`wakeups` 已在 #1292 转成数据面；`sessionFiles`、`managedSearch` 是 #1292 的新名字，不在当前 catalog。`workflowRuns` 不在其中：它是 optional Workflow 包自己的 provides 目录，已经是数据面。每个名字在分类表里恰好一行；说明句和“数据兄弟”列是引用，不算第二行。这些分组是当前迁移状态，不是永久分类；目标是没有 internal，所有名字都成为 data face。

### 3.1 已经可以跨，但 catalog 列没体现

| 名字 | 处理 |
| --- | --- |
| `managementMethods` | 在 `host_key` 上有 dispatcher；修 shared 注册顺序后 catalog 变 `data`。它是 #1289 的代码项，不是迁移。 |
| `localId` | 由 root 提供的 `get` dispatcher；catalog 是 `data`。 |
| `loaderService`、`pluginMounter` | rutis-loader 的 shared 名字，没有 host dispatcher；catalog 是 `shared-no-dispatcher`，leaf 不能调用。 |

### 3.2 第一批数据面（#1292）

| 名字 | 目标 | 备注 |
| --- | --- | --- |
| `scheduleJobs` | 新数据面 `scheduleJobs.call(JobRequest)` | 按 [Scheduler](plugins/Scheduler.md) 的 flat `JobRequest`/`JobReply`；Rust 实现是 `call(request, caller)`，dispatcher 拼成 flat request。`Caller`/`JobReply` 补 serde，`Caller` 用 camelCase |
| `wakeups` | 新数据面 `wakeups.create/list/cancel` | SchedulerSession 同时注入 `ScheduleJobs` 和 `Wakeups`；只做 `scheduleJobs` 不够。形状按 [Scheduler](plugins/Scheduler.md) §10.2 |
| `sessionFiles` | 新数据面，按 owner 契约 | 名字、base64 `content`、`owned_paths`、取消都按 [Session files](appserver/session-files.md) §9.1；`SessionFilesHost` 保持 Rust 类型键 |
| `managedSearch` | 新数据面，只含 `search` | `managedProviders`（§3.6）保持 internal(secret)，账号/token 不出去 |

`workflowRuns` 已经是数据面：optional Workflow 包（`runtime/plugins/workflow`）提供 `workflowRuns`，`call: async`，`RunRequest`/`RunReply` 与 [Workflow](plugins/Workflow.md) 一致。它不在 Rust product-profile 的 118 个名字里，而在该包自己的 provides 目录里；#1292 不新增 provider，只核对形状。

这两个 owner API 也进入迁移；typed owner API 只是 binding，数据兄弟才是公开面。`settings.read` 会新建取消 token、并返回 `SettingsResult` 信封；GoalBranch 需要自己的 stop token，改成数据面会丢取消。`auxiliaryModel.complete` 没有 tools、config 和 cached prefix；PromptSuggestion 克隆的是整轮 `ModelRequest`，换过去是行为变化，不只是形状。它们已有数据兄弟给其他消费者；它们也按 #1292 转 data；取消和行为差异是操作面要保留的语义，不是不转的理由。

### 3.3 所有者 API，已有数据兄弟

本组统一迁移标签 `owner-api`；数据兄弟是公开面，typed owner API 是 binding。

| 名字 | 数据兄弟 | 说明 |
| --- | --- | --- |
| `agentDirectory`、`agentIndex` | `sessions` | 目录查询走 `sessions` |
| `settingsService` | `settings` | GoalBranch 保留 typed；`settings` 给其他消费者 |
| `modelHost` | `auxiliaryModel` | PromptSuggestion 保留 typed；`auxiliaryModel` 给无 tools 的完成 |
| `auxiliaryModelHost` | `auxiliaryModel` | 模型调用走 `complete` |
| `loader` | `rutis/loader` | 插件管理走数据面 |
| `sessionFilesHost` | `sessionFiles` | Rust 绑定；wire 名按 owner 契约 |
| `basePromptSettingsHost` | — | 宿主 prompt settings 端口；`owner-api` |
| `skillsInstructionsSettingsHost` | — | 宿主 skills settings 端口；`owner-api` |
| `summaryPromptSettingsHost` | — | 宿主 summary settings 端口；`owner-api` |
| `sessionStore` | `sessionMetadata`、`sessionFiles` | 行与文件分开 |
| `timelineTree`、`timelineTreeControl` | `treeCommands` | 建/读分支走 `treeCommands` |
| `toolSourcesView` | `toolRegistry` | 工具贡献走注册表 |
| `callers` | `callChannel` | 调用通道走数据面 |

### 3.4 对象/句柄：拆成操作加 id

| 名字 | 现在的对象 | 目标操作 |
| --- | --- | --- |
| `hostPlatform` | `ProcessTree`、`PtyProcessTree`、临时目录 | 数据面 `execHost`：`start`/`wait`/`kill`/`settle`/`output`/`tail`/`removeOutputDirectory`/`shell`/`write`/`resize`/`info`/`confined`/`configured`（[resource and store data faces](resource-faces.md) §3，#1295）；`hostPlatform` 是提供方和它的 Rust binding |
| `fileStorage` | `FilePath`、根与代次 | 数据面 `fsHost`：工具部分 `resolve`/`read`/`write`/`edit`/`glob`/`grep`/`mentions`，存储部分 `stat`/`readBytes`/`openRead`/`readAt`/`closeRead`/`lock`/`unlock`/`backup`/`createDirectory`/`replace`/`list`/`remove`（§4，#1297）；`fileStorage` 是提供方和 binding |
| `sqliteStorage` | 连接与事务 | 数据面 `store` 的 Session 行部分（§5）；`sqliteStorage` 是提供方内部的 SQL 引擎（#1298） |
| `timelineStore` | 连接与批次 | 同一 `store` 面的 Timeline 部分：`append`（同步受理）/`commits`、`closeCommits`（按分支序的完成通知）/`head`/`branches`/`readPage`/`readSegments`/`query`/`trace`/`batchEnd`/`saveSnapshot`/`latestSnapshot`；`TimelineStore` trait 是 binding（#1298） |
| `toolHost` | `ToolDriver` | `execute` 数据面 |
| `mcpSession` | 会话对象 | 每 server 的 id 操作 |
| `backgroundExec` | 任务对象 | task id 操作 |
| `approval`、`permission` | 决策状态 | `decide` 数据面 |
| `inputQueue` | 队列状态 | `submit`/`withdraw` 数据面 |
| `wait` | `WaitResult`、`CancellationToken` | `start`/`wait`/`result` 操作面 |
| `promptSourcesBranch` | 分支 prompt 装配 | 留宿主；`hot-path` |

这些行按“现在的对象形状”分组：拆成操作加 id 是迁移动作，不是永久分类，`object` 只说明今天的形状。前四行是这一批的目标数据面（[resource and store data faces](resource-faces.md)，#1295/#1297/#1298）：本文只定目标形状与契约，catalog 的 `face` 列和本节的计数由对应的代码 PR 一起更新；其余行按 #1292 的批次逐个转。

`hostPlatform`、`fileStorage`、`sqliteStorage`、`timelineStore` 不按对象分类保留：数据面就是公开契约，`execHost`、`fsHost`、`store` 的操作就是它们的接口，旧名字是提供方和它的 Rust binding，调用方在进程内直接调这组操作，不再有第二套公开 API。凭据是操作内部用的能力，不是保留 `internal` 的理由。`hostPlatform`、`fileStorage` 的提供方在持有资源的那个 daemon（控制节点自己的插件，或 #1296 挂载的资源 leaf）；`store` 在持有 Session 状态的那个 daemon，SQLite 还是 PostgreSQL 只是实现（#1298，[resource and store data faces](resource-faces.md) §6）。

### 3.5 热路径与顺序：待操作化
原因 `hot-path`：每回合/每条输入的状态机、严格本地顺序，或同步回调。跨了要重写顺序和失败语义，收益不成比例。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `agentLoop` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `loopState` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `modelProvider` | branch | ModelProvider | hot-path | 待迁移；按 #1292 定义操作面 |
| `prompt` | branch | Prompt | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextPreparation` | branch | ContextPreparation | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextUsage` | branch | ContextUsage | hot-path | 本地请求计数器、请求引用与按序统计；公开统计经 `session.context.usage` 读取 |
| `toolBinding` | branch | ToolBinding | hot-path | 待迁移；按 #1292 定义操作面 |
| `toolExecutor` | branch | ToolExecutor | hot-path | 待迁移；按 #1292 定义操作面 |
| `branchOpening` | branch | BranchScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `configurationPublisher` | branch | ContextPreparation | hot-path | 待迁移；按 #1292 定义操作面 |
| `configureControl` | branch | Prompt | hot-path | 待迁移；按 #1292 定义操作面 |
| `fileReads` | branch | FileReads | hot-path | 待迁移；按 #1292 定义操作面 |
| `branchLine` | branch | BranchLine | hot-path | 待迁移；按 #1292 定义操作面 |
| `submitChecks` | session | InputMethods | hot-path | 待迁移；按 #1292 定义操作面 |
| `commandRulesHost` | session | CommandRules | hot-path | 待迁移；按 #1292 定义操作面 |
| `promptHost` | session | PromptRegistry | hot-path | 待迁移；按 #1292 定义操作面 |
| `promptRegistry` | session | PromptRegistry | hot-path | 待迁移；按 #1292 定义操作面 |
| `initialPromptConfiguration` | session | PromptRegistry | hot-path | 待迁移；按 #1292 定义操作面 |
| `historyRenderer` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextModelHost` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `prepareModelHost` | session | PrepareModelHost | hot-path | 待迁移；按 #1292 定义操作面 |
| `prepareContextHost` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `compactionStrategyHost` | session | ProductSummaryStrategy | hot-path | 待迁移；按 #1292 定义操作面 |
| `faultReporter` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |

### 3.6 入口、secret 与宿主资源：仍需 data face
原因 `control`（入口/控制面）、`secret`（凭据/token）或 `host-resource`（FS 索引、活进程）。`managedProviders` 的搜索走新的 `managedSearch`；账号与 token 仍留在这个 internal 名字上。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `serverControl` | process | ServerControl | control | 留宿主；入口/控制面 |
| `connectionHub` | process | Connection | control | 留宿主；入口/控制面 |
| `pluginFileTransactions` | process | PluginFiles | host-resource | 留宿主；本地资源 |
| `publication` | process | McpPublication | host-resource | 留宿主；本地资源 |
| `mcpAuth` | process | Mcp | secret | 留宿主；只暴露操作 |
| `grokOAuthService` | process | GrokOAuth | secret | 留宿主；只暴露操作 |
| `openAICodexOAuthService` | process | OpenAICodexOAuth | secret | 留宿主；只暴露操作 |
| `uiHost` | process | UiHost | control | 留宿主；入口/控制面 |
| `uiExtensions` | process | UiExtensions | control | 留宿主；入口/控制面 |
| `mentionIndex` | process | WorkspaceToolsOwner | host-resource | 留宿主；本地资源 |
| `skillsCatalog` | process | SkillsCatalog | host-resource | 留宿主；本地资源 |
| `mcpService` | process | Mcp | host-resource | 留宿主；本地资源 |
| `hooksService` | process | HooksSettings | host-resource | 留宿主；本地资源 |
| `providerService` | process | Provider | secret | 留宿主；只暴露操作 |
| `providerConnectionResolver` | process | Provider | host-resource | 留宿主；本地资源 |
| `collaboration` | process | Collaboration | host-resource | 留宿主；本地资源 |
| `sessionLoader` | session | SessionScope | control | 留宿主；入口/控制面 |
| `timelineFactory` | session | SessionScope | control | 留宿主；入口/控制面 |
| `sessionSkills` | session | Skills | host-resource | 留宿主；本地资源 |
| `skillsBranch` | branch | SkillsBranch | host-resource | 留宿主；本地资源 |
| `managedProviders` | process | ManagedProviders | secret | 留宿主；只暴露操作 |

### 3.7 待迁移的数据内容
原因 `legacy-no-consumer`：内容是数据，但还没有转成 data face。没有 leaf 消费者不是保留 internal 的理由；这些名字按批次迁移，`legacy-no-consumer` 只是迁移状态，不是永久分类。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `eventSubscriptions` | process | eventhub | legacy-no-consumer | 按 #1292 定义操作面 |
| `sessionLanguage` | session | PromptSources | legacy-no-consumer | 按 #1292 定义操作面 |
| `executionSettingsHost` | process | ExecutionSettings | legacy-no-consumer | 按 #1292 定义操作面 |
| `toolchainSourceHost` | process | ToolchainSource | legacy-no-consumer | 按 #1292 定义操作面 |
| `promptSuggestionSettingsHost` | process | PromptSuggestionSettings | legacy-no-consumer | 按 #1292 定义操作面 |
| `selectableModelsHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `modelSelectionHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `modalityConnectionHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `worktreeAssociations` | process | SessionStore | legacy-no-consumer | 按 #1292 定义操作面 |
| `listenAddresses` | process | Transport | legacy-no-consumer | 按 #1292 定义操作面 |
| `credentialVariablesHost` | process | root | legacy-no-consumer | 按 #1292 定义操作面 |
| `reservedMethods` | process | — | legacy-no-consumer | 按 #1292 定义操作面 |
| `agentConfig` | session | SessionScope | legacy-no-consumer | 按 #1292 定义操作面 |
| `collaborationConfig` | session | SessionScope | legacy-no-consumer | 按 #1292 定义操作面 |
| `skillsConfig` | session | SessionScope | legacy-no-consumer | 按 #1292 定义操作面 |
| `collaborationDirectory` | process | CollaborationDirectory | legacy-no-consumer | 按 #1292 定义操作面 |
| `sessionClientsHost` | process | Connection | legacy-no-consumer | 按 #1292 定义操作面 |
| `collaborationSession` | session | CollaborationSession | legacy-no-consumer | 按 #1292 定义操作面 |

## 4. 第一批改造（#1292）

每个服务一个 PR；数据面委托现有实现，不写第二份业务逻辑。测试在现有 flow 上扩展，并加一个 loopback session 的跨边界用例。

### 4.1 `settingsService`：不迁移

`GoalBranch` 保留类型化 `SettingsService`。`settings.read` 自己建取消 token、返回 `SettingsResult<T>` 信封（`{ok:true,value}` / `{ok:false,error}`）；换成数据面会丢掉分支的 stop token，也要解一层信封。`settings` 已经是其他 leaf 可用的数据面，不需要为 GoalBranch 再改。

测试：Goal 现有的设置读取与上限测试保持；不为这一项加 loopback。

### 4.2 `modelHost`：不迁移

`PromptSuggestion` 保留 `ModelHost` 和 `ProviderConnectionResolver`。它克隆整轮 `ModelRequest`（provider、model、system、tools、config、cached prefix）并流式读取；`auxiliaryModel.complete` 没有 tools/config/prefix，换过去会静默改变行为。它的设置是 `enabled`/`maxChars`（`prompt_suggestion.rs:61-63`），模型来自当轮请求，不是设置；文中不再写成 `enabled`/`model`。

如果将来要让 PromptSuggestion 进 leaf，先扩展数据面承载完整请求、tools、config、取消，并加覆盖 tools/config 的测试；不要只换名字。

### 4.3 `scheduleJobs`

按 [Scheduler](plugins/Scheduler.md) 已接受的形状，不在这里重定义：

```text
scheduleJobs.call(JobRequest)
  JobRequest = {action, job_id?, job?, enabled?, limit?, caller: {sessionId, branchId, cwd}}
  -> JobReply = {content, records}
  -> error {code, message}
```

这是 Scheduler 的 owner 形状：一个 flat `JobRequest`。Rust 实现是 `ScheduleJobs::call(request, caller)`，数据面的 dispatcher 负责把 `{request, caller}` 拼成这个 flat request；不要把 Rust 的两参数签名当成 wire 形状。`Caller` 补 serde 并加 `#[serde(rename_all = "camelCase")]`，与 owner 的 `sessionId`/`branchId` 一致。

改动：

- 给 `ScheduleJobs` 加 `#[rutis::service]` 数据面（或手写 dispatcher），在 `host_key("scheduleJobs")` 提供；本地仍用 typed。
- `ToolRequest` 已有 `Deserialize`；`Caller`、`JobReply` 补 serde derive。字段按 Scheduler 附录，不另起一套。
- `SchedulerSession` 改注入 `dyn HostDispatch`；调用传 `caller` 和取消。
- 测试：Scheduler 的 Session 工具 flow；离线/取消。

### 4.4 `workflowRuns`：已经是数据面

optional Workflow 包（`runtime/plugins/workflow`）已经提供 `workflowRuns`，`call: async`；`RunRequest`/`RunReply` 与 [Workflow](plugins/Workflow.md) 一致。本项不新增 provider，也不改 WorkflowSession 的注册；只核对形状：

```text
workflowRuns.call(RunRequest)
  RunRequest = {action, name?, source?, params?, run_id?, caller: {sessionId, branchId, callId, cwd}}
  -> RunReply = {content, structured?}
  -> error {code, message}
```

### 4.5 `sessionFiles`

按 owner 契约 [Session files](appserver/session-files.md) §9.1 暴露 `sessionFiles`，委托现有 `SessionFilesHost`；`SessionFilesHost` 保持 Rust 类型键。

```text
sessionFiles.put {sessionId, name, content: base64 string}
  -> {fileId, path, bytes}
sessionFiles.register {sessionId, path}
sessionFiles.owns {sessionId, path} -> {owned: bool}
sessionFiles.owned_paths {sessionId} -> {paths: [string]}
  -> error {code: not_found | invalid | too_large | node_unavailable | cancelled | storage, message}
```

- 名字、`content`（base64）、`owned_paths` 和取消都按 §9.1；取消是协议动作（调用方丢弃调用或发 bridge cancel），不是 `cancel?` 字段。
- 路径是 UTF-8 字符串。16 MiB 上限不变；先测是否超过 Channel 单条消息上限，超了在 owner 契约里加分块，不偷偷截断。
- 消费者：Attachments 已声明 `P:sessionFiles`；WorkspaceTools/ShellSession（`register`）、FileAccessFacts（`owns`/`owned_paths`）。
- 测试：`session_files_flow.rs`、`attachments_flow.rs`，加 loopback。

### 4.6 `managedSearch`

不把 `managedProviders` 变成 shared；新增一个名字，只含搜索：

```text
managedSearch.search {query, count}
  -> {results: [{title, url, snippet, publishedAt?}]}
  -> error {code: unavailable | search_failed, message}
```

- 取消是协议动作（调用方丢弃调用或发 bridge cancel），不是请求字段。
- `managedProviders` 保持 internal(secret)：账号、登录、token、租约仍在类型化面。
- Web 的搜索改调 `managedSearch`；Web 的 settings 仍走 `settings`。
- 新名字使 catalog 多一行；它不在当前 91 个 internal 名字里。

### 4.7 `wakeups`

`SchedulerSession` 同时注入 `ScheduleJobs` 和 `Wakeups`；只有 `scheduleJobs` 数据面时它仍不能 leaf。按 Scheduler §10.2 的方法表：

```text
wakeups.create {sessionId, branchId, at, prompt} -> {wakeupId}
wakeups.list {sessionId, branchId?} -> {wakeups: [{wakeupId, at, prompt, state}]}
wakeups.cancel {sessionId, wakeupId} -> {} | {state}
  -> error {code: invalid_params | wakeup_not_found | ..., message}
```

按 Scheduler §10.2 的方法表，不按 §11.1 的通知表。`branchId` 默认 `main`；`at` 是 RFC 3339；`cancel` 结算 `scheduled` 记录时返回 `{}`，记录已经终态时返回 `{state}`。

改动：给 `Wakeups` 加数据面（或手写 dispatcher），在 `host_key("wakeups")` 提供；`SchedulerSession` 改注入 `dyn HostDispatch`。`WakeupRequest` 补 serde（camelCase）；`Wakeup` 是持久化记录，已经 snake_case，**不要**给它加 `rename_all`，data face 的 record 用单独的 wire 类型或显式映射。测试：wakeup 的 create/list/cancel flow，加 loopback。

## 5. 验证

- `xtask check`：双向测试（dispatcher 未 shared 失败；shared 无 dispatcher 失败，目标没有例外；shared+reason 失败；reason 不在闭集失败）。
- `xtask graph`：catalog 的 `face` 列与真实装配一致；`catalog.md` 的 diff 作为评审材料。
- 每个服务：现有 flow 测试 + 一个 loopback session 的跨边界用例；不新增 mock-only 测试。
- 数量：不改 `cargo test` 的总量要求；按受影响 crate 跑，最终 `cargo test --workspace --locked`。
- 改装配后跑 `cargo run --locked -p xtask -- check` 与 `graph`。

## 6. 风险与开放问题

1. **Channel 单条消息上限**：`sessionFiles.put` 的 16 MiB 可能超过限制。先测；超了就分块，不降低上限。
2. **secret**：secret 的值留在 owner；`providerService`、`mcpAuth`、OAuth 服务、`managedProviders` 仍要有 data face，只暴露操作，不返回值。
3. **`workflowRuns` 已经是数据面**：optional Workflow 包已提供；#1292 只核对形状，不新增 provider。
4. **`Faces` 的传播**：`Names`、`PartRegistry`、`SessionResolver` 都要拿到同一份原因表；独立 host / 测试用空表，检查只在装配和 leaf 加载处生效。
5. **legacy 标记**：当前 internal 名字全部是迁移待办。`legacy-no-consumer` 只是状态，不是永久 internal；目标目录里没有 internal。
6. **shared 与 dispatcher 不一致**：`managementMethods`、`localId` 已在 #1289 变成 `data`；`loaderService`、`pluginMounter` 是当前仅有的两个 `shared-no-dispatcher` 待补 dispatcher，不是永久例外。

## 7. 交付顺序

1. **#1289**：`Faces`、`AgentBuilder::internal`、catalog `face`、双向 `xtask check`、leaf 拒绝、`managementMethods` 注册顺序、`localId` dispatcher；`loaderService`/`pluginMounter` 两个待补 dispatcher。
2. **#1300**：按 §3 的分组在 catalog 里补齐 reason；`xtask graph` 的 diff 就是盘点结果。此 PR 不改服务。
3. **#1292**：把所有名字迁移成 data face。先做 `scheduleJobs`、`wakeups`、`sessionFiles`、`managedSearch`，再按 owner 铺开；没有“确实不能跨”的永久例外，只有还没做的工作。`workflowRuns` 已是数据面，只核对。
4. 每项都带现有 flow 测试；不新增适配层，不写第二份逻辑。
