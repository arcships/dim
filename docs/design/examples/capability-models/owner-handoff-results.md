# Owner 交接 TLC 记录

设计、来源与边界见[对应表](../owner-handoff-verification.md)。机器记录见
[owner-handoff-results.json](owner-handoff-results.json)，包含最终模型/config、源码、读取过的流程测试哈希，
四次最终运行摘要和 12 个可达窗口的状态摘录。

记录版本及哈希范围见 JSON 的 `evidence_revision` 和 `hash_scope`。
本记录仅包含 InterruptHandoff / GenerationHandoff 的四次运行，该阶段未重跑 Rust 行为测试。
后续加强的请求恢复断言与运行结果见[请求恢复记录](request-recovery-results.md)，
本次独立重启与通知故障见[交接补充记录](live-handoff-results.md)；不将旧哈希称为最终交付树。

| 配置 | 边界 | generated | distinct | 队列 | 深度 | exit |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| InterruptHandoff.cfg | 1 turn，每 turn 至多一次 interrupt、一次 batch | 362 | 221 | 0 | 29 | 0 |
| InterruptHandoff-TwoTurns.cfg | 2 turns，同上 | 2,699 | 1,640 | 0 | 58 | 0 |
| GenerationHandoff.cfg | 一个调用，各 owner 一次换代，无 crash | 29,158 | 10,275 | 0 | 21 | 0 |
| GenerationHandoff-Crash.cfg | 同上，额外允许一次 crash | 40,379 | 10,908 | 0 | 21 | 0 |

四项均完成 BFS，无反例、无状态约束截断。TLC 2.19（tla2tools 1.7.4）、OpenJDK 17.0.18，
单 worker、2 GiB heap。使用状态指纹，碰撞概率估计见 JSON 中原始摘要，不称作无界数学证明。
没有公平假设或活性性质；CHECK_DEADLOCK FALSE，因为有意允许完成、故障、等待外部工作的终止状态。

具体启用的公式由 `.cfg` 和 `.tla` 定义，不以性质数量衡量证明力度。
GenerationHandoff 的 PublicationPrefix、OneTerminalPerHistory、NoOldWorkReexecution、
RebuildDoesNotInvoke、CrashDoesNotUndoEffects、WaitRemovalDrainsCall、FailedRebuildNotReady
主要核对动作定义/前提的自洽性，不作为独立验证成果；取消不回滚、host 不重放等仍需实现证据。
交接检查集中在 ReloadDoesNotContinue、失败终态的 EndAfterTerminal，以及结算与卸载组合；
正常工具结果只走继续决定，最终正常 turn/end 不在这个模型内。

## 可达性，不是只看全绿

从 TwoTurns / Crash 两个完整遍历的 `-dump` 中选取：

1. interrupt 已提交、Loop 尚未消费，迟到请求仍然写入。
2. 同一迟到请求被 ToolExecutor 拒绝并写结果。
3. 第一 turn 中断后，第二 turn 的工具可以开始。
4. 普通结果令旧 Loop 决定继续，作为重载失败路径的对照。
5. Wait 已完成换代，ToolExecutor 尚未提交该调用结果。
6. Loop 恢复读到尚未发布的终态。
7. Loop 已重建，仍等 ToolExecutor 的恢复结算。
8. 工具 cleanup 返回错误且卸载结束，历史仍没有终态。
9. 一个 owner 重建失败没有直接设置存储故障；模型不展开 required apply 失败后的装配回滚，不能推断真实分支会继续运行。
10. 进程崩溃丢了未落盘结果，已发生的 host effect 仍存在。
11. 崩溃后恢复补终态并失败结束 turn，没有重新调用 host。
12. 持久化故障下，无需补写的只读重建仍可返回成功；故障并未被清除。

状态编号和关键字段保存在 JSON；它们是正向可达证据，不是故意写反性质制造的“反例”。
没有发现新运行时缺陷。实际修正的是旧文档将 recovery 误写成再次执行的表述。

## 复现

从仓库根运行，`TLA_JAR` 指向 SHA-256 为
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88` 的 jar：

```sh
java -Xmx2g -cp "$TLA_JAR" tlc2.TLC -workers 1 \
  -config InterruptHandoff.cfg -metadir /tmp/dim-owner-interrupt \
  docs/design/examples/capability-models/InterruptHandoff.tla
java -Xmx2g -cp "$TLA_JAR" tlc2.TLC -workers 1 \
  -config InterruptHandoff-TwoTurns.cfg -dump /tmp/dim-owner-interrupt.dump \
  -metadir /tmp/dim-owner-two-turns \
  docs/design/examples/capability-models/InterruptHandoff.tla
java -Xmx2g -cp "$TLA_JAR" tlc2.TLC -workers 1 \
  -config GenerationHandoff.cfg -metadir /tmp/dim-owner-generation \
  docs/design/examples/capability-models/GenerationHandoff.tla
java -Xmx2g -cp "$TLA_JAR" tlc2.TLC -workers 1 \
  -config GenerationHandoff-Crash.cfg -dump /tmp/dim-owner-generation.dump \
  -metadir /tmp/dim-owner-crash \
  docs/design/examples/capability-models/GenerationHandoff.tla
```

各次 fingerprint、seed 和 log SHA 在 JSON 中；需要复现同一指纹选择时传 `-fp` / `-seed`。
每次使用新的 metadir。模型是有限例题，不需要新增 CI 平台或部署依赖。
