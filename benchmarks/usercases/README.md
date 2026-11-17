# usercases：Badcase 自建评测集

来自真实使用失败的 P0 自建语料。与 FrontierHarness、OfficeQA Pro 的定位不同：
那两个回答「这次改动会不会让 agent 变差」，本语料回答「**这次改动有没有把用户真实踩过的坑
重新挖开**」。素材取自本仓库公开 issue 中的真实缺陷（自带复现步骤，无隐私授权负担），
按 badcase 的机制类别转成可判分的任务；已修复的缺陷做**回归题**，未修复的做**能力题**。

- 任务树在私有仓库 [arcships/dim-eval-data](https://github.com/arcships/dim-eval-data) 的
  `eval-badcase` 分支 `usercases/` 下；本仓库只 pin
  [tasks/MANIFEST.json](tasks/MANIFEST.json)，与 `tasks/`、`officeqa/tasks/` 同一惯例。
- 运行前 `corpus.py` 逐个 digest 核对任务树与 pin；改题必须重新生成 manifest 并同步两边。

## P0 任务（2026-11-16）

| 任务 | 来源 issue | 类别 | 角色 |
|---|---|---|---|
| `uc-001-compaction-constraint-retention` | [#169](https://github.com/arcships/dim/issues/169) | 上下文与压缩 | 主任务 |
| `uc-002-compaction-constraint-control` | [#169](https://github.com/arcships/dim/issues/169) | 上下文与压缩 | 对照：短上下文同规则 |
| `uc-003-worktree-outside-workspace` | [#911](https://github.com/arcships/dim/issues/911) | 权限与工作区边界 | 主任务 |
| `uc-004-worktree-in-workspace-control` | [#911](https://github.com/arcships/dim/issues/911) | 权限与工作区边界 | 对照：工作区内同工作 |

两条主任务各配一道同规则对照题：主任务失败而对照通过，失败是「上下文/边界」形态的，
不是题目本身不可解。成对添加是本语料的固定规则；单题入集需在 PR 里说明为什么不成对。

**#169（压缩死区/截断摘要）与 #911（逐文件弹审批）的修复已合入**，四题当前都应通过——
这正是 P0 的用途：先验证整条流水线能测、能判、能区分，分数才有含义。

## 判分与有效性语义

与 runner 现有合同完全一致，无本语料特例：

- 判分是确定性的：`tests/check.sh` 重算期望值逐字段比对，写 `/logs/verifier/reward.txt`。
  判分器自身崩溃（环境坏了）退出非零、不写 reward，trial 记 unscored——**环境坏了不等于
  agent 输了**，反向也不允许：被测 harness 自己的失败是 task_fail，不得借 infra 分类剔除。
- 主任务计分所需的每条规则都写在 instruction.md 里；不考未声明的暗规则。
- k 语义沿用全局约定：k=1 是 screen 不是结论；对照/baseline 采集才允许定性比较。
- `uc-001` 是否真的触发压缩取决于模型 config 的窗口大小：压缩是否发生可从 trial 的
  Timeline 导出核对，属于**记录项**不是判分项；窗口变大后按 `tools/make_fixture.py`
  扩充语料并升 manifest 版本，不得静默换题。

## 运行方式与窗口语义

**当前没有 cron 条目，不接 issue 报告路由，state 独立**（`usercases-state.json`）。但
`daily_run.py` 是唯一的 L2 入口：daily/checkpoint 沿用与 frontier/officeqa 相同的窗口
机制，不绕开 L1——

- 首次运行必须 `--base <sha>` 初始化窗口，否则以 `no window base` 退出；
- 窗口内没有模型可见变更时 **L2 被跳过、窗口照常推进**（`no model-visible change; L2
  skipped`）——这是共享机制的省费行为，不是失败；
- 唯一不判窗口、不推进 state 的路径是 `--run-type baseline`（k=3，在指定 commit 上做
  参照测量，人工触发）；日常粗筛读的参照就是它采的。baseline 文件落在
  `benchmarks/baselines/<pin>.json`，经自己的 PR 合入——换参照是 review 过的改动。

```bash
# 采集参照（k=3，无窗口语义；先于任何日常粗筛跑一次）
python3 benchmarks/daily_run.py --run-type baseline --head <commit> \
    --tasks-dir <dim-eval-data>/usercases --concurrency 4

# 日常粗筛（评测机上；首跑加 --base <sha> 初始化 usercases-state.json）
python3 benchmarks/daily_run.py --remote-judge --tasks-dir <dim-eval-data>/usercases \
    --concurrency 4 --state benchmarks/ledger/usercases-state.json [--base <sha>]

# 上机自检：判定窗口并打印计划，不构建、不写 ledger（首跑同样需要 --base）
python3 benchmarks/daily_run.py --dry-run --no-ledger --remote-judge \
    --tasks-dir <dim-eval-data>/usercases --state benchmarks/ledger/usercases-state.json \
    --base <sha>
```

是否进入 cron，等首批结果证明可复现、有负责人处理失败之后再决定；决定本身走 review。

## 环境

任务未 pin `docker_image`：harbor 从各任务 `environment/Dockerfile` 构建
（`FROM ubuntu:24.04`，worktree 题安装 git），首次构建需要评测机能拉基础镜像与
Ubuntu 仓库。**首次上机先跑 oracle 自检**（`solution/solve.sh` 全部 1 分）与
`--dry-run`：任何任务 unscored 说明环境问题，修环境而不是改判分。

## 为什么 #1633 没有做成考题

恢复类缺陷（如 [#1633](https://github.com/arcships/dim/issues/1633) 未完成的 exec facts）
需要「进程被杀 → 重启恢复」的中断注入，而适配器每次 trial 只跑一次 `dimcode exec`、
验证器阶段禁网，标准 Harbor 任务表达不了这个场景。这类缺陷归产品 flow tests
（修复已带），本语料只收容器内可判分的行为。

## 入集规则

- 只收公开来源（本仓库 issue）或已获授权的真实失败；真实用户数据走反馈授权流程，不入公共仓。
- 一个根案例最多派生主任务 + 对照各一；派生关系记在 manifest 的 `kind`/`source_issue`。
- 题目、判分、fixture 全部确定性可复现；改题升版本，不覆盖历史 baseline。
