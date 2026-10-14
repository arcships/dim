# Timeline 存储与读取

状态：基础存储、检查点、分段读取与恢复扫描已实现。本文定义 Timeline 写入、发布、读取、错误与驻留；Session 的创建、装卸载与关闭等业务生命周期由 App Server 负责，SQL/JSON 见[存储格式](../../design/appserver/native-storage-format.md)（当前 v11）。

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
| `TimelineStore`（runtime 端口） | 事件、原子批次边界、before 轨迹、分支目录（父分支、`branch_point`、`create_anchor`）、各分支压缩检查点的位置；存储编码；按分支和 sequence 读取；写入任务 | 运行中能力状态、事件语义 |
| `Timeline` | before 链、sequence 分配、热窗口、发布水位、祖先链描述、关闭与存储故障状态 | 已分发事件、祖先前缀事件 |
| `Dispatcher` | 本分支唯一的进程内顺序读者 | 历史读取 |
| 能力 | 自身业务状态；冷读取在自有任务中执行 | 全量历史副本 |
| `SessionStorePlugin` | SQLite 实现的 `TimelineStore` | — |
| `MemoryTimelineStore`（runtime） | 内存实现，供独立 `AgentBuilder`、测试与开发 profile 使用；保存全部事件，追加即发布 | 持久化 |

每个 Agent 装配一个 `TimelineStore`：App Server 的各 Agent 共用 `SessionStorePlugin` 提供的实例，独立 `AgentBuilder` 未传入时自建 `MemoryTimelineStore`。Session 创建时由 SessionScope 注入各分支的 Timeline。祖先链描述为 `[(branch_id, cut)]`，由 TimelineTree 在建分支时给出；子分支不持有父分支事件。本文的内存驻留约定针对 SQLite 实现。

## 2. 存储编码

事件以原类型写入与读出；编码只存在于存储内部。

