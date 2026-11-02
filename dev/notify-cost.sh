#!/usr/bin/env bash
# Usage: dev/notify-cost.sh [out-dir]
# Measures the main-thread cost of Session's whole-entity notify (#106) on
# kind-astrolabe-dev; docs/agents/notify-cost.md explains the setup and
# the columns. Seeds this worktree's astrolabe-notify-<worktree>
# namespace, then for each PANELS x RATE launches astrolabe with that many
# tabs open, churns ConfigMaps at RATE/s and prints the mean of the perf
# reports logged over RUN_SECONDS of it, once churn is up to rate. Each run's log lands in out-dir
# (default: a fresh temp dir). Deletes the namespace at the end.
# Env: PANELS (default "1 5 20"), RATES ("0 10 100"), RUN_SECONDS (30),
# WARMUP (10), COUNT (300 seeded objects of each kind).
set -euo pipefail

panels_list=${PANELS:-1 5 20}
rates=${RATES:-0 10 100}
seconds=${RUN_SECONDS:-30}
warmup=${WARMUP:-10}
ramp=5
count=${COUNT:-300}
out=${1:-$(mktemp -d)}
mkdir -p "$out"

context=kind-astrolabe-dev
worktree=$(basename "$(git rev-parse --show-toplevel)" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-')
ns=$(cut -c1-63 <<<"astrolabe-notify-$worktree" | sed 's/-*$//')
kubectl=(kubectl --context "$context")

if ! "${kubectl[@]}" get --raw /readyz >/dev/null 2>&1; then
    echo "$context is unreachable; start its Docker (OrbStack: orb start)" >&2
    exit 1
fi

# Optimised, but with debug assertions so the ASTROLABE_* launch
# preselection (debug-only) still opens the tabs.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true
cargo build --release -q -p astrolabe
cargo build --release -q -p astrolabe-kube --example churn
app=target/release/astrolabe
churn=target/release/examples/churn

"${kubectl[@]}" create namespace "$ns" --dry-run=client -o yaml | "${kubectl[@]}" apply -f - >&2
trap '"${kubectl[@]}" delete namespace "$ns" --wait=false >&2' EXIT
"$churn" "$context" "$ns" seed "$count" >&2

# The ConfigMaps list (so their watch runs; watches stay resident), then
# the Secrets list in its place, then PANELS-1 detail tabs over the seeded
# Deployments, Secrets and Services; the last one opened is in front.
open_spec() {
    local spec=ConfigMap,Secret i
    for ((i = 1; i < $1; i++)); do
        case $((i % 3)) in
        1) spec+=",Deployment/d-$i" ;;
        2) spec+=",Secret/s-$i" ;;
        0) spec+=",Service/svc-$i" ;;
        esac
    done
    echo "$spec"
}

# Mean of each perf field over the report lines in a log excerpt.
summarize() {
    sed 's/\x1b\[[0-9;]*m//g' | grep 'main-thread cost' | awk '
        {
            for (i = 1; i <= NF; i++) if (split($i, kv, "=") == 2 && kv[2] ~ /^[0-9.]+$/) {
                sum[kv[1]] += kv[2]
                if (!(kv[1] in seen)) { seen[kv[1]] = 1; order[++n] = kv[1] }
                if (kv[2] > max[kv[1]]) max[kv[1]] = kv[2]
            }
            lines++
        }
        END {
            if (!lines) { print "no reports"; exit }
            printf "reports=%d", lines
            for (i = 1; i <= n; i++) printf " %s=%.2f", order[i], sum[order[i]] / lines
            printf " lag_max_ms(worst)=%.2f\n", max["lag_max_ms"]
        }'
}

for panels in $panels_list; do
    for rate in $rates; do
        log=$out/panels-$panels-rate-$rate.log
        NO_COLOR=1 RUST_LOG=warn,astrolabe::perf=trace \
            ASTROLABE_CLUSTER=$context ASTROLABE_NAMESPACE=$ns \
            ASTROLABE_OPEN=$(open_spec "$panels") \
            "$app" >"$log" 2>&1 &
        pid=$!
        sleep "$warmup"
        # Churn reaches its rate a few seconds after it starts; sample
        # only the steady part.
        "$churn" "$context" "$ns" run "$rate" $((seconds + ramp + 2)) \
            >"$out/panels-$panels-rate-$rate.churn" &
        churn_pid=$!
        sleep "$ramp"
        from=$(wc -l <"$log")
        sleep "$seconds"
        to=$(wc -l <"$log")
        wait "$churn_pid"
        kill "$pid"
        wait "$pid" 2>/dev/null || true
        printf 'panels=%-2s rate=%-3s %s\n' "$panels" "$rate" \
            "$(sed -n "$((from + 1)),${to}p" "$log" | summarize)"
    done
done
echo "logs: $out" >&2
