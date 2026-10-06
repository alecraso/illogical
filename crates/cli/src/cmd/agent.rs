//! `illogical agent`: an agent block with a prompt, or a Claude Code conversation continued.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, REMOTE, absolute, env_pane, print_json};
use anyhow::Context;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub struct Args {
    /// Any ACP agent server, by its command line.
    #[arg(long, conflicts_with_all = ["fountain", "codex"])]
    acp: Option<String>,
    /// A Fountain agent (name or id), run in Fountain's sandbox.
    #[arg(long, hide = true, conflicts_with = "codex")]
    fountain: Option<String>,
    /// Claude Code wearing a Fountain agent (name or id), on this host:
    /// its system prompt, skills and MCP servers.
    #[arg(long = "as", value_name = "AGENT", hide = true, conflicts_with_all = ["acp", "fountain", "codex", "vm", "machine"])]
    as_fountain: Option<String>,
    /// Codex instead of Claude Code.
    #[arg(long)]
    codex: bool,
    /// Fountain: a vault for its secrets (with `--as`: whose agent-specs
    /// mapping its `${VAR}`s go through, over its environment's).
    #[arg(long, hide = true)]
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
    #[arg(long, hide = true, conflicts_with = "machine")]
    vm: bool,
    /// On this existing machine (`m3` or `3`). (`--host` is another
    /// daemon.)
    #[arg(long)]
    pub machine: Option<String>,
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
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match args {
        Args { resume, fork, session, split, wait, prompt, .. } if resume.is_some() || fork.is_some() => {
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
        Args {
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
            let here = !(vm || machine.is_some() || REMOTE.load(std::sync::atomic::Ordering::Relaxed));
            let cwd =
                if here { cwd.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string())) } else { cwd };
            config["cwd"] = json!(cwd);
            // #379: Claude Code logs in as whoever ran this, not as the
            // daemon's default login ("" says this has none).
            if here && fountain.is_none() && !codex {
                let dir = std::env::var("CLAUDE_CONFIG_DIR").ok().filter(|d| !d.is_empty());
                config["claude_config_dir"] = json!(dir.map(|d| absolute(&d)).transpose()?.unwrap_or_default());
            }
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
            // #335: Claude Code's or Codex's adapter isn't installed here:
            // say how, rather than make a block that can't start.
            if acp.is_none()
                && fountain.is_none()
                && !vm
                && machine.is_none()
                && let Some(why) = adapter_missing(&sock, config["agent"].as_str().unwrap_or_default())
            {
                eprintln!("{why}");
                return Ok(1);
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
    }
    Ok(0)
}

/// Why an agent of `kind` can't start on this daemon, and how to fix it
/// (#335); `None` when it can, or the daemon doesn't say.
fn adapter_missing(sock: &crate::http::Target, kind: &str) -> Option<String> {
    let v = request(sock, "GET", "/api/agents/adapters", None).ok()?.json().ok()?;
    let a = v["adapters"].as_array()?.iter().find(|a| a["kind"] == kind)?;
    adapter_fix(a)
}

fn adapter_fix(a: &Value) -> Option<String> {
    let kind = a["kind"].as_str()?;
    let why = a["why"].as_str().map(str::to_owned).unwrap_or_else(|| format!("{kind}'s adapter can't start"));
    let npm = a["npm"].as_str().unwrap_or_default();
    match a["state"].as_str()? {
        "missing" => Some(format!(
            "{why}: agent blocks run it through it.\n`illogical setup {kind}` installs it (or: {npm}); then run this again."
        )),
        "no_node" => Some(format!(
            "{why}.\nInstall Node (`mise use -g node@22`, or nodejs.org), then `illogical setup {kind}` (or: {npm})."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_missing_adapter_says_how() {
        let a = json!({
            "kind": "claude",
            "state": "missing",
            "why": "Claude Code's adapter isn't installed",
            "npm": "npm install --prefix ~/.local/share/illogical/agents/claude p@1",
        });
        let said = super::adapter_fix(&a).unwrap();
        assert!(said.starts_with("Claude Code's adapter isn't installed"), "{said}");
        assert!(said.contains("`illogical setup claude`") && said.contains("npm install --prefix"), "{said}");
        let node =
            json!({ "kind": "codex", "state": "no_node", "why": "Codex's adapter needs Node 20+", "npm": "npm i" });
        assert!(super::adapter_fix(&node).unwrap().contains("mise use -g node@22"));
        assert!(super::adapter_fix(&json!({ "kind": "claude", "state": "installed" })).is_none());
    }
}
