# 评测机部署

每日评测由评测机自己的 cron 触发，不经 GitHub Actions：评测机连不上 `github.com`
的 HTTPS，但能用 SSH 拉推代码、能访问 `api.github.com`。整条流水线因此是：

```
cron ─► nightly.sh ─► git fetch（SSH）─► daily_run.py ─► 推 eval-ledger（SSH）─► report/issues.py（API）
```

| 文件 | 作用 |
|---|---|
| [`nightly.sh`](nightly.sh) | cron 入口：取 main、跑一个语料、推 ledger、提 issue |
| [`setup-toolchain.sh`](setup-toolchain.sh) | 把 Rust、Zig、cargo-zigbuild 与仓库钉住的 Node 装进一个私有目录 |
| [`../report/issues.py`](../report/issues.py) | 把当晚的问题写成 GitHub issue，同类问题只开一个、复现时评论、消失时关闭 |

机器相关的值（地址、路径、端点、凭据）只放在评测机上，不进仓库。

## 一次性准备

以下以 `DIM_EVAL_HOME=~/dim-eval`（默认值）为例。

1. **工具链。** `bash setup-toolchain.sh`。只写 `~/dim-eval/toolchain`，不改机器默认的 Rust/Node。
2. **harbor。** 装在一个 venv 里，路径填到下面的 `DIM_EVAL_REMOTE_PYTHON`。
3. **SSH 回环。** `daily_run.py` 通过 ssh 在评测机上跑 L2 与 L1 judge，评测机要能 ssh 到自己
   （`DIM_EVAL_HOST`）。不为本机执行开特例：手工复现的 run 与定时跑的是同一个 run。
4. **GitHub 部署密钥。** 生成一把专用 key，在仓库 Settings → Deploy keys 添加，勾选
   *Allow write access*（推 `eval-ledger` 用）。用一个专用的主机别名，不影响这台机器上别的
   GitHub 访问：

   ```
   # ~/.ssh/config
   Host github-dim-eval
     HostName github.com
     IdentityFile ~/.ssh/dim-eval-deploy
     IdentitiesOnly yes
   ```

   并在 `config.env` 里设 `DIM_EVAL_GIT_REMOTE=git@github-dim-eval:arcships/dim.git`。
   部署密钥对整个仓库有写权限，上线前用 ruleset 收窄它能做的事：`main` 要求 PR；`v*` tag
   （`release.yml` 由它触发）限制创建；`eval-ledger` 禁止 force push 与删除（ledger 只追加）。
5. **issue token。** fine-grained personal access token，只授权本仓库，权限只给
   *Issues: Read and write*。写入 `~/dim-eval/github-token`，`chmod 600`，只由 `issues.py`
   读取。token 有过期时间，过期后提 issue 失败，`nightly.sh` 以退出码 `10` 结束，日志里有警告。
6. **模型。** `~/.dim-eval-env` 放模型凭据，以及 `DIM_EVAL_MODEL_CONFIG`（指向评测用的 dimcode
   模型配置，格式同 `crates/dim-agent-aimux/examples/openai.json`）：

   ```sh
   export MODEL_API_KEY=...             # 模型配置里 apiKeyEnv 指向的变量
   export DEEPSEEK_API_KEY=...          # L1 judge
   export DIM_EVAL_MODEL_CONFIG=$HOME/dim-eval/model.json
   ```

   模型配置决定端点、模型和推理强度，它的文本原样写进 job 记录，所以凭据只能用 `apiKeyEnv`
   引用，不能写进 `headers`（`run_l2.py` 会拒绝）。换配置等于换实验条件，要重采 baseline。
7. **`~/dim-eval/config.env`**（不放凭据）：

   ```sh
   DIM_EVAL_HOST=user@eval-host          # ssh 回环的目标
   DIM_EVAL_SCRATCH=/home/user/dim-eval-scratch
   DIM_EVAL_REMOTE_PYTHON=/path/to/harbor-venv/bin/python
   DIM_EVAL_GIT_REMOTE=git@github-dim-eval:arcships/dim.git
   DIM_EVAL_REDACT="models.example.internal"   # issue 正文里额外抹掉的字符串，如模型端点
   # 可选：DIM_EVAL_TOOLCHAIN、DIM_EVAL_LEDGER_REMOTE、
   #       DIM_EVAL_LEDGER_BRANCH（默认 eval-ledger）、DIM_EVAL_GITHUB_REPO（默认 arcships/dim）、
   #       CARGO_TARGET_DIR（跨天复用依赖的编译产物）
   ```

8. **ledger 分支。** 建一个只装 ledger 的 orphan 分支 `eval-ledger`，根目录放
   `daily-state.json` 与 `officeqa-pro-state.json`，内容都是
   `{"last_l2_head": "<从哪个 commit 之后开始评测>"}`。窗口起点不允许猜，缺了它运行直接失败。
9. **cron。** 先 `git clone git@github-dim-eval:arcships/dim.git ~/dim-eval/repo`，再：

   ```
   0 18 * * * /home/user/dim-eval/repo/benchmarks/host/nightly.sh frontier daily
   0 21 * * * /home/user/dim-eval/repo/benchmarks/host/nightly.sh officeqa-pro daily
   ```

   两条共用一把锁，后一条等前一条结束。日志在 `~/dim-eval/logs/`，harbor 结果在
   `~/dim-eval/results/`，保留 30 天。

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
不是隔离。要收紧它，需要在容器里或以另一个用户身份构建；在那之前，上面的 ruleset 决定了
泄露的部署密钥最多能做什么。

## 被测产物

`build/build_image.py` 以 `cargo zigbuild` 编出 glibc 2.28 的 `dimcode`（与
`scripts/build-all.mjs` 相同），用发布脚本 `scripts/stage-plugins.mjs` 准备默认插件，按发布包的
目录结构打成只读挂载的镜像：

```
/opt/dim-agent/
  bin/dimcode
  default-plugins.json
  plugins/…
  ca-certificates.crt     评测机的根证书
  eval-build.json         被测 commit 与构建信息
```

`ca-certificates.crt` 不属于发布包。dimcode 为每个 HTTPS 客户端读取系统根证书，题目镜像里没有
证书时 App Server 无法启动；adapter 用 `SSL_CERT_FILE` 指向这份证书，不改题目文件系统。
