# ContextUsage：保存计算结果，不保存每次回调

Status: Draft.
Scope: branch ContextUsage 的贡献、累计值、压缩、provider 基准、持久化与恢复。
Decision: [ADR 023](../../adr/023-context-usage.md)。Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
合同：[ContextCompaction](ContextCompaction.md#51-productcontextmodel)、[保存边界](../owner-state-indexed-reads.md)、[插件合同](../plugin-contract.md)、[设计规则](../../architecture/rust-agent/principles.md)、[ADR002](../../adr/002-plugin-scope.md)、[rutis](../rutis-usage.md)。

## 1. 决定与边界

`contextUsage` 为 internal(hot-path)：接口使用本地请求引用和计数器，不跨语言传递；外部统计沿用 `session.context.usage`。

ContextUsage 是现有上下文功能内的独立 branch Capability，维护有效上下文的数字。生产者继续写原事实，不调用 Usage 更新、确认、删除或清理统计。Timeline/Store 不做 token 计算。

保留现有 session.context.usage 及错误合同，由 ContextPreparation.usage 委托本 owner。read 固定源截止和当前 tools 身份，等待该目标完成后返回；已就绪读不查历史消息、不重复消息计数、不保存；当前动态工具身份首次出现时按§3取得固定贡献。无配置直接 NotConfigured，不先渲染正文；可组合计数不支持返回 Model/Unsupported，不退回完整历史重建。

保存已经计算出的数字是必要优化：输入/输出已入上下文后，未来显示不应再次读取它们。处理过无关事件、delta、model-start 不构成保存理由。数字结果与单独 boundary 合为一份；不保存逐源数字树或请求正文。

## 2. 对象、改变条件与重载决定

| 实际对象 | 业务事件与具体条件 | 重载时保留/取得 | 持久化决策 |
| --- | --- | --- | --- |
| 内容贡献 | input/consumed 经 render_input 实际产生消息；output-end 结算保留的助手消息；tool-result 产生结果/附带消息；完成 compaction 产生 replacement/基线 | 保留已经计算出的贡献；只计算未处理来源及真正新增内容 | 消息贡献合并进该次数字结果，不另存同源 boundary，不复制正文 |
| 普通历史累计值 | 新源实际产生消息时累加 serialized_bytes、images、message_count；更新最后 HistoryEntry ID/来源位置 | 直接恢复数字；压缩不重写既有累计值 | 随数字结果保存；即使估算 token 四舍五入后相同，贡献/条目身份改变仍保存 |
| 压缩边界 | 仅 context/compaction.status=completed 且有效 retain_from | 保留切点、扣除的累计贡献、新 replacement/基线贡献；先前摘要被替换 | 同一数字结果；failed/skipped 无压缩状态保存 |
| 输出结算标记 | model-start 打开 request；output-end 关闭；旧 tool-result 无 call_event_id 时仅结算仍打开输出 | 从该 request 的 model-start/output-end/旧 flush 边界点查；不续跑旧模型请求 | 内存，不保存 started/settled 集合；原终态足够表达 |
| provider 基准 | 有有效 prompt_tokens，且请求、覆盖前缀、算法、固定内容、压缩均匹配；同请求更新测量 | 已建立基准恢复；尚未建立的实时关联作废；不读旧 request.messages 补证 | 基准与当前数字同一结果；相同数值、相同身份不写新数字结果 |
| 处理位置 | 六类相关源处理完且对应数字已保存；空消息/重复无效 usage 只提高内存位置 | 从数字结果及其匹配的小位置缓存取得 | 不逐源保存；无效果尾部在业务结束/固定读取交接处合并位置缓存，见§5 |

算法标识沿用 `dim/window-facts/v1`。贡献是两个整数 `{serialized_bytes,images}`，计数为字节/4+图片×1600；消息序列化边界严格复用现有 ContextCounter。Text 消息计完整 Message；parts 分项，Image 只计图片；ToolResult Blocks 只计实际 content blocks，不计 structuredContent 或额外包装。未 consumed 的输入不计入。

来源查询先限定六类源，再要求 `context.render=true`；`context/configured`、`model/usage` 是独立控制源，保留处理。带 `parent_call_id` 的子执行事实不进入上下文，不读取其正文、不计算贡献、不保存数字结果。

数字结果的完整字段：

- 顶层：`schema=context-usage/v5`、`source_through`、`epoch`、`branches`、`initialized`、`current`。
- `initialized={branch,sequence}`：本epoch首次建立当前累计值的来源截止，branch是branches中的序号。缺少旧统计时不因此补写更早位置。
- 可选 `legacy_result`：被复用的v1–v3统计事件ID；只读引用，不复制或改写它的节点。仅在真实统计变更后的正常结果中保存，不为升级单独写一份。
- `current={at:{branch,sequence},totals}`：at 是累计值对应的来源位置；不另存重复的 boundary_sequence。
- `totals.configuration`：Ready 时保存 event_id、request_identity、control_identity、configured_tools、algorithm、fixed 和 limits(window/output/threshold)；不支持/不可用有明确状态。
- `totals.ordinary={serialized_bytes,images}`、`message_count`、`last_entry={id,at}`、`local`、`growth`：普通消息累计贡献、条数、最后条目、本地估算及压缩策略用的累计增长数字。message_count为整数或null；null仅表示旧统计未保存条数，不表示0。
- `totals.compression`：event_id、cut、removed、replacement、removed_messages、replacement_messages，或缺省。
- `totals.calibration`：request_id、request_at、anchor_at、configuration、compression、request_identity、local、prompt_tokens，或缺省。

不新写root或数字节点，不复制逐消息正文、所有请求、Weak引用、started/settled数组。兼容读取可以按legacy_result访问已存在的旧节点。当前用量是固定贡献+ordinary−removed+本次replacement/基线。每次压缩替换上一份 replacement，而不是累加历次摘要。

current.at 是这份累计值对应的来源位置；source_through 是本份结果已处理到的位置。无效果来源只提高处理位置，不产生新 current。两者不能互相替代，否则压缩可能漏掉已经计入的消息。

## 3. 每种来源读什么、算什么

| 来源 | 字段和查询 | 计算/写入条件 |
| --- | --- | --- |
| input/consumed | input_id；按 ID 点读 input/message 的 blocks、user | 复用 render_input；每个新 Message 计数一次。没有 Message 时不写数字结果 |
| step/model-output-end | request_id、status；该 request 的最后完整 content 及之后仍保留的 delta | 复用 settled_request_output；不读被完整内容覆盖的 delta；保留内容计数一次，空输出不写数字结果 |
| step/tool-result | request_id、call_event_id、calls、contents、isError、contextMessages | 复用现有 tool_result_message；附带消息同源合并。旧 flush 先按 request 索引取得打开/结算边界，避免重复输出 |
| context/configured | 实际采用的 settings、system、tools | 算法相同保留 ordinary，只计算改变的固定部分/限制并撤销不适用基准；相同固定身份与限制零计数/零数字结果 |
| context/compaction | status；completed 时读 retain_from、replacement、context_baseline | 切点点查已保存累计值；只算新的 replacement/基线；失败/跳过不算正文 |
| model/usage | request_id、usage；只取 prompt_tokens 及缓存输入量 | 有匹配基准且测量变化才替换；相同数值不重复累加，不重算消息 |

六类是恢复源。model-start 不是持久进度源；打开输出的标记由请求控制事实按需取得。context/prepare、model/call、context/budget 用于有限实时关联，不读取它们的历史消息数组，不因为这些回调保存统计。

固定 tools 已在配置中时直接采用它。否则使用原 ToolRegistry 当前声明及 source key/version。集合身份不变直接复用 fixed 数字，不重新序列化工具。读取注册表当前声明的复制成本仍按工具数量计，不冒充零成本；不调用 bind/bind_current。动态集合不产生额外 Timeline 事件。只在内存缓存 configuration ID、source key/version 摘要、实际控制内容身份和固定贡献；source 版本变化但内容相同则复用数字。当前读采用这份固定贡献，并仅采用身份匹配的 provider 基准；重载重新取得当前集合。显式 context/configured 变化仍按原规则保存。

## 4. 复用已经计算出的数值

| 已有数值 | 用法 |
| --- | --- |
| Usage 当前 ordinary/fixed/compression 数字 | 直接用于显示、预算和压缩扣除，旧消息计数调用为0 |
| ContextPolicy 本次已算的完整请求估算 | ContextPreparation 将数值与本次不可变 Arc<ModelRequest>、算法/限制一起保留到最终校验；同一对象复用该数值，不能再 count_tokens 一次 |
| Usage 数字与本次准备请求严格同一有效前缀 | 原预算/最终校验入口可读同一 numeric estimate；只核对配置、切点、最后条目/消息数与本次准备的来源证明，不全文比较。请求经过实际增补/替换，失效后只计实际新请求一次 |
| model/usage.prompt_tokens | 直接作为匹配请求的输入实测，不从 completion_tokens/total_tokens 推导 |
| context/budget.full_estimate | 只有覆盖边界、固定内容和算法匹配才复用为该调用本地总量；它不是单条消息贡献 |
| context/budget.provider_baseline 或 compaction diagnostics | 不反推单条贡献；前者是混合估算，后者不等于 replacement 的贡献 |

现有历史事实没有逐消息贡献，完整请求的一个总数也不能拆出任意压缩切点。新增输入/结算输出的单源贡献仍各计算一次。这项计算不因“共享了同一个函数”就算成复用了数值。请求构造、host 实际改变内容后的最终计数、压缩新摘要计数是各自真实工作，分别记账。

复用只改已有预算和校验的数值传递，不增加 ModelProvider/AgentLoop 的 Usage confirm/discard，不增加请求事件载荷、请求基准事实或 producer→Usage 更新调用。`UsageEvidence` 不携带持久 root；压缩策略需要的增长只通过私有累计数字前驱查询相减取得，不增加公开历史统计 API。临时数值证明不跨重载、不使请求正文常驻。

## 5. 保存与位置缓存

同一原子源批次按原位置处理，用转移结果的标量标记记录变化：新增消息数/贡献、实际配置身份/限制改变、成功压缩、有效基准替换。不要 clone整份状态或重算集合比较。`context-usage/recovered` 的每份结果内部包含该源之后的累计值，不同时写 boundary；本批有几个需要保留切点的内容源，就保存几份，最后一份也是当前结果。提交后才发布。

不同源批次不为少写而等候未来事件。同批多个普通消息源仍须保留压缩切点可寻址的累计值：本批每个实际产生消息的源保存一份 numeric 结果，最后一份同时承担当前结果，不额外再写一份全批快照。固定配置/基准变化也保存其来源边界；不能把较晚基准套到较早切点。一个源中的多条 Message 合并为一个边界，因为 retain_from 按源事件切割。

空消息、失败/跳过压缩、无效/重复 provider 数字没有新 numeric。它们如果发生在最后保存的数字之后，需要确认“已经处理且无效果”，否则每次重载都会重新检查同一段来源。确定保存一种小型派生缓存：

`context-usage/covered`：`schema=context-usage-covered/v1`、`result_id`、`source_through`。result_id指向它延伸的最新数字结果。不含state、贡献数组、正文或请求身份。

仅在以下条件全部满足时写一份：本 owner 已处理固定范围；存在晚于当前 numeric/covered 的相关源；本范围没有需要保存的数字变化；到达 turn/end 的已完成范围，或正在兑现固定 read 目标。无相关源、已被结果覆盖、只有 model-start/delta/interrupt 时不写。多个无效果源在同一交接处只写一份；普通实时回调不写。后续有 numeric 时覆盖这段处理位置，不再另外写 covered。

该缓存是避免重复检查无效果尾部的性能数据，不是业务事实，也不是所有 owner 的通用快照。未变化的普通 getter为0写入；等待新来源的 getter可能等到 owner 完成必要保存，不能把这种情况伪称为已经就绪读取。读一次失败/空来源之后，再次重载不重做这项工作。

## 6. 恢复、切点和查询范围

正常恢复先读最新 numeric 和最新 covered，各1次。只有 covered.result_id 匹配当前 numeric 且范围合法才采用其位置；更早数字的缓存不延伸新结果。再点读保存配置的计数控制字段，并查询 `(saved_source_through,S]` 的六类源，身份每页128；空后缀即结束，正常单分支共4次逻辑 query。非空页逐对象取得必要正文、消费释放；没有额外六次 Last 探测。未显式配置 tools 时，当前集合首次/版本变化另需一次该配置 system 字段点读。不读旧正文，不保存完整源队列。

S 在订阅实时来源之前固定。发布后才启动数字任务，但不能在任务被调度时重新取 S，将已经交付的新输入或请求归入旧历史。

压缩切点：按 retain_from 的逻辑ID取得来源位置；在同一 epoch 的 numeric 结果中按 `data.current.at.branch = 切点分支序号 AND data.current.at.sequence < 切点` 倒序取最近累计值，再按其数字扣除。采用已有 Field 排序语法；必须由[源位置索引](../appserver/timeline-state-read-indexes.md)执行，不用事件提交位置代替源位置，不用 LIMIT1 掩盖历史筛选。单分支2次逻辑query（ID位置+数字边界）；祖先切点按固定chain各段定位，最多R次范围探测，R是分支链长度。边界之前无普通消息时累计值为0，不能因此遍历旧来源。

切点不能回到已被替换的范围。初始化之前的数字不能假设为0；缺少统计的首次初始化按§6.2处理。数字结果 source_through 可能小于自身提交位置，查询同时核对事件在固定可见范围；数值索引不是绕过祖先 cutoff 的权限。

输出结算状态按 request 的已有 request_id 索引取得最后 start/end/旧 flush边界。正常重载没有打开输出时不读取任何旧输出内容；资源 owner 补出的终态属于新的真实内容源。已有 numeric 之前的输出不重计。

### 6.1 统计版本与旧数据

新写私有统计使用context-usage/v5；读取v4原有数值及v1–v3累计数字。v5允许旧统计未保存的message_count、压缩消息条数和控制缓存字段缺失或为null，并增加可选legacy_result。缺失消息条数不使用数值证据的请求快捷校验；请求沿已有实际请求校验，不把未知条数填0，不为建立无法证明的provider关联读取旧请求。v4没有这些缺失语义，缺字段仍按损坏处理。

这是私有派生统计的显式版本决定，native v41、公开事件和Query API不变。Store将扩展data视为所属owner的数据；现有epoch/源位置索引字段不变。不把不兼容结构继续标成v4。旧统计原字节保留；未知schema、算法不兼容和实际读取的坏引用报错，不猜测、不重算掩盖。

v1–v3恢复从最新结果的state.root.summary取得ordinary、固定贡献、压缩、基准、last_entry和growth。source_through、branches与可见截止必须合法，算法须匹配。恢复时补取配置的实际控制字段和tools声明存在性，只读取未完成源后缀；无新来源时0旧消息计数、0数字补写。未来需要升级前切点的累计值时，先按ID读取legacy_result及所需数字节点，整段已累计范围直接采用它的summary，只读取落在切点上的节点；不回放消息，不把旧节点改写为新事件。

### 6.2 确实没有统计

生产旧库可能从未保存 ContextUsage 数字。resume 不为显示用量初始化完整历史，也不后台分页补算；统计未建立时，usage 返回明确的未就绪错误，界面不显示伪造的 0 或把旧请求用量冒充当前值。只有压缩 replacement、没有普通消息的迁移会话同样等待实际请求，不在加载时读取 replacement 或 context_baseline。没有历史内容的新分支可直接建立空累计值。

首次实际请求本来就必须取得有效上下文。ContextUsage 在该次请求已有的上下文投影上计算初始内容贡献，复用已有配置、压缩切点与条目身份；不再自行读取同一历史。只将数字和来源截止交给本 owner 的按序任务，正文在请求处理完后释放。初始化只写截止位置的一份当前数字，不补建更早位置；此后恢复读取这份数字。初始化与新的源处理按固定来源截止串接，不能漏掉初始化之后的内容，也不能重复累加。

初始累计之前没有数字的位置不能当成 0。实际压缩需要一个旧切点时，复用该次请求已有的投影计算被移除的贡献，合并既有 removed，随压缩统计保存；不为所有可能切点提前建记录。请求侧只传递已经取得的数值，不增加 producer confirm/discard、模型请求基准事实或每条来源快照。

fork优先复用祖先可见数字和边界；不可见或没有兼容数字时按 6.2 等待实际请求建立数字。不能使用父分支切点之后的统计。

## 7. provider 基准与异常

建立准备关联前等待 Dispatcher 已交付到该请求覆盖位置，避免更早的配置通知清掉新关联。实时关联保留一个准备槽和一个最近调用槽。Dispatcher 收到匹配的 model/usage 时，为该事实位置暂存请求身份、来源位置及已验证的数字校准基准；不保留请求正文。数字任务处理该事实前等待 Dispatcher 已交付到该位置，消费后立即删除暂存项。后续调用或配置变化不能抹掉已经收到的测量依据；重载不恢复这些实时暂存项。预算/校验通过数字读取验证实际请求的配置、压缩、消息数和覆盖位置，保存该次验证的 Arc<ModelRequest> 的 Weak、原 prepare 身份和已经算出的 local。call 与这个已验证对象相同且请求/配置/提示身份未变才形成候选；不能只和最初 prepare 对象比较，因为正常准备流程也会生成新的不可变请求。弱引用不保留正文；不持久化地址，不用失效地址比较，不建请求表。覆盖期间配置/压缩/动态工具改变，候选作废。图片或请求专有增补不能证明可校准时使用本地估算，不读取历史 request.messages 补证。

统计覆盖该源位置后，候选只保留配置/压缩身份、覆盖位置本地总量及request_id。有效model/usage要求prompt_tokens有限且>0，且不少于cache_read+cache_write之和。当前估算为实测输入+覆盖后本地增量，缓存量不再重复相加。相同request后续usage替换旧测量；旧候选不覆盖更晚基准。相同身份与数值不写新numeric。已建立基准存numeric；未建立候选在重载作废，旧活任务不重启。

异常取消释放当前读取和临时请求关联，已完成数字正常提交；未提交部分由下次恢复处理。可处理的模型/工具异常由其owner结算原终态，不为Usage写paused。提交失败不推进持久处理位置或发布成功数字；读取取消不撤销已接受的业务事件。

## 8. 实际流程的数量

以下“计数”指ContextCounter处理消息或固定内容，不包括请求必须的构造、网络和原流式持久化。B_in/B_out为本轮实际采用的输入/保留输出大小，F为变化的固定内容大小。

| 流程 | 数字/位置新增事件 | 正文读取与计算 |
| --- | --- | --- |
| 普通一轮，无工具/压缩，有新的有效provider基准 | 输入numeric1、基准numeric1、输出numeric1，共3；covered0 | 输入/输出各一次贡献，处理B_in+B_out；无旧历史计数；prepare/最终校验复用同一次已确定数值 |
| 没有有效或没有变化的provider基准 | 输入/输出numeric2 | 仍只有输入/输出两次消息贡献；usage只改标量，不再计一次输出 |
| Goal自动执行一轮并继续 | 同上；Goal运行快照0 | 只计算原续行输入与本轮输出 |
| 固定配置改变 | numeric1；Prompt引用0 | 仅变化固定部分F；消息贡献0次重算 |
| 同值配置/提示、重复已覆盖统计读取 | 0 | 0贡献计算；提示刷新原逐目标比较另计 |
| 无内容失败，相关源尚未覆盖后turn结束 | numeric0、covered1 | 空结算判定及小控制查询；旧正文0；再重载不检查这段失败来源 |
| 中断并保留部分输出 | 该输出numeric1；不另为interrupt保存 | 只读取本请求保留的完整内容+后续片段，按保留字节计一次 |
| 压缩完成 | 新替换贡献numeric1；原compaction事实保留 | 2次切点定位+replacement/基线贡献；旧前缀0次求和/计数 |
| 正常已结算重载 | 0 | 4个逻辑query；1份numeric及配置控制；0消息正文和消息计数，动态tools首次另读system并计算固定贡献 |

一轮原事实明确为input/message、applied、consumed、turn/start/end、model-start/output-end、model/usage、context/budget、context/prepare、model/call各1，加D个原输出事实，共11+D。Usage增加2或3，不是再加2份boundary和逐model-start快照。带工具的一轮按真实tool-call/tool-result及附带消息另列，不用假设积压量作为优化依据。

## 9. 必要修改与机制要求

| 位置 | 必要修改 |
| --- | --- |
| context_usage.rs、context_usage/sources.rs | v4小数字状态、按实际转移保存、covered合并、输出控制按需取得、原read屏障 |
| context_usage/numeric.rs | 私有数字树不保留；累计边界使用已保存numeric事实 |
| context_model.rs、host.rs | 唯一可组合计数定义；可选默认接口保持旧计数实现兼容 |
| history.rs/settled_output.rs | 复用已有投影/结算；不复制转换规则或缓存历史 |
| context_preparation.rs、context_policy.rs | usage委托；本次请求已有数值在原prepare/validate间复用；不拥有第二套累计统计 |
| defaults.rs、lib.rs | 原上下文功能安装与内部服务名 |
| ModelProvider、AgentLoop等 | 不要求Usage确认/清理调用，不增加事件字段或统计状态 |

| owner / 问题 | 现有能力与确定要求 | 序号/取消/重载 |
| --- | --- | --- |
| Store / #1320 | ID、kind/位置、request_id足够读取新源；按源数值位置取前驱尚无索引，补通用标量范围索引与匹配 | 原fixed scope与事务；v40索引方案归机制文档；不在Store计token |
| Timeline / #1320 | 现有append/query足够 | 不改before/提交/发布；数字保存失败不发布；无新状态CRUD |

## 10. Rule check与删减

| 规则 | 本设计 |
| --- | --- |
| principles§1；ADR002 D1 | 统计、任务与恢复归独立branch Capability |
| ADR002 D2–D5 | 无接线层、新用户配置；贡献/服务合同语言中立 |
| §4 1–4 | 不启动其它owner任务，不代交接；handler只通知自己的任务；无before I/O |
| §4 5、6 | 原上下文事实不变，统计不进入模型上下文；保存成功后发布 |
| §4 7、8 | 声明原依赖；机制需求见§9 |
| §4 9 | 数字服务预算/显示/压缩；位置缓存只避免重复处理已确认无效果的尾部 |

删去数字树、独立boundary、每个model-start保存、started/settled持久数组、每次只推进内存位置也保存完整状态、request-base事实、生产者confirm/discard。保留的贡献累计数字、切点、基准和小处理位置缓存各有明确读路径。复用现有上下文flow验证计数等价、同批/分批、重复/迟到usage、空输出、压缩、fork、提交崩溃和关闭；记录实际SQL候选、解码字节、计数调用和WAL，不以LIMIT1或输出行数代替证明。
