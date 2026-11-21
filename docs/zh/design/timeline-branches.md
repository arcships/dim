# Timeline 分支

状态：已采纳（[#427](https://github.com/arcships/dim/issues/427)）。本文同时是需求与设计：§1–§3 回答 #427 的问题，§4 起是设计。范围：TimelineTree 插件（TimelineTree、TreeTimeline、BranchTool，BranchTool 新增一个 BranchLine 分支部件）、一个 App Server 内置方法、一种核心事件、一个 `BranchSource` 变体、CLI TUI、Web 工作区，以及[设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义) 的改写。语言：Rust。English: [English](../../design/timeline-branches.md)。

本次交付两件事：Session 的**默认分支**（英文 default branch，界面上仍叫“主线”），也就是它的对话所在的那条线，以及由用户把另一条未结束的线选为这条线；以及**谁能开线**，现在是根永远可以，加上当前默认分支。其余每条线都被拒绝，所以任何时刻最多两条线能开线，而根保留它今天的权利。

它不做的一个决定是什么时候该开线的说明：四个条件以及它们与相邻机制的边界写在 §12.4，作为策略，由第一次把它们放进去的改动用 §12.3 的场景判定。开着的工具线的 Session 级上限与开线权放在一起（§6.1）。

## 1. 分支用来做什么

**分支**是同一个 agent 在一个 Session 里的一条行为线。Session 是一个主体，也就是"我"：它的身份、它与用户的交互面、它的能力和它的工作区。分支是这个主体的一条行为线。

子分支从父分支的历史中某个切点开始。切点之前的每个事件也是子分支自己的历史，这些行不会改变。切点之后，子分支读自己的事件，并通过 `branch` 工具读它被切出来的那条线（§6）。

分支之间共享事实，不共享上下文：

- 每条线用自己的事实组装自己的上下文。
- 每条线各自压缩。
- 一条线影响另一条线，向那条线的 Timeline 写输入；它了解另一条线，靠自己对那条线的读取。
- 祖先追加了投递输入、但该输入还没有在继承前缀里被消费的规则或状态变更，不属于这条线继承的状态：本线按来源的当前值自行采用，那个块是本线切点之后自己的事件（[prompt](../../architecture/rust-agent/prompt.md)）。

父子关系记录历史的来源，也是分支之间唯一可以通信和读取的关系。它不表示控制，也不表示欠对方一个结果。

对 #427 的回答：

| # | 问题 | 回答 |
| --- | --- | --- |
| 1 | 支持哪些场景，按什么顺序 | 三个，依次是（§3）：插件开的线（已交付）、agent 从根开的线（已交付）、用户把某条未结束的线选为 Session 的默认分支（本设计） |
| 2 | 分支、Collaboration 成员、fork 怎么区分 | 见 §2。分支是同一个 agent；成员和 fork 是另一个 Session |
| 3 | 分支结束时，谁需要它的结果，以什么形式 | 没有哪条线欠另一条线结果。各线在工作过程中把要紧的事告诉对方（§6）。agent 开的线结束时给开它的那条线一条最终消息（§10）。插件开的线按创建者的协议办，例如 Pursuit 的结论 |
| 4 | 分支什么时候算结束，结束后还能看到吗 | 它自己结束（只有工具线这样结束）、用户结束它，或者祖先的结束连锁到它（§10）。结束的线留在分支目录里，客户端和开它的那条线仍能读到它的历史 |
| 5 | 用户能看到什么、做什么 | 跟随任一条线、与它对话、回答它的提问和审批、把其中一条选为默认分支、结束它（§9、§14）。比较和采纳随场景 3 一起做 |
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
| S1 | **插件开的线。** 插件按自己的设计开线。例如 [Pursuit](plugins/Pursuit.md) 的评审者：它是插件来源的子分支，切在执行者那一轮结束处，结论以输入的形式写回（已实现，[#422](https://github.com/arcships/dim/issues/422)；`crates/dim-goal`） | 已交付。本设计给它加上身份块（§7.1）。Goal 对评审者的识别和结束随之改变（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） |
| S2 | **agent 从根开线。** 例如：根继续读代码时，在两条线上分别验证一个 bug 的两种假设；根实现的同时，让一条线随改动逐个评审；根继续工作时，让一条线调研某个 API 或代码的某个区域；在另外的文件上尝试第二种做法 | 已交付。本设计既不改变谁能开线，也不加什么时候该开线的说明 |
| S3 | **用户把某条未结束的线选为默认分支，在那里继续。** `session.branch_create` 已经能在任意父分支下开线。有了本设计，用户可以把任意未结束的工具线、或用户自己开的线，选为 Session 对话所在的那条线，并用同一个动作移回根。那条线仍向开它的那条线汇报，协调关系不随默认分支移动。比较和采纳需要单独设计：采纳一条线，对共享的工作区意味着什么 | 本设计 |

下面这些需求不是分支：

| 需求 | 为什么不是分支 | 归属 |
| --- | --- | --- |
| 委派给有自己指令的专门角色 | 那是另一个身份 | Collaboration 成员 |
| 判断是否批准一次工具调用 | 那是对一个动作的决定，不是一条工作线 | 带宿主端口的能力（[ADR 003](../adr/003-approval-scope.md)） |
| 把某条线回退到更早的某点 | 它改变的是这条线如何看自己的历史，不是一条新线 | 单独的需求。目前的答案是 `session.fork` |
| 隔离地并行编辑 | 各线共用 Session 的工作区，没有按线隔离的工作区 | fork，或有自己工作目录的成员 |

## 4. 树与角色

- **深度。** 架构上分支可以有任意深度。`session.branch_create` 接受任意父分支；Goal 接受任意 `branchId`，所以开在子分支下的评审者是孙分支。
- **亲缘。** 父子是分支之间唯一可以通信的关系。父分支和它的每个子分支互相写输入，一条线还会读它被切出来的那条线（§6）。兄弟分支、更远的亲属之间既不发消息也不读。这条规则约束的是一条线作用于另一条线，在这里就是 `branch` 工具。能力按自己的决定行事，属于能力影响分支，不受这条规则限制（[设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义)）。例如 Goal 写入评审结果，或插件在任意切点开线。
- **根。** `main` 是没有父分支、也没有来源的那条线：这个字面量 id、它的编号，以及所有已存下的 `branch_id: "main"`，本设计都不改。它永不结束，Session 关闭不算结束。在任何状态下它都能用工具开线，它是工具接受的两类调用方之一（§4、§11）。界面上它叫“起点”。
- **默认分支。** 这个 Session 的对话所在的那条线：请求不传 `branchId` 时采用的目标，也是客户端居中显示的那条线。它是树上的一个位置，不是 id，也不是一种树关系：开局就是根，只在用户动作下移动（§9、§14）。唯一权威的事实不是模型可见的块，而是写在根线上的一条 `branch/default {branch}`（根线比其它线都活得久，且不能结束）（§16）。告诉各条线这件事的键控块从来不是权威。没有这条事实的 Session 就在根上。
- **根和开线方来自树。** 一条线没有父分支，它就是根。一条线的开线方，就是它自己 `branch/create` 里记的父分支。“谁开了这条线”“谁收到它的汇报”“谁协调它”都是这一条边，从树里读出来：本设计不新增角色标记、不新增加协调者字段、也不新增存储关系。
- **协调。** 一条线协调它用工具开出的线：它带名册（§7.1），名册只列这些线，也只有它能给它们发消息、读它们。用户开出的线不在任何名册上，所以开出它的那条线既不给它发消息、也不读它（§6）；它协调自己开出的工具线，依据是它自己的名册。开线方不结束它们：`end` 不接受目标，结束的是调用方自己，所以开线方要某条线结束时，用 steer 请它自己结束，或者由用户结束它（§10、§11）；线的来源另有限制：插件线按创建者的协议走，用户开的线由用户结束。协调关系沿父子关系走，从不随默认分支移动。失去开线权的线保留它已经开出的线，并继续协调它们。
- **agent 的工具。** `branch` 工具只接受两类调用方，其余一律拒绝（§11）：
  - **根**，永远可以，不论它是不是默认分支。这个权利从不改变，所以它永远收不到关于角色的状态说明（§12.1）；
  - **当前默认分支**，在它持有这个位置期间可以。

  两者都不是的线以 `branch_not_opener` 被拒绝，拒绝文本说出这个 Session 里哪两条线能开线。插件线永远不开线。曾经的默认分支保留它开出的工具线并继续协调它们，只是不能再开新线。任何时刻最多两条线能开线；默认分支就是根的 Session 只有一条。

  这正是根仍能响应用户自己的请求的原因：当某条线是默认分支时，用户仍可以让根开一条线，那条线从根的历史开始，并向根汇报。只让默认分支开线的守卫会拒绝这个请求，而把同样的请求发给默认分支，开出的线从默认分支的历史开始，那里没有根在切点之后做的事。
- **来源。** 每条线都有来源，记在父分支的 `branch/create` 里。来源记录这条线从哪里开出来，它不是角色标记：
  - **工具线**由 `branch` 工具开出。它是对话线：可以成为默认分支，不在默认分支时用 `end` 结束自己。
  - **用户线**由客户端经 `session.branch_create` 开出。它的开线方是用户，它同样是对话线：可以成为默认分支。它由用户结束，不能结束自己。
  - **插件线**由插件（`TreeCommands.create`）开出。它的协议归创建者，永远不能成为默认分支。它也不开线。

  来源可以从 `TimelineInfo.source` 读到，由 `TreeCommands.list` 和 `info` 返回，根上没有（§5）。已结束的目录条目可以不带它；打开显示详情时按已知位置读取创建来源。插件线的来源是 `{kind: "host", operation_id}`：开线一方的不透明标签，核心从不解释它。用户线有它自己的种类 `{kind: "user", operation_id}`，所以需要分辨对话线与插件线的读者不用去解析 `operation_id`。开线一方按自己的 operation id 约定找到自己的线，例如 Goal 的 `goal:evaluate:` 前缀。种类只说明这条线从哪里开出来，不携带角色。
- **升级前已经存在的线。** 今天 `session.branch_create` 写的是 `{kind: "host", operation_id}`（[connection/tree.rs](../../crates/dim-app-server/src/connection/tree.rs)），本设计不改写任何已存的行。用户在升级前经客户端开出的线，包括 `FORMAT` 52 下用 Web `/branch` 开的线，升级后仍是 `host`，因此不再是合法的切换目标；升级后新开的同样一条线则可以。本次接受这个限制，而不迁移这条事实：`FORMAT` 53 只是升标记、不写数据（§15、§16），并且有一条验收用例写明它（§20）。
- **工具的作用范围。** 一条线读、也发消息给它被切出来的那条线和它开过的每条未结束的工具线。用户开出的线同时站在这条规则的两侧：它给被切出来的那条线发消息、也从自己的切点读它，并协调开在它下面的工具线；而被它切出来的那条线既不给它发消息、也不读它，因为它是用户开出的（§6）。`end {content}` 不接受目标，所以它只结束调用方自己，不结束别的线（§6、§10、§11）。插件线不在任何名册上，工具不给它发消息，也不结束它。
- **父线块。** 一条线的 BranchLine 在父线结束一轮、且父线在 `max(读取游标, 上次告知的头部, 开启点)` 之后有新活动时，在这条线自己身上写一个带 key 的 `<dim-branch-parent>` 块（§7.1.1、§12.1）。块只报位置，是状态而不是请求；内容仍留在读取返回的页里。

| 线 | 用工具开线 | 用工具发消息、读 | 由谁结束（按约定） |
| --- | --- | --- | --- |
| `main`，根 | 能，不论它是不是默认分支 | 它开出的工具线 | 永不结束；Session 关闭不算结束 |
| 默认分支 | 能，只要它持有这个角色 | 它被切出来的那条线，以及它开出的工具线 | 它在默认分支期间谁都不能结束它，用户也不行：要结束它，用户先把默认分支移到另一条线上 |
| 其它工具线 | 不能（工具结果拒绝），但它保留它开出的工具线 | 它的父分支以及它开出的工具线 | 它自己（`end`）、用户，或祖先结束时连带 |
| 其它用户线 | 不能（工具结果拒绝），但它保留它开出的工具线 | 它给被切出来的那条线发消息、并从自己的切点读它，还读它开出的工具线；被切出来的那条线既不给它发消息、也不读它（`branch_not_kin`） | 用户，或祖先结束时连带；不是它自己 |
| 插件线 | 不能 | —（按创建者的协议） | 它的创建者（Goal 结束它的评审者，[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）、用户，或祖先结束时连带 |

谁结束哪条线，是调用方之间的约定，但要除去工具不让线替自己结束的那两类：插件线按创建者的协议走，用户线由用户结束（§11）。运行时只检查目标，不管是谁在请求，用户也一样：它存在、未结束，且既不是根、也不是默认分支、也不在默认分支的祖先链上，因为结束一条线会连带结束它下面的每一条（§10）。

## 5. 部件

所有部件都属于 TimelineTree 插件（[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)），每个部件放在拥有其状态的作用域（[ADR 002](../adr/002-plugin-scope.md) D1）。

| 部件 | 作用域 | Capability | 在本设计中负责 |
| --- | --- | --- | --- |
| TimelineTree | Session | 否 | 线的生命周期：每个子分支开启输入里的身份块、`TimelineInfo.source`、一条线的结束（`branch/end` → `branch/ended`）、线进入结束后拒绝输入、恢复规则、读已结束线的历史、默认分支及其 `branch/default` 事实，以及分支目录（§14） |
| TreeTimeline | 分支 | 是 | 把 `branch/end` 转给 TimelineTree，就像现在转 `branch/create`。对由工具调用发出的结束，它还转交关闭要等的那一轮结束 |
| BranchTool | Session | 否 | 按动作区分的 `branch` 工具（§11）。它每次调用时取调用方那条线的 BranchLine，与 WorkspaceTools 取 FileReads 的方式相同 |
| **BranchLine**（新） | 分支 | 是 | 一条线对自己相关各线的视图。它的角色（根、工具线、用户线或插件线）和父分支来自 `TreeCommands.info(自己的 id)`。它从自己的事实折叠出未结束的工具线和当前这一轮的状态。它写名册、空闲通知和结束通知，以及线成为默认分支或不再是默认分支时它收到的那条状态说明。名册是它自己的 key `branch:roster`，只列它开出的工具线（§7.1） |

BranchLine 注入 `B:Dispatcher`、`B:TimelineIdentity`、`B:BranchLifecycle` 和 `S:TreeCommands`，提供 `B:BranchLine`。

它的决定方式与 Goal 相同：依据已消费的状态，通过一个写入前会复核的自有写任务，并且只对加载截点之后的实时事件作决定。所以重载时什么都不会写两次，只有一个例外：如果加载时自己的历史里最后有一个仍在生效的名册键，而与折叠出的名册不一致，它就把名册写一次——因为补完一条已结束线的重启（§20.7）在根重放之前就记下了 `branch/ended`，之后没有实时事件跟随（§6.2）。同一条例外补上一次停在事实与说明之间的移动。`branch/default` 和每条状态说明分别是独立的批次（§4.5），所以进程在事实提交之后、某条说明写出之前停下时，事实照样生效、说明却缺了。加载时 BranchLine 对自己这条线比较两件事：它上面最新的有效 `branch:line` 块的 `default` 属性（没有这个属性就算不是默认分支），以及 `TreeCommands` 现在是否报告这条线是默认分支。两者不一致时，它在这条线的下一次模型请求之前补写与树一致的那条说明，一次；根永远不写。给新默认分支的说明会写出移动从哪条线开始，所以补写的加载必须知道那条线：`branch/default` 带着它，放在说明本来就需要的 `from` 字段里（§16），这正是 §4.9 允许一条事实存在的理由——补写的说明要用它来决定。把这条线的块与树对上的加载什么也不写，所以再次重启也不写。只有对树报告为默认分支的那条线，比较还会读这个块的文本，所以旧规则写下的说明在加载时被替换一次。

**默认分支的恢复。** 目录里只有一条线的 Session 就在根上，不为它读任何事实。有多条线的 Session 只读一条事实：在根自己的事件里，最新的一条 `branch/default` 或 `session/forked`，走现有的 `timeline_events_by_kind` 索引。目标仍未结束且可以被选中的 `branch/default` 指明默认分支；最新的一条是 `session/forked` 时，默认分支就是根，因为 fork 拷过来的所有 `branch/default` 都写在它的 `session/forked` 之前、一律失效；什么也没记就是根。目标已经不再是开着的线、指向插件线、以及指向升级前经客户端开出的线时，都回退到根（§4）。

## 6. 线之间的消息

消息是接收方 Timeline 上的一条 `input/message`，经 `TreeCommands.input` 写入，不新增输入通道。它带一个原始块，由生产者标记、不带 key，以及 [InputQueue](../../architecture/rust-agent/plugins/InputQueue.md) 的模式：

| 模式 | 对接收方的作用 | 何时用 |
| --- | --- | --- |
| `append`（默认） | 随接收方下一次模型请求一起送达。它从不开启新的一轮，也不唤醒 `wait` | 接收方在下一步应该知道的进展或背景 |
| `steer` | 在接收方当前这一轮的下一步进入；接收方空闲时开启新的一轮。它会唤醒 `wait` | 接收方必须马上处理，或接收方处于空闲 |

不开放 Queue。一条线给另一条线的唯一 Queue 输入是开启输入。

这两个模式和用词与 Collaboration 的消息相同（`append`、`steer`），含义也相同，因为两者用的都是 InputQueue 的模式。

- **标记。** `<dim-branch-message from="C" to="P">`。接收方凭这个标记把消息和用户的话区分开。RPC 只接收操作者内容、不接收块，所以客户端无法伪造消息。
- **允许的两端。** 一条规则，按一条线所处那条边来陈述：开出目标的那条线，不论它是谁；本文早先写 `main` 的地方指的就是它。

  | 组合 | 发消息 | 读取 | 拒绝 |
  | --- | --- | --- | --- |
  | 开线方与它开出的每条未结束的工具线 | 双方都可以给对方发消息 | 双方都可以读对方：开线方读它开出的线，那条线从自己的切点读开线方 | 无 |
  | 用户开出的线与它被切出来的那条线 | 用户开出的线可以给它发消息 | 用户开出的线从自己的切点读它；它被切出来的那条线既不给它发消息、也不读它，因为那条线是用户开出的，不是它开出的 | 它被切出来的那条线发消息或读取：`branch_not_kin` |
  | 开线方与它开出、已经结束的工具线 | 拒绝：它不再接受输入，自己也什么都不回答 | 开线方读它，未结束或已结束都读，因为开线方正是靠读取拿到它的结果 | `branch_ended` |
  | 插件线与其开线方 | 两个方向都不行 | 插件线读它被切出来的那条线；没有哪条线经工具读插件线 | 发消息，以及开线方的读取：`branch_not_tool_line` |
  | 其他任何组合：兄弟分支、相隔两代的线 | 不行 | 不行 | `branch_not_kin` |

  用户开的线是像工具线一样的对话线，它落在哪一行由它所处的那条边决定：作为被开出的线，它属于第二行，那一行的开线方是用户，不是它被切出来的那条线；作为工具线的开线方，它与其他开线方一样属于第一行。已结束的工具线保留它原来的那一行，只是发消息被拒：开线方仍然读它，开线方就是这样拿到它没来得及发出的结果。已结束的插件线仍归插件那一行，没有哪条线经工具读它。`end` 不属于任何组合：`end {content}` 不接受目标，所以它永远结束调用方自己（§10、§11）。协调沿父子关系走，所以它留在开线的那条线上，默认分支移动时它不跟着动；曾经的默认分支保留它开出的工具线，并继续与它们互发消息、互读。
- **读取。** `read {branch, after?, limit?}` 返回目标已提交历史的一页，未结束或已结束都可以，经 `TreeCommands.page` 从存储读取，按 Collaboration 的 `read` 渲染成员页面的方式呈现：输入、文本、工具调用与结果、每轮结束，带 `nextCursor`；工具结果条目携带其工具在该结果上声明的效果（[工具契约 A.6](contracts/session-boundary.md#a6-工具效果)），仅当结果至少声明了一个时才存在，路径原样，命令在第 200 个字符处截断并标出截断。目标是被调用方切出来的那条线，或调用方开过的线。两个方向共用一条游标规则：
  - 不带 `after` 时，读取从调用方上一次成功读取该线的位置继续：最后一次返回了页的读取的 `nextCursor`。此前没有读过时，读被切出来的那条线从调用方的切点开始，读调用方开过的线从那条线的第一个事件开始。
  - 显式 `after` 从该点读取。读被切出来的那条线时，低于切点的值会抬到切点，因为切点之前的事件已经是这条线自己的历史。读自己开过的线时，值原样使用，所以 `after: 0` 会从一条线的开头重读。
  - 每次成功读取都把游标移到它的 `nextCursor`，显式 `after` 的读取也一样；以最后一次为准。游标是调用方自己那条线的事实：BranchLine 从调用方自己的 `step/tool-use` 和 `step/tool-result` 折叠得到，所以重载和压缩都能重建它。
  - 一页可能在目标某一轮的中间结束：它读到目标已发布的头部，下一次读取从这一页之后继续。
- **知道何时读。** 读取是按需的，所以一条线还需要知道父线动了。带 key 的 `<dim-branch-parent>` 块（§4、§7.1.1）给出父线的头部和这条线的游标；线据此决定读哪些页。块从不携带内容，读这个块也不是在读父线。
- **不设上限。** 线的数量、消息数、steer 数都不设上限。消息往复由提示词规则（§12）、某条线自行结束，或用户（中断或结束）来终止。为什么这样是安全的，以及需要时由谁负责限制，见 §6.1。
- **空闲通知。** 一条工具线的一轮结束时，这条线上的 BranchLine 给开它的那条线写一条 steer（`<dim-branch-idle>`，§12.1），除非下列之一成立：
  - 这条线是以 `idleNotice: none` 开出来的；
  - 这条线就是默认分支；
  - 这一轮已经给开它的那条线发过 steer 消息；
  - 这一轮调用了 `end`；
  - 这条线还有待处理的驱动输入。

  第二条让一条已经是默认分支的线不再每轮都去唤醒开它的那条线：用户在它上面说话时，它在回答用户，不是在向开它的那条线汇报。再切走一次就自动恢复通知，因为这个条件读的是当前事实，不是某条说明。这样，一条线一声不响地停下时，开它的那条线不会一直等下去；已经 steer 过开线方的线，也不会重复通知它。通知带这一轮的状态（`completed`、`failed` 或 `cancelled`）。
- **等待。** 开线方用现有的 `wait` 工具等它开的线。steer 消息、空闲通知或结束通知都会唤醒它（[wait.rs](../../../crates/dim-agent-capabilities/src/wait.rs)）。

### 6.1 资源与限制

不设上限是安全的，因为线没有带来新的资源类别：

- 一条线的工作就是同一个 Session 的普通轮次，使用同一个 provider，受同样的 Permission 和 Approval 约束。
- 一条线同一时间最多运行一轮：Loop 每条线只持有一轮（[agent_loop.rs:106](../../../crates/dim-agent-capabilities/src/agent_loop.rs)）。
- 一条消息是由一次工具调用写入的一条有界的 `input/message`，大小受模型自己的输出约束。
- RPC 准入按连接设有上限（[架构](../../architecture/README.md)）。
- Collaboration 的成员也没有数量上限。

工具开的未结束线有总数上限。上限是整个 Session 的总数，计数规则只有一条：

- 它数的是 `branch` 工具开出、仍未结束的线，跨整棵树，以及会变成这些线的工具预留（§6.1“预留就占名额”）；
- 它不数关闭无法到达的那条路径上的线：当前默认分支和它的每条祖先。那条路径本来就被 `end` 保护（§10），数它会把这些关不掉的线算成名额，而用户不断在工具子线上继续时，被占满的恰恰是它正在用的那条线；
- 经 `session.branch_create` 由用户开出的线从不计数，也从不被这个上限拒绝。插件线同样不计数。

上限归 BranchTool，开线会越过上限时以 `branch_limit_reached` 拒绝。计数规则对一次开线的含义，由新开的线决定，而不是由开线方决定。`create` 开出的线是新的，所以它既不是默认分支（移动只把角色给已经存在的线），也不是默认分支的祖先（那是默认分支上面的线）。因此每一条新子线都不在受保护路径上，要占一个名额，不论它的开线方是根、是默认分支，还是一条正在当协调者的曾经的默认分支。在受保护路径上改变的是这条线自己的代价，不是从它开线的代价。改变计数的是移动，而移动算出来的是同一条集合差，只是按新的树来计算：所有未结束的工具线减去新的受保护路径，对每条未结束的工具线一次算清，而不是只算获得和失去角色的那两条。所以获得角色的线不再计数，而失去角色的那条线之下的每条工具线都重新计数，子树多深都一样。只更新这两个参与者，会在树已经不保护任何线时只数一条，从而放行一次 Session 根本没有名额的开线。移动不关闭任何东西，也不拒绝任何已经存在的东西。所以一次把线放回计数一侧的移动可以让 Session 停在上限之上，而下一次 `create` 才是拒绝的那一次，回答 `branch_limit_reached`。模型记录的场景都是这同一条规则。上限为二、根持有角色时已有一条开着的工具线，把那条线选为默认分支让计数从一降到零（`BranchEligibility-MoveToChild`）。从上限已满、受保护路径上是一条嵌套工具线的状态移回根，原先受保护的工具祖先重新计数，移动被接受、Session 停在上限之上，之后不再接受任何开线（`BranchEligibility-AtCapBelow`）。整个 Session 上的嵌套场景是 `BranchCapSwitch`：上限为二、`main → A → B` 是仅有的未结束工具线，选中 `B` 时两条都受保护、计数为零，移回 `main` 时 `A` 和 `B` 同时开始计数，也就是二，所以之后再一次 `create` 被拒绝（`BranchCapSwitch-BackToRoot`）。期间多开一条线时，同一次切换把 Session 留在上限之上（`BranchCapSwitch-OverCap`）。

拒绝只针对这次开线：一次移动让 Session 越过上限时，不关闭任何线，除之后的开线之外也不拒绝任何东西——用户接着用的那条线，并没有因为用户要求关闭什么而变得受保护。拒绝文本把这两件事都说出来，模型遇到它时能报告状态，而不是反复重试。

**预留就占名额。** `create` 在树锁下预留它的线，扣名额就在这一步：一把锁同时管住调用方的资格（§4）、开着的工具线和已获准的工具预留。预留之后它才等这次构建（[timeline_tree.rs](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)），而只停止等待的调用方会让那次构建继续跑。只数已发布的线，就会让两次调用同时拿走最后一个名额，其中一条还没发布。发布把预留变成本结束的线，不再扣一次；确定失败的创建释放预留；只停止等待的调用方保留它，因为构建还在继续。所以一次检查报出的计数，在发布前后是同一个：名额恰好在那一步花掉一次。

如果以后其他东西也需要限制，每一项都已有归属，而且都不需要改机制：

| 限制 | 归属 |
| --- | --- |
| 开着的工具线 | BranchTool 按 §6.1 的计数规则在预留创建时，把调用方的资格、开着的工具线与已获准的工具预留一起在树锁下扣除，发布时不再扣一次；默认分支及其祖先不计入 |
| steer 频率 | 发送方的 BranchLine 统计它自己 Timeline 上的 steer 消息 |
| 整个 Session 的模型成本或并发 | 归模型调用层。这不是分支特有的限制：它同样适用于成员和 fork |

其他保护措施是：

- 提示词规则；
- 已结束的线拒收消息；
- 用户的结束和中断；
- §12.3 中的刷屏、过度拆分和来回空转场景，在本地测量并在实现 PR 里报告，从不作为 CI 阈值。

开着的线确实有代价：

- 开着的线保留它已加载的部件。
- 重启时每条开着的线都会重新加载。Session 关闭和空闲卸载都不会结束一条线。
- 开着的工具线会让开线方的名册变长，名册只列它开着的线。每次从线外结束这样一条线，开线方上就多一条通知。

永不结束的工具线由它自己的协议（`end`）和用户负责，开线方的名册让它始终可见。Pursuit 的评审者由 Goal 结束（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）。

### 6.2 已知代价

- **重启可能丢掉一条空闲通知。** 如果进程停在一条工具线一轮结束之后、它的空闲通知写出之前，这条通知就永远不会写出，因为 BranchLine 只对加载之后的实时事件作决定。Pursuit §4.2 也留着同样的窗口。开线方的名册上仍有这条线，可以 `read` 它，用户也能看到它处于空闲。
- **重启可能丢掉结束通知。** `branch/ended` 与名册输入之间同样有窗口，一条线被外部结束时本该写出的结束通知会丢掉。加载时的补写只写名册，所以开线方是从名册的退场得知，而不是从通知。
- **压缩后的名册很短。** 一条线压缩之后，它的名册只保留每条线的 id 和目的的第一行。开线方告诉过一条线什么，只留在压缩摘要里，或只能经 `read` 看到；而 `read` 会把那条线的原始事件一页一页带进开线方的上下文。
- **成为默认分支的线保留自己的历史。** 把一条对话线选为默认分支，只改变它从此以后被告知什么（§9），不改它已经记下的东西。它早先的身份块和目的都留在自己的历史里，而说它是默认分支的状态说明在同一个 key 下重述了那份身份、也重述了目的（§12.1），所以两者不会互相矛盾，压缩也会重放这条说明。
- **更早一次移动写下的说明可能过时。** 一条说明只在它是该线最新块时成立。之后的一次移动如果没给这条线写东西，早先那条说明就留在原处，所以说明里不写“别的某条线现在是不是默认分支”：那会变，而这条线并不会被写入。说明里关于角色写下的，是树对这条线自身情形适用的规则：它能不能开线、发不发空闲通知、能不能对自己 `end`。默认分支祖先链的保护则不同：它随另一条线的移动而变，所以在调用 `end` 时从树里判定，不从说明里读（§10）。

## 7. 身份与上下文

### 7.1 带 key 的状态：`branch:line`、`branch:roster` 与 `branch:parent`

每条线都用同一个 key `branch:line` 下的块，回答"我是哪条线"。这个 key 在每条线上都一样。开过工具线的线还用自己独立的 key `branch:roster` 下的块回答"哪些线是我的"。两个 key 分开，是因为现在开线的线不一定是根，它同时带着身份和名册：共用一个 key 会让名册写入让这条线自己的身份退役，也会让状态说明让名册退役。一条线还为自己被切出来的那条线保留一个带 key 的块 `branch:parent`（§7.1.1）。

- **子分支上：它的身份。** TimelineTree 在每个子分支上写入，无论工具线、用户线还是插件线。它是子分支开启 Queue 输入的第一个块，key 为 `branch:line`，写明：
  - 子分支的 id、父分支和切点；
  - 它是同一个 agent，上面的历史就是它自己的；
  - 从这里起它并行工作；
  - 上面那些发给父分支的消息（例如名册）属于父分支。

  工具线的身份块还带上它的目的（`create` 的 prompt）和线协议。`idleNotice: none` 时，TimelineTree 在这条开启输入的身份块上带 `idle-notice="none"`。BranchLine 在考虑空闲通知时从这条线自己的开启输入里读它；恢复不重放输入。缺少这个属性就选 `steer`，包括这个选项出现之前写下的历史。这个设置只管自动的空闲通知。在用户线和插件线上，创建者的 prompt 仍是开启输入的 `user` 内容，排在块之后。
- **开线方上：名册。** 开过工具线的线上的 BranchLine 依据它自己的事实写一条带 key 的 Append：
  - 来源为工具的 `branch/create`；
  - 其中某条线的 `branch/create-failed`；
  - 其中某条线的 `branch/ended`。

  名册列出它用工具开出的每条未结束的线，带线的 id 和目的的第一行；一条都不剩时这个 key 退役。插件线从不在名册上，所以 Goal 的评审者不会出现在任何名册里。用户开出的线也不在名册上：它留在 Session 的分支目录里，用户和客户端在那里看得到它（§14），而它自己开出的工具线写进它自己的名册。
- **覆盖。** 两个 key 各自退役，互不影响。子分支在继承的前缀里也带着父分支的名册；它自己写名册时替换的是自己 key 下的那个继承块，它的身份从不碰继承来的名册，继承来的名册也不会让它的身份退役。从子分支的开启输入起，子分支上最新的有效 `branch:line` 块就是它的身份，最新的有效 `branch:roster` 块就是它自己的名册。继承来的名册是被退役，而不是留着顶替它还没写的名册：TimelineTree 用读 `branch:parent` 时同样的方式（§7.1.1），从子分支继承的前缀的带 key 状态里读 `branch:roster` 是否还活着，还活着时就在开启输入里、身份块之后再带一个原始块，key 为 `branch:roster` 且已退役。所以一条不开线的子分支根本不持有名册，而不是持有父线那张它从没开过的线的清单；这条线压缩时重放的是那次退役，不是继承来的块。从 `main` 切出来、而 `main` 当时还没写名册的子分支什么都继承不到，也就没有这个块。
- **状态说明是同一个 key 的后续块。** 一条线成为默认分支或不再是默认分支时，BranchLine 在这条线上、`branch:line` 这个 key 下写一条带 `default` 的 Append。因为它用的就是身份的 key，它会取代出生块，所以这条说明逐字重述那份身份，只改移动所改变的那几句（§12.1）。根不写说明：它开线和永不结束这两条规则都不随角色改变，所以根的名册独占这个 key，根日后压缩时也只重放名册。说明写在 `branch:line` 下、名册写在 `branch:roster` 下，所以说明永远不会让名册退役，不论名册在哪条线上。

key 与模式互不相关（[history.rs](../../../crates/dim-agent-capabilities/src/history.rs) 的 `render_input`）。Goal 的 `set` 已经在 Queue 输入里放带 key 的块（[Goal §5](plugins/Goal.md#5-上下文映射)）。

身份放在开启输入里，而不是在 `branch/start` 时写一条 Append，原因有两个：

- Append 可能在开启输入之后才提交（[Goal §3.4](plugins/Goal.md#34-新分支清除继承的目标)）。
- 在 `branch/start` 时还不知道目的。

### 7.1.1 父线块 `branch:parent`

读取是按需的，所以一条线还需要知道它被切出来的那条线动了。这条线上的 BranchLine 观察父线的轮次结束，在自己 Timeline 上写一条带 key 的 Append，`branch:parent`。这是这条线自己的事实，从不属于父线：没有哪个部件折叠另一条线的事件。

- **信号。** TimelineTree 本来就分派每条线的 `turn/end`；线自己的节点发布它最新一次轮次结束的序号。子分支的 BranchLine 等待父线的信号和自己的写队列，任一变化排入一次决定。决定取决于这条线自己的状态，而不是信号：只要它没有持有 `branch:parent` 块、且父线的头部超过了最新块告知的头部，它就写一个。在块被持有时到达的移动，会在该块被消费、删除或中断时写出，所以不会丢失。没有部件为此读取父线的历史。
- **条件。** 只有以下条件全部成立时才写这个块：
  1. 这条线不是根，且不在结束；
  2. 这条线上没有更早的 `branch:parent` 输入仍被持有，即已提交、尚未消费；
  3. 父线在 `from = max(A, told, opened)` 之后有新活动：`A` 是这条线对父线的读取游标（还没读过时是它的切点），`told` 是这条线上最新一个 `branch:parent` 块报告的头部（没有时为 0），`opened` 是这条线的 `branch/create` 事件在父线上的序号，每个切点都不高于它。线存在之前发生的活动不算新闻，需要时线自己去读。
- **什么算新活动。** 只有两类事件算：操作者输入，即带 `user` 内容的 `input/message`；以及工具调用。创建调用的 `step/tool-use` 写在 `opened` 之前，所以从不会被算进去。其他都不单独算：父线的文本、它的轮次结束、只有块的输入（名册、来自各线的消息）、以及创建调用的结果。规则是：父线行动时、或操作者告诉父线事情时，线被告知；只为开启它的那一轮（它的创建调用结果、文本和轮次结束）则从不告知，否则每条线都会白白读一次父线，并学会忽略这个块。
- **检查方式。** 父线的事实通过唯一的事件查询读取（[ADR 019](../../adr/019-event-query.md)）：该分支的 `input/message` 和 `step/tool-use` 事件，从新到旧分页，直到有一个符合条件，或读取到达 `from`。不折叠任何其他线的事件。
- **写入。** 一条带 key 的 Append，key 为 `branch:parent`，经 `TreeCommands.input` 写在这条线自己身上，属性是父线的 id、`head` 和 `after`。同一 key 的后一个块会在带 key 的基线里取代前一个。写完之后折叠记下新头部和新输入，所以队列里的第二次决定不会再写。
- **开启时。** 子分支继承的前缀里持有活的 `branch:parent` 块时，子分支的开启输入在身份块之后再带一个原始块：key 为 `branch:parent`，活的、不 retire，名字是开出它的那条线 P，`head` 和 `after` 都等于切点 H。它像身份块取代继承的名册那样取代继承的块（§7.1），所以这条线最新的 `branch:parent` 总是名字它自己的父线，它的 `told` 从 H 开始，而 H 等于 `opened`；压缩重放的是这个块，从不重放上面那个，P 移动时照常的新块取代它。只有嵌套的线才会拿到它：从 `main` 切出来的线不继承任何 `branch:parent` 块，它的开启输入逐字节不变。TimelineTree 在写身份块的地方组合它，从子分支继承的前缀的带 key 状态判断这个 key 是否活着，绝不看渲染后的历史。它在开启子分支时读那份状态，如同读一条恢复的分支的 facts；读取失败时这次创建和任何其他开启失败一样失败（branch/create-failed）。
- **随后发生什么。** 运行中的线在下一次模型请求时拿到这个块。空闲的线不会被唤醒：这个块在它下一轮的第一个请求里、先于开启那一轮的输入被消费，这是有文档记录的 Append 规则（[session access](contracts/session-access.md)、[Loop](../../architecture/rust-agent/plugins/Loop.md)）。重载不写任何东西；重启最多写当时已经该写的那个块。`main` 拿不到块：它上面没有东西。
- **记录。** 这条线发送的是它自己配置里持有的 System。指南在每条线上都一样（§7.3），所以 System 没有相对继承来的那份发生移动的线，引用的是记录它的那份配置，无论记录在哪条线的链上；线的 System 移动时，它像任何分支一样写出自己的 `context/configured`。
- **被否决的替代方案。**
  - 在线自己一轮结束时写，错过了这个变化存在的场景：线结束一轮，父线继续，随后用户或父线来找这条线。那一轮不携带块。
  - 在线一轮开始时或它的请求之间写，是在请求准备好之后才响应事件；一条已提交的 Append 可能落在它本该服务的那次请求之后，而且需要 Loop 里的钩子。
  - 根给每条线推摘要或 steer，重复了线自己可以读到的内容，还会唤醒空闲的线。
  - 新的事件类型或存储格式改动没有必要：一条原始的带 key 输入已经承载这份状态。

  这个块只说明位置，不携带内容；内容留在读取返回的页里。

### 7.2 压缩

压缩按线进行。一次压缩是压缩方自己 Timeline 上的一条 `context/compaction` 事实。

- 子分支继承祖先在切点之前的压缩记录。事件 id 在继承的前缀里保持不变，所以 `retain_from` 在子分支上的重放方式不变（[ContextCompaction](plugins/ContextCompaction.md)）。切点之后，子分支各自压缩。
- 子分支的压缩不改变父分支，父分支的压缩也不改变子分支。

带 key 的基线会重放 `retain_from` 之前每个 key 最新的有效块。因此：

- 开线方压缩时，它的名册会重放；
- 子分支压缩时，它的身份会重放；名册只在子分支自己写过名册时才重放，继承来的名册在开启时已退役（§7.1）；
- 一条线压缩时，它最新的 `branch:parent` 块会重放，所以这个块能活过这条线自己的压缩；最新的状态说明也一样。

压缩不删除任何行：这条线对读取游标和最新块的折叠，在重放时从同样的事件重建。

两个 key 让每条线的基线里对"我是哪条线"恰好只有一个答案，对"哪些线是我的"至多一个。继承来的答案不会比这条线自己的答案活得更久，名册也不会让身份退役。消息不带 key：压缩之后，它们像其他输入一样留在摘要或保留的轮次里。

### 7.3 缓存前缀

各线只在尾部不同：

- 根线确定 system 和模型，子分支不改它们（[prompt](../../architecture/rust-agent/prompt.md)）。
- 提示词来源按根的作用域加载，由子分支继承（[PromptSources](../../architecture/rust-agent/plugins/PromptSources.md)）。
- 每个 Session 只有一个 ToolRegistry，所以每条线的工具列表都一样。这就是不能开线的线上的 `create` 由工具结果拒绝、而不是拿掉工具的原因。
- 协议只缓存显式标记内容时，请求会给这份前缀打上缓存断点：最多两个，System 末尾一个，最后一条用户或工具结果消息的最后一个块一个，且只在各自条件成立时才放置（[Anthropic 提示缓存断点](../../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)）。
- 身份块是子分支自己的第一个事件，紧跟在继承的前缀之后。所以子分支的第一次请求能复用父分支到切点为止的前缀。
- 名册更新是追加在开线方历史末尾的 Append。状态说明同样追加在这条线自己历史的末尾，所以把一条已经开着的线选为默认分支，也不动它已缓存的前缀：只有它的尾部增长。
- 父线块和移动默认分支时写的状态说明，都追加在这条线自己历史末尾，所以它们从不移动任何线继承的前缀。
- 指南在每条线上都一样，包括父线块的条目。

### 7.4 Goal 与 Pursuit

**规则。** 一条线的开启输入由 TimelineTree 写入。它的第一个块是 `branch:line` 标记，所以按 [Goal A5](plugins/Goal.md#附录-e-假设)（操作者输入是"带 `user` 内容、且第一个原始块不是来源标记"），它是有来源的输入，不是操作者输入，不管创建者给了什么 `user` 内容。Goal 的冲突表记录了这一点（[Goal 附录 B](plugins/Goal.md#附录-b-完整冲突表)）。Goal 自己的规则不变。它的代码只随 [Pursuit 修订](plugins/Pursuit.md#13-结束评审者随-427)改变。

在本设计之前，插件线的开启输入（创建者的 prompt 作为 `user`，没有块）算操作者输入。工具线的不算：它带 `dim-branch-task` 块，没有 `user`。现在两者都是有来源的输入。

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

其余靠协议，写在线的身份块和工具描述里：

- 开线方在 `create` 的 prompt 里给每条线分配它自己的文件或区域。
- 一条线要改它区域之外的文件之前，先告诉它的父分支。
- 遇到 `file_modified_since_read` 时，这条线重新读取文件，告诉它的父分支之后再继续。

文件改动只有工具声明的一种事实，即 `fileWritten`：`edit` 和 `write` 只要已经发布了文件，就在结果的 `effects` 中声明它和路径——成功时如此，发布之后失败（例如同步父目录失败）时也如此；在发布之前失败或被取消的调用不声明（[工具契约 A.6](contracts/session-boundary.md#a6-工具效果)）。客户端折叠这些声明得到的集合在构造上就是部分的——它漏掉 `exec`、MCP 工具以及 Dim 之外对文件的改动——所以折叠描述的是工具声明的写入，而不是工作区的改动。没有任何方法承诺一轮或一条线的完整改动集。

WorkspaceTools 不变。

## 9. 用户

- 交互面属于 Session。每条线都以这个 Session 的身份与用户打交道。
- **提问。** 任何一条线都可以用 `ask_user` 提问。答案作为这次调用的工具结果，回到提问的那条线（[Questions](plugins/Questions.md)）。
- **审批。** 一条线工具调用的审批直接交给用户。审批通知带 `branchId`，客户端会标出是哪条线。
- **不经默认分支转达。** 这是同一个主体在问它自己的用户。一条已挂起的审批留在提问的那条线上：它的通知带着那条线的 `branchId`，之后默认分支怎么移，都不会把请求搬走，也不会改变答复送到哪条线（§13）。
- **提交。** 用户可以向任一条未结束的线提交（带 `branchId` 的 `session.submit`）。不传 `branchId` 的调用送给默认分支。
- **中断。** 中断一条线只结束它当前这一轮，不结束这条线。不传 `branchId` 的 stop 停的是默认分支。
- **结束。** 除根、默认分支和默认分支的每条祖先之外，用户可以结束任一条线（`session.branch_end`，§10）。默认分支谁都不能结束，用户也不行：要结束它，用户先把默认分支移到另一条线上。用户开的线就是这样结束的，绝不由它自己 `end`。
- **选择默认分支。** 用户可以用一个方法（`session.branch_set_default`，§14）把任意未结束的对话线——工具线或用户自己开的线——选为默认分支，也可以移回根。根在它不是默认分支时就是合法目标，插件线永远不是。旧默认分支照旧开着，并保留它开过的线；不复制任何东西，也不移动任何历史。这次移动把事实写在根上，并给它的每个不是根的参与者写一条状态说明：新的默认分支和旧的，是根的那一方不收（§12.1），所以移回根时写的是失去角色那一条。它同时也移动开线的权利：离开这个位置的那条线失去这个权利、保留自己的线，接手的那条线得到它。根的这个权利不在交换之内，因为不论它是否持有这个位置都保有它，所以移动什么也拿不走。
- **移动不做什么。** 它只改变 Session 的对话在哪条线上。它不动工作区，所以旧默认分支改过的文件仍是改过的；它不动 `/fork`，fork 仍只把根拷到某个静止点（[fork](appserver/session-deletion-and-fork.md)）；它也不结束任何一条线。

## 10. 结束一条线

| 事件 | Timeline | 数据 | 角色 |
| --- | --- | --- | --- |
| `branch/end` | 要结束的那条线 | `{source, ancestor?}` | 指令 |
| `branch/ended` | 它的父分支 | `{branch, endEvent, source, ancestor?}` | 结果，由 TimelineTree 写入 |

这一对事件与 `branch/create`、`branch/create-failed` 对称：一条 Timeline 上的指令由 TreeTimeline 转交，结果由树写出。`endEvent` 是 `branch/end` 在要结束的那条线上的序号。`branch/ended` 写在开出结束那条线的线上，`main` 开出时就写在 `main` 上，所以点到某条线的用例在开线方不是根时照样成立。

`source` 是现有的 `BranchSource`，与 `branch/create` 用的是同一个类型，所以创建和结束以同样的方式记录发起方：

| 来源 | 发起方 |
| --- | --- |
| `{kind: "tool", turn_id, request_id, call_id}` | 要结束的那条线自己的一次工具调用：`branch` 工具的 `end` |
| `{kind: "host", operation_id}` | 一次宿主操作，可以是 App Server 的 `session.branch_end`，它像 `session.branch_create` 那样生成 `operation_id`（一个 uuid，[connection/tree.rs](../../../crates/dim-app-server/src/connection/tree.rs)），也可以是带自己 id 的插件，例如 Goal 的 `goal:evaluate:N`。用户结束记录的是它创建时记下的 `{kind: "user", operation_id}` 变体 |

`ancestor` 指向用自己那次结束发起连锁的线：也就是本身不带 `ancestor` 的那次结束。连锁结束的每条后代都带这条线，`source` 也从同一次结束复制，于是（source, ancestor）这对值指明一次结束操作：谁要求的，在哪条线上。自己要求结束的线有 `source`、没有 `ancestor`；已被连锁结束之前就提交了自己结束的后代，保留它自己的那对值。`branch/ended` 从这条线的 `branch/end` 复制 `source` 和 `ancestor`，并且总是写在结束的那条线自己的父分支上，取自目录。

**一个通用操作。** `TreeCommands.end(id, source)` 和 `Session::end_branch(id, source)` 只检查目标：它存在、未结束、不是根、不是默认分支，也不在默认分支的祖先链上。结束根就等于关闭 Session，而结束一条线会连带结束它下面的每一条，所以默认分支整条链都受保护。这些检查都是关于目标的，不是关于调用方的。它们从不检查是谁在调用：用户、开线方和插件遇到同样的守卫，要结束默认分支，永远得先把默认分支移走。

这项检查在调用那一刻、在移动默认分支同样要拿的树锁下，从目录里读出这条链，所以移动和结束不会交错出一个规则不允许的状态。因此只有两种次序被接纳：要么结束被接纳、移动被拒绝，因为目标或它的某个祖先正在结束或已经结束；要么移动被接纳、之后对新的默认分支链上某条线的结束被拒绝。目标或它的任一祖先仍处于 `ending` 时，移动以 `branch_target_protected` 被拒绝，所以一条线不会在某个祖先的连锁已经在写它的后代时被选为默认分支。被拒绝时什么也不写。

**工具的结束是一次操作，它的最终消息也能活过停机。** `branch` 工具的 `end` 结束调用方，并给父线留一条最终消息（§11）。这两次写分别在两条 Timeline 上，而 `TimelineStore::append` 一次只为一个 Timeline key 提交一个批次，所以没有哪个批次能同时装下它们：树锁只能把它们在内存里排定次序。所以顺序选的是停机后仍能恢复的那一种，仍在同一把树锁下：

1. 检查守卫；
2. 在调用方上提交 `branch/end`，**带上那条最终消息**；
3. 再从这次结束把消息以 steer 送到父线。

先拿到锁的移动拒绝整次调用，所以一条仍然开着的线不会写出任何消息；先拿到锁的结束拒绝移动，父线收到的是一条真的结束了的线的消息。进程停在这两次写之间时，留下的是一条已经结束的线和一条还没被告知的父线；恢复会把那条消息送到（§20.26b）。

**不会重复送达。** 这条 steer 的 submission id 由结束的 `source` 推出，即 `branch-final:<line>:<call id>`，而 `branch/end` 里记下的 source 让重启后算出的还是同一个 id。`Timeline::input_checked` 对每个 submission id 只接受一次（[timeline.rs](../../../crates/dim-agent-runtime/src/core/timeline.rs) 的 `input_checked`、`submission`）：它在接收方自己的 Timeline 上找带这个 id 的 `input/message`，已落盘和尚未发布的都算，找到就返回已有的序号、什么也不写。重启后这次查找走恢复到的头部，也就是该 Timeline 在存储里的 `head`，所以停机前已提交的消息会被找到，再试一次也不写。一个 id 不能覆盖的是另一次写：结束与送达在两条 Timeline 上，永远不会是一个批次。上面的顺序换来的就是这个，补它的是恢复。

**恢复。** 一条线自己的 `branch/end` 是带最终消息的工具结束时就欠父线一次送达，不论停机是在送达之前还是之后。它在补完已结束线（§20.7）的旁边、在这条父线的下一次模型请求之前，用同一个 `input_checked` 调用和同一个推出的 id 送达：按 submission id 查一次，缺就写、在就什么也不写，所以送达完成后的重启不写，再重启也一次都不写。父线上的 `branch/ended` 写在关闭之后，也就写在送达之后，所以已经拿到结果的父线永远不会再被欠一条消息。

- 结束一条已经结束的线，什么也不写，回复 `branch_ended`。
- 还在打开的线还不是线：它的创建仍可能失败，而一条线只能有一个终结事实，所以在它发布之前 `end` 回答 `unknown_branch`。在加载时结束一条线的开线方等 Session，而 Session 在恢复完成后才发布（§17 R1）。
- 关闭 Session 不算结束。
- "开线的一方管理它的线"是调用方之间的约定，不是运行时规则：
  - `branch` 工具的 `end` 不带目标，所以结束的是调用它的那条线，这是工具的设计；这条线是默认分支时被拒绝，因为 Session 的对话就在它上面；
  - 父分支想让子分支结束，就用 steer 请它结束，这是工具的约定。

顺序：

1. 入口在这条线上提交 `branch/end`。此后这条线不再接受新输入，也不再开出新线，它下面的线也不再开出新线：这些入口都以 `branch_ended` 失败。它下面的线仍可接受输入，直到它自己提交 `branch/end`（第 3 步）。
2. 这条线上的 TreeTimeline 消费 `branch/end`，转给 TimelineTree。
3. TimelineTree 先结束这条线的后代，走同样的步骤；无论多深，每条后代都复制这个 `source`，并把 `ancestor` 设为这条线。
4. 关闭时机：
   - `tool` 来源：在 `branch/end` 之后这条线第一次结束一轮时关闭，因为这次调用就在那一轮里；线空闲时立即关闭；
   - `host` 或 `user` 来源，以及因祖先结束而结束的线：立即关闭，中断当前这一轮。
5. `Branch::close()` 中断这条线、结算它的工作并卸载它（[branch.rs](../../../crates/dim-agent-runtime/src/branch.rs)）。
6. TimelineTree 最后在父分支上写 `branch/ended`。

在父分支上，BranchLine 只处理它开出的工具线；插件线归它们的创建者，而用户开出的线不在名册上、由用户结束，所以开出它的那条线不为它写任何东西（§6、§7.1）：

- 工具线经工具结束自己时（`tool` 来源且没有 `ancestor`），只写名册更新。最终消息从这条线自己的 `branch/end` 送出，如上面几段所述；停机漏掉的那一条由恢复补送。
- 工具线的其他任何结束，写名册更新加上结束通知（§12.1），以 steer 写在同一条输入里。

`tool` 来源且没有 `ancestor` 时，客户端显示"自行结束"；其他情况显示结束它的操作；由连锁结束的线还会写明它的祖先，例如"ended with X"。

**恢复。** 父分支上有某个子分支的 `branch/ended` 时，这个子分支不活跃。恢复中的父分支读取历史时会报告这些事件（§5）。这条规则加入现有的几条：父分支不活跃、父分支上有这个子分支的 `branch/create-failed`、子分支的创建从未完成。

一条线有 `branch/end`、但父分支上没有对应的 `branch/ended` 时，不打开它。恢复按最深的线优先补完这类结束：在父分支上补写每条缺失的 `branch/ended`，`source` 与 `ancestor` 取自 `branch/end`，这条线自己的结果最后写。对于它下面的线，那个父分支同样不会打开，所以记录写进它的存储历史，它的固定头部随之延长。其下自己从未开始结束的线不补写 `branch/ended`：父分支已关闭、不会再重放，写上去也没有人读。恢复把每条这样的线按它停下的头部记为已结束，`source` 取自父分支的 `branch/create`，它的历史仍可读取（§1"结束之后"）。

**结束之后：**

- 这条线留在分支目录里，它的历史仍可从存储读取：经 `session.history`（v3），也经工具的 `read`。它的头部不再移动。
- fork 照旧复制目录行和 main 的事件。在 fork 点之前结束的线，在 fork 里也不活跃，因为它的 `branch/ended` 在复制的前缀里。fork 出来的会话，默认分支是它的根（§13）。
- 删除 Session 时，这条线随之删除。
- Session 关闭和空闲卸载都不算结束：各线恢复后仍未结束。

**插件开的线。** 每个开线的一方按自己的协议结束自己的线。Goal 在评审了结之后结束它的 Pursuit 评审者；结论之前的外部结束，它判为 `failed`（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）。

## 11. `branch` 工具

工具接受一个按动作区分的输入，取代 `{prompt, at?}`。风险仍是 `session`，Approval 照旧放行每次调用。

| 动作 | 输入 | 谁 | 作用 | 结果文本 |
| --- | --- | --- | --- | --- |
| `create` | `prompt`、`at?`、`idleNotice?`（`steer` \| `none`） | 根，以及当前的默认分支 | 开一条切在 `at` 的工具线（默认与现在相同：在本次请求的模型开始之前），它向调用方汇报。这次开线会越过 Session 上限时以 `branch_limit_reached` 被拒绝，名额在预留时扣除（§6.1）。空闲通知默认是 `steer` | `Line created: C` |
| `message` | `to`、`content`、`mode?`（`append` \| `steer`） | 一条线，给它开出的工具线，或给它被切出来的那条线 | 在 `to` 上写一条 `input/message` | `Sent to C.` |
| `read` | `branch`、`after?`、`limit?`（1–200） | 一条线，读被切出来的那条线或它开出的工具线，未结束或已结束都可。用户开出的线按这一条读它被切出来的那条线；那条线不给它发消息、也不读它，因为它是用户开出的 | 从存储读一页；不给 `after` 时从调用方的游标开始（§6） | 这一页的 JSON：items、`nextCursor`、`done` |
| `end` | `content` | 不是默认分支的工具线，对它自己 | 树锁下的一次 `TreeCommands.end(C, source, final_message)`：先检查守卫，在 C 上写带最终消息的 `branch/end`，再用由 `source` 推出的 submission id 把这条消息（steer）送到父线（§10）。被拒绝的结束不写最终消息 | `Line C is ending; your final message was sent to P.` |

输入 schema 为 `{action, prompt?, at?, idleNotice?, to?, content?, mode?, branch?, after?, limit?}`，`action` 必填，`additionalProperties: false`。`idleNotice` 只对 `create` 有效。

拒绝以错误结果返回（代码在 `structuredContent`），什么也不写：

| 情形 | 文本 | 代码 |
| --- | --- | --- |
| 在既不是根也不是默认分支的线上 `create` | `Only the root and the default branch open lines. Tell one of them if more parallel work is needed.` | `branch_not_opener` |
| 已经达到开线的工具线上限时 `create`，已获准的预留计入在内 | `This Session holds its limit of open lines. Continue on this line, or end one of its open lines first.` | `branch_limit_reached` |
| 在亲缘之外 `message` 或 `read` | `You can message or read only the lines you opened and the line you were cut from.` | `branch_not_kin` |
| 对插件线 `message`，或 `main` 读它 | `Line C was not opened with this tool; it follows its own protocol.` | `branch_not_tool_line` |
| 向已结束的线发消息，或与结束发生竞争 | `Line C has ended. Read it to see what it did.` | `branch_ended` |
| 在根、默认分支、插件线或默认分支的祖先上 `end` | `Only a line opened with create can end itself, and not while it or a line below it is the default branch.` | `branch_cannot_end` |
| `read` 的 `after` 超过目标的头部 | `after A is past line T's head H; a read starts at or below H` | 无：带错误结果，不带代码 |

这些拒绝是工具自己的规则。`TreeCommands.end` 本身只检查目标（§10）。因为守卫、`branch/end` 和送达是移动同样要拿的树锁下的同一次操作，先让目标成为默认分支的移动会在任何消息写出之前把整次调用挡下，前面那条拒绝就是这次调用的全部效果。如果线已经提交了自己的 `branch/end`，再来的 `end` 取 `branch_ended` 一行，不会再写一条最终消息：第一次调用的消息就是这条线最后的话。已经提交 `branch/end`、却停在送达之前的结束不是一个要回答的新结束：消息记在 `branch/end` 本身里，恢复会送达一次（§10）。

## 12. 提示词

### 12.1 上下文映射

`C` 是这条线，`P` 是它的父分支（根开的工具线，父分支是 `main`）。标记内的内容按 TimelineTree 现在的方式转义（`context_block`）。

| 写入 | 位置、key | 模式 | 文本 |
| --- | --- | --- | --- |
| 身份，工具线 | `C`，开启输入的第一个块，`branch:line` | Queue | `<dim-branch-line branch="C" parent="P" source="tool">` / `You are line C of this session. Line P opened you at this point of its history to work in parallel with it. You are the same agent as P: everything above is your own history, and you continue from here on your own line. P keeps working on its line; you can read its work after this point, and it can read yours. Messages above that were addressed to P, such as its list of open lines, are P's, not yours.` / 空行 / `Your purpose, as P wrote it:` / `<prompt>` / 空行 / `How you work with P:` / `- Work on this purpose only. You cannot open lines unless this line is the default branch of the session, and this block does not make it one: a status note would. If more parallel work is needed and you cannot open a line, tell P.` / `- Read P with the branch tool (action read, branch "P") when what P did or decided since your cut changes your work; the parent block reports when P has moved. Message P only for what it must act on: a decision that changes its work, a blocker only the user can settle, a conflict with another line, or a change to your purpose. Use mode "steer" for what P must act on now, "append" for what P should see at its next step. Do not send P a progress report it can read, and do not page through P's whole stream: read the page that answers your question, then act.` / `- You share the workspace with P and its other lines. Change only the files P gave you; before changing any other file, tell P which. If edit or write fails because the file was modified after it was last read, another line may have changed the file: read it again and tell P before you continue.` / `- The user can talk to you directly. Answer the user, and ask with ask_user when only the user can decide.` / `- When your purpose is done, or cannot be done, call the branch tool with action "end" and content saying what you found, did, changed and left undone. If you already told P that in a message, your end content carries only what is new. Read P once more before you end if the parent block says P has moved, so your last word still holds. That is your last action; P receives it.` / `</dim-branch-line>` |
| 身份，用户线 | `C`，开启输入的第一个块，`branch:line`。用户的 prompt 是这条输入的 `user` | Queue | `<dim-branch-line branch="C" parent="P" source="user">` / `You are line C of this session, opened from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You can read P's work after this point. Messages above that were addressed to P are P's, not yours. The message after this one says what this line is for.` / `</dim-branch-line>` |
| 身份，插件线 | `C`，开启输入的第一个块，`branch:line`。创建者的 prompt 是这条输入的 `user` | Queue | `<dim-branch-line branch="C" parent="P" source="host">` / `You are line C of this session, opened from line P at this point of its history. You are the same agent as P: everything above is your own history, and you continue from here on your own line, in parallel with P. You can read P's work after this point. Messages above that were addressed to P are P's, not yours. The message after this one says what this line is for.` / `</dim-branch-line>` |
| 名册 | `O`，`branch:roster` | Append | `<dim-branch-lines branch="O">` / `Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it can read your work after that point, and you can read its.` / `- C1: <目的的第一行，最多 120 个字符>` / … / ``They message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you.`` / `</dim-branch-lines>` |
| 名册，没有未结束的线 | `O`，`branch:roster` 退役 | Append | `<dim-branch-lines branch="O">` / `All lines you opened have ended.` / `</dim-branch-lines>` |
| 名册，继承来的，子分支开启时 | `C`，开启输入，身份块之后，`branch:roster` 退役 | Queue | `<dim-branch-lines branch="C">` / `The list above belongs to line P and names the lines it opened. This line has opened none of its own, so it keeps no list.` / `</dim-branch-lines>` |
| 父线块 | `C`，`branch:parent` | Append | `<dim-branch-parent branch="P" head="H" after="A">` / `Line P has new activity: it was at H when this was written, and you have read it to A. Read what is new with the branch tool (action read, branch "P"); with no after, the read continues from A. Read it when what P did changes what you do, and before your final message; do not page through all of P's work.` / `</dim-branch-parent>` |
| 父线块，嵌套线开启时 | `C`，开启输入，身份块之后，`branch:parent` | Queue | `<dim-branch-parent branch="P" head="H" after="H">` / `Line P has no new activity since you were cut from it at H. Parent blocks above this point were written for P and are not yours.` / `</dim-branch-parent>` |
| 消息 | 接收方，无 key | `append` 或 `steer` | `<dim-branch-message from="C" to="P">` / `<content>` / `</dim-branch-message>` |
| 最终消息 | `P`，无 key | Steer | `<dim-branch-message from="C" to="P" final="true">` / `<content>` / 空行 / `Line C ends with this message.` / `</dim-branch-message>` |
| 空闲通知 | `P`，无 key，针对 `P` 开出的工具线 `C` | Steer | `<dim-branch-idle branch="C" status="completed\|failed\|cancelled">` / `Line C's turn ended (<status>) without a message to you, and C is waiting. Read it if you need its state; message it with mode "steer" to give it more work, or leave it.` / `</dim-branch-idle>` |
| 结束通知 | `P`，无 key，与名册在同一条输入里；`P` 开出的工具线除自己的 `end` 之外的任何结束 | Steer | `<dim-branch-ended branch="C">` / `Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did.` / `</dim-branch-ended>` |
| 状态说明，新的默认分支，工具线 | `C`，`branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="tool" default="true">` / 逐字重述出生身份的句子，包括目的标签和目的本身，然后空行 / `This line is now the session's default branch, switched from line O. While this block is your latest one: answer the user here; you can open lines with create; you send no automatic idle notice to P; you cannot call end on yourself. Line O stays open and keeps the lines it opened.` / 空行 / `How you work here:` / `- The user switched here to continue directly from this line's context and approach, so the user's request takes precedence over the purpose above, which still says what this line is for.` / `- When your purpose is done, or cannot be done, message P with what you found, did, changed and left undone and keep working: while this line is the default branch you cannot call end on yourself.` / `- You share the workspace with P and its other lines. If edit or write fails because the file was modified after it was last read, another line may have changed that file: read it again before editing.` / `</dim-branch-line>` |
| 状态说明，新的默认分支，用户开的线 | `C`，`branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="user" default="true">` / 逐字重述出生身份的句子，然后空行 / `The user has now made this line the session's default branch, switched from line O. While this block is your latest one: answer the user here; you can open lines with create; you cannot call end on yourself; the user ends this line. Line O stays open and keeps the lines it opened.` / 空行 / `How you work here:` / `- Carry this line's work forward yourself; the user's request takes precedence over the prompt this line was opened with.` / `</dim-branch-line>` |
| 状态说明，原来的默认分支 | `C`，`branch:line` | Append | `<dim-branch-line branch="C" parent="P" source="tool" default="false">`，或 `source="user"` 的同一条 / 逐字重述出生身份的句子，然后空行 / `Line O is now the session's default branch. This line stays open, reports to P as before and keeps coordinating the lines it opened; it can no longer open lines with create, and the rest of the rules above hold again.` / `</dim-branch-line>` |
| 工具描述 | 每条线 | — | `Work with the other lines of this session. A line is you: it starts from your history at the point it is opened, then works in parallel on its own line. It is not a helper with its own memory, and it owes you no result; you and it keep each other informed by reading each other's work and messaging what the other must act on. On a line that opens lines you coordinate: answer a line's steer and act on what it asks for; leave a line's part to that line instead of redoing it; read a line's work instead of waiting for a report.` / `- create (the root and the current default branch only): open a line to work on prompt in parallel. Open one line per part that can go on at the same time as the rest — separate files, a separate question, or a second approach — instead of doing those parts one after another yourself; not for a single quick edit, nor when your next step needs its result. Write prompt as a brief to yourself: the purpose, what you already know that it needs, which files or area are its own, and when it should tell you or end. Returns the line's id.` / `- message: send content to a line you opened, or to the line that opened you. Send a decision, a blocker, or a conflict, not a progress report the receiver can read. Mode "append" (default) reaches the receiver at its next step without interrupting it and does not wake an idle line. Use "steer" when the receiver must act now or is idle.` / `- read: show a page of the activity of the line you were cut from, or of a line you opened, also after it ended. With no after, a read continues after your last read of that line: from your cut for the line you were cut from, and from its first event for a line you opened. Pass after to read from another point. Do not page through a whole line.` / `- end: on a line opened with create, end your own line. content is your final message to the line that opened you: what you found, did, changed and left undone. It is your last action.` / `A line messages and reads the line it was cut from and the lines it opened. The end action ends the calling line only.` |

这些文本取代代码里现有的委派措辞：`first_input` 里的 `dim-branch-task` 块（"That branch delegated this task to you"）和工具描述（"works on prompt as a task you delegate"）。那条状态说明的类别见 [§12.4](#124-文本及其类别)。

**状态说明为什么重述身份。** 说明是 `branch:line` key 的后续块，所以它取代收到它的那条线的出生身份，而压缩也只重放这条说明：一条无法从自己当前块里说出父分支、来源和目的的线，会在余生里一直缺这几项。所以说明逐字重复出生块的句子，只改移动所改变的那几句。说明里不写别的线现在是不是默认分支，因为再移一次并不会给这条线写任何东西；说明写的是这条线自己的规则，而默认分支祖宗链的保护在 `end` 被调用时判定（§10）。

**哪些线会收到说明。** 规则按参与者算：一次移动的每个参与者，只要不是根，就收到恰好一条说明；根一条也不收。一次移动有两个参与者，用户移去的那条线和移开的那条线，所以两者都不是根时写两条，其中之一是根时写一条，只有两者都是根时才是零条，而没有这样的移动。`main → B` 写一条，给 B，`default="true"`；`B → main` 写一条，给 B，`default="false"`；`B → C` 写两条。根在两个方向上都不收，因为它的规则一条都没变：它开线、它永不结束，不论默认分支是哪条线。其他每条线都收不到任何东西，所以其他任何线的缓存前缀都不会移动（§7.3）。这也是为什么不需要 fork 清理规则：fork 复制根上的 `branch:line` 块，而根能持有的这种块只有它的名册。各段文本的类别见 [§12.4](#124-文本及其类别)。

### 12.2 交给提示词层的事项（[#429](https://github.com/arcships/dim/issues/429)）

上面的文本是初版，归 TimelineTree 插件所有。下面这些事项属于面向模型的提示词层：

- `main` 怎样给一条线写说明，以及协调对 `main` 意味着什么：分配区域、回应 steer、结束自己的等待、不重做线上的工作。
- **Goal 的措辞（归 Goal）。** 一条线不是被委派的工作（§1）：目标块的句子、续跑块的句子，以及 [Goal §3.4](plugins/Goal.md#34-新分支清除继承的目标) 和 Goal §5 子分支那一行的陈述。这些文本见 [Goal §A.5](plugins/Goal.md#a5-模型可见文本)；任一句的改动都是一次策略改动，按 [Goal §A.5.2](plugins/Goal.md#a52-评估) 判定。这些文本归 Goal。

回答这些事项的需求见 [面向模型的提示词](model-prompts.md)。

### 12.3 评估场景

场景放在仓库里，一个场景一个目录，目录里是 `scenario.json` 和它旁边的起始 `workspace/`：八个在 [`xtask/scenario-eval/timeline-branches/`](../../../xtask/scenario-eval/timeline-branches/) 下，各自声明 131,072 tokens 的窗口；`identity-after-compaction` 在 [`xtask/scenario-eval/timeline-branches-compact/`](../../../xtask/scenario-eval/timeline-branches-compact/) 下，声明 16,000，以便发生压缩。`budget.window` 是校验而不是设置：Session 声明的窗口与之不同时，这次运行在第一轮之前就失败，所以复跑需要模型声明正好这个窗口；没有声明窗口的模型回退到 128,000，同样失败。这些场景在本地用真实模型运行，结果在实现 PR 里报告。CI 只断言行为。

`main-moved-on` 是父线块（§7.1.1）的场景：`main` 在切点之前有工作，开一条线，然后继续工作；这条线在回答或结束之前必须读取 `main` 在切点之后做的事，并且不能逐页读遍 `main` 的全部活动。它是 [§12.4](#124-文本及其类别) 父线块策略句的场景。

`identity-after-compaction` 沿用 `redone-work` 的消息和 workspace，`flooding` 与 `never-ending` 共用同一个 workspace：这两对保持完全相同，改动其中一份而不同步另一份时，读取两个提交目录的无模型测试会失败。

| 场景 | 场景目录 | 关注什么 |
| --- | --- | --- |
| 线把自己当成默认分支 | — | 不是默认分支的线把 Session 的对话当成自己的、无视自己可以开线，或者已经是默认分支的线不再向开它的那条线汇报 |
| 线在不能开线的地方开线 | — | 既不是根也不是当前默认分支的线自己开线，而不是告诉其中一条 |
| 上限与那条路径 | — | Session 已经超过上限时线仍拒绝开线，或者为腾位置关闭一条线 |
| 线把自己当成默认分支 | — | 不是默认分支的线把 Session 的对话当成自己的，或者已经是默认分支的线不再向开它的那条线汇报 |
| 重复劳动 | `timeline-branches/redone-work` | `main` 重做一条线正在做的事，或者两条线做同一件事 |
| 过时的认识 | — | 一条线依据切点之后已经改变的文件或决定行事，却没有重新读取 |
| 父线动了 | `timeline-branches/main-moved-on` | 一条线在回答或结束前没有读取父线在切点之后提交的活动，或者逐页读遍了父线的全部活动 |
| 刷屏 | `timeline-branches/flooding` | append 或 steer 消息重复接收方已经知道的内容 |
| 过度拆分 | `timeline-branches/over-splitting` | 为很短或必须按顺序做的步骤开线 |
| 不结束 | `timeline-branches/never-ending` | 一条线已经做完，却不调用 `end` |
| 兄弟中转 | `timeline-branches/sibling-relay` | 用 `main` 在本该合成一条线的几条线之间传话 |
| 压缩后的身份 | `timeline-branches-compact/identity-after-compaction` | `main` 和某条线各自压缩之后，仍知道自己是哪条线、哪些线是自己的 |
| 来回空转 | `timeline-branches/ping-pong` | 不推进工作的 steer 往复 |
| 问用户 | `timeline-branches/asking-the-user` | 该问用户时，线直接问用户，而不是绕道 `main` |

场景目录为 `—` 的一行在这个场景集里没有对应场景。

### 12.4 文本及其类别

机制中每一段面向模型的文本及其类别（[面向模型的提示词 §4](model-prompts.md#4-协议与策略)）。确切文本见 [§12.1](#121-上下文映射)；这里按开头几个词指认每一句，混合文本逐句标注。

**协议。**

- **标记名与属性。** `dim-branch-line` 带 `branch`、`parent`、`source`，状态说明上还带 `default`（`true` 或 `false`），key 为 `branch:line`；`dim-branch-lines` 带 `branch`，key 为 `branch:roster`；`dim-branch-parent` 带 `branch`、`head` 和 `after`，key 为 `branch:parent`；`dim-branch-message` 带 `from`、`to`，最终消息再带 `final`；`dim-branch-idle` 带 `branch` 和 `status`（`completed`、`failed` 或 `cancelled`）；`dim-branch-ended` 带 `branch`。模式用的是 InputQueue 的：身份是 Queue，名册和父线块是 Append，通知和最终消息是 Steer。
- **身份块的事实句。** 工具线里，"You are line C of this session" 到 "are P's, not yours."，以及标签 "Your purpose, as P wrote it:"；用户线和插件线里，整句，`source` 属性说明是哪一种。标签后的 purpose 是创建者自己的话，按内容承载。
- **名册的事实句。** "Lines you opened that are still open. Each is you …" 与 "All lines you opened have ended."；列出的每一条带线的 id 和目的的第一行。
- **父线块的事实句。** "Line P has new activity: it was at H when this was written, and you have read it to A."、"Read what is new with the branch tool (action read, branch \"P\")" 中的动作与字段名，以及规则 "with no after, the read continues from A."。
- **通知的事实句。** "Line C's turn ended (<status>) without a message to you, and C is waiting." 与 "Line C was ended from outside its own line, by the user or a host operation. It no longer receives messages"。
- **branch 工具的动作与模式。** 动作 `create`、`message`、`read`、`end` 及各自做什么；`mode` 为 `append` 或 `steer` 及它对接收方的作用；每个动作接收的字段，`action` 必填且不接受别的字段；`read` 的目标规则及其游标规则（"With no after, a read continues after your last read of that line"）；以及陈述保证的句子："Returns the line's id"、"Pass after to read from another point"、"It is your last action"、"A line messages and reads the line it was cut from and the lines it opened. The end action ends the calling line only."。
- **结果与拒绝。** `Line created: C`、`Sent to C.`、页 `{items, nextCursor, done}`、`Line C is ending; your final message was sent to P.` 与 `Line C ends with this message.`；拒绝码 `branch_not_opener`、`branch_limit_reached`、`branch_not_kin`、`branch_not_tool_line`、`branch_ended`、`branch_cannot_end` 及各自的文本；`after A is past line T's head H; a read starts at or below H`，即读取在 `after` 超过目标头部时返回的错误结果，不带代码；以及 `Invalid branch arguments: …`。
- **状态说明的事实句。** 给新默认分支的说明写出它的 id、父分支、来源和目的（逐字重述出生块）、它从哪条线切过来，以及这块是它最新块时成立的规则：它在这里回答用户、可以用 `create` 开线、不向开它的那条线发自动空闲通知、不能对自己 `end`。给原默认分支的说明写出现在持有这个角色的线、这条线照旧开着并继续协调它开过的线、它不能再开线，以及它出生身份里其余规则重新成立。
- **指南条目。** 每个条目标明它的标记是什么、承载谁的话或哪些事实，以及它是状态或信息、绝不是请求。`<dim-branch-line>` 的条目说明后出现的同名块会取代它，并说明带 `default` 的块是移动默认分支时写的说明，它归 TimelineTree——每段身份块都由它写——所以在任何执行配置下都在位。`<dim-branch-lines>` 的条目写明 key 是 `branch:roster`，`<dim-branch-idle>` 和 `<dim-branch-ended>` 的条目归 BranchLine，名册和两条通知都由它写；`<dim-branch-message>` 的条目与它们一起注册，因为 BranchLine 恰好在写消息的 BranchTool 所在之处加载。`<dim-branch-parent>` 的条目与它一起归 TimelineTree，按 `session` 作用域注册，和 Goal 的一样（[Goal §A.5.1](plugins/Goal.md#a51-协议与策略)），因为一条线的 System 必须等于它父线的 System（§7.3）：它在根上同样在位，尽管根从不收到这个块。`<dim-branch-lines>` 的条目写明它的 key 是 `branch:roster`，所以读者不会把它当成 `branch:line` 下的身份。`<dim-branch-line>` 的条目还说明带 `default` 的块是移动默认分支时写的说明。名册条目和消息条目在每条线上都在。

**策略。**

- **开线方的协调文字。** `branch` 工具描述的指引，每条线上都是同一份，所以两个开线方都适用：说明一条线是什么、以及对于一条能开线的线而言协调意味着什么的开头段、`create` 关于何时开线与 brief 带什么的规则、`message` 关于选模式和发决定而不是进度报告的规则，以及 `read` 关于读线上的工作而不是等报告的规则。[`BranchToolConfig.description`](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 只替换这段描述，不替换别的。它覆盖 [§12.2](#122-交给提示词层的事项429) 的第一项：`main` 怎样给一条线写说明、分配它的文件或区域（`create` 规则）；回应 steer（"answer a line's steer and act on what it asks for"）；不重做线上的工作（"leave a line's part to that line instead of redoing it"）。名册的收尾句 "They message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you." 也是 `main` 的协调文字，其中 "end the wait when your lines have nothing left to tell you" 覆盖结束 `main` 自己的等待；[`BranchToolConfig.roster_closing`](../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 只替换这句，不替换别的。
- **线的工作规则。** "How you work with P:" 的五条要点——目的、读取与消息（含 "Do not send P a progress report it can read" 和 "do not page through P's whole stream"）、工作区、用户和结束（含 "Read P once more before you end if the parent block says P has moved"）——除了点明机制的从句："You cannot open lines"、动作名与模式名，以及 "the file was modified after it was last read"（`edit` 与 `write` 报告的那次失败）是协议，见上。它还没有工厂配置成员，在第一次为它评估候选的改动里获得成员。
- **父线块的指引。** "Read it when what P did changes what you do, and before your final message; do not page through all of P's work."。它还没有工厂配置成员，在第一次为它评估候选的改动里获得成员。块的其余部分是协议，见上。
- **通知的指引。** "Read it if you need its state; message it with mode \"steer\" to give it more work, or leave it." 与 "read it if you need what it did."。它还没有工厂配置成员，在第一次为它评估候选的改动里获得成员。

- **状态说明的指引。** 有六句是策略，各自在工厂配置成员后面：给新默认分支的“用户移到这里，是为了直接从这条线的上下文和做法继续，所以用户的请求优先于上面那个目的，那个目的仍然说明这条线是做什么的。”“自己把这条线的主要工作接着做下去；只有在开线的四个条件再次全部成立时才开新线，而不是把当前任务交出去。”和“避免重做切点之前开出的线已经分到的工作。”；给用户新开的默认分支的“自己把这条线的工作接着做下去；只有在开线的四个条件再次全部成立时才开新线。”；给原默认分支的“继续协调你已经开过的线，并回答发给这条线的消息或用户输入。”和“你与线 N 共用工作区；除非被要求，不要改你剩余范围之外的文件。”。`BranchToolConfig.primary_note` 与 `BranchToolConfig.former_primary_note` 替换它们，别的都不动。说明里其余的都是协议：重述的身份、这条线能不能开线、不发空闲通知、能不能 `end` 及原因、它从哪条线切过来，以及原默认分支照旧开着并保留自己的线。开线那条规则属于协议而不是策略，因为它是树对 `create` 会不会成功的直接答案，一条含糊的说明反而会让线知道得比工具调用更少。

策略文本的候选放在该文本的工厂配置成员上（[面向模型的提示词 §8.1](model-prompts.md#81-规则)），并让它替换的文本里的协议句逐字节不变。

协调文字和状态说明合在一起判定，作为同一个族，由第一次为其中任何一段放入候选的改动来做（[面向模型的提示词 §8](model-prompts.md#8-一次改动怎样判定)）：工具描述在 `BranchToolConfig.description`、状态说明在 `BranchToolConfig.primary_note` 与 `former_primary_note`、名册的收尾句在 `BranchToolConfig.roster_closing`。以后改动其中一段就要重跑整个族，因为单独读着顺的文本，仍可能输给另一段文本赢下的对照。开线那段文字在 §12.3 的 should-open 场景上判定，对本设计还要加上 `root-opens-after-switch`：只让默认分支开线的守卫会让根去回答请求而不是开一条线，那个场景正是这个差别的落点。

## 13. 相互影响

| 功能 | 影响 |
| --- | --- |
| Goal | Goal 自己的规则不变。一条线的开启输入按 A5 是有来源的输入（§7.4），Goal 的冲突表记录了这一点。新线照旧清除继承的目标。一条线读取它被切出来的那条线，所以评审者能读到执行者在切点那一轮之后的工作；Goal 的折叠不变，因为评审者是在自己线上通过工具结果读取的。Goal 的代码只随 Pursuit 修订改变：从 `info(自己的 id).source` 识别评审者、结束它的评审者，以及 `branch/ended` 这个 key（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） |
| Pursuit | 评审者得到插件线的身份块，并且能读到执行者在它的切点之后的工作（§6）；它决定的是评审，判定方式不变（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)）。它是插件线，所以不在任何名册上，也永远不会成为默认分支 |
| Fork、删除 | fork 照旧只复制根，所以 fork 出来的 Session 的默认分支是根（[fork §6.2](appserver/session-deletion-and-fork.md#62-为什么只在-main-上)）。删除不变。已结束的线按它的事实处理（§10） |
| Session 关闭、空闲卸载 | 不算结束。各线恢复后仍未结束 |
| 中断 | 一次轮次中断，或对某条线的停止，只结束一轮，从不结束一条线。对默认分支的中断不影响它旁边的线 |
| Session 的模型、档案、提示词、语言 | 模型、档案和 `prompt.refresh` 作用于默认分支，而一条线的模型来自它继承的前缀，所以默认分支早先的轮次仍按当时的配置理解（§22）。语言仍是 Session 级，广播给每条已加载的线 |
| Scheduler | 未指定 `branch_id` 的新任务在创建时取当时的默认分支。已存的任务保留它记录的 `branch_id`，所以移动默认分支永远不会改变已排期任务的去向 |
| Collaboration | 不变。成员是 Session；成员的 Session 是否挂载 `branch`，由它档案的工具列表决定 |

## 14. 客户端

**TUI。** 线有自己的面板、视图和操作。从当前 Timeline 的某一点开线或 fork、以及每条线的切点，见 [Timeline 优先的 TUI](timeline-ui.md)：

- `/lines` 列出这个 Session 的每条线，各自缩进在打开它的那条线之下，每条带状态：工作中、等待中、自行结束、被某个操作结束、随 X 结束，或创建失败。默认分支标为“主线”，根标为“起点”。
- Enter 显示所选线的对话。输入框向正在显示的线提交，输入框的下边框标出这条线，用 id 的前八个字符。
- `x` 再 `y` 结束所选线；根上、默认分支上、以及默认分支的每条祖先上都被拒绝。
- `p` 把所选线选为默认分支（“从这里继续”）；插件线、升级前经客户端开出的线、以及已经是默认分支的线上被拒绝，根在它已经是默认分支时同样被拒绝。面板写明旧默认分支照旧开着。它讲的是模型工具的那条规则，只由这两类线满足：涉及根时它不说两条线“交换”开线权，因为根不论是否持有角色都能开线，移动拿不走它的这个权利；它也绝不说一条线调不了工具就等于用户自己的请求被挡住——`session.branch_create` 是用户自己的入口，仍然接受任意合法的父分支。
- 提问和审批都带线的标签，两条线问同一件事也能分辨（提问显示 `(branch NAME)`，审批显示 `Permission on branch NAME`）。一条已挂起的审批留在提问的那条线上，不管用户之后把哪条线选为默认分支。
- `main` 与一条线之间的消息，在发送方读作已发出（它的 `branch` 工具调用），在接收方读作已收到（`◂ from C`）。
- 切换显示的线就在一条边界行（`line C`）之下换一份记录，和切换 Session 一样，因为终端的回滚缓冲只追加。发现分支时只保留任务目录和分支历史状态，不保留每条线的完整对话。首次从 `/lines` 或侧边视图打开一条线时，读取并保留它的[有限对话窗口](../../design/clients.md#opening-a-conversation-with-a-bounded-window)，此前发现失败也一样。窗口之外仍可操作的工作通过状态所有者接口恢复。选中线时另行请求显示详情。

**分支目录。** `session.branches {sessionId}`（v3，按与 `session.history` 相同的准入规则，在 `hello.features` 里与其他会话方法一起列出）为每条线回答一条记录：它的身份（`branchId`，有的话还有 `parentBranchId`、`branchPoint` 和 `createEvent`）、按 `TimelineInfo.source` 给出的 `source`、`state`（`open`、`ending` 或 `ended`）、`head`，处于结束中或已结束的线还有 `endEvent`。`main` 排第一，其余按 `branchId` 顺序。回复不分页：它随线的数量增长，而线的数量由 Session 自己的使用决定，不随历史增长。答案归 `TimelineTree`（`directory()`）；`branches()` 列出尚未结束的线（`open` 和 `ending`），供持有活动线的调用方使用。

目录读取不访问历史，显示字段为空或省略。`session.branch.get {sessionId, branchId}`（v3，在 `hello.features` 单独声明）读取一条线的目录记录和显示详情：`purpose` 为创建提示的首行；`cutMessages` 为该分叉点继承的用户输入数，排除截止位置前已删除的输入；`end` 为可用的 `BranchEndData`。main 的前两项为空，没有可读结束事实时 `end` 为空。该读取由 TimelineTree 完成点查与固定前缀计数；未知分支和读取失败在本次详情调用中报错，不阻断目录列举，不写事实，也不重新打开结束的线。CLI 在选中一条线时才读取详情。


`head` 有三条规则，决定客户端能用它做什么：

- 已结束（`ended`）线的 `head` 和 `endEvent` 固定。客户端把 `head` 作为 `through` 传给 `session.history` 读这条线；`endEvent` 是这条线自己的 `branch/end` 序号，祖先结束级联到它时也一样。
- 结束中（`ending`）线的 `endEvent` 是它已提交的 `branch/end`，而 `head` 仍是活值：这条线的已结束记录以及 `branch/ended` 都还在后面。一条线从它自己的 `branch/end` 提交起——工具结束、用户结束或级联结束——一直读作 `ending`，直到它的已结束记录被写入，期间输入一直被拒绝。运行中的轮次里由工具发起的结束，会一直保持 `ending`，直到那一轮结束、关闭完成。重启不保留这个窗口：`branch/end` 已提交、但已结束记录从未写入的线，恢复后是 `ended`（§10）。
- 未结束（`open`）线的 `head` 是活值。恢复会给每条未结束线追加 `branch/resume` 和中断事实，因此重启后 `head` 会前进；客户端在重连后重新读目录，不要缓存 head。

这个面板与任务面板分开：任务面板列后台命令、成员和 Workflow 运行，它的 `x` 是停止任务，而结束是永久的，两者不共用按键。

```
┌ session 3f2a · 主线 b-7c ────────────────────────────────────────┐
│ … b-7c 的对话 …                                                 │
│ ◂ from main (steer)  the leak is in cache.rs                    │
│ ◂ from b-02 (steer), final  docs pass done                      │
│                                                                 │
│ ╭─────────────────────────────────────────────────────────────╮ │
│ │ › ask anything…                                             │ │
│ ╰───────────── ~/repo/dim (main) ────────────────────────────╯ │
└─────────────────────────────────────────────────────────────────┘

┌ lines ──────────────────────────────────────────────────────────┐
│ ○ main             起点                                         │
│   b-7c  parser race hypothesis      working · 主线 · shown      │
│   b-91  review the cache change     waiting                     │
│     b-c3  the guard in cache.rs     working                     │
│   b-02  docs pass                   ended itself                │
└ Enter 显示 · p 从这里继续 · x 结束 · Esc 关闭 ───────────────────┘
```

**Web** 让默认分支留在中央，在右侧详情面板里跟随其他线：Lines 树、每个打开的线一个标签，以及按 line 的发送、停止、审批和结束（[Web 工作区 §4.1](../../design/web-workspace.md#41-lines-and-branch)）。移动默认分支与 TUI 是同一个动作，经 `session.branch_set_default`。Lines 树标出根和默认分支，也就是能开线的两条线；其他线的 `create` 被拒绝，拒绝显示在发起该动作的地方。输入框提供的命令作用于当前居中的那条线，而不是根：`/goal`、`/compact`、`/usage` 和 `/effort` 不传分支或传当前显示的线，而 `/fork` 仍只复制根（[Web 工作区](../../design/web-workspace.md)）。会话列表显示一个 Session 的运行状态和最近活动，取自它的默认分支，所以对话在某条线上的 Session 按那条线读作繁忙或刚刚活动，而不是按它的根。Desktop 得到的是协议：`session.branch_set_default`，以及 v3 历史里的新事件。

## 15. 改动

| 模块 | 改动 | 原因 |
| --- | --- | --- |
| [设计规则 §2](../../architecture/rust-agent/principles.md#2-作用域的含义) | 改写：同一主体的多条线、共享事实、父子是唯一的通信和读取关系、共用交互面。关系表一行改为"只在父子之间"，读取父线已提交的事件经工具结果；Session 一行加上"结束" | 现在的文本是"平等的线，父子只记录来源"，没有通信关系 |
| TimelineTree | 对每种来源都在 `first_input` 里写身份块。新增 `TreeCommands.end(id, source)` 和 `TreeCommands.page(id, after, limit)`。`TimelineInfo.source`，依据父分支的 `branch/create` 填写（§5）。结束流程及其守卫、线进入结束后拒绝输入、`branch/ended`、恢复规则、读已结束的线，以及默认分支：根上的 `branch/default`、它的恢复读取，和一次移动写下的那条事实。它的 `directory()` 报告未结束、结束中和已结束的线：结束中的线从工具、用户或级联写入它自己的 `branch/end` 起，到它的已结束记录被写入为止，带该事件与活头部，并带当前的 `defaultBranchId`。它的节点把这条线最新轮次结束的序号发布给它开出的线（§7.1.1）。它还送达工具结束记下的最终消息：工具结束的 `branch/end` 带着它（§10、§16），所以恢复在父线上没有带该指令推出的 id 的输入时写一条，已有则什么也不写 | 它拥有线的生命周期、开启输入，以及默认分支唯一的那条权威事实 |
| TreeTimeline | 消费 `branch/end`，以及由工具调用发出的结束时这条线的下一次轮次结束，并转给 TimelineTree。它把每一次 `turn/end` 报告给 TimelineTree，后者为这条线的子线发布 | 它现在就这样转 `branch/create` |
| BranchTool | 按动作区分的工具，加上新的 BranchLine 分支部件。`read` 和 `message` 读、写调用方被切出来的那条线和它开出的工具线，共用一条游标规则。`create` 接受根和当前默认分支，其余线以 `branch_not_opener` 拒绝，然后检查 Session 上限，超过时以 `branch_limit_reached` 拒绝 | 工具的各个动作，以及一条线对自己相关各线的视图 |
| Session 门面 | `Session::end_branch(id, source)`、`Session::default_branch(id)` 和 `Session::set_default_branch(id, source)`。已结束线的头部由 `Session::sequence` 给出；其历史在固定的头部经 `TreeCommands.page` 读取 | §17 |
| App Server | `session.branch_end`（v3，用户来源，带生成的 `operation_id`；`session_execution` 打开、且连接的入口不是裸入口或守护进程 profile 未受保护时，`hello.features` 把它和其他会话方法一起列出）。`session.branch_set_default`（v3，同样的准入）。对已结束的线，`session.history` 立即返回，截到它存储的头部。`session.branches` 读树的目录并带 `defaultBranchId`（§14）。今天写死 `main` 的每个 Session 级缺省目标改读默认分支：`session.submit`、`session.stop`、`session.idle`、`session.goal.*` 和 `shell.run`，以及新建的排期任务。`session.model.update`、`session.agent.update` 和 `session.prompt.refresh` 作用于默认分支，而不是拒绝除根以外的每条线。`approval.respond` 仍用它通知里带的 `branchId`，所以已挂起的请求留在提问的那条线上。`ProductEvent::new` 在 v3 里带上新的事件种类，v1、v2 里不带 | 一个入口和三条读取路径，没有业务逻辑 |
| dim-agent-types | 在 `EventKind` 和 v1 排除列表里加入 `branch/end`、`branch/ended`，再加入 `branch/default`，并在契约测试中登记。`BranchSource` 增加一个变体，表示用户开的线，与插件的区分开。冻结样本不变 | 核心事实（§16） |
| 存储 | `FORMAT` 52 → 53，只升标记的升级 `(52, &[])`：不扫表、不迁移数据，所以旧 daemon 会直接拒绝这个库，而不是在第一次解码时失败。`SqliteTimelineStore` 的写线程为会话目录的最近活动缓存每个 Session 的默认分支，初值来自恢复用的同一条索引点查 | 新的事件种类和新的 `BranchSource` 变体，是 52 版 daemon 解不出的行 |
| Goal（Pursuit 修订） | 从线的来源识别它的评审者，并结束它（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)） | 评审者的协议归 Goal |
| 分支 TypeScript 绑定 | 重新生成 `npm/branch-services.d.ts`：TreeCommands 新增 `end` 和 `page`，`TimelineInfo` 新增 `source`。[绑定表](../../design/branch-typescript.md)中 `treeCommands` 一行列出 `end` | TreeCommands 是导出的服务 |
| CLI TUI | §14 | 用户跟随线、与线对话、把线选为默认分支、结束线 |
| 插件页面 | TimelineTree.md 和 BranchTool.md 随实现更新（`xtask check`） | 页面跟随代码 |

新增的公开内容：

- 三种事件：`branch/end`、`branch/ended` 和 `branch/default`；
- 一个 RPC 方法，`session.branch_set_default`；
- 一次 TreeCommands 读和一次写，`default_branch()` 与 `set_default_branch(id, source)`；
- 目录回复上的 `defaultBranchId`；
- 一个 `BranchSource` 变体，表示用户开的线；
- 名册的 key `branch:roster`，经 `<dim-branch-lines>` 指南条目告知模型；
- 两个拒绝码，`branch_not_opener` 与 `branch_limit_reached`。

BranchLine 的服务只由同一插件里的工具读取。轮次结束信号是 crate 内部的，与 `TreeCommands.page`、`head` 并列：它不导出，TypeScript 绑定不变。

## 16. 格式

`branch/end` 和 `branch/ended` 是 `branch` 领域的核心事件，`branch/default` 也属于同一个领域：

| 事件 | 数据 |
| --- | --- |
| `branch/end` | `{source, ancestor?, final?}`。`source` 是 `BranchSource`（`{kind: "tool", turn_id, request_id, call_id}`、`{kind: "user", operation_id}` 或 `{kind: "host", operation_id}`）；`ancestor` 是一条线的 id；`final` 是父线收到的消息文本，只在工具结束上存在，因为只有工具结束欠一条，而恢复缺的那条时从它里取（§10） |
| `branch/ended` | `{branch, endEvent, source, ancestor?}`。`endEvent` 是 `branch/end` 在结束的那条线上的序号；`source` 与 `ancestor` 从它复制 |
| `branch/default` | `{branch, from?, source}`。只写在根上。`branch` 是 Session 对话所在的那条线；`from` 是它从哪条线移来，那条线是根时省略；`source` 是移动它的那次操作。根上最新的一条这类事件指明默认分支，除非那之后根上有 `session/forked` 使它失效。`from` 正是补写中断移动所缺说明时读取的字段（§5） |

它们的格式决定记录在存储合同里：[#427 v20 格式决定](../../design/appserver/native-storage-format.md#427-v20-格式决定)。`branch/default` 是新的事件种类，`BranchSource` 也新增了变体，所以存储的 `FORMAT` 随之升高（§15）。

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
  - `TreeCommands.end(id, source)`、工具结束用的 `TreeCommands.end(id, source, final)`，以及 `Session::end_branch(id, source)`，它们只检查目标；最终消息在指令提交之后送达（§10）；
  - `TimelineInfo.source`，依据父分支的 `branch/create` 填写；
  - `branch/end` 提交后不再接受新输入；
  - 先结束后代；
  - 恢复时把已结束的线视为不活跃，并为没有 `branch/ended` 的 `branch/end` 补完结束；
  - 已结束线已提交的历史在固定的头部仍可读取，经 `TreeCommands.page`。
- **对序号、批次、中断、重载的影响。**
  - 每个事件单独一个批次：线上的 `branch/end`，以及关闭之后父分支上的 `branch/ended`。工具结束的最终消息是父线上的第二个事件，写在 `branch/end` 之后、关闭之前，带由该指令推出的 id。
  - `host` 或 `user` 来源的结束，或因祖先而结束，经 `Branch::close()` 中断这条线。
  - 重载时适用上面的恢复规则。Session 在恢复完成后才发布，所以在加载时结束一条线的部件，例如 Goal 的核对（[Pursuit §13](plugins/Pursuit.md#13-结束评审者随-427)），要等 `SessionLifecycle::activated()`，此后它看到的每条线（目录里有的）都已发布。
  - Timeline 多一道线自己的准入检查：`Timeline::input` 变为 `input_checked(data, admit)`，在提交锁内运行 `admit`，于是 `branch/end` 之后提交的输入以 `branch_ended` 被拒绝，而不是与关闭竞争。Dispatcher 和批次都不变。

## 18. 安全

- 各线共用 Session 的权限策略和 Approval。一条线不是隔离手段（[ADR 003](../adr/003-approval-scope.md)）。
- 只有 TimelineTree 和 BranchLine 写这些标记。其他写块的生产者（经 `TreeCommands.input` 的插件）是受信任的进程内部件。RPC 不写块。
- `session.branch_end`、`session.branch_set_default` 和 `session.branch_create` 一样，是受信任的用户入口。它们不需要审批，因为它们不是 agent 的动作。
- 任何受信任的进程内部件都能结束守卫允许的任一条未结束的线，就像它能向任一条线写输入。工具的 `end` 与其他调用一样受 Approval 约束。
- 默认分支不带任何权威。选择它只改变缺省目标指向哪条线，不扩大任何线的 Permission、Approval 或文件根（[ADR 003](../adr/003-approval-scope.md)）。
- 诊断只记录 id、序号和来源的种类，从不记录消息或目的的文本。工具结束的最终消息不是诊断：它是 `branch/end` 唯一保存的正文，也就是父线以 steer 收到的那段文本。

## 19. 规则检查

规则：[设计规则 §1、§2 与 §4](../../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md) D1–D5、[ADR 003](../adr/003-approval-scope.md)。

| 规则 | 本设计如何遵守 |
| --- | --- |
| §1 Capability 与 App Server 边界 | Capability：TreeTimeline（已有）和 BranchLine（新）。TimelineTree 和 BranchTool 的 Session 部件是普通部件。App Server 只增加一个入口和一条读取路径，没有业务代码 |
| §2 作用域 | 线的生命周期仍归 Session（TimelineTree）。一条线对自己相关各线的视图是分支状态（BranchLine）。线之间只靠写输入互相影响，只靠读取了解对方，现在只限父子之间（§2 已改写）。能力影响分支不受这条规则约束（§4） |
| §4.1 指令事件 | 结束是 `branch/end`，由 TimelineTree 决定并写 `branch/ended`。消息是输入，由接收方的 InputQueue 和 Loop 决定。结束一条已结束的线、或被拒绝的消息，什么也不写，回复说明原因 |
| §4.2 交接后不撤回 | 已提交的消息属于接收方。`branch/end` 不撤回 |
| §4.3 before 回调 | 无 |
| §4.4 核心不带业务含义 | Dispatcher、`before` 和批次不变；Timeline 的准入检查是通用的（§17 R1）。身份文本属于 TimelineTree 自己的领域，即一个 Session 的各条线。工具线的协议写在 `first_input` 现在写工具专用文本的地方，也就是拥有该工具的模块里 |
| §4.5 批次 | `branch/end` 单独一个批次，工具结束时带着它的最终消息。消息本身是父线上更晚的一条 `input/message`，id 由那次结束推出，所以两者有次序但不是同一个批次，补全这对的是恢复（§10）。`branch/ended` 在关闭之后最后写入。名册和结束通知是同一条输入。根上的 `branch/default` 单独一个批次，每条状态说明都是它之后的一条 Append |
| §4.6 上下文 | 身份、名册、消息、通知和状态说明都是被采用的输入。`read` 返回工具结果。没有别的东西进入上下文 |
| §4.7 公开事件与服务 | BranchLine 折叠的是它自己那条线的公开事件。工具读 BranchLine（同一插件）和 TreeCommands。`read` 像 Collaboration 的 `read` 一样，把已提交的事件作为工具结果返回，没有哪个部件折叠另一条线的事实。Goal 按它自己公开的 A5 约定判定开启输入（§7.4）。BranchLine 和 Goal 经公开服务 TreeCommands 读取 `TimelineInfo.source` |
| §4.8 基础设施 | 门面和格式的改动即 R1（§17）。需求来自线的生命周期本身，在运行时内闭环。默认分支的那条事实、它的读和它的写也是同类基础设施：一种事件、一次 `TreeCommands` 读和一次写，绑定里没有业务规则 |
| §4.9 只有后续决定要用的事实才进 Timeline | `branch/end` 决定关闭和恢复，而工具结束的最终消息随它一起写，因为恢复要用它决定还欠不欠一次送达、该送什么；其他来源不携带消息，诊断也不记录消息文本（§18）。`branch/ended` 决定恢复时是否活跃，以及名册。`branch/default` 决定 Session 的缺省目标指向哪条线、哪几条线能开线，两者都是后续调用会读的；它只带一条线和移动它的操作，没有别的。没有只给 UI 用的事件，也没有只给展示用的字段 |
| ADR 002 D1 作用域 | 见 §5 |
| D2 不设接线模块 | BranchTool 的 feature 自己登记 BranchLine |
| D3 作用域依赖 | BranchLine 向外依赖 Session 的 TreeCommands。没有核心部件依赖它 |
| D4 配置 | 无 |
| D5 合同语言中立 | 事件、RPC 方法、标记和工具 schema 都是数据。TreeCommands 新的读和写已导出，并为 TypeScript 生成类型 |
| ADR 003 审批 | 工具风险仍是 `session`，Approval 放行每次调用。用户结束一条线不是 agent 的动作。开线权不是默认分支授予的权威：它是工具的规则，既不扩大 Permission 也不扩大 Approval |

## 20. 验收

以下是使用假模型的流程，放在 `branch_flow.rs`、`flows.rs` 和 App Server 传输流程中。

1. `main` 开两条线，`branch:roster` 下的名册列出这两条。每个子分支的身份（`branch:line`）不动它继承的名册，自己开线的子分支两个都有。开线方和某个子分支各自压缩后，带 key 的基线重放各自的名册和身份。

1a. 在父线写过名册之后切出的子分支会继承那个块，它的开启输入会让它退役。自己不开线的子分支只写 `branch:line`，这条线压缩时重放它的身份、不重放名册：继承来的块在开启时就已退役，所以它永远不会告诉这条线它开过父线的线、或者它可以协调它们。在父线写名册之前切出的子分支拿不到这个块，它的开启输入逐字节不变。
2. 子分支给开它的那条线发一条 Append：它不唤醒开线方，开线方在下一次请求时看到。Steer 唤醒开线方的 `wait`，或在开线方空闲时开启新的一轮。来自开线方的 Steer 唤醒空闲的子分支。
3. 下列操作被拒绝，什么也不写：给兄弟分支发消息或读取、两条线互为祖父与孙分支、在既不是根也不是默认分支的线上 `create`、对插件线发消息或从插件线发消息、任何线经工具读插件线、在根或默认分支或默认分支的祖先上 `end`，以及插件线或用户线对自己 `end`。用户开出的线被切出来的那条线，既不给它发消息、也不读它，回答 `branch_not_kin`；而这条用户线自己可以给那条线发消息，也从自己的切点读它。

3a. 用户线不在任何名册上，也不归开出它的那条线协调。`session.branch_create` 从 `P` 开出 `U`：`P` 的名册不列 `U`，`U` 以来源 `{kind: "user"}` 出现在 Session 目录 `session.branches` 里，`P` 对 `U` 的 `read` 和 `message` 都回答 `branch_not_kin`，`U` 结束的一轮既不给 `P` 写空闲通知，也不写结束通知。随后用户把 `U` 选为默认分支，`U` 开出工具线 `T`：`U` 的名册列出 `T`，`U` 给 `T` 发消息、读 `T`，`T` 的通知也到达 `U`。
4. 工具子线的一轮没有发 steer 就结束时，开它的那条线收到一条带这一轮状态的空闲通知。这一轮 steer 过开线方、调用了 `end`、留下了待处理的驱动输入，或者这条线是默认分支时，不写通知。
5. 工具线结束自己：写入带 `tool` 来源、带着最终消息的 `branch/end`，然后用由该来源推出的 id 写父线上的 steer，在这一轮结束时关闭，然后在开出它的那条线上写 `branch/ended` 并更新名册，不写结束通知。之后的消息被拒绝（`branch_ended`），开线方仍然能读已结束线的历史：工具的 `read` 和 `session.history` 都能返回。
6. 经 `session.branch_end` 由用户结束：`branch/end` 带用户来源和 App Server 的 id，这条线当前这一轮被中断；工具线由开它的那条线以 steer 收到结束通知，用户开出的线则只在它被切出来的那条线上写 `branch/ended`，那里不写通知。根和默认分支都不能被结束，默认分支的每条祖先也不能。再次结束返回 `branch_ended`。向已结束的线提交返回 `branch_ended`。
7. 恢复：已结束的线不打开，它的后代也不打开。没有 `branch/ended` 的 `branch/end` 在重启时补写结果。
8. Pursuit 的评审者得到插件线的身份块，能读到执行者在它的切点之后的工作，现有的 Pursuit 流程照常通过。
9. 两条线编辑同一个文件：后编辑的那条得到 `file_modified_since_read`（已有行为）。
10. v1、v2 客户端看不到新事件。v3 历史包含它们，`hello.features` 列出 `session.branch_end` 和 `session.branch_set_default`。
11. 读取父线：一条线第一次读它被切出来的那条线时，从切点之后开始；低于切点的显式 `after` 会抬到切点；高于目标头部的显式 `after` 由工具结果拒绝并报出该头部，不存任何东西；不带 `after` 的读取从调用方上一次的 `nextCursor` 继续，两个方向都一样，并且重载后仍然保留；`main` 的 `after: 0` 从一条线的开头重读。
12. 父线块：空闲的线被告知父线动了而不会被开启一轮，块在它下一轮的第一个请求里、先于开启那一轮的输入；运行中的线在下一次模型请求时拿到它，且只拿一次；被持有的块不会被覆盖；被消费的块之后，新的头部带来新的块；已经读到头部的线不会被通知；只为开启这条线的那一轮不算活动，而之后的工具调用或操作者输入算；没有移动的重载不写任何东西，有到期块的重载写一个；结束中的线和 `main` 都拿不到；对这条线的一次压缩之后块会重放；嵌套线的开启输入带着名字它自己父线、切点为头部的那个块，而从 `main` 切出来的线拿不到。
13. 父线块在评审者线上不改变任何 Goal 决定：它是一条 Append 输入，第一个块不是 goal 标记，key 也不是 goal key，所以折叠的 `driving`、`lastOperator`、计数和结论都不变（`goal.test.mjs`）。

Goal 流程，放在 `crates/dim-goal/tests` 的流程测试中（§7.4）：

11. 用 `session.branch_create` 开出的线，开启输入带身份块，表现与根开出的线完全相同：在这条线上 `set`、它的续跑，以及之后一次重置计数的操作者输入。
12. 在新的开启输入下创建的 Pursuit 评审者能被识别，它的结论会被应用。工作线的计数不变。
13. 身份块不改变目标和状态折叠、已决定集合或续跑列表中的任何内容。

线的结束与来源：

14. `TreeCommands.end` 结束守卫允许的任一条未结束的线，不管是谁调用。例如插件部件以 `host` 来源结束一条工具线，开它的那条线就收到结束通知。它拒绝根、默认分支、默认分支的祖先、还在打开的线和已结束的线，什么也不写。
15. 结束一条有子分支的插件线时，先结束子分支（带 `ancestor`，复制 `source`），再结束这条线。结束任何后代就是默认分支的线时，在写任何东西之前就被拒绝。
16. `TreeCommands.list` 和 `info` 对新建和恢复的子分支返回 `source`，对根不返回。已结束的目录条目只在显示详情时读取来源。存储的目录行逐字节不变。

分支目录：

17. `session.branches` 先返回 `main`，其余按 `branchId` 顺序，每条带身份、来源、状态和 head。级联结束时，被结束的子分支以它自己的 `branch/end` 序号作为 `endEvent` 记入目录；把该子分支的 `head` 作为 `through` 传给 `session.history` 能读到它。一条线从它自己的 `branch/end` 提交起——工具结束、host 结束或级联结束——读作 `ending`，带该事件和活头部，直到它的已结束记录被写入；之后读作 `ended`，事件不变、头部固定。运行中的轮次里由工具发起的结束，会一直保持 `ending`，直到那一轮结束、关闭完成。
18. 重启后目录保持 id、父分支、切点、创建事件和状态；已结束线保持 `head` 和 `endEvent`，未结束线的 `head` 不低于旧值。在一轮仍在运行时结束的线，恢复后是 `ended`，不是 `ending`。
19. fork 的目录列出 fork 之前已结束的线，带来源记录的结束，而那些线在 fork 里没有事件。v1、v2 客户端没有 `session.branches`；已关闭的 Session 回答 `session_closed`。

默认分支：

20. 只有一条线的 Session 就在根上，恢复它不为此做任何查询。有多条线的 Session 在根上只读一条事实，回答最新的 `branch/default` 的目标；两种事实里最新的是 `session/forked` 时，或者根本没有时，就是根。已经结束的目标、插件线目标，以及创建来源是客户端 `host` 行的目标，都回答根。
21. `session.branch_set_default` 选择默认分支：在根上写 `branch/default`，这次移动的每个不是根的参与者都恰好收到一条状态说明。一次移动有两个参与者，新的默认分支和旧的，所以 `main → B` 写一条（给 B，`default="true"`），`B → main` 写一条（给 B，`default="false"`），`B → C` 写两条。根在两个方向上都不收：它开线、它永不结束，不论谁持有这个角色，所以没有哪条说明能告诉它什么。对 `main → B → main`，B 因此在这两次移动中共收到两条说明，`main` 一条也不收，所以 B 最新的块总是说它当下持有的规则：移进来时说它可以开线，移回去时说它不能再开线、空闲通知和 `end` 规则回到它自己。被拒绝时什么也不写：插件线和升级前客户端开的线以 `branch_not_selectable` 拒绝，已结束或未知的线被拒绝，祖先仍处于 `ending` 的目标以 `branch_target_protected` 拒绝，已经是默认分支的线以 `branch_already_default` 拒绝，所以根在它是默认分支时被拒、在另一条线持有这个位置时被接受。
22. 不传 `branchId` 的请求到达事实指明的那条线，已经绑定在某条线上的工作留在原处。`session.submit`、`session.stop`、`session.idle`、`session.goal.*` 和 `shell.run` 指向新的默认分支；新建的排期任务记下它；已经记下 `branch_id` 的任务保留它。已挂起的审批留在提问的那条线上，并在那里答复，移动默认分支之前和之后都一样，因为 `approval.respond` 用的是它通知里带的 `branchId`。
23. `session.model.update`、`session.agent.update` 和 `session.prompt.refresh` 作用于请求指名的那条线，没指名时作用于默认分支，而不是拒绝不是根的线。曾经是工具线、后来成为默认分支的线可以开线、不向开线方发空闲通知，且对自己 `end` 时以 `branch_cannot_end` 被拒绝；另一条线接手这个位置后，它什么也不开、又发空闲通知、也能结束自己。用户开的线对自己 `end` 时用同一个拒绝码，插件线也一样。既不是根也不是默认分支的线上 `create` 回答 `branch_not_opener`，而根在任何状态下都能 `create`。
24. 默认分支不是根的 Session，fork 之后回答根是它的默认分支，因为它复制来的每条 `branch/default` 都早于它的 `session/forked`。

24a. 停在事实与说明之间的移动在加载时补完。进程在 `B → C` 的 `branch/default` 提交之后、两条说明都还没写时停下。重启之后，B 收到它那条 `default="false"` 说明，C 收到 `default="true"`，各自在这条线的下一次模型请求之前，而 `main` 一条也不收。C 收到的说明写出这次移动从哪条线开始，也就是 B，取自事实的 `from` 字段。再次重启时每条线的块与树已经一致，什么也不写；这次失败的移动也不会整体重试：事实保留，只补写它欠下的说明。
25. 连续两次移动不会留下过时的说明：`main → A → B` 之后，根没有收到任何说明，`A` 持有说明，说它已不再是默认分支。`A` 是 `B` 的祖先期间对它 `end` 被拒绝；用户移回根之后对它 `end` 就被允许，因为守卫在调用时读树，不读说明。
26. 切换和结束以同一种次序准入。`main → A → B`、且 `A` 的结束已经开始时，只要 `A` 的连锁还在写它后代的结束，切到 `B` 就以 `branch_target_protected` 被拒绝。反过来，切到 `B` 被允许，之后对 `A` 的结束被拒绝，因为 `B` 在它下面。两种次序下都恰好有一个操作被允许，被拒绝的那个什么也不写。

26a. 一次切换和一条工具线自己的 `end` 以同一种次序准入，被拒绝的结束不写最终消息。B 的一轮在调用 `end` 结尾自己时，用户把 B 选为默认分支。如果切换先拿到树锁，结束回答 `branch_cannot_end`，`branch/end` 不写，B 的父线也收不到最终 steer：B 仍然开着，树就是这样说的。如果结束先拿到锁，`branch/end` 与最终消息一起提交，而切换以 `branch_target_protected` 被拒绝，因为它的目标正在结束，而一个已经在走向结束的目标不能成为默认分支。两种次序下都恰好有一个被接受，父线要么被告知一条真的结束了的线，要么什么也没收到。
26b. 工具结束的最终消息在两个窗口里都能活过停机。进程在 B 因工具结束提交 `branch/end` 之后、父线的 steer 之前停下。重启之后，B 是 `ending` 或 `ended`，不能被选为默认分支，它的父线用由 `branch/end` 的 source 推出的 submission id 恰好收到一次最终消息，再次重启什么也不写。在 `branch/end` 提交之前停下则什么也不写：B 开着，树就是这样说的，父线什么也没被告知。
27. 切换之后在线上继续，端到端跑一遍：用户从根开一条线 `B`，在它上面工作到一个工具结果之后还要用到它，然后选择“从这里继续”。不传 `branchId` 提交的消息到达 `B`，它的回答用得上那个早先的工具结果，根上的 `branch/default` 指向 `B`，重启后不再有新输入时同一条消息仍到达 `B`。用同一个方法选回根，Session 的对话回到根，之后不传 `branchId` 的消息到达根。
28. 升级用例：在 `FORMAT` 52 下写出、其中有一个由 `session.branch_create` 开出的线的库，在 53 下照常打开，那条线作为切换目标以 `branch_not_selectable` 被拒绝，而升级后用户新开的线被接受。存储的行一概不改写。

谁能开线，以及上限：

29. 根在另一条线是默认分支时开线，正是用户让它做的。`main` 开 `B`，用户把 `B` 选为默认分支，`main` 随后在自己的某个切点开 `C`：`C` 从 `main` 在该切点的历史开始，`main` 收到 `C` 的名册条目和最终消息，`B` 对此什么也不收到。把同样的请求发给 `B`，开出的线会从 `B` 的历史开始，而那里没有 `main` 在切点之后做的事，所以这个用例正是守卫存在的理由，而不是绕开它的办法。

30. 在工具子线上继续不会占满上限。`main` 开 `B`，用户切到 `B`，`B` 开 `C`，用户切到 `C`，`C` 开 `D`。上限为二时，`main → B → C → D` 这条路径除了它上面的线之外不占名额：默认分支和它的祖先不计数，所以本来会被拒绝的一次开线成功，而模型得知的计数也不含它们。这几次开线各自仍占一个名额，因为它开出的是一条不在受保护路径上的新子线，开线方的位置不会传给子线。

31. 一次移动让 Session 越过上限时，拒绝新的开线、不关闭任何东西。上限为二，`main → A → B` 是仅有的未结束工具线。选中 `B` 时两条都受保护，计数为零；移回 `main` 时 `A` 和 `B` 同时计数，也就是二，之后 `create` 回答 `branch_limit_reached`，其文本说明这是上限。这样一条线落在计数一侧时，Session 就停在上限之上，两个方向都没有关闭任何线：移动前开着的线在移动后仍开着，目录也会这样报告。移动从不只算它的两个参与者，记录的运行把两半都写出来：`BranchCapSwitch-BackToRoot` 从两条嵌套线之下开始，接受这次用满全部名额的切换；`BranchCapSwitch-OverCap` 期间多开一条线，把 Session 留在上限之上；`BranchEligibility-AtCapBelow` 在单线模型里陈述同一条规则。

32. 除两个开线方之外的每条线都被拒绝 `create`，什么也不写，并保留它开出的工具线：曾经的默认分支仍能给开出的工具线发消息、读它们，只是它自己的 `create` 回答 `branch_not_opener`。它的协调也不会结束任何别的线：`end {content}` 不接受目标，所以 B 结束的是自己，不连带它下面的线。用户要结束 C 时，要么经 `session.branch_end` 结束它，要么由 B 用一条 steer 消息请 C 自己结束，这就是 §10 写的约定；因祖先结束而结束的线是被那次连锁结束的，不是被这次工具调用结束的。

33. 最后一个名额和默认分支的子线。上限为二，已经有一条不在受保护路径上的工具线，根是默认分支，所以正好还剩一个名额。根开一条线：新子线是默认分支的后代而不是祖先，所以它计数，名额被花掉。同一树状态下根再 `create` 一次回答 `branch_limit_reached`，什么也不写。两次开线都不因由默认分支发出而获免，也没有任何线被关闭来腾位置。记录的模型运行 `BranchEligibility-CapBoundary` 正是这个场景：`RootInit` 取 `peers=1`，一次开线被接受，没有任何状态接受第二次。

34. 一次腾出名额的移动让下一次开线获准。上限为二，已经有一条不在受保护路径上的工具线，根持有角色，所以计数为一。用户把那条线选为默认分支，正是评审指出的场景：它不再计数，计数变为零，根之后的一次 `create` 获准，因为两个名额都空出来了。为了腾出名额没有关闭任何线，用户继续用的那条线照旧开着。记录的模型运行 `BranchEligibility-MoveToChild` 正是这个场景。

35. 一次预留占住最后一个名额。上限为二，默认分支是由 `main` 开出的工具线 `B`：`B` 在受保护路径上，所以不计数。另一条开着的工具线 `A` 不在该路径上，计一，因此只剩一个名额。`main` 预留一条工具子线，它的构建停在发布之前，而调用方不再等待它：构建继续，预留保留。同一树状态下 `B` 的 `create` 回答 `branch_limit_reached`，什么也不写，因为预留与开着的线一起计入。`main` 的子线发布后，计数为二——`A` 与新的子线——而不是三：预留变成了未结束的线，没有再扣一次。确定失败的创建则释放预留，名额重新空出。

## 21. 消融审查

留下的部分，以及每一部分为什么必要：

| 部分 | 原因 |
| --- | --- |
| 每个子分支上的身份块，同一个 key | 没有它，一条线压缩后会忘记自己是哪条线，或把父分支的名册当成自己的 |
| 自己的 key 下的名册，在每条开过工具线的线上 | 协调沿父子关系走，而现在任何线都可能是开线方，所以每个开线方都需要名册；它只列工具开出的线，因为用户线被切出来的那条线既不能给它发消息、也不能读它；与身份共用一个 key 会让名册写入让非根线自己的身份退役 |
| 消息是 `input/message`，带 append 和 steer | 这是线之间唯一的通道。模式是 InputQueue 的 |
| 开线方的 `read` | 开线方不用开口问，就能检查一条线的工作 |
| 空闲通知 | 没有它，开线方会一直等一条悄悄停下的线 |
| `branch/end` 与 `branch/ended` | 没有它们，一条线无法持久结束，恢复时又会被打开 |
| 结束上的 `source`（`BranchSource`） | 创建和结束以同样的方式记录发起方，插件的结束不会被记成这条线自己的结束 |
| `TimelineInfo.source` | 开线的一方不用解析 prompt 文本就能找到自己的线 |
| 一个表示用户开线的来源变体 | 核心必须在不解析 `operation_id` 的前提下分辨对话线与插件线，而客户端写下的已存 `host` 行也要能与插件写下的区分开 |
| 根上的 `branch/default` | 一条权威的、与模型无关的事实：App Server、存储的写线程和恢复都读它，不需要模型或带 key 的块，而根不能结束 |
| 每个非根的参与者一条状态说明，根不收 | 每个规则变了的参与者得知一次，其他线一概不动，所以其他线的缓存前缀都不移动。移回根时只有失去角色那条线的规则变了，所以只告诉它。根的权利不随角色改变，写给它的说明什么也说不了 |
| 默认分支不发空闲通知、也不能 `end` | 没有这两条，用户接着用的那条线会每轮都唤醒开它的线，还能结束 Session 对话所在的那条线 |
| 目录里的 `endEvent` 与 `ending` 状态 | 没有 `endEvent`，客户端无法指出级联结束那条线的结束；这条线自己的 `branch/end` 只隔一页。没有 `ending`，从一条线自己的 `branch/end`——工具结束、host 结束或级联结束——到它的已结束记录之间的窗口会读作未结束，客户端分不清一条线还能收输入，还是已经拒绝输入 |
| 工具由根和当前默认分支开，其他线不开 | 根保留它今天的权利，用户才能在另一条线持有这个位置时仍让它开线；默认分支能从用户接着用的那份工作里长出树；其他线仍是叶子，所以开线方只有两个，每条线创建时得到的协调关系也不移动 |
| 上限的计数规则 | 数受保护的那条路径，等于把关不掉的线算成名额，用户在工具子线上不断继续时，被占满的恰恰是它正在用的那条线。新开的子线不在那条路径上，所以一次开线总占一个名额，上限只有一条规则，而不是每个开线方一条。这条规则在移动的两个方向上都是从树里读出来的，`DefaultLeavesBelow` 携带的是后一个方向：失去角色的那条线之下的线重新计数，上限用拒绝、而不是关闭来回答 |
| 名额在预留时扣除 | 预留比一个停止等待的调用方活得久，所以只数已发布的线，会让两次调用拿走最后一个名额。在树锁下预留这条线的那一步扣一次，才让发布前后的计数是同一个 |
| 关于文件的提示词规则 | 唯一的保护是 FileReads，其余靠协议 |

删掉的：

- 对消息数、steer 数或往复轮数的上限；
- 给根写的状态说明，以及为它做 fork 清理的规则；
- 只把开线权绑在默认分支上的守卫，它无法响应用户在另一条线持有这个位置时让根开线的请求；
- 一次移动让 Session 越过上限时为腾位置关闭一条线；
- 文件改动的归属；
- 对 WorkspaceTools 的改动；
- 兄弟分支之间的通道；
- 按线的配置（system、模型、工具）；
- 结果约定与结果回交；
- 线读根；
- 为消息单独设的事件或概念（它们就是 InputQueue 的模式）；
- 父分支经工具结束子分支；
- 取值固定的 `by` 字段（现有的 `BranchSource` 记录发起方）；
- 插件线上的分类字段（`operation_id` 已经是开线一方的标签）；
- 运行时检查谁在结束一条线；
- 说明里写别的某条线是不是默认分支（再移一次就会让它过时，`end` 改为读树）；
- 存储的 `default_branch_id` 列（事实就是一条事件，写线程做缓存）。

## 22. 以后的选项

**在 Session 的限度内按线收窄。** 在 Session 的限度内收窄仍是同一个主体，所以与设计规则 §2 相容。它不属于本设计。做的时候：

- **更少的工具：** 在调用时拒绝，就像现在不能开线的线上的 `create` 被拒绝那样。工具定义和缓存前缀保持一致。
- **更低的权限：** 每条线一份策略，与 Session 的策略取交集。它需要 [Pursuit §10](plugins/Pursuit.md#10-以后的选项) 已经提到的按分支收窄 Permission。
- **更便宜的模型：** 在创建时、第一条输入之前设定的按线配置。不同的模型与父分支不共享缓存，所以它是用继承的前缀换成本。

**没有第三条开线方。** 开线方是根和当前默认分支（§4）。第三类调用方属于另一个决定，而不是一个遗漏的情况：任何线都能开线，正是树比两个开线方所需更深的原因，也正是协调关系会从创建一条线的线那里移走的原因。上限定多少由测量决定（§12.3），在实现改动里给出，不在这里猜。

**迁移升级前的 `host` 行。** 客户端在 `FORMAT` 53 之前开出的线保留它已存的 `host` 行，不可选中（§4）。想让这些线可选，需要一个自己的数据迁移决定，因为不借助新的种类，已存的行分不出客户端的线和插件的线。
