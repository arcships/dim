# 模型失败

一次失败的模型请求会报告什么，以及重试策略如何读取它（issue #1197）。

## 1. 一个封闭的 kind，在读取提供方错误的地方决定

aimux 桥接层是唯一持有提供方结构化错误、并看得到完整响应的地方，因此由它决定 `ModelFailureKind`。它使用 origin、HTTP 状态、归一化后的 code、`AiMuxError` 变体、传输层的可重试标记、结束原因和回复的形态，从不用消息文本做匹配。

| kind | 依据 | 是否重试 | 含义 | 动作 |
| --- | --- | --- | --- | --- |
| `network` | 在无条件码决定时，origin 为 Transport 且不是本地拒绝 | 是 | 无法连接提供方，或连接中断 | 检查网络或代理 |
| `rate_limited` | 429；条件码 `rate_limit_exceeded`、`rate_limit_error` | 是；`Retry-After` 在等待预算内时用作延迟 | 提供方正在限流 | 等待后重试 |
| `server_error` | 408、除 501 和 505 外的 5xx；类别标签 `api_error`、`server_error`、`service_unavailable`、`request_timeout`、`timeout`；条件码 `engine_overloaded`、`model_overloaded`、`server_busy`、`overloaded_error` | 是 | 提供方报告服务器错误 | 稍后重试或换一个模型 |
| `malformed_response` | `InvalidResponseData`、`JsonParse`、`InvalidToolInput`、`NoSuchTool`、`TypeValidation`、无法解码的流、始终没有完成的工具参数、以 end-turn 结束且既无任何非空白文本也无工具调用的回复 | 一次 | 提供方发来的响应 Dim 无法使用 | 重试；若反复出现，换一个模型或检查代理或网关 |
| `auth_rejected` | 401、403；条件码 `invalid_api_key`；类别标签 `authentication_error` | 否（保留现有的一次凭据恢复） | 提供方拒绝了凭据 | 重新登录或检查 API key |
| `quota_exhausted` | 402；条件码 `insufficient_quota`、`quota_exceeded`、`billing_error`、`credit_exhausted` | 否 | 账户没有配额了 | 检查套餐或计费 |
| `not_found` | 404 | 否 | 提供方找不到模型或端点 | 检查模型和提供方 base URL |
| `context_too_long` | 413；条件码 `context_length_exceeded`、`context_too_long` | 否 | 对话超过模型可接受的长度 | 压缩对话或新开会话 |
| `content_filtered` | 条件码 `content_policy_violation`、`safety_block`、`moderation_block` | 否 | 提供方的内容过滤拦截了响应 | 改写请求 |
| `invalid_request` | 传输层本地拒绝；400、409、422、501、505、其他 4xx；类别标签 `invalid_request_error`；条件码 `max_tokens_exceeded`；`InvalidArgument`、`InvalidPrompt`、`UnsupportedFunctionality` | 否 | 请求被拒绝 | 查看详情 |
| `output_limit` | `max_tokens` 停止 | 否 | 模型达到输出上限 | 提高输出 token 上限后重试 |
| `unknown` | 其他 | 否 | 模型请求失败 | 查看详情 |

HTTP 连接或流的超时以传输 `ApiCall` 到达桥接层，是 `network`。`AiMuxError::Timeout` 在聊天调用中没有产生者；若它到达桥接层，是 `unknown`，且不重试。

code 比较前先 trim 并转 ASCII 小写。

### 提供方字符串的两种角色

字符串出现在哪个字段不影响它说的是什么，取决于字符串本身。Anthropic 的 `type` 常是条件码，OpenAI 的常是类别标签，而网关会在任何状态下转发其中任一种。

- **条件码**（condition code）描述状态本身无法表达的特定条件：`insufficient_quota`、`context_length_exceeded`、`rate_limit_exceeded`、`engine_overloaded`、`invalid_api_key`、`content_policy_violation`、`max_tokens_exceeded`。这是状态不携带的信息，所以由它决定。
- **类别标签**（class label）只重述一个 HTTP 类别：`api_error`、`server_error`、`invalid_request_error`、`authentication_error`。网关或中转会把这类标签加在它转发的错误上，无论状态是什么。响应带错误状态时，标签只能与之一致或相悖；相悖时，状态才是事实。

### 规则

origin 为 Transport、没有响应状态或响应头且传输层标为不可重试的失败属于本地拒绝（`invalid_request`），例如序列化后的请求体超出字节上限。其他失败按同一规则处理：

