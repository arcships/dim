# 远程会话的文件输入

[#2277](https://github.com/arcships/dim/issues/2277) 的接口设计。A 是手边的终端，B 是当前选中的 App Server。用户从 A 拖入文件时要把这份内容交给 B；在会话里选 `@文件` 时要引用 B 工作区的文件。两台电脑完全可以在同一路径下存着不同内容。[远程终端设计](native-terminal-targets.md) · [文件传输与持久引用](file-transfer.md)

```text
在 A 的输入框
+----------------------------------------------------+
| > 比较 [notes.txt] 和 @src/notes.txt               |
+------------------------------- Office:/srv/demo ---+
          |                    |
          | 从 A 拖入          +-- 在 B 搜索和读取
          +-- 从 A 发送内容，B 返回会话附件引用
```

## 模块和方法

| 模块 | 建议入口 | 谁负责什么 |
| --- | --- | --- |
| A · dim-cli 的 composer、SessionHost 和一次性 CLI | 扩展 `DraftPart` 的文件来源；在 `submit_draft` 和一次性提交前共用同一附件处理 | 本机来源来自拖入、剪贴板或 `--image`。目标路径来自 B 的补全结果。草稿保存来源，提交时按原顺序解析各项。普通键入的路径文字仍是文字。 |
| A · dim-client | 新增客户端操作 `upload_session_file(target, header, source) -> ImportedFile` | 使用固定目标另开原生连接，写头、流式发送本机字节、读回执。输入读取器只在 A 的进程内使用。会话主连接继续处理审批、取消和通知。 |
| A ↔ B · terminalTransport | 复用 `open / write / read / close` | 转发已有的有序字节流和背压。每个文件用一条连接；同一路径适用 POSIX socket 和 Windows 命名管道。[共享拓扑](native-terminal-targets.md#posix-连接怎样跨机器) |
| B · App Server 的 Transport / Connection | 入口操作 `native.uploadSessionFile` | Transport 的本地读取器提供定长正文。Connection 负责对 Session 准入、暂存和调用附件 owner，持有暂存文件直到导入结算。调用方只能给出文件名，暂存路径由 B 创建。 |
| B · Attachments / SessionStore | 复用 `attachment.import` 与 `sessionFiles.import` | 前者处理普通文件或图片，后者提交文件及 Session 归属。原生入口返回既有导入结果。历史、模型请求和导出继续消费现有引用。[Attachments](../plugins/Attachments.md) · [Session files](../appserver/session-files.md#9-interface) |
| B · WorkspaceFiles / WorkspaceTools | `workspace.files.complete`，复用已有工作区索引 | B 按自己的工作目录、home 和路径语法生成候选。A 只显示结果和接收选择。[工作区索引](../../architecture/rust-agent/plugins/WorkspaceTools.md#shared-file-index) |

表中的两个线路方法分别属于原生传输入口和已有的 `workspace.files` 管理方法表。`attachment.import` 仍由 Session 的 `sessionMethods` 提供，持久文件归属仍由进程服务面 `sessionFiles` 提供。该设计沿用既有的 rutis 服务、插件和 Timeline 事件。[入口与分发](../contracts/session-boundary.md) · [服务面](../service-faces.md)

## 本机附件如何到达 B

```text
A / dim-client           已配对的 relay           B / 原生入口        B / Attachments
   |-- 新连接，v4 hello ----------------------------->|
   |<---------------------- hello.features ----------|
   |-- 一行 native.uploadSessionFile 请求头 -------->|
   |<--- native.uploadSessionFile.ready 或错误 -------|
   |== byteLength 个原始字节，多次连续写入 ==========>|-- 暂存完成
   |                                                |-- attachment.import -->|
   |                                                |<-- 持久引用 -----------|
   |<---------------- 同一请求 ID 的 JSON 回复 ------|
   |-- 关闭此连接                                    |
   |-- 原会话连接：session.submit(有序内容和引用) --------------------------> B
```

**协商与线路格式。** 专用连接使用可选的 v4 原生入口特性 `native.uploadSessionFile`。客户端在专用连接的正常 `hello` 回复里确认该特性，再把上传作为首个业务请求。WebSocket 和普通会话消息继续使用各自现有的帧规则。特性表示入口理解文件体；具体 Session 上的 `attachment.import` 仍在准入时检查。

专用连接取自创建草稿时固定的目标配置。A 将其 `hello.instanceId` 与当前目标连接核对；若 B 已重启，先重新确认设备与 Session，再由用户重试。后端不能按设备显示名重新挑选目的地。

```json
{"jsonrpc":"2.0","id":2,"method":"native.uploadSessionFile","params":{"sessionId":"s1","name":"notes.txt","byteLength":3,"image":false}}
```

请求头以 LF 结束。B 完成准入后发送无参数的 `native.uploadSessionFile.ready` 通知。A 收到通知后写入恰好三个字节，例如 `61 0a 62`。正文里的 LF、NUL 和任意二进制值都是内容。B 先消费行读取器已预读的字节，再继续读同一条连接，使用固定大小缓冲写入暂存。消费完声明长度即可导入和回复。A 保持写端打开直到收到结果，因此 Windows 也无需通过半关闭来标记文件结束。回复与请求 ID 对应，此后关闭专用连接；剩余数据不能作为第二条指令执行。

B 为存储文件生成安全名称 `attachment`，并保留不超过 16 位的 ASCII 字母数字扩展名。A 在输入框和消息链接上保留原文件名，包括中文和空格。普通文件的成功回复含 `{attachmentId, path, bytes, image: null}`。当 `image: true` 时，Attachments 按原图片规则验证和处理内容，返回可直接提交的 `image` 引用。A 使用 B 返回的路径或图像引用；跨端携带的请求头只有文件名，没有 A 的源路径。长文本粘贴继续走 `attachment.put`。普通文件从提交时固定打开的可读文件取数；客户端在发出最后一块前核对长度，发现源增长或截断就终止这条连接。图片发送已有草稿按图片规则准备的内容。

**准入与错误。** B 仅在已握手的私有原生入口接受该操作，沿用当前入口对该 Session 的权限、获取与释放规则。检查文件名、声明大小、附件提供者及接收空位后才创建私有暂存。接收沿用现有文件接收的单文件限额、并发和超时预算，Web 关闭时也可工作；Session 的租用跨越接收、导入结算和清理。A 在发送正文期间同时监听终局回复；B 拒绝请求时，A 停止读取源文件并显示 B 的错误。正常回复通过可靠的请求回复通道发送。

| 中断点 | 结果 |
| --- | --- |
| 提交前，目标或会话发生切换 | 草稿及已确认的引用绑定原设备身份和 Session。旧读取或补全到达时，只能更新原草稿；空白会话首次发送时先在所选目标取得 Session ID。 |
| 接收字节不足、读写失败、取消、超时、Session 关闭 | B 停止接收，释放私有暂存，调用方保留草稿。导入开始后沿用 owner 的取消与提交边界；已提交文件遵循 Session 的清理规则。进程异常退出留下的暂存由接收端在下次启动时清扫。 |
| 已确认部分附件，其余导入失败 | 保留顺序及已确认引用，仅在原目标和原 Session 的显式重试中复用。全部准备好才发 `session.submit`。 |
| B 已提交附件，回执丢失 | A 显示导入结果未知，保留草稿。用户重试可能产生另一份由同一 Session 拥有的附件；消息尚未提交。如果丢的是后续消息提交回执，沿用会话提交 ID 的核对规则。 |
| B 缺少上传特性 | 附件项提示目标版本不支持并保留草稿。禁止把 A 的路径当成 B 的路径代发。 |

暂存使用 B 的私有目录、按接收实例管理并清扫。Web 和原生入口可共享现有暂存与导入实现；两个入口同时启用时由一个所有者持有目录锁和并发预算。文件提交后的唯一持久归属在 SessionStore。没有每块 RPC、上传 ID 或断点续传表。

附件准备完成后，终端保留原设备、Session、会话线、已确认附件与本次提交 ID，直到收到消息回执。明确拒绝时，草稿在原会话线恢复。若回执丢失，TUI 在原会话线的空输入框提示按 Enter 核对，行模式在保留的草稿上提供相同操作。终端用 `session.input_lookup` 查询原提交：已接收就结束等待，查无记录才用原 ID 和原内容重试；查询失败则继续保留。[会话输入的幂等约定](../appserver/sessions-service.md#63-lookup)

## 工作区引用与启动目录

`workspace.files.complete({cwd, query, limit?})` 是私有原生入口的只读管理方法，返回 `{candidates:[{insertText, absolutePath, dir, image}]}`。`cwd` 是界面当前已从 B 确认的工作区，恢复会话时使用该会话自己的目录。普通项目查询调用现有 `MentionIndex.search`。当根是 B 的 home、文件系统根，或输入为 `~/`、绝对路径、含 `../` 的前缀时，B 只枚举用户正在输入的那一级，沿用原生补全的隐藏项、排序与数量规则。键入的目录不存在或不可读时，返回空候选。`insertText` 是 `@` 后面可继续编辑的拼写，例如 `~/work/`；`absolutePath` 是 B 上选中的位置。B 同时用文件扩展名与文件头判断候选是否为图片，A 使用返回的 `image` 标志。根目录与 home 的判断也在 B。已有 `workspace.files.search` 和 `session.files.search` 保留各自的索引查询契约，`/devices` 新建会话继续使用只列可见目录的 `workspace.files.directories`。旧目标未声明补全方法时提示可以直接键入目标路径。

A 给每次补全关联设备、工作区和当前查询序号，仅应用仍然匹配的结果。普通远端文件或目录按 B 返回的位置生成引用；视觉模型下选中 B 的图片时，通过原生连接对 B 的路径调用现有 `attachment.import({sessionId,path,name,image:true})`。当模型只接受文字时，带路径的图片转成目标可读的文件链接，纯剪贴板图片沿用保留草稿的行为。选自 A 的文件型图片若需要发成链接，也须先上传为 B 的会话文件。目录引用可以从 B 的 `@` 选择。A 拖入的是目录时，建议保留草稿并提示选择具体文件；把整棵本机目录作为一次附件处理是单独的产品决定。[原生输入规则](../cli-attachment-input.md)

CLI 在解析完目标和选项后再确定 `--cwd` 的所有者。命名目标或显式共享连接把用户给出的远端路径原样交给 B 的工作区校验，遵循现有创建会话的绝对目录契约；`--target` 与 `--cwd` 的先后顺序等价。A 的启动目录继续解析本机拖入的相对路径、`--image` 与本机导出位置。

## 对接口所有者的要求

| 发起者与问题 | 由哪个模块提供什么能力 | 顺序与生命周期 |
| --- | --- | --- |
| 原生终端需要携带单文件上限大小的正文，现有 NDJSON 单帧装不下其 Base64 表示 | Transport 提供同一已缓冲读取器上的定长读取；Connection 在原生入口受理文件、经现有 Session 方法表完成导入。由这两个 owner 确认可选特性和错误合同。 | 主连接照常分发。文件连接在回复写出后关闭；只有取得持久引用才允许提交。 |
| Composer 只有 A 的目录视图，现有索引无法回答显式路径的逐级补全 | WorkspaceFiles 组合 WorkspaceTools 的工作区索引和目标机直接目录读取，返回可插入文字与资源路径。 | 只读、可取消，迟到的查询结果不进入下一份草稿。 |

两项要求均由 [#2277](https://github.com/arcships/dim/issues/2277) 跟踪。[设计规则 §4](../../architecture/rust-agent/principles.md) 的提交、交接与上下文规则继续落在已有的文件提交和消息采用边界。接收中的字节属于连接 I/O，持久结果使用 SessionStore 现有事务。这里没有新增 Capability、分支业务状态或需要重放的中间事件。[插件契约](../plugin-contract.md) · [ADR 002](../../adr/002-plugin-scope.md)

## 验收与实施顺序

1. 用隔离的 A、B 文件系统，给同一路径写入不同文本和不同颜色的图片。从 A 拖入时逐字节核对会话附件并检查模型收到的图片；在 B 用 `@` 选择时分别核对 `read` 的文字与模型收到的 B 图片。覆盖滚屏、全屏、行模式已有的文件入口、一次性 `--image`、多项输入的顺序与文本模型。
2. 让现有 relay 搬运含 LF、NUL 的 16 MiB 文件。验证临界值、超限、短读、B 提前拒绝、接收期间取消、丢回执、部分成功后重试、B 重启清扫、Windows 保持写端打开、Web 关闭和另一条连接仍可完成 RPC。检查成功回执对应的文件及归属，并覆盖旧客户端和旧服务端。
3. 从 Home 和已恢复会话测试 B 独有目录、显式路径补全、隐藏项、切换设备后的迟到回包，以及两种参数顺序的 `--target B --cwd <B 独有绝对路径>`。以远端 shell、长粘贴和导出到 A 作回归对照。

Git 工作树、登录回调、Web 访问和运维命令的资源归属仍由 [#2277](https://github.com/arcships/dim/issues/2277) 跟踪。
