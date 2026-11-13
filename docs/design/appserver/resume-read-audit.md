# Resume 读取核对与历史增长对照

Scope: [#1320](https://github.com/arcships/dim/issues/1320)，PR #1442。这里记录读取清单、六组配对样本和历史增长对照；不替代完整 CLI/ACP/Web 可输入延迟验收，也不声明全部优化完成。

## 1. 核对结论

发现并修复一个不必要的历史查询：无标题会话收到运行环境等内部输入时，SessionTitle 先找历史第一个有效输入，再判断当前输入没有资格触发标题。现在先按当前输入 ID 判断，不符合条件立即结束。保留同一个查询截止位置、第一条有效输入的标题来源和已有重试行为。

另外发现一个业务流程问题：BackgroundExec 在重载时为丢失的进程写失败结果，然后通过 Steer 通知启动新 turn。新请求构造和首次 Usage 计算会读取旧上下文。未完成任务场景因此没有通过“不随已完成历史增长”的检查；不能用首屏或 pending 查询的低开销掩盖它。详见 §5。

## 2. 读取清单

以下是原生装配的正常打开入口及其条件路径。逻辑 query 与底层 SQLite statement 不等价；一次字段查询还可能读取分支头、定位物理记录并解码字段。表中页只指当前对象或有界显示窗口，除注明的显式操作外不指完整历史。

| 入口 / 实际代码对象 | 读取对象与次数边界 | 加工和历史增长关系 |
| --- | --- | --- |
| `Agents::acquire_checked`；`TimelineTree::restore` | Session 配置、分支目录；每个子分支按子分支身份查询创建/结束边界 | 遍历分支目录，成本随分支数增长。普通打开不调用完整历史校验；活分支装配各自 owner，结束分支不恢复旧执行 |
| `InputQueue::rebuild`，`input_queue/recovery.rs` | 最新 turn/start、turn/end；普通/交接输入当前成员；非空页按 ID 批量读 applied/withdraw，旧数据缺 mode 才点查这些输入 | 空队列四个逻辑查询；只解码当前输入的必要字段，不读已 consumed/deleted 的输入正文 |
| `AgentLoop::rebuild` | 当前 driving 输入及采用记录、最新 interrupt/turn 控制、未结算压缩/步骤 | 只恢复当前控制和结算遗留工作；每页当前对象，不重新执行已完成 turn |
| `ContextPreparation::rebuild` | 最新控制、SteerMark，未结束自动压缩、尚无 model/call 或 output-end 的 prepare | 结算遗留准备操作；不在此构造完整模型上下文 |
| `ModelProvider::rebuild` | 最新控制、SteerMark，未结束 model/call 及其未结算 tool-use | 取消旧请求；页数随未结束对象数增长，不随完成请求数增长 |
| `ToolExecutor::rebuild` | 当前未完成 tool-call、本批 calls、对应子工具结果/子分支终态 | 必须读取要结算的调用；不读取已闭合的调用批次 |
| `Wait::rebuild`；`Permission`；`ToolBinding::reconcile` | 各自最新 turn 控制、permission/changed、context/configured | 分别定位最新记录；Wait 不恢复旧唤醒队列；工具绑定使用采用的设置 |
| `Prompt::restore`，`prompt/recovery.rs` | 最新有效引用、采用配置；提示变更后缀检查；有效正文按来源 ID/数组下标读，生命周期按 scope 键定位 | 随当前有效条目增长。正常打开最多处理一个可证明的未保存采用；多条旧采用缺引用时要求显式迁移，不能退回完整历史重放。`source_through` 设为本次打开位置，之后的 `recover_sources` 只读新产生的后缀 |
| `ContextUsage::restore/initialize` | 最新数字、covered 位置、数字边界/配置；后缀只判断相关来源是否存在 | 有数字则恢复数字；缺失或过期就未就绪，不在打开时遍历旧消息。第一次真实模型请求仍会建立数字，见 §5 |
| `Goal::load` / `pause_reloaded` | opening 位置和最新 `goal/state`；解码一份完整目标信息 | 未结束状态变为内存 paused，不读取旧轮次、评估或输入目录。`load_current_work` 只在显式 Set/Resume 时调用 |
| `HooksBranch` 的 Dispatcher 初始化 | 读取内存 committed 位置，设置 `view.last`；历史 query 为零 | 不重放历史 Hook。有 SessionStart handler 才在激活后查 turn/start 是否存在；已开始过的会话一次 Exists 即结束。尚未开始 turn 时，另查当前未 consumed/deleted 普通输入中的启动标记，最多检查当前 K 项。实际执行的配置命令耗时另计 |
| `SessionTitle::attempt` | 设置、当前标题；当前 input ID 资格查询一次。只有有效用户/Goal 目标输入才继续查第一条采用、正文和模型设置 | 本次修复移除普通 resume 内部通知触发的历史关联查询。真正有效输入触发的“第一条采用”查询仍是现有通用关联计划，不能宣称该操作已变为常数成本 |
| `Approval::rebuild` | 未 respond/cancel 的 approval/request 成员 | 取消无法恢复的审批执行，不重建旧等待者 |
| `BackgroundExec::rebuild/repost` | 未 ended 的 exec/started；尚未投递的 exec/ended；结果对应 start 按 task_id 查 | 读取对象本身已被索引限定。但失败结算/补发通知会启动新 turn，属于 §5 的未解决流程 |
| `collaboration::restore` | 未交付操作、未结算请求、未投递通知，必要来源按 ID 查 | 当前成员分页及逐项交付/结算；不读取完整已完成协作记录。若实际交付产生 Queue/Steer，同样是新执行入口，不能用空集合测试证明它没有模型请求成本 |
| `session.question.pending` | 一页尚无 tool-result 的 ask_user 调用身份/名称，再按 ID 只取对应 calls 下标 | 随当前问题批次增长；不恢复 Questions 的旧内存 waiter |
| `session.input.pending` / `exec.pending` / `collaboration.pending` | 固定 through 的各类当前成员及本页字段 | 只返回当前队列/任务/交付对象；接口分页不是历史补放 |
| `session.turn.get` / `goal.get` / `context.usage` | 最新 turn 和相关输出定位；Goal 活分支内存状态；Usage 已加载状态 | turn.get 仍有索引查询；不能把全部 getter 都写成零 SQL |
| `session.history.display` → `display_transcript` | kind 索引定位普通内容与压缩位置，再只读入选正文/压缩摘要 | 初始 32，空页续读 64，最多四页；显式 earlier 另用 256。大消息成本随实际入选字节增长。压缩摘要投影仍要触及原始大 JSON 所在物理页 |
| CLI `Window` / ACP load、resume、reconnect / Web window | 共享固定 through、游标、首屏停止条件；另取 pending inventories | 历史不重建控制状态；连接恢复替换有界窗口。旧服务器 fallback、显式 raw history/export、模型请求内容构造分别保留原合同，不能算作本次正常 transcript 首屏 |
| `FileReads` | 新实例的绑定表为空；打开不读取旧文件或工具结果 | 文件内容与压缩边界检查发生在实际 read/write 工具调用，不是 resume 的恢复队列 |
| permission / skills / provider catalog、插件配置 | 当前配置、目录及已启用服务 | 不属于 Timeline 全历史；文件、外部进程和网络初始化成本另计 |

底层对应关系：

| 查询形状 | 索引/执行证据 | 实际扫描范围 |
| --- | --- | --- |
| kind + 位置，first/last 或页 | `timeline_events_by_kind(session_id, branch_id, kind, sequence)`；`query.rs::kind_positions` | 每种 kind 的限定坐标；合并输出读取序号范围，再对最终入选位置读正文 |
| ID 点查 | `timeline_events_by_id` 与 `timeline_output_ids`，随后事件主键 | 当前 ID 对应的物理记录及选定字段；数组条件仍须检查该对象的数组 |
| 当前成员 | `timeline_member_roots` 的完整主键前缀与截止序号，`timeline_member_nodes` 主键 | 本次截止位置的成员根及当前成员路径；不是从所有 origin 开始排除已完成对象 |
| 最新配置/状态 | kind + sequence 的反向定位；关联字段按已有 property 索引 | 最新记录或当前引用，不靠返回 `LIMIT 1` 推断扫描量 |
| 修复前标题第一条输入 | kind 索引后 `MATERIALIZE events`、`SCAN a`、关联自动索引和排序 | 将所有候选 input/message、input/applied 做成临时结果；LIMIT 1 未阻止这笔开销 |
| 修复后内部输入资格 | ID 索引定位一条 input/message，再判断 user/blocks | 不执行上述历史关联；SQLite 仍可显示常量表、参数数组和单条结果的 SCAN，不等于扫描历史事件 |

源代码落点：`crates/dim-agent-capabilities/src/` 下对应 owner、`crates/dim-goal/src/{read.rs,branch.rs}`、`crates/dim-session-title/src/lib.rs`、`crates/dim-agent-questions/src/pending.rs`、`crates/dim-app-server/src/{collaboration.rs,connection/tree.rs,storage/query.rs,storage/members.rs}`。跨模块边界仍以[总设计](../resume-read-design.md)为准。

## 3. 六组样本与测量边界

每组分别追加 128、1,280 个旧的已完成 turn，最近四轮及特殊尾部内容完全一致；比较时只归一化绝对 sequence 和压缩切点，然后校验尾部 SHA-256 相同。底库由产品 Session 创建，追加事实走 `SqliteTimelineStore::append_batch`，并由原生写入维护索引。不是绕过索引直接插入 SQL，也不是端到端执行这些历史模型轮次。

| 样本 | 旧历史差异 | 相同的当前内容 | 物理记录数：短 / 长 |
| --- | --- | --- | --- |
| 普通聊天 | 已完成用户输入、文本输出、turn 边界 | 最近四轮 | 944 / 9,008 |
| 工具密集 | 每轮四个已完成工具调用 | 最近四轮同样含工具 | 2,528 / 24,416 |
| 内部输入密集 | 旧 Raw 输入 | 最近四轮普通聊天后，48 条已 consumed 的内部输入 | 1,088 / 9,152 |
| 大消息 | 每个旧输入 4 KiB | 最新输入含 512 KiB 文本 | 944 / 9,008 |
| 压缩 | 已完成普通轮次 | 同一份约 2.4 MB replacement 及切点含义 | 945 / 9,009 |
| 未完成工作 | 旧审批、后台任务均闭合，通知已投递 | 一份未响应审批、一份未结束后台任务 | 1,842 / 17,970 |

全部样本无标题，避免手工标题掩盖 SessionTitle 路径。全部没有保存 Usage 数字；验证的是缺统计时不得在纯打开流程重算。已有数字、多个分支、外部插件、MCP 和实际 Hook 命令没有在这六组中分别制造增长样本，不能把静态代码核对写成这些分支的动态验收。

基线为 `ef45f65a9` 的生产逻辑，二进制 SHA-256 `26393c0d9eb208b0e0d8d4be415aef4870a3936add4a0b194a77236a0fc787f8`；修复只调整标题查询顺序，二进制 SHA-256 `a95f193beb4c79c94a132aa6e95c955b54c992c467a5d8e0c98bc1c945bef226`。均为 release、native v44，使用独立 home 和数据库副本。启动前 executable、数据库和配置的驻留页均检查为零，没有预热数据库。

探针执行 hello、checked attach、methods、branches、实际首屏分页、六个 pending/get、usage、permission、skills/catalog。SQL 跟踪覆盖打开到这些操作完成后 250 ms，包括后台异步工作；不含 detach/shutdown。每次新建守护进程。另用同一无插桩二进制独立运行测 `/proc` I/O 和 `Pss_Anon`，不拿 GDB 的时延/内存当产品数据。

SQL 跟踪读取 SQLite 3.54.1 的实际 statement VM/fullscan/sort/autoindex 计数，记录返回 Rust 的文本列字节。查询计划使用同版 SQLite、捕获的 SQL 和绑定参数：普通场景 45 个不同 SELECT/WITH、未完成场景 64 个，全部成功解释。标量函数仅在 EXPLAIN 中用同签名占位，实际执行计数来自产品进程。

“文本列字节”包含字段 JSON、正文 JSON、ID、设置及 schema 文本，不等于独立消息字节，也不是所有 Rust 解码次数。它遗漏 SQLite 内部 JSON 解析和临时结果复制，正是标题问题需要 VM/扫描量同时检查的原因。SQLite 文件读取量另由实际 `pread64` 记录；匿名内存是测量结束时的值，**不是峰值，也不包括 CLI/Web 和文件缓存**。

## 4. 历史增长对照

### 五组没有自动执行的新请求的结果

下表是整个诊断窗口的实际 SQLite VM 指令数，包含设置、索引维护和打开时原有写入，不能解读成一个查询的成本。

| 场景 | 修复前：短 / 长 | 修复后：短 / 长 |
| --- | --- | --- |
| 普通 | 80,580 / 578,307 | 19,123 / 19,123 |
| 工具 | 81,393 / 579,195 | 19,764 / 19,725 |
| 内部输入 | 123,500 / 684,102 | 31,595 / 32,201 |
| 大消息 | 80,942 / 578,468 | 19,136 / 19,136 |
| 压缩 | 80,966 / 578,715 | 19,383 / 19,526 |

修复前标题临时结果 fullscan 步数：普通 833→7,746，工具 836→7,749，内部输入 1,124→8,037，大消息 837→7,748，压缩 835→7,748。修复后五组访问事件记录的 SELECT 均为零 fullscan 步；仍有固定 85 步 schema 检查，不扫描会话历史。

| 场景 | 修复后 SQL 执行数：短 / 长 | 返回 Rust 文本列 bytes：短 / 长 | SQLite 文件读取 MiB：短 / 长 | 守护进程匿名 MiB：短 / 长 |
| --- | --- | --- | --- | --- |
| 普通 | 433 / 433 | 54,558 / 54,657 | 1.47 / 1.78 | 13.49 / 13.65 |
| 工具 | 439 / 435 | 56,630 / 56,709 | 1.55 / 1.86 | 13.61 / 13.74 |
| 内部输入 | 483 / 523 | 77,482 / 77,567 | 2.18 / 2.63 | 13.91 / 14.01 |
| 大消息 | 434 / 434 | 582,465 / 582,564 | 2.01 / 2.23 | 13.52 / 13.52 |
| 压缩 | 436 / 448 | 54,359 / 54,573 | 6.13 / 6.41 | 15.80 / 15.96 |

内部输入场景多出的 SQL 来自新通知写入触发的 FTS 内部段合并，主要是 `timeline_text_fts_data/idx` 的 DELETE/REPLACE/INSERT；不是消费者翻页读取旧输入。数据库增大也会改变索引页读取和页布局；所以不要求实际磁盘字节完全相等，但不能出现随旧事件数线性增长的正文解码。极端 FTS 合并延迟没有由这两档样本穷尽。

普通长会话修复前后匿名内存为 31.94→13.65 MiB，进程 read-call 字节为 18.64→2.81 MiB。其他长样本匿名内存：工具 31.79→13.74、内部输入 34.57→14.01、大消息 31.70→13.52、压缩 26.00→15.96 MiB。单次样本不证明很小差值的显著性；移除历史临时表的数量级差异有查询计划和扫描计数交叉支持。

普通/工具/大消息/压缩均一页、32 个所选事件；内部输入三页、160 个所选事件。长短样本页数相同，正文大小只因序号编码长度等有少量差异。大消息的半 MiB 仍需传递和解析；压缩样本返回字段很小却读取约 6 MiB SQLite 页，说明摘要字段投影并未消除原始 replacement 的物理页成本。此次不新增摘要缓存或持久化结构。

## 5. 未完成后台任务：检查未通过

实际流程如下；查询未完成对象本身已经使用索引。

```text
打开 Session
  BackgroundExec.rebuild：找到一条未结束 exec/started
    -> exec/ended(failed)
    -> ended / wake_input：input/message(mode=Steer)
  InputQueue：input/applied
  AgentLoop：turn/start -> step/model-start
  ContextPreparation：context/prepare
  ContextPolicy：读取旧上下文、计算预算
  ContextUsage：首次请求建立统计
  ModelProvider：发起新的模型请求
```

同样的首屏探针加 250 ms 后，未完成任务短/长样本的匿名内存仍为 19.25 / 47.11 MiB，进程 read-call 为 6.46 / 27.67 MiB。这时长样本的新请求尚未完成，所以这是已发生的开销，不是完整请求上限。重型逐列 GDB 跟踪使执行更慢，在同一 250 ms 截止前尚未进入完整请求；其约 2.6 万指令不能拿来解释无插桩进程的全部内存。

补测改为等待这次自动启动的 turn 完成，使用本地确定响应的模型替身，排除网络重试等待。只在 statement 结束时取计数，避免逐列断点显著拖慢整段上下文读取；无插桩运行单独测内存/IO。结果如下：

| 自动 turn 完成后的总成本 | 128 个旧 turn | 1,280 个旧 turn |
| --- | --- | --- |
| SQLite 执行次数 | 1,158 | 3,834 |
| SQLite VM 指令 | 555,429 | 5,097,091 |
| 守护进程匿名内存 | 19.25 MiB | 47.59 MiB |
| 进程 read-call | 6.68 MiB | 36.95 MiB |

其中上下文范围查询消耗约 30 万→371 万指令，正文按位置批量读取约 11 万→107 万；请求前缀定位还从约 1.1 万增至 10.8 万。即使 SQLite fullscan 计数很低，沿索引读取全部所需上下文仍会线性增长，不能只检查是否出现 `SCAN`。这个完整请求对照确认了流程问题，不能解释成首屏多取了几条消息。

这不是让 pending 接口改用另一个索引能解决的问题。`BackgroundExec::ended` 明确包含重载生成的 failed；`wake_input` 固定产生 Steer。现有 [BackgroundExec 合同](../../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)及 `idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end` 流程明确保留补发唤醒。它与“打开只恢复和显示，不因旧任务自行开启模型轮次”的目标存在业务冲突。

建议的下一步业务决策是：重载结算和补发的旧结果仍保存、仍向用户展示、仍进入下一次请求的上下文，但使用不启动 turn 的 Append；本次运行中真正结束的后台任务继续 Steer。保留 exec 终态与通知身份标签，不新增快照、缓存或索引；同一结果不能因重载分支与实时 Dispatcher 各投递一次。落点是 BackgroundExec 的恢复/投递区分及已有重启流程测试，不改 ContextPreparation 的正常请求构造。

本次未静默改变这项唤醒合同。协作补交付若同样产生 driving 输入，需要按操作本身的交付合同决定是否应开始新执行，不能把所有恢复通知机械改成 Append。这个检查结果也不允许把“六组都测了”写成“六组都通过”。

## 6. 验证、范围与删减检查

生产代码只调整 SessionTitle 的既有两个查询顺序；未新增类型、缓存、查询接口、索引或存储版本。同步其字段读取合同，修正总设计中已过时的首屏页尺寸描述并改为链接唯一的显示合同。

复用 SessionTitle 全部 25 个流程用例，涵盖普通/Goal 标题来源、追加上下文、初始手工标题、恢复/fork/rename、取消和 Scheduler。首次并行执行时，一个初始标题等待通知测试超时；顺序重跑该文件及后续 native/Scheduler 文件全部通过。未改测试超时或降低断言。受影响 crate 的 all-target Clippy、workspace 格式检查通过；本次没有重跑完整 workspace CI。

删减检查：直接移动既有资格查询即可阻止无效触发；没有理由新增 eligible 状态缓存或恢复事件。保留第一条采用查询，因为有效输入到来时，标题仍须使用历史第一条有效采用的内容，不能改用当前通知或当前用户输入。该有效触发路径的通用关联成本已明确列为范围边界。

实验原始日志、SQL/参数/计划、配对摘要和探针保留在开发机 `/mnt/data/dim-resume-acceptance-20261113/`；数据库和临时插桩不提交仓库。本报告记录的是本次固定版本的证据，后续状态与决策继续写入 #1320 / #1442。
