---
name: dim-alpha-release
description: Build and publish the Rust dimcode CLI as an npm alpha (X.Y.Z-alpha.N on the `alpha` dist-tag) with one local command, scripts/publish-alpha.mjs. Use for "release/publish an alpha", a dry run of one, choosing the next alpha version, or resuming a failed alpha publish. Stable and beta releases follow dim-release.
---

# Dim alpha release

Releases and betas ship as the npm package `dimcode` (command `dim`). Alphas ship as the separate package `dimcode-alpha` (command `dima` only), so the two install side by side. An alpha build also defaults to its own home, `~/.dimcode-alpha`, and never touches `dim`'s `~/.dimcode`. The platform packages (`dimcode-<platform>`) and `dimcode-web` are shared; each wrapper pins their exact version. Each channel has its own dist-tag and upgrades only within itself:

| Version | dist-tag | Install |
| --- | --- | --- |
| `X.Y.Z` | `latest` | `npm i -g dimcode` |
| `X.Y.Z-beta.N` | `beta` | `npm i -g dimcode@beta` (TypeScript) |
| `X.Y.Z-alpha.N` | `alpha` | `npm i -g dimcode-alpha@alpha`, run `dima` (Rust) |

Alphas are built and published from the maintainer's Mac in one command, like the TypeScript CLI's `publish:bin`. Beta and stable releases come only from a published GitHub Release ([dim-release](../../.agents/skills/dim-release/SKILL.md)); `publish-npm.mjs --publish-local` refuses them. The human-readable walkthrough is [docs/release/alpha.html](../../docs/release/alpha.html); keep both in step.

## The command

```sh
node scripts/publish-alpha.mjs                # next alpha
node scripts/publish-alpha.mjs 0.1.0-alpha.3  # explicit version
node scripts/publish-alpha.mjs --dry-run      # build and validate only
node scripts/publish-alpha.mjs --platform darwin-arm64  # only these platforms (repeatable)
```

In order, it:

1. Reads the registry and picks the version: the highest alpha of any of `dimcode-alpha`, `dimcode-web`, the six platform packages or the older `dimcode`, plus one, else `<Cargo version>-alpha.0`. A version that was only partly published is therefore never reused.
2. Refuses an alpha above the newest beta (see below), a version already on npm, local changes (except in a dry run) and less than 8 GiB free disk.
3. Sets every version field (`Cargo.toml`, `Cargo.lock`, `npm/package.json` and its optional dependencies, `runtime/package.json`, `runtime/package-lock.json`) for the build only and restores them when the run ends, successful or not. Nothing is committed; the packages record `HEAD` as their source commit.
4. Builds all six platforms: macOS with cargo, Linux (glibc 2.28) and Windows (`*-windows-gnu`/`gnullvm`) with cargo-zigbuild. With `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` set, each target is the account build; each platform package holds one executable. Each is staged and packed with `stage-release.mjs` / `pack-release.mjs`, then collected and validated with `collect-release.mjs`.
5. Publishes with `publish-npm.mjs --publish-local`: platform packages first, `dimcode-alpha` last, all on `alpha`. It then prints the dist-tags of `dimcode-alpha`.

## Running it for the user

- Run `--dry-run` yourself when asked to check a release; it builds for a long time, so run it in the background. It needs no credentials and writes nothing to npm or git.
- The real publish writes to npm with the user's login and per-package OTP prompts. Hand the user the command to run in their own terminal rather than running it yourself, unless they explicitly ask you to and are already logged in (`npm whoami`).
- Without the two client IDs the alpha has no Dim account and supports API-key providers only. Say so when it matters to the user.
- A run leaves the checkout as it found it: `git status` is clean afterwards and there is nothing to push.

## Constraint: stay below the newest beta

Installed TypeScript betas offer an upgrade to the highest prerelease of any identifier. An alpha above the newest beta would pull beta users onto the Rust CLI. Both `publish-alpha.mjs` (before building) and `publish-npm.mjs` (before writing) refuse such an alpha. Never weaken the check to get a release out; choose a lower version and tell the user why. An alpha with the same `X.Y.Z` as the newest beta is fine, since `alpha` sorts before `beta`.

## Disk space

The disk is small. The script keeps one target's build at a time: it deletes `target/<triple>` after packing each platform, drops staging intermediates as soon as they are consumed, deletes every `staging/alpha-*` at start, and at the end, successful or not, removes `staging/alpha-VERSION`, each `target/<triple>` it built and an empty `staging/`. Only a dry run keeps its packages, for inspection. At the end of every run, successful or not, it runs `mbx gc` when mbx is installed (`target/` is then an mbx-managed link), trimming the shared compile cache and stale targets to mbx's budgets. The script removes nothing else from the host `target/debug` and `target/release`; suggest `cargo clean` to the user when space is still short instead of deleting them yourself.

## Recovery

- Any failure: rerun the same command. It rebuilds from scratch under the next version; a partly published version is abandoned.
- "must stay below the newest dimcode beta": pick a lower alpha explicitly.
- A platform fails to build: fix the cause and rerun. Publish a subset only when the user asks for it with `--platform`: the wrapper then references exactly those platform packages, and other platforms get the launcher's missing-package error. `publish-npm.mjs` requires all six for beta and stable.
