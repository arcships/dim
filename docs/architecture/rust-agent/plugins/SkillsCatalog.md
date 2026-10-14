# SkillsCatalog

进程级的 Skill 发现与全局启停：扫描用户与项目的 Skill 根目录，合成目录视图，经 Skill 设置端口读写禁用集合。不知道 Session 与分支，不决定模型看到什么（[Skills](Skills.md)）。

<!-- generated:plugin SkillsCatalog — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | `P:HostPlatform`、`P:SkillSettingsHost` |
| 提供 | `P:SkillsCatalog` |
| 被注入于 | `Skills` |
<!-- /generated -->

## 依赖

| 服务 | 用途 |
| --- | --- |
| `P:HostPlatform` | `home`：用户根 `~/.agents/skills` |
| `P:SkillSettingsHost` | 禁用集合的读取、单项更新与变化通知；产品中由 app-server 的设置 Plugin 提供 |

## 服务接口

`P:SkillsCatalog`

| 方法 | 语义 | 错误 |
| --- | --- | --- |
| `query(cwd) -> CatalogView` | 用户根与 `<cwd>/.agents/skills`（与用户根相同时不重复）的合成视图；根目录在首次使用时扫描，之后用快照；设置不可用时重读，失败不返回目录 | `SettingsRead`、`Closed` |
| `set_enabled(id, enabled, cwd) -> (entry, version)` | 按设置端口当前版本更新该项，成功后从设置端口重新读取禁用集合 | `UnknownSkill`、`SettingsRead`、`SettingsConflict`、`SettingsWrite`、`SettingsSync`（已写入但读回失败，下次 `query` 重读）、`Closed` |
| `refresh(RefreshTarget { user, project }) -> (version, issues)` | 重读设置并重扫指定的根；任一根不可读时保留全部旧根快照；修复设置文件后可恢复可用状态 | `SettingsRead`、`RootUnreadable`、`Closed` |
| `subscribe()` | 目录版本的 watch | `Closed` |

`CatalogView`：`version`、`entries`（定义、是否启用、`Active` 或 `Shadowed { by }`）、`issues`（清单无效、不可读、根不可读、达到扫描上限）。同名 Skill 先出现者生效，用户根在前。

调用方：[Skills](Skills.md) 注入并订阅；启停与重扫由协议层调用（`skills.set_enabled`、`skills.refresh`）。

## 扫描

- 每个根下查找 `SKILL.md`，最多 1,000 个文件、深度 10；解析 frontmatter 的 `name`、`description`、`allowed-tools`，正文在扫描时读入并替换路径变量。
- ID 格式：`user:<相对路径>` 或 `workspace:<skills 目录>:<相对路径>`。
- `allowed_tools` 只作描述，不授予任何权限。

## 状态

| 状态 | 位置 | 变化 |
| --- | --- | --- |
| 用户根快照、各项目根快照 | 内部 | 首次 `query` 时扫描；`refresh` 整根替换 |
| 设置快照或读取错误 | 内部 | 装载时读取；设置端口通知变化或 `set_enabled` 后从设置端口重新读取，读取串行，后读的结果不会被先读的覆盖；失败保留来源诊断，下一次 `query` 或 `refresh` 重读，不采用默认禁用集合 |
| 目录版本 | `watch`，进程内单调递增 | 禁用集合、可用状态或来源错误变化，以及 `refresh` 时加一 |

## 生命周期

- 装载：读一次设置并保存快照或错误，启动监听设置变化的任务，始终提供服务；可选设置读取失败不阻止进程和其他能力装载。不在装载时扫描目录。
- 卸载：取消监听任务并等待其结束；之后的调用返回 `Closed`。

## 不变式

- 目录只在首次使用或显式 `refresh` 时扫描，不监听文件系统。
- 一次 `refresh` 要么替换全部指定的根，要么都不替换。
- `SettingsRead` 保留设置端口的具体错误；查询、启停和刷新均明确失败，不覆盖损坏文件，不把来源错误解释为空目录。

## 为什么

- 放在进程级：Skill 目录与启停设置对所有 Session 相同，扫描一次即可。
- 不扫描、不监听：变化经显式刷新进入；避免后台扫描影响大目录。

## 代码与测试

- `crates/dim-agent-capabilities/src/skills/{catalog.rs, discovery.rs, mod.rs}`
- `crates/dim-agent-capabilities/src/skill_settings.rs`（端口）
- `tests/integration/tests/skills.rs`
