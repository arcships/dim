# Timeline：业务状态、运行状态与恢复读取

Status: Draft.
Scope: #1320 中 Goal、InputQueue、Prompt、ContextUsage 的保存边界与跨模块流程。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
依据：[查询合同](timeline-property-query.md)、[插件合同](plugin-contract.md)、[ADR 023](../adr/023-context-usage.md)。

## 1. 决定

**处理了一个源事件，不要求保存一次恢复结果。** 处理位置可以只在内存前进。是否保存取决于后续业务判断需要什么、原事实能否直接查询，以及不保存是否会使正常重载重复计算正文。没有统一 owner 快照协议。

| owner | 必须保留 | 内存或重新取得 | 新增保存 |
| --- | --- | --- | --- |
| [Goal](plugins/Goal-state-read.md) | 完整目标与真实业务状态 | 执行计数、轮次、评估等待；重载未结束状态映射为 paused | 仅真实业务变化的 goal/state；无运行快照 |
| [InputQueue](input-queue-recovery.md) | 原 input/message、applied、consumed、deleted、withdraw、turn 与 interrupt | 当前 entries、turn、starting、released 由索引查询这些事实取得 | 无队列恢复事件 |
| [Prompt](prompt-state-read.md) | 有效条目引用、revision、system_revision | turns/tasks 查询生命周期事实；配置查最后 context/configured | 采用版本或有效条目改变时一份引用结果；无生命周期进度快照 |
| [ContextUsage](plugins/ContextUsage.md) | 算法、已经算出的贡献累计值、压缩扣除值、适用 provider 基准 | 未结算输出标记、动态工具快照、实时请求关联 | 实际统计依赖改变时一份数字结果；无效果的内容源尾部在 turn/end 合并一份小型处理位置缓存 |

最后一项位置缓存有明确用途：避免重载再次逐条检查已经确认不产生消息的失败输出等来源。它只保存结果 ID 和已完成源位置，不复制数字结果；不因 delta、模型启动、重复 getter 或无关事件写入。它不是其它 owner 的要求。业务数值不变但上下文身份、压缩边界或 provider 基准改变，仍属于真实统计状态改变。

原事实足以点查时，优先点查，不把已经存在的字段再写一份。只有统计数字和 Prompt 当前引用不能从一个原事实直接取得，才保存派生结果。缓存不是必需业务事实；不能替代 input/applied、删除、退役输入、模型/工具终态等必要输出。

## 2. API、提交和读取位置

~~~text
业务 owner -> 自己的 timeline.query / append_batch
           -> Timeline 固定分支、祖先切点、through 与发布边界
           -> Store 执行已有 Fields / Exists / 位置查询

外部调用者 -> Session/Branch 原公开方法 -> 状态所有者
~~~

不增加 Reader、通用状态 CRUD、插件 SQL 或第二个恢复协调器。QuerySpec 与公开 RPC 保持兼容；存储索引匹配的具体不足见[索引设计](appserver/timeline-state-read-indexes.md)。该文档同时列出现有代码不满足的查询形状，不能把目标设计当作现有能力。

原事实和同一 owner 必需输出保持原原子批次。Prompt采用保留原changed/配置/输入批次；引用依据实际提交事实保存，崩溃时仅补changed_through之后的真实提示变更；退役输入与引用结果同批。Usage 是异步消费者，数字提交后才发布成功结果；保存失败停止统计，不返回未提交数字。InputQueue 不再等待一个并无业务意义的恢复事件才发布 queued。

各 owner 的内存交付位置不共用。Usage 的 source_through 只证明已完成统计和必要保存的源前缀，不是结果事件自身的位置；Prompt 不使用它驱动逐源恢复。已知 ID 的正文点查仍使用业务可见范围，不能被源后缀下界截掉。

## 3. 短时序

### 3.1 普通聊天一轮

