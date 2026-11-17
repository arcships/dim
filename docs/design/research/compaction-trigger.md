# Compaction trigger point and verbatim retention

Status: research recommendation for [#128](https://github.com/arcships/dim/issues/128). No code
changes here; the proposed constant and rule changes are in §7. Rows 1, 3, 5, 7 and 8 of §1 are
implemented in [ContextCompaction](../plugins/ContextCompaction.md) §5.1 and §5.3, row 4 by #169;
rows 6 and 9 keep the current behavior. Design:
[ContextCompaction](../plugins/ContextCompaction.md). Chinese copy:
[中文](../../zh/design/research/compaction-trigger.md).

Every statement carries its kind of evidence:

- **[Code]**: read from this repository at `origin/main` (e541e04).
- **[Measured]**: measured for this note; the method is in §6.
- **[Documented]**: stated in public documentation or source of another tool or provider, linked.
- **[Judgment]**: my recommendation or inference. Not verified.

## 1. Summary

| # | Question | F1 today | Recommendation | Confidence |
| --- | --- | --- | --- | --- |
| 1 | Trigger shape | `0.8 × B`, where `B = window − output` | Both a ratio and an absolute reserve: `threshold = B − reserve`, `reserve = min(max(0.10 × B, 16,000), 0.20 × B)` | Medium |
| 2 | Different windows | One ratio | The same formula; it behaves as an absolute reserve for 32k–200k windows and as 90 % for large windows. No separate per-class rule and no absolute cap for 1M windows now | Medium-low |
| 3 | Retention unit | Last K = 2 turns | Tokens: keep the longest tail (keyed baseline included) that fits `0.25 × B`, cut at the earliest legal cut that fits | Medium |
| 4 | Tail larger than the budget | Only budget recovery may cut inside the last K turns | Every trigger takes the latest legal cut when no cut fits; this removes the "dead zone" of a long single turn | High (the dead zone is a code fact) |
| 5 | Keyed baseline | Not counted by retention | Counted in the retention budget, because it is replayed after every cut | Medium |
| 6 | Prompt caching | Not considered | Cost does not decide the trigger: an early compaction pays off only under a break-even condition this note did not measure (§5.1). The port marks the breakpoints (§2); the remaining follow-up is a summarizer request that shares the conversation prefix | Medium |
| 7 | Recovery | `validate` compares a full local estimate with the whole window | With a provider baseline, use the same estimate as the trigger and compare it with `B`; without one the full estimate and its error remain (§6.4) | Medium |
| 8 | Manual `/compact` | Same K = 2 turn limit, so it skips on short sessions | Cut at the start of the latest turn (or the token rule when that is smaller) | Low-medium |
| 9 | Estimator | `bytes / 4`, 1,600 tokens per image | Keep both. The reserve absorbs the error of a delta over a provider baseline; it is a provisional heuristic that does not cover every no-baseline request (§6.4) | Medium-high |

## 2. How F1 decides today [Code]

- `ProductContextModel` ([context_model.rs](../../../crates/dim-agent-capabilities/src/context_model.rs)):
  `output_tokens` is `config.maxTokens` or the catalog `maxOutputTokens`; `threshold_tokens =
  0.8 × (context_window − output_tokens)`; `count_tokens` is the serialized JSON bytes of system,
  tools and message parts divided by 4, plus 1,600 per image part. Call `B = context_window −
  output_tokens` the input budget.
- `ContextPolicy::prepare` ([context_policy.rs](../../../crates/dim-agent-capabilities/src/context_policy.rs))
  compares an estimate with the threshold once per request boundary. The estimate is the last
  provider `prompt_tokens` for an identical-prefix request plus a local `count_tokens` of the new
  messages (`provider_baseline`), or a full local count when there is no usable baseline (the first
  request, after a completed compaction, after a model or configuration change).
  Aimux reports `prompt_tokens` for Anthropic as `input + cache_creation + cache_read`
  (`aimux-providers`' `anthropic/usage.rs`), so the baseline is the full prompt.
- Retention: `retain_limit` is the `turn/start` of the oldest of the last `RETAINED_TURNS = 2`
  turns; with fewer turns there is no limit. `CompactionInput::cut` takes the latest legal cut at or
  before the limit, and only under `fallback` (budget recovery) the latest legal cut overall.
  Otherwise the strategy returns `skipped: nothing_to_compact`.
- Recovery: `ContextPolicy::validate`, called by ModelProvider before every model call, uses a
  **full local estimate** (`estimate(&request, model, &[])`, no baseline) and fails with budget facts
  only when it is `≥ context_window`. The Loop then retries the request once with `recovery`, which
  forces a compaction with `fallback`.
- A provider-reported overflow (`context_length_exceeded`, Anthropic's "prompt is too long") is
  classified as permanent and carries no budget facts
  ([stream.rs](../../../crates/dim-agent-aimux/src/stream.rs)), so it fails the turn without
  recovery. The design keeps provider overflow out of F1 (§10).
- The read and exec tools inline at most 30,000 characters per result
  (`READ_INLINE_MAX_CHARS`, `INLINE_OUTPUT_CHAR_LIMIT`).
- The summarizer request has its own system prompt and one user message with the transcript as
  text ([summary_strategy.rs](../../../crates/dim-agent-capabilities/src/summary_strategy.rs)).
- The provider port marks up to two Anthropic cache breakpoints on a request
  ([request.rs](../../../crates/dim-agent-aimux/src/request.rs),
  [Anthropic cache breakpoints](../../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)): the end
  of the System, so the tool declarations and the System are one cached prefix, and the last block of
  the last user or tool-result message, so each turn reads the previous prefix and writes only the new
  tail. The exact rule — an explicit System breakpoint is kept, and the message mark is added only
  while the request stays within the API's four — is the crate README's. Both use the API's
  five-minute lifetime; providers with automatic prefix caching (OpenAI and most OpenAI-compatible
  vendors) take no breakpoint and are unaffected.

## 3. Trigger shape

### 3.1 What the current ratio does across windows

| Model shape (window / output) | `B` | `0.8 × B` | Headroom `B − threshold` | Proposed threshold | Proposed headroom |
| --- | --- | --- | --- | --- | --- |
| 32k / 8k | 24,000 | 19,200 | 4,800 | 19,200 | 4,800 (20 % cap) |
| 128k / 8k | 120,000 | 96,000 | 24,000 | 104,000 | 16,000 |
| 200k / 64k | 136,000 | 108,800 | 27,200 | 120,000 | 16,000 |
| 400k / 128k | 272,000 | 217,600 | 54,400 | 244,800 | 27,200 |
| 1M / 128k | 872,000 | 697,600 | 174,400 | 784,800 | 87,200 |

[Judgment] A pure ratio has two faults. On small windows the headroom is a few thousand tokens,
the size of one tool result; on large windows it is 100k+ tokens that are never needed for error
and only make compaction earlier. What the headroom has to absorb is roughly absolute: the
estimation error of what was added since the last provider count (§6), which is bounded by the
tool output caps, not by the window.

### 3.2 What other tools document

| Tool | Trigger | Retention | Source |
| --- | --- | --- | --- |
| Claude Code | Without an override, compacts "when the conversation reaches the model's context limit"; 1M-window models compact "at about 967K tokens"; users may only lower it (`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`, `autoCompactWindow` 100K–1M) | No verbatim turn tail: summary, then re-injects CLAUDE.md, up to 5 recent files (≤ 5,000 tokens each) and skill bodies (5,000 each, 25,000 total) | [model-config](https://code.claude.com/docs/en/model-config#context-window-and-auto-compaction), [env-vars](https://code.claude.com/docs/en/env-vars), [context-window](https://code.claude.com/docs/en/context-window#what-survives-compaction) |
| Codex CLI | `auto_compact_token_limit` defaults to 90 % of the context window and a configured value is clamped to it; compared with the session's total token usage | Its local compaction keeps recent user messages up to `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` (token budget, local estimate bytes / 4) | [`openai_models.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/protocol/src/openai_models.rs), [`compact.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/core/src/compact.rs), [`context_window.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/core/src/session/context_window.rs), [config sample](https://developers.openai.com/codex/config-sample) |
| Aider | Summarizes chat history above `max_chat_history_tokens = min(max(max_input_tokens / 16, 1024), 8192)` | Keeps a tail of up to half that budget (tokens), ending the head on an assistant message | [`models.py`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/models.py), [`history.py`](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/history.py) |
| OpenHands SDK | `LLMSummarizingCondenser`: more than `max_size = 240` events, or more tokens than the LLM's effective input limit | Keeps the first `keep_first = 2` events and a tail of about half (`max_size // 2` events, or enough to fall under half the token limit) | [`llm_summarizing_condenser.py`](https://github.com/OpenHands/software-agent-sdk/blob/6530325e9dd0ea1b6b4f1994d7dab52764cfc3f1/openhands-sdk/openhands/sdk/context/condenser/llm_summarizing_condenser.py), [docs](https://docs.openhands.dev/sdk/arch/condenser) |
| Anthropic API, threshold compaction | Absolute: default `input_tokens` trigger 150,000, minimum 50,000 | Recent turns only by pausing and re-inserting them | [compaction-threshold](https://platform.claude.com/docs/en/build-with-claude/compaction-threshold) |
| Anthropic API, keep-tail compaction | Caller decides | "No parameter sets which turns are kept"; the example keeps 2 turns and warns that kept turns "go back to Claude at full length" | [compaction-keep-recent-turns](https://platform.claude.com/docs/en/build-with-claude/compaction-keep-recent-turns) |

[Documented] The coding agents that compact in-line trigger late: Codex at 90 % of the window,
Claude Code near the limit. Codex compares the limit with the session's token usage; Claude
Code's documentation does not say how it counts. The Anthropic API's
default is an absolute 150k, independent of a 1M window, which reflects context-quality and cost
concerns rather than overflow safety.

### 3.3 Recommendation

[Judgment] Use the same formula for every window:

```
B         = context_window − output_tokens          (unchanged)
reserve   = min(max(0.10 × B, 16_000), 0.20 × B)
threshold = B − reserve
```

- **16,000 absolute floor.** One inline tool result is capped at 30,000 characters; its measured
  worst under-estimate is about 5,000 tokens (§6.2). 16,000 covers a batch of about three
  worst-case results, or one result plus the user input and the system and tool declaration drift.
- **10 % ratio.** A provisional heuristic, not a proven bound. It is meant for full-estimate
  requests (no provider baseline: the first request, right after a compaction, after a change of
  prefix, model or configuration), where the error applies to the whole request. This note did not
  measure a mixed transcript, so it claims no error bound for that case (see §6.4 for a case the
  reserve does not cover). For 400k and larger windows this ratio is also where Codex triggers.
- **20 % cap.** Small windows cannot afford 16k; the cap keeps a 32k model where it is today.
- **No absolute upper trigger for 1M windows.** The two documented data points disagree (Claude
  Code compacts near 1M; the Anthropic API defaults to 150k). Without an evaluation of summary
  quality versus long-context degradation in Dim, a user setting is the better place for an
  earlier trigger. This is the least certain part of the recommendation.
- [Judgment, open] `output_tokens` defaults to the catalog `maxOutputTokens`, so a 200k model with
  64k output gives `B = 136k`, and a model whose output limit is large relative to its window
  compacts much earlier than its window suggests. Anthropic accepts `input + max_tokens > window`
  on Claude 4.5 and newer and stops with `model_context_window_exceeded`
  ([context-windows](https://platform.claude.com/docs/en/build-with-claude/context-windows#context-window-overflow-behavior)),
  but this is not true of every provider, so this note keeps the full reservation. Revisit it with
  per-provider facts.

## 4. Retention

### 4.1 Problems with K = 2 turns [Code, Judgment]

- **Turn size is unbounded.** A Dim turn is one user input followed by any number of model calls
  and tool batches. A long autonomous turn can be larger than the whole budget; two short turns
  can be a few hundred tokens. A turn count therefore keeps either too much or too little.
- **The dead zone.** While the running turn is one of the last two, no threshold compaction can
  cut inside it: `retain_limit` is at or before the start of the previous turn, and with fewer than
  two turns there is no limit, so the strategy returns `skipped: nothing_to_compact` at each new
  boundary above the threshold. The request keeps growing until `validate`'s full local estimate
  reaches the whole window; only then does recovery cut inside the turn. If the content is
  under-estimated (logs, lock files, CJK on some tokenizers; §6), the provider rejects the request
  first with a permanent overflow error and the turn fails.
- **The baseline is outside the count.** The keyed baseline (latest value of each keyed input
  block before the cut, [history.rs](../../../crates/dim-agent-capabilities/src/history.rs)) is
  replayed after the replacement and before the kept entries on every projection. It is never
  summarized, so it is a fixed cost after every compaction that the retention rule does not see.

### 4.2 Recommendation [Judgment]

1. **Count tokens.** `keep_budget = 0.25 × B`, counted with the window facts' `count_tokens` on
   the baseline at the cut plus the entries from the cut on.
2. **Choose the cut.** Among the legal cuts (`legal_cuts`, unchanged: turn starts and
   entry-placing events that do not split a tool call from its result or context), take the
   earliest one whose kept tail fits `keep_budget`. This keeps whole recent turns when they fit and
   cuts inside the running turn when it is too large.
3. **When nothing fits** (one huge tool result, or the baseline alone exceeds the budget): take
   the latest legal cut, for every trigger, not only under recovery. The reduction check in
   `View::compacted` is unchanged and still rejects a result that does not shrink the request.
4. **Why 25 %.** After compaction the context is about system and tools + summary + `0.25 × B`,
   leaving roughly 0.5–0.6 × B of room before the next trigger. Aider and OpenHands keep about half
   of their summarization budget, Codex keeps 20k tokens of user messages, and Anthropic's
   keep-tail example keeps two turns; none of them documents evidence for its value, so 25 % is a
   judgment that bounds compaction frequency (a later trigger and a smaller tail both mean fewer
   compactions) while keeping most recent work verbatim.
5. **Cuts inside a turn summarize that turn's user input.** The summary prompt asks for "the user's
   goals and constraints", and the keyed baseline keeps standing state, so this is acceptable; a
   rule that always pins the running turn's input verbatim (as Codex pins user messages) is a
   possible refinement, not part of this recommendation.

## 5. Prompt caching, recovery and manual compaction

### 5.1 Prompt caching

- [Documented] Prefix caches match exactly; a rewritten history misses from the first changed
  block. Anthropic: 5-minute default lifetime, writes 1.25× (1 hour: 2×), reads 0.1× of input price
  (0.05× on Opus 5.5 and Sonnet 5.5)
  ([prompt-caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)).
  OpenAI caches prompts automatically from 1,024 tokens, with cached input discounted "up to 95%"
  ([guide](https://developers.openai.com/api/docs/guides/prompt-caching)). Claude Code lists
  compaction among the actions that invalidate the cache, and sends its summarization request with
  the conversation's own system prompt, tools and history plus a final instruction, so that request
  reads the cached prefix ([prompt-caching](https://code.claude.com/docs/en/prompt-caching#compacting-the-conversation)).
  The Anthropic API recommends a breakpoint at the end of the system prompt so it stays cached
  across compactions ([compaction-threshold](https://platform.claude.com/docs/en/build-with-claude/compaction-threshold)).
- [Code] Dim's projection puts the replacement first, then the baseline, then the kept tail, so a
  compaction keeps at most the system and tool prefix cached. Its summarizer request has a
  different system prompt and renders the transcript as text, so it shares no prefix with the
  conversation. Its whole input — the strategy's own System and the single user message that carries
  the transcript — is a cache write at 1.25×, and no later request reads that transcript, because
  the compaction replaces it. The message breakpoint on that user message is what makes the write;
  the System breakpoint caches nothing, because the strategy's own System is shorter than the
  model's minimum cacheable prefix
  ([Anthropic cache breakpoints](../../../crates/dim-agent-aimux/README.md#anthropic-提示缓存断点)). So
  the summary call's input costs 1.25 times its input price instead of 1: the accepted cost of
  marking every request the same way.
- [Judgment] Whether compacting earlier costs more depends on what follows. With the breakpoints in
  place, an ordinary call reads the prefix it shares with the previous call at the read price `r`
  (0.1, or 0.05 on Opus 5.5 and Sonnet 5.5) and writes only its new tail at 1.25×, so an earlier
  compaction saves `r` times the tokens it removes on each later call, not their full price. The
  compaction pays the summarizer's whole input as a 1.25× write that is never read, the summary's
  output, and the rebuild of the new prefix (replacement, baseline and kept tail) at 1.25×. It pays
  off once
  `1.25 × summary input + summary output + 1.25 × rebuilt prefix < r × Σ (old prompt − compacted prompt)`
  over the later calls that would otherwise carry the old prefix. For example, at `r = 0.1`,
  `C = 100k` shrunk to `S = 20k` saves 8k input-price units per later call, so a compaction whose
  summarizer input is the replaced range and whose rebuilt prefix is `S` costs about 150k
  input-price units before the summary's own output and is repaid within about twenty calls; at
  `r = 0.05` the saving is 4k per call and repayment takes about twice as many calls. Many later
  small calls with a warm cache can repay an early compaction; few later calls, or a cache that
  expires between them, favour a late trigger. This depends on call count, growth and cache
  lifetime, none of which this note measured, so cost does not decide the trigger. The late trigger
  of §3.3 rests on overflow safety (§3) and on keeping context the model may still need; it remains
  a heuristic. The cheaper lever is the summarizer: reusing the conversation prefix (Claude Code's
  approach) would let it read that cached prefix at `r` instead of writing the whole transcript at
  1.25×, an order-of-magnitude cut in input price on the shared part — arithmetic on the published
  price ratios, not a measurement. That changes the strategy's request shape and is a separate
  follow-up.

### 5.2 Budget-exhaustion recovery

- [Code] Recovery starts only from `validate`, which uses a full local estimate and compares it
  with the whole window, while the trigger uses the provider baseline and `B`. The two can
  disagree in both directions: a context that is mostly code (estimate 1.3–1.4× too high) makes recovery fire
  when the real input is about 70 % of the window, replacing the retained tail with the latest
  legal cut; log- or lock-file-heavy content (estimate up to 1.6× too low on Qwen) lets the real
  input reach the window first, and the provider error is not recovered.
- [Judgment] When a provider baseline exists, `validate` should use the same estimate as the
  trigger (provider baseline plus delta, from the branch's committed events) and fail at
  `estimate > B`. Without a baseline the full local estimate remains, and so does its error (§6.4);
  this change does not remove that dependence. Recovery then means "the request
  does not fit the declared budget", which is consistent with the trigger and the summarizer's own
  chunk budget. With §3.3 and §4.2 in place, recovery is the fallback for a single delta larger
  than the reserve; the latest-legal-cut rule it uses stays as it is.
- [Judgment] Provider-reported overflow should eventually map to the same recovery (the design's
  separate Aimux issue). Until then the reserve is the only protection against under-estimation.

### 5.3 Manual `/compact`

- [Code] Manual compaction uses the same `retain_limit`. On a session with one or two turns it
  returns `skipped: nothing_to_compact`, which a user who asked for compaction does not expect.
- [Judgment] A manual step should cut at the start of the latest turn (keeping only the running or
  last turn), or at the token-rule cut when that keeps less. Claude Code's `/compact` keeps no
  verbatim turns at all, and recommends running it at natural breaks rather than waiting for the
  automatic trigger, which is when its cost is predictable
  ([prompt-caching](https://code.claude.com/docs/en/prompt-caching#compacting-the-conversation)).

### 5.4 What the reserve is for, and the summarizer's generation length [Code, Maintainer review]

The trigger decides how large the context is when a compaction starts, and the compaction is itself a model call whose output (summary plus any thinking) is bounded by `max_tokens`. F1 already plans around that bound:
- `ProductSummaryStrategy` packs the history into chunks that each fit `context_window − output_tokens` with `count_tokens` on the whole summarizer request.
- It folds the chunks one by one, carrying the previous summary.

So a compaction does not need the context to leave room for itself: the context may run close to full and the summarizer still splits by the model's available generation length. The trigger reserve therefore only protects the **next ordinary request**: its output allowance, which already includes thinking, plus the estimation headroom of §6. The 16,000-token floor in §3.3 is estimation headroom, not space for the compaction call.

Two related points, decided in review:
- **Mechanism defect, fixed separately in #169.** A summarizer reply stopped at `max_tokens` is accepted as a complete summary today (`summary_strategy.rs` `collect`). Thinking can use most of the allowance, so a truncated summary may silently replace history. A `max_tokens` stop must not complete. Split the chunk and retry, and fail at the minimum size. The dead zone of §4.1 is fixed in the same issue.
- **Configuration and prompt, not mechanism (#170).** Whether summarizer requests think (inherit, off or low, where the model allows) is a strategy setting. A target summary length is an instruction in the summary prompt. Both are settled by a summary-quality evaluation, not by adding constraints in code.

## 6. Estimation error

### 6.1 Whole samples [Measured]

Method: samples from this repository (English and Chinese design documents, Chinese-only lines of
the Chinese documents, Rust source, an events JSON fixture, `Cargo.lock`, `git log --stat`), split
into ~4 KB chunks at line boundaries. Each chunk is wrapped as one tool-result message part and
serialized as compact JSON with non-ASCII characters unescaped, the shape `count_tokens` measures,
then divided by 4. Real counts come from `tiktoken` 0.14.0 (`o200k_base` for GPT-4o and later,
`cl100k_base` for GPT-4) and Hugging Face `tokenizers` 0.23.3 with the published `tokenizer.json`
of Qwen2.5-7B-Instruct and DeepSeek-V3, counting only the raw chunk text (no message framing,
which slightly favours the estimate). Claude's tokenizer is not public and the token-counting API
needs credentials, so Claude was not measured.

Cells are real tokens and, in brackets, `estimate / real` (above 1 over-estimates):

| Sample | KB | Estimate | o200k | cl100k | Qwen2.5 | DeepSeek-V3 |
| --- | --- | --- | --- | --- | --- | --- |
| Rust source | 81 | 21,917 | 15,288 (1.43) | 15,280 (1.43) | 15,314 (1.43) | 16,361 (1.34) |
| JSON events | 47 | 13,944 | 11,689 (1.19) | 11,677 (1.19) | 12,336 (1.13) | 11,916 (1.17) |
| English Markdown | 126 | 33,646 | 29,517 (1.14) | 29,529 (1.14) | 29,689 (1.13) | 30,498 (1.10) |
| Chinese Markdown | 121 | 31,782 | 33,363 (0.95) | 40,161 (0.79) | 33,090 (0.96) | 31,730 (1.00) |
| Chinese prose only | 17 | 4,311 | 4,525 (0.95) | 6,080 (0.71) | 4,212 (1.02) | 3,833 (1.12) |
| `git log --stat` | 77 | 20,773 | 23,301 (0.89) | 23,323 (0.89) | 27,445 (0.76) | 24,124 (0.86) |
| `Cargo.lock` | 88 | 25,388 | 33,586 (0.76) | 33,514 (0.76) | 40,723 (0.62) | 35,304 (0.72) |

Findings:

- `bytes / 4` over-estimates code (indentation runs merge into few tokens) and JSON, and is close
  for prose. It under-estimates hash- and path-dense output (lock files, logs) by up to 38 %.
- CJK text is 3 bytes per character, so `bytes / 4` gives 0.75 tokens per character. Modern
  tokenizers (o200k, Qwen2.5, DeepSeek-V3) are within ±12 %; `cl100k` under-counts it by 29 %.
- The JSON envelope adds 3–15 % to the estimate (escaped quotes and newlines), which is a small
  built-in margin.

### 6.2 One capped tool result [Measured]

The first 30,000 characters of each sample (the inline cap of the read and exec tools), same
method:

| Sample | Estimate | Real (o200k / cl100k / Qwen2.5 / DeepSeek-V3) | Worst under-estimate |
| --- | --- | --- | --- |
| `Cargo.lock` | 8,303 | 11,041 / 11,011 / 13,318 / 11,651 | 5,015 |
| Chinese Markdown | 12,119 | 13,014 / 15,509 / 13,092 / 12,547 | 3,390 |
| `git log --stat` | 7,722 | 9,257 / 9,277 / 11,043 / 9,576 | 3,321 |
| English Markdown | 7,658 | 7,010 / 7,010 / 7,046 / 7,340 | none |
| Rust source | 7,721 | 5,720 / 5,719 / 5,738 / 6,057 | none |

### 6.3 Other error sources [Documented, Judgment]

- **Images.** Anthropic counts `⌈w/28⌉ × ⌈h/28⌉` visual tokens, at most 1,568 on standard-tier
  models but up to 4,784 on Claude 4.7 and later
  ([vision](https://platform.claude.com/docs/en/build-with-claude/vision#resolution-and-token-cost)).
  1,600 is right for standard-tier models and up to 3× low for high-resolution ones. Dim does not
  resize images before sending. Because the trigger uses the provider baseline after the first
  call, only newly added images are estimated; keep 1,600 and let the reserve absorb a few.
- **Thinking.** Thinking parts and their signatures are serialized into the estimate. Some models
  strip previous-turn thinking and do not count it
  ([context-windows](https://platform.claude.com/docs/en/build-with-claude/context-windows#the-context-window-with-thinking)),
  so the estimate errs high there, which is safe.
- **Claude.** Not measured. Third-party analyses report that the Opus 4.7 tokenizer produces more
  tokens for the same text than earlier Claude models (for example
  [OpenRouter](https://openrouter.ai/blog/insights/opus-47-tokenizer-analysis/)); I could not
  confirm the figure in Anthropic's documentation.

### 6.4 Consequence for headroom [Judgment]

- With a provider baseline, only the delta since the last call is estimated, and per capped
  result the worst measured error is about 5k tokens: the 16k floor covers about three such results.
- Without a baseline the error scales with the whole request, and the reserve does not cover every
  case. The estimates in §6.1 already include the serialized JSON, so no separate envelope margin
  applies on top. A counterexample from the reported Chinese-Markdown/cl100k ratio
  (`31782 / 40161 ≈ 0.79`): at a 128,000-token window with 4,096 output tokens the proposed
  trigger is 107,904, and a 130,000-token no-baseline input estimates to about 102,900. That is
  below the trigger and the proposed validation budget, although the input alone exceeds the
  window. This is arithmetic on the measured ratio, not a new provider measurement. A no-baseline
  request is reachable: the first request, right after a compaction, or after a change of prefix,
  model or configuration (`ContextPolicy::estimate` falls back to `count_tokens(request)`). Until a
  provider-reported overflow carries budget facts into recovery (the Aimux follow-up in §5.2),
  this limitation stays; the reserve is a provisional heuristic.
- A tokenizer per model is not worth its weight: it adds per-provider vocabularies, still cannot
  count Claude locally, and the provider baseline already bounds the error to the delta. Codex uses
  the same `bytes / 4` approximation (`APPROX_BYTES_PER_TOKEN = 4` in
  [`truncate.rs`](https://github.com/openai/codex/blob/96eb400d1231f0199e96f640c740096087cf3e9b/codex-rs/utils/string/src/truncate.rs)).

## 7. Proposed changes to F1

A follow-up code change, with the design text (§5.1, §5.3, §10 of
[ContextCompaction](../plugins/ContextCompaction.md)) and the flow tests that assert the K-turn
limit updated together.

| File | Now | Proposed |
| --- | --- | --- |
| `crates/dim-agent-capabilities/src/context_model.rs` | `const THRESHOLD_RATIO: f64 = 0.8;` and `threshold_tokens = THRESHOLD_RATIO × B` | `const RESERVE_RATIO: f64 = 0.10;` `const RESERVE_MIN_TOKENS: f64 = 16_000.;` `const RESERVE_MAX_RATIO: f64 = 0.20;` and `threshold_tokens = B − (RESERVE_RATIO × B).max(RESERVE_MIN_TOKENS).min(RESERVE_MAX_RATIO × B)` with `B = (context_window − output_tokens).max(0)` |
| same | `BYTES_PER_TOKEN = 4.`, `IMAGE_TOKENS = 1_600.` | Unchanged |
| `crates/dim-agent-capabilities/src/context_policy.rs` | `const RETAINED_TURNS: usize = 2;`, `retain_limit(context)` by turn count | `const RETAINED_SHARE: f64 = 0.25;` and `retain_limit` = the earliest legal cut whose baseline plus kept entries count `≤ RETAINED_SHARE × B` with the window facts, else the latest legal cut. This needs `compact_step` to compute cuts before the limit (it already has the model facts) |
| same, `CompactionInput::cut` in `host.rs` | The latest legal cut only under `fallback` | Unchanged in shape: with the token rule a within-limit cut exists whenever any legal cut exists, so `fallback` keeps meaning only "recovery" |
| same, manual step | Same limit as automatic | Limit at the `turn/start` of the latest turn, or the token-rule cut if that keeps less |
| `ContextPolicy::validate` | Full local estimate, fails at `≥ context_window` | Provider-baseline estimate from the branch's committed events, fails at `> B` |
| Follow-up issues | — | Summarizer request reusing the conversation prefix; provider overflow mapped to recovery (already a separate Aimux issue) |

Not proposed: a separate rule per window class, an absolute cap for 1M windows, a local tokenizer,
or a different image constant.
