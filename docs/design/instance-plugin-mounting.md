# 实例挂载插件模型

Status: Proposed, targets rutis 0.16.0. Tracking issue: #370. Implementation: #371–#376.

本文定义 Dim 的插件契约如何从 process/session/branch 类型层级迁移到 rutis 原生的实例 + 服务模型。运行时的实例包含树保留，但它只用于生命周期和服务可见性，不是事件投递边界；插件作者不实现 Dim 的 `Capability` trait、`InSession`、`InBranch`，也不声明自己属于哪一层。

> 本文取代 [rutis 0.15 升级设计](rutis-0.15-upgrade.md) 中的事件 scope 假设。0.16 保留 0.15.1 的平总线；0.15 设计的 §4、D2/D3/D4 不能按原文实施。

本文是目标设计；当前的生成图见
[`docs/architecture/rust-agent/generated/`](../architecture/rust-agent/generated/)。
手写图只描述目标，当前实现以生成产物为准。

## 问题

当前 Dim 在 rutis 之上又包了一层：

- `Capability` trait 要求 `keys()`；
- `SessionScope`/`BranchScope` 把实例 id 包成两个类型；
- `InSession<T>`/`InBranch<T>` 把父/当前实例编码成依赖类型；
- `AgentBuilder::session_plugin`/`timeline_plugin` 只接受 `Capability`。

但 rutis 已经有跨语言挂载、命名服务、导出的 TypeScript 声明和实例解析：

- `rutis-loader` 用行装载 Node/Python 插件；
- TypeScript/Python 用 `definePlugin({ inject, provides })` 声明依赖和服务；
- `Exports::in_instances(group)` 把 Rust 服务导出到每个实例，行按名字解析到最近的 enclosing group instance；
- `host_key_in(name, instance)` 和 `register_shared_instance` 已经在 Dim 的 TypeScript binding 里使用。

所以不需要再造一套 manifest、role 字段或 `ServiceBindings` 容器。

## 目标

- 保留 process root、Session 实例、Branch 实例的包含树。
- 跨语言插件直接走 rutis loader row + `definePlugin` + 命名服务。
- Dim 只负责：导出哪些服务、每个服务在哪个实例组、哪些服务只允许 Capability 使用、事件数据契约。
- Rust 侧可以有 `Service<T>`/宏做类型检查，但它是 binding，不是跨语言契约。
- 多 Session、多 Branch、Timeline/Dispatcher 的顺序和批次语义不变；事件路由按 0.16 的平总线显式命名。
- 公开 Session/Branch 门面的 `PublicService` 限制不变。

## 非目标

- 不合并 33 个业务插件。
- 不删除实例树，不改变 Session/Branch 关闭顺序。
- 不改变 Timeline、Dispatcher、Loop 的提交顺序、批次或分发顺序。
- 不新增 Dim 私有的插件 manifest、role 字段或服务注册表。
- 0.16 升级是前置条件，不在同一 PR 内完成；0.15 升级设计不再实施。

## Rutis 0.16 评估

**结论：实例挂载方向不变，升级基线改为 0.16.0。实例继续划定生命周期和服务所有权，但不再划定事件可见性。0.15 设计的 scope 事件模型作废。**

### 0.16 改了什么

