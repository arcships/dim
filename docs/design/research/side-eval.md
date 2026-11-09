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
  W      watch: API compatibility idle     last checked: #184
         [Open] [Edit prompt] [Subscriptions] [End]

W: Possible API break at #192. Evidence: ... Suggested check: ...
   [Open evidence] [Send to main]
```

`Tab` 补全、`Enter` 提交，其他行为遵循 [CLI command menu](../cli-commands.md#command-menu)。关注、打开和未读提示复用 Lines，不新增 watcher 仪表盘。客户端可以在任何已展示的辅助分支上使用同一套关注控件。

主 agent 组装 brief：用户目标及来源、关注范围、何时提醒、是否允许直接发送建议、何时结束。不能把自己的目标概括当作用户新增授权。用户明确要求观察时安排 W；主 agent 自行创建时沿用已有分支权限，并说明用途。能在当前任务中直接核对的事情就由主 agent 自己做；需要独立上下文和持续关注时才开 W。

首期 agent 创建沿用现有 `branch` 工具的范围：`main` 创建自己的 tool line，W 不再开子 watcher。客户端或插件创建的 host line 继续由其创建者管理；不因被显示为 side branch 就允许主 agent 接管。

### 2.2 四个管理动作

保留 `create`、`get`、`update`、`end` 四个逻辑动作，归入通用 branch 管理，不再提供独立 `side_eval` 工具。以下是目标合同；当前工具仍是 `create/message/read/end`，缺口列在第 7 节。

| 动作 | 输入与结果 | 行为 |
| --- | --- | --- |
| `create` | `prompt`，可选 `purposeKind, idleNotice`；返回真实 `branchId` | 使用现有分支创建；watch 是主 agent 选择的 brief，不是核心特殊模式 |
| `get` | `branchId`；返回身份、用途、当前 prompt 及 revision、生命周期、执行状态、可用的 `activitySource`、订阅与用量 | 只读；列表复用 branch directory，详情按所属服务读取 |
| `update` | `branchId, prompt, expectedRevision`；返回新的 revision | 只更新该线的委托；相同内容不写新事实，revision 冲突返回当前版本供重读；订阅通过 EventHub 自己的入口修改 |
| `end` | `branchId`；返回 `ending` 或 `ended` | 使用现有结束协议，保留历史并停止接收新输入；订阅通过 unsubscribe 或后续投递的 gone 结果清理，不把结束当作任务验收通过 |

入口只管理其有权管理的线。未知分支、已结束分支、版本冲突、越权或依赖不可用都明确返回；不悄悄新建替代 watcher。工具的 branch ID、revision、订阅 ID 和错误必须出现在 `ToolResult.content`。

状态不另起一套：生命周期使用 `open / ending / ended`；当前执行显示 Loop 的状态；订阅显示 EventHub 的记录及失败。`idle` 表示本轮结束，不能显示成观察已停止；无订阅、源不可用和从未检查不能显示成正在正常监控。最后检查位置来自 W 实际读过的材料，不以订阅成功或发布水位代替。

当前主 agent 通过消息请求 tool line 自行结束；客户端通过现有结束入口操作。直接按 ID 结束自己的 tool line 属于通用工具扩展，在所有者接受之前不假装已有。host evaluator 的结束仍交给 Goal。

**空闲通知需要一个通用选择。** 当前 tool line 每轮结束时，如果本轮没有向主线发送过 `steer`，BranchLine 会自动发 `steer` 空闲通知，普通 `append` 建议不能阻止它。建议创建时可指定 `idleNotice: append`；省略保持现有 `steer` 行为。watch brief 使用前者，让检查完成只更新主线下次请求可见的状态，不唤醒主线。该设置由 BranchLine 执行并持久化，不能靠 prompt 模拟，也不由 `purposeKind` 隐式决定。

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
| `dim-branch-idle` | branch、turn 结果，投递 mode | W 的 BranchLine → 主线 | 复用标记；**新增通用投递策略**，watch 选择 `append`，默认仍为 `steer` |

活动 source 必须由真实生产者 row 声明并持有。多个 branch 实例不能冒用同一个 row 的 source；稳定命名、目录暴露和恢复由活动生产者所有者确定，不能先写死一个所有分支共享的 `branch.activity`。

W 的订阅限定为允许观察的 B：精确类型，加 `match` 中的 Session 与 branch 身份，目标是 W。注册入口核对父子关系和实际调用者，过滤条件本身不是授权。仅发布水位，不广播父线正文；内容读取仍受现有 `branch read` 限制。

跨线报告继续走 [Timeline branches](../timeline-branches.md) 已有的消息协议。EventHub 显式保留这条现有路径，Side Eval 无需为它提出专用豁免。提示通过 EventHub 进入 W 后再读取原始历史，也不依赖 EventHub 传输完整采样正文、排他投递或原子反馈批次。

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
| EventHub | 已有订阅、过滤、投递、回执及客户端管理；模型侧入口不能仅凭其出现在设计文档就视为可用 | 完成其所属的模型订阅入口，按调用范围验证目标；W 先管理自己的订阅。重用订阅存储和结束清理，不新增 Side Eval 订阅表；不要求全局队列、背压或历史重放 |
| CLI / 客户端 | 已有 Lines 和分支操作，没有 watch 入口及关注展示 | `/watch` 将 brief 交给主 agent；按固定命令菜单规则加入；展示用途、真实执行/订阅状态、新回答及检查位置，UI 未读态不成为业务事实 |
| Goal / Pursuit | 已有 evaluator host branch 和严格结论协议 | 可提供 evaluate 用途信息用于统一展示；保持判定协议及 Goal 状态所有权，不把它迁为 watch 模式 |

Side Eval 本身无需独立 Capability。需要扩展的 BranchLine 仍是本线 Capability，TimelineTree 仍拥有 Session 的分支生命周期；EventHub 拥有自己的订阅，Goal 拥有自己的判定。依赖按 [plugin contract](../plugin-contract.md) 与 [rutis usage](../rutis-usage.md) 声明，不增加跨 branch 服务注入或 Session 中的业务状态副本。

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
| §4.8 机制需求 | 通用管理与活动源缺口列于第 7 节，由所有者设计，不假装已有 |
| §4.9 最少事实 | 不新增 Side Eval 事件族；用途/委托保留恢复所需事实，UI 未读仅为展示 |
| ADR 002 D1 | 委托与读取状态属于本线，分支生命周期属于 Session，订阅属于 EventHub |
| ADR 002 D2 | 不设 watcher 调度、报告转发或接线插件 |
| ADR 002 D3 | 通过已声明服务向外依赖；无 branch 互相注入，无 Session 向下装配依赖 |
| ADR 002 D4 | brief 与采样参数是业务数据，不进入 loader rows |
| ADR 002 D5 | 接口与字段语言中立；本提案不增加绑定 API |

## 9. 验证与删减

实现时验证五条用户流程：创建并成功订阅后开始检查；主线持续运行时有界采样并标明覆盖；W 的提醒和自动消息可追溯，检查结束不会因空闲通知唤醒主线；修改/结束与迟到交付不伪造成功或丢掉历史；Pursuit 在同一列表中仍只有 Goal 能判定任务状态。另核对断线恢复、订阅失败与实际 token/缓存统计，不能仅用 prompt 示例证明行为成立。

保留的只有普通 branch、订阅、委托与已有消息；用途和状态用于管理，检查点用于避免逐事件调用模型。无需独立 watcher ID、专属工具、报告存储、疑点状态机、费用账本或跨线报告服务。严格纠偏准入、自动回滚和硬 token 配额不属于本方案，不能由提示词暗中承诺。
