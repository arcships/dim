# Prompt

状态：草案（[#548](https://github.com/arcships/dim/issues/548)）。需求：[面向模型的提示词 §6 与 §9.1](../model-prompts.md#6-输入指南)。规则：[设计规则](../../../architecture/rust-agent/principles.md) §1、§4 与 §5，[ADR 002](../../adr/002-plugin-scope.md)。作用域：分支，以及它的 Session 持有的条目表。语言：Rust。当前能力见 [Prompt](../../../architecture/rust-agent/plugins/Prompt.md) 的能力页。English: [English](../../../design/plugins/Prompt.md)。

Prompt 拥有输入指南：每次请求都会带给模型的、关于它自己输入的那段文字。它拥有指南的框架，把框架与生效的条目组合成一段文字，并承载各个能力贡献的条目。它不拥有任何条目的措辞：写某个标记的能力贡献解释该标记的那一条。框架说明各条通道及其信任类别、通道规则、转义，以及摘要之后的重述，使模型能把操作者的话、运行时添加的内容和材料分开。

## 1. 范围与非目标

Prompt 拥有：

- **框架**——各条通道、信任类别、通道规则与摘要之后的重述（[面向模型的提示词 §6.2](../model-prompts.md#62-各部分归谁)）；任何作用域都删不掉、换不掉；
- **条目表**，位于 Session，以及已装载能力登记条目的那条路（§2）；
- **组合**——框架与生效的条目按同一个确定的顺序组成的一段文字（§3）；
- **通道规则的 `[input <id>]` 例外**，写成运行时保证，因此属于协议（§3.1）。

不属于 Prompt 的：

- 条目的措辞归写该标记的能力（§4）；
- 请求形状归 Loop，采用的配置归 ContextPreparation，摘要路径归 ContextPolicy；
- 状态与规则的来源、以及模型在其中读到的文字归 PromptSources；这两个标记是 Prompt 的，由它写出（§4）。

本文不含：各能力自己的模型可见文字清单与其协议/策略标记，以及该写入者自己流程里的断言（[#551](../model-prompts.md#92-每个写入面向模型文字的能力)、[#552](../model-prompts.md#94-timelinetree)、[#553](../model-prompts.md#95-goal)、[#554](../model-prompts.md#96-contextcompaction)）；移除 Prompt 的两个调用点（[#555](../model-prompts.md#97-loop-与-contextpreparation)）。

## 2. 条目的登记路由

这条路由是一份契约，形式取 [ADR 002 D5](../../adr/002-plugin-scope.md#d5-合同语言中立)：身份、以数据表示的请求与应答、错误、取消与卸载。Rust 是它的一个绑定（[§8](#8-绑定附录)）；经插件桥接贡献的插件用同一份契约登记（§6.1）。

**身份。** 一次登记属于两样东西：做这次登记的已装载部件，以及它的条目所解释的标记。贡献者取自这次调用——Rust 里是它的 `ctx`，经桥接时是调用行——从不由载荷声明，所以没有谁能以别人的名义登记。条目在该部件装载期间恰好生效（[rutis 用法 R6](../rutis-usage.md#r6-贡献向所有者登记)）。

**作用域。** 表是 Session 的，因为贡献者既在 Session 作用域也在分支作用域，而 Session 是它们最近的共同所有者——它的提示词来源同样是 Session 的，理由一样。每次登记声明自己的条目用在哪里：`session` 表示这个 Session 的每个分支，或某一个分支 id 表示只在那条分支。分支作用域的条目在那条分支的部件装载期间恰好生效，所以关闭一条线只撤回它的条目，其他线的组合不受影响。

**唯一性按组合计算。** 对任何一条分支，一个标记最多只有一条生效的条目。因此 Session 作用域的条目会与同一标记的所有条目冲突；两条分支作用域的条目只有在指向同一条分支时才冲突。登记以作用域加第一个标记为键，所以两条分支各自登记自己的那一份分支条目时不会相遇。

**请求。** 一次登记：作用域加一条条目。

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `scope` | `"session"` 或 `{ "branch": <分支 id> }` | 条目在哪里生效 |
| `entry.markers` | 字符串列表，至少一个 | 这条条目解释的标记元素名，例如 `["dim-branch-line", "dim-branch-lines"]` |
| `entry.order` | 整数 | 条目在条目之间的位置（[§3.2](#32-顺序)） |
| `entry.text` | 字符串 | 条目本身，原样，即模型读到的文字 |

**应答。** 登记完成时为 `null`。应答不携带自己的身份：调用方知道自己登记了什么，登记表也不为它另存一个键。

**没装载的写入者做不出条目。** 登记由已装载的部件自己做出、绑定在它自己那一代上，所以不存在让登记比写入者活得久、或从未装载的写入者送来条目的路径。唯一取不到写入者身份的情形，是来自另一种语言、又解析不出调用行的调用；它以 `no_caller_row` 失败，而不是登记在谁的名下。

**错误**以数据表示，`{code, message}`：

| 码 | 何时 |
| --- | --- |
| `duplicate_marker` | 这次登记会让某条分支对同一个标记有两条条目：它声明的某个标记已有一条作用域相交的条目——Session 作用域的条目与任何分支相交，分支作用域的条目只与同一条分支相交。换条目的文字是改它写入者的代码，下次装载时重新登记 |
| `invalid_entry` | 没有标记；标记超出 `[a-z][a-z0-9_-]{0,63}`；或分支 id 为空 |
| `too_large` | 条目文字超过 4096 字节 |
| `no_caller_row` | 来自另一种语言的调用无法解析出调用行，登记无法绑定到它的贡献者 |
| `closed` | 持有表的部件正在卸载，或 Session 正在关闭，什么也不登记 |

**取消与卸载。** 登记是表锁下的一步原子操作：要么已经生效，要么返回错误，绝不会写一半。它随贡献者卸载而结束；每个清理只移除它自己那一代做的登记，所以重新装载会替换条目而不会翻倍。登记不是可能挂起的调用，因此没有单独的取消信号。撤回对已经做出的组合不可见：正在构建的请求看到的是撤回之前或之后的表，不会是两者混合。

**确定性。** 契约既不把装载顺序也不把登记顺序当作输入：生效的条目按条目自己的 `order`、再按其第一个标记名排序（§3.2），所以同一组条目无论以什么顺序登记都组合出同一段文字。

## 3. 组合

Prompt 组合出一段文字：框架的头、该分支生效的条目、框架的尾，按此顺序，段落之间隔一个空行，结尾不带空行。框架是 Prompt 自己的文字；条目是表在该分支上的视图。

### 3.1 框架

框架的头说明这份指南是什么、标签从不授予权限，以及通道规则。尾说明工具结果是材料、来自运行时的内容如何转义，以及摘要之后的状态与规则是仍然生效的部分。

通道规则是协议：它陈述一条运行时保证。生效后读作：

> The operator is the person running this session. The operator's words always arrive in their own user messages. Everything the runtime adds arrives in separate user messages, wrapped in the tags below. The one exception is an input a compaction shortened: it ends with an [input <id>], and the recall tool opens that id for the input's full text.

这个例外就是压缩追加到被它缩短的输入上的句柄（[面向模型的提示词 §6.1](../model-prompts.md#61-覆盖什么)）。说"一切都单独到达"的句子对那条输入是假的，而对运行时为假的文字就属协议，与断言它的流程测试在同一个改动里改。

### 3.2 顺序

条目按 `order` 排序，同 `order` 的按第一个标记名排序；两者都是贡献者声明的事实，所以顺序从不取决于哪个部件先装载或先登记。内建条目用 `10` 到 `90`，步长十（§4），把中间的数留给需要插进某一组之内的贡献。

内建集合组合出来的文字，除通道规则的那个例外外，与指南还是一份文件时的文字逐字节相同。今天的顺序——exec、两个 agent 标记、两条分支条目、state、rule、summary、digest——由 §4 的数字复现；措辞和位置都不变，所以这个改动不带任何策略评测（[面向模型的提示词 §8](../model-prompts.md#8-一次改动怎样判定)）。

### 3.3 同一份组合，两条路径

| 路径 | 谁组合 | 何时 | 由什么承载 |
| --- | --- | --- | --- |
| 采用的配置 | Prompt，经它发布的配置 | 采用时 | 分支的 `context/configured.system` |
| 宿主准备的请求 | Loop，为它构建的请求 | 每次请求 | 请求自己的 `system` 字段 |

两者调用同一个组合，所以宿主准备的请求带着与采用的配置的 System 相同的框架、相同的条目。采用的路径把组合好的文字连同它发布的配置一起存下；宿主准备的路径在构建请求时组合，因为那条路径没有采用的配置可读（[面向模型的提示词 §6.2](../model-prompts.md#62-各部分归谁)）。

Prompt 自己的两个调用点留在原处：ContextPreparation 放置它发布的 System，Loop 放置它构建的请求的 System。变的是指南文字从哪来——从组合而不是一份固定文件——不是由谁放置。移除这两处放置是 [#555](../model-prompts.md#97-loop-与-contextpreparation) 的事。

### 3.4 在写入者装载时采用

指南条目在其写入者装载时生效、在其卸载时结束。Prompt 在那里采用指南：每个组合发生了变化的开放分支发布一条 `context/configured`，其设置——模型、工具与来源文字——保持不变，System 按当时生效的条目重新组合；组合没有变化的分支对这次变化既不发布，也不读自己的上下文。这些时点之间指南不变，因为条目只在其写入者的 `apply` 里登记，并随那一代的卸载而离开。

分支在自己的 open 处还会采用一次指南：fork 继承父分支的配置，恢复的分支承载它持久化的 System，而当时生效的条目是它的写入者在装载时登记的那些。这次采用是 open 的一部分：分支只有在这次采用发布之后才报告自己已打开，所以 open 之后的第一条请求承载的就是调和后的条目，不会有哪一条按继承来的或持久化的 System 组装。这次采用每次 open 读一次上下文，组合与已采用的 System 相同时什么都不发布。

关闭中的分支什么都不采用：它的写入者正因为它要关停而卸载，此时记录下来的配置会带着一个分支从未运行过的 System。

宿主准备的路径在同样的时点改变指南，但不发布任何东西，因为它把指南组合进它构建的请求。装载之后新开的分支在自己的 open 处按当时生效的条目组合，而从已采用配置构建的请求承载该分支已采用的 System。

没有请求自己组装指南，也没有贡献者向请求追加文字。这正是指南不会在会话中途让缓存前缀分叉的原因：指南是前缀的一部分，它只在这次采用本来就移动前缀时才移动。

## 4. 内建条目及其写入者

每条内建条目原样搬到写它那些标记的能力，并从该部件的 `apply` 登记。登记者是那个其生命周期决定标记是否存在的部件。

| 顺序 | 标记 | 登记者 | 作用域 | 条目 |
| --- | --- | --- | --- | --- |
| 10 | `dim-notification` | BackgroundExec | 分支 | exec 通知 |
| 20 | `dim-agent-report` | Collaboration 的 Session 部件 | Session | 委派工作的结果 |
| 30 | `dim-agent-message` | Collaboration 的 Session 部件 | Session | 另一个 agent 的消息 |
| 40 | `dim-branch-line` | TimelineTree | Session | 你是这个 Session 的哪条线 |
| 40 | `dim-branch-lines` | BranchLine | 分支 | `main` 上的线列表；`order` 相同时身份条目在前 |
| 50 | `dim-branch-message`、`dim-branch-idle`、`dim-branch-ended` | BranchLine | 分支 | 另一条线发来的消息，以及运行时关于某条线的报告 |
| 60 | `dim-state` | PromptGuide | Session | 状态块是什么 |
| 70 | `dim-rule` | PromptGuide | Session | 规则块是什么、怎样结束 |
| 80 | `dim-summary` | ContextCompaction 的摘要策略部件 | Session | 摘要是什么 |
| 90 | `dim-digest` | ContextCompaction 的上下文准备部件 | 分支 | 清单是什么 |

登记者是写该标记的部件，不是撰写该来源措辞的部件。Prompt 把任何 `Context` 或 `State` 来源写成 `<dim-rule>` 或 `<dim-state>` 输入，所以即使措辞是 PromptSources 的，这两个标记也是 Prompt 的。只装 Prompt 不装 PromptSources 的 profile 仍可能被宿主塞入上下文来源，它写出的标记需要自己的条目。

压缩产生的替换内容属于任何策略都要经由的 ContextPolicy，而不属于某一个策略：无论哪个策略返回，清单都由 ContextPolicy 组合（[ContextCompaction §5.10](ContextCompaction.md#510-第-3a-类清单)）。因此该条目由绑定 ContextPolicy 的部件登记，且只在有策略绑定时登记——没有策略的 Session 不做压缩，也就不写清单。绑定了产品策略之外策略的 Session 仍然会发送 `<dim-digest>`，它的指南会解释它。

写了标记却不登记条目的能力，是模型读不到解释的能力；为没人写的标记登记条目，是运行时兑现不了的承诺。两者都归写入者自己修，而 [#551](../model-prompts.md#92-每个写入面向模型文字的能力) 让每个写入者在自己流程里断言这一对。

## 5. 复现它的事实

生效的条目在重启或重新装载之后可以复现，和其余提示词来源一样。

| 事实 | 位置 | 复现什么 |
| --- | --- | --- |
| 装配 | Session 及其分支已装载的部件 | 哪些能力已装载，因而每个 `apply` 会重新登记哪些条目 |
| 采用的 System | 分支上的 `context/configured.system` | 该分支采用时的指南本身，重放即可，不必重新组合 |

条目的文字是代码：每个登记者在改变标记的那个提交里陈述它。重启装入的是同一份装配，所以同样的条目会重新登记，分支采用的 System 从 Timeline 重放；之后的采用从同样的登记组合，产生同样的文字，因为顺序是条目自己的数据。重新装载改变的是某个写入者是否在，而不是已经做出的采用所记录的文字。

分支采用的状态与规则不是关于指南的事实：它们是提示词条目，和任何来源一样由 `prompt/changed` 记录。指南说出它们的名字，不携带它们的内容。

## 6. 跨模块需求

### 6.1 插件桥接

经插件桥接贡献的插件，与 Rust 部件用同样的方式登记条目：在自己的 `apply` 里，在它那一行装载期间，通过参数与应答都是 serde 数据的调用。这条路由以三部分到达其他语言：

1. **名字，在 Session 组里共享。** 这种形状的 Rust 登记表只有在提供的实例里共享名字，才能到达其他语言；运行外部部件的宿主列出这些名字（App Server 的 `share_names`），`guideEntries` 与 `toolRegistry` 并列共享在那里。
2. **把登记绑定到调用方的派发。** Rust 的 `register` 从调用方取 `ctx`；别的语言的调用方没有 `ctx`，所以这个名字经一个 `rutis_bridge` 的 `HostDispatch` 提供，它读 `rutis_bridge::session::caller_row()` 并以那一行登记——正是 `toolRegistry` 已经在用的形式。没有调用行时，调用以 `no_caller_row` 失败，而不是登记在谁的名下。
3. **TypeScript 声明。** 接口与 `ToolRegistry` 的并列加入 `npm/plugin.d.ts`，写明上面的请求、应答与错误码：`register(scope, entry)`，`scope` 为 `"session"` 或 `{"branch": "<分支 id>"}`，`entry` 为 `{markers, order, text}`；调用不带返回值；被拒时抛出 `GuideEntryError`，其 `code` 取 §2 的码之一（包括 `closed`），在调用行之外调用时为 `no_caller_row`。

能力本身——表、组合与两处放置——只靠 Rust 就能工作，所以不接桥接时每个内建写入者的条目路由已经完整，桥接让它对生活在另一种语言里的写入者也完整。

### 6.2 写入者

§4 的每个能力增加一个依赖，它所在 Session 的 `guideEntries`，并在 `apply` 里登记自己的条目——Prompt 自己的 `PromptGuide` 部件也一样。它们都不需要新的服务、事件或配置：登记是向所有者发出的调用，和工具向 `toolRegistry`、提示词来源向 `promptRegistry` 登记一样。

### 6.3 ContextPreparation：条目与已发布的 System

§3.4 的目标需要 [ContextPreparation](../../../architecture/rust-agent/plugins/ContextPreparation.md) 发布器上的一处改动（[#555](../model-prompts.md#97-loop-与-contextpreparation)），以及 Prompt 本身的一处改动。发布器在每次发布 System 的采用处组合指南：渲染 System 的提示词变化，用新来源文字与生效的条目组合；只更新工具、只更新模型与指南采用，则在分支已采用的来源文字上重新渲染指南。

- 已发布的配置把该来源文字与它携带的指南分开记录（`context/configured.system_source`），因此指南可以重新组合，而来源始终是分支已采用的那份文字。
- 只更新工具、只更新模型的采用与指南采用都按当前生效的条目组合指南、放在记录的来源文字上：装载的写入者在装载处就把条目带进 System，无论这次装载是否同时采用它的工具；组合本来就读作这样的分支什么都不发布。
- 在来源被记录之前写下的配置没有可重新渲染的来源：它保留已采用的 System，连同其中的指南，直到下一次渲染 System 的采用替换它。`restoring_a_session_keeps_its_adopted_system_and_model`（`tests/integration/tests/prompt_lifecycle.rs`）恢复的就是这样的 System；只按快照里的提示词来源重新组合，已采用的 System 的位置就只剩指南。
- 请求原样读取已采用的 System：由配置构建的请求里，Loop 承载 `context/configured.system`；宿主准备的请求承载 Prompt 组合出的 System。两者都不追加自己的文字。

触发者是 Prompt 的。它的分支部件声明对条目表的读取，并订阅条目表从自己的登记转发出来的 `changes()`，于是这份 watch 留在条目所在的注册表里，而不跨进 ContextPreparation。监听在分支自己的 open 采用组合之前就取好版本与它起始的组合，并在那次采用跑起来之前开始：在 open 进行期间发生的登记因此会改变监听用来比较的组合并被采用，而不是进入一个事后才取、已记为已见的基线；在那次采用期间发生的变化，会由一次排在 open 这一项之后的采用来采用。此后的每次变化都先在内存里重新组合本分支的指南（既不读 Timeline 也不读存储），只有这段文字变了，才往模型变化与工具采用共用的同一条串行队列里排入一次指南采用。open 自己还会在报告自己已打开之前跑一次采用，因为基线是一份组合，而不是分支承载的 System：它每次 open 读一次上下文，两者本来相同时什么都不发布。让这次采用走 open 的路径、而不是排在之后的一项，正是 fork 与恢复之后的第一条请求能承载当时生效的条目的原因。关闭中的分支什么都不记录：分支一开始关闭监听就结束，已经在读取的采用会在提交锁下拒绝自己的追加，所以那一项和其余项一样被丢弃，而不会被记成一个分支从未运行过的 System。

写入者的工具可以比它的指南早一步到达请求。工具的来源一装载，工具就经工具采用到达；条目则经监听和这条队列到达，所以落在这段窗口里的一步带着新工具和上一个 System，下一步再替换 System：前缀因此移动两次，而 §6.2 描述的是移动一次。设计接受这一步。让 Loop 等待待处理的指南采用，为一小步的前缀把它耦合到 Prompt 的队列上，并不值得；指南仍然只在采用处变化，从不在请求内部变化。

## 7. 规则检查

| 规则 | 本文如何满足 |
| --- | --- |
| §1 插件与能力有界 | 框架是 Prompt 的；每条条目是它写入者的。表是承载者：Prompt 保存条目，不拥有其中任何措辞 |
| §3 正交 | 这条路由是又一个贡献所有者，形状与 `toolRegistry`、`promptRegistry`、`sessionMethods` 相同。它不携带任何标记的业务含义，任何能力都能从 Rust 使用它，之后也能从桥接使用 |
| §4 规则 1 `apply` 取得资源、清理释放 | 贡献者在 `apply` 里登记；登记随该部件的那一代结束 |
| §4 规则 2 事实与服务 | 没有新事件、没有新事实：组合由采用的配置和请求承载，两者都是已有的 |
| §4 规则 4 核心不含业务含义 | Timeline 与 Dispatcher 一无所增；表是能力自己的状态 |
| §4 规则 6 上下文来自被消费的事件 | 指南仍经采用的配置或宿主准备的请求到达模型；本文不增加上下文路径 |
| §4 规则 7 公开事件与服务 | `guideEntries` 是 Session 里的一个贡献所有者，和 `promptRegistry` 一样；它不是端口、检查点或适配器 |
| §4 规则 8 基础设施不变 | 没有改变。§6.1 提出桥接所需的东西，不在这里实现 |
| §5 归属 | 每段文字留在它的作者那里，每条需求都在一个所有者之内 |
| ADR 002 D1 作用域 | 框架是 Prompt 的；条目活在它写入者所在之处，表位于 Session——贡献者最近的共同所有者 |
| D2 没有接线模块 | 没有新增；组合是 Prompt 自己的代码 |
| D3 作用域依赖 | 表在 Session 作用域，贡献者是 Session 与分支部件，可以依赖它；没有谁依赖更窄的作用域 |
| D4 配置 | 没有新增 |
| D5 语言中立的契约 | 路由按身份、数据、错误、取消与卸载陈述，Rust 是其中一个绑定；§6.1 指出桥接成为第二个绑定所需的东西 |
| ADR 008 D2 不写交付状态 | 本文只陈述设计 |
| ADR 008 D3 只管自己的范围 | 每个写入者自己的文字与分类留在它自己的页面上 |

## 8. 绑定附录

Rust 是 §2 与 §3 的一个绑定。名字与语义在上文；这一节只是拼写。

```rust
/// 输入指南的一条条目：它解释的标记与它的文字。
pub struct GuideEntry {
    /// 这条条目解释的标记元素名，例如 `dim-branch-line`。
    pub markers: Vec<String>,
    /// 在条目之间的位置；并列按第一个标记名排序。
    pub order: i32,
    /// 条目本身，原样，即模型读到的文字。
    pub text: String,
}

/// 一次登记用在哪里。
pub enum GuideScope {
    /// 这个 Session 的每个分支。
    Session,
    /// 一条分支，按它的 timeline id。
    Branch(String),
}

/// Session 的指南条目，以及在这些条目上的组合。由 Prompt 插件的
/// `PromptGuide` 部件在 Session 里提供。
pub struct GuideEntries { /* 以（作用域，第一个标记）为键的登记表 */ }

impl GuideEntries {
    /// 为 `scope` 登记 `entry`，代表拥有 `ctx` 的部件，直到该部件卸载。
    /// 若这次登记会让某条分支对同一个标记有两条条目则失败。
    pub fn register(&self, ctx: &Ctx, scope: GuideScope, entry: GuideEntry) -> Result<(), Error>;

    /// `branch` 的指南：框架、生效的条目、框架的尾，段落之间隔一个空行。
    pub fn compose(&self, branch: &str) -> String;

    /// `branch` 的 System：`base` 文字在前，其后是指南。`base` 缺失或为空
    /// 时只有指南；`Parts` 形式的 `base` 追加一个文本 part 承载指南。
    pub fn compose_system(&self, branch: &str, base: Option<SystemPrompt>) -> SystemPrompt;
}
```

一次拒绝是一个小的错误类型，作为 `rutis::Error` 所带 `PluginFailed` 的来源，桥接从 downcast 读它的代码，从不读消息：

```rust
/// 表自己决定的代码（§2）。
pub enum GuideEntryErrorCode {
    /// `invalid_entry`
    InvalidEntry,
    /// `duplicate_marker`
    DuplicateMarker,
    /// `too_large`
    TooLarge,
}

impl GuideEntryErrorCode {
    /// §2 拼写出的代码。
    pub fn as_str(self) -> &'static str;
}

/// 一次被拒绝的登记：它的 §2 代码与写给日志的消息。
pub struct GuideEntryError {
    pub code: GuideEntryErrorCode,
    pub message: String,
}
```

`invalid_entry`、`duplicate_marker` 和 `too_large` 是这个绑定产生的代码。`no_caller_row` 只由桥接产生：Rust 的调用方总有自己的 `ctx`，登记就是调用方自己的 effect。`closed` 来自框架：调用方的上下文正在关闭或卸载时，effect 无法登记，框架返回它自己的 closed 错误，桥接把它映射为 `closed`；Rust 绑定原样返回它，而不把它包装成 `GuideEntryError`。

Prompt 经 `GuideEntries::compose_system` 组合：在它发布的采用处，以及宿主准备的请求由 Loop 组装 System 处。组合的结果就是请求承载的 `SystemPrompt`，放在分支已采用的来源文字之后。没有调用方追加自己的文字。

`register` 的对外形式就是 §2 的数据：作用域、条目与错误码；`compose` 只存在于 Rust，因为没有别的语言读取组合——每种语言都经同一个请求到达模型。
