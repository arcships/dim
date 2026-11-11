# Release packaging

PR checks use self-hosted runners. Release preparation, packaging, npm publication and acceptance use standard GitHub-hosted runners. The [project release skill](../../.agents/skills/dim-release/SKILL.md) covers version preparation, PRs, tags, draft inspection and recovery. [Rust alpha release](alpha.html) covers the alpha channels and local publication alternative (Chinese).

The source, including the Dim account plugin, uses MIT. An account-enabled distribution differs from the API-key one only by the OAuth client IDs compiled in from repository secrets.

The Linux executable is also planned as an OCI image. [Container image release](docker-image.md) (Draft) designs its content, volumes, client shapes, image workflow and security boundary; it repackages the archives above rather than rebuilding them. No image is published yet.

## Release skills

Both skills live in this repository and travel with its release workflow. Use the requested stage explicitly:

| Request | Skill | Result |
| --- | --- | --- |
| Choose the next alpha and prepare its version PR | [dim-release](../../.agents/skills/dim-release/SKILL.md) | Committed, consistent metadata and a PR into `main`; no tag/build/publication |
| Build a GitHub draft or publish its npm packages | [dim-release](../../.agents/skills/dim-release/SKILL.md) | The authorized CI stage, using the immutable release version and original archives |
| Choose the alpha route, or explicitly build/publish on a local Mac | [dim-alpha-release](../../skills/dim-alpha-release/SKILL.md) | CI by default; the local command only when that route is requested |

Example agent requests:

```text
Use $dim-release to prepare the next alpha version PR. Select an unused version,
synchronize the metadata and run its consistency checks. Do not merge, tag,
dispatch a build or publish.

Use $dim-release to build a GitHub prerelease draft from the verified release
commit on main. Inspect the artifacts and stop before publishing it or npm.

Use $dim-release to publish the existing GitHub prerelease vX.Y.Z-alpha.N to npm.
Use its original packages and the configured OIDC workflow; do not rebuild them.

Use $dim-alpha-release for a local Mac dry run only. Build and validate the alpha;
do not publish to npm or create GitHub tags/Releases.
```

If an agent does not list a skill, ask it to read the linked repository `SKILL.md` by path. An explicit end-to-end publication request can authorize the complete workflow; a preparation or review request does not. Existing authorization does not need to be requested again at each stage.

## Version preparation

The version-PR route uses an explicit committed `X.Y.Z-alpha.N`. It does not select, bump or write back versions. All eight npm packages and the executable use the same version; the Git tag adds a leading `v`.

For the next alpha, inspect registry versions across `dimcode-alpha`, `dimcode-web`, the six platform packages and legacy `dimcode`, plus reserved remote tags/GitHub Releases and open release PRs. Continue an existing PR for the same preparation task; otherwise increment the highest reserved alpha numerically. A partial publication reserves its version too. Do not infer availability from the checkout's version or a dist-tag alone. Reuse the matching release version and original archives for a CI retry instead of allocating another number. Keep the existing legacy-beta compatibility guard.

Prepare a normal version PR from `origin/main`, synchronizing:

| File | Fields |
| --- | --- |
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | Generated versions of packages inheriting the workspace version; refresh with Cargo metadata |
| `npm/package.json` | `version` and every exact `optionalDependencies` version |
| `runtime/package.json` | `version` |
| `runtime/package-lock.json` | Root `version` and `packages[""].version` |

Explicitly versioned crates such as `dim-agent-types` keep their own version. The npm source template keeps the name `dimcode` and the `dim`/`dimcode` bins: collection selects `dimcode-alpha`/`dima` for an alpha. Do not change Node/Rutis versions or unrelated dependencies as part of the product bump. The skill provides executable consistency checks; metadata-only preparation needs no release build.

```text
version PR -> required CI -> merge into main -> annotated vX.Y.Z-alpha.N tag
           -> GitHub-hosted build -> inspect GitHub draft -> authorized publication
```

The local Mac route differs: `publish-alpha.mjs` selects a version and temporarily updates these product fields, then restores them without committing. Do not use it to prepare a CI version PR. Source changes after tagging need a new version and tag; existing tags and published package bytes are immutable.

