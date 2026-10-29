# OfficeQA Pro

L2 的第二个语料：Databricks OfficeQA Pro 的 133 道 hard 题。设计与取舍见
[trigger-policy § L2 — OfficeQA Pro](../../docs/eval/trigger-policy.md#l2--officeqa-pro)，
这里只写怎么做。

| 路径 | 内容 |
|---|---|
| `assemble.py` | 从 `officeqa_pro.csv` 生成任务树与 `MANIFEST.json` |
| `template/` | 每个任务的模板；`tests/evaluate.py` 调上游 `score_answer` |
| `corpus-image/` | 自建语料镜像的 Dockerfile 与 `build.sh`（备用，见下文） |
| `tasks/` | 生成的 133 个任务，**提交进仓库**，语料 pin 即它的 `tree_digest` |
| `flow_test.py` | 生成 → pin → verifier 的流程测试（数据集为合成） |

## 当前 pin

| 项 | 取值 |
|---|---|
| 数据集 | `databricks/officeqa` revision `763a8366abf2a3605c381d53586d844dc60fa756`，`officeqa_pro.csv` |
| `reward.py` | `databricks/officeqa` commit `7b9a3c154ef9fb40215bb67934afc43e6799de16` |
| 语料镜像 | `docker.io/benediktstroebl/officeqa-corpus@sha256:731237e3b35277ca3c08b1b5e6063c4c6ae0c88a4eb4d7cef201c4f14a3e333d` |
| tolerance | 0 |

语料镜像是 harbor officeqa adapter 公开在 Docker Hub 上的那个，没有自建。采用前把镜像内
697 个文件的 sha256 与上表 revision 的 `treasury_bulletins_parsed/transformed/` 逐个比对过，
全部一致。生成后在该镜像里对 133 题逐题跑 `solution/solve.sh` + `tests/test.sh`（`--network none`），
全部得 1；写一个错误答案得 0。

## 升级 pin

需要一个已在 HF 上接受 [databricks/officeqa](https://huggingface.co/datasets/databricks/officeqa)
条款的账号。HF token 只在这一步用，不进 CI、不进评测机、不进任务文件。

```bash
# 1. 数据集快照（gated）。记下 revision。
export HF_TOKEN=...            # 只在本 shell
python3 - <<'PY'
from huggingface_hub import HfApi, snapshot_download
rev = HfApi().dataset_info("databricks/officeqa").sha
snapshot_download("databricks/officeqa", repo_type="dataset", revision=rev,
                  local_dir="officeqa-data",
                  allow_patterns=["officeqa_pro.csv",
                                  "treasury_bulletins_parsed/transformed/*.txt"])
print(rev)
PY

# 2. 语料镜像。先比对现有镜像；语料没变就沿用它的 digest。
docker run --rm <当前镜像> bash -c 'cd /app/corpus && sha256sum treasury_bulletin_*.txt' | sort -k2 > img.sha
(cd officeqa-data/treasury_bulletins_parsed/transformed && sha256sum *.txt) | sort -k2 | diff - img.sha
#    有差异时自建（Apple Silicon 上要加 DOCKER_DEFAULT_PLATFORM=linux/amd64），打印 <name>@sha256:<digest>：
OFFICEQA_ROOT=$PWD/officeqa-data \
  benchmarks/officeqa/corpus-image/build.sh <registry>/officeqa-corpus:<date>

# 3. reward.py 与任务树（Python >= 3.11）
git clone https://github.com/databricks/officeqa officeqa-src
python3 benchmarks/officeqa/assemble.py \
  --dataset officeqa-data --dataset-revision <第 1 步的 rev> \
  --officeqa-repo officeqa-src --reward-commit "$(git -C officeqa-src rev-parse HEAD)" \
  --corpus-image <第 2 步的 digest 引用> \
  --out benchmarks/officeqa/tasks
```

`assemble.py` 会拒绝：行数不是 133、出现非 hard 题（即拿错成 `officeqa_full.csv`）、
语料镜像不是 digest 引用、`--out` 是一个不由它生成的目录。

然后在评测机上：

```bash
docker pull <MANIFEST.json 里 source.corpus_image 的值>
# oracle 自检：133 题的标准答案应当全部得 1；不是就说明 verifier 或镜像有问题，不要冻 baseline
harbor run -p benchmarks/officeqa/tasks -a oracle
```

最后冻 baseline（k=3，手工，不走 CI）并提交：

```bash
python3 benchmarks/daily_run.py --run-type baseline --head <commit> \
  --tasks-dir benchmarks/officeqa/tasks --concurrency 10
```

任务树、`MANIFEST.json` 与新 baseline 放在同一个 MR 里。换语料镜像、换 tolerance、
升级 `reward.py` 都会移动 pin，都要走这一整套。

## 定时运行

评测机 cron 加一条 `benchmarks/host/nightly.sh officeqa-pro daily`。前提是 `eval-ledger` 分支
根目录已有 `officeqa-pro-state.json`（`{"last_l2_head": "<commit>"}`），其余见
[host/README.md](../host/README.md)。
