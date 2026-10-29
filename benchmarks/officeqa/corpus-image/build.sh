#!/usr/bin/env bash
# Build and push the OfficeQA Pro corpus image, and print the digest to pin.
#
#   OFFICEQA_ROOT=<hf snapshot of databricks/officeqa> \
#     benchmarks/officeqa/corpus-image/build.sh <registry>/officeqa-corpus:<tag>
#
# The digest printed last is what assemble.py takes as --corpus-image. A tag is
# not accepted there: it can be re-pushed, and a re-pushed corpus would move the
# measurement without moving the corpus pin.
set -euo pipefail

IMAGE="${1:?usage: OFFICEQA_ROOT=<dir> $0 <registry>/<name>:<tag>}"
: "${OFFICEQA_ROOT:?set OFFICEQA_ROOT to an HF snapshot of databricks/officeqa}"
SOURCE="$OFFICEQA_ROOT/treasury_bulletins_parsed/transformed"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

count=$(find "$SOURCE" -maxdepth 1 -name '*.txt' -type f | wc -l | tr -d ' ')
if [ "$count" -ne 697 ]; then
  echo "expected 697 bulletin files under $SOURCE, found $count" >&2
  exit 1
fi

rm -rf "$HERE/corpus"
mkdir "$HERE/corpus"
trap 'rm -rf "$HERE/corpus"' EXIT
cp "$SOURCE"/*.txt "$HERE/corpus/"

docker build -t "$IMAGE" "$HERE"
docker push "$IMAGE"
docker inspect --format '{{index .RepoDigests 0}}' "$IMAGE"
