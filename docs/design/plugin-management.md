# 插件管理：需求

状态：需求已定，不是设计。本文写插件管理要达到什么目的、已经定下的方向和需求；设计按这些需求修订或重写 [PluginManager](plugins/PluginManager.md) 等文档（[#3](https://github.com/arcships/dim/issues/3)、[#384](https://github.com/arcships/dim/issues/384)）。现有机制只给出入口链接。

## 目的

用户和 agent 能快速创作插件、装上就能用，并且 Dim 自身的每一部分都可以被插件配置或替换。

- **凡是 rutis 写的插件都可以替换**，包括 Session 等内核部分和各个 Capability，不只是可选插件。产品侧决定哪些不允许替换；这是产品配置，不是插件机制本身的限制。
- 内置插件尽量可以通过配置替换，能热替换的就热替换。

插件只有一个概念：一个插件是一个包，里面可以有下面一种或两种东西。

- **rutis 部件**：可执行的部分（Rust、TypeScript 或其他语言），注册工具、提示词来源、方法、服务，参与进程、Session 和分支。
- **内容**：hooks、skills、MCP 配置等，是插件的内容类别。内容向它的负责方（Hooks、SkillsCatalog、Mcp 等）登记后生效，插件停用时撤回。“hooks 插件”就是只带 hooks 内容的插件。

## 已定的方向

| 方面 | 方向 |
| --- | --- |
| 插件概念 | 只有一个插件概念，共用一套安装、启用、列表、更新、移除。hooks、skills、MCP 配置等是内容类别；内容向负责方登记，负责方不去发现或读取插件 |
| 格式 | 使用 Dim 自己的格式。与 Claude Code、Codex 等外部格式的兼容由外部去做，Dim 不读取对方的文件 |
| 安装 | 支持本地（文件）和远程（Git）；市场以后再建。插件管理不增加作用域、审批、信任等额外约束 |
| 约束 | 插件管理对插件不做限制。需要约束时，由已经在运行的插件自己施加（例如 Permission 对工具调用的决定） |
| 更新确认 | 更新需要用户确认，但这是交互界面的事情；App Server 只提供操作 |
| 默认启用 | 由默认配置文件决定 |
| 做成的标准 | agent 或用户能快速写出一个插件并热加载使用；必须重启才能生效的情况就重启。内置插件可以通过配置替换或热替换，产品侧约束哪些不能替换 |

## 需求

1. **插件是一个目录。** 来自本地路径或 Git，里面可以有代码（TypeScript、Python 等），也可以有 skills、hooks、MCP 配置等内容。
2. **装上就能用。** 安装后立即启用；可以停用、移除。插件装在本机，不分项目；仓库自带的 skills、MCP 配置等仍由各负责方的项目来源处理（[ADR 003](../adr/003-approval-scope.md)）。
3. **改完 reload 就生效，不用重启。** 宿主不监视文件，reload 操作本身就是“改了”的信号。reload 是插件换一代（[rutis usage §4](rutis-usage.md#4-lifecycle-rutis-provides)）：旧的一代卸载，它正在跑的调用由 rutis 取消；新的一代重新登记，Session 和分支里挂的部件随之更换；依赖它的插件由 rutis 重新加载；Capability 从 Timeline 恢复（[插件契约 §7.2](plugin-contract.md#72-generations-reload-and-rebuild)）。
   - 内容：插件这一行换一代，把新内容交给负责方。
   - TypeScript、Python 代码：reload 重新导入这个插件的入口模块，插件带着新代码换一代；同一运行时里的其他插件不受影响。入口模块导入的其他模块不会重新导入，它们的改动要重启运行时才生效（或者把插件代码放在入口模块里）。
   - 编进 Dim 本体的 Rust 代码不能 reload，要重新编译、重启；要换它，用第 4 条。
4. **Dim 自带的插件也能换。** 在配置里把它指向另一个插件；不允许换的，写在默认配置文件里。
5. **agent 也能装插件、reload 插件**，和用户用同一套操作；agent 的调用是工具调用，由 Permission 照常决定。

以后再做：市场（M1 之后）。

不做：按项目安装、审批与信任、文件监视、读取外部格式。

## 现有基础

- [PluginManager](plugins/PluginManager.md)：草案、P3，写于插件契约迁移之前。安全与信任部分已经符合 ADR 003；过时的是把内容发布为 `pluginContents` 服务、由负责方读取的做法，要按上面的方向重写。其中的市场部分推迟到 M1 之后。
- [#3](https://github.com/arcships/dim/issues/3)：loader 的可编辑层，作为启用状态的唯一来源；安装、启用、reload、移除写到这一层。
- [#384](https://github.com/arcships/dim/issues/384)：安装和启用插件，在 #3 之上。
- [Hooks](plugins/Hooks.md)：Ready；插件里的 `hooks.json` 由插件的内容行向 Hooks 登记（[PluginManager §5](plugins/PluginManager.md)）。
- [ADR 004](../adr/004-loader-plugin-management.md)、[插件契约](plugin-contract.md)、[How Dim uses rutis](rutis-usage.md)。

需求跟踪：[#432](https://github.com/arcships/dim/issues/432)。
