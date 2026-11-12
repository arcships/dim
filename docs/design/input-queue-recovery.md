# InputQueue：按当前输入和控制事实恢复

Status: Draft.
Scope: branch InputQueue；客户端 pending 合同不变。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
依据：[InputQueue](../architecture/rust-agent/plugins/InputQueue.md)、[状态保存边界](owner-state-indexed-reads.md)、[插件合同](plugin-contract.md)、[设计规则](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)、[rutis](rutis-usage.md)。

## 1. 决定

不再写 input-queue/recovered、input-queue/node，不保存整个 entries。原事实已经包含队列恢复需要的数据；问题在于原恢复没有直接选出当前对象。采用[成员及控制索引](appserver/timeline-state-read-indexes.md)，不新增队列 API 或私有数据库。

公开 pending 排除 handover，不能拿它的返回值恢复队列；本 owner 自己用同一 query 机制读取普通输入和 handover 两个集合。它不从 UI 获取控制状态。

## 2. 逐对象决策

| 对象 | 实际改变条件 | 重载处理；原事实 | 新增持久化 |
| --- | --- | --- | --- |
| entries | input/message 加入；consumed 移除；withdraw 仅删除尚未 released 的对象；turn/end 删除该 turn 的 handover，并释放最早 held queue | 保留尚无 consumed/deleted 的 input/message，按原位置排序；处理仍待决定的 withdraw 和失效 handover | 无。id、原位置、mode、handover.turn_id 留在原事实 |
| turn | turn/start 设置 ID；turn/end 清空 | start/end 各Last，较新的 start 表示打开 turn；较新的 end 表示无 turn。旧 turn 的实际结算归 Loop，Queue 不假造结束 | 无 |
| starting | 无 turn 时释放非 append 输入置 true；收到 turn/start 或 interrupt 清 false | 无 turn 时，当前 released 非 append 输入的 applied 位置晚于最新 start/end 和 interrupt 才为 true。它阻止第二个 queue 输入越过尚未开始的驱动 | 无 |
| released | Queue 决定释放时变 true，并写 input/applied；不会被 withdraw 改回 false | 当前 input_id 有 applied 即 true；原 mode 从 applied 取得，决定已发生，不重复写 applied | 无 |

handover 从不 applied、不计 queued；仅在所指 turn 打开时保留。其 turn 已结束或不匹配时写原 input/deleted。不能给它套 Goal 的 paused，也不能因重载删除普通待消费输入。Append 可保持 released 等待后续请求，不自动驱动新 turn。

先固定 S，并注册 Dispatcher 交接。恢复期间不把较晚来源混入 S；恢复决定 append 后须提交发布才作为本代完成。S 之后已排队的来源仍按原顺序交付；同一个 input_id 的 applied/deleted 决定通过恢复时已取得的身份去重。新的恢复输出在尾部派发，不作为重复源转移。

## 3. 具体读取

