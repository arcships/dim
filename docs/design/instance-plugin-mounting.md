# 实例挂载插件模型

Status: Proposed. Tracking issue: #370. Implementation: #371–#376.

本文定义插件契约从 process/session/branch 类型层级迁移到“实例 + 服务绑定”的目标结构。运行时的实例包含树保留，但它只是生命周期和可见性机制；插件作者不实现 `Capability` trait、`InSession`、`InBranch`，也不声明自己属于哪一层。`Capability` 保留为 part 的角色：直接读写 Timeline 的 part。

本文是目标设计；当前的生成图见
[`docs/architecture/rust-agent/generated/`](../architecture/rust-agent/generated/)。
手写图只描述目标，当前实现以生成产物为准。

## 问题

当前插件作者模型把运行时的实例树又表达了一遍：

- [`Capability`](../../crates/dim-agent-runtime/src/capability.rs) 在 `rutis::TypedPlugin` 外面要求一个 `keys()`；
- [`SessionScope`/`BranchScope`](../../crates/dim-agent-runtime/src/scope.rs) 把实例 id 包成两个类型；
- `InSession<T>`/`InBranch<T>` 把“当前实例/父实例”编码成两个类型；
- [`AgentBuilder::session_plugin`/`timeline_plugin`](../../crates/dim-agent-runtime/src/assembly.rs) 只接受 `Capability`。

但服务键本身已经是实例键：`TypeKey::instance::<T>(InstanceId)`，rutis 的
`Ctx::check_instance` 也已经在运行时保证实例可见性。层级主要是类型包装，不是 runtime 机制。

后果：

1. 普通的 rutis `Plugin`/`TypedPlugin` 不能直接挂到 Session/Branch，必须先实现 Dim 的 `Capability`。
2. 跨语言插件必须理解 Rust 侧的 scope 类型，或者依赖一组专门适配的命名服务。
3. 一个跨作用域功能天然拆成两个插件，出现 `InputMethods`、`Wait`、`PromptRegistry` 这类薄适配节点。
4. 插件作者必须先理解三层作用域，才能写一个 tool、method 或事件观察者。

## 目标

- 保留 process root、Session 实例、Branch 实例的包含树，用于生命周期、可见性和清理。
- 插件只声明“我需要什么服务”；宿主在挂载时绑定具体实例和服务键。
- 任何 `rutis::Plugin` / `rutis::TypedPlugin` 都可以通过 raw mount 或 loader row 挂载。
- Capability 是角色，不是 Rust trait：只有直接读写 Timeline 的 part 才是 Capability。
- 多 Session、多 Branch、进程级共享资源、Timeline/Dispatcher 语义不变。
- 公开 Session/Branch 门面的 `PublicService` 限制不变。
- 跨语言插件只使用服务名、事件名和生命周期约定；不把 scope 写进插件结构。
- Node/TypeScript 先行，Python/peer 其次，dylib 单独评估。

## 非目标

- 不合并 33 个业务插件。插件合并是独立后续工作。
- 不删除运行时实例树，也不改变 Session/Branch 的关闭顺序。
- 不改变 Timeline、Dispatcher、Loop 的事件顺序或批次语义。
- 不让插件通过“我挂在哪一层”写业务分支；需要不同行为就拆成两个插件。
- 不和 rutis 0.15 升级一起做。

## 当前结构

概念图（不是生成产物）：

```text
rutis root
└─ rutis-loader
   └─ agent ⟨instanced⟩
      ├─ AgentScope
      └─ session ⟨instanced⟩
         ├─ SessionScope
         │   provides SessionConfig, SessionIdentity, SessionLifecycle, ...
         ├─ Session capabilities
         │   ToolRegistry, TimelineTree, Skills, PromptSources, ...
         └─ branch ⟨instanced⟩
             ├─ BranchScope
             │   provides Timeline, Dispatcher, BranchLifecycle, ...
             └─ Branch capabilities
                 Loop, InputQueue, ModelProvider, ToolBinding, ...
```

插件作者看到的契约：

```rust
impl TypedPlugin for WaitBranchPlugin {
    type Deps = (InBranch<Dispatcher>, InBranch<Timeline>);
    // ...
}

impl Capability for WaitBranchPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

builder.timeline_plugin(WaitBranchPlugin::new);
```

服务键已经是实例键，但插件类型和依赖类型仍然按层命名。

## 目标结构

概念图（不是生成产物）：

