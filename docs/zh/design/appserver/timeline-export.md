# Timeline 导出

状态：已在 [#275](https://github.com/arcships/dim/pull/275) 实现，其第一个提交即本设计，先行评审。决定：[ADR 005](../../../adr/005-observability.md) D3，D6 第 3 步（[#274](https://github.com/arcships/dim/issues/274)）。范围：App Server 存储模块里的一个只读读取器，以 `dim-app-server` 二进制的一次性模式运行，加上 CLI 命令 `dimcode export`。不新增插件、RPC 方法或 Timeline 事实。

## 1. 概要

`dimcode export SESSION_ID` 把一个 Session 的完整 Timeline 写成带版本的 JSONL：先是头行和分支目录，然后按分支内序号写出每个分支的每个事件及其 before 轨迹，最后是结束行。它是完整运行的读取路径（ADR 005 D1）：评测与排查同时读取它和 `daemon.log`，二者都不另定格式。

- **入口。** CLI 以一次性导出模式运行同目录的 `dim-app-server` 二进制。读取器就是存储自己的 Timeline 读取器，与写入该存储的守护进程同一构建，所以随存储格式变化（§2）。
- **来源。** 在用的 home（`<home>/rust/dimcode.sqlite`），或用 `--database` 指定的一致副本。数据库以只读方式打开，不取守护进程的租约，也不升级；整次读取是一个 SQLite 读事务，所以导出在用的 home 得到的是一个快照（§4）。
- **内容。** 只有该 Session 的 Timeline，事件、before 轨迹与分支信息都用已有的序列化。读取器从不读取 provider、凭据或设置表（§6）。
- **敏感性。** 输出包含提示词、代码和工具输出。头行注明这一点，文件以私有方式创建且从不覆盖，帮助文本和 README 都有提示（§7）。

不在本设计内：功能清单中请求快照的过滤项（推理、工具记录、压缩内容）、查看器、评测工具，以及读取 ADR 005 之前写下的 trace 归档文件。

## 2. 入口

| 方案 | 经存储自己的读取器读取 | 随发布的 CLI 提供 | 在用的 home 或副本，只读 | 结论 |
| --- | --- | --- | --- | --- |
| A. `dimcode export` 运行同目录的 `dim-app-server --export-timeline` | 是：存储模块的读取器，与守护进程同一构建 | 是：每个平台包都含这两个二进制（`scripts/pack-release.mjs` 会断言） | 是：一次性进程以只读方式打开任意路径，不涉及守护进程 | **采用** |
| B. `dim-cli` 链接 `dim-app-server`，自己打开存储 | 是 | 是 | 是 | 否决：CLI 会成为存储的第二个所有者，并把整个 App Server（运行时、插件、SQLite）链接进一个目前只讲协议的客户端 |
| C. 守护进程方法，如 `session.timeline.export` | 是 | 是 | 否：守护进程在租约下以读写方式打开数据库，并升级旧格式，无法只读读取副本；还需要该 home 上有守护进程在运行 | 否决：还会增加一个协议面，大导出必须在 16 MiB 帧上限下分页 |

方案 A 沿用 `dim-app-server --connect-pipe` 的已有做法：同一二进制的一次性客户端模式，不加载产品状态。CLI 已经能找到同目录的二进制，`--server PATH` 可以覆盖它，所以 CLI 与读取器总是来自同一构建。`dimcode export` 正是 [CLI 命令](../cli-commands.md)为请求快照导出保留的命令名。每个最终模型请求都是一条 `model/call` 事实（ADR 005 D1），所以本导出带有这些快照；快照的过滤项不在本设计范围内。

`dimcode session export` 保持原样：经运行中守护进程的 `session.history` 导出 `main` 的对话，格式为 Markdown 或 JSON。

## 3. 结构

```
 dimcode export S [--output F] [--database D] [--home H]
   │  D 默认为 H/rust/dimcode.sqlite（H：--home、DIMCODE_HOME、~/.dimcode）
   │  以新建且私有的方式打开 F，或沿用自己的 stdout
   │  不启动守护进程，不打开 socket
   ▼
 dim-app-server --export-timeline S --database D      一个进程，没有插件
   │
   ▼
 SqliteStorage::open_read_only(D)
   │  只读连接，不取租约，BEGIN：整次读取一个快照
   │  精确比对结构：当前格式，或本构建可从之升级的格式
   ▼
 storage::export
   │  validate_session(S)：分支目录，按恢复时的规则校验
   │  每个分支 read_range 1..=head，分页读取：事件及其 before 轨迹
   ▼
 stdout：头行、分支行、事件行、结束行   ──►  F 或 CLI 的 stdout
```

导出位于存储模块中、`timeline.rs` 旁边，因此与 Session 恢复使用同一套行解码、请求内容重建和校验。它给 `SqliteStorage` 增加一个构造函数，给二进制的 `main` 增加一个模式；守护进程的路径不变。

## 4. 打开数据库

1. **不创建存储。** 路径必须指向已存在的普通文件。导出不创建目录、`.lock` 文件或数据库，也不执行任何建表语句。
2. **不取租约。** 守护进程的准入租约（`<database>.lock`）排斥第二个写入方。导出不是写入方，所以既不获取也不等待租约，守护进程占有数据库时也能运行。
3. **只读连接。** `SQLITE_OPEN_READ_ONLY`，忙等超时与守护进程相同，为 5 秒，即 `PRAGMA busy_timeout` 控制的设置，只作用于这条连接。不设置守护进程的其他 pragma（`journal_mode`、`synchronous`、`foreign_keys`、`fullfsync`）。数据库文件从不被写入。与任何读取 WAL 数据库的 SQLite 读者一样，会使用数据库旁的 `-wal` 和 `-shm` 文件，缺少时（正常关闭之后或新的副本）创建为空文件；目录只读且缺少这两个文件时，SQLite 无法读取该 WAL 数据库。
4. **一个快照。** 连接执行 `BEGIN` 并读完全部内容；关闭连接即结束读事务。第一次读取确定快照：导出期间守护进程的提交不可见，所以所有分支、head 和事件都来自同一时刻。读者不阻塞守护进程的写入；导出结束前，WAL 不能检查点越过该快照。
5. **格式。** 在快照内，结构必须与本构建能打开的某一格式完全相同：当前格式，或某个升级步骤的起点（对 v25 而言是 v13、v15、v16、v17、v18、v19、v20、v21、v22、v23 和 v24），且标记一致。升级增加表和派生行、删除冗余索引、替换标记，不改写已有事实，所以守护进程升级后使用的读取器能原样读取这些存储；导出按原样读取它们，不做升级。其他任何结构或标记（更新版本 dimcode 的存储、被改动的结构、旧 TypeScript 存储）都以 `unsupported Timeline storage format` 失败，数据库保持原样。

```
 守护进程（写入方）                  导出（读者）
   提交 n                             BEGIN
   提交 n+1  ──────────────────►      第一次读取：快照包含 ≤ n+1 的提交
   提交 n+2                           读取分支与事件（看不到 n+2）
                                      结束行，关闭
```

**副本。** 导出在用的 home 不需要副本。要在别处或之后导出，可在守护进程运行时用 SQLite 的 backup API 复制（例如 `sqlite3 dimcode.sqlite ".backup copy.sqlite"`），或在 `dimcode daemon stop`（`daemon.shutdown`）之后复制 `dimcode.sqlite` 以及旁边可能留下的 `dimcode.sqlite-wal`。正常关闭通常会把 WAL 检查点写入数据库并删除它，但如果另一个连接（例如正在运行的导出）仍打开着数据库，就不会这样做。复制运行中守护进程的文件不是快照（ADR 005 D3）。然后用 `--database` 传入副本。

## 5. 输出

UTF-8 JSONL，每行一个对象，各带一个 `kind`。外层字段用 snake_case，与其携带的原生序列化一致。导出版本 1：

| 行 | 字段 |
| --- | --- |
| `header`，第一行 | `export`（1）、`session_id`、`store_format`（所读数据库的格式）、`sensitive`（恒为 `true`） |
| `branch`，每个分支一行 | `info`：存储中该分支的目录行（`BranchRecord`：`id`，子分支另有 `parent`、`branch_point`、`create_anchor`，从不含 `source`）；`head`：快照中该分支的最后序号 |
| `event`，每个事件一行 | `branch_id`；`event`：`AgentEvent` 的序列化；`trace`：before 轨迹（`TimelineTrace`）或 `null`；`batch_end`：该事件提交时所在原子批次的最后序号 |
| `end`，最后一行 | `branches` 与 `events`：写出的数量 |

```
{"kind":"header","export":1,"session_id":"S","store_format":23,"sensitive":true}
{"kind":"branch","info":{"id":"main"},"head":41}
{"kind":"branch","info":{"id":"B","parent":"main","branch_point":12,"create_anchor":13},"head":7}
{"kind":"event","branch_id":"main","event":{"id":"…","sequence":1,"create_at":1761000000000,"type":"input","name":"message","data":{…}},"trace":null,"batch_end":1}
…
{"kind":"event","branch_id":"B","event":{"id":"…","sequence":7,…},"trace":{"event_id":"…","original":{…},"steps":[…]},"batch_end":7}
{"kind":"end","branches":2,"events":48}
```

- **顺序。** 先分支行，后事件行，均按存储的目录顺序（按分支 id）。每个分支的事件从 1 到其 `head`：即它自己的 Timeline，不含从父分支继承的前缀；读者用 `parent` 与 `branch_point` 重建树。
- **每个事件。** 控制事件（`stop/*`、`input/consumed`）、扩展事件、带完整最终请求的 `model/call`、用量、压缩、分支创建与失败、协作和轮次结果。不过滤、不合并、不重新推导；输出增量按各自的事件出现。
- **没有第二套结构。** `event`、`trace` 与 `info` 是 `AgentEvent`、`TimelineTrace` 和 `BranchRecord` 已有的 serde 序列化。事件正文随事件类型变化，不随导出版本变化。
- **版本。** 读者忽略未知的行类型和未知的外层字段。增加二者都不改版本；删除或改变其含义时提升版本。
- **完整性。** 只有以 `end` 行结束且数量相符的文件才是完整的。

## 6. 导出读取什么

| 表 | 是否读取 | 原因 |
| --- | --- | --- |
| `agent_store_format`、`sqlite_master` | 是 | 格式检查（§4） |
| `agent_sessions` | 该 Session 的行 | 与恢复时的检查相同：Session 存在且其行可以解析；配置不写出 |
| `timeline_branches`、`timeline_events`、`timeline_content` | 该 Session 的行 | Timeline 本身 |
| `providers`、`provider_imports`、`provider_selections`、`settings_documents`、`agents`、`session_owners`、`session_metadata`、Session 文件以及其他所有表 | 从不 | provider 记录可能内联 API key；设置和其他进程状态不属于 Timeline |

头行不含 Session 配置：每次调用的模型在其 `model/call` 请求中，D3 也把导出限定为 Timeline。

## 7. 敏感性与输出处理

- 头行的 `sensitive: true` 随内容一起流转。
- 给出 `--output PATH` 时，CLI 以新建语义创建文件，在 Unix 上权限为 0600，与 `dimcode session export` 相同，从不覆盖。相对路径以 CLI 的当前目录为基准。成功后向 stderr 输出 `Exported Timeline (sensitive): PATH`。导出失败时，CLI 删除它创建的文件。
- 不给 `--output` 时，JSONL 写到 stdout，由用户决定去向。
- 帮助文本和 CLI README 说明：导出包含提示词、代码和工具输出；API key 不会被读取，但如果用户或工具把它写进了 Timeline，它就会出现在导出中。

## 8. 区间指标

导出不增加任何计时字段。每个事件都带 `create_at`，即 Timeline 提交它的时间，早于存储发布它。使用方用两端的事实为区间命名，不把它称为执行时间（ADR 005 D1）：例如 `model/call → step/model-output-end` 还包括存储写入、分发和 ModelProvider 的重试，`step/tool-call → step/tool-result` 覆盖整个工具批次及其 Approval。所有者内部的操作耗时在 `daemon.log` 中，即 D4 的 span。

## 9. 错误

| 情形 | 结果 |
| --- | --- |
| 路径上没有数据库 | `No dimcode database at PATH`；退出码 1 |
| 不支持的格式 | `unsupported Timeline storage format at PATH; the database was not changed`；退出码 1 |
| 没有该 Session | `Session S is not in PATH`；退出码 1 |
| 存储的 Timeline 校验失败 | 读取器的错误，与恢复时报告的相同；退出码 1 |
| 只读目录中缺少 `-wal` 与 `-shm` 文件 | SQLite 的错误；把数据库复制到可写目录；退出码 1 |
| 写入失败（磁盘满、管道关闭） | 退出码 1；删除部分写出的文件 |
| `--output` 指向已存在的路径 | 导出开始前拒绝；退出码 1 |

退出码 1 是 CLI 现有的配置与传输错误码。导出从不重试。

## 10. 对其他文档的修改

- [CLI 命令](../cli-commands.md)：`dimcode export` 即本导出（此前为请求快照导出保留）；"其他目标命令"一段相应修改。
- [CLI README](../../../../crates/dim-cli/README.md)：命令、选项与敏感性说明。
- [App Server README](../../../../crates/dim-app-server/README.md)：`--export-timeline` 模式。
- [ADR 005](../../../adr/005-observability.md) 与 [ADR 索引](../../../adr/README.md)：第 3 步已实现。
- [功能清单](../../../feature-list.md)：`dimcode export` 导出一个 Session 的 Timeline。
- [文档索引](../../../INDEX.md)：本页。

## 11. 规则检查

| 规则 | 本设计如何遵守 |
| --- | --- |
| [原则](../../../architecture/rust-agent/principles.md) §1（插件与 Capability 有界） | 不适用：没有新增插件或 Capability |
| §4 第 1 条（改他人状态用命令） | 不适用：导出不改变任何状态 |
| §4 第 2 条（交接） | 不适用：没有交接 |
| §4 第 3 条（before 回调） | 不适用：没有新增；已有的 before 轨迹按存储原样读取 |
| §4 第 4 条（核心不含业务含义） | 导出原样复制事件和轨迹，不解读 |
| §4 第 5 条（批次） | 不适用：没有写入；`batch_end` 如实报告存储的批次边界 |
| §4 第 6 条（模型上下文） | 对模型不可见 |
| §4 第 7 条（公开事件与服务） | CLI 使用二进制已文档化的模式；导出只读取存储自己的模块 |
| §4 第 8 条（基础设施不变） | Timeline、Dispatcher、事件信封、存储格式、装配和 Session 门面都不变。读取器属于 SessionStore 本身，D3 决定了导出，唯一新增的是对现有存储的只读打开 |
| §4 第 9 条（Timeline 只存决策事实） | 不增加事实；计时不进入 Timeline（§8） |
| AGENTS.md v4 第 4 条（普通调用方经 `Session` 或 `Branch` 读取） | 导出不是运行中 Agent 内的调用方，而是存储的读取器，D3 指定它为读取路径；dim 之外没有任何东西读取这些表 |
| [ADR 002](../../adr/002-plugin-scope.md) D1（作用域随状态） | 没有插件：导出在插件图之外运行，与 `--connect-pipe` 相同 |
| ADR 002 D2 至 D5（不设接线模块、依赖方向、配置、语言中立的合同） | 不增加装配和配置；读取器留在 App Server 的存储模块中，使用运行时的类型；JSONL 各行是本文定义的语言中立合同（§5） |
| ADR 005 D3 | §2（存储的读取器，随发布的 CLI 提供）、§4（在用的 home 或副本，只读，格式）、§5（带版本的 JSONL，已有序列化）、§6（只有 Timeline）、§7（敏感）、§8（区间命名） |

## 12. 向机制所有者提出的需求

无。存储在自己的模块内增加只读打开；其他所有者不变。

## 13. 验收（流程测试）

只使用一次性数据库和隔离的 home，从不使用 `~/.dimcode`；不设计时阈值。

1. **带压缩与工具调用的多分支 Session。** 由真实 App Server 写下的 Session，含工具调用、一次完成的压缩和一个子分支。导出有一个头行；每个存储的分支一行，其 `info` 等于存储的目录；每个分支恰好是它从 1 到 `head` 的事件，与 TimelineStore 对该分支的读取（`read_segments`、`trace`）及其 `batch_end` 相同；结束行的数量相符。其中有 `model/call`、工具调用与结果、压缩和 `branch/create` 事实。
2. **只读打开。** 守护进程运行并持有租约时，导出在用的 home 成功。关闭后导出，数据库文件逐字节不变；导出一个用户不可写的文件也成功。
3. **拒绝不支持的格式。** 标记更新的一次性数据库、结构被改动的一次性数据库，都以不支持格式的错误失败并逐字节不变。本构建可从之升级的格式的存储（缺少后续各步新增表的 v13 结构，以及 v17 标记）不经升级即可读取。
4. **一致副本。** 守护进程运行时用 SQLite backup API 复制的副本，与同一时刻在用的 home 导出相同的行。
5. **不含设置或凭据数据。** 运行前存入一条内联 API key 的 provider 记录和一份设置文档，各含唯一标记；导出中两个标记都不出现。
6. **CLI。** 使用隔离的 `--home` 和构建出的服务端，`dimcode export` 写到 `--output` 与写到 stdout 的行相同，拒绝已存在的输出路径，导出失败时删除自己创建的文件，并且不启动守护进程。

没有产品插件注册 `before` 变换，所以第 1 项的 before 轨迹由测试插件的变换提供。流程测试：App Server `transport_flow` 中的 `timeline_export_reads_a_live_product_home_and_its_copies` 与 `timeline_export_carries_before_traces`，以及 CLI `cli_flow` 中的 `export_writes_the_timeline_without_starting_a_daemon`（CI 用构建出的 App Server 运行）。

## 14. 决定

| # | 决定 | 备选 | 理由 |
| --- | --- | --- | --- |
| D1 | `dimcode export` 运行同目录二进制的一次性模式 | CLI 链接存储；守护进程方法 | §2 |
| D2 | 整次导出一个读事务 | 像守护进程那样逐次自动提交读取 | 导出在用的 home 必须是同一时刻；逐次快照可能把不同时刻的 head 与事件混在一起 |
| D3 | 读取当前格式及本构建可从之升级的格式，不做升级 | 只读当前格式 | 升级从不改写行，守护进程的读取器本就能读取它们；拒绝会迫使用户为了导出而启动守护进程，而守护进程会升级存储 |
| D4 | 每个分支、每个事件一行，外加结束行 | 一个 JSON 文档；没有结束行 | 大 Session 可以流式写出而不必全部持有；结束行能发现被截断的文件 |
| D5 | 外层用 snake_case | 与协议一样用 camelCase | 携带的序列化（`create_at`、`branch_point`、`event_id`）都是 snake_case；一个文件只用一种写法 |
| D6 | 头行不含 Session 配置 | 头行包含配置 | D3 把导出限定为 Timeline；每次调用的模型在其请求中 |
