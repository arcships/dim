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

`internal` **不是“写个原因”**，而是：这个服务面不能被另一个语言或进程实现、提供或注入。它不能借给 leaf，不能由 leaf 提供，不能跨作用域注入。原因解释为什么这个面不能数据化（状态机、锁内回调、OS 对象、secret、热路径）。功能可以在 leaf 里重新实现，但那是新的数据服务；同一进程内换一个同类型契约的 Rust 实现不算跨语言替换。作用域与它无关：instance-scoped 的 `timeline`/`dispatcher` 是数据，process-scoped 的 `hostPlatform` 是 `internal`。

## 2. 策略怎么落地（#1289）

### 2.1 标记

**数据面 = 名字在 catalog 里 shared，并且在 `host_key(name)` 上有一个 `HostDispatch`。** 两个条件缺一不可。`#[rutis::service]`、`register_shared`、`register_shared_instance` 都是标记写法，但 shared 本身不是数据面：`loaderService`、`pluginMounter` 今天是 shared 却没有 dispatcher，leaf 不能调用它们。

`internal` 需要一条原因，放在 `AgentBuilder` 的旁表里，不改 rutis 的 `ServiceCatalog`：

```rust
pub struct Faces {
    reasons: HashMap<String, &'static str>,
}

impl AgentBuilder {
    /// Marks the name of T as internal, with the reason a face cannot cross.
    pub fn internal<T: ?Sized + 'static>(&mut self, reason: &'static str);
    /// The same for a name that has no Rust type (a hand-written dispatcher).
    pub fn internal_name(&mut self, name: &str, reason: &'static str);
    pub fn faces(&self) -> &Faces;
}
```

各 crate 的 `register_names(builder: &mut AgentBuilder)` 在注册完名字后，对自己不共享的名字调用 `builder.internal::<T>(reason)`。`xtask` 和 App Server 用的是同一个 builder，所以 reason 只有一份。

### 2.2 catalog 与检查

- `xtask graph` 把 `Crosses languages` 列换成 `face`：
  - shared 且有 dispatcher：`data`；
  - 非 shared 且有 reason：`internal(<reason>)`；
  - shared 但没有 dispatcher：`shared-no-dispatcher`；除 `loaderService`、`pluginMounter` 两个 rutis-loader 外部名字外，生成失败；
  - 非 shared 且没有 reason：生成失败，报名字。
- `xtask check` 双向检查：
  - 一个在 `host_key(name)` 上有 dispatcher、但没有 shared 的名字失败（`managementMethods` 是这一类）；
  - 一个 shared、但没有 dispatcher 的名字失败；`loaderService`、`pluginMounter` 是仅有的两个例外，catalog 显示 `shared-no-dispatcher`；
  - 一个名字同时 shared 和有 reason 是矛盾，失败。
- `managementMethods` 不是只加一行：`session_typescript.rs:80` 已经 `register_shared`，但 `xtask/src/catalog.rs:81` 在 `serve` 注册 typed 名字之前调用 `share_names`，后者会被覆盖。改动是让 shared 注册发生在 typed 注册之后，或让 `register_names` 保留已有的 shared；这是 #1289 的代码项。
- `localId` 由 root 的 `get` dispatcher 变成 `data`。`loaderService`、`pluginMounter` 保持 `shared-no-dispatcher`：它们是 rutis-loader 的外部名字，leaf 不能调用，去掉或补 dispatcher 需要上游改动。检查只对这两个名字放行。
- `xtask/src/catalog.rs` 的 `crosses` 字段改名 `face`，枚举 `Data | Internal(&'static str) | SharedNoDispatcher`。

### 2.3 leaf 的运行时拒绝

- `PartRegistry` 挂载 leaf 部件前，`Names` 解析每个 `inject` 名字：`face != data` → 失败，`ErrorKind::PluginFailed`，消息以 `internal_service:` 开头并写明名字和原因。
- 一个 leaf 行声明 `provides` 的名字时，同样要求 data；否则它的服务在宿主侧不可解析，直接拒绝，不等到第一次调用。
- `Names` 需要读到 `Faces`：新增 `Names::copy_with_faces(catalog, build, faces)`；`Names::copy` 保持原签名并传空表。不要给 `copy` 加可选参数（Rust 没有），也不要改所有调用点。

### 2.4 作者侧与原因词汇

