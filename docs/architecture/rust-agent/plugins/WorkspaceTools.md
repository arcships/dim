# WorkspaceTools

Session 的宿主工具：执行 `read` `write` `edit` `glob` `grep` `exec`。负责路径解析、文本文件保真、同一文件的写入串行、经 [HostPlatform](HostPlatform.md) 启动进程、前台进程的终止回收、输出截断与落盘。不裁决权限（调用到达这里前已由 [Approval](Approval.md) 准入）、不知道沙箱、不持有分支状态。

<!-- generated:plugin WorkspaceTools — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:FileWriteCoordinator`、`P:HostPlatform`、`S:SessionConfig`、`S:SessionIdentity`、`S:TreeCommands` |
| 提供 | `S:ToolHost` |
| 被注入于 | `ToolRegistry` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `P:HostPlatform` | `home`、shell、环境与 `spawn`；exec 定义中的平台文案 |
| `P:FileWriteCoordinator` | `write`/`edit` 按规范路径的 FIFO 写锁；进程级（`FileWriteCoordinatorPlugin` 由组合根在进程作用域注册、重载时克隆沿用），同一文件跨 Session 串行 |
| `S:SessionConfig` | `cwd`（`session.create` 已校验并规范化）；`collaboration_role` 为成员时 exec 不转后台 |
| `S:SessionIdentity` | 会话输出目录 `<output_root>/<session_id>/` |
| `S:TreeCommands` | 每次调用按 `context.branch_id` 取调用方分支，再查它的 [FileReads](FileReads.md) 与 [BackgroundExec](BackgroundExec.md) |

## 构造输入

`workspace_tools::install(builder, WorkspaceToolsConfig { output_root })`：登记 6 个宿主工具名，装配本 Plugin 与分支上的 FileReads、BackgroundExec。`output_root` 是绝对路径，由组合根给出。

## 服务接口

`S:ToolHost`（`ToolDriver`）：`definitions()` 返回 6 个工具定义；`execute(call, context, cancel)` 按工具名分派。工具的名称、描述、schema、`risk`、输出文本、错误码与上限见下文，有意的取舍见[为什么](#为什么)。

| 工具 | risk | 分支部分 |
| --- | --- | --- |
| `read` | `read_filesystem` | 成功后在 FileReads 记录绑定 |
| `write` | `write_filesystem` | 覆盖已有文件需要本分支的绑定；成功后更新绑定 |
| `edit` | `write_filesystem` | 需要本分支的绑定；成功后更新绑定 |
| `glob`、`grep` | `read_filesystem` | — |
| `exec` | `execute_process` | 转后台时交给 BackgroundExec |

调用方：[ToolRegistry](ToolRegistry.md) 把它包装成工具处理器，[ToolExecutor](ToolExecutor.md) 准入后调用。

## 文件工具

- 路径：`~` 展开；Windows 路径修正（`/cygdrive/c`、`/c/` 等），拒绝驱动器相对路径；拼到 cwd 上，词法去掉 `.`/`..`，再对最长存在前缀取 realpath。
- 文本保真：`read` 识别 BOM、UTF-8/UTF-16、显式 `encoding` 指定的旧编码（`encoding_rs`）和换行风格，写成绑定；`write`/`edit` 按绑定编码写回，保留 BOM 与换行风格。
- 并发：同一规范路径的 `write`/`edit` 按 FIFO 串行；锁来自进程级的 `P:FileWriteCoordinator`，跨 Session 共享、重载后沿用同一实例，因为文件是物理共享的。锁一直持有到写入的阻塞 I/O 与绑定更新完成；等待中的调用可被取消。
- 冲突：绑定记录原始字节的 SHA-256，写入前比对，不一致返回 `file_modified_since_read`；没有绑定返回 `text_binding_required`。`write` 新建文件以独占方式创建，检查之后被别人建出的同名文件不覆盖，返回 `text_binding_required`。
- `read` 流式读取：按块解码并对全文算 SHA-256（写入前的修改核对需要覆盖整个文件），只保留 `offset` 起的窗口行；单行与总量都受 256K 字符预算限制，窗口之外只计行数与换行风格。`offset` 极大时返回 `offset_out_of_range`，不溢出。
- 请求的首行超过预算时，只返回行首并在正文开头说明截断，使落盘预览也能看到警告；不提示从下一行续读，避免跳过本行未展示的尾部。后续整行因预算未展示时，仍按行号提示续读。
- 只读写普通文件：打开不阻塞（`O_NONBLOCK`），并在打开后的句柄上核对类型；成功打开的 FIFO、设备等返回 `not_regular_file`（`read`、`edit`，以及 `write` 覆盖前的核对）。socket 可能在打开阶段就被操作系统拒绝，此时返回对应的打开错误（如 `read_error`），不保证是 `not_regular_file`。
- `read` 超过 30,000 字符：完整结果写到会话输出目录 `read-<callId>-<uuid>.txt`，返回路径与前 2,000 字符预览，结构化结果带 `offloaded`、`outputPath`。落盘成功后才登记读取绑定：落盘失败时模型没有看到正文，不能随后覆盖该文件。
- `read` 的 `offset` 先按打开后文件的大小核对：每行至少占 BOM 之后的一个字节，`offset` 超过正文字节数时不流式读取、不算哈希，直接返回 `offset_out_of_range`；没有正文的文件仍按空读取成功。
- `grep` 的匹配位置按字符边界计算列号（字节模式可能落在多字节字符中间）；例如 `café` 中匹配 `é` 的第二个字节返回列 4，而不是把截断的字节前缀按替换字符解码得到的列 5。`include` 不是合法 glob 时，文件和目录作用域都返回 `invalid_input`，不当作没有匹配或后端故障。
- `glob.pattern` 的末尾分量必须包含至少两个连续字面字符，例如 `*.mjs`、`**/*.rs`；裸 `*`、`**`、`**/*`、前导 `./`、绝对路径、`..`、花括号和反斜杠均不支持。工具定义与输入错误明确给出约束和可用例子；搜索目录通过 `path` 指定。
- `edit`：`newText` 一律按字面插入，不展开 `$&` 等 JS 替换模式。目标大小先按元数据核对，打开（核对文件类型）后先核对读取绑定，没有绑定不读内容；读取按块进行并在块间响应取消，上限按实际读到的字节核对（最多读上限加一字节），读取期间变大的文件同样返回 `edit_target_file_too_large`。
- `glob`/`grep` 用 ripgrep 的 Rust 库（`ignore`、`grep-*`），不调用外部 `rg`。

## exec

```text
command ─► shell 可用？（HostPlatform.shell）── 否 ─► EXEC_SHELL_UNAVAILABLE / CONFIG_INVALID
   │ argv：POSIX/Git Bash -lc；PowerShell -NoLogo -NoProfile -NonInteractive -Command；cmd /d /s /v:off /c
   │ 环境：HostPlatform.env + TERM=dumb NO_COLOR=1 PAGER=cat GIT_PAGER=cat GH_PAGER=cat，去掉 FORCE_COLOR
   ▼
