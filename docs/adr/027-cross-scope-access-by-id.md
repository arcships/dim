# 027: 跨作用域访问按 id 直达所有者

Decision status: Proposed.

## Context

[ADR 002](002-plugin-scope.md) D2 规定，进程插件经 Agents 的 `sessions` 服务访问其他 Session。[Session 访问契约](../design/contracts/session-access.md)据此定义了 `sessions` 的方法。ADR 002 D3 又规定插件只依赖本作用域或外层作用域，所以进程插件不能注入 Session 组或分支组的服务。

`sessions` 的方法分两类：
- `launch`、`exists`、`delete` 是 Agents 自己的操作；
- 其余方法的数据和操作属于 Session 或分支内的所有者，例如输入的撤回与待处理属于 InputQueue，默认分支属于 TimelineTree，停止属于 Loop。

对第二类方法，Agents 要先取得 Session，再转述对方的结果、错误和新鲜度。每新增一种跨 Session 的需求，就要在 `sessions` 上加一个方法。Session 部件访问本 Session 内的分支所有者时，现在经 `Branch` 门面或交出的分支服务对象，同样绕开了所有者自己的操作。

[设计规则 §7](../architecture/rust-agent/principles.md#7-所有者与操作) 要求调用方直接请求数据的所有者，不代查，也不设只转发的门面。这需要一条从外层作用域到内层所有者的路径，而现在没有这样的路径：

- 每个 Session 实例和分支实例下，都有本作用域数据面的派发入口；
- 但实例键只能在实例内部读取，rutis 拒绝实例外的实例键（[Session 边界契约](../design/contracts/session-boundary.md)中的 `Session::instance()` 需求）。

## Decision

### D1. 外层作用域的调用方按 id 直达内层所有者的数据面

- 进程作用域的调用方：用 `sessionId` 到达 Session 组的数据面，用 `(sessionId, branchId)` 到达分支组的数据面。
- Session 作用域的调用方：用 `branchId` 到达本 Session 分支组的数据面。`branchId` 就是分支线的 id（line id）。
- 调用方直接调用目标所有者自己的操作。中间的所有者不按操作名转发，不包装请求或回复，也不转述目标所有者的错误。

### D2. 路由由 dim 运行时按 id 完成，实例键不离开实例

- 这是一个新机制，所有者是 dim 运行时。
- 运行时维护从 `sessionId`、`(sessionId, branchId)` 到当前存活实例的索引，把调用路由到目标实例内的派发入口。
- 调用方只给 id 和服务名，拿不到实例键。所以「实例键只在实例内部可读」的约定不变。
- 解析失败时，调用方要能分清三种情况：Session 不在、Session 正在关闭、分支不在。分支结束不等于 Session 关闭：Session 仍然存活时，分支不在对这个分支是最终结果，不影响同一 Session 的其他调用。错误的名字、每种错误下调用方的下一步和验收，由机制所有者在 [#2127](https://github.com/arcships/dim/issues/2127) 定义。

### D3. 可解析的只有目标作用域的数据面

- 可解析的是目标作用域在 catalog 中共享的数据面。
- 不包括机制登记点（`sessionMethods`、`toolRegistry` 等）。登记属于登记方这一代部件，其他语言的部件属于 bridge 的调用方 row，只能在本作用域内进行。
- 也不包括作用域的生命周期服务。
- 各语言用同一条路径：
  - Rust 插件在进程内调用；
  - TypeScript、Python 等语言运行时和 UI leaf 经 rutis 会话调用，与其他数据面相同。它们是宿主运行的可信本地 rutis 节点，不靠 id 隔离（[Dim 如何使用 rutis](../design/rutis-usage.md)，AGENTS.md 规则 11）。
- 哪些 link 能到达这条路径由部署决定（[服务面](../design/service-faces.md) §2.1）。
- RPC 客户端不经这条路径，仍经 Rpc 的方法表与 Session 授权访问。

### D4. 按 id 调用不是依赖

- ADR 002 D3 的依赖方向不变：调用方不把内层服务声明为依赖，也不参与内层的装配顺序。
- 调用方在每次调用时给出 id 和服务名，由运行时解析。调用方不在装配时声明内层组的名字：外层装配解析不了内层组的名字。
- 解析结果不跨调用保存，也不交给别的部件。

### D5. 让目标保持加载属于 Agents

- Session 已加载时，单次调用不需要保持。
- 进程调用方需要一个 Session 在多次调用之间保持加载，或者目标未加载，向 Agents 取一个保持；单次调用在调用后释放。
- 保持属于调用方的 loader row，在调用方释放或该 row 卸载时结算。
- 保持只决定 Session 是否加载，不提供对 Session 内部的访问。

### D6. Agents 只保留自己的操作

- Agents 拥有 Agent 与 Session 的实例：创建、关闭、删除，准入值，以及保持。
- `sessions` 中不属于 Agents 的方法删除，调用方按 D1 直接调用目标所有者。
- 每个方法的去向和被取代的条款由 Agents 契约（[#2098](https://github.com/arcships/dim/pull/2098)）给出，目标操作由各所有者的契约定义。

## Consequences

- ADR 002 D2 表中「Reach other Sessions」一行由 D1 取代。
- [Session 边界契约](../design/contracts/session-boundary.md)中以下两条不变：
  - 附录 F「No service lookup by name is added to the Session facade」：D1 的路径由运行时路由，不在 Session facade 上加查找；
  - 「实例键只在实例内部可读」：见 D2。
- [Session 访问契约](../design/contracts/session-access.md)与 [sessions 服务设计](../design/appserver/sessions-service.md)中的 `sessions` 服务由 Agents 契约取代，两份文档中关于可达范围的条款改为 D3。
- `sessions` 的使用方 Collaboration、Scheduler、Workflow、EventHub 改成 D1 的形状。Rpc 不使用 `sessions`，它的方法表派发仍走 Session facade。

### 向机制所有者提出的需求

| 项 | 内容 |
| --- | --- |
| 被挡住的 | 设计规则 §7「不代查」：进程部件与 Session 部件要直接调用内层所有者，但 ADR 002 D3 禁止注入内层服务，Session 边界契约又规定实例键只能在实例内部读取。 |
| 考虑过的现有机制 | 经 Agents 的 `sessions` 转发：违反 §7。Session facade 按名查找：附录 F 已经排除。把实例键交给调用方：违反 D2 中实例键不离开实例的约定。 |
| 需要的通用能力 | 按 `sessionId`、`(sessionId, branchId)` 把对目标作用域某个数据面的调用路由到存活实例；失败时区分 Session 不在、Session 正在关闭、分支不在（D2）。不带业务含义，也不把实例键交给调用方。 |
| 对序号、批次、中断、重载的影响 | 无：路由不写事件，调用的语义由目标所有者的操作决定。目标重载后，索引指向新的一代。 |
| 所有者与 issue | dim 运行时，两段都在 [#2127](https://github.com/arcships/dim/issues/2127) 跟踪。使用方：Session 到分支一段是 SessionScope 与 TimelineTree 契约中的 `line_owner`（[#2025](https://github.com/arcships/dim/issues/2025)、[#2024](https://github.com/arcships/dim/issues/2024)）；进程到 Session 一段是 Agents 契约（[#2020](https://github.com/arcships/dim/issues/2020)）。 |

## Non-goals

- 不改变作用域划分（ADR 002 D1）和依赖方向（ADR 002 D3）。
- 不改变机制登记点及其登记方式。
- 不规定任何语言的写法，写法属于绑定。
- 不改变客户端协议：RPC 客户端仍经 Rpc 的方法表访问 Session。

## Supersession

取代 [ADR 002](002-plugin-scope.md) D2 表中「Reach other Sessions」一行。D2 的其余各行和 D1、D3–D5 不变。
