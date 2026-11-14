# 文件与图片传输：现状审计及改造提案

本文是待评审的研究方案，不修改现有所有者契约，也不表示新接口已经可用。
审计基线：Dim `0403f52a2`；另核对 PR [#1398](https://github.com/arcships/dim/pull/1398) 的 `154c45205`。
通信实现核对锁定的 rutis `v0.19.3`；本地 rutis `2b131e77` 的相关编码也仍为 Base64。
关联范围：[#1288](https://github.com/arcships/dim/issues/1288)、[#1292](https://github.com/arcships/dim/issues/1292)、[#1297](https://github.com/arcships/dim/issues/1297)、[#1306](https://github.com/arcships/dim/pull/1306)。

## 1. 结论与问题来源

明确暴露上限冲突的是 #1398 的 `sessionFiles.put` 跨语言评审，不是已知的 Web 图片上传测试失败。
[原始讨论](https://github.com/arcships/dim/pull/1398#discussion_r4528628029)指出：原有 16 MiB 文件经 Base64 后需要 22,369,624 字节，尚未计算调用信封，无法通过 16 MiB 消息上限。
PR 把公共存储上限改成 11 MiB 后，连不经过通信通道的本地调用也被收窄。这个改变没有获得明确的兼容性决定，不应作为本方案前提。

Web 的图片路径有同类结构性问题，但不是同一个 API：浏览器直接把图片放进 `session.submit`，长文本才走 `attachment.put`。
所以只修 `sessionFiles.put`，不能修好 Web 图片，也不能解决图片随历史、工具结果和模型请求再次跨边界的问题。

建议：保留原有文件能力，把大块内容从普通控制消息中移出。文件仍由原所有者管理；传输采用有界操作；模型边界仅在供应商协议要求时编码 Base64。
不把所有二进制都改成文件，也不移除已有小消息 API。

## 2. 经代码核对的现状

| 边界 | 当前行为与证据 | 影响 |
| --- | --- | --- |
| Web 图片 | [ComposerPanel](../../../web/src/features/chat/ComposerPanel.vue) 的 `readImage` 使用 `readAsDataURL`，保存完整 Base64；[chat.submit](../../../web/src/stores/chat.ts) 把图片放入 `user.content` | 多张图片共享一个请求上限；UI 同时持有预览 data URL 和提交内容。未测量实际峰值，不能给出精确内存倍数 |
| Web 长文本 | 同一 `chat.submit` 将完整文本交给 `attachment.put`，成功后把路径引用放入输入 | JSON 转义也会膨胀；10 MiB 原文不保证任何内容都能放入 16 MiB 请求 |
| CLI 图片 | [images.rs](../../../crates/dim-cli/src/images.rs) 缩放后构造 Base64 `ImageSource`，按总编码量拒绝过大提交 | 已有缩放与图片数限制，但仍受整条提交大小约束；不能仅改 Web |
| Session 文件 | [owner contract](../appserver/session-files.md#9-interface)；#1398 的 put 一次接收完整 Base64 并解码，再调用公共 owner | 原 16 MiB 存储契约与传输预算是两种限制；11 MiB 是该 PR 的实现折衷 |
| rutis 数据值 | [v0.19.3 session.mjs](https://github.com/arcships/rutis/blob/v0.19.3/node/rutis-runtime/src/session.mjs) 的 bytes tag 仍使用 Base64，MAX_MESSAGE 为 16 MiB | 改成 Uint8Array / Value::Bytes 只改善类型，不消除消息膨胀和整包内存 |
| rutis WebSocket | [v0.19.3 connection.rs](https://github.com/arcships/rutis/blob/v0.19.3/crates/rutis-bridge/src/websocket/connection.rs) 拒绝 Binary 消息，保留给未来编码 | WebSocket 能传二进制，不等于当前 rutis 通道已支持；也不能靠 WebSocket 分帧绕过整条 message 上限 |
| App Server 客户端通道 | [outbound.rs](../../../crates/dim-app-server/src/outbound.rs)、[transport.rs](../../../crates/dim-app-server/src/transport.rs)、[web_entry.rs](../../../crates/dim-app-server/src/web_entry.rs) 设置独立的 16 MiB 边界 | 只调 rutis 限制不够；App Server WebSocket、stdio 客户端及其解析内存仍需考虑 |
| 文件底层 | [FileStorage](../../../crates/dim-agent-capabilities/src/file_storage.rs)、[PluginFiles](../../../crates/dim-app-server/src/plugin_files.rs) 有 pinned reader、64 KiB 有界读取、临时文件加 flush/rename 的流式写入 | 可复用本地存储机制；`write_stream` 的 Rust `io::Read` 不是可跨语言的数据面 |
| 远程文件设计 | [resource faces §4.1](../resource-faces.md#41-methods) 已提出 `openRead/readAt/closeRead`；`replace` 仍携带完整 bytes | 读取已有有界设计，写入仍有同类大消息风险；这些是目标契约，不能当作已实现接口 |
| HTTP 下载 | [web_entry/resources.rs](../../../crates/dim-app-server/src/web_entry/resources.rs) 已有鉴权、Range、ETag、64 KiB 读取及容量为 1 的发送队列 | 可以扩展既有 Web 边界；现有 UiResources 是运行期发布资源，不能直接当成历史图片的持久身份 |
| 图片数据与模型 | [content.rs](../../../crates/dim-agent-types/src/content.rs) 目前只有 Base64/URL；[aimux request.rs](../../../crates/dim-agent-aimux/src/request.rs) 转成供应商文件输入 | 还没有 Session 文件引用类型；图片引用不能只在 Web 类型里新增 |
| 持久化与历史 | [Timeline 存储](../../../crates/dim-app-server/src/storage/timeline.rs) 序列化事件；现有 ContentBlock 可以携带完整图片 | 大图会进入事件负载；换上传协议却继续把 Base64 写回输入，不能解决历史回放与大事件传输 |
| 其他输出 | [Report](../../../crates/dim-report/src/render.rs) 的 ZIP 明确限制为 64 KiB；PluginFiles 全缓冲操作限制为 8 MiB | 小且有界的结果不值得首批改造；不能见到 Base64 就全量替换 |

本轮是源代码与契约审计，未做峰值内存、带宽或真实浏览器大文件故障复现。下文的性能指标是验收要求，不是测量结果。

## 3. 分开三种预算

1. **业务预算**：Session 文件大小、附件文本长度、图片张数、模型支持的像素/格式，由对应 owner 决定。先保留原 16 MiB Session 文件能力，不借传输改造提升或降低产品上限。
2. **传输预算**：每块字节数、每连接在途窗口、队列、临时磁盘、并发传输和超时。文件大于一条消息是正常情况。
3. **模型预算**：缩放、EXIF、动画首帧、模型 vision 支持及供应商请求编码。复用[现有图片规则](../../architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images)，不由 HTTP 上传决定模型内容。

例如 11 MiB 文件的 Base64 为 15,379,116 字节，约 14.67 MiB；“15.4 MB”与“15.4 MiB”不能混用。
单纯把一个 16 MiB 文件改成 16 MiB 二进制消息仍没有信封余量；多图提交也仍会失败。

## 4. 推荐结构与取舍

目标流程（不是当前实现图）：

```text
Web File/Blob / CLI file / plugin bytes
           |
           +-- Web: authenticated HTTP binary body --+
           +-- stdio / rutis: bounded data chunks ----+--> owning file service
                                                        staging -> commit
                                                             |
                                                     durable file reference
                                                             |
                        session.submit / tool result / history (metadata)
                                                             |
                                 model preparation -> provider-required encoding
```

### 4.1 必选的第一阶段：有界传输，先覆盖现有通道

- 在文件 owner 的数据面提供有界上传操作，Web、CLI、插件都能使用；不以新 rutis 二进制协议为交付前提。
- 现有 JSON 通道的每块允许 Base64。这是**有界块的线编码**，不是把整个文件塞回一条消息。可先用现有读取机制的 64 KiB 作为测量起点；实际值由接收端返回并受编码后帧预算约束。
- 写入在目标节点逐块进入临时文件，禁止接收端把所有块收集成一个 Vec 再调用原 put。
- 同节点直接调用同一 owner 操作，传递字节或从已授权的 pinned reader 复制，不必编码后再解码。不接受浏览器提交任意本机路径作为“零拷贝优化”。
- 保留小 put 便利接口，但由 owner 共用校验、写入和提交逻辑；不制造第二套业务逻辑。
- 正确的文件内容到达服务后仍由业务上限检查。完整的小 put 接口在旧 JSON 通道上的包限制须明示，不能再把它称为所有合法文件的唯一传输入口。

### 4.2 Web 使用现有 HTTP 边界承载二进制

Web 保留 File/Blob，预览用 object URL 并在移除/完成时释放；上传原始字节，得到文件引用后提交元数据。
使用既有 Web listener、cookie/Origin 检查和 Session 访问边界。HTTP handler 调用文件 owner，不直接创建所有权记录或另写一套文件业务。
不新增旁路端口、不把本机绝对路径暴露为公开 URL、不让文件 ID 本身成为授权。

HTTP 上传可将请求体流式送到同一个 owner staging writer；stdio 客户端则走有界块。两种承载方式共享同一个提交/取消契约。
实现时须确认 HTTP 框架没有提前聚合 body，且请求体大小、超时和并发预算独立于 JSON RPC。

### 4.3 rutis 二进制承载作为后续优化

向 rutis 提出通用字节编码、能力协商、按字节背压及公平调度要求；不加入图片、Session 或磁盘路径概念。
新旧 peer 必须协商编码，不能直接开始发送当前会拒绝的 Binary 消息。
同一个有界块操作将来可换成二进制编码；文件所有权与 commit 语义不变。第一阶段不创建一个仅为转发而存在的 `BlobService` 插件。

### 4.4 不选的捷径

| 方案 | 不作为主方案的原因 |
| --- | --- |
| 把公共文件上限降为 11 MiB | 收窄本地能力且不能解决多图、历史和远程写入 |
| 全局把消息上限调到 32/64 MiB | 只推迟故障，放大并发内存及队头阻塞；涉及多个独立通道 |
| 只支持 Web multipart | CLI、跨语言插件和资源节点仍走整包 |
| 只改 bytes 类型 | 当前 rutis 仍 Base64 编码 |
| 给所有数据套通用 Blob 引用 | 让小配置、普通文本、短工具结果也承担文件生命周期复杂度 |
| 直接共享本机路径 | 浏览器/远程节点不可达，路径没有携带节点身份或访问资格 |
| 先做完整可恢复下载器/内容寻址仓库 | 超出问题需要；首批无需断点续传、跨租户去重或新对象存储 |

## 5. 文件 owner 所需的上传语义

以下为待所有者确认的能力草案，最终方法名及字段只能在 Session files 与 fsHost 的 owner 文档定义。不是新增一个全局传输服务。

| 能力 | 输入/结果的必要信息 | 保证 |
| --- | --- | --- |
| 开始上传 | owner 上下文、名称、声明长度、稳定请求键；返回 uploadId、接收块预算 | 在写入前校验业务大小与资源预算；ID 不授予访问权 |
| 写入块 | uploadId、offset、有限字节；返回 nextOffset | 连续写；重复块只有内容一致才确认，错位/不同内容失败；不产生可读最终文件 |
| 查询结果 | 稳定请求键 | 区分进行中、已提交、已失效，解决 commit 回复丢失，不用重新上传猜结果 |
| 完成 | uploadId、总长度，若协议选择则校验摘要 | 校验实际长度及文件内容；复用 owner 的原子提交；返回同一个持久文件引用 |
| 放弃 | uploadId | 幂等；清理尚未提交的临时内容；不能撤销已提交文件 |

- 文件引用还需要读取契约：当前 `sessionFiles` 的 put/register/owns/owned_paths 并没有按 fileId 读取字节的操作。请求 Session 文件 owner 增加基于已授权 Session 上下文的有界读取，复用底层 pinned reader；HTTP 与模型消费者不得绕过 owner 把 fileId 拼成本机路径。操作名称和错误由 owner 决定。
- SessionStore 继续拥有文件及 Session 所有权记录；底层 FileStorage 继续拥有实际 I/O、临时文件、权限和原子替换。fsHost 需要表达有界写入事务，复用同一底层实现，不复制 Session 业务。
- staging ID 属于连接/owner 代次。断连或卸载清理未提交内容；第一阶段不承诺跨进程重启续传。
- commit 一旦生效，取消不能假装未写入；必须查询或返回已提交结果。稳定请求键的完成映射应随 owner 持久化，保留期至少覆盖承诺的重试窗口；具体 schema/保留窗口需在 owner 设计中决定，不能只保存在连接内存。
- HTTP 上传中断与旧 `attachment.put` 的 drain 语义不同：新操作清理未完成 staging；不偷偷改变旧方法的断连保证。
- 上传完成不等于输入已采用。首批保持现有 Session 文件归属语义：即使用户放弃草稿，已完成文件也不自动删除，避免删除被未知结果输入引用的文件；额外的“未使用附件”回收需另行定义引用判定，不靠 UI 猜测。
- Session 删除与 commit 竞态沿用“最终提交时 Session 仍存在”的规则；文件所有权发布失败不能暴露成功引用。
- 起步可按序单窗口验证正确性；公网吞吐需测量后采用小而有界的流水窗口，不能并发无限块。64 KiB、100 ms RTT、单在途理论上仅约 0.625 MiB/s，单窗口不是性能目标。
- 接收端按实际写入字节计费；不信任声明长度。断连、恶意大量 begin、超时和磁盘满必须有有界资源占用。不得在接收了整包后才检查上限。

## 6. 图片应成为持久内容引用

建议由内容类型 owner 增加 Session 文件来源，而不是 Web 单独扩展类型：图片块引用不可变文件 ID，媒体类型由接收边界识别；Session/资源节点位置从 owner 记录解析。
暂定概念为 `image(source = sessionFile(fileId))`，具体 JSON 拼写、版本与 schema 待 owner 批准；不得直接改冻结兼容性夹具。

- `session.submit` / `session.input` 校验引用属于当前 Session（fork 后可有多个所有者），继续执行原有 SubmitChecks、submissionId 去重与采用流程。文件引用不是绕过输入检查的入口。
- `read` / screenshot / MCP 图片工具结果也要审计：若仍把整图留在 `ToolResult.content`，大消息会从回复方向回来。引用必须在 `content` 中可表达；只放 `structuredContent` 对模型不可见。
- 模型准备阶段在现有 owner 任务中解析引用、读取并按现有图片策略生成模型表示；不能在 Timeline before/提交锁中做文件或网络 I/O。
- 所有模型可见图片仍来自已采用输入或工具结果；文件解析不是新增上下文注入路径。
- 如果 `ModelHost` / `PrepareModelHost` 位于远端，引用必须穿过这些内部数据面，不能在控制节点提前具现化成 Base64 再发送大请求。最终供应商 I/O 所在节点通过声明的数据面依赖读取文件；同节点读取是同一契约的绑定。#1399 的 host-port 请求/响应也必须参与引用类型兼容性评审。
- aimux/provider 适配只接受准备好的模型输入；由供应商能力决定 data/base64、供应商文件 ID 或受控 URL，不把内部本机路径或私有 URL 交给外部供应商。外部 provider 只接受 inline 图片时在最外层协议适配中编码；供应商请求预算不反向成为文件存储上限。
- 大图片的解码、缩放本身仍消耗内存。传输有界不代表图片处理零内存；保留像素/解码预算，模型请求同时具现化的图片数也需有界。
- 当前图片缩放在部分客户端/read 工具发生。首批传输迁移保持其可观察规则；是否保留原图及派生缩略图属于图片 owner 的产品决定，不能顺带改变历史看到的像素。
- 历史页面返回引用/元数据，图片按需经鉴权资源接口读取；缩略图可作为派生缓存，但不能成为唯一原件。
- 不把 UiResources 的活 Session/代次发布 ID 作为持久图片身份。可复用其 HTTP 读取机制，持久性、fork 和清扫仍归 SessionStore。

## 7. 相关接口方与优先级

| 方 | 必需改造/保持事项 | 优先级 |
| --- | --- | --- |
| SessionStore / `sessionFiles`（#1398） | 恢复原 16 MiB 能力；有界上传、取消/结果恢复、统一提交；记录只引用完成文件 | P0 |
| FileStorage / `fsHost`（#1297） | 复用临时文件原子替换；补写入侧的有界数据契约，保留已有读取设计 | P0 |
| Web composer / chat / WebEntry | File/Blob、binary HTTP、引用提交、草稿失败保留、鉴权下载与预览 | P1 |
| CLI / dim-client / Attachments | 本地与 stdio 共用有界入口；长粘贴不再受整包转义膨胀；保持现有附件业务限额 | P1 |
| ContentBlock / Session 输入 / Timeline / 历史窗口 | 单一文件引用类型；协议与存储版本决定；旧 inline 读取与新引用分页 | P1，与 Web 同批验收 |
| ModelProvider / PrepareModel / aimux / 远端 ModelHost（#1399） | 引用跨内部调用，仅在供应商 I/O 节点解析；声明文件读取依赖；保留 vision/缩放规则 | P1 |
| WorkspaceTools read、截图、MCP 图片结果 | 消除大图片通过 ToolResult 回传的另一条整包路径；保留文本结果语义 | P1/P2，按来源逐个覆盖 |
| Modality / OCR / image jobs | 盘点已有 artifact 引用、远程节点与供应商上传，复用 owner，不另立 Blob 仓库 | P2；本轮仅发现文件/artifact 路径，未完成全部 provider 审计 |
| PluginFiles | 保留小配置 8 MiB 全缓冲 API及本地流式能力；有真实跨语言大文件需求时使用相同机制 | P2 |
| execHost stdout / shell 输出 | 复用分页与 capture 文件，不改成无限流；大的 stdin/write 需明确块预算 | P2 |
| Report 小 ZIP、OAuth/settings、小普通 RPC | 明确有界即可，首批不迁移；密钥不得放进通用可下载资源 | 不改 |
| rutis / WebSocket link #1306 | 二进制编码协商、消息/队列预算和取消能力核验；上游通用优化 | 并行后续，非首批阻塞 |

## 8. 持久化、兼容性与旧数据

- 原 16 MiB 数据及 legacy migration 的 16 MiB 文件继续可读；本方案不要求缩小、删除或重编码旧文件。
- 旧 Base64/URL 图像事件继续读取。新图片来源会让旧守护进程不能解析：必须由内容/存储 owner 作显式格式版本及拒绝降级决定，再开始写新格式。
- 客户端能力协商覆盖“可上传文件”与“可提交文件引用”，不能上传成功才发现 submit 不识别。旧客户端可保留原有受限 inline 路径；不向它回填无界 Base64 以假装兼容。
- 新输入只写文件引用，避免长期双写完整 Base64。旧数据是否离线转换另开迁移设计，不在此次功能上线时重写历史。
- fork 复制归属而非字节；delete/sweep 必须计入输入图片和工具结果图片的所有者。跨节点移动、导出/导入需携带引用闭包内的文件，不能只复制 Timeline JSON。
- 导出/导入路径是否全部支持二进制附件尚待专项审计；在新引用可写入前必须完成完整 Session 导出/恢复验收，不能视为可选后续。
- 新上传完成映射若改变持久化，使用显式新 schema；验证只用临时数据库，不触碰用户存储。

## 9. Requirements raised to mechanism owners

| Owner | 问题 / 现有能力为何不够 | 请求的通用能力 | 顺序与生命周期影响 |
| --- | --- | --- | --- |
| Session files | 合法文件大于调用消息；整包 put 无法完整表达既有业务能力 | 保持业务限额的有界上传及幂等提交结果查询 | 上传完成再暴露持久引用；失败/取消不能留下可见半文件 |
| fsHost / FileStorage | 有界读已有设计，跨语言写仍是整包；本地 io::Read 不能直接跨界 | 数据形状的有界原子写入事务，复用底层临时文件实现 | staged writes 不替换目标，commit 保持锁/租约和原子替换；卸载清理 I/O |
| 输入与事件格式 owner | 图像类型只接受 Base64/URL，新来源影响持久历史 | 文件来源及版本能力约定，fork/export 的引用保留 | 先保存文件再接收输入；沿用 adopted-input / tool-result 上下文入口 |
| WebEntry | 只有资源读取，无通用上传到 owner 的现有入口 | 鉴权的 binary HTTP 入口调用同一 owner，有界请求体 | 不增加独立业务状态；Session 关闭/删除与 owner 的提交规则一致 |
| ModelProvider / ModelHost | 同步编码函数不能负责远程文件读；过早具现化让远端 host-port 再携带整包 | 引用贯穿内部请求；最终供应商节点在准备任务中读取资源 | 提交锁外 I/O；声明跨节点读取依赖；取消释放读句柄，不删除已提交文件 |
| rutis | bytes 仍整包 Base64，Binary frame 未启用 | 编码能力协商、有限块的二进制承载、字节背压/公平性 | 保留取消和错误隔离；不引入文件或 Session 概念 |

每项需要由 owner 更新自己的契约；本文不直接修改 resource-faces 的“有界页、非无限流”规则。HTTP body 流是承载方式，公共服务仍是有界操作。若改成公共 streaming 类型，必须另提机制决策。

## 10. 实施顺序与验收门槛

1. **先定 owner 契约**：确认保留 16 MiB，定义有界写入、完成幂等性和失败清理；解决 #1398 的上限缩减争议。不要先合入 11 MiB，再让所有客户端适配它。
2. **最小纵向切片**：Rust 与真实 Node 插件各上传 16 MiB 到 SessionStore，同一 owner 提交；每条消息小于限制，不建完整编码副本。此时不改图片事件。
3. **Web/CLI 图片切片**：文件上传 → 引用 submit → adopted input → 模型请求 → 断开并恢复 → 历史预览。同步完成版本、fork、delete、导出/恢复验收。
4. **接入工具结果与资源节点**：read/MCP/Modality 分别证明上下文可见和节点可达；不能只验证客户端到控制节点，把控制节点到资源节点继续留成整包 replace。
5. **按测量优化 rutis**：对比 JSON 有界块和 binary 承载的 CPU、分配量、吞吐；确有收益再改上游编码。Web 不等待它才获得有界 binary HTTP。

验收复用现有 session_files_flow、attachments_flow、read_image_flow、transport_flow、模型请求与 Web 测试：

- 16 MiB 文件在本地、Node、Web和实际连接的资源节点完整落盘；上限加一在所有绑定一致拒绝，无行/无文件残留。
- 多张合法图片总量超过 16 MiB 时上传与元数据提交成功；每张及总业务配额仍受各 owner 规则约束。
- JSON 转义最坏的长文本不会通过降低文本业务上限“修复”；新路径按字节传输，旧路径错误明确。
- 重复块、错序、内容不一致、总长度伪造、commit 回复丢失重试均不会提交不同文件或重复采用输入。
- 取消、断连、代次卸载、磁盘满、Session 删除、写完未提交时崩溃：临时资源可回收，已有文件不被误删。
- 缺失/跨 Session/过期引用明确失败；图片数据不经未授权的 URL 暴露；恶意 MIME 和超大像素不绕过解码预算。
- fork/delete/sweep、冷恢复、旧 inline 图片、完整导出/导入和旧版本拒绝新格式均有端到端覆盖。
- 验证远端 ModelHost：控制节点到模型节点的请求保持引用，模型节点通过声明依赖获取文件；供应商替身验证最终收到正确图片，而非仅验证上传成功。
- 测量 1/16 MiB 与多并发传输：传输队列和 staging 内存随 chunk×窗口增长，而非所有文件大小之和；模型解码内存独立报告。
- 上传期间 stop/cancel 控制消息保持响应；慢接收端不造成无限队列。公平性/窗口数由测量定值，不靠提高全局消息上限。

## 11. Rule check 与消融审查

| 规则 | 本提案的处理 |
| --- | --- |
| §1 Capability / ADR 002 D1 | 沿用 Attachments、模型准备和文件 owner；不添加无状态转发插件。上传暂存是 owner 的 I/O 资源 |
| §4.1–2 指令与交接 | 上传不直接改分支状态；输入采用仍经 Session/branch 入口和现有事实；提交文件不能被 UI 撤销当作未提交 |
| §4.3 / 机制业务边界 | before 内无上传、解码或远程 I/O；rutis 不认识图片/Session |
| §4.4–5 / 原子性 | 文件发布与所有权沿用 owner 事务；Timeline 批次/Dispatcher 不增功能特判 |
| §4.6 / 上下文 | 只解析已采用输入和工具结果的文件引用；没有额外注入路径 |
| §4.7–9 / ADR 002 D2–D3 | 复用底层文件与公开 owner 契约；跨模块缺口在 §9；不读取另一插件的私有状态，不引入装配适配插件 |
| ADR 002 D4 | 传输能力与结构限制不伪装成插件业务配置；文件业务限制归 owner |
| ADR 002 D5 / 服务面 | 公共参数/结果是数据和 ID；本地 io::Read 不宣称跨语言，未实现的 API 明确是提案 |

删除不必要部分：不增加全局 Blob 插件、图片专用 socket、跨节点路径别名、客户端各自的上传业务状态机、Base64 双写或无消费者的通用对象仓库。
保留的状态只有暂存写入资源、有限在途块和恢复 commit 结果所需记录；其目的分别是原子性、背压和幂等性。
文件内容引用与运行期传输 ID 不合并：前者必须跨恢复/fork 生存，后者必须能在断连/卸载时释放。

## 12. 待确认的决策

建议先确认“保留原文件能力，先做 owner 的有界传输；Web 用同一 owner 的 binary HTTP；图片持久引用分阶段上线”。
随后由相关 owner 具体确认上传操作命名、完成映射保留期、协议/存储新版本与图片派生策略。
本研究不批准 #1398 的 11 MiB 缩减，不声称新接口已经实现，也不要求在首批同时重写所有小字节接口。
