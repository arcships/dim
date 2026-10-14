#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 1 || -e "$1" ]]; then
  echo 'Usage: scripts/export-public-source.sh NEW_OUTPUT.tar.gz' >&2
  exit 1
fi
repository=$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)
if [[ -n "$(git -C "$repository" status --porcelain --untracked-files=normal)" ]]; then
  echo 'Commit or resolve working-tree changes before exporting HEAD.' >&2
  exit 1
fi
output=$1
if [[ "$output" != /* ]]; then output="$PWD/$output"; fi
git -C "$repository" archive --format=tar.gz --output="$output" HEAD \
  .gitignore .env.example .node-version .github .agents AGENTS.md CLAUDE.md dev Cargo.toml Cargo.lock rust-toolchain.toml LICENSE README.md \
  crates tests vendor xtask npm runtime scripts docs/INDEX.md docs/architecture docs/design docs/release