| 数据 | 编码 |
| --- | --- |
| 事件 | 一行一事件：分支、sequence、事件 ID、kind、正文、before 轨迹、原子批次终点 |
| 请求内容 | `context/prepare`、`model/call` 的 `request.messages` 每条消息、`tools`、`system` 按内容哈希存入 Session 内的内容表，每种内容一份；事件正文只保存内容编号 |
| 模型输出 | 同一分支同一请求连续的 `step/thinking` 或 `step/text` 合并为一个输出行：行内按顺序保存各事件的 ID、sequence、`create_at`、`tags`、`delta`，共享 `turn_id`、`request_id`。追加条件与 1 秒/4 KB 阈值以[物理编码](../../design/appserver/native-storage-format.md#持久化-json-类型)为准；合并不改变逻辑事件与批次 |
| 读取 | 还原为原事件；同一次 `read_segments` 中相同内容只解码一次，共享引用；恢复与分页各自按页持有缓存 |

`ModelRequest` 的 `messages` 元素、`tools`、`system` 在内存中以 `Arc` 持有，序列化结果不变。

`TimelineStore::rollback_session` 仅撤销本次成功落库但初始化失败的新建记录；调用前停止生产者，SQLite 通过同一 writer 排空此前写入后在事务内删除。普通关闭与恢复失败不触发回滚。

## 3. 写入与发布

| 步骤 | 约定 |
| --- | --- |
| append | `append`、`append_registered`、`append_batch_registered` 保持同步：执行 before 链、分配 sequence、运行登记回调，事件进入热窗口并交给 `TimelineStore` 排队；不等待磁盘 |
| 写入轮 | 写入任务在有待写事件时开始一轮，两轮开始时间至少间隔 20 ms；一轮把所有 Session 的待写批次放入一个事务，原子批次不拆分，同一分支按 sequence 顺序写入 |
| 耐久 | SQLite WAL，`synchronous=FULL`；macOS 另开 `fullfsync`。事务提交返回即已落盘 |
| 发布 | 提交返回后推进各分支发布水位并唤醒 Dispatcher；诊断记录在发布后发出 |
| 可见性 | 事件分配序号即是总线上的事实（[时间总线总纲](time-bus.md)）。能力经注入的 `Timeline` 读取到已写入位置 `committed()`，包括尚未发布、仍在热窗口的事件；Dispatcher 只分发已发布事件；`sequence()`、`watch()` 与分支外的读取（`Branch`、`Session`、RPC）只看到已发布事件，向外部表达的是“已落盘” |
| 驻留 | 已提交历史本身不设容量上限；待提交事件由 writer 队列与热窗口暂持，按 writer 队列、热窗口、冷读结果与已装载实例的实际持有和释放时机估算内存，不为此新增业务硬配额或终态兜底。输入、并发、出站均不设预设额度 |

| 中断时刻（进程崩溃或断电） | 恢复后 |
| --- | --- |
| 事务提交前 | 该轮事件不存在；它们从未发布 |
| 事务提交后、发布前 | 该轮事件存在，但此前从未分发；按恢复规则与其余历史一同处理 |
| 发布后 | 事件存在 |

## 4. 读取

| API | 语义 |
| --- | --- |
| `Timeline::context(through).await` | 事件 sequence 不大于 `through` 的最近一次已完成压缩的检查点，加上其恢复位置起截至 `through` 的事件；没有时从祖先链起点读取 |
| `Timeline::history(through).await` | 祖先链加本分支截至 `through` 的完整固定历史 |
| `Timeline::local(range).await` | 本分支已写入事件的区间（默认终点 `committed()`），可分页 |
| `Timeline::trace(sequence).await` | 该事件的 before 轨迹 |

`through` 不得超过已写入位置，也不得位于原子批次内部。区间在热窗口内时直接取自内存；否则已发布部分读取 `TimelineStore`，其后尚未发布的部分取自热窗口（热窗口保留全部未分发事件，未发布事件一定在其中）。`Branch` 与 `Session` 的同名读取以已发布位置为上限。

### 有界读取与恢复扫描

业务列表、历史接口、协作游标和导出流程由 App Server 定义。存储端口负责以下约定：

| 路径 | 读取边界 | 资源与校验 |
| --- | --- | --- |
| `context(through)` | 最近有效检查点及 `resume` 起的祖先链和本分支后缀 | 不扫描检查点之前的请求正文 |
| `local(range)`、`trace(sequence)` | 指定区间或命中事件的物理行 | 合并行完整解码后按逻辑序号过滤；trace 只返回指定事件的轨迹 |
| 分段读取 | 固定水位，按键 seek；内部读段可切开原子批次，`TimelineStore::read/read_segments` 的最终区间终点须为批次边界 | 每段结束释放连接锁；合并行完整解码后截取，不在锁内解码无限历史 |
| 恢复 | 固定 Session 目录和所有分支头，逐页校验事件、引用、祖先截点及检查点 | 每页缓存解码内容，页结束释放；跨页保留事件 ID 和目录校验信息 |

协作 `read_page` 的 `after` 和固定水位必须为批次边界；页长是目标值，末批超出时完整返回。内部读段可切开批次，但不把内部段尾作为业务游标。反向 `session.history` 仍为独立协议草案。

已装载实例采用发布水位，未装载记录采用已提交 head。公开 `local` 可读取完整范围，业务分页调用方必须传有界区间。当前范围读取每段 256 个逻辑序号，恢复校验每段 64 条物理行；查询索引及跨行规则见[存储格式](../../design/appserver/native-storage-format.md#分页查询)。

序列化、引用解析、hash 校验与类型转换集中在存储边界，能力只消费已验证类型。外部协议仍展开为原事件。序列化优化须测量正文复制、编码次数及耗时；不为优化增加业务状态。

### 压缩检查点

`context/compaction` 的 `Completed` 结果增加内部字段 `checkpoint: ProjectionCheckpoint`；v1 出站编码不含该字段，v2 不发送该事件。

| 字段 | 内容 |
| --- | --- |
| `resume` | 祖先链中的位置 `(branch_id, sequence)`：产生游标之后第一条历史条目的事件，不晚于该次准备的 `through` |
| `state` | 投影在 `resume` 之前的状态：prompt 条目与 revision、已投递的上下文基线、已提交但未采用的输入、最新 `context/configured` |

投影从 `state` 开始重放 `resume` 起的事件，并丢弃游标及其之前的条目。

### 使用方

| 使用方 | 读取 |
| --- | --- |
| Dispatcher | 热窗口；事件分发完即移出窗口 |
| Loop 构建 `context/prepare` | `context(event.sequence)`：配置、prompt revision 与历史投影 |
| ContextPreparation / ContextPolicy | `context(through)` 与 `local(through+1..)`；后者包含同一准备重试时已提交的压缩与预算事件 |
| ModelProvider | 不读取；执行事件中的请求 |
| Prompt 与配置发布 | `context` |
| 能力重载重建 | `local(..=S)` |
| TimelineTree 建分支 | `context(at)` 校验截点并得到子分支的祖先链 |
| 协作读取、App Server 历史、导出 | `local` 分页或 `history` |

- 每个模型步骤两次 `context` 历史读取，每次不超过最近一次压缩之后的历史；ContextPolicy 另读一次 `local(through+1..)` 同准备尾段，稳态下为空。
- Dispatcher handler 内不等待冷读取；需要历史的工作在能力自有任务中读取，完成后再提交事件。
- 热窗口只保存未发布和未分发的事件；进程内不保留已分发的事件；返回给调用方的 `Arc<AgentEvent>` 由调用方决定持有时长。

## 5. 分支、恢复与关闭

| 场景 | 约定 |
| --- | --- |
| 建分支 | 分支目录与 `branch/create` 在同一事务中提交；子 Timeline 以祖先链描述构造 |
| 恢复 | 按[存储格式](../../design/appserver/native-storage-format.md)校验物理格式与提交边界后，Timeline 以存储中最后的 sequence 为发布水位构造；能力按 `context` 或 `local` 重建 |
| Timeline close | 关闭前提交的事件全部发布后，才完成关闭 |
| Session close | 各分支关闭完成后才释放存储端口 |

## 6. 故障

| 故障 | 处理 |
| --- | --- |
| 提交失败 | 该轮按 Session 拆成单独事务重试；仍失败的 Session 进入存储故障：已排队未发布的事件丢弃，后续 `append` 返回存储错误，以 Session 故障进入清理，清理中不再写入终态事件；下次恢复按各能力的终态结算规则处理未完成操作。其他 Session 照常发布 |
| 读取失败 | 发起读取的操作以存储错误失败，并按该能力既有失败终态提交；不进入 Session 禁写状态，后续操作可重试 |
| 格式不可读 | 恢复时拒绝打开该 Session，保留存储数据 |

## 7. 验收

| 场景 | 判定 |
| --- | --- |
| 长会话驻留 | 2000 步、800 KB 上下文：空闲分支的存活申请字节不随步数增长；准备期间的峰值不随压缩前的历史增长 |
| 存储还原 | 写入后读出的事件与原事件逐字段一致，覆盖去重内容、输出行跨写入轮追加、原子批次与 before 轨迹 |
| 检查点 | 从检查点投影与从起点完整投影的结果一致，覆盖跨压缩的排队输入、prompt 变更、上下文基线和游标位于多结果工具事件内部 |
| 读取 | 每个模型步骤两次 `context` 读取和一次同准备 `local` 尾读；测量 p99、max 规模合成会话的每步耗时与 stop 分发延迟 |
| 写入 | 11 与 64 个流式分支下测量写入轮频率、发布延迟、WAL 写入量，以及 writer 队列、热窗口、trace 的实际存活字节；慢盘下记录产生与提交速率、峰值及恢复后的释放 |
| 进程中断 | 在事务提交前、提交后发布前、发布后终止进程：恢复结果符合第 3 节；sequence 连续；在途输出按失败结算 |
| 断电 | 以事务提交返回的事件为准，模拟断电后已发布事件完整 |
| 存储故障 | 分别注入读取失败与提交失败：前者提交当前操作的失败终态且允许后续操作，后者只清理写入失败的 Session；其他 Session 照常发布 |
| 分支 | 多层分支跨祖先链读取的顺序和截点正确；截点早于最近检查点、检查点位于祖先分支时读取正确；父分支关闭后子分支读取不受影响 |
| 分页与恢复 | 长历史页读不先解码全量；跨合并行、批次和跨页 ToolCall 正确；恢复缓存有界，长冷读不独占连接阻塞写入 |

### 结果与通知的独立提交

`exec/ended` 与通知输入分别提交：产生方使用 `append` 记录结果，在分发时向本分支追加带其预览的通知输入（标签 `exec-wake-result:{事件 ID}` 对应结果）（输入的合法性由产生方保证，Timeline 不校验）；后台能力在装载时补齐应唤醒却缺少通知的结果。唤醒条件见[BackgroundExec](plugins/BackgroundExec.md#事件)。Timeline 不判断任务状态，不提供专用生成式批次接口。

两次提交之间可以压缩：通知自带有上限的预览，不引用 `exec/ended`，投影检查点因此不保留任何结果事实；上下文注入一律经输入写入历史。固定草稿的 `append_batch_registered` 仍用于需要整体采用的配置，不改变其 before、登记回滚及批次水位合同。
