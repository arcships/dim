# Prompt：只保存有效条目引用和采用版本

Status: Draft.
Scope: branch Prompt 状态、采用与恢复。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
依据：[Prompt](plugins/Prompt.md)、[模型配置](model-configuration.md)、[数组下标](timeline-array-rows.md)、[保存边界](owner-state-indexed-reads.md)、[设计规则](../architecture/rust-agent/principles.md)、[ADR002](../adr/002-plugin-scope.md)、[插件合同](plugin-contract.md)、[rutis](rutis-usage.md)。

## 1. 决定与实际对象

只保存未来采用需要的有效条目引用和版本。turns、tasks、configuration_event 不随每个生命周期事件写入恢复记录；原事实已表达它们。scope 的结束不等于 Goal paused：Prompt 移除该 scope 的条目，写原必要退役输入；未结束 scope 是否结算由该 scope 的 owner 决定。

| 对象 | 什么事件与条件改变 | 重载决策；已有表达 | 新增保存 |
| --- | --- | --- | --- |
| 有效条目引用 | prompt/changed 的 Set 仅在 scope 活动时替换 `(source,scope)`；Revoke 移除；scope 结束移除其条目 | 保留当前引用；已结束 scope 的旧条目作废，scope 再次 started 不复活旧 Set | 条目集实际改变时保存引用；body 仍在原 Set.entry |
| turns | turn/start 加 ID；turn/end 移除；branch/start 清理 | 重新取得引用或本次配置目标涉及的 turn 的 start/end，以及 branch/start 重置边界 | 无；不存所有历史/当前 turn 集合 |
| tasks | context/scope.started 加完整 scope；ended 移除 | 重新取得需要的 scope 最近 started/ended；以结束位置判断旧 Set 是否作废 | 无；不存整个 tasks 集合 |
| revision | 每个真实 prompt/changed 都采用其 timeline/revision，包括仅 Revoke、甚至 Revoke 的键已不在有效集合 | 必须保留；不能用存活条目的最大版本替代 | 随同该次采用引用结果 |
| system_revision | 变化涉及原 System 条目，或 Set 的新条目 delivery=System，采用该次版本 | 必须保留原规则；System 只允许 branch scope | 随同引用结果；不从剩余条目推导 |
| configuration_event | 原 context/configured 改变当前配置 | 每次冷读直接取最新 context/configured，重用其 settings/system/system_source | 无重复引用；不为配置本身写 Prompt 结果 |

本代的 PromptSnapshot 仍是当前内存状态。turn/task 的活动信息从已交付事实维护；冷恢复仅填充当前条目涉及的 scope。采用一个此前未涉及的 scope 时，在原采用任务内先查询该 scope 的当前边界，随后并入同一内存状态。不能为了重建完整 tasks 集合枚举所有历史 scope。

## 2. 保存字段与写入时机

`prompt-state/recovered` 使用新 schema `prompt-state/v2`，字段只有：

- `schema`。
- `entries`：每项 `{source,scope,event_id,element_index}`，指向当前有效 Set 的原下标。
- `revision`、`system_revision`：原 PromptReference 或 null。
- `changed_through`：最后已经纳入本份引用结果的prompt/changed源位置（branch_id、sequence；没有采用时为null）；只用于补齐尚未保存的真实提示变更。它不因turn、配置或处理回调而前进。

不保存通用 source_through、turns、tasks、configuration_event、System 拼接副本、历史 changes、加载任务/句柄。已提交 v1 可直接取得相同引用和版本，忽略其冗余运行字段；不因读取 v1 补写 v2。未知 schema 报错，不筛掉后冒用旧记录。

prompt/changed及其必要输入/配置保留原采用批次。Prompt处理实际提交、完成before变换的prompt/changed后，更新引用并保存一份结果。不能预先按变换前草稿生成引用：现有append_batch_registered不提供在before之后再生成一条派生草稿的能力，本设计不为这个缓存改变Timeline。

源提交到结果提交之间的崩溃，通过changed_through之后的prompt/changed类型/位置范围查询补齐；只处理未纳入的提示采用，不重新生成已在原批次提交的模型可见输入，不读取中间turn/scope历史。这个位置有明确重载用途，不是逐回调完整快照要求。