spawn ─► 输出泵（stdout/stderr 交错；非法 UTF-8 记为 <0xNN>；≤30,000 字符留内存，超出写 <taskId>.log，64 MiB 后停写）
   │
   ├─ runInBackground（非成员）────────────────────────────► 立即交给 BackgroundExec
   ├─ 前台 30 秒仍在运行（非成员）──────────────────────────► 交给 BackgroundExec，返回 running 与输出路径
   ├─ 退出 ─► completed / failed(退出码)
   ├─ 超时（默认 300 秒，1 秒到 1 小时）─► 终止整棵树 ─► timed_out
   └─ 取消 ─► 终止整棵树 ─► Cancelled（内容保留已有输出）
```

退出与取消、超时、转后台同时就绪时退出优先：已退出的命令按完成结算，不转后台、不当作取消。交给 BackgroundExec 前先确保输出文件存在；建不起来返回 `exec_output_offload_failed` 并回收进程。BackgroundExec 拒绝（分支关闭中）时同样回收并返回错误。

进程结束后，每条输出管道的读取与解码最多再累计运行 100ms，然后等待已读内容落盘并刷新文件。落盘耗时不扣这段预算，避免慢磁盘丢掉管道中已有的数据；持续写入或持有管道不关闭的后代进程也不能无限延长读取。

## 生命周期

- 装载：取依赖，建 `WorkspaceTools`，提供 `S:ToolHost`。
- 调用：调用方令牌与本实例的生命周期令牌合成一个；卸载时新调用与在途调用都以重载结算。
- 卸载：取消生命周期令牌、关闭并等待在途调用（包括正交给 BackgroundExec 的命令）。已交出的后台命令属于 BackgroundExec。

## 不变式

- 本 Plugin 不写事件，也不持有分支状态：文件绑定在 FileReads，后台命令在 BackgroundExec，都按调用分支查找。
- 前台进程在工具结果返回前要么已退出，要么已终止回收，要么已交给 BackgroundExec。

## 为什么

- 文件与 exec 同属一个宿主：都是对工作区的直接操作，共用路径解析、cwd、输出目录。
- 分支部分按调用查找而非注入：Session 级的宿主不能注入某个分支的服务；经 `TreeCommands` 向下调用，没有反向注册。
- `read` 落盘不用 blob 存储：Rust 没有 blob 存储，与 exec 输出文件放在同一目录（目录 0700、文件 0600），模型用同一种方式再读。
- 协作 member 的命令只在前台运行：leader 以 member main 分支 idle 作为 request 的完成点，转后台的命令会在结果之后才结束。member 的 exec 不自动转后台、忽略 `runInBackground`，超时即终止；member 异步执行另立设计。
- 取消、超时、输出失败或等待失败后终止整棵树时，若不能确认树已清空，前台返回 `Command could not be terminated: …`，code `exec_terminate_failed`（后台结算为 failed，同一文案），保留已捕获的输出及其路径、字节数，丢弃句柄时再杀剩余进程。启动后立即取消、转后台失败时的清理也遵循此约定，不吞掉终止错误或误报已停止。
- 高成本搜索拦截对 `find` 只检查起始路径（前导选项 `-H -L -P -D -O… -E -X -d -s -x -f` 之后、第一个以 `-`、`(`、`)`、`!` 开头的表达式记号之前的参数）；`-name node_modules`、`-not -path '*/node_modules/*'` 等表达式操作数不当作扫描目标；把所有非选项参数都当作目标会误拒这类命令。
- 同一会话的在途 exec 不设上限；exec 并发与残留后台进程的提醒另立需求。
- 前台等待进程退出失败（进程已启动）返回 `Failed to wait for process: …`，code `exec_wait_failed`；`Failed to spawn process` 只用于进程真正启动失败。

## 代码与测试

- `crates/dim-agent-capabilities/src/workspace_tools/{mod.rs, exec.rs, files.rs, files/}`
- `crates/dim-agent-capabilities/tests/workspace_files.rs`
- `tests/integration/tests/{local_execution.rs, local_product.rs}`