- 新服务默认写数据面；有 leaf/跨语言消费者时不需要另写适配层。
- 只有确实无法数据化的面才标 `internal`，原因必须是闭集里的一个：
  - `object`：句柄/OS 对象，需要拆操作；
  - `hot-path`：每回合/每条输入的状态机或严格本地顺序；
  - `lock-callback`：commit 锁内同步回调；
  - `secret`：凭据/token，只暴露操作；
  - `owner-api`：类型化所有者 API，已有数据兄弟，或只给本地用；
  - `control`：宿主入口/控制面；
  - `host-resource`：FS 索引、活进程等宿主资源；
  - `legacy-no-consumer`：内容是数据，但还没有 leaf 消费者。
- 模板和 PR 清单固定一问：“公开服务的方法只有数据；句柄留在哪一层内部？”

### 2.5 存量迁移

存量不一次改完。每个非 shared 名字按 §3 的分类标 reason（`object`/`hot-path`/`secret`/…）；#1300 的清单就是映射，出现 leaf 消费者时再改数据面。这样 `xtask check` 从第一天就能拦住“新写的服务忘了形状”，而不用等所有名字全部迁完。

## 3. 盘点（#1300）

catalog 现在 118 个名字：30 data、86 internal、2 shared-no-dispatcher。§3 表覆盖这 86 个 internal 名字；§3.2 的 `scheduleJobs`、`wakeups` 目前还是 internal(owner-api)，#1292 转成数据面；`sessionFiles`、`managedSearch` 是 #1292 的新名字，不在当前 catalog。`workflowRuns` 不在其中：它是 optional Workflow 包自己的 provides 目录，已经是数据面。每个名字在分类表里恰好一行；说明句和“数据兄弟”列是引用，不算第二行。

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

同一批还包括宿主端口改成数据面（§4.8）：`executionSettingsHost`、`toolchainSourceHost`、`promptSuggestionSettingsHost`、`selectableModelsHost`、`modelSelectionHost` 从 `legacy-no-consumer` 变成 `data`。这五项不改名字、不新增 catalog 行。`modalityConnectionHost` 不迁移（§4.8 末段）：它的回复带连接凭据，改成 `internal(secret)`。

两个 owner API **不进第一批**：它们保留类型化调用。`settings.read` 会新建取消 token、并返回 `SettingsResult` 信封；GoalBranch 需要自己的 stop token，改成数据面会丢取消。`auxiliaryModel.complete` 没有 tools、config 和 cached prefix；PromptSuggestion 克隆的是整轮 `ModelRequest`，换过去是行为变化，不只是形状。它们已有数据兄弟给其他消费者；有 leaf 消费者时再按 §4 的规则扩展。

### 3.3 所有者 API，已有数据兄弟

本组统一 reason `owner-api`。

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
| `hostPlatform` | `ProcessTree`、`PtyProcessTree`、临时目录 | `start`/`wait`/`kill`/`output`/`shell`/`write`/`resize` |
| `fileStorage` | `FilePath`、根与代次 | `open`/`read`/`write`/`close`/`list`/`remove` |
| `sqliteStorage` | 连接与事务 | store 端口（#1298） |
| `timelineStore` | 连接与批次 | `append`/`query`/`ack`（#1298） |
| `toolHost` | `ToolDriver` | `execute` 数据面 |
| `mcpSession` | 会话对象 | 每 server 的 id 操作 |
| `backgroundExec` | 任务对象 | task id 操作 |
| `approval`、`permission` | 决策状态 | `decide` 数据面 |
| `inputQueue` | 队列状态 | `submit`/`withdraw` 数据面 |
| `wait` | `WaitResult`、`CancellationToken` | `start`/`wait`/`result` 操作面 |
| `promptSourcesBranch` | 分支 prompt 装配 | 留宿主；`hot-path` |

这些面即使有数据接口，实例核心仍默认留在宿主；它们列在这里是为了说明“对象形状”不是永久 internal，而是需要操作化；原因 `object`。

