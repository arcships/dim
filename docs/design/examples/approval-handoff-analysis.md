# Permission、Approval 与 ToolExecutor：审批交接分析

范围：分支权限状态、一个审批请求的响应竞争，以及准入结果向工具执行器的交接。
代码核对基线为 `8a5fe0980`；工作记录在 [#1629](https://github.com/arcships/dim/issues/1629)。
本页是对现行设计的分析，不替代所属契约，不规定其他 Capability 必须采用同样的形式。

后续的工具执行段模型及其余部件分析见 [逐项分析](timeline-capability-analysis.md)。

## 实际场景与边界

模型发出工具调用，Loop 写 `step/tool-call`；ToolExecutor 解析已绑定的处理器和目标，
向 Approval 请求准入。Approval 先询问 Permission：Allow 直接准入，Deny 拒绝，Ask
写审批请求并等待。此时用户可回答，权限可改变，调用也可能取消。
用户回答经 `approval/respond` 提交、发布、派发后才由 Approval 接受。

| 部件 | 它负责什么 | 本次分析的形式 | 契约 / 实现 |
| --- | --- | --- | --- |
| Permission | 当前分支策略及版本；结合命令规则、文件事实、advisor 给出决策 | 状态表与恢复等式 | [Permission](../../architecture/rust-agent/plugins/Permission.md)、[permission.rs](../../../crates/dim-agent-capabilities/src/permission.rs) |
| Approval | 问题是否仍待处理、哪个响应被接受、准入何时结束 | 状态表、转换论证、有限 TLC 模型 | 同一文档的 Approval 节、[approval.rs](../../../crates/dim-agent-capabilities/src/approval.rs) |
| ToolExecutor | 持有批次取消、已绑定处理器，取得准入后按执行次序调用并结算 | 交接条件表；不在本模型里展开执行器 | [ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md)、[tool_executor.rs](../../../crates/dim-agent-capabilities/src/tool_executor.rs) |

不在本次结论内：完整路径授权算法、所有命令规则和 advisor 组合、操作系统隔离、任意存储
故障下的清理、所有工具调度与重载路径。它们不能由这个审批模型的通过结果代替。

## 事件不能只按“请求 / 唯一终态”理解

| 事件 | 当前语义 | 对本次模型的影响 |
| --- | --- | --- |
| `permission/changed` | 策略变更；派发后生效 | 提交位置与当前生效策略分开 |
| `approval/request` | 已登记的待审批调用 | 模型从一个已登记请求开始；不证明登记前的准入 |
| `approval/respond` | 提交的用户答案，是命令；可能败给更早的答案或取消 | 允许两个调用先通过 pending 检查，再分别提交；只有一个能取走请求 |
| `approval/cancelled` | 结束问题；可表示取消，也可表示策略已允许、不再需要提问 | 不等于工具必然取消；不能据此推导工具是否启动 |
| `step/tool-call` / `step/tool-start` | 工具批次交接 / 执行流程开始 | 不把 tool-start 当成外部处理器已经执行 |
| `step/tool-completed` / `step/tool-result` | 执行完成 / 结果交接 | 归 ToolExecutor / Loop 协议，本模型不生成这些事实 |

特别地，已经接受 Allow 后仍可能因新拒绝或取消写 `approval/cancelled`。
“同一 approval_id 最多一个终态事件”不是现行契约；正确的问题是“最多一个响应能取走待处理请求”。
客户端看到 `approval.resolved` 也不能推断工具已执行，详见所属契约的 Client notifications。

## Permission 的最小状态表

`W` 为最近已登记的本地策略变更位置，`V` 为最近已派发的本地策略版本，`P` 为生效策略。
W 与 V 不能合并：两次并发 update 可能发生在第一次提交后、派发前。

| 输入 / 条件 | 转换 | 可观察结果 |
| --- | --- | --- |
| update(expected, P')，服务开放且 expected=W | 提交锁内登记 W:=新事件序号；登记等待者 | 等待派发，不立即把 P 改为 P' |
| expected≠W | 不登记变更 | Conflict 返回最新已登记版本 |
| 派发本地 permission/changed | P:=事件策略，V:=事件序号 | 释放该更新的等待者 |
| 替换或关闭 | 关闭更新入口，释放等待者 | 未完成更新不能伪装成成功 |
| 恢复 | 固定截止点上选祖先链与本分支的最后策略事实 | 祖先策略继承，但祖先序号不冒充本分支版本 |

恢复公式：令 L 为合法祖先切点和本地截止点内最后一个 `permission/changed`。
没有 L 则用 Session 初始策略；否则 P=L.policy。
若 L 属于本分支，V=L.sequence；否则 V=0；W:=V。
这与代码的 `Chain + QueryPick::Last`、只选 policy/branch_id/sequence 对应。
由于事实按序且后来的策略完整替代之前的策略，逐个考虑“无变更 / 祖先变更 / 本地变更”即可
得到这个结果，不需要执行历史事件处理器。查询失败不是“没有策略”。

这个论证依赖祖先链排序、固定切点与 Dispatcher 恢复交接契约；它没有证明查询引擎或授权算法。

## Approval 的竞争与恢复

| 状态 / 动作 | 关键条件 | 结果 |
| --- | --- | --- |
| 待处理 → 手工响应获胜 | 消费响应时仍有 pending，且调用未取消 | 移走 pending，唤醒准入调用；Allow 仍须复核当前策略 |
| 待处理 → 自动结算 | 当前决策 Deny，或 Allow 且不是 advisor 强制提问 | 先等已提交前缀派发，做异步复核；提交锁内再次确认截止位置与 pending |
| 自动复核期间又有事实提交 | 捕获位置不再紧邻新终态位置 | 放弃这次登记，重新等派发、重新决定 |
| 自动 Allow 终态已登记、未发布 | 终态尚未持久发布 | 不能返回准入成功 |
| 自动终态已发布 | 再查策略 / 规则并检查调用取消 | 只有仍为 Allow 且未取消才准入 |
| 取消或关闭时仍 pending | 同一个 pending 被移走 | 结束等待；关闭/失败 Timeline 上不能无条件承诺终态落盘 |
| 恢复遇到历史未结算请求 | 请求在截止点内没有关联 respond/cancelled | 只查询 approval_id，写 cancelled；不复活旧工具或旧提问 |

恢复选择写成集合：

```text
U(H,c) = { r.approval_id |
           r.kind = approval/request, r 在截止点 c 内，且
           c 内没有同 approval_id 的 approval/respond 或 approval/cancelled }
```

`Approval::rebuild` 的 `kind=request AND NOT EXISTS(关联 respond/cancelled)` 正好表达 U。
固定 c 分页，返回值只有 ID；恢复产生的新 cancelled 在 c 之后，不改变剩余页的选择。
每个选中 ID 都须结算，不启动原工作。这个等式说明查询选什么，不证明存储引擎、分页实现、
任意重复 ID 或中途存储失败；ID 唯一性由现有生产方契约提供。

## 有限模型具体检查什么

[ApprovalOrdering.tla](approval-model/ApprovalOrdering.tla) 从一个已提交的请求开始。
状态保留：已提交 history、持久发布位置 published、派发位置 consumed、生效策略、pending、
两个响应调用的预检查状态、自动复核捕获位置、准入阶段、取消与关闭。
两个初始状态覆盖 advisorAsk 为真或假；同一请求的身份被抽象为一个固定 ID。

| 性质 | 为什么成立 / 对应实现 |
| --- | --- |
| 最多一个响应获胜 | `consume` 在同一互斥区调用 take；pending 从有到无，没有重新加入该 ID 的转换 |
| 自动结算不能越过先提交的响应 | 自动登记要求捕获派发位置恰好是新终态前一位；若期间插入任何事件就重试；若响应已派发，pending 已无 |
| 自动准入不能早于其终态持久发布 | `admit` 等 Timeline 发布水位达到终态位置，模型分别调度 CommitAuto 与 Publish |
| 手工 Allow 不覆盖当前 Deny；自动 Allow 需要仍为 Allow | 返回前的 Permission 复核；advisor 强制提问不被自动 Allow 消解 |
| 取消前尚未成功的准入不能随后成功 | 模型在返回处检查调用取消；这是准入时的判断，不会追溯撤销已经执行的工具 |

单一 winner 的论证不要求响应事件唯一；后续响应仍可提交和被观察到，但取不到 pending。
顺序论证不依赖具体序号上限：新事实使 `captured+1=新终态位置` 不成立。
TLC 只是对这些转换及有限交错的额外检查，不是一般性证明或 Rust 正确性证明。

模型上限为 **5 条事实（包含初始请求）**、两个响应调用、三种策略；允许任意交错，
不加公平性、不证明最终会回答或最终准入。`CHECK_DEADLOCK FALSE` 是因为截断历史或结束
请求后允许无业务动作；这不是死锁自由性验证。没有 symmetry 或隐藏 state constraint。
取消/关闭的终态写入与失败被抽象掉，因此不检查它们的持久性或终态条数。
模型保留已取走 pending 后的准入阶段：关闭 Approval 本身不被假定能取消所有原生调用；
调用者取消与依赖重载的保证必须另外核对，不能从 Close 动作推出整条工具调用已结束。

运行（TLA+ 1.7.4、Java 17；工具和运行输出放仓库外）：

```sh
java -cp /absolute/path/tla2tools.jar tlc2.TLC -workers 1 \
  -metadir /tmp/dim-approval-tlc \
  -config docs/design/examples/approval-model/ApprovalOrdering.cfg \
  docs/design/examples/approval-model/ApprovalOrdering.tla
```

## 与真实实现连接

| 边界 | 代码入口 | 已有可复用流程 |
| --- | --- | --- |
| 版本登记 / 派发生效 / 恢复 | Permission::update / consume / rebuild | `updates_are_versioned_per_branch_take_effect_when_dispatched_and_survive_restore` |
| 手工响应、权限改变、advisor 提问 | Approval::admit / respond / consume | `full_access_releases_all_waiting_calls_without_user_responses`、`full_access_keeps_advisor_questions_without_repeating_advice`、`a_response_that_finds_its_batch_stopped_ends_as_an_interrupt` |
| 自动结算不能越过已提交响应 | admit 中的 Dispatcher 水位与 append_registered 复核 | `automatic_permission_settlement_cannot_overtake_a_committed_denial` |
| 自动准入等待持久发布，并处理失败 | admit 中的 Timeline 水位等待与再次 Permission::decide | `automatic_admission_waits_for_its_terminal_to_be_stored` |
| 外部执行前的交接 | ToolExecutor::admit → 分段 barrier → run | [ToolExecution](../../architecture/rust-agent/plugins/ToolExecution.md) 的并发与生命周期契约；不由本 TLC 模型覆盖 |

前两类用例在 [permission_flow.rs](../../../crates/dim-agent-capabilities/tests/permission_flow.rs)，
后两类在 [transport_flow/approval.rs](../../../crates/dim-app-server/tests/transport_flow/approval.rs)。
结果、覆盖进度和待处理问题留在 issue/PR，不在本页宣称已完成所有审计。

ToolExecutor 当前在批次开始时并发准入，随后可能等前一个执行段完成才调用处理器。
因此“准入时的策略”与“实际调用处理器时的策略”不是同一时刻；本模型只论证前者。
是否需要排队后的再次授权应由既有契约和真实场景判定，不能先把“每次开始都重查”塞进模型，
再据此判实现错误。同样，完整重载结算需要结合调用者取消、依赖级联和执行任务追踪检查。

## 规则与简化检查

本次不改变三个 branch 部件的职责、事件、服务、持久化格式或运行时。
不新增机制需求；跨模块交接仍链接所属契约。模型变量只存在于模型中，不要求生产实现增加它们。
只保留会影响上述竞争结论的三个时间位置；命令文本、文件内容和工具内部执行均不复制到模型。
Permission 的恢复在本文用直接等式解释；版本、发布与恢复的有限模型检查见 [PermissionState](capability-models/PermissionState.tla) 及其[运行记录](capability-models/results.json)。本文复用该模型，不另建副本。
