# 服务面：跨语言形状的策略、盘点与第一批改造

进程服务的目标调用方、替代名称与提供方见[进程操作](contracts/process-owner-operations.md)。

状态：Draft。对应 [#1289](https://github.com/arcships/dim/issues/1289)（标记与检查）、[#1300](https://github.com/arcships/dim/issues/1300)（盘点）、[#1292](https://github.com/arcships/dim/issues/1292)（迁移）。父 issue [#1288](https://github.com/arcships/dim/issues/1288)。研究背景见 `docs/design/research/remote-plugins.md`（由 [#1299](https://github.com/arcships/dim/pull/1299) 引入）。

本文是改动设计，不是实现记录。所有 API 都是目标形状；现有代码位置用于说明改哪里。

## 1. 规则

1. **数据面是默认**。公开服务的方法是 serde 数据：参数、返回值、错误都是普通类型；不出现句柄、`Arc<dyn Trait>`、`&mut`、迭代器、通道、`PathBuf`/`OsString`。
2. **身份是值**。Session、branch、task、file、root、subscription 都是 id；id 不是授权。
3. **操作代替对象**。进程、PTY、文件、store 都是一组操作加 id；对象和句柄留在所有者内部。
4. **流是事件或订阅**。大输出分页；回调可以按引用跨语言，但不在 commit 锁里同步调用。
5. **错误是数据**。`{code, message}`；转发方原样抛。
6. **取消是调用**。显式 cancel，或调用方丢弃。

## 2. 策略（#1289）

### 2.1 目标

**Every public plugin service is a data face.** A data face is a name shared in the catalog **and** a
`HostDispatch` at `host_key(name)`. Deployment determines which links can reach the face.

### 2.2 Migration marker

During migration, an existing entry may carry `internal(reason)`:

- `object`, `hot-path`, `lock-callback`, `secret`, `control`, `host-resource`, `legacy-no-consumer`
  and `owner-api` describe the remaining migration work.
- New public services start as data faces. A batch can expose a face before it has a consumer.
- Secret values stay with the owner. Business methods expose operations that use those values. The storage exception for trusted plugins is limited to the [Provider and Settings persistence operations](contracts/resource-owner-operations.md#credential-values-in-replies).

### 2.3 Checks

`xtask check` classifies the current catalog:

- shared + dispatcher → `data`;
- shared without dispatcher → failure;
- dispatcher without shared → failure;
- not shared + known migration reason → `internal(reason)`;
- not shared without a reason → failure;
- shared and internal at once → failure.

A Rust type key is catalogued only when its registered name maps to that key, or its binding has
a matching dispatcher. The catalog's shared-name fallback does not turn private implementation
keys into public services. The target catalog has only `data` rows.

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

### 2.6 Migration rule

For each legacy catalog entry, the owner identifies the operations plugins should be able to depend
on. Publish those operations through one data face, reusing an existing face when it covers the
contract. Remove the old catalog entry when its role is a private implementation, a typed binding
or a duplicate. Private wiring stays within its owner, using the
[private-name rules](rutis-usage.md#35-how-each-runtime-resolves-a-name) when a key is needed.
Preserve the owner's cancellation, ordering and lifecycle semantics when moving callers.

## 3. 盘点（#1300）

以下按迁移工作分组，每个名字只在盘点表中列一处。批次进度见 [#1292](https://github.com/arcships/dim/issues/1292)。

### 3.1 基础数据面与上游登记

| 名字 | 处理 |
| --- | --- |
| `managementMethods` | 数据面，shared 名字与 dispatcher 一同注册。 |
| `localId` | 数据面，由 root 提供 `get`。 |
| `rutis/loader` | 插件管理数据面；`LoaderService` 是提供方的私有 Rust 类型，不是另一个服务名。 |
| `PluginMounter` 类型键 | rutis-loader 与 rutis-bridge 内部挂载插件定义的装配键，不是公共服务目录项。 |

插件管理复用 `rutis/loader`。类型键是否属于公共目录由实际名称与 dispatcher 决定；不能仅凭类型名查询时的 shared 回退制造一个公共服务名。私有挂载机制遵循 §2.6，不新增面向插件的挂载操作。

### 3.2 第一批数据面（#1292）

| 名字 | 目标 | 备注 |
| --- | --- | --- |
| `scheduleJobs` | `scheduleJobs.call(JobRequest)` | 按 [Scheduler](plugins/Scheduler.md#a2-tool-schedule) 的 flat `JobRequest`/`JobReply`，与 `wakeups` 同一批次（[#1396](https://github.com/arcships/dim/pull/1396)）。 |
| `wakeups` | `wakeups.create/list/cancel` | 形状按 [Scheduler](plugins/Scheduler.md#102-methods-and-tool) §10.2。 |
| `loops` | `loops.start/next/list/stop` | 形状按 [Scheduler](plugins/Scheduler.md#126-interfaces) §12.6；`start`、`next`、`stop` 是迁改，`list` 是读取。投递 pass、恢复与 `settle_delivering` 不在此面。 |
| `sessionFiles` | `sessionFiles.put/import/read/register/owns/owned_paths` | 形状按 [Session files](appserver/session-files.md) §9.1，`SessionFiles` 是进程内绑定。 |
| `commandRules` | `commandRules.read` | Session 命令规则读取，按 [命令规则](appserver/permission-command-rules.md#41-language-neutral-adr-002-d5) §4.1。 |
| `managedSearch` | `managedSearch.search` | 凭据由实现持有，见 §4.6。 |
| `agentConfig` | `agentConfig.get()` | 形状见 §4.8。 |
| `sessionLanguage` | `sessionLanguage.get()` | 形状见 §4.8。 |
| `skillsConfig` | `skillsConfig.get()` | 形状见 §4.8。 |
| `collaborationConfig` | `collaborationConfig.get()` | 形状见 §4.8。 |
| `worktreeAssociations` | `store.associateWorktree` | 形状见 §4.8。 |
| `toolSourcesView` | `toolSourcesView.get/observe` | 同一 ToolRegistry owner 的独立只读观察面；贡献注册仍由 `toolRegistry` 负责，见 [工具贡献 §7.5](../architecture/rust-agent/tool-contributions.md#75-服务接口)。 |

宿主读取端口的数据面（§4.10）包括 `executionSettingsHost`、`toolchainSourceHost`、`promptSuggestionSettingsHost`。模型选择与列表统一由 `providerService` 提供；调用方和配置见[进程操作](contracts/process-owner-operations.md)；其余读取端口沿用现名。

### 3.3 所有者 API，已有数据兄弟

本组统一迁移标签 `owner-api`。迁移批次先核对数据面能否覆盖原调用，再切换到同一组操作的绑定。

| 名字 | 数据兄弟 | 说明 |
| --- | --- | --- |
| `agentIndex` | `sessions` | 目录查询走 `sessions` |
| `settingsService` | `settings` | GoalBranch 的取消契约见 §4.1。 |
| `modelHost` | `modelHost` | 完整模型请求直接调用所选配置的执行入口；辅助请求使用 `providerService.complete`。 |
| `loader` | `rutis/loader` | 插件管理走数据面 |
| `sessionStore` | `store`、`sessionFiles`、`timelineStore` | 标题与 worktree 关联直接调 store |
| `treeCommands`、`timelineTreeControl` | `timelineTree` | 分支操作直接调用 Session 的树；不保留别名 |

### 3.4 对象/句柄：拆成操作加 id

Proposed target operations: [Resource and repository operations](contracts/resource-owner-operations.md).


| 名字 | 现在的对象 | 目标操作 |
| --- | --- | --- |
| `hostPlatform` | `ProcessTree`、`PtyProcessTree`、临时目录 | 服务自身的方法就是公开操作：`info`、`execShell`、`persistentShell`、`sandboxBackend`、`execRoot`、`spawn`/`wait`/`kill`/`settle`/`output`/`retainOutput`/`tail`、`shell`/`write`/`resize`、`spawnWithStdin`/`spawnConfined`/`removeOutputDirectory`/`childPath`；`spawn_tree`/`spawn_with_stdin_process`/`spawn_pty`/`build_process_spec`/`child_path_process` 保持 `#[service(skip)]`。不建 `execHost`（[resource and store services](resource-faces.md) §3，#1295） |
| `fileStorage` | `FilePath`、根与代次 | 数据面 `fsHost`：工具部分 `resolve`/`read`/`write`/`edit`/`glob`/`grep`/`mentions`，存储部分 `stat`/`readBytes`/`openRead`/`readAt`/`closeRead`/`lock`/`unlock`/`backup`/`createDirectory`/`replace`/`list`/`remove`（§4，#1297）；`fileStorage` 是提供方和 binding |
| `sqliteStorage` | 连接与事务 | Session 行走 `store`；原生 Agent、Provider 和 Settings 的记录操作直接调用 `sqliteStorage`，连接与事务由 SQLite 插件持有。`sqliteStorage.storageReport({})` 返回 SQLite 文件和 WAL 的容量指标（[存储契约](appserver/native-storage-format.md)） |
| `timelineStore` | 连接与批次 | Session 作用域的 `timelineStore` 数据面（resource-faces §9 允许与仓储族分名）：`createSession`/`sessionConfig`/`releaseSession`/`rollbackSession`/`append`（同步受理）/`commits`、`closeCommits`（按分支序的完成通知）/`head`/`branches`/`readPage`/`readSegments`/`query`/`trace`/`batchEnd`/`batchEndAt`/`saveSnapshot`/`latestSnapshot`；`TimelineStore` trait 是 binding（#1298） |
| `promptSourcesBranch` | 分支 prompt 装配 | 按 #1292 确认公共操作与装配内部职责。 |

这些行按当前对象形状分组。前四行的数据契约由 [resource and store data faces](resource-faces.md) 定义（#1295/#1297/#1298）。其余行按 #1292 逐批确定操作面。

这些对象服务由所有者提供操作面，Rust 绑定与跨语言调用使用同一实现：

| 名字 | 数据面 | 所有者保留的状态 |
| --- | --- | --- |
| `toolHost` | `execute(call, caller, cancelled)` | ToolDriver、工具声明 |
| `approval` | `pending()`、`decide(requestId, decision)` | 待处理请求、批次准入 |
| `permission` | `check(call, risk, cancelled)` | 预设、权限状态、advisor |
| `inputQueue` | `submit(input)`、`withdraw(command)` | 排序、恢复、InputSnapshot |
| `wait` | `wait(taskId, timeoutMs, cancelled)` | 到达记录、单次等待槽 |

形状与错误见所有者：[Permission](../architecture/rust-agent/plugins/Permission.md#permission-data-face)、
[Approval](../architecture/rust-agent/plugins/Permission.md#approval-data-face)、
[InputQueue](../architecture/rust-agent/plugins/InputQueue.md#data-face)、
[Wait](../architecture/rust-agent/plugins/Wait.md#data-face) 与
[toolHost](../architecture/rust-agent/tool-contributions.md#21-宿主工具的-toolhost-数据面)。
`inputQueue.withdraw` 只做普通撤回；Steer 必须走 `session.input.withdraw` 的校验与提交检查入口。

`mcpSession` 与 `backgroundExec` 的数据面以 id 加操作暴露：

| 名字 | 数据面 | 所有者把什么留在内部 |
| --- | --- | --- |
| `mcpSession` | `state()`、`refresh()`、`widget(published)`、`call(server, tool, arguments)` | 连接代次、已发布工具、发现与重连 |
| `backgroundExec` | `output(taskId)`、`outputPage(taskId, cursor, limit)`、`wait(taskId, timeoutMs)`、`stop(taskId)` | 任务表、task id/路径、事实与唤醒；不持有活进程或采集 |

形状与错误由所有者文档定义：[MCP Apps](appserver/mcp-apps.md#44-the-mcpsession-data-face)（`mcpSession`）和
[WorkspaceTools](../architecture/rust-agent/plugins/WorkspaceTools.md#data-face)（`backgroundExec`）。
`backgroundExec.start` 需要活的进程对象，仍是 `exec` 工具的操作，不在数据面上；`BackgroundExec` 只保存 branch task id、host task id 和 output path，实时读取走 `hostPlatform.tail`/`output`。`backgroundExec.stop` 是 kill。持久 PTY 由 `HostPlatform` 拥有，`ShellSession` 持有策略；公共 PTY 操作只有 `shell`/`write`/`resize`。
这些名字在 catalog 里是 `data`，Rust 类型仍由部件在同一实例按类型键读取。

`hostPlatform`、`fileStorage`、`timelineStore` 不按对象分类保留：服务本身就是公开契约；一个 service 的公开方法就是它的接口，没有单独的 face 对象，也没有第二套公开 API。凭据是操作内部用的能力，不是保留 `internal` 的理由。`hostPlatform` 的公开方法包括 `info`、`execShell`、`persistentShell`、`sandboxBackend`、`execRoot`，以及命令任务和持久 shell 的操作（见 §3.4）；`toolHost`、`backgroundExec`、`sessionMethods.shell` 通过该数据面调用它们。`hostPlatform`、`fileStorage` 的提供方在持有资源的那个 daemon（控制节点自己的插件，或 #1296 挂载的资源 leaf）；`store` 在持有 Session 状态的那个 daemon，SQLite 还是 PostgreSQL 只是实现（#1298，[resource and store services](resource-faces.md) §6）。

### 3.5 热路径与顺序：待操作化

Proposed target operations: [Session and branch operations](contracts/runtime-owner-operations.md).

原因 `hot-path`：每回合或每条输入上的状态机、本地顺序或同步回调。操作面的粒度须保留原有顺序和失败语义。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `agentLoop` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `loopState` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `prompt` | branch | Prompt | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextPreparation` | branch | ContextPreparation | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextUsage` | branch | ContextUsage | hot-path | 本地请求计数器、请求引用与按序统计；`session.context.usage` 是 App Server RPC。按 #1292 确认是否需要独立插件操作。 |
| `toolBinding` | branch | ToolBinding | hot-path | 待迁移；按 #1292 定义操作面 |
| `toolExecutor` | branch | ToolExecutor | hot-path | 待迁移；按 #1292 定义操作面 |
| `configurationPublisher` | branch | ContextPreparation | hot-path | 待迁移；按 #1292 定义操作面 |
| `configureControl` | branch | Prompt | hot-path | 待迁移；按 #1292 定义操作面 |
| `fileReads` | branch | FileReads | hot-path | 待迁移；按 #1292 定义操作面 |
| `historyRenderer` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextModelHost` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `prepareModelHost` | session | PrepareModelHost | hot-path | 待迁移；按 #1292 定义操作面 |
| `prepareContextHost` | session | SessionScope | hot-path | 待迁移；按 #1292 定义操作面 |
| `compactionStrategyHost` | session | ProductSummaryStrategy | hot-path | 待迁移；按 #1292 定义操作面 |

### 3.6 入口、凭据与宿主资源

按 #1292 逐批确定各项的公共操作，并按 §2.6 迁移登记。状态、资源和凭据留在各自的 owner。`managedSearch` 的搜索操作见 §4.6。`skillsCatalog` 的插件内容操作见 [Skills owner 契约](../architecture/rust-agent/plugins/Skills.md#plugin-content-data-face)。

| 名字 | scope | provider | reason |
| --- | --- | --- | --- |
| `serverControl` | process | ServerControl | control |
| `connectionHub` | process | Connection | control |
| `grokOAuthService` | process | GrokOAuth | secret |
| `openAICodexOAuthService` | process | OpenAICodexOAuth | secret |
| `uiHost` | process | UiHost | control |
| `uiExtensions` | process | UiExtensions | control |
| `mentionIndex` | process | WorkspaceToolsOwner | host-resource |
| `mcpService` | process | Mcp | host-resource |
| `hooksService` | process | HooksSettings | host-resource |
| `providerService` | process | Provider | 当前为 secret，迁移后通过数据操作使用凭据 |
| `providerConnectionResolver` | process | Provider | host-resource |
| `timelineFactory` | session | SessionScope | control |

## 4. 批次契约（#1292）

每批处理同一个 owner 的相关操作。

PromptSettings 的三个只读端口按 [prompt overrides §3.1](appserver/prompt-overrides.md#31-data-operations)
提供 `basePromptSettingsHost.base({})`、`skillsInstructionsSettingsHost.instructions({})` 和
`summaryPromptSettingsHost.systemPrompt({})`，回复为字符串或 `null`。它们共享现有 owner，
原生与跨语言入口调用同一读取方法；配置优先级、读取时机和失败回退保持 owner 契约。

### 4.1 `settingsService`：保留调用取消

迁移前 `GoalBranch` 继续用类型化调用，读取随 branch stop token 取消。设置 owner 在 #1292 的批次内确定如何将调用方取消传递给读取操作，并验证 `settings.read` 的 `SettingsResult<T>` 信封和现有错误语义。Goal 切到同一数据契约的 Rust 绑定时，覆盖分支停止中断设置读取的流程。

### 4.2 `modelHost`：保留完整模型请求

迁移前 `PromptSuggestion` 继续用类型化调用。模型 owner 在 #1292 的批次内定义能承载完整 `ModelRequest` 的公开操作，保留当轮 provider、model、system、tools、config、cached prefix、流式回复与取消。精简请求迁往 `providerService.complete`，随后移除 `auxiliaryModel`。PromptSuggestion 切到同一数据契约的 Rust 绑定时，覆盖 tools、config 和缓存前缀。

### 4.3 `scheduleJobs`

按 [Scheduler](plugins/Scheduler.md) 已接受的形状，不在这里重定义：

```text
scheduleJobs.call(JobRequest)
  JobRequest = {action, job_id?, job?, enabled?, limit?, caller: {sessionId, branchId, cwd}}
  -> JobReply = {content, records}
  -> error {code, message}
```

本地绑定与 wire 共用 flat `JobRequest` 的操作语义，保留调用者身份及取消。用 Scheduler 的 Session 工具流程验证。

### 4.4 `workflowRuns`：已经是数据面

optional Workflow 包（`runtime/plugins/workflow`）已经提供 `workflowRuns`，`call: async`；`RunRequest`/`RunReply` 与 [Workflow](plugins/Workflow.md) 一致。本项不新增 provider，也不改 WorkflowSession 的注册；只核对形状：

```text
workflowRuns.call(RunRequest)
  RunRequest = {action, name?, source?, params?, run_id?, caller: {sessionId, branchId, callId, cwd}}
  -> RunReply = {content, structured?}
  -> error {code, message}
```

### 4.5 `sessionFiles`

按 owner 契约 [Session files](appserver/session-files.md) §9.1 暴露 `sessionFiles`。`SessionFiles` 作为这组操作的进程内绑定。

文件导入、分块读取、归属操作的参数、回复、错误及取消语义均由 §9.1 定义。文件传输使用按路径导入与有界读取操作。

进程内绑定和 wire 的行为一致性由 `session_files_flow.rs` 与 `attachments_flow.rs` 验证。

### 4.6 `managedSearch`

`managedSearch` 只公开搜索操作：

```text
managedSearch.search {query, count}
  -> {results: [{title, url, snippet, publishedAt?}]}
  -> error {code: unavailable | search_failed, message}
```

- 取消是协议动作（调用方丢弃调用或发 bridge cancel），不是请求字段。
- `ManagedProviders` 在 owner 内处理登录、凭据和租约。搜索调用迁到 `managedSearch` 后，批次按 §2.6 收敛旧登记（[#1398](https://github.com/arcships/dim/pull/1398)）。
- Web 的搜索使用 `managedSearch`；进程内绑定直接调用同一类型化操作，wire 在边界解码与编码。绑定与 wire 共用取消行为，丢弃调用取消本次搜索，不取消调用方的父 token。Web 的 settings 仍走 `settings`。

### 4.7 `wakeups`

与 `scheduleJobs` 同批迁移。按 Scheduler §10.2 的方法表：

```text
wakeups.create {sessionId, branchId, at, prompt} -> {wakeupId}
wakeups.list {sessionId, branchId?} -> {wakeups: [{wakeupId, at, prompt, state}]}
wakeups.cancel {sessionId, wakeupId} -> {} | {state}
  -> error {code: invalid_params | wakeup_not_found | ..., message}
```

按 Scheduler §10.2 的方法表，不按 §11.1 的通知表。`branchId` 默认 `main`；`at` 是 RFC 3339；`cancel` 结算 `scheduled` 记录时返回 `{}`，记录已经终态时返回 `{state}`。

`Wakeup` 持久化字段继续使用 snake_case，数据面用上面的 wire 字段。测试覆盖 create、list、cancel 和 loopback。

### 4.7b `loops`

与 `wakeups` 同一做法：注册为 shared，在 `host_key("loops")` 提供 dispatcher，原生 `Loops` 在类型键继续提供。按 Scheduler §12.6：

```text
loops.start {sessionId, branchId?, prompt, everySeconds?} -> {loopId, branchId, state, round, prompt, nextAt?, lastRoundAt?, expiresAt, stoppedReason?, settledAt?}
loops.next {sessionId, loopId, delaySeconds?} -> 同一记录形状
loops.list {sessionId, branchId?} -> {loops: [记录…]}
loops.stop {sessionId, loopId} -> 记录形状，未结算时另带 {settled: false}
  -> error {code: invalid_params | loop_not_found | loop_limit | loop_invalid | ..., message}
```

`branchId` 默认 `main`；`delaySeconds` 缺省取工具的默认值。校验与 `session.loop.*` 共用同一个解析函数，所以两端对同一请求给出同一个错误码。投递 pass、恢复与 `settle_delivering` 是内部实现，不在此面。

### 4.8 旧主机端口：`agentConfig` 等五个

这五个端口的内容是配置或一次写入：所有者把操作绑定到端口类型上，`#[rutis::service]` 生成 dispatcher，名字 shared；Rust 调用方直接调同一个方法，不另写适配层。`collaborationSession` 的审批目标读取由 [Collaboration owner](subagent-tool-approval.md#31-data-face) 定义。

```text
agentConfig.get() -> {name, systemPrompt, model: {providerId, modelId}}
  -- Agent 的身份与默认模型；SessionScope 提供
sessionLanguage.get() -> {language?}
  -- 只读：更新还要写 Session 记录并让 Prompt 采用来源，仍走 session.language.update
skillsConfig.get() -> {instructions?}
  -- Skills 的 factory 文本；null 表示内置
collaborationConfig.get() -> {leaderDescription?, memberDescription?}
  -- Collaboration 的 factory 文本；null 表示内置
store.associateWorktree {sessionId, repository, root, subtree} -> {}
  -> error {code: failed, message}
  -- 路径按 §1 是 UTF-8 字符串；一次写入仍由 SessionStore 的原子事务保证
```

- `agentConfig` 在 runtime 的 catalog 注册 shared instance；`SessionScope` 已经提供值，`get` 返回同一个快照。 配置快照直接使用所有者的数据定义，接收端允许未知字段；公开类型名称的迁移见[插件配置类型兼容性说明](../release/README.md#plugin-configuration-type-compatibility)。
- `skillsConfig`、`collaborationConfig` 的注册从 typed 改成 shared instance；值仍由 `install_with_config` 的 `session_value` 提供。
- `sessionLanguage` 只把读做成面；值由 PromptSources 的 Session part 提供，branch 通过 `PromptSourcesBranch::language()` 读同一个 cell。写入不暴露，避免绕过 Session 记录和 Prompt 的采用。
- FileAccess 通过 `store.associateWorktree` 调用 SessionStore；Rust 与跨语言调用共享同一写入操作。迁移调用方时移除 `worktreeAssociations`。路径校验和原子关联由 SessionStore 负责。
- 测试：`crates/dim-agent-capabilities/tests/legacy_session_faces.rs` 与 `crates/dim-app-server/tests/legacy_session_faces.rs` 比较 typed binding 与 wire 的同一操作。

### 4.9 旧主机端口：`credentialVariablesHost` 等六个

六个进程端口的内容本来就是数据，所有者各自把读绑定成端口类型上的 operation，`#[rutis::service]` 生成 dispatcher，名字 shared；Rust 调用方直接调同一个方法，不另写适配层。`eventSubscriptions` 手写 `HostDispatch`，因为 owner 的错误是带动态 `code` 的 struct，生成面会把 `code` 写成类型名。

```text
credentialVariablesHost.variables() -> {variables: [string]}
  -- 只给变量名，永不给凭据值
sessionClientsHost.presences {sessionId} -> {presences: [{channel, label?}]}
  -- channel 是 tui/cli/web/acp/remote/desktop/unknown；label 是 hello.clientName，不是身份
listenAddresses.read() -> {unixPath?, pipeName?, websocketAddress?}
  -> error {code: invalid_path, message}
  -- 路径按 §1 是 UTF-8 字符串；非 UTF-8 的 unix socket 路径是 invalid_path
reservedMethods.check {namespace, declarations: [{name, mutating, notifications}]}
  -> {} | error {code: reserved, message}
  -- interruption/progress 只属于真实注册，不属于这个问题
collaborationDirectory.relations() -> {relations: [{memberSessionId, leaderSessionId, memberAgentId}]}
  -- 只读：Team 关系的写入仍由 Collaboration 的准入决定，数据面不提供第二个写入者
eventSubscriptions.sources() -> sources
eventSubscriptions.subscribe {sessionId, branchId?, types, match?, mode?} -> reply
eventSubscriptions.unsubscribe {sessionId, subscriptionId} -> reply
eventSubscriptions.list {sessionId} -> reply
  -> error {code, message}
  -- subscribe 将 EventHub A.3 的 target.sessionId/branchId 展开到请求顶层；sessionId 是值，不是授权
```

`eventSubscriptions` 的 `sources`、`list` 在 dispatcher 里是 sync，其余是 async。测试：`crates/dim-agent-capabilities/tests/legacy_port_faces.rs`、`crates/dim-app-server/tests/legacy_port_faces.rs` 和 EventHub 的 `fixture_flow.rs` 证明 typed binding 与 wire 返回同一结果。

`reservedMethods` 的 owner 是 App Server 的根值：它从 `process_value`（每个 Agent 一份）改为进程根上的一份，因为 shared 名字的 isolate 只覆盖 dispatcher key，而 `process_value` 的按 Agent 隔离覆盖的是类型 key；一份进程值对所有 Session 表等价，且只有一个 dispatcher。

### 4.10 宿主读取端口与 Provider 选择

这些读取操作持有 owner 的闭包，Rust 绑定直接调用类型化操作；dispatcher 只在跨语言边界解码请求、调用同一个操作并编码回复，不写第二份业务逻辑。Rust 调用不经过 JSON，也不为读取另起桥接任务。每次异步调用持有独立的子取消 token：取消或丢弃调用会取消该次 owner 读取，不取消调用方的父 token。跨语言回复使用 `cancellable_future`，放弃等待会丢弃同一个读取；owner 的生命周期仍约束其工作。

```text
executionSettingsHost.read({}) -> {sandbox, network}
  -> error {code: failed, message}
toolchainSourceHost.list({}) -> {toolchains: [{bin, root}]}          # sync
  -> error {code: failed, message}
promptSuggestionSettingsHost.read({}) -> {enabled, maxChars}
  -> error {code: failed, message}
providerService.selectableModels({}) -> {models: [ModelCapabilitySnapshot]}
providerService.selectableForModality({}) -> {models: [ModalityModelSnapshot]}
providerService.resolve({explicit?: ModelId, cwd?}) -> ModelSelection
  -> error {code: cancelled | owner_closed | invalid_request | model_unavailable | failed, message}
```

- 每个 operation 只收一个请求对象；没有输入的写 `{}`（`provider-management.md` §3.6 的形状），多一个参数或未知字段都是 `invalid_request`。
- 取消是协议动作：调用方丢弃等待，或被取消的绑定调用不再进入 owner；owner 自己的生命周期仍然约束它启动的 worker。
- 回复就是 owner 的类型：`ExecutionSettings`、`PromptSuggestionSettings`、`ModelCapabilitySnapshot`、`ModalityModelSnapshot`、`ModelSelection` 都是 serde camelCase 数据。`InstalledToolchain` 的 `PathBuf` 换成 UTF-8 字符串（`{bin, root}`）：某个 runtime 的目录不是 UTF-8 时只跳过那一项，其余照常给子进程用；`cwd` 的请求方向同理，`ModelSelectionRequest::at` 是 Rust 调用方换字段的唯一一处。
- 提供方仍是原来的部件（App Server 的 `ExecutionSettings`、`ToolchainSource`、`PromptSuggestionSettings`、`Provider`，以及 aimux 的 `ConfiguredModels`）：`provide` 一次给出类型键上的绑定和 `host_key(name)` 上的 face；没有适配层，也没有第二份业务逻辑。
- 测试：`host_port_faces.rs` 比对 Rust binding 与 wire 的结果、请求形状、错误与取消；`provider_flow.rs` 等既有流程验证真实 owner 的读取。

### Skills Session 列表

`skillsBranch` 的 `list({})` 与 Session 内部视图的边界由 [Skills owner](../architecture/rust-agent/plugins/Skills.md#data-face) 定义。Rust binding 与 wire 使用同一 owner；`SessionSkills` 保留为 Session 实例内的私有类型 key，不再有目录名字。

### MCP 插件内容

`mcpService` 的四个既有插件文档操作与取消、错误、私有资源边界由 [MCP owner](appserver/mcp-management.md#81-plugin-content-data-face) 定义。

### 4.11 `hooksService`

插件内容登记、撤回、列表和拒绝原因读取使用同一个 Hooks owner；原生与跨语言绑定保持其持久化、
启用及已接纳工作结算语义。操作与错误由 [Hooks A.2.1](plugins/Hooks.md#a21-plugin-content-data-face) 定义。

## 5. 验证

- 运行 `cargo run --locked -p xtask -- check`，按 §2.3 校验实际装配中的服务面。生成的 catalog 仅供本地查阅。
- 每个服务：现有 flow 测试 + 一个 loopback session 的跨边界用例；不新增 mock-only 测试。
- 数量：不改 `cargo test` 的总量要求；按受影响 crate 跑，最终 `cargo test --workspace --locked`。

## 6. 风险与开放问题

1. **`Faces` 的传播**：`Names`、`PartRegistry`、`SessionResolver` 都要拿到同一份原因表；独立 host / 测试用空表，检查只在装配和 leaf 加载处生效。

## 7. 交付顺序

[#1292](https://github.com/arcships/dim/issues/1292) 按 §3.2 的首批操作和各 owner 推进 §2.6 的迁移，包含 §3.1 的公共名称与私有装配边界。每批随代码更新对应 owner 的操作契约，并按 §5 验证。
