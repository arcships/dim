# 空闲 Session 卸载

状态：提议，待团队评审。路线图：Agents 负责线（[#126](https://github.com/arcships/dim/issues/126)），即
[S2 §8.6](sessions-service.md#86-目标空闲卸载后续项) 的目标。它把 S3 的 Session 活动句柄
（[#57](https://github.com/arcships/dim/issues/57)、[ShellInput A.5](../plugins/ShellInput.md#a5-对其他插件的改动)）
以运行时活动持有的形式重新打开。必须在 Scheduler 的 `create` 模式任务（[P6](../plugins/Scheduler.md)，
[#70](https://github.com/arcships/dim/issues/70)）和 Workflow 反复创建阶段 Session（[P7](../plugins/Workflow.md)，
[#71](https://github.com/arcships/dim/issues/71)）上线之前完成。本文只是设计；实现等 P6 或 P7 进入 Ready 后开始
（[路线图 §2](../../../design/roadmap.md#2-排期原则) 第 5 条）。范围：[ADR 002](../../adr/002-plugin-scope.md) 进程作用域，
在现有 Agents 插件内，另有一项运行时需求和三项所有者采用（§11）。

## 1. 概要

App Server 从不卸载空闲 Session：Session 保持装载，直到被关闭或进程退出
（[生命周期 §5](../../../architecture/rust-agent/lifecycle.md#5-stop-与-close)）。Scheduler 和 Workflow 为每次运行或每个阶段启动一个
Session，已装载的 Session 因此无限增长（§2）。本设计让 Agents 卸载它们。

- **规则。** 一个已装载的 Session 在至少宽限期 `G` 内没有任何租约（`SessionUse`）持有，并且
  `Session::unload_if_idle()` 释放了它，Agents 就卸载它。下一次 `acquire` 恢复它。卸载不是关闭：卸载本身不写事件（恢复时按重启后的做法用
  `branch/resume` 打开各分支），Session 的每个使用者看到的都与卸载前一样。
- **什么持有 Session。** 只有租约（§4）：attach 的客户端、进行中的 Rpc 或 `sessions` 调用，以及 Collaboration 的操作或对成员未结束的
  请求。Agents 不解读其他任何东西。
- **脱离工作**（开放问题 1）。运行中的 `!` 命令、BackgroundExec 任务或未结束的 Collaboration 请求阻止卸载。工作的所有者在工作运行期间取得一个运行时
  **活动持有**，只要还有持有，`unload_if_idle()` 就拒绝（§5，向运行时提出的需求 R1，由 R2 到 R4 采用）。自动卸载从不取消已接纳的工作。
- **何时检查**（开放问题 2）。每隔 `G` 运行一轮检查（§6）。它按状态触发：最后一个租约刚释放的 Session，以及没有租约、之后才变为空闲的 Session，
  都由它覆盖，不会漏掉任何边沿。`G` 默认 60 s，是宿主配置而不是用户设置。
- **竞争。** 检查在该 Session 的准入闸门下卸载；`acquire`、创建和删除已经使用这个闸门，因此卸载期间到来的 `acquire` 先等待，再恢复（§7）。
- **范围。** 有原生存储的宿主上的所有 Session：客户端的、启动的、成员的、分叉的。没有原生存储时运行时无法恢复 Session，因此不卸载任何 Session。

不新增 Rpc 方法、事件、Timeline 事实、存储格式或客户端可见的接口。不向 rutis 提需求。

## 2. 问题与依据

S2 的 spike 测量了一个已装载的空闲 Session（[S2 §2](sessions-service.md#2-spike-结论) 第 6 条；`xtask measure`，release 构建，
mock 模型，Apple M4）：约 300 KB Rust 分配、43 个 Tokio 任务，并且 Session 越积越多，创建越慢。

| 已装载的 Session | 平均创建耗时 |
| --- | --- |
| 10 | 1.5 ms |
| 100 | 4.0 ms |
| 1000 | 57 ms |

每五分钟一个 Scheduler 任务，一天留下约 288 个已装载的 Session：约 86 MB 和 12,000 个任务。S2 D14 在运行记录过期时删除运行用的 Session，
约束的是存储和 Session 列表，不是内存：保留下来的运行，其 Session 仍然装载着。

现状：

| 部件 | 状态 |
| --- | --- |
| `Session::unload_if_idle()`（[agent.rs](../../../../crates/dim-agent-runtime/src/agent.rs)） | 按分支 id 顺序取得各分支的提交锁，检查 Loop 空闲、没有未发布的事件、分发已到尾部，然后把每个分支冻结在该尾部，并以关闭的写入窗口释放实例。生产代码中没有调用方；内存存储、故障或正在关闭的 Session 一律拒绝 |
| 恢复（[agents.rs](../../../../crates/dim-app-server/src/agents.rs) `acquire`） | 在该 Session 的准入闸门下：授权项目，从原生存储恢复 Session，发出 `SessionRestored` |
| 租约 | `SessionUse` 按 Session 计数（`SessionSlot.uses`）；计数一直维护，但从未被读取 |
| 卸载时的脱离工作 | 运行时不检查（BackgroundExec 测试中的“首版限制”）。BackgroundExec 写入 `exec/ended {cancelled}`，它永远不会唤醒；ShellInput 终止运行且什么都不写；Collaboration 把未结束的请求结算为 `failed` 并中断成员 |

恢复也有代价。它重建 Session 作用域和每个分支，把历史重放给需要折叠它的能力，并像重启后一样用 `branch/resume` 打开每个分支。为本设计做的一次性测量
（`c044140` 的 release 构建，一次性数据库上的 `SqliteTimelineStore`，`defaults::mock_hosts` 与 `defaults::install`，Apple M4；每个规模一个
Session，卸载并恢复七次；创建在同一存储上测量以便对比；未提交任何内容）：

| `main` 上的事件数 | 恢复并完成第一次 `idle`，中位数 | 创建 | 卸载 |
| --- | --- | --- | --- |
| 1 | 22 ms | 23 ms | 1.0 ms |
| 101 | 22 ms | 23 ms | 0.5 ms |
| 1,001 | 51 ms | 22 ms | 0.2 ms |
| 10,001 | 2.27 s | 28 ms | 0.2 ms |

在产品中，恢复还要授权项目、绑定 Session 的 TypeScript 部件、重新取得 MCP 连接，这次测量没有包含这些。对设计有两点影响：每次调用都恢复一次，
每次要花几十毫秒到几秒，并且每次多写一个 `branch/resume`，这正是宽限期要避免的（§6.2）；卸载之后的第一次使用要付出一次恢复，和今天重启之后的第一次使用一样。
恢复耗时的增长快于历史长度（最后两行之间事件数增加十倍，耗时增加约 45 倍）。这是恢复性能的问题，归其所有者，不是本设计的前置条件。

## 3. 决定

| # | 决定 | 放弃的方案 | 原因 |
| --- | --- | --- | --- |
| D1 | 只有租约持有 Session；Agents 只读自己的租约计数、运行时的 `unload_if_idle()`，以及（R1 之后）它检查的持有 | Agents 检查每个持有者自己的状态（订阅、关系、运行） | 每个持有者已经在用同一个机制（§4）。由 Agents 读插件状态，就是在核心部件里为插件写特殊处理（原则 §4 第 4、7 条，ADR 002 D3） |
| D2 | 单有 Collaboration 关系不持有成员；未结束的请求持有（它已经持有成员的租约） | 关系钉住成员 | 长期存在的 Team 的成员会一直装载到 leader 关闭：正是本设计要消除的泄漏。关系是进程内存，卸载后仍在，下一次请求会恢复成员 |
| D3 | 已接纳的脱离工作阻止自动卸载 | 照样卸载，即今天 `unload_if_idle()` 的行为、ShellInput §4 接受的结果 | 断开的用户会丢掉 `!` 运行或后台命令，BackgroundExec 的结束会被写成 `cancelled`、永远不唤醒模型，Collaboration 请求会失败、成员被中断。断开客户端不是关闭 Session（AGENTS.md 第 8 条），卸载也不能表现得像关闭 |
| D4 | 由工作所有者取得运行时活动持有来识别脱离工作；只要有持有，`unload_if_idle()` 就拒绝，并且与它的分支检查在同一步完成（R1） | (a) Agents 读所有者的事实，例如没有 `exec/ended` 的 `exec/started`；(b) S3 原先的句柄，由 Agents 提供；(c) 所有者持有 `SessionUse`；(d) 所有者让 Loop 保持忙碌 | (a) ShellInput 的运行不写任何事实，宿主还得解读插件事件。(b)、(c) dim-agent-capabilities 不能依赖 App Server 的类型，其他调用 `unload_if_idle()` 的宿主得不到保护，检查也落在运行时的提交锁步骤之外。(d) 会改变 `idle`，而按合同 `idle` 不包含脱离工作（S2 D10） |
| D5 | 每隔 `G` 一轮、按状态触发的检查来决定；卸载自上一轮以来没有租约、且 `unload_if_idle()` 成功的 Session | (a) 按边沿触发：最后一个租约释放时启动计时器，加上运行时的“已静止”通知；(b) 最后一个租约释放时立即卸载 | (a) 需要新的门面通知，每个没有租约的 Session 一个观察任务，漏掉一次边沿就让 Session 永远装载；仍然需要宽限计时器。(b) 漏掉最后一个租约释放之后才变空闲的 Session（提交后离开、只调 `sessions.input` 不调 `idle`），并且在连续调用之间来回装卸。[Orleans 的 activation collection](https://learn.microsoft.com/en-us/dotnet/orleans/host/configuration-guide/activation-collection) 也是这样：空闲时长由周期扫描检查，调用算作活动，`DelayDeactivation` 相当于持有 |
| D6 | 宽限期 `G` = 60 s，`ServerConfig.idle_unload_after` | 没有宽限期；按数量约束（保留最近 N 个没有租约的 Session）；用户设置 | 没有宽限期时，一次 S2 运行几乎每次调用都要恢复它的 Session（§6.2）。按数量约束会让 N 个 Session 永远装载，而 N 和时间一样是随意选的。没有已知的用户需求（ADR 002 D4）；测试需要短的取值，所以作为宿主配置放在 `startup_timeout` 旁边 |
| D7 | 卸载在该 Session 的准入闸门下进行；检查从不等待闸门 | 单独的卸载锁，或 `acquire` 检查的“正在卸载”标志 | 闸门已经为 `acquire` 的恢复、预备创建和删除排序（S5c §7）。第二把锁容易与它们产生锁顺序问题，标志又需要自己的等待 |
| D8 | 不取租约的读取（`session.list`、`session.summaries`、Collaboration 经 `AgentIndex::read_page` 对成员的 `agent` 读取）在找到的已装载 Session 被卸载时，改读存储 | 为列出的每个 Session 取租约 | 租约会恢复并钉住每个列出的 Session。存储中的记录包含每个已发布的事件 |
| D9 | 由 [ADR 005](../../../adr/005-observability.md) 撤销：它删除了 trace 归档，不再有 Session 为观测而持有租约 | — | 运行事后经 Timeline 与进程日志读取，卸载与恢复都不会打断它们 |
| D10 | SessionTitle 的生成和 S5b 的 `setTitle` 不取持有 | 持有到标题写入 | SessionTitle 的设计接受卸载在交给存储之前取消生成，并在下一个被采纳的输入时重试（[SessionTitle §4](../plugins/SessionTitle.md#4-时序与冲突)）。它的 30 s 预算低于 `G`，所以提交后离开的 Session 在卸载可能开始之前就已生成完标题。已受理的 `setTitle` 是 SessionStore 的进程级操作，无论 Session 是否装载都会结算 |
| D11 | 不加客户端接口，不加指标；每次卸载记一行 debug 日志，每次失败记一条警告 | `session.unload` 方法、卸载事件或计数器 | 没有使用方。`agent.list` 已经报告每个 Agent 的已装载 Session 数 |

## 4. 什么持有 Session

持有者用 `AgentIndex::acquire` 取得租约（需要时恢复 Session），用完释放。

| 持有者 | 今天如何持有 | 是否持有 |
| --- | --- | --- |
| attach 的客户端（`session.attach`，本地或经 RemoteGateway、AcpGateway） | 订阅保留 attach 调用的租约，直到 detach、被替换、`Closed` 或连接关闭 | 是 |
| Session 上的 Rpc 调用：`session.submit`、`history`、`input_lookup`、`stop`、`idle`、`approval.respond`、创建分支、执行类方法、`session.methods` 和每个 `sessionMethods` 调用（`shell.*`、`session.rename`），以及对可用 Session 的 `session.close` | 每次调用一个租约 | 是，调用期间 |
| `sessions` 调用（`input`、`lookup`、`history`、`withdraw`、`stop`、`idle`；`launch` 读取启动方） | 每次调用一个租约（S2 §6） | 是，调用期间 |
| 进行中的 Collaboration 操作（`lookup` 得到的 `Peer`） | 持有对方的租约 | 是，操作期间 |
| 对成员未结束的 Collaboration 请求（已送达，尚无结果） | `Request` 持有成员的租约 | 是，持有成员。leader 一侧是脱离工作：R4 |
| 没有未结束请求的 Collaboration 关系 | 进程内存；卸载后仍在 | 否（D2） |
| `session.fork` 的源 | 创建分叉期间的租约 | 是，期间 |
| `session.delete`、`sessions.delete` | 准入闸门 | 与卸载串行（§7） |
| 未闭合的 turn：模型或工具调用运行中、审批或问题待处理、输入未发布或未分发 | 运行时状态 | 阻止 `unload_if_idle()` |
| 运行中的 `!` 命令、BackgroundExec 任务、未结束 Collaboration 请求的 leader 一侧 | 今天没有 | 通过持有阻止（§5） |
| C4 leaf 观察者（`sessionEvents` 转发） | 不恢复的查找；跳过正在卸载的 Session；恢复后使用新通道（[C4 设计](../../../design/session-typescript.md)） | 否：它们只观察 |
| C6 通知的监听者 | 只有 attach 的客户端 | 经由 attach |
| `session.list`、`session.summaries`、`agent.list`、Collaboration 的 Team 状态与 `agent` 读取 | 不恢复的读取 | 否：它们容忍卸载（D8） |
| Scheduler 和 Workflow 的运行记录 | 各自的记录；每次调用一个租约 | 否：S2 D12，并且它们不得自己加卸载循环 |
| SessionTitle 的生成、S5b 的 `setTitle` | Session 部件的任务；SessionStore 的操作 | 否（D10） |
| `!cmd &` 留在后台的进程 | 运行结束后不归 ShellInput 管（[ShellInput A.2](../plugins/ShellInput.md#a2-执行)） | 否：Dim 中没有任何东西跟踪它们，卸载也触及不到 |

S2 §8.6 把“Collaboration 成员关系”列为持有者。D2 把它收窄为未结束的请求，而请求本来就持有。

## 5. 脱离工作

### 5.1 活动持有

运行时在 `SessionLifecycle` 上增加一个通用的持有。Session 部件注入 `SessionLifecycle`，分支部件以 `InSession<SessionLifecycle>` 取得它（R1）：

```rust
impl SessionLifecycle {
    /// None once the Session is closing or being released.
    pub fn hold(&self) -> Option<SessionHold>;
}
/// Dropping it releases the hold.
pub struct SessionHold { /* private */ }
```

所有者为每一项已接纳的脱离工作取得一个持有，时机在接纳这项工作的 turn 或租约结束之前；在工作的最后一个事实提交之后释放。持有不带身份、结果或取消；任务仍归所有者，
符合生命周期 §4 的要求。只要还有持有，`unload_if_idle()` 就拒绝。持有检查与释放决定在同一步完成：决定之前取得的持有让它拒绝，决定之后请求的持有得到
`None`，与正在关闭的 Session 一样。`Session::close()` 不理会持有，照旧结束工作。`Session::idle()`、`session.idle` 和 `sessions.idle` 不变：
它们仍不包含脱离工作（S2 D10）。

| 所有者 | 取得持有 | 释放持有 | 释放之后 |
| --- | --- | --- | --- |
| BackgroundExec（R2） | 在 `start` 中、追加 `exec/started` 之前（在工具调用中，turn 未闭合） | 监管任务提交 `exec/ended` 之后，或启动被拒绝时 | `exec/ended` 在其处理器追加唤醒输入之前不算已分发，因此直到被唤醒的 turn 结束，分支都不空闲 |
| ShellInput（R3） | 在第二次准入时，与 spawn 和登记一起在登记表锁下完成（此时 Rpc 调用持有租约） | 运行任务结束时：记录输入的追加返回之后，`private` 模式下进程退出时，或被终止时 | 记录下来的 Append 输入在分发时被采纳，不启动 turn |
| Collaboration，leader 一侧（R4） | 与持有成员租约的 `Request` 一起，在 leader 的工具调用等待送达期间创建 | `collaboration/result` 提交、`Request` 移除时 | 结果的分发会投递结果输入，唤醒 leader |

因此每个持有都是在租约或未闭合的 turn 已经让 Session 保持装载时取得的，并且只在一个让分支保持忙碌、直到分发处理完它的事实之后才释放。中间没有空档可以让卸载取消工作或丢失唤醒。

```text
 client      Rpc lease   ShellInput run           pass (every G)               Session S
   |-- shell.run -->| 1  | admit+spawn: hold 1    |                            |
   |-- disconnect   | 0  | running                | lease-free: mark           |
   |                |    |                        | unload_if_idle() -> false  | (hold)
   |                |    | exits; append X        |                            | input/message X (append)
   |                |    | hold 0                 |                            | X applied: idle
   |                |    |                        | unload_if_idle() -> true   | released, no event
```

### 5.2 不变的部分

- 所有者的重载、Session 关闭和 App Server 关闭仍按各自的设计结束工作（ShellInput §3.2、BackgroundExec、Collaboration 的 `unload`）。持有只阻止自动的空闲释放。
- 其他宿主显式调用 `unload_if_idle()` 时得到同样的保护。任何调用方都不能卸载带有已接纳脱离工作的 Session；确实需要结束它的调用方应关闭它。
- 没有取得持有的工作不受保护。今后的脱离工作所有者（例如进入 Ready 时的 ExternalAgent）以同样方式取得持有，或者说明卸载为何可以结束它的工作。
  TypeScript 所有者需要 C4 暴露持有；目前没有这样的需要。

## 6. 何时卸载

### 6.1 检查轮次

Agents 索引的每一代在活动且有原生存储时，每隔 `G` 运行一轮。每一轮访问每个存活 Agent 的每个已装载 Session：

1. 槽已关闭（Session 已关闭或已删除）：跳过。
2. `uses > 0`：忘掉该 Session 的标记。
3. `uses == 0`，且没有标记，或标记与槽的 acquire 计数不同：用该计数标记它。
4. `uses == 0`，且标记等于 acquire 计数：自上一轮以来没有取过租约，所以 Session 已至少 `G` 没有租约。不等待地尝试取得准入闸门；闸门被占说明有恢复、创建或删除在进行，
   跳过。在闸门下再次确认 `uses == 0` 和计数，然后调用 `unload_if_idle()`，直到它返回才释放闸门。
   - `true`：已卸载。忘掉标记。
   - `false`：忙（turn、发布、分发、持有、正在关闭、故障）。保留标记；下一轮再试。
   - 错误：运行时已释放实例并报告清理错误。记一条警告，忘掉标记。下一次 `acquire` 恢复该 Session，恢复会结算释放时没能写入的内容。

槽增加一个计数器，每次 `acquire` 加一。标记是检查轮次的内存，索引重载后重建。一轮里逐个卸载，所以一次慢的释放只会推迟这一轮和该 Session 的下一次
`acquire`，不会推迟其他 Session 上的请求。

```text
                          acquire
          +-----------------------------------------------+
          v                                               |
   +--------------+   last lease drops   +--------------------------+
   |     Held     | -------------------> |        Lease-free        |
   |   uses > 0   | <------------------- | uses == 0, acquire count |
   +--------------+       acquire        +--------------------------+
                                                    | a pass finds the count unchanged
                                                    | since the previous pass
                                                    v
                                     +------------------------------+
                                     | try the admission gate       |-- taken: skip
                                     | uses == 0, count unchanged?  |-- no: skip
                                     +------------------------------+
                                                    | yes, gate held
                                                    v
                                     +------------------------------+  false: busy (turn,
                                     |  Session::unload_if_idle()   |  hold, publication,
                                     +------------------------------+  fault); next pass
                                                    | true
                                                    v
                                     +------------------------------+
                                     | Unloaded: stored only; gate  |
                                     | released; next acquire       |
                                     | restores                     |
                                     +------------------------------+
```

R1 之后 `unload_if_idle()` 内部：

```text
 holds > 0 ?                                            yes -> false (no lock taken)
 skip each branch whose own close has finished: it commits nothing more
 take each other branch's commit lock in branch-id order
 each branch: not closing, Loop Idle, nothing unpublished,
              dispatched == tail ?                                   no  -> unlock, false
 holds == 0, and refuse new holds (one step with the check) ?        no  -> unlock, false
 freeze every branch at its tail, start the release (closing, no stop events)
 settle as an idle release (finalizers may still write), remove the instance
 -> true
```

### 6.2 宽限期

没有宽限期时，一次 S2 运行几乎在每两次调用之间都要卸载并恢复它的 Session：`idle` 在第一个空闲点回复并释放租约，随后的每一页 `history` 都要恢复 Session、
重放历史、写一个 `branch/resume`，然后检查又把它释放。每次这样的恢复要 22 ms 到几秒（§2）。`G` 必须长过这些间隔，又要足够短以约束内存。

| 界限 | 间隔或代价 | 对 `G` 的影响 |
| --- | --- | --- |
| 下界 | 同一使用方的连续调用（S2 的 `input`、`idle`、`history` 分页；不 attach 只读取的客户端）：毫秒级 | 任何 `G` 都能覆盖 |
| 下界 | 客户端重连（CLI 重启、网络中断）在几秒内重新 attach | 几秒 |
| 下界 | Collaboration leader 对同一成员的下一个请求要隔一个模型 turn：几秒到几十秒 | 几十秒 |
| 下界 | SessionTitle 的生成预算是 30 s（D10） | 30 s 以上 |
| 上界 | 被释放的 Session 在 `G` 到 `2G` 之间保持装载：最多是最近 `2G` 内被释放的 Session，每个约 300 KB（§2） | 每五分钟一个任务时，只要 `G` 不超过 2.5 分钟，每个任务最多只有一个已释放的运行 Session 装载着；Workflow 扇出 20 个阶段，最多在 `2G` 内占约 6 MB |

约 30 s 到几分钟之间的任何取值都满足上下界；60 s 是高于标题预算的最短整分钟，也覆盖 leader 的一个 turn。它是 `ServerConfig.idle_unload_after`
（正的 `Duration`，默认 60 s），由宿主像 `startup_timeout` 一样设置；流程测试把它设为几百毫秒。用户没有这项设置。作为对照，Akka Cluster Sharding 默认在实体空闲 120 s 后将其钝化
（[akka#26787](https://github.com/akka/akka-core/pull/26787)），Orleans 回收空闲 15 分钟的 activation，每分钟扫描一次。

### 6.3 Scheduler 式的一次运行

```text
 Scheduler          sessions / AgentIndex           slot S (uses, count)     pass            Session S
   |-- launch ------->| create S (prepared, gate)     | 0, 0                 |               | loaded
   |-- input -------->| acquire                       | 1, 1                 |               | turn starts
   |<- receipt -------| drop lease                    | 0, 1                 | mark 1        |
   |-- idle --------->| acquire, wait                 | 1, 2                 | uses 1: skip  |
   |<- {C} -----------| drop lease                    | 0, 2                 |               | idle
   |-- history x3 --->| acquire / drop, no restore    | 0, 5                 | mark 5        |
   |                  |                               |                      | 5 unchanged:  |
   |                  |                               |                      | gate, unload->| released
   |-- delete (retention, later) -> gate; S not loaded: rows removed, nothing restored
```

周期短于 `G` 的 `reuse` 模式任务让它的 Session 一直装载；周期更长时每次运行恢复一次。

## 7. 竞争与交互

| 场景 | 顺序 | 结果 | 归属 |
| --- | --- | --- | --- |
| 卸载期间的 `acquire` | 检查持有 S 的闸门 | `acquire` 等待闸门，发现 S 未装载，恢复后继续；调用永远看不到释放到一半的实例 | Agents |
| 恢复、创建或删除期间尝试卸载 | 闸门被占 | 这一轮跳过 S | Agents |
| 两轮之间取得又释放了租约 | acquire 计数变化 | 下一轮不卸载；宽限期重新开始 | Agents |
| 卸载期间的 `session.delete` / `sessions.delete` | 删除等待闸门 | 发现 S 未装载，删除其行 | Agents（S5c） |
| 卸载期间的 `session.close` | 关闭会 acquire 可用的 S，因此等待闸门 | 恢复 S，再连同关闭事实一起关闭它 | Rpc、Agents |
| 卸载期间的 Agent 关闭或进程关闭 | 不经过闸门 | 加入已在进行的释放，并发关闭共享同一流程（生命周期 §5） | 运行时 |
| 检查轮次期间 Agents 索引重载 | 旧一代正在停止 | 检查停止；进行中的释放在旧一代的清理返回前完成；新一代重新标记 | Agents |
| S 卸载期间的 `session.list`、`session.summaries` 或 Collaboration 的 `agent` 读取 | 先找到已装载的 S，随后 S 被释放 | 对已装载实例的读取因 S 被释放而失败，改读存储（D8）；调用不会失败，也不会漏掉 S | Agents、Rpc |
| 已卸载成员的 Collaboration Team 状态 | 任意 | `idle`，与任何可用的 Session 一样 | Collaboration |
| 对已卸载成员的 Collaboration 请求 | leader 的工具调用 | `lookup` 恢复成员；`SessionRestored` 让 Collaboration 重新观察它；关系保留 | Collaboration |
| 临时 Agent 的最后一个 Session 被卸载 | 任意 | Agent 保留：Collaboration 只在业务关闭后才关闭它 | Collaboration |
| C4 leaf 观察者 | 任意 | 丢弃正在卸载的 Session 的记录；恢复后得到新通道 | C4 |
| 卸载期间的 C6 通知 | 没有监听者 | 丢弃，与任何没有 attach 客户端的 Session 一样；之后 attach 的客户端经 `session.list` 读取当前状态（[C6](https://github.com/arcships/dim/pull/85)） | C6 |
| 卸载前已受理的 S5b `setTitle` | 任意 | 照常由 SessionStore 结算；如果那时 S 已卸载，其通知被丢弃 | SessionStore |
| 无人观察的启动 Session 有待处理的审批 | turn 未闭合 | 永不卸载；Scheduler 或 Workflow 用 `stop` 结束这次运行 | 运行时 |
| 按 S2 §6.4 第 2 项被扣住的 Queue 输入（Loop 在 `input/applied` 与 `stop/interrupt` 之间重载） | Loop 空闲 | 卸载可以进行；恢复从历史重建 InputQueue 和 Loop，像重启后一样决定这些被扣住的输入 | InputQueue、Loop |
| 分叉一个已卸载的源 | `fork` acquire 它 | 源被恢复；源和分叉若不再使用，各自在 `G` 之后卸载 | Agents（S5c） |
| 恢复时的检查 | 任意 | 与重启后相同：项目授权、存储的 Agent 可用。重启后无法装载的 Session，卸载后同样无法装载 | Agents |
| Session 部件按代保存的内存（ShellInput 用过的 `runId`、SessionTitle 的尝试次数、MCP 连接、TypeScript 部件状态） | 任意 | 与重启后一样被重置；各所有者的设计已经覆盖重载 | 所有者 |
| 故障或正在关闭的 Session | 任意 | `unload_if_idle()` 拒绝；由监督者或关闭结束它 | 运行时 |
| Session 保留、单独关闭的分支（`Branch::close()`） | 任意 | 分支仍在关闭时 `unload_if_idle()` 拒绝，关闭完成后跳过它；恢复时像重启后一样用 `branch/resume` 再次打开它 | 运行时 |
| 没有原生存储的宿主 | 任意 | 不运行检查；运行时无法恢复内存中的 Session | Agents |

## 8. Agents 负责线的改动

| 部件 | 文件 | 改动 |
| --- | --- | --- |
| `SessionSlot` | [agents.rs](../../../../crates/dim-app-server/src/agents.rs) | 在 `uses` 旁增加 acquire 计数器，每次 `acquire` 加一 |
| 卸载检查 | agents.rs | 有原生存储时由 `AgentsPlugin` 为本代索引启动；本代的清理停止它并等待其完成（§6.1） |
| 不取租约的读取 | agents.rs `session_directory`、`session_summary`、`read_page` | 已装载的 Session 在读取过程中被释放时，改读存储（D8） |
| `ServerConfig.idle_unload_after` | App Server `lib.rs` | 一个字段及其默认值，属于装配负责线（zerob13），与 S2 在那里加的一行相同 |

## 9. 可观测性

释放会在进程日志中发出运行时现有的 `session.close` span（[ADR 005](../../../adr/005-observability.md) D4）。Agents 为每次卸载记一行 debug 日志，为每次失败记一条警告，都带 Session id。`agent.list`
已经报告每个 Agent 的已装载 Session 数，验收测的就是它。客户端看不到任何新东西。

## 10. 规则检查

| 规则 | 回答 |
| --- | --- |
| §4 第 1 条（指令事件） | 没有事件。卸载不改变其他模块的状态；持有者释放自己的租约和持有 |
| §4 第 2 条（交接） | 不收回任何东西。已接纳的脱离工作留在所有者手中，直到它的最后一个事实 |
| §4 第 3 条（before 回调） | 无 |
| §4 第 4 条（核心不带业务含义） | Agents 计数租约，运行时计数持有。两者都不知道持有者在做什么 |
| §4 第 5 条（批次） | 没有批次；释放把每个分支冻结在已提交的尾部 |
| §4 第 6 条（模型上下文） | 不变；卸载不写任何模型会读的内容 |
| §4 第 7 条（公开事件与服务） | Agents 使用 Session 门面（`unload_if_idle`）和自己的租约；所有者使用公开的 `SessionLifecycle` |
| §4 第 8 条（基础设施默认不变） | 一项生命周期需求（R1，§11.1），回答生命周期 §5 和事件驱动流程 §3 留下的问题；不向 rutis 提需求 |
| §4 第 9 条（Timeline 事实） | 租约、持有和标记都是进程内存，不是 Timeline 事实 |
| §1（App Server 边界） | 卸载是 Agents 里的宿主决定；能力只声明自己的工作 |
| ADR 002 D1 | 进程作用域：检查跨 Session，放在 Agents |
| ADR 002 D2 | 没有新插件；由现有的 Agents 索引运行检查 |
| ADR 002 D3 | 没有核心部件依赖可选插件；持有是任何部件都能用的运行时服务 |
| ADR 002 D4 | 没有业务配置；`G` 是宿主配置（D6） |
| ADR 002 D5 | 对客户端和其他语言没有新合同；持有是 Rust 运行时服务，有 TypeScript 所有者需要时再加 C4 绑定 |
| ADR 003 | 不适用：没有 agent 动作 |

## 11. 向机制所有者提出的需求

### 11.1 R1：运行时，Session 活动持有

状态：已实现。`SessionLifecycle::hold()` 与 `SessionHold` 在 [config.rs](../../../../crates/dim-agent-runtime/src/config.rs)，
`Session::unload_if_idle()` 按 §6.1 所示的顺序检查持有，验证在
[session_lifecycle.rs](../../../../tests/integration/tests/session_lifecycle.rs)。R2 到 R4 之前没有所有者取得持有。

| 项目 | 内容 |
| --- | --- |
| 被什么挡住 | [生命周期 §5](../../../architecture/rust-agent/lifecycle.md#5-stop-与-close) 把在途脱离工作是否阻止空闲释放、以及如何识别它留待后续设计；`unload_if_idle()` 不检查它，所有者在卸载时取消工作（§2）。[事件驱动流程 §3](../../../architecture/rust-agent/event-driven-flows.md#3-后台任务停止与自动唤醒) 把在途工作识别留到出现需要时再设计；自动空闲卸载就是这个需要。D3 要求空闲释放永不取消已接纳的工作 |
| 考虑过的现有机制 | `Session::idle` 与 Loop 空闲：按合同不包含脱离工作（S2 D10）。所有者的公开事实：ShellInput 不写，宿主读它们就是解读插件事件（第 4 条）。Agents 的租约：App Server 的类型，dim-agent-capabilities 够不到，且检查在提交锁步骤之外。`SessionLifecycle::closing()`：告诉所有者何时停止，而不是告诉运行时何时不释放 |
| 需要的通用能力 | `SessionLifecycle::hold() -> Option<SessionHold>`，Session 部件可直接取得，分支部件经 `InSession<SessionLifecycle>` 取得。Session 正在关闭或正在释放时为 `None`。释放持有即丢弃它。它不带身份，也不带业务含义。只要有任何持有，`Session::unload_if_idle()` 返回 `false`；持有检查与释放决定在同一步完成（决定之前取得的持有让释放被拒绝，之后请求的得到 `None`）。因持有而拒绝时不取提交锁。`close()`、`idle()` 和门面的其他调用不变。生命周期 §4 的“没有共享的在途计数”对执行仍然成立：持有不报告任何东西，也不取消任何东西，只否决空闲释放 |
| 对序号、批次、中断、重载的影响 | 序号与批次：无，没有事件。中断：不变；分支关闭取消该分支的脱离工作，所有者在工作结束时释放持有。重载：重载中的所有者照旧结束工作，持有随之释放。关闭：从不被持有推迟或拒绝 |
| 验证 | 用一个测试能力做运行时测试：持有期间 `unload_if_idle()` 为 `false`，释放后为 `true`；释放开始后请求的持有为 `None`；持有期间 `close()` 成功。生命周期 §4/§5 与事件驱动流程 §3 随之更新；BackgroundExec 页面随 R2 更新（§14） |
| 负责方与 issue | 运行时生命周期，核心机制负责线（eric8810，[路线图 §3.5](../../../design/roadmap.md#35-按人按批次)）；拆分之前在 [#126](https://github.com/arcships/dim/issues/126) 中跟踪 |

### 11.2 R2 到 R4：所有者取得持有

每项采用只需要 R1，只改各所有者自己的 crate。卸载检查上线之前，持有除了让显式的 `unload_if_idle()` 拒绝之外，没有可观察的效果。

| # | 所有者 | 改动 | 测试与文档 |
| --- | --- | --- | --- |
| R2 | BackgroundExec（[background.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs)） | 依赖中加入 `InSession<SessionLifecycle>`。每个任务一个持有，在 `start` 中、`exec/started` 之前取得，在监管任务提交 `exec/ended` 或启动被拒绝后释放；取不到持有（`None`）时像分支正在关闭一样拒绝启动 | `idle_unload_stops_tasks_and_a_restart_wakes_for_every_unreported_end`：任务运行期间卸载被拒绝，任务结束且被唤醒的 turn 结束后卸载成功；恢复的一半不变。BackgroundExec 页面关于卸载的那句随之只涵盖重载和关闭。`xtask graph` 与 `check` |
| R3 | ShellInput（[shell_input.rs](../../../../crates/dim-agent-capabilities/src/shell_input.rs)） | 每次运行一个持有，在第二次准入时与 spawn 和登记一起取得，运行任务结束时释放；`None` 时以 `session_closed` 拒绝运行 | 流程测试：客户端断开后 `!` 运行继续，宿主的卸载被拒绝，运行结果被记录。ShellInput §3.2、§4.1、附录 B 和 A.5 去掉“宿主的空闲卸载”这一结束运行的途径 |
| R4 | Collaboration（[collaboration.rs](../../../../crates/dim-app-server/src/collaboration.rs)） | leader 的 `CollaborationCapability` 注入 `InSession<SessionLifecycle>`；每个 `Request` 在持有成员租约的同时持有 leader 的一个持有，请求结算并移除时释放。只有 leader 正在关闭时才会得到 `None`，Collaboration 已经会结算这种情况 | 流程测试：有未结束请求、没有客户端的 leader 一直装载到成员给出结果，然后两者在 `G` 之后卸载 |

R2 已实现（[#201](https://github.com/arcships/dim/pull/201)）。
[background.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) 在 `start` 中、
`exec/started` 之前取得持有，由任务的监管任务保存，在 `exec/ended` 追加之后释放，启动被拒绝时随之释放；验证在
[local_execution.rs](../../../../tests/integration/tests/local_execution.rs)
（`idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end`，原名
`idle_unload_stops_tasks_and_a_restart_wakes_for_every_unreported_end`）。

R3 已实现（[#200](https://github.com/arcships/dim/pull/200)）。
[shell_input.rs](../../../../crates/dim-agent-capabilities/src/shell_input.rs) 在第二次准入时取得持有，
保存在运行的登记项中，运行任务在运行的最后一个事实之后清除该登记项；验证在
[transport_flow/shell_input.rs](../../../../crates/dim-app-server/tests/transport_flow/shell_input.rs)
（`a_run_outlives_its_caller_and_holds_the_session_until_recorded`）。

R4 已实现（[#202](https://github.com/arcships/dim/pull/202)）。
[collaboration.rs](../../../../crates/dim-app-server/src/collaboration.rs) 在每个 `Request` 持有成员租约的同时取得 leader
的持有，在 `collaboration/result` 追加之后、`Request` 移除时释放；正在关闭的 leader 给不出持有，请求在成员收到之前以 `closed`
拒绝。验证在 [transport_flow/collaboration.rs](../../../../crates/dim-app-server/tests/transport_flow/collaboration.rs)，用显式的
`unload_if_idle()` 调用（`open_requests_hold_their_leader_until_the_last_result`、
`every_settlement_of_a_request_releases_its_leader`、`a_closing_leader_gives_no_hold_and_refuses_the_request`）；测试一栏中
`G` 之后的卸载属于卸载检查的验收（§13 第 6 项）。

C4、SessionTitle、Scheduler 和 Workflow 不需要改动。不向 rutis 提需求。

## 12. 依赖顺序

```text
 R1 runtime hold --+--> R2 BackgroundExec hold --+
                   +--> R3 ShellInput hold ------+--> Agents unload pass (this design) --> P6 create-mode jobs
                   +--> R4 Collaboration hold ---+                                     --> P7 repeated stages
```

每一步都能单独合入。只有 R1 时，改变的只是显式调用 `unload_if_idle()` 的地方，而它们都是测试。只有 R2 到 R4 时，加入的持有还没有人据此行动。
检查最后上线，连同 D8 和配置字段一起，并且必须在 R2 到 R4 之后，因为在那之前自动卸载会取消脱离工作。工作在 P6 或 P7 进入 Ready 后开始（路线图 §2 第 5 条）。

## 13. 验收

实现 PR 的流程测试，在一次性的原生存储上进行，`idle_unload_after` 设为几百毫秒。每个测试都等待一个可观察的转变（被释放实例的
`Session::closed()`、`agent.list` 中的已装载数、某次调用的回复）并设超时，从不只靠 sleep。

1. **#126 的测量场景。** 一个测试插件经 `sessions` 启动 50 个 Session，每个运行一次输入、`idle` 和 `history` 分页，然后释放。宽限期之后全部卸载：
   `agent.list` 显示没有已装载的 Session，而 `session.list` 列出全部 50 个，其历史可以完整读取。之后对其中一个调用 `sessions.input` 会恢复它，并运行其 turn。
   实现 PR 还要给出前后的 `xtask measure` 数据、已装载数和任务数。
2. **一次运行之内不恢复。** 在一次运行中（`input`、`idle`、三页 `history`），调用之间不恢复 Session：`main` 上没有新增 `branch/resume`。
3. **卸载期间的 acquire。** 一个测试 Session 部件把自己的卸载扣住，直到测试放行，于是在释放进行中时 `sessions.history` 和 Rpc 的 `session.history`
   到来。两者都等待，然后在恢复后的 Session 上成功；都不回复 `session_closed`。
4. **卸载不是关闭。** 卸载不改变任何分支的存储头部，不写 `stop/interrupt`；恢复只增加 `branch/resume`。Session 仍在 `session.list` 中，
   Collaboration 关系仍能转交 leader 的下一个请求，C4 观察者能收到恢复后 Session 的事件。
5. **被持有的订阅。** attach 的客户端让 Session 在超过 `2G` 之后仍然装载。detach 或连接关闭之后它被卸载；重新 attach 会恢复它，事件重新开始推送。
6. **脱离工作。** BackgroundExec 任务、`!` 运行和未结束的 Collaboration 请求，各自让没有租约的 Session 在超过 `2G` 之后仍然装载。工作结束时，
   结果被记录（不是 `cancelled`），被唤醒的 turn 运行，Session 在宽限期之后卸载。
7. **待处理的审批。** 一个有待处理审批、没有租约的非交互启动 Session 一直装载到 `stop`。
8. **卸载期间的读取。** Session 卸载期间反复调用 `session.list`、`session.summaries` 以及 leader 对成员的 `agent` 读取，从不失败，也从不漏掉存储中的 Session 或事件。
9. **索引重载。** 在按第 3 项扣住释放的同时重载 Agents 索引：释放完成，没有丢失 Session，新一代卸载其余没有租约的 Session，重载之后的 `acquire` 能恢复。
10. **删除与分叉。** 对已卸载 Session 的 `sessions.delete` 删除其行而不恢复它。分叉一个已卸载的源会恢复源；宽限期之后两者都被卸载。
11. **没有原生存储。** 使用内存存储时不卸载任何 Session。

R1 到 R4 各自带有测试（§11）。

## 14. 对其他文档的改动

随本设计：

- [S2 §8.6](sessions-service.md#86-目标空闲卸载后续项)（中英文）指向本文，并注明 D2。
- [ShellInput A.5](../plugins/ShellInput.md#a5-对其他插件的改动) 与 Q5（中英文）：活动句柄作为提议重新打开（R3）；决定仍归 ShellInput 的所有者。
- [文档索引](../../../INDEX.md)链接本页。

随实现，由各所有者更新：生命周期 §4、§5 与事件驱动流程 §3（R1）；BackgroundExec 页面（R2）；ShellInput §3.2、§4.1 与附录 B（R3）；S2 §1、§8.2、§8.6 与 §15
（已装载的 Session 会被卸载）；本页状态；路线图由 zerob13 同步。