```text
rutis root
└─ rutis-loader
   └─ agent ⟨instanced⟩
      ├─ AgentScope
      └─ session ⟨instanced⟩
         ├─ SessionScopePlugin
         │   provides SessionConfig, SessionIdentity, SessionLifecycle, ...
         ├─ mounted parts
         │   role=capability:     gets Timeline/Dispatcher/TreeCommands when applicable
         │   role=contribution:   gets tools/methods/prompts/host services only
         └─ branch ⟨instanced⟩
             ├─ BranchScopePlugin
             │   provides Timeline, Dispatcher, BranchLifecycle, ...
             └─ mounted parts
                 role=capability:     gets Timeline/Dispatcher
                 role=contribution:   no Timeline/Dispatcher
```

插件作者看到的契约：

```rust
impl TypedPlugin for WaitBranchPlugin {
    type Deps = (Service<Dispatcher>, Service<Timeline>);
    // ...
}

// No Capability trait. The manifest/descriptor marks this part as role=capability.
builder.mount_branch_typed(|| WaitBranchPlugin::new());
```

`Service<T>` 不区分当前实例、父实例或进程。宿主在挂载时，把该 part 的 role 对应的服务集合绑定给插件。

## Capability 角色

Capability 不是“Session/Branch 插件”的同义词，也不是一个必须实现的 trait。它是 part 的角色：

**直接读或写 branch Timeline 的 part 才是 Capability。**

包括：

- 通过 Timeline 写事件；
- 通过 Dispatcher 消费事件；
- 通过 TreeCommands 向 branch 写 input；
- 只消费事件、不写事件的观察者，只要它能驱动外部行为，也算。

不算 Capability：

- 只提供 tool、session method、prompt source、host service；
- 只观察 Session broadcast 或 rutis 服务事件，不直接碰 branch Timeline；
- RPC、Transport、UI、FileStorage、HostPlatform、Provider 连接管理等宿主部件。

角色按 part 算。一个 feature 可以全是 Capability part，也可以混合：Goal 的 branch part 消费/写 Timeline，是 Capability；Goal 的 Session part 通过 `TreeCommands` 写 input，也是 Capability；Questions/Attachments 不写 Timeline，只是 contribution。

Role 由插件 manifest/descriptor 声明，宿主校验。没有声明 capability 的 part，绑定里没有 `Timeline`/`Dispatcher`/`TreeCommands`；它写不了事件，也订阅不了 Timeline。

## 服务模型

### 依赖类型

```rust
pub struct Service<T: ?Sized + Send + Sync + 'static>(pub Arc<T>);
pub struct ServiceNamed<T: ?Sized + Send + Sync + 'static>(pub String, pub Arc<T>);
pub struct Contributions<T: ?Sized + Send + Sync + 'static>(pub Vec<(String, Arc<T>)>);
```

- `Service<T>`：需要 T 的一个已绑定提供者。
- `ServiceNamed<T>(name)`：需要同一类型下指定名字的提供者。
- `Contributions<T>`：宿主提供 names 时，读取这些名字的提供者。

### 为什么 Rust 还需要 Service<T>

rutis 的 `Deps` 是一个具体类型，而且 `Arc<T>` 已经固定表示“root 的 `TypeKey::of::<T>()`”。
要表达“从宿主绑定的服务集合里取 T”，必须有一个不同的类型来实现不同的 `Deps::Keys`。
`Service<T>` 就是这个标记，不是服务容器，也不是层级；它的值仍然是 `Arc<T>`。跨语言插件没有这个类型，只写服务名。

如果不想在作者代码里出现它，可以用宏生成：

```rust
#[dim::deps]
struct WaitBranchDeps {
    #[inject] dispatcher: Arc<Dispatcher>,
    #[inject] config: Arc<SessionConfig>,
}
```

宏展开后仍然是 `Service<Dispatcher>`/`Service<SessionConfig>`；wrapper 留在类型系统里，
因为 rutis 需要它来做依赖门控。

### 服务集合

宿主按 part 的 role 构造显式服务集合：

```text
contribution/process set:  HostPlatform, FileStorage, SkillsCatalog, ...
contribution/session set:  process set + SessionConfig, SessionIdentity, ToolRegistry, ...
contribution/branch set:   session set + BranchLifecycle, TimelineIdentity, ...
capability/session set:    contribution/session set + TreeCommands
capability/branch set:     contribution/branch set + capability/session set +
                           Timeline, Dispatcher, Approval, ToolBinding,
                           ConfigureControl, InputQueue, ...
```

写能力的服务（`Timeline`、`Dispatcher`、`TreeCommands`、`Approval`、`ToolBinding`、`ConfigureControl`、`InputQueue` 等）只出现在 capability 集合。contribution 集合只放只读或纯提供类服务；写能力的服务不在里面。