## Upgrade compatibility

The Timeline property-query change in [#1286](https://github.com/arcships/dim/pull/1286) breaks the plugin `timeline.query` request and reply shapes. Plugins using it require a host build containing #1286 and the matching `dimcode/plugin` declarations. Older released hosts do not implement this contract; the checkout's unchanged `0.1.0-alpha.18` version does not identify support. Migrate requests to `query`/`page` and replies to `result`/`next` using the [query contract](../design/timeline-property-query.md#2-查询接口). There is no legacy-shape fallback or independently negotiated query version. Upgrade the host and plugins together. The same change upgrades native storage to v36; older binaries refuse the upgraded store.

Before replacing an older CLI and daemon, stop that daemon with its matching CLI. A client of another build replaces the daemon on its own only while the daemon is idle; while other clients use it, the client keeps the daemon it finds and says so. The replacement `dimcode daemon stop` refuses daemons without `daemon.shutdown.completed` and leaves them running. Forced shutdown does not certify storage or plugin cleanup.

Account-enabled distributions use an executable built with the Dim account provider (`crates/dim-oauth`) through `ManagedProviderRow`. The public executable supports API-key providers and does not load the former TypeScript account package. Existing launchers must remove the old `dimcode --plugin-dir`, `--node` and `--js-runtime` arguments and use the account build when managed login is required; those arguments are rejected rather than falling back to a package. Embedding applications replace `ManagedPluginConfig` with the [typed provider hook](../../crates/dim-app-server/README.md#managed-provider-plugins).

The public host does not migrate or rewrite the account-owned `auth.json` when replacing the executable. Keep the same explicit home for the replacement account build; its provider owns reading the account format. The API-key-only executable leaves that account data in place but cannot supply its managed login or credentials. Existing Session TypeScript rows retain their separate App Server Node/runtime configuration; removing the embedded account runtime does not install a runtime for those rows.

Release cloning supplies its Git authentication header through process-local Git environment configuration, with shell tracing disabled. The credential is absent from Git arguments and is unset before checkout and build commands. Each release job runs on a fresh GitHub-hosted VM.

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

Release builds use the following [GitHub-hosted runner images](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Each platform stages its native executable after checking its reported release version.

| Platform | Rust target | Runner |
| --- | --- | --- |
| macOS ARM64 | `aarch64-apple-darwin` | `macos-15` |
| macOS Intel | `x86_64-apple-darwin` | `macos-15-intel` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |
| Linux x64 | `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `windows-11-arm` |
| Windows x64 | `x86_64-pc-windows-msvc` | `windows-2022` |

The Ubuntu preflight generates the Rust distribution notices once with cargo-about 0.9.3 using the versioned checkout. All six targets download that artifact. Linux builds on each native host with GNU tools in a digest-pinned manylinux_2_28 container. The executable also runs in that glibc 2.28 environment. Windows uses native MSVC, clang-cl on ARM64, NASM on x64, and the Windows system tar to create ZIP archives. Actions installs Node from `.node-version` and Rust uses `rust-toolchain.toml`. Web and Release assembly run on hosted Ubuntu.

The separate PR workflow uses self-hosted macOS ARM64, Windows x64 and Linux x64 runners. Register those runners with the [self-hosted OS and architecture labels](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/use-in-a-workflow). Keep PR runner credentials and private networks isolated as required by the [CI trust model](#ci-trust-model).

To limit each physical Mac to one `verify` and one `dependencies` job at a time, assign `dim-verify` to one registered runner on each Mac and `dim-dependencies` to a second runner. Once both labels are in place, set the repository Actions variable `DIM_CI_MACOS_DEDICATED_RUNNERS` to `true`. With the variable unset, both jobs use the standard self-hosted macOS ARM64 pool.

## GitHub workflows

`ci.yml` runs on pull requests, pushes to `main`, and manual dispatch. It builds the workspace, checks formatting and Clippy, runs workspace tests and boundary checks, runs the packaging/development script tests, and exercises the ignored actual CLI profile and public Node-backed flows. Pushes to `main` and manual runs also run the account-build ACP Registry check below; pull requests receive no OAuth client IDs. A pull request runs only the groups its changed paths need, as decided by `scripts/ci-changes.mjs` (documentation-only changes skip every group, and unrecognized paths run all of them). `ci-ok` passes when every selected job passed, so it is the check to require. CI builds without debug info (`CARGO_PROFILE_DEV_DEBUG=0`). The [build environment](../architecture/build.md) describes test artifact reuse, concurrency limits, and target deadlines. Cargo runs documentation tests with the same features, and ignored CLI and Node flows reuse the recorded binaries without package-scoped recompilation. Node-backed checks run for runtime, binding and service-contract paths; CLI-only changes retain their ordinary and actual-profile flows. Evaluation tests use `xtask/eval-tests` for `xtask/` changes, and the wall-clock handoff runs for execution-related changes. Main pushes and manual dispatch select every group. Frontend tests are an explicit CI step, separate from release packaging.

The `Verify Python plugins` step is off by default. When a Python plugin ships, set the repository Actions variable `DIM_CI_PYTHON_PLUGINS` to `true` to run it whenever the Node check group is selected. It prepares CPython for the `python_plugin_` flows and reuses the App Server test binary from the workspace build. See the [App Server verification commands](../../crates/dim-app-server/README.md#verification) to run the suites locally.

The `Verify downloaded Node runtime` step runs on pushes to `main` and manual dispatch. It switches a running system Node to the official downloaded archive and reuses the same App Server test binary. The public Node bindings check uses the installed interpreter.

### Action pins and updates

External Actions in `ci.yml`, `release.yml`, `npm-publish.yml` and `image-publish.yml` use full commit SHAs. PR checks use the same policy because their Actions execute on persistent self-hosted runners. Pinning does not change the [CI trust boundary](#ci-trust-model).

Update an Action in a normal pull request:

1. Select a release in the Action's original upstream repository. Review its release notes and the changes since the pinned commit, including runner requirements and input changes.
2. Resolve the release tag to its commit. For example, `git ls-remote https://github.com/actions/checkout.git 'refs/tags/v6.1.0' 'refs/tags/v6.1.0^{}'` shows the commit; for an annotated tag use the peeled `^{}` entry, not the tag object's SHA. Verify the commit in the upstream repository, then update every use of that Action and its version comment together. Keep `NPM_ALPHA_PUBLISH_ENABLED` unset or `false` while validating publication changes.
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

To prepare the next GitHub alpha prerelease from the current main commit, run:

```sh
gh workflow run release.yml --ref main
```

The preflight records the selected main SHA and both `dimcode-alpha` tags. It reads the Web and six native tarball manifests, verifies their registry integrity, source SHA and account-enabled native builds, and checks the wrapper's seven exact dependencies. Only a complete nightly for that SHA can be reused; an existing Git tag alone does not skip a build. A registry authentication, network or service error stops preflight rather than implying an absent version. Otherwise it increments the highest alpha across the registry packages and Git tags, including versions reserved for GitHub Releases awaiting npm publication. All six hosted targets and the Web assembly use that source SHA and stamp the allocated version into the build checkouts. After assembly, CI reserves an annotated `vX.Y.Z-alpha.N` tag on that commit and creates the GitHub prerelease draft.

If a complete nightly already exists, a build-only invocation stops with instructions instead of claiming a verified skip. An explicitly authorized dispatch with `-f repair_nightly=true` calls the npm workflow to repair its wrapper tags, sync the mirror and run full acceptance without rebuilding. It reports that no new build is needed only after acceptance succeeds. This recovery uses registry packages and does not require a corresponding GitHub Release. Build-only requests do not authorize tag writes or mirror sync. The local Mac worktree, `mbx` and cleanup instructions do not apply to these disposable GitHub-hosted runners; their native builds retain the 15 GiB initial disk guard.

To verify a release workflow change before merging, dispatch it from the feature branch with an existing alpha tag. This builds the tag with test OAuth client IDs, runs native version checks and collects all 17 attachments as run artifacts. Only a tag push or a dispatch from main can create the GitHub draft.

An existing tag can also start its build on push or via `gh workflow run release.yml --ref main -f tag=vVERSION`. Its commit must be reachable from main. CI-generated alpha tags repeat the build-time stamp on recovery. Manually prepared tags must match the committed Cargo, npm wrapper and runtime versions. Every build checks out the resolved commit SHA. Six target jobs stage and pack their payloads. Assembly requires every target, matching toolchain/plugin provenance and valid artifact digests before creating a draft GitHub Release. Prerelease tags mark the draft as a prerelease. Release notes are generated from GitHub history and should be reviewed before publishing.

The draft receives seven native archives (six platforms and the Web workspace), eight original npm tarballs (six platform packages, `dimcode-web` and the wrapper), `release-manifest.json` and `SHA256SUMS`: seventeen attachments in total. `SHA256SUMS` covers the native archives; the manifest records digests for both native and npm archives. npm tarballs are not published to a registry by this workflow. Existing releases, including drafts, are rejected to avoid replacing attachments. If an upload leaves an incomplete draft, inspect it and remove that draft only when authorized before retrying; keep its tag.

All Actions artifacts, including the npm publication receipt, use `retention-days: 1`. GitHub Release attachments persist independently of Actions retention. A retry after build artifacts expire must rebuild all targets; npm publication can download the original Release attachments again.

## Local packaging

One product version is shared by `Cargo.toml`, workspace versions in `Cargo.lock`, `npm/package.json`, its exact optional dependencies, runtime package/lock metadata, native version output and the release tag. Keep them synchronized before building. `0.1.0` is a development baseline; check existing npm versions before enabling registry publication. Every build command here sets `DIM_RELEASE_BUILD=1`, which makes the executable report the version above; a build without that mark reports the committed version marked `-source`, because the repository keeps the last released version.

```sh
DIM_RELEASE_BUILD=1 cargo build --release --locked -p dimcode
node scripts/stage-plugins.mjs staging/local-plugins
node scripts/rust-notices.mjs staging/rust-notices.txt
node scripts/stage-release.mjs target/release staging/local 0.1.0 FULL_SOURCE_COMMIT --plugins staging/local-plugins --rust-notices staging/rust-notices.txt
node scripts/pack-release.mjs staging/local release/local
node scripts/stage-web.mjs release/web 0.1.0 FULL_SOURCE_COMMIT
```

The executable carries no frontend. `stage-web.mjs` builds `web/` once (`npm ci`, `npm run build`), records `web/dimcode-web.json` (`version`, `protocolVersion`, `sourceCommit`) beside it and packs the tree as `dimcode-web-VERSION.tar.gz` and the npm package `dimcode-web`, which the wrapper lists as an optional dependency and names to the executable through `DIMCODE_WEB_ASSETS`. A native-archive user unpacks `dimcode-web-VERSION.tar.gz` so that `web/index.html` sits beside the executable's `bin/` or in `<product home>/web`. A build that must carry the frontend uses `DIM_RELEASE_BUILD=1 cargo build --features embedded-web`, which needs npm on the build host.

For a cross build, set `DIM_BUILD_PLATFORM` to the intended platform, build with the matching Rust `--target`, and stage from `target/TARGET/release`. The runner must use the same OS as the target. The six platform names are listed in the workflow matrix.

These commands stage an API-key-only distribution. An account build is the same `dimcode` compiled with the Dim account's OAuth client IDs, which never enter the repository:

```sh
DIM_RELEASE_BUILD=1 DIM_OAUTH_CLIENT_ID_CN=… DIM_OAUTH_CLIENT_ID_GLOBAL=… cargo build --release --locked -p dimcode
node scripts/stage-plugins.mjs staging/local-plugins
node scripts/rust-notices.mjs staging/rust-notices.txt
node scripts/stage-release.mjs target/release staging/local 0.1.0 FULL_SOURCE_COMMIT --account --plugins staging/local-plugins --rust-notices staging/rust-notices.txt
```

In CI, set the `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` repository secrets. Every CI alpha requires both IDs and packages the account-enabled executable. A missing ID fails the build. The IDs are compiled into the executable and never enter manifests. The account build adds no runtime, package or Node. Each platform package and native archive holds the executable, the default configuration file, the default plugins and the optional plugins' directories, without their dependencies (`scripts/stage-plugins.mjs`, then `stage-release.mjs --plugins DIR`); the Node and Python runtimes are not bundled: RuntimeManager acquires them on request (`runtime.install`), verified against pins compiled into Dim, into the home. The shipped configuration disables both `auto` flags, so a first start downloads nothing, and a Node plugin needs `dimcode runtime install node` before it runs ([RuntimeManager](../design/plugins/RuntimeManager.md)):

```text
bin/dimcode[.exe]              # the CLI; runs its daemon as `dimcode app-server`
default-plugins.json          # the default configuration file, passed as `--defaults`
plugins/<name>/               # the default plugins; plugin-authoring has no production node_modules
LICENSE
NOTICE
licenses/                     # Rust dependency texts and vendored notices
release-manifest.json         # `account` records whether this is the account build
```

Workflow is excluded from the default manifest and from development, alpha and release payloads, including its production dependencies. Its TypeScript source remains available for [explicit installation](../../runtime/plugins/workflow/README.md#loading); external packaging and distribution are tracked in [#1091](https://github.com/arcships/dim/issues/1091).

The alpha publisher stages the plugin tree once on the build host and gives every platform the same copy, as `publish-alpha.mjs` does, so it runs `stage-plugins.mjs --portable`. That refuses, before installing anything, a default plugin whose `package-lock.json` names a package with `os`, `cpu` or `libc`: one shared tree cannot carry another platform's package. An optional plugin's lock is not checked, because the tree carries none of its packages: they are installed on the user's machine when the plugin is installed. `release.yml` stages on each platform's own runner and does not pass the flag.

`pack-release.mjs` rejects changed staged files, then creates a native tar.gz (Windows zip) and a platform npm tgz from the same payload. It records SHA-256 for both. `collect-release.mjs` requires `release-<platform>` directories for all six targets and `release-web` from `stage-web.mjs`, checks identical source/version/toolchain/account provenance and artifact digests, creates the thin npm wrapper, then validates all eight npm archives without publishing.

```sh
node scripts/collect-release.mjs staging/platforms staging/complete
node scripts/publish-npm.mjs staging/complete/npm VERSION FULL_SOURCE_COMMIT
node scripts/check-native.mjs staging/complete/native VERSION FULL_SOURCE_COMMIT
```

The wrapper resolves the installed platform optional dependency and forwards arguments, terminal streams and exit status. Installation runs no lifecycle scripts or executable downloads. Package checks include a real offline npm install of local tarballs.

## Alpha npm publication

The registered `.github/workflows/npm-publish.yml` runs only from `main`. New package publication requires a published alpha GitHub Release. Alternatively, `source_commit` selects tag-only recovery of a complete nightly for the dispatch's main SHA; it is mutually exclusive with `release_tag`. Its default is read-only verification on GitHub-hosted Ubuntu. The GitHub-hosted `ubuntu-latest` publication or repair job is scheduled only when both the `publish` input and repository variable `NPM_ALPHA_PUBLISH_ENABLED` are `true`. Leave the variable unset or `false` during setup. Only the npm publication job receives OIDC write permission.

```text
alpha tag -> GitHub-hosted builds -> draft GitHub Release -> maintainer publishes prerelease
          -> read-only hosted package verification
          -> publish=true + NPM_ALPHA_PUBLISH_ENABLED=true
          -> GitHub-hosted npm OIDC publication (no build or tests)
```

Configure each of these eight existing npm packages: `dimcode-alpha`, `dimcode-web`, `dimcode-darwin-arm64`, `dimcode-darwin-x64`, `dimcode-linux-arm64`, `dimcode-linux-x64`, `dimcode-windows-arm64` and `dimcode-windows-x64`. The `dimcode` package is not part of this workflow.

In each package's npm Settings → Trusted publishing, add GitHub Actions with:

| Field | Value |
| --- | --- |
| Organization or user | `arcships` |
| Repository | `dim` |
| Workflow filename | `npm-publish.yml` |
| Environment name | `npm-release` |
| Allowed action | Allow direct `npm publish`; also allow `npm dist-tag` for `dimcode-alpha` |

Use only the filename, without `.github/workflows/`. npm requires Node >=22.14.0; the workflow uses `.node-version` and installs npm 11.21.0, which supports OIDC dist-tag operations. Dist-tag permission is independent of publish permission and defaults to off. For recovery called by `release.yml`, add a second trusted publisher on `dimcode-alpha` for the calling workflow `release.yml`, environment `npm-release`, with `Allow npm dist-tag`; direct package publication need not be enabled for that publisher. npm validates the calling workflow's identity for reusable workflows. A new trusted publisher must complete its first publication within two days, so configure it close to the intended release. No `NPM_TOKEN` or `NODE_AUTH_TOKEN` secret is required. The repository must be public for npm's automatic provenance generation. [npm trusted publishing documentation](https://docs.npmjs.com/trusted-publishers/).

Create the GitHub `npm-release` environment and restrict it to the `main` branch. This configuration has no required environment reviewer: repository write access and an explicit workflow dispatch with `publish=true` authorize publication. After all eight publishers are configured and a release is ready, set the repository Actions variable `NPM_ALPHA_PUBLISH_ENABLED=true`. Setting it alone does not schedule a runner or publish a package.

Release construction and new npm package publication are separate operations. Review and publish the GitHub draft first. Dispatch the npm workflow with the tag of an existing **non-draft** GitHub Release from `main` regardless of that release's age:

```sh
gh workflow run npm-publish.yml --ref main -f release_tag=vX.Y.Z-alpha.N
```

The command above performs read-only verification on a GitHub-hosted Ubuntu runner. It downloads the original eight `.tgz` attachments and manifest, then checks published status, exact package names, versions, source commit and digests with `publish-npm.mjs --check-release`. It also reads npm registry metadata to reject conflicting published bytes and an alpha that would overtake the existing `dimcode` beta. It neither rebuilds nor publishes. [Release attachment download](https://cli.github.com/manual/gh_release_download).

For an explicitly authorized publication, with the repository variable enabled:

```sh
gh workflow run npm-publish.yml --ref main -f release_tag=vX.Y.Z-alpha.N -f publish=true
```

The read-only Ubuntu job verifies the originals first. The GitHub-hosted job independently downloads the original archives and manifest directly from the GitHub Release, rechecks the alpha tag and Release identity, validates the packages and registry state again, and publishes with npm OIDC. Before publishing it snapshots protected tags for the manifest's package set, not the dispatch checkout's template. Only an HTTP 404 represents a never-published package with no tags; authentication, network and server errors stop the snapshot. The publisher consumes only Release attachments. Retrying the workflow downloads the originals from the Release again, independently of any previous run's artifact retention. The publication receipt retains that exact package set through promotion and acceptance; source-only repair records the checkout's package set in its receipt once.

Publication validates every package before the first registry write. A retry compares an already published version's SHA-512 integrity with the original local archive and skips an exact match. A different archive, authentication error or network error fails; only an explicit registry E404 permits publication. The six platform packages and `dimcode-web` precede `dimcode-alpha`; all eight use the `alpha` dist-tag. After all exact versions and source manifests are verified, the wrapper's `latest` and `alpha` tags converge on that same `X.Y.Z-alpha.N` nightly. `dima` remains the only command; existing binaries continue checking the `alpha` update channel. The wrapper's `latest` is an installation default, not a stable release. All `dimcode` tags, platform/Web non-alpha tags, and wrapper tags other than `latest`/`alpha` are recorded before publication and must remain unchanged. An alpha must stay below the newest `dimcode` beta on the registry, because installed TypeScript betas upgrade to the highest prerelease of any identifier; publication checks this before any write.

Tag promotion rechecks both wrapper tags immediately before and after each write, stops on observed concurrent publication, and refuses to move either tag backward. npm does not expose an atomic compare-and-set operation for the pair, so the workflow serializes its publication and recovery runs; outside writers must not publish concurrently. Failed acceptance is not reported as a completed release.

After promotion a separate acceptance job checks all eight exact versions, upstream alpha tags, wrapper latest, metadata and protected tags. It installs both `dimcode-alpha` without a suffix and `dimcode-alpha@alpha`, each with a fresh temporary prefix/cache, and checks the installed package version and `dima --version`. It then requests sync for all eight packages on the Alibaba npm mirror, waits for both wrapper tags and all seven pinned versions, and repeats both installations. Each registry has a ten-minute visibility window with 15-second polling. Installation uses an isolated home and cleanup runs even on failure. Mirror lag or installation/version mismatch fails acceptance; it cannot be reported as a fully updated install/upgrade channel. Retry a failed acceptance job independently of publication.

For authorized tag-only recovery of the exact main SHA selected by the dispatch, use `gh workflow run release.yml --ref main -f repair_nightly=true`, or `gh workflow run npm-publish.yml --ref main -f source_commit=FULL_MAIN_SHA -f publish=true`. The latter does not build or publish packages if no complete nightly exists; it fails. The former builds a new draft when no complete nightly exists. Neither source-only route accepts an older ancestor SHA. An older nightly without a published GitHub Release cannot be repaired through these CI inputs; a new main build is needed. An older published Release can still be retried via `release_tag`, subject to no-rollback checks. The dual-tag nightly contract belongs to the CI route; the explicit local `publish-alpha.mjs` alternative publishes only `alpha`, does not align `latest`, and is not equivalent acceptance.

For a local, non-publishing dry run, create a temporary JSON receipt containing `sourceCommit`, run `node scripts/check-alpha-registry.mjs RECEIPT source --dry-run`, and inspect its selected reusable `version` (null means a build is required). For an existing complete version, `promote --dry-run` verifies provenance and prints required tag changes without writing them. `npm --dry-run` and `mirror --dry-run` check visibility once and exercise both fresh installations if the tags agree. Only `promote --apply` writes wrapper tags, and only `mirror --apply` requests mirror synchronization. These registry checks do not invoke a build, publish packages or create Git tags. Mock-registry flow tests cover promotion, protected tags, incomplete/mixed-source builds, concurrency, stale installs and the no-write path; a local dry run cannot verify GitHub OIDC permissions or a future package's native binaries.

`npm-packages` contains eight tgz files and a release manifest; `native-packages` contains seven native archives, the matching manifest and `SHA256SUMS`. The GitHub Release retains all fifteen original archives and the shared metadata independently of Actions retention. npm publication uploads an Actions receipt containing the Release ID, tag and package digests; that receipt also expires after one day.

## Public source export

`scripts/export-public-source.sh OUTPUT.tar.gz` exports a whitelist of tracked public source, project skills and documentation from HEAD, without `.git`, repository history, private plugins, caches or staging directories. This creates a local archive only and does not push to any remote.

## Distribution notices

The wrapper, native packages and Web package include the root `NOTICE`. Native archives and platform npm packages retain Rust dependency notices under `licenses/RUST-THIRD-PARTY-NOTICES.txt`, Aimux's MIT license under `licenses/aimux/` and the vendored terminal libraries' Apache-2.0 license and notices under `licenses/grok-build/`. The extra Swift syntax asset is excluded pending license verification. Default TypeScript plugins keep the license files supplied by their npm packages.

Run `node scripts/rust-notices.mjs staging/rust-notices.txt` before staging native payloads and pass `--rust-notices staging/rust-notices.txt` to `stage-release.mjs`. CI and the local alpha publisher do this automatically. The generator uses pinned `cargo-about`, the locked workspace's full feature and target graph, and `scripts/release-licenses.toml`; it excludes dev-only dependencies. It preserves the generated license texts and supplements them with the crates' original license, copyright and notice files, including nested native dependencies. Generation errors stop packaging; staging refuses missing or empty Rust notices. This is distribution output, while `deny.toml` remains the license-policy gate.

Every Web production build emits `THIRD-PARTY-NOTICES.txt` from the installed production package licenses. The conservative inventory includes every non-dev lock entry, including type packages, and skips optional packages absent on the build host. It includes Geist and Geist Mono's SIL Open Font License and any upstream notices. Packages that omit license files use the exact-version source notices recorded under `web/licenses/`; a missing notice fails the build. The file is shipped in both Web archives, in the npm package and in embedded Web builds. Staging refuses a frontend build without it, and the release manifest covers its digest.
