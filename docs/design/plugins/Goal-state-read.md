# Goal：业务状态事件与重载暂停

Status: Draft.
Scope: Goal 的持久状态、运行期内存、异常终止、重载和 session.goal.get。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。

依据：[Goal](Goal.md)、[旧事件读取](Goal-property-queries.md)、[设计规则](../../architecture/rust-agent/principles.md)、[ADR 002](../../adr/002-plugin-scope.md)、[插件合同](../plugin-contract.md)、[rutis](../rutis-usage.md)。本文件规定状态保存与读取合同。

## 1. 决定

1. 运行中的次数、轮次、输入和任务集合、评估关系、待处理报告、决定去重及写任务只存在于本次 Goal 实例的内存中。普通输入、input/consumed、turn/start、turn/end 和 exec 事件不因推进消费位置而写恢复记录。
2. 设置、编辑、暂停、恢复、清除、完成、阻塞写完整 Goal 业务状态。异常终止正在运行的 Goal 时，先停止自己的执行、取消任务并释放资源，再提交 paused。
3. 新 Goal 实例读取最后一个业务状态。若仍为 active，内存状态设为 paused，reason=interrupted_reload；不补写 paused，不恢复旧执行，不重放旧决定。
4. 只有新的 set 或 resume 开始一次执行。旧 turn/end、任务结束、评估报告、重连和查询都不能使重载后暂停的 Goal 重新执行。

不使用 goal/recovered、goal/input-boundary、goal/driving-node，不保存 source_through 或当前集合来恢复 Goal 执行。Goal 按[业务保存边界](../owner-state-indexed-reads.md)保存目标状态；它恢复目标定义，终止旧执行。

## 2. 保存对象

### 2.1 完整业务状态

Goal branch 通过自己的 timeline 写私有业务事件 type=goal、name=state。每条都独立表达当时的完整目标：

| data 字段 | 合同 |
| --- | --- |
| schema | goal-state/v1；不复用旧 goal/v1、goal/v2 恢复布局。 |
| operation | set、edit、pause、resume、clear、completed、blocked。start 对应现有 set，不新增公开方法。 |
| objective | 完整目标文本；没有目标为 null。沿用 Goal 长度限制。 |
| state | 原 Goal 状态对象：kind 为 active、paused、completed、blocked、none；paused 的 text 为原因，completed/blocked 的 text 为说明。 |
| judge | model 或 evaluator。 |
| input_id | 本次命令 trigger 的输入 ID；自动终止等没有命令 trigger 时为 null。用于定位被删除的 Goal 变更。 |

信封已有 ID、分支、sequence 和时间，data 不重复保存。clear 写 objective=null、state.kind=none、judge=model，不能靠删除记录表达清除。

~~~json
{
  "type": "goal",
  "name": "state",
  "data": {
    "schema": "goal-state/v1",
    "operation": "pause",
    "objective": "完成数据库迁移",
    "state": {"kind": "paused", "text": "turn_failed"},
    "judge": "model",
    "input_id": null
  }
}
~~~

不保存 previous 链。删除修复需要前一个业务状态时，使用同一分支的 kind/位置索引，在受影响状态位置之前取 Last、limit=1。旧历史没有 goal/state 时按[兼容读取](Goal-property-queries.md)取得目标；下一次真实业务变更才写新格式，加载不回填。

### 2.2 只在内存中的对象

| 对象 | 用途与释放 |
| --- | --- |
| continuation_count、continuation_idle | 本次执行的连续续行、无进展次数。set/resume 从零开始；操作员输入在内存归零；实例终止后丢弃。 |
| 当前轮次及 continuation_driven | 判断当前轮次、无进展规则；不恢复旧轮次。 |
| 当前待消费输入、未结束任务的 ID | 实时判断等待，消费/结束后移除。显式 set/resume 固定当前已提交位置，用已有成员查询重新取得当前未 consumed/deleted 的 queue/steer 输入ID，以及未 ended 的 task_id；不读正文，不加载旧执行。每类按128项分页，无对象时共2次query、零额外事件。普通输入 mode 缺失时按原 user 是否存在判断；handover/Append 不作为驱动。查询期间有新提交则在新固定位置重取，完成交接后由实时事件更新集合。 |
| 当前评估、首次有效报告、延后结论 | 属于创建评估的这次执行；新实例不重新加载、创建或补发旧评估工作。 |
| 已处理决定、在途写入及取消资源 | 本实例去重和关闭；不以进程内任务身份恢复旧执行。 |

