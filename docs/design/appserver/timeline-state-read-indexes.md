# Timeline：当前状态查询的索引匹配

Status: Draft.
Scope: #1320 状态设计所需的现有 query 执行与 v40 索引；不解释插件业务。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
合同：[属性查询](../timeline-property-query.md)、[native格式](native-storage-format.md)、[成员索引](timeline-member-index.md)。

## 1. 已有能力与真实缺口

| 请求 | 已有代码/索引 | 决定 |
| --- | --- | --- |
| kind+位置Last、新源后缀 | timeline_events_by_kind(session,branch,kind,sequence,event_id) | 复用；多kind分别seek，不扫大OR范围 |
| 已知input_id的applied/withdraw | timeline_events_inputs的JSON表达式索引 | 复用，SQL必须约束kind、input_id及位置；不是只用kind前缀 |
| request_id输出控制 | timeline_events_requests | 复用request表达式及位置范围；最后content后只读剩余delta |
| 当前普通输入 | members.rs的input/v1，handover缺失组 | 复用；不加mode/user等残余筛选破坏匹配 |
| 当前handover | Presence分组当前只登记缺失值；matcher也只支持缺失 | v40补存在组与Present匹配；不能从全部input反关联后筛选 |
| 指定turn的start/end | 当前只有kind位置索引，turn_id是残余JSON条件 | v40增加turn_id表达式键；没有该键时Last仍可能查完该kind |
| 指定完整task scope的started/ended | 当前无scope复合键 | v40增加scope.kind/timeline/id/status表达式键；不能按context/scope全历史分组 |
| Usage累计值在源位置前的最后结果 | 原kind索引排序的是提交位置，source_through为残余条件 | v40增加epoch+current.at.branch+current.at.sequence表达式键；Field排序直接使用数值键 |
| Prompt原changes下标 | 现有Fields可表达；大内联JSON仍解析整数组 | 保留已选v39数组locator编码，按事件ID+下标寻址 |

这些是新增索引和执行匹配，不是新增查询API。当前QuerySpec.rows分支不属于需求，不能顺带保留。成员定位固定单个分支段和through，支持from下界、发布中尾部、组合关系OR及pick First/Last。附加残余筛选只检查当前成员，不能先截断再筛选；终态复合键必须完整匹配。group/聚合不声明自动使用成员根。Exists/Count不在该快路，不能宣传自动复用成员根。

## 2. SQL范围与访问上限

类型/位置查询每个kind单独使用等值前缀+sequence上下界，倒序取1（需要续页证据时取2）。已知ID使用原身份索引及v40输出locator。命中后才投影字段；候选数不随已结束历史线性增长。位置过滤不得留给Rust。

InputQueue当前集合按member_roots主键固定through定位根，再按节点主键取得当前K项。普通组与handover组分别查，不给查询追加mode过滤。每页当前ID的applied/withdraw合并为一个按kind/input_id分组的查询；SQL对本页每个ID使用输入键seek；缺失即结束，不扫描其它input_id。模式默认的user存在性也是ID驱动；可能解析该选中内联记录，计入字节，不能称为零正文IO。

指定turn的边界SQL形状：

```sql
SELECT sequence,event_id FROM timeline_events
WHERE session_id=:s AND branch_id=:b AND kind=:kind
  AND coalesce(json_extract(body,'$.record.event.data.turn_id'),json_extract(body,'$.turn_id'))=:turn
  AND sequence<=:head
ORDER BY sequence DESC LIMIT 1;
```

scope边界SQL形状（started、ended各一次）：

```sql
SELECT sequence,event_id FROM timeline_events
WHERE session_id=:s AND branch_id=:b AND kind='context/scope'
  AND json_extract(body,'$.record.event.data.scope.kind')=:scope_kind
  AND json_extract(body,'$.record.event.data.scope.timeline')=:origin
  AND json_extract(body,'$.record.event.data.scope.id')=:scope_id
  AND json_extract(body,'$.record.event.data.status')=:status
  AND sequence<=:head
ORDER BY sequence DESC LIMIT 1;
```

完整scope键不能只用id，避免不同来源分支/种类冲突。再次started不抹掉上次ended；两种status分别seek，Prompt比较它们与Set的位置。branch/start重置另用原kind位置索引，Store不判断条目有效性。

Usage前驱SQL形状：

```sql
SELECT sequence,event_id FROM timeline_events
WHERE session_id=:s AND branch_id=:b AND kind='context-usage/recovered'
  AND json_extract(body,'$.record.event.data.epoch')=:epoch
  AND json_extract(body,'$.record.event.data.current.at.branch')=:branch_ordinal
  AND json_extract(body,'$.record.event.data.current.at.sequence')<:cut
  AND sequence<=:visible_head
ORDER BY json_extract(body,'$.record.event.data.current.at.sequence') DESC,
         sequence DESC
LIMIT 1;
```

native索引有源位置范围而非提交位置范围。正常线性分支的owner按源有序保存，所有结果可见时直接返回1个候选；固定旧visible_head可能排除源位置合格但更晚提交的结果，候选额外数必须报告，不能声称任意历史切点都只访问1条。fork只允许使用cut内可见结果；不存在时按明确首次初始化处理，不扫描正文伪造结果。

QuerySpec使用既有Field排序(`data.current.at.sequence`降序、Position降序)、In/Compare和Fields，不使用pick=Last代替源字段排序。查询执行器识别这组等值键/范围/排序，先选择匹配索引再投影body；不能先物化全部该kind再排序。带不支持残余条件时不能声称这个成本上限。

## 3. 格式与提交

v40在同一目标格式中加入以上三个表达式索引和handover存在组；不把它们写入v39，也不改已提交格式编号。精确DDL只在[native格式](native-storage-format.md#v40-状态字段定位补充)定义。与原已确认输出locator/恢复成员同事务升级，失败回滚。Store不运行Prompt或Usage代码，只索引已声明的公开JSON标量。

Memory支持同一查询语义；SQLite成本验收不能由Memory返回相同结果替代。复制、导入、删除、完整性检查与格式schema验证纳入这些索引，不在普通query代建。存在组沿用同一input反关联与根版本，不另建业务恢复表。

## 4. 验收证据

查询计划必须显示完整等值前缀以及末尾位置范围；`USING INDEX`而没有input_id/scope/epoch表达式键不算通过。索引内的前驱定位不排序；为确认分页，每段先取至多limit+1候选，跨段/最终结果允许对这份有界候选排序。不能把有界排序说成完整结果零排序。统计全部辅助statement、VM_STEP、FULLSCAN_STEP、SORT、AUTOINDEX、成员路径、候选个数、选中wire/内容解码字节。

先以普通聊天一轮、Goal续行一轮、一次scope结束/重启、同值配置、空输出失败和已结算重载验证实际读写清单；再增加已结束轮次做索引独立性验证。不能只比较返回行数，不能用一个LIMIT1断言所有关系查询已经有效。

删减：不新增通用属性索引注册、任意JSON字段索引表、owner状态表或读缓存；只补实际消费端用到的三个键和一个已有分组值。若实测查询无法使用上述键，修正执行器，不退回owner历史遍历。
