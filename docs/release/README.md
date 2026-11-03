# Release packaging

Dim uses self-hosted runners for PR checks and the complete GitHub release workflow. npm publication stays disabled in `.github/workflows/npm-publish.yml.disabled`. The [project release skill](../../.agents/skills/dim-release/SKILL.md) covers version preparation, PRs, tags, draft inspection and recovery. [Rust alpha release](alpha.html) walks through publishing a `-alpha.N` version end to end (Chinese).

The source, including the Dim account plugin, uses MIT. An account-enabled distribution differs from the API-key one only by the OAuth client IDs compiled in from repository secrets.

## Upgrade compatibility

Before replacing an older CLI and daemon, stop that daemon with its matching CLI. The replacement `dimcode daemon stop` refuses daemons without `daemon.shutdown.completed` and leaves them running. Forced shutdown does not certify storage or plugin cleanup.

Account-enabled distributions use an executable built with the Dim account provider (`crates/dim-oauth`) through `ManagedProviderRow`. The public executable supports API-key providers and does not load the former TypeScript account package. Existing launchers must remove the old `dimcode --plugin-dir`, `--node` and `--js-runtime` arguments and use the account build when managed login is required; those arguments are rejected rather than falling back to a package. Embedding applications replace `ManagedPluginConfig` with the [typed provider hook](../../crates/dim-app-server/README.md#managed-provider-plugins).

The public host does not migrate or rewrite the account-owned `auth.json` when replacing the executable. Keep the same explicit home for the replacement account build; its provider owns reading the account format. The API-key-only executable leaves that account data in place but cannot supply its managed login or credentials. Existing Session TypeScript rows retain their separate App Server Node/runtime configuration; removing the embedded account runtime does not install a runtime for those rows.

Release cloning supplies its Git authentication header through process-local Git environment configuration, with shell tracing disabled. The credential is absent from Git arguments and is unset before checkout and build commands. Release jobs continue to use self-hosted runners isolated from PR jobs.

## File access

Under `read-only` and `workspace-write`, a file tool read (`read`, `grep` or `glob`) whose
resolved path is outside the Session workspace, the platform's system roots, the Session's own
artifacts and any Session grant now asks for approval instead of running silently. A
non-interactive run (`exec`) answers that prompt with the ordinary denial and exits 3. The same
facts now deny, instead of ask, a write to a system root or a Session-owned artifact under
`workspace-write`. `full-access` is unchanged.

## Member-approval client compatibility

Upgrade the CLI, bundled Web workspace and ACP gateway together when enabling native member
approval forwarding. Older protocol-v3 clients can display `member_approval_allow` as an
ordinary tool request containing only an opaque route id. They do not retrieve or review the
original member action. Protocol-v3 compatibility alone does not guarantee that review UI;
use a client implementing `session.collaboration.approval.get` before approving member actions.
Updated clients disable Allow when the original action cannot be reviewed and keep Reject available.

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

`ci.yml` runs on pull requests, pushes to `main`, and manual dispatch. It builds the workspace, checks formatting and Clippy, runs workspace tests and boundary checks, runs the packaging/development script tests, and exercises the ignored actual CLI profile and public Node-backed flows. Pushes to `main` and manual runs also run the account-build ACP Registry check below; pull requests receive no OAuth client IDs. A pull request runs only the groups its changed paths need, as decided by `scripts/ci-changes.mjs` (documentation-only changes skip every group, and unrecognized paths run all of them). `ci-ok` passes when every selected job passed, so it is the check to require. CI builds without debug info (`CARGO_PROFILE_DEV_DEBUG=0`). The Rust build step records Cargo test artifacts once. `scripts/run-rust-test.mjs` runs ordinary workspace executables two at a time with four test threads per executable, retaining package working directories, separate log groups and failure exit codes. Cargo runs documentation tests with the same features; ignored CLI and Node flows reuse the recorded binaries without package-scoped recompilation. Node-backed checks run for runtime, binding and service-contract paths; CLI-only changes retain their ordinary and actual-profile flows. Evaluation tests use `xtask/eval-tests` for `xtask/` changes, and the wall-clock handoff runs for execution-related changes. Main pushes and manual dispatch select every group. Frontend tests are an explicit CI step, separate from release packaging.

### Action pins and updates

External Actions in `ci.yml`, `release.yml` and the disabled npm publication workflow use full commit SHAs. PR checks use the same policy because their Actions execute on persistent self-hosted runners. Pinning does not change the [CI trust boundary](#ci-trust-model).

Update an Action in a normal pull request:

1. Select a release in the Action's original upstream repository. Review its release notes and the changes since the pinned commit, including runner requirements and input changes.
2. Resolve the release tag to its commit. For example, `git ls-remote https://github.com/actions/checkout.git 'refs/tags/v6.1.0' 'refs/tags/v6.1.0^{}'` shows the commit; for an annotated tag use the peeled `^{}` entry, not the tag object's SHA. Verify the commit in the upstream repository, then update every use of that Action and its version comment together. Keep npm publication disabled.
3. Run `node --test scripts/*.test.mjs`, review the workflow diff and require PR CI to pass before merging. A change to release-only behavior also needs the affected platform's release-build validation before shipping.

The release script suite rejects external Action references that are not full commit SHAs, including those in disabled workflows. Upstream tag movement never updates a pin automatically.

### ACP Registry checks

The `acp-registry` job runs on the trusted Linux x64 runner for `main` pushes and manual CI runs. It installs the toolchain from `rust-toolchain.toml`, compiles `dimcode` with both OAuth client ID secrets and executes the unmodified upstream Registry authentication checker and unauthenticated protocol matrix, pinned to the revision in `ci.yml`. Missing secrets, authentication failures, skipped authentication checks or failed advertised protocol methods fail `ci-ok`. Pull requests skip this job and retain the ordinary Gateway/helper flow tests; they never receive the client IDs.

`scripts/check-acp-registry.mjs EXECUTABLE REGISTRY_CHECKOUT REPORT_DIRECTORY` also runs locally with uv and tar available. It uses Python 3.14.2 with no added Python packages, makes a temporary binary manifest with the executable's actual version, disables browser launch to exercise terminal capability negotiation, and stops its isolated daemon on exit. Reports retain upstream/source provenance and the auth/matrix results for one day; the executable and account home are never uploaded. Update the pinned upstream revision only after running the same check against the candidate revision.

The protocol matrix reports unsupported optional methods separately; its process exit status alone is not a gate. The adapter checks initialize, authentication discovery, Session listing and every advertised probed method. Empty model configuration and unauthenticated requests are not successful login or chat. Formal Registry metadata, release version/channel and published-package acceptance remain release checks; this job does not relabel alpha versions or publish anything.

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
- PR runners must hold no release credentials and stay off private networks; PR jobs receive no secrets, and `GITHUB_TOKEN` can only read repository contents. Only the trusted `acp-registry` build step receives the paired OAuth client IDs; the upstream-check step receives neither those environment variables nor user credentials.

Accepting contributions from forks or outside collaborators requires moving their checks to ephemeral runners first.

`release.yml` runs when a `v*.*.*` tag is pushed, or when manually dispatched with an existing tag:

```sh
gh workflow run release.yml --ref main -f tag=vVERSION
```

Preflight checks the exact tag commit is reachable from `origin/main`, and verifies the tag, Cargo workspace, npm wrapper/optional dependencies and runtime metadata have the same version. Every build checks out the resolved commit SHA. Six target jobs stage and pack their payloads. Assembly requires every target, matching toolchain/plugin provenance and valid artifact digests before creating a draft GitHub Release. Prerelease tags mark the draft as a prerelease. Release notes are generated from GitHub history and should be reviewed before publishing.

The draft receives seven native archives (six platforms and the Web workspace), eight original npm tarballs (six platform packages, `dimcode-web` and the wrapper), `release-manifest.json` and `SHA256SUMS`: seventeen attachments in total. `SHA256SUMS` covers the native archives; the manifest records digests for both native and npm archives. npm tarballs are not published to a registry by this workflow. Existing releases, including drafts, are rejected to avoid replacing attachments. If an upload leaves an incomplete draft, inspect it and remove that draft only when authorized before retrying; keep its tag.

All Actions artifacts, including the disabled npm publication receipt, use `retention-days: 1`. GitHub Release attachments persist independently of Actions retention. A retry after artifacts expire must rebuild all targets.

## Local packaging

One product version is shared by `Cargo.toml`, workspace versions in `Cargo.lock`, `npm/package.json`, its exact optional dependencies, runtime package/lock metadata, native version output and the release tag. Keep them synchronized before building. `0.1.0` is a development baseline; check existing npm versions before enabling registry publication. Every build command here sets `DIM_RELEASE_BUILD=1`, which makes the executable report the version above; a build without that mark reports the committed version marked `-source`, because the repository keeps the last released version.

```sh
DIM_RELEASE_BUILD=1 cargo build --release --locked -p dimcode
node scripts/stage-plugins.mjs staging/local-plugins
node scripts/stage-release.mjs target/release staging/local 0.1.0 FULL_SOURCE_COMMIT --plugins staging/local-plugins
node scripts/pack-release.mjs staging/local release/local
node scripts/stage-web.mjs release/web 0.1.0 FULL_SOURCE_COMMIT
```

The executable carries no frontend. `stage-web.mjs` builds `web/` once (`npm ci`, `npm run build`), records `web/dimcode-web.json` (`version`, `protocolVersion`, `sourceCommit`) beside it and packs the tree as `dimcode-web-VERSION.tar.gz` and the npm package `dimcode-web`, which the wrapper lists as an optional dependency and names to the executable through `DIMCODE_WEB_ASSETS`. A native-archive user unpacks `dimcode-web-VERSION.tar.gz` so that `web/index.html` sits beside the executable's `bin/` or in `<product home>/web`. A build that must carry the frontend uses `DIM_RELEASE_BUILD=1 cargo build --features embedded-web`, which needs npm on the build host.

For a cross build, set `DIM_BUILD_PLATFORM` to the intended platform, build with the matching Rust `--target`, and stage from `target/TARGET/release`. The runner must use the same OS as the target. The six platform names are listed in the workflow matrix.

These commands stage an API-key-only distribution. An account build is the same `dimcode` compiled with the Dim account's OAuth client IDs, which never enter the repository:

```sh
DIM_RELEASE_BUILD=1 DIM_OAUTH_CLIENT_ID_CN=… DIM_OAUTH_CLIENT_ID_GLOBAL=… cargo build --release --locked -p dimcode
node scripts/stage-plugins.mjs staging/local-plugins
node scripts/stage-release.mjs target/release staging/local 0.1.0 FULL_SOURCE_COMMIT --account --plugins staging/local-plugins
```

In CI, set the `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` repository secrets. Every target is then an account build; without both secrets CI builds API-key-only payloads, and a partial configuration fails. The IDs are compiled into the executable and never enter manifests. The account build adds no runtime, package or Node. Each platform package and native archive holds the executable, the default configuration file and the default plugins (`scripts/stage-plugins.mjs`, then `stage-release.mjs --plugins DIR`); the Node and Python runtimes are not bundled: RuntimeManager downloads them, verified against pins compiled into Dim, into the home ([RuntimeManager](../design/plugins/RuntimeManager.md)):

```text
bin/dimcode[.exe]              # the CLI; runs its daemon as `dimcode app-server`
default-plugins.json          # the default configuration file, passed as `--defaults`
plugins/<name>/               # the default plugins with their production node_modules
LICENSE
NOTICE
release-manifest.json         # `account` records whether this is the account build
```

The alpha publisher stages the plugin tree once on the build host and gives every platform the same copy, as `publish-alpha.mjs` does, so it runs `stage-plugins.mjs --portable`. That refuses, before installing anything, a listed plugin whose `package-lock.json` names a package with `os`, `cpu` or `libc`: one shared tree cannot carry another platform's package. `release.yml` stages on each platform's own runner and does not pass the flag.

`pack-release.mjs` rejects changed staged files, then creates a native tar.gz (Windows zip) and a platform npm tgz from the same payload. It records SHA-256 for both. `collect-release.mjs` requires `release-<platform>` directories for all six targets and `release-web` from `stage-web.mjs`, checks identical source/version/toolchain/account provenance and artifact digests, creates the thin npm wrapper, then validates all seven npm archives without publishing.

```sh
node scripts/collect-release.mjs staging/platforms staging/complete
node scripts/publish-npm.mjs staging/complete/npm VERSION FULL_SOURCE_COMMIT
node scripts/check-native.mjs staging/complete/native VERSION FULL_SOURCE_COMMIT
```

The wrapper resolves the installed platform optional dependency and forwards arguments, terminal streams and exit status. Installation runs no lifecycle scripts or executable downloads. Package checks include a real offline npm install of local tarballs.

## Disabled npm publication

`.github/workflows/npm-publish.yml.disabled` is not registered and cannot run. Do not activate it until package ownership and npm trusted publishing are configured and activation is explicitly requested. It is the only workflow intended to use a GitHub-hosted `ubuntu-latest` runner. No npm token is injected into self-hosted build jobs.

Future activation needs one trusted publisher for `dimcode`, `dimcode-web` and each of the six platform packages, bound to `arcships/dim`, `npm-publish.yml` and `npm-release`. [npm trusted publishing documentation](https://docs.npmjs.com/trusted-publishers/).

Release construction and npm publication are separate operations. Review and publish the GitHub draft first. The npm workflow has only `workflow_dispatch`; it takes the tag of an existing **non-draft** GitHub Release and can run from `main` regardless of that release's age:

```sh
gh workflow run npm-publish.yml --ref main -f release_tag=vVERSION
```

This command becomes available only after explicit workflow activation. Publication downloads the original eight `.tgz` attachments and manifest from the specified Release, resolves the tag's commit, and verifies published status, exact package names, versions, source commit and digests before any registry write. It rechecks Release status and tag identity immediately before publishing. It does not rebuild packages or consume expiring Actions artifacts. [Release attachment download](https://cli.github.com/manual/gh_release_download).

Publication validates every package before the first registry write. A retry compares an already published version's SHA-512 integrity with the original local archive and skips an exact match. A different archive, authentication error or network error fails; only an explicit registry E404 permits publication. All platform packages precede `dimcode`; versions are `X.Y.Z`, `X.Y.Z-beta.N` or `X.Y.Z-alpha.N` and publish to `latest`, `beta` or `alpha` respectively, so each channel upgrades only within itself. An alpha must stay below the newest `dimcode` beta on the registry, because installed TypeScript betas upgrade to the highest prerelease of any identifier; publication checks this before any write.

`npm-packages` contains eight tgz files and a release manifest; `native-packages` contains seven native archives, the matching manifest and `SHA256SUMS`. The GitHub Release retains all fifteen original archives and the shared metadata independently of Actions retention. Future npm publication uploads an Actions receipt containing the Release ID, tag and package digests; that receipt also expires after one day.

## Public source export

`scripts/export-public-source.sh OUTPUT.tar.gz` exports a whitelist of tracked public source, project skills and documentation from HEAD, without `.git`, repository history, private plugins, caches or staging directories. This creates a local archive only and does not push to any remote.

Native archives and platform npm packages include the vendored terminal libraries’ Apache-2.0 license and notices under `licenses/grok-build/`, referenced by the root `NOTICE`. The extra Swift syntax asset is excluded pending license verification.
