# Build environment

Run commands at the workspace root. `rust-toolchain.toml` pins Rust 1.98.1, rustfmt and Clippy. Do not copy target directories between worktrees.

```sh
rustup show
unset RUSTC_WRAPPER
export CARGO_INCREMENTAL=0
cargo build --workspace --locked
```

A normal Cargo installation is sufficient; compiler caching is optional. Each worktree keeps its own target directory. Existing source caches can be reused without sharing build outputs.

The macOS `verify` job allows four concurrent Cargo build jobs. It builds all workspace targets once, checks the architecture, then runs up to two recorded test executables at a time with up to three test threads per executable. The separate CLI profile and Node- and Python-backed Rust suites use two test threads each. The `dependencies` job allows two Cargo build jobs when installing `cargo-deny`.

CI sets `CARGO_TARGET_DIR` to the checkout's `target` directory so a compiler cache's managed-target collection cannot remove binaries between the build and their later execution. Each executable keeps its Cargo package working directory and streams output immediately, with target start/end markers and exit status. A target has a ten-minute deadline (`DIM_TEST_TIMEOUT_MS` overrides it), and timeout and cancellation terminate its process tree. Failures still allow the other targets to finish and fail the step. Cargo runs documentation tests separately with the same feature selection. Local `cargo test --workspace --locked` remains available without Node.

`xtask measure` records per-Session close samples immediately. After closing the Agent and its host, it allows up to 30 seconds for remaining Tokio tasks to settle; `cleanupSettleMs` reports that wait, and `tasksAfterAgentClose` and `allocationBytesAfterAgentClose` report the remaining tasks and Rust allocations. These final samples distinguish asynchronous disposal from retained state.

The root `./dev` entry performs incremental CLI/App Server builds without disabling Cargo incremental compilation and uses a separate development home. Use `./dev --model-config PATH` for an API-key flow; with the Dim account's client IDs set (`.env.dev`), `./dev` builds and runs the account build instead (see the [development contract](development.md)). Tests and full release packaging are explicit operations.

The [Web workspace](../design/web-workspace.md) frontend in `web/` is a separate artifact (`scripts/stage-web.mjs` builds it; the CLI finds the installed `dimcode-web` package or a `web/` directory beside the executable or in the product home, or `DIMCODE_WEB_ASSETS` names one), so building `dim-app-server` and `dimcode` needs neither Node nor npm. Only `cargo build --features embedded-web` compiles the frontend in; then `build.rs` runs `npm ci` and `npm run build` when that frontend changed, and a host without npm fails the build. Otherwise Node is only needed for packaging, packaging checks and TypeScript plugin execution/development. `.node-version` pins the Node version bundled into account-enabled releases. The `runtime/package-lock.json` fixes the rutis Node runtime and its transitive dependencies; `npm ci --ignore-scripts --prefix runtime` installs them for local bridge testing.

Full-workspace CI uses self-hosted macOS ARM64 runners; pull requests run only the check groups their paths need (see [GitHub workflows](../release/README.md#github-workflows)). The separate `windows-cli` job runs native boundary flows on a GitHub-hosted Windows x64 VM, where its fixture writes to the clipboard. Release builds use GitHub-hosted VMs matching each target OS and CPU. See the [runner setup and release matrix](../release/README.md#runner-setup).

The macOS clipboard flows use private pasteboards and serialize access to the shared pasteboard server; other tests remain parallel.

The confined Apple Vision PNG recognition test is manual on macOS ARM64. Use it to reproduce or verify [#1446](https://github.com/arcships/dim/issues/1446).

```sh
cargo test --locked -p dimcode --test ocr_helper_flow an_image_is_recognized_under_the_confinement -- --ignored --exact
```

Each runner's OS/libc baseline must be recorded and verified before declaring that platform supported. Cross-built executables are checked for the target architecture during staging, but installation and runtime acceptance still require the corresponding target system.

Public Node provider and MCP interoperability fixtures use development dependencies declared in `runtime/package.json`. Install them with `npm ci --prefix runtime --ignore-scripts`; distribution staging uses `--omit=dev` and does not ship those SDKs.

Evaluation-tool tests are opt-in: `cargo test --locked -p xtask --features eval-tests`. CI enables them for `xtask/` changes, main pushes and manual runs. The real 30-second background handoff is also opt-in locally: `cargo test --locked -p dim-agent-integration-tests --test local_execution a_long_foreground_command_moves_to_the_background_after_30_seconds -- --ignored --exact`; CI runs it for execution-related paths, main pushes and manual runs. Ordinary workspace tests retain execution cancellation, cleanup and recovery flows.

The full-screen permission acceptance flow runs manually while [#1454](https://github.com/arcships/dim/issues/1454) tracks its intermittent stall. Run `cargo test --locked -p dimcode --test cli_flow tui_permissions_keep_pending_file_and_member_questions_and_save_command_rules -- --ignored --exact`.

`npm run build --prefix web` type-checks and bundles the frontend. Run its component and projection flows separately with `npm test --prefix web`.
