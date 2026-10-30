# SessionStore Session 删除、清理与分叉

状态：已实现。路线图工作项 S5c（[#61](https://github.com/arcships/dim/issues/61)、
[#97](https://github.com/arcships/dim/pull/97)），在 S5a（[#59](https://github.com/arcships/dim/issues/59)）及其设计
[Session 文件](session-files.md) 之后，连同 [#127](https://github.com/arcships/dim/issues/127)（Collaboration）。§16 列出实现中的改动。
总体设计：[附录 B.2](../overview.md#b2-app-server-进程插件) 的 Rpc、Agents 与 SessionStore 行。作用域：
[ADR 002](../../adr/002-plugin-scope.md) 进程作用域，位于现有的 Agents 与 SessionStore 插件内，另加两个 Rpc 内建方法。
使用方：通过 `session.delete` 和 `session.fork` 调用的客户端，以及 CLI 的 `dimcode session` 命令（F6，
[#77](https://github.com/arcships/dim/issues/77)）。

## 1. 概要

客户端用 `session.delete` 删除 Session，用 `session.fork` 分叉 Session。两者都是协议 v3 的 Rpc 内建方法，调用 Agents；Agents 负责加载、关闭和创建 Session，SessionStore 执行行与文件的步骤。

- **删除。** Agents 不再加载该 Session；若它已加载，就关闭它，让各能力结算在途工作并释放所持资源。随后 SessionStore 移除该 Session 的命令输出目录，并在一个事务内删除存储为它保存的所有行，包括 Session 文件的所有者行。失去最后一个所有者的文件由 S5a 的清扫移除。删除不存在的 Session 也成功。
- **清理。** 一次删除移除该 Session 拥有的一切（§3.3）。按时间或保留数量清理并预览，由 CLI 组合 `session.summaries` 与 `session.delete` 完成，没有对应的服务端方法。
- **崩溃安全。** 目录的移除在行事务提交之前落盘，Session 文件按 S5a 的行处理。删除在任何一步中断，都只会留下完整的 Session 或者它的行全部消失，重试或下一次清扫会完成剩余工作（§4）。
- **分叉。** 分叉在一个事务内把源 Session 的 `main` 截至某个切点的部分按存储的字节复制到一个新 Session，并共享源的 Session 文件。切点必须是 `main` 的静止点，`main` 到那时创建的分支只复制目录、不带事件，因此客户端给分叉发送输入之前，分叉不会自己开始任何工作（§6.5）。分叉的 `main` 以 `branch/resume` 打开：分叉就是源的对话在切点处、以新 id 恢复。分叉紧接在复制的事实之后写下自己的一个事实 `session/forked`，让折叠分支自身历史的所有者知道复制到哪里为止（§6.5）。`SessionConfig` 整体复制，`interactive` 和权限预设随之带过去，不放宽任何权限。
- **格式。** S5c 不改格式。新事件行是在 v16 下加入的开发阶段扩展，在它之前构建的守护进程读不了；格式 v17 原样保留它（§8）。不加来源列，不写墓碑。

新增一种事件 `session/forked`（数据为空），不改 Timeline、Dispatcher、TimelineTree、Prompt、rutis 或 Session 门面。

## 2. 结构

```
 客户端 -- session.delete {sessionId} ----------+
        -- session.fork {sessionId, at} --------+
                                                v
 进程作用域（控制节点）                Rpc 内建方法（v3）：参数格式、协议版本
                                                |
                                                v
                         Agents（AgentIndex）：每个 Session 的准入门、
                         关闭、从另一个 Session 的切点创建
                           |                                 |
                           | delete_session(S)               | 预备创建，
                           |                                 | 预备步骤 = fork_session(S, F, at)
                           v                                 v
                         SessionStore ---- 各一个事务 ----> SqliteStorage
                           |      \
                           |       +-- S5a 清扫（Session 文件）
                           | 每次调用按 Session 的资源节点查找（#38）
                           v
 资源节点（P1/P2 即控制节点）              FileStorage: remove(command-output/<S>)
```

Rpc 检查参数和 Session 的协议版本，然后调用 Agents。Agents 负责 Session 的生命周期：`acquire` 恢复 Session 时已经持有的准入门、`Session::close` 和预备创建（`create_prepared_session`）。SessionStore 负责数据库目录下的行和文件。它新增的步骤是作用于原生存储的 Rust 方法，不是服务：只有 Agents 调用。

## 3. 删除

### 3.1 方法

`session.delete {sessionId: string} -> {}`

| 项目 | 规则 |
| --- | --- |
| 类别 | Rpc 内建方法，与 `session.create`、`session.close` 并列。它不是 `sessionMethods` 条目，因为 Session 不能通过自己的部件卸载自己；也不是管理方法：它作用于一个 Session，Rpc 对 Session 的检查都适用 |
| 协议 | v3。v1 与 v2 得到 `unknown_method`（"Method requires protocol v3"）。`hello.features` 在列出 `session.create` 的条件下列出它。协议版本仍为 3：客户端通过 `hello.features` 发现新的内建方法，与发现 `session.summaries` 相同 |
| 调用方 | 能调用 `session.close` 的连接都能调用，配置规则相同：受保护配置下只限本地传输。已配对设备（P3）在 RemoteGateway 的兼容性表中与其他 Session 内建方法一起决定 |
| Session 版本 | Session 记录的协议版本必须等于连接的版本（`session_protocol_version`），与所有 Session 内建方法相同。没有可读所有者行的 Session 没有记录版本，检查通过 |
| 已损坏的 Session | 按 id 删除。删除不读取 Session 的任何记录，因此校验失败或没有可用所有者的 Session 也能删除；Rpc 对不可用 Session 的检查不适用于 `session.delete` |
| 回复 | Session 的行全部删除后回复 `{}`。未知或已删除的 id 也回复 `{}` |

### 3.2 顺序

```
 Rpc          Agents                        Session S            SessionStore              FileStorage
  | delete(S) ->|                              |                     |                          |
  |             | 1 取得 S 的准入门            |                     |                          |
  |             | 2 已加载？close() ---------->| 结算已接受的工作，写终态事实，
  |             |                              | 释放进程、租约、成员请求；
  |             |<-------------- 已关闭 -------|                     |                          |
  |             | delete_session(S) ------------------------------->|                          |
  |             |                              |  3 移除 command-output/S，同步父目录 --------->|
  |             |                              |    不存在视为已移除 <--------------------------|
  |             |                              |  4 在 Timeline 写线程上的事务：所有者行、      |
  |             |                              |    Timeline 行、owner、记录 -> 提交            |
  |             |                              |  5 安排 S5a 清扫                               |
  |             | 6 忘记 S；移除其槽位         |                     |                          |
  |<---- {} ----|                              |                     |                          |
```

1. **准入。** Agents 检查 id（D3），并取得 S 的准入门，即 `acquire` 恢复 Session 时持有、创建在其构建的 Session 发布之前一直持有的 `SessionSlot` 门（§7）。删除进行期间没有请求、订阅或分叉能加载 S，删除仍在构建的 Session 时会等构建结束。已经持有 S 使用权的请求继续持有，由第 2 步的关闭结束其工作。
2. **关闭。** 若 S 已加载，Agents 像 `session.close` 那样关闭它：业务关闭，不是卸载。各能力结算已接受的工作，写出终态事实，释放进程、FileStorage 租约和成员请求。关闭报告清理错误时记日志并继续删除：未能写出的事实会随其余部分一起删除。从关闭起 S 的槽位即关闭：无论行最终是否删除，S 在本进程内都不再加载（第 6 步）。
3. **命令输出。** SessionStore 通过 S 的资源节点上的 FileStorage（与 S5a 一样每次调用时查找）移除 `<数据库目录>/command-output/<S>/`，WorkspaceTools 在这里保存 exec 输出和过大的 `read` 结果（[read.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/files/read.rs) 的 `offload`）。目录不存在视为已移除。移除必须在第 4 步之前落盘（§4），因此 FileStorage 的 `remove` 要同步被移除条目的父目录（§14）。失败时删除在改动任何行之前结束（§3.5）。只有本数据库有 S 的行时，SessionStore 才移除这个目录：`command-output/` 由同一目录下的所有数据库共用，本库没有行的 id，其目录可能属于另一个数据库的 Session（§4）。
4. **行。** 一个 `IMMEDIATE` 事务删除 S 的全部行：`session_file_owners`、`timeline_snapshots`、`timeline_events`、`timeline_branches`、`timeline_content`、`session_owners`、`session_catalog` 和 `agent_sessions`，因外键按此顺序，另加 S5b 的标题元数据（§14）。`session_catalog` 是 [#556 的目录投影](../../../design/appserver/native-storage-format.md#556-v23-格式决定)的派生行。这就是如今创建回滚执行的操作（[timeline.rs](../../../../crates/dim-app-server/src/storage/timeline.rs) 的 `rollback_creation`），S5a 为它加上所有者行；删除与回滚共用它。它像回滚一样在 Timeline 写线程上执行，因此排在已为 S 排队的所有追加之后，并清除 S 记下的写入失败。
5. **清扫。** SessionStore 安排 S5a 的清扫（[Session 文件 §5.4](session-files.md#54-清扫)）。最后一个所有者是 S 的文件在清扫中移除；仍有分叉拥有的文件保留。
6. **忘记。** Agents 删除 S 的协议版本和不可用 Session 条目，把 S 的槽位标为已关闭、已删除，并在没有其他东西持有它时移除它（[空闲卸载 §6.4](idle-unload.md#64-准入槽)）。仍在准入门处等待的调用方会看到槽位已关闭，之后的调用方会发现 S 既没有所有者也没有行：两种情况下 `acquire` 都回答 `session_closed`。在第 4 步之前失败的删除则释放准入门。被它关闭的 Session 保持 `session.close` 之后的状态：行还在，`session.list` 不列出它，本进程内不再加载，重试即可删除；重启后它照常加载。删除没有关闭的 Session 在下次使用时照常加载（§16 D5）。

### 3.3 删除移除什么

| 状态 | 位置 | 由谁移除 |
| --- | --- | --- |
| Session 记录、所有者、分支、事件、内容、快照 | 原生存储 | 第 4 步，一个事务 |
| Session 文件的所有权 | `session_file_owners`（S5a） | 第 4 步。失去所有者的文件由 S5a 的清扫处理 |
| 标题元数据 | 原生存储，S5b 的 `session_metadata` 行（[标题元数据](session-title-metadata.md#9-生命周期)） | 第 4 步，同一事务 |
| exec 输出和转存的 `read` 结果 | `<数据库目录>/command-output/<S>/` | 第 3 步 |
| 已加载实例、租约、订阅、在途使用 | 进程内存 | 第 2 步 |
| 协议版本、准入槽位、不可用 Session 条目 | Agents 内存 | 第 6 步 |
| 指向 S 的 Collaboration 关系，以及因此没有 Session 的临时 Agent | Collaboration 内存 | Collaboration 对 S 的结束处理，把业务关闭当作 Session 的结束（[collaboration.rs](../../../../crates/dim-app-server/src/collaboration.rs) 的 `session_ended`）。已加载的 S 经关闭观察者（`session_closed`）进入这一处理；删除随后失败时也成立：关闭的 Session 在本进程内不再加载（第 6 步）。未加载的 S（例如被空闲卸载释放的）没有关闭：第 6 步之后，Agents 发出 `UnloadedSessionDeleted`，由它进入这一处理。在 Collaboration 重载期间结束的删除（S2 的删除可能如此）没有监听者，由 Collaboration 再次装载时核对（[空闲卸载 §7](idle-unload.md#7-竞争与交互)） |

删除不移除：

- **其他所有者记录中指向 S 的内容**，例如 Scheduler 与 Workflow 存在 `pluginFiles` 中的运行记录。这些所有者使用该 id 时会得到 `session_closed`，与 Session 已关闭时相同。
- **其他 Session。** 被删除的 leader 的成员、S 的分叉都是有自己行的 Session。删除不级联。
- **宿主配置**（按工作区的 Provider 选择、MCP 项目启用），不属于任何 Session。

### 3.4 客户端与订阅

- 第 2 步关闭 S 时，附着在 S 上的连接收到现有的 `session.state` 和 `session.closed` 通知，与 `session.close` 相同。不加原因为 "deleted" 的通知：`session.closed` 在行事务提交之前发出，可能宣告一次随后失败的删除。协议也没有新建 Session 的列表通知，所以删除也不加。
- 回复之后，对 S 的 `session.attach`、Session 方法和 `session.fork` 都失败于 `session_closed`，`session.list` 和 `session.summaries` 不再列出它。
- 发出删除的连接本身也可能附着在 S 上，它先收到通知，再收到回复。

### 3.5 幂等与错误

每一步都容许目标已不存在，因此任何失败或回复不确定之后都可以重试删除。

| 结果 | 何时 | 之后的状态 |
| --- | --- | --- |
| `{}` | 行已删除，或本来就没有 | Session 不存在；失去所有者的文件由清扫处理 |
| `unknown_method`（-32601） | 协议 v1 或 v2 | 不变 |
| `invalid_params`（-32602） | 缺少 `sessionId` 或不是字符串 | 不变 |
| `session_protocol_version` | S 属于另一个协议版本 | 不变 |
| `product_integration_pending` | 该配置不提供 Session 执行 | 不变 |
| `-32603` "Session operation failed" | 第 3 步失败（`Locked`、`DirectoryNotEmpty`、`Unsynced`、I/O、节点不可用）或第 4 步失败（SQLite） | 若第 2 步关闭了 S，S 已关闭但未删除，与 `session.close` 之后相同：行还在，本进程内不再列出、不再加载，关闭结束的关系也不恢复。重启后它照常加载，命令输出可能已部分移除。没有加载的 Session 不变。重试 |
| 没有回复 | 回复之前连接或 Rpc 已结束 | 两种状态之一。重试 |

第 3 步的 `Locked` 和 `DirectoryNotEmpty` 只会来自已关闭 Session 之外的写入方，例如比 shell 活得更久的 PowerShell 子进程，或打开着文件的其他进程。删除失败，等它们退出后重试。

## 4. 崩溃收敛

```
            2 关闭             3 移除（已同步）             4 提交                5 清扫
 [已加载] ---------> [已关闭] -------------------> [无目录] -----------> [无行] ----------> [消失]
    ^                    |                              |                     |
    |                    | 崩溃：行都在，               | 崩溃：行都在，      | 崩溃：无所有者的
    +--- 下次使用 -------+ 目录部分移除                 | 目录已无            | 文件行（S5a）
         加载 S            重试删除                       重试删除              下一次清扫
```

| 中断点 | 存储 | 磁盘 | 收敛方式 |
| --- | --- | --- | --- |
| 关闭之前或期间 | 行都在 | 目录在 | 没有损失；S 下次使用时加载；重试 |
| 移除期间 | 行都在 | 目录部分移除 | 重试：继续移除，不存在的条目视为已移除 |
| 移除之后、提交之前 | 行都在 | 目录已无，已落盘 | 重试。期间 S 可以加载，只是没有命令输出文件 |
| 提交之后、回复之前 | 无行 | 目录已无；S 的文件无所有者 | 客户端重试时本库已无行，不移除目录，得到 `{}` 并再次安排清扫；否则由 S5a 的下一次触发（另一次删除，或之后某一代的第一次写入）移除文件 |
| 清扫期间 | S5a 的状态 | — | [Session 文件 §6](session-files.md) 的崩溃恢复表 |

**为什么目录在提交之前移除。** 没有任何行记录命令输出目录。行删除之后，孤立的目录只能靠列出 `command-output/` 找到，而这个目录由同一数据库目录下的所有数据库共用（一个目录下两个 `--database` 文件），列目录无法区分已删除 Session 的目录和另一个存储的目录。先移除目录，任何已提交的删除都不会留下它。S5a 的文件采用相反的顺序，因为有行记录它们，清扫能找到。FileStorage 的移除原本不同步（[S6](https://github.com/arcships/dim/pull/87)）：断电后，被移除的目录可能重新出现，而之后以 `synchronous=FULL` 提交的事务仍然有效。现在 `remove` 在返回前同步父目录（§14），消除了这个窗口。

**只移除本库有行的 id。** 出于同样的原因，只有本数据库有该 id 的行时，删除才移除它的目录。同一目录下 `a.sqlite` 和 `b.sqlite` 两个守护进程共用 `command-output/`；把 `session.delete {S_b}` 发给前者（例如 CLI 用错了 `--database`）时，它回复 `{}`，不能移除仍在后者中运行的 S_b 的输出。这个检查不影响收敛：目录在行之前移除，所以本库没有 S 的行时，也已没有 S 的目录可移除；Agents 的准入门保证同一 id 的两次删除不会交错。因此提交之后的重试什么都不移除，也不需要移除。

被否决的方案：

| 方案 | 原因 |
| --- | --- |
| 先提交，再移除目录 | 两者之间崩溃会留下没有任何东西指向的目录，而在共用的数据库目录里列目录并不安全 |
| 用墓碑行记录待清理的已删除 id | 为一个目录改格式（v16），而上述顺序不需要它就能收敛 |
| 用卸载代替关闭 | 卸载不结算已接受的工作，也不停止成员请求（AGENTS.md 第 8 条） |

## 5. 清理

### 5.1 按时间或数量清理

[功能清单](../../../feature-list.md)要求"按时间或保留数量清理，并可预览"。由 CLI 组合实现：`session.summaries` 为连接所属协议版本的每个 Session 返回 `createdAt`、`updatedAt` 和预览；CLI 选出要删的 Session、展示所选内容，再逐个调用 `session.delete`。失败的 id 会报告出来并可重试，其余照常删除。

被否决：`session.cleanup {olderThan, keep, dryRun}` 这样的服务端方法。它重复了客户端已有的选择和预览；保留策略需要一个业务所有者和配置，而目前没有任何定义；部分失败时仍要逐个 id 返回结果。

### 5.2 孤立物

| 类别 | 处理 |
| --- | --- |
| Session 文件 | S5a 的清扫：每次删除之后（包括重试的删除），以及每一代的第一次写入 |
| 命令输出 | 不需要：提交之前已移除（§4） |
| 已删除 Session 的行 | 不需要：一个事务 |
| 以前的故障留下的行，例如没有记录的 Timeline 行 | 按该 id 删除即可移除：每张表都按 `session_id` 清除，不论 `agent_sessions` 中是否有该行 |

## 6. 分叉

### 6.1 方法

`session.fork {sessionId: string, at: u64} -> {sessionId: string, sequence: u64}`

| 项目 | 规则 |
| --- | --- |
| 类别 | Rpc 内建方法。它像 `session.create` 一样创建 Session，只是从另一个 Session 的切点创建（Agents） |
| 协议与调用方 | 与 `session.delete` 相同（§3.1）。源必须属于连接的协议版本，分叉也采用该版本 |
| `at` | 分叉继承的最后一个 `main` 序号。0 表示不继承任何事件 |
| 回复 | 分叉发布后的 id 及其 `main` 头部。客户端随后附着 |
| 没有其他参数 | 没有 `branchId` 或配置字段。分叉的配置就是源的配置（§6.3）。客户端之后用 `session.permission.update` 或 `session.model.update` 修改，与 [#26](https://github.com/arcships/dim/issues/26) 对预设的决定相同 |

分叉不是幂等的：回复不确定后重试可能再建一个分叉，客户端可在 `session.summaries` 中找到它。

### 6.2 切点

切点在 `main` 上，规则与 `session.branch_create` 相同（[tree.rs](../../../../crates/dim-app-server/src/connection/tree.rs) 的 `create_branch`）：`at` 不超过已发布的 `main` 头部，位于原子批次的末尾，并且 `history::validate_context_boundary` 接受 `at` 处的上下文（没有缺少结果的工具调用，没有未结束的模型输出）。Agents 获取源 Session 来做这项检查，需要时先恢复它，与 `session.branch_create` 相同。`at` 及之前的行不会改变，分叉期间的追加不影响复制。

`at` 还必须是 `main` 的**静止点**：没有未结束的轮次，每个 Queue 或 Steer 输入都已消费，每个后台任务都已结束，并且它发出的唤醒输入（如果有）也在 `at` 及之前且已消费。如今重载正是从后两类事实开始工作的：Loop 为已采纳但未消费的 Queue 或 Steer 输入启动轮次（[agent_loop.rs](../../../../crates/dim-agent-capabilities/src/agent_loop.rs) 的 `rebuild`），InputQueue 在轮次结束时采纳排在它后面的 Queue 输入（[input_queue.rs](../../../../crates/dim-agent-capabilities/src/input_queue.rs)），BackgroundExec 把重载时仍未结束的任务记为失败，并用 Steer 输入唤醒分支（[background.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs) 的 `rebuild`）。在 F 中，这些轮次会在源的 `cwd` 里重做源已经做过或正在做的工作。切点处未结束的轮次只会被重载记为失败，不会重新执行；要求切点处没有这样的轮次，F 打开时就不会多出一个源里没有的失败轮次。静止点上 F 的重载不会开始任何工作。Append 输入可以留下：它不启动轮次，会并入 F 中由客户端输入启动的下一次请求。Agents 在 `main` 截至 `at` 的前缀上、紧接 `validate_context_boundary` 之后读取这些能力的事实来做这项检查（§14、§16 D1）。常见的分叉点，即某轮结束之后或某条用户消息之前，都是静止点，除非当时有后台任务在运行。

**为什么只在 `main` 上。** 子分支的历史是各祖先的前缀加上它自己的事件，而每个分支的事件都从 1 编号。把它排成一条 `main` 要给事件重新编号，而压缩的 `through`、`branch/create.at`、`branch/start.createEvent`、批次终点和快照锚点都引用这些编号：存储将不得不改写其他所有者写下的事实。要继续某个子分支的工作，客户端在 Session 内从它创建分支。

被否决：以相同的分支 id 复制子分支的祖先链，并在子分支的副本上继续。那样分叉的对话就不在它的 `main` 上，而客户端、`session.list` 和 SessionTitle 读的都是 `main`。

### 6.3 分叉复制什么

```
 源 S                                                     分叉 F（一个事务）
 main: 1 .. 7  8 branch/create(c1)  9 .. 12 | 13 ..       main: 1 .. 12，id 相同      (at = 12，静止点)
                      |                  切点                   然后 13 session/forked；F 首次加载
 c1:   1 branch/start .. 30（可能仍在增长）                     写 14 branch/resume、15 branch/create-failed(c1)
 c2:   在 main 15 处 branch/create                       c1:   目录（存储的 info，头部 0），没有事件
                                                         -     c2 不复制（在切点之后创建）
 timeline_content: 事件引用的行                          只复制被复制事件引用的行及其到达的列表节点
 timeline_snapshots: main、c1                            main 的，锚点在 12 及之前
 session_file_owners: (S,f1) (S,f2)                      (F,f1) (F,f2)，不复制字节
 agent_sessions.config: 存储的文本                       相同的文本
 session_owners: (S, Agent A, v3)                        (F, Agent A, v3)
 session_catalog: 派生行                                 一行，与回填这些事件相同
 S5b 标题元数据                                          不复制
```

| 表 | F 得到的行 |
| --- | --- |
| `agent_sessions` | 新 id（UUID v4）、分叉时刻的 `created_at_ms`，以及原样的源 `config` 文本，也就是整个 `SessionConfig`，包括 `interactive`（[Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-字段)）、`permissionPolicy`、`collaborationRole`、`cwd`、`model`、`systemPrompt` 和 `tools`。复制文本而不重新序列化，`interactive` 的省略规则保持原样 |
| `session_owners` | 源的 Agent 与协议版本 |
| `session_catalog` | 给 F 一行，取值与按 F 的事件回填相同：`updated_at_ms` 是分叉事务最后写入的事件的 `create_at`——非零处为 `at + 1` 的 `session/forked`，`0` 处为 F 的创建时间——`preview` 是被复制事件里按同一按批次完整的窗口的第一条用户输入，`0` 处为 NULL（[#556 的目录投影](../../../design/appserver/native-storage-format.md#556-v23-格式决定)） |
| `timeline_branches` | 头部为 `at + 1` 的 `main`（即它的 `session/forked`；在 `0` 处分叉为 `0`），以及 `branch/create` 位于 `main` 上 `at` 及之前的每个分支，保留存储的 `info`，头部为 0。其下的分支不复制：它们的创建事件在 F 拿不到的事件里 |
| `timeline_events` | `main` 截至 `at` 的行（切点规则保证没有行跨过它），种类、正文、序号、批次终点、事件 id 和 before 轨迹都按存储的样子；随后在 `at + 1` 新写一行 `session/forked`（在 `0` 处分叉不写）。不含任何子分支的行 |
| `timeline_content` | 被复制事件按编解码器写入的引用（请求及其 before 轨迹）所引用的行，以及这些引用到达的每个列表节点（格式 v17）。`session/forked` 行不引用内容 |
| `timeline_snapshots` | 锚定在被复制的 `main` 事件上的行。它们由相同的事实派生，F 首次加载时可以直接使用，不必从头重放 |
| `session_file_owners` | S 拥有的每个文件给 F 一行（[Session 文件 §4](session-files.md#4-分叉时共享)） |
| 标题元数据（S5b） | 不复制：分叉没有标题（[SessionTitle](../plugins/SessionTitle.md) A7） |

**为什么子分支不带事件。** 子分支的事件与 `main` 的切点之间没有先后顺序，子分支没有哪个前缀对应 `at`。整体复制的子分支带着切点之后发生的事，它待处理的工作（尚未开始的第一个输入、Queue 或 Steer 输入、后台任务的唤醒）会在 F 中启动轮次，在同一个 `cwd` 里重做源已经做过或正在做的工作（§6.5）。存储仍要求每个 `branch/create` 都有对应分支的目录，并在加载 Session 时检查（[timeline.rs](../../../../crates/dim-app-server/src/storage/timeline.rs) 的 `validate_session`）；头部为 0 的目录能通过检查。TimelineTree 把缺少 `branch/start` 和第一个 Queue 输入的子分支当作从未开始的创建（[timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 的 `restore`）：F 首次加载时，它在 F 的 `main` 上写 `branch/create-failed`（`error: "branch creation failed"`），不构建这个分支；之后的加载看到这条事实就跳过它。创建已提交、子分支尚未开始时崩溃，走的就是这条路径。在 F 中，这条事实表示该分支不属于 F；分支和它的工作留在 S 中，客户端可以在那里继续。这条事实不进入模型上下文，切点处的提示词前缀不变。

**为什么只复制被引用的内容。** 内容行按 Session 存放。全部复制的话，从长 Session 的早期分叉会把切点之后所有请求的内容也复制过去，而复制期间一直持有存储的写锁。

被否决的方案：

| 方案 | 原因 |
| --- | --- |
| 按分叉事务执行时已提交的样子整体复制每个子分支（本页初稿） | 把子分支在切点之后的工作带进 F，重载规则还会在共用的 `cwd` 里重新执行它待处理的输入和唤醒（§6.5） |
| 子分支只复制到创建为止 | 它的第一个 Queue 输入重新变成待处理，F 会把委托的任务再执行一遍 |
| 子分支复制到它最后一个静止点 | 仍然带着切点之后的工作。还在做第一个任务的子分支在创建之后没有静止点，而且检查要在分叉事务持有写锁时读取每个被复制子分支的事实 |
| 复制源的全部内容行 | 为切点之后、没有被复制事件引用的请求付出磁盘和锁的时间 |

复制是一个写事务；所有 Session 的提交都要等它，就像等任何一轮写入一样，时长随被复制的 `main` 前缀增长。

### 6.4 分叉不复制什么

| 不复制 | 原因 |
| --- | --- |
| `at` 之后的 `main` 事件，`at` 之后创建的分支 | 在切点之后 |
| 子分支的事件，以及其下的分支 | 与切点之间没有先后顺序，重载还会重新执行其中待处理的工作（§6.3）。它们的工作留在 S 中 |
| 已加载实例、租约、订阅、已附着的客户端、在途请求 | 运行时状态，不存储；F 自己建立 |
| 正在提交中的输入 | 尚未提交，不是事实 |
| 命令输出目录 | WorkspaceTools 的每 Session 目录。F 复制来的工具结果引用 `command-output/<S>/` 下的路径，S 存在期间可读（§6.6） |
| 标题元数据 | SessionTitle A7 |
| Collaboration 关系和临时 Agent | 源团队的进程内存。F 不领导任何成员，S 的临时 Agent 对 F 不可见 |
| 宿主配置 | 从不属于 Session |

### 6.5 打开分叉

Agents 用现有的预备创建来创建 F：预备步骤是 SessionStore 的分叉事务，而不是 `create_owned_session`。随后运行时校验 F 的行并构建 F。失败时回滚 F，包括它的所有者行（[Session 文件 §5.3](session-files.md)）；源不受影响。

TimelineTree 以 `branch/resume` 打开已有事件的根分支（[timeline_tree.rs](../../../../crates/dim-agent-capabilities/src/timeline_tree.rs) 的 `restore`），因此 F 的 `main` 在复制来的事实之后接着写 `branch/resume`。在此之前，分叉事务自己在复制来的事实之后写一条自己的事实：位于 `at + 1` 的 `session/forked`（数据为空），因此 F 不会在没有它的情况下存在。它表示 F 的 `main` 中此前的每条事件都是复制来的。按分支自身历史折叠的所有者（Goal，A11）从它之后开始；其他所有者仍按各自的重载规则把复制来的事实当作 F 的历史。在 `0` 处分叉不复制任何东西，不写 `session/forked`，像新 Session 一样以 `branch/start` 打开。F 就是源的对话在切点处、以新 id 恢复，每个所有者按现有的重载规则处理。切点是静止点（§6.2），子分支不带事件（§6.3），所以没有哪条重载规则会开始工作：**客户端给 F 发送输入之前，F 不运行任何轮次、模型调用或工具。** F 与 S 共用 `cwd`，这一点保证分叉不会重复源的副作用。

| 切点处 | 在 F 中 |
| --- | --- |
| 模型配置（`context/configured`）、提示词、已采用的工具 | 保留。恢复的分支保留其配置，Prompt 像任何恢复时一样投递来源变化，因此提示词前缀与 S 在切点处相同 |
| 权限预设（`permission/changed`） | 保留。Permission 折叠复制来的事实（#26）；`SessionConfig` 中的策略也一并复制 |
| 未结束的轮次、未消费的 Queue 或 Steer 输入、运行中的后台任务 | 不存在：切点是静止点（§6.2） |
| 未消费的 Append 输入 | 保持待处理，并入 F 中由客户端输入启动的下一次请求 |
| `main` 创建的分支 | 只有目录，没有事件。F 首次加载时 TimelineTree 在 F 的 `main` 上写 `branch/create-failed`（§6.3） |
| 已送达但没有结果的 Collaboration 请求 | Collaboration 写出失败结果（`capability reloading`）。重载写出的结果不投递报告输入，因此不启动轮次。它不停止源的成员：重载只停止成员关系以重载中的 Session 为 leader 的成员（[#127](https://github.com/arcships/dim/issues/127)，§14） |
| 报告输入在切点之后的 Collaboration 结果 | F 保留结果但没有它的报告：重载不投递报告，与源在这段窗口内崩溃后相同。不会启动任何工作；模型仍可用 `agent` 工具读取结果 |
| 生效中的目标（Goal，P5） | Goal 的 A11（§14） |

**切点之后的子分支。** 子分支在自己的 Timeline 上运行，`main` 同时继续，所以源里可能有切点之后的子分支事件。F 一条都没有：

```
 1. 子分支在切点之后完成
 S main: .. 8 branch/create(c1) .. 10 step/tool-result "Branch created: c1" .. 12 turn/end | 13 ..
                                                                                         切点
 S c1:   1 branch/start  2 input/message（任务）  3 turn/start .. 25 turn/end   （在切点之后）
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   只有目录

 2. 分叉提交时子分支正在运行
 S main: .. 8 branch/create(c1) .. 12 turn/end | ..
 S c1:   1 .. 5 turn/start  6 step/tool-call (exec)   <- 分叉事务在这里提交
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   只有目录：没有要记为失败的轮次，没有要采纳的输入，没有要唤醒的任务

 3. 子分支在切点之后收到输入
 S main: .. 8 branch/create(c1) .. 12 turn/end | ..
 S c1:   1 .. 9 turn/end  10 input/message（steer，来自客户端）  11 turn/start ..
 F main: .. 12 turn/end  13 session/forked  14 branch/resume  15 branch/create-failed(c1)
 F c1:   只有目录
```

| 情况 | 在源中 | 在 F 中 |
| --- | --- | --- |
| 1. 子分支在切点之后完成 | 没有任何东西回到 `main`。TimelineTree 没有交回机制：`branch` 工具在子分支开始后就返回 `Branch created: <id>`，子分支的轮次在它自己的 Timeline 上结束。`wait` 只被后台任务通知和 steer 唤醒，不等子分支 | F 的 `main` 有这次创建，若由 `branch` 工具创建还有那条工具结果，都在切点之前，之后是 `branch/create-failed`。它不会收到结果，不等待任何东西，也不记其他失败。子分支的结果仍可在 S 中读取。Dim 唯一的交回机制是 Collaboration 从成员 Session 交回结果，见上表 Collaboration 一行 |
| 2. 分叉提交时子分支正在运行 | 它的轮次、模型调用或工具批次尚未结束 | 不复制子分支的任何事件，所以没有东西被记为失败、采纳、唤醒或在共用的 `cwd` 里重新执行。F 的 `main` 收到的是 `branch/create-failed`，不是"委托失败"输入：不存在这样的输入 |
| 3. 子分支在切点之后收到输入 | `main` 无法给子分支发送输入。子分支的输入来自客户端（`session.submit {branchId}`）、它自己的能力（exec 唤醒、Collaboration 报告）和 TimelineTree 的第一个输入 | F 没有子分支的任何输入，因此不含 F 从未收到的输入 |

被否决：在复制来的事实之后写 `branch/start`。Prompt 把根分支的 `branch/start` 当作新 Session，会采用 `SessionConfig` 中的模型和系统提示词，从而撤销复制来的事实中的模型变更，并改变提示词前缀（[prompt.rs](../../../../crates/dim-agent-capabilities/src/prompt.rs) 的 `open`）。要避免这一点，得改 TimelineTree（让已有事件的预备根分支以 start 打开）和 Prompt（区分分叉的 start 与新 Session 的 start）。而且 F 提交之后、写出 start 之前崩溃，F 仍会以 `branch/resume` 打开。

### 6.6 身份、来源与路径

- **事件 id。** 复制的事件保留原 id。事件由所属 Session 和 id 共同标识：存储按 Session 检查 id，并按 `(session_id, event_id)` 建索引。换新 id 就要改写所有引用它们的地方：`input_id`、`call_event_id`、压缩游标、before 轨迹、快照锚点和 Collaboration 的操作 id。
- **其他 id。** 分支、轮次、请求、调用、任务和审批 id 都保留，它们同样按 Session 划分。复制来的任务 id 指向一个在切点处已经结束的任务（§6.2）：BackgroundExec 从复制来的事实重建它，`session.exec.stop` 返回它记录的结果，不停止任何东西。
- **`submission_id`。** 它随 `input/message` 复制，`session.input_lookup` 读取 F 的历史，因此能在 F 上找到复制来的输入。[#32](https://github.com/arcships/dim/issues/32) 决定把该字段留在事件中（[sessions 服务 §6.3](sessions-service.md#提交标识存放在哪里)），分叉不需要为它另设记录。
- **来源。** F 不记录源：没有列，`session/forked` 也不带数据。目前没有任何决定使用它；Session 访问 A.3 规定 Session 的来源（`createdBy`）在有使用方时连同其格式决定一起加入。
- **路径。** Session 文件通过所有者行共享，只要还有所有者就有效（S5a）。命令输出不共享：F 复制来的工具结果引用 `command-output/<S>/…`，S 存在期间 F 可以读取。S 删除之后，读取这些路径会得到文件不存在，工具结果中的预览仍在。用户看到的后果：复制来的工具结果中被截成预览的 exec 输出或较大的 `read` 结果，在 F 中再也无法完整读取，模型和客户端都读不到。Session 文件不同：所有者行让它们比写入它们的 Session 活得更久。

S5c 中被否决：在 S 的分叉都删除之前保留 S 的命令输出。删除需要知道 S 有哪些分叉，就得在存储中加分叉关系（改格式），WorkspaceTools 的每 Session 输出也会多出它本没有的所有者。

**以后的改进。** 像 S5a 对 Session 文件那样，给命令输出加所有者行：分叉共享源的输出，最后一个所有者删除时才移除它。这要把 WorkspaceTools 的输出从每 Session 目录改为有所有者的条目，并需要单独的格式决定，因此不在 S5c 之内。

### 6.7 错误

| 结果 | 何时 | 之后的状态 |
| --- | --- | --- |
| `{sessionId, sequence}` | F 已发布 | F 存在 |
| `unknown_method` | 协议 v1 或 v2 | 什么都没创建 |
| `invalid_params`、`session_invalid_input` | 参数格式、`session.branch_create` 在同一位置会拒绝的切点，或不是静止点的切点（§6.2） | 什么都没创建 |
| `session_closed` | 源未知或已删除，包括源的删除在分叉事务之前提交 | 什么都没创建 |
| `session_protocol_version` | 源属于另一个协议版本 | 什么都没创建 |
| `session_build_failed` | F 构建失败 | F 已回滚 |
| `-32603` "Session operation failed" | 存储失败，包括 F 复制来的行校验失败 | 未提交任何东西，或 F 已回滚 |

## 7. 串行化与竞争

删除和分叉在唯一的原生存储连接的写事务上决定，与 [Session 文件 §6](session-files.md) 中的写入相同。Agents 的准入门让删除与加载有先后。

| 竞争 | 结果 |
| --- | --- |
| 分叉先提交，然后源被删除 | F 保留复制的数据和共享的文件 |
| 源的删除先提交，然后是分叉事务 | 分叉找不到源：`session_closed`，什么都没创建 |
| 删除的关闭发生在分叉检查切点期间 | 检查失败于 `session_closed` |
| 写入与删除或分叉 | Session 文件 §6 |
| 两次删除 | 第二次等准入门，然后发现没有行：`{}` |
| 删除仍在构建的 Session：行已提交并已列出的 `session.create`、成员创建或分叉 | 创建从其行提交之前起持有新 id 的准入门，直到 Session 发布、协议版本记下为止（D6）。删除等待，然后关闭已发布的 Session 并删除其行。创建失败时回滚它的行，删除什么也找不到 |
| 删除与 attach、submit 或 Session 方法 | 它们等准入门，然后失败于 `session_closed`；已持有使用权的请求随关闭结束 |
| 删除与该 Session 自己的追加 | 关闭结算它们；行事务在 Timeline 写线程上排在所有已排队的追加之后 |
| 分叉与对源的追加 | `at` 及之前的行不变，分叉也不复制其后的任何行：子分支目录不带事件 |
| 删除 Collaboration leader | 关闭它会像如今一样停止其成员请求；成员仍是 Session |
| 删除成员 | leader 对它的请求像如今一样以成员已关闭结算 |
| 对同一源的两次分叉 | 两个独立的 Session，各自拥有共享的文件 |

**S6 评审意见。** "SessionStore 要先关闭 Session、放掉租约"：第 2 步在任何移除之前。"`attachment.put` 与删除竞争时要预期 `Locked` 或 `DirectoryNotEmpty`"：写入的目录不属于任何 Session（S5a），删除从不移除它，S5a 的清扫只移除无所有者且不在写入中的文件，因此两者都不会碰到写入的租约。命令输出的移除只会因已关闭 Session 之外的写入方遇到 `Locked` 或 `DirectoryNotEmpty`，这时删除失败并重试（§3.5）。"需要把 put 和 delete 串行化"：存储的写事务给它们排序（S5a §6），不另加内存锁。

## 8. 存储格式

**决定：不单独改格式**（S5c 本身不改格式；#222 在 v16 下新增一种事件行，没有升版本号，格式 v17 原样保留它）。记录在
[native-storage-format.md](../../../design/appserver/native-storage-format.md#s5c-格式决定)。

- 删除只从现有表中删行，以及 [#556 的目录投影](../../../design/appserver/native-storage-format.md#556-v23-格式决定)的派生行 `session_catalog`。
- 分叉向现有表插入行，按字节复制存储的文本，然后在 `at + 1` 插入一行 `session/forked`（数据为空），并在同一事务内按它写入的行写下 F 的 `session_catalog` 行。头部为 0、没有事件的子分支目录是存储已经接受的状态：子分支尚未开始的创建。
- 事件 id 在所属 Session 内唯一，分叉在另一个 Session 中重复源的 id。存储本就按 Session 检查和索引 id；S5c 把这一点写明为不变量，不是格式变化。
- 不加来源列、不写墓碑，原因见 §4 和 §6.6。
- **兼容性（开发阶段，随 #222 决定）。** `EventKind` 是封闭列表，所以在 `session/forked` 出现之前构建的守护进程无法解码这一行，也就无法恢复由新版守护进程分叉出的 Session。这一点被接受：原生格式在首次发布前不承诺兼容，因此不升版本号，也不加回退解码或迁移。在 `0` 处分叉的 Session，以及从未被分叉的 Session，都没有这一行。已删除的 Session 读起来如同从未存在。
- **格式 v17。** [请求列表](request-delta-storage.md#11-格式-v17)把格式升到 v17，原样保留这一行；它不引用内容。在 `session/forked` 出现之前构建的守护进程都以现有的"unsupported SQLite schema … preserved"错误拒绝 v17 库，所以能打开 v17 库的守护进程都能解码这一行。上述差异只留在 v16 库中。

## 9. 归属与 S2

| 操作 | 所有者 | 调用方 |
| --- | --- | --- |
| `session.delete`、`session.fork`（参数格式、协议版本） | Rpc 内建方法 | 客户端 |
| 删除的编排：准入门、关闭、忘记 | Agents（`AgentIndex::delete`） | Rpc |
| 从另一个 Session 的切点创建 | Agents（`AgentIndex::fork`），即总体设计 B.2 中 Agents 的补充 | Rpc |
| 命令输出移除、行删除、分叉事务 | SessionStore 的 Rust 方法 | Agents |
| Session 文件的行与清扫 | SessionStore（S5a） | `sessionFiles.put`、`delete_session` |

**S2（[#62](https://github.com/arcships/dim/issues/62)）。** S2 的 `sessions.launch` 由 profile 和以启动方为上限的策略创建 Session，不从切点创建。Agents 为两者保留同一条创建路径：预备创建，`session.create` 和 `launch` 的预备步骤是 `create_owned_session`，分叉的预备步骤是分叉事务。S2 不为 `sessions` 加分叉方法。以后若加 `sessions.fork`，它调用 `AgentIndex::fork`，并像 `launch` 那样以启动方为上限限制复制来的策略，因为进程调用方不能得到比启动方更宽的预设。

## 10. 权限（ADR 003）

- 分叉复制权限预设，从不放宽。切点处的预设是源曾持有的，调用方本来就能用 `session.permission.update` 修改源的预设。
- 复制来的审批事实是对已结算调用的决定。Approval 每次回答都复查当前策略，不记录长期授权，因此它们不带来任何权限。
- 宿主安全设置从不进入 Timeline，分叉既不恢复也不放宽它们（ADR 003 D5）。
- 拥有源的 Session 文件不增加任何权限（[Session 文件 §11](session-files.md#11-安全)）。
- 复制来的对其他 Session 资源的引用（成员 Session id、任务 id、路径）不赋予任何权限：结算分叉的所有者只处理自己 Session 持有的资源（§14，Collaboration）。
- 删除不改变宿主状态；诊断只带 id 和错误类别，从不带事件内容。

## 11. 资源节点（P3）

S5c 与 S5a 一样只实现控制节点。在控制节点上，FileStorage 正在重载或已关闭时第 3 步失败，删除随后重试。在 P3 的资源节点上，第 3 步在该节点的 FileStorage 上执行，节点不可用时删除失败于节点不可用。删除能否照常提交、把目录留给该节点自行对账，在 P3 中随按资源节点查找（[#38](https://github.com/arcships/dim/issues/38)）一起决定：一个资源节点只服务一个控制节点，所以在那里列目录能确定条目的归属，这与共用的数据库目录不同。

## 12. 对其他文档的修改

| 文档 | 本 PR 中的修改 |
| --- | --- |
| [总体设计 B.2](../overview.md#b2-app-server-进程插件) | Rpc 行：内建方法 `session.delete` 与 `session.fork`。Agents 与 SessionStore 行链接到本页 |
| [原生存储格式](../../../design/appserver/native-storage-format.md#s5c-格式决定) | S5c 格式决定：不改格式 |
| [AcpGateway](../plugins/AcpGateway.md)、[RemoteGateway](../plugins/RemoteGateway.md) | 原文说分叉时重置预设，#26 已改为：预设随事实一起复制 |
| [文档索引](../../../INDEX.md) | 链接到本页 |
| [Session 文件](session-files.md) | §5.3 和 §12 原说节点离线时删除照样提交。这对其中的 Session 文件步骤成立；整个删除还会先在节点上移除命令输出（§11）。§5.2、§5.4 和 §6 不变，就是本页执行的步骤；§5.4 注明移除现在会同步 |
| [FileStorage](../../../architecture/rust-agent/plugins/FileStorage.md) | 移除会同步被移除条目的父目录（§14） |
| [单树装配](../../../architecture/rust-agent/single-tree.md) | `Agents` 重载一行：重载结算只停止重载中的 Session 所领导的成员（#127） |
| [Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-字段) | 状态说明：分叉复制存储的 `SessionConfig` 文本，`interactive` 的省略规则随之保留 |

## 13. 规则检查

| 规则 | 回答 |
| --- | --- |
| 原则 §4 第 1 条（改别人的状态写指令） | 删除和分叉只改 Agents 与 SessionStore 自己的状态：Session 记录和行。分叉复制存储的字节，并写一条自己的事实 `session/forked`，不替其他所有者写事实；F 打开时写出的事实来自各自的所有者（TimelineTree 的 `branch/resume` 与 `branch/create-failed`、Collaboration 的失败结果）。Goal 自己依据 `session/forked` 做决定（A11）。预设是复制，不是重置（#26） |
| 原则 §4 第 2 条（交接） | 不适用：没有交接任何东西 |
| 原则 §4 第 3 条（before 只做变换） | 没有 before 回调。Rpc 检查参数格式和 Session 的版本；切点检查是 `session.branch_create` 现有的检查加上静止点，两者都只读固定前缀 |
| 原则 §4 第 4 条（核心不带业务含义） | Timeline、Dispatcher、before 和批次不变。TimelineTree 现有的规则（有事件的根分支以 resume 打开，缺少 start 的子分支是失败的创建）原样适用 |
| 原则 §4 第 5 条（批次） | 切点位于原子批次末尾，复制的都是完整批次；`session/forked` 在其后自成一个批次 |
| 原则 §4 第 6 条（上下文只来自交接后的事件） | F 的上下文来自复制来的已采纳输入和工具结果，没有其他东西进入 |
| 原则 §4 第 7 条（只通过公开事件和服务） | Agents 使用 Session 门面（`close`、`context`）和 SessionStore 的方法；SessionStore 使用 FileStorage 公开的 `remove`。对 Collaboration 的需求让分叉不会作用于源的成员 |
| 原则 §4 第 8 条（向机制所有者提需求） | 不改 Timeline、Dispatcher、事件信封、装配、rutis 或 Session 门面，也不向 rutis 提需求。新增一种事件 `session/forked`，归分叉所有；因为在动态事件（#2）之前 `EventKind` 是封闭列表，这是随 #209 做的格式决定。分叉事务在 F 的存储中给它序号 `at + 1`，与复制来的事实取得序号的方式相同：此时 F 的 Timeline 还不存在，F 加载时发布存储的历史。同步移除是 FileStorage 关于自身操作持久性的补充；静止点检查只读取那些会在重载时开始工作的能力的事实；Collaboration 的检查是它自己的规则（§14） |
| 原则 §4 第 9 条（只有后续决定要用的事实进 Timeline） | 一种 Timeline 事件 `session/forked`，因为 Goal 依据它做决定：按分支自身历史折叠的所有者从它之后开始（时间总线 §2：这就是它的消费规则；不要求其他所有者消费）。它的数据为空。它是内部事件：客户端从回复得知分叉，v1 排除它。删除是对记录的进程级操作，不写事件 |
| 原则 §1（Capability 与 App Server 的边界） | 不新增插件；Agents 和 SessionStore 的补充已列在总体设计 B.2 |
| ADR 002 D1（作用域随状态） | 进程作用域：记录和数据库目录比任何 Session 都长寿 |
| ADR 002 D2（不设接线模块） | Rpc 内建方法调用 Agents；SessionStore 的步骤是内部方法，没有适配器或注册表 |
| ADR 002 D3（依赖随作用域） | Rpc、Agents 和 SessionStore 不依赖任何可选插件；FileStorage 每次调用时查找 |
| ADR 002 D4（结构配置与业务配置分离） | 没有配置 |
| ADR 002 D5（语言中立合同） | 两个线上方法只收发数据。SessionStore 不新增服务 |
| ADR 003（审批范围） | §10：分叉复制预设，从不放宽；复制来的事实和共享的文件不赋予权限 |

## 14. 向机制所有者提出的需求

不向 rutis 提需求。对事件格式：新增事件 `session/forked`（§13 第 8 条）。对其他所有者的补充与需求：

| 对象 | 需求 | 状态 |
| --- | --- | --- |
| FileStorage | `remove` 在报告成功之前同步被移除条目的父目录（条目原本就不存在时也同步），同步失败时返回 `Unsynced`。Windows 没有目录同步，与 `replace` 一样依赖 NTFS 元数据日志。用于让删除的移除在提交之前落盘（§4） | 已在 S5c 中实现（[FileStorage](../../../architecture/rust-agent/plugins/FileStorage.md#listing-and-removal)） |
| Loop、InputQueue、BackgroundExec | 作用于固定前缀的只读静止点检查：没有未结束的轮次，没有等着驱动轮次的 Queue 或 Steer 输入，每个后台任务都已结束且其唤醒已发出（§6.2）。某个能力的重载规则以后要从其他事实开始工作时，就把这些事实加入检查，分叉因此始终不会开始任何工作 | 已在 Agents 中实现，读取这些能力的公开事实，不改动它们（§16 D1）。它们的所有者保留这些事实；某条重载规则以后要从其他事实开始工作时，扩展 [agents.rs](../../../../crates/dim-app-server/src/agents.rs) 中的 `rest_point`。实现中发现、留给 BackgroundExec 所有者的问题：它的 `rebuild` 沿用了更早一代的分支关闭（`session.close` 或正常关机）留下的 `closing`，因此恢复之后不会重新发出崩溃中丢失的唤醒。`rest_point` 像实时唤醒那样把这个标志限定在本代之内，所以落在这样的 `exec/ended` 与其唤醒之间的切点会被拒绝 |
| Collaboration | 分支重建时，已送达但没有结果的请求以失败结算；只有成员关系指明本 Session 是 leader 时才停止该成员。一般地，重载结算只处理本 Session 持有的资源。原先重建会停止事实中指名的任何已加载 Session（[collaboration.rs](../../../../crates/dim-app-server/src/collaboration.rs) 的 `rebuild`），因此分叉一个 leader 会停止源的成员 | 已实现（[#127](https://github.com/arcships/dim/issues/127)）。成员关系是进程内存，守护进程重启后同样不停止任何成员。#127 之前，这种重载会停止已加载的该成员；现在成员自身重载规则启动的工作会继续运行，leader 已把请求结算为失败，成员发给 leader 的 `agent` 消息因没有成员关系而被拒绝（`TeamMemberNotFound`）（[#97 评审](https://github.com/arcships/dim/pull/97)）。验收第 7 条 |
| Goal（P5，[#69](https://github.com/arcships/dim/issues/69)） | 复制来的目标不得在 F 中驱动轮次，也不是 F 的目标。分叉在其事务中、复制来的事实之后写 `session/forked`（§6.5）；Goal 从该事实之后折叠 `main`，并对复制来的生效目标清除一次（[Goal](../plugins/Goal.md) A11） | 随 Goal 验收实现（[#209](https://github.com/arcships/dim/pull/209)） |
| S5b（[#60](https://github.com/arcships/dim/issues/60)） | 标题元数据行在删除事务中删除，分叉不复制 | 已在 S5b 实现（[#96](https://github.com/arcships/dim/pull/96)，[标题元数据 §9](session-title-metadata.md#9-生命周期)） |
| [#32](https://github.com/arcships/dim/issues/32) | `submission_id` 移出 `input/message` 后，分叉在其事务中复制切点及之前输入的幂等记录，让 `session.input_lookup` 在分叉上继续能回答复制来的输入 | 不需要：#32 把该字段留在 `input/message` 中（[sessions 服务 §6.3](sessions-service.md#提交标识存放在哪里)），分叉随事件复制它 |
| F6（[#77](https://github.com/arcships/dim/issues/77)） | `dimcode session delete`，以及按时间或数量清理并预览，由 `session.summaries` 和 `session.delete` 组合（§5.1） | F6 的设计 |

## 15. 验收（流程测试）

1. 删除一个正在运行轮次、并有客户端附着的已加载 Session：客户端收到 `session.closed`，回复为 `{}`，`session.list` 不再列出它，`session.attach` 失败于 `session_closed`。任何表中都不再有它 id 的行，`command-output/<id>/` 已移除，它的 Session 文件由清扫移除。第二次删除回复 `{}`。
2. 删除未加载的 Session，以及行校验失败的 Session：都回复 `{}`，不留任何行。
3. 在 `main` 的静止点处分叉，一个子分支在切点之前创建、一个在之后：F 的 `main` 含有源截至 `at` 的事件且 id 相同，之后是 `session/forked`、`branch/resume` 和为第一个子分支写的一条 `branch/create-failed`；F 有第一个子分支的目录、没有其事件，不含第二个子分支的任何东西；F 只有其 `main` 事件引用的内容行和快照；重启后重新加载，不再写第二条 `session/forked` 或 `branch/create-failed`。在 `0` 处分叉以 `branch/start` 打开，没有 `session/forked`。
4. §6.5 的几种情况：子分支在切点之后完成时分叉，子分支有未结束的轮次、工具批次和后台任务时分叉，客户端在切点之后给子分支发 steer 时分叉。每种情况下 F 的子分支都没有事件；客户端给 F 提交输入之前，F 不记录任何 `turn/start`、模型调用、工具执行或 `exec/started`；源的子分支不受影响地继续。
5. 分叉的 `SessionConfig` 文本与源相同，包括 `"interactive": false`。切点之前在源上设置的权限预设和模型就是 F 的预设和模型。
6. `at` 超过头部、位于批次内部、不在上下文边界或不是静止点（在轮次之内、有未消费的 Queue 或 Steer 输入、有运行中的后台任务或其唤醒未消费），源未知，以及 v2 连接：分叉都被拒绝，什么都没创建。
7. 在 leader 对成员的请求未结束时分叉：F 记录失败结果，不投递报告输入，不启动轮次，源的成员继续运行（[#127](https://github.com/arcships/dim/issues/127)）。
8. 分叉后删除源：F 能加载并读取它的 Session 文件；删除 F 后文件被移除。对同一源的删除与分叉竞争：要么 F 带着文件存在，要么分叉失败于 `session_closed`；绝不会出现没有文件的分叉。
9. 在一次性存储上：命令输出目录已不在但行还在的 Session 能加载，随后删除能完成；删除提交后留下的无所有者文件行在下一代的第一次写入时被清扫。
10. 关闭了一个已加载 leader 之后、因其命令输出无法移除而失败的删除，回复 -32603，leader 的行都在；本进程内不再列出、不再加载 leader，它的成员仍是一个 Session。重启后 leader 照常加载，重试即可删除（D5）。
11. 删除仍在构建的 Session（一次 `session.create` 和一次 `session.fork`，在行提交并被列出之后停在某个能力的 apply 中）时等构建结束，然后回复 `{}`；该 id 既没有行，也没有已加载的 Session（D6）。不是单一路径分量的 id 被拒绝，什么都不移除（D3）。

测试：第 1、2 条是 [deletion.rs](../../../../crates/dim-app-server/tests/transport_flow/deletion.rs) 中的 `delete_closes_a_running_session_and_removes_what_it_stored` 和 `unloaded_and_damaged_sessions_are_deleted_by_id`。第 3 到 8 条在 [fork.rs](../../../../crates/dim-app-server/tests/transport_flow/fork.rs) 中：`a_fork_copies_main_through_a_rest_point_and_starts_nothing`（第 3、4 条和第 5 条的 `interactive`），`a_fork_waits_for_background_work_and_keeps_preset_and_model`（第 4 条中带后台任务的子分支、第 5 条的预设和模型、第 6 条的后台任务与批次切点，使用真实的 BackgroundExec），`a_fork_is_refused_unless_the_cut_is_a_rest_point`（第 6 条，以及 D1 允许留在切点的输入：被撤回的 Queue 输入和停止前未消费的 Steer 输入），`a_fork_of_a_leader_does_not_stop_the_source_member`（第 7 条），`a_fork_shares_session_files_and_outlives_its_source`（第 8 条）。[storage_flow.rs](../../../../crates/dim-app-server/tests/storage_flow.rs) 中的 `a_fork_copies_main_through_the_cut_with_its_content_and_snapshots` 在存储层检查第 3 条的内容行和快照，包括只被 before 记录引用的内容。第 9 条是 [files/tests.rs](../../../../crates/dim-app-server/src/storage/files/tests.rs) 中的 `an_interrupted_delete_converges_on_retry_and_the_next_sweep`，它让删除停在移除输出与提交之间，另加 `unloaded_and_damaged_sessions_are_deleted_by_id` 中没有输出目录的 Session。deletion.rs 中的 `a_delete_leaves_another_databases_command_output` 用两个守护进程检查 §4 的共用目录，其中的 `a_failed_delete_leaves_the_session_closed_until_a_retry` 是第 10 条，`a_delete_waits_for_a_building_session` 是第 11 条，被拒绝的 id 在 `delete_closes_a_running_session_and_removes_what_it_stored` 中。

验证：`./dev check`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `cargo test --workspace --locked`。不改 Capability、装配或 crate 依赖，`xtask` 只需按常规在 CI 中运行。

## 16. 实现中的设计变更

实现对本设计所做的改动（[路线图 §5.3](../../../design/roadmap.md#53-实现中发现的变更)）。每项都只影响 S5c 自己的所有者。

| # | 变更 | 理由 |
| --- | --- | --- |
| D1 | 静止点检查是 Agents 中的 `rest_point`：在 Agents 为 `validate_context_boundary` 已经读取的前缀上，只读地折叠 Loop、InputQueue 和 BackgroundExec 的公开事实，而不是放在能力 crate 中 `validate_context_boundary` 旁边的检查。其中的输入规则与 Loop 一致：在最后一次 `stop/interrupt` 之前已采纳、尚未消费的 Queue 或 Steer 输入允许留下，因为重载不会执行它（它会像 Append 输入一样并入下一次请求）；InputQueue 因撤回而删除的输入（`input/deleted`，S1）永远不会被采纳，不计入；其他尚未采纳的输入则被拒绝。任务若不是在本代的分支关闭之后结束，就必须有唤醒，与实时唤醒的发出条件一致 | 它需要的事实都是公开事件，因此不需要这些能力提供 API，也不改动它们。要求所有这类输入都已消费，会拒绝刚停止一轮之后的切点，而那里并不会执行任何东西 |
| D2 | `session.fork` 需要原生存储。没有原生存储的守护进程（不带 `--database` 的 mock 配置）不在 `hello.features` 中列出它，并回复 `unknown_method`（"Method requires native storage"）。`session.delete` 在那里同样与 `session.create` 一起列出；没有存储时它关闭 Session 并将其遗忘 | 分叉复制的是存储的行；内存中的 Timeline 没有可供复制的 SessionStore |
| D3 | `session.delete` 对不是单一路径分量的 `sessionId` 回复 `session_invalid_input`。空 `sessionId` 会更早被拒绝：在每个 Session 方法共享的边界上回复 `invalid_params`（"sessionId must not be empty"）；路径分量规则覆盖其余 id。Agents 在取得任何准入状态之前检查它；删除若在该 id 下既没有找到已加载的 Session 也没有找到存储的行，就不为它保留槽位；对 Agents 不认识的 id 执行 `session.close` 也不保留 | 这个 id 构成 `command-output/<id>`。Session id 是 UUID；否则像 `<S>/.` 这样的 id 会移除 S 的命令输出，却不删除任何行。没有后两条规则，删除或关闭任意不存在的 id 都会各留下一个槽位，没有上限 |
| D4 | 分叉事务不再检查切点：它复制 `main` 截至 `at` 的行，运行时在构建分叉时校验这些行。校验失败的副本和任何失败的创建一样被回滚 | Agents 已在源上检查过切点（已发布的头部、批次末尾、上下文边界、静止点），而切点及之前的行不会改变；在事务中再检查一次，只会重复每次构建都会执行的校验 |
| D5 | 关闭之后失败的删除让 S 在本进程内保持关闭，与 `session.close` 相同；而不是释放 S 的槽位、让 S 在下次使用时加载（第 6 步、§3.5） | 这次关闭是业务关闭：S 的各能力已结算其工作，Collaboration 也已结束指向 S 的关系，它对每个关闭的 Session 都这样做。再加载 S，会运行一个联系不到成员的 leader，而按 #127 它重载时也不停止任何成员（[#97 评审](https://github.com/arcships/dim/pull/97)）。槽位保持关闭时，失败留下的就是 `session.close` 本来就会产生、各所有者都已处理的状态；要让 Collaboration 区分删除的关闭与其他关闭，就得在 Agents 中加删除状态，并为一条失败路径加一个通知。重试不需要加载，重启后 S 照常加载，与任何已关闭的 Session 一样 |
| D6 | 预备创建（`session.create`、成员创建、`session.fork`）从其准备步骤提交行之前起持有新 id 的准入门，直到 Session 发布、协议版本记下为止（`AgentIndex::admit`）；失败的创建不保留槽位 | Session 的行从提交起就会被列出，而运行时仍在构建它。跳过构建中 Session 的删除可能在构建读取分支之后提交，构建随后会发布一个没有行的已加载 Session，其追加违反外键；分叉还会把已删除 id 的协议版本重新记下。创建就是一次加载，因此取得恢复所取得的同一个门，删除排在它之后（[#97 评审](https://github.com/arcships/dim/pull/97)） |
| D7 | 分叉事务在复制来的事实之后、`at + 1` 处写 `session/forked`（数据为空）（在 `0` 处分叉不写），按分支自身历史折叠的所有者从它之后开始。此前本设计不写 Timeline 事件，留给 Goal 在首次加载时的回退做法 | 随 Goal A11 决定（#209）：分叉的 Session 从没有目标开始，而边界必须是所有者在分叉发生时写下的事实，这样提交与 F 首次打开之间的崩溃不会丢失它，消费方也像对待任何事件一样据它做决定 |

## 附录 A. Rust 绑定

在 dim-app-server 中：

```rust
impl AgentIndex {
    /// `session.delete`: id check, gate, close if loaded, `SessionStore::delete_session`, forget (§3.2).
    pub(crate) async fn delete(&self, id: &str) -> dim_agent_runtime::Result<()>;
    /// A prepared creation under the new id's admission gate until it is published (§7, D6).
    pub(crate) async fn admit(
        &self,
        id: &str,
        agent_id: &str,
        version: u32,
        creation: impl Future<Output = dim_agent_runtime::Result<Session>>,
    ) -> dim_agent_runtime::Result<Session>;
    /// `session.fork`: acquire the source, check the cut, prepared creation (§6.5).
    pub(crate) async fn fork(&self, source: &str, at: u64) -> dim_agent_runtime::Result<Session>;
}
/// The rest point of §6.2 over `main`'s prefix (D1).
fn rest_point(events: &[Arc<AgentEvent>]) -> Result<(), String>;
impl SessionStore {
    /// `ctx` gives each file step the node's FileStorage; `files_root` is the
    /// database directory's root.
    pub fn new(db: Arc<SqliteStorage>, ctx: Ctx, files_root: usize) -> Self;
    /// Removes `command-output/<id>` (synced) while this database has rows of
    /// the id, then every row in one transaction; schedules the Session-file
    /// sweep. Returns whether this database had rows of the id.
    pub async fn delete_session(&self, id: &str) -> Result<bool>;
    /// Prepare step of a fork: copies the rows of §6.3 in one transaction.
    pub async fn fork_session(&self, source: &str, fork: &str, at: u64) -> Result<()>;
}
/// In `storage`: the output root's name below the database directory.
pub const COMMAND_OUTPUT_DIRECTORY: &str = "command-output";
```

`SessionStorePlugin` 已经接收数据库目录的根索引（S5a）。SessionStore 在该根下移除 `command-output/<id>`，这就是守护进程交给 WorkspaceTools 作为输出根的目录（[main.rs](../../../../crates/dim-app-server/src/main.rs)）；`COMMAND_OUTPUT_DIRECTORY` 是这个名字唯一的定义。创建回滚路径（`TimelineStore::rollback_session`）调用与 `delete_session` 相同的行删除。
