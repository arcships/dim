# Consumer handoff：运行记录

Refs #1732 C；[范围、实现和前提](../consumer-handoff-verification.md)。
日期 2026-11-17，源码基线 `c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`。

## TLC

Java 17.0.18；tools 1.7.4 / TLC 2.19；BFS，单 worker，2 GiB heap。
jar SHA-256：`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`。
[机器记录](consumer-handoff-results.json)保存模型/config、基线源文件、锁定 rutis 源文件、修改后测试的哈希及运行参数。

| 配置 | generated / distinct | queue / depth | exit |
| --- | --- | --- | --- | --- |
| ConsumerHandoff.cfg | 157,608 / 53,499 | 0 / 27 | 0 |
| ConsumerHandoff-TwoConsumers.cfg | 2,347,002 / 573,915 | 0 / 31 | 0 |
| ConsumerHandoff-ThreeFilters.cfg | 5,972,409 / 1,130,830 | 0 / 29 | 0 |

三种配置均输出 `Model checking completed. No error has been found.`，队列耗尽。
四个状态不变量：`OrderedOnce`、`NoHandlerBeforeRebuild`、`ExactLivePrefix`、`NoFutureEventAfterFault`；
两个动作性质：`RetainsFirstFault`、`RemovalPreservesHistory`。无活性或任意代数的普遍证明。
未发现模型反例，未确认运行时缺陷；发现并修正了测试没有调用真正 consume 的证据缺口。

单注册配置额外导出同一模型的完整 BFS 状态，确认九类可达窗口：

| 记录名 | 实际匹配条件 |
| --- | --- |
| held_during_rebuild | phase=rebuild 且 held 非空 |
| arrivals_while_drain_awaits | source=drain、busy 非零且 held 非空 |
| batch_and_held_are_distinct | batch 与 held 同时非空 |
| live_cut_covers_unpublished | phase=live 且 cut > published |
| failed_rebuild_still_holds | phase=failed、ready=false、held 非空、calls 为空 |
| removed_with_inflight_call | active=false 且 busy 非零 |
| ordinary_error_advances_current | fault=error、current=0、cursor=faultAt>0 |
| panic_does_not_advance_current | fault=panic、current=0、cursor<faultAt |
| successful_live_suffix | 活跃 live 注册，无故障、派发间隙且 calls 非空 |

状态摘要和该次编号保存在 JSON 中；它们是模型可达状态，不是 Rust 轨迹或失败反例。
这些观察不改变模型转移，也没有通过引入假错误来制造反例。

## 实现验证

修改后的 timeline_tail 使用真正的 Dispatcher.consume，删除手写暂存/covered 交接实现。
原有恰好一次、cut 和顺序断言保留；原未发布/无尾部场景均执行。

- `cargo test --locked -p dim-agent-integration-tests --test timeline_tail`：8 passed，0 failed。
- `cargo clippy --locked -p dim-agent-integration-tests --test timeline_tail -- -D warnings`：通过。
- `cargo fmt --all --check`：通过。

使用 Rust 1.100.0、该 worktree 独立 target、4 个 build jobs、CARGO_INCREMENTAL=0，
DIMCODE_HOME 指向临时隔离目录。未修改运行时代码，未执行全工作区或其他旧流程；它们不计作本次结果。

## 复现

沿用 [README](README.md#重跑) 的 Java/jar 准备，在仓库根目录运行：

```sh
run_dir=$(mktemp -d /tmp/dim-consumer-handoff.XXXXXX)
for config in ConsumerHandoff ConsumerHandoff-TwoConsumers ConsumerHandoff-ThreeFilters; do
  "$JAVA" -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC \
    -workers 1 -metadir "$run_dir/$config" \
    -config "docs/design/examples/capability-models/$config.cfg" \
    docs/design/examples/capability-models/ConsumerHandoff.tla \
    > "$run_dir/$config.log" 2>&1
  result=$?
  printf '%s exit=%s\n' "$config" "$result"
  test "$result" -eq 0 || exit "$result"
done
```

单注册加 `-dump "$run_dir/witness"` 导出 `witness.dump`，按上表字段核对可达窗口。
使用 JSON 的 `-seed`、`-fp` 可复现记录中的随机参数；状态编号只对该运行有效。
结果只用于这里声明的范围，查询正确性、owner cut 充分性、完整卸载协议及客户端错误观察仍有自己的验证责任。

评审修正：Rust 流程增加 rebuild-start 信号和有界完成等待，确保发布前已注册。
补充流程已执行真实注册撤销/重新注册、drain 等待中新到事件、恢复/drain 错误及双消费者普通实时错误。模型/config 未改，原 TLC 结果复用。RetainsFirstFault、RemovalPreservesHistory 是按定义成立的自洽检查，
不按两项新增实现证明计数；模型/config 未修改，原 TLC 证据复用。
