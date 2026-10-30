#!/usr/bin/env bash
# Usage: dev/test-evidence.sh [binary...]
# Runs `cargo test --workspace --no-fail-fast` once, writes the full log to
# a temp file, and prints the PR's Evidence block: each named test
# binary's `Running` and `test result` lines, every failing test with its
# panic message, and a workspace tally. A binary is named as in
# target/debug/deps/<name>-<hash>, i.e. tests/<name>.rs for integration
# tests. Exits with cargo's status; a named binary absent from the log is
# an error.
set -euo pipefail

tmp=${TMPDIR:-/tmp}
log=$(mktemp "${tmp%/}/test-evidence.XXXXXX")
echo "full log: $log" >&2

set +e
cargo test --workspace --no-fail-fast >"$log" 2>&1
status=$?
set -e

awk -v names="$*" '
    function count(word,    s) {
        if (!match($0, "[0-9]+ " word)) return 0
        s = substr($0, RSTART, RLENGTH)
        sub(/ .*/, "", s)
        return s + 0
    }
    /^ +Running / {
        bin = $0
        sub(/.*deps\//, "", bin)
        sub(/-[0-9a-f]+\)$/, "", bin)
        lines[bin] = lines[bin] $0 "\n"
        next
    }
    /^ +Doc-tests / { bin = "doc-tests " $2; next }
    /^test result: / {
        lines[bin] = lines[bin] $0 "\n"
        runs++
        passed += count("passed")
        failed += count("failed")
        ignored += count("ignored")
        section = 0
        next
    }
    /^---- .* stdout ----$/ {
        test = $0
        sub(/^---- /, "", test)
        sub(/ stdout ----$/, "", test)
        failures = failures "FAILED " bin " " test "\n"
        section = 1
        next
    }
    /^failures:$/ { section = 0; next }
    section && NF && !/^note: run with `RUST_BACKTRACE=1`/ {
        failures = failures "    " $0 "\n"
    }
    END {
        n = split(names, wanted, " ")
        for (i = 1; i <= n; i++) {
            if (wanted[i] in lines) {
                printf "%s", lines[wanted[i]]
            } else {
                print "no test binary named " wanted[i] " in the log" \
                    > "/dev/stderr"
                missing = 1
            }
        }
        printf "%s", failures
        printf "workspace: %d test binaries, %d passed, %d failed, " \
            "%d ignored\n", runs, passed, failed, ignored
        exit missing
    }
' "$log" || { [ "$status" -ne 0 ] || status=1; }

exit "$status"
