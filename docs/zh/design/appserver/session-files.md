# SessionStore Session 文件

状态：已实现。路线图工作项 S5a（[#59](https://github.com/arcships/dim/issues/59)、[#95](https://github.com/arcships/dim/pull/95)），在 S6（[#51](https://github.com/arcships/dim/issues/51)）之后。使用它的分叉与删除方法属于 S5c（§13）；§17 列出实现所做的变更。
总体设计：[附录 B.2](../overview.md#b2-app-server-进程插件) 的 SessionStore 与 FileStorage 行。作用域：
[ADR 002](../../adr/002-plugin-scope.md) 进程作用域，位于现有的 SessionStore 插件内。
使用方：[Attachments](../plugins/Attachments.md)（P2），以及 Session 的分叉与删除（S5c）。

## 1. 概要

SessionStore 为 Session 保存不可变文件。调用方为某个 Session 交给它一个文件名和字节；SessionStore 在该 Session 的资源节点上只写一次、以只读方式保存，并返回一个模型可以 `read` 的绝对路径。文件存续期间路径不变，因为 Session 历史会引用它，而历史从不改变。

- **位置。** 文件位于原生数据库 `<db>`（默认 `dimcode.sqlite`）旁边的 `<db>.session-files/<fileId>/<name>`。这个目录属于该数据库，不属于任何单个 Session。
- **所有权。** 所有权是控制节点原生存储中的一组行。分叉只增加行，不复制字节；删除 Session 就删除它的行。
- **移除。** 后台清扫移除最后一个所有者行已消失的文件，以及目录中没有任何行记录的东西。
- **FileStorage。** S5a 只新增一个操作 `create_directory`。共享不需要建链接的操作。
- **格式。** 原生存储升级到 15 版，新增两张表；v13 存储在启动时原地升级。
- **接口。** Attachments 通过进程服务 `sessionFiles` 使用它。
- **读取。** 模型像读其他文件一样用 `read` 读取这些文件：所有权无需授权即可满足文件范围，但不绕过工具准入（§11）。

不新增 Timeline 事件，不新增插件，不改运行时、rutis 或 Session 门面。

## 2. 结构

```
 进程作用域（控制节点）
 +--------------------------------------------------------------------------+
 |  SessionStore（现有插件）                                                |
 |    sessionFiles.put ---------------------------+                         |
 |    S5c 的分叉与删除步骤（Rust，内部）          |                         |
 |    清扫（后台）                                |                         |
 |         |                                      |                         |
 |         | 行                                   | 每次调用按 Session 的   |
 |         v                                      | 资源节点查找（#38）     |
 |  SqliteStorage: session_files,                 |                         |
 |                 session_file_owners            |                         |
 +------------------------------------------------|-------------------------+
                                                  v
 资源节点（P1/P2 即控制节点）               FileStorage: create_directory,
                                            replace, list, remove
 Session 作用域
   Attachments（Deps: SessionIdentity, sessionFiles）-- put(自身 sessionId, name, text)
```

SessionStore 仍只依赖 SqliteStorage，不依赖 FileStorage：它在每次调用时读取 Session 资源节点上的 FileStorage。这是总体设计 [§6.3](../overview.md#63-多节点) 对节点服务的要求，做法与 Rpc 读取管理方法提供方相同（[methods.rs](../../../../crates/dim-app-server/src/methods.rs) 的 `ctx.get_as`）。因此 FileStorage 重载不会让 SessionStore 以及其上的 Agents 和 Session 重载。Attachments 依赖核心部件 SessionStore；SessionStore 不指名任何使用方。

## 3. 布局与所有权

### 3.1 磁盘布局

```
<数据库目录>/                       FileStorage 根（产品配置：~/.dimcode/rust）
  dimcode.sqlite                    原生存储，含 session_files 与 session_file_owners
  dimcode.sqlite.session-files/     该数据库的 SessionStore 目录，0700
    .dim-file-storage-locks/        FileStorage 自己的资源，列目录时隐藏
    3f2a…-uuid/                     一个 Session 文件；fileId 为随机 UUID v4，0700
      .dim-file-storage-locks/
      paste.txt                     内容，只写一次，只读（0400）
    9c41…-uuid/
      notes.md
```

- 目录位于拥有它的那个数据库旁边，并以数据库文件命名，就像 SQLite 的 `-wal` 和 FileStorage 的 `.lock` 伴随文件一样。因此每个数据库都有自己的目录，即使两个数据库位于同一目录（两个 `--database` 文件）。复制数据库文件不会复制它的 Session 文件。
- 控制节点上，数据库目录是 `--home` 目录，或 `--database` 所在的目录。产品配置中它是 `~/.dimcode/rust`，而不是 `native_home` 指向的产品 home `~/.dimcode`（[lib.rs](../../../../crates/dim-app-server/src/lib.rs)）。数据库目录还不是 FileStorage 根时，由 `serve` 注册（使用 `--home` 时已经是），并把索引交给 `SessionStorePlugin`。只要 SessionStore 已加载就提供 `sessionFiles`；没有原生存储的宿主没有 SessionStore，也就没有 Session 文件。
- 返回的路径是 `root_path(root)/<db>.session-files/<fileId>/<name>`。根都是规范化的绝对路径，所以 `read` 解析到同一个文件。
- 在 P3 的资源节点上，目录以同样的名字位于该节点 FileStorage 注册的 home 根下。按节点查找属于 P3（#38）。一个资源节点只服务一个控制节点，因为总体设计 §6.3 不设计跨控制节点的使用。S5a 只实现控制节点，因为 P3 之前没有 `SessionConfig.resourceNode`；但每个文件都记下节点，P3 不需要再改格式。

Attachments 原先假设的布局，即每个 Session 根目录下的 `attachments/<attachmentId>/<name>`，改为 `<db>.session-files/<fileId>/<name>`，`attachmentId = fileId`。

目录不按 Session 划分。历史引用路径，所以只要还有 Session 拥有文件，路径就必须有效。源 Session 删除后，分叉出的 Session 仍保留源的路径。按 Session 划分的目录因此会比它的 Session 活得更久，"这个目录是不是垃圾"仍要看行。目录与 Session 无关时，行是唯一的依据。

### 3.2 所有权状态

一个文件是一行 `session_files` 加上它的目录；它的所有者是它的 `session_file_owners` 行。

```
                txn 1                        txn 2（提交）
   （无行）---------> pending -----------------------------> owned（>= 1 个所有者）
                      writer = G                                |   ^
                        |                                       |   | 分叉：复制源的
                        | 放弃：失败、取消、                    |   | 所有者行
                        | Session 已不存在（writer := NULL）    |   |
                        v                                       v   |
     pending，writer   unowned <---- 最后一个所有者行被删除（删除）
     属于已死的   ---->  |
     代次                | 清扫：FileStorage.remove(<db>.session-files/<fileId>) 成功
     （清扫认领）        v
                      （删除行）
```

- **pending：** `writer` 保存正在写入该文件的 SessionStore 实例的代次 ID G。它没有所有者，所以不会被分叉共享，也没有 Session 能看到它的路径。
- **owned：** `writer` 为 NULL，且至少有一个所有者行。
- **unowned：** `writer` 为 NULL，且没有所有者行。这是终态：所有者只由创建该文件的写入添加，或从另一个 Session 的所有者行复制而来，所以无主文件不会再有所有者。

G 是每次 SessionStore `apply` 生成的随机 UUID。卸载一个代次时，会取消它已接纳的写入，并等待每个写入提交或放弃。因此，writer 不是当前代次的 pending 行，所属的写入已不可能完成。设计并不只依赖这次等待：清扫先认领这样的行，只在 `writer` 仍是那个旧值时把它清空；txn 2 也只在自己的行仍为 G 时提交。所以旧代次的写入永远无法提交一个正被清扫移除的文件。

## 4. 分叉时共享

**决定：原生存储中的引用行。** 分叉在提交时拥有源当时拥有的全部文件。S5c 的分叉事务复制所有者行：

```sql
INSERT INTO session_file_owners (session_id, file_id)
SELECT :fork, file_id FROM session_file_owners WHERE session_id = :source
```

不复制字节，也不碰任何文件，所以资源节点离线时分叉照样成功。分叉共享源拥有的所有文件，而不只是继承前缀引用到的那些：要区分它们，就得在历史文本里扫描路径；而分叉从不引用的共享文件，代价只是分叉删除前占用的磁盘空间。pending 文件从不被共享：它的路径还没有返回，继承的历史不可能引用它。

这个机制借鉴自 containerd 的内容存储：引用记在元数据库里，租约保护进行中的写入不被回收，垃圾回收移除没有引用的内容。这里所有者行就是引用，`writer` 就是租约，清扫就是回收器。

否决的方案：

| 方案 | 原因 |
| --- | --- |
| 硬链接，每个所有者一个（`sessions/<id>/attachments/...`） | 历史只引用一个路径，所以行不通：源删除后，分叉继承的路径就不存在了，不管还剩哪些链接。链接还不能跨卷，在 FAT、exFAT 和部分网络文件系统上不可用；它又是第二套引用计数，崩溃后可能与行不一致。为此还要给 FileStorage 加建链接的操作，却得不到任何好处 |
| 内容寻址存储（`sha256/<hash>`）加所有者行 | Attachments 已决定不去重（A7），每次写入返回一个 id。相同的粘贴会共享一个路径和一个 id，文件名也会从路径中消失；与清扫相同内容的写入竞争还需要额外协调。所有者行反正都要 |
| 所有权放在资源节点上（文件旁的标记文件或 JSON 索引） | 分叉和删除需要节点在线，也无法与它们所属的 Session 行原子提交。两边之间崩溃会泄漏文件，或丢失仍在使用的分叉的文件 |
| 所有权放在单独的 SQLite 文件里 | 能保持 v13，但失去与分叉和删除的原子性：WAL 模式下，跨附加数据库的事务不是整体原子的 |
| `agent_sessions.config` 里的 JSON 字段 | `SessionConfig` 在创建时固定，归 Agents 所有（[session-access A.3](../contracts/session-access.md#a3-sessionconfig-字段)） |

## 5. 流程

下文的 `<sf>` 指 Session 文件目录 `<db>.session-files`。

### 5.1 写入

```
 调用方        SessionStore                  原生存储              FileStorage（Session 的节点）
   | put(S, name, content)                        |                          |
   |----------->| 检查 name、大小                 |                          |
   |            | 读取 S 的资源节点 ------------->| 无 S：not_found          |
   |            | 查找该节点的 FileStorage ------------------------------------> 缺失：node_unavailable
   |            | txn 1：S 存在？插入行           |                          |
   |            |   (file_id, node, writer = G) ->|                          |
   |            | create_directory(<sf>) ------------------------------------->| 同步父目录
   |            | create_directory(<sf>/<id>) -------------------------------->| 同步父目录
   |            | replace(<sf>/<id>/<name>, content, 只读) ------------------->| 同步文件与父目录
   |            | 已取消？-> 放弃                 |                          |
   |            | txn 2：S 存在且行仍为 G？       |                          |
   |            |   插入所有者 (S, id)；          |                          |
   |            |   writer = NULL --------------->|                          |
   |<-----------| {fileId, path, bytes}           |                          |
```

文件及其目录项同步完成、txn 2 提交（`synchronous=FULL`）之后才回复。Windows 没有目录同步；与 `replace` 一样，那里的持久性依靠文件同步和 NTFS 元数据日志。

**放弃。** txn 1 之后发生失败、取消（紧挨 txn 2 之前检查），或 txn 2 发现 Session 已不存在、自己的行已不再以 G 处于 pending 时，SessionStore 执行 `UPDATE session_files SET writer = NULL WHERE file_id = ? AND writer = G`。文件变为无主，并安排一次清扫。如果这条更新也失败，该行以 G 保持 pending，由下一个代次清扫。

put 作为 SessionStore 自己的任务运行：调用方不再等待就会取消它，而任务仍以提交或放弃结束，所以不会有行以存活代次保持 pending。在 P3 加入 `resourceNode` 之前，put 不读取节点：它查找控制节点的 FileStorage，由 txn 1 回答 `not_found`（D2）。

### 5.2 分叉（S5c）

```
 S5c fork(S -> F)             原生存储（一个事务）
   |-- 复制切点之前的事实，创建 F（S5c）---->|
   |-- 把 S 的所有者行复制给 F（§4）------->|
   |-- 提交 -------------------------------| F 拥有同样的文件；不碰任何文件
```

如果源已先被删除，S5c 的分叉事务找不到源，分叉失败。所有者行只在 S5c 自己的事务内复制，从不作为单独的调用，所以分叉不会缺少它的文件。

### 5.3 删除（S5c）

```
 S5c delete(S)       Agents          原生存储（一个事务）                   清扫
   | 关闭 S -------->| 部件卸载；Attachments 不再调用
   | 删除 S 的所有者行，再删除 S 的 Session 行 --->| 提交
   | 安排清扫 ------------------------------------------------------------>|
                                                     移除失去所有者的文件
```

所有者行要在 `agent_sessions` 之前删除，因为 `session_file_owners` 引用它。Session 创建回滚（`rollback_creation`）也同样删除所有者行。分叉的创建被回滚时，它的文件仍以源为所有者；除非源在此期间已被删除，那时文件变为无主并被清扫。提交后，最后一个所有者是 S 的文件变为无主，由清扫移除。这一步不需要节点：节点离线时它的行照样提交，文件以无主行的形式等待，直到清扫到达该节点。整个删除还会先在节点上移除该 Session 的命令输出（[删除与分叉 §11](session-deletion-and-fork.md#11-资源节点p3)）。

### 5.4 清扫

```
 sweep(N)：
   1 认领：对节点 N 上 writer IS NOT NULL 且 writer != G 的每一行：
             txn：SET writer = NULL WHERE file_id = ? AND writer = <该旧值>   （变为无主）
   2 移除：对节点 N 上 writer IS NULL 且无所有者的每一行：
             FileStorage(N).remove(<sf>/<file_id>)          不存在即视为已删除
               成功              -> txn：若该行仍无主，删除它
               Locked、I/O 错误  -> 保留该行，记录诊断；下次清扫重试
               节点不可用        -> 停止；下次清扫重试
   3 列目录：FileStorage(N).list(<sf>)，再读取这些名字对应的行
             没有行的每个条目 -> FileStorage(N).remove(<sf>/<name>)
```

| 触发 | 原因 |
| --- | --- |
| 一次写入放弃了文件 | 它的目录可能已存在 |
| 一次提交删除了所有者行（S5c 删除、创建回滚） | 文件可能失去了最后一个所有者 |
| 在当前 SessionStore 代次的一次清扫在某节点上完成之前，该节点上的每次写入（D3） | 崩溃或重载的代次留下的 pending 行、删除失败留下的无主行，以及因断电而重新出现的已删目录 |
| S5c 清理 | 主动回收 |

第 3 步是安全的：目录只属于一个数据库（§3.1），而写入先提交行、再创建目录。列目录返回的每个目录，到读取行的时候要么已经有行（pending 或已拥有的文件，清扫会跳过），要么从来没有行。之所以需要它，是因为目录可能没有行：断电后，已删除的目录可能重新出现，而以 `synchronous=FULL` 提交的行删除不会回来。S5c 之前 FileStorage 的删除不做同步（[S6](https://github.com/arcships/dim/pull/87)）；现在删除在返回前同步父目录，在能同步目录的主机上关闭了这个窗口。SessionStore 以外创建的条目也同样移除：这个目录归 SessionStore 所有。

每个节点同时至多运行一次清扫；清扫期间的触发会再安排一轮。清扫是 SessionStore 自己的后台工作，卸载时取消并排空；FileStorage 的删除可以重试，所以这是安全的。

## 6. 删除、崩溃恢复与串行化

**与 Session 关闭和租约的顺序。** S5c 先关闭 Session，再提交删除事务，然后 SessionStore 清扫。FileStorage 租约不会比一次写入活得更久：`replace` 只在写入期间持有租约。清扫从不移除当前代次的 pending 文件。因此删除不会因为写入而遇到 `Locked` 或 `DirectoryNotEmpty`；如果因为 SessionStore 以外的原因遇到，该行保留，下次清扫重试。

**写入与删除、分叉的串行化。** 三者都在同一个原生存储连接的写事务上做决定：txn 2 检查 Session 是否仍存在；S5c 的删除同时删除所有者行和 Session 行；分叉在创建它的事务里复制所有者行。SQLite 给这些事务排序，所以不需要内存锁：

| 竞争 | 先提交 | 结果 |
| --- | --- | --- |
| 写入与其 Session 的删除 | 删除 | txn 2 找不到 Session：`not_found`；文件被放弃并清扫 |
| 写入与其 Session 的删除 | 写入 | 删除释放该文件；除非有分叉拥有它，否则被清扫 |
| 写入与其 Session 的分叉 | 写入 | 分叉共享该文件 |
| 写入与其 Session 的分叉 | 分叉 | 只有源拥有它；分叉之后才返回的路径不可能出现在分叉的历史里 |
| 分叉与源的删除 | 分叉 | 分叉保留文件 |
| 分叉与源的删除 | 删除 | 分叉失败：没有源 |

**崩溃恢复。**

| 崩溃位置 | 留下的状态 | 恢复 |
| --- | --- | --- |
| txn 1 之后、写入之前或写入期间 | 已死代次的 pending 行；可能有目录和一个 FileStorage 临时文件 | 清扫认领该行，再移除目录（连同 FileStorage 的资源）和该行 |
| 写入之后、txn 2 之前 | 同上 | 同上；调用方没有拿到路径 |
| txn 2 之后、回复之前 | 已拥有的文件；客户端没有收到回复 | 文件留在 Session 中（不去重，Attachments A7） |
| 删除提交之后、清扫之前或清扫期间 | 无主行；不完整的目录 | 下次清扫；删除可以安全重试 |
| 清扫中，目录已删、行未删 | 无主行，没有目录 | 下次清扫：不存在即视为已删除 |
| 清扫删除目录并删除行之后断电 | 目录可能重新出现且没有行 | 下一次清扫的第 3 步 |

## 7. FileStorage 新增操作

**只新增 `create_directory`。** 共享靠行（§4），FileStorage 不需要建链接或引用计数的操作。删除、列目录和持久写入已经有了：S6 的 `remove` 和 `list`，以及带权限参数的 `replace`。S6 评审已同意在 S5a 中加入 `create_directory`。

`create_directory(path, cancel) -> ()`：让授权根目录下的 `path` 成为一个已存在的业务目录。

| 规则 | 内容 |
| --- | --- |
| 父目录 | 除最后一个之外的每个组成部分都按 `open_directory` 的方式打开：不跟随链接到达的已有目录，且不是保留目录。父目录缺失时以 `Io` `NotFound` 失败；不递归创建，调用方按层依次创建 |
| 保留名 | `path` 已经按名字拒绝保留前缀 `.dim-file-storage-` 和 `auth.json.lock`（`InvalidPath`） |
| 别名 | 在创建任何东西之前，按 `lock` 的方式检查别名。先确保父目录中的保留目录存在，这样文件系统视为等同的任何拼写都已存在。然后确保该保留目录中有 `auth.json.lock` 的锁键，并在最后一个组成部分的锁键存在时打开它；二者是同一个文件时，名字就是账户标记的别名，调用以 `InvalidPath` 失败 |
| 创建 | 最后一个组成部分不存在时，相对已持有的父目录句柄创建：Unix 用 `mkdirat`，模式 `0700`；Windows 用 `NtCreateFile` 加 `FILE_CREATE` 和 `FILE_DIRECTORY_FILE`，继承父目录的 DACL |
| 已存在 | 已存在的目录按 `open_directory` 的方式打开，所以保留目录的别名以 `InvalidPath` 失败。已存在的业务目录视为创建成功，正如 S6 的 `remove` 把不存在的条目视为已删除。其他已存在的条目（文件、链接、重解析点、特殊文件）以 `Io` 失败且不被改动；从不跟随链接 |
| 持久性 | 无论是新建还是已存在，返回前都同步父目录，所以 `Unsynced` 之后的重试能确认持久性。同步失败时返回 `Unsynced`（目录存在且可见，持久性未确认）；它的消息从"file replaced"推广到也涵盖目录。Windows 没有目录同步，与 `replace` 相同 |
| 根目录 | 不能指名根目录本身：`path` 要求相对名称 |
| 取消 | 一直检查到创建为止，创建就是提交边界；创建之后报告同步结果 |
| 错误 | `InvalidPath`、`Io`（父目录 `NotFound`、已存在的非目录条目、其他 I/O）、`Unsynced`、`Closed`、`Cancelled`、`Worker` |

别名检查从不为新目录的名字创建锁键，所以 put 不会在永不删除的 `<db>.session-files/.dim-file-storage-locks` 中留下锁键（设计变更 D1）。该操作位于 FileStorage 自己的模块内，与 `list`、`remove` 并列，使用相同的代次准入。FileStorage 的插件页记录了它。

## 8. 存储格式

**决定：15 版，新增两张表，从恰好的 v13 原地升级。** 记录在 [native-storage-format.md](../../../design/appserver/native-storage-format.md#s5a-v15-格式决定)。

```sql
CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 15))
CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)
CREATE TABLE session_file_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id),
  file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))
CREATE INDEX session_file_owners_by_file ON session_file_owners(file_id)
```

| 列 | 含义 |
| --- | --- |
| `session_files.file_id` | 小写带连字符的 UUID v4；既是目录名，也是调用方拿到的 `fileId` |
| `session_files.node` | 写入时 Session 的 `resourceNode`；NULL 表示控制节点，与 `resourceNode` 缺省的含义相同。所有者删除后它仍在，清扫据此知道去哪里移除 |
| `session_files.writer` | pending 时为代次 ID G；提交或放弃后为 NULL |
| `session_file_owners` | 每个拥有它的 Session 一行。主键服务按 Session 的分叉与删除；索引服务"已无所有者"的检查 |

行里不记文件名、大小和时间戳：没有决定要用到它们，磁盘上的文件本身也有这些信息。

**为什么不能留在 v13。** 启动要求精确的 SQL 结构（[sqlite.rs](../../../../crates/dim-app-server/src/storage/sqlite.rs)），v13 守护进程会拒绝多出表的库。格式文档只在旧读取器能解释新数据、旧写入器不会丢掉新数据、且不变量成立时保持版本；打不开库的旧守护进程不满足第一条。C2 和 S8a 留在 v13，是因为它们只在现有表内增加 JSON 字段和行。S5a 需要与 Session 行原子提交的关系（§4），没有哪张现有表拥有它们。

**为什么是 15 而不是 14。** 分支专用的 v14（`user_configuration`）曾在 e41ceaf 提交，后在 fb3748e 移除。带 14 标记的开发库可能存在，格式文档写明它们永远不会被当作 v13 解释。把 14 给另一种结构，就是给不兼容的已提交格式复用版本号，AGENTS.md 禁止这样做。

**升级路径。** 升级在现有的启动准入事务中进行（`IMMEDIATE`，写入型 PRAGMA 之前）：

| 找到的库 | 处理 |
| --- | --- |
| 空库 | 以标记 15 建立 v15 结构 |
| 结构恰为 v13、标记为 13 | 在同一事务内：以 `CHECK (version = 15)` 删除并重建 `agent_store_format`，写入 15，建两张表和索引。只替换标记行，不读取、不改写任何业务行；失败则回滚，v13 库保持原样 |
| 结构恰为 v15、标记为 15 | 打开 |
| 其他（更旧的格式、分支专用 v14、未知结构） | 照旧拒绝并保留 |

升级是单向的。v13 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。这是可以接受的：改动是增量的，而另一种做法，即拒绝所有 v13 库，会丢失用户的 Session。之后的格式各自追加从前一版本出发的一步。测试用 v13 结构的冻结副本构建一次性 v13 库，从不打开用户的库（AGENTS.md）。

**升级日志。** 打开存储时告诉调用方它做了升级，守护进程随后向 stderr 写一行：`Upgraded native store <path> from format v13 to v15; earlier dimcode versions cannot open it.`。这一行在升级提交之后写，所以不会声称一次已经回滚的升级；也只有执行升级的那次启动写它。它直接写 stderr，不作为 `tracing` 事件，因为守护进程默认的日志过滤只保留错误（[main.rs](../../../../crates/dim-app-server/src/main.rs)）。CLI 启动的守护进程的 stderr 就是数据库目录下私有的 `daemon.log`，回退旧版本的用户能在旧守护进程的拒绝信息旁找到原因。

**S5c 不需要为 Session 文件再改格式。** 它的分叉步骤只插入 `session_file_owners` 行（§4），删除步骤只删除这些行（§5.3），清扫读取 `session_files`；这些步骤读写的内容 v15 都已具备。v16 只可能来自 S5c 对它自己的 Session 记录的改动，不会来自 Session 文件。

**宿主输出是 v29 的第二张表。** [ADR 013](../../adr/013-command-output-ownership.md) 为命令输出、转存的 `read` 结果和 shell 日志新增 `session_output_owners(session_id, path)` 及其索引。它不是 Session 文件表：这些路径没有 `session_files` 行或 id，目录由最后一个拥有它的 Session 删除时移除。分叉像复制 `session_file_owners` 一样复制它的行，因此源 Session 删除后输出仍可读。

## 9. 接口

### 9.1 语言中立形式（ADR 002 D5）

`sessionFiles`，进程作用域，无实例键，由 SessionStore 提供。

| 方法 | 方式 | 请求 | 回复 |
| --- | --- | --- | --- |
| `put` | async | `{sessionId: string, name: string, content: string}`，`content` 是字节的 base64 | `{fileId: string, path: string, bytes: u64}` |
| `register` | async | `{sessionId: string, path: string}`（ADR 013） | `{}`；Session 不存在时为 `not_found` |
| `owns` | async | `{sessionId: string, path: string}` | `{owned: bool}` |
| `owned_paths` | async | `{sessionId: string}` | `{paths: string[]}`，一个 Session 文件目录及每条输出路径 |

- `path` 是 Session 资源节点上的绝对路径，与 `read` 为该 Session 解析的结果一致；`bytes` 是保存的长度。
- `register`、`owns` 和 `owned_paths` 是 ADR 013 的所有权接口。`register` 解析路径并在调用方把路径交给模型之前记录；`owns` 同时读取两张所有权表，ADR 014 把它读作写入的 Session 自有事实。
- 取消是协议动作：调用方放弃调用，或发送桥接协议的取消请求。取消在 txn 2 之前有效；之后文件已提交，回复有效。
- 错误是数据 `{code, message}`（§10）。
- SessionStore 不提供修改、重命名或移除单个文件的操作。共享和释放只发生在 S5c 的分叉和删除事务内，它们是 SessionStore 自己的 Rust 步骤，不是服务方法。

### 9.2 Attachments 如何使用

Attachments（[A.2](../plugins/Attachments.md#a2-attachmentput)）在 `Deps` 中声明 `S:SessionIdentity` 和进程服务 `sessionFiles`：这是对外层作用域核心部件的依赖（D3）。SessionStore 重载会让 Attachments 重载，它的规则检查已接受这一点。处理 `attachment.put` 时，它调用 `put(sessionId = 自身 Session, name, content = 文本的 UTF-8 字节)`，回复 `{attachmentId: fileId, path, bytes}`。Attachments 不引用 SessionStore 的任何类型；Rust 绑定见附录 A。

| `sessionFiles` 错误 | `attachment.put` 结果 |
| --- | --- |
| `not_found` | `session_closed` |
| `invalid` | `attachment_invalid` |
| `too_large` | `attachment_too_large`（Attachments 的 10 MiB 限制低于 SessionStore 的，因此不会发生） |
| `node_unavailable` | `node_unavailable` |
| `cancelled` | `service_reloading`：没有存储任何内容，且该调用是 `drain`，因此以这个结果回复 |
| `storage` | `storage_failed`，与 SessionTitle 对同一失败的映射相同（§15） |

## 10. 错误、限制与不可变性

| 码 | 何时 | 留下什么 |
| --- | --- | --- |
| `not_found` | 没有该 id 的 Session 记录，或它在 txn 2 之前被删除。与 SessionStore 元数据服务的码相同（[SessionTitle](../plugins/SessionTitle.md)） | 不拥有任何文件；放弃的目录会被清扫 |
| `invalid` | `name` 违反下面的规则 | 无 |
| `too_large` | 内容超过 16 MiB | 无 |
| `node_unavailable` | Session 资源节点上的 FileStorage 不可达；在控制节点上即 FileStorage 正在重载或已关闭。可重试 | 不拥有任何文件；放弃的目录会被清扫 |
| `cancelled` | 在 txn 2 之前被取消，包括 SessionStore 卸载，以及后一代次的清扫认领了 pending 行 | 不拥有任何文件；会被清扫 |
| `storage` | SQLite 或 FileStorage 失败，包括 `Unsynced`（持久性未确认） | 不拥有任何文件；会被清扫 |

限制及其归属：

| 限制 | 归属 | 原因 |
| --- | --- | --- |
| 单个文件至多 16 MiB | SessionStore | 一次写入是一条消息，`replace` 把内容放在内存里；这个上限约束单次调用的内存 |
| `name`：`[A-Za-z0-9._-]{1,64}`，不以点开头，不论大小写都不是 `auth.json` 或 `auth.json.lock`，并且节点的 FileStorage 接受它作为一个路径组成部分（Windows 设备名） | SessionStore | 文件名会成为 SessionStore 目录下的路径组成部分。`auth.json` 会让 `replace` 改走 FileStorage 的账户标记协议 |
| 内容规则（非空文本、10 MiB、默认 `paste.txt`） | 各使用方（Attachments A.2） | 功能的业务规则 |
| 不设每个 Session 的数量或总量配额 | 无 | 磁盘占用随 Session 删除和清理（S5c）结束；配额需要一个业务所有者，目前没有 |

**不可变性。** 在文件存续期间，路径始终指向同样的字节；文件只在清扫移除无主文件时结束。文件只写一次，并且只读：Unix 模式 `0400`，Windows 只读属性，通过 `replace` 的权限参数设置。因此 WorkspaceTools 的 `write` 和 `edit` 会以 `permission_denied` 失败：它们的提交步骤在替换前先以写方式打开目标，Windows 上还会检查只读属性。这是一道防护，不是隔离：以 root 运行的守护进程在 Unix 上不受模式限制，shell 命令仍可以修改文件，FileStorage 也只是可信的宿主存储边界，不隔离同一用户的进程。移动数据库目录会破坏历史中的绝对路径，就像移动工作区会破坏指向其中的路径一样。

## 11. 安全

- 文件只写在 `<db>.session-files/<fileId>/` 下，从不写到调用方选择的位置；`name` 是经过校验的单个组成部分。Unix 上目录为 `0700`、文件为 `0400`；Windows 继承父目录的 DACL。
- 目录只属于一个数据库，所以一个存储的清扫不会移除另一个存储的文件，即使两个数据库位于同一目录。
- 内容是用户数据，不出现在任何诊断、行或事件中；诊断只带文件 id、节点和错误。

### 11.1 读取 Session 文件

**决定：模型读取 Session 文件与读取任何文件相同。Session 自有文件是固定事实之一，所以读取通过文件范围检查且不记录授权。工具准入遵循预设与工具列表。**
[Permission 合同](../../../architecture/rust-agent/plugins/Permission.md#policy-contract)拥有两个闸门与旧行为；
所有权不是显式工具允许。[ADR 014](../../../adr/014-file-access-facts.md) 根据工作区、系统目录、Session 自有文件和授权决定文件读写；[ADR 013](../../../adr/013-command-output-ownership.md) 增加第二张所有权表，所以 `owns` 也回答命令输出。

```
 model: read {path: "<database directory>/<db>.session-files/<fileId>/paste.txt"}
   |
   v
 ToolExecutor -> Approval.admit -> Permission.decide(tool "read", risk "read_filesystem", path)
                                   resolve (~, cwd, .., symlinks), then fileAccessFacts(read)
   "read" in disallowedTools ---------------------------> Deny, for every path
   Read only or Workspace read/write, inside facts -----> pass file scope
   Read only or Workspace read/write, outside facts ----> scope Ask
   Unrestricted ----------------------------------------> pass file scope
   tool gate -------------------------------------------> preset risk rules and tool lists
   scope Ask + tool Ask --------------------------------> one approval (exec denies and exits 3)
   |
   v
 WorkspaceTools read: the same canonical target, refuses binary files
```

准入合同由 [permission.rs](../../../../crates/dim-agent-capabilities/src/permission.rs) 的 `decide`、[file_access.rs](../../../../crates/dim-agent-capabilities/src/file_access.rs) 和 [approval.rs](../../../../crates/dim-agent-capabilities/src/approval.rs) 的 `admit` 拥有：

- **自有文件不需要范围询问。** `SessionFiles.owns` 读出本 Session 的附件行和 ADR 013 的输出行，所以 `put` 返回的路径和登记的输出路径在事实中 `owned` 为真。在 `read-only` 和 `workspace-read-write` 文件访问下它位于事实之内。读取不记录授权；工具准入仍遵循预设和工具列表。§11.2 的两条性质对 `owns` 读出的行成立。
- **事实之外的读取需要询问。** 解析后的路径既不是工作区、系统目录、Session 自有文件，也没有授权覆盖时，Read only 和 Workspace read/write 文件访问下需要范围询问（[ADR 014 D2/D4](../../../adr/014-file-access-facts.md#d2-read-and-write)）。CLI 提供 `Allow for this session`，为解析后的路径写入 read 授权；`exec` 以普通拒绝作答、停止回合并以 3 退出。Unrestricted 通过读取范围检查，不回答工具闸门。路径检查在工具列表和 `allowedTools` 之前，显式允许的工具不能绕过路径事实。`grep` 或 `glob` 没有 `path` 时搜索 Session cwd，也就是工作区。
- **授权。** 覆盖解析后路径的 read 或 write 授权满足读取范围，不能绕过工具准入，也不能把 read 授权变成写入权限。策略变更（`session.permission.update`，写成 `permission/changed`）包含策略值，从不包含路径。FileStorage 根和记住的决定都不授予读取权限。
- **FileStorage 根不授予任何东西。** 把数据库目录登记为 FileStorage 根（§3.1），只是让 SessionStore 能通过 FileStorage 在那里写入。`read` 从不查询 FileStorage 根；Session 自有事实来自 `sessionFiles.owns`，不是文件根授权。
- **不是新情况。** WorkspaceTools 已经把超长的 `read` 结果和 `exec` 输出写到 `<数据库目录>/command-output/<sessionId>/`，并把路径交给模型（[read.rs](../../../../crates/dim-agent-capabilities/src/workspace_tools/files/read.rs) 的 `offload`）。在产品配置下，这两个目录都紧挨着 `dimcode.sqlite`。ADR 013 在结果把路径交给模型之前，把这些路径和持久 shell 的 `.dim/exec/<sessionId>/<branchId>/` 日志登记进 `session_output_owners`。登记是可信部件之间的防护，不是读取规则：`read` 仍然不为这些路径询问。

```
 ~/.dimcode/                            product home: auth.json and others
   rust/                                database directory, a FileStorage root
     dimcode.sqlite                     native store; read refuses .sqlite as binary
     command-output/<sessionId>/        WorkspaceTools output; owned by this Session
     dimcode.sqlite.session-files/
       <fileId S owns>/paste.txt        path returned by a put for S
       <fileId T owns>/notes.md         Session T's file, unowned here
```

**Permission 不做什么。** 事实决定的是工具准入，不是隔离（[Permission](../../../architecture/rust-agent/plugins/Permission.md)，ADR 001 的 "Trust and cross-language calls" 一节）。它们不限定进程挂载（由 ADR 010 和 ADR 012 拥有），也不阻止路径解析之后的并发文件系统变更。S 的模型仍可以通过请求读取 T 的文件并由用户批准、使用用户写入的授权、在 `full-access` 下，或通过挂载允许的进程读到它。`read` 拒绝 `dimcode.sqlite`，只是因为它的二进制防护拒绝 `.sqlite` 扩展名，与这些事实无关。S5a 不扩大这个边界，也不为自己的目录开例外去收窄它：通用所有权事实已经覆盖 `put` 返回的路径。S5a 在构造上保证的是：

- `put` 返回的每个路径都是 `<db>.session-files/<fileId>/<name>`，其中 `fileId` 是随机 UUID v4，`name` 是一个经过校验的组成部分。返回的路径不会指向数据库、它的伴随文件或数据库目录中的其他条目。
- SessionStore 只把路径返回给一次 `put`，该文件随即归这次 `put` 所针对的 Session 所有；分叉拥有源拥有的每个文件，所以它继承的历史中来自 `put` 的路径仍归它所有（§4）。

### 11.2 对按路径限定的文件访问的需求

无论哪种机制按路径限定进程内的文件访问，无论针对工具还是进程，都必须保持下面两条性质。[ADR 014](../../../adr/014-file-access-facts.md) 为文件工具实现了它们：在准入前解析 `read`、`grep` 和 `glob`，并查询 `SessionFiles.owns`。F2（[#73](https://github.com/arcships/dim/issues/73)）负责命令规则，F5（[#76](https://github.com/arcships/dim/issues/76)）在启动时隔离进程。这是文件访问的通用需求，不是 Session 文件的例外：WorkspaceTools 的命令输出也是同样的形态，即 Session 自己的能力在工作区以外按路径交给模型的数据。

1. **拥有的文件无需范围审批。** 所有权满足读取范围，不记录授权；它不回答独立的工具审批问题（§11.1）。这样的读取不派生任何授权。
2. **由所有权而不是位置决定。** 在决定访问时，或在隔离进程启动时，Session S 可以读取 SessionStore 的行中 S 拥有的每个文件的 `<db>.session-files/<fileId>/`。这个集合从不覆盖整个 `<db>.session-files/`、数据库目录或产品目录；只有 S 拥有另一个 Session 的文件时才包含它（分叉拥有源的文件）。

SessionStore 从两张所有权表回答"S 拥有哪些文件"（ADR 013）：Session 文件来自 `session_file_owners`，其主键 `(session_id, file_id)` 服务这个查询；登记的命令输出来自 `session_output_owners`。`SessionFiles.owns(session_id, path)` 就是这个查询；[ADR 014](../../../adr/014-file-access-facts.md) 把它作为读写共用的 Session 自有事实。`owned_paths(session_id)` 列出拥有的 Session 文件目录和输出路径。

被否决的方案：

| 方案 | 为什么不用 |
| --- | --- |
| 把 Session 文件目录登记为 `read` 接受的只读根 | `read` 不查询任何根，今天什么都不会改变；文件根授权也不得授予工具权限（ADR 003 D5，AGENTS.md 第 11 条）。而且一个根覆盖整个目录，即所有 Session 的文件 |
| Attachments 把文本放进上下文，或用专门的工具按 `fileId` 读取 | 内联会在提交时把最多 10 MiB 放进上下文，而 Attachments 正是为避免这一点而存在。第二个读取工具重复 `read`（偏移、编码、卸载），路径仍然可以被 `read` 读取 |
| 现在就给 Permission 加一条"Session 可以读取自己的文件"规则 | 它会成为路径无关的 Permission 里的 Session 文件特例，豁免不了其他同类数据。它的通用形式就是 §11.2 的自有事实，由 [ADR 014](../../../adr/014-file-access-facts.md) 实现 |
| `read` 拒绝数据库目录，Session 文件除外 | 在 WorkspaceTools 里为一个 App Server 目录开例外。它覆盖 `dimcode.sqlite`（已经作为二进制被拒绝），却覆盖不到 `auth.json` 和产品目录的其余部分 |

## 12. 时序与冲突

| 场景 | 结果 | 由谁决定 |
| --- | --- | --- |
| 写入、分叉与删除的竞争 | 见 §6 的表 | 原生存储的写入顺序 |
| 对已关闭但未删除的 Session 写入 | 保存成功：记录还在。Attachments 本身随 Session 一起卸载 | — |
| 同一 Session 的两次写入 | 两个独立的文件 | — |
| 写入时节点离线 | `node_unavailable`；什么都不保存；不重载任何东西 | SessionStore |
| 分叉时节点离线 | 不受影响：不碰任何文件 | 原生存储 |
| 删除时节点离线 | Session 文件这一步无需节点即可提交；无主行等待清扫。整个删除见[删除与分叉 §11](session-deletion-and-fork.md#11-资源节点p3) | 清扫 |
| 写入期间 FileStorage 重载 | `node_unavailable` 或 `storage`；放弃并清扫 | SessionStore |
| 写入期间 SessionStore 重载 | 在 txn 2 之前取消（放弃），或已提交；Attachments 随之重载 | SessionStore 卸载 |
| 进程重启 | 遗留物在第一次写入时或下一次删除之后被清扫 | 清扫 |
| SessionStore 以外的东西写入 Session 文件目录 | 顶层条目由清扫的第 3 步移除；某个文件目录内的条目在该文件无主时随目录移除。遇到 `Locked` 则留给下次清扫 | FileStorage、清扫 |
| 最后一个所有者删除后 `read` | 文件不存在；没有存活的 Session 引用该路径 | — |

## 13. 对其他文档的修改

| 文档 | 本 PR 中的修改 |
| --- | --- |
| [Attachments](../plugins/Attachments.md) | 布局改为 `<db>.session-files/<fileId>/<name>`，`attachmentId = fileId`；A1 细化为不属于任何单个 Session 的目录；分叉与删除顺序的行指向本文；离线相关的行不再依赖已撤回的 #5，因为 SessionStore 每次调用时查找 FileStorage（#38）。`attachment.put` 的客户端契约在本 PR 中不变；它的 `storage_failed` 码在 P2 的设计复核中加入（§15）。§7 不再说分叉只读取其历史引用的文件，改为指向 §11 说明读取如何准入 |
| [总体设计 B.2](../overview.md#b2-app-server-进程插件) | FileStorage 行增加目录创建 |
| [原生存储格式](../../../design/appserver/native-storage-format.md) | 计划中的 v15 决定 |

S5c 在 §5.2、§5.3 和 §5.4 的基础上实现 `session.fork` 和 `session.delete`，并通过 RPC 覆盖它们。S5a 实现这些步骤，并在 SessionStore 的 Rust API 上测试。

## 14. 规则检查

| 规则 | 回答 |
| --- | --- |
| 原则 §4 第 1 条（改别人的状态用指令） | Session 文件是 SessionStore 没有 Timeline 的进程级状态。调用方只能通过它的类型化 `put` 修改；分叉和删除是 SessionStore 自己的步骤。SessionStore 不修改其他模块的状态；它使用 FileStorage 的公开操作。 |
| 原则 §4 第 2 条（交接） | 不适用：没有交接。路径只会出现在客户端写入的用户输入中，由此进入分支。 |
| 原则 §4 第 3 条（before 回调） | 不使用 before 回调。文件名和大小检查是所有者在自己服务上的功能检查，不是入口检查；Rpc 不变。 |
| 原则 §4 第 4 条（核心不带业务含义） | Timeline、Dispatcher、before 和批次都不动。 |
| 原则 §4 第 5 条（批次） | 不适用：没有事件。 |
| 原则 §4 第 6 条（上下文来自交接后的事件） | 模型只在已采用的用户输入里看到路径，只在 `read` 结果里看到文件内容。 |
| 原则 §4 第 7 条（只用公开的事件和服务） | Attachments 使用 `sessionFiles`；SessionStore 使用 FileStorage 的公开操作，不读任何插件的内部状态。 |
| 原则 §4 第 8 条（向机制所有者提需求） | 不改 Timeline、Dispatcher、事件格式、rutis、装配或 Session 门面，也不向 rutis 提需求。格式变化属于 SessionStore 自己的存储，记录为格式决定。`create_directory` 是 FileStorage 在自己模块内的补充，已在 S6 评审中同意。Permission 通过固定事实准入文件工具的读写（§11.1）；§11.2 是它实现的通用需求，不是 Session 文件的例外。 |
| 原则 §4 第 9 条（只有后续决定要用的事实进 Timeline） | 没有 Timeline 事件。所有权是 SessionStore 行中的进程级状态。 |
| 原则 §1（Capability 与 App Server 边界） | Session 文件是 SessionStore 的数据（总体设计 B.2）；不新增插件。粘贴功能留在 Attachments 自己的 crate 中。 |
| ADR 002 D1（作用域跟随状态） | 进程作用域：文件比写入它的 Session 活得更久，并在 Session 之间共享。 |
| ADR 002 D2（不设接线模块） | `sessionFiles` 是 SessionStore 自己的服务；没有适配器或注册表。 |
| ADR 002 D3（依赖跟随作用域） | Attachments（可选，Session）依赖 SessionStore（核心，进程）；SessionStore 不依赖任何可选插件，并按资源节点逐次查找 FileStorage，所以 FileStorage 重载不会引起任何重载。 |
| ADR 002 D4（结构性配置与业务配置） | 没有配置：位置和限制都是固定的。 |
| ADR 002 D5（语言中立合同） | §9.1 是合同；附录 A 是一种绑定。 |
| ADR 003（审批范围） | 读取 Session 文件是一次工具调用，像任何 `read` 一样由 Approval 和 Permission 准入（§11.1）。SessionStore 登记的 FileStorage 根不授予任何权限（D5）。 |

## 15. 向机制所有者提出的需求

不向运行时或 rutis 提需求。对其他所有者的补充和需求：

| 对象 | 需求 | 状态 |
| --- | --- | --- |
| FileStorage | `create_directory`（§7） | 在 S5a 的实现中加入，已在 S6 评审中同意 |
| App Server 装配（lib.rs 中的 `serve`） | 数据库目录还不是 FileStorage 根时注册它，并把索引交给 `SessionStorePlugin`（§3.1） | S5a 实现中的几行改动，像其他登记一样在装配线的工作之后追加 |
| Attachments（P2 设计细化） | 把 `sessionFiles` 的 `storage` 映射为 `attachment.put` 的某个码。Attachments 目前没有；SessionTitle 把 SessionStore 的 `storage` 映射为 `storage_failed` | 已决定：`storage_failed`，记录在 [#66](https://github.com/arcships/dim/issues/66)；在 P2 的设计复核中落实 |
| S5c | 在删除事务内、关闭 Session 之后、删除 `agent_sessions` 之前删除所有者行；在分叉事务内复制所有者行；删除后安排清扫 | S5c 的合同（§5.2、§5.3） |
| 首先按路径限定文件访问的那一项（进程方面为 F5 [#76](https://github.com/arcships/dim/issues/76)；进程内文件工具为 [ADR 014](../../../adr/014-file-access-facts.md)） | §11.2：Session 自有文件无需范围审批；由所有权决定，从不是整个 Session 文件目录、数据库目录或产品目录。工具审批仍独立 | S5a 不需要。ADR 014 为读写读取 `sessionFiles.owns`；§11.1 按路径准入文件工具读取 |

## 16. 验收（流程测试）

1. `put` 返回绝对路径 `<数据库目录>/<db>.session-files/<fileId>/<name>`；读回的字节与内容相同，文件只读，Unix 上其目录为 `0700`。
2. 第二个 Session 通过分叉步骤共享文件。释放源后文件仍在（同一文件身份，不复制字节）；释放第二个 Session 后，清扫将其移除。
3. 对未知或已删除的 Session 写入：`not_found`；没有所有者行；若已创建目录，则被清扫。
4. 另一个代次的 pending 行及其目录，在新代次第一次写入时被清扫，没有行的目录也一样。同一目录下第二个数据库的 Session 文件目录保持不动。
5. FileStorage 未激活：`node_unavailable`；什么都不写；SessionStore 和 Attachments 保持加载。
6. 在 txn 2 之前取消：不拥有任何文件；目录被清扫。文件名无效（包括 `AUTH.JSON`）或内容超过 16 MiB：`invalid` 或 `too_large`；什么都不写。
7. 空库建为 v15。一次性 v13 库原地升级为精确的 v15 结构，所有业务行不变，再次打开时为 v15。执行升级的那次打开报告升级，§8 的日志行由此而来；再次打开不报告。分支专用 v14 和未知结构被拒绝并保留。
8. FileStorage 流程：`create_directory` 以 `0700` 创建并同步父目录；已存在的目录成功；父目录缺失以 `NotFound` 失败。保留名、保留目录的大小写别名，以及（在大小写不敏感的文件系统上）`auth.json.lock` 的大小写别名，都以 `InvalidPath` 失败且不创建任何东西。已存在的文件或链接被拒绝且不改动。创建前取消则什么都不创建。
9. 读取准入，经由 Session 的工具路径：`put` 之后，在产品默认的 `workspace-write` 和 `read-only` 下，该 Session 对返回路径的 `read` 返回内容；分支不写 `approval/request`，它的权限状态（版本和策略）不变。拒绝读取另一个 Session 的文件或数据库目录属于 §11.1 事实之外读取询问的验收：两者都不在该 Session 的事实中。

测试：第 1、3（未知 Session）、5、6（名字、大小、已取消的调用）、4、9 项是经由 `sessionFiles` 的流程测试，位于 [session_files_flow.rs](../../../../crates/dim-app-server/tests/session_files_flow.rs)；第 2 项，以及写入与 txn 2 之间的第 3、6 项，在 [files/tests.rs](../../../../crates/dim-app-server/src/storage/files/tests.rs) 中运行 S5c 的分叉步骤和该处的钩子；第 7 项是 [storage_flow.rs](../../../../crates/dim-app-server/tests/storage_flow.rs) 中的 `a_v13_store_upgrades_in_place_once`、`the_daemon_reports_a_v13_upgrade_on_stderr_once` 和拒绝用例；第 8 项是 [file_storage_flow.rs](../../../../crates/dim-agent-capabilities/tests/file_storage_flow.rs) 中的 `directory_creation_stays_inside_the_authorized_root`。

验证：`./dev check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`，以及先 `cargo run --locked -p xtask -- graph` 再 `check`，因为 SessionStore 提供了新服务。

## 17. 实现中的设计变更

实现对本设计所做的变更（[路线图 §5.3](../../../design/roadmap.md#53-实现中发现的变更)），都只影响 S5a。

| # | 变更 | 原因 |
| --- | --- | --- |
| D1 | `create_directory` 确保 `auth.json.lock` 的锁键存在，对新名字的锁键只打开、不创建，不再带创建地打开两个锁键（§7） | 带创建地打开锁键会留下以目录命名的锁键文件，而 FileStorage 只在删除其父目录时才删除锁键。`<db>.session-files` 永远不会被删除，所以每次 put 都会在那里永久留下一个锁键。已存在的账户标记别名仍会解析到标记的锁键，检查结果不变 |
| D2 | P3 之前，put 不读取资源节点。Session 是否存在由 txn 1 判定，它在 FileStorage 查找之后，所以 FileStorage 未激活时，未知 Session 得到 `node_unavailable`（§5.1） | `SessionConfig.resourceNode` 还不存在，txn 1 之前的读取只会重复 txn 1 的检查。P3 在查找之前加入节点读取 |
| D3 | 在本代次的一次清扫在该节点上完成之前，每次 put 都安排清扫，而不只是第一次 put（§5.4） | 否则因节点不可用而中止的第一次清扫，要等到放弃或释放才会重试；完成一次清扫后便不再多发请求 |

## 附录 A. Rust 绑定

放在 `dim-agent-capabilities::host`，与其他宿主端口并列，这样 Attachments 不依赖任何 App Server 类型：

```rust
pub struct StoredSessionFile { pub file_id: String, pub path: PathBuf, pub bytes: u64 }
pub enum SessionFileErrorCode { NotFound, Invalid, TooLarge, NodeUnavailable, Cancelled, Storage }
pub struct SessionFileError { pub code: SessionFileErrorCode, pub message: String }
pub trait SessionFilesService: Send + Sync + 'static {
    fn put(&self, session_id: String, name: String, content: Vec<u8>, cancel: CancellationToken)
        -> Pin<Box<dyn Future<Output = Result<StoredSessionFile, SessionFileError>> + Send + '_>>;
    fn register(&self, session_id: String, path: PathBuf)
        -> Pin<Box<dyn Future<Output = Result<(), SessionFileError>> + Send + '_>>;
    fn owns(&self, session_id: String, path: PathBuf)
        -> Pin<Box<dyn Future<Output = Result<bool, SessionFileError>> + Send + '_>>;
    fn owned_paths(&self, session_id: String)
        -> Pin<Box<dyn Future<Output = Result<Vec<PathBuf>, SessionFileError>> + Send + '_>>;
}
pub struct SessionFilesHost(pub Arc<dyn SessionFilesService>);
```

`register` 保存解析后的输出路径；生产者已按自己的根检查过它。`owns` 接收解析后的绝对路径并读取两张所有权表：`session_output_owners` 的精确行，或路径位于 `<db>.session-files/<fileId>/` 内且 `session_file_owners` 拥有该 fileId。查询失败是错误，会导致拒绝访问。`owned_paths` 列出拥有的 fileId 目录和输出路径。

`SessionStorePlugin` 接收数据库目录的根索引，提供 `SessionFilesHost`，并在每次调用时用 `ctx.get_as` 读取 `FileStorage`。分叉、释放和清扫步骤是 SessionStore 基于原生存储连接的方法，供 S5c 使用。

FileStorage：

```rust
impl FileStorage {
    pub async fn create_directory(self: &Arc<Self>, path: &FilePath, cancel: &CancellationToken)
        -> Result<()>;
}
```
