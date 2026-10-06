//! `illogical`: drive illogicald from a shell or a script. Every command
//! talks to the daemon's HTTP API over its Unix socket (or another daemon's
//! URL, with `--host`); `--json` prints the API's answers as they are, for
//! programs.

mod ask;
mod attach;
mod cmd;
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
mod util;
mod wake;

// tmux's control mode (`-CC`) front is for iTerm2 over ssh to a Unix box.
#[cfg(not(unix))]
mod tmux {
    pub fn run(_: crate::http::Target, _: &[String]) -> anyhow::Result<i32> {
        anyhow::bail!("tmux control mode is for Unix boxes (iTerm2 over ssh)")
    }
}

use std::path::PathBuf;

use clap::{FromArgMatches, Parser, Subcommand};
use cmd::{claude::ClaudeCmd, fountain::FountainCmd};
use util::{Pane, REMOTE};

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

#[derive(Subcommand)]
enum Command {
    /// List panes.
    Ls,
    /// Run a command in a new tab (or split); prints its pane.
    Run(cmd::run::Args),
    /// Machines that panes run on.
    #[command(hide = true)]
    Machines,
    /// Files on a host, read-only.
    ///
    /// `ls`, `stat`, `cat`, `watch`, `recent`. `%N:PATH` is on the host pane %N
    /// runs on, `mN:PATH` on machine N.
    Fs {
        #[command(subcommand)]
        cmd: fs::FsCmd,
    },
    /// Type `cd DIR` into a pane's shell, if it's waiting at its prompt.
    Cd { pane: Pane, dir: String },
    /// A block's type, place and state (any type).
    ///
    /// With `--detection`, how the screen of the agent in a terminal pane reads:
    /// each rule, the text it looked at, and which one fired. `describe --agents`:
    /// the agents configured on this machine, which decide whose screen rules run
    /// here.
    Describe(cmd::describe::Args),
    /// Call one of a block's methods.
    ///
    /// For example `call %4 navigate '{"url":"…"}'`.
    Call(cmd::call::Args),
    /// Open a browser block on a port or a web page.
    ///
    /// `:PORT[/path]` is a port on its machine, `https://…` a web page.
    Open(cmd::open::Args),
    /// Open VS Code on a folder or a file; prints its block.
    ///
    /// The editor runs on the machine this pane runs on. A file opens in its
    /// project.
    Edit(cmd::edit::Args),
    /// Show what changed in a git repository.
    ///
    /// Opens a diff block, prints it, then its files with +/−. No revisions: the
    /// working tree (staged, unstaged, untracked) against HEAD; one: against that;
    /// two: the range. `%N` first: the repository pane %N is in, on its machine.
    Diff(cmd::diff::Args),
    /// Show a file in a file block, read-only and followed live.
    ///
    /// `PATH[:LINE]` here, `%N:PATH[:LINE]` on the host pane %N runs on (relative
    /// to its directory), `mN:PATH[:LINE]` on machine N. Prints its block.
    View(cmd::view::Args),
    /// Show a chant workspace as a block.
    ///
    /// Its members as cards to open shells, agents and diffs on, its records, and
    /// the gates waiting for a person, which are attention you approve (`call %N
    /// approve`). Read through the workspace's own chant. Prints the block, then
    /// its members and gates.
    #[command(hide = true)]
    Workspace(cmd::workspace::Args),
    /// Show a pull request as a block.
    ///
    /// Forgejo through your `tea` login, GitHub through `gh`'s, a GitLab merge
    /// request through your `glab` login (or read-only without one): its checks,
    /// reviews and timeline, and what it waits on you for. `URL`,
    /// `OWNER/REPO#N`, `GROUP/PROJECT!N`, or `N` in this directory's repository.
    /// Prints the block, then the PR as text. `pr comment|review|merge|rerun %N`
    /// write to it; run by an agent (CLAUDECODE or AI_AGENT set), a write is a
    /// draft that waits for a person to send it.
    #[command(args_conflicts_with_subcommands = true)]
    Pr(cmd::pr::Args),
    /// Your Fountain agents as a catalog block.
    ///
    /// Read with your own `fountain` login (FOUNTAIN_API_KEY or
    /// ~/.fountain/credentials): a card per agent, where it comes from, filters,
    /// and Run on Fountain / Spec. Prints the block, then the list. `fountain
    /// agents [QUERY]` lists them here without a block. `fountain --view runner`:
    /// this host as the account's runner instead, with its sandboxes (Follow,
    /// Changes, Shell).
    #[command(args_conflicts_with_subcommands = true, hide = true)]
    Fountain(cmd::fountain::Args),
    /// Show an issue as a block.
    ///
    /// Forgejo through your `tea` login: its labels, assignees, linked pull
    /// requests and timeline. `URL`, `OWNER/REPO#N`, or `N` in this directory's
    /// repository. `issue new` opens one (run by an agent, it's a draft a person
    /// sends); `issue comment %N` comments; `issue agent %N` starts an agent on it
    /// in a worktree and branch of its own, in a tab with the issue.
    #[command(args_conflicts_with_subcommands = true)]
    Issue(cmd::issue::Args),
    /// Type a pane's failed command again.
    ///
    /// Once its shell is waiting at its prompt.
    Rerun(cmd::rerun::Args),
    /// Claude Code conversations on this machine.
    ///
    /// From a terminal or the desktop app's Code tab. `open` shows one as an agent
    /// block; `illogical agent --resume ID` continues one.
    Claude {
        #[command(subcommand)]
        cmd: cmd::claude::ClaudeCmd,
    },
    /// Your studio.
    ///
    /// `login URL` keeps a studio token in the daemon (read from stdin), `logout`
    /// forgets it, and `follower APP` keeps a hud follower link for an app's box.
    #[command(hide = true)]
    Studio {
        #[command(subcommand)]
        cmd: Option<cmd::studio::StudioCmd>,
    },
    /// Open a studio app's box as a block; prints its block.
    ///
    /// With no name, lists your apps.
    #[command(hide = true)]
    App(cmd::app::Args),
    /// Editors in the swarm: VS Code, Cursor or nvim that joined.
    ///
    /// Also editor blocks. `editors install` adds illogical's extension to VS Code
    /// or Cursor here (in a Remote-SSH window's terminal: there).
    Editors {
        #[command(subcommand)]
        cmd: Option<cmd::editors::EditorsCmd>,
    },
    /// illogicald as Claude Code's IDE.
    ///
    /// Its port, and which IDE gets Claude Code's diffs (`--diffs illogical`, or
    /// another IDE's name as it registered, e.g. "Visual Studio Code").
    Ide(cmd::ide::Args),
    /// Standing permission rules for agent blocks.
    ///
    /// What agent blocks on this daemon allow without asking, made by "Always" for
    /// a directory or for every block. `--forget N` forgets one; `--forget-all`,
    /// all of them.
    Rules(cmd::rules::Args),
    /// The shell environment blocks that run your tools get.
    ///
    /// Your login shell's, read once: its PATH. `--refresh` reads it again, after
    /// you change an rc file.
    ShellEnv(cmd::shell_env::Args),
    /// Start an agent block and send it a prompt; prints its block.
    ///
    /// Claude Code by default. Then: `wait %N --idle`, `tail %N`, `call %N
    /// approve`.
    Agent(cmd::agent::Args),
    /// Type text into a pane (`-` reads stdin).
    ///
    /// With `--wait`, it's a prompt for the agent there (Claude Code or Codex in
    /// the terminal, or an agent block): sent with Enter, then waited through.
    /// Prints what it came to and exits 0 when the turn ended, 2 when it needs
    /// someone (or already did, so nothing was typed), 3 when it stalled (no sign
    /// of work), 4 still running at --timeout.
    Send(cmd::send::Args),
    /// Press named keys: C-c, M-x, Up, Enter, F5, Space, ...
    Keys(cmd::keys::Args),
    /// Copy files onto the pane's host and paste their paths into it.
    ///
    /// For an agent there to read: a screenshot into a `claude` on another
    /// machine, say. Only into a shell or an agent unless `--force`; exits 2 if it
    /// wasn't pasted, printing the paths.
    Upload(cmd::upload::Args),
    /// Click, press, release or drag at a cell (from 1,1).
    Mouse(cmd::mouse::Args),
    /// Print a pane's output.
    Tail(cmd::tail::Args),
    /// Wait for a command, an exit, a match, or an agent.
    ///
    /// For a command to finish, the program to exit, or output to match.
    /// Exits with the command's exit code; 124 on timeout.
    Wait(cmd::wait::Args),
    /// Use a pane from this terminal (Ctrl-] to detach).
    Attach { pane: Option<Pane> },
    /// The daemon's tabs and splits in this terminal.
    ///
    /// With a sidebar of sessions, tabs and what needs you. Ctrl-] is the menu key.
    Tui {
        /// Start in this session (name or id); made if there's none.
        #[arg(long)]
        session: Option<String>,
    },
    /// Export a pane's history as an asciicast (`asciinema play`).
    Export(cmd::export::Args),
    /// The pane's foreground process.
    Process(cmd::process::Args),
    /// What a pane shows: the screen, the scrollback or the last command.
    Capture(cmd::capture::Args),
    /// Events as they happen (NDJSON).
    Events(cmd::events::Args),
    /// Close a pane (ending what runs in it); its output stays in history.
    Close(cmd::close::Args),
    /// Claude Code's hook for its questions (PreToolUse on AskUserQuestion).
    ///
    /// Shows its questions as a card beside this pane (every client, with a push),
    /// waits, and prints the answer for Claude Code. Outside an illogical pane, or
    /// "Answer in terminal": no output, so Claude Code shows its picker.
    Ask,
    /// Claude Code's hook for permission prompts.
    ///
    /// `PermissionRequest` becomes an approval card anyone who may answer can allow
    /// or deny; other events close a card the terminal answered first. Outside an
    /// illogical pane: nothing.
    Hook,
    /// Claude Code's background hook for follow-ups (`Stop`, `SessionStart`).
    ///
    /// Waits for a follow-up someone sends the agent, and wakes it with it (exit
    /// 2). A session nobody drives (`claude -p`, the SDK) isn't held: it exits 0 at
    /// once.
    Inbox,
    /// Put Claude Code's hooks in its settings.json, or say which are there.
    ///
    /// `install` adds them, `status` says which are there. Nothing else in the
    /// file is touched.
    Hooks {
        #[command(subcommand)]
        cmd: hooks::HooksCmd,
    },
    /// What wants you, and why.
    ///
    /// Or, given a state, tell illogical whether this pane needs you (for agent
    /// hooks, which pass their JSON on stdin: its `message` becomes the headline).
    Attention(cmd::attention::Args),
    /// Commands run in any pane, including recently closed ones
    ///
    /// Also the answers and approvals given there, and what agent blocks did
    /// (`--kind command|answer|agent`).
    History(cmd::history::Args),
    /// A pane's commands and who ran each.
    ///
    /// `--who`: who typed in it over time (each handoff).
    Log(cmd::log::Args),
    /// Search the output of every pane.
    Search(cmd::search::Args),
    /// A read-only link to a pane.
    ///
    /// Whoever opens it on the tailnet sees it live and can't type, resize or see
    /// anything else.
    Share(cmd::share::Args),
    /// ssh invites that still work.
    ///
    /// Made with `share --guest`. `guests revoke ID` ends one and cuts off
    /// anyone using it.
    #[command(hide = true)]
    Guests {
        #[command(subcommand)]
        cmd: Option<cmd::share::SharesCmd>,
    },
    /// Who else can reach which sessions.
    ///
    /// `access` lists grants, `access grant SESSION WHO ROLE`, `access revoke
    /// SESSION WHO`, `access log`. WHO is a tailnet login (or `account:ID` from
    /// control).
    Access {
        #[command(subcommand)]
        cmd: Option<cmd::access::AccessCmd>,
    },
    /// Bring someone into a session.
    ///
    /// Shares it with them (as a viewer unless --role says otherwise) and
    /// notifies them alone, opening at a pane. WHO is a tailnet login, someone already shared with, or (when
    /// joined to illogical control) a member of your teams, by name.
    /// Prints whether the notification reached them: sent, pending (they
    /// haven't accepted this machine yet) or unreachable, and why.
    Invite(cmd::invite::Args),
    /// Share links that still work.
    ///
    /// `shares revoke ID` ends one.
    Shares {
        #[command(subcommand)]
        cmd: Option<cmd::share::SharesCmd>,
    },
    /// History other hosts synced here (kept encrypted).
    Synced {
        #[command(subcommand)]
        cmd: Option<cmd::synced::SyncedCmd>,
    },
    /// Open this machine's page in your browser, signed in.
    ///
    /// Programs and browsers on this machine show the daemon's local token; this
    /// opens a sign-in link that gives your browser it (once: it stays signed in).
    /// `--print` prints the link instead (it holds the token).
    Web(cmd::web::Args),
    /// Install the daemon.
    ///
    /// `illogicald install` with these arguments.
    Install(cmd::install::Args),
    /// Add a machine to your account on illogical control.
    ///
    /// So the web, the phone and other machines reach it through control. With
    /// `--ssh user@box`: that box, set up over ssh first (illogical installed, its
    /// daemon kept running after you log out); its code shows here, to approve from
    /// a signed-in device. Without: this machine.
    Join(cmd::join::Args),
    /// Make this CLI one of your devices on illogical control.
    ///
    /// So `--host NAME` reaches every machine on your account, directly or through
    /// control's relay. Shows a code to approve on a signed-in device.
    Login(cmd::login::Args),
    /// Forget this CLI's key for control (`illogical login` makes a new one).
    Logout,
    /// How illogical is doing on this machine.
    ///
    /// The daemon, and whether and where it's joined to illogical control
    /// (connected, or dropped by control). Exits 1 when the daemon doesn't
    /// answer or control dropped it.
    Status,
    /// Join stdin and stdout to this daemon's socket.
    ///
    /// On a box a client reaches over ssh (`--ssh`). Clients run it; people
    /// don't.
    #[command(hide = true)]
    Bridge {
        /// Print what's installed and whether the daemon answers, as JSON.
        #[arg(long)]
        probe: bool,
    },
    /// Other daemons to switch to.
    ///
    /// This daemon's host list.
    Hosts {
        #[command(subcommand)]
        cmd: Option<hosts::HostsCmd>,
    },
    /// The sandbox provider's sandboxes.
    ///
    /// This daemon's: open a shell on one (`run --sandbox`), or make a daemon
    /// resident there.
    #[command(hide = true)]
    Sandboxes {
        #[command(subcommand)]
        cmd: Option<hosts::SandboxesCmd>,
    },
    /// An MCP server on stdio, for agents that start one as a command.
    ///
    /// `claude mcp add illogical -- illogical mcp`: illogical's tools, bridged to
    /// the daemon's `/mcp`. `mcp token` makes tokens for clients that reach `/mcp`
    /// over HTTP without a tailnet identity.
    Mcp(cmd::mcp::Args),
    /// Be a tmux server in control mode for iTerm2.
    ///
    /// Also other tmux `-CC` clients: `illogical tmux -CC [attach -t SESSION | new
    /// -s NAME]`. Linked or installed as `tmux`, the CLI does this by itself.
    Tmux {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// The local daemon's state directory, resolved as the daemon and the desktop
/// app do: `ILLOGICAL_STATE_DIR`, then on Windows
/// `%LOCALAPPDATA%\illogical\state`, then `$XDG_STATE_HOME/illogical`, then
/// `~/.local/state/illogical`.
fn state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let windows = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("illogical").join("state"));
    #[cfg(not(windows))]
    let windows = None;
    std::env::var_os("ILLOGICAL_STATE_DIR")
        .map(PathBuf::from)
        .or(windows)
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|d| PathBuf::from(d).join("illogical")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/illogical")))
}

