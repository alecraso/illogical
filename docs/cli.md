# The CLI

`illogical` drives the daemon from any shell, and from inside every pane (`ILLOGICAL_PANE` and `ILLOGICAL_SOCK` are set there, and it's on `PATH`).

```
illogical web                                 # this machine's page in your browser, signed in (--print: the link)
illogical ls                                  # panes, what they're running, who needs you
illogical run -- make test                    # in a new tab; prints its pane (%N)
illogical run --wait -- cargo build           # and exits with its exit code
illogical send %3 'git status' -e             # type a line and press Enter
illogical keys %3 C-c Up Enter                # named keys
illogical wait %3 --command-end               # exit code of what that started
illogical wait %3 --match 'listening on' --timeout 30
illogical tail %3 -f --text                   # follow output, escapes stripped
illogical tail %3 --last-command              # just the last command's output
illogical capture %3 --scrollback [--ansi|--html]
illogical process %3                          # the foreground process
illogical events -f [--pane %3] [--type command_end,attention]
illogical history --failed --since 2h
illogical search 'panic|Traceback' --since 1d
illogical export %3 -o session.cast           # asciinema play session.cast
illogical run --vm -- 'git clone … && make'   # on a throwaway VM (no command: a shell)
illogical run --vm-tab                        # a tab whose panes share a new VM
illogical machines                            # VMs, their owner (@tab or %pane) and state
illogical open example.com                    # a browser block (--split %3 beside a pane)
illogical open --split right :5173/about      # a port, beside this pane, on its VM tab's machine
illogical open --machine m2 :3000             # a port on machine m2 (--machine local: this host)
illogical edit src/main.rs:42                 # VS Code on this file's project, at line 42 (no path: here)
illogical edit --machine m2 ~/app             # on machine m2 (--split right beside this pane)
illogical diff                                # what changed here (a diff block; prints it, then the files)
illogical diff %4 HEAD~3 HEAD                 # in %4's repository, on its machine: a range (one rev: against it)
illogical view %4:src/main.rs:42              # a file block there, at line 42, followed live (PATH, mN:PATH)
illogical rerun %3                            # type %3's failed command again, once its shell is idle
illogical workspace ~/src/app [--env prod]    # a chant workspace block: members, records, gates (no dir: here)
illogical call %6 approve                     # approve the gate it waits at, as you ('{"member","op","gate"}' for another)
illogical call %6 refresh                     # read it again now (it reads by itself when git says something changed)
illogical editors                             # editors in the swarm: VS Code, Cursor, nvim, editor blocks
illogical editors install                     # illogical's extension into VS Code or Cursor here (--with cursor)
illogical editors vsix -o illogical.vsix      # ...or its VSIX, to install by hand
illogical ide                                 # illogicald as Claude Code's IDE: its port, where diffs go
illogical ide --diffs "Visual Studio Code"    # send Claude Code's diffs to that IDE instead (illogical: back)
illogical shell-env [--refresh]               # the PATH blocks that run your tools get (your shell's; --refresh: read it again)
illogical describe %4                         # any block: type, place, state
illogical call %4 navigate '{"url":"…"}'      # a block's own methods
illogical agent "fix the failing test"        # Claude Code here; prints %N (--codex, --fountain A,
                                              #   --acp CMD, --vm, --machine m3, --model haiku,
                                              #   --cwd d, --wait)
illogical agent --allow Read --allow Edit --permission-mode acceptEdits "…"
                                              # pre-approve tools and pick its mode (#163);
                                              #   --user-settings: your Claude Code allow/deny
                                              #   lists and default mode, never your hooks
illogical agent --as pr-reviewer "review this" # Claude Code here wearing a Fountain agent: its prompt,
                                              #   skills and MCP servers (M44; not for illogical.local: false;
                                              #   --vault V: its secrets' mapping)
illogical claude ls [--live] [--all] [words]  # Claude Code conversations here: terminal and desktop app
illogical claude open 3fa9c1                  # one as a stopped agent block, following it; prints %N
illogical studio login https://studio.example # keep a studio token in the daemon (read from stdin)
illogical studio                              # which studio, logged in or not (studio logout: forget it)
illogical app                                 # your studio's apps, and the blocks that show them
illogical app pinboard                        # one as an app block; its agent's questions come here
illogical call %9 send '{"text":"Pick a header colour"}'  # prompt its agent (its first tab; "tab": another)
illogical studio follower pinboard            # keep its box's hud follower link (from stdin; --forget)
illogical pr 84                               # a pull request as a block (in this repo; or a URL, OWNER/REPO#N)
illogical pr comment %7 "LGTM"                # comment; review %7 approve|request_changes|comment [TEXT]; merge %7
                                              #   (under CLAUDECODE or AI_AGENT: a draft a person sends)
illogical fountain runner install             # after sudo bash scripts/fountain-runner-setup.sh: this machine as the Fountain runner
illogical fountain runner status              # its unit, and every runner Fountain lists
illogical fountain runner adopt hud-playground  # an agent onto the runner provider (chant's: change agent-specs instead)
illogical issue 89                            # an issue as a block (or a URL, OWNER/REPO#N)
illogical issue agent %8                      # an agent on it: worktree + branch i89-…, the two in a tab
illogical issue new -t "Frobs leak" -b "…"    # open one here (under an agent: a draft a person sends)
illogical issue comment %8 "On it"            # comment (under an agent: a draft)
illogical fountain                            # your Fountain agents as a catalog block (-q WORDS, --source agent-specs)
illogical fountain agents frontend-design     # ...or listed here, one line each
illogical call %10 run '{"agent":"games"}'    # Run on Fountain (an agent block beside it); spec '{"agent":"pr-reviewer"}'
illogical call %10 run_here '{"agent":"games","cwd":"~/w"}'  # Run here: Claude Code wearing it, in ~/w
illogical call %10 filter '{"source":"agent-specs","query":"review"}'  # the block's filter ("clear": true)
illogical fountain --view runner              # this machine as the Fountain runner: status, other runners, its sandboxes
illogical call %11 shell '{"sandbox":"ID"}'   # a terminal as fountain in a sandbox; changes '{"sandbox":…}' (a diff per checkout),
                                              #   follow '{"conversation":"ID"}' (an agent block on it); view '{"view":"catalog"}'
illogical agent --resume 3fa9c1 "and now?"    # continue it in a block (refused while it's open elsewhere)
illogical agent --fork 3fa9c1                 # a new session with its history, in a block
illogical wait %5 --needs-input               # it asks to run something…
illogical call %5 approve                     # …or '{"option":"always"}'; deny '{"reason":"…"}'; cancel
illogical call %5 send '{"text":"and then?"}' # the next message (queued while it works)
illogical wait %5 --needs-input               # a question: printed as JSON…
illogical call %5 answer '{"question_0":"Red","question_1":["A","B"]}'  # …answered (decline: skip it)
illogical wait %5 --idle                      # the turn ended: prints idle, done or needs-input
illogical tail %5 -f                          # any block's text as it grows
illogical attach %3                           # from a real terminal; Ctrl-] detaches
illogical tui [--session S]                   # every tab and split in this terminal; Ctrl-] is the menu key
illogical close %3                            # its output stays in history
illogical attention needs-input               # from a hook, in the current pane
illogical attention [--json]                  # what wants you and why: ask, failed, exited, done (bundle keys)
illogical ask                                 # Claude Code's AskUserQuestion hook (below)
illogical hook                                # Claude Code's permission prompts as cards anyone on the team answers (below)
illogical inbox                               # Claude Code's background Stop hook: follow-ups from the team (below)
illogical hosts                               # the home daemon's other hosts, last seen
illogical hosts add box https://box.<tailnet>.ts.net
illogical hosts invite                        # a one-time token a sandbox joins with
illogical --host box run --wait -- make       # any command, on another host
illogical --host box run --home               # a shell on box, as a tab in this daemon's layout
illogical --host box run --home --split %4    # …beside %4 here (close %N closes it on box too)
illogical hosts token sbx                     # a dial-out host's token (prints it once)
illogical hosts revoke sbx                    # …revoked, and its connection dropped
illogical share %3 --ttl 2h                   # a read-only link to a pane
illogical shares                              # links that still work; shares revoke ID
illogical search 'panic' --synced sbx         # a host's synced history (all: every host)
illogical tail %4 --synced sbx --text         # one of its panes, after it's gone
illogical synced                              # hosts whose history is kept here
illogical sandboxes                           # the provider's sandboxes and their state
illogical run --sandbox s1                    # a disposable shell on one, nothing installed there
illogical sandboxes promote s1 --as s1        # a resident daemon there, a host reached through the tunnel
illogical --host s1 ls                        # through the tunnel (wakes it)
illogical fs ls -l ~/src                      # files on this host (read-only)
illogical fs cat %4:~/app/log.txt             # on the host %4 runs on (its VM); mN:PATH for machine N
illogical fs watch ~/src                      # changes, as NDJSON (also stat, recent)
illogical run --cwd ~/src                     # a shell in a directory, in a new tab
illogical run --split %4 --join --cwd ~/app   # beside %4, where it runs (its VM tab's machine)
illogical cd %4 ~/src                         # typed into %4's shell, only if it's at its prompt
illogical tmux -CC attach [-t SESSION]        # be tmux for iTerm2 (see *Use it*)
illogical mcp                                 # an MCP server on stdio (claude mcp add illogical -- illogical mcp)
illogical mcp token --name laptop [--scope read]  # a token for /mcp over HTTP, printed once
illogical mcp token --list                    # tokens, and when each was last used
illogical mcp token --revoke laptop           # cut it off at its next call
```

`--json` prints the API's JSON. `--host`, anywhere on the line, is another
daemon; a machine (a VM) is `--machine mN`. `send` then `wait` only sees what happened
after the send. The same calls are an HTTP API (`/api/...`, documented in
`crates/proto/src/api.rs`) on the Unix socket and, behind the usual access
checks, over the tailnet.

## MCP

`illogical mcp` is an MCP server on stdin and stdout, for clients that
start one as a command. It relays to the daemon's own server at `/mcp`
over its socket (or another daemon's, with `--host`), so it can be
started and stopped freely, and a daemon restart doesn't break it.

```
claude mcp add illogical -- illogical mcp
codex mcp add illogical -- illogical mcp
```

The tools:

| Tool | What it does | Reads only |
|---|---|---|
| `run` | A command in a new tab or split (`cwd`, `split`, `vm`, `vm_tab`, `machine`, `session`, `policy`), typed into a shell so it's in history and you can take over. With `wait`, its exit code and last lines. | no |
| `send_input` | Text (Enter after it unless `enter: false`) and named keys (`C-c`, `Up`) to a pane; to an agent block, its next prompt; to an app block, a prompt to its box's agent (`tab`: which) | no |
| `read_output` | A pane's output as text: the latest, from an `offset`, or its `last_command`'s. Paged (16,000 characters by default): pass `next_offset` back | yes |
| `capture_screen` | What a pane shows now | yes |
| `wait` | Until `command_end`, `exit`, `match` (a `pattern`), `idle` or `needs_input`. After `timeout` seconds (100 by default) it answers "still running" with the offset: call it again | yes |
| `list` | Panes and blocks: where, what they run, attention, who started them | yes |
| `close` | Close a pane or block (and a VM it owns) | no |
| `history` | Commands across panes: `failed`, `since` and `before` (`2d`, `36h`), `cwd`, `match` | yes |
| `search` | Lines of output matching a regex | yes |
| `open_port` | A browser block on a port of a pane's machine, beside it | no |
| `open_app` | One of the user's studio apps as an app block, beside a pane; without `app`, their apps | no |
| `start_agent` | An agent block (Claude Code, Codex, Fountain, any ACP agent) with a prompt; `as_fountain` (Claude Code): wear one of the user's Fountain agents here | no |
| `agent_respond` | Allow or deny an agent's pending approval, or answer or skip its question | no |
| `list_conversations` | Claude Code conversations here (a terminal's, the desktop app's): `query`, `cwd`, `live`, `all` | yes |
| `open_conversation` | One as an agent block beside a pane; `then`: `continue` or `fork` | no |
| `read_file` | A text file on this host or a pane's machine, paged | yes |
| `show_changes` | A diff block beside a pane: what changed in its repository (`rev_a`, `rev_b`); returns the files with +/− | no |
| `show_file` | A file block beside a pane, at a `line`, followed live | no |
| `open_workspace` | A chant workspace block beside a pane (`dir`, `env`); returns its members and the gates waiting | no |
| `open_pr` | A pull request (link, `OWNER/REPO#N`, or N in `dir`'s repo) as a block beside a pane; returns it as text | no |
| `read_pr` | A PR block as text, what it waits on the user for, and your drafts (waiting, sent with who and a link, dropped) | yes |
| `open_issue` | An issue (link, `OWNER/REPO#N`, or N in `dir`'s repo) as a block beside a pane; returns it as text | no |
| `read_issue` | An issue block as text: linked PRs, the agent on it and its PR, what it waits on the user for, your drafts | yes |
| `issue_comment`, `issue_new` | Draft a comment on an issue block, or a new issue (a block beside you holding the draft): a card the user sends, edits or drops | no |
| `list_agents` | The user's Fountain agents, one compact row each (`query` over names, descriptions, skills and servers; `source`: agent-specs, hand or app) | yes |
| `read_agent` | One Fountain agent's whole recipe (prompt, skills, MCP servers, model, metadata), its servers' credentials as `${VAR}`s | yes |
| `open_fountain` | The Fountain agent catalog as a block beside a pane (`query`, `source`); returns the list. `view: "runner"`: this host as the Fountain runner and its sandboxes instead | no |
| `pr_comment`, `pr_review`, `pr_merge` | Draft a comment, a review (`event`) or a merge on a PR block: a card the user sends, edits or drops; returns the draft's id at once | no |

Resources: `illogical://history`, and the templates
`illogical://pane/{id}/output`, `illogical://pane/{id}/screen` and
`illogical://block/{id}`.

Waits send a progress notification every 15 seconds: over HTTP, Claude
Code drops a call that's silent for 60. If a long build still doesn't fit,
`MCP_TOOL_TIMEOUT` raises Claude Code's own limit, but the tools return
"still running" well before any limit.

**Over HTTP.** The daemon serves the same tools at `/mcp` (Streamable
HTTP). On this machine, prefer `illogical mcp` (above); over loopback a
client shows the daemon's local token (`local-token` in the state
directory) or a token from `illogical mcp token`, as its bearer. From your
own machines on the tailnet nothing more is needed:

```
claude mcp add --transport http illogical https://home.<tailnet>.ts.net/mcp
```

A client without a tailnet identity of its own (a tagged node, a
container) needs a token:

```
illogical mcp token --name ci                 # prints ilm_…, once
claude mcp add --transport http illogical https://home.<tailnet>.ts.net/mcp \
  --header "Authorization: Bearer ilm_…"
illogical mcp token --revoke ci               # its next call is refused
```

`--scope read` makes a token that sees and calls the read-only tools only.
`illogical mcp --token T` (or `ILLOGICAL_MCP_TOKEN`) sends one through the
bridge.

**Agent blocks** get the server without asking: local agents as `/mcp` on
loopback (or `illogical mcp`, if the agent doesn't take HTTP servers), with
a token of their own that only reaches the block's tab. An agent can start
panes and blocks there (on the tab's machine, in a VM tab), drive and
close what it started, and read the rest of its tab; other tabs and new
VMs are refused. The token ends with the block, and stays out of its log.

An agent in a VM can't reach the host, so the daemon reaches in: it runs a
small relay in the VM (python3, on a Unix socket in `/tmp`) over a non-TTY
exec, and serves each connection to it as an MCP session with the same
scope. The agent's server is a small client for that socket. What the
agent runs lands on its own machine, which then becomes its tab's (as
*Share machine with tab* does), so it stays while those panes do. Agents
in Fountain's sandboxes don't get it.

**Who did it.** Every call is logged with the client's name and token. A
pane or block an MCP client started says "started by mcp:CLIENT" (the
client's own name, `claude-code` say), and what it typed is in history as
`mcp:CLIENT`'s.

## Claude Code in a pane

Claude Code in an ordinary pane can tell you when it needs you, put its
questions and permission prompts on cards anyone on the team who may
answer can answer (from the pane, the swarm's rail or a notification), and
take its next instruction from them. All of it is hooks, in
`~/.claude/settings.json`:

```json
{
  "hooks": {
    "Notification": [{ "hooks": [{ "type": "command", "command": "illogical attention needs-input" }] }],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "illogical attention done" }] },
      { "hooks": [{ "type": "command", "command": "illogical inbox", "asyncRewake": true, "timeout": 86400 }] }
    ],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "illogical inbox", "asyncRewake": true, "timeout": 86400 }] }],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "illogical ask", "timeout": 604800 }] },
      { "hooks": [{ "type": "command", "command": "illogical hook" }] }
    ],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "illogical hook", "timeout": 604800 }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }]
  }
}
```

Outside an illogical pane every one of these does nothing, so the hooks
are safe everywhere. Take only the lines you want: each group below works
on its own.

**Attention** (`Notification`, the first `Stop`). The Notification hook's
message ("Claude needs your permission to use Bash") becomes the headline
`illogical attention` shows. Without these, an agent going quiet
mid-command is the fallback.

**Questions** (`PreToolUse` on `AskUserQuestion`). `illogical ask` shows
Claude Code's questions as a card over the pane (the pane needs you, with
a push notification), waits, and hands your answers to Claude Code, which
then never shows its picker. *Answer in terminal* on the card gives you the
picker instead; Esc or Ctrl-C in Claude Code withdraws the card. The
timeout (7 days, in seconds) is how long a question may wait; Claude
Code's default would give up after 10 minutes and show its picker. If the
daemon restarts while it waits, the card comes back.

**Permission prompts** (`PermissionRequest`, with `PreToolUse`,
`PostToolUse`, `PostToolUseFailure` and `UserPromptSubmit` so the card can
tell when the terminal answered first). A permission prompt becomes a card
with the tool, its input (the command, the file and its diff) and Claude
Code's own "always allow" suggestions: *Allow*, *Always: …* (keeps that
rule, as the terminal's option would), or *Deny* with a message Claude
reads. Claude Code shows its own dialog too, and whichever answers first
wins: a "Yes" in the terminal closes the card when the tool runs, "No" or
Esc withdraws it. AskUserQuestion is left to `illogical ask`.

Anyone who may edit the pane's session may answer, from the card, the
swarm's rail, `illogical`'s API or a notification. The card then says who
answered ("Allowed by Sam, 14:02"), and so do the pane's history
(`illogical history`, `illogical log %N --who`) and the audit log. Viewers
see the card but can't answer it.

**Follow-ups** (`illogical inbox` on `Stop` and `SessionStart`, in the
background with `asyncRewake`). After an answer, the card offers *Send a
follow-up*. It reaches Claude Code through the inbox hook, which waits
after every turn and wakes it with the text as its next instruction, so it
never mixes with whatever the driver has half typed. It's recorded as its
sender's input. Who may send one is who may drive the pane: on someone's
own machine a teammate needs their trust first (the card offers to ask for
30 minutes); on a team's machine or a VM, team editors send straight away.
A session nobody drives (`claude -p`, the Agent SDK: Claude Code sets
`CLAUDE_CODE_SESSION_ATTENDED=0`) gets no follow-ups, and the hook leaves it
at once, so the same settings don't hold a headless run.

**From a script:**

```
illogical attention --json                    # every pane that wants you: ask, failed, exited, input, done
curl --unix-socket "$ILLOGICAL_SOCK" -X POST localhost/api/attention/act \
  -H 'content-type: application/json' -d '{"action":"allow","panes":[3,7]}'
curl --unix-socket "$ILLOGICAL_SOCK" -X POST localhost/api/panes/3/followup \
  -H 'content-type: application/json' -d '{"text":"now run the tests"}'
```

`/api/attention/act` takes `action` (`allow`, `deny`, `answer`,
`dismiss`, and for M28 `accept`, `reject` and `continue`) and a `pane` or
a list of `panes`, plus `option: "always"` and `suggestion: N` (which of
Claude Code's suggestions) for allow, `message` for deny, `content` (the
card's fields) for answer and `text` (the file as it should be saved) for
accept. Each pane is checked on its own (editor on its session) and
answered on its own.

**Diffs** (M28, no hooks needed). Claude Code in a pane connects to
illogicald as its IDE (`CLAUDE_CODE_SSE_PORT` is set in every pane), and
its Edit and Write calls wait as diff cards on the pane and the rail:
accept, change then accept, or reject. `GET /api/panes/N/diff` has the
file before and after. With the `PermissionRequest` hook too, the diff
card is what shows for an edit; the hook still covers Bash and the rest.
In `acceptEdits` mode Claude Code sends no diffs.

**Notifications.** The owner is always told. Anyone else who may answer
chooses which agents notify them: *Notify me about its agents* in the
session menu (or the phone's sheet), or `POST /api/notify` with `{"session": N, "on": true}` (no
session: everything they may edit there). This holds for the daemon's own
push and for pushes through illogical control.
