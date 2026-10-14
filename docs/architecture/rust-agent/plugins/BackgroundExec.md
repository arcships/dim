# BackgroundExec

本分支转入后台的 exec 命令：持有任务表，写 `exec/started`、`exec/ended`，按 `exec/ended` 的状态唤醒本分支。不启动进程（进程由 [WorkspaceTools](WorkspaceTools.md) 启动后交入）、不提供等待（[WaitBranch](WaitBranch.md)）、不提供任务列表。

<!-- generated:plugin BackgroundExec — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:Dispatcher`、`B:Timeline` |
| 提供 | `B:BackgroundExec` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `B:Timeline` | 写 `exec/started`、`exec/ended` 与唤醒输入；装载时读本分支历史 |
| `B:Dispatcher` | 订阅 `exec/ended`、`stop/interrupt` |

## 服务接口

`B:BackgroundExec`

| 方法 | 语义 | 错误 |
| --- | --- | --- |
| `start(request, process) -> task_id` | 写 `exec/started` 并接管进程，由一个监督任务等它结束后写 `exec/ended`；交接失败时先尝试终止回收进程，不能确认终止则优先返回含已有输出的失败结果 | `Closing`（分支关闭中或实例卸载中）、`Timeline`、`Termination`（携带 `exec_terminate_failed` 结果） |
| `stop(task_id) -> ExecEndedData` | 取消运行中的任务（它以 `cancelled` 结束，不唤醒），等它的 `exec/ended` 被分发后返回；已结束的任务直接返回其结果，唤醒照常 | `Unknown`（不属于本分支）、`Closed` |

## 运行时调用方

| 调用方 | 方式 |
| --- | --- |
| [WorkspaceTools](WorkspaceTools.md) exec 转后台 | `TreeCommands.get(context.branch_id)?.service::<BackgroundExec>()` |
| 协议层停止后台命令（`session.exec.stop`） | `Session::service::<BackgroundExec>(branch)` |

## 事件

写出：

| 事件 | 时机 | 字段 |
| --- | --- | --- |
| `exec/started` | `start` | `turnId`、`requestId`、`callId`、`taskId`、`description`（命令首行，≤200 字符）、`outputPath` |
| `exec/ended` | 监督任务结束；或装载时补写上一代未结束的任务 | `taskId`、`status`（`completed`/`failed`/`cancelled`）、`result`（工具结果，含输出预览与输出路径） |
| `input/message`（Steer，直接追加到本分支 Timeline） | 非 `cancelled` 的 `exec/ended` 分发时；装载时补发上一代没发出的 | 一个 raw 块：`<dim-notification source="exec" task>`，正文含任务描述、状态与结果预览（转义，16 KiB 截断）；事件标签 `exec-wake-task:{任务 ID}`、`exec-wake-result:{exec/ended 事件 ID}` |

消费：

| 事件 | 时机 | 作用 |
| --- | --- | --- |
| `exec/started` | 写入时（登记回调） | 加入任务表；分支关闭中则拒绝写入。登记后提交失败（没有写入）时移出该任务，等待它的 `stop` 得到 `Closed`，再回收进程 |
| `exec/ended` | 提交后 | 状态不是 `cancelled` 就投递通知（只看事件，不看内存状态）；投递失败只记日志，不使分发失败，由下一代补发；然后释放等待 `stop` 的调用方 |
| `stop/interrupt`（`scope: branch`） | 提交后 | 分支关闭：拒绝新任务、取消全部任务，结束时不唤醒；序号在它之后的 `exec/ended` 即使不是 `cancelled`（命令恰好先自行退出）也不唤醒 |
| `exec/started`、`exec/ended`、`input/message` | 装载时 | 重建任务表；未配对的任务补写 `exec/ended{failed}`（结果说明未能跨越重启，给出已有输出路径），它被分发时同样唤醒；非 `cancelled` 却没有通知（按 `exec-wake-result` 标签对应）、且不在分支关闭中断之后的 `exec/ended`（上一代写完结果后停止），在装载时直接追加补发一次 |

## 状态

| 状态 | 位置 | 重建 |
| --- | --- | --- |
| 任务表：描述、取消令牌、结束结果 | 内部 | 从本分支 `exec/*` 重建；运行中的进程不跨重载 |
| `closing` / `closed` | 内部 | 装载时为假 |

一个任务：

```text
 start ─ exec/started ─► 运行中 ──进程退出/超时/输出失败──► exec/ended{completed|failed}
                          │                                     │ 分发
                          ├─ stop ──► 取消 ─► exec/ended{cancelled}
                          ├─ 分支关闭 / 卸载 ─► 取消 ─► exec/ended{cancelled}
                          │                                     ▼
                          │                        状态是 cancelled？
                          │                          ├─ 否 ─► 通知（Steer）
                          │                          └─ 是
                          │                                     ▼
                          │                                  已结束（stop 返回）
 装载时发现未配对 ────────────────────────────► exec/ended{failed}（补写，分发时唤醒）
 装载时发现结果未通知 ────────────────────────► 补发通知
```

进程退出与 `stop` 同时就绪时退出优先，按实际结果结算并唤醒。终止未能确认整棵树已清空时结果为 `failed`（`Command could not be terminated`），丢弃句柄时再杀剩余进程。`exec/ended` 写入失败时移出任务表，等待的 `stop` 得到 `Closed`，由下一代补写终态。

## 生命周期

- 装载：先订阅，闸门内重建并补写，记 `rebuilt` 序号，处理器跳过已覆盖的事件，再提供服务。
- 卸载（包括宿主卸载空闲 Session）：标记关闭中、取消全部任务、等待监督任务写完 `exec/ended`，再清空任务表，等待 `stop` 的调用方得到 `Closed`。App Server 不自动卸载，运行中的后台命令只在宿主卸载时被停止。
- `start` 在交接期间持有任务追踪令牌：卸载会等待正在交接的命令。

## 不变式

- 每个 `exec/started` 恰有一个 `exec/ended`；监督任务没写成的，由下一代装载时补写。
- 是否唤醒只由 `exec/ended` 的状态决定：`cancelled`（stop、分支关闭、卸载）不唤醒，其余都唤醒；每个应唤醒的 `exec/ended` 最终恰有一条带其 `exec-wake-result` 标签的通知。
- 进程在交给本服务后由监督任务独占，结束前一定已退出或已终止回收。

## 为什么

- 事件属于本能力：没有共享任务层，`exec/*` 只由 BackgroundExec 写和消费；Loop 不知道后台命令。
- 唤醒用 Steer 输入：命令结果以普通输入进入对话，正在进行的回合也能看到；`wait` 由同一条输入唤醒。
- 唤醒直接追加到本分支 Timeline，不经分支门面：输入由本能力构造、按产生方保证合法；不需要等分支句柄就绪，也不依赖 InputQueue 在线。唤醒输入与其他输入一样由 Loop 处理：写在 `stop/interrupt` 之后的照常开始 turn。
- 不写 `ToolResult.background_task`：任务 ID 与输出路径已在工具结果文本中。

## 代码与测试

- `crates/dim-agent-capabilities/src/workspace_tools/background.rs`
- `crates/dim-agent-capabilities/src/workspace_tools/exec.rs`（`hand_off`、`finish_background`）
- `tests/integration/tests/local_execution.rs`
