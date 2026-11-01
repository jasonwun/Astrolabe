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

# Window points to screen points: offset by the window's position. The
# wheel goes to whichever window is under the pointer, so bring this one to
# the front first; another worktree's window may cover it (#98).
pid=$("$top/dev/shot.sh" --pid)
osascript -e "tell application \"System Events\" to \
set frontmost of (first process whose unix id is $pid) to true"
read -r wx wy < <(osascript -e "tell application \"System Events\" to \
get position of window 1 of (first process whose unix id is $pid)" | tr -d ,)
"$bin" $((wx + x)) $((wy + y)) "$lines"
