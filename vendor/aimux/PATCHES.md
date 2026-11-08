# Agent integration patches

Upstream base: `v0.6.1`, `164d7f4bea6d6b726f5f9eef9bda492190766558`.

Four rows remain. Two are dim customisations beyond what the AI SDK and the tag implement: they keep request fields the tag warns about or rejects. The third keeps the OpenAI Responses `store=false` replay shape and references `arcships/aimux#468`. The fourth bounds an inbound WebSocket message, which upstream master has no way to do (checked 2026-11-07).

| File | Change | Why it survives | Pinning test |
| --- | --- | --- | --- |
| aimux-providers/src/openai/chat/chat_language_model.rs | Send `top_k` as a body field instead of warning it unsupported. | dim customisation: self-hosted OpenAI-shaped gateways (vLLM, SGLang) read `top_k` as a body field, and the Agent sends it on this protocol; the AI SDK and the tag do not. | `dim-agent-aimux/src/request/tests.rs`: `final_request_replays_multimodal_tool_history_and_only_model_readable_results` requires `top_k` in the body. |
| aimux-providers/src/openai/chat/chat_language_model_options.rs | Accept any string as `serviceTier` instead of a fixed list. | dim customisation: the product catalogue names service tiers outside the AI SDK's six-value enum, and the server decides what a tier means; `forceServiceTier` still bypasses the model-id capability gate. | `dim-agent-aimux/tests/model_flow/native.rs`: `explicit_preferences_reach_chat_and_responses_for_model_aliases` sends `future-tier`. |
| aimux-providers/src/openai/responses/convert_to_responses_input.rs | Replay a message item's text parts as one item that keeps its id and per-part `content`, and replay a client `function_call` with its item id, without `store`. | `arcships/aimux#468`: the Responses protocol returns one item per message with per-part content and an id on each item; the tag drops that structure when `store` is false, which is every dim call. | `dim-agent-aimux/tests/model_flow/native.rs`: `four_native_apis_preserve_signed_blocks_and_tool_continuation`; `native_content_keeps_unsigned_thinking_and_respects_committed_tools`. |
| aimux-provider-utils/src/ws.rs | Add `max_message_bytes` to `WebSocketRequest` and pass it to tungstenite's `WebSocketConfig` on both the direct and the proxied connect; every existing request literal sets `None`. | dim customisation: Modality bounds one realtime provider message so an oversized frame is refused while it is framed, not after it is buffered; tungstenite's own default is 64 MiB, and upstream master has no equivalent. | `crates/dim-modality-aimux/tests/realtime.rs`: `a_provider_message_over_the_bound_ends_the_session_at_the_socket`. |

The workspace member list is reduced to the four crates the Agent uses; production source is retained verbatim except for the changes above.

The Agent turns on `CallOptions.strict_stream` for every call, so the tag's strict stream owns the block lifecycle. The checks the integration keeps are these six: a block still open at `Finish`; a completed tool call that repeats, or whose name differs from its `ToolInputStart`; an unsupported warning fails the call; provider-executed tools and provider-generated files, sources and tool results are refused; a content-filtered, errored or other finish fails the turn; and tool input must parse as a JSON object.

The vendored tree carries the tag's own crates, test modules and the fixtures those modules include. The three regression test files the integration used to add were removed with the upgrade; the assertions the tag still satisfies are pinned in `crates/dim-agent-aimux/tests/vendor_contract.rs` and the existing model-flow suites, and the ones it no longer satisfies are listed as behaviour differences. Where a patch changes behaviour a tag test pins, that test's expectation follows the patch.

The patches do not add timeouts, retries, repair, or new model configuration fields, and nothing here enables Core orchestration or host-side tool-call repair in the Agent.
