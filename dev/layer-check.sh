#!/usr/bin/env bash
# Usage: dev/layer-check.sh
# Checks the layer rules a script can check (ADR-0002, #147). The model
# layer (crates/model) is synchronous and does no I/O:
# - its direct dependencies (normal and build) are on the allowlist below;
#   dev-dependencies are left out, as they never reach the layer's code;
# - its sources and tests name no `async fn`, `kube::Client`,
#   `astrolabe_kube::Cluster` or `cluster::Cluster` (the live handle,
#   whichever way it is reached; `ClusterId` and other value types are
#   fine) and read no clock (`::now(`).
# The UI layer (crates/app) does not depend on tokio: the cluster runtime
# is `astrolabe-kube`'s alone (#159). Its sources and tests name no
# `kube::Client`, and only the I/O seam (crates/app/src/cluster_io.rs)
# names the live `Cluster` handle, so no view holds a client (#161).
# Its sources build no `ClusterId` from a name: ids come from
# `astrolabe_kube::clusters()`, so a second source leaves the app as it is
# (#162). Its tests may, to name scripted clusters. An alias of `ClusterId`
# slips past.
# The search is line-based: it catches a full path or a one-line
# `use a::{.., X, ..}`, not a `use` list split over several lines. Comment
# lines are skipped, so a doc comment may name a forbidden item.
set -euo pipefail

model_allowlist=(astrolabe-kube serde_json serde-saphyr regex cronspeak thiserror tracing)
model_dir=crates/model
app_dir=crates/app
app_seam=crates/app/src/cluster_io.rs

status=0

# Prints each direct non-dev dependency of the named package; fails when
# the workspace has no such package.
direct_deps() {
    cargo metadata --format-version 1 --no-deps |
        jq -r --arg pkg "$1" '[.packages[] | select(.name == $pkg)]
            | if length == 1 then .[0] else error("no package \($pkg)") end
            | .dependencies[] | select(.kind != "dev") | .name' |
        sort -u
}

# Fails when a non-comment line under <dir> matches the extended regex.
# Lines in the optional <allowed> file are not checked.
forbid() {
    local dir=$1 pattern=$2 what=$3 allowed=${4:-} hits
    hits=$(grep -rnE --include='*.rs' "$pattern" "$dir" |
        grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' |
        { if [[ -n $allowed ]]; then grep -vF "$allowed:" || true; else cat; fi } || true)
    if [[ -n $hits ]]; then
        echo "$hits"
        echo "$dir must not name $what." >&2
        status=1
    fi
}

# Captured first, so that a metadata failure stops the script under -e.
model_deps=$(direct_deps astrolabe-model)
for dep in $model_deps; do
    if [[ " ${model_allowlist[*]} " != *" $dep "* ]]; then
        echo "astrolabe-model depends on $dep, which is not on the allowlist" \
            "(${model_allowlist[*]})." >&2
        status=1
    fi
done

app_deps=$(direct_deps astrolabe)
if grep -qx tokio <<<"$app_deps"; then
    echo "astrolabe depends on tokio; cluster I/O runs on astrolabe-kube's" \
        "runtime, reached through Cluster." >&2
    status=1
fi

# `path::X` or `path::{.., X, ..}` on one line.
names() { echo "\\b$1::(\\{[^}]*\\b)?$2\\b"; }

forbid "$model_dir" '\basync[[:space:]]+fn\b' '`async fn`'
forbid "$model_dir" "$(names kube Client)" '`kube::Client`'
forbid "$model_dir" "$(names '(astrolabe_kube|cluster)' Cluster)" '`astrolabe_kube::Cluster`'
forbid "$model_dir" '::now[[:space:]]*\(' 'the clock (`::now(`); pass the time in'

forbid "$app_dir" "$(names kube Client)" '`kube::Client`; requests go through Session'
forbid "$app_dir" "$(names '(astrolabe_kube|cluster)' Cluster)" \
    "\`astrolabe_kube::Cluster\` outside $app_seam; views go through Session" "$app_seam"
forbid "$app_dir/src" '\bClusterId[[:space:]]*::[[:space:]]*named\b' \
    '`ClusterId::named`; list Clusters with `astrolabe_kube::clusters()`'

exit $status
