# CLI 命令与导出

F6（#77）覆盖[总体设计附录 D](overview.md#附录-d-cli-层)和[功能清单](../../feature-list.md#cli-commands)
列出的命令入口、对话导出和 JSONL 执行。原生 CLI 仍是现有守护进程的客户端。本文定义实现所依据的
CLI 接口；当前可执行文件由[原生 CLI 合同](../../../crates/dim-cli/README.md)描述。

[附件输入合同](../../design/cli-attachment-input.md)定义空闲草稿编辑器、本地图片来源和粘贴处理。
多行/粘贴编辑器、无头 exec 的图片参数、剪贴板/文件插入、补全和拖入路径已实现。附件合同记录了
各原生平台的验收证据和尚未完成的检查。

## 命令

| 入口 | 行为 |
| --- | --- |
| `dimcode tui` | 打开现有的交互式滚动回显客户端。 |
| `dimcode exec [-p TEXT] [--image PATH ...] [--json]` | 运行文本和/或本地图片后退出；没有 `-p` 时读取重定向的 stdin。 |
| `dimcode exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]` | 用文本和/或图片继续指定的已持久化 Session。 |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | 导出主分支；未给输出路径时写到 stdout。 |
| `/export markdown PATH` 或 `/export json PATH` | 交互客户端空闲时把当前主分支导出到文件。 |
| `/exit` | 在空闲的交互提示符处断开并退出。 |
| `!command` / `!!command` | 以 record/private 模式调用 Session 现有的 `shell.run`，模型回合进行中也可用。 |
| `--permission read-only\|workspace-write\|full-access` | 为 `dimcode`、`tui`、`exec` 和 `exec resume` 选择新建或恢复的 Session 的预设（[权限控制](#权限控制)）。 |
| `/approvals`（别名 `/permissions`） | 交互客户端空闲时查看、切换和管理预设与命令规则。 |

全局的 home、工作区、server 和模型配置选项保持现有含义，放在命令前后都可以。现有的
login/account/logout、models、MCP 和 daemon 命令仍然可用。现有的 `--prompt`、管道 stdin 和
`--resume` 调用行为不变；`exec resume` 也接受同样的全局选项。重复给出 Session 或提示词是参数错误。
`--json` 是 `exec` 的选项，`tui` 需要终端。help/version 与参数错误不会启动守护进程。

交互式导出解析器读取格式词，把行的其余部分当作字面路径（包括空格），不做 shell 求值或引号展开。
缺少路径或格式不支持时打印用法并回到提示符，不提交输入。`/exit` 不带参数。只拦截这些确切的命令名；
其他文本走普通输入路径。子命令不建立插件命令注册表。

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

两个导出入口的相对输出路径都以 CLI 进程的当前目录为基准，恢复后也一样；Session 的 cwd 和
`--home` 都不改变这个基准。CLI 按字面处理 `~`。用法/帮助写明：`PATH is relative to the CLI
current directory; ~ is literal; use an absolute path for another base`。调用方的 shell 可能在 CLI
收到参数前展开它。

其他目标命令依赖各自的功能。F6 不实现全屏界面、常驻 RPC/ACP 服务、验证/修复循环、Provider
编辑器、Session 删除/分叉或模型请求快照导出。`dimcode export` 保留给另一项请求快照功能。

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
回合，等待停止收尾，并以退出码 3 退出。它不改变 `SessionConfig.interactive`。以非交互方式创建的
Session 保留 Approval 现有的所有者侧策略（#98 D10）。`--permission` 只改变哪些调用需要审批；
仍需审批的调用照样被拒绝（[无头执行](#无头执行)）。其他退出码保持原生 CLI 合同，包括回合失败为 2、
中断为 130。传输失败不重试提交。重复的 `--image` 参数遵循无头附件合同：先文本，再按参数顺序放入
经过校验的原始图片字节，路径相对于调用进程。只有图片的调用是有效的。连接仍在时，提交错误保持
"结果不确定"的诊断/退出路径，除非其 `error.data.appError` 是六种
[保证在提交前拒绝](../../../crates/dim-app-server/README.md#session-submission-errors)之一：
`invalid_params`、`session_invalid_input`、`session_closed`、`session_build_failed`、
`service_reloading` 或 `unknown_method`。这些保留普通的错误诊断。其他错误，包括
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

App Server 已经拥有判定工具调用所需的一切：在 `session.create {permissionPolicy}` 时确定、用
`session.permission.get/update` 修改（每个分支有版本）的分支预设，以及用
`permission.rules`/`permission.set_rules` 读取和替换的用户命令规则（[F2](appserver/permission-command-rules.md)）。
CLI 过去只能用 `y` 逐次回答审批。本节在这些合同之上增加 CLI 入口，并实现
[F2 §5.4](appserver/permission-command-rules.md#54-总是允许客户端流程)的客户端流程。App Server、
Permission、Approval、事件、协议和存储都不变。

| 预设 | 哪些会运行（Permission 现有的判定，在 `disallowedTools` 与 deny 规则之后） |
| --- | --- |
| `read-only` | 读取和 Session 工具运行；文件写入、命令、网络和未分类工具被拒绝 |
| `workspace-write` | 读取、文件写入和 Session 工具运行；命令、网络和未分类工具需要询问 |
| `full-access` | 每个工具调用都直接运行，不询问 |

deny 规则在任何预设下都生效。allow 规则只把简单命令的询问变成运行，从不解除拒绝。产品和配置型
守护进程配置以 `workspace-write` 创建 Session；这个默认值不变。

### 启动时选择

`--permission read-only|workspace-write|full-access` 适用于 `dimcode`、`tui`、`exec` 和
`exec resume`，放在命令前后都可以。其他取值、重复给出或用在其他命令上，都是守护进程启动前的
参数错误。

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

新 Session 在 `session.create` 中收到完整策略 `{preset}`：不带工具列表，这是产品配置的形式。Agent
成员复制这份创建时策略（见分支语义）。恢复时该参数只替换预设，保留已存储的
`allowedTools`/`disallowedTools`。更新发生在 attach 之前，所以本次调用提交的回合不会在旧预设下运行。
不给参数时，`session.create` 不带策略，恢复的 Session 保持自己的策略。守护进程没有声明
`session.permission.update` 时，恢复时的参数在 attach 之前失败。这里重试冲突是正确的：要设置的预设
不取决于读到的内容，读取只提供版本号和要保留的工具列表。

预设显示在标识 Session 的位置。守护进程声明了 `session.permission.get` 时，交互启动会读取它；
`exec` 只在给出 `--permission` 时把它打印到 stderr，其他情况下现有诊断输出不变。实时到达 `main`
的 `permission/changed` 事件，无论来自本客户端还是配对设备等其他客户端，都打印一条记录。审批原因里
已经写明当前生效的预设。

```text
$ dimcode --permission read-only
dimcode · model: provider/model
local files: /workspace/project
session: <session-id>
permission: read-only

> Fix the build
Assistant: ...
[permission] main branch preset: workspace-write     <- 由其他客户端修改
```

没有状态行：滚动回显客户端没有常驻区域，编辑器的提示行是按终端宽度截断的固定帮助文本。

### 无头执行

`exec` 保持其合同：审批请求得到普通的拒绝回复，回合被停止并收尾，退出码为 3。`--permission` 只改变
哪些调用会询问。`full-access` 下没有询问；`read-only` 下写入和命令把 Permission 的拒绝结果交给模型，
不询问；`workspace-write` 下命令仍会询问并以 3 退出。没有自动批准。参数本身就是明确的选择，所以
`exec` 不再要求确认。

### 分支语义

一个分支的 Permission 状态是 `SessionConfig.permissionPolicy` 依次叠加祖先前缀和自身的
`permission/changed` 事件。CLI 只写 `main`，也就是它提交输入、等待空闲和导出的分支。

```text
session.create {permissionPolicy}  -->  SessionConfig.permissionPolicy
        |                                         |
        v                                         v  创建成员时复制
 main: permission/changed（按版本 CAS）     agent 成员 Session（Collaboration）
   ^  由恢复时的 --permission
   |  和 /approvals 写入
   +-- 分支点 --> 子分支（branch 工具）：分支点处 main 的策略，
                   之后是它自己的修改
```

- branch 工具在修改之后创建的子分支继承该修改。已有子分支保持自己的策略；CLI 不枚举也不更新它们。
  它们的审批请求仍像现在一样按各自的分支 id 回答。
- Agent 成员复制领导者 Session 的创建时策略。新 Session 上的 `--permission` 会传到成员；之后对
  `main` 的修改不会。切换的确认信息会说明这一点，下文的需求 R2 请 Collaboration 改为复制领导者分支的
  当前策略。
- 分叉复制源 Session 的事实，所以分叉从源 Session 当前的 `main` 策略开始（#26）。

### `/approvals`

`/approvals` 及其别名 `/permissions` 与其他命令一样在空闲提示符处运行，需要纯文本单行输入。不带参数时
读取 `main` 的预设，并在守护进程声明了 `permission.rules` 时读取命令规则并编号打印。打印出的规则及其
修订号会保留，供 `remove` 使用。

```text
> /approvals
Preset (main branch): workspace-write
  read-only        reads run; writes, commands, network and unclassified tools are denied
* workspace-write  reads and file writes run; commands, network and unclassified tools ask
  full-access      every tool call runs without asking
Command rules (user scope, every Session):
  1  allow  exact   cargo test -p dim-cli
  2  deny   prefix  git push
Usage: /approvals [PRESET | allow|deny exact|prefix COMMAND | remove N]

> /approvals full-access
Full access runs every tool call, including commands, without asking.
Switch the main branch to full-access? [y/N]
Preset unchanged: workspace-write

> /approvals read-only
Preset (main branch): read-only. Child branches created later inherit it; agent members keep the Session's starting preset.

> /approvals deny prefix git push
Rule already present: deny prefix `git push`

> /approvals remove 1
Rule removed: allow exact `cargo test -p dim-cli`
Command rules (user scope, every Session):
  1  deny   prefix  git push
```

| 形式 | 调用 | 冲突 |
| --- | --- | --- |
| `/approvals` | 对 `main` 调用 `session.permission.get`；`permission.rules` | 无 |
| `/approvals PRESET` | `get`；`full-access` 需要 `[y/N]` 确认，回车、其他回答、EOF 和 Ctrl+C 都表示否；然后用读到的版本和工具列表 `update` | `permission_conflict`：重新读取，打印当前预设，不写入；由用户重新决定 |
| `/approvals allow\|deny exact\|prefix COMMAND` | `permission.rules`；除非已有相同规则，否则 `set_rules(revision, rules + rule)`，然后打印新列表 | 重新读取并重试，最多 3 次：添加不取决于其他规则 |
| `/approvals remove N` | `set_rules(列表的修订号, 去掉第 N 条后的列表)`，然后打印新列表 | `permission_rules_conflict`：重新读取，打印当前列表，不删除 |

`remove N` 针对最后打印的编号列表。列出规则、添加规则、删除规则以及删除冲突都会打印结果列表，只有打印过的
列表才会保留给 `remove`，所以编号不会指向用户没看到的位置。不打印列表的形式（规则已存在、切换预设、回合中
的"总是"回答）保留最后打印的列表；如果此后规则有变化，它的修订号会让下一次 `remove` 失败，冲突时打印当前
列表。本进程中还没有打印过列表时，`remove` 提示先运行 `/approvals`。COMMAND 是该行的剩余部分；CLI 在发出任何调用前拒绝空命令或含分隔字符的命令
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

### 决定

1. **CLI 不设默认预设。** 新 Session 的默认值是用户的宿主配置，所有创建 Session 的客户端都必须遵守：
   除 `dimcode` 外还有 Web、ACP、配对设备和自动化。CLI 文件或环境变量会让默认值随客户端而不同，
   并成为日后需要迁移的第二个来源。持久的位置是守护进程的用户 Settings，由 `session.create` 读取，
   这是服务端改动：需求 R1。在此之前，用 `--permission`（或 shell 别名）逐次选择预设。
2. **不提供 `--allowed-tools`/`--disallowed-tools`。** 工具名属于守护进程配置，协议没有可用于校验的
   工具目录，拼错的名字会被接受却不起作用。`allowedTools` 还会对某个工具的每次调用绕过预设，而预设和
   命令规则已经能更窄地表达这些需求。CLI 每次更新都保留已有列表。
3. **CLI 只写 `main`**（见上文分支语义）。
4. **不提供 Shift+Tab 循环切换。** 按键解码器属于空闲草稿编辑器，它只返回发送或退出；运行中的回合读取
   普通行。循环切换需要编辑器返回动作结果，并在编辑器内完成 full-access 确认。`/approvals` 已覆盖切换。
5. **`/approvals` 也能添加规则。** F2 §8 把添加列为面板的一部分；在产品存储下，规则位于用户无法当作
   文件编辑的数据库行中，没有 `add` 就无法从 CLI 创建 deny 规则。
6. **只在结果不取决于读取内容时重试。** 启动参数和添加规则会重试冲突；面板中的切换和按编号删除会报告
   冲突，因为用户是根据显示的内容做的选择。
7. **只有交互式切换到 full-access 需要确认。** 启动参数本身就是明确的选择，并且可用于无头执行。

### 范围之外

| 项目 | 需要什么 |
| --- | --- |
| 由审阅模型代替用户回答审批 | 一个通过 Approval 公开的回答操作作答的审批回答者，带自己的模型调用和设计，类似 Collaboration 的领导者路径；不是 CLI 改动 |
| 只在一个 Session 内记住的允许 | 一条记录下来的事实，或 Session 作用域的规则来源（F2 §10.2 暂缓了按工作区的用户规则）；CLI 进程内的记忆会在没有记录决定的情况下代为回答 |
| 在 CLI 中设置沙箱 | F5 只提供 `host-isolation.json`，没有读写方法；需要 HostPlatform 的管理方法，带修订号、仅限本地，类似 `permission.rules` |
| Shift+Tab 循环切换 | 只涉及 CLI：编辑器动作结果和确认路径（决定 4） |
| `--allowed-tools`/`--disallowed-tools` | 一个工具目录读取接口，用于在参数边界校验名字（决定 2） |
| 回合运行中使用 `/approvals` | 只涉及 CLI：运行中的回合读取普通行并回答审批；面板调用需要加入它的多路复用循环 |

### 规则检查

| 规则 | 回答 |
| --- | --- |
| 设计规则 §1（插件有界，App Server 只负责装配） | 没有新插件或 Capability；CLI 是现有方法的客户端 |
| 设计规则 §3（依赖方向） | CLI 依赖 `dim-agent-types` 中的 `PermissionPolicy`、`CommandRule` 和 `COMMAND_SEPARATORS`，与它已经依赖事件类型一样；没有部件依赖 CLI |
| §4 第 1 条（指令事件） | 策略修改经 Permission 现有的 `session.permission.update`，由它写 `permission/changed`；规则经其所有者的类型化写操作 |
| §4 第 2 条（交接） | 不变 |
| §4 第 3 条（before 回调） | 不使用 |
| §4 第 4 条（核心不带业务含义） | Timeline、Dispatcher 和 Permission 不变；工具名只出现在 CLI 判断是否提供"总是"的条件中 |
| §4 第 5 条（批次） | 规则写入和审批回答是两个所有者各自的写入，由客户端按 F2 §5.4 规定的顺序执行 |
| §4 第 6 条（上下文） | 模型只看到现有的结果和拒绝 |
| §4 第 7 条（只经公开接口） | 只用公开方法和通知；CLI 从公开事件流读取 `permission/changed` |
| §4 第 8 条（基础设施不变） | 不改；下文向 App Server 所有者而非基础设施提出两项需求 |
| §4 第 9 条（只有需要的事实进 Timeline） | 没有新事件；规则不进 Timeline |
| ADR 002 D1 至 D5 | 没有新部件、接线、依赖、配置或合同 |
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
- 受阻："成员在领导者的策略下工作"只在领导者修改预设之前成立。Collaboration 复制领导者 Session 的
  `SessionConfig.permissionPolicy`，所以执行 `/approvals read-only` 之后，新成员仍以创建时的预设启动。
- 考虑过：由 CLI 更新每个成员（它看不到成员的创建，并会与之竞争）；只通过参数创建 Session（覆盖不了
  之后的修改）。
- 需要：Collaboration 创建成员时，复制领导者分支已派发的 `PermissionState.policy`（Permission 的公开
  状态），而不是创建时的配置。
- 顺序：成员采用其创建时已派发的策略；领导者之后的修改不影响已有成员，与子分支相同。
- 所有者：Collaboration（App Server）；跟踪于 [#257](https://github.com/arcships/dim/issues/257)。

## 实现与验证

改动限于 `crates/dim-cli`：`args.rs`/`main.rs` 选择命令，`session.rs` 复用 Session 执行及其共享的
分页历史读取器，`display.rs` 把给人看的渲染与 JSON 输出分开，`export.rs` 格式化对话并写新文件。
`permission.rs` 存放任何 CLI 客户端都能调用的权限操作，不读终端、不做输出：带版本检查地读取和切换
`main` 的预设，带修订号检查地列出、添加和删除命令规则，以及"总是允许"的各个步骤（可用的命令、建议的
规则、为多路复用连接的回合循环拆成请求与回复步骤的规则写入，以及允许答复）。`permission_command.rs`
是行模式的 `/approvals` 命令，审批提示留在 `session.rs`；两者只负责展示结果。对话格式化属于 CLI。
不需要改动 App Server、Capability、核心生命周期、协议、存储迁移、依赖或运行时注册。

复用 `cli_flow.rs` 及其真实守护进程配置夹具，覆盖 `exec` 和恢复、stdout/stderr 分离、公开 JSON 通知的
保留、回合/审批失败和停止收尾、固定水位的多页导出、两种格式、拒绝已存在文件以及交互命令拦截。覆盖
CLI 与 Session 目录不同时的相对路径、字面 `~` 用法、子分支省略/标记、创建失败或取消的子分支，以及
Unix 文件私密性。只使用隔离的 home。实现需要通过相关的 CLI 流程测试和受影响部分的 Clippy。

权限控制在两个边界上都有覆盖。脚本化守护进程流程检查 `session.create` 的策略、恢复时带冲突重试的
读取/更新、参数错误、`/approvals` 列表、切换确认、冲突报告、添加和删除规则并在每次之后打印新的编号
列表、连续两次删除时第二个编号针对第一次打印的列表、带前缀规则的"总是"提示、冲突重试、写入失败后回退
为允许一次、已存在的规则不再写入、写入期间 Ctrl+C 放弃规则，以及含分隔符的命令不提供"总是"。
针对构建出的 App Server 的流程（CI 通过 `DIM_TEST_APP_SERVER` 运行）检查：
`exec --permission full-access` 不询问就运行进程工具；默认的 `exec` 仍以 3 退出；`exec resume
--permission` 修改恢复的分支；"总是"写入精确规则，之后相同的调用不再询问，直到 `/approvals remove`
删除该规则。
