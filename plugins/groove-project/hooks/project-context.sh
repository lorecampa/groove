#!/usr/bin/env bash
# Native hook protocol, not the Groove CLI JSON response protocol.
# The harness bounds the entire invocation to ten seconds.
set -euo pipefail
export LC_ALL=C

fail() { printf 'groove-project: %s\n' "$1" >&2; exit 1; }
command -v jq >/dev/null || fail 'install jq 1.6+ to load project instructions'

# Bound input before parsing; never evaluate hook payload or Git config as code.
payload=$(head -c 65537 && printf '.')
payload=${payload%.}
[ "${#payload}" -le 65536 ] || fail 'hook input exceeds 64 KiB'
event=$(printf '%s' "$payload" | jq -er '
  select(type == "object") | .hook_event_name |
  select(. == "SessionStart" or . == "SubagentStart")') || fail 'unsupported hook event'
cwd=$(printf '%s' "$payload" | jq -er '
  .cwd | select(type == "string" and startswith("/") and
    (test("[\u0000-\u001f]") | not))') || fail 'hook cwd must be an absolute path without control characters'

# An enclosing Git invocation must not redirect policy lookup to another repo.
for git_variable in ${!GIT_@}; do unset "$git_variable"; done
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
export GIT_OPTIONAL_LOCKS=0

# --local reads the clone's common config in linked worktrees too. Do not read
# tracked project config or system/global defaults as an implicit opt-in.
if ! enabled=$(git -C "$cwd" config --local --bool --get groove.agent.enabled 2>/dev/null); then
  exit 0
fi
[ "$enabled" = true ] || exit 0

hook_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
context=$(cat "$hook_root/workflow.md")
if instructions=$(git -C "$cwd" config --local --get groove.agent.instructionsFile); then
  case "$instructions" in
    /*) ;;
    *) fail 'groove.agent.instructionsFile must be an absolute path' ;;
  esac
  [ -f "$instructions" ] && [ -r "$instructions" ] || fail 'configured instructions must be a readable regular file'
  extra=$(head -c 32769 < "$instructions" && printf '.') || fail 'cannot read configured instructions'
  extra=${extra%.}
  [ "${#extra}" -le 32768 ] || fail 'configured instructions exceed 32 KiB'
  context=$(printf '%s\n\nAdditional project instructions (%s):\n%s' "$context" "$instructions" "$extra")
fi
printf '%s' "$context" | jq -Rs --arg event "$event" '
  {hookSpecificOutput: {hookEventName: $event, additionalContext: .}}'
