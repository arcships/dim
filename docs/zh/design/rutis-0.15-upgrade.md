# 升级到 rutis 0.15.0

[English](../../design/rutis-0.15-upgrade.md).

**已被取代**：由 [ADR 006](../adr/006-plugin-contract.md) 下的 [rutis 0.16 升级设计](rutis-0.16-upgrade.md)取代。
rutis 0.15.1 撤回了按作用域投递，所以 §4 与 D2–D4 不得实施。本文保留决策的历史，以及 0.16 设计沿用的
D3.1 `ToolRegistry` 来源状态合同。

状态：设计，尚未实现。本文是 `rutis`、`rutis-loader`、`rutis-bridge` 以及随附 Node 运行时包从
0.12.1 升级到 0.15.0 的升级约定。它不替代其他文档；若某份设计描述的 rutis 行为在此发生变化，
该设计在同一次改动中一并更新。

## 1. 范围，以及为什么一步做完

| 项 | 现状 | 目标 |
| --- | --- | --- |
| `Cargo.toml` 工作区依赖 | `rutis`、`rutis-loader`、`rutis-bridge` 0.12.1 | 0.15.0 |
| `runtime/package.json` | `@arcships/rutis-runtime` 0.12.1 | 0.15.0 |
| `runtime/plugins/*/package.json` | `@arcships/rutis` 0.12.1 | 0.15.0 |
| 固定的作者声明 | `docs/typescript-plugin-authoring.md` “main uses 0.12.1” | 0.15.0 |

没有 0.14.0 版本：0.14 的工作（Swift 与 Kotlin SDK、Go 运行时 `restart`）并入 0.15.0 发布。
相关的中间版本是 0.13.0，它整体替换了事件机制。

这次升级无法按宿主与运行时拆开。每个 0.13 端都会在握手中声明 `events-v2`，并拒绝未声明的对端，
且在加载任何内容之前就拒绝：

```text
incompatible session: the far end (<name> <version>) does not declare events-v2
(the event protocol of rutis 0.13): upgrade it to rutis 0.13 or later
```

因此 Rust 宿主（本工作区）、`runtime/` 中的 `@arcships/rutis-runtime`、每个内置 SDK 的插件包，
以及每个连接的节点必须一起升级。只升 Rust 侧会让所有 TypeScript 行无法连接。

不在范围内：`rutis-host`（本项目不运行它）、Go/Swift/Kotlin 运行时、dylib 插件、节点互联、
Cordis 挂载（本工作区未启用 `cordis` feature）。

## 2. 变更内容

### 2.1 0.13：统一事件

- 每个宿主一条逻辑总线。监听器按**事件名**查找，而不是按 Rust 类型：
  `EventKey::<E>::of()` 即 `E::NAME`，同名类型共享监听器，并通过 serde 转换。动态名字
  （`EventKey::dynamic`、`named`）不变。
- `Event` 要求 `Serialize + DeserializeOwned`；`Event::Value` 同样要求。非 serde 的值
  （句柄、通道、`Mutex`）不能放进事件。
- 一张监听器表；由发送方选择模式。五种模式在所有语言一致：`emit`、`parallel`、`serial`、
  `waterfall`、`bail`（Rust 另有 `waterfall_sync`）。`emit` 立即返回，按注册顺序启动监听器，
  失败写入错误汇而不是返回给发送方。`parallel` 等待全部完成，并以
  `CordisError::ListenersFailed`（聚合、按注册顺序）报告。`Some(false)` 视为“无结果”。
  同步模式遇到异步监听器以 `AsyncListener` 失败。
- **scope** 取代按实例键的通道：发送方的 scope 是其 loader 实例链，监听器只接收在自己 scope
  及其之下发出的事件（§4）。
- 监听器被调用时拿到的是**注册它的** `Ctx`，不是发送方的。
- 卸载时先把行的监听器从总线移除，再取消正在运行的调用（`ctx.cancelled()`），并最多等待
  `RUTIS_SHUTDOWN_TIMEOUT`。
- `rutis/*` 名字是保留的框架事件（`rutis/service`、`rutis/row`、`rutis/runtime`）：
  插件可以监听，永远不能发送。
- 插件 API 3（新增 `parallel`、`serial`、`waterfall`、`bail`、`filter`）。运行时仍加载 API 2 插件。

### 2.2 0.13：当前代码在用的内核 API

