# Worktree 按需识别与权限继承

Scope: Session 的工作区范围识别，以及跨 Session 保留的 worktree 使用关联。
Related issue: [#911](https://github.com/arcships/dim/issues/911)。
Rules: [设计规则](../architecture/rust-agent/principles.md)、[ADR 002](../adr/002-plugin-scope.md)、
[插件契约](plugin-contract.md)、[Dim 如何使用 rutis](rutis-usage.md)。

## 1. 目标与边界

Session 访问原工作区外的目录时，按需判断它是否属于同一 Git 仓库的 worktree。确认后的
目录纳入本 Session 的工作区范围，后续文件访问直接命中内存中的目录范围，不逐文件查询
Git，也不在启动时获取整个仓库的 worktree 清单。

“继承权限”指对这些目录应用**当前调用分支的权限策略**。它不复制创建时的策略，不改变
Session cwd，不切换配置来源，也不把同一仓库的所有目录预先加入工作区。文件权限与命令
审批的关系仍由 [Permission](../architecture/rust-agent/plugins/Permission.md#policy-contract)
决定。显式文件授权仍按原路径生效，不被复制到另一个 worktree。

Git 提供仓库关系，Dim 记录 Session 使用关系。普通 `exec` 创建的 worktree 可以被识别；
识别不依赖专用创建工具、shell 命令解析、前后清单比较或 agent 自报路径。Git 无法证明
创建者，首次发现它的 Session 不被记录为创建者。

本文不定义自动删除、目录授权按钮、全局 worktree 浏览器或新的权限预设。识别 worktree
也不启用其中的 MCP、Hooks 或其他项目配置；其信任边界见 [ADR 003](../adr/003-approval-scope.md)。

## 2. 按需识别

### 2.1 输入与结果

输入是 Session 身份、原工作区和已经过现有文件边界解析的实际目标路径。新文件从其最近
存在的父目录识别；识别过程不创建文件或目录。`read`、`write`、`edit`、`grep`、`glob`
使用同一规则，显式指定的进程 cwd 也使用同一目录识别入口。

一次识别产生下列结果之一：

| 结果 | 含义与后续行为 |
| --- | --- |
| 已知范围 | 原工作区或本 Session 已确认的目录覆盖该目标；不启动 Git |
| 同仓库 worktree | 确认仓库身份、worktree 根目录及本 Session 可访问的子树，登记关联后发布到内存 |
| 无关联 | 非 Git 目录、其他仓库或目标在可继承子树外；继续原有权限判断和审批 |
| 识别不可用 | Git 不可用、超时或元数据无法验证；说明原因，不自动扩展范围，保留原有审批路径 |
| 取消或关闭 | 取消识别，不发布目录范围，不把取消解释成允许或一次用户拒绝 |

先处理无需扩展范围即可决定的调用：已有明确拒绝不触发发现，已有工作区或显式授权
可以决定的调用不为发现 worktree 额外等待 Git。识别失败不能覆盖已有拒绝或让一个原本
不允许的调用执行。

```text
Resolved target
  |
  +-- Original workspace / confirmed root --> Current branch policy
  |
  +-- Unknown directory
        |
        +-- Probe this directory with Git
              |
              +-- Same repository and eligible subtree
              |     --> Persist Session association
              |     --> Remember confirmed root
              |     --> Current branch policy
              |
              +-- Unrelated / unavailable --> Existing approval path
```

### 2.2 仓库身份与目录范围

第一次需要发现时，才读取原工作区的 Git 身份，并在本次 Session 装载期间保留。原工作区
不是 Git 工作树时，不根据遇到的第一个外部仓库反向建立授权来源。

仓库身份采用规范化后的公共 Git 目录，不采用 remote URL、仓库名或分支名。候选目录须位于
该仓库的有效工作树内，由 Git 解析其根目录，并核对根目录与 Git 元数据的实际关联。
主工作树与 linked worktree 均可参与，bare 仓库没有可继承的工作目录；独立 clone 和其他仓库的 submodule
不因 URL、对象或历史相同而获得权限。一次发现只读取来源和候选目录自身需要的元数据。

原工作区覆盖整个来源工作树时，目标 worktree 的根目录获得同等工作区范围。如果原工作区
只是来源工作树的子目录，只映射同一相对路径，不扩大到目标 worktree 的其他目录：

| 原工作区 | 遇到的目标 | 可继承范围 |
| --- | --- | --- |
| `/repo` | `/repo-wt/src/a.rs` | `/repo-wt` |
| `/repo/packages/a` | `/repo-wt/packages/a/src/x.rs` | `/repo-wt/packages/a` |
| `/repo/packages/a` | `/repo-wt/packages/b/x.rs` | 无；保留原有审批 |

来源工作区、两个 worktree 根目录及实际目标遵循
[文件路径解析契约](../architecture/rust-agent/plugins/Permission.md#write-targets-without-the-facts)。
以规范化后的来源工作区相对于来源根目录的路径组件确定映射前缀，再将该前缀接到规范化后
的目标根目录；不通过解析目标侧的符号链接改变这个前缀。实际目标相对于目标根目录的路径
组件必须以该前缀开头。从目标根目录到映射子树的任一组件经符号链接重定向时，不发布该
映射范围，即使重定向仍在同一 worktree 内。映射范围内的链接也不能把实际目标带到固定
前缀外。来源、候选或映射关系无法确认时不猜测范围。

## 3. Session 内存与生命周期

目录识别及其缓存属于 Session，所有分支共用一份；Permission 仍由每个调用分支按其已派发
的当前策略决定。缓存保存仓库身份、worktree 根目录及其宿主目录身份、可继承的固定路径
前缀；前缀可以尚不存在，目录身份检查针对已确认的根。不保存 `Allow`、权限预设或已经
批准的工具调用。

- 成功后按根目录覆盖子树。识别 `/repo-wt/src/a.rs` 后，同一可继承根下的 `tests/b.rs`
  不再查询 Git，也不再次写入同一使用关联。
- 同一未知目录的并发识别合并为一项 I/O 工作；单个调用取消只结束自己的等待，不取消其他
  调用仍需的识别。Session 关闭时取消并结算所有识别工作，不留下半发布的范围。不同目录
  的首次请求可能各自查询，确认共同根后都复用它。
- 不永久缓存“不是 worktree”。一个目录稍后可能被 `git worktree add` 使用；失败结果不
  成为持久拒绝，也不把其整个父目录标成无关联。
- 命中缓存仍解析实际目标，并以宿主目录身份检查确认的根未被删除或替换。这不启动 Git；
  根失效时丢弃该条，下一次访问重新按需识别。普通分支切换和文件内容变化不使根失效。
- 确认范围是本次装载对该目录的工作区扩展，不持续轮询 Git 元数据，不启动文件系统监听
  或定时扫描。它沿用现有文件工具的并发文件系统修改边界，不声称是 OS 沙箱。
- Session 关闭或部件卸载后释放内存视图。恢复、重新装载或新 Session 从空视图开始；
  持久记录不能直接填成已授权范围，后续首次使用重新核验。fork 不复制内存确认或使用关联。

File access 从工作区读写改成只读后，已确认 worktree 的写入同样被拒绝；显式 Tool approval
Ask 仍可要求每次工具批准。工作区扩展不改变系统路径、Session-owned 文件或显式拒绝的
优先级，完整优先级由 [Permission 的文件事实契约](../architecture/rust-agent/plugins/Permission.md#file-access-facts)
拥有。worktree 不是 Session-owned 输出文件。

## 4. 持久使用关联与清理边界

持久记录的业务信息只有两类：

| 记录 | 信息与用途 |
| --- | --- |
| Worktree 目录记录 | 所属仓库、worktree 规范根目录；目录没有 Session 关联后仍可作为清理候选被找到 |
| Session 使用关联 | Session、worktree 目录记录、该 Session 的继承子树；表示这个 Session 已识别并关联过该范围 |

记录属于同一存储所有者，目录记录按仓库与根目录去重，使用关联按 Session 与目录记录去重。
跨 Session 不共享权限缓存；每个 Session 独立核验并按自己的工作区边界取得范围。
记录不含创建者推断、复制的权限或自动删除许可。

首次成功识别须先持久保存目录记录和使用关联，再发布内存范围；两类记录由一个存储操作
原子完成。保存失败明确报错，本次不发布自动扩展；用户仍可走原有单次审批路径。识别
完成但调用随后取消时，已保存的关联可以保留，表示确认过的关系，不宣称工具已执行。
已有范围的后续调用不重复读取整个关联表或更新访问时间。

记录只覆盖触发成功识别的目录，不声称穷举所有实际使用：已经由显式授权或 Unrestricted
决定的文件访问、命令内的绝对路径和脚本内部 `cd`，都可能不触发识别。这也是使用关联
不能单独证明一个 worktree 可安全清理的原因。

Session 关闭保留关联。Session 删除解除它的使用关联，目录记录保留为可核验的候选；
仅凭这些记录不得递归删除 worktree。清理前须重新核对 Git 实况、其他 Session 的关联及
活动使用、未提交和未跟踪内容，并取得针对删除的授权。没有关联不能证明没有外部程序
或其他数据库在使用。未知创建者的 worktree 也可能是用户自行创建的。

本文只提供未来清理所需的关联信息，不定义清理命令、保留周期、后台清理或强制删除。
未来删除失败须保留目录记录供重试，不先删记录再尝试删目录。

## 5. 文件工具与进程隔离

文件工具在首次访问时按需确认目标目录。新启动的进程以其显式 cwd 触发同一识别；
已经确认的 Session 目录范围提供给隔离边界的所有者，而不是从进程命令字符串推导权限。
命令审批、网络开关与 [HostPlatform 隔离契约](host-platform-isolation.md) 继续独立生效。

```text
File access: Workspace read/write
Tool approval: Legacy

write /repo-wt/src/a.rs   -> Recognize root -> No outside-workspace prompt
write /repo-wt/tests/b.rs -> Known root     -> No Git probe
exec cwd=/repo-wt        -> Known root     -> Existing command approval
```

新进程只能使用在准备隔离边界前已经确认的范围。一个进程后来在脚本内部 `cd` 到未知
目录，不触发宿主目录识别，也不扩大已运行进程或 persistent shell 的边界。未知 worktree
尚未创建时无法通过识别取得权限；在沙箱外创建它仍受原有创建与隔离限制。

本设计不更改 HostPlatform 的开关、网络语义或现有进程。让新进程访问已确认目录的需求
在下节交给隔离边界所有者处理；文件工具的成功识别本身不代表 OS 沙箱已经扩展。

## 6. 影响面与 Requirements raised to mechanism owners

本文定义 worktree 识别与关联语义。以下接口与存储变化由各自所有者在实现前更新自己的
契约；本文不把尚未提供的接口当成可用依赖，不修改其他模块的设计或 ADR。

### R1. 文件访问范围

- **Blocked:** [ADR 014 D1](../adr/014-file-access-facts.md#d1-fixed-facts) 的工作区事实只来自
  SessionConfig，不能表达本 Session 按需确认的其他根。
- **Existing mechanisms:** 显式文件或目录 grant 表达用户授权，不能冒充 Git 派生关系；
  修改 cwd 会同时改变相对路径与配置来源，超出本需求。
- **Needed:** 文件事实所有者提供 Session 内的目录识别与已确认范围查询，接受规范目录，
  返回确认的范围或无关联、不可用、取消；由文件事实消费，不新增 Permission 决策层。
- **Ordering impact:** 异步探测在现有调用任务中进行；持久关联成功后发布范围，再读取当前
  分支策略。取消、关闭和重载不发布过期结果；Timeline、Dispatcher 和事件格式不变。
- **Owner / issue:** FileAccessFacts 与 Permission；[#911](https://github.com/arcships/dim/issues/911)。

### R2. 关联存储与 Session 删除

- **Blocked:** [Session 删除与 fork 契约](appserver/session-deletion-and-fork.md) 没有 worktree
  使用关联；仅持有内存无法在重启后回答哪些 Session 关联过某个目录。
- **Existing mechanisms:** Session grants 没有仓库身份和全局候选记录；Session-owned 输出的
  删除语义不适用于用户代码。两者均不能直接作为 worktree 记录使用。
- **Needed:** SessionStore 拥有目录记录、原子去重关联写入和 Session 删除时的解除关联；
  目录候选不因最后一条使用关联被删而消失。通过所有者的类型化操作使用，不直接读写表。
- **Ordering impact:** 关联写入不产生 Timeline 事实；与删除串行协调，已删除或正在删除的
  Session 不能被迟到写入重新关联。native 格式改变须由存储所有者作独立格式决定，遵守
  [存储兼容性契约](appserver/native-storage-format.md)，不在本文预占版本号。
- **Owner / issue:** SessionStore、Agents；[#911](https://github.com/arcships/dim/issues/911)。

### R3. 新进程的工作目录与隔离范围

- **Blocked:** [隔离契约的调用流程](host-platform-isolation.md#call-flow) 将 Session 工作区
  与模型指定的 cwd 分开，尚不能消费其他已确认范围。
- **Existing mechanisms:** 只改变 cwd 不改变沙箱；关掉隔离或选择 Full access 会扩大其他
  权限，不能代表继承工作区权限。
- **Needed:** 进程调用所有者在准备前按需确认显式 cwd；HostPlatform 消费宿主已确认的目录
  范围，为新进程按当前隔离边界规则构造相应边界，不接受模型直接指定额外授权根。
- **Ordering impact:** 识别和审批在进程启动前完成；准备失败不启动进程，不重放启动副作用，
  已运行进程保持原边界。没有确认的目录不因命令文本、网络开关或普通 exec 批准而被挂载。
- **Owner / issue:** WorkspaceTools、ShellSession、HostPlatform；[#911](https://github.com/arcships/dim/issues/911)。

这些需求改变文件事实、存储和进程准备的业务契约，不要求修改 Timeline、Dispatcher、
Session/branch 门面、rutis 或 loader。CLI 与 Web 共用宿主判定，不增加各自的 Git 检测。

## 7. Rule check 与消融检查

| 规则 | 本设计的适用方式 |
| --- | --- |
| §1 Capability | 不新增 Capability。目录缓存是 Session 对外部 Git/文件系统状态的运行视图，持久关联归存储所有者；不构造分支状态机 |
| §4 rule 1 | 不新增跨分支指令。识别所有者维护自己的视图，进程级关联通过存储所有者的类型化写操作修改 |
| §4 rule 2 | 不撤回已交接的工具工作；识别只提供执行前的目录事实 |
| §4 rule 3 | 不使用 `before`；Git 与存储等待不进入 Timeline 提交锁 |
| §4 rule 4 | 运行时核心没有 worktree 特例 |
| §4 rule 5 | 无新增事件批次；目录记录和关联的原子性属于存储操作 |
| §4 rule 6 | 无上下文注入；识别结果不成为额外模型输入 |
| §4 rule 7 | 消费者通过公开文件事实、目录范围和存储操作协作，不读他方缓存或数据库内部 |
| §4 rule 8 | R1–R3 分别交给业务机制所有者，不扩展运行时基础设施 |
| §4 rule 9 | 无新增 Timeline 事件；权限仍由已有策略事实决定，外部目录关系重新核验 |
| ADR 002 D1 | Session 拥有识别任务和缓存，SessionStore 拥有跨 Session 的持久记录 |
| ADR 002 D2 | 扩展已有所有者，不新增接线插件、全局发现守护任务或第二套管理器 |
| ADR 002 D3 | 各消费者声明自己的文件事实、存储或进程准备依赖；缺少识别能力时保留原有工作区规则 |
| ADR 002 D4 | 关联和缓存不进 loader 配置，改变权限不触发插件重载 |
| ADR 002 D5 | 本文用目录、仓库、Session 和结果描述语义；语言及 Git 调用写法在附录 |

保留的状态只有当前 Session 的已确认根、进行中的识别工作，以及供恢复核验和未来清理
查询的持久目录与使用关联。去掉根缓存会重复探测；去掉使用关联会丢失 Session 对应关系；
去掉独立目录记录会在最后关联解除后失去清理候选。无需工作树全量清单、负结果永久缓存、
创建者推断、复制的权限、文件系统监听、定时扫描或专用 worktree 创建工具。

## 8. 验收场景

验收复用文件权限、真实工具调用、存储与隔离的公开流程；Git 调用次数通过宿主探测边界
观测，不以私有 helper 的调用顺序代替用户行为。

1. 用普通 `exec` 在原工作区外创建同仓库 worktree，首次文件访问只探测该目录；另一个子目录
   的新文件命中根缓存。启动和首次访问均不运行全量 worktree list。
2. 多分支访问共用已确认根；同一未知目录并发首次识别只进行一项探测。分支各自的当前策略
   仍可给出不同决定；收紧为只读后不能继续写。
3. 非 Git 目录、独立 clone、其他仓库 submodule、原工作区之外的映射子树、符号链接逃逸、
   不可验证的元数据均不扩大范围；新建文件和不存在的中间目录使用最近存在的父目录识别。
   来源工作区为 `/repo/packages/a` 时，目标的 `packages/a` 或其祖先链接到同一 worktree
   内的其他子树或根目录，也不发布映射范围；普通的 `/repo-wt/packages/a/new/x.rs` 仍可识别。
4. Git 不可用、探测超时、取消、Session 关闭、持久保存失败均不发布半完成范围；失败后保留
   原有审批能力。根删除或替换后失效；同位置新建合法 worktree 可再次按需确认。
5. native store 重启保留使用关联，但恢复后的首次访问重新核验；fork 没有继承的确认或关联。
   Session 删除与迟到关联写入竞争不产生悬空 Session 关联，最后关联解除仍留下目录候选。
6. 新进程显式 cwd 首次识别复用相同根；命令审批与网络选择不变。隔离开启时验证新进程的
   实际可访问范围；已有 persistent shell 不因新识别或策略变化自动扩大边界。
7. Session 关闭、删除以及候选查询均不自动删除任何 worktree；多 Session 关联不会被误报为
   单个 Session 独占，也不把首次发现误报为创建。

## Appendix A. Git binding

目录探测使用 Git 的只读元数据接口：[git-rev-parse](https://git-scm.com/docs/git-rev-parse)
的 `--show-toplevel`、`--git-common-dir` 与 `--git-dir`，工作树关联格式见
[git-worktree details](https://git-scm.com/docs/git-worktree#_details)。例如对来源或候选的已有目录：

```sh
git -C "$directory" rev-parse --path-format=absolute --show-toplevel
git -C "$directory" rev-parse --path-format=absolute --git-common-dir
git -C "$directory" rev-parse --path-format=absolute --git-dir
```

每个结果按其单个路径边界读取，不把多个路径拼成按空白分割的文本协议；不能无损表达的
路径明确报告不可用。验证 linked worktree 时核对它与仓库管理目录的双向关联，不仅判断
候选目录是否存在 `.git`。不调用 `git worktree list`、`prune`、`repair`，不改变仓库配置。

宿主直接传递 argv，清除影响仓库定位的继承环境，固定查询目录；不通过 shell 执行模型
拼接的命令，不运行 Hook、不访问网络、不设置 `safe.directory=*`。探测使用有取消和有限
超时的宿主任务，外部输出在此边界解析一次，随后以统一的目录识别类型传给消费者。
