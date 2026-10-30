#!/usr/bin/env bash
# Usage: dev/test-evidence.sh [binary...]
# Runs `cargo test --workspace --no-fail-fast` once, writes the full log to
# a temp file, and prints the PR's Evidence block: each named test
# binary's `Running` and `test result` lines, every failing test with its
# captured output, every binary that crashed before its `test result`
# line, and a workspace tally. A binary is named as in
# target/debug/deps/<name>-<hash>: tests/<name>.rs for integration tests,
# the crate name with `-` as `_` for unit tests (a crate's lib and main
# unit tests share that name and print together). Exits with cargo's
# status; a named binary absent from the log is an error.
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
    # A binary that aborts prints no `test result` line; flag it when
    # the next binary starts or the log ends.
    function close_bin() {
        if (awaiting_result) failures = failures "CRASHED " bin "\n"
        awaiting_result = 0
    }
    /^ +Running / {
        close_bin()
        bin = $0
        sub(/.*deps\//, "", bin)
        sub(/-[0-9a-f]+\)$/, "", bin)
        lines[bin] = lines[bin] $0 "\n"
        binaries++
        awaiting_result = 1
        next
    }
    /^ +Doc-tests / {
        close_bin()
        bin = "doc-tests:" $2
        doc_runs++
        awaiting_result = 1
        next
    }
    /^test result: / {
        lines[bin] = lines[bin] $0 "\n"
        passed += count("passed")
        failed += count("failed")
        ignored += count("ignored")
        awaiting_result = 0
        in_failure = 0
        next
    }
    /^---- .* stdout ----$/ {
        test = $0
        sub(/^---- /, "", test)
        sub(/ stdout ----$/, "", test)
        failures = failures "FAILED " bin " " test "\n"
        in_failure = 1
        next
    }
    /^failures:$/ { in_failure = 0; next }
    in_failure && NF && !/^note: run with `RUST_BACKTRACE=1`/ {
        failures = failures "    " $0 "\n"
    }
    END {
        close_bin()
        n = split(names, wanted, " ")
        for (i = 1; i <= n; i++) {
            name = wanted[i]
            gsub(/-/, "_", name)
            if (name in lines) {
                printf "%s", lines[name]
            } else {
                print "no test binary named " wanted[i] " in the log" \
                    > "/dev/stderr"
                missing = 1
            }
        }
        printf "%s", failures
        printf "workspace: %d test binaries + %d doc-test runs, " \
            "%d passed, %d failed, %d ignored\n", \
            binaries, doc_runs, passed, failed, ignored
        exit missing
    }
' "$log" || { [ "$status" -ne 0 ] || status=1; }

exit "$status"
