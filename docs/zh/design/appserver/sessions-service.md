# Agents `sessions` 服务

状态：已在 S2 实现（[#98](https://github.com/arcships/dim/pull/98)）；`sessions.delete`（§6.8）在 S5c（[#97](https://github.com/arcships/dim/pull/97)）合入后加入。依据 spike 结论（§2）；实现过程中的设计变更见 §17。路线图工作项 S2（[#62](https://github.com/arcships/dim/issues/62)），在 C2（[#48](https://github.com/arcships/dim/issues/48)，已合入）与 S1（[#58](https://github.com/arcships/dim/issues/58)，已在 [#94](https://github.com/arcships/dim/pull/94) 合入）之后。合同：[Session 访问 A.1](../contracts/session-access.md#a1-sessions-服务)。总览：[附录 B.2](../overview.md#b2-app-server-进程插件) Agents 行。作用域：[ADR 002](../../adr/002-plugin-scope.md) 的进程作用域，放在现有的 Agents 插件内。使用方：[Scheduler](../plugins/Scheduler.md)（P6）与 [Workflow](../plugins/Workflow.md)（P7），都用 TypeScript。

Agents 的稳定目录还提供生成的只读服务 `sessionPermissions`：`permission({sessionId, branchId})` 返回当前分发后的 `PermissionState`，`permissionDefaults({})` 返回宿主当前权限上限。这些调用直接返回值或抛出生成的 `RustError`；`sessions` 保留现有回复信封。见[原生访问 §2](../../../design/p6-p7-bindings.md#2-read-only-permission-access)。

§5、§6.2 与 §8.3–8.4 的输入所有权沿用已接受的[原生访问合同](../../../design/p6-p7-bindings.md#4-native-report-settlement)：已准入输入等待自身追加/发布结果，分支对每个 `submissionId` 只接受一次，因此回复丢失的调用方重新提交，而不等待旧写入者。

## 1. 概要

Agents 提供进程服务 `sessions`，让任何语言的进程插件都能像 Collaboration 那样启动和驱动 Session。方法为 `launch`、`input`、`lookup`、`history`、`withdraw`、`stop`、`idle` 和 `delete`，每个都映射到 Agents 现有的 Session 创建与准入（`AgentIndex`）和公共 Session 门面。`withdraw` 转交给 S1 已加入的 InputQueue 撤回服务，`delete` 转交给 S5c 的 `AgentIndex::delete`。

- **形态。** 一个具名进程服务 `sessions`，没有实例 key。它是 `host_key("sessions")` 上的 `dyn HostDispatch`，像 `pluginFiles` 一样经服务目录跨语言共享。由 Agents 中从不重载的目录部分提供，每次调用时查找 Agents 索引，因此使用方从不随索引重载（#35）。
- **启动。** `launch` 创建非交互 Session。它的权限策略是请求的策略，以启动方分支的当前策略为上限；当前策略即由 `permission/changed` 事实折叠出的 `PermissionState`。没有启动方时，上限是宿主的 Session 默认值。Agents 在创建前经 `SessionConfig.permission_policy` 施加上限。
- **输入。** `input` 经 `Session::input` 写入 `input/message`，事件发布后才回复。分支对每个 `submissionId` 只接受一次：重复提交不写入，回复第一条输入的回执。按 `submissionId` 的 `lookup` 读同一个索引，不写入。
- **空闲。** `idle` 就是门面的 `Session::idle`：Loop 没有未结束的 turn，没有等待发布的事件，分发已追到尾部。待处理的审批或提问会让 turn 保持未结束，因此调用方用 `stop` 给等待设上限。
- **撤回。** `withdraw` 转交给 S1 的分支撤回服务，语义归 S1；删除成功时，与 `input` 一样等 `input/deleted` 发布后才回复。
- **生命周期。** Agents 像拥有其他 Session 一样拥有启动的 Session；`sessions` 既不关闭也不卸载它们。已装载的 Session 曾会累积（§2 第 6 条），空闲卸载（[#126](https://github.com/arcships/dim/issues/126)）之后，Agents 卸载在宽限期内没有被任何租约持有的 Session，下一次调用恢复它（§8.6）。存储中的 Session 同样会累积，因此使用方在删掉一次运行的记录时删除该次运行的 Session，经由转交给 S5c 删除的 `sessions.delete`（§6.8、§8.7）。

不新增插件、Timeline 事件，不改存储格式、运行时或门面，不向 rutis 提需求。

## 2. Spike 结论

S2 是 L 级工作项，因此先做 spike（[路线图 §5.4](../../../design/roadmap.md#54-容量与重排)），基于 `main` 的 `3cca153`。临时测试与测量做完即删除，没有提交任何相关代码。

| # | 问题 | 检查方式 | 结论 | 对设计的影响 |
| --- | --- | --- | --- | --- |
| 1 | 启动方的当前策略在哪里？ | 测试：以 full access 创建启动方，再用 `Permission::update` 收窄 | `Session::config().permission_policy` 仍是 full access：它是创建时的初始值。`Session::get::<PermissionState>(branch)` 读到已分发的 workspace-write 策略。以收窄后的 `SessionConfig.permission_policy` 创建的 Session，读回的 `PermissionState` 就是它，版本为 0 | 上限从 `PermissionState` 读取，在创建时施加；运行时不变 |
| 2 | 两个策略能取上限吗？ | `PermissionPreset` 没有顺序，也没有求两个策略交集的操作。用 Permission 自己的 `decide` 对 §6.1 的规则做临时穷举检查：3 × 3 个预设、16 种列表归属、2 个工具名、7 种风险，共 2016 种情况 | 取上限后的策略从不比任一输入更宽。2006 种情况等于逐点取最小。其余 10 种是某工具只出现在预设较低一方的 `allowedTools` 中：逐点最小会询问或允许，上限规则拒绝 | 采用 §6.1 的上限规则，有意保守 |
| 3 | 刚写入的输入还没运行时，`idle` 会返回吗？ | 代码：`TimelineContext::idle` 等待 Loop `Idle`、没有待发布事件、分发追到尾部。InputQueue 采纳输入、Loop 启动 turn，都发生在前一个事件的分发过程中。测试，运行 40 次：三条 Queue 输入后调用 `idle` | 不会。每条输入都在截止点或之前被消费，随后有 `turn/end`。分支空闲时调用 `idle` 立即返回同一个截止点。Append 输入在截止点已被采纳但未被消费 | `idle` 就是门面调用，只与关闭和重载竞争（§6.4） |
| 4 | 待处理的审批对 `idle` 有什么影响？ | 测试：非交互的 workspace-write Session，模型调用 `exec` | 写入了 `approval/request`，500 ms 后 `idle` 仍未返回。`stop` 结束 turn 后 `idle` 返回；`exec` 从未运行。没有任何部件为审批读取 `interactive` | 审批让 `idle` 保持等待；调用方用 `stop` 给运行设上限（§6.4） |
| 5 | TypeScript 使用方怎样访问 Rust 服务？ | `pluginFiles` 先例与 rutis 0.10 源码 | `host_key(name)` 上的 `dyn HostDispatch` 加上 `ServiceCatalog::register_shared(name)`，JavaScript 行就能看到它。`HostDispatch::invoke(method, args)` 不携带调用方身份。JavaScript 插件 API 只有 `inject`，是硬依赖，没有按次查找。rutis 0.10 不会把 JavaScript 的 `AbortSignal` 带进 Rust 调用（[App Server README](../../../../crates/dim-app-server/README.md#trusted-plugin-storage)） | `sessions` 由目录部分提供（§5）；A.1 删去“由 Agents 确定来源”；JavaScript 侧的取消提给 C4（§14） |
| 6 | 一个已装载的 Session 成本多少？ | `xtask measure`，release 构建，mock 模型，Apple M4 | 每个已装载的空闲 Session 约 300 KB Rust 分配、43 个 Tokio 任务。已装载 10 个时平均创建耗时 1.5 ms，100 个时 4.0 ms，1000 个时 57 ms。App Server 从不卸载空闲 Session（[#57](https://github.com/arcships/dim/issues/57)） | 启动的 Session 会累积到重启为止；空闲卸载作为后续项提出（§8.6） |
| 7 | 读者何时能看到一条输入？ | 代码：`Session::input` 在提交时返回，早于发布；协议 v3 的 `session.submit` 会等待发布 | `Session::input` 一返回就读，调用方可能读不到自己的输入 | `input` 在发布后回复；`lookup` 先等待待发布事件（§6.2、§6.3） |
| 8 | 按 `submissionId` 查找在哪里？ | 代码 | 只在 Rpc：`session.input_lookup` 扫描已发布的页，O(n) | 由 Agents 持有唯一的查找实现，Rpc 调用它（§6.3） |

spike 改变了三处设计：服务由谁提供（第 5 条）、生命周期后续项（第 6 条），以及 §12 的合同细化。

## 3. 决定

| # | 决定 | 否决的方案 | 理由 |
| --- | --- | --- | --- |
| D1 | 由 Agents 中从不重载的目录部分提供 `sessions`，每次调用时查找 Agents 索引 | 由索引插件提供，或等待 C4 提供按次查找 | SessionStore、Settings 或 SQLite 存储重载时索引都会重载，注入该服务的每个使用方都会随之重载，违背 #35。rutis 0.10 的 JavaScript API 无法按次查找，而目录部分用现有机制就能达到同样效果 |
| D2 | 只在 `AgentIndex` 上实现一次；具名服务只负责解码请求、编码回复 | 单独的 `SessionsPlugin` | 那是接线模块（ADR 002 D2） |
| D3 | 上限取较低的预设、`disallowedTools` 的并集，并且只保留双方都无条件允许的 `allowedTools` 条目 | 只取较低的预设；拒绝比上限更宽的请求 | 只取预设时，请求的 `allowedTools` 条目可能越过上限，不安全。合同规定对更宽的请求取上限，而不是拒绝 |
| D4 | 没有启动方时，上限是宿主的 Session 默认值（`ServerConfig.session_defaults.permission_policy`） | 新增一个自动化专用的宿主设置 | 它是宿主现有的答案：没人选择策略时 Session 能做什么。不增加任何配置，以后需要单独设置时可以增量添加，不破坏任何东西 |
| D5 | 读不到启动方时启动失败：启动方已不存在时为 `session_closed` 或 `branch_missing`，无法恢复时为 `failed`，其 Permission 重载期间为 `reloading` | 退回到宿主上限 | 失败即关闭：宿主上限可能比启动方的策略更宽 |
| D6 | `launch` 回复 `{sessionId, policy}`，带生效策略 | 只回复 `{sessionId}` | 调用方不必再读一次，就能记录一次运行得到的权限，并看出上限是否生效 |
| D7 | `LaunchRequest.model` 为 `{providerId, modelId}` | 单独的模型 id 字符串 | 模型 id 只在其 provider 内唯一。`{providerId, modelId}` 即 `ModelId`，Agent profile 与 `session.create` 的显式模型都用这个类型 |
| D8 | `input` 在发布后回复 | 提交时回复 | 回复后立即调用 `history {atLeast: sequence}` 必须成功（第 7 条） |
| D9 | `lookup` 先等待待发布事件再扫描；Rpc 的 `session.input_lookup` 使用同一份代码 | 立即扫描；另写一个扫描器 | 已提交但尚未发布的输入会读成 `unknown`，Scheduler 就会把一条正在运行的输入记为 `lost` |
| D10 | `idle` 就是门面的 `Session::idle`，S2 沿用现状：审批待处理时 `idle` 一直等待，调用方用 `stop` 结束等待。它不带超时 | 返回“阻塞”状态、由 S2 决定非交互 Session 中的询问怎么处理（例如直接拒绝），或 `idle {timeoutMs}` | 非交互 Session 中的询问怎么处理由 Approval 决定，也和 Questions 的重新设计相关。它还没有设计，S2 不预设结论：无论 Approval 怎样处理，`idle` 都跟随 turn。调用方本来就有自己的截止时间，并用 `stop` 结束运行 |
| D11 | `history` 用 `after`、`limit` 和固定的 `through` 分页读取该分支自己的事件，返回 `AgentEvent` 记录 | 一次回复包含祖先在内的全部历史 | 这样的回复跨桥接时没有大小上限。每个分支各自编号，一个游标无法跨祖先分页。启动的 Session 都在 `main` 上读取，`main` 没有祖先 |
| D12 | `sessions` 没有 `close` 或 `release`；Agents 拥有启动的 Session | 调用方用完时关闭该次运行的 Session；对释放计数 | 关闭会为该 Session 的所有使用者结束它（设计规则 §4 第 8 条：断开不是关闭）。卸载是宿主的决定，并且必须同样覆盖客户端 Session（§8.6） |
| D13 | 不提供 `sessions.fork` | 随 S2 一起加入 | 没有使用方。从切点创建归 S5c（`AgentIndex::fork`，[#97](https://github.com/arcships/dim/pull/97)） |
| D14 | 启动 Session 的使用方在删掉最后一条指向它的记录时删除它，经由 `sessions.delete`，它转交给 S5c 的 `AgentIndex::delete`（§8.7） | 由用户手动清理；运行结束时就删除其 Session | 每五分钟一个任务，一天就往每个客户端的列表里加约 288 个 Session，而运行记录过期后，没有任何东西再把它的 Session 和任务联系起来。运行记录还在时，这次运行必须仍可查看。使用方的运行保留规则（Scheduler A8、Workflow A9）已经决定了保留多久，因此 Session 数量受它约束，也不会留下没有记录指向的 Session |

## 4. 结构

从使用方到 Session 的调用路径（调用路径，不是装配图）：

```
 Scheduler / Workflow 进程部分                  Rust 进程插件
 （TypeScript 行）                                      |
     | inject 'sessions'                                | 每次使用时 ctx.get::<AgentIndex>()
     v                                                  |
 +----------------------------------------------+       |
 | Agents 目录部分 (AgentDirectoryPlugin)       |       |
 |   在 host_key("sessions") 上提供             |       |
 |   dyn HostDispatch；每次调用：               |       |
 |   ctx.get::<AgentIndex>()，不存在时 reloading |       |
 +----------------------+-----------------------+       |
                        v                               v
 +-----------------------------------------------------------------+
 | AgentIndex：launch -> host(agent)、上限来源、取上限、create_session |
 |             delete -> delete(sessionId)：S5c 的闸门与关闭         |
 |             其他方法 -> acquire(sessionId)：本次调用的租约        |
 +----------------------------+------------------------------------+
                              v
 Session 门面：input、publication、idle、stop、sequence、local_page、
               get::<PermissionState>(branch)、service::<撤回服务>(branch)（S1）
```

| 部分 | 作用域 | 变更 |
| --- | --- | --- |
| `AgentDirectoryPlugin`（[agents.rs](../../../../crates/dim-app-server/src/agents.rs)） | 进程；没有依赖，从不重载 | 提供 `sessions` |
| `AgentIndex`（[agents/sessions.rs](../../../../crates/dim-app-server/src/agents/sessions.rs)） | 进程；随 SessionStore、Settings 与存储重载 | 实现各方法（附录 A），其中 `delete` 即 S5c 的 `AgentIndex::delete`，以及与 Rpc 共用的输入查找 |
| 服务目录（App Server [lib.rs](../../../../crates/dim-app-server/src/lib.rs)） | 进程 | 在 `pluginFiles` 旁加 `register_shared("sessions")`；装配负责线中的一行 |
| Rpc（[connection.rs](../../../../crates/dim-app-server/src/connection.rs)、[connection/tree.rs](../../../../crates/dim-app-server/src/connection/tree.rs)） | 进程 | `session.create` 与 `launch` 共用 cwd 规则；`session.input_lookup` 调用 Agents 的查找 |
| Session 门面、Permission、Loop | Session、分支 | 不变；经公共调用读取 |
| InputQueue 撤回服务 | 分支 | 由 S1 新增 |

## 5. 服务形态

| 项 | 内容 |
| --- | --- |
| 标识 | 名称 `sessions`，进程作用域，只在控制节点，没有实例 key |
| 提供方 | Agents 的目录部分。每次调用查找当前的 `AgentIndex`；没有时调用以 `reloading` 失败。索引重载结束可取消调用，保留须回复自身结果的调用（§8.3），从不让使用方重载（D1） |
| 调用 | 所有方法都是 async。每个方法接收一个请求对象，由 rutis 以 `[request]` 传递，返回一个信封：`{ok: true, value}` 或 `{ok: false, error: SessionsError}`。错误在 `code` 与 `message` 之外带有 `retryable`。未知方法、缺少或多出参数、未知字段或类型错误都回复 `rejected` |
| 错误 | `SessionsError {code, message, retryable}`（§9） |
| 取消 | 使用方放弃调用。丢弃 Rust future 会结束可取消工作并释放租约；被丢弃的 `launch` 仍完成创建（§6.1），已准入 `input` 保留任务与租约（§6.2） |
| Rust 使用方 | 像 Rpc 查找 Provider 那样，每次使用时查找 `AgentIndex`，调用附录 A 的类型化方法；也可以使用具名服务 |
| 核心独立 | Agents、Rpc 和 Session 装配从不依赖 `sessions` 的使用方。Rpc 不使用 `sessions` |
| 暴露范围 | 只限控制节点进程内的插件。`sessions` 不是 Rpc 方法，从不提供给客户端，也从不经 rutis 链接导出 |

**input 取消：** §6.2 准入后，任何调用方丢弃结果都不会停止自持 input 任务或提前释放租约。调用方的生命周期与该任务无关：runtime row 卸载时不等待它，下一代重新提交（§8.4）。

## 6. 方法

除 `launch` 与 `delete` 外，每个方法都 acquire 它的 Session（必要时恢复），并持有租约直到回复。结构沿用合同（[A.1](../contracts/session-access.md#a1-sessions-服务)），按 §12 细化。

### 6.1 launch

```text
// LaunchRequest
{ "agent?": "string",                                  // Agent id；缺省：默认 Agent
  "cwd": "string",                                     // 绝对路径
  "model?": { "providerId": "string", "modelId": "string" },
  "policy?": { "preset": "...", "allowedTools": [], "disallowedTools": [] },
  "starter?": { "sessionId": "string", "branchId": "string" } }
// 回复
{ "sessionId": "string", "policy": { /* 生效的 PermissionPolicy */ } }
```

**策略。** 启动方的当前策略是启动方分支的 `PermissionState`：Permission 已分发的状态，由 Session 配置的策略加上该分支及其祖先上的 `permission/changed` 事实折叠而成。Agents 在持有启动方租约时读取一次。

```
                         有启动方？
                  是 /                   \ 否
 上限 = starter.branchId 的                上限 = 宿主 Session 默认值
   PermissionState（启动时读取）             （ServerConfig.session_defaults.permission_policy）
 请求 = policy，否则为上限                 请求 = policy，否则为 read-only
                  \                      /
             生效 = cap(请求, 上限)
                preset      取较低者：read-only < workspace-write < full-access
                disallowed  两个列表的并集
                allowed     双方都无条件允许的工具名（列在各自的 allowedTools 中，
                            或该方为 full-access），去掉 disallowed
```

目前的宿主上限：产品与 API-key 配置为 workspace-write；开发宿主为 read-only，并允许 `echo` 与 `wait`（[main.rs](../../../../crates/dim-app-server/src/main.rs)）。上限从不比任一方更宽（第 2 条）。只列在一方 `allowedTools` 中的工具，按较低的预设决定。列出它的一方预设较低时，这比逐点取最小更严：逐点最小会按另一方的预设决定。

**创建。** 启动组合 Agents 现有的创建步骤，与 Collaboration 创建成员的方式相同：

| 新 Session 的字段 | 取值 |
| --- | --- |
| Agent | `agent`，否则为默认 Agent。未知 id 以 `agent_unknown` 失败 |
| 系统提示词、工具 | Agent profile 覆盖宿主 Session 默认值（`session_config`），与不带覆盖的 `session.create` 相同 |
| `cwd` | 绝对路径、UTF-8、规范化：沿用 `session.create` 的规则，作为同一个检查共用 |
| `model` | 请求的 `model`，否则为 profile 的模型。有模型选择宿主时（产品配置），经 Agents 每次调用查找的宿主（`AgentIndex::model_selection`）解析，与 Collaboration 成员相同；宿主未激活时为 `reloading`。否则用这两个 id 替换默认值 |
| `permissionPolicy` | 生效策略 |
| `interactive` | `false` |
| `collaborationRole` | 默认值（leader） |
| 协议版本 | `PROTOCOL_VERSION`（3）：写入记录，并在创建后用 `insert_version` 登记，与 `session.create` 和 Collaboration 相同。Rpc 只在版本已登记后才服务已装载的 Session，当前客户端也才能列出它。创建与登记在同一个任务中进行，即使调用方不再等待，两者也一起完成 |
| 记录 | 预备创建（`AgentIndex::create_session`）：先项目授权，再写拥有者记录，与 `session.create` 相同 |

错误：`rejected`（结构、`cwd`、`policy` 中的空工具名）、`agent_unknown`、`session_closed` 或 `branch_missing`（启动方）、`reloading`（启动方的 Permission 重载期间也是）、`failed`（模型解析、项目授权、存储、已存储但不可用的 Agent、无法恢复的启动方、Session 构建）。

启动不是幂等的。回复丢失时，可能留下一个没有调用方知道的空 Session。它是非交互的，并像其他 Session 一样被列出。不为这种无害情况增加启动查找。

### 6.2 input

| `InputWrite` | `input/message`（`InputMessageData`） |
| --- | --- |
| `submissionId` | `submission_id`。非空字符串；按约定为 `<source>:<uuid>`（[A.2](../contracts/session-access.md#a2-来源输入约定)） |
| `mode` | `mode` |
| `rawBlocks[i]` `{text, key?}` | `blocks[i]` = `Raw {text, key: key.name, retire: key.use == "retire"}` |
| `content` | 非空时为 `user.content`；否则该输入没有 user 部分 |

Agents 取得该分支的发布观察器，调用 `Session::input`；它像对任何调用方一样校验输入（有 user 内容或块，文本不能全为空）。然后等待已发布序号到达该输入，以事件的 id 与序号回复 `{inputId, sequence}`。

| 结果 | 回复 |
| --- | --- |
| 已发布 | `{inputId, sequence}` |
| 无效 | `rejected`，未写入 |
| Session 已关闭或分支未知 | `session_closed`、`branch_missing`，未写入 |
| 已提交，但分支在发布前关闭或失败 | `publication_unknown` |
| 准入前索引停止 | `reloading`；未写入 |
| 准入后索引卸载 | 已准入任务自身的结果 |

**每个 `submissionId` 一条输入。** 分支对每个 `submissionId` 只接受一次（A.2）。`Session::input` 把输入交给分支的 Timeline，由它在提交锁内检查标识并追加，因此同一标识的并发输入恰好写入一条。检查读取分支按 `submission_id` 索引的自身输入：分支构建时从恢复的历史填入（与打开时为其 capability 读取的同一份自身事件副本），之后每次提交再加入。重复提交不写入；`Session::input` 返回已提交的输入，`input` 在它发布后回复其 `{inputId, sequence}`，即使第一次写入仍未发布。输入被消费或撤回后，标识仍被占用。多条输入共用的标识（写于分支只接受一次之前）以 `ambiguous` 失败且不写入。遇到 `publication_unknown`、`reloading` 或回复丢失后，调用方以同一 `submissionId` 重新提交，或用 `lookup` 读取。

**input 所有权：** 准备阶段校验请求、取得 Session 租约与发布观察器。准备好的输入和租约同步交给调用 `Session::input` 的自持任务；交接前最后的准入检查查看索引停止令牌，以及来自 runtime row 的调用中该 row 的代令牌（调用时以 `caller_row()` 捕获），中间没有 await。准入决定后的取消不能撤回已准入任务。

| 重载情形 | 回复 / 所有权 |
| --- | --- |
| 没有活动索引，或准入前观察到索引停止 | `reloading`（`retryable:true`）；未写入 |
| 调用的 runtime row 在准入前已停止（包括其自身清理中的调用） | `reloading`；未写入 |
| 准入后索引卸载 | 等待任务自身的追加/发布结果，不以 `reloading` 提前替代 |
| 准入后调用方丢弃结果，或其 row 卸载 | 任务及租约保留到结算；回复丢失，重新提交得到同一回执 |

此结果生命周期同时适用于类型化 Rust 和具名服务调用。原生 Rust 调用方没有 row，也没有 row 检查。

### 6.3 lookup

`{sessionId, branchId, submissionId}` → `{status: "accepted", inputId, sequence}` 或 `{status: "unknown"}`；错误 `ambiguous`。

`Session::lookup` 读取分支的 `submission_id` 索引（§6.2）。对于它持有的输入，等到该输入发布后回复它；分支先关闭或失败时以该错误结束。否则回复 `unknown`。调用前提交的每条输入都在索引中，因此对于查找开始前已经返回的写入，`unknown` 是最终结果，不必等待其他待发布事件，也不扫描页面。重启后同样正确：未发布的尾部随进程丢失，索引从恢复的历史重建。`ambiguous` 只回答写于分支只接受一次之前、有多条输入的标识。Rpc 的 `session.input_lookup` 调用同一方法，其边界仍自行解析参数。提交标识是输入本身的字段，没有其他记录保存它（[见下](#提交标识存放在哪里)）。

#### 提交标识存放在哪里

状态：[#32](https://github.com/arcships/dim/issues/32) 已决定：`submission_id` 仍是 `input/message` 的字段，不在别处另作记录。按 main `bb4028d` 核对。原生格式保持 v18（[格式决定](../../../design/appserver/native-storage-format.md#32-格式决定)）。

#32 把该字段描述为没有运行时能力使用的客户端协议数据，要求由 Rpc 或 SessionStore 持有一条与输入原子写入的幂等记录。此后 S2 让每次 `sessions.input` 都必须带它，Goal 经 `Branch::input` 写入它，Workflow（以及 draft 中的 Scheduler）据它决定恢复：

| 所有者 | 写入 | 据此决定 |
| --- | --- | --- |
| Rpc `session.submit`，协议 v3（[connection/tree.rs](../../../../crates/dim-app-server/src/connection/tree.rs)） | 可选的 UUID | — |
| Agents `sessions.input`（§6.2） | 必填、非空，按约定为 `<source>:<uuid>` | — |
| 分支 Timeline（[core/timeline.rs](../../../../crates/dim-agent-runtime/src/core/timeline.rs)），经 `Session::input` 与 `Session::lookup`（`sessions.input`、`sessions.lookup`、Rpc 的 `session.submit` 与 `session.input_lookup`） | — | 每个分支对每个标识只接受一次；查找回答 `accepted`、`unknown` 或 `ambiguous` |
| Goal 分支部分（[goal.ts](../../../../runtime/plugins/goal/goal.ts)） | 经 `Branch::input` 写 `goal:<uuid>` | 不读取 |
| Workflow（[engine.ts](../../../../runtime/plugins/workflow/engine.ts)） | 每个阶段输入和报告写 `workflow:<uuid>`，写入前先存入运行文件 | 同一代或替换代重试时以同一标识重新提交，按回复结算 |
| Scheduler（[A.5](../plugins/Scheduler.md#a5-timeline-与-capability-机制)；运行时在 draft [#317](https://github.com/arcships/dim/pull/317)） | `scheduler:<run_id>` 与 `scheduler-report:<run_id>` | `submitted` 或 `lost`，从不重写；报告从不写两次 |
| `dimcode` 与 Rust TUI（[#312](https://github.com/arcships/dim/pull/312)） | 每次提交新生成一个 UUID | 目前不读取：不确定的提交只报告给用户，不查找 |
| S5c 分叉（[§6.6](session-deletion-and-fork.md#66-身份来源与路径)） | 随事件复制 | —（在分叉上查找能找到复制来的输入） |
| InputQueue、Loop、历史投影、Goal 的折叠、模型 | — | 从不读取 |

AcpGateway、RemoteGateway、Attachments 与 Hooks 的设计在 `session.input_lookup` 之后以同一标识重试。因此该字段已不再是客户端协议数据，而属于输入合同（[Session 访问 A.2](../contracts/session-access.md#a2-来源输入约定)）：由写入方选择的不透明关联键，按分支划分。分支对每个标识只接受一次（§6.2）；不带标识的输入从不去重。

留在事件中即满足 #32 提出的约束，不需要新机制：

- **天然原子。** 标识是追加事务所提交事件的一部分，与输入同时持久化。发布在提交之后，查找等待它找到的输入发布。重启后恢复的历史就是已提交的集合并重建分支索引，`input` 与 `lookup` 都能确定地决定。流程测试：`product_submit_acknowledges_only_a_committed_input`（原生写入被占住、Rpc 重启）、`input_replies_after_publication_and_lookup_waits_for_committed_inputs`，以及依赖 Node 的 `node_workflow_reports_recover_after_complete_host_restart` 与 `node_workflow_reports_reconcile_after_host_process_restart`。
- **没有自己的生命周期规则。** 分叉复制事件，删除移除事件，[Timeline 导出](timeline-export.md)在事件数据中带出它，查找只读分支自己的事件，压缩不删除任何事件。单独的记录需要为其中每一项、以及以后每一次存储变化各定一条规则。
- **设计规则。** [§4](../../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 第 3 条：草稿表达不了写入方的需要时，向事件所有者要一个通用字段。`submission_id` 就是 InputQueue 的 `input/message` 上的这种字段，InputQueue 不解释它。第 9 条限制新增事件，必需事件可以带这样的字段；这个字段还被后续决定使用，即上面各方的恢复。第 4 条：没有核心部件解释它。第 8 条：保留它不改基础设施。移走它要改 `input/message` 的格式、门面的输入签名、Timeline 追加（一条随批次提交但不是事件的记录）以及每个 TimelineStore。这项需求来自调用方，且现有合同已经满足，因此第 8 条不允许这项改动。

| 方案 | 需要什么 | 为何不采用 |
| --- | --- | --- |
| 存储持有、在追加事务内写入的记录（#32 的提议） | `input/message` 去掉该字段。`Session::input` 与 `Branch::input` 另行接收标识，Timeline 追加把它作为一条非事件记录带下去。每个存储在批次事务内写入它。v19 从已有行填表，并改写这些行的正文与 before 轨迹，或继续解码旧字段。分叉与删除多出要复制和移除的行。导出丢失这些标识或新增字段。`session.history`、`session.event`、`sessions.history` 以及 `session.input_lookup` 回复中的事件不再带 `data.submission_id`。SDK 的 `InputMessageData` 去掉它，Goal 随之修改 | 在没有基础设施自身需求的情况下改基础设施（第 8 条），改变客户端看到的内容，并且是第一次改写事件行的迁移，换来的只是事件已经提供的东西 |
| 由 Agents 或 Rpc 在提交之后保存的记录 | `Session::input` 返回后写入的一张进程表 | 不原子。提交与记录之间崩溃时，查找对已写入的输入回答 `unknown`，重试会写两次 |
| 在输入的批次中另写一个事件 | 一个携带标识的新事件 | 仍在 Timeline 中，并且在一个字段就够的地方新增事件（第 9 条） |
| 把标识放进输入的 raw 块 | 写入方把标识写进自己的标记块，`input/message` 去掉该字段 | 查找就要解析 raw 块，而 [session access A.2](../contracts/session-access.md#a2-来源输入约定) 规定只有产生它的插件才能解释。Rpc 输入没有标记块，事件格式也照样要改 |
| 调用方选择事件 ID | Timeline 为 `input/message` 接受调用方给的 ID | 事件 ID 由 Timeline 分配。ID 在 Session 内唯一，重复的 ID 会被拒绝，而不是回复第一条输入的回执 |
| 从事件正文派生的索引 | 像 `timeline_tool_use_starts` 那样，对 `input/message` 行建 `json_extract(body, '$.record.event.data.submission_id')` 表达式索引。SQL 结构改变，需要单独的格式决定 | 现在不需要：分支打开时已读取自身历史，并据此建立内存索引。若这次读取代价过高，这是重开路径，字段仍留在事件中 |

出现以下任一情况时重新考虑：

1. 打开长分支不再读取其全部自身历史后，代价过高并有本地测量。加上述派生索引，字段位置不变。
2. 已提交的事件可以被移除（保留期或截断）。此时 `unknown` 不再表示“从未写入”，被移除的标识也可能再次被接受，那项设计须说明移除后保留什么。

### 6.4 idle

`{sessionId, branchId}` → `{sequence}`，即调用开始后第一个空闲点的分支已分发序号。它就是 `Session::idle`：

```
              input/applied -> turn/start
   +--------+ ----------------------------> +---------+ <--+ 审批或提问待处理：
   |  Idle  |                               | Running |    | turn 保持未结束
   +--------+ <---------------------------- +---------+ ---+
       ^            turn/end 已分发              |
       |                                         | stop/interrupt
       |                                         v
       |                                    +----------+
       +------------ turn 结束 ------------- | Stopping |
                                            +----------+

 idle 在以下条件同时成立时返回 C = 已分发序号：
   Loop 阶段 == Idle  且  没有已提交未发布的事件  且  已分发 >= 已发布
```

调用方在截止点 `C` 可以依赖：

1. 在本次调用之前收到 `input` 回复的每条 Queue 或 Steer 输入，都已在 `C` 或之前被采纳、消费，其 turn 已结束。排在它后面被持有的 Queue 输入也一样，因为 InputQueue 采纳下一条输入、Loop 启动其 turn，都发生在前一个事件的分发过程中（第 3 条）。`stop/interrupt` 是例外（第 2、3 项）。
2. 通常 InputQueue 不持有任何在 `C` 之前写入的输入，因此它们到 `C` 时都有 `input/applied` 或 `input/deleted`。它只在 turn 未结束或即将启动时，或排在另一条被持有的 Queue 输入后面时，才持有 Queue 输入；每个 `turn/end` 释放下一条，被中断的 turn 也以 `turn/end` 结束。一个很窄的例外：若 Loop 在某输入的 `input/applied` 与之后的 `stop/interrupt` 之间重载，它重建时折叠了该中断，不会为该输入启动 turn，于是排在后面的 Queue 输入没有 `turn/end` 来释放，直到之后某个 turn 结束（[input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs) 的 `starting`，[agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs)）。调用方在 `C` 发现自己的输入既无 `input/applied` 也无 `input/deleted` 时，撤回它或重新写入；再次调用 `idle` 会立即返回。
3. 有些输入在 `C` 时已被采纳但仍待处理：Append 输入从不启动 turn；在 `stop/interrupt` 之前被采纳的输入等待下一个 turn。
4. 待处理的审批或提问让其 turn 保持未结束，因此 `idle` 一直等到有人回答或 turn 被停止（第 4 条）。
5. 脱离工作（例如 BackgroundExec 任务或 `!` 运行）不计入。
6. `idle` 覆盖调用前写入的输入。调用开始时分支已空闲，它会立即返回，截止点早于等待期间写入的任何输入。

门面等待 Dispatcher、发布和 Loop 的观察通道；`sessions` 不增加轮询。调用持有租约。Agents 让门面的等待与 Session 的关闭以及索引的停止令牌竞争，因为关闭会让 turn 收尾，门面随后可能返回一个截止点。Session 正在关闭（关闭、删除、Agent 关闭）时结束的等待回复 `session_closed`；Session 故障回复 `failed`；分支未知回复 `branch_missing`；索引卸载回复 `reloading`；等待的其他失败都是分支的分发失败，回复 `failed`。没有超时：不想再等的调用方放弃调用，或用 `stop`、`withdraw` 结束运行，这同样会结束等待。

### 6.5 stop

`{sessionId, branchId, reason?}` → `{sequence}`。`Session::stop(branch, reason)` 写入 turn 范围的 `stop/interrupt`，在被停止的 turn 收尾后以已分发序号回复。默认原因为 `"Stopped by automation"`。已采纳但未消费的输入保持待处理，等待下一个 turn（[附录 B](../contracts/session-access.md#附录-b-完整冲突表)）。同一 turn 上已在进行的 stop 会被合并。错误：`session_closed`、`branch_missing`、`reloading`，分发失败时为 `failed`。

### 6.6 withdraw

`{sessionId, branchId, inputId, reason?}` → `{status: "deleted", sequence}` 或 `{status: "notHeld"}`。

Agents 在该分支上用 `Session::service::<InputQueue>` 取得 S1 的 InputQueue，调用 `withdraw(InputWithdrawData { input_id, reason }, None)`，并返回它的决定。S1 在写入 `input/deleted` 后就回复，此时该事件已提交但不一定已发布，因此对 `deleted`，Agents 与 `input` 一样等它发布后才回复（D8）；分支先关闭或失败时回复 `publication_unknown`。S2 不增加语义：不检查归属，因为调用方凭写入时拿到的 `input_id` 撤回（合同 §9.3）；不重试，也不写自己的事件。指令、决定、结果事实与格式决定都归 S1（[A.4](../contracts/session-access.md#a4-撤回)）；§14.1 记录了 S1 交付的内容。

错误：`rejected`（S1 的校验：`inputId` 为空、`reason` 超过 2048 字节）、`session_closed`、`branch_missing`、`reloading`、`publication_unknown`。指令写入后才收到的 `reloading`，结果交给 InputQueue 的后继实例决定。指令写入后才开始的关闭同样回复 `reloading`，而不是 `session_closed`：InputQueue 在作出决定前随关闭结束，S1 将其报告为 `Reloading`。这是准确的错误码，因为 `session_closed` 表示什么都没写入，而这条指令已提交，Session 下次装载时按历史决定。之后重试会以 `session_closed` 失败。分发越过该指令之后（之后任何 `idle` 截止点都已越过），历史中对调用方写入的输入恰好有 `input/applied` 与 `input/deleted` 之一。

### 6.7 history

`{sessionId, branchId, after?, through?, atLeast?, limit?}` → `{through, events}`。

| 项 | 规则 |
| --- | --- |
| `through` | 带 `atLeast` 时：已发布 head（`Session::sequence`）；低于 `atLeast` 时为 `future_sequence`。否则为请求的 `through`（不能超过 head，否则 `future_sequence`），或 head |
| `events` | 该分支自己在 `(after, through]` 内的事件，用 `Session::local_page` 读取。`after`（缺省为 0）必须是某批次的结尾。剩余事件足够时，一页先取 `limit` 条（1 到 256，缺省 256），再读到该批次结尾，因此可能超过 `limit`；它从不越过 `through`。以已发布 head 或 `idle` 截止点作为 `through` 时，每个批次都完整。调用方把 `after` 设为上一页最后的序号继续读，直到 `through` |
| 形式 | 分支的每个事件都作为 `AgentEvent` 记录返回，序列化形式与 `session.history` 展示事件时相同。不做过滤，因此一页最后的序号总是某个批次的结尾 |
| 祖先 | 不包含。启动的 Session 在 `main` 上读取，`main` 没有祖先。等有使用方需要时，再增加读取子分支前缀的方式 |
| 错误 | `rejected`（`after` > `through`、`after` 位于批次中间、`limit` 不在 1 到 256 之间、同时给出 `atLeast` 与 `through`）、`future_sequence`、`session_closed`、`branch_missing`、`reloading` |

### 6.8 delete

`{sessionId}` → `{}`。Agents 执行 S5c 的 `AgentIndex::delete`（[S5c §3.2](session-deletion-and-fork.md#32-顺序)）：检查 id，取得该 Session 的准入闸门，Session 已装载时关闭它，由 SessionStore 删除它的命令输出，再在一个事务中删除它的全部行，最后遗忘它。该 Session 上进行中的调用（包括 `idle`，§6.4）以 `session_closed` 结束，之后的调用也一样。不存在或已删除的 Session 回复 `{}`。和每个 `sessions` 方法一样，调用不携带调用方身份，因此不检查调用方是否启动过该 Session（§11）。

删除一旦开始就执行到它自己的结果，与 Rpc 的 `session.delete` 相同：索引重载不会截断它，因为各 Session 及其存储比索引活得更久（D1），而中途停下的删除会让 Session 保持关闭、行仍保留，直到重试。放弃调用的 Rust 调用方可能让删除只执行了一部分，与 Rpc 回复丢失相同（S5c §3.5）；再次删除即可完成。

错误：`rejected`（id 不是单个路径分量，S5c D3）、`reloading`（没有激活的 Agents 索引；什么都没做）与 `failed`（命令输出或行无法删除，[S5c §3.5](session-deletion-and-fork.md#35-幂等与错误)）。`failed` 之后，被这次删除关闭的 Session 在本进程中保持关闭，行仍保留（S5c D5）：对它的调用以 `session_closed` 失败，直到某次删除成功。删除是幂等的，因此调用方在任何错误或回复丢失后都再次删除（§8.7）。

## 7. 时序

一次运行：写入一条输入，第二条输入在被持有时撤回，然后等待：

```
 调用方              sessions / AgentIndex           Session S (main)            InputQueue、Loop
   | launch{starter,policy} ->|                            |                           |
   |                    启动方租约、读 PermissionState、取上限、创建 S（interactive=false）
   |<- {S, policy} ----------|                            |                           |
   | input{S,main,queue,A} ->| Session::input ----------->| input/message A @a        |
   |                         | 等待已发布 >= a            | -- 分发 ----------------->| applied A、turn/start
   |<- {A, a} ---------------|                            |                           |
   | input{S,main,queue,B} ->| ... ---------------------->| input/message B @b        | B 被持有（turn 未结束）
   |<- {B, b} ---------------|                            |                           |
   | withdraw{S,main,B} ---->| S1 撤回服务 -------------->| input/withdraw B @w       |
   |                         |                            | -- 分发 ----------------->| B 被持有：input/deleted @d
   |<- {deleted, d} ---------|                            |                           |
   | idle{S,main} ---------->| Session::idle（观察通道）  |                           | ... A 的 turn/end
   |<- {C} ------------------|                            |                           | Loop 空闲，已分发 >= 尾部
   | history{S,main,through:C,after:0} -> 分页读到 C：A 已消费、turn/end、B 已删除
```

## 8. 生命周期

### 8.1 拥有关系

Agents 像拥有其他 Session 一样拥有启动的 Session 的实例与记录。当前客户端会列出它，用户可以 attach、回答它的审批、停止、关闭或删除它。调用方只拥有自己那些指向该 Session 的记录（运行记录）以及它写入的输入，并在删掉最后一条这样的记录时删除该 Session（§8.7）。关闭或删除启动方不影响启动的 Session；用户关闭或删除启动的 Session，只会让调用方之后的调用以 `session_closed` 失败。

### 8.2 装载、关闭与卸载

和所有 Session 一样，启动的 Session 在有租约持有或其工作运行期间保持装载。有原生存储时，它在宽限期 `idle_unload_after`（默认 60 s）内没有被任何租约持有、且空闲，Agents 就卸载它（[空闲卸载](idle-unload.md)，[生命周期 §5](../../../architecture/rust-agent/lifecycle.md#5-stop-与-close)）。卸载或重启之后，下一次 `sessions` 调用会恢复它。调用只在运行期间持有租约，`launch` 回复后不保留租约。`sessions` 没有 `close` 或 `release`（D12）。

### 8.3 Agents 索引重载

AgentScope 与各 Session 继续运行，`sessions` 服务也保留。旧索引取消其停止令牌时，可取消调用以 `reloading` 结束；新调用以 `reloading` 失败，直到下一个索引激活。使用方不重载。回复为 `reloading` 的 `withdraw` 用 `history` 解决。已开始创建 Session 的 `launch` 不会被截断：它回复该创建的结果，因为 `reloading` 回复会在第一个 Session 仍被创建时招致重试（§6.1）。`delete` 也不会被截断：它回复自己的结果（§6.8）。

 已准入的 `input` 与 launch/delete 一样等待自身结果。所有调用方的任务与 Session 租约都保留到实际追加/发布结算（§6.2）；最后准入检查观察到索引停止时以 `reloading` 回复且未写入。索引重载仍不重载使用方。

### 8.4 调用期间使用方重载

使用方的旧一代卸载时不等待它的 `sessions` 调用。`idle` 等待可以持续到它自己的结束条件（§6.4），并持有租约；回复丢失不关闭其 Session。`launch` 或已准入的 `input` 即使回复丢失也会完成。新一代以同一 `submissionId` 重新提交输入，得到第一条输入的回执，无论它已写入还是仍在进行（§6.2）；其他状态用 `lookup`、`idle` 和 `history` 重新读取。过期的 `idle` 在 Session 的下一个空闲点结束，与新的等待结束在同一时刻。卸载时等待自身调用的部分（例如 C1 `drain` 方法，或在清理中 join 自身工作）会等到该调用回复；这是该部分自己的工作，不是 `sessions` 的规则。

### 8.5 删除（S5c）

删除（[#97](https://github.com/arcships/dim/pull/97)，客户端经 `session.delete`，进程插件经 `sessions.delete`，§6.8）取得该 Session 的准入闸门并关闭它。该 Session 上进行中的调用（包括 `idle`）以 `session_closed` 结束，之后的调用也一样。启动方已被删除的启动以 `session_closed` 失败。启动方被删除后，已启动的 Session 继续运行。

### 8.6 目标：空闲卸载（后续项）

第 6 条表明启动的 Session 会累积。每五分钟运行一次的任务，一天留下约 288 个已装载的 Session，约 86 MB 分配、12,000 个任务，并且之后每次创建都更慢。目标：

- **卸载。** 当每个分支都空闲、且没有任何 `SessionUse` 持有它时，Agents 卸载这个已装载的 Session。“没有持有”指：没有 attach 的客户端或订阅，没有 Collaboration 成员关系，没有进行中的 Rpc 或 `sessions` 调用。`Session::unload_if_idle` 在各分支的提交锁下检查空闲。下一次 `acquire` 恢复该 Session。
- **范围。** 适用于所有 Session，不只是启动的 Session。
- **它负责的开放问题。** 即 #57 留下的问题：脱离工作是否阻止卸载，以及何时检查（最后一个租约释放时，以及没有租约的 Session 变为空闲时）。
- **依赖顺序。** 先 S2，再空闲卸载（Agents 负责线，[#126](https://github.com/arcships/dim/issues/126)；以本次测量重新打开 S3/#57），然后才是规模化的 Scheduler `create` 模式任务（P6）。

S2 不需要为此改动。每次调用都持有租约，`sessions` 不在已装载的 Session 中保存状态，`idle` 等待在第一个空闲点结束，因此过期的等待不会让 Session 在那之后仍被占用。

设计见[空闲 Session 卸载](idle-unload.md)，已在 [#331](https://github.com/arcships/dim/pull/331) 实现。它收窄了上面关于成员关系的一条：持有成员的是未结束的 Collaboration 请求，单有关系并不持有（其 D2）。

### 8.7 删除运行用的 Session

空闲卸载约束的是内存，但每个启动的 Session 还留在存储里，也出现在每个客户端的 Session 列表中。每五分钟一个任务，一天就多约 288 个。因此由使用方删除它们（D14）：

- **谁删、何时删。** 启动 Session 的使用方在删掉最后一条指向它的记录时删除它。Scheduler 在一次运行移出其运行历史（A8，每个任务保留最近 100 次）或任务被删除时删除；`reuse` Session 由任务指向，随任务一起删除。Workflow 在一条运行记录移出其保留范围（A9，保留最近 100 次）时删除；这条记录指向该次运行的所有阶段 Session。
- **只删已结束的运行。** 删除会关闭 Session，从而停止它的 turn。使用方只在运行到达终态后才删除其 Session；任务被删除后仍在进行的运行（Scheduler A14），在它结束时删除其 Session。
- **顺序。** 使用方先删除 Session，再删掉记录。删除失败时记录保留，下一轮重试；删除一个已经不存在的 Session 也会成功，因此在两步之间崩溃不会造成损害。
- **保留某次运行。** 想保留某次运行对话的用户可以 fork 它（S5c 的 `session.fork`）。没有记录指向 fork 出的 Session，它会一直保留到用户删除。
- **留给用户的部分。** `launch` 回复丢失时留下的 Session（§6.1）没有记录指向。用户用 CLI 按时间或数量的清理删除它，该清理由 `session.summaries` 与 `session.delete` 组合而成（S5c §5.1；F6，[#77](https://github.com/arcships/dim/issues/77)）。
- **方法。** `sessions.delete {sessionId}` → `{}`，转交给 S5c 的 `AgentIndex::delete`（[#97](https://github.com/arcships/dim/pull/97)）：准入闸门、关闭，然后删除该 Session 的行与文件（§6.8）。S5c 的 `session.delete` 是 Rpc 内建方法，进程插件调用不到。它和每个 `sessions` 方法一样不携带调用方身份，因此不检查调用方是否启动过该 Session（§11）。
- **依赖顺序。** 先有 S5c 的 `AgentIndex::delete`，再由 Agents 负责线加入 `sessions.delete`（§6.8）：S5c 与 S2 都合入后，连同其 A.1 行一起加入（没有打桩，路线图 §3.6）。接下来是使用方的删除（P6 [#70](https://github.com/arcships/dim/issues/70)、P7 [#71](https://github.com/arcships/dim/issues/71)）。

## 9. 错误

| 错误码 | 方法 | 含义 | `retryable` |
| --- | --- | --- | --- |
| `rejected` | 全部 | 请求结构或边界规则不符；未写入 | false |
| `agent_unknown` | `launch` | 没有已装载的 Agent 使用该 id | false |
| `session_closed` | 除 `delete` 外全部（对 `launch` 指启动方） | Session 已关闭、已删除或未知；未写入 | false |
| `branch_missing` | 除 `delete` 外全部（对 `launch` 指启动方） | 没有该分支；未写入 | false |
| `reloading` | 全部 | Agents 索引、模型选择宿主、启动方的 Permission 或 S1 的撤回服务未激活（`withdraw` 写入指令后 Session 关闭时也是，§6.6），或调用期间索引卸载。对 `input` 以同一 `submissionId` 重新提交；对 `withdraw` 结果不确定：先 `history`。`delete` 只在开始前以 `reloading` 失败（§6.8） | true |
| `future_sequence` | `history` | `atLeast` 或 `through` 超过已发布 head | false |
| `ambiguous` | `input`、`lookup` | 不止一条输入使用该 `submissionId`，写于分支只接受一次之前；未写入 | false |
| `publication_unknown` | `input`、`withdraw` | 已提交，发布未确认；恢复后重新提交、`lookup` 或 `history` | true |
| `failed` | 全部 | Session 故障、存储、模型解析、项目授权、Session 构建；对 `delete` 指 Session 的命令输出或行无法删除（§6.8） | false |

## 10. 时序与冲突

| 场景 | 顺序 | 结果 | 负责方 |
| --- | --- | --- | --- |
| 启动期间启动方的策略变化 | `permission/changed` 与启动时的读取 | 启动使用读取时已分发的策略；之后的变化从不影响启动的 Session | Agents |
| 启动前启动方已关闭或删除 | — | `session_closed`；不创建 | Agents |
| 启动后启动方被删除 | — | 启动的 Session 继续运行 | Agents |
| 收到 `input` 回复后调用 `idle` | 输入已发布 < 调用 | 覆盖该输入的 turn（Queue、Steer） | Loop、InputQueue |
| 在空闲分支上调用 `idle` | 调用 < 输入 | 立即返回，早于该输入 | 调用方 |
| 审批待处理 | turn 未结束 | `idle` 等待；`stop` 结束它 | Approval、Loop |
| 先 `withdraw` 再 `idle` | 撤回 < 截止点 | 历史中该输入有 `input/deleted` 或 `input/applied` | InputQueue |
| Loop 在某输入的 `input/applied` 与 `stop/interrupt` 之间重载 | applied < interrupt，没有 `turn/start` | 排在后面的 Queue 输入到 `idle` 截止点仍被持有，直到之后某个 turn 结束；调用方撤回或重新写入 | InputQueue、Loop |
| 调用期间 Agents 索引重载 | — | `reloading`，已受理的 `launch` 与 `delete` 除外，它们回复自己的结果；使用方保持装载 | Agents |
| `idle` 期间使用方重载 | — | 旧等待持续到结束，其回复被丢弃；新一代重新读取 | Agents、使用方 |
| 调用期间 Session 被关闭或删除 | — | `session_closed` | Agents |
| 两个调用方写同一个 Session | Timeline 顺序 | 按序号顺序、按模式采纳 | InputQueue |
| 提交后重启，回复丢失 | — | `lookup`：已发布则 `accepted`，随未发布尾部丢失则 `unknown` | 调用方 |

## 11. 安全与权限

- **信任。** `sessions` 只服务控制节点进程内受信任的插件。`HostDispatch` 不携带调用方身份，因此 `sessions` 不对调用方授权。受信任的进程内插件不会因依赖声明而被隔离（AGENTS.md，Agent v4 规则第 11 条）。
- **策略上限。** 上限（§6.1）让启动的 Session 不超出启动方的当前策略；请求不带启动方时，不超出宿主默认值。它是给守规矩的使用方用的约定，不是安全边界：`starter` 由调用方自己填，不传时上限是宿主默认值，而默认值可能比启动方的策略更宽。调用方都是受信任的进程内插件（见上），因此 `sessions` 无法区分本来就没有启动方的运行和没传启动方的运行。用户仍可以像对任何 Session 一样，经 `session.permission.update` 修改启动的 Session 的策略。
- **审批。** 启动的 Session 中的工具调用按它自己的策略经 Approval 与 Permission。由配置启动的运行（任务、定义）不需要本机批准（[ADR 003 D2](../../adr/003-approval-scope.md#d2-配置引起的运行不需要本机批准)）。
- **内容。** Agents 从不解析标记或 `submissionId`。输入会重放进模型上下文，因此调用方不能放入凭据（[合同 §7](../contracts/session-access.md#7-安全)）。

## 12. 对其他文档的修改

[Session 访问](../contracts/session-access.md)（中英文），只改 `sessions` 部分：

- **引言、§4、附录 B、§8（D3）。** Agents 重载让进行中的调用以 `reloading` 失败；调用方不重载（D1，#35）。
- **§3.1。** `launch` 箭头去掉 `source`，回复加上 `policy`。
- **§7。** 策略上限是给受信任调用方用的约定，不是安全边界（§11）。
- **A.1。**
  - 删除“调用方的 `source` 由 Agents 固定……”一句：rutis 不传递调用方身份，§9.3 也已撤回该需求。
  - `launch` 回复 `{sessionId, policy}`，新增错误 `agent_unknown`、`session_closed` 与 `branch_missing`。
  - `model` 改为 `{providerId, modelId}`。
  - `stop` 新增 `reason?`。
  - `HistoryRequest` 新增 `limit?`，`HistoryPage.events` 为该分支的 `AgentEvent` 记录。
  - 每个方法也都可能以 `failed` 失败。
  - 链接到本文的绑定与生命周期。
- **A.5。** history 为该分支自己的事件，分页读取。
- **A.6。** 新增 `agent_unknown`，并把 `reloading`、`rejected` 与 `failed` 的含义扩大到 §9 列出的情况。
- **附录 E。** `launch` 返回 `LaunchReply` 而不是租约；key 映射为 `InputBlock::Raw {key, retire}`；`lookup` 为共用的 Agents 查找；方法为 `AgentIndex` 的类型化方法（附录 A）。

其他：

- [总览](../overview.md)（中英文）：B.2 Agents 行链接本页；§5.5 的 `launch` 箭头去掉 `source`。
- [文档索引](../../../INDEX.md)链接本页。

实现过程中（§17）：A.1 的 `withdraw` 增加 `publication_unknown`，`history` 拒绝同时给出 `atLeast` 与 `through`，A.6 的 `publication_unknown` 覆盖 `withdraw`，附录 E 的 `withdraw` 行按 S1 合入的形态改写。加入 `sessions.delete` 时，引言、§6 与 A.1 列出 `delete`，附录 E 给出它的映射；§4 与附录 B 的 Agents 重载行写明已受理的 `launch` 与 `delete` 回复自己的结果；总览 B.2 的 `sessions` 行也列出 `delete`。

S1 的撤回合同（A.4）、输入约定（A.2）与 `interactive`（A.3）不变，因此不需要单独的合同提案。

## 13. 规则检查

| 规则 | 回答 |
| --- | --- |
| §4 第 1 条（指令事件） | `sessions` 经门面写入 `input/message`，经 `Session::stop` 写入 `stop/interrupt`，只经 S1 的服务写入 `input/withdraw`。结果由 InputQueue 与 Loop 写入。没有效果的撤回不写入任何内容，回复 `notHeld` |
| §4 第 2 条（交接） | 不收回任何东西：撤回由 InputQueue 在 `input/applied` 之前决定，之后只能用 `stop` |
| §4 第 3 条（before 回调） | 不使用。入口只检查结构；上限是 Agents 对自己的创建操作的规则 |
| §4 第 4 条（核心不带业务含义） | Agents 从不解释标记、`submissionId` 或 `reason`；上限是通用规则 |
| §4 第 5 条（批次） | 每次 `input` 是一个事件；`sessions` 不写自己的批次 |
| §4 第 6 条（模型上下文） | 没有新的上下文路径；只有已消费的输入进入模型 |
| §4 第 7 条（公开事件与服务） | 只使用 Session 门面（包括 `Session::get::<PermissionState>`，即 Permission 公开的只读状态）、S1 的公开服务，以及 `delete` 所用的 Agents 自己的 `AgentIndex::delete`（S5c） |
| §4 第 8 条（基础设施不变） | 不改 Timeline、Dispatcher、事件格式、rutis、装配或 Session 门面。所需能力在 §14 提给 S1（一个能力）与 C4（#4 桥接）；不向 rutis 提需求 |
| §4 第 9 条（Timeline 只放决定用的事实） | 没有新事件。租约、调用方进度与启动来源都不进 Timeline。`submissionId` 是必需事件 `input/message` 的字段；分支对每个标识只接受一次，调用方重新提交或经 `lookup` 恢复（§6.2、§6.3，#32） |
| §1（App Server 边界） | Agents 只提供访问；调度与工作流逻辑留在各自的插件中 |
| ADR 002 D1 | 进程作用域：服务跨 Session；放在 Agents 中 |
| ADR 002 D2 | 不新增插件；由 Agents 现有的目录部分提供 |
| ADR 002 D3 | 使用方依赖 Agents；没有核心部分依赖使用方。每次调用查找索引，使用方不随索引重载（#35） |
| ADR 002 D4 | 不增加配置；宿主上限是宿主现有的 Session 默认值 |
| ADR 002 D5 | 语言中立合同见 §6 与 A.1；Rust 绑定见附录 A；错误为数据；取消即放弃调用 |
| ADR 003 | 启动的上限让守规矩的使用方不超出启动方的策略，这是约定而不是安全边界（§11）；启动的 Session 中的工具调用由 Approval 与 Permission 按它自己的策略准入 |

## 14. 向机制所有者提出的需求

### 14.1 S1，InputQueue 撤回（eric8810，[#58](https://github.com/arcships/dim/issues/58)）

已在 [#94](https://github.com/arcships/dim/pull/94) 交付：`InputQueue::withdraw(InputWithdrawData { input_id, reason }, steer: Option<InputMessageData>) -> Result<(Withdrawal, Option<Arc<AgentEvent>>)>`，其中 `Withdrawal::{Deleted { sequence }, NotHeld}`。它在决定写入后回复，以 `InvalidInput`（自身的校验）或 `Reloading`（追加后 InputQueue 卸载）失败。`sessions` 不传 `steer`。下表是当初提出的需求；S2 的相应调整见 §17。

| 项 | 内容 |
| --- | --- |
| 被什么挡住 | `sessions.withdraw` 必须转交给 InputQueue 的决定；A.4 与附录 E 指名了该服务，但它还不存在（InputQueue 目前没有公开服务） |
| 考虑过的现有机制 | 由 Agents 写入 `input/withdraw`：Agents 就得等待并解释 InputQueue 的结果（§4 第 1、7 条）。经 `sessionMethods` 的 `session.input.withdraw`：它是客户端方法，进程插件没有连接就用不到 |
| 需要的通用能力 | 一个分支作用域的公开服务（名字注册在分支组中的 `PublicService`），可在分支外经 `Session::service` 取得，方法为 `withdraw(input_id, reason: Option<String>) -> Result<WithdrawOutcome, AgentError>`。它追加 `input/withdraw`，只以 InputQueue 对这条指令的决定作答：`input/deleted` 分发后为 `Deleted { sequence }`，或不写任何内容的 `NotHeld`。失败为 `Closed`、`UnknownBranch`、`Reloading`（追加后 InputQueue 重载；后继实例按历史决定）或 `InvalidInput`（它自己的检查，包括 `reason` 长度上限，与事件一起只定义一次）。在恢复的 Session 上，它按从历史重建的持有集合正确决定 |
| 对序号、批次、中断、重载的影响 | 不超出 A.4：一条指令事件，按序号顺序决定 |
| 负责方与 issue | S1，[#58](https://github.com/arcships/dim/issues/58)。`input/withdraw`/`input/deleted` 的格式决定归 S1；S2 不需要格式决定 |

### 14.2 C4，TypeScript 桥接（zerob13，[#63](https://github.com/arcships/dim/issues/63)）

| 项 | 内容 |
| --- | --- |
| 被什么挡住 | ADR 002 D5 把取消定义为使用方放弃调用。rutis 0.10 不会把 JavaScript 的 `AbortSignal` 带进 Rust 调用，因此 TypeScript 调用方无法放弃一个待处理的 `sessions.idle`（§8.4） |
| 考虑过的现有机制 | `pluginFiles` 额外的取消 Promise 参数：是单个服务的约定，不是绑定规则。`stop` 或 `withdraw`：它们在结束等待的同时也结束了运行 |
| 需要的通用能力 | 在语言中立绑定中，TypeScript 调用方放弃对 Rust 提供的共享服务的调用时，丢弃对应的 Rust future。绑定对进程服务也沿用 App Server 的信封（§5），使 `SessionsError` 以数据形式到达 |
| 对序号、批次、中断、重载的影响 | 无：放弃等待不写入任何内容 |
| 负责方与 issue | C4，[#63](https://github.com/arcships/dim/issues/63) / [#4](https://github.com/arcships/dim/issues/4)。不阻塞 S2：没有它时，等待按 §8.4 结束 |

### 14.3 Agents 负责线：空闲卸载（后续项）

| 项 | 内容 |
| --- | --- |
| 被什么挡住 | 启动的 Session 从不卸载（#57），每个在装载期间约占 300 KB 与 43 个任务（第 6 条） |
| 考虑过的现有机制 | 关闭运行：为该 Session 的所有使用者结束它（D12）。`Session::unload_if_idle`：已存在，但没有调用方 |
| 需要的通用能力 | §8.6：已装载的 Session 空闲且没有租约持有时，宿主卸载它；`acquire` 恢复它 |
| 对序号、批次、中断、重载的影响 | 对 Timeline 无影响：空闲释放不记录事件，下一次使用时恢复 |
| 负责方与 issue | Agents 负责线（yyhhyyyyyy），[#126](https://github.com/arcships/dim/issues/126)，重新打开 S3（[#57](https://github.com/arcships/dim/issues/57)）；应在 Scheduler 的 create 模式任务上线前完成 |

不向 rutis 提需求。

## 15. 给使用方设计的说明

供 Scheduler 与 Workflow 的设计评审参考（zerob13）；S2 不修改这些文档：

- **Rust 绑定。** 它们附录 G 中的 `AgentIndex::launch -> SessionUse` 与 `service::<InputQueue>` 改为使用具名服务（§5）；`SessionUse` 是 Agents 内部的。
- **模型。** `model` 为 `{providerId, modelId}`。
- **启动方。** 只有在运行由某个 Session 发起时都传 `starter` 的使用方，上限才成立（§11）：创建 Scheduler 任务的 Session（A15），或 Workflow 运行的启动方分支。
- **策略读取。** `sessions` 没有读取策略的方法。启动会取上限并回复生效策略（D6）。保存时记录的上限（Scheduler A15）或 Workflow 启动时的检查，需要从调用方 Session 自己的部分取得启动方策略，或在它们的评审中提出一个读取方法。Scheduler 的 reuse 检查也一样。
- **历史。** `sessions.history` 用 `after` 与 `limit` 分页。
- **重载。** `inject: ['sessions']` 不随 Agents 索引重载（D1），因此 Agents 重载时运行不必以 `interrupted` 结束；进行中的调用以 `reloading` 失败，已受理的 `launch` 与 `delete` 除外，它们回复自己的结果（§8.3）。
- **已装载的 Session。** 运行用的 Session 在宽限期内没有被任何租约持有时，Agents 卸载它，下一次调用恢复它（§8.6，[#126](https://github.com/arcships/dim/issues/126)）。`launch` 不是幂等的（§6.1）。
- **删除运行用的 Session。** 这是对两份设计的需求（§8.7）：Scheduler 与 Workflow 在删掉最后一条指向某个自己启动的 Session 的记录时删除它（Scheduler：一次运行移出该任务最近 100 次，或任务被删除；Workflow：一次运行移出最近 100 次），且只在运行到达终态后，先删 Session 再删记录，经由 `sessions.delete`。用户通过 fork 保留某次运行，并用 CLI 的清理删除 `launch` 回复丢失时留下的 Session。
- **再次等待。** “尚未消费，再等一次”不能在 `idle` 上循环：分支空闲时它立即返回。截止点时两种结果都没有的输入处于被持有状态（§6.4 第 2 项）。

## 16. 实现与验收

S1 合入后在 [#98](https://github.com/arcships/dim/pull/98) 上实现，`sessions.delete` 在 S5c 之后加入；没有打桩（路线图 §3.6）。

1. 完成。在 Agents 负责线中实现类型与七个 `AgentIndex` 方法（附录 A）。
2. 完成。目录部分提供 `sessions`；App Server `lib.rs` 注册 `register_shared("sessions")`（装配负责线中的一行）。
3. 完成。查找属于分支（`Session::lookup`，§6.3）；`sessions.lookup` 与 Rpc 的 `session.input_lookup` 都调用它。
4. 完成。`xtask graph` 记录了新模块与 `sessions` 绑定；`xtask check` 通过。
5. 完成。`sessions.delete` 转交给 S5c 的 `AgentIndex::delete`（[#97](https://github.com/arcships/dim/pull/97)），A.1 中有它的行（§6.8）。

流程测试在 [transport_flow/sessions.rs](../../../../crates/dim-app-server/tests/transport_flow/sessions.rs)。`serve` 不接受额外的进程行，因此由一个测试 Session 部分按名称注入 `sessions`（`Keyed<dyn HostDispatch>`，与 `plugin_files_flow` 相同），测试再调用它，从而走语言中立路径。计划中的用例对应如下：

1. **Issue #62 的完成标准**（`a_consumer_launches_writes_waits_and_withdraws_by_input_id`）。该插件启动一个 Session，写入输入，等待空闲，并按 `input_id` 撤回一条被持有的输入（`deleted`）；`input/applied` 之后的撤回回复 `notHeld`。
2. **启动**（`launch_caps_the_policy_by_the_starters_current_policy_or_the_host_defaults`）。启动方被 `permission/changed` 收窄后，启动的 Session 为非交互，其 `PermissionState` 等于上限，回复中也返回该上限。没有启动方时：full-access 请求被宿主默认值限制，不带策略即为 read-only。未知 Agent 以 `agent_unknown` 失败，错误的 `cwd` 以 `rejected` 失败，已关闭的启动方以 `session_closed` 失败。
3. **输入与查找**（`input_replies_after_publication_and_lookup_waits_for_committed_inputs`）。标记在前，user 内容在后。回复后立即调用 `history {atLeast: sequence}` 成功。`lookup` 对写入的 id 返回 `accepted`，对其他 id 返回 `unknown`。以同一 id 重新提交回复第一条回执且不写入；另一个 id 会写入。原生存储被锁住、发布暂停时，`input` 的回复和提交后开始的 `lookup` 都会等待，之后 `lookup` 读到 `accepted`。
4. **空闲**（`idle_covers_written_turns_and_waits_on_a_pending_approval_until_stop`）。截止点覆盖每条输入的消费及其 `turn/end`。审批待处理时，`idle` 一直等待，直到 `stop`。
5. **重载与关闭**（`idle_ends_reloading_on_an_index_reload_and_session_closed_on_close`）。`idle` 期间 Agents 索引重载，以 `reloading` 结束它；测试部分保持装载，下一次调用成功。`idle` 期间关闭 Session，以 `session_closed` 结束它。`launch` 等待被锁住的原生存储时重载（`launch_admitted_before_an_index_reload_replies_the_created_session`），回复已创建的 Session。
6. **历史**（`history_pages_the_branch_through_a_fixed_sequence_on_batch_boundaries`）。`limit` 为 1 的各页连起来到达固定的 head，与分支事件一致；一页会读完两个事件的批次；批次中间的游标、`future_sequence` 及其他边界错误都有检查。
7. **删除**（`a_consumer_deletes_its_launched_session_and_retries_a_failed_delete`，使用一次性原生存储）。`idle` 正等待某个已装载的启动 Session 上待处理的审批时，插件删除该 Session：等待以 `session_closed` 结束，Session 及其行都已删除，之后对它的调用（包括以它为启动方的启动）以 `session_closed` 失败。再删除一次，或删除从未创建的 id，都回复 `{}`；不是单个路径分量的 id 和带多余字段的请求为 `rejected`。原生写入方被占住时，删除期间的 Agents 索引重载不会结束它：写入方释放后它回复 `{}`，行已删除。无法删除该 Session 命令输出的删除回复 `failed`；Session 保持关闭，行仍保留，重试后删除成功。
8. **#98 评审的后续项。**
   - `a_launch_fails_reloading_while_the_starters_permission_reloads`（D5）：测试经一个随 Agents 索引重载的 Session 部分绑定 Permission 的命令规则，并让它的重载停住。启动方的 `PermissionState` 缺失期间，以它为启动方的启动以 `reloading` 失败，而不是使用宿主上限；Permission 恢复后，同样的启动成功。
   - `a_launch_resolves_its_model_through_the_model_selection_host`（§6.1，`model`）：使用配置的模型时，请求的模型（否则为目录默认模型）经宿主解析，并带上目录中该模型的默认配置；宿主无法解析的模型以 `failed` 失败，不创建任何东西。宿主的 `reloading` 情况无法从流程测试触发：`serve` 不会重启任何一个模型选择提供方。
   - `a_dropped_launch_still_registers_its_session`（§17 第 6 条）：在创建等待被占住的原生写入方时放弃的启动，仍会创建 Session，Rpc 也能 attach 它，而这需要它的协议版本已登记。
   - `input_and_withdraw_reply_publication_unknown_when_the_store_fails_the_write`（§17 第 1 条）：原生存储让一条输入的写入失败，再让一次撤回的 `input/deleted` 写入失败。两者都回复 `publication_unknown`，因此都不会在其事件发布前回复。

## 17. 实现过程中的设计变更

每项变更都只影响 S2（路线图 §5.3）；合同中 `sessions` 部分已同步修改。

| # | 变更 | 原因 |
| --- | --- | --- |
| 1 | `withdraw` 等 `input/deleted` 发布后才回复 `deleted`，并可能回复 `publication_unknown` | S1 在删除提交后就回复，而不是 §14.1 要求的分发后。不等发布的话，回复后立即调用 `history {atLeast: sequence}` 可能失败 |
| 2 | 绑定使用 S1 合入的名称：`Withdrawal` 与 `InputQueue::withdraw(InputWithdrawData, None)`，而不是 `WithdrawOutcome` 与 `withdraw(input_id, reason)` | S1 合入的形态（§14.1） |
| 3 | 每个类型化方法接收与其线上形态对应的请求类型（`LookupRequest`、`WithdrawRequest`、`StopRequest`），不再用位置参数 | 每种形态一个类型；具名服务只负责解码与编码 |
| 4 | `history` 拒绝同时给出 `atLeast` 与 `through`，并返回不经过滤的全部事件 | 两者以相互冲突的方式决定 `through`。过滤后一页可能结束在批次中间，下一次的 `after` 会被拒绝 |
| 5 | `stop` 或 `idle` 的等待失败，只要不是 Session 已关闭、分支不存在或重载，都回复 `failed` | 这些失败是分支的分发失败。把处理器的 `InvalidInput` 映射为 `rejected` 会归咎于调用方 |
| 6 | `launch` 在一个比调用活得更久的任务中创建并登记 Session；该任务开始后，回复等待它而不是索引重载 | 被放弃的调用不能留下一个已装载、客户端却看不到的 Session；`reloading` 回复不能招致重复启动 |
| 7 | 代码放在子模块 [agents/sessions.rs](../../../../crates/dim-app-server/src/agents/sessions.rs) | `agents.rs` 只保留目录与索引 |
| 8 | 流程测试从一个 Session 部分取得 `sessions` | `serve` 不接受额外的进程行；该部分像进程插件一样按名称注入服务 |
| 9 | `history` 拒绝超过 256（即缺省值）的 `limit` | #98 的评审：D11 分页是为了限制跨桥接的回复大小，而不设上限的 `limit` 会一页返回整个分支。Collaboration 的 `agent` 读取同样拒绝超过其缺省值的 `limit` |

### 17.1 先装载持久所有权，再发布索引

状态：已在 [#327](https://github.com/arcships/dim/pull/327) 实现，修正 [#318](https://github.com/arcships/dim/issues/318)。
此前的实现先发布 AgentIndex，再由 serve 或 restart 调用 install_default。
这个窗口允许进程 owner 在所属 Agent 与协议版本映射尚未装载时查询正常的持久 Session，
acquire 会返回不可重试的存储失败，使 Scheduler 恢复错误地结算为 lost。

复用已有初始化函数和服务发布边界：

1. AgentsPlugin.apply 构造索引，注册已有清理。
2. 保留现有 config.session_execution 条件。执行已启用时，在 ctx.provide_as::<AgentIndex>
   前 await index.install_default()，先装载持久 Agent、记录不可用项并安装所有权映射，
   再发布索引。执行未启用时跳过该初始化，继续保持 Session 准入关闭，直到产品执行配置安装。
3. 删除 serve 与 RunningServer::restart 中发布后的 install_default 调用，
   同时删除“Agent scope 注入索引”的过时注释；启动、重启和依赖驱动的 Agents 重载共用
   owner 自身的初始化路径。

当前 AgentScopePlugin 只依赖 AgentDirectory 与 Loader，子部件 Sessions 无依赖，
两者都没有声明 AgentIndex。提案不建立索引到自身已发布服务的依赖。
锁定的 rutis-loader 0.16.0 已允许在插件 apply 中调用 Loader::create_instance，
AgentIndex::load 已使用该操作。实施时验证这条启动路径，
不增加就绪服务、gate、标志位、事件或消费方重试策略。

发布前，稳定 sessions 服务沿用已有 reloading 回复；调用方沿用该回复的现有重试行为，
Scheduler 恢复须重试，不能据此把正常持久 Session 判为 lost。初始化返回错误时走已有
必需提供者失败路径。逐记录不可用诊断仍可记录并继续装载其他所有者，但启动受下述进程无进展策略限制；
读取持久 Agent 与所有权记录失败仍是错误。索引重载保留 AgentDirectory 中的 Agent 与 Session。

serve 和 RunningServer::restart 的进程装配遵循
[How Dim uses rutis](../../../design/rutis-usage.md#appendix-a-rust-binding) 的范围内无进展策略。
持续有行激活时，初始化总耗时可以超过一个间隔；进程打开停滞时失败并清理，
不发布部分初始化的索引。单个持久 Agent 沿用已有创建超时：
若该超时先到且进程初始化仍可继续，记该 Agent 不可用，并继续装载其他所有者。
依赖驱动的 Agents 重载沿用相同单 Agent 策略；真实返回的初始化错误仍是错误。
不增加固定的启动或重启总预算。

验收覆盖执行关闭时不装载默认或持久 Agent、执行开启时的空库与已有数据、
坏记录旁的正常所有者、进程停滞失败清理，以及持续有进展的启动期间持久 Agent 的创建超时。
重启和依赖驱动的重载须把尚未运行的持久 Agent 创建超时记为不可用后继续。
索引重载须保持已装载 Session 不变；初始化期间的查询在发布前回复 reloading，
不能把正常持久所有者判成永久失败。
#318 的 Scheduler 整宿主恢复严格断言必须通过。不改变 Session 输入、权限、记录格式、
bridge 或 rutis 合同；Scheduler 与 Workflow 继续使用现有 S2 接口。

## 附录 A. Rust 绑定

类型放在 [agents/sessions.rs](../../../../crates/dim-app-server/src/agents/sessions.rs) 中 `AgentIndex` 旁边。其他 crate 中的 Rust 插件使用具名服务。

```rust
pub struct SessionRef { pub session_id: String, pub branch_id: String }
pub struct LaunchRequest {
    pub agent: Option<String>,
    pub cwd: String,
    pub model: Option<ModelId>,
    pub policy: Option<PermissionPolicy>,
    pub starter: Option<SessionRef>,
}
pub struct LaunchReply { pub session_id: String, pub policy: PermissionPolicy }
pub struct InputWrite {
    pub session_id: String, pub branch_id: String,
    pub mode: InputMode, pub submission_id: String,
    pub raw_blocks: Vec<RawBlock>, pub content: Vec<ContentBlock>,
}
pub struct RawBlock { pub text: String, pub key: Option<BlockKey> }
pub struct BlockKey { pub r#use: KeyUse, pub name: String }   // KeyUse：Live | Retire
pub struct InputReceipt { pub input_id: String, pub sequence: u64 }
pub struct LookupRequest { pub session_id: String, pub branch_id: String, pub submission_id: String }
pub enum LookupReply { Accepted { input_id: String, sequence: u64 }, Unknown }
pub struct HistoryRequest {
    pub session_id: String, pub branch_id: String,
    pub after: Option<u64>, pub through: Option<u64>,
    pub at_least: Option<u64>, pub limit: Option<usize>,
}
pub struct HistoryPage { pub through: u64, pub events: Vec<Arc<AgentEvent>> }
pub struct WithdrawRequest {
    pub session_id: String, pub branch_id: String,
    pub input_id: String, pub reason: Option<String>,
}
pub struct StopRequest { pub session_id: String, pub branch_id: String, pub reason: Option<String> }
pub struct SessionsError { pub code: SessionsErrorCode, pub message: String }  // retryable() 由错误码决定

impl AgentIndex {
    pub async fn launch(self: &Arc<Self>, request: LaunchRequest) -> Result<LaunchReply, SessionsError>;
    pub async fn input(&self, write: InputWrite) -> Result<InputReceipt, SessionsError>;
    pub async fn lookup(&self, request: LookupRequest) -> Result<LookupReply, SessionsError>;
    pub async fn history(&self, request: HistoryRequest) -> Result<HistoryPage, SessionsError>;
    pub async fn withdraw(&self, request: WithdrawRequest) -> Result<Withdrawal, SessionsError>; // Withdrawal 来自 S1
    pub async fn stop(&self, request: StopRequest) -> Result<u64, SessionsError>;
    pub async fn idle(&self, at: SessionRef) -> Result<u64, SessionsError>;
    /// S5c 的删除，作为 `sessions.delete {sessionId}` 提供（§6.8）。
    pub(crate) async fn delete(&self, id: &str) -> dim_agent_runtime::Result<()>;
}
```

请求类型以 camelCase 字段与 `deny_unknown_fields` 派生 `Deserialize`，回复类型派生 `Serialize`。具名服务把每个请求对象（A.1）解码成对应的请求类型，调用方法并编码信封；`withdraw`、`stop` 与 `idle` 的回复在那里编码为 `{status, sequence?}` 与 `{sequence}`。对 `delete`，它把 `{sessionId}` 解码为 Rpc 的 `protocol::SessionId`，按 `From<AgentError>` 映射 `AgentError`（§9），并回复 `{}`。上限是 `launch` 旁边一个作用于两个 `PermissionPolicy` 值的函数。每个方法的租约是 `acquire` 得到的私有 `SessionUse`。`input` 与 `lookup` 调用门面的 `Session::input` 与 `Session::lookup`，Rpc 的 `session.submit` 与 `session.input_lookup` 也调用它们。
