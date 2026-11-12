# 托管 Worktree

Scope: Process 作用域的 worktree 根、创建、列出与清理，以及创建时写入的 Session 使用关联。
Related issue: [#1451](https://github.com/arcships/dim/issues/1451)。
Rules: [设计规则](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)、
[Management methods](appserver/management-methods.md)。
外部 worktree 的识别与权限继承见 [Worktree access](worktree-access.md)。

## 1. 范围与非目标

Dim 自己创建的 worktree 只落在一个根下：

```text
$DIMCODE_HOME/worktrees/<task-id>/<repo-name>
```

创建、列出、删除和清理归 Process 作用域的 ManagedWorktrees 部件；Session 与目录的使用关联仍归
SessionStore。`$DIMCODE_HOME` 是 daemon 服务的产品 home，客户端不拼 `~/.dimcode`，也不按仓库位置猜路径。

非目标：识别或接管外部 `git worktree add` 创建的目录（见 worktree-access.md）；自动删除、后台清理或
保留周期；在新 worktree 里安装依赖；会话进行中切换工作区；跨机器共享 home。第一版一次创建只建一个
仓库（一个 Session 一个 worktree）；同一任务的其他仓库是新的 Session 与新的 `<task-id>`，布局已按仓库
分层，将来放开不需要迁移。

## 2. 命名与路径

| 项 | 规则 |
| --- | --- |
| `<task-id>` | ManagedWorktrees 创建时分配的 6 位小写 base32，在根下唯一。不是 Session id，不由标题或分支名派生：创建先于 Session，标题也尚不存在。 |
| `<repo-name>` | 源仓库工作树根目录的 basename，让 `git worktree list` 和目录浏览可读；同一 `<task-id>` 下只有一个仓库。 |
| 路径 | 规范绝对路径、UTF-8、无相对组件；管理根必须落在 `$DIMCODE_HOME/worktrees` 内，否则拒绝。 |
| base | 默认是源工作区的 `HEAD`；请求可指定其他 commit 或引用。 |
| branch | 默认 `dim/<task-id>`；`branch` 指定时按“新建”处理，已存在即报 `worktree_branch_exists`，不附着到既有分支。本部件不推送分支。 |

`list` 用关联到的 Session id 说明归属，不从目录名推断任务内容；路径本身不需要 slug。

## 3. 创建

入口是 `session.create`：`cwd` 仍是源工作区，新增可选
`worktree: { base?: string, branch?: string }`。回复给出 `taskId`、`root`（管理根）、`cwd`（Session
工作区）、`repository`、`base`、`branch`。成员由 `hello.features` 的 `session.create.worktree` 宣告，
客户端只在 daemon 宣告时发送；旧 daemon 不会收到该成员，因此不需要拒绝未知字段。`sessions.launch` 的
非交互创建不在第一版里：它的请求形状补上同一成员后复用这条路径。

1. 校验源工作区是 Git 工作树且不是 bare 仓库，解析仓库根、base 和源工作区相对仓库根的映射前缀；
   映射路径必须留在新根内。
2. ManagedWorktrees 分配 `<task-id>` 并执行 `git worktree add -b <branch> <root> <base>`。默认分支或
   目标路径已存在时换一个 `<task-id>` 重试（最多三次），显式 `branch` 冲突直接返回
   `worktree_branch_exists`，不重试。
3. 以根下的映射子树作为新 Session 的 `cwd`（源工作区就是仓库根时即根本身），使
   [worktree-access §2.2](worktree-access.md#22-仓库身份与目录范围) 的范围映射成为实际工作区。目录记录与
   使用关联由 SessionStore 在新 Session 的同一事务里写入，记录的 subtree 等于这个 cwd；这一步失败即视为
   Session 未创建。
4. 第 2 步之后的任何失败，ManagedWorktrees 用未取消的清理路径删除本次创建的目录与分支；清理也失败时
   保留它们，由 `list` 列为未登记，入口返回原错误。

创建成功后不写 Timeline 事件：worktree 是进程级事实，不是分支状态。已登记根立即进入本 Session 的
工作区范围，文件工具与显式 `exec` cwd 不再逐文件审批（[§5](worktree-access.md#5-文件工具与进程隔离)）。

## 4. 记录

- 持久事实只有现有的两类记录：`worktree_directories`（repository, root）与 `session_worktrees`
  （session_id, repository, root, subtree）。不新增表、标记文件或第二份索引；Git 是 worktree 实况，
  记录只是候选。
- 目录记录带最后使用时间，由关联的写入和解除写入（创建 Session 与删除 Session 都经过它）；没有值的旧行
  或外部记录视为未知，`--older-than` 不选未知项，`--keep N` 把未知项当作最旧。列变更遵守
  native-storage-format.md，由存储所有者决定。
- 通过识别写入、但根不在管理根下的目录记录不进入 `list`，也永不列为删除候选；管理根内的记录一律按托管候选处理。
- Session 删除解除它的使用关联，目录记录保留；目录记录只在 Git 删除成功、或根已被核验缺失的记录清理
  之后删除，失败先保留供重试。
- 一个 Session 只有一个托管 worktree；同一 Session 的第二个工作区属于 §1 排除的会话中切换。

## 5. 列出、删除与清理

三个操作都是 host 管理方法，注册在 `managementMethods` 的 `worktree` 命名空间：它们是用户命令，不是
agent 工具，不经过 Permission 批准，也从不服务给配对设备的连接（[ADR 002](../adr/002-plugin-scope.md)、
[management methods](appserver/management-methods.md)）。拒绝以类型化错误返回代号与阻塞方（Session id 或
路径），`list` 预先给出同一原因，客户端只展示，不自行判断。

- **list**：读 Git 的 `rev-parse` 与 `status --porcelain --ignored` 和 SessionStore 的目录记录、使用关联，join 后给出
  task-id、仓库、根、分支与 HEAD、所属 Session、最后使用时间、脏/未跟踪计数，以及目录缺失或
  Git 已不登记。根下既无记录也无 Git 登记的目录列为未登记；管理根外的目录记录不进入本列表，也永不列为删除候选。
- **remove**：删除单个已登记的托管 worktree。仍有关联（任何 Session）、任何 Session 的 cwd 在根下、有未提交
  或未跟踪内容时拒绝；要清理仍有关联的工作树，先删除对应 Session。`--force` 只覆盖内容检查。删除走
  `git worktree remove`，与记录删除在同一次存储操作里完成，`dim/<task-id>` 分支保留，已提交内容不随目录
  消失。根已核验缺失时只删记录并清理 Git 元数据。未登记目录按 task-id 显式删除，要求内容干净、没有
  Session 的 cwd 在根下且 Git 在该路径没有登记，分支保留并在结果里报告。
- **prune**：候选只包括根在 `$DIMCODE_HOME/worktrees` 下、没有剩余使用关联的目录记录，再按最后使用时间
  应用 `--older-than` 或 `--keep`；未知时间按 §4 处理，仍有关联、Session 的 cwd 在根下或未登记的目录一律
  不选。删除前逐项复核，先预览再按确认删除；保留项逐条给出原因，单个候选删除失败时保留其记录并继续其余
  候选。选择语义与交互沿用 [session cleanup](cli-commands.md#session-deletion-and-cleanup)，不新增第二套。
- **排斥**：remove 与 prune 在 SessionStore 的会话存储所有权锁内完成复核、Git 删除和记录删除；同一把锁
  覆盖 `associate_worktree`、Session 创建和删除，所以复核之后不会出现新的关联或新的 Session 行。复核时
  SessionStore 回答任何存储中的 Session 的 cwd 是否在根下；托管创建在写入 Session 行的同一事务里登记关联，
  所以有存储时装载中的 Session 一定被这条查询覆盖。可证明的使用只有记录与存储中的 cwd；其余使用不可证明，
  由预览、确认和 worktree-access §4 的删除授权承担。
- Session 删除和 `session cleanup` 不删除也不报告遗留的 worktree；`worktree list` 是查看它们的唯一入口。

## 6. 影响面

| Owner | 现有机制为何不够 | 需要的合同 | 顺序与失败 |
| --- | --- | --- | --- |
| SessionStore | `associate_worktree` 要求 Session 已存在，不能补写创建期记录；目录记录没有时间；删除目录记录会撞上未级联的关联外键 | 创建 Session 时同一事务写入目录记录、使用关联与最后使用时间；关联写入/解除更新时间；按根查询任何存储中 Session 的 cwd；Session 创建/删除与关联写入共用所有权锁；删除目录记录的类型化操作（含核验缺失的记录清理）。列变更遵守 native-storage-format.md | 创建失败不留记录；删除成功才删记录，失败保留供重试 |
| Agents | 现有 `cwd` 只能指向已存在目录，不能把创建目录和创建 Session 合成一次请求 | `session.create` 的可选 `worktree` 与回复字段；`sessions.launch` 的同一成员留给后续 | 入口只校验格式与权限；Git、失败清除由 ManagedWorktrees 执行，创建失败时入口调用它清除 |
| App Server 协议 | 客户端各自 `git worktree add` 会重新引入路径分歧；新成员会被旧 daemon 当作未知字段拒绝 | `hello.features` 新增 `session.create.worktree`（仅在产品 home 与原生存储都存在时宣告）、`worktree.list`、`worktree.remove`、`worktree.prune`；`cwd` 声明为源工作区；三个操作注册为 `managementMethods` 的 `worktree` 命名空间，不服务配对设备 | 客户端只在宣告存在时发送；失败不留半成品，重试创建新的 `<task-id>` |
| CLI 与 Desktop | 没有可调用的创建入口，只能让模型跑 Git | 各自的入口与展示（`--worktree`、`dimcode worktree ...`；TUI 的 `/worktree` 面板留待后续），只消费类型化结果与拒绝原因 | 由各自 owner 更新 cli-commands.md 与 clients.md；不拼路径、不读存储、不直接调用 Git |
| ManagedWorktrees（本部件） | 现有 `Worktrees` 只做 Session 内识别，没有 Process 级根与创建/清理 | Process 部件：根与命名、Git 调用、创建与失败清除、list/remove/prune 的选择、复核与锁的编排 | 不持有 Session 或分支状态；只在用户命令或 Session 创建时动作 |

这些需求都不改变 Timeline、Dispatcher、装配与门面，因此不向基础设施提通用能力。

## 7. 规则检查

| 规则 | 回答 |
| --- | --- |
| §1 部件与作用域 | 一个 Process 部件；目录和记录跨 Session，Session 关联写在 Session 的存储所有者里。App Server 只装配和暴露入口。 |
| §2 作用域关系 | 创建指令来自 Session 入口，执行在 Process 部件；部件不读 Session 内部状态，关联写入与工作区查询经所有者类型化操作。 |
| §3 正交 | 外部 worktree 的识别路径不变；清理只消费现有记录与 Git 事实，不改 Permission、HostPlatform 或 Timeline。 |
| §4.1 指令事件 | 不适用：worktree 生命周期不在 Timeline 上，没有可写的分支状态；创建与删除是进程级类型化操作。 |
| §4.2 交接 | 不适用：没有对象在模块之间交接；目录在创建成功后只由管理方法处置。 |
| §4.3 before 回调 | 不适用：不涉及写入前的草稿变换。 |
| §4.4 核心机制 | 不适用：Timeline、Dispatcher 与门面不带 worktree 业务含义，也不为它加特例。 |
| §4.5 批次 | 不适用：不新增 Timeline 批次；创建事务与记录删除由 SessionStore 自己提交。 |
| §4.6 模型上下文 | 不适用：不向模型上下文加入新事实。 |
| §4.7 公开事件与服务 | 新增的公开内容只有 `managementMethods` 的 `worktree` 命名空间与 `hello.features` 条目；两者都在 §6 说明消费者。 |
| §4.8 基础设施 | 不修改 Timeline、Dispatcher、装配或门面；缺少的创建期写入与所有权锁复用属于 SessionStore 自己的合同。 |
| §4.9 事实取舍 | 不新增 Timeline 事件；列表、计数、时间与路径是命令结果，不进 Timeline。 |
| ADR 002 | 无接线模块、无第二套容器；管理方法归 Process owner，配对设备不服务；配置不参与装载行。 |
