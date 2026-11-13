# 服务面：跨语言形状的策略、盘点与第一批改造

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
- Secret values stay with the owner. Public methods expose operations that use those values.

### 2.3 Checks

`xtask graph` and `check` classify the current catalog:

- shared + dispatcher → `data`;
- shared without dispatcher → failure, with the two upstream entries in §3.1 currently reported as
  `shared-no-dispatcher`;
- dispatcher without shared → failure;
- not shared + known migration reason → `internal(reason)`;
- not shared without a reason → failure;
- shared and internal at once → failure.

The target check rejects every shared name without a dispatcher once the upstream migration in §3.1
is complete. The target catalog has only `data` rows.

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

Rust 产品装配的现状以 [生成的 catalog](../architecture/rust-agent/generated/catalog.md) 为准。以下按迁移工作分组，每个名字只在盘点表中列一处。批次进度见 [#1292](https://github.com/arcships/dim/issues/1292)。

### 3.1 基础数据面与上游登记

| 名字 | 处理 |
| --- | --- |
| `managementMethods` | 数据面，shared 名字与 dispatcher 一同注册。 |
| `localId` | 数据面，由 root 提供 `get`。 |
| `loaderService` | rutis-loader 的装配键，当前为 `shared-no-dispatcher`。插件管理已有 `rutis/loader` 数据面。 |
| `pluginMounter` | rutis-loader 提供的装配键，当前为 `shared-no-dispatcher`。`rutis-bridge` 的 `PluginMounter::mount` 接收 `&Ctx` 并返回 `Box<dyn DynPlugin>`。 |

[#1292](https://github.com/arcships/dim/issues/1292) 跟踪两个装配键的迁移。由 rutis 上游确认各自的本地消费者和装配方式。插件管理复用 `rutis/loader`，其余确有独立公共需求的操作由上游先定义数据契约。两项登记按 §2.6 处理完后，移除临时检查放行。

### 3.2 第一批数据面（#1292）

| 名字 | 目标 | 备注 |
| --- | --- | --- |
| `scheduleJobs` | `scheduleJobs.call(JobRequest)` | 按 [Scheduler](plugins/Scheduler.md#a2-tool-schedule) 的 flat `JobRequest`/`JobReply`，与 `wakeups` 同一批次（[#1396](https://github.com/arcships/dim/pull/1396)）。 |
| `wakeups` | `wakeups.create/list/cancel` | 形状按 [Scheduler](plugins/Scheduler.md#102-methods-and-tool) §10.2。 |
| `sessionFiles` | `sessionFiles.put/register/owns/owned_paths` | 形状按 [Session files](appserver/session-files.md) §9.1，`SessionFilesHost` 是进程内绑定。 |
| `managedSearch` | `managedSearch.search` | 凭据由实现持有，见 §4.6。 |
| `agentConfig` | `agentConfig.get()` | 形状见 §4.8。 |
| `sessionLanguage` | `sessionLanguage.get()` | 形状见 §4.8。 |
| `skillsConfig` | `skillsConfig.get()` | 形状见 §4.8。 |
| `collaborationConfig` | `collaborationConfig.get()` | 形状见 §4.8。 |
| `worktreeAssociations` | `worktreeAssociations.associate` | 形状见 §4.8。 |

### 3.3 所有者 API，已有数据兄弟

本组统一迁移标签 `owner-api`。迁移批次先核对数据面能否覆盖原调用，再切换到同一组操作的绑定。

| 名字 | 数据兄弟 | 说明 |
| --- | --- | --- |
| `agentDirectory`、`agentIndex` | `sessions` | 目录查询走 `sessions` |
| `settingsService` | `settings` | GoalBranch 的取消契约见 §4.1。 |
| `modelHost` | `auxiliaryModel` | PromptSuggestion 的请求与流式语义见 §4.2。 |
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
| `promptSourcesBranch` | 分支 prompt 装配 | 按 #1292 确认公共操作与装配内部职责。 |

这些行按当前对象形状分组。前四行的数据契约由 [resource and store data faces](resource-faces.md) 定义（#1295/#1297/#1298）。其余行按 #1292 逐批确定操作面。

`hostPlatform`、`fileStorage`、`sqliteStorage`、`timelineStore` 不按对象分类保留：数据面就是公开契约，`execHost`、`fsHost`、`store` 的操作就是它们的接口，旧名字是提供方和它的 Rust binding，调用方在进程内直接调这组操作，不再有第二套公开 API。凭据是操作内部用的能力，不是保留 `internal` 的理由。`hostPlatform`、`fileStorage` 的提供方在持有资源的那个 daemon（控制节点自己的插件，或 #1296 挂载的资源 leaf）；`store` 在持有 Session 状态的那个 daemon，SQLite 还是 PostgreSQL 只是实现（#1298，[resource and store data faces](resource-faces.md) §6）。

### 3.5 热路径与顺序：待操作化
原因 `hot-path`：每回合或每条输入上的状态机、本地顺序或同步回调。操作面的粒度须保留原有顺序和失败语义。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `agentLoop` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `loopState` | branch | Loop | hot-path | 待迁移；按 #1292 定义操作面 |
| `modelProvider` | branch | ModelProvider | hot-path | 待迁移；按 #1292 定义操作面 |
| `prompt` | branch | Prompt | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextPreparation` | branch | ContextPreparation | hot-path | 待迁移；按 #1292 定义操作面 |
| `contextUsage` | branch | ContextUsage | hot-path | 本地请求计数器、请求引用与按序统计；`session.context.usage` 是 App Server RPC。按 #1292 确认是否需要独立插件操作。 |
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

### 3.6 入口、凭据与宿主资源

按 #1292 逐批确定各项的公共操作，并按 §2.6 迁移登记。状态、资源和凭据留在各自的 owner。`managedProviders` 的搜索操作见 §4.6。

| 名字 | scope | provider | reason |
| --- | --- | --- | --- |
| `serverControl` | process | ServerControl | control |
| `connectionHub` | process | Connection | control |
| `pluginFileTransactions` | process | PluginFiles | host-resource |
| `publication` | process | McpPublication | host-resource |
| `mcpAuth` | process | Mcp | secret |
| `grokOAuthService` | process | GrokOAuth | secret |
| `openAICodexOAuthService` | process | OpenAICodexOAuth | secret |
| `uiHost` | process | UiHost | control |
| `uiExtensions` | process | UiExtensions | control |
| `mentionIndex` | process | WorkspaceToolsOwner | host-resource |
| `skillsCatalog` | process | SkillsCatalog | host-resource |
| `mcpService` | process | Mcp | host-resource |
| `hooksService` | process | HooksSettings | host-resource |
| `providerService` | process | Provider | secret |
| `providerConnectionResolver` | process | Provider | host-resource |
| `collaboration` | process | Collaboration | host-resource |
| `sessionLoader` | session | SessionScope | control |
| `timelineFactory` | session | SessionScope | control |
| `sessionSkills` | session | Skills | host-resource |
| `skillsBranch` | branch | SkillsBranch | host-resource |
| `managedProviders` | process | ManagedProviders | secret |

### 3.7 待迁移的数据内容
原因 `legacy-no-consumer`：内容是数据，但还没有转成 data face。没有 leaf 消费者不是保留 internal 的理由；这些名字按批次迁移，`legacy-no-consumer` 只是迁移状态，不是永久分类。

| 名字 | scope | provider | reason | 动作 |
| --- | --- | --- | --- | --- |
| `eventSubscriptions` | process | eventhub | legacy-no-consumer | 按 #1292 定义操作面 |
| `executionSettingsHost` | process | ExecutionSettings | legacy-no-consumer | 按 #1292 定义操作面 |
| `toolchainSourceHost` | process | ToolchainSource | legacy-no-consumer | 按 #1292 定义操作面 |
| `promptSuggestionSettingsHost` | process | PromptSuggestionSettings | legacy-no-consumer | 按 #1292 定义操作面 |
| `selectableModelsHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `modelSelectionHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `modalityConnectionHost` | process | Provider | legacy-no-consumer | 按 #1292 定义操作面 |
| `listenAddresses` | process | Transport | legacy-no-consumer | 按 #1292 定义操作面 |
| `credentialVariablesHost` | process | root | legacy-no-consumer | 按 #1292 定义操作面 |
| `reservedMethods` | process | — | legacy-no-consumer | 按 #1292 定义操作面 |
| `collaborationDirectory` | process | CollaborationDirectory | legacy-no-consumer | 按 #1292 定义操作面 |
| `sessionClientsHost` | process | Connection | legacy-no-consumer | 按 #1292 定义操作面 |
| `collaborationSession` | session | CollaborationSession | legacy-no-consumer | 按 #1292 定义操作面 |

## 4. 批次契约（#1292）

每批处理同一个 owner 的相关操作。

### 4.1 `settingsService`：保留调用取消

迁移前 `GoalBranch` 继续用类型化调用，读取随 branch stop token 取消。设置 owner 在 #1292 的批次内确定如何将调用方取消传递给读取操作，并验证 `settings.read` 的 `SettingsResult<T>` 信封和现有错误语义。Goal 切到同一数据契约的 Rust 绑定时，覆盖分支停止中断设置读取的流程。

### 4.2 `modelHost`：保留完整模型请求

迁移前 `PromptSuggestion` 继续用类型化调用。模型 owner 在 #1292 的批次内定义能承载完整 `ModelRequest` 的公开操作，保留当轮 provider、model、system、tools、config、cached prefix、流式回复与取消。现有 `auxiliaryModel.complete` 用于精简请求。PromptSuggestion 切到同一数据契约的 Rust 绑定时，覆盖 tools、config 和缓存前缀。

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

按 owner 契约 [Session files](appserver/session-files.md) §9.1 暴露 `sessionFiles`。`SessionFilesHost` 作为这组操作的进程内绑定。

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

`managedSearch` 只公开搜索操作：

```text
managedSearch.search {query, count}
  -> {results: [{title, url, snippet, publishedAt?}]}
  -> error {code: unavailable | search_failed, message}
```

- 取消是协议动作（调用方丢弃调用或发 bridge cancel），不是请求字段。
- `ManagedProviders` 在 owner 内处理登录、凭据和租约。搜索调用迁到 `managedSearch` 后，批次按 §2.6 收敛旧登记（[#1398](https://github.com/arcships/dim/pull/1398)）。
- Web 的搜索改调 `managedSearch`；Web 的 settings 仍走 `settings`。

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

### 4.8 旧主机端口：`agentConfig` 等五个

这五个端口的内容是配置或一次写入：所有者把操作绑定到端口类型上，`#[rutis::service]` 生成 dispatcher，名字 shared；Rust 调用方直接调同一个方法，不另写适配层。`collaborationSession` 不在这五个操作面的范围内，按 §2.6 处理其公共操作或私有实现职责。

```text
agentConfig.get() -> {name, systemPrompt, model: {providerId, modelId}}
  -- Agent 的身份与默认模型；SessionScope 提供
sessionLanguage.get() -> {language?}
  -- 只读：更新还要写 Session 记录并让 Prompt 采用来源，仍走 session.language.update
skillsConfig.get() -> {instructions?}
  -- Skills 的 factory 文本；null 表示内置
collaborationConfig.get() -> {leaderDescription?, memberDescription?}
  -- Collaboration 的 factory 文本；null 表示内置
worktreeAssociations.associate {sessionId, repository, root, subtree} -> {}
  -> error {code: failed, message}
  -- 路径按 §1 是 UTF-8 字符串；一次写入仍由 SessionStore 的原子事务保证
```

- `agentConfig` 在 runtime 的 catalog 注册 shared instance；`SessionScope` 已经提供值，`get` 返回同一个快照。
- `skillsConfig`、`collaborationConfig` 的注册从 typed 改成 shared instance；值仍由 `install_with_config` 的 `session_value` 提供。
- `sessionLanguage` 只把读做成面；值由 PromptSources 的 Session part 提供，branch 通过 `PromptSourcesBranch::language()` 读同一个 cell。写入不暴露，避免绕过 Session 记录和 Prompt 的采用。
- `worktreeAssociations` 去掉 `AssociateWorktree` trait：端口持有所有者的写闭包，Rust 调用方和 wire 调用同一个 `associate`。SessionStore 绑定 `associate_worktree`；字符串转回 `PathBuf`，绝对性和子树包含仍由所有者校验。
- 测试：`crates/dim-agent-capabilities/tests/legacy_session_faces.rs` 与 `crates/dim-app-server/tests/legacy_session_faces.rs` 比较 typed binding 与 wire 的同一操作。

## 5. 验证

- `xtask check`：按 §2.3 验证当前 catalog，随 §3.1 的上游迁移收紧检查。
- `xtask graph`：catalog 的 `face` 列与真实装配一致；`catalog.md` 的 diff 作为评审材料。
- 每个服务：现有 flow 测试 + 一个 loopback session 的跨边界用例；不新增 mock-only 测试。
- 数量：不改 `cargo test` 的总量要求；按受影响 crate 跑，最终 `cargo test --workspace --locked`。
- 改装配后跑 `cargo run --locked -p xtask -- check` 与 `graph`。

## 6. 风险与开放问题

1. **Channel 单条消息上限**：`sessionFiles.put` 的 16 MiB 可能超过限制。先测；超了就分块，不降低上限。
2. **`Faces` 的传播**：`Names`、`PartRegistry`、`SessionResolver` 都要拿到同一份原因表；独立 host / 测试用空表，检查只在装配和 leaf 加载处生效。

## 7. 交付顺序

[#1292](https://github.com/arcships/dim/issues/1292) 按 §3.2 的首批操作和各 owner 推进 §2.6 的迁移，包含 §3.1 的上游登记。各实现批次随代码更新生成的 catalog 及自己负责的操作契约。