- **一条平总线。** 一个 root 上同名事件的所有监听器都收到事件，不论监听器在哪个实例、runtime 或 node。scope 只是 `ListenerInfo` 的描述，供 filter 使用，不是投递规则。
- `Dispatch::scope`、`scope_contains`、Node/Bun `inScope`、Python/Kotlin/Swift `within`、Go `events.Within`、Node `h.at(scope)`、Python `Bus.emitter(scope)` 删除；`Ctx::scope()`、`ListenerInfo::scope` 和 `ctx.events().filter(..)` 保留。
- 事件仍按名字派发，`Event` 仍要求 serde；线上格式不变。0.15.1 和 0.16 可互通，但 0.15.1 端仍按自己的 scope 过滤，必须两端都升级才是平总线。
- 0.16 不是只改 scope：它继承 0.13/0.15 的统一事件、serde 要求、监听器签名、`Process::emit`/`.emits` 移除和 `#[rutis::service]`。0.16 升级设计必须保留 0.15 文档中仍然有效的迁移项，不能只删掉 scope 部分。
- 0.12.1 与 0.13+ 不兼容，host、runtime、Node SDK 和锁文件必须一起升级；不能先升一半。
- 服务和实例模型没有变：`TypeKey::instance`、`ServiceCatalog`、`Exports::in_instances`、`host_key_in`、`register_shared_instance`、loader 的 `inject`/`isolate` 和 instanced group 都在。0.15 新增的 `#[rutis::service]` 仍可用，是 `Exports` 之外的另一种导出方式。
- `Process::emit`、`RuntimePlugin::emits`、`Mount.emits` 已移除；跨语言事件直接走 `ctx.on`/`ctx.emit`。`Mount.ctx` 和 `load_row(scope)` 仍在，但 scope 只描述监听器。
- 卸载会等待正在运行的监听器，最多到 `RUTIS_SHUTDOWN_TIMEOUT`，并已触发其 `ctx.cancelled()`。
- `definePlugin` 的 `inject`/`provides` 不变；`provides` 可省略，`ctx.provide` 会自行公告。row 的 `inject` 是 host 可见的装载声明，插件自己的 `inject` 必须与它一致或为其子集。

### 对原设计的逐项结论

| 设计点 | 0.15 假设 | 0.16 事实 | 结论 |
| --- | --- | --- | --- |
| 实例树 | 生命周期 + 事件可见性 | 生命周期 + 服务可见性；事件不按实例投递 | 保留实例树，删除“实例隔离事件”的表述 |
| Capability 角色 | 直接读写 Timeline 的 part | 同样成立；但事件订阅不再由实例边界限制 | 角色保留，边界改为“服务 + 私有事件名 + 审计” |
| 服务解析 | `host_key_in`/`Exports` | 不变，`ServiceCatalog`/`Exports::in_instances` 都在 | 原方案成立 |
| 跨语言声明 | `definePlugin` + loader row | 不变；`provides` 仍可选 | 原方案成立 |
| 跨语言事件 | 依赖 scope 或 relay | 平总线，任意 row 可按名字订阅 | 删除 `Process::emit` relay，直接 `ctx.on` |
| `timeline.committed` | 分支实例键 | 静态名会让所有分支的 Dispatcher 互相收到 | 改成分支私有的动态名，由 Dispatcher 管理 |
| `timeline.before` | 分支实例键 | 静态名会让所有分支的 before 中间件互相拦截 | 改成分支私有的动态名；这是正确性要求 |
| `tools.changed` | Session 实例键 | 静态名会让所有 Session 的 branch 监听器互相唤醒 | 继续用 `ToolRegistry::sources_state` watch，或按 Session 命名 |
| `skills.catalog_changed`（key `skills/catalog`） | 根事件向下不可达 | 平总线让所有 Session row 都能收到 | 保留事件；#362 的 watch 不再需要 |
| host→row relay | 需要 agent scope dispatch | 平总线让 row 直接收到根事件 | 删除 relay 和 `.emits`；SDK 需要时只保留 payload 适配 |
| `SessionRestored` | 带 `Session` handle | 事件必须 serde | 改成 id，再由 `AgentIndex` 解析 |
| `BeforeCommit` | 带本地 trace steps | 事件必须 serde | `#[serde(skip)]` 本地字段，只在本分支使用 |
| Capability-only 校验 | loader 看 `inject` | 平总线不提供事件 ACL | 服务注入可校验；事件监听只能靠私有名 + 审计 |
| 升级顺序 | 先 0.15，再实例挂载 | 0.15 scope 设计已作废 | 先 0.16，再做实例挂载 |

### 修订后的规则

