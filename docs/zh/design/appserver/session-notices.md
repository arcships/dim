# Session 广播通知

状态：已在 C6 实现（[#49](https://github.com/arcships/dim/issues/49)，[#85](https://github.com/arcships/dim/pull/85)）。需求：[SessionTitle §9 R1](../plugins/SessionTitle.md#9-向机制所有者提出的需求)。总览：[§6.2 第 2 条约束](../overview.md#62-约束)、[附录 B.2](../overview.md#b2-app-server-进程插件)（Rpc）、[附录 F](../overview.md#附录-f-向机制所有者提出的需求)。广播合同：[事件驱动流程 §6](../../../architecture/rust-agent/event-driven-flows.md#6-session-广播)。第一个发出方：SessionStore 标题元数据（S5b，[#60](https://github.com/arcships/dim/issues/60)，[#96](https://github.com/arcships/dim/pull/96)：[标题元数据](session-title-metadata.md)）。

## 1. 概要

关于 Session 的进程级状态（例如 SessionStore 的标题）从不进入 Timeline（[原则 §4 第 9 条](../../../architecture/rust-agent/principles.md)）。客户端需要实时看到这类状态的变更时，由状态的所有者在变更提交后，在该 Session 的广播频道上发出一条通知宣布变更。Rpc 把通知转发给已附加到该 Session 的协议 v3 连接。

- **记录。** `SessionBroadcastEvent::Notice` 携带 `{source, kind, data}`。广播和 Rpc 都不读取这些字段。
- **发出方。** `session_broadcast::notify(ctx, sessionId, notice)` 在既有频道上发出后立即返回。没有监听方的通知被丢弃。
- **线上形式。** 协议 v3 的 `session.notice {sessionId, source, kind, data}`。
- **不新增。** 不新增 Timeline 事件、序号、存储、重放、插件、生命周期钩子、客户端方法或协议版本。观察事件的 C4 叶子部件跳过通知（§6）。

## 2. 流程

```text
 所有者（进程作用域）            Session 频道 session/<id>          Rpc 订阅                     客户端（v3）
 例如 SessionStore               一条 rutis 命名频道                每个附加连接一个
 -------------------------      ----------------------------       -------------------------   ------------------
 提交变更
 notify(ctx, id, notice) -----> Notice {source, kind, data}
                                  没有监听方：丢弃
                                  （未加载、无人观察）
 SessionBroadcast（分支） ----> Event {branchId, event}
                                  |
                                  | 同一频道，按发出顺序
                                  +------------------------------> Forward
                                  |                                  v1 或 v2：不发通知
                                  |                                  Closed 之后：丢弃
                                  |                                  其他 -------------------> session.notice
                                  |                                                              {sessionId, source,
                                  |                                                               kind, data}
                                  +------------------------------> C4 叶子行：跳过 Notice
```

## 3. 通知记录

```rust
pub enum SessionBroadcastEvent {
    Event { branch_id: String, event: Arc<AgentEvent> },
    /// 关于本 Session 的进程级状态已提交的一次变更。
    Notice(Arc<SessionNotice>),
    Fence { recipient: String, token: u64 },
    Closed,
}
#[derive(Debug, Serialize)]
pub struct SessionNotice {
    pub source: String,           // 所有者，例如 "SessionStore"
    pub kind: String,             // 所有者定义的变更种类，例如 "title"
    pub data: serde_json::Value,  // 所有者为该种类定义的数据
}
```

| 规则 | 内容 |
| --- | --- |
| 含义 | 由所有者定义。`source` 是所有者的名字，每种 `kind` 及其 `data` 的含义由所有者写明（标题通知见 [S5b §7](session-title-metadata.md#7-标题通知)）。广播和 Rpc 从不解释它们；客户端不处理的 `source` 或 `kind` 直接忽略。 |
| Session | 记录本身不写 Session；频道就代表 Session，与 `Event` 相同。所有者不必在 `data` 里重复 Session id。 |
| 位置 | 没有序号，没有批次；不持久化，不重放。通知只报告状态变了，不是之后的决定要用的事实。需要让客户端排序或去重变更的所有者，在 `data` 里放自己的修订号，标题通知就是这样做的。 |
| 正文 | 所有监听方经 `Arc` 共享同一份正文，与 `Event` 相同；没有人复制或改写它。 |
| 大小 | 是变更摘要，不是大块内容：它在每个附加连接上占一个可靠帧（§5），超过连接帧上限时，与任何过大的通知一样关闭该连接。 |

## 4. 发出

```rust
/// 宣布关于一个 Session 的进程级状态已提交的一次变更。
pub fn notify(ctx: &Ctx, session_id: &str, notice: SessionNotice);
```

| 规则 | 内容 |
| --- | --- |
| 时机 | 变更提交之后，用所有者自己的上下文调用。失败或放弃的变更不发通知。 |
| 调用 | 同步：一次 rutis `emit`，不等待监听方，也不报告它们做了什么。总线拒绝的发出记一条警告，提交照常有效。 |
| 送达 | 由发出时有谁在监听决定。未加载的 Session，或没有客户端观察的 Session，没有接收通知的监听方，通知即被丢弃。不会为之后的加载或订阅排队。 |
| 依赖 | 无。`notify` 是共享总线上的函数：所有者不必声明对 SessionBroadcast、Rpc、Agents 或任何可选插件的依赖，也不需要生命周期钩子；无论它们存在、缺失还是在重载，行为都一样（[ADR 002](../../adr/002-plugin-scope.md) D3）。 |
| 顺序 | 每个 Session 只有一条频道，所以每个监听方都按发出顺序收到通知和 `Event` 记录。所有者若是对经广播观察到的事件作出反应，它发出的通知排在该事件的记录之后，客户端也就先看到事件、再看到通知。并发的变更按 `notify` 的调用顺序到达监听方：需要让客户端看到提交顺序的所有者，在自己的提交串行化范围内调用 `notify`，或依靠修订号。 |
| 重载 | 所有者卸载或重载期间不发出任何通知，恢复后也不重放。Session 能力的重载不影响 `notify`，它不经过 SessionBroadcast 插件。 |
| 非 Rust 所有者 | 没有绑定。SessionTitle 需要的发出方 SessionStore 是 Rust。ExternalAgent（[#145](https://github.com/arcships/dim/issues/145)，P3，TypeScript 优先）由其 TypeScript 进程行（外部成员活动的所有者）发出 `externalAgent.activity` 通知；所需绑定已在 [ExternalAgent §9.1](../plugins/ExternalAgent.md#91-session-通知的-typescript-绑定) 提出，尚未被接受。 |

例如 SessionStore 的 `setTitle`（S5b）提交新的修订号后，在它对标题写入的串行化之内调用
`notify(&ctx, session_id, SessionNotice { source: "SessionStore".into(), kind: "title".into(), data })`，其中 `data` 是 `{title, titleSource, titleRevision}`（[S5b §5、§7](session-title-metadata.md#7-标题通知)）。

## 5. Rpc 转发

| 规则 | 内容 |
| --- | --- |
| 线上形式 | 通知 `session.notice`，参数 `{sessionId, source, kind, data}`。`source`、`kind`、`data` 原样取自记录；`sessionId` 是订阅所在的 Session，也是每条 Session 通知都带的路由字段。 |
| 协议 | 只发给协议 v3 连接；v1 和 v2 的通知集合已经冻结，永远收不到它。协议版本仍为 3：这条通知是增量的，与 v3 陆续增加的事件种类一样，客户端忽略自己不处理的通知和种类。CLI 会忽略它，`exec --json` 会把它与该 Session 的其他通知一起输出。`hello.features` 只列方法，不列这条通知。 |
| 路径 | 由订阅既有的广播监听器投影记录，所以在每个连接上，通知与 `session.event` 保持频道中的相对顺序。它和每条 Session 通知一样进入连接的可靠队列：跟不上的客户端作为慢消费者被断开，而不会在无声无息中落后。 |
| 窗口 | 从注册监听器的那次 attach 开始，到订阅结束为止：detach、被替换、连接关闭或 `Closed`。attach 之前发出的通知不重放。附加或重新附加的客户端在 attach 之后经所有者的查询读取当前状态（标题用 `session.summaries`），之后的每次变更都以通知送达。同一次变更可能两边都看到，由所有者的修订号去重。 |
| 关闭 | 频道上的 `Closed` 记录结束订阅的转发。与关闭竞争、排在 `Closed` 之后的通知被丢弃，所以 `session.closed` 仍是该订阅最后一条 Session 通知。之后由所有者的查询读取状态。 |

## 6. C4 叶子观察方

监听 `session/{sessionId}` 的 C4 叶子行会收到该通道的每条记录。通知以 `{kind: 'notice', notice: {source, kind, data}}` 到达，观察方像跳过 `fence` 和 `closed` 一样跳过它（[C4 广播合同](../../../design/session-typescript.md#native-session-broadcast)）。通知报告的是进程状态，其所有者已提供查询，而且没有 TypeScript Session 部件需要通知：SessionTitle 观察 `input/applied`，从 SessionStore 读取标题元数据。将来有使用方时，另行增加它自己的叶子合同。

## 7. 时序与冲突

| 场景 | 结果 |
| --- | --- |
| Session 未加载 | 没有监听方：丢弃。下一次 attach 读取当前状态 |
| Session 已加载，没有客户端附加 | 丢弃；监听该通道的 C4 叶子行跳过它 |
| attach 之前发出的通知 | 不重放 |
| detach，或替换订阅的 attach | 旧订阅的监听器已移除；新订阅只收到之后的通知 |
| Session 正在关闭 | `Closed` 之前转发，之后丢弃 |
| 所有者重载 | 所有者不在运行时不发出；不重放 |
| 慢客户端 | 与事件相同，作为慢消费者断开 |
| 协议 v1 或 v2 连接 | 永远收不到通知 |
| 两个所有者的变更，或同一所有者的两次变更 | 按频道上的发出顺序 |

## 8. 规则检查

| 规则 | 本设计如何遵守 |
| --- | --- |
| [原则](../../../architecture/rust-agent/principles.md) §4 第 1 条（指令事件） | 不适用：通知不改变任何状态；所有者经自己的类型化操作改变自己的状态。 |
| §4 第 2 条（交接） | 不适用：没有交接。 |
| §4 第 3 条（before 回调） | 不适用：没有 before 回调。 |
| §4 第 4 条（核心不带业务含义） | 记录不带业务含义：广播和 Rpc 不读取任何字段，也不点名任何所有者。 |
| §4 第 5 条（批次） | 不适用：通知不是 Timeline 事件，没有批次。 |
| §4 第 6 条（模型上下文） | 没有模型可见的效果。 |
| §4 第 7 条（公开的事件和服务） | 所有者经 `notify` 使用公开的频道 key；Rpc 和客户端只看到记录。 |
| §4 第 8 条（基础设施默认不变） | 在广播内回应 SessionTitle §9 R1：一种记录、一个发出函数，由广播到客户端的流程测试验证。Timeline、Dispatcher、门面和装配都不变。 |
| §4 第 9 条（只有决定要用的事实才进 Timeline） | 通知存在的目的就是让进程级状态留在 Timeline 之外。 |
| §1（Capability 与 App Server 边界） | 记录和 `notify` 与广播一起在 `dim-agent-capabilities`；线上投影在 Rpc。 |
| [ADR 002](../../adr/002-plugin-scope.md) D1（作用域随状态） | 通知关于一个 Session，走该 Session 的频道；发出方保持各自的作用域。 |
| ADR 002 D2（不设接线模块） | 没有接线模块或注册表。 |
| ADR 002 D3（依赖随作用域） | `notify` 不需要依赖；Rpc 不依赖任何所有者。 |
| ADR 002 D4（配置） | 不适用：没有配置。 |
| ADR 002 D5（合同语言中立） | 记录和线上形式都是数据 `{source, kind, data}`；§3 与 §4 给出 Rust 绑定。 |

向机制所有者提出的需求：无。

## 9. 验收

流程测试位于 [notices.rs](../../../../crates/dim-app-server/tests/transport_flow/notices.rs)，经 IPC 和 WebSocket 运行：

1. 一个 Session 级测试所有者经广播观察到 `input/applied` 后调用 `notify`。两个已附加的 v3 客户端各收到一条 `session.notice`，`source`、`kind`、`data` 保持原样，并排在该事件的 `session.event` 之后。已附加的 v2 客户端收到同样的事件，但没有通知。
2. 一个进程级的所有者替身（`RunningServer::notify`，feature `testing`）发出通知，已附加的客户端收到。客户端 detach 期间或 Session 已卸载时发出的通知从不送达；重新附加的客户端只收到 attach 之后发出的通知。`session.close` 回复之后（因而在 `Closed` 之后）发出的通知，不会在 `session.closed` 之前到达客户端。

## 10. 决定

| # | 决定 | 理由 |
| --- | --- | --- |
| D1 | `Notice` 是既有 Session 频道上的一种记录 | 同一频道让通知与事件保持顺序，并复用 Rpc 的监听器；单独的频道需要第二个订阅，也会失去这种顺序 |
| D2 | `notify` 是函数，不是服务或插件 | 所有者不能依赖可选插件，通知也不需要生命周期：所有者重载只会停止它的通知 |
| D3 | 没有监听方即丢弃；不排队，不重放 | R1 的要求；恢复靠所有者的查询，排队则需要保留策略和自己的生命周期 |
| D4 | 记录不写 Session；由 Rpc 加上 `sessionId` | 频道代表 Session，与 `Event` 相同；每条 Session 通知都按 `sessionId` 路由，所有者也无法写错。这细化了 R1（其线上形式是记录本身）：标题数据中自带的 `sessionId` 变得多余，S5b 不再保留它（[S5b §7](session-title-metadata.md#7-标题通知)） |
| D5 | 只发协议 v3，不升版本 | v1、v2 已冻结；这条通知是增量的，与 v3 新增的事件种类相同 |
| D6 | 可靠送达：慢客户端被断开 | 与事件一致；无声丢弃通知会让客户端落后且没有任何信号 |
| D7 | `Closed` 之后的通知被丢弃 | `session.closed` 仍是订阅的最后一条 Session 通知；空档由所有者的查询覆盖 |
| D8 | C4 叶子部件跳过通知 | 监听该通道的行会收到通知；没有 TypeScript 使用方需要 |
| D9 | 正文经 `Arc` 共享 | 与 `Event` 正文相同，不为每个监听方复制 |
