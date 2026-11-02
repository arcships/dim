# 恢复期间共用的历史读取

状态：已在 [#235](https://github.com/arcships/dim/pull/235) 实现；§17（TimelineTree 的恢复事实，[#239](https://github.com/arcships/dim/issues/239)）在此基础上扩展。[#193](https://github.com/arcships/dim/issues/193) 的第 3 阶段。第 1 阶段是在一次读取的各页之间共用有界内容缓存，已以 [#213](https://github.com/arcships/dim/pull/213) 合入；第 2 阶段是请求消息列表（原生格式 v17），已以 [#224](https://github.com/arcships/dim/pull/224) 合入，其[设计](request-delta-storage.md) §15 把本阶段留给运行时。§2 至 §8 的数据是在 #224 的分支上用原型测得的；§8 末尾是实现的数据。这项改动属于运行时基础设施（[原则 §4](../../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 第 8 条），须经运行时机制所有者评审，即核心机制负责线（eric8810，[路线图 §3.5](../../../design/roadmap.md#35-按人按批次)）。作者负责线：Agents/存储（yyhhyyyyyy）。评审：eric8810、zerob13。范围：`Timeline` 的已提交历史读取路径（[timeline.rs](../../../../crates/dim-agent-runtime/src/core/timeline.rs)），由分支作用域开始共用（[assembly.rs](../../../../crates/dim-agent-runtime/src/assembly.rs)），分支离开 `Loading` 时由该分支的 `BranchLifecycle` 结束共用（[config.rs](../../../../crates/dim-agent-runtime/src/config.rs)）。

## 1. 概要

v17 之后，恢复一个 Session 仍要把每个分支的历史解码十来次：存储校验一次，TimelineTree 扫描一次，分支打开期间每个重建的能力再各读一次（§2）。这些读取返回的是同一批不可变事实，重复的只是解码。

- **规则。** 分支打开期间，它的 Timeline 用一份解码副本来服务对“该分支构建时已有历史”的读取。打开期间就是分支生命周期的 `Loading` 阶段：从分支作用域以存储头部 `h` 构建 Timeline 开始，到分支被激活（发布）、开始关闭或卸载、或构建回滚为止。副本保存分支自己的事件 `1..=h`；子分支还保存每个祖先截至截点的前缀（§4）。
- **能力照旧自己读。** 它们固定 `S = committed()`，读取 `local(..=S)`；不向它们推送任何东西。第一个需要整段前缀的读取用现有的存储读取解码它，同时到达的读者等它完成；每个读者拿到自己的 `Vec`，里面是同一批 `Arc<AgentEvent>`，与实时分发把同一个 `Arc` 交给每个订阅者一样。副本保存每条事件的批次终点，所以终点落在原子批次内部的区间会以存储的 `InvalidInput` 失败（§4.3）。`h` 之后的事件（打开期间写入的结算终态、`branch/resume`）照现在的方式读取。打开期间之外的读取不变。
- **时间和大小都有界。** 副本只填充一次，不再增长：它就是一次 `local(..=h)` 的结果，而现在每个重建的读者都各持一份这样的结果，另加每条事件的批次终点。打开期间结束即丢弃，所以副本不会活过发布，空闲分支持有的内容与现在相同（§5）。
- **失败。** 填充失败不会被保留：每个读者要么拿到副本，要么拿到自己那次读取的结果，包括错误（§6）。
- **效果。** 在 #224 的分支上、约 10,000 条事件时，原型让恢复加首次 idle 从 469 ms 降到 126 ms（plain）、从 527 ms 降到 141 ms（tools）、从 496 ms 降到 170 ms（product）；实现相对 v17 分别从 468 ms 降到 129 ms、从 523 ms 降到 141 ms、从 492 ms 降到 169 ms。每次恢复的全量历史解码从 12 次降到 3 次（校验、TimelineTree 扫描和填充）；§17 让 TimelineTree 从副本读取恢复事实后降到 2 次。恢复期间的堆峰值降到约三分之一，首次 idle 时的存活字节不变（§8）。

不变的部分：Timeline 作为每个分支的时间总线，Dispatcher 及其热窗口，能力重建合同，`local`、`history`、`context`、`committed` 的签名和结果（包括错误），事件类型与冻结样本，能力和插件代码，存储及其格式，以及所有协议。不新增事件、服务、方法、配置或公开 API；`testing` 特性为测试增加一个计数（§9）。

## 2. 问题与依据

### 2.1 v17 之后的恢复耗时

#224 测量了 v16 与 v17 的恢复加首次 idle（见其 PR 正文；Apple M4，release，两轮交错、每轮七次取中位数的平均）：约 10,000 条事件时，plain 从 1,697 ms 降到 452 ms，tools 从 2,088 ms 降到 510 ms，一次全量历史读取从 139 ms 降到 35 ms、从 171 ms 降到 40 ms。单次读取的成本降到四分之一，读取次数没变。10,000 条 plain 事件时，v17 的一次恢复仍有 469 ms 中的 403 ms 花在 11 次全量历史读取上，校验 39 ms，其余 27 ms（§8）。

### 2.2 恢复包含哪些步骤

恢复从 `Agent::restore_session` 开始，到 Session 发布结束。调用方之后做的事在恢复之外，也不改变：`restore_cost` 等待的首次 `idle` 没有存储调用，Agents 的 `acquire` 则在已发布的 Session 上通知它的 `SessionRestored` 监听者（Collaboration）。中间的步骤：

```text
Agent::restore_session(id)
  store.session_config(id)
  Agent::create(Restore)，在其构建任务中
    store.branches(id)                     存储校验每个分支和每一行               ........ 读取 A
    build_session：Session 作用域与 Session 能力（所测装配中都不读分支历史）
    TimelineTree::restore：逐个分支，父分支在前
      对存储头部为 h 的分支 b：
        store.read(b, 1..=h)               TimelineTree 自己的扫描                 ........ 读取 B
        仅子分支：read_context(父分支的 Timeline, cut)  校验截点              ........ 读取 C
        TimelineFactory::build
          store.head(b)
          BranchScope：Timeline(b, 祖先链, h)；Dispatcher 尚未运行               ─┐
          能力按依赖分波 apply；各自从一次读取重建                                │ b 的打开期间
            （见下表）；部分能力追加结算终态                                      │（BranchLifecycle
          TimelineContext::start：开始分发                                        │  Loading）
        追加 branch/resume，等它分发完                                            │
          打开处理器：ToolBinding 对齐工具，Prompt 采用初始配置；各读一次上下文   │
        finish_opening：打开回执，然后是已提交位置的围栏                          │
        TimelineContext::activate：分支发布                                      ─┘
    Session::start，entries 置为 Ready，Session::activate：Session 发布
```

`restore_cost` 所测的 `defaults::install` 装配中，一个分支打开期间的读取（最后几行是产品部分）：

| 读者 | 调用 | 读取范围 | 时机 |
| --- | --- | --- | --- |
| ContextPreparation | `local(..=S)` | 本分支 `1..=S` | apply，第 1 波 |
| InputQueue | `local(..=S)` | 本分支 `1..=S` | apply，第 1 波 |
| Permission | `history(Some(0))`、`local(..=S)` | 祖先链，本分支 `1..=S` | apply，第 1 波 |
| ModelProvider | `local(..=S)` | 本分支 `1..=S` | apply，第 2 波（依赖 ContextPreparation） |
| Approval | `local(..=S)` | 本分支 `1..=S` | apply，第 2 波（依赖 Permission） |
| Prompt | `read_context(committed())` | 祖先链和本分支 `1..=S`，或从检查点的 `resume` 起 | apply，第 2 波（依赖 ContextPreparation 提供的 `ConfigurationPublisher`） |
| ToolBinding | `read_context(sequence())` | 同 Prompt | apply，第 3 波（依赖 Prompt） |
| Loop | `local(..=S)` | 本分支 `1..=S` | apply，第 4 波（依赖 ToolBinding） |
| ToolExecutor | `local(..=S)` | 本分支 `1..=S` | apply，第 4 波（依赖 ToolBinding、Approval） |
| ToolBinding | `read_context(sequence())` | 同 Prompt，到 `branch/resume` 为止 | `branch/resume` 的打开回执 |
| 带初始配置的 Prompt（product 配置） | 采用时 `read_context(event.sequence)`，读到 `branch/resume` 事件；发布时 `read_context(sequence())` | 同 Prompt，到 `branch/resume` 为止 | `branch/resume` 的打开回执 |
| Wait、BackgroundExec、Collaboration（产品装配，不在 `restore_cost` 中） | `local(..=S)` | 本分支 `1..=S` | apply |

一次 10,002 条事件的 plain Session 恢复的跟踪（`restore_cost` 加 `RESTORE_COST_SPANS`，列出每次存储调用及其范围；读者按依赖关系推断）：

```text
距 restore_session 的 ms   存储调用                           读者
v17
    0-40                  branches                           校验，读取 A
   40-79                  read 1..=10002                     TimelineTree 扫描，读取 B
   81-193                 read 1..=10002  x3，时间重叠       ContextPreparation、InputQueue、Permission
  198-309                 read 1..=10002  x3，时间重叠       ModelProvider、Approval、Prompt
  315-354                 read 1..=10002                     ToolBinding
  357-431                 read 1..=10002  x2，时间重叠       Loop、ToolExecutor
  435-438                 append branch/resume
  438-476                 read 1..=10003                     ToolBinding，branch/resume 时
  479                     Session 发布；首次 idle 随即返回，没有存储调用
原型，同一个 Session
    0-79                  branches、read 1..=10003           读取 A、读取 B
   82-120                 read 1..=10003                     填充；上面每个读者都由它服务
  122-127                 append branch/resume
  127                     read 10004..=10004                 ToolBinding 在 h 之后的尾段
  129                     Session 发布
```

- **串行还是并发。** rutis 在依赖都 Active 后才 apply 一个能力，所以重建读取分成四波，一波接一波。同一波的读取在时间上重叠，但按段轮流使用存储的连接：三个重叠的读取每个约 111 ms，单独一次是 38 ms。分支也是一个接一个打开：TimelineTree 等前一个分支完成后才读取下一个分支的事件。
- **`h` 之前的 `S` 相同。** Timeline 以最后存储的序号 `h` 构建；没有追加时 `committed()` 就是 `h`。重启前留有未完成的工作时，部分能力在 `apply` 中追加结算终态（ToolExecutor、ContextPreparation、ModelProvider、InputQueue 欠写的决定、BackgroundExec 的唤醒输入；[单树装配](../../../architecture/rust-agent/single-tree.md#能力重载协议)“装载：失败判定”），于是后面几波的读者固定的 `S` 更大。并非每个读者都读到 `committed()`：ToolBinding 和 Prompt 的发布读到 `sequence()`，即已发布序号，终态尚未发布时它小于 `committed()`；Prompt 的采用读到 `branch/resume` 事件；上表逐一列出。这些位置都不小于 `h`，所以每个读者的范围都以同一段 `1..=h` 开头，只在打开期间写入的短尾上不同。
- **热窗口还是存储。** 构建时热窗口为空，已发布序号就是 `h`，所以 `1..=h` 来自存储。打开期间追加的事件进入热窗口（Dispatcher 在 `TimelineContext::start` 之前不运行），写入任务提交后它们也在存储中。Dispatcher 自己只读热窗口。

### 2.3 倍数

上表的每个读者都各自解码 `1..=h`。v17 下每次这样的读取约等于一次全量读取，所以有 `n` 个读者的分支打开期间，除校验和树的扫描外还要做 `n` 次全量解码。plain 与 tools 配置中 `n` 是 10（apply 中 9 个，`branch/resume` 时 1 个），所以一次恢复要解码 12 次历史；产品装配每多一个重建的能力就多一次。成本主要在解码：#193 的剖析中，折叠本身在 10,000 条事件时最多 21 ms。

## 3. 约束

每行写明一项合同、它的出处，以及本设计靠什么机制保持它。

| 合同 | 出处 | 本设计如何保持 |
| --- | --- | --- |
| Timeline 是每个分支的时间总线；能力经 Dispatcher 消费它，并通过读取它来重建 | [时间总线](../../../architecture/rust-agent/time-bus.md)、[timeline-storage §1](../../../architecture/rust-agent/timeline-storage.md#1-结构与所有权) | 副本位于 Timeline 的读取函数之后。Dispatcher、热窗口和订阅都不动 |
| 重建：在重建闸门内固定 `S = Timeline::committed()`，读取 `local(..=S)`（本分支事件，不含祖先前缀）；读取覆盖旧代写出、尚未发布的终态，不等待持久化或 Dispatcher | [单树装配，能力重载协议](../../../architecture/rust-agent/single-tree.md#能力重载协议)“装载：重建” | 能力以同样的 `S` 调用同样的函数，拿到同样的事件。副本只覆盖 Timeline 存在之前就已存储的 `1..=h`；`h` 之后的一切，包括未发布的终态，照现在的方式从热窗口和存储读取 |
| `local`、`history`、`context`、`context_with_checkpoint`、`committed` 的签名与语义 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#4-读取) | 不变，包括错误。`through` 照旧检查。切片按序号选取，所以正好是存储对该区间的读取所返回的事件；切片只在通过存储的终点检查（下一行）后才提供 |
| 区间起点可以位于原子批次内部；终点必须是批次边界，否则读取以 `InvalidInput` 失败 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)“分段读取”行；`TimelineStore::read`（[timeline_store.rs](../../../../crates/dim-agent-runtime/src/timeline_store.rs)） | 现在由存储用 `StoredEvent.batch_end` 检查终点，而恢复后的热窗口里没有 `1..=h` 的任何记录。副本保存填充读到的每条事件的批次终点，并在提供每个切片（本分支或祖先）之前对其终点做同样的检查（§4.3） |
| 事件分配序号后即是事实；先后只看序号 | [时间总线 §1、§2](../../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实) | 副本依赖这一点：`1..=h` 的事件及其批次终点不会改变，所以之后取的、经过终点检查的切片等于之后做的读取 |
| 只共享不可变事实；不推送重放；不做能力快照；不共享能力状态 | [时间总线 §2](../../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实)（每份状态只有一个所有者，只依据自己消费到的事件决定）；[timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)（不为优化增加业务状态） | 副本保存的是实时分发交给每个订阅者的 `Arc<AgentEvent>`（`Committed(Arc<AgentEvent>)`），每条带着存储随它返回的批次终点，不含任何能力状态。每个读者自己调用读取，折叠进自己的状态 |
| 有界读取：分段读取，每段结束释放连接 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | 副本由现有的存储读取填充，带着它的分段和 #213 的有界内容缓存 |
| 检查点读取不扫描快照 `resume` 之前的请求正文 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描) | 不从序号 1 开始的读取从不填充副本：已有副本时取切片，否则从 `resume` 自己读取 |
| 恢复缓存有界；校验的缓存在校验结束时释放 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)“恢复”行、[§7](../../../architecture/rust-agent/timeline-storage.md#7-验收)“分页与恢复” | 副本大小在填充时即固定：一次 `local(..=h)` 的结果（子分支再加一次祖先链读取），现在每个重建的读者都已各持一份，另加每条事件 8 字节的批次终点。它不再增长，打开期间结束即丢弃 |
| 进程内不保留已分发的事件；返回的 `Arc<AgentEvent>` 持有多久由调用方决定 | [timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#使用方)最后一条 | 打开期间 Timeline 为它的读者持有一次读取的结果；打开期间之后不再持有（§15 已随实现补上这句） |
| 空闲分支的存活字节不随步数增长 | [timeline-storage §7](../../../architecture/rust-agent/timeline-storage.md#7-验收) | 副本活不过打开期间；首次 idle 时的存活字节与 v17 相同（§8） |
| 读取失败时，发起读取的操作以存储错误失败，按能力的失败终态结算；不让 Session 禁写 | [timeline-storage §6](../../../architecture/rust-agent/timeline-storage.md#6-故障) | 填充失败不保留；每个读者拿到自己那次读取的错误（§6） |
| 副作用不重放；新一代从明确的序号位置接手 | [时间总线 §5](../../../architecture/rust-agent/time-bus.md#5-副作用不重放) | 不变：能力仍从各自的 `S` 接手 |
| 事件类型与冻结样本；插件代码 | [原则 §6](../../../architecture/rust-agent/principles.md#6-验证)、AGENTS.md 规则 6 | 不动 |

## 4. 机制

### 4.1 范围：分支打开期间

副本属于一个分支的 `Timeline`，只在该分支处于 `Loading` 时存在：

| 时点 | 位置 | 效果 |
| --- | --- | --- |
| 分支作用域以 `h` 构建 Timeline（`BranchScopePlugin::apply`） | assembly.rs | 打开期间开始，带着分支的祖先链和 `h`，由该分支的 `BranchLifecycle` 持有；此时不读取任何东西。`h` 为 0 且没有祖先链的分支没有可共用的内容，不开始打开期间 |
| 分支离开 `Loading`（`BranchLifecycle` 推进到 `Active` 或 `Closing`） | config.rs | 生命周期丢弃打开期间。一个信号覆盖所有出口：`TimelineContext::activate`（发布）、`TimelineContext::finish` 开始时（关闭或卸载：失败、取消、恢复期间关闭 Session），以及 `TimelineContext` 建立之前就失败的构建的回滚 |

Timeline 只持有打开期间的弱引用，所以打开期间随阶段变化结束，不需要自己的调用点；生命周期随作用域一起丢弃时也会丢弃它。

恢复一个接一个地打开分支，所以恢复期间一个 Session 同时至多有一份副本，最后一份在 Session 发布之前丢弃。运行时新建的分支各自独立打开：并发构建的子分支各持一份自己祖先链的副本（§5）。装配遵循本次打开范围内的无进展间隔（[How Dim uses rutis](../../../design/rutis-usage.md#appendix-a-rust-binding)）；行首次激活、打开派发序号前进和回执完成会重置间隔。存储读取有 I/O 取消预算，重放没有固定总期限。失败或取消会回滚打开过程并离开 `Loading`。

打开期间是运行时自己的单位；恢复就是一个所有分支都以 `branch/resume` 打开的 Session。同一个打开期间也服务运行时新建的分支（TimelineTree 的 `branch`、`create_main`）和预先准备的 Session（fork）：新的子分支自己没有事件，但打开时会把祖先链重读好几次（§7）。运行时不需要知道它构建的是哪一种。

### 4.2 副本保存什么

- **本分支部分。** 分支的事件 `1..=h`，按序号排列，每条是一个由事件及其批次终点组成的条目。
- **祖先链部分。** 子分支的每个截点大于 0 的祖先的条目 `1..=cut`，按祖先链顺序。
- **事件与批次终点。** 一个条目就是填充读到的一条去掉轨迹的 `StoredEvent`：`Arc<AgentEvent>` 及其 `batch_end`，即该事件提交时所在原子批次的最后一个序号。存储的区间检查读的就是批次终点（§4.3），而恢复后的热窗口里没有 `1..=h` 的批次终点，所以副本保存它，每条事件 8 字节。不保存 before 轨迹：经此路径服务的读取（`local`、`history`、`context`）只返回事件；`trace` 保留自己的读取。这些 `Arc` 就是存储读取产生的，所以读者保留某个事件时，保留的正是它自己读取后会保留的东西。
- **固定。** 每部分至多填充一次，从不扩展；`h` 和各截点在 Timeline 构建时就已固定。

### 4.3 服务一次读取

Timeline 的每次已提交历史读取都经过同一条内部路径，它把请求拆成热窗口部分和存储部分；`local`、`history`、`context`、`context_with_checkpoint` 以及 `Dispatcher`、`Branch`、`Session` 上的包装都走到这里。只有存储部分改变。每次对已存储区间的读取，无论本分支还是祖先，都经过下面两个接缝之一，两者执行同样的终点检查：

```text
本分支的存储部分 a..=b          （b <= 已发布序号，同现在）
  不在打开期间，或 a > h ............................ store.read(a..=b)               同现在
  a == 1 且 b >= h ............... 填充本分支部分一次 ─┐
  其他区间，本分支部分已填充 .........................─┴─> slice(本分支部分, a..=min(b, h))
                                                         再接 store.read(h+1..=b)，若 b > h
  其他区间，本分支部分未填充 ........................ store.read(a..=b)               同现在

祖先部分 [(branch, a..=b), ...]
  来自：history 和 context 的祖先段；history 的单段捷径（只有一个祖先段，例如新子分支的
  history(Some(0))）；context_with_checkpoint 的锚点读取，锚点在祖先上时
  不在打开期间，或区间超出祖先链 ...................... store.read / read_segments    同现在
  每个祖先，各自 1..=cut ........... 填充祖先链一次 ─┐
  其他组合，祖先链已填充 ............................─┴─> 每段一个切片
  其他组合，祖先链未填充 ............................ store.read / read_segments    同现在
```

- **每次存储读取。** 本分支接缝服务对本分支已存储事件的每次读取：`local`、`history` 和 `context` 的本分支段，以及锚点在本分支上时 `context_with_checkpoint` 的锚点读取（`records(branch, snapshot.sequence, snapshot.sequence)`，锚点行位于原子批次内部时改为读取该段）。祖先接缝服务对祖先的每次读取：`history` 和 `context` 的祖先段、`history_records` 的单段捷径（只有一段时经按分支的读取而不是 `read_segments` 读取；新子分支的 `history(Some(0))` 就是这样的读取），以及锚点在祖先上时的锚点读取。Timeline 中没有其他读取已存储事件的路径。

- **按序号切片。** 切片取序号落在区间内的条目（在 `event.sequence` 上二分查找），所以其中的事件就是存储对该区间的返回：每个序号一个事件，合并的输出行已拆成各自的事件。
- **终点检查。** 提供切片 `a..=e` 之前（本分支部分的 `e` 是 `min(b, h)`，祖先段的 `e` 是该段的终点），先应用存储对区间的规则：起点可以位于原子批次内部，终点必须是批次边界。若 `e` 处条目的 `batch_end > e`，读取以 `AgentError::InvalidInput("sequence {e} is inside an atomic batch")` 失败，即两种存储（`MemoryTimelineStore` 与 SQLite 存储）返回的错误，不提供任何事件。若序号 1 和 2 是一个已持久化的批次且 `h >= 2`，无论副本是否已填充，`local(..=1)`、`history(Some(1))` 和 `context(1)` 都会失败，而 `local(..=2)` 返回事件 1 和 2，`local(2..=2)` 返回事件 2。`h` 是批次边界，所以 `b > h` 时缓存部分总能通过检查，`b` 由现有的 `h` 之后的读取检查。每个祖先切片都用同一个函数执行同样的检查：祖先段终止于它的截点，填充读取已经检查过截点，所以现有读者都不会让它失败，但没有切片会绕过它被提供。
- **`h` 之后。** `h` 之后的部分从不由副本提供，而是由现有代码从热窗口和存储读取，所以 `S` 更大的读者照现在的方式看到打开期间写入的终态。
- **子区间。** `a > 1` 的 `local(a..=b)`、从 `resume` 起的检查点读取和检查点锚点读取，无论本分支还是祖先，在副本存在后是切片，之前是普通读取。它们从不填充副本：不从序号 1 开始的读取不会导致它之前的前缀被解码。
- **不同的 `S`。** 读者各自固定自己的 `S`；副本只替换每个这样的区间开头那段已存储的前缀 `1..=h`。

### 4.4 单次填充

每部分至多填充一次，由第一个需要整段的读取完成；填充进行中需要它的读者等这次填充，而不是自己再读一次。这就是 `tokio::sync::OnceCell::get_or_try_init`，它的语义正是本设计需要的：

- 负责填充的读者在不持锁的情况下执行存储读取；等待者等在这个单元上，而不是 Timeline 的数据锁上，所以追加和分发从不等待填充。
- 填充失败时单元保持为空，负责填充的读者拿到错误，下一个等待者执行自己的读取（§6）。所以等待者一个接一个重试：存储持续失败时，`n` 个等待的读者依次产生 `n` 次失败的读取，每个读者拿到自己的错误；没有副本时同样这些读取会并排失败。
- 负责填充的读者被取消（其 apply future 被丢弃）时，由下一个等待者填充。
- 填充只等待存储，存储调用从不等待 Timeline 的读者，所以无论等待者持有哪个重建闸门，都不会等到自己身上。

### 4.5 释放

分支的 `BranchLifecycle` 持有打开期间，分支离开 `Loading` 时将其丢弃（§4.1）；Timeline 只保留弱引用。在此之前已经拿到副本的读者切完片后丢弃自己的引用，所以最后一个这样的读者返回时副本的内存即被释放。打开期间之后，读取照旧走存储。副本没有其他持有者：它只属于 Timeline 及其生命周期，从不交给能力，也不在 Timeline 之外按键缓存。

### 4.6 打开期间之外的读者

不变，因为没有副本：活跃分支中的能力重载、模型步骤（`context`、ContextPolicy 的 `local(through+1..)`）、Prompt 与配置发布、TimelineTree 在父分支上的 `branch` 校验、App Server 的历史分页与摘要、协作和导出。

## 5. 驻留

现在每个读者从读取到折叠返回一直持有自己解码的历史，同一波的读者在时间上重叠，所以恢复的峰值同时持有好几份解码历史，波次之后还有打开处理器的上下文读取。有了副本，打开期间从第一次填充到分支离开 `Loading`（包括打开处理器，受 §4.1 的装配和 I/O 策略约束）持有一份解码历史；每个读者折叠时只多一个指针向量（每条事件 8 字节）。

用 `restore_cost` 中的计数分配器测得（§8）：恢复及其首次 idle 期间，堆峰值高出恢复之前的量，单位 MB。

| 配置 | 事件数 | `main`（v16 + #213） | #224（v17） | #224 + 原型 |
| --- | --- | --- | --- | --- |
| plain | ~1k | 3.9 | 5.4 | 2.4 |
| plain | ~3k | 18.2 | 17.8 | 6.6 |
| plain | ~10k | 110.7 | 95.9 | 32.7 |
| tools | ~1k | 4.4 | 5.9 | 2.6 |
| tools | ~3k | 21.1 | 20.1 | 7.5 |
| tools | ~10k | 134.7 | 112.5 | 38.1 |
| product | ~1k | 4.9 | 6.8 | 3.0 |
| product | ~3k | 18.0 | 19.7 | 7.3 |
| product | ~10k | 65.3 | 69.2 | 23.7 |

首次 idle 时，三个构建、所有配置和规模下的堆都比恢复前高 0.30 到 0.33 MB，Session 卸载后高 0.10 MB。原型在每次恢复的首次 idle 时都没有存活的打开期间。

- **不比现在差。** 副本就是每个读者本来就会生成的那份结果之一，另加它的批次终点；读者自己的结果变成指针向量。读者保留某个事件时，保留的是它现在也会保留的同一个 `Arc`。
- **批次终点。** 表中的原型只保存事件。§4.2 的批次终点让打开期间持有的那一份副本每条事件最多增加 8 字节，10,000 条事件时为 0.08 MB。在一个约 10,000 条事件的 plain Session 上，不保存与保存批次终点交错各跑两轮、每轮恢复七次：每一轮的堆峰值都是 32.69 MB，首次 idle 时的堆都比恢复前高 0.30 MB。
- **不活过发布。** 副本在分支离开 `Loading` 时丢弃；首次 idle 时的存活字节与 v17 相同。
- **长会话驻留。** 空闲分支不持有副本，所以它的存活字节不随步数增长（[timeline-storage §7](../../../architecture/rust-agent/timeline-storage.md#7-验收)）。
- **子分支。** 子分支打开期间持有自己的事件和一次祖先链读取，连同它们的批次终点：大小等于对该子分支的一次 `context` 读取，而现在 Prompt 和 ToolBinding 各自都会生成一份。
- **每个 Session 一份副本，只对恢复成立。** 恢复一次打开一个分支。运行时新建的分支（并发的 `branch` 调用、预先准备的 Session）各自独立打开，所以并发构建的子分支在打开期间各持一份自己祖先链的副本，而现在它们的每个读者都各持一份解码的祖先链。

## 6. 失败语义

| 情形 | 行为 |
| --- | --- |
| 填充的存储读取失败 | 负责填充的读者拿到错误。单元保持为空；随后每个等待的读者依次执行自己的读取，拿到自己的结果。存储持续失败时，`n` 个等待的读者依次产生 `n` 次失败的读取（§4.4）。读取失败的能力其 `apply` 失败，分支构建失败并照现在的方式回滚，回滚离开 `Loading`，结束打开期间 |
| `h` 之后的尾段读取失败 | 该读者拿到错误，同现在 |
| 负责填充的读者被取消 | 由下一个等待者填充；没有人拿到不完整的副本 |
| 打开期间存储提交失败 | 不变：`h` 之后的部分遵循现在的热窗口与存储规则；`1..=h` 在 Timeline 构建之前就已提交 |
| 读者持有副本时打开期间结束 | 该读者用已拿到的事件完成；之后的读者读存储 |
| 损坏的行 | `TimelineStore::branches` 在构建之前已校验每一行（读取 A）；填充遇到损坏时与任何读取一样失败 |

**每个读者得到同样的结果。** 由副本服务的读者拿到的是 `store.read` 对 `1..=h` 返回的事件（填充就是这样一次读取），先对填充返回的批次终点做存储的终点检查（§4.3），再按序号截到它的区间，然后接上现有代码对 `h` 之后返回的内容。`1..=h` 的事件及其批次终点已经提交，存储从不改变已提交的事件：它只追加事件，只用更晚的事件扩展输出行，升级也不改写业务行（[存储格式](../../../design/appserver/native-storage-format.md)）。所以在完好的存储上，由副本服务的读取返回的就是它自己读取时的结果：同样的事件，或者终点落在原子批次内部时同样的 `InvalidInput`。唯一的区别是对 I/O 失败的暴露：由片刻之前填充的副本服务的读者，不再重复那次 I/O，因此不会遇到它自己读取时可能遇到的瞬时存储错误。

## 7. 消除了什么，还剩什么

一个只有 `main` 的 Session 恢复一次时对 `main` 的读取，取自 §2.2 与 §8 的跟踪：

| 读取 | 现在 plain 与 tools | 现在 product | 有副本后 |
| --- | --- | --- | --- |
| A. `TimelineStore::branches`（`validate_session`） | 1 次全量 | 1 次全量 | 1 次全量；存储自己的校验，见下文 |
| B. TimelineTree 的恢复扫描 | 1 次全量 | 1 次全量 | 1 次全量；§17 起为切片 |
| ContextPreparation、InputQueue、Permission、ModelProvider、Approval、Loop、ToolExecutor 的 `local(..=S)` | 7 次全量 | 7 次全量 | 1 次全量，即填充；其他读者拿切片 |
| apply 中 Prompt 与 ToolBinding 的 `read_context` | 2 次全量 | 2 次，从检查点的 `resume` 起 | 切片 |
| `branch/resume` 时的打开处理器 | 1 次全量（ToolBinding） | 3 次，从 `resume` 起（ToolBinding、Prompt 两次） | 切片，各带一个单事件尾段 |
| Wait、BackgroundExec、Collaboration（产品装配，未测） | 3 次全量 | 3 次全量 | 切片 |

这里的一次检查点读取是两次存储调用：快照序号那一个锚点事件的读取，以及从 `resume` 起的上下文，10,000 条 product 事件时分别是 1 和 212 条事件。

子分支现在除上述本分支读取外，还要读五次祖先链：TimelineTree 在父分支上的截点校验（读取 C）、Permission、Prompt，以及 ToolBinding 两次。有副本后，它读一次本分支部分、两次祖先链（读取 C 与一次填充）。在 v17 上跟踪一个截于 `main` 第 3,002 条事件的子分支：它自己的事件被读了 11 次，`main` 的前缀被读了 5 次；原型下各两次（本分支：扫描和填充；前缀：读取 C 和填充），外加一个单事件尾段。

**把校验结果交给运行时（读取 A）不在范围内。** 存储在运行时构建任何东西之前逐页校验每个分支，用一个有界缓存，校验结束即释放（[timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)“恢复”行）。把它解码出的事件交给运行时，就要从校验一直持有每个分支的历史到最后一个分支打开，要么改变 `TimelineStore` 端口的返回值，要么让它在调用之间保留状态，并且与有界缓存的约定相违。让校验本身更便宜是存储的工作，不需要运行时改动（[恢复校验复用](../../../design/appserver/restore-validation.md)）；每个进程的首次校验仍会解码每条事件，之后目录未变的恢复会复用它。

**TimelineTree 的扫描（读取 B）在本设计中保留**：本设计不改能力代码，而这次扫描发生在分支的 Timeline 存在之前。§17 接手了它：创建失败在分支打开期间从副本读取，子分支的创建检查只读它最前面的事件。

## 8. 实测效果

release 构建，同一台 Apple M4，一次性存储放在隔离的 home 下，测量时没有其他构建在运行。同一个 `restore_cost` 测量程序的三个构建：`main` 位于 2946f25d（格式 v16，含 #213），#224 位于 29d3450b（v17），以及 #224 加本设计的原型（未提交）。测量程序在 #224 版本之上增加了计数分配器、恢复期间带范围的存储调用、可选的子分支，以及创建之前可选的暂停；v16 的 Session 用 `main` 构建生成，v17 的 Session 用 #224 构建生成，原型恢复的也是它们。每种配置两轮，顺序分别为 main、v17、原型与原型、v17、main；每轮每个规模恢复七次；表中是两个中位数的平均。10k 时两轮中位数相差不超过 3%。这些数据都不在测试中断言。这里测量的原型只保存事件；评审后加入的批次终点（§4.2）另在一个 10k 的 plain Session 上单独测量（§5），交错运行时恢复加首次 idle 不保存批次终点为 131 ms 和 134 ms，保存后为 140 ms 和 132 ms。

恢复加首次 idle，单位 ms：

| 配置 | 事件数 | `main` | #224 | #224 + 原型 | 原型 / #224 |
| --- | --- | --- | --- | --- | --- |
| plain | ~1k | 53.7 | 40.3 | 19.0 | 0.47 |
| plain | ~3k | 240.6 | 118.3 | 35.1 | 0.30 |
| plain | ~10k | 1,750 | 469 | 126 | 0.27 |
| tools | ~1k | 62.2 | 45.0 | 17.0 | 0.38 |
| tools | ~3k | 284.3 | 132.6 | 38.6 | 0.29 |
| tools | ~10k | 2,136 | 527 | 141 | 0.27 |
| product | ~1k | 65.8 | 62.0 | 20.5 | 0.33 |
| product | ~3k | 155.7 | 146.9 | 51.0 | 0.35 |
| product | ~10k | 524 | 496 | 170 | 0.34 |

每次恢复中校验之外的存储读取（同一配置各规模的调用次数相同）：读取调用次数，以及它们返回的事件数除以 Session 的事件数，即全量历史读取的次数：

| 配置 | `main` 与 #224 | #224 + 原型 |
| --- | --- | --- |
| plain、tools | 11 次调用，10.9 到 11.0 | 3 次调用，2.0 |
| product | 18 次调用，11.2（1k）、8.7（3k）、8.3（10k） | 8 次调用，2.0 |

原型的调用是 TimelineTree 的扫描、填充，以及 `h` 之后的单事件尾段（plain 与 tools 一个，product 六个：三个打开处理器各从检查点读两次）。每次恢复都有三个读者在填充进行中到达并等待它；10 次（plain、tools）和 17 次（product）读取由副本服务。

约 10k 的 plain 恢复把时间花在哪里，单位 ms（存储的时间段；读取会重叠，所以其墙钟时间只计一次）：

| | 校验 | 读取 | `branch/resume` 追加 | 存储之外 | 合计 |
| --- | --- | --- | --- | --- | --- |
| `main` | 147.8 | 1,577.8 | 4.2 | 20.7 | 1,750 |
| #224 | 39.1 | 403.0 | 4.3 | 22.6 | 469 |
| #224 + 原型 | 39.0 | 75.0 | 4.3 | 7.4 | 126 |

剩下的部分大约各占三分之一：校验（读取 A）、TimelineTree 的扫描（读取 B）和填充。每条事件的字节数不变（plain 10k 时 411 对 410），因为存储没有变。

子分支（一次 plain Session 的恢复跟踪，子分支截于 `main` 第 3,002 条事件；只计次数，见 §7）：`main` 的前缀在 #224 上被读 5 次，原型下 2 次；子分支自己的事件分别是 11 次和 2 次。

**创建 Session。** 同样的几轮中，`main` 与 #224 创建一个 Session 用 9 ms（plain、tools）和 39 ms（product），原型下是 21 到 24 ms 和 51 ms，尽管新的 `main` 不开始打开期间（`h` = 0，没有祖先链）。原因在存储的写入任务：当时两轮开始时间至少间隔 20 ms（[storage/timeline.rs](../../../../crates/dim-app-server/src/storage/timeline.rs)、[timeline-storage §3](../../../architecture/rust-agent/timeline-storage.md#3-写入与发布)）。原型的恢复在它自己的 `branch/resume` 那一轮之后 20 ms 内就结束了，所以创建的第一轮要等待。在创建之前暂停 40 ms，原型的创建是 9 ms（plain）和 36 ms（product）。同样的节奏也拉长了原型在小规模恢复中的 `branch/resume` 追加（plain 1k 时 8.5 ms 对 4.1 ms），所以 1k 的比值低估了收益。

**实现。** 在 #235 上按同样的方式测量：第 2 阶段之前的 `main`（0fb1d2bf，v16 加 #213）、v17（ae1bbe72）和 #235 的发布构建，都使用 #235 提交的 `restore_cost` 测量程序（两个基线不含其中的打开期间计数）；v16 的 Session 由 v16 构建生成，v17 的 Session 由 v17 构建生成，#235 也恢复这些 Session；每个规模交错两轮、每轮恢复七次，取两轮中位数的平均。恢复加首次 idle（ms）以及恢复期间的堆峰值（MB）：

| 配置 | 事件数 | v16 | v17 | #235 | #235 / v17 | 堆峰值，v17 → #235 |
| --- | --- | --- | --- | --- | --- | --- |
| plain | ~1k | 53.0 | 39.7 | 17.9 | 0.45 | 5.43 → 2.36 |
| plain | ~3k | 236.3 | 117.7 | 36.0 | 0.31 | 17.75 → 6.64 |
| plain | ~10k | 1,710 | 468 | 129 | 0.28 | 96.3 → 32.7 |
| tools | ~1k | | 44.4 | 16.7 | 0.38 | 5.90 → 2.61 |
| tools | ~3k | | 131.7 | 38.2 | 0.29 | 20.1 → 7.5 |
| tools | ~10k | | 523 | 141 | 0.27 | 112.5 → 38.1 |
| product | ~1k | | 61.2 | 19.5 | 0.32 | 6.49 → 3.00 |
| product | ~3k | | 144.8 | 50.1 | 0.35 | 19.7 → 7.3 |
| product | ~10k | | 492 | 169 | 0.34 | 69.4 → 23.7 |

每次恢复除校验外的存储读取：plain 与 tools 为 3 次调用、2.0 次全量历史读取，product 为 8 次调用、2.0 次（v17 为 11 次和 18 次调用）。首次 idle 时，每种配置和规模下的堆与 v17 相差不超过 0.003 MB，也没有存活的打开期间。两轮中位数相差不超过 11%（10k 时不超过 8%）。

**恢复之后创建 Session，予以接受。** 同样的几轮中，#235 创建一个 Session 用 21.1、21.9 和 45.6 ms（plain、tools、product），v17 是 10.1、8.9 和 35.3 ms；在创建之前暂停 40 ms，#235 是 8.8 和 32.6 ms（plain、product），v17 是 8.9 和 32.5 ms。差别来自上面所说的写入任务的轮次节奏，而不是创建本身的工作，只在创建紧跟在恢复最后一轮写入之后 20 ms 内开始时出现。这一节奏是存储的约定，不在本次改动范围内，所以接受这一差别，而不去绕开它。[#238](https://github.com/arcships/dim/issues/238) 随后修改了这一约定（[#243](https://github.com/arcships/dim/pull/243)）：除非待写批次全部是流式输出，上一轮提交后下一轮即开始（[timeline-storage §3](../../../architecture/rust-agent/timeline-storage.md#3-写入与发布)）。在那次修改上测量，恢复之后创建 Session 用 7.8 和 12.0 ms（plain、product），修改前是 24.6 和 51.0 ms。

## 9. 影响面

按[原则 §3](../../../architecture/rust-agent/principles.md#3-正交)：目标是运行时 Timeline 之外零改动、零新增公开内容。

| 模块或内容 | 改动 | 理由 |
| --- | --- | --- |
| `dim-agent-runtime` `core/timeline.rs` | 在已提交历史读取路径中加入打开期间的副本：两个接缝（本分支部分、祖先链）和开始打开期间的函数（crate 私有） | 读取成本是 Timeline 自己的 |
| `dim-agent-runtime` `assembly.rs`、`config.rs` | `BranchScopePlugin` 开始打开期间；`BranchLifecycle` 持有它，分支离开 `Loading` 时丢弃 | 分支生命周期已划定 `Loading`；`TimelineContext` 不变 |
| `testing` 特性 | `Agent::live_openings`：该 Agent 存活的分支打开期间数，无论是否已有副本，供 §14 的释放检查使用，类似 #224 的节点加载计数；发布构建不计数 | 构建失败或取消后没有公开调用能触及该 Timeline，否则无法观察它是否释放；读取次数不需要钩子（用计数存储即可） |
| `dim-app-server` `examples/restore_cost.rs` | 堆峰值与存活字节、带范围的存储调用、可选的子分支和创建前的可选暂停、首次 idle 时存活的打开期间 | §8 以及 §14 第 8、9 条的测量；不做断言 |
| Dispatcher、热窗口、`TimelineStore`、存储、事件类型、样本、能力、插件、协议 | 无 | §3 |
| 新增公开内容 | 无 | 不新增事件、服务、方法、wire 字段或配置；`Agent::live_openings` 只在 `testing` 特性下存在 |

## 10. 规则检查

| 规则 | 回答 |
| --- | --- |
| [原则 §4](../../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 第 1 条（改别人的状态写指令事件） | 不适用：不改其他模块的状态 |
| §4 第 2 条（交接） | 不适用：没有交接；读者照旧自己读 |
| §4 第 3 条（before 回调） | 不变；副本保存的是已提交事件，before 回调不会再看到它们 |
| §4 第 4 条（核心不带业务含义） | 副本只按分支和序号区间组织；不读事件种类，不为任何能力设特例 |
| §4 第 5 条（原子批次） | 不变：`h` 是批次边界（已发布序号），且切片只在对副本保存的批次终点做过存储的终点检查后才提供，所以读取返回同样的事件和同样的错误（§4.3） |
| §4 第 6 条（模型上下文） | 不变：同样的事件，所以同样的上下文 |
| §4 第 7 条（公开事件与服务） | 能力继续经 Timeline 服务读取；不读别人的状态，也不接收别人的结果 |
| §4 第 8 条（基础设施） | 对 Timeline 的改动，需求来自 Timeline 自身的读取成本，并在运行时内闭环：§11 |
| §4 第 9 条（只有决定所需的事实进 Timeline） | 没有新事实；副本是进程内存 |
| §1（Capability 与 App Server 边界） | 在运行时内部；不改能力和 App Server |
| §3（正交） | 不改其他模块；影响面见 §9 |
| [ADR 002](../../adr/002-plugin-scope.md) D1（作用域跟随状态） | 分支作用域：副本属于一个分支的 Timeline，随其打开期间存在 |
| ADR 002 D2（不设接线模块） | 不新增模块或插件 |
| ADR 002 D3（依赖跟随作用域） | 不新增依赖 |
| ADR 002 D4（配置） | 无 |
| ADR 002 D5（合同语言中立） | 合同不变；经 `Dispatcher::local` 读取的 TypeScript 部件也同样由副本服务 |

## 11. 向机制所有者提出的需求

不向 rutis 提出需求。改动本身就在运行时，所以这里为运行时机制所有者回答第 8 条的各项：

| 项目 | 内容 |
| --- | --- |
| 被什么挡住 | 不是插件的问题。需求来自 Timeline：打开期间，同一段不可变前缀的每个读者都要付一次存储解码，而重建合同（[单树装配](../../../architecture/rust-agent/single-tree.md#能力重载协议)）让每个重建的能力都成为这样的读者 |
| 考虑过的现有机制 | 热窗口只保存未分发的事件，恢复时为空。存储的缓存按约定以单次读取调用为界，不能跨调用。能力侧的改动（快照、按种类过滤的读取、推送事件）在 §13 中否决 |
| 通用能力 | 分支打开期间，Timeline 用一份解码副本服务对其构建时已有历史的读取。不带业务含义：只涉及分支、序号区间和生命周期阶段 |
| 对序号、批次、中断、重载的影响 | 序号与批次：无；读取返回同样的事件和错误，终点落在原子批次内部时按副本的批次终点拒绝，与存储的拒绝相同（§4.3）。中断：无；打开期间的中断是 `h` 之后的事件。重载：分支仍在打开期间时重载的能力读同一份副本；之后重载的照旧读存储 |
| 验证 | 在运行时及其流程测试内（§14） |
| 负责人与 issue | 运行时，核心机制负责线（eric8810，[路线图 §3.5](../../../design/roadmap.md#35-按人按批次)）；[#193](https://github.com/arcships/dim/issues/193) |

## 12. 决定

| # | 决定 | 否决的方案 | 理由 |
| --- | --- | --- | --- |
| D1 | 副本随分支的打开期间（`Loading`）存在 | 以 Session 恢复为范围，在 Session 发布时释放 | 分支逐个打开，分支范围只持有一个分支的历史；Session 范围要把每个分支的历史一直持有到最后一个分支打开。分支生命周期已在运行时中标出打开期间，运行时也不需要知道分支是恢复、新建还是预先准备的 |
| D2 | 接缝是 Timeline 的已提交历史读取路径 | 为 Session 恢复包一层转发的 `TimelineStore`；在 `Dispatcher::local`、`Branch`、`Session` 上加包装 | 存储包装还能服务 TimelineTree 的扫描（读取 B），但它要在 Session 的整个生命期拦截存储端口，在调用之间保留状态，并改变 TimelineTree 直接读存储的含义。门面包装只覆盖部分读者（`read_context` 直接经过 Timeline） |
| D3 | 副本保存每条事件及其批次终点，不保存 before 轨迹 | 只保存事件；保存带轨迹的存储 `StoredEvent` | 终点落在原子批次内部的读取必须像从存储读取时一样失败，而存储检查的是终点的 `batch_end`；恢复后的热窗口里没有 `1..=h` 的批次终点，只保存事件就会返回半个批次。所服务的路径只返回事件，所以轨迹只会增加内存和深拷贝，没有读者需要 |
| D4 | 只有需要整段的读取才填充 | 任何读取都填充 | 检查点读取和尾段读取不得解码它之前的前缀（timeline-storage §4） |
| D5 | 副本固定在 `h`；`h` 之后照现在的方式读取 | 读者读到尾段时追加进副本 | 尾段只是打开期间写入的几条事件；扩展副本会多一条填充规则和可变的共享状态，没有可测的收益 |
| D6 | 填充失败不保留；等待者各自读取 | 把填充的错误分享给等待者 | 每个读者的结果仍是它自己那次读取的结果（timeline-storage §6），瞬时失败也不会让本可成功的读者失败。代价在存储持续失败时：等待者一个接一个重试，`n` 个等待者产生 `n` 次失败的读取（§4.4） |
| D7 | 子分支的祖先链与本分支事件一样共用 | 只共用本分支事件 | 子分支打开时（新建与恢复都一样）要读约四次祖先链（Permission、Prompt、ToolBinding 两次）；同一条规则即可覆盖，不需要额外机制 |

## 13. 否决的方案

- **向能力重放历史**（由 Dispatcher 或某个恢复步骤把事件交给每个订阅者折叠）。能力不再以自己的 `S` 读取；重建合同和 Dispatcher 只做实时分发的职责都要变。
- **用能力快照重建。** 为优化而存业务状态，而且每个插件都要做（[timeline-storage §4](../../../architecture/rust-agent/timeline-storage.md#有界读取与恢复扫描)），还在事件之外多出第二个事实来源（[时间总线 §3](../../../architecture/rust-agent/time-bus.md#3-总线只承载影响后续行为的结果)）。
- **在能力之间共享重建后的状态**（比如做一次折叠供几个能力读）。状态只有一个所有者，它只依据自己消费到的事件决定（[时间总线 §2](../../../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实)）。
- **存储范围的解码事件缓存。** 驻留会随每个最近装载或读取过的 Session 增长，违背空闲分支的验收；第 2 阶段出于同样的理由否决了它（[请求增量存储 §17](request-delta-storage.md#17-否决的方案)）。
- **校验把结果交给运行时。** 见 §7。
- **按种类过滤的读取**（每个能力只要它折叠的种类）。要改读取 API 和每个能力的重建，而存储仍要解码每一行才能过滤。
- **把副本保留到 Session 发布，或之后再保留一段时间。** 多占驻留却没有读者受益：激活之后读取很少，照现在的方式服务即可。

## 14. 验收

实现 PR 须全部通过。行为检查是确定性的，放在现有流程测试中（`tests/integration`、运行时与能力的流程测试）；耗时与大小目标用 release 构建的 `restore_cost` 测量，在实现 PR 中报告，不在测试中断言。

**行为（CI）：**

1. **每个打开期间解码一次。** 通过计数存储恢复一个 `defaults::install` 的 Session，无论有多少个重建的能力，每个分支自己的历史 `1..=h` 在恢复期间至多从存储读取一次（即填充；§17 起 TimelineTree 从副本读取恢复事实，子分支的创建检查从开头分页读取，止于它的第一个排队输入）；`h` 之后的读取只覆盖打开期间写入的事件。有子分支时，它的祖先链在其打开期间只读一次（另加 TimelineTree 在父分支上的截点校验）。
2. **每个读者看到同样的事件。** 现有的恢复、重载和分支流程测试不改即通过，包括有未完成工作的恢复（打开期间写入结算终态，读者固定的 `S` 不同）、压缩检查点（从 `resume` 读取）和子分支。
3. **原子批次终点。** 在现有的恢复流程中（`crates/dim-agent-capabilities/tests/branch_flow.rs` 的恢复用例，其分支探针在分支打开期间就能拿到 Timeline），序号 1 和 2 是一个已持久化的批次且 `h >= 2`：重建读取 `local(..=S)` 填充副本之后，`local(..=1)`、`history(Some(1))` 和 `context(1)` 以 `InvalidInput` 失败，而 `local(..=2)`（整个批次）返回事件 1 和 2，`local(2..=2)`（起点在批次内部、终点在批次边界）返回事件 2。每个结果都等于激活之后（读存储时）同样读取的结果。
4. **释放。** `restore_session` 返回后没有持有副本的打开期间（`Agent::live_openings`，`testing`），之后的 `local(..)` 读存储。恢复失败（某个分支的重建读取失败、注入的读取失败）以及恢复被 `Agent::close` 或 Session 关闭取消之后，同样没有持有副本的打开期间。
5. **失败。** 注入的填充读取失败使恢复以与现在相同的存储错误失败；下一次恢复成功。
6. **打开期间之外的读者。** 在活跃分支中重载的能力读存储。
7. **冻结合同。** `crates/dim-agent-types/tests/fixtures/` 与 `*_contract.rs` 测试不变。

**测量（在实现 PR 中报告）：**

8. `restore_cost` 的 plain、tools、product 在约 1k、3k、10k 事件时的恢复加首次 idle，与同一台机器上的 v17 对比：任何规模都不慢于 v17；10k 时 plain 与 tools 至多为 v17 的 35%，product 至多 40%（原型测得 27%、27%、34%）。
9. 每种配置和规模下，恢复的堆峰值不超过 v17（原型在 10k 时约为三分之一），首次 idle 时的堆与 v17 相差不超过 0.01 MB。

## 15. 对其他文档的改动

随本设计：[文档索引](../../../INDEX.md)链接本页。

随实现，各自保持原意：[timeline-storage](../../../architecture/rust-agent/timeline-storage.md) §4“使用方”说明：分支打开期间，Timeline 用一份共用副本服务对其构建时已有历史的读取，分支离开 `Loading`（发布、关闭或构建回滚）时释放，副本保存每条事件的批次终点，使 §4 的区间终点规则对它的读取同样成立；§5“恢复”行说明能力的 `context` 或 `local` 读取共用这份副本；[单树装配](../../../architecture/rust-agent/single-tree.md#能力重载协议)的重建一行保持原样。[请求增量存储](request-delta-storage.md)的 §15 与 §20 链接到本页。

## 16. 评审中的问题

1. **子分支祖先链（D7）。** 保留并已实现：子分支的打开期间像共用自己的事件一样共用它的祖先链，这也加快了运行时的分支创建。
2. **TimelineTree 的恢复扫描（读取 B）。** 它读取每个分支的全部历史，只为找到 `branch/start`、第一个排队输入和 `branch/create-failed`。留给 TimelineTree 的所有者作为后续工作；这次改动不动它。已在 [#239](https://github.com/arcships/dim/issues/239) 中处理，见 §17。

## 17. TimelineTree 的恢复事实

[#239](https://github.com/arcships/dim/issues/239) 是 §16 第 2 项的后续工作。所有者：TimelineTree（[timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs)），核心机制负责线。

**问题。** 有了 §4，恢复仍要从存储读取每个分支自己的历史 `1..=h` 两次：TimelineTree 在构建分支之前的扫描（读取 B），以及打开期间的填充。这次扫描找两项事实。一是该分支记录的创建失败（`branch/create-failed`），它们使对应的子分支不再启动；二是仅对子分支而言，它的创建是否完成：`branch/start` 之后有一个排队输入。3k 到 10k 条事件时，这次扫描占恢复耗时的 27% 到 30%（#239）。

**机制。** 只在 TimelineTree 内部：

- **创建失败。** 恢复不再在构建之前读取它们。被恢复分支的构建任务在其能力 apply 之后、激活之前，经该分支的 Timeline 读取 `local(..=h)`；此时打开期间仍在，所以这次读取是副本的切片（§4.3）。它把创建失败交给恢复循环，恢复循环照旧据此检查该分支的子分支。子分支只在父分支发布之后才恢复，所以用到这些事实时它们已经就绪。
- **子分支的创建。** 检查用 `TimelineStore::read_page` 从开头每页读 32 条事件，直到找到 `branch/start` 之后的第一个排队输入；创建完成的子分支在打开期间就会写下它们。创建完成的子分支只读一页；被打断的创建照旧读到头部。检查仍在构建之前进行，所以被打断的子分支不会被构建。

```text
TimelineTree::restore，分支 b，存储头部 h
  仅子分支：read_page(b) 从开头读到 branch/start + 排队输入               ... 一页
  仅子分支：读取 C，在父分支上校验截点（不变）
  启动 b，以 Resume 打开于 h
    build：能力 apply；第一个重建读取填充副本                         ─┐
    TimelineTree：local(..=h)，切片；收集 branch/create-failed               │ b 的打开期间
    追加 branch/resume …… 激活                                              ─┘
  恢复循环：记下 b 的创建失败，再恢复 b 的子分支
```

**保持的约束。** 每个决定都基于相同的事实：创建失败来自相同的事件 `1..=h`，子分支的创建来自截至决定它的那条事件的相同事件。TimelineTree 的这次读取只在填充失败时失败，此时构建失败，像打开期间任何读取失败一样回滚（§6）。创建检查的各页是普通的存储读取，遵守 timeline-storage §4 的区间规则。`Timeline`、`TimelineStore`、事件类型、样本以及除 TimelineTree 以外的所有能力都不变；不新增事件、服务、方法、配置或依赖。

**规则检查。** 同 §10，只有一处不同：改动在拥有恢复的能力 TimelineTree 中，而不在运行时基础设施中（不涉及原则 §4 第 8 条）。TimelineTree 经公开读取 API 读取它自己构建的 Timeline，与它为校验截点读取父分支历史一样（第 7 条）。

**验收。** 行为（CI）：按修订后的 §14 第 1 项，在上下文流程中带子分支的恢复（`manual_compaction.rs`）里检查：`main` 自己的历史从开头只读一次，子分支的也只读一次，子分支的创建检查只读一页，且这一页少于它的历史。现有恢复流程，包括被打断的创建和失败的目录（`branch_flow.rs`），原样通过。本地测量并在实现 PR 中报告：同一台机器上相对 `main` 的恢复加首次 idle，以及堆峰值不变。

**实测效果。** 一台 Apple M4 上 `restore_cost` 的 release 构建：806099d3 的 `main` 与本改动，使用同一批一次性存储，四轮交错、每个大小每轮恢复 15 次；表中是各轮中位数的平均，各轮中位数相差不超过 5%。恢复加首次 idle，ms：

| 组合 | 事件数 | `main` | 本改动 | 比值 |
| --- | --- | --- | --- | --- |
| plain | 1,739 | 18.8 | 18.5 | 0.98 |
| plain | 3,739 | 40.2 | 29.0 | 0.72 |
| plain | 10,739 | 132.6 | 90.0 | 0.68 |
| product | 1,385 | 23.0 | 19.3 | 0.84 |
| product | 3,383 | 54.1 | 38.9 | 0.72 |
| product | 10,393 | 174.3 | 120.0 | 0.69 |
| plain，子分支截于 `main` 头部 | 3,018 | 62.1 | 52.7 | 0.85 |
| plain，子分支截于 `main` 头部 | 10,018 | 221.3 | 181.3 | 0.82 |

除校验外每次恢复的存储读取：plain 从 3 次调用、2.0 次全量历史读取降到 2 次、1.0；product 从 8 次、2.0 降到 7 次、1.0；有子分支时从 8 次、4.0 降到 7 次、3.0（子分支的祖先链仍由读取 C 和它的填充读取）。堆峰值与首次 idle 时的堆在 0.01 MB 内不变，首次 idle 时没有存活的打开期间。plain 1k 时省下的时间被存储写入轮的间隔吃掉：`branch/resume` 的追加等待 7.9 ms，而不是 4.0 ms（[#238](https://github.com/arcships/dim/issues/238)）。