~~~text
用户 -> input/message -> Queue -> input/applied
Loop -> turn/start -> input/consumed -> Usage 计入新输入 -> 数字结果
请求 -> budget / prepare / call -> model-start -> 输出 -> usage / output-end
Usage -> 复用实际输入量；有效 provider 基准、新输出各保存其变化后的数字
Loop -> turn/end -> Queue 清 turn；Prompt 清内存 turn
队列无剩余、提示无变更 -> 两者都不写恢复事件
~~~

### 3.2 提示变化和中断

~~~text
配置采用 -> Prompt 比较本次加载目标 -> 有真实 changes？
           无：零写入
           有：[prompt/changed，必要上下文输入/配置] 同批提交 -> 处理实际事实 -> 引用结果
scope 结束 -> 只找该 scope 的有效条目 -> 有：退役输入+引用结果；无：零写入

中断 -> 资源 owner 结算输出/任务 -> 原终态
     -> Goal 写 paused；Queue/Prompt 不写运行快照
     -> Usage 只结算保留内容，不为 interrupt 另写统计
~~~

### 3.3 重载

~~~text
Goal  : 最新完整目标状态 -> 未结束则内存 paused
Queue : 当前普通输入/交接输入 -> applied/withdraw 点查 -> turn 边界 -> 原调度
Prompt: 最新引用 -> 当前 scope 边界 -> 有效条目下标点读；配置独立读取
Usage : 最新数字+处理位置缓存 -> 仅未处理内容源 -> 发布当前统计
~~~

活实例重新连接不执行这些重载流程。异常资源的终态由启动它的 owner 写入，不能把所有插件机械改成 Goal 的 paused。

## 4. 真实流程的事件数量

计数采用以下明确流程：已有配置、无工具调用/压缩、一个输入、一次模型调用、一次最终 model/usage 和 output-end、无提示或工具集合变化。模型输出的 text/thinking/content 事实数量为 D，按原流式协议产生，不人为固定。下表只计本次状态设计涉及的 owner；budget、prepare、call 等原事实照常存在，完整清单见 Usage 设计。

| 流程 | 原有必要事实 | 额外保存事件 |
| --- | --- | --- |
| 普通聊天一轮 | input/message、applied、consumed、turn/start、turn/end 各1；model-start、usage、output-end 各1；budget、prepare、call 各1；输出 D 条，共11+D | Queue 0、Prompt 0、Goal 0；Usage 输入结果1、有效 provider 基准1、输出结果1，共3；无有效或无变化基准时2 |
| Goal 已 active，自动执行一轮并继续 | 与上行相同，其中 input/message 是原必要续行输入 | Queue/Prompt/Goal 运行快照均0；Usage 同上 |
| 本轮实际判断完成/暂停 Goal | 原模型/工具/turn 终态不变；原必要模型可见状态输入保留 | goal/state 1；运行快照0；Usage 按真正采用的状态输入计入，不把未采用输入提前计入 |
| 仅模型或工具配置改变 | context/configured 1 | Queue 0、Prompt 0；Usage 固定贡献/限制/适用基准改变时1 |
| 采用一个变化的提示条目 | prompt/changed 1、该条目必要上下文输入1；System 变化则改为原 context/configured 1 | Prompt 引用结果1；Usage 配置变化或输入真正 consumed 时各按实际来源保存；不额外按 Prompt 回调保存 |
| 同值配置或同值提示刷新 | 原采用比较返回 unchanged，0 | 全部0 |
| 空输出失败并结束 turn，没有新的有效内容 | 原失败 output-end、turn/end；Goal 正在执行则原 paused 业务输出 | Usage 数字结果0；若存在尚未保存的无效果内容源，处理位置缓存1；Queue/Prompt 恢复事件0 |
| 正常已结算分支重载 | 无新业务事实 | 全部0；动态配置采用有真实变化时另按配置流程计数 |
| 硬退出后重载 | 不能补写退出前事实；资源 owner 按原合同补缺失终态 | Goal 内存 paused、Goal/Queue/Prompt 恢复快照0；Usage 仅为新结算内容或最后未完成统计工作保存 |

