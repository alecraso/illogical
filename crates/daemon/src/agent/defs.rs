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
//! (`npm install --prefix …/claude` the pin below: see `adapters.rs`), then
//! on `PATH`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CLAUDE_ACP: &str = "@agentclientprotocol/claude-agent-acp@0.85.0";
pub const CODEX_ACP: &str = "@agentclientprotocol/codex-acp@2.1.0";

/// Where Claude Code keeps its login and sessions (#379).
pub const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// #379: which login a Claude Code with this environment uses, said when a
/// turn fails on authentication, with how to log in to that one. Names
/// only: never a credential's value.
pub fn login_hint(env: &[(String, String)]) -> String {
    let get = |k: &str| env.iter().rev().find(|(e, _)| e == k).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty());
    let key =
        ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"].into_iter().find(|k| get(k).is_some());
    let mut out = match get(CLAUDE_CONFIG_DIR) {
        Some(dir) => format!(
            "It used the login in CLAUDE_CONFIG_DIR={dir}: to log in to that one, run `CLAUDE_CONFIG_DIR={dir} claude`, then /login."
        ),
        None => "It used the default login (no CLAUDE_CONFIG_DIR): to log in to that one, run \
                 `env -u CLAUDE_CONFIG_DIR claude`, then /login."
            .into(),
    };
    if let Some(k) = key {
        out.push_str(&format!(" Its environment also has {k}, which Claude Code may use instead."));
    }
    out
}

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
    /// M44: Claude Code wearing this Fountain agent (name or id): its
    /// system prompt, skills and MCP servers, on this host. Read with
    /// `profile`'s login. Only the name is kept: the bundle is in the
    /// cache, and its secrets in memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_fountain: Option<String>,
    /// M44: the agent-specs checkout whose Infisical mapping a worn agent's
    /// `${VAR}`s go through (`~/…` allowed; the default one if it's there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specs: Option<String>,
    /// #163: the permission mode to put the session in once it's open
    /// (`acceptEdits`, `auto`, …), by the agent's `session/set_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// #163: Claude Code with your settings (allow and deny lists, default
    /// mode, `CLAUDE.md`) but none of their hooks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_settings: bool,
    /// #379: the `CLAUDE_CONFIG_DIR` of whoever started it (the CLI's, the
    /// MCP bridge's, the pane's), so its Claude Code uses their login, not
    /// the daemon's default one. A path, not a credential: it's kept, and
    /// a restarted daemon starts the adapter with it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_config_dir: Option<String>,
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
            Kind::Claude => match &self.as_fountain {
                Some(a) => format!("Claude Code as {a}"),
                None => "Claude Code".into(),
            },
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
            _ if self.as_fountain.is_some() && self.agent != Kind::Claude => {
                Err("only Claude Code wears a Fountain agent (agent: claude)".into())
            }
            _ if self.as_fountain.as_deref().is_some_and(|a| a.trim().is_empty()) => {
                Err("as_fountain needs the Fountain agent's name or id".into())
            }
            _ if self.user_settings && self.agent != Kind::Claude => {
                Err("user_settings is Claude Code's (agent: claude)".into())
            }
            Kind::Codex | Kind::Fountain if self.claude_config_dir.is_some() => {
                Err("claude_config_dir is Claude Code's (agent: claude, or acp running its adapter)".into())
            }
            Kind::Fountain if self.permission_mode.is_some() => {
                Err("a Fountain agent takes permission, not permission_mode".into())
            }
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
                l.meta = if self.user_settings {
                    user_settings_meta()
                } else {
                    json!({ "claudeCode": { "options": { "settingSources": [] } } })
                };
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
                // Tests put a stand-in there (M43's e2e: the fake ACP agent),
                // so no test reaches a real Fountain.
                let bin = std::env::var("ILLOGICAL_FOUNTAIN_BIN").ok().filter(|b| !b.is_empty());
                l.argv = vec![bin.unwrap_or_else(|| installed(home, "fountain"))];
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
        // #379: a VM's Claude Code logs in with the token it's given, not
        // with a directory of this host's.
        if let Some(dir) = self.claude_config_dir.as_ref().filter(|d| !vm && !d.is_empty()) {
            l.env.push((CLAUDE_CONFIG_DIR.into(), dir.clone()));
        }
        Ok(l)
    }
}

/// Claude's `_meta` with your settings, skills and `CLAUDE.md` but every
/// hook off (S20 Q3: `project` is what loads `CLAUDE.md`, and it brings the
/// project's hooks): an opened conversation's (M33), or a block's that asked
/// for them (#163).
pub fn user_settings_meta() -> Value {
    json!({ "claudeCode": { "options": {
        "settingSources": ["user", "project", "local"],
        "settings": { "disableAllHooks": true },
    } } })
}

