# Build environment

Run commands at the workspace root. `rust-toolchain.toml` pins Rust 1.100.0, rustfmt and Clippy. Do not copy target directories between worktrees.

```sh
rustup show
unset RUSTC_WRAPPER
export CARGO_INCREMENTAL=0
cargo build --workspace --locked
```

A normal Cargo installation is sufficient; compiler caching is optional. Each worktree keeps its own target directory. Existing source caches can be reused without sharing build outputs.

The release profile uses full LTO (`lto = "fat"`) with one codegen unit to reduce binary size. Hosted release builds use two Cargo jobs. The macOS ARM64 and Linux ARM64 release jobs override LTO to `thin` on their 7 GB and 16 GB runners; full LTO requires a successful release-profile build and runtime smoke test within that runner's memory budget before enabling it there. The other release targets retain full LTO. [The resource investigation](https://github.com/arcships/dim/issues/1674) tracks measurements and build costs. Code residency depends on executable layout and file-cache mapping conditions as well as code size. Normal PR tests use the dev profile, so release builds and runtime smoke tests on each supported platform must validate this setting.

## Native executable layout

The opt-in layout build groups functions whose pages are observed during startup,
shutdown, the TUI model picker and a local tool conversation, aiming to reduce unrelated
code pages mapped alongside them. It does not change business logic, introduce
a runtime cache or change the allocator. Each platform is validated on native
hardware and rejects cross compilation:

| Host | Target | Page size | Release LTO |
| --- | --- | --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | 4 KiB | fat |
| Linux arm64 | `aarch64-unknown-linux-gnu` | 4 KiB | thin on the release runner |
| macOS arm64 | `aarch64-apple-darwin` | 16 KiB | thin on the release runner |
| macOS x86-64 | `x86_64-apple-darwin` | 4 KiB | fat |
| Windows x86-64 | `x86_64-pc-windows-msvc` | 4 KiB | fat |
| Windows arm64 | `aarch64-pc-windows-msvc` | 4 KiB | fat |

The command builds `dimcode` with the repository's release profile and
preserves the build environment, including account build settings and the
`CARGO_PROFILE_RELEASE_LTO` selection used by the release workflow. Every
platform uses the same compiler selection: `RUSTC`, then `CARGO_BUILD_RUSTC`,
then `rustc` from PATH; Cargo configuration's `build.rustc` is superseded. Use a new output directory for the initial build. A failed build never produces its final executable.
The release workflow packages the ordered executable on Linux x64 and the regular Cargo executable on other targets.

```sh
python3 scripts/code-layout/build.py --output /tmp/dim-layout-build --jobs 2
/tmp/dim-layout-build/dimcode --version
```

On macOS and Windows the command retains the verified baseline, its map and
captured linker inputs under the output directory. After a training, ordering
or acceptance failure, retry on the same host and checkout with
`--output DIR --resume`. Preserve the original paths, SDK and toolchain.
The retry checks the target and baseline hashes and trains the original build,
even if the layout scripts have since changed. Use a fresh directory for a new
product build. Remove the output directory when finished to reclaim the captured
object files. Run `--output DIR --measure-only --pairs 2` to compare the
validated executables without compiling or linking. The command checks their
hashes before replacing the manifest with the measured version.

Training uses disposable homes and workspaces and a loopback model fixture. It
runs the real App Server ready/shutdown path, opens and closes the TUI model
picker three times and executes the real `read` tool. It does not use the
product home, credentials or remote providers. Fingerprints are regenerated for
every build; no order file, profile or runtime cache is committed.

### Linux

Python 3.9 or later, `cc`, the pinned Rust toolchain's LLD and readable
`/proc/<child>/smaps` / writable `clear_refs` are required. The tool selects
that LLD explicitly for Cargo's original executable link; every replay retains
the captured linker selection. A missing toolchain linker fails before the
product build. It selects `RUSTC`, then `CARGO_BUILD_RUSTC`, then `rustc` from
PATH, and passes that selection to Cargo. Use one of those environment variables
to override the compiler for this command; Cargo configuration's `build.rustc`
is superseded. The compiler supplies both the LLD sysroot and the recorded
version. The tool builds `dimcode` with the repository's release profile and
preserves the build environment, including account build settings. An optional
`--target x86_64-unknown-linux-gnu` selects Cargo's explicit native target path.
Other targets and cross compilation are not supported by this command.

The build captures the final native linker inputs, reproduces Cargo's binary
byte for byte, and trains only that binary. A temporary preload probe prefaults
its executable pages, separates their mappings, clears reference bits and resets
code-page permissions to invalidate cached translations before `main`, and reads
the App Server's referenced pages after normal shutdown. The TUI opens and
closes its model picker three times, is paused with `SIGSTOP` for the sample,
then resumes before its explicit exit and must exit successfully.

