# Side Eval：用 side branch 观察任务

Side Eval 是一种 branch 用法：**长期保留的普通 branch + 活动订阅 + branch prompt**。用户通过 `/watch <prompt>` 表达观察意图，主 agent 安排观察分支；主 agent 也可以根据任务需要自行创建。观察分支检查任务是否偏离目标，给用户提醒，或向主 agent 提供建议。

本文是设计提案。它定义用户流程和需要补齐的通用能力，不改变其他模块的现有合同，也不包含实现。`long run` 表示 branch 可跨多个 turn 保留上下文；等待时没有模型请求，不保持一个无限执行的 turn。

## 1. 一个机制，多种用途

```text
Ordinary branch
  + purpose and prompt
  + subscriptions when needed
  + existing execution, history and messages
      |
      +-- watch       Observe progress and report drift
      +-- evaluate    Check completion for Goal / Pursuit
      +-- task        Work on a delegated part
```

Side branch 是这些辅助行为线的产品统称，身份始终是已有 branch ID。它不新增执行引擎、权限体系、上下文存储或插件状态机。Side Eval 提供 `watch` 的入口与提示词约定；branch 负责执行，EventHub 负责订阅交付，各业务所有者决定如何使用结果。

`purposeKind` 是展示和管理用的用途标签，例如 `watch`、`evaluate`、`task`；它不替代现有 `source`，也不改变 `main`、tool line、host line 的权限和通信规则。提示词和标签不能授予完成 Goal、批准工具或访问其他分支的权限。

## 2. 创建、管理与关注

### 2.1 `/watch <prompt>`

首期只提供 `/watch` 一个名字。它像其他任务入口一样，把观察要求交给当前主 agent；它不是后台定时循环的别名。没有 prompt 时补全命令并等待用户输入，不自动创建“检查所有事情”的 watcher。

```text
> /watch Keep the login fix within scope and preserve the public API.

main
  -> create W with the objective, evidence sources and watch prompt
W
  -> subscribe to main's activity
  -> read changes since its cut
  -> finish this turn; stay open for the next event

Lines
  main   Fix login               working
  W      watch: API compatibility idle     read through: #184
         [Open] [Edit prompt] [Subscriptions] [End]
         Execution: shared tools and workspace

W: Possible API break at #192. Evidence: ... Suggested check: ...
   [Open evidence] [Send to main]
```

