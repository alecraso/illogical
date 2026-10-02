#!/bin/sh
# Launch claude-agent-acp ($1 = 85 for 0.85.0, 81 for 0.81.2) with the parent Claude Code session's env removed.
v="${1:-85}"; [ $# -gt 0 ] && shift
d="$(dirname "$0")/work/node$v/node_modules/.bin"
exec env -u CLAUDECODE -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDE_EFFORT \
  -u CLAUDE_CODE_MESSAGING_SOCKET -u CLAUDE_CODE_SESSION_ATTENDED -u CLAUDE_CODE_ENTRYPOINT \
  -u CLAUDE_CODE_EXECPATH -u CLAUDE_CODE_MESSAGING_TOKEN -u AI_AGENT \
  "$d/claude-agent-acp" "$@"
