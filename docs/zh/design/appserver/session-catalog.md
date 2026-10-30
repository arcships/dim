# 客户端 Session 目录

Issue：[#556](https://github.com/arcships/dim/issues/556)，要求有界的 `/resume` 首页，以及不重复解码完整有效上下文替换的目录元数据；[#557](https://github.com/arcships/dim/issues/557) 加入本合同为之预留位置的分类过滤。总览：[附录 B.2](../overview.md#b2-app-server-进程插件)的 Rpc 与 SessionStore 行。范围：[ADR 002](../../adr/002-plugin-scope.md) 的进程作用域，在既有 Agents 与 Rpc 内建方法内，新增一个 Rpc 内建方法。使用方：经 `session.catalog` 与 `session.summaries` 列目录的客户端，首先是 CLI 的 `/resume` 选择面板。规则：[设计规则](../../../architecture/rust-agent/principles.md)、[ADR 002](../../adr/002-plugin-scope.md)。

## 1. 概要

客户端用 `session.summaries` 列 Session，而这个回复是从历史构造的：App Server 为每个 Session 读起始于前 256 个事件内的批次找预览，再整条读最后一个事件取时间，一个接一个，回复一个没有页边界的数组。导入 Session 的最后一个事件是一个完整的 `context/compaction`，所以一次列目录会解码整个目录的历史，客户端还要排序并转换全部内容才能画出一行。

目录读取改为有界的工作。SessionStore 保存一份列目录所需的投影——每个 Session 的最后活动时间、作为预览的第一条用户输入、以及工作区——由创建事务写入，之后在提交 `main` 每个批次的事务里写入。新的内建方法 `session.catalog` 分页读取这份投影：缺省 50 行、不透明的键集游标，`query`、`cwd` 与 `withPreview` 在页边界之前的查询里应用。`session.summaries` 保留合同并读同一份投影，因此两个方法都不读历史。

- **投影。** 归 SessionStore，随它的批次、并从创建起写入；本页只拥有目录如何被读取（§4）。
- **方法。** `session.catalog {limit?, cursor?, query?, cwd?, withPreview?} -> {sessions, nextCursor?}`（§3.1）。
- **无租约。** 列目录不装载、不恢复任何 Session，也不启动模型、工具或 MCP 服务器（§3.4）。
- **客户端。** 选择面板分页、在服务端搜索，并放弃未完成的读取（§6）。

## 2. 范围与非目标

本页拥有 `session.catalog` 的回复合同、`session.summaries` 的目录读取、顺序、游标与页边界语义，以及 #557 的过滤生效的位置。

不拥有：

- 投影的表、索引、写入事务、删除与分叉维护、升级回填——这些由[原生存储格式](../../../design/appserver/native-storage-format.md#556-v23-格式决定)决定；
- 分类字段、它们的缺省值与呈现——[#557](https://github.com/arcships/dim/issues/557)；
- 选择面板的交互与 project/all 开关——加入它们的那个客户端（§6）；
- `session.list`、`session.attach`，以及客户端如何排序或呈现行。

## 3. 自有合同

### 3.1 `session.catalog`

`session.catalog {limit?, cursor?, query?, cwd?, withPreview?} -> {sessions: SessionSummary[], nextCursor?: string}`

| 项 | 规则 |
| --- | --- |
| 形态 | Rpc 内建方法。Agents 像回答目录读取那样，从目录投影联结 `agent_sessions` 行与标题元数据来回答；Rpc 校验信封 |
| 协议 | v3。v1 与 v2 回复 `unknown_method`（"Method requires protocol v3"），与 `session.summaries` 相同 |
| 发现 | `hello.features` 只在原生存储打开时列出它，条件与 `session.fork` 相同，因此看不到它的客户端继续用 `session.summaries`。没有存储就没有可读取的投影，`session.summaries` 与今天一样从已装载的 Session 回答（§3.3） |
| `limit` | 1 到 200，缺省 50。更大的值被拒绝而不是截断，与 `history` 的 `limit` 相同 |
| `cursor` | 不透明。它编码上一行返回的 `(updatedAt, sessionId)` 与请求的 `query`、`cwd`、`withPreview` 的摘要，按键集分页；调用方原样回传，它不是 Session ID |
| 顺序 | `updatedAt` 降序，再 `sessionId` 升序 |
| `query` | 对今天选择面板搜索的四个字段——Session ID、工作区、model ID 与预览——加上它的搜索文本从不持有的标题与 provider ID，做大小写不敏感的字面子串匹配。查询时由一个存储注册在连接上的 SQL 函数按 `str::to_lowercase` 把查询与每个字段做 Unicode 小写化（与选择面板相同的折叠）；匹配是逐字段的 `instr(fold(field), fold(query)) > 0`，因此是字面的，`%` 与 `_` 只匹配它们本身。它在目录查询里、页边界之前应用。模型取自存储的 Session 配置，即 `session.list` 报告的值，因此查询不解码任何事件 |
| `cwd` | 工作区精确匹配，在页边界之前应用 |
| `withPreview` | 布尔，缺省 `false`。`true` 只保留预览不为 NULL 的行，在页边界之前应用，因此隐藏无预览行的客户端拿到的是整页它显示的行；投影按预览建的局部索引让这条读取保持有界（[原生存储格式](../../../design/appserver/native-storage-format.md#556-v23-格式决定)） |
| 回复 | `SessionSummary` 条目，即 `session.summaries` 返回的形态：`session.list` 记录的字段加上 `preview` 与 `updatedAt`。最后一页不带 `nextCursor` |

读不出的 `cursor`，或摘要与请求的 `query`、`cwd`、`withPreview` 不符的 `cursor`，被拒绝为 `invalid_params`，而不是当作第一页，客户端因此不会在不知不觉中从头再读一遍目录，也不会用另一组过滤的键接着分页。游标不带库的标记：来自另一个库的目录的游标无法分辨，也不需要分辨，因为它只按同一个顺序定位一个键。

很少命中的搜索扫描目录的行：过滤只读投影、存储的标题与存储的模型，不解码任何事件。注册这个函数需要 rusqlite 的 `functions` feature，而不是新的依赖。折叠在查询时进行，因此没有折叠值落盘，也没有折叠值需要与标题或模型保持同步。

### 3.2 顺序、游标与分页

顺序是投影的活动时间，最新在前，Session ID 决定并列的次序，因此一页是某个全序里连续的一段，同一个库总是以同样的方式分页。游标是上一行的键而不是位置：两页之间变得活跃的 Session 移到第一页，后续页既不重复也不跳过任何其他 Session。它还带着取得它时的过滤（§3.1），因此一条页边界只能由同样的 `query`、`cwd` 与 `withPreview` 接着读。

### 3.3 `session.summaries`

`session.summaries` 保留参数、回复类型与按连接判定的协议 v3 规则。原生存储打开时，它改为从同一份投影回答而不是读历史，内容不变：投影原样应用下面的预览规则，而且它的行从 Session 创建起就存在（§5）。没有存储的守护进程没有投影，与今天一样从已装载的 Session 回答；它也不列出 `session.catalog`（§3.1）。所以找不到 `session.catalog` 的客户端靠它列目录，比本变更更旧的守护进程上的客户端也靠它；两者继续可用，因为该方法的形态不变。

**预览保持今天的规则，包括按批次完整的窗口。** 窗口是起始于 `main` 前 256 个事件内的批次，且与今天的读取一样按批次完整：承载第 256 个事件的批次整批读入，因此该批次里紧接 256 之后的用户输入也在窗口内。窗口事件里第一条带用户部分的 `input/message` 决定预览：该部分的文本块以换行连接，截到 120 个字符，更长时以 `…` 结尾。这个决定是最终的——第一条用户消息的文字为空时（例如只有图片）`preview` 为 NULL，之后的消息不再填它；窗口里没有这样的消息时也保持 NULL。投影对实时写入和回填应用同一条规则：批次只在行的预览仍未决定、且决定它的消息在窗口内时才写 `preview`，一旦决定就不再改变。因此回填行与实时行逐列相同（[原生存储格式](../../../design/appserver/native-storage-format.md#556-v23-格式决定)）。去掉或收窄这个窗口是对 `session.summaries` 所列内容的另一次可见变更。

### 3.4 无租约

目录读取从不装载、恢复或启动 Session：存储的字段由投影回答，因此列目录不花任何模型、工具或 MCP 启动。`running` 与 `attachedClients` 取自进程的内存状态，与 `session.list` 相同：已卸载的 Session 读作未运行，两个字段都不落盘，因此读取它们不取租约、也不装载任何东西。

### 3.5 目录过滤（#557）

层级与来源过滤是同一条目录查询的条件，在页边界之前应用，因此后面页上的匹配不会被已取的页挡住。[#557](https://github.com/arcships/dim/issues/557) 定义它们的字段与缺省值。本合同保留它们生效的位置，并要求有过滤时行的位置、游标与任何计数在多次刷新之间保持一致。

### 3.6 取消读取

`session.catalog.cancel {requestId} -> {}` 取消同一连接上尚未完成的 `session.catalog` 或 `session.summaries` 读取。不存在、已完成或非目录请求均为空操作。协议 v3 与原生目录一起公布该方法；它不能取消其他连接或写请求。被取消的读取返回 `cancelled`，已经完成的结果不变。

连接关闭也取消其读取。SessionStore 在排队读取执行前检查取消，并中断正在执行的 SQLite 目录查询。共享连接释放前移除该查询的取消钩子，避免中断后续写入。RPC 的所有权持续到 SQL 操作结束。

## 4. 事件与状态

本部分不写事件，也不拥有状态。投影是派生状态：每一列都由 Timeline 已有的事件蕴含，升级时按它们为整个库重建一次（[原生存储格式](../../../design/appserver/native-storage-format.md#556-v23-格式决定)）。这里没有任何东西进入 Timeline：目录是给选择面板显示的数据，[设计规则 §4 第 9 条](../../../architecture/rust-agent/principles.md)把它挡在 Timeline 之外。

## 5. 依赖

| 依赖 | 本部分要求什么 |
| --- | --- |
| SessionStore 目录投影 | 每个 Session 一行，带最后活动时间、预览与工作区：创建事务以创建时间和空预览写下它，之后每个 `main` 批次的事务维护它，不存在的 Session 没有行。这一行从创建起就与 `agent_sessions` 一一对应，因此新 Session 的列出与今天的 `session.summaries` 相同 |
| [标题元数据](session-title-metadata.md) | 存储的标题，用于显示与 `query`，从不从 Session 读取 |
| `hello.features`（App Server） | 客户端发现目录所用的方法清单：目录在列出 `session.summaries` 的条件下列出，再加上 `session.fork` 同样要求的原生存储条件 |
| Agents 的目录 | 目录应用的协议版本与 Agent 过滤，与 `session.list` 相同 |
| Agents 的索引与连接 hub | 列出 Session 的实时 `running` 与 `attachedClients`，从进程状态读取、不取租约，与 `session.list` 相同 |

不调用 Session 门面、不取租约、不使用其他插件服务。

## 6. 客户端

`/resume` 背后的选择面板是第一个使用方，本页写明它对客户端的要求；客户端自己的设计拥有它的交互。

- **首页。** 首页是不过滤 `cwd` 的目录：`/resume` 今天列出所有工作区。
- **分页。** 选择接近已装载行的末尾时取下一页，跨页保持服务端的顺序。它发送 `withPreview`，因为它今天隐藏没有预览的行，于是首页是它显示的整页行，末尾附近的触发始终有行可取。每次冷启动都创建一个可能永远收不到消息的 Session，没有这个过滤，最新的若干行往往正是它隐藏的那些。
- **搜索。** 过滤以去抖的 `query` 发到服务端，而不是只缩小它已经持有的行；服务端匹配它今天搜索的字段——Session ID、工作区、model ID 与预览——加上标题与 provider ID，用相同的 Unicode 折叠，因此还没取的页上的匹配也会出现。
- **取消。** 关闭、刷新或替换搜索时，若守护进程公布了 `session.catalog.cancel`，先用它取消未完成的读取，再丢弃本地请求标识。之后的命令不等待过期查询。没有公布取消方法的旧守护进程保留原行为。
- **回落。** 不宣告该方法的守护进程经 `session.summaries` 列出。

```text
/resume -> [saved Sessions | search...]
             Esc       -> cancel read -> composer
             Ctrl+R    -> cancel read -> refreshed first page
             new query -> cancel read -> matching first page
```

project/all 开关是另一个客户端决定：`cwd` 为本合同为它而存在，并且在页边界之前应用，选择面板的首页不使用它。缺口记录在[客户端](../clients.md#32-session-管理)。

## 7. 决定

| # | 决定 | 拒绝的替代方案 | 原因 |
| --- | --- | --- | --- |
| C1 | 游标不透明，按 `(updatedAt, sessionId)` 键集分页 | 用 `offset` 或 `skip` 偏移分页 | 一次插入会移动之后每一行，偏移页会重复或跳过客户端已显示的 Session |
| C2 | 新增内建方法 `session.catalog` | 就地给 `session.summaries` 加 `limit` 与 `cursor` | 它的回复是数组；改动会破坏该方法的每个协议 v3 客户端，而数组没有可保留的页边界 |
| C3 | 预览与活动时间取自投影 | 每次请求时从历史计算 | 那正是本设计要去掉的代价：它读每个 Session 的历史，而导入 Session 的最后一个事件是完整的压缩替换 |
| C4 | `query` 与 `cwd` 在目录查询里应用 | 过滤客户端已装载的行 | 它会隐藏客户端还没取的页上的匹配 |
| C5 | `session.summaries` 保留合同并读同一份投影 | 让它继续读历史；删除它 | 没有新方法的客户端靠它列目录；投影同时回答两者后，保留合同不花代价 |
| C6 | 两个方法都不取 Session 租约 | 为列出的每个 Session 取一个租约 | 租约会恢复并钉住每个列出的 Session；投影、以及进程为 `running` 与 `attachedClients` 提供的实时状态，都不需要租约就能回答 |
| C7 | 预览即今天的规则，作用于按批次完整的窗口，实时批次与回填应用得完全相同 | 无上限的实时规则，随带第一条用户输入的批次写入预览；在第 256 个事件之后硬性截止 | 对第一条用户消息在窗口之后的 Session，或第一条用户输入是承载 256 的那个批次里紧接 256 之后的那条，它会改变 `session.summaries` 列出的内容，并让回填行与实时行不同。去掉或收窄窗口是另一次可见的变更 |
| C8 | `withPreview` 在目录查询里丢掉无预览的行，走投影的局部索引 | 客户端一直取页，直到凑够它显示的行 | 冷启动创建的 Session 可能永远收不到消息，最新的行往往没有预览，那种循环会读到无界的一串；局部索引让服务端的读取保持有界 |
| C9 | `query` 在查询时用 Unicode `str::to_lowercase` 折叠两侧，与选择面板相同 | 经 SQLite `LIKE` 只做 ASCII 匹配，它只折叠 ASCII | 它会静默漏掉选择面板今天能命中的重音与非拉丁大小写对 |

## 8. 对其他文档的修改

- [原生存储格式](../../../design/appserver/native-storage-format.md#556-v23-格式决定)：目录表、它的两个索引、从创建起的同一事务写入、删除与分叉维护，以及升级回填与它的一次性代价。
- [Session 删除与分叉](session-deletion-and-fork.md#32-顺序)：它的删除与分叉行清单在各自移除与写入的行旁列出 `session_catalog`。
- [文档索引](../../../INDEX.md)列出本页。
- [客户端](../clients.md#32-session-管理) §5 在实时 Session 列表那一行用 `session.catalog` 取代 `session.summaries`；CLI 文档记录 `/resume` 的分页、服务端搜索与取消。两者描述的行为只在客户端实现后才存在，所以由客户端的实现做这两处修改。

## 9. 规则检查

| 规则 | 回答 |
| --- | --- |
| §4 第 1 条（指令事件改别的所有者状态） | 不写任何东西。目录经 Agents 读取 SessionStore 拥有的行 |
| §4 第 2 条（交接） | 无：读取不把任何东西交给别的模块 |
| §4 第 3 条（before 回调） | 无 |
| §4 第 4 条（核心不带业务含义） | 不改核心；投影是 SessionStore 的业务行 |
| §4 第 5 条（批次） | 行写进 SessionStore 已有的批次事务；目录不加批次、不加事件 |
| §4 第 6 条（模型上下文） | 不变：没有事件，因此没有上下文路径 |
| §4 第 7 条（公开事件与服务） | Agents 从 SessionStore 存储的行回答，与 `session.list` 相同；Rpc 不直接访问存储或别的插件的状态 |
| §4 第 8 条（基础设施不变） | Timeline、Dispatcher、事件信封与格式、装配、Session 门面都不变。存储按存储页自己的格式决定新增一张派生表和零个事件 |
| §4 第 9 条（只有后续决定要用的事实才进 Timeline） | 目录是选择面板的显示数据，因此留在 Timeline 之外；不新增事件 |
| ADR 002 D2（不设接线模块） | 不新增插件：由 Rpc 的内建方法与 Agents 已有的目录回答 |
| ADR 002 D5（合同语言中立） | §3 即以数据表达的合同；Rust 写法归实现 |

## 10. 验收（流程测试）

- 缺省首页 50 行，显式的 `limit` 在 1 到 200 内，最后一页不带 `nextCursor`。
- 用返回的游标继续分页时，按声明的顺序不重不漏地返回其余每个 Session，包括两页之间有 Session 被更新时。
- `query` 能按六个字段中的任一个找到 Session——标题、预览、Session ID、工作区、model ID 与 provider ID——大小写不敏感，包括 ASCII 之外的大小写对，并能找到只在后面页上的 Session；`cwd` 精确匹配工作区；`withPreview` 去掉没有预览的 Session；三者都在页边界之前应用。
- `query` 里的 `%` 或 `_` 只匹配该字符本身。
- 带 `withPreview` 的一页即使前面有许多无预览的 Session，也返回 `limit` 行。
- 为一种 `query`、`cwd`、`withPreview` 组合取得的游标，换另一种重放时被拒绝为 `invalid_params`。
- 创建后从未写入的 Session 从创建起就列出，`updatedAt` 等于创建时间、没有预览，与今天的 `session.summaries` 相同。
- 预览保持按批次完整的窗口与最终性：紧接序号 256 之后、位于承载 256 的那个批次里的用户输入就是预览，实时与回填后都如此；第一条用户消息在起始于序号 256 之后的批次里的 Session 列出时没有预览；第一条用户消息只有图片的 Session 也列出为没有预览，之后的消息不会填它。
- 没有原生存储的守护进程不列出 `session.catalog`，其 `session.summaries` 与今天一样从已装载的 Session 回答。
- `main` 末尾是完整 `context/compaction` 的 Session 用存储的预览列出，不解码那个事件。
- Session 空闲已卸载时列出的存储字段与已装载时相同，读作未运行，且不启动模型、工具或 MCP 服务器。
- `session.summaries` 对协议 v3 保持回复形态与内容。
