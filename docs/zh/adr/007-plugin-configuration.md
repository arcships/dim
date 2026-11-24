# 007：插件配置归 PluginManager；状态归 rutis 和内容负责方

[English](../../adr/007-plugin-configuration.md)

决策状态：已由仓库所有者接受（2026-10-27）。实现：未开始；设计见
[PluginManager](../design/plugins/PluginManager.md)，由 [#3](https://github.com/arcships/dim/issues/3) 和
[#384](https://github.com/arcships/dim/issues/384) 跟踪。

与其他决策的关系：

- 纠正 [ADR 002](002-plugin-scope.md) D4：结构性插件配置不放在 loader 的可编辑层（issue #3），而放在
  PluginManager 自己的记录里（D1）。D4 对结构性配置和业务配置的划分不变。
- 纠正 [ADR 006](006-plugin-contract.md) D3 第一点里的“它的配置、启用和重载都是这一行的”：插件的代码是一行，
  重载是这一行的；但插件是否安装、是否在用、是否关闭，是 PluginManager 的配置，插件带的内容也不是行
  （D2、D3）。D3 的其余部分不变。
- 撤回 [ADR 001](../../adr/001-harness-foundation.md)（开头、交付表和配置说明）里为 #3 记下的方向：“结构性插件配置使用持久化的
  可编辑层和 loader 操作”。
- 保持 [ADR 003](003-approval-scope.md)：启用的插件代码运行时不需要宿主本地批准。

## 背景

Issue #3 定下：loader 的可编辑层及其持久化，是插件启用、停用和配置的唯一来源。第一版插件管理设计（#466）和
它的实现（#470–#474）照此去做：可编辑层是安装记录，插件内容（skills、MCP 配置、hooks）做成 loader 的
“内容行”。维护者否定了这个模型，六个 PR 都已关闭。问题有两个：

- Dim 的产品状态被存成一个基础库的行：装了什么、从哪里装的，是 Dim 自己的数据；可编辑层是 rutis 留给宿主
  配置行的地方。
- 不是代码的内容，只为了让 loader 能持有，就被做成了行，使 skills、MCP 配置和 hooks 跟 rutis 的生命周期
  绑在一起。

## 决策

### D1. 插件配置归 PluginManager，存在它自己的记录里

PluginManager 用自己的文件、Dim 自己的格式保存宿主的插件配置：装了哪些插件、从哪里装的、每个名字用哪个
插件、哪些名字关了。它只保存配置，不保存运行状态。记录覆盖所有插件：内置 rutis 插件（只记用户对它们的
决定）、默认配置文件列出的默认插件、用户安装的插件。默认配置文件还列出锁定的名字，这些名字不能关闭，
也不能切换。

### D2. 插件代码的状态归 rutis

插件的代码是一个 rutis 插件，一行 loader 行。行是否存在、在运行还是失败，是 rutis 的状态，通过
rutis-loader 的接口读取。PluginManager 用 rutis-loader 自己的操作执行配置；loader 不保存安装记录，
PluginManager 的记录也不存成 loader 行。

### D3. 内容的状态归内容负责方

插件带的内容（skills、MCP 配置、hooks）和 rutis 插件是不同的对象。PluginManager 把它登记给负责方
（SkillsCatalog、Mcp、Hooks），插件不再使用时撤回。登记进来的内容由负责方作为自己的状态保存。负责方
从不去找插件。

### D4. 插件按名字认，同名互相替换

同一个名字可以装多个插件，同时只用其中一个。打开另一个，就替换了正在用的那个。内置插件和默认插件就是这样
被替换的；它们可以关闭、替换，不能移除。内置的 Rust 代码不能重载。

### D5. 插件的业务配置归插件自己

插件在运行时自己登记自己的 Settings 来源；App Server 的组装代码和 PluginManager 都不替它声明。ADR 002 D4
的业务配置仍归它的所有者。

## 后果

- PluginManager 设计按这些决定重写；旧设计里发布 `pluginContents` 的做法取消。
- Settings 增加运行时登记来源的操作，组装时写死的来源去掉。
- SkillsCatalog、Mcp、Hooks 增加登记和撤回操作，并保存登记进来的内容。
- 不向 rutis 提任何要求。

## 考虑过的其他方案

- **用 loader 的可编辑层做记录（issue #3、#466）。** 否定：把 Dim 的产品状态存进 rutis，并把内容做成行。
- **PluginManager 同时保存运行状态**，例如一张“应该加载哪些”的完整清单。否定：rutis 和负责方已经持有这些
  状态，再存一份可能对不上。
- **替换前做冲突检查。** 否定：插件按名字互相替换，而 rutis 本来就会拒绝同一服务的第二个提供者。
