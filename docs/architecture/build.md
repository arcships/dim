# Build environment

Run commands at the workspace root. `rust-toolchain.toml` pins Rust 1.98.1, rustfmt and Clippy. Do not copy target directories between worktrees.

```sh
rustup show
unset RUSTC_WRAPPER
export CARGO_INCREMENTAL=0
cargo build --workspace --locked
```

A normal Cargo installation is sufficient; compiler caching is optional. Each worktree keeps its own target directory. Existing source caches can be reused without sharing build outputs.

The root `./dev` entry performs incremental CLI/App Server builds without disabling Cargo incremental compilation and uses a separate development home. Use `./dev --model-config PATH` for an API-key flow without an account plugin or Node; see the [development contract](development.md). Tests and full release packaging are explicit operations. Optional managed-provider packages own their source setup instructions.

Node is only needed for packaging, packaging checks and JS account-plugin development. `.node-version` pins the Node version bundled into account-enabled releases. The `runtime/package-lock.json` fixes the rutis Node runtime and its transitive dependencies; `npm ci --ignore-scripts --prefix runtime` installs them for local bridge testing. The private plugin bundles its SDK and requires no runtime dependency installation in its own directory.

CI uses self-hosted macOS ARM64 runners. Release builds use macOS ARM64 for both macOS targets, Linux x64 for both Linux targets, and Windows x64 for both Windows targets. Runners need rustup, a C/C++ toolchain, tar and Bash (Git Bash on Windows); Actions installs the pinned Node version. Linux ARM64 requires an AArch64 GCC/G++ cross toolchain. Windows ARM64 requires Visual Studio ARM64 libraries and LLVM/clang-cl; Windows x64 requires NASM or the crate's prebuilt NASM objects. Linux release assembly also needs GitHub CLI. See the [runner setup and release matrix](../release/README.md#runner-setup).

Each runner's OS/libc baseline must be recorded and verified before declaring that platform supported. Cross-built executables are checked for the target architecture during staging, but installation and runtime acceptance still require the corresponding target system.

Public Node provider and MCP interoperability fixtures use development dependencies declared in `runtime/package.json`. Install them with `npm ci --prefix runtime --ignore-scripts`; distribution staging uses `--omit=dev` and does not ship those SDKs.