1. **实例是生命周期和服务边界，不是事件边界。** Session/Branch 实例继续拥有自己的服务、任务和清理；事件一律按名字在 root 总线上投递。需要按分支隔离的流，必须使用分支唯一的名字，不能依赖 scope。

2. **Timeline 的 committed/before 是分支私有流。** 内部名使用动态 `timeline/{instance}/committed` 和 `timeline/{instance}/before`；`Dispatcher` 和 `Timeline` 自己构造并管理这两个名字。Capability part 通过 `Dispatcher::listen`/`Timeline::before` 使用，不直接拼事件名。这样 `inject dispatcher` 仍然是“观察 Timeline”的唯一受支持入口。

3. **公开事件保留稳定名字。** `session/{session_id}`、`settings/{source}`、`settings/projects`、`skills/catalog` 等已经是按实例或领域命名的；平总线下继续使用。名字是运行时路由，不持久化，`InstanceId` 不进入存储或用户可见身份。

4. **Capability 判定仍看注入的服务。** row 的 `inject` 是 host 可见的声明；TS 插件自己的 `inject` 必须是它的子集。loader/host 拒绝 contribution row 注入 `timeline`、`dispatcher`、`treeCommands`、`approval`、`toolBinding`、`configureControl`、`inputQueue` 等 capability service。Rust 侧以 `Deps` 和代码评审为准。

5. **事件监听不是权限边界。** 平总线下任何受信插件都能按名字监听；私有名和 reserved prefix 只能减少误用，不能阻止恶意插件。Capability 是设计/审计角色；真隔离需要进程边界，这与 [ADR 003](../adr/003-approval-scope.md) 的受信插件边界一致。不要把事件名当授权，也不要把 scope 当隔离。

6. **服务目录只共享显式名字。** 运行时 `ServiceCatalog` 使用 `share_registered_only()`，通过 `Exports`/`register_shared_instance`/`register_shared` 注册允许跨语言的名字。否则默认 `share_by_name` 会把所有名字暴露给 row，capability-only 校验就失去意义。

7. **跨语言事件直接走 rutis。** 删除 `Process::emit` 和 `.emits`；Session/Settings row 用 `ctx.on` 订阅自己的名字。保留 `session.event` 的 SDK 形状时，只在 host 侧做字段适配，不新增事件总线。

8. **Rust 的 `Service<T>` 仍只是 binding。** `Exports::in_instances` 负责显式分组导出；`#[rutis::service]` 可作为后续替代。二者都只是 rutis 的名字/实例机制，不是 Dim 的第二套契约。

### 跨语言服务面的边界

“Capability part 拿到 Timeline/Dispatcher/TreeCommands”不等于这些 Rust 类型的所有方法都能跨语言。`Exports` 只生成可跨语言的签名；当前 `Dispatcher::subscribe`、`Timeline::before`、部分 `TreeCommands` 方法因为 `impl Into<String>`、回调、`Branch` 或 `AgentError` 不能导出（`xtask check` 已有警告）。

- 跨语言观察 committed 事件使用 `Dispatcher::listen`（已有，参数是 `String` 和可跨语言的 handler）；不要要求它调用 `subscribe`。
- 跨语言需要 before 中间件时，新增一个窄的、参数和返回值都可 serde 的 `listen_before` 方法；否则 before 仅限 Rust Capability。
- 跨语言写 input 前先审计 `TreeCommands` 的导出面；不能把一个 Rust 句柄或 `CancellationToken` 塞进服务方法。
- 导出方法返回 `AgentError`、`watch::Receiver`、`Arc<dyn Fn>` 等类型时，用 `Exports::declared` 明确 TypeScript 形态，或改成简单数据；不要新增 Dim 序列化包装层。

### 修订后的升级顺序