不把这些对象换个字段名再写入 goal/state。普通查询也不序列化它们。

## 3. 写入时机和原子性

| 场景 | Goal 业务状态事件 |
| --- | --- |
| set / resume | 一条 active；提交成功并按序消费后进入 running，建立新的内存执行状态。 |
| edit | 一条新目标的完整状态，保留当前有效状态。重载后 edit 仍为 paused，不能采用旧持久 active 恢复运行。 |
| 用户 pause / clear | 一条 paused / none，结束本次自动推进和评估工作。 |
| 完成 / 阻塞 | 一条 completed / blocked。评估模式下工作分支的完成申请只更新内存请求，不写假的完成状态。 |
| 中断、失败、取消、评估失败、限额、无进展、可控关闭或卸载 | 正在运行的 Goal 结束执行，写一条 paused 及原因；重复终止不重复写同一终态。 |
| 普通输入、计数归零、轮次变化、任务变化、评估创建或等待 | 零条。原事实仍由各自所有者正常写入。 |
| 一次自动续行 | 零条状态事件；只写原有 continuation 输入。count/idle 可保留在必需输入的诊断文本中，但不用于重建执行。 |
| 查询、重连、无目标聊天、重载时 active 映射为 paused | 零条。 |

同一次业务变更的模型状态输入、命令 trigger、goal/state 由 Goal branch 用已有 append_batch 原子提交。原状态输入仍紧邻并位于 trigger 前；结构化 goal/state 放在它们之后。没有 trigger 的暂停/完成将模型状态输入与 goal/state 同批提交。

输入由 Goal 的参数边界校验，submission ID、输入事件 ID 由生产者一次确定；不新增 TreeCommands 写接口，不修改 Branch::inputs。continuation 沿用原输入入口。公开方法仍返回 trigger 的 sequence。提交失败不报告变更成功；无法落盘 paused 时，下一次加载仍按 §4 暂停。

普通事件仅推进内存消费位置。getter 就绪与持久状态事件无关；不得为发布 getter 进度、关闭时刷 source_through 或确认归零而追加事件。

## 4. get 与冷恢复

### 4.1 加载规则

固定分支读头 S 和本地 opening。使用既有 TimelineQuery 的 Branch、kind=goal/state、Last、limit=1 读取完整 data。以下 SQL 只表示已固定分支范围后的逻辑查询：

~~~sql
SELECT id, position, data
FROM events
WHERE kind = 'goal/state'
ORDER BY position DESC
LIMIT 1;
~~~

Store 使用现有类型/位置索引；新格式不从 input/message 文本筛选。取得最新记录后校验 schema；未知或损坏记录报错，不能过滤后冒用旧记录。

| 最后业务状态 | 新实例内存状态 | 自动工作 / 新状态事件 |
| --- | --- | --- |
| active，包括 set/resume 后未结束 | paused，reason=interrupted_reload，保留目标与 judge。 | 不续行，不恢复次数或评估，不写事件。 |
| paused | 保留状态和原因。 | 不自动工作，不写事件。 |
| completed / blocked | 保留状态和说明。 | 不自动工作，不写事件。 |
| none / clear | 没有目标。 | 不自动工作，不写事件。 |
| 没有新格式状态 | 兼容读取目标，再应用相同映射；确实没有目标则为 none。 | 不回填，兼容成本单列。 |

返回 S 交接 Dispatcher；S 以前的轮次、续行、任务、评估、决定不重新交付给 Goal 执行。S 以后仍只有新的 set/resume 能建立执行，其他晚到事实不能使 paused 自动运行。

客户端断线、重新 attach、普通 get 不创建新 Goal 实例，不执行暂停映射。Session 恢复、Goal branch 部件重新加载才创建新实例。子分支和 fork 的本地状态不继承父分支或复制前缀中的目标。

### 4.2 当前状态与持久状态

