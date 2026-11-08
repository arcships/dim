# 带 key 块：`session.keys` 读

状态：就绪。交付状态记录在 issue 中，不在本文（[#958](https://github.com/arcships/dim/issues/958)）。
范围：协议 v3 的 `session.keys` 读，读取一个分支自身的事件。它不新增事件、服务、部件或持久状态；与 `session.history` 一样由内建 Rpc 协议提供。

## 1. 目的与边界

带 key 的 raw 输入块是核心块数据。raw 块携带可选的 key（[session access A.1](../contracts/session-access.md#a1-sessions-服务) 的 `RawBlock.key {use: live | retire, name}`）；key 按分支 Timeline 生效，每个写入方自选 key，无人检查（A.2）。生产者通过普通输入路径写入：Goal 写 `goal:state`（[Goal](../plugins/Goal.md) §2，A2），记录出现之前写成的历史带 Goal 的旧 key `goal:objective` 与 `goal:status`；TimelineTree 写 `branch:line` 与 `branch:parent`。

需要展示存活带 key 状态的客户端，从它加载的分支历史与收到的 `session.event` 实时通知中折叠这些块。有界的历史窗口（最新一轮，[session-history-window](session-history-window.md)）可能从写下某个 key 的输入之后开始，于是从窗口重开会话的客户端会丢掉它从未加载过的存活状态。同一个折叠在每个客户端各写一份，各自都要重新发现同样的规则（opening 重置、retire、已删除输入）。

`session.keys` 把分支自身段在固定位置上的投影成恰好它的存活带 key 块。它是通用的：不认识 Goal，不解释块文本，不加入任何 Goal 语义。Goal 分支插件继续在服务端折叠自己的块（A3、A14）；本读服务客户端。

与 Goal 决定的边界：Goal A8 拒绝 Goal 专属的状态查询，理由是那需要 Session 部分读取每个分支各一份的服务（[ADR 002](../../adr/002-plugin-scope.md) D3）。本读不是那种查询：它返回没有部件拥有的核心块数据，由 Rpc 内建协议在 `session.history` 旁提供，A8 的决定与理由原样保留。Goal §6 与 A8 都把它列为 App Server 提供时客户端读取目标的路径。

本读不新增事件、服务、插件、部件或持久状态。它通过唯一的 event query 读取（[ADR 019](../../adr/019-event-query.md)），经已加载线的 Session，或在分支结束后经 TimelineTree 在其固定头上读取（[ADR 021](../../adr/021-reads-follow-the-state-owner.md) D2、D3）。

## 2. 合同

形状遵循 [session access A.1](../contracts/session-access.md#a1-sessions-服务)。

### 2.1 请求

```text
// KeysRequest
{ "sessionId": "string",
  "branchId": "string",              // v3 连接上必填，与所有指名分支的 v3 session 方法一致；
                                     // 根线由客户端传 "main"
  "through?": 0 }                    // 固定读取位置；缺省：发布头（session.sequence）
```

### 2.2 回复

```text
// KeysResult
{ "through": 0,                      // 投影固定的位置：请求的 through，或本读取得的发布头
  "entries": [ KeyEntry ] }          // 分支自身段上每个存活 key 恰一条；空数组，从不为 null
// KeyEntry
{ "key": "string",                   // RawBlock.key.name，例如 "goal:objective"
  "sequence": 0,                     // 携带该块的 input/message 事件序号
  "inputId": "string",               // 该事件的 id，与 input/deleted.inputId 同一 id 空间
  "text": "string" }                 // RawBlock.text，逐字节原样
```

- `entries` 恰含分支自身段上每个存活 key 的一条：该 key 最新存活的块，按携带输入 `sequence` 升序，同一输入内按块序。固定 `through` 下回复确定。
- `text` 是 raw 块文本：不转义、不渲染、不改写 marker。客户端从服务端对应方看到的同一串字节开始解析它自己的块格式。
- retire 块不产生 entry：key 失效后它不携带信息。无 key 的块同样不产生 entry。
- 空分支或全部 key 已 retire：`{through, entries: []}`。客户端不需要区分两者。
- fork 的 `main` 排除其 `session/forked` 之下的一切；子分支排除其 `branch/start` 之下的一切。拷贝与继承的事实不属于分支自身段。

请求没有 `keys` 过滤参数。回复总是列出该段全部存活 key，包括陌生的；客户端只读它认识的 key，其余忽略——不是错误、不触发重读、不影响其他 key。过滤不会减少扫描（一千条消息之前写下的 key 仍要找到），只会在这条规则之上再加一条未知 key 规则。

没有分页。entry 数随存活 key 数增长，与分支长度无关。`key` 由写入方自选且无人检查，所以计数在此处设上界：超过 1024 个存活 key 以不可重试的应用错误 `too_many_keys` 失败。本读从不截断：截断会悄悄丢掉存活状态。Goal 贡献一个 key `goal:state`（记录出现之前写成的历史上还有旧 key，直到第一条记录退役它们）、TimelineTree 2 个，只有文档合同之外的生产者才会触及该上界。回复同样有大小上界：其序列化 JSON 必须落在 `session.history` 所用的同一回复帧上界之内（`outbound::MAX_FRAME_BYTES` 减去 64 KiB 信封余量），超出即以 `keys_too_large` 失败，而不是被传输层丢弃。几个大块就会先于 key 数触及该上界。截断同样不是答案，理由同上。

### 2.3 规则

- 当分支自身段在 `through` 处某 key 最新存活的块是 `live` 块时，该 key 生效。同 key 的更晚块替换它；`retire` 块结束它。
- 被 `input/deleted` 删除的输入，其块从不生效，无论 `live` 还是 `retire`。该 key 保持仍存活的最新块，或不存在。
- 段始于分支第一个 opening 事件：子线的 `branch/start`、fork `main` 的 `session/forked`。其下一切不属于该段。

### 2.4 错误

| 错误 | 条件 |
| --- | --- |
| `unknown_method` | 连接低于协议 v3，与 `session.history` 相同；v2 连接看不到 `input/deleted`，投影无法在该版本提供 |
| `invalid_params` | v3 连接上 `branchId` 缺失或为空；`through` 超过发布头 |
| `session_invalid_input` | Session 或分支不存在（所有 session 方法的 acquire 与读取结果） |
| `session_closed`、`branch_ended`、`service_reloading` | Session 关闭、线在读取运行前结束、或服务重载；同 `session.history` |
| `too_many_keys` | 存活 key 超过 1024；不可重试，重试得到同样结果 |
| `keys_too_large` | 序列化回复超出回复帧上界；不可重试，重试得到同样结果 |

超过发布头的 `through` 是无效输入，绝不夹取到头部：客户端请求的是它观察到的位置，超出本连接头部意味着它的 overlay 建立在对分支的另一份视图上。夹取会回答客户端没有请求的位置。

## 3. 读取算法与成本

一次下行扫描，`TimelineQuery` 带 `kinds: ["input/message", "input/deleted", "branch/start", "session/forked"]`、`TimelineQueryDirection::Descending`、按 sequence 排序、每页 `limit` 256。扫描从 `through` 开始，在第一个 opening 事件处停止；其下一切丢弃。翻页用上一页的 `min_sequence - 1` 作为 `from_sequence`，绝不用 `offset`：查询的 offset 是跳过语义，存储读取方会先取出 `offset + limit` 行再跳过，按 offset 深翻页的成本与页数的平方成正比。短于 limit 的页说明选择已穷尽（ADR 019 D2）。成本为 O(opening 之上的 input/message 与 input/deleted 行数)，每页一次基于 `(session_id, branch_id, sequence)` 的 zone 查询；两万条消息的分支约 80 页。

删除在同一趟中收集，不需要第二次读取。使其正确的顺序：InputQueue 只为它仍然持有且未应用的输入写 `input/deleted`——撤回在输入既无 `input/applied` 也无消费时才被决定——所以删除事件总是写在被删输入之后。因此下行扫描先遇到删除、后遇到块；块只有在它的输入 id 不在已收集集合中时才存活。被删输入写下的 `retire` 块被同一检查跳过，不会结束 key。

每个事件的带 key 块按块序逆序读取（输入的最后一个块最先），所以同一输入内部也由最新的块获胜，与跨输入一致。已被更新事件决定的 key 被跳过；对尚无更新存活块的 key，`retire` 将其记为对更早块关闭。上界在收集过程中强制，超限的回复在扫描结束前即失败。

不需要新的存储能力：kinds 被下推到存储选择，下行、范围与 limit 都已存在。不存在的是块级 key 谓词——查询的 predicate 是一个 JSON 标量点路径并拒绝数组——所以扫描读取 opening 之上的每条输入消息，而不只是带 key 的。raw 块 key 上的派生索引属于 ADR 019 D4 的派生对象，不在本变更范围。

本读沿用 `session.history` 的双 owner 模式：活线经其 Session 读取（`Session::query`，读发布头），结束线经 TimelineTree 在其固定头读取（`TreeCommands::query`）。调用期间线结束会回答 `branch_ended`；随后改经 tree 在固定头重读。与 `latestTurn` 窗口不同，本读不需要原生存储：两种 owner 都能提供。

## 4. `through` 与客户端的实时 overlay

客户端把 `reply.through` 当作分界。其下写入的块正是回复所反映的；客户端按序应用它已收到的、位于其上的事件：

1. 对 `sequence > through` 的 `input/message`：按块序应用其 raw 块。live 块把该 key 的当前块设为 `{key, sequence, inputId, text}`；retire 块删除它。
2. 对 `input/deleted`：对该 `inputId` 出现在其保留块列表中的每个 key，删掉被删输入的块并取该 key 仍存活的最新块作为当前值；若已无剩余，该 key 不存在。因此客户端必须保留自 `through` 以来见过的**每一个**带 key 块，而不只是每个 key 的最新一块：`input/deleted` 无法回退到客户端已丢弃的更早存活块。

所依赖的保证：投影是段内 opening 之后事件的纯函数——只有块、retire 与已删除输入。压缩只写 `context/*` 事件，不删事件、不携带 key；事件只追加。因此对同一连接的视图恒有

```text
reply(through) ⊕ (through, through'] 事件的 overlay = reply(through')
```

想在更晚位置自检 overlay 的客户端可以重读并比对。

重开会话的客户端在头部读取，或把 `through` 固定到它所加载历史使用的同一位置，使两份视图描述分支的同一时点。

## 5. 与模型 baseline 的等价

模型侧的带 key baseline 在 `input/consumed` 处推进：`history.rs` 重放前缀，渲染每个已消费输入的块（`render_input`）并应用到 `delivered_state`，压缩记录拷贝该 baseline（[history.rs](../../../crates/dim-agent-capabilities/src/history.rs)）。本投影在 `input/message` 处推进，即 Goal A14 让变更生效的位置。

精确关系为：

```text
projection(through) = delivered_state(through)
                    ⊕ 写于 through 及之前、在 through 处既未消费也未删除的
                      输入的各 key 的带 key 块
```

两侧在静止点上完全相等：`through` 处没有「写而未消费」的带 key 输入，例如已稳定的轮末或压缩切点。该关系**按 key** 成立，且只有该 key 的块按序号顺序被消费时才成立：投影取序号最新的一块，基线取最后被消费的一块，因此一个被更晚的同 key 输入超过的 Queue 输入会颠倒二者。由单一写入方只以 Append 模式写的 key 满足这一点，因为 InputQueue 在提交时释放每个块，模型基线按该顺序消费它们。

谁满足它：Goal 的 key 是 `goal:state`，只由分支部件写入，且只以 Append 模式写入。TimelineTree 以 Append 模式写 `branch:line`（当块带线结束通知时改用 Steer），以 Append 模式写 `branch:parent`；`branch:parent` 的开场块搭载该线的第一条输入，它为 Queue，且在该 key 的任何更晚块出现之前就被应用。每个块在提交时或作为那条首输入被释放，因此三个 key 的块都按序号顺序消费，关系对三者都成立。

此时对每个 key：

- `entry.key` 等于 baseline entry 的 `key`；
- baseline entry 的 `message` 是一个文本部分，其字节等于 `entry.text`——`render_input` 用同一个 `RawBlock.text` 构造 `Message::text(Role::User, text)`，两侧都不转义、不渲染；
- 两个 id 指名同一个输入事实：`entry.inputId` 是携带该块的 `input/message` 事件，`ContextBaselineEntry.source_event` 是把该块放进 baseline 的 `input/consumed` 事件（`render_input` 收到的是消费事件），同一分支中相邻一个事件。

两侧顺序按构造不同，不比较：baseline 按 key 首次出现顺序，投影按 sequence 排序。比较按 key→entry 映射进行。

## 6. 规则检查

| 规则 | 本读如何满足 |
| --- | --- |
| [ADR 002](../../adr/002-plugin-scope.md) D2（没有 wiring module；需要归属的服务是部件或内建） | Rpc 把 `session.keys` 作为内建在 `session.history` 旁分派；不创建部件，不注册 `sessionMethods` 命名空间 |
| ADR 002 D3（装配不从分支状态做决定） | 本读是客户端协议方法；不新增部件、加载顺序或依赖配置的装配 |
| [ADR 019](../../adr/019-event-query.md) D1（唯一的有限事件查询） | 扫描是一次 `TimelineQuery`；不新增读取 API |
| ADR 019 D2（固定语义） | 按 sequence 排序的下行页，短页即穷尽，推进用范围而非 offset |
| ADR 019 D3（查询面向事件合同） | 只指名 kinds 与 sequence 起点；不指名表、列或索引 |
| ADR 019 D4（索引是派生的或格式的一部分） | 不新增索引；块级 key 谓词在当前查询中无法表达，也不加入 |
| [ADR 020](../../adr/020-timeline-read-convergence.md) D1（一次读取，门面只转发） | 读取经 `Session::query` 与 `TreeCommands::query`，与 `session.history` 相同的门面 |
| [ADR 021](../../adr/021-reads-follow-the-state-owner.md) D3（客户端读取需要活着的 Session） | 调用先 acquire Session；没有为进程未持有的 Session 读取存储事实的分支 |
| ADR 021 D2（结束线经拥有其结束的对象读取） | 结束线经 TimelineTree 在固定头读取，包括线在调用中途结束的情形 |

Goal A8 不变：Goal 专属查询需要 Session 部分读取每个分支各一份的服务，本读不是。它读取没有部件拥有的核心块数据，Goal §6 把客户端指向它作为折叠历史的替代路径。

## 7. 兼容性

- 方法仅限协议 v3，与 `session.history` 相同：v2 连接回答 `unknown_method`，因为 v2 页不携带 `input/deleted`，投影会是错的。
- 能力在 `hello.features` 中以 `session.keys` 广播（Session 执行开启时），与 `session.history` 并列；客户端只在它被广播时才发送该调用。
- 旧客户端继续使用它们的历史折叠，不受影响：本读不新增事件、不改变任何既有回复，Goal §6 与 A8 都仍把历史折叠列为方法未被提供时的路径。
- 本读是对已在线上事实的投影，所以 fork 与重载都从同样的事件提供它；不为它持久化任何状态。

## 8. 验收

- 一个输入里写两个 key，随后 retire 其一，再重新写其一：回复携带每个 key 最新存活的块，按 sequence、同一输入内按块序升序，且 `inputId` 与 `text` 正确。
- 对写下某 key 的输入执行 `input/deleted`：其块从不生效；存在更早存活块时该 key 回退到它，否则不存在；被删输入写下的 retire 不结束该 key。
- fork 的 `main` 与子分支都不列出父级的 key，写在子分支或 fork 上的 key 只出现在那里。
- 早于后续变更的 `through` 返回旧值；超过头部的 `through` 是 `invalid_params`。
- v2 连接回答 `unknown_method`；结束线在固定头提供与活线折叠相同的 entries。
- 固定在位置 0 的读取回答 `{through: 0, entries: []}`；全部 retire 的分支不列 entry；超过 1024 个存活 key 是 `too_many_keys`。
- 两条带 key 输入的文本合计超过回复帧上界时回答 `keys_too_large`，同一连接仍能回答下一次调用；刚好低于上界的回复正常提供。
- 在静止点上，回复按 key 等于最后一个 `HistoryEntry.baseline`：同样的 key、同样的文本字节，且每个 id 指名同一个输入事实。

## 9. 对其他文档的改动

- [Goal](../plugins/Goal.md) §6 的「读取目标」行与附录 E A8 把本读列为 App Server 提供时的客户端路径。
- [Session access](../contracts/session-access.md) §1 把本读列进它定义的合同，A.1 携带 `KeysRequest`、`KeysResult` 与 `KeyEntry`，上下文映射的「已删除的输入」行声明被删输入不出现在 `session.keys` 中。
- [文档索引](../../INDEX.md) 收录本页。
