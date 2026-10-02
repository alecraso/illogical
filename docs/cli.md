# The CLI

`illogical` drives the daemon from any shell, and from inside every pane (`ILLOGICAL_PANE` and `ILLOGICAL_SOCK` are set there, and it's on `PATH`).

```
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
illogical open --host m2 :3000                # a port on machine m2 (--host local: this host)
illogical describe %4                         # any block: type, place, state
illogical call %4 navigate '{"url":"…"}'      # a block's own methods
illogical agent "fix the failing test"        # Claude Code here; prints %N (--codex, --fountain A,
                                              #   --acp CMD, --vm, --model haiku, --cwd d, --wait)
illogical wait %5 --needs-input               # it asks to run something…
illogical call %5 approve                     # …or '{"option":"always"}'; deny '{"reason":"…"}'; cancel
illogical call %5 send '{"text":"and then?"}' # the next message (queued while it works)
illogical wait %5 --needs-input               # a question: printed as JSON…
illogical call %5 answer '{"question_0":"Red","question_1":["A","B"]}'  # …answered (decline: skip it)
illogical wait %5 --idle                      # the turn ended: prints idle, done or needs-input
illogical tail %5 -f                          # any block's text as it grows
illogical attach %3                           # from a real terminal; Ctrl-] detaches
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
```

`--json` prints the API's JSON. `send` then `wait` only sees what happened
after the send. The same calls are an HTTP API (`/api/...`, documented in
`crates/proto/src/api.rs`) on the Unix socket and, behind the usual access
checks, over the tailnet.

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

**From a script:**

```
illogical attention --json                    # every pane that wants you: ask, failed, exited, input, done
curl --unix-socket "$ILLOGICAL_SOCK" -X POST localhost/api/attention/act \
  -H 'content-type: application/json' -d '{"action":"allow","panes":[3,7]}'
curl --unix-socket "$ILLOGICAL_SOCK" -X POST localhost/api/panes/3/followup \
  -H 'content-type: application/json' -d '{"text":"now run the tests"}'
```

`/api/attention/act` takes `action` (`allow`, `deny`, `answer`,
`dismiss`) and a `pane` or a list of `panes`, plus `option: "always"` and
`suggestion: N` (which of Claude Code's suggestions) for allow, `message`
for deny and `content` (the card's fields) for answer. Each pane is checked
on its own (editor on its session) and answered on its own.

**Notifications.** The owner is always told. Anyone else who may answer
chooses which agents notify them: *Notify me about its agents* in the
session menu, or `POST /api/notify` with `{"session": N, "on": true}` (no
session: everything they may edit there). This holds for the daemon's own
push and for pushes through illogical control.
