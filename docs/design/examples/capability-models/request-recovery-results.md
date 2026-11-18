# 请求恢复结算：有限检查记录

对应 #1732；[合同、代码及范围](../owner-handoff-verification.md#6-loop--preparation--model-的恢复结算责任)。
记录版本及哈希范围见 JSON 的 `evidence_revision` 和 `hash_scope`。2026-11-17 使用 OpenJDK 17.0.18、
tla2tools 1.7.4 / TLC 2.19，单 worker、2 GiB heap。

| 配置 | generated | distinct | queue | depth | exit |
| --- | ---: | ---: | ---: | ---: | ---: |
| RequestRecovery.cfg | 8,316 | 2,442 | 0 | 8 | 0 |
| RequestRecovery-Failure.cfg | 18,964 | 5,568 | 0 | 8 | 0 |

两配置各有 60 个初态，队列耗尽，无反例；没有公平性、活性性质或状态约束。
检查终态唯一、责任方正确、全部恢复成功后无漏项；发布上界另作模型自洽检查。
原有 InterruptHandoff / GenerationHandoff 未变，不重复计算其结果。

[JSON](request-recovery-results.json)保留模型/config/source hashes、工具哈希、seed/fingerprint、
三个 owner 分别负责结算的可达状态及“不同 cut、终态未发布”的可达状态。
这些状态证明关键窗口没有被删掉，不是 Rust 轨迹或失败反例。
早期语法检查失败不计入通过记录。

在仓库根目录按 [README](README.md#重跑) 配置 JAVA/TLA_JAR 后运行：

```sh
run_dir=$(mktemp -d /tmp/request-recovery.XXXXXX)
for config in RequestRecovery RequestRecovery-Failure; do
  "$JAVA" -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC \
    -workers 1 -metadir "$run_dir/$config" \
    -config "docs/design/examples/capability-models/$config.cfg" \
    docs/design/examples/capability-models/RequestRecovery.tla
  result=$?
  test "$result" -eq 0 || exit "$result"
done
```

循环使用新随机参数；复现记录参数时增加 JSON 中该运行的 `-seed` 和 `-fp`。
普通配置增加 `-dump "$run_dir/witness"` 可导出可达状态；编号仅对那次运行有效。


## 实现验证

`cargo test --locked -p dim-agent-capabilities --test context_flow restoring_each_preparation_handoff_settles_without_replaying_work -- --exact`：
1 passed，0 failed；一个流程内执行九种持久化边界，原测试已检查终态总数唯一；新增按 turn/request 计数，确认终态属于正确身份。
对应 context_flow 的 Clippy（`-D warnings`）通过；没有重跑全部 context_flow 或工作区。
使用独立 worktree target、隔离 DIMCODE_HOME、4 build jobs、CARGO_INCREMENTAL=0。
本记录固定于该次实验版本；后续新增的测试及哈希见[交接补充记录](live-handoff-results.md)。
