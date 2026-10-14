# PromptSourcesBranch

分支加载期间，定时比较会变化的来源（项目与用户 AGENTS.md、运行环境、日期）与本分支已采用的内容，不同就请 Prompt 重新采用这些来源。分支启动或恢复时的投递归 Prompt（`branch/start`、`branch/resume`）；本 Plugin 不写输入。来源本身见 [PromptSources](PromptSources.md)。

<!-- generated:plugin PromptSourcesBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:StateCell<PromptSnapshot>`、`B:dyn ConfigureControl`、`S:PromptHost` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:PromptHost` | 按来源名加载当前内容（由 [PromptRegistry](PromptRegistry.md) 汇总） |
| `B:StateCell<PromptSnapshot>` | 本分支已采用的条目，用来比较 |
| `B:dyn ConfigureControl` | 有变化时配置这些来源 |
| `B:BranchLifecycle` | 分支进入 Active 后才开始比较，分支开始关闭时结束；分支未启动就关闭时不开始 |

## 行为

```text
分支 Active ──► 立即比较一次，此后每 every 一次：
  逐个加载 dim/instructions/project、dim/instructions/user、
           dim/environment/context、dim/environment/date
  与已采用的条目不同 ──► configure(这些来源) ──► Prompt 写 replace / revoke / 新状态
  加载失败记日志，下次重试
```

`prompt_sources::install(builder)` 每 60 秒比较一次；`install_with_clock(builder, today, every)` 可替换日期来源与比较间隔（测试用）。

## 不变式

- 只有已加载的分支比较；未加载期间的变化在恢复时由 Prompt 处理 `branch/resume` 时写入。
- 内容没变不写任何事件；变了才经 configure 产生 `prompt/configure-request`、`prompt/configure-result` 与 Prompt 的写入。
- 不改 system：比较的来源都是规则或状态。

## 为什么

- 定时比较而不是监听文件：监听器要跟随分支装卸，生命周期成本高；比较本地日期而不是睡到零点，能跟上休眠唤醒、改时钟与改时区。
- 自己不写输入：常驻内容只有 Prompt 一个写入方，顺序与比较都在一处。
- 周期比较是后台业务，等分支生命周期进入 Active 才开始。

## 代码与测试

- `crates/dim-agent-capabilities/src/prompt_sources.rs`
- `tests/integration/tests/prompt_sources.rs`
