# 评测机部署

每日评测由评测机自己的 cron 触发，不经 GitHub Actions：评测机连不上 `github.com`
的 HTTPS，但能用 SSH 拉推代码、能访问 `api.github.com`。整条流水线因此是：

```
cron ─► nightly.sh ─► git fetch dim 与 dim-eval-data（SSH）─► corpus.py 核对 ─► daily_run.py ─► 推 eval-ledger（SSH）
        ─► report/analyze.py（DeepSeek）─► report/issues.py（API）
```

| 文件 | 作用 |
|---|---|
| [`nightly.sh`](nightly.sh) | cron 入口：取 main、跑一个语料、推 ledger、提 issue |
| [`setup-toolchain.sh`](setup-toolchain.sh) | 把 Rust、Zig、cargo-zigbuild 与仓库钉住的 Node 装进一个私有目录 |
| [`../report/analyze.py`](../report/analyze.py) | 逐个分析失败的 trial：原因与可疑 PR |
| [`../report/issues.py`](../report/issues.py) | 每个语料一个失败跟踪 issue（出现开、新增评论、清零关）；流水线问题同类只开一个、复现时评论、消失时关闭 |

机器相关的值（地址、路径、端点、凭据）只放在评测机上，不进仓库。

## 一次性准备

以下以 `DIM_EVAL_HOME=~/dim-eval`（默认值）为例。

1. **工具链。** `bash setup-toolchain.sh`。只写 `~/dim-eval/toolchain`，不改机器默认的 Rust/Node。
2. **harbor。** 装在一个 venv 里，路径填到下面的 `DIM_EVAL_REMOTE_PYTHON`。
3. **SSH 回环。** `daily_run.py` 通过 ssh 在评测机上跑 L2 与 L1 judge，评测机要能 ssh 到自己
   （`DIM_EVAL_HOST`）。不为本机执行开特例：手工复现的 run 与定时跑的是同一个 run。
4. **GitHub 部署密钥，两把。** GitHub 不允许一把部署密钥用在两个仓库上：
   - `arcships/dim`：**只读**，拉被测代码与 harness；
   - `arcships/dim-eval-data`（私有，任务树与 `eval-ledger` 分支）：勾选 *Allow write access*，
     推 ledger 用。

   每把用一个专用主机别名，不影响这台机器上别的 GitHub 访问：

   ```
   # ~/.ssh/config
   Host github-dim
     HostName github.com
     IdentityFile ~/.ssh/dim-eval-dim
     IdentitiesOnly yes
   Host github-dim-data
     HostName github.com
     IdentityFile ~/.ssh/dim-eval-data
     IdentitiesOnly yes
   ```

   可写的那把只能动 dim-eval-data；在那里给 `eval-ledger` 设 ruleset，禁止 force push 与删除
   （ledger 只追加）。
5. **issue token。** fine-grained personal access token，只授权本仓库，权限只给
   *Issues: Read and write*。写入 `~/dim-eval/github-token`，`chmod 600`，只由 `issues.py`
   读取。token 有过期时间，过期后提 issue 失败，`nightly.sh` 以退出码 `10` 结束，日志里有警告。
6. **模型。** `~/.dim-eval-env` 放模型凭据，以及 `DIM_EVAL_MODEL_CONFIG`（指向评测用的 dimcode
   模型配置，格式同 `crates/dim-agent-aimux/examples/openai.json`）：

   ```sh
   export MODEL_API_KEY=...             # 模型配置里 apiKeyEnv 指向的变量
   export DEEPSEEK_API_KEY=...          # L1 judge 与失败分析
   # 可选：DIM_EVAL_ANALYSIS_BASE_URL、DIM_EVAL_ANALYSIS_MODEL、DIM_EVAL_ANALYSIS_KEY_ENV
   # 换掉失败分析用的模型（默认同 L1 judge）
   export DIM_EVAL_MODEL_CONFIG=$HOME/dim-eval/model.json
   ```

   模型配置决定端点、模型和推理强度，它的文本原样写进 job 记录，所以凭据只能用 `apiKeyEnv`
   引用，不能写进 `headers`（`run_l2.py` 会拒绝）。换配置等于换实验条件，要重采 baseline。
