#!/usr/bin/env bash
# Usage: dev/resize.sh <width> <height>
# Resizes this worktree's astrolabe window to <width> x <height> window
# points and brings it to the front. Targets the app by pid: with another
# worktree's astrolabe running, `tell process "astrolabe"` resizes
# whichever one System Events finds first (#98).
set -euo pipefail

width=$1 height=$2
top=$(git rev-parse --show-toplevel)
pid=$("$top/dev/shot.sh" --pid)
osascript \
    -e "tell application \"System Events\"" \
    -e "tell (first process whose unix id is $pid)" \
    -e 'set frontmost to true' \
    -e "set size of window 1 to {$width, $height}" \
    -e 'end tell' -e 'end tell' >/dev/null
