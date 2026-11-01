# AcpGateway

状态：合同明确支持的首版子集已实现于 [PR #441](https://github.com/arcships/dim/pull/441)，未发布；证据和限制见[验收记录](../../../design/acp-gateway-acceptance.md)。需求：[#436](https://github.com/arcships/dim/issues/436)。作用域：接入方宿主进程，位于 App Server daemon 之外。语言：Rust。[实施合同](../../../design/acp-gateway-implementation.md) 定义方法矩阵、接线、连接状态机和不支持项。[English](../../../design/plugins/AcpGateway.md)

`AcpGateway` 是将 Dim 作为 ACP agent 提供的可复用 rutis 插件。它拥有 ACP 连接和通往现有 App Server 的独立公开 RPC 连接。`dimcode acp` 是第一个宿主和 stdio 入口；其他宿主装载同一实现，或启动该命令。网关不依赖 TUI。Desktop 继续使用同一个 daemon 和持久 Session，无须另写 ACP 协议实现。

Agent core、RPC 框架、内置 Session 协议、Timeline 和持久格式不变。MCP 业务模块注册独立的 [TemporaryMcp](../../../design/plugins/TemporaryMcp.md) Session 部件承接临时服务器，持久 MCP 管理不变。现有公开方法和事实不能表达的协议能力明确不支持，不在 Gateway 中建立第二个业务状态机。Dim 调用外部 ACP agent 仍属于 [ExternalAgent](ExternalAgent.md)，方向和生命周期独立。

## 1. 结构与所有权

```text
ACP client (Lody / acpx / editor)
             |
             | ACP over stdio
             v
client host's rutis tree
  host I/O and connect configuration
             |
        AcpGateway
    ACP requests / updates / Lody
    request and connection lifecycle
             |
             | existing protected local RPC v3
             v
App Server (existing RPC; MCP business parts)
  existing methods and notifications
             |
Session / InputQueue / Loop / Approval / Questions / Attachments / ...
```

| 部分 | 作用域 | 拥有 | 使用 |
| --- | --- | --- | --- |
| AcpGateway | 接入方宿主进程 | ACP/RPC I/O、协商、请求、订阅、回放、输出投影和清理 | 公开 RPC v3 和 ACP schema |
| CLI 入口 | 接入方 composition root | 参数、已有 daemon 查找/启动、插件装载和退出 | 可复用 Gateway、已有客户端启动策略 |
| TemporaryMcp | 原生 Session，归 MCP 功能 | 临时服务器绑定、工具和清理 | 现有 McpServer 插件、ToolRegistry、Session lifecycle |
| 现有业务所有者 | 当前 process / Session / branch | 执行、持久事实、配置、审批、问题、目标、任务和文件 | 不变 |

Gateway 自身只有一个客户端部件，没有原生 Session 或 branch 部件。它不是 Capability：不拥有分支 Timeline 上的状态机。真实连接资源和在途操作使其成为有自身职责的插件，宿主直接完成装配，不加接线部件。

Gateway 实现位于 `crates/dim-acp`。公开 RPC 引擎、产品事件边界、Questions 合同和 loopback callback I/O 位于 `crates/dim-client`，由 CLI 共用。提取和宿主边界见[实施计划 §3](../../../design/acp-gateway-implementation.md#3-模块宿主与接线)。不假定存在 `AcpHost`、新 daemon endpoint、exported rutis link 或可依赖的私有 `RpcService`。

## 2. 协议边界

目标为 ACP v1 加逐项声明的 Lody Core 0.2.0 能力。ACP v2 和额外 transport 不在首版。固定来源见[计划 §2](../../../design/acp-gateway-implementation.md#2-核对基线和协议清单)，SDK 包版本不是协议版本。

每个稳定 v1 agent/client/protocol 方法、选定的 unstable 表面和 Lody 方法，分别在[计划 §4](../../../design/acp-gateway-implementation.md#4-稳定-acp-v1-方法逐项映射)、[§8](../../../design/acp-gateway-implementation.md#8-lody-方法和能力逐项决策) 判定。能力只来自已实现 handler 和真实 native availability，不复制历史能力对象。

首版覆盖标准 Session create/list/load/resume、prompt/cancel、工具输出、审批、图片和文本资源。账户认证、mode/model、表单提问、close/delete、fork 按现有 native 接口的可用性提供。Lody 子集为 history、compaction、终态 turn fork 锚点、新会话 startup model 和 permission mode、detached exec task lifecycle 和双向协商的 snapshot-only subagent events。steering、Goal 控制、token 统计、额度、subagent 查询/输出/取消、scheduled tasks、session activity、标题接管、项目归属及文件回滚不在首版；计划区分原生合同缺口与交付取舍。

Lody 方法和 metadata 按固定 Core DTO 及语义注释实现，不设连接级 Lody 总开关，不从一项推导整套支持。Subagent snapshot 可以如实声明无输出流、progress、read 或 cancel；最小投影不需要旧 SDK task API。准确协商、native fact 关联、startup config 和各项限制见[计划 §8](../../../design/acp-gateway-implementation.md#8-lody-方法和能力逐项决策)。Lody canonical tool ID 不能填任意 native 工具名。

网关不调用 client fs/terminal，不暴露任意账户管理、不注入 raw blocks。new/load/resume/fork 通过 TemporaryMcp 绑定 stdio/HTTP/SSE mcpServers；配置只在内存中，与原生工具并存。旧 daemon 缺少临时绑定接口时接受并忽略列表，且不声明 HTTP/SSE MCP 能力。非空 additionalDirectories 在创建或 attach 前报错。未知非控制 metadata 按 ACP 扩展规则处理；识别到尚不支持的控制字段，在提交输入前拒绝。

socket/pipe 身份校验与模型账户认证分开。网关从现有 account.providers/status 发现 managed provider，并只发布真正可执行的 auth method。agent-type browser 方法通过 authenticate 调用现有 begin/complete/status/cancel；terminal 方法另开交互 helper，不能把其 methodId 发给 authenticate。OAuth、凭据交换和持久化仍归 provider。准确调用、callback 所有权、headless 限制和取消合同见[计划 §4.4](../../../design/acp-gateway-implementation.md#44-账户认证与现有-oauth-插件)。没有账户插件时不强制 configured/API-key 模型登录。

Provider 管理已经归 daemon 的 ProviderPlugin 所有，CLI 使用其公开方法。Gateway 复用该所有者，不新建 Provider registry。[计划 §4.5](../../../design/acp-gateway-implementation.md#45-provider-管理与现有插件) 将 providers/list 接到 provider.management，set/disable 接到带版本的 provider.update，明确完整 headers 替换和 credential 归属。ACP 修改共享持久化设置还是临时 routing，仍待产品选择；持久化本身不禁止 capability，Core 也没有定义这组 Provider API。选择未定前不启用整组 capability。仅临时 routing 缺现成覆盖接口，共享管理已经存在。

## 3. 状态与生命周期

内存只含连接协商/generation、Session 映射、操作槽位、两端请求关联、submission/input/turn ids、固定历史 cursor、流式 Unicode offsets、本地 permission/question waiter 和 task/run 显示关联。task/run 投影从已提交 native history 重建，不由 Gateway 决定业务状态。没有持久 store，runtime handle 不充当 durable identity。

[计划 §6](../../../design/acp-gateway-implementation.md#6-生命周期与连接管理) 定义 close-once、任务结算、队列上限、启动准入、重连、generation fencing 和多客户端。宿主提供独占 ACP I/O 与连接配置；先登记清理再启动 I/O。客户端树不使用 Dim Session/branch publication；mount、RPC hello 和 ACP initialize 分别控制其自身准入。

- ACP EOF、signal、写失败或插件卸载停止本地准入、释放本地等待、detach/断开 RPC；不停止/关闭 Session 或共享 daemon。
- 显式 `session/close` 经 native session.close 关闭业务 Session，影响其他客户端，不等于断线或 detach。
- RPC 丢失时暂停变更，通过现有 endpoint/handshake 恢复、attach、读事实；不盲重发变更。
- 已结束 generation 不能登记迟到的 new/load/fork 结果、发迟到回答或写入下一连接。

不加 loader、生命周期注册表或每 Session daemon relay。宿主在构造时交出独占 I/O、连接及其重连配置；Gateway 不读取外部服务，声明 Deps = ()。既有 CLI 插件在 [#434 U3](https://github.com/arcships/dim/issues/434) 的依赖迁移另行讨论，不引入 AgentBuilder client host 或 raw service-key 依赖。

## 4. Session 与 prompt 流程

### 4.1 创建、装载和回放

创建普通交互 Session（session.create 省略 interactive），然后经 RPC attach。已有 Session 保留自己的配置。并发 load/resume 共用每 Session 的一个准备/runtime 槽位。

load 固定 native 历史头，按 nextAfter/done 分页，按分支 sequence 合并缓存 live，回放写完才响应。resume 重建同一投影但不输出旧消息。raw-only 和 deleted inputs 不成为普通 user message。标题 revision/progress snapshot 各按公开合同处理，因为它们没有 durable sequence。算法见[计划 §5.2](../../../design/acp-gateway-implementation.md#52-固定头与-live-合流)。

### 4.2 Prompt 与取消

一个 ACP prompt 写成带唯一 submission id 的 Queue session.submit。回复由该 input 被删除，或消费它的 turn 的终态决定；别的客户端的 turn 不决定本 prompt。progress/committed text 通过每 request offset 合流，避免重复。

取消尊重 InputQueue → Loop 的交接：held 输入走 withdrawal，随后按需 session.stop。现有原生合同不能保证 stop 精确指向预期 turn，也不能保证已 adopted、未 consumed 输入已删除。状态、顺序和竞态见[计划 §5](../../../design/acp-gateway-implementation.md#5-prompt回放和响应关联)、[§6.4](../../../design/acp-gateway-implementation.md#64-取消的交接边界)。

本连接收到 cancel 后，prompt 必须返回 ACP cancelled，即使取消触发底层异常。Hooks 对 session.submit 的 input_blocked 拒绝显示原消息并返回 refusal；Gateway 不实现 Hooks。

### 4.3 审批与提问

所有分支的 native approval request 转为 ACP permission request。回答走 approval.respond；原生当前 policy 和 first-answer 顺序为准。resolved/cancelled 结束本地 waiter，不提供 always 选项。

client 声明 form elicitation 后，pending ask_user 转为 elicitation/create，回答走现有 session.question.respond。pending question 来自 tool-call/result，不加 server read API。client 不支持 elicitation 时不删除 native 工具；问题可由另一个 Dim 客户端回答，Gateway 显示等待状态。MCP-server elicitation 和 ACP URL elicitation 不支持。

### 4.4 提交不确定性

submission id 是查询键，不是 native exactly-once 保证。是否保证未写入由[原生提交错误合同](../../../../crates/dim-app-server/README.md#session-submission-errors) 决定。不确定失败后，accepted lookup/history 接回原 input；unknown 不证明原调用不能迟到提交。不能自动重发或换新 id。其他变更也遵守同样边界，除非 owner 明确提供更强合同。

## 5. 安全与上下文

保留既有 socket ownership/权限、Windows authenticated pipe、协议/profile 检查及 frame limit。使用明确 native-method 白名单，ACP 方法不能变成任意 RPC proxy。Session 操作外只读取安全 provider catalog，并执行计划 §4.4 中自有的登录流程；原生 Provider 管理的复用与首版写入边界见 §4.5。不开放 logout、quota 或任意 account management。provider 私有 realm、client ID 和 token 流程不进入本仓库。

ACP 内容经现有 session.submit.user 进入，外部边界完成解析和转换。Gateway 不写任意 Timeline 事件、raw context blocks、插件设置或 host tools。模型只看到正常 adopted input/tool result，不注入 ACP identity 或 transport diagnostics。

stdout 仅 ACP。诊断不泄漏凭据、账户 secret、完整 prompt 或无关工具正文。慢/失败输出无法可靠维持时显式断连，不静默丢结果或审批。

## 6. Rule check

规则：[principles §1/§4](../../../architecture/rust-agent/principles.md)、[ADR 002 D1–D5](../../../adr/002-plugin-scope.md)、[plugin contract](../../../design/plugin-contract.md)。

| 规则 | 适用方式 |
| --- | --- |
| §1 Capability / server boundary | 一个客户端进程插件拥有真实 I/O 与请求，不是 branch 状态机，RPC 不放 ACP 业务；临时 MCP 归原生 MCP 所有者 |
| §4.1 指令与结果 | 只调现有 Session 入口，由 owner 决定并写结果，网关仅投影 |
| §4.2 交接 | input/applied 后用 Loop 的 stop，不能假装 adopted 输入已撤回 |
| §4.3 before | 不注册，只做 ACP 外部参数/内容边界校验 |
| §4.4 机制通用 | 不改 Timeline/Dispatcher/runtime/RPC 框架/rutis；不支持项保持不支持 |
| §4.5 原子事实与顺序 | 读已提交事实及分支 sequence，不从本地协议队列制造执行决定或批次 |
| §4.6 上下文 | 只提交普通输入和已有工具答案，无 runtime injection |
| §4.7 公开合同 / 所有权 | 读 RPC/history/notice，不读内部状态，不复制 Goal/task/usage 业务状态 |
| §4.8 机制需求 | 首版为零，后续缺口按计划 §12 记录所有者 |
| §4.9 必需事实 | 不定义 Timeline 事件；连接和展示临时状态留内存 |
| D1 Scope | 跨 Session 连接和在途工作归接入方进程 |
| D2 No wiring modules | 网关拥有资源、请求和清理；宿主直接装配 |
| D3 Dependencies | 指向公开 RPC，核心不依赖网关 |
| D4 Configuration | 启动输入与 daemon business settings 分开；无新 source 或结构管理入口 |
| D5 Language-neutral | ACP/RPC 是 wire 合同，Rust/SDK 类型和本地 I/O 所有权是绑定 |

## 7. Requirements raised to mechanism owners

首版没有机制变更需求，不把未来补齐藏为前置。[计划 §12](../../../design/acp-gateway-implementation.md#12-机制缺口与首版后的评估) 记录不支持的语义、现有合同为何不够、可提出的通用问题和 owner，首版验收后评估。

## 附录 A. 实施与验收

### A.1 CLI 入口

`dimcode acp [--agent ID]` 复用明确 home/cwd/server/model-profile 选项。在客户端 rutis tree 装载 Gateway，不装 Tui、UiLeaf 或 daemon ACP 部件，不触发终端升级提示、不把 stdin 当一次性 chat prompt。stdout 只有协议；独立的 `--auth-setup METHOD_ID` helper 使用已有交互登录流程。

### A.2 ACP 方法

稳定、unstable 和 extension 的完整判定见计划 §4/§8；不能从 SDK 类型或历史实现推导当前能力。

### A.3 Prompt 内容与更新

内容映射见计划 §4.3，事件投影见 §7。当前 CLI 显示子集不够，不能用 UI 状态补猜缺失事实。

### A.4 验收

[计划 §10](../../../design/acp-gateway-implementation.md#10-交付单元和验收顺序) 定义公开 flow 和隔离客户端命令。[验收记录](../../../design/acp-gateway-acceptance.md) 记录实际结果：acpx 基本聊天和公开 native MCP 工具流程通过；Lody 当前停在登录页，GUI MCP 工具验收未完成。客户端 MCP 配置由原生 Session 的临时绑定承接。Desktop UI、Dim 调用外部 ACP agent、资源节点和全量 Lody parity 不在本次交付中。

### A.5 Client label

持久 Session 没有原生 client-origin 字段，ACP Session 筛选和 durable createdBy label 不支持；不拿 marker、interactive 或私有映射冒充。普通 Session 仍可由其他 native 客户端观察。
