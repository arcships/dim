# Timeline：身份查询与恢复操作查询

Scope: 原生 Store 的查询执行。关联问题：[#1320](https://github.com/arcships/dim/issues/1320)。

查询语义沿用[属性查询](../timeline-property-query.md)，模块责任沿用 [Resume §2](../resume-read-design.md#2-最终-api-与模块关系)。本设计不增加公开读取方法，采用已确认的 [v40 格式](native-storage-format.md#提案v40-补齐逻辑事件身份与恢复成员)。

## 1. 调用接口

```text
业务 owner：选择 ID、字段、源位置或未完成条件
  -> Timeline.query：固定 branch/chain、through 和可见尾部
     -> Store.query：执行索引定位与属性投影

结束分支 -> TreeCommands.query -> Store.query
```

插件不读写数据库。Store 不计算业务状态。既有 session 方法仍由各业务 owner 提供。

| 需求 | query 参数 | 执行目标 |
| --- | --- | --- |
| 已知事件或状态节点 | `id IN (...)`，Fields 为本次必要字段 | 按身份定位；命中和未命中都不扫描历史。无需调用方补 kind。 |
| 恢复结果 | 结果 kind，Last，Fields(schema, source_through, state) | 按类型和位置倒序取最后一条，再检查 schema。 |
| 未处理输入 | 源 kind，from 为 source_through 之后，through 固定 | 按类型及位置读后缀；身份页最多 128 项。 |
| 未完成操作 | 原开始条件及 NOT EXISTS 对应终态 | 定位同一 through 的成员根，再取当前成员。 |

结果、缺失值、分页、祖先切点和 publishing tail 仍遵循原 query 合同。点查只返回可见范围内的身份；一个 ID 存在于范围之外仍是未命中。

## 2. 已知 ID 的定位

普通物理行已有 `timeline_events_by_id(session_id,event_id)`。合并输出的一行包含多个逻辑事件，不能用这个索引定位内部 ID。

新增输出身份 locator 只登记 `step/text`、`step/thinking`，不复制普通事件索引。其字段为 Session、事件 ID、分支、物理行起点和逻辑序号；具体 schema 归原生格式。

```text
一批 ID
  -> 普通事件身份索引 + 输出身份 locator
  -> 核对分支、祖先切点与逻辑序号
  -> 合并相同物理位置
  -> 读取这些物理行
  -> 原逻辑展开、剩余过滤与所选字段
```

SQL 的候选位置由两条索引查询合并，不写成“ID 匹配 OR 所有输出类型”。混合 ID、重复 ID 和同一合并行内多个 ID 均只读取一次物理行。命中的合并行可能仍需解析整行来取得内部片段；定位成本不随其它历史增长，单个合并行的解析字节另计。

只对能证明必须满足的 ID 等值/IN 条件采用此路径；不从一个 OR 分支提取 ID 后丢掉其它分支。关系查询需要的其它角色保留各自候选范围，不能只保留外层 ID。

locator 与输出事实在同一事务保存。追加片段只新增该片段的身份，指向原物理起点。fork、复制及 export/import 的裁剪使用逻辑序号，不能复制切点之后的身份。缺失或错误 locator 是损坏错误，不能改成查询未命中或静默扫历史。

## 3. 恢复操作的成员集合

沿用[成员索引](timeline-member-index.md)的版本根、不可变树、反向定位和终态记录。补充下列独立描述，不创建业务状态表：

| 描述 | 开始对象及键 | 关闭它的事实及键 | 消费者 |
| --- | --- | --- | --- |
| prepare/v1 | context/prepare 的 (turn_id, request_id) | model/call 或 step/model-output-end 的同一对键 | ContextPreparation.rebuild |
| model-call/v1 | model/call 的 (turn_id, request_id) | step/model-output-end 的同一对键 | ModelProvider.rebuild |
| model-start/v1 | step/model-start 的 request_id | step/model-output-end 的 request_id | session.turn.get |
| compaction-attempt/v1 | context/compaction-start 的 position.sequence | context/compaction 的 data.start | 自动与手动压缩恢复 |
| compaction-command/v1 | context/compact 的 position.sequence | context/compaction-start 或 context/compaction 的 data.command | 手动压缩恢复 |

这里的字段均指 `data.*`，仅 position.sequence 例外。双键匹配不能简化为 request_id。model start 的原查询只按 request_id 关联，不能自行加强为双键。自动压缩的 command 缺失条件、当前 turn 筛选等原条件继续作用于当前成员，不在索引里改变业务含义。

组合键按每个 QueryValue 的标量相等编码加长度边界组成；字段顺序由描述固定，查询中的 AND 条件顺序不影响匹配。缺字段不能被填成 null。既有四种描述和键编码不变。

多个未完成条件的 OR，各分支先定位自己的成员；合并、去重并按原位置分页。只能在各分支剩余条件已求值后限制结果数。若某分支不匹配受支持描述，保留该分支原语义并单列成本，不能忽略它。正常恢复查询必须有直接覆盖实际请求形状的测试，不能只测一条简化示例。

现有成员路径的适用范围沿用原合同：从分支起点开始、单分支段、无 publishing tail。源后缀查询不套成员根。为覆盖上述内部恢复，新增执行必须把捕获尾部作用于已落盘截止的根，再查询临时集合；临时节点只属于本次查询。尾部的新增开始与终态都按逻辑顺序处理，旧根不修改，也不因出现尾部而退回历史反关联。多段 chain 或裁剪 from 仍不是本设计承诺的成员路径。

## 4. 通用类型候选

未被索引直接解决的关联仍可能需要多个角色。若每个角色都有必要的 kind 等值集合，编译器先以它们的并集限定物理类型，再保留原完整谓词：

```sql
WHERE e.kind IN (:all_required_kinds)
  AND (:original_outer_or_related_predicates)
```

一个角色没有可证明的 kind 限制时，不增加此限制。内层关联保留原固定范围，外层续页位置不裁掉内层终态。此项复用现有类型索引，不改格式，也不把仍需遍历相关 origin 的查询称为成员查询。

## 5. 修改落点与验收

| 修改位置 | 负责的改动 |
| --- | --- |
| storage/query.rs | 提取必要身份；合并两种 locator；类型并集；保留关系范围和逻辑过滤。 |
| storage/members.rs | 新静态描述、组合键及严格形状识别；复用原成员树。 |
| storage/timeline.rs、复制与删除路径 | 在原事务更新输出 locator 与新成员。 |
| storage/sqlite.rs、校验和升级 | 按 v40 准入、回填、完整性检查和失败回滚，不改 v39 解释。 |
| 业务 owner | 原查询条件和字段；不增加 Store 依赖或索引参数。 |

用可丢弃数据库固定命中对象、当前成员及单条大小，将历史从 100 增至 5,000，并补充 6,000/60,000 的验收。分别测 ID 命中、未命中、混合普通/输出 ID、固定旧 through、分叉切点、正反向所有页面，以及真实恢复查询。累计选择、属性投影、关系和内部补查的全部 SQL VM、扫描、排序、自动索引、正文加载字节及 SQLite 内存。

已结束历史增加时，ID 查询不能逐条检查旧行；零成员查询不能物化全部开始或终态。单个大合并行、真实当前成员和捕获尾部的成本分别报告。格式测试覆盖升级失败回滚、终态先出现、重复终态、批次内部 through、fork/复制/删除，以及漏项、多项和错误 locator。写入另计索引行、成员路径和 WAL。

删减：不增加 Reader、ID 缓存、插件数据库接口、业务 SQL 方法或第二套成员树。输出 locator 只补既有身份索引无法覆盖的逻辑片段；保留它是为了同时支持命中与未命中的有界查询。实际模型请求仍按其所需内容构造，不能用此设计声称请求正文处理是常数成本。
