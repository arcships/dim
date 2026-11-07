# Goal

总体设计：[overview §4](../overview.md)。阶段：P2。语言：Rust（[overview 附录 C](../overview.md)）。作用域：Session + 分支（[ADR 002](../../adr/002-plugin-scope.md)）。依赖（[路线图](../../../design/roadmap.md) P5）：C1、C3、C4、C5、S1、S8b（[#118](https://github.com/arcships/dim/issues/118)）。

需求（[功能列表](../../../feature-list.md)，分支 Agent 行为）：`/goal` 设置、编辑、暂停、恢复和清除目标（"切换 Goal 模式"）；Goal 续跑朝明确的目标继续工作，并记录完成或受阻。

Goal 在分支上保持一个常驻目标，并自动续跑该分支，直到模型报告目标已完成或受阻、用户暂停、清除或中断，或达到显式设置的续跑上限。Goal 是分支上的事件驱动状态机。**Goal 的状态是一条 `goal:state` 块**：一条普通的 `input/message`，带一个带 key 的 raw 块，Append，只由 Goal 的分支部分写入，因此模型、客户端与折叠读到同一个事实（§2、A.5）。**变更是一个触发器，不是状态陈述**：`session.goal.*` 的 `set`、`edit`、`pause`、`resume`、`clear` 与模型 `goal_update` 工具的 `completed` / `blocked` 各经 `treeCommands.inputs`（即 `Branch::inputs`）提交一个原子批次、两条输入：回答它的状态记录，然后是触发器；触发器是一个无 key 的块，说明被要求了什么；Goal 把每个触发器的记录写进同一批次、紧排在其前。记录出现之前写成的历史带旧 key `goal:objective` 与 `goal:status`，按旧规则折叠；其上第一个被回答的触发器写入一条记录。**一个目标只属于一个分支**：Goal 对分支 `B` 的折叠只使用写在 `B` 自身上的事件（从 `B` 的 `branch/start` 起的本地后缀），从不使用继承的前缀，因此子分支和分叉 Session 的 `main` 都从没有目标开始；若新分支继承的前缀中仍有存活的目标 key，它用自己的一次起始 clear 退役它（[§3.4](#34-新分支清除继承的目标)）。续跑不带 key，也不是变更。工具和方法只写触发器；它们不读 Goal 的状态，也不判断变更是否允许。**Goal 的折叠就是最新记录陈述的状态**，按序号顺序，用固定规则计算（[§2](#2-数据与状态)）。**分支部分**拥有这个折叠结果；它的 Dispatcher 处理器消费 `turn/end`、`stop/interrupt`、`input/deleted` 或 cancelled 的 `exec/ended` 时，只依据已消费的状态自己决定是否续跑；决定需要写入（续跑、状态记录或自动暂停）时，处理器把写入登记为自己拥有的工作，然后返回。**Session 部分**把 `goal_update` 工具注册到本 Session 的 `toolRegistry`，并把命名空间 `session.goal` 注册到本 Session 的 `sessionMethods`，以此提供 `session.goal.*` 方法（[session-boundary](../contracts/session-boundary.md)）；每次调用按分支 id 经 `treeCommands.inputs`（即分支的公开多输入入口 `Branch::inputs`）把它的记录与触发器作为一个原子批次提交到调用方分支或指定分支。没有调用读取折叠结果：Session 部分从不向分支部分询问任何东西，Session 部分也不能依赖每个分支各一份的服务（ADR 002 D3）。Goal 不写自己类型的事件，也没有单独的"Goal 模式"：目标的存在与状态可通过分支上存活的 goal 块看到（§6），覆盖了功能列表中"切换 Goal 模式"的说法。本文的每个契约都是语言中立的（服务名、作用域、key、带数据形状的方法、工具注册）；Rust 绑定见[附录 G](#附录-g-rust-绑定)。Goal 通过公开的 Rust 导出使用运行时服务：Session 部分用 `TreeCommands` 和 `ToolRegistry`，分支部分用 `Dispatcher`、`Timeline`、`TimelineIdentity` 和 `TreeCommands`。

## 1. 结构

```mermaid
flowchart LR
  subgraph PROC["进程"]
    Rpc["Rpc"]
    Settings["Settings 来源 goal"]
  end
  subgraph SESS["Session 作用域"]
    GS["GoalSession：工具 goal_update，sessionMethods session.goal"]:::new
    TR["ToolRegistry"]
    TC["TreeCommands"]
  end
  subgraph BR["分支作用域"]
    GB["GoalBranch（goal：折叠、决策）"]:::new
    TL["Timeline + Dispatcher"]
    IQ["InputQueue 与 Loop"]
  end
  GS -- "注册 goal_update" --> TR
  Rpc -- "解析 Session，调用 sessionMethods session.goal" --> GS
  GS -- "每次调用：treeCommands.inputs(branchId, [记录, 触发器])" --> TC
  TC -- "Branch::inputs：kind=state 记录（goal:state，Append）然后 kind=goal 触发器（无 key），一个原子批次" --> TL
  TL -- "按顺序 branch/start、input/*、turn/*、stop/interrupt" --> GB
  GB -- "自有写入任务：turn end 或删除之后的记录、续跑或起始 clear" --> TL
  GB -- "turn/end 决策：settings.read maxContinuations" --> Settings
  TL --> IQ
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
```

所有模型可见的效果都是由现有 InputQueue 和 Loop 消费的 `input/message` 事实。

| 部分 | 作用域 | 依赖 | 提供 |
| --- | --- | --- | --- |
| `GoalSession` | Session | `TreeCommands`、`ToolRegistry`、`SessionMethods`、`GuideEntries`（Session 导出） | 把 `goal_update` 注册到 `ToolRegistry`，把 `session.goal` 注册到 `SessionMethods`；经 `TreeCommands::inputs` 按分支 id 把每次变更作为一个原子批次提交（记录在前，触发器在后） |
| `GoalBranch` | 分支 | `Dispatcher`、`Timeline`、`TimelineIdentity`、`BranchLifecycle`（分支导出）、`SessionLifecycle`（Session 导出，用于加载时核对）、`TreeCommands`、用于其来源 `goal` 的进程服务 `SettingsService`（S8b） | 不提供服务；拥有折叠结果、续跑决策、状态记录和 Goal 自己的写入 |

Session 部分在 apply 中把工具注册到 `ToolRegistry`，把方法命名空间 `session.goal` 注册到 `SessionMethods`。分支部分不提供服务；它作为分支定义注册，因此每个分支都有一份副本（C5）。安装了 Goal 的装配的每个 Session 都运行 Goal 的 Session 部分，因此该部分激活后，每个这样的 Session 都有 `session.goal`（[session-boundary A.1](../contracts/session-boundary.md#a1-方法贡献)）。`maxContinuations` 是 Goal 的 Settings 来源 `goal` 中文档的可选字段，属于业务配置（[ADR 002](../../adr/002-plugin-scope.md) D4）。Goal 只通过 S8b 的进程服务 `settings` 读取它（[Settings 服务](../appserver/settings-service.md)）：每次使用时调用 `SettingsService::read_declared`，经其公开的 Rust 绑定。Goal 的行在其 `apply` 中按 S8b §3.1 登记该来源：`source: "goal"`，一个用户文件，没有项目文件（只有用户作用域），没有 user-only 字段，JSON Schema 中 `maxContinuations` 为整数，`minimum: 1`、`maximum: 1000`，没有默认值（A4）。字段缺失表示没有上限；`null` 不是合法值。Settings 在写入和读取持久化文件时校验，因此 Goal 按交付的样子使用文档；变更是数据更新，从不重载。Dispatcher 回调登记自有任务后即返回，不等待 Settings。分支发布后，每个 turn/end 任务使用时调用 `settings.read {source: "goal", scope: "user"}`，再在写入前复核其决定轮次及当前已消费的折叠结果（A.6）。不缓存上限，也不监听 `settings.changed`。在该读取前已提交的变更无须重载即生效，包括文件后端 Settings 用户文档的直接编辑。来源不可读时停止该次决策，不产生自动输入，也不使用回退上限（附录 C）。中断暂停与继承目标的起始 clear 不需要读取上限。Goal 的工具都不写它；客户端经 `settings.read`/`settings.update` 写入它。Goal 只读取 `branch/start`、`input/message`、`input/consumed`、`input/deleted`、`turn/start`、`turn/end`、`stop/interrupt`、`exec/started` 和 `exec/ended` 的公开数据契约，并在 turn/end 决策时读取结束回合的一页有界 `step/tool-call`（A.4 C7）；从不读取其他插件的状态。

## 2. 数据与状态

```mermaid
flowchart TB
  subgraph TLB["分支 Timeline：从 branch/start 起的本地事件（继承前缀不参与折叠）"]
    TR["变更触发器：input/message，一个 kind=goal 块，无 key（set、edit、pause、resume、clear、goal_update）；在同一原子批次中紧随其记录"]
    SR["状态记录：input/message，一个 kind=state 块，key goal:state，Append；回答批次中的变更时按 submission id 写名它的触发器"]
    CI["Goal 自己的 input/message：续跑（无 key）"]
    OI["操作者 input/message（用户内容，无来源标记）"]
    IC["input/consumed、input/deleted"]
    TE["branch/start、turn/start、turn/end、stop/interrupt、exec/started、exec/ended"]
  end
  CFG["Settings 来源 goal：maxContinuations"]
  VIEW["折叠结果：加载时建立，每次 Dispatcher 投递时推进"]:::new
  TR --> VIEW
  SR --> VIEW
  CI --> VIEW
  OI --> VIEW
  IC --> VIEW
  TE --> VIEW
  CFG --> VIEW
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
```

Goal 不拥有自己类型的持久状态。分支 `B` 的目标是 `B` 本地事件中最新的存活 `goal:state` 块所陈述的状态；在记录出现之前写成的历史上，则是按下面规则折叠的两个旧 key。只有 Goal 的分支部分写这条记录。它是一条普通的 `input/message`，带一个 key 为 `goal:state` 的 raw 块，模式 Append，因此模型、客户端与 Goal 的折叠读到同一个事实：模型经 `render_input`，客户端经分支历史或通用带 key 投影，折叠经其 Dispatcher 订阅。没有为它特判任何规则或 ADR：这个块像其他被采用的输入一样进入模型上下文，压缩也像其他 key 一样经带 key 的基线重放它。

**变更**是触发器，不是状态陈述。`session.goal.set/edit/pause/resume/clear` 与 `goal_update` 经 `TreeCommands::inputs`（`Branch::inputs`）提交一个原子批次、两条输入：回答该变更的状态记录，然后是触发器，其唯一的 raw 块 `kind="goal"`，说明被要求了什么（A.5），模式不变。记录排在前面，因此无论调度如何，InputQueue 都先应用记录、再应用触发器：触发器的轮次开始时，记录已经应用。分支部分在自己的写锁下撰写记录，且要等折叠已推进 Goal 自己的上一次写入，因此记录基于它之前的状态陈述该变更。记录按 submission id 在 `trigger="goal:<uuid>"` 中写名触发器，而不是 `after`（A.5）；折叠把这样的记录暂留一个事件，当下一个事件正是写名的触发器时，在触发器的序号处先应用变更、再应用记录（A14）。给定 `after` 的快照只写一次。没有记录的触发器——更早版本写成的历史，或批次之外写入的触发器——在加载时被回答一次（§4.2）。被 InputQueue 删除的变更触发器从未生效：折叠去掉它重新计算；只要已有写名该触发器的记录或任何更晚的记录曾被写入或正在被写入，Goal 就写一条补偿记录，其 `after` 是 `input/deleted` 的序号，陈述分支去掉该触发器后的状态。是否存在这样的记录在写锁下决定，因此在删除被折叠期间提交的记录会被补偿记录取代，绝不会成为最新的存活记录。

折叠在变更 `input/message` 的序号处应用它，并在同一条序号处应用批次中的记录（A14），因此没有任何决策会看到没有记录的触发器。它忽略三类记录：`after` 写名已被删除的变更的、比已折叠的最新变更更旧的、以及比已应用的最新记录更旧的；Goal 也从不撰写 `after` 低于自己已写记录的记录，因此无论写入任务的提交顺序如何，最新的存活记录总是陈述最新的状态。**模型**在受该变更影响的第一个请求处得知它：记录在同一原子批次中排在触发器之前提交，且 InputQueue 按序号顺序应用输入，因此携带触发器的第一个请求已经带着排在它之前的记录。Queue 命令仍驱动自己的轮次（A14）。之后被 InputQueue 删除（`input/deleted`）的变更被移除，折叠去掉它重新计算（[session access](../contracts/session-access.md)）。

**旧历史。** 最新 Goal 块是旧 `goal:objective` 或 `goal:status` 块的分支，按下面记录出现之前的规则折叠，保持不变，加载时不回填记录。该分支上第一个被回答的变更触发器写入一条 `goal:state` 记录，此后记录就是生效状态；同一条输入退役分支上仍然存活的旧 key，此后分支只保留 `goal:state`——在其之后复制的压缩基线不会带任何过期的旧块，`session.keys` 也只剩一个 key。客户端以同样方式回退（§6）。该回退为记录之前写成的历史保留；当再没有这样的历史可读时移除。

只有在目标文本存活时目标才存在。在 [Pursuit](Pursuit.md) 的评审裁判下，干活模型的 `completed` 是请求，不是状态变更（A16）：Goal 在它之后写的记录仍陈述 `active`，因此模型与客户端无需自己的例外。

| 变更（标记中的 `op`） | 写入方 | 触发器块 | 折叠陈述的记录 |
| --- | --- | --- | --- |
| `set {objective, judge?}` | `session.goal.set` | `op="set"`，正文带目标文本，选择时带 `judge="evaluator"` | `status = active`、目标文本、`judge`；计数重新开始 |
| `edit {objective}` | `session.goal.edit` | `op="edit"`，正文带目标文本 | 目标文本被替换，`judge` 与状态保留（目标没有状态时为 `none`） |
| `pause`（原因 `user`） | `session.goal.pause` | `op="pause" reason="user"` | `status = paused (user)`，无论之前是什么 |
| `resume` | `session.goal.resume` | `op="resume"` | `status = active`，无论之前是什么（`completed` 或 `blocked` 的目标重新打开）；计数重新开始；这条 Queue 输入驱动一轮 |
| `clear` | `session.goal.clear` | `op="clear"` | 没有目标、没有状态，也没有 `judge`（一条退役记录） |
| `clear`（原因 `new_branch`） | Goal 自己，在新分支上（§3.4） | — | 一条退役记录；同一条输入还退役继承前缀留下的旧 key |
| `completed` / `blocked {summary}` | `goal_update` | `op="completed"` / `op="blocked"`，正文带 summary | `status = completed` / `blocked`，带 summary。在 [Pursuit](Pursuit.md) 的评审裁判下，干活模型的 `completed` 是请求，状态不变（A16） |
| `pause`（原因 `interrupted`、`limit`、`turn_failed`、`no_progress`、`evaluation_failed`） | Goal 自己 | — | `status = paused (reason)`；Goal 只在 C1 成立后写它（A.4），并在 `after` 中写名做出决定的事实 |
| 续跑（不是变更） | Goal 自己 | — | 只计数：计数加 1；其 `idle` 记录 C7 的连续停滞数 |

基于已消费事件的推导值：

| 字段 | 推导 |
| --- | --- |
| `goal` | 最新的存活状态记录陈述目标文本时为 `{objective, status, reason?, summary?}`（旧历史上则是本地 `goal:objective` 块存活时），否则不存在；记录没有状态时 `status` 为 `none` |
| `A` | 最近一次设置 `status = active` 的变更（`set` 或 `resume`）的序号 |
| `interrupted` | `B` 本地事件中序号大于 `A` 的 `stop/interrupt {scope: turn}` |
| `inherited` | 尚无本地目标块，且 `B` 开场事件之前的前缀中有存活的 `goal:state` 块或存活的旧 key，按每个 key 判断（§3.4） |
| `last_operator` | 最新的操作者输入：带 `user` 内容且未被来源标记的 `input/message`（A5） |
| `count` | 序号大于 `max(A, last_operator)` 的 Goal 续跑输入（标记 `kind="continuation"`，无 key） |
| `idle` | `max(A, last_operator)` 之后最新续跑标记的 `idle` 属性：在该续跑所跟随的回合结束前，连续由续跑驱动且没有 `step/tool-call` 的回合数；没有这种续跑或属性缺失时为零（C7）。结束回合自身的工具调用答案在决策时查询，不进入折叠。评审续跑写零：它不推进 C7（Pursuit §5） |
| `decided` | 对每个 `turn/end` 序号 `N`，是否存在标有 `after="N"` 的 Goal 写入（续跑或状态记录），或在 [Pursuit](Pursuit.md) 的评审裁判下 `goal:evaluate:N` 的 `branch/create`；对 `B` 位于 `S` 的 `branch/start`，是否存在标有 `after="S"` 的起始 `clear` |
| `judge` | 最新的存活状态记录的 `judge` 属性（`set` 设置它，`edit` 保留它，clear 退役它；缺省为 `model`）；旧历史上则取最新的存活 `goal:objective` 块（[Pursuit](Pursuit.md) §3） |
| `request` | 评审裁判下，`goal_update` 写入的 `op="completed"` 触发器：一次完成请求，不改变状态（A16） |
| `evaluation` | 评审裁判下，来源为 host、操作 id 为 `goal:evaluate:N` 的最新本地 `branch/create`：评审 `N` 及它所记录的分支。`N` 之后没有更新的变更时它仍是当前评审：Goal 为回答该回合内的变更而写的状态记录陈述的是那个变更，不是新的变更（Pursuit §4.1） |
| `conclusion` | 第一个本地输入，其第一个块的标记是一份有效报告（[Pursuit](Pursuit.md) §5）：`kind="report"`、`result` 为四种结果之一、`evaluation` 为十进制序号，且 `evaluator` 等于 `evaluation` 的 `branch/create` 所记录的分支。缺少任一项的报告不是结论 |
| `pending_driving` | 有效模式为 Queue 或 Steer、且既无 `input/consumed` 也无 `input/deleted` 的本地 `input/message`（session access §2 待处理规则）。Goal 不解读 InputQueue 的指令 `input/withdraw`；只有 InputQueue 的结果 `input/deleted` 才移除输入 |
| `turn_open` | 最新 `turn/end` 之后存在 `turn/start` |
| `open_tasks` | 本分支以 `exec/started` 启动、尚未被匹配的 `exec/ended` 关闭的后台任务，按 `task_id` 记录（A.4） |

仅运行时：折叠结果，以及在途的写入任务集合，按做出决定的事件序号为键。Dispatcher 处理器推进折叠结果、做决定、需要时登记写入任务，然后返回；它从不等待。没有 worker，没有与调用共享的锁，也不等待某个序号。两者都由加载折叠重建；新一代从不继承在途任务（旧一代的任务随其卸载结束）。

## 3. 流程

### 3.1 设置与续跑

```mermaid
sequenceDiagram
  participant CLI
  participant Rpc
  participant GS as GoalSession（Session）
  participant TL as 分支 Timeline
  participant G as goal（分支）
  participant IQ as InputQueue 与 Loop
  CLI->>Rpc: session.goal.set
  Rpc->>GS: sessionMethods call(MethodCall)
  GS->>TL: Branch::inputs [goal:state 记录（Append）、set 触发器（Queue）]，一次提交，回复 {sequence}
  TL-->>G: Dispatcher：input/message 记录，然后触发器（折叠：active）
  TL-->>IQ: input/applied 记录、input/applied 触发器、turn/start、input/consumed
  IQ->>TL: 在 N 处 turn/end completed
  TL-->>G: Dispatcher 投递 turn/end N
  Note over G: 基于已消费状态判断 C1..C7
  G->>G: 登记写入任务，处理器返回
  G->>TL: 任务复核，Branch::input 写 N 之后的续跑（Queue）
```

方法不读折叠结果；它向分支部分要来该批次，并以触发器的序号回复。记录与触发器作为一个原子批次提交，因此携带触发器的第一个请求已经带着排在它之前的记录；记录是 Append，触发器模式不变，所以 Queue 触发器仍驱动它的轮次。续跑由 Goal 拥有的任务写入；InputQueue 和 Loop 通过各自的订阅消费这些输入。

### 3.2 通过 `goal_update` 完成

```mermaid
sequenceDiagram
  participant Model as 模型
  participant TE as ToolExecutor
  participant GS as GoalSession（Session）
  participant TL as 分支 Timeline
  participant G as goal（分支）
  Model->>TE: goal_update completed，summary
  TE->>GS: 已注册的 execute(ToolCall)
  GS->>TL: Branch::inputs [goal:state 记录 status="completed"（Append）、completed 触发器（Append）]，一次提交
  GS-->>TE: ToolResult "goal_update recorded: completed."
  TL-->>G: Dispatcher：input/message 记录，然后触发器（折叠：completed 请求/状态）
  TE->>TL: 整批完成后写 step/tool-result
  Note over G: 下一个 turn/end：C1 为假，不续跑
```

记录与触发器一起提交、记录在前，位于批次的 `step/tool-result` 之前；工具结果在其后。之后发生压缩时，模型看到的最新 `goal:state` 块陈述 `completed`（在保留的轮次中原样出现，或在该块位于 `retain_from` 之前时经带 key 的基线出现），与折叠结果相同（在 [Pursuit](Pursuit.md) 的评审裁判下，干活模型的触发器是请求，记录陈述 `active`，A16），直到之后的变更替换它。工具确认写入；记录按折叠规则陈述状态。

### 3.3 中断暂停目标

```mermaid
sequenceDiagram
  participant User as 用户
  participant TL as 分支 Timeline
  participant G as goal（分支）
  User->>TL: 在 M 处 stop/interrupt scope turn
  TL-->>G: Dispatcher 投递 stop/interrupt M
  Note over G: 目标 active，A 在 M 之前
  G->>G: 登记写入任务，处理器返回
  G->>TL: 任务复核，Branch::input 暂停记录，reason interrupted（Append）
```

`scope: branch` 的中断被忽略：分支正在关闭。

### 3.4 新分支清除继承的目标

```mermaid
sequenceDiagram
  participant TT as TimelineTree
  participant TL as 新分支 Timeline
  participant G as goal（新分支）
  participant IQ as InputQueue 与 Loop
  TT->>TL: S 处的 branch/start（子分支，或分叉 Session 的 main）
  TL-->>G: Dispatcher 投递 branch/start S
  Note over G: 继承的 key 与 S 未 decided
  G->>G: 登记写入任务，处理器返回
  G->>TL: 任务复核，Branch::input 退役的 goal:state（以及存活的旧 key），after S（Append）
  TL-->>IQ: 随下一次请求消费
```

所用的运行时事实是 `branch/start`，即每个新分支的第一条本地事件：子分支的 Timeline 以它开始（[event-driven flows](../../../architecture/rust-agent/event-driven-flows.md)），新 Session 的根分支也以它开始。分叉 Session 的 `main` 则有分叉在复制来的事实之后写的 `session/forked`（A11），Goal 同样对待这条事实：`S` 就是它的序号。Goal 的写任务等待 `branchLifecycle.activated()`，因为这条事实在分支加载时就已在 Goal 折叠的历史中。Goal 像对待其他决策一样处理它：只依据已消费状态，登记一个自有写入任务并返回。任务复核继承的 key，若已存在本地目标块（本地触发器、任何记录，或这条 clear 本身）就不写。继承读取失败时不写任何内容并记录日志；下一次加载会重新决定开场。写入是一条输入：一条退役的 `goal:state` 记录，带 `reason="new_branch" after="S"`；若继承前缀中旧 key 仍存活，同一条输入再加一条退役的 `goal:objective` 块和一条退役的 `goal:status` 块（旧形式，D5）。于是新分支上的模型不会把父分支的目标当作自己的目标，旧 key 也不会留存活。这条 clear 是新分支的本地事件，因此该分支之后的任何压缩都不会把父分支的目标显示为存活：若 `retain_from` 位于 clear 之后，切点处生效的带 key 基线中记录已退役；否则 clear 在继承来的块之后被原样保留。事件 id（因而 `retain_from`）在继承前缀中保持稳定，因此从父分支继承来的压缩记录在新分支上以同样方式重放。`S` 已消费但未 `decided` 时，加载路径做同一个决策，因此重载或重启最多写一次。由于任务不阻塞创建，分支的初始 Queue 输入可能先于 `clear` 提交；父分支的块已告诉本 Session 的一条线：这个目标属于另一条线，因此不为它调用 `goal_update`，而且该分支的折叠结果无论如何都没有目标，所以它从不续跑。

## 4. 时序与冲突

### 4.1 中断在续跑之前提交（最多多一轮）

```mermaid
sequenceDiagram
  participant G as goal（分支）
  participant TL as 分支 Timeline
  participant LP as Loop
  TL-->>G: 投递 turn/end N（尚未消费中断）
  TL->>TL: stop/interrupt 在 M 处提交，尚未投递
  G->>TL: 写入任务：在 M+k 处续跑（Queue）
  TL-->>G: 投递 stop/interrupt M：写入任务，暂停记录（Append）
  LP->>TL: 轮次开始，消费续跑与暂停记录
  Note over TL: 模型最后看到暂停，该轮结束时 C1 为假
```

按序号决定：决策和任务的复核只使用了直到 `N` 的已消费事件，因此续跑成立，之后的暂停记录结束目标。这多出的一轮是可接受的（决定 E18）；Goal 不撤回其续跑。

### 4.2 重载

任一部分的重载都发生在罕见的结构性操作之后。Session 部分的重载像任何注册的工具一样撤回其 `goal_update` 注册，直到下一代重新注册，可能使在途轮次失败；这是可接受的代价（[ADR 002](../../adr/002-plugin-scope.md) D3）。分支部分的重载从 Timeline 重建；其卸载结束在途写入任务。若最新的 `turn/end` 不是 `decided` 且目标为 active，新一代执行该决策一次（C4 和 C5 去重）。重载前已提交的变更保持有效。没有记录的触发器——更早版本写成的历史，或批次之外写入的触发器——在加载时被回答一次，由一条在 `after` 中写名它的记录回答；位于最新状态记录 `after` 之下的触发器已被回答，因此重载从不为它写第二条记录。

**重建成本。** 重建有一条便宜路径（D4）：从分支已提交的头向开场做一次降序 `input/message` 扫描，在最新的 Goal 块处停止；`input/deleted` 把对应的输入从这次扫描中移除。什么都没找到，或最新的块是一条退役的 `goal:state` 记录时，该分支没有目标，重建到此结束，不做 13 种 kind 的折叠。只有带目标、或带可能承载目标的 Goal 块的分支才付出完整折叠。分叉也用同样方式读取一次复制来的前缀，以取得开场必须退役的 key（§3.4）。便宜路径是可靠的，因为上面的每个决策都以 C1（`running`）开头，而没有目标文本时它为假；结算路径需要一个便宜折叠不可能有的评审：它不结束任何东西。clear 过的分支唯一不能重建的，是它在 clear 之前打开的一条线：那条线由它自己的结束来结束，而不是在下一次重载时。

**补写一条从未提交补偿的删除。** 重载可能发现最新的已发布 `goal:state` 块命名了折叠已不再持有的变更：该变更的触发器的删除已提交，而进程在补偿记录提交之前停止。折叠丢弃被删除的触发器及随它的记录，于是分支重新计算出的状态是变更之前的状态，而模型与客户端仍读到已撤回的那一个。因此，重建结束时，Goal 把拥有该 keyed 块的陈述——最新已提交、未被删除且带有该 key 的输入；`retire` 块表示该 key 已退役（keyed-blocks §2.3）——与折叠为其当前状态算出的陈述相比较，比较 objective、state、judge 和 retire，并按记录写入者所用的同一规范化读取。来自更早版本、无法作为记录读取的块，以及折叠持有目标却没有任何块，都算作不同。当两者不同时，重建写一条陈述折叠状态的记录，在 `after` 中命名重建的截止序号，并经过每条记录都要经过的同一高水位与去重守卫；该记录是拥有型工作，等到分支发布后才执行，因为树只在分支的构建返回后才解析该分支。补写命名的是截止序号，而不是某个决策已作答的事实，因此它绝不会替代续跑或结论。补写还会让位于任何记录：它在写入路径与折叠中都不抬高任何顺序标记，因此加载之后做出的决策仍在截止序号以下的序号陈述自己的状态；它只在本次世代做出的每条 Goal 写入都已折叠之后才从折叠组合；并且只有当没有 Goal 记录提交在重建截止序号之上时才写入，因为这样的记录是从重建后的折叠组合的，已经替换了已发布块。该记录提交后，已发布陈述与折叠一致，于是下一次加载什么也不写，发现两者一致的加载也什么也不写。在线删除路径保留它自己的补偿；补写是补偿从未提交时分支所欠的那一条。

**变更要等重建完成。** `session.goal.*` 调用或 `goal_update` 到达分支 Owner，而 Owner 在其部分 apply 开始时、重建运行之前就进入变更表。调用只在重建返回、且它持有的事实都已折叠之后才组合；对于构建尚未完成的分支，还要等到树拥有该分支，因此与记录一同提交的触发器陈述的是施加于完整折叠的变更，而不是施加于尚未读取的折叠。在部分注册之前到达的调用仍像以前一样回答 `Reloading`。

### 4.3 其他关键行

| 场景 | 序号顺序 | 结果 | 归属 |
| --- | --- | --- | --- |
| 决策时有用户 Queue/Steer 输入待处理 | 输入 < `turn/end` N，未消费 | C5 不成立：不续跑；由用户那一轮的结束来决策，计数重置 | Goal |
| 驱动输入被删除 | 输入 < `input/deleted` < N | 不算待处理；续跑继续 | InputQueue；Goal |
| 驱动输入在 N 之后被删除（例如在被应用前经 `session.input.withdraw` 撤回） | 输入 < N < `input/deleted` | N 的决策在 C5 停止；消费 `input/deleted` 时重新做 N 的决策；C1–C6 成立则续跑 | InputQueue；Goal |
| 一个批次中两个 `goal_update` 调用 | 两个块都在 `step/tool-result` 之前 | 都写入，两个结果都确认写入；折叠按顺序应用两者，第二个是当前状态 | Goal |
| `completed` 或 `blocked` 之后 `resume` | 终态块 < resume | 目标重新打开：`active`，计数重新开始，这条 Queue 输入驱动一轮 | Goal |
| 变更在被持有时被撤回 | 变更 < `input/deleted` | 从折叠中移除；最新未决策或已停止的决策重新进行；当已有写名该变更的记录或任何更晚的记录时，补偿记录陈述重新计算后的分支状态 | InputQueue；Goal |
| 子分支在目标块之后切出 | 已消费 < 切点 < `branch/start` | 子分支的折叠结果没有目标；Goal 写入起始 `clear`（§3.4）；不续跑；在那里的 `goal_update` 或任何变更只影响子分支 | Goal |
| 目标轮次内的压缩（自动或手动） | `turn/start` < 压缩事实 < `turn/end` N | 该轮的一个步骤，没有 `turn/end`：不决策；轮次在压缩后的上下文上继续，由 N 照常决策 | Loop；Goal |
| 没有轮次时的手动压缩 | `context/compact` < `context/compaction-start` < `context/compaction`，没有 `turn/*` | 不决策，不续跑；最新的 `turn/end` 与 `turn_open` 不变 | Loop；Goal |

完整表格：[附录 B](#附录-b-完整冲突表)。

## 5. 上下文映射

| 模型看到的内容 | 形式 | 位置 |
| --- | --- | --- |
| 每次变更 | 一个原子批次、两条没有 `user` 内容的输入：Goal 回答该变更的状态记录，一个 raw 块 `<dim-goal kind="state" status=... trigger="goal:<uuid>" branch=...>`，key 为 `goal:state`；然后是变更触发器，一个无 key 的 raw 块 `<dim-goal kind="goal" op=... branch=...>`，说明被要求了什么（A.5） | 记录与触发器各自在其 `input/consumed` 位置，记录在前（Queue 触发器开启它所驱动的轮次）：携带该变更的第一个请求已经带着排在它之前的记录 |
| 续跑 | `input/message`，Queue，`<dim-goal kind="continuation" count=k limit=L after=N branch=...>`，无 key；不重述目标 | 它所驱动轮次的开头 |
| `goal_update` | 已采用的 `context/configured` 中的固定工具声明；结果在 `ToolResult.content` 中 | 其批次的工具结果位置 |

| 方面 | 规则 |
| --- | --- |
| 压缩 | 一条重放规则，无论自动还是手动、是否在轮次内（F1）：先是 `replacement`，然后是带 key 的基线，即位于 `retain_from` 之前的每个 key 最新的存活块（已退役的 key 不重放），然后原样保留从 `retain_from` 起的事件（保留尾部）。因此模型经 `goal:state`（旧历史上则是两个旧 key）、经基线或在保留的轮次中原样看到当前状态，且最新的记录排在最后。保留轮次中的触发器、记录和续跑原样保留；`retain_from` 之前更早的只通过 replacement 保留 |
| 子分支与分叉 Session | 继承切点之前的块，块中写明所属分支，并告诉本 Session 的一条线：这个目标属于另一条线，因此不为它调用 `goal_update`；Goal 消费新分支的 `branch/start` 时用一次 clear（原因 `new_branch`）退役继承来的 key（§3.4），因此新分支从没有目标开始（A3），在那里压缩之后也是如此 |
| `interactive = false` 的 Session | 映射相同；Goal 不读取 `SessionConfig.interactive`（C2），续跑在没有交互对端时驱动轮次 |
| 始终不可见 | `submissionId`、模式、操作者输入计数、折叠结果、诊断、其他插件的事件 |

确切文本：[附录 A.5](#a5-模型可见文本)。

## 6. 接口（摘要）

| 接口 | 类型 | 用途 |
| --- | --- | --- |
| `goal_update {status, summary}` | 注册到 `toolRegistry` 的工具（Session），风险 `session` | 模型报告完成或受阻；写入一条变更输入并确认写入 |
| `session.goal.set/edit/pause/resume/clear` | `sessionMethods`（Session，命名空间 `session.goal`） | 用户变更目标；每个方法写一条变更输入，并以其序号回复。`set` 还接受 `judge`，即 [Pursuit](Pursuit.md) 的评审裁判 |
| 读取目标 | 分支的事件，或 App Server 提供时分支的存活带 key 块（session.keys，[keyed-blocks](../appserver/keyed-blocks.md)） | 分支本地事件中最新存活的 `goal:state` 记录就是它的目标文本与状态（§2，A.3），也就是模型看到的块；旧历史上则是最新存活的 `goal:objective` 与 `goal:status` 块。客户端从已经收到的分支历史与实时事件中读取，或从该带 key 投影读取；没有 Goal 查询方法（A8） |
| `goal` 来源：`maxContinuations` | 业务配置（Settings） | 可选的续跑上限；每次 turn/end 决策使用时读取文档（§1）；缺失表示没有上限 |

细节：[附录 A](#附录-a-接口细节)。

## 7. 安全

Goal 只在它服务的分支上以来源 `goal` 写入 `input/message`；由此产生的轮次在 Session 的 Permission 与 Approval 策略下运行，目标不授予任何权限。用户无法伪造目标块：`session.submit` 不接受 raw 块，且标记渲染器会转义目标文本和 summary。`goal_update` 在 `session` 下准入。诊断记录 Session id、分支 id、决策与原因，从不记录目标文本。

## 8. 规则检查

规则：[principles §1 与 §4](../../../architecture/rust-agent/principles.md)，[ADR 002](../../adr/002-plugin-scope.md) D1–D5。

| 规则 | 本文如何符合 |
| --- | --- |
| §4.1 改别的模块拥有的状态时写指令事件 | 每次变更都是 Goal 自己消费的输入；Goal 从不删除或应用输入，它依据 InputQueue 的 `input/deleted`，不读 `input/withdraw`。 |
| §4.2 交接后不能撤回 | Goal 从不撤回自己的续跑；中断后才被应用的续跑多跑一轮，之后的暂停块结束目标。 |
| §4.3 只用 before 回调在写入前干预 | 不适用：Goal 不注册 before 回调，其工具和方法也不加入口检查（它们只写不判断）。 |
| §4.4 核心机制不带业务含义 | Goal 不要求 Timeline、Dispatcher、InputQueue 或 Loop 改动；其标记是一个普通 key 下的普通 raw 块，触发器不带 key。 |
| §4.5 批次 | 折叠结果随每条事件推进；对 `branch/start`、`turn/end`、`stop/interrupt` 与 `input/deleted` 的决定在 Goal 消费它们时只依据已消费状态做出，写入前的复核也只用已消费状态。 |
| §4.6 模型上下文只含交接后被读取的事件 | 目标块与续跑只在被采用后进入模型；被删除的输入从不进入，并从折叠中移除。 |
| §4.7 插件之间只通过公开事件和服务交互 | Goal 只读公开的 `branch/start`、`input/*`、`turn/*` 与 `stop/interrupt` 契约，并经 `Branch::input` 写入。 |
| §4.8 向机制所有者提通用需求 | 无：Goal 使用的运行时服务已按名导出（C5，#4）。 |
| §4.9 只有后续决定要用的事实进入 Timeline | 每条变更输入既是模型上下文，也是折叠的输入；Goal 自己的写入驱动轮次、结束续跑或陈述状态；折叠结果就是最新的存活记录，也就是压缩重放的内容（A16）；折叠结果、在途任务和诊断不进入 Timeline。 |
| §1 Capability 与 App Server 边界 | 两个部分都是各自包中的 Capability；Rpc 只分派 `sessionMethods`。 |
| ADR 002 D1 作用域跟随状态所有者 | 工具与方法是 Session 服务；目标、其折叠结果与决策是分支状态。 |
| ADR 002 D2 不设接线模块 | Goal 自己把命名空间注册到 `sessionMethods`、把工具注册到 ToolRegistry；Rpc 经该 Session 的方法表分发，没有模块指名 Goal。 |
| ADR 002 D3 依赖跟随作用域 | 分支部分依赖分支服务与外层的 Session、进程服务（`treeCommands`、`settings`）；Session 部分不依赖任何分支服务；重载重新折叠，可接受。 |
| ADR 002 D4 结构性配置与业务配置分离 | `goal` 来源中的 `maxContinuations` 是业务配置，在自有 turn/end 任务内、已消费状态复核之前读取；不缓存或订阅配置事件。变更从不重载 Goal。 |
| ADR 002 D5 合同语言中立 | 工具、方法与块格式以中立形式给出；§10 是 Rust 实现，附录 G 是它的绑定。 |

## 9. 向机制所有者提出的需求

输入入口每次调用只提交一条输入，变更触发器与其记录因此无法共享一次提交。需求——一个原子多输入提交：批次整体校验、只准入一次，按给定顺序返回已提交事件，对已提交的 id 幂等重放——由输入入口的多输入提交满足：`TreeCommands::inputs`、`Session::inputs` 与 `Branch::inputs`，契约见 [interrupt §2](../../../architecture/rust-agent/interrupt.md#2-事件契约)。Goal 的变更批次是它的第一个消费者（#1195，[#1215](https://github.com/arcships/dim/pull/1215)）。之前的需求"运行时服务按名字暴露给非 Rust 部分"（#4）已由 C5 交付（[分支 TypeScript 绑定](../../../design/branch-typescript.md)）。

## 10. 实现

Rust，位于 `crates/dim-goal`，为一行：其 `install` 注册 Settings 来源，并经部件注册表注册 Session 部分 `GoalSession`（C4）与分支部分 `GoalBranch`（C5）：

| 文件 | 部分 | 声明 |
| --- | --- | --- |
| `text.rs` | 共享：变更触发器、状态记录与续跑输入（A.5）、标记编码、两句策略文本 | — |
| `fold.rs` | 共享：已消费事实上的折叠（§2）与决策（A.4） | — |
| `lib.rs` | 该行：注册 `goal` Settings 来源与两个部分；`install_with` 接收两句策略文本 | 进程行 |
| `session.rs` | `GoalSession`：把 `<dim-goal>` 引导条目（A.5.1）注册到 `GuideEntries`，把 `goal_update` 注册到 `ToolRegistry`，把 `session.goal` 注册到 `SessionMethods`；经 `TreeCommands::inputs` 提交每个变更批次 | Session 部分 |
| `branch.rs` | `GoalBranch`：用 `Dispatcher::consume` 注册，折叠重建读到的内容（先走便宜路径），对实时事件决策，并以自有任务写入状态记录、续跑与 clear；从 `goal` 来源读取 `maxContinuations`。在 [Pursuit](Pursuit.md) 的评审裁判下它还创建评审分支并担任评审者角色 | 分支部分 |

新分支检查（§3.4）按祖先逐个读取继承前缀，从最近的开始，用 `TreeCommands::query` 查该祖先自己的线：`input/message`，从新到旧，每一页都在上一页之下有界，遇到第一个带 key 的块即停止。分支自己的事件从不参与这次读取，因此只看到继承前缀。嵌入方用
`dim_goal::install(&mut AgentBuilder)` 安装 Goal，或用 `install_with` 带上候选策略文本；`dimcode` 产品 profile 在其 install 回调中这样做，App Server 库不依赖本 crate。

验证覆盖精确事件顺序，以及实际插件的公开端口流程。

部件测试台（`crates/dim-goal/tests/`，基于真实的 `Dispatcher`、`Timeline`、`TimelineTree`、`SessionMethods` 与 `ToolRegistry`）按精确的事件顺序运行折叠与决策，包括实时 Session 无法按需复现的竞态：第 2 条（操作者输入重置计数，其他来源的带标记输入不重置）、附录 B 中第 3 条的每一行中断（`turn/end` 之前、`turn/end` 与续跑之间、开始后与应用后、目标之前、`goal_update` 写入前后）、第 4 条（待处理的 Queue/Steer 压制而 Append 不压制、`N` 之前与之后被删除的输入、撤回的 `pause`）、第 5 条（新一代重新折叠不重复决策）、第 6 条（压缩步骤不触发决策，轮内与无轮皆然）、第 9 条（同批两次报告、无目标时的 `pause`/`resume`/`edit`）、第 10 条（`edit`、`clear`、伪造标记）、第 11 与 12 条。恢复 Session 的历史覆盖加载类用例：受控读取做出与上一代同样的决策，重放的轮次结束不报告任何结果。

产品测试台（`goal_product_flow.rs`、`goal_flow.rs`）在产品 profile 中用脚本模型运行插件：第 1 条（set、续跑、`goal_update completed` 结束续跑）、批次顺序（空闲分支上，无目标时的 `set` 与暂停后的 `resume` 各自驱动第一个请求，其中记录排在它的触发器之前；每种各跑 20 次）、第 2 条（达到 `maxContinuations` 时暂停 `limit`）、第 3 条（目标开始后一轮中 `session.stop` 使其暂停，不续跑）、第 4 条（一轮运行中设定目标并排队输入：该轮结束不决策，下一轮决策）、第 5 条（恢复后的分支重新折叠，不重复决策）、第 7 条（子分支与分叉 Session 的 `main`：开场之后一次起始 `clear`，来源不变）、第 8 条（已卸载的 Session 由 `session.goal.pause` 恢复并写入；新的 `maxContinuations` 无需重载即生效；没有 Goal 时为 `unknown_method`）、第 11 条（完成后 resume，续跑到新上限）、方法边界与 `pause`/`resume`/`clear` 的顺序，以及 A.5 编码往返。

这些流程中的 Goal 行是普通可选行，没有任何依赖强制 Goal 先于工具快照：Session 部分注册 `goal_update` 后，
ToolRegistry 即提供它（[Session 边界 A.5](../contracts/session-boundary.md#a5-工具贡献)，#241）。

第 6 条中关于键控基线的一半是 F1 的契约，由其 `keyed_blocks_replay_verbatim_after_repeated_compaction_until_retired`
与手动压缩流程覆盖；产品压缩配置在测试中不加载 TypeScript 行。

## 附录 A. 接口细节

### A.1 工具 `goal_update`

经 `register(declaration, execute)` 注册到本 Session 的 `toolRegistry`（[session-boundary A.5](../contracts/session-boundary.md#a5-工具贡献)）：`declaration` 是 `ToolDeclaration`（名称、A.5 中的描述、下列参数 schema、风险 `session`）；`execute(ToolCall, cancelled): async -> ToolResult`；取消就是对调用发出取消请求。

| 项 | 值 |
| --- | --- |
| 参数 | `{status: "completed" \| "blocked", summary: string}`；`summary` 1..=2000 个字符（A10） |
| 结果 | `ToolResult.content`：`goal_update recorded: completed.` 或 `goal_update recorded: blocked.` 结果只确认写入；按折叠规则，该块成为当前状态（A15），[Pursuit](Pursuit.md) 的评审裁判下干活模型的 `completed` 除外，它是请求（A16）。错误只有参数错误 |
| 风险类别 | `session`（只写分支输入）（A9） |
| 提供范围 | 安装了 Goal 的装配中的每个 Session，所有角色（A9） |
| 处理器 | 校验参数，经 `treeCommands.inputs` 在调用方分支（`ToolCall.branchId`）上把该变更的记录与触发器作为一个原子批次提交，等待提交后返回。它不读折叠结果。在追加之前取消则不写任何内容；一旦追加，写入保持有效 |

### A.2 Session 方法

命名空间 `session.goal`，注册到本 Session 的 `sessionMethods`。其声明为每一行一个 `MethodDecl`，其中 `interruption: report`、`progress: false`、`mutating: true`。该部件的 `call(MethodCall, cancelled, progress)` 接受下列参数，并以 `MethodReply` 回复结果；错误为 `MethodError` 数据（附录 C）。已实现的 Rust 绑定（C1）只覆盖 `name`、`mutating`、`interruption` 和 `progress`（附录 G）。

| 方法 | 参数 | 结果 | 写入 |
| --- | --- | --- | --- |
| `session.goal.set` | `{sessionId, branchId?, objective, judge?}` | `{sequence}` | `set`，Queue；`judge` 为 `"model"`（默认）或 `"evaluator"`，其他值为 `invalid_params`（[Pursuit](Pursuit.md) §3） |
| `session.goal.edit` | `{sessionId, branchId?, objective}` | `{sequence}` | `edit`，Append（A1） |
| `session.goal.pause` | `{sessionId, branchId?}` | `{sequence}` | `pause`（原因 `user`），Append |
| `session.goal.resume` | `{sessionId, branchId?}` | `{sequence}` | `resume`，Queue（A12） |
| `session.goal.clear` | `{sessionId, branchId?}` | `{sequence}` | `clear`（退役 key），Append |

`branchId` 默认为 `main`（A13）。回复中的 `sequence` 是变更触发器的序号，它在同一原子批次中紧跟在记录之后提交；客户端在分支事件的该序号处看到它的效果。写入类方法只校验自己的参数；变更起什么作用由 §2 的折叠规则决定（每个块成为其 key 的当前值），从不由方法决定。Rpc 从参数中读取 `sessionId` 并授权，经 `index.acquire` 解析 Session（若已卸载则恢复），再携带原始参数发送调用；处理器经 `treeCommands.inputs` 按 id 把该批次写入指定分支。

### A.3 读取目标

没有 `get` 方法，也没有分支服务。分支部分、客户端与模型读到同一个事实：分支本地事件中 key 为 `goal:state` 的最新存活块；在记录出现之前写成的历史上，则是 key 为 `goal:objective` 的最新存活块和 key 为 `goal:status` 的最新存活块。状态记录的标记陈述状态——`status="active"`、`status="paused" reason=...`、正文带 summary 的 `status="completed"` / `status="blocked"`，或用于"有目标文本但没有状态"的 `status="none"`——一条退役的 `status="cleared"` 记录表示分支没有目标。其正文以独立元素承载目标文本（`<objective>`）与策略文本，已完成或受阻时再以 `<result>` / `<blocker>` 承载 summary；其 `after` 写名它回答的变更触发器。只有目标文本存活时目标才存在（A16）。标记格式与 A.5 的可逆编码就是契约；读取的值都先解码。续跑计数以及（设置时）上限在每个续跑标记中（`count`、`limit`）。在旧历史上、[Pursuit](Pursuit.md) 的评审裁判下，不带 `by="evaluator"` 的干活模型 `op="completed"` 块是请求，不改变状态（A16）；记录路径不需要这条规则，因为评审路径留下的记录陈述 `active`。

### A.4 续跑决策

Goal 消费分支 `B` 上序号 `N` 处的 `turn/end` 时，基于直到 `N` 的折叠结果：

**后台前置条件。** 若 `open_tasks` 非空，该 turn/end 不做任何决策——既不再续跑也不暂停——并保持未决。后台任务的非 cancelled `exec/ended` 之后会跟着它的唤醒输入（BackgroundExec 的 `<dim-notification>`），该输入驱动下一轮，其结束照常决策。`cancelled` 的 `exec/ended` 不唤醒；当它关闭最后一个开放任务且分支空闲、没有待处理驱动输入时，Goal 此时执行被推迟的 turn/end 决策。该路径只读取事实中的 cancelled 状态，因此不会与唤醒竞争；决定轮次的 `decided` 标记会去重。折叠按 `task_id` 在 `exec/started` 与 `exec/ended` 上跟踪开放任务，重建会恢复该集合。若等待推迟的是一份 `continue` 结论（Pursuit §5），同一结算路径会把该结论交给实时消费它的同一个处理器：折叠按评审序号保留它，直到该评审的续跑或暂停回答它，或被更新的 turn end 或操作者输入取代。结算路径从折叠中重新推导该结论，因此重载后仍然保留。

| # | 条件 | 若为假 |
| --- | --- | --- |
| C1 | 目标存在（最新存活状态记录陈述目标文本，旧历史上则是本地 `goal:objective` 块存活）、`status = active` | 停止 |
| C2 | `!interrupted` | 写暂停（`interrupted`）并停止 |
| C3 | `N` 处的 `turn/end` 为 `completed` | `failed`：写暂停（`turn_failed`）（A6）；`cancelled`：由 C2 处理 |
| C4 | `N` 是最新的 `turn/end`、`!turn_open`，且 `N` 不是 `decided` | 停止（更新一轮的结束会触发） |
| C5 | `!pending_driving`（包括 Goal 自己未消费的续跑；不包括已删除的输入） | 停止 |
| C6 | 没有显式 `maxContinuations`，或 `count < maxContinuations` | 写暂停（`limit`）并停止 |
| C7 | 结束回合不是由 Goal 自己的续跑驱动，或它调用了工具，或包含它的连续停滞数低于 `NO_PROGRESS_TURNS`（3） | 写暂停（`no_progress`）并停止 |
| — | 全部成立 | 写入续跑（Queue），计数为 `count + 1`，并带 `idle` |

C7 的输入是结束回合的 `turn_id`、该回合是否由续跑驱动（两者在 `turn/start` 与 `turn/end` 时保存），以及折叠结果中的 `idle`（§2）。turn/end 任务在决策时查询一次分支的 `step/tool-call`：以结束回合的 `turn_id` 为谓词、limit 为 1，从该回合的 `turn/start` 开始；`step/tool-call` 不是 Goal 消费的种类，重建也不读它（A.6、#1113）。只有当续跑驱动了结束回合时才查询；由其他驱动输入开启的回合不读即从零开始。后台任务开放期间决策被推迟，这次读取也一并跳过；被推迟的决策执行时再读取。C7 只属于分支自己的续跑决策：`continue` 结论不读进度，其评审续跑写 `idle="0"`，因为评审者已经看着完整回合做了判断——生产环境中该回合带有触发请求的 `goal_update` 调用，工具调用读数永远不会触发（Pursuit §5）。

下一条续跑的 `idle` 在结束回合由 Goal 续跑驱动且没有工具调用时为 `idle + 1`，否则为零（结束回合不是续跑驱动，或它调用了工具）。因此即使续跑驱动了回合，一次工具调用也会重置停滞数。停滞数读取自 `max(A, last_operator)` 之后最新的续跑，所以 `set`、`resume` 或操作者输入会丢弃它之前的回合；若一个回合同时消费了操作者输入和续跑，且由续跑驱动、未调用工具，它仍计一次。`NO_PROGRESS_TURNS` 是常量，不是设置项。

投递 `M` 处的 `stop/interrupt`（`scope: turn`）时：若 C1 成立且 `A < M`，写暂停（`interrupted`）。

投递 `input/deleted` 时：去掉被删除的输入重新折叠，丢弃每条写名它的记录；当写名被删除变更的记录或更晚的记录曾被写入或正在被写入时，写入陈述重新计算状态的补偿记录；若最新的 `turn/end` 不是 `decided`，重新做它的决策（C4 与 C5 去重）。

每次写入都是自有工作：处理器登记一个以做出决定的序号为键的任务，然后返回；若某个 turn-end 请求到达时，该 turn end 的任务已在途，则在任务结束时再回答一次。写入前，任务基于此刻已消费的折叠结果复核同样的条件（[time-bus](../../../architecture/rust-agent/time-bus.md) §4），条件不再成立就不写；已提交但尚未消费的事实不读，由其消费方按序号处理（§4.1）。没有变化时，C4 与 `decided` 标记会让这次重复决策成为空操作，因此通常不写任何内容。cancelled 的 `exec/ended` 需要它：它可能在 turn-end 工作已经读过折叠、带着推迟的决策返回之后才关闭最后一个开放任务，而此后没有 wake 再请求一次决策（见上文的 background precondition）。Goal 的写入带 `after="N"`（或 `after="M"`），因此折叠结果记录了该决定，重载不会重复它。

投递位于 `S` 的 `branch/start` 时：若继承的 key 存活且 `S` 未 `decided`，写入起始 clear（原因 `new_branch`，`after="S"`）；任务写入前复核继承的 key（§3.4）。

### A.5 模型可见文本

每条输入都是一条没有 `user` 内容、带一个 raw 块的 `input/message`：标记
`<dim-goal kind="..." ...>`、换行、转义后的正文、换行、`</dim-goal>`（A2）；起始 clear 另外带上要退役的旧 key。变更触发器带 `kind="goal"` 且没有 key；状态记录带 `kind="state"` 和 key `goal:state`；续跑不带 key，也不是变更。每次变更经 `Branch::inputs` 把两条这样的输入作为一个原子批次提交：记录在前，触发器在后。一条 `InputWrite` 可携带任意多个 `RawBlock`（[session access A.1](../contracts/session-access.md#a1-sessions-服务)）；除起始 clear 外只有变更批次使用一条以上的输入，记录正是经该批次进入触发器的轮次。

编码：目标文本、summary 与每个属性值中的 `&`、`<`、`>`、`"` 写成 `&amp;`、`&lt;`、`&gt;`、`&quot;`，因此用户或模型的文本无法伪造模型会当作 Goal 的标记。该编码可逆：折叠与客户端依次把 `&quot;`、`&gt;`、`&lt;`、`&amp;` 替换回原字符来解码。每个自由文本字段各占一个元素：目标文本位于 `<objective>` 与 `</objective>` 之间，已完成目标的结果位于 `<result>` 与 `</result>` 之间，受阻目标的阻碍位于 `<blocker>` 与 `</blocker>` 之间。元素内容经过转义，而编码会映射 `<` 与 `>`，因此内容中不可能出现闭合标签，边界是精确的：值里的标题行、闭合标签或空行都能逐字节往返。折叠与两个客户端都读取标签之间的内容再解码；它们都不再按标题行切分正文。模型读到的是编码后的形式。

**变更触发器**（无 key，无状态句子）：

| 触发器 | 模式 | 文本 |
| --- | --- | --- |
| `set` | Queue | `kind="goal" op="set"`，选择时在 `branch` 之前带 `judge="evaluator"` / `Objective:` / `<objective>` / 空行 / `The user asked to set the goal for branch B to this objective.` |
| `edit` | Append | `op="edit"` / `Objective:` / `<objective>` / 空行 / `The user asked to edit the objective of the goal for branch B.` |
| `pause`（用户） | Append | `op="pause" reason="user"` / `The user asked to pause the goal for branch B.` |
| `resume` | Queue | `op="resume"` / `The user asked to resume the goal for branch B.` |
| `clear` | Append | `op="clear"` / `The user asked to clear the goal for branch B.` |
| `completed` / `blocked` | Append | `op="completed"` 或 `op="blocked"` / `Result:` 或 `Blocker:` / `<summary>` / 空行 / `goal_update reported the goal for branch B as completed.`（或 `blocked`） |

**状态记录**（`kind="state"`，key `goal:state`，Append）。属性按此顺序：`kind`、
`status`、`reason`（仅暂停：`user`、`interrupted`、`limit`、`turn_failed`、`no_progress`、
`evaluation_failed`）、仅在选择时带 `judge="evaluator"`、回答同一批次中的变更时带 `trigger`（它所同批提交的变更触发器的 submission id，`trigger="goal:<uuid>"`），否则带 `after`（它所回答事实的序号）、重建在该截止序号处重述重算状态而不回答任何事实时带 `restated="true"`（§4.2）、
`branch`。正文：`<objective>`、转义后的目标文本、`</objective>`、目标策略文本，然后是状态句子；已完成或受阻的目标再跟 `<result>` 或 `<blocker>` 元素及其转义后的 summary，每个元素的标签各占一行。记录只陈述模型与客户端读取的内容；Goal 的其他东西都不在其中，因为整个块进入模型上下文。每次变更都会重述完整的目标文本，直到一次压缩为止，因此每次变更最多增加一条记录，长度为目标文本上限 4000 字符加上固定句子。

| 记录 | 策略行之后的正文 |
| --- | --- |
| `status="active"` | `The goal for branch B is active. Keep working toward it across turns. When it is fully achieved, call goal_update with status "completed". If you cannot make progress without the user, call goal_update with status "blocked" and explain why.` |
| `status="paused" reason="user"` | `The user set the goal status for branch B to paused. Do not continue working toward a goal for branch B on your own; respond only to the user's latest input.` |
| `status="paused"`（其他原因） | `The goal is paused (<reason phrase>). Do not continue working toward it on your own; respond only to the user's latest input.` |
| `status="completed"` / `"blocked"` | `goal_update set the goal status for branch B to completed.`（或 `blocked`）` Do not continue working toward a goal for branch B on your own.`；评审裁判下为 `An evaluator verified that the goal for branch B is completed.` 或 `An evaluator found that the goal for branch B cannot proceed without the user.` + ` Do not continue working toward it on your own.` 然后是 `<result>` 或 `<blocker>`、转义后的 summary 及其闭合标签，各占一行 |
| `status="none"` | `The goal for branch B has no status.` |
| 退役，`status="cleared"` | `The goal for branch B is not in effect.`，或起始 clear 的 `Any goal above belongs to another branch, not to branch B. Disregard it; branch B has no goal.`（带 `reason="new_branch"`） |

起始 clear 是一条输入：一条退役记录；若继承前缀中旧 key 仍存活，再加一条退役的 `goal:objective` 块和一条退役的 `goal:status` 块（下面的旧形式）。

**旧形式**，写在状态记录之前，只在旧历史上读取：变更写
`goal:objective`（`op="set"` 或 `op="edit"`，正文带目标文本与策略）和/或
`goal:status`（`kind="status" status="active"`、`op="pause" reason=...`、`op="resume"`、
带 summary 的 `op="completed"` / `op="blocked"`，或由 `clear` 退役）；旧的起始 clear 用 `op="clear" reason="new_branch" after="S"` 退役两个 key。续跑不带 key：
`<dim-goal kind="continuation" count="k" limit="L" after="N" idle="k" branch="B">` / `Continue
working toward the active goal for branch B. If it is completed or blocked, call goal_update. If you
are on another line of this Session, the goal belongs to branch B: do not call goal_update for it.`；
只有当文档设置了上限时才写 `limit`，`idle` 始终写。

原因短语：`interrupted` "the user interrupted the turn"，`limit` "the automatic continuation
limit was reached"，`turn_failed` "the last turn failed"，`no_progress` "the goal made no tool call
in its last 3 continued turns"，`evaluation_failed` "the evaluator could not check the result"。
工具声明描述："Report that the current goal is completed or blocked."

### A.5.1 协议与策略

分类按句子判定（[model-facing prompts §4](../model-prompts.md#4-协议与策略)）：如果一句话变得与运行时不符、或某种机制无法再按它写的那样使用就会改变行为，这段文本是**协议**；否则是**策略**，可替换并由评估判定（§A.5.2）。

| 项 | 类别 | 依据 |
| --- | --- | --- |
| 标记名 `dim-goal`；它的 `kind` 值 `state`、`goal`、`continuation`，以及 Pursuit 的 `evaluate`、`evaluate-again`、`report`（[Pursuit](Pursuit.md) §5）；它的 key `goal:state`，以及只在旧历史上读取的旧 key `goal:objective` 与 `goal:status` | 协议 | 折叠、客户端与指南条目都要读它们。改动与行为和断言它的流程测试同批落地，并让按旧形式记录的历史仍可理解 |
| 折叠与客户端读取的属性名与值——`op` 及其 `set`、`edit`、`pause`、`resume`、`clear`、`completed`、`blocked`；`status` 及其 `active`、`paused`、`completed`、`blocked`、`none`、`cleared`；`reason` 及其 `user`、`new_branch`、`interrupted`、`limit`、`turn_failed`、`no_progress` 和 Pursuit 的 `evaluation_failed`；`branch`、`count`、`limit`、`after`、`idle`、`judge`、`evaluation`、`evaluator`、`result`、`by`——以及标签行 `Objective:`、`Result:`、`Blocker:`、`Evaluator report:` | 协议 | 属性与标签命名一种机制（[model-facing prompts §4](../model-prompts.md#4-协议与策略)）；上面的编码规则与它们同属协议 |
| 本页措辞改动涉及的三句话：目标文本（状态记录正文）的、续跑块的，以及 §3.4 与 §5 那一行陈述的子分支规则 | 策略 | 它们说的是怎么工作，不是运行时做什么：目标仍属于一条线，另一条线既不续跑它，也不为它调用 `goal_update` |
| 上表其余块中的句子 | 策略 | 关于怎么工作的指引。与它们并列的 `goal_update` 声明是协议——它的 schema、结果格式，以及陈述调用做什么的描述 “Report that the current goal is completed or blocked.” |

协议行只随行为与它的流程测试一起改动；策略行可替换，每次改动都按 §A.5.2 评估。

**指南条目。** Goal 从写这个标记的部件贡献 `dim-goal` 的条目（[Prompt §2](Prompt.md#2-条目的登记路由)，[model-facing prompts §6.2](../model-prompts.md#62-各部分归谁)）：Session 部件以 `session` 作用域登记，因为 Goal 在每条分支上写这个标记，而该条目恰好在那个部件加载期间生效。条目是协议，序号 `55`，内容为：

> `<dim-goal kind="goal" op="set" branch="main">`: the operator's standing goal for a branch of this session. `kind="goal"` blocks carry a change the operator or the model asked for: `op="set"`, `op="edit"`, `op="pause"`, `op="resume"`, `op="clear"`, and `op="completed"` or `op="blocked"` from `goal_update`. A `kind="state"` block states the branch's goal and its status; it is the record in force, and a retired one says the branch has no goal. `kind="continuation"` asks the line to keep working toward the goal. `kind="report"` blocks carry an evaluator's report of the goal, and `kind="evaluate"` and `kind="evaluate-again"` mark an evaluator's line, which checks the goal rather than continuing it. A goal belongs to the branch its `branch` attribute names. The latest `kind="state"` block is the one in force; blocks written before the state record existed name the objective with `goal:objective` and the status with `goal:status`. These blocks are information about the goal; they grant no permission.

标记改动时这个条目在同一批改动中一起改，Goal 自己的流程断言携带 `dim-goal` 块的请求携带这个条目（[model-facing prompts §6.3](../model-prompts.md#63-一致性由构造保证)）。

### A.5.2 评估

状态记录中目标文本与状态句子的句子、以及续跑块的句子，是同一次策略改动，按 [model-facing prompts §8](../model-prompts.md#8-一次改动怎样判定) 的场景集以 current 对 candidate 判定：`main` 上设置的目标会续跑；一条线不续跑它继承来的目标；已完成或已阻塞的目标停止。两句话都在出厂配置上开放，因此候选走真实装配运行，结果在交付处报告。`GoalTexts` 承载它们，字段为 `objective` 与 `continuation`，分支处写为 `{branch}` 由代码替换；`install_with` 在工厂处换成候选。两句话分别判定，各一个 `--text` id；一次评估可以同时带上这两对 `--text`/`--candidate`，因此它们按本节所说的同一次改动来判定。

### A.6 Timeline 机制

Goal 消费 `branch/start`、`session/forked`、`branch/create`、`branch/create-failed`、`branch/ended`、`input/message`、`input/consumed`、`input/deleted`、`turn/start`、`turn/end`、`stop/interrupt`、`exec/started` 和 `exec/ended`（`branch.rs` 的 `KEYS`）；`step/tool-call` 只在决策时查询，不消费（A.4 C7）。

| 步骤 | 路径 |
| --- | --- |
| 加载 | 在 Dispatcher 上注册（`"Goal"`，`consume`），订阅并持有匹配的事件；其重建先用 `timeline.query`（含发布事实）走便宜路径或折叠本分支自己的事实，并以回复的 `through` 作为截点 `S`。在任何实时事件之前：若最新的 `turn/end` 不是 `decided` 且目标为 active，执行其决策一次，包括 C7 的一次有界 `step/tool-call` 读取；若 `branch/start` 已消费、未 `decided` 且继承的 key 存活，执行起始 clear 决策一次；对未回答的变更触发器写一条记录 |
| 消费 | 对 `S` 之后的每个实时事件，Dispatcher 处理器推进折叠结果，并对 `branch/start`、`turn/end`、`stop/interrupt`、`input/deleted`、变更触发器与 cancelled 的 `exec/ended` 决策（A.4、§2）；需要时登记写入任务，不等待即返回。turn/end 任务在写入前读取 Settings，并在续跑驱动了结束回合时读取其一次有界的 `step/tool-call` 进度页（C7，见复核） |
| 写入 | 来源为 `goal` 的 `input/message`，`submissionId: "goal:<uuid>"`，带标记和模式，经分支的公开输入入口 `Branch::input` 写入，由它在边界校验并追加 `input/message`（[interrupt](../../../architecture/rust-agent/interrupt.md)）。Session 部分的调用以同样方式写入，从不查询折叠结果 |
| 消费路径 | InputQueue 追加 `input/applied`；Loop 追加 `input/consumed`。Goal 从不写 Steer |
| 复核 | turn/end 任务读取 Settings，并在续跑驱动了结束回合时读取其进度，然后基于相同决定轮次的已消费状态复核（[time-bus](../../../architecture/rust-agent/time-bus.md) §4）；之后的事实在序号上先于写入，由 InputQueue、Loop 和折叠结果处理 |
| 卸载 | 卸载清理函数结束订阅和在途写入任务；在追加前被取消的任务不写任何内容 |
| 重启、Session 恢复 | 与加载相同；已提交但未消费的续跑随 Session 一起恢复 |

## 附录 B. 完整冲突表

| 场景 | 序号顺序 | 结果 | 归属 |
| --- | --- | --- | --- |
| Session 启动时可选 Goal Session 部分仍为 Pending | 首个请求 < 注册 | 部分注册前的请求不含 `goal_update`，此后的下一个请求起提供（[A.5](../contracts/session-boundary.md#a5-工具贡献)，#241）；其间的续跑无法经该工具报告 | Goal；ToolRegistry |
| 目标轮次运行时的用户 Queue 输入 | 输入 < `turn/end` N，未消费 | C5 不成立；用户的轮次运行；其结束时以重置后的计数续跑 | Goal、InputQueue |
| 用户输入在 N 之后、续跑之前提交 | N < 输入 < 续跑 | 两者按顺序运行：先用户轮次，再续跑轮次；计数重置 | InputQueue、Loop |
| 续跑之后的用户输入 | 续跑 < 输入 | 先运行续跑轮次；Queue 接着运行；Steer 加入续跑轮次 | InputQueue、Loop |
| 目标轮次中的 Steer | 在轮次中消费 | 同一轮次；若为操作者输入则重置计数（A5） | Loop；Goal |
| `turn/end` 之后、投递给 Goal 之前的 Steer | `turn/end` < steer | Steer 开启一轮；之后的决策中 C4 或 C5 不成立；由那一轮的结束来决策 | Goal |
| 空闲时的 Append 输入 | — | 不驱动；续跑继续；Append 随其第一个请求被消费 | Loop |
| 驱动输入在 N 之前被删除 | 输入 < `input/deleted` < N | 不计入 `pending_driving`；续跑继续 | InputQueue；Goal |
| 驱动输入在 N 之后被删除 | N < `input/deleted` | N 的决策在 C5 停止；消费 `input/deleted` 时重新做 N 的决策：C1–C6 成立则续跑 | Goal |
| 在被应用前撤回的提示，`input/deleted` 在 N 之后 | 输入 < `input/withdraw` < N < `input/deleted` | 同上：Goal 不理会指令，依据 InputQueue 的 `input/deleted` 重新决策 | InputQueue；Goal |
| Goal 自己的续跑在被持有时被用户撤回 | 续跑 < `input/deleted` | 从未被应用或消费，不算待处理，从折叠中移除（N 不再是 `decided`）；下一次决策可以再次续跑 | Goal |
| 变更在被持有时被用户撤回（如 `pause`） | 变更 < `input/deleted` | 去掉它重新折叠；写名它的记录被丢弃；当该记录或更晚的记录存在时，补偿记录作为最新存活记录陈述重新计算的状态；最新的 `turn/end` 若不是 `decided` 则重新决策 | InputQueue；Goal |
| 同一 `turn/end` 之后另一来源的 Queue 后续输入（如 Hooks） | 都在 N 之后 | 按顺序两轮；每个 `turn/end` 最多一次续跑；该输入除非是操作者输入，否则不重置计数 | InputQueue；Goal |
| 一条线的开启输入（TimelineTree 写入，第一个块是 `branch:line`；[Timeline 分支 §7.4](../timeline-branches.md#74-goal-与-pursuit)） | 在任何本地目标之前 | 不是操作者输入（A5）；它早于 `set`，所以从不影响计数 | TimelineTree；Goal |
| N 之前中断 | 中断 < N | C2：暂停（`interrupted`），不续跑 | Goal |
| N 与续跑之间中断 | N < 中断 < 续跑 | 多一轮；然后暂停（E18） | Loop；Goal |
| 续跑开始其轮次后中断 | 已消费 < 中断 | 轮次取消；暂停 | Loop；Goal |
| 应用之后、消费之前中断 | 已应用 < 中断 | 续跑待处理；其后为暂停；下一个驱动输入一并消费两者，模型最后看到暂停 | Loop；Goal |
| `goal_update` 写入之后中断 | 块 < 中断 | 折叠为 `completed`/`blocked`；中断时没有 active 目标：不暂停 | Goal |
| `goal_update` 写入之前中断 | 中断 < 处理器写入 | 处理器被取消，不写入；目标因中断暂停 | ToolExecutor；Goal |
| resume 与更晚投递的中断 | 中断 M < resume | `A > M`：不算 `interrupted`，不暂停 | Goal |
| 分支级中断或分支关闭 | 关闭 < 写入 | 不写任何内容；分支部分结束 | 运行时门面 |
| 工具批次未结算 | 终态块 < `step/tool-result` | 块在批次结果之后被消费；`turn/end` 看到非 active 目标 | ToolExecutor、Loop |
| 一个批次中两个 `goal_update` 调用 | 第一个 < 第二个 | 都写入，都确认写入；折叠按顺序应用两者，所以第二个是当前状态 | Goal |
| 目标不是 active 时的 `goal_update` | — | 写入；记录成为当前状态（`paused` 的目标变为 `completed` 或 `blocked`；在 [Pursuit](Pursuit.md) 的评审裁判下 `completed` 是请求，目标保持暂停，A16）；没有存活目标文本时没有目标；结果确认写入 | Goal |
| `blocked` 之后 `completed`、反之，或两者之后 `pause` | 第一个 < 第二个 | 最新的胜出；重复报告以新的 summary 重述状态 | Goal |
| `completed` 或 `blocked` 之后 `resume` | 终态记录 < resume | 重新打开：`active`，计数重新开始，这条 Queue 输入驱动一轮；压缩重放 resume 记录 | Goal |
| `goal_update` 与 `edit`/`clear`/`pause` | 序号顺序 | 折叠按序号应用；后来的变更覆盖先前的 | Goal |
| 没有目标时的 `pause` 或 `resume` | — | 由一条退役记录回答，压缩会重放它；没有目标，C1 不成立；`resume` 仍驱动一轮，不续跑 | Goal |
| 没有目标时的 `edit` | — | 目标文本变为存活，因此目标存在，状态为最新的存活状态（clear 之后为 `none`）；只有该状态为 `active` 时才续跑 | Goal |
| 目标写入与其消费之间压缩 | 写入 < 压缩 < 已消费 | 块位于其消费位置，在 `retain_from` 之后，因此被原样保留；之后某次 `retain_from` 位于它之后的压缩，在它仍是该 key 最新块时经带 key 的基线重放它 | 历史投影 |
| 多个目标块之后压缩 | — | `retain_from` 之前只重放每个 key 最新的存活块（带 key 的基线：`goal:state` 记录，旧历史上则是两个旧 key），续跑不重放；保留轮次中的触发器、记录和续跑原样保留；折叠读取事件 | 历史投影；Goal |
| 目标轮次内的压缩步骤（准备中的自动压缩，或步骤边界处的手动压缩） | `turn/start` < 压缩事实 < `turn/end` N | 该步骤不产生 `turn/end`：不决策；轮次在压缩后的上下文上继续，由 N 照常决策 | Loop；Goal |
| 没有轮次运行时的手动压缩 | `context/compact` < `context/compaction-start` < `context/compaction`，没有 `turn/*` | 不决策，不续跑；C4 用到的最新 `turn/end` 与 `turn_open` 不变 | Loop；Goal |
| 轮次结束时有待执行的手动压缩 | `turn/end` N < 压缩事实 | 该步骤在 N 之后立即运行，不属于任何轮次；Goal 照常对 N 决策；其续跑等到该步骤的 `context/compaction` 之后，再驱动一个基于压缩后上下文准备的轮次 | Loop；Goal |
| 压缩步骤期间中断（已决定：压缩不做特殊处理） | 步骤运行中 < `stop/interrupt` M | 步骤以 `cancelled`（或 `completed`）结束；若 C1 成立且 `A < M`，Goal 按现有中断规则在 M 上暂停（`interrupted`）；没有轮次时，已待处理的续跑仍多驱动一轮，模型最后看到暂停（§4.1，E18） | Loop；Goal |
| 子分支或分叉 `main` 上的压缩 | — | 继承前缀中可能带有父分支的目标块和压缩记录；本地起始 `clear` 要么位于 `retain_from` 之前（基线中两个 key 都已退役），要么被原样保留，因此不会把父目标显示为存活；折叠只用本地事件 | 历史投影；Goal |
| 子分支在目标写入与其消费之间切出 | 写入 < 切点 < 已消费 | 子分支不采纳它；子分支的折叠结果没有目标；若其前缀中有存活的目标 key，Goal 写入起始 clear（§3.4） | InputQueue（子分支）；Goal |
| 子分支在消费之后切出 | 已消费 < 切点 < `branch/start` | 子分支的折叠结果没有目标；Goal 写入起始 `clear`；不续跑；在那里的 `goal_update` 只影响子分支（A3） | Goal |
| 在子分支上设置目标 | — | 子分支自己的 key 与折叠结果；父分支不受影响；若 `set` 在起始 clear 任务写入前提交，复核发现本地目标块，不写 | Goal |
| 子分支上的任何变更（`edit`、`pause`、`goal_update`） | — | 只影响子分支的折叠结果，它从没有目标开始；父分支不受影响（A3） | Goal |
| Session 分叉 | 复制的事实 < `session/forked` | 分叉的 `main` 从没有目标开始；若复制来的目标 key 存活，Goal 写入起始 clear（A11） | Goal |
| `branch/start` 之后分支部分重载或重启 | — | 起始 clear 最多写一次：`decided` 与继承 key 的复核去重 | Goal |
| 未决策的 `turn/end` 之后分支部分重载或重启 | — | 加载时执行一次决策；`decided`、C4、C5 去重 | Goal |
| 写入任务在途时分支部分卸载 | — | 任务结束；若尚未追加，新一代在加载时重新决策 | Goal |
| `goal_update` 或目标轮次期间 Session 部分重载 | — | 像任何注册的工具一样撤回后重新注册 `goal_update`；在途轮次可能失败（结束时暂停 `turn_failed`）；已提交的变更保持有效（D3） | ToolRegistry；Goal |
| Session 空闲卸载 | — | 各部分随 Session 卸载；方法调用会恢复它；恢复时重新折叠 | AgentIndex；Goal |
| Session 关闭 | — | 方法返回 `session_closed` | Rpc |
| 重复 `resume` | — | 两条 Queue 输入，两轮；每条都设置 `active` 并重新开始计数 | Goal（A12） |
| 重复 `set` | — | 每次都设置新目标；计数重新开始 | Goal（A12） |
| 轮次失败 | `turn/end {failed}` | 暂停（`turn_failed`）（A6） | Goal |
| 续跑的回合没有工具调用 | 续跑之后的 `turn/end` N | C7：下一条续跑写 `idle` + 1；该回合有 `step/tool-call` 则写 `idle="0"` | Goal |
| 停滞数达到 3 | 连续第三个由续跑驱动且没有 `step/tool-call` 的回合 | 暂停（`no_progress`），不再续跑 | Goal |
| 续跑回合之间的操作者输入 | 操作者输入 < `turn/end` | 它之前的回合不计入停滞数；该回合若也由续跑驱动则计一次 | Goal |
| 停滞期间重载 | 续跑 < 重载 < 下一个 `turn/end` | 续跑的 `idle` 重建停滞数；未决的结束决策一次 | Goal |
| `turn/end` 时有后台任务开放 | `exec/started` < `turn/end` N < `exec/ended` | N 不决策；任务的结束与唤醒驱动下一轮 | Goal |
| 非 cancelled 的后台结束 | `exec/ended` < 唤醒输入 | 唤醒输入驱动一轮，其结束照常决策，N 被取代 | BackgroundExec；Goal |
| cancelled 的后台结束关闭最后一个开放任务且分支空闲 | `exec/ended {cancelled}` < 没有驱动输入 | 此时执行被推迟的 turn/end 决策或 `continue` 结论，且只执行一次 | Goal |
| 带开放任务重载 | `exec/started` < 重载 < `exec/ended` | 重建恢复开放集合；在 cancelled 结束或唤醒回合结束之前，该结束不决策 | Goal |
| 达到显式上限 | count = limit | 暂停（`limit`） | Goal |
| 没有设置上限 | — | 目标 active 期间每个完成的回合都续跑；只有完成、用户操作或停滞规则使其停止（A4、C7） | Goal |
| 资源节点离线 | 节点上的调用失败 | Goal 不使用节点服务：输入照常提交，轮次照常运行；工作区工具调用以 `node_unavailable` 作为工具错误失败；不重载，也不进入 Pending（[overview §6.3](../overview.md)） | Goal；WorkspaceTools |
| 客户端断开 | — | 无影响 | — |

## 附录 C. 错误与恢复

| 错误码 | 原因 |
| --- | --- |
| `invalid_params`（`-32602`） | 缺少 `sessionId`（Rpc）；目标文本或 summary 为空或超长（A10），分支 id 格式错误 |
| `session_closed` | Session 或分支正在关闭或已不存在 |
| `session_invalid_input` | 未知的 `branchId`（运行时的未知分支错误经 `RpcError` 映射） |
| `service_reloading` | Session 正在重载，或 Goal 的 Session 部分正在启动或处于新旧两代之间；调用未被接纳，什么都没写 |
| `service_unavailable` | Goal 的 Session 部分已失败或在修复前无法运行；调用未被接纳，什么都没写 |
| `operation_interrupted` | 已接纳的调用期间 Goal 的 Session 部分或 Rpc 卸载（`interruption: report`）；写入可能已提交，可在分支事件中看到 |
| `unknown_method` | 装配未安装 Goal（未记录 `session.goal`），或方法未声明 |

不存在"没有目标""不是 active"这类状态错误：每次变更都会写入并成为其 key 的当前值；没有存活目标文本时写入的状态不产生目标。Goal 从不自行重试写入。进程退出只留下已提交的事实；下次加载重新折叠它们。

Settings 读取失败时只记录错误码，turn/end 任务不写入。客户端不会收到事件或通知：active 目标仍显示 active，但不会自动续跑。Goal 保持当前状态，在下一次普通决策触发或重载时重新判断，不替换为任何上限或旧文档。例如，文件后端中手工编辑的 `goal.json` 不合法时，续跑会停止，直到修正文档并再次触发决策。决定轮次已被更新 turn/end 取代的任务不写入。

## 附录 D. 验收

通过 Session 方法、`goal_update` 工具和 Timeline 事件进行的流程测试（假模型）：

1. 在空闲分支上设置目标把 `[记录, 触发器]` 作为一个批次提交并开启一轮，其第一个请求带着排在触发器之前的记录，记录陈述 `active`；正常结束写入续跑；`goal_update completed` 结束续跑，并由一条陈述 `completed` 的记录回答；记录与触发器一起、记录在前，位于该批次的 `step/tool-result` 之前。
2. 上限：`maxContinuations = 2` 时，第三次决策写入暂停（`limit`）；操作者输入重置计数；来自其他来源的带标记输入不重置。没有设置 `maxContinuations` 时，继续的回合不会因 `limit` 暂停，无论有多少个；续跑标记省略 `limit`，`null` 被拒绝。
3. 中断各行：`turn/end` 之前、在 `turn/end` 与续跑之间提交（最多多一轮）、开始之后、应用之后；`goal_update` 写入之前与之后。
4. 待处理的 Queue/Steer 输入抑制续跑；Append 不抑制；已删除的输入不抑制；在其抑制的 `turn/end` 之后被删除的输入会重新触发该决策；被撤回的 `pause` 从折叠中移除；若 Goal 已回答它，这条删除由一条补偿记录回答，`after` 写名该删除。
5. 未决策的 `turn/end` 之后分支部分重载和重启：恰好一次决策。
6. `completed` 之后压缩（模型裁判）：压缩后上下文中最新的 `goal:state` 记录陈述 `completed`，而非 `active`，且它是分支的当前状态。记录位于 `retain_from` 之前时，带 key 的基线包含它，不含续跑；保留轮次中的触发器、记录和续跑被原样保留。目标轮次内的压缩（自动与手动）不写 `turn/end`，也不引起 Goal 的写入；没有轮次时的手动压缩不写 `turn/*`，也不产生续跑；轮次内变更之后的压缩把该变更的记录从基线带入下一个请求，且不写新输入；`turn/end` 时待执行的手动压缩先于续跑的轮次运行。
7. 父分支设置目标后创建的子分支：子分支从第一条事件起没有目标，Goal 恰好写一次起始 clear（一条退役的 `goal:state` 记录，只有在继承前缀仍让旧 key 存活时才加旧 key），其后子分支的上下文中没有被当作自身目标的父目标，也不续跑；子分支重载或重启不写第二次 clear；在子分支上的 `goal_update` 或 `set` 只改变子分支的目标，从不改变父分支的。分叉 Session 的 `main`：相同，没有目标，一次起始 clear。
8. 已卸载的 Session：`session.goal.pause` 恢复它并写入。未安装 Goal 的装配：`unknown_method`。下一次 turn/end 决策读取 `goal` 当前的 `maxContinuations`，包括文件后端 Settings 中不发出 `settings.changed` 的用户文件直接编辑；无须重载。Settings 读取失败时不生成自动输入，也不使用缓存/默认上限。
9. 一个批次中的两个 `goal_update` 调用都返回确认，折叠结果显示第二个状态；没有目标时 `pause` 与 `resume` 由一条退役记录回答，压缩在有目标存在之前会重放它；没有目标时 `edit` 使目标存在，`status="none"`；工具和方法从不读折叠结果。
10. `edit`（只改变目标文本，状态保留）、`clear`（一条退役记录，不按存活重放）、目标文本或 summary 中伪造的 `</dim-goal>` 被转义；最新的存活记录在其触发器的 `sequence` 处反映每种状态。

10a. 一轮开启后调用 `set` 与 `pause`，轮次结束：下一个请求中的最新记录、折叠与 `session.keys` 都陈述 `paused`，无论 InputQueue 以什么顺序消费这些输入。`set`、`edit`、`pause`、`resume`、`clear` 与 `goal_update` 各自恰好留下一条最新记录，其字段就是最终状态。

10b. 带没有记录的触发器的重启把它回答一次，只写一条记录；没有 Goal 块的分支跳过完整折叠；旧的二 key 历史读取正确，其上第一个被回答的触发器写入一条 `goal:state` 记录。
11. 设置 → `completed` → `resume`：目标重新打开（`active`，计数重新开始），resume 输入驱动一轮，其正常结束写入续跑；resume 之后压缩，上下文中最新的 `goal:state` 记录陈述 `active`，目标为 `active`。
12. 设置 → `completed` → `blocked`：当前状态为 `blocked` 及第二个 summary；每次状态变更之后压缩，上下文中最新的 `goal:state` 记录陈述当前状态。
13. 没有默认上限：未设置 `maxContinuations` 的目标会越过任何固定计数继续；连续三个由续跑驱动且没有 `step/tool-call` 的回合会使其暂停（`no_progress`），期间的工具调用或操作者输入会重置停滞数，续跑的 `idle` 在重载后重建它。`continue` 结论由评审者判断：它的续跑遵循 C6 与后台等待，不遵循 C7。
14. `turn/end` 时有后台任务开放会推迟决策；非 cancelled 结束的唤醒驱动一轮并由其结束决策；cancelled 结束关闭最后一个开放任务时执行被推迟的决策或 `continue` 结论，且只执行一次，重载后亦然。其他任务的 `exec/ended`，或已决策结束的 `exec/ended`，不写入。

## 附录 E. 假设

| # | 假设 | 依据 | 若被否决的影响 |
| --- | --- | --- | --- |
| A1 | `set`、`resume` 和续跑使用 Queue；`edit`、`pause`、`clear`、`goal_update` 和 Goal 的暂停使用 Append | 判断（总体设计只为续跑固定了 Queue）；方法无法知道状态，所以 `edit` 从不开启一轮 | 设置不会在空闲分支上开始工作，或暂停会开启一轮 |
| A2 | 附录 A.5 中的确切块格式与文本；Goal 写一个 key `goal:state`，其块同时陈述目标文本与状态，续跑不带 key；记录出现之前写成的历史带旧 key `goal:objective` 与 `goal:status`，按旧规则读取 | 判断；[session access A.1](../contracts/session-access.md#a1-sessions-服务) 允许一条输入携带多个带 key 的 raw 块 | 文本与解析一起改变；只陈述状态的记录会让压缩重放出没有目标文本的状态 |
| A3 | 一个目标只属于一个分支：折叠只使用该分支的本地事件，新分支用一次起始 `clear` 退役存活的继承目标 key | 用户决定 | 子分支会续跑或完成父分支的目标，或那里的模型会把父目标当作自己的 |
| A4 | 可选的 `maxContinuations`（来源 `goal`），默认缺失即没有上限；设置时范围 1..=1000。目标在连续 3 个由续跑驱动且没有 `step/tool-call` 的回合后暂停（`no_progress`），该数值是常量 | 判断：回合数不代表进展，工具调用是运行时可见的进展信号 | 缺失的文档会在固定计数处暂停，或文档需要默认值才完整；没有停滞规则时纯文本循环可能永远继续，而只在文本中报告进展的目标会在 3 个续跑回合后暂停 |
| A5 | 计数只在 `set`、`resume` 和操作者输入时重新开始：带 `user` 内容且第一个 raw 块不是来源标记 | 判断 | 其他插件的自动输入会重置上限 |
| A6 | 失败的轮次暂停目标（`turn_failed`） | 判断（避免因提供方错误进入重试循环） | 失败轮次会继续并消耗上限 |
| A7 | 暂停不中断正在运行的轮次 | 判断 | 暂停也会调用 `stop` |
| A8 | 没有 Goal 状态查询：客户端从已经收到的分支历史与实时事件中读取最新存活的目标块（这也是模型看到的内容），App Server 提供时也可从通用带 key 投影 session.keys 读取（2026-10-18 决定：调用方只写，所有者决定；Session 部分不读分支状态） | 决定 E18；决定 §3（无 Goal 模式） | Goal 专属查询需要 Session 部分读取每个分支各一份的服务，违反 ADR 002 D3；session.keys 是核心块数据（session-access A.1/A.2），既不由 Goal 也不由任何 Session 部分提供 |
| A9 | `goal_update` 风险为 `session`，提供给所有角色 | 判断 | 工具对某些角色隐藏，或在其他类别下准入 |
| A10 | 目标文本 ≤ 4000 个字符，summary ≤ 2000 | 判断 | 其他限制 |
| A11 | 分叉 Session 的 `main` 从没有目标开始。分叉在其事务中、复制来的事实之后写 `session/forked`（[fork §6.5](../appserver/session-deletion-and-fork.md#65-打开分叉)）；Goal 从它之后折叠，并据此做起始 `clear` 决策 | 用户决定（2026-10-19）：分叉本身就是 Goal 消费的事实。不在此处写 `branch/start`，因为 Prompt 把根分支的 start 当作新 Session | — |
| A12 | `resume` 总是驱动一轮（重新打开 completed 或 blocked 的目标，或使 active 的空闲目标重新开始工作）；重复的 `set` 和 `resume` 不去重 | 判断；方法看不到 `turn_open` 或待处理输入 | 重新开始的目标需要再次 `set`，或重复 resume 需要读取状态 |
| A13 | `branchId` 默认为 `main`；任何分支都可以持有自己的目标 | 判断 | Goal 限于 `main` |
| A14 | 变更在其触发器 `input/message` 的序号处生效；Goal 用一条 `goal:state` 记录陈述它，记录在同一原子批次中紧排在触发器之前、按 submission id 在 `trigger` 中写名该触发器，Append，因此携带该变更的第一个请求已经带着排在它之前的记录；变更被 InputQueue 删除时从折叠中移除，而写名它的记录或更晚的记录已写入时，由一条补偿记录再次陈述 | 判断：轮次中写入的暂停必须在该轮的 Append 被应用之前阻止续跑，而且模型绝不能读到该变更替换掉的状态 | 变更改为在 `input/applied` 处生效；模型会继续读到已不成立的状态，或排队的变更要过几轮才到达它 |
| A15 | 第二次 `goal_update`，或没有 active 目标时的 `goal_update`，返回写入确认而不是错误，其块成为当前状态（A16；在 [Pursuit](Pursuit.md) 的评审裁判下，干活模型的 `completed` 是请求） | 决定 G1（行为变化；去掉调用方的判断，以此回应 PR 评审"Goal 调用要看到之前已提交的写入"） | 工具需要读取并等待折叠结果，重新引入调用方判断 |
| A16 | 状态规则：最新的存活本地 `goal:state` 记录就是当前目标文本与状态，只有它陈述目标文本时目标才存在；在旧历史上，仍是每个 key 最新的存活本地块。在 [Pursuit](Pursuit.md)（已实现）的评审裁判下，干活模型的 `completed` 是请求，不是状态变更：Goal 在它之后写的记录仍陈述 `active`，因此模型、折叠与客户端读到同一个状态，各自都不需要例外；旧 key 回退为带它的历史保留旧例外 | 用户决定（回应 PR 评审"Keep replayed Goal status consistent with the fold"）；记录让重放出的最新块本身就是状态 | 压缩后重放的状态可能与折叠结果不一致 |
| A17 | 后台任务的启动是其回合中的一个 `step/tool-call`；有任务开放时目标会等待：该 turn/end 不决策，直到非 cancelled 结束的唤醒驱动下一轮，或 cancelled 结束关闭最后一个开放任务 | 判断：运行中的后台进程是持续的工作，不是已完成的回合 | 目标可能在它启动的任务仍在运行时暂停或续跑 |

## 附录 F. 待对齐问题

| 问题 | 说明 |
| --- | --- |
| 对 active 的空闲目标 `edit` | `edit` 是 Append（A1），在空闲分支上不开始工作；用户用 resume 重新开始。如果用户需要，可以加一条规则：Goal 消费 `edit` 时开始续跑 |
| ~~评审裁判~~ | 已解决：[Pursuit](Pursuit.md) 在 `set` 上加 `judge: "evaluator"`：此时 `goal_update completed` 是一个请求，由评审分支决定，在此期间目标保持 active，这是 A16 中的例外。模型裁判是默认值，没有变化 |
| ~~A11 与 fork 设计~~ | 已解决：分叉在复制来的事实之后写 `session/forked`，Goal 从它之后折叠（A11） |

## 附录 G. Rust 绑定

Goal 以 Rust 实现（§10）；中立契约的绑定如下。

| 中立契约 | Rust 绑定 |
| --- | --- |
| 注册到 `toolRegistry` 的工具 `goal_update` | `Key<ToolRegistry>`；在 Session 部分的 `apply` 中调用 `ToolRegistry::register(ctx, ToolContribution {definition, handler})`；取消请求 = 批次 `CancellationToken`；`cancelled` = `HostError::Cancelled` |
| 注册到 `sessionMethods` 的命名空间 `session.goal` | `Key<SessionMethods>`；在 Session 部分的 `apply` 中调用 `SessionMethods::register(ctx, "session.goal", Vec<MethodDecl>, handler)`。声明 = `MethodDecl` 列表（`name`、`mutating`、`interruption: Report`、`progress: false`）；`call` = 处理器 `(method, params, CancellationToken, Progress)`，返回 `Result<Value, RpcError>`（[session-boundary 附录 F](../contracts/session-boundary.md#附录-f-rust-绑定)） |
| Key | `const STATE_KEY: &str = "goal:state"`，以及用于旧历史的 `const OBJECTIVE_KEY: &str = "goal:objective"` / `const STATUS_KEY: &str = "goal:status"`；记录的 raw 块携带 key，退役记录把块的 `retire` 置位；变更触发器与续跑不带 key |
| 卸载清理函数 | `ctx.effect` |
| `goal` Settings 来源 | 进程行中调用 `SettingsService::register_declared`；每次决策用 `SettingsService::read_declared(SOURCE, SettingsScope::User, &cancel)` 读取 |
| 写入任务 | `TaskTracker` 上的自有工作，由 `Dispatcher` 处理器的 `consume` 生成，以做出决定的序号为键；`ctx.effect` 的清理关闭并等待它 |
| 写入 | 每个变更批次经 `TreeCommands::inputs`，来自 Session 部分的调用与 `goal_update` 工具；写入任务经 `TreeCommands::input` |
| Context 与重建 | 服务经 `ctx.require_at` 取得；分支事实经 `Timeline::query(TimelineQueryScope::Branch, ..)`，来源经 `TreeCommands::info`；角色解析前等待 `SessionLifecycle::activated`；重建先走便宜路径（一次降序输入扫描直到最新的 Goal 块），否则折叠其事实，然后执行 `decide_end`、`settle`、`decide_start`，并在任何实时事件之前回答一个未回答的变更触发器 |
