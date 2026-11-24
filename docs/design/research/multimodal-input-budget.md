# 多模态请求预算与图像引用

状态：研究提案，待采纳。关联 [#687](https://github.com/arcships/dim/issues/687)。

## 1. 请求边界

[#1717](https://github.com/arcships/dim/pull/1717) 已将对话请求改为使用图片引用。对话历史中的图片数量和像素体积因此不再累加到同一个上游请求。#687 的历史累积场景应在对话请求边界复验；本提案补上仍携带像素的图像理解辅助请求的预算。范围为静态图片。

| 请求 | 预算与归属 |
| --- | --- |
| 对话，包括同轮工具后的续请求 | 按 [Attachments A.1](../plugins/Attachments.md#a1-image-handling-cli-checks-and-modelprovider) 构造发送内容，引用随文本进入请求体。沿 [ContextCompaction](../plugins/ContextCompaction.md) 和 [ContextUsage](../plugins/ContextUsage.md) 的窗口预算、压缩与校准。 |
| 压缩摘要 | 按 [ContextCompaction](../plugins/ContextCompaction.md#52-productsummarystrategy) 使用含 `[image]` 标记的文本转录，应用摘要模型自己的文本输入预算。 |
| `image_understand` | 按 [ImageUnderstanding §4](../plugins/ImageUnderstanding.md#4-model-choice-and-execution) 在每次工具调用中解析理解目标。未命中既有答案复用时，经 [`auxiliaryModel.complete`](../appserver/auxiliary-model.md#41-language-neutral-adr-002-d5) 发送一张图、问题和固定指令，并按该次实际路由检查 §3 的三类预算。 |

现状快照取自 `main` 的 [`aa53e55e0`](https://github.com/arcships/dim/tree/aa53e55e0)（2026-11-19 核对）。[单图准备](../../architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images)采用长边 2,048 px 和 5,000,000 Base64 字节上限；[Aimux 发送防线](../../architecture/rust-agent/plugins/ModelProvider.md#image-handling)为 32 MiB 编码图像数据与 64 MiB 最终 HTTP 请求体。对话侧 WindowFacts v1 仍按逻辑上下文中每个图片块估 1,600 token，实际发送内容按上表适配。见 [计数算法](../plugins/ContextUsage.md)和 [WindowFacts 源码](../../../crates/dim-agent-capabilities/src/context_model.rs)。

历史复现使用 `v0.1.0-alpha.23`（`092aaa7f`）和测试基线 `5f465352f`。同一张 672 × 480 PNG 分别作为用户图、工具图和旧 DeepChat `cdp_send(Page.captureScreenshot)` 的 `{"data":"<base64>"}` 文本回执发送。原生图在同轮工具完成后重新计数，达到阈值时先压缩再发续请求。旧 JSON 的同一负载在 alpha.23 估为 110,602 token，本地测试端用 `o200k_base` 计为 296,755。旧回执仍按文本路径复验，生成新图片结果由[桥接生产者](https://github.com/ThinkInAIXYZ/deepchat/commit/ee0fc0fc3bc112f79c90b84e99dae5673aeb97be)负责。

## 2. Provider 调研

资料核查于 2026-11-16，供图像理解辅助请求选取计数和限额依据。按模型版本、实际端点和图像设置使用表中规则。路由组合来自 [Dim Provider drivers](../../../crates/dim-app-server/src/provider/drivers.rs) 和[内置目录](../../../crates/dim-app-server/resources/default-catalog.json)。目录的 `vision` 只能说明输入能力。计费文档、开源处理器和在线输入计数各有适用边界。运营限额可能随模型、地区或账户变化，表中的例子用于确定规则单位，运行时只采用匹配路由的已核验限制。

### 2.1 原生服务与公开计数规则

| 实际服务 | 图像如何计数 | 计数与请求限制的证据 |
| --- | --- | --- |
| OpenAI API | 按具体模型选择图块或 tile。图块型先应用该模型和 `detail` 的尺寸与图块规则，再对处理后的 `ceil(w/32) × ceil(h/32)` 乘模型系数并上取整。tile 型按 `detail` 取固定值，或应用缩放后加 tile 成本。以 `gpt-4o` 为例，其高细节规则为 85 加每个 512 px tile 的 170。解析模型特定的 `auto`，例如 `gpt-6-astra` 的 `auto` 使用 `original` 尺寸规则，某些模式会拒绝超图块上限的图。 | Responses 有 `POST /v1/responses/input_tokens`，可数同形状的图像请求。Chat Completions 用本路由的 `usage` 验证。官方 Responses Vision 页列出每次 512 MB、1,500 张和单图 30,000 图块的通用上限。模型、细节档和路由另有约束。 [图像规则](https://developers.openai.com/api/docs/guides/images-vision?api-mode=responses#calculating-costs) · [计数接口](https://developers.openai.com/api/docs/guides/token-counting) |
| Azure OpenAI | 由部署所绑定的基础模型和版本决定图像规则。Azure 部署名可由用户命名，需要可信的部署映射和该 API 的限制。 | 同一部署与协议的用量用于校验。Foundry 的 Chat 图像指南另列每次 10 张限制，不能把该例直接写入 Dim 的 Azure Responses 路由。 [Azure 图像请求与限制](https://learn.microsoft.com/en-us/azure/ai-foundry/openai/how-to/gpt-with-vision) |
| Anthropic Claude API | `ceil(w/28) × ceil(h/28)`。先按长边和图块预算做等比适配。官方标准档上限为 1,568 px 和 1,568 visual token，高分辨率档为 2,576 px 和 4,784 visual token。按具体模型选档。部分 computer use / browser use 工具的超尺寸截图会被拒绝，适配时要验证该工具及图像设置是否允许服务端自动缩放。 | Messages `count_tokens` 支持图片、工具和 system。图片张数会影响尺寸限制，`tool_result` 内的图片也计数。 [图像规则](https://platform.claude.com/docs/en/build-with-claude/vision) · [缩放细节](https://platform.claude.com/docs/en/build-with-claude/vision-coordinates#how-claude-resizes-and-pads-images) · [计数接口](https://platform.claude.com/docs/en/build-with-claude/token-counting) |
| Google Gemini API / Vertex AI | 早期 Gemini 文档描述小图 258 token 和大图按 768 px tile 计数。Gemini 2.5 的 `media_resolution` 资料还描述 64、256 及 Pan & Scan 的额外开销。Gemini 3 的低、中、高、极高档分别约 280、560、1,120、2,240 token。先确定该模型世代、请求 API 和实际分辨率设置，再用对应规则。 | `countTokens` 可接收多模态输入。Vertex 文档明确说明多模态预数是估计值，应与执行后的 `usage_metadata` 比对。Vertex 是独立端点，需单独确认兼容性。 [通用计数](https://ai.google.dev/gemini-api/docs/tokens) · [分辨率档](https://ai.google.dev/gemini-api/docs/generate-content/media-resolution) · [Vertex 计数](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/get-token-count) |
| Amazon Bedrock | Converse 的请求结构由 Bedrock 定义，底层模型决定图像 token。可将同一版本的公开模型规则用作待校验的本地估算。 | `CountTokens` 需核对模型、区域和 `Converse` / `InvokeModel` 输入形态。Converse 文档对顶层消息图像列出 20 张、单图 3.75 MB、8000 px 限制，嵌套工具图片与特殊模型另核对。跨区推理的计数支持也可能不同。 [计数支持](https://docs.aws.amazon.com/bedrock/latest/userguide/count-tokens.html) · [Converse Message](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Message.html) |
| 阿里云百炼 Qwen | 对支持的模型复现服务端的尺寸对齐、最少像素和最大像素规则。对齐后按 `w' × h' / s² + 2` 估算，`s` 由模型决定。文档列出 Qwen3-VL 等为 32、Qwen2.5-VL 等为 28。带上实际的 `max_pixels` 或 `vl_high_resolution_images`。 | 官方公式是近似值，按具体模型、实际供应端点和返回用量校验。模型别名及百炼托管的第三方模型单独匹配。 [公式和缩放](https://www.alibabacloud.com/help/en/model-studio/vision) · [设置](https://docs.modelstudio.console.alibabacloud.com/en/model-studio/qwen-api-via-openai-chat-completions) |
| DeepSeek | 公开 Vision 支持以 `deepseek-flash` 为例，服务端按尺寸缩放且公开单图 1,024 token 上界。官方有尺寸计算器，第一版可对已核验的直接路由使用上界。视觉能力按精确模型判断。 | 官方文档分别给出 48 MiB 请求体、内联单图大小、总图片大小、张数和多图尺寸约束。用 `usage` 验证图片档位。 [Vision 与限制](https://api-docs.deepseek.com/guides/vision/) · [尺寸计算器](https://api-docs.deepseek.com/quick_start/token_usage/#calculate-image-token-usage) |
| 火山方舟 / 豆包 | Seed 2.0 及后续文档给出像素面积除以 1,764 并按档位裁切的估法，也将该系列默认 `high` 描述为固定 1,280。以成功请求区分服务端实际档位和计数，未核验前可使用匹配档位的公开上界。其他模型的常数及 `detail` 另查。 | 核对指定模型的 `detail`、尺寸、Base64 单图和请求体限制，避免把常规 Seed 规则用于方舟托管的 DeepSeek 等模型。 [火山方舟图片理解](https://docs.volcengine.com/docs/ark/image-understanding?lang=zh) |

### 2.2 需要路由实测的服务

| 实际服务 | 公开证据和可采用的预算口径 |
| --- | --- |
| Moonshot / Kimi API / Kimi Code | 官方帮助页称视觉图像按每张 1,024 token 计费，未逐个绑定模型及路由。K2.6 开源权重的处理器使用尺寸、padding 与 patch merge。两者仅作为验证线索。Moonshot 提供 `estimate-token-count`，需确认其对目标模型、图片块、工具和最终格式的覆盖。Kimi Code 路由独立验证。 [官方帮助](https://www.kimi.com/en/help/kimi-api/api-model-capabilities) · [估算接口](https://platform.kimi.ai/docs/api/estimate) · [K2.6 处理器](https://huggingface.co/moonshotai/Kimi-K2.6/blob/main/media_utils.py) |
| xAI / Grok | 官方图像理解文档给出 `detail` 和图片输入限制，Chat 文档列出单图 20 MiB。未找到按模型版本可直接复用的图像 token 公式。用同路由成功用量配对验证 Chat 和 Responses，Grok Build 套餐同样单独验证。 [xAI 图像理解](https://docs.x.ai/developers/model-capabilities/images/understanding) · [Chat 限制](https://docs.x.ai/developers/model-capabilities/legacy/chat-completions) |
| 智谱 / Z.ai | GLM-4.6V 和 GLM-5V-Turbo 官方支持视觉输入，托管路由的每图公式待实测。GLM-V 开源处理器仅用于对应开源部署及已确认的设置。 [GLM-4.6V](https://docs.bigmodel.cn/cn/guide/models/vlm/glm-4.6v) · [GLM-5V-Turbo](https://docs.bigmodel.cn/cn/guide/models/vlm/glm-5v-turbo) · [GLM-V 源码](https://github.com/zai-org/GLM-V) |
| MiniMax、Mistral | MiniMax M3 发布的处理器采用尺寸对齐及 patch merge。Mistral 提供带图像配置的官方 tokenizer，图像上限按模型与部署确认。托管 API 要用实际用量验证，开源模型需匹配权重和处理器配置。 [M3 处理器](https://huggingface.co/MiniMaxAI/MiniMax-M3/blob/main/image_processor.py) · [Mistral tokenizer](https://github.com/mistralai/mistral-common) · [Mistral 请求限制](https://docs.mistral.ai/resources/known-limitations#vision) |
| OpenRouter、AIHubMix、ZenMux、New API、Vercel AI Gateway 等中转 | 区分网关的请求限制和实际选中的上游模型版本。模型名含厂商前缀、使用相同消息字段、或连接采用 Responses，均不足以证明同一计数。网关可变路由走未知模型规则。OpenRouter 的 generation 元数据可区分上游、`native_tokens_prompt` 和 `tokens_prompt`，用于具名路由的配对验收。 [OpenRouter 路由](https://openrouter.ai/docs/guides/routing/provider-selection) · [Generation 元数据](https://openrouter.ai/docs/api/api-reference/generations/get-generation) |
| OpenAI Codex、未核验的托管套餐、自建端点和本地模型 | 使用路由明确声明的规则与限额。vLLM 可配置图片张数及处理器参数，Ollama 可返回 `prompt_eval_count`，NVIDIA NIM 的输入和限制取决于模型及部署。其他目录项（包括无公开公式的视觉模型和别名）采用同一未知路由流程。 [vLLM 部署参数](https://docs.vllm.ai/en/stable/configuration/engine_args/) · [Ollama Chat](https://docs.ollama.com/api/chat) · [NIM 多模态输入](https://docs.nvidia.com/nim/large-language-models/latest/advanced-use-cases/multimodal-input.html) |

### 2.3 可复核算例

以下是各家公开规则对同样尺寸的计算例子，图片计数以外的请求内容另算。未调用付费 API。

| 处理规则 | 672 × 480 | 2,048 × 2,048 |
| --- | ---: | ---: |
| Dim 兼容估算（无已核验规则时） | 1,600 | 1,600 |
| OpenAI `gpt-6-astra`，`high` | 378，即 21 × 15 × 1.2 | 3,000，适配至 1,600 × 1,600 后计 2,500 个图块 |
| Claude 标准档 | 432，即 24 × 18 | 1,521，正方形在 1,568 visual token 预算内可覆盖最多 39 × 39 个图块 |
| Gemini 3，`MEDIA_RESOLUTION_HIGH` | 约 1,120 | 约 1,120 |
| DeepSeek Flash，使用文档上界 | ≤ 1,024 | ≤ 1,024 |

## 3. 图像理解请求的预算

以实际发送的模型版本或部署、端点与协议、影响图片计量的设置和规则版本匹配供应商资料。Modality 仍负责选择理解模型，Provider 在执行辅助调用时解析连接和模型配置。同一已解析的目标用于选择规则和发送请求。对不透明网关，执行网关自身的已知限制；未知上游限额保留为未知。

| 检查 | 校验对象与处理 |
| --- | --- |
| 输入 token | 输入额度取上下文窗口扣除实际输出预留后的余量和已知独立输入限额中的较小值。估算包含指令、问题、请求包装及实际发送图像。图片优先使用经核验的公式或适用的文档上界；两者都缺失时取 1,600 token 作为兼容启发式，可能高估或低估。用同路由的实际用量配对校准。已证实输入超限时发送前拒绝；只有启发式估值或上界超过额度时，保留正常上游判定。 |
| 图片张数、单图尺寸及字节 | `image_understand` 每个请求固定一张图。同批多个工具调用各自检查自己的请求。按实际载体核对已知的张数、单图和累计字节限制，区分原始文件字节与编码字节。内联 Base64 长度为 `4 × ceil(raw bytes / 3)`，外层字段和前缀计入相应的请求体。 |
| HTTP 请求体 | Aimux 以即将发出的序列化字节检查本地防线与该路由适用的硬上限，并发送同一份数据。检查覆盖指令、问题、图像及协议包装。 |

每次准备独立计算图像贡献。下次辅助调用若切换模型、部署、路由或细节设置，使用当次实际配置重新匹配和计算。辅助调用的用量仅用于同路由验收与校准，分支的 WindowFacts 算法、ContextUsage 数据和压缩触发沿原契约。

### 超限与图片准备

首批采用现有单图准备规则，并在发送前拒绝已知超限的辅助请求。工具沿既有 `image_understand_failed` 路径返回限制类别、数值、单位及可采取的动作，例如提供尺寸更小的图片。实际发送后收到 400 或 413 时，按既有失败契约结算。准备随调用取消，诊断只含预算标量和路由身份。

按路由自动缩图需由[共享图片准备](../../architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images)所有者先定义可传入的目标尺寸和字节、适用编码及返回的实际尺寸。现有准备器只接受固定的本地上限。若后续启用，副本只用于当次调用，原图继续由现有引用解析和文件授权持有。涉及原图坐标的问题需要单独约定尺度与复用语义。

## 4. 向所有者提出的需求

以下需求归属 [#687](https://github.com/arcships/dim/issues/687)。

| 所有者 | 现有合同及需要的变化 | 时序与结算 |
| --- | --- | --- |
| [Provider](../appserver/provider-management.md) | 视觉能力和窗口大小无法给出具体路由的图片或请求体限制。由 Provider 维护经核验的计数规则和限额，在内置目录及 [v52 模型条目](../appserver/native-storage-format.md#v52-sparse-provider-state)的模型数据中表达，并沿已有解析结果传给执行方。自定义路由可用明确声明的值，缺失表示未知。具体存储兼容约定由存储所有者确定。 | 与单次调用的连接和模型配置一起解析，随既有配置读取和生命周期结算。 |
| [ImageUnderstanding](../plugins/ImageUnderstanding.md) 与 [Modality](../plugins/Modality.md#94-session-image-understanding-consumer) | ImageUnderstanding 在每次调用中通过现有 `modalityTargets.resolve` 与 `auxiliaryModel.complete` 读取 Modality 选定的目标并执行请求，沿 [引用契约](../plugins/ImageUnderstanding.md#3-references)使用 `treeCommands.query` 与 `sessionFiles.read` 取得原图。ImageUnderstanding 将预算拒绝传为现有工具错误。 | 引用解析、目标选择和取消沿现有工具合同；已可复用的答案直接返回。 |
| [AuxiliaryModel](../appserver/auxiliary-model.md) 与 [Aimux](../../architecture/rust-agent/plugins/ModelProvider.md#image-handling) | `auxiliaryModel.complete` 使用当次已解析的模型配置及输出预留。Aimux 当前的通用发送防线不足以执行更严的路由约束；在同一次请求准备和最终发送中完成 §3 的检查。明确的本地预算拒绝经现有失败路径传递，按不可重试处理。 | 先检查再发送，取消或 owner 关闭时释放临时数据。既有工具事实承载结果。 |

### 规则检查

本提案沿用 [ImageUnderstanding](../plugins/ImageUnderstanding.md#8-rule-check) 的 Session 部件、工具事实和命名数据面，以及既有进程级 Provider、Modality 和辅助执行。对照 [设计规则 §1、§4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) 和 [ADR 002](../../adr/002-plugin-scope.md)。

| 条款 | 应用 |
| --- | --- |
| §4.1、§4.2 | 辅助请求预算由执行 owner 在发送交接前校验，工具结果沿已有结算。 |
| §4.3 | 准备、计数和编码在可取消的调用中运行。 |
| §4.4、§4.5 | Timeline 和 Dispatcher 处理原有工具调用及结果事实，批次沿原合同提交。 |
| §4.6 | 图像理解回答经工具结果进入对话，源图按已发布分支历史解析。 |
| §4.7、§4.8 | 依赖 §4 所列既有数据面与模型执行边界，需求由这些所有者消化。 |
| §4.9 | 预算和请求字节的测量只随当次执行。 |
| §1、ADR 002 D1–D3 | 沿原作用域、所有权和启停顺序复用现有部件。 |
| ADR 002 D4–D5 | 限额属于 Provider 业务数据，命名服务延续其语言中立的合同。 |

## 5. 实施验收

1. **历史累积回归。** 用隔离 Session 连续加入用户图，并在同一轮累积大量 `read` 图片，使源图累计字节和张数超过夹具限制。核对对话 provider 收到有效引用及成对的工具调用与结果，后续请求继续成功；切换视觉和纯文本对话模型，摘要按文本预算分片。另用历史 Base64 JSON 验证文本路径。
2. **辅助请求边界。** 在 `image_understand` 的 HTTP 夹具中观察实际路由和发送尺寸。分别让可证实的输入 token、单图尺寸、编码字节和请求体达到上限与高一个单位。已知超限时夹具收不到请求。未知路由仅因 1,600 的启发式估值超限时仍可发送，上游 400／413 返回正常工具错误。改变理解目标后重新匹配；同批多次工具调用各带一张图。压缩、恢复和 fork 后按已有可见性解析原引用，准备取消沿既有工具流程测试。
3. **路由配对校验。** 先选一个 OpenAI Responses 图块模型和一个 Claude Messages 模型，固定部署、协议、指令、问题和输出预留。对文本基线与加入一张图的同形请求比较预数（路由支持时）、实际输入用量和本地估值。采集跨图块尺寸边界的图片、方图、长条截图及同尺寸 PNG／JPEG，记录实际请求字节、图像设置与命中限制。确认已知硬限额均可在发送前拦截，再扩大路由覆盖。

删减检查保留既有引用、摘要和单图准备路径。新校验只随实际辅助请求，避免给分支统计增加路由维度或为旧图生成持久副本。
