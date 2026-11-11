# Resume：状态、查询与显示的最终设计

Status: Draft.
Scope: #1320 的跨模块读取、业务恢复和客户端打开流程。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)、[#1147](https://github.com/arcships/dim/issues/1147)。

本文件规定本轮修改边界及模块关系，具体字段和算法由各 owner 文档唯一规定。目标是消除正常状态读取和客户端首屏对已处理历史的重算。InputQueue直接查询当前输入；Prompt保存有效引用与采用版本；ContextUsage保存已算出的数字。Goal恢复目标定义，未结束执行在内存中暂停，不恢复运行进度。不改模型请求构造、压缩质量或迁移内容。

## 1. 决策表

| 对象 | 采用的设计 | 明确排除 |
| --- | --- | --- |
| ContextUsage | 独立branch owner；真实内容/配置/基准变化保存数字；无效果尾部只合并小位置缓存；既有预算/校验复用已算数值。 | 独立boundary、私有数字树、producer confirm/discard、请求基准事实/正文缓存。 |
| InputQueue | 当前普通/交接输入成员 + 每页ID批量applied/withdraw + 最新turn/interrupt；原必要决定事实。 | 完整队列恢复结果、节点事件、持久queued/starting副本、历史八类型重放。 |
| Goal | 只保存完整业务状态；运行期只改内存，重载将 active 映射为内存 paused，显式 set/resume 才执行。 | goal/recovered、运行状态/集合持久化、每输入边界、旧轮次/计数/评估恢复、get 扫历史。 |
| Prompt | 仅有效引用与revision/system_revision；实际采用后保存引用；退役同批保存；scope边界按键点读，配置独立取得。 | turns/tasks/configuration_event/source_through持久副本、无条目生命周期快照、QueryRows。 |
| Store pending | 保留按位置排序的版本成员树；同一 through 读取当前成员。 | 仅存当前集合、扫描所有 origin 排除旧 terminal、插件访问 SQL。 |
| ID 与字段定位 | v40输出locator、五种恢复描述、handover存在组及实际使用的turn/scope/源位置键。 | 必须补kind才能点查、任意JSON索引注册、owner状态数据库。 |
| 显示 | 有界窗口、完成压缩摘要、一次类型化 detail。 | 每块重读完整详情、文本拼接协议、服务端详情缓存。 |
| 校验 | 保留现有完整性检查与校验复用，分阶段计量。 | 延迟/抽样校验、正文缓存、借扫描成本重写业务。 |
| 请求存储前缀 | 沿用已发布请求存储合同。 | 本轮新增的冷前缀重哈希优化及请求链重构。 |

各设计为确定方案，不列实现者自行挑选的替代结构。表中 O(K)、旧格式兼容和删除修复是各自明确成本，不是正常历史遍历的备用路径。

## 2. 最终 API 与模块关系

以下为目标调用关系，当前装配仍以生成图为准：

~~~text
CLI / ACP / Web
  |-> session.goal.get               -> Goal 当前内存状态
  |-> context.usage                  -> Usage 已完成的统计结果
  |-> 原 *.pending / turn.get         -> 业务方法组织既有 query
  |-> history.display                -> App Server 显示选择
  +-> history.detail                 -> 按身份读取一个展开对象

需要事实的业务 owner / App Server
  -> 活分支：Session/Branch 门面或本部件的 timeline
     结束分支：TreeCommands.query
  -> Timeline 建立固定范围与祖先截止
  -> Store.query 按索引执行，返回选定字段

业务变化 -> 原 append_batch
         -> 原事实、必要输出/恢复结果
         -> Store 同事务维护索引
         -> 提交发布 -> Dispatcher -> 各 owner
~~~

| 层 | 负责 |
| --- | --- |
| 业务 owner | 定义当前状态、源事实、字段、转移、保存及恢复；拥有自己的任务。 |
| Timeline / TreeCommands | 固定分支、祖先、through、发布尾部、权限及取消范围。TreeCommands 仍只支持 branch。 |
| Store | 用既有 query 执行索引定位、字段投影和通用关联；维护存储格式。 |
| 客户端 | 窗口、展开视图、请求调度；采用 owner inventories，不从显示历史重建控制状态。 |

