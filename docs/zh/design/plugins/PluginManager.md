# PluginManager

状态：草案，2026-10-27 已与维护者对齐。需求：[插件管理](../../../design/plugin-management.md)（[#432](https://github.com/arcships/dim/issues/432)）。Issue：[#3](https://github.com/arcships/dim/issues/3)、[#384](https://github.com/arcships/dim/issues/384)。作用域：[ADR 002](../../adr/002-plugin-scope.md)（进程：安装按宿主进行）。语言：Rust。里程碑：§0 中标出的部分属于 M1。

PluginManager 保存一个宿主的插件配置：装了哪些插件，它们从哪里来，每个名字使用哪个插件，哪些名字被关闭。它把这份配置保存在自己的文件 `plugins.json` 中，格式是 Dim 自己的格式。它不保存运行时状态，只在内存中记下它自己拒绝的内容（§3.7）。插件是否已加载、正在运行或已失败，是 rutis 的状态，从 rutis-loader 读取。一个所有者持有什么内容（skill、MCP 服务器、hook），是该所有者的状态。PluginManager 通过向所有者下达命令来执行配置变更：插件代码交给 rutis-loader，内容交给 SkillsCatalog、Mcp 和 Hooks。每个所有者自己做决定，自己保存状态。

一个插件是一个带有清单的目录。它可以带代码（TypeScript 或 Python 写的 rutis 插件），可以带内容（skill、MCP 配置、hook），也可以两者都带。插件按**名字**识别。可以安装多个同名插件，但同一时间只使用其中一个。打开另一个，就会替换正在使用的那个。内置插件就是这样被替换的。

## 0. 实施边界

| 部分 | M1（2026-11-01） | 之后 |
| --- | --- | --- |
| `plugins.json`、默认配置文件、启动层（§2、§3.1） | 是 | |
| 从本地路径或 Git 安装、更新、移除、列出（§3.2–§3.6） | 是 | marketplace |
| 默认配置文件中的可选插件，以及插件锁定的 npm 依赖（§2.3、§3.2、§7） | 是 | |
| 插件代码：创建、打开/关闭、替换、重新加载 loader 行（§3） | 是，TypeScript 和 Python，前提是运行时已经提供（§2.5） | |
| `plugin.*` 管理方法和 `dimcode plugin` 命令（§6、A.1） | 是 | 变更的客户端通知 |
| 通过 Session 部件提供的 agent 工具（§6） | 是 | |
| 默认 CLI 加载内置 SessionTitle、Scheduler、Goal 和 Web 以及配置的默认插件（plugin-authoring） | 是 | |
| 每个插件在运行时自己登记 Settings 来源（§9 R1） | 是，内置插件和 TypeScript 插件一起 | |
| 内容种类：skill、MCP 配置、hook（§3、§9 R2） | skill 和 MCP，前提是它们的所有者按时落地；hook（Hooks 属于 M1） | |

M1 的验收是：用户或 agent 写一个 TypeScript 插件，从本地路径安装它，使用它的工具，修改它并重新加载它，变更不重启就生效；默认 CLI 启动时已加载默认插件。插件代码需要运行时，运行时由 [#476](https://github.com/arcships/dim/issues/476) 提供。

## 1. 结构

```mermaid
flowchart LR
  subgraph AS["App Server process"]
    PM["PluginManager<br/>plugins.json, plugin.* methods"]:::new
    LOADER["rutis-loader<br/>rows: built-in, default, installed"]
    RPC["Rpc"]
    OWN["Content owners<br/>SkillsCatalog, Mcp, Hooks"]
    SET["Settings"]
    CODE["Plugin code rows"]
    PT["PluginTools (Session part)<br/>plugin tool"]:::new
  end
  RPC -->|"plugin.* (managementMethods)"| PM
  PM -->|"reconcile derived layer, reload"| LOADER
  LOADER --> CODE
  PM -->|"register / withdraw (if present)"| OWN
  CODE -->|"register own Settings sources"| SET
  PT -->|"same operations"| PM
  PT -->|"config: read, update"| SET
  classDef new fill:#d3f9d8,stroke:#2b8a3e,color:#000
```

- PluginManager 是一个进程行。它是锁定的（§2.3）：它不能被关闭，也不能被替换，因为没有东西能把它重新打开。
- 它注入 loader 句柄和 `managementMethods`。它只在内容所有者存在时读取它们（`Option<Keyed<_>>`，不是门槛）：缺少某个所有者，只跳过那一种内容。
- 它向 `partRegistry` 登记一个 Session 部件 `PluginTools`。该部件向 Session 的 `toolRegistry` 登记 `plugin` 工具；每次调用都交给 PluginManager 的进程服务，`config` 交给 Settings（§6）。
- 所有者从不查找插件，也从不读取 `plugins.json`。

## 2. 数据

### 2.1 插件目录及其清单

一个插件是一个根目录下有 `dim-plugin.json` 的目录。清单中出现的键说明插件带有哪些种类。

```json
{
  "name": "my-tool",
  "description": "Adds the echo tool",
  "code": {
    "entry": "index.ts",
    "inject": ["partRegistry", "settings"],
    "provides": {},
    "instanceNames": {},
    "config": {}
  },
  "skills": "skills",
  "mcp": "mcp.json",
  "hooks": "hooks.json"
}
```

| 键 | 种类 | 含义 |
| --- | --- | --- |
| `name` | — | 插件的名字。同名插件互相替换。 |
| `version` | — | 插件自身的版本，三段以点分隔的数字，每段一位或多位。可选，可选插件（§2.3）必填，它决定安装目录名。 |
| `code` | 代码 | rutis 行的字段（[rutis 用法 §2](../rutis-usage.md#2-插件是一行实例挂载它的部件)），`id` 和 `name` 除外：`entry` 是目录内的路径；`inject`、`provides`、`instanceNames` 原样复制到行中；`config` 是行的配置，宿主的同名补丁深合并在其上（见下）。 |
| `dependencies` | 代码 | 插件声明的依赖所用的包管理器。今天唯一的值是 `"npm"`：目录中带有 `package.json` 与 `package-lock.json`，锁定的依赖在获取时安装到插件自己的目录（§3.2、§7）。声明了它却缺少其中任一文件时以 `invalid_manifest` 拒绝。 |
| `skills` | skill | 插件内的一个目录，里面是 Skill 文档，登记到 SkillsCatalog。 |
| `mcp` | MCP | 插件内的一个文件，里面是 MCP 服务器条目，登记到 Mcp。 |
| `hooks` | hook | 插件内的一个文件，里面是一份 hooks 文档，登记到 Hooks。 |

每个路径都是相对路径，在跟随符号链接之后在目录内解析；离开目录的路径在安装时和重新加载时被拒绝（`invalid_manifest`）。清单在这个边界上解析；所有者收到的是已经读好的文档。声明了 `dependencies` 的插件从自己的目录运行入口，依赖也在那里解析，符合 [TypeScript 插件编写指南](../../../typescript-plugin-authoring.md) 的要求。

**行的配置。** 行的 `config` 是清单的 `code.config` 上深合并宿主的同名补丁：对象逐成员合并，其他值（包括数组）替换清单的值。宿主在启动前设置补丁；产品不设置，所以随产品发布的插件读到清单里的值。运行候选文本的宿主把文本放在插件作者暴露的成员上，评测工具即如此（[model-prompts §8.1](../model-prompts.md#81-规则)），两臂运行同一份插件代码。

**数据目录。** pluginFiles 根目录依次是：嵌入式宿主给出的根、产品 home、宿主启动时的执行 home（`--home`）。只带数据库文件启动的宿主三者都没有，所以既没有 pluginFiles 根，也不运行 PluginManager；根目录从不取数据库所在目录或工作目录。打开一个带代码的插件时，如果 `<pluginFiles 根目录>/<id>/` 不存在，PluginManager 就创建它：`<id>` 是已安装插件的 id，或默认插件的名字，两者都按 id 规则检查（§7）。关闭或删除插件时，PluginManager 从不删除这个目录，和 Settings 文档留在 home 中一样。已存在的条目只有在它是目录、不是符号链接、并且解析后就是根目录下的它自己时才使用；否则不创建任何东西，插件的文件操作会像没有这个目录时一样失败。插件通过 `pluginFiles` 只在自己的 id 目录下写入（[进程 TypeScript §5.2](../../../design/process-typescript.md#52-public-pluginfiles-binding)）。

插件的业务配置（它的 Settings 来源）不在清单中：插件的代码在启动时向 Settings 登记自己的来源（§9 R1）。现在 TypeScript 行中的 `settings` 成员将被去掉。

仓库自己的 TypeScript 插件（`runtime/plugins/*`）用 `dim-plugin.json` 代替它们的行文件（`web.json`、`process.json`、`settings.json`）。SessionTitle 是内置插件，不是 TypeScript 包。

### 2.2 `plugins.json`

`<home>/plugins.json`，只由 PluginManager 读取和写入。它只保存配置。

```json
{
  "version": 1,
  "installed": [
    {
      "id": "my-tool",
      "name": "my-tool",
      "source": { "path": "/Users/me/src/my-tool" },
      "dir": "/Users/me/src/my-tool",
      "kinds": ["code", "skills"]
    },
    {
      "id": "hooks-plus",
      "name": "Hooks",
      "source": { "git": "https://example.com/hooks-plus.git", "ref": "main", "commit": "4f1c…" },
      "dir": "<home>/plugins/hooks-plus/4f1c…+a1b2c3d4e5f60718",
      "kinds": ["code", "hooks"]
    }
  ],
  "names": {
    "my-tool": { "use": "my-tool" },
    "Hooks": { "use": "hooks-plus" },
    "web": { "off": true }
  }
}
```

| 字段 | 含义 |
| --- | --- |
| `installed[].id` | 在已安装插件中唯一；它是记录的键，也是克隆目录的名字。必须符合 `[a-z0-9_-]{1,64}`（只用小写，所以在不区分大小写的文件系统上，两个 id 不会指向同一个目录），不能是 `builtin` 或 `default`（§7）。调用方没给时，id 是清单名字转成小写（`Hooks` → `hooks`）；转小写后和已装的重复，报 `plugin_exists`，调用方再给一个别的 id。示例中的 `hooks-plus` 是调用方给的（`--id hooks-plus`）。 |
| `installed[].name` | 安装时或上一次重新加载时清单中的名字。 |
| `installed[].source` | 本地目录用 `{path}`，原地使用，从不复制；Git 用 `{git, ref?, commit}`；默认配置文件以可选方式提供的插件（§2.3）用 `{optional}`，从它随发行提供的目录复制。 |
| `installed[].dir` | 被加载的目录：本地路径本身、`<home>/plugins/<id>/<commit>+<suffix>` 下的克隆，或 `<home>/plugins/<id>/<version>+<suffix>` 下的副本（§3.2、§7）。在后缀规则出现之前写入的记录其目录不带后缀；不做迁移。 |
| `installed[].kinds` | 安装时或上一次重新加载时清单带有的种类。 |
| `installed[].version` | `{optional}` 来源的已安装版本，取自其清单；其他来源没有该字段。 |
| `names.<name>.use` | 这个名字使用哪个插件：一个已安装的 id、`"builtin"` 或 `"default"`。不存在时：如果有该名字的内置插件，就用它，否则用该名字的默认插件。 |
| `names.<name>.off` | 用户关闭了这个名字时为 `true`。 |

没有条目的名字处于出厂状态：内置插件或默认插件处于打开状态。写入时先写到同一目录下的临时文件，在它自己的可写句柄上刷盘（`sync_all`，在 Windows 上也可用），然后改名覆盖 `plugins.json`。文件无法解析时，启动失败，并给出文件路径；PluginManager 从不覆盖一个它读不了的文件。格式带有版本号；未知版本使启动失败。

持久化的 `use: "default"` 在发行内容不再提供同名默认包时选择该名称的内置插件。
默认包仍存在时，显式选择继续使用该包。该来源过渡不重写记录；`off` 和已安装来源选择保持原义。

### 2.3 默认配置文件

随 Dim 发布，只读，用 `--defaults PATH` 交给 App Server（默认 CLI 传入其发布包中的文件；`./dev` 把这个文件和它列出的插件复制到检出目录中被忽略的 `staging/dev-plugins`，并传入该副本，使插件解析运行时唯一的一份 `@deepseek-ai/cordis`）。

```json
{
  "plugins": {
    "plugin-authoring": "plugins/plugin-authoring"
  },
  "optional": {
    "light-ocr": "plugins/light-ocr"
  },
  "locked": ["PluginManager", "Rpc", "Transport"]
}
```

- `plugins` 把每个默认插件的名字映射到它的目录，路径相对于这个文件。默认插件和其他插件一样是一个插件目录（§2.1）。
- `optional` 把每个可选插件的名字映射到它的目录，路径相对于这个文件。可选插件随发行提供，但不安装、不打开：在用户安装它之前没有任何东西读取它，`plugin.list` 会列出它供客户端提供（§3.6）。`plugin.install {source: {optional: "<name>"}}` 把该目录——从不含 `node_modules`——复制到 `<home>/plugins/<id>/<version>+<suffix>`，并在那里安装它声明的依赖（§3.2、§7）；随发行的目录保持不动、只读。新发行带有新的 `version`：下次启动时已安装的副本会在后台重新获取（§3.1），在成功之前旧副本继续使用并留在磁盘上，切换后的下一次启动删除它。`enable` 默认为 true，此后 `plugin.enable` 与 `plugin.disable` 与其他已安装插件相同；可选插件的名字在安装之前不记录 `off` 状态：`plugin.list` 把它报告为 off（§3.6）。
- 内置 SessionTitle、Scheduler 和 Goal 由原生产品 profile 安装，在这个文件中没有条目。
- `locked` 列出不能关闭、也不能切换所用插件的名字。锁定名字的其他插件仍然可以安装。锁定是产品配置；PluginManager 执行它。

### 2.4 PluginManager 不保存的状态

| 状态 | 所有者 | 读取方式 |
| --- | --- | --- |
| 有哪些行，以及它们的 `EntryStatus` | rutis-loader | `Loader::entries()`、`Loader::get(id)`、`Loader::waits`；按附录 A.1 显示 |
| 有哪些内置 rutis 插件 | App Server 的装配 | `Loader::entries()` 中 `assembly` 层的行；行 id 就是插件的名字 |
| 从插件登记的 skill、MCP 服务器、hook | SkillsCatalog、Mcp、Hooks | 所有者自己的方法 |
| 插件的业务配置 | 插件的 Settings 来源 | Settings |

### 2.5 运行时

插件代码在 Node 或 Python 的 rutis 运行时里运行。提供运行时不归 PluginManager 管：运行时怎么获取、固定哪个版本、怎么校验，运行时就绪后插件的行怎么启动，都由 [RuntimeManager](RuntimeManager.md) 决定（[#476](https://github.com/arcships/dim/issues/476)，M1）。PluginManager 只负责建行；运行时还没有时，该行在 `plugin.list` 里是 `pending`，等待运行时的连接行（`session-node` 或 `py`）。运行时本身进行到哪一步（获取中、失败、启动中、down）由 `runtime.list` 显示。reload 只重新导入入口模块（[需求第 3 条](../../../design/plugin-management.md)）。

TypeScript 的行以入口文件的绝对路径命名。Python 插件通过 PluginManager 维护的链接导入：`<home>/plugins/.python/<package>` 指向插件目录，`<package>` 是插件的 id（默认插件用名字），其中 `-` 换成 `_`；它的行名是 `py:<package>.<入口模块>`，插件在自己的包内使用相对导入。PluginManager 在打开插件时创建链接，更新后把它指向新目录，移除插件时删除它。Python 运行时从 `<home>/plugins/.python/` 导入（#476）。

打开名字本身从不在 PluginManager 自己的权限下安装运行时，启动也从不安装：运行时只在客户端的 `plugin.install`、`plugin.enable` 或 `plugin.update` 调用中请求，以及在可选插件的后台重新获取中请求（§3.1）。启动 reconcile、所有者打开时的 reconcile、reload，以及处于出厂状态的名字，都从不请求运行时。这类调用打开带代码的插件而运行时关闭时，PluginManager 向 RuntimeManager 请求它——`RuntimeService::install("node")` 或 `install("python")`（§9 R5）——并回复该行 `pending`，`waitingFor` 其连接行。它不在那里等待获取；运行时的进度从 `runtime.list` 读取。必须安装插件依赖是 PluginManager 等待 Node 的唯一场合，由 RuntimeManager 的 `installPackages` 负责该等待（§3.2）。

## 3. 流程

下面说明 PluginManager 的配置如何对应到 rutis-loader。loader 有两层。`assembly` 存放内置行。`plugins` 由 `plugins.json` 和默认配置文件推导出来；除了运行中的树，rutis 不保存它的副本。它存放正在使用的默认插件和已安装插件的行，以及对内置行的 `disabled` 覆盖。PluginManager 建的行，行 id 是 `plugin:<name>`：每个名字一行，不会和内置行的 id 冲突，因为内置行被禁用后仍留在组合结果里。

| `plugins.json` 中的名字 N | 推导出的 `plugins` 层中 |
| --- | --- |
| 使用带代码的默认或已安装插件 P | 按 P 的清单生成的行 `plugin:N` |
| 使用它的内置插件 | 无：内置行运行 |
| 关闭，或使用内置插件以外的插件 | 若有内置行 N，则给它 `disabled: true` |

配置变更写入 `plugins.json`，重新推导 `plugins` 层，并让 loader 执行 `reconcile([assembly, plugins])`。rutis 拒绝的行留在 loader 里，`plugin.list` 显示 rutis 的状态和原因，与重启后相同。行的字段不变时，重新加载是 `Loader::reload("plugin:N")`。rutis 允许一层禁用一个内置行、覆盖它的字段，但不允许改名或移除它，所以替换内置插件时，禁用它的行，并为新插件加一行。rutis 会重新加载发生变化的行的依赖方。

内容跟随同样的切换：当名字 N 不再使用 P 时，PluginManager 从每个存在的所有者那里撤回 P 的内容；当 N 开始使用 P 时，它向每个存在的所有者登记 P 的内容（§3.7）。

### 3.1 启动

1. App Server 用 home 和默认配置文件调用 PluginManager 的启动函数。该函数读取 `plugins.json` 和每个被使用插件的清单，并返回：
   - 初始的 `plugins` 层：处于打开状态的默认插件和已安装插件的行，以及每个被关闭或使用其他插件的内置名字的 `disabled: true`；
   - catalog 需要其 `instanceNames` 的名字（§4）；
   - 被关闭的内置行，App Server 把它们从变为 Active 所需的行中去掉。
2. App Server 对 `[assembly, plugins]` 进行 reconcile。被关闭的内置插件从不启动。
3. PluginManager 的行 apply，向 `managementMethods` 登记 `plugin`，向 `partRegistry` 登记 `PluginTools`，然后在激活之后，删除残留的暂存目录，以及 `<home>/plugins/<id>/` 下没有任何已安装条目引用的每个目录——Git 克隆与可选副本一视同仁。该清理在第 4 步开始前执行到底，并与每次 install、update、remove 串行（§4），因此它从不删除已改名到位、但记录尚未写入的目录。
4. 进程行就绪且内容启动完成之后，已安装的 `{optional}` 插件若记录的 `version` 与随发行清单的不同，就在后台重新获取，做法与 Git 插件的 `plugin.update` 相同：新副本进入暂存目录，安装其依赖，改名为 `<version>+<suffix>`，切换记录并 reconcile 层，旧目录留在原处交给下次启动的清理。在成功之前旧副本继续使用。失败时保留旧副本，为 `plugin.list` 记录 `update.error`，并在下次启动或 `plugin.update {id}` 时重试。重新获取不阻塞客户端接入。

默认插件或已安装插件的行失败时，启动不失败；`plugin.list` 把它显示为失败，并带上 rutis 的错误。正在使用的插件，其清单无法读取时什么都不运行：它没有行，每种内容都从每个存在的所有者那里撤回。进程行全部就绪之后、接纳客户端之前，App Server 调用 PluginManager 的内容启动步骤，让每个存在的所有者与记录一致（§3.7）。

### 3.2 安装

`plugin.install {source, id?, enable?}`。`enable` 默认为 `true`（[需求 2](../../../design/plugin-management.md)：已安装就意味着可用）。

1. 调用方给了 `id` 时，先检查它（§7），已经安装的 `id` 无论来源都直接拒绝（`plugin_exists`），不做任何获取。对 `{optional}`，在获取任何东西之前拒绝默认配置文件不提供的名字（`optional_not_found`）。
2. 获取（§7）：本地路径在原地检查；Git 来源克隆到暂存目录并读取清单；可选来源复制——不含 `node_modules`——到暂存目录并读取清单。没有给 `id` 时，id 取清单名字转成小写，同样检查并拒绝已安装的 id。
3. 依赖：清单声明 `dependencies` 时，PluginManager 调用一次 RuntimeManager 的 `installPackages(staging, cancel)`（§7、§9 R5），其间该调用的进度接收器报告 `dependencies` 阶段。Node 关闭时该调用会打开 Node 并等待其获取，只在 Node 可用后运行 npm。失败会删除暂存目录且不做任何改动：Node 无法提供时是带获取原因的 `node_unavailable`，否则是带 npm 消息的 `dependencies_failed`。
4. 把暂存目录改名为 `<home>/plugins/<id>/<key>+<suffix>`，其中 `<key>` 是 Git commit 或可选插件的 `version`，`<suffix>` 是本次获取从随机源取得的 16 个小写十六进制字符。名字不得已存在；若存在就重新抽一个后缀。任何获取都不替换或复用已有目录。任何失败都会删除暂存目录。
5. 写入 `plugins.json`，加入新条目；当 `enable` 为 true 且名字未锁定时，同时写入 `names.<name>.use = id`。
6. 如果已打开：reconcile 推导出的层（§3）。回复带上行的状态，以及被所有者拒绝的内容。

本地路径插件原地使用，其声明的依赖不会被安装：它们是用户自己的，如编写指南打包一节所述。安装名字已锁定的插件时，只记录它，不打开它，并在回复中说明这一点。

### 3.3 打开、关闭、切换

- `plugin.enable {name, use?}`：`use` 是一个已安装的 id、`"builtin"` 或 `"default"`；不给时，使用该名字当前的 `use`。对锁定的名字，当 `use` 改变时拒绝（`plugin_locked`）。写入 `names.<name>`（清除 `off`，设置 `use`），然后 reconcile 推导出的层（§3）。当该调用打开一个运行时关闭的插件时，它请求运行时（§2.5）；启动 reconcile 从不这样做。
- `plugin.disable {name}`：对锁定的名字拒绝。写入 `off: true`，然后 reconcile 推导出的层。
- `plugin.enable`、`plugin.disable` 与 `plugin.reload` 指名一个没有候选者的名字——没有已安装条目、没有默认插件、也没有内置插件——时，以 `plugin_not_found` 拒绝，与未知名字相同；当名字属于未安装的可选插件时，消息给出 `plugin.install {source: {optional: "<name>"}}`。什么都不安装，调用也不请求运行时：安装始终是显式的 `plugin.install`，`enable` 只打开已安装的东西，从不获取（§2.5）。有候选者的名字——内置、默认或已安装——不受影响。

当一个内容所有者本身被打开时（例如 Hooks），PluginManager 登记每个正在使用、并带有该种类的插件的内容（§3.7）。

### 3.4 重新加载

`plugin.reload {name}` 重新加载该名字使用的插件。宿主不监视任何文件；这个调用就是表示有东西变了的信号。

1. 再次读取清单。读取失败时，这次重新加载在任何改动之前就被拒绝（`invalid_manifest`）：插件保留上一份可用的行和内容，直到记录的下一次变更（来自任何命令）或一次重启；这两者都会再次读取清单，并在清单无法读取期间不运行该插件的任何内容（§3.1）。如果名字变了，拒绝（`name_changed`）；用户用新名字重新安装它。
2. 代码：只调用 `Loader::reload("plugin:<name>")`：没有要清的解析器缓存，运行时在每次解析时都重新描述模块。rutis 重新导入插件的入口模块（它导入的模块不会被重新导入；[需求 3](../../../design/plugin-management.md)），并开始一个新的世代；它在 Session 和分支中的部件被替换，它的依赖方被重新加载。如果清单的 `code` 字段变了，PluginManager reconcile 推导出的层，层里的这一行带着新字段。
3. 内容：把每种内容重新登记到存在的所有者；新文档替换旧文档。清单不再带有的种类被撤回。
4. 如果 `kinds` 变了，更新 `plugins.json` 中的 `kinds`。

使用内置 Rust 插件的名字不能重新加载（`not_reloadable`）：它需要重新构建；它可以被替换（§3.3）。

### 3.5 更新

Git 插件使用 `plugin.update {id, ref?}`：像 §3.2 那样获取新 commit，检查后放入一个新目录，在记录变更前在那里安装它声明的依赖，写入新的 `commit` 和 `dir`（`<commit>+<suffix>`），然后，如果插件正在使用，就 reconcile 推导出的层（入口路径变了，层里的行带着新路径）。依赖安装失败会删除新目录，保留旧 commit 且不做任何改动。旧目录留在原处，由下次启动的清理删除。对 `{optional}` 插件，`plugin.update {id}` 按当前版本重新复制随发行的目录——修复失败的重新获取，并采用新的发行版本——步骤相同：暂存、安装依赖、改名为 `<version>+<suffix>`、切换记录、正在使用时 reconcile、旧目录交给下次启动的清理；失败保留旧副本并记录 `update.error`。因此在版本未变时，新副本会得到自己的 `<version>+<suffix>` 目录，而不是替换正在使用的那一个。本地路径插件没有可更新的东西（`not_updatable`）；重新加载它即可。是否要求用户确认更新，是客户端的事。

### 3.6 移除和列出

- `plugin.remove {id}`：如果有名字在用它，这个名字就不再用它：该名字有内置插件或默认插件时，删掉 `names.<name>.use`，回到那个插件（原来是关闭的仍然关闭）；否则删掉 `names.<name>` 条目，这个名字下不运行任何插件。从 `plugins.json` 中移除条目；它的 Git 或可选目录留在原处，由下次启动的清理删除（`<home>/plugins/<id>/` 至多保留一个进程历史产生的那些副本）。本地目录从不删除。内置插件和默认插件不能移除（`not_removable`）；它们可以被关闭。
- `plugin.list {}`：每个名字一项，由 `plugins.json`、默认配置文件（`plugins` 与 `optional`）和 `assembly` 行构建，列出每个候选者以及正在使用的那一个；状态在调用时从 `Loader::get` 读取。未安装的可选插件有自己的一项：`{name, use: null, off: true, locked: false, candidates: [], optional: true}`，没有状态，供客户端提供安装。`off` 是报告，不是记录的状态——该名字下不运行任何东西，`plugins.json` 中也没有它的条目（§2.3）——`off: true` 让今天的客户端保持诚实：若为 `off: false`，它们会把它显示为运行中并提供 Disable 与 Reload，而这两者都会失败；`off: true` 则把它显示为惰性，TUI 的开关调用 `plugin.enable`，以 `plugin_not_found` 被拒绝（§3.3）。安装之后，它与其他已安装名字相同，可选标记只作为来源信息保留。已安装可选插件若随发行版本不同，带 `update: {version, error?}`：`version` 是它正迁往的发行版本，`error` 是最近一次重新获取的失败。

### 3.7 内容登记

对于 P 带有的每种内容和每个存在的所有者，PluginManager 调用该所有者的 `register({plugin, dir, document})`；撤回时调用 `withdraw({plugin})`，其中 `plugin` 是插件的 id：已安装插件的 id，或默认插件的名字（§7），因此可以安全地用在服务器 id 和文件名中。一个名字从一个插件切换到另一个插件时，撤回旧插件的内容；插件不再带有的种类被撤回。所有者把登记的内容作为自己的状态保存，持久化它，并在自己重新加载后恢复它。PluginManager 不监视所有者，也不代替它们再次登记任何东西。PluginManager 要调用某个所有者而它不存在时，跳过它；回复中列出被跳过的种类。所有者报告它持有哪些插件 id 的内容。在启动时，以及 PluginManager 打开某个所有者的名字时，PluginManager 为每个正在使用的插件登记该种类的内容，并为该所有者持有的其他 id 撤回这种内容；所有者从不查找插件。因此，所有者关闭期间被删除的插件，或进程在写入 `plugins.json` 之后、向所有者发命令之前停止而留下的内容，都不会再次运行；拒绝的原因在重启后会再次报告。

所有者校验交给它的内容。它拒绝的内容（不通过 Mcp 校验的 MCP 文档，`SKILL.md` 无法解析的 Skill）不影响插件的其他内容和代码：回复在 `refused` 中列出它，`plugin.list` 显示调用时各所有者对该插件报告的内容。PluginManager 拒绝无法读取或不是合法 JSON 的 JSON 内容文件。它把这种拒绝保存在内存中，按插件 id 和种类记录，只在该种类最近一次读取失败期间保留：之后对该种类的任何处理，只要不读取或读取成功，都会清除它（文档已交给所有者、清单不再带有该种类、该种类因所有者不在场而被跳过），撤回该种类和移除插件也会清除。`plugin.list` 在同一个 `refused` 条目中显示它，代替所有者的拒绝，且只在该种类的所有者在场时显示；`plugin.install`、`plugin.enable`、`plugin.reload` 和 `plugin.update` 的回复也在 `refused` 中列出它。启动和所有者打开时的 reconcile 中的拒绝，按插件 id、种类和错误记录为 `warn` 日志。

## 4. 时序与冲突

- PluginManager 在内存表上一步完成对 `plugins.json` 的修改，然后写文件，然后 reconcile 由它推导出的层，并向所有者下达命令。针对同一个 id 或名字的第二个操作能看到第一个操作的变更（`plugin_exists`，或者它在新状态上执行）。推导层和 reconcile 一次执行一个，每次都按当时的记录推导，所以最后 reconcile 的层来自最新的记录。rutis-loader 一次执行一个自己的操作。
- 插件的每次文件系统改动——暂存、克隆、复制、改名、依赖安装，以及启动时的清理——都在一个队列里一次执行一个。因此清理从不落在一次获取的改名与它的记录写入之间，也从不删除另一个操作即将引用的目录。
- rutis 拒绝的行或某个所有者拒绝的命令，不改变已写入的配置；该项在 `plugin.list` 中显示这次拒绝（例如，新插件失败，因为另一个不同名的插件提供了同一服务）。
- `instanceNames`：rutis 需要在运行时启动之前，在 catalog 中拿到插件的部件在 Session 和分支中提供的名字（[rutis 用法 §2](../rutis-usage.md#2-插件是一行实例挂载它的部件)）。安装、切换或重新加载到一个 `instanceNames` 不在运行中 catalog 内的插件时，记录变更并回复 `restartRequired`；该行在下次启动时创建。只登记工具、方法或提示来源的插件没有这个限制。
- 正在使用时的重新加载或替换：旧世代卸载，rutis 取消它正在运行的调用，使用它的进行中轮次可能失败（[ADR 002](../../adr/002-plugin-scope.md) 的后果）。这一点写入文档，不加以阻止。

## 5. 上下文映射

PluginManager 除 `plugin` 工具外不向模型上下文加入任何自己的文本。插件的工具和提示来源通过它们自己的登记到达模型（[ADR 006 D4](../../adr/006-plugin-contract.md)）。该工具的文本按[面向模型的提示词 §4](../model-prompts.md#4-协议与策略)归类。声明的 schema 是 protocol，“Manage Dim plugins.” 也是 protocol，因为它陈述工具的触及范围；它携带的单行命令表（连同命令名与每个命令的参数文法）同样是 protocol，因为工具按原样解析它们。“Run `help <command>` for a command's details.” 是 policy 指引，其中 `help <command>` 是 protocol，因为它点明要调用的命令；这句话还没有工厂配置成员，在第一次为它评估候选的改动里获得成员。usage 文本中的 `Usage:` 行是 protocol，usage 行之后的句子以及 `help` 中说明命令做什么、不碰什么的句子是 policy。格式错误的命令，其错误结果携带命令表或该命令的 usage；被拒绝的调用，其错误携带错误码（A.2）——两者都是 protocol。完成的调用结果说明变更了什么、状态是什么，以及适用时的 `restartRequired`；这也是 protocol，因为模型据此决定下一步。

## 6. 接口（摘要）

- 管理方法，命名空间 `plugin`（A.1）：`list`、`install`、`enable`、`disable`、`reload`、`update`、`remove`。
- CLI：`dimcode plugin list | install <path|git-url> [--ref R] [--id ID] [--no-enable] | install --optional <name> [--id ID] [--no-enable] | enable <name> [--use ID|builtin|default] | disable <name> | reload <name> | update <id> | remove <id>`。
- 来自 `PluginTools` 的 agent 工具：一个工具 `plugin`，只有一个字符串参数 `command`，写法与 CLI 参数相同：空白分隔单词，单引号或双引号把内容合成一个单词（含空格的路径要加引号），反斜杠是普通字符，所以 Windows 路径照原样写：

  | 命令 | 作用 |
  | --- | --- |
  | `list`、`install …`、`enable …`、`disable <name>`、`reload <name>`、`update <id>`、`remove <id>` | 调用同名的 `plugin.*` 方法，参数与 CLI 相同；相对路径按 Session 的工作目录解析。结果和错误相同。 |
  | `config <name>` | `<name>` 所用插件登记的声明式（JSON Schema）Settings 来源（R4）；Rust 类型的来源（例如 Mcp 的）不经这里提供，也不列出：每个来源的用户文档、revision、schema 和仅限用户的字段。schema 标为 `writeOnly` 的成员（密钥，[settings-service §1.2](../appserver/settings-service.md#12-决定密钥归其所有者)）不出现在文档中，有值时列在 `writeOnlySet` 里。没有运行的插件没有登记来源。客户端的对应物是 `settings.sources`（[settings-service §4.3](../appserver/settings-service.md#43-管理方法-settings客户端路径)），它只列出声明。 |
  | `config <name> set <key> <value>` | 通过 Settings 替换某个来源的一个顶层成员：读取用户文档，把 `<key>`（插件有多个来源时写成 `<source>.<key>`）设为 `<value>`（能解析为 JSON 就按 JSON，否则按字符串），再用读到的 revision 更新。revision 和 schema 由 Settings 检查；这是所有者写入，所以仅限用户的字段会被拒绝，本身是或含有 `writeOnly` 成员的成员也会被拒绝。所有者在需要时读取自己的来源，所以新值不用重启就生效。行的结构配置不在这里修改。 |
  | `help [command]` | 命令列表，或一个命令的用法和细节。 |

  工具描述只带一行命令列表；细节由 `help` 给出，格式错误的命令在错误结果里带上该命令的用法。它是工具调用：Permission 对每一次调用做决定（[需求 5](../../../design/plugin-management.md)、[ADR 003](../../adr/003-approval-scope.md)）。它声明的 risk 是 `persistent_configuration`；`list` 和 `help` 按 `read_filesystem` 准入（R3）；`config` 的文档可能含有密钥和用户内容，所以按声明的 risk 准入，和这些来源自己的工具一样。
- 插件编写：默认插件 `plugin-authoring` 带一个 Skill `plugin-authoring`，教 agent 编写 TypeScript 插件并用这个工具安装。它只有内容（§2.1），和其他默认插件一样可以关闭。
- PluginManager 调用的所有者操作：SkillsCatalog、Mcp 和 Hooks 上的 `register` / `withdraw`（§9 R2）。

## 7. 安全

安装和打开是通过管理方法或工具调用完成的宿主配置；已启用的插件代码运行时不需要宿主本地审批（[ADR 003](../../adr/003-approval-scope.md)）。PluginManager 不增加作用域、审批或信任步骤（[需求](../../../design/plugin-management.md)）。它守护的是获取过程本身：

- Git 来源只接受 `https://` 或 `ssh://` URL；其他协议（`file://`、`ext::`、scp 风格的路径）以及以 `-` 开头的参数，在 Git 运行之前就被拒绝。克隆时关闭子模块（`--no-recurse-submodules`），关闭 hook（`core.hooksPath` 设为一个空目录），设置 `protocol.file.allow=never`，并且不弹出终端提示。
- **路径。** 所有为插件动文件系统的操作都用同一条规则：安装（暂存、克隆、改名）、更新、移除和启动时的清理。在任何文件系统改动之前，PluginManager 检查两件事：id（无论是调用方给的，还是取自清单名字）符合 `[a-z0-9_-]{1,64}`，且不是 `builtin` 或 `default`；它将要创建、改名或删除的每个目录，解析后恰好是 `<home>/plugins/.staging/<random>`、`<home>/plugins/.python`、`<home>/plugins/<id>`、`<home>/plugins/<id>/<key>` 或 `<home>/plugins/<id>/<key>+<16 个小写十六进制字符>` 之一（`.staging` 和 `.python` 不可能是 id，因为 id 里没有点）。不带后缀的 `<key>` 只在作为现有记录指名的目录以及清理时才被接受。`.python` 中的 Python 链接只作为链接被替换或删除，从不改动它们指向的内容。检查不通过时，操作以 `invalid_id` 或 `path_outside_plugins` 被拒绝，什么都不改：不建目录、不写 `plugins.json`、不发 loader 命令。本地路径插件自己的目录从不被创建、改名或删除，所以这条规则不适用于它。
- **进程运行期间不删除任何副本。** install、update、后台重新获取与 remove 都把被替换的目录留在原处；只有启动清理删除没有已安装条目引用的目录，且它在当次启动的任何获取之前运行（§3.1）。因此读过描述的任务所指名的目录会存在到进程退出，即使插件在任务开始后被更新或移除。代价是磁盘：被替换的副本留到下次启动。曾经考虑过来自 Modality 的租约，予以拒绝：Modality 将不得不寻址 PluginManager 的状态，而[原则 §4 第 7 条](../../../architecture/rust-agent/principles.md)禁止这样做；而且崩溃仍然会把副本留给启动清理。
- 克隆先放到 `<home>/plugins/.staging/<random>`，只有在清单和路径检查通过后才改名放到正式位置。失败时删除暂存目录；启动时删除残留目录，以及没有条目引用的克隆或副本。
- 插件的依赖只来自它自己的 `package-lock.json`，由 RuntimeManager 在暂存目录中以 `npm ci --omit=dev --ignore-scripts --no-audit --no-fund` 安装，因此不会运行任何包的 lifecycle 脚本，也不会解析锁之外的依赖。registry 遵循 RuntimeManager 的规则（`npmRegistries`，然后 `DIMCODE_NPM_REGISTRY`，§9 R5）。随发行的可选目录从不就地安装，也从不被修改。
- 清单中的任何路径在跟随符号链接之后都不能离开插件目录；Git 克隆内指向克隆外部的符号链接被拒绝。
- `plugins.json` 以原子方式写入（§2.2）。

## 8. 规则检查

PluginManager 不是 Capability：它没有 Timeline 状态，不写 Timeline 事件，也没有分支部件。

| 规则 | 本文档如何遵守 |
| --- | --- |
| Principles §4 规则 1（对他人状态使用命令） | Loader 行是 rutis 的状态，内容是其所有者的状态；PluginManager 只向它们下达命令，并在被请求时读取它们的状态。 |
| Principles §4 规则 2（交接） | 不适用：没有输入交接或 Timeline 交接。 |
| Principles §4 规则 3（只有 before 回调） | 没有写入之前的干预。 |
| Principles §4 规则 4（核心中没有业务含义） | 所有者的 `register`/`withdraw` 操作和 Settings 登记（§9）不带插件管理的含义；任何调用方都可以使用它们。 |
| Principles §4 规则 5（批次） | 不适用。 |
| Principles §4 规则 6（上下文来自已交接的事件） | 除工具结果外，没有东西进入模型上下文（§5）。 |
| Principles §4 规则 7（只用公开事件和服务） | rutis-loader 的公开 `Loader` API、`managementMethods`、`partRegistry`、`toolRegistry` 以及所有者的公开服务。 |
| Principles §4 规则 8（向机制所有者提出需求） | §9：Settings、SkillsCatalog、Mcp、Hooks、ToolExecutor 和 Permission；没有向 rutis 提出的需求。 |
| Principles §4 规则 9（只保存之后的决定会用到的事实） | 配置保存在 `plugins.json` 中，这是唯一的记录；运行时状态从其所有者读取。 |
| Principles §1（Capability 与 App Server 的边界） | App Server 的一个进程插件；不是 Capability。 |
| ADR 002 D1（作用域跟随状态） | 进程：安装按宿主进行。`PluginTools` 是 Session 部件，只是因为工具按 Session 登记。 |
| ADR 002 D2（没有接线模块） | 没有接线：通过所有者的登记操作调用它们。 |
| ADR 002 D3（依赖跟随作用域） | 只有进程依赖；所有者是可选读取，所以它们重新加载时不会重新加载 PluginManager。 |
| ADR 002 D4（结构配置与业务配置） | 结构配置（已安装、使用中、已关闭）在 `plugins.json` 中；业务配置留在每个插件的 Settings 来源中，由插件自己登记。 |
| ADR 002 D5（语言中立的契约） | 方法和所有者操作都是 JSON 契约。 |
| ADR 006 D3，由 ADR 007 纠正（插件的代码是一行） | 每个带代码的插件是一个进程行，由 PluginManager 的命令创建、关闭和重新加载；内容不是行（[ADR 007](../../adr/007-plugin-configuration.md)）。 |

## 9. 向机制所有者提出的需求

没有向 rutis 提出的需求：用到的每个 loader 操作在 rutis-loader 0.19.3 中都已存在。

**R1. Settings：在运行时登记来源（M1）。**

| 项 | 内容 |
| --- | --- |
| 阻塞点 | Settings 来源在 App Server 构建 Settings 时就固定了：内置 Rust 所有者在代码中列出（`.reserve::<T>()`），TypeScript 行的 `settings` 成员在启动时读取（`.declare()`）。在运行时安装、替换或移除的插件无法添加或撤回它的来源。 |
| 已考虑的现有机制 | 由 PluginManager 从清单登记来源（业务配置不属于插件管理，ADR 002 D4）；每次安装都重启（违反需求 3）。 |
| 通用能力 | 在具名 `settings` 服务上提供 `settings.register(declaration)`，声明内容与现在的 `settings` 成员相同（`source`、`userFile`、`projectFile?`、`schema`、`userOnly`）。登记属于调用方，在调用方卸载时撤回；同一来源名的第二次登记失败。每个所有者，不论是内置 Rust 还是 TypeScript，都在自己的 `apply` 中登记来源。不论来源是否已登记，存储的文档都留在 home 中。硬编码的 `.reserve::<T>()` 和 `.declare()` 路径被移除。 |
| 对 sequence、批次、中断、重新加载的影响 | 对 Timeline 没有影响。重新加载时由新世代再次登记。 |
| 所有者与 issue | Settings（App Server）；将随 #384 提交。 |

**R2. SkillsCatalog、Mcp、Hooks：登记插件内容并保存它（三者均为 M1；SkillsCatalog 和 Mcp 若按时落地）。**

| 项 | 内容 |
| --- | --- |
| 阻塞点 | 所有者只读取用户来源和项目来源；没有办法把插件的内容交给它们。 |
| 已考虑的现有机制 | 由所有者读取一个 `pluginContents` 服务（所有者会去查找插件；已否决）；把内容作为 loader 行（Dim 的产品状态放进 rutis；已否决）。 |
| 通用能力 | 一个进程操作 `register({plugin, dir, document})`，用于添加或替换 `plugin`（插件 id，§3.7）的内容，以及 `withdraw({plugin})`，再加上读取所有者为 `plugin` 拒绝了什么。所有者把登记的内容作为自己的状态保存，在自己的存储中持久化，并在自己重新加载后恢复它。Mcp 用 `plugin` 作为服务器 id 的前缀（`<plugin>:<server>`）；Hooks 把来源命名为 `plugin:<plugin>`，并给命令设置 `DIM_PLUGIN_ROOT = dir`；SkillsCatalog 读取 `dir` 下的 Skill。 |
| 对 sequence、批次、中断、重新加载的影响 | 与每个所有者现有的来源相同：一次变更是一次数据更新，由所有者用自己的方式通告。 |
| 所有者与 issue | SkillsCatalog、Mcp、Hooks（[Hooks](Hooks.md)）。 |


**R3. ToolExecutor 与 Permission：单次调用的 risk（M1）。**

| 项 | 内容 |
| --- | --- |
| 阻塞点 | 工具的 risk 固定在定义里，所以一个同时有只读命令和修改命令的工具只能按最高 risk 准入：`plugin list` 在 workspace-write 下会请求审批，在 read-only 下会被拒绝。 |
| 已考虑的现有机制 | 每个操作一个工具（即被替换的七个工具；对模型来说太多）；`execute_process` 加 `command` 输入（Permission 会把 shell 命令规则用到插件命令上）。 |
| 通用能力 | 工具处理器可以给出单次调用的 risk；Approval 和 Permission 按它准入该调用，没有给出的按定义里的 risk 准入。 |
| 对顺序、批次、中断、重载的影响 | 无：准入顺序和事件不变。 |
| 所有者与 issue | ToolExecutor 与 Permission（capabilities）；随 #384。 |

**R4. Settings：插件登记了哪些来源（M1）。**

| 项 | 内容 |
| --- | --- |
| 阻塞点 | Settings 知道每个来源的名字和 schema，但不知道是哪个插件登记的，所以 `plugin config <name>` 找不到插件的来源。 |
| 已考虑的现有机制 | 在 manifest 里列出来源（业务配置不属于插件管理，R1）；按插件名给来源命名（不是规则；一个插件可以登记多个来源）。 |
| 通用能力 | Settings 在每个登记上记下登记插件的 fiber 身份，并列出某个 fiber 登记的声明式来源。PluginManager 通过 loader 找到名字所用行的 fiber。 |
| 对顺序、批次、中断、重载的影响 | 无。重载的行保持 fiber 身份并重新登记。 |
| 所有者与 issue | Settings（App Server）；随 #384。 |

**R5. RuntimeManager：插件的 npm 依赖与运行时请求（M1）。**

| 项 | 内容 |
| --- | --- |
| 阻塞点 | `RuntimeService` 只提供 `list` 与 `install`：PluginManager 无法按插件的锁运行 npm，运行时请求目前只为 Python 的链接存在（[RuntimeManager](RuntimeManager.md) §4、§9 R1），调用方也没有办法在运行 npm 之前等待 Node 的获取。 |
| 已考虑的现有机制 | PluginManager 自己运行 npm（重复 RuntimeManager 的 registry、包与失败规则；已拒绝）；要求用户先安装 Node（Python 路径已经会请求它的运行时）。 |
| 通用能力 | 进程 `RuntimeService` 上的 `installPackages(dir, cancel)`：Node 关闭时打开 Node 并用 RuntimeManager 自己的状态等待其获取，然后在 `dir` 内用受管（或 `system`）Node 自带的 npm 按 `dir/package-lock.json` 运行 `npm ci --omit=dev --ignore-scripts --no-audit --no-fund --registry <r>`，registry 依次取自 `npmRegistries` 与 `DIMCODE_NPM_REGISTRY`，npm 退出即返回。错误：Node 无法提供时是带获取原因的 `node_unavailable`，以及 `npm_unavailable`、`install_failed {reason}`、`cancelled`。Python 路径已有的 `install(name)` 请求扩展到 `node`，只用于在 Node 关闭时打开插件的客户端调用，以及可选插件的后台重新获取——从不在启动、所有者打开时的 reconcile、reload 或出厂状态的名字上使用。 |
| 对顺序、批次、中断、重载的影响 | 对行没有影响：安装在目录改名到位之前完成，被取消或失败的安装会删除该目录。 |
| 所有者与 issue | RuntimeManager（App Server）；随 #476。 |

## 附录 A. 接口细节

### A.1 方法（`managementMethods`，命名空间 `plugin`）

| 方法 | 参数 | 结果 |
| --- | --- | --- |
| `plugin.list` | `{}` | `[{name, use?, off, locked, optional?, update?: {version, error?}, candidates: [{id \| "builtin" \| "default", source?, kinds}], status?, error?, waitingFor?, refused?}]` |
| `plugin.install` | `{source: {path} \| {git, ref?} \| {optional: "<name>"}, id?, enable?}` | `{id, name, enabled, status?, skipped?: [kind], refused?: [{kind, error}], restartRequired?}` |
| `plugin.enable` | `{name, use?}` | `{status?, skipped?, refused?, restartRequired?}` |
| `plugin.disable` | `{name}` | `{}` |
| `plugin.reload` | `{name}` | `{status?, skipped?, refused?, restartRequired?}` |
| `plugin.update` | `{id, ref?}` | `{commit, status?, skipped?, refused?, restartRequired?}` |
| `plugin.remove` | `{id}` | `{}` |

名字在用的插件带代码时，`status` 是该行在 `Loader::get` 中的 `EntryStatus`：`Inactive` → `pending`（附带 `waitingFor`，即 `Loader::waits` 给出的服务；等待运行时的行给出运行时的连接行 `session-node` 或 `py`），`Unresolved` → `failed`（附带 `error`），`Running` → `running`，`Stopped` → `stopped`。关闭的名字没有在用的插件，显示 `off`，不显示状态。`install` 和 `update` 通过调用的进度接收端报告阶段 `fetch`、`check`、`dependencies` 和 `load`；插件未声明依赖时没有 `dependencies`。

客户端 RPC 边界对来源、状态错误和内容拒绝的投影，以及一次拒绝可能携带的全部 reason，见 A.2。

### A.2 错误

`plugin_exists`、`plugin_not_found`、`invalid_id`、`path_outside_plugins`（两者都在任何文件系统改动之前，§7）、`plugin_locked`、`not_removable`、`not_reloadable`、`not_updatable`、`name_changed`、`invalid_manifest`（带字段和原因）、`invalid_source`（协议或参数被拒绝）、`acquire_failed`（Git 的消息）、`optional_not_found`（默认配置文件不提供该名字的可选插件）、`node_unavailable`（无法为依赖安装提供 Node）、`dependencies_failed`（npm 的消息）、`record_unreadable`（启动时）。配置写入之后 loader 或所有者的拒绝不是这次调用的错误：它在结果和 `plugin.list` 中报告。

这些 code 在各客户端之间保持稳定。客户端 RPC 错误用稳定、可操作的描述指明其 code，不包含所有者的原始诊断消息，也不回显请求字段；上面的描述指认的是所有者的错误，并不授权发布机密。

客户端 RPC 边界省略 Git 来源的 userinfo、query 和 fragment，保留仓库路径、ref 和 commit。持久化的来源不变，仍可用于 update。只有目录候选来源、顶层状态错误和直接的内容拒绝错误会被投影；插件自己的 settings 不被遍历。状态错误在这个边界上携带通用消息，包括在变更回复中：任意的 Git、loader 和清单文本可能包含凭证。

内容的拒绝在原因带有静态 reason 时，以该 reason 到达客户端——reason 来自所有者，或者来自 PluginManager 自己读取内容文件时的拒绝；否则以通用的 "Plugin content was refused" 到达。reason 永远不携带源位置或原始文本；原始文本（连同其中的路径和 URL）只留在服务端日志和面向模型的 `plugin` 工具里。

| 所有者 | 种类 | 原因 | Reason |
| --- | --- | --- | --- |
| PluginManager | `mcp` | 内容文件不是合法的 JSON | the file is not valid JSON |
| PluginManager | `mcp` | 内容文件读不出来 | the file could not be read |
| PluginManager | `hooks` | 内容文件不是合法的 JSON | the file is not valid JSON |
| PluginManager | `hooks` | 内容文件读不出来 | the file could not be read |
| McpSettings | `mcp` | 文档不是 JSON 对象 | MCP document must be a JSON object |
| McpSettings | `mcp` | 文档没有 `mcpServers` 对象 | MCP document must contain an mcpServers object |
| McpSettings | `mcp` | `mcpServers` 不是 JSON 对象 | mcpServers must be a JSON object |
| McpSettings | `mcp` | 某个服务条目的 id 为空 | MCP server entry id is empty |
| McpSettings | `mcp` | stdio 条目的 `cwd` 不是插件目录内的目录 | MCP plugin cwd must be a directory inside the plugin |
| HooksSettings | `hooks` | 文档不是合法的 hooks 文档 | invalid hook document |

这个表就是契约：客户端按精确的消息匹配一次拒绝。skills 和 UI 两种类没有 reason，因为它们的拒绝会指名文件和路径。内容文件不存在不在表里：`Manifest::read` 会解析每个内容路径，并在 PluginManager 读取之前以 `invalid_manifest` 拒绝缺失的文件。这个表列出插件自身内容能产生的全部 reason；线上还可能出现来自 Dim 自身注册状态的静态消息——例如 `hooks-plugins.json` 损坏时的 `invalid hook plugin registration`——它不在这个表内，客户端必须把任何它不认识的消息按通用消息显示。

## 附录 B. 冲突表

| 场景 | 结果 | 由谁决定 |
| --- | --- | --- |
| 同一个 id 安装两次 | 第二次看到第一次的条目：`plugin_exists` | PluginManager |
| 不同名的插件提供一个已有提供者的服务 | rutis 使新行失败；`plugin.list` 显示错误 | rutis |
| 替换一个被其他插件依赖的内置插件 | 内置行关闭，新行启动；rutis 重新加载依赖方 | rutis |
| Session 正在使用插件的工具时重新加载 | 旧世代卸载，调用被取消，新部件被挂载 | rutis、实例的挂载方 |
| 安装时内容所有者不存在 | 跳过该种类并报告；PluginManager 打开该所有者时登记 | PluginManager、所有者 |
| 内容所有者重新加载 | 它恢复自己已登记的内容 | 所有者 |
| 所有者关闭期间插件被删除 | 所有者被打开时或下次启动时撤回它的内容 | PluginManager |
| `instanceNames` 不在运行中的 catalog 内 | 记录配置，`restartRequired`；该行在下次启动时创建 | PluginManager |
| Git 安装过程中进程停止 | 下次启动时删除暂存目录；`plugins.json` 未被写入 | PluginManager |
| 安装默认配置文件不提供的可选插件 | `optional_not_found`；什么都不创建 | PluginManager |
| 打开、关闭或重新加载一个没有候选者的名字 | `plugin_not_found`，与未知名字相同；对未安装的可选插件，消息给出 `plugin.install {source: {optional: <name>}}`；什么都不安装，也不请求运行时 | PluginManager |
| 已安装可选插件的随发行版本变化 | 后台重新获取；只有新副本就绪后才切换记录，此前旧副本继续服务，并留在磁盘上直到下次启动的清理 | PluginManager |
| 后台重新获取或可选插件的 `plugin.update` 失败 | 旧副本保留并继续服务；`plugin.list` 显示 `update.error`；下次启动或 `plugin.update {id}` 重试 | PluginManager |
| 任务持有描述期间更新或移除 | 旧目录留到下次启动；任务继续读它 | PluginManager |
| 新获取要用的目录名已存在 | 重新抽取 `<suffix>`；从不替换或复用 | PluginManager |
| 依赖安装过程中进程停止 | 下次启动时删除暂存目录；`plugins.json` 未被写入 | PluginManager |
| 更新时依赖安装失败 | 删除新目录，保留旧 commit、旧目录与记录 | PluginManager |
| `plugins.json` 写入之后、loader 或所有者命令之前进程停止 | 下次启动时根据 `plugins.json` 构建行，并让所有者与它一致 | PluginManager 启动 |

## 附录 C. 验收（M1）

1. 使用隔离的 home，`dimcode` 启动时已加载 plugin-authoring。`dimcode plugin list` 显示它和包括 SessionTitle、Scheduler、Goal、Web 的内置插件。Workflow 仅在显式安装和启用后出现。
2. `dimcode plugin install ./my-tool`（一个登记一个工具的 TypeScript 插件）使该工具在新的轮次中对模型可见，不需要重启。
3. 编辑 `index.ts` 并运行 `dimcode plugin reload my-tool`，会改变该工具在已打开 Session 中的行为。
4. agent 通过 `plugin` 工具完成步骤 2–3，每次调用都由 Permission 决定；`plugin config goal set maxContinuations 5` 不用重启就改变 goal 的设置。
5. `dimcode plugin disable web` 移除 web 的工具；`enable web` 把它们恢复；该状态在重启后保留。
6. 安装一个与内置插件同名的插件并启用它，会替换该内置插件；`enable <name> --use builtin` 切换回去。
7. `disable PluginManager` 被拒绝，错误为 `plugin_locked`。
8. 使用 `file://` URL 的 Git 安装被拒绝；失败的克隆不会在 `<home>/plugins` 下留下任何东西；id 为 `../x` 的安装以 `invalid_id` 被拒绝，什么都不创建。
9. `dimcode plugin install <git-url>` 安装一个测试用仓库并打开其插件；仓库有第二个提交后，`dimcode plugin update <id>` 把新提交加载到新的 `<commit>+<suffix>` 目录；旧目录留到下次启动，由它的清理删除。
10. 默认插件的 Settings 来源在它启动时出现，在它被关闭时消失。
11. 从本地路径安装的 Python 插件能登记它的工具，reload 后入口模块的修改生效（Python 运行时由 [#476](https://github.com/arcships/dim/issues/476) 提供）。
12. 在新 Session 中 agent 能列出 `plugin-authoring` Skill；让它编写一个带 echo 工具的插件并安装，它不读 Dim 的源码就能完成。
13. 默认配置文件提供的可选插件在 `plugin.list` 中以 `optional: true` 出现且没有状态；`dimcode plugin install --optional <name>`（一个 fixture 插件）把它的目录复制到 `<home>/plugins/<id>/<version>+<suffix>`，在那里安装其锁定的 npm 依赖，并打开它；随发行的目录中没有 `node_modules` 且未被修改；`disable` 与 `enable` 与其他已安装插件相同；移除把副本留到下次启动，由它的清理删除，并留下随发行的目录。
14. 通过 `plugin.enable` 或 `plugin.install` 在 Node 关闭时打开一个 Node 插件，会请求运行时并以 `pending` 和 `waitingFor` 回复；运行时启动后，该行无需另一条命令即可运行。同一配置的普通启动与 reload 都不请求运行时。
15. 声明 `dependencies` 的 Git 插件在记录变更前把依赖安装到它的克隆中；安装失败保留旧 commit 且不留新目录；声明了依赖却没有 `package-lock.json` 的清单以 `invalid_manifest` 失败。
16. `dimcode plugin install --optional <未知名字>` 以 `optional_not_found` 失败且什么都不创建。
17. 已安装可选 fixture 的随发行版本在两次启动之间变化：第二次启动在后台重新获取到新的 `<version>+<suffix>` 目录，记录与 `dir` 切换，插件继续从旧副本服务；两个目录都留到下次启动，由它的清理删除旧的；依赖安装失败时旧副本保留，`plugin.list` 显示 `update.error`，直到下次启动或 `plugin.update <id>` 修复。
18. 对可选 fixture 在同版本上运行 `plugin.update <id>` 会创建第二个 `<version>+<suffix>` 目录并切换记录；旧目录在下次启动的清理之前保持不变。
19. 在同一进程内先移除、再安装同一版本，会得到新的 `<version>+<suffix>` 目录；两者都留到下次启动，由它的清理只删除没有条目引用的那一个。
20. `plugin.enable`、`plugin.disable` 与 `plugin.reload` 指名一个未安装的可选 fixture 时，以 `plugin_not_found` 拒绝；消息给出 `plugin.install {source: {optional: "<name>"}}`；什么都不安装，也不请求运行时。没有已安装条目的默认 fixture 名字仍然可以被关闭和打开。
21. Phase 2 的 `dim-cli` 测试把包含未安装可选 fixture 项——`{name, use: null, off: true, locked: false, candidates: [], optional: true}`——的 `plugin.list` 回复反序列化为今天的 `PluginItem`，证明形状保持可解码。
22. TUI 的 `/plugins` 面板与 Web 设置页为 `optional: true` 的项提供 Install 动作，调用 `plugin.install {source: {optional: "<name>"}}`；TUI 对该项的开关调用 `plugin.enable`，显示第 20 项的拒绝；Desktop 解码同样的新增字段，无需改动。

## 附录 D. 待对齐问题

本设计没有。运行时获取，包括 Python 插件怎么从它的目录导入模块，见 [#476](https://github.com/arcships/dim/issues/476)。
