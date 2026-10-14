# Skills

Session 的 Skill 视图、列表来源与 `skill` 工具：从 [SkillsCatalog](SkillsCatalog.md) 取本 Session cwd 的目录视图；把渲染好的列表作为提示词来源 `dim/skills/list` 提供给 [PromptRegistry](PromptRegistry.md)；提供 `skill` 工具，按当前视图加载 Skill。视图变化后由 [SkillsBranch](SkillsBranch.md) 请 Prompt 重新采用列表。

<!-- generated:plugin Skills — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:SkillsCatalog`、`S:SessionConfig` |
| 提供 | `S:PromptSource#dim/skills/instructions`、`S:PromptSource#dim/skills/list`、`S:SessionSkills`、`S:ToolContribution#skill` |
| 被注入于 | `PromptRegistry`、`SkillsBranch`、`ToolRegistry` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `P:SkillsCatalog` | 查询目录视图、订阅目录版本 |
| `S:SessionConfig` | `cwd`：决定项目 Skill 根 |

## 服务接口

`S:SessionSkills`

| 方法 | 语义 |
| --- | --- |
| `current() -> Result<Arc<SkillView>, SkillsError>` | 当前视图；目录设置不可用时返回来源错误 |
| `list() -> Result<Vec<SkillSummary>, SkillsError>` | 当前视图中启用的 Skill 摘要，供输入建议；不可用时返回错误，不返回假空列表 |

`SkillView`：`version`（对每个 Skill 的 ID、名称、描述、`allowed_tools`、路径与正文以及禁用集合取 `sha256`，只用于判断视图是否变化）、`skills`（启用且生效的，按名称）、`disabled`、`list`（渲染好的列表，没有启用的 Skill 时为空）。列表规则：所有名称都列出，描述按从短到长在 4,000 字符预算内加入；名称与描述来自 SKILL.md，经转义。使用说明不在列表里，单独作为 system 来源。

`S:PromptSource#dim/skills/instructions`：Skill 使用说明（`<skills_instructions>`，含触发规则与 `skill` 调用方式），固定文字，投递方式 System、order 30，只要装了 Skills 就在 system 里。

`S:PromptSource#dim/skills/list`：返回当前列表（每行 `- 名称: 描述`），投递方式 State、order 300（排在规则、环境、日期之后）；没有启用的 Skill 时无条目，Prompt 写清除；设置不可用时返回来源诊断（`Skills unavailable: …`）作为列表状态。说明与列表分开：状态里不出现指令，列表变化时也不重发说明。

`S:ToolContribution#skill`：`risk: session`；参数 `skill_name`。结果依次为`Skill`、`Description`、`File`、`Allowed tools`（有时）与正文。结果只是普通的工具结果，不登记规则。未知或已禁用返回错误结果；设置不可用时返回包含来源诊断的错误结果，不加载旧正文。

调用方：SkillsBranch 注入 `S:SessionSkills`；ToolRegistry 注入工具贡献；PromptRegistry 注入列表来源。

## 状态

| 状态 | 位置 | 变化 |
| --- | --- | --- |
| 当前视图或来源错误 | 内部 | 目录版本变化时重新查询；视图版本或错误不同才替换。只保留当前这一份 |

## 生命周期

- 装载：订阅目录版本、查询并保存初始视图或来源错误、启动刷新任务，始终提供视图服务、两个提示词来源和固定 `skill` 贡献；设置错误不阻止 Session 与其他工具装载。
- 设置恢复：Catalog 刷新成功后采用真实视图；SkillsBranch 请 Prompt 重新采用列表，不重新创建 Session。
- 卸载：结束刷新任务；进行中的 `skill` 调用以 `skills reloading` 取消。

## 为什么

- 只保留当前视图：模型看到的版本从未出现在上下文里，按“看到的版本”加载的收益抵不上维护多份视图的成本；`skill` 总按当前视图加载，只改正文时下一次加载即得新正文。
- 使用说明与列表分开：说明是固定的指令，放 system；列表是会变的状态，放 `dim-state`。

## 代码与测试

- `crates/dim-agent-capabilities/src/skills/{session.rs, mod.rs}`
- `tests/integration/tests/skills.rs`
