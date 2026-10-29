# 任务集上游 pin

状态: Draft
日期: 2026-09-10

L2 任务集是 [frontier-harness-eval/eval](https://github.com/frontier-harness-eval/eval)
选定的 30 个任务（21 Terminal-Bench 2.1 + 9 DeepSWE）。选择理由与治理规则见
[trigger-policy.md](../../docs/eval/trigger-policy.md#subset-governance)。

## 任务不是一个上游，是三个

frontier-harness-eval 的 `tasks/<task>/` **只有 `task.toml` 和 `instruction.md`**，
没有 verifier、没有环境定义。任务体在另外两个上游。

| 上游 | 提供 | pin | 覆盖范围 |
|---|---|---|---|
| frontier-harness-eval/eval | 选择列表 | commit `8f11b130c30bbf76ca1f3edeea70abc773bd8d2c`（2026-09-03） | 见下 |
| laude-institute/terminal-bench | 21 个任务体与配置 | 见[版本无法在 fetch 时锁定](#版本无法在-fetch-时锁定) | 全量采用 |
| datacurve-ai/deep-swe | 9 个任务体 | commit `0b9fabbb63b9104d678fe965e1632f2dd9eaa2ea` | 被 selection 覆盖 `task.toml` + `instruction.md` |

### 覆盖范围按上游区分，依据是实测而非取舍

**Terminal-Bench：selection 的配置不可执行，只取其成员资格。**

它给 21 个任务**统一**设了 `allow_internet = false`，并**统一**注入了相同的 `PATH`——
整齐划一的全量重写是生成式报告产物的特征，不是逐任务策展。镜像也印证：
selection 与上游指定的镜像 21/21 完全相同，而镜像里既没有这些 verifier 判分时要下载的
工具链（无 curl、无 uv），也没有 selection 自己改写的 instruction 所引用的离线资产
（`build-cython-ext` 让 agent 用 `/app/pyknotid.bundle`，该文件不在镜像里）。

按 selection 的配置采集的第一次 baseline，21 个 TB 任务全部得 0：90 个 trial 里 60 个
从未拿到判定，因为 verifier 无法安装自己；7 个任务的参考解法需要 `pip install` 指定版本
因而无解；8 次超时全部落在这 7 个任务上。改用上游配置后同一套语料通过率 11% → 50%，
infra 失败率 5.6% → 1.1%。

**DeepSWE：selection 的唯一配置改动是把 agent 超时从 10800s 收到 5400s。**
那是预算决策，可执行，同一次 run 中这些任务判分正常，因此保留。

## 授权状态

| 上游 | 授权 |
|---|---|
| laude-institute/terminal-bench | Apache-2.0 |
| datacurve-ai/deep-swe | Apache-2.0；各任务所依赖的上游项目另有 MIT / BSD-3-Clause / Apache-2.0，逐条列在其 `PROVENANCE.md`，均为宽松许可 |
| frontier-harness-eval/eval | **无 LICENSE 文件，`package.json` 无 `license` 字段，README 无授权表述** |

frontier 那 30 个 `task.toml` 与 9 个 `instruction.md` 已按上表装配并 vendor 进本目录。
决定是**仅限内部自用**：本仓库不对外分发这部分内容。

对外开源本仓库、或把 `benchmarks/tasks/` 单独发布之前，必须重新处理 frontier 这一层的授权——
另外两个上游是 Apache-2.0，届时只需补 attribution。

## DeepSWE 镜像的两代 tag

上游的 DeepSWE 镜像同时存在两代 tag：`<id>` 与 `<id>-v1.1`，**内容不同，不是重打 tag**。
vendored 的 9 个任务里 6 个指向 `-v1.1`，3 个指向无后缀那一代（`expr-try-catch-errors`、
`httpx-multipart-response-parsing`、`python-statemachine-state-data-scoping`）。

这个不一致本身保留，如实反映上游的混发状态。**只有一个任务改了代次**：

| 任务 | 改动 | 依据 |
|---|---|---|
| `expr-try-catch-errors` | 无后缀 → `-v1.1` | 其 verifier 依赖 `go-ctrf-json-reporter`，该工具**只存在于 `-v1.1` 镜像**；任务全程 `no-network` 装不上，因此在无后缀镜像下判分链直接断开，reward 恒为 `-1`（未判定），与 agent 表现无关 |

实测证据（2026-09-14，被测 `dim-agent-under-test:ca7c664ae`）：

- 无后缀镜像内 `/root/go/bin` 不存在；`-v1.1` 镜像内为 `/root/go/bin/go-ctrf-json-reporter`。
- 换 `-v1.1` 后单任务复跑，verifier 完整执行并产出 `reward.json`：
  `P2P 66265/66265`、`F2P 78/79`、`BINARY 0`。判分链恢复，结果为有效的 0 分。
- 另两个依赖同一 reporter 的任务（`anko-typed-variable-bindings`、
  `scc-bounded-memory-spilling`）本就指向 `-v1.1`，判分一直正常。

**另外两个无后缀任务不动。** 它们的 verifier 不依赖该 reporter，判分正常
（`httpx` 0/3、`python-statemachine` 3/3）。为了 tag 整齐去换镜像，等于用一个已知可用的
被测环境换一个未知的；`python-statemachine` 还是当前 10 个「3/3 稳定锚点」之一，而锚点集
是[全翻转判据](../../docs/eval/trigger-policy.md#l2-门禁)的全部依托。

## 版本无法在 fetch 时锁定

Terminal-Bench 经 `harbor download terminal-bench/terminal-bench-2-1` 获取，
harbor 解析 `@latest`，registry 不提供不可变 ref（`@v2.1.0` 不存在），
下载产物也不带版本元数据。

因此 **tag 与 registry 版本都不是 pin**，pin 只能是内容本身：
逐任务 digest 记在装配产物的 `MANIFEST.json`，任一任务、verifier 或镜像漂移都会使校验失败。

## 任务环境镜像

| 来源 | 数量 | 镜像 |
|---|---:|---|
| Terminal-Bench 2.1 | 21 | `alexgshaw/<task>:20251031`（Docker Hub） |
| DeepSWE v1.1 | 9 | `public.ecr.aws/d3j8x8q7/swe-bench-202605:<ext_id>-v1.1` |

verifier 与测试都在镜像里，所以镜像同样是任务定义的一部分。
tag 可被上游重推，判定以 digest 为准；digest 清单待镜像预热完成后回填。

## 任务资源规格

来自 `task.toml`，harness 按此分配，不得自行调整——资源规格是 baseline 可比性的前提。

| 项 | 取值分布 |
|---|---|
| `cpus` | 1 / 2 |
| `memory_mb` | 2048 / 4096 / 8192 |
| `agent.timeout_sec` | 600 / 900 / 1200 / 1800 / 5400 |
| 网络 | **全部任务禁网**；模型 API 经 agent 阶段的 `extra_allowed_hosts` 单独放行，verifier 阶段保持隔离 |

单任务内存上限 8 GB 决定并发上限，进而决定单次 L2 的墙钟时长。

## 升级规则

升级任一 pin 属独立 MR，必须同时重跑 baseline 并更新 `benchmarks/baselines/`，
不得与功能改动同 MR。详见 [trigger-policy.md](../../docs/eval/trigger-policy.md#subset-governance)。