| 0.12.1 | 0.15.0 |
| --- | --- |
| `Listener::call(&self, ctx, event)` | `fn call(&self, ctx, event, call: Call<'_, E::Value>)` |
| `PatternListener::call(&self, ctx, key, event)` | 末尾多一个参数 `Call<'_, E::Value>` |
| `SyncEvent`（标记 trait） | 移除；`on_sync` 接受任意 `Event` |
| `on_waterfall`、`on_waterfall_sync` | `on` / `on_sync`；监听器自己调用 `call.next()` |
| `Next`、`SyncNext` | `Call::next()`、`SyncCall::next()` |
| `waterfall(..) -> Result<V>`，终态 `-> Result<V>` | `-> Result<Option<V>>`，终态 `-> Result<Option<V>>` |
| `serial(ctx, key, &E)` | `serial(ctx, key, Arc<E>)` |
| `on_pattern(ctx, pattern, ..)`，`EventPattern` 按值 | `on_pattern(ctx, &pattern, ..)` |
| `EventKey::of().instance(id)`、`EventKey::instance`、`*_instance`、`*_keyed` | 移除；改用 scope（`Ctx::scope`、`EventOptions`） |
| `EventKey::name() -> Option<&str>` | `-> &str` |
| `EventPattern::prefixes()`、`any_prefix` | `as_str()`，每次注册一个前缀 |
| `WaterfallListener`、`SyncWaterfallListener`、`PatternWaterfallListener`、`SyncPatternWaterfallListener` | 实现 `Listener` / `SyncListener` / `PatternListener` / `SyncPatternListener`，并在其中调用 `next()` |
| `ListenerKind::{AsyncWaterfall, SyncWaterfall}` | 移除 |
| `EventSubscription { key, prefixes, selected, invoked }` | `{ name, prefix, remote, global, calls, .. }` |
| `DispatchMode` | `rutis::Mode` |
| `EventBacklog.key`、`DispatchAttempt.key` | `name` |
| `CordisError::ReentrantEvent` | 移除（真正的等待环报 `SyncWaitCycle`） |

`TypeKey` 保留 `instance`、`with_instance`、`keyed_dynamic`；只有事件键失去实例身份。

### 2.3 0.13：当前代码在用的 `rutis-bridge` / `rutis-loader`

| 0.12.1 | 0.15.0 |
| --- | --- |
| `LocalRuntime::emits([..])`、`RuntimePlugin::emits` | 移除：运行时监听器自行注册 |
| `Process::emit(name, args)`（会等待全部监听器） | 移除：在 `ctx.events()` 上分发；等待语义用 `parallel`，键为 `EventKey::<rutis_bridge::events::Args>::dynamic(name)` |
| `Mount.emits`、`Mount.events`（`EventSink`） | 移除。`Mount` 新增 `ctx: Option<rutis::Ctx>`（总线与 scope；缺省时运行时的事件只留在自己的进程里）与 `runtime: Option<String>` |
| `Process::load_row(key, entry, config, isolate, inject)` | 末尾多一个参数 `scope: &[rutis::Scope]`（根部为 `&[]`，实例内为 `&ctx.scope()`） |
| 节点 `EventsPlugin`、`NodeEvent`、`Features { outbound, inbound }` | `Features { export, import }`；本项目未使用 |

本项目不自行构造 `Mount`：`NodeRuntime` 以 Session ctx 转发 `rutis::Plugin::apply`
（`crates/dim-app-server/src/lib.rs:1044`），所以 0.15 会自动把 `Mount::ctx` 设为 Session ctx。
之后加载的行需要显式传入 `scope`。

### 2.4 0.15：对 Rust 嵌入方新增或修复的内容

相关：

- 本地运行时问候超时由 10 秒改为 30 秒（启动慢不再被判失败）。
- `session::Dispatch` 与 `peer::Handler` 各新增一个带默认实现的方法。
- `build.rs` 导出错误的 `code`：单字段结构体现在取类型名。
- `Tap::answered`、`events.expected.json` 的新键：只与运行时和测试工具作者有关，本项目都不使用。
- `#[rutis::service]`、`rutis::CallError`、`TypeKey::type_name`、`Loader::waiting_for`、
  `Provider::Marked`、`rutis-host types`：`build.rs` `Exports` 列表的替代方案。本项目的
  `Exports` 列表在 0.15 下照常工作，因此这是一个选项而非要求（§5 D7）。

无关：Swift/Kotlin 应用宿主、`runtimes.swift`、Go `restart`、dylib 标记、`rutis-host check`。

### 2.5 影响本项目正确性的行为变化

- **scope 过滤**是必须围绕它设计的那一项变化（§4）。
- `emit` 仍然不等待（0.12 也是如此），因此现有的 `emit` 调用点语义不变。返回的错误只是拒绝
  （`ReservedEvent`、跨总线）；监听器失败在它自己运行处报告
  （`rutis: listener of <name> in row <row> failed: …`），不会回到发送方，所以现有的
  `tracing::warn!(%error, "… was not announced")` 仍然只覆盖拒绝情形。
