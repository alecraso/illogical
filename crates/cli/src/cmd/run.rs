//! `arugula run`: start a command in a new tab or split.

use super::Ctx;
use crate::http::{self, request};
use crate::util::{Pane, REMOTE, env_pane, print_json};
use anyhow::{Context, bail};
use serde_json::{Value, json};

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

#[derive(clap::Args)]
pub struct Args {
    /// Session name or id (created if missing).
    #[arg(long)]
    session: Option<String>,
    /// Split this pane instead of opening a tab.
    #[arg(long)]
    split: Option<Pane>,
    /// With --split: run where that pane runs instead of this host.
    #[arg(long, requires = "split")]
    join: bool,
    /// Where it starts (with --join: a directory there).
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
    #[arg(long, hide = true)]
    vm: bool,
    /// In a new tab whose panes share one throwaway VM (splits join it),
    /// deleted when the tab closes.
    #[arg(long, hide = true, conflicts_with_all = ["vm", "split"])]
    vm_tab: bool,
    /// The VM's image (with --vm or --vm-tab).
    #[arg(long, hide = true)]
    image: Option<String>,
    /// On a sandbox that exists (`arugula sandboxes`), over a plain
    /// exec with no daemon there: disposable, and the sandbox stays
    /// when the pane closes. Without a command: a shell.
    #[arg(long, hide = true, conflicts_with_all = ["vm", "vm_tab", "image"])]
    sandbox: Option<String>,
    /// With --host: run it on that host but put it in this daemon's
    /// layout (a tab here, or beside --split, a pane here), as a remote
    /// pane. On the host it's in a session named after this daemon.
    #[arg(long, conflicts_with_all = ["join", "vm_tab"])]
    home: bool,
    /// One argument is a shell command line (`'make && ./app'`);
    /// several are a program and its arguments, quoted as given. None
    /// (with --cwd, --join or --home): a shell.
    #[arg(trailing_var_arg = true, required_unless_present_any = ["vm", "vm_tab", "sandbox", "join", "cwd", "home"])]
    command: Vec<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, local_sock, host_name, .. } = ctx;
    let Args { session, split, join, cwd, policy: pol, wait, vm, vm_tab, image, sandbox, home, command } = args;
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
        let from = std::env::var("ARUGULA_PANE").ok().and_then(|v| v.parse::<u32>().ok());
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
    let cwd = if vm || vm_tab || join || sandbox.is_some() || REMOTE.load(std::sync::atomic::Ordering::Relaxed) {
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
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_quotes_words() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::policy("hook:claude --continue").unwrap()["command"], "claude --continue");
        assert_eq!(super::shell_command(&v(&["make && ./app"])), "make && ./app");
        assert_eq!(super::shell_command(&v(&["make", "test"])), "make test");
        assert_eq!(super::shell_command(&v(&["bash", "-c", "echo hi; exit 3"])), "bash -c 'echo hi; exit 3'");
        assert_eq!(super::shell_command(&v(&["echo", "it's"])), r"echo 'it'\''s'");
    }
}
