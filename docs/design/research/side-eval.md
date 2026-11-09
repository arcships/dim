# Side Eval：任务 watcher 设计

本文定义按任务创建的 Side Eval watcher。**每个 watcher 是被观察分支的一个独立子 branch**，持续保留自己的观察上下文，按检查点分析任务是否偏离用户目标。用户可以接收提醒，也可以授权它向工作中的 agent 发送纠偏建议。

本文是设计提案，不代表功能已经实现。范围包括工具、生命周期、采样、分析、提醒、自动建议、用量与恢复；不改变现有架构合同。文中的参数和算例标为 **[GUESS]**，需要实际轨迹与用量验证。

## 1. 产品边界

用户或 agent 显式创建 watcher，说明“关注哪项任务、担心什么偏移”。安装插件不启动观察模型，没有新材料时不调用模型。默认反馈方式是提醒用户。

| 内容 | 决定 |
| --- | --- |
| 被观察对象 | 当前调用分支 B 的一项任务；一条 B 同时最多一个未结束 watcher |
| watcher 身份 | 子分支 W 的 branch ID；不另建一套 watcher UUID |
| 分支来源 | 插件创建的 host branch，来源关联到 B 上的创建操作 |
| 上下文 | W 继承 B 在合法切点之前的有效上下文，之后拥有自己的输入、模型输出和压缩记录 |
| 模型与能力 | 遵循当前 Session 与分支继承合同；Side Eval 不引入额外的模型选择策略 |
| 观察范围 | B 的已提交执行事实，以及已进入 B 的相关分支消息；不宣称覆盖其他分支的全部过程 |
| 默认输出 | 有证据的疑点、影响与建议；正常时只更新状态 |
| 自动纠偏 | 将建议送入 B，由执行 agent 核对并处理；不是 watcher 直接修改文件或回滚操作 |

watcher 的观察角色通过自己的输入说明。它仍共享 Session 的工具、权限和工作区；“只观察”是任务约束，不能描述成独立安全沙箱。工具准入继续服从现有 Permission / Approval，Side Eval 不批准工具，也不修改这些策略。任务完成验收仍归 Goal / Pursuit 的相应合同。

创建时需要明确目标和关注点，例如“修复登录故障，同时保持公共接口兼容”。普通对话也能创建，不要求先设置 Goal。不能以“检查一切”为由给所有短任务默认开 watcher。

## 2. 工具与用户入口

提供一个工具 `side_eval`，包含 `create`、`get`、`update`、`end` 四个 action。暂停和恢复通过 `update.state` 表达，不再增加两个操作。客户端使用同一业务合同，方法命名为 `session.side_eval.create/get/update/end`；用户处理疑点另用第 6 节的客户端动作。

四个 action 是对外操作入口，内部执行由 Timeline 事实和 Dispatcher 驱动。`create/update/end` 提交请求，由 B 的 Side Eval 按序决定；`get` 读取现有状态，不为查询新增事件。创建完成后，采样、分析报告和反馈继续由事件推进，不需要主 agent 轮询或反复调用工具。

以下为语言中立的数据合同。标识符、序号和用量沿用所属机制的数据约束；未知字段、无效枚举、空目标或空关注点在输入边界拒绝。

### 2.1 创建

~~~json
{
  "action": "create",
  "goal": {
    "text": "Fix the login failure while preserving the public API.",
    "sources": [{"branchId": "B", "sequence": 12}]
  },
  "focus": [
    "Preserve existing exported entry points.",
    "Keep the change within the login failure."
  ],
  "mode": "notify",
  "limits": {
    "maxEvaluations": 10,
    "maxTotalTokens": 300000
  }
}
~~~

| 字段 | 必填 | 含义 |
| --- | --- | --- |
| `goal.text` | 是 | 此 watcher 的观察目标，不是修改任务目标的指令 |
| `goal.sources` | 是 | 支持该目标的已采用用户输入或公开 Goal 事实；引用必须属于允许读取的历史 |
| `focus` | 是 | 非空的具体关注点列表；一个 watcher 可关注多个相关问题 |
| `mode` | 否 | `notify` 或 `auto`；省略为 `notify`，`auto` 受已有用户授权约束 |
| `limits` | 否 | 本 watcher 的有限额度；省略使用用户设置，指定值只能在调用者已获额度内分配 |

agent 工具不接受 `targetBranchId` 或创建者身份：B 来自工具调用上下文。客户端必须明确指定 B；用户直接输入的目标文本本身也保留为来源。agent 的概括不能替换原始用户要求；无法找到依据时返回 `goal_source_required`。

创建顺序：

1. B 的 Side Eval 部件校验范围、来源、管理权限和剩余额度，记录创建请求并预留预算。
2. 在 B 的合法已提交切点创建 W。来源 operation ID 关联创建请求，W 的首条输入包含观察委托、版本和首份采样材料。
3. W 发布后才开始评估。创建结果返回 W 的真实 branch ID；创建失败结算预留和请求，不留下“正在观察”的假状态。

同一 B 已有未结束 watcher 时返回 `watcher_exists`，附现有身份和状态，不覆盖目标。W 不能再创建 watcher；其他子分支不继承 B 的 watcher。创建者与授权来自真实入口，模型提交的文字不能证明身份。

创建是同步等待分支创建结果，不等待第一次评估。返回成功只表示观察分支已经建立。

### 2.2 查询与共同返回值

~~~json
{"action": "get", "watcherId": "W"}
~~~

工具结果的 `content` 至少提供以下数据，不能只写入 `structuredContent`：

~~~json
{
  "watcherId": "W",
  "targetBranchId": "B",
  "createdBy": "agent",
  "revision": 1,
  "state": "active",
  "mode": "notify",
  "goal": "Fix the login failure while preserving the public API.",
  "focus": ["Preserve existing exported entry points."],
  "observedThrough": 192,
  "evaluatedThrough": 184,
  "coverage": "partial",
  "inFlight": true,
  "openFindings": 1,
  "usage": {
    "evaluationsStarted": 3,
    "modelRequests": 4,
    "totalTokens": 71800,
    "cacheReadTokens": 42000,
    "cacheWriteTokens": null,
    "hasUnknownUsage": false
  },
  "limits": {
    "maxEvaluations": 10,
    "maxTotalTokens": 300000
  }
}
~~~

这是状态形状示例，不是实测值。`createdBy` 从真实入口派生为 `user` 或 `agent`，不接受调用者填写。`observedThrough` 表示调度器看到了哪里，`evaluatedThrough` 表示最近有效分析的截止点；后者不表示其中每个事件都被模型完整阅读。`coverage` 为 `full`、`partial` 或 `none`，详细查询列出遗漏范围和原因，并返回 findings 列表：身份、报告版本、状态、目标与证据引用、建议和输入交接状态。用量字段缺失记为未知，不填零。

`get` 读取持久事实派生的状态，不启动评估。当前分支的 agent 可查询关联 watcher；客户端按现有 Session 访问权限查询。查询不扩大可读历史范围。

### 2.3 更新、暂停与恢复

~~~json
{
  "action": "update",
  "watcherId": "W",
  "expectedRevision": 1,
  "patch": {
    "focus": [
      "Preserve existing exported entry points.",
      "Check that the fix also covers token renewal."
    ],
    "state": "active"
  }
}
~~~

`patch` 允许 `goal`、`focus`、`mode`、`state` 和 `limits`；只处理给出的字段，数组整体替换，不做隐式合并。`state` 只能为 `active` 或 `paused`；目标分支不能更换。空 patch 拒绝。修改限额不能低于已消耗和在途预留之和。

- `expectedRevision` 必填。冲突返回 `revision_conflict` 和当前版本，不覆盖并发修改。
- 成功更新在 B 的序列中生效并递增版本。旧版本在途评估失去发送建议的资格；向 W 发送更新输入，结算或取消旧轮次后使用新版本。返回中区分已生效的委托版本与 W 当前正在分析的版本。
- 暂停停止新采样请求、撤回仍未采用的采样，按既有中断协议结算已交接输入与在途评估，并处理未采用的建议；W 保持 open，保留上下文。恢复使用同一个 W、既有用量与版本，从暂停位置补取有界材料，遗漏仍标为 partial。
- 新用户澄清进入正常观察材料，并立即使旧依据不再适合自动建议；不要求用户为每句话手动 update。明确换到无关任务时暂停旧 watcher；无法判断任务归属时只提醒，暂缓自动建议。
- agent 可管理自己创建的 watcher，但不能削弱用户明确给定的限制、结束用户创建的 watcher或提升授权；用户可通过客户端或既有明确授权放开管理范围。不能仅凭“创建者是 agent”允许其改写用户目标。

### 2.4 结束

~~~json
{
  "action": "end",
  "watcherId": "W",
  "expectedRevision": 2,
  "reason": "The watched task is complete."
}
~~~

`end` 记录真实调用来源和原因，停止调度，取消在途分析，撤回尚未采用的建议，然后通过既有分支结束机制结束 W。业务终态随必要结算完成；如果调用返回时仍在关闭，明确返回 `closing: true`，不能报告已释放全部资源。

重复结束同一 watcher 返回已有终态，不重开、不重复通知。对未结束 watcher 的版本冲突仍返回 `revision_conflict`。结束 watcher 不代表任务通过验收；未解决疑点保留可读，不自动标记 resolved。

一个 `turn/end`、空闲或等待用户不自动结束 watcher。B 真正关闭时 W 随现有子分支关闭机制结束；用户中断 B 时 Side Eval 暂停观察，不能假定普通 branch 中断会自动传播。Session 卸载结算在途 I/O，恢复不重放旧评估；正常结束 W 后再次观察需要新的 create。

### 2.5 错误与重复调用

| 错误 | 含义与返回内容 |
| --- | --- |
| `invalid_params` / `goal_source_required` | 参数或目标依据无效，说明字段 |
| `watcher_exists` | 返回 B 已有 W 的身份和状态 |
| `watcher_not_found` / `watcher_ended` | 身份无效或操作不适用于终态 |
| `revision_conflict` | 返回当前版本与可读状态 |
| `not_authorized` | 修改超出创建者的管理权或用户授权，不静默降级 |
| `budget_exhausted` | 没有足够的可分配额度，不创建新观察分支 |
| `watcher_unavailable` | 插件、目标分支或观察分支不可用，主任务继续运行 |

