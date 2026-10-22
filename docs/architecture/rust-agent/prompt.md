# 提示词生命周期与请求上下文

状态：Implemented。公开 Agent/Session 流程已覆盖配置采用、取消、分支隔离、上下文投影与压缩恢复。

本文定义提示词的内容分类、来源固定、交付位置、作用域和压缩规则，以及 Rust 的装配、并发及协议边界，不改变内核与能力的分工。

模型收到的内容按来源分为 system、规则（`<dim-rule>`）、状态（`<dim-state>`）与对话历史，由配套元提示说明各类内容的含义（首版实现，真实模型效果待评测）。

## 职责

| 所有者 | 职责 |
| --- | --- |
| 宿主 | 提供已解析的模型配置、可信提示词来源和业务事实；决定来源读取方式和更新时机 |
| Prompt Capability | 加载候选提示词、校验来源契约、管理版本与作用域、取消过期加载并等待真实收尾 |
| ContextPreparation | 发布本分支完整配置，接收固定来源请求并执行异步准备 |
| 纯投影 | 从固定历史生成模型消息、有效状态与交付关联；无 I/O 和写入副作用 |
| ContextPolicy | 预算、压缩结果验收与可丢弃恢复快照的写入；不加载提示词或选择配置 |
| Loop | 推进 turn，在请求边界固定历史、配置和提示词版本 |
| Timeline / Dispatcher | 原子保存事实、分配序号、按序分发；不解释提示词业务规则 |

Prompt 是 Timeline 级 rutis Plugin，依赖通过 `Deps` 声明。来源 I/O 使用能力宿主端口；任务使用 Tokio、CancellationToken、TaskTracker，不申请执行许可，资源取得后立即登记 rutis effect 清理。Session 只路由分支控制和只读状态，不管理提示词可变状态。

## 更新与发布

保留 `prompt/changed` 与 `context/configured` 两类采用事实。Prompt 拥有提示词版本，ContextPreparation 拥有完整配置；纯 context 提示词更新不必发布新的模型配置。system 变化和关联模型配置必须整体生效。无变化刷新不制造新采用版本。

关联来源全部加载成功、批次仍有效且作用域未结束后，准备整组采用事件，通过 Timeline 原子批量追加。所有事件先完成 before 和产生方登记，再整体提交并发布；任意失败零提交。历史读取截点与分支截点不能位于原子批次内部。before 是可信宿主代码，采用事实以最终提交事件为准；宿主修改关联来源、版本或 system 时负责整组事件的一致性，生产者不在变形后重复校验。

同来源、同范围的新刷新淘汰整个旧批次；模型配置候选之间同样互斥，无关来源可以并行。请求、加载结果及作用域控制的处理遵循所属分支事件顺序。Dispatcher 回调不等待加载或后续事件；加载工作归 Prompt，完成后回到能力的采用流程。加载失败保留旧组合，失败与撤销明确区分。取消信号不能代替批次有效性检查或真实收尾。

运行期刷新在提交锁内按最终请求标识登记操作，加载任务由 Prompt 持有至收尾；登记失败直接返回调用方，不发布请求或终止 Dispatcher。结果标识经 before 改写时，Prompt 在结果发布前同步迁移自身操作关联，调用方仍等待同一实际加载收尾；已占用的标识不能覆盖其他待处理操作。采用前作用域已经结束的批次返回 `Superseded`。

初始配置在分支启动时采用：Prompt 处理 `branch/start`（恢复时 `branch/resume`）时加载来源，分发完成后分支才接受输入。新 Session 的根分支整体采用初始配置；子分支与恢复的分支只比较规则与状态来源，不重新采用 system 与模型。初始失败回滚已建立资源。常规 turn 结束不取消分支级刷新；关闭拒绝新更新并等待实际加载收尾。

宿主通过 `defaults::install_with_prompt` 提供 `LoadPrompt` 和可选初始 `ConfigureUpdate`，运行期通过 `Session::configure` 刷新。宿主返回 `None` 表示撤销，返回错误表示保留旧版本。`Superseded` 在被替代加载真实收尾后返回；`Adopted` 在整组采用事件完成分发后返回。子分支可以在自身历史中撤销或替换继承的来源，不修改父分支。

`prompt/configure-request` 与 `prompt/configure-result` 只由 Prompt 自己消费，用来让配置操作与分支事件同序；加载结果只在内存里。Prompt 卸载或进程重启时，未完成的配置操作以错误返回给调用方，总线上留下的请求没有对应结果，也不需要补写：重载后的 Prompt 不重建未决操作，其他状态机不读取这两个事件，它们不影响任何后续决定。

