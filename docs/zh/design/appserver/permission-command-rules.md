# Permission 命令规则

状态：已实现（实现中发现的修正见 [§12](#12-实现说明)）。路线图项 F2（[#73](https://github.com/arcships/dim/issues/73)）。
总体设计：[§3 命令规则行](../overview.md#3-基于现有机制的集成)与
[附录 B.1 Permission 行](../overview.md#b1-session-与分支-capability)。作用域：
[ADR 002](../../adr/002-plugin-scope.md)（对现有分支级 Permission 的补充，存储归 App Server 内的所有者）与
[ADR 003](../../adr/003-approval-scope.md)（只管 agent 决策）。

## 1. 摘要

用户在一个 Settings 业务来源 `permission` 里维护一份命令规则列表，只有用户作用域。每条规则由命令、
匹配方式（`exact` 或 `prefix`，按词比较）和决定（`allow` 或 `deny`）组成。Permission 每次判定
"启动进程的工具"（`risk: "execute_process"`）且输入带字符串 `command` 的调用时读取这份列表。
不缓存、不重载、不订阅。

- 命中的 `deny` 规则在任何预设和显式工具审批选择下都拒绝该调用。
- 命中的 `allow` 规则只回答 Approval 本来要问的问题：把 `Ask` 变成 `Allow`，绝不把策略的 `Deny`
  变成 `Allow`，也不绕过文件检查。
- `allow` 只匹配简单命令（不含 shell 分隔、重定向或替换）；`deny` 对命令被分隔后的每一段检查。

规则有两条写入途径，都经同一个所有者：客户端管理方法 `permission.rules` 与 `permission.set_rules`
（以 Settings 修订号做 CAS）；"总是允许"也由此实现：客户端先 `set_rules` 写入，再发普通的
`approval.respond allow`。Approval 的事件和方法不变。

命令规则不新增 Timeline 事件或策略字段，也不改变 Timeline、Dispatcher、rutis、Settings 或装配机制。
独立策略字段属于 [Permission 合同](../../../architecture/rust-agent/plugins/Permission.md#policy-contract)
（[ADR 017](../../../adr/017-independent-permission-controls.md)），不属于此规则来源。

### 1.1 不做什么

- 不做项目作用域规则、按工作区的规则、模式或正则、shell 解析器、`ask` 决定。每一项都已暂缓
  （[§10](#10-决定)）。
- 不为其他工具设规则。Session 策略里的 `allowedTools`/`disallowedTools` 已覆盖整个工具。
- 不改执行 Shell 设置。功能清单把"执行 Shell"与命令规则写在一起，但它是 HostPlatform 的业务配置
  （ADR 002"现有插件"）。
- 不是沙箱。与预设一样，规则是准入逻辑；隔离是 HostPlatform 的另一项工作。

## 2. 结构

```
 进程作用域（App Server）
 +---------------------------------------------------------------+
 |  Settings（不变）：来源 `permission`，用户文件                 |
 |    permission.json / 原生行 user/permission                    |
 |  PermissionRules（进程部分，所有者行 `permission`）            |
 |    managementMethods：permission.rules、permission.set_rules   |
 |    --read/update(CAS)--> Settings                             |
 +---------------------------------------^-----------------------+
                                         | read（每次调用）
 Session 作用域                          |
 |  CommandRules（Session 部分，App Server）提供                  |
 |    commandRules（Rust：CommandRulesHost）--read--> Settings    |
 |  （mock 与测试配置：defaults 绑定固定的空宿主）                |
 +---------------------------------------^-----------------------+
                                         | Keyed<CommandRulesHost>
 分支作用域                              |
 |  ToolExecutor --admit--> Approval --decide(call, risk)-->      |
 |                                      Permission（补充）        |
 +---------------------------------------------------------------+
```

依赖指向核心：Permission（核心，dim-agent-capabilities）只依赖 capabilities crate 自己定义的
Session 端口 `CommandRulesHost`。App Server 用自己的 Settings 来源实现该端口；capabilities 不点名
App Server 或 Settings。没有可选依赖：每个装载 Permission 的配置都恰好绑定一个规则宿主（§4.2）。

## 3. 数据与状态

### 3.1 Settings 来源 `permission`

一个文档，只有用户作用域（没有项目文件）：

```json
{
  "commandRules": [
    { "command": "cargo test", "match": "prefix", "decision": "allow" },
    { "command": "git push",   "match": "prefix", "decision": "deny" }
  ]
}
```

| 字段 | 含义 |
| --- | --- |
| `command` | 规则的词：按空白拆分后的字符串。至少一个词。不得含分隔字符（§5.1），否则永远不会命中 |
| `match` | `exact`：命令的词与规则的词相同。`prefix`：规则的词是命令开头的词（`git status` 命中 `git status -s`，不命中 `git statusx`） |
| `decision` | `allow` 或 `deny` |

校验在 Settings 边界进行（`BusinessDocument::validate`），写入时和读取持久化文件时都执行；文件不存在
即默认文档（无规则）。列表顺序没有含义：deny 优先于 allow（§5.2）。重复项无害，不拒绝。

存储：普通配置下是文件来源，v13 `settings_documents` 契约下新增原生 `user/permission` 行，与其他新原生来源一样
（总体设计 §6.4）。现有行格式不变。

### 3.2 运行时状态

无。Permission 保留现有的 `PermissionState`；规则不属于它，不出现在 `session.permission.get`，
也从不进入 Timeline（总体设计 §6.4"安全设置"）。重放、恢复和 fork 既不恢复也不放宽规则。

## 4. 接口

### 4.1 语言中立（ADR 002 D5）

**Session 服务 `commandRules`**，Session 作用域，无实例键，由配置提供。

| 方法 | 方式 | 请求 | 回复 |
| --- | --- | --- | --- |
| `read` | async | `{}` | `{rules: CommandRule[]}` |

`CommandRule = {command: string, match: "exact" | "prefix", decision: "allow" | "deny"}`。错误为
数据：`unavailable`（来源无法读取，包括持久化文件无效）、`cancelled`（调用方放弃调用或提供方正在卸载）。

**管理方法**，命名空间 `permission`，由进程部分注册到 `managementMethods`（总体设计 §6.2 第 9 条；
从不提供给配对设备）。

| 方法 | 请求 | 回复 | 错误 |
| --- | --- | --- | --- |
| `permission.rules` | `{}` | `{revision, commandRules}` | `permission_rules_failed` |
| `permission.set_rules` | `{expectedRevision, commandRules}` | `{revision}` | 参数无效（校验）、`permission_rules_conflict`（修订号过期）、`permission_rules_failed` |

`set_rules` 整体替换列表。回复中的 `revision` 是 Settings 修订号。Settings 照常为这次写入发出现有的
`settings.changed` 事件；F2 不新增监听。

**Permission 判定**（现有分支服务）：判定改为接收整个调用，而不只是名字。结果不变（`Allow`、
`Ask {reason}`、`Deny {reason}`）；读取规则时被取消则报告为取消。

### 4.2 Rust 绑定

在 `dim-agent-types::permission` 中，紧挨 `PermissionPolicy`（一个类型，端口和 App Server 文档共用）：

```rust
#[serde(rename_all = "camelCase")] pub enum CommandMatch { Exact, Prefix }
#[serde(rename_all = "camelCase")] pub enum CommandRuleDecision { Allow, Deny }
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRule { pub command: String,
                         #[serde(rename = "match")] pub matching: CommandMatch,
                         pub decision: CommandRuleDecision }
```

在 `dim-agent-capabilities::permission` 中：

```rust
pub trait ReadCommandRules: Send + Sync + 'static {
    fn rules(&self, cancel: CancellationToken)
        -> HostFuture<'_, Vec<CommandRule>>;   // Result<_, HostError>：Failed | Cancelled
}
pub struct CommandRulesHost(pub Arc<dyn ReadCommandRules>);       // Session 值
pub struct NoCommandRules;                                         // 固定的空宿主

impl Permission {
    pub async fn decide(&self, call: &ApprovalCallData, risk: Option<&str>,
                        cancel: &CancellationToken) -> Result<Decision, HostError>;
}
```

`PermissionPlugin::Deps` 新增 `Keyed<CommandRulesHost>`（硬依赖）。`Approval::admit` 传入它已持有
的调用，并向上传递被取消的读取；它现有的两处 `decide`（首次判定和允许回复后的复核）其余不变。

各配置的绑定（每个配置恰好一个）：

| 配置 | 绑定 |
| --- | --- |
| `defaults::install`、`install_with_prompt`、`install_with_process_model`（App Server mock 配置与测试宿主） | `install_profile` 用 `session_value` 绑定固定的空宿主 `NoCommandRules`，与它已绑定的 `EmptyPromptHost` 并列。现有测试不变 |
| `install_with_prompt_capability`、`install_local_execution`、`install_minimal_execution` | 与 `PromptHost` 一样不绑定：由安装它们的宿主绑定端口。只用 capabilities 的宿主（集成测试、`xtask` 本地图）绑定 `CommandRulesHost(Arc::new(NoCommandRules))` |
| App Server 产品配置（`main.rs` 中的 `install_local_execution`、其测试与 `xtask` App Server 图） | 配置调用 `permission_rules::install(&mut builder)`，加入功能 `CommandRules` 并要求它的 Session 部分 `CommandRules`（`require_feature`，Deps `Keyed<SettingsService>`），它提供的 `CommandRulesHost` 每次调用都读取 `SettingsService::read::<PermissionSettings>(User)`。`serve` 在本地执行时注册进程部分 `PermissionRulesPlugin`（`register_part`，Deps `Keyed<SettingsService>` 与 `Keyed<ManagementMethods>`，所有者行 `permission`），与 `mcp`、`skills` 并列，由它提供管理方法。两者与文档类型 `PermissionSettings`（`NAMESPACE = "permission"`、`USER_FILE = "permission.json"`、无 `PROJECT_FILE`）同在 `dim-app-server/src/permission_rules.rs` |

## 5. 流程

### 5.1 匹配

命令是调用输入的 `command` 字符串，且只针对定义中声明 `risk: "execute_process"` 的工具。目前是
WorkspaceTools 的 `exec` 和 ShellTools 的 `execute`，两者都已接收 `command`。Permission 不点名任何
工具。其他调用从不读取规则。

- 词：按空白拆分；不解释引号。
- 分隔字符：`;` `&` `|` `` ` `` `(` `)` `<` `>` `$` 以及换行/回车。
- **简单命令**：不含任何分隔字符。只有简单命令能命中 `allow` 规则。`cargo test; rm -rf x` 或
  `cargo test > out` 不会命中 `allow cargo test`，回落到预设（通常是一次审批提问）。
- **段**：在每个分隔字符处切开命令。`deny` 规则命中任意一段即命中，所以 `echo ok && git push` 和
  `x=$(git push)` 都命中 `deny git push`。切分不理会引号，引号里的分隔字符可能造成误拒；这是失败即
  关闭，理由里写明规则。

这是字符检查，不是 shell 解析器，针对的是模型常用的命令形式。它看不穿 `sh -c`、别名、环境变量前缀
（`A=1 git push`）、绝对路径或脚本；这些只能靠预设或 OS 隔离拒绝。

### 5.2 判定顺序

```
 decide(call, risk)
  1. call.name 在 disallowedTools 中                -> Deny   （现有）
  2. 是命令调用？读取规则（每次都读）
       读取失败                                    -> Deny "Command rules could not be read: <error>"
       某条 deny 规则命中某一段                    -> Deny "Command denied by command rule `<command>` (<match>)"
  3. 文件调用的文件范围闸门                        -> Deny | Ask，或继续
  4. call.name 在 allowedTools 中                   -> Allow
  5. 显式 toolApproval，否则使用旧风险判定          -> Allow | Ask | Deny
  6. 工具结果为 Ask 且 allow 规则命中该简单命令      -> Allow
```

文件范围和工具询问合并为一次针对本次调用的审批。命令 allow 只回答工具问题；文件闸门和隔离不属于命令规则策略。

| 旧预设（无显式工具覆盖） | `execute_process` | 命中 allow 规则 | 命中 deny 规则 |
| --- | --- | --- | --- |
| read-only | Deny | Deny | Deny |
| workspace-write | Ask | Allow | Deny |
| full-access | Allow | Allow | Deny |

allow 规则不能解除旧风险拒绝。显式 `toolApproval: "ask"` 下，无论种子预设为何，命令都询问，
匹配的 allow 回答此问题。显式 `"bypass"` 下命令无需工具询问，但 deny 规则仍优先。
两者都不改变文件访问或进程隔离。读取失败按关闭处理，因为可能存在 deny 规则。

### 5.3 准入

```
 ToolExecutor       Approval                     Permission           CommandRulesHost
    | admit(call) --->| decide(call, risk) ------->| rules() ----------->| 读用户来源
    |                 |<-- Allow | Ask | Deny -----|<-- rules ----------|
    |<- Ok（执行）    |   Allow                    |
    |<- 错误结果      |   Deny：content = 理由（现有 HostError::failed 路径）
    |                 |   Ask：approval/request ... approval/respond allow
    |                 | 再次 decide(call, risk) ---> 重新读取规则
    |                 |   此时 Deny：approval/cancelled {reason}，错误结果
```

允许回复后的复核本来就有；加上规则后，它也能看到提问期间新写入的 deny 规则。

### 5.4 "总是允许"（客户端流程）

由 CLI 权限控制交付（[#257](https://github.com/arcships/dim/issues/257)；[CLI 设计](../cli-commands.md#总是允许)）。

```
 客户端                         PermissionRules             Approval
   | approval.request {call: exec, command "cargo test -p x"}（展示）
   | 用户选择"总是"
   | permission.rules ------------>| {revision r1, rules}
   | permission.set_rules(r1, rules + {command, exact, allow}) -> {revision r2}
   |   （冲突：重新读取后重试）
   | approval.respond allow ------------------------------------>| 复核，调用执行
```

客户端根据 `call.input.command` 拟出规则（默认 `exact`；用户可以缩短为前缀）。Approval 和
`approval/respond` 不变，所以回答与规则是两次独立的写入：规则写入失败时，客户端仍可以只允许这一次。
非简单命令不能成为 allow 规则（§3.1），所以客户端只对简单命令提供"总是"。

### 5.5 模型看到什么

没有新内容。工具定义和提示词不提规则。被允许的调用照常执行并返回正常结果。被拒绝的调用返回错误
结果，其 `content` 是理由，与现在的预设拒绝一样，例如
`Command denied by command rule \`git push\` (prefix)`。规则不会列给模型。

## 6. 错误

| 情况 | 结果 |
| --- | --- |
| 规则来源无法读取或持久化文件无效 | 命令调用被拒绝，理由为 "Command rules could not be read: …"；其他调用不受影响。`permission.rules` 返回 `permission_rules_failed` |
| 管理调用被中断（所有者卸载） | 与 MCP 一样返回 `operation_interrupted`；写入可能已经提交，重试前先读取 |
| 读取被取消（批次取消、Session 关闭） | 准入以取消结束，与现在相同 |
| `set_rules` 含无效规则（空、含分隔字符、未知字段） | 参数无效；不写入 |
| `set_rules` 修订号过期 | `permission_rules_conflict`；重新读取后重试 |
| Settings 已关闭或正在重载 | 管理调用与其他所有者一样报告重载中；准入拒绝（读取失败） |

## 7. 安全

- 规则只管 agent 的工具调用（ADR 003 D1）。用户命令（`!command`、ShellInput）和按配置运行的命令
  （MCP stdio、Hooks、工作流）不受影响。
- 只有用户作用域，因此项目文件无法给自己授予 allow 规则。项目作用域已暂缓（§10）。
- 规则是宿主安全设置：从不进入 Timeline，重放或 fork 从不恢复或放宽，配对设备不能写入（管理方法
  只在本地提供）。
- allow 规则不能解除策略拒绝或文件检查；deny 规则在所有工具审批选择下生效；读取失败即拒绝。
- 匹配对 `allow` 刻意保守，对 `deny` 刻意宽泛；§5.1 写明了它的边界。它不是隔离。

## 8. 与现有部分的关系

| 部分 | 关系 |
| --- | --- |
| Permission | 补充：`decide` 接收调用，对命令调用读取规则；Session 依赖 `CommandRulesHost`。规则不增加策略字段，不改变策略版本或方法；独立控件遵循 Permission 自身的合同 |
| Approval | 在现有两处把调用传给 `decide`，并传递被取消的读取。事件、`approval.respond`、重放和恢复不变 |
| ToolExecutor、WorkspaceTools、ShellTools | 不变；`exec` 与 `execute` 已声明 `execute_process` 并接收 `command` |
| Settings | 不变；新增一个来源 `permission`（用户文件、原生行）。它的 `settings.changed` 与任何来源一样照常发出；F2 不监听 |
| Timeline、Dispatcher、rutis、装配 | 不变 |
| CLI `/permissions` | CLI 拥有经 `permission.rules`/`permission.set_rules` 列出、添加、删除规则的面板，以及审批提示中简单 `exec`/`execute` 命令的"总是"选项（§5.4）。`/approvals` 和 `/approval` 为兼容别名（[CLI 设计](../cli-commands.md#权限控制)） |
| HostPlatform | 执行 Shell 仍是它自己的业务配置；不属于 F2 |
| RemoteGateway | 配对设备可以回答审批，但不能读写规则 |
| Collaboration、Scheduler、Workflow | 规则是宿主级的，在每个 Session 中生效；由于 allow 只回答 `Ask`，上限为 read-only 的自动化 Session 不会被放宽 |
| Hooks（pre-tool） | 相互独立：Hooks 建议在命令规则之后、且仅当其未拒绝时才被询问（[Hooks A.6.1](../plugins/Hooks.md#a61-permission-的工具建议)） |

新增公开内容：Session 端口 `commandRules`（Rust `CommandRulesHost`）、`CommandRule` 类型、
`permission` 管理命名空间（两个方法）以及 `permission` Settings 来源。每一项都是存储规则、客户端
写入规则、Permission 读取规则所必需的。

## 9. 规则检查

| 规则 | 回答 |
| --- | --- |
| 设计规则 §1（唯一事件总线） | 不新增订阅、watch 通道或事件；规则在使用时读取。Settings 现有的 `settings.changed` 不变 |
| 设计规则 §3（依赖方向） | Permission（核心）依赖 capabilities crate 定义的端口；App Server 实现它。没有核心部件依赖可选插件 |
| §4 第 1 条（指令事件） | 不适用：规则是进程级配置，经所有者的类型化写操作写入（总体设计 §6.2 第 2 条） |
| §4 第 2 条（交接） | 不变；准入仍在工具启动之前 |
| §4 第 3 条（before 回调） | 不使用 |
| §4 第 4 条（核心不带业务含义） | Timeline 与 Dispatcher 不变；Permission 按声明的风险与 `command` 字段匹配，不点名工具 |
| §4 第 5 条（批次） | 没有新写入 |
| §4 第 6 条（上下文） | 模型只看到现有的拒绝结果 |
| §4 第 7 条（只经公开接口） | Permission 经自己声明的端口读取规则；Approval 使用 Permission 公开的 `decide` |
| §4 第 8 条（基础设施不变） | 不改 Timeline、Dispatcher、事件信封、rutis、loader、Agent 装配或 Session 门面。不提需求 |
| §4 第 9 条（只有需要的事实进 Timeline） | 没有事件；规则不进 Timeline |
| ADR 002 D1（作用域） | 规则是宿主级的：存储与管理在进程级；读取端口在 Session 级；判定留在分支级 Permission |
| ADR 002 D2（不设接线模块） | 没有注册表或聚合；Session 部分是规则所有者对 Permission 端口的绑定，与 `DefaultPrepareModelPlugin` 绑定 `PrepareModelHost` 相同 |
| ADR 002 D3（依赖） | 全部是硬依赖；每个配置绑定一个宿主 |
| ADR 002 D4（配置） | 业务配置放在所有者的 Settings 来源，使用时读取，不重载 |
| ADR 002 D5（语言中立） | §4.1 是合同；§4.2 是一种绑定 |
| ADR 003 | 只作用于 agent 工具调用；没有项目条目能授予权限 |

向机制所有者提出的需求：无。

## 10. 决定

维护者已决定：

1. **项目规则：** 暂缓，现在不需要。只有用户作用域规则，没有项目文件。
2. **按工作区的用户规则：** 暂缓，现在不需要。
3. **通配或 shell 解析器：** 暂缓，现在不需要。保持按词的 `exact`/`prefix` 加分隔字符检查（§5.1）。
4. **`ask` 规则：** 暂缓，现在不需要。
5. **专用方法与 S8b 的 `settings.read`/`settings.update`：** 在设计 S8b 时决定；F2 保留
   `permission.rules`/`permission.set_rules`。
6. **绑定：** 按配置在 Session 级绑定端口 `Keyed<CommandRulesHost>`（§4.2），不依赖进程服务；
   现有测试不变。
7. **范围：** F2 只做服务端：规则来源、Permission 检查、管理方法和流程测试。CLI `/permissions` 面板和
   "总是"选项属于 F6（CLI），作为它承接的需求（§5.4、§8）；CLI 已在 [#257](https://github.com/arcships/dim/issues/257) 交付。

## 11. 验收（流程测试）

Permission 流程（`crates/dim-agent-capabilities/tests/permission_flow.rs`，测试宿主的规则由测试在两次
调用之间修改）：

1. workspace-write，规则 `allow cargo test (prefix)`：`exec` 执行 `cargo test -p x` 时不产生
   `approval/request`；`cargo test; rm x` 与 `cargo test > out` 产生审批请求。
2. 规则 `deny git push (prefix)`：full-access 下 `git push origin` 被拒绝，错误内容包含该规则；
   `echo ok && git push` 被拒绝；工具没有执行。
3. 使用时读取：同一 Session 内两次调用，中间修改规则，不重载：第二次调用遵循新规则。
4. read-only 加 allow 规则：调用仍被拒绝。
5. 复核：审批挂起期间加入 deny 规则；允许回复以携带该规则理由的 `approval/cancelled` 结束，工具没有执行。
6. 读取失败时命令调用被拒绝；同一轮中的非命令工具不受影响。

App Server 流程（`crates/dim-app-server/tests/`）：

7. `permission.set_rules` 写入一条规则；已打开的 Session 中下一次 `exec` 遵循它。修订号过期返回
   `permission_rules_conflict`；含 `;` 的规则被拒绝；工作区里的 `.dimcode/permission.json` 不起作用。

由 `crates/dim-agent-capabilities/tests/permission_flow.rs`（1–6）与
`crates/dim-app-server/tests/transport_flow/permission_rules.rs`（7）实现。

## 12. 实现说明

对照代码时发现的草案修正；上文各节已按此表述。

1. **绑定（§4.2）。** 草案只列了 `defaults` 配置和“带 Settings 的 App Server”。
   `install_with_prompt_capability`、`install_local_execution` 与 `install_minimal_execution` 不绑定
   宿主（正如它们不绑定 `PromptHost`），由调用方绑定：App Server 产品配置调用
   `permission_rules::install`，只用 capabilities 的宿主绑定 `NoCommandRules`。`serve` 不能自行安装
   Session 部分，因为有的 App Server 测试把产品 home 与 `defaults::install` 搭配，已经绑定了空宿主。
   进程部分在本地执行时注册（规则在那里被使用），而不是只要有产品 home 就注册。
2. **理由文本（§5.2、§6）。** 拒绝理由为 "Command rules could not be read: <error>" 与
   "Command denied by command rule `<command>` (<match>)"，与 §5.5 及首字母大写的预设理由一致。
3. **复核时的取消（§5.3）。** 允许回复后的复核中若规则读取被取消，请求以 `approval/cancelled`
   （"Tool execution cancelled"）结束并返回取消，与该时刻批次被停止相同。
4. **端口的 future 类型（§4.2）。** `HostFuture<'_, T>` 是现有的
   `BoxFuture<'_, Result<T, HostError>>` 别名。
5. **策略快照（§5.2，评审）。** 规则读取是一次判定中唯一的等待；策略（`disallowedTools`、
   `allowedTools`、预设）在读取之后获取，因此读取等待期间分发的 `permission/changed` 决定该调用，
   首次判定与复核都是如此。
