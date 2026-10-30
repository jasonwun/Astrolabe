#!/usr/bin/env bash
# Usage: dev/registration-path.sh
# Fails when a test under crates/app/tests calls a projection function a
# built-in kind's Registration wraps (a `project_*` or `*_detail` named in
# crates/app/src/builtin.rs). Such tests call
# `BuiltinKind::X.registration().list` or `.detail` with a `DynamicObject`
# instead (CODING_STANDARDS "Tests that claim the Registration's path").
set -euo pipefail

# Files that still call the projection directly, until #121 sweeps them.
# A listed file that no longer does fails the check: remove it here.
pending=(
    crates/app/tests/autoscaling.rs
    crates/app/tests/column_widths.rs
    crates/app/tests/config.rs
    crates/app/tests/endpoint_slices.rs
    crates/app/tests/events.rs
    crates/app/tests/network.rs
    crates/app/tests/quotas.rs
    crates/app/tests/workloads.rs
    crates/app/tests/workloads_p3.rs
)

names=$(grep -oE '\b(project_[a-z_]+|[a-z_]+_detail)\b' crates/app/src/builtin.rs |
    sort -u | paste -sd'|' -)
hits=$(git ls-files -z 'crates/app/tests/*.rs' |
    xargs -0 grep -nE "\b($names)\(" |
    grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)

bad=0
while IFS= read -r hit; do
    [[ -z $hit ]] && continue
    file=${hit%%:*}
    if [[ ! " ${pending[*]} " == *" $file "* ]]; then
        echo "$hit"
        bad=1
    fi
done <<<"$hits"
if ((bad)); then
    echo "Call BuiltinKind::X.registration().list/.detail instead." >&2
fi

for file in "${pending[@]}"; do
    if ! grep -q "^$file:" <<<"$hits"; then
        echo "$file no longer calls a projection: remove it from pending" >&2
        bad=1
    fi
done
exit "$bad"