7. **`~/dim-eval/config.env`**（不放凭据）：

   ```sh
   DIM_EVAL_HOST=user@eval-host          # ssh 回环的目标
   DIM_EVAL_SCRATCH=/home/user/dim-eval-scratch
   DIM_EVAL_REMOTE_PYTHON=/path/to/harbor-venv/bin/python
   DIM_EVAL_GIT_REMOTE=git@github-dim:arcships/dim.git
   DIM_EVAL_DATA_REMOTE=git@github-dim-data:arcships/dim-eval-data.git   # 任务树；ledger 默认也在这里
   DIM_EVAL_REDACT="models.example.internal proxy.example.internal"   # issue 正文里额外抹掉的字符串
   DIM_EVAL_VERIFIER_PROXY=http://proxy.example.internal:port   # 只给判分阶段用的 HTTP 代理，不许带凭据
   # 可选：额外的 NO_PROXY 条目，逗号分隔（例如 compose service 名）
   # DIM_EVAL_VERIFIER_NO_PROXY=main,my-verifier
   # 可选：DIM_EVAL_TOOLCHAIN、DIM_EVAL_LEDGER_REMOTE、
   #       DIM_EVAL_LEDGER_BRANCH（默认 eval-ledger）、DIM_EVAL_GITHUB_REPO（默认 arcships/dim）、
   #       CARGO_TARGET_DIR（跨天复用依赖的编译产物）
   ```

8. **ledger 分支。** 在 dim-eval-data 建一个只装 ledger 的 orphan 分支 `eval-ledger`，根目录放
   `daily-state.json` 与 `officeqa-pro-state.json`，内容都是
   `{"last_l2_head": "<从哪个 commit 之后开始评测>"}`。窗口起点不允许猜，缺了它运行直接失败。
9. **cron。** 先 `git clone git@github-dim:arcships/dim.git ~/dim-eval/repo`，再：

   ```
   0 18 * * * /home/user/dim-eval/repo/benchmarks/host/nightly.sh frontier daily
   0 21 * * * /home/user/dim-eval/repo/benchmarks/host/nightly.sh officeqa-pro daily
   ```

   两条共用一把锁，后一条等前一条结束。日志在 `~/dim-eval/logs/`，harbor 结果在
   `~/dim-eval/results/`，保留 30 天。

## verifier 缓存

30 个任务中有 18 个 Terminal-Bench verifier 会在评分时安装 uv，其中部分还会下载 CPython。
评测机连 GitHub 不稳定时，这两步会把一次正常的 agent 运行变成 0。评测机预先准备一个只读
缓存，`run_l2.py` 把它挂到任务容器的 `/opt/eval-cache`：

```sh
. ~/dim-eval/config.env
cache=~/dim-eval/eval-cache
mkdir -p "$cache/curl-home" "$cache/python" "$cache/uv"
printf 'http1.1\n' > "$cache/curl-home/.curlrc"
export HTTP_PROXY="$DIM_EVAL_VERIFIER_PROXY" HTTPS_PROXY="$DIM_EVAL_VERIFIER_PROXY"
export CURL_HOME="$cache/curl-home"
# 任意一个 uv 0.9.5 的二进制都可以用来填充 CPython 缓存。
curl -LsSf -o /tmp/uv.tar.gz \
  https://github.com/astral-sh/uv/releases/download/0.9.5/uv-x86_64-unknown-linux-gnu.tar.gz
tar -xzf /tmp/uv.tar.gz -C "$cache/uv" --strip-components=1
UV_PYTHON_INSTALL_DIR="$cache/python" UV_CACHE_DIR=/tmp/uv-cache \
  UV_HTTP_TIMEOUT=300 "$cache/uv/uv" python install 3.13.9
```

`UV_PYTHON_INSTALL_DIR` 让 `uvx -p 3.13` 命中 `cpython-3.13.9`；`CURL_HOME` 让 curl 用
HTTP/1.1，避开代理的 HTTP/2 问题。uv 安装本身仍从 astral.sh 下载（verifier 脚本无条件执行
安装器），`UV_INSTALLER_GITHUB_BASE_URL` 不设置，所以缓存缺失时安装器会走网络而不是失败。
`UV_CACHE_DIR` 放在容器的 `/tmp`，每个 trial 独立。`run_l2.py` 只在 `python/` 下找到
`cpython-3.13.*` 时才导出 `UV_PYTHON_INSTALL_DIR`；缺失时打印 warning，让 verifier 回退到
网络下载。prep 必须在能访问 GitHub（或能用代理访问）的机器上执行，否则目录会留空。缓存里的
CPython 版本是固定的 harness 条件；升级 `cpython-3.13.*` 需要按新的 baseline 重采，不能把
旧 baseline 和新缓存混在一起比较。

