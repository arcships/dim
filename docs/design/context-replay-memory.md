# 上下文重建的内存计划

Status: Draft。交付状态在 issue，不在本文。
Issue: #708。

Scope: branch。
业务需求与技术需求的边界见[上下文拼装与识别：业务需求与技术需求](context-assembly-requirements.md)。

Rules: [design rules](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)。

## 1. 范围与非目标

范围：一次模型步骤里 `Timeline` → `HistoryRenderer` → `ContextPreparation` → `ContextPolicy` →
`ModelRequest` 这条重建链的分配形状，以及重放的边界。

非目标：

- 不减少模型步骤数，不改变压缩、快照、中断或配置采用语义。
- 不改变 Timeline 事件、存储格式、wire protocol 或 `ModelRequest` 的序列化。
- 不把历史重新做成常驻缓存。
- 不先做分配器调参；它只作为最后兜底（§8）。

## 2. 问题与证据

环境：`dimcode-server` 0.1.0-alpha.11（release manifest 的 sourceCommit `35b3a06`），产品 home，
Linux x86_64，glibc 2.39，运行约 1.5 小时。只读观察，未改产品数据。

| 指标 | 值 |
| --- | --- |
| RSS / 匿名 PSS | 1.62 GiB / 1.62 GiB |
| malloc system / free / in-use | 1726 MiB / 1619 MiB / **107 MiB** |
| `malloc_trim(0)` 后 RSS | **428 MiB**（释放约 1.20 GiB） |
| arena | 38 个；两个 764.5 / 488.2 MiB（峰值 809 / 1350 MiB） |
| 空闲块 ≥512 KiB | 273 个，合计约 872 MiB |

数据库：106 个 Session、13,831 个事件、969 次 `context/prepare`、963 次 `model/call`，
平均事件正文 1,419 B。

最大会话：

| 指标 | 值 |
| --- | --- |
| 事件数 | 4,903 |
| prepare 次数 | 436 |
| 累计重放事件（prepare 的 sequence 之和） | 1,086,523 |
| 最新输入 | 510,957 tokens / 1,000,000 window |
| `context/compaction` | 0 |
| `timeline_snapshots` | 0 |

最大会话的累计重放约 1.5 GB，与它所在 arena 的 1.35 GiB 峰值同一量级；这支持“该 arena
保留的空闲块来自这个会话的重放缓冲”。全库累计 2.2 GB 与 1.62 GiB RSS 相比，不能单独证明
所有 Session 都没有复用；跨 Session 的复用结论是 P0 要验证的假设，不是本文已证明的事实。

结论：

1. 重建结果确实被释放了：in-use 只有 107 MiB，`malloc_trim` 当场归还 1.2 GiB。
2. 最大会话的 arena 高水位与它的累计重放量同量级，说明这些缓冲在它的线程上没有复用；
   是否适用于所有 Session 由 P0 测量。
3. glibc 把这些空闲块留在 arena 里；动态 mmap 阈值让释放过的大块后续也走堆。
   这是保留现象，不是根因。

## 3. 现状：一次 prepare 实际造了几份

```text
Timeline::context_from
  └─ events: Vec<Arc<AgentEvent>>          整段历史物化              (1)
       └─ HistoryEntry { message: Message } 文本复制                  (2)
            ├─ context_messages()           整份克隆：前缀校验         (3)
            ├─ context_messages()           整份克隆：取长度           (4)
            ├─ context_messages()           整份克隆：组请求           (5)
            └─ ModelSettings::request()     每条消息再 Arc::new        (6)
```

对应代码：