活 get 等入口固定位置已交付后返回内存目标和状态；零 SQL、零写入，不等待新的持久状态事件。不能再读旧 active 事件覆盖内存 paused。

无活 owner 的结束分支通过原 TreeCommands.query 读取业务状态并应用结束/重载映射。保持原 GoalRead；through 是本次读/交付覆盖的位置，不是 source_through。历史页表示当时的持久状态，不能代替当前状态 API。

新格式普通加载只返回一个 Goal 状态对象，不装载集合节点、不查控制源后缀。opening 定位、旧数据兼容、真实未结算删除修复和其他所有者的资源结算单独计量，不能冒充这一条对象读取。

## 5. 终止、重载和模型上下文

~~~mermaid
sequenceDiagram
    actor U as 用户
    participant G as Goal
    participant T as Timeline
    U->>G: set / resume
    G->>T: 原子提交开始状态及必要输入
    T-->>G: 成功，按序派发
    G->>G: running，初始化本次执行
    loop 运行期间
        G->>G: 更新次数、轮次、评估等待状态
        Note right of G: 只改内存，续行只写原有驱动输入
    end
    alt 正常完成或确认阻塞
        G->>T: 提交 completed / blocked
    else 可处理的异常或可控卸载
        G->>G: 禁止新决定，取消任务并释放资源
        G->>T: 提交 paused 及必要模型状态输入
    else 崩溃，无法写入
        Note over G,T: 最后持久状态仍可能为 active
    end
    Note over G,T: 创建新的 Goal 实例
    G->>T: 索引读取最后一条 Goal 状态
    T-->>G: 完整目标和最后业务状态
    G->>G: active 映射为内存 paused，其他状态保留
    Note right of G: 不恢复旧工作，不追加恢复记录
~~~

异常处理在本部件任务中完成：先禁止新自动决定，取消并结算自己发起的工作，再完成 paused 提交，最后移除服务。Timeline 不可写时保留原错误，不宣称 paused 已落盘。

已提交输入、已交接工作仍按 InputQueue、Loop 和资源所有者的关闭规则处理；Goal 不直接删除或回收。paused 阻止新 Goal 自动决定，不承诺撤销已开始的模型/工具调用。

goal/state 不直接进入模型上下文，模型状态仍使用已采用输入。历史 active 块记录当时的开始，不证明本实例仍在执行。Goal 的 guide 和 active 文本明确：只有当前输入中的新 set/resume 或 continuation 指令驱动目标执行，不能因历史 active 块自行续行。不通过 ContextPreparation 注入内存状态，不在加载时补写模型状态输入。

### 5.1 输入删除与旧评估

普通输入删除只改变本实例的输入集合。涉及 Goal 命令时保留原回滚合同：input_id 定位业务状态，按该状态之前的位置索引取得前一个完整目标；只处理其后真正受影响的 Goal 业务变更，排除被删命令。旧输入/补偿解释按[兼容读取](Goal-property-queries.md)。

修复仅恢复目标定义和业务状态，不恢复次数、轮次、任务、评估或旧自动决定。若结果仍为 active，则结束本次执行，以 paused 写完整业务结果。修复期间 get 不成功返回旧状态，新的 set/resume 等待修复完成。删除后、补偿前崩溃仍须完成这项真实业务修复；这条路径不承诺单事件读取，成本按受影响 Goal 变更计，不作为普通加载的执行重放。加载先用类型/位置索引判断最新业务状态之后、固定上界 S 之前是否有 input/deleted；空范围直接结束，不关联旧 Goal 记录。有删除才查询最早受影响的 Goal 业务状态：其 data.input_id 必须存在该范围内的删除。关联使用已有 input_id 索引；不将所有普通聊天的删除 ID 分页读入 Goal。只在实际命中 Goal 定义后开始修复。查询扫描范围和修复成本分别验证，不以 LIMIT1 或新增每输入进度日志掩盖它们。

不为普通输入写删除边界，不新增前驱链；既有类型/位置索引已经能定位相邻业务状态。

Pursuit 的请求、评估关系和报告配对只属于发起它的执行。终止时取消并结算仍拥有的评估工作；新实例评估表为空，旧报告不能完成、恢复或续行当前 Goal。旧评估资源由原生命周期所有者结算，不读取旧回复补造结论。

