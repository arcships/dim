# benchmarks

dim-agent 的能力评测 pipeline。回答的问题是：**这次改动会不会让 agent 变差？**

触发规则、验收标准与分层设计的单一事实来源是
[docs/eval/trigger-policy.md](../docs/eval/trigger-policy.md)。本目录只放实现与数据。

## 目录归属

| 路径 | 内容 | 谁写 |
|---|---|---|
| `daily_run.py` | 每日流水线入口：串联 L1 → 构建 → L2 → 产物 | 实现代码 |
| `policy/always-eval.toml` | 直接判 visible 的路径规则，绕过 L1 agent | 人工维护 |
| `l1/` | L1 判定：canonical diff、路径规则、review agent、verdict cache | 实现代码 |
| `build/` | 批次识别（`daily_batch.py`）与被测镜像构建投递（`build_image.py`） | 实现代码 |
| `tasks/` | vendored 的 30 个 L2 任务定义 + [上游 pin](tasks/UPSTREAM.md) | 升级 pin 的 MR |
| `officeqa/` | OfficeQA Pro：任务生成器、模板、语料镜像；生成的 133 个任务在 `officeqa/tasks/`，见 [officeqa/README.md](officeqa/README.md) | 升级 pin 的 MR |
| `runner/` | 运行器：任务编排、dim adapter、结果解析（`results.py`）、baseline 与配对比较（`baseline.py`） | 实现代码 |
| `ledger_tools/` | ledger 的 schema、append-only store、审计与健康指标 | 实现代码 |
| `baselines/<pin-sha>.json` | 每任务 k 次的通过分数与成本，按任务 pin 分文件 | baseline 采集 |
| `ledger/` | 每个 PR 一条判定记录，长期保留；定时运行写在 `eval-ledger` 分支 | 评测机写入 |
| `host/` | 评测机部署：cron 入口、工具链安装，见 [host/README.md](host/README.md) | 实现代码 |
| `report/` | 把每晚的问题写成 GitHub issue | 实现代码 |

## 分层

| 层 | 触发 | 内容 | 是否阻断 |
|---|---|---|---|
| L1 | 每个 merge | review agent 判定 diff 是否触及模型可见面 | 否，只决定是否跑 L2 |
| L2 | 窗口内任一 merge 判 visible 时 | 本目录 `tasks/` 的 30 任务 × k（日常 1，定论 3）；另有 OfficeQA Pro 133 题，独立语料、独立 schedule | 日常只记 `screen` 或 `invalid`；阈值重标前不阻断 |
| L3 | 每周五 + release 前 | Terminal-Bench 2.1 全量 + DeepSWE 全量 | 否，趋势与兜底 |

## 运行环境

