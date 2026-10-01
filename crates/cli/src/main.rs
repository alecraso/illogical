//! `illogical`: drive illogicald from a shell or a script. Every command
//! talks to the daemon's HTTP API over its Unix socket (or another daemon's
//! URL, with `--host`); `--json` prints the API's answers as they are, for
//! programs.

mod ask;
mod attach;
mod fs;
mod hosts;
mod http;
mod tmux;

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
    /// Talk to another daemon: a name from the local daemon's host list
    /// (`illogical hosts`), or a URL.
    #[arg(long, global = true)]
    host: Option<String>,
    /// Print the API's JSON instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Command,
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
        /// One argument is a shell command line (`'make && ./app'`);
        /// several are a program and its arguments, quoted as given. None
        /// (with --cwd, --join or a VM): a shell.
        #[arg(trailing_var_arg = true, required_unless_present_any = ["vm", "vm_tab", "sandbox", "join", "cwd"])]
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
    /// A block's type, place and state (any type).
    Describe { block: Pane },
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
        /// there; else this host].
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        session: Option<String>,
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
        /// Codex instead of Claude Code.
        #[arg(long)]
        codex: bool,
        /// Fountain: a vault for its secrets.
        #[arg(long, requires = "fountain")]
        vault: Option<String>,
        /// A model to switch to (e.g. `haiku`).
        #[arg(long)]
        model: Option<String>,
        /// On a new throwaway VM of its own.
        #[arg(long, conflicts_with = "host")]
        vm: bool,
        /// On this existing machine (`m3` or `3`).
        #[arg(long)]
        host: Option<String>,
        /// Where it works [default: here, or the VM's home].
        #[arg(long)]
        cwd: Option<String>,
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
    /// Type text into a pane (`-` reads stdin).
    Send {
        pane: Pane,
        #[arg(required = true)]
        text: Vec<String>,
        /// Press Enter afterwards.
        #[arg(short, long)]
        enter: bool,
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
    /// Tell illogical whether this pane needs you (for agent hooks).
    Attention {
        /// needs-input, done, working or idle.
        state: String,
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
    Share {
        pane: Option<Pane>,
        /// How long it works (e.g. 30m, 2h, 7d; a week at most).
        #[arg(long, default_value = "1h")]
        ttl: String,
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
    /// Install the daemon: `illogicald install` with these arguments (e.g.
    /// `--tailnet file:KEY --home URL --join TOKEN` in a sandbox).
    Install {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    /// Be a tmux server in control mode for iTerm2 (and other tmux `-CC`
    /// clients): `illogical tmux -CC [attach -t SESSION | new -s NAME]`.
    /// Linked or installed as `tmux`, the CLI does this by itself.
    Tmux {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SharesCmd {
    /// End a share link now; anyone watching is cut off.
    Revoke { id: u32 },
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

/// The pane given, or the one we're running in.
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
        h if h.starts_with("hook:") => json!({"kind": "hook", "command": &h[5..]}),
        _ => bail!("policy: shell, none, rerun, rerun-ask or hook:COMMAND"),
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

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn main() {
    // Run as `tmux` (a link, or a copy on an ssh host's PATH): be tmux's
    // control mode, with tmux's own arguments.
    let argv0 = std::env::args_os().next().map(PathBuf::from);
    if argv0.as_deref().and_then(|p| p.file_name()).is_some_and(|n| n == "tmux") {
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
    if let Command::Install { args } = &cli.cmd {
        // The daemon beside this binary, else the one on PATH.
        use std::os::unix::process::CommandExt;
        let beside = std::env::current_exe()?.with_file_name("illogicald");
        let daemon = if beside.exists() { beside } else { PathBuf::from("illogicald") };
        let err = std::process::Command::new(&daemon).arg("install").args(args).exec();
        bail!("running {}: {err}", daemon.display());
    }
    if let Command::Ask = cli.cmd {
        // A hook: the local daemon only, and never an error.
        return Ok(ask::run(http::Target::Socket(socket(&cli))));
    }
    let reads_history = matches!(cli.cmd, Command::History { .. } | Command::Search { .. } | Command::Tail { .. });
    let (sock, gone) = match hosts::target(socket(&cli), cli.host.as_deref()) {
        Ok(t) => (t, None),
        // A host that's gone (deleted, unreachable) may have left its
        // history here.
        Err(e) if reads_history && cli.host.is_some() => {
            eprintln!("illogical: {e:#}; reading what it synced here instead");
            (http::Target::Socket(socket(&cli)), cli.host.clone())
        }
        Err(e) => return Err(e),
    };
    REMOTE.store(!matches!(sock, http::Target::Socket(_)), std::sync::atomic::Ordering::Relaxed);
    let json_out = cli.json;
    // `host=` for reading synced history.
    let synced_q = |flag: Option<String>| -> Option<String> {
        flag.or(gone.clone()).map(|h| format!("host={}", enc(if h == "all" { "*" } else { &h })))
    };
    match cli.cmd {
        Command::Share { pane, ttl } => {
            let body = json!({"pane": here(pane)?, "ttl_secs": duration(&ttl)?});
            let v = request(&sock, "POST", "/api/shares", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("{}", v["url"].as_str().or(v["path"].as_str()).unwrap_or_default());
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
        Command::Install { .. } => unreachable!("handled before connecting"),
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
        Command::Describe { block } => {
            print_json(&request(&sock, "GET", &format!("/api/blocks/{}", block.0), None)?.json()?);
        }
        Command::Call { block, method, args } => {
            let args: Value = match args {
                Some(a) => serde_json::from_str(&a).context("args must be JSON")?,
                None => json!({}),
            };
            let path = format!("/api/blocks/{}/call/{}", block.0, enc(&method));
            print_json(&request(&sock, "POST", &path, Some(&args))?.json()?);
        }
        Command::Open { target, split, host, session } => {
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
            let local = host.as_deref() == Some("local");
            let host = match host.filter(|_| !local) {
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
        Command::Agent { acp, fountain, codex, vault, model, vm, host, cwd, session, split, wait, prompt } => {
            let mut config = match (&acp, &fountain) {
                (Some(cmd), _) => json!({ "agent": "acp", "command": cmd }),
                (_, Some(name)) => json!({ "agent": "fountain", "fountain_agent": name, "vault": vault }),
                _ if codex => json!({ "agent": "codex" }),
                _ => json!({ "agent": "claude" }),
            };
            // A VM has none of this host's directories.
            let cwd = if vm || host.is_some() {
                cwd
            } else {
                cwd.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
            };
            config["cwd"] = json!(cwd);
            config["model"] = json!(model);
            let prompt = prompt.join(" ");
            if !prompt.is_empty() {
                config["prompt"] = json!(prompt);
            }
            let host = host.map(|h| h.trim_start_matches('m').parse::<u32>()).transpose().context("--host: m<N>")?;
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
        Command::Run { session, split, join, cwd, policy: pol, wait, vm, vm_tab, image, sandbox, command } => {
            if image.is_some() && !vm && !vm_tab {
                anyhow::bail!("--image is for --vm or --vm-tab");
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
        Command::Send { pane, text, enter } => {
            let mut text = text.join(" ");
            if text == "-" {
                text.clear();
                std::io::stdin().read_to_string(&mut text)?;
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
                request(&sock, "POST", &format!("/api/panes/{}/close", p.0), None)?.json()?;
            }
        }
        Command::Ask => unreachable!("handled first"),
        Command::Attention { state, pane } => {
            let state = state.replace('-', "_");
            // Hooks (Claude Code's, say) run this in every terminal; outside an
            // illogical pane there's nobody to tell, and that's fine.
            let Ok(pane) = here(pane) else { return Ok(0) };
            let path = format!("/api/panes/{pane}/attention");
            request(&sock, "POST", &path, Some(&json!({"state": state})))?.json()?;
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
                println!(
                    "{exit}  {host}%{:<4} {:>8}  {}{closed}   [{}]",
                    c["pane"],
                    time(c["started_ms"].as_u64().unwrap_or(0)),
                    c["text"].as_str().unwrap_or("?"),
                    c["cwd"].as_str().unwrap_or("")
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

#[cfg(test)]
mod tests {
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
    fn run_quotes_words() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::shell_command(&v(&["make && ./app"])), "make && ./app");
        assert_eq!(super::shell_command(&v(&["make", "test"])), "make test");
        assert_eq!(super::shell_command(&v(&["bash", "-c", "echo hi; exit 3"])), "bash -c 'echo hi; exit 3'");
        assert_eq!(super::shell_command(&v(&["echo", "it's"])), r"echo 'it'\''s'");
    }
}
