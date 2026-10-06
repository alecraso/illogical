# S8: which block types come next
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s8-blocks/<file>`.

Run 2026-10-02 on geek, for #6 (before M10, #7, and M11, #8). **Result: build
a cut of M11 (a diff block and a file block, for reviewing what an agent
changed, phone first). Drop M10 as a block type: its one useful piece, Rerun
on a failed command, goes onto terminals as M24's unbuilt action. No notes
block.**

- **The trigger hasn't held, so the evidence is thin.** S8 was meant to run
  after two weeks of using M6. M6 landed on 2026-10-01; the daily daemon's
  whole history is 131 commands over about 23 hours. #55 asked for the
  decision now anyway, so this is a call on a day of use plus how the
  project itself was built, and it is cheap to revisit (see *Revisit when*).
- **People don't do build, log or diff work in illogical yet. Agents do it
  everywhere.** In the daily daemon: no builds, no `git diff`, no file reads,
  no log tails. In Claude Code's transcripts for this repo: of 7,595 shell
  commands, about 1,260 run a build or test, about 1,000 poll or read a
  background job's output, 625 read git history or diffs, and about 1,900
  print a file. Jake's own typed messages (323 in this repo) almost never ask
  to see a diff, a file or a log; they ask whether things are built, merged,
  deployed and green.
- **Terminals already are job blocks.** M23 labels a pane's work `build`,
  `test`, `server` or `logs`; M24 turns a failed or finished long command into
  a `failed`/`done` reason with the command, exit code and duration, a badge,
  the phone's *Needs you* and a push; `illogical wait` exits with the
  command's code; M16 gives agents all of it as tools (its real-Claude test
  runs a failing build, reads why, fixes and reruns). What a job block would
  add (no PTY, split stdout/stderr, retries, CI adapters) has no evidence
  behind it. The gap is acting on a failure from the phone: **Rerun**.
- **Services: no evidence.** One browser block in the daily daemon, no dev
  server in the history, and no issue or message about one dying across a
  wake. Restart policies (`rerun`) and M6a cover what there is.
- **Reviewing an agent's work is the job, and nothing does it on a phone.**
  M28's diff cards show one pending Claude Code edit (capped at 16 KB) for
  approval; M27's code-server shows a diff on a desktop. Nothing answers
  "what did the agent in that VM tab change?" from the phone, for any agent,
  after the fact. That is M11's *Done when*, and it is where agents' work
  piles up: a typical change in this repo is 8 files and ~500 lines (median
  of 150 non-merge commits), p90 28 files and ~3,400 lines.
- **Most of M11 is already built.** M7's `fs.read`/`fs.watch` work on every
  host including VMs; M28's follow view is a read-only CodeMirror 6 with
  highlighting, line numbers and the terminal's colours (its own 180 KB
  gzipped chunk); `Provider::run` runs `git` on a VM; agent blocks keep each
  tool call's `locations`.

## Where the evidence came from

There is no `docs/dogfood.md`: nobody kept a friction log. So, all read-only:

| source | what was taken |
|---|---|
| git history (225 commits, 2026-10-01 → 02) | dates (when M6 landed), change sizes per commit (`--shortstat`) |
| Forgejo issues (#1–#66, with comments) | what was filed as friction: no issue asks for a job, service, file or diff view; the bugs are flakes, flow control and VM agents |
| the daily daemon's `illogical history --json` and `illogical ls --json` (read only, nothing run or sent) | 131 commands in 48 panes over ~23 h; the 7 blocks open now and their types |
| `~/.bash_history` on geek (500 lines) | mostly test daemons' and test shells' commands written into it, so it says little about a person |
| Claude Code transcripts under `~/.claude/projects/` (this repo: 189 files; all others: 410) | every Bash tool call bucketed by pattern; Jake's typed messages bucketed by keyword |

Only counts and patterns are kept here; no command lines, paths, hosts or
messages are quoted. The classifier was a pair of throwaway regex scripts
(left out: they hold home paths). Buckets overlap (one command can poll *and*
read a file), and the regexes over- and under-match; the sizes, not the
exact numbers, are the point.

## Evidence

### The four signals S8 was told to look for

| signal (from PLAN.md) | in the daily daemon (people) | in transcripts, this repo (agents) | in transcripts, all projects (agents) |
|---|---|---|---|
| polling a build | 0 builds; 21 commands failed, all quick probes | ~1,260 build/test commands; 124 run in the background; 677 sleep/until loops; 404 reads of a background job's output; 85 CI status checks | 1,529 poll loops; 1,018 CI status checks; 787 background-output reads |
| tailing a service's logs | 0 | 126 (`systemctl`/`journalctl`/`tail -f`, nearly all on test daemons) | 283 |
| re-reading a diff an agent made | 0 | 625 git history/diff reads: 450 `log --oneline`, ~160 `--stat`, ~130 full diffs or `show` | 2,241 |
| opening a file only to read it | 0 | ~1,900 prints of a source or doc file (plus 222 Read tool calls) | 2,268 |

### What the daily daemon holds

48 panes in a day, most of them in sprites (35 of 131 commands ran in a
sprite's home): `ls`, `hostname`, `cd`, the sprite tool, a few test prompts to
agents. Open now: 3 shells, 2 agent blocks, 1 Claude Code in a terminal, 1
browser block. That is a person trying illogical out, not living in it.

### What Jake asks for

Of 323 typed messages in this repo's transcripts, about 25 ask whether
something is built, merged or green, 2 mention a diff and none ask to see a
file or a log. Across all projects (1,035 typed messages): reviews and PRs
68, tests 59, deploys 44, something down or restarting 42, CI 35, diffs or
"what changed" 22, show a file 17, logs 5. He reads agents' summaries and
asks about outcomes; the agents read the diffs, files and logs.

### What that means for each candidate

- **job.** The work exists in bulk, but it's agents' work, and agents have
  their own background jobs and now M16. For a person, a terminal running the
  build already turns into a *Failed* push with the exit code and duration.
  What's missing on the phone is doing something about it.
- **service.** No sign of a dev server or service that needed keeping up.
- **diff.** Agents look at *what changed* as a list first (`--stat`,
  `--oneline`, 3:1 over full diffs), then open the parts they care about.
  People don't do it at all today, because the only place to do it is a
  terminal; checking an agent's work from the phone is what illogical's
  agents-in-VMs story needs and doesn't have.
- **file.** Opening a file only to read it is the second step of the same
  review: from a hunk, or from an agent's tool call, to the file at that
  line.
- **notes.** No evidence at all (the "notes" in transcripts are test files).

## Fitting the candidates to the block contract

On paper, against `crates/daemon/src/block.rs` as built (`Block`,
`BlockCtx`, `authz.rs`); no throwaway types were compiled. The browser,
agent and editor blocks already cover most of the shapes, so where they
answer a question it says so.

| | job | service | file | diff |
|---|---|---|---|---|
| config | `{host, cmd, cwd, env, retries, timeout}` fits | `{host, cmd, port, restart}` fits | `{host, path, line?}` | `{host, repo, rev_a?, rev_b?}` (no revs: the working tree against `HEAD`) |
| state | phase, attempt, exit, duration: fits | up/down, restarts, port: fits | path, size, mtime, language; contents for viewers (see 2) | files with +/−, the open file's hunks |
| attention | **gap 1**: `failed` needs a full M24 reason | `exited` reason: gap 1 | always `idle` | always `idle` |
| `capture --text` | the output: fits | the last output: fits | the file (capped at `READ_MAX`) | the unified diff |
| methods | `retry`, `cancel`, `logs` | `restart`, `stop`, `open` | `goto{line}`, `open{path}` | `refresh`, `file{path}`, `open{path, line}` |
| log | stdout/stderr: fits | output: fits | **gap 3**: nothing worth keeping | gap 3 |
| VMs | `Provider::pipe`: fits | `put_service`: fits | **gap 4**: `fs.watch` keeps a machine awake | gap 4, and `git` through `Provider::run` |

1. **A block can't raise a full M24 reason.** `BlockCtx::attention` makes a
   plain `input` or `done` reason; only terminals (`mux.rs`, at a command's
   end) make `failed` with a command, exit code, duration and bundle key.
   A job or service block would need `BlockCtx::attention_with(Reason)`.
   Not needed by the types chosen; recorded for whoever builds a job type.
2. **Viewers can't call a block's methods.** Every `/api/blocks/N/call/*` is
   `Role::Editor`, and `/api/fs/*` is owner-only. A file or diff block in a
   shared session must put what a viewer reads in its pushed state (the
   file's lines, the open file's hunks), or the contract grows read-only
   methods a viewer may call. M11 takes the first: state holds what's drawn,
   capped, and methods only change what's drawn (editor role).
3. **Not every block has a log worth keeping.** A file or diff block's source
   of truth is the file and the repo. Its log holds only what it was pointed
   at, when; the contract's "a log" stays, as an event log.
4. **A block doesn't know whether anyone is looking.** `fs.watch` polls a
   machine every 3 s and so keeps it awake. A file or diff block on a VM tab
   must watch only while some client draws it, or a forgotten block keeps a
   sprite running forever. The contract needs a *drawn* signal (clients
   already say which panes they attach; summaries-only clients don't count),
   and these blocks watch only while it's on.

## Decision (2026-10-02)

- **M11: build a cut, next.** A diff block and a file block, phone first.
  Cut from #8: split view on the desktop (unified everywhere; M27's
  code-server has the desktop's split diff), and the ACP tool-call diff
  source (a tool call gets **Open file** at its location instead; M28's diff
  card already shows Claude Code's pending edit). Added: a file list with
  +/− first (how agents read changes), and watching only while drawn (gap 4).
- **M10: not as a block type.** Close #7's job and service blocks with these
  numbers. Its *Done when* ("fails, shows on the phone, retry from the phone
  succeeds") moves onto terminals, as M24's **Rerun** action, which M24 left
  out. Small; it can ride with M11 or go on its own.
- **No notes block.**
- **Contract changes:** gaps 2, 3 and 4 above go into M11. Gap 1 waits for a
  type that needs it.

### Revisit when

- someone keeps a terminal open only to watch a dev server or a log, or a
  dev server in a VM tab dies across a wake and has to be restarted by hand
  (then: the service block, with gap 1);
- a CI or hal0 job is watched from illogical rather than its own page (then:
  a job block with adapters);
- the daily daemon has two weeks of history: rerun this spike's counts.

## What M11 (cut) must deliver

**Diff block** (`type: diff`):

- Config `{host, repo, rev_a?, rev_b?}`: no revs is the working tree
  (staged, unstaged and untracked) against `HEAD`; one rev is that rev
  against the working tree; two is a range. `host` is any pane's host, so a
  VM tab's machine, an M4a peer or local.
- Computed on the host: `git` locally or by a peer's daemon, through
  `Provider::run` on a provider-only machine. Capped (a file's diff over
  ~256 KB shows as "too big, open file"; binary files as "binary").
- Draws a file list with +/− counts first, a file expands to unified hunks
  with highlighting (reusing M28's CodeMirror chunk). Every line has **Open
  file** at that line. Unified on the phone and the desktop.
- Live: re-diffs when the work tree changes (`fs.watch` on the repo, or a
  poll of `git status --porcelain`), only while some client draws the block
  (gap 4).
- State carries the file list and the expanded files' hunks, so viewers of a
  shared session see it with no method calls (gap 2); `refresh`, `file`,
  `open` need editor.
- `capture --text` is the unified diff; `describe` the state; the log
  records what it was pointed at (gap 3).

**File block** (`type: file`):

- Config `{host, path, line?}`, read through M7's `fs.read` in ranges (so it
  works on VMs and peers), read-only, highlighted, with line numbers, in the
  follow view's CodeMirror and colours; scrolled to `line` and marked.
- Follows `fs.watch` while drawn (gap 4), keeping the marked line's place
  when lines above it change.
- `capture --text` is the file; `goto{line}`, `open{path}` change it.

**Ways in:**

- `illogical diff [--host H | %N] [REV_A [REV_B]]` (the repo is `%N`'s cwd's
  git root by default) and `illogical view %N:PATH[:LINE]` / `mN:PATH`, which
  print the new block like `open` does; the same as MCP tools (M16).
- **Changes** on a tab's and a pane's menu and on a swarm tile with a project
  (M23), opening a diff block beside it on that pane's host and repo.
- An agent block's tool call with a location gets **Open file**.

**Plus (M10's remainder): Rerun.** M24's `failed` reason on a terminal gets
a `rerun` action: it types the failed command into the pane again when its
shell is idle (M3's state, as `cd` does), from the badge, the phone's
*Needs you* and the push. Editor role.

**Done when:**

- from the phone, on a VM tab where an agent (an agent block, or Claude Code
  in a terminal) has changed files, **Changes** opens a diff block listing
  them with +/−; tapping a hunk's line opens a live file block at that line;
- both keep updating while the agent keeps editing, and stop watching (the
  VM can sleep) once no client draws them;
- a viewer of a shared session sees both and can't change what they show;
- `capture --text`, `describe`, `illogical diff` and `illogical view` work on
  a local host and a VM;
- a build that fails in a VM tab's terminal shows as *Failed* on the phone,
  and **Rerun** from the phone runs it again in that pane.

**Tests to expect:** a daemon test per block (working tree, a range, an
untracked file, a binary, the caps; a file followed through edits; no watch
while undrawn; a viewer's state and refused calls), a Rerun test beside
`attention.rs`'s, and a Playwright spec on a phone viewport against a VM tab
(the `vm-dev-server.spec` pattern) with an agent stand-in editing files.

**Not in it:** split view, ACP tool-call diffs as a source, accepting or
reverting hunks (read-only, as #8 said), job and service blocks.