集合是宿主组合出来的，不是插件运行时沿树搜索：

- Capability/Branch 能看到 Session/Process 服务，是因为集合显式包含了它们；
- Contribution/Branch 看不到 `Timeline`/`Dispatcher`/`TreeCommands`，所以写不了事件，也订阅不了 Timeline；
- Contribution/Session 看不到 Branch 服务；
- 兄弟 Branch 和另一个 Session 的服务不在集合里；
- 同一类型在同一集合里有多个提供者时，必须用 `ServiceNamed`/命名服务区分；否则装配失败。

没有“最近祖先获胜”的隐式规则。哪一层可见、绑定哪个实例、part 是什么 role，由宿主在挂载时决定并审计。

`TreeCommands` 不是例外：它能把 input 写进 branch Timeline，所以只出现在 capability 集合里。Session 工具/method part 如果要通过它写 input，就必须声明为 capability；只提供 method 但从不写 input 的 part 才是 contribution。

### 跨语言

跨语言插件使用稳定的服务名和事件名，不出现 `SessionScope`/`BranchScope`/`ServiceScope`：

```json
{
  "role": "capability",
  "inject": ["dispatcher", "timeline", "sessionConfig"],
  "events": {
    "writes": ["input/message"],
    "consumes": ["turn/end"]
  }
}
```

```json
{
  "role": "contribution",
  "inject": ["sessionConfig"],
  "provides": ["myTool"]
}
```

宿主用 role 对应的服务集合把名字解析成实例键。Rust 的 `Service<T>` 是类型绑定，TypeScript/Python 用字符串名；两者指向同一个提供者。

每个对外暴露的服务需要一个稳定名字；纯 Rust 内部服务可以只有类型键。名字是契约，不是层级字段。`events` 是描述性契约，用于评审、文档和诊断，不是运行时事件注册表。

### 挂载

```rust
pub trait InstanceDeps {
    type Keys;
    fn keys(bindings: &ServiceBindings) -> Result<Self::Keys, CordisError>;
}
```

`Service<T>`、`ServiceNamed<T>`、`Contributions<T>`、`When<D>`、tuple 实现 `InstanceDeps`。挂载时：

```rust
pub fn typed_at<P, F>(factory: F)
    -> impl Fn(&ServiceBindings) -> Result<Box<dyn Plugin>, CordisError>
where
    P: TypedPlugin,
    P::Deps: InstanceDeps,
    F: Fn() -> P + Send + Sync + 'static,
{
    move |bindings| {
        let plugin = factory();
        let keys = P::Deps::keys(bindings)?;
        Ok(Box::new(Typed::with_keys(plugin, keys)))
    }
}
```

`AgentBuilder` 增加 raw mount：

```rust
mount_session(make: impl Fn(&ServiceBindings) -> Box<dyn Plugin>)
mount_branch(make: impl Fn(&ServiceBindings) -> Box<dyn Plugin>)
mount_session_typed<P, F>(factory: F)
mount_branch_typed<P, F>(factory: F)
```

`ServiceBindings` 由宿主构造；插件不接收 `InstanceScope`，也不保存 scope。

旧的 `session_plugin`/`timeline_plugin`/`Capability` 只在迁移期间保留为内部过渡，不出现在公开文档，最终删除。

### 提供服务

插件不声明 scope，按当前挂载实例提供：

```rust
ctx.provide::<Timeline>(Arc::new(timeline))?;
ctx.provide_named::<ToolContribution>("skill", Arc::new(tool))?;
```

宿主把提供者加入当前实例的服务集合；Branch 集合在创建时组合 Session/Process 集合。插件不判断自己挂在哪一层。

### 缺失与晚到的提供者

- 必需依赖缺失：挂载失败，错误包含服务名、类型和挂载实例。
- 可选依赖：`When<D>` 返回 `None`，或 `OptionalService<T>` 返回 `None`。
- 晚到的提供者：沿用现有 Pending/reload 规则；插件在提供者出现后重新启动。
- 已绑定的服务键不会静默切换到后来出现的提供者；重载后按新的服务集合重新绑定。
- 这些规则要写入运行时测试，不能靠启动超时。

## 可见性与生命周期

