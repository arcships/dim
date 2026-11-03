# aimux 模型宿主

进程级 `AimuxPlugin` 提供 `ModelHost`，支持 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages、Gemini GenerateContent 和 Chat 兼容端点。调用传入最终请求、一次性 `HttpModelConnection` 与取消信号；连接不序列化到 Timeline。请求、工具名与返回结果的转换约定见 [架构约定](../../docs/architecture/README.md)。

`ConfiguredModelsPlugin` 管理开发配置的共享 `AimuxModels` 和 `ProviderConnectionResolver`，每次请求读取凭据环境变量；它依赖 `ModelHost`，并用共享运行提供 `auxiliaryModel` 操作（`AuxiliaryModelHost` 与具名 `auxiliaryModel` 绑定）。Session 的 `ConfiguredModelPlugin` 依赖模型表、SessionConfig、SessionIdentity、ToolRegistry，提供 `PrepareModelHost`。`defaults::install_with_process_model` 将分支 ModelProvider 接到进程模型服务；模型调用仍使用原有任务。

## 工具结果中的图像

工具结果里的图像按请求的 `ModelProtocol` 决定落点，由转换器能否在工具结果内携带图像决定，不改变历史或存储的工具结果。标记与说明文字在 `src/request.rs` 中各定义一个常量；下表路径相对 `vendor/aimux/aimux-providers/src/`。

| ModelProtocol | 形式 | 决定的转换器 |
| --- | --- | --- |
| openai-chat、openai-compatible | 后续 user 消息 | `openai/convert.rs`，`convert_message_to_openai` 的 `Role::Tool` 分支（经 `tool_result_to_content`）只产出字符串 |
| openai-responses | 后续 user 消息 | `openai/responses/convert.rs`，`convert_to_responses_input` 的 `Role::Tool` 分支：`function_call_output.output` 只收字符串 |
| xai | 后续 user 消息 | `xai/convert.rs`，`convert_to_xai_messages` 的 `Role::Tool` 分支：tool 消息的 `content` 只收字符串 |
| xai-responses | 后续 user 消息 | `xai/responses/convert.rs`，`convert_to_xai_responses_input` 的 `Role::Tool` 分支：`function_call_output.output` 只收字符串 |
| gemini | 后续 user 消息 | `google/convert.rs`，`convert_tool_parts`：`functionResponse.response.content` 只收字符串 |
| anthropic | 工具结果内 | `anthropic/convert.rs`，`resolve_tool_result_output` 的 `content`/`error` 分支与调用处的 `ContentPart::ToolResult` 分支：`tool_result.content` 接受内容块数组 |
| bedrock | 工具结果内 | `bedrock/convert.rs`，`ContentPart::ToolResult` 分支与 `resolve_tool_result_output`/`convert_tool_result_content_part`：`toolResult.content` 接受 text/file 块 |

「后续 user 消息」指：工具结果的文本在图像位置留一个标记；该步骤所有 tool 消息结束后放一条 user 消息，图像按工具调用顺序排列，每张图前有一段文本说明它来自哪个工具调用、是工具输出。

Aimux 无目录或 Session 依赖，独占可释放的 HTTP client。卸载关闭准入、释放未读取或停止读取的原生流、排空已受理调用，再释放池；旧宿主和流返回 `OwnerClosed`。HTTP 失败保留类型化 status/code，恢复由调用方决定，adapter 不重试。产品 Provider/OAuth 另行实施。

## 模型端点的重定向

模型 HTTP client 只在同源（scheme、host、port 相同）时跟随 307 与 308，最多 10 跳。301、302、303 一律不跟随：模型请求是带 JSON body 的 POST，而 reqwest 的重定向层在策略之前就把这三种状态下的 POST 改写成不带 body 的 GET，跟随后服务器收到的已不是模型请求；端点要搬迁就返回 307 或 308。任何跨源跳转也不跟随：凭据只发给配置的那个源，reqwest 只在 host 变化时剥离 `Authorization`、`Cookie` 等少数敏感头，`x-api-key`、`x-goog-api-key` 等 provider 自己的头不在其列。

