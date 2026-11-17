# 固定查询范围与分页的有限模型

Refs #1732，第二批 B；独立基于 main `c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`。
第一批写入/发布为 PR #1734，本批不依赖其分支，不把那个 PR 的模型前提视为已经由实现证明。
本页是实验说明，不修改查询合同或插件开发规范。

## 对应的真实问题

[查询合同 §2–3](../timeline-property-query.md)要求后续页继续使用同一个逻辑范围，
但允许磁盘与 publishing tail 的物理划分变化。`after` 限制输出，`from` 限制整个范围，
包括关联两端。把二者混用，可能让上一页之外的终态丢失，或让分组结果随页变化。

[QueryRange](capability-models/QueryRange.tla)不直接把参照答案赋给页面：
`Capture/NextPage` 捕获存储分界和尾部，`Read` 从祖先及当前物理存储范围拼接输入，
`Scan` 逐条累加匹配身份、分组计数和候选，`Page` 最后排序、按游标取页。
另一个声明式定义 `Reference` 直接在固定逻辑历史上做集合选择、极值和基数计算。
`ScanAgrees` 和 `CompleteResult` 比较两条路径；first/last 的行同时携带 group 与选中位置，
因此把正确位置挂到错误分组也不能通过比较。

| 现行合同 | 实现入口 | 本批模型/命题 | 边界与后续 |
| --- | --- | --- | --- |
| branch/chain、祖先 fork cut、through/from | runtime `core/timeline.rs::query` | `Base/Domain` 定义参照；Capture、Read 分别构造边界/输入；`SourcePartition` | 一个祖先加当前分支，fork cut 取 1/2；任意深链、目录构造正确性未证明 |
| 普通查询仅 published；内部可包含 allocated 尾部 | 同一 query 的 visible、stored_through 和 tail | `Capture(internal)` 校验 cut；尾部引用单独保存；`SourcePartition` | 输入是已提交的逻辑事件；原子批次形成归 A，不重复模拟 |
| 尾部落盘不重复，后续页可重新划分 | 同上；memory `Source::records`；SQLite `Compiler::scope` 与 tail 输入 | `Publish` 可在捕获、Read、Scan、两页之间执行；磁盘由当前发布位置与捕获 bound 双重限制；`SourcePartition` | 不含存储损坏、故障后热窗口丢弃及重启；存储错误明确结束读取 |
| 后续追加不能改变固定 cut | query 的 through，store `TimelineReadScope::query_binding` | `AppendSuffix` 追加一个潜在关联终态；`FixedRange/PagesArePrefix` | 固定历史不可修改是存储合同前提；不证明 SQLite immutable rows |
| related 使用完整范围，after 不缩小关联搜索 | memory `Evaluation::prepare/matches` 中 `records(None)`；SQLite relation scope | `Scan` 对每个 start 在整个 input 检查匹配 end，Page 才应用 after；`ScanAgrees/CompleteResult` | 只建模按身份排除关联终态，不包含任意嵌套 outer/root、数组条件和全部过滤表达式 |
| pick first/last 按事件顺序，不被结果升降序改变 | memory query 分组/选取循环和 `push`；SQLite 分组/排序生成 | Scan 按位置累积 first/last；Reference 独立取每组极值；Page 最后排序；`ScanAgrees` | 单个身份分组键；输出按位置升降序。多字段排序/类型比较留实现对照 |
| 分组计数先算完再分页 | 同上 groups/count 与最终 after/limit | Scan 累加计数；Reference 用 Cardinality；分页按组键；`CompleteResult` | 不包括按计数排序、多键分组、数值表示等价规则 |
| next 绑定逻辑范围和查询，不绑定物理落盘分界 | `timeline_store.rs::query_binding`、memory cursor 检查、SQLite `after` | token 保存 chain/cut/from/Kind；NextPage 比较有效 token、修改 cut、修改 query；错误不重启查询 | 仅 chain/cut/from/Kind；实际 binding 还含 Session、branch、intervals、filter、group、select 等身份；不证明这些字段完整绑定、JSON 编码或字段类型校验 |
| 未来范围、无效 from、存储错误不是空结果 | Timeline query 校验及 store Result 传播 | Capture 返回 invalidRange；StorageError/NextPage 返回独立 error 阶段；`ErrorIsNotEmptySuccess` | 这里只检查模型保持错误类别，不能当作 Rust 所有错误传播路径的证明 |

实现源文件：
[Timeline query](../../../crates/dim-agent-runtime/src/core/timeline.rs)、
[范围与 binding](../../../crates/dim-agent-runtime/src/timeline_store.rs)、
[内存求值](../../../crates/dim-agent-runtime/src/memory_timeline_store/query.rs)、
[SQLite 求值](../../../crates/dim-app-server/src/storage/query.rs)。

