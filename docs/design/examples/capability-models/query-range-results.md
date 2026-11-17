# 查询范围与分页：TLC 运行记录

Refs #1732；[范围和源码对应](../query-range-verification.md)。源码基线
`c8e4edd5b6cf01936ec1478b8d000f5476da7a1f`，日期 2026-11-17。

Java 17.0.18、TLA tools 1.7.4 / TLC 2.19、BFS、单 worker、2 GiB heap。
jar SHA-256：`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`。
精确模型/config/source 哈希、seed、fingerprint 与结束时间见[机器记录](query-range-results.json)。

| 配置 | generated / distinct | queue / depth | exit |
| --- | --- | --- | --- |
| QueryRange-unmatched.cfg | 3,686,272 / 1,736,496 | 0 / 36 | 0 |
| QueryRange-first.cfg | 4,703,232 / 2,296,832 | 0 / 20 | 0 |
| QueryRange-last.cfg | 4,703,232 / 2,296,832 | 0 / 20 | 0 |
| QueryRange-count.cfg | 4,703,232 / 2,296,832 | 0 / 20 | 0 |
| QueryRange-TwoRows.cfg | 3,287,872 / 1,538,480 | 0 / 20 | 0 |

所有配置有 1,024 个初态，状态队列耗尽，均输出 `Model checking completed. No error has been found.`。
检查 `SourcePartition`、`ScanAgrees`、`PagesArePrefix`、`CompleteResult`、`ErrorIsNotEmptySuccess`
五个状态不变量和 `FixedRange` 动作性质。没有活性性质或公平性承诺，没有状态约束剪枝。

本批未发现模型反例，也未确认新的实现缺陷。比较覆盖固定逻辑范围与独立拼接/扫描结果，
以及成功返回的页前缀/完整性；它不证明全部查询表达式、SQL 生成或 Rust await/错误分支。
错误分类公式本身只核对模型中的错误没有伪装成成功，不把这一简单检查说成错误传播实现的证明。

## 复现

沿用 [README](README.md#重跑) 的工具准备；`JAVA` 指向 Java 17，`TLA_JAR` 指向已核对哈希的 jar。
在仓库根目录运行：

```sh
run_dir=$(mktemp -d /tmp/dim-query-range.XXXXXX)
for kind in unmatched first last count TwoRows; do
  "$JAVA" -XX:+UseParallelGC -Xmx2g -cp "$TLA_JAR" tlc2.TLC \
    -workers 1 -metadir "$run_dir/$kind" \
    -config "docs/design/examples/capability-models/QueryRange-$kind.cfg" \
    docs/design/examples/capability-models/QueryRange.tla \
    > "$run_dir/$kind.log" 2>&1
  result=$?
  printf '%s exit=%s\n' "$kind" "$result"
  test "$result" -eq 0 || exit "$result"
done
```

上面的循环使用新的随机参数。若需复现记录的随机参数，须在每条命令显式传入对应运行的 `-seed <seed> -fp <fingerprint>`。计数对应最终文件，早期语法错误、
范围构造拆分前的运行和被终止的开发运行不计为本批通过证据。

本批不改运行时代码，未重跑 Rust 工作区；补充实现验证的范围见下节。
文档链接、哈希、ADR freeze 和 diff 检查由本 PR 验证。没有引入通用验证平台、强制 CI 或新业务约定。

评审修正后五配置全部重跑：first/last 同时比较 group 与位置，from 扩展为 0..6，
覆盖当前分支 sequence 2/3。0 仍只是缺省哨兵，wire 显式零拒绝没有由模型验证。
SourcePartition 只比较集合，不验证 tail 向量顺序及单来源重复行。


## 游标绑定的实现验证补充

新增表驱动的真实存储查询流程，覆盖 SQLite/Memory 的 Session、分支及范围、当前/祖先 cut、from，以及 filter/select/group/pick/order 变化。
各变体独立执行成功后，再携原游标断言 binding 输入错误；页大小可改变。物理落盘的兼容正例复用既有 tail-after-commit 流程。

`cargo test --locked -p dim-app-server --test storage_flow property_queries::`：28 passed，0 failed，137 filtered out。
本轮执行该模块，未重跑全工作区。测试文件当前哈希见 JSON 的 modified_flow_test；source_sha256 保留原 code_base 的历史基线。
使用隔离 DIMCODE_HOME、独立 worktree target、4 build jobs、CARGO_INCREMENTAL=0。模型与配置未变，以上五组 TLC 结果复用，没有声称重跑 TLC 或 Rust 精化证明。

对应 storage_flow 的 Clippy（`-D warnings`）、格式、ADR freeze、diff 和模型/config/基线/修改后测试哈希检查通过。

## main 集成与续页身份复核

合入 `543ae5204f94bc9bee73aacf27f2a3ea13840487`，解决 README 冲突时保留 B/C 两个入口。
Timeline 查询实现未变。续页正例从只检查两行，加强为检查完整 group/values 与同范围完整参照页
的对应切片相等，并检查首行身份；直接比较 QueryRow，不增转换层。
模块运行中其余 27 项通过；新断言最初漏传行查询必需的 page，导致 fixture 失败，
改为 page(10) 并确认无后续页后，最终目标流程 1 passed / 0 failed。复用未受影响的 27 项，
不把这两次结果写作一次 28 项全绿。最终 storage_flow Clippy -D warnings 通过。
JSON main_integration 保留最终测试哈希；旧运行快照不改写。模型/config 未变，复用 TLC。
消融：只增加同一公共接口的结果对照，保留完整身份断言；不新增测试框架或运行时改动。
