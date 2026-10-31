#!/usr/bin/env bash
# Usage: dev/registration-path.sh
# Fails when a test under crates/app/tests calls a projection function a
# built-in kind's Registration wraps (a `project_*` or `*_detail` named in
# crates/app/src/builtin.rs). Such tests call
# `BuiltinKind::X.registration().list` or `.detail` with a `DynamicObject`
# instead (CODING_STANDARDS "Tests that claim the Registration's path").
set -euo pipefail

names=$(grep -oE '\b(project_[a-z_]+|[a-z_]+_detail)\b' crates/app/src/builtin.rs |
    sort -u | paste -sd'|' -)
hits=$(git ls-files -z 'crates/app/tests/*.rs' |
    xargs -0 grep -nE "\b($names)\(" |
    grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)

if [[ -n $hits ]]; then
    echo "$hits"
    echo "Call BuiltinKind::X.registration().list/.detail instead." >&2
    exit 1
fi
