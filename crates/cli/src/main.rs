//! `illogical`: drive illogicald from a shell or a script. Every command
//! talks to the daemon's HTTP API over its Unix socket (or another daemon's
//! URL, with `--host`); `--json` prints the API's answers as they are, for
//! programs.

mod ask;
mod attach;
mod control;
mod fountain_runner;
mod fs;
mod hook;
mod hooks;
mod hosts;
mod http;
mod mcp;
// The daemon's named pipe as a stream (Windows).
#[cfg(windows)]
mod pipe;
mod ssh;
mod term;
#[cfg(unix)]
mod tmux;
mod tui;
mod wake;

// tmux's control mode (`-CC`) front is for iTerm2 over ssh to a Unix box.
#[cfg(not(unix))]
mod tmux {
    pub fn run(_: crate::http::Target, _: &[String]) -> anyhow::Result<i32> {
        anyhow::bail!("tmux control mode is for Unix boxes (iTerm2 over ssh)")
    }
}

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use http::{enc, request};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(version, about = "Drive illogicald: panes you can script")]
struct Cli {
    /// The daemon's socket [default: $ILLOGICAL_SOCK, else
    /// $XDG_STATE_HOME/illogical/sock].
    #[arg(long, global = true, env = "ILLOGICAL_SOCK")]
    socket: Option<PathBuf>,
    /// Talk to another daemon: a name from the local daemon's host list or
    /// from control's directory once this CLI is logged in (`illogical
    /// hosts` lists both), or a URL.
    #[arg(long, global = true, conflicts_with = "ssh")]
    host: Option<String>,
    /// Talk to the daemon on a box you can ssh into (`user@box`, or a Host
    /// from ~/.ssh/config), with your own ssh. Offers to install illogical
    /// there if it's missing.
    #[arg(long, global = true, value_name = "DEST")]
    ssh: Option<String>,
    /// Print the API's JSON instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Command,
}

/// `mN`, `N` or `local`: a machine, not a daemon's name.
fn looks_like_machine(s: &str) -> bool {
    s == "local" || s.trim_start_matches('m').parse::<u32>().is_ok()
}

/// Panes are `%N` or `N`; commands default to the pane they run in
/// ($ILLOGICAL_PANE).
#[derive(Clone, Debug)]
struct Pane(u32);

impl std::str::FromStr for Pane {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        s.trim_start_matches('%').parse().map(Pane).map_err(|_| format!("not a pane: {s} (want %N or N)"))
    }
}