`ConfigureUpdate.model` 只更新模型身份、参数与 metadata，保留已采用的工具与来源版本。产品的初始 `InitialPromptConfiguration` 只有 [PromptRegistry](plugins/PromptRegistry.md) 一个提供者，它从 `SessionScope.config.model` 读取完整 `ModelSelection`，与全部按名登记的来源合成同一更新，由 Prompt 在根分支 `branch/start` 时采用；组合根不重复提供该键。开发 profile 可以没有初始采用配置，继续使用 `PrepareModelHost`。

配置的工具部分只由 [ToolBinding](plugins/ToolBinding.md) 经 `Prompt::adopt_tools` 写入，Prompt 不注入 ToolRegistry（否则 `ToolHost` 的重载会连带 Prompt）。首次采用模型时配置没有工具部分：Loop 经 `ToolBinding::bind_current` 声明当前固定工具；有动态来源时 ToolBinding 采用完整工具部分。模型更新与工具采用在 Prompt 的串行队列中分别只替换各自部分，互不覆盖；只更新其中一部分时 `system` 沿用已采用的值。

来源作用域的可见性取决于当前分支与固定祖先历史，撤销不抹除祖先身份，子分支可以重新加载已撤销的继承来源。工具结果不携带提示词条目：Skill 正文只是工具结果，不登记规则；上下文注入一律经输入写入历史。

## 请求、投影和压缩

请求从固定历史位置选取已发布配置与提示词版本，不混合宿主实时设置。`context/prepare` 与 `model/call` 保留来源关联，不另外建立逐请求提示词渲染事件链。预算恢复复用原来源和准备结果，后来到达的规则只能影响后续请求。

未发布配置的既有宿主保留 `PrepareModelHost` 路径；一旦采用完整配置，Loop 直接读取固定前缀中的配置，不再调用该路径选择模型。两条路径都带有配套元提示。`Session::input` 接收结构化 `user` 与 raw `blocks`；RPC 只接收操作者内容，不接收块。没有指定模式且没有 `user` 的输入按 Append 等待合法输入边界；只有块的输入不唤醒空闲分支。

system 只保存静态内容。Context 与 State 投递的提示词变化由 Prompt 写成 Append 输入（`<dim-rule>`、`<dim-state>`，每项一条，与 `prompt/changed` 同一批次，按 `order` 排列；作用域结束时写 expired），在 Loop 消费的位置进入上下文，不改写已交付历史。分支创建或恢复时，Prompt 在 `branch/start`、`branch/resume` 分发时加载全部来源并写入变化；新 Session 的根分支同时采用 system 与模型，子分支与恢复的分支不改 system。来源由各能力按名登记，经 PromptRegistry 汇总。

已完成压缩事实（`context/compaction` 的 `Completed`）包含 source through、cursor、summary 与 cursor 处的有效状态基线，是持久历史事实；恢复快照是可选的派生缓存，不能替代它（AGENTS 规则 9，见 [Timeline 存储](timeline-storage.md#压缩检查点)）。产生方从固定历史计算基线，摘要、基线和后缀一起验收并计入预算，不能用请求上界 through 的最新状态代替 cursor 处状态。采用与后续恢复使用最终提交的事实，包括可信 before 对基线的变形，不在消费方重新计算并覆盖。基线按 key 维护：每个仍有效的 key 的最新块原样重放，已结束（retire）的 key 不重放。投影取最新的 completed 事实；其 cursor 无法投影到固定历史时明确失败，不跳过、不重新生成摘要。快照缺失或无效只回退完整事实重放。

任务作用域由创建任务的业务 Capability 定义，不等同于脱离工作的执行句柄。父分支 turn 范围不延续到子分支；分支与业务任务范围从祖先链历史恢复，在子分支独立演进。继承只做纯投影，不重放加载、工具或通知。

## 协议与日志

内部统一类型在 `dim-agent-types` 定义。App Server v1 的固定事件样本保持不变，新事件和扩展字段由出站适配处理，保留真实 sequence。内部演进不以旧 wire 格式限制职责分工。

采用与请求关联属于业务事实，记录在 Timeline。来源加载的耗时与结果属于进程日志：每次实际加载一个 `prompt.load` span，带 Session、分支、来源名与作用域，加载出条目时记录其版本；不记录正文。日志不参与业务决定，也不是恢复数据（[ADR 005](../../adr/005-observability.md) D1、D4）。

## 验收

公开流程检查关联更新不混版、任一加载或提交失败不半发布、重叠刷新乱序完成不覆盖新版本、取消和初始化回滚等待真实清理、请求与恢复固定来源、分支截点与隔离、工具正文不重复交付、撤销及作用域结束在压缩后仍有效。

在仓库根目录运行 `cargo test --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、格式检查、`cargo run -p xtask -- check` 和 `cargo run -p xtask -- graph`。固定 v1 样本位于 `crates/dim-agent-types/tests/fixtures`，由 `dim-agent-types` 的 wire 合同测试校验。
