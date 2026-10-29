#!/usr/bin/env bash
# Installs the toolchain that builds the dimcode under test into one private
# directory on the eval machine. Nothing outside DIM_EVAL_TOOLCHAIN changes, so
# other projects on the machine keep their own Rust, Zig and Node.
#
# Usage: benchmarks/host/setup-toolchain.sh [DIR]   (default $DIM_EVAL_HOME/toolchain)
set -euo pipefail

root="${1:-${DIM_EVAL_TOOLCHAIN:-${DIM_EVAL_HOME:-$HOME/dim-eval}/toolchain}}"
repo="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$root"
export RUSTUP_HOME="$root/rustup" CARGO_HOME="$root/cargo"

if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init \
    -o "$root/rustup-init"
  chmod +x "$root/rustup-init"
  "$root/rustup-init" -y --no-modify-path --profile minimal --default-toolchain none
  rm "$root/rustup-init"
fi

# cargo-zigbuild links against glibc 2.28 the way scripts/build-all.mjs does, so
# the binary runs in every task image, not only in ones as new as this host.
if [ ! -x "$root/zig/bin/python" ]; then
  python3 -m venv "$root/zig"
fi
"$root/zig/bin/pip" install --quiet 'ziglang==0.14.1'
# The pinned toolchain builds dimcode; rustup also installs a newer pin on
# demand when main moves rust-toolchain.toml.
pinned="$(sed -n 's/^channel = "\(.*\)"/\1/p' "$repo/rust-toolchain.toml")"
"$CARGO_HOME/bin/rustup" toolchain install "$pinned" --profile minimal \
  --target x86_64-unknown-linux-gnu
"$CARGO_HOME/bin/rustup" default "$pinned"
"$CARGO_HOME/bin/cargo" install --locked --quiet cargo-zigbuild --version 0.20.1

# scripts/stage-plugins.mjs stages the release's default plugins with the Node
# the repository pins.
node_version="$(cat "$repo/.node-version")"
if [ "$("$root/node/bin/node" --version 2>/dev/null)" != "v$node_version" ]; then
  rm -rf "$root/node"
  mkdir -p "$root/node"
  curl -sSfL "https://nodejs.org/dist/v$node_version/node-v$node_version-linux-x64.tar.xz" \
    | tar -xJ --strip-components=1 -C "$root/node"
fi

echo "toolchain ready in $root"