工具调用 ID、客户端请求 ID 用于业务请求去重，同一 ID 不接受不同内容。分支创建本身没有按 operation ID 去重的保证，不能靠重试 create 恢复身份；必须根据已记录的创建事实查回已有 W。

## 3. 分支、状态与通信

复用[分支合同](../timeline-branches.md)、[插件合同](../plugin-contract.md)和[设计规则](../../architecture/rust-agent/principles.md)。同一个 Side Eval 功能提供一个 Session 入口部件和一个 branch 部件；branch 部件根据真实来源在 B 与 W 上承担不同职责，不拆成多个插件。

| 所有者 | 自己持有的事实与工作 |
| --- | --- |
| Session 入口 | 工具、客户端方法、内部报告入口、提示说明的注册；格式和来源校验、路由；不持有 watcher 的业务折叠状态 |
| B 的 branch 部件 | 创建关联、观察委托与版本、授权与预算分配、采样水位、已接收报告、疑点处置、通知与发往 B 的建议 |
| W 的 branch 部件 | 采样输入的采用、当前分析轮次、自己的模型用量、原始分析报告和报告提交回执 |
| TimelineTree / Loop 等现有所有者 | 分支身份与生命周期、模型与工具执行、上下文、输入交接；Side Eval 不复制它们 |

B 通过自己 Timeline 的 Dispatcher 观察工作，W 通过自己的普通 Loop 分析。W 不在后台直接折叠 B 的 Timeline；来自 B 的材料以已定稿采样输入进入 W。需要详情时，W 可经现有父分支读取工具得到证据，结果留在 W 自己的历史。

### 3.1 入口、持久事实与分发

改变业务状态的操作遵循同一条路径：入口校验格式、来源和权限，交给目标分支的 Side Eval 写入请求事实；该分支 Dispatcher 按序交付后，所有者检查当前版本与业务条件、记录有效决定，并登记后续任务。入口不直接推进分析循环，也不直接修改所有者的内存状态。

对外调用可以等待与自己请求关联的处理结果。例如 create 等待 W 创建完成、update 等待 B 的委托更新生效；等待发生在入口持有的调用任务内，Dispatcher 回调登记工作后即返回。请求提交成功、业务决定生效和远端工作完成是三个不同阶段，工具返回遵循第 2 节各 action 的约定。

Dispatcher 只保证本分支的按序消费，不替插件把事件广播到其他 branch。跨分支传递需要明确目标、来源、关联身份和持久化回执；抵达目标后再进入目标自己的 Timeline 与 Dispatcher。B 与 W 的序号不可直接比较，使用 `sampleId`、委托版本和原始证据位置关联因果。

~~~text
Tool / client: create, update, end
          |
          v
B Timeline: request --> B Dispatcher --> Side Eval decision
                                              |
B execution facts --> B Dispatcher --> sample |
                                              v
                                    W input/message (Queue)
                                              |
                                       W input/applied
                                              |
                                         W ordinary Loop
                                              |
                                         W turn/end
                                              |
                                       W Dispatcher
                                              |
                                     W Timeline: report
                                              |
                                     W Dispatcher
                                              |
                                 Side Eval report delivery
                                              |
B Timeline: received report <-----------------+
          |
     B Dispatcher --> validate and decide
          |
          +--> finding --> user notice
          |
          +--> authorized input --> B input/applied --> B Loop
~~~

### 3.2 两条 branch 之间传递什么

| 方向与内容 | 到达目标的方式 | 目标如何处理 |
| --- | --- | --- |
| B → W：采样材料 | B 先记录定稿采样，通过既有输入入口提交 W 的 Queue input | W 自己的 InputQueue / Loop 采用材料，W 的 Side Eval 跟踪该轮次 |
| B → W：委托更新 | B 先记录新版本，再向 W 提交 Append input | W 收到自己的输入事件；下一轮分析使用新版本，B 同时拒绝旧版本报告触发建议 |
| W → B：分析报告或失败终态 | W 先记录报告，再由 W 的 Capability 经 Side Eval 内部入口提交，由 B 的所有者写接收事实 | B Dispatcher 消费后决定是否更新疑点、通知或发送建议；交付回执只证明接收事实已提交 |
| B → B：用户发送或自动纠偏 | B 记录处置并提交普通建议 input | B 的 InputQueue / Loop 按现有规则采用，后续采用、消费和执行事实驱动效果复查 |
| 暂停、撤回、结束 | Side Eval 记录决定，向现有输入与执行所有者提交对应请求 | 以所有者产生的撤回、turn 或 branch 终态为准，不将请求当作已完成 |

内部报告入口是 Side Eval Capability 在 W 完成一轮后调用的插件业务服务，不是暴露给模型的跨分支工具，也不是一种“不可见 input”。Session 入口将经过校验的报告提交给 B 的 Side Eval 所有者，由后者写自己的领域指令事实，再按序决定。W 不持有 B 的服务句柄，Session 不代替 B 作业务判断。

这采用既有“能力影响分支”的关系以及 [Session 方法向目标所有者路由的合同](../contracts/session-boundary.md)。入口绑定真实 W、B、创建操作和轮次；调用者不能通过报告字段伪造来源。新增的是 Side Eval 自有业务入口，不是允许外部随意写 Timeline 的通用接口。

**notify 模式下，报告不会以 input 形式发给主模型。** 通知和状态来自 B 已接收的报告事实。只有用户明确发送或 auto 决策通过后，才创建给 B 模型的建议输入。任何跨分支业务步骤都不是跨 Timeline 的原子事务；报告送达和恢复规则见第 9 节。

Session 广播用于客户端状态更新和提醒，不承担上述命令或报告的唯一交付路径。通知可能丢失，业务决定必须能够从已提交事实恢复；不新增一套跨 branch 事件总线，也不让 W 直接订阅 B 的 Dispatcher 来维护 B 的状态。

### 3.3 与 EventHub 的分工

[EventHub](../plugins/EventHub.md) 将生产者发布的事件按订阅和显式目标投递为普通 branch input；本地 Timeline 与 Dispatcher 则承载有所有者的业务事实和状态转换。EventHub 的发布类型（如 `source.name`）与本节的 Timeline 类型（如 `side-eval/sample`）不是同一套类型，也不会自动互相分发。

**本提案保留 B/W 之间的定向工作协议，同时把已经通过 EventHub 进入 B 的材料纳入观察。** EventHub 不替代 Side Eval 的领域事实、报告接收入口或纯用户通知；Side Eval 也不另建可供其他会话订阅的消息分发系统。

| 链路 | 使用方式 | 原因 |
| --- | --- | --- |
| 外部生产者 → B 的构建结果、任务通知等 | 由 EventHub 投递；Side Eval 消费 B 已有的输入事实 | 复用统一事件入口，不新增一条观察订阅或复制投递队列 |
| 创建 W 与首份采样 | 继续使用 TimelineTree 的开线 prompt | EventHub 不创建 branch，不能替代首条输入的身份与开线合同 |
| B → W 的后续采样与委托更新 | 本提案使用第 8.5 节的定向输入协议 | 需要关联固定采样、委托版本、预留和 W 生命周期；EventHub 的公开订阅分发不能直接等价替换 |
| W → B 的报告和用量 | 第 8.6 节的插件业务入口，先写 report-received | 先由 B 决定是否提醒或纠偏；EventHub 投递会成为模型可读 input，不满足 notify 的隔离要求 |
| B 的纠偏 input | 第 8.7 节的本地反馈决定与输入批次 | EventHub 的远端投递不在该原子批次中，不能同时保留这条保证又声称全部改走 publish |
| 只提醒用户 | 由持久疑点派生客户端通知 | EventHub 的目标是模型输入，不是 UI 通知通道 |

EventHub 到达的内容遵循其输入说明：它是来源材料，不能因被投递或采用，就成为新的用户目标、委托更新或自动纠偏授权。待采用的外部事件也不能冒充“用户有一条尚未处理的指令”。确有目标调整时，仍由真实用户输入或 Side Eval 管理操作决定。

本分工不要求 Side Eval 注册 EventHub source、创建订阅或增加 process 部件。EventHub 不可用时，既有 B/W 工作协议仍成立。若用户另行要求把评估摘要公开给其他会话订阅，那属于 EventHub 的职责，应定义可发布的摘要合同，不能直接把内部 report 当成公开通知发送。

#### EventHub 不能被默认假定具备的保证

| EventHub 的现有语义 | 对 Side Eval 的影响 |
| --- | --- |
| `to` 与匹配订阅的目标取并集 | 显式指定 W 不表示只有 W 会收到。仅添加 match 属性也不是排他或授权边界，不能据此承诺内部材料只送到 B/W |
| 显式 `to.mode` 优先；没有显式值时按匹配订阅选最强模式 | 如迁移采样，必须明确 Queue；不能把订阅默认值当成固定执行策略 |
| 正文超过 5,120 字节转为 Session 文件及 1,024 字节预览，存储不可用时可能只剩截断内容 | 当前 token 预算不能保证正文低于字节阈值。收到输入不等于模型已看完整采样；读取文件还会增加请求与 token 消耗 |
| 同一生产者与 event ID 提供每目标输入去重，回执带实际 inputId/sequence | 可复用其投递去重，但仍须保留 sample/报告版本、采用状态与业务结算；delivered 不是 evaluated |
| 没有持久未送达队列或自动重试 | 本插件的持久源事实与恢复决定仍不可删除，不能把 delivery/evaluation 账目全部交给 EventHub |
| 已受理 publish 由 EventHub 持有直至结束，调用者取消或 EventHub 停用不撤销它 | 暂停 watcher 不能假称取消了该 publish；迟到 input、已采用内容和费用仍须另行处理 |
| source 归实际声明它的 row，publish 必须由同一个所有者发起 | 多个 B/W 部件不能各自声明或冒用同一个固定 source。迁移需要明确生产者资源的 scope，而不是在每条分支上直接复用名称 |

这些是采用 EventHub 时必须保留的语义，不是通过一层适配器就能消除的差异。其设计的 Routing principle 对“交付型输入”的措辞较宽；内部受控工作协议是否在其范围内，按第 10.1 节列为实现前的合同对齐事项，不由本提案暗中改写。

## 4. 采样：持续观察，有条件分析

### 4.1 什么算一步

调度器消费 B 自己已经提交的事件，只做事件归类、计数、时间检查和去重，不调用模型。流式 token、工具输出的每个小块、重复 UI 状态不各自算一步。

