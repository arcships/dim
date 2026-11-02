# CLI 命令与导出

F6（#77）覆盖[总体设计附录 D](overview.md#附录-d-cli-层)和[功能清单](../../feature-list.md#cli-commands)
列出的命令入口、对话导出和 JSONL 执行。原生 CLI 仍是现有守护进程的客户端。本文定义实现所依据的
CLI 接口；当前可执行文件由[原生 CLI 合同](../../../crates/dim-cli/README.md)描述。

[附件输入合同](../../design/cli-attachment-input.md)定义空闲草稿编辑器、本地图片来源和粘贴处理。
多行/粘贴编辑器、无头 exec 的图片参数、剪贴板/文件插入、补全和拖入路径已实现。附件合同记录了
各原生平台的验收证据和尚未完成的检查。

## 命令菜单

scrollback 与 fullscreen TUI 都按分类分组，分类及分类内的命令名按其英文标识的字母顺序排列。
搜索时匹配程度优先，同等匹配按分类和命令名排序。
退出和挂起必须完整匹配命令名或别名才执行：选择不完整匹配时只补全命令名并保留输入框。
完整输入的命令（包括隐藏命令）仍走各自的处理流程。
菜单中的权限入口为 `/permissions`；`/approvals` 和 `/approval` 是完整输入时仍可使用的兼容别名。
菜单不列出运行、排队和 steer 的按键，它们只由按键触发。菜单只列出斜杠命令：选择较早消息、滚动到顶部或底部等视图操作也不列出；
`Tab`、`Home`、`End` 保留各自的绑定。

两种模式下，菜单都适应终端可用的列数和行数。命令名优先于描述和快捷键标签；快捷键标签会挤占
命令名时省略标签，但不改变按键绑定。短屏幕少显示一些命令，可见窗口跟随选择。
Scrollback 保留菜单上方的命令草稿，必要时省略菜单标题和页脚；fullscreen 保留输入框上方的
命令面板查询行和选中项。分类分组和排序不随终端尺寸变化。

当通常的历史保留空间使实时区域不足八行时，实时控件优先：两种渲染器都可为实时区域使用最多八行，
但不超过终端高度。Fullscreen 先减少输入框的空白内边距和命令面板查询行的留白，避免两个输入区域
重叠。草稿编辑器在剩余行数内滚动，保持光标可见且不丢弃文字。其他情况下保留通常的历史空间。

```text
Commands
  app     /exit        exit
  app     /keys        show the key bindings
  model   /permissions   file access, tool approval, process isolation and command rules
  session /rename      rename this session

/ + Enter          -> /exit (completion only)
/exit + Enter      -> exit
/permissions + Enter -> permission controls
```

```text
Fullscreen, 8 rows             Scrollback, 8 rows
+- Command Palette -------+
| >                       |   +- input -----------------+
| app /exit               |   | > /                     |
+-------------------------+   +-------------------------+
                              +-------------------------+
+- input -----------------+   | app /exit               |
|                         |   +-------------------------+
+-------------------------+   AGENT
```

## 运行按键与排队

排队与引导和 Codex CLI 不同：运行中 `Enter` 排队，`Ctrl+S` 引导。其余运行按键与 Codex CLI 的交互一致（已在其 TUI v0.166.0 中核对）。没有运行时，`Enter` 提交。

```text
按键                 空闲                         运行中
Enter               提交                         排队草稿；草稿为空：steer 最早的排队消息
Tab                 选择较早消息                 选择较早消息
                    （fullscreen）               （fullscreen）
Shift+Left          —                            草稿为空：把最新一条排队消息取回草稿
Esc                 保留草稿                     中断；所有排队消息按先后顺序回到草稿
Ctrl+S              —                            草稿非空：steer 草稿；为空：steer 最早的排队消息
Up / Down           输入历史                     输入历史
Home / End          滚动（fullscreen，草稿为空） 滚动（fullscreen，草稿为空）
```

- 排队消息在当前运行结束后运行，每次运行一条。被 steer 的消息在当前运行的下一个轮次边界并入该运行。
- 取回消息和中断都通过 `session.input.withdraw` 撤回消息。文字只在撤回被删除后才进入草稿。
  已离开队列的消息不会放回，其文字也不会再次发送。
- 中断产生的撤回先于 stop 发出，因此运行不会开始下一条排队消息。
- `Ctrl+C` 清空草稿。`Esc` 从不清空草稿。

### 队列条

有消息排队时，输入框上方的队列条在带标题的边框里画出队列：按排队顺序排列，最先运行的一条在最上面，最后一行提示作用于队列的按键。运行中，草稿为空时，`Shift+Left` 把最新排队的消息取回输入框编辑。`Ctrl+S` steer 当前草稿；草稿为空时，`Ctrl+S` 或 `Enter` steer 最早的排队消息。Up/Down 仍用于输入历史。提示只在命令表里有对应命令时才写出命令名，因此它写出的 `/name` 都在本地执行，不会作为用户消息发给模型。目前没有查看队列的命令，所以提示只列按键。

```text
┌─ Queue (2) ─────────────────────────────────────┐
│ › fix the failing test in session_host          │
│ › then update the design note                   │
│ shift+← edit last · ctrl+s steer now            │
└────────────────────────────────────────────────┘
```

队列条没有显示的消息把数量加进提示：`+3 more · shift+← edit last · ctrl+s steer now`。队列条最多显示五条消息，并在队列排空时保持已达到的高度。

## 用量报告

两种 TUI 模式下，`/usage` 都输出主分支的上下文用量。当前模型使用 Dim OAuth 时，
还通过现有[账号用量合同](../../../crates/dim-oauth/docs/runtime-contract.md)查询并输出账号的
套餐、积分、滚动窗口和功能额度。其他模型提供方只显示上下文用量；仅登录 Dim 账号不会启用
账号用量报告。`/login` 中的账号视图保持可用。

上下文和账号查询失败时分别报告：上下文用量不可用不会隐藏账号用量，账号查询失败也不会丢弃
上下文报告。账号未返回的可选字段不显示。下图为虚构示例，不代表账号一定具有 5h 窗口。

```text
/usage
  Context: 1,849 / 256,000 tokens (...) ...

  DimAgent OAuth
  Pro · active · expires 2026-11-30

  Credits     25,000 / 300,000  8%
  [###-------------------------------------]

  5h window   9,500 / 10,000  95%
  [######################################--]

  Web search  211 / 1,500  14%
  [######----------------------------------]
```

## 目标

