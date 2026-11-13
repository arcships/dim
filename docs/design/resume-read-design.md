# Resume：状态、查询与显示的最终设计

Status: Draft.
Scope: #1320 的跨模块读取、业务恢复和客户端打开流程。
读取清单与六组历史增长对照见[检查报告](appserver/resume-read-audit.md)。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)、[#1147](https://github.com/arcships/dim/issues/1147)。

本文件规定本轮修改边界及模块关系，具体字段和算法由各 owner 文档唯一规定。目标是消除正常状态读取和客户端首屏对已处理历史的重算。InputQueue直接查询当前输入；Prompt保存有效引用与采用版本；ContextUsage保存已算出的数字。Goal恢复目标定义，未结束执行在内存中暂停，不恢复运行进度。请求准备复用同次上下文和初始计数，见 [ContextCompaction](plugins/ContextCompaction.md#request-local-projection-handoff)；不改变请求内容、压缩质量或迁移内容。

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
| 校验 | 普通 resume 不校验已接受的历史或子分支切点；仅在导入、升级、新建切点时验证对应边界。 | 打开时完整解析历史、重复验证不可变的旧切点。 |
| 请求存储前缀 | 沿用已发布请求存储合同。 | 本轮新增的冷前缀重哈希优化及请求链重构。 |

各设计为确定方案，不列实现者自行挑选的替代结构。表中当前成员数量与显式迁移是各自明确成本，不是正常历史遍历的备用路径。

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

保留 TimelineQuery、QuerySpec、Fields、QueryRow、QueryValue、原 pending/get 方法。**不新增必填字段、QueryRows 或状态 CRUD。** v42 通知关联增加兼容的 concat/root 操作数，语义由[属性查询](timeline-property-query.md#22-字段和运算)规定。 Prompt 使用原有数字下标路径；只改其 Store 执行和数组定位。唯一新增业务读取服务是 ADR 023 决定的内部 contextUsage.read；显示协议由 App Server 自己定义。

插件不取得 Store 句柄、表名或索引名，不自行写数据库。未打开但仍活动的分支按现有 Session 获取流程打开，不能套用结束分支快读。

## 3. 正常恢复和实时处理

不再统一要求owner每处理源事件就保存完整结果。内存位置前进和持久化触发分别由业务设计决定，字段/事件数唯一见[保存边界](owner-state-indexed-reads.md)。

~~~text
Goal  -> 最新完整业务状态 -> 未结束在内存paused；运行计数不恢复
Queue -> 当前输入成员 -> 每项决定事实 -> 最新控制 -> 原内存调度
Prompt-> 最新有效引用 -> 指定scope边界 -> 有效Set下标；配置独立读
Usage -> 最新数字/小位置缓存 -> 仅类型/位置探测 -> 有缺口则未就绪；实际请求建立数字
~~~

普通已就绪getter直接用owner当前内存；固定本次目标，等待真实工作交接，不等待并不存在的“每回调恢复事件”。usage不以新总量冒充旧through。查询取消不撤销已接受业务工作。Goal重载暂停不传播到其它owner；Loop/Tool/Task等仍结算自己的资源。

### 3.1 必要对象和成本

| 对象 | 正常单分支冷读 | 新增写入 |
| --- | --- | --- |
| [Usage](plugins/ContextUsage.md) | 数字/位置缓存/计数控制各1+六类源合并后缀1，共4 query；未指定tools时首次另点读system；无旧消息解码 | 每个实际内容源一份numeric（同时是累计边界），配置/基准变化按实际来源保存；仅无效果尾部交接保存小covered |
| [队列](input-queue-recovery.md) | 空队列4query；有队列只增加当前对象的applied/withdraw及必要默认mode、interrupt定位 | 恢复事件0；仅原applied/deleted |
| [Goal](plugins/Goal-state-read.md) | opening位置和最新goal/state；只解码一个目标，活get零SQL | 仅真实goal/state；运行进度和重载均0 |
| [Prompt](prompt-state-read.md) | 引用1+配置1+提示变更后缀检查1+G个有效正文事件+已知scope键定位 | 真实采用/退役每批1份引用；普通无提示变化的turn0 |
| [pending](appserver/timeline-member-index.md) | 固定through根、当前成员与本页字段 | 原事实事务维护成员路径；不复制整个当前队列 |

表中为逻辑query，不是statement计数。SQL表达式、索引键、当前未匹配条件和额外扫描范围见[状态字段索引](appserver/timeline-state-read-indexes.md)。源码拥有的原有能力与目标新增执行必须分开验证；LIMIT1不是性能证据。

## 4. 初始化、删除和错误

| 情况 | 确定处理 |
| --- | --- |
| 旧数据/缺少派生结果 | Queue直接用原事实和升级后的成员索引，不写初始快照；Prompt已知v1可取原引用；正常恢复只补一条可证明的采用缺口，旧多条无引用历史须用显式离线迁移；Usage旧v1–v3复用可证明有效的已有数字；启动读头前有未覆盖源或无数字时等待实际请求最终投影。Goal只读取当前goal/state；没有该事实即没有Goal，不读旧文本、不回填运行快照。 |
| 配置不存在或计数不支持 | Usage 直接返回原业务错误，不先渲染正文。 |
| 未知 schema、缺引用、损坏 | 返回错误，不隐藏为“无状态”，不使用更旧结果或全历史回退。 |
| fork | 各owner遵守原opening/祖先截止；只用切点内可见引用和数字，缺数字时等待实际请求建立数字，不冒用父分支未来状态。 |
| 删除排队输入 | 只删除对应排队/续行输入身份，不撤销已生效Goal，不读取历史、不写补偿目标。 |
| 正常打开 | 读取目录、当前控制和当前对象；不进行历史完整性校验。 |

Goal 的目标定义只由明确的目标操作与执行终态改变。输入删除不是目标撤回；不保留旧文本兼容或删除回放。

## 5. 首屏、详情和 Recall

### 5.1 客户端打开

~~~text
attach -> 固定窗口 -> 显示首批内容并接受输入
                  -> 相同头的 owner inventories
                  -> 异步 usage

展开 -> 一个 detail RPC -> 原视图
较早 -> 原范围下一页
~~~

CLI/ACP/Web 首开按可显示事件的 kind 索引取有界窗口；首张、续读及显式较早读取的页尺寸由[显示读取合同](appserver/session-display-read.md)统一规定。较早内容由用户请求。缺 turn/start 不改 done 的含义，不把整条分支读完。历史页、usage 和用户输入不能被同一个串行 await 相互阻塞。

[显示设计](appserver/session-display-read.md)规定 summary、实时协商、类型化 detail 和超限错误。首屏 replacement 传输为零；内联大事件的 SQLite 解析仍计费。详情一次读取/编码，不按字节 offset 反复重读。超出既有回复帧预算明确报错并保留导出路径。

### 5.2 Recall 与已有 FTS5

复用已有 FTS5 TextCandidate，不增加渲染结果索引。索引只提供源属性的无漏报候选；既有 3–200 ASCII 安全词规则、跨 delta 尾部及未索引词保留语义不变。

Recall 先取候选位置和关联键，按候选重建自己需要的 thing 正文并验证；达到结果页及续页证据即停止。没有命中的保守候选仍会被验证，这是真实候选工作，不是 owner 状态恢复。非可索引词、渲染新增文字和未索引类型不作“零扫描搜索”承诺。

Store 对有选择性的文本候选从候选驱动关联，不能先扫全部 origin 再逐条检查 FTS；没有 TextCandidate 的查询不构建全文候选集合。关系判断保留原固定范围，分页只推进外层。具体执行只在[读取执行设计](timeline-read-followups.md)定义，不改 Recall 业务匹配。

Hooks 不做历史恢复，按 [Hooks §3.5](plugins/Hooks.md#35-loading-and-reads-at-hook-points) 在真实触发时读取身份和当前轮次，不另建持久状态 owner。其它现有恢复查询只接受通用 SQL 执行优化。其实际候选和计算量单列；本设计不把执行器修复等同于所有插件都已消除历史折叠，也不以它为由扩写所有插件状态机。

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

不向 v39 静默写入 v40 结构；旧程序拒读新格式。当前 native 格式为已确认的 v47，合并 main v46 与 resume v44，按完整结构兼容 main v42–v46 和 resume v42–v44；升级方案见 [native v47](appserver/native-storage-format.md#v47合并-main-v46-与-resume-v44)。不增加业务快照表。owner 的 ExtensionEvent/schema 与 native 格式分开：Queue不再写恢复事件；Prompt新引用为prompt-state/v2，Usage数字为context-usage/v5，小位置缓存为context-usage-covered/v1；Goal业务事件为goal-state/v1，不沿用旧 goal/v1 或提议 goal/v2 的运行快照。旧布局按各 owner 的兼容合同处理，Goal 不初始化运行快照；Store 不执行业务初始化。普通调用继续使用现有 query；旧 QuerySpec 的 Rust 字面量与线上请求不增加可选字段。

v42 为审批、协作交付、未通知后台结果补成员索引，保留 exec 任务身份定位。只索引原事实，不增加状态事件；兼容 v41 和两套旧格式家族，失败回滚。首次升级单独测量，不计为已经建好索引的正常加载；具体结构与通知关闭边界只在[原生格式 v42](appserver/native-storage-format.md#v42-indexes-for-unfinished-external-work)规定。

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
| 产品结果 | 同机、同缓存条件、release 产品装配及隔离 home，拆分 attach/owner/首帧/inventory/usage；采用本轮 CLI 启动至首屏可输入 <=200ms 的目标验证，不以旧微测宣称通过。 |

[恢复成本设计](appserver/restore-read-cost.md)要求普通恢复不扫描历史做准入校验；配置和目录按索引读取，实际内容按需解码。旧Usage复用兼容数字，不以升级为由补建所有历史累计位置。装配、owner计算、格式升级和外部等待分别报告；最终用长会话整体恢复复测。

## 9. 冷启动的剩余历史读取

普通 resume 不重播已经结束的轮次、输入、工具调用或后台任务。查询返回空集或一行也必须验证扫描范围。保持原有 QuerySpec、TimelineQuery 和协议；业务筛选由 owner 提供，索引执行由 Store 负责。

| 所有者 | 必要对象与查询 | 不在 resume 读取的内容 |
| --- | --- | --- |
| BranchLine | 挂载零历史查询；名单对齐只读当前子分支及已知创建位置；提醒发生时读最后的 branch:line/branch:parent 块，游标按实际目标查。 | 普通输入历史、所有工具结果、旧轮次 steering。 |
| Collaboration | 未结束 request、未采用的审批输入；不恢复旧 turn、interrupt 集合或 admission 任务。 | 所有 turn/start/end、全部 applied/deleted、已交付操作。 |
| BackgroundExec | 未结束 started；未通知的 ended。历史任务仅在按 task_id 读取或停止时取得。 | 所有输入、已通知的结果正文、所有旧任务的常驻表。 |
| Loop | 当前普通/交接输入成员，再按输入 ID 取 applied；已消费输入不参与候选。 | 所有旧 applied 与 consumed 的配对。 |
| ToolExecutor | 当前工具调用成员；未完成父调用只取子结果 effects。 | 已完成调用、子结果正文。 |
| Prompt | 有效引用与已知 scope 键；SQL 使用 turn/scope 索引定位。 | 为查旧 scope 扫到该位置的全部事件。 |
| 首屏 | 最新 turn/start；范围内第一条 model-start；该范围的 consumed ID，再按 ID 取输入位置。 | 从起点寻找当前轮的首个请求或被消费输入。 |
| TimelineTree | 目录的固定父子切点与创建位置；继续必要的未完成关闭。 | 重新渲染父分支上下文来确认已经接受的切点。 |

200ms 验收采用 release 构建、隔离 home、同一份约 25,000 条事件的 SQLite 副本；每次重新启动守护进程，PTY 回答终端协议，从 CLI 进程启动计时至历史出现且按键可见。数据库及测试用程序文件须在启动前确认驻留页为零；共享系统库的缓存条件单列。保留文件缓存的结果和热重连单列，不能代替这项验收。不得以局部查询耗时替代端到端结果。

另用独立 cgroup 限制 CLI、守护进程及其子进程的合计内存并禁用该组交换，记录实际限制值、峰值、匿名内存、文件缓存、磁盘读取字节和限制触发次数。统计组内的测试工具开销单列；不能只报 RSS，也不能以预热整个数据库取得达标时间。首次格式升级仍单列。


### 9.1 恢复的是责任，不是旧执行现场

| 对象 | 重载必须保留的业务责任 | 不恢复的数据 | 必要读法 |
| --- | --- | --- | --- |
| Goal | 目标、状态、判定者；未关闭目标在内存 paused | 旧轮次、续行计数、评估等待、任务等待 | 最新完整 Goal 业务状态 |
| InputQueue | 已接受但未消费/撤回的输入 | 旧 turn 的 starting/released 副本 | 当前输入成员及这些 ID 的采用决定 |
| Prompt | 当前有效提示引用及配置 | 已结束轮次、任务集合 | 最新引用、所引用事件的指定条目、已知 scope 的结束事实 |
| ContextUsage | 已计算的内容贡献与总数 | 旧请求执行状态、已结束 request 集合 | 最新兼容数字；不能为显示数字启动全历史计算 |
| Loop | 结束被中断的当前轮次，处理仍待采用的输入 | 已结束的模型步骤、旧工具批次 | 最新 turn；若未结束，只定位最后请求及其未结算对象 |
| ContextPreparation / ModelProvider | 给已接收但未结束的请求写失败/取消终态 | 旧请求正文、旧网络任务 | 当前未完成成员；只读这些请求的身份和结算字段 |
| ToolExecutor | 给未结束工具批次写终态，保留已确认副作用 | 旧工具执行进度、结果正文目录 | 当前工具批次；按父调用和起点定位子结果 effects |
| Approval | 取消已无人执行的未答复审批 | 旧对话框、等待通道、历史审批集合 | 未答复审批 ID；写原 cancelled，不重新询问 |
| BackgroundExec | 结算未结束进程；补交确实未交付的完成通知 | 所有旧任务的常驻目录、已通知的结果正文 | 当前未结束 started、未通知 ended；历史 output/stop 按 task_id 取得 |
| Collaboration | 终止上一代仍未完成的 request 与其成员工作 | 旧 turn/interrupt 集合、admission 任务、message/cancel/close 的旧调用栈 | 当前 request；按其操作 ID 取交付结果。未交付的其它操作只补原 Closed 终态；不恢复调用栈 |
| BranchLine / Hooks | 在新的触发点作决定 | 旧回调、旧 steering、旧工具参数和消息集合 | 挂载零历史读取；触发时按位置或 ID 查询 |
| Permission | 采用的权限策略 | 旧审批决策过程 | 最新 permission/changed |
| Wait | 当前轮次是否尚未关闭 | 旧 wait 调用、已结束轮次到达队列 | 最新 turn/start/end；没有重启等待任务 |
| TimelineTree | 分支目录、固定父子切点、必要的未完成关闭 | 已接受切点的上下文重新渲染和验证 | 目录和已知创建位置；按子分支 ID 定位必要终态 |

正常打开不执行旧模型或工具，不重启旧等待任务。终态结算与重新执行是两个动作；保留前者不构成恢复后者的理由。首屏之外不得将完整历史分页读入业务层。缺少派生数字也不例外：原事实仍保留，数字首次计算必须随真正需要构造的请求完成，并复用那次已取得的内容；不能在 resume 期间另起历史重建任务。
