# The CLI

`illogical` drives the daemon from any shell, and from inside every pane (`ILLOGICAL_PANE` and `ILLOGICAL_SOCK` are set there, and it's on `PATH`).

```
illogical web                                 # this machine's page in your browser, signed in (--print: the link)
illogical status                              # the daemon: its version, the service that runs it, its
                                              #   binary and log; whether and where it's joined to
                                              #   control (connected, or dropped by control: exit 1);
                                              #   each agent's adapter, and Claude Code's MCP server
illogical setup claude                        # Claude Code's adapter (installed, or updated to the pin)
                                              #   and illogical's MCP server, then what changed
                                              #   (`setup codex`: Codex's adapter)
illogical ls                                  # panes, what they're running, who needs you
illogical run -- make test                    # in a new tab; prints its pane (%N)
illogical run --wait -- cargo build           # and exits with its exit code
illogical send %3 'git status' -e             # type a line and press Enter
illogical send %4 --wait 'fix the test'       # prompt the agent there and wait for its turn (exit 0 done,
                                              #   2 it asks for you, 3 stalled: nothing started)
illogical keys %3 C-c Up Enter                # named keys
illogical upload %4 shot.png                  # a file onto %4's host, its path pasted in (for a claude there)
                                              # (to an agent block: sent as its prompt, an image as an image)
illogical wait %3 --command-end               # exit code of what that started
illogical wait %3 --match 'listening on' --timeout 30
illogical tail %3 -f --text                   # follow output, escapes stripped
illogical tail %3 --last-command              # just the last command's output
illogical capture %3 --scrollback [--ansi|--html]
illogical process %3                          # the foreground process
illogical events -f [--pane %3] [--type command_end,attention]
illogical history --failed --since 2h
illogical history --kind answer                 # who allowed or answered what (command, answer or agent)
illogical search 'panic|Traceback' --since 1d
illogical export %3 -o session.cast           # asciinema play session.cast
illogical open example.com                    # a browser block (--split %3 beside a pane)
illogical open --split right :5173/about      # a port, beside this pane
illogical open --machine m2 :3000             # a port on machine m2 (--machine local: this host)
illogical edit src/main.rs:42                 # VS Code on this file's project, at line 42 (no path: here)
illogical edit --machine m2 ~/app             # on machine m2 (--split right beside this pane)
illogical diff                                # what changed here (a diff block; prints it, then the files)
illogical diff %4 HEAD~3 HEAD                 # in %4's repository, on its machine: a range (one rev: against it)
illogical view %4:src/main.rs:42              # a file block there, at line 42, followed live (PATH, mN:PATH)
illogical rerun %3                            # type %3's failed command again, once its shell is idle
illogical editors                             # editors in the swarm: VS Code, Cursor, nvim, editor blocks
illogical editors install                     # illogical's extension into VS Code or Cursor here (--with cursor)
illogical editors vsix -o illogical.vsix      # ...or its VSIX, to install by hand
illogical ide                                 # illogicald as Claude Code's IDE: its port, where diffs go
illogical ide --diffs "Visual Studio Code"    # send Claude Code's diffs to that IDE instead (illogical: back)
illogical shell-env [--refresh]               # the PATH blocks that run your tools get (your shell's; --refresh: read it again)
illogical describe %4                         # any block: type, place, state
illogical describe %4 --detection             # how its agent's screen reads: each rule, what it saw, which fired
illogical describe --agents [--refresh]       # agents configured here (chant audit --agents): whose screen rules run
illogical call %4 navigate '{"url":"…"}'      # a block's own methods
illogical agent "fix the failing test"        # Claude Code here; prints %N (--codex,
                                              #   --acp CMD, --machine m3, --model haiku,
                                              #   --cwd d, --wait)
illogical agent --allow Read --allow Edit --permission-mode acceptEdits "…"
                                              # pre-approve tools and pick its mode;
                                              #   --user-settings: your Claude Code allow/deny
                                              #   lists and default mode, never your hooks
illogical claude ls [--live] [--all] [words]  # Claude Code conversations here: terminal and desktop app
illogical claude open 3fa9c1                  # one as a stopped agent block, following it; prints %N
illogical pr 84                               # a pull request as a block (in this repo; or a URL, OWNER/REPO#N)
illogical pr comment %7 "LGTM"                # comment; review %7 approve|request_changes|comment [TEXT]; merge %7
                                              #   (under CLAUDECODE or AI_AGENT: a draft a person sends)
illogical issue 89                            # an issue as a block (or a URL, OWNER/REPO#N)
illogical issue agent %8                      # an agent on it: worktree + branch i89-…, the two in a tab
illogical issue new -t "Frobs leak" -b "…"    # open one here (under an agent: a draft a person sends)
illogical issue comment %8 "On it"            # comment (under an agent: a draft)
illogical agent --resume 3fa9c1 "and now?"    # continue it in a block (refused while it's open elsewhere)
illogical agent --fork 3fa9c1                 # a new session with its history, in a block
illogical wait %5 --needs-input               # it asks to run something…
illogical call %5 approve                     # …or '{"option":"always"}'; deny '{"reason":"…"}'; cancel
illogical call %5 approve '{"option":"always","scope":"cwd","prefix":"cargo"}'  # a standing rule (or "everywhere")
illogical rules                               # standing rules on this machine (--forget N, --forget-all)
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
illogical hooks install [--project DIR] [--dry-run]  # add all of those to Claude Code's settings.json, keeping yours
illogical hooks status                        # which of them are there (below)
illogical hosts                               # the home daemon's other hosts, last seen, and control's machines once logged in
illogical login [--account FP]                # make this CLI one of your devices on control (approve its code on a signed-in device)
illogical --host mini capture %2              # a machine on your control account, direct or relayed (nothing in hosts.json)
illogical --host mini attach %2               # attach, tui and --follow work through control too
illogical --host sams-box tui                 # a team's machine, or one shared with you
illogical logout                              # forget the CLI's key for control
illogical hosts add box https://box.<tailnet>.ts.net
illogical hosts invite                        # a one-time token another daemon joins with
illogical --host box run --wait -- make       # any command, on another host
illogical --host box run --home               # a shell on box, as a tab in this daemon's layout
illogical --host box run --home --split %4    # …beside %4 here (close %N closes it on box too)
illogical hosts token sbx                     # a dial-out host's token (prints it once)
illogical hosts revoke sbx                    # …revoked, and its connection dropped
illogical share %3 --ttl 2h                   # a read-only link to a pane
illogical shares                              # links that still work; shares revoke ID
illogical invite sam@example.com --note "the flaky test"   # share this session with them and notify them alone
illogical invite bea --role editor --drive 30 --pane %4    # a teammate, by name: may also type in %4 for 30 min
illogical search 'panic' --synced sbx         # a host's synced history (all: every host)
illogical tail %4 --synced sbx --text         # one of its panes, after it's gone
illogical synced                              # hosts whose history is kept here
illogical --host s1 ls                        # through the tunnel (wakes it)
illogical --ssh me@box tui                    # a box you can ssh into; installs illogical there first if asked
illogical hosts add box ssh://me@box          # saved: `illogical --host box …` runs your ssh to it
illogical --ssh me@box join                   # set the box up over ssh and add it to control (approve the code from your phone)
illogical --ssh me@box join --account FP      # the same, checking the account's fingerprint instead of asking
illogicald install --system                   # macOS: start the daemon at boot, with nobody logged in (sudo)
illogicald uninstall                          # remove the service install set up (binaries and state stay)
illogical fs ls -l ~/src                      # files on this host (read-only)
illogical fs cat %4:~/app/log.txt             # on the host %4 runs on; mN:PATH for machine N
illogical fs watch ~/src                      # changes, as NDJSON (also stat, recent)
illogical run --cwd ~/src                     # a shell in a directory, in a new tab
illogical run --split %4 --join --cwd ~/app   # beside %4, where it runs
illogical cd %4 ~/src                         # typed into %4's shell, only if it's at its prompt
illogical tmux -CC attach [-t SESSION]        # be tmux for iTerm2 (see *Use it*)
illogical mcp                                 # an MCP server on stdio (claude mcp add illogical -- illogical mcp)
illogical mcp token --name laptop [--scope read]  # a token for /mcp over HTTP, printed once
illogical mcp token --list                    # tokens, and when each was last used
illogical mcp token --revoke laptop           # cut it off at its next call
```

`illogical invite WHO` shares a session (this pane's, or `--session S`) as
a viewer (`--role editor` to drive; never lower than they have: revoke
first) and pushes that person alone: "*you* brought you into *session*",
the note, opening at the pane (this one, `--pane %N`, or the session's
first). WHO is a tailnet login, someone already shared with, or, joined to
illogical control, a member of your teams by name (your browser tells
your machines which teams it checked; each machine checks the roster
itself). Anyone else: share once from the web, which checks their
fingerprint, then invite. `--root DEVICE` names an account's first device
yourself: it prints the fingerprint to check with them and goes on only
with `--yes` (or a yes at a terminal). It prints whether they were
notified: *sent* (a subscription of theirs took it), *pending* (they
haven't accepted this machine yet; it goes out when they do, for a day)
or *unreachable*, and why (a tailnet guest who hasn't turned on
notifications here hears once they connect). It's in `illogical access
log`. `--drive MINUTES` also trusts an editor to type in that pane on this
machine; on a team's machine they drive by their role anyway.

`--json` prints the API's JSON. `--host`, anywhere on the line, is another
daemon; a machine is `--machine mN`.

Once `illogical login` has made the CLI one of your devices, `--host NAME`
also finds the machines control lists: your own, your teams', and those
shared with you. Every command works that way, `attach`, `tui` and
`--follow` included, over one end-to-end channel per command (directly
when the machine lists a URL that answers, else through control's relay;
`ILLOGICAL_VERBOSE=1` says which). Another account's machine is checked
against that account's root, which the CLI remembers the first time it
sees it (in `cli-control.json`, as a browser keeps it); if control later
reports a different root for that account, the CLI refuses the machine and
says so. `illogical hosts` marks another account's machines with whose
they are. What you may do on one is what the web lets you: a team editor
reads and types in its panes, and making panes stays its owner's.

`--ssh DEST` reaches a box with your own `ssh` (your `~/.ssh/config`, keys
and agent; a password or 2FA prompt shows in your terminal once). One master
connection per box carries every command after it. The first time, if the
box has no illogical, it offers to put this version in `~/.local/bin` there
(the release for the box's platform, copied over ssh, so the box needs no
network) and starts its daemon: a systemd user service with lingering where
it can, detached otherwise. Declining is remembered.
`ILLOGICAL_SSH_INSTALL=yes` answers yes, `ILLOGICAL_SSH` replaces the `ssh`
command, and `ILLOGICAL_SSH_AGENT=no` keeps your agent here; otherwise panes
on the box use it (for `git push`) while you're connected. `send` then `wait` only sees what happened
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

`illogical setup claude` (or Getting started's *Use Claude Code with
illogical*) adds it to Claude Code, and installs the adapter agent blocks
run Claude Code through, in one go.

The tools. `list`, `show` and `draft` each group several jobs under a
`kind` argument, so a client's list stays short; the tool's
description says what each kind takes:

| Tool | What it does | Reads only |
|---|---|---|
| `run` | A command in a new tab or split (`cwd`, `split`, `machine`, `session`, `policy`), typed into a shell so it's in history and you can take over. With `wait`, its exit code and last lines. | no |
| `send_input` | Text (Enter after it unless `enter: false`) and named keys (`C-c`, `Up`) to a pane; to an agent block, its next prompt | no |
| `attach` | A file (a `path` on this host, or base64 `data`) into a pane: into a terminal, its path pasted where a shell or an agent reads it (else refused unless `force`); to an agent block, with `text` as its next prompt, an image as an image | no |
| `read_output` | A pane's output as text: the latest, from an `offset`, or its `last_command`'s. Paged (16,000 characters by default): pass `next_offset` back. `screen: true`: what it shows now | yes |
| `wait` | Until `command_end`, `exit`, `match` (a `pattern`), `idle` or `needs_input`. After `timeout` seconds (100 by default) it answers "still running" with the offset: call it again | yes |
| `list` | By `kind`: `panes` (the default: where, what they run, attention, who started them), `conversations` (Claude Code's here, a terminal's or the desktop app's: `query`, `cwd`, `live`, `all`) or `devices` (the user's devices lending tools) | yes |
| `close` | Close a pane or block | no |
| `history` | Commands across panes, and answers and approvals with who gave them: `kind` (`command`, `answer`, `agent`), `failed` (commands only), `since` and `before` (`2d`, `36h`), `cwd`, `match`. `kind: "output"`: lines of output matching a `pattern` instead (`since`, `limit`) | yes |
| `show` | A block beside a pane (`beside`), by `kind`: `port` (a browser on a port of the pane's machine), `changes` (a diff of its repository, `rev_a`, `rev_b`; returns the files with +/−), `file` (at a `line`, followed live), `pr` and `issue` (a link, `OWNER/REPO#N`, or N in `dir`'s repo; returns it as text), `conversation` (a Claude Code conversation by `id`; `then`: `continue` or `fork`) | no |
| `start_agent` | An agent block (Claude Code, Codex, any ACP agent) with a prompt | no |
| `prompt_agent` | A prompt to an agent (an agent block, or Claude Code or Codex in a terminal), waited through in one call: `done`, `needs_input` with its question, or `stalled` with its screen's last lines when nothing starts within 5 seconds. An agent waiting on someone isn't typed at (`answering` to answer it) | no |
| `agent_respond` | Allow or deny an agent's pending approval, or answer or skip its question | no |
| `read_forge` | A PR or issue block as text: what it waits on the user for, an issue's agent and its PR, and your drafts (waiting, sent with who and a link, dropped) | yes |
| `draft` | By `kind`: `comment` on a PR or issue block, `review` (`event`) or `merge` of a PR block, or `issue`, a new one (a block beside you holding the draft). Each is a card the user sends, edits or drops; returns the draft's id at once | no |
| `invite_person` | Ask to bring someone (`who`: a teammate, a grantee, `tailnet:<login>`) into the pane's session as a viewer or an editor (`role`), with a `note`: a card on an invite block beside you that only the session's owner sends (editing the role, note or drive trust) or declines. `pane` defaults to your own (`illogical mcp` in a pane sends `$ILLOGICAL_PANE`); returns the draft's id at once | no |
| `read_invite` | What became of a draft: `waiting`, `sent` (grant, `delivery`), `declined` (reason), `dropped` (unanswered for a day, or its block closed by the owner, the only one who may) or `failed`; who settled it, when | yes |
| `read_file` | A text file on this host or a pane's machine, paged | yes |
| `device_call` | A tool on one of the user's devices (`list` kind `devices`), which the user allows or denies there | no |

The older tool names (`capture_screen`, `search`, `open_port`,
`open_pr`, `pr_comment`, `read_issue` and the rest) still answer, as the
tool and kind that do their job now; they aren't listed.

Resources: `illogical://history`, and the templates
`illogical://pane/{id}/output`, `illogical://pane/{id}/screen` and
`illogical://block/{id}`.

`invite_person` asks twice in a terminal: Claude Code's own permission
prompt for the tool, then the invite card. The card is the real gate
(only the session's owner sends it, by any route; never an editor or an
agent), so allowing `mcp__illogical__invite_person` in Claude Code is
safe.

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
panes and blocks there, drive and
close what it started, and read the rest of its tab; other tabs are
refused. The token ends with the block, and stays out of its log.

**Who did it.** Every call is logged with the client's name and token. A
pane or block an MCP client started says "started by mcp:CLIENT" (the
client's own name, `claude-code` say), and what it typed is in history as
`mcp:CLIENT`'s.

## Claude Code in a pane

Claude Code in an ordinary pane can tell you when it needs you, put its
questions and permission prompts on cards anyone on the team who may
answer can answer (from the pane, the swarm's rail or a notification), and
take its next instruction from them. All of it is hooks, in
`~/.claude/settings.json`. `illogical hooks install` writes exactly this
(merged into what's there, never replacing it; `--project DIR` for a
project's `.claude/settings.json`, `--dry-run` to see the result first),
and `illogical hooks status` checks it:

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
30 minutes); on a team's machine, team editors send straight away.
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
`dismiss`, and for the editor's diffs `accept`, `reject` and `continue`) and a `pane` or
a list of `panes`, plus `option: "always"` and `suggestion: N` (which of
Claude Code's suggestions) for allow, `message` for deny, `content` (the
card's fields) for answer and `text` (the file as it should be saved) for
accept. Each pane is checked on its own (editor on its session) and
answered on its own.

**Diffs** (no hooks needed). Claude Code in a pane connects to
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