1. 普通输入：kind=input/message，handover 缺失，不存在 consumed/deleted.input_id=id。Fields(id,position,data.mode)。成员索引直接读当前集合，页128。
2. handover：同一反关联，改为 handover 存在。Fields(id,position,data.handover.turn_id)。v40 的 input-handover/v1 维护 presence=true 分组。
3. 每个非空当前成员页最多128个 input_id，一次查询 kind IN(applied,withdraw) AND data.input_id IN(本页ID)，按 kind、input_id 分组取 First，并保留所选 withdraw 的 id、position；handover 页只查 withdraw。最多256条控制字段，当页用完释放。同一输入的第一个 withdraw 决定候选，后续指令不能覆盖；applied 是该输入唯一的交接事实。当前无 applied 且已有 withdraw 的条目欠一个接受决定；已有 applied 的 withdraw 返回 notHeld，不重新 apply，也不加载候选。仅为欠下的接受决定，按所选 withdraw 身份查询可选 data.steer，恢复完整已检查的候选，不重新运行提交检查。没有候选时只写 deleted；有候选时同一原子批次先写替换 input/message、最后写原输入的 input/deleted；不同当前成员的欠下结果按 withdraw 位置排序。已提交该批次的原输入不在当前成员中，不重建替换。候选不是已采纳输入，不进入模型上下文。持久格式见 [v43 决定](appserver/native-storage-format.md#v43-conditional-queued-input-steer)。
4. start/end 各取最后一条 turn_id、position，构造 turn。存在 released 驱动且 turn 为空时才查最后 interrupt。只比较标量位置，不遍历旧 turns。
5. 已有 applied 时采用 applied.mode，缺省为 Queue，与原 Loop 一致。只有未 applied 且 input/message.mode 缺失时才对该 input_id 查询 user 的 Present/非 null，按已有 effective_mode 规则恢复。不得为判断存在性返回整个 user。SQLite 对内联 JSON 的解析成本仍计入这一个选中对象，不宣称 Fields 自动免解析。
6. 建立当前 entries 后执行原调度：已释放不重放；有效 turn 或 starting 时持有 queue；可释放时写原 applied；无效 handover 写原 deleted，有效撤回写第3步确定的删除或替换批次。

判定 withdraw 使用决定时的状态：applied 在 withdraw 之前已经提交则不可撤回；没有 applied 则 held。按序 owner 不会先写较晚释放再处理较早撤回；发现违背此生产合同的数据报错，不能用最新值掩盖。

每完成一页当前成员读取，调用已有 opening_advanced 报告真实初始化进度；单次查询仍受原超时限制，不用定时心跳掩盖阻塞。

当前输入字段不包含 user 正文、blocks、历史终态、任务句柄或回复 waiter。answers/answered/lifecycle 仍为本代内存；已接受撤回后结果发布未知沿用 publication_unknown，不能返回假 notHeld 或自动重发。

## 4. 变化检测和发布

普通实时处理直接用原 QueueState 更新内存。删除/消费只查目标 id；turn/end 只处理对应 handover 与下一 held 输入。保留原标准有序集合，不为本轮重写调度算法；当前 K 项的查找最坏 O(K)，K不包含已结束历史。queued 使用入队/消费/删除的计数变化维护，不先复制整个 entries 比较。

有 Apply/Delete 输出（包括已接受 Steer 的替换/删除原子批次）时沿用原 append_batch 和发布合同；纯 turn/starting 内存变化在该已提交源的处理完成后发布 InputSnapshot，不新增等待恢复事件的门槛。重复 turn 边界、空队列、无效 withdraw 都不写恢复结果。关闭只清理本代 waiter，不刷新快照。

InputQueue 分支服务返回已提交决定；InputMethods 经分支 facade 确认已接受 Steer 的结果发布后才返回 RPC receipt，确认失败返回 publication_unknown。notHeld 不写替换或删除，也不返回替换 receipt；不能因发布失败把已接受决定改成 notHeld。

~~~text
input/message -> 内存 entries -> 必要 input/applied
input/consumed -> 内存移除 -> queued 通知
turn/end -> 清 turn -> 有 handover/held 时写必要 deleted/applied；否则零写入
reload -> 当前成员 + 每项决定事实 + 最新控制 -> 当前内存 -> 原调度
~~~

## 5. 成本与修改

空队列正常冷读4次 query（两个当前成员组、两个 turn 边界），无正文、无写入。一个非空成员页增加一次批量控制查询，只解码本页当前 Entry；仅旧未释放输入的 mode 缺失时另查存在性。欠下的接受决定另按所选 withdraw 身份加载 data.steer，其完整候选字节和读取成本单独计入，不为此读取原 input/message 正文或扫描历史。存在 released 驱动且无打开 turn 时再加一次 interrupt。当前集合分多页时每组按128个身份分页，只有有 next 才续页；这些都是当前任务，不是已完成历史。

普通完整聊天一轮 Queue 仍写原 applied 1条；message、consumed、start/end 归原生产者。本设计额外事件为0。自动续行、配置改变、异常终止、空队列重载的恢复事件同样为0；异常后的实际删除/释放逐项按业务决定计数，不在“0快照”中抹去。

| 必要落点 | 修改 |
| --- | --- |
| input_queue.rs | 恢复标准当前集合；去掉逐源 checkpoint、恢复事件订阅和以结果位置发布的逻辑；保留撤回发布错误语义 |
| input_queue/recovery.rs | 当前成员、applied/withdraw、turn/interrupt 定位；不再八类型历史重放 |
| input_queue/entries.rs、recovery/nodes.rs | 私有持久节点不进入目标实现；原标准集合承担当前调度 |
| input_queue/pending.rs | 公共语义不变；不拿它替本 owner 恢复，也不增加参数 |
| Store | 维护已声明 handover 分组，输入属性关联命中既有 input_id 索引；细节归索引设计 |

旧 input-queue/v1/v2 结果只作为旧派生记录保留，不加载、不回填；原输入和决定事实仍是兼容来源。索引升级一次建立当前成员，普通打开不得退回历史重放。缺索引/损坏返回存储错误。

## 6. Requirements raised to mechanism owners

| owner / 问题 | 现有能力与不足 | 确定要求及位置/取消 |
| --- | --- | --- |
| Store / #1320 | input/v1 只登记 handover 缺失组；普通 pending 不能返回交接对象 | v40 增加同一关系的存在组；维持固定 S、旧版本分页、取消及事务。见索引设计 |
| Timeline / #1320 | 现有 query、原子 append、Dispatcher 交接足够 | 不增加 checkpoint 参数或查询服务；恢复输出走原尾部发布 |

## 7. 验收、Rule check 与删减

复用 queue/steer/append、handover、starting/interrupt、消费、撤回、装配恢复与 publication_unknown 流程。覆盖 applied 前后崩溃、withdraw 提交后尚无 deleted（有候选时恢复原检查结果并原子替换，不重跑检查；已提交替换不重建；notHeld 无替换/删除）、turn/end 尚未清 handover、已消费驱动不误设 starting、同一 cutoff 续页。正常输入消费在其 turn/start 之后；违反此关系不是另造恢复状态的理由。

| 规则 | 本设计 |
| --- | --- |
| principles §1；ADR002 D1 | 原 branch Capability 决定队列，Loop 决定执行 |
| ADR002 D2–D5 | 无接线层、用户配置、新服务；事实和查询语言中立 |
| §4 1–4 | 原交接/异步责任；无 before I/O，Store 不调度 |
| §4 5、6 | 保留必要 Apply/Delete 与发布规则；无上下文旁路 |
| §4 7、8 | 复用原服务；机制要求见§6 |
| §4 9 | 只保留真实释放/删除事实，不保存可直接取得的重复控制状态 |

删减审查：移除完整队列快照、节点事件、queued 的持久副本、逐源 source_through；不增加替代快照。保留成员索引是为直接取得当前未完成输入，不以保存恢复代码方便为理由。