scope 结束时，从内存按 scope 定位受影响的键。只有确实移除了引用才写 `[必要退役输入, 最终引用结果]`；没有条目则零写入。提交回调核对 before 处理后的退役输入及完整引用结果与本次生成的数据一致；任一项被改变则拒绝整个批次，保留已提交源事实供重载重新结算。通过核对后，同批提交使重复恢复不会重复退役。scope 结束不伪造 prompt/changed 版本，revision/system_revision 沿用原语义。

同值刷新在 `Prompt::consume_result` 比较本次加载的目标条目，发现没有 changes 则不生成 prompt/changed 或引用结果；只改变模型/tools 配置也不写引用结果。不要先 clone 整个 Snapshot 或遍历全部键来检测“无变化”。变化键的旧/新条目和 scope 退役项足以生成 context_changes；System 变化才重排/拼接当前 System 条目。

## 3. 冷读和生命周期判断

固定读头 S，注册原 Dispatcher 交接：

1. 读取最新引用结果和最新 context/configured，各1次 query。没有引用且没有历史 prompt/changed 是空状态，零回填。
2. 先补changed_through之后的真实prompt/changed；每条新变更按其源位置取得目标scope的活动性，引用结果按实际提交内容生成。之后按当前引用的完整 scope 分组。branch scope 保留；turn scope 查询该 ID 的最近 start/end，并检查相关 branch/start 是否晚于 Set；task scope 查询该完整 scope 的最近 started/ended。
3. 某 scope 的 ended 或 branch/start 重置位置晚于 Set，旧 Set 永久失效，即使同 ID 后来再次 started；不得复活。仍有效的引用保留。新采用的活动性则看 started/end 中较新者，不能把旧 Set 的有效性判断与“现在能否采用新 Set”混为一个条件。
4. 当前 K 个有效引用分组到 G 个原 changed 事件，沿用 `Fields(data.changes[原下标].entry)`，每个事件1次 query。同一事件的多个下标合并，不取其它元素。外置 changes 直接按 locator 读取；内联小数组解析量按原格式4KiB阈值计。
5. 发现尚未处理的 scope 结束，按原规则完成必要退役并同批保存引用。普通已结算冷读不写结果。新提交来源按原 Dispatcher 顺序交接。

不能查询 source_through 之后的全部 turn/scope 历史；需要的是已知 scope 的最近边界。两个 scope 的 started/ended 查找必须使用[完整 scope 索引](appserver/timeline-state-read-indexes.md)，不是 `kind=context/scope LIMIT1` 后逐条筛选。related branch/start 查询按分支类型/位置定位，同一分支只查一次并复用标量。

turns/tasks 不是异步 load、Pending、TaskTracker。后者在重载时取消/丢弃，不重启旧加载；新 opening 按原 InitialPromptConfiguration 重新加载当前源，与已采用条目逐目标比较。旧已采用正文在新结果成功提交前仍有效，加载不等于采用。异常 scope 的 owner 写出 ended 后，Prompt 消费其结束，不自行猜测任务已失败。

## 4. 采用入口和发布

~~~text
本次加载结果 -> 按目标键比较 -> changes 空？ -> unchanged，零写入
                             -> 非空：[changed + 输入/配置 + 引用结果]
提交完成 -> 同一内存 Snapshot 发布 -> 后续回调按已采用事件身份去重

scope end -> 该 scope 有条目？ -> 无：仅更新内存
                             -> 有：[退役输入 + 引用结果] -> 发布
~~~

保留原串行采用、取消、提交注册检查、closing 和 opening 位置语义。ConfigurationPublisher 接收 Prompt 当前已采用状态，做真实配置转换；不自己恢复历史，不反向依赖 Prompt StateCell。Dispatcher 尚未启动的装配采用仍同步应用其实际提交事实，不能等待尚未启动的派发。并发加载结束后只能按当前目标有效性采用，不能拿旧 Snapshot 覆盖新状态。

