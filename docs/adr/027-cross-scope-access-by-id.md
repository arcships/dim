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
- 调用方直接调用目标所有者自己的操作。中间不为某个操作单独开转发方法（例如 `sessions` 现在的 `withdraw`、`pending`），不改写请求或回复，也不转述目标所有者的错误。
- D2 中按 id 选目标的调用不算转发：它不带业务含义，对每个操作都一样，请求、回复和目标所有者的错误原样传递；只有选不中或加载不了时，才由持有者回答 D2 中的情况（三种选不中，以及加载失败）。错误的形状在 [#2127](https://github.com/arcships/dim/issues/2127) 定义。
- 调用里表示分支的字段名由绑定决定，在 [#2127](https://github.com/arcships/dim/issues/2127) 定义。

### D2. 由持有实例的一方按 id 选目标，实例键不离开实例

跨语言的调用只能传数据，不能传对象：rutis 不能把 Rust 的服务对象交给其他语言，也拒绝实例外的实例键，而 rutis 是外部依赖，不向它提需求（[设计规则 §4 规则 8](../architecture/rust-agent/principles.md)）。部件只能注入本实例和外层实例的服务（ADR 002 D3），所以往里一层，只能由持有子实例的一方按 id 选出目标。

- Session 由 Agents 持有：进程调用方经 Agents 的 `sessions`，按 `sessionId` 选 Session，可选再带 `branchId`。
- 分支由它所在的 Session 持有：Session 部件经本 Session 自己的数据面，按 `branchId` 选本 Session 的分支。它不能指定别的 Session。这个数据面是 Session 这个持有者自己的操作，不是只做路由的对象；Session 自己的读取也由它提供（[#2025](https://github.com/arcships/dim/issues/2025)）。
- 每一层查的是持有者已有的表：Agents 的 Session 表、Session 的分支表。不新建路由对象，也不另建索引。
- 一个分支在它发布之后才能被选中，结束之后选不中。分支正在建立、还没发布时，进程调用方和 Session 部件看到的都是「分支不在」：两条路径看的是 Session 分支表的同一份发布状态。
- 选中目标之后，调用的是目标所有者在 catalog 中的操作本身。所有者不为此登记，也不写代码。
- 调用方只给 id、服务名、方法名和参数，拿不到实例键，也拿不到对象。所以「实例键只在实例内部可读」的约定不变。
- 选不中时，调用方要能分清三种情况：Session 不在、Session 正在关闭、分支不在。分支结束不等于 Session 关闭：Session 仍然存活时，分支不在对这个分支是最终结果，不影响同一 Session 的其他调用。
- 加载失败不等于 Session 不在：Agents 正在重载时，调用方可以稍后重试；存储的 Session 恢复失败时，原因要交给调用方。只有 Session 不存在或已经关闭，才是「Session 不在」。这些情况都在调用目标的操作之前回答，不会调用到目标。
- 错误的名字、每种错误下调用方的下一步和验收，在 [#2127](https://github.com/arcships/dim/issues/2127) 定义。

### D3. 可调用的只有目标作用域的数据面

- 可调用的是目标作用域在 catalog 中共享的数据面。
- 不包括机制登记点（`sessionMethods`、`toolRegistry` 等）。登记属于登记方这一代部件，其他语言的部件属于 bridge 的调用方 row，只能在本作用域内进行。
- 也不包括作用域的生命周期服务。
- 各语言用同一条路径：
  - Rust 插件在进程内调用，用的是同一组数据操作，不另有按类型取对象的入口；
  - TypeScript、Python 等语言运行时和 UI leaf 经 rutis 会话调用，与其他数据面相同。它们是宿主运行的可信本地 rutis 节点，不靠 id 隔离（[Dim 如何使用 rutis](../design/rutis-usage.md)，AGENTS.md 规则 11）。
- 哪些 link 能到达这条路径由部署决定（[服务面](../design/service-faces.md) §2.1）。
- RPC 客户端不经这条路径，仍经 Rpc 的方法表与 Session 授权访问。

### D4. 按 id 调用不是依赖

- ADR 002 D3 的依赖方向不变：调用方不把内层服务声明为依赖，也不参与内层的装配顺序。
- 调用方在每次调用时给出 id 和服务名，由持有者解析。调用方不在装配时声明内层组的名字：外层装配解析不了内层组的名字。
- 解析结果不跨调用保存，也不交给别的部件。

### D5. 加载与保持属于 Agents

- 进程调用方经 `sessions` 的一次调用，由 Agents 在调用期间保持目标 Session 加载：Session 未加载时先加载，调用结束后释放。所以只有 Session 不存在，或已经关闭、不能再加载时，才是「Session 不在」（加载失败见 D2）。这与现在会取得 Session 的 `sessions` 方法和 Rpc 的 Session 方法相同；`exists` 这类只读是否存在的操作不加载 Session，也不走这条路径。
- Session 部件只调用本 Session，它在调用时一定已加载。
- 这一条只规定这条调用路径上的保持。现有的长期保持不变：Collaboration 发出的请求在结果写回之前保持成员 Session 加载，客户端 attach 之后在 detach 之前保持 Session 加载。改变它们是另外的产品决定。
- 保持只决定 Session 是否加载，选目标与调用仍按 D2。

### D6. Agents 只保留自己的操作

- Agents 拥有 Agent 与 Session 的实例：创建、关闭、删除，准入值，加载与保持，以及 D2 中按 `sessionId` 选 Session 的调用。
- `sessions` 中不属于 Agents 的方法删除，调用方按 D1 直接调用目标所有者。
- 每个方法的去向和删除它的改动，在 [#2020](https://github.com/arcships/dim/issues/2020) 给出，目标操作由各所有者定义。

## Consequences

- ADR 002 D2 表中「Reach other Sessions」一行由 D1 取代。
- [Session 边界契约](../design/contracts/session-boundary.md)中以下两条不变：
  - 附录 F「No service lookup by name is added to the Session facade」：按 `branchId` 选分支的调用属于 Session 自己的数据面，不加在 Rust 的 Session facade 上；
  - 「实例键只在实例内部可读」：见 D2。
- [Session 访问契约](../design/contracts/session-access.md)与 [sessions 服务设计](../design/appserver/sessions-service.md)中的 `sessions` 服务由 Agents 契约取代，两份文档中关于可达范围的条款改为 D3。
- `sessions` 的使用方 Collaboration、Scheduler、Workflow、EventHub 改成 D1 的形状。Rpc 不使用 `sessions`，它的方法表派发仍走 Session facade。
- 不新增机制，也就不向运行时另提机制需求。

## Non-goals

- 不改变作用域划分（ADR 002 D1）和依赖方向（ADR 002 D3）。
- 不改变机制登记点及其登记方式。`sessionMethods` 继续只服务客户端协议，不为内部调用方新增条目；要改它另开 ADR。
- 不规定任何语言的写法，写法属于绑定。
- 不改变客户端协议：RPC 客户端仍经 Rpc 的方法表访问 Session。

## Supersession

取代 [ADR 002](002-plugin-scope.md) D2 表中「Reach other Sessions」一行。D2 的其余各行和 D1、D3–D5 不变。
