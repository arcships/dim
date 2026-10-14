# PromptSources

Session 的产品提示词来源：按来源名向 [PromptRegistry](PromptRegistry.md) 提供系统提示词来源 `dim/base`、`dim/session`，AGENTS.md 规则来源 `dim/instructions/project`、`dim/instructions/user`，以及运行环境与日期状态 `dim/environment/context`、`dim/environment/date`。提示词正文、运行环境与日期的渲染都在本能力内实现（见 `prompt_sources.rs`）。投递由 Prompt 负责，变化检查由 [PromptSourcesBranch](PromptSourcesBranch.md) 负责，Skill 列表由 [Skills](Skills.md) 提供。

<!-- generated:plugin PromptSources — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:HostPlatform`、`S:SessionConfig` |
| 提供 | `S:PromptSource#dim/base`、`S:PromptSource#dim/environment/context`、`S:PromptSource#dim/environment/date`、`S:PromptSource#dim/instructions/project`、`S:PromptSource#dim/instructions/user`、`S:PromptSource#dim/session` |
| 被注入于 | `PromptRegistry` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `P:HostPlatform` | 运行环境（home、shell、平台、时区）；用户 `~/AGENTS.md` 的位置 |
| `S:SessionConfig` | `cwd`（运行环境与项目 `AGENTS.md` 查找起点）、`system_prompt`（`dim/session`） |

## 服务接口

每个来源一个 `S:PromptSource#<来源名>`（`LoadPrompt::load(target, cancel)`），同一实现：

| 来源 | 投递 | order | 内容 | 失败 |
| --- | --- | --- | --- | --- |
| `dim/base` | System | 0 | 身份、默认准则（去掉首版没有的工具与特性） | — |
| `dim/session` | System | 50 | `SessionConfig.system_prompt`，空白时无条目 | — |
| `dim/instructions/project` | Context | 100 | `# Project instructions (路径; applies to this directory and below)` 加 Session 启动 cwd 向上最近的 `AGENTS.md`，路径与正文转义；没有或为空时无条目 | 文件不可读或不是 UTF-8 时失败 |
| `dim/instructions/user` | Context | 110 | `# User instructions (~/AGENTS.md)` 加 `~/AGENTS.md`，正文转义；与项目文件是同一文件、不存在或为空时无条目 | 不可读时视为没有 |
| `dim/environment/context` | State | 200 | 每行一个标签：`<cwd>`、`<home>`、`<shell>` 与版本及用法指引、输出编码、Windows 的文件编码指引、`<platform>`、`<timezone>`；标签是结构，值转义。不输出 `<available_commands>` | — |
| `dim/environment/date` | State | 210 | 本地日期 `YYYY-MM-DD` | — |

`version` 为正文的 `sha256:<hex>`；未知来源返回错误。Context 与 State 的正文是最终文本，Prompt 原样放进 `<dim-rule>` 或 `<dim-state>`。`prompt_sources::refresh_update()` 返回这六个来源的更新，供显式刷新。

`prompt_sources::install(builder)` 同时登记这六个来源名并安装 PromptRegistry。

## 生命周期

- 装载：取依赖，按名提供六个来源。内容在每次 `load` 时读取，不缓存。
- 卸载后仍在进行的 `load` 返回失败（`prompt sources reloading`），由 Prompt 按重载处理。

## 不变式

- 本 Plugin 不写事件；采用、比较与投递由 Prompt 完成。
- 同一正文得到同一 `version`：内容未变时 Prompt 不产生新修订。

## 为什么

- system 只放静态内容：运行环境、日期、Skill 列表与 AGENTS.md 都会变化，放在 system 里每次变化都要重建整段提示词并让前缀缓存失效；改为输入，变化时追加一次即可。
- order 决定启动时的写入顺序：规则 → 运行环境 → 日期 →（Skills 的）列表，先给要遵守的，再给现在的情况。
- AGENTS.md 拆成项目与用户两个来源：各自变化、各自撤销，规则 key 互不影响。
- 项目 `AGENTS.md` 读不出时失败而不是忽略：静默丢掉项目规则比报错更危险。

## 代码与测试

- `crates/dim-agent-capabilities/src/prompt_sources.rs`
- `tests/integration/tests/prompt_sources.rs`
