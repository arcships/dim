# ToolBinding

分支的工具绑定：把注册表当前的工具集合作为配置的工具部分采用、pin 前缀可能仍声明的来源版本、在 Loop 构造请求时固定本请求的声明与实现。不收集来源（[ToolRegistry](ToolRegistry.md)）、不准入、不执行（[ToolExecutor](ToolExecutor.md)）。

<!-- generated:plugin ToolBinding — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:BranchOpening`、`B:Dispatcher`、`B:Prompt`、`B:Timeline`、`S:ToolRegistry` |
| 提供 | `B:ToolBinding` |
| 被注入于 | `Loop`、`ToolExecutor` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `S:ToolRegistry` | 当前工具与来源集合、按版本取旧集合、变化通知 |
| `B:Prompt` | `adopt_tools`：在 Prompt 的串行采用队列里写 `context/configured` 的工具部分 |
| `B:Timeline` | 读前缀里已采用的配置 |
| `B:Dispatcher` | 订阅 `branch/start`、`branch/resume`：Prompt 在这两个事件分发时写入模型配置，ToolBinding 随后（它依赖 Prompt，分发在其后）对齐工具部分，先于分支接受输入 |
| `B:BranchOpening` | `branch/start`、`branch/resume` 分发时登记对齐工具部分的回执：等 Prompt 初始采用完成后对齐，分支开启等这些回执完成，对齐失败则开启失败 |
| `B:BranchLifecycle` | 后台监听等待分支 Active，分支开始关闭时结束；装配失败时不处理注册表变化 |

## 服务接口

`B:ToolBinding`

| 方法 | 语义 | 错误 | 调用方 |
| --- | --- | --- | --- |
| `bind(request, through, tools, tool_sources)` | 按前缀已采用的声明与来源版本登记快照；成功后释放不再被声明的旧 pin | `tool source <key>@<version> is unavailable`；`tool <name> has no handler`；请求已登记 | [Loop](Loop.md) |
| `bind_current(request, declared)` | 配置没有工具部分时：`declared` 为空则声明注册表当前的工具，否则按当前定义核对 `PrepareModelHost` 给出的声明；返回声明 | 声明名不存在或与当前定义不等 | Loop |
| `handler(request, name)` | 本请求快照中的定义与处理器 | `request <id> has no active tool binding`；`Unknown tool: <name>` | ToolExecutor |
| `transfer(from, to)` | 预算恢复时把快照转给新请求 | `from` 无快照 | Loop |
| `release(request)`、`retain_only(request?)` | 释放快照 | — | Loop |

`handler` 只读快照：来源在调用途中被替换或撤销，不影响本请求。

## 事件

| 事件 | 方向 | 时机 | 说明 |
| --- | --- | --- | --- |
| `context/configured` | 经 `Prompt::adopt_tools` 写出 | `branch/start`、`branch/resume` 分发时，装载时（能力重载），或注册表变化后：已有模型配置且当前工具部分与本分支最新采用的不同 | 只替换 `tools` 与 `tool_sources`，模型部分与 system 沿用 |

只订阅 `branch/start`、`branch/resume`（对齐工具部分）；其余采用由注册表的 `changed()` 驱动，绑定与释放由 Loop 调用。

## 状态

| 状态 | 内容 | 重建 |
| --- | --- | --- |
| pin 列表 | `[(本分支序号, 集合)]`：已采用、可能仍被请求前缀声明的集合；装载时直接 pin 的记 0 | 装载时按前缀重建 |
| 最新采用 | 本分支最近采用的工具部分 | 同上 |
| 请求快照 | `request → { 名称 → ToolContribution }`，并强持所声明的集合 | 不跨重载；Loop 单独重载时 `retain_only` |

## 生命周期

```text
apply:
  前缀无已采用配置 ──> 直接提供服务；请求经 PrepareModelHost + bind_current
  前缀配置的工具部分 == 注册表当前（没有工具部分且没有动态来源也算一致） ──> pin(0)
  否则 ──> adopt_tools(当前) ──> pin(采用序号)
  之后才提供 B:ToolBinding：Loop 的首个请求一定在采用落入 Timeline 之后
后台监听: 等分支 Active；装配失败则退出；等待期间的 changed 通知保留
注册表 changed(): 当前 != 最新采用 ──> adopt_tools ──> 追加 pin；失败记日志，下次变化重试
卸载: 停止监听，释放 pin 与全部快照
```

pin 释放：`bind(…, through, …)` 成功后，取序号不大于 `through` 的最大 pin，释放比它更早的 pin。同一分支同时只有一个请求在准备或执行，且 `through` 单调递增，之后的请求前缀都含这个 pin。

重载：注入链为来源方、`ToolHost` → ToolRegistry → ToolBinding → Loop、ToolExecutor；Prompt → ToolBinding。ToolBinding 重载时快照随旧实例释放，ToolExecutor 以重载结算在途批次。

## 不变式

- 同一请求的声明与实现来自同一版本。
- 被某个请求快照或 pin 持有的集合不会被释放。
- 配置的工具部分只由 ToolBinding 经 Prompt 写入；模型部分只由 `configure` 写入，两者互不覆盖。

## 为什么

- 采用、pin、快照放在同一个分支插件：三者生命周期相同，随分支卸载一起释放。
- 没有工具部分且没有动态来源时不采用：首版没有动态来源，固定工具由 `bind_current` 直接声明，不为每个分支多写一条 `context/configured`。
- 不做“同名改绑当前 handler”：前缀版本不可取得就明确失败，版本变化一律留下采用事实。

## 代码与测试

- `crates/dim-agent-capabilities/src/tool_binding.rs`
- `tests/integration/tests/tool_binding.rs`
