# Container image release

Status: Draft. Issue: #1348.

Scope: release packaging of the Linux executable. This document adds no plugin, part, service or
Timeline event.

## 1. Design

### 1.1 Artifact

| Item | Value |
| --- | --- |
| Repository | `ghcr.io/arcships/dim` |
| Immutable tag | `X.Y.Z-alpha.N`, the release version without `v` |
| Moving tag | `alpha`, moved only after both platforms accept the version index |
| Platforms | `linux/amd64`, `linux/arm64` in one index |
| Pin | CI uses the index digest: `ghcr.io/arcships/dim@sha256:…` |

The image is assembled from the published Release's Linux and Web archives. Its executable bytes
match the npm and native-archive channels.

### 1.2 Contents

| Path | Content |
| --- | --- |
| `/opt/dim/bin/dimcode` | The released executable |
| `/opt/dim/plugins/`, `default-plugins.json` | The release's default plugins and configuration, unchanged |
| `/opt/dim/web/` | The release's Web workspace |
| `/opt/dim/release-manifest.json`, `web-release-manifest.json` | Archive manifests |
| `/opt/dim/LICENSE`, `NOTICE`, `licenses/` | Distribution notices |
| `/usr/local/bin/dim`, `dimcode` | Symlinks to the executable |
| `/workspace` | Working directory, owned by `dim` |
| `/home/dim` | `HOME`; product home, plugins, sessions, runtimes |

Base image: `debian:bookworm-slim`, pinned by digest. System packages: `git`, `ca-certificates`,
`tini`. User: `dim`, UID/GID 1000. `tini` is PID 1 and reaps agent background processes.

Node and Python runtimes use the release's default configuration (`download`, `auto: false`) and are
installed into `/home/dim` by `dimcode runtime install node|python`.

### 1.3 Usage

The client is the container's main process; the daemon ends with the container. One foreground
client per container. The client waits for the current turn to settle before it exits.

| Use | Command |
| --- | --- |
| TUI | `docker run --rm -it -v "$PWD:/workspace" -v dim-home:/home/dim IMAGE` |
| One-shot | `docker run --rm -v "$PWD:/workspace" -v dim-home:/home/dim IMAGE exec -p TEXT` |
| ACP | `docker run -i --rm -v "$PWD:/workspace" -v dim-home:/home/dim IMAGE acp` |
| Web (Linux) | `docker run --rm --network host -v "$PWD:/workspace" -v dim-home:/home/dim IMAGE web --no-open` |

Web listens on `127.0.0.1`, so the container shares the host network. Model credentials come from
`--env-file`/`-e`; a catalog file uses `--model-config`. The product home is
`/home/dim/.dimcode-alpha`. One home volume has one container writer. For a host workspace owned by
another UID, run with `--user "$(id -u):$(id -g)"` and a writable home.

### 1.4 Publication

1. Require a published, non-draft alpha Release.
2. Check out the release tag; download the Linux and Web archives and `SHA256SUMS`.
3. Verify archive names, `SHA256SUMS`, the internal manifests, version, source commit, platform and
   Web protocol; before extraction, reject symlinks, hard links, special files and members outside
   the archive root.
4. Build and push the two-platform index with provenance and an SBOM; record its index digest.
5. Pull that exact digest on native amd64 and arm64 runners; check `dimcode --version`, the
   version/revision/source labels, UID 1000, both CLI symlinks and the Web assets.
6. Copy the accepted index to `alpha` and assert that `alpha` has the same digest.

`publish=false` builds the amd64 image locally and runs its checks without pushing. A version tag is
written once; an existing tag fails the run. The `build` and `alpha` jobs hold `packages: write`;
`accept` holds `packages: read`.

One-time setup: create the `image-release` environment with protection rules, then after the first
push make the GHCR package public and verify it is linked to `arcships/dim`. The first dry run
requires an alpha Release cut after this change; earlier tags contain neither the staging script nor
the Dockerfile.

### 1.5 Security

The container is the isolation boundary. The image runs as `dim` with ordinary container
permissions; the mounts are the workspace and the home volume. Dim's permission presets, approvals
and project-entry trust keep their normal behavior. Credentials stay in `--env-file`/`-e` or the
home volume; the image contains the runtime packages and release bytes.

## 2. Plan

### 2.1 Files

| File | Purpose |
| --- | --- |
| `scripts/stage-image.mjs` | Verify release archives and produce the Docker rootfs |
| `scripts/stage-image.test.mjs` | Flow tests for verification and layout |
| `docker/Dockerfile` | Base, system packages, user, rootfs, entrypoint |
| `.github/workflows/image-publish.yml` | Dry run, publish, accept and `alpha` promotion |
| `scripts/ci-changes.mjs` | Map `docker/**` to the scripts check group |

`stage-image.mjs` takes `--native ARCHIVE --web ARCHIVE --sums FILE --version VERSION
--source-commit SHA --platform linux-x64|linux-arm64 --output DIR` and writes
`DIR/rootfs-<arch>/opt/dim/…`.

### 2.2 Verification

| Check | Command |
| --- | --- |
| Staging | `node --test scripts/stage-image.test.mjs` |
| Script suite | `node --test scripts/*.test.mjs` |
| Documentation | `node scripts/adr-check.mjs --base <ref>` |
| Image | `image-publish.yml` with `publish=false` on a published alpha tag |

### 2.3 Order

1. The issue is #1348.
2. Add or change the staging script and its test.
3. Add the Dockerfile; build and run it locally.
4. Add the workflow and align this document.
5. Run the dry run on a published alpha tag.
6. Publish the image, then add the README install line and the release-skill entry.
