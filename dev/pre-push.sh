#!/usr/bin/env bash
# Usage: dev/pre-push.sh [base-ref]   (default: origin/main)
# Runs, with CI's own flags, what the `lint` and `mutants` jobs of
# .github/workflows/ci.yml run, so a push does not find out in CI. Keep
# this in step with those two jobs. Run it on the final commit, review
# fixes included, after `git fetch` and a rebase onto the base.
set -euo pipefail

base=${1:-origin/main}
cd "$(git rev-parse --show-toplevel)"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
dev/comment-width.sh
dev/registration-path.sh
dev/log-capture.sh
dev/layer-check.sh
for crate in astrolabe-kube astrolabe-model; do
  if cargo tree -p "$crate" --locked | grep -qi gpui; then
    echo "gpui found in $crate's dependency tree" >&2
    exit 1
  fi
done

# The `mutants` job: the diff against the base, mutants of the two gpui-free
# crates only. `mktemp -d` because $TMPDIR is empty on this machine.
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
git diff "$base...HEAD" >"$dir/pr.diff"
cargo mutants -p astrolabe-kube -p astrolabe-model --cargo-arg=--locked \
  --in-diff "$dir/pr.diff" -j 2