1. **0.16 升级（独立前置 PR）。** 版本 pin、serde 事件、平总线、动态分支名、删除 relay、Node SDK/声明、测试。0.15 设计不再实施。
2. **实例挂载设计/ADR。** 删除 `Capability` trait、`InSession`/`InBranch`、`SessionScope`/`BranchScope`；保留一个由 Session/Branch 工厂提供实例键的 `Service<T>` binding。
3. **服务导出与目录。** 补齐 Session/Branch 服务的 `Exports::in_instances`，生成 TS 声明，启用 `share_registered_only`。
4. **Capability-only 校验。** row `inject` 分类、reserved event prefix 审计、contribution 拒绝。
5. **raw Session/Branch rows。** 打开 Node/Python 行，验证无感挂载和直接事件订阅。
6. **清理与文档。** 删除旧 API、更新生成图和架构文档。

0.15 的 `SkillsCatalog::changes()`（#362）不再需要；`tools.changed` 的 watch 是可选的隔离优化，不是升级阻塞项。

## Capability 是什么

Capability 是 part 的角色，不是 trait，也不是 scope：

**直接读或写 branch Timeline 的 part 是 Capability。**

> 待确认：本文删除的是 `Capability` trait + scope 这层，不是 Capability 概念。若维护者要求一个运行时标记来强制 capability-only 服务注入，可以保留最小的 `Capability` marker/wrapper（工厂提供 keys），但不得恢复 `SessionScope`/`BranchScope`/`InSession`/`InBranch`。不保留它时，capability-only 只能靠 row/`Deps` 校验和审计。

包括：

- 注入 `timeline`，通过它写事件；
- 注入 `dispatcher`，消费事件（纯观察者也算）；
- 注入 `treeCommands`，向 branch 写 input；
- 注入其他写能力服务（如 `approval`、`toolBinding`、`configureControl`、`inputQueue`）。

不是 Capability：

- 只提供 tool、session method、prompt source、host service；
- 只观察 Session broadcast 或 rutis 服务事件，不直接碰 branch Timeline；
- RPC、Transport、UI、FileStorage、HostPlatform、Provider 连接管理等宿主部件。

**不需要单独声明 role。** 一个 part 的 `inject` 里出现 capability service，它就是 Capability；没有就是 contribution。loader/宿主可以校验并拒绝 contribution 注入 capability service。这样跨语言只写 rutis 已有的 `inject`，没有额外声明。注意：平总线让任何受信插件都能按名字监听，所以这只能是设计/审计规则，不是事件 ACL；真正的隔离需要进程边界。

角色按 part 算。一个 feature 可以全是 Capability part，也可以混合：

- Goal 的 branch part 注入 `dispatcher`/`timeline`，是 Capability；
- Goal 的 Session part 注入 `treeCommands` 写 input，也是 Capability；
- Questions、Attachments 不注入 Timeline 服务，是 contribution。

## 挂载与声明

### TypeScript / Python

```ts
export default definePlugin({
  name: 'GoalBranch',
  inject: ['dispatcher', 'timeline', 'treeCommands'],
  provides: {},
  apply(ctx) { /* ... */ },
});
```

```ts
export default definePlugin({
  name: 'GoalSession',
  inject: ['treeCommands'],
  provides: {
    'toolContribution:goal_update': { declaration: 'sync', execute: 'async' },
    'sessionMethods:session.goal': { describe: 'async', call: 'async' },
  },
  apply(ctx) { /* ... */ },
});
```

```ts
export default definePlugin({
  name: 'Questions',
  inject: [],
  provides: {
    'toolContribution:ask_user': { declaration: 'sync', execute: 'async' },
    'sessionMethods:session.question': { describe: 'async', call: 'async' },
  },
  apply(ctx) { /* ... */ },
});
```

没有 `role` 字段，没有 `ServiceBindings`，没有 Dim 类型。loader row 就是挂载声明；`inject`/`provides` 就是依赖和服务声明。

### Rust

Rust 插件仍实现 `rutis::Plugin` / `rutis::TypedPlugin`。`Service<T>` 只是可选的 `Deps` 适配器，让 Rust 作者不用手写 key；它不出现在跨语言契约里。宏可以生成它：

