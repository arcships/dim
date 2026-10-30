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
| 模型管理 | 区分默认与用户模型，分别处理描述覆盖和显式 New API 路由 |
| 模型偏好 | 客户端读取持久偏好，并在明确采用模型时携带完整目标 |
| 账号与用量 | 复用账号 owner、合同与错误；不增加静态 Provider 用量配置 |
| 多客户端 | 记录 CAS，打开及自身写入后重读，不推送变更通知 |
| 旧配置导入 | 显式只读导入，映射来源、逐项返回结果并保留源字节 |

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
| `models` | `{model:ModelEntry,source:"default"\|"override"\|"user",enabled:boolean,endpoint:ModelProtocol\|null,unavailableReason:string\|null,actions:string[],endpointOverride?:NewApiEndpoint}[]`；路由覆盖限 §6 可执行子集。仅账号无本地目录记录时为 `null`，坏目录报错。模型操作为 `set_enabled`、`edit`、`reset`、`delete`，按来源/authority 限制 |
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
models.set_enabled 接受默认模型及用户模型的 ID；这些操作从同一记录更新有效目录和路由，
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
服务端不自动应用保存偏好；显式目标省略偏好仍从目录默认开始，无目标 F3 只改已采用模型的
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
先把它存成该模型的 `reasoningEffort`，再连同其余已保存偏好一起发送。

catalog 条目带 `zdr: true` 的模型在推理强度前显示 `ZDR` 标签，固定子代理模型选择器也一样。
搜索 `zdr` 会包含这些模型；字段缺失或为 false 时不显示标签。替换模型列表时，
标签随服务端最新字段更新，不保留已撤销的标记。

列表还会通过 `provider.refresh` 填充 catalog 无模型的静态 provider：`provider.list`
说其模型可列举时，每个连接对 catalog 的每个版本只问一次，并显示刷新写入的 catalog。
刷新成功时记录它写入的版本，因此列举为空的结果不会再次询问；记录被写入（启停用、
改端点）后再问一次。读取或刷新失败时该 provider 的提示行保留；面板关闭后才到达的
回复只记录这次填充，下次打开再读。

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

旧 v19 能读取新记录，但重写会丢新增顶层字段，包含 fingerprint 变化触发的自动 seed_bundled，
不只用户主动编辑。明确接受该降级损失。一次性数据库验收须用旧 v19 代码读取并实际重写新记录。
不增加存储 owner 门禁、删除历史或格式号；未知数据库格式仍按原规则拒绝并保留字节。

| 改动 | 对已采用 Session 的影响 |
| --- | --- |
| 凭据、base URL、headers、代理、超时 | 下次请求解析当前连接；已发 I/O 保持其准入连接 |
| 模型 descriptor/capability、持久偏好 | 已采用快照不变，下次显式采用生效；偏好还需客户端显式传入 |
| 禁用/删除 Provider 或模型 | 下次请求/采用返回不可用/不存在，不强制切换或跨写引用 |
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

旧 TS 配置继续显式只读导入，逐条 imported/already_imported/rejected。明确映射连接、默认、
用户覆盖/新增、禁用、偏好及 scoped 选择。activeModelId 仅作提示，不覆盖已有选择。
非空静态用量设置、未支持 retryConfig/capabilityDefaults 或其他未映射字段拒绝并保留源，
按以下冻结映射处理。不覆盖已有目标、不复制 OAuth grants、不因重复导入复活删除项。
草稿、模板、用量回复、已解析凭据不持久化。资源取得即登记清理；关准入、取消自有 I/O、
释放租约并结算已接受调用后移除服务。

冻结的旧库结构通过 `modelsSource` 区分模型来源：

| 旧字段 | 原生记录 |
| --- | --- |
| `modelsSource: bundled/remote`、`models` | 默认描述；只有 `remote` 设置 `remoteModels` |
| `modelsSource: user`、`models` | `userModels`；已有内置项保留原生捆绑默认，ID 冲突则拒绝该项 |
| `modelConfigOverrides` | 匹配用户模型的描述替换该用户项；其余保留为描述覆盖，已消失的默认也不会变成用户模型 |
| `metadata.enabledModelIds/disabledModelIds` | 默认和用户模型的显式禁用；列表冲突则拒绝 |
| `metadata.modelPreferences` | 既有严格 `ModelPreferences` 字典 |
| `activeModelId` | 单个 Provider 的提示，无权创建或覆盖 scoped 选择 |
| `provider_selections` | 沿既有目标校验和导入账本导入明确的 scoped 选择 |

不支持的用量/retry/capability 设置为 null 或空对象/数组时不携带配置；非空值拒绝该项。
其他 metadata 键、未知描述字段同样拒绝，源文件字节不变。

## 7. 验收与交付

| 门禁 | 必须提供的可观察证据 |
| --- | --- |
| 合同 | 类型化 DTO、CLI 迁移、显式偏好目标、路由处理和冻结导入映射 |
| 秘密/patch | 单 header 更新不覆盖其他项，改超时保留代理认证，keep/set/clear/ref 及 authority，RPC/日志/错误无秘密 |
| 模型/路由 | 内置/自定义用户模型真实调用；create 的 probe 默认项，以及随后新增/编辑/启停/删除、refresh 后保留且不成为默认覆盖的手动/模板项；仅改端点时未有 descriptor 覆盖的默认项仍为 source:"default"，descriptor 继续随 refresh 更新；已有 descriptor 覆盖不变；reset 保留路由覆盖，clear 保留 descriptor 覆盖；显式/profile/默认选择均建标记，无目标 F3 与 Collaboration inherit 保留缺失；inherit 创建时不重新解析，首次请求校验已有标记；refresh 和 endpoint override 改路由时拒绝不匹配标记，重选后 effort/tier 正确；无标记 Session 在路由未变时继续工作，覆盖两种改变路由风险；显式删除用户模型解决 refresh 冲突 |
| Probe | 未保存探测、合并当前记录且不检查版本、换 host 使用原凭据前校验，失败/超时/卸载无写入，过期回复丢弃，不宣称客户端可取消 |
| 偏好 | 独立 set/clear、重启，空 patch/不支持值为业务错误，格式错误为边界错误；参数与准确目标一起采用，不可用不回退，无目标 F3 和消费方选择不变 |
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
| §4.7 | 账号沿公开方法，不跨写其他 owner 引用 |
| §4.8 | §5 无机制需求，复用 metadata 和 v19 顶层 JSON |
| §4.9 | 不加 Timeline 事件或进程推送通知 |
| §1 | 扩展现有进程 Provider，不增 Capability/接线插件 |
| ADR 002 D1 | 进程记录归 Provider，已采用分支状态归原 owner，草稿归客户端 |
| ADR 002 D2 | 复用 namespace/resolver，不增管理插件或注册表 |
| ADR 002 D3 | 可选账号失败只影响自身能力，不加 core 依赖 |
| ADR 002 D4 | 业务配置归 owner，修改不 reload |
| ADR 002 D5 | §3 明确字段、方法、mutating/interruption、错误，不只写 Rust trait |

## 9. 消融检查

管理读取只保留一份编辑快照；原 catalog 服务选择。create/update/probe 分别承担可恢复
identity、保留秘密的局部写和未保存网络探测；Disconnect 复用 update。独立用户模型列表
用于区分默认覆盖并让新增项可执行；路由标记沿已有 metadata 保留采用时协议含义。
不需要通知、跨删除版本服务、模板实体、凭据抽象或第二份客户端配置。