- `parallel` 的失败以 `CordisError::ListenersFailed { name, errors }` 到达，
  `AgentError::aggregate`（`crates/dim-agent-runtime/src/error.rs:37`）不会展平它
  （它只展平 `Aggregate`）。本项目有两处 `parallel`；补上这种展平，使诊断保持“每个原因一行”。
- 卸载现在会等待运行中的监听器调用。本项目的处理器会等待存储与服务，因此慢处理器最多会把分支
  关闭延长 `RUTIS_SHUTDOWN_TIMEOUT`（10 秒）后才被取消。分发器的监听器
  （`SubscriptionListener`）应观察注册 ctx 的取消，以保证关闭延迟不退化（§5 D6）。

## 3. 当前用法

本工作区（`crates`、`tests`、`xtask`）的统计：10 个 `Event` 实现、16 个文件里的 23 个
`Listener` / `PatternListener` 实现、46 处 `EventKey`、14 处 `emit` 与 2 处 `parallel`、
1 处 `waterfall_sync`、1 处 `on_waterfall_sync`、2 处 `on_pattern`、2 处 `Process::emit`，
以及 1 处 `.emits(..)` 构建调用。

### 3.1 事件类型

| 事件 | 名字 | 载荷 | 当前 serde |
| --- | --- | --- | --- |
| `SessionBroadcastEvent`（`crates/dim-agent-capabilities/src/session_broadcast.rs:13`） | `session.broadcast`，以及动态的 `session/{id}` | `Arc<AgentEvent>`、`Arc<SessionNotice>`、`Fence`、`Closed` | 枚举本身无 derive；`SessionNotice` 只有 `Serialize` |
| `Committed`（`crates/dim-agent-runtime/src/core/dispatcher.rs:17`） | `timeline.committed` | `Arc<AgentEvent>` | 无 derive |
| `BeforeCommit`（`crates/dim-agent-runtime/src/core/timeline.rs:25`） | `timeline.before` | `EventDraft` + `Mutex<Vec<TraceStep>>` | 无 derive；`steps` 非 serde |
| `SettingsChanged`（`crates/dim-agent-capabilities/src/settings.rs:19`） | 动态 `settings/{source}` | `source`、scope、revision | 是 |
| `CatalogChanged`（`crates/dim-agent-capabilities/src/skills/catalog.rs:29`） | `skills.catalog_changed` | `u64` 版本号 | 无 derive |
| `ToolsChanged`、`ToolProviderChanged`（`tool_registry.rs:28`、`:35`） | `tools.changed`、`tools.provider_changed` | 单元 | 无 derive |
| `ProjectAuthorized`（`crates/dim-app-server/src/settings.rs:74`） | `settings.project_authorized` | `root`、`cwd` | 是 |
| `SessionRestored`（`crates/dim-app-server/src/agents.rs:80`） | `agent/session-restored` | `agent_id`、**`Session` 句柄** | 无 derive；句柄非 serde |
| `UnloadedSessionDeleted`（`agents.rs:93`） | `agent/unloaded-session-deleted` | 两个 id | 无 derive |

`SessionRestored` 无法满足 0.13 的约束：`Session` 是运行时句柄。`BeforeCommit` 携带本地 trace
步骤，必须保持本地。

### 3.2 需要移除实例键的位置

| 位置 | 现状 | 之后 |
| --- | --- | --- |
| `timeline.rs:870` `before_key` | `EventKey::of().instance(self.0.instance)` | `EventKey::of()` |
| `timeline.rs:205` 分发 | 同一个键 | `waterfall_sync(..)`，终态返回 `Ok(Some(draft))` |
| `dispatcher.rs:113`、`:216` | `.instance(self.instance)` | `EventKey::of()` |
| `tool_registry.rs:351`、`:895` | `.instance(scope.instance)` | `EventKey::of()` |
| `tool_binding.rs:375` | `.instance(self.scope.session.instance)` | `EventKey::of()`（被替换，§5 D3） |
| `agents.rs:1466-1467` | `.instance(self.root.instance())` | `EventKey::of()` |
| `mcp/session.rs:156` | `.instance(self.scope.instance)` | `EventKey::of()` |

除 §4 列出的流向之外，这些键今天都等于或低于监听器的 scope。

### 3.3 bridge 与运行时位置

- `session_typescript.rs:511-517`：`LocalRuntime::node(..).named(..).emits([..])`
  —— `.emits(..)` 已移除。
- `session_typescript.rs:166` 与 `:251`：`process.emit(name, vec![value])`
  —— 已移除；两处今天都会等待全部监听器（对应按 Session 的有序队列）。
- `session_typescript.rs:77`：`key.name().and_then(..)` —— `name()` 现在返回 `&str`。
- `session_typescript.rs:112`、`:242`：`on_pattern(ctx, pattern, ..)` 按值传 pattern。
- `session_typescript.rs:212-222`：Settings/项目授权中继与 `forward_session_events` 运行在
  `NodeRows` 内，而它是进程行（`lib.rs:478`、`:685`），因此监听器位于根 scope。投递不变；
  失去的是向下的投递手段（§4、D4）。