/// Commands and options that work but stay out of `--help` unless the machine
/// has the `labs` file: they're for what a stranger doesn't have. Others that
/// are `hide = true` are internals, and stay hidden.
const LABS_COMMANDS: [&str; 7] = ["fountain", "studio", "app", "workspace", "guests", "machines", "sandboxes"];
const LABS_OPTIONS: [(&str, &[&str]); 3] = [
    ("agent", &["fountain", "as_fountain", "vault", "vm"]),
    ("run", &["vm", "vm_tab", "image", "sandbox"]),
    ("share", &["guest", "rw", "reusable", "relay", "addr", "name"]),
];

/// The command line, listing the labs set in `--help` when `labs`.
fn labs_command(labs: bool) -> clap::Command {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    if labs {
        for name in LABS_COMMANDS {
            cmd = cmd.mut_subcommand(name, |s| s.hide(false));
        }
        for (name, opts) in LABS_OPTIONS {
            cmd = cmd.mut_subcommand(name, |s| opts.iter().fold(s, |s, o| s.mut_arg(*o, |a| a.hide(false))));
        }
    }
    cmd
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
    // What a stranger doesn't get shows in `--help` where this machine has
    // the `labs` file; everything works either way.
    let labs = state_dir().is_some_and(|d| illogical_proto::hosts::labs(&d));
    let cli = Cli::from_arg_matches(&labs_command(labs).get_matches()).unwrap_or_else(|e| e.exit());
    match real_main(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("illogical: {e:#}");
            std::process::exit(1);
        }
    }
}

