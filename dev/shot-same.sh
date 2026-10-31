#!/usr/bin/env bash
# Usage: dev/shot-same.sh <before-dir> <after-dir> <name>...
# Compares the pixels of each <name>.png in the two directories, as
# dev/shot.sh wrote them, and prints `same` or `DIFF` per name. Exits 1
# when any pair differs. PNG bytes carry metadata, so each image is
# decoded to BMP first.
set -euo pipefail

before=$1 after=$2
shift 2
tmp=$(mktemp -d "${TMPDIR:-/tmp}/shot-same.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

status=0
for name in "$@"; do
    for side in before after; do
        dir=${!side}
        sips -s format bmp "$dir/$name.png" --out "$tmp/$name-$side.bmp" >/dev/null
    done
    if cmp -s "$tmp/$name-before.bmp" "$tmp/$name-after.bmp"; then
        echo "same $name"
    else
        echo "DIFF $name"
        status=1
    fi
done
exit $status
