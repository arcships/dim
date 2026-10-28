# MCP OAuth

状态：守护进程侧（维护者在 [#111](https://github.com/arcships/dim/pull/111) 评审通过）和原生 CLI（§13）已实现。路线图项 F4
（[#75](https://github.com/arcships/dim/issues/75)）。总体设计：
[附录 B.2 Mcp 行](../overview.md#b2-app-server-进程插件)。阶段：P2。语言：Rust。
作用域：放在现有的 Mcp 进程插件内（[ADR 002](../../adr/002-plugin-scope.md)）。依据：
MCP 规范修订版 **2026-07-28**，即当前版本
（[Authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)）。

## 1. 摘要

HTTP 或 SSE MCP 服务器回答 `401` 时，连接不再就此失效，而是走 OAuth。Mcp 发现授权服务器、
注册客户端，把授权 URL 交给用户；用 PKCE 和资源指示完成授权码交换，保存令牌，并在使用时刷新。
等待该服务器的 Session 会自行重连。

所有部分都在 Mcp 内，它是 MCP 配置与连接的所有者：

- 一个进程级凭据权威，带自己的 0600 存储；
- 现有的 Session 发现与连接实例，由它们附加 bearer 令牌；
- 三个新管理方法：`mcp.auth.start`、`mcp.auth.complete` 与 `mcp.auth.logout`。

F4 不新增插件和 Timeline 事件，不改基础设施和共享合同，也不依赖 S8b。完成标准：受 OAuth
保护的 MCP 服务器能连接并刷新令牌（附录 B）。

OAuth 协议步骤来自工作区里已有的官方 Rust SDK：rmcp 3.5.0 的 `auth` feature。其余都归 Mcp：
编排、存储、用户流程，以及 rmcp 留下的缺口，其中三处是规范的 MUST（附录 A）。

## 2. 依据

| 来源 | 确定了什么 | Dim 采用什么 |
| --- | --- | --- |
| MCP 2026-07-28 [Authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)、[Authorization Server Discovery](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery)、[Client Registration](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration)、[Security Considerations](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/security-considerations) | OAuth 2.1 加 PKCE `S256`。受保护资源元数据（RFC 9728），来自 `WWW-Authenticate` 的 `resource_metadata` 或 well-known URI（先带路径的形式，再根路径）。授权服务器元数据按固定顺序走 RFC 8414 或 OIDC 发现，`issuer` 须与所用 URL 一致。注册优先级：预注册，然后客户端 ID 元数据文档（CIMD），然后动态客户端注册（RFC 7591，已弃用），最后由用户输入。授权请求和令牌请求都带 `resource`（RFC 8707）。校验 `iss`（RFC 9207）。缺少 `code_challenge_methods_supported` 时客户端 MUST 拒绝。授权服务器端点 MUST 用 HTTPS，重定向 URI MUST 是 `localhost` 或 HTTPS。每个请求都带 bearer 令牌，绝不放进查询串。`403 insufficient_scope` 要求以 scope 并集提升授权。授权服务器轮换公共客户端的刷新令牌 | §§5 至 10 中所有规范性要求 |
| rmcp 3.5.0，feature `auth`（`src/transport/auth.rs`） | `auth` 模块覆盖元数据发现、DCR 与 CIMD 客户端选择、PKCE 与 `state`、带 `resource` 和 `iss` 的授权码交换，以及带轮换的刷新。`CredentialStore` 与 `StateStore` trait 可替换。它与传输层共用同一个 reqwest 0.13 | 协议引擎；缺口见附录 A |
| Claude Code（`/mcp`、`claude mcp login`、[文档](https://code.claude.com/docs/en/mcp)） | `.mcp.json` 中的 `oauth` 对象：`clientId`、`callbackPort`、`authServerMetadataUrl`、`scopes`。重定向为 `http://localhost:PORT/callback`；曾改成 `127.0.0.1`，使按注册 URI 精确匹配的服务器失败，后已改回。重定向失败时粘贴回调 URL。客户端密钥不进配置。遇到 `401` 先刷新，再重试一次 | `oauth` 的字段名和类型、重定向形式、粘贴兜底、密钥不进配置 |
| MCP TypeScript 与 Python SDK（`OAuthClientProvider`、`TokenStorage`） | 存储与重定向处理由宿主注入。凭据带 issuer 标记，不匹配时丢弃。`invalid_client` 清除注册，`invalid_grant` 清除令牌 | 所有者提供的存储、issuer 绑定、清除规则 |
| Dim 账号登录（`account.login.browser.begin/complete`、`managed_oauth.rs`、`dim-cli/src/login.rs`） | 守护进程返回 `{attemptId, authorizationUrl, expiresAt}`。客户端把粘贴的回调 URL 送回，输入时隐藏回显。令牌存于 home 根目录的 `auth.json`，经 FileStorage 租约原子替换为 0600 | 尝试的形状、粘贴输入、存储方式 |

## 3. 结构

| 部分 | 作用域 | 变更 | 依赖 |
| --- | --- | --- | --- |
| `McpAuth`，`McpPlugin` 内的凭据权威 | 进程 | 新增，Mcp 内部 | `FileStorage`（home 根目录，文件 `mcp-oauth.json`）、rmcp `AuthorizationManager` |
| Mcp 管理方法（`mcp.*`） | 进程 | 新增 `mcp.auth.start/complete/logout`；`mcp.catalog` 条目增加 `auth` | `McpAuth`、Settings（现有） |
| `McpSession` | Session | 报告 `auth_required`；条目的凭据变化时重新发现这些条目 | `McpService`（现有）、`McpAuth` |
| `McpServer` 连接实例 | Session loader 实例 | 每个请求附加 bearer；向 `McpAuth` 报告 `401` 和 `403`；`auth_required` 之后可以重连 | `McpAuth` 取代只用于排序的 `Gate<McpService>`（同一插件，重载传播不变） |

- **重载。** Mcp 之外没有部件依赖这些部分。`McpPlugin` 像 `OpenAICodexOAuthPlugin::new(home_root)`
  一样在工厂里接收 home 根目录，并在 `Deps` 中增加 `Arc<FileStorage>`，属于核心到核心的依赖。
- **独立宿主。** 注册 `McpPlugin` 的宿主提供同一个根目录。
- **生命周期。** Mcp 重载会丢弃待完成的尝试，它们只在内存中；存储保留。
- **装配。** Mcp 只在本地执行（`execution_root`）时装配，因此 OAuth 恰好在有 MCP 的地方存在。

## 4. 数据与状态

### 4.1 凭据记录（持久化）

每个凭据 key 一条记录。key 是服务器 `id` 加规范化的服务器 URL：配置中的 `url` 经 URL
解析器序列化（scheme 和 host 小写，省略默认端口），单独的 `/` 路径去掉。key 是进程级的，
与配置来源和 Session 无关（§10 D8）。

```text
// mcp-oauth.json，位于 home 根目录，权限 0600，经 FileStorage 原子替换
{ "version": 1,
  "credentials": [
    { "serverId": "github", "url": "https://api.example.com/mcp",
      "resource": "https://api.example.com/mcp",   // 授权请求发出的原值
      "issuer": "https://auth.example.com",        // 记录的授权服务器
      "tokenEndpoint": "https://auth.example.com/token",
      "revocationEndpoint?": "https://auth.example.com/revoke",
      "client?": { "clientId": "…", "clientSecret?": "…",
                   "registration": "preregistered" | "dynamic",
                   "redirectUri": "http://localhost:1456/callback",
                   "authMethod": "none" | "client_secret_basic" | "client_secret_post" },
      "tokens?": { "accessToken": "…", "refreshToken?": "…",
                   "expiresAt?": 0, "scopes": ["…"] },   // expiresAt：Unix 毫秒
      "requestedScopes": ["…"] } ] }
```

- **唯一写入者。** 只有 Mcp 写这个文件。
- **自足。** 记录带有令牌端点、撤销端点和客户端的认证方式，因此刷新和登出不需要重新发现。
  记录在尝试完成时连同客户端和令牌一起写入；放弃的尝试不留下任何东西。
- **写入路径。** 写入时取存储的 FileStorage 锁，重读文件，只替换自己 key 的记录，再以 0600
  原子替换文件。Windows 上沿用 FileStorage 的 DACL 处理。替换已经生效、只是同步失败的写入算作已
  写入：读者已经看到它，崩溃时丢失它的结果与 §7 中进程被杀相同。
- **令牌请求持锁。** 授权码交换、刷新和撤销从重读开始，经过请求，到替换为止一直持有这把锁，
  与托管 OAuth 宿主相同。两个守护进程可能共用一个 home（例如 `--product-home` 相同而
  `--database` 不同），这把锁防止二者兑换同一个会轮换的刷新令牌。因此不同服务器的令牌请求
  会互相等待，每个受 30 秒超时约束；这类请求很少。发现和注册在锁外进行。
- **发出即结算。** 已发出的令牌请求在 `McpAuth` 自己的任务中接收并持久化，不论谁在等待，所以
  调用方离开或卸载都不会丢掉轮换后的刷新令牌或已兑换的授权码。Mcp 卸载时会等待它。若授权码交换
  期间其尝试已被取代，或其记录无法写入，响应照样接收，但得到的令牌改为撤销并丢弃（§7）。
- **未知版本。** 所有服务器的 OAuth 都不可用（`mcp_auth_unsupported`），文件保留，绝不覆盖。
  此时连接发出的请求不带 bearer，不需要登录的服务器照常工作。以后不兼容的格式使用新版本号，
  并单独做格式决定。
- **过期。** 没有 `tokens` 的记录即为过期。刷新被拒时删除 `tokens`，收到 `invalid_client` 时
  还删除 `client`，重启后状态仍在。

### 4.2 进程内存（不持久化）

| 状态 | 内容 | 生命周期 |
| --- | --- | --- |
| 质询 | 每个 key：任一连接最近一次看到的 `WWW-Authenticate`（`resource_metadata`、`scope`、`error`） | 到授权成功、登出或 Mcp 重载为止 |
| 尝试 | `attemptId`、key、PKCE verifier、`state`、预期 `issuer`、元数据、客户端、重定向 URI、scope、`expiresAt` | 每个 key 至多一个；在 complete、过期（10 分钟）、登出或同一 key 再次 start 时结束（§7） |
| 尝试锁 | 覆盖所有 key 的尝试与已完成登出的一把进程内异步锁，是尝试状态唯一的顺序（§7）。持有者从不等待存储锁或网络请求 | Mcp 代 |
| 存储锁与变化通知 | FileStorage 锁前面的一把进程内异步锁；FileStorage 锁立即准入或失败，因此被另一进程持有时会重试，与托管 OAuth 宿主相同。另有每个 key 一个 watch，key 的记录每次变化时通知，并带上记录是否持有令牌：处于 `auth_required` 的 Session 在任何变化时重试，就绪的 Session 在令牌消失时作出反应 | Mcp 代 |

PKCE verifier 与 `state` 从不持久化。守护进程重启会放弃待完成的尝试。没有记录缓存：每个请求
不加锁地读取文件，这是安全的，因为写入都是原子替换；同一 home 上另一个守护进程做的刷新也能
被看到。

### 4.3 配置（`mcp` 业务来源）

远程条目可以带一个 `oauth` 对象，字段名和类型与 Claude Code 的 `.mcp.json` 一致，
因此与它共用的文件在 Dim 中也能通过校验：

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `clientId` | 字符串，非空 | 预注册的公共或机密客户端（注册优先级 1） |
| `callbackPort` | 整数 1 至 65535 | 重定向 `http://localhost:{port}/callback` 的端口；默认 1456 |
| `authServerMetadataUrl` | `https` URL，或回环 `http` | 服务器没有有效 PRM 时使用的授权服务器元数据（§5.2） |
| `scopes` | 字符串，空格分隔（RFC 6749 §3.3） | 固定请求的 scope（§5.5） |

- **未知字段。** `oauth` 中的未知字段保留并忽略，与条目级扩展字段一样。`McpSettings::validate`
  已把 `oauth` 列为保留键，所以没有已存文档会改变含义。
- **静态凭据排除 OAuth。** 带 `auth`（静态 bearer）或 `Authorization` 请求头的条目从不使用
  OAuth。`oauth` 与二者之一同时出现是 `mcp_invalid_config`。
- **没有密钥字段。** `oauth` 不带任何密钥。机密客户端的密钥在登录时输入（§5.3），保存在记录中。

## 5. 流程

### 5.1 发现与授权

```text
 CLI（本地传输）            守护进程：Mcp                     MCP 服务器            授权服务器
  |                          | Session 连接                      |                        |
  |                          |-- POST initialize --------------->|                        |
  |                          |<- 401 WWW-Authenticate: Bearer resource_metadata=…, scope=…|
  |                          | 记下质询；状态 auth_required；不发布工具                   |
  | mcp.auth.start {id}      |                                   |                        |
  |------------------------->|-- GET resource_metadata --------->|                        |
  |                          |<-- PRM（resource、AS 列表）-------|                        |
  |                          |-- GET /.well-known/oauth-authorization-server ------------>|
  |                          |<- 元数据：issuer、端点、S256 ------------------------------|
  |                          |   （检查：issuer、PKCE S256、HTTPS）                       |
  |                          |-- POST 注册（仅 DCR）------------------------------------->|
  |                          |<- client_id -----------------------------------------------|
  |                          | 尝试：verifier、state、issuer（只在内存）                  |
  |<- {attemptId, authorizationUrl, redirectUri, resource, issuer, expiresAt}             |
  | 显示 resource 与 issuer；打开浏览器 ------------------------------------------------->|
  |                          用户在授权服务器登录并同意                                   |
  |<- 浏览器 -> http://localhost:1456/callback?code&state&iss（CLI 监听，或粘贴）         |
  | mcp.auth.complete {attemptId, callbackUrl}                                            |
  |------------------------->| 检查重定向 URI、state、iss        |                        |
  |                          |-- POST token：code、code_verifier、resource -------------->|
  |                          |<- access_token、refresh_token?、expires_in ----------------|
  |                          | 持久化记录（0600）；递增 key 版本                          |
  |<- {}                     | 处于 auth_required 的 Session 重连                         |
  |                          |-- initialize, Authorization: Bearer … --->|                |
```

时限：Mcp 交给 rmcp 的 HTTP 客户端每个请求超时 30 秒，与 rmcp 自己的默认值相同，Mcp 自己的
令牌请求也用它；撤销请求 10 秒；一次尝试存活 10 分钟。Mcp 卸载会取消进行中的发现和注册，但已
发出的令牌请求先结算（§4.1）。调用方离开不取消任何东西（管理方法规则）。

### 5.2 发现规则

每次 `mcp.auth.start` 都重新发现，不在尝试之间缓存。Mcp 使用 rmcp 的发现，它遵循规范的顺序，
另有附录 A 列出的补充。

1. **受保护资源元数据。** 使用记下的质询中的 `resource_metadata` URL。没有质询时（例如重启后），
   先试 `/.well-known/oauth-protected-resource{path}`，再试根路径形式。元数据 URL 必须与服务器
   同源，`resource` 必须是服务器 URL，或同源下它的父路径（§10 D16）。不符为
   `mcp_auth_discovery_failed`。
2. **授权服务器元数据。** 按顺序尝试 `authorization_servers` 的条目，每个都按规范为带路径和不带
   路径的 issuer 规定的 well-known 顺序；`issuer` 必须等于构造 URL 所用的 issuer。issuer 不符时
   发现立即失败，而不是换下一个条目（rmcp 的行为，失败时关闭）。
3. **没有有效 PRM。** 规范写明"MCP clients MUST use OAuth 2.0 Protected Resource Metadata for
   authorization server discovery"，其发现流程在找不到时以"中止或使用预先配置的值"结束。
   - 设置了 `oauth.authServerMetadataUrl` 时使用该文档。well-known 形式的 URL 按发现结果同样校验；
     其他 URL 是配置给定的信任锚点，记录其 `issuer`。此时资源为 rmcp 授权请求序列化后的服务器
     URL，记录原样重复这个值。
   - 否则为 `mcp_auth_discovery_failed`。
   - Mcp 从不在服务器源上猜测授权服务器，也从不使用 rmcp 合成的 `/authorize` 与 `/token` 端点。
   - 有效 PRM 总是优先于覆盖值，因此对发布了 PRM 的服务器，项目文件无法把用户引到另一个授权
     服务器。
4. **拒绝。** 没有 `code_challenge_methods_supported`，或列表中没有 `S256` 的元数据被拒绝；
   不是 `https` 的授权、令牌、注册或撤销端点也被拒绝，回环 `http` 除外。二者都是
   `mcp_auth_discovery_failed`。非回环主机上纯 `http` 的服务器 URL 是 `mcp_auth_unsupported`：
   令牌从不以明文传输。
5. **issuer 绑定。** 记录把客户端和令牌绑定到记录的 `issuer`，这是规范对客户端凭据的要求。
   `start` 发现 PRM 现在指向另一个授权服务器时，为旧服务器记录的任何东西都不再使用。DCR 或
   CIMD 客户端重新注册。配置的 `oauth.clientId` 以 `mcp_auth_client_required` 拒绝，因为规范
   要求报错，而不是使用不匹配的凭据；登出会删除记录，下一次 `start` 就把配置的客户端绑定到新的
   issuer。服务器保持登录期间，授权服务器的切换表现为刷新后仍 `401`，于是记录变为 `expired`，
   下一次 `start` 发现这一变化。

### 5.3 客户端注册

按规范的优先级，先匹配者生效：

1. **`oauth.clientId`。** 机密客户端由用户提供密钥，CLI 在 `mcp.auth.start {clientSecret}` 中
   发送一次。Mcp 把它存入记录，在配置的 `clientId` 与记录的 issuer 都一致时复用，从不返回它。
2. **CIMD。** 构建中带有客户端元数据 URL，且授权服务器声明了
   `client_id_metadata_document_supported` 时使用。客户端 ID 是 Dim 发布的文档的 HTTPS URL：
   `client_name` 为 `Dim`，`redirect_uris` 为 `["http://localhost:1456/callback"]`，
   `token_endpoint_auth_method` 为 `none`。维护者决定（#111）：Dim 目前不发布客户端元数据文档，
   先用 DCR；等有服务器要求 CIMD 时再定。在此之前 F4 不带这个 URL，跳过这一步（附录 C）。
   启用后，`callbackPort` 不是 1456 时也跳过，因为文档只列出那一个重定向 URI。
3. **DCR。** 授权服务器有 `registration_endpoint` 时使用。请求带 `client_name` `Dim`（从不取自
   配置）、重定向 URI、授权类型 `authorization_code` 与 `refresh_token`、
   `token_endpoint_auth_method` `none` 和 `application_type` `native`。服务器仍签发的密钥存入记录。
   `callbackPort` 改变后，重定向 URI 与当前不同的已存 DCR 客户端会重新注册。
4. **无路可走。** 否则为 `mcp_auth_client_required`：到服务器的门户注册客户端并设置
   `oauth.clientId`。

收到 `invalid_client` 时清除已存的注册及其令牌。登录中，尝试以 `mcp_auth_failed` 结束，因为
授权码是签给那个客户端的；刷新中，记录变为 `expired`。下一次 `mcp.auth.start` 以 DCR 或 CIMD
重新注册，或再次使用配置的 `clientId`。

### 5.4 回调校验（`mcp.auth.complete`）

依次检查。第 1 至 3 步失败时尝试保持待完成，粘贴错了可以再试：

1. **源与路径。** scheme、host、端口和路径必须与尝试的重定向 URI 完全相同，不做规范化比较。
   否则为 `mcp_auth_callback_invalid`。
2. **state。** `state` 必须等于尝试中的值，以常数时间比较。否则为 `mcp_auth_callback_invalid`。
3. **issuer。** `iss` 按规范的 RFC 9207 表处理：存在时必须以简单字符串比较等于记录的 issuer；
   缺失时只有元数据声明了 `authorization_response_iss_parameter_supported` 才拒绝。不匹配时不处理、
   也不显示响应中的任何 `error` 字段（`mcp_auth_callback_invalid`）。
4. **授权服务器错误。** 带 `error` 参数时尝试以 `mcp_auth_failed` 结束，只带 RFC 6749 错误码，
   例如 `access_denied`。既无 `error` 也无 `code` 的回调是 `mcp_auth_callback_invalid`，尝试保持
   待完成。
5. **交换。** 尝试仍是该 key 的当前尝试时（§7），Mcp 自己发送令牌请求：`code`、`code_verifier`、
   `redirect_uri`、`client_id`、`resource` 和记录中的客户端认证。它不调用 rmcp 的交换函数，因为
   该函数会记录授权码（附录 A）。成功则结束尝试，并在回复之前持久化记录。令牌错误以 `mcp_auth_failed` 结束尝试。传输错误保持
   待完成（`mcp_auth_unavailable`）；如果服务器已经兑换了授权码，重试会以 `mcp_auth_failed`
   失败，用户需要重新开始。

### 5.5 scope

- **固定。** 设置了 `oauth.scopes` 时按原样请求。
- **否则：**
  - 首次授权：质询的 `scope`，否则 PRM 的 `scopes_supported`，否则不带 `scope` 参数；
  - 重新授权：`requestedScopes`、最近一次质询的 `scope` 与每个 `403` 要求的 scope 的并集（提升
    授权）。只有记录的 issuer 仍是当前 issuer 时才计入 `requestedScopes`。
- **`offline_access`。** 授权服务器在 `scopes_supported` 中列出它时加上，以便签发刷新令牌
  （规范中的 MAY）。

### 5.6 使用与刷新令牌

```text
 连接 A（Session 1）    连接 B（Session 2）    McpAuth，key K            授权服务器
   |                      |                      |                              |
   |-- lease(K) -------------------------------->|                              |
   |                      |                      | 剩余不少于 30 秒：           |
   |                      |                      | 直接回复；否则锁存储，       |
   |                      |                      | 复查后刷新                   |
   |                      |-- lease(K) --------->|                              |
   |                      |                      | （等待锁）                   |
   |                      |                      |-- refresh, resource -------->|
   |                      |                      |<-- access, rotated refresh --|
   |                      |                      | 持久化，解锁                 |
   |<-- lease r2 --------------------------------|                              |
   |                      |<-- lease r2 ---------|                              |
   | MCP：r2 得到 401     |                      |                              |
   |-- rejected(r2) ---------------------------->|                              |
   |                      |                      | 仍为 r2：刷新；              |
   |                      |                      | 否则给出更新的租约           |
   |<-- lease r3 --------------------------------|                              |
   | 用 r3 重试一次；再次 401：记录 expired，连接 auth_required
```

- **每个请求。** 每个 HTTP 请求都向 `McpAuth` 取一个租约并发送 `Authorization: Bearer`；规范要求
  每个请求都带授权。这覆盖 streamable HTTP 的 POST、GET 和 DELETE，以及 SSE 的 GET 和 POST。
  因此刷新后的令牌无需重连即可使用。
- **过期之前。** 剩余不足 30 秒的令牌在使用前刷新，与托管 OAuth 宿主和 rmcp 的余量相同。
  只在使用时刷新，没有后台定时器。
- **遇到 `401`。** 带不带 `WWW-Authenticate` 的 `401` 都算。用令牌 `rN` 发出的请求得到 `401` 时，连接把 `rN` 报告为被拒。只有记录仍为
  `rN` 时 `McpAuth` 才刷新；否则另一个 Session 已经刷新过。请求重试一次。被拒的请求服务器没有
  处理，重试不会重复任何效果。
- **单飞。** 每个 key 同一时刻在所有 Session 中只有一次刷新：拿到锁的等待者重读记录，得到新令牌。
- **轮换。** 新的刷新令牌取代旧的；没有返回新的就保留旧的。新记录在任何租约使用新访问令牌之前
  持久化。
- **重新授权。** `invalid_grant`、`invalid_client`、没有刷新令牌的令牌过期或被拒，或刷新后仍
  `401`，记录置为 `expired`。该 key 上的每个 Session 随即报告 `auth_required`，不必等自己发出
  请求。刷新遭到其他拒绝，或响应不是 OAuth 错误时，记录保留：访问令牌在有效期内照常使用，之后
  请求失败，直到授权服务器恢复应答。
- **存储错误。** 存储无法读取或版本未知时，请求不带 bearer 发出，不需要登录的服务器照常工作；
  只由 `401` 决定。

### 5.7 提升授权（`403 insufficient_scope`）

`WWW-Authenticate` 带 `error="insufficient_scope"` 的 `403` 只让该请求失败，连接保持 `ready`。
Mcp 自己检查 `error` 参数，因为 rmcp 把任何带该请求头的 `403` 都映射为 `InsufficientScope`。

- 工具结果是 §8 的固定文本。
- Session 发现服务器时（握手或 `tools/list`）遇到 `403`，没有工具可发布，于是报告
  `auth_required`，直到凭据变化。
- `McpAuth` 记下质询的 scope，目录中显示为 `missingScopes`。
- 下一次 `mcp.auth.start` 请求 §5.5 的并集。提升授权由用户发起，不会循环。
- 固定了 `oauth.scopes` 时，按固定值原样请求。用户把该 scope 加进固定值，与 Claude Code 中
  同一字段的做法相同。

### 5.8 登出

`mcp.auth.logout` 在授权服务器声明了 `revocation_endpoint`（RFC 7009）时，先撤销刷新令牌，
再撤销访问令牌。然后无论撤销结果如何都删除记录，包括注册和任何客户端密钥。回复说明撤销是否成功。
该 key 上的每个 Session 随即报告 `auth_required`。登出一经接受就关闭该 key 待完成的尝试，并关闭
在它完成之前开始的尝试，因此同一守护进程中进行中的登录无法撤销登出（§7）。条目不再使用 OAuth 后（加了静态凭据，
或改为明文 URL）也允许登出，因此已存的凭据总能删除。

`mcp.remove` 不动凭据，因为另一来源中同 `id`、同 URL 的条目可能仍在使用；CLI 会先提示登出
（§13）。孤立记录不起作用：其令牌只发往自己的 URL，且只用于该 key 的条目。

## 6. 令牌生命周期

每个节点是一个 key 在目录中的 `auth.status`。`refreshing` 是内部状态，不显示。

```text
 absent --401--> required --start+complete--> authorized <--success-- refreshing
   ^                                                  |                  ^    |
   |                                                  +-- 临近过期 ------+    |
   |                                                      或 401              |
   |                                                                          |
   |                expired <--- invalid_grant、invalid_client、无刷新 -------+
   |                   |         令牌，或刷新后仍 401
   |                   +--start+complete--> authorized
   |
   +---- logout，从 required、authorized 或 expired
```

`absent`：Dim 对该 key 没有记录，也没有记下的质询。登出会删除记录并清除质询。此时目录没有 `auth` 字段。

## 7. 时序与冲突

**尝试的时效。** 每个 key 至多有一个当前尝试，以其 `attemptId` 标识。只有当前尝试可以兑换授权码、
存入令牌或唤醒 Session。尝试只存在于一个守护进程的内存中（§4.2），因此这条规则只为同一守护进程
内的操作排序。共用 home 的另一个守护进程在登出之后完成的登录，是一次更晚的登录。

**唯一的顺序。** 尝试锁（§4.2）是尝试状态唯一的线性化点。`start` 持有它使尝试成为当前尝试，
`logout` 在被接受时和完成时持有它关闭尝试，`complete` 持有它校验回调，交换在两次时效检查时持有
它。交换从最后一次检查起一直持有它，直到结果发布完毕。这把锁的持有者从不等待存储锁或网络请求，
因此等它至多等一次本地存储写入，从不等待令牌请求或刷新。于是每个 `start` 和 `logout` 要么排在
交换的最后一次检查之前，交换随即发现其尝试已被取代；要么排在其发布之后：之后完成的新尝试替换存入
的凭据，登出则删除并撤销它。被取代的尝试绝不会在更新的 `start` 返回之后写入存储或唤醒 Session。

- **`start`** 返回时使其尝试成为当前尝试，取代之前的尝试。进行中的发布持有尝试锁，因此新尝试在
  该发布之后才成为当前尝试。若 `start` 读取记录之后有登出完成，`start` 改为回答
  `mcp_auth_attempt_closed`，因为它的尝试可能复用登出已删除的客户端。
- **`logout`** 在被接受时、等待存储锁之前关闭当前尝试；删除记录后再关闭一次，结束期间开始的尝试。
- **`complete`** 在交换结算时结束尝试，传输错误除外（§5.4）。回调中带授权服务器错误，或过期，同样
  结束尝试。
- **交换**两次检查其尝试是否仍是当前尝试：取得存储锁之后、发送令牌请求之前；收到响应之后、写入
  存储之前。从第二次检查起它一直持有尝试锁，直到结果发布完毕：写入记录、清除质询与缺失的 scope、
  唤醒 Session；`invalid_client` 时清除注册；传输错误后尝试重新待完成。
  - 在请求之前被取代：授权码从不发送。
  - 在请求发出期间被取代：请求仍会结算（§4.1）。它取得的令牌被撤销（RFC 7009，尽力而为，先撤销
    刷新令牌）并丢弃。撤销在仍持有存储锁时进行，因此先于之后的任何令牌请求。无论响应如何都不写
    存储，也不唤醒 Session。
  - 两种情况下 `complete` 都回答 `mcp_auth_attempt_closed`。
  - 最后一次检查时仍是当前尝试：结果在之后任何 `start` 或 `logout` 生效之前发布。授权码已经用掉，
    所以记录写不进去时尝试照样结束；令牌随后在释放尝试锁之后撤销。
- **刷新**不需要这种检查。它在存储锁下重读记录，因此能看到在它之前持锁的登录或登出；在它之后持锁的
  登录或登出会替换或删除它的结果。

| 竞争 | 结果 |
| --- | --- |
| 同一 key 两次 `start` | 后返回的取代先返回的；完成先返回的得到 `mcp_auth_attempt_closed` |
| `complete` 进行中时 `start` | 旧交换的最后一次检查之前，新尝试取代旧尝试：尚未发送的授权码不再发送，已取得的令牌被撤销并丢弃，`complete` 回答 `mcp_auth_attempt_closed`。检查之后，`start` 要等旧交换写完记录、唤醒 Session 才成为当前尝试；完成新尝试会替换该记录 |
| 同一尝试两次 `complete` | 一次只有一个交换：第二次立即回答 `mcp_auth_attempt_closed`。传输错误之后尝试重新待完成 |
| 尝试待完成或交换进行中时 `logout` | `logout` 被接受时尝试关闭。仍在等待存储锁的交换不发送任何东西；请求已发出的交换丢弃其令牌；已过最后一次检查的交换先发布，`logout` 在其之后被接受。随后 `logout` 删除记录并撤销其中的令牌 |
| `logout` 进行中时 `start` | 若登出在 `start` 返回之前完成，`start` 回答 `mcp_auth_attempt_closed`；`start` 在此之前设为当前的尝试在登出完成时关闭 |
| 刷新进行中时 `complete` | 二者依次持有存储锁：`complete` 之后的刷新发现令牌新鲜而不做事；刷新之后的 `complete` 替换令牌 |
| 刷新进行中时 `logout` | `logout` 在刷新之后取得存储锁；刷新得到的令牌被撤销并删除。`logout` 之后的刷新找不到记录 |
| 两个 Session 同时得到 `401` | 只刷新一次（§5.6）；两边都用新租约重试 |
| 条目 URL 被修改 | key 改变，新的代从 `absent` 开始，第一次 `401` 时变为 `required`；旧记录不起作用 |
| 尝试期间条目被禁用或删除 | 尝试仍可完成并持久化；条目启用之前不会连接 |
| Mcp 重载 | 尝试被丢弃（之后 `complete` 回答 `mcp_auth_attempt_closed`）；进行中的调用回答 `operation_interrupted`；存储保留 |
| 服务器已轮换刷新令牌、持久化之前进程被杀 | 旧刷新令牌失效：`expired`，需重新登录。正常停止会先结算请求（§4.1），只有被杀时才会落入这个窗口 |
| 尝试中途调用方离开（Ctrl-C、断开） | 无需撤销：待完成的尝试在 `complete` 之前没有任何效果，10 分钟后过期，或被下一次 `start` 取代 |
| 时钟变化 | `expiresAt` 在收到时由 `expires_in` 算出；时钟错误只会让刷新提前或推迟，`401` 处理会兜住 |

## 8. 上下文映射

模型看不到这些。

- **未授权时没有工具。** 处于 `auth_required` 的服务器不发布工具，所以它的工具是缺席，而不是失败。
- **失败的调用。** 因授权失败的调用以下表固定文本作为 `ToolResult.content`，并设置 `isError`。
- **没有事件。** 授权不写任何 Timeline 事件（原则 §4 第 9 条）。
- **不由 agent 触发。** 登录是用户经管理方法发起的操作，从不是工具。

| 情形 | `ToolResult.content` |
| --- | --- |
| 需要授权或授权已过期 | `The MCP server "<id>" requires the user to sign in. Ask the user to sign in to it with /mcp.` |
| `403 insufficient_scope` | `The MCP server "<id>" needs additional permissions for this tool (<scopes>). Ask the user to sign in to it again with /mcp.` |

`<id>` 是配置的服务器 id，`<scopes>` 是质询中的 scope 名；质询没有给出 scope 时省略括号。二者都不是密钥，文本中没有 URL、
令牌、授权码或 issuer。

## 9. 接口

### 9.1 管理方法（命名空间 `mcp`，仅本地传输）

三个方法都与其余 `mcp.*` 一样是 `report`，使
[迁移表](../contracts/session-boundary.md#a4-迁移与兼容性)保持成立。收到
`operation_interrupted` 后读 `mcp.catalog` 查看结果。三个都是修改型方法。目标沿用现有的
`{source, id, cwd?}` 形式。`source` 是 `"user"`、`"project"` 或 `"plugin"`；插件条目的 `id` 是 `<plugin id>:<server>`，与 `mcp.catalog` 列出的一致（[PluginManager §3.7](../plugins/PluginManager.md#37-内容登记)）。

| 方法 | 参数 | 回复 | 说明 |
| --- | --- | --- | --- |
| `mcp.auth.start` | `{source: "user" \| "project" \| "plugin", id, cwd?, clientSecret?}` | `{attemptId, authorizationUrl, redirectUri, resource, issuer, expiresAt}` | 条目必须是已启用、无静态凭据的远程条目。只有配置了 `oauth.clientId` 才接受 `clientSecret`。`expiresAt` 为 Unix 毫秒，10 分钟后。取代同一 key 的待完成尝试 |
| `mcp.auth.complete` | `{attemptId, callbackUrl}` | `{}` | §5.4。记录持久化后才返回 |
| `mcp.auth.logout` | `{source, id, cwd?}` | `{revoked: boolean}` | 对禁用条目，以及不再使用 OAuth 的远程条目也可用。没有记录：`{revoked: false}` |

不需要每次调用的进度（C1，PR [#83](https://github.com/arcships/dim/pull/83)）：`start` 直接
返回 URL，也没有后台过程会完成尝试。

### 9.2 现有回复的补充

- **`mcp.catalog`。** 每个条目增加 `auth?: {status: "required" | "authorized" | "expired",
  missingScopes?: string[]}`。stdio 条目、静态凭据条目和 `absent` 状态没有该字段。URL、issuer、
  客户端和令牌都不出现，与目录已经排除 URL 和凭据一致。
- **`session.mcp`。** 服务器 `state` 词汇由 Mcp 拥有、由协议投影照抄，增加 `auth_required`，
  `message` 为 `authorization required` 或 `authorization expired`。与 `failed` 不同，key 的凭据
  变化时它会自行消除。

### 9.3 错误（`appError`，code `-32000`）

| `appError` | 何时 | 客户端动作 |
| --- | --- | --- |
| `mcp_auth_unsupported` | stdio 条目；配置了 `auth` 或 `Authorization` 请求头；非回环的纯 `http` 服务器；存储格式版本未知 | 停止：修正条目，或使用写出该存储的 Dim 版本 |
| `mcp_auth_discovery_failed` | 没有 PRM 也没有 `authServerMetadataUrl`；`resource` 或 `issuer` 不符；没有 `S256`；非 HTTPS 端点 | 修正服务器，或设置 `authServerMetadataUrl` |
| `mcp_auth_client_required` | 没有客户端 ID、没有可用的 CIMD、没有 `registration_endpoint`；或配置的 `clientId` 记录在另一个 issuer 下 | 设置 `oauth.clientId`；issuer 改变后先登出 |
| `mcp_auth_attempt_closed` | 尝试未知、已过期、已完成、已被取代或已被登出关闭；`start` 被登出抢先 | 重新开始 |
| `mcp_auth_callback_invalid` | 重定向 URI、`state` 或 `iss` 不符；没有 `code` | 粘贴正确的 URL；尝试保持待完成 |
| `mcp_auth_failed` | 授权服务器拒绝：回调中的 `error`、令牌错误、注册被拒 | 修正原因后重新开始 |
| `mcp_auth_unavailable` | 授权服务器网络失败、超时或 5xx；存储 I/O | 重试 |

现有错误码不变：`mcp_unknown_server`、`mcp_invalid_config`，以及 `mcp_unavailable`（禁用的条目
也用它，与现在相同）。消息中从不带密钥、回调 URL，也不带未经校验的响应中的授权服务器
`error_description` 文本。

## 10. 决定与被否决的方案

| # | 决定 | 被否决的方案及原因 |
| --- | --- | --- |
| D1 | OAuth 引擎：rmcp 3.5.0 `auth`（`AuthorizationManager`）负责发现、动态注册和授权请求（PKCE `S256`、`state`、`resource`），由 `McpAuth` 通过自己的 HTTP 客户端驱动，并用一个 `StateStore` 把 verifier 和 `state` 交给尝试。所有令牌请求（授权码交换、刷新、撤销）由 Mcp 自己发送，并补上附录 A 的缺口 | Dim 自写 OAuth 客户端要重新跟进官方 SDK 已在跟进的每次规范修订。rmcp 高层的 `OAuthState` 与 `AuthClient` 每个 HTTP 客户端持有一个 manager，默认用内存存储，会静默退回猜测的端点，也不能在 Session 之间共享一次刷新。通过 `CredentialStore` 使用 rmcp 的 `refresh_token`：重启后它发送基础 URL 而不是记录的 `resource`，`invalid_client` 只以文本报告，还要为一个 Mcp 在授权码交换时本就自己发送的请求在记录外包一层适配器 |
| D2 | 所有部分都在 Mcp 内：一个进程级权威，加上现有的 Session 发现、连接实例和管理方法 | 单独的 OAuth 插件只把 Mcp 接到它自己的令牌上，是接线模块（ADR 002 D2）；托管 OAuth 宿主服务固定的 Provider，不认识 MCP 发现。每个 Session 一份凭据意味着每个 Session 登录一次 |
| D3 | 发现严格按规范；`authServerMetadataUrl` 只在没有有效 PRM 时生效；拒绝缺少 PKCE 元数据和非 HTTPS 端点 | rmcp 的基础 URL 发现以及合成的 `/authorize`、`/token`、`/register`：规范要求用 PRM 发现，否则以中止或使用预先配置的值结束，猜测的端点可能把用户带到意外的页面。覆盖值优先于有效 PRM（Claude Code 的语义）会让项目文件把用户引到另一个授权服务器，即便服务器已经指明了自己的授权服务器 |
| D4 | 注册：`clientId`，然后 CIMD（产品 URL，发布后），然后 DCR，然后 `mcp_auth_client_required`。机密客户端的密钥在登录时输入并存入记录 | 配置中的密钥（`clientSecretRef`）：`.mcp.json` 与项目共享，项目条目可以把密钥指向自己的令牌端点；而且要等 S8b |
| D5 | 重定向 URI `http://localhost:{callbackPort}/callback`，默认端口 1456（1455 是账号登录的） | `127.0.0.1` 会破坏为 Claude Code 的 `localhost` 形式所做的注册（它的 v2.1.229 回归）；每次尝试随机端口会破坏稳定的 DCR 注册和固定的 CIMD 文档 |
| D6 | 回调总是经 `mcp.auth.complete {attemptId, callbackUrl}` 送回；CLI 用回环监听捕获，或由用户粘贴 | 守护进程中的回环监听是业务所有者内部的新外部入口，而且经 SSH 时本来就够不着；一次带进度的长调用会让 RPC 挂上几分钟，仍然需要监听；设备流不在 MCP 规范中 |
| D7 | 三个方法，全部 `report`；没有取消方法，也不轮询尝试状态 | `drain` 会让 Mcp 的卸载等待授权服务器调用，并打破 `mcp.*` 统一的规则。取消方法无事可撤，因为待完成的尝试没有效果且会过期；轮询无物可轮，因为没有后台方会完成尝试 |
| D8 | 凭据 key = 服务器 `id` + 规范化 URL，进程级；记录绑定 `resource` 与 `issuer` | 只用 URL 作 key 时，一个服务器只能有一个账号；`(source, id)` 会让令牌跟着名字去到新 URL；按 Session 作 key 与进程所有权相悖（ADR 002 D1） |
| D9 | 存储为 home 根目录下的 `mcp-oauth.json`，经 FileStorage 原子替换为 0600，带版本；令牌请求持有存储锁，发出即结算。S8b 不提供凭据服务，因此没有东西需要迁移：令牌是所有者状态，每次刷新都会重写，每个所有者自己处理密钥（[settings-service §1.2](settings-service.md#12-决定密钥归其所有者)）；其他 OAuth 持有方照此办理 | SQLite 中的 Settings 行存放业务文档，不放密钥；`pluginFiles` 是 FileStorage 给非 Rust 插件的桥接 API，Rust 所有者直接用 FileStorage，托管 OAuth 宿主就是这样；OS 钥匙串需要一个覆盖所有 OAuth 所有者的宿主级密钥存储决定；等待 S8b 会卡在一个无法保存轮换密钥的 API 上 |
| D10 | 使用时刷新，余量 30 秒；`401` 时刷新并重试一次；每个 key 单飞；轮换在使用前持久化 | 后台刷新定时器会为空闲服务器唤醒守护进程，并轮换没人用的令牌 |
| D11 | 提升授权由用户发起：调用以固定文本失败，下一次登录请求并集 | 从工具调用自动重新授权，需要在一轮中途打开浏览器 |
| D12 | 登出先撤销（RFC 7009，尽力而为），再删除整条记录；`mcp.remove` 不动凭据 | 在 `mcp.remove` 时删除，无法知道另一来源中同 key 的条目是否仍在使用 |
| D13 | `session.mcp` 增加会自行消除的 `auth_required`；目录增加 `auth` | 复用 `failed` 会要求登录后手动刷新，CLI 也无从据以行动 |
| D14 | `start` 要求条目已启用；登出总是允许；一个 key 在各来源之间共享 | 在禁用的项目条目上登录，会在用户启用之前访问项目文件中的 URL（ADR 003 D3） |
| D15 | 带静态凭据的条目从不使用 OAuth；OAuth 需要 `https` 或回环 `http` | 静态凭据失败后退回 OAuth 会混用两种凭据来源；明文传输的 bearer 令牌可能被窃取 |
| D16 | 保留 rmcp 对 PRM 的接受规则：`resource_metadata` URL 必须同源，`resource` 可以是服务器 URL 或同源下的父路径 | RFC 9728 不要求元数据 URL 同源；接受其他源会让任何 `401` 把发现引到别的主机。RFC 9728 要求 `resource` 完全相同；坚持这一点就得替换 rmcp 的校验，而父路径规则让令牌受众仍限于服务器的源 |
| D17 | 尝试的时效（§7）：`attemptId` 标识尝试，只有该 key 的当前尝试兑换授权码，在取得存储锁之后和收到令牌响应之后各检查一次。一把尝试锁为尝试状态的每次变化排序；交换从第二次检查起一直持有它，直到结果发布完毕，因此之后的 `start` 或登出在发布之后生效。登出在被接受时和完成时关闭尝试；在一次已完成的登出之前准备的 `start` 被拒绝。已发出的被取代请求照样结算，其令牌在存储锁下尽力撤销后丢弃。持有存储锁使撤销排在更新尝试的交换之前：撤销整个会话的服务器会在登录时拒绝更新的授权码，而不是留下一份存入时就已失效的凭据 | 只在请求之前检查，会让仍在途中的响应覆盖更新的尝试。在写存储之前结束尝试、写完再发布，会让更新的 `start` 在写入期间返回，于是被取代的尝试在新尝试成为当前尝试之后仍存入令牌并唤醒 Session。先写入、被取代时再回滚，会让不加锁的读者在两次写入之间用上这些令牌，回滚失败还会把它们留在存储中。取消途中的请求违背“发出即结算”（§4.1）：已兑换的授权码会留下无人知晓的有效令牌。先存入、留待之后的登出清理，会撤销用户已经完成的登出。不撤销就丢弃，令牌在授权服务器上一直有效到过期。在 `start` 中持有存储锁，会让每次登录排在最长 30 秒的刷新之后。每个尝试另设代计数，与 `attemptId` 重复 |

## 11. 安全

- **PKCE 与 `state`。** PKCE 始终为 `S256`，不声明它的服务器被拒绝。`state` 随机、一次性，以常数
  时间比较。verifier 与 `state` 只在内存中存在，只属于一次尝试。
- **重定向 URI。** 重定向 URI 总是 `http://localhost:{port}/callback`，这是规范允许的形式。回调
  必须与它完全一致。恶意的重定向目标得不到任何东西：没有 verifier，授权码毫无用处，而 verifier
  从不离开守护进程。
- **混淆攻击。** 通过 RFC 9207 的 `iss` 与记录的 issuer 检查。残余风险：既不发送也不声明 `iss`
  的授权服务器仍可能遭受混淆攻击，所有符合规范的客户端都是如此。规范表示预计会把 `iss` 改为必需，
  届时这一风险消除。
- **TLS。** 授权服务器端点，以及附带令牌时的 MCP 服务器，都要求 TLS；回环 `http` 是唯一例外，
  用于本地开发，而且只有 MCP 服务器也在回环上时，授权服务器端点才可以使用它。所有发现和注册
  请求都经过 Mcp 的 HTTP 客户端，它拒绝其他明文 URL，也从不跟随重定向。对远程 MCP 服务器，任何
  OAuth URL 都不得指向回环、私有、链路本地或云元数据主机，HTTPS 也不行（这是 rmcp 对授权服务器
  元数据 URL 的规则，推广到所有端点）。令牌只发往配置 URL 的源；旧版 SSE 传输已经拒绝跨源端点。
- **令牌受众。** 授权、令牌和刷新请求都带 `resource`。令牌从不发往其他服务器，`start` 也从不复用
  记录在另一个 issuer 下的客户端或令牌（§5.2）。
- **存储。** 存储权限为 0600，经 FileStorage 原子替换。密钥不进入 Timeline、`ToolResult.content`、
  目录、`session.mcp`、错误消息或诊断。Mcp 为每次认证操作发出一个 `tracing` 事件，target 为
  `mcp.auth`，带服务器 id、issuer 主机、结果码和耗时。
- **rmcp 的 debug 日志。** rmcp 3.5.0 在 `exchange_code_for_token_with_issuer` 中以 `debug` 级别
  记录授权码，所以 Mcp 自己发送授权码交换（§5.4）。rmcp 的其他 `auth` 日志只通过 `oauth2` 会打码的
  密钥类型输出令牌。不依赖宿主的日志过滤器。
- **钓鱼。** 打开浏览器之前，CLI 显示服务器的 `resource` 和授权服务器的 `issuer`。DCR 的
  `client_name` 固定为 `Dim`，配置无法设置。
- **项目条目。**
  - 只有用户启用后才运行（ADR 003 D3），登录也要求如此。
  - 它们的 `oauth` 字段不带密钥，所以项目文件无法把用户的密钥送到任何地方。
  - 与用户条目共享 key 时，令牌只发往同一个 URL；启用项目条目即视为信任它（ADR 003 D4）。
- **授权不是批准。** 登录是用户在授权服务器上的同意。Dim 不保存任何授予，Approval 与 Permission
  仍然准入每次 MCP 工具调用（ADR 003 D1）。

## 12. 规则检查

| 规则 | 本设计如何遵守 |
| --- | --- |
| 原则 §4 第 1 条（改别人的状态写指令） | Mcp 只改自己的状态：它的存储、尝试和连接状态。 |
| 原则 §4 第 2 条（交接） | 不适用：没有对象在模块之间交接。 |
| 原则 §4 第 3 条（只用 before 回调） | 没有 before 回调。入口只检查格式和授权；OAuth 检查归 Mcp。 |
| 原则 §4 第 4 条（核心不带业务含义） | 不改核心；Timeline、Dispatcher 和 Rpc 分发都不动。 |
| 原则 §4 第 5 条（批次） | 不适用：不写事件。 |
| 原则 §4 第 6 条（上下文来自已交接事件） | 只有 ToolExecutor 写的工具结果进入模型，授权失败时为固定文本（§8）。 |
| 原则 §4 第 7 条（只用公开事件和服务） | Mcp 使用 FileStorage、Settings 和 rmcp；新服务只由 Mcp 自己的部分使用。 |
| 原则 §4 第 8 条（基础设施不变） | 不改 Timeline、Dispatcher、事件格式、装配、rutis 或门面。不向外部依赖 rutis 提需求。 |
| 原则 §4 第 9 条（只记后续决定要用的事实） | 没有 Timeline 事件；凭据是进程状态，经 Mcp 的类型化管理方法修改。 |
| 原则 §1（Capability 与 App Server 边界） | 是对现有 Mcp 进程插件的补充，与附录 B.2 的记录一致；不新增 App Server 业务模块。 |
| ADR 002 D1（作用域跟随状态） | 凭据跨 Session 共享，因此是进程状态；连接的使用留在 Session 所有的实例中。 |
| ADR 002 D2（不设接线模块） | 不新增插件、端口或适配器。 |
| ADR 002 D3（依赖指向核心） | Mcp 新增对 FileStorage 的依赖，核心到核心；没有新依赖指向可选插件。 |
| ADR 002 D4（结构配置与业务配置分离） | `oauth` 是 `mcp` 来源中的业务配置，使用时读取；令牌存储是所有者自己的存储。编辑不重载任何东西。 |
| ADR 002 D5（合同语言中立） | 方法、回复和错误都是 JSON 数据；取消就是调用方离开，待完成的尝试会过期。 |

## 13. 向机制所有者提出的需求

无。F4 不需要运行时提供任何东西，也不向 rutis 提需求。

对 CLI 负责人（zerob13）的需求，以行为描述；CLI 的内部实现由其自定：

1. **登录。** `dimcode mcp` 的登录命令和 `/mcp` 的操作，对 `mcp list` 显示的条目调用
   `mcp.auth.start`，与 `mcp enable ID` 相同。
   - 打开浏览器之前，显示 `resource` 和 `issuer` 中的授权服务器主机。
   - 尽力打开浏览器。
   - 提供以隐藏输入录入客户端密钥的方式。CLI 从不回显、记录或保存它。
2. **送回回调。** `redirectUri` 是回环地址且其端口在 `127.0.0.1` 和 `::1` 上都空闲时，用一个只服务
   一个匹配回调的短时本地监听捕获回调，用 `mcp.auth.complete` 转发完整 URL，再给浏览器回一个静态页面。
   - 否则（例如端口被占用、处于 SSH 下，或用户要求不打开浏览器），像账号登录一样以隐藏输入请用户
     粘贴完整的回调 URL。
   - 收到 `mcp_auth_callback_invalid` 时再次询问。
3. **停止等待。** 按 Ctrl-C 或到达 `expiresAt` 时停止，并提示如何重新开始；尝试无需清理。
4. **登出。** `dimcode mcp` 的登出命令和 `/mcp` 的操作调用 `mcp.auth.logout`，并报告 `revoked`。
   删除 `auth.status` 为 `authorized` 的条目时，先提示登出。
5. **状态。** `mcp list` 和 `/mcp` 把目录的 `auth.status` 显示为需要登录、已登录或登录已过期，
   并显示 `missingScopes`。Session 视图显示 `auth_required` 并给出登录提示。
6. **非交互。** 显式登录命令要求终端，否则打印应运行的命令。`--prompt`、exec 和管道的提示内容
   保持为字面模型输入，包括斜杠文本；CLI 不把这些内容解释为登录操作。
7. **错误。** 把 §9.3 的错误码映射到所列动作，绝不把回调 URL 回显出来。

原生 CLI 提供 `mcp login ID [--no-browser] [--client-secret]`、`mcp logout ID`，以及空闲时的
`/mcp`、`/mcp login [--no-browser] [--client-secret] ID`、`/mcp logout ID`。目标来源取自公开目录，
保留项目条目遮蔽用户条目的语义。`--client-secret` 只开启隐藏输入，不接受参数值。回环监听同时绑定
`127.0.0.1` 和 `::1`。只有配置的回调路径会被接收；其他路径回 404，不消耗监听器。每个连接有
两秒请求头期限和大小上限，无效连接之后继续监听。首个匹配回调会释放监听器，将其原始路径和查询
与已知来源组成完整 URL，等待 `complete` 结算后回复静态页面。浏览器等待期间也可隐藏粘贴 URL，
先到者生效。重定向、state、issuer 和授权结果仍只由守护进程校验。取消退出 130，过期退出 1 并明确
提示；已提交操作若成功完成则退出 0。未知服务器错误不回显未确认的命令文本。

Ctrl-C 或到期会释放监听和隐藏输入状态。已经提交的 RPC 在返回交互提示前读完回复，保留现有响应身份
约束，不增加尝试取消方法、令牌状态、后台轮询或守护进程入口。当前 CLI 配置操作是启用和禁用条目；
通过配置 API 删除条目时仍需遵守上述先提示登出的要求。
[CLI README](../../../../crates/dim-cli/README.md#mcp-servers) 提供交互示意。

其他后续事项，不是需求：

- **CIMD。** 维护者决定（#111）：项目目前不发布客户端元数据文档，没有预注册的服务器由 DCR 覆盖；
  等有服务器要求 CIMD 时再定（附录 C）。

## 附录 A. rmcp 3.5.0 `auth`：使用的部分与补上的缺口

启用 `auth` 会加入不带默认 feature 的 `oauth2` 5、`async-trait` 和 `url`。它复用
`transport-streamable-http-client-reqwest` 已在使用的 reqwest 0.13，不会多出第二套 HTTP 栈。
改动后重新运行 `xtask check` 的依赖规则。

| 从 rmcp 使用 | rmcp 3.5.0 的缺口 | Mcp 如何补上 |
| --- | --- | --- |
| PRM 发现（请求头、well-known、同源、`resource` 检查）、授权服务器发现顺序与 `issuer` 检查、对 PRM 指出的授权服务器 URL 做的纯字符串 SSRF 过滤 | 会退回服务器源和合成的端点，只在 `AuthorizationMetadataResolution.source` 中报告 | 只使用来自 PRM 的元数据或配置的 `authServerMetadataUrl`；拒绝退回（§5.2） |
| 同上 | 规范顺序之外的补充：路径形式与根路径形式之间多一个 `/{path}/.well-known/oauth-protected-resource` 候选；先读单数字段 `authorization_server` 再读 `authorization_servers`；对带路径的 issuer，在三个候选之后多一个根路径 `oauth-authorization-server` 候选。`issuer` 不符时中止发现，而不是尝试下一个授权服务器 | 接受：多出的 well-known 候选仍在被发现的源上，并且仍检查 `issuer`；单数字段是该列表的旧名；中止是失败时关闭 |
| 无 | 缺少 `code_challenge_methods_supported` 时只发警告 | 拒绝（规范 MUST） |
| 无 | 不检查授权服务器元数据中各端点的 scheme | 要求 HTTPS，回环除外 |
| `configure_client` 与 `register_client`（DCR）；不使用 CIMD（§5.3） | 不持久化 `client_secret` | 记录保存它，每次操作重新应用 |
| `get_authorization_url`（PKCE `S256`、`state`、`resource`；verifier、`state` 和预期 issuer 存于 Mcp 的内存 `StateStore`） | 它自己的选择取之前请求的、质询的和 PRM 的 scope 的并集，并集为空时退回授权服务器的 `scopes_supported` | 传入按 §5.5 选出的 scope |
| 无 | `AuthorizationCallback::from_redirect_url` 忽略 `error`，也不比较重定向 URI；`exchange_code_for_token_with_issuer` 以 `debug` 级别记录授权码 | Mcp 自己解析并校验回调（含 `iss`），并自己发送授权码交换（§5.4） |
| 无（不使用 `refresh_token`，D1） | `refresh_token` 从重启后的守护进程所没有的发现状态中取 `resource`，加锁后不复查，`invalid_client` 只以文本报告 | Mcp 自己在存储锁下发送刷新请求，带复查和轮换（§5.6） |
| `StreamableHttpClientTransport` | 其 worker 等待第一条消息时不响应取消，因此关闭一个从未发送消息的传输不会结束 | 先轮询一次握手，再响应取消 |
| `StreamableHttpError::AuthRequired` 与 `InsufficientScope` | 任何带 `WWW-Authenticate` 的 `403` 都是 `InsufficientScope` | Mcp 读取 `error="insufficient_scope"` |
| 无 | 没有 RFC 7009 撤销 | `mcp.auth.logout` 中每个令牌一个请求 |
| 无 | 没有 SSE 客户端传输（rmcp 0.11 已移除） | Dim 自己的旧版 SSE 传输附加租约并报告 `401` |

## 附录 B. 验收（流程测试）

`crates/dim-app-server/tests/mcp_oauth_flow.rs` 在使用隔离 home 的守护进程上，经 `mcp.auth.*`、
`mcp.catalog` 和 `session.mcp` 覆盖第 1 至 10 项和第 13 项，流式 HTTP 与旧版 SSE 都有。第 11 项
通过发现阶段的 `403` 覆盖；工具调用的固定文本没有流程测试，因为测试模型不发起工具调用。第 12 项
（issuer 变化）尚无流程测试。

在 `dim-app-server` 的流程测试中，以回环上的模拟 MCP 服务器和模拟授权服务器为对象，使用隔离的 home：

1. **未授权。** 未授权的连接使 `session.mcp` 为 `auth_required`、目录 `auth.status` 为 `required`，
   且不发布工具。
2. **登录。** 先 `start` 再 `complete`：经 `resource_metadata` 发现并以 DCR 注册。授权 URL 带 `S256`、
   `state` 和 `resource`，令牌请求带 `code_verifier` 和 `resource`。等待中的 Session 无需刷新调用
   即重连并发布工具，每个请求都带 bearer。
3. **刷新。** 临近过期的访问令牌在使用前刷新。轮换得到的刷新令牌被持久化，旧的不再发送。Mcp 卸载时
   进行中的刷新仍会被持久化。
4. **共享刷新。** 两个 Session 同时得到 `401`：授权服务器只看到一次刷新，两边的请求重试后都成功。
5. **重新授权。** 刷新后仍 `401`，或 `invalid_grant`，得到 `expired` 与 `auth_required`。重新登录
   后两个 Session 都恢复。
6. **登出。** `logout` 发送撤销，删除记录，回复 `revoked: true`。下一个请求以 `auth_required` 结束。
7. **拒绝。** 以下情形都不发送令牌请求：
   - `state`、重定向路径或 `iss` 错误（尝试保持待完成）；
   - 元数据没有 `code_challenge_methods_supported`；
   - 非回环主机上的纯 `http`；
   - 服务器没有 PRM，也没有配置覆盖值。
8. **密钥。** 存储权限为 0600。在 `RUST_LOG=debug` 下，令牌、授权码、verifier 和客户端密钥都不出现在
   Timeline、`ToolResult.content`、回复、错误或捕获的 `tracing` 输出中。
9. **项目条目。** 在禁用的项目条目上 `start` 被拒绝。用户启用后，它使用同一 key 的记录。
10. **重启。** 守护进程重启后令牌保留、尝试丢弃，`complete` 回答 `mcp_auth_attempt_closed`。
11. **提升授权。** `403 insufficient_scope` 只让该调用失败，返回固定文本。目录显示 `missingScopes`，
    下一次 `start` 请求并集。
12. **issuer 改变。** PRM 指向新的授权服务器时，`start` 重新以 DCR 注册客户端，并在登出之前以
    `mcp_auth_client_required` 拒绝配置的 `clientId`。
13. **被取代的尝试。** 每种竞争都在模拟授权服务器上保持打开；存储写入无法从外部暂停，因此经
    `testing` 特性的钩子在写入处保持打开：
    - 刷新持有存储锁时，登出关闭待完成的尝试，其回调被拒。授权码从不发送，不存储任何东西，
      Session 需要登录。
    - 新的 `start` 取代一个交换正在等锁的尝试，其授权码从不发送。等待期间，该尝试的第二次回调被拒。
    - 新的 `start` 取代一个令牌请求已发出的尝试，令牌被撤销，从不存储或使用。
    - 已过最后一次检查的交换正在写入记录时完成发现的新 `start`，只在该发布之后返回。随后完成新
      尝试会替换该记录。
    - 在 `start` 进行中完成的登出使该 `start` 被拒，下一次 `start` 注册新的客户端。

## 附录 C. 推迟事项

| 事项 | 为何不阻塞 F4 | 负责人 |
| --- | --- | --- |
| 发布 Dim 的客户端元数据文档（CIMD） | 维护者决定（#111）：目前不发布，没有它的服务器由 DCR 和 `clientId` 覆盖。等有服务器要求 CIMD 时再定；届时开启需要一个稳定的项目 HTTPS URL | 维护者（发布） |