## 有限范围与保留的交错

每个配置有 1,024 个初态：前四个位置的 `(start/end, id=1/2)` 全组合，
祖先 fork cut 为 1/2、结果升降序两种。前两个位置在祖先，后两个在当前分支。
当前分支初始 allocated=2、published=1；可追加第三个位置，其内容为 id=1 的 end，
因此晚到后缀可能改变关联判断，不能把它当成无关事件。

祖先位置 2 在 fork=1 时不可见。查询可选 branch 或 chain、内外部可见性、through=0..4，
以及默认或显式 from（含祖先/当前分支/范围外）。模型的 0 是“未提供 from”的哨兵，
不是 wire 中的显式 sequence=0；其拒绝属于参数校验，未在此模型展开。位置 3/4/5 对应当前分支 sequence 1/2/3。未来 cut、越界 from 作为明确错误返回。
每页重新捕获磁盘/尾部分界，但不能改变逻辑 chain/cut/from；扫描间允许发布继续推进。
分页可能是空、单页、多页，也可能在后续页发生错误；错误保留先前已返回的前缀，不伪装成成功结束。

四种查询分别为未结算 start、按身份分组 first、last、count；各用一行页。
第五配置用两行页检查截取/剩余行判断。不存在限定“只测试好历史”的状态过滤：
允许 end 在 start 前、相同身份有多条 start/end，查询只是解释给定范围，不暗加业务身份合法性约束。

这里没有承诺永不挂起：Spec 没有公平调度假设，只检查安全性和成功完成时的完整结果。
`CHECK_DEADLOCK FALSE` 允许环境不再提供进展。TLC 穷尽配置内可达状态不等于实际数据库读取必定完成。

## 现有实现证据及未覆盖内容

复用 [property_queries.rs](../../../crates/dim-app-server/tests/storage_flow/property_queries.rs)：

- `property_queries_match_memory_for_grouping_relations_arrays_and_keyset_pages`：SQL/memory 分组、关联、数组及 keyset 对照。
- `property_queries_clip_merged_records_and_keep_the_same_tail_after_commit`：物理合并行和 tail 落盘后的读取。
- `input_identity_relations_keep_the_full_ancestor_range_across_pages_and_tails`：真实祖先、分页及尾部关联。
- [timeline_tail](../../../tests/integration/tests/timeline_tail.rs)：经 Timeline 入口跨页读取同 cut 的 publishing facts。

后续补充在同一 storage flow 中增加
`query_cursors_reject_changed_logical_scope_and_queries_in_both_stores`，对 SQLite 和 Memory 分别取真实游标：

- Session、分支及 branch-only 范围、当前 cut、祖先 interval、inclusive from 改变时拒绝旧游标。
- filter、select、group_by、pick、order_by 改变时拒绝旧游标。
- 每个变体先独立查询成功，再携旧游标确认是 binding 的 InvalidInput，不把非法表达式或不存在的目录当作游标校验。
- 页大小从 1 改为 2 时仍可继续且返回两行；仅物理落盘分界改变的正例复用并重跑上述 tail-after-commit 流程。

这直接验证存储接口的逻辑绑定，不证明 Timeline 门面的授权、全部参数组合或任意 SQL 表达式。
分支用例同时改变 branch key 和合法分支范围，不声称单独隔离了二者的实现贡献。
本轮执行 property_queries 模块；其他文件里的引用测试没有因此算作重跑。
每个物理来源按位置唯一、记录内容不变是存储前提；模型检查磁盘/尾部集合不相交，
也不检查 tail 向量顺序，不证明 SQL 解码不会在单一来源内重复产生同一行。续页调用者的权限/可见性重新校验也未展开。正文编码、missing/null、数值/数组语义、
任意字段排序、任意深祖先链、取消/故障恢复、完整游标身份校验和 SQL 性能没有由本模型证明。
#1732 的 B 因此只推进表中子集；完整表达式和边界审计不能因这五个配置通过就勾为全部完成。

## 消融与证据

保留一个共享模型和查询类型/页大小配置，没有为每个消费者复制查询。
去掉生产者任务、业务状态机和 SQL 执行计划；保留逐行 scan 与独立 Reference，避免直接赋正确答案。
物理分界、捕获 tail、after 和完整范围不可合并，它们分别承担落盘交错、输出分页和关联搜索的区别。
count/first/last 共用扫描循环，其他查询表达式不添作未经检查的伪接口。

本批结果、配置/源码哈希及复现命令见[运行记录](capability-models/query-range-results.md)。

补充流程的消融审查：只增加一个表驱动的存储公共接口流程，复用现有建库、事件和分页设施，不增加游标封装或模型状态。模型与配置未改，原五配置 TLC 结果复用。
