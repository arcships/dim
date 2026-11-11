# Goal 旧事件读取与变更删除

Status: Draft.
Scope: 没有 goal/state 的旧历史，以及 Goal 命令删除后的目标定义修复。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)、[#1147](https://github.com/arcships/dim/issues/1147)。

目标状态机、数据与正常读取唯一由 [Goal 状态设计](Goal-state-read.md)规定。本文不是恢复旧执行进度的入口。

## 1. 模块与读取范围

通过已有 timeline.query 或结束分支的 treeCommands.query 读取。固定 Session、分支、本地 opening 和上界 S，不混入祖先或 fork 复制前缀的目标。Store 执行属性查询，不解析 dim-goal。

## 2. 读取对象

仅取目标定义、判定方式和最后业务状态所需的输入位置、ID、submission_id、blocks，以及引用输入的删除位置。blocks 是原有序 InputBlock，不构造缺字段 AgentEvent。

不读取用户正文、模型请求、输出、工具结果、exec、轮次历史、续行历史或评估关系来恢复执行。不装载旧 goal/recovered 及 goal/driving-node 的运行状态；未知 goal/state schema 不能冒充旧历史。

旧输入候选还必须包含 goal:state、goal:objective、goal:status 键，或 kind="goal" 业务变更。单独的 continuation、report、evaluate、evaluate-again 不进入定义重建。返回新实例前仅保留目标、业务状态、judge、有效键及其来源位置，解释旧格式时产生的执行字段全部丢弃。

## 3. 查询条件与保留的遍历

新格式 goal/state 存在时不执行旧候选读取。没有新格式时：

| 查询 | 必要结果 |
| --- | --- |
| 本地 opening | 最后 branch/start 或 session/forked 的位置和类型。 |
| Goal 候选 | 本地 input/message 的 Goal 标记候选，只取必要位置、身份和 blocks，按原语义解释完整状态、触发、旧 key 和补偿。 |
| 删除引用 | 将本页完整状态实际引用的 after 位置去重，按最多128个已知位置一批查询原输入及该 ID 的删除关联；trigger 通过原始紧邻位置配对。不读取全部普通输入的删除位置，不读正文。 |
| 原始紧邻位置 | 必要处按位置点查类型和 submission_id；候选页相邻不等于原始相邻。 |

沿用 TextCandidate 的保守 goal 候选和原属性条件，合法标记、转义和配对由 Goal 解释。每页消费完立即释放。兼容读取仍可能处理多个候选，按候选数、正文和 SQL 扫描计费，不承诺只读一个事件。

## 4. 每个状态从哪里得到

只计算 objective、state、judge。goal:state 是完整目标记录；更老的 goal:objective / goal:status 按原合同解释。被删触发、无效 after、restated 和原顺序仍决定记录是否有效。

active 映射为内存 paused，reason=interrupted_reload。不恢复 count/idle、轮次、pending 集合或评估，不按旧 after 重试输出。无目标为 none，读取失败为错误，不能相互替代。

## 5. 启动、实时与按需读取

返回目标定义和固定读头 S，交接 S 后的新事实。加载不写恢复记录，不回填 goal/state，不补答旧触发，不补发续行或评估报告。下一次真实业务变更写完整新格式状态，以后普通加载只读取它。

继承 key 清理归 Goal 新分支规则。旧状态文本不证明当前实例仍在执行。

## 6. 删除后的重算与顺序

保留 Goal 命令被 InputQueue 删除后的回滚语义。普通输入删除只更新内存，不写删除边界。

1. 按 input ID 定位受影响业务状态。新格式通过 data.input_id 定位变更，再用该位置之前的 kind=goal/state、Last、limit=1 定位前一个完整目标；旧格式通过候选和删除引用定位必要前缀。
2. 按原序重新解释前缀之后真正受影响的 Goal 业务变更，排除已删除输入及无效补偿。只修复目标、judge 和业务状态，不恢复次数、轮次、输入/任务集合、评估或旧决定。
3. 结果仍为 active 时，本次执行以 paused 结束；完整业务状态与必要模型输入原子提交。修复和新命令沿原写入串行规则，set/resume 等修复完成后才开始新执行。
4. 删除后、补偿前崩溃仍须完成真实业务修复，不能成功返回未修复状态；不能借此在普通加载中重放所有源事件。

成本按受影响 Goal 业务变更计。原用户正文、工具调用和执行控制不进入重算，不为每条普通输入保存 before。

## 7. 分页、内存与 API 缺口

沿用 TimelineQuery、Fields、Any、Prefix、Present、Exists、Last 和位置游标；无新公共 API、字段别名、Store 句柄或 Reader。128 是身份页限量，不是正文预算，正文读后即消费。

### Requirements raised to mechanism owners

无。属性、位置、ID/input_id 和文本候选遵循已有合同，业务标记和删除解释不下推到 Store。

## 8. 验收与删减检查

覆盖完整旧记录、旧双 key、retire、删除、原始紧邻配对、无目标、本地 opening 和 fork。旧 active 一律在内存暂停，不恢复次数/评估，重复加载不写事件。新格式存在后禁止旧候选读取，未知新 schema 必须报错。

### Rule check

Goal branch 是原 Capability；作用域、依赖及配置沿用 [Goal](Goal.md)。principles §4 规则 1、2：删除由 InputQueue 决定；规则 3、4：无 before I/O、无 Store 业务解释；规则 5：修复及必要输入原子提交；规则 6：上下文仍来自采用输入；规则 7、8：原命名服务，无新机制；规则 9：不保存兼容进度。ADR 002 D1–D5 的放置、无接线层、依赖方向、配置和语言中立合同不改变。

## 9. Rust 修改位置与入口替换

旧 read/restore 仅保留目标定义解释。移除执行状态及 driving 节点装载、Q3–Q7 控制状态恢复、续行/评估重放、goal/v2 完整运行快照升级。不新增泛化恢复框架，不要求其他插件修改。
