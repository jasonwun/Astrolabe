#!/usr/bin/env bash
set -euo pipefail

output=$(mktemp)
trap 'rm -f "$output"' EXIT

cargo mutants "$@" | tee "$output"
if grep -q 'Found 0 mutants to test' "$output"; then
  echo 'mutation filter matched no mutants; check the repository-relative file path' >&2
  exit 1
fi
