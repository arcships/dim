# 业务覆盖补充实验

本轮根据 [覆盖复查](coverage-audit.md) 中的具体缺口补模型。承诺来自该表链接的现行合同和 `8a5fe0980` 的实现入口，
不是根据哪个性质容易检查来新增产品要求。下面逐项说明补了什么、依赖什么以及还不能下什么结论。
[运行结果](coverage-fill-results.md)记录实际完成的检查，[JSON](coverage-fill-results.json)保存精确文件摘要。原 [results.json](results.json) 保留为旧模型实验记录。

## 已补的可观察场景

| 场景 / 来源入口 | 模型与检查 | 取值及明确边界 |
| --- | --- | --- |
| Wait：`wait.rs::consume/take/arrival` | [WaitRuntime](WaitRuntime.tla) 保留到达队列；任务匹配、Steer 优先、不匹配通知保留；第二调用拒绝不动原调用；timeout、interrupted、reload、drop 分开；注册通知后复查队列，睡眠时不能漏掉已有匹配通知。原 [观察探针](WaitSelectionCoverage.tla) 的公式改为比较待取队列，重新检查 FIFO | 两个不同 task、一个 Steer、两次接纳、两个 turn 边界、一次换代；不证明任意长队列。关闭后的旧代不再接纳/取通知，换代清空队列，因此关闭后的新输入在本模型中抽象为无效果。ordinary input 不改变通知状态；通知只在 open turn 入队。drop 表示无返回的调用清理，不是新增 API 结果 |
| Wait → ToolExecutor → Loop：`WaitTool::run`、`tool_executor::settled`、`agent_loop::consume` | [WaitHandoff](WaitHandoff.tla) 区分工具参数错误、普通文本结果、reload 错误与 settlement 标记；reload 失败回合，不能成为普通下一次模型调用；已观察 turn interrupt 同样阻止续行 | 工具 timeout 的 0/1/300000/300001 边界；native/face 不套工具上限。服务已取得后的调用，face 本身退休时的 unavailable 仍属服务绑定契约。Drop 无回复；后台任务执行/取消不在这个 Wait 模型内 |
| Loop → Preparation → ModelProvider：`model_provider/recovery.rs::invoke`、`RecoveryState::decide` | [RetryChain](RetryChain.tla) 把 attempts/waited 带到第二 request，包含准备失败、压缩交接、取消、不可逆工具输出；malformed/backoff 局部额度重置不重置全链预算 | 两个 request、8 次调用、300 秒等待预算；计划 delay 与实际 elapsed 都取 0/150/300/301。elapsed 可超过计划延时，因此不声称真实累计等待永远 ≤300 秒。等到恰好 300 秒时，**当前请求已获准的下一次重试**仍可调用；新 continuation 的入场检查拒绝已耗尽预算。模型检查每次计划延时不超当时剩余额度；原 RetryBudget 的 WaitBound 仅对应计划等待抽象。实际计时器、jitter 和流解析仍属实现契约 |
| 子调用 → 两个父调用：`tool_executor` 的子结果查询和父结果补 effects | [NestedSettlement](NestedSettlement.tla) 改为两个 parent、三个 child 的全部归属分配；效果不串父调用、父已有返回不重复补入、确认效果保留；截止后允许迟到 child 结算 | `confirmedAtEnd` 固定父结算时刻。已返回 effects 视为 handler 提供的完整聚合；模型检查 host 不再追加，并不证明 handler 自身算对了聚合。父原返回使用序列，保留检查重复项的能力；不是任意深树证明 |
| Hooks：Stop follow-up、tool outcome、普通回合 reset | [HookFollowups](HookFollowups.tla) 每条链最多三次，两条链分别计数；普通回合清零；重载后消费仍使用输入里的 round；成功/失败/recovered tool result 选不同 hook 点 | 两条链，每链 round 0..3；普通回合发生在没有待消费 follow-up 时。不同 handlers 的合并顺序、onFailure、approval_id 隔离仍由独立调度合同及后续模型处理 |
| Prompt：`prompt.rs::consume_result` / `configure_loaded` | [PromptOutcomes](PromptOutcomes.tla) 当前完整 source candidate 才能采用；load/append 失败及 superseded 不改 entries；missing entry 只撤销指定 source/scope；Adopted/Unchanged 有不同含义；丢 waiter 不撤回命令 | 两个配置命令，三个 source/scope 组合，含同 source 不同 scope。载入内容为 0/1（缺失/存在）；scope 本身保持 active。scope 生命周期与退休原子批次仍见 PromptRetirement，不声称两模型已组合证明 |
| ToolBinding：`reconcile/adopt/prepare/release` | [ToolAdoption](ToolAdoption.tla) 监听先于首次读取；activation 前变化不丢；opening 未成功不建请求；动态采用失败保留旧前缀/pins；两个请求分别绑定和释放，各自运行旧 handler | 三个 registry 版本、两个 request、空快照；配置已存在的路径。请求从已采用配置取快照；未配置时 retained-registry 的选择由原 ToolBinding 模型覆盖。handler 退休后的执行细节不在模型内 |
| ContextPreparation / Usage：最终请求检查与初值保存 | [FinalRequest](FinalRequest.tla) 实际 request/counter/limits 身份必须匹配；host 改写后不能重用旧估计；超限先恢复、仍超限可进入失败且不再次恢复/调用；projection 之后的新 compaction 阻止初值保存 | 两个 request 对象/计数器/限制身份、一次 budget recovery、一个较新 cut。数字是否准确和压缩文本质量不是状态命题；没有覆盖 Steer 在压缩中的完整接管和数字贡献来源选择 |
| Goal：`dim-goal/src/fold.rs::decide_turn_end/decide_evaluation/decide_conclusion/decide_evaluated_continuation` | [GoalProgress](GoalProgress.tla) pending input / open tasks 阻止续行，失败可暂停；normal input 重置计数；limit 与连续无工具进展阈值；模型/evaluator 结论；旧 execution/turn 报告无效；edit 不改运行状态；reload/fork 不自动继续 | 两次 execution、四个 turn、limit 为无限/2，无进展阈值 3。evaluation 以 execution/turn 抽象唯一身份，evaluator 路径假定该 turn 含模型 request；不实现 evaluator 子分支协议，也不证明评估内容正确。有限环境可提供普通或 continuation turn，非整个 Goal-Loop 联合模型 |
| Settings / Clients：各自的发布任务 | [SnapshotRefresh](SnapshotRefresh.tla) 监听先于 read，读取中变化保留下一次 wake；read 与 append 失败分开，允许连续失败；成功才改 last；关闭不写；新代清 last，重新报告 | 两个外部值、两次改变、一次 reload。Settings 配置有 retry；Clients 配置无 timer retry、无异步 read failure。`stable` 表示外部值及 I/O 已恢复，配合弱公平调度才检查 Settings 的最终发布；Clients 不作最终成功保证 |
| Permission：`permission.rs::decide` 文件 facts await | [PermissionFacts](PermissionFacts.tla) facts 返回时版本变化重新取策略；系统/owned 写入拒绝；facts 读取失败/取消不能准入；后来收紧不反向撤销已经作出的决定 | 一个 facts 服务存在的 workspace-write 调用、一次 allow→deny 策略改变、五种 facts 结果。不新增排队后的二次授权；command rules、read/unrestricted 和 advisor 组合仍不在本模型内 |
| FileReads：`workspace_tools/file_reads.rs::established/in_context` | [FileBindingQueries](FileBindingQueries.tla) 分文件/分支绑定；read/write/edit 成功均可建立绑定；edit 只需绑定，write 还需上下文保留；查询中有新 compaction 也只按同一捕获 cut 判断；reload 清绑定 | 三个 branch/file 组合、四个位置、一个查询、一次 reload。Bind 是已成功操作的登记，不替代文件写入本身的准入或 CAS；字节/编码算法不在模型内 |

