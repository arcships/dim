# Rust 会话存储格式

## v41：合并 child tool-call 与 Timeline 索引格式

main 已使用 v37 表示 child tool-call 事件信封、v38 表示 session_owners 允许协议4；本分支先前的 v37–v40 使用独立的成员/全文/数组/身份索引格式，不能只按编号判定两者兼容。

合并后的目标格式为 v41：保持本分支 v40 的全部索引与编码，并采用 main 的 session_owners 协议约束 `(1,2,3,4)` 和 parent_call_id 事件合同。不复用 v40 写入不同约束的 schema，不改变业务事实或 Usage 私有版本。

格式识别比较完整 SQL 结构和 marker。分别识别 main v37/v38 与本分支 v37–v40；只有 marker 和其完整结构均匹配才升级。main 路径从其已有 v36 索引集合补建成员、收窄全文、转换数组、建立输出身份/状态字段索引；本分支路径保留已建结构，只执行尚缺步骤。最后仅在 session_owners 仍是旧 `(1,2,3)` 约束时重建该表，原数据逐行复制。所有步骤仍在原准入事务内完成，最后写 v41 marker；失败整体回滚。

只读导出识别上述两个来源的完整旧结构，保持原 marker，不升级。v40 与更老的程序拒读新 v41。验收覆盖 main v37/v38、分支 v40、协议4的 Session owner、回滚、只读旧库导出和 fork 配置/数组共同复制。

## v39：大数组按元素引用