### 3.5 热路径与顺序：契约上 internal
原因 `hot-path`：每回合/每条输入的状态机、严格本地顺序，或同步回调。跨了要重写顺序和失败语义，收益不成比例。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `agentLoop` | branch | Loop | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `loopState` | branch | Loop | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `modelProvider` | branch | ModelProvider | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `prompt` | branch | Prompt | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `contextPreparation` | branch | ContextPreparation | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `toolBinding` | branch | ToolBinding | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `toolExecutor` | branch | ToolExecutor | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `branchOpening` | branch | BranchScope | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `configurationPublisher` | branch | ContextPreparation | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `configureControl` | branch | Prompt | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `fileReads` | branch | FileReads | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `branchLine` | branch | BranchLine | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `submitChecks` | session | InputMethods | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `commandRulesHost` | session | CommandRules | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `promptHost` | session | PromptRegistry | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `promptRegistry` | session | PromptRegistry | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `initialPromptConfiguration` | session | PromptRegistry | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `historyRenderer` | session | SessionScope | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `contextModelHost` | session | SessionScope | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `prepareModelHost` | session | PrepareModelHost | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `prepareContextHost` | session | SessionScope | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `compactionStrategyHost` | session | ProductSummaryStrategy | hot-path | 留宿主；有 leaf 消费者时再加数据面 |
| `faultReporter` | session | SessionScope | hot-path | 留宿主；有 leaf 消费者时再加数据面 |

### 3.6 入口、secret 与宿主资源
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
| `modalityConnectionHost` | process | Provider | secret | 留宿主；连接带凭据，只留类型化端口 |

### 3.7 数据内容，但还没有 leaf 消费者
原因 `legacy-no-consumer`：内容是数据，但还没有 leaf 消费者。它们不是“永远不能跨”。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `eventSubscriptions` | process | eventhub | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `sessionLanguage` | session | PromptSources | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `worktreeAssociations` | process | SessionStore | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `listenAddresses` | process | Transport | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `credentialVariablesHost` | process | root | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `reservedMethods` | process | — | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `agentConfig` | session | SessionScope | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `collaborationConfig` | session | SessionScope | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `skillsConfig` | session | SessionScope | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `collaborationDirectory` | process | CollaborationDirectory | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `sessionClientsHost` | process | Connection | legacy-no-consumer | 有 leaf 消费者时加数据面 |
| `collaborationSession` | session | CollaborationSession | legacy-no-consumer | 有 leaf 消费者时加数据面 |

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
- 新名字使 catalog 多一行；它不在当前 86 个 internal 名字里。

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

### 4.8 宿主端口：`executionSettingsHost` 等五个

这五个名字的内容本来就是数据（§3.7），没有 leaf 消费者时留在宿主；这一批把它们做成数据面，名字不变。每个名字只有一份定义：端口类型持有 owner 的读（闭包形式，没有第二个 owner trait），它的 dispatcher 在同一个模块里写出 wire 形状，Rust 绑定是这个 dispatcher 的薄客户端——调用同一个 operation，不写第二份逻辑。调用方拿到的回复是 `cancellable_future`：丢弃等待就取消这次调用，owner 看到的 token 随之取消（`drop_guard`），所以一次被放弃的读不会在宿主里跑下去。

```text
executionSettingsHost.read({}) -> {sandbox, network}
  -> error {code: failed, message}
toolchainSourceHost.list({}) -> {toolchains: [{bin, root}]}          # sync
  -> error {code: failed, message}
promptSuggestionSettingsHost.read({}) -> {enabled, maxChars}
  -> error {code: failed, message}
selectableModelsHost.selectableModels({}) -> {models: [ModelCapabilitySnapshot]}
selectableModelsHost.selectableForModality({}) -> {models: [ModalityModelSnapshot]}
modelSelectionHost.resolve({explicit?: ModelId, cwd?}) -> ModelSelection
  -> error {code: cancelled | owner_closed | invalid_request | model_unavailable | failed, message}
```

