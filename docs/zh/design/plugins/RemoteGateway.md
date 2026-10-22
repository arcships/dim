# RemoteGateway

状态：草案。总体设计：[overview §4 插件目录](../overview.md)，[Transport、Connection 变更](../overview.md)，[多节点](../overview.md)，[通用约束](../overview.md)，[ADR 002](../../adr/002-plugin-scope.md)。阶段：P3（认证、断连与 TLS 的取舍与 [#5](https://github.com/arcships/dim/issues/5) 一起决定）。语言：Rust。

> **P3：按 rutis 现有能力重新设计。** rutis 是外部依赖，本文向 rutis 提出的需求已撤回（[原则 §4 第 8 条](../../../architecture/rust-agent/principles.md)）。

RemoteGateway 在 Transport 的 WebSocket 上为手机和 Web 工作区开设一个经认证的入口：通过二维码或配对码配对，每设备令牌及其轮换与吊销，以及 Web 工作区的静态资源。已配对设备使用现有的 App Server 协议；该入口只提供[方法](../contracts/session-boundary.md)契约中的 Session 方法（Rpc 的 Session 内置方法和每个 `sessionMethods` 服务），因此设备可以调用每个 Session 方法，包括 `session.permission.update` 和 `shell.*`（决策 E3），但从不能调用管理方法：`managementMethods` 与 Rpc 的管理类内置方法从不提供给已配对的远程设备。该入口是产品 UI 面，从不承载节点之间的 rutis 链路面。

## 1. 结构

```mermaid
flowchart LR
  subgraph P["App Server 进程，控制节点"]
    RG["RemoteGatewayPlugin<br/>配对、设备、令牌、资源"]:::new
    EH["entryHost<br/>Transport 契约"]:::contract
    TR["Transport"]
    CONN["Connection<br/>ConnectionEntry 记录"]
    RPC["Rpc<br/>内置方法、sessionMethods 服务、<br/>managementMethods 方法表"]
    SET["Settings<br/>remote 业务来源"]
    LDR["rutis Loader<br/>enabled 行字段（x-volatile），<br/>只由 PluginManager 写入"]
    PST["pluginFiles（扩展）<br/>remote-devices.json"]:::contract
  end
  DEV["手机或 Web 工作区"]
  DEV -->|"HTTPS 与 WebSocket"| EH
  RG -->|"打开入口，带调用内的<br/>authenticate 与 handle"| EH
  TR -->|"提供"| EH
  EH --> CONN
  CONN --> RPC
  RPC -->|"将 remote.* 作为 managementMethods 收集"| RG
  RG --> SET
  LDR -->|"VolatileUpdate"| RG
  RG --> PST
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
  classDef contract stroke-dasharray:4 3
```

RemoteGateway 只认证升级请求，并在连接上记录设备；此后的每次调用都是由其所有者提供的普通 Session 方法。它自己的 `remote.*` 方法是一个 `managementMethods` 进程服务（key `remote`），由 Rpc 按名称查找；与每个管理方法一样，可从本地入口访问，从不能从已配对设备访问。Rpc 按调用查找该服务，不依赖它，因此 RemoteGateway 重载不会重载 Rpc（[方法 §3.2](../contracts/session-boundary.md#32-管理方法)）。

| 部分 | 作用域 | 依赖 | 提供 |
| --- | --- | --- | --- |
| `RemoteGatewayPlugin` | 进程，控制节点 | `entryHost`（Transport）、`pluginFiles`（扩展）、Settings 业务来源 `remote`、它自己的 loader 行（`VolatileUpdate`） | `managementMethods` key `remote`（`remote.*`），由 Rpc 收集；回答其打开的入口在调用内发来的 `authenticate` 与 `handle` 请求 |
| `TransportPlugin`（变更） | 进程 | 现有；不依赖 RemoteGateway 的任何内容 | 现有 `ListenAddresses`；新增 `entryHost` |
| `ConnectionPlugin` / `RpcPlugin`（变更） | 进程 | 现有 | `ConnectionEntry {name, deviceId}`；Rpc 从不向带 `deviceId` 的连接提供管理方法 |

此处定义的契约：设备记录与令牌、配对与轮换 HTTP 端点、`remote.*` 方法、`remote-devices.json` 格式。依赖的契约：Transport 的 `entryHost` 服务及其调用内的 `authenticate` 与 `handle` 请求（拟议的 Transport 变更，[语言中立](../../adr/002-plugin-scope.md)形式见 A.1，Rust 绑定见附录 G；依赖是单向的，RemoteGateway 依赖 Transport）、Connection 的入口记录、[方法](../contracts/session-boundary.md)契约（收集进 Rpc 方法表的 `managementMethods`，从不提供给已配对设备）、`pluginFiles`（扩展）（[App Server README](../../../../crates/dim-app-server/README.md#trusted-plugin-storage)）、它自己 loader 行上的结构性 `enabled` 字段（`x-volatile`），只由 PluginManager 写入（[PluginManager](PluginManager.md)，[#3](https://github.com/arcships/dim/issues/3) 的 loader 类型化操作），以 rutis-loader 0.10 的 `VolatileUpdate` 送达 RemoteGateway，以及对业务来源 `remote` 的 Settings 类型化操作，用于监听设置，其 schema 由 RemoteGateway 声明（ADR 002 D4）。RemoteGateway 不了解任何 Session 方法的含义，也不了解任何其他插件。

## 2. 数据与状态

```mermaid
flowchart TB
  subgraph LEL["Loader 可编辑层，只由 PluginManager 写入（#3）"]
    CFG["结构性：enabled、x-volatile"]
  end
  subgraph BS["Settings 业务来源 remote"]
    RS["运行时设置：bind、tls、plaintext<br/>revision、变更通知"]
  end
  subgraph PS["pluginFiles（扩展），RemoteGateway 根目录"]
    DEVS["remote-devices.json v1<br/>device_id、name、token_hash、<br/>previous_hash、rotation_required、时间戳"]
  end
  subgraph MEM["内存"]
    OFF["配对邀约<br/>code_hash、expires_at、attempts"]:::new
    LIVE["每设备的活动连接"]:::new
  end
  TL["Timeline<br/>仅含 Session 所有者写入的事实"]
  CFG --> RG["RemoteGateway"]:::new
  RS -->|"入口打开时读取；变更时重新绑定"| RG
  DEVS --> RG
  RG --> OFF
  RG --> LIVE
  RG -.-|"从不写入"| TL
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
```

`enabled` 是结构性插件配置，只由 PluginManager 写入（RemoteGateway 从不写它）；`bind`、`tls` 和 `plaintext` 是业务配置（[ADR 002 D4](../../adr/002-plugin-scope.md)），在入口打开时读取；变更是带通知的数据更新，RemoteGateway 据此重新绑定监听器（关闭并重新打开入口），从不触发插件重载（A16）。设备记录是位于 `pluginFiles`（扩展）中的宿主本地安全材料（原子替换、锁、格式版本、仅所有者可访问的文件）；文件损坏或不可读时失败即拒绝。邀约和活动连接位于内存中，重载或重启时丢失。Timeline 只保存 Session 所有者为远程调用写入的事实，不含设备归属（A2）。

## 3. 流程

### 3.1 配对与连接

```mermaid
sequenceDiagram
  participant U as Terminal CLI
  participant RG as RemoteGateway
  participant PS as pluginFiles
  participant D as Phone
  participant T as Transport
  participant R as Rpc
  U->>RG: remote.pair
  RG-->>U: code、url、expiresAt
  D->>RG: POST /api/pair code、deviceName
  RG->>PS: 原子替换，写入新设备记录
  RG-->>D: deviceId、令牌或 HttpOnly cookie
  D->>T: GET /ws 携带令牌
  T->>RG: 认证升级请求
  RG-->>T: allow、device_id
  T->>R: 连接，ConnectionEntry 带 deviceId
  D->>R: hello v3、session.list、session.attach
```

邀约为一次性使用，在写入记录之前即被消耗；令牌只以哈希形式存储。

### 3.2 远程控制 Session

```mermaid
sequenceDiagram
  participant D as Phone
  participant T as Terminal
  participant R as Rpc
  participant O as Session owners
  participant TL as Timeline
  D->>R: session.permission.update
  R->>O: Permission 所有者校验
  O->>TL: 追加 permission/changed
  T->>R: approval.respond
  D->>R: approval.respond 同一请求
  R->>O: Approval
  O->>TL: 追加第一个 approval/respond
  O-->>D: invalid_params 已解决
  TL-->>D: 经附着分发的事件
  TL-->>T: 经附着分发的事件
```

远程和本地客户端是对等的 Session 客户端；所有者在各自边界校验，由 Timeline 序号在它们之间裁决。两个附着方以每分支顺序看到相同的事件。

### 3.3 吊销与轮换

```mermaid
sequenceDiagram
  participant U as Terminal CLI
  participant RG as RemoteGateway
  participant PS as pluginFiles
  participant C as Device connections
  U->>RG: remote.revoke deviceId
  RG->>PS: 删除记录
  RG->>C: entryHost closeDevice，连接关闭
  RG-->>U: 完成
  U->>RG: remote.rotate_token
  RG->>PS: 设置 rotation_required
  RG->>C: 关闭受影响的连接
  C->>RG: POST /api/token/rotate 携带当前令牌
  RG->>PS: 新哈希，旧哈希在宽限窗口内保留
```

吊销在连接关闭之前持久化，因此与之竞争的、使用旧令牌的升级请求会被拒绝。被吊销设备已准入的命令继续执行；其回复丢失。

## 4. 时序与冲突

```mermaid
sequenceDiagram
  participant D as Phone
  participant T as Terminal
  participant TL as Timeline
  T->>TL: stop/interrupt，序号 n
  D->>TL: approval.respond 到达，Approval 检查已消费的事实
  Note over TL: 请求已在 n 处 approval/cancelled
  TL-->>D: 响应失败，不存在或已解决
```

远程与本地客户端之间的每场竞争都由所属插件内部的 Timeline 序号裁决；RemoteGateway 只决定其自身的配对、吊销与轮换顺序。

| 场景 | 结果 | 负责方 |
| --- | --- | --- |
| 手机与终端回答同一审批或问题 | 审批：第一个被消费的事实决定；问题：第一个被接受的 `session.question.respond` 决定；第二个失败 | Approval、Questions |
| 手机 `session.permission.update` 与终端竞争 | 两个 `permission/changed` 事实；序号较后者生效 | Permission |
| 手机在输入被应用时撤回该输入（`session.input.withdraw`） | `input/applied` 先于 `input/withdraw`：`notHeld`，不写入任何内容，正常消费；`input/withdraw` 在先：`input/deleted`，从不被应用 | InputQueue |
| 带待处理审批断开连接 | 保持待处理；重新附着时重放；无超时（A14） | Approval |
| 该设备的请求运行时被吊销 | 连接关闭；命令继续；回复丢失 | Rpc |

完整表格：[附录 B](#附录-b-完整冲突表)。

## 5. 上下文映射

自身无模型可见效果。远程客户端的内容与本地客户端内容完全一样地进入模型：`session.submit` 用户内容（文本、图像）在输入被消费的位置成为一条用户消息，不带来源标记，因为用户就是其作者；审批、停止、撤回和权限变更不是模型内容。压缩、子分支继承和非交互 Session 遵循 Session 输入规则（[session access](../contracts/session-access.md)）。模型从不看到设备 id、设备名、令牌、入口名或诊断。

## 6. 接口（摘要）

| 接口 | 类型 | 用途 |
| --- | --- | --- |
| `GET /`、`/assets/*` | HTTP | Web 工作区静态资源 |
| `POST /api/pair`、`POST /api/token/rotate` | HTTP | 配对、设备令牌轮换 |
| `GET /ws` | WebSocket | 面向已配对设备的 App Server 协议 v3：全部 Session 方法 |
| `remote.status/pair/devices/revoke/rotate_token` | 管理方法 | 入口与设备的本地管理；入口由 PluginManager 的 `plugin.set_enabled` 开关 |
| `entryHost`（调用内的 `authenticate`、`handle`）、`ConnectionEntry` | Transport/Connection 变更（进程服务、数据记录） | 经认证的入口；设备连接从不获得管理方法 |
| CLI | `/remote`、`/web`、`dimcode remote`、`dimcode web` | 用户入口 |

详见：[附录 A](#附录-a-接口细节)。

## 7. 安全

每个已配对设备可以在宿主的每个 Session 上调用每个 Session 方法：提交输入、回答审批和问题、停止轮次、`session.permission.update`、`session.exec.stop`、`session.model.update`、`session.mcp*`，以及在任何已授权目录中创建 Session（决策 E3、A12）。这放宽了如今仅限本地的执行类 Session 方法，并记录在[兼容性表](#a3-远程入口上的协议与兼容性)中。远程更改的权限预设是 Session 配置（决策 E4）：一个由子分支继承、fork 时随事实一起复制的 Timeline 事实（[#26](https://github.com/arcships/dim/issues/26)）。该入口从不授予管理权限：配对、吊销和配置是管理方法，`daemon.shutdown` 是 Rpc 内置方法；它们都不提供给已配对设备。认证保持最小化（[overview 多节点](../overview.md)）：每设备一个 bearer 令牌，静态存储为哈希，由设备轮换，在本地吊销；配对码短时有效、一次性使用且有速率限制。Cookie 为 `HttpOnly`、`Secure`、`SameSite=Strict`，并进行同源 `Origin` 检查。令牌从不出现在 URL、日志、诊断或 Timeline 中。该入口从不接受 rutis 链路帧，也不导出服务。

## 8. 规则检查

规则：[principles §1 与 §4](../../../architecture/rust-agent/principles.md)，[ADR 002](../../adr/002-plugin-scope.md) D1–D5。

| 规则 | 本文如何符合 |
| --- | --- |
| §4.1 改别的模块拥有的状态时写指令事件 | RemoteGateway 不写 Timeline 事件；远程调用到达所有者自己的方法，由所有者写指令与结果，RemoteGateway 只改动自己的设备记录，并经 Settings 类型化操作改动它的业务来源 `remote`；它从不写自己的结构性 `enabled` 字段。 |
| §4.2 交接后不能撤回 | 远程撤回是 InputQueue 的 `input/withdraw`，在 `input/applied` 之后不生效（`notHeld`）；之后只能用 `stop/interrupt`。 |
| §4.3 只用 before 回调在写入前干预 | RemoteGateway 不在 Session 入口加检查；它只认证传输层升级，Rpc 保留其外部边界检查。 |
| §4.4 核心机制不带业务含义 | `entryHost` 与 `ConnectionEntry` 是通用的传输记录；Rpc 只对设备连接拒绝管理方法。 |
| §4.5 决定前读完整个批次 | 不适用：RemoteGateway 不依据 Timeline 事件做决定；远程调用由所有者按自己已消费的事实决定。 |
| §4.6 模型上下文只含交接后被读取的事件 | 远程内容只作为被消费的普通 Session 输入进入模型。 |
| §4.7 插件之间只通过公开事件和服务交互 | RemoteGateway 不了解任何 Session 方法的含义，只使用 Transport、Settings、`pluginFiles` 与 Rpc 的方法收集。 |
| §4.8 向机制所有者提通用需求 | 中立服务协议中提供方在调用内向调用方发请求的需求见 §9.1；loader 类型化操作（#3）见 PluginManager §9.1；Transport 与 Connection 变更列在 overview 附录 B.2。 |
| §4.9 只有后续决定要用的事实才进 Timeline | RemoteGateway 不写 Timeline 事件；设备记录、邀约和连接留在 `pluginFiles` 和内存中，远程调用只写所有者自己的事实，不含设备归属。 |
| §1 Capability 与 App Server 边界 | RemoteGateway 是位于自己 crate 的进程插件，不是 Capability；Transport、Connection 与 Rpc 只做通用的入口变更。 |
| ADR 002 D1 作用域跟随状态所有者 | 设备与监听器按宿主存在，因此 RemoteGateway 是进程作用域。 |
| ADR 002 D2 不设接线模块 | Rpc 按名字收集 `managementMethods`；没有接线模块。 |
| ADR 002 D3 依赖跟随作用域 | RemoteGateway 单向依赖 Transport 的 `entryHost`；Rpc 不依赖 RemoteGateway，它的重载不会重载 Rpc。 |
| ADR 002 D4 结构性配置与业务配置分离 | 只有 `enabled` 是结构性的，且只由 PluginManager 写入；`bind`、`tls`、`plaintext` 是来源 `remote` 中的业务配置，变更时重新绑定监听器而不重载。 |
| ADR 002 D5 合同语言中立 | `entryHost`、HTTP 接口与各方法以中立形式给出；附录 G 只是绑定。 |

## 9. 向机制所有者提出的需求

### 9.1 提供方在调用内向调用方发请求

已撤回：rutis 是外部依赖，不向它提需求。认证与处理由 RemoteGateway 和 Transport 用 dim 侧的服务解决。

| 项 | 内容 |
| --- | --- |
| 被哪条规则挡住 | `entryHost`（A.1）在 `open` 运行期间必须向调用方请求 `authenticate` 与 `handle`，而 ADR 002 D5 要求契约有中立形式；目前中立服务协议只支持调用方向提供方发请求，以及调用内的通知。 |
| 考虑过的现有机制 | 调用内通知没有回复；`callChannel` 是由提供方调用的另一个服务，用它会让 Transport 依赖 RemoteGateway；由 RemoteGateway 提供、Transport 收集的另一个服务同样会反转依赖。 |
| 需要的通用能力 | 提供方在一个打开的调用上向调用方发请求并得到回复，作用范围限于该调用，调用结束时随之结束；不了解入口、设备或 HTTP。 |
| 对序号、批次、中断、重载的影响 | 不涉及 Timeline 序号或批次；丢弃调用会取消其未完成的请求；任一方重载都会结束该调用及其请求。 |
| 由谁实现，对应 issue | rutis（服务协议与 rutis-bridge）；尚无 issue。Rust 之间的使用（附录 G）不必等待它。 |

### 9.2 其他需求（在别处写明）

| 需求 | 所有者 | 五项内容 |
| --- | --- | --- |
| loader 可编辑层上的类型化操作，PluginManager 用它写 RemoteGateway 的 `enabled` 字段 | rutis-loader，[#3](https://github.com/arcships/dim/issues/3) | 见 [PluginManager](PluginManager.md) §9.1 |

`entryHost`、`ConnectionEntry {name, deviceId}` 以及 Rpc 不向设备连接提供管理方法，是对 Transport、Connection 与 Rpc 的改动，列在 [overview 附录 B.2](../overview.md)。

## 附录 A. 接口细节

### A.1 Transport 变更（拟议，由 Transport 拥有）

一个进程服务 `entryHost`（无 key），由 Transport 提供。入口的认证器与 HTTP 路由不是由 Transport 收集的服务：它们是入口打开期间 Transport 在 `open` 调用内发给调用方的请求，与[方法](../contracts/session-boundary.md)契约中 `callChannel` 的按调用方式相同。因此 RemoteGateway 依赖 Transport，Transport 不依赖 RemoteGateway 的任何内容。错误是数据 `{ code, message }`。

| 方法 | 类型 | 请求 | 回复 |
| --- | --- | --- | --- |
| `entryHost.open` | 异步，入口运行期间保持打开 | `{ name, bind: "<ip>:<port>", tls?: { cert_path, key_path } }` | 监听器绑定后在该调用上发出通知 `opened { entry_id, address }`；入口关闭后以 `{}` 结束 |
| `entryHost.close` | 异步 | `{ entry_id }` | 监听器及其接受的每个连接关闭（脱离）后返回 `{}` |
| `entryHost.closeDevice` | 异步 | `{ entry_id, device_id }` | 该设备的连接关闭后返回 `{}` |
| `authenticate`（调用内请求，Transport 发给调用方） | 异步 | `{ headers, origin?, remote_addr }`（令牌只在 headers 或 cookie 中） | `{ verdict: "allow", device_id } \| { verdict: "deny" }` |
| `handle`（调用内请求，Transport 发给调用方） | 异步 | `{ method, path, headers, body_base64? }` | `{ status, headers, body_base64? }` |

入口接受的每个连接都携带数据记录 `ConnectionEntry { name, deviceId }`（`deviceId` 来自 `authenticate` 的回复）；对带 `deviceId` 的连接，Rpc 只提供 Session 方法（A1）。吊销调用 `closeDevice`；丢弃 `open` 调用或调用 `close` 会关闭入口。Transport 与 RemoteGateway 是 Rust 实现，使用附录 G 中的绑定。

没有 RemoteGateway 时，裸 WebSocket 保持现有规则（受保护 profile：只允许 `hello` 和 `daemon.ping`）。

### A.2 HTTP 接口面

| 路径 | 认证 | 行为 |
| --- | --- | --- |
| `GET /`、`GET /assets/*` | 无 | 限定在发行包 assets 目录内的静态文件；不可变缓存头；严格的 Content-Security-Policy（A5） |
| `POST /api/pair` | 配对码 | `{code, deviceName, web?}` → `{deviceId, token}`；`web: true` 时改为设置 `HttpOnly; Secure; SameSite=Strict` cookie（A6）；邀约一次性使用 |
| `POST /api/token/rotate` | 当前或上一个令牌 | 新令牌；上一个令牌对该端点和升级请求保持有效，直至新令牌被使用或经过 10 分钟（A7） |
| `GET /ws` | Bearer 令牌，或带同源 `Origin` 的 cookie | 升级；未知、已吊销或 `rotation_required` 时返回 401 |

令牌：32 个随机字节，base64url；SHA-256 哈希以常量时间比较。配对码：8 个 Crockford base32 字符（A8）。尝试次数：每个邀约 5 次，每个入口每分钟 20 次；超出则使邀约失效（A9）。默认 loopback；非 loopback 需要 `tls` 或显式 `plaintext: true`（A10）。

### A.3 远程入口上的协议与兼容性

| 项目 | 规则 |
| --- | --- |
| 版本 | 仅 `hello` v3；v1/v2 以 `protocol_version` 拒绝（A11） |
| 方法 | Rpc Session 内置方法和每个 `sessionMethods` 服务，作用于宿主上的每个 Session（E3、A12）；管理方法（`managementMethods` 与 Rpc 的管理类内置方法）以及 Rpc 内置方法 `daemon.shutdown` → `-32601 unknown_method` |
| `hello.features` | 仅 Session 功能 |
| 身份 | `ConnectionEntry.deviceId` 用于诊断与吊销；不授予超出入口方法集的任何权限 |
| 反向请求 | 无：客户端发请求，服务端发通知（[#5](https://github.com/arcships/dim/issues/5)） |

兼容性（来源：[protocol.rs](../../../../crates/dim-app-server/src/protocol.rs) 中的 `LOCAL_EXECUTION_METHODS`、`Call::is_management`；[connection.rs](../../../../crates/dim-app-server/src/connection.rs) 中的 `validate_call`）：

| 客户端 | 之前 | 之后 |
| --- | --- | --- |
| v3 远程入口（新） | 不存在；执行类 Session 方法（`session.permission.get/update`、`session.exec.stop`、`session.skills`、`session.model.update`、`session.prompt.refresh`、`session.mcp`、`session.mcp.refresh`）仅限本地 | **兼容性变更（E3）：**与其他全部 Session 方法一起对已配对设备可用；管理方法和 `daemon.shutdown` 为 `unknown_method` |
| v3 裸 WebSocket，受保护 profile，无 RemoteGateway | 除 `hello`/`daemon.ping` 外均为 `local_transport_required` | 不变 |
| 远程入口上的 v1、v2 | 不存在 | `protocol_version`（A11） |

### A.4 方法（`managementMethods`，key `remote`）

| 方法 | 参数 | 结果 | 中断 |
| --- | --- | --- | --- |
| `remote.status` | `{}` | `{enabled, running, address?, tls, error?, devices, connections}` | Drain |
| `remote.pair` | `{ttlSeconds?}`（默认 300，最大 900） | `{code, url, expiresAt}`；`url` 为 `<origin>/pair#code=<code>` | Drain |
| `remote.devices` | `{}` | `[{deviceId, name, pairedAt, lastSeenAt, connections, rotationRequired}]` | Drain |
| `remote.revoke` | `{deviceId}` | 记录删除且连接关闭后返回 `{}` | Report |
| `remote.rotate_token` | `{deviceId?}`（缺省：全部） | 标记持久化且连接关闭后返回 `{}` | Report |

RemoteGateway 没有启动或停止方法，也从不写自己的 `enabled` 字段。入口由 PluginManager 的 `plugin.set_enabled {id: "remote-gateway", enabled}` 开关（[PluginManager](PluginManager.md) A.5），它经 loader 类型化操作（#3）写入该字段；loader 投递 `VolatileUpdate` 而不重载插件，RemoteGateway 随之调用 `entryHost.open` 或 `close`。监听设置 `bind`、`tls`、`plaintext` 经 Settings 类型化操作写入业务来源 `remote`，遵循 RemoteGateway 声明的 schema（该 schema 拒绝没有 `tls` 或 `plaintext: true` 的非 loopback `bind`）。CLI `/remote` 和 `dimcode remote start` 先写设置，再调用 `plugin.set_enabled`，然后读取 `remote.status`。入口运行期间业务来源变更时，RemoteGateway 重新绑定监听器：以新设置关闭并重新打开入口（连接脱离），而不重载插件（A16）。

### A.5 Capability 机制

| 步骤 | 路径 |
| --- | --- |
| 写入 | RemoteGateway 不写入。远程调用与本地调用一样经 Rpc 到达所有者：`session.submit` → Session 输入边界（`Timeline::append`），`approval.respond` → 分支 Approval（`approval/respond`），`session.stop` → `stop/interrupt`，`session.permission.update` → Permission（`permission/changed`），`session.input.withdraw` → InputQueue 的 `sessionMethods`（撤回指令 `input/withdraw`；若 InputQueue 仍持有该输入，则写入 `input/deleted`） |
| Dispatcher | 不订阅；设备通过其每连接的 Session 附着接收事件 |
| 并发附着方 | 无独占所有者（A13）；全部以每分支顺序收到相同事件 |
| 重载 | 关闭入口：监听器与远程连接关闭（脱离）；记录保留；邀约丢失；RemoteGateway 重新激活前 `remote.*` 返回 `unknown_method`，Rpc 不重载；在途的 `remote.*` 调用按其 `interruption` 处理。Transport 重载会重载 RemoteGateway（它依赖 `entryHost`）；运行时设置变更从不重载它 |
| 进程重启 | 若 `enabled` 则重新打开入口；设备重连并重新附着；已提交的输入与待处理审批在附着时重放 |

## 附录 B. 完整冲突表

| 场景 | 序列顺序 | 结果 | 决策负责方 |
| --- | --- | --- | --- |
| 手机与终端回答同一审批 | 第一个 `approval/respond` | 第一个决定；第二个 `invalid_params` | Approval |
| 轮次 `stop/interrupt` 之后的远程回答 | 中断 < 响应 | 请求已 `approval/cancelled`；响应失败 | Approval |
| 中断之前的远程回答 | 响应 < 中断 | 被接受；工具仍可能在启动前被停止 | Approval、ToolExecutor |
| 附着期间的分支级中断或分支关闭 | — | 事件被投递；之后在该分支上的调用失败 | Session 所有者 |
| 终端 Queue 输入待处理时手机 Steer | Timeline 顺序 | 按序号顺序依模式应用 | InputQueue、Loop |
| 远程撤回与输入的应用 | InputQueue 序号顺序中的 `input/applied` 对 `input/withdraw` | 应用在先：`notHeld`，不写入任何内容；撤回在先：`input/deleted`，从不被应用 | InputQueue |
| 未结算工具批次期间的远程提交 | 输入 < `step/tool-result` | Steer/Append 在批次结果之后；Queue 在 `turn/end` 之后 | Loop |
| 远程 `session.permission.update` 对终端更新 | 序号 | 较后的 `permission/changed` 为当前值；工具准入使用准入时消费的策略 | Permission、Approval |
| 远程 `session.exec.stop` 对 exec 完成 | 序号 | 较早的终态事实胜出 | WorkspaceTools exec 所有者 |
| 远程输入附近的压缩或子分支切分 | — | 同任何输入（[session access](../contracts/session-access.md)） | ContextPreparation、InputQueue |
| 远程权限变更后的 fork | — | fork 随事实一起复制预设（E4、#26） | SessionStore |
| 两台设备回答同一问题 | 第一个被接受的 `session.question.respond` | 第一个被接受；第二个 `question_not_pending` | Questions |
| 审批待处理时断开连接 | — | 保持待处理；重新附着时重放（A14） | Approval |
| `session.submit` 已准入后断开，回复丢失 | 输入已提交 | 重连后按 `submissionId` 调用 `session.input_lookup` | 客户端 |
| 设备请求运行时被吊销 | 已准入 < 吊销 | 连接关闭；命令继续；回复丢失 | Rpc |
| 升级请求与吊销竞争 | 记录删除 < 连接关闭 | 使用旧令牌的升级被拒绝 | RemoteGateway |
| 两部手机使用同一配对码 | 第一个 `POST /api/pair` | 第二个失败，计为一次尝试 | RemoteGateway |
| 轮换响应丢失 | — | 上一个令牌在宽限窗口内有效；设备重试 | RemoteGateway |
| 设备轮换期间 `remote.rotate_token` | 持久化标记的顺序 | 标记之前完成的轮换也会被标记 | RemoteGateway |
| 有附着设备时 `enabled` 被设为 false（`plugin.set_enabled`） | `VolatileUpdate` | 连接关闭（脱离）；Session 继续 | RemoteGateway、Transport |
| Rpc 重载 | — | 受影响的调用 `service_reloading`；除非协议实例改变，否则连接保留 | Rpc |
| 贡献 Session 方法的 Session 插件重载 | — | 其名称在重载期间 `service_reloading`，之后恢复提供；在途调用遵循[方法](../contracts/session-boundary.md)契约；连接保留 | Rpc、贡献方 |
| 设备已连接时运行时设置变更 | 业务来源 revision | 入口以新设置重新打开；连接脱离并重连 | RemoteGateway |
| Session 被另一个客户端关闭 | `session.closed` | 远程附着方收到它 | Connection |
| 只有远程附着方的 Session 空闲卸载 | — | 附着会钉住 Session；脱离时释放 | AgentIndex |
| Session 的资源节点离线 | — | 到达该节点的调用以 `node_unavailable` 失败，与本地客户端相同；不发生重载，也没有任何东西变为 Pending（[overview 多节点](../overview.md)） | Session 所有者 |
| 存在未完成邀约时进程重启 | — | 邀约丢失；设备保留 | RemoteGateway |

## 附录 C. 错误与恢复

| 情况 | 结果 |
| --- | --- |
| `enabled` 设置后绑定失败 | `remote.status.error` = `remote_bind_failed`；入口未打开；`enabled` 不变（RemoteGateway 从不写它）；由 CLI 报告 |
| TLS 文件不可读 | `remote.status.error` = `remote_tls_invalid`；入口未打开 |
| 无 TLS 或 `plaintext` 的非 loopback 绑定 | 对 `remote` 的 Settings 类型化写入以 `invalid_params` 失败（schema 由 RemoteGateway 声明）；不写入任何内容 |
| 设备文件损坏或不可读 | `remote.status.error`；拒绝所有升级请求；`remote.devices` 以 `remote_storage_failed` 失败；保留文件以供修复 |
| 配对/吊销/轮换时 `pluginFiles` 加锁或写入失败 | `-32000 remote_storage_failed`；内存状态不变；吊销不报告为完成 |
| 未知设备 id | `-32602 invalid_params` |
| 令牌错误的升级请求 | HTTP 401，无细节；计入诊断，不含令牌 |
| 运行中监听器失败 | 入口关闭，设置 `remote.status.error`，`enabled` 不变；守护进程继续运行（A15） |

## 附录 D. 验收

在 App Server 测试套件中基于真实 WebSocket 入口进行流程测试：

1. 对 RemoteGateway 的行调用 `plugin.set_enabled` 会打开和关闭入口而不重载，可由 `remote.status` 看到；RemoteGateway 自己从不写 `enabled`；没有 TLS 的非 loopback `bind` 被业务来源 schema 拒绝；`enabled` 时重启会重新打开；在业务来源中修改 `bind` 会重新打开入口而不重载插件。
2. 使用有效、过期、重复使用的配对码以及尝试次数过多时的配对；`remote.devices` 列出该设备。
3. 已配对设备调用 Session 方法，包括 `session.permission.update`、`session.exec.stop`、`session.model.update` 和 `session.input.withdraw`；每个管理方法（包括 `remote.*` 和 `daemon.shutdown`）返回 `unknown_method`；v1 被拒绝。
4. 两个附着的客户端：都收到事件；竞争的 `approval.respond`；远程响应之前和之后的中断；远程撤回与应用竞争（`input/deleted` 或 `notHeld`）。
5. 带待处理审批和已提交输入时断开连接；重新附着时重放；`session.input_lookup` 找到该输入。
6. 吊销关闭连接并拒绝令牌；轮换宽限窗口；`remote.rotate_token` 强制轮换。
7. 损坏的设备文件导致拒绝所有升级请求，且不重置该文件。

## 附录 E. 假设

| # | 假设 | 依据 | 若被否决的影响 |
| --- | --- | --- | --- |
| A1 | 来自已配对设备的连接（带 `deviceId`）只获得 Session 方法；管理方法从不提供给它 | 方法契约；决策 E3 | — |
| A2 | 设备身份不写入 Timeline | Overview §9；判断 | 输入携带设备归属 |
| A3 | 设备存储在 RemoteGateway 的 `pluginFiles`（扩展）根目录中的 `remote-devices.json` | 决策 R8 | 换用另一存储并变更格式 |
| A4 | 配对邀约只在内存中 | 判断 | 持久化邀约 |
| A5 | Web 工作区资源随发行包提供，无需认证 | 功能清单 `/web`；判断 | 资源需要认证 |
| A6 | 浏览器配对使用 HttpOnly cookie；原生客户端获得令牌 | 判断 | 令牌存于浏览器存储 |
| A7 | 由设备发起轮换，宽限窗口 10 分钟；通过标记强制轮换 | 功能清单“rotate access tokens”；判断 | 不同的轮换模型 |
| A8 | 配对码格式与 32 字节令牌 | 判断 | — |
| A9 | 尝试上限：每个邀约 5 次，每分钟 20 次 | 判断 | 不同的上限 |
| A10 | 默认 loopback；非 loopback 需要 TLS 或显式 plaintext | Overview §6.3；判断 | 策略在 #5 中决定 |
| A11 | 远程入口只接受协议 v3 | 判断 | 入口上支持 v1/v2 |
| A12 | 已配对设备可访问每个 Session 和每个 Session 方法 | 决策 E3；功能清单“access projects and Sessions” | 每入口方法过滤或每设备作用域划分 |
| A13 | 无独占的 Session 所有权；“接管”指附着并操作 | 功能清单“collaborate with or take over” | 控制锁协议 |
| A14 | 无客户端附着时待处理审批无超时 | 现有 Approval 行为 | 在 Approval 中加入超时 |
| A15 | 监听器失败只停止远程入口 | 判断 | 远程失败时守护进程关闭 |
| A16 | `bind`、`tls`、`plaintext` 是 Settings 业务来源中的业务配置；只有 `enabled` 是结构性的，且只由 PluginManager 写入 | ADR 002 D4（监听设置是业务配置） | 监听设置成为结构性 loader 字段 |

## 附录 F. 待对齐问题

| 问题 | 描述 |
| --- | --- |
| [#5](https://github.com/arcships/dim/issues/5) 与 overview 待对齐问题“Remote access” | 产品设备身份与对端令牌的关系、链路断连、TLS 部署 |
| 每设备作用域 | 之后是否可将设备限制到特定工作区或 Session（E3 不要求） |
| Transport/Connection 变更 | `entryHost` 及其调用内的 `authenticate` 与 `handle` 请求在此提出，需要 Transport 变更文档；中立协议的支持是 [§9.1](#91-提供方在调用内向调用方发请求) 中的需求 |

## 附录 G. Rust 绑定

Transport 与 RemoteGateway 是 Rust 实现；它们使用 A.1 的这一绑定，A.1 的中立形式是规范性的。

```rust
/// Binding for service `entryHost` (process, no key).
impl EntryHost {
    /// Maps `open`; dropping the handle maps `close`.
    pub async fn open(&self, spec: EntrySpec) -> Result<EntryHandle, EntryError>;
}
pub struct EntrySpec {
    pub name: String,                                // "remote"
    pub bind: SocketAddr,
    pub tls: Option<TlsFiles>,
    pub authenticate: Arc<dyn UpgradeAuthenticator>, // call-scoped `authenticate`
    pub routes: Arc<dyn HttpRoutes>,                 // call-scoped `handle`
}
pub trait UpgradeAuthenticator: Send + Sync {
    fn authenticate(&self, request: &UpgradeRequest) -> HostFuture<'_, Result<Authenticated, Rejected>>;
}
/// `revoked` maps `closeDevice`: the connection closes when it fires.
pub struct Authenticated { pub device_id: String, pub revoked: CancellationToken }
pub struct ConnectionEntry { pub name: String, pub device_id: Option<String> }
```

`remote.*` 使用[方法](../contracts/session-boundary.md)契约的 `managementMethods` 绑定。
