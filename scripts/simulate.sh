#!/bin/sh
# Simulates Claude Code hook events against a running Claude Companion,
# so the tray and popup can be tried without a real Claude session.
#   scripts/simulate.sh start       # session appears, tray goes idle
#   scripts/simulate.sh work        # tray starts blinking
#   scripts/simulate.sh permission  # popup appears; prints the decision JSON
#   scripts/simulate.sh question    # AskUserQuestion heads-up popup
#   scripts/simulate.sh stop        # "task finished" popup, tray turns green
#   scripts/simulate.sh end         # session removed
set -e
SID="${SID:-sim-session-1}"
CWD="${CWD:-$PWD}"

send() {
  printf '%s' "$1" | curl -s -X POST -H 'Content-Type: application/json' \
    -H "X-CC-PID: $$" -H "X-CC-Term: ${TERM_PROGRAM:-}" \
    -H "X-CC-Bundle: ${__CFBundleIdentifier:-}" -H "X-CC-Iterm: ${ITERM_SESSION_ID:-}" \
    --data-binary @- http://127.0.0.1:47823/hook
  echo
}

base="\"session_id\":\"$SID\",\"cwd\":\"$CWD\""
case "$1" in
  start) send "{$base,\"hook_event_name\":\"SessionStart\",\"source\":\"startup\"}" ;;
  work) send "{$base,\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"Add a dark mode toggle to the settings page\"}" ;;
  permission) send "{$base,\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm -rf node_modules && npm install\",\"description\":\"Reinstall dependencies\"}}" ;;
  question) send "{$base,\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"AskUserQuestion\",\"tool_input\":{\"questions\":[{\"question\":\"Which database should we use?\"}]}}" ;;
  stop) send "{$base,\"hook_event_name\":\"Stop\",\"last_assistant_message\":\"Added a dark mode toggle to Settings. It follows the system theme by default and remembers your choice.\"}" ;;
  end) send "{$base,\"hook_event_name\":\"SessionEnd\",\"reason\":\"other\"}" ;;
  *) sed -n '2,10p' "$0"; exit 1 ;;
esac
