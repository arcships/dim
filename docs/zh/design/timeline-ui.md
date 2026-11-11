# Timeline 优先的 TUI：branch 与 fork 从时间线派生

状态：提案。依赖 [Timeline branches](timeline-branches.md)（[#427](https://github.com/arcships/dim/issues/427)）与 [Session fork](appserver/session-deletion-and-fork.md)。范围：CLI TUI 的信息层级、交互，以及它在客户端折叠的数据；不改 runtime、事件、存储或 App Server 方法。English: [英文](../../design/timeline-ui.md)。

## 1. 问题

#427 之后 TUI 有三个互不相关的入口：对话本身（`main` 的 transcript）、与 `/tasks` 平级的 `/lines` 面板，以及 TUI 里完全没有入口的 `session.fork`。用户看不到一条线从哪里长出来，也不能从历史中的某一点开线（#427 S3）。

在数据上这是同一件事：**branch 和 fork 都是在一条 Timeline 的某个序号处切割。** `branch/create` 带 `at`，`session.branch_create {parentBranchId, at, prompt}` 与 `session.fork {sessionId, at}` 都以 `at` 为参数。所以 Timeline 是主轴，branch 和 fork 是在它某一点上的动作，lines 面板是这些动作结果的地图。

## 2. 层级

```
L0  Session ............ 主体：身份、工作区、交互面
 └ L1  Timeline ........ 当前显示的那条线的 transcript；唯一的主界面
     ├ 点 .............. 切点：某条用户消息之前的回合边界
     │   ├ e 编辑 / q 引用         （已有）
     │   ├ b 从这里分支 → 本 Session 的一条线
     │   └ f 从这里 fork → 一个新 Session（仅 main）
     ├ 标记 ............ ⑂ 在这里打开了一条线（可选中）
     └ /timeline ....... 当前线的所有点，可跳转（§4.6）
 L3  Lines（/lines）.... 每条线挂在它被切出的父线下
```

1. **只有一个主界面。** 屏幕永远显示一条线的 Timeline。换线 = 换一条 Timeline 看，不换界面。
2. **派生从点出发。** branch 和 fork 都从当前 Timeline 的某个点开始；它的末尾也是一个点（`/branch`、`/fork`）。
3. **结果回到 Timeline。** branch 在父线上留下 `⑂` 标记，并在侧面板里回答，屏幕留在父线（§4.7）；fork 进入新 Session 的 Timeline。

## 3. Branch 与 fork

两者共享"点 → 派生"的入口，区别在结果住在哪里（#427 §2）：

| | Branch（`b`） | Fork（`f`） |
| --- | --- | --- |
| 结果 | 本 Session 的一条线 | 一个新 Session |
| 共享 | 工作区与同一个主体；与父线互发消息 | 只共享切点之前的历史 |
| 可用的点 | 任一未结束的线，合法的上下文边界 | 仅 `main`，且必须是 rest point |
| 之后 | 屏幕不动；新线在侧面板里回答（§4.7） | TUI attach 到 fork |
| 在源上的痕迹 | `⑂` 标记 | 无（§7） |

不适用的动作用 notice 说明原因（`A fork copies main; b opens a line here`、`Line has ended: X`）。切点是否合法只由 App Server 判断，拒绝时显示为 notice。

## 4. Timeline 与点

### 4.1 点

fullscreen 的 Tab 选取本来就停在用户消息上。被选消息的点是它之前的那个序号，即"这条消息还没说出口的时候"。它是原子批次的结尾，通常满足 `validate_context_boundary`，在 `main` 上通常也是 rest point；在这里换一种说法分支，正是 S3 的重问与对比。回合运行中排队的消息切在它提交之前，而不是它进入 transcript 的位置；这个切点落在运行中的回合里，所以在那里 fork 会被拒绝，开线也只在上下文边界允许时可行。

当前线的末尾也是一个点：它最近结束的回合，供 `/branch`、`/fork` 使用。还没有结束的回合时没有点（`No finished turn to cut at yet`）。

```
┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ › 把 cache 的锁换成 RwLock 试试                            ┃
┗━e 编辑 · b 分支 · f 派生━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
```

框太窄放不下时只显示 `e 编辑`。

### 4.2 `b`：从这里分支

`session.branch_create` 需要新线的开场输入，所以 `b` 把被选消息放进 composer，composer 的下一次提交就打开这条线：

```
╭──────────────────────────────────────────── provider / model ─╮
│ › 把 cache 的锁换成 RwLock 试试█                               │
╰──────────────────────────── ⑂ 从 main 开新分支线 · Esc 取消 ─╯
```

`Enter` 发送 `session.branch_create {parentBranchId: 当前线, at, prompt}`，新线在侧面板里回答（§4.7）；`Esc` 取消；切换会话或线会丢弃草稿。

### 4.3 `f`：从这里 fork

仅 `main`，从 Tab 选取或 `/timeline`（§4.6）发起；scrollback 就是用 `/timeline` 选消息。独立的有边框确认框写明切点，并说明当前会话保持不变；问题和选项在框内，不再放在 composer 底边。选中消息时问 `在“把 cache 的锁换成 RwLock 试试”之前派生会话？`，`/fork` 问 `在最近一个回合之后派生会话？`。

```text
╭─ 派生会话 ─────────────────────────────────────────────╮
│ 在最近一个回合之后派生会话？                           │
│ 创建新会话，当前会话保持不变。                         │
│   1. 创建派生会话                                      │
│ › 2. 取消                                              │
│ ↑↓ 选择 · Enter 确认 · Esc 取消                         │
╰────────────────────────────────────────────────────────╯
```

默认选中取消；↑↓ 或 Tab 切换，Enter 确认，`1`/`y` 派生，`2`/`n`/Esc 取消，其余按键不关闭对话框。确认后发送 `session.fork {sessionId, at}`，在 `已派生 ID · N 条消息` 边界下 attach 到 fork，并把被选消息放进它的 composer。fork 在用户提交之前不开始任何工作（fork 设计 §6.5）。从 Timeline 发起时，取消返回原操作列表；否则恢复输入区，保留草稿和 transcript 位置。

scrollback 在 live 交互区以确认框替换输入框，不提交到历史；fullscreen 显示为 overlay。两种模式的选项、按键一致，窄窗口折行显示问题并保留可选操作。

### 4.4 标记

`branch/create` 在线打开的那一刻写在父线上，对过去的切点来说就是父线的末尾，而不是切点处。已提交的 scrollback 只能追加，所以标记留在事实所在的位置，并写明它的切点：

```
 › find the leak
 … 回答 …
 › try the parser
 … 回答 …
 ⑂ try the parser via cache · before “try the parser” · f7f0a984
```

线以它用途的第一行命名，后面跟短 id；没有用途的线用短 id 命名。切点恰好在本 transcript 的某条消息之前时才写 `before “…”`；在末尾打开的线（`branch` 工具调用）没有。标记和成员卡片一样是 Tab 的停靠点：`Enter` 或点击显示那条线。结束不做标记：lines 面板会显示，`main` 也会收到它的工具线的通知。

### 4.5 我在哪里

显示一条线时，composer 底边写出从 `main` 到它的路径（每条线用它的名字，§4.4）和切点：

```
╰── ~/repo (main) · ⑂ main ▸ parser race ▸ the guard · after message 4 ─╯
```

`after message N` 是切点时父线历史中的用户消息数，包括它继承的部分。

这个标签的工作区部分先于线让步。左侧状态很宽时，它先整体缩短（截掉路径前段，再放弃分支，再只保留截断的路径），而不是直接消失；只有连这些都放不下时边框才丢掉它。显示线时仍是原来那三档：完整标签、只剩 `⑂`、不画，所以路径信息永远不会挤掉线的信息。标签写的是 Session 实际工作的目录，不是进程的启动目录；attach 到别的目录的 Session（显式 `--cwd`、`/resume` 到另一个 worktree）时它连同那个目录的分支一起跟过去。还没有 Session 的 Home 写回启动工作区。文件提及、粘贴路径和拖入的文件仍以本地 `--cwd` 为基准（[CLI 附件输入](../../design/cli-attachment-input.md)），所以跨工作区 resume 之后，标签写的目录可能与它们不同；不同时 `@` 面板会在标题里写出该基准。

### 4.6 `/timeline`：跳到某条消息

Timeline 的点就是它的用户消息。Tab 从最新一条起逐条走；`/timeline` 把它们一次列出，与桌面端的消息导航条一样：每行是这条消息和它回答的第一段文字。

```
┌ Timeline ────────────────────────────────────────────────────────┐
│   #1  weather in Shanghai      Cloudy, 20°C, light wind …        │
│   #2  try RwLock for the cache The lock moves into …             │
│ › #3  adsa                                                       │
└────────────────────────────────── Enter actions · Esc close ┘
```

- 默认选中最新一条。输入不做过滤：字母键作用于选中的消息，与选取上的按键相同（§4.1–4.3）。
- `Enter` 打开操作列表，写明所选消息，列出跳转、编辑为新草稿、引用、创建分支、派生会话；派生选项说明切点在该消息之前。↑↓ 和 Enter 选择，数字 1–5 直接执行；原有 `e`、`q`、`b`、`f` 在两个列表都可用。Esc 返回消息列表，保留所选消息和列表位置，再按 Esc 关闭。scrollback 在 live 交互区显示，fullscreen 显示为 overlay，均不提交到历史。
- 跳转会关闭面板。全屏选中这条消息，与 Tab 选中一样，之后可用选取的按键。scrollback 不能往上走：还没画出的消息从它开始作为一页更早历史画出；已经画出的给出提示，`/tui` 可直接跳到它。

恢复会话时只画最新 100 条。全屏在 transcript 顶部放一行加载按钮（`↑ 130 earlier messages · Enter or click to load`）：点击，或用 Tab 走到它再按 `Enter`，就在上方画出前 100 条，并让原来最早的那条消息留在视图顶部。scrollback 没有这一行，更早的历史由 `/timeline` 跳转画出。它取代了 `/history`：后者把更早的消息接在最新消息之后翻页画出。

### 4.7 `/branch <prompt>` 与侧面板

`/branch` 像旁支提问一样把 prompt 直接写在命令后面。面板列出参数；在带参数的命令上按 `Tab` 或 `Enter` 不会立即执行，而是填进 composer，参数位置留一个提示：

```
╭──────────────────────────────────────────── provider / model ─╮
│ › /branch <prompt>                                            │
╰───────────────────────────────────────────────────────────────╯
```

在 `/branch try the parser via cache` 上按 `Enter`，一步就在当前线末尾（§4.1）发送 `session.branch_create`，没有第二次草稿。当前线上正在跑的回合继续：这条命令不排队，也不打断。不带 prompt 的 `/branch` 留在 composer 里，并提示 `/branch needs <prompt>`。

新线在面板里回答，屏幕留在父线。fullscreen 下面板在 composer 上方：

```
 … 父线的 transcript 继续输出 …

╭─ ⑂ b-7c · try the parser via cache ─────────────────────────╮
│ ⑂ working…                                                   │
╰──────────────────────────────────────────────── Esc close ─╯
╭──────────────────────────────────────────── provider / model ─╮
│ ›                                                             │
╰───────────────────────────────────────────────────────────────╯
```

```
╭─ ⑂ b-7c · try the parser via cache ─────────────────────────╮
│ ✓ Explored 2 files ▸                                         │
│                                                              │
│ The lock is held across the await in cache.rs:88; moving the │
│ parse out of the guard removes the race. Patched and tested. │
│ ⑂ done                                                       │
╰──────────────────────────────────────────────── Esc close ─╯
```

1. 面板像屏幕画 transcript 一样画出这条线的过程（标题里已有的开场输入除外）：思考、按工具详情设置显示的工具调用，以及边到边显示的回答。超过屏幕三分之一时显示末尾，最后一行是线的状态：回合运行时是 `working…`，回合结束、线等待输入时是 `done`，线结束时写明它是怎么结束的。
2. `Esc` 先关面板，再做别的事。线继续运行，关掉的只是面板对它的跟随。留在 scrollback 里的是 `⑂` 标记（§4.4），在它上面按 `Enter`，或用 `/lines`，可以显示这条线。
3. 同一时间只有一个面板：再一次 `/branch` 或 `b` 会替换它。显示某条线、切换 Session 或 `/new` 会关闭它。
4. fullscreen 下面板把 transcript 顶上去，关闭后 transcript 回到 composer 上方贴底。scrollback 不会抬起 composer，所以面板开在 composer 下方，和菜单同一个位置，关闭时 composer 上方不会留空。
5. 无法开线的 `/branch`（还没有结束的回合、当前线已结束）把草稿留在 composer 里，并说明原因。

### 4.8 模型还没读到的消息

用户消息先写入、再由 InputQueue 采用、最后由 Loop 消费。客户端从 `input/applied` 起画出这一行，行显示模型是否已经读到：

| 状态 | 事实 | 模型读到了吗 | 画成 |
| --- | --- | --- | --- |
| queue | `input/message`，InputQueue 还压着 | 否 | 线运行期间 composer 上方的队列条 |
| applied | `input/applied`，等 Loop 的下一个请求 | 否 | 虚化的消息行 |
| consumed | `input/consumed`，已进入携带它的那次请求 | 是 | 正常的用户消息行 |

虚化的行保留文本和切点，用 muted 颜色画、不加底色条。fullscreen 里消息在它被说出的位置就提交，`input/consumed` 时原地重画这一块；离开 fullscreen 时先把已提交的待消费行结算，因为 scrollback 收不回这个提示。scrollback 只能追加，所以那里会变的行留在 live tail 里（与流式回答、运行中的工具组同一规则），结算时才以用户色进入 scrollback。tail 画在所有已提交的行之后，所以待消费行只在它是最新一行时留在那里：如果后面的行先结算（例如跨过一次 stop 的 applied 输入），这条消息按 Timeline 顺序以用户色提交，已提交的行收不回这个提示。于是只要绘制还能改，等下一个请求的消息就读作未读：

```
 › how do we fix this deadlock                       模型已读到
 › add the missing test too                          仍是虚化：还没读到
```

applied 的输入可以跨过一次 stop 继续待消费，正是下一个 turn 要消费的状态（[中断](../../architecture/rust-agent/interrupt.md)）。

界面自己直接提交、不属于 transcript 那些行的内容（notice、`!!` 运行、失败行、resume 的边界）不在这条顺序规则里，可以排在待消费消息之上。

用户消息的那一行承载操作者自己的话。运行时为模型在其后追加的上下文不属于他们，客户端可以把它画成紧跟在消息下面的一行：CLI 就这样画钩子的上下文，每个 `dim-hooks` 块一行静音的`钩子上下文 · N 行`，正文放在详情设置之后。这样的行不是消息：它不参与这条顺序、不带切点，也不被消息导航栏、`/timeline` 或更早历史页计数，且从不使用用户颜色或竖条。

上下文行属于它那条消息的一组，而不属于整个 transcript，所以它自己绝不对那条消息表态。消息还在 scrollback 的 live tail 里等待时，它的上下文行也留在 tail 里、紧跟在它后面，只追加的 scrollback 两者都不提交：上下文行不是一个已落定的行，因此不能把消息挤出 tail。消息一旦落定——模型读了它，或后面某个已落定的行迫使它提交——它的上下文行就跟它一起按内容顺序提交，紧跟在它后面。因此无论谁让消息落定，也就让它的上下文行落定，客户端不需要再维护第二个可能与之不一致的标记。fullscreen 保持自己的规则：消息在说出的位置提交，它的上下文行也一样，而待消费消息的原地重画不会移动它们。

待消费的样式只属于终端界面：Web 的 v1 `session.event` 会过滤 `input/consumed`（[事件驱动流程](../../architecture/rust-agent/event-driven-flows.md)），所以 Web 上消息仍显示为已发送。

行按 input id 从写下它的 `input/message` 跟到结算它的 `input/consumed`，所以重连后折叠同一批事件时，画出的就是它已经到达的状态，而不是写入那一刻的状态。

## 5. 切线

与 #427 §14 相同：切换时写一行 `⑂ 名字 · id` 边界，等该线的历史加载完后接着是它的 transcript，只含它自己的事件（每个分支各自编号，[sessions service D11](appserver/sessions-service.md)）。它继承的历史属于父线，离它只有一个标记的距离。

## 6. Lines 面板

`/lines` 是 Timeline 的地图。子线先按它在父线上被切出的位置排序，再按打开顺序。每行以线的用途开头，后面写 `after message N` 和它的 id：

```
┌ Lines ───────────────────────────────────────────────────────────┐
│   main                  the session itself                       │
│     docs pass           after message 1 · b-02 · ended itself    │
│     parser race         after message 3 · b-7c · working · shown │
│       the guard         after message 4 · b-c3 · working         │
└──────────────────── Enter show the line · x end it · Esc close ──┘
```

fork 是别的 Session，归 `/resume`。按键保持 #427 §14 的定义。

### 分支未读回答

输入框底栏提示有未读回答的分支数。用 `/lines` 查看标记为「未读取」的分支，面板打开时选中第一条未读分支。按 Enter 打开对应正文并清除这条分支的提示。新回答到达时，输入焦点留在原处。

```text
+--------------------------------------------+
| >                                          |
+------------------------ 未读取 (1) · /lines+

/lines
  main
  * parser race       未读取 · 等待中
  Enter 打开所选分支
```

Session worker 通过实时 `session.event` 中非空白的 assistant 正文判断新回答。已显示的分支和已打开的侧面板视为已读。提示按分支计数，历史加载和重复事件不会重新点亮提示。回答及用户已读位置只保存在当前 attachment 内。新 attachment 从空标记开始，旧回答仍可作为历史查看。

两种 TUI 模式都复用输入框底栏。空间不足时优先显示提醒，按可用宽度依次缩成 `/lines (N)`、`/lines`、`*`。全屏模式仍优先保留左侧状态。

## 7. 客户端数据

| # | 需求 | 归属 | 说明 |
| --- | --- | --- | --- |
| R1 | `LineView.cut`：父线序号及该处的消息数 | CLI projection | 由 `branch/create.at` 与父线的用户输入折叠而来；无格式变化 |
| R2 | 用户消息行的切点（`ViewRow.cut`）、模型是否已消费（`ViewRow.pending`）与标记行的线（`ViewRow.line`） | CLI projection | 来自客户端已有的序号与输入状态 |
| R3 | `session.branch_create` 与 `session.fork` 调用 | CLI session host | 已有方法；从过期 attachment 发出的请求在本地拒绝 |
| R4 | 后续、可选：显示 fork 的来源（源上的 `↗ forked`，fork 里的 `forked from S`） | App Server，需格式决定 | `session/forked` 不带数据，fork 设计也不加来源列 |
| R5 | CLI 快照里的未读分支 ID | CLI session worker | 从现有实时回答及客户端已读位置派生。位置属于当前 attachment。 |

## 8. 不做的事

- **改写 `main` 的历史。** #427 §3：不是 branch。"从这里重来"是 `b`（在本 Session 里对比）或 `f`（新 Session）。
- **Compare / adopt。** S3 的后半段需要单独设计；同一切点下的兄弟线就是对比要展示的东西。
- **画出 DAG。** scrollback 只能追加；拓扑由标记、面板和路径表达。

## 9. 消融检查

- 没有新模式：点沿用 Tab 选取，线标记和加载按钮沿用卡片选取，切线沿用 #427，fork 后的 attach 沿用 `/resume`。唯一的新面板 `/timeline` 列出选取所走的点，按键与选取相同。
- 新增的交互：选取上的 `b`/`f`、composer 的分支草稿、fork 确认框、Timeline 操作列表、路径式底边、`/branch` 与 `/fork`。确认框让切点和影响可见，操作列表使动作不再依赖底部快捷键提示；两者复用已有 prompt 渲染，不写持久历史。
- 侧面板（§4.7）是唯一新增的界面：不离开父线就跟随一条线，需要一个父线 Timeline 装不下的视图。它不折叠新数据，CLI 本来就为每条打开的线保留 transcript。它收起后的形态就是已有的 `⑂` 标记，所以关闭时不写任何东西。
- 考虑过但去掉的：lines 面板里的 `b`（需要每条线的末尾；当前线上的 `/branch` 已覆盖）、按点置灰的提示（按键时的 notice 说的是同一件事，状态更少）、结束标记（§4.4）、继承前缀的一行摘要与 `Esc Esc` 回父线（§5 和标记已经通向那里）。
- 待消费消息（§4.8）不新增一份列表：还是同一行，按 input id 关联，队列条就是这一行的前一态。scrollback 的 live tail 与 fullscreen 的原地重画，就是流式回答、运行中工具组和委派卡片已有的规则，三个状态都来自客户端已收到的事件——没有计时器、没有新事件、没有第二份 transcript。

## 10. 决定

1. 点是用户消息之前的边界，加上线的末尾；不提供回答之后的边界。
2. `b` 和 `f` 都像 `e` 一样把被选消息带进 composer。
3. R4 等待格式决定。
4. 开线后屏幕留在父线；新线在侧面板里回答（§4.7）。
