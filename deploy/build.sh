#!/usr/bin/env bash
set -euo pipefail

# The archive binds all source inputs to the revision recorded in the image.
if [[ $# -ne 2 || ! $1 =~ ^[0-9a-f]{40}$ || -z $2 ]]; then
  printf 'Usage: %s FULL_COMMIT_SHA IMAGE_TAG\n' "$0" >&2
  exit 2
fi
source_revision=$1
image_tag=$2
repository=$(git -C "$(dirname "${BASH_SOURCE[0]}")/.." rev-parse --show-toplevel)
resolved_revision=$(git -C "$repository" rev-parse --verify "$source_revision^{commit}")
if [[ $resolved_revision != "$source_revision" ]]; then
  printf 'Source must name an exact commit.\n' >&2
  exit 2
fi
build_root=$(mktemp -d)
trap 'rm -rf -- "$build_root"' EXIT
git -C "$repository" archive --format=tar "$source_revision" | tar -xf - -C "$build_root"
test -f "$build_root/deploy/Dockerfile"
docker build --platform linux/amd64 --target runtime \
  --build-arg "BOKKIE_SOURCE=$source_revision" \
  --iidfile "$build_root/image-id" \
  --file "$build_root/deploy/Dockerfile" --tag "$image_tag" "$build_root"
printf 'source_revision=%s\nimage_id=%s\n' "$source_revision" "$(cat "$build_root/image-id")"
