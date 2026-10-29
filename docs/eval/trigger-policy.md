# Agent Eval Trigger Policy

状态: Draft
日期: 2026-09-07

## Purpose

本文档定义 dimcode 的 agent 能力评测在什么情况下触发、跑哪些任务、用什么参数跑、以及结果达到什么条件才算通过。

目标是回答一个日常问题：**这次改动会不会让 agent 变差？** 全量 benchmark 太慢太贵，不可能每次改动都跑；但"看起来是纯工程改动"这种人肉判断不可靠，会漏掉真正影响模型行为的变更。本文档把这个判断规则化。

本文档是 eval 触发规则和验收标准的单一事实来源。

## Scope

| 范围 | 是否纳入 | 说明 |
|---|---:|---|
| 触发判据 | 是 | 哪些改动进 pipeline，哪些跳过 |
| 分层 pipeline 定义 | 是 | L1 / L2 / L3 各层的任务集、成本、频率 |
| 子集选择与 vendor 策略 | 是 | 用哪个 benchmark、怎么锁版本 |
| 实验参数 | 是 | model、temperature、重复次数、并发、超时 |
| 验收标准 | 是 | 什么算回归、什么算噪声、门禁怎么判 |
| benchmark harness 本体实现 | 否 | 见对应 delivery task |
| 单元测试与 lint 策略 | 否 | 见 [AGENTS.md](../../AGENTS.md) |
| 模型自身能力评测 | 否 | 本文档只评 harness，模型作为受控变量 |

## Principle: model-visible surface

按**模型可见面**分类。判据只有一条：

> 这次改动是否可能改变**发给模型的请求**——请求里的内容原文，或这些内容何时、是否、以何种顺序进入请求？

可能 → 触发。不可能 → 跳过。

这是一个**数据流追踪**问题，不是质量判断。判据不问"会不会变差"（无法从 diff 回答），只问"改动的产物有没有流进发给模型的字节，或流进的时机与条件"。

[AGENTS.md](../../AGENTS.md) 已经定义了这条边界：`ToolResult.content` 是模型唯一可读文本，`structuredContent` 仅用于持久化、UI 和内部逻辑。因此 pipeline 直接复用：**改动落在 `content` 一侧触发，只落在 `structuredContent` 一侧不触发。**

### 三条判定轴

**轴一：内容原文** — 最终成为发给模型的字符串的一部分

- system prompt、prompt catalog、prompt locale
- tool name、description、JSON schema
- `ToolResult.content` 的格式化：字段顺序、换行、单位、路径写法、行号格式、diff 渲染
- error message 文本（fail-fast 的错误信息进入 content，模型据此决定重试还是换路径）
- 截断、分页、省略策略与阈值
- compaction、摘要、history 重建、session resume 的历史拼装
- memory、skill、plugin、`AGENTS.md` / `CLAUDE.md` 的注入时机与拼装顺序

**轴二：时序与机制** — 不改文本本身，但改变它何时、是否、以何顺序进入请求，或改变循环如何运行

- model 与 provider 路由、采样参数、thinking 预算
- subagent 编排、workflow、终止条件、最大轮数、重试与退避
- hook、permission gate（放行与否改变轨迹）
- 工具对某个 agent-type 是否可见

**轴三：执行作用** — 工具对外部世界做的事变了，即使上下文原文一模一样

判据是：同样的模型输出，执行后世界的状态会不会不同。这一轴不被"进入上下文"覆盖，但会改变任务 verifier 的结果，必须单列。

- builtin tool 的真实行为、sandbox 边界、exec-shell 语义

### 路径映射

下表同时是 [L1 提示词](./l1-review-agent-prompt.md)里给 agent 的**先验**。它只用于提示，不作为最终判据——同一文件可能同时含渲染与 prompt 拼装逻辑，diff 的实际内容可以在两个方向上推翻先验：跳过区的文件若开始拼装进入上下文的文本，判 visible；轴一路径下的文件若本次只改测试或注释，判 invisible。

| 路径 | 默认分类 |
|---|---|
| `crates/dim-agent-capabilities/src/prompt*.rs`、`input-guide.txt` | 轴一 |
| `crates/dim-agent-capabilities/src/context_*.rs`、`history.rs`、`recall.rs`、`summary_strategy.rs`、`timeline_tree.rs` | 轴一 + 轴二 |
| `crates/dim-agent-capabilities/src/agent_loop.rs`、`steer.rs`、`wait.rs`、`input_queue.rs`、`branch_input.rs` | 轴二 |
| `crates/dim-agent-capabilities/src/tool_*.rs`、`workspace_tools/**`、`shell_*.rs`、`output.rs` | 轴一 + 轴三 |
| `crates/dim-agent-capabilities/src/model_provider*`、`crates/dim-agent-aimux/**`、`vendor/aimux/**` | 轴二 |
| `crates/dim-agent-capabilities/src/skills/**`、`skill_settings.rs` | 轴一 |
| `crates/dim-agent-capabilities/src/permission.rs`、`approval.rs`、`host_platform/**` | 轴三 |
| `crates/dim-agent-runtime/**` | 轴二 |
| `crates/dim-agent-types/**` | 按具体字段判定（类型改动常不改变产物） |
| `crates/dim-app-server/src/collaboration*.rs`、`agents/**`、`agent_records.rs` | 轴二 |
| `crates/dim-app-server/src/provider/**`、`model_catalog.rs`、`managed_provider/**` | 轴二 |
| `crates/dim-app-server/src/mcp/**`、`plugin_manager/**`、`session_typescript.rs`、`skills_management.rs` | 按具体 plugin 判定 |
| `runtime/plugins/**`、`runtime/default-plugins.json` | 按具体 plugin 判定（`goal`、`workflow`、`scheduler`、`session-title` 等） |
| `crates/dim-cli/**`、`crates/dim-client/**`、`crates/dim-acp/**`、`web/**` | 跳过（客户端与渲染层；`dimcode exec` 的参数解析除外） |
| `crates/dim-oauth/**`、`crates/dim-app-server/src/*oauth*` | 跳过（账号登录） |
| `crates/dim-app-server/src/storage/**`、`transport.rs`、`connection/**` | 跳过 |
| `docs/**`、`scripts/**`、`xtask/**`、`.github/**`、`benchmarks/**`、`tests/**` | 跳过 |

### 明确跳过

渲染、样式、动画、desktop 窗口与托盘、web 站点、UI 文案 i18n、CLI 打包与签名、发版脚本、文档、telemetry 落盘、`structuredContent` 与纯持久化 schema、不改行为的类型重构与重命名。

例外：i18n 中 **prompt locale 不属于 UI 文案**，落在轴一。持久化 schema 若影响 resume 后的历史重建，落在轴二。

### 灰区：默认按触发处理