保留 TimelineQuery、QuerySpec、Fields、QueryRow、QueryValue、原 pending/get 方法。**不新增 query 字段、QueryRows 或状态 CRUD。** Prompt 使用原有数字下标路径；只改其 Store 执行和数组定位。唯一新增业务读取服务是 ADR 023 决定的内部 contextUsage.read；显示协议由 App Server 自己定义。

插件不取得 Store 句柄、表名或索引名，不自行写数据库。未打开但仍活动的分支按现有 Session 获取流程打开，不能套用结束分支快读。

## 3. 正常恢复和实时处理

不再统一要求owner每处理源事件就保存完整结果。内存位置前进和持久化触发分别由业务设计决定，字段/事件数唯一见[保存边界](owner-state-indexed-reads.md)。

~~~text
Goal  -> 最新完整业务状态 -> 未结束在内存paused；运行计数不恢复
Queue -> 当前输入成员 -> 每项决定事实 -> 最新控制 -> 原内存调度
Prompt-> 最新有效引用 -> 指定scope边界 -> 有效Set下标；配置独立读
Usage -> 最新数字/小位置缓存 -> 仅真正尚未处理来源 -> 数字提交后发布
~~~

普通已就绪getter直接用owner当前内存；固定本次目标，等待真实工作交接，不等待并不存在的“每回调恢复事件”。usage不以新总量冒充旧through。查询取消不撤销已接受业务工作。Goal重载暂停不传播到其它owner；Loop/Tool/Task等仍结算自己的资源。

### 3.1 必要对象和成本

| 对象 | 正常单分支冷读 | 新增写入 |
| --- | --- | --- |
| [Usage](plugins/ContextUsage.md) | 数字/位置缓存/计数控制各1+六类源合并后缀1，共4 query；未指定tools时首次另点读system；无旧消息解码 | 每个实际内容源一份numeric（同时是累计边界），配置/基准变化按实际来源保存；仅无效果尾部交接保存小covered |
| [队列](input-queue-recovery.md) | 空队列4query；有队列只增加当前对象的applied/withdraw及必要默认mode、interrupt定位 | 恢复事件0；仅原applied/deleted |
| [Goal](plugins/Goal-state-read.md) | 新格式普通加载1条；兼容/删除修复单列；活get零SQL | 仅真实goal/state；运行进度和重载均0 |
| [Prompt](prompt-state-read.md) | 引用1+配置1+提示变更后缀检查1+G个有效正文事件+已知scope键定位 | 真实采用/退役每批1份引用；普通无提示变化的turn0 |
| [pending](appserver/timeline-member-index.md) | 固定through根、当前成员与本页字段 | 原事实事务维护成员路径；不复制整个当前队列 |

表中为逻辑query，不是statement计数。SQL表达式、索引键、当前未匹配条件和额外扫描范围见[状态字段索引](appserver/timeline-state-read-indexes.md)。源码拥有的原有能力与目标新增执行必须分开验证；LIMIT1不是性能证据。

## 4. 初始化、删除和错误

| 情况 | 确定处理 |
| --- | --- |
| 旧数据/缺少派生结果 | Queue直接用原事实和升级后的成员索引，不写初始快照；Prompt已知v1可取原引用，缺引用时明确一次初始化；Usage旧v1–v3明确初始化数字。Goal仅兼容目标定义，不回填运行快照。 |
| 配置不存在或计数不支持 | Usage 直接返回原业务错误，不先渲染正文。 |
| 未知 schema、缺引用、损坏 | 返回错误，不隐藏为“无状态”，不使用更旧结果或全历史回退。 |
| fork | 各owner遵守原opening/祖先截止；只用切点内可见引用和数字，缺数字时明确首次初始化，不冒用父分支未来状态。 |
| Goal 删除 | input_id 定位受影响业务状态，类型/位置索引定位前一个完整目标；只修复实际受影响的 Goal 业务变更，结束旧执行并完成必要补偿，不重建控制源。 |
| 首次完整性校验 | 按存储合同完整检查，单列 O(N) 读取；不改变校验要求。 |

