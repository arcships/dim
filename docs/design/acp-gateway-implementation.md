# AcpGateway 首版实施计划

状态：合同明确支持的首版子集已实现于 [PR #441](https://github.com/arcships/dim/pull/441)，未发布。需求入口：[ACP #436](https://github.com/arcships/dim/issues/436)。设计合同：[AcpGateway](plugins/AcpGateway.md)。本文定义方法映射、接线、生命周期和不支持项；实际验收及未通过项见[验收记录](acp-gateway-acceptance.md)。

## 1. 目标和固定边界

1. `AcpGateway` 是接入方进程中的 rutis 插件。`dimcode acp` 装载它；其他宿主复用同一实现或启动同一命令。它不是 daemon 的 loader row，也不是分支 Capability。
2. Agent core、RPC 框架、内置 Session 协议、Timeline 事件和存储格式不变。MCP 业务模块注册独立的临时 Session 插件及通用 MCP 绑定方法；持久 MCP 管理不变。Gateway 只使用公开 RPC v3，不链接 App Server 的私有 `Connection` / `RpcService`，不新增 ACP 专用 RPC、反向调用通道或资源节点。
3. Gateway 拥有 ACP 连接、RPC 连接、请求生命周期、订阅、回放和协议投影。Session、turn、审批、Goal、任务、账户和持久化继续由现有所有者管理。
4. 现有接口不能保证的语义标为不支持；完成首版验收后再评估。不得用额外账本、状态预查、猜测字段或模拟成功补齐。
5. 首版目标是 Lody 和 acpx 的 ACP **agent** 接入。Dim 调用其他 ACP agent 是 [ExternalAgent](plugins/ExternalAgent.md) / #145，独立交付。

## 2. 核对基线和协议清单

| 来源 | 固定依据 | 用途 |
| --- | --- | --- |
| Dim | `7cf77abf`；下文链接指向仓库内现有所有者 | 判断 RPC、事件和数据是否已存在 |
| ACP v1 | [schema/meta，`66ff8076`](https://github.com/agentclientprotocol/agent-client-protocol/tree/66ff8076ec2320d85e84fedc83cb779e9d906a89/schema/v1) | 稳定协议和 `schema.unstable.json` 的完整方法盘点 |
| Rust SDK | [`agent-client-protocol` 3.5.0，`5ef0fa3a`](https://github.com/agentclientprotocol/rust-sdk/tree/5ef0fa3aa9e1f268ce16b58b1219b91f2fc24399/src/agent-client-protocol) | typed protocol、JSON-RPC、stdio；只选择需要的功能，不打开总 `unstable` |
| Lody Core | [0.2.0，`11e0708d`](https://github.com/LodyAI/acp-extension-core/tree/11e0708df0669568f44e287f17626a3944921dc5/src) | 每项独立 version 1；方法、元数据和计数语义 |
| Lody Codex adapter | [`df464ab9`](https://github.com/LodyAI/acp-extension-codex/tree/df464ab94e3028ce49a95ae0a9377957c54d8b93/src) | Core 列出的参考实现；用于理解临时 Provider routing 和 startup config，不取代 Core/RFD 合同，也不复制其进程重启策略 |

ACP 协议版本固定为 1，不能把 SDK 包版本当成协议版本。[v2](https://agentclientprotocol.com/protocol/v2/overview) 是另一个生命周期合同，首版不启用。Cargo.lock 固定 SDK 3.5.0 / schema 1.12.0，二者许可证均为 Apache-2.0；只启用所需的 unstable session fork。

下表“可接”表示已实现的现有接口映射；“条件可接”表示只有公开能力存在时提供；“不支持”表示首版不声明、不调用，收到已知但不支持的控制参数时在写入前报错。实际客户端验收不能从方法覆盖推定，见验收记录。

## 3. 模块、宿主与接线

```text
Lody / acpx / another ACP client
                 |
                 | stdin / stdout: ACP JSON-RPC
                 v
dimcode acp / another native host
  rutis client tree
    host inputs: owned ACP I/O + App Server connect configuration
    AcpGateway
      ACP SDK connection + serialized output
      per-session request / replay / progress projection
      one independently owned RPC connection
                 |
                 | existing protected socket / authenticated pipe, RPC v3
                 v
App Server (existing RPC; MCP business parts)
  session.* / approval.respond / contributed Session methods
  session.event / state / model.progress / notice / approval.*
                 |
                 v
existing Session and business owners
```

### 3.1 实施路径

dim-client 提供共享 RPC、产品事件和 Questions 边界以及 callback I/O；dim-acp 实现 Gateway，CLI 提供 acp 宿主入口。

| 路径 | 职责和输入输出 | 调用者 / 依赖 |
| --- | --- | --- |
| `crates/dim-client/` | 从现有 `dim-cli/src/client.rs` 提取 RPC framing、request/response、hello、socket/pipe 安全连接；复用现有 loopback callback capture 的纯 I/O 部分。输入连接配置，输出已有 Client/Message/RpcError 语义 | CLI 和 Gateway 共用；不依赖 `dim-cli` 或 `dim-app-server`，不包含 OAuth provider 实现 |
| `crates/dim-acp/` | `AcpGateway` 插件及其 ACP、Session、prompt、更新和 Lody 协议实现 | 依赖 `dim-client`、`dim-agent-types`、rutis、官方 ACP SDK；不依赖 TUI / App Server 实现 |
| `crates/dim-app-server/src/mcp/temporary.rs` | 独立的 TemporaryMcp Session 部件、临时绑定与服务器子插件；合同见 [TemporaryMcp](plugins/TemporaryMcp.md) | 由现有 MCP 功能注册，复用 McpServer、ToolRegistry 和 Session lifecycle；不改 RPC 框架或 Agent core |
| `crates/dim-cli/src/client.rs` | 保留 CLI 的 daemon 定位、启动、产品安装路径和命令策略，调用提取后的连接实现 | 保留全部既有调用者；不把 CLI Options 作为共享库合同 |
| `crates/dim-cli/src/args.rs`、`lib.rs`、`plugins.rs` | 解析 `acp`，取得宿主输入、装载插件、等待终止、返回退出码；`acp --auth-setup METHOD_ID` 是独立交互认证入口 | 不写协议 handler；普通 ACP 入口不装载 Tui、UiLeaf、WebHold 或更新提示 |
| `crates/dimcode/` 和既有打包验证 | 同一个 executable 分发 `acp`；保留跨平台打包和启动行为，仅发现实际打包缺口时修改脚本 | 不新增常驻进程或独立 ACP daemon |
| Gateway 公开流程测试 | 从 stdio 驱动真实 Gateway，再连接隔离 mock-model daemon | 使用现有 transport/CLI fixture；不测私有映射 helper |

共享 RPC 提取是为了复用已有实现，不能复制第二个 RPC client。安全检查、16 MiB framing、Windows authenticated pipe bridge、profile mismatch 和错误内容必须保留。daemon 启动、安装路径、Web assets 查找仍留在 CLI；Gateway 只接收连接参数并自行恢复它拥有的连接。提取时枚举 `client::` 的全部调用者，保留既有行为，不顺带重构命令或 UI。

### 3.2 插件输入和装配顺序

宿主提供两个明确的进程级输入：独占的 ACP reader/writer；App Server endpoint、server executable 和预期 model profile 等连接配置。可选登录输入复用现有宿主账户登录配置与 browser/terminal 能力，见 §4.4；不把实际账户 realm、client ID 或 token 流程写入 Gateway。I/O 只交出一次。宿主提供自己的错误报告/退出入口，协议正文只写 ACP writer。上述为本地宿主输入，不是 daemon 新服务、loader 配置或跨语言 API。

实现使用 rutis 现有 Plugin 和 effect，不新建插件容器。客户端树没有 Dim 的 Session/branch group，也不需要 AgentBuilder。AcpGateway 的连接、连接配置和 ACP I/O 都由宿主在构造时一次性交出，插件不读取外部服务，因此 Deps 为 `()`；不存在以 raw Arc 或手工 key 绕过 catalog 的服务依赖。#434 U3 中既有 CLI 插件是否迁移的讨论仍独立，不在此重写 TUI、引入 AcpHost 服务或等待 #403/#405。

```text
parse CLI options (including isolated --home / --cwd / --server / --agent)
  -> existing daemon discovery/start, preserving security/profile checks
  -> create client rutis root and supply host inputs
  -> mount AcpGateway; register cleanup before starting readers
  -> connect RPC; hello v3; verify required methods
  -> accept initialize; reply with the actual supported ACP subset
  -> serve requests until EOF, I/O failure, signal or plugin disposal
  -> close once; join owned tasks; dispose client root; exit
```

`--agent` 只选择 `session.create.agentId`；不创建 Agent、不改变默认 Agent 配置。插件不得拿走 Desktop/TUI 正在使用的 RPC connection。一个 ACP connection 拥有一个独立 RPC connection，多 Session 在该连接上按 Session id 分发。

Zed 保持原有启动入口：npm 包的 `dim` 与 `dimcode` 别名指向同一原生 CLI，`command:"dim"`、`args:["acp"]` 无需调整。`ACP_STICKY_SESSION` 可作为旧环境配置保留，不改变协议行为：同一 Session id 的连续 prompt 保留上下文；`session/new` 始终创建独立 Session；继续已有 Session 使用 load/resume。它不自动选择最近会话，也不把同目录的新会话合并。此移植未添加环境变量分支；上述行为来自现有 Session 语义与公开流程测试。

```json
{
  "agent_servers": {
    "dim": {
      "type": "custom",
      "command": "dim",
      "args": ["acp"],
      "env": { "ACP_STICKY_SESSION": "true" }
    }
  }
}
```

### 3.3 原生调用白名单

必需：`hello`、`session.create`、`session.attach`、`session.detach`、`session.submit`、`session.input_lookup`、`session.history`、`session.stop`。先验证协议 v3 及执行 profile 的 create/history/input_lookup flags；缺失时 initialize 失败。hello.features 不是完整方法目录：attach/detach/submit/stop/list/close 属于已协商 v3 的基础调用，没有逐项重复列出，不能据其未列出而判断不支持。可选管理方法和新能力仍按实际 flags / Session methods 判断。

可选：`session.list` / `session.summaries`、`session.close`、`session.delete`、`session.fork`、`session.permission.get/update`、`approval.respond`；临时 MCP 使用 `mcp.temporary.capabilities`、`session.mcp_temporary.attach/renew/detach`；每 Session 用 `session.methods` 发现 `session.input.withdraw`、`session.question.respond`、`session.context.usage` 和 `attachment.put`。缺少撤回时取消直接走现有 `session.stop`，并披露已接收输入可能保留的限制。

模型选择只读取 `provider.list`、`provider.catalog`、`provider.selection` 的安全投影。Provider 管理已有独立插件和公开读写接口，接线及尚未满足的 ACP 合同见 §4.5；首版不通过这些接口写入共享产品配置。账户认证只允许 §4.4 列出的 `account.providers/status` 和属于当前连接的 login attempt 调用，不开放 `account.*` 代理、recover/logout/usage、settings、持久 MCP 配置、插件管理或凭据接口。也不把 ACP 方法名直接转成原生 RPC 方法名。

## 4. 稳定 ACP v1 方法逐项映射

原生参数和返回值以 [protocol.rs](../../crates/dim-app-server/src/protocol.rs)、[tree_protocol.rs](../../crates/dim-app-server/src/tree_protocol.rs) 及所属插件为准。下文保留两端各自的字段拼写，转换只发生在协议边界。

### 4.1 Client → agent

| ACP 方法 | 首版 | 原生调用、返回时点和限制 |
| --- | --- | --- |
| `initialize` | 可接 | 先 `hello {protocolVersion:3, clientKind:"other", clientName:"AcpGateway"}`。按 SDK 协商 ACP 1；记录 client capabilities；读取现有 account discovery/status，按 §4.4 生成真实可执行的 authMethods。initialize 不自动开始登录；无 managed provider 时不强制登录。重复 initialize 拒绝 |
| `authenticate` | 条件可接 | 仅接受本连接已发布的 agent-type methodId；`account.status` 确认已登录则返回 `{}`，否则走现有 browser begin → 宿主 callback capture → browser complete → status。OAuth 和 token 交换由原插件完成，不由本地 RPC transport 的身份校验代替 |
| `logout` | 不支持 | 不映射到账户登出，不声明 logout capability |
| `session/new` | 可接 | 校验绝对 `cwd`，只接受空/缺失 additional directories；按 §8.3 验证 startup config 后，在同一 `session.create {cwd, agentId?, model?, permissionPolicy?}` 中传入 → `session.attach {sessionId}` → 读取 main 固定历史头和 Session methods / permission / adopted model → 按 §4.6 绑定 mcpServers。初始化及初始工具注册完成才返回 id、可用 modes/configOptions |
| `session/load` | 可接 | 从 `session.summaries` 或 `session.list` 获取真实 cwd，检查一致；attach → 固定头 main history 从 0 回放 → 接上 live → 返回。重复 load 复用 runtime，不重复建立订阅；在 prompt 中拒绝重放 |
| `session/resume` | 可接 | 同 load 的准备和状态恢复，但历史只用于内部关联/去重，不向客户端发送旧消息；先返回，再发送准备期间缓存的真正 live updates |
| `session/list` | 条件可接 | `session.summaries {agentId?}`；不存在时用 `session.list {agentId?}` 并省略不可得的 updatedAt。按 ACP cwd 在内存过滤；毫秒时间转 ISO 8601。返回完整的一页，不声明原生分页，不制造 nextCursor；非空未知 cursor 报错。原生全量响应超过 frame limit 就报错，不循环扫描凑页 |
| `session/prompt` | 可接 | §5；`session.submit {sessionId, branchId:"main", submissionId:<uuid>, user:<mapped>, mode:"queue"}`。回复携带原生已提交 event；等消费该 input 的 turn 终态才回复 ACP。每 Session 同时只接受一个 prompt |
| `session/cancel` | 可接 | §6；作用于该连接已登记的 Session，有本连接 prompt 时先撤回尚未采用的 input，再按需 `session.stop {sessionId,branchId:"main"}`；没有本连接 prompt 时，仍可请求该 Session main 的当前 turn 停止。不能因本地没有 prompt 就忽略取消 |
| `session/close` | 条件可接 | 原生 `session.close {sessionId}`；这是显式关闭整个 Dim Session，影响其他客户端和分支。等待原生结算成功后清理本 runtime、回复 `{}`；不是 detach。当前 daemon 对关闭 id 的再 attach 可能拒绝，不能承诺关闭后可立即 resume |
| `session/delete` | 条件可接 | 原生 `session.delete {sessionId}`，等待原生持久化删除结果，再删除本 runtime。失败保留诊断，不本地隐藏记录冒充删除 |
| `session/set_mode` | 条件可接 | 仅提供 `read-only`、`workspace-write`、`full-access`。先 get policy/version，再 update `{sessionId,branchId:"main",expectedVersion,policy:<保留工具列表，仅替换 preset>}`。冲突返回错误并刷新；不盲重试旧 policy，不映射 plan mode |
| `session/set_config_option` | 条件可接 | `mode` 复用上一行。`model` 仅在安全 provider catalog 和 `session.model.update` 可用时提供：保留 catalog 中 native model id，使用生成该选项的 providerId 调用 `{sessionId,branchId:"main",providerId,modelId}`。等待原生 `{model,outcome:"adopted"|"unchanged"}` 并接收 `context/configured`，再返回完整 configOptions；`configuration_superseded` 时刷新并报错，不把请求值直接当 currentValue |

mode 和 model 的选项来自当前 Session 的公开状态，不能用用户偏好默认值冒充已采用值。模型切换影响可用选项时重读 catalog；超时或连接丢失后的写入不能报告为未应用。effort、fast/service tier 的写入口虽然存在，公开 `context/configured` 视图现在也读回已采用的 `model_preferences`，但首版不发布这些 config options。不同 Session 的可选方法逐个检查，不能因另一个 Session 安装了插件就假定可用。

new/load/resume/fork 通过独立的 [TemporaryMcp](plugins/TemporaryMcp.md) Session 插件接入 `mcpServers`，与原生工具并存，不写共享配置。只有旧 daemon 缺少 `mcp.temporary.capabilities` 时保留接受并忽略行为，且不声明 HTTP/SSE MCP 能力。非空 `additionalDirectories` 仍在创建或 attach 前拒绝。

### 4.2 Agent → client 及协议取消

| 方法 | 首版 | 调用或来源 |
| --- | --- | --- |
| `session/update` | 可接 | §7 的有序投影；普通客户端始终能消费标准更新，不要求 Lody |
| `session/request_permission` | 可接 | `approval.request` → `allow_once` / `reject_once`，分别转 `approval.respond {sessionId,branchId,requestId,decision:"allow"|"deny"}`；不提供 always 选项。client cancelled 不替用户作决定；native resolved/cancelled 后忽略迟到答案 |
| `elicitation/create` | 条件可接 | client 明确支持 form 且该 Session 存在 `session.question.respond`；从未结束的 `ask_user` tool-call 生成 form，使用 branchId/requestId/callId 关联。有效 accept → native answers；decline、client cancel、无效或空答案、client RPC error → `declined:true`；turn cancel / EOF 只释放本地等待，不伪造回答。不是 MCP-server elicitation |
| `elicitation/complete` | 首版不发送 | 不支持 URL elicitation 或 out-of-band completion；form 请求正常响应即结算 |
| `fs/read_text_file`、`fs/write_text_file` | 首版不调用 | 无现有客户端文件访问/未保存 buffer backend 接线；仍由 Dim 工作区工具访问资源节点 |
| `terminal/create`、`terminal/output`、`terminal/wait_for_exit`、`terminal/kill`、`terminal/release` | 首版不调用 | 执行继续归 Dim 的 exec/BackgroundExec；不把客户端 terminal 接成第二个工具执行入口 |
| `$/cancel_request` | 有限支持 | 可以取消本地只读准备/回放及 §4.4 的认证等待，按 SDK 返回 cancelled。已投递变更不回滚、不宣称未执行；认证只取消自己尚 pending 的 attempt。`session/prompt` 的运行取消通过 `session/cancel`；首版不承诺所有原生 RPC 都可按 request id 撤销 |

提问表单沿用 [Questions](plugins/Questions.md) 的 options、multiSelect、allowCustom 及答案合同；优先复用 CLI 中与终端无关的 [question projection](../../crates/dim-cli/src/question/mod.rs)，不能复制另一套工具校验。仅存在真实 Lody question 字段时附加 elicitation metadata；answer notes、secret、preview、auto-answer 无来源，不发布。另一客户端先回答时服从 native `question_already_answered` / `question_cancelled`。

多选数组使用 ACP 的 `items.anyOf`，`minItems:1`。没有自定义答案时，选择字段为 required；允许自定义答案时保留选择字段可选，以支持仅填自定义文本。每个已完成的客户端请求都会提交有效答案或 decline，不留下无法再次呈现的 pending call。

### 4.3 Draft / unstable 和内容边界

| 表面 | 首版 | 原生路径 / 缺口 |
| --- | --- | --- |
| `session/fork` | 条件可接，单独启用 SDK `unstable_session_fork` | 无锚点时固定 main head；有 Lody turnId 时定位该真实 turn 的 `turn/end` sequence。`session.fork {sessionId,at}` → 新 id → attach。原生 rest-point 检查决定能否 fork；活动 turn 内的切点失败，不偷偷改成早一点的切点 |
| `providers/list` | 读取可复用；整组语义待选择 | `provider.management {}` → 安全可配置 Provider 投影；ACP 的 providers capability 同时承诺三个方法，不能只凭 list 可读就声明，见 §4.5 |
| `providers/set` | 编辑可复用；共享设置或临时覆盖待选择 | `provider.management {providerId}` → 同版本 `provider.update` 是共享设置的路径；全量 headers、固定 driver/auth 和凭据归属须准确转换。临时覆盖另缺采用接口，见 §4.5 |
| `providers/disable` | 禁用可复用；共享设置或临时覆盖待选择 | 同版本 `provider.update {patch:{enabled:false}}` 是持久化禁用，不是 CLI disconnect、delete、logout 或清除临时 override；不能混淆，见 §4.5 |
| `mcp/message` | 不支持 | 没有现成 MCP-over-ACP 入口 |
| `session/set_model`、`document/didOpen`、`document/didChange`、`document/didClose`、`document/didSave`、`document/didFocus` | 不支持 | 历史 SDK 的入口不在固定稳定方法表；模型走 config options，文档通知不伪装成已维护 buffer |
| `text` / `resource_link` | 可接 | 映射到 UserInput text；resource_link 为 `@<uri>` 引用，不自行下载或赋予路径访问权 |
| `image` | 可接 | 在 ACP 输入边界检查 MIME/base64，再映射现有 image ContentBlock，经 `session.submit.user` 发送；是否为模型支持由 ModelProvider 决定，不走 attachment.put |
| 文本 embedded `resource` | 条件可接 | UTF-8 ≤32 KiB 作为 `<uri>\n<text>`；更大文本调用 `attachment.put {sessionId,name?,text}`，再提交返回路径的引用。最大 10 MiB 且须满足完整 JSON frame 16 MiB；多项提交前逐项预检，避免无效 prompt 已上传部分文件 |
| `audio`、二进制 resource、超限 frame | 不支持 | 写入前报 `unsupported_content` / 参数或大小错误；不把二进制当文本，不绕过上限 |
| ACP v2 / HTTP / WebSocket transport | 不支持 | 不用默认生产 WS endpoint 执行；首版只有 stdio 和现有本地 RPC transport |

attachment 上传成功而后续 submit 失败时，文件仍归原生 Session，交给已有清理规则；Gateway 不直接删 Session 文件。跨上传和 submit 没有原子事务，不宣称有。

### 4.4 账户认证与现有 OAuth 插件

本地 socket/pipe 的身份校验决定谁能连接 daemon；模型账户登录决定对应 managed provider 能否取得凭据。两者不是同一认证，前者成功不能推出后者已登录。

现有公开接线已经完整：[ManagedProvider](../../crates/dim-app-server/src/managed_provider.rs) → [account methods](../../crates/dim-app-server/src/managed_provider/management.rs) → 插件的 browser/device/login attempt 实现。账户插件 `crates/dim-oauth` 通过现有 `run_with` 安装；Gateway 只使用公开 RPC，不导入账户实现、client ID、token 或存储格式。浏览器发起/完成调用见 [CLI login](../../crates/dim-cli/src/login.rs)，自动 callback capture 和结算见 [SessionHost](../../crates/dim-cli/src/session_host.rs)；复用 [Capture](../../crates/dim-cli/src/mcp.rs) 的纯 loopback I/O 部分，不复制 HTTP parser 或 OAuth 实现。

#### Discovery 和认证方法

1. `account.providers {}` 返回 `providerId/displayName/loginMethods`；`account.status {providerId}` 返回 AccountSnapshot。只有方法明确不存在或 providers 为空时，才视为没有此接入；网络、存储、配置错误不能吞成“无须认证”。
2. auth method id 从发现的 provider/method/flow 确定性编码，按本连接已发布 id 查表，不从客户端 methodId 解析任意 provider、realm 或 callback。provider id 来自原生响应，public build 不硬编码某个私有账户。独立 terminal helper 重新 discovery 并生成同一 id 表，再校验传入 id，不依赖父进程的内存映射。
3. 当 provider 声明 browser，且宿主可打开浏览器并捕获其已配置 loopback redirect 时，发布 agent-type browser method；它才使用 `authenticate`。已登录时调用 authenticate 只核对状态并成功，不重启登录。
4. client 声明 `clientCapabilities.auth.terminal=true` 或 Registry 的 `clientCapabilities._meta["terminal-auth"]=true` 时，可发布 terminal-type method。args **替换**默认启动参数，完整包含 `acp`、宿主的绝对 `--home`、显式 `--server` / `--model-config`、登录 directory / redirect URI，以及 `--auth-setup METHOD_ID`；不得依赖被替换参数中的配置。新交互进程复用既有 CLI login 的 browser/device 流程与相同 executable、home 和 profile。客户端等待交互进程退出后再试请求；按照 ACP，**不能**把 terminal methodId 发给 authenticate。
5. 无可交付 UI/终端的 headless 宿主不发布不可执行的方法。device 已有原生接口，但首版通过上述交互 helper 提供，不另造 ACP user-code/form 流程。终端 helper 是独立交互进程，其文本输出不进入 ACP 连接 stdout。复用 Gateway 的宿主通过 `Login.terminal_args` 提供完整启动前缀；`None` 表示没有终端入口。

```text
ACP client -> dimcode acp --home /isolated/home ...
           <- terminal auth: args=[acp, --home, /isolated/home, ..., --auth-setup, ID]
ACP client -> dimcode <advertised args> -> existing interactive login
           <- helper exits -> client retries the original ACP request
```

认证方法可在账户已登录时继续发布，供后续重新认证；是否需要认证必须针对当前选择的 provider 判断。API-key/configured profile 不因其他 managed provider 未登录而被阻止。没有 account 插件时 authMethods 可为空；同一 Gateway 不安装插件、不切换默认 provider、不启动另一个账户构建。

#### Protocol-driven authenticate

```text
authenticate(advertised agent methodId)
  -> account.status(providerId)
  -> already signed in? return {}
  -> bind owned loopback callback capture for configured redirect
  -> account.login.browser.begin(providerId, directory, redirectUri)
  -> open returned authorizationUrl through the host
  -> await callback / attempt terminal state / cancellation / expiry
  -> account.login.browser.complete(providerId, attemptId, callbackUrl)
  -> account.status(providerId), require authenticated account
  -> authenticate response {}
```

directory/redirectUri 来自宿主现有登录配置和入口选择，不来自 ACP 任意元数据；只有插件决定授权端点、PKCE、state、token 交换、刷新和持久化。callbackUrl 仅在本地捕获后交还同一个 native attempt，不进入 form elicitation、model context、ACP 消息或日志。

| 原生方法 | Gateway 使用范围 |
| --- | --- |
| `account.providers`、`account.status` | discovery、确认所选 provider 的账户状态；不向 ACP 暴露完整账户身份或 credentialEpoch |
| `account.login.browser.begin {providerId,directory,redirectUri,email?}` | 发起本连接主动 authenticate；首版不从 ACP 收集 email，沿用宿主已有配置 |
| `account.login.browser.complete {providerId,attemptId,callbackUrl}` | 只完成当前连接自己捕获的 attempt；返回 AccountSnapshot，不返回 token |
| `account.login.status {providerId,attemptId}` | 以已有 pending/completed/failed/cancelled/expired 判断等待结果；只在本连接认证等待期间按现有 CLI 一秒节奏查询，不建立常驻账户 poller |
| `account.login.cancel {providerId,attemptId}` | 只取消本连接尚在等待的 attempt；不能按任意客户端字段取消其他客户端登录 |
| `account.login.begin {providerId,directory,email?}` | 已有 device flow，首版仅独立 terminal helper 复用既有 CLI login 调用；不宣称支持 ACP 内的 device UI |

浏览器启动、callback capture 失败或端口被占用时，结束本次认证并尽力取消自己已创建的 attempt，返回真实错误；不自动改 redirect 端口或跳到新登录。确认所选 managed provider 缺账户时可以返回 ACP `auth_required`（-32000）；仅知道 native `provider_failed` 时不能解析文案冒充这个结论。登入成功是 native 账户状态，不保证每个后续模型请求不会发生网络或凭据拒绝。

#### 认证生命周期

每连接每 provider 同时最多一个认证操作，保留 methodId/native attemptId、callback listener、deadline 和 generation；不持有 token。到期使用原生 expiresAt / attempt terminal state，不另设短于原生登录流程的任意总超时。

EOF、插件卸载、用户取消或 `$/cancel_request` 结束本地认证等待、关闭 callback listener，并尽力 `account.login.cancel` 当前连接自己的 pending attempt；不调用 logout，不清除已存账户。若 browser.complete 已经提交，插件仍拥有被接受的交换与持久化，即使 Gateway 不再等待；断线后只能查询 status/attempt_status，不自动重发 complete 或 begin，也不能把取消等待说成回滚登录。迟到 callback 必须同时匹配 connection generation、provider 和 attempt，不能完成下一次登录。

`logout` 和 `account.usage` 仍不在首版接入范围：有原生方法不等于 Gateway 公开通用账户管理。此认证接线不增加 App Server 接口或修改 OAuth 插件。

### 4.5 Provider 管理与现有插件

Provider 业务已经由 daemon 的 [ProviderPlugin](../../crates/dim-app-server/src/provider.rs) 拥有，并通过 [management methods](../../crates/dim-app-server/src/provider/management.rs) 发布。现有 [CLI Provider flow](../../crates/dim-cli/src/session_host.rs) 调用 `provider.management`、`provider.probe`、`provider.create` 和 `provider.update`。Gateway 应复用这些公开方法；不新增 ACP Provider 插件、registry、数据库或与 ProviderPlugin 平行的模型路由所有者。字段、版本、secret 和 action 的权威合同是 [Provider management](appserver/provider-management.md)，对应 #329 / #349；下表只描述 ACP 边界转换。

#### 已有调用与边界转换

| ACP 方法 | 可复用的原生调用 | 必须保留的语义 |
| --- | --- | --- |
| `providers/list {}` | `provider.management {}` → `{drivers,providers,errors}` | `providerId` 保留原值；只列能兑现配置合同的记录。静态连接的 driver/实际 endpoint 决定 `supported` 和 `current.apiType`；enabled=false 才投影 `current:null`，不能把未登录、缺凭据或缺模型伪装成禁用。只返回非秘密 `apiType/baseUrl`，不返回 headers、credential、secret ref、账户或 proxy 身份。损坏记录按原生 errors 处理，不能伪装为已禁用记录 |
| `providers/set {providerId,apiType,baseUrl,headers?}` | 先 `provider.management {providerId}`，取得当前 `version/connection/actions`；再 `provider.update {providerId,expectedVersion,patch}` | 已知 id、可编辑记录、可表达的协议和认证方式才能进入写入；patch 组合 `baseUrl`、`enabled:true`、完整 header 操作及必要的 credential 操作，在原生一次 CAS 事务中提交。成功只返回 ACP `{}`，不返回原生管理快照 |
| `providers/disable {providerId}` | 同样读取管理快照，再 `provider.update {providerId,expectedVersion,patch:{enabled:false}}` | 保留凭据、路由、模型和 Provider 身份；禁用后仍可发现且 `current:null`。明确未知 id 按草案幂等返回 `{}`；已存在但受限、损坏或管理读取失败不是未知 id。required=true 必须拒绝，不能以当前模型选中了该 Provider 就推导 required |

`apiType` 不是 `driverKind` 的别名。原生 update 不允许改变 driver/auth；同一 Provider 只能列出其现有 driver 和实际模型路由确实支持的协议，不能把 `drivers` 全局目录当成每项的 `supported`。例如 `openai-compatible` / `custom` 的单一 OpenAI-compatible 路由、`anthropic` 的单一 Anthropic 路由可以明确映射；不能因字段相似便把 Gemini 当作 Vertex，或把 New API 多协议路由当作可整体切换的单协议 Provider。不存在的 Provider 在 set 时返回 invalid_params，不把 set 改造成 create，不生成新 UUID，也不悄悄换成另一个 Provider id。

ACP set 的 `headers` 是**完整替换**，省略等同空 map；原生 patch 的省略则保留已有值。转换必须在外部协议边界完成：

1. 从同一管理快照的 `headerNames` 得到已有名字；按 HTTP header 大小写不敏感比较，拒绝输入中重复名字。对输入未包含的旧名字生成 remove，对输入中的普通 header 生成 set；不需要读取旧值。
2. 原生 Bearer / ApiKey / CustomHeader 的认证 header 归 credential 字段所有，不能同时写入普通 headers。只有输入可被既定 auth 的 header/prefix 无损表达时，才转换成已有 `credential:{action:"set",value}`；省略该 header 必须明确 clear，不能保留旧 credential 或 secret ref。清空后若该固定 auth 不允许启用无凭据记录，就属于当前原生合同不支持的路由，不能用旧密钥补齐，也不能伪造匿名认证。
3. 原生 AwsBedrock 签名、managed account 连接、需要改变 driver/auth 的配置不能借普通 headers 绕过所有者。`connection:null`、`enabled:null` 或缺 version 也不能补默认值。账户插件的登录仍走 §4.4；它不自动获得 route override 能力。
4. 不改未由 ACP 表达的 model catalog、selection、preferences、proxy 或 timeout；不调用 probe/refresh 后才承认 set 成功。版本冲突返回真实冲突，不重新读取后自动覆盖；已发送的 Drain 写入即使取消等待、EOF 或丢失回复仍可能提交。不得重发或在卸载时写回旧快照；安全管理读取也无法证明未回显的 secret header 值。

CLI 的 Disconnect 是 `{credential:{action:"clear"},enabled:false}`，不适合 ACP disable；不能直接复用 UI action 而改变协议含义。管理写入也不等于为现有 Session 采用新模型；模型配置的后续采用仍归现有 Session/Provider 所有者。

#### 首版作用域与能力声明

固定基线的 [Configurable LLM Providers RFD](https://github.com/agentclientprotocol/agent-client-protocol/blob/66ff8076ec2320d85e84fedc83cb779e9d906a89/docs/rfds/custom-llm-endpoint.mdx) 写明配置为 process-scoped，并 **SHOULD NOT** 持久化；后者是推荐而非禁止。RFD 没有为代理加共享 daemon 的架构指定该进程必须是哪一个，更没有要求每 ACP connection / Session 隔离。Lody Core 本身不定义 `providers/*`，也没有改写此处作用域。独立 Gateway 进程并不自动构成必须隔离配置的协议要求。

当前原生 `provider.update` 将版本化记录写入产品 home 的数据库，重启后仍在，共享该 home/daemon 的其他客户端后续模型请求也可能受影响。**若产品选择 ACP 管理 Dim 的共享设置，上表读写路径可复用，持久化本身不构成关闭 capability 的充分理由**；但必须明确共享影响及与 RFD 推荐的差异。**若产品选择仅本次 ACP 运行的临时路由覆盖，当前公开接口没有对应合同**，不能以 Gateway 本地 map 改 list 结果却继续使用旧模型连接，也不能暂存/还原共享记录。

参考实现 [CodexAcpClient](https://github.com/LodyAI/acp-extension-codex/blob/df464ab94e3028ce49a95ae0a9377957c54d8b93/src/CodexAcpClient.ts) 将 ACP set 保存为内存 `gatewayConfig`，通过 `mergeGatewayConfig` 合入原生 Session 建立参数；[CodexAcpServer](https://github.com/LodyAI/acp-extension-codex/blob/df464ab94e3028ce49a95ae0a9377957c54d8b93/src/CodexAcpServer.ts) 在必要时排空并替换其自有 app-server。Dim 不拥有共享 daemon，不能复制该重启路径。该适配器的 disable 清除 override、恢复 native routing，list 随后仍有 current；这与 RFD 所述禁用后 current:null 不一致，不能把参考实现的行为当成协议授权，或等同于 Dim 停用持久化 Provider。

两种产品语义仍待选择；当前不发布 `agentCapabilities.providers`、不启用 SDK `unstable_llm_providers`，不是协议禁止持久化的裁定。无论选择哪种，都复用现有 Provider 所有者；临时覆盖需要在实际模型连接采用处具备合同，不新增第二个路由业务所有者。相关公开流程复用现有 Provider fixture，覆盖完整 headers 替换/省略清空、auth header 分离、协议不匹配、managed 记录拒绝、未知 id、disable 保留 credential、CAS 冲突和 Drain 回复丢失，并验证真实模型调用的路由。产品语义未确定前，先验证 capability 缺席、调用不支持方法不修改隔离 home 的 Provider 记录。

### 4.6 临时 MCP 接线

[TemporaryMcp](plugins/TemporaryMcp.md) 是 MCP 所有者的独立 Session 部件，定义完整的 attach/renew/detach、工具隔离、租约和子插件清理合同。Gateway 在 ACP 边界转换 stdio/HTTP/SSE 参数；native Session 建立、cwd 检查及历史准备之后，通过现有 contributed Session methods 完成初始绑定，成功后才响应建立请求。每个 Gateway/Session 持有一个 bindingId，重复建立复用，同一绑定的不同配置明确冲突，空列表不清空既有绑定。

Gateway 每十秒续租；native 重连后用相同 bindingId 幂等恢复资源，prompt 前核验绑定，过期则重建；不重放工具调用或模型输入。EOF/卸载先尽力释放自己的 MCP 绑定，再 detach viewer。强杀或无法恢复连接时，原生三十秒租约结束后卸载资源。新方法属于 MCP 插件，RPC 框架、Agent core 和持久 MCP 配置不变。

会话配置读取同时消费 hello 中的内置 permission/model 方法与 session.methods 中的贡献方法，二者不能互相替代。

## 5. Prompt、回放和响应关联

### 5.1 状态与决定

```text
validating -> submitting -> accepted(input event id)
                               |
                               +-> input/deleted -----------> cancelled
                               |
                               +-> input/applied
                                      -> input/consumed in turn T
                                      -> turn/end(T) --------> response
```

native reply、事件和 live progress 可以先后任意到达。先登记 submissionId 与 ACP request，再发送 native submit；reply 未到先收到的事实保留，不能丢失。使用 input event id 关联 `input/applied` / `input/consumed`，以消费位置上的真实 `turn/start` 确定 T，绝不把别人的 `session.state.running=false` 当作当前 prompt 完成。

`Completed` 默认 `end_turn`；只有 native terminal reason 明确对应 token/request 限制时才返回 `max_tokens` / `max_turn_requests`，不匹配错误文本。`model-output-end` 的失败 presentation 明确给出 `reason = max_tokens` 时，对其对应 turn 返回 `max_tokens`；Session 与 ACP 连接继续接受后续 prompt。`Cancelled` → `cancelled`；其他 `Failed` → `turn_failed` JSON-RPC 错误；本连接已收到 session/cancel 时，最终 prompt 回复必须是 `cancelled`，不能让清理异常改成泛化 failure。无在途 prompt 的重复 cancel 是一次 addressed stop，不制造 prompt response。

现有 Hooks 设计中的 `input_blocked` 尚不是此基线的已实现提交检查；首版不依赖 Hooks。如果服务端未来返回该“未写入”错误，展示原消息并返回 `refusal`，不在 Gateway 实现 Hook 或解析专属 marker。

一个原生 turn 可消费多条输入，Goal/后台结果也可能再驱动新 turn。ACP v1 的普通 prompt 只等待消费它的 T；后续真实更新仍可到达，不表示 Gateway 拥有整个 Goal 或后台生命周期。不得用第一次 turn/end 冒充 Lody Goal 完成。

### 5.2 固定头与 live 合流

1. 为同一 Session 的 new/load/resume/fork/history 操作同步建立一个准备槽位，再开始 await；并发 load 共用该槽位，不能创建两个消费者。失败清除槽位，关闭标志阻止迟到准备重新登记。
2. attach 前登记接收路由；attach 后先缓存该 Session 的 live 事件和 notices。调用 `session.history {sessionId,branchId:"main",after:0,limit:256}` 固定返回的 through=H；后续页显式传 through=H，并使用原生 nextAfter 前进。不能因过滤后的 events 为空就提前结束。
3. 按 sequence 折叠至 H，依据原生 done 结束；history 中删除的输入不渲染成有效用户消息，raw-only input 不渲染。Gateway 自己的 prompt 不重复 echo，其他客户端的真实 user input 可以投影。
4. 丢弃缓存中已折叠的 durable events，顺序消费 H 之后的部分；progress 用 request id 和 Unicode scalar offsets 与 committed text/thinking 合流。resume 不向 ACP 输出 H 以内的旧内容。load 的回放 updates 全部成功写出后才回复。
5. Session notice 无 Timeline sequence。标题按 titleRevision 去重；非持久状态不可仅靠 history 恢复，重新查询公开元数据。所有 I/O、reply 和回放都检查连接 generation，旧 generation 不能写入新连接。

### 5.3 提交不确定性

`submissionId` 是查询关联键，不是服务端的 exactly-once 保证。不得自动重发已送出的 create、fork、delete、attachment.put、configuration update 或 approval response。

submit 的拒绝/不确定性遵循 [现有提交合同](../../crates/dim-app-server/README.md#session-submission-errors)：明确保证未提交的错误才是 rejection；其他错误、断线和 `submission_unknown` 都不能报告“未发送”。重连后 `session.input_lookup {sessionId,branchId:"main",submissionId}` 返回 accepted 时继续关联同一个 input；unknown 时可能原调用仍未完成，不立即重发。恢复窗口内继续查询与 catch-up；仍无证据则返回 `submission_unknown`，提示客户端 load 检查，禁止自动生成新 submissionId 重试。

## 6. 生命周期与连接管理

### 6.1 资源所有权

| 资源 / 状态 | 所有者 | 结束方式 |
| --- | --- | --- |
| rutis root、启动配置 | 接入方宿主 | root disposal；不因此停止共享 daemon |
| ACP I/O、SDK connection、唯一 writer、RPC reader/writer | AcpGateway generation | EOF、I/O failure、signal、插件卸载进入同一个 close-once 路径 |
| RPC ids、ACP ids、请求关联、每 Session 操作槽位 | 当前连接 generation | 响应结算或 close；不持久化，不跨连接复用 request id |
| Session attach、回放 cursor、流式 offsets、permission/form waiter | Gateway 的 Session runtime | 显式 native close/delete、detach、RPC generation 切换或 Gateway close |
| authentication waiter、loopback callback listener、自己的 attempt 关联 | Gateway 的连接 generation | §4.4 结算、取消、到期或 close；实际 OAuth 交换和账户持久化仍归 managed provider |
| background task 的 tool 关联、collaboration request 到 opaque runId 的投影 | 每 Session 的观察 activation | 从已提交 history 重建；RPC 观察丢失结算仍开放的 subagent 显示状态，EOF/卸载释放；不停止原生任务或 member |
| Session / turn / 工具进程 / 目标 / 后台工作 | App Server 的现有所有者 | 只有显式业务命令和原生生命周期决定；Gateway 退出不接管这些资源 |

### 6.2 插件和连接状态

```text
created -> applying -> RPC ready -> ACP initialized -> serving
                 |                                      |
                 +------------- failure ----------------+
                                                        v
                                                     closing
                                                        |
                                                     disposed

RPC: connected -> recovering -> reattached -> connected
                     |
                 recovery failed -> close ACP connection
```

`apply` 先登记 disposer，之后启动 I/O。客户端插件没有 Session/branch 的 activated 阶段：以 mount 完成、RPC hello 成功和 ACP initialize 成功为各自准入门，不臆造 daemon publication。关闭时先停止新请求/新 runtime 准入，再撤销本地 waiter 和待发送任务、尽力 detach、关闭连接并 join 所属任务；同时发生的 EOF/signal/unload 复用同一结束结果。host 进程关闭允许最多 5 秒的 best-effort detach，然后关闭本地 I/O；这不是原生业务调用完成的保证。

插件只返回终止原因，不直接退出进程。CLI 对正常 EOF/正常卸载返回 0，用户中断返回 130，启动或不可恢复 transport/protocol failure 返回 1；单个 ACP 请求的业务失败通过 JSON-RPC 回复，不结束其他 Session 的服务。嵌入宿主自行决定进程退出策略。

RPC I/O 只有一个 owner，用 `send` / `next` 关联 response 和 notifications。不能持有 Client mutex 等待一个长 RPC，阻塞审批/进度/取消。每个 Session 序列化生命周期变更；prompt 等待终态不阻塞 actor，cancel、审批和问题答案可继续处理；各 Session 可独立运行。连接最多接纳 128 个未结算请求，许可持有至工作结算。输入、native notice 和 native call 队列分别限制为 128、256、128 项。

ACP responses 和主动通知由单 writer 保序。可靠业务输出按序列化大小预留，直到 frame flush，预算为 16 MiB；SDK 自行生成的协议拒绝不计入这项业务预算。每个 frame 上限 16 MiB，单 frame flush 截止时间五秒。可靠输出溢出或写失败就关闭连接并留诊断，不能无声丢工具结果或审批。恢复失败时先为已知等待者生成终态，再最多等待五秒排空输出，避免 SDK 在 foreground 返回错误时丢弃已排队回复。CLI 在插件自有任务结算后使用非等待式 runtime shutdown，避免被对端堵住的 OS stdio worker 阻碍进程退出。Gateway 不给原生 turn 加任意超时。

### 6.3 断线、恢复和多客户端

| 情况 | 必须行为 |
| --- | --- |
| ACP EOF / writer failure / signal / plugin unload | 关闭本地准入，释放本地请求和 subscriptions；按 §4.4 尽力取消自己的 pending login attempt，尽力 session.detach 后断开 RPC。不调用 session.stop/close、daemon.shutdown、Agent close 或 logout；共享执行和已被插件接受的登录交换继续 |
| RPC 连接丢失、ACP 仍在线 | 暂停新变更请求，保留已知关联；重连、submission lookup 和 history catch-up 共用从断线起算的 30 秒截止点。按 0.25/0.5/1/2 秒封顶间隔重连同一受保护 endpoint，不启动第二个 daemon。每次 hello 核验协议/profile/instance，再 attach 和按固定头补历史；排队 call 与异步结果均核验 generation |
| 恢复后 instance id 未变 | 接回当前 runtime；read/attach/history 可重试，已送出的变更只查询结果 |
| daemon instance id 改变 | 旧 RPC ids 和非持久 progress 失效；Session identity 仍用持久 id。按 history 重建 turn 终态；未知 permission id 不擅自重发旧答案，等待 attach 的真实 pending replay |
| 无法在恢复窗口内恢复 | 已收到 cancel 的 prompt 仍 best-effort 返回 cancelled，并记录无法确认 native stop 的诊断；其他在途请求返回 daemon_unavailable，随后关闭 ACP 连接。不报告原生执行已经取消 |
| namespace service_reloading / unavailable | 只影响该功能；已知未投递的 read 可以重试，变更仍服从各自合同。不卸载其他 Session，不安装缺失插件 |
| 同一 Session 被其他客户端操作 | 依原生 sequence 和 owner 结果更新；审批 first answer wins；本连接的 prompt 只认消费自己 input 的 turn |
| 连接关闭时 new/fork 已成功但 response 未送达 | 保留原生 Session，不自动删除或补建；报告不确定性。迟到准备不能重新建立 attach；必要时关闭 RPC 释放它 |

Gateway restart 不恢复旧 ACP request，只允许客户端重新 initialize/load。重连成功不保证客户端旧 prompt 的消息已持久显示；ACP transport 没有可靠消费 ACK，因此不承诺跨 Gateway 进程的 exactly-once 展示。

### 6.4 取消的交接边界

cancel 到达 submit reply 之前时保留 cancel 标志，等拿到 input id 或查询出它，再执行撤回。若 InputQueue 仍持有输入：`session.input.withdraw` 返回 deleted，并以 `input/deleted` 确认未采用；若已应用：返回 notHeld，转 `session.stop`。不因尚未看到 input/applied 就假定一定可撤回。

`session.stop` 针对当前 branch turn，不带预期 turnId；与其他客户端/自动继续并发时，可能停止 stop 真正执行时的另一个 turn。首版不能承诺精确取消指定 turn。已采用、尚未消费的输入在 stop 后可能留待后续输入驱动，这是原生中断合同，不能谎称已删除。用真实终态完成本 prompt；stop 已结算且该输入仍未消费时，可按原生合同返回 cancelled，但保留输入并在之后实际消费时照常展示。

## 7. 输出投影

| 原生数据 | ACP 输出 / 首版处理 |
| --- | --- |
| main `session.model.progress` 与 `step/text` / `step/thinking` | agent message/thought chunks；每 request 维护已发 Unicode scalar 区间。committed delta 是事实，progress 是可替换预览；重放、重连及 emoji 不重复、不截断 |
| main `input/message` + applied/consumed/deleted | 有效 user message chunks；raw-only inputs 不显示，deleted 输入不进入有效回放；自己发出的 prompt 不重复 echo |
| `step/tool-use-start`、`step/tool-use`、`step/tool-call`、`step/tool-start` | tool_call / tool_call_update，真实工具名、输入、位置和 pending/in_progress；ACP toolCallId 由 `(branchId,requestId,callId)` 确定性编码，避免模型在后续 request 重用 callId。native 回答仍用原始身份，显示 id 不充当 task id |
| `step/tool-completed` | 工具已执行的预览；不能提前宣称 tool batch 已提交到上下文 |
| `step/tool-result` | 对应 tool_call_update completed/failed；只读 ToolResult.content 作为文本，最多 16 KiB；保留失败标记，不把 structuredContent 当模型结果 |
| `approval.request`、`approval.resolved`、`approval.cancelled` | permission 请求和本地 waiter 结算；all branches 的审批均转交。显式保留 branchId/requestId，不能用 callId 单独关联 |
| `SessionStore` title notice / native list metadata | 标准 session_info_update；按 revision 更新。manual 可映射 explicit；auto 不足以区分 generated/fallback，省略不可靠的 titleSource |
| `session.context.usage` | 有值时 `usage_update {used:ceil(estimatedInputTokens),size:floor(contextWindow)}`，两个字段按 ACP 的 `u64` 输出 JSON 整数。这是估算上下文量。缺数据时不发送，未知 cost 省略，不能写 cost:0。不要把累计 token 消耗当上下文占用 |
| `context/compaction-start`、`context/compaction` | 真正的 compaction activity；start/terminal 配对，completed/skipped/failed/cancelled 区分。只透传真实 diagnostics，不伪造节省量/耗时 |
| `exec/started`、`exec/ended` | 后台任务的真实开始/终态，按 §8.3 生成独立 tool lifecycle + task metadata；不把原 exec 工具返回当后台结束 |
| `collaboration/request`、`collaboration/delivered`、`collaboration/result` | 按 §8.3 投影最小 subagent snapshot 流；执行身份是 request，不能把可复用的 member Session 当作一次执行 |
| child branch 普通内容 | 首版不混入主线 assistant 输出，也不借状态流声明已经接入 child 输出流 |
| plan / proposal plan / scheduled task | 没有完整、已接入的公开来源，首版不合成 |

共享 [wire.rs](../../crates/dim-client/src/wire.rs) 定义产品事件、progress 和固定头历史页边界，CLI 与 Gateway 直接消费同一类型。保留 input/consumed、turn/start、model/usage 等关联事实，不用 UI running 状态补猜，也不在投影层重复 parse 已知数据。

## 8. Lody 方法和能力逐项决策

### 8.1 合同来源与协商

以固定版本的 [capabilities.ts](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/src/capabilities.ts)、[methods.ts](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/src/methods.ts)、[session.ts](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/src/session.ts) 及其语义注释为接线依据。Core 定义 7 个 request、5 个 notification；标准 ACP 可表达时沿用标准方法，缺字段时放 `_meta.lody`，最后才使用自定义方法。历史 Dim 和参考适配器只提供实现证据，不能覆盖合同。

Agent 在 initialize response 的 `agentCapabilities._meta.lody` 按项声明；Client 在 request 的 `clientCapabilities._meta.lody` 声明。每项独立 version 1，没有“全套 Lody v1”开关。Core 的 Client 类型只有 `subagentEvents` 和 `elicitation`；不能捏造 client usage/rateLimits capability，也不能等待第一次 `_lody/*` 调用才启用已协商的通知。仅 subagentEvents 必须双方都声明；其他项按各自合同执行，不因看到任意 Lody metadata 而全部开启。

Core README 在 capability catalog 中提到 sessionConfig，但固定版本的 `LodyExtensionCapabilities` 未列该字段；实际 DTO 是 `LodySessionMeta.sessionConfig`。首版按明确 metadata DTO 处理，不自创 `sessionConfig:{version:1}` capability。对这个上游文档/类型差异不作兼容字段猜测。

首版计划接入 `sessionHistory`、`compaction`、条件提供的 `forkAtTurn`、后台 exec 的 `tasks:{version:1,background:true}`，以及双向协商的最小 `subagentEvents` 状态流。新会话的 startup config 走已有创建参数。完整 task/subagent 控制、计量和 Goal 不由这些声明隐含获得。

### 8.2 逐项映射

| Lody 表面 | 原生现状 | 首版接法 / 不支持原因 |
| --- | --- | --- |
| `_lody/session/history/read` / sessionHistory | 有 history | 已登记 Session；与 load 同一回放路径，拒绝本连接 prompt 在途；固定头完整输出后返回 `{}`。其他客户端仍可能运行，按固定头处理，不假定 Session 全局静止 |
| `_meta.lody.forkAtTurn` / forkAtTurn | 有 native turn id、turn/end、session.fork | 只接有终态的真实 main turn，按 §4.3 调用；锚点未知、非 main、未结束均报错。标准更新带真实 turnId；不拿 tool call id 或 request id 冒充 turnId |
| compaction / `_meta.lody.activity` | 有完整 compaction facts | §7；metadata 只增强标准工具活动，普通 ACP 仍可显示 |
| `_lody/session/usage_update` / usage / usageScopeId | 有 model/usage；aimux 保留输入/输出总量、可选 cache，丢失 reasoning/text 分区，cost/currency 为 None | 首版不支持。costUSD、reasoningOutputTokens、cacheCreationInputTokens 是可选，不因缺费用而禁用。实际缺口是必填 outputTokens 必须排除 reasoning，cacheReadInputTokens 也必填；上游未知分区不能补零或把总 completion 当文本。后续按 §8.4 从真实事实投影，无需新累计 RPC |
| `_lody/rate_limits/get`、`_lody/rate_limits/update` / rateLimits | 有 account.usage；窗口能表达部分 quota，credits 是单位而非货币，完整 account/model 过滤尚未定义 | 首版暂不接，属于接线/交付范围，不能宣称 App Server 没有额度能力。以后复用真实 usage windows，cap>0 才算百分比，durationHours 转秒，过期时间转 epoch 秒；同长度窗口保留各自项，wallet 缺币种/金额时省略。get 是非 Session 绑定读取，不能强制要求 sessionId；不得造空列表掩盖读取失败 |
| `_lody/session/steer`、`_lody/session/steer_applied`、prompt steer / steering | mode=steer 存在，但不能原子限定目标活跃 turn；空闲 steer 可驱动新 turn | 首版不支持。Core 允许 request/prompt、same/handoff、active/apply，不能把仅缺 same-turn 当作整个协议绝对不可实现。当前也没有已接入的“原生换 turn、同一 ACP prompt 继续承接并确认 injected”的 handoff 路径；不能把下一次 Queue 或闲时新 turn 冒充 handoff。请求报 method-not-found，prompt steer 控制在提交前报 unsupported |
| `_lody/session/goal`、prompt goalControl、goal snapshot / goal | 有 session.goal.* 写入口；无旧 controller snapshot；自动继续跨原生 turns | 不支持。缺少与单 ACP prompt 对应的 Goal 完整结算合同；不能逐字转发方法或按第一次 turn/end 宣称目标结束。普通 Session 上已有 Goal 不因此被修改 |
| `_lody/subagents/list`、`_lody/subagents/cancel`、`_lody/subagents/output` / subagents | 有 collaboration facts、member attach/history 和 addressed stop；查询/取消尚无已接入的执行级归属映射 | 首版不接这组查询/控制，不影响独立 subagentEvents。以后以 request 执行身份、父子归属和单 run 能力分别兑现 list/cancel/output；不能把 member session id 当 task id或取消其不属于当前执行的新工作 |
| `_lody/subagents/event` / subagentEvents | request/delivered/result 能直接给出一次委派的开始、交付和终态 | 条件可接。双向 version 1；首版 snapshot-only，`support:{stream:[],progress:false,outputRead:"none",cancel:false}`，不要求先做完输出/查询/取消。request 关联 opaque runId，断线/观察丢失为 unknown/outputIncomplete；详细接线见 §8.3 |
| `_meta.lody.task` / tasks.background / tasks.scheduled | BackgroundExec 已拥有 detached exec，提交 exec/started、exec/ended | 后台 exec 可接：新建该任务的标准 tool lifecycle，附 `{version:1,taskId,kind:"background",status}`；原生 task_id 为任务身份，父工具只作关联。scheduled 不声明，不把所有后台工作统称为已接入，见 §8.3 |
| `_lody/session/activity_update` / sessionActivity | 有 branch running 和各类后台 facts，没有已接入的完整 requires_action/backgroundTaskCount 投影 | 不支持。不能把 main running=false 当作整个 Session idle |
| sessionTitle / `_meta.lody.titleSource` | 有标准标题通知；当前 auto/manual 不能证明 generated/fallback，初始化也不能保证每 Session 安装标题插件 | 不声明 title ownership；标准标题照常投影，不拦截 Lody 的 title-agent，不加专用环境变量 |
| worktreeProject | 没有相同的逻辑项目归属合同 | 不支持；不把 cwd 当 originProjectPath |
| `_meta.lody.sessionConfig` | session.create 可接 model、modelPreferences、permissionPolicy；attach/fork 没有同等 startup override | new 可接 model 和 mode 选择；显式 modelId 优先于 configOptionValues.model，模型使用本 Gateway 发布的候选映射，不能自行拆 provider id。mode 选择映射到同一 create 的 permissionPolicy.preset。缺 metadata 保持默认。其他配置项、load/resume/fork 的非空覆盖在变更前拒绝；不能先 attach/fork 后 update 冒充建立前采用，见 §8.3 |
| Lody elicitation metadata | Questions 可表达部分表单字段 | 仅实际 question/options/multiSelect/allowCustom；不声明 answerNotes，不合成 secret/preview/自动应答 |
| toolName / notice / messagePhase / plan_mode / proposal plan | 标准工具名和真实错误已有；独立计划模式等来源不完整 | 标准 name/title 保留真实工具名；Lody toolName 仅可写 LODY_TOOL_NAMES 的 ImageGeneration/CronCreate/CronDelete/CronList/ScheduleWakeup，且必须有对应真实 flow，不能写任意 native name。notice 只展示真实诊断。messagePhase、独立 boolean plan_mode、提案计划不支持，plan_mode 不得替代 Permission preset |
| `_dimcode/rewind-files/preview`、`_dimcode/rewind-files`、`_dimcode/reapply-files`、`_dimcode/file-undo-state` | native 没有历史实现中的文件回滚合同 | 不支持，不以 session.fork 冒充文件回滚 |

`_meta.lody` 是附加元数据，不替代标准字段。subagentEvents 只有双向 version 1 才主动发送；不设置“收到第一个 Lody 请求才启用”之类额外总开关。普通 ACP 客户端继续消费标准 task/tool 活动，忽略未识别的 metadata；不向它发送未协商的 subagent 自定义通知。未知 request 返回 method-not-found；未知 notification 仅忽略/诊断，不生成 JSON-RPC response。已识别但不支持的控制字段拒绝执行，其他未知非控制元数据按 ACP 忽略。

### 8.3 首版具体接线与生命周期

**Startup config**：在任何 native 写入之前解析 `{version:1,modelId?,configOptionValues}`。接受 model 和 mode 两个选项；未知选项以参数错误拒绝，错误信息及 data.field 指明配置路径。modelId 若存在即为唯一模型选择，否则取 configOptionValues.model；被覆盖的 model 选项不再参与选择，但整个 DTO 仍须合法。通过安全 catalog 和生成 ACP model option 时保留的映射取得 `{provider,modelId}`，放入同一 `session.create` 请求的 `model`。mode 只接受 `read-only`、`workspace-write`、`full-access`，放入同一请求的 `permissionPolicy:{preset:mode}`，沿用原生显式创建策略；未提供 mode 时不发送 permissionPolicy，保留原生默认。两项可以单独提供，不调用 provider.select/update 或创建后的 permission.update，不改共享默认值。显式模型选择要求 catalog 能解析唯一目标，不能退回默认。load/resume/fork 若携带有效非空选择，在 attach/fork 之前拒绝；空配置无副作用。native 建立成功后才从实际响应生成 config options。

**Background exec**：直接消费已提交的 `exec/started` / `exec/ended`，来源是现有 [BackgroundExec](../../crates/dim-agent-capabilities/src/workspace_tools/background.rs)，不另造 task 业务服务。开始时生成与发起工具不同的稳定 ACP toolCallId，并以 parentToolCallId 关联；这样原工具因转后台返回 completed 后，不会被重新改成 in_progress。任务状态开始为 in_progress，原生 completed → completed，failed/cancelled → failed 并保留真实原因，不能造 Core 不存在的 task cancelled 状态。结束通知允许发生在 prompt response 之后；不因此延长或重新打开那个 prompt。固定头回放重建投影并去重，live 后续仅处理新的 sequence。EOF/卸载只丢弃显示关联，不停止原生后台任务。

**Subagent snapshots**：从主分支 `collaboration/request` 的 event id 识别一次执行，生成 opaque runId，并用其 branchId/requestId/callId 关联 parentToolCallId；member Session 可复用，不能作为 runId。request → pending；匹配 `collaboration/delivered.op_event_id` 的成功交付 → running，失败交付 → failed；`collaboration/result.request_event_id` → completed/failed/cancelled。native result 的文本可作为 summary，不伪装成已订阅 child stream。先发送每个 run 的 snapshot，再处理同一连接内的后续更新；不将别的委派结果匹配到这个 run。仅声明空输出流、无 progress/read/cancel 的最小 support，故不需要额外 member attach 或修改 App Server。RPC 观察丢失时为仍开放的 run 发 unknown + outputIncomplete + disconnected；重连按固定头重建新的观察 activation 和新 runId，不在已终态 run 上倒退。ACP EOF 无法可靠发出终态，释放本地关联即可，不能取消 member 执行。

这些关联都属于现有事实的客户端投影，随连接 activation 释放并可从 history 重建；不是 Gateway 决定 task/subagent 的业务状态。首版不新建持久化表或跨代业务账本。Core 的 [subagent emitter test](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/test/subagent-emitter.test.mjs) 和 [event test](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/test/subagent-events.test.mjs) 是 run 复用、迟到输出、断线和受限 support 的参考向量，不要求引入 Node runtime 或 TypeScript helper。

### 8.4 计量和控制的真实缺口

[usage.ts](https://github.com/LodyAI/acp-extension-core/blob/11e0708df0669568f44e287f17626a3944921dc5/src/usage.ts) 定义 inputTokens/outputTokens/cacheReadInputTokens 为必填且 bucket 不重叠；reasoning、cache creation 和 cost 是可选。当前 aimux 的 input/output total 不足以保证这种分区，未知 cache read 也不能写 0；这是边界保真问题。累计读取 API 并非硬性前置：有完整真实计数时，可以按已提交 model/usage 的 request 身份去重投影 modelUsage，用新的 usageScopeId 区分观察生命周期，继承回放不作为新 delta，compaction/换模型不重置 scope；delta 已包含在 modelUsage，不能双加。未知费用省略，不估造价格。Core accumulator 明确为 process-local，客户端计量投影不等同于原生业务账本；首版不为它添加数据库。

Steering 的 handoff 允许上游换 turn，并不允许脱离 ACP prompt 槽启动无人承接的运行；确认 injected 和 steer_applied 必须有真实采用依据。Goal 的可选统计字段可省略，不能因缺 tokenBudget 就一概拒绝；当前缺的是 owner 的可观察 goal snapshot、控制动作不启动 turn 的保证，以及 set/resume 对应的多 turn ACP prompt 完成合同。仅有返回 sequence 的 session.goal.* 不能满足这些要求。临时 Provider 覆盖和原生配置写入则按 §4.5 分别讨论，不从 Lody 的 Session 配置元数据推导共享 Provider 更新授权。

## 9. 错误和恢复合同

| 错误 | 处理 |
| --- | --- |
| JSON/envelope/参数错误 | ACP 标准 parse/invalid-request/invalid-params；不写入 native |
| 未实现方法 | `-32601`；缺 capability 不是伪成功 |
| `session_not_found` / native session_closed | 拒绝后续请求，清理该 runtime，不建立替代 Session |
| `prompt_in_progress` | 拒绝同 Session 第二个 prompt 或本连接 prompt 中的 load/history；其他 Session 可继续 |
| `unsupported_content` / unsupported metadata control | 映射为参数错误，data.code 保留具体原因，提交前结束 |
| native `service_reloading` / `service_unavailable` | 保留 appError 原因和消息；不把永久不可用当作无限重试 |
| `permission_conflict` / question terminal errors | 刷新真实状态、结束本次操作；不自动替用户覆盖其他客户端决定 |
| `submission_unknown` | 保留 submissionId 和可查询的 Session id，报告不确定；不能提示客户端盲重发 |
| `request_too_large` | 原生帧编码后超过 16 MiB，在写入前拒绝，返回 invalid-params；只结算当前请求，保留连接及其他在途调用 |
| `auth_required` | 仅在可确认所选 managed provider 缺账户时使用 ACP -32000；保留已发布的可执行 auth methods，不从泛化 provider_failed 文案猜认证状态 |
| native mutation reply 丢失 | 报 operation outcome unknown；只做有合同的查询恢复，不自动重复动作 |
| `turn_failed` / `daemon_unavailable` | 保留真实原因；不伪造 end_turn 或 cancelled |

错误的 ACP `data.code` 是 Gateway 的外部错误分类，native appError 作为诊断保留，不再通过错误文本猜原因。原生 RPC 的 -32000 是通用 app error，**不能原样穿透为 ACP -32000**，因为后者专指 Authentication required。ACP 的 parse/invalid-request/method-not-found/invalid-params、request-cancelled、resource-not-found 按其标准 code 输出；可确认需要认证才用 -32000，其余已知业务失败用 Gateway 的 Other code -32001，意外内部错误用 -32603。这样 turn_failed、provider_failed 或存储错误不会错误触发客户端登录。stdout 只允许 ACP，stderr/日志不输出 auth headers、secret references、完整 prompt 或未授权的工具正文。

live `session.submit` 仅将[原生保证集合](../../crates/dim-app-server/README.md#session-submission-errors)中的错误视为确定未提交；集合外的回复同样返回 `submission_unknown` 和两个标识。传输中断沿既有恢复路径查询，不重发输入；本地 `request_too_large` 则已确认未写入。

## 10. 交付单元和验收顺序

| 单元 | 交付路径 | 接线和验收，完成条件 |
| --- | --- | --- |
| A. 共享 RPC 与宿主 | 已完成：dim-client、现有 CLI client/MCP callback 调用者 | 保留 socket/pipe/profile/frame/security 行为；Gateway 构造接收自有输入且 Deps=()，不迁移既有 CLI 插件。2 个公共传输/callback 边界测试、94 个现有 CLI flows、受影响 crate 全 target Clippy、xtask check 通过；10 个需额外环境的既有 CLI flows 未运行。无 ACP handler，无 daemon diff |
| B. 插件与标准会话 | 已完成：dim-acp、CLI acp 入口、共享产品事件边界 | initialize/new/list/load/resume/prompt；3 个 ACP stdio flows 覆盖真实 daemon 与 loopback model、消费输入关联、emoji 去重、固定头空页和并发准备。受影响 CLI flows 共 97 通过，10 个额外环境用例未运行 |
| C. 交互与结算 | 已完成：同一 Gateway 的 auth/permission/question/config/content | managed provider discovery、browser authenticate、terminal helper；输入撤回/stop、审批、form、mode/model、图片/文本资源和 close/delete。101 个 CLI flows 通过、10 个额外环境用例跳过；另有 2 个公共 Gateway 宿主流程验证真实 loopback callback、native exchange 和请求取消。RPC recovery 与输出/卸载结算在 E 验证 |
| D. 有依据的 Lody 子集 | 已完成接线：history/forkAtTurn/compaction、startup model/mode、后台 task、最小 subagent snapshots | 公开 stdio flows 覆盖固定头历史、真实终态锚点、task 与父工具独立、压缩取消、同 member 多次委派的 run 身份、startup model 优先级、三种 permission mode 与非 new 覆盖拒绝，以及断线 unknown/恢复新 activation。Lody GUI 基本聊天通过；不能据协议流测试声明所有 GUI 扩展已验收 |
| E. 连接恢复、宿主退出与实机 | 已完成：RPC recovery、dimcode 原生入口、文档和验收记录 | 本轮 CLI 回归 123 passed、11 ignored、1 个恢复截止用例 filtered；Node MCP stdio 流程单独通过。覆盖不重发不确定变更、请求取消、输出预算、卸载结算。macOS acpx 基本聊天通过；Lody GUI MCP 工具验收受登录页阻塞，真实 OAuth、Windows/Linux 和发布包待验收。证据见验收记录 |

这是串行交付范围，不为每个字段建立独立任务或 agent。实际实现路径引入 crate/依赖时运行 xtask check；装配变化按项目规则生成 graph，禁止手写 generated 输出。

A 的消融复核：传输只保留一份，CLI 继续拥有 daemon 启动策略；删除了可由 hello features 直接判断的重复 shutdown flag。ConnectionConfig 是连接身份、profile 校验和重连所需的实际边界输入；callback 模块只拥有 I/O，没有账户业务。未发现可进一步删除且不损害既有行为的结构。

B 的消融复核：没有服务注册层、业务 Session store 或额外 daemon。每个原生连接仅一个 RPC reader；每 Session 一个准备槽位合并并发建立，保留旧 runtime 只用于建立失败恢复。history cursor 校验和产品事件解析移至共享边界，删除 CLI 的重复定义。

C 的消融复核：Questions 的协议解析移入 dim-client，CLI 和 Gateway 使用同一校验及答案编码；保留原有终端交互。认证只接公开账户方法和宿主 browser/callback 配置，无 token、私有 realm 或独立 OAuth 实现。配置使用原生 CAS 和采用结果，没有第二个 Provider registry。

D 的消融复核：Lody 只是同一投影的 metadata 和受协商约束的通知，没有扩展注册框架、后台执行器或 member Session attach。run 映射由原生 request event 身份重建；唯一额外观察 cursor 防止显式 history 重放重复发布同一 activation 的 snapshots。取消与 close 的顺序由当前 Session 的实际在途操作管理，不新增业务状态机。

### 10.1 自动化公开流程

复用已有 mock-model daemon 和 transport/CLI fixture，在临时 workspace 和隔离 product home 内验证：

1. ACP initialize/版本/能力与 handler 一致；new/load/resume/fork 接入临时 MCP；旧 daemon 缺少接口时接受并忽略且不声明 HTTP/SSE。验证真实工具调用、配置不落盘、显式释放、连接恢复和进程丢失后的租约清理。未知方法、非空 additionalDirectories、音频/二进制资源在写入前拒绝。
2. new → prompt → text/thought/tool → end_turn；单连接多个 Session；同 Session 第二个 prompt 拒绝；另一 RPC 客户端可 attach 同一 Session。
3. progress/committed/history 重复和乱序到达、Unicode scalar offsets、页内无可见事件和 nextAfter 前进、load/load 与 load/resume 单消费者、关闭期间迟到准备。
4. cancel 在提交回复前、持有输入、采用后/消费前、消费后、turn/end 竞态；native 残留输入如实保留，不能断言已删除。
5. 审批 allow/deny、其他客户端先答、native cancel 后迟答；Questions 单选/多选/custom/decline、重复 callId 的不同 request、断线和关闭。
6. RPC 断线/重连/daemon 重启、丢失 mutation reply、lookup unknown 后原请求迟到；确认没有重复 submit/new/fork/attachment/config/approval。
7. EOF、signal、插件 unload 只 detach；从另一客户端观察 turn 继续。显式 close 才结算 Session；其他 Session/daemon 不受影响。可靠输出队列溢出结束连接而不无声丢消息。
8. 标准 model/mode 选项反映真实采用状态；无 catalog/Questions/Attachments 的 profile 只失去对应功能。startup config 的显式 modelId 优先，公布的 mode 可回传到新会话建立参数；无 metadata 保留默认，歧义/未知模型、非法 mode/类型、未知选项/version 报错；new 在同一 create 采用，load/resume/fork 非空覆盖在变更前拒绝；不修改 Provider selection 或共享配置。
9. history、终态 turn fork、compaction skipped/failed/cancelled；后台 exec 开始/终态使用独立 toolCallId，原工具转后台完成不等于 task 完成，取消保留真实原因；终态可发生在 prompt response 之后。普通客户端兼容，Lody capability 和 wire 输出一致。
10. 通过公开 ManagedProvider 测试装配模拟 browser/device：无插件/API-key profile 不被其他账户阻塞、method 只按实际能力发布、已登录 authenticate 不重复 begin、callback 匹配、端口冲突、过期、取消/EOF、complete reply 丢失不重发。terminal method 不能走 authenticate；helper 保留同一 executable/home/profile，完成后重新检查 native 账户状态。断言 native 通用 -32000 错误不会变成 ACP auth_required。复用 callback capture 时保留既有 MCP/TUI 的安全与公开流程回归。
11. Provider 管理插件存在时仍不虚报整组 providers capability；三个 ACP 管理方法不修改 Provider 记录。可复用调用与后续启用后的流验证见 §4.5，不能以 list 可读替代 set/disable 的作用域验收。
12. subagentEvents 双向 version 1 才发送；从同一 member Session 发起的两次委派得到不同 opaque runId，最小 snapshot support 不声称输出/查询/取消。验证 request/delivery/result 关联、回放去重、迟到终态、断线 unknown/outputIncomplete、重连新 activation；不向普通 ACP 或错误 version 客户端发送自定义通知。toolName 只允许有真实 flow 的 Core canonical ID，不能以任意 native 工具名触发客户端特殊处理。

公开流程的实际结果见验收记录；此清单定义所需语义，不能将未单独触发的每个分支都记作测试通过。共享 RPC 提取复用现有 CLI 回归，合并前完整检查仍遵循 AGENTS.md。

### 10.2 本机 acpx 和 Lody 验收

本机客户端为 acpx `0.7.0` 和 Lody Nightly `0.109.0-nightly.0`（bundle build `1.1.39`）。本次构建身份、实际协议观察和未通过项见[验收记录](acp-gateway-acceptance.md)；每次验收重读工具版本/help。

隔离验收的 Bash 示例：

```bash
ACP_TEST_ROOT=$(mktemp -d)
mkdir -p "$ACP_TEST_ROOT/home" "$ACP_TEST_ROOT/workspace"
ACP_TEST_BIN="$PWD/target/debug/dimcode"
ACP_TEST_MODEL_CONFIG=/absolute/path/to/controlled-model.json
ACP_TEST_COMMAND="\"$ACP_TEST_BIN\" acp --home \"$ACP_TEST_ROOT/home\" --cwd \"$ACP_TEST_ROOT/workspace\" --model-config \"$ACP_TEST_MODEL_CONFIG\""
acpx --version
acpx --agent "$ACP_TEST_COMMAND" --cwd "$ACP_TEST_ROOT/workspace" \
  --deny-all --format json --timeout 45 exec 'Reply with ACP_OK.'
```

使用受控 loopback provider / mock fixture，不用个人真实凭据和生产 `~/.dimcode`。本机 acpx 0.7.0 的 config、sessions、queues 使用其用户 `.acpx` 目录，未发现受支持的独立 cache/home 参数；上面的命令仍可能写 acpx 自身记录。手工验收用唯一临时 cwd 和 session 名，不清空或覆盖既有 acpx 配置/记录；需要全隔离时在独立测试用户/容器内运行同一已安装版本。不得改写 shell 的 HOME，也不能臆造 `--home`、`--mcp-config` 等 acpx 参数。上面的 `--home` 属于 dimcode 命令。退出后只关闭本次隔离 home 的 daemon，不用 pkill 处理产品进程。

| 工具 | 场景与证据 |
| --- | --- |
| acpx exec | initialize/new、stdout 纯协议、普通 prompt、工具成功/失败、deny、超时；保存 JSON 帧、工具版本和测试构建 SHA |
| acpx sessions / set / set-mode / cancel | 依本机 help 使用命名 session：new、load/resume、历史、model/mode、第二个客户端取消、close。记录实际使用的方法，不假定 CLI 覆盖全部 SDK 方法 |
| 公开协议驱动 | 覆盖 acpx 0.7.0 未暴露的方法、能力拒绝、并发、drop reply、EOF、故障恢复、elicitation 和 Lody 请求；通过真实 stdio 驱动 SDK Gateway，连接公开 native RPC |
| 账户认证 | 在具备账户插件的隔离构建/home 上验证实际 authMethods：agent authenticate 自动捕获 callback；terminal 支持时另开交互 helper；无 terminal 的客户端不收到 terminal method。验证取消/过期、重复登录、端口冲突、RPC 断线及从同一 home 继续 Session；不保存真实授权 URL/callback/token |
| Lody 自定义 agent | command 使用本次构建的绝对 dimcode 路径；arguments 为 `acp --home <isolated> --cwd <temporary-workspace> ...`，不使用 npx 默认 Dim。确认握手 agentInfo 和实际进程路径 |
| Lody 基本交互 | 新建、重开、图片、工具、允许/拒绝审批、form 选项/custom/decline、取消、切模型/模式；与另一个 native 客户端观察同一 Session |
| Lody 扩展 | history、终态 fork、自动/手动压缩活动；usage/Goal/steer/subagent 等未声明的按钮应隐藏或显示不支持，不允许 UI 发起后静默成功 |
| 生命周期 | 关闭 ACP 连接后 native 执行继续；主动 session close 结束；重启 Gateway 后 load，RPC/daemon 断线和恢复；检查不会重复工具执行或产生第二个 Session |

本机 acpx 0.7.0 的 `--model` / set-model 路径实际调用旧 `session/set_model`，首版预期拒绝；成功的模型切换验收使用 `acpx set model <native-model-id>` 的 `session/set_config_option` 路径，并确认握手/Session 响应发布的是 configOptions。不得把旧入口失败归因于 native 不支持模型切换，也不为通过该入口临时增加兼容别名。

验收记录保留版本/SHA、隔离配置说明、场景、观察到的协议方法/结果和未通过项；不提交真实聊天、凭据或个人路径。真实客户端验收以其实际能力协商为准；acpx 标准兼容不能替代 Lody 扩展验收。

## 11. Issues 对照

关联范围包括 ACP/Lody 需求、相关开放 issue 和已关闭契约 issue 的裁定。开放 issue 的交付描述可能落后于代码；以下以源码和 owner 合同判断能力，不把旧提案当作新 API。这里的关联不表示本 PR 关闭这些 issue。

| Issue | 与首版的关系 / 本计划的处理 |
| --- | --- |
| [#436](https://github.com/arcships/dim/issues/436) ACP requirements | 主跟踪。目标 Lody/acpx、ACP v1；标准 prompt/cancel/approvals/modes/images/questions 优先；main 消息/all-branch 审批；无 client label；§10 定义 done |
| [#434](https://github.com/arcships/dim/issues/434) migration leftovers，U3 | 直接相关的 binding 问题；客户端树的 named dependencies 在 A 单元确认，不以 daemon 内部 raw key 作为可复用合同 |
| [#437](https://github.com/arcships/dim/issues/437) Desktop | 复用 Gateway 包/命令和原有 RPC Session，不开发 Desktop 网关或 UI；Desktop 自身验收归其仓库 |
| [#276](https://github.com/arcships/dim/issues/276)、[#278 裁定](https://github.com/arcships/dim/issues/278#issuecomment-6320361569)、[#281 裁定](https://github.com/arcships/dim/issues/281#issuecomment-6320397540) | 既有裁定要求少改 App Server：turn/question 从 history 推导；审批消费 resolved/cancelled；用量来自事实且标明不足；不新增客户端专用读 API |
| [#47](https://github.com/arcships/dim/issues/47)、[#48](https://github.com/arcships/dim/issues/48)、[#49](https://github.com/arcships/dim/issues/49)、[#58](https://github.com/arcships/dim/issues/58) | 已有方法贡献、interactive、notice 和撤回合同；复用，不重做 |
| [#329](https://github.com/arcships/dim/issues/329)、[#349](https://github.com/arcships/dim/pull/349) | Provider 管理插件、版本化安全视图、局部更新及 CLI 迁移已存在；ACP 的全量配置和进程作用域另见 §4.5，不把原生持久化编辑直接宣称为临时路由覆盖 |
| [#61](https://github.com/arcships/dim/issues/61)、[#26](https://github.com/arcships/dim/issues/26)、[#279](https://github.com/arcships/dim/issues/279) | native fork/delete 与 cut 的边界；fork 不重置 Permission，不等于文件撤销，附件读取/归档不在此新增 |
| [#277](https://github.com/arcships/dim/issues/277)、[#384](https://github.com/arcships/dim/issues/384)、[#432](https://github.com/arcships/dim/issues/432)、[#3](https://github.com/arcships/dim/issues/3) | daemon 可选插件的实际安装/启用另归产品机制；Gateway 不安装业务插件，不因 first-party client plugin 必须等待 marketplace 或结构管理实现 |
| [#403](https://github.com/arcships/dim/issues/403)、[#405](https://github.com/arcships/dim/issues/405) | 外语插件部件定义与 rutis 机制；main 已接入 rutis 0.19.3。Gateway 为 native client plugin，TemporaryMcp 为 native Session part，均直接使用现有机制，不构成外语运行时依赖 |
| [#145](https://github.com/arcships/dim/issues/145)、[#5](https://github.com/arcships/dim/issues/5) | 反向 ACP client 和远程节点不是此交付；共享协议 schema/一致性场景即可，不拉入运行时生命周期 |
| [#263](https://github.com/arcships/dim/issues/263)、[#264](https://github.com/arcships/dim/issues/264) | 复用已有 collaboration facts 投影最小执行状态；查询、输出和 addressed stop 另按每执行归属接入，不能因 snapshot 可读就宣称完整 Lody subagent/task 支持 |
| [#143](https://github.com/arcships/dim/issues/143)、[#422](https://github.com/arcships/dim/issues/422) | Hooks/Pursuit 各自交付；Gateway 不实现 prompt hooks、Goal evaluator 或 Goal 的续轮结算 |
| [#427](https://github.com/arcships/dim/issues/427)、[#428](https://github.com/arcships/dim/issues/428)、[#429](https://github.com/arcships/dim/issues/429) | branches/compaction/prompt 的新要求由原所有者决定；本版本只投影已有 main 与压缩事实，不改变 context 输入路径 |
| [#387](https://github.com/arcships/dim/issues/387) | daemon startup 无进展超时仍归原所有者；Gateway 不修改其策略，30 秒仅为已经断开的 RPC 恢复窗口 |
| [#114](https://github.com/arcships/dim/issues/114) | 交付顺序与评审流程；本 PR 进入代码评审，实施和验证按 §10 记录，不自动合并 |

其余开放项涉及发布、性能预算、CI 或无关功能，不因 ACP 计划扩大范围。#386 的 UI leaf events 不是 ACP transport，不作为接线依赖；#438–#440 的发布验收不在此宣称完成。

## 12. 机制缺口与首版后的评估

本版本向 App Server RPC 框架、Agent core、Timeline、Dispatcher、rutis 提出的机制变更需求为 **零**。临时 MCP 由 MCP 业务模块的独立插件承接；下面记录其他未支持能力，不是本版前置任务。

| 问题 / 为什么现有能力不足 | 将来可评估的通用合同 | 顺序影响 / 所有者 |
| --- | --- | --- |
| steer 没有预期 turn 条件；预查与 submit 之间会换 turn | 在输入所有者处明确“只向指定活跃行为投递”的原子语义，而不是 ACP 专用 if | 准入与写入必须同一决定；输入/Loop 所有者 |
| Goal 方法只写输入，快照及跨轮 prompt 完成无现成合同 | 由 Goal 定义可观察状态和提交/终态关联；不在 Gateway 拥有第二个 Goal fold | 完成不能早于 owner 的决定；Goal，含 #422 的边界 |
| usage 缺 reasoning 明细，费用可能未知 | 由 Usage/模型适配保留真实计数；累计显示仍从已有事实投影 | 持久化兼容需单独决定；ModelProvider/aimux/types |
| 未保存 buffer / client terminal 没有资源生命周期合同 | 沿既有 HostPlatform 方案明确资源所有权与启用方式；客户端 MCP 已由 TemporaryMcp 承接 | 断线不能泄漏进程或赋予权限；HostPlatform/#5 |
| Provider 共享设置与 ACP 临时覆盖的产品语义尚未选择；只有后者缺现成覆盖接口，driver/auth 也固定 | 选择共享设置时复用已有管理合同；选择临时覆盖时才需明确模型连接覆盖的作用域、认证归属、采用时点和关闭语义 | 在实际模型连接采用处生效，退出不能覆盖其他写入；Provider/ModelHost 所有者。首版不提出变更要求 |
| 新建 Session 已有模型参数，load/resume/fork 没有同等建立前配置覆盖 | 若需要该行为，由 Session 建立/恢复所有者明确允许覆盖的配置及采用原子性，不能先启用旧配置后补 update | 配置须在 native establishment 前采用；Session/ModelHost 所有者。首版仅接 new 的 model 选择 |
| 额度管理面的只读暴露未定义 | 明确已有安全 usage read 的访问范围与过滤语义 | 不扩展为任意管理透传；Provider 所有者 |

消融：保留一个 Gateway 插件、一个共享 RPC 实现和必要的连接/投影状态；不建 ACP daemon、第二套 Session store、业务账本、通用扩展注册框架、定时 quota poller、每个协议字段一个插件或 daemon 内的接线插件。共享 RPC crate 用于消除实现复制；每 Session slot/cursor、在途请求、writer 队列分别用于并发准入、history/live 合流、结果关联和可靠输出，不能删除其职责。