- 每个 operation 只收一个请求对象；没有输入的写 `{}`（`provider-management.md` §3.6 的形状），多一个参数或未知字段都是 `invalid_request`。
- 取消是协议动作：调用方丢弃等待，或被取消的绑定调用不再进入 owner；owner 自己的生命周期仍然约束它启动的 worker。
- 回复就是 owner 的类型：`ExecutionSettings`、`PromptSuggestionSettings`、`ModelCapabilitySnapshot`、`ModalityModelSnapshot`、`ModelSelection` 都是 serde camelCase 数据。`InstalledToolchain` 的 `PathBuf` 换成 UTF-8 字符串（`{bin, root}`）：某个 runtime 的目录不是 UTF-8 时只跳过那一项，其余照常给子进程用；`cwd` 的请求方向同理，`ModelSelectionRequest::at` 是 Rust 调用方换字段的唯一一处。
- 提供方仍是原来的部件（App Server 的 `ExecutionSettings`、`ToolchainSource`、`PromptSuggestionSettings`、`Provider`，以及 aimux 的 `ConfiguredModels`）：`provide` 一次给出类型键上的绑定和 `host_key(name)` 上的 face；没有适配层，也没有第二份业务逻辑。
- `Names::registered` 现在把 shared 名字也算“assembly 注册了它”：同名从类型键改成 data face 后，可选依赖仍能认出部件存在（PromptSuggestion 的设置端口就是这样，`prompt_suggestion.rs`）。
- 测试：`host_port_faces.rs` 逐个比对“Rust binding 的结果 == wire 的结果”，钉住 `{}` 请求、字段拼写和错误码，并用一个持有 token 的 owner worker 证明丢弃等待会取消 owner 的读；`provider_flow.rs`、`managed_oauth_flow.rs`、`dim-modality` 与 aimux 的 flow 用真实 Provider 装配跑同样的读。

**`modalityConnectionHost` 不迁移。** 它的回复是 `ModalityConnection`，含解析后的连接（header、bearer/apiKey、Bedrock 凭据）；把它做成 shared 数据面等于把凭据按名字交给任何 leaf，这不是一个调用范围内可撤销的能力，和 §2.4 的 `secret`（只暴露操作，不暴露值）冲突。要转成数据面，先要 owner 定一个 capability-gated operation（Provider 或宿主代为发起这次请求，凭据不出现），并写进 owner 文档；在此之前它保持 `internal(secret)` 的类型化端口。

## 5. 验证

- `xtask check`：双向测试（dispatcher 未 shared 失败；shared 无 dispatcher 失败，两个外部名字除外；shared+reason 失败；reason 不在闭集失败）。
- `xtask graph`：catalog 的 `face` 列与真实装配一致；`catalog.md` 的 diff 作为评审材料。
- 每个服务：现有 flow 测试 + 一个 loopback session 的跨边界用例；不新增 mock-only 测试。
- 数量：不改 `cargo test` 的总量要求；按受影响 crate 跑，最终 `cargo test --workspace --locked`。
- 改装配后跑 `cargo run --locked -p xtask -- check` 与 `graph`。

## 6. 风险与开放问题

1. **Channel 单条消息上限**：`sessionFiles.put` 的 16 MiB 可能超过限制。先测；超了就分块，不降低上限。
2. **secret**：`providerService`、`mcpAuth`、OAuth 服务、`managedProviders` 保持 internal；数据面只暴露操作，不暴露值。
3. **`workflowRuns` 已经是数据面**：optional Workflow 包已提供；#1292 只核对形状，不新增 provider。
4. **`Faces` 的传播**：`Names`、`PartRegistry`、`SessionResolver` 都要拿到同一份原因表；独立 host / 测试用空表，检查只在装配和 leaf 加载处生效。
5. **legacy 标记**：当前 86 个 internal 名字里 12 个是“数据内容、无消费者”，标 `legacy-no-consumer`；不是永久 internal。
6. **shared 与 dispatcher 不一致**：`managementMethods`、`localId` 已在 #1289 变成 `data`；`loaderService`、`pluginMounter` 是仅有的两个 `shared-no-dispatcher` 例外，检查不放行其他 shared-without-dispatcher。

## 7. 交付顺序

1. **#1289**：`Faces`、`AgentBuilder::internal`、catalog `face`、双向 `xtask check`、leaf 拒绝、`managementMethods` 注册顺序、`localId` dispatcher；`loaderService`/`pluginMounter` 作为两个外部 `shared-no-dispatcher` 例外。
2. **#1300**：按 §3 的分组在 catalog 里补齐 reason；`xtask graph` 的 diff 就是盘点结果。此 PR 不改服务。
3. **#1292**：按 `scheduleJobs` → `wakeups` → `sessionFiles` → `managedSearch` 的顺序，一项一个 PR；宿主端口的数据面（§4.8）是同一批的独立 PR。`workflowRuns` 已是数据面，只核对；`settingsService`、`modelHost`、`modalityConnectionHost` 不迁移。
4. 每项都带现有 flow 测试；不新增适配层，不写第二份逻辑。
