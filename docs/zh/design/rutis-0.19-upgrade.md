# 升级到 rutis 0.19.3

[English](../../design/rutis-0.19-upgrade.md)

状态：Ready。

Issue：[#681](https://github.com/arcships/dim/issues/681)。

规则：[设计规则](../../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)、
[ADR 006](../adr/006-plugin-contract.md)、[Dim 如何使用 rutis](rutis-usage.md)。

本次升级把 `rutis`、`rutis-loader`、`rutis-bridge`、随附的 Node 运行时、仓库内的 TypeScript 插件以及
Python SDK 从 0.17.1 升到 0.19.3。rutis 0.18 和 0.19 都有不兼容变更（[0.18 迁移指南](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.18.md)、
[0.19 迁移指南](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.19.md)）。本次升级不保留兼容路径。
rutis 删掉的 API，Dim 一律改用 rutis 规定的新写法，不加重建旧 API 的包装层。

## 1. 范围，以及为什么一步做完

| 项 | 现状 | 目标 |
| --- | --- | --- |
| `Cargo.toml` 工作区依赖 | `rutis`、`rutis-bridge` 0.17.1；`rutis-loader` 0.17.1，带 `node`、`python` | 0.19.3；`rutis-loader` 只带 `node`（0.19 没有 `python` feature，`node` 会启用 `runtimes`） |
| 新增依赖 | — | 无。Dim 自己拉起运行时（M4）；npm 上的 `@arcships/rutis-host` 只是测试用的开发依赖（M10） |
| `xtask` | `rutis-bridge`，带 `exports` | `rutis-types` 0.19.3 |
| `runtime/package.json` 及锁文件 | `@arcships/rutis-runtime`、`@arcships/rutis` 0.17.1 | 0.19.3；另加开发依赖 `@arcships/rutis-host` 0.19.3（M10） |
| `runtime/plugins/{goal,scheduler,session-title,workflow}/package.json` 及锁文件 | `@arcships/rutis` 0.17.1 | 0.19.3 |
| `npm/package.json` 的 peer 依赖 | `@arcships/rutis` `~0.17.0` | `~0.19.0` |
| `runtime/python-requirements.txt` | `rutis==0.17.1` 及其哈希 | `rutis==0.19.3` 及其哈希 |
| 作者指南的固定版本 | [作者指南](../../typescript-plugin-authoring.md) | 0.19.3 |
| `.github/workflows/ci.yml` | Node 绑定一步用 0.17.1 的测试工具跑插件测试 | 为 0.19 的测试工具导出 `RUTIS_HOST`（M10） |

0.19 使用会话协议 4，握手时会拒绝 0.18 及更早的对端，两个方向都是如此。因此 App Server、Node 运行时、
Python 运行时和交互式 `dimcode` 进程（UI leaf，M8）必须一起升级。只升级 Rust 侧会让所有 TypeScript 和
Python 行都无法连接。

目标是 0.19，而不是 0.18。0.18 的大部分不兼容变化（诊断、配置校验）原样延续到 0.19；而如果先升到 0.18，
要适配的那些 API（链接、`LocalRuntime`、`RuntimeResolver`）在 0.19 里又都被删掉了。固定版本是 0.19.3。0.19.1
已于 2026-10-30 发布到 crates.io、npm 和 PyPI，它补回了 M2 需要的同步 waterfall（[rutis#947](https://github.com/arcships/rutis/pull/947)），并公开了
[#684](https://github.com/arcships/dim/pull/684) 依赖的 `mark_scope`（[rutis#948](https://github.com/arcships/rutis/issues/948)）。
它和 0.19.0 使用同一个协议，所以 `npm` 的 peer 范围仍是 `~0.19.0`。0.19.2 只修 bug、不增减 API；本次升级用它是为了
[rutis#978](https://github.com/arcships/rutis/pull/978) 让大量实例一起关闭时的峰值变成线性（#714）。0.19.3 把 rutis
自己的两个服务 `loader` 和 `sessions` 改名为 `rutis/loader` 和 `rutis/sessions`
（[rutis#986](https://github.com/arcships/rutis/issues/986)），把这两个名字留给宿主；它还把被 dispose 或子树
shutdown 取消的构造的清理错误放回结果（[rutis#983](https://github.com/arcships/rutis/issues/983)）。本次升级两点都用。

不在范围内：Go、Swift、Kotlin 运行时，dylib 插件，远程运行时与节点连接行（`rutis-host/dial`、
`rutis-host/listen`），`rutis-host` 命令，以及 Cordis 宿主。Dim 都不使用。`rutis-migrate` 停在 0.18，不使用它。

## 2. 0.17.1 到 0.19.3 之间的变化

| 版本 | 与 Dim 相关的内容 |
| --- | --- |
| 0.17.2 | 无 API 变化。坏值只让这一次调用失败；运行时退出不再连累其他行；10 秒后报告 `StillWaiting` |
| 0.18 | 错误文案带错误码（`session::Error::Binding`）；默认错误 sink 写标准错误；配置按 schema 校验并填入默认值；`reconcile` 不再等待仍在启动的运行时；带方法的对象可以传给每种语言 |
| 0.19 | 本地运行时是一个会话的对端；链接、peer、传输和身份这一层整个删除；服务目录共享所有未登记的名字；运行时的行遵守 `isolate`；运行时的每次 emit 都经过宿主；`LoaderPlugin` 提供服务 `loader`；每个 loader 只用一个 `rutis/row` 监听器，而不是每行一个（[rutis#808](https://github.com/arcships/rutis/pull/808)，即 [#518](https://github.com/arcships/dim/issues/518) 在等的修复）；SDK 测试工具运行在真实的 `rutis-host` 上；要求 Python 3.12 及以上；线上错误名 `CordisError` 改为 `Error` |
| 0.19.1 | 同步 waterfall 以 `waterfall(event).sync(terminal)` 的形式补回（M2）；外语插件定义的副本可以挂在宿主自己的实例里，`mark_scope` 公开（rutis#948，#684 使用）；`Loader::instance_rows`；运行时 `ready()` 之后的 `reconcile` 一定装载它的行；rutis-host 的运行时插件从创建起就是 Starting（rutis#964；Dim 不用它，M4） |
| 0.19.2 | 只修 bug、不增减 API：大量实例一起关闭时的峰值变成线性（rutis#978）；被驱动取消的加载中插件回到 Pending，它的清理错误只进 error sink（rutis#969，见 §8）；放弃的 `reconcile` 不再卡住后续操作；TypeScript 测试工具在每个测试结束时卸载插件 |
| 0.19.3 | rutis 自己的服务破坏性改名：`loader` 变成 `rutis/loader`，`sessions` 变成 `rutis/sessions`，只有注入或调用它们的代码需要改（rutis#986）；被 dispose 或子树 shutdown 取消的构造卸载到 `Disposed`，那次 dispose 或 shutdown 重新返回它的清理错误（rutis#983）；测试调用的回调与插件调用的假服务交错时 TypeScript 测试工具会返回（rutis#987），一次假服务 provide 失败只让那一次 `load` 失败（rutis#988） |

### 2.1 Dim 调用的内核 API

| 0.17.1 | 0.19.3 |
| --- | --- |
| `Ctx::waterfall_sync(event, terminal)` | 在 0.19.0 中删除，迁移指南给的替代是 `waterfall`（异步）或同步的 `bail`，两者都不适用于 `before` 链。0.19.1 补回了 `ctx.waterfall(event).sync(terminal)`（rutis#946），M2 用它 |
| 同步监听器里的 `SyncCall::next()` | 总是返回 `Err(WaterfallNext)`：同步监听器后面没有链可以继续执行 |
| `Ctx::root()` 的默认错误 sink | 仍把每一种上报的错误写到标准错误，只是去掉了 `[rutis]` 前缀；0.18 上报的种类更多：`StartFailed`、`RuntimeDown`、`StillWaiting` |
| `ErrorKind` | 新增 `StartFailed`、`RuntimeDown`、`NotServedByName`、`StillWaiting`；`DisposeWaitError::TimedOut` 新增字段 `stuck` |

`Ctx`、`DynPlugin`、效果、`Key`、`TypeKey`、`Ctx::isolate`、监听器、`on_sync`、`prepend` 和
`#[rutis::service]` 保持 0.17.1 时的契约。

### 2.2 Dim 调用的 bridge 与 loader API

| 0.17.1 | 0.19.3 |
| --- | --- |
| `rutis_bridge::runtime::LocalRuntime`（`node`、`python`、`named`、`interpreter`、`python_path`、stdio、`handle`） | 宿主自己的连接行：`rutis_bridge::runtime::Launcher`、`pipe::greeted`、`Session::new`、`pipe::Processes`。不使用迁移指南给的 `rutis_host::connect::SpawnPlugin`（M4） |
| `RuntimePlugin`、服务 `runtime::Runtime` | 删除；由连接插件记录自己运行时的 `RuntimeStatus`（M4） |
| `Stdio::Lines` | `pipe::Stdio::Pipe(factory)`；Dim 的 factory 把逐行输出转给 `tracing` |
| `rutis_loader::runtime::RuntimeResolver::node` / `::modules`、`with_catalog`、`invalidate` | 改为一个 `rutis_loader::runtime::SessionResolver`（`npm(&catalog, row)`、`attach(&loader)`、`follow_row(row)`）；没有需要失效的缓存；npm 名字由 Node 解析 |
| `RuntimeRows`、`RowsSource`、`RuntimeRowsPlugin` | `SessionRows::key(row)`；`SessionResolver::follow_row` |
| `ServiceCatalog::share_registered_only()` | 删除：所有没有另行登记的名字都按名字共享 |
| `Loader::waiting_for(entry)` | `Loader::waits(entry)` |
| `Loader::set_overlay`、`overlays()` | 删除：在交给 `reconcile` 的层列表里放一层自己的 |
| `PeerPlugin`、`LinkConfig`、`LinkState`、`Features`、`Peer`、`peer_key`、`plugins.load` / `plugins.unload` | `Session::handshake(&ctx, channel)`，再挂载 `Greeted`，它的 `id()` 给出对端的名字；在对端运行插件要用 `SessionHandle::run` 或 `<row>:<plugin>` 这样的 loader 行，Dim 都不使用（M8） |
| `Identity`、`StaticIdentity`、`IdentityPlugin`、`identity_key` | `rutis_bridge::LocalId(PeerId)`，每个内核一个 |
| `Transport`、`Dial`、`transport_key` | 删除：连接就是交给会话的一个 `Channel` |
| `transport::local::framed` | `pipe::framed` |
| `Closer::close(&str)` | `Closer::close(Close)` |
| `StaticCatalog::with(name, Described { .. }, factory)` | `StaticCatalog::with(name, Installed::new(factory))`，提供在 `catalog_key()`；不使用：leaf 不提供目录（M8） |
| `rutis_bridge::build::Exports` | `rutis_types::Exports`，API 不变；生成文件的文件头变了 |
| `LoaderPlugin` 提供 `Loader` 和 mounter | 另外还提供标记服务 `rutis/loader`，以及 `catalog_key()` 下的会话插件目录 |

`host_key`、`HostDispatch`、`Build`、`ServiceCatalog`、`Builtins`、`Resolved`、`mounter_key`、
`session::Error` 和 `settle` 保持原有契约。

### 2.3 影响 Dim 正确性的行为变化

- **标准错误。** 默认错误 sink 在 0.17.1 上就会把它上报的内容写到标准错误。0.18 上报得更多：没人等待的启动
  失败、运行时退出、等待超过 10 秒的插件。loader 的配置警告也会写到那里。TUI 运行期间，终端归交互式
  `dimcode` 进程所有（M7）。
- **配置会被校验。** 0.18 起，每一行的配置在 `build` 之前按其 schema 校验并填入默认值。对象类型的 schema
  缺省配置会变成 `{}`，所以 Dim 的模块（`NoConfig`）能通过。配置不符合自身 schema 的 TypeScript 插件，
  现在会让它的行失败。
- **`reconcile` 不再等待仍在启动的运行时**（0.18）。这个运行时的行会等它就绪，不出现在报告里。
- **npm 名字由 Node 运行时解析**（0.19）。不认识的裸名字不再立刻失败，而是等待运行时的会话。
- **运行时的每次 emit 都经过宿主**（0.19）。Dim 的插件只监听 Rust 发出的事件。
- **运行时的行遵守 `isolate`**（0.19）。Dim 不在运行时的行上写 `isolate`；Session 和分支的 TypeScript
  部件是插件定义的副本，没有 `isolate`。
- **会话会导出和公布。** 每个会话都会把非平台插件提供的所有进程级共享服务导出给对端，并把每个监听器的
  名字公布给对端。这包括 Node 和 Python 运行时的会话，也包括 UI leaf 的会话。对端可以运行本端会话插件目录
  能解析的任何插件（0.19）。
  - 注入仍然门控运行时的行能否启动。
  - 绑定调用方行的方法，会以 `not-declared` 拒绝没有声明该名字的调用方（rutis-bridge `marked.rs:842`）。
  - Dim 不靠这两点做隔离：已安装的插件是受信代码（规则 11）。
- **错误文案变了，并带上错误码**（0.18）。Dim 没有按 rutis 的错误文案做匹配。Dim 手写的 dispatcher
  继续返回 `session::Error::Value`。
- **SDK 测试工具运行在 `rutis-host` 上**，被测插件必须是所在模块的默认导出（0.19）。
- **要求 Python 3.12 及以上**（0.19）。下载的运行时是 3.12.15，CI 用的也是 3.12。

## 3. Dim 的当前用法

在 76a5105f 上对已发布的 0.19.1 试编译了所有 target（`cargo check --workspace --all-targets --keep-going`），
结果见下表。第二次按 M2、M3 的做法修好 `dim-agent-runtime` 的两处错误后，编到了排在它后面的 crate。两次都没编到的
crate 靠搜索定位。此前在 70ad3835 上对 0.19.0 做的同样试编译，结果与此相同。实施时会在自己的基线上重新跑一次。

| Crate | 结果 |
| --- | --- |
| `dim-agent-runtime` | 2 个错误：`waterfall_sync`（`core/timeline.rs:318`）、`share_registered_only`（`names.rs:95`） |
| `dim-agent-types`、`dim-agent-capabilities`、`dim-agent-aimux`、`dim-agent-attachments`、`dim-agent-questions` 及其测试 | 修好上面两处后即可编译 |
| `dim-app-server` | 16 个错误：`lib.rs`（运行时的行、`NodeRuntime`）、`session_typescript.rs`、`runtime_manager.rs`、`plugin_manager.rs`、`ui_leaf.rs` |
| `tests/integration` | `extensions.rs`（一个 `LocalRuntime` 及其解析器）；其余测试都能编译 |
| 没编到、靠搜索定位的 | `dim-cli/src/leaf.rs`；`xtask/src/types.rs`；`dim-app-server/tests/transport_flow/ui_host.rs` 和 `session_typescript.rs:1674`；`transport_flow/{scheduler,workflow,process_typescript}.rs` 里的 `set_overlay`（9 处调用）；`examples/session_typescript_spike.rs`；M8 要改名的两处 `ui.leaf` feature 字符串（`dim-app-server/src/transport.rs:725`、`dim-cli/src/lib.rs:148`） |
| Agent 放置（M6，不是 rutis 的错误） | 41 个文件里的 120 处 `build()` 和 39 处 `build_in` 调用：测试、示例，以及 `xtask/src/main.rs`（`graph`、`measure`）和 `xtask/src/plugins.rs`（本地图） |

**与本次升级相关的未完成工作。**

- [#518](https://github.com/arcships/dim/issues/518) 在等 rutis#808，0.19 带上了它。本次升级凭 §6 的测量结果关闭它。
- [#645](https://github.com/arcships/dim/pull/645) 为 [#594](https://github.com/arcships/dim/issues/594) 使用 `Loader::instance_rows`，0.19.1 发布了它（rutis#941），
  rutis#920 则已关闭、没有合并。#645 随后以 0.19.2 上的后续改动合入：装配通过这个 API 读取当前的
  行句柄，行为不变。但 #594 不是这样解决的：Session 和分支变成用公开的 `mark_scope` 标记自己的插件，
  不再有实例行可查。[#684](https://github.com/arcships/dim/pull/684) 承载这个设计（已合入，79fbb1b3），
  它的实现在本次升级之后进行。

## 4. 决策

### M1. 宿主、运行时、leaf 和插件作为一次升级单元

§1 的所有固定版本放在一个提交里升级。握手会拒绝版本混搭，所以任何中间状态都构建不出能用的产品。
`runtime/plugin-sdk.test.mjs` 会打包 `npm/` 并对作者指南做类型检查，因此作者文档里的类型和 SDK 版本一起变化。

### M2. Timeline 的 `before` 链改用同步 waterfall

0.19.0 删掉了 `Ctx::waterfall_sync`，同步监听器也不能再调用链上的下一环。0.19 迁移指南给的两种替代都不适用于
这条链：

- **异步的 `waterfall`**：会让 `Timeline::append` 和所有调用方都变成异步，而且 `on_sync` 监听器在异步 waterfall
  里不能调用 `next`（`NextMisuse::SyncInAsync`）。`Timeline::commit` 在追加守卫和提交锁之内运行这条链，并在那里
  分配序号。
- **同步的 `bail`**：没有 `next`，串不成中间件。

[rutis#946](https://github.com/arcships/rutis/issues/946) 把同步 waterfall 补了回来，写作
`ctx.waterfall(event).sync(terminal)`。它由 [rutis#947](https://github.com/arcships/rutis/pull/947)
合入，在 0.19.1 发布。`before` 链改用它：

- `commit` 在现在调用 `waterfall_sync` 的地方改为调用
  `ctx.waterfall(offered).sync(|offered| Ok(Some(offered.draft.clone())))`。各个变形仍用 `on_sync` 注册，并像现在
  一样调用 `next`。
- 下面几点不变：
  - 私有名字 `dim/timeline/{instance}/before` 和 `Timeline::before`
    （[0.16 U2](rutis-0.16-upgrade.md#u2-分支机制通道使用私有名字)）；
  - 顺序：先注册的中间件包住后注册的，后注册的先变形；
  - 失败规则：某个变形失败时，保留原始 draft，不记录任何步骤。

目前没有任何产品代码注册 `before` 变形，只有测试在用。

### M3. 服务目录共享所有未登记的名字

删除 `share_registered_only`。已登记在服务目录里的名字保持原来的键。外来行注入、而服务目录没有登记的名字，
现在在 rutis 里按它的共享键等待，而不是交给运行时自己处理。[Dim 如何使用 rutis §3.4](rutis-usage.md#34-服务目录)
删掉关于 `share_registered_only` 的那句话。

### M4. 本地运行时是一个连接行

0.19 把本地运行时变成会话的对端，并删掉了运行时插件和它的状态（`LocalRuntime`、`RuntimePlugin`、服务
`runtime::Runtime`）。迁移指南给 `LocalRuntime` 指的替代是 rutis-host 的 `SpawnPlugin`。这个插件是为 rutis-host
自己的命令服务的：它为 rutis-host 的 `status` 和 `restart` 维护 `RuntimeStates`，并通过
`SessionHandle::process()` 读取运行时的进程，而这个方法在 rutis-bridge 里是隐藏的（`#[doc(hidden)]`，给
rutis-loader 用）。Dim 本身就是宿主，这两样都不需要。0.19 里，一个运行时，无论本地还是远端，都是一个连接行：
自己的连接，就是一个把 `Channel` 交给 `Session::new` 的插件；由宿主启动的运行时，命令行是
`<channel> --id <name> <project>`（[0.19 迁移指南](https://github.com/arcships/rutis/blob/v0.19.3/docs/migration-0.19.md)
"一起升级"一节）。拉起进程的插件最清楚进程还在不在，所以由它自己记录这个状态。Dim 不链接 `rutis-host`，
也不照抄 `SpawnPlugin`。

- **运行时是一个连接行。** `session-node` 和 `py` 是进程行。它们的插件 `RuntimeConnection` 做的就是任何连接插件
  做的事，用的都是 rutis-bridge 的公开零件：
  - 用 `rutis_bridge::runtime::Launcher` 按运行时的启动命令启动进程，用它自己的 channel、带 `--id <name>`，把进程记进
    它自己的 `pipe::Processes`，并等它问候（`pipe::greeted`）；
  - 挂载 `Session::new(channel)`。loader 会标记这一行，所以会话的句柄在 `session_key(row)`；
  - 记录运行时的状态（见下），并用自己的一个任务等待会话结束；
  - 卸载时，杀掉还没结束的进程并等它结束（`Processes::kill`、`Processes::ended`）；
  - 通过 `pipe::Stdio::Pipe` 把标准错误逐行转给 `tracing`。标准输入和输出为空，与现在相同。
- **对端的插件都是行。** 一个 TypeScript 或 Python 插件就是 `session-node:<plugin>` 或 `py:<plugin>` 这一行。
  `SessionResolver` 通过那个会话解析它，在会话就绪之前，它等 `SessionRows::key(row)`。会话结束后，这些行回到等待。
  它们都由 loader 管理，Dim 不另外记一份。
- **连接自己记录运行时的状态。** 一行自己的状态表达不了"运行时死了"：`apply` 返回后这一行就是 Active，没有任何
  东西能把 Active 的行变成 Failed。rutis 自己的连接行也是让行保持 Active，把连接的状态另外记下来（`rutis-host/dial`
  的链接状态、`SpawnPlugin` 的 `RuntimeStates`）。`RuntimeConnection` 也这样做，用一个 Dim 自己的小值
  `RuntimeStatus`：`starting`、`ready`，或带原因的 `down`。
  - 它从 `apply` 一开始就把这个值提供在一个仅限 Rust、以行名命名的进程级键上（`Key::<RuntimeStatus>::dynamic(row)`），
    值为 `starting`。
  - 会话变成 Active 后，它设为 `ready`。
  - 启动或问候失败时，它设为 `down`，原因是那个错误；会话自己结束时，它设为 `down`，原因是进程是怎么结束的
    （`Processes::status()`，例如 `exited with exit status: 1`）。后者由等待会话结束的那个任务设置。这一行保持
    Active，与 rutis 的连接行一样，所以状态始终读得到；运行时的行回到等待。
  - `RuntimeStatus` 不是 rutis 的 `RuntimeStates`：后者的 `ready` 要传入进程，而进程只有隐藏的
    `SessionHandle::process()` 才拿得到。

  `runtime.list` 的取值保持不变。`off`、`acquiring`、`failed` 是 RuntimeManager 自己的，表示获取运行时的进度。
  启动命令提供之后，RuntimeManager 读取连接的 `RuntimeStatus`：`starting`、`ready`，或带原因的 `down`；这一行还没
  启动、键还不存在时，为 `starting`。`idle` 不再出现。Dim 里没有任何地方持有 `RuntimeStates` 或 `RuntimeHandle`。
- **启动命令。** Dim 保留 rutis-host 自己的启动器为这两个包用的两条命令（`languages/node.rs`、
  `languages/python.rs`），解释器和路径沿用 Dim 现在的配置：
  - Node：在 `@arcships/rutis-runtime` 包里执行 `node --import tsx src/runner.mjs`；
  - Python：在它的项目里执行 `python -m rutis`，把项目和插件包放进 `PYTHONPATH`，并设置 `PYTHONUNBUFFERED=1`、
    `PYTHONDONTWRITEBYTECODE=1`。
- **运行时什么时候启动、重启。** 这一行注入一个携带启动命令的、仅限 Rust 的进程级服务（`nodeRuntime`、
  `pythonRuntime`），所以在命令就绪之前，rutis 会让它一直等着。
  - 使用配置路径时（`--node`、`--js-runtime`、`--python`），App Server 在启动时提供这个命令，这一行是必需行，
    与现在相同。
  - 在 RuntimeManager 下，由 RuntimeManager 在获取到运行时之后提供它，不再把 `LocalRuntime` 挂成自己的子插件。
    这一行是可选行，所以启动从不等待下载。
  - 对处于 `down` 的运行时执行 `runtime.install`，会撤掉这个命令的 `Disposer`，再重新提供它。注入的服务被撤下又
    重新提供时，rutis 会重启这一行，于是启动一个新进程。RuntimeManager 不挂子插件，也不持有任何句柄。
- **标识。** App Server 的根提供 `LocalId("dim-daemon")`。这个内核在每个会话里都用它问候，运行时的会话和
  leaf 的会话（M8）都是。它不挂载 `Session::service()`：那个插件提供名为 `sessions` 的服务，而这个名字已经是
  Dim 的 `AgentDirectory`。
- **解析。** 一个 `SessionResolver::npm(catalog, "session-node")` 服务两个运行时，放在解析链最后。
  - 在 loader 挂载前调用 `attach(&loader)`；loader 运行后对每个运行时行调用 `follow_row`，这样会话一就绪，
    它的行就放行。
  - 不调用 `runtime(handle)`。它只是把一个 `RuntimeHandle` 列进 `Loader::status().runtimes`；RuntimeManager
    读的是连接的 `RuntimeStatus`。
  - Dim 自己的包装改名为 `SnapshotResolver`。它继续检查每一行的 `provides` 快照、加上 `workflowFiles`，
    但不再分流 `py:` 名字。
  - 识别"运行时没在运行"的方式与 loader 相同：没有 schema，且 `meta.schema` 是一段文字。不匹配这段文字的内容。
- **门槛。** 准入原来等 `RuntimeRows`，现在改为等 `SessionRows::key(name)`。删除的有：`NodeRows` 部件；
  `SessionNodeRows` / `SessionPythonRows` 两行；以及 `SessionNodeRuntime` / `SessionPythonRuntime` 两行，由
  `session-node` 和 `py` 两行取代。
- **重新加载。** `plugin.reload` 只调用 `Loader::reload(row)`。没有解析器缓存了，Node 运行时每次解析都会重新
  描述模块。
- **Python。** 系统解释器的检查改为要求 3.12。

### M5. 外语行能拿到 `rutis/loader`；行的记录仍归 PluginManager

0.19.3 的 `LoaderPlugin` 在 `host_key("rutis/loader")` 提供标记服务 `rutis/loader`（`create`、`reload`、`remove`、`status`）。
loader 把这个名字登记在 Dim 交给它的服务目录里，所以外语行注入 `rutis/loader` 就拿到 rutis 的行服务；Dim
自己不登记这个名字。rutis 自己的 `tests/loader_service.rs` 里就有一个 Python 行这样用。

- Dim 没有配置可编辑层：它的两处 reconcile 都不传可编辑层。所以 `create` 和 `remove` 会被拒绝（"this host keeps
  no editable configuration, so rows cannot be added or removed"）；`reload` 和 `status` 可用，而重新加载不改动任何层。
- Dim 按规则 11 接受这一点：已安装的插件是受信代码，而且它们通过 `rutis/loader` 能做的事都不会改动配置。行仍然只由
  PluginManager 根据 `plugins.json` 写入（[PluginManager](plugins/PluginManager.md)）。
- `LoaderPlugin` 是平台插件，所以没有任何会话会把 `rutis/loader` 导出给对端。Dim 没有任何行提供名为 `rutis/loader` 的服务。

### M6. loader 按宿主划分，Agent 建在宿主的 loader 里

一个宿主只有一个 loader。App Server 的 loader 属于它所在的进程，它的 Agent 都放进这个 loader
（`dim-app-server/src/agents.rs:1699`）。另外两种放置方式违反了这条规则：

- `build()` 给 Agent 一个私有 root 和一个自己的 loader。
- `build_in(ctx)` 给每个 Agent 在宿主下面再建一个自己的 loader，并按 Agent 隔离这个 loader 的服务，好让多个
  Agent 共用一个宿主（[single-tree](../../architecture/rust-agent/single-tree.md)、
  [Dim 如何使用 rutis §2](rutis-usage.md#2-插件是一行实例挂载它的部件)"隔离标签"）。

`build_in` 在 0.19 上行不通了。`LoaderPlugin` 还会提供 `LoaderService`，它是 rutis-loader 的私有类型，键无法隔离，
所以同一宿主里的第二个 loader 会因 `ServiceExists` 失败。本次升级恢复这条规则，Agent 只保留一种放置方式：

- **只有一个 API。** `AgentBuilder::build(loader, anchor)` 就是现在的 `build_at`，只是改名为 `build`。loader、Agent
  实例和 root 都归宿主所有，Agent 既不建 root 也不建 loader。App Server 只需要改名。
- **删除的内容。**
  - 会自建 loader 的 `build()`、`build_in`、`AgentHost::Private` 和 `AgentHost::Within`。
  - `own_placement` 按 Agent 对 `Loader`、`PartRegistry` 和 mounter 做的隔离，以及它的标签计数器。
  - `Agent::close` 只关闭 Agent 的 Session。Agent 实例由宿主卸载，root 由宿主关闭。
- **测试和 `xtask` 就是宿主。** 产品代码里没有别的放置方式。`build()` 和 `build_in` 的 159 处调用（§3）全部在测试、示例和
  `xtask` 里。
  - 在 `dim-agent-runtime` 现有的 `testing` feature 下提供一个最小宿主。它像 App Server 一样装配一个 root 和这个 root
    的 loader：builder 的模块和插件行、`agent_row`，以及一个 Agent 实例；再用 `build` 把 Agent 建在这个 loader 里，
    并负责 root 的生命周期。它以宿主的身份接手 `own_placement` 的装配，而不是 Agent 的一种放置方式。
  - 在 Agent 启动前挂到 root 上的宿主插件改为通过 `AgentBuilder::plugin` 成为行，或通过 `process_value`、`session_value`
    作为值提供。测试里是 `RecoveryHosts`、`AimuxPlugin`、`ConfiguredModelsPlugin` 等；`xtask` 里是 `--model-config` 用的
    `AimuxPlugin` 和 `ConfiguredModelsPlugin`，以及本地图用的 `FileStoragePlugin` 和 `GraphPlatform`。
  - `xtask` 的图保留原有的作用域；移动的插件成为宿主 loader 里的行，本地图的进程子图会把它们列为行（§6）。
  - 测试在 Agent 运行期间挂上或卸下的宿主插件（`tests/integration/tests/tool_binding.rs` 里的 `EnableContributor`）
    改为在最小宿主的 loader 里增删行。
  - `agents_of_one_builder_mount_only_their_own_plugins_parts`（`tests/integration/tests/part_registry.rs`）改用两个
    最小宿主，并且仍然检查每个宿主都有自己的部件登记表。
- **文档。**
  - `AgentBuilder` 的文档和 single-tree 只写这一种放置方式。
  - Dim 如何使用 rutis §2 写明测试和 App Server 一样是宿主（"插件在哪里运行"），并删掉"两个共享宿主的 Agent 给各自
    的 loader、部件登记表和 mounter 用同一个标签"那句话（"隔离标签"）。

多个 Agent 仍然可以像在 App Server 里那样共享一个宿主，`agent_row` 已经会隔离进程值。M6 不依赖 0.19，因为
`build_at` 在 0.17.1 上就能用。它先行合入，作为一个单独的 pull request（§5）。

### M7. rutis 的输出不进入 TUI 的终端

- `dimcode` 的根改为 `Ctx::root_with_sink`，配一个什么都不写的 sink。
  - CLI 插件（`DaemonLink`、leaf、`Tui`）的失败，本来就在 CLI 等待它们的地方报告给 CLI。
  - 0.17.1 的默认 sink 本来就会打印它上报的每一种错误，带 `[rutis]` 前缀。0.18 上报得更多：没人等待的启动失败、
    运行时退出、等待超过 10 秒的插件。这些现在都会进入 TUI 的终端。
- App Server 保留它的 `tracing` sink。它的 loader 的配置警告通过 `Loader::on_config_warning` 转到 `tracing`。

### M8. UI leaf 是一个插件，它的生命周期就是一条会话

leaf 保持 [Tui §3.6](../../design/plugins/Tui.md#36-ui-registry-and-ui-leaves) 的目的：daemon 掌握哪些界面活着，
可以关闭或替换它们；生命周期和存活是 rutis 的事实；不另做 UI 控制协议；业务流量仍走协议 v3。

在 0.19 上，daemon 不再把 `Tui` 加载进 leaf。在对端运行插件，只有远程运行和 loader 行两种方式：远程运行要等对端
开始提供行，每次 `ui.open` 都建 loader 行则需要 loader 的层有第二个属主。这两样都不需要：

- daemon 的 `UiLeaf` 是一个插件，与现在相同。
- 它的生命周期就是与 leaf 之间的会话。
- leaf 自己运行 `Tui`。

```text
daemon 根（LocalId dim-daemon）                   dimcode leaf 根
  UiHostPlugin                                     LeafNode
    └─ UiLeaf（ctx.plugin，每次 ui.open 一个）       ├─ LocalId ui-<id>
         ├─ 对 catalog_key() 隔离的 ctx              ├─ 只接受一次连接，pipe::framed
         ├─ 拨号 address ──── 一条 channel ────────► │   握手：对端 id 必须是 dim-daemon
         ├─ 握手：对端 id 必须是 ui-<id>              ├─ 会话 ── 它结束时销毁客户端树
         └─ 会话 ◄═════════════ rutis 会话 ══════════►┘
                                                    └─ Tui（由 leaf 自己挂载，配置来自 ui.open 的回复）
                                                         └─ DaemonLink ── 协议 v3 ──► Rpc（ui.attach）
```

- **`ui.open`。** 与现在相同，leaf 先绑定私有地址，并挂好在那里接受连接的 `LeafNode`，然后才在本地连接上调用
  `ui.open {sessionId, kind, peer, address}`。这样，`ui.open` 还没返回时握手就能完成。
  - daemon 挂载一个 `UiLeaf`。`UiLeaf` 拨号到这个地址、完成问候，并挂载会话。
  - 随后 `ui.open` 回复 `{uiId, generation, token}`。token 从被托管 `Tui` 的加载配置挪到这条回复里，daemon 仍然只
    保存它的 SHA-256 摘要。
  - `ui.open` 只在链接或握手失败时报错：`ui_leaf_failed`，不留下任何登记，与现在相同。它不再等待 `Tui` 加载，
    因为 leaf 的 `Tui` 归 leaf 自己。
- **leaf 侧。** `LeafNode` 提供 `LocalId(ui-<id>)`，不提供插件目录，所以 daemon 不能在 leaf 上运行任何插件。
  - 它像现在的 `Accept` 一样，在私有地址上只接受一次连接（现在是一个返回 `Channel` 的普通函数），然后用
    `Session::handshake` 问候。对端 id 不是 `dim-daemon` 时直接丢弃，channel 随之关闭。
  - 之后它挂载会话，并在 `ui.open` 回复后挂载 `Tui`。`Tui` 从回复里取 `uiId`、token 和 `sessionId`，其余都来自
    leaf 自己的启动参数，与原来被托管的 `Tui` 相同。
  - 会话结束时销毁客户端树，`Tui` 自己的 effect 恢复终端，进程以 TUI 的退出码结束。
  - 在两侧都用会话的结束取代 `LinkState::Stopped`。挂载 `Session::handshake` 返回的 `Greeted` 会得到会话的 fiber，
    它的结束（通过 `FiberView` 看到 Disposed 或 Failed）就是这个事实。不需要轮询或定时器来观察链接。
  - `Tui` 加载失败或出错结束时，同样会销毁客户端树。leaf 以非零码退出，它的会话随之结束，daemon 移除登记。
- **daemon 侧。** `UiLeaf` 只拨号 leaf 给出的私有地址：
  - Unix 上是原生 home 里的 `unix:` socket；
  - Windows 上是 loopback 的 `tcp:` 地址，并且和现在一样，在握手之前先把一次性 token 作为第一行发出。

  握手里的 id 是双方自己声明的，所以在 loopback TCP 上，真正认证 leaf 的是这个 token。`UiLeaf` 从对
  `catalog_key()` 做了隔离的上下文发起问候，所以 leaf 不能在 daemon 上运行任何插件。对端 id 不是 `ui.open` 给出的
  `peer` 时直接丢弃。
- **状态。** leaf 从 `ui.open` 起处于 `starting`，直到它的 `Tui` 调用 `ui.attach {uiId, token}` 之后变为 `online`，
  与现在相同。
  - **时限。** 从回复算起，leaf 在 `ui.open` 现有的启动时限（`START`，20 秒）内没有 attach，就按 `ui.close` 关闭。
    不会留下没有界面的登记。
  - **替换。** `ui.open {replace}` 在新 leaf attach 之后（也就是变为 `online` 时）才关闭旧界面。Tui §3.6 承诺的是
    新界面加载完之后再关闭旧的。如果新 leaf 先结束或先到时限，旧界面保留，认领释放，与现在启动失败时相同。
  - 认领和代次都不变。
- **关闭。** 会话就是 leaf 的生命周期：
  - 用户退出：leaf 销毁客户端树，终端恢复，会话关闭；在 daemon 上，会话结束会销毁 `UiLeaf`，`UiHost` 移除登记。
  - daemon 关闭 leaf（`ui.close`、替换、daemon 关闭）：先发出提示性的 `ui.shutdown` 通知，再销毁 `UiLeaf`。会话随之
    关闭，leaf 照上面的方式销毁自己的树。
  - daemon 被强制退出时，会话同样结束。
- **版本不一致。** 链接协议和 `ui.open` 的回复都变了，所以 `hello` 公布的 feature 从 `ui.leaf` 改为 `ui.leaf.2`。
  无论是旧 CLI 连新 daemon，还是新 CLI 连旧 daemon，Tui §3.6 现有的回退机制都会改为直接运行 TUI。
- **链接现在承载的内容**（对 [Tui §8](../../design/plugins/Tui.md#8-security) 的更新）：
  - **规则。** 会话导出所有能跨语言的进程级名字。[生成的服务目录](../../architecture/rust-agent/generated/catalog.md)
    列出了它们；已安装的外语进程行还会加上它们自己提供的名字。
  - **目前的集合。**
    - `auxiliaryModel`、`managementMethods`、`partRegistry`、`pluginFiles`、`sessionMetadata`、`sessionPermissions`、
      `settings`、`workflowFiles`；
    - `sessions`，它的 `launch`、`input`、`lookup`、`history`、`withdraw`、`stop`、`idle`、`delete` 方法会改动 Session；
    - 默认插件提供的 `scheduleJobs` 和 `workflowRuns`。
  - **因此 leaf 能做的事。** 它可以操作任何 Session、登记管理方法，还可以登记 Session 部件，而这些部件的副本会
    运行在 leaf 里。
  - **为什么接受。** leaf 只能经由本地 `ui.open` 调用方给出的私有地址连通。这个调用方本来就持有 `daemon.sock` 上的
    本地协议 v3 连接，能操作同样的 Session 和管理方法。它的 `Tui` 只使用协议 v3。信任边界仍是本地用户
    （[ADR 003](../adr/003-approval-scope.md)；AGENTS 规则 11）。
  - **Tui §8 的改写。** §8 围绕这个集合重写，去掉"leaf 链接不暴露任何 Session 或管理方法"这句话。
  - 这些服务没有对 leaf 做隔离，尽管 rutis 对"不应让对端看到的服务"的建议是在提供它的行上设置 `isolate`。它们是
    Dim 共享的跨语言服务，仓库自己的 TypeScript 插件就注入它们。隔离它们会把这些插件一起挡在外面，原因有两个：
    - SessionTitle 的 Session 部件注入了 `sessions`、`settings`、`sessionMetadata` 和 `auxiliaryModel`。它是插件
      定义的副本，没有行级 `isolate`（[Dim 如何使用 rutis §3.5](rutis-usage.md#35-各运行时如何解析名字)）。
    - 会话会把服务一视同仁地导出给每个对端，而 Node 运行时也是一个会话。
  - `ui.attach` 用的 token 只出现在本地连接上 `ui.open` 的回复里。
  - 事件是一条总线，监听器名字在两个方向上都会公布。leaf 可以发出 daemon 上任何一个监听器的事件名，不只是
    `ui/*`，daemon 上同名的监听器就会收到。leaf 的代码目前不发出任何事件；它能发出什么，范围同上。
- **将来的 UI 事件。** 重写 Tui §3.6 的"Later"一节：0.19 没有 `export`/`import` 闸门，也没有前缀订阅，所以 leaf
  发出的 `ui/*` 事件会到达 daemon 上所有同名监听器。这些事件的契约不变，仍未实现：serde 载荷，带 `uiId` 和
  `sessionId`，只通过 adopted input 进入。
- **接受的限制。** Tui §10 的两条限制不再存在：已经没有 `peer:` 行，也没有本地传输。
  - 改为接受一条新的限制，而不向 rutis 提需求：会话插件目录的键是 `#[doc(hidden)]`。
  - 之所以接受，是因为 rutis-bridge 的 catalog 模块文档把 `catalog_key()` 写成宿主提供目录的位置。Dim 用的就是这个
    文档写明的位置，而且只用它来让 daemon 的目录对 leaf 隔离。

### M9. 较小的替换

- `plugin_manager.rs`：`waitingFor` 列出 `Loader::waits(entry)` 里的服务。等待运行时会话的行会写出运行时那一行
  （`session-node` 或 `py`），这是 `Loader::waits` 的报法（`WaitReason::OwnRuntime`）。运行时行本身不是插件行，
  所以它们等启动命令这件事不出现在 `plugin.list` 里，而是由 `runtime.list` 显示：RuntimeManager 下载运行时时是
  `acquiring`，下载失败是 `failed`，命令提供之后、这一行加载时是 `starting`。下载卡住时在那里仍然看得到。
- 9 处 `set_overlay` 测试调用改为把夹具行写进一层自己的配置，再用它做 reconcile。
- `tests/integration/tests/extensions.rs` 用一个不走 npm 解析的名字保留"模块无法解析"的用例。裸的 npm 名字
  现在会等待运行时，而不是失败。
- `xtask types` 改用 `rutis_types::Exports`。为新的文件头重新生成一次 `npm/branch-services.d.ts`。
  `AgentErrorThrown` 保持原名：只有名为 `Error` 的错误类型才会改名。
- Dim 手写的 dispatcher 继续用 `session::Error::Value` 拒绝参数和方法。改用 `Binding` 错误码不在本次升级范围内。

### M10. TypeScript 插件测试运行在 `rutis-host` 上

- `runtime/package.json` 加开发依赖 `@arcships/rutis-host`。
  - SDK 依次在 `RUTIS_HOST`、工作目录的 `node_modules/.bin`、`PATH` 里找它。
  - CI 在仓库根目录跑插件测试，后两处都找不到它。
  - 所以 `.github/workflows/ci.yml` 的 Node 绑定一步导出
    `RUTIS_HOST="$PWD/runtime/node_modules/.bin/rutis-host"`。
- SessionTitle 的测试加载的是具名导出 `part`。这个部件移到单独的模块，`index.ts` 和测试都导入它的默认导出。
  其他插件的测试本来就加载默认导出。
- `load(plugin, { config, services })` 的形状不变。现在测试和插件之间传值遵循进程之间的规则，测试观察到的
  事件会在发出方返回之后才到达。在 `emit` 之后立刻读取结果的测试要改为等待。
- 测试要调用插件交给它的函数时，经过运行时去调，而不是自己直接调：测试自己发起的这种调用是同步会话调用，
  插件在其中无法 await 测试的服务（`SyncWaitCycle`）。`runtime/fixtures/plugin-test.ts` 就是那行插件，
  由它在运行时里异步调用；goal 和 session-title 的测试把它和被测插件一起装载。
- 这两个测试等的是插件停下来，而不是自己事件循环转一圈：它们数插件对测试服务的调用，这类调用跨进程。
- goal 和 session-title 两个插件的测试重新在 CI 里运行。0.19.3 修掉了当初把四个测试停下时的那三个测试工具问题
  （[rutis#986](https://github.com/arcships/rutis/issues/986)、
  [#987](https://github.com/arcships/rutis/issues/987)、[#988](https://github.com/arcships/rutis/issues/988)），
  这两个套件随后按上面的跨进程规则改过。session-title 的生成预算测试被跳过：30 秒预算属于插件所在运行时的计时器，
  测试推进不了它。
- scheduler 和 workflow 的测试仍然不在 CI 里：0.19.3 的 TypeScript 测试工具收不到插件定义，而这两个插件一上来就
  注册部件——`partRegistry.registerDefinition` 会失败，报“cannot send a plugin definition: the far end does not
  accept plugin definitions (it did not declare the capability plugins)”。这个能力属于 Rust 一侧，测试进程没有声明它，
  所以测试的任何替身都收不到插件注册的东西，尽管 rutis 自己的设计里测试替身可以收到定义并装载它
  （[plugin values R10](https://github.com/arcships/rutis/blob/main/docs/design-plugin-values-2026-10-25.en.md)）。
  `tsc` 仍检查这两个插件，§6 的 Node 流程测试仍实际加载它们。
- zod 的 `z.toJSONSchema` 结果还带着 `~standard`，0.19 的 `definePlugin` 会把它当成 Standard Schema 拒绝，所以 goal、
  scheduler、workflow 插件把它转成纯 JSON 再传。

## 5. 实施计划

M6 先行合入，作为一个单独的 pull request，基于 0.17.1。它不改变对 rutis 的任何用法，而它的测试迁移量如果并进来，
会淹没升级本身的 diff。之后握手不允许部分升级，所以步骤 1–5 放在一个 pull request 里。每一步是一个独立的提交。

| 步骤 | 工作 | 门槛 |
| --- | --- | --- |
| 0（单独的 pull request，基于 0.17.1） | M6：只保留一个放置 API，提供最小宿主，并把 41 个文件迁到它上面 | `cargo test --workspace --locked`；`xtask check` 和 `graph`，并对照 §6 中步骤 0 的图 diff；§6 中放置的检查 |
| 1 | 固定版本和锁文件（M1），升到 0.19.3；作者指南的固定版本 | `cargo metadata` 能解析；锁文件写的是 0.19.3 |
| 2 | 内核与服务目录：M2、M3 | `dim-app-server` 以下各 crate 和 `tests/integration` 的测试（`extensions.rs` 在步骤 3 跟上）；§6 中 Timeline `before` 的检查 |
| 3 | 运行时：M4、M5、M9 | App Server 和 `tests/integration` 中由 Node、Python 驱动的流程测试 |
| 4 | leaf：M7、M8 | `cargo test -p dimcode --test ui_leaf`；`transport_flow ui_host::` |
| 5 | TypeScript 包与工具：M10、`xtask types`、`.github/workflows/ci.yml` 里的 `RUTIS_HOST` | `runtime/plugins/*` 的插件测试；`runtime/plugin-sdk.test.mjs`；`xtask check` |
| 6 | 文档（§7） | `node scripts/adr-check.mjs`；`node --test scripts/*.test.mjs` |

## 6. 验证

`.github/workflows/ci.yml` 运行的检查，在工作区根目录按它的顺序：

1. `cargo deny --locked check advisories licenses`（dependencies 任务）
2. `cargo build --workspace --locked`、`cargo fmt --all -- --check`、
   `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`
3. `cargo run --locked -p xtask -- check`，然后 `graph`。`npm/branch-services.d.ts` 过期时 `check` 也会失败。提交的图
   来自 mock、本地和 App Server 三个装配，它们都不启动语言运行时，所以 M4 删掉的行从不出现在图里。各个 pull request
   预期的 diff：
   - 步骤 0（M6）：
     - `generated/services.json` 和 `generated/app-server/` 预期不变；
     - `generated/local/` 里，`FileStorage` 和 `HostPlatform` 从 root 下移到宿主 loader 的行里；
     - 插件页面里的生成块随之重新生成。
   - 升级：只有 rutis 0.19 自己新增的 fiber。

   每个 pull request 都记录实际测得的 diff，并对照这份清单解释每一处变化。
4. `node --test scripts/*.test.mjs`
5. 实际的 CLI profile：`cargo test --locked -p dimcode --test cli_flow -- --ignored`，带 `DIM_TEST_APP_SERVER` 和
   `DIM_TEST_NPM_WRAPPER`
6. 公开的 Node 绑定：
   - `runtime/plugin-sdk.test.mjs`；
   - goal、session-title 两个插件的测试，scheduler、workflow 的 `tsc` 检查，现在带上 `RUTIS_HOST`（M10）；
   - 由 `runtime/python-requirements.txt` 建立的 Python 环境。
7. 由 Node 和 Python 驱动的 Rust 流程，带 `DIM_TEST_NODE`、`DIM_TEST_JS_RUNTIME`、`DIM_TEST_RUTIS_SDK` 和
   `DIM_TEST_PYTHON`：
   - `cargo test --locked -p dim-app-server --test transport_flow --test mcp_http_flow node_ -- --ignored`；
   - `cargo test --locked -p dim-migration --test migration_flow node_ -- --ignored`；
   - `cargo test --locked -p xtask node_ -- --ignored`。

测试套件必须覆盖的流程检查；只在契约变化处扩展现有测试：

- **`before` 顺序（M2）：** 同一分支上的两个变形按现在的顺序生效并记录各自的步骤；某个变形失败时保留原始 draft；
  另一个分支的变形不运行。
- **运行时的行（M4）：**
  - `session-node` 和 `py` 两行在启动命令提供之前一直等待，RuntimeManager 获取到运行时之后才启动；
  - TypeScript 行和 Python 行都能启动，`runtime.list` 显示它们的运行时为 `ready`；
  - 运行时进程退出后显示 `down` 和退出原因，它的行回到等待；`runtime.install` 启动新进程，这些行重新运行；
  - 启动命令起不来时显示 `down` 和启动错误；
  - 准入会等待运行时的行；
  - `plugin.reload` 能用上改动后的模块。
- **快照行（M4）：** 运行时停着时解析的行，按它的 `provides` 快照被接受。
- **`rutis/loader`（M5）：** 注入 `rutis/loader` 的 TypeScript 行可以 `reload` 某一行、读取 `status`；`create` 和 `remove` 会被拒绝，
  因为 Dim 没有配置可编辑层。
- **放置（M6）：** 迁到最小宿主上的测试保留原有断言。
  - `agents_of_one_builder_mount_only_their_own_plugins_parts` 的用意变了：只剩一个放置 API 之后，登记表属于宿主，
    同一宿主里也不再有两个自行放置的 Agent。它现在检查的是：同一个 builder 构建、放在两个最小宿主里的两个 Agent
    只挂载各自插件的部件。Session 运行期间测试加入、随后又删除的贡献者行，会先进入 Session 的工具，之后再离开。关闭 Agent
  后它的实例留给宿主处理，关闭最小宿主的 root 时实例被卸载。
- **leaf（M8）：** 现有的 `ui_leaf` PTY 流程（启动、`/new`、`/web`、退出码、替换代次、被杀掉的 leaf、
  `ui.close`、daemon 关闭）。新增检查：
  - `ui.open` 的回复带上 token，leaf 的 `Tui` 用它完成 attach，token 错误时返回 `ui_token`；
  - `Tui` 加载失败时，leaf 以非零码退出，登记随之移除；
  - 替换时，在新 leaf attach 之前旧界面一直保留；新 leaf 先结束时，旧界面仍保留；
  - 一直不 attach 的 leaf 在 `START` 之后被关闭；
  - leaf 试图在 daemon 上运行插件时，因为没有安装该插件而失败；
  - 对端用别的 id 问候时，两侧都会拒绝；
  - `hello` 提供 `ui.leaf.2`，找不到它的客户端改为直接运行 TUI。
- **终端（M7）：** daemon 消失时，leaf 在 TUI 恢复终端之前不往终端打印任何内容。
- **负载（#518、#594）：** 分别在升级的基线和升级分支上运行 `cargo run --release --locked -p xtask -- measure`。
  - 在 1、10、100、1000 个 Session 下，记录创建和关闭耗时，以及关闭后的任务数和存活内存。
  - #518 凭这份结果关闭。
  - #594 剩余的创建耗时增长按新的数字重新归因。

## 7. 随升级一并更新的文档

- [Dim 如何使用 rutis](rutis-usage.md)：§1 和 §2（"插件在哪里运行"和"隔离标签"两段，它们描述的是按 Agent 自带的 loader 和 M6 删掉的标签计数器）、§3.4（M3）、§3.5（运行时的行与 `isolate`）、
  §5（"Dim 使用 rutis 0.17.1"）、附录 A（`LocalId`、运行时的绑定）和附录 B（Python 3.12）。
- [Tui](../../design/plugins/Tui.md)：§3.1、§3.6（示意图、Link、Hosted `Tui` 改为 leaf 自己的 `Tui`、Leaving、`ui.open` 的回复、`hello` feature 那段、Later）、§8、§9 和 §10（M8）。
- [RuntimeManager](plugins/RuntimeManager.md) 的 §1–§3 和 §5（M4）：
  - 它提供启动命令（`nodeRuntime`、`pythonRuntime`），不再挂载 `LocalRuntime`；`runtime.install` 会重新提供命令；
  - 它从连接的 `RuntimeStatus` 读取运行时的状态，不持有句柄：§2.4 里"运行时是否在运行是 rutis 的状态
    （`RuntimeHandle::state`）"和 §3.3 里"rutis 的运行时状态"都改为连接的 `RuntimeStatus`，§5 里 `runtime.list` 的取值去掉 `idle`；
  - `session-node` 和 `py` 两行取代 `Runtime#…` 这些名字；
  - Python 版本要求；
  - 英文原文 [RuntimeManager](../../design/plugins/RuntimeManager.md) 和中文版一起改。

  没有客户端单独显示 `idle`：`dimcode runtime list` 原样打印每个取值（`dim-cli/src/lib.rs:690-698`），TUI 也没有
  运行时视图。
- [PluginManager](plugins/PluginManager.md)：`waitingFor` 和重新加载（M9、M4），以及需求一节里对 rutis 0.17.1 的引用。
- [设计总览](overview.md)：
  - 其中"只有进程插件登记管理方法"的管理平面表述：对端也能拿到 `managementMethods`（M8）；
  - 其中带 `export`、`import` 和 `peer:` 行的 rutis 节点链接描述，0.19 已经删掉了这些。
- [Scheduler](plugins/Scheduler.md) 和 [ACP gateway 实现](../../design/acp-gateway-implementation.md)：其中对 rutis 0.17.1 的引用。
  [ACP gateway 验收](../../design/acp-gateway-acceptance.md)记录的是一次基于 0.17.1 的运行，保持原样。
- [Session](../../design/session-typescript.md)、[分支](../../design/branch-typescript.md)和
  [进程](../../design/process-typescript.md)的 TypeScript 设计，[作者指南](../../typescript-plugin-authoring.md)，
  [single-tree](../../architecture/rust-agent/single-tree.md)（也包括 Agent 的放置方式，M6），[lifecycle](../../architecture/rust-agent/lifecycle.md)，
  [插件页面索引](../../architecture/rust-agent/plugins/README.md)和[架构概览](../../architecture/README.md)：
  其中对 0.17 的引用，以及运行时和 leaf 的机制。
- 已有中文版的，同时更新 `docs/zh/` 下的对应文档。`docs/architecture/rust-agent/generated/` 下的生成页面
  只通过 `xtask graph` 更新。

## 8. 风险

| 风险 | 处理 |
| --- | --- |
| 宿主、运行时和 leaf 之间版本不一致（握手会拒绝） | M1 在一个提交里升级所有固定版本；`ui.leaf.2` 让版本不一致的 CLI 和 daemon 改走直接 TUI |
| daemon 的服务变得可以从 leaf 访问 | M8：只用私有地址（Unix socket，或带一次性 token 的 loopback TCP）、隔离会话插件目录、核对 id；Tui §8 写明完整的集合，以及为什么 `isolate` 不适用 |
| 已安装的插件能拿到导出的服务和 `rutis/loader` | 接受：它们是受信代码（规则 11）；没有可编辑层时，`rutis/loader` 不能增删行（M5） |
| leaf 建立了会话，但它的 `Tui` 一直不 attach | 在 `START` 之后关闭；替换时在新界面 attach 之前保留旧界面（M8） |
| 宿主里出现第二个 loader | M6：loader 按宿主划分，Agent 从不自建 loader；App Server 和每个最小宿主各持有一个 |
| 迁移 159 处调用改变了测试或图所检查的内容 | 每个迁出的测试保留原有断言；M6 单独基于 0.17.1 合入，它的 diff 里没有升级本身的改动；§6 写明了它预期的图 diff |
| #645 合入 | 它在本次升级之后合入，用的是 0.19.2 公开的 `Loader::instance_rows`，行为不变，因此与固定版本和本次升级都不冲突；它读取的实例行会随 #684 去掉（§3） |
| rutis 往 TUI 的终端打印内容 | M7 的 sink；rutis-bridge 在会话无法运行插件时自己调用的 `eprintln!`，由终端检查覆盖 |
| 配置不符合 schema 的 TypeScript 插件现在会失败 | 插件测试和 Node 驱动的流程使用随产品发布的配置运行 |
| 不认识的 npm 名字会一直等待，而不是失败 | `RuntimeManager` 的状态会显示这个等待；M9 调整依赖原有失败行为的测试 |
| Dim 保留的启动命令和运行时包脱节 | 它们和这些包一起固定版本（`runtime/package.json`、`runtime/python-requirements.txt`），由 Node 和 Python 驱动的流程测试实际启动两个运行时 |
| Agent 关闭打断 Session 构造时，某个插件的清理失败 | 0.19.3 由取消这个构造的 dispose 或子树 shutdown 返回清理错误，runtime 把它保留在 `Agent::close` 的结果里；取消本身不作为关闭失败报告。`lifecycle_review` 两者都检查 |
| scheduler、workflow 两个插件的测试暂不在 CI 里运行 | `tsc` 和 Node 流程测试仍覆盖这两个插件；在 rutis 的测试工具能收到插件定义之前，测试替身收不到它们注册的那份定义（M10） |

回滚：撤销这次升级改动和固定版本即可。没有任何持久化格式变化；Timeline、存储和线上格式的夹具都不动。

## 9. 向机制所有者提出的需求

一项，已有答复：为 Timeline 的 `before` 链提供同步 waterfall（M2），即
[rutis#946](https://github.com/arcships/rutis/issues/946)，已由 [rutis#947](https://github.com/arcships/rutis/pull/947)
合入，在 0.19.1 发布；本次升级以 0.19.3 为目标。其余决策都按 rutis 的现状使用它。接受一条限制而不提需求（M8）。`LoaderService` 的键无法隔离，
这正好符合 loader 按宿主划分（M6）。

## 10. 规则检查

本次升级不新增部件、事件或事实。

| 规则 | 回答 |
| --- | --- |
| 原则 §4 规则 1–3、5–7、9 | 不变：命令、交接、`before` 契约、批次、上下文路径和事实都不变；M2 保留 `before` 的契约、顺序和失败规则 |
| AGENTS 规则 4（状态归属） | 行仍只由 PluginManager 写入：没有可编辑层时，插件可以通过 `rutis/loader` 重新加载某一行，但不能增删行（M5）。运行时的状态是连接插件的 `RuntimeStatus`，由这个插件写、RuntimeManager 读；RuntimeManager 自己只保留获取运行时的状态（M4） |
| §4 规则 8（基础设施默认不变） | 改动来自依赖升级，限定在 runtime crate、App Server 和 CLI 之内；唯一向 rutis 提出的需求是 M2 的，已由 0.19.1 中的 rutis#947 答复（§9） |
| AGENTS 规则 11（受信插件，明确的边界） | M8 写明 leaf 链接暴露了什么，信任边界保持在本地用户；审批和权限都不变 |
| AGENTS 规则 6（冻结的夹具） | 不动 `crates/dim-agent-types/tests/fixtures/` |
| ADR 002 D1–D5 | 作用域放置、接线、依赖和配置都不变：运行时仍是进程部件，leaf 仍属于 CLI 进程。M6 把 Agent 的放置 API 改成唯一的 `build(loader, anchor)`，遵循 [ADR 004](../../adr/004-loader-plugin-management.md) 的"不引入第二个 loader" |
| ADR 006 D2 | Dim 直接使用 rutis 的会话和解析器；用 rutis 写明的"宿主自己的连接"形式拉起运行时；不为被删的 API 加包装 |

## 11. 消融检查

- 直接删除、不做替代：`NodeRows` 及其两行；解析器缓存和 `invalidate`；`share_registered_only`；`TuiRow`、
  daemon 在 leaf 上运行 `Tui`、leaf 的目录以及 daemon 为它设的卸载时限；leaf 和 daemon 各自的 `Transport` 实现、`StaticIdentity`，以及 `UiHostPlugin` 的传输和身份；
  会自建 loader 的 `build()`、`build_in`、`AgentHost::Private` 和 `Within`，以及 `own_placement` 按 Agent 的隔离及其
  标签计数器（M6）。
- `own_placement` 的装配移到最小宿主里，供测试和 `xtask` 使用，它们是唯一需要最小宿主的调用方。它在那里是一个宿主，而不是
  Agent 的一种放置方式（M6）。
- 不在 CI 里加 `git diff --exit-code -- npm/branch-services.d.ts`：`xtask check` 已经会在这个文件过期时失败。
- Dim 只为 rutis 不做的事保留自己的解析器包装：`provides` 快照检查和 `workflowFiles`。
- 没有兼容层，也不引入宿主应用。运行时是一个连接行，这是 rutis 为"宿主自己的连接"写明的形式，用的是
  rutis-bridge 的公开零件。唯一的运行时状态是连接插件自己的 `RuntimeStatus`。不链接 `rutis-host` crate，
  也不照抄它的 `SpawnPlugin` 和 `RuntimeStates`。`RuntimeManager` 不再挂载子插件：它只提供启动命令，由 rutis 的
  门控启动这一行。
- `ui.leaf.2` 复用现有的 feature 回退机制，不新增版本检查。