fn real_main(cli: Cli) -> anyhow::Result<i32> {
    if let Command::Join(args) = &cli.cmd {
        return cmd::join::run(args, &cli);
    }
    if let Command::Login(args) = &cli.cmd {
        return cmd::login::run(args, &cli);
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
    if let Command::Install(args) = &cli.cmd {
        return cmd::install::run(args);
    }
    if let Command::Ask = cli.cmd {
        // A hook: the local daemon only, and never an error.
        return Ok(ask::run(http::Target::Socket(socket(&cli))));
    }
    if let Command::Hook = cli.cmd {
        return Ok(hook::run(http::Target::Socket(socket(&cli))));
    }
    if let Command::Web(args) = &cli.cmd
        && cli.ssh.is_none()
    {
        // The local daemon's own link, over its socket (only ours); with
        // --ssh, the box's, below.
        return cmd::web::web(&http::Target::Socket(socket(&cli)), args.print);
    }
    if let Command::Inbox = cli.cmd {
        return Ok(hook::inbox(http::Target::Socket(socket(&cli))));
    }
    if let Command::Hooks { cmd } = cli.cmd {
        // A settings file: no daemon involved.
        return hooks::run(cmd, cli.json);
    }
    if let Command::Fountain(args) = &cli.cmd
        && let Some(FountainCmd::Runner { cmd }) = &args.cmd
    {
        // Fountain's API and this host's unit: no daemon involved.
        return fountain_runner::run(cmd, cli.json);
    }
    // `claude ls --host all` (#78): every host's, as each answers.
    if cli.host.as_deref() == Some("all")
        && let Command::Claude { cmd: ClaudeCmd::Ls { all, live, cwd, limit, words } } = &cli.cmd
    {
        return cmd::claude::claude_ls_all(
            socket(&cli),
            cmd::claude::conversations_path(*all, *live, cwd.clone(), *limit, words),
            cli.json,
        );
    }
    let reads_history = matches!(cli.cmd, Command::History(_) | Command::Search(_) | Command::Tail(_));
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
                && matches!(cli.cmd, Command::Open(_) | Command::Edit(_) | Command::Agent(_)) =>
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
    let ctx = cmd::Ctx { sock, json_out, local_sock, host_name, gone };
    match cli.cmd {
        Command::Share(args) => cmd::share::run(args, ctx),
        Command::Guests { cmd } => cmd::share::guests(cmd, ctx),
        Command::Access { cmd } => cmd::access::run(cmd, ctx),
        Command::Invite(args) => cmd::invite::run(args, ctx),
        Command::Shares { cmd } => cmd::share::shares(cmd, ctx),
        Command::Mcp(args) => cmd::mcp::run(args, ctx),
        Command::Synced { cmd } => cmd::synced::run(cmd, ctx),
        Command::Hosts { cmd } => {
            hosts::run(&ctx.sock, cmd, ctx.json_out, util::duration)?;
            Ok(0)
        }
        Command::Sandboxes { cmd } => {
            hosts::sandboxes(&ctx.sock, cmd, ctx.json_out)?;
            Ok(0)
        }
        // Only with --ssh: the box daemon's link, over the bridge to its
        // socket.
        Command::Web(args) if matches!(ctx.sock, http::Target::Ssh(_)) => cmd::web::web(&ctx.sock, args.print),
        Command::Install(_)
        | Command::Web(_)
        | Command::Bridge { .. }
        | Command::Join(_)
        | Command::Login(_)
        | Command::Logout => {
            unreachable!("handled before connecting")
        }
        Command::Tmux { args } => tmux::run(ctx.sock, &args),
        Command::Ls => cmd::ls::run(ctx),
        Command::Status => cmd::status::run(ctx),
        Command::Describe(args) => cmd::describe::run(args, ctx),
        Command::Call(args) => cmd::call::run(args, ctx),
        Command::Open(args) => cmd::open::run(args, ctx),
        Command::Edit(args) => cmd::edit::run(args, ctx),
        Command::Diff(args) => cmd::diff::run(args, ctx),
        Command::View(args) => cmd::view::run(args, ctx),
        Command::Workspace(args) => cmd::workspace::run(args, ctx),
        Command::Pr(args) => cmd::pr::run(args, ctx),
        Command::Fountain(args) => cmd::fountain::run(args, ctx),
        Command::Issue(args) => cmd::issue::run(args, ctx),
        Command::Rerun(args) => cmd::rerun::run(args, ctx),
        Command::Agent(args) => cmd::agent::run(args, ctx),
        Command::Claude { cmd } => cmd::claude::run(cmd, ctx),
        Command::Studio { cmd } => cmd::studio::run(cmd, ctx),
        Command::App(args) => cmd::app::run(args, ctx),
        Command::Editors { cmd } => cmd::editors::run(cmd, ctx),
        Command::Rules(args) => cmd::rules::run(args, ctx),
        Command::Ide(args) => cmd::ide::run(args, ctx),
        Command::ShellEnv(args) => cmd::shell_env::run(args, ctx),
        Command::Machines => cmd::machines::run(ctx),
        Command::Fs { cmd } => fs::run(&ctx.sock, cmd, ctx.json_out, REMOTE.load(std::sync::atomic::Ordering::Relaxed)),
        Command::Cd { pane, dir } => fs::cd(&ctx.sock, pane.0, &dir),
        Command::Run(args) => cmd::run::run(args, ctx),
        Command::Send(args) => cmd::send::run(args, ctx),
        Command::Upload(args) => cmd::upload::run(args, ctx),
        Command::Keys(args) => cmd::keys::run(args, ctx),
        Command::Mouse(args) => cmd::mouse::run(args, ctx),
        Command::Tail(args) => cmd::tail::run(args, ctx),
        Command::Wait(args) => cmd::wait::run(args, ctx),
        Command::Attach { pane } => attach::run(&ctx.sock, util::here(pane)?),
        Command::Tui { session } => tui::run(&ctx.sock, session),
        Command::Export(args) => cmd::export::run(args, ctx),
        Command::Process(args) => cmd::process::run(args, ctx),
        Command::Capture(args) => cmd::capture::run(args, ctx),
        Command::Events(args) => cmd::events::run(args, ctx),
        Command::Close(args) => cmd::close::run(args, ctx),
        Command::Ask | Command::Hook | Command::Inbox | Command::Hooks { .. } => {
            unreachable!("handled first")
        }
        Command::Attention(args) => cmd::attention::run(args, ctx),
        Command::History(args) => cmd::history::run(args, ctx),
        Command::Log(args) => cmd::log::run(args, ctx),
        Command::Search(args) => cmd::search::run(args, ctx),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn machine_is_not_the_global_host() {
        use clap::Parser;
        let parse = |a: &[&str]| super::Cli::try_parse_from(a).unwrap();
        let c = parse(&["illogical", "--host", "box", "open", "--machine", "m2", ":3000"]);
        assert_eq!(c.host.as_deref(), Some("box"));
        assert!(matches!(c.cmd, super::Command::Open(ref a) if a.machine.as_deref() == Some("m2")));
        let c = parse(&["illogical", "agent", "--machine", "3", "hi"]);
        assert!(c.host.is_none());
        assert!(matches!(c.cmd, super::Command::Agent(ref a) if a.machine.as_deref() == Some("3")));
        assert!(super::Cli::try_parse_from(["illogical", "agent", "--vm", "--machine", "m3"]).is_err());
        assert!(super::looks_like_machine("m2") && super::looks_like_machine("local"));
        assert!(!super::looks_like_machine("box"));
    }

    /// What a newcomer can't use stays out of the help, and still works for
    /// anyone who knows the command.
    #[test]
    fn what_a_stranger_cant_use_is_hidden_but_there() {
        use clap::{CommandFactory, Parser};
        let mut root = super::Cli::command();
        let listed = root.render_long_help().to_string();
        let listed: Vec<&str> =
            listed.lines().filter_map(|l| l.strip_prefix("  ")?.split_whitespace().next()).collect();
        for hidden in ["fountain", "studio", "app", "workspace", "guests", "machines", "sandboxes"] {
            assert!(root.find_subcommand(hidden).unwrap().is_hide_set(), "{hidden} isn't hidden");
            assert!(!listed.contains(&hidden), "{hidden} is in `illogical --help`");
            // Asking for it by name still gives its help.
            let e = super::Cli::try_parse_from(["illogical", hidden, "--help"]).err().expect("help is an early exit");
            assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp, "{hidden}");
        }
        assert!(listed.contains(&"run") && listed.contains(&"pr"), "{listed:?}");
        // Options: out of the subcommand's help, still parsed.
        for (cmd, opts) in [
            ("agent", &["fountain", "as_fountain", "vault", "vm"][..]),
            ("run", &["vm", "vm_tab", "image", "sandbox"][..]),
            ("share", &["guest", "rw", "reusable", "relay", "addr", "name"][..]),
        ] {
            let sub = root.find_subcommand(cmd).unwrap();
            for o in opts {
                let arg = sub.get_arguments().find(|a| a.get_id() == o).unwrap_or_else(|| panic!("{cmd} has no {o}"));
                assert!(arg.is_hide_set(), "{cmd} {o} isn't hidden");
            }
        }
        assert!(super::Cli::try_parse_from(["illogical", "agent", "--fountain", "x", "hi"]).is_ok());
        assert!(super::Cli::try_parse_from(["illogical", "run", "--vm", "--", "make"]).is_ok());
        assert!(super::Cli::try_parse_from(["illogical", "share", "--guest", "--rw", "%3"]).is_ok());
        assert!(super::Cli::try_parse_from(["illogical", "guests"]).is_ok());
    }

    /// With the `labs` file the help lists what #342 hid, all of it and
    /// nothing more: the internals stay hidden, and everything parses either
    /// way. The test passes the bool; it reads neither the filesystem nor the
    /// environment.
    #[test]
    fn labs_unhides_the_labs_set_and_only_that() {
        // What `--help` hides, as `command` or `command --option`.
        fn hidden(c: &clap::Command) -> Vec<String> {
            let mut out = vec![];
            for a in c.get_arguments().filter(|a| a.is_hide_set()) {
                out.push(format!("--{}", a.get_id()));
            }
            for sub in c.get_subcommands() {
                if sub.is_hide_set() {
                    out.push(sub.get_name().to_owned());
                }
                for h in hidden(sub) {
                    out.push(format!("{} {h}", sub.get_name()));
                }
            }
            out.sort();
            out
        }
        let (off, on) = (hidden(&super::labs_command(false)), hidden(&super::labs_command(true)));
        let mut set: Vec<String> =
            ["fountain", "studio", "app", "workspace", "guests", "machines", "sandboxes"].map(String::from).into();
        for (cmd, opts) in [
            ("agent", &["fountain", "as_fountain", "vault", "vm"][..]),
            ("run", &["vm", "vm_tab", "image", "sandbox"][..]),
            ("share", &["guest", "rw", "reusable", "relay", "addr", "name"][..]),
        ] {
            set.extend(opts.iter().map(|o| format!("{cmd} --{o}")));
        }
        for h in &set {
            assert!(off.contains(h), "{h} isn't hidden without labs");
            assert!(!on.contains(h), "{h} is still hidden with labs");
        }
        // Only those: the rest of what is hidden is hidden both ways.
        let rest: Vec<&String> = off.iter().filter(|h| !set.contains(h)).collect();
        assert!(rest.iter().any(|h| *h == "bridge"), "{rest:?}");
        assert_eq!(on.iter().collect::<Vec<_>>(), rest, "labs unhides something that isn't in its set");
        assert_eq!(off.len(), set.len() + rest.len());

        // The help lists them with labs and not without.
        let listed = |labs: bool| {
            let help = super::labs_command(labs).render_long_help().to_string();
            help.lines()
                .filter_map(|l| l.strip_prefix("  ")?.split_whitespace().next().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let (without, with) = (listed(false), listed(true));
        for name in ["fountain", "studio", "app", "workspace", "guests", "machines", "sandboxes"] {
            assert!(!without.iter().any(|l| l == name), "{name} in `illogical --help` without labs");
            assert!(with.iter().any(|l| l == name), "{name} isn't in `illogical --help` with labs");
        }
        assert!(!with.iter().any(|l| l == "bridge"), "an internal is listed with labs");
        let sub_help = |labs: bool, cmd: &str| {
            let mut root = super::labs_command(labs);
            root.find_subcommand_mut(cmd).unwrap().render_long_help().to_string()
        };
        for (cmd, flag) in [("run", "--vm-tab"), ("agent", "--fountain"), ("share", "--guest")] {
            assert!(!sub_help(false, cmd).contains(flag), "{cmd} {flag} without labs");
            assert!(sub_help(true, cmd).contains(flag), "{cmd} {flag} with labs");
        }
        // And either way they parse.
        for labs in [false, true] {
            use clap::FromArgMatches;
            let parse = |a: &[&str]| {
                super::Cli::from_arg_matches(&super::labs_command(labs).try_get_matches_from(a).unwrap()).is_ok()
            };
            assert!(parse(&["illogical", "run", "--vm", "--", "make"]));
            assert!(parse(&["illogical", "guests"]));
        }
    }

    /// The help is for strangers: no milestone or issue numbers, no names of
    /// our own machines, and each command's summary is one short line.
    #[test]
    fn help_has_no_internal_numbers_or_names_and_short_summaries() {
        use clap::CommandFactory;
        fn walk(c: &mut clap::Command, path: &str, bad: &mut Vec<String>) {
            let help = c.render_long_help().to_string();
            let bytes = help.as_bytes();
            for (at, ch) in help.char_indices() {
                let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
                let digits = |from: usize| help[from..].chars().take_while(|c| c.is_ascii_digit()).count();
                // M36, S18 (a letter and two digits) and #166.
                let milestone = matches!(ch, 'M' | 'S') && before_ok && digits(at + 1) >= 2;
                let issue = ch == '#' && digits(at + 1) >= 2;
                if milestone || issue {
                    bad.push(format!("{path}: {}", &help[at..(at + 12).min(help.len())]));
                }
            }
            for name in ["geek", "jake-mini", "arugula-salad"] {
                if help.contains(name) {
                    bad.push(format!("{path}: {name}"));
                }
            }
            for sub in c.get_subcommands_mut() {
                if let Some(about) = sub.get_about() {
                    let about = about.to_string();
                    if about.contains('\n') || about.len() > 90 {
                        bad.push(format!("{path} {}: summary {about:?}", sub.get_name()));
                    }
                }
                let name = format!("{path} {}", sub.get_name());
                walk(sub, &name, bad);
            }
        }
        let mut bad = vec![];
        walk(&mut super::Cli::command(), "illogical", &mut bad);
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// The README and the docs for users don't carry the milestone log, and
    /// the README opens with the agents pitch, as the site does.
    #[test]
    fn the_readme_and_docs_have_no_milestone_numbers() {
        let docs = [
            ("README.md", include_str!("../../../README.md")),
            ("docs/features.md", include_str!("../../../docs/features.md")),
            ("docs/cli.md", include_str!("../../../docs/cli.md")),
            ("docs/advanced.md", include_str!("../../../docs/advanced.md")),
            ("docs/teams.md", include_str!("../../../docs/teams.md")),
            ("docs/control.md", include_str!("../../../docs/control.md")),
        ];
        for (name, text) in docs {
            let b = text.as_bytes();
            for (at, _) in text.match_indices('M') {
                let word_start = at == 0 || !b[at - 1].is_ascii_alphanumeric();
                let digits = b[at + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                assert!(!(word_start && digits >= 2), "{name}: {}", &text[at..(at + 20).min(text.len())]);
            }
            for hash in text.match_indices(" (#") {
                let n = text[hash.0 + 3..].chars().take_while(|c| c.is_ascii_digit()).count();
                assert!(n == 0, "{name}: {}", &text[hash.0..(hash.0 + 20).min(text.len())]);
            }
        }
        let readme = docs[0].1;
        let first = readme.lines().skip(2).take_while(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        assert!(first.starts_with("Keep track of your agents without checking every session."), "{first}");
    }
}