以下改动通常被描述为"纯工程重构"，但实际改了可见面，一律触发。其中依赖升级不体现在 diff 语义里，由 [L1](#l1--触发判定) 的路径规则直接触发。

| 灰区 | 风险 |
|---|---|
| ToolResult 格式化重构 | 换行与字段顺序改变模型注意力分布 |
| 工具调用并发化 / 批处理 | 改变结果进入 context 的顺序 |
| 缓存与 prompt cache 断点调整 | 语义不变但轮数与成本变化，属性能回归 |
| 时间戳、绝对路径、cwd 进入 prompt | 破坏可复现性，同时影响模型行为 |
| provider SDK 依赖升级 | 上游默认参数静默变化 |
| 上下文上限、截断阈值调参 | 表面是常量，实际是机制 |

## Pipeline

分三层，职责严格分离：**L1 只决定要不要跑 L2，不阻断合并；L2 是唯一的阻断门禁；L3 不阻断，负责兜底与趋势。**

| 层 | 触发时机 | 内容 | 时长 | 是否阻断 |
|---|---|---|---|---|
| L1 | 每个 MR | review agent 判定 diff 是否触及可见面 | < 5 min | 否，只决定是否触发 L2 |
| L2 | 窗口内任一 merge 判定为可见时（见 [L2 触发粒度](#l2-触发粒度)） | FrontierHarness 30 任务 × k 次 | 实测 2.2–6.9 h | 是（见验收标准） |
| L3 | 每周五定时 + release 前 | Terminal-Bench 2.1 + DeepSWE | 数小时 | 否，趋势跟踪 + 假阴性率回算 |

### L1 — 触发判定

L1 **不承诺**"证明这次改动无影响"，只承诺决定要不要跑 L2；残余风险由 L3 兜底。因此 L1 允许假阴性，但守住一条边界：**L1 只能增加 L2 的运行，永远不能阻断合并**——它判错的代价是算力，不是正确性。

L1 就是一个 review agent：读 MR 的代码 diff，按[三条判定轴](#三条判定轴)逐文件判定 visible / invisible，任一文件 visible 即进入 L2，并给出可核对的数据流 trace。

完整提示词见 [L1 review agent prompt](./l1-review-agent-prompt.md)。那是被逐字使用的文本，修改它等同于修改 pipeline 的触发行为。

唯一的例外是**依赖升级**：manifest 的 diff 里只有版本号，行为变化在上游，agent 没有判断依据。这类改动由一条路径规则直接判为 visible，不询问 agent——用不确定的模型去执行一条确定的规则没有意义。规则维护在 `benchmarks/policy/always-eval.toml`，本仓库需覆盖：

```
Cargo.lock   rust-toolchain.toml   runtime/package-lock.json   runtime/plugins/*/package-lock.json
vendor/**    crates/dim-app-server/resources/default-catalog.json
```

`vendor/**` 尤其重要：改 vendored 的上游依赖正是「diff 里读不出行为变化」的典型。

#### 实现约束

每一条对应一种已知失效模式：

| 约束 | 对应的失效模式 |
|---|---|
| 只输入 diff，不输入 MR 标题、描述、branch 名 | 作者写"pure refactor"会直接污染判定 |
| 逐文件出结论，但允许全仓库查符号与调用链 | 大 diff 一次性判会被截断；只看 diff 文本又追不动数据流，两者都会导致误判 invisible |
| 不采信 diff 内的自述性文本（注释、commit message） | 代码注释里的"pure refactor"和 MR 描述是同一类污染，且属于被评估的内容而非指令 |
| 追踪不下去时判 visible 并写明卡点 | 无法完成追踪等同于无法确定，默认必须倒向 visible |
| temperature 0，verdict 按 diff hash 缓存 | 同一 diff 两次给出不同结论会摧毁信任 |
| **每个 MR 都写一条记录**到 `benchmarks/ledger/`，不论判定为何 | 只记 invisible 就算不出 visible 率，也分不清回归是 L1 漏放还是 L2 没拦住 |
| 只输出 visible / invisible，不输出阻断建议 | 保持 L1 只增不断的边界 |

prompt 中要求 agent 对以下几类给予更高权重，它们容易被描述成重构：工具结果格式化、并发与批处理、重试退避与超时、sandbox 与 `exec-shell` 语义、采样参数与 thinking 预算、截断与上下文阈值等机制常量。这是 prompt 内容，不是独立机制。

已知失效模式，接受并由 L3 兜底：跨文件的隐式耦合（共用 formatter、util 的默认参数、常量迁移），以及被描述为重构但实际改了行为的变更。

### L2 — FrontierHarness 子集

任务源：[frontier-harness-eval/eval](https://github.com/frontier-harness-eval/eval)，30 个任务（21 Terminal-Bench + 9 DeepSWE），deterministic verifier 判 pass/fail。

选它的理由：这 30 个任务本身就是 TB 与 DeepSWE 的子集，任务格式、verifier、容器定义与 L3 全量同源，L2 与 L3 的结果可以直接对齐并合成同一条趋势线；无需自建子集。

#### 任务定义的实际构成

上游仓库里每个任务只有两个文件：

```
tasks/<task-id>/
  ├ instruction.md   任务描述
  └ task.toml        元数据、资源规格、超时、pin 死的 docker_image
```

**verifier 与测试都在镜像里，不在仓库里。** 因此「把任务定义拷进仓库」不足以锁定复现性，见 [Subset governance](#subset-governance) 的三重 pin。

| 任务来源 | 数量 | 镜像 |
|---|---:|---|
| Terminal-Bench 2.1 | 21 | `alexgshaw/<task>:20251031`（Docker Hub） |
| DeepSWE v1.1 | 9 | `public.ecr.aws/d3j8x8q7/swe-bench-202605:<ext_id>-v1.1` |

#### 运行方式：harbor，不用上游脚本

上游 `skills/frontierharness-eval/scripts/` 依赖 **Runta 云运行时**（`runta` CLI、golden checkpoint、fresh restore），本仓库不采用——那条路会让自有评测机失去意义。

`task.toml` 的 schema（`[verifier]` / `[agent]` / `[environment]` / `verifier.collect`）就是 **harbor**（Terminal-Bench 2.x 的 harness）的任务格式，直接用 harbor 在自有机器上跑。harbor 属上游 harness，与任务源同一生态。

被测 agent 通过 harbor 的 agent 接口接入，adapter 由本仓库实现，落在 `benchmarks/harness/`。

#### 禁网与模型代理

30 个任务**全部** `network_mode = "no-network"` / `allow_internet = false`。容器内的 agent 默认够不到模型 API。

**实现没有用宿主代理。** 改为按 trial 放行模型主机（harbor 的 `extra_allowed_hosts`），
key 经 agent env 注入容器。两点代价要如实记着：key 进了容器；放行是 agent 阶段的，
verifier 阶段仍然隔离。落盘侧是安全的——harbor 对敏感 env 做模板化，运行配置里写的
是 `${VAR}` 引用而非字面值。若要恢复「key 不进容器」，需要真的建一个宿主代理，那是
一项未开始的工作。

#### 资源规格与并发上限

| 项 | 取值 |
|---|---|
| 单任务 cpus | 1–2 |
| 单任务内存 | 2–8 GB |
| 单任务磁盘 | 10–20 GB |
| agent 超时 | TB 任务 900 s；DeepSWE 任务 5400 s |

并发受**内存**约束而非 CPU：单任务峰值 8 GB，据此定并发上限。

单次 L2（30 任务 × k=3 = 90 trial）**实测 2.16 / 4.64 / 5.49 / 6.92 小时**（并发 5，四次真实 run）。跨度这么大是因为**耗时与超时 trial 数正相关**：每个超时 trial 烧满它的 agent deadline（TB 900 s，DeepSWE 5400 s），所以跑得越糟的 run 越长。给运行设墙钟上限时必须按最长的那次留余量——紧上限会恰好在结果最值得看的时候把 run 掐掉。

#### L2 触发粒度

**只有一种模式：batch。** main 上按窗口定时跑，窗口是 `上次真正跑过 L2 的 commit .. 当前 main`。

曾设计过 per-mr（每个判 visible 的 MR 跑一次，排不开时再切 batch），已放弃。原因是实测数据把它排除了：

| 事实 | 对 per-mr 的影响 |
|---|---|
| 单次 k=3 运行实测 2.16 / 4.64 / 5.49 / 6.92 小时 | 一天几个 visible MR 就排不开，串行队列只会无限增长 |
| 阻断判据的阈值[已被证伪](#l2-门禁)（误拦率 28–32%） | per-mr 的全部价值在于「合并前拦住」，而现在没有可用的阻断阈值 |

per-mr 的成本是实打实的，收益却依赖一个尚不存在的可靠门禁。在阈值重标之前它换不来任何东西。

**batch 模式下回归的处置是合并后发现并 revert**，不是阻断合并，也不是豁免。归因窗口是一个批次而非一周，由载体任务 bisect 定位。

这条决定顺带简化了 [ledger](#pipeline-ledger)：judge 的判定单位是一个 merge，L2 的答复单位是一段 main，两者分开记录；而 merge commit 的 sha 永不变化，所以不存在同一 MR 的多次 attempt、head 前移、或并发流水线写入竞态。

若日后阈值重标成功、且单次运行成本大幅下降，再重新评估 per-mr —— 届时它是一个新决定，不是一个被搁置的开关。

**不把它当作通过率门禁。** n=30、pass rate ≈ 0.6 时单次运行标准误约 8.9pp，两次运行差值的标准误约 12.6pp（≈ 3.8 个任务）。掉 3 个任务完全落在噪声内。因此 L2 的判据是 per-task 配对翻转与成本，不是聚合百分比（见验收标准）。

上述为理论推算。真实 trial 的 bootstrap 与之吻合：k=1 下两次运行「净变坏任务数」的
95% 上界为 **4 个任务**，与推算的 3.8 个一致。单次运行标准误实测约 5.9pp，小于推算的
8.9pp——推算假定所有任务共享同一通过概率，而实测中约四成任务在三次 trial 里全通或全挂，
贡献的方差接近零。真值介于两者之间：这些「确定性」任务的判断仅基于 3 个样本，3/3 并不
等于 p=1。**做阈值时取推算值（更保守），不要取 bootstrap 的下界。**

### L2 — OfficeQA Pro

第二个 L2 语料，与 FrontierHarness 子集**并列、互不混合**：各有各的任务目录、`MANIFEST.json`、语料 pin 与 baseline。它测的是另一种能力——在约 460 MB 的非结构化文档里检索、读表、算数，而不是改代码。

| 项 | 取值 |
|---|---|
| 任务源 | [databricks/officeqa](https://huggingface.co/datasets/databricks/officeqa)（HF，gated）的 `officeqa_pro.csv`，**133 题，全部 hard** |
| 不是 | OfficeQA Full（246 题，含 113 道 easy）；OfficeQA Pro V2（90 题，另一套语料） |
| 语料 | 697 期美国财政部公报（1939–2025）的 parsed text，`treasury_bulletins_parsed/transformed/*.txt` |
| verifier | 上游 [`reward.py`](https://github.com/databricks/officeqa/blob/main/reward.py) 的 `score_answer`，逐字 vendor 进每个任务；只返回 0/1，直接落在现有 pass/fail 读取上 |
| tolerance | **0**（`score_answer` 的默认值）。harbor 官方 adapter 用 0.01；两者的分数不可比，所以取值写进每个任务的 `tests/config.json`，改它会改变语料 pin |
| 目录 | `benchmarks/officeqa/tasks/`，由 `benchmarks/officeqa/assemble.py` 生成后提交 |

#### 任务定义的实际构成

上游没有任务树，只有一张 CSV 和一个语料目录，所以 adapter 在本仓库：`assemble.py` 按 `benchmarks/officeqa/template/` 为每一行生成一个 harbor 任务。

```
officeqa/tasks/<uid>/
  ├ instruction.md        题面 + 语料位置 + 输出约定（/app/answer.txt，单行）
  ├ task.toml             docker_image = 语料镜像 @sha256；allow_internet = false
  ├ environment/Dockerfile  永不构建；被构建时直接失败
  ├ solution/solve.sh     写标准答案（harbor oracle 自检用）
  └ tests/                test.sh → evaluate.py → reward.py(score_answer) + config.json
```

**语料不进仓库，也不在 trial 里下载。** 它在一个预构建镜像里，每个 `task.toml` 按 **digest** 引用它；`assemble.py` 拒绝 tag。于是评测机每个镜像只拉一次，trial 不需要 HF token，也不需要网络。HF token 只在准备阶段用一次：下载数据、生成任务。

当前 pin 的是 harbor officeqa adapter 在 Docker Hub 上公开的镜像 `docker.io/benediktstroebl/officeqa-corpus@sha256:731237e3b35277ca3c08b1b5e6063c4c6ae0c88a4eb4d7cef201c4f14a3e333d`（amd64/arm64），与 FrontierHarness 任务一样从 Docker Hub 拉取。采用前逐文件比对过：镜像内 697 个 `treasury_bulletin_*.txt` 的 sha256 与 HF revision `763a8366abf2a3605c381d53586d844dc60fa756` 的 `treasury_bulletins_parsed/transformed/` 完全一致。`benchmarks/officeqa/corpus-image/` 是自建同样镜像的备用路径，用于该镜像不可用或需要换数据 revision 时。

#### pin

与 FrontierHarness 的三重 pin 同构：

1. **任务树**——`MANIFEST.json` 的 `tree_digest`。题面、答案、tolerance、`reward.py`、语料镜像 digest 都落在任务文件里，任一变化都会移动它。
2. **语料镜像 digest**——写在每个 `task.toml`，也记在 `MANIFEST.json` 的 `source.corpus_image`。
3. **harbor 版本**——同 FrontierHarness。

HF 数据集的 revision 与 `reward.py` 的上游 commit 记在 `MANIFEST.json` 的 `source` 里，作为重新生成的依据；它们不进 `tree_digest`，因为同样的任务文件无论从哪次下载得来，测的都是同一件事。

#### 禁网、资源与并发

- 网络与 FrontierHarness 相同：`allow_internet = false`，模型主机经 trial 级 `extra_allowed_hosts` 仅在 agent 阶段放行，verifier 隔离。**不照搬 harbor adapter 的 `allow_internet = true`**——题目只需要容器内的语料，公网只会让 agent 去搜答案。
- 单任务 1 cpu / 8 GB / 10 GB 磁盘，agent 超时 1800 s，verifier 120 s；取自 harbor adapter 的 full-corpus 模板。
- **并发 10**，不是 5。最坏情况的墙钟约为 133 × 1800 s ÷ 并发：并发 5 是 13.3 小时，超过单次 L2 的 9 小时 ssh 上限；并发 10 是 6.65 小时。代价是峰值 80 GB 内存——评测机放不下时降并发，同时必须放宽 CI 超时，**不能**两边都不动。并发记入 baseline 的 `params.n_concurrent_trials`，改它就要重冻 baseline。

#### 运行方式

入口不变：`daily_run.py --tasks-dir benchmarks/officeqa/tasks --concurrency 10`。窗口、k、baseline、ledger 的规则与 FrontierHarness 完全相同，只是 state 单独一份（`officeqa-pro-state.json`），两个语料各自推进窗口——一个语料 L2 失败 hold 住的窗口，不该被另一个语料的成功推进掉。

定时运行由 `nightly.sh officeqa-pro daily` 选择，且**只允许 `daily`**。k=3 的 checkpoint 是 399 个 trial，最坏 20 小时，放不进一个 job；要做 checkpoint 或 baseline 就手工跑。

**同样不当作通过率门禁。** n=133 时单次运行标准误约 4.3pp（p≈0.5），比 FrontierHarness 小，但判据仍是逐题配对翻转，理由见上节。

### L3 — 全量

每周五定时触发，跑 Terminal-Bench 2.1 全量 89 任务 + DeepSWE 全量 113 任务。用于趋势跟踪与 release gate，不阻断日常 MR。

n=89 时两次运行差值标准误仍约 6.6 个任务，因此 **L3 同样不能靠单次全量下结论**，必须依赖重复次数与配对统计。

## Prompt regression test

这是一个普通回归测试，与 unit test 同级，**不属于 pipeline，不参与 L2 的触发决策**。放在这里只是为了说明它和 L1 的区别。

固定一组 scripted 任务（模型响应与工具返回均为 canned），录制发给模型的完整 request body：system block、tools schema、每轮 messages、sampling params、thinking 预算、`cache_control` 断点。脱敏后提交进仓库。用途有二：

- 抓非预期的 prompt 变更：重构模板时漏掉一段注入、少拼一句话、误改了 temperature
- 出问题时作为排查材料：直接显示 tool description 里变了哪几个 token，不必从源码 diff 反推

它**不能**用来豁免 L2：snapshot 只能证明"被脚本任务走到的路径上、在 canned 轨迹下、字节没变"，未覆盖的分支、被 mock 掉的执行作用和真实运行中分叉的轨迹都在它视野之外。

## Subset governance

| 项 | 规则 |
|---|---|
| Vendor | 任务定义拷入 `benchmarks/tasks/`，不做运行时拉取 |
| Pin | **三重 pin**，全部记录在 `benchmarks/tasks/UPSTREAM.md`：① frontier-harness-eval 的 commit SHA（任务选择列表）② 每个任务镜像的 **digest**（非 tag——tag 可被上游重推）③ harbor 版本 |
| 镜像 | 30 个任务镜像镜像到自有 registry，不依赖 Docker Hub / ECR 长期可用 |
| 升级 | 升级 pin 属独立 MR，必须同时重跑 baseline 并更新基线文件；不得与功能改动同 MR |
| 基线 | `benchmarks/baselines/<pin-sha>.json`，记录每任务 k 次的 pass 分数与成本 |

上游改了任务定义而 pin 未锁，会把别人的编辑显示成你的回归。

## Experiment parameters

| 参数 | L2 | L3 |
|---|---|---|
| 任务数 | 30 | 89 + DeepSWE 全量 |
| 每任务重复次数 k | 日常 batch **1**；baseline 与重要节点 **3** | tb2.1为5，deepswe为4 |
| Reasoning effort | **high**，显式传入不继承端点默认 | 同 L2 |
| Primary model | 评测机模型配置里的 `defaultModel`（见 [host/README.md](../../benchmarks/host/README.md)） | 同 L2，另加第二模型族 |
| Temperature | 固定；不使用随机 seed 抖动 | 同 L2 |
| 并发 | 每任务独立容器，禁止跨任务共享状态 | 同 L2 |
| 资源 | 固定 vCPU / 内存 / 磁盘，每次 fresh restore | 同 L2 |
| 单任务超时 | 固定墙钟上限，超时计 fail 并单独标记 | 同 L2 |
| 最大轮数 | 固定上限，触顶单独标记为 `turn_exhausted` | 同 L2 |
| 网络 | 与 baseline 一致的出网策略 | 同 L2 |
| Trajectory | 完整请求/响应落盘保留 | 完整落盘保留 |

补充规则：

- **日常用 k=1，定论用 k=3。** 灵敏度随 √k 增长而成本随 k 线性上升：在本语料上实测，
  整体通过率的 95% 区间 k=1 为 23.3pt、k=3 为 13.3pt、k=5 为 10.7pt（bootstrap，
  下同）。两者都拦不住 5pt 级别的小回归，为这点差别每天多花约 3.6 小时机器时间不
  划算。日常 batch 的职责是粗筛塌方，定论交给 baseline 与重要节点的 k=3。
  实测佐证：同一构建的 k=1 轮次给出 53.3%，k=3 为 45.6%，偏差 7.7pt；同一轮 k=1 还
  曾把 `output_truncated` 判为已消除，k=3 显示并未消除。**k=1 的单轮结论不得写入
  ledger 作为判定依据。**
- **噪声底是语料的性质，不随模型状态变化。** 同一套 30 任务上，reasoning effort 为
  `max`（通过率 35.6%）与 `high`（45.6%）两种状态下测得的区间宽度与误拦率几乎相同
  （k=1 均为 23.3pt / 28%，k=3 均为 13.3pt / 约 32%）。因此这些阈值一经标定即可跨
  模型与跨 effort 复用，不必每次换模型重标。
- **锁模型是硬性要求。** 上游公开结果跑在 Kimi K3 上，与本仓库 baseline 不可直接比较；本仓库只与自己的 baseline 比。
- **`glm-5.3` 是 alias，不是 snapshot id。** 本来要求锁 snapshot，但自建端点不
  暴露带日期的 snapshot，所以锁不到。实际的保护是两层：每次运行把端点报告的
  model 记进 baseline 与 run 记录，运行前与 baseline 比对，**id 变了就拒绝运行**；
  id 不变而权重悄悄换掉则检测不到，靠的是「这个部署是我们自己的、变更会通告」这
  条流程保证，不是技术保证（见 [`runner/model_identity.py`](../../benchmarks/runner/model_identity.py)
  的模块注释）。端点若不再由我们运维，这是第一个要重做的地方。
- **Reasoning effort 必须显式传入。** 不传时端点按模型自身默认执行，该默认不在本仓库
  控制之下、也不出现在任何运行记录里，等同于一个未记录的实验变量。实测 `glm-5.3`
  的 catalog 默认为 `max`，其通过率、超时率与 token 消耗均劣于显式的 `high`。
- **第二模型族只在 L3 跑。** harness 改动经常只在某一族模型上回归，但每 MR 跑两族成本不可接受。
- **基础设施失败必须与任务失败分开计数**（容器起不来、provider 5xx、限流）。infra failure 率超过 5% 的整次 run 作废重跑，不得进入判定。

## Metrics

FrontierHarness 自身的结论是：pass rate 相近（50–66.7%）而成本相差 17.5x（每成功任务 $1.05–$18.34）。这说明在 harness 这一层，**通过率的区分度很低，成本与轮数才是主信号**。本仓库改的正是 harness，因此指标定义如下。

| 指标 | 级别 | 定义 |
|---|---|---|
| Per-task pass rate | Primary | 每任务 k 次中的通过比例，与 baseline 配对比较 |
| Cost per successful task | Primary | 成功任务的 token 成本中位数 |
| Turns per successful task | Primary | 成功任务的 assistant 轮数中位数 |
| Aggregate pass rate | Secondary | 仅用于趋势线，不作为门禁 |
| Wall time per task | Secondary | 中位数 |
| Failure mode 分布 | Secondary | `turn_exhausted` / 工具误用 / 提前放弃 / verifier fail |
| Infra failure rate | Guard | 超过 5% 则整次 run 作废 |

统计方法：**配对比较**（同一任务的 before/after 配对，McNemar 或 paired bootstrap）。任务难度差异在配对中抵消，方差比看聚合通过率小一个量级。禁止用两次独立运行的聚合百分比之差下结论。

## Acceptance criteria

### L1 门禁

| 条件 | 结果 |
|---|---|
| 命中依赖升级路径规则 | 进入 L2，不询问 agent |
| agent 任一文件判为 visible | 进入 L2 |
| 全部判为 invisible | 跳过 L2 |

L1 不产生 Fail，只产生"跑"或"不跑"。三种结果都写入 ledger。

### L2 门禁

以 baseline 为对照，同时满足以下条件才算通过。

> **阈值状态：全部初值已被本语料上的标定证伪，重标前 L2 一条都不得 blocking。**
> 下面先记最早发现的一条，再记后来对全表的逐条标定。
>
> 最早证伪的是「净退化 ≤ 1」：在代码零改动时的误拦率实测为 **k=1 28.0% / k=3 31.7%**
> ——每天跑就是每三四天误拦一次整批。要把误拦压到 5% 以下，阈值需放宽到「净退化 > 4」
> （k=1 3.9%），而那时只有挂掉 5 个以上任务（≈17pp）的回归才拦得住。**两端都不可用，
> 说明「净退化任务数」这一判据本身不适合当阻断门禁。**
>
> 注意 **k=3 的误拦率高于 k=1**：per-task 分数在 k=3 下有 0、⅓、⅔、1 四个取值，两次
> 比较更容易不相等；k=1 只有两态，大量任务两次同为 0 或同为 1，不计翻转。**加大 k 不
> 能修复这条判据，反而加重它。**
>
> 这组数字由三次真实 run（deepseek-v4-flash、glm-5.3 max、glm-5.3 high）的 trial 重
> 采样得出。bootstrap 以 3 个 trial 估计单任务的通过概率，会低估真实波动，故它是下界。
>
> **全表已逐条标定，五条统计判据无一及格。** 用
> [`runner/calibrate.py`](../../benchmarks/runner/calibrate.py) 在实测 baseline
> f71b5af2（30 任务 × k=3）上跑零改动的 A/A：候选臂由 baseline 自己的 trial 合成，
> 两臂同构建，因此任何判据一响都是误拦。两列对应两种「同一构建再跑一次」的建模——
> `resample` 从该任务已记录的 trial 有放回抽样（把 k/k 的任务估成必过，**系统性低估**，
> 是下界）；`beta` 先从 Beta(通过+1, 失败+1) 后验抽通过率再抽 trial（三次没翻车本就
> 不是「永不翻车」的强证据，更可信）。三个随机种子复跑，数字稳定在下表区间内。

| 条件 | 阈值（待标定） | 误拦率 resample / beta | 状态 |
|---|---|---|---|
| 无任务从 k/k 全通退化到 0/k 全挂 | 出现即 Fail | 0% / **25%** | 已证伪，不阻断 |
| 净退化任务数（退化 − 改善） | ≤ 1 | **27%** / **41%** | 已证伪，不阻断 |
| paired bootstrap 通过率变化 95% CI | 下界 ≥ −5pp | **55%** / **92%** | 已证伪，不阻断 |
| 成功任务成本中位数上升 | ≤ 15% | **6–10%** / **6–9%** | 已证伪，不阻断 |
| 成功任务轮数中位数上升 | ≤ 15% | **28%** / **28%** | 已证伪，不阻断 |
| `turn_exhausted` 数量上升 | ≤ 1 | 0% / 0% | **空转**，不阻断 |
| Infra failure rate | ≤ 5% | 0% / 0% | **冗余**，不阻断 |

> **逐条成因不同，不可一概而论。**
>
> **通过率 CI** 是**阈值比尺子还细**：该 CI 半宽在 resample 下约 **±5.6pp**、beta 下
> **±11.6pp**，而门限画在 −5pp，落在噪声带以内。代数上要让下界不破线，本次 run 必须比
> baseline **高出 +0.6pp**——零改动时约一半的日子做不到。90 次里多挂 1 次即 1.1pp，
> 这条判据想抓的量级与噪声同级。CI 随 sqrt(trial 数) 收窄，靠加大 k 代价过高。
>
> **全通翻转**的 0% 是估计量在说话，不是门禁本身：resample 把记录为 k/k 的任务估成
> 必过，于是永不翻转。按 Beta 后验给它们应有的失败概率，四分之一的零改动 run 会触发。
> 下方「关于第一条判据的锚点」用同一后验算出「观测 3/3 下轮仍 3/3 约 72%」，两处同源。
>
> **成本 / 轮数中位数**是这轮标定里最意外的一条：「各任务中位数再取中位数」读起来像
> 稳健统计，但它对**任务在通过集与失败集之间进出**并不稳健——只有两臂都成功的任务进入
> 中位数，参与者每次都在换，中位数就跟着换。轮数尤其糟（28%）。
>
> **`turn_exhausted` 是空转**：CLI 的 `RunEndReason` 枚举里没有这个值（`end_turn` /
> `stop` / `user_interrupt` / `provider_error` / `tool_error`），该 delta 恒为 0。
> 它不是被调松的门禁，是没接线的门禁——曾经的 blocking 宣称了一次从未发生的检查。
>
> **infra rate 是冗余**：超限已由 `JobReport.problems` 判为 `invalid`，且 `invalid`
> 先于 breach 被读取。在此阻断只会把「测不出来」误标成「测出退步」，而这恰是本文档
> 最坚持要分开的一组区别。

**重新启用一条判据的门槛。** 不是把 `blocking` 改回 `True`。需要一次误拦率低于 5% 的
标定，并把结果记在它所支持的阈值旁边——就像上表。跑法：

```bash
python3 benchmarks/runner/calibrate.py benchmarks/baselines/<pin>.json
```

注意这仍**不是**真 A/A（同一镜像在机器上独立跑两次），候选臂是合成的。它给的是下界：
在低估的估计量下已经误拦的判据，不可能靠更仔细的测量救回来。真 A/A 仍待补。代码侧的
落地在 [`runner/baseline.py`](../../benchmarks/runner/baseline.py) 的
`Comparison.criteria`，每条判据的状态与理由逐条写在该处注释里。

**关于第一条判据的锚点。** 它依赖存在一批「稳定全通」的任务。实测这批任务是存在
且跨模型一致的：`deepseek-v4-flash` 的 3 个 3/3 任务（`fastapi-deprecation`、
`katex-multicolumn`、`kv-store-grpc`），在 `glm-5.3` high 档下无一例外同样是 3/3。
但覆盖面有限——两模型交集只有这 3 个任务，且 3/3 只是「3 次没翻车」：按 Beta 后验，
观测 3/3 的任务下一轮仍 3/3 的概率约 72%，high 档现有 6 个 3/3 期望下轮只剩约 4.3 个。
**因此这条判据可用，但只覆盖语料的十分之一，不能作为唯一门禁。**

早期曾依据 `max` 档数据得出「两模型 3/3 交集为空、锚点不可靠」，该结论已被证伪：
`max` 档的 3 个 3/3（`anko`、`db-wal-recovery`、`log-summary-date-ranges`）在 high 档
下分别退化为 2/3、2/3、1/3，它们是模型在 truncation 与超时拖累下的偶然产物。**用一个
处于退化状态的 run 去判断语料的稳定性，会把模型的问题误读成语料的问题。**

任一 Primary 指标越界 → 该批次判 Fail，记入 [ledger](#pipeline-ledger) 的 run 记录并列出越界的判据。

**阈值重标之前，这个 Fail 不得用来阻断任何东西。** 它会被写进 run 记录，因为记录的
是「按当前阈值算，这批越界了」这一事实；但上面那段警告说明这些阈值的误拦率是
28–32%，所以在重标完成前，Fail 是一条待人工判读的信号，不是结论。日常 batch 是
k=1，schema 层面就写不出 `Pass`／`Fail`——这条限制只对 k≥3 的 `checkpoint` 有效。

k=1 能记的只有两个：`screen`（跑完了，有测量，不下结论）与 `invalid`（**打不出分**：
缺 trial、trial 没被判、任务没跑满 k 次、infra 超限）。两者的分界是**有没有测量**，
不是 k：`screen` 宣称存在一份测量而它不下结论，所以一次打不出分的运行不是 screen。
k 规则约束的是**判定**，`invalid` 不是判定，因此不受它约束——见 `schema.py` 中
`RunRecord.validate` 的这条豁免。

**没有豁免流程。** [batch 模式](#l2-触发粒度)下 L2 在合并之后运行，没有可阻断的合并，因而也没有可放行的东西；处置是定位载体 commit 并 revert。豁免是 per-mr 门禁的产物，随 per-mr 一起取消。

**成本与轮数与通过率同级。** 通过率不变但成本上升 30% 的改动同样判 Fail——这正是 harness 层最常见的回归形态。

### 日常 screen 的判定

> **状态：设计草案，尚未实现。** 未实现的是本节定义的东西：累积统计量、`profile` 运行
> 模式、报警动作。
>
> 已经在跑的是「观测」那一半：日常 k=1 每晚留一份 `run.json`，并写一条 ledger run 记录
> —— 正常是 `screen`，打不出分是 `invalid` 加对应的 `invalid_reason`。**但一条 `screen`
> 不是结论**，它只说明当晚跑了、没塌。把这些 screen 累积成「该不该去跑一次 k=3」的判断，
> 是本节还没落地的部分。

日常 batch 是 k=1，[Experiment parameters](#experiment-parameters) 已论证它读不出小
回归：30 任务 × 1 次，代码一个字不改，整体通过率的 95% 区间就有 20.4pt（plug-in 估法，
与上文 23.3pt 同源）到 31.1pt（Beta 后验，更贴近真实）。**任何「今晚对比昨晚」式的单晚
判定都被这个底板压死。**

但有一件事被浪费了：**回归落在 main 上就持续存在，之后每一晚都会重测它；而噪声不会
持续。** 日常 screen 的判定因此不在单晚做，而是累积。

#### 职责

**日常 screen 不给结论，它只回答一个问题：值不值得花 2–7 小时跑一次 k=3 checkpoint。**

这是它与 L2 门禁的分工。门禁回答「这批变差了吗」，需要 k≥3；screen 回答「现在该不该
去问那个问题」，k=1 足够。

#### 统计量

零点由校准期给出每任务通过概率 `p_i`。当晚：

```
E   = Σ p_i                     （期望通过数）
SD  = √Σ p_i(1 − p_i)           （单晚标准差）
z_n = (当晚实际通过数 − E) / SD
S_n = min(0, S_{n−1} + z_n + 0.5)        累积，只累向下偏离
报警 ⟺ S_n ≤ −4
```

`p_i` 与 `E`、`SD` 只在当晚**实际被观测到的任务**上计算：infra 失败的 trial 不计入
分子也不计入分母（与 [Metrics](#metrics) 的 infra 口径一致）。整晚 `invalid` 的运行
不产生观测，`S` 不变——**不是记 0，是跳过**：记 0 会让一次故障被当成一晚正常。

不同 k 之间判据无定义，但这里比的不是判据。k=3 的 baseline 给的是每任务通过概率，
k=1 给的是每任务一次伯努利抽样，**观测对期望是标准的比法**，不需要两臂同 k。

#### 实测的检测能力

以本语料（30 任务、实测通过率 52%）模拟，「这批 commit 搞坏 n 道题」时：

| 累积阈值 | 误报间隔 | 坏 3 道 | 坏 5 道 | 坏 8 道 | 坏 12 道 |
|---|---|---|---|---|---|
| −3 | 105 晚 | 14 晚 | 7 晚 | 3 晚 | 2 晚 |
| **−4** | **156 晚** | 20 晚 | 10 晚 | **4 晚** | 3 晚 |
| −5 | 190 晚 | 29 晚 | 12 晚 | 5 晚 | 3 晚 |

取 **−4**：约半年误报一次，塌方级回归四晚内抓到。

对照单晚判定：标到同样 5%/晚 误报时，坏 8 道题当晚只有 40% 抓到，而误报每 20 晚一次。
**累积在两个维度上同时更好**，因为它用上了「回归会持续」这个事实。

坏 1–2 道题在任何方案下都读不出来，这是 30 × k=1 的信息量决定的，不是算法问题。

#### 四个阶段

**① 校准。** 锁一个 commit，在它上面收集**每任务至少 14 次观测**，据此定出 `p_i` 并
冻结。这期间只记数、不报警。定义见下面的[校准运行](#校准运行-profile)——**现有的三种
运行模式没有一种能做这件事**，这一步必须先有实现。

- 不必等 14 个日历晚：需要的是观测次数。**5 次 k=3**（每次 2.16–6.92h，在 job 的 10h
  超时内）给出 15 次，5 天完成。k=7 每次 10–16h 会超时，不可用。
- 锁 commit 是必须的：校准期间若落进真回归，它会被写进零点，**从此永远测不出来**——
  与「拿被测 commit 当 baseline」是同一种中毒。
- 结束时做**同质性检查**：前半段与后半段的通过数若有显著落差，说明期间混进了变化，
  这份零点作废重采。
- 校准期间 main 照常前进；这些 merge 在校准结束后作为一个批次进入窗口。盲区越短这个
  批次越小，这是压缩校准天数的真实理由。

**② 日常。** 每晚 k=1，算 `z_n` 与 `S_n`。不下任何结论。两者的存放见
[状态归属](#状态归属)——**不进 ledger 的 [Run 记录](#run-记录--ledgerrunsrun-idjson)**。

**③ 报警。** `S_n ≤ −4` → 跑一次 k=3 checkpoint 确认。确认是真回归后，按 `S` 曲线的
拐点收窄嫌疑窗口，再用**坏掉的那几道题**在窗口内 bisect。

- 拐点位置本身有噪声（5 道题的回归中位 10 晚才检出，拐点不确定性约 ±3–5 晚），
  收窄后**要留缓冲**，否则载体 commit 可能落在窗口外。
- **bisect 的每一步不得用 k=1。** 既然只剩几道题，提高 k 很便宜：5 道题 × k=10 = 50
  次，与日常一晚的 30 次同量级，可靠性却完全不同。见 [自动 bisect](#自动-bisect)。

**④ 复位。** 累积量 `S` 归零。**零点不动。**

| checkpoint 结论 | `S` | 零点 |
|---|---|---|
| 没问题（误报） | 归零 | **保持** |
| 是回归，已修复／revert | 归零 | **保持**（水平会回来） |
| 是回归，但**决定接受** | 归零 | 重走阶段 ①，**不是**拿这次 k=3 |

**不能用 k=3 刷新零点**，这一条反直觉但是硬的：一次 k=3 对 `E` 的估计误差是 **0.58σ**，
而 14 次观测的校准期是 **0.27σ**。CUSUM 的 drift 容差是 0.5σ，用前者刷新等于注入一个
超过容差的系统偏差，之后天天单向漂移直到误报。**拿 3 次观测覆盖 14 次观测的结果，是用
更差的估计替换更好的。**

零点真正需要重采的触发条件是别的：**语料 pin 变化**或**模型身份变化**——两者都已被
[Reproducibility requirements](#reproducibility-requirements) 追踪，直接挂上去即可。

#### 校准运行 `profile`

现有三种运行模式没有一种能重复测量同一个 commit：

| 用现有模式跑第二次 | 实际发生 |
|---|---|
| `checkpoint` | 第一次结束时窗口推进到该 commit，第二次 `base == head`，`resolve_window` 返回 `None`，打印「no new commits」后返回 0。**L2 根本不跑。** |
| `baseline` | 第一次冻结基线文件。第二次在**构建之前**即被拒绝（「基线已存在」），根本不会跑。 |

因此增加第四种运行模式 `profile`：

| 属性 | 取值 | 理由 |
|---|---|---|
| `k` | 3（可配） | 单次时长 2.16–6.92h（TS 版实测），在单次 L2 的 9h 上限内。k=7 需 10–16h，超出上限 |
| `judges_window` | `false` | 它测的是一个固定 commit，不答复任何 main 区间；不读也不写 `last_l2_head` |
| 是否写 baseline | **否** | 这是 `profile` 与 `baseline` 的分界：同样是定点重复测量，但 baseline 是给判据用的参照（写一次、不可覆盖），`profile` 是给 screen 用的零点（要累积多次） |
| **是否写 ledger run 记录** | **否** | 见下 |
| 产物 | `profiles/<corpus-pin>.json` | 逐任务的通过次数与观测次数，**追加式**：每次 `profile` 运行把这一轮的计数并进去，并记下贡献它的 run id（同质性检查要用） |

**为什么不写 ledger run 记录。** 不是为了省事，是现有 schema 里**没有一个 outcome 能
如实描述它**：

| 候选 | 为什么不行 |
|---|---|
| `screen` | k≥3 时构造不出来。`RunRecord.conclusive` 是由 `k` **派生**的（`schema.py` 的 `conclusive` property），不是由运行模式声明的，所以 k=3 的 `profile` 在 schema 眼里就是 conclusive，而 conclusive 的运行记 `screen` 会被拒——理由是「screen 会扔掉已付费的结果」 |
| `pass` / `fail` | 都宣称做过一次比较。`profile` 没有比较对象，它就是参照本身 |
| `invalid` | 语义是「这次运行什么也没说」，字面上成立，但它在人读 ledger 和查健康度时的含义是「这次运行坏了」。用它会让一段正常的校准期看起来像一串故障。schema 现在还把这条堵死了：`invalid` 必填 `invalid_reason`，而那四个值（`l2_failed` / `settle_failed` / `unscoreable` / `no_reference`）没有一个描述得了一次正常的 `profile` |

`RunType` 的 Literal 也不含 `profile`，所以任何记录方式都要动 schema。与其为一个不判定
任何东西的运行扩充判定记录的类型，不如让它的溯源留在它自己的产物里——`profiles/` 记下
每一轮的 run id，这与 baseline 产物里已有的 `sources` 字段是同一个做法。

> **相邻的一处待决。** `baseline` 运行目前**会**写 ledger run 记录，且 outcome 为
> `pass`——它同样没有做过任何比较。今天无害（它的 `covers` 为空，进不了 `cleared`），
> 但这与[刚修掉的 k 不匹配记 `pass`](#l2-门禁) 是同一个形状。`profile` 不沿用这个做法，
> 而 `baseline` 该不该继续这样记，列入 [Open items](#open-items)。

`profile` 产物在观测数达到阈值（每任务 ≥ 14）前标记为未完成，screen 不得使用它。达标时
做[同质性检查](#四个阶段)，通过则封存为只读，`p_i = 通过次数 / 观测次数`。

> **与 `runner/calibrate.py` 无关。** 那个工具校准的是[L2 门禁](#l2-门禁)各判据的误拦率，
> 回答「这条阈值能不能用来阻断」。这里校准的是每任务通过概率，回答「今晚这个数正不正常」。
> 两件事都叫「校准」是命名的巧合，不是同一个东西——实现时不要合并。

#### 报警之后

统计量给出信号，下面是信号之后的动作契约。

**表现。** 新增退出码 `9`，由 issue 报告器开一个 `screen` 类 issue，窗口照常推进。
不用 `8`：它已是 ledger 推送失败（见 [benchmarks/README.md](../../benchmarks/README.md)
的退出码表）。它与运行失败分开：这次运行本身没有任何问题，回归也**尚未被确认**——
`S ≤ −4` 是「值得去查」，不是「查实了」。「回归」issue 留给 checkpoint 真的判出 `fail` 的时候。

**由谁发起 checkpoint。** **人工**，不自动排程。自动升级需要日常 job 能触发另一条流水线，
即需要一个能触发运行的常驻凭据；而这台机器的设计方向恰恰相反——构建被测代码时不让
任何凭据进入环境变量。为一次
「约半年一次」的自动化新增一个常驻凭据，不划算。报警信息里直接给出该跑的命令。

**等待期间 `S` 继续累积，不冻结。** 冻结会丢掉等待期的观测，而这几晚同样在测 main。

**复位的前提是 checkpoint 的 head 不早于最后一晚计入 `S` 的 head。** 这一条是必须的：
误报期间若有新回归合入，它的证据也在 `S` 里；直接归零会把它一并抹掉。而只要 checkpoint
跑在不更早的 head 上，那些 merge 就在它的窗口内，已被一次 k=3 更充分地检查过，其证据
被正当地取代。**不满足这个条件时不复位，继续报警**——拿一次旧 head 的 checkpoint 去清零
新 commit 的嫌疑，是这套机制最容易犯的一次错。

> **依赖。** 「checkpoint 判定为干净」目前不是一个有约束力的结论：[L2 门禁](#l2-门禁)
> 的全部判据都因误拦率过高而处于非阻断状态。在重标完成前，这一步实际是「人去读 k=3 的
> 数」而不是「机器判定无回归」。本节的状态机不依赖判据重标即可运行，但它的最后一环要到
> 那时才真正闭环。

#### 状态归属

| 数据 | 存放 | 生命周期 |
|---|---|---|
| 每任务 `p_i`、`E`、`SD` | 按语料 pin 存的校准产物 | 校准期写一次，之后只读 |
| 累积量 `S`、上次复位时的 head | 评测流的状态文件（与 `last_l2_head` 同处） | 每晚更新，满足复位前提的 checkpoint 后归零 |
| `profile` 的逐任务计数 | `profiles/<corpus-pin>.json` | 校准期追加，达标后只读 |
| **`z_n` 的历史**（每晚一条：日期、head、`z_n`、`S_n`） | **同一个状态文件**，滚动保留最近约 40 晚 | 与 `S` 同写同读 |
| 当晚的 `z_n`、`E`、`SD` | `run.json` | 人读单次运行用；评测机上保留 30 天 |

**`z_n` 不进 ledger 的 Run 记录**，尽管它是一次运行的产物。两个原因：

* **加字段要迁移 digest。** `RunRecord._digest_material()` 决定记录的 `digest`，而
  `ledger_tools/audit.py` 用「重算的 digest 与存档的是否相同」判篡改。往材质里加一个
  字段，**已有的每一条记录重算后都对不上，全部会被判 `tampered`**。为一个派生统计量
  付这个代价不值得。
* **需要它的是流，不是单次运行。** CUSUM 更新与阶段③的拐点定位都要读**多晚的序列**，
  这是评测流的状态，不是某一次运行的属性。`S` 已经在状态文件里，序列与它同写同读才不会
  出现「窗口推进了但序列没更新」这类不一致。

状态文件就是 ledger clone 里的 `daily-state.json`，随 ledger 一起提交并推送到
`eval-ledger` 分支，因此是持久的。`run.json` 不是：它只在评测机的结果目录里保留 30 天，
**不能作为序列的唯一副本**。

`S` 必须跨运行持久化，且与 `last_l2_head` 在同一个文件、同一次写入——两者不一致时
（例如窗口推进了但 `S` 没更新）无法判断哪一个是对的。

### L3 验收

对比基准：**上一次绿色（通过）的 L3 run**。

| 条件 | 动作 |
|---|---|
| 无任务翻转，且 Primary 指标均在阈值内 | 记录趋势，无动作 |
| 出现任务翻转 | 触发自动 bisect（见下） |
| cost 或 turns 越界 | 同样触发自动 bisect，并阻断下一次 release |

**任一 Primary 指标越界都必须走 bisect。** 若只对翻转做 bisect，一个只推高成本、不动通过率的回归就拿不到 commit 级归因，也就永远不会计入 L1 假阴性——而这恰恰是 harness 层最常见的回归形态（见 [Metrics](#metrics)）。

Release gate：release 前必须有一次不早于 7 天的 L3 通过记录。

### 自动 bisect

L3 周五触发，回归的 blame window 从**上一次绿色 L3** 起算：顺利时为一周，run 作废、bisect 未破案或元凶未修复都会把窗口拉长到多周。因此 L3 出现任务翻转时必须自动二分。

**翻转的定义**：任务从 k/k 全通退化为 0/k 全挂，与 L2 门禁红线同口径。浅翻转（如 4/4 → 3/4）落在重复次数的噪声范围内，只进趋势统计，不触发 bisect。

1. 确定**回归载体任务集**——不做全量二分，只用出问题的那几个任务：
   - 翻转载体：按上述定义发生翻转的任务
   - 成本载体：cost 或 turns 超出该任务自身噪声带的任务
2. 取**自上一次绿色 L3 以来**合并进 main 的 commit 列表——与对比基准同源，不按自然周切分；按"本周"取窗口，会在 L3 作废或元凶未修复时把元凶永久留在窗口外
3. 只重跑载体任务集，k 次
4. 二分定位到具体 commit，自动开 issue 并 @ 作者
5. 回查该 commit 在 [ledger](#pipeline-ledger) 中的记录，按下表归因

| ledger 中的记录 | 归因 |
|---|---|
| L1 判 invisible，L2 未跑 | L1 假阴性 |
| L1 判 visible，L2 跑了并 Pass | **L2 漏放**——阈值过松或噪声，触发阈值重标，与 L1 无关 |
| L1 判 visible，L2 判 Fail，但该批次未被 revert | 流程问题，回溯为什么带着已知回归继续发布 |

没有第 4 步，L3 只能告诉你"这周变差了"，无法定位。没有第 5 步，即使定位到 commit 也分不清是哪一层的责任。

成本类载体通常比翻转载体更容易定位：给定轨迹形状，token 数相当稳定，方差主要来自轨迹分叉，所需重复次数往往低于翻转 bisect。

#### 弥散型回归：bisect 无载体时

有一类回归没有载体任务——例如往 system prompt 加了 200 token，每个任务都多花 3%，聚合越界但没有任何单任务超出自己的噪声带。此时二分无从下手，改走：

1. 先取**自上一次绿色 L3 以来**合并进 main、且**没有任何 run 覆盖过**的 merge，逐个补跑 L2
2. 若这批都不能解释，再取**被某次 conclusive 且 Pass 的 run 覆盖**的 merge 补跑

顺序不能反过来：第一批更可能是元凶且从未被 L2 检验过，成本也更低。但**第二批不能省**——弥散型回归同样可能来自一个 L2 判 Pass 的 MR，只查第一批会把所有弥散回归都记到 L1 头上，重演"只记 invisible"那个错误。

补跑结果直接作为归因，并按 [bisect 的归因表](#自动-bisect)分别喂给 L1 假阴性率或 L2 漏放率。

## Pipeline ledger

ledger 记两种记录，因为判定的两个部分归属不同的单位：**L1 判的是一个 MR，L2 答复的是一段 main**。把两者压进同一条记录，要么一次运行的结论被复制进 N 条 MR 记录且看不出它们同源，要么 batch 模式下的 L2 结论无处可放。

两种记录都是 JSON，append-only，长期保留，不做过期清理——季度统计和 bisect 归因都要回查历史。

### Merge 记录 — `ledger/merges/<sha 前两位>/<merge-sha>.json`

每个 MR 留**一条**，不论 L1 判定为何、L2 是否运行。只记 invisible 的话，visible 率无从统计，回归归因也分不清层次。

| 字段 | 说明 |
|---|---|
| `mr` / `commit` | 标识 |
| `l1_verdict` | `visible` / `invisible` |
| `l1_trigger` | `agent` / `path_rule`（依赖升级走路径规则，不算 agent 的判断）/ `fail_open`（judge 失联，未判定） |
| `l1_reason` | agent 给出的、指向具体 hunk 的理由 |
| `l1_identity` | 判定的 identity（diff + policy + prompt + judge 模型）。两条记录对同一 identity 给出不同 verdict，就是 audit 要抓的矛盾 |
| `visible_files` | 判 visible 时，触及可见面的文件 |

**MR 记录里没有任何 L2 字段。** L2 从不为单个 merge 运行，「这个 merge 被测过没有」只能由 run 记录的 `covers` 回答；在 MR 记录上留一个恒为 `not_run` 的字段，只会让读的人以为它还可能是别的值。

**直推 main 的 commit 不产生 MR 记录**（没有 MR 可记），只出现在 run 记录的 `unjudged` 里。少了这一条，ledger 会漏掉这次运行实际答复过的代码。

每个事实只记一次，记在归属它的地方：「这个 merge 没被判定」属于 MR 记录的 `l1_trigger`，「这段代码没有任何判定」属于 run 记录的 `unjudged`。

**`fail_open` 是唯一可以被替换的 MR 记录。** 它说的是「这一次 judge 没给出结论」，不是判定。窗口因它保留、下一次运行重判；judge 得出的判定（`agent`、`path_rule` 或 `oversize`）直接替换这条记录，不算分歧，不进 `disputed`。若保留 `fail_open`、把真实判定只记成分歧，这个 merge 在 ledger 里就永远是「未判定」，归因时查不到 L1 的真实判断。

替换只有 `fail_open` → 判定这一个方向。已写入的判定仍然 write-once，重判结果不同照旧记进 `disputed`；重判再次 `fail_open` 时保留原记录。不追加第二条记录：同一个 merge 有两条记录，每个读 ledger 的地方都得先决定用哪一条。被替换的记录留在 `eval-ledger` 分支的 git 历史里（该分支禁止 force push），judge 当时失联的事实不会丢；那次运行的 run 记录照常在 `covers` 里列出这个 merge，因为它的代码确实进了被测构建。

### Run 记录 — `ledger/runs/<run-id>.json`

每次 L2 运行留一条，记它答复了哪段 main、用什么参数跑的、结论是什么。

| 字段 | 说明 |
|---|---|
| `run_id` / `run_type` | 标识；`daily` / `checkpoint` / `baseline` |
| `k` | 每任务重复次数 |
| `conclusive` | **派生字段，不由写入方给定**：`k >= 3` |
| `measured` | **派生字段**：这次运行有没有测到东西。`outcome != invalid`，或 `invalid_reason = no_reference`。决定本次运行能否 cover 它的 merge —— 见 [两种记录如何 join](#两种记录如何-join) |
| `corpus_pin` | 语料 pin（`tasks/MANIFEST.json` 的 `tree_digest`） |
| `subject_commit` | 被测构建的 commit |
| `window_base` / `window_head` | 答复的 main 区间；baseline 无 base |
| `covers` | 本次运行答复的 merge commit sha（全长） |
| `unjudged` | 本次答复了、但**没有任何 L1 记录**的 commit —— 即直推 main 的提交。judge 失联的 merge 不在此列：它有自己的 MR 记录，`l1_trigger = fail_open` 已经说明它未被判定，而它的代码确实进了被测构建 |
| `outcome` | `pass` / `fail` / `invalid` / `screen` |
| `invalid_reason` | `outcome = invalid` 时**必填**，否则必须为空：`l2_failed` / `settle_failed` / `unscoreable` / `no_reference`。`measured` 由它派生，所以不写就没人读得懂这条记录 |
| `l2_exit_code` | harbor 的非零退出码；只在非零时记 |
| `settle_error` | 打分失败时抛的异常，或结果根本没取回来的原因 |
| `breaches` | 越界的阻断判据名，`fail` 时非空 |
| `disputed` | 本次重判、且与已有 L1 判定不一致的 merge sha（verdict、trigger、identity 或文件清单任一不同）。已有判定不被覆盖，分歧记在这里。已有记录是 `fail_open` 时不算分歧：它被本次的判定替换（见 Merge 记录一节） |
| `runner_dirty` | 评测 harness 有未提交改动时为真。只有显式 `--allow-dirty` 才可能出现，且 baseline 禁止 |
| `infra_rate` | 本次运行的 infra 失败率。`measured` 为假时它是默认值 `0.0`，不是观测值 |

**`conclusive` 为假时 `outcome` 只能是 `screen` 或 `invalid`。** 日常 batch 是 k=1，[Experiment parameters](#experiment-parameters) 禁止单轮 k=1 作为判定依据；把这条写成 schema 约束而不是惯例，k=1 的 `pass` / `fail` 就根本构造不出来，不依赖任何读取方记得这条规矩。

`invalid` 不在此列，因为 k 这条规则约束的是**判定**，而 `invalid` 不是判定——它说的是这次运行没有产出可判定的东西。挡掉它并不能保护这条规则，只会让一次打不出分的 k=1 运行无处可去，只能记成 `screen`，而 `screen` 宣称存在一份测量。

`invalid` 与 `fail` 不同：harbor 非零退出、打分失败、缺 trial、未打分、语料 pin 不符、
infra 超限、**没有可比的 baseline**，都是「这次运行什么也没说」，不是「这次改动变差了」。

`invalid_reason` 把这些分成两类，分界是**有没有测量**，不是 k：

| reason | 含义 | `measured` |
|---|---|---|
| `l2_failed` | harbor 非零退出 | 假 |
| `settle_failed` | 结果取回来了但读不出分，或根本没取回来 | 假 |
| `unscoreable` | 读出来了但不成立：缺 trial、trial 没被判、任务没跑满 k 次、infra 超限 | 假 |
| `no_reference` | **测量是好的**，只是没有可比的参照 | 真 |

`no_reference` 包括两种形态：没有该语料 pin 的 baseline，以及有一份但 k 与本次不同——不同
k 之间判据无定义（3/3 的任务拿 0/1 去比会读成全通翻转），所以一条判据都不评估，
`blocking_breaches` 因此为空。而空的违反列表正是 `pass` 的构成条件，所以这必须显式记成
`invalid`：否则一次什么都没比的运行会被记成「L2 明确放行过」，窗口内每个 merge 都会进入
漏放率的分母。

**但 `no_reference` 必须保住 `measured`**，理由正好相反：它的窗口会推进，之后不会有任何
运行再覆盖那批 merge。把它和前三种一样看待，整个「baseline 还没冻」期间的 merge 会永久
落进 L1 漏放率的分母，被记成「L1 放过且从没测过」——而事实是测过了。

k=1 同样适用这一整套。日常 screen 打不出分时记 `invalid` / `unscoreable`；没有 baseline
时记 `screen`，因为 screen 本来就不比较，「没有参照」对它不是新消息。

**两种记录都没有豁免字段。** batch 模式下回归的处置是合并后 revert（见 [L2 触发粒度](#l2-触发粒度)）；没有可阻断的合并，就没有要豁免的东西。

### 两种记录如何 join

L2 漏放率的分母是「跑了 L2 且 Pass 的 MR」。batch 模式下没有任何 MR 记录满足这个条件，因此分母必须经 run 记录取得：

> 被某次 `conclusive` 且 `outcome = pass` 的 run 的 `covers` 认领、且已合并的 commit。

join 方向只能是 run → MR。MR 记录在 L1 判定时就写入，run 记录在运行结束后才写入；若改为在 MR 记录上回填 run 指针，就需要修改已写入的记录，而 MR 记录写入后不再修改——唯一的例外是 `fail_open` 被真实判定替换。

### 健康指标

每季度统计一次，三个指标各自对应一层的失效：

| 指标 | 分母 | 分子 | 阈值与处置 |
|---|---|---|---|
| **L1 假阴性率** | 判 invisible 且**没有任何 run 覆盖过**的 merge（见下） | 其中经 L3 bisect 或弥散型补跑确认引入 Primary 回归（含 cost / turns）的 MR | > 5%：先看漏掉的类别能否写成路径规则，其次收紧 agent 的 prompt；不通过"换更强的模型"解决 |
| **L1 visible 率** | 全部 MR | 判为 visible 的 MR | 长期 > 60%：L1 名存实亡，等于每个 MR 都跑 L2，只贡献延迟。先排查是否在只改测试文件、只改注释这类 MR 上误触发，再考虑收窄可见面定义 |
| **L2 漏放率** | 被某次 conclusive 且 Pass 的 run 覆盖的 merge | 其中日后被确认引入 Primary 回归的 MR | > 5%：L2 阈值过松 |

L2 漏放率是 [L2 门禁](#l2-门禁)阈值的唯一实证来源。那些阈值的初值均为拍定，不靠这个指标就永远没有修正依据。

**为什么假阴性率的分母不是「所有 invisible 的 merge」。** L2 跑的是窗口 head，所以一个
invisible merge 的代码，只要同窗口里有任何一个 merge 触发了运行，就已经在被测构建里了。
把这些也算进分母，等于拿 L2 实际测过的代码去问责 L1。只有**没有任何 run 覆盖过**的 merge，
才是真正因为 L1 判 invisible 而没被测到的。

**「覆盖过」的判据是 `measured`，不是「有一条 run 记录」。** harbor 崩了、打分炸了、结果
不成立的运行什么也没测出来，让它 cover 等于把不存在的覆盖记到 L1 头上——一次 harness 崩溃
会被读成一批 L1 漏放。反过来 `no_reference` 必须 cover：它测过了，而且它的窗口已经推进，
不会再有运行回头覆盖那批 merge。

**为什么 k=1 的 run 不能 clear 一个 merge。** 日常 batch 是 k=1，其结论按
[Experiment parameters](#experiment-parameters) 不作数；一个只被 screen 覆盖过的 merge
仍在等判定，不进漏放率的分母。否则 k 规则等于白立——漏放率会把一批从未被真正判过的 merge
算作「L2 说它没问题」。

## Reproducibility requirements

以下是判定结果可信的前提，任一不满足则该次 run 的结论无效：

1. 任务定义 pin 与 baseline 一致
2. model id、版本、temperature 与 baseline 一致
3. 容器资源规格与 baseline 一致，每任务 fresh restore
4. prompt 中的时间、路径、id 等易变输入**受控**（见下表），而非被移除
5. 完整 trajectory 已落盘（回归发生时要看的是轨迹 diff，不是分数）
6. infra failure 与 task failure 分开计数

### 易变输入的控制方式

不能通过删除 `<environment_context>` 字段来满足第 4 条——删掉会改变 agent 本身。评测是配对设计，**恒定的混杂因子在配对中抵消，变动的不会**，因此要求是"两臂取值一致"，不是"取值真实"。

| 字段 | 来源 | 控制方式 |
|---|---|---|
| `cwd` | `dimcode exec` 的工作目录 | 容器固定挂载路径 |
| `home` / `shell` / `platform` | 从 `HostEnv` 解析 | 容器 env 固定 |
| `timezone` | 进程本地时区 | 固定 `TZ` |
| `current_date` | [`local_today`](../../crates/dim-agent-capabilities/src/prompt_sources.rs)（`chrono::Local::now()`） | `install_with_clock` 可注入，但 `dimcode` 未暴露，**见 Open items** |
| session id / run id | 运行时生成 | 仅 prompt 回归测试需要归一化；L2/L3 不参与字节比对，无需处理 |

冻结日期的取值随 baseline 走：**重采 baseline 时重新钉定，与升级任务 pin 同一个 MR**，取值记入 `benchmarks/baselines/<pin-sha>.json`。同一 baseline epoch 内它是常量，配对有效；跨 epoch 跳变，但跨 epoch 本来就因任务 pin 变更而不可比。

不使用 `libfaketime` 之类冻结整个容器时钟的做法：那会同时影响任务 verifier、构建工具和文件时间戳，可能直接让任务失败，风险大于收益。

## Open items

| 项 | 状态 |
|---|---|
| `benchmarks/` Python harness 重建并提交 | 已完成 |
| L1 review agent 与 `benchmarks/ledger/` 落盘 | 已完成 |
| baseline 首次采集 | Rust `dimcode` 尚无 baseline。TS 版的 baseline（2026-09，被测 v0.5.1）测的是另一个 agent，不迁入。**待用干净的 harness 在某个 main commit 上采集 k=3** |
| 阈值重标：现有阻断阈值[已被证伪](#l2-门禁)，重标前 L2 不得 blocking | 未开始，**阻塞 L2 成为门禁** |
| 真 A/A（同一构建独立跑两次 k=3）以取得真实噪声底 | 未开始；现有区间由三次 run 重采样得出，是下界 |
| 定时调度（评测机 cron + [`host/nightly.sh`](../../benchmarks/host/nightly.sh)，问题自动提 GitHub issue） | 已实现。上线前需在评测机配置部署密钥（推 `eval-ledger`）与只含 Issues 权限的 token，并建 `eval-ledger` 分支，见 [host/README.md](../../benchmarks/host/README.md) |
| L1 逐文件判定 + 可查仓库（[实现约束](#实现约束)要求，实现从未落实）| 未开始。当前用路径规则与体积守卫兜底：端点上限 880k token，而 8% 的 merge 超过 1.4 MB |
| CODEOWNERS：控制面豁免目前没有对应保护，需覆盖 `benchmarks/**`、`docs/eval/**` | 未开始，已在 [always-eval.toml](../../benchmarks/policy/always-eval.toml) 注明 |
| 宿主机模型代理（现实现把 key 注入容器，见 [禁网与模型代理](#禁网与模型代理)） | 未开始 |
| prompt 回归测试录制链路（不阻塞 pipeline） | 未开始 |
| `current_date` 冻结：`prompt_sources::install_with_clock` 已能注入时钟，`dimcode` 尚无入口传入冻结日期 | 未开始 |
| [日常 screen 的判定](#日常-screen-的判定)：累积统计量、`profile` 运行模式、报警动作契约 | 设计已定，**实现未开始**。观测那一半已在跑：日常 k=1 每晚留 `run.json` 并写一条 run 记录（`screen`，打不出分则 `invalid`）。缺的是把多晚的 screen 累积成「该不该跑 k=3」的判断 |
| `settle` 的异常未捕获：异常发生在写 manifest 之前，烧掉的 2–7 小时连 `run.json` 都不留 | **已完成**。异常记入 `run.json` 的 `settle_error`，窗口保持，退出码 6；「baseline 已存在」改为构建前拒绝 |
| `baseline` 运行写的 ledger 记录 outcome 为 `pass`，而它没有做过任何比较（今天无害：`covers` 为空）。与刚修掉的 k 不匹配记 `pass` 是同一形状，需决定是改记录方式还是不记 | 未开始 |
| `harbor_version` 只进 `params`，而 `params` 仅用于冻结 baseline——`run.json` 里没有这个字段，`freeze` CLI 产出的 baseline 里也没有。[三重 pin](#实现约束) 的第③项从未落盘 | 未开始 |
| 单次运行成本预算上限（batch 模式下按批计，不按 MR） | 待定 |
| [OfficeQA Pro](#l2--officeqa-pro)：生成 `benchmarks/officeqa/tasks/`、推送语料镜像、oracle 自检、冻 k=3 baseline、加 `nightly.sh officeqa-pro daily` 的 cron | 任务已生成并通过容器内 oracle 自检（133/133）；待在评测机上跑 harbor oracle、冻 baseline、加 cron，见 [officeqa/README.md](../../benchmarks/officeqa/README.md) |
| `expr-try-catch-errors` 之外，另两个仍指向无后缀镜像代次的 DeepSWE 任务是否对齐 | 已决定不动，见 [UPSTREAM.md](../../benchmarks/tasks/UPSTREAM.md) |

## References

- [AGENTS.md](../../AGENTS.md) — `ToolResult.content` 边界定义
- [frontier-harness-eval/eval](https://github.com/frontier-harness-eval/eval) — L2 任务源
- [databricks/officeqa](https://github.com/databricks/officeqa) — OfficeQA Pro 数据集说明与 `reward.py`
- [Terminal-Bench (arXiv 2601.11868)](https://arxiv.org/abs/2601.11868) — L3 任务源与方法学