## 这些检查解决什么

原 Wait 模型把通知顺序丢掉，独立观察探针能发现反例；现在保留队列后，同一探针检查通过。
这是补回与当前实现有关的信息，不是把反例性质删掉，也不是修改产品的选择规则。
原反例及其旧文件摘要保留在 [coverage-counterexample.md](coverage-counterexample.md)。

跨请求预算需要同时保存全链总数和请求局部额度；父子效果需要不同 parent 身份；Hooks 需要第二条链；
Goal 需要 pending、任务和评估身份。这些状态都决定可观察结果，不能再作为无结构的“输入有效”前提省略。
同时，有限模型通过只说明这些题设下的公式成立，不等于 Rust 的全部 await、错误和析构分支已经被证明。

## 第一轮结束时的缺口

本节保留第一轮实验边界；后续逐项对应和有限取值见[第二轮补充](coverage-completion.md)。
本次没有把原覆盖表整行改为“完成”。Approval 多 pending 与写失败，InputQueue FIFO/跨 turn handover，
ToolExecutor claim/resolve_write/结果身份，Loop 的完整输入组成，ModelOutput 的半截 ToolUse，Tree 的 opening receipt，
BranchLine 的角色/idle 资格，Collaboration 的 member/request/operation 身份，Exec/Shell 的多类停止竞争，
以及 SessionBroadcast 的 Event 与 Notice/Fence 区别，仍在 [原表](coverage-audit.md) 中。
上表每行列出的未建模部分也保留；Shell crash 问题继续跟踪 #1633。

这份补充没有改变业务设计、运行时、accepted ADR、持久格式、装配或强制 CI/作者模板。
对缺口的收敛按业务场景记录，不以新增模型数量、通过性质数量或缺陷数量衡量完整性。

## 验证与消融

按 README 的统一命令重跑配置；状态目录只在临时目录。新实验使用与原实验相同的 Java 17 / TLA+ tools 1.7.4。
未变动模型按 SHA-256 核对后复用原记录；改动的 WaitRuntime、NestedSettlement 与依赖 WaitRuntime 的探针重新执行。
运行时代码未变，因此复用此前流程验证，不为了模型文档改动重跑 660 项流程测试。

删除了 Goal 为检查单次决定而保存的全历史结果副本，改为在该转换上检查条件；保留去重身份集合。
删除了 WaitHandoff 中恒定的“后台任务仍运行”字段及断言：模型没展开后台执行，不能靠恒定字段声称证明它。
快照失败可以反复发生，不用“一生只失败一次”的标志减少路径；稳定性仅用于有条件活性，不影响安全性探索。
没有新增通用证明平台、运行时包装或每个部件必须填满的模板。
