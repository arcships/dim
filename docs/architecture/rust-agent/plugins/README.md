# Plugin 文档

每个 Plugin 一篇，文件名等于 `Plugin::name()`。一篇文档写清这个 Plugin 的装配事实，以及它所持有能力的事件处理与状态；跨 Plugin 的链路写在所属功能的流程文档里（例如[事件驱动约定](../event-driven-flows.md)、[中断](../interrupt.md)），这里只写本 Plugin 收到什么、写出什么。

当前覆盖本地执行与上下文相关的 Plugin，以及默认装配的输入、上下文准备、模型调用与分支目录。Prompt 的职责与采用流程见[提示词生命周期](../prompt.md)，不另写插件文档。

| Plugin | 作用域 | 范围 |
| --- | --- | --- |
| [HostPlatform](HostPlatform.md) | 进程 | 完整 |
| [SkillsCatalog](SkillsCatalog.md) | 进程 | 完整 |
| [WorkspaceTools](WorkspaceTools.md) | Session | 完整 |
| [Wait](Wait.md) | Session | 完整 |
| [PromptSources](PromptSources.md) | Session | 完整 |
| [PromptRegistry](PromptRegistry.md) | Session | 完整 |
| [Skills](Skills.md) | Session | 完整 |
| [ToolRegistry](ToolRegistry.md) | Session | 部分：固定表与动态来源 |
| [TimelineTree](TimelineTree.md) | Session | 完整 |
| [Permission](Permission.md) | 分支 | 完整 |
| [Approval](Approval.md) | 分支 | 完整 |
| [FileReads](FileReads.md) | 分支 | 完整 |
| [BackgroundExec](BackgroundExec.md) | 分支 | 完整 |
| [WaitBranch](WaitBranch.md) | 分支 | 完整 |
| [PromptSourcesBranch](PromptSourcesBranch.md) | 分支 | 完整 |
| [SkillsBranch](SkillsBranch.md) | 分支 | 完整 |
| [ToolBinding](ToolBinding.md) | 分支 | 完整 |
| [ToolExecutor](ToolExecutor.md) | 分支 | 部分：执行准入与按快照取实现 |
| [Loop](Loop.md) | 分支 | 部分：工具声明与绑定 |
| [InputQueue](InputQueue.md) | 分支 | 完整 |
| [ContextPreparation](ContextPreparation.md) | 分支 | 完整 |
| [ModelProvider](ModelProvider.md) | 分支 | 完整 |
| [TreeTimeline](TreeTimeline.md) | 分支 | 完整 |

## 结构

```text
# <Name>
一句话职责；不负责什么
<生成块>            作用域、注入、提供、被注入于
## 依赖             每项注入的用途（手写，受校验）
## 构造输入          构造参数与宿主提供的值
## 服务接口          方法、前置条件、错误、调用方
## 运行时调用方       经 TreeCommands / Session::service 按调用查找本服务的一方（装配图看不到）
## 事件              写出 / 消费；消费标明时机
## 状态              持有什么、放在哪、怎么重建；有生命周期的对象画状态机
## 生命周期          装载、重载、卸载、取消
## 不变式
## 为什么            只属于本 Plugin 的取舍
## 代码与测试
```

没有内容的小节省略。

## 服务记号

`P:` 进程、`S:` Session、`B:` 分支，后接类型名（去掉模块路径），带键的服务写作 `S:ToolContribution#wait`。

## 事件处理的三个时机

| 时机 | 机制 | 用途 |
| --- | --- | --- |
| 写入时 | `Timeline::before`、`append_registered` 的登记回调（提交锁内） | 校验、补字段、在事件可见前登记等待者 |
| 提交后 | `Dispatcher::subscribe`，按分支内顺序分发 | 驱动行为：解除等待、更新状态、唤醒 |
| 装载时 | 订阅后、闸门释放前读 `Timeline::history` / `local` | 重建状态；为上一代未结束的对象补写结束事件 |

装载时先订阅再重建，重建期间闸门挡住处理器，重建结束记下 `rebuilt` 序号，处理器跳过不大于它的事件，见[能力重载协议](../single-tree.md#能力重载协议)。

## 生成与校验

生成块由 `cargo run -p xtask -- graph` 从真实装配改写：xtask 按产品组合根的方式装配 `defaults::install_local_execution`（进程级注册 FileStorage、HostPlatform、SkillsCatalog 与 Skill 设置端口，模型为脚本化 mock），记录到 [generated/local](../generated/local/services.md)，再改写每篇文档的生成块。`cargo run -p xtask -- check` 校验：

- 生成块与记录一致；
- `## 依赖` 表第一列恰好是本 Plugin 的全部注入，不多不少。

事件与状态不在 rutis 诊断中，手写维护。
