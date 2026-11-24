# 压缩触发点与原文保留

状态：[#128](https://github.com/arcships/dim/issues/128) 的研究建议。本文不改代码；建议的常量和规则改动见 §7。§1 的第 1、3、5、7、8 行已在 [ContextCompaction](../plugins/ContextCompaction.md) §5.1 与 §5.3 实现，第 4 行由 #169 实现；第 6、9 行保持现有行为。
设计文档：[ContextCompaction](../../../design/plugins/ContextCompaction.md)。英文版：
[English](../../../design/research/compaction-trigger.md)（以英文版为准）。

每条结论都标明证据类型：

- **[代码]**：读自本仓库 `origin/main`（e541e04）。
- **[实测]**：为本文测量，方法见 §6。
- **[文档]**：其他工具或厂商公开的文档或源码，附链接。
- **[判断]**：本文的建议或推断，未经验证。

## 1. 结论

| # | 问题 | F1 现状 | 建议 | 置信度 |
| --- | --- | --- | --- | --- |
| 1 | 触发形状 | `0.8 × B`，`B = 窗口 − 输出` | 比例与绝对预留并用：`threshold = B − reserve`，`reserve = min(max(0.10 × B, 16,000), 0.20 × B)` | 中 |
| 2 | 窗口差异 | 单一比例 | 同一公式：32k–200k 窗口上表现为绝对预留，大窗口上表现为 90 %。不按窗口分档，暂不给 1M 窗口设绝对上限 | 中低 |
| 3 | 保留单位 | 最近 K = 2 轮 | 按 token：保留能放进 `0.25 × B` 的最长尾部（含 keyed baseline），取满足预算的最早合法切点 | 中 |
| 4 | 尾部超出预算 | 只有预算恢复可切进最近 K 轮 | 任何触发在无切点满足时都取最晚合法切点，消除长单轮的"死区" | 高（死区是代码事实） |
| 5 | keyed baseline | 保留规则不计 | 计入保留预算，因为每次切点后都会重放 | 中 |
| 6 | 提示缓存 | 未考虑 | 成本不决定触发点：提前压缩只有满足本文没有测量的收支平衡条件才划算（§5.1）。端口已按 §2 标记断点；剩下的后续是让摘要请求复用对话前缀 | 中 |
| 7 | 恢复 | `validate` 用完整本地估算对比整个窗口 | 有 provider 基线时改用与触发相同的估算并对比 `B`；没有基线时仍是完整估算，误差仍在（§6.4） | 中 |
| 8 | 手动 `/compact` | 同样受 K = 2 限制，短会话会跳过 | 切在最近一轮的开始（若 token 规则保留更少则用它） | 中低 |
| 9 | 估算器 | `bytes / 4`，每图 1,600 | 两者保持不变。预留吸收的是 provider 基线之上增量的误差；它是暂定经验值，不能覆盖所有无基线请求（§6.4） | 中高 |

## 2. F1 目前如何决定 [代码]

- `ProductContextModel`（[context_model.rs](../../../../crates/dim-agent-capabilities/src/context_model.rs)）：
  `output_tokens` 取 `config.maxTokens`，否则取目录的 `maxOutputTokens`；`threshold_tokens =
  0.8 × (context_window − output_tokens)`；`count_tokens` 是 system、工具声明和消息各部分序列化 JSON
  的字节数除以 4，每个图片部分加 1,600。下文记 `B = context_window − output_tokens` 为输入预算。
- `ContextPolicy::prepare`（[context_policy.rs](../../../../crates/dim-agent-capabilities/src/context_policy.rs)）
  在每个请求边界把估算与阈值比较一次。估算为上一个前缀相同请求的 provider `prompt_tokens`
  加新消息的本地 `count_tokens`（`provider_baseline`）；没有可用基线时（首个请求、压缩完成后、模型或
  配置变化后）用完整本地估算。Aimux 对 Anthropic 报告的 `prompt_tokens` 是
  `input + cache_creation + cache_read`，所以基线是完整提示。
- 保留：`retain_limit` 是最近 `RETAINED_TURNS = 2` 轮中最早一轮的 `turn/start`；轮数不足时没有上限。
  `CompactionInput::cut` 取上限之前最晚的合法切点，只有 `fallback`（预算恢复）时才取全局最晚合法切点，
  否则策略返回 `skipped: nothing_to_compact`。
- 恢复：ModelProvider 每次调用模型前调用 `ContextPolicy::validate`，它用**完整本地估算**
  （`estimate(&request, model, &[])`，无基线），仅当 `≥ context_window` 时带预算事实失败。Loop 随后以
  `recovery` 重试一次，强制以 `fallback` 压缩。
- provider 报告的溢出（`context_length_exceeded`、Anthropic 的 "prompt is too long"）被归为永久错误且不带
  预算事实（[stream.rs](../../../../crates/dim-agent-aimux/src/stream.rs)），回合直接失败，不进入恢复。
  设计把 provider 溢出留在 F1 之外（§10）。
- read 与 exec 工具每个结果最多内联 30,000 字符（`READ_INLINE_MAX_CHARS`、`INLINE_OUTPUT_CHAR_LIMIT`）。
- 摘要请求使用自己的 system prompt，把转录作为一条用户消息的文本发送
  （[summary_strategy.rs](../../../../crates/dim-agent-capabilities/src/summary_strategy.rs)）。
- provider 端口在一个 Anthropic 请求上最多标记两个缓存断点（[request.rs](../../../../crates/dim-agent-aimux/src/request.rs)，
  [Anthropic 提示缓存断点](../../../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)）：System 末尾
  （工具声明与 System 合成一个缓存前缀），以及最后一条用户或工具结果消息的最后一个块（每一轮读取上一轮前缀、
  只写入新的尾部）。确切规则——显式 System 断点原样保留，消息断点只在请求仍不超过 API 的四个时加——以 crate
  README 为准。两者都用 API 默认的五分钟生命周期；自动前缀缓存的厂商（OpenAI 及多数 OpenAI 兼容厂商）
  不加断点，也不受影响。

## 3. 触发形状

### 3.1 现有比例在不同窗口下的表现

| 模型（窗口 / 输出） | `B` | `0.8 × B` | 余量 `B − threshold` | 建议阈值 | 建议余量 |
| --- | --- | --- | --- | --- | --- |
| 32k / 8k | 24,000 | 19,200 | 4,800 | 19,200 | 4,800（20 % 上限） |
| 128k / 8k | 120,000 | 96,000 | 24,000 | 104,000 | 16,000 |
| 200k / 64k | 136,000 | 108,800 | 27,200 | 120,000 | 16,000 |
| 400k / 128k | 272,000 | 217,600 | 54,400 | 244,800 | 27,200 |
| 1M / 128k | 872,000 | 697,600 | 174,400 | 784,800 | 87,200 |

[判断] 纯比例有两个缺点：小窗口的余量只有几千 token，相当于一个工具结果；大窗口的余量超过 100k，
对误差毫无必要，只让压缩更早。余量要吸收的东西大致是绝对量：上次 provider 计数之后新增内容的估算误差
（§6），它受工具输出上限约束，而不是窗口大小。

### 3.2 其他工具的公开做法

| 工具 | 触发 | 保留 | 来源 |
| --- | --- | --- | --- |
| Claude Code | 未覆盖时在"对话达到模型上下文上限"时压缩；1M 窗口模型约在 967K 压缩；用户只能调低（`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`、`autoCompactWindow` 100K–1M） | 不保留原文轮次：摘要后重新注入 CLAUDE.md、最多 5 个最近文件（每个 ≤ 5,000 token）和 skill 正文（每个 5,000，总计 25,000） | [model-config](https://code.claude.com/docs/en/model-config#context-window-and-auto-compaction)、[env-vars](https://code.claude.com/docs/en/env-vars)、[context-window](https://code.claude.com/docs/en/context-window#what-survives-compaction) |
| Codex CLI | `auto_compact_token_limit` 默认为上下文窗口的 90 %，配置值会被夹到此值以下；与会话的 token 用量比较 | 本地压缩保留最近的用户消息，最多 `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000`（token 预算，本地估算 bytes / 4） | [`openai_models.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/protocol/src/openai_models.rs)、[`compact.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/core/src/compact.rs)、[`context_window.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/core/src/session/context_window.rs)、[配置样例](https://developers.openai.com/codex/config-sample) |
| Aider | 聊天历史超过 `max_chat_history_tokens = min(max(max_input_tokens / 16, 1024), 8192)` 时摘要 | 保留不超过该预算一半的尾部（按 token），头部以 assistant 消息结束 | [`models.py`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/models.py)、[`history.py`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/history.py) |
| OpenHands SDK | `LLMSummarizingCondenser`：事件数超过 `max_size = 240`，或 token 超过 LLM 有效输入上限 | 保留前 `keep_first = 2` 个事件和约一半的尾部（`max_size // 2` 个事件，或降到 token 上限一半以下） | [`llm_summarizing_condenser.py`](https://github.com/OpenHands/software-agent-sdk/blob/6530325e9dd0ea1b6b4f1994d7dab52764cfc3f1/openhands-sdk/openhands/sdk/context/condenser/llm_summarizing_condenser.py)、[文档](https://docs.openhands.dev/sdk/arch/condenser) |
| Anthropic API 阈值压缩 | 绝对值：默认 `input_tokens` 触发 150,000，最小 50,000 | 仅能通过暂停后重新插入保留最近轮次 | [compaction-threshold](https://platform.claude.com/docs/en/build-with-claude/compaction-threshold) |
| Anthropic API 保留尾部压缩 | 调用方决定 | 没有参数决定保留哪些轮；示例保留 2 轮，并提醒保留的轮次以完整长度发送 | [compaction-keep-recent-turns](https://platform.claude.com/docs/en/build-with-claude/compaction-keep-recent-turns) |

[文档] 在对话内压缩的编码代理触发得都晚：Codex 在窗口的 90 %，Claude Code 接近上限。Codex 用会话的
token 用量比较；Claude Code 的文档没有说明如何计数。Anthropic API 默认是与 1M 窗口无关的绝对 150k，
反映的是上下文质量与成本考虑，而不是溢出安全。

### 3.3 建议

[判断] 所有窗口用同一个公式：

```
B         = context_window − output_tokens          （不变）
reserve   = min(max(0.10 × B, 16_000), 0.20 × B)
threshold = B − reserve
```

- **16,000 绝对下限。** 单个内联工具结果上限 30,000 字符，实测最坏低估约 5,000 token（§6.2）。16,000
  可覆盖一批约三个最坏结果，或一个结果加用户输入与 system、工具声明的变化。
- **10 % 比例。** 这是暂定的经验值，不是经过证明的上界。它面向没有 provider 基线的完整估算请求（首个请求、
  刚压缩完、前缀/模型/配置变化之后），此时误差作用于整个请求。本文没有测混合转录，所以不给出这种情况的
  误差上界（预留覆盖不到的情形见 §6.4）。400k 及以上窗口时这也是 Codex 的触发点。
- **20 % 上限。** 小窗口承受不起 16k；上限让 32k 模型保持现状。
- **1M 窗口暂不设绝对上限。** 两个公开数据点相互矛盾（Claude Code 接近 1M 才压缩，Anthropic API 默认
  150k）。在 Dim 没有评估摘要质量与长上下文退化之前，更早的触发更适合作为用户设置。这是本建议中最不
  确定的部分。
- [判断，待定] `output_tokens` 默认取目录的 `maxOutputTokens`，所以 200k 窗口、64k 输出的模型
  `B = 136k`，输出上限相对窗口很大的模型会比窗口看起来早得多地压缩。Anthropic 在 Claude 4.5 及以后接受
  `input + max_tokens > window`，生成到上限时以 `model_context_window_exceeded` 停止
  （[context-windows](https://platform.claude.com/docs/en/build-with-claude/context-windows#context-window-overflow-behavior)），
  但并非所有厂商如此，因此本文保留完整的输出预留。应在掌握各厂商事实后再议。

## 4. 保留

### 4.1 K = 2 轮的问题 [代码、判断]

- **轮的大小没有上界。** Dim 的一轮是一条用户输入加任意多次模型调用和工具批。一个长的自主轮可能比整个
  预算还大；两个短轮可能只有几百 token。按轮计数要么保留过多，要么过少。
- **死区。** 只要正在运行的轮属于最近两轮之一，阈值压缩就不能切进它：`retain_limit` 在上一轮开始处或
  之前；轮数不足两轮时没有上限，策略在阈值之上的每个新边界都返回 `skipped: nothing_to_compact`。请求
  持续增长，直到 `validate` 的完整本地估算达到整个窗口，恢复才切进该轮。如果内容被低估（日志、lock
  文件、部分分词器上的 CJK；§6），provider 会先以永久的溢出错误拒绝请求，回合失败。
- **baseline 不在计数内。** keyed baseline（切点前每个 keyed 输入块的最新值，
  [history.rs](../../../../crates/dim-agent-capabilities/src/history.rs)）在每次投影时都重放在 replacement
  之后、保留条目之前。它从不被摘要，是每次压缩后保留规则看不到的固定成本。

### 4.2 建议 [判断]

1. **按 token 计数。** `keep_budget = 0.25 × B`，用窗口事实的 `count_tokens` 计算切点处的 baseline 加
   切点之后的条目。
2. **选择切点。** 在合法切点中（`legal_cuts` 不变：轮开始和放置条目的事件，不拆开工具调用与其结果或
   上下文）取保留尾部能放进 `keep_budget` 的最早切点。能放下时保留完整的最近轮次，运行中的轮过大时切进它。
3. **都放不下时**（一个巨大的工具结果，或 baseline 本身超出预算）：任何触发都取最晚合法切点，不只限于
   恢复。`View::compacted` 的缩减检查不变，仍拒绝没有缩小请求的结果。
4. **为什么是 25 %。** 压缩后上下文约为 system 与工具 + 摘要 + `0.25 × B`，到下次触发前还有约
   0.5–0.6 × B 的空间。Aider 和 OpenHands 保留约一半的摘要预算，Codex 保留 20k token 的用户消息，
   Anthropic 的示例保留两轮；它们都没有为取值给出证据，所以 25 % 是判断：在限制压缩频率（更晚的触发和
   更小的尾部都意味着更少的压缩）的同时，保留大部分最近工作的原文。
5. **切进一轮会把该轮的用户输入纳入摘要。** 摘要提示要求记录"用户的目标和约束"，keyed baseline 保留
   常驻状态，因此可以接受；总是原文保留运行中那轮的输入（类似 Codex 保留用户消息）是可能的改进，不在本
   建议内。

## 5. 提示缓存、恢复与手动压缩

### 5.1 提示缓存

- [文档] 前缀缓存精确匹配，历史被改写时从第一个变化的块开始失效。Anthropic：默认 5 分钟寿命，写入为输入
  价的 1.25×（1 小时为 2×），读取为 0.1×（Opus 5.5 与 Sonnet 5.5 为 0.05×）
  （[prompt-caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)）。OpenAI 从 1,024
  token 起自动缓存，缓存输入折扣"最高 95%"（[指南](https://developers.openai.com/api/docs/guides/prompt-caching)）。
  Claude Code 把压缩列为使缓存失效的操作之一，并用对话自己的 system prompt、工具和历史加一条最终指令发送
  摘要请求，使其读取已缓存的前缀
  （[prompt-caching](https://code.claude.com/docs/en/prompt-caching#compacting-the-conversation)）。Anthropic
  API 建议在 system prompt 末尾放断点，使其在多次压缩间保持缓存
  （[compaction-threshold](https://platform.claude.com/docs/en/build-with-claude/compaction-threshold)）。
- [代码] Dim 的投影把 replacement 放在最前，然后是 baseline 和保留尾部，所以压缩最多保留 system 与工具前缀
  的缓存。摘要请求的 system prompt 不同且把转录渲染成文本，与对话没有共同前缀。它的全部输入——策略自己的
  System 与承载转录的那条用户消息——按 1.25× 写入缓存，而被替换的转录不会再被读取，因为压缩会替换掉它。
  摘要请求那条用户消息上的消息断点就是这次写入的标记；System 断点写不进缓存，因为策略自己的 System 短于
  该模型的最小可缓存长度
  （[Anthropic 提示缓存断点](../../../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)）。
  因此摘要调用的输入按 1.25 倍输入价计费，而不是 1 倍，这是把每个请求同样标记换来的已知代价。
- [判断] 提前压缩是否更贵，取决于之后发生什么。断点到位后，普通调用以读取价 `r`（0.1，Opus 5.5 与
  Sonnet 5.5 为 0.05）读取与上一次调用共有的前缀，只按 1.25× 写入新增的尾部，所以提前压缩在每次后续调用上
  省下的是被移除 token 的 `r` 倍，而不是它们的全价。压缩要付摘要请求的全部输入（1.25× 写入且不会被读取）、
  摘要的输出，以及按 1.25× 重建新前缀（replacement、baseline 与保留尾部）。满足
  `1.25 × 摘要输入 + 摘要输出 + 1.25 × 重建前缀 < r × Σ（旧提示 − 压缩后提示）` 时压缩就值得，Σ 取之后本该
  携带旧前缀的各次调用。例如 `r = 0.1` 时，`C = 100k` 压到 `S = 20k`，每次后续调用省 8k 个输入价单位；摘要
  输入约为被替换范围、重建前缀为 `S` 的压缩在摘要输出之前约花 15 万个输入价单位，约二十次调用即可收回；
  `r = 0.05` 时每次只省 4k，收回所需的调用次数约翻倍。之后有很多小调用且缓存保持有效时，提前压缩可以收回
  成本；后续调用很少、或调用之间缓存过期，则晚触发更省。这取决于调用次数、增长速度和缓存寿命，本文都没有测，
  所以成本不决定触发点。§3.3 的晚触发依据的是溢出安全（§3）和保留模型可能还需要的上下文，它仍是经验值。
  更便宜的杠杆是摘要请求：复用对话前缀（Claude Code 的做法）让它以 `r` 读取对话已缓存的前缀，而不是把整段
  转录按 1.25× 写入，这一部分的输入价按公开价格比例算相差一个数量级（这是算术，不是测量）。这改变策略的请求
  形状，属于单独的后续工作。

### 5.2 预算耗尽恢复

- [代码] 恢复只从 `validate` 开始，它用完整本地估算对比整个窗口，而触发用 provider 基线和 `B`。两者可能
  双向不一致：以代码为主的上下文（估算偏高 1.3–1.4 倍）会在真实输入约为窗口 70 % 时触发恢复，用最晚合法
  切点替换掉保留的尾部；以日志或 lock 文件为主的内容（在 Qwen 上估算最多偏低 1.6 倍）会让真实输入先达到
  窗口，而 provider 错误不会被恢复。
- [判断] 有 provider 基线时，`validate` 应与触发使用同一估算（分支已提交事件上的 provider 基线加增量），
  并在 `estimate > B` 时失败。没有基线时仍是完整本地估算，误差也仍在（§6.4）；这项改动并不消除这种依赖。这样恢复的含义就是"请求放不进声明的预算"，与触发和摘要请求自己的分块预算一致。在 §3.3 和
  §4.2 落地后，恢复是单次增量超过预留时的兜底；它使用的最晚合法切点规则保持不变。
- [判断] provider 报告的溢出最终也应映射到同一恢复（设计中单独的 Aimux issue）。在此之前，预留是防止
  低估的唯一保护。

### 5.3 手动 `/compact`

- [代码] 手动压缩使用同一个 `retain_limit`。只有一两轮的会话会返回 `skipped: nothing_to_compact`，这不符合
  主动要求压缩的用户的预期。
- [判断] 手动步骤应切在最近一轮的开始（只保留运行中或最后一轮），若 token 规则保留更少则用它。Claude Code
  的 `/compact` 完全不保留原文轮次，并建议在自然停顿时运行而不是等待自动触发，此时成本可预期
  （[prompt-caching](https://code.claude.com/docs/en/prompt-caching#compacting-the-conversation)）。

### 5.4 预留到底留给谁，以及摘要请求的生成长度 [代码、维护者评审]

触发点决定了发起压缩时上下文有多大。压缩本身也是一次模型调用，它的输出（摘要加上可能的思考）受 `max_tokens` 约束。F1 已经按这个约束来规划：
- `ProductSummaryStrategy` 用 `count_tokens` 对整个摘要请求计数，把历史装成每块都不超过 `context_window − output_tokens` 的分块；
- 再逐块折叠，带上前一块的摘要。

所以压缩不需要上下文为它自己留空间：上下文可以接近拉满，摘要时仍按模型可用的生成长度拆分。触发预留因此只保护**下一次正常请求**，也就是它的输出额度（已经包含思考），加上 §6 的估算余量。§3.3 里 16,000 token 的下限是估算余量，不是留给压缩调用的空间。

评审中定下的两点：
- **机制缺陷，单独在 #169 修复。** 摘要回复因 `max_tokens` 截断时，现在会被当作完整摘要接受（`summary_strategy.rs` 的 `collect`）。思考可能用掉大部分额度，被截断的摘要会悄悄替换掉历史。`max_tokens` 停止不能算完成：要把这一块拆小重试，到最小尺寸仍截断就失败。§4.1 的死区也在同一个 issue 里修。
- **属于配置和提示词，不是机制（#170）。** 摘要请求要不要思考（沿用、关闭或降低，视模型是否支持）是策略的设置。目标摘要长度是摘要提示词里的一条要求。两者都靠摘要质量评估来定，不在代码里加约束。

## 6. 估算误差

### 6.1 整体样本 [实测]

方法：取本仓库的样本（英文和中文设计文档、中文文档中的纯中文行、Rust 源码、事件 JSON fixture、
`Cargo.lock`、`git log --stat`），按行切成约 4 KB 的块。每块包装成一个工具结果消息部分，按 `count_tokens`
测量的形状序列化为紧凑 JSON（非 ASCII 字符不转义），再除以 4。真实计数来自 `tiktoken` 0.14.0
（`o200k_base` 对应 GPT-4o 及以后，`cl100k_base` 对应 GPT-4）和 Hugging Face `tokenizers` 0.23.3 加载
Qwen2.5-7B-Instruct 与 DeepSeek-V3 公开的 `tokenizer.json`，只计原始块文本（不含消息框架，略有利于估算）。
Claude 的分词器不公开，token 计数 API 需要凭据，因此未测 Claude。

单元格为真实 token 数，括号内为 `估算 / 真实`（大于 1 表示高估）：

| 样本 | KB | 估算 | o200k | cl100k | Qwen2.5 | DeepSeek-V3 |
| --- | --- | --- | --- | --- | --- | --- |
| Rust 源码 | 81 | 21,917 | 15,288 (1.43) | 15,280 (1.43) | 15,314 (1.43) | 16,361 (1.34) |
| JSON 事件 | 47 | 13,944 | 11,689 (1.19) | 11,677 (1.19) | 12,336 (1.13) | 11,916 (1.17) |
| 英文 Markdown | 126 | 33,646 | 29,517 (1.14) | 29,529 (1.14) | 29,689 (1.13) | 30,498 (1.10) |
| 中文 Markdown | 121 | 31,782 | 33,363 (0.95) | 40,161 (0.79) | 33,090 (0.96) | 31,730 (1.00) |
| 纯中文行 | 17 | 4,311 | 4,525 (0.95) | 6,080 (0.71) | 4,212 (1.02) | 3,833 (1.12) |
| `git log --stat` | 77 | 20,773 | 23,301 (0.89) | 23,323 (0.89) | 27,445 (0.76) | 24,124 (0.86) |
| `Cargo.lock` | 88 | 25,388 | 33,586 (0.76) | 33,514 (0.76) | 40,723 (0.62) | 35,304 (0.72) |

结论：

- `bytes / 4` 高估代码（缩进被合并成少量 token）和 JSON，对散文接近；对哈希和路径密集的输出（lock 文件、
  日志）最多低估 38 %。
- CJK 每字符 3 字节，`bytes / 4` 即每字符 0.75 token。现代分词器（o200k、Qwen2.5、DeepSeek-V3）在 ±12 %
  以内；`cl100k` 低估 29 %。
- JSON 包装使估算增加 3–15 %（转义的引号和换行），相当于一个小的内置余量。

### 6.2 单个被截断的工具结果 [实测]

各样本的前 30,000 字符（read 与 exec 工具的内联上限），方法同上：

| 样本 | 估算 | 真实（o200k / cl100k / Qwen2.5 / DeepSeek-V3） | 最坏低估 |
| --- | --- | --- | --- |
| `Cargo.lock` | 8,303 | 11,041 / 11,011 / 13,318 / 11,651 | 5,015 |
| 中文 Markdown | 12,119 | 13,014 / 15,509 / 13,092 / 12,547 | 3,390 |
| `git log --stat` | 7,722 | 9,257 / 9,277 / 11,043 / 9,576 | 3,321 |
| 英文 Markdown | 7,658 | 7,010 / 7,010 / 7,046 / 7,340 | 无 |
| Rust 源码 | 7,721 | 5,720 / 5,719 / 5,738 / 6,057 | 无 |

### 6.3 其他误差来源 [文档、判断]

- **图片。** Anthropic 按 `⌈w/28⌉ × ⌈h/28⌉` 计视觉 token，标准档模型最多 1,568，Claude 4.7 及以后最多
  4,784（[vision](https://platform.claude.com/docs/en/build-with-claude/vision#resolution-and-token-cost)）。
  1,600 对标准档准确，对高分辨率档最多偏低 3 倍。Dim 发送前不缩放图片。由于首次调用后触发使用 provider
  基线，只有新加入的图片被估算；保持 1,600，由预留吸收少量图片的误差。
- **思考内容。** thinking 部分及其签名会被序列化进估算。部分模型会剥离之前轮次的 thinking 且不计数
  （[context-windows](https://platform.claude.com/docs/en/build-with-claude/context-windows#the-context-window-with-thinking)），
  这时估算偏高，是安全的。
- **Claude。** 未测量。第三方分析称 Opus 4.7 的分词器对同一文本产生的 token 比早期 Claude 模型多（例如
  [OpenRouter](https://openrouter.ai/blog/insights/opus-47-tokenizer-analysis/)）；我未能在 Anthropic 文档中
  核实该数字。

### 6.4 对余量的影响 [判断]

- 有 provider 基线时只估算上次调用后的增量；每个被截断结果的实测最坏误差约 5k token，16k 下限覆盖约三个。
- 没有基线时误差随整个请求增长，预留不能覆盖所有情形。§6.1 的估算本身已含序列化后的 JSON，所以不能再另加
  一份包装余量。以报告中的中文 Markdown/cl100k 比例（`31782 / 40161 ≈ 0.79`）为例：窗口 128,000、输出
  4,096 时建议的触发点是 107,904，一个 130,000 token 的无基线输入会被估成约 102,900，低于触发点和建议的
  校验预算，而输入本身已经超过窗口。这是按实测比例做的算术，不是新的 provider 测量。无基线请求是会出现的：
  首个请求、刚压缩完、前缀/模型/配置变化之后（`ContextPolicy::estimate` 会退回 `count_tokens(request)`）。
  在 provider 报告的溢出能把预算事实带进恢复之前（§5.2 的 Aimux 后续），这一局限一直存在；预留是暂定的经验值。
- 按模型引入分词器得不偿失：要增加各厂商词表，仍无法在本地计数 Claude，而 provider 基线已把误差限制在
  增量上。Codex 用同样的 `bytes / 4` 近似（[`truncate.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/utils/string/src/truncate.rs)
  中的 `APPROX_BYTES_PER_TOKEN = 4`）。

## 7. 对 F1 的改动建议

作为后续代码改动，与设计文本（[ContextCompaction](../../../design/plugins/ContextCompaction.md) 的 §5.1、
§5.3、§10）以及断言 K 轮上限的流程测试一起更新。

| 文件 | 现状 | 建议 |
| --- | --- | --- |
| `crates/dim-agent-capabilities/src/context_model.rs` | `const THRESHOLD_RATIO: f64 = 0.8;`，`threshold_tokens = THRESHOLD_RATIO × B` | `const RESERVE_RATIO: f64 = 0.10;` `const RESERVE_MIN_TOKENS: f64 = 16_000.;` `const RESERVE_MAX_RATIO: f64 = 0.20;`，`threshold_tokens = B − (RESERVE_RATIO × B).max(RESERVE_MIN_TOKENS).min(RESERVE_MAX_RATIO × B)`，其中 `B = (context_window − output_tokens).max(0)` |
| 同上 | `BYTES_PER_TOKEN = 4.`、`IMAGE_TOKENS = 1_600.` | 不变 |
| `crates/dim-agent-capabilities/src/context_policy.rs` | `const RETAINED_TURNS: usize = 2;`，`retain_limit(context)` 按轮数 | `const RETAINED_SHARE: f64 = 0.25;`，`retain_limit` 为 baseline 加保留条目（用窗口事实计数）`≤ RETAINED_SHARE × B` 的最早合法切点，否则为最晚合法切点。这需要 `compact_step` 先算切点再定上限（它已有模型事实） |
| 同上，`host.rs` 中的 `CompactionInput::cut` | 只有 `fallback` 时取最晚合法切点 | 形状不变：按 token 规则，只要存在合法切点就存在上限内的切点，`fallback` 只保留"恢复"的含义 |
| 同上，手动步骤 | 与自动相同的上限 | 上限取最近一轮的 `turn/start`，若 token 规则保留更少则用它 |
| `ContextPolicy::validate` | 完整本地估算，`≥ context_window` 时失败 | 用分支已提交事件上的 provider 基线估算，`> B` 时失败 |
| 后续 issue | — | 摘要请求复用对话前缀；provider 溢出映射到恢复（已是单独的 Aimux issue） |

不建议：按窗口分档的规则、1M 窗口的绝对上限、本地分词器、更改图片常量。
