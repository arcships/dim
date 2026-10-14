# SkillsBranch

分支加载期间，Session 的 Skill 视图变化时请 Prompt 重新采用 Skill 列表来源（`dim/skills/list`）。列表由 Prompt 在 `branch/start`、`branch/resume` 时投递，变化时也由 Prompt 写入。本 Plugin 不写输入。

<!-- generated:plugin SkillsBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:dyn ConfigureControl`、`S:SessionSkills` |
| 提供 | `B:SkillsBranch` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:SessionSkills` | 视图变化通知 |
| `B:dyn ConfigureControl` | 视图变化后配置 `dim/skills/list` |
| `B:BranchLifecycle` | 监听任务等分支进入 Active 后才开始，分支开始关闭时结束 |

## 服务接口

`B:SkillsBranch`：`session()` 返回 `S:SessionSkills`，供经分支到达的入口使用。

## 行为

```text
Session 视图变化 ──► configure(dim/skills/list)
                      └─ Prompt 比较列表文本：变了写一条 dim-state 输入；没变不写
```

- 只改正文、列表文本不变时，不写新输入；`skill` 工具总按当前视图加载，模型下一次加载就拿到新正文。
- `watch` 可合并尚未处理的中间变化，不承诺为每个中间版本配置一次。
- Session 设置不可用时，视图变为不可用：列表来源给出诊断，`skill` 即刻明确失败；恢复后视图变化，列表照常经 Prompt 更新。

## 为什么

- 列表只有 Prompt 一个写入方：启动时它与规则、环境、日期一起按顺序写，运行中的变化也经它比较后写，不会出现重复或乱序。
- 不再跟踪"模型看到的视图版本"：版本从未出现在模型上下文里，维护多份视图的收益抵不上成本。
- 监听任务是后台业务，等分支生命周期进入 Active 才开始。

## 代码与测试

- `crates/dim-agent-capabilities/src/skills/session.rs`
- `tests/integration/tests/skills.rs`
