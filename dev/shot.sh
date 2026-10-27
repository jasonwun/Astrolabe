#!/usr/bin/env bash
# Usage: dev/shot.sh <dir> <name> [orca computer action args...]
# Runs the orca action against astrolabe (get-app-state when none is given),
# keeps the Retina original as <dir>/<name>-orig.png for the PR, and writes
# <dir>/<name>.png at one pixel per window point for reading. Prints the
# latter path.
set -euo pipefail

dir=$1 name=$2
shift 2
if [ $# -eq 0 ]; then set -- get-app-state; fi
mkdir -p "$dir"

json=$(orca computer "$@" --app astrolabe --json)
path=$(jq -r .result.screenshot.path <<<"$json")
scale=$(jq -r .result.screenshot.scale <<<"$json")

cp "$path" "$dir/$name-orig.png"
width=$(sips -g pixelWidth "$path" | awk '/pixelWidth/ {print $2}')
sips --resampleWidth $((width / scale)) "$path" --out "$dir/$name.png" >/dev/null
echo "$dir/$name.png"