/// M44: a worn Fountain agent on Claude's `session/new` `_meta`: its
/// system prompt appended, its skills as a local plugin, its model. The
/// rest stays, `settingSources: []` above all (without it the user's own
/// hooks fire inside the block, S24).
pub fn wear_meta(meta: &mut Value, system: &str, plugin: &Path, model: Option<&str>) {
    if !meta.is_object() {
        *meta = json!({});
    }
    meta["systemPrompt"] = json!({ "append": system });
    if !meta["claudeCode"].is_object() {
        meta["claudeCode"] = json!({});
    }
    if !meta["claudeCode"]["options"].is_object() {
        meta["claudeCode"]["options"] = json!({});
    }
    let o = &mut meta["claudeCode"]["options"];
    o["plugins"] = json!([{ "type": "local", "path": plugin.display().to_string() }]);
    if let Some(m) = model {
        o["model"] = json!(m);
    }
    if o.get("settingSources").is_none() {
        o["settingSources"] = json!([]);
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
        // #163: your settings, never your hooks.
        let mine = Def { user_settings: true, ..Default::default() }.launch(home, false).unwrap();
        assert_eq!(mine.meta["claudeCode"]["options"]["settingSources"], json!(["user", "project", "local"]));
        assert_eq!(mine.meta["claudeCode"]["options"]["settings"]["disableAllHooks"], json!(true));
        assert!(Def { agent: Kind::Codex, user_settings: true, ..Default::default() }.launch(home, false).is_err());

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
    fn a_worn_agent_on_the_meta() {
        let home = Path::new("/nonexistent-home");
        let def = Def { as_fountain: Some("games".into()), ..Default::default() };
        assert_eq!(def.label(), "Claude Code as games");
        let mut meta = def.launch(home, false).unwrap().meta;
        wear_meta(&mut meta, "be games", Path::new("/c/plugin"), Some("claude-opus-5"));
        assert_eq!(
            meta,
            json!({
                "systemPrompt": { "append": "be games" },
                "claudeCode": { "options": {
                    "settingSources": [],
                    "plugins": [{ "type": "local", "path": "/c/plugin" }],
                    "model": "claude-opus-5",
                } },
            })
        );
        // Even from nothing, the user's settings stay out.
        let mut bare = Value::Null;
        wear_meta(&mut bare, "x", Path::new("/p"), None);
        assert_eq!(bare["claudeCode"]["options"]["settingSources"], json!([]));
        assert!(bare["claudeCode"]["options"].get("model").is_none());
        assert!(Def { agent: Kind::Codex, as_fountain: Some("x".into()), ..Default::default() }.check().is_err());
        assert!(Def { as_fountain: Some(" ".into()), ..Default::default() }.check().is_err());
    }

    #[test]
    fn a_callers_login_goes_to_a_local_claude_only() {
        // #379.
        let home = Path::new("/nonexistent-home");
        let def = Def { claude_config_dir: Some("/u/.claude-two".into()), ..Default::default() };
        let env = vec![(CLAUDE_CONFIG_DIR.to_owned(), "/u/.claude-two".to_owned())];
        assert_eq!(def.launch(home, false).unwrap().env, env);
        assert!(def.launch(home, true).unwrap().env.is_empty(), "a VM logs in with its token");
        let acp = Def { agent: Kind::Acp, command: vec!["claude-agent-acp".into()], ..def.clone() };
        assert_eq!(acp.launch(home, false).unwrap().env, env);
        assert!(Def { agent: Kind::Codex, ..def.clone() }.check().is_err());
        assert!(Def::default().launch(home, false).unwrap().env.is_empty());
    }

    #[test]
    fn a_login_hint_names_the_login_and_never_a_secret() {
        let e = |kv: &[(&str, &str)]| kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let h = login_hint(&e(&[("CLAUDE_CONFIG_DIR", "/u/two")]));
        assert_eq!(
            h,
            "It used the login in CLAUDE_CONFIG_DIR=/u/two: to log in to that one, run `CLAUDE_CONFIG_DIR=/u/two claude`, then /login."
        );
        let h = login_hint(&e(&[("CLAUDE_CONFIG_DIR", ""), ("ANTHROPIC_API_KEY", "sk-secret")]));
        assert!(h.starts_with("It used the default login (no CLAUDE_CONFIG_DIR)"), "{h}");
        assert!(
            h.contains("`env -u CLAUDE_CONFIG_DIR claude`, then /login.") && h.contains("ANTHROPIC_API_KEY"),
            "{h}"
        );
        assert!(!h.contains("sk-secret"), "{h}");
    }

    #[test]
    fn splits_commands() {
        assert_eq!(split_command("a  'b c' \"d\" ''"), vec!["a", "b c", "d", ""]);
        assert_eq!(split_command("  x "), vec!["x"]);
    }
}
