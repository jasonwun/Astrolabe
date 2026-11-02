#!/usr/bin/env bash
# Usage: dev/log-capture.sh
# Fails when a test under crates/app/tests outside support/ keeps its own
# log buffer (`Arc<Mutex<Vec<u8>>>`): such tests capture `tracing` output
# with `support::LogCapture` instead (CODING_STANDARDS "View-test steps
# the support module already has").
set -euo pipefail

# Untracked test files count too (#106 retro).
if git ls-files -z --cached --others --exclude-standard 'crates/app/tests/*.rs' ':!crates/app/tests/support/*' |
    xargs -0 grep -nF 'Arc<Mutex<Vec<u8>>>'; then
    echo "Capture tracing output with support::LogCapture instead." >&2
    exit 1
fi
