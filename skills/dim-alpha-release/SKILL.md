---
name: dim-alpha-release
description: Choose and operate the Dim Rust alpha release route. Use for alpha preparation, next-alpha versions, local Mac alpha builds or recovery. GitHub CI is the default; local publication requires choosing that route.
---

# Dim alpha release

Rust alphas use `dimcode-alpha@alpha`, install only `dima`, and default to `~/.dimcode-alpha`. The legacy `dimcode` package installs `dim`; its stable/beta channels are not Rust release targets. The six platform packages and `dimcode-web` are shared names pinned to the exact release version.

## Choose the route

- **Version PR, GitHub CI or an unspecified alpha release:** follow [dim-release](../../.agents/skills/dim-release/SKILL.md). It owns version selection, committed metadata, release PRs/tags, draft inspection and guarded npm OIDC publication. A manual main build allocates and stamps the next version. A version PR uses committed release metadata. Both CI paths create a GitHub prerelease draft before the separate npm publication.
- **Explicit local Mac build/publication:** use `scripts/publish-alpha.mjs` below. It changes versions temporarily and restores them; it does not prepare a version PR or push a GitHub release tag. This alternative publishes only `alpha`: it does not align the wrapper's `latest`, sync the mirror or provide CI's dual-entry installation acceptance. Use the CI route for the dual-tag nightly contract; do not report a local alpha-only publish as equivalent.
- Do not switch routes to bypass a failure or missing npm authorization. Check the user's existing authorization once and complete only the requested stage.

The human walkthrough is [Rust alpha release](../../docs/release/alpha.html); CI/npm setup is [alpha npm publication](../../docs/release/README.md#alpha-npm-publication).

## Local Mac command

This route runs the same catalog step from `dim-release` first, on the same clean `main` checkout.

```sh
node scripts/publish-alpha.mjs                # build and publish next alpha
node scripts/publish-alpha.mjs 0.1.0-alpha.3  # build and publish an explicit version
node scripts/publish-alpha.mjs --dry-run      # build and validate without publishing
node scripts/publish-alpha.mjs --platform darwin-arm64  # explicit platform subset
```

- Read-only version review and skill/configuration work use registry/Git inspection through `dim-release`, not this command. Even `--dry-run` performs real builds and staging cleanup; run it only when local build verification is requested or authorized.
- The script reads versions across the alpha wrapper, Web, six platforms and legacy `dimcode`; it chooses the next alpha unless an explicit version is supplied. It refuses reused npm versions, an alpha above the newest legacy beta, a dirty publication checkout and less than 8 GiB free disk. A partial publication reserves the version. Never weaken these guards.
- The script temporarily updates `Cargo.toml`, `Cargo.lock`, the npm wrapper and exact optional dependencies, and the runtime package/lock root versions. It restores those files on success or failure. Unlike the CI route, its build version is not committed. Published packages record the source `HEAD`.
- Every release build uses `DIM_RELEASE_BUILD=1`. macOS targets use Cargo; Linux uses the glibc 2.28 Zig target and Windows uses GNU/gnullvm cross targets. Read [the local prerequisites](../../docs/release/alpha.html) before starting. Default plugins are staged once with `--portable`, then reused across platforms.
- With both `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL`, the script builds the Dim-account variant. Without both, it is API-key-only; a partial configuration fails. Never commit the IDs.
- Real local publication uses the user's npm login and per-package OTP prompts. By default give the user the local publish command to run in their terminal. Run it yourself only when explicitly asked and already logged in (`npm whoami`); an existing end-to-end authorization need not be requested again.
- Six platform packages and `dimcode-web` publish before `dimcode-alpha`, all on `alpha`. A subset is allowed only when requested with `--platform`; other platforms cannot install that release.

## Local cleanup and recovery

The script removes `staging/alpha-*` at startup, keeps one target build at a time and removes its staging/target intermediates on exit. Only a dry run retains its packages for inspection. When installed, `mbx gc` runs at the end. Do not add manual deletion of development build directories; the script owns its cleanup.

A new local invocation rebuilds and chooses the next unused version unless one is specified. After a partial local publish, abandon that version when using this command again. CI publication retries instead reuse their existing immutable Release archives under `dim-release`; the two recovery paths are different.

After an authorized local publish, verify the eight packages (or the requested subset plus Web/wrapper), `dimcode-alpha`'s `alpha` dist-tag, the unchanged legacy `dimcode` tags and the restored working tree. Do not turn that check into an unrequested install or model/TUI run against the product home.
