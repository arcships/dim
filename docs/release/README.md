# Release packaging

Dim uses self-hosted runners for PR checks and the complete GitHub release workflow. npm publication stays disabled in `.github/workflows/npm-publish.yml.disabled`. The [project release skill](../../.agents/skills/dim-release/SKILL.md) covers version preparation, PRs, tags, draft inspection and recovery. [Rust alpha release](alpha.html) walks through publishing a `-alpha.N` version end to end (Chinese).

The public source uses MIT. An account-enabled distribution can contain a separately supplied private runtime plugin; `NOTICE` excludes that artifact from the public source license. This project does not select or publish a license for that private plugin.

## Upgrade compatibility

Before replacing an older CLI and daemon, stop that daemon with its matching CLI. The replacement `dimcode daemon stop` refuses daemons without `daemon.shutdown.completed` and leaves them running. Forced shutdown does not certify storage or plugin cleanup.

Account-enabled distributions use an executable built with the private Rust provider through `ManagedProviderRow`. The public executable supports API-key providers and does not load the former TypeScript account package. Existing launchers must remove the old `dimcode --plugin-dir`, `--node` and `--js-runtime` arguments and use the account build when managed login is required; those arguments are rejected rather than falling back to a package. Embedding applications replace `ManagedPluginConfig` with the [typed provider hook](../../crates/dim-app-server/README.md#managed-provider-plugins).

The public host does not migrate or rewrite the account-owned `auth.json` when replacing the executable. Keep the same explicit home for the replacement account build; its provider owns reading the account format. The API-key-only executable leaves that account data in place but cannot supply its managed login or credentials. Existing Session TypeScript rows retain their separate App Server Node/runtime configuration; removing the embedded account runtime does not install a runtime for those rows.

Release cloning supplies its Git authentication header through process-local Git environment configuration, with shell tracing disabled. The credential is absent from Git arguments and is unset before checkout and build commands. Release jobs continue to use self-hosted runners isolated from PR jobs.

## Runner setup

Workflows use GitHub's [default self-hosted OS and architecture labels](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/use-in-a-workflow). Register runners with these labels and grant this repository access to their runner group:

| Job or target | Rust target | Runner labels | Build mode |
| --- | --- | --- | --- |
| PR checks | Host default | `self-hosted`, `macOS`, `ARM64` | Native workspace build and tests |
| PR Windows CLI checks | Host default | `self-hosted`, `windows`, `x64` | Native CLI tests |
| macOS ARM64 | `aarch64-apple-darwin` | `self-hosted`, `macOS`, `ARM64` | Native |
| macOS Intel | `x86_64-apple-darwin` | `self-hosted`, `macOS`, `ARM64` | Cross compile with Apple Clang |
| Linux x64 | `x86_64-unknown-linux-gnu` | `self-hosted`, `linux`, `x64` | Native |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `self-hosted`, `linux`, `x64` | Cross compile with AArch64 GCC/G++ |
| Windows x64 | `x86_64-pc-windows-msvc` | `self-hosted`, `windows`, `x64` | Native MSVC |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `self-hosted`, `windows`, `x64` | MSVC ARM64 libraries and clang-cl |
| Release preflight and draft assembly | None | `self-hosted`, `linux`, `x64` | Version/provenance checks and GitHub upload |

All runners need a current GitHub Actions runner, Git, rustup, a C/C++ toolchain, tar and Bash. Actions installs Node from `.node-version`; Rust is pinned by `rust-toolchain.toml`, and release jobs install their Rust target. PR runners must be isolated from release credentials and private networks. Apply GitHub's approval policy before running untrusted PRs on self-hosted machines.

- macOS: install Xcode Command Line Tools and accept the Xcode license. Apple Clang and its SDK build both architectures.
- Linux: provision a GNU/Linux baseline suitable for the intended users. On Debian/Ubuntu install `build-essential gcc-aarch64-linux-gnu g++-aarch64-linux-gnu`. The ARM64 job selects that cross compiler, archiver and linker; it does not require emulation. Install `gh` for release assembly.
- Windows: install Git for Windows (Bash and tar), PowerShell 7, Visual Studio Build Tools with the x64 and ARM64 MSVC tools and Windows SDK, LLVM/clang-cl, and NASM for x64 assembly. Put `clang-cl` and `nasm` on the runner's PATH. The workflow configures `amd64` or `amd64_arm64` and selects the target MSVC linker by full path to avoid Git Bash's unrelated `link.exe`. [Windows native dependency requirements](https://aws.github.io/aws-lc-rs/requirements/windows.html).

Cross-built executables are inspected for their ELF, Mach-O or PE target architecture. Staging runs `dimcode --version` on native builds. Cross-target version execution and target-system runtime acceptance require the corresponding system; a successful cross build alone is not that acceptance. Record and verify OS/libc baselines before declaring installation support.

## GitHub workflows

`ci.yml` runs on pull requests, pushes to `main`, and manual dispatch. It builds the workspace, checks formatting and Clippy, runs workspace tests and boundary checks, runs the packaging/development script tests, and exercises the ignored actual CLI profile and public Node-backed flows. It receives no private plugin credentials. Pushes to `main` and manual runs do all of it; a pull request runs only the groups its changed paths need, as decided by `scripts/ci-changes.mjs` (documentation-only changes skip every group, and unrecognized paths run all of them). `ci-ok` passes when every selected job passed, so it is the check to require. CI builds without debug info (`CARGO_PROFILE_DEV_DEBUG=0`).

### Rust dependency checks

The `dependencies` job installs the fixed `cargo-deny` version declared in
[`ci.yml`](../../.github/workflows/ci.yml) and runs:

```sh
cargo deny --locked check advisories licenses
```

Install that same tool version to run the check locally. The job runs for Rust
check groups and dependency-policy changes; `deny.toml` or root `LICENSE` edits
alone do not trigger workspace compilation. Pushes to `main` and manual CI runs
also scan. Its failure or cancellation fails the existing `ci-ok` check.

[`deny.toml`](../../deny.toml) owns the policy. It enables all workspace features
and includes development dependencies, unpublished workspace members and all
target-specific dependencies. Known vulnerability advisories fail. Unsoundness
advisories also fail, except for the two reviewed, unreachable APIs below.
Maintenance-notice exceptions identify specific advisory IDs, with their
existing dependency owners and reasons beside each entry. An unused advisory
exception fails so it must be removed when the dependency disappears. License
expressions must satisfy the explicit allow list or a crate-specific exception. The
certificate datasets have their own narrowly scoped data-license exceptions.
Crates whose licenses cannot be identified or do not satisfy that policy fail;
manual clarifications require evidence from the actual license files.
[cargo-deny policy reference](https://embarkstudios.github.io/cargo-deny/checks/index.html).

`vendor/aimux` is checked as ordinary path dependencies, together with its
transitive dependencies. Its packages inherit MIT from the vendored workspace
and retain `vendor/aimux/LICENSE`; there is no vendor exclusion or blanket
advisory exception. Vendored terminal packages are included by the same rule.

The terminal stack's Ratatui 0.29 is the sole parent of `lru` 0.12.5. Its private
layout cache only calls `new`, `resize` and `get_or_insert`; those paths do not
use `IterMut` ([RUSTSEC-2026-0002](https://rustsec.org/advisories/RUSTSEC-2026-0002/))
or `pop` ([RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253/)).
The `(Rect, Layout)` keys also have no potentially panicking destructor required
by the latter advisory. These two specific exceptions depend on that call-path
review; recheck them whenever the terminal dependencies change. They do not
certify `lru` 0.12.5 for arbitrary use or suppress other advisories for it.

Duplicate major versions are accepted for now (#296). Aimux and the App Server/MCP
stack use different upstream dependency APIs, including the parallel `reqwest`,
`sha2` and `base64` chains. Unification belongs with compatible upstream upgrades,
not a forced major-version substitution in the scan gate.
`cargo tree --locked --duplicates` remains the diagnostic command; duplicate versions do not bypass
advisory or license checks. The job intentionally runs only `advisories` and
`licenses`, without a second `cargo-audit` run or a duplicate-version ban.

This gate checks the public Rust dependency graph. Node/Python dependencies,
separately supplied private plugins, non-Cargo assets and distribution notices
retain their own checks and ownership.

### CI trust model

Every `ci.yml` job runs on a persistent self-hosted runner, and a pull request's checks build and run that pull request's code, so a job can read and change whatever the runner keeps between jobs. CI therefore verifies only code pushed by people with write access to `arcships/dim`:

- The binding boundary is the repository's GitHub settings: forking is disabled, and "Run workflows from fork pull requests" is off for this private repository. Keep both that way while CI uses self-hosted runners.
- `ci.yml` also skips every job when a pull request's head branch is not in `arcships/dim`, and `ci-ok` then fails rather than reporting an unverified pull request as passing. This guard is defense in depth only: a pull request supplies its own workflow file, so the settings above are what keep untrusted code off the runners.
- PR runners must hold no release credentials and stay off private networks; `ci.yml` receives no secrets, and its `GITHUB_TOKEN` can only read repository contents.

Accepting contributions from forks or outside collaborators requires moving their checks to ephemeral runners first.

`release.yml` runs when a `v*.*.*` tag is pushed, or when manually dispatched with an existing tag:

```sh
gh workflow run release.yml --ref main -f tag=vVERSION
```

Preflight checks the exact tag commit is reachable from `origin/main`, and verifies the tag, Cargo workspace, npm wrapper/optional dependencies and runtime metadata have the same version. Every build checks out the resolved commit SHA. Six target jobs stage and pack their payloads. Assembly requires every target, matching toolchain/plugin provenance and valid artifact digests before creating a draft GitHub Release. Prerelease tags mark the draft as a prerelease. Release notes are generated from GitHub history and should be reviewed before publishing.

The draft receives seven native archives (six platforms and the Web workspace), nine original npm tarballs (six platform packages, `dimcode-web`, `dimcode-plugins` and the wrapper), `release-manifest.json` and `SHA256SUMS`: eighteen attachments in total. `SHA256SUMS` covers the native archives; the manifest records digests for both native and npm archives. npm tarballs are not published to a registry by this workflow. Existing releases, including drafts, are rejected to avoid replacing attachments. If an upload leaves an incomplete draft, inspect it and remove that draft only when authorized before retrying; keep its tag.

All Actions artifacts, including the disabled npm publication receipt, use `retention-days: 1`. GitHub Release attachments persist independently of Actions retention. A retry after artifacts expire must rebuild all targets.

## Local packaging

One product version is shared by `Cargo.toml`, workspace versions in `Cargo.lock`, `npm/package.json`, its exact optional dependencies, runtime package/lock metadata, native version output and the release tag. Keep them synchronized before building. `0.1.0` is a development baseline; check existing npm versions before enabling registry publication.

```sh
cargo build --release --locked -p dimcode
node scripts/stage-release.mjs target/release staging/local 0.1.0 FULL_SOURCE_COMMIT
node scripts/pack-release.mjs staging/local release/local
node scripts/stage-web.mjs release/web 0.1.0 FULL_SOURCE_COMMIT
node scripts/stage-plugins.mjs release/plugins 0.1.0 FULL_SOURCE_COMMIT
```

The executable carries no frontend. `stage-web.mjs` builds `web/` once (`npm ci`, `npm run build`), records `web/dimcode-web.json` (`version`, `protocolVersion`, `sourceCommit`) beside it and packs the tree as `dimcode-web-VERSION.tar.gz` and the npm package `dimcode-web`, which the wrapper lists as an optional dependency and names to the executable through `DIMCODE_WEB_ASSETS`. A native-archive user unpacks `dimcode-web-VERSION.tar.gz` so that `web/index.html` sits beside the executable's `bin/` or in `<product home>/web`. A build that must carry the frontend uses `cargo build --features embedded-web`, which needs npm on the build host.

`stage-plugins.mjs` packs the default config file's TypeScript plugins as the platform-independent npm package `dimcode-plugins`, with no native archive. Node does not strip types under `node_modules`, so each plugin ships as JavaScript (its TypeScript with the types stripped and relative `.ts` imports renamed `.js`). The package's own pinned dependencies are the plugins' npm dependencies and `@arcships/rutis-runtime`, which npm installs for the user's platform. The wrapper lists it as an optional dependency and names its `plugins.default.json`, the rutis runtime and its own Node to the executable (`DIMCODE_PLUGIN_DEFAULTS`, `DIMCODE_JS_RUNTIME`, `DIMCODE_NODE`), so the daemon loads Goal, Web, SessionTitle, Workflow and Scheduler by default ([PluginManager §7](../design/plugins/PluginManager.md#7-built-in-plugins-and-the-default-config-file)).

For a cross build, set `DIM_BUILD_PLATFORM` to the intended platform, build with the matching Rust `--target`, and stage from `target/TARGET/release`. The runner must use the same OS as the target. The six platform names are listed in the workflow matrix.

These commands stage an API-key-only distribution. An account build is the `dimcode` of the private `dim-oauth-plugin-rust` repository, checked out beside this one; it starts from this checkout's `Cargo.lock`:

```sh
(cd ../dim-oauth-plugin-rust && scripts/sync-lock.sh && cargo build --release -p dim-oauth)
node scripts/stage-release.mjs ../dim-oauth-plugin-rust/target/release staging/local 0.1.0 FULL_SOURCE_COMMIT --account
```

In CI, set the `DIM_OAUTH_REPOSITORY` (HTTPS clone URL) and `DIM_OAUTH_REVISION` (full commit) variables and the `DIM_OAUTH_GIT_CREDENTIAL` secret (`user:token` of a read-only deploy token). Every target then clones that commit beside the public checkout and builds its `dimcode`; without both variables CI builds API-key-only payloads, and a partial configuration fails. Bump `DIM_OAUTH_REVISION` to release a new plugin version. The credential is used only for the clone and never enters manifests. The account build adds no runtime, package or Node: each platform package and native archive holds only the executable:

```text
bin/dimcode[.exe]              # the CLI; runs its daemon as `dimcode app-server`
LICENSE
NOTICE
release-manifest.json         # `account` records whether this is the account build
```

`pack-release.mjs` rejects changed staged files, then creates a native tar.gz (Windows zip) and a platform npm tgz from the same payload. It records SHA-256 for both. `collect-release.mjs` requires `release-<platform>` directories for all six targets and `release-web` from `stage-web.mjs` and `release-plugins` from `stage-plugins.mjs`, checks identical source/version/toolchain/account provenance and artifact digests, creates the thin npm wrapper, then validates all nine npm archives without publishing.

```sh
node scripts/collect-release.mjs staging/platforms staging/complete
node scripts/publish-npm.mjs staging/complete/npm VERSION FULL_SOURCE_COMMIT
node scripts/check-native.mjs staging/complete/native VERSION FULL_SOURCE_COMMIT
```

The wrapper resolves the installed platform optional dependency and forwards arguments, terminal streams and exit status. Installation runs no lifecycle scripts or executable downloads. Package checks include a real offline npm install of local tarballs.

## Disabled npm publication

`.github/workflows/npm-publish.yml.disabled` is not registered and cannot run. Do not activate it until package ownership and npm trusted publishing are configured and activation is explicitly requested. It is the only workflow intended to use a GitHub-hosted `ubuntu-latest` runner. No npm token is injected into self-hosted build jobs.

Future activation needs one trusted publisher for `dimcode`, `dimcode-web`, `dimcode-plugins` and each of the six platform packages, bound to `arcships/dim`, `npm-publish.yml` and `npm-release`. [npm trusted publishing documentation](https://docs.npmjs.com/trusted-publishers/).

Release construction and npm publication are separate operations. Review and publish the GitHub draft first. The npm workflow has only `workflow_dispatch`; it takes the tag of an existing **non-draft** GitHub Release and can run from `main` regardless of that release's age:

```sh
gh workflow run npm-publish.yml --ref main -f release_tag=vVERSION
```

This command becomes available only after explicit workflow activation. Publication downloads the original nine `.tgz` attachments and manifest from the specified Release, resolves the tag's commit, and verifies published status, exact package names, versions, source commit and digests before any registry write. It rechecks Release status and tag identity immediately before publishing. It does not rebuild packages or consume expiring Actions artifacts. [Release attachment download](https://cli.github.com/manual/gh_release_download).

Publication validates every package before the first registry write. A retry compares an already published version's SHA-512 integrity with the original local archive and skips an exact match. A different archive, authentication error or network error fails; only an explicit registry E404 permits publication. All platform packages precede `dimcode`; versions are `X.Y.Z`, `X.Y.Z-beta.N` or `X.Y.Z-alpha.N` and publish to `latest`, `beta` or `alpha` respectively, so each channel upgrades only within itself. An alpha must stay below the newest `dimcode` beta on the registry, because installed TypeScript betas upgrade to the highest prerelease of any identifier; publication checks this before any write.

`npm-packages` contains nine tgz files and a release manifest; `native-packages` contains seven native archives, the matching manifest and `SHA256SUMS`. The GitHub Release retains all sixteen original archives and the shared metadata independently of Actions retention. Future npm publication uploads an Actions receipt containing the Release ID, tag and package digests; that receipt also expires after one day.

## Public source export

`scripts/export-public-source.sh OUTPUT.tar.gz` exports a whitelist of tracked public source, project skills and documentation from HEAD, without `.git`, repository history, private plugins, caches or staging directories. This creates a local archive only and does not push to any remote.

Native archives and platform npm packages include the vendored terminal libraries’ Apache-2.0 license and notices under `licenses/grok-build/`, referenced by the root `NOTICE`. The extra Swift syntax asset is excluded pending license verification.
