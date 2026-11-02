# RuntimeManager

RuntimeManager 提供插件代码运行所需的 Node 与 Python rutis 运行时（[#476](https://github.com/arcships/dim/issues/476)）。它是 App Server 的一个进程插件，占一行 loader 行。它获取每个开启的运行时（下载固定版本到 home，按官方校验文件校验，或使用机器上已安装的），把装了什么记在自己的文件里，然后提供运行时的启动命令。命令一到，运行时的连接行就启动进程；插件行按 rutis 的规则等待这个运行时的会话。

发布包带可执行文件、默认配置文件和默认插件，不带运行时（[发布打包](../../../release/README.md#local-packaging)）。

## 0. 实现范围

| 部分 | M1（2026-11-01） | 以后 |
| --- | --- | --- |
| Node：下载固定版本、按官方校验文件校验、按固定 lock 安装 `@arcships/rutis-runtime`、启动（§3） | 是 | |
| Python：下载固定版本、按官方校验文件校验、按固定 hash 安装 `rutis` 包、启动（§3） | 是 | |
| 两个运行时都支持 `source: system`（§2.2） | 是 | |
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
  "npmRegistries": ["https://registry.npmjs.org", "https://registry.npmmirror.com"],
  "pythonIndexes": ["https://pypi.org/simple", "https://pypi.tuna.tsinghua.edu.cn/simple"]
}
```

- Dim 固定版本号和每个平台的压缩包文件名（`darwin-arm64`、`darwin-x64`、`linux-x64`、`linux-arm64`、`windows-x64`、`windows-arm64`）。压缩包的 SHA-256 在获取时从 `checksums` 读取，即发布方自己的校验文件（nodejs.org 的 `SHASUMS256.txt`，python-build-standalone Release 的 `SHA256SUMS`），通过 HTTPS 从这个固定地址取。Dim 信任官方发布方；环境变量和镜像都不能替换这个地址。拦截这个地址的网络是用户自己的网络。
- 运行时的包沿用已有的固定位置：`runtime/package.json` 和 `runtime/package-lock.json`（`@arcships/rutis-runtime`，与宿主的 rutis 版本一致，每个包都有 npm `integrity`），以及 `runtime/python-requirements.txt`（`rutis`，带 `--hash`）。它们都编译进 Dim；升级宿主的 rutis 版本时在同一个提交里一起改。
- 下载源按顺序尝试，用第一个与官方 SHA-256 一致的文件。镜像只提供文件内容，不增加信任。环境变量把用户自己的源放在最前：`DIMCODE_NODE_MIRROR` 和 `DIMCODE_PYTHON_MIRROR`（基础 URL，后面接文件名），`DIMCODE_NPM_REGISTRY`（CLI 已在用）和 `DIMCODE_PYTHON_INDEX`。

### 2.2 配置：默认配置文件的 `runtimes` 一节

默认配置文件（[PluginManager §2.3](PluginManager.md#23-默认配置文件)）随 Dim 发布，增加：

```json
"runtimes": {
  "node":   { "source": "download", "auto": true },
  "python": { "source": "download", "auto": false }
}
```

- `source`：`download` 用上面的压缩包；`system` 用 `PATH` 上的解释器（`node`；`python3`，Windows 上是 `python`），获取时检查：Node 22.19 或更高（`@arcships/rutis-runtime` 的 `engines`）且带自己的 npm（`npm-cli.js` 在官方压缩包和安装程序放的位置；没有它的 Node 会失败，原因里写出这个路径），Python 3.12 或更高且有 pip。检查不通过就是这个运行时的失败（§3.3）。
- `auto`：`true` 每次启动都获取。`false` 在 `runtime.install`（§5）之前不动；装好后会被记录（§2.4），之后每次启动都像 `auto` 一样获取。
- 用户在 `<home>/runtimes/config.json` 里覆盖预置，形状相同，按运行时、按字段覆盖；客户端通过 `runtime.configure`（§5）写它，保持这个合并。它在 home 里，所以只有用户本地配置能改，项目配置不能。
- 配置在动作应用它的那一刻读取，而不是启动时读一次：`runtime.install` 当时重新读文件和环境（§3.3），所以 `source` 或 `auto` 的改动不必重启进程即可生效。`runtime.list` 报告的是正在生效的运行时，不是文件——改了的文件要等某个动作应用它才出现。

### 2.3 `<home>/runtimes/` 下的布局

| 路径 | 内容 |
| --- | --- |
| `node-v24.21.0-<平台>/` | 解压后的 Node |
| `node-packages-<lock 摘要>/` | `package.json`、`package-lock.json` 和 `npm ci` 装的 `node_modules/` |
| `python-3.12.15+20261003-<平台>/` | 解压后的 Python |
| `python-packages-<requirements 摘要>/` | `pip install --target` 装的 `rutis` 包 |
| `runtimes.json` | RuntimeManager 的记录（§2.4） |
| `config.json` | 用户的覆盖（§2.2），可选 |
| `*.partial/`、`*.download` | 进行中的内容；每次尝试前删除 |

每个路径都由固定版本、配置和 home 算出，所以 App Server 在 reconcile 之前就能用它们建好启动命令和行解析器，和使用配置的路径时一样；运行时进程要等 RuntimeManager 提供命令才启动。`source: system` 时，解释器路径在 App Server 建命令时从 `PATH` 查找。

启动命令就是 rutis-host 自己的启动器对这两个包用的命令：Node 在 `@arcships/rutis-runtime` 包里运行 `node --import tsx src/runner.mjs`；Python 在它的 project 里运行 `python -m rutis`，`PYTHONPATH` 上有 `rutis` 包和 project，并设 `PYTHONUNBUFFERED=1` 和 `PYTHONDONTWRITEBYTECODE=1`。每个进程的最后几个参数是它的通道、`--id <行>` 和它的 anchor。

Python 运行时的 project 目录是 `<home>/plugins/.python/`，归 PluginManager（§4）。Node 运行时的 anchor 是它的包目录里的 `package.json`。

### 2.4 `runtimes.json`

```json
{ "node":   { "source": "download", "version": "24.21.0", "sha256": "…", "packages": "node-packages-…" },
  "python": { "source": "download", "version": "3.12.15+20261003", "sha256": "…", "packages": "python-packages-…" } }
```

只由 RuntimeManager 在运行时和它的包装好之后写。`version` 对 `download` 是固定的版本；对 `system` 是解释器的版本，每次启动都检查，`runtime.list` 报告的也是它。`sha256` 是压缩包校验时用的官方 SHA-256。某一项的 source、version、packages 与固定版本和配置算出的一致、且对应目录存在，就是已安装：下次启动直接用，不再下载。`auto: false` 的运行时有记录，表示用户装过它。别的都不存；运行时是否在运行是它的连接的 `RuntimeStatus`。

## 3. 流程

### 3.1 启动

1. App Server 读固定版本和配置，用固定路径（§2.3）建启动命令，加入这些行：`RuntimeManager`（必需）、`session-node` 和 `py`（非必需）、插件行。
2. RuntimeManager 的 `apply` 在 `managementMethods` 上登记 `runtime`，提供 `RuntimeService`，然后为每个开启的运行时（`auto`，或在 `runtimes.json` 里有记录）起一个任务。
3. 每个任务获取自己的运行时（§3.2）并提供它的启动命令。连接行、以及等待它的会话的插件行随后自己启动。两个运行时互不影响：一个失败不影响另一个。

启动不等待任何运行时：没有网络的首次启动也能到达用户，插件行处于 Pending。

### 3.2 获取

每个运行时在自己的任务里：

1. 如果 `runtimes.json` 记录它已安装（§2.4），跳到第 5 步。
2. `download`：先从官方校验文件读取压缩包的 SHA-256（校验文件里没有列出的文件不下载），再按顺序从每个下载源取压缩包，写到 `<home>/runtimes/<目录>.download`，边写边算 SHA-256，用第一个一致的。一致之后才解压，解压到 `<目录>.partial/`，拒绝离开该目录的条目和指向目录外的链接。解压完成后改名为最终路径，不替换任何已校验过的内容。`system`：查找并检查解释器（§2.2）。
3. 包，先装到 `<目录>.partial/` 再改名：Node 用已校验的（或 system 的）Node 自带的 npm，对固定的 `package.json` 和 lock 执行 `npm ci --ignore-scripts --omit=dev --no-audit --no-fund --registry <r>`，按顺序尝试每个 registry；npm 按 lock 的 `integrity` 校验每个包。Python 执行 `python -m pip install --require-hashes --no-deps --only-binary :all: --target <目录> --index-url <i> -r requirements`，按顺序尝试每个 index。
4. 写 `runtimes.json`。
5. 从 RuntimeManager 的上下文提供启动命令。

不执行任何未校验的东西：运行 npm 或 pip 的解释器是已校验的压缩包里的（或用户用 `system` 选的那个），npm 关闭脚本安装并按固定 lock 校验每个包，pip 拒绝任何没有固定 hash 的文件。

### 3.3 失败与离线

获取失败的运行时在 `runtime.list` 里是 `failed`，原因列出每个下载源和它的错误。没有东西被替换：半成品目录在下次尝试时删除，之前已安装的版本（如果有）保留。需要它的插件行保持 Pending，等待它的连接行的会话。下一次尝试是下次启动或 `runtime.install`（§5）；没有自动重试。

运行期间改的配置由 `runtime.install` 应用：该调用重新读 `<home>/runtimes/config.json` 和环境，当给出的 `source` 与正在生效的不同时，撤回该运行时的启动命令、释放正在运行的连接，再按新来源获取，连接行于是用新的解释器重新启动。除此之外没有别的东西重启。配置读不出来时该调用以错误返回，运行时保持原样。没有文件监听：没有动作去应用的文件不产生任何变化。

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
| 启动命令 | `nodeRuntime`、`pythonRuntime`：连接行 `session-node` 和 `py` 注入的只在 Rust 里的进程服务；运行时获取后提供。 |
| `RuntimeStatus` | 每个连接行在以行名命名的只在 Rust 里的进程键上维护：`starting`、`ready`，或带原因的 `down`。RuntimeManager 读取它。 |
| `RuntimeService`（Rust 进程服务） | `list() -> [RuntimeEntry]`；`install(name)`：开启运行时并开始获取；正在获取或正在运行时什么都不做；获取失败后再获取一次；连接 `down` 后重新提供启动命令。用可选依赖读取，所以 RuntimeManager reload 不会让调用方 reload。 |
| `runtime.list`（管理方法） | `[{name, source, auto, version, status, reason?}]`；`auto` 是正在生效的配置，客户端因此不必自己存一份；`status` 是 `off`、`acquiring`、`failed`，或提供启动命令后连接的 `RuntimeStatus`：`starting`（连接行还没启动时也是）、`ready`，或带原因的 `down`。 |
| `runtime.install {name}`（管理方法，有修改） | `RuntimeService::install`：重读配置，`source` 变化时通过重启连接行应用（§3.3），并立即回复当前状态；配置无法读取时该调用以 `runtime_configuration` 错误返回。 |
| `runtime.configure {name, source?, auto?}`（管理方法，有修改） | 原子地写 `<home>/runtimes/config.json`，按运行时、按字段覆盖预置（§2.2），随后按 `runtime.install` 的方式应用；写不出或读不出时该调用以 `runtime_configuration` 错误返回。该运行时正在获取时拒绝，因为获取过程中它的 plan 不能变（§3.1）。 |
| 客户端 | 两种终端模式下的 `/runtime`（面板，以及打字的 `/runtime install|source|auto`），以及 `dimcode runtime list`、`install <node|python>` 和 `config NAME [--source …] [--auto …]`（[CLI 命令](../../../zh/design/cli-commands.md#插件运行时)）。 |

## 6. 发布

发布的原生压缩包和每个 npm 平台包，在 `bin/` 旁边带：

```text
bin/dimcode[.exe]
default-plugins.json        # PluginManager §2.3，含 `runtimes`（§2.2）
plugins/<name>/             # goal、web、session-title、workflow、scheduler，带生产依赖的 node_modules；plugin-authoring
LICENSE  NOTICE  release-manifest.json
```

`scripts/stage-plugins.mjs` 复制 `runtime/default-plugins.json` 和每个默认插件的目录（不含测试、`tsconfig.json`、`node_modules`），在每个有 `package.json` 的目录里按它固定的 lock 执行 `npm ci --omit=dev --ignore-scripts --no-bin-links`；`stage-release.mjs --plugins DIR` 把结果放在 `bin/` 旁边。默认插件从自己的 `node_modules` 解析 `@arcships/rutis`；它不能解析出自己的一份 Cordis，因为 rutis-runtime 拒绝 Cordis 与运行时不同的插件。插件入口是 TypeScript，放在 npm 包的 `node_modules` 下也可以：Node 运行时通过 tsx 加载它们，不用 Node 自带的去类型。查找这个文件（`<prefix>/default-plugins.json`，即 `bin/` 的上一级，或 `DIMCODE_DEFAULT_PLUGINS`）和 App Server 的 `--defaults` 参数归 PluginManager（[#485](https://github.com/arcships/dim/pull/485)）；RuntimeManager 读这个文件的 `runtimes` 一节。有 `--defaults` 且没有 `--node` 时，App Server 用 RuntimeManager 获取运行时；否则行为与现在相同（显式的 `--node`、`--js-runtime`、`--python` 继续用于开发和测试）。

## 7. 安全

- 每个下载的压缩包在解压或运行之前，都按官方发布方列出的 SHA-256 校验，这个 SHA-256 从固定的 HTTPS 地址读取；npm 和 pip 的包按仓库里固定的 lock 和 requirements 校验。下载源和镜像不增加信任。
- 压缩包条目只解压到它的目录里：拒绝绝对路径和 `..`，也拒绝目标离开目录的链接（Windows 上，目录内的链接解压为副本）；zip 包里的链接条目一律拒绝。不保留 setuid、setgid、sticky 位（Unix 上权限最多为 `rwxr-xr-x`）。
- npm 带 `--ignore-scripts` 运行；pip 只装 wheel（`--only-binary :all:`），安装期间不运行任何包的代码。
- RuntimeManager 只写 `<home>/runtimes/`。不使用也不保存任何凭据。
- 完整性在获取时保证。之后启动时，RuntimeManager 信任自己的记录和已安装的目录，不再重新算 hash：它们在用户的 home 里，只有用户自己的进程会写；能改它们的进程，同样能改配置或插件本身。校验防的是网络上来的内容，不是 home。

## 8. 规则检查

RuntimeManager 不是 Capability：它没有 Timeline 状态，不写 Timeline 事件，也没有 Session 或 branch 部件。

| 规则 | 本文如何满足 |
| --- | --- |
| 原则 §4 第 1 条（别人的状态用命令） | RuntimeManager 拥有自己的记录和目录。PluginManager 用 `install` 请求；插件行的状态是 rutis 的，运行时的 `RuntimeStatus` 是它的连接行的，RuntimeManager 只读取。 |
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
