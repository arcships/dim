# Provider 管理

状态：**已实现**。进程 Provider 管理服务
[#329](https://github.com/arcships/dim/issues/329) 的 Desktop/CLI 流程。
[English](../../../design/appserver/provider-management.md)。
[S4 auxiliaryModel](auxiliary-model.md) 合同保持不变。

## 1. 范围

两端通过同一 App Server 完成 [Providers and models](../../../feature-list.md#providers-and-models)。

| 流程 | 合同 |
| --- | --- |
| 列表与编辑 | 安全管理快照包含连接字段、记录版本及完整模型描述 |
| 连接配置 | UUID 新建与局部更新，保留客户端不能回读的秘密 |
| 断开与删除 | 断开时原子清除凭据并禁用；删除自定义项时清理自有选择 |
| 获取模型 | 探测未保存连接，不写入 Provider、目录、凭据或选择 |
| 模型管理 | 区分默认与用户模型，分别处理描述覆盖和显式 New API 路由；两项上限各自记录声明来源，没有任何来源的模型列出原因且不可选（§3.5） |
| 模型偏好 | 客户端读取持久偏好，并在明确采用模型时携带完整目标 |
| 账号与用量 | 复用账号 owner、合同与错误；不增加静态 Provider 用量配置 |
| 多客户端 | 记录 CAS，打开及自身写入后重读，不推送变更通知 |
| 旧配置导入 | 显式只读导入，映射来源、逐项返回结果并保留源字节 |
| 已存选择 | 对已准入静态记录和显式模型 ID 的一次无副作用投影；数据边界消费方复用 Provider 的路由、描述和可选择性，不再重建目录（§3.7） |
| 进程 owner | Provider 提供一次读取，列出实际会被服务的模型及其 provider/model ID 与已声明能力；路由 owner 用它代替复制可用性判断（§3.6） |

源码入口：[Provider owner](../../../../crates/dim-app-server/src/provider.rs)、
[管理方法](../../../../crates/dim-app-server/src/provider/management.rs)、
[模型类型](../../../../crates/dim-app-server/src/model_catalog.rs)、
[导入](../../../../crates/dim-app-server/src/provider/legacy.rs)、
[账号合同与 Usage](../../../../crates/dim-app-server/README.md)、
[F3](../../../design/model-configuration.md)。媒体执行归 [Modality](../plugins/Modality.md)，
SessionTitle、子代理等用途的模型选择归各消费 owner。

## 2. 所有权与影响面

Provider 拥有进程配置、目录、覆盖、偏好、校验、安全投影、写入和网络任务；账号 owner
拥有登录、token 与凭据租约。SqliteStorage 负责事务和格式准入。客户端只拥有草稿和展示。
已采用的 Session 配置归原 owner；管理调用不跨写状态。

| 表面 | 消费者与改动 |
| --- | --- |
| `managementMethods`，process，命名空间 `provider` | 本地 v3 Desktop/CLI 和受信任进程调用者；§3 沿既有命名空间扩展 |
| Provider 模块边界 `stored_selection`（Rust） | Migration 等原生数据边界消费方；§3.7 增加一个基于已准入记录的无副作用函数，不是 RPC、host 端口或管理方法 |
| Provider host 端口 `SelectableModelsHost`（core `dim-agent-capabilities`），由该配置的模型拥有方提供（Provider，或配置式模型文件） | 按模型能力路由的进程内 owner（Modality）与 `agent` 工具的 `models` 动作；§3.6 增加两次读取，不是管理方法或 RPC 方法 |
| Provider host 端口 `ModalityConnectionHost`（core `dim-agent-capabilities`），由该配置的模型拥有方提供 | Modality 的图像生成、编辑与增强进程适配器；§3.6 定义一次按调用的解析，不是管理方法或 RPC 方法 |
| `account`、`account.codex`、`account.grok` | 复用账号调用，不新增账号方法、事件或 managed host capability |
| `ProviderConnectionResolver`、`ModelSelectionHost` | Provider 沿现有类型处理目录和路由，不增加 core 端口 |
| `auxiliaryModel.complete` | S4 由调用者显式选模型的合同不变 |
| `provider` 设置源，user 作用域（`provider.json`） | Provider 部件注册它；其中 `userAgent` 是 User-Agent 覆盖值（§2 模型请求身份），使该选择成为已声明、已校验的文档，而不是请求内的临时输入 |
| 原生 CLI Provider 流程 | 能力发现、创建、更新、断开随 owner 方法一起迁移 |

业务留在 Provider，不进入 `connection.rs`、`protocol.rs` 或接线插件。不增加 Session
Capability、通知流、目录副本、凭据服务或注册表。继续遵守
[managed provider boundary](../../../architecture/development.md#managed-provider-boundary)，
私有账号实现和凭据不进入本仓库。

### 模型请求身份

dim 通过
[`build_model`](../../../../crates/dim-agent-aimux/src/driver.rs) 构建的每个 chat 模型请求
都在 Provider 层携带自己的身份；这一层的 User-Agent 之后会接上库自己的
`ai-sdk-<package>/<version>` 和 `ai-sdk-provider-utils/<version>`。默认值是
`User-Agent: dim/<version> DimAgent/<version>`、`X-Title: DimCode` 和
`HTTP-Referer: https://dimagent.com/`。`dim` 放在最前，因为按首个 token 识别客户端的
网关会落到 Dim 发布的那一条；`DimAgent` 是旧的 relay 身份，为仍按它匹配的网关保留。
版本取 workspace 版本，由 `env!` 读出，不手写。

媒体请求目前还不携带这一身份：媒体适配器直接构建 provider
（[`dim-modality-aimux`](../../../../crates/dim-modality-aimux/src/media.rs)），三个头都不发送。

身份是 Provider 层的默认值，所以每次调用的头仍然优先：Provider 自己的
`User-Agent`、`X-Title` 或 `HTTP-Referer`（记录里的 `headers`，启动连接里的
`DIM_CUSTOM_HEADERS`）只覆盖该 Provider 的对应头。优先级由高到低：冒充路径、Provider
自己的头、下面的覆盖值、内置默认值、库的部分。

两条路径保留自己的头，不使用这一默认身份：

- 托管的 `openai-codex` Provider 以 Codex CLI 自己的 `User-Agent`、`originator`
  和 `OAI-Product-Sku` 取代这一身份；其 token 请求发送 Codex CLI 的 `User-Agent`
  和 `originator`。仅靠"被覆盖"不够：Provider 层的 `X-Title` 和 `HTTP-Referer` 仍会
  上线，因此该 Provider 的模型根本不装配身份头。它的版本是请求必须冒充的 Codex 发布版本，
  `provider.rs` 与 `managed_oauth.rs` 里的版本号一起改。
- Dim OAuth relay 用自己 `DimCode/<version>`、`X-Title` 和 `HTTP-Referer` 头取代默认身份
  （[`dim-oauth`](../../../../crates/dim-oauth/src/http.rs)），其线上行为不变。

托管的 Grok Provider（`grok-build`）不冒充，像其他 Provider 一样发送默认身份。

| 覆盖 | 位置 | 说明 |
| --- | --- | --- |
| `provider` 设置源，`userAgent` | `provider.json`，user 作用域，归 Provider 部件所有 | 整体替换 User-Agent 字符串；长度为 1–512 个可打印 ASCII 字符，否则由该源的 schema 拒绝。该值由服务该 App Server 的部件持有，因此同一进程里两个 Server 各自独立 |
| `DIM_USER_AGENT` | 承载模型进程的环境变量 | 校验相同，且优先于设置项。App Server 命令行在读取其他启动输入处读取一次，再传给每个它构建的 Server；请求无法承载的值会让该进程启动失败 |

`DIM_USER_AGENT` 优先于设置项，因为它只作用于承载模型的那个进程的一次运行，与环境覆盖在其
它地方的作用方式相同。共享 daemon 保留它启动时的环境，所以新的值要等 daemon 重启才生效。

两者同样位于 Provider 层，所以库的部分仍在其后。设置行把读取与发布串行化，旧文档不会覆盖
新文档。没有短格式：请求无法承载的值会被拒绝，不会被裁剪。

## 3. 管理合同

### 3.1 读取模型

| 方法 | 请求 | 回复 | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.management` | `{providerId, modelId?, page?: {after?, limit?, query?}}` | `{drivers: ManagementDriver[], providers: ManagementProvider[], errors: ProviderRecordError[], next: string|null}`；每个连接新增 `gluedPrefix`，即其记录声明的前缀，每个模型新增 `alias`，即所列 id 所指的 catalog id，以及 `catalogName`，即 `provider.catalog` 为该行发布的名字；未设置时省略 | false | `Drain` |
| `provider.list` | `{}` | 原 `{providers,errors}` 摘要；每项新增 `remoteModelList`，表示 `provider.refresh` 能否列举该 Provider 的模型，客户端据此在空 catalog 的提示行给出或省略该命令，以及 `gluedPrefix`，即其记录声明的前缀，未设置时省略 | false | `Drain` |
| `provider.selection` | `{cwd?}` | 既有 `{selection}`，其 scope/version 独立。带 `cwd` 时只读该作用域的行，该文件夹没有行时返回 `null`；工作区优先于全局是一条 `configuration` 规则，不属于此读取 | false | `Drain` |
| `provider.model.set_enabled` | `{providerId, expectedVersion, modelId, enabled}` | `{version}` | true | `Drain` |
| `provider.catalog` | `{providerId, modelId?}` | 既有 `{catalog}` 和 `ProviderModel`；每个模型的 `name` 在其 Provider 内唯一（见下方 §3.1） | false | `Drain` |

`provider.catalog` 发布的 `name` 在所属 Provider 内唯一。重复判定覆盖该目录的全部行，
停用或不可用的行也算在内，因此开关某一行不会改名其他行，各客户端读到的名字一致。名字按
去除首尾空白后比较。名字唯一的行为保留原文。与其他行同名的行发布其标签候选名
`<自身名字> · <列举结果为该行声明的来源>`，来源取它携带的标签中最后一个 ` · ` 之后的
文字（group 行为 `routing group`）；仅当没有其他行得到同一候选名、且没有行自身声明该名字
时才采用它。同组的其他行改在末尾追加自己的 id，因此两行共用的标签不能命名其中任何一行；
列举结果未声明来源的行没有候选名，同样追加 id。计算所得的名字均由去除空白后的片段拼成，
追加的 id 也算在内，因此仅在空白上不同的两个 id 得到同一个名字。没有任何其他行得到、也没有行
自身声明的追加 id 名字照原样保留，这样保留的名字（标签或 id）都在分配计数之前占位，因此计数
不会抢走其他行自己发布的名字。此后仍被占用的名字再加计数，剩下的行按同一顺序取号：先按 id 名，
再按 id，最后按声明的名字。只有这一步可能受列举顺序影响，且有了该顺序后只有共用同一个 id 与
同一个声明名字的行才会走到，因此同一批行换序后每行得到的名字相同。
这个名字在构建回复时计算，从不存储：所有 descriptor 和持久字段都保留 Provider 或用户声明
的名字，因此保存编辑不会写回后缀，后缀也随其重复行一起消失。此规则适用于所有 Provider，
包括两行同名的自定义 Provider。

详情必须指定 Provider ID，返回该项或明确错误；可再指定 modelId，只返回该模型和它的偏好。模型不存在时返回明确的未找到错误。
管理读取独自提供编辑视图：version、模型 descriptor、管理说明来自同一 Provider 记录快照，
不能把另一次 `provider.catalog` 的目录与该 version 拼接后写入。已知目标的能力读取指定 Provider 和模型 ID；模型选择界面使用 `provider.models.query`。不承诺账号 owner 之间的原子快照。

| `ManagementProvider` 字段 | 类型与含义 |
| --- | --- |
| `providerId`、`displayName` | 稳定 identity 和显示名称，均为字符串 |
| `source` | `builtin` / `custom` / `managed`，不是登录状态 |
| `credentialAuthority` | `static` / `openai_codex` / `grok` / `account`，标明凭据 owner |
| `version` | 记录版本；仅账号已公布但尚无本地目录记录时为 `null`，不能伪造 0 |
| `enabled` | 静态记录为 boolean；账号 owner 的记录没有 Provider 启用字段时为 `null` |
| `account` | 可选 `{namespace, loginMethods, state}`；namespace 为 `account` / `account.codex` / `account.grok`，登录方式为 `browser` / `device`，state 沿该 namespace 现有公开 status 回复；静态 authority 不含该字段 |
| `actions` | 当前可用操作名：`update`、`delete`、`refresh`、`models.create`、`models.edit`、`models.reset`、`models.delete`、`models.set_enabled`、`preferences.update`、`login`、`logout`、`usage`；没有逐操作原因代码 |
| `connection` | 下表静态记录安全连接；连接配置只在可选账号包内时为 `null` |
| `models` | `{model:ModelEntry,source:"default"\|"override"\|"user",enabled:boolean,endpoint:ModelProtocol\|null,unavailableReason:string\|null,missingLimits:("contextWindow"\|"maxOutputTokens")[],actions:string[],endpointOverride?:NewApiEndpoint}[]`；路由覆盖限 §6 可执行子集。仅账号无本地目录记录时为 `null`，坏目录报错。`missingLimits` 指出没有任何来源声明的上限（§3.5），否则为空。`catalogName` 是 `provider.catalog` 为该行发布的名字，仅当它不同于 `model.displayName` 时出现；字段缺失表示显示名就是应显示的名字。它是计算所得，因此 `model.displayName` 始终是 Provider 或用户声明的名字，编辑也不会把计算出的名字写回。模型操作为 `set_enabled`、`edit`、`reset`、`delete`，按来源/authority 限制；`set_enabled` 只出现在没有 `unavailableReason` 的条目上，因为集合会保留不可用条目的启用状态 |
| `preferences` | model ID 到既有 `ModelPreferences` 的映射 |
| `modelsListed` | 读取和不改 `baseUrl`、`credential`、`headers` 的更新不含该字段；其余更新说明本次写入如何处理记录自身的模型：`listed`、`bundled`、`unsupported` 或 `disabled` |

`ManagementDriver` 为 `driverKind:string`、`authTypes:StaticAuth["type"][]`、
`remoteModelList:boolean`，由可执行 driver 推导，只列允许自定义创建的 driver。
`StaticAuth` 沿已有 tagged type；不造动态表单 schema 或第二份 driver 注册表。

| 安全 `connection` 字段 | 类型与含义 |
| --- | --- |
| `driverKind`、`auth`、`baseUrl` | driver 字符串、既有 `StaticAuth`、已校验 URL |
| `credentialConfigured` | boolean；不返回凭据或 secret reference |
| `headerNames` | 字符串数组，不返回 header 值 |
| `proxyUrl` | 无 user info 的 URL 或 `null`；只供展示，不能自动写回 |
| `proxyAuthConfigured` | boolean；代理认证仍为秘密 |
| `timeoutMs` | 正整数或 `null`（driver 默认值） |

列表错误沿用 `ProviderRecordError {providerId,message}`，健康项仍可使用。内置记录没有
remoteModels 且存储 fingerprint 不同于 bundled 定义时，推导 reseed 诊断（§6），
已准入条目和删除操作仍可用。有冲突时指出模型 ID；删除冲突项后，仍提示需重启，
直到下次激活允许访问时采用新定义。接纳和管理读取共用一个 Provider 自有候选构建函数，
包含远程模型替换规则，避免候选不一致。pending 尝试归源接纳所有，不禁用条目。
actions 只说明当前可用性，
不是授权或后续写成功的承诺。模型来源由 §6 的独立持久列表推导；默认项消失后，其覆盖
不能变成用户新增。
账号退出或切换后，已有目录仍保留版本和描述，但模型标为不可用；刷新到当前账号前不提供模型写操作。
`source:"override"` 表示有 descriptor 覆盖的默认模型；编辑用户模型后仍为 `source:"user"`。

### 启动、选择与内存

远程模型列表共用的默认 HTTP 客户端在首次列表请求时创建。本地读取、模型选择和
内置连接配置不会创建它。构建失败通过既有 Provider 错误返回给该操作，不使进程
装配失败；指定代理或超时的请求仍按自己的网络配置创建客户端。

显式选择读取指定 Provider 和模型。默认选择先尝试工作区，再尝试全局保存的选择；
第一个可用目标直接返回。只有两者均不可用时才查询启用且已配置的候选，继续遵守
账号优先、Provider ID 排序和 owner 的实时账号可用性判断。仅给模型 ID 的启动
按精确模型 ID 查询，优先有效 Provider，多个可用 Provider 时拒绝歧义。

内置目录在构建时生成可独立读取的记录和识别键，运行时不展开或保留全量目录。
激活只公布可用源代次，不展开 Provider。逐 Provider 接纳、退役和 pending 冲突
遵循下文“索引源与稀疏接纳”合同。

普通连接和模型编辑保留已接纳的模型基线，包括没有已知源代次的历史行。定义指纹
相同不表示已有历史行可以改为隐式默认值；只有首次配置尚未保存的内置 Provider，
或执行下述明确的源接纳流程时，才建立新基线。

Web 与 CLI 模型选择器打开时查询、关闭时释放结果。已采用模型的能力读取携带
Provider 和模型 ID，不枚举候选目录。具体选择、同步、保留量和峰值验证合同见
[英文对应章节](../../../design/appserver/provider-management.md#startup-selection-and-memory)。

### 模型显示查询

`provider.models.query` 是进程 `managementMethods` 的只读操作，请求为
`{providerId?, modelId?, query?, after?, limit?}`。limit 默认 32，范围 1–128。
query 分别匹配模型 ID、显示名和 Provider 名；`zdr` 也匹配账号 owner 声明的 ZDR 标志。
after 和 next 是按序、排他的 `{providerId, modelId}` 游标，空模型 ID 表示没有模型行的
启用 Provider；客户端原样传回游标，不把游标当作可选模型。

回复为 `{models, empty, next}`。模型行含 providerId、providerName、model、version、
preferences 和 disabled；停用或不可用模型仍可见，以供诊断。empty 项含 providerId、
providerName 和 reason：`{kind:"no_models", listable}` 或 `{kind:"unreadable"}`。
每页最多准入 limit 个模型或空 Provider 候选；错误页可能少于 limit 个模型。
next 为 null 表示结束。非法 limit 返回 `provider_invalid_config`；取消和存储错误
沿用 Provider 操作合同。查询不刷新远端目录，刷新是显式管理操作。

`provider.catalog` 的可选 modelId 用于已知目标的能力读取；回复仍是同一个 catalog
封装，但只含指定模型，并保留所属 Provider 的命名规则。完整字段和游标合同见
[英文对应章节](../../../design/appserver/provider-management.md#model-display-queries)。

### 按需读取管理数据

`provider.management.list` 提供分页概要，请求为
`{providerId?, after?, limit?, accountsOnly?, query?}`。每页默认 32 条，范围 1–128。
`providerId` 与 `after` 互斥；游标是上一页最后访问的 Provider ID。`query` 按 ID
或显示名搜索，`accountsOnly` 只列账户提供商。回复为 `{drivers,providers,errors,next}`；
末页的 `next` 为 null，损坏记录占用页位置并进入 errors，翻页可以越过它。

概要只包含身份、来源、凭据归属、版本、启用状态、账户、操作和安全连接信息，
不包含 models 和 preferences 字段。Web 按需翻页，连接配置使用摘要，打开 Models 后读取详情；
CLI 列表逐页读取概要，指定账户或模型的操作携带相应 ID。

Provider 按 ID 点查或按页读取，每次请求结束释放结果，不保留目录结果、seed 记录
映射或模型投影缓存。版本检查仍以 Provider 为单位；模型详情在同一本地快照内
读取头部、目标载荷和状态以及索引定位的命名依赖，不展开其他模型载荷。接口、所有权和流程见[英文对应章节](../../../design/appserver/provider-management.md#on-demand-management-reads)。

### 索引源与稀疏接纳

使用构建期不可变偏移目录，包含 Provider 声明指纹、有序目标键、命名/匹配输入和
可单独读取的模型载荷。名称复用同一业务规则，包含有效名称补全、Unicode 空白、
覆盖与冲突依赖分量；不缓存发布名称。冲突分量可能覆盖整个 Provider，任意子串
查询也可能需要检查较多索引候选，不能宣称所有查找均为常数工作量。

连接配置通过现有 `provider.management.list` 读取摘要，通过 `provider.update` 的
`reply:"summary"` 接收已提交版本与安全摘要；未指定 `reply` 时仍返回完整快照。
CLI/Web 的普通连接配置不展开模型，Web 在打开 Models 标签时才读取完整编辑快照。

```text
Provider 列表 -> 连接配置 -> 保存（摘要与提交版本）
                   |
                   +-> Models -> 完整模型编辑快照
```

原生 v52 将模型级禁用、偏好、别名和路由按模型键保存，保留原有顺序及凭据引用。
可用目录代次与每个 Provider 已接纳的基线分开：启动不解码或插入内置模型；
访问时按 Provider 原子接纳变化，冲突保留旧基线，在下次启动后访问时重试。
源代次变化时，只重建持久化可变行的有效命名输入，并读取其覆盖或别名所需的基线目标；
不展开无关默认项，派生更新不改变版本或模型载荷。
迁移逐 Provider 保留不可重建的历史，归档源随备份恢复。协议细节由
[英文所有者契约](../../../design/appserver/provider-management.md#indexed-source-and-sparse-adoption)与
[原生格式契约](../../../design/appserver/native-storage-format.md#v52-sparse-provider-state)定义。

### 3.2 写入与字段操作

下表的 `ManagementProvider` 均为 §3.1 的编辑快照。已有方法除明确新增
`ModelEdit.endpoint` 和 §3.4 错误分类外，保持请求/回复；已有写方法继续
`mutating:true`、`interruption:Drain`。

| 方法 | 请求 | 回复与语义 | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.create` | `{providerId,displayName,connection,models:ModelEntry[],enabled,gluedPrefix?}` | `ManagementProvider`；只创建，客户端生成新 UUID，已存在 ID 冲突；`gluedPrefix` 按 patch 的规则：去空白，空值即未声明 | true | `Drain` |
| `provider.update` | `{providerId,expectedVersion,patch}` | `ManagementProvider`；校验后单事务局部写入；patch 改动 `baseUrl`、`credential` 或 `headers` 时，同一次 CAS 写入还用新 endpoint 的列举结果替换记录自身的模型，`modelsListed` 说明结果；只改 `network` 时单独保存 | true | `Drain` |
| `provider.probe` | `{providerId,connection}` | `{models:ModelEntry[]}`；只做 §3.3 网络探测，不写入或检查版本 | false | `Drain` |
| `provider.models.create` | `{providerId,expectedVersion,model:ModelEntry,reply?:"full" \| "summary"}` | 默认返回既有 catalog，`reply:"summary"` 返回 `{version}`；写独立用户新增列表，与默认/用户项重复的 ID 拒绝；以启用状态新增，同事务清掉该 ID 遗留的 modelConfigOverrides、disabledModelIds、modelPreferences 和 endpointOverrides 项 | true | `Drain` |
| `provider.models.delete` | `{providerId,expectedVersion,modelId,reply?:"full" \| "summary"}` | 默认返回既有 catalog，`reply:"summary"` 返回 `{version}`；仅删用户新增项，同事务移除该 ID 的 disabledModelIds 项并清理自有偏好/路由/选择 | true | `Drain` |
| `provider.preferences.update` | `{providerId,expectedVersion,modelId,patch,reply?:"full" \| "summary"}` | 默认返回 `ManagementProvider`，`reply:"summary"` 返回 `{version}`；effort/tier 分别 set/clear，按模型校验 | true | `Drain` |

`provider.models.edit` 与 `provider.models.reset` 接受同样的可选 `reply`，分别答复既有 catalog 或 `{version}`。只重读所改行的客户端请求 summary，提交后的版本即其下次写入的 CAS 令牌；省略 `reply` 时对既有调用方保留完整答复。

客户端发送前生成并保留 UUID。create 只接受规范的小写、带连字符 UUID，其他值返回
`provider_invalid_config`，不从名称生成 identity。已有内置/导入的非 UUID ID 对其他方法
仍然有效。回复不确定时按该 ID
重读；重复 create 返回 `provider_conflict`，不覆盖或重复创建。删除后另行创建用新 UUID。
不增加 tombstone、跨删除版本时钟或幂等服务。

创建 connection 必填 `driverKind`、`auth`、`baseUrl`，可选 `network:ModelNetwork`、
`headers:Record<string,string>`、既有 keep/clear/set/set_aws/set_ref `credential` 操作。
新记录缺 headers/network 表示无覆盖，缺 credential 表示无凭据；启用的 keyed Provider
必须有合法凭据。`create.models` 只承载 probe 结果，写入默认列表；不支持发现或未发现模型
时传空列表。refresh 可以替换默认项，models.delete 不能删除它们。创建后，手动项通过
`provider.models.create` 加入 userModels，保留于 refresh 后且允许删除；模板项也用此方法，
按 `{providerId,modelId}` 复制完整公开 `ModelEntry`，不保存模板引用或客户端 bundled catalog。
客户端不反向转换执行投影 `ProviderModel`。

`update` 改动 `baseUrl`、`credential` 或 `headers`（记录所达 endpoint 或账户）时，若记录自身的模型不属于
bundled catalog（bundled 未定义该 Provider，或 `remoteModels` 已为真），就在写入之外列举
新 endpoint 的模型，并与连接在同一次 CAS 写入中提交；用户模型冲突规则与 `refresh` 相同。
非法 patch 在列举之前就被拒绝，返回带原因的 `provider_invalid_config`。列举
失败则更新失败、不写入任何内容，重新指向不会把旧 endpoint 的模型留在新 endpoint 上。
记录已停用而模型属于自身时，同一次写入丢弃这些模型、不发送请求：它们属于旧 endpoint，
空的、可列举的 catalog 由记录启用后选择器的填充补齐。
只改 `network`（代理或超时）时单独保存、不列举：它改变到达 endpoint 的方式而非其内容，
若要求列举成功，列举失败时这些设置就无法保存。driver 不能列举时只保存连接。
模型属于 bundled catalog 的记录在连接改动后仍保留它们，
之后的 bundled 更新也照常到达；显式替换仍走 `dimcode models PROVIDER --refresh`。写入的
列举结果按 `refresh` 的方式置 `remoteModels`；回复的 `modelsListed` 说明属于哪种情况。

| patch 字段 | 操作 | 可写记录 |
| --- | --- | --- |
| `displayName` | 非空字符串 | 仅 custom |
| `baseUrl` | 已校验 URL 字符串 | 静态 authority 的 builtin/custom |
| `enabled` | boolean | 本地 builtin/custom |
| `credential` | 既有 keep/clear/set/set_aws/set_ref；`keep` 与省略同等，均不改动，因此只含 keep 的 patch 即空 patch | 仅静态 authority |
| `headers` | `{name,action:"set",value}` / `{name,action:"remove"}` 数组；未提及名称保留 | 静态 authority 的 builtin/custom |
| `network.proxy` | `{action:"set",value:URL}` / `{action:"clear"}` | 本地 builtin/custom，沿用 authority 校验 |
| `network.timeoutMs` | `{action:"set",value:正整数}` / `{action:"clear"}` | 本地 builtin/custom |
| `gluedPrefix` | 字符串设置该 Provider 列出 id 携带的前缀；`null` 清除它 | 静态 authority 的 builtin/custom |

空的 `provider.update` patch 返回 `provider_invalid_config`，不修改记录或版本。

字段省略保留；null、未知字段、重复/冲突 header 名及非法操作在边界拒绝。header 名在该处
按大小写无关处理。代理和超时独立更新；替换代理 URL 明确替换其认证。内置 identity、名称、
driver、auth、默认模型只读。custom 的 driverKind/auth 创建后同样不可改，需另建 Provider
并显式选择。账号凭据/连接字段由账号 owner 修改。偏好 patch 为
`{reasoningEffort?:{action:"set",value:string}|{action:"clear"},serviceTier?:{action:"set",value:string}|{action:"clear"}}`。
空 patch 或模型不支持的值返回 `provider_invalid_config`；未知字段或格式错误的操作
沿 §3.4 输入边界错误。两字段独立修改，clear 表示
随后显式采用时使用目录默认。只校验本次设置的值，不重验另一字段保留的值。
保存偏好来自用户选择（含旧用户配置导入）；发现结果只提供支持的选项，不产生这些选择。
描述、路由编辑和刷新都不自动删除不再受支持的用户偏好。显式采用失败时标明保存的偏好，
由用户分别设置或清除字段。

已有 delete 保持内置禁用/自定义删除。models.edit 保留 `ModelEdit`，只为静态 New API
模型增加 `endpoint:{action:"set",value:NewApiEndpoint}|{action:"clear"}`，限 §6 可执行子集。
override 优先，clear 回到发现结果，不删除 metadata；未知上游字符串仅作描述。
仅设置或清除 endpoint 的编辑只写 endpointOverrides，不创建或修改 descriptor 覆盖。
models.reset 只移除 descriptor 覆盖，保留端点覆盖；endpoint: clear 只移除路由覆盖。
models.edit 另增 `alias:{action:"set",value:string}|{action:"clear"}`，写入记录的别名映射而不是
descriptor 覆盖：别名是 Provider 对其 id 的声明，只设置别名的编辑不留覆盖给 reset 处理。
models.reset 恢复仍存在的默认项，用户项用 delete。refresh 保留用户模型、覆盖、偏好和
显式禁用，沿用 admitted 配置冲突检查。托管目录换账号 refresh 时，偏好随 descriptor 覆盖
和禁用项一起清空；同账号 refresh 则保留它们。托管账号目录不通过这些方法新增用户模型/路由。
静态提供方只要已保存凭据或使用匿名认证，且驱动和定义都支持远端发现，就可以显式刷新。
禁用的提供方刷新后仍保持禁用，模型仍不可用于执行。
models.edit 修改用户模型的 descriptor 时替换该项 userModels，不把用户 ID 写入 modelConfigOverrides。
models.set_enabled 接受默认模型及用户模型的 ID，并给出账号可用的完整集合：不对应任何目录项的
ID 被拒绝，不可用项保持原有启用状态。这些操作从同一记录更新有效目录和路由，
endpointOverrides 仍是独立映射。

删除自定义 Provider 时，同事务清理其自有选择。内置项 delete 禁用记录并保留 scoped 选择，
后续默认解析跳过不可用项。删除用户模型同事务清理其自有选择、偏好、路由
及 disabledModelIds 项。新增用户模型以启用状态开始，不带 descriptor 覆盖、偏好或端点覆盖，
并清掉该 ID 的遗留项，不会继承已消失模型的设置。
Agents profile、SessionTitle 配置、已采用 Session 保留原 ID，后续使用返回 not found/unavailable。
不跨写其他 owner，也不静默换模型。新建 Provider 使用新 ID，不重新绑定这些引用。

CLI 对不支持列模型的 Provider 仍保存连接，并说明未测试密钥。写入失败保留草稿；
随后重读刷新版本和安全状态，由用户确认后重试。表单列表读取失败可用 Ctrl+R 重读，保留编辑内容。

### 3.3 Probe、偏好采用与用量

新连接 probe 携带客户端已生成的标准小写 UUID，提供完整创建 connection；UUID 只作为
既有密钥 host 的 Provider 上下文，不创建记录。完整 connection 形状选择新草稿 probe，
即使该 ID 已存在；此时 credential:keep 没有已存凭据可保留。已有连接提供原 ID，connection 采用 §3.2 patch。
Provider 为该 patch 读取一次当前记录，在内存合并配置，沿原 authority 取得凭据。probe 不检查版本，
不写入，其结果也不能作为后续写入的版本依据。请求前先做与
create/update 相同的 URL/header/network 校验。测试修改后的 base URL 会把已保存凭据发到
新 host，即使不保存；界面须说明。该权限来自已有本地管理边界。
不持久化临时记录、目录、选择或凭据。不支持列模型返回 provider_unsupported，仍可手动添加。
不返回秘密或原始响应正文。草稿过期后 probe 继续到请求超时，客户端丢弃回复；断线/离开
页面不能取消。owner 卸载才沿既有 token 取消并 drain。

Provider 持久保存的偏好供客户端读取；[F3 客户端](../../../design/model-configuration.md) 把偏好和其所属的**完整 `{providerId,modelId}`** 一起传给既有
session.create/session.model.update。指定目标不可用就报错，不去掉目标重试或回退其他模型。
请求没有声明的字段，由服务端按该 `{providerId,modelId}` 已保存的值补齐：
session.create、session.model.preview 和带目标的 session.model.update 逐个字段取值，
先是客户端声明的值，再是该模型保存的值，最后是模型声明的默认值。客户端声明的字段优先。
模型已不再接受的保存值只丢弃该字段，改用声明的默认值，并由服务端记一条警告；
客户端声明的值被拒仍使采用失败。无目标 F3 只改已采用模型的
指定参数。哪些目录声明会被应用、其余为何只描述模型，见
[模型配置规则](../../../design/model-configuration.md#model-parameters)。
`/fast` 仍只修改当前 Session；保存 serviceTier 与保存推理档位一样由服务端应用。
sessions.launch——Scheduler、Workflow 及其他插件启动工作的方式——不是客户端 Session：
它保留目录默认值，不继承聊天偏好；其他消费模型同样不继承。

用量复用已交付的 `account.usage {providerId} -> Usage`：原生 account namespace 转发
一次调用到可选包既有 usage 方法。[Usage DTO](../../../../crates/dim-app-server/README.md)
及账号错误不变。不增加静态 Provider 用量配置、探测、CAS、缓存/状态，不增加账号代次复查或
managed host API。缺失指标保持缺失；明确 unlimited 桶保持 DTO 约定。

hello.features 在 account namespace 活跃时就公布 account.usage，**不证明包实现了 usage**。
对 account.providers 返回的条目，方法已公布时可显示查询动作。包/方法缺失可能返回
account_provider_unavailable，展示实际安全错误，不自行改称“不支持”，不影响模型/登录。
unknown_method 仅说明旧 daemon，不作为包能力发现机制。更精细的错误/发现合同归账号 owner。

### 3.4 错误、准入与兼容

面向管理的 Provider 操作在 dim-app-server 内返回 Provider 自有错误枚举，
provider/management.rs 直接把其变体映射为下列代码。不扩展共享 HostErrorKind，
也不从通用 host 错误字符串反推业务类型。

业务错误沿 JSON-RPC `code:-32000`、安全 message 和 data.appError：
provider_not_found、provider_conflict、provider_invalid_config、provider_unsupported、
provider_auth_required、provider_auth_failed、provider_transport_failed、provider_cancelled、
provider_failed、provider_catalog_stale、provider_account_changed。按 owner 类型错误分类，不解析 message。已有 delete、models.edit/reset/
set_enabled、select、refresh 的不存在、CAS/admitted-refresh 冲突、校验错误也分别映射这些
类型；请求/回复和 JSON-RPC code 不变。未知方法和错误 RPC 参数保持通用边界错误。
秘密、secret reference、header 值、代理认证和上游正文不进入错误、日志或诊断。

托管 Provider 的 models.set_enabled、models.edit、models.reset 与 preferences.update
先检查记录版本；版本不匹配仍返回 provider_conflict，即使账号同时变化，也应先重读。
版本匹配但账号已退出，或目录来自另一账号 epoch/generation，返回 provider_catalog_stale，
不修改记录。普通凭据刷新仅增加 credentialRevision，不使目录失效。

provider_catalog_stale 在 data.appError 旁增加以下字段：

| 字段 | 类型和含义 |
| --- | --- |
| providerId | 托管 Provider ID |
| expectedVersion、currentVersion | 请求和拒绝时的记录版本；版本检查优先，所以二者相等 |
| issuer、subject | 当前公开账号身份；已退出时均为 null；不包含资料、scope、凭据 epoch 或 generation |
| requiredAction | 已登录时为 provider.refresh，包括身份变化；已退出时为 account.login |

provider.refresh 表示刷新该 Provider，使用回复的版本重试原写入至多一次。
account.login 表示完成账号所公布的登录流程，并在新写入前刷新目录。
客户端不解析消息、不比较账号代次，也不因 provider_conflict 自动刷新。
账号 owner 报告变化，或刷新进行期间检测到账号变化时，返回 provider_account_changed；
先重读账号状态再决定后续动作，不把它当作记录版本冲突。

新方法、endpoint 编辑和类型化 owner 错误随可发现的 provider.management 单元一起发布。
create/update 替代 provider.save 并与 CLI 调用者一起移除旧方法，不回退 save/直接改库。
Rpc 沿用 service_reloading/service_unavailable 和准入；调用由 owner 追踪、卸载时 drain。
断线只丢回复，不撤销已提交写入；先重读不确定结果，不盲重放。业务更新不 reload。

原生 service 与 RPC 使用相同的 create、部分更新与模型操作。新自定义 Provider 使用规范 UUID；
现有内置及显式导入的非 UUID 身份继续通过既定的部分写入接口编辑。

### 3.5 声明的模型上限

模型的上下文窗口与最大输出是来源声明的事实，Provider 从不假定。每项上限的声明来源由目录
已有机制记录：

| 来源 | 记录方式 | 作用范围 |
| --- | --- | --- |
| Provider 描述 | 存储描述上的 `capabilityOverrideFields`；描述中的非正值视为未声明 | 该描述所述的模型 |
| 捆绑目录 | `metadataSource` 指向匹配的捆绑 Provider：条目仅在其 `metadata.raw` 声明处才有上限，其余取值在捆绑加载时删除；该 Provider 的 `contextWindowOverride`、`contextWindowFallback` 元数据仍是声明 | 匹配到的模型，或该 Provider 的全部模型 |
| 用户 | 同一 `capabilityOverrideFields` 字段，写在描述覆盖或用户模型上：`models.edit` 为它编辑的每项上限写入，两个旧版导入器为导入时保留的每项值写入 | 覆盖所指的模型 |

清单以同样的方式声明模型的推理档位，且它声明的第一个形状就是模型自己的列表：先取
`reasoning_options` 中 `type` 为 `effort` 那一项的 `values`，否则取各项
`supported_reasoning_levels` 的 `effort`，最后才是扁平的 `reasoning_levels` 列表。
`reasoning_options` 仍在原处读取，即清单项的顶层或存储条目保留的 `raw` 旁；另两个形状按同样
方式读取。每个取值去空白，非字符串与空串丢弃，重复值保留第一个，因此顺序即清单的顺序。某个
已声明的形状取值全被丢弃即声明空列表：该模型不提供推理档位，识别出的行不补它，存储的描述保留
`effortOptions: []`，使之后再次读取仍声明它。清单未声明任何形状、以及 `reasoning` 为 `false`
的模型都保持原状：识别出的行仍只补未声明的列表，绝不补已声明的列表。`reasoning` 本身不变：
未声明 `reasoning` 的清单仍仅凭档位声明 `supported: true`，与今天仅凭 `reasoning_options`
声明时一致。

先识别模型，再从识别指明的条目读取数字。识别在目录上进行，第一个只留下一个模型的步骤胜出：
Provider 的声明按精确 id；然后是整个限定 id，当它携带的命名空间正是持有该 id 那一行的
Provider 时，聚合方自有的 house model 因此不会被其末段 token 偶然指名的无关行作答；然后是自有
token 是 id 头部、尾部至多为日期、`latest`、tag 后缀或方括号上下文修饰的厂商条目；接着把厂商的
上游规范 id 与显示名合看，显示名将多个规范命中收窄为一个，仍有分歧即歧义而非猜测；然后按同样
两条规则读取 `custom-provider` 行；然后是 Provider 声明该 id 所指的模型，即别名；最后，对既没有
厂商条目、也没有 `custom-provider` 行、也没有别名指名的模型，合并自有 id 为同名同 tag 的聚合行，
且只取安全方向：最小窗口、最小输出上限与 effort 选项的交集。Provider 默认值不是声明，携带其
Provider 在大量行上写出的那一对数值的行在该处不声明任何上限。

第一步的声明是 Provider 自己对某个 id 的说明，绝不是它的列举产出。捆绑 Provider 的声明是它的
捆绑行与用户自行添加的模型；自定义 Provider 在这一步不声明任何东西。列举写出的行属于列举的产出，
既不为它自己作答，也不为同级的另一个 id 作答：列表中的 id 在每次发现与每次读取时都对着目录识别，
而不是对着更早的列举留下的那一行。用户声明的内容在两种 Provider 上都照旧生效，经由
`capabilityOverrideFields` 与其余用户自有状态。

厂商条目是 role 为 `vendor` 的 Provider 上的行，且该行的规范 id 命名该 Provider 自有的命名空间，
或该行未声明规范 id。vendor Provider 上规范 id 指向另一家厂商的行是转售：读取厂商条目的各步骤
跳过它，合并像读取聚合行那样读它。第一步仍会为所属 Provider 读取它，因为无论转售哪家的模型，
捆绑行都是该 Provider 的声明。Provider 自有的命名空间是 `metadata.vendorNamespace`，由同步从该
Provider 的策略写入，与 role 相邻；凡是行上携带规范 id 的 vendor Provider 都声明它。转售另一家
厂商模型、而 upstream 未给出规范 id 的 plan 行，其规范 id 在已提交的 catalog 中声明，并由同步保持。

只有 kind 与该 id 的 kind 相符的行才加入该合并。不声明 kind 的行，也就是 raw `type` 缺失或为
`model` 的行，可加入任意 id。声明 `chat` 的 id 只取声明 `chat` 的行，声明其他 kind 的 id 只取
声明同一 kind 的行，不做同义归并。不声明 kind 的 id，在其同名行中有任一声明 `chat` 时只取声明
`chat` 或不声明 kind 的行，否则取该名的全部行：它因此保留原有合并的那些 media 行，并携带它们的
`type`，正是这个 `type` 让 media 模型不进入对话。没有任何行加入的 id 保持未识别。

Provider 记录声明其列出 id 携带的前缀。该前缀在 id 头部、不区分大小写地只去除一次，早于 id 被
拆成各 token 与名称步骤所比的 token。第一步与限定 id 步骤读取整个 id，因此精确行与限定 id 不受该
声明影响；限定 id 步骤在去前缀之前作答，因为它读的是整个 id，会让它不剩 token 的前缀因此夺不走
它。名称步骤看到的是去前缀后的 id，因为中转方给 id 加了前缀，而不是给模型加了前缀。前缀是
token 头部，因此 `auto` 与 `auto-` 是同一份声明；把整个 id 消耗掉的前缀不留下任何名称，于是识别
不出任何模型，而不是匹配一个空名称。发现流程写入 Provider 上报的 id，并在不带前缀的情况下读取
捆绑目录：前缀是用户对其所见 id 的声明，不是列举可以推断的东西。`provider.create` 按与
`provider.update` 相同的规则声明前缀：去空白，空值与未声明是同一份声明。

Provider 记录还把列出的 id 映射到它所指的模型 id。该别名是规则读不出的 id 的退路，因此在所有
规则之后、合并之前读取：它从不覆盖规则能读出的 id。它的目标按同一套步骤解析，只是排除别名步骤
本身，因此别名作答的是目标行自己的 rule、score 与数值，而不是另一套词；也正因如此，别名不会
再链到另一个别名。目标本身存在歧义时仍然歧义，并带着目标自己的候选行，因为别名指名一个模型，
不得替分歧选边；目标什么都指不出时，id 保留没有该别名时的拒绝或合并结果，因此过时的声明绝不会
让 id 比原来更差。别名以 Provider 自己拼写的精确 id 为键，区分大小写，因为那个拼写属于 Provider
自身；目标则经过各步骤，大小写折叠已经在那里。

限定 id 的命名空间是身份，不是提示。当它指名某个捆绑 Provider 时，token、规范 id 或显示名步骤
若只找到其他 Provider 的行，则该步骤不作答，并以这些行作为候选拒绝该 id：仅凭末段 token 不得把
限定 id 交给 id 未指名的厂商。命名空间不指名任何捆绑 Provider 的 id 是多个卖方共同列出的普通
名字，这些步骤照常读取。

投影把结果记入 `metadata.identification`：`rule` 取 `provider-exact`、`qualified`、`token`、
`canonical`、`custom-provider`、`merge`、`ambiguous` 之一；`narrowedByDisplayName` 表示显示名
收窄了多个规范命中；`candidates` 指出歧义拒绝的各行；`filled` 指出为该行未声明的每项上限提供
取值的条目、规则与数值。合并对每项上限各取一个最小值，各自来自一行，因此其 `filled` 指出声明
该值的行，并带该合并自己的 `rule`。行自身声明的上限没有 `filled` 记录，`provider.models.edit`
为其声明的每项上限删除对应记录。拒绝时消息指出 Provider 与模型 id、缺失的上限、仍然分歧的
候选行，以及声明它们的写入 `provider.models.edit`。

`metadataSource` 与 `matchedProviderId` 指出投影条目复制自哪一行，对合并即第一行加入的行：
条目的显示名、价格、能力，以及该行自身声明的上限都来自它。它们不指出合并后某项上限的卖方；
`filled` 逐项指出。

识别结果是可用性旁边的事实，不是其理由的一部分。没有任何 reason 值用来指出它：识别成功的模型、
多行仍分歧的模型与什么都没识别出的模型，在未声明上限时都保持 `limits_undeclared`，而结果说的
是三者中的哪一种。列举模型的客户端从它本就取得的回复中读取该结果。终端的模型列表把结果附在行
的 effort 之后，而为 Provider 未声明模型指明写入命令的提示行也一并指出候选行。Web 客户端的读取
器在条目上保留 `identification`，其模型列表把结果作为后缀渲染在行本就写出的理由或上限之后，因此
行原有的内容不会被替换。两端都说明 id 被认作哪个模型、数值来自多行、仍然分歧的行，或什么都没
指名该 id。不读取该字段的客户端照常工作，只是不显示新增内容。

缺失是类型上的值，不是数字。目录侧使用 `CatalogCapabilities`：两项上限都是可选的，没有来源
声明时缺省；存储描述、其覆盖、用户模型、投影结果和托管目录的模型都用它。面向 Session 的
`ModelCapabilities` 两项上限仍然必填且为正，并且没有默认值，因此任何投影、夹具或回退都无法
凭空造出窗口。只有一个转换把目录项变成 `ModelCapabilities`，任一上限缺失时它拒绝；这条转换
就是采用路径，所以未声明的模型无法被采用。

去掉默认值对工作区外的扩展 crate 是破坏性变更：用 `ModelCapabilities::default()` 或
`..Default::default()` 构造的值不再编译。替代品是 `ModelCapabilities::with_limits`
（`crates/dim-agent-types/src/model.rs`）；还需要设置其他字段的值保留结构体字面量，并以该
构造函数为基础。

线上形状：`provider.catalog` 的 `ProviderModel.capabilities` 与 `provider.management` 的
`models[].model.capabilities` 把 `contextWindow`、`maxOutputTokens` 作为可选数字，缺键即
未声明，`missingLimits` 指出缺的项。所有树内客户端都按可选取值：`dim-cli` 以
`CatalogCapabilities` 解码目录项（`crates/dim-cli/src/configure.rs`、
`crates/dim-cli/src/session_host.rs`），`dim-acp` 只按 ID、名称和不可用原因列出模型
（`crates/dim-acp/src/config.rs`），Web 工作区读取同样三个字段（`web/src/rpc/read.ts`），
`dim-client` 不解码该回复。`PROTOCOL_VERSION` 保持 3：客户端随守护进程一起发布，写入这种
目录的构建就是读取它的构建，且 Web 入口拒绝为其他协议版本构建的资源
（`crates/dim-app-server/src/web_entry.rs`、`web/src/rpc/client.ts`）。此变更之前构建、
要求两个数字都在的协议 v3 客户端无法解码含未声明模型的目录；`hello.features` 不协商该形状。

读取结果说明缺哪几项：`ProviderModel` 与 `ManagementModel` 带 `missingLimits`，若它是第一个
适用原因，`unavailableReason` 为 `limits_undeclared`。缺任一项的模型永不可选：`select`、
`configuration`（显式目标、默认解析和 Agents profile）、`apply_preferences`、
`preferences.update` 以及每次请求前、取得凭据之前的可用性校验都沿既有不可用路径拒绝，请求
路径的错误指出缺失的上限。启用或禁用这样的模型不改变其可选性。

有效目录按来源补上缺失的上限：默认项和用户模型来自捆绑匹配，托管目录模型来自同一捆绑回退，
描述覆盖未自行声明的上限则来自其基础项。probe、refresh 与创建只保存声明的值；`models.reset`
让模型回到发现结果，发现结果没有声明时它重新变为未声明。

格式 v24 保存客户端给出的上限却不写标记，其探测也为每个缺失字段补上假定值（窗口 128000、输出
16384）。因此格式升级与两个旧版导入器都通过同一个函数，对用户模型、静态记录的描述覆盖与自建
Provider 自有模型的每项上限单独判定：已列入 `capabilityOverrideFields` 或由 `metadata.raw` 声明的
上限是声明；未标记且恰等于假定值的上限删除，因为它可能是探测填充；其余数值只可能来自输入或
清单，保留并补上标记。该规则宁可再次询问，也不保留可能的假定值：用户若恰好输入 128000 或
16384，升级后该项显示为未声明，用 `--set-missing-limits` 或 `/models` 重新声明即可。

这些写入者会写标记：`models.edit` 为它编辑的每项上限写；两个旧版导入器为导入时保留的每项值写，
描述覆盖同样写。`provider.create`、`provider.models.create` 与旧版探测都不写。静态记录的描述覆盖
按逐字段规则，因为两个旧版导入器把旧库的 `modelConfigOverrides` 原样复制、不写标记，其中的值是
用户输入；托管记录的描述覆盖按严格规则，因为它只由 `models.edit` 写入，而后者为它编辑的每项上限
写标记。

两个旧版导入器沿用同一划分：旧记录中 `bundled`/`remote` 行的 `models` 属于投影输出，按严格
规则处理；用户模型与该行的描述覆盖按逐字段规则处理。`modelsSource` 为 `user` 的行的 `models`
不是该行的输入，而是记录起步的种子（既有记录的 models，或没有目标时捆绑定义），导入保持它的
原样，不加标记也不删除；捆绑定义的种子在捆绑加载处就已按严格规则处理。

Session 保留已采用的能力快照。模型变为未声明的 Session 仍可恢复并保持可读；下一次请求在
解析器处失败，错误指出缺失的上限，且发生在凭据或网络 I/O 之前。两项都声明后再选择该模型
即采用新值；在此之前 `/usage` 报告已采用快照，与其他描述修改相同（§6）。

托管账号目录遵循同一规则，并像任何 Provider 一样识别其模型：记录的自身前缀与别名就是识别所读的
投影，因此目录叫不出名字的账号模型由别名所指行提供上限，只设置别名的编辑也不是描述覆盖。账号
模型条目带上与其他 Provider 条目相同的记录：`metadata.identification` 指出命名其行的规则、被拒时
的候选行，或没有任何步骤指名该 id 时为空；匹配的 Provider、模型、策略与分数；以及 `filled` 指出为
条目未自行声明的每项上限提供取值的行。该记录在每次读取时重新得出，因此清空别名或刷新清单都会
重算它，指名不了的声明既不留规则也不留过期的行。账号清单与任何覆盖自身声明的值仍然归它们：在
`capabilityOverrideFields` 中标记的上限永不被填也不被列出，模态、推理、价格与 `metadataSource`
保留账号自己的描述。中继
清单在记录存在之前就已投影，因此它只用捆绑目录能说的内容。中继与捆绑目录都未声明上限的模型按
缺失投影，并保留其 `unavailableReason`（托管目录投影）；其自有家族规则仍然声明窗口。宿主只在
模型带原因时接受缺失的上限。[模型文件配置](../../../../crates/dim-cli/README.md#api-key-configuration)
不变：它仍在加载时拒绝缺少任一项的条目并指出该条目
（[#517](https://github.com/arcships/dim/issues/517)）。

存储记录以缺省键表示缺失。[#522 格式决定](../../../design/appserver/native-storage-format.md#522-v26-格式决定)
是引入它的格式决定，§6 说明升级对既有记录做了什么：清单投影的模型在 `metadata.raw` 中保留它来自的清单项目，
因此该项目声明的上限即使存储条目没有标记也仍是声明，只删除没有任何来源声明的上限。

### 3.6 供进程 owner 读取的可选模型

按模型能力路由工作的进程 owner 不能自留一份它路由到的模型的可用性规则。下面的端口在每个
配置里有一个提供方——产品配置是 Provider，配置式模型配置（`dim-agent-aimux`）是它的模型文件
——各自提供无副作用的读取，返回它会服务的模型，以及路由决策需要的标识和已声明能力。本节
定义端口与 Provider 的规则；配置式配置的规则归它自己的页面。这次读取是进程 host 端口，不是
管理方法：不在 `provider` 命名空间上，不经 RPC 提供，也不改变 `provider.catalog`。

**为什么是端口而不是 `ProviderService` 上的方法。** 消费方插件在自己的 crate 中，
`xtask check` 拒绝依赖装配层 `dim-app-server`，因此消费方无法命名 `ProviderService`。
端口及其数据类型放在 Provider 与消费方都依赖的 `dim-agent-capabilities`；注册为进程名后，
消费方可以命名该端口并由 catalog 解析。依赖从扩展指向 core，符合
[ADR 002 D3](../../adr/002-plugin-scope.md)，实现、目录和规则仍留在各自的提供方。

**合同（语言中立）。** 进程 host 端口，无实例键：产品配置由 Provider 提供，配置式模型配置
（`--model-config`）由它的模型文件提供。操作 `selectableModels`，异步，请求 `{}`，回复
`{models: ModelCapabilitySnapshot[]}`。元素就是既有
`ModelCapabilitySnapshot`——`{target: {providerId, modelId}, capabilities:
ModelCapabilities, reasoning?, serviceTier?}`——不是新的模型类型：它已经携带 provider/model
标识与已声明能力，就是执行时采用的那份描述。可选模型一定声明了两项上限（§3.5），因此既有
`capability_snapshot` 转换不会失败。放弃调用会取消 owner 的本次读取；丢弃回复不表示读取成功。
错误使用下文列出的公开错误码。

**第二个操作：`selectableForModality`。** 同一端口还提供一次面向非对话用途的进程读取
（Modality §9.2）：操作 `selectableForModality`，异步，请求 `{}`，回复
`{models: ModalityModelSnapshot[]}`，元素为
`{target: {providerId, modelId}, operations: string[]}`。`operations` 是该模型目录条目在
`metadata.raw.modalityCapabilities` 中声明的 `<category>.<operation>` id，按条目中的顺序；
未声明任何操作的模型被省略。本次读取应用与 `selectableModels` 相同的 provider、账号、模型、
路由、凭据与状态规则，但去掉其 text modality、chat 类型与已声明文本上限三项要求：没有
`text` modality 的模型、非 chat 模型与 `limits_undeclared` 的模型只要声明了 modality 操作
仍是候选；不可用状态、停用的 Provider 或模型、缺少路由、已登出的账号与缺少静态凭据仍会排除
它们。它不携带原因字段：消费方从选项推导可用性，并用自己的快照解释不再可选的持久默认值。

**Provider 应用的规则。** 候选是按 Provider 启用状态、凭据配置和模型状态查询的持久模型目标。
每个候选按 Provider ID 和模型 ID 点查，账号可用性由账号拥有方判断。捆绑替换、descriptor
覆盖、用户模型、路由和禁用状态都按该目标的执行规则投影。模型只有在既有 `ProviderCatalog::selectable` 接受时才出现：
`unavailableReason` 缺失，且 ID 不在 `disabledModelIds` 中。该原因已覆盖全部排除项：
`ModelStatus::Unavailable` 的模型、未声明 `text` modality 的模型、在 `new-api-router`
之外的 driver 上的非 chat 模型、禁用的 Provider 记录、没有路由的模型、已登出的账号、
缺凭据的静态记录，以及未声明上限（`limits_undeclared`）。读取不增加第二个谓词：它读的
就是选择已经在用的那一个。modality 读取是上面同一条规则去掉三项要求后的样子，而两个目录
构建点都从对条目的一次分类得出会话原因与 modality 原因，因此新增的排除原因只需在一处
声明其类别，两条读取不会漂移。模型出现，意味着同一快照下 `select`、`configuration`、
`apply_preferences`、每请求检查和 `auxiliaryModel` 都会接受它。执行仍是权威：之后的写入可
使其不可选，消费方把 ID 交回 Provider，由后者重新检查。

**配置式配置应用的规则。** 它的候选是模型文件里的每个 provider 与模型条目，按 provider、再按
模型 id 排序。文件要求两个上限，因此每个条目都带这两项；文件不声明其它能力，条目按文本默认
能力给出。这个配置没有账号、路由、停用或偏好输入，读取也不加这类测试：文件的校验与它的规则
都归它自己的页面（[aimux](../../../crates/dim-agent-aimux/README.md)）。

这次读取不是 [#560](https://github.com/arcships/dim/issues/560) 的校验合同。后者的合同在
§3.7：它接收已准入的持久化静态记录和显式模型 ID，回答该选择能否执行；本次读取没有输入，
只枚举当前可用项。两者定义在同一目录和同一可选性谓词上，因此对同一个模型不会给出不同答案。

**快照与一致性。** 一次调用先枚举持久目标 ID，再分别点查。每个目标记录读取使用自己的
已提交快照；并发的 `provider.*` 写入可能对部分目标可见、对其他目标不可见，同一 Provider
内的模型也一样。读取前已删除的目标跳过。回复没有整体版本，不保留结果缓存；下次调用
读取当前状态。不发起实时请求：不 refresh、不列 provider 端点，也不写记录、选择或偏好；账号
状态沿用 `provider.catalog` 的同一读取。回复是派生数据，消费方不得持久化，也不得当作已
采用的快照。采用仍走模型选择，后者只解析一次配置。

**错误。** 公开数据面及其 Rust 绑定在调用方取消时返回 `cancelled`，Provider 卸载时返回
`owner_closed`，Provider 存储不可读时返回 `failed`。单个 provider 的记录或账号状态不可读时按默认解析的方式跳过；它不会遮蔽其他
provider，读取也不返回逐项错误列表。需要该 provider 诊断的调用方使用 `provider.list` 或
`provider.management`。

**消费方。** Modality 用这两次读取计算每个操作的选项和自动默认值。adapter 服务的类别，
候选是两次读取去重后的结果：先是执行可选的模型，后是 modality 读取的模型，各按其读取的
顺序；每个操作只保留已收集 adapter 为它服务的那些。因此被执行拒绝的媒体模型只要 adapter
服务它，就仍在选项中，不声明任何操作的可选模型也一样。没有 adapter 服务的操作，候选是
modality 读取中条目声明了该操作 id 的模型；`image.understand` 的候选是带 `vision` 的可选
模型。配置的模型不在其中时报告 "no adapter for the configured model"。它不调用
`provider.catalog`，也不自留可用性谓词。回复只带 ID 和能力，不带显示名：渲染选项
的客户端用 `provider.catalog` 或 `provider.management` 的模型列表按 ID join，Modality 不复制
名称。Provider 不增加逐消费方的键、配置或过滤器：读取没有输入，需要单个 provider 的调用方
自行筛选回复。Collaboration 的 `agent` 工具用同一次读取实现 `models` 动作，把其中的 ID 交给
leader 的模型，同样不自留规则（[Collaboration](../plugins/Collaboration.md#7-成员能运行的模型)）。

**不变的部分。** `provider.catalog`、`provider.management`、`provider.select`、模型选择、
采用、偏好及其错误；客户端协议与 `PROTOCOL_VERSION`；持久化及其格式；Timeline、
Dispatcher、before 回调、Session 与分支部件；driver、路由和非 chat 规则。读取不增加状态、
缓存、通知、crate 或模块。

**Rust 绑定。** 绑定直接调用类型化操作，不经过 JSON 或桥接任务。绑定接收
`CancellationToken`；取消它返回 `ModelPortError::Cancelled`。每次调用持有独立的子 token，
调用完成或被丢弃时取消该子 token，不取消调用方的 token。操作将 owner 的 `HostError`
映射为公开的 `ModelPortError`；dispatcher 调用同一个操作并编码回复或错误。

端口是 `crates/dim-agent-capabilities/src/host.rs` 中的
`SelectableModelsHost`，位于 `ModelSelectionHost` 旁。它的两个读取就是 §3.6 列出的操作：
端口类型持有 owner 的闭包，dispatcher 写出 wire 形状，Rust 读取方通过绑定调用同样的操作。
Provider 在 `ProviderPlugin::apply` 中用 `provide_selectable_models` 提供该端口；catalog 以
shared 名注册它（[服务面 §4.10](../../../design/service-faces.md#410-宿主端口executionsettingshost-等五个)）。

owner 实现是 `ProviderService::selectable_models` 与
`ProviderService::selectable_modality_models`，`ProductResolver` 转调两者；配置式模型配置
（`dim-agent-aimux`）用模型文件中的条目提供这两个读取；该配置不声明任何 modality 操作，
因此它的 `selectableForModality` 读取返回空
（[aimux](../../../crates/dim-agent-aimux/README.md)）。需要这次读取的消费方在 `Deps` 中声明
`Keyed<SelectableModelsHost>` 并以它作为加载门槛（Modality）；必须在没有该读取时也能加载的消费方
在提供时按名读取、不设门槛（Collaboration 的 `models` 动作），并在加载时取得快照，因此提供方
重载不会重载它。消费方不直接读取 `ProviderService`。非 Rust 消费方按名调用
`selectableModelsHost`，方法即 §3.6 列出的那些；该面委托同一次 owner 读取，两种语言的
消费方得到执行会选的同一批模型。

**连接合同。** Provider 为可信进程内适配器解析一次非对话连接。这是
`ProviderConnectionResolver` 旁的原生端口，不是管理方法或按名字提供的数据面。
输入是模型身份与操作，回复包含本次连接及其协议、模型和实时声明。解析后的连接含有凭据，
只在本次适配器调用期间留在进程内。

两个原生连接端口复用 owner 的记录读取、目录选择和凭据租约。一次 modality 调用只解析一次，
路由事实与凭据来自同一次解析；对话路径的临时启动连接与已采用快照不适用。
这个端口不另增 HTTP 执行器，媒体协议与传输行为仍由现有适配器负责。
跨语言操作契约由 [#1613](https://github.com/arcships/dim/issues/1613) 跟踪。

**规则。** 它应用 `selectableForModality` 读取的同一组规则——provider、账号、模型、路由、
凭据与状态——并要求模型目录条目在其 `metadata.raw.modalityCapabilities` 中声明 `operation`。
任何一条不满足的模型都在任何网络调用之前被拒绝：可用性类失败带执行对该模型报告的原因，
未声明的操作作为指明它的 invalid request 拒绝。读取与连接不会给出不同结论，因为二者共用
目录与其选择判定。托管账号的凭据租约读取同一条规则：只拒绝排除所有用途的原因，因此目录标记为
非 chat 的媒体模型仍会为这次调用发放租约。该边界规则同时读取存储的原因与条目状态：
`unavailable` 状态无论存储了何种原因都排除所有用途，因此旧投影为这类模型写下的记录不会被
任一读取列出，也不会发放任何租约。

**实时声明。** 提供实时操作的条目在 `metadata.raw.realtime: {protocol, path}` 中声明它所说的
传输。Provider 将其解析为 `RealtimeDeclaration`，并作为 `realtime` 随连接回复携带；未声明的
条目回复 `null`，配置式配置因为没有目录条目，对每个目标都回复 `null`。该操作仍与其余操作一样
声明在 `metadata.raw.modalityCapabilities` 中，因此一个列出 `realtime.voice` 却没有传输声明的
模型会解析出 `realtime` 为 `null` 的连接：Modality 在任何网络调用之前拒绝该模型的 `open`，
而不是猜测 URL。`protocol` 是适配器为该线上协议使用的名字，`path` 由该适配器追加到路由的
base URL 之后。

**凭据规则与生命周期。** 连接只活在那一次调用中。Provider 从不持久化它，从不把它返回给
客户端，也从不写进任务记录、结果、诊断或错误。它的 `Debug` 打印模型、路由、base URL、头名
以及是否存在凭据，从不打印头的值、bearer 或 key；`ModelConnection` 当前未实现 `Debug`，
因此没有其他连接会格式化凭据。托管连接保留供可信进程内调用方使用的恢复 handle，但现有媒体适配器
不调用托管凭据恢复；媒体拒绝会返回调用方，该端口不提供自动刷新与重试。
对话恢复路径独立，不构成非对话调用的重试保证。

**Rust binding。** `ModalityConnectionHost` 持有 `Arc<dyn ResolveModalityConnection>`。
`resolve_for_modality(target, cancel)` 返回 `ModalityConnection { resolved, protocol,
model, realtime }`；`resolved` 是 `ResolvedModelConnection`，适用时包含恢复 handle。
`ProviderPlugin::apply` 提供类型化端口；配置式模型配置实现同一个 trait，其条目未声明
modality 操作，因此逐操作拒绝。catalog 名字没有 dispatcher。
调用方将 `CancellationToken` 传入现有原生解析路径。

### 3.7 已存选择的离线校验

读取 Provider 持久化原生记录的数据边界消费方，需要在不启动 Provider、不发起实时请求的
情况下判断一个明确的 provider/model 选择能否执行。Migration 的 Provider 步骤是第一个这样的
消费方。Provider 对已准入的静态记录拥有该判断，并以一次无副作用投影公开它，而不是让消费方
重建路由、描述或可用性规则。合同接收原生记录，不导入旧格式行、迁移回执或任务状态，也不读取
存储、账号状态或网络。

**合同。** 一个同步的 Provider 模块函数，无 I/O、无取消：

| 部分 | 值 |
| --- | --- |
| 输入 | `providerId`；`parse_static` 或 `convert_legacy_provider` 返回的已准入 `StaticProviderRecord`；显式 `modelId`；调用方已加载的捆绑目录 |
| 回复 | `{protocol: ModelProtocol, snapshot: ModelCapabilitySnapshot}` |
| 错误 | 记录的凭据 authority 不是 `static` 时为 `account_owned`；有效目录没有该模型时为 `not_found`；模型存在但被既有 `ProviderCatalog::selectable` 拒绝时为 `unavailable {reason: string \| null, disabled: boolean, missingLimits: string[]}` |

回复就是执行时采用的有效描述：`snapshot` 是既有 `ModelCapabilitySnapshot`，其 `target`
携带 provider/model 标识，`protocol` 是有效路由。只需要“能/不能”的消费方忽略这些字段。本合同
不应用已保存的 `ModelPreferences`、不选择默认模型、也不创建路由标记；这些属于采用，仍由
`configuration` 解析。记录的前置条件由已准入类型保证：它的路由由 Provider 解析器按当前 driver
和 New API 规则派生，因此本函数不重新解析记录、不重新校验字段、也不重新派生路由，不增加第二条
准入路径。

**应用的规则。** 它构建与 `provider.catalog`、`catalog_for` 相同的单 provider 目录
（`static_catalog`），并且只通过既有 `ProviderCatalog::selectable` 接受模型。有效描述——捆绑
替换、descriptor 覆盖、用户模型和已声明上限——就是该目录投影出的那一份；既有原因覆盖全部
排除项：`ModelStatus::Unavailable`、没有 `text` modality、`new-api-router` 之外的 driver 上的
非 chat 模型、禁用的 Provider 记录、没有路由（不支持、缺失或歧义的 New API 端点）、不支持的
driver、缺少静态凭据，以及未声明上限。禁用模型 ID 来自目录的 `disabledModelIds`，不是另一份列表。
路由是目录条目的 `endpoint`，由准入时的 `drivers::routes`/`new_api::routes` 派生，不增加第二个
路由匹配器。`capability_snapshot` 生成已声明描述，因此本投影与执行在上限、reasoning 和
service tier 上不会给出不同答案。拒绝细节来自同一个目录条目，在 `selectable` 拒绝之后读取，
不参与接受与否的判断。

对于 `account_owned` 记录，合同在构建目录前拒绝。托管记录的当前目录、凭据和路由归其账号
owner，且不包含在持久化静态记录中；执行它需要该 owner 的读取，而不是这次离线判断。静态记录
才是本合同的输入；托管模型仍以账号 owner 的目录为准。

**与 §3.6 的关系。** 可选模型读取是无输入的进程 host 端口，枚举 Provider 当前会服务的模型
（包含托管账号目录），并为路由返回能力快照。本合同接收一条持久化静态记录和一个显式模型，
不读取账号或存储，返回路由与快照。两者共用 `static_catalog`、`ProviderCatalog::selectable`
和 `capability_snapshot`；对一条双方都能看到的静态 authority 已存记录，一个目标出现在 §3.6
回复中当且仅当本合同接受它。二者的区别在范围而不在规则：§3.6 回答“现在有哪些可用”，本合同
回答“这条已存选择能否执行”。二者都不是管理方法或 RPC。

**消费方与 Migration。** Migration 用 Provider 的解析器读取目标记录，并通过 Provider 冻结的
选择转换导入旧的 provider 选择。该转换是唯一的离线判断，目前使用 Provider 私有的目录谓词；
本合同是它的公开形式，转换调用本合同。Migration 保留自己的旧格式解码、转换、写入和回执，
不拥有路由匹配器、可用性谓词或目录构建。因此本合同服务任何原生记录消费方，不增加迁移专用
owner API、回调或 RPC。

**兼容性。** 本合同是建立在已准入类型上的 Rust 模块边界，不是 rutis 服务、RPC 或管理方法。
它不增加服务名、生成图条目、crate 依赖、持久化或 `PROTOCOL_VERSION` 变更。旧配置导入的结果和
拒绝文本不变，因为选择转换把任何拒绝映射为既有原因，接受的选择写入相同的 provider/model ID。
回复不包含凭据、header、代理或秘密引用；记录输入就是 Provider 解析器已持有的那一份。

**Rust 绑定。** 唯一绑定是 `dim_app_server::provider` 导出的 Provider 模块函数。没有跨语言
消费方：输入是已准入记录类型，携带凭据引用，因此不作为命名服务暴露。

```rust
pub struct StoredSelection {
    pub protocol: ModelProtocol,
    pub snapshot: ModelCapabilitySnapshot,
}

pub enum StoredSelectionError {
    AccountOwned,
    NotFound,
    Unavailable {
        reason: Option<String>,
        disabled: bool,
        missing_limits: Vec<String>,
    },
}

pub fn stored_selection(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
    model_id: &str,
) -> Result<StoredSelection, StoredSelectionError>;
```

### 3.8 适配器的媒体执行

Provider 为一次非对话调用解析模型的连接，Modality 在该调用中把连接交给适配器（[Modality A.2](../plugins/Modality.md#a2-适配器与-ocr)）。该连接不是公开合同，也不作为 data face 暴露。

**连接为何不能是面。** 它的返回值包含仅限这一次调用的凭据材料，`ResolvedModelConnection` 为一次最终请求解析且从不序列化（`dim-agent-capabilities` `host.rs`）。[服务面 §2.2](../../../design/service-faces.md#22-migration-marker) 要求秘密值留在所有者内部、公开方法暴露使用它们的操作，因此能跨语言的只有**操作**——代适配器执行的那次请求——而不是连接。

**决定：现在不提供该操作。** 该连接是 Provider 的私有 typed binding，不是公开服务：它返回的是一次调用的凭据材料，因此既不会成为 data face，也不作为目录中的名字保留。[服务面 §2.6](../../../design/service-faces.md#26-migration-rule) 已规定角色为 typed binding 的名字从目录移出。媒体适配器在进程内（`dim-modality-aimux`）经由这条原生路径执行。本决定不改变任何媒体操作、签名、重试、重定向、上传、下载或流式行为。

**为何 `modalityRequest` 执行器是另一份合同。** 它是新的 HTTP 执行器，而不是既有操作的序列化，无法保留原生路径今天的行为（[#1613](https://github.com/arcships/dim/issues/1613)）：

| 既有行为 | 操作必须做到 |
| --- | --- |
| 每次适配器调用一次解析 | 每次调用解析一次，并把这一次解析用于该调用的重试；用缓存回答后续调用会报出一个适配器从未解析过的配置，因此后续调用重新解析 |
| 流式下载到文件，2 GiB 上限（`dim-modality-aimux`） | 流式写入文件；不得把整个响应缓冲成 base64 |
| WebSocket realtime open/send/receive/close | 连接级协议，而不是一次请求/应答 |
| 厂商路由与凭据路由，包括 fal 图像路由与查询参数 | 与目录声明相同的路由选择与凭据处理 |
| 重定向与厂商错误映射 | 与当前连接查询相同的规则 |
| 每一步的取消与释放 | 取消所有者工作；丢弃调用即结算，且保留调用方的 token |

在 [#1399](https://github.com/arcships/dim/pull/1399) 较早修订中提出的 `modalityRequest` 还接受 Bedrock 路由却不做 SigV4 签名，而所有内置媒体适配器都拒绝 Bedrock 连接。Bedrock 上的媒体支持是新增特性，不是既有操作的序列化。

**若日后实现该操作的验收。** 实现前由维护者确认操作形状、所有权与覆盖面；既有媒体操作及其上限保持不变，或明确声明范围变更；每次调用生命周期内只解析一次且不混用快照；Rust 与真实外部运行时流程覆盖相同的请求、应答、取消与错误合同；测试覆盖受影响的流程边界，包括凭据隔离、厂商路由、查询参数、重定向、下载流与 realtime 清理；文件传输与 [#1564](https://github.com/arcships/dim/issues/1564) 及其进行中的改动保持一致，不另立上传实现。

**对使用方的影响。** Provider 没有暴露媒体执行操作，因此外部语言适配器不提供任何媒体操作。Modality 在自己的文档中说明这一点；本页不定义适配器如何声明或运行其工作。

## 4. 客户端集成

`provider.management` 只作用于单个 Provider；原先的无参聚合读取已移除。需要所有
Provider 的版本、连接与模型描述符的客户端，用 `provider.management.list` 分页读取
概览，并按需用 `provider.models.query` 读取要展示或编辑的模型；单个编辑表单使用精确的
`provider.management {providerId, modelId?}` 快照。这些读取都是有界分页或一个显式目标，
服务器不保留其结果，也不保留解析后的目录。从 `provider.management {}` 迁移时，提供方
列表与保存版本读取改为 `provider.management.list`，模型能力与上限读取改为
`provider.models.query` 或精确的 management 快照。

```text
Desktop / CLI             Local App Server            Provider / account
    | hello: actual methods      |                            |
    | management / selection --->|---- safe snapshot --------->|
    | edit unsaved draft         |                            |
    | probe -------------------->|---- validated I/O --------->|
    | create(UUID) / CAS patch -->|---- atomic owner write ---->|
    | reread after own write --->|                            |
    | explicit target + saved preferences -> existing Session adoption
    | reopen / reconnect: reread |                            |
```

使用同一 product home 的本地 socket/pipe，配对设备不因此取得管理权。CAS 冲突保留草稿，
重读后由用户确认，不自动用新 version 覆盖。坏条目不清空列表；旧视图/草稿回复不覆盖当前
编辑。其他客户端修改下次读取可见，不依赖 watch/replay。provider.select 只改 scoped 默认，
当前 Session 另行显式采用，分别报告结果并提供准确目标。

CLI 同步迁移 crates/dim-cli/src/session_host.rs 的 PROVIDER_METHODS、全部 save 调用者及
真实 daemon 流程：Add 先 probe 未保存连接再 create，保留 UUID 供丢回复后恢复，不能先保存
再 refresh 测试；编辑按需 probe 草稿，然后只 update 改动字段，记录自身的模型由该 update
用新连接列举并保存；Disconnect 原子提交
credential clear 和 enabled:false，修复只 clear 导致已启用记录缺凭据被拒绝的问题。
启停用 update，账号登录/登出仍走原 owner。
重复创建时若 UUID 已存在，转为该记录的编辑表单并保留 URL/密钥草稿，由用户再次 Apply 确认，
不直接报告草稿已保存。只有空 headers:[] 或 network:{} 的更新拒绝，不增加记录版本。

/models 对当前选中模型提供 Ctrl+X，按列表读取时的版本 CAS 清除保存的 reasoningEffort
与 serviceTier；列表保持打开，Enter 另行显式采用默认值。冲突则刷新列表，由用户重试。
刷新后的列表先于操作结果提示送达选择器，用户看到提示后重试时使用刷新后的目录版本。
非交互入口为 `dimcode models PROVIDER --clear-preferences MODEL`，先重读版本再同样更新。
两者都不改当前 Session 已采用的模型；发现和采用失败不会自动删除历史偏好。
`←` 与 `→` 在选中模型上循环其声明的推理强度。Enter 采用该模型，并写入该文件夹中
下一个 Session 读取的默认值：该文件夹已有工作区选择时写工作区作用域，否则写全局作用域，
`expectedVersion` 取自同一作用域。写工作区时确认行写明该文件夹，写全局时确认行不提文件夹。
若强度被切换过，
先把它存成该模型的 `reasoningEffort`，再连同其余已保存偏好一起发送。编辑提供方的模型列表属于
Models 管理领域（[CLI 命令 § Models 领域](../cli-commands.md#models-领域)）：`/models` 的
`Manage models` 与 `dimcode models PROVIDER add|edit|set|remove|inspect` 读取
`provider.management`，并经 `provider.models.*` 写入；上面的选择器动作保持不变。

catalog 条目带 `zdr: true` 的模型在推理强度前显示 `ZDR` 标签，固定子代理模型选择器也一样。
搜索 `zdr` 会包含这些模型；字段缺失或为 false 时不显示标签。替换模型列表时，
标签随服务端最新字段更新，不保留已撤销的标记。

模型列表通过 `provider.models.query` 按页读取和服务端搜索。远程目录刷新由 Provider 或
账号管理面板中的显式操作发起；模型选择器关闭时释放结果，重新打开时重新查询。

未声明的模型不是选择器行。选择器的 `Model management` 分区位于滚动的候选列表之外，保留紧凑的诊断：
多少模型缺少上限、多少不可用或已禁用、多少 Provider 没有模型、多少目录所有者无法读取
（[CLI 命令 § Models 领域](../cli-commands.md#models-领域)）。Enter 打开 Provider 列表，每行写明
Provider 及其状态；至少有一个模型缺少上限的 Provider 写明数量，footer 给出声明命令
（`dimcode models PROVIDER --set-missing-limits ...`）。没有任何可选模型的 Provider 仍可在该列表中
找到，并写明原因，包括 `no models` 与 `catalog unreadable`。选择 Provider 打开既有的 Models
管理领域，因此诊断从不成为模型候选。守护进程未声明 `provider.management` 时，同样的原因及其
恢复命令以只读行显示，没有可执行的管理操作。`dimcode models PROVIDER --set-limits MODEL --context-window N
--max-output-tokens N` 通过 `models.edit` 为单个模型声明两项上限；
`--set-missing-limits --context-window N --max-output-tokens N` 为列表标为 `undeclared`
的每个模型声明，即缺失上限是唯一障碍的启用模型；被禁用或因其他原因不可用的模型不写入。
两种形式都必须同时给出两个具名值。两者都不能与
`--refresh` 或 `--clear-preferences` 组合。两者都先读取记录版本，串接每次 `models.edit`
回复的版本，然后像 `models PROVIDER` 一样打印目录。列表把上限未声明的模型标为
`undeclared`，并在行下列出声明命令。

```text
Models
  Conversation model
  > Provider / model          ZDR  ‹ high ›
  Selected: Provider / model
Enter select + default · ←/→ effort · Tab section · Esc clear/close
```

```text
Providers                         Provider detail
[ Search... ] [+ Add]             Name / source / connection status
  Builtin A       Connected       [Edit] [Disable] [Disconnect]
  Custom B        Disabled        Credential: Stored [Keep | Replace | Clear]
  Account C       Signed out      Models [Search...] [Add] [Refresh]
  Broken D        Error           Model / Enabled / [Edit] [Reset/Delete]
                                  Account usage: result or actual query error
                                  Draft [Test connection] [Cancel] [Save]
                                  Conflict: reread / retain draft
```

复用既有布局/组件；CLI 提供等价操作，不照搬 Desktop 布局或按硬编码名称猜能力。

## 5. 向机制 owner 提出的需求

None。现有具名管理调用、CAS、Provider 存储、metadata 与生命周期可满足本方案。
不要求 core、Rpc、rutis、装配或账号 host API 扩展。S4 已实现。客户端通知若需要，与
Settings 一起另行决定，不作为本方案前置。

## 6. 持久化、已运行 Session 与迁移

[v19 决定](../../../design/appserver/native-storage-format.md#329-格式决定)引入的以下逻辑字段
承载用户配置，缺失表示无对应用户数据；原生表示现遵循
[v52](../../../design/appserver/native-storage-format.md#v52-sparse-provider-state)。

| 结构 / JSON 字段 | 值与用途 |
| --- | --- |
| StaticProviderRecord.user_models / userModels | ModelEntry[] 用户新增；models 保持默认，modelConfigOverrides 只保存覆盖 |
| StaticProviderRecord.model_preferences / modelPreferences | model ID 到既有 ModelPreferences 的映射 |
| ProviderRecord.model_preferences / modelPreferences | 托管目录同样保存偏好映射 |
| StaticProviderRecord.endpoint_overrides / endpointOverrides | model ID 到既有 NewApiEndpoint；此处可执行覆盖仅限 openai、openai-response、anthropic、gemini，与发现的 endpointTypes 分开 |

[存储合同](../../../design/appserver/native-storage-format.md#329-格式决定)记录格式决定及
降级损失。不往 ModelMetadata、ModelNetwork、StaticCredential 或闭合 endpoint enum 加字段/变体，
旧 v19 读取会拒绝未知嵌套字段。ModelPreferences 同样拒绝未知字段，NewApiEndpoint 保持闭合；
支持这些新顶层映射的二进制会拒绝其内部后续增加的偏好字段或端点值，增加前须另作格式决定。
来源由默认/覆盖/userModels 列表归属推导，不额外加 tag。
有效静态目录与 driver 路由都纳入 defaults + userModels，descriptor 覆盖只作用于自有默认项，
路由推导前应用 endpointOverrides。refresh、普通写入、bundled reseed 均保留新增字段；
托管目录的偏好仅在同账号 refresh 时保留（§3.2）。
用户 ID 不能与默认/覆盖 ID 冲突；refresh 引入冲突则失败，不替换已准入目录。
列举新端点的 update 遵循同一规则：失败，点名冲突的用户模型，不写入任何内容。
解决时通过 `provider.models.delete` 显式删除冲突的用户模型，再重试 refresh 或该 update。

源接纳遵循 §3.1 的索引源合同。冲突沿既有逐项错误提示，保留已准入基线。
[v52 原生格式](../../../design/appserver/native-storage-format.md#v52-sparse-provider-state)
替代 v51 的全量投影存储，不重新解释已提交的旧格式。

两项模型上限另有自己的格式决定：[#522 格式决定](../../../design/appserver/native-storage-format.md#522-v26-格式决定)
把未声明的上限变为缺省键（上一格式的读取器两种取值都解不了），并在启动事务内原地升级既有记录，
而不是沿用版本号。该决定写明本次改动引入的格式版本、启动事务执行的改写，以及旧二进制的启动拒绝；
§3.5 说明有效目录随后如何按来源补齐声明值。

v19 的历史决定接受旧 v19 二进制重写时丢失当时新增顶层字段，包括自动
seed_bundled 重写。这不允许旧 daemon 打开 v52 存储。未知数据库格式仍按原规则
拒绝并保留字节。

| 改动 | 对已采用 Session 的影响 |
| --- | --- |
| 凭据、base URL、headers、代理、超时 | 下次请求解析当前连接；已发 I/O 保持其准入连接 |
| 模型 descriptor/capability、持久偏好 | 已采用快照不变，下次显式采用生效；保存的偏好届时由服务端应用，请求声明了则以声明为准 |
| 禁用/删除 Provider 或模型 | 下次请求/采用返回不可用/不存在，不强制切换或跨写引用 |
| 声明上限，或移除其来源 | 已采用 Session 保留快照。出现未声明的上限时，下一次请求在凭据或网络 I/O 之前失败并指出缺失项；两项都声明后，重新选择该模型即采用（§3.5） |
| refresh 或 endpoint override 改变有效路由 | 已采用路由标记不匹配时失败，提示重新选择；无标记 Session 在下次请求使用当前路由，两种修改均可能破坏协议相关历史，或丢失按旧协议映射的 effort/tier 选项 |

静态 New API 选择时，Provider 在现有 ModelSelection.metadata.extensions["providerProtocol"]
写入选中的 ModelProtocol 字符串。Prompt/请求准备已有 metadata 传递路径，不新增字段。
存在标记时，Provider 在解析凭据、网络 I/O 前比较当前路由。缺少标记就不检查：
升级前已采用的 Session 在路由未变时继续工作；refresh 和 endpoint override 均保留上述
无标记路由改变风险。无目标 F3 参数更新保留原标记
或其缺失状态，仅在存在标记时先检查路由再映射选项。ProviderService::configuration
解析的每次新选择都建立标记，包括显式目标、Agents profile 模型和默认解析；无目标 F3 apply_preferences 与 Collaboration inherit 都保留原标记或其缺失状态。
inherit 复制已经采用的选择，不在创建时重新解析。标记存在但不匹配时，在成员首次请求的
凭据解析/网络 I/O 前失败；旧配置缺少标记时保留上述未校验路由风险。只复制 Provider
拥有的选择 metadata；成员请求的 Session/run/trace/turn 标识由其自己的生产者提供。
这是现有 metadata 扩展中的 Provider 自有数据，不加 core 字段、epoch 或注册表。
托管账号原有 route/lease 校验保持。

旧 TS 配置继续显式只读导入，逐条 imported/already_imported/rejected；冻结映射由
Provider 以单次纯转换持有：输入旧行字段、既有目标记录与捆绑目录，输出原生记录与省略
的源字段，或拒绝原因。provider.import_legacy 与 Migration 的 Provider 步骤都调用它；
Migration 保留自己的快照、回执、账本与报告。明确映射连接、默认、用户覆盖/新增、禁用、
偏好及 scoped 选择。activeModelId 仅作提示，不覆盖已有选择。无原生对应物的字段丢弃并
报告为省略，该项照常导入：非空静态用量设置、retryConfig、capabilityDefaults，以及映射
之外的 metadata 键。未知描述字段与启用/禁用列表冲突会改变模型本身，仍拒绝该项。不覆盖
已有目标、不复制 OAuth grants、不因重复导入复活删除项。草稿、模板、用量回复、已解析
凭据不持久化。资源取得即登记清理；关准入、取消自有 I/O、释放租约并结算已接受调用后
移除服务。

冻结的旧库结构通过 `modelsSource` 区分模型来源：

| 旧字段 | 原生记录 |
| --- | --- |
| `modelsSource: bundled/remote`、`models` | 默认描述；只有 `remote` 设置 `remoteModels` |
| `modelsSource: user`、`models` | `userModels`；已有内置项保留原生捆绑默认，ID 冲突则拒绝该项 |
| `modelConfigOverrides` | 匹配用户模型的描述替换该用户项；其余保留为描述覆盖，已消失的默认也不会变成用户模型；其中每项上限保留取值并补上标记（§3.5） |
| `metadata.enabledModelIds/disabledModelIds` | 默认和用户模型的显式禁用；列表冲突则拒绝 |
| `metadata.modelPreferences` | 既有严格 `ModelPreferences` 字典 |
| `activeModelId` | 单个 Provider 的提示，无权创建或覆盖 scoped 选择 |
| `provider_selections` | 沿既有目标校验和导入账本导入明确的 scoped 选择 |

旧行 source 为 builtin 时映射到该 ID 的内置项。没有目标时导入从该内置项的捆绑定义
开始，与在已播种目标上的导入完全相同，因此 ID 没有捆绑定义的 builtin 行会被拒绝；已有
目标必须是未改动的播种记录。转换后的记录写的是定义的名称，而不是源行的名称：内置项的
名称是用户不设置的目录数据，所以目录改名之前写下的源行仍能导入，并带上当前名称。它写的
driver 与 auth 必须一致，因为这两项决定记录到达哪里。custom 行的 ID 属于捆绑目录时拒绝。
两个入口用同一解析器解析并校验既有目标记录，校验不通过则拒绝该项。Migration 在转换之前把
不是未改动播种记录的目标归为 Conflict 且不带原因；其余情况下转换的拒绝原因同时到达两个
入口。

provider_selections 同样共用一次转换：工作区作用域是已存在目录的规范路径，所选模型
必须在 Provider 目录中对该记录可用。目录已不存在的选择按该原因拒绝，它本就无法匹配。
较早导入过的 Provider 行仍可供其选择使用：两个入口都从导入账本读取该记录，已存记录
无法读取时选择按该读取错误拒绝。provider.import_legacy 的回复在已导入项上增加 omitted
（转换丢弃的源字段名），导入的选择项不带该字段；该字段为增量，协议 v3 不变。

不支持的用量/retry/capability 设置为 null 或空对象/数组时不携带配置，也不报告。
源文件字节不变。

## 7. 验收与交付

| 门禁 | 必须提供的可观察证据 |
| --- | --- |
| 合同 | 类型化 DTO、CLI 迁移、显式偏好目标、路由处理和冻结导入映射 |
| 秘密/patch | 单 header 更新不覆盖其他项，改超时保留代理认证，keep/set/clear/ref 及 authority，RPC/日志/错误无秘密 |
| 模型/路由 | 内置/自定义用户模型真实调用；create 的 probe 默认项，以及随后新增/编辑/启停/删除、refresh 后保留且不成为默认覆盖的手动/模板项；仅改端点时未有 descriptor 覆盖的默认项仍为 source:"default"，descriptor 继续随 refresh 更新；已有 descriptor 覆盖不变；reset 保留路由覆盖，clear 保留 descriptor 覆盖；显式/profile/默认选择均建标记，无目标 F3 与 Collaboration inherit 保留缺失；inherit 创建时不重新解析，首次请求校验已有标记；refresh 和 endpoint override 改路由时拒绝不匹配标记，重选后 effort/tier 正确；无标记 Session 在路由未变时继续工作，覆盖两种改变路由风险；显式删除用户模型解决 refresh 冲突 |
| Probe | 未保存探测、合并当前记录且不检查版本、换 host 使用原凭据前校验，失败/超时/卸载无写入，过期回复丢弃，不宣称客户端可取消 |
| 偏好 | 独立 set/clear、重启，空 patch/不支持值为业务错误，格式错误为边界错误；参数与准确目标一起采用，不可用不回退，无目标 F3 和消费方选择不变 |
| 模型上限 | 描述未声明上限的探测模型带 missingLimits 列出，被选择、采用和偏好拒绝；CLI 声明两项后变为可选，已采用快照带上声明的窗口；捆绑目录匹配保持已声明；托管模型缺上限时保留原因而不是替代窗口；模型文件配置仍拒绝缺任一项的条目 |
| 可选读取 | 同一快照下，读取结果与 `select`/`configuration` 对同一批 provider 接受的集合一致；禁用模型、禁用 Provider、不可用模型、未声明 `text` modality、其他 driver 上的非 chat 模型、无路由模型和已登出账号不出现，而其 Provider 仍可应答；每个返回项的 `capabilities.modalities` 与目录模型的声明集合一致；缺上限的模型不出现，读取不因此报错；托管与静态 provider 都出现；mock 端点未收到请求；写入改变下一次读取而不改变更早的回复；取消传入 token 与 Provider 关闭使用既有错误 |
| 已存选择 | 对同一条已准入静态记录和捆绑目录，`stored_selection` 接受的模型 ID 与 `select`/`configuration` 完全一致，并返回相同协议和已采用能力；固定 driver 以及 New API 显式、推断、不支持、歧义路由；缺失、禁用、不可用、非文本、禁用 Provider、缺凭据和未声明上限的模型均被拒绝；`account_owned` 记录被拒绝；mock 端点未收到请求；Migration 的 provider/选择流程和 `provider.import_legacy` 经共享合同给出相同结果 |
| 账号 | 既有登录/登出/用量；托管目录同账号 refresh 保留偏好、descriptor 覆盖和禁用项，换账号则一起清空；方法已公布但包缺实现的实际错误；Usage 不变，无静态用量配置 |
| 并发/恢复 | 新旧写方法双客户端类型化冲突；丢 create 回复按规范 UUID 恢复；拒绝非规范 create ID，其他方法仍支持已有非 UUID ID；重复 create 冲突；坏条目/草稿保留 |
| 存储/导入 | 旧 v19 读取并重写新记录含 reseed；bundled 升级与内置 Provider 的用户新增模型冲突时 Provider 激活，该记录/定义/fingerprint/version 不变，独立接纳的其他记录更新，管理返回冲突；显式删除后保留需重启提示，下次激活后访问再采用新定义；删除禁用用户模型时清理遗留禁用项，新内置项为启用状态；用已消失默认模型的 ID 新建用户模型时，新项启用且不继承原默认项的 descriptor 覆盖、偏好或端点覆盖；有效/非法/重复导入及源保留，均用一次性数据库 |
| 客户端 | 实际源码 App Server 和 Desktop/CLI：草稿→probe→创建→选模型→调用→修改→调用→重启→断开/删除，不只 fake daemon |

Provider、transport、托管账号与源码构建的原生 CLI 流程使用隔离 home 和真实 HTTP 请求
验证这些合同。原生 CLI 覆盖已保存偏好的显式采用、替换凭据、daemon 重启和断开/删除；
RPC 流程覆盖 CAS 与丢失 create 回复后的恢复。Desktop UI 验收归该客户端。
旧 v19 的读取、写回和自动 reseed 已使用已接受基线构建的二进制与一次性数据库验证，
见[存储合同](../../../design/appserver/native-storage-format.md#329-格式决定)。

## 8. 规则检查

| 规则 | 应用 |
| --- | --- |
| §4.1 | Provider 只写自有进程状态，Session 采用沿既有 owner 命令 |
| §4.2 | 断线不撤回已接受工作或已提交写入 |
| §4.3 | 不加 before；边界/业务校验分工，网络不占提交锁 |
| §4.4 | core/Rpc/Dispatcher 不加 Provider 分支 |
| §4.5 | 自有模型/选择/偏好/路由同事务；既有采用原子性不变 |
| §4.6 | 管理数据不进模型上下文，配置只沿现有采用路径 |
| §4.7 | 账号沿公开方法，Modality 经 §3.6 的 core host 端口读取可选模型，Migration 使用 §3.7 由 Provider 拥有的判断，不跨写其他 owner 引用 |
| §4.8 | 下述错误数据需求使用方法 owner 的通用字段；§5 复用 metadata 和 v19 顶层 JSON；§3.6 的读取是数据面（[服务面 §4.10](../../../design/service-faces.md#410-宿主端口executionsettingshost-等五个)），非 Rust 消费方按名调用；§3.7 不增加需求 |
| §4.9 | 不加 Timeline 事件或进程推送通知 |
| §1 | 扩展现有进程 Provider，不增 Capability/接线插件 |
| ADR 002 D1 | 进程记录归 Provider，已采用分支状态归原 owner，草稿归客户端 |
| ADR 002 D2 | 复用 namespace/resolver，不增管理插件或注册表 |
| ADR 002 D3 | 可选账号失败只影响自身能力，不加 core 依赖；§3.7 是 Provider 模块函数 |
| ADR 002 D4 | 业务配置归 owner，修改不 reload |
| ADR 002 D5 | §3 明确字段、方法、mutating/interruption、错误，不只写 Rust trait；§3.6 以中立形式给出进程读取，core host 端口是消费方声明的绑定；§3.7 是 Rust-only 模块边界，因为输入是 Provider 已准入且携带凭据引用的记录，且没有跨语言消费方 |

### 向机制所有者提出的需求

- **问题：** §3.4 的恢复合同需要结构化错误数据，固定代码和安全消息无法携带版本和公开身份。
- **既有机制：** 解析消息丢失类型合同，另行读取也无法分类被拒绝写入当时观察到的状态。
- **通用能力：** 原生方法提供安全的 owner 自有错误字段，由[方法错误合同](../../../design/contracts/session-boundary.md#appendix-c-errors-and-recovery)保留，Rpc 不解释 Provider 字段。
- **顺序影响：** 不影响序号、批次、中断或重载；被拒绝的写入不修改记录。
- **owner/issue：** dim-agent-capabilities 方法错误与 App Server；[#1495](https://github.com/arcships/dim/issues/1495)。

## 下游按范围读取

`provider.management` 的单 Provider 模型管理读取可携带
`page: {after?, limit?, query?}`，不能同时传 `modelId`。每页默认 32 行，
上限 128，至少 1 行。按模型 ID 字典序排列，`after` 为上一页最后的模型 ID，
回复 `next:null` 表示结束。搜索分别匹配模型 ID、显示名，忽略大小写。
禁用和不可用模型也展示。每页的模型、偏好和 Provider 版本来自同一已提交快照；
不同页可看到不同版本。只解码选中行及计算名称所需的数据。

Web 模型管理页打开时读取第一页，搜索交给服务端，后续页按需加载。
单模型修改后按 `modelId` 回读并替换该行，删除后只移除该行。
有搜索条件时，修改后改为重读该查询的有界第一页，由服务端决定修改后的行是否仍匹配。
返回模型页签时，从无过滤条件的第一页开始，与空搜索框保持一致。
打开已有模型编辑时，点查该模型的内容、偏好和版本。
版本冲突后需重新读取，才能再次写入。

`managementMethods` 上的 `provider.model.set_enabled` 接收
`{providerId, expectedVersion, modelId, enabled}`，返回 `{version}`。
拥有方核对版本、托管账号身份和目标可用性，在同一事务内只修改该模型的启停状态
并递增版本。目标不存在返回 `provider_not_found`，不可用目标拒绝切换。
不得根据当前显示页推导其他页模型的启停状态。

`provider.models.query` 的 `selectableOnly:true` 只返回拥有方现有执行规则
认可的模型。游标按已检查候选推进，过滤后空页也可能携带 `next`。
ACP 读取这些有界页，组成协议需要的完整可用模型回复；跨 Provider 重复的
模型 ID 仍因歧义而排除。CLI 单模型限制修改及修改后的展示都传 `modelId`；
批量补齐限制按其缺失条件枚举目标。

## 9. 消融检查

管理读取按范围提供编辑快照；已知目标的能力读取指定 Provider 和模型 ID，模型选择器使用
`provider.models.query`。create/update/probe 分别承担可恢复
identity、保留秘密的局部写和未保存网络探测；Disconnect 复用 update。独立用户模型列表
用于区分默认覆盖并让新增项可执行；路由标记沿已有 metadata 保留采用时协议含义。
声明的上限不需要新状态：目录已有的标记与匹配说明值是否存在，缺失由目录类型承载，原因在投影处
推导，因此不增加平行模型列表、第二份目录或 `limits` 实体。
可选模型读取不增加状态、缓存、事件或模型类型：它把既有 `ProviderCatalog::selectable`
应用到每个目标的点查目录，并返回既有 `ModelCapabilitySnapshot`，因此没有第二个可用性
谓词或平行目录。
已存选择合同同样不增加状态、存储或账号读取、缓存或谓词：它构建既有目录、应用既有谓词，
并返回既有已采用快照和执行已经派生的路由。需要一个命名的回复类型，因为路由与快照就是
公开形状；返回整个目录或客户端 DTO 会暴露更多，且仍需同一判断。Migration 侧不增加谓词或
目录构建，选择转换调用本合同，而不是保留一份只服务旧格式的私有判断。
不需要通知、跨删除版本服务、模板实体、凭据抽象或第二份客户端配置。

稀疏表示不需要重复头部投影、模型重写触发器、驻留解码目录或第二解析器。
源归档让已接纳基线无需旧二进制即可读取；按键保存的可变行独立保留用户状态。
命名依赖索引保存既有冲突规则所需的输入，不另存发布名称。摘要回复复用既有管理数据面
与版本合同。这些结构承担必要的持久化和读取边界，无需额外层次。
