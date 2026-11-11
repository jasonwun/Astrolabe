#!/usr/bin/env bash
# Usage: dev/shot.sh <dir> <name> [--crop <x>,<y>,<w>,<h>] [orca computer action args...]
#        dev/shot.sh --pid
# Runs the orca action (get-app-state when none is given) against the
# astrolabe started from this worktree, keeps the Retina original as
# <dir>/<name>-orig.png for the PR, and writes <dir>/<name>.png at one
# pixel per window point for reading, cropped to the --crop rectangle (in
# window points) when given. Prints the latter path.
# --pid prints that astrolabe's pid. Other worktrees run their own
# astrolabe, so targeting the process name would drive theirs.
# ASTROLABE_PID=<pid> targets that process instead, for an astrolabe not
# started from this worktree, such as the .app launched from Finder
# (working directory /).
set -euo pipefail

# ASTROLABE_PID, else the astrolabe whose working directory is inside this
# worktree.
own_pid() {
    if [ -n "${ASTROLABE_PID:-}" ]; then
        echo "$ASTROLABE_PID"
        return
    fi
    local top pid cwd found=()
    top=$(git rev-parse --show-toplevel)
    for pid in $(pgrep -x astrolabe || true); do
        cwd=$(lsof -a -d cwd -p "$pid" -Fn 2>/dev/null | sed -n 's/^n//p')
        if [[ $cwd == "$top" || $cwd == "$top"/* ]]; then
            found+=("$pid")
        fi
    done
    if [ ${#found[@]} -ne 1 ]; then
        echo "expected one astrolabe started from $top, found ${#found[@]}" >&2
        exit 1
    fi
    echo "${found[0]}"
}

# pixels Width|Height <png>
pixels() {
    sips -g "pixel$1" "$2" | awk "/pixel$1/ {print \$2}"
}

if [ "${1:-}" = --pid ]; then
    own_pid
    exit
fi

dir=$1 name=$2
shift 2
crop=
if [ "${1:-}" = --crop ]; then
    crop=$2
    shift 2
fi
if [ $# -eq 0 ]; then set -- get-app-state; fi
mkdir -p "$dir"

pid=$(own_pid)
if ! json=$(orca computer "$@" --app "pid:$pid" --json) || [ "$(jq -r .ok <<<"$json")" != true ]; then
    # Say why, rather than exit 1 silently and leave the last capture
    # looking like this one's (#96).
    jq -r '.error.message // .' <<<"$json" >&2
    if [ "$(jq -r .error.code <<<"$json")" = window_not_focused ]; then
        # A macOS permission dialog holds the focus (#96).
        osascript -e 'tell application "System Events" to tell process "UserNotificationCenter" to get value of static text 1 of every window' >&2 2>/dev/null || true
    fi
    exit 1
fi
path=$(jq -r .result.screenshot.path <<<"$json")
scale=$(jq -r .result.screenshot.scale <<<"$json")

cp "$path" "$dir/$name-orig.png"
if [ -n "$crop" ]; then
    IFS=, read -r x y w h <<<"$crop"
    if [ "$x" -eq 0 ] && [ "$y" -eq 0 ]; then
        # sips centres a crop whose offset is 0 0, so crop the opposite
        # corner of the image turned 180 degrees, then turn it back.
        sips -r 180 "$path" --out "$dir/$name.png" >/dev/null
        sips -c $((h * scale)) $((w * scale)) \
            --cropOffset $(($(pixels Height "$path") - h * scale)) $(($(pixels Width "$path") - w * scale)) \
            "$dir/$name.png" >/dev/null
        sips -r 180 "$dir/$name.png" >/dev/null
    else
        sips -c $((h * scale)) $((w * scale)) --cropOffset $((y * scale)) $((x * scale)) \
            "$path" --out "$dir/$name.png" >/dev/null
    fi
    sips --resampleWidth "$w" "$dir/$name.png" >/dev/null
else
    width=$(pixels Width "$path")
    sips --resampleWidth $((width / scale)) "$path" --out "$dir/$name.png" >/dev/null
fi
echo "$dir/$name.png"
