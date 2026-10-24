# 实例挂载插件模型

Status: Proposed. Tracking issue: #370. Implementation: #371–#376.

本文定义 Dim 的插件契约如何从 process/session/branch 类型层级迁移到 rutis 原生的实例 + 服务模型。运行时的实例包含树保留，但它只用于生命周期和可见性；插件作者不实现 Dim 的 `Capability` trait、`InSession`、`InBranch`，也不声明自己属于哪一层。

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
- 多 Session、多 Branch、Timeline/Dispatcher 语义不变。
- 公开 Session/Branch 门面的 `PublicService` 限制不变。

## 非目标

- 不合并 33 个业务插件。
- 不删除实例树，不改变 Session/Branch 关闭顺序。
- 不改变 Timeline、Dispatcher、Loop 的事件顺序或批次语义。
- 不新增 Dim 私有的插件 manifest、role 字段或服务注册表。
- 不和 rutis 0.15 升级一起做。

## Capability 是什么

Capability 是 part 的角色，不是 trait，也不是 scope：

**直接读或写 branch Timeline 的 part 是 Capability。**

包括：

- 注入 `timeline`，通过它写事件；
- 注入 `dispatcher`，消费事件（纯观察者也算）；
- 注入 `treeCommands`，向 branch 写 input；
- 注入其他写能力服务（如 `approval`、`toolBinding`、`configureControl`、`inputQueue`）。

不是 Capability：

- 只提供 tool、session method、prompt source、host service；
- 只观察 Session broadcast 或 rutis 服务事件，不直接碰 branch Timeline；
- RPC、Transport、UI、FileStorage、HostPlatform、Provider 连接管理等宿主部件。

**不需要单独声明 role。** 一个 part 的 `inject` 里出现 capability service，它就是 Capability；没有就是 contribution。loader/宿主可以校验并拒绝 contribution 注入 capability service。这样跨语言只写 rutis 已有的 `inject`，没有额外声明。

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

- capability services 只导出给 `inject` 中包含它们的行；
- contribution 行如果注入 capability service，loader 直接拒绝或明确标成 Capability；
- 纯观察者注入 `dispatcher`，所以它是 Capability；
- 通过 `treeCommands` 写 input 的 Session part 注入 `treeCommands`，所以它是 Capability；
- 只提供 tool/method 的 part 不注入这些服务，保持 contribution。

这条规则由 loader/host 校验，不靠插件自报 role。

## 事件声明

`events.writes/consumes` 仍可作为设计文档和评审信息，但不是 rutis 运行时注册表。ADR 001 的决定不变：Timeline 只检查信封和保留域，不靠事件声明做注册。Capability 的身份由 `inject` 决定，不靠事件列表。

## 前后对比

| 方面 | 当前 | 目标 |
| --- | --- | --- |
| 插件契约 | `TypedPlugin + Capability` | `Plugin` / `TypedPlugin`；无 Dim trait |
| Capability 定义 | Rust trait + Session/Branch scope | 直接读/写 Timeline 的 part；由 `inject` 推导 |
| 作用域选择 | `SessionScope`/`BranchScope` | loader row 的实例组 |
| 依赖 | `InBranch<T>` / `InSession<T>` / `Arc<T>` | rutis 命名服务；Rust `Service<T>` 只是 binding |
| 实例解析 | Dim 自己维护 scope key | `host_key_in` / `Exports::in_instances` / `ServiceCatalog` |
| 跨语言 | 部分命名服务 | 原生 `definePlugin` + loader row + 自动声明 |
| Timeline 权限 | 任何 Session/Branch Capability 都可能拿到 | 只有注入 capability service 的 part 拿到 |
| 注册 | `session_plugin` / `timeline_plugin` | loader row / raw rutis plugin |
| 公开门面 | `SessionService`/`BranchService` + `PublicService` | 只保留 `PublicService` 限制 |

## 迁移顺序

1. 设计文档与 ADR；明确 Capability 由注入的 Timeline 服务推导，不新增 Dim manifest/role。
2. 补齐服务导出：把 `timeline`、`dispatcher`、`treeCommands`、`approval`、`toolBinding` 等按 Session/Branch group 导出，生成 TypeScript 声明。
3. 在 loader/catalog 加 capability-service 校验；contribution 注入 capability service 时拒绝或标成 Capability。
4. 迁移现有 Rust 插件：删除 `Capability` trait/scope 依赖，改用命名服务或 Rust `Service<T>` binding。
5. 删除 `InSession`/`InBranch`、`SessionScope`/`BranchScope`、`SessionService`/`BranchService`，更新文档和生成图。
6. 打开 Node/Python 的 raw Session/Branch rows，验证无感挂载。

详细任务见 #370–#376。

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
| §4.8 基础设施默认不变 | 这是插件装配基础设施自身的契约：删除 Dim 层类型，复用 rutis loader/export/catalog；不新增第二套 resolver。 |
| §4.9 只有后续决定需要的事实进 Timeline | 不新增 Timeline 事实；`InstanceId` 不持久化。 |
| ADR 002 D1 | 实例树仍是状态所有权；Capability 由 Timeline 访问推导，不由 scope 类型决定。 |
| ADR 002 D2 | 不新增 wiring module；复用 rutis loader/export。 |
| ADR 002 D3 | 依赖从扩展指向核心；命名服务由宿主导出。 |
| ADR 002 D4 | 结构配置仍在 loader row。 |
| ADR 002 D5 | 跨语言契约只有名字/数据/事件；Rust `Service<T>` 是 binding。 |

## Requirements raised to mechanism owners

- **Runtime/App Server**：补齐 Session/Branch 服务的 `Exports::in_instances`，生成 TypeScript 声明，加 capability-service 校验；删除旧层 API。跟踪 #371, #375.
- **Loader**：支持 Session/Branch raw rows 和 Node/Python runtime；跟踪 #376.
- **rutis**：不新增需求；使用现有 `ServiceCatalog`、`Exports`、`host_key_in`、`definePlugin` 和 runtime resolvers。
- **文档**：更新 ADR 002、`single-tree.md`、`principles.md` 和 TypeScript authoring guide，明确 Capability 由注入的 Timeline 服务推导。

## Decisions

- Capability 不是 trait，也不是 scope；它是“注入 Timeline/写能力服务的 part”。
- 不新增 Dim 私有 manifest、role 字段、`ServiceBindings` 或第二套服务目录；复用 rutis loader/ServiceCatalog/Exports。
- 跨语言只写 `inject`/`provides`；Rust 的 `Service<T>` 是可选 binding。
- 纯 Timeline 观察者算 Capability；通过 `TreeCommands` 写 input 的 part 也算 Capability。
- 只观察 Session broadcast/rutis 服务事件、不碰 branch Timeline 的 part 不算 Capability。
- 不与插件合并、rutis 0.15 升级同批进行。
