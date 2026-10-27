#!/usr/bin/env bash
# Usage: dev/demo.sh apply|delete
# Seeds dev/demo.yaml on kind-astrolabe-dev into this worktree's own
# namespace (astrolabe-demo-<worktree>), or deletes that namespace, so
# worktrees never seed or delete each other's demo. Prints the namespace.
set -euo pipefail

worktree=$(basename "$(git rev-parse --show-toplevel)" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-')
ns=$(cut -c1-63 <<<"astrolabe-demo-$worktree" | sed 's/-*$//')
kubectl=(kubectl --context kind-astrolabe-dev)

case ${1:-} in
apply) sed "s/astrolabe-demo\([,}]\)/$ns\1/g" "$(dirname "$0")/demo.yaml" | "${kubectl[@]}" apply -f - >&2 ;;
delete) "${kubectl[@]}" delete namespace "$ns" >&2 ;;
*) echo "usage: $0 apply|delete" >&2; exit 2 ;;
esac
echo "$ns"