- 服务键继续使用 `TypeKey::instance`；可见性继续由 rutis 的 `check_instance` 和 subtree 检查保证。
- 可见服务由宿主构造的 `ServiceBindings` 决定；没有隐式祖先搜索。
- `InstanceId` 不是持久身份；重载后重新生成，不写入 Timeline 或存储。
- 关闭 Session 关闭它的 Branch 子树；关闭 Branch 只卸载该 Branch。进程资源仍跨 Session 共享。
- `PublicService` 继续限制公开 `Session`/`Branch` 门面能读取的服务；`Service<T>` 不是公开门面的绕过口。

## 前后对比

| 方面 | 当前 | 目标 |
| --- | --- | --- |
| Capability 定义 | Rust trait + Session/Branch scope | 角色：直接读写 Timeline 的 part；没有 trait |
| 插件契约 | `TypedPlugin + Capability` | `Plugin` / `TypedPlugin`；不需要 `Capability` trait |
| 作用域选择 | 类型层 `SessionScope`/`BranchScope` | 宿主挂载点 + `ServiceBindings` |
| 当前/父/进程依赖 | `InBranch<T>` / `InSession<T>` / `Arc<T>` | 统一为 `Service<T>` |
| 服务范围声明 | 插件 `Capability::keys` | 宿主构造 role 对应的服务集合；插件不声明 scope |
| Timeline 权限 | 任何 Session/Branch Capability 都可能拿到 | 只有 role=capability 的 part 拿到 Timeline/Dispatcher/TreeCommands |
| 命名服务 | `Contributions<T>` + `SessionScope` | `ServiceNamed<T>` / `Contributions<T>` |
| 注册 | `session_plugin` / `timeline_plugin` | `mount_session` / `mount_branch` / raw loader row |
| 编译期防错 | Session 插件写 `InBranch` 直接编译失败 | 运行时校验：缺绑定挂载失败或 Pending，错误说明服务和实例 |
| 外部 Rust 插件 | 必须实现 Dim `Capability` | 普通 rutis 插件；只写 `Service<T>` 和 role |
| 外部语言插件 | 只有部分命名服务绑定 | role + 服务名 + 事件名 + 生命周期约定；无 scope 类型 |
| 插件文档 | 每页按 Session/Branch/Process 标作用域 | 每页标 role + 事件声明；实例 kind 只作诊断 |
| 运行时行为 | 实例键 + subtree 可见性 | 不变 |
| 重载 | InstanceId 重建 | 不变 |
| 公开门面 | `SessionService`/`BranchService` + `PublicService` | 只保留 `PublicService` 限制 |

### 示例：WaitBranch

当前：

```rust
type Deps = (InBranch<Dispatcher>, InBranch<Timeline>);
fn keys(&self) -> KeysOf<Self> { scoped::<Self::Deps, _>(&self.scope) }
```

目标：

```rust
type Deps = (Service<Dispatcher>, Service<Timeline>);
// no keys()
builder.mount_branch_typed(|| WaitBranchPlugin::new());
```

manifest/descriptor 把这个 part 标成 `role=capability`。宿主按这个角色把当前 Branch 的 `Dispatcher`/`Timeline` 绑给它。

### 示例：Branch 读取 Session 配置

当前：

```rust
type Deps = (InSession<SessionConfig>, InInstance<Dispatcher>);
```

目标：

```rust
type Deps = (Service<SessionConfig>, Service<Dispatcher>);
```

Branch capability 集合显式包含 Session 的 `SessionConfig` 和 Branch 的 `Dispatcher`。没有隐式祖先查找。

### 示例：命名 registry

当前：

```rust
type Deps = (InSession<SessionConfig>, Contributions<PromptSource>);
```

目标：

```rust
type Deps = (Service<SessionConfig>, Contributions<PromptSource>);
```

names 仍由宿主工厂提供；`Contributions<T>` 只解析这些名字，不引入 scope 类型。

### 示例：Goal 的混合角色

```json
{
  "name": "goal",
  "parts": {
    "branch": {
      "role": "capability",
      "inject": ["dispatcher", "timeline", "treeCommands"],
      "events": { "writes": ["input/message"], "consumes": ["turn/end", "input/deleted"] }
    },
    "session": {
      "role": "capability",
      "inject": ["treeCommands"],
      "provides": ["toolContribution:goal_update", "sessionMethods:session.goal"]
    }
  }
}
```

- Branch part 消费事件并写 input，是 Capability。
- Session part 通过 `TreeCommands` 写 input，也是 Capability；它不能以“只是提供工具”为由绕过。
- 如果某个 Session plugin 只提供 method、从不写 input，它才是 contribution，服务集合里没有 `TreeCommands`。

## 迁移顺序

