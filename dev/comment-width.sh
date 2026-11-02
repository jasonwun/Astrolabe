#!/usr/bin/env bash
# Usage: dev/comment-width.sh
# Fails when a comment line under crates/ is wider than 80 characters,
# indentation included. Stable rustfmt does not wrap comments, so this
# check does. New files count before they are added: a file the index
# lacks passed unchecked during #106.
set -euo pipefail

git ls-files -z --cached --others --exclude-standard 'crates/*.rs' | xargs -0 perl -CSD -ne '
    if (m{^\s*//} && length($_) - 1 > 80) {
        printf "%s:%d: %d chars\n", $ARGV, $., length($_) - 1;
        $bad = 1;
    }
    close ARGV if eof;
    END { exit $bad ? 1 : 0 }
'
