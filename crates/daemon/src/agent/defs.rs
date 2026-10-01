//! Agent definitions: which ACP agent server a block runs, as a command line
//! plus a few defaults.
//!
//! - **claude**: Claude Code through `claude-agent-acp` (pinned 0.85.0:
//!   0.81.2, Fountain's pin, asks AskUserQuestion and MCP forms too, but its
//!   Claude Code refuses MCP servers' sign-in links), with no settings
//!   sources so your own hooks don't fire inside the block.
//! - **codex**: `codex-acp` against the installed `codex`.
//! - **fountain**: `fountain acp --agent X`, an agent in a Fountain sandbox.
//! - **acp**: any other ACP agent server, by its command line.
//!
//! The npm adapters are looked for in `~/.local/share/illogical/agents/`
//! (`npm install --prefix …/claude @agentclientprotocol/claude-agent-acp@0.85.0`),
//! then on `PATH`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CLAUDE_ACP: &str = "@agentclientprotocol/claude-agent-acp@0.85.0";
pub const CODEX_ACP: &str = "@agentclientprotocol/codex-acp@2.1.0";

/// The parent Claude Code session's variables, which would make the
/// adapter's Claude Code think it is nested inside another one.
pub const CLAUDE_ENV_REMOVE: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SSE_PORT",
    "AI_AGENT",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Claude,
    Codex,
    Fountain,
    /// Any ACP agent server, by `command`.
    Acp,
}

/// What a block runs and how it starts a session: everything here is in
/// `layout.json`, so nothing secret goes in it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Def {
    #[serde(default)]
    pub agent: Kind,
    /// The command line, for `acp` (and to override the others').
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Fountain: the agent's name or id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fountain_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// Fountain's `--permission` (default `ask`: approvals come to the block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    /// Fountain's credentials profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// A model to switch to after the session starts (`haiku`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// How to start the agent server.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub argv: Vec<String>,
    /// Added to the environment.
    pub env: Vec<(String, String)>,
    /// Taken out of it.
    pub remove: Vec<String>,
    /// `session/new`'s `_meta`.
    pub meta: Value,
    /// In a VM: the npm package to install there first, if any.
    pub npm: Option<&'static str>,
}

/// Where illogical keeps agent adapters on this host.
pub fn agents_dir(home: &Path) -> PathBuf {
    std::env::var_os("ILLOGICAL_AGENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share/illogical/agents"))
}

/// `name` from our agents directory if it's installed there, else `name`
/// (found on PATH).
fn adapter(home: &Path, dir: &str, name: &str) -> String {
    let p = agents_dir(home).join(dir).join("node_modules/.bin").join(name);
    if p.exists() { p.display().to_string() } else { name.to_owned() }
}

fn installed(home: &Path, name: &str) -> String {
    let p = home.join(".local/bin").join(name);
    if p.exists() { p.display().to_string() } else { name.to_owned() }
}

impl Def {
    pub fn label(&self) -> String {
        match self.agent {
            Kind::Claude => "Claude Code".into(),
            Kind::Codex => "Codex".into(),
            Kind::Fountain => format!("Fountain {}", self.fountain_agent.as_deref().unwrap_or("agent")),
            Kind::Acp => self.command.first().map(|c| c.rsplit('/').next().unwrap_or(c).to_owned()).unwrap_or_default(),
        }
    }

    pub fn check(&self) -> Result<(), String> {
        match self.agent {
            Kind::Fountain if self.fountain_agent.as_deref().is_none_or(str::is_empty) => {
                Err("a Fountain agent block needs the agent's name or id".into())
            }
            Kind::Acp if self.command.is_empty() => Err("an ACP agent block needs a command".into()),
            _ => Ok(()),
        }
    }

    /// The command and environment, on this host (`vm` false) or in a VM.
    pub fn launch(&self, home: &Path, vm: bool) -> Result<Launch, String> {
        self.check()?;
        let mut l = Launch { argv: vec![], env: vec![], remove: vec![], meta: json!({}), npm: None };
        match self.agent {
            Kind::Claude => {
                l.argv = if vm {
                    vec!["claude-agent-acp".into()]
                } else {
                    vec![adapter(home, "claude", "claude-agent-acp")]
                };
                l.remove = CLAUDE_ENV_REMOVE.iter().map(|s| s.to_string()).collect();
                // Without this, your Claude Code settings and hooks (M3's
                // attention hooks too) fire inside the block.
                l.meta = json!({ "claudeCode": { "options": { "settingSources": [] } } });
                l.npm = Some(CLAUDE_ACP);
            }
            Kind::Codex => {
                l.argv = if vm { vec!["codex-acp".into()] } else { vec![adapter(home, "codex", "codex-acp")] };
                if !vm {
                    l.env.push(("CODEX_PATH".into(), installed(home, "codex")));
                }
                l.npm = Some(CODEX_ACP);
            }
            Kind::Fountain => {
                if vm {
                    return Err("Fountain agents run in Fountain's sandboxes, not in a VM here".into());
                }
                l.argv = vec![installed(home, "fountain")];
                if let Some(p) = &self.profile {
                    l.argv.extend(["--profile".into(), p.clone()]);
                }
                l.argv.extend(["acp".into(), "--agent".into(), self.fountain_agent.clone().unwrap_or_default()]);
                if let Some(v) = &self.vault {
                    l.argv.extend(["--vault".into(), v.clone()]);
                }
                l.argv.extend(["--permission".into(), self.permission.clone().unwrap_or_else(|| "ask".into())]);
            }
            Kind::Acp => l.argv = self.command.clone(),
        }
        if !self.command.is_empty() {
            l.argv = self.command.clone();
        }
        Ok(l)
    }
}

/// Split a command line on whitespace, honouring simple quotes.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches() {
        let home = Path::new("/nonexistent-home");
        let claude = Def::default().launch(home, false).unwrap();
        assert_eq!(claude.argv, vec!["claude-agent-acp"]);
        assert_eq!(claude.meta["claudeCode"]["options"]["settingSources"], json!([]));
        assert!(claude.remove.iter().any(|r| r == "CLAUDECODE"));

        let f = Def {
            agent: Kind::Fountain,
            fountain_agent: Some("arena".into()),
            vault: Some("v".into()),
            ..Default::default()
        };
        assert_eq!(
            f.launch(home, false).unwrap().argv,
            split_command("fountain acp --agent arena --vault v --permission ask")
        );
        assert!(f.launch(home, true).is_err());
        assert!(Def { agent: Kind::Fountain, ..Default::default() }.check().is_err());

        let codex = Def { agent: Kind::Codex, ..Default::default() }.launch(home, false).unwrap();
        assert_eq!(codex.env, vec![("CODEX_PATH".into(), "codex".into())]);
    }

    #[test]
    fn splits_commands() {
        assert_eq!(split_command("a  'b c' \"d\" ''"), vec!["a", "b c", "d", ""]);
        assert_eq!(split_command("  x "), vec!["x"]);
    }
}
