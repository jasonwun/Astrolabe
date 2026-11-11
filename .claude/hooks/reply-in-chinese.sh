#!/usr/bin/env bash
# Stop hook: AGENTS.md says to reply to the user in Chinese. When the final
# reply has no Chinese in it, block the stop and ask for the reply again in
# Chinese (#377 retro: replies drifted to English, mostly after a compaction).
set -euo pipefail
input=$(cat)
# The re-sent reply stops for good, whatever its language.
if [ "$(jq -r '.stop_hook_active // false' <<<"$input")" = true ]; then
    exit 0
fi
msg=$(jq -r '.last_assistant_message // empty' <<<"$input")
if [ -z "$msg" ]; then
    transcript=$(jq -r '.transcript_path // empty' <<<"$input")
    [ -f "$transcript" ] || exit 0
    msg=$(jq -c 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text' "$transcript" |
        tail -n 1 | jq -r '. // empty')
fi
[ -n "$msg" ] || exit 0
if jq -en --arg m "$msg" '$m | test("\\p{Han}")' >/dev/null; then
    exit 0
fi
jq -n '{
  decision: "block",
  reason: "AGENTS.md: reply to the user in Chinese. Your last reply has no Chinese in it; send it again in Chinese (code, paths and commands stay as they are)."
}'