```rust
#[dim::deps]
struct GoalBranchDeps {
    #[inject] dispatcher: Arc<Dispatcher>,
    #[inject] timeline: Arc<Timeline>,
    #[inject] tree: Arc<TreeCommands>,
}
```

`Service<T>` 的实例键由 Session/Branch 工厂提供（`Build::instance` 或 `InstanceId`），不是由插件实现 `Capability::keys`。它只是一层 Rust binding；跨语言插件永远不会看到这个类型。

### 服务解析

继续使用 rutis 的 `ServiceCatalog` 和 `Exports`：

- Rust 服务在 `TypeKey::of::<T>()` 或 `TypeKey::instance::<T>(id)`；
- 对外导出的服务在 `host_key(name)` 或 `host_key_in(name, instance)`；
- `Exports::in_instances(SESSION_GROUP)` / `in_instances(BRANCH_GROUP)` 让一行按名字解析到最近的 enclosing group instance；
- TypeScript 提供的服务通过 `register_shared_instance(service, SESSION_GROUP/BRANCH_GROUP)` 进入同一目录。

这就是“无感挂载和声明”：插件只写名字，rutis 负责实例解析和跨语言 dispatch。Dim 不再加第二套 resolver。

## Capability-only 服务

宿主把服务分成两类：

```text
contribution services: toolContribution, sessionMethods, prompt sources, host reads ...
capability services:   timeline, dispatcher, treeCommands, approval, toolBinding,
                       configureControl, inputQueue, ...
```

- 运行时 catalog 只共享显式注册的名字（`share_registered_only`）；capability services 由 host 注册，但只允许 `inject` 中包含它们的行装载；
- contribution 行如果注入 capability service，loader 直接拒绝或明确标成 Capability；
- 纯观察者注入 `dispatcher`，所以它是 Capability；
- 通过 `treeCommands` 写 input 的 Session part 注入 `treeCommands`，所以它是 Capability；
- 只提供 tool/method 的 part 不注入这些服务，保持 contribution。

这条规则由 loader/host 校验，不靠插件自报 role。

## 事件声明

`events.writes/consumes` 仍可作为设计文档和评审信息，但不是 rutis 运行时注册表。ADR 001 的决定不变：Timeline 只检查信封和保留域，不靠事件声明做注册。Capability 的身份由 `inject` 决定，不靠事件列表。

平总线下，事件名是跨语言契约。`timeline/*` 是分支私有前缀，只有 `Dispatcher`/`Timeline` 生成和使用；`session/*`、`settings/*`、`skills/*` 是公开名字。新增按实例隔离的流必须用实例唯一的名字，或由所有者服务提供订阅方法，不能退回 scope 过滤。

## 前后对比

| 方面 | 当前 | 目标 |
| --- | --- | --- |
| 插件契约 | `TypedPlugin + Capability` | `Plugin` / `TypedPlugin`；无 Dim trait |
| Capability 定义 | Rust trait + Session/Branch scope | 直接读/写 Timeline 的 part；由 `inject` 推导 |
| 作用域选择 | `SessionScope`/`BranchScope` | loader row 的实例组 |
| 依赖 | `InBranch<T>` / `InSession<T>` / `Arc<T>` | rutis 命名服务；Rust `Service<T>` 只是 binding |
| 实例解析 | Dim 自己维护 scope key | `host_key_in` / `Exports::in_instances` / `ServiceCatalog` |
| 跨语言 | 部分命名服务 + host relay | 原生 `definePlugin` + loader row + 直接 `ctx.on` |
| 事件投递 | 实例键/scope 过滤 | 0.16 平总线；按实例唯一名字或所有者服务的订阅方法 |
| Timeline 权限 | 任何 Session/Branch Capability 都可能拿到 | 只有注入 capability service 的 part 拿到服务；事件监听不是 ACL |
| 注册 | `session_plugin` / `timeline_plugin` | loader row / raw rutis plugin |
| 公开门面 | `SessionService`/`BranchService` + `PublicService` | 只保留 `PublicService` 限制 |