v39 增加本文件末尾[数组元素方案](#提案状态读取索引与数组元素)定义的 locator 表及 array-element 内容编码；v38 的 FTS、关联及成员索引合同不变。公开事件、trace 与 query 回复不增加存储字段。旧程序必须拒读 v39，不能把数组引用当成原内容读取。

大数组在写入事务内拆出；FTS 使用拆出之前的公开数据。已有数组版本按数组身份和长度复用，不重新保存它的元素。格式升级同事务建立元素存储并改写相应 wire，保留原事实身份、位置、批次和既有文本索引。失败回滚全部变化；普通打开不重复编码。只读旧格式导出仍按旧内联数组读取，不触发升级。

## v38：收窄全文与关联索引的写入范围

v38 保留 v37 的事件编码与 SQL 表，收窄索引的写入范围。`data` 全文索引覆盖核心保留域的事件，仍排除 context/prepare、model/call 和 context/compaction；不为不透明扩展事实建立全文倒排项。`data.delta` 的 step/text 索引不变。域集合复用事件信封合同，不在 Store 列出 Goal、InputQueue 或统计插件的类型。

request、input 和 user 关联索引保留原列及排序，改为只保存对应索引表达式非 SQL NULL 的记录。没有这些字段的事实不再写入三个无用的 NULL 索引项。JSON null 的 user 仍有 `json_type='null'`，保留原 Present 语义；非字符串关联值仍按公开查询合同处理。等值、In 与 Present 查询继续使用原表达式定位；缺失字段的查询保留通用执行语义，不强制套用不覆盖它的部分索引。

扩展事实仍完整持久化、导出和复制；TextCandidate 查询这些类型时保留候选，由调用方验证，不以索引缺失判定不匹配。混合核心与扩展类型的条件也必须保留未索引部分。普通结构化字段、身份、位置和关联查询不受影响。

v37→v38 在同一升级事务中删除覆盖范围外的全文元数据，由既有删除触发器清理对应倒排项，并以部分索引替换上述三个关联索引；不重写事实正文、不重建成员集合、不改变流式尾部。失败连同格式标记回滚，普通打开不重复清理。旧程序认为扩展事实的 data 全部已索引，可能漏掉新写扩展事实的候选，因此必须以 v38 格式拒绝旧程序，不能复用 v37。

数组元素内容编码仍采用后续独立格式；本步骤不引入数组表或新查询 API。

## v37：未完成集合索引的格式决定

本次格式采用 v37，不修改已提交的 v36。v37 增加 `timeline_member_nodes`、
`timeline_member_roots`、`timeline_member_origins` 和 `timeline_member_terminals`。
节点采用不可变 AVL 树：每节点保存一个成员、两个子节点身份与高度；更新复制并平衡受影响路径。
这是成员索引设计允许的简化实现，须通过相同的历史增长、固定 through 与累计 I/O 验收。
不同时引入叶块、内部节点两种编码或节点缓存。

根主键为 `(session_id, branch_id, index_id, group_key, sequence)`；反向定位主键为
`(session_id, branch_id, index_id, match_key, sequence, group_key)`；终态键主键为
`(session_id, branch_id, index_id, match_key)`，保留最早终态位置。节点按整数身份点查，
同时检查所属 Session。所有新增行与其原事实同事务提交；旧根不被后续提交修改。

升级事务按分支和原序号建立输入、工具、执行和协作集合；重复终态与终态先于开始均保留原查询语义。
失败回滚 schema、索引行和版本标记。Session 复制重建目标前缀索引，删除清理其全部索引行。
只有完整分支、无 publishing tail、与静态描述逐项相同的反关联可使用成员根；其余范围保留通用查询。
插件仍只调用 append/query，不读写这些表。

本步骤不更改事件编码。数组元素内容编码属于后续格式步骤，不能在提交 v37 后复用其编号。
v36 的文本索引和事件编码合同继续适用。

后续提案：[状态读取索引与数组元素](#提案状态读取索引与数组元素)。数组元素编码采用独立新格式；owner 结果复用既有事件索引。

The current native format is **v41**, with directly addressed large-array elements, versioned unfinished-member indexes, property text-candidate indexes, a submission identity index, a Session's activity time following its own activity, worktree directory and Session usage records, source-owned `settings_documents`,
SessionStore's Session-file rows, its title metadata, request messages stored
as lists of content-addressed nodes, extension event rows, Session
configurations and Agent profiles that may omit their mount list, the two
branch end events, the tool-effect and digest fields, and tool schemas that may
omit `type`, the Session catalog projection, a configuration's System source,
a model limit that only a declaration writes, a delegation's Agent owner and
member leader, a Session's creation origin, Session file grants, command output
ownership and the two context reading switches on timeline events. Startup
requires the exact v41 SQL schema and a single
format marker of 41 before writable PRAGMAs, or the exact v40, v39, v38, v37, v36, v35, v34, v33, v32, v31, v30, v29, v28, v27, v26, v25, v24, v23,
v22, v21, v20, v19, v18, v17, v16, v15 or v13 schema with its own marker,
which the same startup transaction upgrades in place to v41 one step after
another ([v36 text candidates](#v36-property-text-candidates), [v35 submission index](#v35-submission-identity-index), [#1099 v34 decision](#1099-v34-格式决定), [#911 v33 decision](#911-v33-格式决定), [#605 v32 decision](#605-v32-格式决定), [#708 v31 decision](#708-v31-格式决定), [#557 v30 decision](#557-v30-格式决定), [ADR 013 v29 decision](#adr-013-v29-格式决定), [#717 v28 decision](#717-v28-格式决定), [#593 v27 decision](#593-v27-格式决定),
[S5a v15 decision](#s5a-v15-格式决定),
[S5b v16 decision](#s5b-v16-格式决定), [#193 v17 decision](#193-v17-格式决定),
[#2 v18 decision](#2-v18-格式决定), [#364 v19 decision](#364-v19-格式决定),
[#427 v20 decision](#427-v20-格式决定), [#527 v21 decision](#527-v21-格式决定),
[#580 v22 decision](#580-v22-格式决定), [#556 v23 decision](#556-v23-格式决定),
[#555 v24 decision](#555-v24-格式决定), [#596 v25 decision](#596-v25-格式决定),
[#522 v26 decision](#522-v26-格式决定)).
Older databases and the removed
branch-only v14 `user_configuration` format are unsupported and preserved, never
migrated, downgraded or interpreted as an admitted format.
No migration API or startup option is provided. S5c (Session deletion and fork)
kept v15 unchanged ([S5c decision](#s5c-格式决定)). v13 document bytes remain
valid in v41, and v16 request rows stay readable as written.
除格式自身的 SQL 外，每次打开都接受派生索引：`timeline_history_anchors`
（`(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'input/message')`，
[latest-turn 窗口 §5.1](session-history-window.md#51-the-derived-index)）、
`timeline_policy_turns`（`(session_id, branch_id, kind, sequence, event_id) WHERE kind = 'turn/start'`，
已无读者，删除它要作为格式步骤）与事件查询的
`timeline_events_by_kind`（`(session_id, branch_id, kind, sequence, event_id)`，按 kind
读取事件而不碰事件体，[ADR 019](../../adr/019-event-query.md) D4）：读写打开在启动事务内用
`CREATE INDEX IF NOT EXISTS` 创建或补建，只读导出只接受它们。它们不含数据，可从
`timeline_events` 重新推导，读同一份事件不因它们有无而不同，因此不属于格式：启动门禁的
精确结构比较忽略这些名字，格式标记不变（当前 v41）；其余任何多余对象仍按原规则拒绝。代价是早于
该变更的二进制不认识这些名字，会以 "unsupported SQLite schema … preserved" 拒绝已有该
索引的库（行不变，新二进制可再次打开）。
Storage versions are independent of RPC versions. Private MCP/Skill business documents do not belong
in loader rows, events, diagnostics, Sessions or Timelines.

## 启动不兼容诊断

启动门禁仍然只做拒绝：库被保留，返回的错误文本不变，标记和业务行都不改。为了让
用户不必打开追加式 `daemon.log` 才知道原因，拒绝时在库旁原子写一份供人阅读的
启动说明 `<database>.startup`（Unix 上以 0600 创建；Windows 不自定义权限，沿用
home 目录继承的 ACL；正常打开不写）。说明只描述本次启动看到的路径、标记、当前
构建支持的格式和恢复办法，不复制历史日志，也不含业务内容或凭据。第一行是
`dimcode-startup-failure v1 pid=<pid>`，后面是正文。

**为什么是旁路文件，而不是子进程 stderr。** CLI 以 detached 方式启动守护进程：
`setsid`，stdin/stdout 为 null，stderr 直接追加到 `daemon.log`。守护进程比客户端
活得久，用管道捕获本次 stderr 会在客户端退出后让守护进程写向已关闭的管道；而
`daemon.log` 跨多次启动追加，不能可靠地取出“本次启动”的错误。旁路说明是本次启动
的最小通道：一个私有文件、首行带 PID，CLI 只校验并打印它，不解析日志，也不复制
业务内容。

**升级进度。** 格式升级可能远长于启动等待。升级事务运行期间，存储 owner 在同一位置
原子改写进度说明，首行是 `dimcode-startup-progress v1 pid=<pid>`，正文是
`store-upgrade from=<from> to=<to> progress=<n>`：`n` 每次重写递增。SQLite 的进度回调
最多每秒报告一次，每个升级步骤完成时再报告一次；库新建或已是最新格式时不写。进度
说明让 CLI 区分“仍在升级”和“卡住”，不含业务内容或凭据，成功打开时与遗留说明一起
删除。只认失败首行的旧版 CLI 读到进度说明时按“没有说明”处理，退回通用错误文本；
CLI 等待规则见 [CLI 命令 §守护进程启动失败](../cli-commands.md#daemon-startup-failure)。

**库被其他进程占用。** 存储的准入租约 `<database>.lock` 已被另一个进程持有：它正在服务
这个库，或正在启动。本次启动拿不到库，返回的错误文本不变；拒绝时同样写一份启动说明，
正文点名占用者。库旁有进度说明时，正文带上该进程的 pid 和它正在升级的格式；否则只说库
正被另一个 dimcode-server 使用；两种正文都让用户等它结束再运行命令。这次写入会盖掉旁边
的进度说明，占用者的下一次进度报告会把它改写回来；等待中的客户端在这一次轮询里读不到
进展。占用不是格式诊断：库本身没有被改动。

```text
Cannot open native store <path>.
  Another dimcode-server (pid <pid>) is upgrading it from format v<from> to v<to>.
  The store stays with that process until the upgrade finishes; wait for it, then run the command again.
```

```text
Cannot open native store <path>.
  Another dimcode-server is using the store.
  Wait for it to finish, then run the command again.
```

“本构建认识的形状”指当前格式的表和索引集合，或某个升级步骤起点的集合：结构完全相符时
先看标记是否等于该版本，再看是否需要升级。结构不相符时按可读的单一标记分类，下表四行
互斥。

| 情形 | 判据 | 说明的结论 |
| --- | --- | --- |
| 更新格式 | 结构不是本构建认识的形状，`agent_store_format` 可读且恰有一行 `version > FORMAT`（当前 36） | 本构建读不了这个更新格式的库 |
| 标记与结构不符 | 结构是本构建认识的形状（当前格式或某个升级起点），但标记行不是该版本的唯一值 | 标记和结构不一致；不诊断为“降级” |
| 无法识别 | 结构不是本构建认识的形状，且可读的单一标记在本构建接受的范围内（`MIN_FORMAT ≤ version ≤ FORMAT`，已移除的 v14 除外），或没有可读的单一标记 | 结构无法识别；读出标记时一并给出 |
| 过旧或不再准入 | 结构不是本构建认识的形状，且可读的单一标记低于 `MIN_FORMAT`，或为已移除的 v14 | 该格式不再被这个构建读取 |

正文模板（`<path>`、`<marker>`、`<schema>` 由存储填写；`<format>` 取 `FORMAT` 常量（当前 36），不写字面量）。
更新格式：

```text
Cannot open native store <path>.
  Store format: v<marker>
  Daemon format: v<format>
  This daemon cannot read the store. Existing data was preserved.
  Run a build supporting v<marker>, or use --home PATH for a separate environment.
```

标记与结构不符：

```text
Cannot open native store <path>.
  Store format: v<marker> (schema is v<schema>)
  Daemon format: v<format>
  The store's format marker and schema disagree. Existing data was preserved.
  Use the build that wrote this store, or use --home PATH for a separate environment.
```

无法识别（结构不相符，但读出了单一标记）：

```text
Cannot open native store <path>.
  Store format: v<marker> (unrecognized schema)
  Daemon format: v<format>
  The store is not a format this daemon recognizes. Existing data was preserved.
  Use the build that wrote this store, or use --home PATH for a separate environment.
```

无法识别（没有可读的单一标记）：

```text
Cannot open native store <path>.
  Store format: unrecognized
  Daemon format: v<format>
  The store is not a format this daemon recognizes. Existing data was preserved.
  Use the build that wrote this store, or use --home PATH for a separate environment.
```

过旧：

```text
Cannot open native store <path>.
  Store format: v<marker> (no longer read by this daemon)
  Daemon format: v<format>
  This daemon cannot read the store. Existing data was preserved.
  Use a build that reads v<marker>, or use --home PATH for a separate environment.
```

`Store format` 行按分支取值的全部情况：标记与结构不符且读出单一标记时写
`v<marker> (schema is v<schema>)`，否则写 `v<schema> (format marker is missing or ambiguous)`；
结构不相符时，读出单一标记写 `v<marker>`，读不出则只写 `unrecognized`。

- **写入与清理**：拒绝前用同目录临时文件写入并原子替换；写入失败不改变返回值，原错误照常返回。正常打开、升级成功和其他启动失败不写失败说明；格式升级期间写进度说明，成功打开（含升级成功）时存储 owner 删除遗留说明，删除失败不影响启动，下一次成功打开再清理。说明的读取、打印和其余清理属于 CLI 启动，见 [CLI 命令 §守护进程启动失败](../cli-commands.md#daemon-startup-failure)；说明不进入日志历史。
- **位置**：说明是库的同级文件，不是新数据库格式，也不是 `daemon.log` 的一部分。CLI 启动的守护进程使用 `<native home>/dimcode.sqlite`，所以 CLI 读取 `<native home>/dimcode.sqlite.startup`。直接以 `--database` 启动的守护进程把说明写在它的库旁，CLI 的启动路径不读取。
- **不自动恢复**：不删除、重建、降级、重置标记或复制库；`--home PATH` 只建立另一个环境。终端说明只给兼容构建和 `--home` 两条路；开发说明继续使用 `./dev` 的隔离 home。
- **验证**：`storage_flow` 用一次性库覆盖全部分支：v39 标记的未知结构被拒绝、库字节不变，说明为“更新格式”；当前格式的库把标记改成 v29，以及 v13 结构配 v32 标记，都得到“标记与结构不符”而不是“更新格式”；当前格式的空标记表得到 `v38 (format marker is missing or ambiguous)`；未知结构配可读标记 v20 得到 `v20 (unrecognized schema)`，没有可读标记得到 `unrecognized`；v12 和已移除的 v14 得到“过旧”；正常打开和升级成功删除遗留说明，失败替换上一次留下的说明。CLI 启动流程另覆盖终端打印、PID、权限和控制字符，见 [CLI 命令 §守护进程启动失败](../cli-commands.md#daemon-startup-failure)。

## SQLite 运行参数与空间回收

进程用一个写连接打开本库，另有四个只读连接服务读取（[#599](https://github.com/arcships/dim/issues/599)）：写连接在格式门禁及其准入事务提交后才设置连接级 PRAGMA：`journal_mode=WAL`、`synchronous=FULL`（macOS 另加 `fullfsync`）、`foreign_keys=ON`、`wal_autocheckpoint=0`、`journal_size_limit=64 MiB`、`cache_size=-32768`（32 MiB）。只读连接以 `SQLITE_OPEN_READ_ONLY` 打开，只设 `busy_timeout=5s`、`cache_size=-32768` 和 `catalog_fold` 函数，不设 `journal_mode`、`synchronous`、`foreign_keys`、`wal_autocheckpoint`、`journal_size_limit` 或 `fullfsync`。每次读取在其语句开始时的最后提交上运行，不等待写事务或 fsync；调用按顺序轮换连接，目录扫描等长读不会占住其他读取。写连接与 fork/delete 的复制连接用 `sqlite3_busy_handler`：每 1 ms 重试一次、约 5 秒后放弃，两者因此轮流取得写锁，而不是一方在另一方退避期间长期占有。

| 参数 | 值 | 作用 |
| --- | --- | --- |
| `wal_autocheckpoint` | `0` | 关闭提交事务内的自动检查点。writer 在每轮提交并发布后、WAL 达到 4 MiB（SQLite 默认的 1000 页）时执行 `PRAGMA wal_checkpoint(PASSIVE)`；检查点因此不占提交路径。只读连接不自动检查点。 |
| `journal_size_limit` | 64 MiB | 检查点重启 WAL 后，文件截到不超过该值；单轮写入本身更大时保留该轮的大小。导出等读者持长读事务时 WAL 仍会在导出期间增长，导出结束后下一次检查点照常复制并重启。被动检查点不等待读者。 |
| `cache_size` | 每个连接 32 MiB | 在 650 MB、2,300 个 Session 的一次性库上重复读取每个 Session 的尾部事件：缓存为 2–16 MiB 时十次读取共从分页器读约 205 MB，32 MiB 起降到约 20 MB，64 MiB 与 128 MiB 不再减少。32 MiB 是容纳该热点工作集的最小值；写连接和四个只读连接各用该值。 |
| `busy handler` | 每 1 ms，约 5 秒 | 写连接与 fork/delete 的复制连接共用；两者在同一 WAL 上交替取得写锁，使分块的复制/删除与写入轮都能前进。 |
| `foreign_keys` | 复制连接 `OFF` | fork 在最后一个事务才插入 `agent_sessions`，此前先写事件、内容、快照与分支；SQLite 的外键是连接级设置，复制连接关闭它，最后一个事务在提交时仍满足全部约束。 |

关闭时先关闭四个只读连接，再执行 `PRAGMA wal_checkpoint(TRUNCATE)`，把 WAL 折回数据库并截为零；另一个连接（如运行中的导出）仍持读事务时这一步可能失败，下次打开继续。

fork 与 Session 删除各用一条复制连接，按 256 个逻辑序号（内容按 1024 个哈希）分块提交，每块之间让写入轮取得写锁。复制连接与写连接一样在 macOS 上设置 `fullfsync`。fork 的中间块以 `synchronous=NORMAL` 提交，最后插入 `agent_sessions` 的事务以 `FULL` 提交：它的 fsync（macOS 上为 F_FULLFSYNC）覆盖此前写入 WAL 的全部帧，中断只留下不可达的行，不会出现可见但不完整的 Session。删除先在一个 `FULL` 事务中移除使 Session 可见的行（文件与输出归属、授权、标题、目录、owner、session 与分支），再以 `NORMAL` 分块删除事件、快照和内容；中断只留下不可达的行，重试同一 id 即可清除。两者都不改变持久化格式。

删除 Session 只释放库内页，数据库文件本身不缩小。维护入口是 `dimcode daemon vacuum [--database PATH]`：守护进程未运行时它取同一把维护锁，校验当前格式的精确结构后执行 `VACUUM`，打印操作前后数据库与 WAL 的字节数之和。`VACUUM` 重写整个库，需要约等于库大小的临时空间；守护进程持锁时该命令拒绝执行，也不升级或创建库。

## Current v41 tables

下表逐列对应 `SqliteStorage::SCHEMA`。新表按业务实体命名，其中 `agent_sessions` 与旧 TypeScript 的 `sessions` 区分；当前启动门禁仍只接受本格式，不因此支持旧表同库存取。`—` 表示 SQL 中没有显式 `DEFAULT`；可空列若省略会得到 SQLite 的隐式 `NULL`。SQLite rowid 表中的 `TEXT PRIMARY KEY` 不自动保证非空，因此下表如实标出 v1 的两个缺口；正常写入仍必须给出 ID。

| 表.列 | SQLite 类型 | 可空 | 默认 | 约束与含义 |
| --- | --- | --- | --- | --- |
| `agent_store_format.version` | `INTEGER` | No | — | Primary key, `CHECK(version = 41)`; exactly one row |
| `settings_documents.source` | `TEXT` | No | — | Primary key; `user/mcp`, `user/skills`, `user/mcp-projects` or `user/mcp-plugins`; workspace files are not mirrored |
| `settings_documents.version` | `INTEGER` | No | — | Positive per-source CAS revision; token `native:<source>:<version>` |
| `settings_documents.document` | `TEXT` | No | — | Plain business payload JSON, without a `{kind, document}` envelope; unknown business fields are preserved |
| `settings_documents.imported_revision` | `TEXT` | No | — | Original file SHA-256 or `missing`; import provenance, not a CAS token |
| `provider_imports.kind` | `TEXT` | 否 | — | 联合主键；`provider` 或 `selection` |
| `provider_imports.id` | `TEXT` | 否 | — | 联合主键；来源业务 ID，删除 Provider 不删除标记 |
| `provider_imports.source_schema` | `TEXT` | 否 | — | 已检查的来源两表结构 SHA-256 |
| `provider_imports.source_version` | `INTEGER` | 否 | — | 来源行版本，不作目标 CAS 版本 |
| `provider_imports.source_digest` | `TEXT` | 否 | — | 来源完整行 SHA-256，不包含源文件路径，不通过 RPC 返回 |
| `providers.provider_id` | `TEXT` | 否 | — | 主键；managed、bundled 与 custom 共用 |
| `providers.version` | `INTEGER` | 否 | — | 正整数，目录与启用状态的 CAS 版本 |
| `providers.record` | `TEXT` | 否 | — | authority-specific `ProviderRecord` JSON；managed 记录不含凭据，静态记录可含请求凭据且只允许非秘密投影读取 |
| `provider_selections.scope` | `TEXT` | 否 | — | 主键，`global` 或 `workspace:<absolute cwd>` |
| `provider_selections.version` | `INTEGER` | 否 | — | 正整数，选择的 CAS 版本 |
| `provider_selections.provider_id` | `TEXT` | 否 | — | Provider ID；与 model_id 共同标识目标 |
| `provider_selections.model_id` | `TEXT` | 否 | — | 对应 Provider 的模型 ID；模型暂不可用时保留选择 |
| `agents.agent_id` | `TEXT` | **SQL 允许** | — | 主键；正常写入必须是非空 Agent ID |
| `agents.profile` | `TEXT` | 否 | — | `AgentProfile` JSON，创建后固定 |
| `agents.is_default` | `INTEGER` | 否 | — | `CHECK(is_default IN (0, 1))`；唯一的 1 由 `agents_default_unique` 保证至多一个，至少一个由目录校验保证 |
| `agents.owner_session_id` | `TEXT` | 是 | — | 外键到 `agent_sessions(session_id)`；非空表示该 leader Session 拥有这个私有 Agent，NULL 表示公开 Agent；写入后不变 |
| `session_catalog.session_id` | `TEXT` | No | — | Primary key, foreign key to `agent_sessions`; one row from creation |
| `session_catalog.updated_at_ms` | `INTEGER` | No | — | Time of the newest Session-activity event on `main` (issue #1099), or creation time for a Session with none |
| `session_catalog.preview` | `TEXT` | Yes | — | First user text in the batch-complete preview window |
| `session_catalog.cwd` | `TEXT` | No | — | Creation configuration workspace |
| `agent_sessions.session_id` | `TEXT` | 否 | — | 主键，原 Session ID |
| `agent_sessions.created_at_ms` | `INTEGER` | 否 | — | `CHECK(created_at_ms >= 0)`；创建事务写入真实 Unix 毫秒 |
| `agent_sessions.config` | `TEXT` | 否 | — | `SessionConfig` JSON，创建时的配置 |
| `agent_sessions.leader_session_id` | `TEXT` | 是 | — | 外键到 `agent_sessions(session_id)`，保存 [目录的父会话](session-classification.md#31-the-origin)。协作成员资格由会话的创建角色确定 |
| `agent_sessions.origin` | `TEXT` | 否 | `'unknown'` | `CHECK (origin IN ('client', 'acp', 'internal', 'unknown'))`；创建来源，创建事务写入一次；默认值只为让 `ALTER TABLE` 能加非空列，写入方一律显式给出（v30，[#557](https://github.com/arcships/dim/issues/557)） |
| `session_owners.session_id` | `TEXT` | **SQL 允许** | — | 主键，外键到 `agent_sessions(session_id)`；业务要求每个 Session 恰有一条非空 owner |
| `session_owners.agent_id` | `TEXT` | 否 | — | 外键到 `agents(agent_id)` |
| `session_owners.protocol_version` | `INTEGER` | 否 | — | `CHECK(protocol_version IN (1, 2, 3, 4))`；Session 所属协议版本 |
| `timeline_branches.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `timeline_branches.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列；同 Session 内唯一 |
| `timeline_branches.info` | `TEXT` | 否 | — | `TimelineInfo` JSON：`id`、可选 `parent`、`branch_point`、`create_anchor` |
| `timeline_branches.sequence` | `INTEGER` | 否 | — | `CHECK(sequence >= 0)`；本分支最新已提交事件序号，派生水位 |
| `timeline_content.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `timeline_content.hash` | `TEXT` | 否 | — | 联合主键第 2 列；同 Session 内内容地址 |
| `timeline_content.kind` | `TEXT` | 否 | — | `node`、`tools` 或 `system`，以及 v16 写入的 `message`；v1 无 SQL 枚举 `CHECK`，读取时校验 |
| `timeline_content.body` | `TEXT` | 否 | — | 对应类型的完整 JSON 正文；写入后不可原地修改 |
| `timeline_events.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；与 `branch_id` 一起外键到 `timeline_branches` |
| `timeline_events.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列 |
| `timeline_events.sequence` | `INTEGER` | 否 | — | 联合主键第 3 列，`CHECK(sequence > 0)`；物理行首事件的本分支序号 |
| `timeline_events.last_sequence` | `INTEGER` | 否 | — | `CHECK(last_sequence >= sequence)`；物理行末事件序号，合并行可大于 `sequence` |
| `timeline_events.kind` | `TEXT` | 否 | — | `EventKind::key()`，如 `model/call`、`step/text`；与正文逐事件核对 |
| `timeline_events.body` | `TEXT` | 否 | — | 下节定义的 `Row` JSON，包含事件、引用、before 轨迹及批次终点 |
| `timeline_events.event_id` | `TEXT` | 是 | 隐式 `NULL` | 普通行保存原事件 ID；输出合并行为空；恢复时核对正文 |
| `timeline_events.context_render` | `INTEGER` | 否 | `0` | 0/1 冗余开关：渲染器是否消费这行（v31，[#708](#708-v31-格式决定)） |
| `timeline_events.context_state` | `INTEGER` | 否 | `0` | 0/1 冗余开关：checkpoint 折叠是否消费这行（v31，[#708](#708-v31-格式决定)）；扩展事件取事件上的 `context_state` |
| `timeline_snapshots.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；与 branch_id、sequence 一起外键到事件锚点 |
| `timeline_snapshots.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列 |
| `timeline_snapshots.sequence` | `INTEGER` | 否 | — | 联合主键第 4 列，`CHECK(sequence > 0)`；已提交 completed 压缩事件序号 |
| `timeline_snapshots.interpreter` | `TEXT` | 否 | — | 联合主键第 3 列；解码前可检查的解释器兼容合同 |
| `timeline_snapshots.event_id` | `TEXT` | 否 | — | 锚点的精确事件 ID，写入和候选查询校验 |
| `timeline_snapshots.state` | `TEXT` | 否 | — | 不透明原始字符串；SQLite 不解析或反序列化 |
| `session_files.file_id` | `TEXT` | 否 | — | 主键；小写带连字符的 UUID v4，也是 `<db>.session-files/` 下的目录名（v15） |
| `session_files.node` | `TEXT` | 是 | 隐式 `NULL` | 写入时 Session 的资源节点；`NULL` 表示控制节点。owner 全部删除后仍保留，供清扫定位 |
| `session_files.writer` | `TEXT` | 是 | 隐式 `NULL` | 写入未完成时是 SessionStore 代次 ID；提交或放弃后为 `NULL` |
| `session_file_owners.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `session_file_owners.file_id` | `TEXT` | 否 | — | 联合主键第 2 列；外键到 `session_files(file_id)` |
| `session_grants.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)`；随 Session 删除（v28） |
| `session_grants.path` | `TEXT` | 否 | — | 联合主键第 2 列；规范绝对路径；文件 grant 精确匹配，目录 grant 按路径分量包含子项 |
| `session_grants.kind` | `TEXT` | 否 | — | `CHECK (kind IN ('file', 'dir'))` |
| `session_grants.mode` | `TEXT` | 否 | — | `CHECK (mode IN ('read', 'write'))`；write 同时允许读，read 不降级已有 write |
| `session_output_owners.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)`；随 Session 删除（v29，ADR 013） |
| `session_output_owners.path` | `TEXT` | 否 | — | 联合主键第 2 列；命令输出的规范绝对路径；fork 复制行，删除时按父目录清理 |
| `worktree_directories.repository` | `TEXT` | 否 | — | 联合主键第 1 列；规范公共 Git 目录，UTF-8 绝对路径（v33） |
| `worktree_directories.root` | `TEXT` | 否 | — | 联合主键第 2 列；worktree 规范根目录；最后关联解除后保留 |
| `session_worktrees.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)`；随 Session 删除，不随 fork 复制 |
| `session_worktrees.repository` | `TEXT` | 否 | — | 联合主键第 2 列；与 `root` 联合作为外键引用 `worktree_directories(repository, root)` |
| `session_worktrees.root` | `TEXT` | 否 | — | 联合主键第 3 列；关联的 worktree 根目录 |
| `session_worktrees.subtree` | `TEXT` | 否 | — | 本 Session 核验的继承子树；规范 UTF-8 绝对路径，位于 `root` 内 |
| `session_metadata.session_id` | `TEXT` | 否 | — | 主键；外键到 `agent_sessions(session_id)`；每个 Session 至多一行，没有行表示修订号 0、没有标题（v16） |
| `session_metadata.revision` | `INTEGER` | 否 | — | `CHECK(revision > 0)`；该 Session 已提交 `setTitle` 的次数，`setTitle` 的 CAS 修订号 |
| `session_metadata.title` | `TEXT` | 是 | 隐式 `NULL` | 标题文本，原样保存；清除后为 `NULL`，行保留 |
| `session_metadata.title_source` | `TEXT` | 是 | 隐式 `NULL` | `CHECK(title_source IN ('auto', 'manual'))`；表级 `CHECK((title IS NULL) = (title_source IS NULL))` |

v1 的外键只约束上述表列；`info.parent`、`body` 中的内容哈希、before 轨迹和检查点位置都是 JSON 内关系，必须在存储读取边界校验。`timeline_content` 的地址是小写十六进制 `SHA-256(kind + NUL + body)`，其中 `body` 是写入的原始 JSON 字符串字节；同一内容只在同一 Session 内共享。创建 Session、owner 和 `main` 分支是同一事务；写入轮把事件、首次出现的内容、由 `branch/create` 生成的分支目录及分支 head 放在同一事务，提交后才发布。

## 持久化 JSON 类型

### 配置来源与单写者

Settings owns each source independently. Original v13 rows under `user/mcp` or
`user/skills` contain the plain MCP or Skill document JSON, not a tagged envelope.
`user/mcp-projects` holds MCP's user-local project enablement
(`{"enabled":{"<canonical workspace>":["<server id>"]}}`, ADR 003 D3). Its import
file is `mcp-projects.json`, which only the standalone file backend writes; a
native home normally has none, so the first read imports `missing` and an empty
record. The row is an additive source within
v13: the schema and format version are unchanged, and older binaries ignore it.
`user/mcp-plugins` holds the MCP documents plugins registered with Mcp
(`{"plugins":{"<plugin id>":{"dir":"<plugin directory>","mcpServers":{…}}}}`,
[PluginManager §3.7](../plugins/PluginManager.md#37-content-registration)), and
`user/skills` may carry `pluginSkills` (`{"<plugin id>":"<skills root>"}`); both
are additive in the same way.
Generic Settings persistence retains these exact payload shapes, not loader
patches or UUID snapshots. Business interpretation and validation belong to the namespace owner.
Unknown extension fields and disabled Skill IDs are preserved.

For example, these original v13 rows store `document` as TEXT containing the
plain payload, not a serialized SettingsDocument wrapper:

```json
[
  {"source":"user/mcp","version":1,"document":"{\"mcpServers\":{}}","imported_revision":"missing"},
  {"source":"user/skills","version":2,"document":"{\"disabledSkillIds\":[\"example\"]}","imported_revision":"missing"}
]
```

Their CAS tokens are `native:user/mcp:1` and `native:user/skills:2`. Updating
Skills increments only its version and preserves `imported_revision`.

The first successful source read imports its legacy file into SQLite. Document
and import provenance commit atomically in one row; concurrent imports adopt
one winner. Invalid files or failed writes create no authoritative row and do
not fall back to empty settings. Originals remain available for repair/retry.
After import, SQLite is authoritative: edits CAS the named source revision,
increment only that source's version, and never rewrite or reread the old file
as runtime authority. Import provenance does not change on ordinary edits.
Project `.mcp.json` keeps file authority and content CAS; project enablement is
never written there. Standalone hosts may explicitly use the file backend.

Revision notifications follow successful persistence and contain no business
document bodies. They certify neither loader adoption nor generation settlement.
Loader rows represent runtime instances, not the MCP/Skill document authority.
Importing legacy settings files does not authorize migrating users' databases.

### 目录与内容

Managed `providers.record` is `{kind:"managed", account, models, modelConfigOverrides, disabledModelIds, version, modelPreferences?}`. The optional `modelPreferences` stores per-model client preferences under the #329 format decision; missing means none. Each model is `{entry, protocol, unavailableReason, zdr}`; the plugin returns the normalized `ModelEntry` and public host protocol. Only non-secret account identity is stored here. Refresh and access tokens remain in the separately supplied plugin's account store. Managed and static variants share the existing `providers` table; the v10 table and column layout is unchanged, with the format marker advanced to v11. Provider catalog replacement and user overrides are owned by the App Server, as described in the [Provider contract](../../../crates/dim-app-server/README.md).

静态目录的 record 为 `{kind, displayName, driverKind, auth, credentialAuthority, baseUrl, network, headers, credential, models, remoteModels, endpointTypes, modelConfigOverrides, disabledModelIds, enabled, definitionFingerprint, version, userModels?, modelPreferences?, endpointOverrides?}`。可选的 `userModels`、`modelPreferences`、`endpointOverrides` 沿 #329 格式决定保存用户模型、偏好和端点覆盖，缺失表示无对应配置。`kind` 为 `builtin` 或 `custom`；models 和覆盖均为完整 `ModelEntry`；`definitionFingerprint` 是 bundled 定义的 SHA-256，自定义记录为 null。`credentialAuthority` 为 `static`、`openai_codex` 或 `grok`，缺省仅解释为静态来源；后两者只允许对应内置 Provider 的固定非秘密绑定，credential 必须为空。`credential` 可保存静态凭据或不透明秘密引用，但不写回引用解析结果；`network` 保存逐连接代理与超时，只读 `provider.list` 不返回代理 URL；`provider.management` 只返回去除 user info 的 `proxyUrl`，代理认证仍不返回，展示值不得自动写回。`remoteModels` 标记已取得的独立模型快照，避免 bundled 升级覆盖；`endpointTypes` 保存显式协议映射，派生路由不落盘。凭据和 header 值只由 resolver 使用，管理面仅返回非秘密摘要。写入及读取边界校验模型、地址和请求头，外层 SQL 版本须与 record.version 一致；seed、编辑和删除规则见[Provider合同](../../../crates/dim-app-server/README.md)。

`agents.profile` 是 `AgentProfile`：`{name, description, systemPrompt, tools?, model?}`；`model` 若存在为 `{providerId, modelId}`。`agent_sessions.config` 是 `SessionConfig`：`{permissionPolicy, cwd, model, systemPrompt, tools?, collaborationRole, interactive?, language?}`；两者的 `tools` 是字符串数组形式的挂载列表，没有挂载列表时省略（[Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields)，格式决定见 [#364 v19 格式决定](#364-v19-格式决定)）；其中 `model` 是 `ModelSelection {providerId, modelId, config?, metadata?}`，创建时固定完整模型参数，省略参数的身份记录仍可读取。`permissionPolicy` 为 `{preset, allowedTools, disallowedTools}`（兼容规则见下段），`collaborationRole` 为 `leader` 或 `member`。可选字段 `interactive`（布尔，缺省 `true`）由[路线图](../roadmap.md) C2 加入，已实现，格式决定如下：保持 v13，不升级到 v14，这是配置 JSON 中增量的可选字段；缺少它的已存储记录表示 `true`，因为该字段出现之前创建的每个 Session 都是客户端 Session；只写入 `false`，交互式记录不含该字段，与该字段出现之前写入的记录格式相同；这一省略规则写在 `SessionConfig` 的 serde 定义上，原生存储是它唯一的序列化方，在其他地方暴露 `SessionConfig` 须另行决定；旧版守护进程忽略该字段，会把非交互记录读成交互式，这一点可以接受。可选字段 `language`（回复语言代码）同样是配置 JSON 中的成员，缺少它表示操作者没有选择；原生存储保持 v13，并在 `session.language.update` 时就地更新该成员，其余内容不变；旧版守护进程忽略它。字段合同见 [Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields)。`timeline_branches.info` 是 `BranchRecord`：`main` 为 `{id:"main"}`，子分支为 `{id, parent, branch_point, create_anchor}`；后两项是父分支的本地序号。运行时的 `TimelineInfo` 是它加上 `source`，该字段不落盘。JSON 字段名按各类型的 serde 定义，不能由 SQL 列名的拼写推断。

`PermissionPolicy` 的格式决定见 [ADR 018](../../adr/018-permission-is-the-preset.md)。
SQL schema 和格式标记不变，读取不重写记录。策略对象中的未知键被忽略，包括已删除的
`fileAccess` 和 `toolApproval`；保存时只写入预设和工具列表。Session 配置与
`permission/changed {policy}` 的正文采用同一策略类型，外层记录仍严格校验。
旧记录按保存的预设执行，包括曾受更严格覆盖字段限制的记录；不映射旧字段、不添加提示。
字段语义属于 [Permission 合同](../../architecture/rust-agent/plugins/Permission.md#policy-contract)。

`context/configured` 的 `ModelConfiguration` 分别固定模型与工具部分：模型使用上述参数，`tools` 为已采用声明，可选 `toolSources` 为 `{key,version}[]` 的来源版本；没有来源时省略。模型更新保留已采用的工具部分，工具采用保留模型部分，完整合同见[App Server合同](../../../crates/dim-app-server/README.md)。

`timeline_content.body` 的 `node` 是 `{parent,message}`：`message` 是一条消息，`parent` 是前一条消息所在节点的地址，首条消息为 `null`，所以一个节点标识从首条到它的整段消息前缀（v17）。消息正文 `{role,content}` 与 v16 的 `message` 种类相同，`role` 为 `user`/`assistant`/`tool_result`，`content` 为文本或 `MessagePart[]`；v16 写入的 `message` 行照常可读，v17 不再写入。`tools` 是 `ToolDefinition[]`，每项为 `{name,description,inputSchema,source,risk?,metadata?,fileChanges?}`，嵌套的 schema/metadata/fileChanges 保持现有类型定义；`system` 是文本或 `SystemPromptPart[]`，当前部件为 `{type:"text",text,cache_control?}`。各类正文均直接按对应运行时类型的 JSON 序列化，节点只多出 `parent`。

### 事件封装

`timeline_events.body` 的完整外层联合为：

```text
Row =
  { encoding: "event", record: WireEvent }
  | { encoding: "output", turn_id: string, request_id: string,
      started_at: integer, records: WireEvent[] }
WireEvent = { event: AgentEvent, refs: ListReferences | References | null,
              trace: WireTrace | null, batch_end: integer }
ListReferences = { list: string | null, length: integer,
                   tools: string | null, system: string | null }        v17
References = { messages: string[], tools: string | null, system: string | null }   v16, read only
WireTrace = { event_id: string, original: WireDraft, steps: WireStep[] }
WireDraft = { event: EventDraft, refs: ListReferences | References | null }
WireStep = { handler: string, result: WireDraft | null }
AgentEvent = { id: string, sequence: integer, create_at: integer,
               tags?: string[], type: string, name: string, data: object }
EventDraft = { tags?: string[], type: string, name: string, data: object }
```

`AgentEvent` 与 `EventDraft` 的 `{type,name,data}` 是 `EventKind` 标记联合，核心定义的事件由该变体的类型定义（如 `ModelCallData`、`ContextCompactionData`）校验；存储没有第二套业务事件定义。`EventDraft.id` 是 runtime 的进程内预分配字段，不写入存储；持久化身份由 `AgentEvent.id` 表示。核心未定义领域的扩展事件（`EventKind::Extension`，[ADR 001](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)）以同一信封原样保存 `data`，`kind` 为 `<type>/<name>`；核心保留领域的行只按其类型解码，解码失败即报错，不当作扩展事件。格式决定见 [#2 v18 格式决定](#2-v18-格式决定)。`ModelRequest` 正文为 `{providerId,modelId,messages,tools?,system?,config?,metadata?}`，末尾四项是可选字段，缺省时省略，不能写 `null`。`event` 行有且仅有一个 `record`，其 `sequence = last_sequence = timeline_events.sequence`；`output` 行的 `records` 非空、同一 `kind`/turn/request，逐条保留 `id`、sequence、`create_at`、tags、delta、refs、trace 和各自 `batch_end`。输出行只在 `step/text` 或 `step/thinking` 出现，`turn_id`/`request_id` 提到行级并从各 `record.event.data` 移除，读取时补回；`started_at` 是首事件的 `create_at`。合并不改变逻辑事件或原子批次，`last_sequence` 必须等于最后一条的序号。

写入时只有前一物理行与当前增量同 `kind`、同 `turn_id`/`request_id`，前一行编码长度小于 4096 字节，且当前事件的 `create_at - started_at < 1000` 毫秒，才向该行追加；否则新建 `output` 行。阈值检查发生在追加前，故追加后的行可以超过 4096 字节。读取现有输出行不依赖这些写入侧阈值，只校验行内事件与区间。

### 请求引用与 before 轨迹

仅 `context/prepare` 与 `model/call` 的 `data.request` 可带 `refs`；存储中的 `request.messages` 为 `[]`，`tools` 和 `system` 字段移除，其余请求字段保持原样。其他事件及非请求 before 草稿的 `refs` 为 `null`，字节与 v16 相同。v17 写入 `ListReferences`：

- `list` 是请求消息列表最后一个节点的地址，`length` 是消息条数；`list` 恰在 `length` 为 0 时为 `null`。
- 写入方取该记录所在分支逻辑历史中的上一个请求（分支自己在它之前的最后一个请求事件，否则依次是各祖先在切点及以前的最后一个），两者 `messages` 的最长公共前缀（先比指针，再用 `Message` 的 `==`）沿用上一个请求的节点，其余每条消息写一个新节点，父节点为它前一条消息的节点。没有上一个请求、上一个请求是 v16 行，或压缩之后前缀为空时，从头写一条新列表。节点以 `INSERT OR IGNORE` 写入。
- `tools`/`system` 缺席为 `null`；与上一个请求相等（先比指针，再比 `==`）时沿用其地址，否则写成 `kind=tools`/`system` 的内容行。
- `WireTrace.original` 和每个非空的 `WireStep.result` 中的请求按同一规则编码，以记录自身的事件请求为上一个请求；没有 before 轨迹时 `trace` 为 `null`。

v16 写入的 `References` 照常读取：`messages` 按顺序引用 `kind=message` 的内容行，即使为空也存在。两种形状按字段区分，各自拒绝对方的字段和未知字段。这些 `null` 属于存储封装，事件类型自己声明为可选的字段仍按原 serde 规则省略。字节只由分支的逻辑历史和事件决定，与写入轮次、写入方缓存或重启无关。

### 可丢弃恢复快照

Completed compaction facts contain source through, `retain_from` (the Timeline
event id of the first event kept verbatim), `replacement`, `strategy`, baseline
and diagnostics, not snapshots. Disposable snapshots commit separately without
affecting fact durability. `RecoverySnapshot={sequence,event_id,interpreter,state}`;
the default `dim/context-projection/v4` interpreter encodes `{payload,sha256}`,
with the complete ProjectionCheckpoint JSON string below as payload. SQLite
continues to treat state as opaque TEXT; the snapshot change needed no new database format.

摘要算法为 SHA-256：先写入 UTF-8 `dim/recovery-snapshot/v1` 和一个 NUL，再写入 sequence 的 8 字节大端编码，随后依次写入 event_id、interpreter、payload；每个字符串先写入 UTF-8 字节长度的 8 字节大端编码，再写入原始字节。sha256 保存小写十六进制。解释器在解码 payload 前校验摘要，覆盖 resume、全部状态及锚点身份。旧 v1 无校验快照、按条目 ID 记录切点的 v2 快照，以及 resume 不带事件 ID 的 v3 快照直接绕过，不通过补算摘要升级；v4 不接受裸检查点或缺失摘要。历史事实及摘要文本不受影响。

`HistoryRenderer` 同时拥有 `encode_checkpoint` 和 `decode_checkpoint`，自定义格式必须成对提供并管理自己的 interpreter 标识。完整性校验检测编码后发生的损坏，不是认证或生产者语义证明：可信 builder 必须生成与完整事实重放等价的状态，并通过差分测试验证。已知 builder 语义缺陷需要修复并更换解释器版本，不能给错误状态重算摘要。有效快照仍只读取恢复位置以后的事实，不在每次读取时重复完整重放。

候选查询只选择相同 interpreter 且 sequence 不大于固定 through 的最新快照，并连接事件锚点验证 ID 与序号，不读取事件种类或内容；孤立或元数据不匹配的快照被忽略。锚点能否承载状态由快照所有者的解码回调判断：默认解释器只接受自己完成的 compaction 事实，且其 `retain_from` 与状态一致、through 早于锚点（#130）。Session 恢复不校验可选 state；缺失、不兼容、损坏、越界或锚点不符的快照回退事实重放，replacement 仍保留。运行时在裁剪前按 `resume.event_id` 定位该事件并校验 resume 属于固定祖先链和 source through；按 ID 查不到时使用记录的位置。投影的所有者默认从祖先链起点完整折叠（`history::replay_context`），默认能力用 `history::read_context` 先取 `Timeline::checkpoint` 选出的快照再从 `resume` 起折叠。事实或必需引用损坏仍失败并保留原字节。回滚先删除 snapshots 再删除 events。

```text
ProjectionCheckpoint = {
  resume: { branch_id: string, sequence: integer, event_id: string },
  state: {
    retain_from: string,
    prompts: { entries: PromptEntry[], revision: PromptReference | null,
      system_revision: PromptReference | null, system: string,
      turns: string[], tasks: PromptScope[] },
    delivered: ContextBaselineEntry[],
    inputs: [string, InputMessageData][],
    configuration: ContextConfiguredData | null,
    output: MessagePart[], open_tools: string[], output_open: boolean
  }
}
```

上述检查点中的叶子类型仍是事件类型的原 JSON 形状，不另做存储转换：`PromptReference={timeline,revision}`；`PromptScope={kind:"branch",timeline}` 或 `{kind:"turn"|"task",timeline,id}`；`PromptEntry={source,version,body,scope,delivery,order}`，其中 `delivery` 为 `system`/`context`/`state`。

`ContextBaselineEntry={key,sourceEvent,message}`；`InputMessageData={submission_id?,user?,blocks?,mode?}`，`user.content` 是文本/图片块列表。`blocks` 项只有原文一种：`{type:"raw",text,key?,retire?}`，`text` 是能力写入的最终文本，`key` 表示仍然有效的内容，`retire:true` 结束该 key；语义见[架构约定](../../architecture/README.md)。

`ContextConfiguredData={timeline,revision,previous?,prompt?,settings,system?,system_source?}`，`settings={providerId,modelId,tools?,toolSources?,config?,metadata?}`；`toolSources` 是 `{key,version}[]`，为空时省略。`Message` 是 `{role,content}`，`content` 是文本或 `MessagePart[]`；`MessagePart` 保持 `text`、`image`、`tool_use`、`tool_result`、`thinking` 的标记联合；text / thinking / tool_use 可保存闭合 `providerMetadata`，原生签名、Responses item 身份及 summary 分组见 [模型宿主](../../../crates/dim-agent-aimux/README.md)。内部 `step/model-content` 保存成功输出的完整有序 parts；增量事件仍服务流式显示。

### 读取校验

读取事件时解析所选 `Row` 和运行时事件。需要请求正文的读取按 `(session_id, hash)` 加载其引用内容，复核 hash、kind、列表长度和父节点，再重建 `Arc<ModelRequest>`；同一次读取按既有预算复用解码结果。列表每加载至多256个节点释放一次连接。缺失或损坏内容明确报错并保留原字节，不把损坏请求填为空请求。历史分页、摘要、上下文投影等不使用请求正文的读取不加载请求内容，见[请求增量存储§7.5](request-delta-storage.md#75-reads-that-do-not-use-request-bodies)。Session恢复只读取配置和分支目录，不解码全部事件、不全查内容引用或派生索引、不维护校验缓存，见[恢复读取边界](restore-validation.md)。导出在它原本的事件遍历中加载所需正文，不预扫描；错误返回且不输出终止记录，CLI删除失败文件，stdout可留下不完整流。

## 格式版本

`agent_store_format.version` 同时标识 SQL 结构、持久化 JSON 和内容寻址规则。已发布格式发生不兼容变化时，必须定义新版本及读取或转换路径。保持版本的条件是旧读取器能正确解释新值、旧写入器不会重写丢失新值，且存储不变量不变。纯运行时重构不改变格式版本。

首版定稿前的开发库兼容范围由上层持久化设计明确，不预设下一版本或迁移字段；S5a 的 Session 文件、S5b 的标题元数据、#193 的请求列表、#2 的扩展事件、#364 的挂载列表、#427 的分支结束事件、#527 的工具效果和清单条目、#580 的工具 schema、#556 的目录投影、#555 的 System 来源与 #596 的冗余索引移除各自的格式决定见下文。

### #1002 格式决定

[#1002](https://github.com/arcships/dim/issues/1002)（[ADR 020](../../adr/020-timeline-read-convergence.md) D5）的格式决定：**不改格式**，v33 不变，不迁移；只升检查点解释器，并改 `ProjectionCheckpoint.resume` 的形状。

- **只改可丢弃快照的 payload**：`resume` 从 `{branch_id, sequence}` 变为 `{branch_id, sequence, event_id}`，`event_id` 是该 resume 点的 Timeline 事件 ID。读取按 `event_id`（`from_event`）定位该事件并从此处续读祖先链，记录的位置只在按 ID 查不到该事件时使用。SQL 结构、`timeline_snapshots` 的列、其余持久化 JSON、内容寻址与事实字节都不变。
- **不改版本**：`timeline_snapshots.state` 对 SQLite 是不透明 `TEXT`，快照是可丢弃的派生缓存，标识它编码的是 `interpreter` 而不是存储版本。旧读取器不解析 `state`，旧写入器不重写已有快照行，存储不变量不变，因此保持版本的条件都成立。
- **解释器升版**：默认解释器从 `dim/context-projection/v3` 升到 `dim/context-projection/v4`。v3 的 payload 没有 `event_id`，且摘要覆盖 payload，不能给旧行重算摘要。
- **旧快照**：v3 及更早的快照不被选中（候选查询按 `interpreter` 过滤）、不转换、不回填，随 Session 删除或回滚消失；读取回退完整事实重放，completed 事实与 replacement 不受影响。
- **resume 落在合并输出行**：没有保留条目时 `projection_checkpoint` 回退到上下文的最后一个事件，其种类不受限，可能是位于合并 `output` 行内的 `step/text` 或 `step/thinking`，这些行的 `timeline_events.event_id` 为 NULL。读取按 ID 经查询查找该事件（ID 选择会检查合并行内的记录），查不到时用记录的位置；两条路径都从同一事件开始重放，结果与完整重放一致。
- **验证**：`storage_flow.rs` 的 `a_resume_inside_a_merged_output_row_restores_the_full_context` 在一次性库中让两个增量合并成一行，核对 resume 落在该行的第二个事件上、恢复的上下文等于完整重放；`a_v3_snapshot_is_bypassed_and_left_as_stored` 核对 v3 快照不被选中且行字节不变；`timeline_flow.rs` 的 `a_checkpoint_resumes_at_the_event_its_id_names` 与 `a_resume_id_the_store_does_not_hold_keeps_the_recorded_position` 核对按 ID 续读与按位置兜底。既有快照命中、缺失、损坏、摘要不符、越界回退事实重放的流程测试不变并通过。

### #1099 v34 格式决定

[客户端 Session 目录](session-catalog.md#32-order-cursor-and-paging)（#1099）的格式决定：升级到 **v34**，从 v33 原地升级。这是一次只改数据的一步：不加表、列、索引，也不改持久化 JSON。

- **规则**：`session_catalog.updated_at_ms` 是该 Session `main` 上最新一条活动事件的 `create_at`，没有活动事件时取 `agent_sessions.created_at_ms`。活动是 Session 自己的活动：带用户部分的 `input/message`、`turn/end`、`branch/create`（父分支记下分支 prompt）与 `session/forked`；其余一切都是家事事件——宿主注入的 `input/message`（没有用户部分）、`branch/resume`、`branch/start`、`branch/ended`、`branch/end`、`branch/create-failed`、每一类 `context`/`prompt`/`model`/`step`/`tool`/`approval`/`stop`/`exec`/`permission`/`collaboration` 事件，以及扩展事件——不移动这一行。判断只此一处，SQL 过滤与各写入方都从同一份种类表构造（[目录 §3.2](session-catalog.md#32-order-cursor-and-paging)）。
- **为什么修**：v23 起这列跟着"最后一次提交的最后一个事件"走。`session.attach`/恢复写入的 `branch/resume`、分支生命周期事件、配置与压缩批次，都会在没有用户活动时把 Session 抬到最前；恢复选择面板于是把"只是被恢复过"的 Session 排在用户刚说过话的 Session 前面。活动事件才是列表要回答的次序。
- **读取或转换路径**：在现有的 `IMMEDIATE` 启动准入事务内，对结构恰为 v33、标记相符的库执行 `catalog::backfill`：每个 Session 已有的行保留（v23 起创建即写行），`refresh` 按本条规则重写 `updated_at_ms`，行缺失的 Session 以创建时间补写；不触碰任何其他行。更早的受支持格式先执行既有各步，再执行本步。失败回滚，整库不变。
- **写入方**：写 `main` 批次的同一事务只在批次含活动事件时更新这一行，写该批次最新一条活动事件的 `create_at`；只含家事事件的批次不碰这一行（热路径不加读）。`session.fork` 在分叉事务内写 `session/forked`（活动），`refresh` 与回填同一规则，`0` 处分叉取创建时间。没有原生存储的守护进程经 `AgentIndex::session_summary` 用同一谓词对已装载 Session 求最新活动事件，两条列表不会给出不同答案（[目录 §3.3](session-catalog.md#33-sessionsummaries)）。
- **回填代价**：`refresh` 为每个 Session 在 `main` 上按 `(session_id, branch_id, sequence)` 主键倒序读，直到第一条活动事件，候选的 `input/message` 体上各做一次 `json_extract`——不是旧的"读最后一行"。预览仍未决定的少数 Session 再走 v23 的预览窗口。一次性库上（2,300 个 Session，各 5 条活动与 5 条家事事件，约 2% 已有预览）整步约 0.25 秒，每个 Session 的行都重算；只在升级的那次启动支付。
- **单向**：v33 守护进程以现有"unsupported SQLite schema … preserved"错误拒绝 v34 库；不降级。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v33 to v34; earlier dimcode versions cannot open it.`
- **验证**：`storage_flow.rs` 的一次性 v33 库核对结构、标记与其余行逐行不变，`updated_at_ms` 被重写为最新活动事件的 `create_at`（尾部的 `branch/resume` 不再抬高它），没有活动事件的 Session 落在创建时间（`a_v33_store_recomputes_the_catalog_activity_time_on_upgrade`）；`upgrades_in_place_once` 各版本仍逐行核对。`transport_flow` 核对用户消息、完成的 turn 与非零处分叉都移动这一行，恢复（`branch/resume`）不移动，`session.summaries` 与 `session.catalog` 就同一值作答（`catalog_activity_time_follows_user_activity_not_housekeeping`、`an_unload_writes_nothing_and_the_restore_only_resumes` 与目录流程测试）。

### S5a v15 格式决定

[路线图](../roadmap.md) S5a（[Session 文件](session-files.md#8-storage-format)，已实现）的格式决定：升级到 **v15**，不是 v13 内的增量，也不用 14。

- **新增两张表与一个索引**：`session_files(file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)` 与 `session_file_owners(session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))`，索引 `session_file_owners_by_file(file_id)`。`agent_store_format` 的约束改为 `CHECK (version = 15)`。
  - `file_id` 是小写带连字符的 UUID v4，也是目录名。
  - `node` 是写入时 Session 的 `resourceNode`，NULL 表示控制节点。
  - `writer` 在写入未完成时是 SessionStore 代次 ID，提交或放弃后为 NULL。
  - 删除 Session 时，先删其 `session_file_owners` 行，再删 `agent_sessions`。
  - S5c 的分叉与删除只增删 `session_file_owners` 行，清扫只读 `session_files`，不为 Session 文件再改格式。
- **不能留在 v13**：启动要求精确的 v13 SQL 结构，v13 守护进程会拒绝多出表的库。旧守护进程无法打开新库，不满足本节"旧读取器能正确解释新值"的保持条件。C2 和 S8a 留在 v13，是因为它们只在现有表内增加 JSON 字段或行。
- **不用 14**：分支专用的 v14（`user_configuration`）曾经提交（e41ceaf），后被移除（fb3748e）；带 14 标记的开发库可能存在，本文承诺永不解释它们。复用 14 就是给不兼容的已提交格式复用版本号。
- **读取或转换路径**：在现有的启动准入事务（`IMMEDIATE`，写入型 PRAGMA 之前）内处理：
  - 空库直接建 v15。
  - 结构恰为 v13 且标记为 13 的库，在同一事务内重建 `agent_store_format`、写入 15，并建新表和索引。不读取、不改写任何已有行；失败则回滚，原 v13 库保持不变。
  - 恰为 v15 的库直接打开。
  - 其他库（更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **单向**：v13 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。这是增量改动，代价可以接受；另一种做法是拒绝所有 v13 库，会丢失用户的 Session。之后的格式各自追加从前一版本出发的一步。
- **升级日志**：升级事务提交后，打开存储的调用方得知已升级，守护进程向 stderr 写一行 `Upgraded native store <path> from format v13 to v15; earlier dimcode versions cannot open it.`。只有执行升级的那次启动写这一行。它直接写 stderr，不走 `tracing`，因为守护进程默认的日志过滤只保留错误。CLI 启动的守护进程的 stderr 就是数据库目录下私有的 `daemon.log`，回退旧版本的用户能在旧守护进程的拒绝信息旁找到原因。
- **验证**：测试用 v13 结构的冻结副本和下面的完整记录示例构建一次性 v13 库，升级后核对结构恰为 v15、业务行逐行不变，从不打开用户已有的库（`storage_flow.rs` 的 `a_v13_store_upgrades_in_place_once`）。

### S5c 格式决定

[路线图](../roadmap.md) S5c（[Session 删除、清理与分叉](session-deletion-and-fork.md#8-storage-format)，已实现）的格式决定：**S5c 本身不改格式**。#222 在 v16 下让分叉在复制的行之后再写一行 `session/forked`（数据为空），没有升版本号；[v17](#193-v17-格式决定) 原样保留这一行。

- **删除**：在一个事务内按 `session_id` 删除该 Session 在各表中的全部行（顺序为 `session_file_owners`、`timeline_snapshots`、`timeline_events`、`timeline_branches`、`timeline_content`、`session_owners`、`agent_sessions`），与创建回滚是同一操作。不写墓碑行：命令输出目录在提交之前移除，Session 文件由 S5a 的行和清扫处理。
- **分叉**：在一个事务内为新 Session 插入现有表的行。`agent_sessions.config` 按存储的文本原样复制；`main` 截至分叉点的事件行、被复制事件引用的内容行、锚定在被复制事件上的快照，以及 S5a 的 `session_file_owners` 行，都按原字节复制。截至分叉点由 `main` 创建的分支只复制目录行（保留存储的 `info`，头部为 0，不含事件），其下属分支不复制；没有事件的子分支目录是存储已经接受的状态，即子分支尚未开始的创建。持久化 JSON 类型不变；随后在 `at + 1` 写入一行 `session/forked`，`main` 的头部为 `at + 1`（在 `0` 处分叉不写，头部为 `0`）。
- **事件 ID**：事件 ID 在所属 Session 内唯一；分叉在另一个 Session 中重复源 Session 的 ID。现有校验和 `timeline_events_by_id` 索引本就按 Session 划分，这是对不变量的说明，不是格式变化。
- **不记来源**：不加记录分叉来源的列，`session/forked` 不带数据；目前没有任何决定使用它，[Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields) 规定 Session 来源在有使用方时连同其格式决定一起加入。
- **旧版本（开发阶段兼容范围，随 #222 决定）**：`EventKind` 是封闭列表，在 `session/forked` 出现之前构建的守护进程无法解码这一行，因而无法恢复新版守护进程分叉出的 Session；首次发布前原生格式不承诺兼容，所以不升版本号、不加回退解码或迁移。没有这一行的 Session（从未分叉、或在 `0` 处分叉）照常读取；已删除的 Session 当作从未存在。v17 起，在这一行出现之前构建的守护进程都以现有错误拒绝 v17 库，能打开 v17 库的守护进程都能解码它，上述差异只留在 v16 库中。

### S5b v16 格式决定

[路线图](../roadmap.md) S5b（[标题元数据](session-title-metadata.md#11-storage-format)）的格式决定：升级到 **v16**，从 v15 原地升级。

- **新增一张表**：`session_metadata(session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), revision INTEGER NOT NULL CHECK (revision > 0), title TEXT, title_source TEXT CHECK (title_source IN ('auto', 'manual')), CHECK ((title IS NULL) = (title_source IS NULL)))`。`agent_store_format` 的约束改为 `CHECK (version = 16)`。
  - 每个 Session 至多一行，没有行表示修订号 0、没有标题。
  - `revision` 是该 Session 已提交 `setTitle` 的次数；清除标题时保留该行，`title` 与 `title_source` 置为 NULL，修订号不会回退。
  - 删除 Session 时，在 S5c 的同一删除事务里、在 `agent_sessions` 之前删除这一行；分叉不复制它。
- **不能留在 v15**：启动要求精确的 SQL 结构，v15 守护进程会拒绝多出表的库，不满足"旧读取器能正确解释新值"。标题不能放进已有的行：`agent_sessions.config` 是分叉整体复制的不可变创建配置，`settings_documents` 属于 Settings；给 `agent_sessions` 加列同样改变存储的 SQL。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v16。
  - 结构恰为 v15 且标记为 15 的库，在同一事务内重建 `agent_store_format`、写入 16，并建 `session_metadata`。不读取、不改写任何已有行；失败则回滚，原 v15 库保持不变。
  - 结构恰为 v13 且标记为 13 的库，在同一事务内先走 S5a 的一步，再走本步，直接成为 v16。
  - 恰为 v16 的库直接打开。
  - 其他库（标记不符的 v15 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **单向**：不做备份，与 S5a 相同（开发阶段的决定）。v15 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。升级日志沿用 S5a 的那一行，写明原版本：`Upgraded native store <path> from format v15 to v16; earlier dimcode versions cannot open it.`
- **验证**：测试用 v15 结构的冻结副本和完整记录示例构建一次性 v15 库，升级后核对结构恰为 v16、业务行逐行不变、新表为空；v13 库一次升级到 v16；标记不符或升级到一半的 v15、v16 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once` 与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #193 v17 格式决定

[请求增量存储](request-delta-storage.md#11-format-v17)（#193）的格式决定：升级到 **v17**，从 v16 原地升级。

- **只改持久化 JSON**：消息以列表节点存入 `timeline_content`（新种类 `node`，正文 `{parent, message}`，地址照旧为 `SHA-256(kind NUL body)`），消息前缀相同的请求共用节点。`context/prepare` 与 `model/call` 记录及其请求草稿以 `ListReferences {list, length, tools, system}` 引用最后一个节点、列表长度与 v16 的 `tools`、`system` 内容，不再为每条消息保存一个引用；其他记录的字节不变（见上文[请求引用与 before 轨迹](#请求引用与-before-轨迹)）。请求只由自己的行与其到达的内容行还原，不经过其他事件行，所以检查点读取仍不读取恢复位置之前的请求行；遍历列表每加载至多 256 个节点释放一次连接。
- **不能留在 v16**：v16 的 `References` 要求 `messages` 并拒绝 `list`、`length`，v16 读取器也不认识 `node` 内容，不满足"旧读取器能正确解释新值"。
- **SQL 不变**：表、列与索引不变（`timeline_content.kind` 本无 SQL 枚举 CHECK），`agent_store_format` 的约束改为 `CHECK (version = 17)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v17。
  - 结构恰为 v16 且标记为 16 的库，在同一事务内只重建 `agent_store_format` 并写入 17，不读取、不改写任何业务行；失败则回滚，原 v16 库保持不变。
  - 结构恰为 v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v17。
  - 恰为 v17 的库直接打开。
  - 其他库（标记不符的 v16 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **混存**：v16 行保持原样且永久可读，不做回填。升级后一个分支写下的第一条请求行，其上一个请求是没有节点的 v16 行，于是为每条消息写一个节点：每个分支一次完整列表，之后的请求共用它。两种行可混存于同一分支与同一库，分叉照原样复制。
- **分叉**：除被复制事件直接引用的内容行外，还复制每个被引用列表从最后一个节点到首个节点途经的全部节点，在分叉事务内用一条对 `(session_id, hash)` 的递归查询完成；切点之后的请求新增、且没有更早请求共用的节点不复制。分叉随后写下的 `session/forked` 不带请求、不引用内容，闭包不因它复制任何行。
- **单向**：不做备份，与 S5a、S5b 相同。v16 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库：其启动门禁比较精确的 SQL 结构，`agent_store_format` 的 `CHECK (version = 17)` 已不同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v16 to v17; earlier dimcode versions cannot open it.`
- **验证**：测试用 v16 结构的冻结副本和完整记录示例构建一次性 v16 库，升级后核对结构恰为 v17、业务行逐行不变，再续写一条请求行，核对它写出完整列表、旧行仍可读；v15、v13 库一次升级到 v17；标记不符或升级到一半的 v16、v17 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v16_store_upgrades_in_place_once`、`a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once` 与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #2 v18 格式决定

[扩展事件](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)（#2）的格式决定：升级到 **v18**，从 v17 原地升级。

- **只增一种行**：核心未定义领域的事件以 `EventKind::Extension` 写入普通 `event` 行，`kind` 为 `<type>/<name>`，`data` 原样保存（见上文[事件封装](#事件封装)）。它不进输出行、不带请求引用；行结构、内容寻址和其他记录的字节不变。
- **不能留在 v17**：v17 读取器的 `EventKind` 是封闭列表，解码不了扩展事件行，不满足"旧读取器能正确解释新值"。#193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行（见上文 [S5c 决定](#s5c-格式决定)）；留在 v17 就打破这一点，为不兼容的已提交格式沿用了版本号。此后的读取器按领域解码：核心保留领域只按其类型解码，其他领域都读作扩展事件，所以新的扩展领域不再改变格式；新增核心事件仍需各自的格式决定。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 18)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v18。
  - 结构恰为 v17 且标记为 17 的库，在同一事务内只重建 `agent_store_format` 并写入 18，不读取、不改写任何业务行；失败则回滚，原 v17 库保持不变。
  - 结构恰为 v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v18。
  - 恰为 v18 的库直接打开。
  - 其他库（标记不符的 v17 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v17 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v18 库：其启动门禁比较精确的 SQL 结构，`agent_store_format` 的 `CHECK (version = 18)` 已不同。留在 v17 时，它会打开该库，到恢复含扩展事件的 Session 时才按[格式不可读](../../architecture/rust-agent/timeline-storage.md#6-故障)拒绝该 Session 并保留数据；v18 让这一失败在启动时出现，并由版本号说明原因。
- **单向**：不做备份，与 S5a、S5b、#193 相同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v17 to v18; earlier dimcode versions cannot open it.`
- **验证**：测试用 v17 结构和完整记录示例构建一次性 v17 库，升级后核对结构恰为 v18、标记为 18、业务行逐行不变；v16、v15、v13 库一次升级到 v18；标记不符的 v17、v18 结构被拒绝并保留；扩展事件各占一行、保存 `data` 的原字节、两侧的输出增量不跨它合并，经 Session 恢复原样读出；从不打开用户已有的库（`storage_flow.rs` 的 `a_v17_store_upgrades_in_place_once`、`a_v16_store_upgrades_in_place_once`、`a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once`、`unsupported_legacy_or_changed_schema_is_rejected_without_migration` 与 `extension_events_keep_their_own_rows_and_restore_unchanged`）。

### #364 v19 格式决定

[挂载列表](../contracts/session-access.md#a3-sessionconfig-fields)（#364）的格式决定：升级到 **v19**，从 v18 原地升级。

- **只改一个字段的写法**：`agent_sessions.config`（`SessionConfig`）与 `agents.profile`（`AgentProfile`）的 `tools` 成为可选。有挂载列表时照旧写成字符串数组；没有挂载列表时省略该字段，表示挂载进程提供的全部工具。已存储的记录都带 `tools`，读作有挂载列表，含义不变；例外是默认 Agent：它由进程按默认档案建立，不能由客户端编辑，存储的档案只是建立它的那个版本的进程默认值，因此启动时可读的默认 Agent 一律采用当前进程默认档案并改写该行（v19 内的同格式写入，不升级格式），否则 #364 之前建立的库会一直冻结在旧的挂载列表上，之后新增的工具（如 `ask_user`）永远不会挂载；读不出或不合法的默认 Agent 仍保持不可用，不被替换。表、列、索引、内容寻址和其他记录的字节不变。
- **不能留在 v18**：v18 读取器要求 `tools` 存在，读不了省略它的记录，不满足"旧读取器能正确解释新值"；也不能改写成 `null` 或空数组，那会把"全部挂载"读成"不挂载"。
- **SQL 不变**：`agent_store_format` 的约束改为 `CHECK (version = 19)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v19。
  - 结构恰为 v18 且标记为 18 的库，在同一事务内只重建 `agent_store_format` 并写入 19，不读取、不改写任何业务行；失败则回滚，原 v18 库保持不变。
  - 结构恰为 v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v19。
  - 恰为 v19 的库直接打开。
  - 其他库照旧拒绝并保留。
- **旧版本的失败方式**：v18 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v19 库，不会到恢复某个 Session 或读取某个 Agent 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用同一行：`Upgraded native store <path> from format v18 to v19; earlier dimcode versions cannot open it.`
- **验证**：测试用 v18 结构和完整记录示例构建一次性 v18 库，升级后核对结构恰为 v19、标记为 19、业务行逐行不变；v17、v16、v15、v13 库一次升级到 v19；标记不符的 v18、v19 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v18_store_upgrades_in_place_once` 及其他各步的同名测试与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #32 格式决定

[#32](https://github.com/arcships/dim/issues/32) 的格式决定：**不改格式**，v18 不变。`submission_id` 留在 `input/message` 的 `data` 中，不另建提交记录表（[sessions 服务 §6.3](sessions-service.md#where-the-submission-identity-lives)）。

- **已有库**：已写入的行原样有效，不迁移，不改写。
- **冻结样本**：`crates/dim-agent-types/tests/fixtures/` 不变。v1 wire 的 `input/message` 只有 `{text,mode?}`，没有该字段的视图。
- **以后加索引时**：若查找延迟需要索引，按 `timeline_tool_use_starts` 的方式从事件正文派生表达式索引，字段仍留在事件中。SQL 结构随之改变，届时单独做格式决定。

### #329 格式决定

[Provider 完整管理 #329](https://github.com/arcships/dim/issues/329) 的格式决定沿用当时当前的 **v19，不额外改变格式号或 SQL 结构**（#364 已决定 v18 → v19；其后 #427 升到 v20、#527 升到 v21、#556 升到 v23、#555 升到 v24，本决定随之保持）；具体合同见[Provider 管理 §6](provider-management.md#6-persistence-existing-sessions-and-migration)。
这是对上文保持版本条件中“旧写入器不会重写丢失新值”的明确例外，按首次发布前的开发阶段兼容范围接受。

- 只增加三个可选顶层 JSON 字段：静态记录的 `userModels: ModelEntry[]`、静态/托管记录的 `modelPreferences: Record<modelId, ModelPreferences>`，以及静态记录的 `endpointOverrides: Record<modelId, NewApiEndpoint>`。缺失表示无对应用户数据；默认模型与用户新增列表分开。
- 旧 v19 二进制忽略这些未知顶层字段，重写时会丢失；明确接受该降级损失，包括 fingerprint 改变时自动 `seed_bundled` 重写。不得只验证读取成功；实施须在一次性数据库中运行旧 v19 的实际读取和重写路径。
- 不扩展既有严格嵌套类型。`ModelPreferences` 拒绝未知字段，`NewApiEndpoint` 是闭合枚举；已支持新映射的旧二进制会拒绝后续新增的内部字段/枚举值，增加前须另作显式格式决定。
- 新版本 reseed 若因用户模型与新默认项重名，保留该记录已准入定义、fingerprint 和 version，继续处理其他记录，管理面沿既有逐项错误提示。用户删除冲突模型后，下次启动采用新定义；不以 Provider 激活失败阻断修复入口。其他非法记录和未知数据库格式沿原规则拒绝并保留。
- 验证在一次性 v19 库中运行 Provider 管理实现之前的实际 App Server 二进制：只读不改变自定义记录字节；旧 `provider.save` 重写丢失三个新增字段，旧启动 reseed 丢失内置记录的用户模型和偏好，库标记不因本决定改变。新增字段的缺失值、重启保留、用户模型冲突、删除和重新装载由 Provider 流程测试保护。

### #427 v20 格式决定

[Timeline 分支](../timeline-branches.md#16-format)（#427）的格式决定：升级到 **v20**，从 v19 原地升级。

- **状态**：v20 已随 #427 的实现生效；当前格式先后由 [#527 v21 格式决定](#527-v21-格式决定)、[#580 v22 格式决定](#580-v22-格式决定)、[#556 v23 格式决定](#556-v23-格式决定) 与 [#555 v24 格式决定](#555-v24-格式决定) 升到 v24；[#596 v25 格式决定](#596-v25-格式决定) 删除冗余索引后为 v25，[#522 v26 格式决定](#522-v26-格式决定) 改写 Provider 模型上限后为 v26，[#593 v27 格式决定](#593-v27-格式决定) 加两列后为 v27。
- **只增两种核心事件**：两者都以 `encoding: "event"` 写入普通 `event` 行，不进输出行、不带请求引用。行结构、内容寻址和其他记录的字节不变。
  - `branch/end` 写在要结束的分支上，`data` 为 `{source, ancestor?}`。`source` 是 `branch/create` 已在用的 `BranchSource`：`{kind: "tool", turn_id, request_id, call_id}` 或 `{kind: "host", operation_id}`。`ancestor` 是分支 ID，只在因祖先结束而结束时出现。
  - `branch/ended` 写在其父分支上，`data` 为 `{branch, endEvent, source, ancestor?}`。`endEvent` 是 `branch/end` 在结束分支上的序号（安全整数），`source` 与 `ancestor` 取自 `branch/end`（[Timeline 分支 §10](../timeline-branches.md#10-ending-a-line)）。
- **目录行不变**：`timeline_branches.info` 照旧是不含 `source` 的 JSON，类型是 `BranchRecord`（运行时视图 `TimelineInfo` 是它再加上 `source`）。运行时的 `TimelineInfo` 新增 `source`，但存储写目录行时不写它；恢复时由 TimelineTree 从父分支的 `branch/create` 补齐（[Timeline 分支 §5](../timeline-branches.md#5-parts)）。
- **不能留在 v19**：
  - v19 读取器对核心保留领域只按类型解码（见上文 [#2 v18 决定](#2-v18-格式决定)），解码不了 `branch/end` 与 `branch/ended` 行。
  - #193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行；留在 v19 就打破这一点，为不兼容的已提交格式沿用了版本号。
  - #2 的 v18 决定写明，新增核心事件需要各自的格式决定。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 20)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v20。
  - 结构恰为 v19 且标记为 19 的库，在同一事务内只重建 `agent_store_format` 并写入 20，不读取、不改写任何业务行；失败则回滚，原 v19 库保持不变。
  - 结构恰为 v18、v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v20。
  - 恰为 v20 的库直接打开。
  - 其他库（标记不符的 v19 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v19 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v20 库，因为其启动门禁比较精确的 SQL 结构，而 `agent_store_format` 的 `CHECK (version = 20)` 已不同。它不会等到恢复含这两种事件的 Session 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v19 to v20; earlier dimcode versions cannot open it.`
- **验证**：
  - 用 v19 结构和完整记录示例构建一次性 v19 库，升级后核对结构恰为 v20、标记为 20、业务行逐行不变。
  - v18、v17、v16、v15、v13 库一次升级到 v20。
  - 标记不符的 v19、v20 结构和升级到一半的库被拒绝并保留。
  - `branch/end` 与 `branch/ended` 各占一行，经 Session 恢复原样读出；已结束的分支恢复后不再打开。
  - 新子分支的目录行不含 `source`，与 v19 的写法逐字节相同。
  - 从不打开用户已有的库。

  对应 `storage_flow.rs` 的 `a_v19_store_upgrades_in_place_once` 及其他各步的同名测试、`unsupported_legacy_or_changed_schema_is_rejected_without_migration` 与 `branch_end_events_restore_unchanged_and_the_line_stays_ended`。

### #527 v21 格式决定

[工具效果](../contracts/session-boundary.md#a6-tool-effects)与[压缩清单](../plugins/ContextCompaction.md#510-class-3a-the-digest)（#527）的格式决定：升级到 **v21**，从 v20 原地升级。

- **状态**：v21 的字段合同保持；当前格式由 [#580 v22 格式决定](#580-v22-格式决定)、[#556 v23 格式决定](#556-v23-格式决定) 与 [#555 v24 格式决定](#555-v24-格式决定) 升到 v24；[#596 v25 格式决定](#596-v25-格式决定) 删除冗余索引后为 v25，[#522 v26 格式决定](#522-v26-格式决定) 改写 Provider 模型上限后为 v26，[#593 v27 格式决定](#593-v27-格式决定) 加两列后为 v27。
- **只增两处可选字段**：SQL 结构、表、列、索引、内容寻址与其他记录的字节都不变。
  - `ToolResult.effects`：`step/tool-result` 的 `result` 上一个可选的效果声明数组；不声明任何效果的工具不写它，此前写下的事件也都没有它。
  - 已完成 `context/compaction` 的 `digest_entries`：清单条目（按顺序）与 `dropped`；没有清单消息的压缩不写它。
- **不能留在 v20**：
  - `ToolResult` 与 `ContextCompactionResult` 都拒绝未知字段，v20 读取器解不了这两个值。
  - 自 #193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行；打开 Session 时还会解码每条已存事件（`validate_session`），所以留在 v20 会让含这两种值的 Session 打不开。
  - #2 的 v18 决定要求不兼容的已存值各自做格式决定。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 21)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v21。
  - 结构恰为 v20 且标记为 20 的库，在同一事务内只重建 `agent_store_format` 并写入 21，不读取、不改写任何业务行；失败则回滚，原 v20 库保持不变。
  - 结构恰为 v19、v18、v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v21。
  - 恰为 v21 的库直接打开。
  - 其他库（标记不符的结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v20 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v21 库，因为其启动门禁比较精确的 SQL 结构，而 `agent_store_format` 的 `CHECK (version = 21)` 已不同。它不会等到恢复含这两种值的 Session 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用同一行：`Upgraded native store <path> from format v20 to v21; earlier dimcode versions cannot open it.`
- **验证**：用 v20 结构和完整记录示例构建一次性 v20 库，升级后核对结构恰为 v21、标记为 21、业务行逐行不变；v19、v18、v17、v16、v15、v13 库一次升级到 v21；标记不符的 v20、v21 结构和升级到一半的库被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v20_store_upgrades_in_place_once` 及其他各步的同名测试、`unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #580 v22 格式决定

[工具声明](../contracts/session-boundary.md#a5-tool-contribution)（#580）的格式决定：升级到 **v22**，从 v21 原地升级。

- **持久化值**：`JsonSchema.type` 可以省略，组合关键字（例如 `oneOf`、`const`）原样保存；显式 `type: null` 仍非法。该类型同时用于 `context/configured.settings.tools` 和模型请求在 `timeline_content` 中的 `tools` 内容行。
- **不能留在 v21**：旧读取器要求 schema 节点带 `type`，无法恢复包含无类型组合节点的 Session；旧格式门禁必须在启动时拒绝新库，不能等到 Session 恢复时才失败。
- **升级路径**：沿用 `IMMEDIATE` 启动准入事务，只将 `agent_store_format` 的约束和唯一标记改为 22，不读取或改写任何业务行、内容地址或快照。精确匹配且标记相符的 v21 库原地升级；v20、v19、v18、v17、v16、v15、v13 库先走既有步骤。空库创建 v22，v22 直接打开，未知或标记不符的库拒绝并保留。失败回滚。
- **单向**：v21 daemon 按现有精确 SQL 结构检查拒绝 `CHECK (version = 22)`，不会尝试解码新值；不做降级或备份。升级提交后沿用一次性 stderr 日志：`Upgraded native store <path> from format v21 to v22; earlier dimcode versions cannot open it.`
- **验证**：`storage_flow.rs` 的 `a_v21_store_upgrades_in_place_once_and_restores_typeless_tool_schemas` 使用一次性 v21 库，核对所有原有业务行不变、新结构和标记、再次打开不再升级，并在采用工具配置和模型请求两条路径写入无 `type` 的组合 schema，关闭后恢复 Session，逐值核对。既有各版本升级与非法结构保留测试覆盖准入路径；冻结 wire fixtures 不变。

### #911 v33 格式决定

[Worktree access](../worktree-access.md#4-持久使用关联与清理边界) 的关联存储使用 **v33**，
从精确匹配且标记相符的 v32 原地升级。若合并前其他格式变更占用该编号，使用主线的下一个编号。

- **结构**：新增 `worktree_directories(repository TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (repository, root))`
  和 `session_worktrees(session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), repository TEXT NOT NULL,
  root TEXT NOT NULL, subtree TEXT NOT NULL, PRIMARY KEY (session_id, repository, root),
  FOREIGN KEY (repository, root) REFERENCES worktree_directories(repository, root))`。
  `session_worktrees_by_directory(repository, root, session_id)` 支持查询某目录的所有 Session 关联。
- **升级**：现有启动事务内创建两张空表及索引，重建标记表并写入 33；不扫描 Git、不回填关联、
  不修改其他业务行。更早受支持的格式先走已有步骤。任一步失败回滚。
- **生命周期**：关联与目录原子写入；Session 删除和创建回滚只移除关联，保留目录；fork 不复制关联。
  完整调用与错误契约见 [SessionStore](session-deletion-and-fork.md#17-worktree-usage-records)。
- **单向**：v32 守护进程拒绝 v33 库；不降级。目录记录和关联都不是授权、创建者证据或删除许可。
- **不兼容结构**：标记为 v32、包含 worktree 表但仍使用两个旧上下文读取索引的结构不是准入的 v32；精确结构门禁拒绝并保留它，不根据相同标记猜测转换。
- **验证**：一次性 v32 库升级后结构和标记精确匹配 v33，已有行保持不变，两张新表为空；
  重开不重复升级。公开存储流程覆盖去重、原子失败、多 Session、fork、删除和迟到写入。

### #605 v32 格式决定

[#605](https://github.com/arcships/dim/issues/605) 的存储格式决定：升级到 **v32**，从 v31 原地升级。v31 为两个上下文读取开关各建一个部分索引，键都是 `(session_id, branch_id, sequence)`；同一行两个开关都为 1 时，一次插入维护两个键相同的索引，WAL 多写一页。

- **结构变化**：删除 `timeline_events_render`、`timeline_events_state`，新增 `timeline_events_context (session_id, branch_id, sequence) WHERE context_render = 1 OR context_state = 1`。列、其他索引、持久化 JSON、内容寻址与事件引用均不变。上下文读取的条件本就是 `context_render=1 OR context_state=1`，新索引与它逐字对应；`EXPLAIN QUERY PLAN` 仍按 `(session_id, branch_id, sequence)` 定位并按 `sequence` 顺序返回，不需要临时 B 树。
- **升级**：沿用 `IMMEDIATE` 启动准入事务。空库直接建 v32。精确匹配且标记相符的 v31 库删除 `timeline_events_render`、`timeline_events_state`，创建 `timeline_events_context` 并重建标记表。v30 及更早的受支持格式先执行已有步骤：v30 步骤只添加两列、按 key 回填两个开关并创建 `timeline_events_positions`，不再创建两个旧索引；本步只创建合并索引，因此不存在“先建旧索引再删”的中间状态。失败回滚，旧业务行与开关值不变。
- **单向**：v31 守护进程拒绝 v32 库；不降级。准入仍要求精确 SQL 结构及对应标记。
- **测量**：`timeline_acceptance rounds 0 200 64 consumed`（64 个 Session 每轮各追加一条约 230 字节的 `input/consumed` 行，两个开关都为 1，逐条独立成行）在 v31 与 v32 上交替运行三次，每事件 WAL 中位数分别约 16.0 KB 与 12.4 KB，约少 23%（v32 的 12.3–12.6 KB 对 v31 的 15.7–16.2 KB）。`turn` 事件只置 `context_state`，只用一个部分索引，不受影响。
- **验证**：`storage_flow.rs` 的 `a_v31_store_upgrades_in_place_once` 用一次性 v31 库核对结构、标记、业务行与开关值，重复打开不再升级；v13 起的各版一次升级到 v32。`timeline_acceptance` 报告改动前后的每事件 WAL 字节，结果写在 PR。

### #708 v31 格式决定

[#708](https://github.com/arcships/dim/issues/708) 的读取边界：`timeline_events` 增加两个 0/1 冗余开关，从 v30 原地升级。

- **新增结构**：`context_render`、`context_state` 两列（`INTEGER NOT NULL DEFAULT 0`）和 `timeline_events_render`、`timeline_events_state`、`timeline_events_positions` 三个部分索引。
- **填充**：核心事件的两列是其 key 的纯函数（`dim_agent_types::context_flags`）；扩展事件的两列取事件自己的值——信封多两个可选布尔 `context_render`、`context_state`，缺省 false，为 false 时不写，由写事件的插件给出，核心不推算。CORE 事件的 wire 结构不变，扩展事件的 payload 契约不变。
- **回填**：升级事务内加列后按 `CONTEXT_RENDER_KEYS`、`CONTEXT_STATE_KEYS` 填核心事件；已有的扩展事件没有这两个字段，保持 0。提交前库不可用，提交后整体生效。写入、fork、离线导入都按事件自己的值填这两列。
- **升级**：空库创建 v31；精确匹配且标记相符的 v30 库在现有启动准入事务内执行上述步骤，重建标记表并写入 31。更早的受支持格式先执行已有步骤。失败回滚，旧业务行不变。
- **单向**：v30 守护进程拒绝 v31 库；不降级。准入仍要求精确 SQL 结构及对应标记。
- **验证**：`storage_flow.rs` 的 v13 升级测试抽查 `step/text` 为 `(1,1)`、`model/call` 为 `(0,0)`；`context_flow` 的流式与全量渲染差分测试核对开关筛选后的条目和状态逐条一致。

### #557 v30 格式决定

[Session 分类](session-classification.md)（#557）的格式决定：升级到 **v30**，从 v29 原地升级。若在实现之前另有格式变更先合入，实现取当时的下一个编号，本节其余内容不随编号改变。

- **只加一列**：`agent_sessions.origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown'))`。其他表、列、持久化 JSON、内容寻址与既有索引不变，不新增索引。
  - 它是 Session 的创建来源，创建事务写入一次后不再改变，取值与写入方见 [Session 分类 §3.1–§3.2](session-classification.md#32-which-creation-entry-writes-which-value)。默认值只为让 `ALTER TABLE` 能加非空列；每个写入方都显式列出该列，读到 `unknown` 是"本库没有记录来源"这一事实，不是占位。
  - `CHECK` 使新增取值成为一次格式变更：旧读取器不会遇到读不懂的值，拒绝发生在启动而不在读某一行时。
  - 子 Session 的父仍是 v27 的 `agent_sessions.leader_session_id`，本步不重复记录。
- **为什么必须落盘**：ACP 与 Dim 自己创建的 Session 目前没有任何持久来源标记。`clientKind` 是连接属性，不是 Session 的属性；`interactive` 是运行语义，客户端也可以创建非交互 Session；`collaboration_role` 是成员的运行语义，把导入的子 Session 设成 Member 会凭空造出一个协作关系。三者都不能当分类标签。迁移还需要写入映射后的来源与父，因此这个值必须在库里。
- **为什么不放别处**：放 `SessionConfig` 会让一个插件可读的运行类型携带目录显示数据，而 fork 原样复制它的文本、ACP 来源会被分叉继承；放 `session_catalog` 则与该表"每列都从事件推导"的性质冲突，创建来源不是事件，回填或重建都取不回来。理由见 [Session 分类 D1](session-classification.md#9-decisions)。
- **升级**：沿用 `IMMEDIATE` 启动准入事务。
  - 空库直接建 v30，该列连同上面的约束与默认值写入 `agent_sessions` 的建表语句。
  - 精确匹配且标记相符的 v29 库在同一事务内 `ALTER TABLE agent_sessions ADD COLUMN origin ...`（ALTER 生成的表文本须与空库的完全一致，启动门禁逐字比较），随后 `UPDATE agent_sessions SET origin='internal' WHERE leader_session_id IS NOT NULL`；标记表改为 30。
  - 标记相符的 v28 及更早受支持格式先执行已有各步，再执行本步。
  - 其余旧行保持 `unknown`：升级不根据 `interactive`、Agent 归属或消息文本猜来源。
- **写入方**：`session.create` 写 `client`，或用请求里的 `acp`；Collaboration 成员创建写 `internal` 并同时写 leader；`sessions.launch` 写 `internal`；`session.fork` 写 `client`；[迁移](../plugins/Migration.md)导入会话时写映射后的来源或 `unknown`。这些写入都使用显式列名，包括迁移与分叉的插入语句。
- **不能留在 v29**：启动要求精确 SQL 结构，v29 守护进程会拒绝多出该列的库；这正是"旧读取器无法解释新值"时的既有行为。
- **单向**：不做备份、降级或迁移选项，与之前各步相同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v29 to v30; earlier dimcode versions cannot open it.`
- **验证**：
  - 用 v29 结构构建一次性库，其中一个成员 Session（`leader_session_id` 非空）与一个普通 Session；升级后核对结构与标记恰为 v30、成员行 `origin='internal'`、普通行 `origin='unknown'`、其他列逐行不变，重复打开不再升级。
  - v28、v27、v26、v25、v24、v23、v22、v21、v20、v19、v18、v17、v16、v15、v13 库一次升级到 v30。
  - 标记不符或升级到一半的结构被拒绝并保留，旧二进制拒绝升级后的库。
  - 每条创建路径写入自己的值，且分叉不继承来源；由 `session-classification` 的流程测试与 `storage_flow.rs` 本步的 `a_v29_store_upgrades_in_place_once` 覆盖。

### ADR 013 v29 格式决定

[Command output ownership](../../adr/013-command-output-ownership.md)（ADR 013）的原生存储格式为 **v29**，从 v28 原地升级。

- **新增结构与索引**：`session_output_owners(session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), path TEXT NOT NULL, PRIMARY KEY (session_id, path))` 与索引 `session_output_owners_by_path(path)`。其他 SQL 结构、持久化 JSON、内容寻址与既有索引不变。主键服务 fork 复制、Session 内读取和删除；索引服务删除时按目录判断“本库是否还有别的 Session 需要它”，主键无法承担该检查。
- **为什么另立一表**：输出没有 `session_files` 行或 id，运行中会增长，并按目录删除；多态表需要可空键和类型判别，且每次清扫都要按类型过滤。该表的存在使 fork 在源 Session 删除后仍能读取被复制的输出。
- **升级**：空库创建 v29；精确匹配且标记相符的 v28 库在现有启动准入事务内创建空表与索引，重建标记表并写入 29。更早的受支持格式先执行已有步骤，再执行本步。失败回滚；旧业务行不变，不回填历史输出（升级前的输出没有行，恢复的 Session 可能再请求一次，升级后的 fork 不继承）。
- **单向**：v28 守护进程拒绝 v29 库；不降级。准入仍要求精确 SQL 结构及对应标记。
- **验证**：`storage_flow.rs` 的 `a_v28_store_upgrades_in_place_once` 核对 v28 原地升级、业务行保持、新表初始为空和再次打开不再升级；`output_ownership_survives_a_fork_and_a_source_delete` 核对注册、fork 复制、源删除后 fork 读取、最后所有者删除目录，以及 `owns` 同时读取两张所有权表；`documented_native_records_restore_with_legacy_snapshot_bypassed` 经完整记录示例的输出行核对重启后原样恢复；既有各格式升级测试核对完整升级链。

### #717 v28 格式决定

[Session grants](../../adr/012-session-grants.md) 的原生存储格式为 **v28**，从 #593 的 v27 原地升级。v27 保留委派归属的两列和两个索引。

- **新增结构**：`session_grants(session_id, path, kind, mode)`，列、主键、外键与约束见当前表定义；其他 SQL 结构和持久化 JSON 不变。主键服务 Session 内的读取、upsert 和删除；Permission 通过 `SessionGrants` 读取该 Session 的全部 grants 后匹配，没有按路径的单列查询，因此不另建索引。
- **升级**：空库创建 v28；精确匹配且标记相符的 v27 库在现有启动准入事务内创建空的 grants 表，重建标记表并写入 28。更早的受支持格式先执行已有步骤，再执行本步。失败回滚；旧业务行及委派归属列原值不变，不从历史审批生成 grants。
- **单向**：v27 守护进程拒绝 v28 库；不降级。准入仍要求精确 SQL 结构及对应标记，不能将不同结构共用同一编号。
- **验证**：`storage_flow.rs` 的 `a_v27_store_upgrades_in_place_once` 核对 v27 原地升级、业务行保持、grants 初始为空和再次打开不再升级；`documented_native_records_restore_with_legacy_snapshot_bypassed` 经完整记录示例的 grants 行核对重启后原样恢复；`a_delete_removes_the_sessions_grants` 核对删除事务同时清掉 grants；既有各格式升级测试核对完整升级链。

### #593 v27 格式决定

[Collaboration](../plugins/Collaboration.md#6-delegation-ownership-and-lifetime)（#593）的格式决定：升级到 **v27**，从 v26 原地升级。

- **只加两个可空列与两个索引**：`agents.owner_session_id TEXT REFERENCES agent_sessions(session_id)` 与 `agent_sessions.leader_session_id TEXT REFERENCES agent_sessions(session_id)`，以及 `agents_by_owner`、`agent_sessions_by_leader`。其他表、列、持久化 JSON、内容寻址与既有索引不变。
  - `agents.owner_session_id` 非空表示该 leader Session 拥有这个私有 Agent；NULL 表示公开 Agent（默认 Agent 与 `agent.create` 创建的）。它就是"临时 Agent"的唯一标志，不另设布尔列。
  - `agent_sessions.leader_session_id` 非空表示这个 Session 是那个 leader Session 的委派成员；公开 Agent 下的私有成员同样靠它。`session_owners` 仍只表示"Session 属于哪个 Agent"，不是委派归属。
- **为什么必须落盘**：成员 Session 的 `session_owners.agent_id` 要求 `agents` 行存在，所以私有 Agent 不能"不落盘"，只能记住归属和寿命；归属原在进程内存，重启后丢失，而读取方把"查不到归属"当成公开。
- **升级**：空库直接建 v27。精确的 v26 库在同一启动准入事务内先 `ALTER TABLE` 两列，再建两个索引并重建标记写入 27；精确的 v13、v15–v25 库先执行已有步骤（含 v26 的 Provider 上限改写），再执行本步。启动门禁比较精确 SQL 结构，因此 v26 与更早版本的表结构单独识别；v26 守护进程拒绝 v27 库并保留数据。
- **旧行**：升级不写业务行，v26 及更早写入的 `agents`、`agent_sessions` 行两列均为 NULL。旧委派的创建者只存在于当时的进程内存，无法恢复，因此这些 Agent 升级后仍是公开、无主，只能显式删除。不做一次性清理：`agent.create` 创建的公开 Agent 可能同样没有 Session，无法与旧委派区分。
- **验证**：一次性 v26 库升级后结构恰为 v27、标记为 27、业务行逐行不变，重复打开不再升级；v13 起的各版一次升级到 v27；`agents` 与 `agent_sessions` 的写入都使用显式列名，分叉与目录副本显式复制或不复制这两列。见 `storage_flow.rs` 的 `a_v26_store_upgrades_in_place_once` 及同组各步测试。

### #596 v25 格式决定

[SessionStore #596](https://github.com/arcships/dim/issues/596) 的格式决定：升级到 **v25**，从 v24 原地升级。

- **删除索引**：删除 `timeline_events_by_end`（`CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence)`）。正向区间读取已按主键限定上下界，该索引没有读取方；合并输出行不再为它维护每次 `last_sequence` 更新。其他表、列、索引、持久化 JSON 与内容寻址不变。
- **版本边界**：`agent_store_format` 改为 `CHECK (version = 25)`，唯一标记为 25。不能在 v24 内删除索引，因为启动门禁要求精确 SQL 结构；v24 守护进程拒绝 v25 库并保留数据。
- **升级**：空库直接创建 v25。精确的 v24 库在同一启动准入事务内删除索引、重建标记并写入 25；精确的 v13、v15–v23 库先执行已有步骤，再删除索引。旧结构校验仍要求该索引存在，缺失索引、标记不符或半升级的库都拒绝并保留。失败回滚整个事务，不改写任何已有业务行。
- **读取与迁移**：只读导出可以原样打开 v24 及既有受支持格式，也可读取 v25，不升级来源。迁移目标使用 v25；已有目标由同一存储准入路径升级。升级单向，不新增备份、降级或迁移选项。
- **验证**：一次性 v24 库升级后精确结构、标记与业务行保持预期，重复打开不再升级；既有各版升级、损坏库保留、只读导出、迁移和主键区间计划由公共流程验证。`timeline_acceptance rounds` 报告前后的每事件 WAL 字节与 checkpoint sync 次数，`read-load` 报告流式写入下的读延迟与 fork/delete 期间的发布延迟，测量结果放在 PR。

### #555 v24 格式决定

[Prompt](../plugins/Prompt.md)（#555）的格式决定：升级到 **v24**，从 v23 原地升级。

- **只增一处可选字段**：SQL 结构、表、列、索引、内容寻址与其他记录的字节都不变。
  - `ContextConfiguredData.system_source`：`context/configured` 上 `system` 所依据的源文本，即 System 中除去 guide 的部分。它是模型、工具或指南单独的 adoption 重渲染 guide 的依据：这类 adoption 用当前 frame 和当前 entries 在 `system_source` 上重渲染 System，并照旧记录新的 `system_source`。此前写下的记录没有它，按缺失读取，保留其已采用的 System，不做重渲染。
- **不能留在 v23**：
  - `ContextConfiguredData` 拒绝未知字段，v23 读取器解不了 `system_source`。
  - 自 #193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行；打开 Session 时还会解码每条已存事件（`validate_session`），所以留在 v23 会让含该字段的 Session 打不开。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 24)`。
- **协议不变**：v1 连接不投影 `context/configured`（`v1::EventRef` 排除该事件），v1 形状不变；v2 与 v3 携带原生事件，`system_source` 缺省时照旧省略，不新增协议版本。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v24。
  - 结构恰为 v23 且标记相符的库，在同一事务内只重建 `agent_store_format` 并写入 24，不读取、不改写任何业务行；失败则回滚，原 v23 库保持不变。
  - 结构恰为 v22、v21、v20、v19、v18、v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v24。
  - 恰为 v24 的库直接打开。
  - 其他库（标记不符的结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v23 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v24 库，因为其启动门禁比较精确的 SQL 结构，而 `agent_store_format` 的 `CHECK (version = 24)` 已不同。它不会等到恢复含该字段的 Session 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用同一行：`Upgraded native store <path> from format v23 to v24; earlier dimcode versions cannot open it.`
- **验证**：用 v23 结构和完整记录示例构建一次性 v23 库，升级后核对结构恰为 v24、标记为 24、业务行逐行不变；v22、v21、v20、v19、v18、v17、v16、v15、v13 库一次升级到 v24；标记不符的 v23、v24 结构和升级到一半的库被拒绝并保留；没有 `system_source` 的旧 `context/configured` 记录照原样重放并保留其 System；从不打开用户已有的库（`storage_flow.rs` 的 `a_v23_store_upgrades_in_place_once` 及其他各步的同名测试、`unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #556 v23 格式决定

[客户端 Session 目录](session-catalog.md#31-sessioncatalog)（#556）的格式决定：升级到 **v23**，从 v22 原地升级。若在实现之前另有格式变更先合入，实现取当时的下一个编号，本节其余内容不随编号改变。这是本文件第一个读取已有行的升级。

- **新增一张表与两个索引**：`session_catalog(session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0), preview TEXT, cwd TEXT NOT NULL)`，索引 `session_catalog_by_activity(updated_at_ms DESC, session_id)` 与局部索引 `session_catalog_with_preview(updated_at_ms DESC, session_id) WHERE preview IS NOT NULL`。`agent_store_format` 的约束改为 `CHECK (version = 23)`。
  - 每个 Session 至多一行，从创建起与 `agent_sessions` 一一对应；写入、回滚与删除见下面的规则。
  - `updated_at_ms` 是该 Session `main` 上最新一条活动事件的 `create_at`：活动是 Session 自己的活动——带用户部分的 `input/message`、`turn/end`、`branch/create`（父分支记下分支 prompt）与 `session/forked`——其余事件都是家事事件，不移动这一行；没有活动事件的 Session 停在 `created_at_ms`。[#1099 v34 格式决定](#1099-v34-格式决定) 按本规则重写既有行，并取代本节"最后一次提交的最后一个事件"的写法。
  - `preview` 是窗口里第一条带用户部分的 `input/message` 的文字：各文本块以换行连接，超过 120 个字符时截到 120 个字符并以 `…` 结尾。窗口是 `main` 上起始于前 256 个事件内的各个批次，与今天的读取一样按批次完整——承载第 256 个事件的批次整批读入，因此该批次里紧接 256 之后的用户输入也在窗口内。该消息的文字为空（例如只有图片）时为 NULL，之后的消息不再填它；窗口里没有这样的消息时也为 NULL。决定一旦做出，包括决定为 NULL，就不再改变。
  - `cwd` 是 `agent_sessions.config` 的工作目录。目录查询按工作区过滤并回复它，所以不必为每个 Session 解码配置 JSON。
  - `session_catalog_by_activity` 服务按活动时间的键集分页；`session_catalog_with_preview` 服务只要非空预览的读取，因此前面有多少没有预览的行都不影响那条读取的代价。
- **创建即写行**：`session.create` 在写 `agent_sessions` 的同一事务里写下这一行：`updated_at_ms` 为 `created_at_ms`，`preview` 为 NULL，`cwd` 取自创建的配置。因此新 Session 从创建那一刻起就按目录列出，与今天的 `session.summaries` 相同：创建时间、没有预览。
- **同一事务写入**：SessionStore 在提交 `main` 每个批次、且批次里含有活动事件的同一个写入事务里写这一行，写下该批次最新一条活动事件的 `create_at`；只含家事事件的批次不碰这一行。因此这一行与事件不会有一半可见。`preview` 只在仍未决定时按上面的规则写入，之后的批次只更新 `updated_at_ms`。实时写入没有单独的规则：一个批次带用户消息、该消息在上面按批次完整的窗口内、且窗口里没有更早的带用户部分的 `input/message` 时，它写入 `preview`（文字为空时写 NULL）。这个判断用与回填相同的窗口读一次已提交事件——读取的批次止于第 256 个事件所在的批次；Session 的 `main` 事件不足 256 个时，止于它最后一个已提交批次——且只在 `preview` 仍为 NULL 时发生；因此回填行与实时行逐列相同。
- **删除、回滚与分叉**：删除与创建回滚在 S5c 的同一事务里、在 `agent_sessions` 之前删除这一行；分叉在分叉事务内写新行，与按 F 的事件回填逐列相同：`updated_at_ms` 是该事务写入的最新一条活动事件的 `create_at`——`0` 以外的分叉就是 `at + 1` 处那条 `session/forked`，其 `create_at` 是分叉当时的时刻；`0` 处分叉不写事件，取创建时间——`preview` 是同一按批次完整的窗口里第一条用户输入的文字或 NULL，`0` 处分叉为 NULL。
- **不记分类**：本步不记录子 Session 或创建来源。[#557](https://github.com/arcships/dim/issues/557) 的分类字段连同它自己的格式决定一起加入，与上文不记分叉来源相同。
- **不能留在 v22**：启动要求精确的 SQL 结构，v22 的守护进程会拒绝多出表和索引的库，不满足"旧读取器能正确解释新值"的保持条件。
- **读取或转换路径**：在现有的启动准入事务（`IMMEDIATE`，写入型 PRAGMA 之前）内处理：
  - 空库直接建 v23，`session_catalog` 为空。
  - 结构恰为 v22 且标记相符的库，在同一事务内建新表与两个索引，并从已存事件为每个 Session 回填一行。
  - 标记相符的 v21、v20、v19、v18、v17、v16、v15 或 v13 库，在同一事务内先走已有各步，再走本步，直接成为 v23。
  - 恰为 v23 的库直接打开。
  - 其他库（标记不符的 v22 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **回填的一次性代价**：回填为每个 Session 读一次 `main`：按上面的活动规则取最新一条活动事件给出 `updated_at_ms`，没有活动事件的 Session 得到 `created_at_ms`；起始于前 256 个事件内的批次里找第一条用户输入作为 `preview`，找不到时得到 NULL 预览，都与创建时写下的一行相同。这与今天一次完整目录读同阶——导入 Session 的最后一个批次是完整的 `context/compaction`，本步把它解码一次。按 #556 的测量，2,300 个导入 Session 的库上约 3 秒，2.3 GB 的库上超过 20 秒；它只在升级的那次启动支付，此后的目录读不再解码任何事件。[#1099 v34 格式决定](#1099-v34-格式决定) 复用这段预览回填与缺行补写，`updated_at_ms` 改取最新一条活动事件，代价见该节。
- **单向**：不做备份，与之前各步相同。旧守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝升级后的库。
- **升级日志**：沿用 S5a 的那一行：`Upgraded native store <path> from format v22 to v23; earlier dimcode versions cannot open it.`
- **验证**：
  - 用 v22 结构和完整记录示例构建一次性 v22 库，升级后核对结构恰为 v23、标记为 23、业务行逐行不变，`session_catalog` 每个 Session 一行且与事件一致，没有已提交 `main` 批次的 Session 得到 `created_at_ms` 与 NULL。
  - v21、v20、v19、v18、v17、v16、v15、v13 库一次升级到 v23。
  - 标记不符或升级到一半的结构被拒绝并保留。
  - 回填写出与继续提交写出的行逐列相同：升级后同一批事件再提交，`updated_at_ms` 跟着最新一条活动事件走，`preview` 不被覆盖。
  - 预览的窗口与最终性：承载第 256 个事件的批次里紧接 256 之后的用户输入升级后即为预览；第一条用户消息在起始于 256 之后的批次里的 Session 升级后没有预览，此后的批次也不写它；第一条用户消息只有图片的 Session 保持 NULL，之后的消息不填它。
  - `session.create` 写下的一行有创建时间与 NULL 预览，创建回滚后没有这一行。
  - 删除与分叉在各自的事务内维护这一行；非零处分叉的一行以该事务写入的最新一条活动事件——`at + 1` 处的 `session/forked`——的 `create_at` 为 `updated_at_ms`，`0` 处分叉的一行为创建时间与 NULL。
  - `withPreview` 的读取按 `EXPLAIN QUERY PLAN` 使用 `session_catalog_with_preview`，前面有多少无预览的行都不改变它的代价。
  - 从不打开用户已有的库。

  对应 `storage_flow.rs` 中本步的 `a_v22_store_upgrades_in_place_once` 及其他各步的同名测试、`unsupported_legacy_or_changed_schema_is_rejected_without_migration` 与本期新增的目录流程测试。

### #522 v26 格式决定

[Provider 管理 §3.5](provider-management.md#35-declared-model-limits)（#522）的格式决定：升级到 **v26**，从 v25 原地升级（[#596 v25 格式决定](#596-v25-格式决定) 先占用 v25，本步在其基础上顺延；版本分配见 [#596 的格式分配](https://github.com/arcships/dim/issues/596#issuecomment-6512499424)）。

- **改变的已存值**：`providers.record` 内每个模型描述的两项上限 `contextWindow`、`maxOutputTokens` 变成可选。没有来源声明时该键缺省，不再存假定值。这是 [Provider 管理 §3.5](provider-management.md#35-declared-model-limits) 的缺失语义在存储上的表示。
- **为什么不能沿用 v25**：v25 读取器要求两项上限都是正整数，遇到缺键的 Provider 记录会解析失败；那是升级后到读取某个 Provider 才失败，违反本文件的启动拒绝规则。启动门禁比较精确的 SQL 结构，所以格式号必须改变。
- **升级路径**：沿用 `IMMEDIATE` 启动准入事务，在改写标记的同一事务内改写 Provider 记录：
  - 空库直接建新版本。
  - 结构恰为 v25 且标记相符的库，在同一事务内处理；标记相符的更旧库先走既有各步，再走本步。
  - 对每条 `providers.record`，只有投影输出可能失去没有来源声明的上限：静态记录中 `remoteModels` 为真时的 `models`（远程清单）、内置记录由捆绑目录生成的 `models` 与托管记录的 `models[].entry`（中继）删除既未在 `metadata.capabilityOverrideFields` 中列出、也未被该条目的 `metadata.raw` 声明的值，因为它们都从各自来源重新生成。静态记录的描述覆盖、`remoteModels` 为假时的 `models` 与每个 `userModels` 条目保存客户端或旧版导入器给出的值，与两个旧版导入器共用同一个函数，按每项上限分别判定：已列入 `capabilityOverrideFields` 或由 `metadata.raw` 声明的上限是声明，保留并补上标记；未列为标记、未被 `metadata.raw` 声明、且数值恰为该字段旧假定值（`contextWindow` 128000、`maxOutputTokens` 16384）的上限可能来自探测投影的填充，删除该键使其未声明；其余数值只可能来自用户输入或清单声明，保留并补上标记。每项各自判定，所以存下的 `32000`/`16384` 保留窗口、删除输出上限。v25 的 `provider.create` 与 `provider.models.create` 要求客户端给出两个数值，却从不写标记，而探测为每个缺失字段分别补上假定值，因此恰好等于假定值的未标记数字无法与用户输入区分；本决定选择宁可再次询问，也不保留可能的假定值——用户若恰好输入 128000 或 16384，升级后该项显示为未声明，用 `--set-missing-limits` 或 `/models` 重新声明即可。第二个代价落在静态记录只改名的编辑上：v25 的 `models.edit` 只改显示名时会把生效条目复制成不带标记的描述覆盖，若该条目原有的上限来自捆绑目录或清单、不等于假定值且 `metadata.raw` 未声明，本步会保留它并补上标记，等于把升级时的取值冻结为用户声明——升级当场取值正确，但此后捆绑目录的更正不会再到达该模型，直到用户重置这条覆盖。标记由这些写入者写下：v25 的 `models.edit` 为它编辑的每项上限写标记；两个旧版导入器在导入时为保留的上限补标记，描述覆盖也补，本步对静态记录的描述覆盖与用户模型按同一规则补标记。`provider.create`、`provider.models.create` 与旧版探测都不写标记。描述覆盖不能套用严格规则：它不只由 `models.edit` 写入，两个旧版导入器把旧库的 `modelConfigOverrides` 原样复制、不写标记，其中的上限是用户输入的值；若本步删除，同一份旧数据全新导入却会保留并补上标记，两条升级路径对同一份数据给出不同结果。托管记录的描述覆盖仍按严格规则：它只由 `models.edit` 写入，而 v25 的 `models.edit` 已为它编辑的上限写标记。`metadata.raw` 保留清单项目的原文，升级与投影用同一组键和同一函数读取它（`context_window`/`contextWindow`/`inputTokenLimit`、`max_output_tokens`/`maxOutputTokens`/`outputTokenLimit`，`limit.context`/`limit.output` 与 `capabilities.contextWindow`/`capabilities.maxOutputTokens`，以及 OpenRouter 的 `context_length` 与 `top_provider.max_completion_tokens`、vLLM 的 `max_model_len`、LM Studio 的 `max_context_length`），不另列清单。投影输出只删除没有任何来源声明的上限，不比较数值，也不补齐替代值；两个旧版导入器中 `modelsSource` 为 `bundled`/`remote` 的 `models` 同属投影输出，按同一严格规则处理。
  - 其余取值不变：改写会按当前结构重新序列化每条记录，空的可选字段与字段顺序可能不同，但除上面删除与补上的上限及其标记外，`capabilityOverrideFields` 的其他项、其他能力位、metadata、凭据、偏好、路由与选择都不改；任何一步失败则回滚，原库保持不变。
  - 恰为新版本的库直接打开；其他库（标记不符、升级到一半、更旧格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **改写的位置**：`providers.record` 的 JSON 由 Provider 拥有的记录转换函数改写，存储层只负责在准入事务内对每条记录调用它，不解析 Provider 的字段。
- **升级后从哪里取得值**：缺省键的上限由有效目录按来源补齐，不写回记录——默认项与用户模型走既有捆绑匹配，托管目录模型走同一 `BundledCatalog::fallback`，描述覆盖未自行声明的上限取自其基础项（[Provider 管理 §3.5](provider-management.md#35-declared-model-limits)）。补上的标记使保留的上限不会被捆绑匹配替换：与捆绑目录同 ID 的用户模型升级后仍取用户声明的窗口；远程清单里没有任何来源声明的私有模型变为未声明，由管理面报告原因。
- **单向**：旧二进制按既有精确 SQL 结构检查拒绝整个新库，报现有的 "unsupported SQLite schema … preserved" 错误；不会到读取某个 Provider 时才失败。不做降级或备份。
- **升级日志**：沿用 S5a 的那一行：`Upgraded native store <path> from format v25 to v26; earlier dimcode versions cannot open it.`
- **验证**：
  - 在一次性 v25 库中写入 Provider 记录，覆盖远程清单（`remoteModels` 为真）中声明了两项、仅窗口与两项皆无的条目，只改名的描述覆盖，带标记的覆盖，导入器原样复制的描述覆盖（不写标记，两项为 `200000`/`32000` 的输入值），`remoteModels` 为假的自建模型的条目，带标记与不带标记的用户模型（不含标记的分别持有旧假定值对、`32000`/`16384` 的混合对与 `64000`/`8000` 的输入值），一个与捆绑目录同 ID 但窗口不同的用户模型，以及一条托管记录；升级后按写在测试里的固定 JSON 值逐条核对：清单与托管记录只删除没有任何来源声明的上限（未列入标记且 `metadata.raw` 未声明），描述覆盖、自建模型与用户模型按每项上限判定（输入值保留并补标记，恰好等于假定值的字段删除为未声明），标记与结构为新版本，其余取值不变，再次打开不再升级。两个旧版导入器对同一规则的执行由 `provider_flow.rs` 的 `provider.import_legacy` 流程与 `dim-migration` 的迁移流程各自核对。
  - 有效目录中与捆绑目录同 ID 的用户模型仍取用户声明的窗口，命中捆绑目录的模型取值与升级前相同；扫描不到的模型带 `missingLimits` 且不可选；前者由本步的存储测试经投影核对，其余由 Provider 流程测试保护。
  - 用本决定之前的基线构建的旧二进制打开升级后的库，在启动时以 "unsupported SQLite schema … preserved" 拒绝并保留，不读取任何 Provider。
  - 对应 `storage_flow.rs` 的 `a_v25_provider_record_keeps_supplied_limits_and_drops_assumed_ones` 与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`，以及 `provider_flow.rs` 的目录流程。

## 索引与实际查询

| 版本与索引 | 精确列序/条件 | 服务的查询 |
| --- | --- | --- |
| v1 主键 `agents`、`agent_sessions` | 各自 ID | 单 Agent/Session 查找；Agent 全目录目前仍按 `is_default DESC, agent_id` 扫描排序 |
| v1 主键 `session_owners` | `(session_id)` | Session 到 Agent/协议版本；v1 全 owner 目录按 `session_id` 扫描 |
| v1 主键 `timeline_branches` | `(session_id, branch_id)` | 分支 head、整 Session 分支目录 |
| v1 主键 `timeline_events` | `(session_id, branch_id, sequence)` | 追加前读取最后一行、单分支顺序恢复、从起点读取历史 |
| v1 主键 `timeline_content` | `(session_id, hash)` | 请求引用的定点读取和 Session 内去重；v17 列表节点逐个加载与分叉复制的节点闭包 |
| v1 唯一 `agents_default_unique` | `(is_default) WHERE is_default = 1` | 至多一个默认 Agent；不保证其存在 |
| v27 `agents_by_owner` | `(owner_session_id)` | 删除 owner 时定位它拥有的 Agent；启动时找出无成员或失去 owner 的私有 Agent |
| v27 `agent_sessions_by_leader` | `(leader_session_id)` | 装载时重建 Team 关系 |
| 派生（不占格式号）`timeline_history_anchors` | `(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'input/message')` | 最新回合边界与回合提示输入的查找（[latest-turn 窗口](session-history-window.md)） |
| 派生（不占格式号）`timeline_policy_turns` | `(session_id, branch_id, kind, sequence, event_id) WHERE kind = 'turn/start'` | 无读者：它只服务已删除的 `policy_order`。删除派生索引要作为格式步骤，所以保留它的创建与启动门禁的豁免 |
| 派生（不占格式号）`timeline_events_by_kind` | `(session_id, branch_id, kind, sequence, event_id)` | 按 kind 的事件查询与它的事件 id（[ADR 019](../../adr/019-event-query.md) D4） |
| v12 主键 `timeline_snapshots` | `(session_id, branch_id, interpreter, sequence)` | 兼容 interpreter，`sequence<=through ORDER BY sequence DESC LIMIT 1`；连接事件锚点 |
| v15 主键 `session_files` | `(file_id)` | put 的两次事务与放弃、清扫按文件定点更新和删除 |
| v15 主键 `session_file_owners` | `(session_id, file_id)` | put 写入 owner；fork 复制、删除和创建回滚按 Session 读取和删除 |
| v15 `session_file_owners_by_file` | `(file_id)` | 清扫判断文件是否已无 owner |
| v23 `session_catalog_by_activity` | `(updated_at_ms DESC, session_id)` | 目录键集分页 |
| v23 `session_catalog_with_preview` | `(updated_at_ms DESC, session_id) WHERE preview IS NOT NULL` | 过滤无预览行的目录分页 |
| v16 主键 `session_metadata` | `(session_id)` | `read`、`setTitle`、列表联结标题、删除与创建回滚按 Session 删除 |
| v28 主键 `session_grants` | `(session_id, path)` | 一个 Session 的 grant 读取、add 的 upsert、remove 与 delete 按 Session 定点删除 |
| v29 主键 `session_output_owners` | `(session_id, path)` | 注册的 upsert、`owns` 的精确查找、fork 复制与 delete 按 Session 定点删除 |
| v29 `session_output_owners_by_path` | `(path)` | 删除一个目录前判断本库是否还有别的 Session 在该目录下有行 |
| v33 主键 `worktree_directories` | `(repository, root)` | 目录去重和候选列表；无 Session 关联也保留 |
| v33 主键 `session_worktrees` | `(session_id, repository, root)` | Session 内目录去重、删除与创建回滚 |
| v33 `session_worktrees_by_directory` | `(repository, root, session_id)` | 某目录的所有 Session 及继承子树 |

## 分页查询

以下索引已与表结构一起创建，供目录过滤、事件分页和跨页 ToolCall 读取使用。事件 ID 索引非唯一，恢复时逐事件查重并核对索引值。

| 索引 | 精确列序/条件 | 查询 |
| --- | --- | --- |
| `session_owners_by_agent` | `(agent_id, protocol_version, session_id)` | 按 Agent 和协议过滤目录 |
| `timeline_events_by_id` | `(session_id, event_id) WHERE event_id IS NOT NULL`，非唯一 | 按事件 ID 查找跨页 ToolCall |
| `timeline_input_submissions` | `(session_id, branch_id, json_extract(body, '$.record.event.data.submission_id'), sequence) WHERE kind = 'input/message'`，非唯一 | 按提交标识定位输入；属于 v35 精确 schema，不属于启动时补建的派生索引 |
| `timeline_events_requests` | `(session_id, branch_id, kind, coalesce(json_extract(body,'$.record.event.data.request_id'),json_extract(body,'$.request_id')), sequence)`；WHERE 为该 coalesce 表达式 IS NOT NULL | 请求标识等值定位，兼容 inline 与 output 行 |
| `timeline_events_inputs` | `(session_id, branch_id, kind, json_extract(body,'$.record.event.data.input_id'), sequence)`；WHERE 为该 json_extract 表达式 IS NOT NULL | 输入采纳、删除、应用和撤回关联 |
| `timeline_events_users` | `(session_id, branch_id, kind, json_type(body,'$.record.event.data.user'), sequence)`；WHERE 为该 json_type 表达式 IS NOT NULL | 用户输入存在性筛选与计数 |
| `timeline_tool_use_starts` | `(session_id, json_extract(body, '$.record.event.data.request_id'), json_extract(body, '$.record.event.data.call_id')) WHERE branch_id = 'main' AND kind = 'step/tool-use-start'` | 按请求与调用 ID 查找跨页 `step/tool-use-start`，为不带 `call_event_id` 的结果取调用名；查询须使用相同表达式与条件 |
| `timeline_events_context` | `(session_id, branch_id, sequence) WHERE context_render = 1 OR context_state = 1` | 只取渲染器或 checkpoint 折叠消费的行（v32；v31 的两个部分索引各取其一） |
| `timeline_events_positions` | `(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'context/compaction-start', 'context/compaction')` | 策略定位：轮次开始与压缩边界（v31） |

正向页的 `after` 与 `through` 须为批次边界。事件查询的起点是事件序号，落在合并行内部时从覆盖它的物理行开始读、再按序号过滤，且页按事件数截断、不与原子批次对齐（[插件契约 §5.4](../plugin-contract.md#54-the-event-query)），见 [读取边界](../context-read-and-compaction.md#4-读取边界)。区间查询先按主键定位起点之前或等于起点的最后一条物理行，以其 `sequence` 为下界、区间终点为上界，按 `sequence` 读取；没有前驱行时以下界起点读取，并以 `last_sequence >= 起点` 排除不相交的前驱行。合并行覆盖页起点时完整解码，再按内部 sequence 过滤；反向页须纳入 `sequence < before <= last_sequence` 的跨界行，并停在原子批次边界。正向读取按 256 个逻辑序号分段，每段结束释放读连接；恢复不扫描历史；请求列表的遍历每加载至多 256 个节点也释放一次，下一次持有从中断处继续。单条物理行完整解码，允许超过通常页大小。流程测试用 `EXPLAIN QUERY PLAN` 核对正向读取主键的上下界；`timeline_acceptance paged-reads` 测量长历史读/恢复与并发提交延迟；`restore_cost` 在一次性库上经 mock 模型轮次增长 Session，按历史规模测量恢复耗时，并拆分目录校验、事件读取与 `branch/resume` 写入，同时报告每条事件的存储字节数（`cargo run --release --locked -p dim-app-server --example restore_cost -- [plain|tools|product] [1001,3001,10001] [7]`；`product` 使用产品上下文窗口与摘要压缩）；`single-append` 与 `single-partial` 测量单个请求的冷读与重开后第一次写入（`-- [single-append|single-partial] [2000,20000] [7] [chain|last]`）。`timeline_acceptance crashes` 与 `powerloss` 的中断批次含一个请求，恢复后校验行可读、序号连续、每个列表的节点俱在，并核对重启后写入与未中断写入的行逐字节一致。反向历史接口仍为独立协议草案。

`session_owners.session_id` 与 `agents.agent_id` 两个 ID 列的 SQL 可空缺口见表结构。永久关闭字段与旧记录回填尚未设计。

## 完整记录示例（v38）

v38 的记录与 v37 相同；可选 `parent_call_id` 在示例中省略；顶层模型批次不写该字段。

下例是一条 Session `s1`、主分支 `main` 和子分支 `child` 的全部原生表记录，用于核对存储关系，不代表完整的对话执行序列。为便于阅读，`profile`、`config`、`info` 和事件 `body` 这几个实际为 `TEXT` 的列在展示时解析为 JSON；写入时各自是完整 JSON 字符串，类型规定省略的可选字段照常省略。ID/时间是示例值。`timeline_content.body` 保留实际写入的字符串形式，因此下面各 hash 可以按上文算法复核。`main` 第 4 条 `model/call` 是升级前 v16 写入的行，`child` 第 3 条是升级后 v17 写入的行，示例同时展示两种行混存。

The example retains an old `dim/context-projection/v1` snapshot: the default v4
interpreter bypasses that unchecked cache and restores from facts. Existing v13
databases needed no migration for this snapshot interpreter change. `s1` owns
one Session file, stored at `<db>.session-files/<file_id>/<name>`; its name and
bytes live only on disk. Its title was written twice; the second write is the
current manual title.

```json
{
  "agent_store_format": [{"version": 34}],
  "session_catalog": [{"session_id":"s1","updated_at_ms":1008,"preview":"Hi","cwd":"/work"}],
  "settings_documents": [],
  "providers": [],
  "provider_selections": [],
  "provider_imports": [],
  "agents": [{"agent_id": "a1", "is_default": 1, "profile": {
    "name": "Example", "description": "Example agent", "systemPrompt": "You are helpful.",
    "tools": [], "model": {"providerId": "mock", "modelId": "deterministic"}
  }}],
  "agent_sessions": [{"session_id": "s1", "created_at_ms": 1000, "origin": "client", "config": {
    "permissionPolicy": {"preset": "read-only", "allowedTools": [], "disallowedTools": []},
    "cwd": "/work", "model": {"providerId": "mock", "modelId": "deterministic"},
    "systemPrompt": "You are helpful.", "tools": [], "collaborationRole": "leader"
  }}],
  "session_owners": [{"session_id": "s1", "agent_id": "a1", "protocol_version": 3}],
  "timeline_branches": [
    {"session_id": "s1", "branch_id": "main", "info": {"id": "main"}, "sequence": 8},
    {"session_id": "s1", "branch_id": "child", "info": {
      "id": "child", "parent": "main", "branch_point": 7, "create_anchor": 8
    }, "sequence": 3}
  ],
  "timeline_content": [
    {"session_id": "s1", "hash": "59cdf8dcbefa00e8fa27452182332492a68dccb11804c2bd571c53f1750c4e69",
     "kind": "message", "body": "{\"role\":\"user\",\"content\":\"Hi\"}"},
    {"session_id": "s1", "hash": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288",
     "kind": "system", "body": "\"You are helpful.\""},
    {"session_id": "s1", "hash": "a081b4c298123f73baa9bc6dfd71782ef548fd8298a097b00eeea206f8d28ffe",
     "kind": "node", "body": "{\"parent\":null,\"message\":{\"role\":\"user\",\"content\":\"Hi\"}}"},
    {"session_id": "s1", "hash": "c5ef63e65386b0f98fd778a499f4c73964afe57473de5f71c8f727a9b1a166ea",
     "kind": "node", "body": "{\"parent\":\"a081b4c298123f73baa9bc6dfd71782ef548fd8298a097b00eeea206f8d28ffe\",\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"}}"},
    {"session_id": "s1", "hash": "03e36a3f41a953b3d046e6ed7eb0b2c7da95c26a166402c5bce7970f55ec447d",
     "kind": "node", "body": "{\"parent\":\"c5ef63e65386b0f98fd778a499f4c73964afe57473de5f71c8f727a9b1a166ea\",\"message\":{\"role\":\"user\",\"content\":\"Follow up\"}}"}
  ],
  "timeline_events": [
    {"session_id": "s1", "branch_id": "main", "sequence": 1, "last_sequence": 1,
     "event_id": "e1",
     "kind": "input/message", "body": {"encoding": "event", "record": {
       "event": {"id": "e1", "sequence": 1, "create_at": 1001, "type": "input", "name": "message", "data": {"user": {"content": [{"type": "text", "text": "Hi"}]}}},
       "refs": null, "trace": null, "batch_end": 1
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 2, "last_sequence": 2,
     "event_id": "e2",
     "kind": "input/consumed", "body": {"encoding": "event", "record": {
       "event": {"id": "e2", "sequence": 2, "create_at": 1002, "type": "input", "name": "consumed",
                 "data": {"input_id": "e1"}},
       "refs": null, "trace": null, "batch_end": 2
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 3, "last_sequence": 3,
     "event_id": "e3",
     "kind": "context/compaction", "body": {"encoding": "event", "record": {
       "event": {"id": "e3", "sequence": 3, "create_at": 1003, "type": "context", "name": "compaction", "data": {
         "turn_id": "t1", "request_id": "r1", "through": 2, "status": "completed",
         "retain_from": "e2", "replacement": [{"role": "user", "content": "Earlier: greeting"}],
         "strategy": "summary",
         "diagnostics": {
           "reason": "auto", "trigger": "threshold", "retainFrom": "e2",
           "estimatedInputTokens": 1.0, "estimatedOutputTokens": 1.0,
           "entriesReplaced": 0, "modelCallCount": 1, "durationMs": 1,
           "strategy": {"kind": "summary", "summaryChars": 17, "estimatedSummaryTokens": 5,
                        "chunkCount": 1, "compactedToolPayloadCount": 0, "droppedMessageCount": 0}
         }
       }},
       "refs": null, "trace": null, "batch_end": 3
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 4, "last_sequence": 4,
     "event_id": "e4",
     "kind": "model/call", "body": {"encoding": "event", "record": {
       "event": {"id": "e4", "sequence": 4, "create_at": 1004, "type": "model", "name": "call", "data": {
         "turn_id": "t1", "request_id": "r1", "request": {
           "providerId": "mock", "modelId": "deterministic", "messages": []
         }
       }},
       "refs": {"messages": ["59cdf8dcbefa00e8fa27452182332492a68dccb11804c2bd571c53f1750c4e69"],
                "tools": null, "system": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288"},
       "trace": null, "batch_end": 4
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 5, "last_sequence": 6,
     "kind": "step/text", "body": {
       "encoding": "output", "turn_id": "t1", "request_id": "r1", "started_at": 1005,
       "records": [
         {"event": {"id": "e5", "sequence": 5, "create_at": 1005,
           "type": "step", "name": "text", "data": {"delta": "He"}},
          "refs": null, "trace": null, "batch_end": 6},
         {"event": {"id": "e6", "sequence": 6, "create_at": 1006,
           "type": "step", "name": "text", "data": {"delta": "llo"}},
          "refs": null, "trace": null, "batch_end": 6}
       ]
     }},
    {"session_id": "s1", "branch_id": "main", "sequence": 7, "last_sequence": 7,
     "event_id": "e7",
     "kind": "step/model-output-end", "body": {"encoding": "event", "record": {
       "event": {"id": "e7", "sequence": 7, "create_at": 1007, "type": "step", "name": "model-output-end",
                 "data": {"turn_id": "t1", "request_id": "r1", "status": "completed", "finish_reason": "end_turn"}},
       "refs": null, "trace": null, "batch_end": 7
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 8, "last_sequence": 8,
     "event_id": "e8",
     "kind": "branch/create", "body": {"encoding": "event", "record": {
       "event": {"id": "e8", "sequence": 8, "create_at": 1008, "type": "branch", "name": "create", "data": {
         "at": {"kind": "sequence", "sequence": 7},
         "source": {"kind": "host", "operation_id": "op1"},
         "branch": "child", "prompt": "Follow up"
       }},
       "refs": null, "trace": null, "batch_end": 8
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 1, "last_sequence": 1,
     "event_id": "child-e1",
     "kind": "branch/start", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e1", "sequence": 1, "create_at": 1009, "type": "branch", "name": "start", "data": {
         "parent": "main", "createEvent": 8
       }},
       "refs": null,
       "trace": {"event_id": "child-e1", "original": {
         "event": {"type": "branch", "name": "start", "data": {"parent": "main", "createEvent": 8}},
         "refs": null
       }, "steps": [{"handler": "example", "result": null}]},
       "batch_end": 1
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 2, "last_sequence": 2,
     "event_id": "child-e2",
     "kind": "input/message", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e2", "sequence": 2, "create_at": 1010, "type": "input", "name": "message",
                 "data": {"user": {"content": [{"type": "text", "text": "Follow up"}]}}},
       "refs": null, "trace": null, "batch_end": 2
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 3, "last_sequence": 3,
     "event_id": "child-e3",
     "kind": "model/call", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e3", "sequence": 3, "create_at": 1011, "type": "model", "name": "call", "data": {
         "turn_id": "t2", "request_id": "r2", "request": {
           "providerId": "mock", "modelId": "deterministic", "messages": []
         }
       }},
       "refs": {"list": "03e36a3f41a953b3d046e6ed7eb0b2c7da95c26a166402c5bce7970f55ec447d", "length": 3,
                "tools": null, "system": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288"},
       "trace": null, "batch_end": 3
     }}}
  ],
  "timeline_snapshots": [{"session_id": "s1", "branch_id": "main", "sequence": 3,
    "interpreter": "dim/context-projection/v1", "event_id": "e3",
    "state": "{\"resume\":{\"branch_id\":\"main\",\"sequence\":2,\"event_id\":\"e2\"},\"state\":{\"retain_from\":\"e2\",\"prompts\":{\"entries\":[],\"revision\":null,\"system_revision\":null,\"system\":\"\",\"turns\":[],\"tasks\":[]},\"delivered\":[],\"inputs\":[[\"e1\",{\"user\":{\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}}]],\"configuration\":null,\"output\":[],\"open_tools\":[],\"output_open\":false}}"
  }],
  "session_files": [{"file_id": "0b4e7d2c-5a1f-4c3e-9f6a-2d8b1e7c4a90", "node": null, "writer": null}],
  "session_file_owners": [{"session_id": "s1", "file_id": "0b4e7d2c-5a1f-4c3e-9f6a-2d8b1e7c4a90"}],
  "session_grants": [{"session_id": "s1", "path": "/work/notes.md", "kind": "file", "mode": "write"}],
  "session_output_owners": [{"session_id": "s1", "path": "/work/command-output/s1/exec.log"}],
  "worktree_directories": [{"repository":"/repo/.git","root":"/repo-wt"}],
  "session_worktrees": [{"session_id":"s1","repository":"/repo/.git","root":"/repo-wt","subtree":"/repo-wt"}],
  "session_metadata": [{"session_id": "s1", "revision": 2, "title": "Example conversation", "title_source": "manual"}]
}
```

这十一条逻辑事件占十条物理事件行；`main` 的已发布 head 为 8，`child` 为 3。`input/consumed` 把 `e1` 转成 ID 为 `e2` 的用户历史条目；压缩的 `retain_from` 指向事件 `e2`，即从该事件放置的条目起原样保留；可选快照从第 2 条事件重放，所保存的前缀状态含尚未采纳的 `e1` 输入。`main` 的输出行合并两个增量，读出后各自恢复 `turn_id`/`request_id`，两者的原子批次终点都是 6。子分支通过 `branch_point=7` 共享 `main` 的前缀 1–7，不复制父事件；`create_anchor=8` 指向父分支的 `branch/create`。`main` 的 v16 `model/call` 的两个 hash 在同一 Session 的 `timeline_content` 命中。`child` 的 v17 `model/call` 以 `list` 引用三条消息的最后一个节点，`length` 为 3；它在逻辑历史中的上一个请求是 `main` 第 4 条 v16 行，没有可共用的节点，所以三条消息各写一个节点，`system` 按 v16 的写法得到同一地址。`timeline_snapshots` 的独立行锚定主分支第 3 条 completed 事实，事件本身没有快照数据；删除该快照不影响事实重放或 replacement。回滚 Session 时先删除快照，再删除事件、分支、内容、owner 和 Session 文件、grants、输出 owner 行，最后删除 `agent_sessions`；Agent 只有在不再被任何 Session 引用时才能删除。

## v35: submission identity index

The property-query consumer needs `(session, branch, submission_id)` to locate an
input without scanning all input bodies. The required index is:

```sql
CREATE INDEX timeline_input_submissions
ON timeline_events (
  session_id, branch_id,
  json_extract(body, '$.record.event.data.submission_id'), sequence
)
WHERE kind = 'input/message';
```

This index belongs to the validated schema. Its introduction takes format **35**;
it is not added to v34's derived-index exemptions. A v34-to-v35 transaction creates
the index, validates the resulting schema and changes the marker atomically.
Malformed indexed input bodies fail the transaction and leave v34 intact. Existing
row/content encodings do not change. Older binaries reject v35 before normal use;
there is no silent downgrade or in-place user-store test.

The index is not unique: historical duplicate identities must remain readable so
the submission contract can report ambiguity. SQL uses the literal JSON expression
above and the input kind constraint so the planner can use the index. The Timeline
commit lock still controls duplicate admission; this read index is not a replacement
for that concurrency rule.

Acceptance uses disposable v34 stores, duplicates, malformed inputs, upgrade
rollback, reopen and an older-format rejection check. Query diagnostics must show
an indexed lookup for both a hit and a miss as the input count grows. This change
does not introduce Recall FTS tables or change text-search semantics.

## v38: session protocol

v38 rebuilds `session_owners` so `protocol_version` accepts 4. It is the
protocol-v4 connection identity stored with a Session; business tables, columns,
indexes and rows do not change. A v37 store upgrades in place; an older binary
refuses the v38 marker.

## v37: child tool-call parent

v37 adds the optional `AgentEvent.parent_call_id` envelope property and the child
tool-call batch semantics. The property names the outer `ToolCall.id` for a
child `step/tool-call`/`step/tool-result`, and the producing child call for an
asynchronous callback or background result. No SQL object changes: the marker
only. A v36 store upgrades in place; v37 is the parent-only step. A v37 store
then upgrades to v38 for protocol 4; an older binary refuses the v38 marker.
v1, v2 and v3 projections omit the property; v4 clients read it.

## v36: property text candidates

Offline import initializes the imported Session's text index inside the same
transaction as its event rows and catalog. A target already at v36 does not
rerun the format upgrade, so import must not rely on upgrade backfill. The
Session becomes visible only after these writes commit together.


v36 adds derived text-candidate metadata, a contentless FTS5 trigram index and bounded stream tails. It retains the v35 submission identity index. The public candidate semantics and indexed properties are owned by [Timeline property-query design §6C](../timeline-property-query.md#6c-文本候选查询与-fts5).

The writer updates these tables in the same transaction as the corresponding Timeline batch. Physical output merging does not merge logical event identities in the index. Fork copies rebuild from their copied prefix; deleting a Session removes its text rows, postings and tails. None of these rows is an authoritative event or a replacement for event content.

Opening v35 creates and backfills the index before committing the v36 marker. Any failure rolls back the entire upgrade. Backfill reads one physical row at a time in branch/sequence order and keeps at most 199 characters per text stream. Existing v13–v34 upgrades continue through v35 and then v36. SQLite owns the FTS virtual table's shadow schema; the format gate checks its declared virtual-table schema and the ordinary metadata/tail tables.

FTS contains no stored copy of the original text. Costs to accept are the metadata and postings on disk, one bounded overlap per text fragment, and indexing work in the append transaction. Acceptance measures database growth and append time as well as query plans, decoded bodies, SQLite memory and VM steps. A returned candidate count alone is insufficient evidence.

The v36 scalar lookup indexes use these literal expressions:

```sql
CREATE INDEX timeline_events_requests ON timeline_events(session_id, branch_id, kind, coalesce(json_extract(body,'$.record.event.data.request_id'),json_extract(body,'$.request_id')), sequence);
CREATE INDEX timeline_events_inputs ON timeline_events(session_id, branch_id, kind, json_extract(body,'$.record.event.data.input_id'), sequence);
CREATE INDEX timeline_events_users ON timeline_events(session_id, branch_id, kind, json_type(body,'$.record.event.data.user'), sequence);
```

The exact ordinary and virtual text-index objects are:

```sql
CREATE TABLE timeline_text (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL, physical_sequence INTEGER NOT NULL, property TEXT NOT NULL, partition_key TEXT NOT NULL, UNIQUE(session_id, branch_id, sequence, property));
CREATE INDEX timeline_text_partition ON timeline_text(session_id, branch_id, property, partition_key, sequence);
CREATE VIRTUAL TABLE timeline_text_fts USING fts5(text, content='', contentless_delete=1, detail=none, tokenize='trigram case_sensitive 1');
CREATE TRIGGER timeline_text_delete AFTER DELETE ON timeline_text BEGIN DELETE FROM timeline_text_fts WHERE rowid=old.id; END;
CREATE TABLE timeline_text_tails (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, request_id TEXT NOT NULL, tail TEXT NOT NULL, PRIMARY KEY(session_id, branch_id, request_id));
```

`timeline_text.id` is the FTS rowid. `sequence` is a logical event position;
`physical_sequence` points to its containing physical row. `property` is `data`
or `data.delta`; `partition_key` is an event identity for `data`, or the encoded
kind/request tuple for `data.delta`. Text is Unicode-lowercased before indexing.
Tails retain at most 199 characters per request stream and remain derived rows
after request completion, so later fragments of that identity still concatenate.
FTS shadow tables are SQLite-owned and cannot contain authoritative records.
The complete-record example above illustrates authoritative records; its derived
text rows and SQLite-owned FTS shadow rows are omitted.

Input identity joins must constrain the expression key itself, not only the
Session/branch/kind prefix. Comparing a JSON expression with a TEXT column can
prevent SQLite from using the expression key because of column affinity. The
compiled join uses `json_extract(r.body,'$.record.event.data.input_id') = +(e.event_id)`
to retain the event ID value without its column affinity. Plan acceptance requires
`<expr>=?` in `timeline_events_inputs`; merely naming that index is insufficient.

## 提案：v40 补齐逻辑事件身份与恢复成员

本轮目标格式确定为 v40；该方案已获确认，目标格式为 v40。它支持[身份与恢复操作查询](timeline-identity-reads.md)，不改变公开 query、事件或请求内容编码。不能把新表或新成员描述写入 v39 数据库；本文保留“提案”标题的既有链接锚点，不表示还需选择格式。

### 输出身份定位

普通事件继续使用 timeline_events_by_id。新增表只覆盖合并输出的逻辑事件：

```sql
CREATE TABLE timeline_output_ids (
  session_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  branch_id TEXT NOT NULL,
  physical_sequence INTEGER NOT NULL CHECK(physical_sequence > 0),
  logical_sequence INTEGER NOT NULL CHECK(logical_sequence >= physical_sequence),
  PRIMARY KEY(session_id,event_id)
);
CREATE INDEX timeline_output_ids_by_position ON timeline_output_ids
  (session_id,branch_id,physical_sequence,logical_sequence,event_id);
```

每条 step/text 或 step/thinking 逻辑事件恰有一条 locator，指向包含它的物理行；包括旧 encoding=event 的单条输出，其物理与逻辑序号相同。其它类型不登记。按位置索引用于有界验证、裁剪复制和删除，不用于扫描整条历史来实现 ID 查询。

普通身份索引与此表分别按 session_id、event_id 定位；合并位置并去重后读取物理行。逻辑身份必须仍属于本次分支范围，才可进入后续字段投影。一个 ID 不存在时，两次索引未命中即结束，不补扫输出类型。输出合并只新增片段 locator，原物理行起点不变。ID查询强制使用(session_id,event_id)主键，不让SQLite改选只匹配session/branch的覆盖索引。合并行内records连续，所选下标为logical_sequence−physical_sequence；已选单位置字段读取也直接取该元素，不展开同行其它片段。仍需读取并解析所在物理JSON行；合并在既有4KiB阈值停止追加，大单片段的成本仍按实际字节计。

### 恢复成员描述

在原 member 表中增加五种版本描述，身份、关联字段及消费者仅在[执行设计 §3](timeline-identity-reads.md#3-恢复操作的成员集合)定义。复用原 AVL 节点、根和关闭键，不添加第二套索引表。已有四种描述的身份、键编码和旧根不变。

新描述的关联键统一使用 tuple-v1：先写 4 字节无符号大端字段数量，每个字段依次写 8 字节无符号大端长度和 QueryValue.scalar_key 的字节。单字段的新描述也使用此编码；旧描述保持旧编码。字段顺序由描述固定，缺失字段不生成关联键，不能写成 null。不能用字符串拼接或原始 JSON 文本比较。

### 原子升级与校验

v39 到 v40 在原准入事务内建表、回填并检查，再更新 marker；任何阶段失败全部回滚。旧程序按原机制拒读 v40。不得先写 marker，再把回填留到后台。

输出 locator 回填只读取 step/text、step/thinking 的物理行，解析 encoding=output 的 records 或 encoding=event 的单条事件。缺失/无效身份、重复身份、错误逻辑位置均报错。新成员只回放涉及新描述的逻辑事实；不重建已有成员索引，不加载模型请求的 messages/tools/system。

普通写入、输出合并、Session 创建/复制/删除都在事实原事务内更新对应结构。fork 内部切点按逻辑序号裁剪，不能照抄父行全部 locator。升级、写入失败和复制失败均不得遗留半套索引。

升级和写入在事实事务内维护输出与locator对应；格式升级失败回滚。普通恢复不全查locator，所选数据沿实际读取边界解码；未读取的索引损坏不阻止目录恢复或真实事件导出。

验收覆盖旧格式逐级升级、回滚、旧程序拒读、合并输出命中/未命中、批次内部 through、祖先切点、fork/复制/删除及成员终态乱序；性能与写入量按执行设计计入同一份累计结果。无证据前不承诺恢复耗时或写入收益。

## 提案：状态读取索引与数组元素

本节固定 v39 的数组编码，并由 v40 原样沿用；不能把它加入已提交的 v38。基础 member 结构沿用本文件开头的 v37 决定，不重新建立第二套成员树。查询使用既有 Fields 数字下标，不新增元素行语言。

### 范围

支持[未完成成员索引](timeline-member-index.md)、[数组元素读取](../timeline-array-rows.md)和[状态结果协议](../owner-state-indexed-reads.md)。Store 不解释 Goal、队列或 token；原事件格式和公开完整事件结果不变。

### 沿用与新增结构

| 逻辑对象 | 主键/索引与内容 |
| --- | --- |
| member_roots | 主键 session、branch、index_id、group_key、sequence；值为 root_id。支持最后版本与旧 through。 |
| member_nodes | 沿用 v37 的不可变 AVL 节点及主键；引用位置和左右节点，不复制事件正文。 |
| member_origins | session、branch、index_id、关联键、原位置的反向定位，含成员分组。 |
| member_terminals | session、branch、index_id、关闭键的定位，保留最早终态位置。 |
| timeline_array_elements | 主键 session_id、array_id、element_index；值为 content_ref。下标 0 另存正整数 array_length，其余行该列为 NULL；不另建根表。事件及 trace 各自在自己的 wire 中保存数组身份和长度。 |

按提交位置读取最后结果复用既有 `timeline_events_by_kind(session_id, branch_id, kind, sequence, event_id)`，不新建 result_positions 表。schema 在选中结果后由 owner 检查；Usage 按源位置查询累计前驱使用下文v40标量索引。

index_id 是规范化的、由 Store 明确支持的查询描述身份；不是插件名或可执行回调。group_key 编码已声明的标量分组，未分组也有固定编码。member 表和节点编码沿用 v37，不在本节重新选择。唯一数组表固定为：

```sql
CREATE TABLE timeline_array_elements (
  session_id TEXT NOT NULL,
  array_id TEXT NOT NULL,
  element_index INTEGER NOT NULL CHECK(element_index >= 0),
  content_ref TEXT NOT NULL,
  array_length INTEGER,
  PRIMARY KEY(session_id, array_id, element_index),
  CHECK((element_index = 0 AND array_length IS NOT NULL AND array_length > 0)
     OR (element_index > 0 AND array_length IS NULL))
);
```

内容引用和数组身份在存储读取/校验边界验证，不新增另一套外键或根表；普通 query 不创建持久结构。

普通成员描述覆盖 input、tool-call、exec 和 collaboration 的反关联，见成员索引合同。owner 的保存与恢复方式由各自设计决定，不统一保存source_through。Usage读取已保存数字之后的未完成源，Queue直接读当前输入，Prompt读有效引用和指定scope边界；不新增源确认成员树。Store不执行业务初始化。

### 查询定位

以下是 Store 内部定位步骤，不是插件可调用的 SQL 接口。数组表使用上节已固定的名称和主键。

最后结果首先定位身份与版本，再投影所选字段：

```sql
SELECT sequence, event_id
FROM timeline_events
WHERE session_id = :session AND branch_id = :branch
  AND kind = :result_kind AND sequence > :opening AND sequence <= :head
ORDER BY sequence DESC
LIMIT 1;
```

该查询利用既有按类型索引倒序定位；再按所选位置投影 schema、source_through 与 state。缺少或不支持的 schema 都交给 owner 报错，不在 SQL 中排除后冒用更旧结果。当前公开读取不提供任意历史版本；Usage的压缩累计前驱按v40的epoch/源位置范围定位，不能将结果提交位置视为源位置。

新源的单类型定位使用相同索引和显式位置范围：

```sql
SELECT sequence, event_id
FROM timeline_events
WHERE session_id = :session AND branch_id = :branch AND kind = :source_kind
  AND sequence > :source_through AND sequence <= :source_cutoff
ORDER BY sequence
LIMIT :page_size;
```

多个源类型分别定位后按原位置合并，附加属性只在这个范围求值；SQL 页游标继续约束位置下界。已完成前缀不进入候选。捕获的 publishing tail 只合并范围内、尚未落盘的源，不能有尾部就扫描历史。普通 pending 仍先按指定 through 取得成员根，再遍历当前成员。

已知事件身份使用既有 `timeline_events_by_id(session_id, event_id)` 定位：

```sql
SELECT branch_id, sequence
FROM timeline_events
WHERE session_id = :session AND event_id IN (:id_1, :id_2);
```

这里的两个 ID 只是批量参数示例，本改造的 owner 每批至多提交 128 个，不为通用 query 增加新的 ID 数量限制。定位后先核对每个逻辑位置属于运行时传入的固定读取范围，再投影必要属性；不先读取正文再判断祖先可见性。未命中的 ID 不触发类型历史查询。合并输出中的逻辑事件沿其既有逻辑身份定位合同处理，不能把物理行起点当成所查询事件的位置。owner 以私有事实保存的数字/状态节点也使用同一身份路径，不新增节点专用读取接口；Store 内部的 pending 成员节点仍按成员索引合同读取。

Prompt 已有引用转成成对的定位键，例如：

```sql
WITH wanted(array_id, element_index) AS (VALUES (:a_array, 2), (:b_array, 7))
SELECT w.array_id, w.element_index, a.content_ref
FROM wanted w
LEFT JOIN timeline_array_elements a
  ON a.session_id = :session AND a.array_id = w.array_id
 AND a.element_index = w.element_index;
```

事件 ID 先通过既有身份索引取得逻辑位置及小体积的 wire 数组引用，并核对固定查询范围；之后只加载所选 content_ref。原下标在长度内但 locator 或内容缺失时返回损坏错误；长度外仍是原查询的缺失值。这条路径不展开未选中元素，也不读取整份原数组。所有辅助定位与内容加载都计入查询成本，不能仅统计最终 SELECT 的行数。

### 数组编码

首版对公开字段 data.changes 提供元素寻址，编码机制只处理 JSON 数组，不解释 Set/Revoke。序列化后不超过 4 KiB 的数组继续内联，避免小变更新增内容行；更大数组移出 event.data.changes。WireEvent 和 WireDraft 各增加可选 changes 引用，保存 id、length；无引用时省略该字段，旧 wire 仍可读。

array_id 是原数组 JSON 序列化的内容哈希，使用 array 内容域。每个 array-element 内容节点保存 array、index、value，并对这个完整对象计算内容哈希；locator 表登记该节点引用。单元素读取检查节点哈希及其数组身份、原下标，不能把其它数组或位置的合法节点冒作本元素。完整读取还校验数量、顺序和原数组哈希。正文只在元素节点保存一次，不另存一份完整数组。

数组身份代替来源 sequence 作为 locator 键，是为了同时覆盖 before trace 的不同草稿；这些草稿共用事件身份，却可能有不同的 changes。相同内容可共享同一个数组版本。分支位置和 property 由原事件 wire 解析，不再增加 trace 路径列或另一套草稿索引。插件仍只保存事件 ID 和原下标，不接触这些内部身份。

完整事件读取按原顺序还原数组；Fields 的数字下标路径只取选中节点。新编码下的 Any、Fields、Count、关联和完整 Events 保持原事件行语义，不新增数组元素行。不能让 inline JSON 路径把引用标记看作公共值或缺失字段。

节点的序列化、哈希、共享与损坏错误沿用内容存储的验证边界。读取某个数组引用时按主键核对下标0的长度；不在Session首开扫描所有数组的locator。所选正文在读取时解码和验哈希；导出完整重建。查询游标只含公开位置与元素身份，不含内容哈希。FTS 在写入时消费移出数组前的公开数据；升级保留已有倒排项，复制时使用原数组内容，不索引引用标记。

已知下标的 Fields 先以事件 ID 定位数组引用，再按 locator 主键逐个取本次字段所需元素。相同事件 wire 在本 query 内复用，不展开未选元素。Count/Exists 仍统计或判断事件，不被重新解释为数组长度；不增加元素分页、分组或排序执行器。

### 事务与升级

```text
接收原子事实批次
 -> 编码数组元素/已有内容节点
 -> 按逻辑顺序更新所有受影响成员根
 -> 保存事件、节点和根，维护既有事件索引
 -> 同事务提交，再执行原发布回调
```

从 v38 升级时逐批读取所需逻辑属性，保留已有普通成员与事件索引，建立新增数组编码；同一升级事务最后更新格式标记。更旧受支持格式先按既有升级合同到达 v38。owner 状态的业务初始化不由 Store 执行：缺结果的 owner 在其明确初始化流程产生第一份状态。升级失败整体回滚，报告既有升级进度，不在下次普通 query 补建。

Session 复制保留逻辑位置、身份关系和原数组顺序，在目标范围重建索引；复制节点引用必须连同节点验证，不留下跨 Session 悬挂引用。删除 Session 同事务删除其根、关系和仅属于它的内容。旧版本程序按新格式拒读，不修改旧格式标记来伪装兼容。

### 进入实现前的验收清单

一次性 v38→新格式数据库验证升级、回滚、重复打开、只读导出、Session 复制/删除及各提交阶段崩溃。query 的祖先、批次内部 through、合并输出、数组与 publishing tail 做语义对照。固定 10 个成员或 1 个数组元素，扩大历史及原数组体积，记录真实扫描、节点加载、VM、SQLite/Rust 内存和 WAL。不能只通过表结构检查就宣布性能满足。


## v40 状态字段定位补充

本节是v40格式细化，与输出locator/五种恢复描述共同升级；不适用于已提交v39。查询匹配、真实范围和缺口见[状态索引设计](timeline-state-read-indexes.md)，业务判断归各owner。

```sql
CREATE INDEX timeline_events_turns ON timeline_events(
  session_id,branch_id,kind,
  coalesce(json_extract(body,'$.record.event.data.turn_id'),json_extract(body,'$.turn_id')),sequence
) WHERE coalesce(json_extract(body,'$.record.event.data.turn_id'),json_extract(body,'$.turn_id')) IS NOT NULL;
CREATE INDEX timeline_events_scopes ON timeline_events(
  session_id,branch_id,kind,
  json_extract(body,'$.record.event.data.scope.kind'),
  json_extract(body,'$.record.event.data.scope.timeline'),
  json_extract(body,'$.record.event.data.scope.id'),
  json_extract(body,'$.record.event.data.status'),sequence
) WHERE kind='context/scope';
CREATE INDEX timeline_events_source_positions ON timeline_events(
  session_id,branch_id,kind,
  json_extract(body,'$.record.event.data.epoch'),
  json_extract(body,'$.record.event.data.current.at.branch'),
  json_extract(body,'$.record.event.data.current.at.sequence'),sequence
) WHERE kind='context-usage/recovered';
```

三个索引只服务实际等值/范围查询，不索引Prompt body或token语义。最后numeric读取仍使用kind/提交位置索引，不在SQL中跳过未知schema。源位置前驱显式固定epoch和逻辑可见范围，额外visible_head筛选成本单独报告。

原input成员的presence分组在v40同时登记handover缺失和存在两种互斥值；缺失组编码与旧根保持不变，存在组使用独立的input-handover/v1描述及presence=true分组，复用相同表/节点；这样可回填新组而不改写旧普通输入根。每个输入只进入一组，不复制为第三个全集；consumed/deleted按原input_id移除其所属组。Store matcher分别识别Not(Present(data.handover))与Present(data.handover)。

v39→v40事务内建立表达式索引并从handover输入与其终态回填存在组，保留已有普通组与旧cutoff根。所有新结构成功、格式校验通过后才写v40 marker；失败回滚。正常写入/导入/复制在原事实事务中维护，删除沿Session原事务。不得为这些字段增加plugin SQL接口或替代业务原事实。

### ContextUsage private numeric version

ContextUsage v5 changes only its private extension data: optional legacy_result and explicit unknown legacy counting metadata. Existing v4 and v1–v3 results are interpreted by ContextUsage. Native v41 is unchanged by Usage's private version: SQL schema, event envelope, content encodings and the indexed epoch/current.at properties are unchanged; no native migration or Store-side token calculation is added. The owner rejects an unsupported numeric schema explicitly. See [ContextUsage §6](../plugins/ContextUsage.md#61-统计版本与旧数据).

## Proposed v42: indexes for unfinished external work

This proposal adds no recovery snapshots and no executable progress. It extends the existing versioned member index with the following relationships, using original facts:

| Origin | Removed by | Match |
| --- | --- | --- |
| `approval/request` | `approval/respond`, `approval/cancelled` | `data.approval_id` |
| Each of `collaboration/request`, `collaboration/cancel`, `collaboration/close`, `collaboration/message` | `collaboration/delivered` | origin `id` = terminal `data.op_event_id` |
| `exec/ended` that requires a notification | tagged `input/message` | `exec-wake-result:` + origin `id` = an input tag |

The new index_id values are approval/v1, collaboration-delivery/v1 and exec-wake/v1. Existing descriptors keep their meanings.

Collaboration keeps its existing unfinished-request index for accepted delegations without a final result. The new undelivered-operation roots locate only missing delivery terminals; recovery writes Closed without repeating the operation. These are outstanding decisions/notifications, not old execution state. Completed work is absent from the current root; loading an empty root must not examine any old origin. Recovery reads current members only. Historical task output is located by task identity when requested, rather than loaded into a permanent task catalog on resume.

An exec end requires a notification only when its status is completed or failed and its original position is not in a closed generation. Compare the latest preceding branch/start, branch/resume or session/forked with the latest preceding branch-scoped stop/interrupt. A later stop suppresses that end; a subsequent resume starts a new generation. This is evaluated at the end's position, not at the current read head. An eligible end before a close remains owed if its input was never committed. An end suppressed by a close is absent from the notification root and cannot accumulate as a candidate on every resume. Loading never restores a previous generation's closing flag or process handle.

The close lookup uses a v42 partial index on session_id, branch_id and sequence, restricted to stop/interrupt with data.scope=branch; the existing kind/position index locates the opening boundaries. Updating an exec end therefore needs indexed boundary lookups, not a pass over prior interrupts. The existing exec/v1 origin and terminal identity records locate a requested historical task's start and end by data.task_id. Query execution must reuse those identities rather than scan old exec events or rely on a task ID being an FTS-safe UUID. Normal owner queries also respect the branch's local opening; copied fork history does not transfer process ownership.

```sql
CREATE INDEX timeline_events_branch_interrupts
ON timeline_events(session_id, branch_id, sequence)
WHERE kind = 'stop/interrupt'
  AND json_extract(body, '$.record.event.data.scope') = 'branch';
```

The existing schema can encode these roots, but v41 does not guarantee their contents. Therefore adding them requires v42 rather than treating absent v41 roots as empty. The upgrade builds the derived relationships in the existing upgrade transaction and switches the marker only after success. Failure rolls back. Existing v41 and both historical version families remain readable through upgrade. Old binaries refuse v42. Verification uses disposable stores.

The notification relationship requires two compatible additions to property-query operands. String concatenation expresses the literal wake-tag prefix plus the end ID. A reference to the second enclosing event expresses the close boundary: within an end, a preceding stop counts only if no opening lies between that stop and that same end. The current Outer refers only to the immediately enclosing event and cannot express the upper bound from the second nested Exists. Existing query objects remain valid and gain no required fields. The indexed root must match the complete notification predicate, including closing, in both native and memory execution; it cannot silently omit closed ends from a broader query. The store recognizes the exact indexed relationship and still evaluates unsupported query shapes correctly. No new business state API, owner database, event type or per-source recovery event is introduced.

Native upgrade cost is separate from normal loading. A v41 database cannot acquire an index over its older facts without reading the indexed facts once; this proposal does not call that work a sub-200ms normal resume. The <=200ms end-to-end acceptance uses an already indexed native database; first upgrade is reported separately.
