# Timeline 存储与读取

状态：基础存储、可丢弃恢复快照、分段读取与恢复扫描已实现。本文定义 Timeline 写入、发布、读取、错误与驻留；Session 的创建、装卸载与关闭等业务生命周期由 App Server 负责，SQL/JSON 见[存储格式](../../design/appserver/native-storage-format.md)。

进程 SQLite 服务在关闭时先停止操作准入，再排空已受理的查询/事务并关闭连接；等待者被丢弃不取消实际 blocking 工作或已启动的关闭。成功关闭立即释放数据库维护锁，保留旧 `Arc` 不占锁且拒绝新操作；关闭失败保留实际连接与锁，错误明确返回。SessionStore 仍拥有业务 writer、批次与发布，沿依赖先停止 Session 并排空 writer，SQLite 不承接第二套事件队列。

## 1. 结构与所有权

```text
 能力 ──append──> Timeline（每分支）
                    │ before 链、分配 sequence、登记回调、放入热窗口（不等待磁盘）
                    ▼
                 TimelineStore（端口）── 写入任务按轮提交并 fsync ──> 存储
                    │ 提交成功：推进各分支的发布水位
                    ▼
 Dispatcher <── 热窗口中已发布的事件
    └─> 各能力 handler、SessionBroadcast

 冷读取 ──context / history / local──> Timeline ──> 热窗口或 TimelineStore
```

| 模块 | 拥有 | 不拥有 |
| --- | --- | --- |
| `TimelineStore`（runtime 端口） | 事件、原子批次边界、before 轨迹、分支目录（父分支、`branch_point`、`create_anchor`）；可丢弃恢复快照（`save_snapshot`/`latest_snapshot`，按锚点保存，不解释 state）；存储编码；按分支和 sequence 读取；写入任务 | 运行中能力状态、事件语义 |
| `Timeline` | before 链、sequence 分配、热窗口、发布水位、祖先链描述、关闭与存储故障状态 | 已分发事件、祖先前缀事件 |
| `Dispatcher` | 本分支唯一的进程内顺序读者 | 历史读取 |
| 能力 | 自身业务状态；冷读取在自有任务中执行 | 全量历史副本 |
| `SessionStorePlugin` | SQLite 实现的 `TimelineStore` | — |
| `MemoryTimelineStore`（runtime） | 内存实现，供独立 `AgentBuilder`、测试与开发 profile 使用；保存全部事件，追加即发布 | 持久化 |

每个 Agent 装配一个 `TimelineStore`：App Server 的各 Agent 共用 `SessionStorePlugin` 提供的实例，独立 `AgentBuilder` 未传入时自建 `MemoryTimelineStore`。Session 创建时由 SessionScope 以 Session 名字 `timelineStore` 提供它，各分支的 Timeline 从中读写。祖先链描述为 `[(branch_id, cut)]`，由 TimelineTree 在建分支时给出；子分支不持有父分支事件。本文的内存驻留约定针对 SQLite 实现。

## 2. 存储编码

事件以原类型写入与读出；编码只存在于存储内部。