1. 条件码决定，不重试的条件码优先于重试的，因此 `insufficient_quota` 与 `rate_limit_error` 同时出现时是 `quota_exhausted`；
2. 否则 origin 为 Transport 的失败是 `network`；
3. 否则由响应上的错误状态（4xx 或 5xx）决定，按上表；
4. 否则由类别标签决定，实际上只发生在流事件上；
5. 否则 kind 为 `unknown`。

4xx、5xx 以外的状态不是错误状态，不说明失败的 kind，第 3 步跳过它，交由类别标签决定。2xx 到达桥接层的情形：成功响应体过大或解析失败，或提供方在 2xx 响应体内报告了错误；3xx 的情形：无法跟随的重定向。

流事件的状态是提供方对事件自身字符串的映射，不是响应状态：事件带显式 4xx 或 5xx 时用它，否则 aimux 由事件的 code 与 type 猜测，提供方什么都匹配不上时得到 500。Dim 无法区分显式状态与猜测值，因此流事件跳过第 3 步，由第 1、4 步决定；读那个状态会把每个无法识别的事件变成可重试的 `server_error`。501 和 505 是 `invalid_request` 出自第 3 步的状态表，不是本规则的例外；同时出现的条件码仍然决定。规则带来两处 kind 变化：只带服务器类别标签的 4xx 不再重试；带 `invalid_request_error` 的 5xx 成为 `server_error` 并被重试。

桥接层产生的每个 `HostError::Failed` 都带事实，包括 `AiMuxError::Timeout`、`InvalidResponseData`/`JsonParse`、`InvalidArgument`/`InvalidPrompt` 和 `UnsupportedFunctionality`。

kind 的好坏取决于提供方的结构化字段。若提供方把配额错误报成一个只有 message 的裸 429，它就会成为 `rate_limited` 并在预算内被无用地重试；`reason` 仍会显示提供方自己的消息。缺失的 code 加到桥接层的 code 列表里，绝不靠文本匹配。

## 2. 事实

`ModelOutputOutcome::Failed` 新增 `failure: Option<ModelFailure>`：

```
ModelFailure {
  kind,             // 第 1 节
  status?,          // 收到响应时的 HTTP 状态
  providerCode?,
  requestId?,
  reason?,          // 一条具体句子，单行，最多 300 字符
  cause: string[],  // 由外到内的链；每帧单行，最多 200 字符，最多 6 帧
  attempts,         // 至少 1
  waitedMs,         // 退避等待总时长
  retryForbidden?,  // 仅当失败策略禁止重试时为 true
  retryAfterMs?     // 提供方要求的等待超过剩余预算，因此停止
}
```

- `failure` 可选。旧事件里不存在；不属于模型提供方调用的失败也没有：agent loop 在重载时对未结算调用的失败（`agent_loop.rs`）和上下文准备失败（`context_preparation.rs`）。字段的大小写和 `optional_non_null` 处理与 `presentation` 一致。
- `retryForbidden` 仅在为 true 时存在。ModelProvider 在失败策略禁止重试结束该请求的失败时写入它；续接会拒绝它，因此该禁止覆盖续接。
- `reason`：响应或流事件失败取提供方的错误消息；传输失败取原因链最内层。以空回复完成的请求背后没有提供方错误，其 `reason` 是该规则写明的固定句子，不引用任何响应内容。JSON 解析的消息内嵌了它读不懂的响应体文本，因此其 `reason` 取错误结构中的解析诊断；错误没有 cause 时为空。工具失败从 `tool_name` 字段命名：`invalid arguments for tool '<tool_name>'` 或 `the model called an unknown tool '<tool_name>'`。`TypeValidation` 不带 reason，因为它的消息内嵌了被校验的值。工具输入、可用工具列表和被校验的值从不存储。
- `cause`：来自 `ApiCallError.message` 和 `ErrorCause` 的链，由外到内，每帧一行。`providerCode` 和 `requestId` 是单行且有界，最多 200 字符；为空则不存在。
- 从不存储：请求 URL、请求头、请求值、响应体或任何凭据。aimux 已丢弃 URL；桥接层不得复制 `url`、`request_body_values` 或 `response_body`，`reason` 和原因帧只是去除了控制字符的文本。
- 旧客户端读取的 `error` 是默认英文行 `<meaning>: <reason>. <action>`，没有 reason 时省略 `: <reason>`。传输层在本地拒绝请求且有 reason 时，只使用该 reason，不将拒绝归因于提供方。不再有 `ApiCall:` 前缀。当该失败使其 turn 结算为 Failed 时，该回合在 `TurnOutcome::Failed.error` 中携带同一字符串，客户端据此可以区分两者并去掉重复。不使 turn 失败的失败（例如输出上限停止）不在 turn 的结果中携带任何内容。
- `max_tokens` 停止保留其 presentation，同时写入 `failure.kind = output_limit`。它不使 turn 失败：Loop 以 `output_limit` 为 reason 把该 turn 结算为 Completed（见 Loop.md「声明了调用却从未执行的失败步骤」一节）。
- 工具参数始终没有完成时写入 `failure.kind = malformed_response`，与 `InvalidToolInput` 对同一缺陷的 kind 相同。
- 以 end-turn 停止、既没有非空白文本也没有工具调用的回复写入 `failure.kind = malformed_response`，原因为 "the response had no text and no tool call"。只有 thinking 不算回答，所以只带 thinking 的回复同样算空回复。该判断落在桥接层流结束处，因为只有它看得到完整响应。是否重试取决于这次尝试交出了什么：只交出过消息开始的，与任何 `malformed_response` 一样重试一次；一旦交出过别的任何东西，包括只有空白的正文片段，就过了可重试的界线（`output.rs` 除消息开始外每个片段都清除 replay safety），因此 turn 带着已记录并展示的失败结束，由用户重新发送。重试在任何情况下都不能跟在已提交的输出之后：请求已提交的内容必须与下一份完整回复一致，thinking 也算在内（`output.rs`），而第二次尝试会带来它自己的 thinking。该规则适用于每个 turn，包括 Loop 与 Goal 的驱动 turn：空回复在那里同样不是一个有效步骤。
- 没有提供方边界的宿主机失败得到 kind `unknown` 和它原本携带的消息。