没被跟随的跳转让请求照常失败，错误里点名状态码与目标 URL，并说明原因：跨源、跟随会丢掉方法与 body，或超过跳数上限。目标不带查询串、URL 里的 userinfo 已隐去；reqwest 把跳转错误压成固定文案不带目标，补写发生在 `src/driver.rs` 的错误路径上。流式与普通请求走同一条路径。

## Anthropic 提示缓存断点

请求的 `ModelProtocol` 是 anthropic 时，`src/request.rs` 的转换器在请求里最多标两个提示缓存断点；其他协议不带断点，Bedrock 不变。放在哪里是 dim 的产品决定：aimux 只负责线上编码（part 或消息上的 `providerOptions.anthropic.cacheControl`，每个请求至多四个断点），自己不放断点。

- **System 末尾**：System 的最后一个块标一个断点，工具声明与 System 因此合成一个缓存前缀。这个前缀在一次 Session 内保持一份，因为 System 只放静态文本（[model-prompts §5 第 3 条](../../docs/design/model-prompts.md#5-scopes)）；System 已带显式断点（`SystemPromptPart::Text` 的 `cache_control`）时原样保留，不再加 System 断点。
- **显式断点由生产者负责**：显式标记必须保持在 API 的四个以内，端口原样保留、不裁剪。超过四个时，vendored 转换器在请求里保留前四个、丢弃其余标记并报 unsupported 警告；请求发出之后，dim 以 `Unsupported model parameter: cacheControl breakpoint limit` 失败。
- **最后一条用户或工具结果消息**：在消息级 `providerOptions.anthropic.cacheControl` 标一个断点，由转换器落在该消息最后一个可缓存块上；只有请求仍在四个断点以内时加。
- **生命周期**：用 API 默认的五分钟，每次使用刷新；一小时的长期缓存不用。
- **其他尾部**：最后一条消息既不是用户消息也不是工具结果消息时，不加消息断点。
- **不变的部分**：Timeline 与存储的请求不变；模型日志记录线上形态，包含断点。
- **一次性请求**：压缩摘要这类一次性请求也带消息断点，付一次缓存写入而没有后续读取；短于该模型最小可缓存长度的提示不缓存，也不计费。这是把每个请求同样标记换来的已知代价。

## 请求录制

`recording::set_request_recording` 开关 aimux 的 RFC-0023 录制，写入 `<目录>/recordings.jsonl`；App Server 的 `model-log` Settings 源调用它（[设计](../../docs/design/appserver/model-log.md)）。

Dim 直接调用 provider 的 `do_stream`，不经过 aimux 层 A，所以录制由本 crate 收尾：`request::convert` 在录制开启时把请求绑定到 `RecordingContext` 并返回 `Capture`；driver 记录输入侧，并用 `RecordingOutcomeStream` 包住 provider 流，在 Finish、错误或丢弃时写 outcome 与传输封闭。关闭时这些路径不产生调用。

## 启动

在仓库根目录运行，先在当前环境设置配置引用的密钥变量：

```sh
cargo run --locked -p dim-app-server -- \
  --model-config crates/dim-agent-aimux/examples/openai.json \
  --ws 127.0.0.1:8787
```

配置包含 `defaultModel`、`providers`；每个 provider 配置端点、可选凭据环境变量和模型默认设置。每个模型条目还必须声明 `contextWindow`（上下文窗口）与 `maxOutputTokens`（输出上限），两者都是正整数；缺失或为零会让配置加载失败，并在错误里点名 provider 与模型。配置在监听前读取；不使用环境变量时可接入免密本地端点。App Server 使用生产工作区工具；MockTools 只属于显式 mock 模型配置。

## 离线图与资源测量

以下命令只创建实例，不请求模型：

```sh
cargo run --locked -p xtask -- graph --model-config crates/dim-agent-aimux/examples/offline.json
cargo run --release --locked -p xtask -- measure --model-config crates/dim-agent-aimux/examples/offline.json
```

图生成到 `docs/architecture/rust-agent/generated/aimux/`。测量输出空 Session 的申请内存、任务数与关闭耗时；实际请求流通过本地 HTTP/SSE 流程测试。

## 依赖与验证

固定上游源码和必要补丁位于 [vendor/aimux](../../vendor/aimux/SOURCE.md)。`cargo test -p dim-agent-aimux` 同时运行请求映射、结果转换、真实 HTTP/SSE Agent 流程和上游补丁回归。