### 3.4 TypeScript 包与测试

- `runtime/plugins/session-title/session-title.test.mjs:48` 传入
  `{ emits: ['session.event'] }` —— 0.13 已移除，并有明确报错信息。
- `session-title`、`goal`、`web` 测试里 `await loaded.emit(..)` 用来让监听器执行；
  从 0.13 起 `emit` 立即返回。等待改为 `loaded.parallel(..)`；监听器失败从 `t.reported`
  读取（共享总线形式为 `loaded.reported()`）。
- `docs/typescript-plugin-authoring.md` 固定 0.12，且把 leaf 事件写成 0.11 时代的语义。

## 4. 语义变化：事件只向上传递

0.13 的唯一规则：**监听器接收在自己 scope 以及其下每个 scope 中发出的事件。** 不向上，
也永远收不到兄弟 scope 的事件。

```text
进程根                        scope []                      能听到：全部
└── agent 实例                [agent]
    └── Session 实例          [agent, session]
        ├── Session 行        [agent, session]             能听到：Session 及其下
        └── 分支实例          [agent, session, branch]
            └── 分支行        [agent, session, branch]     只能听到：分支
```

本项目把按实例键的事件当作同一实例子树内的双向通道（0.12 的分发不考虑方向）。在 0.13 下同一个
键变成单向投递，因此这些流向必须重新表达。

| 流向 | 发送方（scope） | 监听方（scope） | 0.13 结果 |
| --- | --- | --- | --- |
| `session/{id}` 广播 | 分支 | App Server 连接（根）、进程 TS 中继（根）、测试（根） | 投递（向上） |
| `timeline.committed` | 分支 | 分支订阅者 | 投递（同级） |
| `timeline.before` | 分支 | 分支中间件 | 投递（同级） |
| `tools.provider_changed` | Session | ToolRegistry、MCP Session（Session） | 投递（同级） |
| `agent/session-restored`、`agent/unloaded-session-deleted` | 根 | Collaboration（根） | 投递（同级） |
| `rutis/service`、`rutis/row` | 提供方所在行（Session、分支） | ToolRegistry、`ProjectedServices`（Session） | Session/分支行投递；根 scope 的提供方收不到 |
| `tools.changed` | Session | `ToolBinding`（分支） | **不投递**（向下） |
| `skills.catalog_changed` | 根 | Skills Session 行（Session） | **不投递**（向下） |
| `settings.changed` | 根 | Skills 目录（根）、进程 TS 中继（根）、测试（根） | 投递（同级） |
| `settings.project_authorized` | 根 | MCP 配置（根）、进程 TS 中继（根） | 投递（同级） |

如果只升级而不处理本节：分支 `ToolBinding` 在工具来源变化后不再刷新，Skills Session 行在目录
变化后不再唤醒——两者都是静默的（无错误、无超时）。另外，TypeScript 行完全收不到宿主事件，因为
`Process::emit` 被移除（D4）；那是 API 删除，不是 scope 问题。这两点都说明它是设计决策而不是
编译修复。

## 5. 决策

### D1 —— 宿主、运行时与插件作为一次升级单元

三个 crate、`runtime/package.json`、三个插件包及其 lock 文件一起升级。保持
`runtime/plugin-sdk.test.mjs` 通过：它会打包 `npm/`、复制
`runtime/node_modules/@arcships/rutis` 并对作者指南做类型检查，因此作者类型与 SDK 版本必须同步。

### D2 —— 用 scope 取代事件实例键

事件键失去实例身份；隔离来自 loader 标记的实例链（每个创建的组实例都会
`Ctx::mark_scope`）。本项目没有代码调用 `mark_scope`；`Loader::create_instance` 已经产生
装配使用的实例（`crates/dim-agent-runtime/src/assembly.rs:543`、`:549`）。在所有位置移除
`EventKey::of().instance(..)`（§3.2），并在没有其他读取者后删除只用于喂键的 `instance` 字段
（`Timeline`、`Dispatcher`）。

### D3 —— 向下的通知不是事件

按流向采用三种形态：

1. **依赖方读取其所有者的状态。** Session 级服务的分支消费方改用该服务的状态而不### D3 —— 向下的通知不是事件

Settings 与项目授权无需改动：它们的发送方与全部监听方都是根 scope 的进程行
（`SettingsPlugin`、`NodeRows`、`McpPlugin` 都注册在进程 loader 的 `builtins` 中，
`crates/dim-app-server/src/lib.rs:478`），投递是同级的，保持原样。真正断裂的是两条“消费方位于
发送方之下、且已持有所有者服务”的流向：