L2/L3 在专用 Linux amd64 + Docker 机器上运行。任务全部 `no-network`，
容器内默认够不到模型 API，这不是可选项，见
[tasks/UPSTREAM.md](tasks/UPSTREAM.md#任务资源规格)。

**当前实现没有宿主代理**：按 trial 放行模型主机（harbor 的 `extra_allowed_hosts`），
key 经 agent env 注入容器。**key 会进 agent 容器**，这是当前接受的代价，不是设计目标。
完整的边界与代价见
[trigger-policy.md · 禁网与模型代理](../docs/eval/trigger-policy.md#禁网与模型代理)——
那里是这条规则的唯一事实来源。

被测对象与评测工具必须分离：

- **被测对象** = 被测 commit 构建出的 `dimcode`，按发布包结构打成镜像只读挂载进任务容器
- **评测工具** = 本目录的 harness、任务定义、baseline、阈值，**始终取自 main**

否则一个 PR 可以改掉评判自己的尺子。

这条由 `daily_run.py` 强制，不是靠自觉：`benchmarks/` 有未提交改动时，运行在判定窗口
和构建镜像**之前**就被拒绝。

- 正式运行：先提交，或显式传 `--allow-dirty`。后者会把 `runner_dirty` 写进 ledger run
  记录——放行不等于洗白，读记录的人必须能看出这个数字复现不出来。
- **baseline 没有这个开关**：`--allow-dirty` 也不行，schema 层面直接拒。它是之后所有
  比较的参照，用只存在于一台机器上的代码定义它，等于让所有后续比较都无法溯源。
  冻结和读取两侧都查：已经存在的 dirty 参照不会被拿来比较，这次运行记为「没有可比的
  baseline」（退出码 `7`），直到有人重采。

## 运行

`daily_run.py` 在评测机（Linux amd64）上运行：构建需要
[host/setup-toolchain.sh](host/setup-toolchain.sh) 装好的工具链与系统根证书，L2 与 judge 经 ssh
到 `DIM_EVAL_HOST`，并读取 `DIM_EVAL_SCRATCH`、`DIM_EVAL_REMOTE_PYTHON`（见
[host/README.md](host/README.md)）。只有 `--dry-run` 不带 `--remote-judge` 时可以在别处运行。

```bash
# 每日粗筛：窗口内 L1 判定 + k=1 的 L2
python3 benchmarks/daily_run.py --remote-judge

# 只做 L1 判定，不构建也不跑 L2
python3 benchmarks/daily_run.py --dry-run

# 基线采集：在某个 commit 上做 k=3 参照测量，不判窗口、不推进 state
python3 benchmarks/daily_run.py --run-type baseline --head <commit>

# OfficeQA Pro：同一入口，换语料、并发与 state
python3 benchmarks/daily_run.py --remote-judge --tasks-dir benchmarks/officeqa/tasks \
    --concurrency 10 --state benchmarks/ledger/officeqa-pro-state.json
```

| run type | k | 窗口 | 可否定论 |
|---|---:|---|---|
| `daily` | 1 | 判定并推进 | 否，粗筛 |
| `checkpoint` | 3 | 判定并推进 | 是 |
| `baseline` | 3 | 不判、不推进（参照测量） | 是 |

k 由 run type 决定，`--k` 可覆盖；但 `checkpoint` / `baseline` 低于 k=3 直接拒绝运行——
[trigger-policy](../docs/eval/trigger-policy.md#experiment-parameters) 规定 k=1 不得作为
判定依据，写出一份 k=1 的 baseline 之后没有任何比较能看出它撑不起结论。

### 定时运行

由评测机上的 cron 调用 [host/nightly.sh](host/nightly.sh)，每个语料一条：`frontier` 跑 30 个
FrontierHarness 任务，`officeqa-pro` 跑 OfficeQA Pro（只允许 daily）。它跑的就是上面那条命令，
区别只在三处：ledger 与 state 存在 `eval-ledger` 分支；`baseline` 被显式拒绝——换参照是要
review 的改动，不是 cron 的副产品；跑完后由 [report/issues.py](report/issues.py) 把问题写成
GitHub issue（标签 `eval-pipeline`）。部署与前提见 [host/README.md](host/README.md)。

### 窗口

窗口是 `state.last_l2_head..origin/main`，**起点是上次真正跑过 L2 的 commit**，不是上次
看过的 commit。只有批次落定才推进，落定要求三件事同时成立：L2 跑完、没有未判定的 MR、
**结果打得出分**。L2 失败、judge 失联、或 run 跑完却缺 trial／未打分／infra 超限时窗口
保持不动，下一次运行继续覆盖这些 merge。

第三条是分开写的：harbor 退出 0 只说明 job 跑过了，不说明它可以被打分。少了这一条，一批
`invalid` 的 run 会照常推进窗口——那批 merge 就此退出所有未来窗口，而流水线什么都不报。

「没有 baseline 可比」不在此列。它同样让这次 run 记为 `invalid`，但测量本身是好的，重跑
也变不出一份 baseline——只有人能去冻一份。为它 hold 窗口会卡在一个重跑永远清不掉的条件上。

直推 main 的 commit 没有 MR 可判，L1 看不到它们。这类 commit 强制触发 L2 并记入
`direct_pushes`，但不 hold 窗口——没有后续运行能改判它们，hold 会让窗口永久卡死。

### baseline 与比较

```bash
# 采集基线：k=3，跑完自动写 baselines/<corpus-pin>.json
python3 benchmarks/daily_run.py --run-type baseline --head <commit>
```

基线按**语料 pin**（`--tasks-dir` 下 `MANIFEST.json` 的 `tree_digest`）命名，两个语料因此各有各的 baseline，存每任务每次 trial 的
通过与否、token、轮数、结束原因，以及端点当时报告的 model、被测 commit、harness sha 和
实验参数。已存在的基线**不会被覆盖** —— 换掉它等于换掉此前所有比较的参照，必须是一次
显式提交。跑不出完整分数的 run 拒绝冻结为基线。

之后每次 run 自动与同 pin 的基线做**逐任务配对比较**，按
[L2 门禁](../docs/eval/trigger-policy.md#l2-门禁)逐条给出判据。两点按文档实现：

- **「净退化 ≤ 1」标记为非阻断**。该阈值在本语料上实测误拦率 28%(k=1)–32%(k=3)，已被证伪。
- **成本是 token，不是美元**。取 uncached input + output 的中位数；harbor 那个 `cost_usd`
  的来源是 `reported_by_dimcode`，即被测对象自报，不能当门禁输入。

任务定义 checksum 与基线不符时**拒绝比较**，而不是给出一个看起来能比的数字。

### 产物

落在 `results/<run-id>/`。run-id 形如 `2026-09-22T0814__checkpoint__4b6281ff9__a3f1`：
时间戳用来排序、类型与 head 用来识别，**末尾的随机 token 保证唯一**。它不是按「日期 + head」
生成的——那样同一天重跑同一个 commit 会与上一次共用远端结果目录，上一次的 trial 还留在
里面，第二次会连它们一起读回来（声明 3 个、目录里 6 个）。拉取时带的 `--delete` 只能让本地
与远端一致，清不掉远端目录里上一次的残留，于是因为上一次的残留而再次 `invalid`；ledger 的 write-once 也会拒掉它的记录。而重跑正是退出码
`6` 要求人做的事。

`run.json`（run type、k、`conclusive`、窗口、逐 MR 的 L1 判定、
语料 pin、镜像标识、L2 退出码）与 harbor 的原始结果。

退出码：

| 码 | 含义 | 窗口 | issue |
|---|---|---|---|
| `0` | 落定 | 推进 | 关闭该语料已消失的问题 |
| `3` | 有 PR 未被判定 | 保持 | 开（下次重判即可） |
| `4` | L2 失败 | 保持 | 开 |
| `5` | judge 一个都没判出来 | 保持 | 开 |
| `6` | run 跑完了但打不出分（缺 trial／未打分／k 不对／infra 超限） | 保持 | 开 |
| `7` | 测量好了，但没有可比的 baseline | 推进 | 开；同时关闭其余已消失的问题 |
| `8` | ledger 推送失败（`nightly.sh` 自己的码，`daily_run.py` 不返回它） | 见下 | 开；当晚不关闭任何 issue |
| `10` | issue 没能提交（`nightly.sh` 自己的码），多半是 token 过期 | — | 无法提交，见评测机日志 |

`8` 只由 [host/nightly.sh](host/nightly.sh) 产生，且只在评测本身没有别的结论时才覆盖——一次
L2 失败不该被降格成一次推送失败。远端分支没有前移，所以持久状态没坏：未推上去的 commit 留在
评测机的 ledger clone 里，下一次运行 rebase 后连同新记录一起推送，state 文件也随之补上。

`8` 与 `5` 分开是因为恢复动作不同：`5` 去查评测机上的 judge，`8` 去查部署密钥的写权限
与分支保护。

`6` 与 `4` 的区别是「跑完了但结果不完整」对「没跑完」：前者花掉了机器时间却拿不到可用的
测量，两者都必须重跑。

`7` 与 `6` 的区别是**重跑有没有用**。`6` 重跑就好了，所以窗口保持、issue 催人重跑。
`7` 重跑多少次都一样——缺的是参照，只有人能补：去冻一份 baseline（`--run-type baseline`，
走 review，不从 schedule 触发）。所以窗口照常推进，issue 保持打开并每晚追加一条，而不是天天开新的让人
习惯忽略。全新部署、baseline 还没冻那段时间这个 issue 会一直开着，那正是当时的真实状态。

**日常 k=1 对 k=3 baseline 不算这一类。** 那是设计本身：判据在不同 k 之间无定义，而 k=1
的 screen 本来就不下结论，所以「没有判据适用」对它不是新消息。只有 k≥3 的 checkpoint 撞上
不同 k 的 baseline 才是配置错误。

`5` 与 `8` 曾经是同一个码，现已分开——见上表与 `8` 那一节。

`run.json` 本身不是 ledger 记录，`conclusive` 字段说明能否由它导出一条判定：`daily` 为
`false`，k≥3 的 `checkpoint` / `baseline` 为 `true`。