## 6. 成本和修改落点

以下只计 Goal 状态事件，原输入、工具结果、分支生命周期和资源结算事实另计：

| 流程 | 状态写入 |
| --- | --- |
| set 后任意轮，再完成 | 两条：active、completed。 |
| set 后可处理的异常终止 | 两条：active、paused。 |
| set 后直接崩溃，再加载 | 一条 active；加载零条，内存 paused。 |
| 用户再次 resume | 增加一条 active，次数从零开始。 |
| 普通输入、轮次、工具调用、评估等待 | 不因此增加状态事件。 |

体积随当前目标和说明长度变化，不随历史轮数、当前输入数或任务数增长。解析一条 JSON 仍有正文成本；兼容、删除和资源结算分别报告。

新格式普通冷读共3次逻辑查询：opening 位置、最新 goal/state、该状态之后的删除存在性。只解码1个目标对象，不读取删除列表；状态事件已是固定读头时省去第3次。活 get 为0次查询。范围内确有删除才增加受影响定义查询；其扫描和真实修复另计，不能算入“只读一条”的普通路径。

| 代码对象 | 修改职责 |
| --- | --- |
| branch 状态及生命周期 | 区分最后业务状态与本实例执行；异常/关闭写 paused，重载只映射内存。 |
| 状态写入 | 一个完整业务状态类型，与必要输入原子提交，保持命令配对顺序。 |
| read / Session 路由 | 活 get 读内存，冷读最后业务状态；保留 Session 身份隔离。 |
| 当前计算 | 仅推进本实例新事实；不序列化运行对象，不逐事件 checkpoint。 |
| 兼容和删除 | 仅保留目标定义解释；移除运行状态/节点装载和每输入边界。 |
| 模型文本与 Pursuit | 历史状态不驱动执行，重载不继续旧评估。 |

## 7. Requirements raised to mechanism owners、验收与 Rule check

无新机制要求。使用已有 Timeline.append_batch、kind/位置及 ID/input_id 查询、Dispatcher 和生命周期。goal/state 是扩展事件，不新增核心事件变体、Store 业务表、Goal 专用接口或 native 版本。

验收使用公开 Goal 流程：

1. 普通聊天、计数归零、自动续行、评估等待不生成恢复快照或每输入删除边界。
2. set → 多轮 → completed 恰有两条完整业务状态；每条独立还原目标、judge 和状态。
3. 可处理异常、失败、取消、可控卸载：资源释放后一个 paused；重复终止不重复写。
4. active 后崩溃、paused 提交前后崩溃：加载不自动续行，也不补写状态。
5. reload 丢弃次数和评估；新的 set/resume 才执行，旧报告/turn/end 不恢复执行。
6. 重连保持原实例；部件 reload 暂停；UI/get 不把旧 active 当作当前 active。
7. clear、旧数据、opening、fork、删除期间崩溃和未知 schema 明确处理，不装载旧恢复节点。
8. 原子提交失败无半份状态；固定 Goal 变更数，扩大十倍普通聊天历史，不增加新格式状态定位的候选扫描/返回对象。

| 规则 | 本设计 |
| --- | --- |
| principles §1；ADR 002 D1 | Goal branch 是 Capability，拥有目标、内存和任务；Session 部件仅提供入口。 |
| ADR 002 D2–D5 | 沿用作用域、依赖、Settings 和语言中立数据，无新接线层。 |
| §4 规则 1、2 | 输入交接和删除归原所有者，Goal 不回收已交接工作。 |
| §4 规则 3、4 | 无 before I/O，Store 不解释 Goal。 |
| §4 规则 5 | 业务状态及必要输入原子提交，按序消费，提交前不发布成功。 |
| §4 规则 6 | 模型只读采用输入，不注入私有状态或内存对象。 |
| §4 规则 7、8 | 只用声明的原服务和事实，不改核心查询合同。 |
| §4 规则 9 | 只保存业务状态，执行进度留在内存。 |

删减：移除执行快照、逐事件进度写入、持久输入集合、节点协议、每输入删除边界。保留完整业务状态是为一条独立读取；input_id 服务既有命令删除语义，前驱状态复用类型/位置索引定位。