Goal 删除及删除后补偿未提交的崩溃，属于目标定义的真实业务修复，成本按受影响 Goal 变更计；不恢复旧次数、轮次、任务或评估，不为普通输入保存边界。旧格式兼容与该修复不承诺只读一条；新格式普通加载不借此重扫历史。

## 5. 首屏、详情和 Recall

### 5.1 客户端打开

~~~text
attach -> 固定窗口 -> 显示首批内容并接受输入
                  -> 相同头的 owner inventories
                  -> 异步 usage

展开 -> 一个 detail RPC -> 原视图
较早 -> 原范围下一页
~~~

客户端沿用 100 行、四页和 300 身份的首开预算。缺 turn/start 不改 done 的含义，不把整条分支读完。历史页、usage 和用户输入不能被同一个串行 await 相互阻塞。

[显示设计](appserver/session-display-read.md)规定 summary、实时协商、类型化 detail 和超限错误。首屏 replacement 传输为零；内联大事件的 SQLite 解析仍计费。详情一次读取/编码，不按字节 offset 反复重读。超出既有回复帧预算明确报错并保留导出路径。

### 5.2 Recall 与已有 FTS5

复用已有 FTS5 TextCandidate，不增加渲染结果索引。索引只提供源属性的无漏报候选；既有 3–200 ASCII 安全词规则、跨 delta 尾部及未索引词保留语义不变。

Recall 先取候选位置和关联键，按候选重建自己需要的 thing 正文并验证；达到结果页及续页证据即停止。没有命中的保守候选仍会被验证，这是真实候选工作，不是 owner 状态恢复。非可索引词、渲染新增文字和未索引类型不作“零扫描搜索”承诺。

Store 对有选择性的文本候选从候选驱动关联，不能先扫全部 origin 再逐条检查 FTS；没有 TextCandidate 的查询不构建全文候选集合。关系判断保留原固定范围，分页只推进外层。具体执行只在[读取执行设计](timeline-read-followups.md)定义，不改 Recall 业务匹配。

Hooks 等其它现有恢复查询只接受通用 SQL 执行优化，不在本轮另建持久状态 owner。其实际候选和计算量单列；本设计不把执行器修复等同于所有插件都已消除历史折叠，也不以它为由扩写所有插件状态机。

## 6. 必要修改和撤出范围

| 单元 | 必要落点 | 不保留的现有分支扩张 |
| --- | --- | --- |
| Usage | 数字owner；现有usage委托、计数/投影复用；原ContextPolicy.prepare/validate复用同一请求已算数值。 | 数字树、boundary与snapshot双写、request-base事实、ModelProvider/AgentLoop confirm/discard、事件计数载荷。 |
| InputQueue | 原当前集合与发布；当前成员/决定/控制查询。 | recovered、entries节点/序列化、八类型源历史重放；公共pending合同改动。 |
| Goal | 内存执行状态、完整业务事件、异常/关闭 paused、重载映射、owner get、文本/评估生命周期对齐。 | 完整运行快照、逐事件 checkpoint、节点装载、源后缀执行重放、每输入边界、重载后自动续行/评估。 |
| Prompt | 精简引用、指定scope读取、原采用/退役批次合并、ConfigurationPublisher显式状态；按变化键生成输入。 | 逐生命周期结果、整集复制比较、通用QueryRows及调用方rows=None。 |
| Store | query匹配、数组locator、v40输出ID/恢复成员、handover组及三个实际字段键、升级/复制/校验。 | 业务状态机SQL、任意字段索引注册、正文缓存、owner状态API。 |
| 显示客户端 | 历史/实时协议、原 RPC 边界、worker、projection 和视图；ACP/Web 原窗口。 | offset 分块协议、完整 JSON 文本拼接、服务端 detail 缓存。 |
| 验证及配套 | 真实受影响 flow、格式升级、生成声明与装配图。 | 泛化数组组合测试、仅机械补字段的调用方修改、手写生成图。 |

