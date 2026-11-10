---
name: dim-release
description: Prepare Dim release versions and release PRs, or build and publish alpha releases through GitHub CI. Use for version bumps, next-alpha selection, release tags, GitHub Releases, npm OIDC publication and release recovery.
---

# Dim release

Use this skill for committed release metadata and the GitHub CI route. CI currently accepts only `X.Y.Z-alpha.N`; it does not publish the Rust CLI to the stable or beta channels. Use [dim-alpha-release](../../../skills/dim-alpha-release/SKILL.md) only when selecting the local Mac alternative.

Read [version preparation](../../../docs/release/README.md#version-preparation) before editing metadata and [alpha npm publication](../../../docs/release/README.md#alpha-npm-publication) before operating CI. Read runner prerequisites only when a build is requested.

## Requested stage

- A version bump or release-PR request ends with a reviewable PR. It does not authorize merging, tags, builds, GitHub Release publication or npm writes.
- A build request covers the GitHub-hosted build and draft inspection. npm and container-image publication are separate stages.
- An explicit end-to-end publication request may authorize all stages. Honor authorization already given; do not ask again at each stage.
- A container-image request covers its dry run and, with `publish=true`, GHCR publication; it does not authorize npm or GitHub Release changes.
- A skill review or configuration check never runs a release. `publish-alpha.mjs --dry-run` builds the targets; it is not a read-only version check.

## Prepare the next GitHub alpha from main

Dispatch `gh workflow run release.yml --ref main` for an authorized GitHub alpha build. Preflight verifies the wrapper and all seven dependency tarballs for the selected main SHA, including integrity, exact versions and account-enabled native provenance; a Git tag or matching Web source alone does not skip a build. If a complete nightly exists, a build-only dispatch stops with instructions. When tag writes, mirror sync and installation acceptance are authorized, add `-f repair_nightly=true` to reuse it without rebuilding; only successful acceptance reports no new build needed. Otherwise the workflow allocates the next alpha across npm packages and Git tags, stamps that version into the GitHub-hosted build checkouts, tags their source commit and uploads the original native and npm archives to the prerelease draft. Review the draft and publish it when authorized. npm publication is a separate manual dispatch against that published tag.

## Select the version for a version PR

1. Inspect the working tree, current branch, `origin/main`, remote tags, open release PRs and existing GitHub Releases. Fetch `origin/main` and tags without changing unrelated work. Continue an existing PR for the same preparation task rather than opening a duplicate. Resume the matching release branch or immutable Release when the request is a retry.
2. Read registry versions for `dimcode-alpha`, `dimcode-web`, every `dimcode-<platform>` in `npm/package.json`, and legacy `dimcode`. Use `npm view PACKAGE versions --json --registry=https://registry.npmjs.org`; only an explicit E404 means an absent package. Authentication or network failures do not mean a version is free.
3. Use an explicit requested version. For “next alpha” without a number, choose the highest alpha across those registry versions, reserved GitHub tags/Releases and other open release PRs, then increment its numeric `N`. If no alpha exists, use the Cargo workspace's `X.Y.Z-alpha.0`. Compare SemVer numerically: `alpha.10` follows `alpha.9`. Do not choose from a dist-tag or the committed Cargo version alone; a partial publication reserves its version too.
4. A new version must be absent from every package and remote tag/Release. An existing matching release is a retry, not a new version. An alpha must stay below the newest legacy `dimcode` beta; retain the packaging scripts' check. Do not change the release core the user explicitly selected.

## Prepare the version PR

- Work from `origin/main` on `dimagent/release-vVERSION`, or continue its corresponding branch. Use a normal PR into `main`; no separate release integration branch is needed.
- Synchronize the fields in the version-preparation table. Refresh `Cargo.lock` with `cargo metadata --format-version 1 --no-deps > /dev/null`; review its diff for unrelated dependency changes. Do not replace lockfile versions by text substitution or bump explicitly versioned crates such as `dim-agent-types`.
- Keep the npm template named `dimcode`, with its `dim`/`dimcode` bins. `collect-release.mjs` selects `dimcode-alpha` and `dima` when packaging an alpha. This is a product-version change, not a Node/Rutis/dependency upgrade.
- Export `DIM_RELEASE_VERSION` as the selected version without the leading `v`, then verify:

```sh
node --input-type=module <<'NODE'
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const version = process.env.DIM_RELEASE_VERSION;
assert.match(version, /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-alpha\.(0|[1-9]\d*)$/);
const json = file => JSON.parse(readFileSync(file, 'utf8'));
assert.equal(readFileSync('Cargo.toml', 'utf8').match(/\[workspace\.package\]\s+version = "([^"]+)"/)[1], version);
const wrapper = json('npm/package.json');
assert.equal(wrapper.version, version);
assert(Object.values(wrapper.optionalDependencies).every(value => value === version));
assert.equal(json('runtime/package.json').version, version);
const lock = json('runtime/package-lock.json');
assert.equal(lock.version, version);
assert.equal(lock.packages[''].version, version);
NODE
cargo metadata --format-version 1 --no-deps --locked --offline > /dev/null
git diff --check
```

- Metadata-only changes need these checks and the existing PR checks; they do not need six-platform builds or a repeated full local test suite. Reuse valid evidence under `AGENTS.md`.
- Summarize user-visible changes since the previous release for the PR and release notes. Commit as `chore: prepare vVERSION`. Push and create/update the PR when requested. Report the chosen version, source commit, changed metadata and verification; for preparation-only work, end here.

## Build and inspect the draft

- After the required CI passes and the release commit is on `origin/main`, create an annotated `vVERSION` tag on that exact commit. Merge and push the tag only within the requested stage; never force-push `main` or move a release tag.
- A tag push triggers `release.yml`. For an existing unchanged tag, use `gh workflow run release.yml --ref main -f tag=vVERSION` only when no run for it is active. Watch the existing run instead of dispatching duplicates.
- Build and draft assembly use the GitHub-hosted runner matrix in the release guide. Check the draft's seventeen attachments: seven native archives including Web, eight npm tarballs, `release-manifest.json` and `SHA256SUMS`. Verify names, sizes, native digests and npm manifest digests, and review release notes. Each native payload must report the selected version on its target runner.
- Publish the GitHub prerelease only within the authorized stage. npm publication requires a non-draft published Release. Original attachments persist independently of one-day Actions artifacts.

## Publish the original npm packages

- Confirm the eight packages' trusted publishers and the `npm-release` environment using the linked npm setup instructions. The `dimcode-alpha` publisher must separately allow `npm dist-tag`; for reusable recovery from `release.yml`, configure a second tag-only trusted publisher naming that calling workflow and the same environment. Only the hosted npm publication job has OIDC write permission. Do not start publication before these permissions are confirmed.
- `gh workflow run npm-publish.yml --ref main -f release_tag=vVERSION` performs read-only verification on hosted Ubuntu. Do not dispatch it just to review skill/configuration changes.
- Actual publication requires an explicit npm-publication request, `NPM_ALPHA_PUBLISH_ENABLED=true`, and the same command with `-f publish=true`. Keep the switch unset or `false` during setup and ordinary verification; do not silently enable it for a preparation request.
- The hosted job independently downloads the original tarballs and manifest from the GitHub Release and validates their bytes and registry state again. It snapshots protected tags using the Release's package set and publishes without building or repacking: six platform packages and `dimcode-web`, then `dimcode-alpha`, all on `alpha`. After completeness and provenance verification, it aligns only the wrapper's `latest` and `alpha` to that nightly. It never publishes `dimcode` or changes its tags, platform/Web non-alpha tags, or unrelated wrapper tags. Acceptance installs both default `dimcode-alpha` and `dimcode-alpha@alpha` with fresh prefixes/caches on npm and, after sync, npmmirror; both installed package versions and `dima --version` must match. Mirror lag fails acceptance. Retain the workflow's publication receipt.

## Publish the container image

- A published, non-draft Release is required; the workflow verifies that state before downloading assets. `gh workflow run image-publish.yml --ref main -f release_tag=vVERSION` builds both platform rootfs trees and the amd64 image on hosted Ubuntu, runs local acceptance, and pushes nothing. Create the `image-release` environment with protection rules and, after the first push, make the GHCR package public and link it to `arcships/dim`. The first dry run needs an alpha Release cut after the image workflow lands.
- Actual publication requires an explicit image-publication request and the same command with `-f publish=true`. The workflow checks out the release tag's Dockerfile and staging script, verifies the original Linux and Web archives against `SHA256SUMS`, their manifests and expected names, refuses an existing version tag, and pushes the two-platform index with provenance and an SBOM.
- Acceptance pulls that exact index digest on native amd64 and arm64 runners, checks `dimcode --version`, the version/revision/source labels, UID 1000, both CLI symlinks and the Web assets. Only then does it copy the accepted index to `alpha` and assert the digest is unchanged. A wrong image needs a new alpha; do not rebuild a version tag.

## Recovery

- For a complete nightly built from the exact main SHA selected by the dispatch, an authorized `gh workflow run npm-publish.yml --ref main -f source_commit=FULL_MAIN_SHA -f publish=true` repairs tags and runs acceptance without a GitHub Release or package publication. It fails if that SHA has no complete nightly. `release.yml -f repair_nightly=true` reuses this path when complete, but builds a draft otherwise. Both require `NPM_ALPHA_PUBLISH_ENABLED=true`; neither source-only input accepts older ancestor SHAs. Older published Releases use `release_tag`; an older nightly without a Release requires a new main build. This is the CI dual-tag nightly route; the explicit local publisher remains alpha-only and does not provide its acceptance contract.
- For an unchanged source/tag, resume its existing run, Release and original archives. A matching registry integrity may be skipped; different bytes or registry/authentication errors stop publication. Do not allocate a new version just because the publication job needs retrying.
- Source changes need a new version and new tag. Never rebuild or substitute missing archives in an already published Release; prepare a complete new release.
- After one-day build artifacts expire, rebuild all targets for the unchanged tag if no complete Release exists. An incomplete draft must be inspected and removed only when authorized before retrying; keep the tag.
- Verify hosted-runner workflow fixes by dispatching release.yml from the fix branch with the existing tag. Wait for all six builds and attachment assembly before merging. Then dispatch release.yml from main with the same tag. Check that the new run builds the original tag commit.