## 3. 谁来措辞

客户端从 `kind` 出发，经各自的 i18n 组织自己的那一行。事实携带 kind 和数据，而不是句子。`presentation` 保持原意：宿主机或 `ModelFailurePolicy` 提供的措辞优先于客户端的 kind 表。它是覆盖项，不是始终存在的形式。

终端事实的 `error` 是一条完整的兜底行，因此所有已经打印 `error` 的渲染器无需自身改动就能显示更好的行。

失败的请求只显示一行：`<meaning>: <reason>. <action>`，事实没有原因时省略 `: <reason>`。事实记录的尝试多于一次时，行尾加上 `Tried N times over M s.`，总等待时间取整到秒；只有一次尝试则不加。细节仍保留自己的 `tried N times over M s`。措辞来自 kind，该行不带 `model failed:` 前缀。请求已经显示过文本、思考或工具行时，该行以“以上响应不完整。”开头；各客户端从自己持有的行推导，不另外存储。事实出现之前写下的事件仍显示 `error` 行及其前缀。

回合自身的 `failed` 结果不会重复模型失败：错误与同一回合已显示的模型失败相同时不再打印。这种抑制按回合计算：每次回合结束都会忘记上一回合显示过的内容，无论该回合是完成、失败还是被取消，因此之后回合携带相同文本的失败仍会打印。不是模型失败的回合失败仍会打印。

CLI 的 line 模式与 `exec` 只打印这一行。TUI 与 Web 把细节放在一步之外：kind、HTTP 状态、提供方 code、请求 id、尝试多于一次时的 `已尝试 N 次，历时 M 秒`、提供方要求的等待，以及原因链，每帧一行。TUI 的细节加入 `/tools` 循环与错误行的点击展开；Web 从错误行下方的“详情”展开项打开。两者默认收起。

请求等待重试时，TUI 的工作行与 Web 的工作指示显示 `4 秒后第 3/8 次重试 · <meaning>: <reason> · <stop>`，按客户端时钟从 `ModelProgress.retry.retryAtMs` 倒数。该时刻已过而尚无新进度时，改为显示 `Waiting for the provider` 与 kind 的含义，而不是 `in 0 s`。可见的倒计时不是 live region：读屏在一次等待开始或 attempt 变化时听到 `Retrying, attempt 3 of 8: <meaning>`，不会每秒重读。CLI line 模式每次尝试在它写其他状态行的地方写一行；`exec` 不写重试行。重试不产生 transcript 行，之后成功也不产生；失败行的 `已尝试 N 次` 才是持久记录。

## 4. 重试

`invoke` 由 ModelProvider、辅助模型和摘要策略共用，因此只有一份策略，也没有按调用方设置的配置，但结果并不总是相同：replay safety 由消费方自己清除，而辅助模型与摘要策略只是读取流、不交出任何片段，所以只流出 thinking 或空白正文的回复在它们那里会被重试，在主 turn 里不会。

