# Settings 服务

状态：设计已通过评审；已实现。Rust 侧（S8b 第 1 部分，§12）、TypeScript 所有者路径（第 2 部分）、
向 TypeScript 部分投递事件（第 3 部分）、由所有者在运行时登记来源（第 4 部分，
[PluginManager §9 R1](../plugins/PluginManager.md#9-向机制所有者提出的需求)）。共享的 `credentials` 服务已撤销
（§1.2）。路线图项 S8b（[#118](https://github.com/arcships/dim/issues/118)）。
基于 S8a（[#54](https://github.com/arcships/dim/issues/54)，PR [#90](https://github.com/arcships/dim/pull/90)）；
实现依赖 C4（[#63](https://github.com/arcships/dim/issues/63)）。
总体设计：[§6.4 配置](../overview.md#64-约定)。作用域：
[ADR 002](../../adr/002-plugin-scope.md) D4 与 D5（进程级，放在现有的 Settings 插件内）；
[ADR 004](../../../adr/004-loader-plugin-management.md) D4。

## 1. 摘要

Settings 为不是用 Rust 写的插件提供业务来源。这类插件在启动时向 Settings 登记自己的每个来源
（[ADR 007](../../adr/007-plugin-configuration.md) D5）：来源名、文件、JSON Schema（含 `default` 值）和
仅限用户的字段（§3.1）。随后 Settings 在 JSON 文档上提供三样东西：

- 进程服务 `settings`，供所有者使用：`register`、`read`、`update`（按 revision 做 CAS）和 `projects`；
- 客户端管理方法 `settings.read` 和 `settings.update`；
- 与 S8a 相同的 rutis 事件 `settings.changed` 和 `settings.project_authorized`。它们的载荷改为
  可序列化。没有 `subscribe(source)`。

写入有两条路径。客户端写入（`settings.update`）可以改任何字段。所有者写入（`settings` 服务）
不能改仅限用户的字段。所有者代其工具发起的写入属于所有者写入，因此改动仅限用户字段时被拒绝。

Settings 不赋予任何字段特殊含义，只有一个例外：schema 带 JSON Schema 注解 `writeOnly: true` 的成员
对 agent 来说是密钥（§1.2）。除此之外，密钥是所有者文档中的普通字段，是否保存、如何保存、加密或查找，
由所有者自己决定。

`BusinessDocument` 仍是 Rust 绑定；Rust 所有者以同样方式登记自己的文档类型（§4.5）。Rust 来源保留
其所有者的管理方法。F2 的
`permission.rules`/`permission.set_rules` 保留（§8.1）。

不新增插件，不新增 Timeline 事件，不改 Timeline、Dispatcher、rutis 或装配。

同一 Settings 提供者还提供生成的 `workflowFiles`，只访问定义目录；它读取现有项目授权表和原调用进程行声明的包范围，不导出 FileStorage 或原始 root 编号。见[原生定义文件合同](../../../design/p6-p7-bindings.md#3-workflow-definition-files)。

### 1.1 不做什么

- 不做 `subscribe(source)`。issue 正文列了它，但 S8a 已把变更通知移到 rutis 事件总线（见 issue
  评论）。Settings 不保留自己的监听通道。
- 不向客户端通用地开放 Rust 来源（`mcp`、`mcp-projects`、`mcp-plugins`、`skills`、`permission`）。它们的校验是
  Rust 代码，Settings 无法拿来校验客户端传来的 JSON。
- 不提供来源发现方法，不做设置界面。见 §10。
- 不提供密钥存储、凭据服务或字段名规则（§1.2）。唯一的标记是 schema 的 `writeOnly` 注解。
- MCP 的内联 token、`env` 和 `headers` 保持原样（ADR 004 D4）。

### 1.2 决定：密钥归其所有者

插件是否保存密钥、如何存储或加密，是该插件自己的业务，Rust 和 TypeScript 都一样。基础设施不把
某个所有者的需求变成共享服务（原则 §4 第 8 条）。现有所有者本来就是这样做的：MCP OAuth 把 token
存在自己的 0600 `FileStorage` 记录里（[MCP OAuth](mcp-oauth.md)），MCP 把内联 token 放在自己的
来源里，Provider 保留内联凭据和自己可选的 `secretRef` host。

本设计的早先版本新增了进程服务 `credentials.resolve(owner, reference)`，要求 `*Ref` 字段为非空
字符串，并把 Provider 的 secret host 迁到这个服务上。没有任何产品插件提供该服务，于是每个使用
引用的所有者都依赖一个无人负责的存储。该服务和 `*Ref` 规则现已撤销：

- 密钥是普通的文档字段，规则由所有者的 schema 表达（例如 `minLength`）。只给用户填写的字段声明在
  `userOnly` 中，并放在用户文件而不是项目文件里，因此不会随项目提交。
- 存储与读取：文件存储 profile 在 Unix 上以 0600 权限写用户文档；原生 profile 将其作为
  `user/<source>` 存入 SQLite，没有独立的 0600 文件保证。权限边界是产品 home：Unix 新建目录使用
  0700，Windows 继承父目录 DACL，已有目录权限保持不变。`settings.read` 只服务本地连接（§7），
  所以本地客户端可以读到密钥，远程与配对客户端不能。所有者从不把它放进工具声明、结果、事件、
  诊断、错误或日志。
- 能经 `properties` 到达的成员上的 `writeOnly: true` 标记密钥。它不改变校验。agent 读取文档的途径，
  即 `plugin` 工具的 `config` 命令（[PluginManager §6](../plugins/PluginManager.md#6-接口摘要)），
  从不返回它的值：该成员被去掉，有值时列出它的 JSON Pointer 表示已设置；该命令也拒绝写入本身是或
  含有 `writeOnly` 成员的成员。客户端不变：本地的 `settings.read` 和 `settings.update` 像其他字段
  一样读写它（§7）。所有者在 schema 中给每个凭据声明 `writeOnly`，同时也声明 `userOnly`。
- 需要钥匙串或加密的所有者在自己的插件内实现，只把它选择的内容放进文档。
- Provider 保留自己的 `ProviderSecretHost` / `ResolveProviderSecret`（`provider/secrets.rs`），
  与 S8b 之前相同。这是 Provider 自己的扩展点，不是共享契约。

## 2. 结构

```
 进程作用域
 +-------------------------------------------------------------------------+
 |  Settings（现有插件）                                                   |
 |   SettingsService                                                       |
 |     已登记的来源，各自保留到登记它的插件卸载：                          |
 |       BusinessDocument 类型（Rust 所有者）                              |
 |       声明的来源：schema、默认值、文件、仅限用户的指针                  |
 |   具名服务 `settings`（HostDispatch）  <-- 所有者写入                   |
 |   managementMethods `settings`          <-- 客户端写入                  |
 |   在 settings/<source> 上发出 settings.changed，                        |
 |   在 settings/projects 上发出 settings.project_authorized               |
 +------^-------------------------------^----------------------------------+
        | register/read/update/projects | settings.read / settings.update
        |                               |
  TypeScript 所有者（rutis 运行时行）   Rpc  <--  本地客户端
```

依赖指向核心。Settings 不依赖任何插件，也不点名任何所有者。TypeScript 所有者依赖 `settings`。

## 3. 数据与状态

### 3.1 来源登记

每个所有者，不论是内置 Rust 还是 TypeScript，都在自己进程行的 `apply` 中登记来源：TypeScript 所有者
用 `settings.register(declaration)`（§4.2），Rust 所有者用 `SettingsService::register::<D>`（§4.5）。
App Server 的装配和 PluginManager 都不声明来源。登记属于发起它的插件，在该插件卸载（关闭、移除或
重载）时撤回；重载时由新一代重新登记。不论来源是否已登记，存储的文档都留在 home 中：未登记的来源
在所有路径上都是 `unknown_source`，有所有者登记它后，其文档重新可用。

所有者代码中持有的一份声明：

```json
{
    "source": "web",
    "userFile": "web.json",
    "projectFile": null,
    "schema": {
      "type": "object",
      "properties": {
        "searchService": {
          "type": "object",
          "properties": {
            "kind": { "type": "string", "minLength": 1 },
            "endpoint": { "type": "string", "format": "uri" },
            "apiKey": { "type": "string", "minLength": 1 }
          },
          "required": ["kind"],
          "additionalProperties": false
        },
        "allowPrivateHosts": { "type": "array", "items": { "type": "string" }, "default": [] },
        "proxy": { "type": "string", "format": "uri" }
      },
      "additionalProperties": false
    },
    "userOnly": ["/searchService", "/allowPrivateHosts"]
}
```

| 字段 | 规则（登记时检查一次） |
| --- | --- |
| `source` | `[a-z0-9_-]`，与 Rust 来源相同。在进程内已登记的来源（含 Rust 来源）中唯一 |
| `userFile` | 规则同 `BusinessDocument::USER_FILE`（home 根目录下的普通文件名）。两个已登记来源不能共用用户文件 |
| `projectFile` | 可选，规则同 `PROJECT_FILE`。存在即表示该来源也有项目作用域。这就是"允许的作用域"声明。两个已登记来源不能共用项目文件 |
| `schema` | JSON Schema（draft 2020-12）。必须能编译，根必须是 `type: object` |
| `userOnly` | 指向对象成员的 JSON Pointer（RFC 6901）。每个都必须能经 `properties` 到达 |

默认文档是应用默认值（§3.2）后的 `{}`。它必须通过 schema 校验，否则登记失败。因此文件缺失时
读到的总是合法文档。被拒绝的登记使登记调用失败，从而使调用方的 `apply` 失败。

### 3.2 文档、校验与默认值

声明的来源保存一个 JSON 对象。Settings 在其边界处理它，与 S8a 处理 Rust 文档的方式相同：

- **默认值**：对每个 schema 带 `properties` 的对象，缺失且有 `default` 的成员被填上。只沿
  `properties` 递归（不跟随 `$ref`、`allOf`、`anyOf`、`oneOf`），作用范围与嵌套结构体上的
  serde `#[serde(default)]` 相同。
- **时机**：每次写入时，以及读取持久化文件或原生行时。文件缺失即应用默认值后的 `{}`。写入和
  返回的都是填充后的文档。
- **校验**：应用默认值后按 schema 校验。Settings 不对成员名或值附加自己的规则（§1.2）。
- 使用方按交付的样子使用文档，不再校验，也不自带默认值。JSON Schema 表达不了的规则（例如
  CIDR）由所有者在使用时给出诊断。

### 3.3 持久化与 revision

与 S8a 相同。用户作用域使用用户文件，或 v13 `settings_documents` 契约下增量的原生行 `user/<source>`。项目作用域
使用项目文件和内容 CAS。revision 是同样的不透明字符串（`missing`、文件的 SHA-256，或
`native:user/<source>:<version>`）。没有格式变化。

### 3.4 运行时状态

Settings 在内存中保存已登记的来源（声明已编译），各自保留到登记它的插件卸载。此外没有新增状态。
不进入 Timeline。

## 4. 接口

### 4.1 通用形态

| 形态 | JSON |
| --- | --- |
| `Scope` | `"user"` 或 `{"project": <root>}`。`root` 是 Settings 在本进程内分配的项目根编号（`SettingsScope::Project`）。它不是持久标识，从不持久化 |
| `Snapshot` | `{revision: string, document: object}` |
| `Error` | `{code, message}` |

### 4.2 服务 `settings`（所有者路径）

进程作用域，无实例 key，由 Settings 提供。

| 方法 | 模式 | 请求 | 回复 |
| --- | --- | --- | --- |
| `register` | async | 一份声明（§3.1） | 无；被拒绝时 reject |
| `read` | async | `{source, scope: Scope}` | `Snapshot` |
| `update` | async | `{source, scope: Scope, expectedRevision, document}` | `Snapshot`（保存后的文档） |
| `projects` | sync | `{}` | `{projects: [{root, cwd}]}`：当前已授权的项目根 |

- `register` 是调用行的登记，在该行卸载时撤回。它在该行自己的 `apply` 中调用；被拒绝（§3.1）时
  reject 而不返回信封，因此像每个所有者的 `register` 一样使该 `apply` 失败。
- `update` 是 CAS：`expectedRevision` 不是当前 revision 时以 `conflict` 失败。
- `update` 是所有者写入。已保存的文档与新文档在任一 `userOnly` 指针处不同（值改变、新增或删除）
  时以 `user_only_field` 失败。比较的是应用默认值后的文档。
- 所有者先读 `projects`，之后跟随 `settings/projects`（§4.4）。
- 取消：调用方丢弃调用。已提交的写入保持已提交，与 S8a 相同。
- `read`、`update` 和 `projects` 的错误（§6）：`unknown_source`、`invalid_scope`、`invalid_document`、
  `user_only_field`、`conflict`、`unsynced`、`failed`、`closed`、`cancelled`。

只有 `register` 使用调用方身份，以便随调用行撤回登记。所有权来自登记它的行：每个来源只有一个所有者，
只有那个包针对它编写。这是进程内
插件的信任模型（AGENTS.md 规则 11）：依赖声明不是沙箱。

### 4.3 管理方法 `settings.*`（客户端路径）

命名空间 `settings`，由依赖 Settings 的进程部分 `SettingsMethods` 注册到 `managementMethods`
（§4.5）。从不向已配对设备提供（总体设计 §6.2 第 9 条）。

| 方法 | 请求 | 回复 | 错误 |
| --- | --- | --- | --- |
| `settings.read` | `{source, scope}` | `{revision, document}` | 参数无效（作用域错误）、`settings_unknown_source`、`settings_failed` |
| `settings.update` | `{source, scope, expectedRevision, document}` | `{revision, document}` | 参数无效（作用域错误、违反 schema）、`settings_unknown_source`、`settings_conflict`、`settings_unsynced`、`settings_failed` |

- 只服务已登记的声明来源。Rust 来源（例如 `mcp`）返回 `settings_unknown_source`，仍由其所有者的方法
  修改。
- 客户端写入可以改仅限用户的字段。除此之外与所有者路径没有区别。
- `closed` 映射为现有的可重试 reloading 错误。

### 4.4 事件

通道和名称沿用 S8a。载荷改为可序列化数据：

| 事件 | 通道 | 载荷 |
| --- | --- | --- |
| `settings.changed` | `settings_changes(source)` = `settings/<source>` | `{source: string, scope: Scope, revision: string}` |
| `settings.project_authorized` | `project_authorizations()` = `settings/projects` | `{root: number, cwd: string}` |

声明的来源与 Rust 来源完全一样，在提交后发出 `settings.changed`，两条写入路径都如此。载荷从不
带文档内容。使用方先读一次，再跟随事件。

任何运行时中的部分都在总线上监听这些通道（§10 第 2 条）。TypeScript 部分为它用到的每个来源注册
`ctx.on('settings/<source>', (payload) => …)`，并注册 `ctx.on('settings/projects', …)`，
唯一的参数就是上表的 JSON 载荷。

- 部分监听它用到的来源，包括 Rust 来源。载荷从不携带文档，所以不会暴露部分读不到的内容。
- 同一通道的事件按提交顺序开始。慢的部分既不拖慢下一个事件，也不拖慢 Settings 的其他监听方。
- 不重放：部分启动时读一次，之后跟随事件，与 Rust 使用方相同。部分的行加载之前发出的事件
  （例如它自己 Session 创建时的项目授权）不会投递给它。
- 投递失败（监听方抛错）进入 rutis 的错误出口，不影响后续事件。

不需要对变更做出反应的所有者仍可在使用时读取（F2 的 Permission 就是这样）。

### 4.5 Rust 绑定

在 `dim-app-server::settings` 中（S8a 的类型，只在注明处改动）：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsScope { User, Project(usize) }            // + Serialize/Deserialize

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChanged { pub source: String,           // 原为 &'static str
                             pub scope: SettingsScope, pub revision: String }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAuthorized { pub root: usize, pub cwd: PathBuf }   // + Serialize/Deserialize

/// 一份声明（§3.1），即 `register` 收到的内容。
pub struct SourceDeclaration { pub source: String, pub user_file: String,
                               pub project_file: Option<String>, pub schema: serde_json::Value,
                               pub user_only: Vec<String> }

pub enum Writer { Owner, Client }

impl SettingsService {
    /// 登记 `D`（或一份声明），直到拥有 `ctx` 的插件卸载。
    pub fn register<D: BusinessDocument>(&self, ctx: &Ctx) -> rutis::Result<()>;
    pub fn register_declared(&self, ctx: &Ctx, declaration: SourceDeclaration) -> rutis::Result<()>;
    pub async fn read_declared(&self, source: &str, scope: SettingsScope,
                               cancel: &CancellationToken) -> Result<SettingsSnapshot<Value>>;
    pub async fn update_declared(self: &Arc<Self>, source: &str, scope: SettingsScope,
                                 expected: &str, document: Value, writer: Writer,
                                 cancel: &CancellationToken) -> Result<SettingsSnapshot<Value>>;
}
```

- 两种登记都是拥有 `ctx` 的插件的副作用，在它卸载时撤回，并在同一份登记状态下检查（§3.1）。
  `register_declared` 用 `jsonschema` 编译 schema。进程级所有者在 `apply` 中登记：Mcp（`mcp`、
  `mcp-projects`、`mcp-plugins`）、SkillsCatalog（`skills`）、PermissionRules（`permission`）、
  CollaborationModelPolicy（`collaboration`）和 App Server 的 ExecutionSettings（`execution`）。
  其他插件中的读取方只读；对某个名字用不是其已登记类型的
  `read::<D>`/`update::<D>` 得到 `UnknownSource`。`settings` 服务的 `register` 以调用行的上下文调用
  `register_declared`。
- `execution` 只有用户作用域：它不声明项目文件，因此项目作用域的读或写会被拒绝。它持有两个布尔值
  `sandbox` 和 `network`，默认都是 `false`，revision 就是普通的 CAS revision。TUI 的 `Process isolation` 控件
  和任何本地管理客户端使用 `settings.read`/`settings.update`；所有者的读取方通过
  `ExecutionSettingsHost` 端口取得文档，那是进程内读取契约，不是管理方法
  （[ADR 011](../../../adr/011-execution-settings.md) D1）。可见选项到现有布尔值的映射由
  [进程隔离](../../../design/host-platform-isolation.md#process-isolation)定义；权限预设和工具列表
  不写入此来源。
- 文件和原生路径与 `BusinessDocument` 共用，按来源名而不是 `D::NAMESPACE` 取键。CAS、gate、
  lease、unsynced 情形和 `publish` 保持不变。
- `SettingsError` 新增 `UnknownSource`、`UserOnlyField(pointer)`、`InvalidScope` 和
  `Document(message)`。写入的文档违反 schema 或不是对象时为 `Document`，带校验器的
  消息（实例路径，从不带值）；客户端路径把它映射为参数无效。已持久化的文档违反 schema 时仍为
  `Invalid`（`invalid_document` / `settings_failed`）。
- `settings` 服务是 `host_key("settings")` 下的 `HostDispatch`，装配把 `settings`
  登记到 `ServiceCatalog`，与现在的 `pluginFiles` 相同。回复使用 `sessions` 的
  信封：`{ok: true, value}` 或 `{ok: false, error: {code, message}}`。
- `settings` 的 `ManagementMethods` 由 `SettingsMethods` 提供，它是依赖 Settings 的进程部分
  （模块 `settings_methods`），与 PermissionRules 拥有 `permission` 的方式相同。Settings 本身不能
  拥有命名空间：Rpc 经 Agents 依赖 Settings，而 Rpc 不得依赖管理方法所有者（方法契约 §3.2），
  模块边界也让 Settings 保持为持久化基础设施（ADR 004 D4，`xtask check`）。装配把
  `SettingsMethods` 的行列为 `settings` 的所有者。
## 5. 流程

### 5.1 所有者读取与更新

```
 TS 所有者                settings（HostDispatch）     SettingsService            存储
   | read {web, "user"} ---->| 解码请求                 |                          |
   |                         | read_declared ---------->| 文件 / user/web 行 ----->|
   |                         |                          | 默认值、schema 检查      |
   |<-- {revision r1, document} -------------------------|                          |
   | update {web, "user", r1, doc'} ->| update_declared(Owner)                       |
   |                         |                          | 默认值、schema           |
   |                         |                          | 与当前文档比较仅限用户字段
   |                         |                          |   不同：user_only_field  |
   |                         |                          | CAS r1 ------------------>| 替换
   |                         |                          | 发出 settings.changed {web, "user", r2}
   |<-- {revision r2, document} -------------------------|                          |
```

仅限用户字段的比较与 CAS 在同一个 lease（或原生事务）内进行，比较对象是将被替换的文档。

### 5.2 客户端修改仅限用户的字段

```
 客户端            Rpc              Settings（managementMethods）
   | settings.read {web, "user"} ---->| {r2, doc}
   | settings.update {web, "user", r2, 带 searchService 的 doc} -> update_declared(Client)
   |<-- {r3, doc}                     | 发出 settings.changed {web, "user", r3}
 TS 所有者（其工具要求改 searchService）
   | update {web, "user", r3, 带另一个 searchService 的 doc} -> user_only_field，不写入
```

### 5.3 登记

```
 所有者行 apply                   settings（HostDispatch）     SettingsService
   | register(declaration) ------->| caller = 调用行            |
   |                               | register_declared -------->| 解析（拒绝未知字段）
   |                               |                            | 检查名字、文件、指针
   |                               |                            | 编译 schema；应用默认值后的 {} 合法
   |                               |                            | 名字或文件已登记？拒绝
   |                               |                            | 记录；撤回 = 调用方卸载
   |<-- resolve（或 reject：该行的 apply 失败） ---------------|
 所有者行卸载 -> 它的来源被撤回；文档留在 home 中
```

与以前一样，只要有 product 或 target home 就构建 Settings；登记来源的插件需要它。

## 6. 错误

| 代码（服务 / wire） | 何时 | 重试 |
| --- | --- | --- |
| `unknown_source` / `settings_unknown_source` | 没有登记该名字的来源，或（在这些路径上）它是 Rust 来源 | 其所有者登记后 |
| `invalid_scope` / 参数无效 | 来源没有 `projectFile` 却用项目作用域，或根未授权 | 授权项目后 |
| `invalid_document` / 参数无效 | 违反 schema 或文档不是对象 | 否；不写入 |
| `invalid_document` / `settings_failed` | 持久化文件或行无效（读取时） | 用户修复后 |
| `user_only_field` | 所有者写入改动了仅限用户的字段 | 否；由用户经客户端修改 |
| `conflict` / `settings_conflict` | `expectedRevision` 过期 | 重新读取后重试 |
| `unsynced` / `settings_unsynced` | 新内容已就位并已通知，但未能同步到磁盘 | 重试前先读取 |
| `failed` / `settings_failed` | 存储、持久化或 worker 失败 | 重试前先读取 |
| `closed` / reloading | Settings 正在卸载 | 是 |
| `cancelled` | 调用被丢弃或取消 | 由调用方决定 |

## 7. 安全

- 管理方法只在本地提供；已配对设备从不能调用 `settings.*`（总体设计 §6.2 第 9 条）。
- 仅限用户的字段保护用户的决定不被所有者自己的写入改动，包括为模型工具发起的写入。它不是防御
  恶意插件的边界。进程内插件是受信的，不在沙箱中（AGENTS.md 规则 11、ADR 003）。
- 项目文件保留自己的权威（ADR 001）。声明的项目文件只在根已授权时读取，与 Rust 来源相同。
  Settings 不是安全设置，任何 Settings 文档都不授予批准（ADR 003）。
- 错误消息和校验器消息只带路径，从不带值。文档中的密钥由所有者负责保护（§1.2）：放在用户来源
  中，并列入 `userOnly`。文件存储与原生 SQLite profile 沿用 §1.2 的权限边界；`userOnly` 限制写入，
  不屏蔽读取。`settings.read` 会把它返回给本地客户端，从不返回给远程或配对客户端。
- Settings 文档从不进入 Timeline。回放和 fork 不恢复也不放宽它们。

## 8. 与现有部分的关系

| 部分 | 关系 |
| --- | --- |
| S8a Settings | 补充：声明的来源、具名服务、`settings` 命名空间、可序列化的事件载荷。`BusinessDocument`、CAS、原生行和通道不变 |
| MCP、Skills | 不变。它们的来源仍是 Rust 文档，保留各自的管理方法 |
| 权限规则（F2） | 不变；见 §8.1 |
| Provider | 不变。保留内联凭据和自己可选的 `ProviderSecretHost`（§1.2） |
| ADR 004 D4 | 它写明 S8b 之前 Settings 不发布管理方法。S8b 新增 `settings` 命名空间，仅限声明的来源。ADR 004 随实现更新 |
| C4（TypeScript 桥接） | 它的行在代码中登记自己的来源（§3.1），并自己监听 Settings 通道（§4.4） |
| PluginManager | 不声明来源：插件的业务配置归插件自己（[ADR 007](../../adr/007-plugin-configuration.md) D5）。关闭插件时，它的来源随它的行撤回 |
| Rpc | 不变；像分派任何所有者的命名空间一样分派新命名空间 |
| Timeline、Dispatcher、rutis、loader | 不变 |

新增的公开内容：`settings` 服务（含 `register`）、`settings` 管理命名空间（两个方法），
以及两个现有事件的 JSON 形态。每一项都是必需的：让非 Rust 所有者读写自己的来源并获知变更，
让客户端编辑它。

### 8.1 决定：保留 `permission.rules`/`permission.set_rules`

F2 留待决定是否把它的方法并入 `settings.read/update`。决定保留：

- `permission` 是 Rust 来源。它的校验（不含分隔字符、至少一个词）是 Rust 代码。
  `settings.update` 按声明的 JSON Schema 校验，要服务 Rust 来源就需要在 Settings 中建立一个类型
  擦除的 Rust 文档注册表，而没有别的需求需要它。
- `permission.set_rules` 已有类型化契约和错误码，F6 按它设计。
- MCP 和 Skills 适用同一规则：Rust 所有者保留自己的方法，`settings.*` 服务声明的来源。

## 9. 规则检查

| 规则 | 回答 |
| --- | --- |
| 原则 §1（唯一事件总线） | 通知仍在 rutis 事件总线上，使用 S8a 的事件。没有 `subscribe`，没有监听通道，没有第二条总线。TypeScript 部分在同一条总线上监听同一批事件 |
| 原则 §3（依赖方向） | Settings 不依赖任何插件。所有者依赖 Settings。没有核心部件依赖可选插件 |
| §4 第 1 条（指令事件） | 不使用。Settings 是经所有者类型化写入修改的进程配置（总体设计 §6.2 第 2 条） |
| §4 第 2 条（交接） | 不适用；模块之间没有交接 |
| §4 第 3 条（before 回调） | 不使用 |
| §4 第 4 条（核心不带业务含义） | Settings 按声明的 schema 校验，不点名任何所有者 |
| §4 第 5 条（批次） | 没有 Timeline 写入 |
| §4 第 6 条（上下文） | 不进入模型上下文。工具看到哪些设置由其所有者决定 |
| §4 第 7 条（只经公开接口） | 所有者用 `settings` 服务；客户端用管理方法 |
| §4 第 8 条（基础设施不变） | 不改 Timeline、Dispatcher、事件信封、rutis、loader、Agent 装配或 Session 门面。按现状使用现有的 `HostDispatch`、`ServiceCatalog`、`managementMethods` 和 rutis 事件 |
| §4 第 9 条（只有需要的事实进 Timeline） | 不新增事件；设置不是 Timeline 事实 |
| ADR 002 D1（作用域） | Settings 是进程插件；来源的生命周期长于 Session |
| ADR 002 D2（无接线模块） | 不新增插件。服务和方法放在数据的所有者 Settings 内 |
| ADR 002 D3（依赖） | 不新增可选依赖 |
| ADR 002 D4（配置） | 业务配置位于所有者的具名来源，使用时读取，带通知变更，不重载 |
| ADR 002 D5（语言中立） | §4.1–§4.4 是契约；§4.5 是 Rust 绑定 |

向机制所有者提出的需求：无。TypeScript 部分在 rutis 总线上监听（§10 第 2 条）；这不是对 dim 运行时
或 rutis 的新需求。

## 10. 待决问题

1. ~~**哪个清单承载 `settings` 条目。**~~ 已由 ADR 007 D5 定下：没有清单承载它；插件的代码登记自己的
   来源（§3.1）。
2. ~~**TypeScript 部分如何监听 rutis 事件。**~~ 已定下：部分在 rutis 唯一的总线上监听 Settings
   通道（§4.4）。Session 广播不承载它们。
3. ~~**运行中安装的包。**~~ 已定下：来源由插件自己在运行时登记（§3.1），因此安装、替换或移除插件
   不会重载 Settings。
4. ~~**产品中由哪个插件提供 `credentials`。**~~ 随该服务一并撤销：每个所有者自己处理密钥（§1.2）。
5. **设置界面的来源发现。** 通用的 `/settings` 页面需要声明的来源及其 schema（例如一个
   `settings.sources` 方法）。S8b 没有需求要求它；随 CLI 设置设计决定。
6. **Rust 来源的仅限用户字段。** 目前没有 Rust 所有者代其工具写自己的来源。若将来有（Scheduler、
   Workflow），它需要在其 `BusinessDocument` 上声明仅限用户的字段。随该所有者的设计决定。

## 11. 验收（流程测试）

App Server 流程测试（`crates/dim-app-server/tests/settings_flow.rs`，以及经 C4 路径加载的
TypeScript 行）：

1. 一个 TypeScript 测试包登记来源 `example`（用户文件、项目文件、带 `default` 的 schema、一个
   仅限用户的指针）。第一次 `read` 返回默认文档，revision 为 `missing`。用该 revision `update`
   保存文档并返回新 revision。过期 revision 以 `conflict` 失败。违反 schema 时失败且不写入。
2. 客户端 `settings.update` 修改仅限用户的字段。所有者随后改动该字段的 `update` 以
   `user_only_field` 失败；保持该字段不变的 `update` 成功。
3. `settings_changes("example")` 上的 Rust 监听器在两条路径的每次成功写入后收到
   `{source, scope, revision}`，被拒绝的写入后收不到。载荷序列化为 §4.4 的 JSON 形态。
4. 原生配置：用户作用域保存在 `user/example` 行；现有 v13 行不变。未授权根上的项目作用域以
   `invalid_scope` 失败。
5. `settings.read` 读 `mcp` 返回 `settings_unknown_source`。行登记 `mcp`、同一来源的第二次登记，或声明
   格式错误，使登记的行失败。来源在所有者登记时出现，所有者卸载后为 `unknown_source`，其文档留在
   home 中。
6. ~~经共享 `credentials` 存储解析凭据引用。~~ 已撤销（§1.2）。
7. 已配对设备的连接得不到 `settings.read`。

## 12. 实现状态

**第 1 部分（Rust 侧，S8b-1）。** 不依赖 C4 已实现：可序列化的事件载荷（§4.4）、带 schema 校验、
默认值和仅限用户字段的声明来源（§3.1–§3.4）、`settings` 服务（§4.2）、`settings.*` 方法（§4.3）。
第 2 部分把声明移到 C4 行之前，声明来自嵌入方单独给的列表。第 1 部分还交付了 `credentials`
服务和 `*Ref` 规则；两者均已移除，Provider 自己的 secret host 已恢复（§1.2）。

对照代码所做的修正（已反映到 §4.3 和 §4.5）：

- 客户端方法由单独的进程部分 `SettingsMethods` 提供，而不是 Settings 本身（方法契约 §3.2；
  ADR 004 D4 的边界）。这是唯一新增的插件行；§2 图中的 `managementMethods settings` 位于这一部分。
  §9 中 ADR 002 D2 的回答对它同样成立，与 PermissionRules 一样：它是 Settings 数据的客户端路径，
  不是接线模块。
- `SettingsError` 除两个指定变体外还有 `InvalidScope` 和 `Document(message)`，使客户端路径能区分
  参数无效与已持久化文档的失败。
- 服务回复的信封与 `sessions` 相同。

§11 验收中由 Rust 流程测试（`settings_flow.rs`、`provider_flow.rs`）覆盖的部分，所有者经 `settings` 服务的 `HostDispatch` 调用：第 1–5 条和第 7 条。

**第 2 部分（S8b-2）。** 已在 C4 路径上实现：

- 经 C4 行的 TypeScript 所有者路径。（声明最初是行的 `settings` 成员；第 4 部分改为登记。）
- `transport_flow/settings_methods.rs` 在 Node 运行时中运行一个 TypeScript 所有者
  （`runtime/fixtures/settings-owner.*`）。第 1 条（默认文档、CAS、过期修订、无效文档）、第 2 条
  （仅限用户字段：客户端可以改，所有者不可以）和第 3 条（Rust 监听方在任一路径每次成功写入后收到
  一次 §4.4 变更，被拒绝的写入没有）经它的 Session 方法通过，这些方法从 TypeScript 调用
  `settings`。客户端路径（§4.3、第 7 条）和登记边界（第 5 条：Rust 来源名、同一来源的第二次登记、
  声明格式错误）在同一文件中运行。它们是 `node_` 流程，由 CI 的 Node 步骤运行。

**第 3 部分（S8b-3）。** TypeScript 部分监听 `settings/<source>` 和 `settings/projects`
（§4.4，§10 第 2 条）。`transport_flow/settings_methods.rs` 检查所有者 fixture 的 `ctx.on` 监听按提交
顺序收到第 3 条的三次 `example` 变更，以及该部分加载后才创建的项目的授权。

**第 4 部分（PluginManager M1，R1）。** 来源在运行时登记（§3.1、§4.2、§4.5）：行的 `settings` 成员、
`SettingsPlugin::reserve`/`declare` 以及首次使用时认领 Rust 名字的做法已移除。Rust 所有者和
`runtime/plugins` 下的 TypeScript 插件在各自的进程行中登记来源。`settings_flow.rs` 检查来源在登记时
出现、所有者卸载后为 `unknown_source` 而文档仍在，以及无效或冲突的登记使登记的插件失败。

S8b 没有遗留未完成的部分。