| 位置 | 行为 |
| --- | --- |
| [timeline.rs:805](../../crates/dim-agent-runtime/src/core/timeline.rs#L805) `context_from` | 从 sequence 1 的整段历史装进 `ContextHistory.events` 和 `positions` |
| [history.rs:10](../../crates/dim-agent-capabilities/src/history.rs#L10) `HistoryEntry` | `message: Message`，文本由事件复制而来 |
| [history.rs:555](../../crates/dim-agent-capabilities/src/history.rs#L555) `context_messages` | 对每条消息 `entry.message.clone()`；有压缩时还克隆 `replacement` 和 `context_baseline[].message`（两者仍是 `Message`） |
| [context_preparation.rs:542](../../crates/dim-agent-capabilities/src/context_preparation.rs#L542) | 为校验前缀整份克隆 |
| [context_policy.rs:271](../../crates/dim-agent-capabilities/src/context_policy.rs#L271) | `context_messages(...).len()`，只为取长度整份克隆 |
| [agent_loop.rs:671](../../crates/dim-agent-capabilities/src/agent_loop.rs#L671) | prepare 路径组请求时的第三次整份克隆 |
| [host.rs:99](../../crates/dim-agent-capabilities/src/host.rs#L99) `ModelSettings::request` | `messages.into_iter().map(Arc::new)`，每条消息一次分配 |

`configured_request`（[context_policy.rs:221](../../crates/dim-agent-capabilities/src/context_policy.rs#L221)）也调用 `context_messages`，
但它出现在 usage（[context_preparation.rs:481](../../crates/dim-agent-capabilities/src/context_preparation.rs#L481)）和
compaction（[agent_loop.rs:359](../../crates/dim-agent-capabilities/src/agent_loop.rs#L359)）路径，不在一次 prepare 的三次克隆里。

每一步的缓冲都比上一步大（历史在增长），旧洞装不下新块；事件、投影、消息又交错分配，
释放后洞不连续。于是 `free` 了，但下一次用不上。

压缩边界本可以避免全量重放，但快照只在压缩完成后写入。这个会话 51 万 / 100 万 tokens，
尚未压缩，快照为 0，所以每一步都从 sequence 1 重放。

## 4. 目标

- 为取长度、校验前缀、组请求不再物化整份上下文。
- 一次 prepare 的**峰值**只跟批大小和投影有关，不跟历史长度成正比；读取总量仍是 O(历史)，CPU/IO 不承诺下降。
- 流式读取把一次 prepare 的峰值限制为一段，而不是整段历史。
- 重建次数、压缩与快照语义不变；重载后仍能从 Timeline 重建同样的投影。
- **单个 50 万 token 会话的自身常驻（消息、投影、索引）≤10MB live。** 这项不含 provider 在调用瞬间构造的序列化 body。
- 调用瞬间的 provider body/response 单独 ≤25MB；进程只加载一个这样的会话时 RSS ≤50MB，两个 ≤80MB，空闲进程 ≤30MB。

## 5. 方案

### P0 测量基线（先做）

已落地为 `xtask` 的 opt-in 命令（不进 CI）：

- `context-replay`：mock model 造 400–500k token、100 轮、无压缩，SQLite store；记录每轮 live/peak、RSS 和 close 值。
- `restore-replay DB SESSION`：加载真实会话的 SQLite 副本，跑一轮 mock，记录恢复/运行/close 和 `malloc_trim` 后的值。
- `context-breakdown DB SESSION`：把一轮拆成读事件、投影、`context_messages`、组请求四段，分别记录 live/peak。

实测（真实 `f1fab61a`，4,903 事件、51 万 token）：读事件 14.2 MB，投影 25.8 MB，消息 8 KB，请求 0.4 KB。
投影里 24.09 MB 是 baseline message 重复；改 `Arc` 后投影 3.34 MB，整轮峰值 45.4 → 22.1 MB。

### P1 共享消息，去掉深拷贝（#709）

- `HistoryEntry.message` 改为 `Arc<Message>`。
- `context_messages` 返回 `Vec<Arc<Message>>`，只克隆指针。
- `ModelSettings::request` 接收 `Vec<Arc<Message>>`，去掉 `map(Arc::new)`。
- `ModelRequest` 的序列化不变（它本来就是 `Vec<Arc<Message>>`）。
- 压缩的 `replacement: Vec<Message>` 保持原类型；`ContextBaselineEntry.message` 改为 `Arc<Message>`，
  事件 JSON 不变，4,720 条 baseline 快照共享文本。P0 显示 baseline 重复占 24.09 MB，改后投影从 25.8 MB 降到 3.34 MB。

净分配：当前 `ModelSettings::request` 已经对每个请求的每条消息做一次 `Arc::new`；把 `Arc` 放进
`HistoryEntry` 是用克隆替换这些分配，不是新增。P1 净减少的是 `context_messages` 的整文本克隆和
每条消息的 `Arc::new`。

收益：每次 prepare 少 2–3 份整文本拷贝；前缀校验与取长度可以直接读条目。
事件 JSON 不变，冻结 fixture 不动。

### P2 去掉元数据物化

- 前缀长度由 `replacement.len() + context_baseline.len() + history.len()` 算出，不再造 Vec。
- 前缀校验按引用迭代比较 `&Message` 与 `&Arc<Message>`，不克隆。
- usage/compaction 的 `configured_request` 也接收同一份 `Arc<Message>` 列表，不另造；它不在 prepare 路径上。

### P3 流式读取与增量投影（核心，#710）

不保留投影 cache，也不把整段历史装进 `ContextHistory.events`。每次 prepare 按批读取事件，
边读边投影，投影之外的批立即释放。没有跨 prepare 的派生状态，也没有缓存失效规则。

- **读取**：`Timeline::stored_context_page_in` 读取某个位置之后、带上下文开关的完整原子批次，
  目标是一批的事件数、永不在原子批次中间结束；`HistoryRenderer::rendered_stream`
  以 256 条为默认批大小，从快照的恢复位置或第一个有效段的段首顺序读取；不经过 hot cache，
  不保留事件体。
- **投影**：`project_batch` 接受上一批的 `ProjectionCheckpointState`，返回新的 entries/state；
  `rendered_stream` 合并 entries，事件只活到当前批。
- **压缩**：批次内遇到 completed compaction 时按 `retain_from` 裁剪已累计的 history；
  cut 在更早批次时按 `PolicyOrder` 在累计 history 上裁剪；找不到 cut 或数据损坏时报错，
  不再回退全量。
- **结果**：一次 prepare 的峰值 = 一批事件 + 投影，不随历史长度增长；读取从快照的恢复位置
  或第一个有效段的段首开始，只取参与投影的行。

#### policy 不再消费整份 ContextHistory

policy 需要的事实和窗口单独查询，不经过 `ContextHistory.events`：

- `PolicyFacts`：`context/configured`、`context/compaction`、`model/usage` 及其 `model/call`、
  当前 turn 的输入、预算；SQLite 直接选行，默认 store 用同一选择。
- `PolicyOrder`：只含 id、branch、sequence、turn start、compaction start；合并 ancestry，
  用于合法 cut 枚举和 `regrown`。
- `PolicyWindow`：锚点、当前 turn 输入、`input/consumed`、上一次 cut 之后的 tool-result；
  合并 ancestry，用于 pinning 和 digest。
- `ContextPolicy` 的输入改为 `PolicyContext`，只持有 configuration、compaction、order、
  streamed checkpoint 和可选的 full context；`full` 只用于显式的 `rendered_context` 读取，
  准备路径总是用默认投影。
- `compact_step` 的 snapshot 由 `stream_checkpoint` 从最近的有效快照折叠到 cut。

#### 与快照的关系

持久快照和 `context_with_checkpoint` 保持原样；流式读取不依赖快照。
`stream_checkpoint` 只在需要写新 snapshot 时重放到 cut，不把完整 history 留在内存。

### P4 冷重建分段（并入 P3）

P3 已经覆盖冷重建：读取按批进行，不再一次物化整段历史，因此不再单列 P4。

## 6. 验收与测量

| 项 | 方式 |
| --- | --- |
| 现有流程 | `context_flow`、`context_policy`、`storage_flow` 不变通过 |
| 流式/全量一致 | 在固定前缀上分别用流式批读取和全量 `rendered_context`，entries、由它们组出的 messages，以及 configuration 与 prompt 必须一致（复用 checkpoint_projection 的测试模式） |
| 失败路径 | 无缓存可推进；读取、解码、前缀校验失败只影响本次 prepare，下一次仍从全量开始 |
| 每步分配 | P0 命令：改动前后的 live/peak 曲线 |
| 流式峰值 | 峰值只跟批大小和投影有关，不随历史长度增长；用 `restore-replay` / `context-breakdown` 对照 |
| 单会话常驻 | 50 万 token 会话自身 ≤10MB live（消息、投影、索引；不含 provider 调用 body） |
| 调用瞬间 | 会话自身 ≤10MB；provider body/response ≤25MB |
| 进程 | 一个会话 ≤50MB RSS；两个会话 ≤80MB；空闲 ≤30MB |
| 最终结果 | `context-replay` peak 8.35MB；真实会话 prepare peak 11.53MB；单会话 RSS 50.8MB，双会话 54.6MB，空 daemon 29.7MB |
| 产品验证 | `dimcode app-server` 加载真实 SQLite session，mock model 跑一轮后测 RSS；allocator 只作为 backstop |
| 堆高水位 | P0 验证最大会话的堆高水位是否随步数增长；不是断言，是测量 |
| 兼容 | `cargo test --workspace --locked`；`xtask check` 不需要 `graph` 变更 |

不设 CI 字节阈值；数值写进 PR。

## 7. 兼容性

- 无事件、存储、协议或配置变化；`ProjectionCheckpoint`/`ProjectionCheckpointState` 的序列化不变。
- `ModelRequest` 序列化不变；冻结的事件 fixture 不动；压缩 `replacement` 仍是 `Message`，
  `ContextBaselineEntry.message` 改为 `Arc<Message>`，事件 JSON 不变。
- 改动限于 capabilities 的内部类型，以及 runtime 新增的批量读取、policy facts/order/window 查询。
- P3 不改变事件、快照、压缩契约；准备好整条分支链统一从位置起读，没有全量回退。
- `PolicyContext` 是 crate 内部输入，不新增事件、服务、配置项或持久状态。
- `Timeline` 的快照和压缩契约不变；读取新增分页入口，现有读取语义不变。

## 8. 风险与备选

| 项 | 说明 |
| --- | --- |
| P3 的一致性 | 流式投影必须与全量 `rendered_context` 在固定前缀上一致；读取/解码/校验失败报错，不回退；没有跨 prepare 的缓存状态 |
| P3 的内存 | 峰值 = 一批事件 + 投影；不保留历史；读取从快照的恢复位置或段首开始，只取参与投影的行，压缩之后的读取量随裁剪点而不是历史长度增长 |
| policy facts | SQL 选择必须与默认 store 相同；按 ancestry 合并；`input/consumed` 引用的输入即使写在 turn 之前也要选中 |
| policy 窗口 | cut 必须落在窗口内；跨 batch cut 在累计 history 上裁剪；找不到或损坏时报错 |
| restore | `Dispatcher::follow` 仍为每个 consumer 读完整本地历史，restore peak 约 19.9MB；这是后续分页 replay 的工作，不影响 prepare |
| 代码页 | 产品二进制 `.text` 约 60MB；空 daemon RSS 29.7MB，单会话 50.8MB；代码页是 RSS 下限，不是 allocator 能解决 |
| P1 的分配 | `HistoryEntry.message` 和 `ContextBaselineEntry.message` 共享 Arc；baseline 重复从 24.09 MB 降到共享 |
| Provider body | aimux 实测 946 条消息/1.9 MB → 10.3 MB 峰值；不是 100 MB 大头。私有 provider 只提供租约/连接，执行走 aimux |
| 分配器兜底 | `malloc_trim` 实测 78.7 → 49.1 MB RSS；成本随 heap，需低频/阈值触发，不能每轮调 |
| 备选：更快压缩 | 改变上下文质量与成本，不在本计划内 |
| 未做 | 跨 Session 共享投影、磁盘投影快照、快照频率调整 |

## 9. 规则检查

- **§4 rule 1**（改别的模块的状态要写指令事件）：不适用。投影是 branch 内由 Timeline 事实派生的状态，
  不改变其他模块的状态，不新增指令事件或结果事件。
- **rule 2**（交接后不能撤回）：不适用，交接没有变化。P3 只按已提交事件顺序流式重放；
  不提交派生状态，取消或失败只丢弃当前批，下一次仍从全量开始。
- **rule 3**（before 只同步变换草稿）：不适用，没有 before 回调。
- **rule 4**（核心机制不带业务含义）：不适用，Timeline、Dispatcher、批次不改；投影在 capability 内。
- **rule 5**（原子批次一起提交、按序派发）：不适用，不新增批次、不写事件；流式读取按已提交事件的序号处理。
- **rule 6**（模型上下文只包含交接后被读取的事件）：满足。流式投影与全量 `replay` 使用同一套
  采纳/消费规则；`ProjectionCheckpointState` 仍由现有 `replay` 维护；测试在固定前缀比较流式与全量结果。
- **rule 7**（插件只通过公开事件和服务交互）：满足。流式读取由 `HistoryRenderer`/capability 使用
  `Timeline` 的公开分页入口，不读其他能力的状态；投影不是新的 owner 状态。
- **rule 8**（基础设施默认不变）：**不满足**。P3 需要 `TimelineStore`/`Timeline` 的分页读取，
  policy 需要 facts/order/window 查询，属于 runtime 机制变更，按五项格式提出：
  - 被挡住的规则：模型上下文必须能从已提交事件重建，同时一次 prepare 的峰值不能是整段历史。
  - 考虑过的机制：内存投影 cache（引入失效状态）、持久投影快照（跨进程但仍需整段解码）、
    一次性全量读取（峰值 45 MB）。
  - 通用能力：`TimelineStore` 的分页读取、policy facts/order/window 查询，以及
    `HistoryRenderer` 的批量续接；不是业务能力。
  - 对序号、批次、中断、重载的影响：只读已发布事件；不新增批次或事件；中断丢弃当前批；
    重载仍能重建同样的投影。
  - owner 与 issue：runtime owner，#710。
- **rule 9**（只有后续决定要用的事实才进 Timeline）：不适用，流式投影是内存态，不写事件。
- **ADR 002 D1–D5**：branch 作用域；不新增 wiring 模块、不新增配置项、不改生命周期；投影只由
  branch 读取并重建，接口保持语言中立。

## 10. 删减检查

- P4 已并入 P3：按批读取就是冷重建分段，不新增一条实现路径。
- P1 把 `HistoryEntry.message` 和 `ContextBaselineEntry.message` 换成 `Arc<Message>`，
  压缩 `replacement` 仍是 `Message`，不新增包装类型。
- P3 复用现有 `ProjectionCheckpointState`/`replay`，不新增持久状态、缓存层或失效规则；
  新增的只有 runtime 的分页读取入口。
- 不新增通用内存统计接口；测量只存在于 opt-in example。
- 不为了压低 RSS 而改压缩阈值；allocator 只在 RSS 阈值/低频 idle 后 trim，作为 backstop。