`/goal` 读取当前显示线的目标，并通过 Goal 注册的方法写入它。方法、标记块和读取目标的规则属于
[Goal](plugins/Goal.md#a2-session-方法)（A.2、A.3）和 [Pursuit](plugins/Pursuit.md#3-对-goal-的修改)；
本节规定原生客户端用它们做什么。该命令作为 `session` 分类列在命令菜单里，与 `/compact` 相同。

| 形式 | 调用 |
| --- | --- |
| `/goal` | 无调用：当前显示线上的目标，或它没有目标 |
| `/goal OBJECTIVE` | 目标处于进行中或暂停时 `session.goal.edit`，否则 `session.goal.set` |
| `/goal set OBJECTIVE` | `session.goal.set` |
| `/goal edit OBJECTIVE` | `session.goal.edit` |
| `/goal pause` | `session.goal.pause` |
| `/goal resume` | `session.goal.resume` |
| `/goal clear` | `session.goal.clear` |

每次调用都把当前显示线写进 `branchId`，并以该输入的 `sequence` 回复。不写其他内容的形式
（`/goal`、`pause`、`resume`、`clear`）不再接受别的词；否则行内其余内容就是目标文本，含空格。
`set` 和 `edit` 也显式给出目标，因此恰好是这些形式词之一的目标写成 `/goal set pause`（目标为 "pause"），
目标为 "set" 时写成 `/goal set set`。

`/goal OBJECTIVE` 只在目标处于进行中或暂停时写 `edit`：状态、裁判和续跑计数保持不变，暂停中的目标仍为暂停。
没有目标、目标没有状态、或目标已完成/受阻时，它写 `set`：完成的目标不会被编辑回工作状态，完成之后再输入
新目标就是开始下一个目标——状态为进行中、计数从第一次续跑重新开始，该线会朝它工作。

`judge` 属于 Pursuit，任何形式都不发送它：从终端设置的目标由模型判定；由评审者判定的目标经
`/goal edit` 保持其裁判不变，因为 `edit` 只写目标文本。`/pursue` 是 Pursuit 的后续选项（§10），
不属于本契约。

**读取目标。** 客户端按 Goal A.3 的规则，从它本来就在读的分支事件中折叠目标：最新存活的
`goal:objective` 块是目标文本，最新存活的 `goal:status` 块是状态，`clear` 退役两者，`edit` 保留裁判，
被 InputQueue 删除的输入会使折叠跳过它的块。它不新增查询方法，也不向 Goal 询问任何东西。不带参数的 `/goal`
打印这个折叠结果。在 `set` 与 `edit` 之间选择之前，客户端先把该线读到已发布的头部，写入之后再读一次才打印：
显示的状态与选择的方法都来自事件，而不是从回复猜出来的状态。折叠在该线自己的开场事件处重新开始——它的 `branch/start`，
或分叉在复制完事实之后写入的 `session/forked`——所以新开的线没有目标，分叉 Session 的 `main` 也从不显示
复制来的目标（Goal §2、A11）。v3 客户端会收到这个开场事件，因此分叉在客户端与 Goal 里的读法一致。

**Goal 未装载时。** Session 会列出它提供的方法（`session.methods`）。其中没有 `session.goal.set` 时，
`/goal` 打印 `Goal is not available on this daemon.` 并且不做任何调用。不提供 `session.methods` 的守护进程
按它的话处理，直到某次调用回复 `unknown_method`，那时打印同一行。任何形式都不把输入行当作消息提交：
会写入的 `/goal` 发送的是 Goal 自己的变更输入，之后由 Goal 决定。

**客户端显示什么。**

```text
> /goal
No goal on main.

> /goal Make the CLI tests pass
Goal set on main · active
  Make the CLI tests pass

> /goal Keep the CLI tests green
Goal edited on main · active
  Keep the CLI tests green

> /goal pause
Goal paused on main · paused (user)
  Keep the CLI tests green

> /goal
Goal on main · paused (user)
  Keep the CLI tests green

> /goal resume
Goal resumed on main · active
  Keep the CLI tests green

> /goal clear
Goal cleared on main.
```

第一行给出实际调用的方法、目标所属的线和当前状态；`edited` 表示找到已有目标、只改了目标文本的那种形式。
目标由 Pursuit 的裁判判定时，状态后跟 `evaluator judge`。暂停状态给出原因：`user`、`interrupted`、
`limit`、`turn failed` 或 `evaluation failed`。目标文本另起一行，按它自身的换行输出。目标已清除或该线没有目标时只打印一行。

行客户端和两种终端 UI 模式都打印这些行；在终端 UI 里它们是一段报告，因此更早的报告仍能在上方读到。
当前显示线跟随目标（进行中或暂停）时，状态区带一个短元素，完成、受阻或已清除的目标没有：

```text
 ╭──────────────────────────────────────────────────────────────────────────────╮
 │ › _                                                                          │
 ╰────────────────────────────── ⑂ main ▸ child work · after message 0 ─────────╯
  AGENT  ⏵⏵ Ask (legacy)  ◎ goal active  1.8K / 256K              provider/model
```

fullscreen 把同一元素画在输入框的下边框，位于运行状态旁：

```text
 ╭──────────────────────────────────────────────────────────────────────────────╮
 │ › _                                                                          │
 ╰──── ◎ goal paused   ⏵⏵ Bypass   1.8K / 256K ───── ● main · after message 4 ─╯
```

## 提供方账号

原生客户端为每个在 `provider.management` 里带 [`account` 描述符](appserver/provider-management.md#31-读取模型)
的提供方签到。描述符是唯一来源：客户端不保存按提供方维护的列表，也不比较任何提供方 id。
被描述的账号就是签到列表；描述符的 `loginMethods` 决定每个账号走哪种流程（`browser` 或
`device`）；调用是描述符命名空间下的方法——`<namespace>.login.begin`（设备）、
`<namespace>.login.browser.begin`、`<namespace>.login.browser.complete`、
`<namespace>.login.status`、`<namespace>.login.cancel` 和 `<namespace>.logout`。
守护进程没有公布的方法按「不支持」报告，而不是启动一个无法完成的流程。

`account` 命名空间就是 Dim 账号。它的调用还带 `providerId` 和账号目录；`/login` 保留其账号面板
（身份、套餐与用量、企业邮箱入口），因为描述符既不带身份也不带用量。其他命名空间都是单一账号，
调用只带 attempt id。

终端界面在 `/connect` 的每一行上提供这些操作：未登录的账号显示 `Sign in`，打开即进入它唯一的
流程；已登录的账号显示状态并提供 `Sign out`。命名空间为 `account` 的行改为打开 Dim 账号的
`/login` 面板，其中包含身份、套餐与用量。没有描述符的提供方保留连接表单，打开时什么也不启动。

```text
/connect
╭─ Providers ──────────────────────────────────╮
│  OpenAI Codex                     Sign in    │
│  Grok Build                       Sign in    │
│› Add custom provider                         │
╰─ Enter open · Esc clear/close ───────────────╯
        │ 在未登录的被描述行上按 Enter
        ▼
╭─ OpenAI Codex OAuth ─────────────────────────╮
│   Browser login                              │
│ https://…/authorize?…                        │
│   Callback URL: (paste here)                 │
│   Submit callback                            │
╰─ Select to copy · Enter opens link ──────────╯

╭─ Grok Build OAuth ───────────────────────────╮
│   Device login                               │
│   Enter code:    ABCD-EFGH                   │
│ https://…/device                             │
│   Restart device login                       │
│   Waiting for authorization                  │
╰─ Select to copy · Enter opens link ──────────╯
```

`dimcode login [PROVIDER]` 在行客户端启动同样的流程，并打印守护进程返回的 URL 或验证码；
不给 PROVIDER 时列出被描述的账号。`dimcode logout PROVIDER` 让其中一个账号退出；
`dimcode account PROVIDER` 打印它的非机密状态。`--help` 列出这些形式，且从不连接守护进程。
没有 Dim 账号提供方的构建会从这些命令里说明这一点，并指出 API-key 提供方这一替代方式。

## 插件运行时

RuntimeManager 提供插件代码运行的 Node 与 Python 运行时（[RuntimeManager](plugins/RuntimeManager.md)）。
守护进程在动作应用配置的那一刻才读配置（RuntimeManager §3.3），所以客户端可以让用户打开运行时、
选择它的来源、看它在做什么，而不必手改 `<home>/runtimes/config.json`，也不必重启任何东西。
`/runtime` 在两种终端模式下打开同一个面板；行客户端与 CLI 里对应的区域是 `dimcode runtime`。

### 面板

行是两个运行时，固定 node 在前、python 在后。每行显示名字、配置的来源（`download` 或 `system`）、
正在生效的版本（`download` 是固定版本，`system` 是检查过的解释器版本，`off` 且从未检查时为空）、
打开时的 `auto`，以及 `runtime.list` 给出的状态：`off`、`acquiring`、`starting`、`ready`、带原因的 `down`，或带
原因（列出每个来源和它的错误）的 `failed`。

行下方列出在等这个运行时的插件行——因为它是 `off` 或 `down` 而处于 Pending 的那些
（[PluginManager §2.5](plugins/PluginManager.md#25-runtimes)）——这样"goal 为什么不工作"和它的
解决办法在同一个地方。失败时每个来源一行，与 `runtime.list` 报的一致。

```text
/runtime
╭─ 插件运行时 ──────────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    ready                                    │
│   python    system    3.12.15    off      未安装                          │
│                                                                           │
│   4 个插件在等 node：goal、session-title、scheduler、workflow             │
╰─ Enter 安装或应用 · s 来源 · a 自动 · r 重试 · Esc 关闭 ──────────────────╯
```

```text
╭─ 插件运行时 ──────────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    acquiring  12s                           │
│   python    system    3.12.15    off                                      │
╰─ Esc 关闭 · 关掉面板不会中断正在进行的获取 ───────────────────────────────╯
```

```text
╭─ 插件运行时 ──────────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    failed                                   │
│               https://nodejs.org/dist/v24.21.0/…：连接超时                │
│               https://npmmirror.com/mirrors/node/v24.21.0/…：404          │
│   python    system    3.12.15    off      未安装                          │
│                                                                           │
│   4 个插件在等 node：goal、session-title、scheduler、workflow             │
╰─ Enter 重试 · s 把 node 换成 system · Esc 关闭 ───────────────────────────╯
```

`acquiring` 只显示已用秒数，不显示百分比：守护进程不报告获取的字节数，面板也不编一个。来源列表只
在失败时出现，因为那时守护进程已经报告了它。

### 菜单项

`/runtime` 是 `app` 类的 slash 命令，因此菜单把它列在 `/keys` 与 `/settings` 之间；它不需要 Session
（`Needs::Nothing`），也不接受内联参数，因为动作都在面板里。下面打字的子形态只在输入时接受，不作为
菜单补全列出。

### 按键

面板只接受键盘，scrollback 与 fullscreen 两种模式一致，符合命令菜单契约。

| 按键 | 含义 |
| --- | --- |
| `Up` / `Down` | 在运行时的行之间移动 |
| `Enter` | `runtime.install NAME`：把当前配置应用下去 |
| `s` | 切换该运行时的来源（`download` ↔ `system`）并应用 |
| `a` | 切换该运行时的 `auto` 并应用 |
| `r` | 重试：与 `Enter` 同一个调用，保留它是因为失败行的页脚这么写 |
| `Esc` | 关闭面板；已经在跑的获取继续跑 |

`acquiring` 的行上按 `Enter` 什么也不做，这正是守护进程在获取期间的 `install` 行为。`ready` 的行上
来源没变时按 `Enter` 也不做事；来源变了则撤回、释放并按新来源重新获取，即 RuntimeManager §3.3 的定义。

### 写入

`runtime.install {name}` 就是运行时已有的那个应用动作：`off` 或 `failed` 时获取，`down` 时重新提供
启动命令，配置里的 `source` 与上次应用不同时撤回、释放并按新来源获取。它立即带着条目回复，所以面板
跟着 `runtime.list` 走，而不是等一次获取完成。

`s` 与 `a` 是 `runtime.configure {name, source?, auto?}` 再加上同一条应用路径。该方法原子地写
`<home>/runtimes/config.json`——RuntimeManager 自己的文档——按运行时、按字段覆盖预置；写不出或读不出
就是该调用的 `runtime_configuration` 错误。面板随后显示这个错误，并保留该行原来的状态，因为什么都没变。

### 等在运行时上的插件

`/plugins` 说明 Pending 的行在等哪个运行时，在这些行上按 `Enter` 会打开 `/runtime` 并选中那个运行时。
今天这些行只说 `pending`。

```text
/plugins
╭─ 插件 ────────────────────────────────────────────────────────────────────╮
│ › goal           on     pending · 等待 node 运行时                        │
│   session-title  on     pending · 等待 node 运行时                        │
│   scheduler      on     pending · 等待 node 运行时                        │
│   workflow       on     pending · 等待 node 运行时                        │
╰─ Enter 打开 /runtime · o 开/关 · r 重载 · Esc 关闭 ───────────────────────╯
```

### 行客户端与 CLI

打字的形态覆盖整个区域，调用方不需要面板：

```text
/runtime                              在 scrollback 客户端里打印同样的行
/runtime install node                 应用 node 的配置
/runtime source node system           把 node 换成系统解释器并应用
/runtime auto python on               用配置打开 python 并应用
```

```text
$ dimcode runtime list
node    download  24.21.0   ready
python  system    3.12.15   off       未安装
$ dimcode runtime install node
node    download  24.21.0   acquiring
$ dimcode runtime config python --source system --auto on
python  system    3.12.15   off
```

`dimcode runtime list` 每个运行时打印一行制表符分隔的字段，与面板显示的一致。哪些插件在等某个运行时是
`/plugins` 的答案（行客户端里是 `dimcode plugin list`），不是这条命令的职责：按插件列出的地方只有一处，
面板只就当前查看的那个运行时重复一次。
`install` 与 `config` 在调用后打印该运行时的行，即 `acquiring` 或它回到的状态；调用失败则打印错误并以
非零退出。`--help` 列出这些形态，且从不连接守护进程。

不提供 `runtime.list` 的守护进程会回答 `unknown_method`；此时面板说明该守护进程不提供运行时并打印版本，
而不是给一个读起来像"没有运行时"的空列表。

## 进程隔离设置

`/settings` 和 `/permissions` 提供同一个 `Process isolation` 控件，替代旧 `Execution` 面板和
独立 Sandbox 入口，不保留重复开关；没有原生 `dimcode isolation`、`/isolation` 命令或策略文件。
选项与布尔值的映射属于[进程隔离设计](../../design/host-platform-isolation.md#process-isolation)，
不属于 Permission。File access 和 Tool approval 都不改变它。

```text
Settings                         Process isolation
  File access          >       > Off
  Tool approval        >         Sandboxed · No network
  Process isolation    >         Sandboxed · Network allowed
                                 Applies to new processes and persistent shells.
```

打开控件时经 [`settings.read`](appserver/settings-service.md) 读取
`{source: "execution", scope: "user"}` 并保留 CAS revision。守护进程声明该方法时，
`host.execution.backend {}` 返回 `{available: true}` 或 `{available: false, reason}`；直接显示其原因，
不根据客户端平台推断可用性，选项仍可编辑。↑↓/Tab 移动，Enter 选择，Esc 返回。保存通过
`settings.update` 携带读取时的 revision；冲突时显示错误，不把用户选择自动重试到未见过的数据上。
Reload 丢弃草稿并重新读取。加载时禁止修改；不支持的方法报告不支持的守护进程错误，不回退到本地。
Reset 要求确认后把两个布尔值都写为 false。Off 可保留无效的旧 Network 值；两个沙箱选项各自显式写入
其 network 值。

这是用户作用域的宿主配置，不是分支策略，也不是运行中命令的边界。运行中的进程（包括常驻 shell）
保持启动边界。后端不可用时下次沙箱启动仍以 `isolation_unavailable` 拒绝；显示原因不削弱执行约束。
继续复用现有后端与 execution 来源。

获准写入该来源的模型可以像用户操作此控件一样改变下次进程的边界；写入不创建授权，也不改变运行中的
进程（[ADR 011 D2](../../adr/011-execution-settings.md)）。File access 不是 shell 代码的 OS 隔离。

## 管理界面

一个管理领域就是一个所有者的一组管理方法。App Server 经各自的所有者提供每个领域，不向客户端提供跨领域的通用通路（[Settings 服务](appserver/settings-service.md#11-不做什么)）：Skills 是 `skills.*`，Hooks 是 `hooks.*`，MCP 是 `mcp.*`，Scheduler 是 `schedule.*`，Workflow 定义是 `workflow.*`，命令规则是 `permission.rules` 与 `permission.set_rules`，插件是 PluginManager 的方法。它们的入口形态各不相同，并保持不变；本节只把客户端一侧做成同一个结构，使一个领域接入一次，而不是每个界面各接一次。

每个领域在原生客户端里有两个界面：

- 终端 UI 面板 `/<area>`，两种 TUI 模式都有，遵循[命令菜单契约](#命令菜单)；
- 子命令 `dimcode <area> <verb>`。

两个界面使用同一个领域适配器。`dimcode hooks list|enable|disable` 保持现有输出与退出码，只有来源的状态行与面板共用同一措辞；`dimcode hooks edit` 与 `dimcode hooks set PATH` 经同一个适配器写入。

管理文字在两个界面上都按界面语言生成。面板的文字从下一次回复起跟随 `/language`，`dimcode <area>` 使用已保存的语言，没有保存时使用系统的语言。

### 适配器

适配器经客户端连接与守护进程通信，所以子命令不需要 Session。它在边界处把所有者的回复转成同一个客户端视图，执行该领域的动作，并拒绝守护进程没有公布其方法的动作，给出「守护进程不支持」错误。守护进程没有公布读取方法时，拒绝只指出领域；公布了读取但没有该种类的方法时，拒绝指出种类：`This daemon lists scheduled jobs but cannot remove them.` 它从不回退到文件、别的方法或本机环境。

客户端不推断方法没有报告的东西。视图需要回复里没有的事实时，由生产者报告：`skills.catalog` 报告每个条目的 `source`，`hooks.catalog` 报告用户文档及其 revision。

比这些事实更旧的守护进程得到的领域缺少该事实：没有报告 `source` 的 Skill 列在领域自己的标题 `skills` 下，没有报告 `userDocument` 的 Hooks 不提供编辑。客户端从不从 id、路径或别的东西推导这类事实，所以也不需要靠特性串识别更新的契约。

### 视图

条目按所有者报告的分组归类——user、project、`plugin:<id>`、内置——顺序与所有者的报告顺序一致；一个 Hook 来源本身就是一个条目，归在 `sources` 分组下。每个条目有标签、详情、它的状态（已启用、已禁用、无效、被覆盖、只读）以及它提供的动作。所有者不允许客户端写入的条目（插件所有、已锁定）不提供写入动作，详情说明原因。所有者无法使用的东西作为 issue 列在条目之后。当 run 种类接受输入文档时，条目可以携带该操作的输入文档：Ctrl+T 先用外部编辑器打开它，run 随编辑器留下的文本发出。

```text
╭─ Skills ──────────────────────────────────────╮
│ >                                             │
│                                               │
│   user                                        │
│ › commit      enabled                         │
│   review      disabled  shadowed by commit    │
│   project                                     │
│   deploy      enabled                         │
│   Issues                                      │
│     /w/.agents/skills/old/SKILL.md: no name   │
│   Writes commit messages from the staged diff.│
│   /home/u/.agents/skills/commit/SKILL.md      │
╰──── Enter toggle · Ctrl+R reload · Esc close ─╯
```

### 动作

客户端有一组封闭的动作种类。每种在每个面板里都只有一个键，键提示按表格顺序列出该领域提供的种类，加上所选条目提供的种类。

| 种类 | 键 | 做什么 |
| --- | --- | --- |
| toggle | Enter 或 Space | 启用或停用该条目，与[执行设置](#执行设置)一致。 |
| edit | Ctrl+E | 用外部编辑器打开该领域的文档，再写回。 |
| reload | Ctrl+R | 重新读取该领域，或请所有者重新扫描，如 `skills.refresh`。 |
| add | Ctrl+N | 新建一个条目。 |
| remove | Ctrl+X | 确认后删除该条目，与 `/models` 清除已保存记录一致。 |
| run | Ctrl+T | 该领域的具名操作，例如 test、run now 或 start。 |
| inspect | Tab | 只读的详情视图。 |

Esc 关闭面板。动作键要么是 Ctrl 组合键，要么是筛选从不接收的键，因为筛选会接收所有普通字母。作用于领域本身的种类属于领域的视图而不是某个条目——reload，以及新建条目的 add——所以它不需要选中项：无论是否选中条目，键提示都会列出它，读取失败后 Ctrl+R 会重新读取该领域。最先承载的两个领域用 toggle、reload 和 edit。Scheduler 领域引入 add、remove、run 和 inspect，客户端写的就是这四个种类；之后的领域只需加入自己的适配器以及它所用种类调用的方法，并在此表中写下它新增的种类。如果领域给 run 种类起了名字，键提示就用那个名字：MCP 的提示是 `Ctrl+T test`。

底部边框放不下的键提示会画在朴素底边之上的几行里，在 ` · ` 分隔处换行；比一行还宽的部件按显示宽度换行，任何键和动作都不会被截掉。这些行与页脚一样从列表中占用空间，所以任何宽度下每个键都可见，删除确认也不例外。

- add 用外部编辑器打开适配器给出的新建条目文档——所有者的创建形态，填入示例值——并以领域的创建方法写入。文档没有改动时不写入；被拒绝时保留草稿，下次 Ctrl+N 重新打开用户的文本，与 Ctrl+E 对编辑的处理一致。冲突时也同样保留：新建没有 revision，丢弃草稿与过期 revision 的关联只是编辑的规则，不适用于新建。把条目追加到已读到列表的 add（如命令规则领域）改为重试冲突，因为追加不覆盖任何内容。`dimcode <area> add PATH`（`-` 表示 stdin）是不经编辑器的同一次写入。
- remove 先在键提示里询问：键提示变为 `Remove <label>? Enter confirm · Esc cancel`，除 Enter 外的任何键都取消且什么都不改。Enter 调用领域的删除方法，然后重新读取该领域。`dimcode <area> remove ID` 不提问直接删除，因为命令行上输入的 id 就是意图，与 `dimcode logout PROVIDER` 不提问直接退出一致。
- run 对所选条目调用领域的具名操作。答复会改变领域所列内容的运行——Scheduler 的立即运行会加入任务的历史和上次结果——重新读取该领域，并把答复作为消息报告；所有者跳过的一次运行会说明原因。答复不改变领域所列内容的运行，像 inspect 一样把答复作为详情显示，且不重新读取该领域：MCP 的 test 是临时连接发现的工具列表。所有者是否把该方法声明为 mutating 并不决定这一点，因为 `mcp.test` 会运行该条目，却仍然不写入领域所列的任何内容。需要参数的操作是该领域自己的步骤，不属于这个种类：条目携带输入文档，run 种类在调用前用外部编辑器打开它。
- inspect 向适配器索取条目的详情：一个标题和若干行，而不是一个视图。面板用它替代列表显示，方向键与 PageUp/PageDown 滚动，Esc 带着保留的选中项回到列表，再按一次 Esc 关闭面板。`dimcode <area> inspect ID` 打印同样的行，或该领域自己的动词，如果它的设计另有规定。

`/workflow:save` 让领域在面板的新建编辑器中打开：第一个视图到达时，面板像 Ctrl+N 那样在新建文档上打开编辑器；领域不提供 add 时，面板停留在列表，并显示支持检查给出的那句话。

文档按条目分存的领域，其条目为它自己的文档提供 edit：此时 Save 动作携带该条目的 id，适配器保留它与草稿一起读到的 revision。

保留的草稿属于它被取走时对应的写入目标：领域的文档、某个条目的文档，或一次新建。某个目标的编辑键只重新打开该目标自己的保留草稿，并带着它的文本来源的那个 revision；另一个目标的编辑打开它自己的文档，不动保留的草稿；也只有草稿自身写入的回复才会结算该草稿。

### 写入

写入遵循[执行设置](#执行设置)：

- 适配器保存它与草稿一起读到的 revision，revision 不是用户输入的字段；
- 冲突时什么都不写并显示错误；随后重新读取该领域，丢弃草稿与过期 revision 的关联，下次编辑打开已存储的文档。冲突后的读取失败同样丢弃该关联，并说明为何不绘制该领域；下一次读取会重试。它从不自动重试，也从不覆盖更新的文档；
- 重新加载丢弃草稿并重新读取；
- 冲突消息只陈述所有者的事实——什么变了、什么都没写——把用户文本在哪里留给持有它的界面：面板说明下一次编辑或新建会怎样处理保留的文本，并指出其文件；命令行指出保存文本的文件（包括来自 stdin 的输入）并以 1 退出；
- 与更新文档无关的拒绝——文本解析失败，或所有者拒绝该文本——保留草稿的 revision，所以编辑键会带着用户的文本再次打开并再次写入；
- 与草稿无关的回复——toggle、读取、其他领域的回复——同样保留草稿及其 revision；只有草稿自身的保存发生冲突、它所属领域重新加载，或草稿自身结算时才丢弃这一关联；
- 一个动作算不算写入，由它背后的方法决定：toggle、save 与 Skills 的 reload（`skills.refresh`），这些方法所有者都声明为 mutating；Hooks 的 reload 是读取，因为它在 `hooks.catalog` 之前不调用任何方法；
- 守护进程已接受的写入按已完成报告：其后的读取失败在动作之后作为读取失败报告，绝不算拒绝或冲突。CLI 先打印动作那一行，再在标准错误上打印读取错误并以 0 退出；Skills 与 Scheduler 的 toggle 按随后的读取命名条目，读取失败时写 id，因为名字来自读取；Hooks 的 toggle 写工作区，因为 toggle 就是项目来源；而单独的读取失败仍然是失败。面板显示动作的消息和读取错误，在下一次读取前不保留视图，并按成功结算草稿：删除其文件并丢弃其关联。已应用的动作即使其后的读取失败，也保留所有者的回答；
- 读取和写入期间禁止修改。

### 外部编辑器

- 编辑器依次取 `$VISUAL`、`$EDITOR`、平台默认值：`vi`，Windows 上为 `notepad`。Unix 上像 git 一样经 `sh -c '<editor> "$@"' <editor> <file>` 运行，所以带引号的值或含空格的路径都能用。
- 每次编辑写入自己的文件，独占创建且只有用户可读（Unix 上权限 0600），位于产品 home 下的临时目录（`<home>/edit/<area>-<unique>.json`），它既不是原生存储、不是 Web 工作区，也不被导出。写入成功或草稿没有变化时删除该文件；保留的草稿一直留到用户删除它，每条保留草稿的消息都说明它的路径。
- 编辑器运行期间终端 UI 挂起——退出原始模式和备用屏幕，结束后恢复——两种 TUI 模式都一样。
- 文件内容没有变化时不写入；编辑器以非零状态退出时同样不写入：消息说明退出状态和保留用户文本的文件。
- 文本解析失败或所有者拒绝时，显示错误，编辑键带着用户的文本再次打开编辑器。
- 交互式编辑需要终端。每次编辑都有一个非交互的孪生命令 `dimcode <area> set PATH`（`-` 表示 stdin），它用写入前紧邻读到的那次 revision 写入。
- 文档可以包含命令、环境变量和请求头。临时文件的暴露面与用户自己的设置文件相同，不会更多。

### 会话端口与唯一的面板

- 管理经会话端口上的一个通用请求和一个通用回复，各自携带领域与类型化的动作或视图：`Command::Management(ManagementRequest)` 与 `Update::Management(ManagementReply)`。
- 一个面板类型绘制所有领域。它建立在共享列表面板（[`ListPanel`](../../../crates/dim-cli/src/ui/list_panel.rs)）之上，确认、忙碌和消息状态只有一处。
- 新增一个领域意味着：一个适配器、它的命令菜单行、它的子命令动词和它的设计表格行。它不新增 `Prompt`、`Command` 或 `Update` 变体。
- `/plugins` 保留自己的面板。`/permissions` 保留面板与按键，其 Command rules 行打开下面的领域；三个控件是值的选择，不是条目，所以仍留在同一个面板的页面里。

```text
/skills ──► Command::Management { area, List } ──► 适配器
                                                    │ 方法已公布？ 否 ──► 不支持该守护进程的错误
                                                    │ skills.catalog
                          Update::Management ◄───────┴ 分组、条目、状态、动作、issues
面板  ◄───────────────────{ area, view }◄──────────┘
   │
   ├─ Enter ──► Command { area, Toggle { id, enabled } } ──► skills.set_enabled ──► skills.catalog
   │                                                                                （再次给出视图）
   └─ Ctrl+E ──► 外部编辑器 ──► Command { area, Save { id?, expectedRevision, document } }
                                ──► hooks.save ──► hooks.catalog

dimcode skills enable ID ──────────────────────────────► 同一个适配器，用自己的连接
dimcode hooks set PATH   ──────────────────────────────► 同一个适配器，用自己的连接
```

### 最先承载的两个领域

Skills（#814）：`/skills` 面板与 `dimcode skills list|enable|disable|reload`，经 `skills.catalog`、`skills.set_enabled` 与 `skills.refresh`；`/reload-skills` 不打开面板，直接运行 reload 动作并报告结果。条目按 id 启用或停用，所以 toggle 不需要自己的 revision。安装和删除 Skill 包仍归插件安装（[总览](overview.md#附录-d-cli-层)）所有。

Hooks（#809）：`/hooks` 面板与 `dimcode hooks list|enable|disable|edit|set PATH`。面板把每个来源列为一个带状态的条目；toggle 经 `hooks.set_enabled` 启用或停用项目来源（项目来源初始为停用，只有用户可以启用它）；edit 用编辑器打开用户文档，并以 `hooks.save` 写入，即 Settings 来源 `hooks` 的 CAS 写入。`dimcode hooks set PATH`（`-` 表示 stdin）是不经编辑器的同一次写入；revision 过期时两者都把用户文本留在 `<home>/edit/` 下它自己的文件里。用户来源提供 edit、不提供 toggle，因为它始终生效；插件来源两者都不提供，详情说明所属插件。

### Scheduler 领域

Scheduler（#815）：`/scheduled` 面板，`/schedule` 在两种模式下都是它的别名，以及 `dimcode scheduled list|enable ID|disable ID|edit ID|set ID PATH|add PATH|remove ID|run ID|history ID`，经 `schedule.list`、`schedule.save`、`schedule.delete`、`schedule.set_enabled`、`schedule.run_now` 与 `schedule.history`（[Scheduler §A.3](plugins/Scheduler.md#a3-管理方法)）。每个任务是条目：名称是标签，调度、下次窗口与上次结果分别是详情。所有者无法使用其调度的任务标记为无效，详情带上所有者的消息；已停用的任务说明没有下次窗口。所有者不报告分组，所以任务共用 `jobs` 一个分组。

任务的文档就是所有者写入的那条记录；客户端只改动 `schedule.save` 接受的字段——`name`、`schedule`、`prompt`、`cwd`、`model`、`mode`、`policy`、`max_run_minutes`、`enabled`、`session_id` 与 `agent_id`——并把 id 放在 Save 动作上、把任务的 revision 作为草稿读取时的 revision。`schedule.save` 是合并到已存任务上的补丁，所以一次编辑只写入草稿改动的成员：适配器重新读取该任务，草稿的 revision 不再等于任务的就拒绝，然后带着 id 与 revision 只发送改动的成员。没有改动任何可编辑成员的草稿不写入；草稿删除的成员无法清除，因为补丁没有 null：写入在发送前就被拒绝，并指出该成员。例外是 `mode` 变为 `create` 时被删除的 `session_id`，由所有者自己清除。revision 过期就是 `schedule_conflict`，即类型化的冲突：什么都不写，重新读取任务，下次编辑打开当前记录。`created_by`、`policy_cap` 与 `enabled_at` 由所有者推导，客户端从不发送。新条目从所有者的创建形态开始——一个名称、一个 `every` 示例调度、一个提示词与会话的工作区——以不带 revision 的 `schedule.save` 写入。`inspect` 就是 `history`：任务最近的运行，最新的在前，带所有者记录的状态。

```text
╭─ Scheduler ──────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ >                                                                                                                    │
│                                                                                                                      │
│   jobs                                                                                                               │
│ › nightly-build   enabled                                                                                            │
│   release         disabled  invalid                                                                                  │
│                                                                                                                      │
│   every 60 minutes                                                                                                   │
│   Next run 2026-11-02T09:00:00.000Z.                                                                                 │
│   Last run completed at 2026-11-01T09:00:12.000Z.                                                                    │
╰───── Enter toggle · Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Ctrl+T run · Tab inspect · Esc close ─╯
```

Ctrl+T 立即运行所选任务，并报告所有者的答复——`launching`，或运行被跳过的原因；Tab 用任务的历史替代列表显示：

```text
╭─ Recent runs of nightly-build ─────────────────────────────╮
│ 2026-11-01T09:00:12.000Z  completed                        │
│ 2026-11-01T08:00:05.000Z  skipped (overlap)                │
│ 2026-10-31T09:00:03.000Z  completed                        │
╰───────────────────────────────────── ↑↓ scroll · Esc back ─╯
```

`dimcode scheduled add PATH`（`-` 表示 stdin）是不经编辑器的创建；`dimcode scheduled history ID` 打印详情显示的那些行；`dimcode scheduled remove ID` 不经面板的确认直接删除。

### Workflow 领域

Workflow（#816）：`/workflow` 面板，`/workflow:save` 在两种模式下打开它的新建编辑器，以及 `dimcode workflow list|enable NAME|disable NAME|edit NAME|set NAME PATH|add PATH|start NAME [PATH]|history NAME`，经 `workflow.list`、`workflow.save`、`workflow.start` 与 `workflow.status`（[Workflow](plugins/Workflow.md#a3-管理方法)），再加上启用所需的 Settings 客户端方法。每个定义都是一个条目，按所有者报告的分组归类：`user`、`project` 与 `plugin:<包 id>`。用户定义始终启用，其文档就是条目的文档；项目定义默认停用，是仓库中的文件；包定义由安装提供，不提供写入。所有者无法使用的条目标记为无效，详情带上所有者的问题。

项目定义的启用状态是用户 Settings 文档中的 `projectEnabled[cwd][name]` 成员。所有者自己的服务拒绝修改它（`userOnly`），所以 toggle 是一次客户端写入：`settings.read`，在按读取原样复制的文档上设置该成员，再以读取报告的 revision 调 `settings.update`。文档按读取原样复制，因此定义、限额或另一个工作区的映射都不会丢失。`settings_conflict` 走冲突路径：什么都没有写入，重新读取定义，消息说明这一点。

add 用一个文档 `{source, document}` 写入任一来源，`source` 默认 `user`；项目保存创建或替换 `.dimcode/workflows/<name>.yaml`，并且保持停用，消息会说明。edit 只为用户定义提供：列表的 Settings 读取带上文档，保存带上面板读到的 revision，`conflict` 答复意味着编辑期间定义已变：什么都不写，重新读取领域，草稿保留用户的文本。CAS 单位是整个用户文档，所以编辑期间另一个定义或某个启用状态变了，保存同样返回 `conflict`。保存不得重命名定义：所有者以文档的 name 为键，重命名的编辑会保存第二个定义并留下旧的，而本领域没有 remove；写入在调用前被拒绝，草稿保留。项目定义在它所在的地方编辑，与项目 Hook 文档一样，条目的详情给出其路径。

run 为任何来源中已启用且有效的定义提供。声明了参数的定义会用编辑器打开一个 JSON 对象，每个参数一个键：定义给出 `default` 时用默认值，否则用空字符串。读回的文档必须是键都在定义声明之内的字符串对象；其他情况以 `Parameters are a JSON object of strings.` 拒绝，什么都不启动。空字符串是一个给定值，不是省略。没有参数的定义立即启动。启动带上条目的名称、来源与工作区；从面板发起时还带上 `report_to {session_id, branch_id}`，指向会话与转录所显示的那一条线，也就是提交消息所去的那条线，`/tasks` 下次打开时据此找到管理启动的运行。尚无会话的面板（例如 `/new` 之后、第一条消息之前）所启动的运行不向任何地方报告。命令行没有会话，所以 `dimcode workflow start` 不发送 `report_to`，其运行不在 `/tasks` 中。消息是 `Started <name>: <run_id>.`。在面板打开的 run 文档里，保持在打开时取值上的参数不会发送，由所有者应用定义自身的完整默认值；来自文件或 stdin 的文档则原样发送。

inspect 读取 `workflow.status {}`，把保留的运行过滤到该条目的名称与来源。项目定义属于一个工作区，所以它的运行还按本工作区的 `cwd` 过滤：另一个工作区的同名定义是另一个定义。用户或包定义在任何地方都是同一个，所以它的运行来自所有工作区，每行给出该运行的 `cwd`。详情是定义的行、一行 `Recent runs`，以及最多十条运行，最新的在前。本领域没有 remove：所有者没有删除方法，用户定义只能通过编辑 Settings 文档移除，项目定义通过删除其文件移除。

```text
╭─ Workflow ────────────────────────────────────────────────────────────────────────────────────╮
│ >                                                                                             │
│   user                                                                                        │
│ › review-and-fix   enabled                                                                    │
│   project                                                                                     │
│   nightly-check    disabled  invalid                                                          │
│   plugin:kit                                                                                  │
│   release-notes    enabled  read-only                                                         │
│                                                                                               │
│   Analyze, fix in parallel, summarize                                                         │
│   Saved in the user workflow source; always enabled.                                          │
╰─ Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+T run · Tab inspect · Esc close ─────────────╯
```

`dimcode workflow list` 每个定义打印一行（名称、来源、状态、描述）以及无效定义的问题；`enable` 与 `disable` 作用于该名称的项目定义；`edit` 与 `set` 作用于用户定义，遇到项目名称时给出要编辑的文件并拒绝；`add` 是不经编辑器的新建；`start NAME [PATH]` 从 PATH（`-` 表示 stdin）读取参数，没有 PATH 时不带参数启动；`history NAME` 打印详情的行。裸名称按 `workflow.start` 的规则解析：先找启用的定义，再找任何定义，每一轮都按 project、然后 user、然后 package 的顺序。项目定义默认停用，因此裸名称仍能指向它。在 Settings 不认识的工作区运行命令时，打印所有者的 `definition_source_failed` 消息并以 1 退出。所有者在一个载荷处截断的列表仍打印已有的定义，并说明后续定义被省略。
### MCP 领域

MCP 不新增种类。它的条目就是生效目录——各服务器按 id 合并，project 最后——按 `user`、`project`、`plugin` 的顺序分组，组内按 id 排列；条目的标签和 id 都是服务器的 id。面板与 `dimcode mcp` 用 toggle、edit、reload、add、remove、run 与 inspect，经 `mcp.catalog`、`mcp.configuration`、`mcp.update`、`mcp.save`、`mcp.remove`、`mcp.set_enabled`、`mcp.test` 与 `mcp.inspect`（[MCP 管理](appserver/mcp-management.md)）。

toggle 以 `mcp.catalog` 紧接写入前报告的 revision 写该来源的启用状态，所以比较并交换只覆盖这段窗口；它只作用于一个 id，不会覆盖来源中别处的改动。用户条目的启用状态是本地记录；项目条目一开始是停用的，只有 toggle 或 `dimcode mcp enable|disable` 会启用它。edit 先读 `mcp.configuration` 取得条目的 revision 与文档，再写 `mcp.update`，绝不用 `mcp.save`：save 接收完整配置，而读取会隐藏机密，往返一次就会把它们抹掉。

编辑文档就是 patch：省略的字段保留已存值，给出的值替换它，`env` 或 `headers` 只改动它点名的键。种子文档给出命令与工作目录、空的 `env` 与 `headers`，以及已配置 OAuth 的已报告字段；它从不给出已存参数、环境变量值、请求头值、Bearer 令牌或 URL，因为这些要么被隐藏，要么整体替换。文档不带 transport，因为已存条目的 transport 是固定的。stdio 的 patch 点名命令、工作目录与要改动的环境变量键：

```json
{"command": "npx", "cwd": "/work", "env": {"TOKEN": "value"}}
```

远端条目的 patch 对 endpoint 与请求头做同样的事。两者都不在种子里，所以想改它们的 patch 必须把值给出；省略 `url` 或某个 `headers` 键就保留已存值。

```json
{"url": "https://mcp.example.com/mcp", "headers": {"X-Key": "value"}}
```

add 用目录的来源 revision——`userRevision`，来源为 `project` 时用 `projectRevision`——写 `mcp.save`，并拒绝目录中已列出的 id，因为一次 save 会替换该条目。新建文档就是一个条目，带它的来源与 `mcp.save` 接收的定义：

```json
{
  "id": "example",
  "source": "user",
  "configuration": {
    "type": "stdio",
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-everything"],
    "env": {},
    "enabled": false
  }
}
```

两种远程形态把 `configuration` 换成 `{"type": "http", "url": "https://mcp.example.com/mcp", "headers": {}, "enabled": false}`，或把 `"type"` 换成 `"sse"` 的同样内容；静态凭据再加 `"auth": {"type": "bearer", "token": "…"}`，OAuth 则加 `"oauth": {"clientId": "…", "callbackPort": 1456, "scopes": "…", "authServerMetadataUrl": "…"}`；两者互斥，OAuth 还与 `Authorization` 请求头互斥。编辑器没有 transport 选择器，所以文档展示 stdio，并说明如何切换。

run 就是 `mcp.test`：它执行或联系已存条目一次，不写入领域所列的任何内容，所以答复是详情——临时连接发现的工具，标题为 `Tools of <id>`——且不再重新读取领域。inspect 就是 `mcp.inspect`：生效条目与它的会话连接，标题为 `MCP entry <id>`，从不写回。插件条目的配置没有 revision，所以不提供写动作，状态带 `read-only`，详情说明它属于某个插件；test 与 inspect 仍然可用。登录、退出、回调与断开连接是状态机或只作用于连接的动作，不是种类，所以它们仍是行命令与动词。

```text
╭─ MCP ────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│   user                                                                                                               │
│ › docs      enabled                                                                                                  │
│     user · stdio · signed in                                                                                         │
│     Executable: npx                                                                                                  │
│     Stored arguments (hidden): 2                                                                                     │
│     Working directory: —                                                                                             │
│     Environment names: DOCS_TOKEN                                                                                    │
│   project                                                                                                            │
│   metrics   disabled                                                                                                 │
│   plugin                                                                                                             │
│   notes     enabled  read-only                                                                                       │
╰───── Enter toggle · Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Ctrl+T test · Tab inspect · Esc close ─╯
```

### 命令规则领域

命令规则（#819）：Permissions 面板的 Command rules 行，以及 `dimcode permissions list|add PATH|remove RULE`，管理用户的命令规则（[权限控制](#权限控制)、[命令规则](appserver/permission-command-rules.md)）。所有者用 `permission.rules` 报告规则列表及其 Settings revision，并用 `permission.set_rules` 按该 revision 替换整个列表；revision 过期即为 `permission_rules_conflict`。

该行追加在 Process isolation 之后，现有行因此保持原位置。它打开本领域的面板，Esc 返回 Permissions 面板；它上面的三个控件保留各自的页面。

```text
╭─ Permissions ─────────────────────────────────────────────────╮
│ >                                                             │
│   File access          Workspace read/write                 › │
│   Tool approval        Ask (legacy tool exceptions)         › │
│   Process isolation                                         › │
│   Command rules                                             › │
╰──── ↑↓/Tab move · Enter apply · Esc back ─────────────────────╯

╭─ Command rules ───────────────────────────────────────────────╮
│ >                                                             │
│   user                                                        │
│ › allow exact cargo test -p dim-cli                           │
│   deny prefix git push                                        │
│                                                               │
│   Matches only this command.                                  │
│   Allows a simple command that would otherwise ask.           │
│   Applies in every Session, in every workspace.               │
╰──── Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Esc close ───╯
```

每条规则是一个条目。所有者没有为规则提供 id，所以 id 与标签就是规则的值，`DECISION MATCH COMMAND`——`allow exact cargo test -p dim-cli`、`deny prefix git push`。规则只有允许或拒绝的决定，没有开关状态，所以条目不上报启用状态（`enabled: None`），面板既不画 `enabled` 也不画 `disabled`；条目也没有 states，因为所有者无法使用的已存规则会让整次读取失败。领域提供三种动作：reload 重新读取 `permission.rules`（所有者每次调用都读取 Settings 来源，所以没有 rescan 方法），add 追加一条规则，remove 删除条目所指的规则。不提供 toggle、edit、run 与 inspect：规则没有开关，唯一一次写入替换整个列表，而一条规则的全部内容就是它的标签与两条详情。

Add 用外部编辑器打开一条规则，即所有者写入单元去掉 revision：

```json
{"command": "replace-with-a-command", "match": "prefix", "decision": "deny"}
```

命令是一个没人会保留的占位值，所以模板读起来就是示例：把文档原样保存不会写入任何内容（共享的创建规则），面板会说 `No change.`。`deny` 配 `prefix` 是失败关闭的示例：用户只替换命令，规则仍然拒绝。适配器把文档解码为命令规则，并用行内提示符的 `permission::rule` 校验（按词归一、命令非空、不含 shell 分隔符）；写入调用 `permission::add_rule`，也就是行内提示符自己的操作：读取 `permission.rules`，除非已有相等的规则否则追加，并按读到的 revision 写入。add 是唯一会重试冲突的动作，最多三次，因为追加不依赖其他规则，也不会覆盖任何内容。相等的规则不写入，并说明它已存在。`dimcode permissions add PATH`（`-` 表示 stdin）是不经编辑器的同一次写入。

Remove 先在提示栏询问，`Remove deny prefix git push? Enter confirm · Esc cancel`，然后按适配器读到的 revision 写入去掉该规则的列表。动作本身不携带 revision：规则的 id 就是它的值，所以 remove 作用在用户看到的那条规则上，面板打开期间其他规则的变化不算冲突。两种失败都什么都不写，并显示当前的规则列表：规则已不在列表中（`That command rule is no longer listed; nothing was removed.`），或另一客户端在读取与写入之间写入、所有者的答复是 `permission_rules_conflict`（`Command rules changed since they were read; nothing was removed.`）。`dimcode permissions remove 'deny prefix git push'` 把整条规则作为一个加引号的参数，不做确认直接删除，因为命令行上写出的规则就是意图。

领域只有用户作用域：Permission 只有一个用户 Settings 来源，没有项目规则，`.dimcode/permission.json` 不起作用。没有任何缓存：面板在打开时、Ctrl+R 时以及每次自己的写入之后读取列表，所以由"总是允许"、行内提示符或 Web 写入的规则会出现在下一次这样的读取里，而不会在面板空闲时出现。

`dimcode permissions list` 每行打印一条规则，用制表符分隔决定、匹配方式与命令，然后打印列说明以及 deny 的含义。`dimcode permissions remove` 把整条规则作为一个加引号的参数，因此全局选项不会产生歧义。

## 命令

| 入口 | 行为 |
| --- | --- |
| `dimcode tui` | 打开现有的交互式滚动回显客户端。 |
| `dimcode exec [-p TEXT] [--image PATH ...] [--json]` | 运行文本和/或本地图片后退出；没有 `-p` 时读取重定向的 stdin。 |
| `dimcode exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]` | 用文本和/或图片继续指定的已持久化 Session。 |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | 导出主分支；未给输出路径时写到 stdout。 |
| `dimcode session rename SESSION TITLE` | 经 `session.rename` 设置该 Session 的手动标题，并打印服务端返回的标题。 |
| `dimcode session rename SESSION --clear` | 清除手动标题，Session 回到无标题状态。 |
| `dimcode session delete SESSION` | 永久删除该 Session 及存储为它保留的一切；不弹出确认。 |
| `dimcode session cleanup (--older-than DURATION\|--keep N) [--yes]` | 列出某个选择器命中的 Session，确认后删除。 |
| `/rename TITLE` 或 `/rename --clear` | 设置或清除所显示 Session 的手动标题；该文本是命令，永不成为模型输入。 |
| `/export markdown PATH` 或 `/export json PATH` | 交互客户端空闲时把当前主分支导出到文件。 |
| `dimcode export SESSION_ID [--output PATH] [--database PATH]` | 不经守护进程，把 Session 的完整 Timeline 写成带版本的 JSONL（[Timeline 导出](appserver/timeline-export.md)）；未给输出路径时写到 stdout。 |
| `dimcode daemon vacuum [--database PATH]` | 用 SQLite `VACUUM` 重写离线原生库，回收已删除 Session 留下的页，并打印操作前后的大小（[存储格式](appserver/native-storage-format.md#sqlite-运行参数与空间回收)）；守护进程持库时失败，也不创建或升级库。 |
| `dimcode skills list` | 经 `skills.catalog` 列出工作区的 Skills：每个条目的名称、来源、状态、文件与描述，然后是目录的 issues。 |
| `dimcode skills enable\|disable ID` | 经 `skills.set_enabled` 启用或停用单个 Skill，然后再次报告目录。 |
| `dimcode skills reload` | 经 `skills.refresh` 重新扫描用户与项目根，并通过 [Skills 管理界面](#管理界面)报告结果。 |
| `/skills` | 打开会话所在工作区的 Skills 面板（[管理界面](#管理界面)）。 |
| `/reload-skills` | 不打开面板，直接为会话所在工作区运行 reload 动作并报告找到的条目与 issues。 |
| `dimcode hooks list [--cwd DIR]` | 经 `hooks.catalog` 列出生效的 Hook 目录（[Hooks](plugins/Hooks.md#a7-管理方法)）：每个来源的路径、启用状态与状态，以及每个处理器的事件、匹配器、命令与状态。 |
| `dimcode hooks enable\|disable [--cwd DIR]` | 经 `hooks.set_enabled` 启用或停用 `DIR`（默认当前目录）所在工作区的项目 Hook，记录在 Settings 为其授权的项目根上。 |
| `dimcode hooks edit` | 用 `$VISUAL`、`$EDITOR` 或平台默认编辑器打开用户 Hook 文档，并以 `hooks.catalog` 报告的 revision 经 `hooks.save` 写回；交互式编辑需要终端。 |
| `dimcode hooks set PATH` | 从 `PATH`（`-` 表示 stdin）读取用户 Hook 文档并写入，用写入前紧邻读到的 revision；内容没有变化时不写入。 |
| `/hooks` | 打开会话所在工作区的 Hooks 面板（[管理界面](#管理界面)）：每个来源一个条目，项目来源提供 toggle，用户文档提供 edit。 |
| `dimcode scheduled list` | 经 `schedule.list` 列出 Scheduler 持有的任务（[Scheduler](plugins/Scheduler.md#a3-管理方法)）：每个任务的 id、名称、计划、状态、下一次窗口与上次结果，然后是计划无法被 owner 使用的任务。 |
| `dimcode scheduled enable\|disable ID` | 经 `schedule.set_enabled` 启用或暂停单个任务，然后再次报告任务。 |
| `dimcode scheduled edit ID` | 用 `$VISUAL`、`$EDITOR` 或平台默认编辑器打开单个任务的文档，并以列表报告的 revision 经 `schedule.save` 写回；交互式编辑需要终端。 |
| `dimcode scheduled set ID PATH` | 从 `PATH`（`-` 表示 stdin）读取单个任务的文档并写入，用写入前紧邻读到的 revision；内容没有变化时不写入。 |
| `dimcode scheduled add PATH` | 从 `PATH`（`-` 表示 stdin）读取文档，不经 revision 经 `schedule.save` 创建任务：id、`created_by` 与 `policy_cap` 由 owner 推导。 |
| `dimcode scheduled remove ID` | 不经确认，经 `schedule.delete` 删除单个任务：命令行上的 id 就是意图。 |
| `dimcode scheduled run ID` | 经 `schedule.run_now` 立即运行单个任务，并报告 owner 返回的内容，或它跳过这次运行的原因。 |
| `dimcode scheduled history ID` | 经 `schedule.history` 打印单个任务最近的运行，最新的在前，并带上每次运行的状态与时间。 |
| `/scheduled`、`/schedule` | 打开 Scheduler 面板（[管理界面](#管理界面)）：每个任务一个条目，带计划、下一次窗口与上次结果，以及 toggle、edit、add、remove、run 与 inspect 种类；Scheduler 中 inspect 的动词是 history。任务属于主机，不属于会话或工作区；只有新建任务的默认 `cwd` 来自会话。 |
| `dimcode workflow list` | 经 `workflow.list` 列出定义（[Workflow](plugins/Workflow.md#a3-管理方法)）：每个定义的名称、来源、状态与描述，然后是无效定义的问题。 |
| `dimcode workflow enable\|disable NAME` | 经 `settings.read` 与 `settings.update` 启用或停用该名称的项目定义，然后再次报告定义。 |
| `dimcode workflow edit NAME` | 用 `$VISUAL`、`$EDITOR` 或平台默认编辑器打开用户定义的文档，并以用户文档读取时的 revision 经 `workflow.save` 写回；给出项目名称时改为给出要编辑的文件。 |
| `dimcode workflow set NAME PATH` | 从 `PATH`（`-` 表示 stdin）读取用户定义并写入，用写入前紧邻读到的 revision；内容没有变化时不写入。 |
| `dimcode workflow add PATH` | 从 `PATH`（`-` 表示 stdin）读取定义，经 `workflow.save` 创建或替换：文档给出 `source`（默认 `user`）与 `document`。 |
| `dimcode workflow start NAME [PATH]` | 经 `workflow.start` 启动定义，参数取自 `PATH`（`-` 表示 stdin），没有 PATH 时不带参数；报告 `Started NAME: RUN_ID`，不带 `report_to`。 |
| `dimcode workflow history NAME` | 经 `workflow.status` 打印定义的行与最近的运行，并按 `workflow.start` 的规则解析名称。 |
| `/workflow` | 打开会话所在工作区的 Workflow 面板（[管理界面](#管理界面)）：每个定义一个条目，带来源、状态、参数与问题，以及 toggle、edit、add、run 与 inspect 种类。 |
| `/workflow:save` | 在新建编辑器中打开 Workflow 面板。 |
| `dimcode mcp list` | 经 `mcp.catalog` 列出生效目录（[MCP 领域](#mcp-领域)）：每个条目的 id、来源、transport、启用状态与授权，然后是各来源 application 的错误。 |
| `dimcode mcp add PATH` | 以面板的新建文档（`-` 表示 stdin）经 `mcp.save` 创建条目，带 `mcp.catalog` 报告的来源 revision；目录已列出的 id 会被拒绝。 |
| `dimcode mcp edit ID` | 用 `$VISUAL`、`$EDITOR` 或平台默认编辑器打开条目的 `mcp.update` patch，并以 `mcp.configuration` 报告的 revision 写回；内容没有变化时不写入。 |
| `dimcode mcp set ID PATH` | 从 `PATH`（`-` 表示 stdin）读取 patch 并写入，用写入前紧邻读到的 revision。 |
| `dimcode mcp remove ID` | 不经确认，经 `mcp.remove` 以来源 revision 删除单个条目：命令行上的 id 就是意图。 |
| `dimcode mcp test ID` | 经 `mcp.test` 运行已存条目一次，并打印临时连接发现的工具；不保存任何内容。 |
| `dimcode mcp inspect ID` | 经 `mcp.inspect` 打印生效配置与会话连接，不联系服务器。 |
| `dimcode mcp disconnect ID` | 经 `mcp.disconnect` 关闭条目的活动连接；它不写配置，条目下次使用时重新连接。 |
| `dimcode mcp login\|logout\|enable\|disable ID` | 行为保持不变：浏览器或设备签到、退出，以及两个 `/mcp` 客户端共用的启用/停用。 |
| `/mcp` | 打开会话所在工作区的 MCP 面板（[管理界面](#管理界面)）：每个服务器一个条目，按 `user`、`project`、`plugin` 分组，带 toggle、edit、add、remove、test 与 inspect 种类。 |
| `/mcp list\|login\|callback\|logout\|disconnect\|enable\|disable` | 行命令，行为不变：`list` 打印目录与会话的服务器状态，`disconnect` 关闭一个条目的活动连接，其余用于签到、完成回调、退出或切换启用状态。 |

| `dimcode permissions list` | 经 `permission.rules` 列出用户的命令规则：每条规则的决定、匹配方式与命令，然后是列说明以及 deny 的含义。 |
| `dimcode permissions add PATH` | 从 `PATH`（`-` 表示 stdin）添加一条规则，经 `permission.rules` 与 `permission.set_rules` 写入：规则已存在时不写入；冲突时重新读取并重试，最多三次。 |
| `dimcode permissions remove RULE` | 删除 `RULE` 指定的规则，`RULE` 是作为单个加引号参数的整条 `'DECISION MATCH COMMAND'`；经刚读到的 revision 调用 `permission.rules` 与 `permission.set_rules`；规则已不存在或写入被并发修改时什么都不写，退出码 1。 |
| `dimcode login [PROVIDER] [--directory URL] [--device] [--redirect-uri URI]` | 启动被描述账号的浏览器或设备签到（[提供方账号](#提供方账号)）；不给 PROVIDER 时列出被描述的账号。 |
| `dimcode account PROVIDER` | 打印被描述账号的非机密状态。 |
| `dimcode logout PROVIDER` | 让被描述的账号退出。 |
| `/connect` | 列出 `provider.management` 报告的提供方；被描述的账号打开签到或退出步骤，其他提供方打开连接表单。 |
| `/exit` | 在空闲的交互提示符处断开并退出。 |
| `!command` / `!!command` | 以 record/private 模式调用 Session 现有的 `shell.run`，模型回合进行中也可用。 |
| `--permission read-only\|workspace-write\|full-access` | 为 `dimcode`、`tui`、`exec` 和 `exec resume` 选择新建或恢复的 Session 的预设（[权限控制](#权限控制)）。 |
| `/goal [OBJECTIVE \| set OBJECTIVE \| edit OBJECTIVE \| pause \| resume \| clear]` | 在行客户端和两种终端 UI 模式下读取并写入当前显示线的目标（[目标](#目标)）。 |
| `/permissions` | 打开 Permissions 面板：独立的 File access、Tool approval 与 Process isolation 控件，以及列出、添加、删除用户命令规则的 Command rules 行（[权限控制](#权限控制)）。 |
| `/approvals`、`/approval` | `/permissions` 的兼容别名，仍支持旧预设参数。 |

`dimcode hooks` 只写用户的 Hook 文档，而且只经 edit 这一种类：`dimcode hooks edit`、
`dimcode hooks set PATH` 以及 `/hooks` 里的 Ctrl+E 都调用守护进程所服务 home 的 Settings
来源 `hooks`（原生存储中为 `user/hooks`，独立 home 中为 `hooks.json`）上的 `hooks.save`。
项目文档是仓库中的 `.dimcode/hooks.json`，在仓库里编辑。`dimcode hooks enable|disable`
把该工作区记录在用户本地的 `hooks-projects` 中，所以项目文件不能启用自己
（[Hooks](plugins/Hooks.md#a7-管理方法)）。

全局的 home、工作区、server 和模型配置选项保持现有含义，放在命令前后都可以。`--server PATH`
指向独立的 `dim-app-server` 二进制，CLI 按它自己的名字直接运行；默认则运行本可执行文件的
`app-server` 子命令，因此指向本可执行文件是参数错误。指向不存在的路径、程序在开始服务前
退出，或未及时开始服务时，也按同一要求报错，除非它留下了[启动说明](#守护进程启动失败)。现有的
login/account/logout、models、MCP 和 daemon 命令仍然可用。现有的 `--prompt`、管道 stdin 和
`--resume` 调用行为不变；`exec resume` 也接受同样的全局选项。重复给出 Session 或提示词是参数错误。
`--json` 是 `exec` 的选项，`tui` 需要终端。help/version 与参数错误不会启动守护进程。

### 守护进程启动失败

所有启动守护进程的入口共用 `connect_inner` 里的客户端启动。在 Unix 上，守护进程只保留
从启动方继承的标准描述符 0-2，启动时关闭其余继承来的描述符，因此长期运行的守护进程
不会持有启动方的管道。Linux 用一次 `close_range` 调用关闭整个范围；该调用不可用时，
以及其他 Unix 系统上，守护进程列出 Linux 的 `/proc/self/fd` 或 `/dev/fd`，关闭列表中
的每个编号；连列表都读不到时，从 3 关闭到进程的描述符上限（依次取软限制、硬限制、
`sysconf(_SC_OPEN_MAX)`，都没有时取 65,536），因此没有哪条路径会留下
继承来的描述符。守护进程是 detached 的，
stderr 追加到 `daemon.log`，所以每次启动的一份说明是唯一既不解析历史、也不依赖客户端
可能关闭的管道的通道。启动前 CLI 删除 `<native home>/dimcode.sqlite.startup`；原生 home
是守护进程自己的 home，即 `<home>/rust`。子进程在客户端连上之前退出时，CLI 读取这份
说明（最多 16 KiB，且只接受普通文件；Unix 上还要求属于当前用户、组和其他用户都没有
权限）。Unix 上这套权限就是说明的保护，因为守护进程以 0600 写它；Windows 上 CLI 只
检查普通文件、大小和首行，边界是 home 目录继承的 ACL。CLI 要求首行是
`dimcode-startup-failure v1 pid=<child pid>`，去掉正文中除换行和制表符以外的控制字符，
把正文作为命令的错误返回，并在后面加上 `Details: <native home>/daemon.log`。存储门禁在
诊断出不兼容时写这份说明，写明库、标记、守护进程支持的格式、库是更新、标记与结构
不符、无法识别还是过旧，以及恢复办法（[原生存储格式 §
启动不兼容诊断](../../design/appserver/native-storage-format.md#启动不兼容诊断)）。
没有匹配的说明时，内嵌守护进程保留现有的 `App Server exited (<status>); see ...`，
`--server` 指定的程序则报出该选项必须指向独立的 `dim-app-server` 二进制。失败的启动读完后
删除说明；连接成功也删除任何说明。CLI 从不打印 `daemon.log`，从不删除、重建、降级或
改写库，并以 1 退出。开发说明继续使用 `./dev` 和它的隔离 home；终端说明本身只给兼容
构建或 `--home PATH` 两条路。

```text
$ dima
Error: Cannot open native store /Users/me/.dimcode-alpha/rust/dimcode.sqlite.
  Store format: v30
  Daemon format: v29
  This daemon cannot read the store. Existing data was preserved.
  Run a build supporting v30, or use --home PATH for a separate environment.
  Details: /Users/me/.dimcode-alpha/rust/daemon.log
```

交互式导出解析器读取格式词，把行的其余部分当作字面路径（包括空格），不做 shell 求值或引号展开。
缺少路径或格式不支持时打印用法并回到提示符，不提交输入。`/exit` 不带参数。只拦截这些确切的命令名；
其他文本走普通输入路径。子命令不建立插件命令注册表。

### 会话标题

手动标题是 Session 自己的状态：`session.list` 与 `session.summaries` 以 `manual` 来源报告它，
之后的自动生成永不替换它（[标题元数据](appserver/session-title-metadata.md)、
[SessionTitle](plugins/SessionTitle.md)）。`session.rename {sessionId, title}` 设置它，
`title: null` 清除它。标题文本不属于任何线，也不产生 Timeline 事件，所以重命名不增添模型读到的内容。

`session.rename` 是 Session 方法：客户端直接调用它，`hello.features` 并不列出它。不提供它的
Session 回答 `unknown_method`，客户端将其报告为不支持的守护进程错误，输入的文字绝不会
作为替代被提交。尚未附加 Session 时同理：没有显示中的 Session，`/rename` 直接说明并不发送任何内容。

`dimcode session rename SESSION TITLE` 打印服务端返回的标题，`--clear` 打印该 Session 已无标题。
TITLE 与 `--clear` 必须恰好给出一个，所以漏写参数是用法错误，绝不因此清除标题；空白或只含空格的
TITLE 视为缺失，也按同样方式拒绝。
`/rename TITLE` 与 `/rename --clear` 对所显示的 Session 做同样的事，并用一行状态说明发生了
哪一种。命令面板的这一行声明了标题参数，所以从面板选中 `/rename` 只是填入 `/rename ` 并等待
输入；对空的 `/rename` 回车会得到 `needs <title>` 提示且不发送任何内容，`--clear` 是唯一
无需标题即可清除的形式。两种形式都是客户端处理的命令：文本从不入队、从不提交，模型回合进行
中也是如此。`/resume` 列表每次打开都读取 `session.catalog`，在没有该目录的守护进程上退回
`session.summaries`，因此显示新标题；页头显示的是首条消息的主题，不受影响。空闲的行式客户端
读取同样的两种形式，对空的 `/rename` 打印 `Use /rename TITLE or /rename --clear`。

### 会话删除与清理

`dimcode session delete SESSION` 删除该 Session 及其存储为它保留的一切
（[Session 删除](appserver/session-deletion-and-fork.md)）。不弹出确认：命令行上给出的 id 就是意图。
回复说明本次调用是否移除了它（[`deleted`](appserver/session-deletion-and-fork.md#31-method)）：为
`true` 时打印 `Deleted ID`，为 `false` 时在 stderr 打印
`Nothing was removed: no stored Session ID.` 并以 1 退出。守护进程
在这个 id 下没有行，是一次什么都没移除的成功删除；这不是用法错误，命令行本身写错才是。回复不带该字段
（来自更旧的 App Server）时按一次删除处理：两种情况下行都已不在，也就是所要求的状态。

`dimcode session cleanup` 用同样的两个方法组合删除，符合
[§5.1](appserver/session-deletion-and-fork.md#51-cleanup-by-age-or-count) 的要求。它用
`includeClosed: true` 读 `session.summaries`，因此本守护进程关闭的 Session 也会列出，并在预览里
标为 `closed`：它的行仍在存储中，而别的列目录都不显示它
（[Session 目录](appserver/session-catalog.md#33-sessionsummaries)）。没有这个参数的旧版 App Server
会以 `invalid_params` 拒绝，而缺了它命令就看不到本守护进程关闭的 Session；因此命令在任何删除之前
停下，打印 `This App Server is too old to clean up Sessions safely; nothing was deleted.` 并以 1
退出。它在每次删除时带的 `requireInactive` 与 `includeClosed` 一同落地，所以拒绝其一的服务器两者
都拒绝；某次删除在尚未删除任何东西之前回答 `invalid_params` 时，也按同一方式停下。它用两个选择器之一，
两者都读 `session.summaries` 的 `updatedAt`，即 Session 的最后活跃时间：

| 选择器 | 选择范围 |
| --- | --- |
| `--older-than DURATION` | `updatedAt` 距今至少 `DURATION` 的每个 Session。`DURATION` 是数字加单位 `d`、`h` 或 `m`，例如 `30d`。 |
| `--keep N` | 保留最近活跃的 N 个 Session；其余全部选中。 |

清理只选 `/resume` 默认列出的 Session
（[分类](../../design/appserver/session-classification.md#35-catalog-visibility)）：来源为 `client` 或 `unknown`。
`internal` 的 Session
永远不会被选中：成员随其 leader 的删除一起消失，启动的 Session 属于它的插件。`acp` 的 Session
同样不会被选中；用 `dimcode session delete ID` 删除它。不被选中的 Session 既不占用 `--keep N`
的名额，也不出现在预览里。

轮次正在运行或仍有客户端附着的 Session 会被保留：预览在选择结果之后以 `Kept (active):` 列出这些
被跳过的 Session，列与选择结果相同，其后是原因（`running`、`N clients attached`），这样的 Session
一律不删除，`--yes` 也一样。失败或正在关闭的线不是保留的理由：线上没有工作，删除自身的关闭会结算它，
所以没有别的使用在用时，清理会删除这样的 Session。`--keep N` 仍然保留最近活跃的 N 个 Session；
比这 N 个更旧的活跃 Session
也保留，且不占用 N 中的一个名额。`dimcode session delete ID` 仍可有意删除一个活跃 Session：服务端
允许，这条规则属于批量命令。这个划分是预览对 `running`（只覆盖 `main`）与 `attachedClients` 的读取；
每个删除都带 `requireInactive: true`，所以所有者在删除时再查一次，逐条线，并把使用与 hold 都算进去。
被所有者以 `session_active` 拒绝的 Session 在删除之后以 `Kept (in use at the delete):` 列出，列相同，
其后是所有者的
原因；拒绝不是失败，不改变退出状态。回复为 `deleted: false` 的删除表示摘要读出之后另一个调用方已把该
Session 移除：命令打印 `Already gone: ID`，既不算作删除也不算作失败。回复中没有该字段则算作删除，与
`dimcode session delete` 的读法一致。选中的 Session 全部被保留时，命令打印
`Nothing to delete: every matching Session is active.` 并以 0 退出，无论预览把它们全部保留还是
所有者拒绝了每一次删除。

必须恰好给出一个选择器。命令在读取按键之前先打印选择结果——id、标题（无标题时 `-`）以及该 Session
最后活跃距今多久，每个一行，本守护进程关闭的 Session 在其后加 `closed`，此处以空格示意——确认之后
才删除：

```text
$ dimcode session cleanup --keep 1
a1b2c3d4-…  Notes on the build  3d ago
5e6f7a8b-…  -                   12d ago  closed
Kept (active):
c3d4e5f6-…  Fix the parser      1d ago   running
Delete 2 Sessions? [y/N] y
Deleted a1b2c3d4-…
Deleted 5e6f7a8b-…
```

在终端上只有 `y` 或 `yes` 确认。没有终端时任何输入都不算确认，脚本要传 `--yes`，它同时跳过提示；
否则命令拒绝并点名 `--yes`。确认后的运行即使某个 id 失败也会删除其余每个选中的 id：每个失败都连同
其 id 和方法错误一起报告，并为每个失败的 id 打印可重试的命令（`dimcode session delete ID`），
整次运行以 1 退出。选择器没有选中任何 Session 时，命令说明它保留了什么：`--keep N` 打印
`Nothing to delete: --keep N keeps every Session.`，`--older-than DURATION` 打印
`Nothing to delete: no Session is older than DURATION.`；两种情况都以 0 退出。
选中的 Session 全部活跃时打印上面那句。

P1 shell 前缀只用于交互输入。其执行、记录、进度、取消和生命周期遵循 [ShellInput](plugins/ShellInput.md)。
CLI 在发送请求前生成 `runId`，从不重试一次运行。前缀之后的命令原样传递；空命令打印用法。
命令挂起期间，再输入一行 shell 会提示该调用方已有一个运行。其他客户端的运行仍可能导致服务器的
`shell_busy` 错误。两种情况都不提交模型输入。执行期间输入的普通文本保持原生客户端的丢弃行为，
并提示等待下一个提示符；没有输入队列。挂起的审批独占其回答行，包括 `!`。

shell 运行期间的第一次 Ctrl+C 为客户端生成的标识发送 `shell.stop`，包括在被接纳之前。第二次中断
使用现有的模型回合停止并等待收尾；它从不跳过挂起的回复。EOF 在断开前停止调用方的 shell 运行和
模型回合。没有运行时，现有的中断行为适用。进度标为不完整；最终回复再次打印权威的有界输出、其状态、
截断情况以及记录的序号或记录错误。输出在终端边界过滤。RPC 错误仍是命令错误，从不成为模型文本。
调用方断开并不意味着运行已停止或其记录的输入丢失。

```text
> Explain the failing build
Assistant: Inspecting the build...
!cargo check
[shell started] record
[shell progress] (incomplete)
    Checking project
^C
[shell stopping] Waiting for command settlement
^C
[stopping] Waiting for resource settlement
[shell output] (final)
    Checking project
[shell stopped] ...
[shell recorded] sequence: 42

> !!printf private
[shell started] private
[shell output] (final)
private
[shell completed] exit: 0

> _
```

这些记录使用终端滚动回显，按终端宽度换行；没有第二个提示符、浮层或客户端进程。尽力而为的进度
可能被丢弃。最终输出完整显示，而不是从进度推断。

各导出入口的相对输出路径都以 CLI 进程的当前目录为基准，恢复后也一样；Session 的 cwd 和
`--home` 都不改变这个基准。CLI 按字面处理 `~`。用法/帮助写明：`PATH is relative to the CLI
current directory; ~ is literal; use an absolute path for another base`。调用方的 shell 可能在 CLI
收到参数前展开它。

其他目标命令依赖各自的功能。F6 不实现全屏界面、常驻 RPC/ACP 服务、验证/修复循环、Provider
编辑器或 Session 删除/分叉。`dimcode export` 是 [Timeline 导出](appserver/timeline-export.md)（ADR 005 D3）：每个最终模型请求都是其中的一行 `model/call`；功能清单中请求快照的过滤项尚未实现。

## 更新对话框

既有更新对话框在 fullscreen 和 scrollback 中使用相同的动作，并适应可用列数和行数。
较长的版本、命令和错误文字换行，必要时用 Up/Down 滚动。标题和动作行保持可见。
所有动作无法放在同一行时，只显示当前聚焦的动作及其序号；Left/Right 和 Tab/BackTab 仍循环
切换所有动作，Enter 确认，Esc 关闭。宽屏空间足够时保留既有确认布局。

```text
+- Update available --------+
| Version 0.12.0-alpha.124  |
| is available (you have   |
| > Later 3/3              |
| Left/Right Tab Enter Esc |
+----------- Up/Down 1-2/7-+
```

## 终端交互

```text
$ dimcode tui --home /tmp/dim-work
dimcode · model: provider/model
workspace: /workspace/project
session: <session-id>

> /export markdown
Usage: /export <markdown|json> PATH
PATH is relative to the CLI current directory; ~ is literal.

> /export markdown /tmp/Session notes.md
Exported Markdown: /tmp/Session notes.md

> /export json /tmp/events.json
Exported JSON: /tmp/events.json

> /exit
$

$ dimcode session export <session-id> --format markdown > notes.md
$ dimcode exec -p 'Explain this project' --json > run.jsonl
```

提示符保持键盘焦点，终端保留滚动回显。没有新的对话框、浮层或固定宽度的列。路径和错误信息使用
现有的终端文本过滤；窄终端正常换行。空 Session 导出空事件列表或只有标题的 Markdown，不编造消息。

## 执行与 JSONL 输出

`exec` 使用现有的 create/attach/history/submit/idle/detach 顺序和停止收尾。恢复时正在运行的回合
先收尾，再提交新提示词。作为已连接的交互客户端，`exec` 主动对审批请求发送普通的拒绝回复，停止
回合，等待停止收尾，并以退出码 3 退出。[问题](#问题)同样得不到回答：该问题所在的分支和回合被停止，
退出码为 3。它不改变 `SessionConfig.interactive`。以非交互方式创建的
Session 保留 Approval 现有的所有者侧策略（#98 D10）。`--permission` 选择包含文件范围和风险判定的旧策略；
仍需审批的调用照样被拒绝（[无头执行](#无头执行)）。其他退出码保持原生 CLI 合同，包括回合失败为 2、
中断为 130。传输失败不重试提交。重复的 `--image` 参数遵循无头附件合同：先文本，再按参数顺序放入
经过校验的原始图片字节，路径相对于调用进程。只有图片的调用是有效的。连接仍在时，提交错误保持
"结果不确定"的诊断/退出路径，除非其 `error.data.appError` 是七种
[保证在提交前拒绝](../../../crates/dim-app-server/README.md#session-submission-errors)之一：
`invalid_params`、`session_invalid_input`、`session_closed`、`session_build_failed`、
`service_reloading`、`unknown_method` 或 `input_blocked`。这些保留普通的错误诊断。其他错误，包括
`submission_unknown` 和不透明的发布/存储失败，都可能发生在提交之后，不得触发再次发送。

JSON 模式下，stdout 每行一条紧凑的 JSON-RPC 通知，使用守护进程现有的 `method` 和 `params`。只输出
所选 Session 的通知。协议握手、RPC 回复和其他 Session 不进入这个流。Session 句柄和给人看的诊断
写到 stderr；普通的助手/工具渲染不再同时写到 stdout。

```json
{"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"s","branchId":"main","requestId":"r","textOffset":0,"text":"Hello"}}
```

通知字段仍由现有的公开 v3 协议定义。实时观察从 attach 之后开始；存储的历史用于对账，不作为执行
输出转储。持久事件保留其序号和公开载荷，包括文本显示不渲染的字段。模型进度保留现有方法和偏移：
进度通知不是额外的持久文本事实。JSON 编码会转义控制字符；终端文本过滤不改写机器输出中的数据。
CLI 不添加合成的生命周期事件或第二套事件模式。通知投递保持守护进程现有的尽力而为语义；需要
固定完整记录的使用方应使用导出。

## 对话导出

两个入口都调用现有的 `session.history` RPC。第一次响应固定 `through`；后续分页使用该水位和服务器的
`nextAfter`，包括被过滤记录造成的空洞。CLI 复用现有的游标检查。导出不提交输入、不改变模型上下文、
不关闭 Session，也不实现服务端导出器。

导出的分支始终是 `main`；不获取也不包含子分支历史。Markdown 把每个 `branch/create` 事实连同子分支
id 渲染为 `Child branch <id> (contents not exported).`。JSON 原样保留该主分支事件。`branch/create`
在子分支构建之前记录请求。对 `branch/create-failed`，Markdown 用事件的 `branch` 字段为失败和取消的
创建都渲染 `Child branch <id> was not created.`。JSON 原样保留失败事实。不为判断结果而获取子分支历史。
读者可以区分创建失败与被省略的子分支内容。

JSON 输出包含 `{sessionId, branchId, through, events}`。每个事件都是公开的历史事件，不丢弃未知字段或
事件种类。这是对话导出，不是恢复/导入文件或原生数据库格式。Markdown 按历史顺序渲染用户/助手内容，
清楚标注推理、工具调用/结果和非文本附件。它不执行内容、不解析附件 URL、不重放工具。它保留代码围栏，
同时不允许一条消息闭合外层的工具结果围栏。

CLI 先成功读取固定前缀，再打开输出文件。文件使用"新建"语义：已存在的路径失败，不被替换。在 Unix 上
通过标准 OpenOptions 扩展以 0600 权限创建；Windows 使用原生继承的 ACL。从不为修改权限而打开已有文件。
历史读取出错后不创建输出文件。写入失败会被报告，可能留下部分新建的文件；不打印成功信息。导出到
stdout 失败时返回非零码。交互式导出错误回到输入提示符，从不成为模型输入。交互式导出已被接纳期间，
Ctrl+C 提示 CLI 正在等待结果，并持续消费后续中断直到操作收尾。这些中断不会在下一个提示符处变成退出
或清空草稿的动作。

## 权限控制

Permission 拥有由 `session.create {permissionPolicy}` 初始化、经带版本的
`session.permission.get/update` 更新的分支策略。独立可选字段及旧语义属于
[Permission 合同](../../architecture/rust-agent/plugins/Permission.md#policy-contract)，由
[ADR 017](../../adr/017-independent-permission-controls.md) 扩展。命令规则仍使用
`permission.rules`/`permission.set_rules`（[F2](appserver/permission-command-rules.md)）；文件授权仍使用
`session.grants.add/remove`（[ADR 012](../../adr/012-session-grants.md)）。

| TUI 控件 | 选项 | 策略字段或所有者 |
| --- | --- | --- |
| File access | Read only / Workspace read/write / Unrestricted | `fileAccess` |
| Tool approval | Ask / Bypass | `toolApproval` |
| Process isolation | Off / Sandboxed · No network / Sandboxed · Network allowed | 现有 `execution` 设置，不属于 Permission |

Ask 表示每次工具调用都需要同意，除非工具或命令规则显式允许；Bypass 只跳过工具询问。
两者都不回答文件范围问题、不解除显式拒绝，也不改变沙箱。File access 使用现有固定事实作为独立闸门：
Read only 即使对已允许的工具也拒绝写入；read 授权从不允许写入。同时需要范围和工具同意的调用只审批一次。
新 TUI 不把选项标为 Full access 或 Auto；Auto 留给未来真正的自动决策系统。

为兼容，未带显式覆盖的策略在工具列表与命令规则之后保留以下旧行为，文件检查仍优先：

| 预设 | 哪些会运行（Permission 现有的判定，在 `disallowedTools` 与 deny 规则之后） |
| --- | --- |
| `read-only` | 读取工作区、系统目录、Session 自有文件和已授权路径直接运行，其他读取需要询问；Session 工具运行；文件写入、命令、网络和未分类工具被拒绝 |
| `workspace-write` | 上述读取直接运行、其他读取需要询问；工作区内文件写入和 Session 工具运行；写入系统目录或 Session 自有文件被拒绝；其他工作区外写入、命令、网络和未分类工具需要询问 |
| `full-access` | 每个工具调用都直接运行，不询问 |

deny 规则在任何预设下都生效。allow 规则只把简单命令的询问变成运行，从不解除拒绝。产品和配置型
守护进程配置以 `workspace-write` 创建 Session；这个默认值不变。

### 启动时选择

`--permission read-only|workspace-write|full-access` 适用于 `dimcode`、`tui`、`exec` 和
`exec resume`，放在命令前后都可以。其他取值、重复给出或用在其他命令上，都是守护进程启动前的
参数错误。

终端 UI 在产品 home 的 `config.json` 中把独立 `fileAccess` 和 `toolApproval` 选择与旧
`permissionPreset` 一起保存。缺少的新字段仍保持缺少；加载旧配置不会把预设转换成显式 Ask，
也不放宽原有风险拒绝。这是后续 Session 的 CLI 偏好，不是所有客户端共享的守护进程默认值。
没有活动 Session 的主屏上，`/permissions` 无需登录或创建 Session 即可保存所选字段。
第一条消息使用记住的选择创建 Session，同一客户端进程内亦如此。有活动 Session 时，只有带版本的
`main` 策略更新成功后才记住选择。改变一个字段保留另一个字段、旧种子和工具列表。
Process isolation 则使用现有宿主设置来源。

```text
Home > /permissions > File access > Read only
  -> remember fileAccess (no Session or login required)
  -> first message -> session.create {permissionPolicy: {preset: <legacy seed>,
                                                       fileAccess: "read-only", ...}}
```

```text
$ dimcode exec --permission full-access -p 'Run the tests'
  hello
  session.create {cwd, permissionPolicy: {preset: "full-access"}}
  attach -> history -> submit -> idle -> detach

$ dimcode exec resume <session-id> --permission read-only -p 'Summarize'
  hello
  session.permission.get {sessionId, branchId: "main"}  -> {version: v, policy}
     预设已是 read-only：不写入
  session.permission.update {..., expectedVersion: v, policy: 只把预设换成 read-only}
     permission_conflict：重新读取并重试，最多 3 次，之后以 1 退出
  attach -> history -> submit -> idle -> detach
```

使用旧参数创建的新 Session 收到不带显式覆盖的 `{preset}`；该次运行的参数优先于记住的 CLI 选择。
新成员继承发起分支的当前策略（见分支语义）。恢复时仅更新预设只改变旧种子，保留所有其他策略字段，
包括显式覆盖与工具列表；显式覆盖仍优先于种子。通知必须显示其生效值，不能暗示改预设已替换显式控件。
更新发生在 attach 之前，所以本次调用提交的回合不会在旧预设下运行。没有参数或记住的选择时，
`session.create` 省略策略，恢复的 Session 保持自己的策略。应用记住的选择时显式发送，不重新解释历史。
守护进程没有声明 `session.permission.update` 时，恢复参数在 attach 之前失败。此处可重试冲突，因为
所请求的预设不取决于读取结果；读取只提供版本和要保留的工具列表。

生效的控件值显示在标识 Session 的位置。守护进程声明了 `session.permission.get` 时，交互启动读取策略；
`exec` 只在给出 `--permission` 时把它打印到 stderr，其他情况下现有诊断输出不变。实时到达 `main`
的 `permission/changed` 事件，无论来自本客户端还是配对设备等其他客户端，都打印一条带独立值与旧工具例外的记录。

总览和 Settings 使用 `Ask (legacy tool exceptions)`；紧凑 TUI scrollback 状态使用
`Ask (legacy)`，为设置保存通知留出 80 列内的空间。它表示相同的旧策略，不是显式 Ask，也不是另一选项。

```text
$ dimcode --permission read-only
dimcode · model: provider/model
local files: /workspace/project
session: <session-id>
File access: Read only
Tool approval: Ask (legacy tool exceptions)

> Fix the build
Assistant: ...
[permission] main: File access = Workspace read/write  <- 由其他客户端修改
```

滚动回显客户端在编辑器下方画一行状态行：代理标记与工具审批方式（旧策略下为 `Ask (legacy)`）、目标标记、
上下文对窗口的占用，以及一个显示当前状态的槽位——通知、需要回答或审批、`esc interrupt` 或后台任务数。
模型（放得下时连同推理强度）靠右对齐；整行按终端宽度截断。

### 无头执行

`exec` 保持其合同：审批请求得到普通拒绝，回合停止并收尾，退出码为 3。没有显式覆盖的旧策略中，
`full-access` 不询问但显式拒绝仍生效；`read-only` 拒绝写入和命令；`workspace-write` 对命令询问并以 3
退出。显式 Ask 也会对原本在范围内的读取询问；Bypass 仍保留文件范围审批。没有自动审批回答者。
参数是明确的旧策略选择，因此 `exec` 不要求确认。

### 分支语义

一个分支的 Permission 状态是 `SessionConfig.permissionPolicy` 依次叠加祖先前缀和自身的
`permission/changed` 事件。CLI 的权限入口只写 `main`；分支交互见 [Timeline branches](../../design/timeline-branches.md)。

```text
Session 创建配置 --> main 当前策略 --分支点--> 子分支策略
                         |                       |
                         +--新成员请求采样        +--新成员请求采样
                         v                       v
                    新成员权限               新成员权限

后续修改不更新已有子分支或成员。
```

- branch 工具在修改之后创建的子分支继承该修改。已有子分支保持自己的策略；CLI 不枚举也不更新它们。
  它们的审批请求仍像现在一样按各自的分支 id 回答。
- 新成员复制发起分支在采样点已派发的完整权限；已有成员保留自身权限，复用时也不刷新。
  采样顺序、失败和复用行为由[权限继承设计](../../design/subagent-tool-approval.md#2-permission-inheritance)定义。
  权限切换确认须说明其影响后续从该分支创建的成员，不更新已有成员；交付跟踪在 #257。
- 分叉复制源 Session 的事实，所以分叉从源 Session 当前的 `main` 策略开始（#26）。

### 子 agent 工具审批

审批转交、权限继承和取消恢复见 [Collaboration 设计](../../design/subagent-tool-approval.md)。
`ask` 的完整终端交互以 [CLI 审批设计](../../design/cli-commands.md#subagent-tool-approval)为准：
成员请求交给发起委派的 leader 分支；leader 的允许工具需要审批时，展示成员身份、双方分支、
原始工具和完整参数、原因，只提供允许一次或拒绝。详情从服务端读取，加载失败不能允许；
成员详情面板仍然只读，不直接回答成员请求。这是设计要求，不代表当前二进制已实现。

### `/permissions`

`/permissions` 是两种 TUI 模式下可见的菜单入口；`/approvals` 和 `/approval` 作为兼容别名打开同一组
控件，不另设预设选择器。控件作用于 `main`；每次字段选择使用当前完整策略与版本，只写所选维度。
保存失败既不改变记住的选择，也不回退到旧预设。

Process isolation 打开执行设置。Command rules 行打开[命令规则领域](#命令规则领域)的管理面板，在那里列出、添加和删除规则；Esc 返回本面板，该行上面的三个控件保留各自的页面。终端 UI 中 `/permissions` 后面的参数改为打开该面板，而不是被忽略；面板打开时就带着一行说明参数在哪里生效——行内提示符与 `dimcode permissions`——这行一直显示在面板的答复之上，直到用户在这里按下第一个键，无论首次读取要多久；紧凑语法属于行内提示符。

```text
Permissions (main branch)
> File access          Workspace read/write >
  Tool approval        Ask                  >
  Process isolation    Off                  >
  Command rules                             >

File access                         Tool approval
  Read only                       > Ask
> Workspace read/write              Bypass
  Unrestricted

Hint: Read only: no writes.          Hint: Ask before tool calls.
      Tool approval still applies.        File restrictions still apply.

↑↓/Tab move · Enter apply · Esc back
```

fullscreen 与 scrollback 中，三个控件都在所有选项与操作下方显示简短说明，中间空一行。
说明块以 `Hint:` 开头，各段另起一行，按词边界折行，续行对齐前缀后的内容；说明不是可选行，错误独立显示。

旧策略没有显式工具覆盖时，在适用处显示 `Ask (legacy tool exceptions)`，解释已有自动允许与风险拒绝，
不能假装已启用显式 Ask。仅打开面板不写入；显式选择 Ask 才替换旧工具风险行为，单选 File access 不会。
扩大文件访问或选择 Bypass 时说明显式拒绝仍有效、进程隔离不变，不声称每次调用都会运行。
审批提示不提供组合式放宽快捷入口。

行模式提示符下，无参数命令打印生效的独立控件与支持时的编号命令规则。预设参数只是兼容操作，
不是 TUI 标签。打印的规则与 revision 保留给 `remove`。

```text
> /permissions
File access (main branch): Workspace read/write
Tool approval (main branch): Ask (legacy tool exceptions)
Process isolation: Off
Command rules (user scope, every Session):
  1  allow  exact   cargo test -p dim-cli
  2  deny   prefix  git push

> /permissions deny prefix git push
Rule already present: deny prefix `git push`

> /permissions remove 1
Rule removed: allow exact `cargo test -p dim-cli`
Command rules (user scope, every Session):
  1  deny   prefix  git push
```

| 形式 | 调用 | 冲突 |
| --- | --- | --- |
| `/permissions` | 对 `main` 调用 `session.permission.get`；`permission.rules`；现有 execution 设置读取 | 无 |
| File access / Tool approval 选择 | `get` 后 `update`，保留未修改的策略字段 | 每次重试前重新读取；从不替换另一维度或工具列表 |
| `/permissions PRESET`（旧操作） | `get`；`full-access` 需要 `[y/N]` 确认，回车、其他回答、EOF 和 Ctrl+C 都表示否；然后用读取的版本和其余不变的策略 `update` | `permission_conflict`：重新读取，打印当前策略，不写入；由用户重新决定 |
| `/permissions allow\|deny exact\|prefix COMMAND` | `permission.rules`；除非已有相同规则，否则 `set_rules(revision, rules + rule)`，然后打印新列表 | 重新读取并重试，最多 3 次：添加不取决于其他规则 |
| `/permissions remove N` | `set_rules(列表的修订号, 去掉第 N 条后的列表)`，然后打印新列表 | `permission_rules_conflict`：重新读取，打印当前列表，不删除 |

`remove N` 针对最后打印的编号列表。列出规则、添加规则、删除规则以及删除冲突都会打印结果列表，只有打印过的
列表才会保留给 `remove`，所以编号不会指向用户没看到的位置。不打印列表的形式（规则已存在、切换预设、回合中
的"总是"回答）保留最后打印的列表；如果此后规则有变化，它的修订号会让下一次 `remove` 失败，冲突时打印当前
列表。本进程中还没有打印过列表时，`remove` 提示先运行 `/permissions`。`remove N` 保留编号，是因为它作用于本提示符打印过的那份列表并持有该列表的修订号；而 `dimcode permissions remove` 与面板取规则的值，因为它们各自在写入前才读取列表，没有可编号的打印列表。COMMAND 是该行的剩余部分；CLI 在发出任何调用前拒绝空命令或含分隔字符的命令
（`dim_agent_types::COMMAND_SEPARATORS`，即 F2 校验的字符集）。已经生效的预设不会再次写入。守护进程
没有声明相应方法时，命令报告该守护进程不提供它们。调用挂起期间，Ctrl+C 提示 CLI 正在等待结果，与
`/export` 相同。错误作为命令错误打印，从不成为模型输入。

### "总是允许"

以下条件全部满足时，审批提示提供 `a`；否则仍是原来的 `Allow once? [y/N]`：

- 调用是 `exec` 或 `execute`，即 F2 点名的两个工具（请求不带风险类别；为其他工具写的规则永远不会命中，
  所以失败的后果只是再问一次）；
- `call.input.command` 是至少含一个词、不含分隔字符的字符串；
- 守护进程声明了 `permission.rules` 和 `permission.set_rules`。

```text
[approval] exec: Tool `exec` requires approval for starting processes under the workspace-write preset
  command: cargo test -p dim-cli
Allow once, always, or deny? [y/a/N] a
Always allow `cargo test -p dim-cli` (exact). Press Enter, or type its leading words for a prefix rule: cargo test
[rule saved] allow prefix `cargo test`
[tool] exec
[done] exec
```

`y`/`yes` 允许一次，`a`/`always` 进入规则提示；其他回答（包括空行）都拒绝。在规则提示处，回车保留
精确规则；输入命令开头的若干词得到前缀规则（输入全部词得到精确规则）；其他文本会重新显示规则提示。
EOF 和 Ctrl+C 含义不变：EOF 拒绝并关闭输入，Ctrl+C 停止回合。

```text
 CLI                                    App Server
  | approval.request {exec, command}  <--|
  | 用户：a，规则的词                      |
  | permission.rules ------------------->|  {revision r1, rules}
  | permission.set_rules(r1, rules+rule)->|  {revision r2}
  |    permission_rules_conflict：重新读取并重试（最多 3 次）
  |    其他失败：[rule not saved] ...，然后对同一请求显示 "Allow once? [y/N]"
  | approval.respond allow ------------->|  Approval 重新判定；此后相同的调用
  |                                      |  由规则直接放行，不再询问
```

规则和回答是两次独立的写入。已存在的规则不会再写（`[rule present]`）。写入挂起期间请求被取消时，
CLI 报告规则的结果，不发送回答。Ctrl+C 之后不再发起新的写入：挂起的读取以 `[rule not saved]`
结束，由停止来收尾该请求。允许之后的重新判定（F2 §5.3）仍然适用，所以期间写入的 deny 规则优先。
命令文本经终端过滤后显示。

### "Allow for this session"

对于带 `path` 的 `read`、`write` 和 `edit` 调用，文件范围询问还提供 `Allow for this session`
（ADR 012）。Read only 或 Workspace read/write 文件访问下，外部读取需要范围同意。
仅因显式 Tool approval Ask 而询问的范围内调用不需要文件授权。
CLI 把工具给出的路径原样发给 `session.grants.add {sessionId, path, kind: "file", mode}`：`read` 的
mode 是 `read`，其余是 `write`。守护进程按 Session 工作区解析 `~`、相对路径、`..` 和符号链接并保存
规范路径；尚不存在的文件按父目录和最终名称解析。CLI 不在本地决定路径或 kind。

```text
[approval] read: Reading outside the session workspace requires approval: /home/user/.ssh/config
  path: /home/user/.ssh/config
› Allow once   Allow for this session   Reject
```

`Allow for this session` 先写入授权，再回答 `approval.respond allow`；重新判定会看到该授权。写入失败时 CLI
显示 `Grant not saved: <error>`，仍按用户要求本次允许，失败的存储写入不会阻塞回合。非交互运行
（`exec`）把事实之外的读取以普通拒绝结束，并以 3 退出；不增加新的错误路径。该提示不列出也不删除授权；
撤销尚无客户端入口。授权只回答文件范围，不回答显式 Tool approval Ask；当前答复仍对本次调用表示同意。
write 授权也允许读取，read 授权从不允许写入，分叉不继承源的授权。

### 决定

1. **记住的 CLI 选择不是宿主默认值。** 独立选择与旧配置一起保存，由 CLI 显式发送。
   Web、ACP、配对设备和自动化共同遵守的默认值属于守护进程用户 Settings：需求 R1。
   不得重新解释旧配置或已有策略来模拟此类默认值。
2. **不提供 `--allowed-tools`/`--disallowed-tools`。** 工具名属于守护进程配置，协议没有可用于校验的
   工具目录，拼错的名字会被接受却不起作用。`allowedTools` 还会对某个工具的每次调用绕过预设，而预设和
   命令规则已经能更窄地表达这些需求。CLI 每次更新都保留已有列表。
3. **CLI 只写 `main`**（见上文分支语义）。
4. **不提供 Shift+Tab 循环切换。** 按键解码器属于空闲草稿编辑器，它只返回发送或退出；运行中的回合读取
   普通行。循环切换需要编辑器返回动作结果和独立交互设计。`/permissions` 已覆盖切换。
5. **`/permissions` 也能添加规则。** F2 §8 把添加列为面板的一部分；在产品存储下，规则位于用户无法当作
   文件编辑的数据库行中，没有 `add` 就无法从 CLI 创建 deny 规则。
6. **只在结果不取决于读取内容时重试。** 启动预设、独立字段赋值和添加规则可在重新读取并保留未修改字段后
   重试。旧面板预设切换和按编号删除报告冲突，因为用户根据所见内容选择。Execution Settings 保持读取时
   revision 的保存合同。
7. **保留旧放宽预设确认。** 交互式旧 `full-access` 参数仍需确认；启动参数本身明确且可无头使用。
   新控件说明各自独立的效果，不提供该组合 TUI 选项。

### 范围之外

| 项目 | 需要什么 |
| --- | --- |
| 由审阅模型代替用户回答审批 | 一个通过 Approval 公开的回答操作作答的审批回答者，带自己的模型调用和设计，类似 Collaboration 的领导者路径；不是 CLI 改动 |
| 只在一个 Session 内记住的允许 | 一条记录下来的事实，或 Session 作用域的规则来源（F2 §10.2 暂缓了按工作区的用户规则）；CLI 进程内的记忆会在没有记录决定的情况下代为回答 |
| Shift+Tab 循环切换 | 只涉及 CLI：编辑器动作结果和确认路径（决定 4） |
| `--allowed-tools`/`--disallowed-tools` | 一个工具目录读取接口，用于在参数边界校验名字（决定 2） |
| 回合运行中使用 `/permissions` | 只涉及 CLI：运行中的回合读取普通行并回答审批；面板调用需要加入它的多路复用循环 |

本次改动中，ACP、Hooks 和 Web 的权限选择仍是仅使用旧预设的客户端。其现有预设控件不选择显式
File access 或 Tool approval，省略的覆盖字段保持旧风险判定。扩展这些 UI 属于独立工作，不在这里
另做一套 TUI 控件；Web 现有 execution 设置仍属于宿主设置。

### 规则检查

| 规则 | 回答 |
| --- | --- |
| 设计规则 §1（插件有界，App Server 只负责装配） | 没有新插件或 Capability；CLI 是现有方法的客户端 |
| 设计规则 §3（依赖方向） | CLI 依赖 `dim-agent-types` 中的 `PermissionPolicy`、`CommandRule` 和 `COMMAND_SEPARATORS`，与它已经依赖事件类型一样；没有部件依赖 CLI |
| §4 第 1 条（指令事件） | 策略修改经 Permission 现有的 `session.permission.update`，由它写 `permission/changed`；规则经其所有者的类型化写操作 |
| §4 第 2 条（交接） | 不变 |
| §4 第 3 条（before 回调） | 不使用 |
| §4 第 4 条（核心不带业务含义） | Timeline 和 Dispatcher 不变；独立策略语义属于 Permission，不属于 CLI |
| §4 第 5 条（批次） | 规则写入和审批回答是两个所有者各自的写入，由客户端按 F2 §5.4 规定的顺序执行 |
| §4 第 6 条（上下文） | 模型只看到现有的结果和拒绝 |
| §4 第 7 条（只经公开接口） | 只用公开方法和通知；CLI 从公开事件流读取 `permission/changed` |
| §4 第 8 条（基础设施不变） | 不改；下文向 App Server 所有者而非基础设施提出两项需求 |
| §4 第 9 条（只有需要的事实进 Timeline） | 没有新事件；规则不进 Timeline |
| ADR 002 D1 至 D5 | 没有新部件、接线或依赖；增量策略字段遵循 ADR 017，CLI 偏好是本地业务配置 |
| ADR 003 | 预设和规则只管 agent 工具调用；`!command` 不受影响；规则只有用户作用域，没有项目文件能放宽它们；重放、恢复和分叉不授予新权限 |

### 向机制所有者提出的需求

**R1. 新 Session 的用户默认预设。**
- 受阻：功能清单中的权限策略和工具审批设置需要一个供新 Session 使用的默认值。`session.create` 从
  守护进程配置固定的 `session_defaults` 取默认值（产品和配置型配置为 `workspace-write`），没有任何
  Settings 来源保存用户的选择。
- 考虑过：CLI 文件或环境变量（只有 `dimcode` 会遵守；其他客户端和自动化会以另一个默认值创建
  Session）；`permission` 来源中的字段（所有者正确，但会改变 F2 文档和 `session.create`，属于服务端改动）。
- 需要：一个用户作用域（从不是项目作用域）的值，请求省略 `permissionPolicy` 时由 `session.create` 使用，
  通过类似 `permission.rules` 的仅限本地、带修订号的方法管理。已有 Session 不会再读取它：其
  `SessionConfig` 在创建时就已固定。
- 序号、批次、中断、重载：无影响；只在创建时读取一次，不是事件。
- 所有者：App Server（Sessions 服务和 `permission` Settings 所有者）；跟踪于
  [#257](https://github.com/arcships/dim/issues/257)。

**R2. Agent 成员跟随领导者分支的当前策略。**
- 所有者：Collaboration；跟踪于 [#257](https://github.com/arcships/dim/issues/257)。
- 合同、采样顺序、失败和复用行为由[权限继承设计](../../design/subagent-tool-approval.md#2-permission-inheritance)拥有。
- CLI 只更新目标分支，不枚举成员或传播策略；确认文案须说明对新成员和已有成员的不同影响。

## 问题

模型的 `ask_user` 工具向用户提出一到四个选择题并等待回答，回答成为该调用的工具结果
（[Questions](../../architecture/rust-agent/plugins/Questions.md)，#262）。行模式 CLI 是负责回答的
客户端之一。它按到达顺序、一次一个调用地询问已连接 Session 所有分支上等待中的调用，并用
`session.question.respond` 回答：

```text
> Plan the service
[question] Database                             <- 标题，没有时为 "[question]"
1. Which database should the service use?
   1. PostgreSQL  managed, relational
   2. SQLite      single file
   0. Other (type your answer)                  <- 仅在允许自填时出现
Choose 1-2, 0 for other, empty to decline: 1
2. Which features ship first?                   <- multiSelect
   1. Auth
   2. Billing
   3. Search
   0. Other (type your answer)
Choose numbers, comma separated, 0 for other: 1,3,0
Other: audit log
[question answered]
[question] Deploy (branch reviewer)             <- 另一个分支的问题
1. Where to deploy?
   1. production
   2. staging
   0. Other (type your answer)
Choose 1-2, 0 for other, empty to decline:
[question answered elsewhere]                   <- 另一个客户端先回答了
```

CLI 从每个分支的 `step/tool-call` 和 `step/tool-result` 事件中找出等待中的调用：实时的
`session.event` 通知、连接时读取的 `main` 历史，以及其他每个分支的历史（在该分支第一次出现
`session.state` 通知时读取一次）。同一个事件可能既实时到达又出现在历史里，先后不定，所以调用和结果
按事件身份匹配。调用 id 可能在之后的模型请求中再次出现，所以一个调用由它的分支、其 `step/tool-call`
的 `request_id` 和调用 id 共同命名、结算和回答：

```text
session.event（实时，所有分支）──┐
session.history（连接时读 main， ├──> 等待中的 ask_user 调用 ──> 一次一个提示
  其他分支在首个 state 时读取）──┘    step/tool-call 加入调用        │
                                      step/tool-result 移除调用      v
                                                       session.question.respond
                                                       {branchId, requestId, callId,
                                                        answers | declined}
```

第一个问题输入空行表示拒绝回答整个调用；后续问题输入空行或无效内容会重新询问。问题打开期间，审批、
模型输出和 shell 结果都要等待，与审批提示时一样；已打开的审批也会让问题等待。

| 回复或事件 | CLI |
| --- | --- |
| `{}` | `[question answered]` |
| `invalid_params` | `[question rejected] <原因>`；从该调用的第一个问题重新询问 |
| 调用结果到达前的 `question_not_pending` | `[question not ready; press Enter to send again]`；按 Enter 重发同一个回答 |
| 调用结果到达前的其他失败 | `[question error] <错误>; press Enter to send again` |
| 回复之前或尚未发送时读到该调用的 `step/tool-result` | 结果是回答或拒绝时打印 `[question answered elsewhere]`，否则打印 `[question cancelled]` |
| 问题提示时按 `Ctrl+C` 或 EOF | 对问题所在分支发送 `session.stop`（该分支上的其他调用也随之结束），回合仍在运行时也停止回合；与审批提示时一样，不会先停止正在运行的 shell 命令 |

没有终端输入时（`exec` 或重定向的提示词）没有人能回答。CLI 打印一次
`Interactive answer required; stopping. Rerun in a terminal to answer it`，停止退出前读到的每个问题
所在的分支，回合仍在运行时也停止回合，并以 3 退出。退出码 3 现在表示"需要交互式审批或回答"。

### 终端 UI 回答问题

终端 UI 复用既有 Questions 客户端模块和提示面板，不增加第二份 waiter 投影或协议。
每个分支的已提交 call/result 沿已有固定水位历史与实时事件合并流程读取；响应始终携带原
Session 地址及 `(branchId,requestId,callId)`。

```text
 [ Database ] [ Features ] [ Confirm ]
 Question on branch reviewer
 Which features ship first?
   [x] Auth
   [ ] Billing
       Type your own answer: audit log
 Space toggle · Enter next · Tab switch · / commands · Esc decline
```

复用既有选择、确认和窄屏窗口：数字键选择，Space 多选切换，Tab/Shift+Tab 或左右键切题，
Confirm 上 Enter 提交。只有 allowCustom 为 true 时显示自定义输入，并与所选标签分开提交。
自定义行输入非空文字即勾选（单选时替换已选标签），清空则取消；编辑中 Esc 保留文字退出编辑，
Enter 保留并进入下一题。编辑之外 Esc 拒答整次调用。显示脱敏不改变提交给 owner 的原始标签，composer 草稿保持。

问题标签页无法放在同一行时，只显示当前标签及其位置，包括 Confirm 页。
Tab/Shift+Tab 和 Left/Right 保留既有切换行为；标签文字不能超出提示框边界。

挂起的问题不阻塞命令。其自定义回答未进入编辑时，`/`、Ctrl+O 和 Ctrl+P 在问题之上打开命令面板，
画在该模式画命令面板的位置，问题保留标签页、选择和已输入的回答。命令面板开启命令输入状态，
按键一直留在 composer，键入命令的参数并显示在 composer 行上；Enter 与回复进行中一样执行命令，
命令输入持续到这次 Enter、Esc，或草稿不再是命令为止。在命令面板中以及在输入参数时按 Esc
都结束命令输入、把按键交还给问题，不拒答问题，也不中断正在运行的回合。
会打开自身面板的命令把问题暂存，面板关闭时带回答题及其状态；
期间 Session 已停止询问的问题不再带回。进入自定义行编辑之前 `/` 打开命令面板，因此回答本身可以以路径开头：
Enter、Space、该行编号或任何其他输入字符进入编辑行，在其中输入的 `/` 是文本。
其余场合 Esc 语义不变：有问题时拒答，无面板且运行中时中断，没有时间窗口。

发送期间禁止重复提交；成功回复或已提交的批次 result 关闭提示。invalid/not-pending/其他
失败保留回答和诊断，由 Confirm 显式重试；Esc 可重试拒答。result 先于迟到回复时以 result 为准；后续复用
callId 或同一模型请求中的其他调用不复用旧提示状态。审批/问题沿已有模态顺序；leader 问题
出现后按既有清理流程关闭成员详情，成员自己的问题不能变成 leader 的回答控件。
Ctrl+C 保持 TUI 既有中断/退出规则；实际中断时，先停止问题所在分支及仍在运行的 leader
回合，不先停止另行运行的 shell。行模式及无终端输入的策略不变。

[CLI 合同](../../../crates/dim-cli/README.md#questions-in-the-terminal-ui) 记录具体交互。
验收通过真实 TUI/NDJSON 边界覆盖结构化答案、重试、重复 identity、分支恢复、成员隔离和
草稿保留；实际 App Server/模型流程验证下一次模型请求通过 ToolResult.content 读到回答。

## 实现与验证

改动限于 `crates/dim-cli`：`args.rs`/`main.rs` 选择命令，`session.rs` 复用 Session 执行及其共享的
分页历史读取器，`display.rs` 把给人看的渲染与 JSON 输出分开，`export.rs` 格式化对话并写新文件。
`permission.rs` 存放任何 CLI 客户端都能调用的权限操作，不读终端、不做输出：带版本检查地读取和更新
`main` 的策略，带修订号检查地列出、添加和删除命令规则，以及"总是允许"的各个步骤（可用的命令、建议的
规则、为多路复用连接的回合循环拆成请求与回复步骤的规则写入，以及允许答复）。`permission_command.rs`
是行模式的 `/permissions` 命令，审批提示留在 `session.rs`；两者只负责展示结果。`question/mod.rs` 存放
任何客户端处理问题都需要的部分，不读终端、不做输出：`ask_user` 的输入规则、由事件推导出的等待中调用、
`session.question.respond` 的参数以及回复的含义。`question/line.rs` 是行模式的问题提示，由
`session.rs` 把通知和回复交给它。对话格式化属于 CLI。独立控件使用 Permission 的增量策略合同
（ADR 017）与现有 execution Settings 合同；不需要改变核心生命周期、存储迁移或运行时注册。

复用 `cli_flow.rs` 及其真实守护进程配置夹具，覆盖 `exec` 和恢复、stdout/stderr 分离、公开 JSON 通知的
保留、回合/审批失败和停止收尾、固定水位的多页导出、两种格式、拒绝已存在文件以及交互命令拦截。覆盖
CLI 与 Session 目录不同时的相对路径、字面 `~` 用法、子分支省略/标记、创建失败或取消的子分支，以及
Unix 文件私密性。只使用隔离的 home。实现需要通过相关的 CLI 流程测试和受影响部分的 Clippy。

在两个边界验证权限控制。复用脚本化守护进程流程检查 `session.create` 的策略、恢复时带冲突重试的
读取/更新、参数错误、`/permissions` 列表、旧别名与确认、冲突报告、添加和删除规则并在每次之后打印新的编号
列表、连续两次删除时第二个编号针对第一次打印的列表、带前缀规则的"总是"提示、冲突重试、写入失败后回退
为允许一次、已存在的规则不再写入、写入期间 Ctrl+C 放弃规则，以及含分隔符的命令不提供"总是"。
针对构建出的 App Server 的流程（CI 通过 `DIM_TEST_APP_SERVER` 运行）应检查：
`exec --permission full-access` 不询问就运行进程工具；默认的 `exec` 仍以 3 退出；`exec resume
--permission` 修改恢复的分支；"总是"写入精确规则，之后相同的调用不再询问，直到 `/permissions remove`
删除该规则。

独立控件验收场景：显式 Ask 对范围内读取询问；工具允许只回答工具问题；Bypass 仍对范围外文件询问；
Read only 即使工具已允许也拒绝写入；同时需要范围和工具同意时只询问一次；read 授权不能允许写入；
Bypass 下显式拒绝仍优先；旧策略和配置保留精确旧行为，不静默写入。验证每次选择只改自己的字段、
策略成功更新后才记住、Session 创建前也可使用，冲突或不支持服务器时不放宽。两种 TUI 的菜单只显示
`/permissions`，两个别名可用，Process isolation 没有重复 Execution/Sandbox 入口。复用进程流程验证
布尔映射、CAS 失败、后端不可用原因，以及切换隔离前启动的进程保持原边界。这些是要求，不表示实现或验证已完成。

问题由脚本化守护进程流程（`cli_flow/question.rs`）覆盖：单选和多选、自填文本、拒绝回答、无效输入行、
处理器会拒绝的输入和其他工具不会触发询问、与审批双向的延后、等待中的 shell 结果、重复使用的调用 id、
之后的请求重用同一调用 id 且实时先于含有前一个调用及其结果的历史页到达、从两页历史中读到并以其
`branchId` 回答的子分支问题、重复的实时事件、上表中的每种回复、在回复之前或期间关闭提示的结果、回合收尾前后的 Ctrl+C 和 EOF、Ctrl+C 之后被丢弃的回复，以及 `exec` 在 `main`
上、子分支上和回合收尾之后遇到问题时以 3 退出，而先于调用读到结果时不会询问。