| 项 | 值 |
| --- | --- |
| 重试的 kind | `network`、`rate_limited`、`server_error`、`malformed_response` |
| 尝试次数 | 每个请求合计最多 8 次，含第一次 |
| `malformed_response` | 每个请求只重试一次：第二次 malformed 响应即停止，无论此前发生过什么 |
| 退避 | 第 n 次重试等待 1 秒 × 2^(n−1)，上限 30 秒，再乘以 [0.8, 1.0] 的均匀抖动 |
| `Retry-After` | 在剩余预算内时原样用作延迟，即使超过 30 秒；放不下时立即停止并记录 `retryAfterMs` |
| 等待预算 | 一个请求在失败前合计最多睡眠五分钟；放不进剩余预算的等待会让请求立即停止 |
| 保持不变 | 一旦该次尝试交出了消息开始以外的任何东西（thinking 与空白正文都算）就不再重试；中断取消等待；凭据恢复一次且无延迟，仍计入 8 次上限；`ModelFailurePolicy::forbids_retry`；终止性凭据拒绝会被结清 |

名义退避为 1、2、4、8、16、30、30 秒，合计约 91 秒。这些数字来自 #1197 的故障，它持续了数十秒：8 次尝试可撑过约 90 秒；1 秒基数避免亚秒级的尝试再次撞上同一故障；30 秒上限让最后几次尝试不会相隔数分钟；服务端自己的建议决定等待时长，但不能延长预算。一个请求在失败前合计最多可等待五分钟；被拒绝的凭据恢复从不等待，无论它携带了什么建议。

睡眠期间，等待会发布为 `ModelProgress.retry`：`attempt`（即将运行的尝试，至少为 2）、`maxAttempts`、`retryAtMs`（开始时刻的 Unix 毫秒）、`kind` 和失败的受限 `reason`。`retryAtMs` 是用于展示的墙钟时间，仅供参考；预算是按单调时钟计量的，因此客户端不得把 `retryAtMs` 当作截止时间。ModelProvider 在睡眠前设置它，并在尝试开始、调用结束或等待被取消时清除它。它不是 Timeline 事实：后续决策不读它，终端事实保留 `attempts` 和 `waitedMs`。

### 失败链

续接请求延续它之前失败的那个请求。它的 `context/prepare` 与 `model/call` 携带 `failure_chain {from_request_id, attempts, waited_ms}`：被续接的失败请求、该请求已用的尝试次数和已睡眠的退避时间。ModelProvider 用它们初始化续接请求的重试状态，因此它的第一次调用就是这条链的下一次尝试，整条链共用 8 次调用与 300 秒睡眠的预算。没有尝试或等待预算时，请求不发起调用而直接结算为失败。携带失败链的请求在结算为失败时，其终态 `failure` 报告链的总计，因此失败行的 `Tried N times over M s` 覆盖整条链；这适用于提供方失败、经 `invoke` 的 `map_err` 结算的未声明工具启动拒绝、调用之前或之外的失败，以及重载。`context/prepare` 失败的续接不会到达 ModelProvider：它的 `step/model-output-end` 没有 `failure`，turn 以 Failed 结束。共享的只有尝试次数与等待预算：退避曲线与那一次 `malformed_response` 重试是请求本地的，在续接中重新开始。没有该字段的请求获得全新预算；工具步骤之后的普通请求不属于任何链。

ModelProvider 在调用携带失败链的请求时执行共享的尝试次数和等待上限，不需要独立处置方。ModelProvider 在失败策略禁止重试结束该请求的失败时写入 `failure.retryForbidden`；该字段仅在为 true 时写入，续接会拒绝它。

## 5. 兼容性

字段是增量的。没有 `failure` 的二进制无法解码带它的事件，因为 `ModelOutputOutcome` 拒绝未知字段。这与 `presentation` 的前向兼容限制相同，因此跨此变更的降级不受支持，正如它此前对 `presentation` 也不受支持。

同一决定覆盖续接字段、重试禁止与 handover 输入。`ContextPrepareData` 与 `ModelCallData` 上可选的 `failure_chain`、`InputMessageData` 上可选的 `handover`，以及 `ModelFailure` 上可选的 `retryForbidden` 都是增量的；没有它们的二进制无法读取携带它们的事件，因此跨此变更的降级不受支持。每个新增都在契约测试中登记该决定及其兼容用例；冻结样本保持不变。