1. 设计文档与 ADR；明确 Capability 的角色定义，不保留公开兼容层。
2. 新增 `Service<T>`、`ServiceNamed<T>`、`Contributions<T>`、`ServiceBindings`、role 校验、raw mount，并在一个真实 Branch 插件上验证。
3. 按事件声明给现有 part 分类：直接读写 Timeline 的标为 capability，其余标为 contribution；再迁移 `InSession<T>`/`InBranch<T>` 到 `Service<T>`。
4. 迁移 App Server 宿主插件：MCP、collaboration、provider、settings、transport；有 Timeline 写入的 Session 部分也标 capability。
5. 删除 `Capability` trait、`KeysOf`、`InSession`/`InBranch`、`SessionScope`/`BranchScope`、`SessionService`/`BranchService`，更新文档和生成图。
6. 并行推进 raw loader rows 和外部语言；Node/TS 先行。

详细任务见 #370–#376。

## Rule check

| 规则 | 应用 |
| --- | --- |
| §1 Capability 有界 | Capability 只指直接读写 Timeline 的 part；process 插件和纯 contribution 不叫 Capability；runtime 拥有 role 校验和挂载机制。 |
| §4.1 改状态写指令事件 | 本设计不改业务状态或事件；只改插件依赖/挂载契约。 |
| §4.2 对象交接不可撤回 | Timeline/Dispatcher 交接语义不变。 |
| §4.3 before 回调 | 不新增 before；仍在提交锁内同步变换。 |
| §4.4 核心机制不带业务含义 | `ServiceBindings` 只保存服务键和实例，不含业务字段。 |
| §4.5 原子批次 | 不变。 |
| §4.6 上下文路径 | 不变；仍经 adopted input/tool result。 |
| §4.7 插件间公开交互 | `Service<T>` 只读公开服务；不读私有状态。 |
| §4.8 基础设施默认不变 | 这是基础设施自身的契约问题：插件注册/挂载被层类型限制；修改在 runtime/loader 内闭环，不为某个业务功能设特例。 |
| §4.9 只有后续决定需要的事实进 Timeline | 不新增 Timeline 事实；`InstanceId` 不持久化。 |
| ADR 002 D1 | 实例树仍按状态所有者划分；插件不声明层级，宿主决定绑定。 |
| ADR 002 D2 | 不新增 wiring module；一个通用 mount/bindings 取代按层的注册适配。 |
| ADR 002 D3 | 依赖仍从扩展指向核心；服务集合由宿主显式组合。 |
| ADR 002 D4 | 结构配置仍在 loader row；服务绑定不是业务配置。 |
| ADR 002 D5 | 非 Rust 插件用稳定服务名；Rust `Service<T>` 只是 binding。 |

## Requirements raised to mechanism owners

- **Runtime (dim-agent-runtime)**：实现 `Service<T>`、`ServiceNamed<T>`、`Contributions<T>`、`ServiceBindings`、role 校验、capability-only 的 `Timeline`/`Dispatcher`/`TreeCommands` 绑定、raw session/branch mount、`provide`/`provide_named` helper、晚到提供者的 Pending/reload；删除旧层 API。跟踪 issue: #371, #375.
- **Loader/App Server**：为 Session/Branch group 增加 raw `PluginFactory` 行、role 字段和稳定服务名目录；Node/TS 先行，Python/peer 其次。跟踪 issue: #376.
- **rutis**：不向 rutis 提新需求；使用现有 `isolate`、`TypeKey::instance`、`Typed::with_keys` 和 loader instanced group。
- **文档**：更新 ADR 002、`single-tree.md`、`principles.md`，删除“Capability 只指 Session/Branch”的旧表述。

## Decisions

- 不保留 `Capability` trait/`SessionScope`/`BranchScope` 的公开兼容层；Capability 保留为 part 的角色。
- Capability = 直接读或写 branch Timeline 的 part，包括纯观察者和通过 `TreeCommands` 写 input 的 part。
- 只有 `role=capability` 的 part 拿到 `Timeline`/`Dispatcher`/`TreeCommands`；contribution 拿不到，宿主绑定强制。
- 不使用 `InInstance`/`InParent`/`InProcess`，也不使用 `ScopedService`/`ServiceScope`；服务可见性由宿主挂载时的 `ServiceBindings` 决定。
- **不做自动祖先搜索。** 哪个服务对哪个实例可见，是宿主组合，不是运行时猜测。
- 跨语言插件只依赖 role、服务名、事件名和生命周期约定；Rust 语法只是 binding。Node/TS → Python/peer → dylib。
- 不与插件合并、rutis 0.15 升级同批进行。
