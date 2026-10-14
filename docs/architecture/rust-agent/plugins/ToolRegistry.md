# ToolRegistry

> 部分文档：只写本地执行相关的合同。完整设计见[工具贡献](../tool-contributions.md)。

Session 的工具：按 `SessionConfig.tools` 的顺序，把 `S:ToolHost` 的工具与 Session 能力贡献的工具合成固定表；收集登记的动态来源（`S:SessionToolSource`），整组校验、原子替换。不裁决权限、不执行工具、不决定分支声明什么（[ToolBinding](ToolBinding.md)）。

<!-- generated:plugin ToolRegistry — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `S:SessionConfig`、`S:ToolContribution#branch`、`S:ToolContribution#skill`、`S:ToolContribution#wait`、`S:ToolHost` |
| 提供 | `S:ToolRegistry`、`S:ToolSourcesView` |
| 被注入于 | `PrepareModelHost`、`ToolBinding` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:ToolHost` | 宿主工具的定义与执行；本地执行下由 [WorkspaceTools](WorkspaceTools.md) 提供 |
| `S:SessionConfig` | `tools`：本 Session 启用的工具名与顺序 |
| `S:ToolContribution#branch` | 贡献的 `branch` 工具（TimelineTree） |
| `S:ToolContribution#skill` | 贡献的 `skill` 工具（[Skills](Skills.md)） |
| `S:ToolContribution#wait` | 贡献的 `wait` 工具（[Wait](Wait.md)） |

贡献的注入由装配时 `AgentBuilder::session_tool(name)` 登记的名字决定；动态来源由 `AgentBuilder::session_tool_source(name)` 登记，注入 `S:SessionToolSource#<name>`（登记名无提供方时 Session 构建失败）。上表是本地执行产品装配下的结果，首版没有登记动态来源；MCP 接入时登记 `mcp`。

## 服务接口

`S:ToolRegistry`

| 方法 | 语义 | 调用方 |
| --- | --- | --- |
| `snapshot()` | 同一把锁内读出的全部声明与其来源集合 | ToolBinding（采用与 `bind_current`） |
| `definitions()` | 全部声明：固定工具在前（`SessionConfig.tools` 顺序），来源工具按键排序、键内按集合顺序 | PrepareModelHost |
| `fixed(name)` | 固定工具的定义与处理器 | ToolBinding |
| `sources()` | 当前各键的集合 | 当前无调用方；ToolBinding 使用 `snapshot()` 同时读取声明与集合 |
| `source(key, version)` | 当前或仍被持有的旧版本集合 | ToolBinding |
| `changed()` | 来源替换或撤销后通知（可合并） | ToolBinding |
| `sources_state()` | 每个键的 `adopted` 版本与 `rejected { version, reason }` | 宿主经 `S:ToolSourcesView` 读取 |

另外提供 `S:ToolSourcesView`（`PublicService`）：只含上述状态的只读视图，宿主经 `Session::session_service::<ToolSourcesView>()` 读取。ToolRegistry 本身不公开，因为它能交出工具处理器。

动态来源端口：

```rust
pub struct SessionToolSource(pub Arc<dyn ToolProvider>);
pub trait ToolProvider {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>>;   // 每个键一个完整集合
    fn changed(&self) -> watch::Receiver<()>;     // 可合并，不带数据
}
pub struct ToolSourceSet { key, version, tools: Vec<Arc<ToolContribution>> }
```

## 来源替换

任一来源方通知变化时，重读**全部**来源方，逐键处理。刷新从读取到提交整体串行，较早的读数不会覆盖较晚的；一轮内反复处理直到没有新采用，所以同一轮里让出的名字当场被被拒来源接手：

| 情况 | 结果 |
| --- | --- |
| 键不再出现 | 撤销：移出当前表，旧集合只留弱引用 |
| 版本与当前相同 | 不变 |
| 校验通过（键属于该登记名；版本未被持有过；工具名 `[A-Za-z0-9_-]{1,64}`、集合内唯一、不与固定工具或其他键的当前工具重名） | 原子替换，旧版本转弱引用，记 `adopted`，通知 `changed()` |
| 校验失败 | 整组拒绝，保留旧集合，记 `rejected`；被拒版本不算已持有，下次任一变化时重新校验，所以占名的来源撤销后被拒来源自动恢复 |

旧版本靠 `Arc` 所有权保留：ToolBinding 的 pin 或请求快照仍持有时，`source(key, version)` 仍能取得；全部释放后集合与其处理器一并释放。

## 本次改动

- 删除 `ToolRegistry::execute` 与 `ToolDriver::authorize`：执行准入移到分支的 [Approval](Approval.md)。
- 删除 `get(name)`：执行按请求快照取实现，归 ToolBinding。
- 固定表按 `SessionConfig.tools` 的顺序声明（原来按名字排序）。
- `risk` 由工具定义声明（`ToolDefinition.risk`），ToolRegistry 原样携带，不解释。
- 装配时的错误：启用名既不在宿主也不在贡献中，或同名重复，装载失败（`InvalidInput`）。

## 生命周期

任一注入（`ToolHost` 或某个贡献）重载时随之重载，下游 ToolBinding（及其下游 Loop、ToolExecutor）、PrepareModelHost 按[能力重载协议](../single-tree.md#能力重载协议)重载；在途调用由 ToolExecutor 以重载结算。

## 代码与测试

- `crates/dim-agent-capabilities/src/tool_registry.rs`