scope 结束/interrupt 的 handler 及时取消受影响加载。scope 退役需要等待的工作在现有采用任务内，handler 不等待数据库。普通 turn 生命周期更新没有引用保存，StateCell 不以恢复事件的提交位置作为发布门槛。

## 5. 查询量、计算量、事件数

单分支有 K 个有效条目、分布于 G 个 changed 事件；这些引用涉及 T 个 turn scope、A 个 task scope、R 个重置分支。正常恢复为 `3 + G + 2T + 2A + R` 次逻辑 query。前2次是引用与配置，另1次是提示变更后缀检查；它有明确位置下界。G次元素查询解码K个 PromptEntry；每个生命周期查询至多返回一个标量边界。无scope、一个branch提示时为4次查询；提示正文重新加载1份，不遍历旧 turns 或 changed。旧 v1 的格式边界解码可能仍读其较大旧记录，单列兼容成本。

普通聊天/Goal 自动执行一轮、没有 turn/task 作用域提示：引用结果0，turn start/end 只更新内存。一个 Context/State 提示变化：原 changed1+必要输入1+引用结果1；只改变 model/tools：原 configured1，Prompt 结果0。一个有效 scope 提示退役：原退役输入1+引用结果1；scope 没条目：0。System 提示变化需要原 configured1，引用结果仍只1份。

采用比较只访问本次目标的已有值；scope 结束只访问该 scope 当前引用；保存时才序列化K条引用，不复制K份 body。恢复只解码有效内容，System 在确实需要重建时拼接一次。

## 6. 必要落点与 Requirements raised to mechanism owners

| 位置 | 修改 |
| --- | --- |
| prompt/recovery.rs | v2引用格式与读取、scope 定位；去掉逐源 checkpoint、turn/task 持久化、配置引用重复保存 |
| prompt.rs / prompt_state.rs | 当前键/作用域的变更结果；原采用批次合并引用；复用本次变更生成必要输入，避免整集 clone/比较 |
| context_preparation.rs 的 ConfigurationPublisher | 使用显式已采用状态；原采用批次仍保存真实配置/输入；引用结果在实际事实处理后保存，不依据变换前草稿 |
| Store query/格式 | 现有数字下标直接寻址；完整 scope/turn 键的范围查询；不解释 Prompt 活动规则 |

| owner / 问题 | 复用与确定要求 | 位置/取消/重载 |
| --- | --- | --- |
| Store / #1320 | 现有属性过滤能表达条件，但没有完整 scope 与 turn_id 的匹配索引；补通用字段索引和执行匹配 | 固定祖先切点；末条边界直接定位；v40格式及计划归索引设计 |
| Timeline / #1320 | append_batch/预分配 ID/Fields 已足够 | 不改序号、before或关闭；引用只指实际提交事实，不保存内容哈希 |

## 7. 验收、Rule check 与删减

覆盖同事件 Set/Revoke 顺序、仅 Revoke 的 revision、scope 结束再启动不复活、branch/start重置、同值刷新零写入、仅模型配置变化零引用写入、采用后引用保存前崩溃、scope 退役崩溃、fork、装配期采用与并发 load/unload。现有 v1 与没有结果的旧数据明确初始化一次；旧历史初始化不属于正常读取，也不在 getter 静默执行。

| 规则 | 本设计 |
| --- | --- |
| principles §1；ADR002 D1 | 原 branch Prompt 拥有有效条目；Session guide 归原 owner |
| ADR002 D2–D5 | 无接线层、新用户配置或加载器约定；引用语言中立 |
| §4 1–4 | load/adopt/交接不变；无 before I/O，SQL不解释生命周期 |
| §4 5、6 | 真实采用/输入原批次不变；退役与引用同批，采用后的引用由changed_through闭环；无上下文旁路 |
| §4 7、8 | 声明原依赖；机制要求见§6 |
| §4 9 | 只保存未来采用需要的当前引用和真实版本 |

删减：去掉 turns/tasks/configuration_event/source_through 的持久副本、无条目 turn/end 的全量 clone、逐回调恢复结果、五处历史重建。不引入 QueryRows、Reader、全文缓存或任务恢复。