详细任务见 #370–#376。0.16 升级本身另开实现 PR；本文只定义升级后的插件契约。

## Rule check

| 规则 | 应用 |
| --- | --- |
| §1 Capability 有界 | Capability 只指直接读写 Timeline 的 part；contribution/host 不是 Capability；不新增 wiring module。 |
| §4.1 改状态写指令事件 | 本设计不改业务事件。 |
| §4.2 对象交接不可撤回 | 不变。 |
| §4.3 before 回调 | 不变。 |
| §4.4 核心机制不带业务含义 | Capability-only 服务表是通用机制，不含业务字段。 |
| §4.5 原子批次 | 不变。 |
| §4.6 上下文路径 | 不变。 |
| §4.7 插件间公开交互 | 只读命名服务，不读私有状态。 |
| §4.8 基础设施默认不变 | 这是插件装配基础设施自身的契约：删除 Dim 层类型，复用 rutis loader/export/catalog；不新增第二套 resolver。0.16 平总线是外部依赖的既定行为，设计用它求解，不向 rutis 提新需求。 |
| §4.9 只有后续决定需要的事实进 Timeline | 不新增 Timeline 事实；`InstanceId` 只用于运行时路由，不持久化。 |
| ADR 002 D1 | 实例树仍是状态和服务所有权；事件不按实例隔离。 |
| ADR 002 D2 | 不新增 wiring module；复用 rutis loader/export。 |
| ADR 002 D3 | 依赖从扩展指向核心；命名服务由宿主导出。 |
| ADR 002 D4 | 结构配置仍在 loader row。 |
| ADR 002 D5 | 跨语言契约只有名字/数据/事件；Rust `Service<T>` 是 binding，事件 payload 必须 serde。 |

## Requirements raised to mechanism owners

- **Runtime/App Server**：先完成 0.16 升级（平总线、动态分支名、serde、删除 relay），再补齐 Session/Branch 服务的 `Exports::in_instances`、TypeScript 声明、`share_registered_only` 和 capability-service 校验；删除旧层 API。跟踪 #371, #375.
- **Loader**：支持 Session/Branch raw rows 和 Node/Python runtime；跟踪 #376.
- **rutis**：不新增需求；使用现有 `ServiceCatalog`、`Exports`、`host_key_in`、`definePlugin`、`ctx.events().filter` 和 0.16 平总线。
- **文档**：更新 ADR 002、`single-tree.md`、`principles.md`、TypeScript authoring guide，明确“实例不是事件边界”“Capability 由注入的服务推导”“分支私有事件名”。0.15 升级文档标记为历史。

## Decisions

- 实例挂载方向不变；升级基线改为 rutis 0.16。
- Capability 不是 trait，也不是 scope；它是“注入 Timeline/写能力服务的 part”。
- 不新增 Dim 私有 manifest、role 字段、`ServiceBindings` 或第二套服务目录；复用 rutis loader/ServiceCatalog/Exports。
- 跨语言只写 `inject`/`provides`；Rust 的 `Service<T>` 是可选 binding。
- 纯 Timeline 观察者算 Capability；通过 `TreeCommands` 写 input 的 part 也算 Capability。
- 只观察 Session broadcast/rutis 服务事件、不碰 branch Timeline 的 part 不算 Capability。
- 0.16 平总线：实例不是事件边界；`timeline.committed`/`timeline.before` 使用分支私有动态名，由 Dispatcher/Timeline 管理。
- 事件监听不是授权；capability-only 服务校验靠 row `inject`，事件误用靠私有名和审计。
- 0.15 升级设计不再实施；#362 的 `SkillsCatalog` watch 不再需要。
- 0.16 升级是实例挂载的前置 PR，不与插件合并同批进行。