Usage 每个真正改变累计值或基准的来源保存一份数字；同源多条消息合并，最后一份即当前结果。不同来源保留各自边界，不等待未来 usage。普通轮输出若确实没有 model/usage，原事件数减1，统计只保存输入与输出2份。

## 5. 查询、计算与验收

| owner | 正常冷读对象和逻辑 query 数（单分支） | 解码与计算 |
| --- | --- | --- |
| Goal | 最新业务状态1；兼容及删除修复另按其设计 | 一份目标；不计算旧进度 |
| Queue，当前为空 | 两个当前成员组+start/end 各Last，共4 | 至多2个 turn 标量；无输入正文、无 entries 序列化 |
| Queue，当前有输入 | 两个成员组的页数+2次 turn 边界；每个非空成员页增加1次批量 applied/withdraw 查询；有 released 驱动且无 turn 时加1次 interrupt；仅未 applied 且 mode 缺失的旧输入另查 user 存在性 | 只解码当前控制字段；不解码 user/blocks；必要释放/删除按原状态机 |
| Prompt | 引用1、配置1、未保存提示变更检查1；引用所在 G 个原 changed 事件为G；每个引用涉及的 turn/task scope 两次状态边界，另查相关 branch/start 重置边界 | K个有效 PromptEntry；只处理本次发生变化的键/结束 scope；不重放聊天轮次 |
| Usage，无未完成内容源 | 数字1、位置缓存1、counter 配置字段1、六类源的合并后缀查询1，共4；未显式配置 tools 时首次取得当前集合另点读配置 system 1次 | 一份数字状态和配置控制字段；无后缀时0旧消息、0消息计数；动态工具首次或身份变化时计算固定贡献，后续复用 |
| Usage，有 U 个未处理内容源 | 上行定位后只读 source_through 之后的对应源；按源的实际字段/正文点查 | 计算真实尚未完成的 U 个来源；不重算已保存前缀 |

这些是逻辑 query 数，不是 SQLite statement 数；一个类型 IN 可拆多个索引范围，一次元素查询也可能访问 locator 与内容表。Query 的范围、索引缺口和 SQL 验证由[索引设计](appserver/timeline-state-read-indexes.md)规定。输入当前成员仍须读完当前集合，不能把它称为整段历史读取。

验收使用上述实际流程逐事件登记写入；统计 SQL statement、候选访问、VM_STEP、FULLSCAN_STEP、SORT、AUTOINDEX、解码字节、计数调用、WAL。额外增加已结束聊天历史只用于检验扫描独立性，不用“重复1000次”推导产品收益。空队列、同值提示、空输出失败、重复 provider 数字必须各有流程证据。

删减：取消统一 source_through/逐事件完整结果；取消队列恢复结果；Prompt 不保存 turns/tasks/configuration_event；Usage 不再同时写 boundary 和完整结果、不保存请求正文或逐源树节点。唯一保留的无数值变化保存是有明确重载用途的合并处理位置缓存，其字段和触发条件由 Usage 独占。

## 6. Native restore and initial statistics

Native branches reads only configuration and branch metadata; no full-history admission scan or validation cache remains. Export reads and decodes its output once and omits end on failure; see [restore boundary](appserver/restore-validation.md).

Usage reuses v1–v3 saved aggregates and v4 numeric results; v5 explicitly preserves unavailable legacy message counts instead of recounting history to fill auxiliary metadata. No legacy-prefix backfill occurs at upgrade. If no statistics ever existed, resume reports usage as not yet initialized. The first actual model request supplies its already acquired projection; Usage counts those entries once and saves one current total, without another history read. Missing pre-initialization numbers are calculated only for an actual compaction that needs them, never for normal resume; see [Usage §6](plugins/ContextUsage.md#6-恢复切点和查询范围).
