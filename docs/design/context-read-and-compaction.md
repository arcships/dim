# 上下文读取与压缩定位

Status: Draft。交付状态在 issue，不在本文。

Scope: branch。
Issue: [#708](https://github.com/arcships/dim/issues/708)。

Rules: [design rules](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)、[ADR 006](../adr/006-plugin-contract.md)。

相关文档：[上下文重建的内存计划](context-replay-memory.md)（已落地的 P0–P3）、[上下文拼装与识别](context-assembly-requirements.md)（业务与技术边界）、[压缩需求](compaction-quality.md)（压缩保留什么、质量如何判断）、[原生存储格式](appserver/native-storage-format.md)（格式、升级、快照的持久合同）。

## 1. 范围与非目标

范围：一次模型步骤从 Timeline 读到 `ModelRequest` 的读取计划——起点、读取集合、批次推进、定位查询、回退；以及压缩在这条路径里的定位语义、结算状态机与失败后果。

非目标：

- 压缩策略保留什么、摘要怎么写、质量如何判断（[压缩需求](compaction-quality.md)）。
- 提示词内容（[model-prompts](model-prompts.md)）。
- 事件种类与事件正文格式、`ModelRequest` 序列化。
- **准备步骤之间保留投影**（[#710](https://github.com/arcships/dim/issues/710) 的方向）。它要求修改[上下文拼装与识别](context-assembly-requirements.md) §3.2“不引入需要失效规则的投影缓存、不在准备步骤之间保留派生位置”，那是该文档所有者的决定；本文不预设，也不依赖它。
- 界面、协议、任务执行。

## 2. 现状与问题

### 2.1 每次准备都从序号 1 重放

[history.rs](../../crates/dim-agent-capabilities/src/history.rs) 的流式入口以 `start = 1`、`state = None` 开始，只有读到压缩事件时才裁剪已累计的历史，读取范围覆盖整条分支。压缩过的会话因此每轮都读“裁剪点之前 + 之后”。

### 2.2 定位查询读整条分支

[storage/timeline.rs](../../crates/dim-app-server/src/storage/timeline.rs) 的 `policy_order` 每轮执行：

```sql
SELECT event_id, kind, sequence, json_extract(body, '$.record.event.data.through')
FROM timeline_events
WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND event_id IS NOT NULL
ORDER BY sequence
```

每轮把整条分支的行读一遍，并对每行做一次 JSON 提取，只为得到轮次起点、压缩起点和“某个裁剪点 id 是否在分支内”。

### 2.3 两个读取入口，起点语义不一致

运行时已有从快照恢复位置起读的入口（[timeline.rs](../../crates/dim-agent-runtime/src/core/timeline.rs) `context_with_checkpoint`），完整路径在用：命中快照时它只物化恢复位置之后的后缀，而不是整条分支。流式入口绕开它，固定从 1 开始。同一个问题因此有两套语义：一套能起读但物化后缀，另一套不物化但不起读。

### 2.4 阈值压缩未完成时，失败发生在模型调用处且丢掉原因

自动压缩在准备路径里内联执行（[context_policy.rs](../../crates/dim-agent-capabilities/src/context_policy.rs)）。结算后：

- 完成：用压缩后的请求；恢复模式下若仍超输入预算，报 `recovery_overflow`；
- 取消：整个准备报取消；
- 失败/跳过且不是恢复模式：准备继续，用未压缩的请求。

未压缩请求随即在 `ModelProvider::run` 的校验处被拒绝（[model_provider.rs](../../crates/dim-agent-capabilities/src/model_provider.rs) → `ContextPolicy::validate`），错误是“Model input exhausts its context window”，对照的是输入预算 `窗口 − 输出预留`（[context_policy.rs](../../crates/dim-agent-capabilities/src/context_policy.rs) `input_budget`）。请求不会发给 provider，但**失败信息里没有压缩的结算结果与原因**：读者只能看到“上下文超了”，不知道本步压缩失败/跳过/为什么。

### 2.5 证据

[#708](https://github.com/arcships/dim/issues/708) 的实测：最大会话 4,903 事件、436 次准备、累计重放 1,086,523 个事件、51 万 token、`context/compaction` 与 `timeline_snapshots` 均为 0；进程 1.62 GiB 常驻，`malloc_trim` 归还 1.2 GiB。

## 3. 目标

- **读取量**：一次准备读取的行只与“恢复位置之后仍在读取集合内的事件 + 当前批”有关；没有快照时仍是一次全量，但按批读取、只读读取集合。这改变了[上下文拼装与识别](context-assembly-requirements.md) §3.2.3 的“每步仍然读取全量事件”，按 §9 提给该文档所有者。
- **峰值**：只与批大小和投影有关，不随历史长度线性增长。
- **一致性**：起读结果必须与全量重放逐条一致；不满足即回退，不猜。
- **定位**：轮次边界、压缩尝试与完成、输入窗口、用量来自列或窄表的定点查询，不来自正文的逐行提取；查询的读行数有上界，并有可验证的度量方式。
- **失败语义**：压缩的每个触发方式在每种中断点都有确定终态与下一次行为；压缩未完成时的失败必须带上结算结果与原因，并以输入预算为界。

## 4. 设计

### 4.1 读取计划

一次准备有四个位置，不能混用：

| 名称 | 含义 | 关系 |
| --- | --- | --- |
| 锚点 | 写快照的那条压缩事实 | 事实序号大于它记录的 `through` |
| 恢复位置 | 重放开始的第一条事件 | 同一段内**早于**锚点；或在更早的祖先段 |
| 段边界 | 每一段（祖先或当前分支）固定到的序号 | 当前段是 through，祖先段是建分支时的切点 |
| 起点 | 实际开始读的位置 | 有快照时等于恢复位置，否则是第一段的序号 1 |

恢复位置早于锚点，是因为快照状态折叠的是**恢复位置之前**的事件；恢复位置与锚点之间的事件是保留尾部，必须照常读取，由它们重放并安装压缩事实的替代内容与基线。把锚点当起点会跳过这段保留尾部；让恢复位置晚于锚点则会跳掉提供替代内容的事实。

顺序：

1. **选快照**：按祖先段到当前段的顺序**倒着**试。每一段取解释器一致、序号不超过该段边界的最新快照；校验锚点是对应压缩事实、事实的 `through` 早于快照序号、`retain_from` 与状态一致、恢复位置在固定祖先链内，且同段时 `恢复位置 < 锚点`、祖先段时不超过该段边界。第一条通过的就用。该段最新候选损坏时继续试更早的祖先段；全部失败才回退。
2. **定起点**：命中快照时以恢复位置为起点；否则从第一段的序号 1 开始。
3. **分段批读**：**恢复位置所在段之前的段完全不读**（它们的折叠已经在快照状态里）。恢复位置所在段从其序号读到段边界，后续段从头读到各自的段边界，当前段读到 through。每批默认 256 条，读一批投影一批，事件只活到当前批。
4. **回退**：没有有效快照、读取或解码失败、投影校验失败，一律从头按批重放一次，并记录回退原因。

快照缺失时的“从压缩事实推导跨段状态”不作为本设计的实现路径：它需要把跨段状态从投影器内部状态提升成显式契约，本文只把它列为后续项。

### 4.2 读取集合

过滤的前提不是“事件看起来重不重要”，而是**投影实际消费哪些事件**。读取集合由投影自己的实现导出，不手工分类：

- 历史投影（[history.rs](../../crates/dim-agent-capabilities/src/history.rs) 的重放、裁剪与消息组装）：`context/configured`、`input/message`、`input/consumed`、`context/compaction`、`branch/start`、`step/model-start`、`step/thinking`、`step/text`、`step/tool-use-start`、`step/tool-use`、`step/model-content`、`step/model-output-end`、`step/tool-result`；
- 提示词折叠（[prompt_state.rs](../../crates/dim-agent-capabilities/src/prompt_state.rs)）：`prompt/changed`、`turn/start`、`turn/end`、`branch/start`、`context/scope`。

其余事件不改变默认投影的模型上下文，读取时整行跳过；自定义渲染器不在过滤范围内，它照旧走完整路径。三条规则：

1. **集合从实现导出**：常数表与重放/折叠里的 `match` 分支必须一致，穷举测试要求每个 `EventKind` 要么在读取集合内，要么被显式列为忽略；新增变体未列入即失败。
2. **定位不等于跳过读取**：参与投影的种类（如 `turn/start`、`context/scope`）仍必须按序号重放；定位查询只返回位置与游标，不能替代读取。
3. **条目带来源序号**：投影条目记录其来源事件的序号，供策略按序号比较位置；否则 `regrown`、`legal_cuts` 需要“事件 id 到位置”的全量映射，等于又扫一遍分支。

一致性由 §6 的差分测试兜底：同一前缀上，过滤读取与不过滤读取的 entries 与状态必须逐条一致；覆盖成功、失败、中断的输出、工具调用与作用域提示词。

### 4.3 定位查询与标签

`timeline_events` 已有列：`session_id`、`branch_id`、`sequence`、`last_sequence`、`kind`、`body`、`event_id`；主键 `(session_id, branch_id, sequence)`，`event_id` 与工具调用各有一个部分索引，没有 `kind` 的通用索引。

需要的查询固定为五个，各自对应标签：

| 查询 | 回答什么 | 标签与索引 |
| --- | --- | --- |
| 最近一次压缩尝试 | `regrown` 的边界（任何触发、任何结算），以及它的 `through` | `kind='context/compaction-start'`；索引 `(session, branch, kind, sequence)` 倒序取一 |
| 最近一次完成压缩 | 起读的锚点与裁剪点 | 窄表 `status='completed'`，索引 `(session, branch, status, sequence)`；无窄表时 `kind='context/compaction'` 且只扫压缩行 |
| 段边界之后的轮次边界 | 合法裁剪点枚举与预算分段 | `kind='turn/start'` 的 `turn_id`；索引 `(session, branch, kind, sequence)` |
| 输入窗口 | 当前轮输入与它引用的、写在轮次之前的已消费输入 | `input/consumed` 的 `input_id`、`input/message` 的 id；输入行的部分索引 |
| 用量基线 | 估算用的 `model/call` 与其 `model/usage` 配对 | 按 `request_id`/`turn_id` 的部分索引 |

位置一律用序号表达：`regrown` 的边界是“段边界之后的最后一条事件”，在当前段就是 through；`legal_cuts` 用条目来源序号与轮次开始序号比较。两者都不再需要 `policy_order` 的 id 列表。

压缩窄表 `timeline_compactions`（由存储文档定格式与版本）：

- 一行一次尝试：`{session_id, branch_id, start_sequence, fact_sequence, command, through, trigger, status, retain_from}`；
- 手动终止事实没有开始事件（[agent_loop.rs](../../crates/dim-agent-capabilities/src/agent_loop.rs) 的 `start: None`），这一行 `start_sequence` 为空、`command` 有值；
- 与压缩开始/事实在同一批次内维护，可从事件重建；
- 升级时按 `kind` 回填，只读压缩行，行数与压缩次数同级；回填前或窄表缺失时退回 `kind` 索引 + 压缩行单行提取，不回退到全分支扫描；
- 过滤后的批次推进按物理行的 `last_sequence` 前进，不按 `sequence + 1`。

不索引：消息正文、工具结果正文、模型输出文本。

### 4.4 快照与压缩事实

- 压缩事实是真相：`retain_from`、替代内容、基线、诊断都在事件里，可以完整重建上下文。
- 快照是可选缓存：在事实提交之后写入，锚定该事实，内容折叠**恢复位置之前**的事件；写入失败只告警。
- 快照只在 §4.1 的校验全部成立时被采用；按段倒序尝试，全部失败才回退全量。
- 两次压缩之间取最新快照；更早的快照属于更早的锚点，不参与本次起读。

### 4.5 压缩状态机

状态：**判断 → 开始 → 结算 → 快照（可选）**。

不变量：

1. 每个“开始”最终都有一条“结算”。进程内由本步骤保证，**但结算写入失败时例外**：写入错误向上返回，开始保持未结算，直到下一次重载由恢复逻辑补 `Cancelled（capability reloading）`。
2. 结算单调：一条压缩只有一条结算；谁先提交谁赢。
3. 快照只出现在结算为“完成”之后，且锚定该事件；快照的构造与保存在结算之后等待，不改变结算。
4. 结算提交前，压缩不改变任何读取结果。

三类触发的后果：

| 触发 | 完成 | 取消 | 失败/跳过 |
| --- | --- | --- | --- |
| 手动 | 用压缩后的上下文，用户可见 | 记录原因，用户可见 | 记录原因，用户可见，可重试 |
| 阈值 | 用压缩后的请求；仍超输入预算 → 以结算为原因结束本轮准备 | 结束本轮准备 | 见下 |
| 恢复（provider 已拒绝过） | 仍超预算 → `recovery_overflow` | 结束本轮 | 硬失败 |

**阈值未完成或完成后仍超预算时**：以输入预算 `B = 窗口 − 输出预留` 为界。

- 未压缩请求估算超过 `B`：以明确错误结束本轮，错误里带这条结算的事件序号与原因；不再让通用校验去报“上下文超了”。
- 未压缩请求仍在 `B` 内：允许继续；结算事实（失败/跳过及其诊断）就是记录，不新增事件字段。
- 已有的 `regrown` 旁路保持不变：没有再增长时本就判断为不需要压缩。

**超时**：只包住策略调用，不包结算写入与快照保存。超时先通过取消令牌让策略停止并等待它自己持有的工作结束（网络请求、子任务），再提交 `Cancelled` 并说明超时；策略在取消生效前已经返回的结果优先采用。结算写入与快照保存都在 `commit` 内、结算之后等待，取消检查在 `compact_step` 返回之后——这条顺序保持不变：**结算已提交后到达的取消不改变结算**，下一次准备按它继续。

**退避**：连续失败计数由 branch 内存维护，可从事实重建但重载后清零（重载后允许再试一次）；成功清零；手动压缩与恢复触发不受暂停限制，暂停只作用于自动阈值触发；暂停后只有成功、手动触发、恢复触发或重载能解除，这一点在结果里说明。达到上限后的提示属于[压缩需求](compaction-quality.md) §4 的范围，本文只要求状态可被读取。

**结算归属**：自动压缩的未结算开始由 `ContextPreparation::rebuild` 结算，手动命令的由 `AgentLoop` 的重建路径结算（[context_preparation.rs](../../crates/dim-agent-capabilities/src/context_preparation.rs)、[agent_loop.rs](../../crates/dim-agent-capabilities/src/agent_loop.rs)）；两者按开始事件序号配对，不重复结算。

**其余中断点**：压缩开始之前就失败的准备（配置或请求无法构造）不写压缩开始，也不写压缩结算，只由准备自己的失败终态结束；策略 panic 视为失败并写结算；读到一半取消丢弃已读批次，下一次按事实重建。

### 4.6 存储格式决定

新增索引与压缩窄表是存储格式变更。版本号由 [原生存储格式](appserver/native-storage-format.md) 决定，取落地当时的下一个可用版本，并给出原地升级步骤：升级事务内按 `kind` 回填窄表；窄表只存定位字段，可从事件重建；旧版本守护进程按既有门禁拒绝新库并保留字节。

### 4.7 消息与存储粒度

- 消息正文共享（`Arc<Message>`）保持。
- 文本行按 4 KiB 或 1 秒刷（[output.rs](../../crates/dim-agent-capabilities/src/output.rs)），思考按块一行，保持现状。
- 若 §6 测量显示行数成为瓶颈，另开设计处理块化。

## 5. 兼容性与回退

- 事件种类、正文与 `ModelRequest` 序列化不变；冻结的事件 fixture 不动。
- 快照格式不变；新增索引与窄表按 §4.6 处理，窄表是派生数据。
- 回退：没有有效快照、锚点不符、解释器不符、快照损坏、读取/解码失败、投影校验失败，一律从头按批重放，并记录回退原因。**回退时读取量是全量**，§6 的“只读后缀”只对命中快照的路径成立。
- 未回填或窄表缺失的旧会话：定位查询退回 `kind` 索引路径，不退回全分支扫描。

## 6. 验收与测量

| 项 | 方式 |
| --- | --- |
| 起读与全量一致 | 固定前缀上，恢复位置起读与全量重放的 entries 与状态逐条一致 |
| 起读的边界情况 | 连续两次压缩、最新快照缺失、最新快照损坏、裁剪点跨批、恢复位置在祖先段、重放中途取消 |
| 过滤一致 | 过滤读取与不过滤读取逐条一致；覆盖成功/失败/中断的输出、工具调用与作用域提示词；穷举测试覆盖全部事件种类 |
| 回退矩阵 | 无快照、锚点不符、解释器不符、损坏、祖先段全部失败、窄表缺失各一条流程；回退路径读取全量 |
| 压缩失败矩阵 | 阈值失败且超 `B` → 带结算的错误；阈值失败且 `B` 内 → 继续；阈值完成仍超 `B` → 带结算的错误；恢复失败/跳过 → 硬失败；取消 → 取消；结算写入失败 → 下次重载补结算；超时与返回竞争 → 单次结算；策略 panic → 失败结算；取消在结算之后 → 结算保留；崩溃在开始后、结算后、快照前 → 恢复逻辑与回退 |
| 退避 | 连续失败达上限后自动压缩暂停；成功、手动、恢复、重载各自解除；恢复触发不被暂停挡住 |
| 手动命令 | 开始前失败（无开始）、已有命令进行中、重载时已有命令未结算，各一条流程 |
| 读取量 | 命中快照的会话：读行数只与恢复位置之后有关；`context-breakdown` 对照 |
| 定位查询 | 用查询计划或访问行数证明五个查询不读正文行，且不随前缀长度线性增长 |
| 峰值 | 与批大小相关，不随历史增长；`restore-replay` 与 `context-replay` |

不设 CI 字节阈值；数值写进实现 PR。

## 7. 风险与备选

| 项 | 说明 |
| --- | --- |
| 读取集合漏项 | 上下文会不同或投影直接失败；集合从实现导出 + 穷举测试 + 差分测试 |
| 快照语义漂移 | §4.1 的校验拒绝不匹配的快照；拒绝即回退 |
| 窄表与事实重复 | 只存定位字段，可从事件重建；升级回填只读压缩行 |
| 过滤与合并行 | 批次推进按 `last_sequence`；差分测试覆盖 |
| 备选：准备之间保留投影 | 需要改[上下文拼装与识别](context-assembly-requirements.md) §3.2，由该所有者决定；不在本文 |
| 备选：从压缩事实推导跨段状态 | 需要显式跨段状态契约，后续项 |
| 备选：提前压缩 | 改变上下文质量与成本，属于产品参数 |

## 8. 规则检查

本文涉及的部分都是 Capability：[history.rs](../../crates/dim-agent-capabilities/src/history.rs)、[context_policy.rs](../../crates/dim-agent-capabilities/src/context_policy.rs)、[context_preparation.rs](../../crates/dim-agent-capabilities/src/context_preparation.rs)、[agent_loop.rs](../../crates/dim-agent-capabilities/src/agent_loop.rs)。

- **§4 rule 1**：不适用。读取只读已提交事实；窄表与快照是派生状态，不改变其他模块的业务状态。
- **§4 rule 2**：不适用，交接没有变化；读取按已提交序号处理，失败丢弃当前批。
- **§4 rule 3**：不适用，没有 before 回调。
- **§4 rule 4**：满足。Timeline、Dispatcher、批次不变；读取集合与压缩结算属于 capability 自己的语义；退避计数是内存态，可从事实重建。
- **§4 rule 5**：满足。不新增批次、不写事件；结算与快照的写入顺序不变。
- **§4 rule 6**：满足。过滤只减少被解码的事件，采纳与消费规则不变；差分测试在固定前缀上比对。
- **§4 rule 7**：满足。读取使用 Timeline 的公开入口；窄表与索引是存储内部实现。
- **§4 rule 8**：**不满足**，需要机制所有者新增能力，见 §9。
- **§4 rule 9**：满足。压缩事实与快照的边界不变；快照仍是可选派生状态，不能替代事实。
- **ADR 002 D1–D5**：branch 作用域；不新增 wiring 模块、配置项或生命周期变化；接口语言中立；§9 明确列出需要新增的公开入口。
- **ADR 006 D1/D9**：规则只在本层陈述一次；本文不重述存储、压缩质量或提示词合同。

## 9. 向机制所有者提出的需求

- **Runtime（TimelineStore / Timeline）**
  - 被挡住的规则：模型上下文必须能从已提交事实重建，同时一次准备的读取量与峰值不能是恢复位置之前的全量。
  - 考虑过的机制：准备之间保留投影（需要失效规则，且要求改[上下文拼装与识别](context-assembly-requirements.md) §3.2）、持久投影快照（已有，跨装载）、一次性全量读取（峰值 45 MB）。
  - 通用能力：按 `kind` 过滤的分页读取与按 `last_sequence` 的游标推进；按 `kind` 与标签的定点选择（只返回位置与游标，不解释业务状态）；以显式初始状态与起点开始的批读入口。
  - 分工：运行时只提供通用索引选择（种类、序号、标签），**“最近一次完成压缩”这类完成语义由 Capability 判断**，不进入存储合同。
  - 对序号、批次、中断、重载的影响：只读已发布事件；不新增批次或事件；中断丢弃当前批；重载仍从事实重建。
  - owner 与 issue：runtime owner，[#708](https://github.com/arcships/dim/issues/708)。
- **Storage（原生存储）**
  - 需求：§4.3 的 `kind` 索引、输入与用量的标签索引、压缩窄表、升级回填、窄表缺失时的退回路径；格式版本由该文档决定。
  - owner 与 issue：存储文档与实现。
- **上下文拼装与识别（requirements owner）**
  - 需求：§3.2.3“每步仍然读取全量事件”改为“读取量只与恢复位置之后有关”；§3.2.4/§3.2.5 的缓存与派生位置结论不变，本文不引入准备之间的续读。
  - owner 与 issue：该文档所有者，[#708](https://github.com/arcships/dim/issues/708)。
- **HistoryRenderer / ContextPolicy / ContextPreparation / AgentLoop（本 capability）**
  - 需求：§4.1 的读取计划、§4.2 的读取集合、§4.5 的结算与失败语义、退避与超时；由本文自身的实现承担。

## 10. 删减检查

- 不新增事件、不新增配置项、不新增协议字段。
- 不新增缓存层：本文不含准备之间的续读状态；跨装载只保留既有快照。
- 窄表只存定位字段，可从事件重建，不成为第二份真相。
- 读取集合是常数表加穷举测试，不引入运行时注册或插件接口。
- 不改变压缩策略与摘要内容；不合并存储行；不引入内存统计接口。
