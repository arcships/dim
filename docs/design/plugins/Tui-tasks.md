# Tui 插件化：任务拆分

对应设计：[Tui.md](Tui.md)。每项可以单独完成、单独验收、单独提交；按编号顺序做，后面的依赖前面的。每项结束跑 `./dev check`。

## T1. dim-cli 引入 rutis

- [x] `crates/dim-cli/Cargo.toml` 加 `rutis.workspace = true`
- [x] `cargo build --locked -p dimcode` 通过
- [x] `cargo run --locked -p xtask -- check` 通过（确认 dim-cli → rutis 的 crate 依赖被允许；不允许就在 xtask 的限制里放行并说明）

验收：构建和边界检查都绿。不改任何 `.rs`。

## T2. DaemonLink 插件

- [x] 新建 `crates/dim-cli/src/plugins.rs`，`lib.rs` 里 `mod plugins`
- [x] `struct DaemonLink { options: Options, start: bool }`，实现 `rutis::TypedPlugin`，`Deps = ()`
- [x] `apply`：调用现有 `client::connect_inner(options, start)`；成功后 `ctx.provide(Arc<Client>)`；`start: false` 且没 socket 时提供 `None` 语义（给 `daemon stop` 用）
- [x] `ctx.effect`：断开连接
- [x] `client::connect_inner` 不改逻辑，只按需要放宽可见性

验收：单元级不需要新测试；T3 接上后由现有 CLI 流程测试覆盖。

## T3. 组合根建树，一次性命令走 Client

- [x] `run_cli` 里建 `rutis::Ctx::root()`，装 `DaemonLink`，等它 Active
- [x] `run()` 里 `exec / export / login / account / logout / models / mcp* / daemon stop` 从树里取 `Arc<Client>` 调 RPC，不再各自 `client::connect`
- [x] 结束时 `root.dispose()`，再返回退出码
- [x] `help / version / --fullscreen default` 不建树

验收：`crates/dimcode/tests` 里 exec、export、login、models、daemon stop 相关用例全部不改、全部通过。

## T4. Tui 插件

- [x] `struct Tui { env: ui::Env, options: Options, leave: Mutex<Option<oneshot::Sender<Leave>>> }`，`Deps = (Arc<Client>,)`
- [x] `apply` 第一件事：`ctx.effect` 登记恢复终端（`ui::terminal` 现有的 restore）
- [x] 然后：`SessionHost::start`（会话线程，复用现有实现，改为拿树里的 `Client` 而不是自己连）
- [x] 再：开 `dim-tui` 线程跑 `ui::tui::run`，结束时把 `Leave` 发回 oneshot
- [x] `ctx.effect` 登记关会话线程（drain + join）
- [x] `run_cli` 交互路径：装 `Tui`，等 oneshot，`Leave::Exit(code)` 时 dispose 并返回
- [x] 主线程不再跑 TUI 循环，只阻塞在 oneshot 上

验收：
- 现有交互启动、退出用例通过
- 新用例：让 `Terminal::new` 失败（例如 stdout 不是 tty 时走 Tui 路径），断言终端已恢复、错误已打印、退出码非零

## T5. WebHold 插件与 /web 开关

- [x] `struct WebHold { port, open_browser }`，`Deps = (Arc<Client>,)`；`dimcode web` 用它持有 entry
- [x] `apply`：调 `web.open`，打印面板，等 Ctrl+C / SIGTERM 的任务放在 `apply` 里 spawn，不阻塞 `apply`
- [x] TUI `/web` 只发 `web.toggle`，不卸载 `Tui`、不打印 handoff 面板
- [x] `dimcode web --off` 调 `web.disable` 后退出，不装 `WebHold`
- [x] 删除 `Leave::Web`/`WebHandoff` 和 `open_web` 里不再需要的部分

验收：`web` 相关用例通过；`/web` 后 TUI 仍在运行，开关关闭时 entry 按 holder/开关规则处理。

## T6. 断线不触发驱逐（设计 §4 的决定）

- [x] 确认 `DaemonLink` 在连接断开时不撤销 `Client` 服务；重连逻辑仍在会话线程（`session.attach` + 按序号补历史），和现在一样
- [x] 流程测试：启动 TUI 后让 daemon 退出，断言 Tui 没被卸载、终端仍在 raw mode、界面显示错误；用户退出后终端恢复

验收：该用例通过。

## T7. 文档收尾

- [x] `Tui.md` 状态 Draft → Implemented，附录 A 的行数估计改成实际
- [x] `docs/design/plugins/README.md` 索引行标注 implemented
- [ ] PR 描述按模板回答 Rule check 清单
- [ ] 如需要，`docs/zh/design/plugins/Tui.md` 中文镜像

## T8. 合并前验证

- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [x] `cargo test --workspace --locked`
- [x] `cargo run --locked -p xtask -- check`
- [x] 消融审查：`DaemonLink`、`Tui`、`WebHold` 各自拥有什么资源，写进 PR；能删的删

## 提交划分建议

| 提交 | 内容 |
| --- | --- |
| 1 | T1 |
| 2 | T2 + T3（DaemonLink 和组合根一起才可运行） |
| 3 | T4 |
| 4 | T5 |
| 5 | T6 |
| 6 | T7 + T8 |
