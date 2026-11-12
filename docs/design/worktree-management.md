# 托管 Worktree

Scope: Process 作用域的 worktree 根、创建、列出与清理，以及创建时写入的 Session 使用关联。
Related issue: [#1451](https://github.com/arcships/dim/issues/1451)。
Rules: [设计规则](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)。
外部 worktree 的识别与权限继承见 [Worktree access](worktree-access.md)。

## 1. 范围与非目标

Dim 自己创建的 worktree 只落在一个根下：

```text
$DIMCODE_HOME/worktrees/<task-id>/<repo-name>
```

创建、列出、删除和清理归 Process 作用域的 ManagedWorktrees 部件；Session 与目录的使用关联仍归
SessionStore。`$DIMCODE_HOME` 是 daemon 服务的产品 home，客户端不拼 `~/.dimcode`，也不按仓库位置猜路径。

非目标：识别或接管外部 `git worktree add` 创建的目录（见 worktree-access.md）；自动删除、后台清理或
保留周期；在新 worktree 里安装依赖；会话进行中切换工作区；跨机器共享 home。

## 2. 命名与路径

| 项 | 规则 |
| --- | --- |
| `<task-id>` | ManagedWorktrees 创建时分配的 6 位小写 base32，在根下唯一。不是 Session id，不由标题或分支名派生：创建先于 Session，标题也尚不存在。 |
| `<repo-name>` | 源仓库工作树根目录的 basename；一个任务涉及多个仓库时，每个仓库一个子目录。 |
| 路径 | 规范绝对路径、UTF-8、无相对组件；根必须落在 `$DIMCODE_HOME/worktrees` 内，否则拒绝。 |
| base | 默认是源工作区的 `HEAD`；请求可指定其他 commit 或引用。 |
| 工作树 | `git worktree add -b <branch> <root> <base>`；默认分支 `dim/<task-id>`，`--branch NAME` 可覆盖。本部件不推送分支。 |

`list` 用关联到的 Session 标题补足可读性，不从目录名推断任务内容；路径本身不需要 slug。

## 3. 创建

一次创建同时决定目录和 Session：`session.create` / `sessions.launch` 接受可选
`worktree: { base?: string, branch?: string }`，回复给出 `taskId`、`root`、`repository`、`base`、
`branch`。客户端不先建目录再传 cwd，也不自己分配路径。

1. 入口校验源工作区是 Git 工作树且不是 bare 仓库，解析仓库根与 base；不通过时返回具体错误，不创建目录。
2. ManagedWorktrees 分配 `<task-id>` 并执行 `git worktree add -b`；分支已存在时换一个 `<task-id>` 重试，
   仍失败则不写任何记录。
3. 以该根为 cwd 创建 Session。目录记录与使用关联由 SessionStore 在新 Session 的同一事务里写入，继承
   子树按 [worktree-access.md §2.2](worktree-access.md) 的映射确定；这一步失败即视为 Session 未创建。
4. 第 2 步之后任何失败，ManagedWorktrees 删除刚建的目录并报告原因；删除也失败时保留目录并在错误里
   给出路径，`list` 将其列为未登记。

创建成功后不写 Timeline 事件：worktree 是进程级事实，不是分支状态。已登记根立即进入本 Session 的
工作区范围，文件工具与显式 `exec` cwd 不再逐文件审批（[§5](worktree-access.md#5-文件工具与进程隔离)）。

## 4. 记录

- 持久事实只有现有的两类记录：`worktree_directories`（repository, root）与 `session_worktrees`
  （session_id, root, subtree）。不新增表、标记文件或第二份索引；Git 是 worktree 实况，记录只是候选。
  目录记录另外保存最后活动时间供 `prune` 选择，它不是第三类记录。
- Session 删除解除它的使用关联，目录记录保留；目录记录只在 `git worktree remove` 成功后删除，失败先
  保留供重试。
- 同一 Session 再次创建 worktree 是另一个任务，得到新的 `<task-id>`；记录按（Session, 目录）去重，
  不覆盖既有根。

## 5. 列出、删除与清理

三者都是用户命令，不是 agent 工具，不经过 Permission 批准；入口由 CLI 与 App Server 各自提供，本部件
给出事实与操作。

- **list**：读 `git worktree list --porcelain` 与 SessionStore 的目录记录、使用关联，join 后给出
  task-id、仓库、根、分支与 HEAD、所属 Session 与标题、最近活动、脏/未跟踪计数，以及目录缺失或
  Git 已不登记。根下既无记录也无 Git 登记的目录列为未登记，永不删除。
- **remove**：有未提交或未跟踪内容、Session 运行中或已附加、仍有其他 Session 关联时拒绝并说明原因；
  `--force` 只覆盖内容检查。删除走 `git worktree remove`，成功后删除目录记录；`dim/<task-id>` 分支保留，
  已提交内容不随目录消失。
- **prune**：只选择没有剩余使用关联的目录记录（Session 删除即解除关联），再按目录记录的最后活动时间
  应用 `--older-than` 或 `--keep`；仍有关联的目录一律保留。删除前逐项复核 Git 实况、根未变、内容干净、
  无活动 Session，先预览再按确认删除；保留项逐条给出原因。选择语义与交互沿用
  [session cleanup](cli-commands.md#session-deletion-and-cleanup)，不新增第二套。
- Session 删除和 `session cleanup` 不连带删除 worktree，只报告留下了哪些目录；删除用户代码需要独立的
  授权命令。

## 6. 影响面

| Owner | 现有机制为何不够 | 需要的合同 | 顺序与失败 |
| --- | --- | --- | --- |
| SessionStore | `associate_worktree` 已是原子写，但要求 Session 已存在，不能用它补写创建期记录；目录记录也没有供清理选择的时间 | 创建 Session 时在同一事务写入目录记录与使用关联；目录记录保存最后活动时间；另提供删除目录记录的类型化操作。列变更遵守 native-storage-format.md，由存储所有者决定 | 创建失败不留记录；目录删除成功才删记录，失败保留供重试 |
| Agents / `session.create` | 现有 `cwd` 只能指向已存在目录，无法把创建目录和创建 Session 合成一次请求 | 可选 `worktree` 参数与回复字段（task-id、root、repository、base、branch） | 入口只校验格式与权限；Git 与失败清除由 ManagedWorktrees 执行 |
| App Server 协议 | 客户端各自 `git worktree add` 会重新引入路径分歧，且拿不到规范化结果 | `session.create`、`sessions.launch` 的可选字段，以及 `worktree.list`、`worktree.remove`、`worktree.prune` | 失败不留半成品；客户端重试创建新的 `<task-id>`，遗留目录由 `list` 核对 |
| CLI 与 Desktop | 没有可调用的创建入口，只能让模型跑 Git | 各自的入口与展示（`--worktree`、`/worktree`、`dimcode worktree ...`） | 由各自 owner 更新 cli-commands.md 与 clients.md；不拼路径、不读存储、不直接调用 Git |
| ManagedWorktrees（本部件） | 现有 `Worktrees` 只做 Session 内识别，没有 Process 级根与创建/清理 | Process 部件：根与命名、Git 调用、创建与失败清除、列出/删除/清理的选择与复核 | 不持有 Session 或分支状态；只在用户命令或 Session 创建时动作 |

这些需求都不改变 Timeline、Dispatcher、装配与门面，因此不向基础设施提通用能力。

## 7. 规则检查

| 规则 | 回答 |
| --- | --- |
| §1 部件与作用域 | 一个 Process 部件；目录和记录跨 Session，Session 关联写在 Session 的存储所有者里。App Server 只装配和暴露入口。 |
| §2 作用域关系 | 创建指令来自 Session 入口，执行在 Process 部件；部件不读 Session 内部状态，关联写入经 SessionStore 的类型化操作。 |
| §3 正交 | 外部 worktree 的识别路径不变；清理只消费现有记录与 Git 事实，不改 Permission、HostPlatform 或 Timeline。 |
| §4.1 指令事件 | 不适用：worktree 生命周期不在 Timeline 上，没有可写的分支状态；创建与删除是进程级类型化操作。 |
| §4.3 before 回调 | 不适用：不涉及写入前的草稿变换。 |
| §4.6 模型上下文 | 不适用：不向模型上下文加入新事实。 |
| §4.8 基础设施 | 不修改 Timeline、Dispatcher、装配或门面；缺少的创建期写入属于 SessionStore 自己的合同。 |
| §4.9 事实取舍 | 不新增 Timeline 事件；列表、计数与路径是命令结果，不进 Timeline。 |
| ADR 002 | 无接线模块、无第二套容器；配置不参与装载行；合同语言中立。 |
