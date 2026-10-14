# FileReads

分支记录「模型在本分支读到过的文本文件」：每个路径一条绑定（原始字节修订与解析出的文本格式）。覆盖已有文件必须有本分支的绑定。不读写文件，只存绑定。

<!-- generated:plugin FileReads — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | — |
| 提供 | `B:FileReads` |
| 被注入于 | — |
<!-- /generated -->

## 依赖

无注入。

## 服务接口

`B:FileReads`，只对 crate 内开放：

| 方法 | 语义 |
| --- | --- |
| `get(path)` | 本分支对规范路径的绑定 |
| `set(path, binding)` | 记录或替换绑定 |

`TextFileBinding`：`raw_revision`（原始字节 SHA-256）、`encoding`（`Utf8`、`Utf16Le`、`Utf16Be`、`Legacy { name, encoding }`）、`encoding_source`（`Bom`、`User`、`Utf8Policy`）、`bom`、`eol`（`None`、`Lf`、`Crlf`、`Mixed`）。`Legacy` 只由显式的 `read.encoding` 建立，从不猜测。

## 运行时调用方

| 调用方 | 方式 |
| --- | --- |
| [WorkspaceTools](WorkspaceTools.md) 的 `read`、`write`、`edit` | `TreeCommands.get(context.branch_id)?.service::<FileReads>()` |

## 状态

| 状态 | 位置 | 重建 |
| --- | --- | --- |
| 路径 → 绑定 | 内部 | 不重建：随分支实例存在；重载或重启后为空，模型需重新 `read` 才能覆盖 |

## 生命周期

装载时提供空表；卸载时丢弃。

## 为什么

- 按分支而非 Session：分支是独立的对话，一个分支读过文件不代表另一个分支的模型见过它的内容。
- 不写事件、不持久化：绑定描述的是模型当前上下文，重载后要求重新读取是安全的保守行为。

## 代码与测试

- `crates/dim-agent-capabilities/src/workspace_tools/file_reads.rs`
- `crates/dim-agent-capabilities/tests/workspace_files.rs`（`writes_require_a_fresh_read_on_the_same_branch`）