## 试运行

```sh
~/dim-eval/repo/benchmarks/host/nightly.sh frontier daily --dry-run
```

只判定窗口、打印计划，不构建、不跑 L2、不推进窗口；issue 只打印、不提交。L1 判定照常写入
ledger 并推送到 `eval-ledger`。

## 信任边界

被测代码是 main 上已合并的 commit，构建时会以评测机用户的身份执行它依赖里的构建脚本
（`build.rs`、过程宏；npm 依赖以 `--ignore-scripts` 安装）。凭据不进入构建的环境变量，但
**这个用户能读到的文件，构建也能读到**：部署密钥、issue token、`~/.dim-eval-env` 里的模型
凭据。这与旧的 GitLab runner 是同一个前提——信任的是「进了 main 的代码」，靠的是 PR review，
不是隔离。要收紧它，需要在容器里或以另一个用户身份构建；在那之前，泄露的密钥最多能读 dim、
写 dim-eval-data，而 `eval-ledger` 的 ruleset 挡住改写与删除。

失败分析把 agent 的输出和判分日志交给模型，这些文本可能含有诱导模型的内容。提示词声明它们
只是数据；模型的输出只进 issue 正文（先脱敏），不驱动任何动作，可疑 PR 也只能从窗口内的 PR
里选。

## 被测产物

`build/build_image.py` 以 `cargo zigbuild` 编出 glibc 2.34 的 `dimcode`（release
仍用 `scripts/build-all.mjs` 的 2.28 目标），用发布脚本 `scripts/stage-plugins.mjs` 准备默认插件，按发布包的
目录结构打成只读挂载的镜像：

```
/opt/dim-agent/
  bin/dimcode
  default-plugins.json
  plugins/…
  ca-certificates.crt     评测机的根证书
  eval-build.json         被测 commit 与构建信息
```

另外两处评测环境与用户机器不同，都只改 dimcode 自己的配置，不改题目文件系统：

- `ca-certificates.crt` 不属于发布包。dimcode 为每个 HTTPS 客户端读取系统根证书，题目镜像里没有
证书时 App Server 无法启动；adapter 用 `SSL_CERT_FILE` 指向这份证书。
- 判分阶段可走 HTTP 代理（`DIM_EVAL_VERIFIER_PROXY`）。30 个 FrontierHarness 任务里有 18 个
  的判分脚本在打分时从 GitHub 下载 uv；评测机直连不上 GitHub 时它们什么都没测就记 0 分。代理
  只经 harbor 的 `verifier.env` 进入判分阶段；agent 阶段不受影响。代理 URL 必须是裸的
  `http://`/`https://` 地址，**不能带用户名或密码**：job config 会写进评测机磁盘，带凭据的 URL
  会随 Harbor 的 trial 记录扩散；代理认证请用网络 ACL。job config 以 `0600` 写入。
  `NO_PROXY` 默认包含 `localhost,127.0.0.1,::1,host.docker.internal`，需要额外绕过 compose
  service 名时设 `DIM_EVAL_VERIFIER_NO_PROXY`。第一次启用时用会启动本地 server 的任务做一次
  smoke test；若 verifier 通过 service 名访问，把该名字加进这个变量。每次运行都把
  `verifier_proxy` 和 `verifier_no_proxy` 记入 `run.json`，baseline 同时记入 params；任一项
  不一致，或旧 baseline 没有该字段（未知）时，comparison 判为不可比，需要在同一代理条件下
  重采 baseline。
- 命令沙箱默认关闭（`/settings > Execution` 的 Sandbox），不再写也不再读 `host-isolation.json`；
  容器本身就是隔离边界。dimcode 的沙箱依赖 bubblewrap，题目镜像里没有，打开 Sandbox 后每个
  trial 的第一条命令会以 `isolation_unavailable` 失败。
