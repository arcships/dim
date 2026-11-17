# 动态消费注册、恢复与实时交接

Refs #1732，第三批 C。源码基线 `c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`，
独立于 #1734/#1737 的 A/B 分支。本文是实验与实现对应，不修改运行时或插件合同。

## 从实际代码拆出的边界

[插件合同 §5.2](../plugin-contract.md#52-consuming-facts)的具体入口是
[Dispatcher::consume](../../../crates/dim-agent-runtime/src/core/dispatcher.rs)：
注册时 holding=true；rebuild 返回 cut 后，反复在锁内取走暂存批次、锁外逐个 await handler；
只有锁内发现暂存为空，才切为 live。之后订阅回调跳过 covered 以内的事件，其他事件直接 await handler。
恢复读取什么由 owner 决定，订阅 keys 不是历史查询声明。

这里有三条不同的错误路径，不能建成一个“失败立即冻结所有位置”的开关：

| 路径 | 源码行为 | 模型 |
| --- | --- | --- |
| rebuild 或暂存排空中的 handler 失败 | `consume` 返回错误；holding 尚未关闭，不放行实时业务 handler；本函数不会直接调用 Dispatcher.fail | `Rebuilt(FALSE)` 或 drain `Finish(error/panic)` 停止该注册的恢复调用。panic 表示调用逃逸到外层，**不声称被 consume 捕获为 Result** |
| 实时 handler 返回普通错误 | `SubscriptionListener` 调用 fail 后返回 Ok(None)，serial 可继续当前事件的其余订阅者；当前事件游标也可能推进 | live `Finish(error)` 保存 first fault，允许剩余 Route 和 EndEvent；下一事件 Begin 被禁止 |
| 实时 handler panic | 保存归属订阅者的故障，并向 serial 返回 Err；当前派发中断，游标不推进 | live `Finish(panic)` 设置 aborted，EndEvent 保留 cursor；已有更早故障不能被覆盖 |

这些是源码核对得到的区别，不是新发现的运行时缺陷。错误之后哪些外部调用仍能观察到原错误，
还涉及 #1718/#1719 的 facade/关闭路径，本模型只检查 Dispatcher 内部 first fault 不被覆盖。

## 合同、实现、模型与前提

| 承诺 | 源码对应 | 模型/检查 | 明确的边界 |
| --- | --- | --- | --- |
| 注册前不接收，派发按当时注册快照选择 | Dispatcher.subscribe；锁定 rutis 0.19.3 的 `serial_erased` snapshot | `Register/Begin`，每次 Begin 保存 todo | 任意选择 todo 中的一个订阅者，保守覆盖订阅顺序；不证明 rutis 实现本身 |
| 已撤回的快照项在开始调用前跳过 | rutis `Entry::start/live`，`EventBus::start` | `Remove/Route` 检查 active | 已开始的调用可随后完成或因取消退出；不承诺卸载瞬间撤销已发生效果 |
| 精确/domain/*/* 过滤 | runtime `core/keys.rs::KeyFilter` | c1 精确 a/x；c2 全部；c3 a/*；`OrderedOnce` | 三种合法 key 的有限取值；非法语法由既有 flow 检查，不证明字符串解析 |
| 先注册、再捕获 cut，恢复期间暂存 | consume 的 Held 与 rebuild | `Capture/Rebuilt/Route`；`NoHandlerBeforeRebuild` | owner 捕获 cut 在当时 published..allocated 之间；查询正确性由 B/owner 提供，不使用任意过旧 cut |
| 排空可与后续派发交错，空队列切换必须原子 | consume 的 take/for/holding=false | `Take/Drain/Finish`；held 与 batch 分开，busy 保留 await 窗口 | 不在锁内等待 handler；不将整个排空过程建成一步 |
| cut 以内不重放，cut 以后按序且至多调用一次 | holding=false 的 covered 检查和暂存批次过滤 | `OrderedOnce` 检查调用序列严格递增、匹配 key 且大于 cut | calls 记录调用开始，可能随后失败或取消；不是外部效果 exactly-once |
| 完成恢复的活跃消费者没有漏掉已分发后缀 | consume 注册/排空/live 与 Dispatcher.run | 无故障且事件派发间隙，`ExactLivePrefix` 比较 calls 与 cut..cursor 上全部匹配事件 | 已移除/失败注册没有继续交付义务；不是跨代旧内存完整恢复 |
| 失败后不开始后续事件，保留最初故障 | SubscriptionListener、Dispatcher.fail/run | `NoFutureEventAfterFault/RetainsFirstFault` | 区分当前事件剩余订阅者与下一事件；不证明客户端读取错误的所有时序 |
| 删除注册不删除历史 | listener 生命周期与 store 独立 | `RemovalPreservesHistory` 为模型边界核对 | 不模拟生产插件卸载；真实持久历史证据仍是 storage_flow 的 extension-events 恢复流程 |
| consume Future 和 listener 生命周期不同 | consume 的直接 handler await 与 ctx-owned subscription | Remove 只撤 listener；DropRebuild 可终止恢复调用；已在途 Finish 仍允许完成 | 不假设撤 listener 就同步取消外部持有的 consume Future；完整插件换代和工作结算归 D |

依赖来源：[rutis 使用合同 §4–5](../rutis-usage.md#4-lifecycle-rutis-provides)，
Cargo.lock 的 rutis **0.19.3**（不是过时的绑定版本说明）。其 crate checksum 及核对的 bus 源文件哈希在运行记录中。
rutis 的 `run_serial` 顺序 await 并对 Err 提前返回，`Entry::start` 在调用前检查存活代次。
模型接受这些机制保证作为前提，没有把它们的实现算成 TLC 已证明。

cut 的充分性也属于 owner 前提：本模型只允许在注册之后捕获当时 published..allocated 内的 cut，
使注册前已经派发的事实不落入随后必须 live 交付的后缀。consume 接受 rebuild 返回的 u64，
并不自行验证它是否过旧；Wait 的当前 query cut、InputQueue 捕获 committed 后等发布是具体做法。
本模型不能推出任意第三方 rebuild 闭包都满足该前提，逐 owner 的假设闭合仍须在 D 核对。

## 有限实验

[ConsumerHandoff](capability-models/ConsumerHandoff.tla)使用三种事件 key 的全组合。
单注册/三事件配置展开多轮暂存与排空；双注册/两事件配置展开不同注册、恢复、退出时机及当前事件中的错误竞争；
三注册/单事件配置补充 exact/all/domain 的独立注册与退出。
注册 ID 表示一次注册生命期，同一 ID 不重新注册；新插件/新代使用新 ID。无限换代、重复复用 ID 不由此模型证明。

允许事件在注册前已经派发，恢复可覆盖尚未发布尾部；无需枚举静态消费者名单。
没有要求每个新消费者重新消费全部历史。模型只用 cut 表示 owner 声明已经覆盖的范围，
不把历史状态重建假设成重跑 handler，也不恢复旧 I/O 工作。

没有公平性或活性结论；handler、恢复 Future、存储发布可以等待。
`ExactLivePrefix` 检查已经完成恢复且没有当前派发时的完整后缀，不代表每个注册最终都能进入 live。
无状态过滤剪掉错误或移除路径。单次注册内不重复，不推出多个注册之间的外部效果去重。

## 实现证据的修正

本批核对发现，既有 [timeline_tail](../../../tests/integration/tests/timeline_tail.rs) 的 TailPart
手写了 subscribe、staged 队列和 covered 过滤，而没有调用 Dispatcher.consume。
原测试验证了查询/tail 交接思路，不能直接作为 consume 实现的针对性证据。

本 PR 将该流程改为实际 `consume`，删除测试自己的 staged/read 状态；rebuild callback 做原来的精准查询，
live callback 只记录所处理序号。保留未发布尾部/无尾部两个场景及全部原有序号、cut 和恰好一次断言。
rebuild 入口先发注册完成信号，测试收到后才发布输入；无尾部场景等待 Dispatcher idle 后才放行查询，
因而确定覆盖已暂存事件的 cut 过滤。注册/完成信号等待都有十秒上限；任务 panic 不再留下无界等待。
不改运行时，不放宽业务断言，不重试失败。

[dispatch_flow](../../../crates/dim-agent-capabilities/tests/dispatch_flow.rs)已有 panic 归属及游标冻结、domain wildcard 流程；
动态工具装配、producer 卸载后历史读取沿用既有 tool_binding/storage_flow 证据。
新增真实消费交接流程，直接使用 branch 的 Timeline、Dispatcher 和 rutis 注册生命周期：

| 流程 | 对应模型窗口 | 实际断言 |
| --- | --- | --- |
| `consume_drains_new_arrivals_and_reregisters_without_repeating_history` | rebuild hold、drain await/new arrival、remove/register | 注册后查询取 cut；首个 held handler 等待时再次写入并确认 Dispatcher 已暂存；排空后再测 live；注销期间的事实由新注册精准查询恢复，旧注册不再处理，恢复与 live 合并序号无重复/遗漏 |
| `consume_rebuild_and_drain_failures_stay_local_until_the_owner_is_removed` | rebuild error、drain error | 两种失败返回调用方，后续事实不放行给失败消费者；Dispatcher 无全局 fault 且继续推进；owner 注销后仍无回调 |
| `consume_live_error_finishes_the_current_fact_and_stops_its_suffix` | ordinary handler Err、current serial completion | 同批两事实的第一项报错，另一个健康 consumer 仍收到当前事实；cursor 停在第一项，两者都不处理后缀 |

等待由注册、handler 入口与 Dispatcher 位置握手驱动，均设十秒上限。实时错误用原子批次固定已有后缀，避免错误与第二次 append 竞争。
这些测试覆盖真实注册作用域的撤销/重建，不等于 PartRegistry 全套动态安装或业务 owner 的恢复算法已验证。
`RetainsFirstFault` 和 `RemovalPreservesHistory` 直接来自动作定义，属于自洽检查/边界声明，
不当作额外实现证明。
新增流程不改变模型动作或前提，复用原三组 TLC 结果；并不声称所有模型交错都已在 Rust 中穷举。

## 消融与结果

保留一个模型和三种有限配置。held、batch、busy 分别对应暂存、锁外处理批次和 await 中的调用，
合并会删掉实际窗口；current/todo/busBusy 保留一次 serial 派发，不把不同订阅者当作独立总线。
不另建注册表框架、故障平台或生产状态。测试反而删除了一份交接实现，复用真正的 owner API。

运行结果、关键可达窗口、源码哈希和复现命令见[本批记录](capability-models/consumer-handoff-results.md)。
#1732 的 D、完整 runtime 精化及 facade 故障传播不因本批通过而完成。

补充流程的消融审查：复用现有测试文件和真实服务，不增加生产抽象；保留注册作用域、通知握手与有界等待，分别用于验证注销效果、强制交错窗口和避免故障悬挂。未增加重复暂存队列或恢复算法。
