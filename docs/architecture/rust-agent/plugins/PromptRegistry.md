# PromptRegistry

Session 的提示词来源汇总：把各能力按名提供的 `S:PromptSource#<名>` 汇总成每个分支的 Prompt 加载用的 `S:PromptHost`，并给出新 Session 的初始配置。与 ToolRegistry 汇总工具同一模式。

<!-- generated:plugin PromptRegistry — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:PromptSource#dim/base`、`S:PromptSource#dim/environment/context`、`S:PromptSource#dim/environment/date`、`S:PromptSource#dim/instructions/project`、`S:PromptSource#dim/instructions/user`、`S:PromptSource#dim/session`、`S:PromptSource#dim/skills/instructions`、`S:PromptSource#dim/skills/list`、`S:SessionConfig` |
| 提供 | `S:InitialPromptConfiguration`、`S:PromptHost` |
| 被注入于 | `Prompt`、`PromptSourcesBranch` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:SessionConfig` | `model`：新 Session 采用的模型 |
| `S:PromptSource#dim/base` | 身份与默认准则（[PromptSources](PromptSources.md)） |
| `S:PromptSource#dim/session` | Session 自定义 system prompt（PromptSources） |
| `S:PromptSource#dim/instructions/project` | 项目 AGENTS.md 规则（PromptSources） |
| `S:PromptSource#dim/instructions/user` | 用户 AGENTS.md 规则（PromptSources） |
| `S:PromptSource#dim/environment/context` | 运行环境状态（PromptSources） |
| `S:PromptSource#dim/environment/date` | 日期状态（PromptSources） |
| `S:PromptSource#dim/skills/instructions` | Skill 使用说明，进 system（[Skills](Skills.md)） |
| `S:PromptSource#dim/skills/list` | Skill 列表状态（Skills） |

来源的注入由装配时 `AgentBuilder::session_prompt_source(name)` 登记的名字决定，登记名无提供方时 Session 构建失败。上表是本地执行产品装配下的结果。

## 服务接口

| 服务 | 内容 |
| --- | --- |
| `S:PromptHost` | `load(target)` 按 `target.source` 交给对应来源；未登记的来源返回错误 |
| `S:InitialPromptConfiguration` | 全部登记来源（`branch main` 作用域）加 `SessionConfig.model`。Prompt 处理根分支的 `branch/start` 时整体采用；子分支启动与 `branch/resume` 时只比较规则与状态来源 |

## 为什么

- 来源按名登记、由汇总方注入：Prompt 不需要知道有哪些能力，新增来源（如 Skill 列表）只在提供方与装配处出现。

## 代码与测试

- `crates/dim-agent-capabilities/src/prompt_registry.rs`
- `tests/integration/tests/prompt_sources.rs`
