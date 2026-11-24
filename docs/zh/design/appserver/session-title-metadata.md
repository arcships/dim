# SessionStore 标题元数据

目标绑定为 `store.readMetadata` 和 `store.setTitle`。SessionTitle 与重命名调用方一起切换到 `store`，随后移除 `sessionMetadata`。下文的旧绑定名描述迁移前的实现。

状态：已在路线图 S5b 实现（[#60](https://github.com/arcships/dim/issues/60)，[#96](https://github.com/arcships/dim/pull/96)），在 C6（[#49](https://github.com/arcships/dim/issues/49)，[#85](https://github.com/arcships/dim/pull/85)）之后；实现与本设计一致。评审细化了写入何时被接纳、卸载如何了结它（§6、D14）。需求：[SessionTitle](../plugins/SessionTitle.md) §2、§4 与 [A.2](../plugins/SessionTitle.md#a2-依赖的形状由所有者定义)。总览：[附录 B.2](../overview.md#b2-app-server-进程插件) 的 SessionStore 与 Rpc 行。范围：[ADR 002](../../adr/002-plugin-scope.md) 进程作用域，在既有 SessionStore 插件内，外加三个 Rpc 列表字段。使用方：SessionTitle。

## 1. 概要

SessionStore 为每个 Session 保存一个标题和一个按 Session 计数的修订号。标题只能通过 `setTitle` 修改，这是一次带修订号检查的写入。每次变更提交后，SessionStore 在该 Session 的广播频道上宣布新标题（[C6](session-notices.md)），Rpc 在协议 v3 的 Session 列表里显示标题。

- **数据。** 一个修订号计数（0 表示从未写过），以及无标题或标题 `{text, source}`，`source` 为 `auto` 或 `manual`。清除时保留计数，所以修订号不会重复。
- **写入。** `setTitle {sessionId, expectedRevision, title | null}` 提交修订号 `expectedRevision + 1`，或者失败为 `conflict`、`not_found`、`storage`。调用即交接：除非 SessionStore 正在关闭，它在收到调用时接纳写入，并由自己了结；放弃调用只会丢掉回复。
- **通知。** 每次提交后发出 `{source: "SessionStore", kind: "title", data: {title, titleSource, titleRevision}}`，按提交顺序。
- **列表。** 协议 v3 的 `session.summaries` 条目带 `title`、`titleSource`、`titleRevision`。
- **绑定。** 进程服务 `sessionMetadata`（`read`、`setTitle`）由 SessionStore 提供，经既有服务目录到达 TypeScript，与 `sessions`、`settings` 相同。
- **生命周期。** 删除在 S5c 的删除事务里移除标题；分叉不复制标题（SessionTitle A7）；重启后标题保留。读写标题从不装载 Session。
- **格式。** 原生存储 v16：新增一张表，从 v15 原地升级；v13 经由 v15 的那一步升级。

不新增 Timeline 事件，不改 Dispatcher、InputQueue、运行时或 rutis，不新增插件、客户端方法或协议版本。

## 2. 结构

```text
 Session 作用域                          进程作用域（控制节点）
 ----------------------------           --------------------------------------------------------------
 SessionTitle（Session 部分）           SessionStore（既有插件）
   inject 'sessionMetadata'               sessionMetadata（HostDispatch）
     read -------------------------->       read     -> metadata()   ------------------+
     setTitle ---------------------->       setTitle -> set_title()                    |
                                                          |                            v
                                          标题写入者：一次一个写入 -----------> SqliteStorage
                                            先提交，再 notify                     表 session_metadata
                                              |                                        ^
 Session 频道 session/<id>  <-----------------+  Notice {SessionStore, title, data}     |
   |                                                                                   |
   v                                    Rpc：session.summaries ------------------------+
 Rpc 订阅（v3）                           SessionStore 的目录查询联结标题行
   -> session.notice {sessionId, source, kind, data}
```

| 部件 | 作用域 | 修改 |
| --- | --- | --- |
| SessionStore（[timeline.rs](../../../../crates/dim-app-server/src/storage/timeline.rs)、[metadata.rs](../../../../crates/dim-app-server/src/storage/metadata.rs)） | 进程 | `metadata` 与 `set_title`、标题写入者、`sessionMetadata` 服务；删除和创建回滚移除标题行；目录查询读取它 |
| SqliteStorage（[sqlite.rs](../../../../crates/dim-app-server/src/storage/sqlite.rs)） | 进程 | v16 结构及其升级步骤 |
| Rpc（[protocol.rs](../../../../crates/dim-app-server/src/protocol.rs)、[connection.rs](../../../../crates/dim-app-server/src/connection.rs)） | 进程 | v3 列表字段 |
| 服务目录（[lib.rs](../../../../crates/dim-app-server/src/lib.rs)） | 进程 | `register_shared("sessionMetadata")`，一行，与 S2 为 `sessions` 所做的相同 |
| Session 广播、Rpc 通知转发 | Session、进程 | 不变（C6） |

## 3. 数据与修订号

| 项 | 含义 |
| --- | --- |
| `revision` | 按 Session 计数的已提交 `setTitle` 次数。0 表示从未写过。每次提交都把它设为 `expectedRevision + 1`：新标题、再次写入相同文本、改变来源或清除都算 |
| `title` | `{text, source}` 或无 |
| `source` | `auto`（SessionTitle 生成）或 `manual`（`session.rename`）。SessionStore 保存给它的来源，不附加任何规则。"手动标题总是胜出"是 SessionTitle 的规则，通过修订号检查实现（[SessionTitle §4](../plugins/SessionTitle.md#4-时序与冲突)） |

- **清除保留计数。** `title: null` 让该 Session 的行保留新修订号而没有标题。如果删除这一行，计数会从 0 重新开始：一次在重命名和清除之前读到修订号 0 的生成，就会在用户清除之后提交自动标题。
- **文本原样保存。** SessionStore 保存收到的文本。标题规则（去空白、长度、单行）属于 SessionTitle（[A.3](../plugins/SessionTitle.md#a3-配置)）：它的方法校验重命名输入，并规范化生成的文本。生产者保证合同，所以 SessionStore 不再重复检查（AGENTS.md）。
- **按 Session。** 不同 Session 的修订号互不相关。分叉从 0 开始。

## 4. 操作

### 4.1 语言中立形式（ADR 002 D5）

`sessionMetadata`，进程作用域，只在控制节点，没有实例键，由 SessionStore 提供。

| 方法 | 模式 | 请求 | 回复值 | 错误 |
| --- | --- | --- | --- | --- |
| `read` | async | `{sessionId}` | `{revision, title: {text, source} \| null}` | `not_found`、`storage` |
| `setTitle` | async | `{sessionId, expectedRevision, title: {text, source: "auto" \| "manual"} \| null}` | `{revision}`：提交后的修订号 | `conflict`、`not_found`、`storage` |

- 回复使用同一个信封：`{ok: true, value}` 或 `{ok: false, error: {code, message}}`，与 `settings` 相同。
- 两个方法都不装载 Session。`read` 读取已提交的状态，不等待标题写入者（§5）。
- `setTitle` 必须带 `title`；`null` 表示清除。缺少 `title` 的请求被拒绝，而不是当作清除，这样调用方的遗漏不会抹掉用户的标题。
- 格式错误的调用（未知方法、不是恰好一个请求对象、未知字段、类型错误或缺字段）是桥接错误，与 `settings` 相同：这是调用方的编程错误，不是写入的结果。

| 代码 | 何时 | 之后的状态 |
| --- | --- | --- |
| `not_found` | 存储里没有该 Session 的记录：从未创建、已删除或已回滚。已关闭或未装载但已存储的 Session 能找到 | 不变 |
| `conflict` | `expectedRevision` 不是当前修订号 | 不变，没有通知。重新读取 |
| `storage` | 事务失败，或调用在 SessionStore 开始关闭之后到达（§6） | 不变，没有通知 |

### 4.2 Rust

Rust 使用方注入 `sessionMetadata` 的原生绑定 `SessionMetadataService`，调用 `read` 和 `set_title`。
此绑定就是原 dispatcher，同一实例提供在类型键和 `host_key("sessionMetadata")` 上，两条路径直接调用
同一个 SessionStore owner；既有回复类型 `SessionMetadata` 不变。

`set_title` 是返回 owner 等待 future 的普通函数。调用时就接纳写入，无需先轮询 future；丢弃 future
只丢弃回复。绑定不在接纳前增加异步包装，也不增加取消点、读取或第二个标题写入者。

## 5. setTitle 时序

```text
 调用方                  SessionStore                                     SQLite             Session 频道
 ------                  -----------------------------------------------  -----------------  ---------------
 setTitle(S, r, T) ----> SessionStore 正在关闭？ -> 立即 storage，不写入
                         接纳：启动写入任务     <== 交接（调用本身）
                           |  （任务归 SessionStore 所有；从这里起
                           |   放弃调用只会丢掉回复）
                           |  等待轮到它使用标题写入者（一次一个写入）
                           +-- BEGIN IMMEDIATE ---------------------------> 读 S 的修订号
                           |                                                 没有 Session：not_found，回滚
                           |                                                 修订号 != r：conflict，回滚
                           |                                                 写入 r+1 与 T
                           +-- COMMIT ------------------------------------> 已提交，或 storage
                           |  仅在已提交时：
                           |    notify(S, {SessionStore, title,
                           |      {title, titleSource, titleRevision: r+1}}) -------------------> Notice
                           |  结束它的轮次
 <---- {revision: r+1} ----+
```

`notify` 在提交之后、下一个写入轮到之前运行，所以同一 Session 的通知按提交顺序到达它的频道。它从不在 SQLite 连接锁内运行：监听方代码不在数据库锁内运行（[timeline.rs](../../../../crates/dim-app-server/src/storage/timeline.rs) 的规则）。

## 6. 交接、取消与了结

本节确定 SessionTitle §0 留给 S5b 的边界。

| 时点 | 规则 |
| --- | --- |
| 调用之前 | 调用方自己的工作。在调用前取消，不写入任何东西 |
| 调用 | 交接点是 SessionStore 收到调用：`setTitle` 在返回之前接纳写入，并把它作为 SessionStore 自己的任务启动；SessionStore 一旦开始关闭，则失败为 `storage`，不写入。Rust 的 `set_title` 是普通函数，做完这些后返回回复的 future。这个 future 只等待该任务，所以放弃它只会断开回复，不会断开写入。命名服务在 `invoke` 里调用它，所以 JavaScript 的调用在宿主收到时即完成交接 |
| 调用之后 | 写入由 SessionStore 自己了结：提交并宣布，或者失败且不写入。放弃调用、卸载调用方的行或丢失回复，都不会取消或撤销它。`setTitle` 没有取消动作 |
| 回复丢失 | 调用方用 `read` 读取。不要盲目重复写入：按旧修订号重复会得到 `conflict`，按新修订号重复则是第二次变更（[SessionTitle 附录 C](../plugins/SessionTitle.md#附录-c-错误与恢复)） |
| SessionStore 卸载 | 它的清理先停止接纳：此后的调用立即失败为 `storage`，不写入。然后它等待每个已接纳的写入（正在执行的和仍在等待轮次的）提交并宣布或失败。之后它才关闭 Session 文件和 Timeline 写入线程；SqliteStorage 在 SessionStore 之后卸载，再关闭连接。这些通知按 C6 投递：Session 不再装载时无人监听，通知被丢弃。重载后不重试 |

不采用：在事务开始前都有效的取消动作。它只是转移了不确定性，放弃调用的一方仍然要读取才能知道结果；而且 JavaScript 调用方在 rutis 里无法取消宿主调用（[sessions §2](sessions-service.md#2-spike-结论) 第 5 条）。

## 7. 标题通知

| 字段 | 值 |
| --- | --- |
| `source` | `"SessionStore"` |
| `kind` | `"title"` |
| `data` | `{title: string \| null, titleSource: "auto" \| "manual" \| null, titleRevision}` |

- **每次提交一条。** 每次写入提交后，用 SessionStore 自己的上下文经 `session_broadcast::notify` 发出。冲突、找不到 Session 或写入失败都不发。
- **`data` 里没有 `sessionId`。** 频道代表 Session，Rpc 在 `session.notice` 里加上 `sessionId`（[C6 D4](session-notices.md#10-决定)）。
- **与列表相同的字段。** `data` 恰好是 v3 Session 列表条目的三个标题字段（§8）。客户端在 `titleRevision` 高于条目时把它们复制进条目即可。清除发送显式的 null，所以复制即清除。
- **顺序。** 同一 Session 的通知按提交顺序（§5）。`titleRevision` 也让客户端丢弃比手上更旧的内容。
- **投递**沿用 C6：没有监听方时丢弃（Session 未装载，或没有附加的 v3 客户端），从不重放，从不发给协议 v1 或 v2。

## 8. Session 列表

协议 v3 的 `session.summaries` 条目增加三个字段：

| 字段 | 值 |
| --- | --- |
| `title` | 标题文本，或 `null` |
| `titleSource` | `"auto"`、`"manual"`，没有标题时为 `null` |
| `titleRevision` | 读取条目时的修订号；从未写过时为 0 |

- SessionStore 的目录查询在同一次 SQL 读取里联结标题行，列表不会按 Session 逐个读取。没有原生存储的守护进程没有标题，它的 v3 条目显示 `null`、`null`、`0`。
- v1 与 v2 客户端没有 Session 列表，也从不收到标题通知（[C6 D5](session-notices.md#10-决定)）。
- **为什么列表带修订号。** 列表可能在一次提交之前读取，却在同一连接上晚于这次提交的通知到达客户端。客户端保留 `titleRevision` 较高的那个。C6 依赖这一点："由所有者的修订号去重"（[C6 §5](session-notices.md#5-rpc-转发)）。
- 客户端在附加之后读取列表；之后的每次变更都以通知到达（C6 §5）。

Rpc 从不调用 `setTitle`，也不新增方法。各错误到达客户端的方式：

| SessionStore | `session.summaries` | `session.rename`（[SessionTitle A.1](../plugins/SessionTitle.md#a1-sessionrename)） |
| --- | --- | --- |
| `storage` | 目录读取照旧失败 | `storage_failed` |
| `not_found` | 该 Session 不在列表中 | `session_not_found` |
| `conflict` | — | SessionTitle 重新读取后重试（[§3.2](../plugins/SessionTitle.md#32-手动重命名)） |

## 9. 生命周期

| 事件 | 标题 |
| --- | --- |
| 创建 Session | 没有标题，修订号 0；不写行 |
| 装载、卸载、关闭、重启 | 不变：标题与 Session 的行一起保存，不随实例。`read` 和 `setTitle` 对未装载的 Session 也可用；它们的通知因无人监听而被丢弃 |
| `session.delete`（S5c） | 在 S5c 唯一的删除事务里、在 Session 记录之前移除。之后 `read` 和 `setTitle` 失败为 `not_found` |
| 创建回滚 | 与删除是同一事务；它会移除并发写入提交的行 |
| `session.fork`（S5c） | 不复制（[SessionTitle A7](../plugins/SessionTitle.md#附录-e-假设)）：分叉从无标题、修订号 0 开始 |
| SessionStore 或 SqliteStorage 重载 | 按 §6 了结。`sessionMetadata` 的使用方随 SessionStore 重载（[ADR 002](../../adr/002-plugin-scope.md) D3），SessionTitle §1 已接受。SessionStore 只随没有依赖的 SqliteStorage 重载 |

## 10. 时序与冲突

| 场景 | 结果 |
| --- | --- |
| 两次写入带相同的预期修订号 | 先提交的胜出；另一次失败为 `conflict`，不宣布 |
| 写入不同 Session | 由唯一的标题写入者逐个执行；各自在自己的事务里提交 |
| `setTitle` 与 `session.delete` | 由存储的写事务排序：先提交的会被删除移除；先删除则写入得到 `not_found` |
| `setTitle` 与 `session.fork` | 互不影响：分叉不复制标题 |
| `setTitle` 与 Timeline 追加 | 不同的行；各自在同一存储连接上使用自己的事务 |
| 同一连接上的通知与列表读取 | §8：`titleRevision` 较高者胜出 |
| 没有附加客户端时 `setTitle` | 已提交；通知被丢弃；下一次列表显示标题 |
| 再次写入相同文本 | 提交新修订号并宣布（[SessionTitle 附录 B](../plugins/SessionTitle.md#附录-b-完整冲突表)：重复重命名会重写） |

## 11. 存储格式

**决定：版本 16，新增一张表，从恰为 v15 的库原地升级。v13 的库在同一事务里先走 v15 的那一步。** 记录在 [native-storage-format.md](../../../design/appserver/native-storage-format.md#s5b-v16-格式决定)。

```sql
CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 16))
CREATE TABLE session_metadata (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id),
  revision INTEGER NOT NULL CHECK (revision > 0), title TEXT,
  title_source TEXT CHECK (title_source IN ('auto', 'manual')),
  CHECK ((title IS NULL) = (title_source IS NULL)))
```

```text
 agent_sessions (session_id PK) <---- session_metadata (session_id PK, FK)
                                        revision      > 0，每次提交 setTitle +1
                                        title         清除后为 NULL
                                        title_source  'auto' | 'manual'；恰在 title 为 NULL 时为 NULL
 没有行：修订号 0，没有标题
```

- **为什么升版本。** 启动要求精确的 SQL 结构（[sqlite.rs](../../../../crates/dim-app-server/src/storage/sqlite.rs)），v15 守护进程会拒绝多出一张表的库。格式文档只在旧读取器能解释新数据时保持版本；打不开库的旧守护进程不满足这一条件。
- **为什么用新表而不是已有的行。** `agent_sessions.config` 是不可变的创建时 `SessionConfig`，分叉会整体复制：标题放在那里会违反 A7 被复制，还会把可变值变成配置。`settings_documents` 属于 Settings。给 `agent_sessions` 加列会改变它存储的 SQL，同样需要新版本，还会把可变值混进创建记录。
- **为什么每个 Session 一行。** 只读取当前标题和修订号。没有决定会用到以前的标题。
- **索引。** 主键服务 `read`、`setTitle`、列表联结和删除。不加其他索引。

| 发现的库 | 启动时在既有准入事务（`IMMEDIATE`，写入型 PRAGMA 之前）里的动作 |
| --- | --- |
| 空库 | 建 v16 结构，标记 16 |
| 结构恰为 v16，标记 16 | 打开 |
| 结构恰为 v15，标记 15 | 用 `CHECK (version = 16)` 重建 `agent_store_format`，写入 16，建 `session_metadata`。不读取、不改写任何业务行；失败则回滚到原样的 v15 库 |
| 结构恰为 v13，标记 13 | 同上，先走 S5a 的那一步：v16 标记、S5a 的两张表和索引，再建 `session_metadata` |
| 其他：标记不符的 v15 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构 | 照旧拒绝并保留 |

升级是单向的，不做备份，与 S5a 在开发阶段的决定相同。v15 守护进程以既有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。升级提交后，守护进程向 stderr 写一次 `Upgraded native store <path> from format v15 to v16; earlier dimcode versions cannot open it.`（S5a 的日志行，带上库原来的版本）。

## 12. TypeScript 绑定

| 项 | 规则 |
| --- | --- |
| 身份 | 由 SessionStore 提供、位于 `host_key("sessionMetadata")` 的 `dyn HostDispatch`；App Server 服务目录中的 `register_shared("sessionMetadata")`，与 `sessions`、`settings` 并列 |
| 使用 | C4 Session 部分声明 `inject: ['sessionMetadata']` 并调用 `ctx.use('sessionMetadata')`，与 Session 部分使用 `settings` 的路径相同（[settings-owner 夹具](../../../../runtime/fixtures/settings-owner.ts)）。类型在 [npm/plugin.d.ts](../../../../npm/plugin.d.ts) |
| 模式 | `read` 与 `setTitle` 都是 `async` |
| 写入者 | 服务不携带调用方身份，与 `settings` 相同（AGENTS.md 第 11 条）。按设计 SessionTitle 是标题唯一的写入者（[SessionTitle §2](../plugins/SessionTitle.md#2-数据与状态)）；其他包只读取 |
| 重载 | 使用它的行随 SessionStore 重载。SessionStore 只随 SqliteStorage 重载，所以不需要一个永不重载的目录部件；`sessions` 需要，是因为它的索引随 Settings 重载（[sessions D1](sessions-service.md#3-决定)） |
| 提供方缺失 | 没有原生存储的守护进程没有 SessionStore，注入 `sessionMetadata` 的行按既有规则保持 Pending；原生 Session 照常 |

这里只用了既有的服务目录和 `HostDispatch` 机制（[#4](https://github.com/arcships/dim/issues/4) 命名服务规则）。S5b 没有留给 C4 或 S4 的工作；S4 自己的辅助模型命名绑定是 SessionTitle 另一项前置。

## 13. 对其他文档的修改

| 文档 | 修改 |
| --- | --- |
| [SessionTitle](../plugins/SessionTitle.md) | §0 元数据前置与交接段落、§1 部件行、§6 接口行、§9 R1 通知数据、A.2、A.4 方法名、附录 F 与 G 指向本合同 |
| [Session 广播通知](session-notices.md) | 第一个发出方的状态、标题数据形状、D4 的结论 |
| [总览 B.2](../overview.md#b2-app-server-进程插件) | Rpc 与 SessionStore 行 |
| [Session 删除与分叉](session-deletion-and-fork.md) | §3.3 与 §14 指向本设计 |
| [原生存储格式](../../../design/appserver/native-storage-format.md) | v16 决定、表、完整记录示例 |
| [ADR 001](../../../adr/001-harness-foundation.md)、[Timeline 存储](../../../architecture/rust-agent/timeline-storage.md) | 当前格式 v16 |
| [TypeScript 插件编写](../../../typescript-plugin-authoring.md)、[npm/plugin.d.ts](../../../../npm/plugin.d.ts) | `sessionMetadata` 服务及其类型 |

## 14. 规则检查

| 规则 | 本设计如何满足 |
| --- | --- |
| [原则](../../../architecture/rust-agent/principles.md) §4 第 1 条（修改他人状态用指令） | 标题是 SessionStore 的进程状态，不在任何 Timeline 里。其他部分只能经 SessionStore 的类型化 `setTitle` 修改它，由 SessionStore 通过修订号检查决定 |
| §4 第 2 条（交接） | 交接是明确的：调用本身（§6）。之前调用方可以取消；之后只有 SessionStore 了结写入 |
| §4 第 3 条（before 回调） | 不适用：没有 before 回调 |
| §4 第 4 条（核心不带业务含义） | SessionStore 保存文本和来源而不解释它们；通知是 C6 的通用记录；Rpc 原样复制字段 |
| §4 第 5 条（批次） | 不适用：没有 Timeline 事件 |
| §4 第 6 条（模型上下文） | 对模型不可见 |
| §4 第 7 条（公开的事件与服务） | SessionTitle 使用公开的 `sessionMetadata` 服务；客户端使用公开的通知与列表字段 |
| §4 第 8 条（基础设施不变） | Timeline、Dispatcher、事件格式、装配、rutis 与 Session 门面都不变。SessionTitle 的需求在它的所有者 SessionStore 内闭环，见总览 B.2 的补充 |
| §4 第 9 条（只有决定用的事实进 Timeline） | 标题从不进入 Timeline；客户端经通知看到变更 |
| §1（Capability 与 App Server 的边界） | 在既有的 SessionStore 插件与 Rpc 内；不新增插件 |
| [ADR 002](../../adr/002-plugin-scope.md) D1（作用域随状态） | 进程作用域：标题与 Session 的行一起保存，比任何实例都长久 |
| ADR 002 D2（没有接线模块） | 命名服务只做解码和编码；服务目录加一行，与 `sessions` 相同 |
| ADR 002 D3（依赖随作用域） | SessionStore 不依赖任何可选插件；`notify` 不需要依赖；使用方依赖属于核心的 SessionStore |
| ADR 002 D4（配置） | 不适用：没有配置 |
| ADR 002 D5（语言中立的合同） | §4.1 以数据给出合同；附录 A 是 Rust 绑定 |

## 15. 向机制所有者提出的需求

无。S5b 使用 C6 的 `notify`、既有的服务目录与 `HostDispatch`，以及 S5c 的删除事务。

## 16. 验收（流程测试）

1. `setTitle` 提交并返回下一个修订号；`read` 返回它。经 IPC 和 WebSocket 附加的两个 v3 客户端，每次提交各收到恰好一条 `session.notice`，按提交顺序，`data` 等于列表的标题字段。冲突和未知 Session 不宣布任何东西。
2. 并发写入：每次提交都恰好宣布一次，`titleRevision` 递增。
3. 清除（`null`）提交一个没有标题的修订号，并宣布 null。
4. 读写从未创建或已删除的 Session：`not_found`。
5. 事务失败（一次性库上的失败触发器）：`storage`，没有通知，状态不变；之后按同一修订号写入能提交。
6. 调用方等待回复后又放弃它，而写入停在事务之前：该 `setTitle` 仍然提交，并且恰好宣布一次（交接，§6）。
7. v3 的 `session.summaries` 带 `title`、`titleSource`、`titleRevision`；v2 没有 Session 列表。
8. 标题在重启后保留，Session 未装载时也能读写；分叉从无标题、修订号 0 开始；删除移除该行。
9. 空库建为 v16。一次性的 v15 与 v13 库原地升级为精确的 v16 结构，所有业务行不变，并报告一次升级；标记不符的 v16 结构、升级到一半的库与更旧的结构被拒绝并保留。
10. 在 Node 中运行的 TypeScript Session 部分注入 `sessionMetadata`，读写标题，附加的客户端收到通知（`node_` 测试，由 CI 的 Node 步骤运行）。
11. 缺少 `title` 的 `setTitle` 请求被拒绝；插件 SDK 类型检查覆盖调用形状。
12. 卸载：SessionStore 开始关闭之前已接纳的写入（一个正在执行，一个在等待轮次）提交并回复各自的修订号，关闭过程等待它们；关闭开始之后的调用立即失败为 `storage`，重启后存储里没有它（§6）。

测试：第 1 至 8 项、第 10 项与第 12 项在 [titles.rs](../../../../crates/dim-app-server/tests/transport_flow/titles.rs)，经由 Session 部件注入的 `sessionMetadata` 服务和 [session-metadata 夹具](../../../../runtime/fixtures/session-metadata.ts)；第 6 项与第 12 项用 `storage::testing::hold_title_write`（`testing` 特性）让写入停住；第 9 项是 [storage_flow.rs](../../../../crates/dim-app-server/tests/storage_flow.rs) 中的 `a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once`、`the_daemon_reports_a_v13_upgrade_on_stderr_once` 与拒绝用例；第 11 项是 titles.rs 中格式错误的调用和 [plugin-sdk.test.mjs](../../../../runtime/plugin-sdk.test.mjs)。S5c 的删除测试也统计 `session_metadata` 的行。

## 17. 决定

| # | 决定 | 不采用 | 原因 |
| --- | --- | --- | --- |
| D1 | 表 `session_metadata`，原生格式 v16 | `agent_sessions.config` 里的字段；`settings_documents` 的行；`agent_sessions` 的新列 | §11 |
| D2 | 修订号 0 表示没有行；清除保留该行 | 清除时删除行 | 计数绝不能重复（§3） |
| D3 | 调用即交接；`set_title` 被调用时就接纳并启动写入 | 事务开始前有效的取消动作 | §6：它只转移不确定性，并不消除它；JavaScript 也无法取消宿主调用 |
| D4 | 一个进程级的标题写入者；`notify` 在它之内、提交之后运行 | 按 Session 加锁；在数据库锁内 `notify`；只依赖修订号 | 标题写得很少，一个写入者没有代价。监听方代码从不在数据库锁内运行。只依赖修订号会让客户端看到乱序的通知 |
| D5 | 通知 `data` 为 `{title, titleSource, titleRevision}` | A.2 的 `{sessionId, title, titleSource, revision}` | 频道已经代表 Session（C6 D4）；列表条目和通知用同一组字段，客户端合并时无需映射 |
| D6 | v3 列表条目带 `titleRevision` | 只带 `title` 与 `titleSource` | 没有它，客户端无法给列表回复和通知排序（§8） |
| D7 | 列表字段只用于协议 v3 | 所有协议版本 | v1 与 v2 没有 Session 列表，也收不到标题通知 |
| D8 | 服务 `sessionMetadata`，方法 `read` 与 `setTitle`；Rust binding 使用 `read` 与 `set_title` | A.2 的方法名 `metadata` | `sessionMetadata.metadata` 重复；`read` 与 `settings.read` 一致 |
| D9 | 错误 `conflict`、`not_found`、`storage` 作为数据；格式错误的调用是桥接错误 | 像 `sessions` 那样的 `rejected` 代码 | 三个代码是写入的结果；格式错误是编程错误，与 `settings` 相同 |
| D10 | 存储里没有的 Session 返回 `not_found` | `session_closed` | 维护者的决定；这是 SessionStore 的用语，SessionTitle 把它映射为 `session_not_found` |
| D11 | SessionStore 原样保存文本 | 校验长度和字符 | 标题规则属于 SessionTitle，由它校验；再查一次会重复合同 |
| D12 | 由 SessionStore 自己提供 `sessionMetadata` | 一个永不重载的部件 | SessionStore 只随 SqliteStorage 重载（§12） |
| D13 | 再次写入相同文本也提交新修订号 | 当作空操作 | SessionTitle 附录 B 期望重复重命名会重写；空操作需要自己的回复 |
| D14 | 在调用时接纳；卸载先停止接纳，再等待每个已接纳的写入 | 写入轮到时才接纳，卸载时让仍在等待的写入失败（最初的实现） | 那样已交接的写入没有确定的接纳点：它能否执行，取决于它的任务和清理谁先到达标题写入者；卸载期间到达的调用，在当前写入结束前既不被拒绝也不被接纳。调用时接纳让交接和拒绝都立即确定。标题写得很少，等待已接纳的写入很短。Session 文件的写入也这样接纳 |

## 附录 A. Rust 绑定

在 `dim_app_server::storage` 中：

```rust
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource { Auto, Manual }
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title { pub text: String, pub source: TitleSource }
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SessionMetadata { pub revision: u64, pub title: Option<Title> }
/// v3 Session 列表条目的标题字段，也是标题通知的 data。
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleFields { pub title: Option<String>, pub title_source: Option<TitleSource>, pub title_revision: u64 }
pub enum MetadataError { Conflict, NotFound, Storage(String) }

impl SessionMetadataService {
    pub async fn read(&self, session_id: &str) -> Result<SessionMetadata, MetadataError>;
    /// 被调用时就接纳并启动写入；SessionStore 正在关闭时失败为 `Storage`：
    /// 调用即交接。放弃返回的 future 只丢掉回复。
    pub fn set_title(&self, session_id: &str, expected_revision: u64, title: Option<Title>)
        -> impl Future<Output = Result<u64, MetadataError>> + Send + 'static;
}
```