Whole functions intersecting referenced pages are grouped by shared usage:
startup/shared functions first, then TUI, then conversation-only functions;
original address order is retained within each group. Functions not selected
retain the linker's default ordering. A diagnostic link identifies names LLD
resolves to shared or discarded definitions. Duplicate local names are not
independently detected.
Only reported names are removed from the order; a strict relink runs when any
were removed. Any other diagnostic fails the build. This is page-access
profiling, including neighboring functions on each page, rather than an
instruction trace or a call-count profile.

The same native objects are relinked with LLD's `--symbol-ordering-file`. The
function-name/size multiset must remain equal. Selected function addresses must
follow the final order, treating symbols at the same address as aliases, and at
least one selected address must move. An unchanged or unordered candidate fails
before publication; movement alone does not establish an RSS or CPU gain.

LLD ranks an *input section*, not a symbol: it gives a section the best rank of
the symbols that name it and keeps the section's own order inside it
([`sortISDBySectionOrder`](https://github.com/llvm/llvm-project/blob/llvmorg-21.1.8/lld/ELF/Writer.cpp#L1158-L1220)).
A relink therefore cannot place two functions independently when one input
section defines both, and an order file that splits them is a request the link
cannot satisfy. Because rustc emits one input section per function, this held
for Rust code but not for native assembly and C objects that place several
functions in one `.text` section, which is why the release run failed only on a
full product link. The order file is built from the baseline's
`--Map`/`--no-demangle` map: functions sharing a `.text` chunk are emitted
contiguously in address order at the position of the chunk's best-ranked
member, and a name the map attributes to more than one chunk is left out. Both
flags leave the linked bytes unchanged, so the baseline hash check still applies.
`ordered_link` returns the names the candidate was actually linked with after
unplaceable ones were removed, and acceptance ranks addresses against that list.

Linux x64 releases build and validate the ordered executable in the pinned manylinux 2.28 container. The candidate
then runs all three flows without the probe, plus CLI help/version checks. Only
after they pass does the command produce `dimcode`.

### macOS

macOS requires the Xcode command-line tools, Python 3.9 or later and a native
arm64 or x86-64 host. `--target` may name the host architecture explicitly; a
different architecture is rejected. The layout build uses the linker selected
by `cc` (ld64) and does not change code signing or hardening of the Cargo
output. The optimized candidate is an unsigned opt-in artifact, not a release
artifact.

The selected Rust sysroot supplies Cargo, rustc and `rust-objcopy`; its shared
library directory is carried through the capture wrappers and replay.
The build sets [`RUSTUP_TOOLCHAIN`](https://rust-lang.github.io/rustup/environment-variables.html)
to that sysroot so a compiler proxy keeps the
same toolchain when Cargo compiles dependencies outside the workspace.
Explicit compiler and compiler-wrapper selections are preserved. The
command checks that `rust-objcopy --version` runs before starting Cargo. This
tool ships with the pinned Rust compiler; a missing or broken tool fails the
build instead of silently skipping stripping.

The capture wrapper archives inputs and forwards Cargo's original arguments and
input paths to the compiler, adding the ld64 map argument. It does not call the
replay implementation to produce this first executable. A temporary compiler
wrapper records the effective `-Cstrip` option and preserves any existing
`RUSTC_WRAPPER`.
Replays apply the same selected toolchain's `rust-objcopy` step that Rust runs
after linking on macOS. The baseline must reproduce Cargo's executable byte for
byte before any training, then restores Cargo's original file. The native
fixture builds a Rust release executable with LTO and debug stripping through
this same capture/replay path. ld64's map records exact atom sizes, so
function-name/size parity is checked directly. Cargo passes the selected Rust deployment target to native dependencies.
SDK and deployment-target environment values are captured for replay along with
the tool loader paths.

The training probe is a temporary dylib loaded with `DYLD_INSERT_LIBRARIES`.
Its constructor locates the page-aligned `__TEXT,__text` range and removes
the injection variables so child processes are unaffected. It
reads `mincore` residency at normal exit and, for the TUI, on `SIGUSR1` before
the explicit exit. This measures backing-page residency, including warm file
cache pages from linking or previous runs; it does not identify pages accessed
only by this process. The page size is 4 KiB on x86-64 and 16 KiB on arm64.
[XNU's mincore implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_mman.c)
queries VM page dispositions, so replacing a mapping would not evict those
backing pages. No mapping replacement is used. These samples guide an
experimental order; process RSS/CPU improvements still require native measurement.
The manifest identifies this metric as `codeResidencySource: mincore-backing-pages`;
its `codePages`/`codeResident` fields describe backing pages, separately from
process `resident` and `physicalFootprint`.

Functions whose pages were observed are ordered with ld64's `-order_file`.
`-order_file_statistics` reports entries the linker cannot match; only those
names and names explicitly reported as ambiguous between objects are removed.
The final link checks diagnostics, allowing the exact duplicate `-lSystem`
warning once if the baseline replay emitted it. Other warnings fail the check. The candidate must keep the same atom name/size
multiset, the same function-start count and the selected addresses in the final
order, and at least one selected address must move. Reordering can move
alignment padding between functions, so the total `__text` size is recorded but
is not treated as an invariant.

The release workflow uses thin LTO for `darwin-arm64` on its 7 GB runner and fat
LTO for `darwin-x64`. The layout command preserves that environment selection;
it does not override the release LTO policy.

### Windows

Windows requires Python 3.9 or later, a native x86-64 or ARM64 MSVC Rust toolchain, the MSVC build
tools, and either LLVM's `lld-link.exe` or the Rust toolchain's `rust-lld.exe`.
Native dependency builds also use NASM on x64 and `clang-cl` on ARM64, matching
the [release build environment](../release/README.md#runner-setup).
The command uses that LLD linker for Cargo's original executable and every replay.
Normal release builds still use the MSVC `link.exe`; the optimized candidate is
an opt-in artifact linked by `lld-link`, and the release workflow is unchanged.
`--target` may name the host architecture explicitly; a different architecture
is rejected.

The captured link line may contain response files. The wrapper archives input
object/library files named by existing paths and `/NATVIS:` files, and saves the
expanded arguments for later replay. The first link directly forwards the
original arguments, including response-file references and original input paths,
to LLD; it does not use the saved argument vector or replay implementation.
The capture adds `/Brepro` when absent, `/demangle:no` and the `/lldmap` argument
to both the first link and the saved record. Input paths
and [linker arguments](https://github.com/llvm/llvm-project/blob/main/lld/COFF/PDB.cpp)
contribute to PDB identity even when release Rust code has no debug information,
so adding the map only during replay would change the PE
metadata. The baseline relink uses the same output, PDB and map paths and must
reproduce the executable byte for byte; then Cargo's executable and PDB are
restored. The map is copied to the artifact directory after the original link.
The native fixture checks this replay at the captured output path with CodeView
debug information and a PDB, after removing the original object and NatVis file
to verify that the saved inputs suffice. It also captures and replays a real
Rust LTO executable after the compiler has removed its temporary NatVis files.
It retains both fixture executables, their PDBs and
the captured arguments on failure; Windows short-path aliases must not replace
the recorded path in the replay command. Bare/default SDK library names remain
linker-resolved with captured `LIB`/`INCLUDE`; the same native SDK and toolchain
must remain installed. The capture is not a portable archive of the SDK.

On either platform, a baseline byte mismatch retains both executable files and
the captured link record in the CI artifact; Windows also retains the PDB pair.
The hash check remains mandatory before training or ordering.
Before replay, saved inputs that Rust has removed are temporarily restored at
their original paths; only files and directories created by that replay are
removed afterwards. Existing inputs must match their captured hashes; replay
fails instead of overwriting another build's files. This preserves path-dependent
metadata while letting the first link remain independent of input capture and
argument serialization. The completed first executable, including Rust's
post-link work, is backed up before replay at the same canonical output path.
Its bytes must match the replay before training, including when the saved input
record was produced incorrectly. The artifact fixture injects a deterministic
capture error to verify that the original executable survives and validation fails.
The check covers reproduction of this opt-in link, not equivalence to a release
built without the wrapper or proof of function semantics; native flow acceptance
and symbol/size checks cover those separate boundaries.
During baseline replay, handled interrupts (`SIGINT`/`SIGTERM`, plus Windows
`SIGBREAK`) stop and reap the active linker or stripping tool before restoring
Cargo's files. A PDB created only by replay is removed. Forced process termination
(`SIGKILL` or Windows `TerminateProcess`) does not run this cleanup.

The trainer launches the App Server suspended and empties its working set at
entry before resuming. The ConPTY-backed TUI is created running and trimmed
immediately afterwards, because the hosted console path does not combine a
suspended start with pseudoconsole attachment; the startup profile covers the
entry-time pages and the TUI profile covers pages faulted by the UI flow. Before
exit or shutdown the trainer queries the executable's `.text` working-set pages
with `QueryWorkingSetEx`. App Server shutdown sampling and the exit wait share a
60-second deadline; expiry fails training and its owned process is terminated.
A pre-shutdown sample is retained if shutdown finishes before another poll.
Every Windows target runs the real TUI picker; a
console-less runner is not accepted as native TUI evidence. The trainer
frames command text as bracketed paste and leaves Enter outside the paste,
preserving submission when the input reader coalesces fast key bursts.
The ConPTY launcher uses explicit null standard handles with
`STARTF_USESTDHANDLES` so a redirected parent does not pass its pipes to the child
([Windows console guidance](https://github.com/microsoft/terminal/discussions/15814)).
The fixture checks that all three standard handles are console handles, captures
both output streams through ConPTY, and keeps the child alive until it receives
the input handshake. It uses the same explicit environment path as TUI training;
initialization bytes or an early exit are not counted as success. This is genuine working-set
residency after an entry-time trim, not a reference-bit trace: the operating
system may prefetch neighboring pages and the result includes page-granularity
rounding. App Server shutdown
uses the public `daemon.shutdown` RPC with `force`; the TUI exits through
`/exit`. Both must exit with status 0.

The order file is passed to `lld-link` as `/order:@file`. The diagnostic link
reports unmatched names as `missing symbol ... [LNK4037]`; only those names are
removed, then `/WX` makes the strict relink fail on any remaining warning. The
`/lldmap` output uses `/demangle:no` so its raw COFF names also match `/order`.
Its output-section, input-chunk and symbol columns are parsed structurally.
COFF input subsections such as `.text`, `.text$mn` and `.text$unlikely` retain
their lexicographic placement. The order file groups names by that input
subsection, preserving the training priority inside each group, because
[`lld-link` sorts each partial section independently](https://github.com/llvm/llvm-project/blob/llvmorg-21.1.8/lld/COFF/Writer.cpp#L1030-L1041).
It records all `.text` symbols, including static ones. Only
symbols inside their input chunk's half-open address range describe code;
end labels are excluded even when another function follows that chunk, and
extents never include padding beyond their own chunk. Parity
always checks the remaining symbol-name multiset. It compares the COMDAT body size for
every name whose entries are unambiguous on both sides; a name with a chunk
holding several symbols (for example an alias or folded body) on either side is
compared by name and count only, and the skipped entries are counted in
`manifest.json` as ambiguous. Selected addresses must follow the final order
and at least one must move. Aliases at the same address share an ordering position.

Both current Windows release targets use fat LTO. The layout command preserves
`CARGO_PROFILE_RELEASE_LTO` and never enables thin LTO implicitly.

### Measurements and artifacts

With `--measure-only`, the macOS and Windows commands alternate baseline and candidate
samples from fresh disposable homes under the same platform sampling mechanism.
It records wall time to App Server readiness, startup CPU, total resident
memory, code-page residency and the wall time of a local-fixture tool
conversation. On macOS, code pages come from the training probe's `mincore`
sample at ready plus 100 ms, total resident memory from `ps`, and physical
footprint from `vmmap` when available; on Windows they come from
`QueryWorkingSetEx` and `GetProcessMemoryInfo`, with working set, private usage
and pagefile usage kept distinct. These are OS-specific metrics and are never summed across platforms
or treated as interchangeable with Linux `/proc` numbers.

`baseline`, `symbols.order`, the linker maps, native page samples and
`manifest.json` record the comparison inputs, platform, target, page size, LTO
choice, source state, Rust version, artifact hashes, training counts and
alternating samples after the optional measurement on macOS/Windows. The Linux manifest records
artifact/layout validation without these alternating measurements. Linker maps and native fixture maps are
uploaded on failure, and a size mismatch reports bounded before-only and
after-only symbol/size entries. The probe
and temporary homes are removed on completion or failure. Scheduling can change
which neighboring pages a training run observes, so the optimized artifact is
not promised to be byte-identical across training runs; each build regenerates
its own order.

On macOS and Windows, each manifest `inputs` entry records absolute `file`
(archived copy) and `source` (original linker input) paths on the originating
build host, plus the archived copy's `sha256`.

## Development and CI

CI and `./dev check` run the format check as `cargo fmt --all -- --check --config error_on_line_overflow=true`: when one line in a block cannot fit `max_width`, rustfmt abandons the block without a diagnostic, so the option turns that otherwise-skipped line into a failure ([#1769](https://github.com/arcships/dim/issues/1769)).

The macOS `verify` job has a seventy-five-minute overall deadline and allows four concurrent Cargo build jobs. It builds all workspace targets once, checks the architecture, then runs up to three recorded test executables at a time with up to three test threads per executable. The workspace run starts `transport_flow` and `cli_flow` first. The separate CLI profile and Node- and Python-backed Rust suites use two test threads each. The `dependencies` job allows two Cargo build jobs when installing `cargo-deny`.

CI sets `CARGO_TARGET_DIR` to the checkout's `target` directory so a compiler cache's managed-target collection cannot remove binaries between the build and their later execution. Each executable keeps its Cargo package working directory and streams output immediately, with target start/end markers and exit status. A target has a ten-minute default deadline (`DIM_TEST_TIMEOUT_MS` overrides it); the full workspace step sets fifteen minutes per executable for the large flow suites at three test threads. Timeout and cancellation terminate its process tree. Failures still allow the other targets to finish and fail the step. Cargo runs documentation tests separately with the same feature selection. Local `cargo test --workspace --locked` remains available without Node.

Terminal-interaction end-to-end tests for the native TUI run locally through tmux, not in CI: `node scripts/tui-tmux/run.mjs` after building `dimcode` and, for the provider scenario's `--server` profile, `dim-app-server`. A TUI case keeps running in CI until a tmux scenario proves the same behavior; [Terminal end-to-end suite](tui-e2e.md) records which cases moved and which still wait. CI keeps the component render and keyboard tests, the protocol contract tests and the line-mode CLI flows.

`xtask measure` records per-Session close samples immediately. After closing the Agent and its host, it allows up to 30 seconds for remaining Tokio tasks to settle; `cleanupSettleMs` reports that wait, and `tasksAfterAgentClose` and `allocationBytesAfterAgentClose` report the remaining tasks and Rust allocations. These final samples distinguish asynchronous disposal from retained state.

The root `./dev` entry performs incremental CLI/App Server builds without disabling Cargo incremental compilation and uses a separate development home. Use `./dev --model-config PATH` for an API-key flow; with the Dim account's client IDs set (`.env.dev`), `./dev` builds and runs the account build instead (see the [development contract](development.md)). Tests and full release packaging are explicit operations.

The [Web workspace](../design/web-workspace.md) frontend in `web/` is a separate artifact (`scripts/stage-web.mjs` builds it; the CLI finds the installed `dimcode-web` package or a `web/` directory beside the executable or in the product home, or `DIMCODE_WEB_ASSETS` names one), so building `dim-app-server` and `dimcode` needs neither Node nor npm. Only `cargo build --features embedded-web` compiles the frontend in; then `build.rs` runs `npm ci` and `npm run build` when that frontend changed, and a host without npm fails the build. Otherwise Node is only needed for packaging, packaging checks and TypeScript plugin execution/development. `.node-version` pins the Node version bundled into account-enabled releases. The `runtime/package-lock.json` fixes the rutis Node runtime and its transitive dependencies; `npm ci --ignore-scripts --prefix runtime` installs them for local bridge testing.

Full-workspace CI uses self-hosted macOS ARM64 runners; pull requests run only the check groups their paths need (see [GitHub workflows](../release/README.md#github-workflows)). The separate `windows-cli` job runs native boundary flows on a self-hosted Windows x64 runner, where its fixture writes to the clipboard. The layout workflow uses hosted `windows-2022` and `windows-11-arm` runners, with Python 3.13 and the pinned native MSVC Rust host toolchain provisioned explicitly. Release builds use GitHub-hosted VMs matching each target OS and CPU. See the [runner setup and release matrix](../release/README.md#runner-setup).

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

The App Server build compiles `crates/dim-app-server/resources/default-catalog.json`
into one embedded offset image: provider headers/fingerprints, original model order,
sorted matching/target/naming keys and independently compressed model payloads.
Integer offsets avoid a pointer relocation per record. No runtime catalog index or
decoded catalog is retained. Packaging remains a single executable on every target;
no new release sidecar or linker section policy is required. Native source archives
use the [v52 image contract](../design/appserver/native-storage-format.md#v52-sparse-provider-state).
Runtime reads decode only selected payloads and necessary match candidates.
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
sync alone: the binary embeds the compiled catalog and `reasoning-off-overrides.json`
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
summary. No diff means the release PR carries only its version commit. A diff is committed on its own branch with
the summary in the commit message, as the release PR's first commit, and it reaches `main` through that pull request the user
approves; a patch that no longer matches a model fails the run instead of being dropped, so
fix the override or the policy and run again. Never sync twice for one release, and
recovery or nightly repair skips the step because it rebuilds an existing SHA or tag. The
[release skill](../../.agents/skills/dim-release/SKILL.md#prepare-the-release-pr)
carries the steps.
