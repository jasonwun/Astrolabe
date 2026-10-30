#!/usr/bin/env bash
# Usage: dev/demo.sh apply|delete
# Seeds dev/demo.yaml on kind-astrolabe-dev into this worktree's own
# namespace (astrolabe-demo-<worktree>), or deletes that namespace, so
# worktrees never seed or delete each other's demo. Prints the namespace.
set -euo pipefail

worktree=$(basename "$(git rev-parse --show-toplevel)" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-')
ns=$(cut -c1-63 <<<"astrolabe-demo-$worktree" | sed 's/-*$//')
kubectl=(kubectl --context kind-astrolabe-dev)

if ! "${kubectl[@]}" get --raw /readyz >/dev/null 2>&1; then
    echo "kind-astrolabe-dev is unreachable; start its Docker (OrbStack: orb start)" >&2
    exit 1
fi

# The namespace's default ServiceAccount arrives a moment after the
# namespace; a Pod created before it is forbidden.
wait_for_service_account() {
    for _ in $(seq 60); do
        "${kubectl[@]}" -n "$ns" get serviceaccount default >/dev/null 2>&1 && return
        sleep 1
    done
    echo "namespace $ns has no default ServiceAccount after 60s" >&2
    exit 1
}

case ${1:-} in
apply)
    "${kubectl[@]}" create namespace "$ns" --dry-run=client -o yaml |
        "${kubectl[@]}" apply -f - >&2
    wait_for_service_account
    sed "s/astrolabe-demo\([,}]\)/$ns\1/g" "$(dirname "$0")/demo.yaml" | "${kubectl[@]}" apply -f - >&2
    ;;
delete) "${kubectl[@]}" delete namespace "$ns" >&2 ;;
*) echo "usage: $0 apply|delete" >&2; exit 2 ;;
esac
echo "$ns"
