# light-ocr

需求：[插件管理](../../../design/plugin-management.md)。Issue：[#1158](https://github.com/arcships/dim/issues/1158)。作用域：[ADR 002](../../adr/002-plugin-scope.md)（进程：每个宿主一个服务）。语言：TypeScript（行）与 CommonJS（helper）。里程碑：P3，与 [Modality](Modality.md) 一起。

light-ocr 是一个第一方插件，提供 Modality 的 `local/pp-ocr` OCR 目标（[Modality §A.2.1](Modality.md#a21-ocr-目标)）。它的一个进程行提供进程级服务 `ocrHelper`，其 `describe` 方法返回目标的描述：运行该行的 Node、helper 脚本，以及 helper 必须可读的路径。Modality 每次调用时读取该描述，并为每个 OCR 任务在进程外、受隔离地启动 helper。引擎是 `@arcships/light-ocr` 的已发布 npm 包，由插件的 `package-lock.json` 精确锁定为 `0.5.9`；Dim 不构建也不托管任何 OCR bundle，RuntimeManager 中没有任何 OCR 组件。插件作为**可选**插件随 Dim 发布（[PluginManager §2.3](PluginManager.md#23-默认配置文件)）：在用户安装它之前，它未被安装也不在运行，其依赖在安装那一刻从它自己的 lock 装入用户的副本（[PluginManager §3.2](PluginManager.md#32-安装)、[RuntimeManager §5](RuntimeManager.md#5-接口)）。

## 0. 实现范围

| 部分 | M1 | 以后 |
| --- | --- | --- |
| 本仓库中的插件目录 `runtime/plugins/light-ocr/`，列在 `runtime/default-plugins.json` 的 `optional` 下（§2） | 是 | |
| 行的 `ocrHelper` 服务与 `describe`（§3） | 是 | |
| helper：argv、引擎、图像、PDF、产物、错误映射（§4、附录 A） | 是 | |
| 每个平台都使用 CPU 提供方（§5） | 是 | 在受限运行完成测量后启用 Core ML |
| 针对 fixture 引擎的测试，不下载（附录 C） | 是 | |
| 唯一一次真实受限运行（附录 C） | 验收，由人手动运行 | |

在设计的三阶段中，Phase 2 拥有 `plugin.list` 中未安装的可选项、enable/disable/reload 指名它时的 `plugin_not_found` 拒绝、`dim-cli` 解码测试与客户端的 Install 动作；插件目录、行与 helper 属于 Phase 3。

## 1. 结构

```text
Modality（Rust）                     插件行（Node）                 helper（每任务一个进程）
  ocr.recognize，目标 local/pp-ocr
  读取 ocrHelper.describe() ────────▶ ocrHelper
  HostPlatform::spawn_confined ────────────────────────────────────────▶ node helper.cjs --input … --artifacts …
        read：Node 根目录、插件目录、输入                                          │
        write：任务工作目录；无网络；services：none                                │
  解析 result.json ◀──────────────────────────────────────────────────  result.json、text.txt、page-NNNN.png
```

- 行是 `light-ocr` 插件的一个 TypeScript 进程行，像其他插件行一样由 Node 运行时加载。它提供 `ocrHelper`，不做别的：不启动引擎、不读文件、不持有状态，因此保持加载的代价很小，重载也不会丢失任何东西。
- `ocrHelper` 是由外部语言的部件提供、由 Rust 读取的名字，因此登记在宿主 loader 的目录中（[rutis 用法 §3.4](../rutis-usage.md#34-服务目录)），而不是标注在 Rust 类型上。
- helper 是插件目录中独立的 CommonJS 文件，由 `process.execPath`——运行该行的 Node——运行，从不在行自身的进程中运行。每个任务一个 helper 进程；引擎在开始时创建，退出前关闭。
- Modality 中没有任何地方点名引擎：它发布 helper 写出的产物，helper 的契约就是 Modality 已为 `local/apple-vision` 定义的那一个（Modality §A.2）。

## 2. 插件包

`runtime/plugins/light-ocr/` 通过默认配置文件的 `optional` 映射随发布提供：安装时该目录被复制到 `<home>/plugins/light-ocr/<version>+<suffix>`，依赖也在那里安装。映射的值相对于 `runtime/default-plugins.json`，因此条目写作 `"light-ocr": "plugins/light-ocr"`。

```text
runtime/plugins/light-ocr/
  dim-plugin.json       name、version、code（entry index.ts、provides ocrHelper）、dependencies "npm"
  index.ts              行：提供 ocrHelper；describe()
  helper.cjs            OCR helper（附录 A）
  package.json          私有包；依赖见下
  package-lock.json     信任锚
```

- `dim-plugin.json` 声明 `"dependencies": "npm"` 与 `"version"`；两个成员都归 PluginManager（[PluginManager §2.1](PluginManager.md#21-插件目录及其清单)）。运行的是副本，从不包含随发行的目录。
- `package.json` 声明两个依赖：精确的 `@arcships/light-ocr` `0.5.9`，以及与 `runtime/package-lock.json` 相同版本的 `@arcships/rutis`，因为默认插件或已安装插件从自己的 `node_modules` 解析 `@arcships/rutis`（[RuntimeManager §6](RuntimeManager.md#6-发布)）。
- `package-lock.json` 用 npm `integrity` 锁定每个包，包括 facade 自身的锁定——runtime `0.1.9`、模型 `light-ocr-model-ppocrv6-small` `0.3.4` 与各平台包——因为 facade 把它们声明为自己的依赖。决定运行什么的是 lock，而不是版本范围，正如 `runtime/package-lock.json` 对运行时包的作用。
- 升级 light-ocr 是一次 Dim 改动：本插件中的新 lock，像任何依赖变更一样评审。新发行带有新的 `version`：下次启动时已安装副本在后台重新获取，只有新副本就绪后才切换记录，旧副本在此之前继续服务（[PluginManager §3.1](PluginManager.md#31-启动)）。同一条路径通过 `plugin.update` 修复失败的重新获取；`@arcships/rutis` 与运行时不再匹配的插件也以同样方式被替换。
- 随发行的目录不带 `node_modules`，也没有构建产物：`scripts/stage-plugins.mjs` 复制它但不运行 `npm ci`（[RuntimeManager §6](RuntimeManager.md#6-发布)）。安装时不运行任何包的 lifecycle 脚本，因为安装使用 `--ignore-scripts`。
- 发行包既不携带 Node，也不携带任何引擎包：只有客户端的 `plugin.install`、`plugin.enable` 或 `plugin.update`——或已安装副本的后台重新获取——打开插件时，才经 RuntimeManager 获取 Node（[PluginManager §2.5](PluginManager.md#25-运行时)、[RuntimeManager §3.2](RuntimeManager.md#32-获取)）；引擎包只经 `npm ci` 进入用户的副本（[RuntimeManager §6](RuntimeManager.md#6-发布)）。从不安装该插件的用户不会为它下载任何东西。

### 2.1 客户端与旧安装

Dim 只从插件的 `ocrHelper` 读取 PP-OCR 目标。它从不读取客户端自带的运行时根目录，例如 Desktop 的 feature-runtime 安装，也不为其保留兼容路径。

Desktop 迁移到 `plugin.install {source: {optional: "light-ocr"}}`，启用该插件，并废弃自己的 light-ocr feature-runtime 条目。它通过 `modality.capabilities` 列出 `local/pp-ocr` 来检查结果，而不是通过 CLI 调用；设计中的客户端 CLI 是 `dimcode ocr <file> [--out PATH] [--json] [--no-wait]`，见 [Modality §A.5](Modality.md#a5-cli)，一个提交命令。

只有旧安装的用户会看到 `local/pp-ocr` 以 `runtime` 原因不可用，直到插件被安装。客户端可以在升级后的首次启动时安装它。没有就地迁移：旧目录由客户端自行删除。

## 3. 行与 `ocrHelper`

服务 `ocrHelper`，作用域为进程，无 key，由 `light-ocr` 行的 `provides` 提供。一个方法：

| 方法 | 类型 | 请求 | 回复 |
| --- | --- | --- | --- |
| `describe` | 异步 | `{}` | `{contract: 1, target: {providerId: "local", modelId: "pp-ocr"}, executable, args, read}` 或 `null` |

- `contract` 是本插件实现的 helper 契约版本，`1`。仅当它与 Modality 实现的版本相等时 Modality 才提供该目标；仍在运行的旧副本使目标不可用并带“the PP-OCR plugin needs an update”，直到重新获取完成（Modality §A.2.1）。
- `target` 是 `{providerId: "local", modelId: "pp-ocr"}`，因此 Modality 把该目标列为 `local/pp-ocr`。
- `executable` 是 `process.execPath`：运行该行的 Node 安装。`source: system` 时它是 `PATH` 上的解释器；无论哪种，Modality 都不记录它，每次调用都信任描述。
- `args` 是 `[<插件目录>/helper.cjs]`，绝对路径。Modality 只追加 `--input`、`--artifacts` 与 `--output-mode`，别无其他。
- `read` 是经 `fs.realpathSync` 解析的 Node 安装根目录与插件目录——分别对可执行文件与插件目录解析——因此符号链接的 `node`（包管理器的 `bin` 链接）授权的是二进制真正所在的目录。Modality 把任务输入加入这些路径，使 helper 能加载自己的脚本与 `node_modules`、平台包中的 Node addon，以及输入。

行从不自己执行 OCR 来回应请求，也从不检查输入。`describe` 声明为 `async`，因此发起调用从不阻塞调用方；Modality 在调用前读取声明的类型，从不调用声明为其他类型的 `describe`，并把该目标读作需要更新（Modality §A.2.1）。Modality 限定等待时长，停止应答的行按缺席读取。它在进程生命周期内返回相同的值；Modality 可以在每个任务上调用它。

## 4. helper

### 4.1 调用与产物

`node helper.cjs --input <path> --artifacts <dir> --output-mode <text|layout>`，由 Modality 经 `HostPlatform::spawn_confined` 启动，`SystemServices::None`，无网络，任务的工作目录是唯一可写路径，超时 30 分钟，stdout 与 stderr 上限各 64 KiB（Modality §7、§A.2.1、A14）。取消与超时会杀死进程；helper 不安装信号处理函数。

helper 遵循 Modality 已定义的 helper 契约（Modality §A.2）：

- 成功时写出 `result.json`、`text.txt`，以及仅 `layout` 模式下每个页面图像一个 `page-NNNN.png`，在一行打印 `{"ok": true, "pageCount": N}` 并以 0 退出。
- 类型化拒绝时在一行打印 `{"ok": false, "error": {"code", "message"}}` 并以 2 退出。其他退出码表示失败。
- 所有临时文件都写在产物目录内，目录外不留下任何东西。
- 写入每个产物前先计算其大小，若任务的产物将越过 1 GiB 则以 `resource_limit_exceeded` 拒绝。Modality 在写入每个文件时计入同一上限，因此目录持有量从不超过它。
- 从字节嗅探输入，从不依赖声明的媒体类型，接受 PNG 与 JPEG 图像以及 PDF 文档。

`result.json` 是 Modality 的文档形状：`{text, pageCount, pages: [{page, text, width, height, imagePath?, regions: [{text, confidence, polygon}]}]}`。`text` 是各页文本以换行连接；每页的 `page.text` 是其行以换行连接；`polygon` 是页面像素中的四组 `[x, y]`，从左上角开始顺时针排列——这正是 `crates/dim-ocr` 已为 `local/apple-vision` 定义的编码。

### 4.2 图像

图像输入只读一次，用 `createEngine({execution: {provider: "cpu"}}).recognizeEncoded(bytes)` 识别。结果是一页：`page` 为 1，`width` 与 `height` 取自 `imageWidth` 与 `imageHeight`，`regions` 取自 `lines`（`text`、`confidence`、`box` 作为多边形），`text` 由各行按阅读顺序连接。图像输入不产生 `page-NNNN.png`：它未被栅格化，`layout` 模式下其页面的 `imagePath` 缺席，因为输入本身就是页面图像。`invalid_image`、`unsupported_pixel_format` 或 `invalid_argument` 的 `OcrError` 是类型化拒绝 `invalid_input`。

### 4.3 PDF

PDF 用 `createDocumentEngine({engineOptions: {execution: {provider: "cpu"}}}).recognizeDocument(bytes, options)` 打开，按顺序迭代其页面。每一页都被栅格化并识别；没有原生文本路径，因此 PDF 的内嵌文本层被忽略。选项携带 Modality 的上限：`maxPages: 500`、`maxPagePixels: 25_000_000`、`maxTotalPixels: 12_500_000_000`（500 页 × 25,000,000，因此总上限从不拒绝 Modality 上限接受的文档）、`maxFileBytes: 104_857_600` 与 `dpi: 150`。每个值都在 facade 自身的选项校验范围内（0.5.9 的 `packages/light-ocr/src/document.cjs`：第 73 行的 `positiveInteger` 默认上限为 `Number.MAX_SAFE_INTEGER`；`normalizeOptions` 第 81–124 行，选项字段在 87–103 行，把 `maxPages` 限制在 10,000、`dpi` 限制在 36–600）。12,500,000,000 小于 2^53，因此引擎精确接受它，helper 除了传入这些选项之外不自行施加其他上限。

`layout` 模式下 helper 用平台包的 `pdfium` 导出——文档引擎自己加载的渲染器——以相同的 `dpi` 再次渲染每一页，把字节写成 `page-NNNN.png`，并设置该页的 `imagePath`。第二次渲染是取得页面图像的唯一方式：文档 API 返回行与尺寸，不返回像素。那里的渲染失败是 `local_runtime_failed`。`text` 模式下除识别本身外不再渲染页面图像。

文档引擎没有针对加密 PDF 的错误码：栅格化器在加载文档时报告口令或权限失败。light-ocr 加载的绑定 `pdfium-native` 0.6.1 把 PDFium 的 `FPDF_ERR_PASSWORD` 映射为 `PASSWORD:Password required or incorrect`，把 `FPDF_ERR_SECURITY` 映射为 `SECURITY:Unsupported security scheme`（`src/napi_helpers.h` 的 `GetPdfiumErrorMessage`，第 24–55 行，两个 case 在 37–44 行），`src/pdfium_addon.cc:57` 用该消息拒绝加载。helper 把消息以 `PASSWORD:` 或 `SECURITY:` 开头的加载拒绝报告为 `encrypted_pdf`；其他加载或渲染失败是带渲染器消息的 `invalid_input`。

### 4.4 结果与错误映射

| light-ocr | Modality helper 契约 |
| --- | --- |
| `DocumentPage.index`（从 1 开始） | `pages[].page` |
| `DocumentPage.width`、`height` | `pages[].width`、`pages[].height` |
| `DocumentLine.text` / `OcrLine.text` | `pages[].text` 的行、`regions[].text` |
| `DocumentLine.confidence` / `OcrLine.confidence` | `regions[].confidence`，限制在 [0, 1] |
| `DocumentLine.box` / `OcrLine.box`（四个点） | `regions[].polygon`，页面像素中的四组 `[x, y]`，从左上角顺时针 |
| `invalid_argument`、`invalid_image`、`unsupported_pixel_format`、`unsupported_capability` | `invalid_input` |
| `resource_limit_exceeded`、`queue_full` | `resource_limit_exceeded` |
| `invalid_model_bundle`、`unsupported_model`、`model_integrity_failed`、`runtime_initialization_failed`、`invalid_engine`、`bundle_io_failed`、`unsupported_platform`、`package_load_failed`、`environment_closing`、`internal_error` | `local_runtime_unavailable` |
| `inference_failed`、`postprocess_failed` | `local_runtime_failed` |
| 渲染器因口令或权限拒绝的 PDF | `encrypted_pdf` |
| 其他任何错误（文件不存在、安装损坏） | `local_runtime_failed`，或输入本身是原因时的 `invalid_input` |

helper 自身的失败遵循同样的五个 code：缺少参数是 `invalid_input`，输入超过 100 MiB 是 `resource_limit_exceeded`，平台包无法加载是 `local_runtime_unavailable`。

## 5. 提供方

每个引擎都以 `execution: {provider: "cpu"}` 创建，所有平台一致。CPU 是 light-ocr 为其支持的每个平台发布的唯一提供方；Core ML 只存在于 `darwin-arm64`，且其受限行为尚未测量；WebGPU 只存在于 Linux x64 与 Windows x64。`execution.provider` 固定在 helper 中，任务无法配置，因此请求永远无法选择隔离配置未覆盖的提供方。

CPU 路径已在 `HostPlatform::spawn_confined` 于 macOS arm64 构建的配置中测量：读集合 {经真实路径解析的 Node 安装根目录、插件目录、输入}、以任务工作目录为唯一可写路径、无网络与 `SystemServices::None` 已足够。一张 PNG 与两个 PDF——一个带文本层、一个扫描件——的识别结果与无隔离运行的行、框、置信度完全一致，每张 PNG 约 0.8 秒，每次运行后工作目录为空，且没有创建 Core ML 缓存。

以后启用 Core ML 需要两件事：在该隔离配置下对 Core ML 路径做一次受限运行的测量，以及把 `OnDeviceRecognition` 加入该配置的 `SystemServices`（[HostPlatform](../../../architecture/rust-agent/plugins/HostPlatform.md)）；两者都是前提，不是本设计排期的工作。light-ocr 自身的规则让 `darwin-x64` 只用 CPU。

## 6. 时序与冲突

- 每个任务一个 helper 进程，任务开始时启动，取消、超时、Modality 卸载与进程退出时杀死。任务之间没有 OCR 状态；重启像任何宿主任务一样中断运行中的任务（Modality §A.6）。
- 并发由 Modality 决定：每个类别至多 4 个运行中的 OCR 任务。每个都是带自己引擎的进程，因此两个 helper 除了 CPU 之外不共享任何东西。
- 行没有在途工作：重载、更新、重新获取或卸载插件都只影响之后的调用。任务保留它开始时读到的描述，而 PluginManager 在进程运行期间不删除任何已安装副本（[PluginManager §7](PluginManager.md#7-安全)），因此描述所指名的目录会存在到进程退出。若该副本在 helper 启动时仍然不可读——例如用户手工删除了它——spawn 会拒绝该已消失的路径，任务结算为 `executor_error`，其工作目录被删除。
- 因此卸载插件影响的是之后的调用，而不是已接纳的任务：它的副本留在原处，由下次启动的清理删除（[PluginManager §3.1](PluginManager.md#31-启动)）。代价是磁盘——对 light-ocr 而言，一个被替换的副本约 124 MB，保留到下次启动。

## 7. 安全

- **信任锚。** 插件的 `package-lock.json` 是唯一的包来源，由 RuntimeManager 以 `npm ci --omit=dev --ignore-scripts --no-audit --no-fund` 安装；npm 按 lock 的 `integrity` 校验每个包，且不运行任何 lifecycle 脚本（[RuntimeManager §3.2](RuntimeManager.md#32-获取)、§7）。registry 是 RuntimeManager 的规则；镜像不增加信任。随发行的插件目录从不被修改。
- **不可信输入。** 输入文档只在受限 helper 中解析。Modality 与 daemon 从不打开它、从不解析它，也从不把它的内容交给其他进程。这是 OCR 作为宿主任务而非适配器的原因（Modality §A.2）。
- **隔离。** helper 在 `HostPlatform::spawn_confined` 下以 `SystemServices::None` 运行：Node 安装根目录、插件目录与输入可读；任务工作目录可写；无网络。已在 macOS arm64 与 CPU 提供方上测量：工作目录之外不写任何东西——每次运行后目录为空——也不创建 Core ML 缓存。在不提供任何隔离的平台上——Windows，以及没有 `bubblewrap` 的 Linux——helper 无隔离运行，Modality 在任务上记录 `confined: false`（Modality A14）；插件自己不作任何声明，也不做探测。
- **运行时无下载。** helper 只加载磁盘上已锁定的包。它没有网络，也没有更新器；包缺失或损坏是 `local_runtime_unavailable`。
- **无凭据。** helper 不读取凭据，也不联系任何服务。它的环境是 Modality 的允许列表，描述中不携带任何环境变量增补。
- **不进入模型上下文。** helper 返回的任何内容都不进入模型上下文；用户通过普通方式把结果移入 Session（Modality §5）。

## 8. 规则检查

规则：[principles §1 与 §4](../../../architecture/rust-agent/principles.md)，[ADR 002](../../adr/002-plugin-scope.md) D1–D5。

| 规则 | 本文如何符合 |
| --- | --- |
| §4.1 改别的模块拥有的状态时写指令事件 | 插件不拥有任何状态：行提供描述，helper 写入 Modality 创建的任务工作目录，任务记录归 Modality。 |
| §4.2 交接后不能撤回 | 不适用：helper 交接文件与一个退出状态；Modality 只结算任务一次。 |
| §4.3 只用 before 回调在写入前干预 | 不适用：没有回调，也没有核心入口。 |
| §4.4 核心机制不带业务含义 | 除 §9 中的通用需求外，插件不要求 Modality、HostPlatform 或 RuntimeManager 做任何改动；目标是数据，不是机制。 |
| §4.6 模型上下文只来自已交接的事件 | 插件不写任何模型可见的内容。 |
| §4.7 插件之间只通过公开事件和服务交互 | 行提供 `ocrHelper`，不读其他插件的状态。Modality 只通过描述这个公开服务到达 helper。 |
| §4.8 向机制所有者提出通用需求 | §9 列出已在别处提出的三项；本文没有新增。 |
| §4.9 只有后续决策会用的事实进 Timeline | 不写 Timeline 事件。 |
| §1 Capability 与 App Server 边界 | 不是 Capability：没有 Timeline 状态，也不登记工具、方法或提示来源。 |
| ADR 002 D1 作用域跟随状态归属 | 进程：每个宿主一份 helper 描述，供所有任务使用。 |
| ADR 002 D2 不设 wiring 模块 | 没有 wiring：Modality 每次调用读取名字，由 loader 目录解析。 |
| ADR 002 D3 依赖跟随作用域 | Modality 对 `ocrHelper` 的读取是可选的按调用读取；插件除了自己的包不依赖任何东西。 |
| ADR 002 D4 结构性配置与业务配置 | 没有用户配置：提供方策略固定在 helper 中，包集合就是 lock。 |
| ADR 002 D5 语言无关契约 | `ocrHelper` 以及 helper 的 argv、产物与 JSON 行是 JSON 与进程契约；行与 helper 是它们的一个实现。 |

## 9. 向机制所有者提出的需求

本插件不新增需求；它使用 Modality 与 PluginManager 已提出的三项。

| 所有者 | 需求 | 位置 |
| --- | --- | --- |
| Modality | 列出插件提供的目标，每次调用读取 `ocrHelper`，并按描述启动 helper | [Modality §9.1](Modality.md#91-按任务适配器查找)、§A.2.1 |
| PluginManager | 安装可选插件，把它复制到 home，并在建行之前安装它声明的 npm 依赖 | [PluginManager §2.3](PluginManager.md#23-默认配置文件)、§3.2、§9 R5 |
| RuntimeManager | `RuntimeService::installPackages`，以及在 Node 关闭时打开插件所需的 `install("node")` | [RuntimeManager §5](RuntimeManager.md#5-接口)、§9 R1 |

HostPlatform 的 `spawn_confined` 与 `Confinement` 已存在；本插件对它没有新需求。

## 附录 A. helper 契约

- **argv。** `--input PATH`、`--artifacts DIR`、`--output-mode text|layout`。`--input` 与 `--artifacts` 必填；`--output-mode` 默认为 `text`。未知参数或缺少值是 `invalid_input`。
- **输入。** 一个普通文件，至多 100 MiB，按前几个字节判断为 PNG、JPEG 或 PDF。声明的媒体类型从不使用。
- **目录。** helper 不在 `--artifacts` 之外创建任何东西；Modality 在任务工作目录内创建该目录。
- **产物。** `result.json`（总是）、`text.txt`（总是）、`page-NNNN.png`（`layout` 模式，每个渲染的 PDF 页面一个，`NNNN` 补零到四位）。每个名字都是单个组件。
- **退出。** 0 并打印 `{"ok": true, "pageCount": N}`；2 并打印 `{"ok": false, "error": {"code", "message"}}`；其他任何 code 都是失败，Modality 报告任务的 `executor_error`。
- **上限。** 500 页、每页 25,000,000 像素、输入 100 MiB、每任务 1 GiB 产物。
- **无状态。** 一个进程、一个任务，没有缓存，没有锁文件。退出前关闭引擎。

## 附录 B. 包布局与 lock

```json
{
  "name": "light-ocr",
  "version": "<插件版本>",
  "description": "Local PP-OCR for Modality through light-ocr",
  "code": { "entry": "index.ts", "provides": { "ocrHelper": { "describe": "async" } } },
  "dependencies": "npm"
}
```

`package.json`：

```json
{
  "name": "dim-light-ocr",
  "private": true,
  "dependencies": {
    "@arcships/light-ocr": "0.5.9",
    "@arcships/rutis": "<runtime/package-lock.json 锁定的版本>"
  }
}
```

lock 以 `integrity` 锁定 facade、它的 runtime、模型与平台包，以及 `@arcships/rutis`。facade 自身的依赖按其声明安装，因此 lock 是整套包唯一的固定点。`helper.cjs` 只从插件的 `node_modules` 解析 `@arcships/light-ocr`，不解析别处。

## 附录 C. 测试策略

没有自动化测试会下载、安装或加载真实引擎。

- **helper 单元测试。** 测试树中的 fixture `node_modules/@arcships/light-ocr` 导出 `createEngine`、`createDocumentEngine`、`getVersion`、`hasPdfSupport` 与 `OcrError`，其结果与失败由测试控制。helper 作为子进程在 fixture 输入上运行，测试断言 argv 解析、产物名字与形状、`text.txt` 内容、多边形转换、上限检查、§4.4 的每个错误映射，以及拒绝时不会在越界后留下部分产物。不获取任何东西；fixture 是仓库中的一个目录。
- **行与插件测试。** 行连同 fixture 引擎一起加载，断言 `describe` 符合 Modality A.2.1 的形状——`target`、`executable` = `process.execPath`、`args`、`read`。服务在场、缺席与移除是 Modality 的验收流程，用桩提供方运行（Modality 附录 D 第 9 项）；插件自身的测试只覆盖它发布的描述。
- **安装测试。** 用一个声明 `dependencies` 的 fixture 插件，PluginManager 的获取会复制它、从本地 fixture registry 安装其锁定依赖并改名到位；随发行的目录不变（PluginManager 附录 C 第 13 项）。
- **唯一一次真实运行。** 一次端到端验收，不进 CI，在 macOS arm64 上：从发布的 optional 目录安装插件，从公共 registry 取真实 lock，对一页 PNG 和两页 PDF 以两种输出模式运行 `modality.ocr.recognize`，断言文本、页数、`layout` 模式下的 `page-NNNN.png`、退出码 0、`confined: true`，以及 helper 未在工作目录之外写任何东西。它会安装并在受限进程中加载下载的原生代码，因此由人手动运行，从不作为常规测试套件的一部分。
- **测试装置。** 任何受限测试或装置都像 `spawn_confined` 一样给 helper 的 stdout 与 stderr 接管道。把它们重定向到授权根之外的文件会让 Node 在初始化时中止（已测量：退出 250，五次全部如此）；必须使用文件的装置把它们放进可写根内。

## 附录 D. 验收

1. 安装前，`plugin.list` 以 `optional: true` 显示 `light-ocr`；`dimcode plugin install --optional light-ocr` 把它复制到 `<home>/plugins/light-ocr/<version>+<suffix>`，在那里安装其锁定依赖并打开它；随发行的目录没有 `node_modules` 且未被修改。
2. 插件已安装且行在运行时，`modality.capabilities` 在 `local/apple-vision` 之外列出 `local/pp-ocr`；插件关闭、Pending 或移除后，只列出 `local/apple-vision`，其余不变。
3. 图像任务返回文本与区域；`layout` 任务返回相同的区域，且图像输入没有页面图像；PDF 任务每页返回一页；两页 PDF 在 `layout` 模式下返回 `page-0001.png` 与 `page-0002.png`。
4. 上限生效：501 页的 PDF 和超过 100 MiB 的输入是 `resource_limit_exceeded`；加密 PDF 是 `encrypted_pdf`；损坏的图像是 `invalid_input`。
5. 取消与超时会杀死 helper；任务结算为 cancelled 或 failed，工作目录被删除。
6. macOS 上 `confined: true`；Windows 与没有 `bubblewrap` 的 Linux 上 `confined: false`。
7. 新发行的副本在下次启动时把已安装插件后台重新获取到新的 `<version>+<suffix>` 目录：只有新副本就绪后才切换记录与 `dir`，旧副本继续服务并留在磁盘上直到下次启动的清理，目标无需再 `enable` 一次即可继续工作；失败的重新获取保留旧副本，并在 `plugin.list` 中显示，直到 `plugin.update` 修复。
8. 仓库中没有任何测试会下载、安装或加载真实引擎；fixture 引擎与桩 `ocrHelper` 提供方覆盖 Modality 的流程。
9. 在暂存的发行包中，`plugins/light-ocr/` 恰好只有五个随发行文件——`dim-plugin.json`、`index.ts`、`helper.cjs`、`package.json`、`package-lock.json`——没有 `node_modules`，且发行包中不含 Node 安装。