一个“有效步骤”是一次完整模型响应及其已完成的相关工具结果，或者一个可独立判断的执行终态。并行工具结果按请求归组；尚未结束的工具可作为“正在执行”材料，不能推断其结果。计时器到期但没有新增相关材料时不评估。

### 4.2 何时启动

以下参数是 **[GUESS] 初始建议**，保存在 Side Eval 自有业务设置中，不作为通用默认事实：

| 触发 | 行为 |
| --- | --- |
| 创建 watcher | 首轮建立目标理解与当前进度基线；已有可用材料直接分析，不重复拉完整历史 |
| 正常推进 | 累积 3 个有效步骤，或距上次调度 30 秒且存在新材料，启动一次 |
| 优先信号 | 新用户要求已采用、连续失败模式、完成宣称与失败结果并存等，提升下一次采样优先级 |
| 同时出现多个信号 | 合并到一个采样，不按信号数量并发调用 |
| 分析仍在进行 | 只推进待处理高水位，不启动第二轮；完成后按新材料和剩余预算再决定 |
| 等待用户、空闲、无新证据 | 不触发模型请求；可保留一次已有新材料的收尾分析 |
| 暂停、关闭或无额度 | 不启动；状态明确显示暂停原因和覆盖缺口 |

连续失败等规则只用于选择分析时机，不等于判定偏移。优先采样同样受预算约束；短时间大量事件不能绕过合并规则形成请求风暴。

用户中断、委托更新、授权撤销是控制变化，立即使旧轮次失去自动建议资格，不等待下一次模型判断。优先采样不能中断 B 正在运行的工具。

首份采样随 W 的创建 Queue 输入启动。后续采样只在上一轮结算、W 空闲时提交新的 Queue 输入；不把材料作为 Steer 注入正在分析的轮次。委托更新通过 Append 告知 W，暂停时使用既有中断入口停止它的在途工作；恢复后的下一份采样才启动新轮次。B 的版本决定始终先使旧报告失效，不依赖 W 是否已读到更新。

### 4.3 每次送什么

每轮固定 `sampleId`、`revision`、B 的 `after/through`、触发原因及材料范围。`through` 必须是已提交的合法批次边界。首次输入包含完整观察委托；后续输入只追加增量与必要的目标引用，目标改变时追加新版本。

材料按以下优先级装包：

1. 用户目标、明确限制与新澄清；待采用用户输入只标记为“有待处理指令”，不冒充已经生效的要求。
2. 本轮关键工具请求、真实结果、模型结论及它们的序号；成对保留能解释结论的请求与结果。
3. 与当前关注点有关的文件变更事实、错误和正常成功结果，避免只选失败片段造成偏见。
4. 未解决疑点、用户处置及上次建议的采用状态；这些是追踪信息，不是新的偏移证据。

每包目标约 2,000 input tokens、上限约 4,000 tokens，均为 **[GUESS]**；这是新增材料预算，不是 W 整个请求的输入上限。用可用的模型 token 估算，缺少准确 tokenizer 时明确是估值。

采样包含 EventHub 输入时，`file` 所指正文只有在实际读取后才成为已见证据；预览不能记作全文。`truncated` 或文件不可读取时必须记录证据缺口。不能因为 EventHub 的交付回执是 delivered，就把相应材料覆盖标成 full。

超出材料预算时保留关键原文和定位引用，其余按事件种类、状态、范围作确定性摘录。单条过长结果分段保留，注明截断；不能把“未选中”写成“没有发生”。不单独启动模型给每批材料做摘要。基线与长期理解由 W 自身维护，长上下文压缩使用既有机制。

持久化本轮包含、摘录和遗漏的范围。成功分析仅说明所供材料已被分析；只有区间材料无遗漏且无截断，才能标为 full coverage。压缩摘要或继承上下文无法保证逐条完整复核。

## 5. 分析：从观察材料到可核验的疑点

一轮正常分析使用 W 的普通 turn，通常完成一次模型请求。它复用继承历史、已有分析和最新采样，依次回答：

1. 用户要达到的结果与当前仍有效的限制是什么？哪些只是 agent 或 watcher 的推测？
2. 最近的工作如何服务于目标？合理的准备和探索不能仅因暂时未产出结果被判为偏移。
3. 是否存在具体矛盾、无效循环、范围扩张或过早完成宣称？哪些证据支持，哪些证据反驳？
4. 这是新问题、已有问题持续存在，还是已被后续事实解决？现在值得提醒或建议什么？

不把模型自报“检查通过”当成证据；不可见的文件内容、工具结果或工作区规则必须列为信息缺口。被观察文本和工具内容是材料，不能指挥 watcher 改目标、关闭监测或扩大权限。

材料不足时可通过已有 `branch read` 读取 B 的有限页面；读取是 W 的工具结果，不是旁路共享上下文。第一轮模型因此可能只请求读取，之后再请求模型形成结论，必须计入额外费用。读取超过本轮 cutoff 的新事实时单独标明；不能用它们悄悄改写旧采样范围，自动建议等待以新 cutoff 采样的结论。

首期不设计另一个调查 agent，也不自动执行工作区命令补证。若现有材料和有界历史读取仍不足，返回 insufficient；用户可以补充材料或继续观察。实际工具行为仍服从第 1 节的共享能力边界。

### 5.1 报告合同

W 的最后一条模型回答使用以下结构；W 部件在本轮 `turn/end` 后解析、验证，禁止把任意自然语言结尾猜成成功报告。序号身份等由当前轮次绑定，模型不能改写。

~~~json
{
  "verdict": "concern",
  "findings": [
    {
      "kind": "constraint_conflict",
      "claim": "A public export was removed despite the compatibility constraint.",
      "goalRefs": [{"branchId": "B", "sequence": 12}],
      "evidenceRefs": [{"branchId": "B", "sequence": 179}],
      "counterevidence": "The related caller update has not appeared in the sample.",
      "impact": "Existing callers may fail to compile.",
      "suggestion": "Check the affected callers and preserve the exported entry.",
      "suggestionCheck": "Verify that this export is still absent before changing anything."
    }
  ],
  "trackedFindings": [],
  "missingEvidence": []
}
~~~

- `findings` 的元素为 `FindingDraft`，字段就是示例中的 `kind, claim, goalRefs, evidenceRefs, counterevidence, impact, suggestion, suggestionCheck`，均必填；没有反证时明确说明材料中未见反证，不能把它写成“不存在反证”。
- `trackedFindings` 的元素为 `{findingId, state, evidenceRefs, reason}`；state 为 `open/resolved/obsolete/insufficient`。resolved/obsolete 必须有新的证据引用；insufficient 可以没有新证据，但须说明缺口。
- `missingEvidence` 为字符串数组，逐项说明缺少什么；不得伪造一个不存在的引用来填充。

- `verdict` 为 `clear`、`concern` 或 `insufficient`。`clear` 只说明本轮材料未发现可报告偏移，不是任务验收通过。
- `kind` 为 `constraint_conflict`、`goal_drift`、`unproductive_loop`、`unsupported_claim` 或 `premature_completion`。
- 引用只能指向本轮可见材料或 W 已有的可追溯证据。错误引用、无法解析的结构与失败 turn 都记为分析失败，不产生自动建议。
- `trackedFindings` 按已有 finding ID 报告 `open`、`resolved`、`obsolete` 或 `insufficient`，附新证据。未提及旧疑点不代表已解决。
- 模型给出的分类和判断是报告材料；是否通知、是否有权发建议、是否过时由 B 的 Side Eval 决定。不使用未经校准的“置信度 90%”作为自动动作门槛。

原始报告保留在 W。B 接收的领域报告含上述结论、来源、cutoff、覆盖信息和用量回执，支持恢复与处置，不重复保存整个轨迹。

分析失败、被中断或没有合法报告时，也通过同一内部入口交付本轮终态及已知用量。B 收到终态释放该轮 inFlight 占位，保留失败与未知用量；不能等待一份不会出现的成功报告，也不能把失败水位算成成功覆盖。

### 5.2 同一问题如何追踪

finding 身份在 B 首次接受时由原始 reportId 和报告内索引确定，具体字段见第 8.4 节；不由模型任意生成。后续采样携带 open findings 的 ID 和原始证据，模型引用已有 ID 更新状态。

相同目标限制、同类问题、相同或重叠证据更新同一疑点，不重复弹窗。无法确定是否相同时保守合并展示并禁用重复自动建议；没有实质新证据就不能仅换一种措辞再次纠偏。用户 dismiss 的证据不重复提醒，出现新的实质证据才允许重新打开。

## 6. 发现偏移后的交互

### 6.1 仅提醒用户

1. W 完成分析，内部报告入口将报告送到 B。
2. B 按序检查 watcher 状态、委托版本、证据引用、覆盖和已发生的用户变化。失效报告可供历史查看，但不产生当前提醒。
3. 有效 concern 更新疑点并发出一次用户通知；clear 安静更新覆盖；insufficient 显示信息不足，不包装成风险结论。
4. 用户可以查看证据、修改关注点、忽略该证据，或将建议发送给 agent。

通知不抢走输入焦点，不中断正在输入的内容；客户端重连从状态读取未处理疑点，不靠重新弹出历史通知恢复。

