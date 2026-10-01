#!/bin/sh
# Launch the pinned claude-agent-acp with the parent Claude Code session's env removed.
# Uses the Claude Code binary bundled in @anthropic-ai/claude-agent-sdk unless CLAUDE_CODE_EXECUTABLE is set.
d="$(dirname "$0")/work/node/node_modules/.bin"
exec env -u CLAUDECODE -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDE_EFFORT \
  -u CLAUDE_CODE_MESSAGING_SOCKET -u CLAUDE_CODE_SESSION_ATTENDED -u CLAUDE_CODE_ENTRYPOINT \
  -u CLAUDE_CODE_EXECPATH -u CLAUDE_CODE_MESSAGING_TOKEN -u AI_AGENT \
  CLAUDE_AGENT_LOGS="${CLAUDE_AGENT_LOGS:-}" \
  "$d/claude-agent-acp" "$@"
