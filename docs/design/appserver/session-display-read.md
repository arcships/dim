# Session 显示读取：窗口与按需详情

Status: Draft.
Scope: App Server 显示读取及 CLI、ACP、Web 消费。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
合同：[历史窗口](session-history-window.md)、[客户端窗口](../clients.md#opening-a-conversation-with-a-bounded-window)、[读取归属](../timeline-read-ownership.md)、[属性查询](../timeline-property-query.md)。

## 1. 决定与兼容

保留 session.history 的完整产品事件合同。新增 display/detail 能力，显示页明确区分完整 ProductEvent 和完成压缩摘要。摘要不冒充缺字段 AgentEvent，不进入模型或状态恢复。

hello 在协议 v3 公布 session.history.display 和 session.history.detail。新客户端仅在两者均支持时于 attach 传 display=true；模式在订阅前确定，属于该 attachment，detach 释放。未协商客户端保留完整事件模式；新版客户端遇到旧服务器沿用既有有界 history，不全量回退。

## 2. 公开方法与类型

| 方法 | 请求 | 回复 |
| --- | --- | --- |
| session.history.display | 原 session/branch、latestTurn/before、through、limit；可协商的 tail | items、through、nextBefore、done、可选 turnStart。 |
| session.history.detail | sessionId、branchId、eventId、sequence | eventId、sequence、data；data 为完整 ContextCompactionData。 |

`session.history.display.tail` 公布后，客户端在显示读取传 `tail=true`。它与 `latestTurn`、`after` 互斥，只选择固定已发布 `through` 以下最近的有界序号窗口。初始页不查询 turn 起点、首个请求或 consumed 输入；后续 `before` 仍为排他上界。显示页允许在原子批次内结束，不扩张到整批。当前控制状态由 owner inventories 和实时通知提供；显示页不恢复它们。旧 history、显式 latestTurn 和导出的原子批次合同不变。旧服务器仍使用已有有界读取，不全量回退。


协商 `session.history.display.transcript` 后，显示读取同时传 `tail=true, transcript=true`。
`limit` 是所选 kind 的逻辑事件数，上限 256，不是原始序号跨度，也不是屏幕行数。
显示类型为 input/message、input/applied、input/consumed、input/withdraw、input/deleted，
step/text、step/thinking、step/tool-use、step/tool-use-start、step/tool-call、step/tool-start、
step/tool-completed、step/tool-result、step/model-output-end，turn/end，exec/started、exec/ended，
collaboration/request、collaboration/delivered、collaboration/result，branch/create、branch/create-failed、
branch/ended，以及 context/compaction-start、context/compaction。
`input/message` 整类保留，不按正文前缀或来源进一步筛选；原客户端决定如何显示其内容。

普通显示事件与压缩事件分别用现有 kind 索引定位最近的有界位置，再按原序号合并，最多保留 limit 个。
位置查询直接读取现有 kind 索引；普通事件的序号直接返回，合并文本只读物理记录的起止序号，
按本页剩余数量产生逻辑序号，不解析 records 正文。每种 kind 最多提供 limit+1 个位置，再合并取最新页。
只为这些位置读取普通正文或压缩摘要。查询的 before 是排他上界，through 固定；后续页沿用同一 through。
不补齐整轮、整批或窗口外的输入控制事件。控制状态仍由 owner inventories 提供。
不新增索引、业务事件、缓存或存储格式；未知类型仍由原完整 history/导出合同提供。
CLI 行模式、主线、分支和成员视图、ACP 加载及单页扩展、Web 加载/重连/加载更早均协商此模式。
未支持该能力的服务器继续使用既有 tail 或 latestTurn 路径。

历史 context/configured 不进入 tail。attach 的模型配置及其后实时配置按自己的 model_through 交接，不能因配置序号已被聊天窗口覆盖而丢弃。工具起点不在窗口时保留调用身份/结果，完整参数按详情需要点读，不为首屏补齐旧工具输入。

压缩事件先按 kind 和窗口范围定位，再一次提取状态及摘要字段；不能在普通事件候选和摘要筛选中重复解析大正文。completed 使用下面的摘要，其他结果保留原产品事件。字段投影必须保留 Missing 与 null 的原语义；不外置压缩正文、不新增格式或缓存。

完成压缩摘要：

~~~json
{"id":"event-id","sequence":42,"create_at":1700000000000,
 "type":"display","name":"compaction",
 "data":{"start":38,"strategy":"summary"}}
~~~

摘要表示 completed；start 在旧事实未记录时省略；insufficientSpace 仅在原诊断有该标记时出现。replacement、context_baseline、digest_entries 和完整 diagnostics 不发送。其它压缩结果仍是原完整产品事件。

display=true 时，实时完成压缩通过 session.display {sessionId,branchId,item} 发送同一摘要，其它实时产品事件仍走 session.event。重连同样协商模式，不能只改历史页而让实时流继续传大 replacement。

### 2.1 detail 固定为一次类型化读取

detail 只读本分支已发布且身份、sequence 同时匹配的完成压缩。按原位置和 ID 定位，不读取整个 turn。空身份/未来位置返回 invalid_params；未找到或类型不符返回 detail_not_found。

一次请求加载这一个 data、编码一次并返回类型化对象。不返回 JSON 文本块，不使用 offset/nextOffset/totalBytes，不在每个分块重读和重编码完整事件，也不建立服务端详情缓存。

回复使用既有 REPLY_FRAME_BYTES 预算，即 16 MiB 减 64 KiB；超出预算返回 detail_too_large，保留摘要，提示通过既有导出取得完整事实。不截断内容，不返回半份 data，不增加无限大详情传输协议。2.4 MB replacement 在此预算内。

该限制约束回复，不是 Store 内存上限。为判断编码大小，服务端仍读取并解码一个完整详情，成本 O(B)；超限对象也计入这次读取。客户端在现有 RPC 边界解码一次类型化 data，不再拼接字符串后做第二次 JSON 文本解析。

## 3. 窗口与数据读取

1. 固定 through 和原始位置游标。transcript 先按 kind 筛选并限制位置数量；仅协商 tail 时保留原始序号窗口。端点不要求完整批次；旧显式 latestTurn 保持原约定。
2. transcript 每批最多读取 8 个已选位置；旧 tail 每批最多处理 8 个原始位置，在正文解码前按同一 ProductEvent 可见性规则选择普通正文或压缩摘要；排除历史配置、隐藏 model/call、context/prepare 和私有事实。没有再加一次包含全部可见类型的位置查询；混合输出展开会使内联大行重复读取。
3. 完成压缩只取摘要字段，不在客户端丢弃已传输的 replacement。
4. 按实际编码字节执行原窗口预算；隐藏位置保留内部零载荷位置项，仍推进游标，不伪造事件。
5. 客户端投影后释放原始页。transcript 首开每页 64 个所选事件，出现可展示条目即停止；加载更早每页 256 个所选事件。单次仍限制四页或 100 个条目身份，保留最多 300 个身份及完整页边界。Shell/Hook 等输入可能不形成普通聊天条目，因此不承诺一页填满屏幕，也不扫描完整历史填屏。缺 turn/start 不影响此规则。
6. owner inventories 使用窗口固定头建立完整控制状态，然后交接后续实时事实；较早显示页不再推进当前 Goal/pending/turn 状态。同一 exec/started 同时出现在显示窗口与 pending 清单时，按分支和事件 ID 保留一个任务，停止全部对该任务只发一次请求。

摘要字段仍位于同一存储事件 JSON。该物理行的读取和 SQLite JSON 解析计入服务端成本；本次不增加摘要表、不外置整个 compaction、不承诺摘要点读只触碰几个字节。确定减少的是 replacement 网络传输、客户端解码和折叠视图内存。隐藏请求的外置消息节点加载必须为零。

普通大消息和工具结果保持完整对象；显示按小批读取并编码，达到页字节目标即停止，不为原子批次扩张。单对象超过帧预算返回原传输错误，不引入另一套摘要。导出始终使用完整事实。

## 4. 客户端与交互

~~~text
resume -> 显示窗口 -> 先显示并接受输入
      -> owner inventories -> 安装控制状态
      -> 异步 usage -> 更新读数

Fullscreen: ✓ Context compacted ▸
  展开 -> ▾ Loading -> 正文
                     -> 失败：保留摘要，可收起后重试
                     -> 超出显示预算：提示导出
  收起 -> 释放正文，旧回复不再写回
~~~

TUI 两种模式沿用 [CLI 详情设置](../cli-commands.md#transcript-detail-settings)；fullscreen 支持逐行展开，scrollback 不显示披露箭头，通过 `/tools` 和设置控制详情。不增加菜单命令。展开偏好先显示摘要，再异步读取。行模式、ACP、Web 保留原先对 compaction 的显示效果，不新增原本不存在的详情视图。JSON exec 和导出继续完整模式。

沿用现有 worker 和 RPC 完成通知。只保存目标身份、projection generation、RPC ID，以及 unloaded/loading/loaded/error 状态；一次至多一个详情 RPC，不建立字节分块队列或拼接缓冲。目标包含 Session、分支、事件 ID 和 sequence。主线、侧线和选中成员使用相同的可见/展开判断；不能渲染为 Expanded 却因全局 Collapsed 不发请求。

收起、切换附件或关闭视图后释放正文；已发回复按目标及 generation 丢弃。一次失败不循环重试；重新展开才重试。worker 和视图共享已读对象，不复制成视图外永久缓存。

模型目录、权限读数和技能目录在首屏历史请求发出后分别异步取得，回复按本次附件和模型身份处理；首屏不等待这些回复。切换附件时丢弃旧请求身份，权限实时变化使旧权限回复失效。

usage 请求独立于历史和用户命令等待，不占用同一串行 await。未配置结果保留到配置变化，不在每轮空闲重复发送。它的服务端成本归 ContextUsage，异步消费本身不算计算优化。

## 5. 成本与修改范围

| 操作 | 目标和实际成本 |
| --- | --- |
| 首屏 | 窗口可见正文和摘要；未展开 replacement 传输为零；内联大行 JSON 解析另计。 |
| 隐藏请求 | 零 system/tools/messages 节点加载；仍定位原始窗口位置。 |
| 展开 B 字节详情 | 一次对象点读、一次编码、一次类型化消费；服务端和客户端临时内存 O(B)，不乘以分块次数。 |
| 收起 | 释放详情；零持久化。 |
| 加载更早 | 原固定范围下一页；不重放当前控制状态。 |

| 修改对象 | 必须修改 |
| --- | --- |
| App Server 历史/Session 实时编码 | 显示摘要、附件协商和单次 detail。 |
| 协议 DTO / native RPC 边界 | 明确 item 和类型化 detail；旧完整事件不变。 |
| CLI worker、projection、TUI/成员详情 | 原异步请求与视图状态中接入一次详情，不做片段重组。 |
| ACP / Web | 协商、窗口和原通知消费；不新增详情产品能力。 |
| 历史/上下文业务 owner | 不改变状态机或上下文内容。 |

## 6. Requirements raised to mechanism owners 与验收

| owner / 问题 | 复用和要求 | 位置、兼容和取消 |
| --- | --- | --- |
| App Server / #1320 | 新显示类型，复用原窗口、可见性与回复帧预算。 | 先协商再订阅；不改旧通知和位置。 |
| Store / #1320 | 先选窗口/可见字段，再加载所需内容；detail 按位置和 ID。 | 范围/取消不变；不增加 UI 表、业务分支或缓存。 |
| 客户端 / #1320 | 复用原 worker、窗口和视图所有权。 | 取消只结束等待；旧 generation 回复不得覆盖新视图。 |

复用显示和客户端 flow：原子批次、隐藏页、未知可见类型、合并输出、缺 turn/start、四页预算、实时压缩、重连、展开失败和超限。2.4 MB replacement 首屏传输为零，展开只请求一次；超预算明确报错，原导出仍完整。验证全局折叠但成员详情展开的场景。

分别记录首屏、展开和所有页累计 SQL、JSON 解析、节点/字节、网络、CPU、内存；不能把回复变小等同于 Store 字节读取变小。旧服务器路径仍有界且可用。

本设计不新增 Capability、Timeline 事实或模型上下文入口。Store 只执行通用字段/身份读取。删去分块文本协议、offset 校验链、片段拼接状态和重复整读；保留摘要、单个异步请求和视图状态，是实际折叠/展开交互所需。
