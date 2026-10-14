---
name: dim-release
description: Prepare and deliver Dim releases in this repository. Use for product version changes, release branches or PRs, version tags, self-hosted release builds, draft GitHub Releases, manual npm publication from a published Release, or recovery of an incomplete release. npm publication is disabled until explicitly activated.
---

# Dim release

Read [release packaging](../../../docs/release/README.md) for runner prerequisites, platform artifacts and plugin configuration. The workflow is `.github/workflows/release.yml`; CI is `.github/workflows/ci.yml`.

## Prepare

- Inspect the working tree, current branch, `origin/main`, tags and existing GitHub Releases before changing release metadata. Fetch `origin/main` and tags. Preserve unrelated local work.
- Select the version requested by the user; if none was supplied, ask for it. Use `vX.Y.Z` or a prerelease such as `vX.Y.Z-beta.1`. `0.1.0` is a development baseline, not evidence of an unused npm version.
- Prepare on `codex/release-vVERSION` from `origin/main`, or continue the corresponding existing branch. Use a normal PR into `main`; this repository has no separate release integration branch or special merge command.
- Set the same version in `Cargo.toml` under `[workspace.package]`, `npm/package.json` and every exact optional dependency, `runtime/package.json`, and both root version fields in `runtime/package-lock.json` (`version` and `packages[""].version`). Refresh the workspace package versions in `Cargo.lock` with `cargo metadata --format-version 1 > /dev/null`; review the lockfile for unrelated dependency changes.
- Summarize user-visible changes from commits since the previous tag for the PR and draft release notes. Do not add unrelated features or release infrastructure.

## Verify and merge

- Run the applicable checks from `AGENTS.md`, reusing unchanged evidence. Metadata-only changes need version consistency and locked Cargo metadata checks. Run `node --test scripts/*.test.mjs` when changing packaging scripts. The PR check must succeed before tagging.
- Commit in English following repository conventions. When no stronger convention exists, use `chore: prepare vVERSION`.
- Push the branch and create or update its PR when the user has requested that delivery. Merge only within the user's authorization. Never force-push `main`.

## Build the draft

- Create an annotated `vVERSION` tag on the verified release commit already reachable from `origin/main`. The tag, Cargo version, wrapper, optional dependencies and runtime metadata must agree.
- Pushing the tag triggers release CI. Treat a tag push as a release action and perform it only within the user's request. For an existing tag, use `gh workflow run release.yml --ref main -f tag=vVERSION`; do not dispatch while the tag-triggered run is active.
- Watch the run with `gh run watch RUN_ID --exit-status`. All jobs use self-hosted runners. Cross-built macOS x64, Linux ARM64 and Windows ARM64 artifacts receive architecture checks; these checks do not demonstrate target-system runtime acceptance.
- CI collects six native archives and seven npm tarballs, validates their version/source/toolchain/plugin provenance and SHA-256, and attaches all thirteen original archives, `release-manifest.json` and `SHA256SUMS` to a draft GitHub Release. Prerelease tags create prerelease drafts. npm registry writes remain disabled.
- Inspect the draft with `gh release view vVERSION --json url,isDraft,tagName,assets`. Confirm all fifteen attachment names and sizes, check native archive digests against `SHA256SUMS` and npm tarball digests against the manifest, and review generated notes. Publish the GitHub draft only when requested; npm publication requires it to be non-draft first.
- Actions artifacts expire after one day. All original archives persist as GitHub Release attachments, so later npm publication does not depend on Actions retention.

## Publish npm packages

- `.github/workflows/npm-publish.yml.disabled` stays disabled until package ownership and trusted publishers for all seven packages are configured and the user explicitly requests activation.
- After activation, dispatch only when the user requests npm publication: `gh workflow run npm-publish.yml --ref main -f release_tag=vVERSION`. Use the tag of an already published, non-draft GitHub Release. There is no automatic publish trigger and no build-run input.
- The public Ubuntu runner downloads the original npm tarballs and manifest from that Release. It rechecks published status and tag identity, then validates exact packages, version, source SHA and digests before writing to npm. Never rebuild, repack or substitute packages during publication.
- Platform packages publish before `dimcode`; prereleases use `next`, stable versions use `latest`. An existing version is skipped only when its registry integrity exactly matches the original archive. Different bytes, registry authentication errors and network failures stop publication.
- The publication receipt records the Release ID, tag and package digests as a one-day Actions artifact. It does not modify the published GitHub Release.

## Recovery

- Fix build failures on the release preparation branch and use a new version tag if the source changes. Never move an existing release tag or overwrite published assets without an explicit request.
- A failed build creates no draft. Rerun failed jobs while successful artifacts are still available; after one day rerun all jobs or dispatch a complete build for the unchanged tag.
- An upload failure may leave an incomplete draft. The workflow rejects an existing release instead of replacing attachments. Inspect it, then delete only that incomplete draft when authorized before retrying. Keep its tag.
- Missing runner labels or cross toolchains must be fixed on the self-hosted fleet; never fall back to GitHub-hosted release runners. Plugin provenance mismatches require the same immutable archive URL and SHA-256 across all platforms before rebuilding.
- If an older published Release lacks its original npm tarballs or manifest, stop npm publication. Do not rebuild packages to fill the gap; prepare a new release containing the complete original artifacts.
- Do not rename or enable `.github/workflows/npm-publish.yml.disabled` during ordinary release work. Enabling npm requires package ownership and trusted publishers for all seven packages, and a separate user request.
