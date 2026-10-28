# Timeline 分支

状态：已采纳（[#427](https://github.com/arcships/dim/issues/427)）。本文同时是需求与设计：§1–§3 回答 #427 的问题，§4 起是设计。范围：TimelineTree 插件（TimelineTree、TreeTimeline、BranchTool，BranchTool 新增一个分支部件）、一个 App Server 内置方法、两种核心事件、CLI TUI，以及[设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义) 的改写。语言：Rust。English: [English](../../design/timeline-branches.md)。

## 1. 分支用来做什么

**分支**是同一个 agent 在一个 Session 里的一条行为线。Session 是一个主体，也就是"我"：它的身份、它与用户的交互面、它的能力和它的工作区。分支是这个主体的一条行为线。

子分支从父分支的历史中某个切点开始。切点之前的每个事件也是子分支自己的历史，这些行不会改变。切点之后，子分支只读自己的事件（[timeline.rs](../../../crates/dim-agent-runtime/src/core/timeline.rs)）。

分支之间共享事实，不共享上下文：

- 每条线用自己的事实组装自己的上下文。
- 每条线各自压缩。
- 一条线影响另一条线，只能向那条线的 Timeline 写输入。

父子关系记录历史的来源，也是分支之间唯一可以通信的关系。它不表示控制，也不表示欠对方一个结果。

对 #427 的回答：

| # | 问题 | 回答 |
| --- | --- | --- |
| 1 | 支持哪些场景，按什么顺序 | 三个，依次是（§3）：插件开的线（已交付）、agent 从 main 开的线（本设计）、用户从历史某点开线、再比较和采纳（以后） |
| 2 | 分支、Collaboration 成员、fork 怎么区分 | 见 §2。分支是同一个 agent；成员和 fork 是另一个 Session |
| 3 | 分支结束时，谁需要它的结果，以什么形式 | 没有哪条线欠另一条线结果。各线在工作过程中把要紧的事告诉对方（§6）。agent 开的线结束时给 main 一条最终消息（§10）。插件开的线按创建者的协议办，例如 Pursuit 的结论 |
| 4 | 分支什么时候算结束，结束后还能看到吗 | 它自己结束、开它的一方结束它，或者用户结束它（§10）。结束的线留在分支目录里，客户端和 main 仍能读到它的历史 |
| 5 | 用户能看到什么、做什么 | 跟随任一条线、与它对话、回答它的提问和审批、结束它（§9、§14）。比较和采纳随场景 3 一起做 |
| 6 | 怎样算"完成" | 验收流程（§20）通过，评估场景（§12.3）已在本地用真实模型跑过，并在实现 PR 里报告。发布演示（[#439](https://github.com/arcships/dim/issues/439)）可以展示它 |

## 2. 分支、Collaboration 成员、fork

| | 分支 | Collaboration 成员（subagent） | Fork |
| --- | --- | --- | --- |
| 是什么 | 同一个 agent 的另一条线，在同一个 Session 里 | 另一个有自己档案的 agent，在它自己的 Session 里 | 从 `main` 复制出来的一个新的、独立的 Session |
| 历史 | 父分支切点之前的历史，之后是它自己的 | 从任务开始，没有 leader 的历史 | 复制 `main` 到某个静止点为止的历史（[fork](appserver/session-deletion-and-fork.md)） |
| 身份与能力 | Session 的：同一套 system、工具、模型、权限和工作区 | 它自己的档案：指令、工具和模型 | 配置的副本，之后各自独立 |
| 关系 | 父子在工作过程中互相写输入。没有任务约定，也没有控制 | leader 给它任务，结果自动返回。成员不能再委派 | 复制之后没有关系 |
| 何时用 | 同一个 agent 在同一批事实上并行工作 | 工作需要另一个身份、另一份档案，或单独的上下文 | 要从过去某点在别处继续，而不动这个 Session |

设计规则 §2 的规则不变：工作需要不同的身份或能力时，建一个新的 Session（成员或 fork），绝不开分支。

## 3. 场景

| # | 场景 | 状态 |
| --- | --- | --- |
| S1 | **插件开的线。** 插件按自己的设计开线。例如 [Pursuit](plugins/Pursuit.md) 的评审者：它是 host 来源的子分支，切在执行者那一轮结束处，结论以输入的形式写回（已实现，[#422](https://github.com/arcships/dim/issues/422)；`runtime/plugins/goal`） | 已交付。本设计给它加上身份块（§7.1）。Goal 对评审者的识别和结束随之改变（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） |
| S2 | **agent 从 main 开线。** 例如：main 继续读代码时，在两条线上分别验证一个 bug 的两种假设；main 实现的同时，让一条线随改动逐个评审；main 继续工作时，让一条线调研某个 API 或代码的某个区域；在另外的文件上尝试第二种做法 | 本设计 |
| S3 | **用户从历史某点开线、比较、采纳。** `session.branch_create` 已经能开 host 线。有了本设计，用户可以跟随这些线、与它们对话、结束它们。比较和采纳需要单独设计：采纳一条线，对 main 的上下文和共享的工作区意味着什么 | 以后，在本设计之后 |

下面这些需求不是分支：

| 需求 | 为什么不是分支 | 归属 |
| --- | --- | --- |
| 委派给有自己指令的专门角色 | 那是另一个身份 | Collaboration 成员 |
| 判断是否批准一次工具调用 | 那是对一个动作的决定，不是一条工作线 | 带宿主端口的能力（[ADR 003](../adr/003-approval-scope.md)） |
| 把 main 回退到更早的某点 | 它改变的是 main 如何看自己的历史，不是一条新线 | 单独的需求。目前的答案是 `session.fork` |
| 隔离地并行编辑 | 各线共用 Session 的工作区，没有按线隔离的工作区 | fork，或有自己工作目录的成员 |

## 4. 树与角色

- **深度。** 架构上分支可以有任意深度。`session.branch_create` 接受任意父分支；Goal 接受任意 `branchId`，所以开在子分支下的评审者是孙分支。
- **亲缘。** 父子是分支之间唯一可以通信的关系。父分支和它的每个子分支互相写输入；兄弟分支、更远的亲属之间不通信（§6）。这条规则约束的是一条线作用于另一条线，在这里就是 `branch` 工具。能力按自己的决定行事，属于能力影响分支，不受这条规则限制（[设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义)）。例如 Goal 写入评审结果，或插件在任意切点开线。
- **agent 的工具。** `branch` 工具只能从 `main` 开线。`main` 是根线，没有父分支。用工具开出的线是叶子，不能再开线。所以 agent 只有一个协调者 `main`，每条工具线都和 `main` 对话。
- **来源。** 每条线都有来源，记在父分支的 `branch/create` 里：
  - **工具线**由 `branch` 工具开出。
  - **host 线**由插件（`TreeCommands.create`）或客户端（`session.branch_create`）开出。它的协议归创建者。

  来源可以从 `TimelineInfo.source` 读到，由 `TreeCommands.list` 和 `info` 返回，`main` 上没有（§5）。host 线的来源是 `{kind: "host", operation_id}`：开线一方的不透明标签，核心从不解释它。开线一方按自己的 operation id 约定找到自己的线，例如 Goal 的 `goal:evaluate:` 前缀。客户端开的线带的是 App Server 的 id。
- **工具的作用范围。** 工具的 `message`、`read`、`end` 动作只作用于工具线。host 线不在 main 的名册上，工具不给它发消息，也不结束它。

| 线 | 用工具开线 | 用工具发消息 | 用工具读 | 由谁结束（按约定） |
| --- | --- | --- | --- | --- |
| `main` | 能 | 它的工具线 | 它的工具线，未结束或已结束都可以 | 永不结束；Session 关闭不算结束 |
| 工具线 | 不能（工具结果拒绝） | `main`，即它的父分支 | — | 它自己（`end`）、用户，或祖先结束时连带 |
| host 线 | 不能 | —（按创建者的协议） | — | 它的创建者（Goal 结束它的评审者，[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）、用户，或祖先结束时连带 |

谁结束哪条线，是调用方之间的约定。运行时只检查目标：它存在、未结束、不是 `main`（§10）。

## 5. 部件

所有部件都属于 TimelineTree 插件（[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)），每个部件放在拥有其状态的作用域（[ADR 002](../adr/002-plugin-scope.md) D1）。

| 部件 | 作用域 | Capability | 在本设计中负责 |
| --- | --- | --- | --- |
| TimelineTree | Session | 否 | 线的生命周期：每个子分支开启输入里的身份块、`TimelineInfo.source`、一条线的结束（`branch/end` → `branch/ended`）、线进入结束后拒绝输入、恢复规则，以及读已结束线的历史 |
| TreeTimeline | 分支 | 是 | 把 `branch/end` 转给 TimelineTree，就像现在转 `branch/create`。对由工具调用发出的结束，它还转交关闭要等的那一轮结束 |
| BranchTool | Session | 否 | 按动作区分的 `branch` 工具（§11）。它每次调用时取调用方那条线的 BranchLine，与 WorkspaceTools 取 FileReads 的方式相同 |
| **BranchLine**（新） | 分支 | 是 | 一条线对自己相关各线的视图。它的角色（main、工具线或 host 线）和父分支来自 `TreeCommands.info(自己的 id)`。它从自己的事实折叠出未结束的工具线和当前这一轮的状态。它写名册、空闲通知和结束通知 |

BranchLine 注入 `B:Dispatcher`、`B:TimelineIdentity`、`B:BranchLifecycle` 和 `S:TreeCommands`，提供 `B:BranchLine`。

它的决定方式与 Goal 相同：依据已消费的状态，通过一个写入前会复核的自有写任务，并且只对加载截点之后的实时事件作决定。所以重载时什么都不会写两次，只有一个例外：如果加载时自己的历史里最后有一个仍在生效的名册键，而与折叠出的名册不一致，它就把名册写一次——因为补完一条已结束线的重启（§20.7）在 `main` 重放之前就记下了 `branch/ended`，之后没有实时事件跟随（§6.2）。

**线的来源。** TimelineTree 依据父分支的 `branch/create` 填写 `TimelineInfo.source`：

- 创建时，取自这次创建本身；
- 恢复时，取自恢复中的父分支对自己历史的读取。

这次读取已经存在。恢复中的父分支在构建时把历史读到它的头部，并报告它记录为失败的创建（[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 中 `Opening::Resume` 的 `failed_creates`）。现在它还报告每次创建的来源，以及每条 `branch/ended`，恢复规则会用到它们（§10）。存储的目录行不变（[存储格式](../../design/appserver/native-storage-format.md#427-v20-格式决定)）。

## 6. 线之间的消息

消息是接收方 Timeline 上的一条 `input/message`，经 `TreeCommands.input` 写入，不新增输入通道。它带一个原始块，由生产者标记、不带 key，以及 [InputQueue](../../architecture/rust-agent/plugins/InputQueue.md) 的模式：

| 模式 | 对接收方的作用 | 何时用 |
| --- | --- | --- |
| `append`（默认） | 随接收方下一次模型请求一起送达。它从不开启新的一轮，也不唤醒 `wait` | 接收方在下一步应该知道的进展或背景 |
| `steer` | 在接收方当前这一轮的下一步进入；接收方空闲时开启新的一轮。它会唤醒 `wait` | 接收方必须马上处理，或接收方处于空闲 |

不开放 Queue。一条线给另一条线的唯一 Queue 输入是开启输入。

这两个模式和用词与 Collaboration 的消息相同（`append`、`steer`），含义也相同，因为两者用的都是 InputQueue 的模式。

- **标记。** `<dim-branch-message from="C" to="P">`。接收方凭这个标记把消息和用户的话区分开。RPC 只接收操作者内容、不接收块，所以客户端无法伪造消息。
- **允许的两端。** `main` 和它每条未结束的工具线之间，双向都可以。其他任何组合都由工具结果拒绝，什么也不写：兄弟分支、host 线、已结束的线。
- **读取。** `read {branch, after?, limit?}` 让 `main` 读它的一条工具线，未结束或已结束都可以。它经 `TreeCommands.page` 从存储读取已提交的完整批次，按 Collaboration 的 `read` 渲染成员页面的方式呈现：输入、文本、工具调用与结果、每轮结束，带 `nextCursor`。线不读 `main`。切点之后一条线对 `main` 的了解，就是 `main` 发给它的内容，所以由 `main` 决定每条线需要知道什么，也不会有哪条线把另一条线的整个事件流拉进自己的上下文。
- **不设上限。** 线的数量、消息数、steer 数都不设上限。消息往复由提示词规则（§12）、某条线自行结束，或用户（中断或结束）来终止。为什么这样是安全的，以及需要时由谁负责限制，见 §6.1。
- **空闲通知。** 一条工具线的一轮结束时，这条线上的 BranchLine 给 `main` 写一条 steer（`<dim-branch-idle>`，§12.1），除非下列之一成立：
  - 这一轮已经给 `main` 发过 steer 消息；
  - 这一轮调用了 `end`；
  - 这条线还有待处理的驱动输入。

  这样，一条线一声不响地停下时，`main` 不会一直等下去；已经 steer 过 `main` 的线，也不会重复通知它。通知带这一轮的状态（`completed`、`failed` 或 `cancelled`）。
- **等待。** `main` 用现有的 `wait` 工具等它的线。steer 消息、空闲通知或结束通知都会唤醒它（[wait.rs](../../../crates/dim-agent-capabilities/src/wait.rs)）。

### 6.1 资源与限制

不设上限是安全的，因为线没有带来新的资源类别：

- 一条线的工作就是同一个 Session 的普通轮次，使用同一个 provider，受同样的 Permission 和 Approval 约束。
- 一条线同一时间最多运行一轮：Loop 每条线只持有一轮（[agent_loop.rs:106](../../../crates/dim-agent-capabilities/src/agent_loop.rs)）。
- 一条消息是由一次工具调用写入的一条有界的 `input/message`，大小受模型自己的输出约束。
- RPC 准入按连接设有上限（[架构](../../architecture/README.md)）。
- Collaboration 的成员也没有数量上限。

如果以后需要限制，每一项都已有归属，而且都不需要改机制：

| 限制 | 归属 |
| --- | --- |
| 开着的线数 | BranchTool 依据 main 的名册（由 BranchLine 折叠）拒绝 `create` |
| steer 频率 | 发送方的 BranchLine 统计它自己 Timeline 上的 steer 消息 |
| 整个 Session 的模型成本或并发 | 归模型调用层。这不是分支特有的限制：它同样适用于成员和 fork |

在那之前，保护措施是：

- 提示词规则；
- 已结束的线拒收消息；
- 用户的结束和中断；
- §12.3 中的刷屏、过度拆分和来回空转场景，在本地测量并在实现 PR 里报告，从不作为 CI 阈值。

开着的线确实有代价：

- 开着的线保留它已加载的部件。
- 重启时每条开着的线都会重新加载。Session 关闭和空闲卸载都不会结束一条线。
- 开着的工具线会让 main 的名册变长，名册只列开着的工具线。每次从线外结束一条工具线，`main` 上就多一条通知。

永不结束的工具线由它自己的协议（`end`）和用户负责，main 的名册让它始终可见。Pursuit 的评审者由 Goal 结束（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）。

### 6.2 已知代价

- **重启可能丢掉一条空闲通知。** 如果进程停在一条工具线一轮结束之后、它的空闲通知写出之前，这条通知就永远不会写出，因为 BranchLine 只对加载之后的实时事件作决定。Pursuit §4.2 也留着同样的窗口。`main` 的名册上仍有这条线，可以 `read` 它，用户也能看到它处于空闲。
- **重启可能丢掉结束通知。** `branch/ended` 与名册输入之间同样有窗口，一条线被外部结束时本该写出的结束通知会丢掉。加载时的补写只写名册，所以 `main` 是从名册的退场得知，而不是从通知。
- **压缩后的名册很短。** `main` 压缩之后，名册只保留每条线的 id 和目的的第一行。`main` 告诉过一条线什么，只留在压缩摘要里，或只能经 `read` 看到；而 `read` 会把那条线的原始事件一页一页带进 main 的上下文。

## 7. 身份与上下文

### 7.1 一个 key：`branch:line`

每条线都用同一个 key `branch:line` 下的块，回答"我是哪条线，哪些线是我的"。这个 key 在每条线上都一样。

- **子分支上：它的身份。** TimelineTree 在每个子分支上写入，无论工具线还是 host 线。它是子分支开启 Queue 输入的第一个块，key 为 `branch:line`，写明：
  - 子分支的 id、父分支和切点；
  - 它是同一个 agent，上面的历史就是它自己的；
  - 从这里起它并行工作；
  - 上面那些发给父分支的消息（例如名册）属于父分支。

  工具线的身份块还带上它的目的（`create` 的 prompt）和线协议。在 host 线上，创建者的 prompt 仍是开启输入的 `user` 内容，排在块之后。
- **`main` 上：名册。** `main` 上的 BranchLine 依据 `main` 自己的事实写一条带 key 的 Append：
  - 来源为工具的 `branch/create`；
  - `branch/create-failed`；
  - `branch/ended`。

  名册列出每条未结束的工具线，带它的 id 和目的的第一行。没有未结束的工具线时，这个 key 退役。
- **覆盖。** 子分支在继承的前缀里也带着父分支的名册。它自己同一 key 下的身份块覆盖了那份名册。从子分支的开启输入起，子分支上最新的有效 `branch:line` 块就是它的身份。

key 与模式互不相关（[history.rs](../../../crates/dim-agent-capabilities/src/history.rs) 的 `render_input`）。Goal 的 `set` 已经在 Queue 输入里放带 key 的块（[Goal §5](plugins/Goal.md#5-上下文映射)）。

身份放在开启输入里，而不是在 `branch/start` 时写一条 Append，原因有两个：

- Append 可能在开启输入之后才提交（[Goal §3.4](plugins/Goal.md#34-新分支清除继承的目标)）。
- 在 `branch/start` 时还不知道目的。

### 7.2 压缩

压缩按线进行。一次压缩是压缩方自己 Timeline 上的一条 `context/compaction` 事实。

- 子分支继承祖先在切点之前的压缩记录。事件 id 在继承的前缀里保持不变，所以 `retain_from` 在子分支上的重放方式不变（[ContextCompaction](plugins/ContextCompaction.md)）。切点之后，子分支各自压缩。
- 子分支的压缩不改变父分支，父分支的压缩也不改变子分支。

带 key 的基线会重放 `retain_from` 之前每个 key 最新的有效块。因此：

- `main` 压缩时，它的名册会重放；
- 子分支压缩时，它的身份会重放。

只用一个 key，每条线的基线里对"我是哪条线"恰好只有一个答案，继承来的答案也不会比这条线自己的答案活得更久。消息不带 key：压缩之后，它们像其他输入一样留在摘要或保留的轮次里。

### 7.3 缓存前缀

各线只在尾部不同：

- 根线确定 system 和模型，子分支不改它们（[prompt](../../architecture/rust-agent/prompt.md)）。
- 提示词来源按 main 的作用域加载，由子分支继承（[PromptSources](../../architecture/rust-agent/plugins/PromptSources.md)）。
- 每个 Session 只有一个 ToolRegistry，所以每条线的工具列表都一样。这就是子分支上的 `create` 由工具结果拒绝、而不是拿掉工具的原因。
- 身份块是子分支自己的第一个事件，紧跟在继承的前缀之后。所以子分支的第一次请求能复用父分支到切点为止的前缀。
- 名册更新是追加在 `main` 历史末尾的 Append。

### 7.4 Goal 与 Pursuit

**规则。** 一条线的开启输入由 TimelineTree 写入。它的第一个块是 `branch:line` 标记，所以按 [Goal A5](plugins/Goal.md#附录-e-假设)（操作者输入是"带 `user` 内容、且第一个原始块不是来源标记"），它是有来源的输入，不是操作者输入，不管创建者给了什么 `user` 内容。Goal 的冲突表记录了这一点（[Goal 附录 B](plugins/Goal.md#附录-b-完整冲突表)）。Goal 自己的规则不变。它的代码只随 [Pursuit 修订](plugins/Pursuit.md#13-结束评审者随-427)改变。

在本设计之前，host 线的开启输入（创建者的 prompt 作为 `user`，没有块）算操作者输入。工具线的不算：它带 `dim-branch-task` 块，没有 `user`。现在两者都是有来源的输入。

为什么这不改变 Goal 的任何行为：

| Goal 的决定 | 身份块的影响 |
| --- | --- |
| 基于块的决定：已决定集合、续跑列表、`conclusionOf`，以及带 key 的目标和状态折叠 | 没有影响。`marker()` 只匹配 `^<dim-goal `，折叠还要求 `goal:*` key，所以这些决定都看不到身份块 |
| 操作者判断 | 它唯一改变的字段：开启输入不再设置 `lastOperator` |
| `count` | `lastOperator` 只在这里使用：`since = max(active, lastOperator)` |
| 计数窗口 | 折叠只看本线的事件，并在这条线自己的开启处从头开始。子分支开始时没有目标，Goal 会写入开始时的 `clear`（[Goal §3.4](plugins/Goal.md#34-新分支清除继承的目标)）。`active` 只在之后的本地 `set` 或 `resume` 时移动，而这两者总在开启输入之后。所以开启输入从不改变 `since`，计数保持不变 |
| 评审者识别 | [Pursuit 修订](plugins/Pursuit.md#13-结束评审者随-427)之后，Goal 从 `TimelineInfo.source` 和 `parent` 识别评审者，不再读第一条 Queue 输入的 `user` 文本。身份放在 `blocks` 里，`render_input` 先渲染原始块，再渲染操作者的话（[history.rs:181](../../../crates/dim-agent-capabilities/src/history.rs)），所以模型先看到身份，再看到 prompt。上面的 A5 规则照原文不变 |

## 8. 工作区

各线共用 Session 的工作区。没有哪条线能知道文件是谁改的，本设计也不去归属改动。现有的保护已经够用：

- FileReads 按线保留读取绑定。
- 文件在这条线上次读取之后改变时，`edit` 和 `write` 以 `file_modified_since_read` 拒绝，不管是谁改的（[edit.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/files/edit.rs)、[write.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/files/write.rs)）。

其余靠协议，写在工具线的身份块和工具描述里：

- `main` 在 `create` 的 prompt 里给每条线分配它自己的文件或区域。
- 一条线要改它区域之外的文件之前，先告诉 `main`。
- 遇到 `file_modified_since_read` 时，这条线重新读取文件，告诉 `main` 之后再继续。

WorkspaceTools 不变。

## 9. 用户

- 交互面属于 Session。每条线都以这个 Session 的身份与用户打交道。
- **提问。** 任何一条线都可以用 `ask_user` 提问。答案作为这次调用的工具结果，回到提问的那条线（[Questions](plugins/Questions.md)）。
- **审批。** 一条线工具调用的审批直接交给用户。审批通知带 `branchId`，客户端会标出是哪条线。
- **不经 `main` 转达。** 这是同一个主体在问它自己的用户。
- **提交。** 用户可以向任一条未结束的线提交（带 `branchId` 的 `session.submit`，不变）。
- **中断。** 中断一条线只结束它当前这一轮，不结束这条线。
- **结束。** 除 `main` 外，用户可以结束任一条线（`session.branch_end`，§10）。

## 10. 结束一条线

| 事件 | Timeline | 数据 | 角色 |
| --- | --- | --- | --- |
| `branch/end` | 要结束的那条线 | `{source, ancestor?}` | 指令 |
| `branch/ended` | 它的父分支 | `{branch, endEvent, source, ancestor?}` | 结果，由 TimelineTree 写入 |

这一对事件与 `branch/create`、`branch/create-failed` 对称：一条 Timeline 上的指令由 TreeTimeline 转交，结果由树写出。`endEvent` 是 `branch/end` 在要结束的那条线上的序号。

`source` 是现有的 `BranchSource`，与 `branch/create` 用的是同一个类型，所以创建和结束以同样的方式记录发起方：

| 来源 | 发起方 |
| --- | --- |
| `{kind: "tool", turn_id, request_id, call_id}` | 要结束的那条线自己的一次工具调用：`branch` 工具的 `end` |
| `{kind: "host", operation_id}` | 一次宿主操作。可以是 App Server 的 `session.branch_end`，它像 `session.branch_create` 那样生成 `operation_id`（一个 uuid，[connection/tree.rs](../../../crates/dim-app-server/src/connection/tree.rs)），也可以是带自己 id 的插件，例如 Goal 的 `goal:evaluate:N` |

`ancestor` 指向用自己那次结束发起连锁的线：也就是本身不带 `ancestor` 的那次结束。连锁结束的每条后代都带这条线，`source` 也从同一次结束复制，于是（source, ancestor）这对值指明一次结束操作：谁要求的，在哪条线上。自己要求结束的线有 `source`、没有 `ancestor`；已被连锁结束之前就提交了自己结束的后代，保留它自己的那对值。`branch/ended` 从这条线的 `branch/end` 复制 `source` 和 `ancestor`，并且总是写在结束的那条线自己的父分支上，取自目录。

**一个通用操作。** `TreeCommands.end(id, source)` 和 `Session::end_branch(id, source)` 只检查目标：它存在、未结束、不是 `main`。结束 `main` 就等于关闭 Session，所以这是关于目标的检查，不是关于调用方的。它们从不检查是谁在调用。

- 结束一条已经结束的线，什么也不写，回复 `branch_ended`。
- 还在打开的线还不是线：它的创建仍可能失败，而一条线只能有一个终结事实，所以在它发布之前 `end` 回答 `unknown_branch`。在加载时结束一条线的开线方等 Session，而 Session 在恢复完成后才发布（§17 R1）。
- 关闭 Session 不算结束。
- "开线的一方管理它的线"是调用方之间的约定，不是运行时规则：
  - `branch` 工具的 `end` 不带目标，所以结束的是调用它的那条线，这是工具的设计；
  - 父分支想让子分支结束，就用 steer 请它结束，这是工具的约定。

顺序：

1. 入口在这条线上提交 `branch/end`。此后这条线不再接受新输入，也不再开出新线，它下面的线也不再开出新线：这些入口都以 `branch_ended` 失败。它下面的线仍可接受输入，直到它自己提交 `branch/end`（第 3 步）。
2. 这条线上的 TreeTimeline 消费 `branch/end`，转给 TimelineTree。
3. TimelineTree 先结束这条线的后代，走同样的步骤；无论多深，每条后代都复制这个 `source`，并把 `ancestor` 设为这条线。
4. 关闭时机：
   - `tool` 来源：在 `branch/end` 之后这条线第一次结束一轮时关闭，因为这次调用就在那一轮里；线空闲时立即关闭；
   - `host` 来源，以及因祖先结束而结束的线：立即关闭，中断当前这一轮。
5. `Branch::close()` 中断这条线、结算它的工作并卸载它（[branch.rs](../../../crates/dim-agent-runtime/src/branch.rs)）。
6. TimelineTree 最后在父分支上写 `branch/ended`。

在父分支上，BranchLine 只处理工具线；host 线归它们的创建者：

- 工具线经工具结束自己时（`tool` 来源且没有 `ancestor`），只写名册更新。这条线的最终消息已经在 `branch/end` 之前以 steer 送到 `main`。
- 工具线的其他任何结束，写名册更新加上结束通知（§12.1），以 steer 写在同一条输入里。

`tool` 来源且没有 `ancestor` 时，客户端显示"自行结束"；其他情况显示结束它的操作；由连锁结束的线还会写明它的祖先，例如"ended with X"。

**恢复。** 父分支上有某个子分支的 `branch/ended` 时，这个子分支不活跃。恢复中的父分支读取历史时会报告这些事件（§5）。这条规则加入现有的几条：父分支不活跃、父分支上有这个子分支的 `branch/create-failed`、子分支的创建从未完成。

一条线有 `branch/end`、但父分支上没有对应的 `branch/ended` 时，不打开它。恢复按最深的线优先补完这类结束：在父分支上补写每条缺失的 `branch/ended`，`source` 与 `ancestor` 取自 `branch/end`，这条线自己的结果最后写。对于它下面的线，那个父分支同样不会打开，所以记录写进它的存储历史，它的固定头部随之延长。其下自己从未开始结束的线不补写 `branch/ended`：父分支已关闭、不会再重放，写上去也没有人读。恢复把每条这样的线按它停下的头部记为已结束，`source` 取自父分支的 `branch/create`，它的历史仍可读取（§1"结束之后"）。

**结束之后：**

- 这条线留在分支目录里，它的历史仍可从存储读取：经 `session.history`（v3），也经工具的 `read`。它的头部不再移动。
- fork 照旧复制目录行和 main 的事件。在 fork 点之前结束的线，在 fork 里也不活跃，因为它的 `branch/ended` 在复制的前缀里。
- 删除 Session 时，这条线随之删除。
- Session 关闭和空闲卸载都不算结束：各线恢复后仍未结束。

**插件开的线。** 每个开线的一方按自己的协议结束自己的线。Goal 在评审了结之后结束它的 Pursuit 评审者；结论之前的外部结束，它判为 `failed`（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）。

## 11. `branch` 工具

工具接受一个按动作区分的输入，取代 `{prompt, at?}`。风险仍是 `session`，Approval 照旧放行每次调用。

| 动作 | 输入 | 谁 | 作用 | 结果文本 |
| --- | --- | --- | --- | --- |
| `create` | `prompt`、`at?` | `main` | 开一条切在 `at` 的工具线（默认与现在相同：在本次请求的模型开始之前） | `Line created: C` |
| `message` | `to`、`content`、`mode?`（`append` \| `steer`） | `main` → 它未结束的工具线；工具线 → 它的父分支 | 在 `to` 上写一条 `input/message` | `Sent to C.` |
| `read` | `branch`、`after?`、`limit?`（1–200） | `main`，读它的工具线 | 从存储读一页 | 这一页的 JSON：items、`nextCursor`、`done` |
| `end` | `content` | 工具线，对它自己 | 先给父分支发最终消息（steer），再调用 `TreeCommands.end(C, source)`，`source` 就是这次调用 | `Line C is ending; your final message was sent to P.` |

输入 schema 为 `{action, prompt?, at?, to?, content?, mode?, branch?, after?, limit?}`，`action` 必填，`additionalProperties: false`。

拒绝以错误结果返回（代码在 `structuredContent`），什么也不写：

| 情形 | 文本 | 代码 |
| --- | --- | --- |
| 不在 `main` 上 `create` | `Only main opens lines. Tell main if more parallel work is needed.` | `branch_not_main` |
| 在亲缘之外 `message` 或 `read` | `You can message only the lines you opened and the line that opened you.` | `branch_not_kin` |
| 对 host 线 `message` 或 `read` | `Line C was not opened with this tool; it follows its own protocol.` | `branch_not_tool_line` |
| 向已结束的线发消息，或与结束发生竞争 | `Line C has ended. Read it to see what it did.` | `branch_ended` |
| 在 `main` 或 host 线上 `end` | `Only a line opened with create can end itself.` | `branch_cannot_end` |

这些拒绝是工具自己的规则。`TreeCommands.end` 本身只检查目标（§10）。如果线已经提交了自己的 `branch/end`，再来的 `end` 取 `branch_ended` 一行，不会再写一条最终消息：第一次调用的消息就是这条线最后的话。

## 12. 提示词

### 12.1 上下文映射

`C` 是这条线，`P` 是它的父分支（工具线的父分支是 `main`）。标记内的内容按 TimelineTree 现在的方式转义（`context_block`）。

| 写入 | 位置、key | 模式 | 文本 |
| --- | --- | --- | --- |
| 身份，工具线 | `C`，开启输入的第一个块，`branch:line` | Queue | `<dim-branch-line branch="C" parent="P" source="tool">` / `You are line C of this session. Line P opened you at this point of its history to work in parallel with it. You are the same agent as P: everything above is your own history, and you continue from here on your own line. P keeps working on its line; you do not see its work after this point, and it sees yours only through what you send it. Messages above that were addressed to P, such as its list of open lines, are P's, not yours.` / 空行 / `Your purpose, as P wrote it:` / `<prompt>` / 空行 / `How you work with P:` / `- Work on this purpose only. You cannot open lines; if more parallel work is needed, tell P.` / `- Tell P with the branch tool (message, to "P"). Use mode "steer" for what P must act on now: a finding that changes its work or another line's, a blocking question, a conflict. Use "append" for progress P should see at its next step. Do not repeat what P already knows.` / `- You share the workspace with P and its other lines. Change only the files P gave you; before changing any other file, tell P which. If edit or write fails with file_modified_since_read, another line may have changed the file: read it again and tell P before you continue.` / `- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.` / `- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. That is your last action; P receives it.` / `</dim-branch-line>` |
| 身份，host 线 | `C`，开启输入的第一个块，`branch:line`。创建者的 prompt 是这条输入的 `user` | Queue | `<dim-branch-line branch="C" parent="P" source="host">` / `You are line C of this session, opened from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You do not see P's work after this point. Messages above that were addressed to P are P's, not yours. The message after this one says what this line is for.` / `</dim-branch-line>` |
| 名册 | `P`，`branch:line` | Append | `<dim-branch-lines branch="P">` / `Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it does not see your work after that point unless you message it.` / `- C1: <目的的第一行，最多 120 个字符>` / … / `They message you when they find something, need you, or end. Use the branch tool to message or read them, and wait to wait for them.` / `</dim-branch-lines>` |
| 名册，没有未结束的线 | `P`，`branch:line` 退役 | Append | `<dim-branch-lines branch="P">` / `All lines you opened have ended.` / `</dim-branch-lines>` |
| 消息 | 接收方，无 key | `append` 或 `steer` | `<dim-branch-message from="C" to="P">` / `<content>` / `</dim-branch-message>` |
| 最终消息 | `P`，无 key | Steer | `<dim-branch-message from="C" to="P" final="true">` / `<content>` / 空行 / `Line C ends with this message.` / `</dim-branch-message>` |
| 空闲通知 | `P`，无 key | Steer | `<dim-branch-idle branch="C" status="completed\|failed\|cancelled">` / `Line C's turn ended (<status>) without a message to you, and C is waiting. Read it if you need its state; message it with mode "steer" to give it more work, or leave it.` / `</dim-branch-idle>` |
| 结束通知 | `P`，无 key，与名册在同一条输入里；工具线除自己的 `end` 之外的任何结束 | Steer | `<dim-branch-ended branch="C">` / `Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did.` / `</dim-branch-ended>` |
| 工具描述 | 每条线 | — | `Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed while you work.` / `- create (main only): open a line to work on prompt in parallel. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Open a line only for work that can go on without you. Returns the line's id.` / `- message: send content to a line you opened, or to the line that opened you. Mode "append" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use "steer" when the receiver must act now or is idle.` / `- read: show the recent activity of a line you opened, also after it ended. Pass nextCursor as after to continue.` / `- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.` / `Lines talk only with the line that opened them and the lines they opened.` |

这些文本取代代码里现有的委派措辞：`first_input` 里的 `dim-branch-task` 块（"That branch delegated this task to you"）和工具描述（"works on prompt as a task you delegate"）。

### 12.2 交给提示词层的事项（[#429](https://github.com/arcships/dim/issues/429)）

上面的文本是初版，归 TimelineTree 插件所有。下面这些事项属于面向模型的提示词层：

- `main` 什么时候该开线：工作可以并行、可以拆开，而且不需要 `main` 也能进行。什么时候不该开：只是很短的一步，或者需要 `main` 的下一个决定。
- `main` 怎样给一条线写说明，以及协调对 `main` 意味着什么：分配区域、回应 steer、结束自己的等待、不重做线上的工作。
- **Goal 的措辞（归 Goal）。** 下面这些文本把子分支当作被委派的工作，与 §1 矛盾：
  - 目标文本 "If you are working on a delegated sub-task started from branch X, treat this goal as background and do not call goal_update."（`runtime/plugins/goal/goal.ts:98`）；
  - 续跑文本（`goal.ts:131`）；
  - [Goal §3.4](plugins/Goal.md#34-新分支清除继承的目标) 和 Goal §5 子分支那一行里的 "delegated work"。

  替换为 "on another line of this Session"。本设计不改这些文本，它们归 Goal。

### 12.3 评估场景

这些场景在本地用真实模型运行，结果在实现 PR 里报告。CI 只断言行为。

| 场景 | 关注什么 |
| --- | --- |
| 线把自己当成 main | 它开线、把名册当成自己的，或去协调其他线 |
| 重复劳动 | `main` 重做一条线正在做的事，或者两条线做同一件事 |
| 过时的认识 | 一条线依据切点之后已经改变的文件或决定行事，却没有重新读取 |
| 刷屏 | append 或 steer 消息重复接收方已经知道的内容 |
| 过度拆分 | 为很短或必须按顺序做的步骤开线 |
| 不结束 | 一条线已经做完，却不调用 `end` |
| 兄弟中转 | 用 `main` 在本该合成一条线的几条线之间传话 |
| 压缩后的身份 | `main` 和某条线各自压缩之后，仍知道自己是哪条线、哪些线是自己的 |
| 来回空转 | 不推进工作的 steer 往复 |
| 问用户 | 该问用户时，线直接问用户，而不是绕道 `main` |

## 13. 相互影响

| 功能 | 影响 |
| --- | --- |
| Goal | Goal 自己的规则不变。一条线的开启输入按 A5 是有来源的输入（§7.4），Goal 的冲突表记录了这一点。新线照旧清除继承的目标。Goal 的代码只随 Pursuit 修订改变：从 `info(自己的 id).source` 识别评审者、结束它的评审者，以及 `branch/ended` 这个 key（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） |
| Pursuit | 评审者得到 host 线的身份块。Goal 从线的来源识别它的评审者，并在评审了结之后结束它（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） |
| Fork、删除 | 不变。已结束的线按它的事实处理（§10） |
| Session 关闭、空闲卸载 | 不算结束。各线恢复后仍未结束 |
| 中断 | 一次轮次中断，或对某条线的停止，只结束一轮，从不结束一条线。对 `main` 的中断不影响它的线 |
| Collaboration | 不变。成员是 Session；成员的 Session 是否挂载 `branch`，由它档案的工具列表决定 |

## 14. 客户端

**TUI。** 线有自己的面板、视图和操作：

- `/lines`（别名 `/branches`）列出这个 Session 的每条线，各自缩进在打开它的那条线之下，每条带状态：工作中、等待中、自行结束、被某个操作结束、随 X 结束，或创建失败。
- Enter 显示所选线的对话。输入框向正在显示的线提交，输入框的下边框标出这条线，用 id 的前八个字符。
- `x` 再 `y` 结束所选线；`main` 上被拒绝。
- 提问和审批都带线的标签，两条线问同一件事也能分辨（提问显示 `(branch NAME)`，审批显示 `Permission on branch NAME`）。
- `main` 与一条线之间的消息，在发送方读作已发出（它的 `branch` 工具调用），在接收方读作已收到（`◂ from C`）。
- 切换显示的线就在一条边界行（`line C`）之下换一份记录，和切换 Session 一样，因为终端的回滚缓冲只追加。一条线的记录从它的事件折叠而来，显示与否都在折叠，所以打开一条线不读历史。

这个面板与任务面板分开：任务面板列后台命令、成员和 Workflow 运行，它的 `x` 是停止任务，而结束是永久的，两者不共用按键。

```
┌ session 3f2a · main ────────────────────────────────────────────┐
│ … main 的对话 …                                                 │
│ ◂ from b-7c (steer)  the leak is in cache.rs                    │
│ ◂ from b-02 (steer), final  docs pass done                      │
│                                                                 │
│ ╭─────────────────────────────────────────────────────────────╮ │
│ │ › ask anything…                                             │ │
│ ╰───────────── ~/repo/dim (main) ────────────────────────────╯ │
└─────────────────────────────────────────────────────────────────┘

┌ lines ──────────────────────────────────────────────────────────┐
│ ● main                          the session itself · shown      │
│   b-7c  parser race hypothesis          working                 │
│   b-91  review the cache change         waiting                 │
│     b-c3  the guard in cache.rs         working                 │
│   b-02  docs pass                       ended itself            │
└────────── Enter show the line · x end it · Esc close ───────────┘
```

**Web** 目前只显示 `main`，线以后再做。Desktop 得到的是协议：`session.branch_end`，以及 v3 历史里的两种事件。

## 15. 改动

| 模块 | 改动 | 原因 |
| --- | --- | --- |
| [设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义) | 改写：同一主体的多条线、共享事实、父子是唯一的通信关系、共用交互面。关系表一行改为"只在父子之间"；Session 一行加上"结束" | 现在的文本是"平等的线，父子只记录来源"，没有通信关系 |
| TimelineTree | 对每种来源都在 `first_input` 里写身份块。新增 `TreeCommands.end(id, source)` 和 `TreeCommands.page(id, after, limit)`。`TimelineInfo.source`，依据父分支的 `branch/create` 填写（§5）。结束流程、线进入结束后拒绝输入、`branch/ended`、恢复规则、读已结束的线 | 它拥有线的生命周期和开启输入 |
| TreeTimeline | 消费 `branch/end`，以及由工具调用发出的结束时这条线的下一次轮次结束，并转给 TimelineTree | 它现在就这样转 `branch/create` |
| BranchTool | 按动作区分的工具，加上新的 BranchLine 分支部件 | 工具的各个动作，以及一条线对自己相关各线的视图 |
| Session 门面 | `Session::end_branch(id, source)`。在固定的头部从存储读已结束线的历史（`local_page`、`sequence`） | §17 |
| App Server | `session.branch_end`（v3，host 来源，带生成的 `operation_id`；`session_execution` 打开、且连接的入口不是裸入口或守护进程 profile 未受保护时，`hello.features` 把它和其他会话方法一起列出）。对已结束的线，`session.history` 立即返回，截到它存储的头部。`ProductEvent::new` 在 v3 里带上两种事件，v1、v2 里不带 | 一个入口和一条读取路径，没有业务逻辑 |
| dim-agent-types | 在 `EventKind` 和 v1 排除列表里加入 `branch/end` 和 `branch/ended`，并在契约测试中登记。冻结样本不变 | 两种核心事实（§16） |
| Goal（Pursuit 修订） | 从线的来源识别它的评审者，并结束它（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） | 评审者的协议归 Goal |
| 分支 TypeScript 绑定 | 重新生成 `npm/branch-services.d.ts`：TreeCommands 新增 `end` 和 `page`，`TimelineInfo` 新增 `source`。[绑定表](../../design/branch-typescript.md)中 `treeCommands` 一行列出 `end` | TreeCommands 是导出的服务 |
| CLI TUI | §14 | 用户跟随线、与线对话、结束线 |
| 插件页面 | TimelineTree.md 和 BranchTool.md 随实现更新（`xtask check`） | 页面跟随代码 |

新增的公开内容：

- 两种事件；
- 一个 RPC 方法；
- 两个 TreeCommands 方法：`end(id, source)` 和 `page(id, after, limit)`；
- `TimelineInfo.source`，由 BranchLine 和 Goal 读取；
- `branch` 工具的新输入。

BranchLine 的服务只由同一插件里的工具读取。

## 16. 格式

`branch/end` 和 `branch/ended` 是 `branch` 领域的核心事件：

| 事件 | 数据 |
| --- | --- |
| `branch/end` | `{source, ancestor?}`。`source` 是 `BranchSource`（`{kind: "tool", turn_id, request_id, call_id}` 或 `{kind: "host", operation_id}`）；`ancestor` 是一条线的 id |
| `branch/ended` | `{branch, endEvent, source, ancestor?}`。`endEvent` 是 `branch/end` 在结束的那条线上的序号；`source` 与 `ancestor` 从它复制 |

它们的格式决定记录在存储合同里：[#427 v20 格式决定](../../design/appserver/native-storage-format.md#427-v20-格式决定)。

## 17. 向机制所有者提出的需求

**R1. 一条线的持久结束**（所有者：dim 运行时，即 TimelineTree 与 Session 门面；随本设计在 #427 下实现）。

- **被挡住的功能与规则。**
  - 一条线无法单独结束。`Branch::close()` 存在，但没有任何入口调用它，关闭也不留记录，所以恢复时这条线又被打开。
  - Session 门面只从已发布的线读历史，所以已结束线的历史读不到了。
  - 门面和事件格式属于基础设施（[设计规则 §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 第 8 条）。
- **考虑过的现有机制。**
  - `stop/interrupt` 结束的是一轮，不是一条线。
  - Session 关闭会结束所有线。
  - `branch/create-failed` 只在创建完成之前存在。
  - 插件自有的事件行不通，因为恢复归 TimelineTree，它不能读插件的事实（第 4、7 条）。
  - 由 App Server 直接读存储会绕过门面。
- **通用能力。** 不带业务含义：
  - 线上的指令 `branch/end {source, ancestor?}`，以及父分支上的结果 `branch/ended {branch, endEvent, source, ancestor?}`；
  - `TreeCommands.end(id, source)` 和 `Session::end_branch(id, source)`，它们只检查目标；
  - `TimelineInfo.source`，依据父分支的 `branch/create` 填写；
  - `branch/end` 提交后不再接受新输入；
  - 先结束后代；
  - 恢复时把已结束的线视为不活跃，并为没有 `branch/ended` 的 `branch/end` 补完结束；
  - 已结束线已提交的历史在固定的头部仍可读取，经 `Session::local_page` 和 `TreeCommands.page`。
- **对序号、批次、中断、重载的影响。**
  - 每个事件单独一个批次：线上的 `branch/end`，以及关闭之后父分支上的 `branch/ended`。
  - `host` 来源的结束，或因祖先而结束，经 `Branch::close()` 中断这条线。
  - 重载时适用上面的恢复规则。Session 在恢复完成后才发布，所以在加载时结束一条线的部件，例如 Goal 的核对（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)），要等 `SessionLifecycle::activated()`，此后它看到的每条线（目录里有的）都已发布。
  - Timeline 多一道线自己的准入检查：`Timeline::input` 变为 `input_checked(data, admit)`，在提交锁内运行 `admit`，于是 `branch/end` 之后提交的输入以 `branch_ended` 被拒绝，而不是与关闭竞争。Dispatcher 和批次都不变。

## 18. 安全

- 各线共用 Session 的权限策略和 Approval。一条线不是隔离手段（[ADR 003](../adr/003-approval-scope.md)）。
- 只有 TimelineTree 和 BranchLine 写这些标记。其他写块的生产者（经 `TreeCommands.input` 的插件）是受信任的进程内部件。RPC 不写块。
- `session.branch_end` 和 `session.branch_create` 一样，是受信任的用户入口。它不需要审批，因为它不是 agent 的动作。
- 任何受信任的进程内部件都能结束除 `main` 之外的任一条未结束的线，就像它能向任一条线写输入。工具的 `end` 与其他调用一样受 Approval 约束。
- 诊断只记录 id、序号和来源的种类，从不记录消息或目的的文本。

## 19. 规则检查

规则：[设计规则 §1、§2 与 §4](../../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md) D1–D5、[ADR 003](../adr/003-approval-scope.md)。

| 规则 | 本设计如何遵守 |
| --- | --- |
| §1 Capability 与 App Server 边界 | Capability：TreeTimeline（已有）和 BranchLine（新）。TimelineTree 和 BranchTool 的 Session 部件是普通部件。App Server 只增加一个入口和一条读取路径，没有业务代码 |
| §2 作用域 | 线的生命周期仍归 Session（TimelineTree）。一条线对自己相关各线的视图是分支状态（BranchLine）。线之间只靠写输入互相影响，现在只限父子之间（§2 已改写）。能力影响分支不受这条规则约束（§4） |
| §4.1 指令事件 | 结束是 `branch/end`，由 TimelineTree 决定并写 `branch/ended`。消息是输入，由接收方的 InputQueue 和 Loop 决定。结束一条已结束的线、或被拒绝的消息，什么也不写，回复说明原因 |
| §4.2 交接后不撤回 | 已提交的消息属于接收方。`branch/end` 不撤回 |
| §4.3 before 回调 | 无 |
| §4.4 核心不带业务含义 | Dispatcher、`before` 和批次不变；Timeline 的准入检查是通用的（§17 R1）。身份文本属于 TimelineTree 自己的领域，即一个 Session 的各条线。工具线的协议写在 `first_input` 现在写工具专用文本的地方，也就是拥有该工具的模块里 |
| §4.5 批次 | `branch/end` 单独一个批次。`branch/ended` 在关闭之后最后写入。名册和结束通知是同一条输入 |
| §4.6 上下文 | 身份、名册、消息和通知都是被采用的输入。`read` 返回工具结果。没有别的东西进入上下文 |
| §4.7 公开事件与服务 | BranchLine 折叠的是它自己那条线的公开事件。工具读 BranchLine（同一插件）和 TreeCommands。`read` 像 Collaboration 的 `read` 一样，把已提交的事件作为工具结果返回，没有哪个部件折叠另一条线的事实。Goal 按它自己公开的 A5 约定判定开启输入（§7.4）。BranchLine 和 Goal 经公开服务 TreeCommands 读取 `TimelineInfo.source` |
| §4.8 基础设施 | 门面和格式的改动即 R1（§17）。需求来自线的生命周期本身，在运行时内闭环 |
| §4.9 只有后续决定要用的事实才进 Timeline | `branch/end` 决定关闭和恢复。`branch/ended` 决定恢复时是否活跃，以及名册。没有只给 UI 用的事件 |
| ADR 002 D1 作用域 | 见 §5 |
| D2 不设接线模块 | BranchTool 的 feature 自己登记 BranchLine |
| D3 作用域依赖 | BranchLine 向外依赖 Session 的 TreeCommands。没有核心部件依赖它 |
| D4 配置 | 无 |
| D5 合同语言中立 | 事件、RPC 方法、标记和工具 schema 都是数据。TreeCommands 的新方法已导出，并为 TypeScript 生成类型 |
| ADR 003 审批 | 工具风险仍是 `session`，Approval 放行每次调用。用户结束一条线不是 agent 的动作 |

## 20. 验收

以下是使用假模型的流程，放在 `branch_flow.rs`、`flows.rs` 和 App Server 传输流程中。

1. `main` 开两条线，名册列出这两条。每个子分支的身份覆盖它继承的名册。`main` 和某个子分支各自压缩后，带 key 的基线重放名册或身份。
2. 子分支给 `main` 发一条 Append：它不唤醒 `main`，`main` 在下一次请求时看到。Steer 唤醒 `main` 的 `wait`，或在 `main` 空闲时开启新的一轮。来自 `main` 的 Steer 唤醒空闲的子分支。
3. 下列操作被拒绝，什么也不写：给兄弟分支发消息、在子分支上 `create`、对 host 线 `message` 或 `read`、工具线 `read` `main`、在 `main` 上 `end`。
4. 子分支的一轮没有发 steer 就结束时，`main` 收到一条带这一轮状态的空闲通知。这一轮 steer 过 `main`、调用了 `end`，或留下了待处理的驱动输入时，不写通知。
5. 工具线结束自己：写入最终消息（steer）和带 `tool` 来源的 `branch/end`，在这一轮结束时关闭，然后在 `main` 上写 `branch/ended` 并更新名册，不写结束通知。之后的消息被拒绝（`branch_ended`）。工具的 `read` 和 `session.history` 都能返回已结束线的历史。
6. 经 `session.branch_end` 由用户结束：`branch/end` 带 App Server id 的 `host` 来源，这条线当前这一轮被中断，`main` 以 steer 收到结束通知。`main` 不能被结束。再次结束返回 `branch_ended`。向已结束的线提交返回 `branch_ended`。
7. 恢复：已结束的线不打开，它的后代也不打开。没有 `branch/ended` 的 `branch/end` 在重启时补写结果。
8. Pursuit 的评审者得到 host 线的身份块，现有的 Pursuit 流程照常通过。
9. 两条线编辑同一个文件：后编辑的那条得到 `file_modified_since_read`（已有行为）。
10. v1、v2 客户端看不到新事件。v3 历史包含它们，`hello.features` 列出 `session.branch_end`。

Goal 流程，放在 `goal.test.mjs` 和 `transport_flow/goal.rs` 中（§7.4）：

11. 用 `session.branch_create` 开出的线，开启输入带身份块，表现与 `main` 完全相同：在这条线上 `set`、它的续跑，以及之后一次重置计数的操作者输入。
12. 在新的开启输入下创建的 Pursuit 评审者能被识别，它的结论会被应用。工作线的计数不变。
13. 身份块不改变目标和状态折叠、已决定集合或续跑列表中的任何内容。

线的结束与来源：

14. `TreeCommands.end` 结束除 `main` 外的任一条未结束的线，不管是谁调用。例如插件部件以 `host` 来源结束一条工具线，`main` 就收到结束通知。它拒绝 `main`、未知的线和已结束的线，什么也不写。
15. 结束一条有子分支的 host 线时，先结束子分支（带 `ancestor`，复制 `source`），再结束这条线。
16. `TreeCommands.list` 和 `info` 对新建和恢复的子分支返回 `source`，对 `main` 不返回。存储的目录行逐字节不变。

## 21. 消融审查

留下的部分，以及每一部分为什么必要：

| 部分 | 原因 |
| --- | --- |
| 每个子分支上的身份块，同一个 key | 没有它，一条线压缩后会忘记自己是哪条线，或把父分支的名册当成自己的 |
| `main` 上的名册 | `main` 压缩后仍要知道哪些线还开着 |
| 消息是 `input/message`，带 append 和 steer | 这是线之间唯一的通道。模式是 InputQueue 的 |
| `main` 的 `read` | `main` 不用开口问，就能检查一条线的工作 |
| 空闲通知 | 没有它，`main` 会一直等一条悄悄停下的线 |
| `branch/end` 与 `branch/ended` | 没有它们，一条线无法持久结束，恢复时又会被打开 |
| 结束上的 `source`（`BranchSource`） | 创建和结束以同样的方式记录发起方，插件的结束不会被记成这条线自己的结束 |
| `TimelineInfo.source` | 开线的一方不用解析 prompt 文本就能找到自己的线 |
| 工具限于 `main` 和工具线 | 一个协调者，创建者也保有自己的协议 |
| 关于文件的提示词规则 | 唯一的保护是 FileReads，其余靠协议 |

删掉的：

- 对线数、消息数、steer 数或往复轮数的上限；
- 文件改动的归属；
- 对 WorkspaceTools 的改动；
- 兄弟分支之间的通道；
- 按线的配置（system、模型、工具）；
- 结果约定与结果回交；
- 线读 `main`；
- 为消息单独设的事件或概念（它们就是 InputQueue 的模式）；
- 父分支经工具结束子分支；
- 取值固定的 `by` 字段（现有的 `BranchSource` 记录发起方）；
- host 线上的分类字段（`operation_id` 已经是开线一方的标签）；
- 运行时检查谁在结束一条线。

## 22. 以后的选项

**在 Session 的限度内按线收窄。** 在 Session 的限度内收窄仍是同一个主体，所以与设计规则 §2 相容。它不属于本设计。做的时候：

- **更少的工具：** 在调用时拒绝，就像现在子分支上的 `create` 被拒绝那样。工具定义和缓存前缀保持一致。
- **更低的权限：** 每条线一份策略，与 Session 的策略取交集。它需要 [Pursuit §10](plugins/Pursuit.md#10-以后的选项) 已经提到的按分支收窄 Permission。
- **更便宜的模型：** 在创建时、第一条输入之前设定的按线配置。不同的模型与父分支不共享缓存，所以它是用继承的前缀换成本。
