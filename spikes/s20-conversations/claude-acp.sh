#!/bin/sh
# The adapter the daemon installs (~/.local/share/illogical/agents/claude), with
# the parent Claude Code session's env removed (as CLAUDE_ENV_REMOVE does).
exec env -u CLAUDECODE -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDE_EFFORT \
  -u CLAUDE_CODE_MESSAGING_SOCKET -u CLAUDE_CODE_SESSION_ATTENDED -u CLAUDE_CODE_ENTRYPOINT \
  -u CLAUDE_CODE_EXECPATH -u CLAUDE_CODE_MESSAGING_TOKEN -u AI_AGENT -u ILLOGICAL_PANE -u ILLOGICAL_SOCK \
  "$HOME/.local/share/illogical/agents/claude/node_modules/.bin/claude-agent-acp" "$@"