#[derive(Subcommand)]
enum Command {
    /// List panes.
    Ls,
    /// Run a command in a new tab (or split); prints its pane.
    Run {
        /// Session name or id (created if missing).
        #[arg(long)]
        session: Option<String>,
        /// Split this pane instead of opening a tab.
        #[arg(long)]
        split: Option<Pane>,
        /// With --split: run where that pane runs (its VM tab's machine, or
        /// the sandbox it has a shell on) instead of this host.
        #[arg(long, requires = "split")]
        join: bool,
        /// Where it starts (with --join, or on a VM: a directory there).
        #[arg(long)]
        cwd: Option<String>,
        /// After a restart: shell, none, rerun, rerun-ask, or hook:COMMAND.
        #[arg(long)]
        policy: Option<String>,
        /// Wait for it to finish and exit with its exit code.
        #[arg(long)]
        wait: bool,
        /// On a new throwaway VM, deleted when the pane closes. Without a
        /// command: a shell on one.
        #[arg(long)]
        vm: bool,
        /// In a new tab whose panes share one throwaway VM (splits join it),
        /// deleted when the tab closes.
        #[arg(long, conflicts_with_all = ["vm", "split"])]
        vm_tab: bool,
        /// The VM's image (with --vm or --vm-tab).
        #[arg(long)]
        image: Option<String>,
        /// On a sandbox that exists (`illogical sandboxes`), over a plain
        /// exec with no daemon there: disposable, and the sandbox stays
        /// when the pane closes. Without a command: a shell.
        #[arg(long, conflicts_with_all = ["vm", "vm_tab", "image"])]
        sandbox: Option<String>,
        /// With --host: run it on that host but put it in this daemon's
        /// layout (a tab here, or beside --split, a pane here), as a remote
        /// pane. On the host it's in a session named after this daemon.
        #[arg(long, conflicts_with_all = ["join", "vm_tab"])]
        home: bool,
        /// One argument is a shell command line (`'make && ./app'`);
        /// several are a program and its arguments, quoted as given. None
        /// (with --cwd, --join, --home or a VM): a shell.
        #[arg(trailing_var_arg = true, required_unless_present_any = ["vm", "vm_tab", "sandbox", "join", "cwd", "home"])]
        command: Vec<String>,
    },
    /// Machines that panes run on (VM panes).
    Machines,
    /// Files on a host, read-only: `ls`, `stat`, `cat`, `watch`, `recent`.
    /// `%N:PATH` is on the host pane %N runs on (its VM), `mN:PATH` on
    /// machine N.
    Fs {
        #[command(subcommand)]
        cmd: fs::FsCmd,
    },
    /// Type `cd DIR` into a pane's shell, if it's waiting at its prompt.
    Cd { pane: Pane, dir: String },
    /// A block's type, place and state (any type). With `--detection`, how
    /// the screen of the agent in a terminal pane reads: each rule, the
    /// text it looked at, and which one fired. `describe --agents`: the
    /// agents configured on this machine (`chant audit --agents`), which
    /// decide whose screen rules run here.
    Describe {
        #[arg(required_unless_present = "agents")]
        block: Option<Pane>,
        #[arg(long, requires = "block")]
        detection: bool,
        #[arg(long, conflicts_with_all = ["block", "detection"])]
        agents: bool,
        /// With `--agents`: ask chant again first.
        #[arg(long, requires = "agents")]
        refresh: bool,
    },
    /// Call one of a block's methods, e.g. `call %4 navigate '{"url":"…"}'`.
    Call {
        block: Pane,
        method: String,
        /// Arguments as JSON.
        args: Option<String>,
    },
    /// Open a browser block: a port (`:5173/path`) on its machine, or a web
    /// page (`https://…`).
    Open {
        /// `:PORT[/path]`, or a URL.
        target: String,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`. In a VM tab the block is on the tab's machine.
        #[arg(long)]
        split: Option<String>,
        /// The machine whose port it is: `mN` (see `illogical machines`), or
        /// `local` for this host [default: the VM tab's, when splitting
        /// there; else this host]. (`--host` is another daemon.)
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Open an editor block: VS Code (code-server) on a folder, or on a
    /// file in its project, on the machine this pane runs on. Prints its
    /// block.
    Edit {
        /// A folder or file, optionally `FILE:LINE` [default: here].
        path: Option<String>,
        /// The line to show.
        #[arg(long)]
        line: Option<u32>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        /// The machine it runs on: `mN`, or `local` for this host [default:
        /// this pane's machine]. (`--host` is another daemon.)
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// What changed in a git repository (M11): opens a diff block, prints
    /// it, then its files with +/−. No revisions: the working tree (staged,
    /// unstaged, untracked) against HEAD; one: against that; two: the
    /// range. `%N` first: the repository pane %N is in, on its machine.
    Diff {
        /// `[%N] [REV_A [REV_B]]`.
        args: Vec<String>,
        /// The repository (any directory in it) [default: %N's directory,
        /// or this one].
        #[arg(long)]
        repo: Option<String>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Show a file in a file block (M11), read-only and followed live:
    /// `PATH[:LINE]` here, `%N:PATH[:LINE]` on the host pane %N runs on
    /// (relative to its directory), `mN:PATH[:LINE]` on machine N. Prints
    /// its block.
    View {
        spec: String,
        /// The line to mark and show.
        #[arg(long)]
        line: Option<u32>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Show a chant workspace as a block (M34): its members as cards to
    /// open shells, agents and diffs on, its records, and the gates waiting
    /// for a person, which are attention you approve (`call %N approve`).
    /// Read through the workspace's own chant. Prints the block, then its
    /// members and gates.
    Workspace {
        /// The workspace root, holding chant.workspace.json [default: here].
        dir: Option<String>,
        /// The environment whose gates and releases to read.
        #[arg(long, default_value = "local")]
        env: String,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Show a pull request as a block (M36: Forgejo, through your `tea`
    /// login; M38: GitHub, through `gh`'s; M39: a GitLab merge request,
    /// through your `glab` login, or read-only without one): its checks,
    /// reviews and timeline, and what it waits on you for. `URL`,
    /// `OWNER/REPO#N`, `GROUP/PROJECT!N`, or `N` in this directory's
    /// repository. Prints the block, then the PR as text. `pr
    /// comment|review|merge|rerun %N`
    /// write to it; run by an agent (CLAUDECODE or AI_AGENT set), a write
    /// is a draft that waits for a person to send it.
    #[command(args_conflicts_with_subcommands = true)]
    Pr {
        #[command(subcommand)]
        cmd: Option<PrCmd>,
        /// The pull request.
        target: Option<String>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Your Fountain agents as a catalog block (M43), read with your own
    /// `fountain` login (FOUNTAIN_API_KEY or ~/.fountain/credentials): a
    /// card per agent, where it comes from (agent-specs, hand-made, an
    /// app), filters, and Run on Fountain / Spec. Prints the block, then the
    /// list. `fountain agents [QUERY]` lists them here without a block.
    /// `fountain --view runner` (M45b): this host as the account's runner
    /// instead, with its sandboxes (Follow, Changes, Shell).
    #[command(args_conflicts_with_subcommands = true)]
    Fountain {
        #[command(subcommand)]
        cmd: Option<FountainCmd>,
        /// What the block shows: catalog (the agents) or runner (this host
        /// as the Fountain runner, its status and its sandboxes).
        #[arg(long, value_parser = ["catalog", "runner"], default_value = "catalog")]
        view: String,
        /// Start with this search (names, descriptions, skills, MCP servers).
        #[arg(long, short)]
        query: Option<String>,
        /// Start with this source: agent-specs, hand or app.
        #[arg(long)]
        source: Option<String>,
        /// The credentials profile (default: FOUNTAIN_PROFILE, else default).
        #[arg(long)]
        profile: Option<String>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Show an issue as a block (M37: Forgejo, through your `tea` login):
    /// its labels, assignees, linked pull requests and timeline. `URL`,
    /// `OWNER/REPO#N`, or `N` in this directory's repository. `issue new`
    /// opens one (run by an agent, it's a draft a person sends); `issue
    /// comment %N` comments; `issue agent %N` starts an agent on it in a
    /// worktree and branch of its own, in a tab with the issue.
    #[command(args_conflicts_with_subcommands = true)]
    Issue {
        #[command(subcommand)]
        cmd: Option<IssueCmd>,
        /// The issue.
        target: Option<String>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Type a pane's failed command again (M24's `failed`), once its shell
    /// is waiting at its prompt.
    Rerun { pane: Option<Pane> },
    /// Claude Code conversations on this machine, from a terminal or the
    /// desktop app's Code tab (M33). `open` shows one as an agent block;
    /// `illogical agent --resume ID` continues one.
    Claude {
        #[command(subcommand)]
        cmd: ClaudeCmd,
    },
    /// Your studio (M35: arugula-salad's): `login URL` keeps a studio
    /// token in the daemon (read from stdin), `logout` forgets it, and
    /// `follower APP` keeps a hud follower link for an app's box.
    Studio {
        #[command(subcommand)]
        cmd: Option<StudioCmd>,
    },
    /// Open a studio app's box as a block (M35); prints its block. With no
    /// name, lists your apps.
    App {
        /// The app's name in studio.
        name: Option<String>,
        /// Split a block instead of opening a tab: `right` for the one this
        /// runs in, or `%N`.
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
        /// Follow the box with the app's follower link (`illogical studio
        /// follower`), so hud is told who answered. The default whenever a
        /// link is kept for the app.
        #[arg(long)]
        follower: bool,
    },
    /// Editors in the swarm (M28): VS Code, Cursor or nvim that joined, and
    /// editor blocks. `editors install` adds illogical's extension to VS
    /// Code or Cursor here (in a Remote-SSH window's terminal: there).
    Editors {
        #[command(subcommand)]
        cmd: Option<EditorsCmd>,
    },
    /// illogicald as Claude Code's IDE (M28): its port, and which IDE gets
    /// Claude Code's diffs (`--diffs illogical`, or another IDE's name as
    /// it registered, e.g. "Visual Studio Code").
    Ide {
        #[arg(long)]
        diffs: Option<String>,
    },
    /// Standing permission rules (#166): what agent blocks on this daemon
    /// allow without asking, made by "Always" for a directory or for every
    /// block. `--forget N` forgets one; `--forget-all`, all of them.
    Rules {
        #[arg(long, conflicts_with = "forget_all")]
        forget: Option<usize>,
        #[arg(long)]
        forget_all: bool,
    },
    /// The shell environment blocks that run your tools get (your login
    /// shell's, read once): its PATH. `--refresh` reads it again, after
    /// you change an rc file.
    ShellEnv {
        #[arg(long)]
        refresh: bool,
    },
    /// Start an agent block (Claude Code by default) and send it a prompt;
    /// prints its block. Then: `wait %N --idle`, `tail %N`, `call %N approve`.
    Agent {
        /// Any ACP agent server, by its command line.
        #[arg(long, conflicts_with_all = ["fountain", "codex"])]
        acp: Option<String>,
        /// A Fountain agent (name or id), run in Fountain's sandbox.
        #[arg(long, conflicts_with = "codex")]
        fountain: Option<String>,
        /// Claude Code wearing a Fountain agent (name or id), on this host:
        /// its system prompt, skills and MCP servers (M44).
        #[arg(long = "as", value_name = "AGENT", conflicts_with_all = ["acp", "fountain", "codex", "vm", "machine"])]
        as_fountain: Option<String>,
        /// Codex instead of Claude Code.
        #[arg(long)]
        codex: bool,
        /// Fountain: a vault for its secrets (with `--as`: whose agent-specs
        /// mapping its `${VAR}`s go through, over its environment's).
        #[arg(long)]
        vault: Option<String>,
        /// A model to switch to (e.g. `haiku`).
        #[arg(long)]
        model: Option<String>,
        /// An MCP server for the session, `NAME=COMMAND LINE` (stdio; may be
        /// repeated). Its forms and sign-in links show as cards.
        #[arg(long = "mcp", value_name = "NAME=COMMAND")]
        mcp: Vec<String>,
        /// Approve this tool's requests without asking (`Read`, `Edit`,
        /// `Bash`, …; may be repeated), as "always" on its card does.
        #[arg(long, value_name = "TOOL")]
        allow: Vec<String>,
        /// The permission mode its session starts in: `default`,
        /// `acceptEdits`, `plan`, `auto` (Claude Code's), or the agent's own.
        #[arg(long, value_name = "MODE", conflicts_with = "fountain")]
        permission_mode: Option<String>,
        /// Claude Code with your settings (allow and deny lists, default
        /// mode, `CLAUDE.md`), but none of their hooks.
        #[arg(long, conflicts_with_all = ["acp", "fountain", "codex"])]
        user_settings: bool,
        /// On a new throwaway VM of its own.
        #[arg(long, conflicts_with = "machine")]
        vm: bool,
        /// On this existing machine (`m3` or `3`). (`--host` is another
        /// daemon.)
        #[arg(long)]
        machine: Option<String>,
        /// Where it works [default: here, or the VM's home].
        #[arg(long)]
        cwd: Option<String>,
        /// Continue a Claude Code conversation from a terminal or the
        /// desktop app (its id, or the start of it; `illogical claude ls`).
        #[arg(long, value_name = "ID", conflicts_with_all = ["acp", "fountain", "codex", "vm", "machine", "fork", "as_fountain"])]
        resume: Option<String>,
        /// Fork a Claude Code conversation and go on in the fork (for one
        /// that's still open somewhere else).
        #[arg(long, value_name = "ID", conflicts_with_all = ["acp", "fountain", "codex", "vm", "machine", "as_fountain"])]
        fork: Option<String>,
        #[arg(long)]
        session: Option<String>,
        /// Split this block instead of opening a tab.
        #[arg(long)]
        split: Option<Pane>,
        /// Wait for the turn to end (or to need you); prints the transcript.
        #[arg(long)]
        wait: bool,
        /// The first prompt.
        prompt: Vec<String>,
    },
    /// Type text into a pane (`-` reads stdin). With `--wait`, it's a
    /// prompt for the agent there (Claude Code or Codex in the terminal, or
    /// an agent block): sent with Enter, then waited through. Prints what
    /// it came to and exits 0 when the turn ended, 2 when it needs someone
    /// (or already did, so nothing was typed), 3 when it stalled (no sign
    /// of work), 4 still running at --timeout.
    Send {
        pane: Pane,
        #[arg(required = true)]
        text: Vec<String>,
        /// Press Enter afterwards.
        #[arg(short, long)]
        enter: bool,
        /// Prompt the agent there and wait for its turn.
        #[arg(long)]
        wait: bool,
        /// With --wait: it's waiting on a question and this answers it.
        #[arg(long, requires = "wait")]
        answering: bool,
        /// With --wait: seconds before giving up waiting (default 100).
        #[arg(long, requires = "wait")]
        timeout: Option<f64>,
    },
    /// Press named keys: C-c, M-x, Up, Enter, F5, Space, ...
    Keys {
        pane: Pane,
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Click, press, release or drag at a cell (from 1,1).
    Mouse {
        pane: Pane,
        x: u16,
        y: u16,
        /// left, middle, right, wheel_up, wheel_down
        #[arg(long, default_value = "left")]
        button: String,
        /// click, press, release, drag
        #[arg(long, default_value = "click")]
        action: String,
    },
    /// Print a pane's output.
    Tail {
        pane: Option<Pane>,
        /// Keep printing new output.
        #[arg(short, long)]
        follow: bool,
        /// Start at this stream offset.
        #[arg(long, conflicts_with = "last_command")]
        from: Option<u64>,
        /// The output of the last (or current) command.
        #[arg(long)]
        last_command: bool,
        /// Strip colors and other escape sequences.
        #[arg(long)]
        text: bool,
        /// A pane of this host, from the history it synced here (once it's
        /// gone, say).
        #[arg(long, value_name = "HOST", conflicts_with_all = ["follow", "last_command"])]
        synced: Option<String>,
    },
    /// Wait for a command to finish, the program to exit, or output to match.
    /// Exits with the command's exit code; 124 on timeout.
    Wait {
        pane: Option<Pane>,
        #[arg(long, group = "until")]
        command_end: bool,
        #[arg(long, group = "until")]
        exit: bool,
        /// A regular expression to wait for in the output.
        #[arg(long = "match", group = "until")]
        matching: Option<String>,
        /// Until it's no longer working (an agent's turn ended, or it needs
        /// you): any block.
        #[arg(long, group = "until")]
        idle: bool,
        /// Until it needs you (an agent asks to run something).
        #[arg(long, group = "until")]
        needs_input: bool,
        /// Seconds.
        #[arg(long)]
        timeout: Option<f64>,
    },
    /// Use a pane from this terminal (Ctrl-] to detach).
    Attach { pane: Option<Pane> },
    /// The daemon's tabs and splits in this terminal, with a sidebar of
    /// sessions, tabs and what needs you. Ctrl-] is the menu key.
    Tui {
        /// Start in this session (name or id); made if there's none.
        #[arg(long)]
        session: Option<String>,
    },
    /// Export a pane's history as an asciicast (`asciinema play`).
    Export {
        pane: Option<Pane>,
        #[arg(long, default_value_t = true)]
        cast: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// The pane's foreground process.
    Process { pane: Option<Pane> },
    /// What a pane shows: the screen, the scrollback or the last command.
    Capture {
        pane: Option<Pane>,
        #[arg(long, group = "format")]
        ansi: bool,
        #[arg(long, group = "format")]
        html: bool,
        #[arg(long, group = "scope")]
        scrollback: bool,
        #[arg(long, group = "scope")]
        last_command: bool,
    },
    /// Events as they happen (NDJSON).
    Events {
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        pane: Option<Pane>,
        /// Comma-separated: command_start, command_end, notify, attention, ...
        #[arg(long = "type")]
        types: Option<String>,
        /// Without --follow: how far back (e.g. 30m, 2h).
        #[arg(long)]
        since: Option<String>,
    },
    /// Close a pane (ending what runs in it); its output stays in history.
    Close {
        #[arg(required = true)]
        panes: Vec<Pane>,
    },
    /// Claude Code's PreToolUse hook on AskUserQuestion: show its questions
    /// as a card beside this pane (every client, with a push), wait, and
    /// print the answer for Claude Code. Outside an illogical pane, or
    /// "Answer in terminal": no output, so Claude Code shows its picker.
    Ask,
    /// Claude Code's hooks (M29): `PermissionRequest` becomes an approval
    /// card anyone who may answer can allow or deny; other events close a
    /// card the terminal answered first. Outside an illogical pane: nothing.
    Hook,
    /// Claude Code's background (asyncRewake) `Stop` and `SessionStart`
    /// hook: wait for a follow-up someone sends the agent, and wake it with
    /// it (exit 2). A session nobody drives (`claude -p`, the SDK) isn't
    /// held: it exits 0 at once.
    Inbox,
    /// Put those hooks in Claude Code's settings.json (`install`), or say
    /// which are there (`status`). Nothing else in the file is touched.
    Hooks {
        #[command(subcommand)]
        cmd: hooks::HooksCmd,
    },
    /// What wants you, and why (M24); or, given a state, tell illogical
    /// whether this pane needs you (for agent hooks, which pass their JSON
    /// on stdin: its `message` becomes the headline).
    Attention {
        /// needs-input, done, working or idle; none lists what wants you.
        state: Option<String>,
        #[arg(long)]
        pane: Option<Pane>,
    },
    /// Commands run in any pane, including recently closed ones.
    History {
        #[arg(long)]
        pane: Option<Pane>,
        /// Only commands that failed.
        #[arg(long)]
        failed: bool,
        /// e.g. 30m, 2h, 7d.
        #[arg(long)]
        since: Option<String>,
        /// Only commands run in this directory (or below).
        #[arg(long)]
        cwd: Option<String>,
        /// Only commands matching this regular expression.
        #[arg(long = "match")]
        matching: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Another host's history, as synced here (`all`: every host's).
        #[arg(long, value_name = "HOST")]
        synced: Option<String>,
    },
    /// A pane's commands and who ran each; `--who`: who typed in it over
    /// time (each handoff).
    Log {
        pane: Option<Pane>,
        #[arg(long)]
        who: bool,
    },
    /// Search the output of every pane.
    Search {
        re: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Another host's history, as synced here (`all`: every host's).
        #[arg(long, value_name = "HOST")]
        synced: Option<String>,
    },
    /// A read-only link to a pane: whoever opens it on the tailnet sees it
    /// live and can't type, resize or see anything else.
    ///
    /// With --guest: an invite for someone with only OpenSSH. It prints an
    /// `ssh` command to send them, with this machine's host key pinned.
    /// Read-only unless --rw; one login unless --reusable. A machine joined
    /// to control with no --addr (or --guest-ssh-host) is reached through
    /// control's ssh jump host, so a box behind NAT works too.
    Share {
        pane: Option<Pane>,
        /// How long it works (e.g. 30m, 2h, 7d; a week at most, a day with
        /// --guest, two hours with --rw).
        #[arg(long, default_value = "1h")]
        ttl: String,
        /// An ssh invite instead of a link (M65). (`--ssh` is taken: it
        /// reaches a box over ssh, so `--ssh box share --guest` makes an
        /// invite there.)
        #[arg(long)]
        guest: bool,
        /// They may type, when nobody else is driving the pane.
        #[arg(long, requires = "guest")]
        rw: bool,
        /// Good for any number of logins until it ends.
        #[arg(long, requires = "guest")]
        reusable: bool,
        /// What to call them on their input [default: guest].
        #[arg(long, requires = "guest")]
        name: Option<String>,
        /// The address they should ssh to [default: the daemon's
        /// --guest-ssh-host, else its hostname]. Not through control.
        #[arg(long = "addr", requires = "guest", conflicts_with = "relay")]
        addr: Option<String>,
        /// Through control's ssh jump host, or fail [default: when this
        /// machine is joined to control and no address is set].
        #[arg(long, requires = "guest")]
        relay: bool,
    },
    /// ssh invites that still work (`share --guest`); `guests revoke ID` ends
    /// one and cuts off anyone using it.
    Guests {
        #[command(subcommand)]
        cmd: Option<SharesCmd>,
    },
    /// Who else can reach which sessions: `access` lists grants,
    /// `access grant SESSION WHO ROLE`, `access revoke SESSION WHO`, `access
    /// log`. WHO is a tailnet login (or `account:ID` from control).
    Access {
        #[command(subcommand)]
        cmd: Option<AccessCmd>,
    },
    /// Share links that still work; `shares revoke ID` ends one.
    Shares {
        #[command(subcommand)]
        cmd: Option<SharesCmd>,
    },
    /// History other hosts synced here (kept encrypted).
    Synced {
        #[command(subcommand)]
        cmd: Option<SyncedCmd>,
    },
    /// Open this machine's page in your browser, signed in. Programs and
    /// browsers on this machine show the daemon's local token; this opens
    /// a sign-in link that gives your browser it (once: it stays signed
    /// in). `--print` prints the link instead (it holds the token).
    Web {
        #[arg(long)]
        print: bool,
    },
    /// Install the daemon: `illogicald install` with these arguments (e.g.
    /// `--tailnet file:KEY --home URL --join TOKEN` in a sandbox).
    Install {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Add a machine to your account on illogical control, so the web, the
    /// phone and other machines reach it through control (M52). With
    /// `--ssh user@box`: that box, set up over ssh first (illogical
    /// installed, its daemon kept running after you log out); its code
    /// shows here, to approve from a signed-in device. Without: this machine.
    Join {
        /// The control [default: https://control.illogical.widgets.wtf].
        url: Option<String>,
        /// Its name in the directory [default: its hostname].
        #[arg(long)]
        name: Option<String>,
        /// Join it to a team (its id), not your account alone.
        #[arg(long)]
        team: Option<String>,
        /// The account's fingerprint, as the approving device shows it:
        /// checked instead of asking.
        #[arg(long, value_name = "FINGERPRINT")]
        account: Option<String>,
    },
    /// Make this CLI one of your devices on illogical control (M49), so
    /// `--host NAME` reaches every machine on your account, directly or
    /// through control's relay. Shows a code to approve on a signed-in
    /// device.
    Login {
        /// The control [default: the one this machine's daemon joined, else
        /// https://control.illogical.widgets.wtf].
        url: Option<String>,
        /// What the account calls this terminal [default: illogical CLI on
        /// <hostname>].
        #[arg(long)]
        name: Option<String>,
        /// The account's fingerprint, as the approving device shows it:
        /// checked instead of asking.
        #[arg(long, value_name = "FINGERPRINT")]
        account: Option<String>,
    },
    /// Forget this CLI's key for control (`illogical login` makes a new one).
    Logout,
    /// On a box a client reaches over ssh (`--ssh`): join stdin and stdout
    /// to this daemon's socket. Clients run it; people don't.
    #[command(hide = true)]
    Bridge {
        /// Print what's installed and whether the daemon answers, as JSON.
        #[arg(long)]
        probe: bool,
    },
    /// Other daemons to switch to (this daemon's host list).
    Hosts {
        #[command(subcommand)]
        cmd: Option<hosts::HostsCmd>,
    },
    /// The sandbox provider's sandboxes (this daemon's): open a shell on
    /// one (`run --sandbox`), or make a daemon resident there.
    Sandboxes {
        #[command(subcommand)]
        cmd: Option<hosts::SandboxesCmd>,
    },
    /// An MCP server on stdio, for agents that start one as a command
    /// (`claude mcp add illogical -- illogical mcp`): illogical's tools,
    /// bridged to the daemon's `/mcp`. `mcp token` makes tokens for
    /// clients that reach `/mcp` over HTTP without a tailnet identity.
    Mcp {
        /// A token to send (an agent block's, or a client token for a
        /// daemon this machine has no identity on).
        #[arg(long, env = "ILLOGICAL_MCP_TOKEN", hide_env_values = true)]
        token: Option<String>,
        #[command(subcommand)]
        cmd: Option<McpCmd>,
    },
    /// Be a tmux server in control mode for iTerm2 (and other tmux `-CC`
    /// clients): `illogical tmux -CC [attach -t SESSION | new -s NAME]`.
    /// Linked or installed as `tmux`, the CLI does this by itself.
    Tmux {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Make a token for an MCP client to use `/mcp` over HTTP (as
    /// `Authorization: Bearer TOKEN`), printed once; `--list` shows them,
    /// `--revoke NAME` cuts one off.
    Token {
        /// What to call it (the client, or the machine it's on). A token
        /// with the same name is replaced.
        #[arg(long, default_value = "client")]
        name: String,
        /// `full` (every tool) or `read` (the read-only tools only).
        #[arg(long, default_value = "full")]
        scope: String,
        #[arg(long, conflicts_with = "revoke")]
        list: bool,
        #[arg(long, value_name = "NAME")]
        revoke: Option<String>,
    },
}

#[derive(Subcommand)]
enum ClaudeCmd {
    /// List them, newest first.
    Ls {
        /// Everything: `claude -p` and SDK runs, archived ones, ones whose
        /// folder is gone.
        #[arg(long)]
        all: bool,
        /// Only ones open in a terminal, the desktop app or a pane now.
        #[arg(long)]
        live: bool,
        /// Only ones under this folder.
        #[arg(long)]
        cwd: Option<String>,
        /// How many [default: 30].
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
        /// Words in the title, prompts or folder.
        words: Vec<String>,
    },
    /// Show one as an agent block (stopped, its transcript as it grows);
    /// prints the block. The block that has it already, if one does.
    Open {
        /// Its id, or the start of it.
        id: String,
        #[arg(long)]
        session: Option<String>,
        /// Split this block instead of opening a tab.
        #[arg(long)]
        split: Option<Pane>,
    },
}

/// Writes to a PR block (M36).
#[derive(Subcommand)]
enum PrCmd {
    /// Comment on it.
    Comment { block: Pane, body: String },
    /// Review it: approve, request_changes or comment.
    Review { block: Pane, event: String, body: Option<String> },
    /// Merge it (merge, rebase, rebase-merge, squash, fast-forward-only;
    /// GitHub: merge, squash or rebase).
    Merge {
        block: Pane,
        #[arg(long)]
        style: Option<String>,
    },
    /// Rerun its failed checks (GitHub: each red workflow run's failed
    /// jobs; Forgejo has no API for it).
    Rerun { block: Pane },
}

/// Fountain: the catalog (M43) and this machine as the runner (M45).
#[derive(Subcommand)]
enum FountainCmd {
    /// This machine as the account's Fountain runner (M45): `install`,
    /// `status`, `adopt` (the root half is `scripts/fountain-runner-setup.sh`).
    Runner {
        #[command(subcommand)]
        cmd: fountain_runner::RunnerCmd,
    },
    /// List your agents here, one line each, without opening a block.
    Agents {
        /// Words to look for (names, descriptions, skills, MCP servers).
        query: Option<String>,
        /// agent-specs, hand or app.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

/// Issues (M37).
#[derive(Subcommand)]
enum IssueCmd {
    /// Open a new issue in this directory's repository (or --repo).
    New {
        #[arg(long, short)]
        title: String,
        #[arg(long, short)]
        body: Option<String>,
        /// OWNER/REPO.
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Comment on an issue block's issue.
    Comment { block: Pane, body: String },
    /// Start an agent on it: a worktree and branch `iN-<slug>`, the agent
    /// there with the issue as its prompt, and the two in a tab.
    Agent {
        block: Pane,
        /// claude (the default), codex, fountain or acp.
        #[arg(long)]
        agent: Option<String>,
        /// More for its prompt.
        #[arg(long)]
        prompt: Option<String>,
        /// The clone to work in (default: where the issue was opened).
        #[arg(long)]
        dir: Option<String>,
    },
}

#[derive(Subcommand)]
enum StudioCmd {
    /// Keep a studio token in the daemon (mode 0600, never sent to a
    /// client). The token is read from stdin, or asked for.
    Login {
        /// The studio, e.g. `https://studio.example`.
        url: String,
    },
    /// Forget the token, and every follower link.
    Logout,
    /// Keep the follower link the box's owner made with `hud share --role
    /// follower` (read from stdin), or with `--forget`, drop it.
    Follower {
        app: String,
        #[arg(long)]
        forget: bool,
    },
}

#[derive(Subcommand)]
enum EditorsCmd {
    /// Write illogical's VS Code extension (a VSIX) to a file.
    Vsix {
        /// Where [default: illogical-editor-VERSION.vsix here].
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Install illogical's extension in VS Code or Cursor with their CLI
    /// (`code --install-extension`).
    Install {
        /// The editor's command [default: `code`, else `cursor`].
        #[arg(long)]
        with: Option<String>,
    },
}

#[derive(Subcommand)]
enum SharesCmd {
    /// End a share link now; anyone watching is cut off.
    Revoke { id: u32 },
}

#[derive(Subcommand)]
enum AccessCmd {
    /// Let someone reach a session: as a viewer (watch), an editor (drive
    /// its panes, make and close tabs and splits) or an owner. Takes effect
    /// at once.
    Grant { session: String, who: String, role: String },
    /// Take it away; they're cut off at once.
    Revoke { session: String, who: String },
    /// Every grant and revoke, oldest first.
    Log,
}

#[derive(Subcommand)]
enum SyncedCmd {
    /// Forget a host's synced history.
    Rm { name: String },
    /// Re-encrypt all synced history under a new key, and drop the old one.
    RotateKey,
}

/// Talking to another daemon (`--host`): this shell's pane and directory
/// mean nothing there.
static REMOTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pane we're running in, if it is on the daemon we're talking to.
fn env_pane() -> Option<u32> {
    if REMOTE.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    std::env::var("ILLOGICAL_PANE").ok().and_then(|v| v.parse().ok())
}

fn socket(cli: &Cli) -> PathBuf {
    cli.socket.clone().unwrap_or_else(default_socket)
}

fn default_socket() -> PathBuf {
    if let Some(s) = std::env::var_os("ILLOGICAL_SOCK") {
        return PathBuf::from(s);
    }
    // Windows: the daemon's named pipe, which it records beside its state.
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        let state = PathBuf::from(d).join("illogical").join("state");
        return match std::fs::read_to_string(state.join("sock.path")) {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
            _ => state.join("sock"),
        };
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("illogical");
    // A state directory too deep for a socket path puts it elsewhere.
    match std::fs::read_to_string(state.join("sock.path")) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
        _ => state.join("sock"),
    }
}

/// `illogical web`: the sign-in link, opened in a browser (or printed).
/// With `--ssh`, the box daemon's link: printed, since its page is on the
/// box, with how to forward its port from here.
fn web(sock: &http::Target, print: bool) -> anyhow::Result<i32> {
    let v = request(sock, "GET", "/api/signin-link", None)?.json()?;
    let Some(url) = v["url"].as_str() else { bail!("the daemon has no sign-in link: {v}") };
    let page = url.split("/auth?").next().unwrap_or(url);
    if print {
        println!("{url}");
        return Ok(0);
    }
    if let http::Target::Ssh(r) = sock {
        println!("The page is on {}. Open this in a browser here once its port is forwarded:\n\n  {url}\n", r.dest);
        if let Some(hint) = ssh_forward(page, &r.dest) {
            println!("{hint}\n");
        }
        println!("It holds {}'s local token: don't share it.", r.dest);
        return Ok(0);
    }
    // Windows: the URL handler itself (`cmd /c start` would split at `&`).
    let (opener, pre): (&str, &[&str]) = if cfg!(windows) {
        ("rundll32.exe", &["url.dll,FileProtocolHandler"])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let opened = (cfg!(any(target_os = "macos", windows))
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some())
        && std::process::Command::new(opener)
            .args(pre)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    if opened {
        println!("Opened {page} in your browser, signed in.");
    } else {
        println!("Open this in a browser on this machine (it signs the browser in, once):\n\n  {url}\n");
        if let Some(hint) = ssh_hint(page, std::env::var_os("SSH_CONNECTION").is_some()) {
            println!("{hint}\n");
        }
        println!("It holds this machine's local token: don't share it.");
    }
    Ok(0)
}

/// Over ssh there's no browser on this machine: forward its port from
/// the computer you're at, and the same link works there.
fn ssh_hint(page: &str, over_ssh: bool) -> Option<String> {
    if !over_ssh {
        return None;
    }
    let (port, addr) = page_port(page)?;
    Some(format!(
        "Over ssh? On the computer you're at, forward the port first, then open the link there:\n\n  ssh -L {port}:{addr} <this machine>"
    ))
}

/// `--ssh dest web`: the forward that makes dest's page reachable here.
fn ssh_forward(page: &str, dest: &str) -> Option<String> {
    let (port, addr) = page_port(page)?;
    Some(format!("Forward it with:\n\n  ssh -N -L {port}:{addr} {dest}"))
}

/// A page's port and address (`http://127.0.0.1:7681/` is 7681 and
/// 127.0.0.1:7681).
fn page_port(page: &str) -> Option<(&str, &str)> {
    let addr = page.strip_prefix("http://")?.trim_end_matches('/');
    Some((addr.rsplit_once(':')?.1, addr))
}

/// The pane given, or the one we're running in.
/// `src/main.rs:42` is a file and a line (when `check`, only if the file
/// is there and the whole name isn't).
fn file_line(p: &str, check: bool) -> (String, Option<u32>) {
    if let Some((file, line)) = p.rsplit_once(':')
        && let Ok(n) = line.parse::<u32>()
        && !file.is_empty()
        && (!check || (std::path::Path::new(file).exists() && !std::path::Path::new(p).exists()))
    {
        return (file.to_owned(), Some(n));
    }
    (p.to_owned(), None)
}

/// A path from here as a whole one.
fn absolute(p: &str) -> anyhow::Result<String> {
    let p = std::path::Path::new(p);
    let whole = if p.is_absolute() { p.to_owned() } else { std::env::current_dir()?.join(p) };
    Ok(whole.display().to_string())
}

/// `--split right` (the pane this runs in) or `--split %N`.
fn split_of(split: Option<&str>) -> anyhow::Result<Option<u32>> {
    Ok(match split {
        None => None,
        Some("right") => Some(here(None)?),
        Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
    })
}

/// What `send --wait` came to, for people, and its exit code.
fn prompted(pane: u32, r: &Value) -> (String, i32) {
    let q = |r: &Value| r["question"].as_str().unwrap_or("a question").to_owned();
    match r["result"].as_str().unwrap_or_default() {
        "done" => (format!("%{pane} finished its turn"), 0),
        "needs_input" => (format!("%{pane} asks: {}", q(r)), 2),
        "blocked" => (
            format!("%{pane} was already waiting on someone ({}); nothing was typed (--answering to answer it)", q(r)),
            2,
        ),
        "stalled" => {
            let screen = r["screen"].as_str().unwrap_or_default();
            (format!("%{pane} stalled: {}\n{screen}", r["why"].as_str().unwrap_or_default()), 3)
        }
        "still_running" => (format!("%{pane} is still working (illogical wait %{pane} --idle)"), 4),
        other => (format!("%{pane}: {other}"), 1),
    }
}

/// `describe %N --detection` for people: what fired, then each rule with
/// the text it saw, highest priority first.
fn detection_text(pane: u32, v: &Value) -> String {
    use std::fmt::Write;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let Some(agent) = v["agent"].as_str() else {
        return match v["command"].as_str() {
            Some(c) => format!("%{pane} runs `{c}`: no agent with screen rules\n"),
            None => format!("%{pane} is at its shell: no agent with screen rules\n"),
        };
    };
    let mut out = format!("%{pane} runs {} ({agent})", s(&v["name"]));
    if v["unread"] == true {
        let found: Vec<&str> = v["configured"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let found = if found.is_empty() { "nothing".to_owned() } else { found.join(", ") };
        let _ = writeln!(
            out,
            ": its screen isn't read here. chant audit --agents found {found} configured on this machine, not {agent} \
             (illogical describe --agents --refresh after you set it up)"
        );
        return out;
    }
    match v["fired"].as_str() {
        Some(rule) => {
            let state = v["rules"].as_array().into_iter().flatten().find(|r| r["rule"] == rule);
            let _ = write!(out, ": {} by rule {rule}", state.map(|r| s(&r["state"])).unwrap_or_default());
        }
        None => out.push_str(": no rule matches"),
    }
    let _ = writeln!(out, " (shown: {})", v["shown"].as_str().unwrap_or("nothing yet"));
    let _ = writeln!(out, "title: {}", s(&v["title"]));
    for r in v["rules"].as_array().into_iter().flatten() {
        let mark = if r["matched"] == true { "matched" } else { "no match" };
        let _ =
            writeln!(out, "\n{} ({}, {}) {}: {mark}", s(&r["rule"]), s(&r["state"]), r["priority"], s(&r["region"]));
        let text = r["text"].as_array().cloned().unwrap_or_default();
        if text.iter().all(|l| l.as_str().is_none_or(|l| l.trim().is_empty())) {
            out.push_str("  (empty)\n");
        }
        for l in text.iter().filter_map(|l| l.as_str()).filter(|l| !l.trim().is_empty()) {
            let _ = writeln!(out, "  | {}", l.trim_end());
        }
    }
    out
}

/// `describe --agents` for people: what chant found, then whose screen
/// rules run here.
fn inventory_text(v: &Value) -> String {
    use std::fmt::Write;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let list = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    match v["state"].as_str() {
        Some("read") => {
            let _ = writeln!(out, "chant {} (audit --agents) found:", s(&v["version"]));
            if v["sites"].as_array().is_none_or(|a| a.is_empty()) {
                out.push_str("  no agent configuration\n");
            }
            for site in v["sites"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    out,
                    "  {:<9} {:<7} {}  {}",
                    s(&site["runtime"]),
                    s(&site["scope"]),
                    s(&site["root"]),
                    s(&site["summary"])
                );
            }
        }
        Some("reading") => out.push_str("chant audit --agents hasn't answered yet\n"),
        Some("off") => out.push_str("not asking chant here (ILLOGICAL_CHANT is empty)\n"),
        _ => {
            let _ = writeln!(out, "no inventory: {}", v["error"].as_str().unwrap_or("chant didn't say"));
        }
    }
    let off = list(&v["rules"]["off"]);
    let _ = writeln!(
        out,
        "screen rules run for: {}",
        if off.is_empty() { "every agent with rules".into() } else { list(&v["rules"]["run"]) }
    );
    if !off.is_empty() {
        let _ = writeln!(out, "not configured here, so not read: {off}");
    }
    out
}

/// A block's `describe` once it has read what it shows (M11's views read
/// in the background; at most 30s).
fn loaded(sock: &http::Target, block: u64) -> anyhow::Result<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let v = request(sock, "GET", &format!("/api/blocks/{block}"), None)?.json()?;
        if v["state"]["loading"] != true || std::time::Instant::now() > deadline {
            return Ok(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn here(p: Option<Pane>) -> anyhow::Result<u32> {
    match p {
        Some(Pane(n)) => Ok(n),
        None => env_pane().context("which pane? (give %N, or run this inside an illogical pane)"),
    }
}

/// `90s`, `30m`, `2h`, `7d` (or plain seconds) as seconds.
fn duration(s: &str) -> anyhow::Result<u64> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().with_context(|| format!("bad duration {s}"))?;
    Ok(n * match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("bad duration {s} (use s, m, h or d)"),
    })
}

fn policy(s: &str) -> anyhow::Result<Value> {
    Ok(match s {
        "shell" => json!({"kind": "shell"}),
        "none" => json!({"kind": "none"}),
        "rerun" => json!({"kind": "rerun", "confirm": false}),
        "rerun-ask" => json!({"kind": "rerun", "confirm": true}),
        "resume" => json!({"kind": "resume"}),
        h if h.starts_with("hook:") => json!({"kind": "hook", "command": &h[5..]}),
        _ => bail!("policy: shell, none, rerun, rerun-ask, resume or hook:COMMAND"),
    })
}

fn time(ms: u64) -> String {
    let secs = ms / 1000;
    let ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(secs))
        .unwrap_or(0);
    match ago {
        0..60 => format!("{ago}s ago"),
        60..3600 => format!("{}m ago", ago / 60),
        3600..86400 => format!("{}h ago", ago / 3600),
        _ => format!("{}d ago", ago / 86400),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Seconds as the largest whole unit.
fn span(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// A hook's `message` (Claude Code's Notification: "Claude needs your
/// permission to use Bash"), when its JSON is on stdin.
fn hook_message() -> Option<String> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut input = String::new();
    std::io::stdin().take(1 << 20).read_to_string(&mut input).ok()?;
    let v: Value = serde_json::from_str(&input).ok()?;
    v["message"].as_str().map(str::to_owned).filter(|m| !m.is_empty())
}

/// `GET /api/conversations` with `claude ls`'s filters.
fn conversations_path(all: bool, live: bool, cwd: Option<String>, limit: usize, words: &[String]) -> String {
    let mut q = vec![format!("limit={limit}")];
    if all {
        q.push("all=1".into());
    }
    if live {
        q.push("live=1".into());
    }
    if let Some(d) = cwd {
        let d = std::fs::canonicalize(&d)
            .or_else(|_| std::env::current_dir().map(|h| h.join(&d)))
            .map(|p| p.display().to_string())
            .unwrap_or(d);
        q.push(format!("cwd={}", enc(&d)));
    }
    if !words.is_empty() {
        q.push(format!("q={}", enc(&words.join(" "))));
    }
    format!("/api/conversations?{}", q.join("&"))
}

/// One line of `claude ls`; `~` for `home`.
fn print_conversation(c: &Value, home: Option<&str>) {
    let s = |k: &str| c[k].as_str().unwrap_or("");
    let mut cwd = s("cwd").to_owned();
    if let Some(home) = home.filter(|h| !h.is_empty())
        && cwd.starts_with(home)
    {
        cwd = format!("~{}", &cwd[home.len()..]);
    }
    let src = match s("source") {
        "terminal" => "term",
        "desktop" => "desk",
        _ => "other",
    };
    let mut tags = vec![];
    if let Some(p) = c["live"]["place"].as_str() {
        tags.push(p.to_owned());
    }
    if let Some(b) = c["block"].as_u64() {
        tags.push(format!("block %{b}"));
    }
    let tags = if tags.is_empty() { String::new() } else { format!("  [{}]", tags.join(", ")) };
    let id: String = s("id").chars().take(8).collect();
    let title: String = s("title").chars().take(60).collect();
    let when = time(c["updated_ms"].as_u64().unwrap_or(0));
    println!("{id}  {src:<5} {when:>8}  {title}  {cwd}{tags}");
}

/// `claude ls --host all` (#78): each host's conversations under its name,
/// as it answers; one that's asleep or doesn't answer within 5 s says so.
/// `--json`: `{hosts: [{host, conversations | error | asleep}]}` once all
/// are in.
fn claude_ls_all(socket: PathBuf, path: String, json_out: bool) -> anyhow::Result<i32> {
    let mut out = vec![];
    let mut first = true;
    hosts::each(socket, &path, std::time::Duration::from_secs(5), |host, a| {
        if json_out {
            out.push(match a {
                hosts::Answer::Json(v) => json!({ "host": host, "conversations": v["conversations"] }),
                hosts::Answer::Asleep => json!({ "host": host, "asleep": true }),
                hosts::Answer::Failed(e) => json!({ "host": host, "error": e }),
                hosts::Answer::Silent => json!({ "host": host, "error": "no answer in 5 s" }),
            });
            return;
        }
        if !first {
            println!();
        }
        first = false;
        match a {
            hosts::Answer::Json(v) => {
                let list = v["conversations"].as_array().cloned().unwrap_or_default();
                println!("{host} ({})", list.len());
                // Its home, from its paths: another machine's isn't ours.
                let home = list.iter().find_map(|c| home_of(c["cwd"].as_str()?));
                for c in &list {
                    print_conversation(c, home);
                }
            }
            hosts::Answer::Asleep => println!("{host}: asleep (not woken to ask)"),
            hosts::Answer::Failed(e) => println!("{host}: {e}"),
            hosts::Answer::Silent => println!("{host}: no answer in 5 s"),
        }
    })?;
    if json_out {
        print_json(&json!({ "hosts": out }));
    }
    Ok(0)
}

/// `/home/me` or `/Users/me` at the start of a path.
fn home_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/home/").or_else(|| path.strip_prefix("/Users/"))?;
    let end = path.len() - rest.len() + rest.find('/').unwrap_or(rest.len());
    Some(&path[..end])
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// The version, findable in the binary's bytes: the testnet tests read it
/// from a box's static build they can't run here (#259).
#[used]
static VERSION_MARK: &str = concat!("\0illogical-version=", env!("CARGO_PKG_VERSION"), "\0");

fn main() {
    // Run as `tmux` (a link, or a copy on an ssh host's PATH): be tmux's
    // control mode, with tmux's own arguments.
    let argv0 = std::env::args_os().next().map(PathBuf::from);
    let name = argv0.as_deref().and_then(|p| p.file_name());
    if name.is_some_and(|n| n == "tmux" || cfg!(windows) && n.eq_ignore_ascii_case("tmux.exe")) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let target = http::Target::Socket(default_socket());
        match tmux::run(target, &args) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("tmux (illogical): {e:#}");
                std::process::exit(1);
            }
        }
    }
    let cli = Cli::parse();
    match real_main(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("illogical: {e:#}");
            std::process::exit(1);
        }
    }
}

fn real_main(cli: Cli) -> anyhow::Result<i32> {
    if let Command::Join { url, name, team, account } = &cli.cmd {
        let control = url.clone().unwrap_or_else(|| ssh::CONTROL.to_owned());
        let mut args = vec!["join".to_owned(), control.clone()];
        for (flag, v) in [("--name", name), ("--team", team), ("--account", account)] {
            if let Some(v) = v {
                args.extend([flag.to_owned(), v.clone()]);
            }
        }
        if let Some(dest) = &cli.ssh {
            return ssh::Remote::parse(dest)?.join(&args, &control);
        }
        let beside = std::env::current_exe()?.with_file_name(format!("illogicald{}", std::env::consts::EXE_SUFFIX));
        let daemon = if beside.exists() { beside } else { PathBuf::from("illogicald") };
        let mut cmd = std::process::Command::new(&daemon);
        cmd.args(&args);
        #[cfg(unix)]
        let err = std::os::unix::process::CommandExt::exec(&mut cmd);
        // No exec on Windows: run it and pass on its exit code.
        #[cfg(not(unix))]
        let err = match cmd.status() {
            Ok(s) => return Ok(s.code().unwrap_or(1)),
            Err(e) => e,
        };
        bail!("running {}: {err}", daemon.display());
    }
    if let Command::Login { url, name, account } = &cli.cmd {
        let url = match url {
            Some(u) => u.clone(),
            None => http::request(&http::Target::Socket(socket(&cli)), "GET", "/api/host", None)
                .and_then(|r| r.json())
                .ok()
                .and_then(|v| v["control"].as_str().map(String::from))
                .unwrap_or_else(|| ssh::CONTROL.to_owned()),
        };
        let name = name.clone().unwrap_or_else(|| {
            let h = fountain_runner::hostname().unwrap_or_default();
            if h.is_empty() { "illogical CLI".into() } else { format!("illogical CLI on {h}") }
        });
        control::login(&url, &name, account.as_deref())?;
        return Ok(0);
    }
    if let Command::Logout = cli.cmd {
        control::logout()?;
        return Ok(0);
    }
    if let Command::Bridge { probe } = cli.cmd {
        // On a box, for a client that ssh'd in: this daemon's socket on
        // stdin and stdout.
        return ssh::bridge(&socket(&cli), probe);
    }
    if let Command::Install { args } = &cli.cmd {
        // The daemon beside this binary, else the one on PATH.
        let beside = std::env::current_exe()?.with_file_name(format!("illogicald{}", std::env::consts::EXE_SUFFIX));
        let daemon = if beside.exists() { beside } else { PathBuf::from("illogicald") };
        let mut cmd = std::process::Command::new(&daemon);
        cmd.arg("install").args(args);
        #[cfg(unix)]
        let err = std::os::unix::process::CommandExt::exec(&mut cmd);
        // No exec on Windows: run it and pass on its exit code.
        #[cfg(not(unix))]
        let err = match cmd.status() {
            Ok(s) => return Ok(s.code().unwrap_or(1)),
            Err(e) => e,
        };
        bail!("running {}: {err}", daemon.display());
    }
    if let Command::Ask = cli.cmd {
        // A hook: the local daemon only, and never an error.
        return Ok(ask::run(http::Target::Socket(socket(&cli))));
    }
    if let Command::Hook = cli.cmd {
        return Ok(hook::run(http::Target::Socket(socket(&cli))));
    }
    if let Command::Web { print } = cli.cmd
        && cli.ssh.is_none()
    {
        // The local daemon's own link, over its socket (only ours); with
        // --ssh, the box's, below.
        return web(&http::Target::Socket(socket(&cli)), print);
    }
    if let Command::Inbox = cli.cmd {
        return Ok(hook::inbox(http::Target::Socket(socket(&cli))));
    }
    if let Command::Hooks { cmd } = cli.cmd {
        // A settings file: no daemon involved.
        return hooks::run(cmd, cli.json);
    }
    if let Command::Fountain { cmd: Some(FountainCmd::Runner { cmd }), .. } = &cli.cmd {
        // Fountain's API and this host's unit: no daemon involved.
        return fountain_runner::run(cmd, cli.json);
    }
    // `claude ls --host all` (#78): every host's, as each answers.
    if cli.host.as_deref() == Some("all")
        && let Command::Claude { cmd: ClaudeCmd::Ls { all, live, cwd, limit, words } } = &cli.cmd
    {
        return claude_ls_all(socket(&cli), conversations_path(*all, *live, cwd.clone(), *limit, words), cli.json);
    }
    let reads_history = matches!(cli.cmd, Command::History { .. } | Command::Search { .. } | Command::Tail { .. });
    let resolved = match &cli.ssh {
        Some(dest) => ssh::Remote::parse(dest).map(http::Target::Ssh),
        None => hosts::target(socket(&cli), cli.host.as_deref()),
    };
    let (sock, gone) = match resolved {
        Ok(t) => (t, None),
        // A host that's gone (deleted, unreachable) may have left its
        // history here.
        Err(e) if reads_history && cli.host.is_some() => {
            eprintln!("illogical: {e:#}; reading what it synced here instead");
            (http::Target::Socket(socket(&cli)), cli.host.clone())
        }
        // `open --host m2` meant a machine: the flag is `--machine` (#61).
        Err(e)
            if cli.host.as_deref().is_some_and(looks_like_machine)
                && matches!(cli.cmd, Command::Open { .. } | Command::Edit { .. } | Command::Agent { .. }) =>
        {
            let m = cli.host.as_deref().unwrap_or_default();
            return Err(e.context(format!("--host is another daemon; for machine {m}, use --machine {m}")));
        }
        Err(e) => return Err(e),
    };
    REMOTE.store(!matches!(sock, http::Target::Socket(_)), std::sync::atomic::Ordering::Relaxed);
    // Over ssh: the master, illogical installed there, its daemon up.
    if let http::Target::Ssh(r) = &sock {
        r.prepare()?;
    }
    let json_out = cli.json;
    // `run --home`: the local daemon too, and the host's name in its list.
    let (local_sock, host_name) = (socket(&cli), cli.host.clone());
    // `host=` for reading synced history.
    let synced_q = |flag: Option<String>| -> Option<String> {
        flag.or(gone.clone()).map(|h| format!("host={}", enc(if h == "all" { "*" } else { &h })))
    };
    match cli.cmd {
        Command::Share { pane, ttl, guest: true, rw, reusable, name, addr, relay } => {
            let body = json!({
                "pane": here(pane)?, "ttl_secs": duration(&ttl)?, "rw": rw, "reusable": reusable,
                "label": name, "host": addr, "relay": relay.then_some(true),
            });
            let v = request(&sock, "POST", "/api/guests", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("{}", v["command"].as_str().unwrap_or_default());
                let left = v["expires_ms"].as_u64().unwrap_or(0).saturating_sub(now_ms()) / 1000;
                if let Some(jump) = v["jump"].as_str() {
                    eprintln!(
                        "Through control's ssh jump host {jump} (ssh -J, written out so its key is pinned \
                         too): control carries the session and can't read it."
                    );
                }
                eprintln!(
                    "Invite {}: {}, {}, for {}. `illogical guests revoke {}` ends it.\n\
                     The host key is pinned in the command ({}). ssh older than 8.5 has no \
                     KnownHostsCommand: save this to a file and pass -o UserKnownHostsFile=<file>:\n{}",
                    v["id"],
                    if rw { "read-write" } else { "read-only" },
                    if reusable { "reusable" } else { "one login" },
                    span(left),
                    v["id"],
                    v["fingerprint"].as_str().unwrap_or_default(),
                    v["known_hosts"].as_str().unwrap_or_default(),
                );
            }
        }
        Command::Guests { cmd: None } => {
            let v = request(&sock, "GET", "/api/guests", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let list = v.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("no ssh invites");
            }
            for g in list {
                let left = g["expires_ms"].as_u64().unwrap_or(0).saturating_sub(now_ms()) / 1000;
                let kind = match (g["rw"].as_bool(), g["reusable"].as_bool()) {
                    (Some(true), Some(true)) => "rw, reusable",
                    (Some(true), _) => "rw",
                    (_, Some(true)) => "ro, reusable",
                    _ => "ro",
                };
                println!(
                    "{:<4} %{:<4} {:<12} {:<14} {} connected{}, expires in {}",
                    g["id"],
                    g["pane"],
                    g["label"].as_str().unwrap_or(""),
                    kind,
                    g["sessions"],
                    if g["used"] == true && g["reusable"] != true { ", spent" } else { "" },
                    span(left)
                );
            }
        }
        Command::Guests { cmd: Some(SharesCmd::Revoke { id }) } => {
            request(&sock, "DELETE", &format!("/api/guests/{id}"), None)?.json()?;
        }
        Command::Share { pane, ttl, .. } => {
            let body = json!({"pane": here(pane)?, "ttl_secs": duration(&ttl)?});
            let v = request(&sock, "POST", "/api/shares", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("{}", v["url"].as_str().or(v["path"].as_str()).unwrap_or_default());
            }
        }
        Command::Access { cmd } => {
            let principal = |who: &str| if who.contains(':') { who.to_owned() } else { format!("tailnet:{who}") };
            // A session by name or id ($N), from the panes list.
            let session_id = |want: &str| -> anyhow::Result<u64> {
                let v = request(&sock, "GET", "/api/panes", None)?.json()?;
                let id = want.strip_prefix('$').and_then(|n| n.parse().ok());
                v.as_array()
                    .into_iter()
                    .flatten()
                    .find(|p| {
                        Some(p["session"].as_u64().unwrap_or(0)) == id || p["session_name"].as_str() == Some(want)
                    })
                    .and_then(|p| p["session"].as_u64())
                    .ok_or_else(|| anyhow::anyhow!("no session {want}"))
            };
            let log = matches!(cmd, Some(AccessCmd::Log));
            let v = match cmd {
                None | Some(AccessCmd::Log) => request(&sock, "GET", "/api/acl", None)?.json()?,
                Some(AccessCmd::Grant { session, who, role }) => {
                    if !matches!(role.as_str(), "viewer" | "editor" | "owner") {
                        anyhow::bail!("a role is viewer, editor or owner");
                    }
                    let body = serde_json::json!({ "session": session_id(&session)?, "principal": principal(&who), "role": role });
                    request(&sock, "POST", "/api/acl", Some(&body))?.json()?
                }
                Some(AccessCmd::Revoke { session, who }) => {
                    let body = serde_json::json!({ "session": session_id(&session)?, "principal": principal(&who), "role": null });
                    request(&sock, "POST", "/api/acl", Some(&body))?.json()?
                }
            };
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            if log {
                for e in v["audit"].as_array().into_iter().flatten() {
                    println!(
                        "{}  {:<6} ${:<3} {:<30} {}",
                        time(e["at"].as_u64().unwrap_or(0)),
                        e["action"].as_str().unwrap_or(""),
                        e["session"],
                        e["principal"].as_str().unwrap_or(""),
                        e["role"].as_str().unwrap_or("")
                    );
                }
            } else {
                let grants = v["grants"].as_array().cloned().unwrap_or_default();
                if grants.is_empty() {
                    println!("nothing is shared");
                }
                for g in grants {
                    println!(
                        "${:<4} {:<7} {}",
                        g["session"],
                        g["role"].as_str().unwrap_or(""),
                        g["principal"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Command::Shares { cmd: None } => {
            let v = request(&sock, "GET", "/api/shares", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for s in v.as_array().into_iter().flatten() {
                let left = s["expires_ms"].as_u64().unwrap_or(0).saturating_sub(now_ms()) / 1000;
                println!(
                    "{:<4} %{:<4} made {:>8}, expires in {}",
                    s["id"],
                    s["pane"],
                    time(s["created_ms"].as_u64().unwrap_or(0)),
                    span(left)
                );
            }
        }
        Command::Mcp { token, cmd: None } => return mcp::run(sock, token),
        Command::Mcp { cmd: Some(McpCmd::Token { list: true, .. }), .. } => {
            let v = request(&sock, "GET", "/api/mcp/tokens", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let list = v.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("no MCP tokens");
            }
            for t in list {
                let used = t["used_ms"].as_u64().map(time).unwrap_or_else(|| "never".into());
                println!(
                    "{:<20} {:<5} made {:>8}, last used {used}",
                    t["name"].as_str().unwrap_or(""),
                    t["scope"].as_str().unwrap_or(""),
                    time(t["created_ms"].as_u64().unwrap_or(0))
                );
            }
        }
        Command::Mcp { cmd: Some(McpCmd::Token { revoke: Some(name), .. }), .. } => {
            request(&sock, "DELETE", &format!("/api/mcp/tokens/{}", enc(&name)), None)?.json()?;
            eprintln!("revoked {name}: a client using it is cut off at its next call");
        }
        Command::Mcp { cmd: Some(McpCmd::Token { name, scope, .. }), .. } => {
            if !matches!(scope.as_str(), "full" | "read") {
                bail!("--scope: full or read");
            }
            let v =
                request(&sock, "POST", "/api/mcp/tokens", Some(&json!({ "name": name, "scope": scope })))?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("{}", v["token"].as_str().unwrap_or_default());
            eprintln!(
                "That's shown once. A client sends it to /mcp as `Authorization: Bearer <token>`; \
                 `illogical mcp token --revoke {name}` cuts it off."
            );
        }
        Command::Shares { cmd: Some(SharesCmd::Revoke { id }) } => {
            request(&sock, "DELETE", &format!("/api/shares/{id}"), None)?.json()?;
        }
        Command::Synced { cmd } => {
            let v = match cmd {
                None => request(&sock, "GET", "/api/synced", None)?.json()?,
                Some(SyncedCmd::Rm { name }) => {
                    request(&sock, "DELETE", &format!("/api/synced/{}", enc(&name)), None)?.json()?
                }
                Some(SyncedCmd::RotateKey) => request(&sock, "POST", "/api/synced/rotate-key", None)?.json()?,
            };
            if json_out || !v.is_array() {
                print_json(&v);
                return Ok(0);
            }
            for h in v.as_array().into_iter().flatten() {
                let panes = h["panes"].as_object().cloned().unwrap_or_default();
                let bytes: u64 = panes.values().filter_map(|p| p["bytes"].as_u64()).sum();
                let last = panes.values().filter_map(|p| p["last_push_ms"].as_u64()).max().unwrap_or(0);
                println!(
                    "{:<20} {} panes, {} KB, last pushed {}",
                    h["name"].as_str().unwrap_or("?"),
                    panes.len(),
                    bytes / 1024,
                    time(last)
                );
            }
        }
        Command::Hosts { cmd } => hosts::run(&sock, cmd, json_out, duration)?,
        Command::Sandboxes { cmd } => hosts::sandboxes(&sock, cmd, json_out)?,
        // Only with --ssh: the box daemon's link, over the bridge to its
        // socket.
        Command::Web { print } if matches!(sock, http::Target::Ssh(_)) => return web(&sock, print),
        Command::Install { .. }
        | Command::Web { .. }
        | Command::Bridge { .. }
        | Command::Join { .. }
        | Command::Login { .. }
        | Command::Logout => {
            unreachable!("handled before connecting")
        }
        Command::Tmux { args } => return tmux::run(sock, &args),
        Command::Ls => {
            let v = request(&sock, "GET", "/api/panes", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for p in v.as_array().into_iter().flatten() {
                let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
                let tab = p
                    .get("tab_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("@{}", p["tab"]));
                let what = if !p["running"].as_bool().unwrap_or(false) {
                    "(waiting)".to_owned()
                } else {
                    p["current"]["text"].as_str().or(p["command"].as_str()).unwrap_or("").to_owned()
                };
                let attention = match s("attention") {
                    "idle" | "" => String::new(),
                    a => format!("  [{a}]"),
                };
                let host = p["host"].as_u64().map(|m| format!("  (vm m{m})")).unwrap_or_default();
                println!(
                    "%{:<4} {:<12} {:<14} {:<36} {what}{attention}{host}",
                    p["id"],
                    s("session_name"),
                    tab,
                    s("cwd")
                );
            }
        }
        Command::Describe { agents: true, refresh, .. } => {
            let v = match refresh {
                true => request(&sock, "POST", "/api/hosts/self/agents/refresh", None)?.json()?,
                false => request(&sock, "GET", "/api/hosts/self/agents", None)?.json()?,
            };
            if json_out {
                print_json(&v);
            } else {
                print!("{}", inventory_text(&v));
            }
        }
        Command::Describe { block: Some(block), detection: true, .. } => {
            let v = request(&sock, "GET", &format!("/api/panes/{}/detection", block.0), None)?.json()?;
            print!("{}", detection_text(block.0, &v));
        }
        Command::Describe { block: Some(block), .. } => {
            print_json(&request(&sock, "GET", &format!("/api/blocks/{}", block.0), None)?.json()?);
        }
        Command::Describe { block: None, .. } => unreachable!("clap requires a block"),
        Command::Call { block, method, args } => {
            let args: Value = match args {
                Some(a) => serde_json::from_str(&a).context("args must be JSON")?,
                None => json!({}),
            };
            let path = format!("/api/blocks/{}/call/{}", block.0, enc(&method));
            print_json(&request(&sock, "POST", &path, Some(&args))?.json()?);
        }
        Command::Open { target, split, machine, session } => {
            let config = match target.strip_prefix(':') {
                Some(rest) => {
                    let (port, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
                    let port: u16 = port.parse().with_context(|| format!("not a port: {target}"))?;
                    json!({ "port": port, "path": if path.is_empty() { "/" } else { path } })
                }
                None => json!({ "url": target }),
            };
            let split = match split.as_deref() {
                None => None,
                Some("right") => Some(here(None)?),
                Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
            };
            let local = machine.as_deref() == Some("local");
            let host = match machine.filter(|_| !local) {
                Some(m) => {
                    Some(m.trim_start_matches('m').parse::<u32>().with_context(|| format!("not a machine: {m}"))?)
                }
                None => None,
            };
            let body = json!({
                "type": "browser",
                "config": config,
                "split": split,
                "host": host,
                "local": local,
                "session": session,
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"]);
            }
        }
        Command::Edit { path, line, split, machine, session } => {
            let split = match split.as_deref() {
                None => None,
                Some("right") => Some(here(None)?),
                Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
            };
            let local = machine.as_deref() == Some("local");
            let machine = match machine.filter(|_| !local) {
                Some(m) => {
                    Some(m.trim_start_matches('m').parse::<u32>().with_context(|| format!("not a machine: {m}"))?)
                }
                None => None,
            };
            // On this host (not another daemon's, not a VM's), a path is
            // this directory's.
            let mine = machine.is_none() && !REMOTE.load(std::sync::atomic::Ordering::Relaxed);
            let (path, at) = match path {
                Some(p) => {
                    let (p, at) = file_line(&p, mine);
                    (Some(p), at)
                }
                None => (None, None),
            };
            let path = match path {
                Some(p) if mine => Some(absolute(&p)?),
                Some(p) => Some(p),
                None if mine => Some(std::env::current_dir()?.display().to_string()),
                None => None,
            };
            let body = json!({
                "type": "editor",
                "config": { "path": path, "line": line.or(at) },
                "split": split,
                "host": machine,
                "local": local,
                "session": session,
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"]);
            }
        }
        Command::Diff { args, repo, split, session } => {
            let (from, revs) = match args.first().and_then(|a| a.strip_prefix('%')) {
                Some(n) => (Some(n.parse::<u32>().with_context(|| format!("not a pane: %{n}"))?), &args[1..]),
                None => (None, &args[..]),
            };
            if revs.len() > 2 {
                bail!("at most two revisions: diff [%N] [REV_A [REV_B]]");
            }
            let remote = REMOTE.load(std::sync::atomic::Ordering::Relaxed);
            let repo = match repo {
                Some(r) if !remote && from.is_none() => Some(absolute(&r)?),
                Some(r) => Some(r),
                None if from.is_none() && !remote => Some(std::env::current_dir()?.display().to_string()),
                None => None,
            };
            let body = json!({
                "type": "diff",
                "config": { "repo": repo, "rev_a": revs.first(), "rev_b": revs.get(1) },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": from.or_else(env_pane),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            let st = &v["state"];
            if let Some(e) = st["error"].as_str() {
                bail!("{e}");
            }
            for f in st["files"].as_array().into_iter().flatten() {
                let counts = match (f["binary"].as_bool(), f["big"].as_bool()) {
                    (Some(true), _) => "binary".to_owned(),
                    (_, Some(true)) => "too big".to_owned(),
                    _ => format!("+{} -{}", f["add"], f["del"]),
                };
                let path = match f["old"].as_str() {
                    Some(old) => format!("{old} -> {}", f["path"].as_str().unwrap_or("")),
                    None => f["path"].as_str().unwrap_or("").to_owned(),
                };
                println!("{:<10} {path}  {counts}", f["status"].as_str().unwrap_or(""));
            }
            let n = st["files"].as_array().map_or(0, Vec::len);
            println!(
                "{n} file{} changed, +{} -{} ({})",
                if n == 1 { "" } else { "s" },
                st["add"],
                st["del"],
                st["against"].as_str().unwrap_or("")
            );
        }
        Command::View { spec, line, split, session } => {
            let remote = REMOTE.load(std::sync::atomic::Ordering::Relaxed);
            let (on, path) = match spec.split_once(':') {
                Some((on, p)) if on.starts_with('%') || (on.starts_with('m') && on[1..].parse::<u32>().is_ok()) => {
                    (Some(on.to_owned()), p.to_owned())
                }
                _ => (None, spec.clone()),
            };
            let (path, at) = file_line(&path, on.is_none() && !remote);
            let (from, host) = match on.as_deref() {
                Some(p) if p.starts_with('%') => {
                    (Some(p[1..].parse::<u32>().with_context(|| format!("not a pane: {p}"))?), None)
                }
                Some(m) => (None, Some(m[1..].parse::<u32>()?)),
                None => (env_pane(), None),
            };
            let path = if on.is_none() && !remote { absolute(&path)? } else { path };
            let body = json!({
                "type": "file",
                "config": { "path": path, "line": line.or(at) },
                "split": split_of(split.as_deref())?,
                "host": host,
                "local": on.is_none() && !remote,
                "session": session,
                "from_pane": from,
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"]);
            }
        }
        Command::Workspace { dir, env, split, session } => {
            let root = absolute(dir.as_deref().unwrap_or("."))?;
            let body = json!({
                "type": "workspace",
                "config": { "root": root, "env": env },
                "split": split_of(split.as_deref())?,
                "local": true,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            // Its first read: four chant processes, a second or two.
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            let st = &v["state"];
            if let Some(e) = st["error"].as_str() {
                bail!("{e}");
            }
            let members = st["members"].as_array().map_or(0, Vec::len);
            let gates = st["gates"].as_array().cloned().unwrap_or_default();
            println!(
                "{}: {members} members, {} records, {} waiting at a gate",
                st["name"].as_str().unwrap_or("workspace"),
                st["records"].as_array().map_or(0, Vec::len),
                gates.len()
            );
            for g in gates {
                let s = |k: &str| g[k].as_str().unwrap_or("").to_owned();
                println!("  {}: {} waits at gate {}", s("member"), s("op"), s("gate"));
            }
        }
        Command::Pr { cmd: Some(cmd), .. } => {
            let (block, method, mut args) = match cmd {
                PrCmd::Comment { block, body } => (block, "comment", json!({ "body": body })),
                PrCmd::Review { block, event, body } => (block, "review", json!({ "event": event, "body": body })),
                PrCmd::Merge { block, style } => (block, "merge", json!({ "style": style })),
                PrCmd::Rerun { block } => (block, "rerun_checks", json!({})),
            };
            if http::agent() {
                args["agent"] = json!(true);
            }
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/{method}", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else if let Some(d) = v["draft"].as_str() {
                println!("draft {d}: waits for a person to send it on %{}", block.0);
            } else {
                println!("{}", v["url"].as_str().or(v["said"].as_str()).unwrap_or("sent"));
            }
        }
        Command::Pr { cmd: None, target, split, session } => {
            let target = target.context("which pull request? a URL, OWNER/REPO#N, or N in this repository")?;
            let body = json!({
                "type": "forge",
                "config": { "pr": target, "dir": absolute(".")? },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            if let Some(e) = v["state"]["error"].as_str() {
                bail!("{e}");
            }
            print!("{}", request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.text()?);
        }
        Command::Fountain { cmd: Some(FountainCmd::Agents { query, source, profile }), .. } => {
            let mut q = vec![];
            for (k, v) in [("query", query), ("source", source), ("profile", profile)] {
                if let Some(v) = v {
                    q.push(format!("{k}={}", enc(&v)));
                }
            }
            let path = if q.is_empty() {
                "/api/fountain/agents".into()
            } else {
                format!("/api/fountain/agents?{}", q.join("&"))
            };
            let v = request(&sock, "GET", &path, None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let rows = v["agents"].as_array().cloned().unwrap_or_default();
            println!("{} of {} agents on {}", rows.len(), v["total"], v["base_url"].as_str().unwrap_or("Fountain"));
            if let Some(n) = v["unreadable"].as_u64().filter(|n| *n > 0) {
                println!("({n} couldn't be read: Fountain sent something this illogical doesn't understand)");
            }
            for r in rows {
                let s = |k: &str| r[k].as_str().unwrap_or("").to_owned();
                let list = |k: &str| {
                    r[k].as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()
                };
                let mut line = format!("{}  [{}] {}", s("name"), s("runtime"), s("source"));
                if !list("skills").is_empty() {
                    line.push_str(&format!("  skills: {}", list("skills")));
                }
                if !list("mcp").is_empty() {
                    line.push_str(&format!("  mcp: {}", list("mcp")));
                }
                println!("{line}");
            }
        }
        Command::Fountain { cmd: None, view, query, source, profile, split, session } => {
            if view == "runner" && (query.is_some() || source.is_some()) {
                bail!("--query and --source filter the catalog, not the runner view");
            }
            let mut filter = json!({});
            if let Some(q) = query {
                filter["query"] = json!(q);
            }
            if let Some(s) = source {
                filter["sources"] = json!(s.split(',').map(str::trim).collect::<Vec<_>>());
            }
            let body = json!({
                "type": "fountain",
                "config": { "profile": profile, "view": view, "filter": filter },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            if let Some(e) = v["state"]["error"].as_str() {
                bail!("{e}");
            }
            print!("{}", request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.text()?);
        }
        Command::Issue { cmd: Some(IssueCmd::Comment { block, body }), .. } => {
            let mut args = json!({ "body": body });
            if http::agent() {
                args["agent"] = json!(true);
            }
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/comment", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else if let Some(d) = v["draft"].as_str() {
                println!("draft {d}: waits for a person to send it on %{}", block.0);
            } else {
                println!("{}", v["url"].as_str().or(v["said"].as_str()).unwrap_or("sent"));
            }
        }
        Command::Issue { cmd: Some(IssueCmd::Agent { block, agent, prompt, dir }), .. } => {
            let dir = dir.map(|d| absolute(&d)).transpose()?;
            let args = json!({ "agent": agent, "prompt_extra": prompt, "dir": dir });
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/agent", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!(
                    "%{} on branch {} in {}",
                    v["agent"],
                    v["branch"].as_str().unwrap_or("?"),
                    v["worktree"].as_str().unwrap_or("?")
                );
            }
        }
        Command::Issue { cmd: Some(IssueCmd::New { title, body, repo, split, session }), .. } => {
            let body = json!({
                "type": "forge",
                "config": { "issue": "new", "title": title, "body": body.unwrap_or_default(), "repo": repo, "dir": absolute(".")? },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            // A person's goes out now; an agent's waits as a draft.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let v = loop {
                let v = request(&sock, "GET", &format!("/api/blocks/{block}"), None)?.json()?;
                let st = &v["state"];
                let settled = st["number"].as_u64().is_some_and(|n| n > 0)
                    || st["new"]["agent"] == true
                    || !st["error"].is_null()
                    || !st["new"]["error"].is_null();
                if settled || std::time::Instant::now() > deadline {
                    break v;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let st = &v["state"];
            if let Some(e) = st["error"].as_str().or(st["new"]["error"].as_str()) {
                bail!("{e}");
            }
            match st["number"].as_u64().filter(|n| *n > 0) {
                Some(n) => println!(
                    "%{block} {}#{n} {}",
                    st["repo"].as_str().unwrap_or(""),
                    st["new"]["url"].as_str().unwrap_or("")
                ),
                None => println!("%{block}: a draft that waits for a person to send it"),
            }
        }
        Command::Issue { cmd: None, target, split, session } => {
            let target = target.context("which issue? a URL, OWNER/REPO#N, or N in this repository")?;
            let body = json!({
                "type": "forge",
                "config": { "issue": target, "dir": absolute(".")? },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            if let Some(e) = v["state"]["error"].as_str() {
                bail!("{e}");
            }
            print!("{}", request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.text()?);
        }
        Command::Rerun { pane } => {
            let pane = here(pane)?;
            let v = request(&sock, "POST", "/api/attention/act", Some(&json!({ "action": "rerun", "pane": pane })))?;
            let v = v.json()?;
            if let Some(e) = v["results"][0]["error"].as_str() {
                bail!("{e}");
            }
        }
        Command::Agent { resume, fork, session, split, wait, prompt, .. } if resume.is_some() || fork.is_some() => {
            let (id, then) = match (resume, fork) {
                (Some(id), _) => (id, "continue"),
                (_, Some(id)) => (id, "fork"),
                _ => unreachable!(),
            };
            let prompt = prompt.join(" ");
            // With a prompt, sending it is what continues it.
            let then = if prompt.is_empty() || then == "fork" { Some(then) } else { None };
            let body = json!({
                "id": id,
                "then": then,
                "session": session,
                "split": split.map(|p| p.0),
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/conversations/open", Some(&body))?.json()?;
            let block = v["block"].as_u64().context("no block in the answer")?;
            if let Some(e) = v["error"].as_str() {
                eprintln!("%{block}: {e}");
                return Ok(1);
            }
            if !prompt.is_empty() {
                let r = request(
                    &sock,
                    "POST",
                    &format!("/api/blocks/{block}/call/send"),
                    Some(&json!({ "text": prompt })),
                )?;
                if let Err(e) = r.json() {
                    eprintln!("%{block}: {e}");
                    return Ok(1);
                }
            }
            if json_out {
                print_json(&v);
            } else {
                println!("%{block}");
            }
            if wait && !prompt.is_empty() {
                let w = request(&sock, "GET", &format!("/api/panes/{block}/wait?until=idle"), None)?.json()?;
                let text = request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.ok()?.text()?;
                print!("{text}");
                return Ok(if w["state"] == "needs_input" { 2 } else { 0 });
            }
        }
        Command::Claude { cmd: ClaudeCmd::Ls { all, live, cwd, limit, words } } => {
            let path = conversations_path(all, live, cwd, limit, &words);
            let v = request(&sock, "GET", &path, None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let home = std::env::var("HOME").ok();
            for c in v["conversations"].as_array().into_iter().flatten() {
                print_conversation(c, home.as_deref());
            }
        }
        Command::Studio { cmd } => {
            let v = match cmd {
                None => request(&sock, "GET", "/api/studio", None)?.json()?,
                Some(StudioCmd::Login { url }) => {
                    let token = secret_input("Studio token: ")?;
                    request(&sock, "POST", "/api/studio", Some(&json!({ "url": url, "token": token })))?.json()?
                }
                Some(StudioCmd::Logout) => request(&sock, "DELETE", "/api/studio", None)?.json()?,
                Some(StudioCmd::Follower { app, forget: true }) => {
                    request(&sock, "DELETE", &format!("/api/studio/followers/{}", enc(&app)), None)?.json()?
                }
                Some(StudioCmd::Follower { app, forget: false }) => {
                    let link = secret_input("Follower link: ")?;
                    let path = format!("/api/studio/followers/{}", enc(&app));
                    request(&sock, "PUT", &path, Some(&json!({ "link": link })))?.json()?
                }
            };
            if json_out {
                print_json(&v);
            } else if let Some(apps) = v["apps"].as_array() {
                println!("logged in; {} app{}", apps.len(), if apps.len() == 1 { "" } else { "s" });
            } else if let Some(url) = v["url"].as_str() {
                let state = if v["logged_in"] == true { "logged in" } else { "logged out" };
                println!("{url}: {state}");
                for f in v["followers"].as_array().into_iter().flatten() {
                    println!("  follower link for {}", f.as_str().unwrap_or("?"));
                }
            } else if v.get("logged_in").is_some() {
                println!("no studio: `illogical studio login <url>`");
            }
        }
        Command::App { name: None, .. } => {
            let v = request(&sock, "GET", "/api/studio/apps", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for a in v["apps"].as_array().into_iter().flatten() {
                let s = |k: &str| a[k].as_str().unwrap_or("");
                let blocks: Vec<String> = a["blocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.as_u64())
                    .map(|b| format!("%{b}"))
                    .collect();
                let open = if blocks.is_empty() { String::new() } else { format!("  [{}]", blocks.join(", ")) };
                let status = if s("status").is_empty() { String::new() } else { format!(" ({})", s("status")) };
                println!("{:<24} {}{status}{open}", s("name"), s("url"));
            }
        }
        Command::App { name: Some(name), split, session, follower } => {
            let split = match split.as_deref() {
                None => None,
                Some("right") => Some(here(None)?),
                Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
            };
            let body = json!({
                "type": "app",
                "config": if follower { serde_json::json!({ "app": name, "follower": true }) } else { serde_json::json!({ "app": name }) },
                "split": split,
                "session": session,
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"]);
            }
        }
        Command::Claude { cmd: ClaudeCmd::Open { id, session, split } } => {
            let body = json!({ "id": id, "session": session, "split": split.map(|p| p.0), "from_pane": env_pane() });
            let v = request(&sock, "POST", "/api/conversations/open", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"].as_u64().context("no block in the answer")?);
            }
        }
        Command::Agent {
            acp,
            fountain,
            as_fountain,
            codex,
            vault,
            model,
            mcp,
            allow,
            permission_mode,
            user_settings,
            vm,
            machine,
            cwd,
            session,
            split,
            wait,
            prompt,
            ..
        } => {
            let mut config = match (&acp, &fountain) {
                (Some(cmd), _) => json!({ "agent": "acp", "command": cmd }),
                (_, Some(name)) => json!({ "agent": "fountain", "fountain_agent": name, "vault": vault }),
                _ if codex => json!({ "agent": "codex" }),
                _ => match &as_fountain {
                    Some(name) => json!({ "agent": "claude", "as_fountain": name, "vault": vault }),
                    None => json!({ "agent": "claude" }),
                },
            };
            if vault.is_some() && fountain.is_none() && as_fountain.is_none() {
                anyhow::bail!("--vault goes with --fountain or --as");
            }
            // A VM, or another daemon's machine, has none of this host's
            // directories.
            let cwd = if vm || machine.is_some() || REMOTE.load(std::sync::atomic::Ordering::Relaxed) {
                cwd
            } else {
                cwd.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
            };
            config["cwd"] = json!(cwd);
            config["model"] = json!(model);
            if !mcp.is_empty() {
                config["mcp_servers"] = json!(mcp);
            }
            // #163: what it may do without a card.
            if !allow.is_empty() {
                config["allow"] = allow.iter().map(|t| json!({ "tool": t })).collect();
            }
            config["permission_mode"] = json!(permission_mode);
            if user_settings {
                config["user_settings"] = json!(true);
            }
            let prompt = prompt.join(" ");
            if !prompt.is_empty() {
                config["prompt"] = json!(prompt);
            }
            let host =
                machine.map(|h| h.trim_start_matches('m').parse::<u32>()).transpose().context("--machine: m<N>")?;
            let body = json!({
                "type": "agent",
                "config": config,
                "vm": vm,
                "host": host,
                "split": split.map(|p| p.0),
                "session": session,
                "from_pane": std::env::var("ILLOGICAL_PANE").ok().and_then(|v| v.parse::<u32>().ok()),
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            let block = v["block"].as_u64().context("no block in the answer")?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{block}");
            }
            if wait && !prompt.is_empty() {
                let w = request(&sock, "GET", &format!("/api/panes/{block}/wait?until=idle"), None)?.json()?;
                let text = request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.ok()?.text()?;
                print!("{text}");
                return Ok(if w["state"] == "needs_input" { 2 } else { 0 });
            }
        }
        Command::Editors { cmd: None } => {
            let v = request(&sock, "GET", "/api/editors", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for e in v.as_array().into_iter().flatten() {
                let s = |k: &str| e[k].as_str().unwrap_or("");
                let app = e["editor"]["app"].as_str().unwrap_or("?");
                let remote = e["editor"]["remote"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
                let n = e["editor"]["followers"].as_u64().unwrap_or(0);
                let following = if n > 0 { format!("  {n} following") } else { String::new() };
                let why = e["reason"]["headline"].as_str().map(|h| format!("  [{h}]")).unwrap_or_default();
                println!(
                    "%{:<4} {:<12} {:<40} {}{following}{why}",
                    e["pane"],
                    format!("{app}{remote}"),
                    s("folder"),
                    s("file")
                );
            }
        }
        Command::Editors { cmd: Some(EditorsCmd::Vsix { out }) } => {
            let (name, bytes) = vsix(&sock)?;
            let out = out.unwrap_or_else(|| PathBuf::from(name));
            std::fs::write(&out, bytes).with_context(|| format!("writing {}", out.display()))?;
            println!("{}", out.display());
        }
        Command::Editors { cmd: Some(EditorsCmd::Install { with }) } => {
            let (name, bytes) = vsix(&sock)?;
            let path = std::env::temp_dir().join(name);
            std::fs::write(&path, bytes)?;
            let which = |c: &str| {
                std::process::Command::new(c)
                    .arg("--version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            };
            let editor = match with {
                Some(w) => w,
                None => ["code", "cursor", "code-server"]
                    .into_iter()
                    .find(|c| which(c))
                    .context("no `code` or `cursor` here: pass --with, or install the VSIX (`illogical editors vsix`) by hand")?
                    .to_owned(),
            };
            let st = std::process::Command::new(&editor).arg("--install-extension").arg(&path).status()?;
            let _ = std::fs::remove_file(&path);
            if !st.success() {
                bail!("{editor} --install-extension failed");
            }
            println!("Installed. In the editor: \"illogical: Show this workspace in the swarm\".");
        }
        Command::Rules { forget, forget_all } => {
            if forget_all {
                request(&sock, "DELETE", "/api/rules", None)?.json()?;
            } else if let Some(i) = forget {
                request(&sock, "DELETE", &format!("/api/rules/{i}"), None)?.json()?;
            }
            let v: Value = request(&sock, "GET", "/api/rules", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let rules = v["rules"].as_array().cloned().unwrap_or_default();
            if rules.is_empty() {
                println!("No standing rules: agent blocks ask (\"Always\" for a directory or everywhere makes one)");
            }
            for r in rules {
                println!("{:>3}  {}", r["index"], r["text"].as_str().unwrap_or(""));
            }
        }
        Command::Ide { diffs } => {
            let v = match diffs {
                Some(d) => {
                    request(&sock, "PUT", "/api/ide", Some(&json!({ "diffs": d })))?.json()?;
                    request(&sock, "GET", "/api/ide", None)?.json()?
                }
                None => request(&sock, "GET", "/api/ide", None)?.json()?,
            };
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            if v["on"] != true {
                println!("illogicald isn't Claude Code's IDE (--no-claude-ide)");
                return Ok(0);
            }
            println!("Claude Code's IDE on port {} ({})", v["port"], v["lock_dir"].as_str().unwrap_or(""));
            println!("diffs go to: {}", v["diffs"].as_str().unwrap_or(""));
            for o in v["others"].as_array().into_iter().flatten() {
                let alive = if o["alive"] == true { "" } else { "  (gone)" };
                let folders: Vec<&str> =
                    o["folders"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                println!(
                    "  also: {:<24} port {}  {}{alive}",
                    o["name"].as_str().unwrap_or("?"),
                    o["port"],
                    folders.join(", ")
                );
            }
        }
        Command::ShellEnv { refresh } => {
            let v = match refresh {
                true => request(&sock, "POST", "/api/hosts/self/shell-env/refresh", None)?.json()?,
                false => request(&sock, "GET", "/api/hosts/self/shell-env", None)?.json()?,
            };
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let shell = v["shell"].as_str().unwrap_or("?");
            match v["error"].as_str() {
                Some(e) => println!("{shell}: {e}; blocks get the daemon's environment"),
                None => {
                    println!("{shell} ({} ms)", v["ms"]);
                    match v["path"].as_str() {
                        Some(p) => println!("PATH={p}"),
                        None => println!("PATH is the daemon's"),
                    }
                    let vars: Vec<&str> =
                        v["vars"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                    println!("also sets: {}", vars.into_iter().filter(|k| *k != "PATH").collect::<Vec<_>>().join(" "));
                }
            }
        }
        Command::Machines => {
            let v = request(&sock, "GET", "/api/machines", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for m in v.as_array().into_iter().flatten() {
                let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("");
                let image = m["image"].as_str().unwrap_or("default image");
                let owner = match (m["owner"]["tab"].as_u64(), m["owner"]["pane"].as_u64()) {
                    (Some(t), _) => format!("@{t}"),
                    (_, Some(p)) => format!("%{p}"),
                    _ => "?".into(),
                };
                let (state, sprite, provider, name) = (s("state"), s("sprite"), s("provider"), s("name"));
                println!("m{:<4} {owner:<5} {state:<9} {name:<18} {sprite:<34} {provider} ({image})", m["id"]);
            }
        }
        Command::Fs { cmd } => return fs::run(&sock, cmd, json_out, REMOTE.load(std::sync::atomic::Ordering::Relaxed)),
        Command::Cd { pane, dir } => return fs::cd(&sock, pane.0, &dir),
        Command::Run { session, split, join, cwd, policy: pol, wait, vm, vm_tab, image, sandbox, home, command } => {
            if image.is_some() && !vm && !vm_tab {
                anyhow::bail!("--image is for --vm or --vm-tab");
            }
            if home {
                let Some(host) = host_name.as_deref().filter(|h| !h.contains("://")) else {
                    anyhow::bail!("--home needs --host NAME, a host in this daemon's list");
                };
                let local = http::Target::Socket(local_sock);
                let this = request(&local, "GET", "/api/hosts", None)?.json()?["this"]
                    .as_str()
                    .context("this daemon has no name")?
                    .to_owned();
                // On the host first, in a session named after us...
                let body = json!({
                    "command": (!command.is_empty()).then(|| shell_command(&command)),
                    "vm": vm,
                    "image": image,
                    "sandbox": sandbox,
                    "session": this,
                    "cwd": cwd,
                    "policy": pol.as_deref().map(policy).transpose()?,
                });
                let v = request(&sock, "POST", "/api/run", Some(&body))?.json()?;
                let pane = v["pane"].as_u64().context("no pane in the answer")?;
                // ...then its place in our layout.
                let from = std::env::var("ILLOGICAL_PANE").ok().and_then(|v| v.parse::<u32>().ok());
                let body = json!({
                    "type": "remote",
                    "config": {"host": host, "pane": pane},
                    "session": session,
                    "split": split.map(|p| p.0),
                    "from_pane": from,
                });
                let b = request(&local, "POST", "/api/blocks", Some(&body))?.json()?;
                let block = b["block"].as_u64().context("no block in the answer")?;
                if json_out {
                    print_json(&json!({"block": block, "host": host, "pane": pane}));
                } else {
                    println!("%{block} ({host} %{pane})");
                }
                if wait {
                    let w = request(&sock, "GET", &format!("/api/panes/{pane}/wait?until=exit"), None)?.json()?;
                    return Ok(w["code"].as_i64().unwrap_or(1) as i32);
                }
                return Ok(0);
            }
            // A VM (or another daemon's host) has none of this host's
            // directories.
            let cwd = if vm || vm_tab || join || sandbox.is_some() || REMOTE.load(std::sync::atomic::Ordering::Relaxed)
            {
                cwd
            } else {
                cwd.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
            };
            let body = json!({
                "command": (!command.is_empty()).then(|| shell_command(&command)),
                "vm": vm,
                "vm_tab": vm_tab,
                "image": image,
                "sandbox": sandbox,
                "session": session,
                "split": split.map(|p| p.0),
                "join": join,
                "cwd": cwd,
                "policy": pol.as_deref().map(policy).transpose()?,
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/run", Some(&body))?.json()?;
            let pane = v["pane"].as_u64().context("no pane in the answer")?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{pane}");
            }
            if wait {
                let w = request(&sock, "GET", &format!("/api/panes/{pane}/wait?until=exit"), None)?.json()?;
                return Ok(w["code"].as_i64().unwrap_or(1) as i32);
            }
        }
        Command::Send { pane, text, enter, wait, answering, timeout } => {
            let mut text = text.join(" ");
            if text == "-" {
                text.clear();
                std::io::stdin().read_to_string(&mut text)?;
            }
            if wait {
                let body = json!({"text": text, "answering": answering, "timeout": timeout});
                let r = request(&sock, "POST", &format!("/api/panes/{}/prompt", pane.0), Some(&body))?.json()?;
                let (line, code) = prompted(pane.0, &r);
                println!("{line}");
                return Ok(code);
            }
            request(
                &sock,
                "POST",
                &format!("/api/panes/{}/send", pane.0),
                Some(&json!({"text": text, "enter": enter})),
            )?
            .json()?;
        }
        Command::Keys { pane, keys } => {
            request(&sock, "POST", &format!("/api/panes/{}/keys", pane.0), Some(&json!({"keys": keys})))?.json()?;
        }
        Command::Mouse { pane, x, y, button, action } => {
            let body = json!({"x": x, "y": y, "button": button, "action": action});
            request(&sock, "POST", &format!("/api/panes/{}/mouse", pane.0), Some(&body))?.json()?;
        }
        Command::Tail { pane, follow, from, last_command, text, synced } => {
            let synced = synced_q(synced);
            // Another host's pane number means nothing here: say which.
            let pane = if synced.is_some() { pane.map(|p| p.0).context("which pane? (give %N)")? } else { here(pane)? };
            let mut q: Vec<String> = synced.into_iter().collect();
            if let Some(f) = from {
                q.push(format!("from={f}"));
            }
            if last_command {
                q.push("from=last-command".into());
            }
            if follow {
                q.push("follow=1".into());
            }
            if text {
                q.push("text=1".into());
            }
            let mut res = request(&sock, "GET", &format!("/api/panes/{pane}/tail?{}", q.join("&")), None)?.ok()?;
            let mut out = std::io::stdout().lock();
            let mut buf = [0u8; 65536];
            loop {
                let n = res.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
        }
        Command::Wait { pane, command_end, exit, matching, idle, needs_input, timeout } => {
            let pane = here(pane)?;
            let mut q = match (command_end, exit, &matching) {
                _ if idle => "until=idle".to_owned(),
                _ if needs_input => "until=needs-input".to_owned(),
                (_, true, _) => "until=exit".to_owned(),
                (_, _, Some(re)) => format!("until=match&re={}", enc(re)),
                _ => "until=command-end".to_owned(),
            };
            if let Some(t) = timeout {
                q.push_str(&format!("&timeout={t}"));
            }
            let v = request(&sock, "GET", &format!("/api/panes/{pane}/wait?{q}"), None)?.json()?;
            if json_out {
                print_json(&v);
            }
            return Ok(match v["result"].as_str() {
                Some("timeout") => {
                    if !json_out {
                        eprintln!("illogical: timed out");
                    }
                    124
                }
                Some("command_end") => {
                    if !json_out {
                        println!("{} exited {}", v["text"].as_str().unwrap_or("command"), v["exit"]);
                    }
                    v["exit"].as_i64().unwrap_or(0) as i32
                }
                Some("exit") => v["code"].as_i64().unwrap_or(0) as i32,
                Some("attention") => {
                    if json_out {
                    } else if v["ask"].is_object() {
                        // What it asks, for a script (or another agent) to
                        // answer with `call %N answer`.
                        print_json(&v["ask"]);
                    } else {
                        println!("{}", v["state"].as_str().unwrap_or("").replace('_', "-"));
                    }
                    0
                }
                Some("match") => {
                    if !json_out {
                        println!("{}", v["text"].as_str().unwrap_or(""));
                    }
                    0
                }
                _ => 1,
            });
        }
        Command::Attach { pane } => return attach::run(&sock, here(pane)?),
        Command::Tui { session } => return tui::run(&sock, session),
        Command::Export { pane, cast: _, output } => {
            let pane = here(pane)?;
            let text = request(&sock, "GET", &format!("/api/panes/{pane}/export.cast"), None)?.ok()?.text()?;
            match output {
                Some(path) => std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?,
                None => print!("{text}"),
            }
        }
        Command::Process { pane } => {
            let v = request(&sock, "GET", &format!("/api/panes/{}/process", here(pane)?), None)?.json()?;
            if json_out {
                print_json(&v);
            } else {
                let argv: Vec<&str> = v["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                println!("{} {}  (cwd {})", v["foreground"], argv.join(" "), v["cwd"].as_str().unwrap_or("?"));
            }
        }
        Command::Capture { pane, ansi, html, scrollback, last_command } => {
            let format = if ansi {
                "ansi"
            } else if html {
                "html"
            } else {
                "text"
            };
            let scope = if scrollback {
                "scrollback"
            } else if last_command {
                "last-command"
            } else {
                "screen"
            };
            let path = format!("/api/panes/{}/capture?format={format}&scope={scope}", here(pane)?);
            let text = request(&sock, "GET", &path, None)?.ok()?.text()?;
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
        }
        Command::Events { follow, pane, types, since } => {
            let mut q = vec![];
            if follow {
                q.push("follow=1".to_owned());
            }
            if let Some(p) = pane {
                q.push(format!("pane={}", p.0));
            }
            if let Some(t) = types {
                q.push(format!("type={}", enc(&t)));
            }
            if let Some(s) = since {
                q.push(format!("since={}", duration(&s)?));
            }
            let mut res = request(&sock, "GET", &format!("/api/events?{}", q.join("&")), None)?.ok()?;
            let mut out = std::io::stdout().lock();
            let mut buf = [0u8; 16384];
            loop {
                let n = res.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
        }
        Command::Close { panes } => {
            for p in panes {
                // A remote pane (#17): close it on its host too. If that
                // can't be reached, it stays in the host's own layout.
                let remote = match request(&sock, "GET", &format!("/api/blocks/{}", p.0), None).and_then(|r| r.json()) {
                    Ok(d) if d["info"]["type"] == "remote" && !REMOTE.load(std::sync::atomic::Ordering::Relaxed) => {
                        Some((d["state"]["host"].as_str().unwrap_or_default().to_owned(), d["state"]["pane"].clone()))
                    }
                    _ => None,
                };
                if let Some((host, pane)) = remote {
                    let closed = hosts::target(local_sock.clone(), Some(&host))
                        .and_then(|t| request(&t, "POST", &format!("/api/panes/{pane}/close"), None)?.json());
                    if let Err(e) = closed {
                        eprintln!("illogical: %{pane} on {host} stays open there: {e:#}");
                    }
                }
                request(&sock, "POST", &format!("/api/panes/{}/close", p.0), None)?.json()?;
            }
        }
        Command::Ask
        | Command::Hook
        | Command::Inbox
        | Command::Hooks { .. }
        | Command::Fountain { cmd: Some(FountainCmd::Runner { .. }), .. } => {
            unreachable!("handled first")
        }
        Command::Attention { state: None, .. } => {
            let v = request(&sock, "GET", "/api/attention", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for i in v.as_array().into_iter().flatten() {
                let r = &i["reason"];
                let bundle = r["bundle"].as_str().map(|b| format!("  [{b}]")).unwrap_or_default();
                println!(
                    "%{:<4} {:<7} {}{bundle}",
                    i["pane"],
                    r["kind"].as_str().unwrap_or(""),
                    r["headline"].as_str().unwrap_or("")
                );
            }
        }
        Command::Attention { state: Some(state), pane } => {
            let state = state.replace('-', "_");
            // Hooks (Claude Code's, say) run this in every terminal; outside an
            // illogical pane there's nobody to tell, and that's fine.
            let Ok(pane) = here(pane) else { return Ok(0) };
            let path = format!("/api/panes/{pane}/attention");
            request(&sock, "POST", &path, Some(&json!({"state": state, "why": hook_message()})))?.json()?;
        }
        Command::History { pane, failed, since, cwd, matching, limit, synced } => {
            let mut q = vec![format!("limit={limit}")];
            q.extend(synced_q(synced));
            if let Some(p) = pane {
                q.push(format!("pane={}", p.0));
            }
            if failed {
                q.push("failed=1".into());
            }
            if let Some(s) = since {
                q.push(format!("since={}", duration(&s)?));
            }
            if let Some(c) = cwd {
                q.push(format!("cwd={}", enc(&c)));
            }
            if let Some(m) = matching {
                q.push(format!("match={}", enc(&m)));
            }
            let v = request(&sock, "GET", &format!("/api/history?{}", q.join("&")), None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for c in v.as_array().into_iter().flatten() {
                let exit = match c["exit"].as_i64() {
                    Some(0) => "  ".to_owned(),
                    Some(e) => format!("{e:>2}"),
                    None => " …".to_owned(),
                };
                let closed = if c["open"].as_bool() == Some(false) { " (closed)" } else { "" };
                let host = c["host"].as_str().map(|h| format!("{h}:")).unwrap_or_default();
                let by = c["by"].as_str().map(|b| format!("  by {b}")).unwrap_or_default();
                println!(
                    "{exit}  {host}%{:<4} {:>8}  {}{closed}   [{}]{by}",
                    c["pane"],
                    time(c["started_ms"].as_u64().unwrap_or(0)),
                    c["text"].as_str().unwrap_or("?"),
                    c["cwd"].as_str().unwrap_or("")
                );
            }
        }
        Command::Log { pane, who } => {
            let id = here(pane)?;
            if !who {
                let v = request(&sock, "GET", &format!("/api/history?pane={id}&limit=1000"), None)?.json()?;
                if json_out {
                    print_json(&v);
                    return Ok(0);
                }
                for c in v.as_array().into_iter().flatten() {
                    println!(
                        "{:>8}  {:<16} {}",
                        time(c["started_ms"].as_u64().unwrap_or(0)),
                        c["by"].as_str().unwrap_or("-"),
                        c["text"].as_str().unwrap_or("?")
                    );
                }
                return Ok(0);
            }
            let v = request(&sock, "GET", &format!("/api/panes/{id}/drivers"), None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for d in v.as_array().into_iter().flatten() {
                println!(
                    "{:>8}  @{:<10} {}",
                    time(d["at_ms"].as_u64().unwrap_or(0)),
                    d["offset"],
                    d["who"].as_str().unwrap_or("")
                );
            }
        }
        Command::Search { re, since, limit, synced } => {
            let mut q = vec![format!("re={}", enc(&re)), format!("limit={limit}")];
            q.extend(synced_q(synced));
            if let Some(s) = since {
                q.push(format!("since={}", duration(&s)?));
            }
            let v = request(&sock, "GET", &format!("/api/search?{}", q.join("&")), None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for h in v.as_array().into_iter().flatten() {
                let cmd = h["command"].as_str().map(|c| format!("  ({c})")).unwrap_or_default();
                let host = h["host"].as_str().map(|h| format!("{h}:")).unwrap_or_default();
                println!("{host}%{}@{}: {}{cmd}", h["pane"], h["offset"], h["line"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(0)
}

/// The shell command line for `run`: a single argument as written, several
/// as words, each quoted if it needs to be.
fn shell_command(argv: &[String]) -> String {
    if let [one] = argv {
        return one.clone();
    }
    let safe = |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b));
    argv.iter()
        .map(|w| if safe(w) { w.clone() } else { format!("'{}'", w.replace('\'', r"'\''")) })
        .collect::<Vec<_>>()
        .join(" ")
}

/// illogical's VS Code extension, from the daemon (M28).
fn vsix(sock: &http::Target) -> anyhow::Result<(String, Vec<u8>)> {
    let res = request(sock, "GET", "/api/editors/vsix", None)?;
    let name = res
        .header("content-disposition")
        .and_then(|d| d.split("filename=").nth(1))
        .map(|f| f.trim_matches('"').to_owned())
        .unwrap_or_else(|| "illogical-editor.vsix".into());
    Ok((name, res.bytes()?))
}

/// A secret from stdin: piped, the first line; on a terminal, asked for
/// without echo.
fn secret_input(prompt: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    let mut line = String::new();
    if tty {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let read = term::read_hidden(&mut line);
        eprintln!();
        read?;
    } else {
        stdin.read_line(&mut line)?;
    }
    let v = line.trim().to_owned();
    if v.is_empty() {
        bail!("nothing given on stdin");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn detection_for_people() {
        let v = serde_json::json!({
            "agent": "claude", "name": "Claude Code", "shown": "blocked", "fired": "permission_prompt",
            "title": "✳ Create a file",
            "rules": [
                {"rule": "permission_prompt", "state": "blocked", "priority": 1000, "region": "after the last rule",
                 "text": [" Do you want to proceed?", "", " ❯ 1. Yes"], "matched": true},
                {"rule": "title_idle", "state": "idle", "priority": 250, "region": "title", "text": [""], "matched": false},
            ],
        });
        let text = super::detection_text(4, &v);
        assert!(
            text.starts_with("%4 runs Claude Code (claude): blocked by rule permission_prompt (shown: blocked)\n"),
            "{text}"
        );
        assert!(text.contains("\npermission_prompt (blocked, 1000) after the last rule: matched\n  |  Do you want to proceed?\n  |  ❯ 1. Yes\n"), "{text}");
        assert!(text.contains("\ntitle_idle (idle, 250) title: no match\n  (empty)\n"), "{text}");
        let none = super::detection_text(2, &serde_json::json!({"agent": null, "command": "vim notes"}));
        assert_eq!(none, "%2 runs `vim notes`: no agent with screen rules\n");
        let unread = super::detection_text(
            3,
            &serde_json::json!({"agent": "codex", "name": "Codex", "unread": true, "rules": [], "configured": ["claude"]}),
        );
        assert!(
            unread.starts_with(
                "%3 runs Codex (codex): its screen isn't read here. chant audit --agents found claude configured"
            ),
            "{unread}"
        );
    }

    #[test]
    fn inventory_for_people() {
        let v = serde_json::json!({
            "state": "read", "version": "0.95.0", "notes": [],
            "sites": [{"id": "user-claude", "scope": "user", "runtime": "claude", "root": "/home/ada",
                       "summary": "1 instruction file · model sonnet"}],
            "rules": {"run": ["claude"], "off": ["codex"]},
        });
        assert_eq!(
            super::inventory_text(&v),
            "chant 0.95.0 (audit --agents) found:\n  claude    user    /home/ada  1 instruction file · model sonnet\n\
             screen rules run for: claude\nnot configured here, so not read: codex\n"
        );
        let none = serde_json::json!({"state": "no_chant", "error": "no chant on PATH", "sites": [],
                                      "rules": {"run": ["claude", "codex"], "off": []}});
        assert_eq!(
            super::inventory_text(&none),
            "no inventory: no chant on PATH\nscreen rules run for: every agent with rules\n"
        );
    }

    #[test]
    fn web_link_over_ssh() {
        assert_eq!(super::ssh_hint("http://127.0.0.1:7681", false), None);
        let hint = super::ssh_hint("http://127.0.0.1:7681", true).unwrap();
        assert!(hint.ends_with("ssh -L 7681:127.0.0.1:7681 <this machine>"), "{hint}");
    }

    #[test]
    fn web_forward_for_an_ssh_box() {
        let hint = super::ssh_forward("http://127.0.0.1:7681/", "box").unwrap();
        assert!(hint.ends_with("ssh -N -L 7681:127.0.0.1:7681 box"), "{hint}");
        assert_eq!(super::ssh_forward("https://x", "box"), None);
    }

    #[test]
    fn durations_and_policies() {
        assert_eq!(super::duration("90").unwrap(), 90);
        assert_eq!(super::duration("30m").unwrap(), 1800);
        assert_eq!(super::duration("2d").unwrap(), 172800);
        assert!(super::duration("2w").is_err());
        assert_eq!(super::policy("hook:claude --continue").unwrap()["command"], "claude --continue");
        assert_eq!("%12".parse::<super::Pane>().unwrap().0, 12);
    }

    #[test]
    fn files_with_lines() {
        let s = |p: &str, check| super::file_line(p, check);
        assert_eq!(s("src/main.rs:42", false), ("src/main.rs".into(), Some(42)));
        assert_eq!(s("src/main.rs", false), ("src/main.rs".into(), None));
        assert_eq!(s(":42", false), (":42".into(), None));
        assert_eq!(s("a:b", false), ("a:b".into(), None));
        // Here, only when the file is there.
        assert_eq!(s("/nowhere/x.rs:3", true), ("/nowhere/x.rs:3".into(), None));
        let there = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert_eq!(s(&format!("{there}:3"), true), (there.into(), Some(3)));
        // Unix paths: on Windows `/a/b` has no drive.
        if cfg!(unix) {
            assert_eq!(super::absolute("/a/b").unwrap(), "/a/b");
            assert!(super::absolute("b").unwrap().ends_with("/b"));
        }
    }

    #[test]
    fn run_quotes_words() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::shell_command(&v(&["make && ./app"])), "make && ./app");
        assert_eq!(super::shell_command(&v(&["make", "test"])), "make test");
        assert_eq!(super::shell_command(&v(&["bash", "-c", "echo hi; exit 3"])), "bash -c 'echo hi; exit 3'");
        assert_eq!(super::shell_command(&v(&["echo", "it's"])), r"echo 'it'\''s'");
    }

    #[test]
    fn machine_is_not_the_global_host() {
        use clap::Parser;
        let parse = |a: &[&str]| super::Cli::try_parse_from(a).unwrap();
        let c = parse(&["illogical", "--host", "box", "open", "--machine", "m2", ":3000"]);
        assert_eq!(c.host.as_deref(), Some("box"));
        assert!(matches!(c.cmd, super::Command::Open { machine: Some(ref m), .. } if m == "m2"));
        let c = parse(&["illogical", "agent", "--machine", "3", "hi"]);
        assert!(c.host.is_none());
        assert!(matches!(c.cmd, super::Command::Agent { machine: Some(ref m), .. } if m == "3"));
        assert!(super::Cli::try_parse_from(["illogical", "agent", "--vm", "--machine", "m3"]).is_err());
        assert!(super::looks_like_machine("m2") && super::looks_like_machine("local"));
        assert!(!super::looks_like_machine("box"));
    }
}
