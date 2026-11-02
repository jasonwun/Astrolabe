#!/usr/bin/env bash
# Usage: dev/layer-check.sh
# Checks the layer rules a script can check (ADR-0002, #147). The model
# layer (crates/model) is synchronous and does no I/O:
# - its direct dependencies (normal and build) are on the allowlist below;
#   dev-dependencies are left out, as they never reach the layer's code;
# - its sources name no `async fn`, `kube::Client`, `astrolabe_kube::Cluster`
#   (the live handle; `ClusterId` and other value types are fine) and read
#   no clock (`::now(`).
# Comment lines are skipped, so a doc comment may name a forbidden item.
set -euo pipefail

model_allowlist=(astrolabe-kube serde_json regex cronspeak thiserror)
model_src=crates/model/src

status=0

# Prints each direct non-dev dependency of the named package.
direct_deps() {
    cargo metadata --format-version 1 --no-deps |
        jq -r --arg pkg "$1" '.packages[] | select(.name == $pkg)
            | .dependencies[] | select(.kind != "dev") | .name' |
        sort -u
}

# Fails when a non-comment line under <dir> matches the extended regex.
forbid() {
    local dir=$1 pattern=$2 what=$3 hits
    hits=$(grep -rnE --include='*.rs' "$pattern" "$dir" |
        grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)
    if [[ -n $hits ]]; then
        echo "$hits"
        echo "$dir must not name $what." >&2
        status=1
    fi
}

while read -r dep; do
    [[ -z $dep ]] && continue
    if [[ " ${model_allowlist[*]} " != *" $dep "* ]]; then
        echo "astrolabe-model depends on $dep, which is not on the allowlist" \
            "(${model_allowlist[*]})." >&2
        status=1
    fi
done < <(direct_deps astrolabe-model)

forbid "$model_src" '\basync[[:space:]]+fn\b' '`async fn`'
forbid "$model_src" '\bkube::Client\b' '`kube::Client`'
forbid "$model_src" '\bastrolabe_kube::Cluster\b' '`astrolabe_kube::Cluster`'
forbid "$model_src" '::now\(' 'the clock (`::now(`); pass the time in'

exit $status
