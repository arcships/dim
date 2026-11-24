# 客户端 UI 扩展

Status: Ready.

范围：App Server 的 UI 内容目录、Session 的展示资源、客户端视图中的面板实例。
问题：[#1136](https://github.com/arcships/dim/issues/1136)。
规则：[插件契约](plugin-contract.md)、[rutis 使用](rutis-usage.md)、[设计原则](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)。

本文定义 UI 扩展的目标契约和实现边界。公开概念与行为在正文，具体 manifest、客户端接口和传输拼写在附录。是否已经提供这些接口，以 issue 的交付记录为准。

## 1. 目标与取舍

插件可以给支持网页渲染的客户端提供一个面板，不修改客户端源码。插件自行选择前端技术栈，交付同一份浏览器模块和资源；Web 与 Desktop 各自实现面板宿主，复用插件产物。首个位置是当前会话旁的面板，窄屏由客户端改用覆盖面板。

选择浏览器模块与标准 DOM 挂载边界。宿主给每个实例独立的 ShadowRoot，插件在其中挂载自己的界面，返回清理函数。Web Components 是可用的实现方式，不要求插件向全局 CustomElementRegistry 注册一个固定标签。框架应用也可以直接挂载到容器中。

| 选择 | 理由与代价 |
| --- | --- |
| DOM 挂载，不共享框架组件 | 宿主升级 Vue、React 或组件库不改变插件接口；插件须自行处理内部组件、弹层和可访问性 |
| 插件自行打包全部运行依赖 | 不增加共享依赖解析器、import map、模块联邦或宿主框架导出；接受不同插件重复携带框架代码的体积成本 |
| 固定的客户端公开能力，加上声明过的 Session 方法 | 复用业务 owner 的协议；不暴露内部 store、任意服务定位器或任意 RPC/IPC 转发 |
| 一个面板位置 | 满足预览、播放器、图表等用途；不建设任意 slot、页面路由、菜单和布局组合系统 |
| 独立的资源读取 | 有字节流入口的宿主（Web 的 HTTP 路由）继续用字节流，图片与视频不经 JSON/base64 搬运；没有字节流入口的宿主按协议分页读取交互尺寸的资源（§6.1、§6.2）；两条路径都不做网络代理和实时转码 |

不新增 Agent Capability、Timeline 事件、模型上下文输入路径或插件安装系统。此扩展不属于终端 UI registry / UiLeaf（见 [TUI §3.6](plugins/Tui.md#36-ui-registry-and-ui-leaves)）；CLI 不持有网页面板或展示资源。TUI 不渲染 HTML；MCP 工具携带的文档继续遵循 [MCP Apps](appserver/mcp-apps.md)，整个 UI 进程继续遵循 [Clients](clients.md)，不由面板生命周期管理。

### 1.1 信任边界

此契约的执行主体是用户安装并启用的**可信客户端插件**。同页面模块拥有所在页面的 JavaScript 执行环境。[Shadow DOM](https://developer.mozilla.org/en-US/docs/Web/API/Web_components/Using_shadow_DOM) 用于封装 DOM 和样式，不是权限沙箱；所需能力声明用于兼容性检查与约束公开依赖，不能强制阻止插件访问页面全局对象或自行发出同源请求。

因此，契约不承诺隔离恶意插件，也不宣称未注入某个服务就撤销了页面原有权限。不得向组件上下文传递 daemon 密钥、完整传输对象、Electron preload 或 Node 能力。宿主页面本身也不能有供任意页面脚本调用的全权限 native 桥。需要隔离不可信页面的场景不在本契约内。

启用与项目来源规则遵循 [ADR 003](../adr/003-approval-scope.md)。UI 的人工操作走已有业务入口及其授权；模型决定的工具调用继续由原来的 Permission/Approval 处理。界面不能把普通展示数据直接写入模型上下文。

## 2. 职责与新增内容

| 部分 | 作用域与所拥有的状态 | 提供的能力 |
| --- | --- | --- |
| UiExtensions | App Server 进程；被选中且启用的 UI 内容注册、不可变资源版本、目录 revision | 注册/撤下 UI 内容，读取公开目录与包内资源；注册一个 Session 部分 |
| UiResources | Session；该 Session 中由后端插件发布的文件引用、打开的文件及读取任务 | 发布/撤下有限长度文件，供已授权客户端读取；Session 关闭时结束读取 |
| 面板宿主 | 每个客户端视图；选中面板、挂载实例、观察者、当前连接与视图绑定 | 校验兼容性、加载模块、提供公开能力、挂载和清理 |
| 插件 UI 模块 | 一个挂载实例；播放器、框架实例等展示状态 | 渲染容器内的内容，消费注入的接口，释放自己的资源 |

UiExtensions 是拥有内容状态的 App Server 插件，不是把错误作用域接回 Session 的接线模块。UiResources 的资源属于 Session，不能只在进程表里存一个 Session ID，再另建 Session 关闭观察系统。两者都不读写分支 Timeline，不是 Capability。两者由独立的 UiExtensions 插件装配并可停用；位于 App Server crate 不代表属于协议内核。App Server 负责装配与受保护传输，Web/Desktop 负责界面。将内容与文件 owner 放入 CLI 会让直连 daemon 的客户端依赖另一个常驻客户端，因此不这样放置。

复用 [PluginManager](plugins/PluginManager.md) 的安装、选择、启停、重载与内容分发。PluginManager 将已经验证的 UI 声明交给内容 owner，不保存客户端面板状态，不复制 UI 目录和资源读取任务。使用 [贡献注册规则](rutis-usage.md#r6-contributions-register-with-their-owner) 管理注册与撤下，不增加插件运行时、依赖图或第二套服务总线。

客户端没有 rutis loader 的镜像。它只消费已启用的内容目录，管理面板实例；这里的客户端能力是一个固定接口集合，不是浏览器中的通用依赖注入容器。

## 3. 声明、发现与兼容性

### 3.1 UI 内容声明

携带顶层 `ui` 的插件包要求支持此内容项的 Dim。旧版 manifest 读取器拒绝未知字段，会以 `invalid_manifest` 拒绝整个包，包括其中的 code、skills、MCP 和 hooks；它不会只忽略 UI。需要兼容旧版的后端应保持不含 `ui` 的包版本，或把 UI 发布为单独包。恢复 v1 安装记录不能修复新 manifest 的兼容性。

插件 manifest 增加可选的 UI 内容项。它声明契约主版本、资源根目录和面板列表。每个面板声明插件内稳定 ID、标题、可选图标、入口模块、语义位置、所需客户端能力及所用的插件 Session 方法。

- 面板的完整身份是插件名与面板 ID 的组合。安装副本 ID 不成为跨更新的面板身份；选择同名插件的另一个安装副本是替换。
- 标题有必填默认文本，可带语言到文本的映射。宿主按当前语言、基础语言、默认文本选择；缺少翻译不会隐藏入口。图标是资源根内的可选文件，不接收可执行 HTML。省略、缺失、不可读或浏览器解码失败时使用宿主通用图标，面板仍可用；非法路径或越界仍拒绝声明。目录只保留成功复制到快照的图标；同时作为入口的文件仍是必需资源。
- UI 根是专用的浏览器资源目录，可以直接放置 HTML、CSS、JS，也可以使用构建产物。根内普通资源会公开，因此不得放置后端源码、配置或凭据；`.map` 文件明确排除在快照外。入口、相对模块导入、样式和图标都在此根内，包根不自动公开。
- 运行依赖随插件产物打包。没有裸模块名解析、远端 CDN 依赖、宿主框架 external 或加载时安装步骤。
- UI 可以单独存在，不要求一个空的后端代码 row。业务后端存在时仍按原来的插件结构提供服务。
- 所需能力和方法均为必需依赖。首期不增加可选依赖与分级降级逻辑；缺失时明确说明不可用。

具体字段见附录 A。安装记录格式调整由 PluginManager 完成，见 R1；不能在现有持久化版本中直接加入旧读取器不认识的内容种类。

### 3.2 内容版本与目录

UiExtensions 公开目录只包含当前被选择并启用的内容，不包含本机绝对路径。一次内容注册拥有一个不复用的 generation 标识，包括声明和该次资源文件集；目录 revision 在新增、替换、撤下时变化。它们都是运行期身份，进程重启后失效，不作为持久化业务 ID。

注册时将专用 UI 根制作成不可变内容快照，完整验证后才发布目录。一个 generation 的入口、懒加载模块和样式必须读到同一快照；本地开发目录的后续修改直到显式重载才生效。建立快照和复制文件使用流式 I/O，失败不发布半套资源。

客户端连接后读取完整目录，并消费目录失效通知。通知只携带 revision，客户端重读目录；连续变化可以合并。订阅建立在第一次目录读取之前，读期间收到更晚 revision 就再读，避免遗漏。断线重连重新读取，不把旧通知当成可重放日志。传输要求见 R3。

目录的 generation 属于 UI 内容 owner，不等于 rutis 后端代码 generation：

- 禁用、移除、选择其他安装副本及显式刷新内容会撤下旧 UI generation。
- UI 声明解析失败时，由 PluginManager 报告内容诊断并撤下旧注册；已验证声明的快照构建失败，由 UiExtensions 在目录中报告内容错误。不能静默继续展示已被替换的版本，各 owner 只保留自己产生的错误。
- 后端代码重启不自动证明内容变化。面板所需方法不可用时，面板按 §5 进入不可用状态；UI-only 内容不依赖后端 row 是否 Active。
- 后端方法的准入和已接受调用仍使用 [Session 方法契约](contracts/session-boundary.md#a1-method-contributions)。UI generation 不包装成另一层业务请求 generation，不改变原来的调用结算。

### 3.3 支持范围

客户端面板宿主持有其支持的契约主版本、语义位置和公开能力名称，与目录声明在本地比较，不增加服务端协商会话。目录发现本身不执行模块；仅在版本、位置、所需能力和方法都满足后才加载所选面板。

不支持的面板在扩展入口列表显示原因，后端功能仍可用。没有网页承载能力的客户端可完全不提供 UI 入口。Desktop 支持同一浏览器绑定后加载同一份产物，布局可以不同。

兼容性版本只描述 UI 契约，不是 App Server 协议版本或业务插件版本。版本 1 的声明字段集固定；使旧读取器无法接受的字段、能力或生命周期变更使用新的契约主版本。业务方法的参数和结果由该方法 owner 维护，UI 声明不复制业务 schema；不兼容业务操作使用新方法名，不能假设所有客户端会与后端同步刷新。

## 4. 面板与公开服务

### 4.1 挂载边界

宿主提供空的 ShadowRoot、绑定好的上下文和结束信号，调用模块唯一的挂载入口。模块同步返回清理函数，异步加载由实例自己启动并观察结束信号；模块顶层只定义代码，不注册全局副作用。挂载抛错前，模块负责释放已经取得的资源。

宿主销毁时先令上下文失效并触发结束信号，再调用清理函数，最后移除容器。清理覆盖框架卸载、订阅、计时器、播放器及实例发起的网络读取。一个实例的清理失败由宿主报告，不能阻止其他实例清理。

全局 [CustomElementRegistry](https://developer.mozilla.org/en-US/docs/Web/API/CustomElementRegistry) 的定义不能随面板销毁撤销。插件可以使用自定义元素，但不能要求宿主卸载或覆盖全局标签；不能把全局标签名当作可热替换入口。首个示例直接使用 DOM 挂载，保证更新无需依赖 scoped registry 的浏览器支持。

宿主只提供有版本的少量语义 CSS 变量：背景、前景、弱化文本、边框、强调色与字体。插件不读取宿主 DOM 结构或内部 CSS 类名，弹层也留在自己的根中。主题与语言变化通过上下文观察者通知，不要求重建框架实例。

### 4.2 上下文与数据访问

一个实例固定绑定创建时的 Session 和 branch。业务调用不允许传入另一个目标来改变绑定。切换任一身份销毁实例并重新挂载；不能把可变的“当前 Session”对象交给尚未结束的异步操作。

公开接口只包含以下集合；精确拼写见附录 B：

| 集合 | 行为 | 依赖 |
| --- | --- | --- |
| 视图上下文 | 当前绑定、主题、语言及其变化；结束信号 | 客户端视图自身 |
| Session 状态 | 已同步状态快照与观察；区分当前分支工作状态和整个 Session 是否有分支在工作 | 已有 Session attach/state 协议与客户端投影 |
| 分支历史读取 | 有界读取，返回既有公开历史类型；不返回宿主渲染树或组件状态 | 已有 history 协议 |
| 用户输入与中断 | 向当前绑定提交既有用户输入或请求中断，返回原协议结果 | 既有输入和中断入口 |
| 声明过的插件方法 | 每个声明名称对应一个固定的可调用函数 | 该 Session 的 `session.methods` 与 `sessionMethods` |
| 展示资源定位 | 将本 Session 的资源引用转换为客户端可加载地址；不同 Session 的引用以 `invalid_binding` 拒绝 | UiResources 与客户端传输适配 |

既有数据类型只保留一份定义；客户端类型声明从公共协议复用或生成，不复制 Pinia 类型，也不为每个面板重新设计 DTO。依赖既有接口不表示可以导出整个 RpcClient。浏览器公开声明必须能以类型依赖使用，不能导入 Node/rutis 运行时代码。

整个 Session 的工作状态按已同步的所有未结束分支状态计算：任一分支 running 或 stopping，Session 仍在工作；当前分支的两个状态字段按公开协议原样提供。不能用会话目录里的主分支 `running` 冒充整个 Session。这个聚合是客户端展示投影，不新增服务器业务状态。初次连接或重连未同步完时状态不可用，不能补成 idle。状态观察只服务当前展示，不承诺回放断线期间每一次状态变化；重连后交付当前快照。播放器可以根据该快照暂停，不借此调度后端业务。

### 4.3 有限的方法绑定

方法声明是显式的公开接口依赖，可以由本包或其他已启用插件提供，不按包所有权划分权限。宿主根据面板声明及当前 Session 的方法发现结果，构造只包含这些名称的函数表。调用时由宿主填入固定的 Session 身份；声明为 branch 操作时再填入固定 branch 身份。插件参数不能覆盖这两个字段，冲突在调用边界拒绝。是否为 branch 操作在 UI 声明中明确，与业务方法契约一致。

函数表不接受待转发的 method/channel/service 字符串。没有通配符、管理方法绑定、任意 IPC 或后端服务目录浏览。声明一个方法不能使原本不对该连接开放的方法变得可调用；每次调用仍经过 App Server 的现有入口授权。依赖发现发生在挂载和显式重试时，运行中的 provider 不可用由调用错误暴露，不增加实时方法依赖图。

业务参数与返回值在原有外部协议边界验证。插件前后端共享自己方法的类型，宿主只负责绑定和传输，不解释业务字段或再校验一遍业务状态。原始业务错误保持原码；展示层的失效错误另见 §7。

方法集合属于接口兼容性边界，不是同页面脚本的安全限制。旧视图在收到内容失效通知之前可能调用当前后端 provider；业务接口必须遵守 §3.3 的兼容性规则。已经被旧 provider 接受的调用按原 Session 方法契约收尾，不能由新 UI 实例重试。

## 5. 视图交互与生命周期

```text
Wide
+----------------+----------------------------+-----------------------+
| Sessions       | Current conversation       | Lines | Extensions    |
|                |                            | [Preview v]       [x] |
|                |                            | Plugin ShadowRoot     |
|                | Composer                   |                       |
+----------------+----------------------------+-----------------------+

Compact
+--------------------------------------------------+
| Conversation                         [Extensions] |
|                                                  |
| +----------------------------------------------+ |
| | Preview                                  [x] | |
| | Plugin ShadowRoot                            | |
| +----------------------------------------------+ |
+--------------------------------------------------+
```

每个视图同时打开一个扩展面板。入口按插件名、面板 ID 稳定排序；用户选择后才加载模块。默认展示原有会话界面，不自动打开所有插件。没有当前 Session 时，入口说明需要选择会话，不创建隐式 Session。

打开时焦点进入面板容器；关闭按钮始终属于宿主，关闭后焦点回到入口。窄屏覆盖层有可见标题、关闭按钮和焦点约束；Escape 关闭覆盖层。插件内部键盘操作不接管全局会话快捷键。加载、不可用和失败状态由宿主占据同一位置显示，不能使聊天区域消失。宽窄切换沿用客户端已有 Shell 布局模式，不增加插件专用断点。

首期不保活隐藏面板。切换回 Lines、关闭面板、离开会话页或打开另一个扩展都会销毁实例；重新打开重新挂载。只持久化客户端自己的面板选择与尺寸，不持久化 JavaScript 句柄。插件需要保留的业务状态仍由自己的业务 owner 保存。

| 情况 | 可观察结果 |
| --- | --- |
| 首次打开 | 先校验目录和依赖，再显示加载状态；成功后挂载；失败显示原因与显式重试入口 |
| 主题/语言变化 | 同一实例收到更新；标题与主题变量同步更新 |
| Session/branch 切换 | 旧上下文立即失效；旧结果不得更新新实例；新绑定同步完成后重新挂载 |
| 隐藏或关闭 | 清理实例的观察和展示 I/O；不关闭 Session，不取消业务 owner 已接受的操作 |
| 目录替换/撤下 | 撤下旧实例和旧入口；同一稳定身份仍存在且兼容时，当前打开位置重新挂载新 generation |
| 调用发现必需 provider 已卸载/不可用 | 释放实例并显示不可用；用户重试时重新发现依赖，不自动重放调用；普通业务拒绝或参数错误由插件在现有面板展示 |
| UI 模块导入/挂载失败 | 该面板显示失败；其他面板和 Session 继续运行；模块顶层副作用不在可恢复保证内 |
| 连接断开或认证失效 | 销毁实例并停止媒体读取，宿主显示断线状态；重连重新发现目录、绑定和快照，再挂载 |
| Session 关闭或分支结束 | 销毁绑定实例，显示目标不可用；不自动改用 main 或其他 Session |
| 多窗口/多客户端 | 分别拥有选择与实例；共享业务状态来自原 owner；一个窗口关闭不清理另一个窗口的实例 |

模块导入可能在面板已经关闭后才完成。宿主在导入完成时再次核对实例是否仍有效；失效时不调用挂载入口。ES module 在页面中的缓存和已定义代码不能真正卸载，重新加载使用新的内容地址；逻辑卸载仅保证契约内的资源清理。开发期反复更新后可刷新客户端页面释放模块缓存，不增加热更新运行时。

## 6. 静态与动态资源

### 6.1 包内资源

UI 资源读取需要当前服务入口的身份认证，不能成为匿名静态站点。资源身份由内容 generation 和根内相对路径组成，传输适配将其转换为 URL；目录及插件界面都看不到宿主绝对路径。

读取只允许快照内的普通文件。路径解码和校验在入口进行，拒绝绝对路径、父目录跳转、编码绕过、目录列表和越界符号链接。实际打开也必须保持根约束，不能只做一次字符串前缀检查。不存在的资源返回资源错误，不能回落到宿主 SPA HTML。

类型由资源 owner 给出固定的 MIME 映射；模块使用 JavaScript MIME，带 `nosniff`。不认识的类型作为下载数据处理，不猜成可执行文档。缓存使用内容版本与 ETag，但受保护响应必须在复用前重新验证授权，采用 `private, no-cache`；撤下后的网络读取不再返回旧内容。已经被客户端读取或执行的可信代码无法通过撤下收回。

撤下内容时停止新读取，取消该版本在途读取并等待其释放，再删除快照。新版本使用新地址，不覆盖正在读取的版本文件。

Web 使用当前已授权服务入口下的资源地址与原生动态 import，相对模块、CSS 和媒体地址也留在同一版本的资源根。模块不能要求额外 localhost 端口或扩大的 CSP。Desktop 的受控 scheme/资源入口必须支持同样的相对地址、模块 MIME 和版本语义，不能把插件放到有全权限 native 桥的页面里。

没有字节流入口的宿主（Desktop、TUI 等非 Web 客户端）通过 `ui.extensions.read` 分页读取：请求 `{generation, path, offset?, limit?}`，返回 `{offset, bytes, total, mediaType, data}`，`data` 为标准 base64。默认页 1 MiB，最大 4 MiB，更大的 `limit` 截断为 4 MiB，`limit: 0` 为 `invalid_params`。资源不可变、`total` 固定：客户端在 `offset + bytes == total` 时读完，不需要额外的空页；`offset == total` 返回空页，`offset > total` 为 `invalid_params`（不截断，避免掩盖客户端错误）。线上开销约为原字节的 1.33 倍（base64 与 JSON）；一页占用该连接出站队列一帧的时间，页越大，其他回复与通知在该连接上被推迟得越久，4 MiB 的最大页限制了这一推迟，所以交互尺寸的模块、CSS、图片与短音频可接受；有字节流入口的宿主（Web 的 HTTP 路由）继续用字节流，分页不是大视频等大文件的通用传输。错误语义见 §7。

### 6.2 Session 文件资源

UiResources 提供通用的**已完成文件发布**：受信任的后端 Session part 把自己决定可以向该 Session 客户端展示的普通文件交给资源 owner，取得不含路径的资源引用，作为其业务方法结果的一部分返回。UI 根据该引用设置图片或视频地址。

- 发布只接受 App Server 所在文件系统可读的有限长度普通文件，不接受目录、设备、socket 或网络 URL。远端浏览器不需要访问该文件系统；不共享文件系统的后端执行节点不是首期文件发布来源。
- 发布参数 `path` 是 App Server 文件系统上的 canonical 绝对路径，且须位于 FileStorage 的既有授权根内。相对路径以 `invalid_params` 拒绝；绝对路径没有匹配的授权根、越界或指向链接时以 `resource_unavailable` 拒绝。UiResources 在可信后端发布边界解析到根相对路径，不因 UI 请求扩大授权。所需的原生文件读取能力见 R5。
- 发布调用是后端命名服务，不是浏览器 Session RPC。浏览器没有“路径换 URL”、任意文件读取、上传或 URL 代理方法。业务 owner 在自己的外部输入边界决定哪些文件可以发布，不能直接把客户端传来的路径当成发布许可。
- 生产者必须在文件完成后发布，并保证发布期间字节不变。UiResources 打开文件，记录长度和 MIME，持有读取资源；它不复制业务缓存、不接管下载或转码。替换内容要发布新引用，不能修改已发布文件。
- 引用绑定 Session、发布者的本次注册与资源 ID。它不是 bearer 凭据，也不是持久业务 ID。一个资源可被同一 Session 的多个面板读取；每个客户端仍需通过服务入口和 Session 访问授权。
- 注册属于发布者的生命周期；显式撤下、发布者卸载和 Session closing 都停止准入，取消在途读取并等待文件句柄释放。撤下完成后，生产者才可删除缓存文件。Session 未发布前不接受读取。
- UI 关闭只取消自己的读取，不撤下别的面板仍可能使用的资源。资源过期后由业务方法显式取得新引用，不自动下载或重放具有副作用的操作。

第一阶段的文件读取支持 GET/HEAD；Range 只对 GET 生效：完整读取为 200，合法单区间为 206，不可满足的区间为 416 并包含总长度。支持开放尾部与 suffix range；合法多区间请求及未支持的 range unit 忽略 Range 并返回完整 200，不实现 multipart。HEAD 忽略 Range，返回完整 GET 的元数据而不读 body。动态资源使用 `private, no-store`，不转换编码后再按原文件偏移处理 Range。

动态资源只内联栅格图片、音视频、纯文本与 PDF；HTML、SVG、JavaScript 等主动内容和未支持类型以 `application/octet-stream`、`Content-Disposition: attachment` 返回，并带 `nosniff`。插件程序与打包图标只从 §6.1 的可信内容入口加载。

读取按传输背压拉取有界字节块，内存不随文件大小增长；客户端关闭连接、取消、Session closing 或资源撤下均停止文件读取。头部发出前失败返回结构化资源错误；发出后失败终止流，不能在媒体 body 后追加 JSON。

没有字节流入口的宿主通过 Session 方法 `session.ui.resources.read` 分页读取已发布资源：请求 `{sessionId, id, offset?, limit?}`，返回与 §6.1 相同的页形状和边界（标准 base64、1 MiB 默认、4 MiB 上限、`offset + bytes == total` 结束、`offset > total` 为 `invalid_params`）。它是普通 Session 方法：由 UiResources part 注册，经 `session.methods` 发现，并按 Session 方法的规则服务到各入口。按 ADR 021 D3，客户端读取要求活动 Session：资源 id 只在发布它的那个 Session 实例内有效，Session 卸载或 daemon 重启后，之前的每个 id 都回答 `resource_gone`；读取随 Session 路由进入，不为回答旧 id 跳过 Session。撤下与在途页竞争时，客户端收到整页或 `resource_gone`，绝不会收到半页；Session closing 与发布者卸载同样停止新读取并取消在途读取。

Rust 和 TypeScript Session part 均可使用此服务。路径由已持有文件位置的业务 owner 提供，例如后端插件使用 Node 文件 I/O 在已授权工作目录生成文件，或由现有业务服务返回绝对路径。`pluginFiles` 只提供相对文本文件接口，不提供绝对路径，因此它的相对路径不能直接传给发布服务，也不能通过猜测产品 home 拼接。

[sessionFiles](appserver/session-files.md) 负责持久化不可变文件及 Session 所有权记录、fork 和删除；UiResources 只持有已有文件的临时展示引用与读取句柄，不写入该存储、不复制文件、不增加持久化所有权行。关闭展示引用不等于删除 Session 文件，故不把传输生命周期加入 sessionFiles。

此服务不复用文本型 [pluginFiles](process-typescript.md#52-public-pluginfiles-binding) 的编码或事务格式。图片生成、网络抓取、媒体解密、转码与缓存策略属于发布资源的业务插件。

## 7. 错误与恢复

| 条件 | 所属层与可观察结果 | 恢复方式 |
| --- | --- | --- |
| UI 声明非法、版本不支持、资源根/入口不合法 | UiExtensions：目录 `errors` 中的 `pluginName` 与诊断 `message`，不提供独立错误码 | 修正声明后显式重载 |
| 位置、所需客户端能力不支持或业务方法缺失 | 客户端入口检查：展示包含具体位置、能力或方法名的不可用原因，不提供独立错误码 | 不执行模块；后端恢复后显式重新检查 |
| 模块导入或挂载失败 | 客户端加载器：展示原始错误消息 | 显式重试；不得自动重放业务调用 |
| 面板绑定已销毁 | 浏览器绑定：`PanelError.code = binding_closed` | 旧接口拒绝新调用；只能通过新实例操作 |
| 覆盖绑定目标，或资源引用属于其他 Session / ID 非法 | 浏览器绑定：`PanelError.code = invalid_binding` | 使用当前绑定及其资源引用 |
| 创建绑定时遇到未知客户端能力 | 浏览器绑定：`PanelError.code = missing_capability` | 修正所需能力声明 |
| 发布路径不是绝对路径，或撤下其他 part 的引用 | UiResources：`ResourceError.code = invalid_params` | 修正调用参数或使用发布者撤下 |
| Session / 资源 owner 已结束，或引用所属 Session 不匹配 | UiResources：`ResourceError.code = resource_gone` | 使用活动 Session 的新引用 |
| 发布者关闭导致清理注册失败 | UiResources：`ResourceError.code = resource_closed` | 由新的活动发布者重新发布 |
| 页读取的路径非法、`limit: 0` 或 `offset > total` | UiExtensions：`invalid_params`（-32602） | 修正参数 |
| 页读取的 generation 未知或已替换、路径不在该快照内 | UiExtensions：`resource_gone` | 重新读取目录后取新 generation |
| 页读取的文件 I/O 失败 | UiExtensions：`resource_unavailable` | 生产者修复资源；不返回空页成功 |
| 页读取时 Session 已关闭、不可用，或引用不属于该 Session | Rpc 的 Session 路由先回答（如 `session_closed`）；UiResources 对不属于本 Session 的引用回答 `resource_gone` | 使用活动 Session 的新引用 |
| 文件定位、打开或撤下清理失败 | UiResources：`ResourceError.code = resource_unavailable` | 生产者修复资源；不返回空文件成功结果 |
| 内容 generation、文件或动态引用不可用 | WebEntry：HTTP 404，响应 `error.code = resource_gone` | 重新发现内容或取得新引用 |
| 已开始的 HTTP 资源读取失败或取消 | WebEntry：响应流中断；已发送响应头后不追加 JSON 错误码 | 由展示方处理加载失败，不自动重放业务操作 |

绑定错误的公开类型是 `dimcode/ui` 的 `PanelError`，其 `name` 为 `PanelError`，`code` 是上述三个浏览器绑定码之一。RPC 失败保留原来的错误对象：数值 `code` 是 JSON-RPC 错误码，`appError` 是服务端业务错误码；不能把它与 `PanelError.code` 混用。UiResources 的后端服务错误保留生成绑定中的 `ResourceError` 形状。诊断消息供人阅读，不作为稳定分支条件。资源类型的内联/下载选择遵循 §6，不产生类型拒绝错误。

认证失败、Session 授权失败、Session 已关闭以及业务方法自身错误保留原契约语义，不改写成插件加载错误。资源入口先认证和授权，再解析资源存在性，避免跨 Session 探测。未认证为 401，无权访问为 403，授权范围内未知引用为 404，已知撤下引用可以统一为 404；不为区分 404/410 长期保存墓碑。§6 的 Range 错误是 416。

失效回调、超时和断线只说明当前视图不再接收结果，不能证明业务没有发生。宿主不自动重试提交、中断或插件方法。重连只重建目录、只读快照和订阅。

## 8. 依赖与 Requirements raised to mechanism owners

| 部分 | 声明的服务依赖 | 用途 |
| --- | --- | --- |
| UiExtensions | `managementMethods`、`partRegistry`、`fileStorage` | 目录方法、Session 部分注册、内容快照与文件读取 |
| UiResources | `sessionIdentity`、`sessionLifecycle`、`fileStorage` | 固定 Session 身份、发布与关闭边界、文件读取资源 |
| PluginManager | 对 `uiExtensions` 的可选读取 | 与已有内容 owner 一样提交或撤下内容；owner 缺失不阻塞后端代码 |

具体装配按 [rutis 使用](rutis-usage.md) 声明，贡献使用既有调用者生命周期。面板宿主依赖现有 Session 公开协议，不引入后端任意命名服务访问。没有对 rutis、Timeline 或 Dispatcher 的需求。

### R1. PluginManager：UI 内容与安装记录格式

- **阻塞与规则：** §3 的 UI-only 包没有现有 manifest 内容项；[PluginManager §2](plugins/PluginManager.md#2-data) 的安装记录持久化内容种类，旧读取器不认识新种类。
- **考虑的机制：** 给每个 UI-only 包增加空代码 row 会违背内容归 owner 的规则；另建 UI 安装记录会重复安装和选择状态。
- **所需通用能力：** 在原 manifest 增加 UI 项，按原内容分发操作注册/撤下；内容 owner 缺失只使 UI 不可用。安装记录显式升级为 v2，读取 v1 时无损转换，保留所有安装项、选择和 off 状态；首次写 v2 前保留原始 v1 备份，原子写入成功后才使用新格式，备份或写入失败保留原记录并报错。未知版本失败且不覆盖。旧程序拒绝 v2，回退前需恢复备份，不能把新种类写进 v1。
- **时序影响：** 重载、更新、禁用和移除撤下旧内容；新内容完整准备后才发布。失败不留旧 UI 可用的假象，不撤销后端已经接受的操作。无 Timeline 影响。
- **Owner / issue：** PluginManager，[#1136](https://github.com/arcships/dim/issues/1136)；实现时由 owner 更新其设计及格式验证。

### R2. WebEntry 与 Desktop 资源适配：受保护的字节流

- **阻塞与规则：** §6 需要受保护包资源和有限文件流；[Web workspace §2](web-workspace.md#2-local-web-entry) 的宿主静态文件与 SPA 入口不是插件资源权限和媒体读取契约。
- **考虑的机制：** 现有 JSON RPC 不是大文件的字节流，所以 Web 宿主保留 HTTP 路由，分页读取只服务没有字节流入口的宿主（§6.1）；文本型 pluginFiles 不是媒体服务；独立端口破坏当前服务入口的身份与远端访问。
- **所需通用能力：** 资源适配读取 UiExtensions/UiResources 的已准入资源，保持身份、Session 授权、根限制、MIME、缓存、Range、背压和取消语义。没有字节流入口的宿主改由 App Server 协议的 `ui.extensions.read` 分页读取同一资源（§6.1、附录 C）；WebEntry 继续拥有 Web 宿主的字节流入口。Web 只增加明确路径，保留现有 Host/Origin/CSP 规则；读请求校验 Host，存在 Origin 时校验，拒绝显式跨站资源请求，不要求所有同源图片/媒体 GET 都携带 Origin。不增加 CORS 放行，不把全体管理方法开放给 Web。
- **时序影响：** 每次读取在 owner 准入时绑定版本；撤下停止新读取并结算旧读取。取消传输不取消后端业务。无 Timeline 影响。
- **Owner / issue：** WebEntry，[#1136](https://github.com/arcships/dim/issues/1136)；Desktop 适配由其仓库承接，[#437](https://github.com/arcships/dim/issues/437)。

### R3. App Server 客户端协议：UI 目录读取与失效通知

- **阻塞与规则：** §3.2 的客户端必须知道启用内容变化；[Session 方法边界](contracts/session-boundary.md#a1-method-contributions) 的方法发现不包含 UI 内容，现有管理进度消息也不是可靠目录通知。
- **考虑的机制：** 每个插件建立自己的 WebSocket、在 Timeline 写 UI 变化、轮询完整 plugin.list 都扩大边界；复用 plugin.list 会向组件泄露与展示无关的管理信息。
- **所需通用能力：** 增加精确的只读 UI 目录方法与进程目录失效通知，读取直接来自内容 owner。Connection 使用已有控制消息通道承接 owner 的变化；不能使用允许丢弃的 progress 通道。无法可靠交付时断开，让客户端完整重同步。不扩展为任意插件事件总线。
- **时序影响：** 通知只在目录修改生效后发出；建立订阅后读取快照，revision 防止读期间遗漏。连接重建重新订阅。无持久化和 Timeline 影响。
- **Owner / issue：** Rpc/Connection，[#1136](https://github.com/arcships/dim/issues/1136)。

### R4. 客户端公开协议类型与绑定

- **阻塞与规则：** §4.2 要求框架无关类型；现有 Web 内部 store 不能成为插件契约，Node 插件 SDK 也不应作为浏览器运行时。
- **考虑的机制：** 每个插件复制 wire 类型会产生多个定义；导出宿主 store、组件库或完整客户端会穿透所有权边界。
- **所需通用能力：** 公共协议 owner 提供可复用的浏览器类型声明；UI 绑定只引用这些声明。`dimcode/ui` 是类型入口，声明文件内部引用同包的 `client-protocol.d.ts`，不引入运行依赖、服务解析器或共享框架。现有业务类型由原定义生成或导出，插件自己的方法类型由插件自己提供。
- **时序影响：** 仅有类型与客户端调用边界，不改变业务准入、中断或重放。
- **Owner / issue：** 公共协议与 npm 类型发布 owner，[#1136](https://github.com/arcships/dim/issues/1136)。

### R5. FileStorage：受授权根约束的原生只读流

- **阻塞与规则：** §3.2 和 §6 需要有界复制与按字节区间读取；[FileStorage](../architecture/rust-agent/plugins/FileStorage.md) 的按路径偏移读满足分页（`read` 的 offset/length），不提供文件句柄或背压流；撤下与在途页竞争时只返回整页或 `resource_gone`。
- **考虑的机制：** 整块读取使内存随媒体大小增长；在 UI owner 中重写根约束和安全打开逻辑会复制文件边界；文本事务不能表达二进制流。
- **所需通用能力：** FileStorage 的 `stat` 返回只读文件长度，`read(path, offset, length)` 由调用方自己携带偏移分页读取；保留原有防越界打开、取消、generation 准入与收尾保证，不由读取请求扩大根授权。没有读取资源或文件句柄跨出所有者。
- **时序影响：** UiResources 保证已发布路径在撤下前不被改写，因此同一引用的分页来自同一版本；取消或 owner 关闭停止读取。无 Timeline、格式或写事务变更。
- **Owner / issue：** FileStorage，[#1136](https://github.com/arcships/dim/issues/1136)；实现与根约束、关闭流程验证在 FileStorage 内完成。

## 9. 实施顺序与波及面

| 单元 | 新增或调整 | 完成边界 |
| --- | --- | --- |
| 契约与类型 | UI manifest、目录、上下文、错误和浏览器类型入口 | 示例不导入宿主框架或 Node 运行时；所有接口在实现前有明确 owner |
| 内容 owner 与安装 | App Server 中新增 UiExtensions；复用注册表与文件 I/O；PluginManager 的 UI 内容分发、v2 安装记录迁移 | UI-only 包可安装、启停、重载；旧记录和失败恢复可验证 |
| 目录与静态资源 | 精确的目录协议、通知和受保护资源入口；不可变内容快照 | 多客户端能发现同一内容并独立更新；未认证、越界和旧版本读取失败 |
| Web 面板宿主 | 在 `web/` 增加加载/清理、固定公开接口和一个面板位置 | 安装即可出现入口；切换、断线、失败和重载不影响会话 |
| Session 文件资源 | FileStorage 原生只读流、UiResources 的发布/撤下与读取、Web 的流式资源响应 | 大文件、seek、背压、取消和关闭时句柄释放成立 |
| 示例与真实场景 | 一个零构建的 HTML + CSS + JS 示例，演示挂载、公开数据访问与清理 | 同一套契约覆盖多种渲染技术，业务逻辑不进入核心 |
| Desktop | 在其仓库实现相同浏览器绑定、公开能力和资源适配 | 同一个插件包和构建产物无需重建；没有能力时明确不可用 |

实际实现位置集中在 `crates/dim-app-server/`、`web/`、`npm/` 及相关传输流程测试；FileStorage 在 `crates/dim-agent-capabilities/` 内补充 R5 的 I/O 能力。UI 原型不要求调整 AgentLoop、Timeline、Dispatcher、事件 fixture 或 native Session 存储格式。新增服务和装配时才更新对应实现架构页，并按仓库规则生成 catalog/graph/types；设计阶段不手写当前架构图，也不修改生成文件。

成本主要在资源入口的授权与流式取消、内容版本一致性、实例清理及安装记录迁移。播放器的下载、转码和缓存由插件承担。共享框架依赖解析、任意 slot、声明式 UI DSL、组件间通信总线和通用资源代理均不属于实施项。

## 10. 验收与消融检查

复用现有 PluginManager、Session 方法和 WebEntry 的公共流程测试，增加以下用户流，不按内部 helper 分拆测试：

1. UI-only 包与带业务代码的包分别完成安装、发现、打开、关闭、禁用、重载、移除；原 Session 保持可用，后端代码失败与 UI 内容失败可区分。
2. 两个不同技术栈的包在同一宿主工作，不读取宿主组件、store 或共享框架；Web 与 Desktop 使用相同产物，Desktop 验证在其仓库完成。
3. 切换 Session/branch、关闭后延迟导入、迟到响应、主题变化、多窗口、断线重连和目标关闭；没有串用上下文或旧实例继续渲染。
4. 所需能力/方法缺失、声明管理方法、覆盖绑定身份、方法中断、操作提交后断线；错误明确且不自动重放业务操作。
5. 未认证/无权访问、跨站请求、编码路径穿越、符号链接逃逸、旧 generation、原目录在懒加载前被修改；不能读到根外文件或混合版本。
6. 文件读取的 GET/HEAD、单 Range、suffix、416、多 Range 的 200、慢消费者、取消、撤下与 Session closing；读取内存有界，文件可在撤下完成后删除。通过当前授权入口的外部浏览器访问不依赖浏览器本机端口。
7. v1 安装记录迁移后保持选择、off、安装来源和目录；写入失败保留原记录；未知版本不覆盖。只使用临时 home 和可丢弃 fixture。
8. 没有字节流入口的本地客户端按 generation 分页读取包内资源：两页衔接与 `offset + bytes == total`、`offset == total` 的空页、`offset > total` 与非法路径与 `limit: 0` 的 `invalid_params`、未知或已替换 generation 和快照外路径的 `resource_gone`；Web 入口上该方法为 `unknown_method` 且不出现在其 `hello.features`，HTTP 路由照常。Session 资源按 id 分页读取：未知或已撤下 id、发布者卸载后与其他 Session 的读取为 `resource_gone`，Session 删除后为 `session_closed`，重启后的旧 id 为 `resource_gone`，撤下与在途页竞争只返回整页或 `resource_gone`。

消融结论：不增加通用前端插件容器、共享依赖机制、持久化面板状态服务或第二条 RPC 通路。UiExtensions 必须拥有内容版本和目录，UiResources 必须拥有 Session 资源与读取收尾，面板宿主必须拥有视图实例；这些状态不能分别推给 PluginManager、进程全局 Session 选择或业务组件。原型若无需某个公开能力，则从首期导出集合删除，不以占位接口保留。

## 11. Rule check

UiExtensions、UiResources 和客户端面板都不是 Capability：没有分支事实状态机，没有自有 Timeline 事件。

| 规则 | 本设计的处理 |
| --- | --- |
| Principles §4.1：其他模块的状态由 owner 决定 | UI 操作调用已有公开入口；业务 owner 决定并写自己的事实；UI 目录是进程 owner 的类型化内容操作 |
| §4.2：交接后不撤回 | 关闭、断线和重载不撤销已接受的业务调用；资源读取由其启动者取消，不代表业务回滚 |
| §4.3：before 只同步变换 | 没有 before 回调或提交锁内外部等待 |
| §4.4：核心没有插件业务含义 | 没有播放器或其他示例业务进入核心；目录与文件读取只处理公开元数据和字节 |
| §4.5：原子批次 | 无新增 Timeline 批次；目录在完整资源版本可用后发布 |
| §4.6：上下文只来自交接后的输入 | 展示不进入模型上下文；提交使用原用户输入入口 |
| §4.7：只用公开事件和服务 | 固定客户端能力、声明过的 Session 方法、资源引用；不读取其他插件内部状态 |
| §4.8：基础设施默认不变 | R1–R5 明确提出 owner 需求；不改变 Timeline、Dispatcher、rutis 或 Session/branch 门面 |
| §4.9：只有后续决定需要的事实入 Timeline | UI 目录是进程通知，视图状态在客户端，资源是 I/O 生命周期；都不写 Timeline |
| Principles §1：Capability 与 App Server 边界 | 两个服务部分均在 App Server；没有新增 Agent Capability |
| ADR 002 D1：状态决定作用域 | 目录/包资源在进程，动态资源在 Session，面板实例在客户端视图 |
| D2：没有接线模块 | 每个新增部分有自身内容或 I/O 状态；不增加空后端 row 或通用依赖容器 |
| D3：依赖随作用域 | Session 资源部分依赖本 Session 生命周期；内容 owner 不读取分支状态；前端消费公开协议 |
| D4：结构与业务配置分离 | manifest 是结构声明；安装选择由 PluginManager 保存，业务配置沿用各插件 Settings source |
| D5：语言中立 | 正文定义身份、行为、生命周期和错误；浏览器与传输拼写只在附录，后端 Rust/TypeScript 共享资源语义 |

## 附录 A：Manifest 绑定

下列是 UI 内容项；`code` 等其他内容项继续使用 [PluginManager](plugins/PluginManager.md) 的定义。

```json
{
  "name": "preview",
  "ui": {
    "version": 1,
    "root": "dist/ui",
    "panels": [
      {
        "id": "viewer",
        "title": { "default": "Preview", "translations": { "zh": "预览" } },
        "icon": "icon.svg",
        "entry": "panel.js",
        "placement": "session-panel",
        "requires": ["session.state", "resources"],
        "methods": [
          { "name": "session.preview.list", "scope": "session" },
          { "name": "session.preview.open", "scope": "branch" }
        ]
      }
    ]
  }
}
```

`version` 是必填正整数；首个版本为 1。`root` 是插件目录内的相对目录；其余文件路径均相对于该根。面板 `id` 匹配 `[a-z0-9][a-z0-9_-]{0,63}`，同一插件内唯一。标题默认文本非空；`translations` 与 `icon` 可省略，省略的含义见 §3.1。`panels` 非空；`requires`、`methods` 必填且可以是空数组，名称不得重复。未知主版本保留为不支持的 UI 内容，不执行其中模块；版本 1 的未知字段与非法路径报内容声明错误。

`methods` 只声明面板依赖的公开 Session 方法，不要求其 provider 与面板属于同一个包。内置能力使用 `requires` 中的固定名称：`session.state`、`session.history`、`session.submit`、`session.interrupt`、`resources`。上下文与结束信号始终提供。声明不能增加新的宿主能力名称。

## 附录 B：浏览器绑定

浏览器入口导出 `mount`。以下类型展示 SDK 声明文件的挂载边界。历史读取参数/结果、`UserInput` 和操作结果引用公共协议定义，不在本文件重定义。`PanelActivity` 是 §4.2 新增的展示聚合类型；其中的分支状态从公共 `session.state` 通知的载荷类型派生，公共类型出口由 R4 提供，不要求作者使用尚未发布的 SDK。

```ts
import type { StateNotice } from "./client-protocol";

export type Dispose = () => void;

export type PanelActivity = {
  readonly branch: Readonly<Pick<StateNotice, "running" | "stopping">>;
  readonly sessionRunning: boolean;
};

export interface PanelEnvironment {
  readonly sessionId: string;
  readonly branchId: string;
  readonly theme: "light" | "dark";
  readonly locale: string;
}

export interface PanelContext<Services, Methods> {
  readonly signal: AbortSignal;
  readonly environment: PanelEnvironment;
  observeEnvironment(listener: (value: PanelEnvironment) => void): Dispose;
  readonly services: Services;
  readonly methods: Methods;
}

export interface PanelModule<Services, Methods> {
  mount(root: ShadowRoot, context: PanelContext<Services, Methods>): Dispose;
}
```

`environment` 是该次挂载的初始快照；观察者注册时同步收到最新快照，再接收变化，避免读取与订阅间遗漏。身份不变，主题与语言可变。所有观察者随实例结束自动撤销，也可以提前调用返回的清理函数。

公开能力在 `services` 下使用固定成员：`session.state.observe`、`session.history.read`、`session.submit`、`session.interrupt`、`resources.url`。宿主只提供 `requires` 声明的成员；`state.observe` 立即交付最新的 `PanelActivity`，返回提前停止观察的 `Dispose`。UI 在状态同步完成前不挂载；连接丢失按 §5 销毁。`resources.url(ref)` 返回当前适配的可加载地址，不能接收本机路径或任意 URL。

宿主在根上设置 `--dim-ui-background`、`--dim-ui-foreground`、`--dim-ui-muted`、`--dim-ui-border`、`--dim-ui-accent`、`--dim-ui-font-family`，它们属于版本 1 的主题绑定。插件可以不使用这些变量，但不能依赖宿主未公开的 CSS token。

插件方法类型由插件自己的前后端契约提供，例如：

```ts
type PreviewMethods = {
  "session.preview.list": (input: ListRequest) => Promise<ListResult>;
  "session.preview.open": (input: OpenRequest) => Promise<OpenResult>;
};
```

这些参数类型不包含 `sessionId`、`branchId`。宿主按 manifest 的 scope 补入绑定身份。没有 `call(name, args)` 或 `getService(name)`。方法表是经已声明名称构造的函数集合，不加载或解析插件的运行依赖。

## 附录 C：服务与传输绑定

| 名称 | 范围 | 输入与结果 |
| --- | --- | --- |
| `uiExtensions` | process，后端命名服务 | 接受 PluginManager 的已验证内容注册/撤下；提供目录快照及变化观察，不供页面发布内容 |
| `uiResources` | Session，后端命名服务 | `publishFile({path, mediaType}) -> ResourceRef`；`withdraw(ref)` 等到读取结束；注册自动归调用者生命周期，调用者不能撤下其他注册 |
| `ui.extensions.list` | 已授权客户端的精确只读方法 | `{}` → `{revision, extensions, errors}`；extension 包含插件名、generation 和面板声明，不含 URL；errors 仅含 UiExtensions 自己产生的内容错误且不含绝对路径 |
| `ui.extensions.read` | 已授权客户端的精确只读方法（local） | `{generation, path, offset?, limit?}` → `{offset, bytes, total, mediaType, data}`；`data` 为标准 base64；默认 1 MiB、上限 4 MiB；不在 `WEB_MANAGEMENT_METHODS` |
| `session.ui.resources.read` | UiResources part 注册的 Session 方法 | `{sessionId, id, offset?, limit?}` → `{offset, bytes, total, mediaType, data}`；同一页边界；经 `session.methods` 发现；id 只在发布它的 Session 实例内有效 |
| `ui.extensions.changed` | 同一连接的控制通知 | `{revision}`；不是 Timeline 或 Session 广播事实 |
| `ResourceRef` | 公开可序列化数据 | `{sessionId, id}`；id 不复用、定位一个发布注册，不包含文件路径或认证令牌 |

客户端在 `hello.features` 中发现 `ui.extensions.list`；服务该连接的 UI 目录通知从 hello 完成前开始建立，遵循 §3.2 的读取竞态规则。旧服务器没有此能力时，客户端明确不支持扩展，不轮询插件管理接口。

Web 路径绑定为 `/api/ui/extensions/<generation>/<path>` 与 `/api/ui/sessions/<sessionId>/resources/<id>`。所有路径都经过当前 WebEntry 认证；动态资源还检查 Session 访问权限。客户端适配根据当前授权连接和目录 generation 生成模块与静态资源基地址，插件内部通过模块相对地址引用自己的静态文件。Desktop 使用自身适配生成地址，插件不得解析或拼接地址中的 host、端口、cookie 或 scheme。

UI 服务的语言绑定遵循已有的调用者生命周期注册方式。TypeScript 发布只传递文件位置与元数据，字节由 App Server 文件读取器发送；没有跨语言二进制回调协议。插件不能从浏览器调用 `publishFile`，只能从其受信任后端 Session part 调用。
