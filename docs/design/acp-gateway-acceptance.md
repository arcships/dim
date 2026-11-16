# ACP Gateway acceptance

验收对象是 [AcpGateway 合同](acp-gateway-implementation.md) 及 [TemporaryMcp](plugins/TemporaryMcp.md) 中明确支持的子集。Agent core、RPC 框架、内置 Session 协议和持久化格式没有改动；MCP 业务目录注册独立临时插件并复用现有服务器实现，持久 MCP 管理保持原行为。PR：[arcships/dim#441](https://github.com/arcships/dim/pull/441)，主需求：[#436](https://github.com/arcships/dim/issues/436)。

## Build and environment

| 项目 | 验收值 |
| --- | --- |
| 日期 / 平台 | 2026-10-26，macOS arm64 |
| 实现源码 commit | `0c3f12d75f0c62af45d1cb9f6ee6f248d535570b` |
| Native executable | `dimcode 0.1.0-alpha.6`，本仓库 `cargo build --locked -p dimcode`，Mach-O arm64 |
| Executable SHA-256 | `e449baef8ec14ba108d767a2bbb841b7b440b79664b8b1f6193296c1cfb4efe1` |
| Cargo.lock SHA-256 | `41ce54e8ffcddcfb2d9c7994be37779b538d1fc37b3a7a9c07ba9def0a366fd5` |
| ACP | protocol v1；Rust SDK 3.5.0，schema 1.12.0；源协议及 Lody Core 固定版本见合同 §2 |
| acpx | 0.7.0 |
| Lody | Nightly 0.109.0-nightly.0，build 1.1.39 |
| 模型 / 账户 | loopback OpenAI-compatible fixture，无真实凭据；managed-provider flow 使用公开 RPC fixture |
| 数据 | 独立临时 home 和 workspace；不使用产品 home。acpx 使用唯一临时 cwd，但仍按其自身实现写用户 `.acpx` 记录 |

## Automated public flows

上表标识本机 acpx 验收使用的构建。以下公开协议回归使用 PR 当前源码，覆盖 live submission 不确定回复、原生帧超限隔离和固定版本 ACP schema 的反序列化。

| 范围 | 结果 / 证据 |
| --- | --- |
| CLI compatibility | `cli_flow -- --skip acp_stdio_recovery_deadline`：123 passed，11 ignored，1 filtered；包括当前 CLI 的旧 daemon 替换行为。本轮没有复跑被过滤的恢复截止用例 |
| ACP stdio | 回归覆盖现有 ACP 和 terminal-auth helper；19 个非 ignored ACP 流程通过，Node MCP stdio 流程另行执行通过。表单和 usage 通知由固定版本 schema 1.12.0 实际反序列化 |
| Zed legacy configuration | 设置 `ACP_STICKY_SESSION=true` 的原生 stdio 流程通过：同 Session 第二次真实模型请求包含前一轮输入和回复；同 cwd 新建 Session 的 id 与上下文独立；resume 原 Session 不重放旧消息。Zed 启动配置见合同 §3.2，GUI 未实机验收 |
| 临时 MCP | native MCP Session suite 13 passed，含真实 30 秒租约过期、失败清理、Session 隔离、无设置写入和真实工具调用；工具列表变更另行复核通过 |
| 既有 MCP | `mcp_flow` 5 passed；`mcp_http_flow` 7 passed、4 ignored。原有配置、连接代际、HTTP/SSE、取消和关闭行为回归通过 |
| 共享传输 | `dim-client` 两个公共连接/callback 测试通过；身份/profile 校验、交错响应、半帧取消和受限 loopback callback |
| Browser authentication | `dim-acp` 三个公共宿主 flow 通过：真实 callback listener/native complete、请求取消、EOF 时先取消自己的 pending attempt 再释放 RPC |
| 静态检查 | 受影响 crate 的 all-target Clippy、格式和 whitespace 检查通过 |
| 架构 | xtask source/crate boundaries、20 个 plugin documents、service catalog 与 service declarations 通过；357 modules / 2120 edges；graph 由 xtask 生成 |
| Node Session interop | rutis runtime/SDK 0.17.1 下，row reload 与 Session metadata 两条流程通过；测试 profile 将必须在创建前可用的 fixture part 声明为 required，不要求迟到的 optional 注册阻塞发布 |

流程使用真实 `dimcode acp` stdio 和公开 RPC socket，而不是替代 App Server trait。production flow 连接本次构建的 App Server 和 loopback HTTP 模型，得到 `ACP_REAL_OK`。

复现入口：

```sh
cargo test --locked -p dimcode --test cli_flow
cargo test --locked -p dim-client -p dim-acp
cargo test --locked -p dim-app-server --test mcp_session_flow --test mcp_flow --test mcp_http_flow
npm ci --prefix runtime --ignore-scripts
cargo test --locked -p dimcode --test cli_flow acp_stdio_temporary_mcp -- --include-ignored
cargo clippy --locked -p dim-app-server -p dim-acp -p dim-cli --all-targets -- -D warnings
cargo run --locked -p xtask -- check
```

恢复截止 flow 和 native MCP 过期 flow 各故意等待真实 30 秒。前者可用 `cargo test --locked -p dimcode --test cli_flow acp_stdio_recovery_deadline` 单独运行；临时 MCP 接线时执行了后者。Node MCP flow 需要已安装的 runtime MCP SDK，复用既有官方 SDK fixture；CI 在 CLI ignored profile 之前安装 runtime 依赖。测试使用自己的隔离 home。

覆盖的行为包括：

- initialize、new/list/load/resume、prompt、close；固定头空页、load/load 和 load/resume 共享准备；标题元数据刷新；resume 不重放旧消息。
- submission reply 与 consumed/turn-end 的竞态、另一 turn 提前结束、Unicode scalar progress/committed 去重；撤回持有输入、无本地 prompt 的 addressed stop、stop/close 的顺序。
- read request 取消不结算其他 waiter；迟到准备不能覆盖关闭后新建的准备；128 个在途请求准入与结算后的释放；慢客户端可靠输出溢出明确失败，原生端只观察到 detach。
- 临时 MCP 首次请求即可见、模型实际调用及工具结果回到模型；headers/environment/cwd 接线；工具列表变更；同 bindingId 幂等、不同配置冲突；过期后重新建立资源且不重发输入；忙碌请求不改变绑定。
- Gateway EOF 结算正在等待的 stdio MCP 调用，官方 SDK fixture 收到取消且子进程退出。native 失败准备撤销资源，30 秒未续租后关闭连接；Session close 同时释放既有和临时连接，持久 MCP 文件保持不变。
- all-branch approval、form 多选/custom、图片和文本 resource；mode CAS 保留工具列表，冲突刷新且不重试；model 使用 catalog 的真实 provider 映射及采用结果。
- live submit 的保证拒绝集合与已接受后的 `submission_unknown`、不透明错误区分；不确定回复包含真实 Session/submission id。16 MiB ACP 帧在原生封装后超限，只拒绝该请求，保留已在途的 list 调用和后续正常提交。
- usage 的 ceil/floor 结果输出 JSON 整数；多选使用 ACP `items.anyOf`。客户端取消、空答案、无效选择或 RPC error 均提交 decline；仅自定义答案仍可正常回答。以上 payload 通过固定版本 ACP schema 反序列化。
- auth discovery、已登录不重复 begin、terminal method 不走 authenticate；helper 用同一 home 重新发现 method，并复用 CLI device flow。
- RPC 断线和 daemon instance 变化；丢失 submit reply 后 lookup 同一 submissionId，重建真实终态；丢失 create reply 后不补建 Session。重连、lookup 和 catch-up 共用 30 秒恢复截止点；排队 RPC 也按连接 generation 隔离。恢复 lookup 不回复时，到期先送达 cancelled + nativeCancellation:unconfirmed，再以连接不可用退出；不宣称 native stop 已执行。
- Lody history、真实 terminal turn fork、startup model 优先级、非 new 覆盖拒绝；后台 task 与父工具分开；compaction cancelled；同 member 的两次委派得到不同 runId。无 capability / version 不匹配时无自定义 subagent 通知；观察中断产生 unknown/outputIncomplete，新 activation 使用新 runId。

## Local clients

acpx 用本次构建的绝对 executable 路径、隔离 home/model config 和临时 cwd，执行 `--deny-all --format json --timeout 30 exec`。观察到：

```text
initialize -> protocolVersion: 1
session/new -> native sessionId
session/prompt -> agent_message_chunk: ACP_ACCEPTANCE_OK
               -> stopReason: end_turn
EOF -> local cleanup and native detach
```

**本构建的 Lody GUI MCP 工具验收未完成。** Lody Nightly 当前显示登录页，没有可用会话，未继续账号操作。不能把公开协议测试或文本聊天当作本构建的 Lody MCP 工具调用通过。

本构建的真实 MCP 调用证据来自公开 stdio → production daemon → 官方 SDK MCP fixture → model 流程；HTTP 及列表变更来自公开 Session 流程。new/load/resume/fork 在支持接口的 daemon 上使用临时绑定，旧 daemon 缺接口时才接受并忽略列表。客户端配置不写入共享 home，非空 additionalDirectories 仍拒绝。

## Registry compatibility

认证入口遵循 [ACP Registry 认证合同](https://github.com/agentclientprotocol/registry/blob/49f768659a8089f07874f56a0d474312695ad861/AUTHENTICATION.md)。Gateway 接受标准的 `clientCapabilities.auth.terminal` 与 Registry 的 `clientCapabilities._meta["terminal-auth"]` 布尔能力；没有能力声明或字符串值 `"true"` 均不授予 terminal 支持。Terminal setup 的 args 替换默认启动参数，包含 `acp`、原来的绝对 home、显式 server/model profile、登录 directory/redirect URI 和 method ID。helper 从原生公开接口重新发现方法，不接收客户端指定的任意 provider。

### 已验证范围

2026-10-27，macOS arm64，Rust `dimcode 0.1.0-alpha.6`，账户构建使用 `crates/dim-oauth` 与本地 build-time client IDs。测试使用独立空 home，没有真实账号、token 或 API key；产品 home 未使用。上游固定为 [Registry `49f768659a8089f07874f56a0d474312695ad861`](https://github.com/agentclientprotocol/registry/tree/49f768659a8089f07874f56a0d474312695ad861)，Python 3.14，由 uv 启动。原始脚本没有修改；临时 manifest 的 binary distribution 指向本机编译归档，不能等同于最终 npm 发布包。

| 检查 | 结果 |
| --- | --- |
| `verify_agents.py --auth-check --agent dimcode` | 通过；从真实账户插件发现 browser/device terminal 方法。使用 `DIMCODE_OAUTH_MANUAL=1` 禁用 browser Agent Auth，保证检查覆盖 Registry 的旧 terminal capability |
| `protocol_matrix.py --agent dimcode --table-mode capabilities` | initialize 和 Session 列表通过；空模型配置的 session/new 返回 `-32001 No available model`；fork/resume 对不存在的测试 Session 返回资源不存在；未声明的 stop/set_model 返回方法不存在。矩阵不是登录或聊天验收 |
| 原生 CLI terminal helper flow | 直接用公布的 args 启动新进程，保留独立 home、自定义 directory、redirect URI 和 server；设备登录经公开 RPC 完成。标准、旧字段、无能力及错误类型均覆盖 |
| Gateway browser auth flows | 公开 managed-provider fixture 验证 callback 完成、取消与 EOF 清理；不访问真实账户 |

Linux x64 的账户构建与同一上游认证/协议门槛已由 [CI run 40466783820](https://github.com/arcships/dim/actions/runs/40466783820/job/122064644884) 验证通过。报告记录源码 `f7cfe6b8f79d453435e3c5e5ad155482230d41dc`、上游 revision、原生版本和平台；auth 为 1 passed、0 failed、0 skipped，矩阵结果与上表一致。该构建仍使用临时归档，未覆盖正式登记 metadata 或发布包。

### 发布与 Registry 门槛

- 目标分发是包含 OAuth 插件且配置真实 build-time client IDs 的账户构建；无 IDs 的 API-key 构建不作为 Registry 的认证分发。不得添加虚假 auth method。
- Registry 稳定字段仅接受 `X.Y.Z`，preview 接受 `X.Y.Z-preview.N` 或 `X.Y.Z`。原生 `0.1.0-alpha.6` 不直接登记，也不伪装成另一个版本。登记的稳定版本仍为 `0.5.21`；preview 低于稳定版本会被上游稳定通道覆盖。须随实际发布确定可用的版本及通道，再填写与该产物一致的分发坐标。
- 最终发布包的 metadata/build validator 和登记版本随正式发布验收；源码 CI 不把 alpha 改名为合法登记版本，也不把临时 binary manifest 当作正式登记 metadata。
- `ci.yml` 的 `acp-registry` 运行于 Linux x64，触发范围见 [GitHub workflows](../release/README.md#github-workflows)。它使用真实 build-time client IDs 编译当前提交的账户版，执行固定上游 commit 的原始 auth checker 与未登录协议矩阵。PR 复用原生 Gateway/helper 的无凭据 flow。
- `scripts/check-acp-registry.mjs` 使用独立 home 和临时归档，强制 checker 通过且不得 skip；矩阵必须只有一个 Dim 结果、版本与待测 executable 一致、initialize/list 成功并声明 terminal auth。已声明的 fork/resume 等探测不得退化为方法不存在、超时或传输失败；未声明的可选方法只记录结果。矩阵进程退出成功本身不代表验收通过。
- initialize 回复的 `agentInfo.version` 与 `--version` 打印的是同一个构建标签：原生 CLI 把标签传给 Gateway，而不是 Gateway 自己取包版本。发布流程未标记的构建因此带 `-source`，有 git 时还带构建提交，形如 `<version>-source+0123abcd`；CI 里未加标记的构建也满足上面的版本一致性断言。
- `acp-registry` 失败或取消会使 `ci-ok` 失败；缺少任一 client ID secret 直接失败，不跳过。报告保留 1 天，包含上游 revision、源码 commit、版本、auth 输出和矩阵；不上传带 client IDs 的 executable、归档或账户文件。native helper 回归仍由既有 CLI flow 执行，不复制上游 checker。

## Limits and unverified environments

- 真实账户、浏览器授权与令牌交换未实机验收；公开 managed-provider RPC、callback 和取消路径已有测试。OAuth client IDs 和凭据不进入仓库。
- Windows 实机、npm 发布包、其余环境依赖 ignored flows 和被过滤的恢复截止用例未验收。Linux CI 仅覆盖上述原生账户构建、认证发现和未登录矩阵；没有 release、publish 或合入 main。
- 本次运行受影响范围的测试和 Clippy；workspace 全量 Clippy/test 及 CI 的 ignored profiles 没有在本机运行，仍须按 AGENTS.md 在合入前完成。
- Lody GUI 的图片、审批、form、model/mode 和扩展展示未实机验收；对应 Gateway 协议流由公开 stdio 测试验证。
- `providers/*`、Goal、steer/handoff、Lody 精确 token usage、quota、subagent 查询/输出/取消、scheduled tasks、client terminal/fs backend 和 MCP-over-ACP 等维持合同中的不支持/延期状态。临时 MCP 只承接服务器及工具，不新增 OAuth 登录或持久凭据管理。

## Ablation review

保留一个 client-host 插件、一份共享 RPC、一个 native reader 和一个 SDK writer。没有 daemon 接线插件、第二个 Session store、扩展 registry、业务账本或常驻账户 poller。连接 generation、准备 ticket、prompt 输入关联、取消 waiter、输出预算和 task tracker 各自保护已列出的公开流程；不将运行时 handle 作为持久身份。EOF/卸载只结算本地资源及自己的 pending login，并尽力 detach；显式 close/delete 才触发原生业务关闭。

MCP 只增加实际拥有资源的 Session 部件和绑定子插件，复用原有 McpServer、工具定义/执行和 rutis 子树清理。绑定表用于幂等及隔离，租约用于没有 RPC disconnect hook 时回收强杀留下的资源；Gateway 保留配置只为续租和重连，不另存业务状态。失败准备的所有权标记阻止未完成请求留下配置，持久 MCP 目录不变。没有第二套 MCP transport、loader、全局服务目录或持久租约。

可靠业务输出按序列化大小预留至 writer flush，预算 16 MiB；SDK 自行生成的协议错误不计入此预算。单 frame 16 MiB，单 frame 五秒内未 flush 则结束连接。恢复失败先生成已知请求的终态并限时排空输出；公开恢复截止用例验证响应在退出前送达。CLI 在插件自有工作已回收后，不等待被对端堵住的 OS stdio blocking worker，避免退出挂住。该宿主退出处理不改变共享 daemon 的执行生命周期。