~~~text
+------------------------------------------------------------------+
| Side Eval: preserve API compatibility                             |
| Target: B       Watcher branch: W       Mode: Notify              |
| Observed: #192  Evaluated: #184         Coverage: Partial         |
| [Open watcher] [Edit] [Pause] [End]                               |
+------------------------------------------------------------------+
| Possible conflict with a user constraint                         |
| Goal: preserve the public API                         [B #12]    |
| Evidence: exported entry removed                      [B #179]   |
| Impact: existing callers may fail to compile                      |
| Suggestion: check callers and preserve the exported entry         |
|                                                                  |
| [View evidence] [Send suggestion] [Dismiss]                       |
| If target is idle: [Send and continue]                            |
+------------------------------------------------------------------+
| Usage: 3 evaluations / 4 requests                                |
| Input: 69.8k (cache read: 42k)  Output: 2k                        |
| Total: 71.8k / 300k     Cache write: unknown                      |
+------------------------------------------------------------------+
~~~

客户端动作包含 `watcherId`、`findingId` 与当前报告版本。发送前重新校验；过时按钮返回 stale 状态并更新卡片，不发送旧建议。

客户端使用 `session.side_eval.respond`，参数为 `{watcherId, findingId, expectedReportRevision, action}`，action 为 `dismiss`、`send` 或 `send_and_continue`。`send` 只适用于仍在执行的 B，`send_and_continue` 明确授权从空闲继续；运行状态与操作不匹配时返回 `target_state_changed`，不自动换成另一种行为。重复请求返回同一处置结果，发送成功附 `inputId` 和当前交接状态。报告版本冲突返回 `finding_stale`，附最新报告或失效原因；没有发送即不占用建议次数。该方法是用户入口，不作为 agent 自行批准 auto 的工具。

- B 仍在执行时，用户发送使用 Append，等待既有输入交接。
- B 空闲时，必须明确选择“发送并继续”，对应 Steer；“发送”不能暗中唤醒任务。
- dismiss 只改变该疑点的提醒处置，不抹除证据，也不代表问题被修复。
- “已发送”“已采用”“已解决”分别显示；不能用发送成功替代解决证据。

### 6.2 自动纠偏

`auto` 表示用户允许 Side Eval 在以下条件全部成立时自动发送建议。创建者是 agent 不提供额外授权。

| 门槛 | 判定 |
| --- | --- |
| 授权 | 当前 mode 为 auto，用户授权仍有效，干预次数与模型额度未耗尽 |
| 依据 | 明确用户要求与具体事实存在可解释冲突；不是意图推测或纯信息不足 |
| 版本 | 报告与当前委托一致，没有待采用的新用户指令 |
| 新鲜度 | 自 cutoff 后没有新的用户目标变化、相关完整执行步骤或已知解决事实；不能只看 wall clock |
| 运行状态 | B 的被观察 turn 仍在运行，未中断、未结束，未等待用户或批准 |
| 重复控制 | 同一疑点和证据尚未自动发送；之前建议的观察期已结束 |
| 覆盖 | 判断所需的目标与关键证据完整；无关材料的 partial 不自动否定结论，但缺少关键事实时必须降为提醒 |

普通进度事实也可能使结论变旧；无法判断是否相关时，按过时处理，合并进入下一次采样。宁可少自动发送，也不能用持续刷新同一旧结论伪装新鲜。

通过门槛后，B 在自己的 Timeline 同一批次记录处置与建议 input，由 Side Eval 所有者按稳定 submission ID 串行去重，具体保证见第 8.7 节。内容包含来源 W、目标版本、证据截止位置、具体疑点、建议和“先核对当前事实是否仍成立”的要求；用户同时看到自动发送记录。

~~~text
concern accepted
      |
current + authorized + actionable?
      | no
      +------> notify or mark stale
      |
      v
Append suggestion to B
      |
input/applied --> input/consumed --> observe new work
      |                                  |
      |                                  +--> resolved / persists / insufficient
      |
goal changed, paused, ended or revoked?
      +--> request withdrawal if still pending
      +--> already handed off: cannot recall; show state and clarify if needed
~~~

Append 不主动唤醒空闲任务，不中断当前模型请求或工具执行。自动建议绑定报告所观察的 B turn；该 turn/end、用户中断、目标改变或授权撤销时，B 请求撤回未采用输入，不能因为 watcher仍 active 就让旧建议等待下一轮。InputQueue 决定能否撤回。发送后到采用前仍有异步窗口，已交接输入不能召回，因此不能承诺建议在执行时仍然最新，更不能承诺在错误工具动作发生前阻止它。

主 agent 收到建议后仍负责核对当前状态、选择具体修复动作并服从原有权限。自动纠偏不自动改 Goal、不回滚文件、不代点批准、不取消用户等待，也不重启已结束的任务。

### 6.3 纠偏后的观察期

只有 `input/consumed` 后才进入“观察是否改善”；单纯入队不算纠偏发生。采样暂缓同一疑点的再次发送，至少等待包含新结果的下一份采样，不能以固定时间过去就判定无效。

- 有新证据证明恢复：标为 resolved，记录证据。
- 仍有同一矛盾：更新疑点，交给用户；首期同一疑点最多自动建议一次。
- 任务转向或用户改变目标：标为 obsolete，保留历史。
- 没有足够的新结果：保持 insufficient，不宣称已修复或失败。

每个 watcher 最多自动建议 2 次为 **[GUESS]** 初始上限，用户授权可更低；不同疑点也共用此上限。达到上限降为仅提醒，恢复、更新或重建 watcher不能自行重置用户授权额度。

## 7. Token、缓存与预算

### 7.1 计费材料来自哪里

调度计数、读取已提交事实和去重本身不产生模型 tokens。W 的模型请求、工具读取后的继续分析、失败重试和上下文压缩可能产生模型消耗；不能只统计最终报告那次请求。

每次普通请求的逻辑输入近似为：

~~~text
I_i = inherited effective prefix
    + watcher instructions
    + all retained prior samples and analysis
    + current sample and tool results
~~~

同一历史会在后续请求中再次作为输入；是否命中缓存影响处理和费用，不把这部分历史从逻辑 input tokens 中删除。输出预算也包含服务实际报告的其他输出用量，不能只以可见报告字数估计。

第一轮继承多少取决于创建切点与有效压缩历史，不能预设为固定的小上下文。创建前显示估算上下文和有限额度；一次请求预计就超过剩余额度时不启动评估。很晚才创建的 watcher可能比从短上下文开始的 watcher消耗显著更多。

### 7.2 一个可复算的量级例子

**[GUESS]** 假设每轮只用一次模型请求、没有额外读取和压缩：

- 继承的有效输入及固定观察说明 P = 12,000 tokens。
- 每次新增采样 D = 2,000 tokens。
- 每轮输出 O = 500 tokens，后续保留。
- 共 n = 10 轮。

第 i 轮输入为 `P + i × D + (i - 1) × O`。因此：

~~~text
Total input = nP + D × n(n + 1) / 2 + O × n(n - 1) / 2
            = 252,500 tokens
Total output = nO = 5,000 tokens
Total logical usage = 257,500 tokens
~~~

| 缓存情景 | 普通输入 | 缓存读取输入 | 输出 |
| --- | ---: | ---: | ---: |
| 无缓存命中 | 252,500 | 0 | 5,000 |
| 每轮仅命中同一个 12k 固定前缀 | 132,500 | 120,000 | 5,000 |
| 首轮冷启动，后续理想命中上次请求输入 | 36,500 | 216,000 | 5,000 |

这些是理想化情景，不是承诺或实测；第二行要求首轮也已有可用前缀缓存。第三行将上轮输出与本轮采样视为新输入，没有假定刚生成的输出已进入缓存。缓存写入机制、附加成本及失效不在这张分类表中。

如果继承前缀 P 从 12k 增至 50k，同样 10 轮的总输入会变为 632,500 tokens。若某轮额外读一次工具，通常还需一次携带完整 W 上下文的模型请求；不能简单只加“读取到的 2k”。表格不含 B 因采用建议增加的后续执行用量，它属于工作分支，产品统计应单独展示。

### 7.3 哪些缓存能用，哪些不能保证

| 层次 | 用途与边界 |
| --- | --- |
| Timeline 历史复用 | W 继承 B 切点前的历史；减少重复保存或读取不等于模型缓存命中 |
| 采样与去重状态 | 同一 sample 固定内容，只增量准备新材料；没有新证据时不发新请求 |
| 模型 prompt cache | 共同 System、工具和继承前缀提供复用机会；命中依赖协议、模型、路由、有效期与实际前缀一致性 |
| W 连续请求前缀 | 采样追加在尾部，历史不重写时具备后续复用机会；仍以服务返回的用量为准 |
| Context compaction | 缩短后续输入，压缩请求也消耗 tokens，并可能使原有前缀缓存失效 |

动态目标、采样时间、序号和报告追加到输入尾部，不塞入 System 前部。更新委托追加新版本，不为了展示“当前状态”反复重写整段历史。压缩、配置采用或工具声明变化依照原有合同处理，不能为了缓存保留过时目标。

不保存一套跨任务“判断结果缓存”：目标、工作区状态和证据可能已经改变。已检查 sample 的重复请求通过业务去重返回已有结果即可。

### 7.4 用量和停止条件

同时显示已启动评估轮数、公开事实记录到的普通模型与压缩请求数、prompt/completion/total tokens、cache read/write tokens，以及服务提供的 cost/currency。普通用量来自 model/usage，压缩用量来自 context/compaction 的诊断；统计范围和未知情况见第 8.7 节。只统计 W 开线后自己发起的请求，继承历史中的 B 用量不再记作 W 的消费。未提供 cost 不推算成零费用；未提供缓存用量显示 unknown。

缓存读写是输入用量的细分，不能将 `promptTokens + cacheReadTokens + cacheWriteTokens` 当作总输入再累加。缓存命中率只在用量口径完整可比时按累计缓存读取输入 / 累计总输入显示，部分请求缺字段时显示“已知请求”覆盖。

预算分为三个约束：

1. **评估轮数**：开始调度即占用；失败、取消和无有效结论也算已启动，防止无限重试。
2. **总 tokens**：累计 W 的全部可归属模型使用，包括缓存输入、输出和压缩；用于约束规模，不冒充精确费用上限。
3. **自动建议次数**：按实际提交的建议计数，不能因未解决问题反复发送。

B 持有用户授权额度和分配事实，W 持有本分支实际用量，报告回执带累计水位。创建预留有限份额，在途与未知用量仍占用；结束后仅释放可以确定未使用的预留。恢复、更新和 agent 重建 watcher不增加 B 的授权额度，只有用户明确增加授权才能继续分配。

启动前估算并预留，结束后按实际 Usage 结算。普通 Loop 可能重试或额外调用工具，异步观察用量无法保证在 token 上限的最后一个 token 处停止；额度是调度和停止策略，超出幅度取决于在途请求及内部重试，本文不承诺固定超额上限。未知用量保留估算占用并标记，不当作剩余额度重新花掉。

额度不足时暂停模型分析，展示“额度不足，最近分析至 #N”，保留历史和已有判断。精确金额上限与逐请求硬准入不属于此设计的保证。

## 8. 事件合同、跨分支交付与依赖

本节是 Side Eval 提案的事件声明。**复用事件保持现有字段和所有者不变；新增事件只使用 `side-eval` 域。** 新事件尚未实现，不加入核心事件枚举，不修改已有格式样本。事件进入持久化和 Dispatcher 的方式遵循[插件事件合同](../plugin-contract.md#5-events)。

### 8.1 复用现有事件

下表列出 Side Eval 会消费的事件及实际使用的字段，不重新定义其完整合同。现有字段拼写以[当前事件定义](../../../crates/dim-agent-types/src/events.rs)为准；不存在 `sample_id`、`watcher_id` 等核心字段。B、W 均只消费自己分支上的事实。

| 现有事件 | 使用的字段 | 现有生产者 | Side Eval 消费者与用途 |
| --- | --- | --- | --- |
| `input/message` | 信封 `id/sequence`；`submission_id?`、`user?`、`blocks`、`mode?` | 验证输入入口；首条子分支输入由 TimelineTree 生成；EventHub 经既有 Session 输入入口投递 | B：区分真实用户要求与来源材料，读取待处理输入和已发送建议；W：采样与委托更新 |
| `input/applied` | `input_id`、`mode?` | InputQueue | B/W：确认输入已交给 Loop，不能再按未采用输入撤回 |
| `input/consumed` | `input_id` | Loop | B：建议进入执行上下文；W：采样实际被消费 |
| `input/withdraw`、`input/deleted` | 请求：`input_id, reason?`；结果：`input_id` | 请求由有权调用的输入入口写入；结果由 InputQueue 写入 | B/W：请求撤回与撤回成功分别处理，不把请求当成功 |
| `stop/interrupt` | `reason, scope` | 既有中断入口 | B：暂停 watcher；W：结算被中断分析 |
| `turn/start`、`turn/end` | `turn_id`；终态 `status` 及 `reason/error` | Loop | B：运行范围及建议过期；W：关联分析轮次、形成报告或失败终态 |
| `step/model-start`、`step/model-output-end` | `turn_id, request_id`；输出终态 | Loop / ModelProvider | B：完整模型步骤边界；W：实际请求身份和失败状态 |
| `step/text` | `turn_id, request_id, delta` | Output | B：采样可见回答；W：拼接本轮最后一个请求的报告文本；不按 delta 次数触发评估 |
| `step/tool-use`、`step/tool-result` | `turn_id, request_id, call`；`call_event_id?, results[{call_id,result}]` | Output / ToolExecutor | B：请求与结果证据；W：有界父分支读取的结果与引用 |
| `exec/started`、`exec/ended` | `task_id`、所属 turn/request；终态 `status,result` | BackgroundExec | B：异步工具的实际终态；不把初始工具返回当成后台任务已成功 |
| `approval/request`、`approval/respond`、`approval/cancelled` | 请求：`approval_id, source, call, reason`；响应：`approval_id, decision`；取消：`approval_id, reason` | Approval 及其响应入口 | B：待审批状态；Side Eval 不产生审批决定 |
| `model/usage` | `turn_id, request_id, usage` | Output | W：普通模型请求的已报告用量 |
| `context/compaction-start`、`context/compaction` | 开始序号；结果的 `start?`、`status`、`diagnostics.modelUsage?`、`diagnostics.modelCallCount` | ContextPolicy | W：压缩请求和用量；它们不额外产生普通 `model/usage` |
| `branch/create` | `at, source, branch, prompt` | TimelineTree，在 B 写入 | B：真实 W 身份、创建操作关联；不等同于 W 已发布 |
| `branch/create-failed` | `branch, createEvent, status, error/reason` | TimelineTree，在 B 写入 | B：创建失败结算；不用新增 watcher 创建失败事件 |
| `branch/start`、`branch/resume` | `parent?, createEvent?`；resume 无业务载荷 | TimelineTree，在 W 写入 | W：自身开线与恢复；角色身份同时依据公开目录中的 source |
| `branch/end`、`branch/ended` | `source, ancestor?`；ended 另有 `branch, endEvent` | TimelineTree：end 在 W，ended 在 B | W：停止工作；B：确认 W 已结束、结算残留采样 |

`input/applied` 和 `input/consumed` **没有 turn/request 字段**。W 依据自己的事件顺序，将采样 input ID 与当前 open turn、随后实际发生的 `step/model-start` 关联。当前消费事实与 model-start 并非一个原子批次；只收到 consumed、尚未产生请求便中断时，不能声称分析过该采样。

没有新增 `sample-applied`、`analysis-started`、`watcher-ready`、`watcher-ended` 或 `suggestion-consumed` 事件。这些状态由上述事实和 Side Eval 的关联记录派生。B 的配置与权限变更仍由其公开所有者提供；不能凭收到一条 Side Eval 通知推断授权改变。

### 8.2 新事件的共同约定

所有新事件沿用现有信封 `id, sequence, create_at, type, name, data`。本插件设置 `type: "side-eval"`，`name` 为下表名称；`data.schemaVersion: 1` 是本插件载荷版本，不增加全局事件版本。新事件不设置 `context_render/context_state`，不会直接进入模型上下文。

| 数据 | 精确含义 |
| --- | --- |
| `EventRef` | `{branchId, eventId, sequence}`，引用已经提交的事件。branchId 来自所属 Timeline，另外两项来自其信封 |
| `requestId` | 入口为一次管理操作保留的稳定身份；工具来源绑定实际 call，客户端来源绑定请求；同 ID 不接受不同内容 |
| `createRequestId` | 创建 watcher 的 requestId。W 尚未分配时用它关联工作；它不是第二个 watcher ID |
| `watcherId` | 创建成功后的真实 W branch ID，由 B 的 `branch/create` 给出 |
| `sampleRef / sampleId` | 指向 B 的 `side-eval/sample`；sampleId 就是该事件的 eventId，不另分配身份 |
| `reportRef / reportId` | 指向 W 的 `side-eval/report`；reportId 就是该事件的 eventId |
| `revision` | B 已接受的观察委托版本；create 为 1，成功的 update/end 递增；无效请求不递增 |
| `Origin` | `kind: user\|agent\|runtime`；agent 附真实 `callRef/callId`；runtime 附 `causeRef` 或 `settingsRef: {source, scope, revision}`，二者必有其一。入口填写，模型参数不能指定 |
| `WatchSpec` | 第 2 节的 `goal, focus, mode, limits` 与 `state: active\|paused`；字段只有这一份合同 |
| `Grant` | `source, maxTotalTokens, maxEvaluations, maxSuggestions, agentManagement`；source 为 `{kind: command, commandRef}` 或 `{kind: settings, source, scope, revision}`；agentManagement 为 `own` 或 `user_delegated`，均遵守第 2 节的用户约束。它是授权快照，不是用户身份凭证 |

`EventRef` 用于协议关联；原有模型证据引用仍使用第 5 节的 `{branchId, sequence}`，不要求模型生成 eventId。字段标 `?` 才可缺省，枚举外值拒绝；可空值只用于合同明确表示未知的字段。类型和格式验证在入口、模型报告解析、持久化读取边界完成；内部生产者保证合同，消费者直接使用类型，不反复转换或补默认值。

身份为非空字符串，序号、版本和次数为现有安全整数范围内的非负整数，revision 从 1 开始；token 估值/预留为有限非负数，实际用量沿用 Usage 的数值合同。settings source/scope/revision 沿用设置所有者的字段类型。版本不支持时，插件停止该 watcher 的业务处理并报告不可用，保留原事件可读；不能把未知版本按当前格式填默认值后继续运行。

B/W 序号分别有序，不能跨线比较。版本、sampleRef、reportRef 用于关联，不代替来源校验。W 身份根据真实 host source、parent 与 B 的创建关联识别，不根据 prompt 中的标记文字识别。

### 8.3 新增事件目录

新增九个类型，各有后续决定需要的数据；生产者均通过自己分支的 timeline 写入。Session 入口只路由到所有者，不自行向任意分支 append。

| 新事件 | 所属 Timeline / 生产者 | 消费者 | 为什么需要 |
| --- | --- | --- | --- |
| `side-eval/command` | B / Side Eval 的请求入口 | B Side Eval | 管理操作的来源、顺序、并发版本与去重 |
| `side-eval/watch` | B / Side Eval 决策任务 | B Side Eval；客户端读状态 | 已接受的委托、授权分配、暂停或结束意图 |
| `side-eval/sample` | B / Side Eval 采样任务 | B Side Eval 投递任务 | 固定采样内容、覆盖、版本与预留，不能重试时重新取一个不同快照 |
| `side-eval/delivery` | B 或 W / 本地 Side Eval 投递任务 | 同一分支 Side Eval | 记录已提交的远端身份或本代投递终态，避免把丢回执当未发送 |
| `side-eval/report` | W / Side Eval 分析结算任务 | W Side Eval 投递任务 | 原始分析终态与用量，支持恢复补交而不重跑模型 |
| `side-eval/report-received` | B / Side Eval 报告接收入口 | B Side Eval | 本地持久化接收到的报告，B 不折叠 W 的历史 |
| `side-eval/evaluation` | B / Side Eval 决策任务 | B Side Eval | 本轮是否有效、过时或失败，释放 inFlight 与结算预留 |
| `side-eval/finding` | B / Side Eval 决策任务 | B Side Eval；客户端读状态 | 疑点身份、证据与解决/忽略状态，供后续去重和复查 |
| `side-eval/feedback` | B / Side Eval 决策任务 | B Side Eval；客户端读状态 | 一次建议的授权依据、稳定输入身份与次数扣除 |

这些是本插件的领域合同，不需要核心、其他插件或通用分发器理解其业务。客户端从 Side Eval 的状态与通知读取，不要求通用客户端解释任意扩展事件。

### 8.4 新事件字段

以下字段均位于 `data`，且均包含 `schemaVersion: 1`。

**管理请求与已接受状态**

| 事件 | 字段与约束 |
| --- | --- |
| `command` | `requestId, origin, action, args`。action 为 `create/update/end/respond`；args 复用第 2、6 节对应请求数据，身份由入口补齐；get 不产生 command |
| `watch` | `commandRef, createRequestId, revision, change`。change 为 `create/update/end`；下面的条件字段与 change 同处 data，按取值出现 |
| `watch.change=create` | `spec, grant, reservedTokens`；revision=1，此时不要求 W 已存在 |
| `watch.change=update` | `spec, grant`；保存已接受的完整有效委托，含暂停/恢复，不能只存无法独立解释的自由文本 |
| `watch.change=end` | `reason`；使旧评估失去干预资格并停止新调度；W 的实际结束仍以 `branch/ended` 为准 |

自动暂停等运行期改变也产生带 runtime Origin 的 command，其 causeRef 指向本地中断或结算事实；不伪装成用户操作。入口的格式/权限错误直接返回；请求进入 B 后不符合当前业务条件，由调用结果说明，不额外写无状态变化的 watch 事件。重建不会把没有 watch 结果的旧 command 自动当成新请求重新执行。

**采样与投递**

| 事件/字段 | 合同 |
| --- | --- |
| `sample.createRequestId, revision` | 关联已接受的委托；接收方不能通过样本中的文字改变授权 |
| `sample.targetTurnId?` | 采样时 B 的被观察 turn；空闲采样可省略，不能自动纠偏唤醒空闲任务 |
| `sample.after, through` | B 上本次增量区间 `(after, through]`；through 是固定的已提交批次边界 |
| `sample.reasons` | 非空集合：`initial/steps/elapsed/user_input/failure_pattern/completion_claim/follow_up/resume` |
| `sample.content` | 已定稿的模型材料正文，包括目标依据、证据摘录和必要跟踪信息；投递重试不重新生成 |
| `sample.included` | `[{ref: EventRef, coverage: full\|excerpt}]`，列出正文使用的原始事实 |
| `sample.omitted` | `[{after, through, reason: budget\|unavailable}]`，B 上省略的材料区间；excerpt 另由 included 说明 |
| `sample.coverage` | `full/partial/none`，遵循第 4 节；full 也不是任务正确性的保证 |
| `sample.estimatedPromptTokens, reservedTokens` | 预计整个下一次 W 请求的输入与本轮 token 预留；都是调度估值，非实际 Usage；该事件占用一次评估额度 |
| `delivery.sourceRef, channel, status` | sourceRef 为自己分支的 sample/watch/report；channel 为 `sample_input/watch_input/report`；status 为 `committed/failed/interrupted` |
| `delivery.status=committed` | 必须带 `targetRef`：sample/watch 对应 W 的实际 input/message，report 对应 B 的 report-received |
| `delivery.status=failed/interrupted` | 带 `targetBranchId, code, reason, commitState: not_committed\|unknown`；unknown 不能当作“目标没有收到” |

delivery 不记录每次临时重试，只记录已经获得的提交回执或本代最终失败/中断。首次 sample 通过开线 prompt 交付，使用既有分支创建事实结算，不伪造一条拥有可控 submission ID 的 sample_input delivery；绑定规则见第 8.5 节。

watch.create 的 reservedTokens 是从 B 用户授权中划给 W 的份额；sample 的 reservedTokens 是这份额度内的本轮预留，不能把两层预留都算作实际消费。实际消费只来自报告中的已知 Usage。

**报告、接收与采样结算**

| 事件/字段 | 合同 |
| --- | --- |
| `report.kind` | `analysis/usage`。analysis 结算一次分析；usage 只补报新的累计用量，不能产生偏移判断 |
| report 共同字段 | `targetBranchId, createRequestId, usage`；来源 W 由该事件的所属分支确定 |
| `report.kind=analysis` | `sampleRef, revision, inputRef, status`；completed/invalid_report 必须有 `turnId, finalRequestId, terminalRef`，terminalRef 指向 W 的 turn/end；其他状态只携带确实存在的 turn/request/terminal 引用 |
| analysis 的 `status` | `completed/failed/cancelled/interrupted/invalid_report`；completed 必须有第 5 节定义的 `result`，其他状态必须有 `reason`，不得附一份伪造成功结论 |
| `report.kind=usage` | 不带 result 或采样终态；用于晚到的压缩/请求用量，避免同一 sample 被再次判决 |
| `report-received` | `reportRef, report`；report 是源事件的同一个有类型的载荷。B 复制这份有界数据以作本地判断，不在消费时再读取 W |
| `evaluation` | `sampleRef, causeRef, status, findingIds`；causeRef 是 B 自己的 report-received、投递失败、中断或创建失败事实；创建在核心事实提交前被拒绝时可引用本轮 sample 作为原工作身份 |
| evaluation 的 `status` | `evaluated/stale/failed/cancelled/interrupted`；evaluated 需要有效 analysis 报告，其余带 `reason`。每个 sample 只结算一次 |

B 仅在 evaluated 时推进有效分析覆盖；stale 报告不变成新的当前判断。无论报告是否过时，其中新的已知用量都要结算。已结算 sample 又收到晚到报告时不再写第二条 evaluation，只按新报告的用量水位更新账目。

`report.usage` 是 W 在 `through` 截止处的累计业务统计，字段为：

- `through`：W 自己已提交的统计截止序号。
- `ordinaryRequests`：W 自己记录到的普通模型请求次数。
- `compactionRequestsKnown`：压缩诊断明确给出的请求次数之和；不能推断没有诊断的部分为零。
- `known`：沿用现有 Usage 类型，表示已报告用量的合计；缓存和费用可选字段仍保持其未知含义。
- `hasUnknownUsage, hasUnknownRequestCount`：是否存在缺失、被中断或无法完整计量的请求；已知合计不表示账目完整。

W 从本分支普通 model/usage 与压缩结果诊断形成这份统计，不累计继承自 B 的历史，也不累计 context/budget 等估算。原始用量事实按唯一身份处理一次；B 收到更大的 through 才替换累计快照并计算差额，不把每份累计报告再次相加。同水位同内容不重复结算，同水位内容冲突是合同错误；较小水位不覆盖新账目，其 analysis 结论仍单独接受版本与时效检查。不能用取最大 token 值掩盖数据冲突。

**疑点与建议**

| 事件 | 字段与约束 |
| --- | --- |
| `finding` | `findingId, reportRevision, causeRef, state, disposition, detail`。新 findingId 为首次接收报告的 reportId 与 findings 索引组成的稳定身份；更新沿用该身份，reportRevision 单调递增 |
| `finding.state` | `open/resolved/obsolete/insufficient`；`disposition` 为 `pending/dismissed`，把忽略提醒与问题已解决分开 |
| `finding.detail` | 第 5 节 finding 的有类型内容，保留目标、证据、反证、影响与建议；模型跟踪更新只有在引用有效时才成为 finding 事实 |
| `feedback` | `findingId, reportRevision, revision, causeRef, source: user\|auto, submissionId, mode, content`；auto 另必须有 `targetTurnId` |
| `feedback.mode` | `append/steer`；auto 仅 append，用户明确从空闲继续才 steer |
| `feedback.content` | 已定稿建议，与同批 input/message 的业务正文一致；submissionId 唯一标识此次发送，实际 inputId 从输入事件获得 |

finding 的 causeRef 可以是本地 report-received，也可以是用户 respond command；解决和重新打开必须有新证据。同一次报告产生的 finding 变更与 evaluation 同批提交，evaluation 放最后，通知在其提交后发出。用户 dismiss 只写必要的 finding 变化。feedback 只为实际提交的建议产生，失败或被拒绝的发送不写“已发送”。

### 8.5 B → W：采样的完整交付

**首次采样：**

1. B 接受 create，提交 watch，准备并提交 sample S0。sample 的 envelope.id 就是 sampleId。
2. B 将 S0 的引用、revision 和定稿正文放入创建 prompt，调用现有分支创建入口，host operation_id 关联 createRequestId。
3. TimelineTree 在 B 记录 branch/create，分配真实 W；在 W 生成 branch/start 与第一条 input/message。首条输入的 `mode=queue`、`user` 为创建 prompt，blocks 含分支身份，嵌套时可能还有父分支块；`submission_id` 与 handover 不由 Side Eval 指定。
4. W 从自己的开线事实与实际 input/message 绑定 `sampleRef → inputRef`。采样信息不保证位于首个 block；角色身份也不从 prompt 推断。
5. 已写 branch/create 的创建失败消费 B 上的 branch/create-failed 结算 S0。若创建入口因切点无效、父分支关闭等原因在写入核心事实前明确拒绝，B 直接以 S0 为 cause 写 evaluation(failed/interrupted) 和实际原因；不存在一条必然出现的 branch/create-failed。提交情况不明时保留未知预留并按已有创建事实核对。成功恢复通过已记录的 branch/create 找回 W，不因缺少回执重试 create。

**后续采样：**

B 在 sample 提交后，按已声明的 treeCommands 输入入口向已关联 W 写入下列现有 input/message 载荷。示例中的标记是 Side Eval 自己的输入协议，样本正文只作为材料；外部文本按提示说明合同转义。

~~~json
{
  "submission_id": "side-eval:sample:S17",
  "mode": "queue",
  "blocks": [
    {
      "type": "raw",
      "text": "<dim-side-eval kind=\"sample\" sample=\"S17\" branch=\"B\" sequence=\"240\" revision=\"3\" through=\"236\">Analyze the finalized evidence supplied here.</dim-side-eval>"
    }
  ]
}
~~~

其中 S17 是 B 上 sample 的实际 eventId，sequence 是该 sample 的实际序号；through 则是被观察材料截止点，二者不混用。业务正文由该 sample.content 逐字带入，不在重试时重新取当前历史；包装模板随 schemaVersion 固定，同一 sourceRef 的完整输入不能改变。样本不设置 key，避免将某轮材料当作永久生效的状态块；`user` 与 handover 省略。

输入入口返回 W 的实际事件后，B 写 delivery(committed)。同一 sample 重试只用相同 submission_id 和相同内容；返回原输入是已提交去重，不是再次采用或再次分析。B 不订阅 W 的 Dispatcher；W 按本地 applied、consumed、turn 和 model-start 处理，并在终态报告中返回实际 inputRef。

委托更新复用相同路径，但 sourceRef 指向 watch、mode=append、submission_id 按该 watch 事件 ID 派生，不占用一个新 sample。暂停后的继续执行由下一份 Queue sample 驱动。

恢复时，B 不因缺少 delivery 回执就盲目重发可能尚未被目标接收的采样；本代未完成投递按 interrupted/unknown 结算。W 已提交的输入按普通恢复合同处理，可能继续执行；晚到报告仍受 B 的版本和轮次终态约束。运行期重试与恢复补发不能混为一谈。

### 8.6 W → B：报告的完整交付

W 在自己的 turn/end 后解析并验证结果，提交 report R。R 提交后，W Dispatcher 登记投递任务，通过 **新增的本插件 Session 服务 `sideEval.submitReport`** 交付：

| 项目 | 合同 |
| --- | --- |
| 方法 | `async submitReport(request, cancelled)`；cancelled 是调用方持有的取消回调，按既有服务跨语言合同传递，不写进事件载荷 |
| 请求 | `{reportRef: EventRef, report: ReportData}`，ReportData 就是第 8.4 节 report 的载荷，不另造一种报告格式 |
| 调用方 | W 的 Side Eval Capability；不是模型工具或客户端可写任意报告的 RPC |
| 校验 | 来源 W 的真实 parent/host source 与 B 的创建关联一致；reportRef 所属分支与来源一致；模型解析只提供 result，不能提供这些控制字段 |
| 目标 | report.targetBranchId，即关联 B；Session 只按公开业务入口路由 |
| 接收 | B 自己的 Side Eval 入口在本地串行去重并写 report-received；不执行报告中的建议 |
| 返回 | `{receivedRef: EventRef}`；只在 B 的接收事实已提交后返回 |
| 重复 | 同 reportRef、同内容返回同一个 receivedRef；不同内容返回 `report_conflict` |
| 其他错误 | `invalid_report_source/invalid_params/branch_closed/service_unavailable/operation_interrupted`；不静默改投其他分支 |

Session 的入口依赖本层 treeCommands 等服务，不在装载依赖中注入或等待 B/W。B/W 向上声明使用 sideEval；Session 仅在调用时路由到目标分支的本插件业务入口。因此调用路径可能返回同一分支，装配依赖图仍单向，不能实现成 Session 与 B 的相互等待。

该分支业务入口由 B 的部件提供为新增的 branch 作用域公开服务 `sideEvalBranch`：`request(CommandData)` 异步处理第 2、6 节操作并返回其既定结果，`receiveReport(request, cancelled)` 只提交接收事实并返回 receivedRef，`get()` 读取自己的状态。参数直接复用本节类型。Session 在调用时通过目标分支门面获取这个服务，不把它列为 Session 的向下装配依赖；W 不获取或调用它。它是 Side Eval 自己的命令/状态入口，不是另一个部件或任意事件写入接口。

取消只结束调用方持有的等待和 I/O，不撤销已经提交的接收事实；提交状态不明时记录 delivery(interrupted, unknown)，以后按同一 reportRef 核对或补交。取消回调由当前插件代持有，不能作为持久身份保存。

B 的接收入口只保证提交，不等待 B Dispatcher 的业务判决，更不等待 W。W 收到回执后写自己的 delivery(committed)，记录 `R → receivedRef`。B Dispatcher 随后消费 report-received，核对 sample、版本、状态和证据，产生 evaluation/finding 或只更新用量。

~~~text
W: report R committed
        |
        v
sideEval.submitReport(R)
        |
        v
B: report-received Q committed ---- receipt(Q) ----> W: delivery(R, Q)
        |
        v
B Dispatcher --> evaluation + findings --> notice / feedback
~~~

通知、跨线收到、业务接受和模型采用分别有自己的事实，不设置一个含糊的 delivered=true 表示全部成功。

若 B 已提交 Q 而回执丢失，W 可补交同一个持久化 R；B 返回 Q，不能再写一份接收事实。若 W 已记录 R 但从未送达，恢复只补交 R，不重新调用模型；B 仍按当前状态判断。关闭导致无法投递时保留 W 的报告和失败 delivery；B 的 branch/ended 或本地中断结算等待中的 sample。Session 关闭时不为补交报告重新打开 B。

### 8.7 B 内部反馈、用量与原子边界

**纠偏输入与决定。** B 的 Side Eval 在自己的串行决定路径中检查稳定 submissionId 是否已有已提交或待发布的 feedback。只有第一次有效决定才提交同一批次：先 input/message，再 feedback；起决定作用的 feedback 放最后。输入使用第 6 节的 Append/Steer 规则，feedback 记录权限依据和次数。

当前 Rust 绑定的 Timeline 混合批次能够同时提交领域事实与 input，但**混合批次本身不按 submission_id 去重**。这一点不同于只接收输入的 Branch.input/inputs。Side Eval 必须保证自己的决定与写入串行、把待发布决定纳入去重、恢复时不重发已有 feedback；不能把提交 ID 当成核心自动提供的“恰好一次”。也不能将当前仅该绑定提供的混合批次方法写成已经存在的跨语言服务。

同一建议的来源是原有 source event ID，submissionId 按来源稳定派生；实际 inputId 在提交后由 input/message 获取。已提交的反馈原子批次不会只留下“已发送决定”或只留下输入。之后 applied、consumed、deleted 继续复用现有事件，feedback 不伪造交接结果。

**用量来源。** W 普通用量使用 model/usage；压缩用量使用 context/compaction 的 diagnostics.modelUsage/modelCallCount，两者不能漏记或重复记。压缩 turn_id/request_id 可为空，以本线压缩 start/结果身份归属，不能硬挂到某轮普通模型请求。压缩汇总没有可保证的完整费用信息，收到 modelUsage 也不证明每个请求都回报了用量；取消或诊断缺失保持 unknown。

报告中的累计水位用于 B 更新预算，模型/压缩原始事件仍由现有生产者拥有。已知计数只反映公开事实能表示的请求，内部传输重试不保证逐次可见。上下文估算不能替代 Usage。正常用量随 analysis 报告交付；只有该轮终态后又有新用量，或没有在途分析而发生独立压缩时，才合并发 report(kind=usage) 补账。不为每个 usage 事件复制一轮交付；补账既不占评估轮数，也不触发提醒。

**跨线没有原子批次。** B 的 sample 与 W 的 input、W 的 report 与 B 的 report-received 分别提交。稳定来源身份、目标提交回执和本地去重解决重复交付；不能保证两条线同时提交。输入投递与报告投递的恢复行为分别遵循第 8.5、8.6 节，不套用同一种自动重试。

### 8.8 依赖、分发与关闭

| 部件 | 声明依赖与目的 |
| --- | --- |
| Session 入口 | 工具、方法、提示注册所有者；treeCommands 运行期路由；Session lifecycle；配置与客户端通知；提供本插件 sideEval 业务服务 |
| B 角色 | timeline、dispatcher、branchLifecycle、分支身份；treeCommands 创建和输入；本插件设置与业务入口；公开输入撤回/执行控制入口；提供 branch 作用域 sideEvalBranch |
| W 角色 | timeline、dispatcher、branchLifecycle、分支身份；Session sideEval 报告服务；普通 Loop 的公开执行事实 |
| 客户端 | Side Eval 的状态查询和 Session 通知；不依赖原始跨 branch 消费回调 |

Session 不保存分支业务 fold，不新增一个报告转发插件。内部入口只属于本插件的业务合同，不是允许外部写任意事件的通用端口；它也不提供对可信进程内插件的安全隔离。

此方案不直接依赖 eventHub 服务：EventHub 的输入已经由输入所有者交付，B 观察它们时只依赖上述公开事件。若按第 10.1 节的对齐结论迁移内部输入投递，必须一起修改生产者 scope、输入身份、交付回执、正文覆盖与取消合同，不能只替换调用方法。

所有写入先提交再分发。处理中新写的事实进入 Timeline 尾部，消费者随后按序看到；不能递归同步调用下一位消费者。异步创建、投递和关闭登记在发起部件自己的任务中，Dispatcher 回调立即返回。activated 后才工作，closing 先停止新工作并释放所持 I/O，再结算必要终态；普通 Loop 的请求由既有执行所有者停止。

只保留影响后续决定的事实。原始报告和接收副本分别服务于两个分支的恢复；delivery 防止重复投递，evaluation 防止同一 sample 重复结算，finding/feedback 分别服务问题跟踪与实际发送。查询、UI 刷新、计时器空转和每个模型 token 都不新增 Side Eval 事件。

## 9. 中断、迟到与恢复

- **分支创建**：从 B 的创建请求及 `branch/create` 恢复关联。operation ID 只是来源关联，不是 create 幂等键；已有创建事实时查回身份，未完成创建按 TimelineTree 的结算结果处理，不盲目再次开分支。
- **采样输入**：首份采样通过创建 prompt 交付，沿用 TimelineTree 生成的真实开线 input；后续采样固定内容后使用 submission_id。重复同 ID 返回已有输入，不代表内容重新校验或模型已经采用；恢复缺回执的投递按第 8.5 节结算，禁止同 ID 改 payload 或盲目重发。
- **分析中断**：恢复时将已启动但未结算的分析记为 interrupted，保留已扣轮数、已知用量和未知预留，不重放模型请求。已接收但尚未启动的采样则服从普通输入队列的恢复合同，可能继续启动执行；沿用原 sample 和预算身份，不重复扣轮数。恢复后的版本或授权不符时仍禁止旧报告自动建议。不能承诺普通 branch 恢复后绝不调用模型。
- **报告交付**：W 的原始报告先持久化，再经内部入口提交。B 按真实 reportRef 幂等接收，W 记录回执；analysis 再按 sampleRef 保证只结算一次，usage 按水位更新。恢复可重投同一份已定稿报告以完成未确认交付；这只补齐已提交事实的内部投递，不重放模型、工具或已做出的反馈决定。B 必须照常检查当前状态，不能因重投就执行旧建议。
- **派发恢复**：两条 Timeline 没有原子提交保证。B 已接收但回执丢失时，重投返回同一个接收结果；W 已结束但 B 留有未完成轮次时，结合分支终态结算为中断，不无限等待。
- **建议输入**：处置事实与 input 在 B 同批提交。恢复只重建发送/采用/消费状态，不重复发同一个建议；失效且未采用的请求撤回，已交接的不能抹除。
- **通知**：重连读取现有未处理疑点；广播失败不丢持久状态，也不反复弹同一条消息。
- **用量回执迟到**：按来源和用量水位结算一次，不能因报告已过时而丢弃费用；未知用量不得提前退款。
- **插件卸载**：停止采样，通过现有执行入口请求停止 W 的在途 turn，结算本代采样、分析与报告交付；不能只停止调度器而留下普通 Loop 继续执行。保留已提交业务事实，缺失期间明确显示观察不可用，不影响 B 的普通执行。Session 或分支本身关闭时服从其关闭顺序，不重复请求关闭。

观察分支意外结束时，在 B 显示“观察已结束/不可用”并结算关联；不会悄悄创建替代分支。恢复后的历史缺口允许补取有界材料，但补取、压缩或分析都必须重新受预算约束。

## 10. 影响面与 Requirements raised to mechanism owners

| 所有者 | 本设计要求 |
| --- | --- |
| Side Eval | 自有业务设置、工具与方法、两个 branch 角色、Session 的 sideEval 与 branch 的 sideEvalBranch 业务服务、第 8 节九种领域事件和状态查询；这些均是新增插件合同 |
| 产品装配 | 按配置安装可选插件，注册其依赖；核心不硬依赖它 |
| 客户端 | 创建与更新表单、watcher branch 入口、提醒与证据卡片、建议处置、覆盖与用量展示 |
| TimelineTree / Loop / InputQueue | 使用现有分支创建、普通执行、输入与撤回合同；不添加 Side Eval 专属分支模式 |
| 上下文与用量所有者 | 使用既有压缩和 Usage 合同，缺失数据如实呈现 |
| Goal / Pursuit / Permission / Approval | 使用既有公开边界，不改变其状态或决策职责 |
| EventHub | 接受其已投递输入作为来源材料；内部工作协议与其 Routing principle 的适用范围按第 10.1 节对齐 |

本范围没有必须修改基础设施的要求。内部报告是本插件自身的业务方法和事实，不能包装为核心新增的隐藏 input，也不能为实现方便读写其他插件私有状态。

若产品以后要求“建议只可由指定 turn 在最新条件仍成立时采用”的严格保证，现有 Append 与异步撤回不足。该要求涉及输入所有者的通用条件采用合同，必须另行提出并由其所有者完成设计与验证；本提案不假设它存在。硬 token 准入与独立能力隔离同样不由观察提示词实现，不列入本次交付条件。

### 10.1 与 EventHub 所有者对齐内部交付边界

- **受到影响的合同：**EventHub 的 Routing principle 要求交付型输入走其入口；本提案的 B/W 采样与反馈同时受固定任务范围、模型可见性和本地原子决定约束。现有表述不能直接证明这些内部工作输入应当迁移，也不能默认认定它们已经被排除。
- **已考虑的现有机制：**publish 的显式 to 仍合并订阅；普通 input 不提供非模型报告收件箱；溢出文件会改变材料可见范围；交付回执不与 B 的决定原子提交；取消调用不取消已受理 publish。因此完整迁移会改变第 3.3 节列明的业务合同，不能作为等价重构处理。
- **需要对齐的通用边界：**区分“供订阅者处理的生产者事件”与“业务所有者管理的定向工作协议”。本提案建议前者复用 EventHub，后者继续通过其所属机制的公开入口处理；这不是为 Side Eval 增加专用例外。若 EventHub 的所有者要求后者也统一迁移，应先在其设计中明确排他目标、内容完整性、取消和与业务决定协调的通用语义，再由 Side Eval 更新自己的合同。
- **序号、批次、中断、恢复：**按本提案分工无需改变。若选择迁移，不能继续承诺现有原子 feedback/input 批次，也不能把发布取消或重试视为当前输入路径的同义操作；必须重新核对这四项。
- **所有者与 issue：**EventHub 与 Side Eval 各自维护所属设计；合同边界对齐由 #1312 跟踪。此处只提出合同边界，不修改 EventHub 文档、运行时或外部依赖。

## 11. Rule check

| 规则 | 回答 |
| --- | --- |
| Principles §1 | B 与 W 的 branch 部件按自己的事实折叠和写入，均为 Capability；Session 部件是入口与注册，不持有分支业务状态；App Server 只装配和暴露入口 |
| §4.1 状态所有者 | 内部报告先成为 B 的领域指令，由 B 决定处置；输入采用、撤回、关闭由现有所有者决定 |
| §4.2 交接不可撤回 | 未采用建议可请求撤回；已交接建议只能通过后续事实说明或现有中断协议处理 |
| §4.3 before | 不使用，不在提交锁内分析或等外部工作 |
| §4.4 核心无业务语义 | 核心不认识偏移、采样、疑点或纠偏 |
| §4.5 原子批次 | 同一 B 上输入在前、反馈决定在后，一次提交；混合批次的去重由 B 负责；跨 B/W 不宣称原子，使用持久化身份和幂等接收 |
| §4.6 上下文路径 | W 收采样 input / 读取工具结果，B 只在需要发送建议时收 input；领域报告不会自动进入主模型 |
| §4.7 公开交互 | 只用公开服务和各自的事实；内部入口绑定真实来源，不让 W 注入 B 服务或折叠 B 状态 |
| §4.8 基础设施 | 不要求修改核心机制；EventHub 的路由原则适用范围按第 10.1 节显式对齐；严格条件采用等更强保证不承诺 |
| §4.9 事实最小化 | 仅持久化后续调度、授权、费用、去重、恢复与处置需要的事实；纯显示刷新通知即可 |
| ADR 002 D1 | B 拥有观察委托与反馈决策，W 拥有自己的分析工作；Session 无业务聚合状态 |
| ADR 002 D2 | 一个功能插件，无独立接线、缓存或报告转发插件 |
| ADR 002 D3 | B/W 向上依赖 Session 服务；Session 只运行期路由，不在装配依赖中反向等待 branch；核心不依赖 Side Eval |
| ADR 002 D4 | 安装配置与预算/采样等业务配置分离；任务委托是分支事实 |
| ADR 002 D5 | 行为与数据合同语言中立；具体语言绑定和注册写法留给实现文档 |

### 11.1 当前机制复核结论

按现有运行时机制，本提案可以作为可选插件完成；需要新增第 8 节列出的插件业务类型、状态与入口，不需要修改核心事件域或 Dispatcher。与 EventHub 的内部交付边界仍按第 10.1 节对齐，不能将机制可实现等同于所有路由合同已确定。此结论是静态架构核对，不代表已经实现或通过流程验证。

| 核对项 | 结论与当前依据 |
| --- | --- |
| 事件所有权与模型上下文 | 自有域使用现有扩展事件信封，领域事件不直接进入上下文；采样/建议复用 adopted inputs，符合插件合同 §5 与基础 ADR 的上下文边界 |
| 首次与后续采样 | TimelineTree 的首条输入只有创建 prompt 与其自有身份块，不能指定 submission_id；后续输入使用公开输入入口及其去重合同，已分别定义 |
| 采样消费关联 | input/consumed 没有 turn/request 字段，按 W 自身顺序关联；不虚构一个将 consumed 与 model-start 原子提交的机制 |
| 跨线报告 | 使用本插件 Session 业务服务运行期路由，B 在本地写接收事实并分发；W 不持有 B 服务，不折叠 B 历史，不依赖瞬时广播保证交付 |
| 装配依赖 | Session 先注册入口，branch 向上依赖；Session 不在 apply 等待 branch，避免向下依赖或启动环 |
| 原子反馈 | 当前 Rust Timeline 支持混合批次，但不替插件做 submission ID 去重；由 B 串行保证。当前跨语言绑定不能据此宣称已有相同方法 |
| 创建与恢复终态 | 提交前拒绝由 B 结算 sample，提交后由既有创建事实结算；不盲目重放 create。报告可以幂等补交，未确认采样不在恢复中盲目重新提交 |
| 用量与迟到报告 | 普通 usage 与压缩 diagnostics 分开取数；较旧水位不回退账目；过时分析仍可提供更新的用量 |
| EventHub | 负责可订阅事件的普通输入投递，不能直接替代领域事实或 notify 报告入口；输入材料纳入采样，截断与正文文件按实际可见范围处理 |

复核未要求新增全局报告队列、共享 watcher 状态或跨 branch 总线。保留的边界是普通分支共享权限、输入交接后不可召回、token 额度非硬准入；这些不通过新事件名称绕过。

## 12. 验证标准与删减审查

实现时验证完整公开流程，不以私有 helper 测试替代：

1. 用户与 agent 创建、重复创建、权限限制、并发更新、暂停恢复和幂等结束；W 的身份是真实 branch ID。
2. W 继承切点前上下文，之后按自己的输入与压缩推进；同 Session 模型与工具合同保持一致。
3. 流式输出、长工具、并行工具、空闲计时与优先信号都只产生有界采样；一轮在途时只保留下一轮高水位。
4. 目标澄清、材料截断、正常探索、重复失败和虚假完成宣称分别得到有证据且不过度推断的判断。
5. notify 模式不把报告写入 B 模型上下文；用户发送和 auto 的状态正确区分提交、采用、消费与解决。
6. 迟到报告、撤销授权、目标更新、结束 turn、用户等待、撤回竞争与重复疑点均不造成重复或越权自动建议；已交接输入的局限可见。
7. 创建中断、W 请求中断、报告回执丢失、插件重载及客户端重连能恢复事实，不重跑已启动的模型请求、不重复开线或发建议；尚未启动的采样恢复执行时沿用原预算身份。
8. 缓存缺字段、取消未回 usage、额外工具请求、压缩与重试费用纳入统计；恢复或重建不能重置预算。
9. 固定轨迹评估提醒准确性、误报、漏报、发现延迟和 token 开销；自动建议额外检查是否改善任务结果，而非只评报告措辞。
10. action 入口只提交请求或读取状态；改变执行的决定都经所属分支 Dispatcher。B/W 的样本与报告分别落入目标历史，重复交付不重复决定；仅丢失 Session 通知不改变业务结果，notify 报告不因跨线传递而进入 B 模型上下文。
11. 首次采样无自定义 submission_id、创建在核心事实提交前失败、consumed 后尚未 model-start 的中断均有明确结算；未知提交状态不能伪装成未发送。
12. 同 reportRef 的不同内容拒绝，乱序用量水位不回退账目，usage-only 报告不再结算 sample；并发建议在混合批次之外仍由 B 保证只提交一次。
13. EventHub 输入被当作来源材料而非用户指令；待采用通知不误触发用户目标变更，正文预览或不可读文件不被当作完整证据；外部投递不改变 watcher 的授权。

删减审查：保留一个工具、一个 watcher branch、一个功能插件及按真实所有者划分的部件。branch 是持续上下文的承载者，B 的部件是采样和反馈决策所有者，Session 入口用于既有工具/客户端注册与同插件报告提交；三者职责不同，不能合并到错误 scope。暂停复用 update，预算复用现有 Usage，历史复用 Timeline。九个领域事件分别承担第 8 节列明的恢复和决策用途；不增加可由现有 input/turn/branch 事件推导的采用、开始、结束等镜像事件。

首期不增加常驻全局观察 agent、每 token 模型调用、独立摘要模型、另一层调查分支、自动回滚、跨任务结论缓存、独立告警存储或综合评分仪表盘。上述完整用户流程仍成立，没有需要为证明删减而人为制造的实验。