- `ToolBinding`（分支）改为读取 `ToolRegistry` 现有的来源状态 watch
  （`sources_state`，`tool_registry.rs:236`），不再依赖 `tools.changed`。
- Skills Session 行改为读取 `SkillsCatalog` 的版本 watch，不再依赖 `skills.catalog_changed`。

两者本就作为依赖持有服务句柄，因此不引入新服务、新传输或新事件；被删除的事件没有其他（向上的）
消费方，所以两个事件一并删除，其名字也从诊断中消失。`SkillsCatalog` 需要新增一个小 watch
（目前没有）；`ToolRegistry` 不需要。

这两个替换都需要生产者/消费方契约，而不只是“读那个 watch”。第一个已存在；第二个尚不存在，
作为 R1 上报（§10）。

#### D3.1 `ToolRegistry` 来源状态（已存在）

| 方面 | 约定 |
| --- | --- |
| 观测值 | `ToolSourcesState`：每个已注册来源的状态（`tool_registry.rs:96`），包含 `ToolSourceRejection` |
| 何时变化 | 每次让注册表 `send_if_modified` 生效的变更（来源新增、替换、退役，或记录拒绝） |
| 消费方如何取得 | `ToolRegistry::sources_state()`（`tool_registry.rs:236`），位于 `ToolBinding` 已在 `Deps` 声明的句柄上 |
| 初始状态 | 接收端从当前值开始；`ToolBinding` 打开时仍自行做首次读取，watch 只驱动之后的刷新 |
| 关闭 | 注册表释放时发送端被丢弃，`changed()` 返回 `Err`，绑定刷新任务结束（Session 关闭） |
| 失败 | 被拒绝的来源本身就是状态的一部分，因此唤醒点与原先发送事件处完全一致，不新增错误路径 |
| 验收 | watch 前完成首次读取；替换刷新一次；拒绝可见；无变更不唤醒；关闭结束任务 |

#### D3.2 `SkillsCatalog` 版本 watch（新增，R1）

| 方面 | 约定 |
| --- | --- |
| 观测值 | `CatalogVersion`（`u64`），通知计数器——不是持久身份，也不跨进程比较 |
| 何时变化 | 只在 `advance()` 中（`catalog.rs:149`）：根目录重扫或禁用集合变化；严格递增 |
| 消费方如何取得 | `SkillsCatalog::changes() -> watch::Receiver<CatalogVersion>`，位于 Session 行已在 `Deps` 声明的 `Arc<SkillsCatalog>` 上；不新增服务 |
| 初始状态 | `subscribe()` 给出当时的版本；该行已执行首次 `query()`，因此不需要重放 |
| 关闭 | 目录释放时发送端被丢弃，`changed()` 返回 `Err`，该行任务结束 |
| 失败 | 设置重读失败不推进版本，因此不唤醒——与原先事件只在该处发送一致；`query()` 仍报告已存的错误 |
| 验收 | 从版本 N 启动的行读到 N 且不重扫；装配期间推进在发布后唤醒（现有测试改写）；重复推进合并收敛；重读失败不唤醒；关闭结束任务 |

被否决的替代方案：把状态变化作为事件转发进各子树（一个自身不持有状态的中继）；把这些消费方的
监听器注册到进程根（会丢掉 `timeline.*` 与 `tools.*` 依赖的子树隔离）；让消费方直接读所有者的
私有状态（watch 才是服务约定）。

### D4 —— 宿主到行的事件在 agent 的 scope 内分发

中继（`forward_session_events`、`forward_settings_events`）与运行时都是进程行，因此它们的 ctx
是进程根；`NodeRuntime` 用该上下文应用运行时，0.15 因此把 `Mount::ctx` 设为它
（`crates/dim-app-server/src/lib.rs:478`）。用根上下文分发不会到达任何行，因为行位于 Session 与
分支 scope 中（已对 0.15.0 验证：根部 `emit` 只到达根监听器）。中继必须在 agent 实例内分发；
App Server 已经持有该上下文：`AgentHost.ctx`（`crates/dim-app-server/src/agents.rs:76`），
可从 `AgentIndex` 取得：

```rust
let key = EventKey::<Args>::dynamic(name);
agent_ctx
    .events()
    .parallel(&agent_ctx, &key, Arc::new(Args(args)))
    .await?;
```

agent scope 是该 agent 所有 Session 的最近公共父级，因此保持了今天的语义：`Process::emit`
广播给运行时的每一行，各行再按绑定的 Session id 过滤。`parallel` 保持顺序（中继的每 Session
队列逐条等待投递）与失败报告。更窄的“按 Session 分发”需要 `Session` 暴露自己的上下文，并且会把
Settings 投递从“每行自行过滤”变成“只有目标 Session 的行收到”，因此不属于本次升级。需要在
[session-typescript.md](../../design/session-typescript.md) 记录：宿主事件到达它被分发的那个
agent 的行；分支行若需要，必须从该分支内部派发（目前没有分支行监听宿主事件）。

