# Timeline：按已知下标读取数组元素

Status: Draft.
Scope: 现有属性查询的下标点读和原生执行。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。
合同：[属性查询](timeline-property-query.md)。消费者：[Prompt 恢复](prompt-state-read.md)。

## 1. 决定

不增加 QuerySpec.rows、QueryRows、元素行、数组聚合或新的读取方法。Prompt 已保存事件 ID 和原始下标，直接使用现有 Fields 的数字下标路径。原 QuerySpec 的 Rust 构造、JSON、返回类型和事件游标保持不变。

需要修改的是原生执行：先定位事件，再定位选中数组元素，最后解码所选内容。返回一项不能以解析整个数组为代价。

## 2. 请求形状

按事件 ID 分组，每组最多 128 个引用；同一事件的多个下标在一个 Fields 请求中读取。例如：

~~~text
A 的下标 2、7：
  scope  = 原业务可见范围，through = 固定恢复截止
  filter = kind = prompt/changed AND id = A
  select = Fields [id, position,
                   data.changes[2].entry, data.changes[7].entry]
  pick   = null
  group_by / order_by = []
  page   = limit 1，无续页

B 的下标 7：
  相同范围，filter 中 id = B
  select = Fields [id, position, data.changes[7].entry]
~~~

调用方按请求中的字段顺序取得 QueryValue，直接在持久化边界解码 PromptEntry。不同事件不共享下标集合，不请求 A/7 以外未引用元素。每个恢复批次有 K 个引用、分布于 G 个事件时，G <= K <= 128，执行 G 次逻辑 query，不把它写成一次 SQL。

## 3. 原执行合同

沿用字段下标语义：事件不可见或不存在则没有行；可见数组的下标越界返回 Missing；数组内 null 保持 null。Prompt 持有的有效 Set 引用缺失、不是 Set 或 entry 损坏时，恢复报错，不能静默丢掉提示。

Memory 使用现有数字下标投影。SQLite 对外置 changes 先读事件的数组引用，再以 (session_id, array_id, element_index) 定位元素内容；不展开其它下标。原始内联旧编码在格式升级时转换，正常新格式读取不临时迁移。具体编码归[原生格式](appserver/native-storage-format.md#提案状态读取索引与数组元素)。

完整事件读取仍还原全部 changes；Any、Count、Exists 和原事件分组保持原合同。本工作不把它们推广成独立数组元素的计算语言。

## 4. 修改和验收

| 对象 | 修改 |
| --- | --- |
| Prompt 恢复 | 将已存引用按事件分组，提交已存在的 Fields 下标路径。 |
| storage/query、codec | 下标投影直接加载所选元素；同一次 query 内复用同一事件 wire。 |
| 数组 locator 与升级 | 保留直接寻址、内容验证、复制/删除和导出；不附带通用展开器。 |
| runtime 查询类型、普通调用方、SDK | 不增加字段或类型，不批量补 rows=None。 |

验收复用 Prompt 恢复 flow：固定两个有效引用，将原 changes 从 128 扩至 2,048 项。返回与旧投影相同，未选元素内容节点加载为零；被选元素损坏失败，未选元素损坏不被本次读取加载。完整导出仍发现损坏。覆盖祖先截止、同事件多下标、缺失、越界和同名键。

删减：移除通用 QueryRows 及其筛选/关联/分组/排序/游标分支；保留现有字段接口和真实需要的元素定位。无需为没有生产消费者的数组分析组合新增测试。
