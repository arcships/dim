# Rust Agent 设计规则

设计时的判断规则；机制与分工见[架构](../README.md)。事件与状态机的原则（只认序号、事件契约与消费规则、状态可恢复、写入前复核、副作用不重放）以[时间总线总纲](time-bus.md)为准，本文不重复。

## 1. Capability 有界

新功能做成一个 Capability。Capability 实现 rutis `TypedPlugin` 与 runtime `Capability`：`Deps` 声明全部依赖，含只用于装载顺序的依赖，`apply` 从 `Deps` 取得依赖；`apply` 注册服务与订阅，把句柄交给业务状态和回调；取得的资源立即用 `ctx.effect` 登记释放；`apply` 完成即服务与订阅可用，输入消费和后台业务在此之后启动。Capability 经 Session 级或 Timeline 级工厂注册，工厂只捕获配置和实例标识；作用域服务以该作用域的实例键（`SessionScope`/`BranchScope`）提供，依赖方以 `InSession`/`InBranch` 声明注入。Capability 之间的协调通过提交事件、订阅事件或 `Deps` 声明的服务完成。

所有可装配的运行能力和跨插件服务均由 Plugin 提供，包括文件访问、数据库和代码执行。普通模块只作为所属 Plugin 内部的函数、类型与算法实现，不能藏有绕过依赖声明的全局服务、资源或订阅。资源归属是单父树，服务依赖是允许多个上游的无环图；根、校验、失效重载以及唯一事件总线的目标约束见[单树装配](single-tree.md#产品装配约束目标)。

框架按 rutis 0.10.0 设计：使用 `TypeKey` 和 `require` / `require_as` 读取必需服务，使用 `EventKey<E>` 注册和派发事件。EventKey 的类型约束不代替 caps 事件声明或产品授权，严格读取不代替依赖环预检。具体接口统一见[单树合同](single-tree.md#rutis)，不增第二套服务容器、事件总线或生命周期管理器。

## 2. 正交

加功能是新增 Capability，或扩展该 Capability 自己的行为和状态。需要改动其他 Capability 时，把协调改为事件或 `injects` 声明的服务，由各 Capability 按自己的消费逻辑处理。

## 3. 职责归属

| 职责 | 归属 |
| --- | --- |
| 事件事实与 sequence 分配 | Timeline |
| 事件含义、处理逻辑、写入时机 | Capability |
| Timeline 写入 | Capability，经分支内注册的 `Timeline` 服务 |
| 状态 | 拥有它的 Capability（`StateCell`，外部只读） |
| 异步操作与取消 | 启动它的 Capability |
| 事件处理器内的等待 | Capability 自持任务，登记后返回 |
| 模型、工具、上下文的执行与格式转换 | Capability 定义的宿主端口；审批宿主在 app-server |

职责放错位置时修正归属方。

## 4. 验证

需求变化时，旧断言按新需求修改或删除，测试跟随实现。日常行为验证用各 crate 的 `crates/*/tests` 与仓库根的 `tests/integration`（跨包扩展、公开流程）。事件格式样本位于 `crates/dim-agent-types/tests/fixtures/`，是冻结的兼容基线：保留原始公开 wire 样本及其来源，不随实现变更更新；契约测试（`crates/dim-agent-types/tests/*_contract.rs`）对这些样本做反序列化与序列化校验，`cargo run -p dim-agent-types --example compat -- --emit` 输出原生 Rust 样本的 v1 wire 形式供对照。对冻结事件结构的扩展须经设计决定，并在契约测试中登记。