Goal 的 Session 身份隔离、RPC 边界的既有正确性修复保留为独立单元，不据此扩大性能改造。图片复制优化、请求冷前缀重哈希、迁移 replacement 裁剪、MCP 重试和启动超时策略不属于本轮；设计不要求它们作为交付前置。

上述是按行为划分的明确范围，不以已有 diff 文件数量反推必要性。各 owner 文档给出字段与代码对象；实现不能为保留旧分支结构而恢复已排除的机制。当前实现及生成图不因文档更新而宣称已达到目标。

## 7. 存储格式与兼容

v39 保留已确定的 changes 元素定位编码，不附加数组查询语言。v40 采用已确认的[输出逻辑身份和恢复成员方案](appserver/native-storage-format.md#提案v40-补齐逻辑事件身份与恢复成员)：输出locator和五种恢复描述保留；本次补齐handover存在组与turn/scope/epoch及源位置三个定位键，同事务升级、失败回滚。

不向 v39 静默写入 v40 结构；旧程序拒读新格式。owner 的 ExtensionEvent/schema 与 native 格式分开：Queue不再写恢复事件；Prompt新引用为prompt-state/v2，Usage数字为context-usage/v4，小位置缓存为context-usage-covered/v1；Goal业务事件为goal-state/v1，不沿用旧 goal/v1 或提议 goal/v2 的运行快照。旧布局按各 owner 的兼容合同处理，Goal 不初始化运行快照；Store 不执行业务初始化。普通调用继续使用现有 query；旧 QuerySpec 的 Rust 字面量与线上请求不增加可选字段。

## 8. 验收

设计预算和实测成绩严格分开。正常路径固定 K/U，历史 N=6,000/60,000，U=0/3，pending=0/10；Prompt 固定有效引用扩大原数组 128/2,048；队列按真实空队列、一个待输入、交接/撤回流程计量；Goal 固定业务变更数，扩大普通聊天轮数并核对零运行快照写入；显示用 2.4 MB replacement 及超过回复预算的对象。

| 检查 | 必须满足 |
| --- | --- |
| 正常状态恢复 | Queue当前对象、Prompt有效引用/指定scope、Usage数字/未完成工作；不遍历已结束聊天。Goal只恢复业务状态并内存暂停。 |
| 已知 ID 命中/未命中 | 普通身份索引加输出 locator，不扫输出类型；验证旧截止、fork、混合 ID。 |
| pending 全部分页 | 只读固定版本当前成员；已结束历史十倍增长不能造成十倍 origin 检查。 |
| SQL | 累计 statement、VM_STEP、FULLSCAN_STEP、SORT、AUTOINDEX、节点/字节及 EXPLAIN；不只看返回数。 |
| 写入 | 按保存边界文档的真实流程登记；空队列/无提示变化0恢复事件；Usage禁止boundary与结果双写；小位置缓存单列；原事实、成员、FTS、WAL全部计入。 |
| 一致性 | 提交前后崩溃、回调丢失、self-output、并发分页、取消/closing、复制/删除和旧库初始化。 |
| 用户流程 | CLI/ACP/Web 首开、侧线/成员、加载较早、重连、异步 usage、详情失败/超限；旧能力服务器保持有界。 |
| 产品结果 | 同机、同缓存条件、release 产品装配及隔离 home，拆分 attach/owner/首帧/inventory/usage；采用 #1320 的首帧 <=2s 目标验证，不以旧微测宣称通过。 |

[恢复成本设计](appserver/restore-read-cost.md)要求普通恢复不扫描历史做准入校验；配置和目录按索引读取，实际内容按需解码。旧Usage复用兼容数字，不以升级为由补建所有历史累计位置。装配、owner计算、格式升级和外部等待分别报告；最终用长会话整体恢复复测。
