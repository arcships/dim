# aimux 模型宿主

进程级 `AimuxPlugin` 提供 `ModelHost`，支持 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages、Gemini GenerateContent 和 Chat 兼容端点。调用传入最终请求、一次性 `HttpModelConnection` 与取消信号；连接不序列化到 Timeline。请求、工具名与返回结果的转换约定见 [架构约定](../../docs/architecture/README.md)。

`ConfiguredModelsPlugin` 管理开发配置的共享 `AimuxModels` 和 `ProviderConnectionResolver`，每次请求读取凭据环境变量。Session 的 `ConfiguredModelPlugin` 依赖模型表、SessionConfig、SessionIdentity、ToolRegistry，提供 `PrepareModelHost`。`defaults::install_with_process_model` 将分支 ModelProvider 接到进程模型服务；模型调用仍使用原有任务。

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