| 数据 | 编码 |
| --- | --- |
| 事件 | 一行一事件：分支、sequence、事件 ID、kind、正文、before 轨迹、原子批次终点 |
| 请求内容 | `context/prepare`、`model/call` 的 `request.messages` 存为 Session 内容表中按内容寻址的列表节点，每个节点是一条消息和前一条消息节点的地址，相邻请求共用相同前缀的节点；`tools`、`system` 按内容哈希存入，每种内容一份；事件正文只保存最后一个节点的地址、列表长度与 `tools`、`system` 的编号 |
| 模型输出 | 同一分支同一请求连续的 `step/thinking` 或 `step/text` 合并为一个输出行：行内按顺序保存各事件的 ID、sequence、`create_at`、`tags`、`delta`，共享 `turn_id`、`request_id`。追加条件与 1 秒/4 KB 阈值以[物理编码](../../design/appserver/native-storage-format.md#持久化-json-类型)为准；合并不改变逻辑事件与批次 |
| 读取 | 还原为原事件；一次读取（`read_segments`、`read_page`）或一次恢复校验的各页共用一个解码内容缓存，相同内容只解码一次，共享引用。缓存按内容正文计不超过 64 MiB，将超出时先清空，调用结束即释放 |

`ModelRequest` 的 `messages` 元素、`tools`、`system` 在内存中以 `Arc` 持有，序列化结果不变。

`TimelineStore::rollback_session` 仅撤销本次成功落库但初始化失败的新建记录；调用前停止生产者，SQLite 通过同一 writer 排空此前写入后在事务内先删除恢复快照、再删除事件等记录。普通关闭与恢复失败不触发回滚。

## 3. 写入与发布

| 步骤 | 约定 |
| --- | --- |
| append | `append`、`append_registered`、`append_batch_registered` 保持同步：执行 before 链、分配 sequence、运行登记回调，事件进入热窗口并交给 `TimelineStore` 排队；不等待磁盘 |
| 写入轮 | 写入任务逐轮提交：一轮取走所有 Session 的全部待写批次（到下一条回滚或关闭命令为止），放进一个事务；原子批次不拆分，同一分支按 sequence 顺序写入。上一轮提交后，有待写批次就开始下一轮；只有待写批次全部只含输出行事件（`step/text`、`step/thinking`，见第 2 节“模型输出”）时，这一轮才等到上一轮开始后 20 ms，其间来了其他批次或回滚、关闭命令就不再等。这样区分只看存储的物理编码：输出行靠改写同一行增长，一轮把改过的页写入 WAL 并 fsync 一次，等待让只有流式输出时每秒至多 50 轮，增量最多等待的时间和以前一样。其他批次不等，所以一串依赖写入（每次等上一次发布后才写）每步只等一次提交 |
| 耐久 | SQLite WAL，`synchronous=FULL`；macOS 另开 `fullfsync`。事务提交返回即已落盘。自动检查点关闭；writer 在提交与发布后按 WAL 大小执行被动检查点，WAL 文件有上限，关闭时截断（[存储格式](../../design/appserver/native-storage-format.md#sqlite-运行参数与空间回收)） |
| 发布 | 提交返回后推进各分支发布水位并唤醒 Dispatcher |
| 可见性 | 事件分配序号即是总线上的事实（[时间总线总纲](time-bus.md)）。能力经注入的 `Timeline` 读取到已写入位置 `committed()`，包括尚未发布、仍在热窗口的事件；Dispatcher 只分发已发布事件；`sequence()`、`watch()` 与分支外的读取（`Branch`、`Session`、RPC）只看到已发布事件，向外部表达的是“已落盘” |
| 驻留 | 已提交历史本身不设容量上限；待提交事件由 writer 队列与热窗口暂持，按 writer 队列、热窗口、冷读结果与已装载实例的实际持有和释放时机估算内存，不为此新增业务硬配额或终态兜底。输入、并发、出站均不设预设额度 |

发布与提交回调可能读取或追加；它们必须在连接互斥锁释放后运行，否则回调写入或读取时会在同一把锁上死锁。

同一事务内，writer 按 Session 和分支保留最后一个输出行的已解码 `Row`，
后续增量直接追加，避免每条增量再次查询并解析已合并的 JSON。
已有行只在首次需要追加时从 SQLite 解码；非输出事件使该分支的临时行失效。
事务结束即释放，回滚和按 Session 重试各自重新读取；物理行格式、合并阈值和发布顺序不变。

| 中断时刻（进程崩溃或断电） | 恢复后 |
| --- | --- |
| 事务提交前 | 该轮事件不存在；它们从未发布 |
| 事务提交后、发布前 | 该轮事件存在，但此前从未分发；按恢复规则与其余历史一同处理 |
| 发布后 | 事件存在 |

写入轮的已知局限：其他批次不等待，含其他批次的一轮又会取走所有在等的增量，所以许多 Session 快速写其他事件（比如工具循环）时，轮次频率只受提交耗时限制；一边还有流式输出时，每一轮都重写这些增量的输出行。在 Apple M4 上用 `timeline_acceptance rounds 64 200 64` 测量（release；64 个流式分支每秒各写 200 个增量，另有 64 条相互独立的依赖写入链，每次写入等上一次发布后才追加），每秒 97 轮、WAL 每秒写 80 MB，此前的 20 ms 间隔下是每秒 44 轮、40 MB；链上每步发布延迟的中位数从 22 ms 降到 9 ms，链的写入吞吐是此前的 1.8 倍，每次写入的 WAL 字节是 1.7 倍。fsync 不刷新磁盘缓存时（`TIMELINE_ACCEPTANCE_PLAIN_FSYNC=1`），同样的负载每秒 2,835 轮、WAL 每秒写 278 MB（此前 41 轮、39 MB），每步延迟中位数从 25 ms 降到 0.6 ms，吞吐是 4.9 倍，每次写入的 WAL 字节是 4.4 倍。只有增量时两者都不超过每秒 50 轮（实测 45 与 50 轮）。让这样的一轮只取本分支的增量可以减少 WAL 写入，但会改变“一轮取走全部待写批次”，暂不实施（[#238](https://github.com/arcships/dim/issues/238)）。

## 4. 读取

| API | 语义 |
| --- | --- |
| `Timeline::context(through).await` | 祖先链起点起截至 `through` 的完整事实重放，不需要解释器或快照 |
| `Timeline::context_with_checkpoint(through, interpreter, decode).await` | 选取 sequence 不大于 `through`、解释器相同的最近恢复快照，校验锚点事实后从其 `resume` 起读取；无可用快照时等同 `context` |
| `Timeline::history(through).await` | 祖先链加本分支截至 `through` 的完整固定历史 |
| `Timeline::local(range).await` | 本分支已写入事件的区间（默认终点 `committed()`），可分页 |
| `Timeline::trace(sequence).await` | 该事件的 before 轨迹 |

`through` 不得超过已写入位置，也不得位于原子批次内部。区间在热窗口内时直接取自内存；否则已发布部分读取 `TimelineStore`，其后尚未发布的部分取自热窗口（热窗口保留全部未分发事件，未发布事件一定在其中）。`Branch` 与 `Session` 的同名读取以已发布位置为上限。

### 有界读取与恢复扫描

业务列表、历史接口、协作游标和导出流程由 App Server 定义。存储端口负责以下约定：

| 路径 | 读取边界 | 资源与校验 |
| --- | --- | --- |
| `context(through)` | 完整祖先链和本分支前缀 | 按段读取，每段释放连接锁 |
| `context_with_checkpoint(through, ..)` | 最近有效快照的 `resume` 起的祖先链和本分支后缀 | 不扫描快照恢复位置之前的请求正文 |
| `local(range)`、`trace(sequence)` | 指定区间或命中事件的物理行 | 合并行完整解码后按逻辑序号过滤；trace 只返回指定事件的轨迹 |
| 分段读取 | 固定水位，按键 seek；内部读段可切开原子批次，`TimelineStore::read/read_segments` 的最终区间终点须为批次边界 | 每段结束释放连接锁；合并行完整解码后截取，不在锁内解码无限历史 |
| 不读请求正文的分页 | `read_page_without_request_bodies`：历史分页、摘要、协作与流式投影；批次边界与 `read_page` 相同 | 不还原 `context/prepare`、`model/call` 的请求，也不加载其内容行，只校验记录种类与引用形状（[请求增量存储 §7.5](../../design/appserver/request-delta-storage.md#75-不使用请求正文的读取)）；使用请求的读取与恢复仍完整校验 |
| 恢复 | 固定 Session 目录和所有分支头，逐页校验事件、引用、祖先截点及 completed 事实的 through 边界；可选快照 state 不参与恢复校验 | 每页结束释放连接锁；解码内容缓存跨页共用、有界（第 2 节），校验结束释放；跨页保留事件 ID 和目录校验信息 |

协作 `read_page` 的 `after` 和固定水位必须为批次边界；页长是目标值，末批超出时完整返回。内部读段可切开批次，但不把内部段尾作为业务游标。反向 `session.history` 仍为独立协议草案。

已装载实例采用发布水位，未装载记录采用已提交 head。公开 `local` 可读取完整范围，业务分页调用方必须传有界区间。当前范围读取每段 256 个逻辑序号，恢复校验每段 64 条物理行；查询索引及跨行规则见[存储格式](../../design/appserver/native-storage-format.md#分页查询)。

序列化、引用解析、hash 校验与类型转换集中在存储边界，能力只消费已验证类型。外部协议仍展开为原事件。序列化优化须测量正文复制、编码次数及耗时；不为优化增加业务状态。

### 压缩检查点

压缩事实与恢复快照分离（[ADR 001](../../adr/001-harness-foundation.md)，AGENTS 规则 9）：

| 数据 | 性质 | 内容 |
| --- | --- | --- |
| `context/compaction` 的 `Completed` | 不可变历史事实，随原子批次提交与发布 | source `through`、`cursor`、`summary`、`context_baseline`、diagnostics；不含快照 |
| `RecoverySnapshot` | 可丢弃的派生缓存，存于 `timeline_snapshots` | `{sequence, event_id, interpreter, state}`：锚定已提交 completed 压缩事件；`state` 由解释器编码，存储不解析 |

默认解释器 `dim/context-projection/v2`（`HistoryRenderer`）把 `ProjectionCheckpoint` 编码为带 SHA-256 摘要的 state：

| 字段 | 内容 |
| --- | --- |
| `resume` | 祖先链中的位置 `(branch_id, sequence)`：产生游标之后第一条历史条目的事件，不晚于该次准备的 `through` |
| `state` | 投影在 `resume` 之前的状态：`compacted_through` 游标、prompt 条目与 revision、已投递的上下文基线、已提交但未采用的输入、最新 `context/configured`、未闭合的输出与工具 |

- 写入：ContextPolicy 在 completed 事实被采用后构建快照，`Timeline::save_snapshot` 等锚点发布后才写缓存；构建或保存失败只记录告警，不回滚事实、不重做模型工作。
- 读取：`context_with_checkpoint` 按解释器选候选，校验锚点 ID、sequence、completed 种类、游标与 `resume` 位于固定祖先链和 source through 之内；解码失败、摘要不符、越界或孤立快照一律忽略并回退完整事实重放。默认解释器有两个读取入口：`HistoryRenderer::rendered_context` 返回上下文与其渲染，校验检查点的那次渲染就是调用方使用的渲染，没有检查点时也在这里渲染一次；`HistoryRenderer::context` 只返回上下文，有检查点时渲染一次用于校验并丢弃结果，没有检查点时不渲染。两者的检查点渲染失败都回退完整重放（`rendered_context` 渲染该重放，`context` 不渲染）；读取必需事实的错误不被吞掉，`history::read_context` 与 ContextPolicy 的预算校验走 `context`。
- 投影从 `state` 开始重放 `resume` 起的事件，并丢弃游标及其之前的条目。编码与摘要规则见[存储格式](../../design/appserver/native-storage-format.md#可丢弃恢复快照)。

### 使用方

| 使用方 | 读取 |
| --- | --- |
| Dispatcher | 热窗口；事件分发完即移出窗口 |
| Loop 构建 `context/prepare` | `HistoryRenderer::rendered_context`（带快照加速）：配置、prompt revision 与历史投影，以及校验检查点的那次渲染 |
| ContextPreparation | `HistoryRenderer::rendered_context(through)`：配置、prompt revision 与历史投影 |
| ContextPolicy | `HistoryRenderer::context(committed())` 做预算校验，并读 `local(through+1..)`（同一准备重试时已提交的压缩与预算事件）；在压缩完成后写恢复快照 |
| ModelProvider | 不读取；执行事件中的请求 |
| Prompt 与配置发布 | `history::read_context` |
| 能力重载重建 | `local(..=S)` |
| TimelineTree 建分支 | `history::read_context(at)` 校验截点并得到子分支的祖先链 |
| 协作读取、App Server 历史、导出 | `local` 分页或 `history` |

- 每个模型步骤两次上下文历史读取，有有效快照时每次不超过最近一次压缩之后的历史；ContextPolicy 另读一次 `local(through+1..)` 同准备尾段，稳态下为空。
- Dispatcher handler 内不等待冷读取；需要历史的工作在能力自有任务中读取，完成后再提交事件。
- 热窗口只保存未发布和未分发的事件；进程内不保留已分发的事件；返回给调用方的 `Arc<AgentEvent>` 由调用方决定持有时长。唯一的例外是分支打开期间（`Loading`）：Timeline 用一份共用的解码副本服务对其构建时已有历史的读取（本分支截至构建时的存储头部，以及祖先链各截点之前），分支离开 `Loading`（发布、关闭或构建回滚）时释放；副本保存每条事件的批次终点，切片前按“有界读取与恢复扫描”中“分段读取”行的终点规则检查，结果与错误都与读存储相同（[恢复期间共用的历史读取](../../design/appserver/restore-shared-read.md)）。

## 5. 分支、恢复与关闭

| 场景 | 约定 |
| --- | --- |
| 建分支 | 分支目录与 `branch/create` 在同一事务中提交；子 Timeline 以祖先链描述构造 |
| 恢复 | 按[存储格式](../../design/appserver/native-storage-format.md)校验物理格式与提交边界后，Timeline 以存储中最后的 sequence 为发布水位构造；能力按 `context` 或 `local` 重建，这些读取在分支打开期间共用一份解码副本（第 4 节“使用方”）；TimelineTree 的恢复事实（分支记录的创建失败）也在打开期间从中读取，子分支的创建检查只分页读到它的第一个排队输入 |
| Timeline close | 关闭前提交的事件全部发布后，才完成关闭 |
| Session close | 各分支关闭并结算其写入、Session scope 清理完成后释放该次加载的存储准入状态；存储仍接收写入时，释放排在已接收批次之后，完成后才从 Agent 移除 Session。只清理故障与 writer memo，不删除持久数据；恢复初始化失败且生产者停止后也执行此释放 |

同一存储内，同一 Session ID 同时只允许一个加载实例，跨 Agent 共享存储的调用方也须串行关闭与恢复。释放是幂等操作；SQLite 存储开始关闭或 writer 已停止后，释放视为已完成，不再向 Session 清理追加通道错误。存储关闭仍负责排空此前接收的批次并报告其自身的故障，各批次自己的结果仍报告写入失败。

SQLite 关闭与 lease：只关闭描述符可能让并发派生的子进程在 exec 前继续持有 flock，所以关闭时显式 `unlock`，并让 lease 在连接之后释放；否则下一个守护进程会把数据库误报为“另一个进程拥有该 SQLite 数据库”。

## 6. 故障

| 故障 | 处理 |
| --- | --- |
| 提交失败 | 该轮按 Session 拆成单独事务重试；仍失败的 Session 进入存储故障：已排队未发布的事件丢弃，后续 `append` 返回存储错误，以 Session 故障进入清理，清理中不再写入终态事件；完全关闭后释放故障状态，同一进程下次恢复按各能力的终态结算规则处理未完成操作。旧 Session 句柄仍保持关闭。其他 Session 照常发布 |
| 读取失败 | 发起读取的操作以存储错误失败，并按该能力既有失败终态提交；不进入 Session 禁写状态，后续操作可重试 |
| SQLite 操作 panic | 回滚该操作未提交的事务，返回存储错误；连接仍可使用，不使连接锁中毒 |
| writer 操作 panic | panic 转为存储错误，未提交事务回滚。准备与写入沿用按 Session 重试规则，仅写入失败的 Session 禁写，健康 Session 照常提交；创建回滚只使该次操作失败，保留记录并允许重试。已接收批次的回调结算一次；发布回调自身 panic 记录 Session、分支和 panic 信息，不阻断其余回调 |
| writer 意外退出 | 已接收而未完成的批次以存储错误结算回调，发布与关闭的等待者结束；所有 Session 的后续写入被拒绝，需重启进程 |
| before 回调 panic | 按 [before 失败约定](principles.md#4-通过时间线扩展行为)处理，不使分支提交锁中毒 |
| 格式不可读 | 恢复时拒绝打开该 Session，保留存储数据 |

## 7. 验收

| 场景 | 判定 |
| --- | --- |
| 长会话驻留 | 2000 步、800 KB 上下文：空闲分支的存活申请字节不随步数增长；准备期间的峰值不随压缩前的历史增长 |
| 存储还原 | 写入后读出的事件与原事件逐字段一致，覆盖去重内容、输出行跨写入轮追加、原子批次与 before 轨迹 |
| 检查点 | 从快照投影与从起点完整投影的结果一致，覆盖跨压缩的排队输入、prompt 变更、上下文基线和游标位于多结果工具事件内部；快照缺失、损坏、摘要不符或越界时回退事实重放且保留 summary |
| 读取 | 每个模型步骤两次 `context` 读取和一次同准备 `local` 尾读；测量 p99、max 规模合成会话的每步耗时与 stop 分发延迟 |
| 写入 | 11 与 64 个流式分支（每个分支每秒 30 与 200 个增量）下测量写入轮频率、发布延迟、WAL 写入量，以及 writer 队列、热窗口、trace 的实际存活字节；另有相互独立的依赖写入链（各在自己的 Session，每次写入等上一次发布后才追加）：64 个流式分支下 1、11 与 64 条，没有流式分支时 11 与 64 条；测量链上每步的发布延迟、链的写入吞吐、写入轮与 fsync 频率和 WAL 写入量，并在 macOS 上另测 fsync 不刷新磁盘缓存的模型；慢盘下记录产生与提交速率、峰值及恢复后的释放。流式分支和依赖写入链用 [`timeline_acceptance rounds`](../../../crates/dim-app-server/examples/timeline_acceptance/README.md) 在本地 release 构建中测量，结果写进 PR，不作为 CI 阈值 |
| 进程中断 | 在事务提交前、提交后发布前、发布后终止进程：恢复结果符合第 3 节；sequence 连续；在途输出按失败结算 |
| 断电 | 以事务提交返回的事件为准，模拟断电后已发布事件完整 |
| 存储故障 | 分别注入读取失败与提交失败：前者提交当前操作的失败终态且允许后续操作，后者只清理写入失败的 Session；其他 Session 照常发布 |
| 分支 | 多层分支跨祖先链读取的顺序和截点正确；截点早于最近快照、快照位于祖先分支时读取正确；父分支关闭后子分支读取不受影响 |
| 分页与恢复 | 长历史页读不先解码全量；跨合并行、批次和跨页 ToolCall 正确；恢复缓存有界，长冷读不独占连接阻塞写入 |

### 结果与通知的独立提交

`exec/ended` 与通知输入分别提交：产生方使用 `append` 记录结果，在分发时向本分支追加带其预览的通知输入（标签 `exec-wake-result:{事件 ID}` 对应结果）（输入的合法性由产生方保证，Timeline 不校验）；后台能力在装载时补齐应唤醒却缺少通知的结果。唤醒条件见[BackgroundExec](plugins/WorkspaceTools.md#backgroundexec)。Timeline 不判断任务状态，不提供专用生成式批次接口。

两次提交之间可以压缩：通知自带有上限的预览，不引用 `exec/ended`，恢复快照因此不保留任何结果事实；上下文注入一律经输入写入历史。固定草稿的 `append_batch_registered` 仍用于需要整体采用的配置，不改变其 before、登记回滚及批次水位合同。
