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
| Provider host 端口 `SelectableModelsHost`（core `dim-agent-capabilities`），由 Provider 提供 | 按模型能力路由的进程内 owner（Modality）；§3.6 增加一次读取，不是管理方法或 RPC 方法 |
| `account`、`account.codex`、`account.grok` | 复用账号调用，不新增账号方法、事件或 managed host capability |
| `ProviderConnectionResolver`、`ModelSelectionHost` | Provider 沿现有类型处理目录和路由，不增加 core 端口 |
| `auxiliaryModel.complete` | S4 由调用者显式选模型的合同不变 |
| 原生 CLI Provider 流程 | 能力发现、创建、更新、断开随 owner 方法一起迁移 |

业务留在 Provider，不进入 `connection.rs`、`protocol.rs` 或接线插件。不增加 Session
Capability、通知流、目录副本、凭据服务或注册表。继续遵守
[managed provider boundary](../../../architecture/development.md#managed-provider-boundary)，
私有账号实现和凭据不进入本仓库。

## 3. 管理合同

### 3.1 读取模型

| 方法 | 请求 | 回复 | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.management` | `{providerId?}` | `{drivers: ManagementDriver[], providers: ManagementProvider[], errors: ProviderRecordError[]}` | false | `Drain` |
| `provider.list` | `{}` | 原 `{providers,errors}` 摘要；每项新增 `remoteModelList`，表示 `provider.refresh` 能否列举该 Provider 的模型，客户端据此在空 catalog 的提示行给出或省略该命令 | false | `Drain` |
| `provider.selection` | `{cwd?}` | 既有 `{selection}`，其 scope/version 独立 | false | `Drain` |
| `provider.catalog` | `{providerId}` | 既有 `{catalog}` 和 `ProviderModel`，**不修改** | false | `Drain` |

省略 ID 返回全部已知条目，包含未登录账号；指定 ID 返回该项或明确错误。
管理读取独自提供编辑视图：version、模型 descriptor、管理说明来自同一 Provider 记录快照，
不能把另一次 `provider.catalog` 的目录与该 version 拼接后写入。选择和只读 web 客户端
继续使用原 catalog。不承诺账号 owner 之间的原子快照。

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
| `models` | `{model:ModelEntry,source:"default"\|"override"\|"user",enabled:boolean,endpoint:ModelProtocol\|null,unavailableReason:string\|null,missingLimits:("contextWindow"\|"maxOutputTokens")[],actions:string[],endpointOverride?:NewApiEndpoint}[]`；路由覆盖限 §6 可执行子集。仅账号无本地目录记录时为 `null`，坏目录报错。`missingLimits` 指出没有任何来源声明的上限（§3.5），否则为空。模型操作为 `set_enabled`、`edit`、`reset`、`delete`，按来源/authority 限制；`set_enabled` 只出现在没有 `unavailableReason` 的条目上，因为集合会保留不可用条目的启用状态 |
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
直到启动采用新定义。seed 和管理读取共用一个 Provider 自有候选构建函数，包含远程模型
替换规则，避免候选不一致。不持久化另一份状态，也不禁用条目。
actions 只说明当前可用性，
不是授权或后续写成功的承诺。模型来源由 §6 的独立持久列表推导；默认项消失后，其覆盖
不能变成用户新增。
账号退出或切换后，已有目录仍保留版本和描述，但模型标为不可用；刷新到当前账号前不提供模型写操作。
`source:"override"` 表示有 descriptor 覆盖的默认模型；编辑用户模型后仍为 `source:"user"`。

### 3.2 写入与字段操作

下表的 `ManagementProvider` 均为 §3.1 的编辑快照。已有方法除明确新增
`ModelEdit.endpoint` 和 §3.4 错误分类外，保持请求/回复；已有写方法继续
`mutating:true`、`interruption:Drain`。

| 方法 | 请求 | 回复与语义 | mutating | interruption |
| --- | --- | --- | --- | --- |
| `provider.create` | `{providerId,displayName,connection,models:ModelEntry[],enabled}` | `ManagementProvider`；只创建，客户端生成新 UUID，已存在 ID 冲突 | true | `Drain` |
| `provider.update` | `{providerId,expectedVersion,patch}` | `ManagementProvider`；校验后单事务局部写入；patch 改动 `baseUrl`、`credential` 或 `headers` 时，同一次 CAS 写入还用新 endpoint 的列举结果替换记录自身的模型，`modelsListed` 说明结果；只改 `network` 时单独保存 | true | `Drain` |
| `provider.probe` | `{providerId,connection}` | `{models:ModelEntry[]}`；只做 §3.3 网络探测，不写入或检查版本 | false | `Drain` |
| `provider.models.create` | `{providerId,expectedVersion,model:ModelEntry}` | 既有 catalog；写独立用户新增列表，与默认/用户项重复的 ID 拒绝；以启用状态新增，同事务清掉该 ID 遗留的 modelConfigOverrides、disabledModelIds、modelPreferences 和 endpointOverrides 项 | true | `Drain` |
| `provider.models.delete` | `{providerId,expectedVersion,modelId}` | 既有 catalog；仅删用户新增项，同事务移除该 ID 的 disabledModelIds 项并清理自有偏好/路由/选择 | true | `Drain` |
| `provider.preferences.update` | `{providerId,expectedVersion,modelId,patch}` | `ManagementProvider`；effort/tier 分别 set/clear，按模型校验 | true | `Drain` |

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
models.reset 恢复仍存在的默认项，用户项用 delete。refresh 保留用户模型、覆盖、偏好和
显式禁用，沿用 admitted 配置冲突检查。托管目录换账号 refresh 时，偏好随 descriptor 覆盖
和禁用项一起清空；同账号 refresh 则保留它们。托管账号目录不通过这些方法新增用户模型/路由。
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
服务端不自动应用保存偏好；显式目标省略偏好仍从目录默认开始，包括模型声明的默认推理档位和
服务层级；无目标 F3 只改已采用模型的
指定参数。`/fast` 仍只修改当前 Session；保存 serviceTier 是持久 fast/tier 选择，但只有客户端
显式传入才生效。其他消费模型不自动继承聊天偏好。

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
provider_failed。按 owner 类型错误分类，不解析 message。已有 delete、models.edit/reset/
set_enabled、select、refresh 的不存在、CAS/admitted-refresh 冲突、校验错误也分别映射这些
类型；请求/回复和 JSON-RPC code 不变。未知方法和错误 RPC 参数保持通用边界错误。
秘密、secret reference、header 值、代理认证和上游正文不进入错误、日志或诊断。

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

托管账号目录遵循同一规则：中继与捆绑目录都未声明上限的模型按缺失投影，并保留其
`unavailableReason`（托管目录投影）；其自有家族规则仍然声明窗口。宿主只在模型带原因时接受
缺失的上限。[模型文件配置](../../../../crates/dim-cli/README.md#api-key-configuration)
不变：它仍在加载时拒绝缺少任一项的条目并指出该条目
（[#517](https://github.com/arcships/dim/issues/517)）。

存储记录以缺省键表示缺失。[#522 格式决定](../../../design/appserver/native-storage-format.md#522-v26-格式决定)
是引入它的格式决定，§6 说明升级对既有记录做了什么：清单投影的模型在 `metadata.raw` 中保留它来自的清单项目，
因此该项目声明的上限即使存储条目没有标记也仍是声明，只删除没有任何来源声明的上限。

### 3.6 供进程 owner 读取的可选模型

按模型能力路由工作的进程 owner 不能自留一份 Provider 可用性规则。Provider 只提供一次
无副作用的读取，返回它会服务的模型，以及路由决策需要的标识和已声明能力。这次读取是进程
host 端口，不是管理方法：不在 `provider` 命名空间上，不经 RPC 提供，也不改变 `provider.catalog`。

**为什么是端口而不是 `ProviderService` 上的方法。** 消费方插件在自己的 crate 中，
`xtask check` 拒绝依赖装配层 `dim-app-server`，因此消费方无法命名 `ProviderService`。
端口及其数据类型放在 Provider 与消费方都依赖的 `dim-agent-capabilities`；注册为进程名后，
消费方声明 `Keyed<SelectableModelsHost>`，由 catalog 解析。依赖从扩展指向 core，符合
[ADR 002 D3](../../adr/002-plugin-scope.md)，实现、目录和规则仍留在 Provider。

**合同（语言中立）。** 进程 host 端口，无实例键，由 Provider 提供：操作 `selectableModels`，
异步，请求 `{}`，回复 `{models: ModelCapabilitySnapshot[]}`。元素就是既有
`ModelCapabilitySnapshot`——`{target: {providerId, modelId}, capabilities:
ModelCapabilities, reasoning?, serviceTier?}`——不是新的模型类型：它已经携带 provider/model
标识与已声明能力，就是执行时采用的那份描述。可选模型一定声明了两项上限（§3.5），因此既有
`capability_snapshot` 转换不会失败。调用方传入自己的取消 token；取消它即取消本次读取并返回
`Cancelled`。

**应用的规则。** 候选 provider 与默认解析一致：托管包公布的每个账号 provider，以及
`provider.list` 返回的每个静态 provider。每个模型来自与 `provider.catalog` 相同的单 provider
目录读取（`catalog_for`），所以捆绑替换、descriptor 覆盖、用户模型、路由、禁用和账号状态
都是执行时看到的那一份。模型只有在既有 `ProviderCatalog::selectable` 接受时才出现：
`unavailableReason` 缺失，且 ID 不在 `disabledModelIds` 中。该原因已覆盖全部排除项：
`ModelStatus::Unavailable` 的模型、未声明 `text` modality 的模型、在 `new-api-router`
之外的 driver 上的非 chat 模型、禁用的 Provider 记录、没有路由的模型、已登出的账号、
缺凭据的静态记录，以及未声明上限（`limits_undeclared`）。读取不增加第二个谓词。模型出现，
意味着同一快照下 `select`、`configuration`、`apply_preferences`、每请求检查和
`auxiliaryModel` 都会接受它。执行仍是权威：之后的写入可使其不可选，消费方把 ID 交回
Provider，由后者重新检查。

这次读取不是 [#560](https://github.com/arcships/dim/issues/560) 的校验合同。后者的合同在
§3.7：它接收已准入的持久化静态记录和显式模型 ID，回答该选择能否执行；本次读取没有输入，
只枚举当前可用项。两者定义在同一目录和同一可选性谓词上，因此对同一个模型不会给出不同答案。

**快照与一致性。** 一次调用对每个 provider 各读一次。没有跨 provider 事务、没有版本、
没有缓存：并发的 `provider.*` 写入可能对部分 provider 可见、对另一部分不可见，下一次调用
返回新状态。不发起实时请求：不 refresh、不列 provider 端点，也不写记录、选择或偏好；账号
状态沿用 `provider.catalog` 的同一读取。回复是派生数据，消费方不得持久化，也不得当作已
采用的快照。采用仍走模型选择，后者只解析一次配置。

**错误。** 本次读取没有自己的错误码，返回 Provider 既有错误：调用方取消传入的 token 时
`Cancelled`，Provider 卸载时 `Failed` 且 `HostErrorKind::OwnerClosed`，Provider 存储不可读时
`Failed`。单个 provider 的记录或账号状态不可读时按默认解析的方式跳过；它不会遮蔽其他
provider，读取也不返回逐项错误列表。需要该 provider 诊断的调用方使用 `provider.list` 或
`provider.management`。

**消费方。** Modality 用它计算每个操作的选项和自动默认值：把返回的 ID 与已收集 adapter
声明支持的操作求交，并在配置的模型不在其中时报告 "no adapter for the configured model"。
它不调用 `provider.catalog`，也不自留可用性谓词。回复只带 ID 和能力，不带显示名：渲染选项
的客户端用 `provider.catalog` 或 `provider.management` 的模型列表按 ID join，Modality 不复制
名称。Provider 不增加逐消费方的键、配置或过滤器：读取没有输入，需要单个 provider 的调用方
自行筛选回复。

**不变的部分。** `provider.catalog`、`provider.management`、`provider.select`、模型选择、
采用、偏好及其错误；客户端协议与 `PROTOCOL_VERSION`；持久化及其格式；Timeline、
Dispatcher、before 回调、Session 与分支部件；driver、路由和非 chat 规则。读取不增加状态、
缓存、通知、crate 或模块。

**Rust 绑定。** 端口定义在 `crates/dim-agent-capabilities/src/host.rs`，`ModelSelectionHost`
旁。Provider 在 `ProviderPlugin::apply` 中提供该端口，App Server 的 catalog 以 Rust-only 名字
`selectableModelsHost` 注册它（[#4](https://github.com/arcships/dim/issues/4) 之前没有跨语言绑定）：

```rust
pub trait SelectableModels: Send + Sync + 'static {
    fn selectable(
        &self,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Vec<ModelCapabilitySnapshot>>;
}
pub struct SelectableModelsHost(pub Arc<dyn SelectableModels>);
```

owner 实现是 `ProviderService::selectable_models`，`ProductResolver` 转调它。消费方在 `Deps`
中声明 `Keyed<SelectableModelsHost>`；消费方不直接读取 `ProviderService`。非 Rust 消费方在
运行时按名暴露服务的要求（[#4](https://github.com/arcships/dim/issues/4)）交付后按名访问；
该要求已在 [Modality §9](../plugins/Modality.md#9-向机制所有者提出的需求) 提出，这里不重复提出。

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

## 4. 客户端集成

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
`←` 与 `→` 在选中模型上循环其声明的推理强度。Enter 采用该模型；若强度被切换过，
先把它存成该模型的 `reasoningEffort`，再连同其余已保存偏好一起发送。编辑提供方的模型列表属于
Models 管理领域（[CLI 命令 § Models 领域](../cli-commands.md#models-领域)）：`/models` 的
`Edit models` 与 `dimcode models PROVIDER add|edit|set|remove|inspect` 读取
`provider.management`，并经 `provider.models.*` 写入；上面的选择器动作保持不变。

catalog 条目带 `zdr: true` 的模型在推理强度前显示 `ZDR` 标签，固定子代理模型选择器也一样。
搜索 `zdr` 会包含这些模型；字段缺失或为 false 时不显示标签。替换模型列表时，
标签随服务端最新字段更新，不保留已撤销的标记。

列表还会通过 `provider.refresh` 填充 catalog 无模型的静态 provider：`provider.list`
说其模型可列举时，每个连接对 catalog 的每个版本只问一次，并显示刷新写入的 catalog。
刷新成功时记录它写入的版本，因此列举为空的结果不会再次询问；记录被写入（启停用、
改端点）后再问一次。读取或刷新失败时该 provider 的提示行保留；面板关闭后才到达的
回复只记录这次填充，下次打开再读。

未声明的模型不是选择器行。每个至少有一个此类模型的 Provider 在模型行之后保留一行灰色提示：
统计需要窗口的模型数量并给出声明命令；该 Provider 没有任何可选模型时，这条提示取代空 Provider
行，而不是与之并列。`dimcode models PROVIDER --set-limits MODEL --context-window N
--max-output-tokens N` 通过 `models.edit` 为单个模型声明两项上限；
`--set-missing-limits --context-window N --max-output-tokens N` 为列表标为 `undeclared`
的每个模型声明，即缺失上限是唯一障碍的启用模型；被禁用或因其他原因不可用的模型不写入。
两种形式都必须同时给出两个具名值。两者都不能与
`--refresh` 或 `--clear-preferences` 组合。两者都先读取记录版本，串接每次 `models.edit`
回复的版本，然后像 `models PROVIDER` 一样打印目录。列表把上限未声明的模型标为
`undeclared`，并在行下列出声明命令。

```text
Select a model
> Provider / model          ZDR  ‹ high ›
Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close
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

**格式决定：保持 v19 和现有 SQL schema。** 只增加以下可选顶层字段，缺失表示无对应用户数据：

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

bundled reseed 遇到这种冲突不能导致 seed 或 Provider 激活失败。fingerprint 改变后的实际
替换候选若与用户模型冲突，保留该记录已准入的定义、fingerprint 和 version，继续在既有
事务中处理其他记录；remoteModels 为真的记录继续保留其远程默认列表。§3.1 沿既有逐项
错误列表提示冲突，Provider 和 models.delete 保持可用。显式删除冲突用户模型后，下次
启动采用新 bundled 定义。其他校验错误沿用原失败行为。替代用户模型的 ID 须与新 bundled
默认列表中的 ID 不同；不自动
改名/覆盖、不持久化警告，也不新增迁移服务。

两项模型上限另有自己的格式决定：[#522 格式决定](../../../design/appserver/native-storage-format.md#522-v26-格式决定)
把未声明的上限变为缺省键（上一格式的读取器两种取值都解不了），并在启动事务内原地升级既有记录，
而不是沿用版本号。该决定写明本次改动引入的格式版本、启动事务执行的改写，以及旧二进制的启动拒绝；
§3.5 说明有效目录随后如何按来源补齐声明值。

旧 v19 能读取新记录，但重写会丢新增顶层字段，包含 fingerprint 变化触发的自动 seed_bundled，
不只用户主动编辑。明确接受该降级损失。一次性数据库验收须用旧 v19 代码读取并实际重写新记录。
不增加存储 owner 门禁、删除历史或格式号；未知数据库格式仍按原规则拒绝并保留字节。

| 改动 | 对已采用 Session 的影响 |
| --- | --- |
| 凭据、base URL、headers、代理、超时 | 下次请求解析当前连接；已发 I/O 保持其准入连接 |
| 模型 descriptor/capability、持久偏好 | 已采用快照不变，下次显式采用生效；偏好还需客户端显式传入 |
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
目标必须是未改动的播种记录。custom 行的 ID 属于捆绑目录时拒绝。两个入口用同一解析器
解析并校验既有目标记录，校验不通过则拒绝该项。Migration 在转换之前把不是未改动播种记录的
目标归为 Conflict 且不带原因；其余情况下转换的拒绝原因同时到达两个入口。

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
| 存储/导入 | 旧 v19 读取并重写新记录含 reseed；bundled 升级与内置 Provider 的用户新增模型冲突时 Provider 激活，该记录/定义/fingerprint/version 不变，其他记录更新，管理返回冲突；显式删除后保留需重启提示，启动再采用新定义；删除禁用用户模型时清理遗留禁用项，新内置项为启用状态；用已消失默认模型的 ID 新建用户模型时，新项启用且不继承原默认项的 descriptor 覆盖、偏好或端点覆盖；有效/非法/重复导入及源保留，均用一次性数据库 |
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
| §4.8 | §5 无机制需求，复用 metadata 和 v19 顶层 JSON；§3.6、§3.7 不增加需求，非 Rust 消费方使用 Modality 已提出的 [#4](https://github.com/arcships/dim/issues/4) |
| §4.9 | 不加 Timeline 事件或进程推送通知 |
| §1 | 扩展现有进程 Provider，不增 Capability/接线插件 |
| ADR 002 D1 | 进程记录归 Provider，已采用分支状态归原 owner，草稿归客户端 |
| ADR 002 D2 | 复用 namespace/resolver，不增管理插件或注册表 |
| ADR 002 D3 | 可选账号失败只影响自身能力，不加 core 依赖；§3.7 是 Provider 模块函数 |
| ADR 002 D4 | 业务配置归 owner，修改不 reload |
| ADR 002 D5 | §3 明确字段、方法、mutating/interruption、错误，不只写 Rust trait；§3.6 以中立形式给出进程读取，core host 端口是消费方声明的绑定；§3.7 是 Rust-only 模块边界，因为输入是 Provider 已准入且携带凭据引用的记录，且没有跨语言消费方 |

## 9. 消融检查

管理读取只保留一份编辑快照；原 catalog 服务选择。create/update/probe 分别承担可恢复
identity、保留秘密的局部写和未保存网络探测；Disconnect 复用 update。独立用户模型列表
用于区分默认覆盖并让新增项可执行；路由标记沿已有 metadata 保留采用时协议含义。
声明的上限不需要新状态：目录已有的标记与匹配说明值是否存在，缺失由目录类型承载，原因在投影处
推导，因此不增加平行模型列表、第二份目录或 `limits` 实体。
可选模型读取不增加状态、缓存、事件或模型类型：它把既有 `ProviderCatalog::selectable`
应用到同一份单 provider 目录，并返回既有 `ModelCapabilitySnapshot`，因此没有第二个可用性
谓词或平行目录。
已存选择合同同样不增加状态、存储或账号读取、缓存或谓词：它构建既有目录、应用既有谓词，
并返回既有已采用快照和执行已经派生的路由。需要一个命名的回复类型，因为路由与快照就是
公开形状；返回整个目录或客户端 DTO 会暴露更多，且仍需同一判断。Migration 侧不增加谓词或
目录构建，选择转换调用本合同，而不是保留一份只服务旧格式的私有判断。
不需要通知、跨删除版本服务、模板实体、凭据抽象或第二份客户端配置。
