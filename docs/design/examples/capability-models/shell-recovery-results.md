# Shell 恢复修复的验证

原基线的两个失败配置和记录保持不变：`RecoverHistory = FALSE` 对应当时最小执行配置没有恢复入口。
[#1640](https://github.com/arcships/dim/pull/1640)（`89a4e1760`，修复 #1633）让该配置挂载现有 BackgroundExec，ShellSession 声明对它的装载依赖，
只写终态，由 BackgroundExec 统一恢复和发通知。这里没有修改模型动作或削弱性质。

[修复配置](ExecLifecycle-ShellRecovered.cfg) 使用 `RecoverHistory = TRUE`，与已有 [BackgroundExec 配置](ExecLifecycle.cfg)逐字相同。
这是已有模型的重跑，不增加状态空间或承诺覆盖范围；单独保留配置名，是为了将 Shell 修复后的装配与这次运行明确对应。
已有运行见 [原始记录](results.json)。
TLC 检查五个不变量与 `StopKeepsWork`、`RecoveredSettled`、`RecoveredWoken`：
生成 2,005 个状态，624 个不同状态，队列耗尽，深度 14，退出码 0。
[机器记录](shell-recovery-results.json) 保存配置、模型和工具摘要及复现命令；工具下载见 README。
这是原 70 个配置之外单独记录的一次重跑，不改写历史统计。

代码联系由 `minimal_execution` 流程验证：给卸载后的存储放入“开始但未结束”与“结束但未通知”的已提交前缀，
恢复后分别补失败终态与缺失通知；取消不通知，再恢复不重复，历史命令不执行，已有输出保留。
新增用例在修复前失败，修复后通过。另一个用例重载恢复组件，检查旧 Shell 结算与新代继续执行。
该提交的最小执行流程 25 项通过，本地执行流程 38 项通过、1 项按原配置忽略；App Server 共存流程、受影响包的 Clippy 与架构检查通过。

上述定向测试不能替代 #1640 的完整工作区 CI；完整 CI 是独立的验证关卡。

该模型仍不证明 Rust、PTY、宿主崩溃时的操作系统清理，也没有验证所有配置组合。
持久前缀测试不是杀死真实宿主的实验；重载顺序由运行时流程测试核对。
