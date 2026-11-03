# AuxiliaryModel

状态：已实现（S4 原生操作及具名绑定）。路线图项 S4（[#53](https://github.com/arcships/dim/issues/53)）。
总体设计：[附录 B.2 Provider 行](../overview.md#b2-app-server-进程插件)。作用域：
[ADR 002](../../adr/002-plugin-scope.md)（进程级，由 Provider 以及 model-config 配置中的 ConfiguredModels 提供）。

## 1. 摘要

`auxiliaryModel` 服务只有一个异步方法 `complete(model, request)`。它在任何 Session 之外发起
一次非流式模型调用：由模型选择的所有者解析给定模型的连接，调用现有的 `ModelHost`，返回文本。
它不写任何 Timeline，凭据不离开该所有者。

谁提供它由一条规则决定：模型选择的所有者提供这个操作。产品配置里是 Provider，用它自己的目录
和凭据；model-config 配置里是 ConfiguredModels（`--model-config` 文件），用配置的目录和其
provider 声明的 `apiKeyEnv` 连接。绑定和运行是共享的能力代码
（`dim_agent_capabilities::auxiliary_model`），两个所有者用同一份实现。mock 配置不选择模型，
因此不提供这个操作。

用哪个模型由调用方决定，这个选择保存在调用方自己的业务配置里。这个操作不保存任何按调用方
区分的策略：没有用途 key，没有用途存储，也不新增管理方法。

S4 完成标准里只有一个使用者：SessionTitle（P4），它需要在自己的 Session 之外调用模型，又拿
不到凭据。不新增插件、不新增 Timeline 事件、不改基础设施。

### 1.1 为什么这么小

需求来自功能清单："Auxiliary models: configure models for auxiliary tasks"，以及设置页条目
"Auxiliary model and reasoning effort"。总体设计的早期版本把它变成了由 Provider 持有的按用途
策略（单一写入路径），还让压缩也走这个服务。对照代码，两者都超出了需求：

- 配置辅助模型是执行该任务的使用者自己的业务设置。每个使用者保存自己的选择；CLI 设置页逐个
  读取各所有者，汇总展示。没有任何需求要求由 Provider 保存它们。
- 压缩已经有自己的端口 `ResolveContextModel` → `ContextModel`
  （`dim-agent-capabilities/src/host.rs`）：上下文窗口、输出预算、token 计数和 `compact`。
  只返回文本的 `complete` 会丢掉这些信息。生产压缩宿主（F1）实现这个端口，不使用
  `auxiliaryModel`。

## 2. 结构

```
 进程作用域
 +----------------------------------------------------------------+
 |  Provider（产品）或 ConfiguredModels（model-config）           |
 |   目录、凭据与默认选择（不变）                                 |
 |   AuxiliaryModel --使用--> 共享的 auxiliary_model::complete、  |
 |                             Aimux ModelHost                    |
 +----------------------------------------------^-----------------+
                                                | complete(model, request)
 Session 作用域                                 |
   SessionTitle（Deps：AuxiliaryModelHost） ----'
     model = 其 Settings 来源 `session-title` 的 model，否则为 Session 的模型
```

依赖指向核心：两个所有者都新增对 Aimux `ModelHost` 的硬依赖，并使用 `dim-agent-capabilities`
中共享的运行；两者都不点名任何使用者。

## 3. 数据与状态

无。Provider 不新增存储、不缓存；进行中的调用由它现有的准入与任务跟踪管理。ConfiguredModels 没有准入，也不跟踪任务。

## 4. 接口

### 4.1 语言中立（ADR 002 D5）

`auxiliaryModel`，进程作用域，无实例 key，由该配置中模型选择的所有者提供（产品配置为
Provider，model-config 配置为 ConfiguredModels）。

| 方法 | 模式 | 请求 | 回复 |
| --- | --- | --- | --- |
| `complete` | async | `{model: {providerId, modelId}, request: {system?: string, messages: Message[], maxOutputTokens?: u32}}` | `{text, stopReason: "end_turn" \| "max_tokens" \| "stop_sequence", usage?: {promptTokens, completionTokens, totalTokens, cacheReadTokens?, cacheWriteTokens?, cost?, currency?}}` |

- `model` 必填。调用方从自己的配置中解析出它；Provider 不回退到其他模型。
- `messages` 使用现有可序列化的 `Message` 类型（dim-agent-types），例如
  `[{role: "user", content: "…"}]`。不提供工具；丢弃 thinking 输出；只有文本块组成 `text`。
- `stopReason` 有三个取值。Rust 的 `ModelStopReason` 还有 `ToolUse`；因为不提供工具，出现工具
  停止属于提供方缺陷，调用以 `failed` 失败。
- `maxOutputTokens` 限制回复长度；缺省为模型上限，并截到模型的 `max_output_tokens`。
- 非流式：使用者需要完整文本。
- 取消：使用者丢弃调用（或发送桥接协议的取消请求）。服务本身没有超时，预算归使用者。
- 错误是数据 `{code, message}`：`invalid_request`、`model_unavailable`、`failed`、
  `cancelled`（§6）。

### 4.2 Rust 绑定

放在 `dim-agent-capabilities::host`，与 `ModelSelectionHost` 相邻：

```rust
pub struct AuxiliaryRequest { pub system: Option<String>, pub messages: Vec<Message>,
                              pub max_output_tokens: Option<u32> }
pub struct AuxiliaryReply { pub text: String, pub stop_reason: ModelStopReason,
                            pub usage: Option<Usage> }
pub enum AuxiliaryErrorCode { InvalidRequest, ModelUnavailable, Failed, Cancelled }
pub struct AuxiliaryError { pub code: AuxiliaryErrorCode, pub message: String }
pub trait CompleteAuxiliary: Send + Sync + 'static {
    fn complete(&self, model: ModelId, request: AuxiliaryRequest, cancel: CancellationToken)
        -> Pin<Box<dyn Future<Output = Result<AuxiliaryReply, AuxiliaryError>> + Send + '_>>;
}
pub struct AuxiliaryModelHost(pub Arc<dyn CompleteAuxiliary>);
```

`auxiliary_model::provide` 把所有者的操作同时绑定为 `AuxiliaryModelHost` 和具名绑定。每个
所有者的 `apply` 用各自的 `CompleteAuxiliary` 实现调用它；其 `Deps` 增加 `Arc<ModelHost>`。

### 4.3 具名绑定

所有者提供的同一个操作经上述共享的 `provide` 在 `host_key("auxiliaryModel")` 实现
`HostDispatch`，进程 catalog 登记此共享名称。普通 C4 行注入 `auxiliaryModel`，调用
`complete({model, request})`；Rust 的 SessionTitle 部分改为注入 `AuxiliaryModelHost`，两者
到达同一个所有者操作。SDK 在
[`dimcode/plugin`](../../../../npm/plugin.d.ts) 中将其声明为 `AuxiliaryModel`。这是既有操作的绑定，
不新增插件或另一套模型执行器。

调用接收一个含 `model` 和 `request` 的数据对象。在此边界解码原生请求一次；未知字段、非法消息
数据或参数形状以 `invalid_request` 拒绝。可选的 `system` 和 `maxOutputTokens` 可以缺省或为 null。
回复沿用原生 `AuxiliaryReply` 序列化，字段用 camelCase，停止原因用 snake_case；没有 usage 时
省略该字段。失败拒绝为带既有 `code` 和 `message` 的 Error，不增加回复信封。Provider 的调用
使用它的准入、任务跟踪和清理；ConfiguredModels 随进程存在，没有准入，调用以自己的结果或
调用方的取消结束。凭据和连接句柄仍留在所有者内。

可选的第二个参数 `cancelled: () => Promise<void>` 沿用 C5 的函数引用机制，绑定原生取消 token。
返回的 Promise 完成或失败（含其语言运行时消失）时，取消原操作，并等待原操作结算后返回。使用者在
超时或 closing 时完成它；单纯停止 await 不代表取消。不增加桥接协议或独立的取消任务。

## 5. 流程

```
 使用者            AuxiliaryModel（所有者）    目录/连接             ModelHost（Aimux）
    | complete(model, req)  |                       |                       |
    |---------------------->| 检查请求              |                       |
    |                       | 解析模型与默认值 ->| 可选？                  |
    |                       |<-- 模型配置 | model_unavailable              |
    |                       | 构造 ModelRequest（system、messages、max_tokens、无工具）
    |                       | 解析连接（凭据留在这里）
    |                       | invoke(request, connection, cancel) --------->|
    |                       |<------------ 块：Text、Usage、MessageStop     |
    |<-- {text, stopReason, usage}                                          |
```

Provider 通过 `ProviderService` 解析模型、通过 `ProductResolver` 解析连接；ConfiguredModels
则从 `--model-config` 文件及其 provider 声明的环境变量解析两者。

失败处理与 Session 请求用同一套策略，放在一个共用函数里
（`dim-agent-capabilities::model_provider::invoke_with_recovery`，ModelProvider 也用它），
所以这套策略只有一份实现：

- **凭据：** 被拒的凭据恢复一次；恢复后再次被拒（`Terminal`）时，先由凭据所有者的
  `finish_rejected` 结算（例如托管账号被登出），再让调用失败。
- **瞬时失败：** 在共用预算内退避重试（至多三次尝试、等待合计 60 秒），提供方的
  `ModelFailurePolicy::forbids_retry` 禁止时不重试。

此外的重试归使用者（SessionTitle 在之后的输入时重试）。任何地方都不写事件。

## 6. 错误

| 代码 | 何时 | 重试 |
| --- | --- | --- |
| `invalid_request` | `messages` 为空、`maxOutputTokens = 0` | 否（调用方缺陷） |
| `model_unavailable` | 模型不可选，或其 Provider、目录缺失 | 用户修正使用者的设置或目录之后 |
| `failed` | 共用策略处理之后仍失败的连接解析、传输、HTTP 或流错误；工具停止 | 由使用者决定 |
| `cancelled` | 调用被丢弃、取消令牌触发，或 Provider 卸载（其停止令牌） | 由使用者决定 |

## 7. 安全

- 凭据不离开所有者：`ResolvedModelConnection` 在 `complete` 内部构造和使用，从不返回或序列化。
  使用者只看到文本、停止原因和用量。
- 错误消息不含秘密：`failed` 使用现有 `HostError` 的消息，它不包含凭据值；所有者从不把连接
  格式化进消息。
- 进程日志：`complete` 记录一条 `tracing` 事件（target 为 `auxiliary.complete`），含 provider id、model id、
  结果、耗时和 token 用量，从不包含请求或回复内容。使用者自行记录自己的结果日志。辅助
  调用不是 Session 事实，不进入任何 Timeline。

## 8. 与现有部件的关系

| 部件 | 关系 |
| --- | --- |
| 默认选择（`provider.select`） | 不变。`complete` 不使用它 |
| SessionTitle（P4） | Rust 部分（`dim-session-title`）要求 `AuxiliaryModelHost` 并调用 `complete`；`model` 取其 Settings 来源 `session-title` 中可选的 `model`，否则取前缀中最新 `context/configured` 的模型。没有提供该操作的所有者时，必需依赖让该部分保持 Pending；原生 Session 不依赖该行。具名 `auxiliaryModel` HostDispatch 绑定仍供 TypeScript Session 部分使用。SessionTitle 自身的 Ready 决策和实现仍单独处理 |
| 压缩宿主（F1） | 不使用本服务。实现 `ResolveContextModel`；如何选模型、如何读上下文窗口由 F1 设计 |
| Collaboration | S4 不带来变化。成员模型 = profile 的模型，否则为默认选择；如果以后需要单独的成员默认值，那是 Collaboration 自己的设置 |
| Modality | 把每个类别的默认模型保存在自己的 Settings 来源 `modality` 中，并显式传入模型。非对话型连接解析是另一项 Provider 补充，不在 S4 内 |
| Workflow | `stage.model` > profile 模型 > Agents 默认选择已经覆盖模型选择；该操作没有阶段默认值 |
| `--model-config` 配置（ConfiguredModels，没有 Provider 插件） | 用配置的目录及其 provider 声明的连接提供 `auxiliaryModel`，走同一份共享运行；SessionTitle 的必需依赖在该配置中同样满足 |

## 9. 规则检查

| 规则 | 回答 |
| --- | --- |
| 原则 §3（依赖方向） | 每个所有者（核心）只依赖能力代码和 Aimux（核心）；使用者依赖该操作；没有任何所有者点名使用者。 |
| §4 规则 3（before 回调） | 不使用。 |
| §4 规则 8（基础设施不变） | 不改 Timeline、Dispatcher、事件格式、rutis 或装配；使用 `ModelHost`、`ProductResolver` 和现有目录。不向 rutis 提需求。 |
| §4 规则 9（只有需要的事实进 Timeline） | 辅助调用不写事件。 |
| ADR 002 D2（无接线模块） | 不新增插件；共享运行是能力代码，由现有的模型选择所有者提供。 |
| ADR 002 D3（依赖） | 核心不硬依赖可选插件；SessionTitle（可选）可以依赖本服务。 |
| ADR 002 D4（配置） | 该操作不新增配置；每个使用者的模型选择是它自己的业务配置。 |
| ADR 002 D5（语言中立契约） | §4.1 是契约；§4.2 是一种绑定。 |

## 10. 开放问题

推迟：按用途的推理强度（功能清单："Auxiliary model and reasoning effort"）。如果要加，它是
使用者自己设置里的一个字段，`complete` 把它作为请求参数接收；随 `/settings` 设计决定，不在 S4 内。

## 11. 验收（流程测试）

1. 对 mock HTTP provider 调用 `complete`：请求发往给定模型，带 system 文本、messages、
   `max_tokens` 且无工具；回复是文本加用量。
2. 模型被禁用或不存在：`model_unavailable`，不发请求，不回退。
3. 删除模型所属的 Provider：下一次调用返回 `model_unavailable`。
4. 流进行中丢弃调用会取消请求；卸载 Provider 会以 `cancelled` 结束进行中的调用。
5. 托管账号恢复后的凭据再次被拒：账号被结算（登出），调用以 `failed` 失败；端点恰好收到两次请求。
6. 回复、错误和 `auxiliary.complete` 日志事件都不含 API key；调用后 Session 的 Timeline 没有新事件。
7. 真实 Node Session 部分按名称注入进程服务，经 `complete({model, request})` 发起 HTTP 调用；
   回复字段和抛出的错误码保持不变。非法请求、未知模型不发送 HTTP 请求，调用不写 Session 历史。
   将模型请求停在 HTTP 端点后，关闭调用方会完成取消引用，在 Provider 保持装载时关闭 HTTP 流。
   没有 auxiliaryModel 的所有者时，原生 Session 仍能创建，只有外来命名空间不可用。
8. model-config 配置用其目录及 provider 声明的连接提供并运行该操作：调用把请求发往配置的模型，
   `maxTokens` 截到模型的 `max_output_tokens`；未声明的模型返回 `model_unavailable` 且不发请求；
   Session 部分和 Session 的自动标题在该配置中都能到达它。