`Tab` 补全、`Enter` 提交，其他行为遵循 [CLI command menu](../cli-commands.md#command-menu)。关注、打开和未读提示复用 Lines，不新增 watcher 仪表盘。`/watch` 首期是 CLI 入口；其他客户端沿用普通 branch 入口，不承诺创建体验一致。创建结果与详情注明共享工具和工作区，不加确认弹窗，也不显示“只读隔离”。

主 agent 组装 brief：用户目标及来源、关注范围、何时提醒、是否允许直接发送建议、何时结束。不能把自己的目标概括当作用户新增授权。用户明确要求观察时安排 W；主 agent 自行创建时沿用已有分支权限，并说明用途。能在当前任务中直接核对的事情就由主 agent 自己做；需要独立上下文和持续关注时才开 W。

首期 agent 创建沿用现有 `branch` 工具的范围：`main` 创建自己的 tool line，W 不再开子 watcher。客户端或插件创建的 host line 继续由其创建者管理；不因被显示为 side branch 就允许主 agent 接管。

### 2.2 四个管理动作

保留 `create`、`get`、`update`、`end` 四个逻辑动作，归入通用 branch 管理，不再提供独立 `side_eval` 工具。以下是目标合同；当前工具仍是 `create/message/read/end`，缺口列在第 7 节。

| 动作 | 输入与结果 | 行为 |
| --- | --- | --- |
| `create` | `prompt`，可选 `purposeKind, idleNotice`；返回真实 `branchId` | 使用现有分支创建；watch 是主 agent 选择的 brief，不是核心特殊模式 |
| `get` | `branchId`；返回身份、用途、委托及 revision、生命周期、执行状态、读取位置、可用的 `activitySource` 与本线用量 | 只读，包括已结束线；列表复用 branch directory，详情经本线所有者读取，不聚合 EventHub 私有状态 |
| `update` | `branchId, prompt, expectedRevision`；返回新的 revision | 只更新该线的委托；相同内容不写新事实，revision 冲突返回当前版本供重读；订阅通过 EventHub 自己的入口修改 |
| `end` | `branchId`；返回 `ending` 或 `ended` | 使用现有结束协议，保留历史并停止接收新输入；订阅通过 unsubscribe 或后续投递的 gone 结果清理，不把结束当作任务验收通过 |

入口按真实调用身份、Session、`source` 和父子关系校验以下允许范围，不能仅凭 branch ID 或 `purposeKind` 授权：

| 调用者 | `get` | `update` | `end` |
| --- | --- | --- | --- |
| 模型：main | 自己及自己创建的 tool lines | 自己创建的 tool lines | 自己创建的 tool lines；按 ID 直达属于待补齐合同 |
| 模型：tool line | 自己；父线材料仍走已有 `read` | 不允许自行改写委托，需请求 main 或用户 | 只结束自己 |
| 模型：host line | 自己 | 不通过通用模型工具更新 | 不通过通用模型工具结束 |
| 已获 Session 访问权的客户端 | 该 Session 的各线 | tool lines 和用户创建的 host lines；插件委托须走插件入口 | 该 Session 的非 main 线；结束 evaluator 时由 Goal 处理失败 |
| 插件所有者 | 经既有公开入口管理自己创建的 host lines | 按自己的业务协议决定，不接受模型伪造来源 | Goal 按既有协议结束自己的 evaluator |

模型 `create` 仍仅 main 可用。兄弟线、其他 Session 和 main 未创建的 host line 不向模型开放管理。管理入口拒绝对 ending/ended 线的更新；对已结束线再次 end 返回现有 `branch_ended`，不写新事实。未知目标、越权、版本冲突或依赖不可用都明确返回，不创建替代线。工具返回的身份、revision 与错误必须出现在 `ToolResult.content`。

目录与现有 `purpose` 保持创建时摘要的含义，不随 `update` 改写。`get` 从本线最新委托记录读 `purposeKind, prompt, revision`；未更新时来自开线委托，main 无委托时为空。列表仅列目录，不复制 prompt；展开详情显示“创建摘要”和“当前委托”，区分最新提交与模型是否已经采用。

订阅由客户端另调 `eventhub.subscriptions`，模型另用 EventHub 的 `events` 入口，受各自入口权限限制；branch `get` 不代理管理权限或读取订阅文件。无 EventHub 管理权限的客户端显示订阅不可查看。创建方把父线 `get` 提供的活动 source 写入 W 的 brief，W 无需越权读取父线管理详情。

状态不另起一套：生命周期使用 `open / ending / ended`；当前执行显示 Loop 的状态；订阅显示 EventHub 的记录及失败。`idle` 表示本轮结束，不能显示成观察已停止；无订阅、源不可用和从未读取不能显示成正在正常监控。“已读至”由 W 的 BranchLine 从自身成功的 `branch read` 调用目标和工具结果 `nextCursor` 派生，采用最近一次成功读取，不解析模型回答。它只证明材料已返回，不证明完整分析；没有读取则显示未读取。

当前主 agent 通过 `branch.message`、显式 `mode: steer` 请求 tool line 自行结束，让空闲 W 也能收到并执行；发送成功不是已结束，以生命周期事实为准。客户端通过现有结束入口操作。直接按 ID 结束自己的 tool line 属于通用工具扩展，在所有者接受之前不假装已有。host evaluator 的自动结束仍交给 Goal，用户强制结束不被当作成功。

**空闲通知需要一个通用选择。** 当前 tool line 每轮结束时，如果本轮没有向主线发送过 `steer`，BranchLine 会自动发 `steer` 空闲通知，普通 `append` 建议不能阻止它。建议创建时可指定 `idleNotice: none`，不产生自动空闲输入；省略保持现有 `steer` 行为。watch 使用 `none`，需要报告时主动发送消息；客户端仍从 W 的执行事实显示空闲。该设置由 BranchLine 持久化并执行，不能靠 prompt 模拟，也不由用途标签隐式决定；显式消息和结束通知不受影响。

### 2.3 Branch prompt

创建 prompt 说明 W 的长期职责；后续 `update` 是一次有身份和 revision 的委托更新，经普通 input 采用，并纳入压缩后仍有效的分支委托。它不修改 System，不安装另一套模型配置，也不清除已经发生的动作。

建议的 watch prompt 包含以下要求，不强制模型输出一份专有 JSON 报告：

```text
Watch the parent task against the user's objective and constraints.
Read the changed history before judging it. Cite concrete evidence.
Distinguish normal exploration, missing evidence and actual drift.
Report a new concern or a material change; do not repeat unchanged advice.
State the last history position inspected and any coverage gaps.
Use the agreed reporting route. Do not take over the parent's work.
Finish each check and wait for new input. End when the assignment ends.
```

默认在 W 的回答中提醒用户。正常检查只留下简短检查位置，不主动给主线发进度消息；客户端展示新回答，不解析自然语言来宣称发现了确定故障。用户授权自动建议时，brief 允许 W 通过现有父子消息向主 agent 发送建议。W 与主线共享 Session 的模型、工具、权限和工作区；这些行为要求是提示词约束，不是安全隔离。

## 3. 订阅、采样与分析

订阅的是“父分支出现了值得检查的新活动”，不把每条 Timeline 事件转成一次模型调用。EventHub 不做采样；活动生产者在发布前合并信号，W 再通过 `branch read` 取证。

```text
B committed facts
  -> B's activity producer: coalesce checkpoints
  -> EventHub: match W's subscription, deliver ordinary input
  -> W's Loop: run a turn
  -> branch read B: bounded history pages as tool results
  -> W: inspect, optionally report, finish the turn
```

1. **先订阅，再补读。** W 创建后订阅 B 的活动源，订阅成功后从自己的切点补读到当时的 head，覆盖创建到订阅之间的窗口。输入中的水位只是提示，实际读取位置以工具结果为准。
2. **生产者做轻量合并。** 只看自己 B 的已提交事实：新的用户要求、完整模型步骤、工具结果、turn 结束等；不逐条转发输出 delta。不把 watcher 发回的消息或纯状态刷新再次计作活动，避免仅靠通知互相唤醒。
3. **候选频率为 [GUESS]。** 有新材料时，约每 3 个完整步骤或 30 秒发布一次检查点；目标变更和 turn 结束可提前检查。无新材料不发布，不为空闲 W 设置心跳。计数与水位从 B 的事实重建，重载不重放历史发布。
4. **事件带位置，读取带内容。** 检查点正文只说明来源和截止位置；W 每轮先读约 2k token 的增量，必要时扩到约 4k，均为 [GUESS]。长工具输出只摘取相关部分，必须说明截断；材料不足可以继续读取，但额外工具和模型请求计入开销。
5. **允许不完整覆盖。** W 继承切点前的有效上下文，之后读取父线公开历史。不能把摘要、预览或局部页说成完整检查。报告说明检查到哪里、证据是什么、哪些结论仍不确定。
6. **合并不等于背压保证。** 活动输入建议用 `queue`，让本轮完成后处理；Loop 串行执行 W，但 EventHub 不负责清除已入队的旧检查点。积压时 W 以最新水位取材，不保证“一事件一评估”，也不承诺严格的一轮在途上限。

EventHub 没有持久待投队列。漏掉一个提示会延迟检查；下一次唤醒按已读游标补取历史。重载、订阅失败或源停用期间显示检查缺口，不宣称连续覆盖；没有下一次活动时需要用户或主 agent 再发一次检查请求。恢复不重跑已经启动的模型请求。

### Token 与缓存

持续保留上下文有利于复用相同请求前缀，但模型每次请求仍带上其有效上下文。缓存命中会影响实际计费，不能从逻辑输入 token 中扣掉，也不能预先保证命中。

**[GUESS] 算例：**初始前缀 12k，每轮新增材料 2k、回答 0.5k，共 10 轮、每轮一次模型请求且不压缩：输入合计约 252.5k，输出约 5k。若首轮冷缓存，之后完整命中上轮已发送的输入，约 216k 是缓存读取，36.5k 是普通输入；这是理想情景，不是价格或实测承诺。

展示 W 自身的 `model/usage`，以及压缩记录中实际返回的用量；继承来的父线历史不重复记作 W 已发生的消费。额外取证后的模型请求、重试、压缩都增加消耗；缓存字段缺失显示未知。先复用普通 branch 用量和结束控件，不新增 watcher 预算账本，也不承诺 prompt 能执行硬 token 上限。模型选择沿用 Session 合同。

## 4. 提醒与纠偏

| 路径 | 如何交付 | 谁决定下一步 |
| --- | --- | --- |
| 提醒用户 | W 的普通回答保留证据与建议；客户端在被关注的线显示新回答和未读提示 | 用户查看、更新 brief、结束 W，或把建议发给主 agent |
| 用户发送建议 | 使用现有主线输入入口，包含来源 W 与证据位置 | 用户选择现有输入方式；主 agent 执行 |
| 自动建议 | W 使用 `branch.message` 发给父线，默认 `append` | 主 agent 在下一次模型请求核对当前目标与事实，再决定是否采纳 |
| 严格完成验收 | evaluator 按 Goal 的协议返回结论 | Goal 验证来源、目标版本并决定完成、续跑或阻塞 |

自动建议不替用户批准工具，不直接回滚，也不在主线空闲时主动启动工作。`append` 本身不唤醒主线；需要立即打断时由用户通过已有控制操作处理。相同问题没有新证据时不重复发送，主 agent 不因 watcher 的建议改写用户目标。

建议是异步消息，不承诺与父线最新状态原子绑定。已交接的输入不能召回；更新 prompt、取消订阅或结束 W 都不能抹掉已经发出的建议。是否仍适用由接收方检查。这一限制适用于普通分支消息，无需另造 report、receipt、finding 或 feedback 状态机。

## 5. 事件与跨 branch 边界

Timeline / Dispatcher 负责所属分支的事实与执行；EventHub 负责活动订阅交付。两者不是同一种事件系统。下表仅列本方案实际用到的交界，字段完整合同仍由原所有者维护。

| 事件或记录 | 关键字段 | 生产者 → 消费者 | 复用 / 新增 |
| --- | --- | --- | --- |
| `branch/create`、`branch/create-failed` | 分支身份、切点、`source`、创建结果 | TimelineTree → 创建方、目录、客户端 | 复用，开线不经 EventHub |
| `input/message`、`input/applied`、`input/consumed` | 输入身份、内容、mode、sequence | 输入所有者 / Loop → 目标 branch 的上下文与执行 | 复用；活动提示、prompt 更新和消息最终都走此路径 |
| `step/model-output-end`、`step/tool-result`、`turn/end` | 请求或 turn 身份、结果、sequence | B 的执行部件 → B 的活动生产者 | 复用，合并检查点；生产者不折叠 W 的历史 |
| `<source>.activity` | 发布 `id`；`attributes: {sessionId, branchId, through}`，值均为字符串；简短 `title/body` | B 的通用活动生产者 → EventHub → 订阅 W | **新增 EventHub 生产者类型**，不是新增 Timeline 类型；through 表示已提交水位 |
| `dim-event` 输入标记与交付回执 | source、event ID、属性；回执的 input ID、sequence 或失败 | EventHub → W / 发布方 | 复用；收到输入不等于完成分析 |
| `dim-branch-message` | `from, to` 与普通消息内容 | W → 主线 | 复用父子消息；不新增通用发布订阅报告通道 |
| `model/usage`、`context/compaction` | 实际用量、可用缓存字段、压缩用量 | W 执行部件 → 用量展示 | 复用，不解析回答估算实际账单 |
| `branch/end`、`branch/ended` | 来源、目标、结束结果 | 分支 / TimelineTree → 目录、创建者 | 复用；订阅清理由 EventHub 自己完成 |
| branch 委托记录 | `purposeKind, prompt, idleNotice`，revision 取提交位置 | 通用 branch 管理所有者 → 本线上下文、管理读取 | **通用合同需求**；建议复用有 key 的 adopted input，不覆盖已有分支身份，不新增 Side Eval 事件族 |
| `dim-branch-idle` | branch、turn 结果，投递 mode | W 的 BranchLine → 主线 | 复用标记；**新增通用通知开关**，`none` 不写此输入，默认仍为 `steer` |

活动 source 必须由真实生产者 row 声明并持有。提议命名为 `b-` 加 `SHA-256` 摘要前 30 个小写十六进制字符，共 32 个 ASCII 字符；摘要输入是持久 `sessionId, branchId` 组成的紧凑 JSON 字符串数组的 UTF-8 字节。它满足 EventHub 名称约束，恢复后稳定，也区分复制到不同 Session 的同一 branch ID。生产者登记与本线 `get.activitySource` 使用同一派生值；只有声明可用时才报告可订阅，实际身份仍由发布属性指明。碰撞或别的 row 占用时按 `duplicate_source` 报错并显示源不可用，不改名冒用。此命名合同由第 7 节的生产者所有者落实。

W 的订阅限定为允许观察的 B：精确类型，加 `match` 中的 Session 与 branch 身份，目标是 W。注册入口核对父子关系和实际调用者，过滤条件本身不是授权。仅发布水位，不广播父线正文；内容读取仍受现有 `branch read` 限制。

本提案建议父子任务协商、W 对父线的定向建议和请求子线结束继续使用 [Timeline branches](../timeline-branches.md) 已有的消息协议：它们面向已确定的协作方，依赖父子授权和接收方响应，而非寻找订阅者。活动提示仍走 EventHub，进入 W 后再读取历史，不传输完整采样正文。**这是待所有者确认的边界，不是 EventHub 已有的明确豁免**；其 Routing principle 仍需按第 7 节对齐。

## 6. Pursuit 是哪一种 side branch

**Pursuit 的 evaluator 是 side branch 的一种用途；Pursuit 整体仍是 Goal 的完成判定策略。** 统一的是分支执行、提示词、查看和生命周期，不能把“完成判定权”简化成一个 `evaluate` 标签。

| 用途 | 触发 | 输出 | 状态决定者 |
| --- | --- | --- | --- |
| Watch | 任务活动与用户检查请求 | 偏移疑点、证据、建议 | 用户或主 agent，结果是建议 |
| Pursuit evaluator | worker 宣称完成，Goal 按既有条件发起 | `DONE / CONTINUE / BLOCKED` 或失败 | Goal，结果参与正式任务状态转换 |

Pursuit 可以进入同一个 side branch 列表，使用同样的详情与关注控件；它不必变成长期订阅者。Goal 继续创建 host evaluator、验证结论来源和目标版本、处理失败与迟到结论、结束 evaluator。普通 watch 输出不能完成 Goal。详见 [Pursuit](../plugins/Pursuit.md)。

## 7. Requirements raised to mechanism owners

以下要求由 #1312 跟踪。本文不修改其他所有者的设计；实现前由所有者在其合同内闭环。

| 所有者 | 现有机制及缺口 | 通用要求与影响 |
| --- | --- | --- |
| TimelineTree / BranchLine | 已有创建、消息、读取、结束、目录和 purpose；缺少用途字段、可更新委托与统一 get/update；当前空闲通知固定唤醒主线 | 定义用途、prompt、`idleNotice` 的所有权、输入记录、revision、压缩保留和调用范围；委托经本线 input 顺序采用，重载从事实恢复，通知策略在首轮运行前确定；不为 watch 添加核心分支模式 |
| BranchLine 的活动生产者 | 已有父线 head 提示不会唤醒空闲 W；EventHub 不自动镜像 Timeline，也不负责合并 | 发布通用、有界的活动水位，声明 source、目录与读取授权；依赖本线 Timeline/Dispatcher 和 EventHub；仅在分支发布后工作，关闭时取消，重载不补发旧事件；丢失通过后续水位和历史读取补偿 |
| EventHub | 已有订阅、过滤、投递、回执及客户端管理；模型入口尚需其合同闭环；Routing principle 未明确父子定向协议的边界 | 完成模型订阅入口，W 只管理自己的订阅；订阅读取沿用其公开入口。与 TimelineTree 所有者确认下述定向协议范围，不增加 Side Eval 特例、订阅表、全局队列或重放 |
| CLI / 客户端 | 已有 Lines 和分支操作，没有 watch 入口及关注展示 | CLI 按菜单规则加入 `/watch`；TUI 与其他客户端共用 branch 详情及 EventHub 管理读取，不另算用途或订阅状态；无对应入口或权限时显示不可用，UI 未读态不成为业务事实 |
| Goal / Pursuit | 已有 evaluator host branch 和严格结论协议 | 可提供 evaluate 用途信息用于统一展示；保持判定协议及 Goal 状态所有权，不把它迁为 watch 模式 |

Side Eval 本身无需独立 Capability。需要扩展的 BranchLine 仍是本线 Capability，TimelineTree 仍拥有 Session 的分支生命周期；EventHub 拥有自己的订阅，Goal 拥有自己的判定。依赖按 [plugin contract](../plugin-contract.md) 与 [rutis usage](../rutis-usage.md) 声明，不增加跨 branch 服务注入或 Session 中的业务状态副本。

**路由对齐是实现条件。** EventHub 的 Routing principle 覆盖交付型输入，现有 `branch.message` 则直接向固定父子线写输入。提议由 EventHub 与 TimelineTree 所有者在 #1312 确认：父子协作中的任务协商、定向建议、结束请求保留既有消息协议，面向订阅者的活动事件走 EventHub。现有消息已经提供父子授权与响应关系；改为 publish 会引入显式目标与订阅目标的并集，不能当作等价替换。若采纳本提议，序号、批次、中断与恢复均沿用原路径；若要求迁移，先由所有者明确受众、授权、mode、回执与恢复差异，再调整本提案。确认前不能将自动建议的直接路径作为已获批准的实现合同；本文件不修改 EventHub 的规则。

## 8. Rule check

规则依据：[principles §1、§4](../../architecture/rust-agent/principles.md)、[ADR 002 D1–D5](../../adr/002-plugin-scope.md)。

| 规则 | 本方案 |
| --- | --- |
| §1 Capability | Side Eval 是入口与 prompt 用法；事实消费属于 BranchLine、Loop、Goal 等真实所有者，App Server 只提供入口 |
| §4.1 指令与结果 | 管理和消息通过所有者公开入口提交；目标状态由目标所有者决定；无变化不写结果事实 |
| §4.2 交接 | 输入采用后不召回；取消订阅不能撤销已交付输入 |
| §4.3 before | 不使用；采样与网络等待不在提交锁中 |
| §4.4 核心语义 | 核心不识别偏移或 watch 判断；用途标签不授予能力 |
| §4.5 批次 | 沿用本地输入批次；跨 branch 不承诺原子检查与纠偏 |
| §4.6 上下文 | 仅采用输入和工具结果；活动水位不直接导入父线上下文 |
| §4.7 公开交互 | 不折叠其他分支私有状态；读取与消息沿用父子合同 |
| §4.8 机制需求 | 通用管理、活动源和定向协议的路由边界列于第 7 节，由所有者确认后实现 |
| §4.9 最少事实 | 不新增 Side Eval 事件族；用途/委托保留恢复所需事实，UI 未读仅为展示 |
| ADR 002 D1 | 委托与读取状态属于本线，分支生命周期属于 Session，订阅属于 EventHub |
| ADR 002 D2 | 不设 watcher 调度、报告转发或接线插件 |
| ADR 002 D3 | 通过已声明服务向外依赖；无 branch 互相注入，无 Session 向下装配依赖 |
| ADR 002 D4 | brief 与采样参数是业务数据，不进入 loader rows |
| ADR 002 D5 | 接口与字段语言中立；本提案不增加绑定 API |

## 9. 验证与删减

实现时验证五条用户流程：创建并成功订阅后开始检查；主线持续运行时有界采样并标明覆盖；W 的提醒和自动消息可追溯，检查结束不写自动空闲输入；合法修改/结束正确生效，子线不能越权读取或改写兄弟线/父线委托，空闲 W 能收到 steer 结束请求；Pursuit 在同一列表中仍只有 Goal 能判定任务状态。另核对创建摘要与当前委托的区分、读取游标而非分析结论、短 source 名称及碰撞拒绝、断线恢复、订阅失败和实际 token/缓存统计。

保留的只有普通 branch、订阅、委托与已有消息；用途和状态用于管理，检查点用于避免逐事件调用模型。空闲通知选择 `none` 而非 `append`：父线无需每轮消费“正在等待”，客户端可直接读执行状态，因此连同无用途的输入事实一起省去。无需独立 watcher ID、专属工具、报告存储、疑点状态机、费用账本或跨线报告服务。独立只读权限、严格纠偏准入、自动回滚和硬 token 配额不属于本方案，不能由提示词暗中承诺。