alogChanged`、`ToolsChanged`、
  `ToolProviderChanged`、`UnloadedSessionDeleted` 派生 `Serialize`/`Deserialize`；
  `SessionNotice` 补 `Deserialize`（`Arc` 需要 serde 的 `rc`，工作区已启用）。
- `BeforeCommit` 的 trace 步骤保持本地：`#[serde(skip)] steps: Mutex<Vec<TraceStep>>`
  （反序列化时用默认值）。trace 按设计从不跨进程；该事件只由分支自己的
  `waterfall_sync` 在进程内分发。
- `SessionRestored` 携带 `agent_id` 与 `session_id`；监听器通过 `AgentIndex` 解析 `Session`，
  句柄本来就在那里（`crates/dim-app-server/src/collaboration.rs:522`）。事件里放句柄在 0.13
  是违反约定的。

### D6 —— 监听器迁移是机械的，只有一处取消语义变化

补上 `Call` / `SyncCall` 参数，把 `on_waterfall_sync` 换成 `on_sync`、`SyncNext` 换成
`SyncCall`（`timeline.rs:846-870`），让时间线中间件与终态返回 `Option`，给 `serial` 传
`Arc<E>`（`dispatcher.rs:216`），按引用传 `&EventPattern`（`session_typescript.rs:112`、`:242`），
并适配 `EventKey::name()`。

在 `SubscriptionListener::call` 中，让处理器与注册 ctx 的取消竞争，这样卸载等待监听器后分支
关闭的延迟不退化：

```rust
tokio::select! {
    _ = ctx.cancelled() => Ok(None),
    result = handler(event) => { .. }
}
```

### D7 —— 保留 `build.rs` 导出列表

`Exports::new().service(..).service_in(..).declared(..).typescript(..)` 在 0.15 下照常工作，
`include_exports!` 不变。`#[rutis::service]` 能去掉 `build.rs` 列表，但会引入一个标记和第二种
声明同一约定的方式；只在升级变绿之后单独改动中采用。

### D8 —— 不改事件名

名字现在是全局的，因此就是约定。本项目的名字已唯一，且不在 `rutis/*` 与 `internal/*` 之内；
保留它们，并记录新名字不得与框架名字冲突。`tools.changed` 与 `skills.catalog_changed`
随 D3 消失，而不是改名。

## 6. 实施计划

每个阶段都是完整、可审查的改动，并有自己的验证；P1 是一个提交，因为 0.13 没有兼容路径，
只做一半的拓扑改动会是静默错误的。

