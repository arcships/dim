# 升级到 rutis 0.16.0

[English](../../design/rutis-0.16-upgrade.md)

状态：已由 [ADR 006](../adr/006-plugin-contract.md) 下[插件契约](plugin-contract.md#10-迁移)的 P1 阶段（[#379](https://github.com/arcships/dim/issues/379)）实现。
它取代 [rutis 0.15 升级设计](rutis-0.15-upgrade.md)；该设计采用的按作用域投递模型已被 rutis 0.15.1 撤回。

本次升级把 `rutis`、`rutis-loader`、`rutis-bridge` 以及随附的 Node 包从 0.12.1 升到 0.16.0。
Dim 尚未发布，所以本次升级不保留任何兼容路径：平总线让某个现有形态变得多余时，这个形态就在同一次改动中
去掉，TypeScript SDK、仓库内的插件及其测试一起改。其余的契约改动由插件契约的后续阶段完成。

## 1. 范围，以及为什么一步做完

| 项 | 现状 | 目标 |
| --- | --- | --- |
| `Cargo.toml` 工作区依赖 | `rutis`、`rutis-loader`（`node`）、`rutis-bridge` 0.12.1 | 0.16.0；`rutis-macros` 通过 `rutis` 引入 |
| `crates/dim-app-server` 构建依赖 | `rutis-bridge` 0.12.1，带 `exports` | 0.16.0，带 `exports` |
| `runtime/package.json` | `@arcships/rutis-runtime`、`@arcships/rutis` 0.12.1 | 0.16.0 |
| `runtime/plugins/{web,workflow,session-title}/package.json` | `@arcships/rutis` 0.12.1 | 0.16.0 |
| 作者指南的固定版本 | [作者指南](../../typescript-plugin-authoring.md)：“main uses 0.12.1” | 0.16.0 |

从 0.13 起，每一端都在握手中声明 `events-v2`，并在加载任何内容之前拒绝未声明的对端。因此 Rust 宿主、
`@arcships/rutis-runtime`、每个内置 SDK 的插件包及其锁文件必须一起升级。只升级 Rust 侧会让每个
TypeScript 行都无法连接。

不在范围内：`rutis-host`、Go/Swift/Kotlin 运行时、dylib 插件、节点互联与 Cordis 挂载。Dim 不使用它们。

## 2. 0.12.1 到 0.16.0 之间的变化

| 版本 | 与 Dim 相关的内容 |
| --- | --- |
| 0.13 | 统一事件：监听器按名字查找，载荷走 serde，一张监听器表，五种模式；按实例键的事件 API 被移除；`Process::emit`、`.emits` 与 `Mount.emits` 被移除；`events-v2` 握手。插件 API 3；API 2 插件仍可加载 |
| 0.15（没有 0.14） | `#[rutis::service]`、`rutis-macros`、`Loader::waiting_for`；本地运行时问候超时为 30 秒；单字段错误的 `code` 是其类型名 |
| 0.15.1 | **一条平总线：** 某个名字的每个监听器都会收到该事件，无论它在哪里；scope 只用于在过滤器中描述监听器 |
| 0.16 | 移除 0.15.1 的 scope 兼容层；`Ctx::is_shutting_down`；修复 exports 中 `#[cfg]` 方法与别名泛型的问题 |

插件 trait（`Plugin`、`TypedPlugin`、`Deps`）、效果、带实例的 `TypeKey`、按实例的 loader 组、`Build`、
`ServiceCatalog` 与 `Exports` 保持 0.12.1 时的契约。

### 2.1 Dim 调用的 Rust 内核 API

| 0.12.1 | 0.16.0 |
| --- | --- |
| `Listener::call(&self, ctx, event)` | `call(&self, ctx, event, call: Call<'_, E::Value>)`；监听器拿到的是注册它的 ctx |
| `PatternListener::call(&self, ctx, key, event)` | 末尾多一个参数 `Call<'_, E::Value>` |
| `SyncEvent` 标记 | 移除；`on_sync` 接受任意 `Event` |
| `on_waterfall`、`on_waterfall_sync`；`Next`、`SyncNext` | `on`、`on_sync`；监听器自己调用 `call.next()` |
| `waterfall(..) -> Result<V>`，终态 `-> Result<V>` | 两者都是 `-> Result<Option<V>>` |
| `serial(ctx, key, &E)` | `serial(ctx, key, Arc<E>)` |
| `on_pattern(ctx, pattern, ..)` | `on_pattern(ctx, &pattern, ..)` |
| `EventKey::of().instance(id)` 以及所有 `*_instance`/`*_keyed` 方法 | 移除。隔离靠名字（§4） |
| `EventKey::name() -> Option<&str>` | `-> &str` |
| `DispatchMode` | `rutis::Mode` |
| `EventBacklog.key`、`DispatchAttempt.key` | `name` |
| `CordisError::ReentrantEvent` | 移除（真正的等待环报 `SyncWaitCycle`） |
| `parallel` 失败 | 总是 `CordisError::ListenersFailed { name, errors }`，即使只有一个失败 |

`TypeKey::instance`、`with_instance` 与 `keyed_dynamic` 不变；只有事件键失去实例身份。

### 2.2 Dim 调用的 bridge 与 loader API

| 0.12.1 | 0.16.0 |
| --- | --- |
| `LocalRuntime::emits([..])`、`RuntimePlugin::emits` | 移除：运行时监听器自行注册到宿主总线上 |
| `Process::emit(name, args)`（会等待全部监听器） | 移除：在 `ctx.events()` 上用 `EventKey::<rutis_bridge::events::Args>::dynamic(name)` 分发；等待形式是 `parallel` |
| `Mount.emits`、`Mount.events` | 移除。`Mount` 新增 `ctx` 与 `runtime`。Dim 从不自行构造 `Mount` |
| `Process::load_row(key, entry, config, isolate, inject)` | 末尾多一个参数 `scope: &[rutis::Scope]`：根部为 `&[]`，实例内为 `&ctx.scope()`。它只描述监听器 |

### 2.3 影响 Dim 正确性的行为变化

- **平总线**是 Dim 必须围绕它设计的那一项变化（§4）。
- `emit` 仍然立即返回。监听器的失败进入错误汇，永远不回到发送方。
- **同一名称的事件不再互相等待监听器。** 0.12.1 对同一名称的 `emit` 串行派发：上一个事件的每个监听器
  都执行完，下一个事件的监听器才运行。0.16 上只需那些监听器已经启动，因此正在 await 的监听器不会拖住
  任何事件。Dim 的监听器都在第一次 await 之前做完有序的部分，投递顺序与 Rpc 的投递栅栏不变；barrier
  测试改为在监听器的同步部分阻塞，以模拟慢总线（[Dim 如何使用 rutis §5](rutis-usage.md#5-事件总线)）。
- 卸载时把行的监听器从总线上移除，通过 `ctx.cancelled()` 取消正在运行的调用，并最多等待它们
  `RUTIS_SHUTDOWN_TIMEOUT`（10 秒）。
- 从 Rust 发出的事件无需声明即可到达 TypeScript、Python 与 Go 监听器，因此公开事件的 serde 形式
  对每个运行时都可见。

## 3. Dim 的当前用法

在 dd87f6af 上对 `crates`、`tests` 与 `xtask` 的统计：10 个 `Event` 实现、23 个监听器实现、
46 处 `EventKey`、16 处 `emit` 与 2 处 `parallel`、2 处 `waterfall_sync`、2 处 `on_pattern`、
2 处 `Process::emit`，以及 1 处 `.emits(..)` 调用。

| 事件 | 名字 | 载荷 | 当前 serde |
| --- | --- | --- | --- |
| `SessionBroadcastEvent`（`dim-agent-capabilities/src/session_broadcast.rs`） | `session.broadcast`，在 `session/{id}` 下分发 | `Arc<AgentEvent>`、`Arc<SessionNotice>`、`Fence`、`Closed` | 无 derive；`SessionNotice` 只有 `Serialize` |
| `Committed`（`dim-agent-runtime/src/core/dispatcher.rs:15`） | `timeline.committed` | `Arc<AgentEvent>` | 无 derive |
| `BeforeCommit`（`dim-agent-runtime/src/core/timeline.rs:27`） | `timeline.before` | `EventDraft` 与本地 trace 步骤 | 否；步骤不是 serde |
| `SettingsChanged`（`dim-agent-capabilities/src/settings.rs:24`） | 在 `settings/{source}` 下分发 | `source`、scope、revision | 是 |
| `CatalogChanged`（`dim-agent-capabilities/src/skills/catalog.rs:32`） | `skills.catalog_changed` | `u64` 版本号 | 无 derive |
| `ToolsChanged`、`ToolProviderChanged`（`tool_registry.rs:29`、`:36`） | `tools.changed`、`tools.provider_changed` | 单元 | 无 derive |
| `ProjectAuthorized`（`dim-app-server/src/settings.rs:78`） | `settings.project_authorized` | `root`、`cwd` | 是 |
| `SessionRestored`（`dim-app-server/src/agents.rs:84`） | `agent/session-restored` | `agent_id` 与一个 **`Session` 句柄** | 否；句柄不是 serde |
| `UnloadedSessionDeleted`（`agents.rs:96`） | `agent/unloaded-session-deleted` | 两个 id | 无 derive |

## 4. 语义变化：一条平总线

0.12.1 把按实例键的事件隔离在其实例的子树内。在 0.16 上，某个名字的每个监听器都会收到该名字的
每个事件。依赖实例键的流向现在会跨分支或跨 Session 投递。

| 流向 | 发送方 | 监听器 | 0.12.1 上的隔离 | 在平总线上 | 处理 |
| --- | --- | --- | --- | --- | --- |
| `timeline.committed` | Dispatcher（分支） | 该分支的订阅者 | 分支实例键（`dispatcher.rs:113`、`:216`） | **每个分支的订阅者都会收到每个分支的事实** | U2 |
| `timeline.before` | Timeline（分支） | 该分支的变形 | 分支实例键（`timeline.rs:870`） | **每个分支的变形都会在每次提交上运行** | U2 |
| `tools.changed` | ToolRegistry（Session） | ToolBinding（分支） | Session 实例键（`tool_registry.rs:351`、`tool_binding.rs:375`） | 任一 Session 的变化都会唤醒每个 Session 的绑定 | U3 |
| `tools.provider_changed` | McpSession（Session） | ToolRegistry（Session） | Session 实例键（`mcp/session.rs:156`、`tool_registry.rs:895`） | 任一 Session 的变化都会让每个 Session 的注册表刷新 | U3 |
| `agent/session-restored`、`agent/unloaded-session-deleted` | Agents（进程） | Collaboration（进程） | 根实例键（`agents.rs:1528-1529`） | 不变：只有一个根 | U4 |
| `session/{id}` | SessionBroadcast（分支） | 连接、TypeScript 中继、测试 | 动态名字 | 不变 | — |
| `settings/{source}`、`settings.project_authorized` | Settings（进程） | SkillsCatalog、MCP、中继、测试 | 进程范围 | 不变 | — |
| `skills.catalog_changed` | SkillsCatalog（进程） | Skills（Session 行） | 进程范围 | 不变：它本就面向每个 Session | — |
| `rutis::FiberStatusChanged`、`rutis::ServiceChanged` | rutis | ToolRegistry、ProjectedServices、MCP 可用性 | 进程范围 | 不变：监听器已按键过滤或重新读取 | — |
| 发往 TypeScript 行的 `session.event` 与 Settings | App Server 中继，`Process::emit` | 各行，各自按绑定的 Session id 过滤 | 运行时范围 | `Process::emit` 已不存在 | U5 |

前两行是正确性需求。两条工具行仍然正确，因为每个监听器都会重新读取自己的注册表，但一个 Session 的
变化会唤醒每个 Session。

## 5. 决策

### U1. 宿主、运行时与插件作为一次升级单元

三个 crate、`runtime/package.json`、三个插件包及其锁文件一起升级。`runtime/plugin-sdk.test.mjs`
会打包 `npm/`、复制 `runtime/node_modules/@arcships/rutis` 并对作者指南做类型检查，因此作者类型与
SDK 版本同步变化。

### U2. 分支机制通道使用私有名字

Dispatcher 与 Timeline 用各自的分支实例构造总线名字：`dim/timeline/{instance}/committed` 与
`dim/timeline/{instance}/before`。构造函数是 `dim-agent-runtime` 私有的。部件继续使用
`Dispatcher::subscribe`/`listen` 与 `Timeline::before`，从不直接使用这些名字
（[ADR 006 D5](../adr/006-plugin-contract.md#d5-timeline-事实与总线事件是两回事)）。`Dispatcher` 与
`Timeline` 的 `instance` 字段保留，现在用于构造名字。`InstanceId` 只用于运行时路由；它从不被持久化，
也从不展示。

### U3. 按 Session 的工具事件

- 移除 `tools.changed`。ToolBinding 在它已声明的句柄上读取 ToolRegistry 现有的来源状态 watch
  （`sources_state()`，`tool_registry.rs:236`），沿用
  [0.15 设计 D3.1](rutis-0.15-upgrade.md#d31-toolregistry-来源状态已存在) 的合同：打开时首次读取，
  每次替换刷新一次，拒绝可见，无变更不唤醒，关闭时任务结束。
- `tools.provider_changed` 改用按 Session 的私有名字 `dim/tools/{instance}/provider-changed`。
  `tool_registry` 导出它的构造函数，McpSession 使用它。不采用在每个提供方上的 watch，因为
  ToolRegistry 不依赖可选提供方（[ADR 002 D3](../adr/002-plugin-scope.md#d3-依赖跟随作用域并从扩展指向核心)）。

### U4. 根事件去掉实例

`agent/session-restored` 与 `agent/unloaded-session-deleted` 使用 `EventKey::of()`；一个进程只有一个根。

### U5. 移除中继

在平总线上，行可以直接收到公开事件，所以两条中继都去掉：Session 中继及其按 Session 的队列
（`session_typescript.rs:70-197`）、Settings 中继（`:212-263`）、`session.event` 这个名字和 `.emits(..)`
（`:515`）。不用任何东西替代它们。

- Session 行用 `ctx.on` 监听 `session/{sessionId}`。在 R2 提供 `sessionIdentity` 之前，它仍从现有的
  `sessionEvents:<part>` 绑定得知自己的 Session id。
- 使用某个 Settings 来源的行监听 `settings/{source}`；需要时也监听 `settings/projects`。
- 同一进程发出的同名事件按发出顺序开始处理。需要严格顺序的行改经 `dispatcher` 消费事实；SessionTitle
  不需要，因为它每收到一条事件都会重读自己的来源。

不需要 0.15 设计的 agent 上下文分发（D4）。

### U6. 每个载荷都是 serde，本地状态不放进去

- 为 `SessionBroadcastEvent`、`Committed`、`CatalogChanged`、`ToolProviderChanged` 与
  `UnloadedSessionDeleted` 派生 `Serialize`/`Deserialize`，并给 `SessionNotice` 补上 `Deserialize`
  （`Arc` 需要 serde 的 `rc`，已启用）。
- `BeforeCommit` 用 `#[serde(skip)]` 让 trace 步骤保持本地。该事件只由它自己分支的 `waterfall_sync`
  在进程内分发。
- `SessionRestored` 携带 `agent_id` 与 `session_id`。Collaboration 通过 `AgentIndex` 解析 `Session`，
  句柄本来就在那里。
- `SessionBroadcastEvent` 是 `session/{sessionId}` 的公开载荷，所以它的 serde 形式现在就设计好，而不是用
  默认派生：`#[serde(tag = "kind", rename_all = "camelCase")]`，得到 `{kind: "event", branchId, event}`、
  `{kind: "notice", ..}`、`{kind: "fence", recipient, token}` 和 `{kind: "closed"}`。SDK 声明这个形式。

### U7. 监听器迁移是机械的，只有一处取消语义变化

补上 `Call`/`SyncCall` 参数；把 `on_waterfall_sync` 换成 `on_sync`、`SyncNext` 换成 `SyncCall`；
让变形链及其终态返回 `Option`；给 `serial` 传 `Arc<E>`，给 `on_pattern` 传 `&EventPattern`；
适配 `EventKey::name()`；让 `AgentError::aggregate` 展平 `ListenersFailed`。Dispatcher 的
`SubscriptionListener` 让处理器与注册 ctx 的取消竞争，这样在卸载会等待运行中的监听器之后，分支关闭的
延迟保持不变：

```rust
tokio::select! {
    _ = ctx.cancelled() => Ok(None),
    result = handler(event) => { /* as today */ }
}
```

### U8. Exports 与服务目录保持原样

`build.rs` 的 `Exports` 列表在 0.16 下照常工作。`#[rutis::service]` 与 `share_registered_only` 属于
插件契约中与服务目录有关的阶段（P2、P4），不属于本次升级。

### U9. 不改公开名字

公开名字保持原有拼写。Skills 目录事件 `skills/catalog` 保留，因为平总线会把它投递给每个 Session 行。0.15 设计
所要求的 `SkillsCatalog::changes()` watch（[#362](https://github.com/arcships/dim/issues/362)）不再需要。

### U10. TUI leaf 链接随二进制一起升级

交互式 `dimcode` 进程是一个由 daemon 拨入的 rutis 节点（[Tui §3.6](../../design/plugins/Tui.md#36-ui-registry-and-ui-leaves)）。
升级不改变这条链接：

- `Accept`（`dim-cli/src/leaf.rs`）与 `LeafDialer`（`dim-app-server/src/ui_leaf.rs`）在
  `transport::local::framed` 之上实现 `rutis_bridge::Transport`；两者在 0.16 上无需修改即可构建。
- 两端是同一个二进制，握手以及按 `LinkConfig.peer` 对端点标识的检查一起变化。
- `TuiRow` 以 `[key, "Tui", config, [], []]` 调用 `plugins.load`、以 `[key]` 调用 `plugins.unload`，
  与 0.16 自己的 peer 行使用的参数相同。
- `Features` 的事件字段自 0.13 起名为 `export` 与 `import`。leaf 链接只设置 `host`，因此没有事件经过它们。

发往 agent 的 UI 事件（leaf 的链接导出 `ui/*`、daemon 的链接导入它，见 Tui §3.6 “Later”）不属于这次升级；
它们一直在等平总线，可以在升级之后进行。Tui §10 中接受的限制（私有的 `peer:` 行、不能监听的本地传输）
仍然接受：rutis 是外部依赖，Dim 不向它提需求（[设计规则 §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 第 8 条）。

## 6. 实施计划

0.13 没有兼容路径，因此第 1–4 步在一个 pull request 中落地。每一步各自一个提交。

| 步骤 | 工作 | 门槛 |
| --- | --- | --- |
| 1 | 固定版本与锁文件（U1）；作者指南的固定版本 | `cargo metadata` 可解析；锁文件显示 0.16.0 |
| 2 | 内核事件：U2、U3、U4、U6、U7 | §7 中针对 Timeline、工具、Skills、Settings 与广播的流程检查 |
| 3 | bridge 与运行时：U5（移除中继与 `.emits`）、`load_row` 的 scope 参数 | Node 驱动的流程测试：真实的行收到 `session/{sessionId}` 与它的 Settings 事件 |
| 4 | TypeScript 包：SessionTitle 与各 Settings 消费方改为监听公开事件；测试中去掉 `emits: [..]`，用 `parallel` 代替 `emit` 来等待，从 `reported` 读取失败；SDK 声明 `session/{sessionId}` 的载荷 | `runtime/plugins/*` 中的插件测试；`runtime/plugin-sdk.test.mjs` |
| 5 | 工具链与文档（§8） | `xtask check`；`node --test scripts/*.test.mjs` |

## 7. 验证

在工作区根目录按 CI 顺序运行完整的 CI 序列：

1. `./dev check`
2. `cargo clippy --workspace --all-targets --locked -- -D warnings`
3. `cargo test --workspace --locked`
4. `cargo run --locked -p xtask -- check` 与 `graph`。装配与 crate 依赖不变，因此快照不应变动；
   出现 diff 说明这次升级改到了不该改的地方
5. `node --test scripts/*.test.mjs`
6. 按 CI 的方式运行 Node 绑定，包括 `runtime/plugin-sdk.test.mjs`、插件测试、`tsc` 检查与
   `git diff --exit-code -- npm/branch-services.d.ts`
7. Node 驱动的 Rust 流程（`DIM_TEST_NODE`、`DIM_TEST_JS_RUNTIME`、`DIM_TEST_RUTIS_SDK`）与
   `cargo test -p dimcode --test cli_flow -- --ignored`

测试套件必须覆盖的流程检查；只在契约变化处扩展现有测试：

- **分支隔离（新增，U2）：** 同一 Session 的两个分支，以及两个 Session 的分支。订阅者只收到自己分支的
  事实，`before` 变形只在自己分支的提交上运行。
- **工具来源（U3）：** 来源变化让 ToolBinding 从 `sources_state()` 刷新，并满足 D3.1 的检查；
  Session A 中的 MCP 提供方变化刷新 A 的注册表，而不刷新 B 的。
- **发往行的事件（U5）：** Settings 变更与项目授权到达监听它们的 TypeScript 行；`session/{sessionId}` 事件
  到达该 Session 中监听它的行，不到达其他 Session 的行。
- **Skills：** 目录推进在发布后唤醒 Session 行（现有测试）。
- **恢复（U6）：** Collaboration 通过 id 观察到被恢复的 Session。
- **关闭（U7）：** 监听器运行中关闭分支，延迟有界，且不遗留任务。
- **广播：** 连接通知不变。
- **TUI leaf（U10）：** `cargo test -p dimcode --test ui_leaf` 与 `transport_flow ui_host::`。

## 8. 随升级一并更新的文档

- [作者指南](../../typescript-plugin-authoring.md)：固定版本，以及 leaf 事件语义（`emit` 不等待；
  `parallel` 等待；没有 `emits` 声明）。
- [单树装配](../../architecture/rust-agent/single-tree.md) 的 §事件 与 §rutis，以及
  [生命周期](../../architecture/rust-agent/lifecycle.md)：按实例键的事件改为名字；版本基线。
- [Session](../../design/session-typescript.md)、[分支](../../design/branch-typescript.md)与
  [进程](../../design/process-typescript.md)的 TypeScript 设计；[管理方法](../../design/appserver/management-methods.md)、
  [sessions 服务](appserver/sessions-service.md)、[P6/P7 绑定](../../design/p6-p7-bindings.md)、
  [ExternalAgent](plugins/ExternalAgent.md)、[路线图](../../design/roadmap.md)与
  [发布说明](../../release/README.md)：0.12.1 引用。
- 存在对应译文的 `docs/zh/` 文件。

## 9. 风险

| 风险 | 应对 |
| --- | --- |
| 依赖实例键的流向现在跨实例投递，且没有错误 | §4 列出了每个带 `.instance(..)` 的事件键；分支隔离检查是合并门槛；合并前 grep 带实例的 `EventKey` |
| 宿主与运行时版本漂移（握手拒绝） | 第 1 步在一个提交内升级所有固定版本；暂存的发布归档从 `runtime/package.json` 记录 `toolchain.rutis` |
| 卸载等待监听器调用 | U7 在 Dispatcher 监听器中处理取消；关闭检查 |
| 私有的受管提供方插件（rutis 0.10，API 2）在 0.16 运行时下行为不同 | 在同一次发布中用 0.16 SDK 重建它，并在 0.16 运行时下验证 |
| `ListenersFailed` 不再被展平进 `AgentError::Multiple` | U7 扩展 `AgentError::aggregate` |

回滚：回退升级改动与固定版本。不移动任何持久化格式；Timeline、存储与 wire fixture 都不受影响。

## 10. 向机制所有者提出的需求

无。每个决策都按原样使用 rutis 0.16 与 Dim 的现有服务。0.15 设计的 R1（#362）撤回（U9）。

## 11. 规则检查

本次升级不新增部件，也不新增事件。

| 规则 | 回答 |
| --- | --- |
| 原则 §4 第 1–3、5–7、9 条 | 不变：指令、交接、`before` 契约、批次、上下文路径与事实都没有变化 |
| §4 第 4 条（核心机制不带业务含义） | U2 与 U3 是通用的机制名字；没有针对特定插件的处理 |
| §4 第 8 条（基础设施默认不变） | 改动来自依赖升级，并在运行时、capabilities crate 与 App Server 的中继内部闭环；不对 rutis 提需求 |
| AGENTS 第 6 条（冻结 fixture） | 不动 `crates/dim-agent-types/tests/fixtures/` |
| ADR 002 D1–D5 | 不改变放置、接线、依赖或配置；在契约阶段之前 D5 不变 |
| ADR 006 D5 | U2 与 U3 遵循它：按实例的流向是由其所有者构造的私有名字 |

## 12. 消融检查

- 去掉一个事件（`tools.changed`）；不新增事件。U2 与 U3 用名字取代实例键，不新增状态。
- U5 删除两条中继、它们的队列和 `.emits`，不用任何东西替代。不引入“按名字发送”的辅助函数。
- 不做兼容层：0.13 没有旧路径，兼容层只会掩盖已经断裂的流向。
