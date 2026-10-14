# HostPlatform

本进程的机器事实（home、系统、架构、时区、环境、执行 shell），以及启动子进程的唯一入口。不决定命令能否执行、不管理输出、不知道 Session 与分支。

<!-- generated:plugin HostPlatform — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | `P:FileStorage` |
| 提供 | `P:HostPlatform` |
| 被注入于 | `PromptSources`、`SkillsCatalog`、`WorkspaceTools` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `P:FileStorage` | 读 `exec-shell.json`（配置了 `exec_shell_file` 时才注入） |

## 构造输入

`HostPlatformPlugin::new(HostPlatformConfig { exec_shell_file })`：`Some((root, relative))` 指向 FileStorage 某根下的 `exec-shell.json`；`None` 表示没有 profile home，shell 自动解析，也不注入 FileStorage。

## 服务接口

`P:HostPlatform`，装载时确定，进程内不变。

| 字段/方法 | 语义 |
| --- | --- |
| `home` | 用户主目录（Unix：`HOME`，再退到 passwd；Windows：`USERPROFILE`，再退到系统 Profile 目录） |
| `os`、`arch` | `Os::{Macos, Linux, Windows}`；`arch` 取 `arm64`、`x64` 这类名字，供环境上下文与工具文案使用 |
| `timezone` | 进程启动时的 IANA 时区名；系统不报告时为 `None` |
| `env` | 服务进程启动时的环境快照，从不包含客户端的环境 |
| `shell` | `Available(ExecShellRuntime)` 或 `Unavailable { code, reason }` |
| `spawn(ProcessSpec) -> ProcessTree` | 以给定 argv、cwd、完整环境（不继承）启动进程，stdin 为空，stdout/stderr 为管道；进程成为自己进程树的根 |

`ProcessTree`：

| 方法 | 语义 |
| --- | --- |
| `stdout()` / `stderr()` | 各取一次 |
| `wait()` | 等根进程退出；可取消 |
| `terminate()` | 终止整棵树并确认没有残留；失败返回错误 |
| 丢弃 | 只有根自行退出并被 `wait` 回收、且从未请求终止时保留其后代；其余情况杀掉剩下的进程，包括 `terminate` 未能确认清空时 |

调用方：[WorkspaceTools](WorkspaceTools.md)（exec 的 shell、环境、`spawn`；文件工具的 `home`）、[PromptSources](PromptSources.md)（环境上下文）、[SkillsCatalog](SkillsCatalog.md)（用户 Skill 根目录）。

## Shell 解析

只在装载时做一次，进程内所有 Session 共用这一结果（修改配置对之后启动的服务进程生效）：

```text
exec-shell.json ──读失败/格式错──► Unavailable(EXEC_SHELL_CONFIG_INVALID)
      │ { version: 1, executablePath? }
      ▼
探测候选 shell
  Unix：$SHELL、passwd 登录 shell、/etc/shells、/bin 与 PATH 中的 bash/zsh/sh/dash/ksh
  Windows：pwsh、Windows PowerShell、cmd、Git Bash，并发启动探测进程确认版本；
           Git Bash 还需其 Git 根可用且不是产品自带的 Git
      │
      ├─ 指定了 executablePath ──► 在候选中按路径匹配 ── Explicit
      └─ 未指定 ──► Unix：受支持的登录 shell（Login），否则 /bin/bash → bash → /bin/sh → sh（Fallback）
                    Windows：pwsh → Windows PowerShell → cmd（Automatic）
      │
      └─ 没有可用的 ──► Unavailable(EXEC_SHELL_UNAVAILABLE)
```

`output_encoding`：Git Bash、Windows PowerShell、cmd 为 `Unknown`（按系统代码页解码），其余 `Utf8`。

## 进程树

| 平台 | 启动 | 终止 |
| --- | --- | --- |
| Unix | `setsid`：子进程是新会话与进程组的组长 | 向进程组发 SIGTERM，2 秒内组内无成员即成功；否则 SIGKILL 再等 2 秒；仍有残留返回错误 |
| Windows | 挂起创建（`CREATE_SUSPENDED`、`CREATE_NO_WINDOW`），加入新 Job Object（`KILL_ON_JOB_CLOSE`）后恢复，后代不会逃出；根正常退出并回收后丢弃句柄时取消该限制，保留后代 | `TerminateJobObject`，回收根进程，确认 Job 内活动进程为 0 |

## 生命周期

- 装载：读配置、解析 shell、采集事实，然后提供服务；此后不变。
- shell 不可用只记录在 `shell` 中，不使装载失败：服务照常启动，exec 返回 `EXEC_SHELL_UNAVAILABLE` 或 `EXEC_SHELL_CONFIG_INVALID` 错误。
- 不持有子进程：`ProcessTree` 由调用方独占。

## 不变式

- 本进程内启动子进程只经 `spawn`。
- 子进程环境完全由 `ProcessSpec.env` 给出。
- 同一 `ProcessTree` 的 `terminate` 至多执行一次终止。

## 为什么

- 事实在进程级解析一次：shell 探测在 Windows 上需要启动探测进程，代价不应落在每个 Session。
- shell 问题不使服务失败：没有可用 shell 时读写文件、对话仍然可用，失败只属于 exec。
- 进程树放在这一层：终止整棵树的做法与平台绑定，工具只表达「启动、等待、终止」。
- 不提供沙箱：沙箱是宿主对 `spawn` 的替换或包装，不暴露给工具与权限层，首版不引入。

## 代码与测试

- `crates/dim-agent-capabilities/src/host_platform.rs`
- `crates/dim-agent-capabilities/tests/host_platform_flow.rs`
