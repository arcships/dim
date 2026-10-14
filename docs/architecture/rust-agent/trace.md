# Rust 运行时诊断与 Trace

状态：已实现；诊断经 `diagnostics/{sessionId}` 通道交付。

Trace 是可选观察能力，不是恢复数据，也不参与业务决策。诊断接口独立于 App Server RPC、客户端接入和业务事件兼容基线。

诊断遵守[单树事件合同](single-tree.md#事件)：`diagnostics_channel(session_id) -> EventKey<DiagnosticRecord>`，调用分别为 `emit(host_ctx, &key, Arc::new(record))` 与 `on(host_ctx, &key, listener)`。接纳失败须报告诊断交付错误，不能视为归档完成。

## 分工与复用

- runtime 为每个 Session 提供 `Diagnostics`，把诊断记录以 rutis 命名通道 `diagnostics/{sessionId}` 发出（`emit`）；通道键由 runtime 导出的 `diagnostics_channel(session_id)` 生成。分支的记录带独立 `branch_id`。Timeline 在成功提交后发出原始事件及现有 before 轨迹，不创建第二份历史。
- Session 级 `TracePlugin` 构造时取得 `TraceHost`（宿主归档记录端与比 Session 更晚卸载的宿主 ctx），经 `injects` 获取 `SessionIdentity` 与 `Diagnostics`；在 main 分支装配前，先以宿主 ctx 注册诊断监听，再开启诊断。未开启时不产生记录，也不创建 span。
- span 直接使用 `tracing::Span`；`tracing-subscriber::Registry` 分配 ID、保存扩展并报告最终关闭。每个 Session 使用局部 `Dispatch`，不替换应用全局 subscriber。扩展中的 `Instant` 仅用于计算从创建到真实结束的墙钟耗时。
- 组件就绪来自 rutis 的现有诊断快照，订阅释放来自 rutis 拥有的 listener 析构；不新增插件注册表或任务管理器。
- 各 Capability 在自有操作上记录结果。取消继续使用既有 `CancellationToken`、TaskTracker 和 rutis Effect；不能通过结束 span 宣称任务已结束。
- 宿主拥有监听和归档资源。监听不随 TracePlugin 或 Session 卸载，收到该 Session 的结束记录后由归档释放。App Server 的宿主 ctx 为 Agent 的 Sessions 父节点。

## 诊断约定

`DiagnosticRecord` 包含 Session、可选分支、时间和类型化事件，实现 rutis `Event`。同一 Session 的记录在其通道上按发出顺序到达；Session 完整关闭后 runtime 发出结束记录 `DiagnosticEvent::Closed`，它是该通道的最后一条记录。诊断类别包含组件、订阅、span、Timeline；span 通过 `turn_id`、`request_id`、`call_id` 与业务事件关联，ID 以 Session 为作用域。基础 span 不假设跨 Tokio 任务自动继承线程局部上下文。

组件 `released` 表示 rutis 关闭等待已结束，不表示业务成功；关闭 span 的 `outcome` 区分正常结束和失败。不能把某个清理错误归因于同 root 的全部插件。Session 与每个分支是同一 rutis 树上的作用域子树，组件快照按作用域记录；不同 `session_id` / `branch_id` 下的同名组件（包括 `SessionScope`、`BranchScope`）不是同一个实例，不按组件名跨范围去重。

宿主监听器是非阻塞的交付边界，只把记录排入不限长度的归档队列，不执行磁盘 I/O、等待或重入 runtime。写入端已停止、错误或 panic 使该 Session 的归档标记 `incomplete`；监听器错误进入 rutis root 的错误出口，不进入 FaultReporter，不改变业务结果、停止或清理。

原始记录可能包含秘密，只有可信宿主可以消费。归档必须在写入前脱敏，不以字段黑名单保证任意文本安全。归档缺少成功完成标记、写入失败或发生丢弃时，都必须视为不完整，不能作为恢复输入。

完整排查由宿主显式选择 `ArchiveContent::SensitiveContent`，不改变默认摘要模式。该模式保存最终提交的事件正文、模型请求、上下文、工具输入输出和 before 链。写入前递归遮蔽已知凭据字段（包括 headers、authorization、apiKey、token、password 的常见命名）；这只是降低误泄漏风险，正文中的凭据、个人数据、路径和代码仍可能保留，文件必须按敏感数据管理，不能直接分享。完整表示保留已采集内容，不表示保证采集无损或所有秘密已清除。

Prompt 在实际来源加载期间记录 `prompt.load`，取消后仍等待宿主 future 结束才关闭 span。采用点使用 `prompt/changed`、`context/configured` 的 revision；`context/prepare` 与 `model/call` 携带固定配置和提示词引用。诊断层不推测或补造来源。默认归档保留版本、作用域和请求引用，不保留来源名称、路径或正文；加载来源信息仅存在于可信宿主的原始 diagnostics 中。

## 本地归档

`dim_agent_capabilities::trace::archive::TraceArchive` 是宿主适配器。宿主用 `create(directory)` 创建新目录，或用 `new(writer)` 提供 Tokio `AsyncWrite`，经 `trace::install` 把归档与宿主进程 ctx 交给该 Session 的 TracePlugin。文件初始化失败返回 I/O 错误，由宿主报告并决定是否继续无归档运行；不把该错误转换为 Agent 故障。

关闭顺序是 `agent.close().await` → `archive.finish().await`。后者先等待 `closed()`（已注册 Session 的结束记录全部到达），再排空队列、flush、shutdown，返回写入条数及 `incomplete`；写入或关闭失败返回 I/O 错误。取消 finish 的等待者不会中断 writer。直接丢弃归档拥有者会关闭入口，让 writer 排空并标记不完整；进程退出前仍需宿主等待 finish，不能依赖 Drop 保证落盘。

默认输出为新目录中的 `trace.jsonl`，不覆盖已有目录或文件。Unix 目录权限为 `0700`，文件为 `0600`；其他平台的目录 ACL 由宿主负责。路径仅由宿主提供，不从 Session、分支名或事件拼接。

| 格式 | 约定 |
| --- | --- |
| 版本 | 每行 `version: 1`；新增可选字段保持版本，删除字段、改类型或语义则升版本；读取器拒绝不支持的版本 |
| 开始 | `kind: archive_start`，`content: metadata_only` |
| 记录 | Session、分支、时间与 `record`；Timeline 记录保留本地 sequence、事件 ID 和结构摘要；分支间不承诺全局业务顺序 |
| 结束 | `kind: archive_end`，记录条数与 `incomplete`；缺失结束行、条数不符、解析失败或宿主报告写入失败时都不能认定完整 |
| 脱敏 | 显式保留结构标识、结果分类、模型标识与 blob ID；不保存正文、错误自由文本、tags、headers、请求配置、工具参数/输出、路径或 URL；before 的原始与变形结果使用同样的摘要规则 |
| 额度 | 不预设额度：队列不限条数与字节，单条记录不限大小，记录不因大小或拥塞被丢弃或省略；归档体积过大时由宿主按文件分片处理 |

完整模式使用 `create_with_content(directory, content)` 或 `with_content(writer, content)`，各行 `version: 2`，开始行 `content: sensitive_content`。Timeline 结构摘要不变，增加 `record.payload: { event, before }`，直接沿用事件与 before 的现有序列化契约，不造第二套业务类型。`event` 是成功提交的事实，不是供应商网络报文；`model/call` 中的 request 是交给模型能力的最终请求。没有该事件时不能把准备阶段当成最终请求。

完整模式同样不限单条大小，payload 总是完整写出，不截取字符串。读取器必须验证结束行的布尔状态、条数和结构，不支持的版本拒绝读取。

组件名、工具名、模型名和关联 ID 是可记录的结构元数据，产生方不得在这些字段中嵌入凭据。归档不复制工具产物，只保留 blob ID 关联，产物仍由原宿主管理。span ID 按字符串落盘，避免 JSON 使用者的整数精度损失。

## App Server 接入

`session.create.trace` 省略时不开启诊断。显式传入 `true` 或文件路径时，App Server 创建私有文件，并选择 `ArchiveContent::SensitiveContent`：保留此前显式开启 Trace 后可读取请求正文的用途，同时复用凭据字段遮蔽、内容预算和完整性标记。完整归档仍须按敏感数据管理。

App Server 在创建 Session 前准备归档，经 `Agent::create_session_with_extension` 装载本次请求的 `trace` 分组并传入 `TraceHost`。`SessionExtension` 在 Session 作用域服务就绪后创建扩展，不等待普通能力全部就绪；`TracePlugin` 按自身 `Deps` 注册监听并开启 `Diagnostics`。因此其他能力缺依赖或 apply 超时时仍能记录回滚与结束；不保证回放监听开启前的诊断，也不保证 Trace 自身无法装载时有记录。扩展与普通能力全部 Active 后才初始化 main。不把文件路径、归档对象或 Trace 开关加入业务 `SessionConfig`。

所有分支的记录经同一 Session 通道送达。归档保存初始化、原始 Timeline 事件、before 链、请求与工具关联、运行时 span 和关闭诊断；不从产品事件流反推诊断，不维护第二份历史，也不为每个分支创建文件写入任务。分支创建与审批摘要保留 `source`；创建失败摘要保留分支 ID、父日志创建位置和失败或取消分类，不保留错误自由文本。

`tracePath` 仍是文件路径，`trace: true` 在服务配置的目录内选择唯一文件名。文件内容统一为本页的版本 2 完整归档，替换旧的无版本 `{event, trace}` 主分支记录。旧文件不转换、不覆盖；解析脚本读取 `record.payload.event` 和 `record.payload.before`。

宿主等待 Session 完整关闭后才调用 `archive.finish()`，包括 RPC close、运行时故障和服务退出；关闭请求的连接断开不取消排空任务。Session 构建失败时，等待既有回滚结束，再排空初始化诊断。文件创建失败直接拒绝本次创建，不发布 Session。写入失败或内容丢弃只影响诊断完整性，不将已完成的业务操作改判失败；App Server 记录归档错误或不完整警告，读取者必须核对结束标记。业务失败但记录无损的归档仍可完整。

Trace 与 RPC 都以分支本地 sequence 定位事件。用 `session_id`、`branch_id`、事件 ID 和本地 sequence 对照 RPC；诊断中可能包含未发布候选分支的初始化与回滚，不能据此认为分支已对客户端可用。

## 验证

通过 `tests/integration` 的公开 Agent/Session 流程验证事件归属、分支隔离、订阅释放、正常收尾及监听器失败隔离。装配或依赖变化后运行 `cargo run -p xtask -- check` 和 `cargo run -p xtask -- graph`。不需要模型凭据。

在仓库根目录执行 `cargo run -p dim-agent-capabilities --example trace_archive -- /tmp/rust-trace-sample`，可用确定性模型和工具生成包含提示词初始化、配置采用、请求来源引用、主分支、子分支与工具调用的完整样本。目标目录必须不存在，生成结果为该目录下的 `trace.jsonl`。这是宿主接入示例，不会默认开启 App Server 或 CLI 的内容采集。
