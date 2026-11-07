# RuntimeManager

RuntimeManager 提供插件代码运行所需的 Node 与 Python rutis 运行时、多数 Python MCP 服务器用来启动的 uv 工具（`uvx <server>`），以及任务里的 OCR helper 读取的 OCR 组件（PP-OCR bundle）（[#476](https://github.com/arcships/dim/issues/476)）。它是 App Server 的一个进程插件，占一行 loader 行。它获取每个开启的运行时或工具（下载固定版本到 home，按官方校验文件校验，或使用机器上已安装的），把装了什么记在自己的文件里，然后提供运行时的启动命令。命令一到，运行时的连接行就启动进程；插件行按 rutis 的规则等待这个运行时的会话。它下载的运行时也作为兜底供 App Server 启动的进程使用——agent 的 `exec`、`!` shell 输入、hook 命令和 stdio MCP 服务器：它们的 bin 目录排在用户自己的 `PATH` 之后（§5）。

发布包带可执行文件、默认配置文件和默认插件，不带运行时（[发布打包](../../../release/README.md#local-packaging)）。

## 0. 实现范围

| 部分 | M1（2026-11-01） | 以后 |
| --- | --- | --- |
| Node：下载固定版本、按官方校验文件校验、按固定 lock 安装 `@arcships/rutis-runtime`、启动（§3） | 是 | |
| Python：下载固定版本、按官方校验文件校验、按固定 hash 安装 `rutis` 包、启动（§3） | 是 | |
| uv：下载固定版本、按官方校验文件校验、`source: system` 用 `PATH` 上的 uv；没有启动命令、包目录和连接行（§3） | 是 | |
| OCR：组件的下载、按 Dim 自己的 pin 校验、属主的冒烟测试和激活；没有启动命令、包目录和连接行（§2.5、§3.2） | 是 | |
| 两个运行时和 uv 都支持 `source: system`（§2.2） | 是 | |
| 下载源列表：官方源在前，镜像在后（§2.1） | 是 | |
| `runtime.list`、`runtime.install`、`runtime.configure`、`/runtime` 面板与 `dimcode runtime` 各形态（§5） | 是 | 状态变化通知客户端 |
| 发布包带默认插件和默认配置文件（§6） | 是 | |
| Python 通过 PluginManager 的链接目录导入插件（§4） | 运行时一侧在这里；链接由 PluginManager 建（[#3](https://github.com/arcships/dim/issues/3)） | |
| 删除本版 Dim 不再使用的运行时目录 | | 是 |

## 1. 结构

```text
App Server 启动
  Parts::new：用固定路径建启动命令和行解析器（§2.3）
  行：RuntimeManager（必需）、session-node、py（非必需）、插件行
        │
        ▼
RuntimeManager apply：登记 `runtime` 方法和 RuntimeService，然后为每个开启的运行时起一个任务
        │  获取（§3）：已安装且与固定版本一致？否则 下载 → 校验 → 解压 → 改名
        ▼
ctx.provide_at(nodeRuntime / pythonRuntime, 启动命令)
        │ rutis 依赖等待：连接行注入它
        ▼
session-node / py：启动进程、握手、挂载会话、维护 RuntimeStatus
        │ 会话放行自己的行
        ▼
插件行 session-node:… / py:…：Pending → Active
```

- **RuntimeManager**（进程行 `RuntimeManager`，必需）：`apply` 只做登记；获取在它自己的任务里进行，所以它立刻 Active，启动从不等待下载。获取到的运行时，其启动命令从 RuntimeManager 的上下文提供，所以 RuntimeManager 卸载时命令撤回，运行时进程随之结束。它不挂子插件，也不持有句柄。
- **连接行** `session-node` 和 `py` 就是运行时。每一行注入自己的启动命令（只在 Rust 里的进程服务 `nodeRuntime`、`pythonRuntime`），所以命令提供后 rutis 才启动它。它启动进程，在进程的通道上挂载会话，并在以行名命名的只在 Rust 里的进程键上维护运行时的 `RuntimeStatus`（`starting`、`ready`，或带原因的 `down`）。插件行运行在这个会话上，像等待任何运行时一样等它；`Loader::waits` 给出的是连接行，`plugin.list` 显示的就是这个（[PluginManager §2.5](PluginManager.md#25-运行时)）。
- **可替换**：RuntimeManager 是插件。另一个提供相同启动命令的插件可以替换它。
- **是工具，不是运行时**：uv 供用户自己的命令、以及用户用 `uvx` 启动的 MCP 服务器使用。它没有连接行、没有启动命令、没有包目录、没有 `RuntimeStatus`；它的 `ready` 来自记录（§2.4）。`Kind::launches_runtime` 把运行时与工具、组件区分开：获取、`provide`、`install` 和 `runtime.list` 都按它分支，只有 system 检查读取 uv 自己的 `--version` 格式。`Kind::is_program` 把程序与 OCR 组件区分开：启动只安装程序，只有程序能用 `source: system`，也只有程序的目录会进入进程的 `PATH`。

## 2. 数据

### 2.1 版本和下载源：`runtime/runtimes.json`，编译进 Dim

```json
{
  "node": {
    "version": "24.21.0",
    "checksums": "https://nodejs.org/dist/v{version}/SHASUMS256.txt",
    "sources": ["https://nodejs.org/dist/v{version}/{file}", "https://npmmirror.com/mirrors/node/v{version}/{file}"],
    "files": { "darwin-arm64": "node-v24.21.0-darwin-arm64.tar.gz" }
  },
  "python": {
    "version": "3.12.15+20261003",
    "checksums": "https://github.com/astral-sh/python-build-standalone/releases/download/20261003/SHA256SUMS",
    "sources": ["https://github.com/astral-sh/python-build-standalone/releases/download/20261003/{file}",
                "https://registry.npmmirror.com/-/binary/python-build-standalone/20261003/{file}"],
    "files": { "darwin-arm64": "cpython-3.12.15+20261003-aarch64-apple-darwin-install_only.tar.gz" }
  },
  "uv": {
    "version": "0.13.7",
    "checksums": "https://github.com/astral-sh/uv/releases/download/{version}/sha256.sum",
    "sources": ["https://github.com/astral-sh/uv/releases/download/{version}/{file}"],
    "files": { "darwin-arm64": "uv-aarch64-apple-darwin.tar.gz" }
  },
  "ocr": {
    "version": "1",
    "sha256": { "darwin-arm64": "…", "linux-x64": "…" },
    "sources": ["https://github.com/arcships/dim/releases/download/ocr-bundle-v1/{file}"],
    "files": { "darwin-arm64": "ocr-1-darwin-arm64.tar.gz" }
  },
  "npmRegistries": ["https://registry.npmjs.org", "https://registry.npmmirror.com"],
  "pythonIndexes": ["https://pypi.org/simple", "https://pypi.tuna.tsinghua.edu.cn/simple"]
}
```

- Dim 固定版本号和每个平台的压缩包文件名（`darwin-arm64`、`darwin-x64`、`linux-x64`、`linux-arm64`、`windows-x64`、`windows-arm64`）。压缩包的 SHA-256 在获取时从 `checksums` 读取，即发布方自己的校验文件（nodejs.org 的 `SHASUMS256.txt`，python-build-standalone Release 的 `SHA256SUMS`，uv Release 的 `sha256.sum`），通过 HTTPS 从这个固定地址取。Dim 信任官方发布方；环境变量和镜像都不能替换这个地址。拦截这个地址的网络是用户自己的网络。uv 的 Release 在每个压缩包旁提供单文件 `<file>.sha256`，也提供合并的 `sha256.sum`；Dim 固定使用合并文件，它的行用 `*` 标出文件名。Linux 使用 uv 静态链接的 musl 压缩包，因此不依赖 glibc。组件在这个文件里固定的是 Dim 自己产物的摘要：`sha256` 逐目标给出摘要，不取任何校验文件，bundle 由 Dim 构建，源码树就是它的信任锚（§2.5）。OCR bundle 不为 Intel Mac 发布：ONNX Runtime 自 1.23.2 起没有 `osx-x86_64` 构建，因此 `files` 和 `sha256` 只列出其余五个平台，`runtime.install ocr` 拒绝缺的那个。
- 运行时的包沿用已有的固定位置：`runtime/package.json` 和 `runtime/package-lock.json`（`@arcships/rutis-runtime`，与宿主的 rutis 版本一致，每个包都有 npm `integrity`），以及 `runtime/python-requirements.txt`（`rutis`，带 `--hash`）。它们都编译进 Dim；升级宿主的 rutis 版本时在同一个提交里一起改。
- 下载源按顺序尝试，用第一个与官方 SHA-256 一致的文件。环境变量把用户自己的源放在最前：`DIMCODE_NODE_MIRROR`、`DIMCODE_PYTHON_MIRROR`、`DIMCODE_UV_MIRROR` 和 `DIMCODE_OCR_MIRROR`（基础 URL，后面接文件名），`DIMCODE_NPM_REGISTRY`（CLI 已在用）和 `DIMCODE_PYTHON_INDEX`。镜像只提供文件内容，由 pin 校验它们。uv 和 OCR bundle 都没有配置公开镜像：没有已验证其从版本固定的地址提供同一文件。

### 2.2 配置：默认配置文件的 `runtimes` 一节

默认配置文件（[PluginManager §2.3](PluginManager.md#23-默认配置文件)）随 Dim 发布，增加：

```json
"runtimes": {
  "node":   { "source": "download", "auto": false },
  "python": { "source": "download", "auto": false },
  "uv":     { "source": "download", "auto": false },
  "ocr":    { "source": "download", "auto": false }
}
```

- `source`：`download` 用上面的压缩包；`system` 用 `PATH` 上的解释器（`node`；`python3`，Windows 上是 `python`；`uv`），获取时检查：Node 22.19 或更高（`@arcships/rutis-runtime` 的 `engines`）且带自己的 npm（`npm-cli.js` 在官方压缩包和安装程序放的位置；没有它的 Node 会失败，原因里写出这个路径），Python 3.12 或更高且有 pip，uv 0.11.0 或更高且旁边有 `uvx`。检查不通过就是这个运行时或工具的失败（§3.3）。
- `auto`：`true` 每次启动都获取该运行时或工具。`false` 在 `runtime.install`（§5）之前不动；装好后会被记录（§2.4），之后每次启动都像 `auto` 一样获取。组件无论 `auto` 如何都不会由启动安装：它的 bundle 很大，而且在属主的校验器就位之前没有东西能读它，所以只有 `runtime.install` 这一条路。组件的 `auto` 无论文件怎么写都读作 `false`，对组件调用 `runtime.configure {auto: true}` 会在写任何东西之前被拒绝。
- `ocr` 拒绝 `source: system`：组件只下载。`runtime.configure` 在改动文件之前就拒绝它，手写这样的文件会让启动失败，和任何手改的非法配置一样。
- 发布文件把三个都设为 `auto: false`，因此首次启动不下载任何东西。Node 插件（例如 Workflow）需要先执行 `runtime.install`（`/runtime` 面板，或 `dimcode runtime install node`）才能运行；用 `uvx` 启动的 Python MCP 服务器需要先 `dimcode runtime install uv`。
- 用户在 `<home>/runtimes/config.json` 里覆盖预置，形状相同，按运行时、按字段覆盖；客户端通过 `runtime.configure`（§5）写它，保持这个合并。它在 home 里，所以只有用户本地配置能改，项目配置不能。
- 配置在动作应用它的那一刻读取，而不是启动时读一次：`runtime.install` 当时重新读文件和环境（§3.3），所以 `source` 或 `auto` 的改动不必重启进程即可生效。`runtime.list` 报告的是正在生效的运行时，不是文件——改了的文件要等某个动作应用它才出现。

### 2.3 `<home>/runtimes/` 下的布局

| 路径 | 内容 |
| --- | --- |
| `node-v24.21.0-<平台>/` | 解压后的 Node |
| `node-packages-<lock 摘要>/` | `package.json`、`package-lock.json` 和 `npm ci` 装的 `node_modules/` |
| `python-3.12.15+20261003-<平台>/` | 解压后的 Python |
| `python-packages-<requirements 摘要>/` | `pip install --target` 装的 `rutis` 包 |
| `uv-0.13.7-<平台>/` | 解压后的 uv：`uv` 和 `uvx`（Windows 上是 `uv.exe`、`uvx.exe` 和 `uvw.exe`） |
| `ocr-1-<平台>/` | 解压后的 OCR bundle（§2.5），`manifest.json` 在它的根上 |
| `runtimes.json` | RuntimeManager 的记录（§2.4） |
| `config.json` | 用户的覆盖（§2.2），可选 |
| `*.partial/`、`*.download` | 进行中的内容；每次尝试前删除 |

每个路径都由固定版本、配置和 home 算出，所以 App Server 在 reconcile 之前就能用它们建好启动命令和行解析器，和使用配置的路径时一样；运行时进程要等 RuntimeManager 提供命令才启动。`source: system` 时，解释器路径在 App Server 建命令时从 `PATH` 查找。

启动命令就是 rutis-host 自己的启动器对这两个包用的命令：Node 在 `@arcships/rutis-runtime` 包里运行 `node --import tsx src/runner.mjs`；Python 在它的 project 里运行 `python -m rutis`，`PYTHONPATH` 上有 `rutis` 包和 project，并设 `PYTHONUNBUFFERED=1` 和 `PYTHONDONTWRITEBYTECODE=1`。每个进程的最后几个参数是它的通道、`--id <行>` 和它的 anchor。uv 没有启动命令：用户自己的命令和 MCP 条目从它的归档目录运行 `uv` 或 `uvx`，这个目录在兜底 `PATH` 上（§5）。

Python 运行时的 project 目录是 `<home>/plugins/.python/`，归 PluginManager（§4）。Node 运行时的 anchor 是它的包目录里的 `package.json`。

### 2.4 `runtimes.json`

```json
{ "node":   { "source": "download", "version": "24.21.0", "sha256": "…", "packages": "node-packages-…" },
  "python": { "source": "download", "version": "3.12.15+20261003", "sha256": "…", "packages": "python-packages-…" },
  "uv":     { "source": "download", "version": "0.13.7", "sha256": "…" },
  "ocr":    { "source": "download", "version": "1", "sha256": "…" } }
```

只由 RuntimeManager 在运行时和它的包装好之后、或 uv 工具装好之后写。`version` 对 `download` 是固定的版本；对 `system` 是解释器的版本，每次启动都检查，`runtime.list` 报告的也是它。`sha256` 是压缩包校验时用的官方 SHA-256。某一项的 source、version、packages 与固定版本和配置算出的一致、且对应目录存在，就是已安装：下次启动直接用，不再下载。uv 的项没有 `packages`，因为工具没有包目录。组件也没有包：它的目录存在，当且仅当它的 `manifest.json` 存在；`runtime.list` 里的 `path` 就是这个目录。`auto: false` 的运行时有记录，表示用户装过它。记录也是 `runtime.list` 的 `path` 反映的东西：记录的该项在正在生效的 plan 下是已安装时，运行时才报告它的解释器路径。别的都不存；运行时是否在运行是它的连接的 `RuntimeStatus`，而 uv 不启动运行时，仅凭记录就是 `ready`。

### 2.5 OCR 组件的 bundle

bundle 是一个平台上的 PP-OCRv6 small 模型，以及 OCR helper 运行它们所需的一切：

```text
ocr-1-<平台>/
  manifest.json                  # {bundle: "pp-ocrv6-small", version: "1", target, files: [{path, bytes, sha256}]}
  lib/libonnxruntime.<版本>.<扩展名> # darwin .1.31.1.dylib，linux .so.1.31.1，windows onnxruntime.dll
  lib/libpdfium.dylib | libpdfium.so | pdfium.dll
  models/{det.onnx,rec.onnx,ppocrv6_dict.txt,det.inference.yml,rec.inference.yml}
  licenses/                      # ONNX Runtime 与 pdfium 的许可证及第三方声明；模型为 Apache-2.0
```

- **来源**：ONNX Runtime 1.31.1（MIT）和 pdfium `chromium/8125`（BSD-3-Clause），各自取发布方为该目标提供的官方压缩包，逐字节复制——只保留核心库文件，因为 CPU execution provider 就在它里面。模型是 PaddleOCR 的 PP-OCRv6 small 检测与识别图（Apache-2.0）及其两个 `inference.yml`，作为出处随包交付；helper 读取的字典从固定的识别 `inference.yml` 推导，所以模型和它的字符集落在同一个摘要上。每个模型文件都从固定的仓库 revision 获取（检测 `28fe5895c24fd108c19eb3e8479f4ab385fbfc62`，识别 `b8f84f0b80c529de40b4fbb3544b84fa7233a513`），所以分支上的重新上传不会破坏构建。Linux 库需要 glibc 2.28 或更高（`libonnxruntime.so`；pdfium 需要 2.16），因此 bundle 的下限是 2.28。
- **可重现**：`scripts/ocr-bundle.mjs` 按 `runtime/ocr-bundle.json` 里的 pin 构建一个目标：下载每个上游文件，摘要不符就拒绝，按名字从每个压缩包取出所需文件，推导字典，写 manifest，然后打包成一个顶层目录，条目有序、时间戳固定、uid 和 gid 为 0、权限 0644 与 0755、gzip 头不带名字和时间戳。这里不签名：macOS 的两个 dylib 带发布时的链接器 ad-hoc 签名，字节不依赖 `codesign` 版本。`--check` 把每个构建出的压缩包与 `runtimes.json` 里的摘要比较；`--verify` 对别处构建的压缩包做同样的检查。
- **pin**：bundle 先固定、后发布：本地构建五个目标，把 `sha256` 和 `files` 填进 `runtime/runtimes.json`，所以源码树就是信任锚，第一次安装不需要文件旁的校验文件。`.github/workflows/ocr-bundle.yml` 是 `workflow_dispatch`：打包五个目标并与该 pin 比对；维护者以 `publish` 派发时把压缩包上传到固定的 `ocr-bundle-v1` Release，也就是 `sources` 指向的位置。在维护者派发之前，`runtime.install ocr` 下载不到任何已存在的东西。


## 3. 流程

### 3.1 启动

1. App Server 读固定版本和配置，用固定路径（§2.3）建启动命令，加入这些行：`RuntimeManager`（必需）、`session-node` 和 `py`（非必需）、插件行。
2. RuntimeManager 的 `apply` 在 `managementMethods` 上登记 `runtime`，提供 `RuntimeService`，然后为每个开启的运行时（`auto`，或在 `runtimes.json` 里有记录）起一个任务。
3. 每个任务获取自己的运行时（§3.2）并提供它的启动命令。连接行、以及等待它的会话的插件行随后自己启动。两个运行时互不影响：一个失败不影响另一个。

启动不等待任何运行时：没有网络的首次启动也能到达用户，插件行处于 Pending。

### 3.2 获取

每个运行时在自己的任务里：

1. 如果 `runtimes.json` 记录它已安装（§2.4），跳到第 6 步。
2. `download`：先从官方校验文件、组件则从 pin 读取压缩包的 SHA-256（校验文件里没有列出的文件不下载），再按顺序从每个下载源取压缩包，写到 `<home>/runtimes/<目录>.download`，边写边算 SHA-256，用第一个一致的。一致之后才解压，解压到 `<目录>.partial/`，拒绝离开该目录的条目和指向目录外的链接。解压完成后改名为最终路径，不替换任何已校验过的内容。`system`：查找并检查解释器（§2.2）。
3. 组件在激活之前先受检：构建里的 `ComponentVerifier`——在 Dim 里是属主自己的 helper，在沙箱里对一个内嵌样本运行——读取 `<目录>.partial/` 并作答。校验器失败时删除该 partial 目录、不写记录，安装保持 `failed` 并带上原因。构建里没有该组件的校验器时直接拒绝安装：激活没有经过任何检查的字节就违背了这一步的意义。
4. 包，先装到 `<目录>.partial/` 再改名：Node 用已校验的（或 system 的）Node 自带的 npm，对固定的 `package.json` 和 lock 执行 `npm ci --ignore-scripts --omit=dev --no-audit --no-fund --registry <r>`，按顺序尝试每个 registry；npm 按 lock 的 `integrity` 校验每个包。Python 执行 `python -m pip install --require-hashes --no-deps --only-binary :all: --target <目录> --index-url <i> -r requirements`，按顺序尝试每个 index。uv 没有包目录，这一步对它是空的。
5. 写 `runtimes.json`。
6. 从 RuntimeManager 的上下文提供启动命令。不启动运行时的工具跳过这一步：它的记录就是它 `ready` 的依据。

不执行任何未校验的东西：运行 npm 或 pip 的解释器是已校验的压缩包里的（或用户用 `system` 选的那个），npm 关闭脚本安装并按固定 lock 校验每个包，pip 拒绝任何没有固定 hash 的文件。

### 3.3 失败与离线

获取失败的运行时在 `runtime.list` 里是 `failed`，原因列出每个下载源和它的错误。没有东西被替换：半成品目录在下次尝试时删除，之前已安装的版本（如果有）保留。校验器失败的组件是 `failed`，带原因；没有东西被激活。需要它的插件行保持 Pending，等待它的连接行的会话。下一次尝试是下次启动或 `runtime.install`（§5）；没有自动重试。

运行期间改的配置由 `runtime.install` 应用：该调用重新读 `<home>/runtimes/config.json` 和环境，当给出的 `source` 与正在生效的不同时，撤回该运行时的启动命令、释放正在运行的连接，再按新来源获取，连接行于是用新的解释器重新启动。工具没有启动命令也没有连接可撤回；来源变了只会重新获取它。除此之外没有别的东西重启。配置读不出来时该调用以错误返回，运行时保持原样。没有文件监听：没有动作去应用的文件不产生任何变化。

运行时进程自己结束时，它的会话结束，会话上的行回到 Pending。连接行保持 Active，它的 `RuntimeStatus` 是 `down`，原因是进程如何结束（例如 `exited with exit status: 1`）；启动或握手失败时是带那个错误的 `down`。`runtime.list` 显示这个状态和原因。对 `down` 的运行时执行 `runtime.install` 会撤回它的启动命令再重新提供，rutis 于是用新进程重新启动连接行。

### 3.4 升级与回退

固定了新版本的 Dim 获取到新目录（目录名带版本和摘要），新版本装好后才改写 `runtimes.json`。在此之前、以及失败时，之前的目录不动；在同一个 home 上启动旧版 Dim 会找到它自己的目录并使用。M1 不删除本版 Dim 不再使用的目录。

## 4. 从目录安装的 Python 插件如何导入

rutis 的 Python 运行时是一个进程；行 `py:<模块>` 用 `importlib.import_module` 从运行时的 `sys.path` 导入，`sys.path` 里有它的 project 目录。插件目录可以在任何位置（本地路径原地使用；Git 安装在 `<home>/plugins/<id>/<commit>`）。

- Python 运行时的 project 目录是 `<home>/plugins/.python/`，归 PluginManager；App Server 启动时创建它，让运行时的导入路径在第一个插件之前就存在。
- 开启一个 Python 插件时，PluginManager 在那里建一个目录链接 `<包名>` 指向插件目录（`<包名>` 是插件 id 把 `-` 换成 `_`；Windows 上用 junction），并建行 `py:<包名>.<入口模块>`。正在运行的进程不重启就能导入新插件。
- 插件内部的模块之间用相对导入（`from . import helpers`）。reload 时，入口模块文件变了就重新导入，这是 rutis 对每个 Python 行的做法（[需求第 3 条](../../../design/plugin-management.md)）。
- PluginManager 开启 Python 插件而 Python 处于关闭时，调用 `RuntimeService::install("python")`（§5）。

## 5. 接口

| 接口 | 内容 |
| --- | --- |
| 启动命令 | `nodeRuntime`、`pythonRuntime`：连接行 `session-node` 和 `py` 注入的只在 Rust 里的进程服务；运行时获取后提供。uv 没有。 |
| `RuntimeStatus` | 每个连接行在以行名命名的只在 Rust 里的进程键上维护：`starting`、`ready`，或带原因的 `down`。RuntimeManager 读取它。uv 没有。 |
| `RuntimeService`（Rust 进程服务） | `list() -> [RuntimeEntry]`；`install(name)`：开启运行时、工具或组件并开始获取；正在获取或正在运行时什么都不做；获取失败后再获取一次；连接 `down` 后重新提供启动命令。对 uv 它只安装并让工具 `ready`，没有连接可重启。`component(name) -> Option<ComponentState>`：组件 `name` 的状态——`off`、`acquiring`、带原因的 `failed`，或带 root 和版本的 `ready`——读自 `runtime.list` 渲染的同一份状态，两者不可能不一致；本构建没有该名称的组件时为 `None`。用可选依赖读取，所以 RuntimeManager reload 不会让调用方 reload。 |
| `ToolchainSource`（Rust 端口，HostPlatform 读取） | `toolchains() -> [InstalledToolchain { bin, root }]`：按正在生效的 plan（§2.4）已安装的下载运行时与工具，顺序固定为 node、python、uv，每项带可执行文件所在的目录（`bin`）和它的归档目录（`root`）。`system` 解释器不贡献任何一项：它是用户自己的，用户的 `PATH` 已经能到达它。HostPlatform 在每次启动进程时读取，把 `bin` 接在子进程 `PATH` 中用户自己的目录和嵌入宿主目录之后，并把 `root` 作为受沙箱限制的进程的只读根（§7）。App Server 的 `ToolchainSource` 行通过可选依赖读取 `RuntimeService` 来作答，所以 RuntimeManager reload 既不会让 HostPlatform reload，也不会让它持有一个已被撤销的服务。没有这一行的宿主，启动的进程只有没有任何运行时时的那份 `PATH`。 |
| `runtime.list`（管理方法） | `[{name, source, auto, offered, version, path?, status, reason?}]`；`auto` 是正在生效的配置，客户端因此不必自己存一份；`offered` 说明这个平台有没有可安装的东西——每个程序类都是 `true`，组件在此平台没有 bundle 时是 `false`，此时 `reason` 是 `install` 拒绝时用的同一段文字、`status` 保持 `off`；该字段是新增的，忽略它的客户端照旧工作；`path` 是该运行时所用可执行文件的绝对路径——对 OCR 组件是 bundle 目录——只在 §2.4 的记录按正在生效的 plan 判定为已安装时出现，与跳过下载是同一条判断；路径不是合法 UTF-8 时省略，并由 `reason` 说明；`status` 是 `off`、`acquiring`、`failed`，不启动运行时的工具装好后是 `ready`，或提供启动命令后连接的 `RuntimeStatus`：`starting`（连接行还没启动时也是）、`ready`，或带原因的 `down`。 |
| `runtime.install {name}`（管理方法，有修改） | `RuntimeService::install`：重读配置，`source` 变化时通过重启连接行应用（§3.3），并立即回复当前状态；配置无法读取时该调用以 `runtime_configuration` 错误返回。本构建没法校验的组件、以及本平台没有 bundle 的目标，也在这里被拒绝。 |
| `runtime.configure {name, source?, auto?}`（管理方法，有修改） | 原子地写 `<home>/runtimes/config.json`，按运行时、按字段覆盖预置（§2.2），随后按 `runtime.install` 的方式应用；写不出或读不出时该调用以 `runtime_configuration` 错误返回。该运行时正在获取时拒绝，因为获取过程中它的 plan 不能变（§3.1）。 |
| 客户端 | 两种终端模式下的 `/runtime`（面板，以及打字的 `/runtime install|source|auto`），以及 `dimcode runtime list`、`install <node|python|uv|ocr>` 和 `config NAME [--source …] [--auto …]`（[CLI 命令](../../../zh/design/cli-commands.md#插件运行时)）。 |

## 6. 发布

发布的原生压缩包和每个 npm 平台包，在 `bin/` 旁边带：

```text
bin/dimcode[.exe]
default-plugins.json        # PluginManager §2.3，含 `runtimes`（§2.2）
plugins/<name>/             # plugin-authoring，没有生产依赖的 node_modules
LICENSE  NOTICE  release-manifest.json
```

`scripts/stage-plugins.mjs` 复制 `runtime/default-plugins.json` 和每个默认插件的目录（不含测试、`tsconfig.json`、`node_modules`），在每个有 `package.json` 的目录里按它固定的 lock 执行 `npm ci --omit=dev --ignore-scripts --no-bin-links`；`stage-release.mjs --plugins DIR` 把结果放在 `bin/` 旁边。默认插件从自己的 `node_modules` 解析 `@arcships/rutis`；它不能解析出自己的一份 Cordis，因为 rutis-runtime 拒绝 Cordis 与运行时不同的插件。插件入口是 TypeScript，放在 npm 包的 `node_modules` 下也可以：Node 运行时通过 tsx 加载它们，不用 Node 自带的去类型。查找这个文件（`<prefix>/default-plugins.json`，即 `bin/` 的上一级，或 `DIMCODE_DEFAULT_PLUGINS`）和 App Server 的 `--defaults` 参数归 PluginManager（[#485](https://github.com/arcships/dim/pull/485)）；RuntimeManager 读这个文件的 `runtimes` 一节。有 `--defaults` 且没有 `--node` 时，App Server 在运行时的 `auto` 开启或它已被记录为安装过时，用 RuntimeManager 获取它；否则行为与现在相同（显式的 `--node`、`--js-runtime`、`--python` 继续用于开发和测试）。

## 7. 安全

- 每个下载的压缩包在解压或运行之前，都按官方发布方列出的 SHA-256 校验，这个 SHA-256 从固定的 HTTPS 地址读取；npm 和 pip 的包按仓库里固定的 lock 和 requirements 校验。下载源和镜像不增加信任。
- 下载的 Python 是外部管理的（PEP 668）。获取时 RuntimeManager 在解释器的 stdlib 目录（`lib/python3.<minor>`，Windows 上是 `Lib`）写入 `EXTERNALLY-MANAGED`，位置在 `.partial` 目录里、改名之前；早于这个标记装好的下载会在下次启动时补上。它的 `[externally-managed]` 错误指向虚拟环境或 uv。`system` 解释器从不改动。RuntimeManager 自己的 `pip install --target` 不受阻挡：带 `--target` 时 pip 跳过 PEP 668 检查（pip 26.2.1，`pip/_internal/commands/install.py` 第 376-392 行）。
- 压缩包条目只解压到它的目录里：拒绝绝对路径和 `..`，也拒绝目标离开目录的链接（Windows 上，目录内的链接解压为副本）；zip 包里的链接条目一律拒绝。不保留 setuid、setgid、sticky 位（Unix 上权限最多为 `rwxr-xr-x`）。
- npm 带 `--ignore-scripts` 运行；pip 只装 wheel（`--only-binary :all:`），安装期间不运行任何包的代码。
- RuntimeManager 只写 `<home>/runtimes/`。不使用也不保存任何凭据。
- 完整性在获取时保证。之后启动时，RuntimeManager 信任自己的记录和已安装的目录，不再重新算 hash：它们在用户的 home 里，只有用户自己的进程会写；能改它们的进程，同样能改配置或插件本身。校验防的是网络上来的内容，不是 home。

## 8. 规则检查

RuntimeManager 不是 Capability：它没有 Timeline 状态，不写 Timeline 事件，也没有 Session 或 branch 部件。

| 规则 | 本文如何满足 |
| --- | --- |
| 原则 §4 第 1 条（别人的状态用命令） | RuntimeManager 拥有自己的记录和目录。PluginManager 用 `install` 请求；插件行的状态是 rutis 的，运行时的 `RuntimeStatus` 是它的连接行的，RuntimeManager 只读取。HostPlatform 在每次启动进程时通过 `ToolchainSource` 读取已安装的 bin 和归档目录；运行时的目录仍归 RuntimeManager，其他部件不写它们。 |
| 原则 §4 第 2 条（移交） | 不适用。 |
| 原则 §4 第 3 条（只有 before 回调） | 写入前没有干预。 |
| 原则 §4 第 4 条（核心不带业务含义） | 连接行是 rutis 给宿主自己的连接用的形式；rutis 的依赖等待不带 Dim 的含义。 |
| 原则 §4 第 5、6 条 | 不适用：没有 Timeline 批次，也没有内容进入模型上下文。 |
| 原则 §4 第 7 条（只用公开事件和服务） | rutis-bridge 的 `Launcher`、`pipe::greeted`、`pipe::Processes` 和 `Session`，rutis-loader 的依赖等待和 `SessionResolver`，不改也不包装。 |
| 原则 §4 第 8 条（向机制所有者提需求） | §9：对 rutis 没有；对 PluginManager 一条。 |
| 原则 §4 第 9 条（只记后续决策要用的事实） | `runtimes.json` 只记下次启动要用的：装了什么。 |
| 原则 §1 | App Server 的进程插件。 |
| ADR 002 D1（范围跟随状态） | 进程：运行时按宿主。 |
| ADR 002 D2（没有接线模块） | 没有接线：插件行通过连接行的会话与运行时相连。 |
| ADR 002 D3（依赖跟随范围） | 只有进程依赖（`managementMethods`）。 |
| ADR 002 D4（结构配置与业务配置） | `runtimes` 是默认配置文件里的产品配置；用户覆盖在 home 里。 |
| ADR 002 D5（与语言无关的接口） | 连接行和 JSON 方法。 |

## 9. 向机制所有者提出的需求

对 rutis 没有：连接行要等启动命令提供后才启动进程，行按 rutis-loader 0.19.3 已有的依赖等待来等它的会话。

**R1. PluginManager：Python 链接和运行时请求（M1）。**

| 项 | 内容 |
| --- | --- |
| 阻塞 | Python 插件的目录不在正在运行的 Python 运行时的导入路径上。 |
| 考虑过的已有机制 | 把每个目录加到 `PYTHONPATH`、每次安装重启 Python 运行时（所有 Python 行都要重启；两个插件的同名模块会冲突）。 |
| 通用能力 | PluginManager 维护 `<home>/plugins/.python/<包名>` 链接，Python 行命名为 `py:<包名>.<入口>`（§4）；开启 Python 插件而 Python 关闭时调用 `RuntimeService::install("python")`。 |
| 对序列、批次、中断、reload 的影响 | 对 Timeline 没有影响。 |
| 所有者与 issue | PluginManager，[#3](https://github.com/arcships/dim/issues/3)。 |
