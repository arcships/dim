# Timeline 写入与发布：运行记录

范围与源码对应见[合同表](../timeline-contract-coverage.md)。这是 #1732 第一批的有限模型结果，
不改变已有能力模型的结论，也不代表查询、动态消费者、跨代接管或 Rust 实现已经证明。

## 环境与结果

- 日期：2026-11-17；源码基线：`c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`。
- OpenJDK 17.0.18，TLA tools 1.7.4，TLC 2.19；单 worker，2 GiB heap，BFS。
- jar SHA-256：`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`。
- [机器记录](publication-results.json)包含最终模型/配置/源码哈希、seed、fingerprint 和可达状态摘要。

| 配置 | 有限范围 | generated / distinct | queue / depth | 时间 | exit |
| --- | --- | --- | --- | --- | --- |
| `TimelinePublication.cfg` | 2 个写入者；最多 2 批，每批 1–2 个事件 | 285,028 / 95,138 | 0 / 16 | 10s | 0 |
| `TimelinePublication-ThreeBatches.cfg` | 2 个写入者；最多 3 批，每批 1 个事件 | 302,722 / 93,734 | 0 / 16 | 9s | 0 |

两个配置均完成状态空间与时序性质检查，输出 `Model checking completed. No error has been found.`。
TLC 给出的乐观 fingerprint 碰撞概率估计分别为 `9.8E-10`、`1.1E-9`；这些是有限检查结果，不是任意规模定理。

检查六个状态不变量：`TypeOK`、`SequenceAndBatch`、`AtomicVisibility`、`TransformOutcome`、
`ExternalPrefix`、`ClosedMeaning`；三个动作性质：`FailureFreezesPublication`、`NoAdmissionAfterClose`、
`DeliveryIsOrdered`；一个条件活性：`ClosingSettles`。精确公式在 [模型](TimelinePublication.tla) 中。

其中 `SequenceAndBatch/AtomicVisibility/TransformOutcome/ExternalPrefix/DeliveryIsOrdered`
是动作定义直接维持的自洽检查，不是五项独立的实现证明。组合交错的结论与存储前提见合同表。
`ClosedMeaning` 不覆盖 idle thaw。

## 题设没有排除的关键窗口

从同次 BFS 的 `-dump` 状态导出中核对以下谓词，机器记录保存第一个匹配状态的编号及相关字段。
它们是**可达状态**，不是失败反例，也不是实际 Rust 执行轨迹。状态编号只对该次运行有效。

| 机器记录中的名称 | 判断条件 / 含义 |
| --- | --- |
| `durable_before_publication` | `durable > published` 且未故障：落盘不等于回调已发布 |
| `publication_before_consumption` | `published > consumed`：发布不等于投影已消费 |
| `inside_batch_delivery` | consumed 非零且不在 ends：已发布完整批次可以只消费到批中；只适用于两事件配置 |
| `closing_waits_for_tail` | closing 且未 closed：关闭会等待已分配尾部 |
| `late_callback_after_sync_rejection` | Timeline failed，store 未异步失败，accepted 小于批数，callbacks 小于 resolved，durable 大于 published：较早批次已落盘，但 store closing 使后次入队同步拒绝；已知 Session 故障的分配前拒绝不属于此窗口 |
| `inflight_delivery_can_outlive_fault` | failed 且 inflight 非零：故障发生时可能已经取出一条事件 |
| `asynchronous_storage_failure` | failed 且 storeFailed：异步失败确实被探索 |
| `both_writers` | events 中 writer 集合为 `{1, 2}` |
| `mixed_transform_results` | events 同时有 failed 和 transformed 的 before 结果 |
| `older_read_snapshot` | `0 < readCut < published`：旧快照返回不是漏发布 |
| `multi_batch_round` | resolved 比 callbacks 至少多 2，durable 非零：一轮落盘多批，回调逐批推进 |

两事件配置找到全部 11 类；三批单事件配置找到除批中消费外的 10 类。
`inflight` 的存在只证明保留了该窗口；其后 Deliver 是否保持顺序由全状态动作性质检查。
没有为制造失败而修改模型规则，也没有把性质本身加入状态空间过滤。

## 复现

沿用 [README 的工具准备](README.md#重跑)。设置 `JAVA` 为 Java 17 可执行文件的绝对路径，
`TLA_JAR` 为已核对哈希的 jar 路径。在仓库根目录执行：

```sh
run_dir=$(mktemp -d /tmp/dim-publication.XXXXXX)
for config in TimelinePublication TimelinePublication-ThreeBatches; do
  "$JAVA" -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC \
    -workers 1 -metadir "$run_dir/$config" \
    -dump "$run_dir/$config-states" \
    -config "docs/design/examples/capability-models/$config.cfg" \
    docs/design/examples/capability-models/TimelinePublication.tla \
    > "$run_dir/$config.log" 2>&1
  result=$?
  printf '%s exit=%s\n' "$config" "$result"
  test "$result" -eq 0 || exit "$result"
done
```

如需相同随机参数，分别添加机器记录中的 `-seed` 和 `-fp`。状态导出为 `*-states.dump`，
按上表字段检查可达性；导出文件较大，仅保留在临时目录，不提交数据库或生产日志。
所有通过记录对应消融后的最终文件；开发时的命名/量词语法错误已修正，不计为有效运行。
删除“登记拒绝发生过”的观察状态后两个配置均重跑；该路径由 stuttering 和既有登记失败流程证据承担。

本批不改变 Rust、依赖、服务面、持久化格式或生产合同，因此不重复执行全工作区 Rust 测试。
既有流程入口是实现对应证据，不能在本记录中计作本次通过；PR 另记录文档、链接与哈希检查。
