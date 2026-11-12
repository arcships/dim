# Timeline read consumers and page execution

Scope: bounded display reads and execution of the existing property query contract.
[Timeline property queries](timeline-property-query.md) owns query semantics;
[Clients](clients.md#opening-a-conversation-with-a-bounded-window) owns window sizes.
This design changes no event format or search matching rule. The [native storage design](appserver/native-storage-format.md) owns format decisions.

## Consumers

| Consumer | Read and lifetime |
| --- | --- |
| CLI / ACP | Existing initial windows and owner inventories under the Clients contract. Export reads its explicit full prefix. Reconnect consumes the missing live prefix. |
| Web main / selected lines | Initial backward window under the same client limits. Reconnect replaces opened windows. Earlier content requires an explicit action. Raw pages last only until projection. |
| Web unopened lines | Directory identities/lifecycle and pending question state; no transcript read. Pending questions are read at each directory head, then updated by live facts. |
| SessionTitle | Four one-row property queries at one published cutoff: first adopted operator id, triggering operator id, selected user content, latest adopted provider/model. [SessionTitle §0.1](plugins/SessionTitle.md#01-fields-read-for-one-title-attempt) owns selection and eligibility. |
| InputQueue | Read current ordinary/handover members and their existing decisions, plus latest turn controls. No queue snapshots or historical source replay. [Queue recovery](input-queue-recovery.md). |
| Prompt | Save effective references and adoption versions only on real adoption/retirement. Read known scope boundaries and selected entry elements. [Prompt state](prompt-state-read.md). |
| ContextUsage | Persist computed numeric results; coalesce no-effect processing into a small position cache at handover boundaries. No separate boundary plus result writes. [Usage](plugins/ContextUsage.md). |
| Goal | Live reads use the owner's memory. New instances read the last complete business state and map unfinished execution to paused; they do not load control history or write progress snapshots. [Goal state](plugins/Goal-state-read.md) owns the lifecycle. |
| Branch tool activity | Raw page endpoint from batch metadata, then display fields within that range. [Timeline branches](timeline-branches.md#activity-page-execution) owns rendering and cursor behavior. |
| Recall search | Candidate positions/identity fields, then the required facts for small batches of candidates, then existing narrative rendering and exact matching. Search stops once the result page and continuation evidence are available. |
| Context request preparation | Selected adopted inputs/tool results and required request components for that request. Rebuilding a concrete request may legitimately consume a large selected history. The request owns that memory. |
| Restore integrity / explicit export | Their full-range reads serve integrity or an explicit full export; they do not define the default display or state-query path. |

## Page execution

SQL must distinguish two ranges:

- The outer candidate window advances past the output position cursor.
- Related facts retain the original fixed range, including ancestor cuts. A terminal or an input before the cursor can still decide whether a later origin matches.

For an exactly representable position query, SQL bounds the outer range and takes at most the requested
number plus one before field projection. Physical rows that merge output deltas remain eligible when
they overlap that range. Logical output positions are clipped afterwards. A predicate that still needs
referenced content cannot use that early limit.

Input ids use the event identity index. Request relations use the existing request identity index.
A related disjunction can compile into separate indexed alternatives. Safe text predicates inside
those relations test the text index by event identity or request partition; they do not rebuild a
complete temporary text-candidate set for each output page. Ancestry concatenation and publishing
tails keep the conservative existing candidate path when a direct index predicate cannot preserve it.

General grouping, unsupported relation shapes and content-dependent predicates retain their complete
fixed scope. This design does not promise constant work for arbitrary query expressions.

The store's default batch page first resolves the final batch endpoint and reads only that range.
Memory and SQLite resolve a containing batch from metadata. The trait default can probe existing
batch-boundary validation for custom stores; it preserves their source compatibility without reading
an entire remaining suffix.

## Web flow and controls

```text
attach -> directory -> main window -> owner state at window head -> show -> live
           |
           +-> pending questions on other lines (no transcripts)

select a line -> its window + owner state + selected-line detail -> show -> live
load earlier  -> older window at original head -> prepend display rows
reconnect     -> fresh windows for main and previously opened lines
```

The frontend requires window support and the pending-input, pending-question and current-turn reads.
Read failure leaves the affected conversation retryable; unsupported servers do not trigger full-history
fallback. The current model comes from attach, Goal from its owner, and absolute output character counts
from the current-turn read. A missing start in a window does not establish that work is finished.
Missing tool origins are resolved by request/call identity, including results arriving live. Events
remain ordered while an origin read is pending. Loading older display rows does not replay old controls
into the current owner snapshots.

## Acceptance and limits

Use disposable stores. Run property parity across ancestor cuts, merged output and publishing tails;
record plans and VM instructions for both the first page and **all pages cumulatively**. Scale candidate
sets by at least 10×. Exercise the combined input/model/tool relation shape as well as a simple input
join, hits and misses. A small returned page or zero `FULLSCAN_STEP` is not evidence of a bounded scan.

The full-pagination benchmark intentionally consumes every candidate. Reducing its VM instructions
means less repeated SQL work; it does not mean fewer candidates or fewer body verifications in the
business flow. Recall may still consume all candidates when exact rendering finds too few matches.
Short/non-indexable words and text produced only by rendering require conservative candidate retention.
Rendered search output is not inserted into FTS. Source-field relations without a matching identity
index can still scan the relevant event kind; their cost must not be described as an indexed point read.

Public flows cover whole atomic pages, hidden events, explicit older loads, lazy branch transcripts,
window-external pending controls, output offsets, missing tool origins, title selection and branch
activity rendering. A range-refusing store pins that BranchTool uses property reads; large tool results
exercise its existing displayed effects without requiring full stored-event reads.

## Structure and removal check

The existing facades still establish scope and cutoff; stores execute generic queries; consumers own
selection and display. No Reader service, query session, persistent projection or rendered-text index
is added. Web drops its historical Goal fold and per-branch input history for message counting. The
short-lived raw window is required for backwards pages to project in order; it is released afterwards.
The branch activity enum orders three field projections before rendering. SQL compiler state and
temporary content tables belong only to one query.

## 属性投影与累计执行成本

本节规定 [#1320](https://github.com/arcships/dim/issues/1320) 的既有查询执行优化，不增加查询运算或持久化格式。目标范围及排除项归[Resume 设计](resume-read-design.md)。

只需要内联字段的选中行，应在候选限量后批量投影这些字段，不先选位置再为每行构造并执行同形 SQL。存在混合字段时，将内联字段和确实依赖内容引用的字段分开：前者批量返回，后者只对所需行解析对应内容。字段是否缺失及数组投影的形状仍使用既有查询类型，不能靠填默认值合并结果。

候选位置推进和关系范围仍遵循上文。不能把分页游标加到内层 terminal 查询，也不能在存在性或分组语义不等价时提前 LIMIT。Memory、SQLite 和 publishing tail 返回相同的逻辑结果。

内联大 JSON 的多个 json_extract 仍有 SQLite 解析费用，批量投影不消除单个大行的读取。已知数组下标使用[元素直接定位](timeline-array-rows.md)，不解析未选元素。分别报告 SQLite 内存、解析字节、正文节点和 Rust 内存；不得把返回几列或分配减少等同于 CPU 降低。

统计覆盖外层选中语句、selected-properties 内部语句、关联查询、内容引用和临时表构建。对同一业务查询报告累计 statement 数、VM 指令、候选与返回行数、解析字节及排序成本。第一页面和全部页面分别测量；索引只在计划与放大测试证明必要时增加，并由存储 owner 另作格式决定。

本节不引入统计持久表或通用业务投影。索引保证身份与位置访问能力，不保证任意分组、NOT EXISTS 或数组表达式都是常数成本。

包含 TextCandidate 时，从有选择性的文本候选驱动外层关联；不能只因有关系就固定扫描 origin 再逐条检查 FTS。没有 TextCandidate 的查询不建立全文候选集合。验收增加“输入历史增长十倍、文本候选为零、附带删除反关联”的对照，同时计入候选生成和全部分页；保留删除判断、固定关系范围和未索引内容的保守候选。Recall 仍验证原渲染正文，不将渲染结果写入 FTS。

### 多类型位置分页

对少量明确类型的有序窗口，候选阶段分别使用 timeline_events_by_kind 的 session、branch、kind 和 sequence 范围。每个类型先取本页所需的候选上限，再按位置合并并限制总数；不能为满足 ORDER BY sequence 改扫整条分支。原范围仍用于关系判断，祖先切点、合并输出与 publishing tail 的语义不变。

这项优化使用现有索引，不改数据库格式。大量类型或不能安全提前限量的表达式保留通用执行路径，成本另报。验收固定匹配对象数量，将无关历史从 100 增到 5,000；覆盖正反向所有页面、First/Last、重复类型，并统计全部选择与属性语句的累计 VM 指令。
