#!/usr/bin/env bash
# Usage: dev/move-check.sh [<base>]
# For a move-only refactor: diffs the working tree (new files must be
# tracked or `git add -N`) against its merge-base with <base> (default
# origin/main; a local main may lag) and prints the lines that are not
# moves: removed lines that come back nowhere, and added lines that were
# removed nowhere. Lines compare
# as a multiset, ignoring trailing whitespace and a leading `pub`/`pub(..)`
# so a visibility change still counts as a move; blank lines are skipped.
# Review what is left: in a move it is `use`/`mod` lines, re-exports and
# rustfmt's re-wraps of the lines that gained visibility.
set -euo pipefail

base=$(git merge-base "${1:-origin/main}" HEAD)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git diff -U0 --no-color --no-ext-diff "$base" |
    awk -v removed="$tmp/removed" -v added="$tmp/added" '
        /^(diff --git|index |--- (a\/|\/dev\/null)|\+\+\+ (b\/|\/dev\/null)|@@ |new file|deleted file|similarity|rename )/ { next }
        { sub(/[ \t]+$/, "") }
        match($0, /^[-+][ \t]*pub(\([a-z:]+\))? /) {
            head = substr($0, 1, RLENGTH)
            sub(/pub(\([a-z:]+\))? $/, "", head)
            $0 = head substr($0, RLENGTH + 1)
        }
        /^-/ && length($0) > 1 { print substr($0, 2) > removed }
        /^\+/ && length($0) > 1 { print substr($0, 2) > added }
    '
touch "$tmp/removed" "$tmp/added"
LC_ALL=C sort "$tmp/removed" >"$tmp/removed.sorted"
LC_ALL=C sort "$tmp/added" >"$tmp/added.sorted"

gone=$(LC_ALL=C comm -23 "$tmp/removed.sorted" "$tmp/added.sorted")
new=$(LC_ALL=C comm -13 "$tmp/removed.sorted" "$tmp/added.sorted")
echo "## Removed lines that come back nowhere"
if [ -n "$gone" ]; then echo "$gone"; fi
echo
echo "## Added lines that were removed nowhere"
if [ -n "$new" ]; then echo "$new"; fi
