#!/usr/bin/env bash
# Usage: dev/scroll.sh <x> <y> <up|down> <lines>
# Scrolls this worktree's astrolabe at window point (x, y) by <lines>
# wheel lines, by posting real scroll-wheel events: `orca computer scroll`
# moves no GPUI scroll view. Builds dev/scroll.swift into target/dev/ on
# first use. Capture the result with `dev/shot.sh <dir> <name>`.
set -euo pipefail

x=$1 y=$2 direction=$3 lines=$4
case $direction in
    up) ;;
    down) lines=$((-lines)) ;;
    *) echo "direction must be up or down, got $direction" >&2; exit 1 ;;
esac

top=$(git rev-parse --show-toplevel)
bin=$top/target/dev/scroll
if [ ! -x "$bin" ] || [ "$top/dev/scroll.swift" -nt "$bin" ]; then
    mkdir -p "$(dirname "$bin")"
    swiftc -O -o "$bin" "$top/dev/scroll.swift"
fi

# Window points to screen points: offset by the window's position.
pid=$("$top/dev/shot.sh" --pid)
read -r wx wy < <(osascript -e "tell application \"System Events\" to \
get position of window 1 of (first process whose unix id is $pid)" | tr -d ,)
"$bin" $((wx + x)) $((wy + y)) "$lines"
