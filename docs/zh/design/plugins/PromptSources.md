# PromptSources

状态：草案（[#548](https://github.com/arcships/dim/issues/548)）。需求：[面向模型的提示词 §7 与 §9.1](../model-prompts.md#7-语言)。规则：[设计规则](../../../architecture/rust-agent/principles.md) §1、§4 与 §5，[ADR 002](../../adr/002-plugin-scope.md)。作用域：Session。语言：Rust。当前能力见 [PromptSources](../../../architecture/rust-agent/plugins/PromptSources.md) 的能力页。English: [English](../../../design/plugins/PromptSources.md)。

PromptSources 提供 Session 的具名提示词来源和 Session 的提示词注册表。它拥有自己撰写的来源的措辞——base 提示词和 shell 指引——而承载但不拥有宿主的 system prompt 以及用户与项目的 `AGENTS.md`。它把运行时环境和日期作为事实陈述。它不贡献任何输入指南条目：它投递的标记是 Prompt 的，由 Prompt 写出并解释（[Prompt §4](Prompt.md#4-内建条目及其写入者)）。

## 1. 来源及其分类

每个来源是一个名字，带所有者、投递方式和分类（[面向模型的提示词 §4](../model-prompts.md#4-协议与策略)）。分类按文字而不是按来源，一个来源的块里混有两类时也一样。

| 来源 | 投递 | 顺序 | 作者 | 分类 |
| --- | --- | --- | --- | --- |
| `dim/base` | System | 0 | PromptSources | 策略 |
| `dim/session` | System | 50 | 宿主或 profile | 内容 |
| `dim/instructions/project` | Context（`<dim-rule>`） | 100 | 项目 | 内容 |
| `dim/instructions/user` | Context（`<dim-rule>`） | 110 | 用户 | 内容 |
| `dim/environment/context` | State（`<dim-state>`） | 200 | PromptSources | 事实，另有两段指引（§1.2） |
| `dim/environment/date` | State（`<dim-state>`） | 210 | PromptSources | 事实 |
| `dim/language` | State（`<dim-state>`） | 220 | PromptSources | 事实（已设计，§3） |

投递方式决定变更在哪里生效。`System` 来源组成 system 提示词：另一条线经前缀的配置继承它，在找到该事实处应用。`Context` 与 `State` 来源以 Append 输入块交付，与该 `prompt/changed` 写在同一批次，变更通过 `inputs` 命名该块的 id；一条线继承了该变更、但投递输入还没有在它的前缀里被消费时，不把该变更算作已采用，而是按来源的当前值自行采用，块在本线自己的消费位置到达模型。`Initialize` 变更同样如此：决定的是投递方式，不是变更被写入的原因。

### 1.1 策略

`dim/base` 是策略：身份那句话和各项规则——安全、语气、客观、工程、git、工作方法、结束回合，以及链接约定。它是关于怎样工作的指引；无论它说什么，运行时都一样。

shell 指引也是策略。它是环境来源为操作者的 shell 携带的那句话（`Use POSIX shell syntax. …`，或 PowerShell、cmd、Git Bash 等版本），以及在 Windows 上的文件编码指引：怎样引用、怎样调用带空格的路径、向 read 工具请求哪种旧编码。它没有一句陈述运行时事实。

两者都可以在 PromptSources 的出厂配置里替换（§2）；产品一个都不设置，所以候选走的是随产品发布的装配（[面向模型的提示词 §8.1](../model-prompts.md#81-规则)）。

### 1.2 事实，以及对环境来源的报告

环境来源陈述事实：工作目录、home 目录、shell 的可执行文件与版本、它的输出编码、平台、时区。日期来源陈述日期，按进程的本地时区。

**环境来源里有两段文字是指引而不是事实**：`shell_guidance` 与 `file_encoding_guidance`。它们告诉模型怎样工作；即使换成别的说法，运行时也完全一样，而且它们不对任何运行时事实作出真假判断。它们留在原处——在环境块里，正如模型今天读到的——并成为出厂配置里的策略成员。把它们移出状态块会改变每次请求携带的内容，那是一次文字改动，本 issue 不做。

### 1.3 内容

`dim/session` 是宿主或 profile 的 system prompt：承载，从不编辑。用户和项目的 `AGENTS.md` 是指令正文：按文件读取，用上下文块的转义规则转义，使它的文字无法伪造标记，然后原样承载——项目的那份加一行标明路径以及它适用于该目录及其子目录的抬头，用户的那份加一行标明它是 `~/AGENTS.md` 的抬头。两者都不被概括、改写或裁剪，只去掉首尾空白。文件缺失或不可读时不产生条目，而不是产生一个空条目。

## 2. 策略在出厂配置

出厂配置承载这三段策略文字，形状与[摘要策略](../plugins/ContextCompaction.md#134-评估)那个可选的 system prompt 相同：各是一个可选成员，`None` 表示沿用内建文字，产品 profile 一个都不设置。

| 成员 | 替换 |
| --- | --- |
| `base` | `dim/base` 的身份与规则 |
| `shell_guidance` | 环境来源携带的 shell 指引，按 shell 版本 |
| `file_encoding_guidance` | 环境来源携带的文件编码指引 |

候选通过随产品发布的装配装入——同一个插件，用一份配置构造——绝不在请求边界上打补丁（[面向模型的提示词 §8.1](../model-prompts.md#81-规则)）。

## 3. "Agent prompt language" 状态来源

这个来源指明模型对操作者书写所用的语言（[面向模型的提示词 §7](../model-prompts.md#7-语言)）。它解决操作者的环境语言与其消息语言不一致的情形——往中文会话里粘贴英文日志或代码——把回答语言变成明确的选择，而不是从上一条消息推断。

**块。** 来源 `dim/language`，投递 State，顺序 220，排在环境与日期之后。正文是一句话，语言用英文名：

```text
<dim-state source="dim/language">The operator set this session's reply language to Simplified Chinese.</dim-state>
```

正文陈述这项设置本身，不要求任何事：状态块总是被读成情况而不是指令，它指明的是模型对操作者书写所用的语言（[面向模型的提示词 §7](../model-prompts.md#7-语言)）。语言名就是操作者所选的那些——English、简体中文、繁體中文、日本語、한국어、Bahasa Indonesia、Deutsch、Français、Español、Português (Brasil)、Português (Portugal)。这句话的措辞是策略：在来源被构建时按当前文字判定（[面向模型的提示词 §8](../model-prompts.md#8-一次改动怎样判定)），本文不构建它。

**没有选择就没有块。** 操作者没有选过语言时——跟随系统默认——来源不产生条目，base 那句话的规则成立。

**载体。** 客户端把操作者的选择带进 Session：`session.create {language?}` 播种它，`session.language.update` 修改它，值就是 `SessionConfig.language`，Session 配置记录中的一个可选成员。daemon 从不读取客户端的界面语言文件。PromptSources 的 Session 部件提供该 Session 的活值（`SessionLanguage` 单元），每次加载时读取它；分支部件把该单元再暴露到每个分支，供修改它的 Session 方法使用，正如 Skill 列表的做法。没有做出选择的 Session（包括旧客户端写下的记录）没有值，也不产生条目。更新先在 `main` 上采纳，并在回复之前让每个已加载的分支也采纳，因此这些分支的下一次请求就带着新值、无需唤醒；之后才加载的分支在加载时读到当前值，各分支的定时检查仍是兜底。

## 4. 依赖

| 服务 | 用途 |
| --- | --- |
| `S:PromptRegistry` | 按名字登记每个来源，直到本部件卸载 |
| `P:HostPlatform` | home、环境与操作者的 shell |
| `S:SessionConfig` | 工作目录与宿主的 system prompt |

语言来源读取本部件自己的 `SessionLanguage` 单元，而不是注入的服务。

## 5. 跨模块需求

1. **这个 Session 的操作者语言**（§3）：Session 的配置携带它（`SessionConfig.language`），`session.create {language?}` 播种它，`session.language.get/update` 读取并修改它，本插件读取该 Session 的活值。没有配置文件携带它，也没有其他模块决定它。
2. **插件桥接**（[Prompt §6.1](Prompt.md#61-插件桥接)）：没有再向 PromptSources 提出的需求；它的来源投递的标记是 Prompt 的，由 Prompt 的页面提出它们所需的东西。

## 6. 规则检查

| 规则 | 本文如何满足 |
| --- | --- |
| §1 插件与能力有界 | 来源、它们的分类与文字是 PromptSources 的；指南的框架与它写出的那些标记的条目是 Prompt 的 |
| §3 正交 | 来源走既有的提示词来源契约；本文不新增自己的路由 |
| §4 规则 1 `apply` 取得资源、清理释放 | 来源在 `apply` 里登记，随该部件的那一代结束 |
| §4 规则 6 上下文来自被消费的事件 | 来源仍经采用与 Append 输入到达模型；本文不增加路径 |
| §4 规则 7 公开事件与服务 | 来源不增加自己的协议方法或事件：它是既有集合里多加的一个名字。Session 的值是一个服务，由所有者的部件在 Session 作用域提供，并在每个分支上再暴露，正如 Skill 列表（ADR 002 D1、D2） |
| §4 规则 8 基础设施不变 | 没有改变。语言这个事实是提出的，不是在这里实现的 |
| ADR 002 D1 作用域 | 来源活在 Session，也就是它们状态所在之处 |
| D4 配置 | 不加配置文件；策略成员是插件的出厂配置，正如摘要策略的 system prompt 那样 |
| D5 语言中立的契约 | 语言来源是既有通道上的数据块 |
| ADR 008 D2 不写交付状态 | 本文只陈述设计 |
| ADR 008 D3 只管自己的范围 | 指南的路由与框架留在 [Prompt](Prompt.md) 的页面上 |

## 7. 绑定附录

```rust
/// 出厂配置里来源的策略文字。产品一个都不设置，因此生效的是内建文字。
#[derive(Clone, Default)]
pub struct PromptSourcesConfig {
    /// 取代内建身份与规则的 `dim/base`。
    pub base: Option<Arc<str>>,
    /// 环境来源携带的 shell 指引，按版本。
    pub shell_guidance: Option<Arc<dyn Fn(ShellEdition) -> String + Send + Sync>>,
    /// 环境来源携带的文件编码指引。
    pub file_encoding_guidance: Option<Arc<str>>,
}

impl PromptSourcesPlugin {
    /// 内建策略文字生效的插件。
    pub fn new(names: &Names<'_>, today: Today) -> rutis::Result<Self>;
    /// 出厂配置里换成候选文字的插件。
    pub fn with_config(
        names: &Names<'_>,
        today: Today,
        config: PromptSourcesConfig,
    ) -> rutis::Result<Self>;
}
```

语言来源的绑定是来源加载器里多出来的一个分支：名字 `dim/language`、投递 `State`、顺序 220，有操作者设置时给出正文，没有时不产生条目。