| 阶段 | 工作 | 门槛 |
| --- | --- | --- |
| P0 | 升级 `Cargo.toml`（3 个 crate）、`cargo update -p rutis -p rutis-loader -p rutis-bridge`；升级 `runtime/package.json`、三个插件 `package.json` 与 lock 文件（各自 `npm install`）；更新作者版本声明 | `cargo metadata` 可解析；lock 显示 0.15.0 |
| P1 | 内核事件迁移：serde 派生与载荷改动（D5）、监听器签名（D6）、`on_sync`/`SyncCall`/`Option`（时间线）、`serial(Arc)`、`on_pattern(&)`、移除实例键（D2）、拓扑（D3 的两个契约）。**被 R1 阻塞**（[#362](https://github.com/arcships/dim/issues/362)）：必须先落地 `SkillsCatalog` watch 契约，才能移除第一个 `skills.catalog_changed` 消费方 | `cargo test --workspace --locked`，覆盖受影响流程：时间线批写、工具绑定、Skills、Settings、Session 广播 |
| P2 | bridge/运行时迁移：移除 `.emits(..)`；把两处 `Process::emit` 换成从 agent 上下文使用的 `parallel`（D4）；用一个真实 Node 行的测试确认行加入总线、分支行按 scope 收到事件 | `cargo test --workspace --locked`；Node 相关 profile 测试 |
| P3 | TypeScript：去掉 `emits: [..]`，把 `await emit` 改为 `parallel`，通过 `t.reported` 断言失败；更新 `docs/typescript-plugin-authoring.md`（版本、事件语义） | `runtime/plugins/*` 插件测试；`runtime/plugin-sdk.test.mjs` |
| P4 | 工具链与文档：`cargo run --locked -p xtask -- graph` 与 `check`；更新 0.12.1 引用（§8）；在架构文档中记录 scope 规则 | `xtask check`；`node --test scripts/*.test.mjs` |

## 7. 验证

工作区根目录的完整顺序（与 CI 一致）：

1. `./dev check`
2. `cargo clippy --workspace --all-targets --locked -- -D warnings`
3. `cargo test --workspace --locked`
4. `cargo run --locked -p xtask -- check` 与 `graph`（装配与 crate 依赖不变，快照不应变动；
   若有 diff 说明这次升级改到了不该改的地方）
5. `node --test scripts/*.test.mjs`
6. Node 绑定，按 CI 的方式：`npm ci --prefix runtime` 与三个插件包，
   `npm install --no-save --install-links` 链接插件，`node --test runtime/plugin-sdk.test.mjs`，
   四个插件测试（`runtime/plugins/{goal,web,session-title,workflow}/*.test.mjs`）以及三个
   `tsc -p runtime/plugins/*/tsconfig.json` 类型检查；
   `git diff --exit-code -- npm/branch-services.d.ts` 确认生成的声明（也即 `Exports` 生成器）没有变化。
7. Node 驱动的 Rust 流程：设置 `DIM_TEST_NODE`、`DIM_TEST_JS_RUNTIME`、`DIM_TEST_RUTIS_SDK`
   后运行 `dim-app-server` 的 `node_` 测试，以及
   `cargo test -p dimcode --test cli_flow -- --ignored`。

本次改动必须覆盖的流程检查（优先复用现有测试，只在约定变化处扩展）：

- Settings 变更 → TypeScript 行收到 `settings.changed`（从 agent 上下文分发，D4）
- 项目授权 → TypeScript 行收到 `settings.project_authorized`
- 工具来源变化 → 分支 `ToolBinding` 从 `sources_state()` 刷新：首次读取、替换、拒绝可见、
  无变更不唤醒、关闭结束任务（D3.1）
- 目录版本变化 → Skills Session 行从 `changes()` 刷新：初始版本不重扫、装配期间推进在发布后
  唤醒、重复推进合并、重读失败不唤醒、关闭结束任务（D3.2）
- 监听器运行中关闭分支：关闭延迟有界，无遗留任务
- Session 广播 → 连接通知不变（向上投递）

设计中的两条主张已在评审时用未提交的临时 crate 对 0.15.0 验证：(a) §2.2、§2.3 以及
D4/D6/D5 片段中的 API 映射按原样可编译；(b) §4 的 scope 规则成立——Session scope 的监听器
听不到父级或根部发出的事件，根监听器能听到其下每个 scope 的事件。上面的流程检查与 CI 序列
仍是合并门槛。

受管提供方插件（私有，按 rutis 0.10 API 2 打包，在
`crates/dim-app-server/src/lib.rs:542` 校验）会在升级后的 Node 运行时中运行。0.13+ 运行时仍可
加载 API 2 插件，但它的 `emit` 时序与内置 SDK 必须在发布前对 0.15 宿主重新验证；除非重建该插件，
`rutisVersion` 校验保持原样。

## 8. 需要更新的文档

- `docs/typescript-plugin-authoring.md`：固定的版本、leaf 事件语义
  （`emit` 不等待，改用 `parallel`；`emits` 声明已移除）。
- `docs/design/session-typescript.md`、`branch-typescript.md`、`process-typescript.md`：
  0.12.1 引用与宿主到行的投递 scope。
- `docs/design/appserver/management-methods.md`、`sessions-service.md`、`p6-p7-bindings.md`：
  0.12.1 引用。
- `docs/design/appserver/settings-service.md`：投递路径（agent scope 分发，D4）。
- `docs/design/plugins/Tui.md`、`docs/design/roadmap.md`：版本引用。
- `docs/architecture/rust-agent/lifecycle.md`、`principles.md`、`single-tree.md`：
  事件由实例键改为 scope。
- `docs/INDEX.md`：本文。
- 存在对应译文的 `docs/zh/` 文件。

## 9. 风险

| 风险 | 应对 |
| --- | --- |
| §4 未列出的向下流向被静默丢失 | P1 的门槛跑五个受影响流程的测试；合并前逐个检查每处 `emit` 站点并归类其监听器 |
| 运行时与宿主版本漂移（握手被拒） | P0 在一个提交内升级所有固定版本；发布归档从 `runtime/package.json` 记录 `toolchain.rutis`（`scripts/stage-runtime.mjs:165`），漏改会在发布清单中暴露 |
| 卸载等待监听器调用 | D6 在分发器监听器中处理取消；分支关闭测试断言延迟 |
| `SessionRestored` 载荷变化 | 仅 App Server 内部；恢复流程测试覆盖 |
| 宿主到行的分发 scope 选得过高或过低 | 过高什么都收不到，过低会漏行；Node 相关流程测试断言 Session 行收到 `session.event` 与 Settings 事件 |
| `ListenersFailed` 不再被展平进 `AgentError::Multiple` | 扩展 `AgentError::aggregate` 遍历它；不改变用户可见格式 |
| 私有插件（0.10 API 2）在 0.15 运行时下行为不同 | 发布前重新验证；必要时用 0.15 SDK 重建 |

回滚：回退升级改动与四个固定版本；本次升级不移动任何持久化格式（时间线、存储与 wire fixture
都不受影响）。

## 10. 需要上报给机制所有者的需求

### R1 —— `SkillsCatalog` 所有者状态 watch（阻塞 P1，[#362](https://github.com/arcships/dim/issues/362)）

- **问题**：Skills Session 行只通过总线事件 `skills.catalog_changed` 得知目录推进；在 0.15 scope
  下该事件在行的上层发出，行收不到，因而不再刷新。
- **现有机制为何不足**：目录唯一的 watch 是它内部的设置唤醒（`catalog.rs:310`）；版本只通过
  `query()` 只读暴露，而出问题的正是事件这一机制。`ToolRegistry` 给出了形状
  （`sources_state()`），但它承载的是工具来源状态，不是目录版本。
- **通用能力**：目录服务上的版本 watch（`SkillsCatalog::changes()`），契约见 D3.2。
- **顺序影响**：对 Timeline、存储与装配无影响。唤醒仍是合并式的、发布后生效：该行已把首次更新
  推迟到 `SessionLifecycle::activated()`。
- **所有者与 issue**：`dim-agent-capabilities` 的 `SkillsCatalog`：
  [#362](https://github.com/arcships/dim/issues/362)。在它落地前，P1 保留事件。

本次升级没有其他机制是 0.15 缺失的：host→row 分发使用 App Server 已持有的 agent 上下文（D4），
工具 watch 已存在（D3.1）。

## 11. 后续项（非前置）

以下各项不阻塞 P0–P4；如被接受，各自单独改动。

1. 按 Session 分发 host→row（需要 `Session` 的上下文访问器）。今天的 agent scope 分发保持
   “广播、各行自行过滤”的现状；把它变窄属于 Settings 投递行为变化。
2. `rutisVersion == "0.10.0"` 插件包校验：等私有插件重建后再决定是否改为最低版本。
3. 升级变绿后采用 `#[rutis::service]`（D7）。

## 12. 设计规则检查

本次升级不新增 Capability 也不新增插件；下表说明每条规则对“移动事件机制”这类改动意味着什么。

| 规则 | 回答 |
| --- | --- |
| §4.1 Capability 与 `Deps` | 不新增 Capability。由于 `ToolBinding` 与 Skills Session 行已在 `Deps` 中声明 `ToolRegistry` / `SkillsCatalog`，D3 不需要新的依赖声明。 |
| §4.2 关键效果走事件 | 留在总线上的部分不变。两个事件被删除（D3），因为其唯一消费方是向下的；不引入新事件，也不把协调改成对 Capability 不拥有的句柄的直接调用。 |
| §4.4 状态归属 | D3 让状态留在所有者处（注册表的来源状态、目录的版本号），由依赖方读取；不复制、缓存或从事件重建状态。 |
| §4.6 冻结 fixture | `crates/dim-agent-types/tests/fixtures/` 不动：事件种类、种类的键与存储格式都不移动。 |
| §4.7 复用现有机制 | 使用 `Dispatcher`、既有的状态 watch；不新增端口或宿主 trait。 |
| ADR 002 D1–D5 | D1/D2：不新增接线模块，不做跨插件直接调用；D4 在既有总线上从既有上下文分发。D5：只有具名服务承载状态；不新增绑定。 |
| 需要上报给机制所有者的需求 | 一项：R1（§10），`SkillsCatalog` 版本 watch，含问题、替代方案、契约（D3.2）、顺序影响与所有者。它阻塞 P1。按 Session 分发的替代方案属于后续项（§11），不是前置，因为从既有 agent 上下文分发今天即可用。 |

## 13. 消融复核

- scope 让代码变少而不是变多：`before_key`、`Committed` 的实例键、`Dispatcher::instance`，
  以及 `Timeline::branch` 中与实例有关的那一半，都只为事件隔离而存在；D2 之后它们被删除，
  而不是被替换。
- D3 不新增抽象：两个消费方使用的 watch，一个是所有者本就有的（ToolRegistry），
  一个来自它本就维护的计数器（SkillsCatalog）。
- D4 不新增服务、插件或 scope 覆盖：它把两处分发移到 App Server 本就持有的上下文
  （`AgentHost.ctx`）。
- 不为“按名字发送”引入包装：D4 只有两个调用点，且都在同一个文件里；现在提取只会制造耦合。
  出现第三个时再提取。
- 不改名、不新增事件名、不做兼容层：0.13 没有旧路径，兼容层只会掩盖已经断裂的流向。
