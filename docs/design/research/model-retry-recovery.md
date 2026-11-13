# 模型请求重试与任务续接方案

本文提出可评审的产品行为、职责划分和交付边界。请求内重试沿用 ModelProvider 的所有权；跨请求续接提出独立 ModelRecovery 能力及其需要的合同。本文不声明新的服务或事件已经存在，也不修改其他所有者的现行合同。

设计议题：[模型重试可见性与同一 turn 的有界恢复 #1210](https://github.com/arcships/dim/issues/1210)。相关工作：[模型失败与重试 #1197](https://github.com/arcships/dim/issues/1197)、[失败事实 #1201](https://github.com/arcships/dim/pull/1201)、[请求重试与进度 #1207](https://github.com/arcships/dim/pull/1207)。交付状态由 issue 跟踪。

规则依据：[设计规则](../../architecture/rust-agent/principles.md)、[时间总线](../../architecture/rust-agent/time-bus.md)、[ADR 002](../../adr/002-plugin-scope.md)、[插件合同](../plugin-contract.md)、[rutis 使用方式](../rutis-usage.md)。

## 1. 推荐范围

建议分成两项可以独立评审的交付：

1. **请求内重试及可见性**：完善结构化错误、有限退避、取消和客户端展示，复用现有 ModelProvider 与 `session.model.progress`。这项可以直接形成实现范围。
2. **同一 turn 内的跨请求续接**：由独立的 branch ModelRecovery Capability 决定，但必须先由 Loop 等所有者接受并定义交接合同。这项首先交付合同，不能凭本文的概念字段直接写实现。

本次不增加统一 ErrorHandler、全局重试调度器、另一套事件总线、持久计时任务或每个调用者自己的重试配置。不重新执行工具，不自动换模型，不因鉴权、余额或任意未知错误盲目重试。

### 1.1 名词

| 名词 | 含义 |
| --- | --- |
| turn | Loop 拥有的一轮执行，可能包含多个模型请求和工具步骤 |
| request | 一份已经确定上下文和工具绑定的模型请求，有自己的 requestId |
| attempt | 同一请求的一次实际模型调用；第一次调用也计数 |
| 请求内重试 | 在输出仍可安全重放时，再执行同一份请求 |
| 续接 | 旧请求已经结算，基于提交的历史准备一个新的 request，在同一 turn 内继续 |
| 故障链 | 一次失败及其唯一一次续接构成的关联，用于限制预算；不是独立的运行、服务或持久任务 |

## 2. 已确认的边界

本仓库源码基线：`7283f418b6f6422131a9417ab0a6830fe93966f6`。以下是代码证据，不是待实施项的完成声明。

| 边界 | 证据 | 对方案的约束 |
| --- | --- | --- |
| ModelProvider 已是 branch plugin | [默认装配](../../../crates/dim-agent-capabilities/src/defaults.rs)、[ModelProvider 合同](../../architecture/rust-agent/plugins/ModelProvider.md) | 无需为已有请求重试再拆一个接线 plugin |
| 主请求、辅助模型、摘要共用请求恢复实现 | [recovery.rs](../../../crates/dim-agent-capabilities/src/model_provider/recovery.rs)、[auxiliary_model.rs](../../../crates/dim-agent-capabilities/src/auxiliary_model.rs)、[summary_strategy.rs](../../../crates/dim-agent-capabilities/src/summary_strategy.rs) | 不能把重试移入只覆盖主对话的分支监听器 |
| 本地基线最多 3 次尝试，两次默认退避合计 1.125–1.5 秒 | [recovery.rs](../../../crates/dim-agent-capabilities/src/model_provider/recovery.rs) | 体验上的“立即停止”不等于从未重试 |
| 主请求收到非 MessageStart chunk 后关闭原请求重放 | [output.rs](../../../crates/dim-agent-capabilities/src/output.rs) | thinking、usage、工具参数也可能关闭重放；不能只检查是否有可见文本 |
| 失败通常结束 turn；符合既有预算恢复合同的失败有例外 | [agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs)、[Loop 合同](../../architecture/rust-agent/plugins/Loop.md) | 订阅失败事件的独立 plugin 无法自然取得同一 turn 的控制权 |
| 进度是当前调用的可替换快照 | [contracts.rs](../../../crates/dim-agent-runtime/src/contracts.rs)、[订阅转发](../../../crates/dim-app-server/src/connection/subscription.rs) | 倒计时应扩展当前进度，不每秒写 Timeline |

## 3. 所有权

| 所有者 | 自己负责 | 不接管 |
| --- | --- | --- |
| Provider adapter / aimux bridge | 在外部边界读取 status、code、错误来源和重试提示，产生类型化失败事实 | 根据用户界面文案决定重试 |
| ModelProvider | 当前调用的 attempt、退避、取消、输出安全、进度及终态 | 工具重放、跨 turn 自动启动、任务续接策略 |
| 凭据所有者 | 刷新凭据、确认拒绝、清理失效凭据 | 通用任务恢复和 UI 计时 |
| ModelRecovery，拟新增 branch Capability | 跨请求恢复资格、每 turn 续接预算、恢复决定及其结算 | 直接调用模型或工具、写 Loop 的结果、改别的插件状态 |
| Loop | turn、请求准备与步骤推进；接受或拒绝针对当前步骤的恢复命令 | 识别 HTTP 错误、决定供应商退避策略 |
| ContextPreparation / compaction 所有者 | 按自身合同准备上下文、执行压缩 | 把任意模型错误解释为上下文超限 |
| App Server | 装配、公开入口和数据转发 | 重试计时和业务恢复策略 |
| 客户端 | 投影、倒计时、错误呈现和用户操作 | 在本地定时重发模型请求 |

请求内控制状态属于当前 I/O；重载结算调用后不恢复其计时。跨请求预算影响后续决定，必须从本分支事实重建。两者不能为了统一接口而塞进同一状态容器。

## 4. 请求内重试

### 4.1 策略

请求重试策略由 ModelProvider 通过共享调用路径执行。参数建议沿用 [#1207 的模型失败设计](https://github.com/arcships/dim/blob/ec6b21a2253f49394955c1775237e0f69c455dd4/docs/design/model-failures.md#4-retry)。

| 项目 | 建议 |
| --- | --- |
| 总尝试上限 | 8，包含第一次调用；凭据恢复后的调用也计入 |
| 默认退避 | 第 n 次重试为 min(1 秒 × 2^(n−1), 30 秒)，乘以 [0.8, 1.0] 抖动 |
| 默认累计睡眠 | 7 次等待约 72.8–91 秒，不包含网络调用耗时 |
| Retry-After | 有效提示按原值等待，不缩短为 30 秒；超过剩余等待预算则停止并保留提示 |
| 等待预算 | 累计实际睡眠最多 300 秒；不是请求的总耗时上限 |
| 凭据恢复 | 凭据所有者支持时最多一次，无需等待。遵循现有的重试禁令与终态凭据结算合同 |
| 开关和配置 | 首期不增加业务配置入口；不允许调用者各加一层重试循环 |

| 失败类别 | 请求内动作 | 续接边界 |
| --- | --- | --- |
| network、rate_limited、server_error | 输出可重放且预算足够时重试 | 已有部分输出时才考虑第 6 节；耗尽同一故障链预算后停止 |
| malformed_response | 输出仍可重放时最多重试一次 | 首期不自动修补协议或拼接半截工具参数 |
| auth_rejected | 交给现有凭据恢复合同，恢复无效后停止 | 不以“继续”绕过鉴权失败 |
| quota_exhausted、not_found、invalid_request、content_filtered | 停止并显示对应动作 | 不自动续接 |
| context_too_long / 有效预算事实 | 仅使用已有上下文恢复合同 | 不从字符串猜 token 数，不新增错误到压缩的通道 |
| output_limit | 保留提示与事实，不自动重试 | 带未执行调用的回复由 Loop 在同一 turn 内续接一次（见 §6.2）；自动续写文本需另行定义 |
| unknown、配置/准备失败、存储故障、用户取消、重载 | 不进入通用请求重试 | 不自动续接 |

分类只在拥有原始外部数据的边界完成。业务错误码优先级、stream event 的状态码含义、安全展示字段遵循模型失败所有者的合同；消费者不得再次从 `error` 文本推断类别。HTTP 200 的流错误不能据 HTTP 200 判定成功，裸 429 也不能被客户端擅自解释成余额不足。

### 4.2 不变量

1. 一次请求固定上下文、模型选择和工具绑定；重试不重新读取最新配置来悄悄改变请求。凭据更新仅通过已有凭据所有者。
2. 失败 attempt 先取消并排空资源，再进入下一次 attempt；同一请求不会并行运行两次。
3. 主请求按输出所有者的安全判断停止原请求重放，不放宽为“还没有完整回答就能重试”。辅助模型的收集边界继续由它的调用方式保证。
4. 等待和调用都响应取消。stop、steer、关闭和重载使用已有所有者语义，不另设无法中止的 sleep。
5. 多个窗口、客户端断开或重新 attach 不改变执行次数；历史读取不产生请求。
6. 所有适用预算耗尽后正常结算失败；不额外追加隐藏“继续”来开启一轮新的 8 次尝试。

## 5. 请求重试的可见性

### 5.1 数据契约

复用 `session.model.progress` 的替换快照语义，消费上游 `retry`，不再新建一个 Session 通知类型。以下字段来自 #1207 的提案，实施以其最终所有者合同为准：

| 字段 | 含义 |
| --- | --- |
| sessionId、branchId、turnId、requestId | 当前进度所属工作；客户端不能跨请求复用等待状态 |
| retry.attempt | 即将执行的第几次 attempt，从 2 开始；不是“已重试次数” |
| retry.maxAttempts | 总尝试上限，包含首次 |
| retry.retryAtMs | 运行时预计结束等待的 Unix 毫秒时间，供展示 |
| retry.kind | 结构化错误类别 |
| retry.reason | 可选、安全且有长度限制的诊断摘要 |

等待开始时设置 `retry`；等待结束、取消或调用结算时清除。字段缺失代表没有当前等待，不等于成功，也不足以证明下一次网络调用已发出。

倒计时由客户端计算 `ceil(max(0, retryAtMs - now) / 1000)`。真正的等待由运行时单调时钟控制；客户端时钟偏差不会提前触发请求。到零尚未看到新状态时显示 `Waiting for provider`；收到不带等待的活动进度后恢复普通运行展示，不宣称 `Retried successfully`。

### 5.2 展示

Web / Desktop 的目标交互：

```text
+----------------------------------------------------------------+
| Conversation                                                   |
|   Existing assistant output stays here.                         |
|                                                                |
|   [!] Provider temporarily unavailable                          |
|       Attempt 3/8 starts in 4s                         [Stop]    |
|                                                                |
| > Message...                                                   |
+----------------------------------------------------------------+
```

窄屏或终端活跃区域：

```text
Retry 3/8 in 4s · Network error
> Message...
```

最终失败：

```text
[!] Could not reach the provider
    8 attempts · 78s waiting
    Check the network or proxy.
```

- 同一请求只有一个等待状态。开始输出后清除等待展示；首期成功后不保留永久的“重试成功”时间线行。
- 等待仍属于 running；停止操作沿用现有交互。保持输入焦点、草稿、滚动位置，不弹模态窗口，不新增 TUI 快捷键或命令菜单项。
- 窄屏优先显示尝试次数、等待时间和短类别；详细 reason 换行或使用既有详情入口，不挤掉输入和停止控制。
- 使用现有主题中的警示语义，但不能只靠颜色区分状态。读屏在进入等待或终态时播报，不每秒重读整条错误。
- 首期没有“立即重试”按钮。它需要准确唤醒当前等待的服务合同和 Retry-After 限制，不能用发送“继续”代替唤醒操作。
- fullscreen 和 scrollback TUI 使用活跃区域更新倒计时，不每秒追加 transcript。普通 line 模式每次进入新的等待至多输出一条人类可读提示；`exec --json` 的 stdout 继续遵循已有 JSONL 合同，不混入倒计时文本。

按[客户端边界](../clients.md)，本仓库负责 Desktop 可消费的 App Server 合同。Desktop 对接以公开协议验收，不依赖其内部实现；必须验证通知接收、当前状态展示和停止操作。

### 5.3 历史、重连和去重

`retry` 是当前调用的显示状态，随重新 attach 的当前进度快照恢复；读历史时不补造倒计时。关闭 Session 或恢复进程后以所有者结算事实为准，不沿用客户端缓存的等待。

失败终态使用上游 `failure` 的类别、实际 attempts、waitedMs、可选 retryAfterMs 和安全诊断，保留 `error` 供旧展示路径兜底。正常成功不为了 UI 新增 Timeline 事实；若未来需要成功重试历史，应单独明确谁读取并用它做什么决定。

客户端按同一 turn 内的实际请求关联模型失败和 turn 失败，只抑制同一原因的重复行。不能全局按错误文本去重，也不能因曾显示过一个模型失败而吞掉后续准备失败、存储失败或不同原因的 turn 失败。若只读窗口没有相关模型失败，则仍显示 turn 失败。同一终止原因只显示一处，诊断数据不因去重而丢失。

## 6. 跨请求续接：ModelRecovery 的目标合同

本节是建议接受的行为边界。第 7 节的所有者合同完成前，不注册新事件、端口或插件。

### 6.1 第一批覆盖

仅处理有结构化 `network`、`rate_limited` 或 `server_error` 事实，且原请求因已产生输出无法重放的模型失败。首期不处理空响应、output_limit、任意 malformed_response、工具错误、Hook 错误或持久化失败。

这组排除不限制 Loop 自己的处置：声明了工具调用而一个都没执行的失败输出，由 Loop 在同一 turn 内续接一次，不等同于本文的文本续接，也不需要处置方端口。其成立依据、顺序和工具边界见 [Loop 的该节](../../architecture/rust-agent/plugins/Loop.md#a-failed-step-whose-calls-were-announced-and-never-executed)。

恢复必须满足旧请求已结算、可用输出已提交、当前 turn 已执行的工具调用均已结算且结果可用于构造上下文、turn 未中断或结束、模型策略允许恢复且仍有预算。未知错误、缺少结算事实或历史无法合法续接时，保留失败。

首期自动续接只允许补全回答文本。Loop 与工具执行所有者须保证续接请求不向模型声明工具，也不执行模型意外输出的工具调用。遇到这种调用时保留已提交内容并结束该 turn。若无法建立这个边界，按原失败结算。

### 6.2 预算与内容

- 每个 turn 最多接受一次自动续接；正常文本、工具结果、错误类别变化、模型切换或新 requestId 都不重置这个计数。新用户输入实际启动的新 turn 才有新额度。
- 这个计数由 Loop 的 turn 状态持有：处置方续接与「调用已声明但从未执行」的续接共用同一次额度，两者都不再另立计数器。上下文预算恢复属于另一条合同，不读也不消耗它。
- 原请求与续接请求共用该故障链的 **8 次模型调用、300 秒累计睡眠** 上限，继承已用 attempts 和 waitedMs。总预算耗尽即停止，不追加另一轮完整预算。
- 故障链以原失败 requestId 关联；普通工具步骤之后的正常请求不属于这条链，不能把整个长 turn 限成只有 8 次模型调用。
- 续接使用新的 requestId 和同一 turnId。保留已提交 assistant 内容和真实工具结果；不修改历史输出、不执行不完整的工具调用、不重放已完成的 callId。未执行的调用不能被伪造为成功结果。
- 模型可以用新 callId 重复先前的操作。callId、既有工具权限和恢复提示都无法证明操作只执行一次。[ShellInput](../plugins/ShellInput.md#a1-methods) 的 `shell.run` 是非幂等操作，客户端不得自动重试。若未来允许自动续接调用工具，须先由工具所有者定义可执行的重放合同，或由用户明确确认具体操作。
- 需要模型可读的恢复提示时，由 Recovery 通过正式输入入口提交最终内容，完成采用后再准备下一请求。展示状态不是输入，不能由 HistoryRenderer 将 Recovery 私有事件解释成额外上下文通道。
- 恢复提示使用固定语义说明“从已提交内容继续，避免重复已完成工作”；不将供应商原始错误、headers、响应体或凭据嵌入提示。
- 需要等待时由模型调用所有者执行剩余预算下的退避，不叠加独立等待层；共享预算与跨请求衔接的绑定由第 7 节定义。

### 6.3 交接顺序

下面是所需业务顺序，不是已存在的事件命名或装配图：

```text
ModelProvider settles the failed request and its output
                         |
                         v
Loop opens a bounded recovery decision boundary
                         |
                         v
ModelRecovery reads committed facts and decides
             /                               \
       Continue request                   Finish turn
             |                               |
  Adopt recovery input, if needed            |
             |                               |
  Loop accepts the matching decision         |
             |                               |
  Prepare a new request, same turn     Loop writes turn/end
             |
  ModelProvider owns the new invocation
```

Loop 继续拥有 turn，只交出恢复策略的判断；Recovery 不能自己写 `turn/end`、`step/model-start` 或 `model/call`。决定必须绑定失败事实、turnId 和 requestId；Loop 消费决定时复核是否仍是当前工作，写出自己的处理结果。

### 6.4 并发、取消和恢复

| 情况 | 必须满足的结果 |
| --- | --- |
| stop 在恢复接受前被消费 | 结算取消；迟到的决定和输入不得再启动模型 |
| stop 在新请求开始后到达 | ModelProvider 按原取消合同结束新请求；不再恢复 |
| steer 已被采用且替换了恢复目标 | 旧决定失效，用户输入走既有 steer 合同；不另起恢复请求 |
| 普通 queue 输入 | 不被 Recovery 偷偷消费、删除或解释成授权；由 Loop 正常接手 |
| 重复决定或重复通知 | 至多接受一次；按稳定工作关联去重，不按到达时间猜测 |
| plugin 缺失、Pending、禁用或退出 | 回到确定的失败结算，不留下永久 running |
| Session/branch closing 或 plugin reload | 停止自己的任务，结算在途工作，再撤销服务 |
| 进程恢复、fork 或历史重放 | 不重新执行模型/工具，不恢复旧计时；分支恢复计数不从父分支错误继承 |
| Goal、Scheduler 或 Workflow 观察 turn | 恢复期间没有伪造的 turn/end，不重复触发完成或后续调度 |

ModelRecovery 的计数、决定和结算属于本分支事实，内存只是其投影；I/O 任务按生命周期管理。采用输入与恢复决定如何避免被 stop 之后的普通队列重新驱动，是交接合同的验收项，不能靠 producer 一次取消检查保证。

## 7. Requirements raised to mechanism owners

### R1. Loop 的失败处置交接

- **阻塞合同**：[Loop](../../architecture/rust-agent/plugins/Loop.md) 及 `agent_loop.rs` 的失败消费路径通常立即结束 turn；没有可由另一业务所有者完成的恢复决策阶段。
- **已考虑机制**：订阅失败事件存在终态竞争；在 turn/end 后 Queue 一个“继续”会启动新 turn 并影响 Goal/调度语义；`before` 不能等待外部工作；直接修改 Loop 内存违反所有权。
- **所需能力**：由 Loop 定义“一个已结束步骤，在其 turn 结束前等待一个有界处置结果”的业务合同。候选语义为继续或结束，携带明确工作身份；不让 Loop 识别 Provider 分类，不增加任意 handler 注册框架。
- **可评审的输入与结果**：交接至少关联 turnId、requestId、失败事实序号；恢复命令至少关联同一交接、决定及已采用恢复输入的身份；Loop 的接受结果关联新 requestId 或最终结算。这里列的是数据需求，事件名、域和 binding 由所有者定义。
- **缺失与取消**：必须在交接前确定是否有可用的处置方；交接后失去处置方要有确定的终态。不能“广播后永远等回复”，也不能依赖订阅顺序让 Recovery 抢在 Loop 前面。
- **序号、批次、中断、重载**：接收方按序检查 stop/steer 与工作身份；需要一起生效的事实由所有者原子提交；重载结算未完成交接，不重发请求。
- **所有者与议题**：Loop 业务所有者。此项需要在 [#1210](https://github.com/arcships/dim/issues/1210) 中达成一致，再更新 Loop 自己的合同。不是要求 Timeline、Dispatcher、Session 门面或 rutis 增加基础设施特例。
- **合同落点**：[Loop 的失败处置交接](../../architecture/rust-agent/plugins/Loop.md#a-failed-model-step-and-its-disposer)。

### R2. 输出结算、安全续接与跨请求预算

- **阻塞合同**：现有失败事实和 request-local 重试计数，不足以独立承诺“已交接输出安全续接且不会重置预算”。
- **已考虑机制**：只看 `failure.kind` 不知道输出/工具结算情况；读别的插件内存不允许；全新请求默认再获 8 次尝试会放大预算；复用旧 requestId 会混淆输出和计数。
- **所需能力**：ModelProvider/输出所有者提供可供后续决定使用的结算事实，并接受续接请求的剩余故障链预算。保留一个内部类型来源，不新增全局 BudgetManager。工具结果配对和上下文合法性由各自所有者保证，Recovery 只消费其结果。
- **工具边界**：Loop、请求准备与工具执行所有者须按 §6.1 保证续接请求只有文本输出。Loop 接受处置前确认历史工具批次已结算，请求准备不为本次请求声明或绑定可执行工具，工具执行入口拒绝该请求发出的任何调用。已提交的工具结果仍可作为上下文。
- **影响**：原失败和续接请求保持独立身份，预算关联可从事实重建；取消及重载不恢复旧请求。禁止策略必须覆盖原请求及其续接，不能被新 request 绕过。
- **所有者与议题**：ModelProvider、Loop、输出/历史所有者共同在 [#1210](https://github.com/arcships/dim/issues/1210) 定义边界，各自修改自己的合同。类型和持久化扩展按第 8 节作显式兼容决定。
- **合同落点**：[ModelProvider 的失败链](../../architecture/rust-agent/plugins/ModelProvider.md#failure-chain) 与 [ToolExecution 的执行器拒绝](../../architecture/rust-agent/plugins/ToolExecution.md#toolexecutor)。

### R3. 模型可读恢复输入

- **阻塞规则**：[设计规则 §4.6](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 与输入采用合同要求模型内容通过正式交接进入上下文；当前没有同一 turn 的 Recovery 输入身份与恢复命令配对合同。
- **已考虑机制**：将状态 notice 当 user message、从扩展事件直接渲染上下文、在失败后提交普通 Queue，都改变既有语义。
- **所需能力**：InputQueue/Loop 明确 Recovery 输入何时采用、如何关联处置、取消后如何保证不再驱动请求；若现有输入合同已能完整表达，则直接复用，不增加新输入模式。
- **影响**：只在输入采用后开始下一请求；失败、取消和重载收敛输入与交接，旧输入不得在未来一次用户提交时意外复活。
- **所有者与议题**：InputQueue 和 Loop，在 [#1210](https://github.com/arcships/dim/issues/1210) 定义合同。在该合同证明成立前，不用隐藏消息模拟同一 turn 恢复。
- **合同落点**：[InputQueue 的 handover 输入](../../architecture/rust-agent/plugins/InputQueue.md#handover-inputs) 与 [interrupt 的第二条采用路径](../../architecture/rust-agent/interrupt.md#2-事件契约)。

### R4. 客户端展示

- **已有能力**：`session.model.progress` 和当前进度快照已存在，无需新的基础设施。
- **要求**：App Server/客户端类型按上游合同传递 retry；各 UI 所有者实现第 5 节行为，并在自己的客户端合同中落下对应交互。
- **影响**：不改变执行或模型上下文；Desktop 是独立仓库交付，不以本仓库类型导出代替 UI 验收。

R1–R3 是第二项交付的设计门槛。本文没有批准新增具体服务、保留域事件或 facade 方法；若所有者不能在现有边界内给出有界交接，第二项保留为设计问题，第一项仍可独立交付。

## 8. 兼容性与失败处理

1. 新客户端读取缺少 `failure` / `retry` 的旧记录时沿用原错误展示，不猜测重试次数；已知类型在内部直接使用，不层层补默认字段。
2. RPC 外部边界按合同验证字段；不为了容错将非法次数、非法时间或未知状态转成成功/可重试。
3. 可选字段不等于旧 reader 一定接受。基线 `ModelOutputOutcome` 使用 `deny_unknown_fields`，新增持久失败字段会限制旧二进制读取和降级。采用 #1201 时必须沿用并核实其显式格式/最低 reader 决策；第二项新增事实另作兼容决定，不能默默复用不兼容的格式版本。
4. 冻结事件样本不随功能改写。新增兼容用例覆盖旧记录、扩展记录、严格 reader 和回滚边界，全部使用一次性数据库与独立 home。
5. 诊断只带已允许、限长、清理控制字符的错误字段。禁止复制 headers、完整响应体、请求参数、prompt 或凭据；UI、日志和模型提示均不绕过这个边界。
6. 首期仅提供实时等待和最终失败统计；不承诺恢复已成功请求的完整重试历史，也不承诺断流续接已经可用。

## 9. 影响面与建议任务

| 任务 | 需要到达的位置 | 完成条件 |
| --- | --- | --- |
| A. 复用上游失败事实与请求重试 | `crates/dim-agent-aimux/src/stream.rs`、`crates/dim-agent-capabilities/src/model_provider/`、`host.rs`、`output.rs`、`crates/dim-agent-types/src/events.rs`；辅助模型和摘要调用者 | 与 #1197/#1201/#1207 合并后的实现对齐，不再复制一份分类或重试策略 |
| B. 进度传输 | `crates/dim-agent-runtime/src/contracts.rs`、`crates/dim-app-server/src/tree_protocol.rs`、`connection/subscription.rs`、`crates/dim-client/src/wire.rs`、`npm/client-protocol.d.ts`、`web/src/rpc/protocol.ts` / `read.ts` | 当前值、清除和重新 attach 都能传到消费者 |
| C. CLI 与 Web 展示 | CLI 的 `session.rs` / `session_host.rs`、`model/`、`projection.rs` 与现有活跃状态绘制；Web 的 `stores/chat.ts`、`projection.ts`、`features/chat/` | 两种 TUI、line/JSONL 及 Web 的状态、重复错误和窄屏行为满足第 5 节 |
| D. Desktop 对接 | 公开 App Server 协议及客户端对接合同 | 独立验证真实 daemon 的通知、当前状态展示和停止操作 |
| E. 续接合同 | Loop、InputQueue、ModelProvider/输出、工具执行与历史所有者的文档 | 关闭 R1–R3；明确关闭、禁用、恢复、预算、输入采用与禁止工具执行的全路径 |
| F. ModelRecovery 实现 | 合同接受后新增独立 crate，branch part；实际装配、公开类型及相关消费者 | 与 A–D 分开评审；没有仅接线的 Session/Process 部件，不新增基础设施特例 |

这是一份修改地图，不要求把每行拆成一个 PR。B 中已由上游提供的部分直接复用；C 应形成可观察的端到端交付。新增装配或依赖时才运行 `xtask graph/check` 并更新能力页，生成文件不手改。

## 10. 验收

优先扩展现有流程，不以内部 helper 单测或 mock 调用顺序充当验收。

| 场景 | 可观察结果 | 复用位置 |
| --- | --- | --- |
| 临时故障后成功、连续故障耗尽 | 尝试有界，等待可见；最终成功或一次最终错误 | `host_flows.rs`、aimux `model_flow.rs` |
| Retry-After 为 45 秒、超过剩余预算、等待时停止 | 不提前请求、不超预算、停止后不再调用 | 同上 |
| 鉴权恢复、余额不足、错误请求、未知失败 | 各走自己的动作；没有通用兜底 | 同上及相关凭据流程 |
| 收到 thinking / usage / 部分文本 / 工具参数后失败 | 首项交付不重放原请求，不重复输出或工具 | 现有模型输出与 host 流程 |
| 等待时 steer、Provider reload、Session close | 任务和资源结算，等待状态清除 | `host_flows.rs`、`interrupt_flow.rs` |
| 两客户端、断开重连、切换分支、迟到通知 | 只有一份执行；展示属于正确工作；旧等待不复活 | App Server `transport_flow`、客户端投影流程 |
| 模型与 turn 报同一错误，或报不同错误 | 同一原因只显示一次；不同原因及仅有 turn 的历史不丢失 | CLI 与 Web 投影流程 |
| TUI 窄屏、scrollback、line、JSONL | 输入不被覆盖，不刷屏，不污染 JSON stdout | `dimcode` CLI 流程和一次人工布局检查 |
| 旧记录和扩展失败记录 | 新 reader 读旧记录；旧 reader/降级限制与格式决定一致 | wire contract、一次性存储流程 |
| 部分输出后续接成功或再次失败 | 同一 turn、新 request；已结算工具结果仍可见；预算不翻倍 | 第二项的集成流程 |
| 有工具批次未结算，或续接请求生成了新 callId 的工具调用 | 前者按原失败结算，后者结束续接；两者均不派发新的工具调用 | 第二项的 Loop 与工具执行集成流程 |
| 恢复决定与 stop/steer 交错、恢复输入晚到 | 按消费序号结算，停止后不会被未来输入复活 | 第二项的输入/Loop 集成流程 |
| Recovery 不可用、卸载、重启及 Goal/调度观察 | 无永久 running、无副作用重放、无中途假终态 | 第二项的生命周期与组合流程 |

实现时按变化运行受影响流程；合并前按仓库规范完成必要检查，不在文档阶段启动完整构建或测试。

## 11. Rule check

| 规则 | 回答 |
| --- | --- |
| §1：Capability 和业务位置 | 现有 ModelProvider 是 Capability；拟新增 ModelRecovery 的 branch part 按事实决定续接，也是 Capability。App Server 不持有恢复策略。 |
| §4.1：指令与结果 | Recovery 提出关联当前工作的命令，Loop 决定并写自己的结果；具体合同须由 R1 接受。 |
| §4.2：交接不可撤回 | 旧请求先结算；恢复决定交给 Loop 后不能靠改 Recovery 内存撤回，后续停止走所有者中断合同。 |
| §4.3：before | 不使用 before 阻塞或拦截失败。 |
| §4.4：核心无业务含义 | Timeline/Dispatcher 不识别重试类别；Loop 只拥有步骤交接，不含 Provider 策略。 |
| §4.5：原子批次 | 跨所有者消费不依赖订阅顺序；需要原子性的事实由生产者成批提交，决定性事实最后。具体批次归 R1–R3 所有者。 |
| §4.6：模型上下文 | 倒计时与恢复事实不是上下文；恢复文本仅通过采用后的输入进入模型。 |
| §4.7：公开交互 | 不读取其他插件内存，不直接调用工具或修改 Loop 状态；仅使用公开事实、输入和声明服务。 |
| §4.8：基础设施 | 第一项无需修改基础设施；第二项只提出业务所有者合同需求。本文不向 Timeline、Dispatcher、facade、装配或 rutis 提机制变更。 |
| §4.9：必要事实 | 请求内等待是瞬时进度；跨请求计数和决定影响后续行为才持久化，不为了每秒 UI 造事件。 |
| ADR 002 D1：scope | 当前调用由原所有者管理；续接状态在 branch，不建全局恢复队列或 Session 状态副本。 |
| ADR 002 D2：无接线模块 | 第一项不新增 plugin；第二项只因拥有决策和预算而存在，不加代理/聚合 plugin。 |
| ADR 002 D3：依赖方向 | Recovery 依赖公开核心合同；Loop 不硬依赖可选 Recovery。处置方缺失时如何结算必须在 R1 证明，不靠暗含装配顺序。 |
| ADR 002 D4：配置 | 首期无新增业务设置，不把预算放进 loader 行；未来配置由业务所有者定义。 |
| ADR 002 D5：语言中立 | 本文使用数据语义描述合同，不把 Rust trait 当作通用合同；不预先发明跨语言端口。 |

## 12. 消减与评审决策

已排除：统一异常中心、重复分类、第二套通知通道、重试专用数据库、每秒持久事件、成功重试历史、立即重试按钮、额外最终兜底、自动工具重放和预先搭建的 Recovery 注册框架。

保留结构的理由：ModelProvider 已拥有请求；跨请求恢复若实施，确实拥有独立预算与决定；Loop 必须继续拥有 turn；UI 的当前等待快照可复用现有进度。继续合并这些所有权会使一处失败改变另一所有者的状态。

请评审以下产品和合同选择：

1. 请求内重试留在现有 ModelProvider；独立 plugin 只做跨请求恢复。
2. 第一项先交付，第二项先完成 R1–R3，不以新增框架作为第一项前提。
3. 不自动重试余额、配置或未知错误；鉴权恢复仅由凭据所有者执行。
4. 首期只有实时倒计时和最终失败统计，无永久成功记录或立即重试按钮。
5. 续接首批仅覆盖已输出后的结构化临时模型故障，每 turn 一次，同一故障链共享 8 次调用和 300 秒睡眠预算。
6. 持久字段和新事件必须作显式兼容决定；不能把源码层面的 optional 等同于旧二进制可读。
