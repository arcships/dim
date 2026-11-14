# Build environment

Run commands at the workspace root. `rust-toolchain.toml` pins Rust 1.100.0, rustfmt and Clippy. Do not copy target directories between worktrees.

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

Full-workspace CI uses self-hosted macOS ARM64 runners; pull requests run only the check groups their paths need (see [GitHub workflows](../release/README.md#github-workflows)). The separate `windows-cli` job runs native boundary flows on a self-hosted Windows x64 runner, where its fixture writes to the clipboard. Release builds use GitHub-hosted VMs matching each target OS and CPU. See the [runner setup and release matrix](../release/README.md#runner-setup).

The macOS clipboard flows use private pasteboards and serialize access to the shared pasteboard server; other tests remain parallel.

The confined Apple Vision PNG recognition test is manual on macOS ARM64. Use it to reproduce or verify [#1446](https://github.com/arcships/dim/issues/1446).

```sh
cargo test --locked -p dimcode --test ocr_helper_flow an_image_is_recognized_under_the_confinement -- --ignored --exact
```

Each runner's OS/libc baseline must be recorded and verified before declaring that platform supported. Cross-built executables are checked for the target architecture during staging, but installation and runtime acceptance still require the corresponding target system.

Public Node provider and MCP interoperability fixtures use development dependencies declared in `runtime/package.json`. Install them with `npm ci --prefix runtime --ignore-scripts`; distribution staging uses `--omit=dev` and does not ship those SDKs.

Evaluation-tool tests are opt-in: `cargo test --locked -p xtask --features eval-tests`. CI enables them for `xtask/` changes, main pushes and manual runs. The real 30-second background handoff is also opt-in locally: `cargo test --locked -p dim-agent-integration-tests --test local_execution a_long_foreground_command_moves_to_the_background_after_30_seconds -- --ignored --exact`; CI runs it for execution-related paths, main pushes and manual runs. Ordinary workspace tests retain execution cancellation, cleanup and recovery flows.

`npm run build --prefix web` type-checks and bundles the frontend. Run its component and projection flows separately with `npm test --prefix web`.

## Bundled provider catalog

The App Server embeds the checked-in `crates/dim-app-server/resources/default-catalog.json`.
It also serves as the starting catalog for maintenance. Use the Node version in `.node-version`.

```sh
node scripts/update-provider-catalog.ts                       # Validate the checked-in catalog offline
node scripts/update-provider-catalog.ts --sync kimi-for-coding # Sync one provider from PublicProviderConf
node scripts/update-provider-catalog.ts --sync                # Sync all configured remote sources
node scripts/update-provider-catalog.ts --sync --full         # Sync and print every changed value
```

The bundled catalog owns provider settings such as authentication, adapter, and
`remoteModelList`. Upstream supplies model descriptors and the API base URL. Sync derives
the default model from the refreshed model list.

Two tables beside the catalog drive it. `provider-sync.json` states a policy for every
provider: `remote` for one refreshed from PublicProviderConf's published `dist/`, `local`
for one maintained here, with a one-line reason a reviewer reads, and a `local` provider
is never touched by a sync. A `remote` policy may keep the provider's base URL, default
model, or curated model list, name the upstream file when it differs from the provider id,
or declare its bundled rows authoritative. `model-overrides.json` holds field patches per
provider and model, re-applied after every sync. A patch merges key by key into the
upstream row, so a `limit` patch keeps the output cap upstream states, and a patch that no
longer matches a model fails the run instead of disappearing. Both tables are read by the
sync alone: the binary embeds `default-catalog.json` and `reasoning-off-overrides.json`
and reads neither table.

The offline validation checks the tables against the catalog: every provider has a policy,
no policy names a provider the catalog lacks, a `local` policy states a reason, every model
override names a provider whose policy is `remote`, every provider with models states
whether it is a `vendor` or an `aggregator`, and each provider's `metadata.source` follows
its policy. The bundled `k3.1-pro` context limit uses the 262,144 tokens available to Plus
subscribers, and its override states it. The same model ID supports a larger window on
higher tiers.
[Kimi Code model limits](https://www.kimi.com/code/docs/en/third-party-tools/claude-code.html)

Sync replaces the complete catalog after validation and prints a summary. The changes a
reviewer must judge come first: per provider, the models added and removed, and every
changed context window, output cap, status, capability or effort option with its old and
new value. A model whose override differs from upstream ends that line with
`(override: limit.context)`, naming the leaves the table decides rather than upstream.
Next comes every other changed leaf, in the provider entry and in each surviving model. A
path dim reads prints with both values. A path nothing reads that moved on more than three
models of one provider is one line naming the path and the count, and `--full` prints its
values when a reviewer needs them. Every changed path is therefore named, and a provider
counts as unchanged only when its whole entry is equal. `canonical_model_id` is not
upstream's to omit: a surviving model keeps the bundled value when upstream states none,
and an `=` line names it, since removing it from the bundled entry by hand is how one is
retired. Next comes the override report, each patched leaf with the value upstream states
beside it, so a divergence is visible without reading the diff, plus a line for an
override that repeats upstream's own value and can be retired. The local providers and the
reason each one is not synced come last. An up-to-date run prints the override report too,
since a redundant override never changes the catalog. Read that summary instead of the
diff. A sync must supply at least one usable upstream model for a provider whose bundled
catalog includes models outside its local policy. A sync that produces the same content
preserves the file and its timestamp. A fetch or validation failure leaves the catalog as
it was. Review and commit the complete catalog after a sync. Model entries occupy
individual lines for review. Upstream responses and intermediate model lists stay in
memory.

Before a release, run `node scripts/update-provider-catalog.ts --sync` once with the Node
version in `.node-version`, in a clean checkout of the latest `origin/main`, and read its
summary. No diff means the release continues. A diff is committed on its own branch with
the summary in the commit message, and it reaches `main` through a pull request the user
approves; a patch that no longer matches a model fails the run instead of being dropped, so
fix the override or the policy and run again. Never sync twice for one release, and
recovery or nightly repair skips the step because it rebuilds an existing SHA or tag. The
[release skill](../../.agents/skills/dim-release/SKILL.md#sync-the-bundled-catalog-before-each-release)
carries the steps.
