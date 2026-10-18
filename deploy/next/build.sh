#!/usr/bin/env bash
# Builds the bokkie-next image from one exact Bokkie commit. See README.md.
#
#   deploy/next/build.sh [commit]        default: HEAD of this checkout
#
# The commit must be on origin/main (fetched first), because only merged
# commits are deployed; BOKKIE_NEXT_ALLOW_UNMERGED=1 lifts that for a local
# trial build. The image is built from the commit's own tree, exported with
# `git archive`, so local changes and untracked files never reach it. It is
# tagged bokkie-next:<full commit> and carries the commit as its
# org.opencontainers.image.revision label. The last line printed is the
# image ID.
#
# CONTAINER_ENGINE names the build command (default: docker).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
engine="${CONTAINER_ENGINE:-docker}"

die() {
  echo "build: $*" >&2
  exit 1
}

revision="$(git -C "$root" rev-parse --verify "${1:-HEAD}^{commit}")" ||
  die "no such commit: ${1:-HEAD}"
if [[ "${BOKKIE_NEXT_ALLOW_UNMERGED:-}" != 1 ]]; then
  git -C "$root" fetch --quiet origin main
  git -C "$root" merge-base --is-ancestor "$revision" origin/main ||
    die "$revision is not on origin/main; deploy only merged commits"
fi

context="$(mktemp -d)"
trap 'rm -rf -- "$context"' EXIT
git -C "$root" archive --format=tar "$revision" | tar -x -C "$context"

tag="bokkie-next:$revision"
echo "build: building $tag" >&2
"$engine" build --file "$context/deploy/next/Dockerfile" \
  --build-arg "BOKKIE_SOURCE=$revision" --tag "$tag" "$context" >&2
label="$("$engine" image inspect --format \
  '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$tag")"
[[ "$label" == "$revision" ]] || die "$tag is labelled $label, not $revision"
"$engine" image inspect --format '{{.Id}}' "$tag"
