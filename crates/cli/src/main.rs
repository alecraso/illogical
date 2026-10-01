//! `illogical`: drive illogicald from a shell or a script. Every command
//! talks to the daemon's HTTP API over its Unix socket (or another daemon's
//! URL, with `--host`); `--json` prints the API's answers as they are, for
//! programs.

mod attach;
mod hosts;
mod http;

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
        /// One argument is a shell command line (`'make && ./app'`);
        /// several are a program and its arguments, quoted as given.
        #[arg(trailing_var_arg = true, required_unless_present_any = ["vm", "vm_tab"])]
        command: Vec<String>,
    },
    /// Machines that panes run on (VM panes).
    Machines,
    /// A block's type, place and state (any type).
    Describe { block: Pane },
    /// Call one of a block's methods, e.g. `call %4 navigate '{"url":"…"}'`.
    Call {
        block: Pane,
        method: String,
        /// Arguments as JSON.
        args: Option<String>,
    },
    /// Open a web page in a browser block.
    Open {
        url: String,
        /// Split this block instead of opening a tab.
        #[arg(long)]
        split: Option<Pane>,
        #[arg(long)]
        session: Option<String>,
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
    },
    /// Search the output of every pane.
    Search {
        re: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
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
    cli.socket.clone().unwrap_or_else(|| {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
            .join("illogical");
        // A state directory too deep for a socket path puts it elsewhere.
        match std::fs::read_to_string(state.join("sock.path")) {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
            _ => state.join("sock"),
        }
    })
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

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn main() {
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
    let sock = hosts::target(socket(&cli), cli.host.as_deref())?;
    REMOTE.store(matches!(sock, http::Target::Url(_)), std::sync::atomic::Ordering::Relaxed);
    let json_out = cli.json;
    match cli.cmd {
        Command::Hosts { cmd } => hosts::run(&sock, cmd, json_out, duration)?,
        Command::Install { .. } => unreachable!("handled before connecting"),
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
        Command::Open { url, split, session } => {
            let body = json!({
                "type": "browser",
                "config": { "url": url },
                "split": split.map(|p| p.0),
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
                let (state, sprite, provider) = (s("state"), s("sprite"), s("provider"));
                println!("m{:<4} {owner:<5} {state:<9} {sprite:<34} {provider} ({image})", m["id"]);
            }
        }
        Command::Run { session, split, cwd, policy: pol, wait, vm, vm_tab, image, command } => {
            if image.is_some() && !vm && !vm_tab {
                anyhow::bail!("--image is for --vm or --vm-tab");
            }
            // A VM (or another daemon's host) has none of this host's
            // directories.
            let cwd = if vm || vm_tab || REMOTE.load(std::sync::atomic::Ordering::Relaxed) {
                cwd
            } else {
                cwd.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
            };
            let body = json!({
                "command": (!command.is_empty()).then(|| shell_command(&command)),
                "vm": vm,
                "vm_tab": vm_tab,
                "image": image,
                "session": session,
                "split": split.map(|p| p.0),
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
        Command::Tail { pane, follow, from, last_command, text } => {
            let pane = here(pane)?;
            let mut q = vec![];
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
        Command::Wait { pane, command_end, exit, matching, timeout } => {
            let pane = here(pane)?;
            let mut q = match (command_end, exit, &matching) {
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
        Command::Attention { state, pane } => {
            let state = state.replace('-', "_");
            // Hooks (Claude Code's, say) run this in every terminal; outside an
            // illogical pane there's nobody to tell, and that's fine.
            let Ok(pane) = here(pane) else { return Ok(0) };
            let path = format!("/api/panes/{pane}/attention");
            request(&sock, "POST", &path, Some(&json!({"state": state})))?.json()?;
        }
        Command::History { pane, failed, since, cwd, matching, limit } => {
            let mut q = vec![format!("limit={limit}")];
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
                println!(
                    "{exit}  %{:<4} {:>8}  {}{closed}   [{}]",
                    c["pane"],
                    time(c["started_ms"].as_u64().unwrap_or(0)),
                    c["text"].as_str().unwrap_or("?"),
                    c["cwd"].as_str().unwrap_or("")
                );
            }
        }
        Command::Search { re, since, limit } => {
            let mut q = vec![format!("re={}", enc(&re)), format!("limit={limit}")];
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
                println!("%{}@{}: {}{cmd}", h["pane"], h["offset"], h["line"].as_str().unwrap_or(""));
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
